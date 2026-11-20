//! Preview proxies: their paths, the encode that makes them and the sidecar that describes them.

use super::*;

// ---- preview proxies -------------------------------------------------------

/// Cap on the proxy's width, in pixels (`scale='min(W,iw)'`). ~720p: small
/// enough that one frame decodes in a few ms, large enough to preview framing.
pub(super) const PROXY_MAX_WIDTH: u32 = 1280;

/// Cap on the proxy's width for spherical sources. A reframed preview crops
/// roughly a 100° window out of a 360° picture, so only about a quarter of the
/// proxy's width ever reaches the screen: at 1280 that leaves ~355 px of real
/// detail — visible mush. 3072 puts ~850 px across the shot while an all-intra
/// frame still decodes fast enough to scrub.
pub(super) const PROXY_MAX_WIDTH_SPHERICAL: u32 = 3072;

/// The width a flat asset's proxy is rendered at: the default is
/// [`PROXY_MAX_WIDTH`], and Settings can pick a smaller one (or turn proxies off,
/// which is the caller's business — this is only the size). It is part of the cache
/// key, so a proxy made at another size is a different file and the default's
/// cache stays good.
static PROXY_BASE_WIDTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(PROXY_MAX_WIDTH);

/// The width a flat asset's proxy is rendered at right now (see [`proxy_width`]).
pub fn proxy_base_width() -> u32 {
    PROXY_BASE_WIDTH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set the flat proxy width. Takes effect for every later lookup; an existing
/// proxy at the old size stays on disk, unused, until it is deleted.
pub fn set_proxy_base_width(width: u32) {
    PROXY_BASE_WIDTH.store(width.max(2), std::sync::atomic::Ordering::Relaxed);
}

/// The proxy width for an asset when flat proxies are `base` wide (pure,
/// unit-tested): `base` itself, or for a spherical source the same ratio the
/// default has (3072 for 1280), never past [`PROXY_MAX_WIDTH_SPHERICAL`]. (The cap dates from
/// proxies that were hardware-encoded, which stops at 4096 across; it stays.)
pub fn proxy_width_for(projection: Option<Projection>, base: u32) -> u32 {
    if projection.is_some_and(|p| p.is_spherical()) {
        (base.saturating_mul(PROXY_MAX_WIDTH_SPHERICAL) / PROXY_MAX_WIDTH).min(PROXY_MAX_WIDTH_SPHERICAL)
    } else {
        base
    }
}

/// The proxy width to render (and look up) an asset at. 360 footage is preserved
/// at a larger size because reframing throws most of the frame away.
pub fn proxy_width(projection: Option<Projection>) -> u32 {
    proxy_width_for(projection, proxy_base_width())
}

/// FNV-1a over `s`. A small, dependency-free, deterministic hash for naming a
/// source's proxy file — stability across sessions is what lets a re-import
/// reuse the cached proxy (a non-deterministic hasher would orphan it).
pub(in crate::engine) fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A content key for `src` that changes whenever the file is replaced: its path
/// plus size and modified time. Hashed into a cache file name so re-imports of
/// the same source reuse the cached artifact, while a swapped-out source
/// regenerates one. Shared by the proxy cache, the Insta360 stitch cache and
/// the waveform peak cache.
pub(crate) fn source_key(src: &Path) -> String {
    let meta = std::fs::metadata(src).ok();
    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}|{len}|{mtime}", src.display())
}

/// Which file a cached decode came from, as one number: [`source_key`] (path, size and
/// modified time) hashed, so it changes when the file is replaced and a cache keyed by it
/// never serves a stale frame. A proxy and its original are different paths, so different
/// identities. A path that cannot be read hashes with size 0 and mtime 0: stable, and it
/// changes the moment the file appears.
pub fn source_identity(src: &Path) -> u64 {
    fnv1a(&source_key(src))
}

/// How long after the container's start the video's first frame lies, in
/// seconds: the video stream's start less the container's (the earliest of all
/// its streams — audio at 0 and video at 0.08 s gives 0.08).
///
/// The number matters because ffmpeg's input `-ss T` is **relative to the
/// container's start**, not to the video's. A source that starts its video late
/// is asked for source time `T` and answers with the frame `lead` seconds into
/// the video's own timeline; a proxy is video-only, so its container *is* the
/// video and starts where the video does — which would make the same `-ss T`
/// land `lead` seconds further into the footage (see [`build_proxy_args`]).
/// Negative or missing values are `0.0`.
pub(super) fn head_lead(stream_start: Option<f64>, format_start: Option<f64>) -> f64 {
    match (stream_start, format_start) {
        (Some(stream), Some(format)) => (stream - format).max(0.0),
        _ => 0.0,
    }
}

/// The smallest lead worth a proxy of its own. Under a millisecond is a hair
/// inside the error of the container's own timestamps; anything above shifts a
/// seek by a whole frame for the stretch of each frame interval it covers.
pub(super) const HEAD_PAD_MIN: f64 = 0.001;

/// A video stream's [`StreamInfo::lead`] from its start and the earliest start of the file's
/// picture and sound streams (pure): the [`head_lead`] when it is above [`HEAD_PAD_MIN`], else
/// `0.0`. Unlike [`SourceTraits::lead`] a transport stream is measured too: the export reads
/// its sound beside its picture.
pub(crate) fn video_lead(stream_start: Option<f64>, av_start: Option<f64>) -> f64 {
    Some(head_lead(stream_start, av_start))
        .filter(|&lead| needs_head_pad(lead))
        .unwrap_or(0.0)
}

