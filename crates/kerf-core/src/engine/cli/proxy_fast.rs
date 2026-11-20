//! The opt-in **Fast (GPU)** preview proxy: decode, scale and encode on the graphics hardware with no
//! frame downloaded in between (`-hwaccel X -hwaccel_output_format X`, the vendor's own scaler, the
//! vendor's H.264 encoder). Off by default (`proxy::ProxyEncoder`); with it off nothing here runs and
//! every proxy argv is what it always was.
//!
//! **Every pipeline below is unconfirmed on real hardware** (the machine it was written on has no GPU
//! encoder). What makes that harmless is the shape around it, not the pipelines being right:
//!
//! * a **probe** per vendor runs the *whole* pipeline (decode, GPU scale, encode) on a tiny generated
//!   clip and checks the result with the guard the real builds use; only a vendor that passes is
//!   offered, and only for the kinds of source (H.264, HEVC 8-bit, HEVC 10-bit) it passed on;
//! * a source it cannot vouch for ([`fast_eligible`]) is built by x264, as it always was;
//! * every build is **checked** ([`check_fast_output`]: frame count, first and last timestamp, size,
//!   colour tags against the source's stream) and a proxy that is not what the software encode would
//!   have given is deleted and rebuilt by x264 ([`fast_then_software`]);
//! * any failure or stall of a fast build is rebuilt by x264 too, and a failure that reads as the
//!   hardware's turns the pipeline off for the process on its **own** flag ([`FAST_PROXY_HEALTH`]:
//!   the stitch's `HW_ENCODE_OK` and the export's state are never touched).

use super::*;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// An answer that is a reason when it is no (the crate's `Result` is the engine's error).
type Verdict<T> = std::result::Result<T, String>;

// ---- the setting ----------------------------------------------------------------------------------------

static FAST_WANTED: AtomicBool = AtomicBool::new(false);

/// Whether the user asked for the Fast (GPU) pipeline (`proxy::set_proxy_encoder` mirrors its setting here).
pub fn proxy_fast_gpu() -> bool {
    #[cfg(test)]
    if let Some(wanted) = WANTED_OVERRIDE.with(std::cell::Cell::get) {
        return wanted;
    }
    FAST_WANTED.load(Ordering::Relaxed)
}

/// Mirror the setting. Turning it **on** re-arms a pipeline convicted this session (the user is
/// saying try again) and starts the capability check in the background.
pub fn set_proxy_fast_gpu(on: bool) {
    let was = FAST_WANTED.swap(on, Ordering::Relaxed);
    if on && !was {
        FAST_PROXY_HEALTH.reset();
        start_fast_probe();
    }
}

// ---- vendors ------------------------------------------------------------------------------------------

/// A vendor's whole hardware pipeline for a proxy. Probed in this order (the first that passes is the one used).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FastVendor {
    /// `-hwaccel cuda`, `scale_cuda`, `h264_nvenc`.
    Nvenc,
    /// `-hwaccel amf`, `vpp_amf`, `h264_amf` (the native AMF pipeline of AMD's wiki; Windows, and Linux per AMD).
    Amf,
    /// `-hwaccel qsv`, `vpp_qsv`, `h264_qsv`.
    Qsv,
    /// `-hwaccel vaapi`, `scale_vaapi`, `h264_vaapi` (Linux AMD and Intel), on the render node the encoder probe verified.
    Vaapi,
}

impl FastVendor {
    pub const ALL: [FastVendor; 4] = [FastVendor::Nvenc, FastVendor::Amf, FastVendor::Qsv, FastVendor::Vaapi];

    /// What the settings page and the logs call it.
    pub fn name(self) -> &'static str {
        match self {
            FastVendor::Nvenc => "NVIDIA NVENC",
            FastVendor::Amf => "AMD AMF",
            FastVendor::Qsv => "Intel Quick Sync",
            FastVendor::Vaapi => "VAAPI",
        }
    }

    /// The H.264 encoder (what a proxy's sidecar records).
    pub fn encoder(self) -> &'static str {
        match self {
            FastVendor::Nvenc => "h264_nvenc",
            FastVendor::Amf => "h264_amf",
            FastVendor::Qsv => "h264_qsv",
            FastVendor::Vaapi => "h264_vaapi",
        }
    }

    /// The `-hwaccel` (and the `-hwaccel_output_format` that keeps the frames on the device).
    fn hwaccel(self) -> &'static str {
        match self {
            FastVendor::Nvenc => "cuda",
            FastVendor::Amf => "amf",
            FastVendor::Qsv => "qsv",
            FastVendor::Vaapi => "vaapi",
        }
    }

    /// The scaling filter this ffmpeg has to have.
    fn scaler(self) -> &'static str {
        match self {
            FastVendor::Nvenc => "scale_cuda",
            FastVendor::Amf => "vpp_amf",
            FastVendor::Qsv => "vpp_qsv",
            FastVendor::Vaapi => "scale_vaapi",
        }
    }
}

/// Which encode a proxy build is running, told to the queue as each attempt starts holding its
/// slot (so a job that fell back to software says so and is timed as software).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyPipeline {
    /// x264 on the cores, with hardware decode where there is one.
    Software,
    /// The vendor's whole pipeline on the hardware's slot.
    Gpu(FastVendor),
}

impl ProxyPipeline {
    /// The encoder that writes the proxy.
    pub fn encoder(self) -> &'static str {
        match self {
            ProxyPipeline::Software => "libx264",
            ProxyPipeline::Gpu(vendor) => vendor.encoder(),
        }
    }
}

// ---- the kinds of source the pipeline is tried on ----------------------------------------------------------

/// What a source is, as far as a hardware decoder is concerned. The probe clip of each kind is run
/// through the vendor's pipeline, and a source of a kind the probe did not pass is not offered it: a
/// pipeline that decodes 8-bit H.264 says nothing about 10-bit HEVC (a conversion the scaler may lack).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum SourceClass {
    H264,
    Hevc8,
    Hevc10,
}

impl SourceClass {
    const ALL: [SourceClass; 3] = [SourceClass::H264, SourceClass::Hevc8, SourceClass::Hevc10];

    fn name(self) -> &'static str {
        match self {
            SourceClass::H264 => "H.264",
            SourceClass::Hevc8 => "HEVC 8-bit",
            SourceClass::Hevc10 => "HEVC 10-bit",
        }
    }

    /// The class of a decoder-vouched source (`InputFacts::hardware_decodes` already said yes).
    fn of(input: InputFacts) -> Option<Self> {
        match (input.codec, input.bits) {
            (Codec::H264, Some(8)) => Some(SourceClass::H264),
            (Codec::Hevc, Some(8)) => Some(SourceClass::Hevc8),
            (Codec::Hevc, Some(10)) => Some(SourceClass::Hevc10),
            _ => None,
        }
    }
}

// ---- the plan and its argv ------------------------------------------------------------------------------------

/// Everything a fast build's argv is made of besides the two paths: pure data, so the builder is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FastPlan {
    pub vendor: FastVendor,
    /// The render node, VAAPI only (the one the encoder probe verified).
    pub device: Option<String>,
    /// The proxy's size, explicit and even (the GPU scalers do not share `scale`'s expressions).
    pub width: u32,
    pub height: u32,
    /// How this ffmpeg spells `-fps_mode`.
    pub fps_flag: &'static str,
}

/// The constant quality a hardware proxy is encoded at: the x264 proxy's CRF number, taken 1:1 (which
/// at constant QP spends more bits than x264 would; larger files are what the speed costs).
const FAST_QUALITY: u32 = 24;

/// The vendor's rate control and speed flags for a proxy: the stitch's constant-quality spelling
/// (`quality_args`) with the encoder's fastest sensible preset. VAAPI has no stitch spelling.
fn fast_quality_args(vendor: FastVendor) -> Vec<String> {
    let s = |v: &str| v.to_string();
    match vendor {
        FastVendor::Amf => {
            let mut args = quality_args(vendor.encoder(), FAST_QUALITY);
            args.extend([s("-quality"), s("speed")]);
            args
        }
        FastVendor::Nvenc => {
            let mut args = quality_args(vendor.encoder(), FAST_QUALITY);
            args.extend([s("-preset"), s("p2")]);
            args
        }
        FastVendor::Qsv => {
            let mut args = quality_args(vendor.encoder(), FAST_QUALITY);
            args.extend([s("-preset"), s("veryfast")]);
            args
        }
        FastVendor::Vaapi => vec![s("-rc_mode"), s("CQP"), s("-global_quality"), FAST_QUALITY.to_string()],
    }
}

/// The scaling filter of `plan`, to explicit `w` x `h` and the encoder's own `nv12`.
fn fast_filter(plan: &FastPlan) -> String {
    let (w, h) = (plan.width, plan.height);
    match plan.vendor {
        FastVendor::Amf => format!("vpp_amf=w={w}:h={h}:format=nv12:scale_type=bilinear"),
        FastVendor::Nvenc => format!("scale_cuda=w={w}:h={h}:interp_algo=bilinear:format=nv12"),
        FastVendor::Qsv => format!("vpp_qsv=w={w}:h={h}:format=nv12"),
        FastVendor::Vaapi => format!("scale_vaapi=w={w}:h={h}:format=nv12"),
    }
}

/// The ffmpeg argument list (pure, unit-tested) of a **fast** proxy of `src` at `dst`: the frames
/// stay on the device from the decoder to the encoder. Everything a proxy promises holds as it does
/// for x264: all-intra (`-g 1`) with no B-frames, audio dropped, no `-r`, no `-ss`, no trim,
/// timestamps untouched (`-fps_mode vfr`, which FFmpeg 7 and newer make of an mp4 anyway and older
/// make `cfr`), the muxer named for the `.part` temp file. No `-threads` and no `-progress`: those are
/// written at spawn time like every other cap. No tone-map, no head pad, no autorotate: a source that
/// needs one is not given this pipeline ([`fast_eligible`]).
pub(super) fn fast_proxy_args(plan: &FastPlan, src: &str, dst: &str) -> Vec<String> {
    let s = |v: &str| v.to_string();
    let hw = plan.vendor.hwaccel();
    let mut args: Vec<String> = vec![s("-hide_banner"), s("-loglevel"), s("error"), s("-y")];
    args.extend([s("-hwaccel"), s(hw), s("-hwaccel_output_format"), s(hw)]);
    if let (FastVendor::Vaapi, Some(device)) = (plan.vendor, &plan.device) {
        args.extend([s("-hwaccel_device"), device.clone()]);
    }
    args.extend([s("-i"), s(src), s("-an"), s("-vf"), fast_filter(plan)]);
    args.extend([s("-c:v"), s(plan.vendor.encoder())]);
    args.extend(fast_quality_args(plan.vendor));
    args.extend([s("-g"), s("1"), s("-bf"), s("0")]);
    if plan.vendor == FastVendor::Amf {
        args.extend([s("-max_b_frames"), s("0")]);
    }
    args.extend([s(plan.fps_flag), s("vfr"), s("-f"), s("mp4"), s(dst)]);
    args
}

