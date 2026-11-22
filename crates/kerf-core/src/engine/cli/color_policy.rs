//! How this `ffmpeg` composites tagged and untagged colour, measured once.

use super::*;

// ---- the composite's colour policy ---------------------------------------------

/// How this ffmpeg picks the matrix of a composite — see [`CompositeColorPolicy`].
/// Probed by **running the still graph** twice on the same picture, once in a
/// clip tagged BT.709 and once in an untagged one, and seeing whether the
/// finished composites differ: FFmpeg 6 hands the encoder an untagged frame
/// (BT.601 whatever the clip was, so the two agree); FFmpeg 9 negotiates the
/// colourspace across the overlay chain, so the tagged clip's composite is
/// converted as BT.709 and the two differ. Measured, not read off a version
/// string — and the probe checks that the tag **survived** into the clip it
/// measures (an `ffprobe` of it), because a tag that was lost on the way would
/// read as "FFmpeg 6" on any build.
///
/// **A probe that could not tell is [`CompositeColorPolicy::Unknown`], not a
/// guess.** FFmpeg 6 and 9 disagree precisely on BT.709 and BT.2020 footage, so
/// no answer is safe for it: `Unknown` mirrors only the stacks both agree on
/// (all BT.601-class) and refuses the rest. It is not remembered — a measured
/// policy is kept for the process, a failed probe is retried on a later call, at
/// most once per backoff period (5 s, doubling to 5 min) so a broken ffmpeg is
/// not respawned for every frame. The whole probe is bounded
/// ([`POLICY_PROBE_TIMEOUT`]): a hung ffmpeg cannot hold a caller for longer.
///
/// **The first call blocks** — 70 to 200 ms where ffmpeg works (six short runs,
/// two at a time; 70 ms measured on a static build, 200 on a distro one) and up
/// to the timeout where it does not — and concurrent first callers wait behind it,
/// so call it from a blocking thread, not an async task or the UI's. Later calls
/// are a lock and a read.
pub fn composite_color_policy() -> CompositeColorPolicy {
    static PROBE: Mutex<PolicyProbe> = Mutex::new(PolicyProbe::new());
    cached_policy(&PROBE, Instant::now(), || {
        measure_composite_color_policy(&ffmpeg_bin(), &ffprobe_bin(), POLICY_PROBE_TIMEOUT)
    })
}

/// The longest the whole policy probe may take. It is six short runs of a
/// 64x64 clip, anything near this is a wedged ffmpeg, and every other caller of
/// [`composite_color_policy`] waits behind it.
pub(super) const POLICY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// What the process knows about its ffmpeg's composite colour policy.
pub(super) struct PolicyProbe {
    /// The measured policy, once there is one: kept for good.
    measured: Option<CompositeColorPolicy>,
    /// Failed probes in a row, and the earliest time the next one may run.
    failures: u32,
    pub(super) retry_at: Option<Instant>,
}

impl PolicyProbe {
    pub(super) const fn new() -> Self {
        Self {
            measured: None,
            failures: 0,
            retry_at: None,
        }
    }
}

/// The wait before the probe may run again after `failures` failures in a row:
/// 5 s, doubling, capped at 5 minutes.
pub(super) fn policy_probe_backoff(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300))
}

/// The policy in `state`, measuring it with `measure` if nothing is known yet and
/// the backoff after the last failure has passed. Holds the lock while it
/// measures, so concurrent first callers wait for one probe rather than each
/// running their own; the probe is bounded, so the wait is.
pub(super) fn cached_policy(
    state: &Mutex<PolicyProbe>,
    now: Instant,
    measure: impl FnOnce() -> Option<CompositeColorPolicy>,
) -> CompositeColorPolicy {
    let mut s = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(policy) = s.measured {
        return policy;
    }
    if s.retry_at.is_some_and(|t| now < t) {
        return CompositeColorPolicy::Unknown;
    }
    match measure() {
        Some(policy) => {
            tracing::debug!(?policy, "probed ffmpeg's composite colour policy");
            s.measured = Some(policy);
            policy
        }
        None => {
            s.failures += 1;
            let wait = policy_probe_backoff(s.failures);
            s.retry_at = Some(Instant::now() + wait);
            tracing::warn!(
                failures = s.failures,
                retry_in_secs = wait.as_secs(),
                "could not measure ffmpeg's composite colour policy; only BT.601-class stacks are drawn on the GPU until it can"
            );
            CompositeColorPolicy::Unknown
        }
    }
}

/// The probe picture: a flat YCbCr whose red the two matrices convert to values
/// well apart (about 20 levels on FFmpeg's converter).
const POLICY_PROBE_YUV: (u8, u8, u8) = (60, 128, 230);