/// A lead as the file's `duration` allows it (pure): one that reaches the end of the file is
/// a broken start time, not a picture that never comes, and reads as none — otherwise every
/// window would start inside it and no clip of the file would ever be seeked.
pub(crate) fn lead_within(lead: f64, duration: f64) -> f64 {
    if lead < duration {
        lead
    } else {
        0.0
    }
}

/// Whether a source with this [`head_lead`] needs its proxy's head filled in.
pub(super) fn needs_head_pad(lead: f64) -> bool {
    lead > HEAD_PAD_MIN
}

/// The cache-key text of a proxy (pure, unit-tested): the source's identity and
/// the width, plus a suffix for each way the proxy was made differently from the
/// plain one. A suffix is only ever *added* to a source that needs it, so the
/// key of every ordinary SDR file with a normal start is what it always was and
/// its cached proxy stays good.
pub(super) fn proxy_key(source_key: &str, width: u32, tonemapped: bool, head_padded: bool) -> String {
    let tone = if tonemapped { "|sdr" } else { "" };
    let pad = if head_padded { "|lead" } else { "" };
    format!("{source_key}|{width}{tone}{pad}")
}

/// The on-disk path of `src`'s preview proxy at `width` (whether or not it
/// exists yet): `<cache>/kerf/proxies/<hash>.mp4`. `None` when no OS cache
/// directory is resolvable (a proxy simply can't be cached — preview falls back
/// to the original).
///
/// The width is part of the key, so an asset that is later marked as 360 (or
/// stops being one) looks up a different file and regenerates instead of
/// silently reusing a proxy rendered at the wrong size.
///
/// An HDR source also keys on the fact that its proxy is **tone-mapped**: a
/// proxy cached before the engine did that is an untouched HDR picture squeezed
/// into BT.709, and must be rebuilt rather than trusted. SDR keys are unchanged.
///
/// So does a source whose video **starts after its container** (`|lead`): a proxy
/// cached before the engine filled that head in has the video starting at its own
/// time zero, and a seek into it lands on the wrong frame (see [`head_lead`]).
/// FFmpeg 9 wrote exactly such proxies for every build that had the default
/// frame-sync mode of its day. Only these sources are rebuilt, and their proxy is
/// named `<hash>.lead.mp4` (`is_head_padded_proxy`, in `clip_timing`).
pub fn proxy_path(src: &Path, width: u32) -> Option<PathBuf> {
    proxy_path_for(src, width, source_traits(src).unwrap_or_default())
}

/// [`proxy_path`] for traits already read: an encode takes **one** reading for its destination and
/// for its filters, because a probe that failed is remembered only for a minute and an encode that
/// waited for the machine's slot can read again, and answer differently, before it starts — a
/// tone-mapped or head-padded picture would then be written to the plain proxy's name.
pub(super) fn proxy_path_for(src: &Path, width: u32, traits: SourceTraits) -> Option<PathBuf> {
    let dir = crate::paths::cache_dir()?.join("kerf").join("proxies");
    let padded = needs_head_pad(traits.lead);
    let key = proxy_key(&source_key(src), width, traits.hdr.is_some(), padded);
    Some(dir.join(proxy_file_name(fnv1a(&key), padded)))
}

/// Every file `src`'s proxy at `width` could be in the cache, with its sidecar: the four spellings
/// of the key (tone-mapped or not, head-padded or not), so it needs none of the probes
/// [`proxy_path`] runs and can be asked with a lock held. For the cache cap, which must not remove
/// what the open project plays from.
pub(crate) fn proxy_cache_files(src: &Path, width: u32) -> Vec<PathBuf> {
    let Some(dir) = crate::paths::cache_dir().map(|d| d.join("kerf").join("proxies")) else {
        return Vec::new();
    };
    let source = source_key(src);
    let mut files = Vec::with_capacity(8);
    for (tonemapped, padded) in [(false, false), (true, false), (false, true), (true, true)] {
        let proxy = dir.join(proxy_file_name(fnv1a(&proxy_key(&source, width, tonemapped, padded)), padded));
        files.push(proxy_sidecar_path(&proxy));
        files.push(proxy);
    }
    files
}

/// A proxy's file name (pure, unit-tested): `<hash>.mp4`, or `<hash>.lead.mp4` for
/// one made with a padded head.
pub(super) fn proxy_file_name(hash: u64, head_padded: bool) -> String {
    if head_padded {
        format!("{hash:016x}{HEAD_PADDED_SUFFIX}")
    } else {
        format!("{hash:016x}.mp4")
    }
}

/// The proxy for `src` at `width` **if it has already been generated** (the file
/// exists), for a preview path to decode from instead of the original; `None`
/// otherwise. Preview must never block on generation, so this is a pure
/// existence check.
pub fn ready_proxy(src: &Path, width: u32) -> Option<PathBuf> {
    proxy_path(src, width).filter(|p| p.is_file())
}