/// The size of the proxy of a `src_w` x `src_h` picture at `width` (pure, unit-tested) — what
/// `scale='min(W,iw)':-2` makes of it, spelled out: the width is `min(W, iw)` and the height keeps
/// the aspect ratio, rounded to the nearest **even** number (a half goes up). Checked against both
/// ffmpeg 4.4 and 9.0 over random sizes (`proxy_dims_equal_what_scale_makes`). `None` for a size
/// that is not a picture, one whose width stays odd (x264 refuses it too) and one the hardware
/// encoders' limits leave out ([`FAST_MIN_SIDE`] to [`FAST_MAX_SIDE`]).
pub(super) fn fast_dims(src_w: u32, src_h: u32, width: u32) -> Option<(u32, u32)> {
    if src_w == 0 || src_h == 0 || width == 0 {
        return None;
    }
    let w = width.min(src_w);
    if !w.is_multiple_of(2) {
        return None;
    }
    let h = 2 * ((u64::from(w) * u64::from(src_h) + u64::from(src_w)) / (2 * u64::from(src_w)));
    let h = u32::try_from(h).ok()?;
    ((FAST_MIN_SIDE..=FAST_MAX_SIDE).contains(&w) && (FAST_MIN_SIDE..=FAST_MAX_SIDE).contains(&h)).then_some((w, h))
}

/// The smallest side a hardware encoder is asked for (every family's minimum clears 256; the
/// encoder probe uses the same).
pub(super) const FAST_MIN_SIDE: u32 = 256;

/// The largest side: H.264 hardware encoders stop at 4096 across.
pub(super) const FAST_MAX_SIDE: u32 = 4096;

// ---- what a source is, and whether it may have this pipeline ----------------------------------------------------

/// Everything [`fast_eligible`] judges a source by: the cached per-file probe (`SourceTraits`) and how
/// the stream is stored (`StoredVideo`, one more `ffprobe` read of the same JSON the import reads).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FastFacts {
    pub traits: SourceTraits,
    pub stored: StoredVideo,
}

/// Why a source is not given the fast pipeline, or the kind of source it is (pure, unit-tested). Every
/// doubt is a refusal: the proxy is then what it always was. Refused:
///
/// * a probe that did not describe the file, and a codec, chroma format or depth no hardware decoder
///   is known to have (`InputFacts::hardware_decodes`: H.264 4:2:0 at 8 bit, HEVC 4:2:0 at 8 / 10);
/// * **HDR** (the tone-map chain runs on the cores) and a **full-range** picture (the scalers' range
///   handling is unverified);
/// * a video that **starts late** (the head pad is `split` / `trim` / `interleave`) and a picture
///   **rotated** by the display matrix (autorotate inserts a software transpose, and a proxy is
///   written upright);
/// * **interlaced** fields, **non-square** pixels, a file with **more than one picture stream**;
/// * a container that is not MP4 / MOV / Matroska / WebM (**MPEG-TS** and the rest have timestamps
///   that are measured, not stored) and one whose index does not say how many frames it holds and for
///   how long: the guard compares the proxy with exactly that (so Matroska, which does not, is x264's);
/// * a proxy size the encoders' limits leave out ([`fast_dims`]).
pub(super) fn fast_eligible(facts: &FastFacts, width: u32) -> Verdict<(SourceClass, (u32, u32))> {
    let (traits, stored) = (&facts.traits, &facts.stored);
    let no = |why: &str| Err(why.to_string());
    if !traits.described {
        return no("the source could not be probed");
    }
    let input = InputFacts {
        codec: traits.codec,
        chroma: traits.chroma,
        bits: traits.bits,
        ..InputFacts::default()
    };
    if !input.hardware_decodes() {
        return no("no hardware decoder is known to have this codec, chroma format and bit depth");
    }
    let Some(class) = SourceClass::of(input) else {
        return no("the bit depth is not one the pipeline was checked on");
    };
    if traits.hdr.is_some() {
        return no("HDR needs the tone-map chain, which runs on the cores");
    }
    if needs_head_pad(traits.lead) {
        return no("the video starts after the container (the head pad is a software filter chain)");
    }
    if stored.rotation != 0 {
        return no("the picture is rotated for display (autorotate is a software transpose)");
    }
    if stored.video_streams != 1 {
        return no("the file does not have exactly one picture stream");
    }
    if let Some(order) = stored.field_order.as_deref() {
        if !matches!(order, "progressive" | "unknown") {
            return no("the picture is interlaced");
        }
    }
    if let Some(aspect) = stored.sample_aspect_ratio.as_deref() {
        if !matches!(aspect, "1:1" | "0:1" | "N/A") {
            return no("the pixels are not square");
        }
    }
    if !traits.indexed {
        return no(
            "the container is not MP4, MOV, Matroska or WebM (MPEG-TS and the rest have no stored timestamps to check against)",
        );
    }
    if traits.full_range || stored.color_range.as_deref() == Some("pc") {
        return no("the picture is full range (the scalers' range handling is unverified)");
    }
    if stored.nb_frames.is_none_or(|n| n < 2) || stored.duration.is_none() {
        return no("the container does not say how many frames the video has and how long it runs");
    }
    match fast_dims(stored.width, stored.height, width) {
        Some(dims) => Ok((class, dims)),
        None => no("the proxy's size is not one the hardware encoders take"),
    }
}

// ---- the guard --------------------------------------------------------------------------------------------------------

/// A stream's colour tags as the guard compares them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ColorTags {
    pub space: Option<String>,
    pub primaries: Option<String>,
    pub transfer: Option<String>,
    pub range: Option<String>,
}

impl ColorTags {
    fn of_stored(stored: &StoredVideo) -> Self {
        Self {
            space: stored.color_space.clone(),
            primaries: stored.color_primaries.clone(),
            transfer: stored.color_transfer.clone(),
            range: stored.color_range.clone(),
        }
    }

    /// Whether `self` (the source's) and `proxy` (what the encoder wrote) say the same: the matrix,
    /// primaries and transfer exactly (an encoder that drops a tag the source had would make a
    /// renderer read the proxy in another matrix than the original), and a range that is either
    /// limited or unsaid on both sides (an encoder writes `tv` where x264 writes nothing; neither is
    /// full range, which an eligible source never is).
    fn matches(&self, proxy: &ColorTags) -> bool {
        let limited = |r: &Option<String>| matches!(r.as_deref(), None | Some("tv"));
        self.space == proxy.space
            && self.primaries == proxy.primaries
            && self.transfer == proxy.transfer
            && limited(&self.range)
            && limited(&proxy.range)
    }
}

/// What the software encode would have given, from the source's own stream.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FastExpect {
    /// The frames the container's index holds.
    pub frames: u64,
    /// The stream's length in seconds.
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub color: ColorTags,
}

impl FastExpect {
    pub(super) fn of(stored: &StoredVideo, (width, height): (u32, u32)) -> Option<Self> {
        Some(Self {
            frames: stored.nb_frames?,
            duration: stored.duration?,
            width,
            height,
            color: ColorTags::of_stored(stored),
        })
    }
}

/// What a finished proxy says of itself: one `ffprobe` of its stream and the timestamps of its packets.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ProxyRead {
    /// Every packet's presentation time, in file order.
    pub pts: Vec<f64>,
    pub width: u32,
    pub height: u32,
    pub color: ColorTags,
}

/// Whether the proxy that was read is the proxy the software encode would have made (pure, unit-tested):
/// its **size** is the plan's; it holds **exactly the source's frame count**; its first timestamp is
/// the start of the file (a source whose video starts with its container is rebased to zero by the
/// software encode, and refused here if it starts late); its timestamps **strictly increase**; its
/// last frame and one frame more land at the source's length (within two mean frame intervals: a
/// variable-frame-rate clip's last interval is not its mean); and its **colour tags** are the
/// source's. An `Err` is the reason, for the log.
pub(super) fn check_fast_output(expect: &FastExpect, read: &ProxyRead) -> Verdict<()> {
    if (read.width, read.height) != (expect.width, expect.height) {
        return Err(format!(
            "the proxy is {}x{}, not the {}x{} that was asked for",
            read.width, read.height, expect.width, expect.height
        ));
    }
    let frames = read.pts.len() as u64;
    if frames != expect.frames {
        return Err(format!("the proxy has {frames} frames and the source has {}", expect.frames));
    }
    if frames < 2 {
        return Err("a proxy of one frame cannot be checked".to_string());
    }
    let (first, last) = (read.pts[0], read.pts[read.pts.len() - 1]);
    if first.abs() > HEAD_PAD_MIN {
        return Err(format!("the proxy starts at {first:.4} s, not at the start of the file"));
    }
    if read
        .pts
        .windows(2)
        .any(|w| w[1].partial_cmp(&w[0]) != Some(std::cmp::Ordering::Greater))
    {
        return Err("the proxy's timestamps do not strictly increase".to_string());
    }
    let step = (last - first) / (frames - 1) as f64;
    let end = last + step;
    if (end - expect.duration).abs() > 2.0 * step + 0.002 {
        return Err(format!(
            "the proxy runs {end:.3} s and the source's video {:.3} s",
            expect.duration
        ));
    }
    if !expect.color.matches(&read.color) {
        return Err(format!(
            "the proxy's colour tags {:?} are not the source's {:?}",
            read.color, expect.color
        ));
    }
    Ok(())
}

/// [`ProxyRead`] from the JSON of `ffprobe -select_streams v:0 -show_entries packet=pts_time:stream=...`
/// (pure, unit-tested). `None` for anything that is not a picture stream with a time for every packet.
pub(super) fn parse_proxy_read(json: &str) -> Option<ProxyRead> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let stream = v.get("streams")?.get(0)?;
    let pts: Vec<f64> = v
        .get("packets")?
        .as_array()?
        .iter()
        .map(|p| {
            p.get("pts_time")?
                .as_str()?
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|t| t.is_finite())
        })
        .collect::<Option<_>>()?;
    if pts.is_empty() {
        return None;
    }
    let tag = |key: &str| known_color(stream.get(key).and_then(|t| t.as_str()));
    Some(ProxyRead {
        pts,
        width: u32::try_from(stream.get("width")?.as_u64()?).ok()?,
        height: u32::try_from(stream.get("height")?.as_u64()?).ok()?,
        color: ColorTags {
            space: tag("color_space"),
            primaries: tag("color_primaries"),
            transfer: tag("color_transfer"),
            range: range_tag(stream.get("color_range").and_then(|t| t.as_str())),
        },
    })
}

