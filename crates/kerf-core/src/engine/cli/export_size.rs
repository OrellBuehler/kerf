//! How big an export will be, before it is rendered.
//!
//! The export dialog wants a size to show next to the Export button, and the platform check wants
//! one to hold against a platform's file-size cap. Both want it before anyone waits for the render, so
//! it is a pure estimate over what the options and the cut already say: no ffmpeg, no file read, no
//! lock (`Project::export_size_inputs` snapshots the timeline, [`estimate_export`] computes outside
//! it). Everything here is a **range** with the way it was reached ([`SizeBasis`]), never one number
//! that pretends to be a prediction.
//!
//! # What is arithmetic and what is a model
//!
//! * **Exact**: PCM (`wav`, `pcm_*` in a video container) is rate x channels x width x length;
//!   constant-bitrate MP3 is its bitrate x length. The band is a container's headers and nothing else.
//! * **Targeted**: a target bitrate (`-b:v`, single- or two-pass, or `-b:a` of a lossy codec) is
//!   bitrate x length, held by the rate control to within a tolerance that depends on how long the
//!   cut is (see [`overshoot`]) and that never reaches *under* it for content that needs the bits
//!   (simpler pictures come out smaller: the low end is a fraction of the target).
//! * **Modelled**: constant quality (CRF) has no arithmetic. The bytes depend on how much detail and
//!   motion the footage has, which nothing here can see, so the model is two curves of *bits per pixel
//!   per frame* against the CRF, per codec: [`Curve::low`] for soft, low-detail pictures (a talking
//!   head, a slide, a screen) and [`Curve::high`] for grainy, high-detail ones. Real footage lands
//!   between them; the band is wide on purpose (75-230x for x264 and x265 at the CRFs the dialog
//!   offers, 110-385x for VP9 and AV1, 3-9x for ProRes) and the note says so. GIF and lossless are
//!   modelled the same way with their own bands.
//! * **Unknown**: said out loud, with the reason (an encoder the model does not know, a cut with
//!   nothing in it). There is no container that writes an image sequence, so none is estimated.
//!
//! # Where the numbers come from
//!
//! Every constant below was measured with the pinned FFmpeg 9.0.2 (libx264, libx265, libvpx-vp9,
//! libsvtav1, prores_ks, the gif palette pipeline) on lavfi sources of three kinds, at 1080p30 unless
//! a factor says otherwise: a blurred `testsrc2` (low detail, 0.014 bpp at x264 CRF 23), plain
//! `testsrc2` (0.098) and `testsrc2` plus temporal noise pre-encoded at 25 Mbps the way a camera
//! would hand it over (0.58), so the sources are compressed footage, not raw noise. Each curve's low
//! end sits at 0.6x the easiest class and its high end at 1.4x the hardest, with the slope fitted
//! through the CRFs the dialog offers. `export_size_rendered` re-measures through the real export
//! (`cargo test -p kerf-core --no-default-features -- --ignored export_size_rendered`) and holds the
//! bands to it; `.claude/docs/engine-cli.md` has the table and says what falls outside.
//!
//! Outside the high end, on purpose: raw white noise (x264 CRF 23 costs 2.8 bpp, three and a half times the top of
//! the band) and a source with more bits per pixel than the camera-grade one above (60 Mbps noise
//! at 1080p30 costs 1.15 bpp at CRF 23). Nothing a camera records is noise, and a band that covered
//! it would say "up to gigabytes" of every export.
//!
//! Hardware encoders reuse the software curve of their codec with a wider band ([`hardware_spread`]):
//! one clip on an RX 7900 XT is all the data there is (see `hw_qp`), and it puts AMF at the x264 curve
//! for H.264 and HEVC alike.

use serde::Serialize;

use super::*;

/// How an estimate was reached, and so how far to trust it. Ordered from the firmest: a total over
/// parts takes the loosest basis among them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SizeBasis {
    /// Arithmetic from the options: the range is the container's headers.
    Exact,
    /// A target bitrate times the length, within the rate control's tolerance.
    Targeted,
    /// A model over the codec and the quality: the footage decides, so the range is wide.
    Modelled,
    /// Cannot be estimated; the note says why and the bounds are 0.
    Unknown,
}

/// The two ends of an estimate, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct SizeBounds {
    pub low_bytes: u64,
    pub high_bytes: u64,
}

/// What one output file will weigh: a range, how it was reached, and what to tell a person about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct SizeEstimate {
    pub basis: SizeBasis,
    /// The least the file plausibly weighs. 0 when `basis` is `unknown`.
    pub low_bytes: u64,
    /// The most it plausibly weighs (a hard source at this quality; noise-grade footage can exceed
    /// it, see the module docs). 0 when `basis` is `unknown`.
    pub high_bytes: u64,
    /// One or two sentences for the dialog: what the number depends on, or why there is none.
    pub note: String,
}