/// Delete `src`'s proxy at `width` and the sidecar beside it, returning the bytes
/// freed (0 when there was none). A proxy that is being written is a `.part` file
/// and is left to the encode that owns it.
pub fn remove_proxy_files(src: &Path, width: u32) -> u64 {
    let Some(proxy) = proxy_path(src, width) else {
        return 0;
    };
    let freed: u64 = [proxy.clone(), proxy_sidecar_path(&proxy)]
        .iter()
        .filter_map(|file| {
            let bytes = std::fs::metadata(file).ok()?.len();
            std::fs::remove_file(file).ok().map(|()| bytes)
        })
        .sum();
    crate::plan_epoch::bump_plan_epoch();
    freed
}

/// How many CPU threads a single preview-proxy encode may use. Follows the
/// engine's CPU budget (see [`cpu`]), except that it never takes the whole
/// machine even at 100% — an uncapped `libx264` grabs every core, and a proxy
/// is background work the user did not ask to wait for. `KERF_PROXY_THREADS`
/// still overrides it outright (clamped to >= 1).
fn proxy_threads(budget: cpu::Budget) -> usize {
    if let Some(n) = std::env::var("KERF_PROXY_THREADS").ok().and_then(|v| v.parse::<usize>().ok()) {
        return n.max(1);
    }
    budget.threads().min(budget.cores().saturating_sub(1).max(1)).max(1)
}

/// Constant-quality flags for encoder `vc` at software-CRF-scale `crf` — the
/// proxy / stitch analogue of the export's `push_video_opts`, spelling the same
/// intent per hardware family (each names its quality knob differently).
pub(super) fn quality_args(vc: &str, crf: u32) -> Vec<String> {
    let s = |v: &str| v.to_string();
    match enc_family(vc) {
        EncFamily::Software => vec![s("-preset"), s("veryfast"), s("-crf"), crf.to_string()],
        EncFamily::Nvenc => vec![s("-rc"), s("vbr"), s("-cq"), crf.to_string(), s("-b:v"), s("0")],
        EncFamily::Qsv => vec![s("-global_quality"), crf.to_string()],
        EncFamily::VideoToolbox => vec![s("-q:v"), crf_to_vt_quality(crf).to_string()],
        // The QP is the CRF's number, not `hw_qp`'s: this spells the stitch, a source that is
        // decoded again, where the extra bits cost disk and not picture. The export is what
        // the offset is for.
        EncFamily::Amf => {
            let qp = crf.to_string();
            vec![s("-rc"), s("cqp"), s("-qp_i"), qp.clone(), s("-qp_p"), qp]
        }
        EncFamily::Vaapi => unreachable!("a stitch or a proxy has no upload for a VAAPI encoder"),
    }
}

/// Encoder input pixel format: the hardware encoders all take `nv12` (some take
/// nothing else), the software path keeps its historical `yuv420p`. Both are
/// 4:2:0, so the decoded proxy looks the same either way. (A VAAPI encoder is fed by an
/// upload the export graph ends in, which a stitch and a proxy do not have.)
pub(super) fn encode_pix_fmt(vc: &str) -> &'static str {
    match enc_family(vc) {
        EncFamily::Software => "yuv420p",
        EncFamily::Vaapi => unreachable!("a stitch or a proxy has no upload for a VAAPI encoder"),
        _ => "nv12",
    }
}

/// The proxy's argv as [`generate_proxy_with`] runs it: **x264 unless `fast` is a plan**
/// ([`build_proxy_args`] with the encoder fixed to `libx264`, hardware decode only), and a plan only
/// ever comes from the opt-in Fast (GPU) pipeline for a source it vouched for (`fast_eligible`: no
/// tone-map, no head pad, nothing the hardware decoder lacks), whose argv is
/// [`fast_proxy_args`] and ignores the software-only inputs it was refused on. Pinned by a test over
/// every combination of the other inputs: with no plan the encoder is `libx264` whatever else is
/// asked.
#[allow(clippy::too_many_arguments)]
pub(super) fn proxy_encode_args(
    fast: Option<&FastPlan>,
    src: &str,
    dst: &str,
    threads: usize,
    width: u32,
    hw_decode: Option<&str>,
    tonemap: Option<&str>,
    head_pad: Option<&str>,
) -> Vec<String> {
    match fast {
        Some(plan) => fast_proxy_args(plan, src, dst),
        None => build_proxy_args(src, dst, threads, width, "libx264", hw_decode, tonemap, head_pad),
    }
}