/// How long a read of a finished proxy (headers and the packet list) may take.
const FAST_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Read `proxy` for the guard: one bounded `ffprobe`.
pub(super) fn read_fast_output(proxy: &Path) -> Verdict<ProxyRead> {
    let mut cmd = command(&ffprobe_bin());
    cmd.args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("packet=pts_time:stream=width,height,color_space,color_primaries,color_transfer,color_range")
        .args(["-of", "json"])
        .arg(proxy);
    let output = output_bounded(&mut cmd, FAST_READ_TIMEOUT, 1 << 16).map_err(|e| format!("ffprobe of the proxy failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe of the proxy failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_proxy_read(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| "the proxy could not be read back".to_string())
}

/// How a source is stored, from one bounded `ffprobe` (the import's own JSON). `None` when the probe
/// fails or finds no picture.
pub(super) fn read_stored_video(src: &Path) -> Option<StoredVideo> {
    let mut cmd = command(&ffprobe_bin());
    cmd.args(probe_args(src, crate::paths::demuxer_whitelist()));
    let output = run_piped_until(&mut cmd, Vec::new(), Instant::now() + SOURCE_PROBE_TIMEOUT)?;
    let parsed: probe::ProbeJson = serde_json::from_slice(&output).ok()?;
    probe::stored_video(&parsed)
}

// ---- when the pipeline stops being offered ------------------------------------------------------------------------------

/// How a fast build that did not give a proxy failed, which decides what it costs the pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FastFailure {
    /// ffmpeg said nothing for [`FAST_STALL`]: a wedged driver.
    Stalled,
    /// An error that reads as the hardware's (no device, no session, a format the device has no path for).
    Hardware(String),
    /// Any other failure, and a proxy the guard refused (`check_fast_output`).
    Other(String),
}

impl FastFailure {
    fn text(&self) -> String {
        match self {
            FastFailure::Stalled => format!("ffmpeg stopped reporting progress for {}s", FAST_STALL.as_secs()),
            FastFailure::Hardware(t) | FastFailure::Other(t) => t.clone(),
        }
    }
}

/// How long a fast build may go without a word from ffmpeg before it is killed and built in software:
/// far shorter than the software encode's [`EXPORT_STALL`], since a hardware pipeline that has not
/// moved in a minute is a driver that has hung, and nothing else holds the slot for it.
pub(super) const FAST_STALL: std::time::Duration = std::time::Duration::from_secs(60);

/// How many failures that do not read as the hardware's, and proxies the guard refused, the pipeline
/// is allowed before it is turned off for the process (a successful fast build starts the count again).
const STRIKES_TO_CONVICT: u32 = 3;

/// Whether the text of a failed fast run says the hardware could not do it, rather than the file being
/// bad (pure, unit-tested): `looks_like_hardware_decode_failure`'s phrases and the encoder's, the
/// filters' and the device libraries'.
pub(super) fn looks_like_hardware_pipeline_failure(text: &str) -> bool {
    const MARKS: [&str; 15] = [
        "error while opening encoder",
        "failed to create  hardware device context",
        "failed to create hardware device context",
        "device creation failed",
        "cannot load libcuda",
        "cannot load libnvcuvid",
        "openencodesessionex failed",
        "no capable devices found",
        "error creating a mfx session",
        "dll libamfrt64",
        "cannot open drm render node",
        "cannot open a va display",
        "failed to initialise va",
        "impossible to convert between the formats supported by the filter",
        "no hw context provided",
    ];
    let lower = text.to_ascii_lowercase();
    looks_like_hardware_decode_failure(text) || MARKS.iter().any(|m| lower.contains(m))
}

/// [`FastFailure`] of the stderr of a fast run that exited without success.
pub(super) fn classify_fast_failure(stderr: &str) -> FastFailure {
    let text = stderr.trim().to_string();
    if looks_like_hardware_pipeline_failure(&text) {
        FastFailure::Hardware(text)
    } else {
        FastFailure::Other(text)
    }
}

/// Whether the fast pipeline is still offered this session, and why not. Its own flag: nothing here
/// touches [`HW_ENCODE_OK`] (the Insta360 stitch's) or the export's hardware state, and a proxy that
/// cannot be built fast is built by x264, so nothing here costs a proxy.
pub(super) struct FastHealth {
    convicted: AtomicBool,
    strikes: AtomicU32,
    why: Mutex<Option<String>>,
}

impl FastHealth {
    pub(super) const fn new() -> Self {
        Self {
            convicted: AtomicBool::new(false),
            strikes: AtomicU32::new(0),
            why: Mutex::new(None),
        }
    }

    /// `Err(why)` once convicted.
    pub(super) fn usable(&self) -> Verdict<()> {
        if !self.convicted.load(Ordering::Relaxed) {
            return Ok(());
        }
        let why = self.why.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Err(format!(
            "Fast (GPU) stopped working this session{}",
            why.map(|w| format!(": {w}")).unwrap_or_default()
        ))
    }

    /// A fast build that the guard passed.
    pub(super) fn accepted(&self) {
        self.strikes.store(0, Ordering::Relaxed);
    }

    /// A fast build failed and the software rebuild of the same proxy **succeeded**, so the failure
    /// was the pipeline's and not the file's: a hardware-looking one or a stall turns it off, the
    /// others count against it.
    pub(super) fn record(&self, failure: &FastFailure) {
        let convict = match failure {
            FastFailure::Stalled | FastFailure::Hardware(_) => true,
            FastFailure::Other(_) => self.strikes.fetch_add(1, Ordering::Relaxed) + 1 >= STRIKES_TO_CONVICT,
        };
        if convict && !self.convicted.swap(true, Ordering::Relaxed) {
            let text = failure.text();
            let first = text.lines().next().unwrap_or("").trim().to_string();
            *self.why.lock().unwrap_or_else(|e| e.into_inner()) = Some(first.clone());
            tracing::warn!(reason = %first, "the fast GPU proxy pipeline failed where x264 worked; proxies are built in software from now on this session");
        }
    }

    /// Offer the pipeline again (the user turned the setting off and on).
    pub(super) fn reset(&self) {
        self.convicted.store(false, Ordering::Relaxed);
        self.strikes.store(0, Ordering::Relaxed);
        *self.why.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// The process's: the fast proxy pipeline's own conviction (see [`FastHealth`]).
pub(super) static FAST_PROXY_HEALTH: FastHealth = FastHealth::new();

/// The conviction every read and write of a build goes through: [`FAST_PROXY_HEALTH`], or, while a test
/// has pinned a world of its own on this thread ([`with_fast_world`]), that world's.
pub(super) fn fast_health() -> &'static FastHealth {
    #[cfg(test)]
    if let Some(own) = HEALTH_OVERRIDE.with(std::cell::Cell::get) {
        return own;
    }
    &FAST_PROXY_HEALTH
}

#[cfg(test)]
thread_local! {
    /// Whether the setting is on, on this thread, while a test pins it (the process's stays as it is).
    static WANTED_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    /// The conviction this thread's builds use while a test pins a world of its own.
    static HEALTH_OVERRIDE: std::cell::Cell<Option<&'static FastHealth>> = const { std::cell::Cell::new(None) };
}

/// Run `f` in a world of its own on this thread: the setting `wanted`, and a fresh conviction nobody
/// else reads or writes. A test that drives a proxy build through the Fast (GPU) pipeline needs the
/// setting on and the conviction clean, and the process's are shared by every test that builds a proxy
/// and by every queue test: pinning them per thread (as the capability check and `ffmpeg` are pinned)
/// means this test neither waits for another nor leaves the setting on for one running beside it.
#[cfg(test)]
pub(crate) fn with_fast_world<R>(wanted: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>, Option<&'static FastHealth>);
    impl Drop for Reset {
        fn drop(&mut self) {
            WANTED_OVERRIDE.with(|c| c.set(self.0));
            HEALTH_OVERRIDE.with(|c| c.set(self.1));
        }
    }
    let own: &'static FastHealth = Box::leak(Box::new(FastHealth::new()));
    let _reset = Reset(
        WANTED_OVERRIDE.with(|c| c.replace(Some(wanted))),
        HEALTH_OVERRIDE.with(|c| c.replace(Some(own))),
    );
    f()
}

/// Held by every test that moves the process-wide setting or conviction of the fast pipeline: they
/// are read by every proxy build, which cargo runs on threads of one process.
#[cfg(test)]
pub(crate) fn fast_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// How a fast build ended.
pub(super) enum FastEnd<T> {
    /// The proxy is in place.
    Done(T),
    /// Cancelled: nothing is rebuilt.
    Cancelled,
    /// No proxy came of it; build it in software.
    Failed(FastFailure),
}

/// One proxy build, fast where it can be (pure of the machine: the two builds are closures, so a
/// failing runner is a test): `fast` is run first; a proxy it made is the result, a cancel is a
/// cancel, and **any failure** runs `software`, whose success then counts the failure against the
/// pipeline (`health`) and whose failure counts it for nothing (the source is bad, not the GPU).
pub(super) fn fast_then_software<T>(
    health: &FastHealth,
    fast: impl FnOnce() -> FastEnd<T>,
    software: impl FnOnce() -> Result<T>,
) -> Result<T> {
    match fast() {
        FastEnd::Done(done) => {
            health.accepted();
            Ok(done)
        }
        FastEnd::Cancelled => Err(Error::Cancelled),
        FastEnd::Failed(failure) => {
            tracing::warn!(error = %failure.text(), "fast GPU proxy build failed; building it in software");
            let built = software()?;
            health.record(&failure);
            Ok(built)
        }
    }
}

// ---- the capability check ---------------------------------------------------------------------------------------------------

/// What the one-per-process check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FastPipeline {
    pub vendor: FastVendor,
    pub device: Option<String>,
    /// The kinds of source the whole pipeline was run on and passed.
    pub covers: Vec<SourceClass>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FastProbe {
    pub pipeline: Option<FastPipeline>,
    /// Why there is none (empty when there is one).
    pub reason: String,
}

static FAST_PROBE: OnceLock<FastProbe> = OnceLock::new();

#[cfg(test)]
thread_local! {
    /// What the capability check says on this thread while a test pins it.
    static PROBE_OVERRIDE: std::cell::Cell<Option<&'static FastProbe>> = const { std::cell::Cell::new(None) };
}

/// Run `f` as if the capability check had found `probe` (a test's: the check itself needs a GPU).
#[cfg(test)]
pub(super) fn with_fast_probe<R>(probe: FastProbe, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<&'static FastProbe>);
    impl Drop for Reset {
        fn drop(&mut self) {
            PROBE_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let leaked: &'static FastProbe = Box::leak(Box::new(probe));
    let _reset = Reset(PROBE_OVERRIDE.with(|c| c.replace(Some(leaked))));
    f()
}

/// Run `f` as if the capability check had found `vendor`'s pipeline (or nothing, with `None`) and it covered
/// every kind of source: for the queue's tests, which need a machine with a GPU and have none.
#[cfg(test)]
pub(crate) fn with_pinned_fast_check<R>(found: Option<FastVendor>, f: impl FnOnce() -> R) -> R {
    let probe = FastProbe {
        pipeline: found.map(|vendor| FastPipeline {
            vendor,
            device: None,
            covers: SourceClass::ALL.to_vec(),
        }),
        reason: "pinned: nothing passed".to_string(),
    };
    with_fast_probe(probe, f)
}

/// Whether the setting is on in this process, for tests of the setting's plumbing.
#[cfg(test)]
pub(crate) fn proxy_fast_gpu_is_on() -> bool {
    proxy_fast_gpu()
}

/// Whether a unit test lets the real check run (it runs ffmpeg and tests the hardware): off, so that
/// a test that turns the setting on does not start probes in the background of every other test.
#[cfg(test)]
static REAL_PROBE_ALLOWED: AtomicBool = AtomicBool::new(false);

/// Let the real capability check run in this test process (the `#[ignore]`d tests that drive ffmpeg).
#[cfg(test)]
pub(crate) fn allow_real_fast_probe() {
    REAL_PROBE_ALLOWED.store(true, Ordering::Relaxed);
}

/// Start the capability check on a thread of its own, once. It never blocks the caller.
pub(super) fn start_fast_probe() {
    #[cfg(test)]
    if !REAL_PROBE_ALLOWED.load(Ordering::Relaxed) {
        return;
    }
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("kerf-fast-proxy-probe".to_string())
            .spawn(|| {
                FAST_PROBE.get_or_init(detect_fast_pipeline);
            });
    });
}

/// How long a proxy build waits for the check when it has not finished: its own worst case is
/// minutes (a hung driver is killed after [`HW_PROBE_TIMEOUT`] per run), a build is not held for that.
const FAST_PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(45);

/// The check's answer, waiting up to `limit` for it (starting it when nothing has).
pub(super) fn fast_probe_within(limit: std::time::Duration) -> Option<&'static FastProbe> {
    fast_probe_until(limit, &|| false)
}

/// [`fast_probe_within`] that stops waiting when `cancel` turns true (polled four times a second): a
/// proxy that is deleted or rebuilt while it waits for the check is not held to its end.
pub(super) fn fast_probe_until(limit: std::time::Duration, cancel: &dyn Fn() -> bool) -> Option<&'static FastProbe> {
    #[cfg(test)]
    if let Some(pinned) = PROBE_OVERRIDE.with(std::cell::Cell::get) {
        return Some(pinned);
    }
    start_fast_probe();
    let deadline = Instant::now() + limit;
    loop {
        let slice = deadline
            .saturating_duration_since(Instant::now())
            .min(std::time::Duration::from_millis(250));
        if let Some(probe) = wait_for(&FAST_PROBE, slice) {
            return Some(probe);
        }
        if Instant::now() >= deadline || cancel() {
            return None;
        }
    }
}

