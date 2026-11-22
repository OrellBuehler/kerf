//! `ffprobe` on a file: its JSON read into the streams of a `ProbeResult`.

use super::*;

// ---- probe -----------------------------------------------------------------

#[derive(serde::Deserialize)]
pub(super) struct ProbeJson {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    format: Option<ProbeFormat>,
}

#[derive(serde::Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

#[derive(serde::Deserialize)]
struct ProbeStream {
    index: u32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    /// The stream's tick, `"1/1000"` (ffprobe prints it for every stream).
    time_base: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u16>,
    duration: Option<String>,
    start_time: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    color_space: Option<String>,
    color_range: Option<String>,
    pix_fmt: Option<String>,
    /// How the stream's fields are stored (`progressive`, `tt`, ...): only the Fast (GPU) proxy asks.
    field_order: Option<String>,
    /// The stream's pixel shape (`"1:1"`): only the Fast (GPU) proxy asks.
    sample_aspect_ratio: Option<String>,
    /// The frames the container's index holds, as ffprobe prints it (a string; absent for a format
    /// with no index): only the Fast (GPU) proxy asks.
    nb_frames: Option<String>,
    /// `attached_pic` is a cover picture, which is a video stream that is no film.
    #[serde(default)]
    disposition: Option<std::collections::HashMap<String, i64>>,
    /// Container-level stream tags. Only `rotate` matters: FFmpeg before 6.0
    /// reported a phone's turn as this tag (clockwise) as well as in the
    /// `Display Matrix` side data (counter-clockwise).
    #[serde(default)]
    tags: Option<std::collections::HashMap<String, String>>,
    /// Stream-level side data, which is where a spherical mapping surfaces. The
    /// mov demuxer fills this from the `sv3d` box (and the legacy Google
    /// spatial-media `uuid` blob), and `-show_streams` already prints it — no
    /// extra ffprobe flag is needed. (`-export_side_data` is a *decoder* option
    /// for film grain / motion vectors and is unrelated.)
    #[serde(default)]
    side_data_list: Option<Vec<ProbeSideData>>,
}

#[derive(serde::Deserialize)]
struct ProbeSideData {
    side_data_type: Option<String>,
    projection: Option<String>,
    rotation: Option<f64>,
}

/// How long `ffprobe` may take over one file before it is killed. It reads headers, so this is a
/// file on a share that stopped answering, not a big file — but a share that is merely *slow to
/// wake* (a NAS spinning its disks up, a VPN coming back) takes a while to answer its first
/// read, and ffprobe prints nothing until it is done, so there is no progress to build a stall
/// bound on: five minutes, and a cancel ends the wait sooner.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// How long the once-per-process capability probes (`-filters`, `-h full`, `-h filter=…`) may
/// take. They run inside a `OnceLock` every other thread waits on, and a cold first launch
/// under a virus scan is slow, so the bound is generous; a binary that does not answer reads
/// as the probe's "cannot run at all" case.
pub(super) const CAPABILITY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Probe a media file via `ffprobe -of json`.
// In a full `ffmpeg` build the in-process libav probe is used instead.
#[cfg_attr(feature = "ffmpeg", allow(dead_code))]
pub fn probe(path: &Path) -> Result<ProbeResult> {
    probe_with(path, None)
}

/// [`probe`] that gives up with [`Error::Cancelled`] once `cancel` is true (ffprobe is killed).
// In a full `ffmpeg` build the in-process libav probe is used instead.
#[cfg_attr(feature = "ffmpeg", allow(dead_code))]
pub fn probe_cancellable(path: &Path, cancel: &dyn Fn() -> bool) -> Result<ProbeResult> {
    probe_with(path, Some(cancel))
}

#[cfg_attr(feature = "ffmpeg", allow(dead_code))]
fn probe_with(path: &Path, cancel: Option<&dyn Fn() -> bool>) -> Result<ProbeResult> {
    probe_formats(path, cancel, crate::paths::demuxer_whitelist())
}

/// The `ffprobe` arguments of a probe of `path`: `-format_whitelist` only when `formats` names some
/// (a confined run), so every other probe's argv is unchanged.
pub(super) fn probe_args(path: &Path, formats: Option<&str>) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = ["-v", "error", "-show_format", "-show_streams", "-of", "json"]
        .into_iter()
        .map(Into::into)
        .collect();
    if let Some(formats) = formats {
        args.extend(["-format_whitelist".into(), formats.into()]);
    }
    args.push(path.into());
    args
}