/// Build the ffmpeg argument list (pure, unit-tested) that transcodes `src` into
/// an all-intra, audio-less preview proxy at `dst`, capped to `width` pixels
/// across (see [`proxy_width`]) and using at most `threads` CPU threads.
/// `encoder` is `libx264`: [`generate_proxy_with`] hands a proxy to a hardware encoder only
/// through the Fast (GPU) pipeline's own builder ([`fast_proxy_args`]); this one still spells the
/// hardware families' quality flags (`quality_args`, the stitch's); `hw_decode` adds an
/// input-side `-hwaccel`. `-g 1` makes every frame a keyframe, so a seek decodes
/// exactly one frame (instant scrub even on long-GOP 4K/HEVC). fps and duration
/// are left untouched — no `-r`, no `-t`, no trim — so a source time maps 1:1
/// onto the proxy and a preview seek lands on the same frame the export (which
/// always reads the original) would. `tonemap`, for an HDR source, is the chain
/// that brings it down to SDR BT.709 after the downscale: the **proxy is where
/// that conversion happens**, so everything that decodes it afterwards — the
/// preview stream, scrubbed stills, the composited frame — sees ordinary SDR and
/// converts nothing a second time. (Upright rotation is the decoder's: ffmpeg
/// autorotates and writes the proxy without a matrix.)
///
/// `head_pad` is for a source whose video starts after its container (see
/// [`head_lead`]), and carries the spelling of the frame-sync flag
/// ([`fps_mode_flag`]). The proxy has no audio to start the container early, so
/// without help its video would start at its own zero and an input `-ss T` would
/// land `lead` seconds past where the original's does — which is what an
/// FFmpeg-9 proxy of a late-starting video did. The fix keeps every timestamp
/// exactly and fills the gap the way the original answers a seek into it, by
/// holding the first frame: one clone of it at time zero, merged in front of the
/// stream by `interleave` (which orders by timestamp, so nothing is regridded).
/// That is not `-fps_mode cfr`, the other way to fill a head: cfr snaps every
/// frame to a grid anchored at zero, which is half a frame off for most leads
/// (a seek then picks the neighbouring frame at every frame boundary) and turns
/// a variable-frame-rate source into a constant one. The mode is spelled out as
/// `vfr` because FFmpeg 6 and older default an mp4 to cfr — which would undo it —
/// and FFmpeg 7 and newer to vfr. An ordinary source passes `None` and gets the
/// plain `-vf` chain it always did. The clone is a frame the original has no
/// counterpart for, so a reader that starts the proxy from zero without a seek —
/// which the original answers with its real first frame — drops it again (see
/// [`video_clip_chain`]); the proxy's file name says it has one
/// (`is_head_padded_proxy`, in `clip_timing`).
#[allow(clippy::too_many_arguments)]
pub(super) fn build_proxy_args(
    src: &str,
    dst: &str,
    threads: usize,
    width: u32,
    encoder: &str,
    hw_decode: Option<&str>,
    tonemap: Option<&str>,
    head_pad: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
    ];
    if let Some(hw) = hw_decode {
        args.push("-hwaccel".to_string());
        args.push(hw.to_string());
    }
    let chain = match tonemap {
        Some(t) => format!("scale='min({width},iw)':-2:flags=bilinear,{t}"),
        None => format!("scale='min({width},iw)':-2:flags=bilinear"),
    };
    args.extend(["-i".to_string(), src.to_string(), "-an".to_string()]);
    if head_pad.is_some() {
        args.extend([
            "-filter_complex".to_string(),
            format!("[0:v:0]{chain},split[a][b];[a]trim=end_frame=1,setpts=PTS-STARTPTS[h];[h][b]interleave[v]"),
            "-map".to_string(),
            "[v]".to_string(),
        ]);
    } else {
        args.extend(["-vf".to_string(), chain]);
    }
    args.extend(["-c:v".to_string(), encoder.to_string()]);
    args.extend(quality_args(encoder, 24));
    args.extend([
        "-g".to_string(),
        "1".to_string(),
        "-threads".to_string(),
        threads.max(1).to_string(),
        "-pix_fmt".to_string(),
        encode_pix_fmt(encoder).to_string(),
    ]);
    if let Some(flag) = head_pad {
        args.extend([flag.to_string(), "vfr".to_string()]);
    }
    args.extend([
        // The encode writes a `.part` temp file, whose extension tells ffmpeg
        // nothing — name the muxer instead of letting it guess, or it exits
        // with "unable to find a suitable output format" before decoding a frame.
        "-f".to_string(),
        "mp4".to_string(),
        dst.to_string(),
    ]);
    args
}

/// Generate the preview proxy for `src` if it isn't cached yet, returning its
/// path. A cache hit (the proxy already on disk) returns immediately — so this
/// is cheap to call for every asset on project open. The encode writes to a
/// per-process temp file and atomically renames it into place, so a partial /
/// interrupted / concurrent encode never leaves a half-written file that
/// [`ready_proxy`] would mistake for a finished proxy. Blocking; callers run it
/// off the project lock (e.g. on a background thread).
pub fn generate_proxy(src: &Path, width: u32) -> Result<PathBuf> {
    generate_proxy_with(
        src,
        width,
        ProxyRun {
            reservation: None,
            duration: None,
            progress: &mut |_| {},
            cancel: &|| false,
            pipeline: &mut |_| {},
        },
    )
}

/// What a caller hooks into a proxy encode (see [`generate_proxy_with`]).
pub struct ProxyRun<'a> {
    /// The place in front of the heavy-job queue the caller took when it queued the
    /// proxy ([`cpu::reserve`]); without one the encode asks for the slot in the
    /// same lane when it starts.
    pub reservation: Option<cpu::Reservation>,
    /// The source's length in seconds — what the encoder's position is measured
    /// against to give `progress` a fraction. Without it nothing is reported.
    pub duration: Option<f64>,
    /// Called with how far along the encode is, `0.0..1.0`: once with `0.0` when it holds the
    /// slot and starts (so a proxy queued behind an export is not shown at 0%), then about twice
    /// a second. A build that falls back to software after a Fast (GPU) attempt reports `0.0`
    /// again when the software encode starts.
    pub progress: &'a mut dyn FnMut(f64),
    /// Polled while queued and while encoding; once true the encode is killed, its
    /// partial file removed and [`Error::Cancelled`] returned.
    pub cancel: &'a dyn Fn() -> bool,
    /// Told which encode is running each time an attempt holds its slot and starts (just before its
    /// `0.0` report): x264, or a vendor's whole GPU pipeline. Twice for a proxy whose GPU build
    /// failed and was built again in software.
    pub pipeline: &'a mut dyn FnMut(ProxyPipeline),
}