/// Which policy two measured reds say: the same red from the tagged and the
/// untagged clip means the tag did not reach the conversion. A difference between
/// the two readings is not a result.
pub(super) fn policy_from_reds(tagged: u8, untagged: u8) -> Option<CompositeColorPolicy> {
    match tagged.abs_diff(untagged) {
        0..=3 => Some(CompositeColorPolicy::FixedBt601),
        10.. => Some(CompositeColorPolicy::BottomLayerTag),
        _ => None,
    }
}

/// Measure the policy of `ffmpeg` (with `ffprobe` to check the probe clips), or
/// `None` when it cannot be read: a binary that will not run, a run that fails or
/// outlasts `timeout`, a tag that did not survive, readings that do not decide.
pub(super) fn measure_composite_color_policy(
    ffmpeg: &str,
    ffprobe: &str,
    timeout: std::time::Duration,
) -> Option<CompositeColorPolicy> {
    let deadline = Instant::now() + timeout;
    // The two clips are independent, so they are measured side by side.
    let (tagged, untagged) = std::thread::scope(|scope| {
        let tagged = scope.spawn(|| probe_composite_red(ffmpeg, ffprobe, true, deadline));
        let untagged = probe_composite_red(ffmpeg, ffprobe, false, deadline);
        (tagged.join().ok().flatten(), untagged)
    });
    let (tagged, untagged) = (tagged?, untagged?);
    tracing::debug!(tagged, untagged, "composite colour probe");
    policy_from_reds(tagged, untagged)
}

/// The red of the middle pixel of the still graph's composite over a one-frame
/// clip of [`POLICY_PROBE_YUV`], with the clip tagged BT.709 or not.
pub(super) fn probe_composite_red(ffmpeg: &str, ffprobe: &str, tag_bt709: bool, deadline: Instant) -> Option<u8> {
    let (y, u, v) = POLICY_PROBE_YUV;
    // 1. A one-frame clip of exactly that picture (raw planes, so no generator gets
    // to round it), tagged the way a camera's file is, or not.
    let mut raw = vec![y; 64 * 64];
    raw.extend(std::iter::repeat_n(u, 32 * 32));
    raw.extend(std::iter::repeat_n(v, 32 * 32));
    // The tag goes on the *input* (a decoder option): put on the output of a
    // raw-video encode it would make FFmpeg 9 convert the picture into the new
    // matrix on the way, and the two clips would no longer hold the same planes.
    let mut maker = command(ffmpeg);
    maker.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-s",
        "64x64",
    ]);
    if tag_bt709 {
        maker.args(["-colorspace", "bt709"]);
    }
    maker.args([
        "-i",
        "pipe:0",
        "-frames:v",
        "1",
        "-c:v",
        "ffv1",
        "-pix_fmt",
        "yuv420p",
        "-f",
        "matroska",
        "pipe:1",
    ]);
    let clip = run_piped_until(&mut maker, raw, deadline).filter(|c| !c.is_empty())?;

    // 2. The tag must have survived into the clip, as the probe the app imports
    // with reads it: a clip that lost it (a wrapper, a build that drops the
    // option) would make the two measurements identical on *any* FFmpeg, and the
    // probe would read "FFmpeg 6" off an FFmpeg 9.
    let mut checker = command(ffprobe);
    checker.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=color_space",
        "-of",
        "csv=p=0",
        "-i",
        "pipe:0",
    ]);
    let found = run_piped_until(&mut checker, clip.clone(), deadline)?;
    let found = String::from_utf8_lossy(&found);
    let found = found.trim();
    let survived = if tag_bt709 {
        found == "bt709"
    } else {
        !matches!(found, "bt709" | "bt2020nc" | "bt2020c")
    };
    if !survived {
        tracing::debug!(found, tag_bt709, "the probe clip's colour tag is not what was written");
        return None;
    }

    // 3. The still graph of a one-clip timeline over that clip, built by the very
    // builder the stills use, ending in raw RGB.
    let asset = Asset {
        id: uuid::Uuid::new_v4(),
        path: "pipe:0".to_string(),
        name: "probe".to_string(),
        duration: 1.0,
        streams: vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "ffv1".to_string(),
            width: Some(64),
            height: Some(64),
            fps: Some(1.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: Some("yuv420p".to_string()),
            color_space: tag_bt709.then(|| "bt709".to_string()),
            color_range: None,
            lead: 0.0,
            time_base: None,
        }],
        imported_at: chrono::Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    let timeline = Timeline {
        tracks: vec![crate::model::Track {
            clips: vec![Clip::new(asset.id, 0.0, 1.0, 0.0)],
            ..crate::model::Track::new(StreamKind::Video, "V1")
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let args = build_still_args(
        &timeline,
        std::slice::from_ref(&asset),
        &ExportOptions::default(),
        0.0,
        64,
        None,
        &StillOutput::RgbPipe,
    )
    .ok()?;
    let mut render = command(ffmpeg);
    render.args(&args);
    let out = run_piped_until(&mut render, clip, deadline)?;
    if out.len() != 64 * 64 * 3 {
        return None;
    }
    Some(out[(32 * 64 + 32) * 3])
}