/// What the settings page shows of the pipeline: cheap, never blocks, never spawns anything but the
/// one background check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastStatus {
    /// Whether a vendor passed and the pipeline is still offered.
    pub available: bool,
    /// Whether the check has finished (`false` until it has: `reason` then says "not checked yet").
    pub checked: bool,
    /// The vendor's name when `available`.
    pub pipeline: Option<&'static str>,
    pub reason: Option<String>,
}

/// [`FastStatus`] now.
pub fn fast_pipeline_status() -> FastStatus {
    let probe = fast_probe_within(std::time::Duration::ZERO);
    fast_status_of(probe, fast_health().usable())
}

/// The pure half of [`fast_pipeline_status`].
pub(super) fn fast_status_of(probe: Option<&FastProbe>, health: Verdict<()>) -> FastStatus {
    let Some(probe) = probe else {
        return FastStatus {
            available: false,
            checked: false,
            pipeline: None,
            reason: Some("not checked yet".to_string()),
        };
    };
    match (&probe.pipeline, health) {
        (Some(found), Ok(())) => FastStatus {
            available: true,
            checked: true,
            pipeline: Some(found.vendor.name()),
            reason: None,
        },
        (Some(found), Err(why)) => FastStatus {
            available: false,
            checked: true,
            pipeline: Some(found.vendor.name()),
            reason: Some(why),
        },
        (None, _) => FastStatus {
            available: false,
            checked: true,
            pipeline: None,
            reason: Some(probe.reason.clone()),
        },
    }
}

/// The test clips: a second of nothing in particular at 1280x720, 15 frames, one per class.
const PROBE_CLIP_FRAMES: u64 = 15;

/// The size the probe scales its clips to.
pub(super) const PROBE_TARGET: (u32, u32) = (640, 360);

/// Whether the `-hwaccels` and `-filters` listings of this ffmpeg name `vendor`'s pieces (pure,
/// unit-tested); the reason when not.
pub(super) fn vendor_in_listing(vendor: FastVendor, hwaccels: &str, filters: &str) -> Verdict<()> {
    let has_hwaccel = hwaccels.lines().any(|l| l.trim() == vendor.hwaccel());
    let has_filter = filters.lines().any(|l| l.split_whitespace().nth(1) == Some(vendor.scaler()));
    match (has_hwaccel, has_filter) {
        (true, true) => Ok(()),
        (false, _) => Err(format!("this ffmpeg has no `-hwaccel {}`", vendor.hwaccel())),
        (_, false) => Err(format!("this ffmpeg has no `{}` filter", vendor.scaler())),
    }
}

/// The one-line listing of `ffmpeg <flag>` (`-hwaccels`, `-filters`), empty when it cannot be had.
pub(super) fn listing(bin: &str, flag: &str) -> String {
    output_bounded(
        command(bin).args(["-hide_banner", "-loglevel", "quiet", flag]),
        CAPABILITY_PROBE_TIMEOUT,
        0,
    )
    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    .unwrap_or_default()
}

/// A generated probe clip, and what the software reading of it says.
pub(super) struct ProbeClip {
    pub class: SourceClass,
    pub path: PathBuf,
    pub stored: StoredVideo,
}

/// Make the probe clips in `dir` with the software encoders of this ffmpeg (an HEVC class a build
/// without `libx265` cannot make is simply not probed, and so not offered).
pub(super) fn make_probe_clips(bin: &str, dir: &Path) -> Vec<ProbeClip> {
    let mut clips = Vec::new();
    for class in SourceClass::ALL {
        let (codec, pix_fmt) = match class {
            SourceClass::H264 => ("libx264", "yuv420p"),
            SourceClass::Hevc8 => ("libx265", "yuv420p"),
            SourceClass::Hevc10 => ("libx265", "yuv420p10le"),
        };
        let path = dir.join(format!("{}.mp4", class.name().replace([' ', '.'], "-")));
        let mut cmd = command(bin);
        cmd.args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=s=1280x720:r=30:d={}", PROBE_CLIP_FRAMES as f64 / 30.0))
            .args(["-c:v", codec, "-pix_fmt", pix_fmt]);
        if codec == "libx265" {
            cmd.args(["-x265-params", "log-level=error"]);
        }
        cmd.args(["-f", "mp4"]).arg(&path).stdout(Stdio::null()).stderr(Stdio::null());
        if !matches!(status_within(&mut cmd, HW_PROBE_TIMEOUT), Ok(Some(s)) if s.success()) {
            continue;
        }
        if let Some(stored) = read_stored_video(&path) {
            clips.push(ProbeClip { class, path, stored });
        }
    }
    clips
}

/// What a failed run says went wrong, for a status line (pure, unit-tested): the first two lines of
/// its error output, which hold the cause (the last line of an ffmpeg failure is usually only "Error
/// splitting the argument list" or "Error opening output file").
pub(super) fn failure_reason(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).take(2).collect();
    if lines.is_empty() {
        "ffmpeg failed".to_string()
    } else {
        lines.join(" / ")
    }
}

/// Run `vendor`'s whole pipeline on one probe clip and check what came out with the guard the real
/// builds use. `Err` is the reason it did not work (an ffmpeg error's first line, a timeout, a guard's
/// refusal).
pub(super) fn probe_pipeline_on(bin: &str, plan: &FastPlan, clip: &ProbeClip, dir: &Path) -> Verdict<()> {
    let out = dir.join(format!("out-{}.mp4", plan.vendor.encoder()));
    let _ = std::fs::remove_file(&out);
    let (Some(src), Some(dst)) = (clip.path.to_str(), out.to_str()) else {
        return Err("the probe's temp path is not UTF-8".to_string());
    };
    let mut cmd = command(bin);
    cmd.args(fast_proxy_args(plan, src, dst));
    let result = match output_bounded(&mut cmd, HW_PROBE_TIMEOUT, 0) {
        Ok(output) if output.status.success() => {
            let expect = FastExpect::of(&clip.stored, (plan.width, plan.height)).ok_or("the probe clip has no frame count")?;
            read_fast_output(&out).and_then(|read| check_fast_output(&expect, &read))
        }
        Ok(output) => Err(failure_reason(&String::from_utf8_lossy(&output.stderr))),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            Err(format!("no answer in {} s (the driver hung)", HW_PROBE_TIMEOUT.as_secs()))
        }
        Err(e) => Err(format!("could not run ffmpeg: {e}")),
    };
    let _ = std::fs::remove_file(&out);
    result
}