/// [`generate_proxy`] that reports progress and can be abandoned.
///
/// The encode is a whole-file job in the **proxy** lane of the heavy-job queue: a proxy
/// is what the preview is waiting for (it decodes the original until the proxy lands),
/// where analysis is only wanted eventually, so it goes before every analysis that is
/// waiting — but after an export or anything else a user is waiting on. A job already
/// running is not interrupted.
///
/// It runs with `-progress` on a pipe (written here at spawn time, never in
/// [`build_proxy_args`]) and is killed after [`EXPORT_STALL`] without a word, so a
/// wedged decoder cannot hold the machine's one heavy-job slot for good.
///
/// With the opt-in **Fast (GPU)** setting on ([`proxy_fast_gpu`]), a source the pipeline can vouch
/// for ([`choose_pipeline`]) is first built with decode, scale and encode on the hardware, holding
/// the hardware's slot in the proxy lane instead of the cores'. A finished build is checked against
/// the source's own stream (`check_fast_output`), and one that fails, stalls or does not check out
/// is built again by x264 exactly as with the setting off (`fast_then_software`). With the setting
/// off nothing of that runs.
pub fn generate_proxy_with(src: &Path, width: u32, run: ProxyRun<'_>) -> Result<PathBuf> {
    let ProxyRun {
        reservation,
        duration,
        progress,
        cancel,
        pipeline,
    } = run;
    // One reading of the source answers everything below: where the proxy goes, whether to tone-map
    // it and whether its video starts late.
    let traits = source_traits(src).unwrap_or_default();
    let dst = proxy_path_for(src, width, traits)
        .ok_or_else(|| Error::Engine("no cache directory available for preview proxies".to_string()))?;
    if dst.is_file() {
        return Ok(dst);
    }
    if cancel() {
        return Err(Error::Cancelled);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Engine(format!("could not create proxy cache dir: {e}")))?;
    }
    let src_str = src
        .to_str()
        .ok_or_else(|| Error::Engine("asset path is not valid UTF-8".to_string()))?;
    // One name per encode: a rebuild that cancels the encode before it must not share a
    // temp file with the one that replaces it.
    static ENCODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = ENCODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dst.with_extension(format!("{}.{n}.part", std::process::id()));
    let tmp_str = tmp
        .to_str()
        .ok_or_else(|| Error::Engine("proxy temp path is not valid UTF-8".to_string()))?;
    let job = ProxyJob {
        src,
        src_str,
        dst: &dst,
        tmp: &tmp,
        tmp_str,
        width,
        duration,
        traits,
        cancel,
        bin: ffmpeg_bin(),
    };
    let hooks = std::cell::RefCell::new(Hooks {
        reservation,
        progress,
        pipeline,
    });
    match choose_pipeline(src, width, traits, cancel) {
        FastChoice::Software(why) => {
            if let Some(why) = why {
                tracing::info!(%why, "Fast (GPU) is on but this proxy is built in software");
            }
            software_build(&job, &mut hooks.borrow_mut())
        }
        FastChoice::Gpu(plan, expect) => fast_then_software(
            fast_health(),
            || fast_build(&job, &plan, &expect, &mut hooks.borrow_mut()),
            || software_build(&job, &mut hooks.borrow_mut()),
        ),
    }
}

/// What one proxy build is about, whichever encode makes it.
struct ProxyJob<'a> {
    src: &'a Path,
    src_str: &'a str,
    dst: &'a Path,
    tmp: &'a Path,
    tmp_str: &'a str,
    width: u32,
    duration: Option<f64>,
    traits: SourceTraits,
    cancel: &'a dyn Fn() -> bool,
    bin: String,
}

/// The caller's hooks, which the fast attempt and the software rebuild take turns using.
struct Hooks<'a> {
    reservation: Option<cpu::Reservation>,
    progress: &'a mut dyn FnMut(f64),
    pipeline: &'a mut dyn FnMut(ProxyPipeline),
}