impl SizeEstimate {
    /// The range to hold against a limit; `None` when the file cannot be estimated.
    pub fn bounds(&self) -> Option<SizeBounds> {
        (self.basis != SizeBasis::Unknown).then_some(SizeBounds {
            low_bytes: self.low_bytes,
            high_bytes: self.high_bytes,
        })
    }

    fn unknown(why: impl Into<String>) -> Self {
        Self {
            basis: SizeBasis::Unknown,
            low_bytes: 0,
            high_bytes: 0,
            note: why.into(),
        }
    }
}

/// What the sound of an export is, for the estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeSound {
    pub sample_rate: u32,
    /// 1 or 2: the graph mixes down to mono or stereo.
    pub channels: u16,
}

/// What an estimate reads off the cut, resolved by [`export_size_inputs`] for one set of options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizeInputs {
    /// The length of the file: the range's, or the whole cut's.
    pub duration_secs: f64,
    /// The picture the file carries, as the export will render it; `None` for a file without one.
    pub picture: Option<ExportedFrame>,
    /// The sound the file carries; `None` for a file without any.
    pub sound: Option<SizeSound>,
}

/// One output file of an export and what it will weigh.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct FileSizeEstimate {
    /// The delivery frame this file is cut for, in a multi-format export; `None` for the single file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    /// The length of the file, seconds: what the estimate was made for.
    pub duration_secs: f64,
    /// The picture it was made for (size, rate and where the rate came from); `None` for a file without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<ExportedFrame>,
    pub estimate: SizeEstimate,
}

/// What an export of the cut with some options will weigh, file by file.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct ExportSizeEstimate {
    /// One entry per file, in the order the export writes them: the single file, or each delivery.
    pub files: Vec<FileSizeEstimate>,
    /// All files together; `None` when any of them cannot be estimated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<SizeBounds>,
}

// ---- resolving the inputs ---------------------------------------------------------------------