/// [`probe_with`], held to the demuxers `formats` names when it names some.
pub(super) fn probe_formats(path: &Path, cancel: Option<&dyn Fn() -> bool>, formats: Option<&str>) -> Result<ProbeResult> {
    let bin = ffprobe_bin();
    let mut cmd = command(&bin);
    cmd.args(probe_args(path, formats));
    let output = output_bounded_cancellable(cmd, PROBE_TIMEOUT, cancel)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::TimedOut => Error::Engine(format!(
                "ffprobe did not answer for {} in {} s and was stopped (is the file on a share that went away?)",
                path.display(),
                PROBE_TIMEOUT.as_secs()
            )),
            _ => launch_err(&bin, e),
        })?
        .ok_or(Error::Cancelled)?;
    if !output.status.success() {
        return Err(Error::Engine(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let parsed: ProbeJson =
        serde_json::from_slice(&output.stdout).map_err(|e| Error::Engine(format!("could not parse ffprobe output: {e}")))?;
    Ok(probe_from_json(parsed, Some(path)))
}

pub(super) fn probe_from_json(parsed: ProbeJson, path: Option<&Path>) -> ProbeResult {
    let format_dur = parsed
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .and_then(|d| d.parse::<f64>().ok());

    // A still image probes as a lone video stream in an image codec with no
    // playable duration and no audio. We mark that stream so the engine loops it
    // for the clip length on export and decodes its single frame without seeking.
    // A still has no real timeline; we treat a sub-second duration as "none" so the
    // probe is robust whether the demuxer reports N/A (ffprobe) or a single frame's
    // worth (libav). The image-codec guard keeps short *videos* from being misread.
    let video_count = parsed
        .streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("video"))
        .count();
    let has_audio = parsed.streams.iter().any(|s| s.codec_type.as_deref() == Some("audio"));
    let still = video_count == 1
        && !has_audio
        && format_dur.unwrap_or(0.0) < STILL_MAX_DURATION
        && parsed
            .streams
            .iter()
            .any(|s| s.codec_type.as_deref() == Some("video") && is_still_codec(s.codec_name.as_deref()));

    let secs = |t: Option<&String>| t.and_then(|t| t.trim().parse::<f64>().ok()).filter(|t| t.is_finite());
    // A video's lead is measured from the earliest picture or sound, not the container's start:
    // a subtitle or data track can start the container earlier, and the export rebases the
    // sound to its own first sample as it does the picture, so only the gap between them is
    // a lead (the libav probe measures the same).
    let av_start = parsed
        .streams
        .iter()
        .filter(|s| matches!(s.codec_type.as_deref(), Some("video" | "audio")))
        .filter_map(|s| secs(s.start_time.as_ref()))
        .reduce(f64::min);
    let mut streams = Vec::new();
    let mut max_stream_dur = 0.0_f64;
    for s in &parsed.streams {
        let kind = match s.codec_type.as_deref() {
            Some("video") => StreamKind::Video,
            Some("audio") => StreamKind::Audio,
            Some("subtitle") => StreamKind::Subtitle,
            _ => StreamKind::Data,
        };
        if let Some(d) = s.duration.as_deref().and_then(|d| d.parse::<f64>().ok()) {
            max_stream_dur = max_stream_dur.max(d);
        }
        let is_video = kind == StreamKind::Video;
        let rotation = if is_video { display_rotation(s) } else { 0 };
        let (width, height) = displayed_size(s.width, s.height, rotation);
        streams.push(StreamInfo {
            index: s.index,
            kind,
            codec: s.codec_name.clone().unwrap_or_default(),
            width,
            height,
            fps: nominal_fps(
                s.r_frame_rate.as_deref().and_then(parse_rational),
                s.avg_frame_rate.as_deref().and_then(parse_rational),
            ),
            sample_rate: s.sample_rate.as_deref().and_then(|r| r.parse().ok()),
            channels: s.channels,
            image: still && kind == StreamKind::Video,
            projection: if is_video { detect_projection(path, s) } else { None },
            rotation,
            color_transfer: if is_video {
                known_color(s.color_transfer.as_deref())
            } else {
                None
            },
            color_primaries: if is_video {
                known_color(s.color_primaries.as_deref())
            } else {
                None
            },
            pix_fmt: if is_video {
                s.pix_fmt.clone().filter(|p| !p.is_empty() && p != "none")
            } else {
                None
            },
            color_space: if is_video {
                known_color(s.color_space.as_deref())
            } else {
                None
            },
            color_range: if is_video { range_tag(s.color_range.as_deref()) } else { None },
            lead: if is_video && !still {
                video_lead(secs(s.start_time.as_ref()), av_start)
            } else {
                0.0
            },
            time_base: if is_video && !still {
                s.time_base.as_deref().and_then(parse_time_base)
            } else {
                None
            },
        });
    }
    let duration = format_dur.unwrap_or(max_stream_dur).max(0.0);
    for s in &mut streams {
        s.lead = lead_within(s.lead, duration);
    }
    ProbeResult { duration, streams }
}

/// How the first picture stream of a file is *stored*, beyond what a [`StreamInfo`] keeps: the facts
/// the Fast (GPU) proxy refuses a source on (`proxy_fast::fast_eligible`) and checks its output
/// against. Read from the same `-show_streams` JSON the import probe reads; nothing here is stored
/// in a project.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct StoredVideo {
    /// Picture streams, cover art not counted: a file with two is not one `-vf` can be said to read.
    pub video_streams: usize,
    /// The coded size, before any rotation.
    pub width: u32,
    pub height: u32,
    /// How far the picture is turned for display (see `display_rotation`): the proxy is written upright.
    pub rotation: i16,
    pub field_order: Option<String>,
    pub sample_aspect_ratio: Option<String>,
    /// Frames the container says it holds; `None` for a format whose index does not count them.
    pub nb_frames: Option<u64>,
    /// The stream's own length in seconds.
    pub duration: Option<f64>,
    pub color_space: Option<String>,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub color_range: Option<String>,
}