/// A proxy with Fast (GPU): the hardware's slot in the proxy lane (the cores' place in front of the
/// background lane is given up first, since this build will not use the cores; with the hardware
/// lane off it is the cores' slot as ever), the vendor's whole pipeline, and a check of the result.
fn fast_build(job: &ProxyJob<'_>, plan: &FastPlan, expect: &FastExpect, hooks: &mut Hooks<'_>) -> FastEnd<PathBuf> {
    let cancel = job.cancel;
    let lease = if cpu::gpu_lane() {
        match cpu::lease_proxy_gpu_waiting(&mut |_| {}, cancel) {
            Ok(lease) => {
                drop(hooks.reservation.take());
                lease
            }
            Err(_) => return FastEnd::Cancelled,
        }
    } else {
        let waited = match hooks.reservation.take() {
            Some(reserved) => reserved.lease_waiting(&mut |_| {}, cancel),
            None => cpu::lease_priority_waiting(cpu::Priority::Proxy, &mut |_| {}, cancel),
        };
        match waited {
            Ok(lease) => lease,
            Err(_) => return FastEnd::Cancelled,
        }
    };
    if cancel() {
        return FastEnd::Cancelled;
    }
    (hooks.pipeline)(ProxyPipeline::Gpu(plan.vendor));
    if job.duration.is_some() {
        (hooks.progress)(0.0);
    }
    let granted = lease.budget();
    let threads = proxy_threads(granted);
    let mut args = proxy_encode_args(Some(plan), job.src_str, job.tmp_str, threads, job.width, None, None, None);
    cpu::limit_args(&mut args, granted.with_threads(threads));
    let removed = || {
        let _ = std::fs::remove_file(job.tmp);
    };
    let output = match run_ffmpeg_streamed(&job.bin, &args, job.duration, FAST_STALL, &mut *hooks.progress, cancel) {
        Ok(output) => output,
        Err(e) => {
            removed();
            return FastEnd::Failed(FastFailure::Other(e.to_string()));
        }
    };
    match output.outcome {
        StreamOutcome::Cancelled => {
            removed();
            return FastEnd::Cancelled;
        }
        StreamOutcome::Stalled => {
            removed();
            return FastEnd::Failed(FastFailure::Stalled);
        }
        StreamOutcome::Ended => {}
    }
    if !output.status.success() {
        removed();
        return FastEnd::Failed(classify_fast_failure(&output.stderr));
    }
    // The guard: what the software encode would have made of this source, from the source's own stream.
    if let Err(why) = read_fast_output(job.tmp).and_then(|read| check_fast_output(expect, &read)) {
        removed();
        return FastEnd::Failed(FastFailure::Other(format!(
            "the proxy was not what x264 would have made: {why}"
        )));
    }
    let encoder = plan.vendor.encoder();
    match finalize_proxy(
        job.tmp,
        job.dst,
        || {
            probe(job.tmp)
                .ok()
                .and_then(|p| p.streams.into_iter().find(|s| s.kind == StreamKind::Video))
        },
        Some(encoder),
    ) {
        Ok(path) => FastEnd::Done(path),
        Err(e) => {
            removed();
            FastEnd::Failed(FastFailure::Other(e.to_string()))
        }
    }
}

/// A proxy with x264 on the cores' slot, hardware decode where there is one: what every proxy was
/// before the Fast (GPU) pipeline, and still is with it off.
fn software_build(job: &ProxyJob<'_>, hooks: &mut Hooks<'_>) -> Result<PathBuf> {
    let ProxyJob {
        src,
        src_str,
        dst,
        tmp,
        tmp_str,
        width,
        duration,
        traits,
        cancel,
        ..
    } = *job;
    let bin = &job.bin;
    // A full-file re-encode. Importing a folder queues them one behind the next
    // rather than starting one per file at once; they wait in the proxy lane, behind
    // anything a user is waiting on and ahead of analysis. A proxy that is deleted, rebuilt
    // or switched off while it waits gives up its place without having started.
    let cpu = match hooks.reservation.take() {
        Some(reserved) => reserved.lease_waiting(&mut |_| {}, cancel)?,
        None => cpu::lease_priority_waiting(cpu::Priority::Proxy, &mut |_| {}, cancel)?,
    };
    // Cancelled in the instant it got the slot: do not start the encode.
    if cancel() {
        return Err(Error::Cancelled);
    }
    (hooks.pipeline)(ProxyPipeline::Software);
    // The first report says the encode has begun (it held no slot until now).
    if duration.is_some() {
        (hooks.progress)(0.0);
    }
    let granted = cpu.budget();
    let threads = proxy_threads(granted);
    let tonemap = traits.hdr.map(tonemap_filter);
    let head_pad = needs_head_pad(traits.lead).then(fps_mode_flag);
    let mut run = |hw_decode: Option<&str>| -> Result<Streamed> {
        let mut args = proxy_encode_args(
            None,
            src_str,
            tmp_str,
            threads,
            width,
            hw_decode,
            tonemap.as_deref(),
            head_pad,
        );
        cpu::limit_args(&mut args, granted.with_threads(threads));
        run_ffmpeg_streamed(bin, &args, duration, EXPORT_STALL, &mut *hooks.progress, cancel)
    };
    // The decode is the GPU's when there is one; the encode is x264's unless the opt-in Fast (GPU)
    // pipeline took the proxy (it was not given this one, or it failed). A hardware H.264 intra
    // pass behind a *software scale* costs the same wall time as x264's (1.85 s against 1.90 s for
    // 8 s of 4K, which downloaded every frame and scaled on the cores), saves about 0.4 CPU-s and
    // writes proxies a third larger; the fast pipeline is the one with no download. A decode
    // failure falls back to software once.
    let hw_dec = decode_hwaccel_for(src, Reading::Whole);
    let mut output = run(hw_dec.as_deref())?;
    if output.outcome == StreamOutcome::Cancelled {
        let _ = std::fs::remove_file(tmp);
        return Err(Error::Cancelled);
    }
    if retry_proxy_in_software(output.outcome, output.status.success(), hw_dec.is_some()) {
        let _ = std::fs::remove_file(tmp);
        let err = if output.outcome == StreamOutcome::Stalled {
            // A hardware decoder that hangs is as broken as one that refuses, and costs five
            // minutes a proxy to find out: turn it off for the process, like any other
            // hardware failure, and go again in software.
            disable_decode_hwaccel();
            format!("ffmpeg stopped reporting progress for {}s", EXPORT_STALL.as_secs())
        } else {
            output.stderr.trim().to_string()
        };
        output = run(None)?;
        if output.outcome == StreamOutcome::Cancelled {
            let _ = std::fs::remove_file(tmp);
            return Err(Error::Cancelled);
        }
        if output.status.success() {
            // The decoder is the culprit only if the failure reads as a hardware one: a named
            // accelerator such as `d3d11va` exits non-zero on a machine with no device, where
            // `auto` fell back quietly, so without this every proxy would pay a failed launch
            // first; a full disk or a bad file must not turn it off for the process. (A
            // *stall* is convicted before the rerun, as it always was.)
            if looks_like_hardware_decode_failure(&err) {
                convict_hardware_decode();
            }
            tracing::warn!(error = %err, "hardware-accelerated proxy decode failed; using software");
        }
    }
    if output.outcome == StreamOutcome::Stalled {
        let _ = std::fs::remove_file(tmp);
        return Err(Error::Engine(format!(
            "could not generate preview proxy: ffmpeg stopped reporting progress for {}s and was stopped",
            EXPORT_STALL.as_secs()
        )));
    }
    if !output.status.success() {
        let _ = std::fs::remove_file(tmp);
        return Err(Error::Engine(format!(
            "could not generate preview proxy: {}",
            proxy_failure_reason(&output.stderr)
        )));
    }
    // Another generator may have finished the same proxy while we encoded: theirs is in place
    // with its own sidecar, and ours is redundant.
    finalize_proxy(
        tmp,
        dst,
        || {
            probe(tmp)
                .ok()
                .and_then(|p| p.streams.into_iter().find(|s| s.kind == StreamKind::Video))
        },
        None,
    )
}