/// The file `opts` makes of `timeline`, as far as its size is concerned: the length of the range
/// (or the cut), and the picture and sound the container carries, resolved the way
/// [`build_export_args`] gates them (the same cut, the same format, the same stream rules).
pub fn export_size_inputs(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> SizeInputs {
    let cut = export_timeline(timeline, opts);
    let c = opts.container;
    let has_video = cut.tracks.iter().any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    let has_audio = cut
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .any(|clip| clip_sounds(clip, assets));
    let (fmt, fps_source) = export_format_sourced(&cut, assets, opts);
    let picture = (has_video && !c.is_audio_only()).then_some(ExportedFrame {
        width: fmt.width,
        height: fmt.height,
        fps: fmt.fps,
        fps_source,
    });
    let sound = (has_audio && !c.is_video_only() && opts.include_audio).then_some(SizeSound {
        sample_rate: fmt.sample_rate,
        channels: if fmt.channels <= 1 { 1 } else { 2 },
    });
    SizeInputs {
        duration_secs: cut.duration(),
        picture,
        sound,
    }
}

/// What an export of `timeline` with `opts` will weigh. With `deliveries`, one file per delivery
/// frame (each cut for its shape, [`Timeline::for_delivery`], the way [`render_variants`] writes
/// them); without, the one file. Pure: it reads only its arguments.
pub fn estimate_export(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    deliveries: &[Delivery],
) -> ExportSizeEstimate {
    let one = |delivery: Option<Delivery>| {
        let (inputs, opts) = match delivery {
            Some(d) => {
                let per_delivery = ExportOptions {
                    resolution: Some((d.width, d.height)),
                    fit: d.fit,
                    ..opts.clone()
                };
                (
                    export_size_inputs(&timeline.for_delivery(d), assets, &per_delivery),
                    per_delivery,
                )
            }
            None => (export_size_inputs(timeline, assets, opts), opts.clone()),
        };
        FileSizeEstimate {
            delivery,
            duration_secs: inputs.duration_secs,
            frame: inputs.picture,
            estimate: estimate_size(&inputs, &opts),
        }
    };
    let files: Vec<FileSizeEstimate> = if deliveries.is_empty() {
        vec![one(None)]
    } else {
        deliveries.iter().map(|d| one(Some(*d))).collect()
    };
    let total = files
        .iter()
        .map(|f| f.estimate.bounds())
        .try_fold((0u64, 0u64), |(lo, hi), b| {
            b.map(|b| (lo.saturating_add(b.low_bytes), hi.saturating_add(b.high_bytes)))
        })
        .map(|(low_bytes, high_bytes)| SizeBounds { low_bytes, high_bytes });
    ExportSizeEstimate { files, total }
}

// ---- the estimate ------------------------------------------------------------------------------

/// A part of a file (its picture, its sound) in bytes, and how it was reached.
struct Band {
    low: f64,
    high: f64,
    basis: SizeBasis,
    /// What to say about it; empty for a part that needs no words.
    note: &'static str,
}

impl Band {
    fn new(low: f64, high: f64, basis: SizeBasis, note: &'static str) -> Self {
        Self { low, high, basis, note }
    }
}

/// Why a part cannot be estimated.
struct Cannot(&'static str);

/// What a part of the estimate answers: its bytes, or why it has none.
type Part<T> = std::result::Result<T, Cannot>;

/// What `opts` make of `inputs`, in bytes. Pure and total: any options and any inputs answer, with
/// `unknown` for what the model does not cover.
pub fn estimate_size(inputs: &SizeInputs, opts: &ExportOptions) -> SizeEstimate {
    let secs = inputs.duration_secs;
    if !secs.is_finite() || secs <= 0.0 {
        return SizeEstimate::unknown("The cut is empty, so there is nothing to size.");
    }
    let c = opts.container;
    let picture = inputs.picture.filter(|_| !c.is_audio_only());
    let sound = inputs.sound.filter(|_| !c.is_video_only() && opts.include_audio);
    if picture.is_none() && sound.is_none() {
        return SizeEstimate::unknown("These settings would export nothing.");
    }

    let mut parts = Vec::new();
    if let Some(p) = picture {
        match video_band(p, secs, opts) {
            Ok(band) => parts.push(band),
            Err(Cannot(why)) => return SizeEstimate::unknown(why),
        }
    }
    if let Some(s) = sound {
        match audio_band(s, secs, opts) {
            Ok(band) => parts.push(band),
            Err(Cannot(why)) => return SizeEstimate::unknown(why),
        }
    }
    let frames = picture.map_or(0.0, |p| frame_count(p.fps, secs));
    let (head_low, head_high) = container_overhead(opts, frames, if sound.is_some() { secs } else { 0.0 });

    let mut low = head_low;
    let mut high = head_high;
    let mut basis = SizeBasis::Exact;
    let mut notes: Vec<&str> = Vec::new();
    for part in &parts {
        low += part.low;
        high += part.high;
        basis = basis.max(part.basis);
        if !part.note.is_empty() && !notes.contains(&part.note) {
            notes.push(part.note);
        }
    }
    SizeEstimate {
        basis,
        low_bytes: to_bytes(low),
        high_bytes: to_bytes(high.max(low)),
        note: notes.join(" "),
    }
}

/// `bytes` as a count, rounded outward by the caller; non-finite and negative answers are 0 and a
/// huge one saturates.
fn to_bytes(bytes: f64) -> u64 {
    if bytes.is_finite() && bytes > 0.0 {
        bytes as u64
    } else {
        0
    }
}

/// How many pictures a file of `secs` at `fps` holds.
fn frame_count(fps: f64, secs: f64) -> f64 {
    (sane_fps(fps) * secs).round().max(1.0)
}

/// A frame rate that can be multiplied: positive and finite, else the 30 the format defaults to.
fn sane_fps(fps: f64) -> f64 {
    if fps.is_finite() && fps > 0.0 {
        fps
    } else {
        30.0
    }
}

// ---- the picture --------------------------------------------------------------------------------

/// The frame every curve below is measured at: 1080p, 30 fps, 8-bit 4:2:0.
const REF_PIXELS: f64 = 1920.0 * 1080.0;
const REF_FPS: f64 = 30.0;

/// The most a picture can cost per pixel: lossless x264 on white noise measured 8.4 bits.
const MAX_BPP: f64 = 9.0;
/// The least a picture of a cut with anything moving in it can cost; static test cards are below.
const MIN_BPP: f64 = 0.0003;

/// A codec's constant-quality curve: bits per pixel per frame at `ref_crf`, halving every
/// `*_halving` CRF steps, for soft (`low`) and for grainy, detailed (`high`) pictures.
struct Curve {
    ref_crf: f64,
    low: f64,
    low_halving: f64,
    high: f64,
    high_halving: f64,
}

impl Curve {
    fn bpp(&self, crf: f64) -> (f64, f64) {
        let low = self.low * ((self.ref_crf - crf) / self.low_halving).exp2();
        let high = self.high * ((self.ref_crf - crf) / self.high_halving).exp2();
        (low, high)
    }
}

/// x264, medium. Measured 1080p30, CRF 14..38: soft 0.034..0.0047 bpp, plain 0.20..0.017, grainy
/// 1.46..0.022 (and still 0.119 at CRF 32, where the grain stops being coded).
const H264: Curve = Curve {
    ref_crf: 23.0,
    low: 0.0084,
    low_halving: 8.4,
    high: 0.81,
    high_halving: 7.0,
};
/// x265, medium. CRF 18..38: soft 0.011..0.0023, plain 0.19..0.015, grainy 0.90..0.0092.
const HEVC: Curve = Curve {
    ref_crf: 23.0,
    low: 0.0036,
    low_halving: 8.5,
    high: 0.76,
    high_halving: 6.5,
};
/// SVT-AV1, preset 8. CRF 24..50: soft 0.0064..0.0015, plain 0.20..0.018, grainy 1.06..0.025.
const AV1: Curve = Curve {
    ref_crf: 30.0,
    low: 0.0027,
    low_halving: 12.0,
    high: 0.75,
    high_halving: 7.0,
};
/// libvpx-vp9, `-cpu-used 4 -deadline good`. CRF 24..45: soft 0.0088..0.0022, plain 0.17..0.041,
/// grainy 0.95..0.114.
const VP9: Curve = Curve {
    ref_crf: 31.0,
    low: 0.0031,
    low_halving: 10.0,
    high: 0.89,
    high_halving: 8.0,
};

/// What `-crf 0` costs per pixel in the lossless mode, over the soft and the grainy class. x265's
/// soft 0.148 and VP9's grainy 5.66 (x264 on white noise 8.4) bound it, and SVT-AV1's `lossless=1`
/// (0.59 / 0.69 / 4.6 at 360p) sits inside; it does not move with the frame rate or the size.
const LOSSLESS_BPP: (f64, f64) = (0.08, MAX_BPP);

/// ProRes (`prores_ks`) per profile 0..5, bits per pixel per frame at 1080p, over the soft and the
/// grainy class: it codes every frame alone at a rate the profile sets, so neither the CRF nor the
/// frame rate enters. Measured 1080p30: Proxy 0.36..0.78, LT 0.76..1.76, Standard 0.99..2.5, HQ
/// 1.36..3.7, 4444 / XQ 1.8..15.8 (the 4444 profiles carry alpha). The soft end falls with the frame
/// (HQ 2.9 at 360p, 1.36 at 1080p, 0.85 at 4K: the pixels ^ -0.35 of `PRORES_SIZE_LOW`) and the grainy
/// end rises on a small one (6.2 at 360p, 3.7 at 4K: `(1080p / pixels) ^ 0.3`, from 1080p down).
const PRORES_BPP: [(f64, f64); 6] = [(0.30, 1.0), (0.65, 2.2), (0.85, 3.1), (1.15, 4.7), (1.5, 19.0), (1.5, 19.0)];
/// How ProRes's soft end moves with the number of pixels, as a power of `pixels / 1080p`.
const PRORES_SIZE_LOW: f64 = -0.35;
/// How ProRes's grainy end rises below 1080p, as a power of `1080p / pixels`.
const PRORES_SIZE_HIGH: f64 = 0.3;
/// Raw 4:4:4 10-bit is 30 bits per pixel: nothing coded costs more.
const PRORES_MAX_BPP: f64 = 30.0;

/// GIF, bytes per pixel per frame (palette pipeline, `stats_mode=diff`, any dither): flat colour and
/// static pictures 0.001..0.06, gradients and blur 0.05..0.10, grain 0.62..0.68.
const GIF_BYTES_PER_PIXEL: (f64, f64) = (0.01, 0.9);

/// The curve family a `-c:v` belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    H264,
    Hevc,
    Vp9,
    Av1,
}

/// How the picture is coded, from the container and the options.
enum Picture {
    Gif,
    ProRes,
    /// A software or a hardware encoder of this codec.
    Coded(Family, EncFamily),
}

fn picture_kind(opts: &ExportOptions) -> Part<Picture> {
    if opts.container == Container::Gif {
        return Ok(Picture::Gif);
    }
    let Some(vc) = opts.video_codec.as_deref() else {
        // The encoder ffmpeg takes for a container when `-c:v` is not given (FFmpeg 9.0.2: x264 for mp4,
        // mov and mkv, VP9 for webm).
        return match opts.container {
            Container::Mp4 | Container::Mov | Container::Mkv => Ok(Picture::Coded(Family::H264, EncFamily::Software)),
            Container::Webm => Ok(Picture::Coded(Family::Vp9, EncFamily::Software)),
            _ => Err(Cannot("No video encoder is chosen for this container.")),
        };
    };
    match vc {
        "gif" => Ok(Picture::Gif),
        "prores_ks" => Ok(Picture::ProRes),
        "libx264" => Ok(Picture::Coded(Family::H264, EncFamily::Software)),
        "libx265" => Ok(Picture::Coded(Family::Hevc, EncFamily::Software)),
        "libvpx-vp9" => Ok(Picture::Coded(Family::Vp9, EncFamily::Software)),
        "libsvtav1" => Ok(Picture::Coded(Family::Av1, EncFamily::Software)),
        _ => {
            let hardware = enc_family(vc);
            let family = if vc.starts_with("h264_") {
                Family::H264
            } else if vc.starts_with("hevc_") {
                Family::Hevc
            } else if vc.starts_with("av1_") {
                Family::Av1
            } else {
                return Err(Cannot("This video encoder is not one the size model knows."));
            };
            if hardware == EncFamily::Software {
                return Err(Cannot("This video encoder is not one the size model knows."));
            }
            Ok(Picture::Coded(family, hardware))
        }
    }
}

/// The CRF the options mean: the one given, else what the encoder uses when none is (measured on
/// FFmpeg 9.0.2: libx264 23, libx265 28, libsvtav1 35, libvpx-vp9 about 32); a hardware encoder
/// without one is read at the dialog's default for its codec.
fn effective_crf(family: Family, hardware: EncFamily, crf: Option<u32>) -> f64 {
    let max = match family {
        Family::H264 | Family::Hevc => 51.0,
        Family::Vp9 | Family::Av1 => 63.0,
    };
    if let Some(n) = crf {
        return f64::from(n).min(max);
    }
    match (family, hardware) {
        (Family::H264, _) => 23.0,
        (Family::Hevc, EncFamily::Software) => 28.0,
        (Family::Hevc, _) => 25.0,
        (Family::Av1, EncFamily::Software) => 35.0,
        (Family::Av1, _) => 30.0,
        (Family::Vp9, _) => 32.0,
    }
}

/// The curve a codec is read on. A hardware HEVC encoder is read on the x264 curve: the one clip
/// measured (`hw_qp`) put `hevc_amf` at the same size as x264 at the same CRF, where x265's own curve
/// runs larger on plain and grainy pictures and smaller on soft ones.
fn curve_of(family: Family, hardware: EncFamily) -> &'static Curve {
    match (family, hardware) {
        (Family::Hevc, EncFamily::Software) => &HEVC,
        (Family::H264 | Family::Hevc, _) => &H264,
        (Family::Vp9, _) => &VP9,
        (Family::Av1, _) => &AV1,
    }
}