/// `vendor`'s pipeline if it works here: its pieces are in this ffmpeg, its encoder passed the
/// encoder probe, and the whole pipeline passed on the H.264 probe clip (the HEVC clips only widen
/// what it covers).
fn check_vendor(bin: &str, vendor: FastVendor, listings: (&str, &str), clips: &[ProbeClip], dir: &Path) -> Verdict<FastPipeline> {
    vendor_in_listing(vendor, listings.0, listings.1)?;
    if !hw_encoders().iter().any(|e| e == vendor.encoder()) {
        return Err(format!(
            "{} did not pass the encoder test (no usable device)",
            vendor.encoder()
        ));
    }
    let device = match vendor {
        FastVendor::Vaapi => Some(
            hw_probe()
                .vaapi_device
                .clone()
                .ok_or("no VAAPI render node passed the encoder test")?,
        ),
        _ => None,
    };
    let plan = FastPlan {
        vendor,
        device: device.clone(),
        width: PROBE_TARGET.0,
        height: PROBE_TARGET.1,
        fps_flag: fps_mode_flag(),
    };
    let mut covers = Vec::new();
    for clip in clips {
        match probe_pipeline_on(bin, &plan, clip, dir) {
            Ok(()) => covers.push(clip.class),
            Err(why) if clip.class == SourceClass::H264 => return Err(why),
            Err(why) => {
                tracing::info!(vendor = vendor.name(), class = clip.class.name(), %why, "fast GPU proxy: not offered for this kind of source");
            }
        }
    }
    if !covers.contains(&SourceClass::H264) {
        return Err("the H.264 test clip could not be made".to_string());
    }
    Ok(FastPipeline { vendor, device, covers })
}