/// [`StoredVideo`] of the first picture stream of a probe's JSON, `None` for a file with no
/// picture (or a stream with no size).
pub(super) fn stored_video(parsed: &ProbeJson) -> Option<StoredVideo> {
    let video = |s: &&ProbeStream| {
        s.codec_type.as_deref() == Some("video")
            && s.disposition
                .as_ref()
                .and_then(|d| d.get("attached_pic"))
                .copied()
                .unwrap_or(0)
                == 0
    };
    let first = parsed.streams.iter().find(video)?;
    let number = |text: Option<&String>| text.and_then(|t| t.trim().parse::<f64>().ok()).filter(|n| n.is_finite());
    Some(StoredVideo {
        video_streams: parsed.streams.iter().filter(video).count(),
        width: first.width?,
        height: first.height?,
        rotation: display_rotation(first),
        field_order: first.field_order.clone().filter(|f| !f.is_empty()),
        sample_aspect_ratio: first.sample_aspect_ratio.clone().filter(|f| !f.is_empty()),
        nb_frames: first
            .nb_frames
            .as_deref()
            .and_then(|n| n.trim().parse::<u64>().ok())
            .filter(|n| *n > 0),
        duration: number(first.duration.as_ref()).filter(|d| *d > 0.0),
        color_space: known_color(first.color_space.as_deref()),
        color_primaries: known_color(first.color_primaries.as_deref()),
        color_transfer: known_color(first.color_transfer.as_deref()),
        color_range: range_tag(first.color_range.as_deref()),
    })
}

/// How far a video stream is turned for display, in whole degrees
/// counter-clockwise in `(-180, 180]`, from its `Display Matrix` side data (or,
/// on an FFmpeg old enough to lack that, the clockwise `rotate` tag). 0 for an
/// unturned stream.
fn display_rotation(s: &ProbeStream) -> i16 {
    let raw = s.side_data_list.iter().flatten().find_map(|sd| sd.rotation).or_else(|| {
        let tag: f64 = s.tags.as_ref()?.get("rotate")?.trim().parse().ok()?;
        Some(-tag)
    });
    let Some(raw) = raw.filter(|r| r.is_finite()) else {
        return 0;
    };
    let mut deg = raw.round().rem_euclid(360.0) as i16;
    if deg > 180 {
        deg -= 360;
    }
    deg
}