/// How much wider a hardware encoder's band is than the software curve's: `(low, high)` multipliers.
/// AMF and VAAPI map the CRF to the constant QP that measured equal to x264 on one clip; NVENC, QuickSync
/// and VideoToolbox have no measurement at all (an AMF at x264's own CRF number spent 1.9x the bits), so
/// they get the widest.
fn hardware_spread(hardware: EncFamily) -> (f64, f64) {
    match hardware {
        EncFamily::Software => (1.0, 1.0),
        EncFamily::Amf | EncFamily::Vaapi => (0.6, 1.8),
        EncFamily::Nvenc | EncFamily::Qsv => (0.5, 2.5),
        EncFamily::VideoToolbox => (0.4, 3.0),
    }
}

/// What a speed preset does to the size at the same CRF, `(low, high)` multipliers, measured at CRF 23
/// (x264, x265) / 30 (SVT-AV1) / 31 (VP9) on the three classes. A preset is only read for the software
/// encoders that take one, and an absent or unknown one is the encoder's default (x264 / x265 medium,
/// SVT-AV1 10, VP9 4).
fn preset_factors(family: Family, hardware: EncFamily, preset: Option<&str>) -> (f64, f64) {
    if hardware != EncFamily::Software {
        return (1.0, 1.0);
    }
    let p = preset.unwrap_or("");
    match family {
        // Soft / plain pictures cost 2.7x / 2.5x at ultrafast, 2.0x / 1.75x at superfast, 0.76x at
        // veryslow; grainy ones 1.7x, 1.3x and 0.95x.
        Family::H264 => match p {
            "ultrafast" => (2.0, 1.9),
            "superfast" => (1.6, 1.4),
            "veryfast" | "faster" => (0.9, 1.0),
            "fast" => (1.0, 1.05),
            "slow" => (0.95, 1.0),
            "slower" => (0.85, 1.0),
            "veryslow" => (0.75, 1.0),
            _ => (1.0, 1.0),
        },
        // Ultrafast makes soft pictures 0.82x and grainy ones 0.61x; slow and slower grainy 1.1x.
        Family::Hevc => match p {
            "ultrafast" => (0.8, 1.0),
            "superfast" => (0.85, 1.0),
            "veryfast" | "faster" | "fast" => (0.9, 1.0),
            "slow" | "slower" | "veryslow" => (0.9, 1.15),
            _ => (0.95, 1.0),
        },
        // Presets 2..12 at CRF 30: soft 0.78..0.91x, plain 0.68..1.15x, grainy 0.78..1.04x.
        Family::Av1 => match p.parse::<u32>() {
            Ok(n) if n <= 4 => (0.65, 1.0),
            Ok(6) => (0.75, 1.0),
            Ok(n) if n >= 10 => (0.8, 1.1),
            _ => (0.8, 1.0),
        },
        // `-cpu-used` 0..8: soft 0.86..1.04x, plain 0.93..1.01x, grainy 0.95x.
        Family::Vp9 => (0.8, 1.05),
    }
}