/// What a failed proxy encode says of itself (pure, unit-tested): the end of ffmpeg's log, never
/// the whole of it, and in words when the file is the cause. A clip whose copy was cut short is
/// full length on disk and zeros from some point on: ffmpeg wrote four lines for each of its 2800
/// undecodable frames, the reason was 775 KB, and every `proxy_status` and `get_asset_metadata`
/// answer carried it.
pub(super) fn proxy_failure_reason(stderr: &str) -> String {
    const LINES: usize = 8;
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(LINES)..].join("\n");
    let tail: String = tail.chars().rev().take(1200).collect::<Vec<_>>().into_iter().rev().collect();
    let damaged = stderr.contains("Decode error rate")
        || stderr.matches("Invalid data found when processing input").count() > 20;
    if damaged {
        format!(
            "the file is damaged: most of its picture cannot be decoded (an incomplete copy reads like this; copy it again from the card or a backup)\n{tail}"
        )
    } else {
        tail
    }
}

/// Whether a proxy encode that used hardware decode is run again in software (pure,
/// unit-tested): when it failed on its own **or stalled** — a cancelled one is not a failure,
/// and an encode that was software already has nothing to fall back to.
pub(super) fn retry_proxy_in_software(outcome: StreamOutcome, success: bool, hw_decode: bool) -> bool {
    !success && matches!(outcome, StreamOutcome::Ended | StreamOutcome::Stalled) && hw_decode
}

/// Put the finished encode `tmp` in place as `dst`, with its sidecar. If `dst` is already there
/// (a concurrent generator got there first) it is left as it is — **its sidecar stays too, no
/// new one is written** — and `tmp` is dropped. Otherwise `probe` describes `tmp` and the
/// sidecar is written *before* the rename, so a reader that finds the proxy finds its
/// description; a failed probe only costs the sidecar (the reader probes once and writes it).
pub(super) fn finalize_proxy(
    tmp: &Path,
    dst: &Path,
    probe: impl FnOnce() -> Option<StreamInfo>,
    encoder: Option<&str>,
) -> Result<PathBuf> {
    if dst.is_file() {
        let _ = std::fs::remove_file(tmp);
        return Ok(dst.to_path_buf());
    }
    if let Some(video) = probe() {
        write_proxy_sidecar(dst, tmp, video, encoder);
    }
    std::fs::rename(tmp, dst).map_err(|e| Error::Engine(format!("could not finalize preview proxy: {e}")))?;
    crate::plan_epoch::bump_plan_epoch();
    Ok(dst.to_path_buf())
}

/// What `generate_proxy` writes beside a proxy (`<hash>.json` next to `<hash>.mp4`, the
/// same name a `.lead.mp4` proxy's sidecar takes with its own ending): the video stream
/// **of the proxy file** — its size, pixel format and colour tags, not the original's.
///
/// A proxy is a smaller, always-`yuv420p`, upright picture, so what a renderer that decodes
/// it has to know about its layer (geometry, a format it refuses, a matrix) is a fact about
/// the proxy. Asking `ffprobe` for it would put a process spawn on the interactive path; the
/// sidecar makes it a small file read. It carries the proxy's byte size so a proxy that was
/// replaced under the same name does not keep describing the one that is gone.
///
/// It also says **which encoder wrote the proxy** (`encoder`: `h264_amf`, `h264_nvenc`, ...), absent
/// for x264, which is every proxy made before the Fast (GPU) pipeline existed, so those stay valid.
/// What a read that depends on the proxy's pixels (a scene score) asks of it
/// ([`proxy_hardware_encoded`]): a hardware encoder's quantisation is not x264's, and the few
/// hundredths a score may differ by were only measured for x264.
#[derive(serde::Serialize, serde::Deserialize)]
struct ProxySidecar {
    version: u32,
    size: u64,
    video: StreamInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encoder: Option<String>,
}