/// The rotation a 3x3 display matrix (nine 16.16 fixed-point values in file
/// order, as libav hands them over) describes, in whole degrees counter-clockwise
/// in `(-180, 180]` — `av_display_rotation_get`, for the libav probe, which gets
/// the raw matrix where ffprobe gets the angle.
#[cfg_attr(not(feature = "ffmpeg"), allow(dead_code))]
pub(crate) fn matrix_rotation(m: &[i32; 9]) -> i16 {
    let f = |i: usize| m[i] as f64 / 65536.0;
    let (s0, s1) = (f(0).hypot(f(3)), f(1).hypot(f(4)));
    if s0 == 0.0 || s1 == 0.0 {
        return 0;
    }
    let deg = -(f(1) / s1).atan2(f(0) / s0).to_degrees();
    let mut deg = deg.round().rem_euclid(360.0) as i16;
    if deg > 180 {
        deg -= 360;
    }
    deg
}

/// The size a stream is *shown* at: the coded size, swapped when the stream is
/// turned a quarter. Every FFmpeg decode autorotates, so this is the size of the
/// pixels the engine actually receives — the one every piece of frame geometry
/// (the project frame, fit, crop, smart crop) has to be done against.
pub(crate) fn displayed_size(width: Option<u32>, height: Option<u32>, rotation: i16) -> (Option<u32>, Option<u32>) {
    if rotation.rem_euclid(180) == 90 {
        (height, width)
    } else {
        (width, height)
    }
}

/// A colour tag as the engine stores it: `None` when the file does not say,
/// rather than ffprobe's literal `"unknown"` — or `"reserved"`, a value no matrix has, which
/// FFmpeg reads as it reads none.
pub(crate) fn known_color(tag: Option<&str>) -> Option<String> {
    tag.filter(|t| !t.is_empty() && !matches!(*t, "unknown" | "unspecified" | "reserved"))
        .map(str::to_string)
}

/// A range tag as the engine stores it: `"pc"` (full) or `"tv"` (limited), `None` when the file does
/// not say (ffprobe prints `unknown`, or nothing). ffprobe's own spellings are `tv` / `pc`; the
/// libav names (`mpeg` / `jpeg`) and the plain words are folded into them.
pub(crate) fn range_tag(tag: Option<&str>) -> Option<String> {
    match tag?.trim().to_ascii_lowercase().as_str() {
        "pc" | "jpeg" | "full" => Some("pc".to_string()),
        "tv" | "mpeg" | "limited" => Some("tv".to_string()),
        _ => None,
    }
}

/// The frame rate to build a project around. `r_frame_rate` is "the lowest rate
/// all timestamps are a multiple of", which for a variable-frame-rate phone clip
/// can be a multiple of the real one (a ~30 fps clip with jittery timestamps
/// probes as 120) and would turn the whole export into a 4x-duplicated render.
/// When it runs far past the stream's average the average, snapped to the
/// nearest standard rate, is the nominal rate; otherwise `r_frame_rate` stands,
/// since a clip that merely dropped a few frames still belongs to its nominal rate.
pub(crate) fn nominal_fps(r_frame_rate: Option<f64>, avg_frame_rate: Option<f64>) -> Option<f64> {
    const STANDARD: [f64; 12] = [23.976, 24.0, 25.0, 29.97, 30.0, 48.0, 50.0, 59.94, 60.0, 100.0, 119.88, 120.0];
    let r = r_frame_rate.filter(|r| *r > 0.0);
    let avg = avg_frame_rate.filter(|a| *a > 0.0);
    match (r, avg) {
        (Some(r), Some(avg)) if r > avg * 1.5 => Some(
            STANDARD
                .iter()
                .copied()
                .filter(|s| (s - avg).abs() / s < 0.03)
                .min_by(|a, b| (a - avg).abs().total_cmp(&(b - avg).abs()))
                .unwrap_or(avg),
        ),
        (Some(r), _) => Some(r),
        (None, avg) => avg,
    }
}

/// FFmpeg `codec_name`s for single-frame still images. Animated containers
/// (gif/webp/apng) only land here when they probe with no playable duration —
/// i.e. they really are one frame; an animated one keeps a real duration and is
/// treated as ordinary video.
/// A probed duration below this (seconds) counts as "no real duration" when
/// deciding whether an image-codec stream is a still vs. an animated/looping one.
pub(crate) const STILL_MAX_DURATION: f64 = 1.0;