/// What a `-tune`, a profile and a pixel format add at the top of the band (they never lower it),
/// measured at x264 / x265 CRF 23 on the three classes: grain 1.4x (x265 1.55x), zerolatency 1.5x,
/// fastdecode 1.3x, stillimage 1.2x, baseline profile 1.12x, 4:2:2 1.3x, and 10-bit 4:2:0 1.0x.
/// Soft pictures react much more (x265 grain 3.5x), which is below the high end and so not counted.
fn encode_extras(opts: &ExportOptions, family: Family) -> (f64, f64) {
    let mut high: f64 = 1.0;
    let mut low: f64 = 1.0;
    if matches!(family, Family::H264 | Family::Hevc) {
        match opts.tune.as_deref() {
            Some("grain") => high *= 1.6,
            Some("zerolatency") => high *= 1.5,
            Some("fastdecode") => high *= 1.3,
            Some("stillimage") => high *= 1.2,
            Some("animation") => low *= 0.8,
            _ => {}
        }
    }
    if family == Family::H264 && opts.profile_v.as_deref() == Some("baseline") {
        high *= 1.15;
    }
    if let Some(pf) = opts.pix_fmt.as_deref() {
        let wide = ["422", "444", "rgb", "bgr", "gbr", "yuva"].iter().any(|m| pf.contains(m));
        if wide {
            high *= 1.3;
        } else if pf.contains("10") || pf.contains("12") || pf == "p010le" {
            high *= 1.05;
        }
    }
    (low, high.min(2.0))
}