const PROXY_SIDECAR_VERSION: u32 = 1;

/// Where the sidecar of `proxy` lives.
pub(crate) fn proxy_sidecar_path(proxy: &Path) -> PathBuf {
    proxy.with_extension("json")
}

/// A temp name for writing the sidecar at `dst`, **unique per writer** (the process and a
/// counter: two threads writing the same sidecar must not share one) and not the proxy's own
/// (`<hash>.<pid>.part`), which the write runs beside.
pub(super) fn sidecar_temp(dst: &Path) -> PathBuf {
    static WRITERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    dst.with_extension(format!("json.{}.{n}.part", std::process::id()))
}

/// Write `proxy`'s sidecar, describing `file` (the proxy itself, or the temp it is being
/// written as). Best effort and atomic (temp file, rename): the worst a failure does is make
/// the reader probe.
pub(super) fn write_proxy_sidecar(proxy: &Path, file: &Path, video: StreamInfo, encoder: Option<&str>) {
    let Ok(size) = std::fs::metadata(file).map(|m| m.len()) else {
        return;
    };
    let sidecar = ProxySidecar {
        version: PROXY_SIDECAR_VERSION,
        size,
        video,
        encoder: encoder.map(str::to_string),
    };
    let dst = proxy_sidecar_path(proxy);
    let tmp = sidecar_temp(&dst);
    let written = serde_json::to_vec(&sidecar)
        .ok()
        .is_some_and(|bytes| std::fs::write(&tmp, bytes).is_ok())
        && std::fs::rename(&tmp, &dst).is_ok();
    if !written {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The sidecar of `proxy` if it is there, current and describes this very file.
pub(super) fn read_proxy_sidecar(proxy: &Path) -> Option<StreamInfo> {
    let sidecar: ProxySidecar = serde_json::from_slice(&std::fs::read(proxy_sidecar_path(proxy)).ok()?).ok()?;
    let size = std::fs::metadata(proxy).ok()?.len();
    (sidecar.version == PROXY_SIDECAR_VERSION && sidecar.size == size).then_some(sidecar.video)
}

/// The encoder `proxy`'s sidecar says wrote it, **even a stale one** (the proxy's size no longer
/// matches), `None` for x264 and for a proxy with no readable sidecar. Stale is wanted: a sidecar
/// written again for a proxy that lost its own must not turn a hardware proxy into an x264 one.
pub(super) fn stored_proxy_encoder(proxy: &Path) -> Option<String> {
    let sidecar: ProxySidecar = serde_json::from_slice(&std::fs::read(proxy_sidecar_path(proxy)).ok()?).ok()?;
    sidecar.encoder
}

/// Whether a hardware encoder wrote the proxy at `proxy` (its sidecar says so; x264's and
/// a proxy that never had a sidecar do not). A scene **score** read from such a proxy is not the
/// original's within the few hundredths measured for x264, so the analysis reads the original for
/// it (`Project::scoring_proxy`); a time (a contact sheet) is the same either way, since the guard
/// checked the frames and timestamps against the source's.
pub fn proxy_hardware_encoded(proxy: &Path) -> bool {
    stored_proxy_encoder(proxy).is_some_and(|encoder| enc_family(&encoder) != EncFamily::Software)
}

/// How long a failed probe of a proxy is remembered before it is tried again.
const PROXY_PROBE_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// The video stream of the proxy at `proxy`, as decoding it yields it: from its sidecar, and
/// for a proxy that has none (one made before sidecars existed) from **one** `ffprobe`,
/// remembered per file (size and modified time) and written back as the sidecar so no later
/// run asks again. `None` when the proxy cannot be read; the caller decodes the original.
///
/// The fallback spawns a process: call it off the project lock, never from a render loop.
/// A plan resolves it once per `Planner`.
pub(crate) fn proxy_video_info(proxy: &Path) -> Option<StreamInfo> {
    /// What one probe of a proxy found, and when.
    type Probed = (Instant, Option<StreamInfo>);
    static CACHE: OnceLock<Mutex<HashMap<String, Probed>>> = OnceLock::new();
    if let Some(video) = read_proxy_sidecar(proxy) {
        return Some(video);
    }
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = source_key(proxy);
    let remembered = cache.lock().ok().and_then(|c| c.get(&key).cloned());
    if let Some((at, found)) = remembered {
        if found.is_some() || at.elapsed() < PROXY_PROBE_RETRY {
            return found;
        }
    }
    let found = probe(proxy)
        .ok()
        .and_then(|p| p.streams.into_iter().find(|s| s.kind == StreamKind::Video));
    if let Some(video) = &found {
        write_proxy_sidecar(proxy, proxy, video.clone(), stored_proxy_encoder(proxy).as_deref());
    }
    if let Ok(mut c) = cache.lock() {
        c.insert(key, (Instant::now(), found.clone()));
    }
    found
}