pub(crate) fn is_still_codec(codec: Option<&str>) -> bool {
    matches!(
        codec,
        Some(
            "png"
                | "mjpeg"
                | "jpeg"
                | "jpegls"
                | "bmp"
                | "gif"
                | "webp"
                | "tiff"
                | "ppm"
                | "pgm"
                | "pgmyuv"
                | "pam"
                | "targa"
                | "tga"
                | "qoi"
                | "apng"
                | "jpeg2000"
                | "j2k"
                | "heif"
                | "heic"
        )
    )
}

/// Decide whether a probed video stream is 360 footage, and in which projection.
///
/// Two signals, strongest first:
///
/// 1. A `Spherical Mapping` side-data entry declaring an equirectangular
///    projection. This is authoritative — it is what a stitched export (Insta360
///    Studio, the Google spatial-media injector, YouTube-ready files) writes.
/// 2. An Insta360 `.insv` whose frame is two squares side by side — the raw
///    dual-fisheye shape (a 5.7K capture probes as 5760x2880, i.e. two 2880x2880
///    circular hemispheres).
///
/// Deliberately **no** bare aspect-ratio guess. A 2:1 frame is a real and common
/// shape for anamorphic masters, ultrawide edits and ordinary stitched panoramas,
/// and the costs are lopsided: a missed detection costs one click in the
/// Inspector, whereas a false positive silently reprojects ordinary footage and
/// changes the export resolution out from under the user. Everything past these
/// two signals is a manual override.
fn detect_projection(path: Option<&Path>, s: &ProbeStream) -> Option<Projection> {
    if let Some(list) = &s.side_data_list {
        for sd in list {
            if sd.side_data_type.as_deref() == Some("Spherical Mapping") {
                return spherical_side_data_projection(sd.projection.as_deref());
            }
        }
    }
    projection_from_shape(path, s.width, s.height)
}

/// Signal 1: map the `projection` field of a `Spherical Mapping` side-data entry.
/// `None` for the field covers older ffmpeg builds that name the side data but not
/// its projection — equirect is the only mono-360 projection Insta360 and the
/// spatial-media spec actually emit. Cubemap / mesh projections exist but are not
/// something we can reframe faithfully, so they stay flat rather than being
/// reprojected wrongly.
pub(crate) fn spherical_side_data_projection(projection: Option<&str>) -> Option<Projection> {
    match projection {
        Some("equirectangular") | Some("half equirectangular") | None => Some(Projection::Equirect),
        _ => None,
    }
}

/// Signal 2: an Insta360 capture (`.insv` / `.insp`) whose frame is exactly two
/// squares side by side — the raw dual-fisheye packing (a 5.7K capture probes as
/// 5760x2880, i.e. two 2880x2880 circular hemispheres). Shared with the libav
/// probe backend so both agree on the geometry rule.
pub(crate) fn projection_from_shape(path: Option<&Path>, width: Option<u32>, height: Option<u32>) -> Option<Projection> {
    let insta360 = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("insv") || e.eq_ignore_ascii_case("insp"));
    match (insta360, width, height) {
        (true, Some(w), Some(h)) if h > 0 && h.checked_mul(2) == Some(w) => Some(Projection::DualFisheye),
        _ => None,
    }
}

/// A stream's tick as ffprobe prints it (`"1/1000"`): both terms whole and above zero, else not known.
pub(super) fn parse_time_base(s: &str) -> Option<(u32, u32)> {
    let (num, den) = s.split_once('/')?;
    time_base_of(num.trim().parse().ok()?, den.trim().parse().ok()?)
}

/// A stream's tick from the two terms a probe read (ffprobe's text, libav's `Rational`): both whole, above zero and in
/// range, else not known. The ffprobe and the libav probes share it.
pub(crate) fn time_base_of(num: i64, den: i64) -> Option<(u32, u32)> {
    let (num, den) = (u32::try_from(num).ok()?, u32::try_from(den).ok()?);
    (num > 0 && den > 0).then_some((num, den))
}

/// Parse an FFmpeg rational like `"30000/1001"` into an `f64`.
pub(super) fn parse_rational(s: &str) -> Option<f64> {
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.trim().parse().ok()?;
    let den: f64 = den.trim().parse().ok()?;
    if den == 0.0 {
        None
    } else {
        Some(num / den)
    }
}