/// The picture's bytes (low, high) and how they were reached.
fn video_band(p: ExportedFrame, secs: f64, opts: &ExportOptions) -> Part<Band> {
    let pixels = f64::from(p.width.max(2)) * f64::from(p.height.max(2));
    let fps = sane_fps(p.fps);
    let frames = frame_count(fps, secs);
    let per_bpp = pixels * frames / 8.0;
    match picture_kind(opts)? {
        Picture::Gif => Ok(Band::new(
            GIF_BYTES_PER_PIXEL.0 * pixels * frames,
            GIF_BYTES_PER_PIXEL.1 * pixels * frames,
            SizeBasis::Modelled,
            "GIF size varies widely with the picture: flat colour and still frames are small, grain and gradients are not.",
        )),
        Picture::ProRes => {
            let (low, high) = PRORES_BPP[usize::from(opts.prores_profile.unwrap_or(3)).min(PRORES_BPP.len() - 1)];
            let low = low * (pixels / REF_PIXELS).powf(PRORES_SIZE_LOW);
            let high = (high * (REF_PIXELS / pixels).max(1.0).powf(PRORES_SIZE_HIGH)).min(PRORES_MAX_BPP);
            Ok(Band::new(
                low * per_bpp,
                high * per_bpp,
                SizeBasis::Modelled,
                "ProRes size is set by the profile and varies with how much detail the picture has.",
            ))
        }
        Picture::Coded(family, hardware) => {
            let spread = hardware_spread(hardware);
            let hw_note = if hardware == EncFamily::Software {
                ""
            } else {
                " A hardware encoder is less predictable than a software one, so the range is wider."
            };
            match opts.rate_control {
                RateControl::Bitrate | RateControl::TwoPass => {
                    let Some(bps) = bitrate_target(opts, hardware) else {
                        return Err(Cannot("There is no video bitrate to size by."));
                    };
                    let nominal = bps * secs / 8.0;
                    // A hardware encoder holds its target less closely than x264's rate control does.
                    let (low, high) = if hardware == EncFamily::Software {
                        (0.6, overshoot(secs))
                    } else {
                        (0.45, overshoot(secs) * 1.35)
                    };
                    let (basis, note) = if hardware == EncFamily::Software {
                        (
                            SizeBasis::Targeted,
                            "Size follows the target bitrate times the length; a simple picture can come out smaller and a very short cut larger.",
                        )
                    } else {
                        (
                            SizeBasis::Modelled,
                            "Size follows the target bitrate times the length; a hardware encoder holds it less closely than a software one, and a simple picture can come out smaller.",
                        )
                    };
                    Ok(Band::new(nominal * low, nominal * high, basis, note))
                }
                RateControl::Lossless => Ok(lossless_band(per_bpp, spread, hw_note)),
                RateControl::Crf => {
                    let crf = effective_crf(family, hardware, opts.crf);
                    // CRF 0 is lossless for every software encoder (SVT-AV1 gets `lossless=1` for it).
                    if crf <= 0.0 && hardware == EncFamily::Software {
                        return Ok(lossless_band(per_bpp, spread, hw_note));
                    }
                    Ok(crf_band(family, hardware, crf, pixels, fps, frames, opts, hw_note))
                }
            }
        }
    }
}

/// The lossless band: the picture costs what its content needs, bounded by raw video.
fn lossless_band(per_bpp: f64, spread: (f64, f64), hw_note: &'static str) -> Band {
    let note = if hw_note.is_empty() {
        "Lossless size depends heavily on the footage, and is very large."
    } else {
        "Lossless size depends heavily on the footage, and is very large. A hardware encoder is less predictable than a software one, so the range is wider."
    };
    Band::new(
        LOSSLESS_BPP.0 * per_bpp * spread.0.min(1.0),
        LOSSLESS_BPP.1 * per_bpp * spread.1.min(1.0),
        SizeBasis::Modelled,
        note,
    )
}