/// The check: the first vendor, in [`FastVendor::ALL`] order, whose whole pipeline passes.
fn detect_fast_pipeline() -> FastProbe {
    let none = |reason: &str| FastProbe {
        pipeline: None,
        reason: reason.to_string(),
    };
    if !hw_encode_enabled() {
        return none("hardware encoding is turned off (KERF_HW_ENCODE)");
    }
    if hwaccel().is_none() {
        return none("hardware decoding is turned off (KERF_HWACCEL)");
    }
    if hw_encoders().is_empty() {
        return none("no hardware video encoder passed its test on this machine");
    }
    let bin = ffmpeg_bin();
    let (hwaccels, filters) = (listing(&bin, "-hwaccels"), listing(&bin, "-filters"));
    let dir = std::env::temp_dir().join(format!("kerf-fast-proxy-probe-{}", std::process::id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return none("no temp directory to test in");
    }
    let clips = make_probe_clips(&bin, &dir);
    let mut why: Vec<String> = Vec::new();
    let mut found = None;
    // One vendor at a time: consumer NVENC counts sessions.
    for vendor in FastVendor::ALL {
        match check_vendor(&bin, vendor, (&hwaccels, &filters), &clips, &dir) {
            Ok(pipeline) => {
                found = Some(pipeline);
                break;
            }
            Err(reason) => why.push(format!("{}: {reason}", vendor.name())),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    let probe = match found {
        Some(pipeline) => FastProbe {
            pipeline: Some(pipeline),
            reason: String::new(),
        },
        None => FastProbe {
            pipeline: None,
            reason: why.join("; "),
        },
    };
    tracing::info!(
        vendor = ?probe.pipeline.as_ref().map(|p| p.vendor.name()),
        covers = ?probe.pipeline.as_ref().map(|p| p.covers.iter().map(|c| c.name()).collect::<Vec<_>>()),
        reason = %probe.reason,
        "fast GPU proxy pipeline checked"
    );
    probe
}

// ---- choosing a pipeline for one proxy -----------------------------------------------------------------------------------------

/// What [`choose_pipeline`] decided for one proxy.
pub(super) enum FastChoice {
    /// x264. The text is why the fast pipeline was not used when the user had asked for it (`None`
    /// when they had not: nothing to say).
    Software(Option<String>),
    Gpu(FastPlan, FastExpect),
}

/// The plan for a source the pipeline `found` may be given (pure, unit-tested): the source must be
/// eligible ([`fast_eligible`]) and of a kind the check ran the whole pipeline on. The reason when not.
pub(super) fn plan_for(
    facts: &FastFacts,
    width: u32,
    found: &FastPipeline,
    fps_flag: &'static str,
) -> Verdict<(FastPlan, FastExpect)> {
    let (class, dims) = fast_eligible(facts, width)?;
    if !found.covers.contains(&class) {
        return Err(format!(
            "{} did not pass the GPU check on {} sources",
            found.vendor.name(),
            class.name()
        ));
    }
    let expect = FastExpect::of(&facts.stored, dims).ok_or("the source has no frame count")?;
    let plan = FastPlan {
        vendor: found.vendor,
        device: found.device.clone(),
        width: dims.0,
        height: dims.1,
        fps_flag,
    };
    Ok((plan, expect))
}

/// Whether `src`'s proxy at `width` is built by the fast pipeline (decided before the build holds any
/// slot). With the setting off this reads one atomic and returns. Once the setting is on it waits for
/// the capability check (up to [`FAST_PROBE_WAIT`], and not past `cancel`) and, if a pipeline passed
/// and is still offered, spawns one `ffprobe` of the source (the same JSON the import reads);
/// nothing here waits on a lock.
///
/// What the **previews** learned about hardware decode (`decode_hwaccel()`: any rescue of a still, a
/// frame grab or a stream clears it, whatever failed, and one unit test leaves it cleared for the
/// process) is deliberately not asked: it is about `-hwaccel auto`, the pipeline names its own
/// accelerator (`cuda`, `amf`, `qsv`, `vaapi`), and the check ran that very accelerator on a clip.
/// `KERF_HWACCEL=none` and `KERF_HW_ENCODE=none` are honoured where the check reads them
/// (`detect_fast_pipeline`), once, so the decision here does not depend on the process's environment.
pub(super) fn choose_pipeline(src: &Path, width: u32, traits: SourceTraits, cancel: &dyn Fn() -> bool) -> FastChoice {
    if !proxy_fast_gpu() {
        return FastChoice::Software(None);
    }
    let software = |why: String| FastChoice::Software(Some(why));
    if let Err(why) = fast_health().usable() {
        return software(why);
    }
    let Some(probe) = fast_probe_until(FAST_PROBE_WAIT, cancel) else {
        return software("the GPU check has not finished".to_string());
    };
    let Some(found) = &probe.pipeline else {
        return software(probe.reason.clone());
    };
    let Some(stored) = read_stored_video(src) else {
        return software("the source's stream could not be read".to_string());
    };
    match plan_for(&FastFacts { traits, stored }, width, found, fps_mode_flag()) {
        Ok((plan, expect)) => FastChoice::Gpu(plan, expect),
        Err(why) => software(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(vendor: FastVendor) -> FastPlan {
        FastPlan {
            vendor,
            device: (vendor == FastVendor::Vaapi).then(|| "/dev/dri/renderD129".to_string()),
            width: 1280,
            height: 720,
            fps_flag: "-fps_mode",
        }
    }

    fn joined(vendor: FastVendor) -> String {
        fast_proxy_args(&plan(vendor), "/in.mov", "/out.mp4").join(" ")
    }

    // ---- the argv of each vendor ---------------------------------------------------------------------

    #[test]
    fn the_amf_pipeline_is_the_native_one_of_amds_wiki() {
        assert_eq!(
            joined(FastVendor::Amf),
            "-hide_banner -loglevel error -y -hwaccel amf -hwaccel_output_format amf -i /in.mov -an \
             -vf vpp_amf=w=1280:h=720:format=nv12:scale_type=bilinear -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 \
             -quality speed -g 1 -bf 0 -max_b_frames 0 -fps_mode vfr -f mp4 /out.mp4"
        );
    }

    #[test]
    fn the_nvenc_pipeline_is_cuda_decode_scale_cuda_and_nvenc() {
        assert_eq!(
            joined(FastVendor::Nvenc),
            "-hide_banner -loglevel error -y -hwaccel cuda -hwaccel_output_format cuda -i /in.mov -an \
             -vf scale_cuda=w=1280:h=720:interp_algo=bilinear:format=nv12 -c:v h264_nvenc -rc vbr -cq 24 -b:v 0 \
             -preset p2 -g 1 -bf 0 -fps_mode vfr -f mp4 /out.mp4"
        );
    }

    #[test]
    fn the_quick_sync_pipeline_is_qsv_decode_vpp_qsv_and_h264_qsv() {
        assert_eq!(
            joined(FastVendor::Qsv),
            "-hide_banner -loglevel error -y -hwaccel qsv -hwaccel_output_format qsv -i /in.mov -an \
             -vf vpp_qsv=w=1280:h=720:format=nv12 -c:v h264_qsv -global_quality 24 -preset veryfast \
             -g 1 -bf 0 -fps_mode vfr -f mp4 /out.mp4"
        );
    }

    #[test]
    fn the_vaapi_pipeline_names_the_probed_render_node_for_decode_and_nothing_else() {
        assert_eq!(
            joined(FastVendor::Vaapi),
            "-hide_banner -loglevel error -y -hwaccel vaapi -hwaccel_output_format vaapi \
             -hwaccel_device /dev/dri/renderD129 -i /in.mov -an -vf scale_vaapi=w=1280:h=720:format=nv12 \
             -c:v h264_vaapi -rc_mode CQP -global_quality 24 -g 1 -bf 0 -fps_mode vfr -f mp4 /out.mp4"
        );
        // No node probed: no device option (ffmpeg's own default), never an invented path.
        let mut bare = plan(FastVendor::Vaapi);
        bare.device = None;
        assert!(!fast_proxy_args(&bare, "/in", "/out").contains(&"-hwaccel_device".to_string()));
    }

    #[test]
    fn every_fast_proxy_keeps_what_a_proxy_promises() {
        for vendor in FastVendor::ALL {
            let args = fast_proxy_args(&plan(vendor), "/in.mov", "/out.mp4");
            let flag = |name: &str| args.iter().position(|a| a == name).map(|i| args[i + 1].as_str());
            // All-intra, no B-frames, audio dropped, the muxer named, the proxy last.
            assert_eq!(flag("-g"), Some("1"), "{vendor:?}");
            assert_eq!(flag("-bf"), Some("0"), "{vendor:?}");
            assert!(args.contains(&"-an".to_string()), "{vendor:?}");
            assert_eq!(flag("-f"), Some("mp4"), "{vendor:?}");
            assert_eq!(args.last().map(String::as_str), Some("/out.mp4"));
            // A source time maps 1:1 onto the proxy: no retime, trim or seek, timestamps kept.
            for banned in ["-r", "-t", "-ss", "-vsync", "-itsoffset", "-copyts"] {
                assert!(!args.contains(&banned.to_string()), "{vendor:?} has {banned}");
            }
            assert_eq!(flag("-fps_mode"), Some("vfr"), "{vendor:?}");
            // Thread caps and progress are written at spawn, never here; no pixel format of its own
            // (the frames are device frames the scaler already made nv12).
            for banned in ["-threads", "-progress", "-pix_fmt", "-filter_complex"] {
                assert!(!args.contains(&banned.to_string()), "{vendor:?} has {banned}");
            }
            // The decode is an input option: before the `-i`.
            let hw = args.iter().position(|a| a == "-hwaccel").unwrap();
            let input = args.iter().position(|a| a == "-i").unwrap();
            assert!(hw < input, "{vendor:?}");
            assert_eq!(
                flag("-hwaccel"),
                flag("-hwaccel_output_format"),
                "the frames stay on the device"
            );
            // The encoder is the vendor's own H.264 one, and no software pixel pipeline is in the graph.
            assert_eq!(flag("-c:v"), Some(vendor.encoder()));
            let vf = flag("-vf").unwrap();
            assert!(
                vf.starts_with(vendor.scaler()) && vf.contains("w=1280:h=720") && vf.contains("format=nv12"),
                "{vf}"
            );
            assert!(
                !vf.contains("scale=") && !vf.contains("hwdownload") && !vf.contains("hwupload"),
                "{vf}"
            );
        }
    }

    #[test]
    fn the_legacy_fps_flag_is_spelled_the_way_the_ffmpeg_takes_it() {
        let mut old = plan(FastVendor::Nvenc);
        old.fps_flag = "-vsync";
        let args = fast_proxy_args(&old, "/in", "/out");
        assert!(args.windows(2).any(|w| w[0] == "-vsync" && w[1] == "vfr"));
    }

    #[test]
    fn the_listing_decides_which_vendors_this_ffmpeg_could_run() {
        let hwaccels = "Hardware acceleration methods:\ncuda\nvaapi\nqsv\ndrm\namf\n";
        let filters = " .. vpp_amf           V->V       AMF video scaling\n .. scale_cuda        V->V       GPU\n .. scale_vaapi       V->V       x\n";
        assert!(vendor_in_listing(FastVendor::Amf, hwaccels, filters).is_ok());
        assert!(vendor_in_listing(FastVendor::Nvenc, hwaccels, filters).is_ok());
        assert!(vendor_in_listing(FastVendor::Vaapi, hwaccels, filters).is_ok());
        // `qsv` is a hwaccel here but `vpp_qsv` is not a filter of this build.
        assert!(vendor_in_listing(FastVendor::Qsv, hwaccels, filters)
            .unwrap_err()
            .contains("vpp_qsv"));
        // A build with no `amf` hwaccel says so.
        assert!(vendor_in_listing(FastVendor::Amf, "cuda\nvaapi\n", filters)
            .unwrap_err()
            .contains("-hwaccel amf"));
        // A name that only contains another's is not it.
        assert!(vendor_in_listing(FastVendor::Amf, "amf_x\n", filters).is_err());
    }

    // ---- the size -----------------------------------------------------------------------------------------

    #[test]
    fn the_proxy_size_is_explicit_even_and_what_scale_makes() {
        // 16:9 4K, 1080p and an odd-height 3:2 at the three sizes and the spherical one.
        assert_eq!(fast_dims(3840, 2160, 1280), Some((1280, 720)));
        assert_eq!(fast_dims(3840, 2160, 720), Some((720, 406)));
        assert_eq!(fast_dims(1920, 1080, 1080), Some((1080, 608)));
        assert_eq!(fast_dims(5760, 2880, 3072), Some((3072, 1536)));
        // A source narrower than the proxy is not scaled up (`min(W,iw)`), the height still even.
        assert_eq!(fast_dims(1000, 563, 1280), Some((1000, 564)));
        assert_eq!(fast_dims(1440, 1080, 1280), Some((1280, 960)));
        // A half goes up to the even number above it, as `-2` does (1000x563 at 720: 405.36 -> 406).
        assert_eq!(fast_dims(1000, 563, 720), Some((720, 406)));
        assert_eq!(fast_dims(1001, 563, 720), Some((720, 404)));
        // Vertical.
        assert_eq!(fast_dims(2160, 3840, 1280), Some((1280, 2276)));
    }

    #[test]
    fn a_size_the_hardware_encoders_do_not_take_is_refused_not_fitted() {
        // Odd width that is not scaled (x264 refuses it too), nothing, too small, too large.
        assert_eq!(fast_dims(1281, 720, 1920), None);
        assert_eq!(fast_dims(0, 720, 1280), None);
        assert_eq!(fast_dims(1280, 720, 0), None);
        assert_eq!(fast_dims(320, 240, 1280), None, "{FAST_MIN_SIDE} is the least a side may be");
        assert_eq!(fast_dims(5000, 400, 3072), None, "a 400-high 12:1 strip scales to 246");
        assert_eq!(fast_dims(8192, 8192, 8192), None, "{FAST_MAX_SIDE} is the most");
    }

    #[test]
    fn every_size_is_even_and_within_the_encoders_limits() {
        for src_w in [1920u32, 2560, 3840, 4096, 2704, 1440, 1000, 3000, 4000] {
            for src_h in (480u32..=3000).step_by(37) {
                for width in [720u32, 1080, 1280, 3072] {
                    if let Some((w, h)) = fast_dims(src_w, src_h, width) {
                        assert!(w % 2 == 0 && h % 2 == 0, "{src_w}x{src_h}@{width} -> {w}x{h}");
                        assert!(
                            w <= width.max(2) && w <= src_w,
                            "never scaled up: {src_w}x{src_h}@{width} -> {w}x{h}"
                        );
                        assert!((FAST_MIN_SIDE..=FAST_MAX_SIDE).contains(&w) && (FAST_MIN_SIDE..=FAST_MAX_SIDE).contains(&h));
                    }
                }
            }
        }
    }

    // ---- who is eligible ----------------------------------------------------------------------------------------

    /// A 4K H.264 phone clip that qualifies in every way.
    fn facts() -> FastFacts {
        FastFacts {
            traits: SourceTraits {
                described: true,
                indexed: true,
                codec: Codec::H264,
                chroma: Chroma::Yuv420,
                bits: Some(8),
                ..SourceTraits::default()
            },
            stored: StoredVideo {
                video_streams: 1,
                width: 3840,
                height: 2160,
                nb_frames: Some(900),
                duration: Some(30.0),
                field_order: Some("progressive".to_string()),
                sample_aspect_ratio: Some("1:1".to_string()),
                color_space: Some("bt709".to_string()),
                color_primaries: Some("bt709".to_string()),
                color_transfer: Some("bt709".to_string()),
                color_range: Some("tv".to_string()),
                ..StoredVideo::default()
            },
        }
    }

    fn refused(edit: impl FnOnce(&mut FastFacts)) -> String {
        let mut f = facts();
        edit(&mut f);
        fast_eligible(&f, 1280).expect_err("this source must be refused")
    }

    #[test]
    fn an_ordinary_source_the_hardware_decodes_is_eligible() {
        assert_eq!(fast_eligible(&facts(), 1280), Ok((SourceClass::H264, (1280, 720))));
        let mut hevc = facts();
        (hevc.traits.codec, hevc.traits.bits) = (Codec::Hevc, Some(10));
        assert_eq!(fast_eligible(&hevc, 1280).unwrap().0, SourceClass::Hevc10);
        hevc.traits.bits = Some(8);
        assert_eq!(fast_eligible(&hevc, 1280).unwrap().0, SourceClass::Hevc8);
        // Untagged footage and one with no field order or pixel shape stated are ordinary too.
        let mut bare = facts();
        bare.stored.color_space = None;
        bare.stored.color_primaries = None;
        bare.stored.color_transfer = None;
        bare.stored.color_range = None;
        bare.stored.field_order = None;
        bare.stored.sample_aspect_ratio = None;
        assert!(fast_eligible(&bare, 1280).is_ok());
    }

    #[test]
    fn every_source_the_pipeline_cannot_vouch_for_is_refused_with_its_reason() {
        let cases: Vec<(&str, String)> = vec![
            ("HDR", refused(|f| f.traits.hdr = Some(Hdr::Pq))),
            ("starts after the container", refused(|f| f.traits.lead = 0.08)),
            ("rotated for display", refused(|f| f.stored.rotation = 90)),
            ("rotated for display", refused(|f| f.stored.rotation = -90)),
            ("rotated for display", refused(|f| f.stored.rotation = 180)),
            ("interlaced", refused(|f| f.stored.field_order = Some("tt".to_string()))),
            ("interlaced", refused(|f| f.stored.field_order = Some("bb".to_string()))),
            (
                "not square",
                refused(|f| f.stored.sample_aspect_ratio = Some("4:3".to_string())),
            ),
            ("MPEG-TS", refused(|f| f.traits.indexed = false)),
            ("full range", refused(|f| f.traits.full_range = true)),
            ("full range", refused(|f| f.stored.color_range = Some("pc".to_string()))),
            ("exactly one picture stream", refused(|f| f.stored.video_streams = 2)),
            ("exactly one picture stream", refused(|f| f.stored.video_streams = 0)),
            ("could not be probed", refused(|f| f.traits.described = false)),
            ("how many frames", refused(|f| f.stored.nb_frames = None)),
            ("how many frames", refused(|f| f.stored.nb_frames = Some(1))),
            ("how many frames", refused(|f| f.stored.duration = None)),
            ("size", refused(|f| (f.stored.width, f.stored.height) = (320, 240))),
        ];
        for (needle, why) in &cases {
            assert!(why.contains(needle), "{needle:?} is not in {why:?}");
        }
    }

    #[test]
    fn a_codec_or_depth_no_hardware_decoder_vouches_for_is_refused() {
        let hardware = |edit: fn(&mut SourceTraits)| {
            let mut f = facts();
            edit(&mut f.traits);
            fast_eligible(&f, 1280).expect_err("refused")
        };
        // High10 H.264 (no D3D11VA, DXVA2 or VAAPI decoder), an unrecorded depth, 4:2:2, 4:4:4, AV1, ProRes, nothing known.
        assert!(hardware(|t| t.bits = Some(10)).contains("hardware decoder"));
        assert!(hardware(|t| t.bits = None).contains("hardware decoder"));
        assert!(hardware(|t| t.chroma = Chroma::Other).contains("hardware decoder"));
        assert!(hardware(|t| t.chroma = Chroma::Unknown).contains("hardware decoder"));
        assert!(hardware(|t| t.codec = Codec::Other).contains("hardware decoder"));
        assert!(hardware(|t| t.codec = Codec::Unknown).contains("hardware decoder"));
        // HEVC at a depth the probe never ran (12 bit) is refused though the decoder policy would allow it.
        let mut hevc12 = facts();
        (hevc12.traits.codec, hevc12.traits.bits) = (Codec::Hevc, Some(12));
        assert!(fast_eligible(&hevc12, 1280).unwrap_err().contains("bit depth"));
    }

    #[test]
    fn the_source_classes_follow_the_codec_and_the_depth() {
        let class = |codec, bits| {
            SourceClass::of(InputFacts {
                codec,
                bits,
                ..InputFacts::default()
            })
        };
        assert_eq!(class(Codec::H264, Some(8)), Some(SourceClass::H264));
        assert_eq!(class(Codec::Hevc, Some(8)), Some(SourceClass::Hevc8));
        assert_eq!(class(Codec::Hevc, Some(10)), Some(SourceClass::Hevc10));
        assert_eq!(class(Codec::H264, Some(10)), None);
        assert_eq!(class(Codec::Hevc, None), None);
        assert_eq!(class(Codec::Other, Some(8)), None);
    }

    // ---- the guard ------------------------------------------------------------------------------------------------------

    fn expect() -> FastExpect {
        FastExpect::of(&facts().stored, (1280, 720)).unwrap()
    }

    /// What a good 900-frame 30 fps proxy reads as.
    fn read() -> ProxyRead {
        ProxyRead {
            pts: (0..900).map(|i| f64::from(i) / 30.0).collect(),
            width: 1280,
            height: 720,
            color: expect().color,
        }
    }

    #[test]
    fn a_proxy_that_is_what_the_software_encode_gives_passes() {
        assert_eq!(check_fast_output(&expect(), &read()), Ok(()));
        // An encoder that writes `tv` where the source says nothing, or says it where the source is silent, is not full range.
        let mut e = expect();
        e.color.range = None;
        let mut r = read();
        r.color.range = Some("tv".to_string());
        assert_eq!(check_fast_output(&e, &r), Ok(()));
        // A variable-frame-rate clip whose last interval is not its mean.
        let mut vfr = read();
        let last = vfr.pts.len() - 1;
        vfr.pts[last] -= 0.02;
        assert_eq!(check_fast_output(&expect(), &vfr), Ok(()));
    }

    #[test]
    fn a_proxy_that_is_not_the_software_encode_is_refused_with_the_reason() {
        let refuse = |edit: &dyn Fn(&mut ProxyRead)| {
            let mut r = read();
            edit(&mut r);
            check_fast_output(&expect(), &r).expect_err("refused")
        };
        // Frames dropped or doubled: the count is the source's.
        assert!(refuse(&|r| {
            r.pts.pop();
        })
        .contains("899 frames"));
        assert!(refuse(&|r| r.pts.push(30.0)).contains("901 frames"));
        // Started late, or shifted: every seek would land elsewhere.
        assert!(refuse(&|r| r.pts.iter_mut().for_each(|t| *t += 0.0334)).contains("starts at"));
        // Frames that repeat or run backwards.
        assert!(refuse(&|r| r.pts[10] = r.pts[9]).contains("strictly increase"));
        assert!(refuse(&|r| r.pts.swap(10, 11)).contains("strictly increase"));
        // The right number of frames over the wrong time (a retimed output).
        assert!(refuse(&|r| r.pts.iter_mut().for_each(|t| *t *= 1.1)).contains("runs"));
        // Another size.
        assert!(refuse(&|r| r.width = 1278).contains("1278x720"));
        // A matrix the encoder dropped or changed: a renderer would read the picture in another.
        assert!(refuse(&|r| r.color.space = None).contains("colour tags"));
        assert!(refuse(&|r| r.color.transfer = Some("smpte2084".to_string())).contains("colour tags"));
        // Full range where the source is limited.
        assert!(refuse(&|r| r.color.range = Some("pc".to_string())).contains("colour tags"));
    }

    #[test]
    fn a_probed_proxy_reads_back_from_ffprobes_json() {
        let json = r#"{"packets":[{"pts_time":"0.000000"},{"pts_time":"0.033333"},{"pts_time":"0.066667"}],
            "streams":[{"width":1280,"height":720,"color_space":"bt709","color_primaries":"bt709","color_transfer":"bt709","color_range":"tv"}]}"#;
        let read = parse_proxy_read(json).expect("a proxy");
        assert_eq!((read.pts.len(), read.width, read.height), (3, 1280, 720));
        assert_eq!(read.color.space.as_deref(), Some("bt709"));
        assert_eq!(read.color.range.as_deref(), Some("tv"));
        // An unsaid tag (`unknown`, absent) is none.
        let bare = r#"{"packets":[{"pts_time":"0.0"}],"streams":[{"width":640,"height":360,"color_space":"unknown"}]}"#;
        assert_eq!(parse_proxy_read(bare).unwrap().color, ColorTags::default());
        // No packets, a packet with no time, no stream, junk: not a proxy.
        for bad in [
            r#"{"packets":[],"streams":[{"width":1,"height":1}]}"#,
            r#"{"packets":[{"flags":"K_"}],"streams":[{"width":1,"height":1}]}"#,
            r#"{"packets":[{"pts_time":"N/A"}],"streams":[{"width":1,"height":1}]}"#,
            r#"{"packets":[{"pts_time":"0.0"}],"streams":[]}"#,
            "not json",
        ] {
            assert_eq!(parse_proxy_read(bad), None, "{bad}");
        }
    }

    // ---- failing and falling back -------------------------------------------------------------------------------------

    fn failed(text: &str) -> FastFailure {
        classify_fast_failure(text)
    }

    #[test]
    fn what_reads_as_the_hardwares_failure_and_what_does_not() {
        for hardware in [
            "[AMF @ 0x1] DLL libamfrt64.so.1 failed to open\nDevice creation failed: -1313558101.",
            "[vist#0:0/h264 @ 0x1] [dec:h264_amf @ 0x2] No device available for decoder: device type amf needed for codec h264_amf.",
            "[h264_nvenc @ 0x1] Cannot load libcuda.so.1\nError while opening encoder - maybe incorrect parameters such as bit_rate, rate, width or height.",
            "[h264_qsv @ 0x1] Error creating a MFX session: -9.",
            "[h264_nvenc @ 0x1] OpenEncodeSessionEx failed: out of memory (10): (no details)",
            "[VAAPI @ 0x1] Cannot open DRM render node for device 0.",
            "Impossible to convert between the formats supported by the filter 'graph 0 input from stream 0:0' and the filter 'auto_scale_0'",
            "[Parsed_vpp_qsv_0 @ 0x1] No hw context provided.",
            "Unrecognized hwaccel: amf. Supported hwaccels: vaapi",
        ] {
            assert!(matches!(failed(hardware), FastFailure::Hardware(_)), "{hardware}");
        }
        for file in [
            "/in.mov: Invalid data found when processing input",
            "Error muxing a packet\nNo space left on device",
            "moov atom not found",
            "",
        ] {
            assert!(matches!(failed(file), FastFailure::Other(_)), "{file}");
        }
    }

    #[test]
    fn a_hardware_failure_the_software_rerun_cures_turns_the_pipeline_off_on_its_own_flag() {
        let health = FastHealth::new();
        assert!(health.usable().is_ok());
        let mut software_ran = false;
        let built = fast_then_software(
            &health,
            || FastEnd::Failed(failed("Device creation failed: -1")),
            || {
                software_ran = true;
                Ok("software proxy")
            },
        );
        assert_eq!(built.unwrap(), "software proxy");
        assert!(software_ran, "the proxy is built by x264");
        let off = health.usable().expect_err("convicted");
        assert!(off.contains("Device creation failed"), "{off}");
        // The setting turned off and on is "try again".
        health.reset();
        assert!(health.usable().is_ok());
    }

    #[test]
    fn a_stall_convicts_too_and_a_fast_proxy_that_works_never_runs_the_software_build() {
        let health = FastHealth::new();
        let built = fast_then_software(
            &health,
            || FastEnd::Done("fast proxy"),
            || -> Result<&str> { unreachable!("software ran") },
        );
        assert_eq!(built.unwrap(), "fast proxy");
        assert!(health.usable().is_ok());
        let built = fast_then_software(&health, || FastEnd::Failed(FastFailure::Stalled), || Ok("software proxy"));
        assert_eq!(built.unwrap(), "software proxy");
        assert!(health.usable().unwrap_err().contains("stopped reporting progress"));
    }

    #[test]
    fn a_failure_the_software_rerun_shares_is_the_files_and_counts_for_nothing() {
        let health = FastHealth::new();
        for _ in 0..10 {
            let built: Result<&str> = fast_then_software(
                &health,
                || FastEnd::Failed(failed("Device creation failed: -1")),
                || {
                    Err(Error::Engine(
                        "could not generate preview proxy: moov atom not found".to_string(),
                    ))
                },
            );
            assert!(built.is_err(), "the software error is the build's");
        }
        assert!(health.usable().is_ok(), "a bad file is not the GPU's fault");
    }

    #[test]
    fn a_cancel_is_not_rebuilt_and_not_counted() {
        let health = FastHealth::new();
        let built: Result<&str> = fast_then_software(&health, || FastEnd::Cancelled, || unreachable!("a cancel is not rebuilt"));
        assert!(matches!(built, Err(Error::Cancelled)));
        assert!(health.usable().is_ok());
    }

    #[test]
    fn failures_that_do_not_read_as_the_hardwares_count_three_in_a_row() {
        let health = FastHealth::new();
        let other = || FastEnd::<&str>::Failed(failed("Error submitting a packet to the muxer: Invalid argument"));
        for _ in 0..2 {
            fast_then_software(&health, other, || Ok("software")).unwrap();
        }
        assert!(health.usable().is_ok(), "two strikes");
        // A fast build that passes the guard starts the count again.
        fast_then_software(&health, || FastEnd::Done("fast"), || Ok("software")).unwrap();
        for _ in 0..2 {
            fast_then_software(&health, other, || Ok("software")).unwrap();
        }
        assert!(health.usable().is_ok(), "the count started again");
        fast_then_software(&health, other, || Ok("software")).unwrap();
        assert!(health.usable().is_err(), "three in a row");
        // A proxy the guard refused is one of those.
        let guard = FastHealth::new();
        for _ in 0..3 {
            let rejected = FastEnd::<&str>::Failed(FastFailure::Other(
                "the proxy has 899 frames and the source has 900".to_string(),
            ));
            fast_then_software(&guard, || rejected, || Ok("software")).unwrap();
        }
        assert!(guard.usable().unwrap_err().contains("899 frames"));
    }

    #[test]
    fn convicting_the_fast_pipeline_leaves_the_stitch_flag_and_the_decode_proof_alone() {
        let _flags = fast_test_lock();
        // The process-wide flags this one must never touch: the Insta360 stitch's `HW_ENCODE_OK` and the
        // export's `HWACCEL_PROVEN_BAD`. (Not `HWACCEL_OK`: the previews' learned fallback, which another
        // unit test clears on purpose and leaves cleared, so it is not this test's to compare.)
        let before = (
            HW_ENCODE_OK.load(Ordering::Relaxed),
            HWACCEL_PROVEN_BAD.load(Ordering::Relaxed),
        );
        FAST_PROXY_HEALTH.reset();
        fast_then_software(
            &FAST_PROXY_HEALTH,
            || FastEnd::<()>::Failed(failed("Cannot load libcuda.so.1")),
            || Ok(()),
        )
        .unwrap();
        assert!(FAST_PROXY_HEALTH.usable().is_err(), "the proxy flag is off");
        let after = (
            HW_ENCODE_OK.load(Ordering::Relaxed),
            HWACCEL_PROVEN_BAD.load(Ordering::Relaxed),
        );
        assert_eq!(before, after, "HW_ENCODE_OK and HWACCEL_PROVEN_BAD are as they were");
        FAST_PROXY_HEALTH.reset();
        assert!(FAST_PROXY_HEALTH.usable().is_ok());
    }

    #[test]
    fn turning_the_setting_off_and_on_gives_a_convicted_pipeline_another_chance() {
        let _flags = fast_test_lock();
        set_proxy_fast_gpu(false);
        FAST_PROXY_HEALTH.record(&FastFailure::Stalled);
        assert!(FAST_PROXY_HEALTH.usable().is_err());
        // Setting it on again (the check is not allowed to run in unit tests) re-arms it; setting it on
        // when it already is changes nothing.
        set_proxy_fast_gpu(true);
        assert!(FAST_PROXY_HEALTH.usable().is_ok(), "off and on is \"try again\"");
        FAST_PROXY_HEALTH.record(&FastFailure::Stalled);
        set_proxy_fast_gpu(true);
        assert!(FAST_PROXY_HEALTH.usable().is_err(), "on while on re-arms nothing");
        FAST_PROXY_HEALTH.reset();
        set_proxy_fast_gpu(false);
    }

    #[test]
    fn a_world_pinned_on_one_thread_is_seen_by_that_thread_alone() {
        let _flags = fast_test_lock();
        set_proxy_fast_gpu(false);
        FAST_PROXY_HEALTH.reset();
        let seen_elsewhere = with_fast_world(true, || {
            assert!(proxy_fast_gpu(), "this thread has the setting on");
            fast_health().record(&FastFailure::Stalled);
            assert!(fast_health().usable().is_err(), "and a conviction of its own");
            // Another thread (another test) has the process's: off, and nobody convicted.
            std::thread::spawn(|| (proxy_fast_gpu(), fast_health().usable().is_ok()))
                .join()
                .unwrap()
        });
        assert_eq!(seen_elsewhere, (false, true));
        // Back to the process's own once the world is left, whatever happened inside it.
        assert!(!proxy_fast_gpu() && fast_health().usable().is_ok() && FAST_PROXY_HEALTH.usable().is_ok());
    }

    // ---- what the settings page shows ---------------------------------------------------------------------------------------

    #[test]
    fn the_status_is_unknown_until_the_check_has_run_and_then_says_what_it_found() {
        let unknown = fast_status_of(None, Ok(()));
        assert_eq!(
            (
                unknown.available,
                unknown.checked,
                unknown.pipeline,
                unknown.reason.as_deref()
            ),
            (false, false, None, Some("not checked yet"))
        );
        let found = FastProbe {
            pipeline: Some(FastPipeline {
                vendor: FastVendor::Amf,
                device: None,
                covers: vec![SourceClass::H264],
            }),
            reason: String::new(),
        };
        let ok = fast_status_of(Some(&found), Ok(()));
        assert_eq!(
            (ok.available, ok.checked, ok.pipeline, ok.reason),
            (true, true, Some("AMD AMF"), None)
        );
        // Found and then convicted: not available, and the vendor and the reason are said.
        let off = fast_status_of(Some(&found), Err("Fast (GPU) stopped working this session: x".to_string()));
        assert_eq!((off.available, off.pipeline), (false, Some("AMD AMF")));
        assert!(off.reason.unwrap().contains("stopped working"));
        let nothing = FastProbe {
            pipeline: None,
            reason: "AMD AMF: this ffmpeg has no `-hwaccel amf`".to_string(),
        };
        let none = fast_status_of(Some(&nothing), Ok(()));
        assert_eq!((none.available, none.checked, none.pipeline), (false, true, None));
        assert!(none.reason.unwrap().contains("no `-hwaccel amf`"));
    }

    fn found(covers: &[SourceClass]) -> FastPipeline {
        FastPipeline {
            vendor: FastVendor::Nvenc,
            device: None,
            covers: covers.to_vec(),
        }
    }

    #[test]
    fn a_source_is_planned_for_the_vendor_that_passed_on_its_kind_of_source() {
        let (plan, expect) = plan_for(&facts(), 1280, &found(&SourceClass::ALL), "-fps_mode").unwrap();
        assert_eq!(
            plan,
            FastPlan {
                vendor: FastVendor::Nvenc,
                device: None,
                width: 1280,
                height: 720,
                fps_flag: "-fps_mode"
            }
        );
        assert_eq!((expect.frames, expect.width, expect.height), (900, 1280, 720));
        // The VAAPI node the check verified goes with the plan.
        let mut vaapi = found(&SourceClass::ALL);
        (vaapi.vendor, vaapi.device) = (FastVendor::Vaapi, Some("/dev/dri/renderD129".to_string()));
        assert_eq!(
            plan_for(&facts(), 1280, &vaapi, "-vsync").unwrap().0.device.as_deref(),
            Some("/dev/dri/renderD129")
        );
        // A vendor that passed the check on H.264 only is not given HEVC, 10-bit or not.
        let mut hevc10 = facts();
        (hevc10.traits.codec, hevc10.traits.bits) = (Codec::Hevc, Some(10));
        let why = plan_for(&hevc10, 1280, &found(&[SourceClass::H264]), "-fps_mode").unwrap_err();
        assert!(why.contains("NVIDIA NVENC") && why.contains("HEVC 10-bit"), "{why}");
        assert!(plan_for(&hevc10, 1280, &found(&[SourceClass::H264, SourceClass::Hevc10]), "-fps_mode").is_ok());
        // An ineligible source says why it is (the vendor does not matter).
        let mut hdr = facts();
        hdr.traits.hdr = Some(Hdr::Hlg);
        assert!(plan_for(&hdr, 1280, &found(&SourceClass::ALL), "-fps_mode")
            .unwrap_err()
            .contains("HDR"));
    }

    #[test]
    fn the_choice_is_software_with_nothing_said_unless_the_user_asked_and_with_the_reason_when_they_did() {
        let _flags = fast_test_lock();
        let nowhere = Path::new("/kerf-no-such-source.mp4");
        let said = |choice: FastChoice| match choice {
            FastChoice::Software(why) => why,
            FastChoice::Gpu(..) => panic!("a GPU plan where none was possible"),
        };
        // The setting off: nothing is looked at (this source does not exist, and nothing probes it).
        set_proxy_fast_gpu(false);
        assert_eq!(said(choose_pipeline(nowhere, 1280, SourceTraits::default(), &|| false)), None);
        // On, with no pipeline that passed the check: the check's reason.
        set_proxy_fast_gpu(true);
        let why = with_pinned_fast_check(None, || {
            said(choose_pipeline(nowhere, 1280, SourceTraits::default(), &|| false))
        });
        assert!(why.unwrap().contains("nothing passed"));
        // On, with one, for a source that cannot be read: said so, not tried.
        let why = with_pinned_fast_check(Some(FastVendor::Amf), || {
            said(choose_pipeline(nowhere, 1280, SourceTraits::default(), &|| false))
        });
        assert!(why.unwrap().contains("could not be read"));
        // What the previews learned about `-hwaccel auto` is not this pipeline's business: with it cleared (as
        // any rescue of a still does, and as `disabling_decode_hwaccel_is_shared_with_the_preview_path` leaves
        // it for the process), a pipeline that passed its check is still offered, and the reason is the source's.
        disable_decode_hwaccel();
        let why = with_pinned_fast_check(Some(FastVendor::Amf), || {
            said(choose_pipeline(nowhere, 1280, SourceTraits::default(), &|| false))
        });
        assert!(why.unwrap().contains("could not be read"), "the learned flag was asked");
        // On, with one, convicted this session: x264, and the reason says why the GPU is not used.
        FAST_PROXY_HEALTH.record(&FastFailure::Stalled);
        let why = with_pinned_fast_check(Some(FastVendor::Amf), || {
            said(choose_pipeline(nowhere, 1280, SourceTraits::default(), &|| false))
        });
        assert!(why.unwrap().contains("stopped working"));
        FAST_PROXY_HEALTH.reset();
        set_proxy_fast_gpu(false);
    }

    #[test]
    fn a_proxy_that_is_cancelled_while_it_waits_for_the_check_stops_waiting() {
        // The check is not allowed to run in unit tests, so it never answers: a wait for ten minutes
        // that is cancelled ends at the next poll, a quarter of a second at most, not at the limit.
        let started = Instant::now();
        let probe = fast_probe_until(std::time::Duration::from_secs(600), &|| true);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(probe.is_none() || FAST_PROBE.get().is_some());
        // Not cancelled, and a limit that has run out: the same answer, as soon.
        let started = Instant::now();
        let _ = fast_probe_until(std::time::Duration::from_millis(100), &|| false);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_failed_probe_run_is_explained_by_its_first_lines_not_its_last() {
        let noisy = "[AMF @ 0x1] DLL libamfrt64.so.1 failed to open\nDevice creation failed: -1313558101.\n[vist#0:0/h264] No device\nError opening output file -.\n";
        assert_eq!(
            failure_reason(noisy),
            "[AMF @ 0x1] DLL libamfrt64.so.1 failed to open / Device creation failed: -1313558101."
        );
        assert_eq!(failure_reason("\n  Unrecognized option 'x'.\n"), "Unrecognized option 'x'.");
        assert_eq!(failure_reason(""), "ffmpeg failed");
        assert_eq!(failure_reason(" \n\n "), "ffmpeg failed");
    }

    #[test]
    fn a_pinned_check_is_what_a_proxy_build_and_the_status_read() {
        let pinned = FastProbe {
            pipeline: None,
            reason: "pinned".to_string(),
        };
        with_fast_probe(pinned, || {
            assert_eq!(fast_probe_within(std::time::Duration::ZERO).unwrap().reason, "pinned");
            assert_eq!(fast_pipeline_status().reason.as_deref(), Some("pinned"));
        });
    }
}