/// The constant-quality band at `crf`: the codec's curve, the preset, the picture's size and rate,
/// and whatever the options add at the top.
#[allow(clippy::too_many_arguments)]
fn crf_band(
    family: Family,
    hardware: EncFamily,
    crf: f64,
    pixels: f64,
    fps: f64,
    frames: f64,
    opts: &ExportOptions,
    hw_note: &'static str,
) -> Band {
    let (mut low, mut high) = curve_of(family, hardware).bpp(crf);
    let (preset_low, preset_high) = preset_factors(family, hardware, opts.preset.as_deref());
    let (extra_low, extra_high) = encode_extras(opts, family);
    let spread = hardware_spread(hardware);
    // Bigger frames cost fewer bits per pixel when the picture is soft (4K read 0.65x the 1080p
    // figure); smaller ones cost more, which is left out of the floor. Grainy pictures cost the
    // same per pixel at any size (0.98..1.03x from 360p to 4K).
    let size = (pixels / REF_PIXELS).max(1.0).powf(-0.35);
    // More frames per second share the same picture: 60 fps read 0.67x (soft) and 0.83x (grainy) of
    // 30 fps, 15 fps 1.4x and 1.2x. Held to the 12..120 fps that was measured.
    let rate = (fps.clamp(12.0, 120.0) / REF_FPS).max(f64::MIN_POSITIVE);
    low *= preset_low * extra_low * spread.0 * size * rate.powf(-0.6);
    high *= preset_high * extra_high * spread.1 * rate.powf(-0.3);
    let (low, high) = (low.max(MIN_BPP), high.min(MAX_BPP).max(low.max(MIN_BPP)));
    let per_bpp = pixels * frames / 8.0;
    Band::new(
        low * per_bpp,
        high * per_bpp,
        SizeBasis::Modelled,
        if hw_note.is_empty() {
            "Quality-based (CRF) size depends on how much detail and motion the footage has, so this is a range, not a prediction. A target bitrate gives a predictable size."
        } else {
            "Quality-based (CRF) size depends on how much detail and motion the footage has, so this is a range, not a prediction. A target bitrate gives a predictable size. A hardware encoder is less predictable than a software one, so the range is wider."
        },
    )
}

/// The video bit rate an export asks for, in bits per second, capped by a `-maxrate` the encoder
/// honours (x264 / x265 and NVENC). `None` without a readable `-b:v`.
fn bitrate_target(opts: &ExportOptions, hardware: EncFamily) -> Option<f64> {
    let target = bitrate_bps(opts.video_bitrate.as_deref()?)?;
    let cap = matches!(hardware, EncFamily::Software | EncFamily::Nvenc)
        .then(|| opts.max_rate.as_deref().and_then(bitrate_bps))
        .flatten()
        .filter(|cap| *cap < target);
    Some(cap.unwrap_or(target))
}

/// A bitrate token as `-b:v` reads it: a number with an optional `k` / `M` (1000 and 1 000 000).
pub(super) fn bitrate_bps(token: &str) -> Option<f64> {
    let s = token.trim();
    let (digits, scale) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1e3),
        'm' | 'M' => (&s[..s.len() - 1], 1e6),
        _ => (s, 1.0),
    };
    let n: f64 = digits.parse().ok()?;
    (n.is_finite() && n > 0.0).then_some(n * scale)
}

/// How far over its target a bitrate-targeted encode of a `secs` cut can land: the first keyframe
/// is spent before the average settles, so a short cut overshoots (x264 ABR on a grainy 1080p source,
/// 8 Mbps target: 1.71x at 5 s, 1.28x at 10 s, 1.10x at 20 s, 1.04x at 40 s; two-pass 1.6..2.6x at 3-5 s
/// and 1.0x from 10 s). Capped at 3x.
fn overshoot(secs: f64) -> f64 {
    (1.1 + (5.0 / secs.max(0.1)).powf(1.7)).min(3.0)
}

// ---- the sound ----------------------------------------------------------------------------------

/// What a lossy encoder writes for a stereo second of digital silence, less than the 4 kbps
/// (500 B/s) the whole file measured, headers and all (AAC and Opus, 128k and 320k alike).
const SILENT_BYTES_PER_SEC: f64 = 100.0;

/// The sound's bytes (low, high).
fn audio_band(s: SizeSound, secs: f64, opts: &ExportOptions) -> Part<Band> {
    let channels = f64::from(s.channels.clamp(1, 2));
    let rate = f64::from(s.sample_rate.max(1));
    // The encoder ffmpeg takes for a container when `-c:a` is not given (FFmpeg 9.0.2: AAC for mp4, mov
    // and m4a, Vorbis for mkv, Opus for webm).
    let codec = match opts.audio_codec.as_deref() {
        Some(c) => c,
        None => match opts.container {
            Container::Mp4 | Container::Mov | Container::M4a => "aac",
            Container::Mp3 => "libmp3lame",
            Container::Wav => "pcm_s16le",
            Container::Flac => "flac",
            Container::Mkv => "libvorbis",
            Container::Webm => "libopus",
            Container::Gif => return Err(Cannot("A GIF carries no sound.")),
        },
    };
    let bitrate = opts.audio_bitrate.as_deref().and_then(bitrate_bps);
    let pcm = |bytes_per_sample: f64| rate * channels * bytes_per_sample * secs;
    match codec {
        "pcm_s16le" => Ok(Band::new(pcm(2.0), pcm(2.0), SizeBasis::Exact, "")),
        "pcm_s24le" => Ok(Band::new(pcm(3.0), pcm(3.0), SizeBasis::Exact, "")),
        "libmp3lame" => {
            // Constant bitrate (default 128k stereo, 64k mono), snapped to one of MP3's own: measured
            // 128.4 and 321.0 kbps of the 128k and 320k asked, headers included.
            let target = bitrate.unwrap_or(64_000.0 * channels);
            let nominal = target * secs / 8.0;
            let standard = [
                8e3, 16e3, 24e3, 32e3, 40e3, 48e3, 64e3, 80e3, 96e3, 112e3, 128e3, 160e3, 192e3, 224e3, 256e3, 320e3,
            ]
            .contains(&target);
            let (low, high) = if standard { (0.99, 1.02) } else { (0.85, 1.2) };
            Ok(Band::new(
                nominal * low,
                nominal * high,
                if standard { SizeBasis::Exact } else { SizeBasis::Targeted },
                "",
            ))
        }
        "aac" => {
            // The native encoder lands on its bitrate (130 kbps of the 128k default, 233..284 of 320k:
            // it cannot spend more) and writes almost nothing for silence (4 kbps whatever the target,
            // headers included).
            let nominal = bitrate.unwrap_or(64_000.0 * channels) * secs / 8.0;
            Ok(Band::new(
                SILENT_BYTES_PER_SEC * secs,
                nominal * 1.08,
                SizeBasis::Targeted,
                "The sound lands near its bitrate; silent stretches come out smaller.",
            ))
        }
        "libopus" => {
            // Variable bitrate around the target: 0.5..1.4x of 128k on tones and noise, 4 kbps for silence;
            // the default (no `-b:a`) measured 121..139 kbps on tones and 67 on noise in stereo, 89 and 60
            // in mono, so it does not halve with the channels.
            let nominal = bitrate.unwrap_or(96_000.0) * secs / 8.0;
            Ok(Band::new(
                SILENT_BYTES_PER_SEC * secs,
                nominal * 1.6,
                SizeBasis::Targeted,
                "The sound varies around its bitrate; silent stretches come out smaller.",
            ))
        }
        "libvorbis" => {
            // FFmpeg's default for matroska: quality-driven, not a bitrate. Measured stereo 6 kbps
            // (silence), 30..35 (tones), 83 (noise).
            let nominal = 128_000.0 * secs / 8.0;
            Ok(Band::new(
                SILENT_BYTES_PER_SEC * secs,
                nominal * 1.1,
                SizeBasis::Modelled,
                "The sound's size varies with what it contains.",
            ))
        }
        "flac" | "alac" => {
            // Lossless: 16-bit PCM is the yardstick. Silence 0.002x (headers and padding included), a
            // tone 0.09x, a chord 0.56x, white noise 0.72x (FLAC) / 0.97x (ALAC); 24-bit full-scale noise
            // can cost more than 16-bit PCM.
            Ok(Band::new(
                pcm(2.0) * 0.001,
                pcm(2.0) * 1.7,
                SizeBasis::Modelled,
                "Lossless sound varies with what it contains: silence is tiny, dense music is most of the raw size.",
            ))
        }
        _ => Err(Cannot("This audio encoder is not one the size model knows.")),
    }
}

// ---- the container ------------------------------------------------------------------------------

/// What the container adds around the streams, `(low, high)`: fixed headers, an index and per-packet
/// framing that grows with the picture count and the sound's length. Headers and metadata (a title,
/// chapters, an embedded caption track) are all in the high end.
fn container_overhead(opts: &ExportOptions, frames: f64, sound_secs: f64) -> (f64, f64) {
    // (fixed low, fixed high, per picture, per second of sound)
    let (fixed_low, fixed_high, per_frame, per_sound_sec) = match opts.container {
        Container::Mp4 | Container::Mov => (1_000.0, 20_000.0, 16.0, 400.0),
        Container::Mkv | Container::Webm => (1_000.0, 24_000.0, 12.0, 200.0),
        Container::M4a => (1_000.0, 12_000.0, 0.0, 400.0),
        Container::Mp3 => (0.0, 4_000.0, 0.0, 0.0),
        Container::Wav => (44.0, 600.0, 0.0, 0.0),
        Container::Flac => (4_000.0, 20_000.0, 0.0, 0.0),
        Container::Gif => (0.0, 4_000.0, 0.0, 0.0),
    };
    let variable = per_frame * frames + per_sound_sec * sound_secs;
    let captions = if opts.embed_captions { 200_000.0 } else { 0.0 };
    (fixed_low + 0.5 * variable, fixed_high + 2.0 * variable + captions)
}
