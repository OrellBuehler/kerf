//! Domain model for a Kerf project: assets, cached analysis metadata, and the
//! non-destructive timeline (edit-decision-list).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::error::{Error, Result};

/// Kind of an elementary media stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Data,
}

/// How a video stream maps the world onto its frame. `Flat` is ordinary
/// rectilinear video; the rest describe 360 sources — a raw Insta360 `.insv` is
/// `DualFisheye` (two circular hemispheres side by side), a stitched Insta360
/// Studio export is `Equirect`. Drives the `v360` reprojection at export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Projection {
    Equirect,
    DualFisheye,
    Fisheye,
    Flat,
}

impl Projection {
    /// The `v360` `input=` / `output=` token naming this projection.
    pub fn v360_name(self) -> &'static str {
        match self {
            Projection::Equirect => "e",
            Projection::DualFisheye => "dfisheye",
            Projection::Fisheye => "fisheye",
            Projection::Flat => "flat",
        }
    }

    /// True when this projection covers the sphere, i.e. is worth reframing.
    pub fn is_spherical(self) -> bool {
        !matches!(self, Projection::Flat)
    }

    /// True when the source is lens-shaped, so the lens field of view
    /// (`ih_fov`/`iv_fov`) is meaningful on input.
    pub fn is_fisheye(self) -> bool {
        matches!(self, Projection::DualFisheye | Projection::Fisheye)
    }

    /// Parse the wire name (`equirect`, `dual_fisheye`, `fisheye`, `flat`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "equirect" => Some(Projection::Equirect),
            "dual_fisheye" => Some(Projection::DualFisheye),
            "fisheye" => Some(Projection::Fisheye),
            "flat" => Some(Projection::Flat),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Projection::Equirect => "equirect",
            Projection::DualFisheye => "dual_fisheye",
            Projection::Fisheye => "fisheye",
            Projection::Flat => "flat",
        }
    }
}

/// Structured description of a single stream inside an imported asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub index: u32,
    pub kind: StreamKind,
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u16>,
    /// True for a single-frame still image (PNG/JPEG/…): the stream has no real
    /// duration, so the engine loops it for the clip's length on export and never
    /// seeks into it. Defaulted (and omitted when false) so older `.kerf` JSON —
    /// which predates the flag — still deserializes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub image: bool,
    /// Spherical projection of a video stream, when the source is 360 footage.
    /// Detected at probe time from the file's spherical metadata or its geometry
    /// (see `engine::cli::detect_projection`); `None` for ordinary flat video.
    /// Defaulted (and omitted when unset) so older `.kerf` JSON still
    /// deserializes — this rides along in the `streams` JSON column, which is
    /// why 360 support needs no schema migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<Projection>,
    /// How far a player turns this video to show it upright, in degrees
    /// counter-clockwise as ffprobe's `Display Matrix` reports it (a portrait
    /// iPhone clip is a landscape sensor frame with 90 or -90 here). `width` /
    /// `height` are already the **displayed** size — every FFmpeg decode
    /// autorotates, so the pixels the engine sees are the rotated ones — and
    /// this only records that the file's coded frame is turned. Defaulted (and
    /// omitted at 0) so older `.kerf` JSON still deserializes.
    #[serde(default, skip_serializing_if = "is_zero_rotation")]
    pub rotation: i16,
    /// The video's transfer characteristic as ffprobe names it
    /// (`arib-std-b67` for HLG, `smpte2084` for PQ, `bt709`, …). Together with
    /// [`StreamInfo::hdr`] this is what decides whether the engine tone-maps the
    /// stream to SDR. Defaulted so older `.kerf` JSON (probed before it was
    /// read) still deserializes, as SDR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_transfer: Option<String>,
    /// The video's colour primaries as ffprobe names them (`bt2020`, `bt709`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_primaries: Option<String>,
    /// The pixel format as ffprobe names it (`yuv420p`, `yuva420p`, `rgba`, ...).
    /// What lets a renderer that draws only opaque 4:2:0 pictures tell, from the
    /// probe alone, that a source carries an alpha channel (see
    /// [`StreamInfo::has_alpha`]). `None` for an asset probed before it was
    /// recorded — a renderer must then find out from the pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pix_fmt: Option<String>,
    /// The YCbCr matrix the stream declares, as ffprobe names it (`bt709`,
    /// `smpte170m`, `bt470bg`, `bt2020nc`, ...); `None` when it declares none.
    /// A decode keeps the picture in its own YCbCr, so this only matters where
    /// the pipeline converts through RGB *inside* a graph — FFmpeg then uses
    /// the frame's matrix, and a BT.709 picture round-tripped as BT.601 loses
    /// its saturation to the RGB gamut clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_space: Option<String>,
}

fn is_zero_rotation(r: &i16) -> bool {
    *r == 0
}

/// The high-dynamic-range transfer functions the engine knows how to bring down
/// to SDR. Dolby Vision profile 8.4 (an iPhone's) carries an HLG-compatible base
/// layer, so it probes as `Hlg` and needs nothing of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hdr {
    /// Hybrid log-gamma (`arib-std-b67`) — what an iPhone records.
    Hlg,
    /// SMPTE ST 2084 perceptual quantizer (`smpte2084`) — HDR10.
    Pq,
}

impl Hdr {
    /// The zimg / `zscale` name of the transfer function.
    pub fn zscale_name(self) -> &'static str {
        match self {
            Hdr::Hlg => "arib-std-b67",
            Hdr::Pq => "smpte2084",
        }
    }
}

/// Whether an ffprobe pixel-format name carries an alpha channel. A name is
/// matched, not looked up in libavutil's table (this crate needs no dev
/// libraries): `yuva*` and `gbrap*` planar families, the packed `rgba` / `bgra` /
/// `argb` / `abgr` (and their 64-bit forms), `ayuv` / `vuya`, `ya8` / `ya16*`,
/// the `rgb32` / `bgr32` aliases, and `pal8`, whose palette may hold transparent
/// entries (a GIF) with nothing in the name to say so.
///
/// This is the deny-list; what a renderer that draws only opaque pictures should
/// trust is the allow-list, [`pix_fmt_layout`] — a format on neither is unknown.
pub fn pix_fmt_has_alpha(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("yuva")
        || n.starts_with("gbrap")
        || n.starts_with("ya8")
        || n.starts_with("ya16")
        || n.starts_with("ayuv")
        || n.starts_with("vuya")
        || n.starts_with("rgb32")
        || n.starts_with("bgr32")
        || n == "pal8"
        || ["rgba", "bgra", "argb", "abgr"].iter().any(|p| n.starts_with(p))
}

/// How a known-opaque picture is laid out, as far as FFmpeg's scaler cares: the
/// chroma it carries decides whether decoding to 8-bit 4:2:0 *before* scaling (what
/// a renderer that handles only 4:2:0 has to do) gives the picture FFmpeg's own
/// scaling of the native format would — it does not when the picture is enlarged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixLayout {
    /// 8- or 10-bit 4:2:0 (`yuv420p`, `yuvj420p`, `yuv420p10le`, `nv12`, `p010le`, ...):
    /// the layout the compositor works in.
    Yuv420,
    /// Luma only (`gray`, `gray10le`, ...): scaling touches one plane, and the
    /// 4:2:0 chroma is constant.
    Gray,
    /// Any other YCbCr layout (4:2:2, 4:4:4, 4:1:1, 4:4:0, 12 bit and up, packed).
    OtherYuv,
    /// An RGB-family picture (`rgb24`, `bgr0`, `gbrp`, ...): FFmpeg scales it as
    /// RGB, and converts to YCbCr only where the graph says so.
    Rgb,
}

/// The layout of a **known-opaque** pixel format, from an allow-list; `None` for
/// anything else — alpha-carrying, palettised, or simply not on the list. A
/// renderer that cannot draw alpha should refuse what is not here rather than
/// trust a deny-list to have thought of every name (`ayuv`, `vuya`, and the
/// `rgb32` aliases all slipped past one).
pub fn pix_fmt_layout(name: &str) -> Option<PixLayout> {
    let n = name.to_ascii_lowercase();
    // `yuv420p10le` -> `yuv420p10`.
    let t = n.strip_suffix("le").or_else(|| n.strip_suffix("be")).unwrap_or(&n);
    // The semi-planar high-depth names carry their depth in the name itself.
    match t {
        "p010" => return Some(PixLayout::Yuv420),
        "p012" | "p016" | "p210" | "p212" | "p216" | "p410" | "p412" | "p416" => return Some(PixLayout::OtherYuv),
        _ => {}
    }
    // Split a depth tail off a planar name: (`yuv420p`, 10). `rgb24` / `bgr24` /
    // `rgb48` keep their digits: they are part of the name, not a depth.
    let digits = t.chars().rev().take_while(char::is_ascii_digit).count();
    let (stem, tail) = t.split_at(t.len() - digits);
    let (base, bits) = match tail.parse::<u32>() {
        Ok(b) if stem.ends_with('p') || stem == "gray" => (stem, b),
        _ => (t, 8),
    };
    let planar_depth = matches!(bits, 8 | 9 | 10 | 12 | 14 | 16);
    match base {
        "yuv420p" | "yuvj420p" if bits <= 10 && planar_depth => Some(PixLayout::Yuv420),
        "yuv420p" if planar_depth => Some(PixLayout::OtherYuv),
        "nv12" | "nv21" => Some(PixLayout::Yuv420),
        "gray" if planar_depth => Some(PixLayout::Gray),
        "yuv422p" | "yuvj422p" | "yuv444p" | "yuvj444p" | "yuv440p" | "yuvj440p" | "yuv411p" | "yuvj411p" | "yuv410p"
            if planar_depth =>
        {
            Some(PixLayout::OtherYuv)
        }
        "nv16" | "nv24" | "nv42" | "uyvy422" | "yuyv422" | "yvyu422" | "uyyvyy411" => Some(PixLayout::OtherYuv),
        "gbrp" if planar_depth => Some(PixLayout::Rgb),
        "rgb24" | "bgr24" | "rgb0" | "bgr0" | "0rgb" | "0bgr" | "rgb48" | "bgr48" | "rgb8" | "bgr8" | "rgb4" | "bgr4"
        | "rgb4_byte" | "bgr4_byte" | "rgb565" | "bgr565" | "rgb555" | "bgr555" | "rgb444" | "bgr444" | "x2rgb10" | "x2bgr10" => {
            Some(PixLayout::Rgb)
        }
        _ => None,
    }
}

/// The chroma subsampling of a picture's **native** pixel format, as base-2 shifts —
/// the grid FFmpeg's `crop`, `pad` and Cover crop round a position to, because they
/// run on the picture as it is (the conversion to 4:2:0 comes after them in the
/// graph): even in both directions for 4:2:0, in width only for 4:2:2, and not at
/// all for 4:4:4, gray or RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subsampling {
    pub log2_w: u8,
    pub log2_h: u8,
}

impl Subsampling {
    /// 4:2:0 — what the compositor's planes are, and what most footage is.
    pub const YUV420: Self = Self { log2_w: 1, log2_h: 1 };
    /// No chroma subsampling (4:4:4, gray, RGB).
    pub const NONE: Self = Self { log2_w: 0, log2_h: 0 };
    /// Every grid a pixel format might have (4:1:1 and 4:1:0 included), for a
    /// picture whose format is not known well enough to say which it is.
    pub const ALL: [Self; 6] = [
        Self::NONE,
        Self { log2_w: 1, log2_h: 0 },
        Self { log2_w: 0, log2_h: 1 },
        Self::YUV420,
        Self { log2_w: 2, log2_h: 0 },
        Self { log2_w: 2, log2_h: 2 },
    ];

    /// `v` rounded down to a whole chroma column.
    pub fn round_w(self, v: i64) -> i64 {
        v & !((1i64 << self.log2_w) - 1)
    }

    /// `v` rounded down to a whole chroma row.
    pub fn round_h(self, v: i64) -> i64 {
        v & !((1i64 << self.log2_h) - 1)
    }
}

/// The native chroma grid of a pixel format — what the first `crop` rounds its
/// window to. It is a property of FFmpeg's pixel format descriptor, so the table is
/// by name: planar YCbCr and gray / planar RGB at 8 to 16 bits, the semi-planar
/// `nv*` / `p*` families, and byte-aligned packed RGB (`rgb24`, `bgr0`, ...). `None`
/// for everything else — bit-packed and palettised RGB, packed YCbCr, alpha formats,
/// a name nobody here has seen — where `crop` does more than round to a chroma grid,
/// or nobody checked. `the_subsampling_table_matches_ffmpegs_crop` (an `#[ignore]`d
/// test against the real binary) measures the table.
pub fn pix_fmt_subsampling(name: &str) -> Option<Subsampling> {
    let n = name.to_ascii_lowercase();
    let t = n.strip_suffix("le").or_else(|| n.strip_suffix("be")).unwrap_or(&n);
    // The semi-planar high-depth names carry their depth in the name itself.
    match t {
        "p010" | "p012" | "p016" => return Some(Subsampling::YUV420),
        "p210" | "p212" | "p216" => return Some(Subsampling { log2_w: 1, log2_h: 0 }),
        "p410" | "p412" | "p416" => return Some(Subsampling::NONE),
        _ => {}
    }
    let digits = t.chars().rev().take_while(char::is_ascii_digit).count();
    let (stem, tail) = t.split_at(t.len() - digits);
    let (base, bits) = match tail.parse::<u32>() {
        Ok(b) if stem.ends_with('p') || stem == "gray" => (stem, b),
        _ => (t, 8),
    };
    let planar_depth = matches!(bits, 8 | 9 | 10 | 12 | 14 | 16);
    match base {
        "yuv420p" | "yuvj420p" if planar_depth => Some(Subsampling::YUV420),
        "nv12" | "nv21" => Some(Subsampling::YUV420),
        "yuv422p" | "yuvj422p" if planar_depth => Some(Subsampling { log2_w: 1, log2_h: 0 }),
        "nv16" => Some(Subsampling { log2_w: 1, log2_h: 0 }),
        "yuv440p" | "yuvj440p" if planar_depth => Some(Subsampling { log2_w: 0, log2_h: 1 }),
        "yuv411p" | "yuvj411p" => Some(Subsampling { log2_w: 2, log2_h: 0 }),
        "yuv410p" => Some(Subsampling { log2_w: 2, log2_h: 2 }),
        "yuv444p" | "yuvj444p" | "gray" | "gbrp" if planar_depth => Some(Subsampling::NONE),
        "nv24" | "nv42" => Some(Subsampling::NONE),
        "rgb24" | "bgr24" | "rgb0" | "bgr0" | "0rgb" | "0bgr" | "rgb48" | "bgr48" => Some(Subsampling::NONE),
        _ => None,
    }
}

impl StreamInfo {
    /// Whether the picture has an alpha channel, when the probe recorded the
    /// pixel format: `Some(true)` for a format that carries one, `Some(false)` for
    /// a known-opaque one ([`pix_fmt_layout`]), and `None` when the format was
    /// never recorded (an asset saved before [`StreamInfo::pix_fmt`] existed) **or**
    /// is not one the allow-list knows. `None` is not "no alpha".
    pub fn has_alpha(&self) -> Option<bool> {
        let name = self.pix_fmt.as_deref()?;
        if pix_fmt_has_alpha(name) {
            Some(true)
        } else {
            pix_fmt_layout(name).map(|_| false)
        }
    }

    /// The HDR transfer this video stream is encoded in, or `None` for SDR and
    /// for anything that is not video.
    pub fn hdr(&self) -> Option<Hdr> {
        if self.kind != StreamKind::Video {
            return None;
        }
        match self.color_transfer.as_deref() {
            Some("arib-std-b67") => Some(Hdr::Hlg),
            Some("smpte2084") => Some(Hdr::Pq),
            _ => None,
        }
    }
}

/// An imported media file plus the structured metadata probed from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: Uuid,
    /// Absolute path on disk to the source media.
    pub path: String,
    pub name: String,
    /// Total duration in seconds.
    pub duration: f64,
    pub streams: Vec<StreamInfo>,
    pub imported_at: DateTime<Utc>,
    /// The original capture files this asset was derived from, when `path` is
    /// something Kerf produced at import rather than a file the user picked —
    /// today only an Insta360 lens pair stitched into one equirect video. Empty
    /// for an ordinary asset, whose `path` *is* its source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_paths: Vec<String>,
    /// The script and voice this audio was synthesized from, when Kerf generated
    /// it rather than the user importing it. `None` for ordinary media.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voiceover: Option<Voiceover>,
}

/// What a generated voiceover was made from — enough to regenerate it, and the
/// sentence timings the synthesizer measured.
///
/// The timings are why this rides on the asset rather than living only in its
/// analysis: they are exact (each sentence is synthesized on its own, so its
/// start and end are sample counts, not a speech model's estimate), and an
/// analysis pass would otherwise replace them with whisper's guess at audio it
/// could only get wrong. Analysis reads its transcript from here instead.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Voiceover {
    pub text: String,
    /// Kokoro voice id, e.g. `af_heart`.
    pub voice: String,
    /// Speaking rate multiplier; 1.0 is the voice's natural pace.
    pub speed: f64,
    /// One entry per sentence, in the audio's own (source) time.
    pub segments: Vec<TranscriptSegment>,
}

impl Asset {
    /// The dominant stream kind, used when auto-selecting a target track.
    pub fn primary_kind(&self) -> StreamKind {
        if self.streams.iter().any(|s| s.kind == StreamKind::Video) {
            StreamKind::Video
        } else if self.streams.iter().any(|s| s.kind == StreamKind::Audio) {
            StreamKind::Audio
        } else {
            StreamKind::Data
        }
    }

    pub fn has_audio(&self) -> bool {
        self.streams.iter().any(|s| s.kind == StreamKind::Audio)
    }

    /// True when this asset is a still image (a single-frame PNG/JPEG/…). Such an
    /// asset has no intrinsic duration, so it is placed on the timeline with a
    /// default length and looped — not seeked — on export.
    pub fn is_image(&self) -> bool {
        self.streams.iter().any(|s| s.image)
    }

    /// How far into this asset a clip's source window may reach (seconds): its
    /// duration, or infinity for a still, which loops and so never runs out. What
    /// the edit modes that slide a window over footage (roll, slip, slide) clamp to.
    pub fn source_limit(&self) -> f64 {
        if self.is_image() {
            f64::INFINITY
        } else {
            self.duration
        }
    }

    /// The spherical projection of this asset's video, if it is 360 footage.
    /// Clips cut from such an asset are reframed to flat by default.
    pub fn projection(&self) -> Option<Projection> {
        self.streams.iter().find_map(|s| s.projection).filter(|p| p.is_spherical())
    }

    /// The HDR transfer of this asset's video, if it is HDR footage that must be
    /// tone-mapped to SDR wherever it is decoded.
    pub fn hdr(&self) -> Option<Hdr> {
        self.streams.iter().find_map(|s| s.hdr())
    }

    /// Whether this asset's picture carries an alpha channel, by the probed pixel format of
    /// its first video stream. `false` for a format that was never recorded (assets saved
    /// before the field existed): nothing is assumed transparent that was never seen to be.
    pub fn has_alpha(&self) -> bool {
        self.streams
            .iter()
            .find(|s| s.kind == StreamKind::Video)
            .and_then(|s| s.pix_fmt.as_deref())
            .is_some_and(pix_fmt_has_alpha)
    }

    /// This asset as seen through its generated preview proxy: same metadata
    /// (so the composite geometry matches the export) but SDR, because the
    /// proxy was tone-mapped when it was encoded and must not be converted a
    /// second time.
    pub(crate) fn as_sdr_proxy(&self) -> Asset {
        let mut asset = self.clone();
        for s in asset.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
            // The proxy of HDR footage is tagged BT.709 when it is tone-mapped;
            // an SDR proxy keeps the original's matrix.
            if s.hdr().is_some() {
                s.color_space = Some("bt709".into());
            }
            s.color_transfer = None;
            s.color_primaries = None;
        }
        asset
    }
}

/// Default timeline length, in seconds, given to a still image on import (it has
/// no intrinsic duration). The clip can be trimmed like any other afterwards.
pub const DEFAULT_IMAGE_DURATION: f64 = 5.0;

/// A half-open time range `[start, end)` in seconds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

/// A transcript line with timecodes (seconds).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// EBU R128 loudness measurement of an asset's audio, from a single `loudnorm`
/// analysis pass. Lets an agent level a clip to a target or balance a voiceover
/// against a music bed instead of guessing at a linear gain.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Loudness {
    /// Integrated (program) loudness in LUFS.
    pub integrated_lufs: f64,
    /// Loudness range (LRA) in LU.
    pub loudness_range: f64,
    /// Maximum true peak in dBTP.
    pub true_peak_dbtp: f64,
    /// Gating threshold used for the measurement, in LUFS.
    pub threshold_lufs: f64,
}

/// The streaming-delivery loudness target, in LUFS — the level the export's
/// `loudnorm` normalises to, and what [`Levels`] judges a mix against.
pub const LEVELS_TARGET_LUFS: f64 = -14.0;
/// The true-peak ceiling platforms ask of a delivery, in dBTP.
pub const LEVELS_TRUE_PEAK_CEILING_DBTP: f64 = -1.0;
/// How much further than the overshoot the notes advise lowering a limiter's ceiling:
/// the peak between samples moves with the signal, so landing exactly on the line is
/// not landing under it.
const LEVELS_CEILING_MARGIN_DB: f64 = 0.5;

/// What one `ebur128` meter read over a stretch of the mix. Every number is
/// `None` where there is nothing to report — a silent stretch has no integrated
/// loudness and a peak of minus infinity, neither of which JSON can carry.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct LevelReading {
    /// Integrated (programme) loudness in LUFS, gated per EBU R128. `None` when
    /// nothing in the span was loud enough to count.
    pub integrated_lufs: Option<f64>,
    /// Loudness range in LU: how far the loud and quiet passages sit apart.
    pub loudness_range_lu: Option<f64>,
    /// The loudest short-term (3 s window) loudness reached, in LUFS. `None` for
    /// a span under three seconds, where the first full window never closes.
    pub short_term_max_lufs: Option<f64>,
    /// The highest sample, in dBFS.
    pub peak_dbfs: Option<f64>,
    /// The highest *true* (inter-sample) peak in dBTP — what a re-encode can
    /// clip on, and so what the master is judged on for delivery.
    pub true_peak_dbtp: Option<f64>,
}

/// One track's reading: the strip's output, after its fader and pan and ahead of
/// the duck bus and the master.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackLevels {
    pub track_id: Uuid,
    pub name: String,
    pub kind: StreamKind,
    /// The track is flagged to duck under the rest of the mix. Its reading is
    /// taken *before* the duck, so it is as loud as the track is on its own.
    pub ducked: bool,
    /// Whether the track reaches the render at all — `false` for a muted track,
    /// or one a solo shadows, which is why it has no `level`.
    pub heard: bool,
    pub level: Option<LevelReading>,
}

/// How loud the cut is: the finished mix and each track that feeds it, measured
/// in one pass over the audio the export would render.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Levels {
    /// Seconds of the cut measured (the range asked for, clamped to the cut).
    pub duration: f64,
    /// The finished mix — master fader, limiter and, when asked for, `loudnorm`
    /// included. `None` when the cut has no audio to measure.
    pub master: Option<LevelReading>,
    pub tracks: Vec<TrackLevels>,
    /// Whether the measurement ran through `loudnorm`, as an export with
    /// normalisation on would.
    pub loudnorm: bool,
    /// The streaming loudness target the notes judge against.
    pub target_lufs: f64,
    /// What the numbers mean for delivery, phrased as advice (over target,
    /// over the true-peak ceiling, a track clipping the sum).
    pub notes: Vec<String>,
}

impl Levels {
    /// Put a measurement together with the tracks it came from and judge it, given
    /// the master bus the mix went through (the advice for a hot peak depends on
    /// whether its limiter is already on).
    pub fn new(duration: f64, master: Option<LevelReading>, tracks: Vec<TrackLevels>, loudnorm: bool, bus: &MasterBus) -> Self {
        let notes = level_notes(master.as_ref(), &tracks, bus);
        Self {
            duration,
            master,
            tracks,
            loudnorm,
            target_lufs: LEVELS_TARGET_LUFS,
            notes,
        }
    }
}

/// The advice behind [`Levels::notes`], pure so the thresholds are tested.
fn level_notes(master: Option<&LevelReading>, tracks: &[TrackLevels], bus: &MasterBus) -> Vec<String> {
    let Some(master) = master else {
        return vec!["The cut has no audio to measure.".to_string()];
    };
    let mut notes = Vec::new();
    match master.integrated_lufs {
        None => notes.push("The mix is silent.".to_string()),
        Some(i) if i - LEVELS_TARGET_LUFS > 1.0 => notes.push(format!(
            "Integrated loudness {i:.1} LUFS is {:.1} LU over the {LEVELS_TARGET_LUFS:.0} LUFS streaming target — platforms \
             will turn it down. Lower the master or the loudest track, or export with loudnorm.",
            i - LEVELS_TARGET_LUFS
        )),
        Some(i) if LEVELS_TARGET_LUFS - i > 3.0 => notes.push(format!(
            "Integrated loudness {i:.1} LUFS is {:.1} LU under the {LEVELS_TARGET_LUFS:.0} LUFS streaming target — it will \
             sound quiet beside other posts. Raise the master or the tracks, or export with loudnorm.",
            LEVELS_TARGET_LUFS - i
        )),
        Some(i) => notes.push(format!(
            "Integrated loudness {i:.1} LUFS is close to the {LEVELS_TARGET_LUFS:.0} LUFS streaming target."
        )),
    }
    if let Some(tp) = master.true_peak_dbtp.filter(|tp| *tp > LEVELS_TRUE_PEAK_CEILING_DBTP) {
        let over = format!(
            "True peak {tp:.1} dBTP is over the {LEVELS_TRUE_PEAK_CEILING_DBTP:.0} dBTP platforms ask for and can clip when \
             re-encoded."
        );
        if bus.limiter {
            // The limiter holds the *sample* peak at its ceiling, so what is left over
            // is the peak between samples: take the ceiling down by that, and a margin.
            let ceiling = bus.safe_ceiling_db();
            let lower = (ceiling - (tp - LEVELS_TRUE_PEAK_CEILING_DBTP) - LEVELS_CEILING_MARGIN_DB).max(MASTER_MIN_CEILING_DB);
            if lower < ceiling {
                notes.push(format!(
                    "{over} The master limiter is already on, but it holds the sample peak at {ceiling:.1} dBFS and the peak \
                     between samples runs above that. Lower its ceiling to about {lower:.1} dBFS (set_master_limiter)."
                ));
            } else {
                notes.push(format!(
                    "{over} The master limiter is already on at its lowest ceiling. Lower the master or the loudest track."
                ));
            }
        } else {
            notes.push(format!(
                "{over} Turn on the master limiter (set_master_limiter) or lower the master."
            ));
        }
    }
    for track in tracks {
        if let Some(peak) = track.level.and_then(|l| l.peak_dbfs).filter(|p| *p > 0.0) {
            notes.push(format!(
                "Track {} peaks at {peak:+.1} dBFS before the master, over full scale. Nothing clips until the mix is written, \
                 so the master can still bring it under (its fader or limiter); otherwise lower this track's fader.",
                track.name
            ));
        }
    }
    notes
}

/// Coarse content class of an asset's audio. Heuristic (energy continuity +
/// zero-crossing-rate variability), so it is a hint, not a trained classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioClass {
    /// Predominantly spoken word (gappy energy, variable ZCR).
    Speech,
    /// Predominantly music (continuous energy, steady ZCR).
    Music,
    /// Both present (e.g. dialogue over a music bed).
    Mixed,
    /// Could not be determined.
    Unknown,
}

/// An [`AudioClass`] verdict with a confidence in 0.0–1.0.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AudioClassification {
    pub class: AudioClass,
    pub confidence: f64,
}

/// Estimated tempo and beat grid for an asset's audio. Best-effort: derived by
/// autocorrelating the onset envelope, so it is most reliable on percussive
/// music and may land on a tempo octave — gate on `confidence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tempo {
    /// Estimated tempo in beats per minute.
    pub bpm: f64,
    /// Beat timestamps in seconds across the asset.
    pub beats: Vec<f64>,
    /// How periodic the audio is, 0.0–1.0 (the normalized autocorrelation peak).
    pub confidence: f64,
}

/// A fixed beat grid fitted to a piece of music: beat `k` is at `phase_s + k * period_s`,
/// and the beats `downbeat_offset`, `downbeat_offset + beats_per_bar`, … start bars.
/// `phase_s` is the first beat: it lies in `[-0.02, period_s - 0.02)`, so a beat on the
/// file's first sample that the fit places a hair early is still the first beat.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BeatGrid {
    pub period_s: f64,
    pub phase_s: f64,
    pub downbeat_offset: u32,
    pub beats_per_bar: u32,
}

impl BeatGrid {
    pub fn bpm(&self) -> f64 {
        60.0 / self.period_s
    }

    pub fn bar_s(&self) -> f64 {
        self.period_s * self.beats_per_bar as f64
    }

    /// The first downbeat (at most a few ms before 0).
    pub fn first_downbeat(&self) -> f64 {
        self.phase_s + self.downbeat_offset as f64 * self.period_s
    }

    /// Where bar `k` starts (counted from the first downbeat).
    pub fn bar_start(&self, k: usize) -> f64 {
        self.first_downbeat() + k as f64 * self.bar_s()
    }

    /// How many whole bars fit between the first downbeat and `duration`.
    pub fn whole_bars(&self, duration: f64) -> usize {
        let span = duration - self.first_downbeat();
        if span <= 0.0 || self.bar_s() <= 0.0 {
            return 0;
        }
        // A bar that ends within a microsecond of the end still counts as whole.
        ((span + 1e-6) / self.bar_s()).floor() as usize
    }

    /// Every beat in `[0, duration)`.
    pub fn beats(&self, duration: f64) -> Vec<f64> {
        if self.period_s <= 0.0 {
            return Vec::new();
        }
        (0..)
            .map(|k| self.phase_s + k as f64 * self.period_s)
            .take_while(|t| *t < duration)
            .collect()
    }

    /// Every downbeat in `[0, duration)`.
    pub fn downbeats(&self, duration: f64) -> Vec<f64> {
        if self.bar_s() <= 0.0 {
            return Vec::new();
        }
        (0..).map(|k| self.bar_start(k)).take_while(|t| *t < duration).collect()
    }
}

/// Two phrases of `bars` bars, starting at bars `a` and `b` (counted from the first
/// downbeat), whose bars all match in harmony — jumping from one to the other is a
/// splice the ear does not hear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhraseMatch {
    pub a: usize,
    pub b: usize,
    pub bars: usize,
}

/// The bar-level structure of a piece of music: its beat grid, the harmony of each
/// whole bar and the repeating phrases between them. What "fit music to length"
/// plans its splices from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MusicAnalysis {
    pub grid: BeatGrid,
    /// Length of the analysed audio in seconds.
    pub duration: f64,
    /// L2-normalized 12-bin chroma (C = 0 … B = 11, A4 = 440 Hz) of every whole bar,
    /// from the first downbeat. All zeros for a silent bar.
    pub bar_chroma: Vec<[f32; 12]>,
    /// Repeating 8- and 4-bar phrases (`a < b`), the legal splice points.
    pub phrases: Vec<PhraseMatch>,
}

/// Everything the rhythm analysis pass derives from one decoded PCM stream:
/// onsets, tempo and the speech/music class. Bundled because the three share
/// the decode (and onsets/tempo the onset envelope) — computing them together
/// costs one full-file ffmpeg decode instead of three.
#[derive(Debug, Clone, Default)]
pub struct Rhythm {
    pub onsets: Vec<f64>,
    pub tempo: Option<Tempo>,
    pub audio_class: Option<AudioClassification>,
    pub music: Option<MusicAnalysis>,
}

/// One kind of analysis an asset can have: each is a step of its own, can be run alone
/// and is cached independently (see [`AssetAnalysis::ran`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisKind {
    /// Silent spans (`silence_segments`).
    Silence,
    /// Scene changes (`scene_changes`).
    Scenes,
    /// EBU R128 loudness (`loudness`).
    Loudness,
    /// Onsets, tempo and the speech/music class (`onsets`, `tempo`, `audio_class`).
    Rhythm,
    /// Speech-to-text (`transcript`).
    #[serde(alias = "transcription", alias = "transcribe")]
    Transcript,
}

impl AnalysisKind {
    /// Every kind, in the order a pass runs them: the cheap ones the timeline draws
    /// first, transcription — by far the slowest — last.
    pub const ALL: [AnalysisKind; 5] = [
        AnalysisKind::Silence,
        AnalysisKind::Scenes,
        AnalysisKind::Loudness,
        AnalysisKind::Rhythm,
        AnalysisKind::Transcript,
    ];

    /// Whether the step reads the audio: everything but scene detection. A picture with no
    /// sound has none of it to find — and nothing to fail on.
    pub fn needs_audio(self) -> bool {
        self != AnalysisKind::Scenes
    }

    /// The wire name (`silence`, `scenes`, `loudness`, `rhythm`, `transcript`).
    pub fn name(self) -> &'static str {
        match self {
            AnalysisKind::Silence => "silence",
            AnalysisKind::Scenes => "scenes",
            AnalysisKind::Loudness => "loudness",
            AnalysisKind::Rhythm => "rhythm",
            AnalysisKind::Transcript => "transcript",
        }
    }

    /// The kind a name (or an alias an agent is likely to try) means.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "silence" | "silences" => Some(AnalysisKind::Silence),
            "scenes" | "scene" | "scene_changes" => Some(AnalysisKind::Scenes),
            "loudness" | "lufs" => Some(AnalysisKind::Loudness),
            "rhythm" | "tempo" | "beat" | "beats" | "onsets" => Some(AnalysisKind::Rhythm),
            "transcript" | "transcription" | "transcribe" | "speech" => Some(AnalysisKind::Transcript),
            _ => None,
        }
    }

    /// A list of names (`all` meaning every kind) as kinds, in pass order and without
    /// repeats. An empty list is an error: asking for no steps is a mistake, not a no-op.
    pub fn parse_list<S: AsRef<str>>(names: &[S]) -> Result<Vec<AnalysisKind>> {
        let mut wanted = std::collections::HashSet::new();
        for name in names {
            let name = name.as_ref();
            if name.trim().eq_ignore_ascii_case("all") {
                wanted.extend(AnalysisKind::ALL);
            } else {
                wanted.insert(AnalysisKind::parse(name).ok_or_else(|| {
                    Error::InvalidArgument(format!(
                        "unknown analysis step `{name}`; expected any of silence, scenes, loudness, rhythm, transcript, or all"
                    ))
                })?);
            }
        }
        if wanted.is_empty() {
            return Err(Error::InvalidArgument(
                "no analysis steps were named; pass some of silence, scenes, loudness, rhythm, transcript, or all".to_string(),
            ));
        }
        Ok(AnalysisKind::ALL.into_iter().filter(|k| wanted.contains(k)).collect())
    }
}

/// Cached, pluggable analysis results for an asset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssetAnalysis {
    pub asset_id: Uuid,
    #[serde(default)]
    pub silence_segments: Vec<TimeRange>,
    #[serde(default)]
    pub scene_changes: Vec<f64>,
    #[serde(default)]
    pub transcript: Vec<TranscriptSegment>,
    /// EBU R128 loudness of the asset's audio, when it has any. `None` until the
    /// asset is analyzed (and for silent / video-only assets).
    #[serde(default)]
    pub loudness: Option<Loudness>,
    /// Onset (transient) timestamps in seconds — moments where new sound energy
    /// arrives. Snap cut points to these to land edits on the beat.
    #[serde(default)]
    pub onsets: Vec<f64>,
    /// Estimated tempo and beat grid, when the audio is rhythmic enough. `None`
    /// for silent / video-only assets and non-rhythmic material.
    #[serde(default)]
    pub tempo: Option<Tempo>,
    /// Coarse speech/music classification of the audio. `None` for silent /
    /// video-only assets. Route ducking/leveling decisions off this.
    #[serde(default)]
    pub audio_class: Option<AudioClassification>,
    /// Bar-level structure (fitted beat grid, chroma, repeating phrases), when the
    /// audio has a steady enough pulse to fit one.
    #[serde(default)]
    pub music: Option<MusicAnalysis>,
    /// The kinds whose step has run to completion — the only way to tell "ran and
    /// found nothing" (no silence, no speech) from "never ran", both of which leave the
    /// data empty. Absent in an analysis cached before kinds were recorded: for those
    /// a kind counts as done when it has data ([`AssetAnalysis::done`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ran: Vec<AnalysisKind>,
}

impl AssetAnalysis {
    /// Whether `kind` has data in this analysis, whatever recorded it.
    fn has_data(&self, kind: AnalysisKind) -> bool {
        match kind {
            AnalysisKind::Silence => !self.silence_segments.is_empty(),
            AnalysisKind::Scenes => !self.scene_changes.is_empty(),
            AnalysisKind::Loudness => self.loudness.is_some(),
            AnalysisKind::Rhythm => !self.onsets.is_empty() || self.tempo.is_some() || self.audio_class.is_some(),
            AnalysisKind::Transcript => !self.transcript.is_empty(),
        }
    }

    /// Whether `kind` is done: its step ran (recorded in [`AssetAnalysis::ran`]), or —
    /// for an analysis from before that was recorded — it holds data of that kind.
    pub fn done(&self, kind: AnalysisKind) -> bool {
        self.ran.contains(&kind) || self.has_data(kind)
    }

    /// The kinds that are done, in pass order.
    pub fn done_kinds(&self) -> Vec<AnalysisKind> {
        AnalysisKind::ALL.into_iter().filter(|k| self.done(*k)).collect()
    }

    /// Fold `patch` — the results of the steps just run, with those steps in its
    /// `ran` — into this analysis, replacing the data of those kinds and leaving every
    /// other kind exactly as it was.
    pub fn merge(&mut self, patch: &AssetAnalysis) {
        for kind in AnalysisKind::ALL {
            if !patch.ran.contains(&kind) {
                continue;
            }
            match kind {
                AnalysisKind::Silence => self.silence_segments = patch.silence_segments.clone(),
                AnalysisKind::Scenes => self.scene_changes = patch.scene_changes.clone(),
                AnalysisKind::Loudness => self.loudness = patch.loudness,
                AnalysisKind::Rhythm => {
                    self.onsets = patch.onsets.clone();
                    self.tempo = patch.tempo.clone();
                    self.audio_class = patch.audio_class;
                    self.music = patch.music.clone();
                }
                AnalysisKind::Transcript => self.transcript = patch.transcript.clone(),
            }
            // The legacy kinds an old analysis counted by their data are recorded now,
            // so a step that finds nothing this time does not make it look unrun.
            if !self.ran.contains(&kind) {
                self.ran.push(kind);
            }
        }
        self.ran.sort();
    }
}

fn one() -> f64 {
    1.0
}

/// Per-clip geometric transform applied when compositing at export. A default
/// transform is the identity (full-frame, centered, opaque, uncropped).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Uniform scale multiplier applied after the clip is fit to the frame
    /// (1.0 = fit). Values < 1.0 shrink the picture for picture-in-picture.
    #[serde(default = "one")]
    pub scale: f64,
    /// Horizontal offset as a fraction of the frame width (0.0 = centered).
    #[serde(default)]
    pub pos_x: f64,
    /// Vertical offset as a fraction of the frame height (0.0 = centered).
    #[serde(default)]
    pub pos_y: f64,
    /// Clockwise rotation in degrees.
    #[serde(default)]
    pub rotation: f64,
    /// Opacity in 0.0–1.0 (1.0 = fully opaque).
    #[serde(default = "one")]
    pub opacity: f64,
    /// Fraction of the source cropped from each edge (0.0 = no crop).
    #[serde(default)]
    pub crop_left: f64,
    #[serde(default)]
    pub crop_right: f64,
    #[serde(default)]
    pub crop_top: f64,
    #[serde(default)]
    pub crop_bottom: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            crop_left: 0.0,
            crop_right: 0.0,
            crop_top: 0.0,
            crop_bottom: 0.0,
        }
    }
}

impl Transform {
    /// True when the transform leaves the picture untouched (full-frame fit).
    pub fn is_identity(&self) -> bool {
        *self == Transform::default()
    }

    /// True when compositing this clip needs an alpha channel (rotation leaves
    /// transparent corners; opacity blends; both require alpha).
    pub fn needs_alpha(&self) -> bool {
        self.opacity < 1.0 || self.rotation != 0.0
    }

    /// True when any edge crop is requested.
    pub fn has_crop(&self) -> bool {
        self.crop_left > 0.0 || self.crop_right > 0.0 || self.crop_top > 0.0 || self.crop_bottom > 0.0
    }
}

/// Per-clip color correction applied at export via the `eq` filter. A default
/// is the identity (no change).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Color {
    /// Additive brightness in -1.0–1.0 (0.0 = unchanged).
    #[serde(default)]
    pub brightness: f64,
    /// Contrast multiplier (1.0 = unchanged).
    #[serde(default = "one")]
    pub contrast: f64,
    /// Saturation multiplier (1.0 = unchanged).
    #[serde(default = "one")]
    pub saturation: f64,
    /// Gamma (1.0 = unchanged).
    #[serde(default = "one")]
    pub gamma: f64,
    /// Warm/cool shift in -1.0–1.0 (0.0 = unchanged): positive warms the
    /// picture (lifts red, lowers blue), negative cools it. Rendered as
    /// opposing per-channel gammas — what makes one-click warm / cool looks
    /// possible, since plain saturation/gamma can't tint.
    #[serde(default)]
    pub temperature: f64,
}

impl Default for Color {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            gamma: 1.0,
            temperature: 0.0,
        }
    }
}

impl Color {
    /// True when the color correction leaves the picture untouched.
    pub fn is_identity(&self) -> bool {
        *self == Color::default()
    }

    /// The `(gamma_r, gamma_b)` the `eq` filter is given for the warm / cool
    /// shift, or `None` at 0 (so a temperature-free clip's graph is unchanged).
    ///
    /// `eq` has no white-balance knob, but opposing per-channel gammas warm / cool
    /// convincingly: ±1.0 maps to a ±30% split. Kept here, not in the graph
    /// builder, because every renderer of a [`Color`] — the FFmpeg `eq` filter
    /// and the GPU compositor's port of it — has to start from the same two
    /// numbers.
    pub fn temperature_gammas(&self) -> Option<(f64, f64)> {
        (self.temperature != 0.0).then(|| {
            let t = self.temperature.clamp(-1.0, 1.0);
            (1.0 + 0.3 * t, 1.0 - 0.3 * t)
        })
    }
}

/// How a clip blends with the preceding clip on its track.
///
/// Three families, and the family is what decides how the cut is rendered:
/// a **dip** takes both sides through a solid colour, a **dissolve** mixes them,
/// and a **motion** transition slides the incoming clip in over the outgoing one
/// (`Slide*`) or shoves the outgoing one out of frame with it (`Push*`). All of
/// them borrow the outgoing clip's unused source handle to keep it playing under
/// the transition, so a cut with no handle left degrades to a hard cut rather
/// than to a fade from black.
///
/// The direction in a motion transition names the direction of **travel**, the
/// way an editor says it: `SlideLeft` brings the new shot in from the right edge
/// and moves it left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// Dissolve: the incoming clip fades up over the outgoing clip's tail.
    Crossfade,
    /// Dip to black: the outgoing clip fades to black, the incoming up from it.
    DipToBlack,
    /// Dip to white — the same shape as [`Self::DipToBlack`], through white.
    /// Reads as a brighter, faster beat than black, which is why a montage of
    /// daylight footage usually wants it instead.
    DipToWhite,
    /// The incoming clip travels in from the right edge over the held outgoing one.
    SlideLeft,
    /// The incoming clip travels in from the left edge.
    SlideRight,
    /// The incoming clip travels up from the bottom edge.
    SlideUp,
    /// The incoming clip travels down from the top edge.
    SlideDown,
    /// Both clips travel left: the incoming pushes the outgoing out of frame.
    PushLeft,
    /// Both clips travel right.
    PushRight,
    /// Both clips travel up.
    PushUp,
    /// Both clips travel down.
    PushDown,
}

impl TransitionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TransitionKind::Crossfade => "crossfade",
            TransitionKind::DipToBlack => "dip_to_black",
            TransitionKind::DipToWhite => "dip_to_white",
            TransitionKind::SlideLeft => "slide_left",
            TransitionKind::SlideRight => "slide_right",
            TransitionKind::SlideUp => "slide_up",
            TransitionKind::SlideDown => "slide_down",
            TransitionKind::PushLeft => "push_left",
            TransitionKind::PushRight => "push_right",
            TransitionKind::PushUp => "push_up",
            TransitionKind::PushDown => "push_down",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "crossfade" => Some(TransitionKind::Crossfade),
            "dip_to_black" | "diptoblack" => Some(TransitionKind::DipToBlack),
            "dip_to_white" | "diptowhite" => Some(TransitionKind::DipToWhite),
            "slide_left" => Some(TransitionKind::SlideLeft),
            "slide_right" => Some(TransitionKind::SlideRight),
            "slide_up" => Some(TransitionKind::SlideUp),
            "slide_down" => Some(TransitionKind::SlideDown),
            "push_left" => Some(TransitionKind::PushLeft),
            "push_right" => Some(TransitionKind::PushRight),
            "push_up" => Some(TransitionKind::PushUp),
            "push_down" => Some(TransitionKind::PushDown),
            _ => None,
        }
    }

    /// Every kind, in the order a picker should offer them.
    pub const ALL: [TransitionKind; 11] = [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::DipToWhite,
        TransitionKind::SlideLeft,
        TransitionKind::SlideRight,
        TransitionKind::SlideUp,
        TransitionKind::SlideDown,
        TransitionKind::PushLeft,
        TransitionKind::PushRight,
        TransitionKind::PushUp,
        TransitionKind::PushDown,
    ];

    /// Every kind's wire name, quoted and comma-joined — so an error message
    /// listing what was expected cannot drift from the enum.
    pub fn wire_names() -> String {
        Self::ALL
            .iter()
            .map(|k| format!("\"{}\"", k.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The solid colour this transition dips through, if it is a dip.
    pub fn dip_color(self) -> Option<&'static str> {
        match self {
            TransitionKind::DipToBlack => Some("black"),
            TransitionKind::DipToWhite => Some("white"),
            _ => None,
        }
    }

    /// Where the incoming clip starts, as an offset from its final position in
    /// frame widths and heights, for a motion transition. It travels from here
    /// to `(0, 0)` over the transition, so the vector points back along the
    /// direction of travel: a `SlideLeft` starts one full frame to the right.
    pub fn slide_from(self) -> Option<(f64, f64)> {
        match self {
            TransitionKind::SlideLeft | TransitionKind::PushLeft => Some((1.0, 0.0)),
            TransitionKind::SlideRight | TransitionKind::PushRight => Some((-1.0, 0.0)),
            TransitionKind::SlideUp | TransitionKind::PushUp => Some((0.0, 1.0)),
            TransitionKind::SlideDown | TransitionKind::PushDown => Some((0.0, -1.0)),
            _ => None,
        }
    }

    /// True when the outgoing clip is carried out of frame by the incoming one
    /// instead of being covered where it stands.
    pub fn pushes(self) -> bool {
        matches!(
            self,
            TransitionKind::PushLeft | TransitionKind::PushRight | TransitionKind::PushUp | TransitionKind::PushDown
        )
    }

    /// True when both sides play at once — a dissolve or any motion transition.
    /// Such a transition needs the outgoing clip's source handle; a dip does not,
    /// because the two halves happen either side of the cut.
    pub fn overlaps(self) -> bool {
        !matches!(self, TransitionKind::DipToBlack | TransitionKind::DipToWhite)
    }

    /// Human name, for a diff line or a picker label.
    pub fn label(self) -> &'static str {
        match self {
            TransitionKind::Crossfade => "Crossfade",
            TransitionKind::DipToBlack => "Dip to black",
            TransitionKind::DipToWhite => "Dip to white",
            TransitionKind::SlideLeft => "Slide left",
            TransitionKind::SlideRight => "Slide right",
            TransitionKind::SlideUp => "Slide up",
            TransitionKind::SlideDown => "Slide down",
            TransitionKind::PushLeft => "Push left",
            TransitionKind::PushRight => "Push right",
            TransitionKind::PushUp => "Push up",
            TransitionKind::PushDown => "Push down",
        }
    }
}

/// A transition blending the **start** of a clip with the clip that precedes it
/// on the same track. Realized at export.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub kind: TransitionKind,
    /// Duration of the transition in seconds.
    pub duration: f64,
}

/// The outline of a [`Mask`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaskShape {
    /// An axis-aligned rectangle — a sign, a screen, a lower band of the frame.
    #[default]
    Rect,
    /// An ellipse — a face, a spotlight.
    Ellipse,
}

impl MaskShape {
    pub fn as_str(self) -> &'static str {
        match self {
            MaskShape::Rect => "rect",
            MaskShape::Ellipse => "ellipse",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "rect" | "rectangle" => Some(MaskShape::Rect),
            "ellipse" | "circle" | "oval" => Some(MaskShape::Ellipse),
            _ => None,
        }
    }
}

/// A shape cut out of a clip: inside the shape the clip is kept, outside it goes
/// transparent (or the other way round, `inverted`). Everything is a **fraction
/// of the clip's rendered frame**, so a mask does not have to be redone when the
/// delivery frame changes.
///
/// Deliberately one primitive rather than a masking *mode* per use. A mask makes
/// a clip see-through, and the timeline already stacks tracks — so blurring a
/// face is a copy of the shot on the track above, blurred, masked to the face;
/// a picture-in-picture vignette is a mask on the upper clip; a region grade is
/// a masked copy with its own colour. One thing to learn, and it composes with
/// what is already there instead of adding a second compositor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Mask {
    #[serde(default)]
    pub shape: MaskShape,
    /// Centre of the shape, as a fraction of the frame. 0.5, 0.5 is the middle.
    #[serde(default = "half")]
    pub x: f64,
    #[serde(default = "half")]
    pub y: f64,
    /// Size of the shape as a fraction of the frame (its full width / height,
    /// not a radius).
    #[serde(default = "half")]
    pub width: f64,
    #[serde(default = "half")]
    pub height: f64,
    /// Softness of the edge, as a fraction of the shape's own half-size. 0 is a
    /// hard cut — which on a face reads as a sticker, so the default is soft.
    #[serde(default = "default_feather")]
    pub feather: f64,
    /// Keep what is *outside* the shape instead of inside it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inverted: bool,
}

fn default_feather() -> f64 {
    0.15
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            shape: MaskShape::default(),
            x: 0.5,
            y: 0.5,
            width: 0.5,
            height: 0.5,
            feather: default_feather(),
            inverted: false,
        }
    }
}

impl Mask {
    /// The mask with every field clamped into range: sizes to a visible minimum,
    /// the centre to the frame, feather to 0..1. A zero-width mask would blank
    /// the clip entirely, which is never what was meant.
    pub fn normalized(self) -> Self {
        Self {
            shape: self.shape,
            x: clamp01(self.x),
            y: clamp01(self.y),
            width: self.width.clamp(0.01, 2.0),
            height: self.height.clamp(0.01, 2.0),
            feather: clamp01(self.feather),
            inverted: self.inverted,
        }
    }
}

fn clamp01(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

/// A per-clip video effect, realized as a filter inserted into the clip's video
/// chain at export (after color correction). The order in `Clip::effects` is the
/// order they are applied. `ChromaKey` is the one effect that establishes an
/// alpha channel, so the clip composites with transparency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VideoEffect {
    /// Gaussian blur (`gblur`); larger `sigma` = softer.
    Blur { sigma: f64 },
    /// Unsharp-mask sharpen; `amount` is the luma strength.
    Sharpen { amount: f64 },
    /// Desaturate to grayscale.
    Grayscale,
    /// Invert colors (negative).
    Invert,
    /// Darken the frame edges.
    Vignette,
    /// Key out a color to transparency (green/blue screen). `color` is any ffmpeg
    /// color (e.g. `green`, `0x00ff00`); `similarity`/`blend` in 0.0–1.0.
    ChromaKey { color: String, similarity: f64, blend: f64 },
}

impl VideoEffect {
    /// Short name, for listing a chain in a diff or a log line.
    pub fn name(&self) -> &'static str {
        match self {
            VideoEffect::Blur { .. } => "blur",
            VideoEffect::Sharpen { .. } => "sharpen",
            VideoEffect::Grayscale => "grayscale",
            VideoEffect::Invert => "invert",
            VideoEffect::Vignette => "vignette",
            VideoEffect::ChromaKey { .. } => "chroma key",
        }
    }

    /// True when applying this effect leaves the frame with an alpha channel.
    pub fn produces_alpha(&self) -> bool {
        matches!(self, VideoEffect::ChromaKey { .. })
    }
}

/// A per-clip audio effect, realized as a filter inserted into the clip's audio
/// chain at export (after the clip gain). The order in `Clip::audio` is the order
/// they are applied. Thresholds/gains are in dB at the model boundary and
/// converted to the linear units ffmpeg's dynamics filters want by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AudioEffect {
    /// High-pass: attenuate below `hz` (cut rumble / handling noise).
    Highpass { hz: f64 },
    /// Low-pass: attenuate above `hz` (cut hiss).
    Lowpass { hz: f64 },
    /// Single parametric EQ band at `hz`, `width` Hz wide, `gain_db` boost/cut.
    Equalizer { hz: f64, width: f64, gain_db: f64 },
    /// Dynamic-range compressor.
    Compressor {
        threshold_db: f64,
        ratio: f64,
        attack_ms: f64,
        release_ms: f64,
        makeup_db: f64,
    },
    /// Noise gate: silence audio below `threshold_db`.
    Gate { threshold_db: f64 },
}

impl AudioEffect {
    /// Whether the effect reacts to *level* (a compressor, a gate): a gain put ahead of
    /// it changes what it does, so the clip's level cannot be moved across it. Filters
    /// and EQ are linear and commute with any gain.
    pub fn is_dynamic(&self) -> bool {
        matches!(self, AudioEffect::Compressor { .. } | AudioEffect::Gate { .. })
    }

    /// Short name, for listing a chain in a diff or a log line.
    pub fn name(&self) -> &'static str {
        match self {
            AudioEffect::Highpass { .. } => "highpass",
            AudioEffect::Lowpass { .. } => "lowpass",
            AudioEffect::Equalizer { .. } => "EQ",
            AudioEffect::Compressor { .. } => "compressor",
            AudioEffect::Gate { .. } => "gate",
        }
    }
}

fn half() -> f64 {
    0.5
}
fn lower_third_y() -> f64 {
    0.82
}
fn default_text_size() -> f64 {
    0.06
}
fn default_text_color() -> String {
    "white".to_string()
}

/// How a keyframe's value travels to the next one: the shape of the **outgoing** segment.
///
/// Every renderer reads the same curve because the curve *is* a polyline: a non-linear
/// segment is [`EASE_STEPS`] straight pieces through points of the true curve
/// ([`eased_points`]), which is what the still / preview path interpolates
/// ([`Clip::transform_at`]), what the export writes into its per-frame expressions
/// (`keyframe_expr`, which only knows straight lines) and what a GPU pass will sample — so
/// they agree exactly rather than approximately, and slicing an eased segment
/// ([`Timeline::slice`], a trimmed head) is exact too: its pieces become plain keys.
/// `Hold` keeps the value until the next key and jumps there. `Linear` is the default and is
/// omitted from the JSON, so every existing project and graph is byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    Hold,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// A CSS-style cubic bezier from (0, 0) to (1, 1). Both control points are held to the unit
    /// square: no overshoot, so an eased value never leaves the range of its two keys (and the
    /// engine's tiny-scale and opacity guards, which look at the keys, stay true).
    Bezier {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    },
}

/// Straight pieces an eased segment is drawn with (see [`Easing`]).
pub const EASE_STEPS: usize = 12;

impl Easing {
    pub fn is_linear(&self) -> bool {
        matches!(self, Easing::Linear)
    }

    /// The control points of a curved easing (`None` for `Linear` and `Hold`).
    fn bezier(&self) -> Option<(f64, f64, f64, f64)> {
        let unit = |v: f64| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        match *self {
            Easing::Linear | Easing::Hold => None,
            Easing::EaseIn => Some((0.42, 0.0, 1.0, 1.0)),
            Easing::EaseOut => Some((0.0, 0.0, 0.58, 1.0)),
            Easing::EaseInOut => Some((0.42, 0.0, 0.58, 1.0)),
            Easing::Bezier { x1, y1, x2, y2 } => Some((unit(x1), unit(y1), unit(x2), unit(y2))),
        }
    }

    /// The true curve's progress at `u` in `[0, 1]` (`Hold` is 0 until the end).
    pub fn curve(&self, u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        let Some((x1, y1, x2, y2)) = self.bezier() else {
            return if matches!(self, Easing::Hold) && u < 1.0 { 0.0 } else { u };
        };
        bezier_axis(bezier_param(x1, x2, u), y1, y2)
    }

    /// The two easings a segment becomes when a key is put `u` of the way along it
    /// (`0 < u < 1`): the first for the part before the new key, the second for the part
    /// after, each normalized to its own unit square, so the curve is the same curve drawn in
    /// two pieces (de Casteljau at the parameter where the curve is `u` of the way across).
    /// A hold stays a hold on both sides and a linear segment linear; the preset curves come
    /// back as the beziers they are. A half whose value does not change (the new key sits
    /// on a plateau) has no shape and is linear. Exact for every preset and for any bezier
    /// whose control points rise (`x1 <= x2`, `y1 <= y2`); a curve whose half would need a
    /// control point outside the unit square (an S whose `y` turns back) has it clamped there —
    /// the same ends and a smooth curve between them, re-fitted: a few hundredths off.
    pub fn split(&self, u: f64) -> (Easing, Easing) {
        let Some((x1, y1, x2, y2)) = self.bezier() else {
            return (*self, *self);
        };
        let lerp = |a: (f64, f64), b: (f64, f64), s: f64| (a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s);
        let (p0, p1, p2, p3) = ((0.0, 0.0), (x1, y1), (x2, y2), (1.0, 1.0));
        let s = bezier_param(x1, x2, u.clamp(0.0, 1.0));
        let (a, b, c) = (lerp(p0, p1, s), lerp(p1, p2, s), lerp(p2, p3, s));
        let (d, e) = (lerp(a, b, s), lerp(b, c, s));
        let at = lerp(d, e, s);
        let unit = |v: f64| v.clamp(0.0, 1.0);
        let half = |from: (f64, f64), h1: (f64, f64), h2: (f64, f64), to: (f64, f64)| {
            let (w, h) = (to.0 - from.0, to.1 - from.1);
            if w < 1e-9 || h < 1e-9 {
                return Easing::Linear;
            }
            let norm = |p: (f64, f64)| (unit((p.0 - from.0) / w), unit((p.1 - from.1) / h));
            let (n1, n2) = (norm(h1), norm(h2));
            Easing::Bezier {
                x1: n1.0,
                y1: n1.1,
                x2: n2.0,
                y2: n2.1,
            }
        };
        (half(p0, a, d, at), half(at, e, c, p3))
    }
}

/// One axis of the cubic bezier from 0 to 1 with inner control values `p1` and `p2`, at `s`.
fn bezier_axis(s: f64, p1: f64, p2: f64) -> f64 {
    let r = 1.0 - s;
    3.0 * r * r * s * p1 + 3.0 * r * s * s * p2 + s * s * s
}

/// The bezier parameter at which its x is `u`: x(s) is monotonic on the unit square, so it is
/// found by bisection (exact enough in 40 halvings).
fn bezier_param(x1: f64, x2: f64, u: f64) -> f64 {
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if bezier_axis(mid, x1, x2) < u {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// The polyline a channel of `(time, value, outgoing easing)` keys is drawn as (sorted by time):
/// a linear segment is its two ends, a `Hold` a step (an equal-time pair at the next key), a
/// curve [`EASE_STEPS`] pieces through the true curve. All-linear keys come back as they went
/// in. [`interpolate`] over the result is the channel's value at any time.
pub fn eased_points(keys: &[(f64, f64, Easing)]) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(keys.len());
    for (i, &(t0, v0, easing)) in keys.iter().enumerate() {
        out.push((t0, v0));
        let Some(&(t1, v1, _)) = keys.get(i + 1) else { break };
        if t1 - t0 < 1e-9 {
            continue;
        }
        match easing {
            Easing::Linear => {}
            Easing::Hold => out.push((t1, v0)),
            curved => {
                for j in 1..EASE_STEPS {
                    let u = j as f64 / EASE_STEPS as f64;
                    out.push((t0 + (t1 - t0) * u, v0 + (v1 - v0) * curved.curve(u)));
                }
            }
        }
    }
    out
}

/// One keyframe of a clip's animated transform: the value of each animatable
/// channel at `time` (seconds from the clip's start). With two or more keyframes
/// the engine interpolates between them — along each key's [`Easing`], linearly unless
/// set — and renders the motion with per-frame ffmpeg expressions; crop and the rest of
/// the static [`Transform`] are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Keyframe {
    /// Offset from the clip's `timeline_start`, in seconds.
    pub time: f64,
    #[serde(default = "one")]
    pub scale: f64,
    #[serde(default)]
    pub pos_x: f64,
    #[serde(default)]
    pub pos_y: f64,
    #[serde(default)]
    pub rotation: f64,
    #[serde(default = "one")]
    pub opacity: f64,
    /// The shape of the segment from this key to the next.
    #[serde(default, skip_serializing_if = "Easing::is_linear")]
    pub easing: Easing,
}

impl Keyframe {
    /// A keyframe at `time` carrying the values of `transform`'s animatable
    /// channels (the static defaults for a fresh keyframe), linear out.
    pub fn from_transform(time: f64, t: &Transform) -> Self {
        Self {
            time,
            scale: t.scale,
            pos_x: t.pos_x,
            pos_y: t.pos_y,
            rotation: t.rotation,
            opacity: t.opacity,
            easing: Easing::Linear,
        }
    }

    /// Every animatable channel `progress` of the way from `self` to `next`, at `time`.
    fn toward(&self, next: &Keyframe, progress: f64, time: f64) -> Keyframe {
        let mix = |a: f64, b: f64| a + (b - a) * progress;
        Keyframe {
            time,
            scale: mix(self.scale, next.scale),
            pos_x: mix(self.pos_x, next.pos_x),
            pos_y: mix(self.pos_y, next.pos_y),
            rotation: mix(self.rotation, next.rotation),
            opacity: mix(self.opacity, next.opacity),
            easing: Easing::Linear,
        }
    }
}

fn default_fov() -> f64 {
    100.0
}

fn default_lens_fov() -> f64 {
    190.0
}

fn flat() -> Projection {
    Projection::Flat
}

/// Narrowest / widest virtual field of view, in degrees. `v360` reads `d_fov=0`
/// as "unset" (derive from `h_fov`/`v_fov`), so the floor must stay above zero,
/// and a command carrying an out-of-range value is *silently discarded* — hence
/// every path that produces a fov clamps into this band first.
pub const MIN_FOV: f64 = 1.0;
pub const MAX_FOV: f64 = 359.0;

/// One keyframe of an animated [`Reframe`]: where the virtual camera points and
/// how wide it sees at `time` (seconds from the clip's start).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReframeKeyframe {
    /// Offset from the clip's `timeline_start`, in seconds.
    pub time: f64,
    #[serde(default)]
    pub yaw: f64,
    #[serde(default)]
    pub pitch: f64,
    #[serde(default)]
    pub roll: f64,
    #[serde(default = "default_fov")]
    pub fov: f64,
}

/// Per-clip reprojection of 360 footage: aim a virtual camera into the sphere
/// and render what it sees. This is the reframing workflow — a 360 source in, an
/// ordinary rectilinear shot out — and with keyframes the camera moves over the
/// clip (a pan across a scene, a whip to a subject) without the source ever being
/// re-encoded.
///
/// `input` is the source's own projection, seeded from the asset's probed
/// [`Projection`] but overridable, since detection is a heuristic. `output` is
/// `Flat` for a normal deliverable, or `Equirect` to stitch a dual-fisheye source
/// without picking a viewing direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Reframe {
    /// Projection of the source footage being read.
    pub input: Projection,
    /// Projection to render into: `Flat` (reframe) or `Equirect` (stitch only).
    #[serde(default = "flat")]
    pub output: Projection,
    /// Field of view of each physical lens, in degrees — only meaningful for a
    /// fisheye `input`. Insta360's lenses run a little past 180°; 190 is a
    /// reasonable starting point, and tuning it moves the stitch seam.
    #[serde(default = "default_lens_fov")]
    pub lens_fov: f64,
    /// Virtual camera heading, in degrees. Wraps at ±180.
    #[serde(default)]
    pub yaw: f64,
    /// Virtual camera elevation, in degrees, clamped to ±90 (straight down to
    /// straight up). Unlike yaw this does *not* wrap — see [`Reframe::sample`].
    #[serde(default)]
    pub pitch: f64,
    /// Virtual camera roll (horizon tilt), in degrees. Wraps at ±180.
    #[serde(default)]
    pub roll: f64,
    /// Diagonal field of view of the virtual camera, in degrees. Maps to `v360`'s
    /// `d_fov`, which derives an aspect-correct horizontal/vertical pair — unlike
    /// `h_fov`, which would need `v_fov` set in lockstep or the picture stretches.
    #[serde(default = "default_fov")]
    pub fov: f64,
    /// Camera animation. Empty = the static pose above; otherwise the engine
    /// interpolates these and drives `v360` over the clip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<ReframeKeyframe>,
}

/// A [`Reframe`] sampled at one instant: the static projection settings plus the
/// virtual camera's pose there, already wrapped and clamped into the ranges
/// `v360` accepts. This is what the engine turns into a `v360` filter, for both
/// the export chain and the still / preview path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedReframe {
    pub input: Projection,
    pub output: Projection,
    pub lens_fov: f64,
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
    pub fov: f64,
}

impl Reframe {
    /// A level, forward-facing 100° view of a source in `input`. This is what a
    /// 360 clip gets when it lands on the timeline, so it previews as ordinary
    /// footage instead of a raw equirect smear or a pair of fisheye circles.
    pub fn new(input: Projection) -> Self {
        Self {
            input,
            output: Projection::Flat,
            lens_fov: default_lens_fov(),
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            fov: default_fov(),
            keyframes: Vec::new(),
        }
    }

    /// True when the clip's camera moves (i.e. carries keyframes).
    pub fn is_animated(&self) -> bool {
        !self.keyframes.is_empty()
    }

    /// The keyframes sorted by time (the stored order is kept sorted by the
    /// editing op, but render code must not assume it).
    pub fn sorted_keyframes(&self) -> Vec<ReframeKeyframe> {
        let mut k = self.keyframes.clone();
        k.sort_by(|a, b| a.time.total_cmp(&b.time));
        k
    }

    /// The static pose, ignoring any animation.
    pub fn pose(&self) -> ResolvedReframe {
        ResolvedReframe {
            input: self.input,
            output: self.output,
            lens_fov: self.lens_fov,
            yaw: wrap180(self.yaw),
            pitch: self.pitch.clamp(-90.0, 90.0),
            roll: wrap180(self.roll),
            fov: self.fov.clamp(MIN_FOV, MAX_FOV),
        }
    }

    /// Sample the virtual camera at `local` seconds from the clip's start.
    ///
    /// Yaw and roll interpolate along the shortest arc, so a pan from 170° to
    /// -170° travels 20° rather than sweeping 340° the long way round. Pitch
    /// deliberately does **not**: panning up and over the pole is never what was
    /// meant, and `v360`'s `|pitch| > 90` region renders an upside-down view.
    pub fn sample(&self, local: f64) -> ResolvedReframe {
        let mut r = self.pose();
        if self.keyframes.is_empty() {
            return r;
        }
        let k = self.sorted_keyframes();
        let pts = |get: fn(&ReframeKeyframe) -> f64| k.iter().map(|kf| (kf.time, get(kf))).collect::<Vec<_>>();
        if let Some(v) = interpolate_angle(&pts(|kf| kf.yaw), local) {
            r.yaw = v;
        }
        if let Some(v) = interpolate(&pts(|kf| kf.pitch), local) {
            r.pitch = v.clamp(-90.0, 90.0);
        }
        if let Some(v) = interpolate_angle(&pts(|kf| kf.roll), local) {
            r.roll = v;
        }
        if let Some(v) = interpolate(&pts(|kf| kf.fov), local) {
            r.fov = v.clamp(MIN_FOV, MAX_FOV);
        }
        r
    }
}

impl ReframeKeyframe {
    /// A keyframe at `time` carrying `pose`'s animatable channels (the values a
    /// fresh keyframe pins).
    pub fn from_pose(time: f64, p: &ResolvedReframe) -> Self {
        Self {
            time,
            yaw: p.yaw,
            pitch: p.pitch,
            roll: p.roll,
            fov: p.fov,
        }
    }
}

/// One keyframe of an animated [`TextOverlay`]: position and opacity at `time`
/// (seconds from the overlay's `start`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TextKeyframe {
    pub time: f64,
    #[serde(default = "half")]
    pub pos_x: f64,
    #[serde(default = "lower_third_y")]
    pub pos_y: f64,
    #[serde(default = "one")]
    pub opacity: f64,
}

/// A timed text element drawn over the composited picture at export (titles,
/// lower-thirds, captions, watermarks). Positions are fractions of the output
/// frame with the text centered on `(pos_x, pos_y)`; `size` is the font height
/// as a fraction of the frame height. Rendered with `drawtext`. Captions are
/// just a batch of these generated from a transcript (see
/// `Project::captions_from_transcript`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextOverlay {
    pub id: Uuid,
    pub text: String,
    /// When the overlay appears / disappears, in timeline seconds.
    pub start: f64,
    pub end: f64,
    #[serde(default = "half")]
    pub pos_x: f64,
    #[serde(default = "lower_third_y")]
    pub pos_y: f64,
    /// Font height as a fraction of the frame height.
    #[serde(default = "default_text_size")]
    pub size: f64,
    /// Any ffmpeg color (e.g. `white`, `#ffcc00`, `yellow@0.9`).
    #[serde(default = "default_text_color")]
    pub color: String,
    /// Optional box color behind the text (e.g. `black@0.5`); `None` = no box.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    /// Optional system font family name (see `fonts::list_system_fonts`);
    /// `None` = FFmpeg's `drawtext` default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(default)]
    pub bold: bool,
    /// Optional position/opacity animation; with ≥1 keyframe the position and
    /// opacity animate over the overlay's lifetime.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<TextKeyframe>,
    /// Written by [`Timeline::captions`] rather than by hand. Regenerating
    /// captions replaces these and leaves everything else alone, so re-running
    /// after a trim does not stack a second set on top of the first — and does
    /// not throw away the title the editor typed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub generated: bool,
}

impl TextOverlay {
    pub fn new(text: impl Into<String>, start: f64, end: f64) -> Self {
        Self {
            id: Uuid::new_v4(),
            text: text.into(),
            start,
            end,
            pos_x: 0.5,
            pos_y: 0.82,
            size: 0.06,
            color: "white".to_string(),
            bg: None,
            font: None,
            bold: false,
            keyframes: Vec::new(),
            generated: false,
        }
    }

    /// Sample `(pos_x, pos_y, opacity)` at timeline time `t`. Static fields when
    /// the overlay is not animated; the interpolated keyframe values otherwise.
    /// Used by the still / preview path, which can't evaluate the export's
    /// per-frame `drawtext` expressions.
    pub fn sample(&self, t: f64) -> (f64, f64, f64) {
        if self.keyframes.is_empty() {
            return (self.pos_x, self.pos_y, 1.0);
        }
        let local = t - self.start;
        let chan = |get: fn(&TextKeyframe) -> f64, fallback: f64| {
            interpolate(&self.keyframes.iter().map(|k| (k.time, get(k))).collect::<Vec<_>>(), local).unwrap_or(fallback)
        };
        (
            chan(|k| k.pos_x, self.pos_x),
            chan(|k| k.pos_y, self.pos_y),
            chan(|k| k.opacity, 1.0),
        )
    }
}

/// Linearly interpolate a channel of `(time, value)` keyframes at `at`, holding
/// the end values flat beyond the first / last keyframe. Empty input → `None`.
pub fn interpolate(points: &[(f64, f64)], at: f64) -> Option<f64> {
    match points {
        [] => None,
        [single] => Some(single.1),
        _ => {
            if at <= points[0].0 {
                return Some(points[0].1);
            }
            for pair in points.windows(2) {
                let (t0, v0) = pair[0];
                let (t1, v1) = pair[1];
                if at < t1 {
                    if t1 <= t0 {
                        return Some(v0);
                    }
                    return Some(v0 + (v1 - v0) * (at - t0) / (t1 - t0));
                }
            }
            Some(points[points.len() - 1].1)
        }
    }
}

/// Wrap an angle in degrees into `[-180, 180)`.
pub fn wrap180(deg: f64) -> f64 {
    if !deg.is_finite() {
        return 0.0;
    }
    let mut d = (deg + 180.0) % 360.0;
    if d < 0.0 {
        d += 360.0;
    }
    d - 180.0
}

/// Linearly interpolate an *angular* channel (degrees) at `at`, taking the
/// shortest arc across the ±180 seam: a 170° → -170° pair travels +20°, not
/// -340°.
///
/// The whole sequence is unwrapped once onto a continuous path before
/// interpolating, rather than resolving each segment on its own. That matters
/// because two different callers walk this data — the still / preview path
/// samples it point by point, while the export emitter marches across it — and
/// per-segment unwrapping would let them disagree about which way the camera
/// turned right at the seam. The result is wrapped back into `[-180, 180)`,
/// since `v360` silently discards a command outside that range.
pub fn interpolate_angle(points: &[(f64, f64)], at: f64) -> Option<f64> {
    let (first, rest) = points.split_first()?;
    let mut unwrapped = Vec::with_capacity(points.len());
    let mut prev = wrap180(first.1);
    unwrapped.push((first.0, prev));
    for &(t, v) in rest {
        prev += wrap180(v - prev);
        unwrapped.push((t, prev));
    }
    interpolate(&unwrapped, at).map(wrap180)
}

/// Render a transcript as a SubRip (`.srt`) subtitle document.
pub fn transcript_to_srt(segments: &[TranscriptSegment]) -> String {
    fn ts(seconds: f64) -> String {
        let s = seconds.max(0.0);
        let ms = (s * 1000.0).round() as u64;
        let (h, rem) = (ms / 3_600_000, ms % 3_600_000);
        let (m, rem) = (rem / 60_000, rem % 60_000);
        let (sec, milli) = (rem / 1000, rem % 1000);
        format!("{h:02}:{m:02}:{sec:02},{milli:03}")
    }
    let mut out = String::new();
    for (i, seg) in segments.iter().enumerate() {
        out.push_str(&format!(
            "{n}\n{start} --> {end}\n{text}\n\n",
            n = i + 1,
            start = ts(seg.start),
            end = ts(seg.end),
            text = seg.text.trim(),
        ));
    }
    out
}

/// Shortest a generated caption line is allowed to stay on screen (seconds).
/// Splitting a fast sentence strictly by character share can hand a two-letter
/// chunk a couple of frames, which reads as a flicker rather than as a word, so
/// chunks below this are merged back into a neighbour instead.
pub const MIN_CAPTION: f64 = 0.45;

/// How much of a caption line has to survive a cut for it to be kept (seconds).
/// A line whose words were trimmed away leaves a sliver of overlap at the clip
/// edge; showing it would caption footage that is no longer there.
pub const MIN_CAPTION_VISIBLE: f64 = 0.15;

/// The same two floors for [`CaptionStyle::WordPunch`], where a line *is* one
/// word. Held to [`MIN_CAPTION`] every short word would merge into a neighbour
/// and the style would collapse back into [`CaptionStyle::Lines`]; words still
/// merge — a one-letter word's character share is a couple of frames — just far
/// later.
pub const MIN_WORD_CAPTION: f64 = 0.12;
pub const MIN_WORD_VISIBLE: f64 = 0.06;

/// The shape a generated caption set takes on screen.
///
/// Two, because they are consumed differently. A subtitle line is *read*: it
/// holds still long enough to take several words in at once. The one-word form
/// is *watched* — each word lands on the beat of the speech, which is the look
/// social captions have converged on and most of why a muted feed video holds
/// attention. It is not a font choice: the word count, the size, the position
/// and the floors that stop a line flickering all move together, so it is one
/// decision rather than four.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptionStyle {
    /// A few words at a time, held as a subtitle line low in the frame.
    #[default]
    Lines,
    /// One word at a time, large and bold, cut in and out on the word.
    WordPunch,
}

impl CaptionStyle {
    /// The style's own numbers, before any per-call override.
    fn layout(self) -> CaptionLayout {
        match self {
            Self::Lines => CaptionLayout {
                max_words: 4,
                max_chars: 28,
                pos_y: 0.88,
                size: 0.05,
                bold: false,
                min_line: MIN_CAPTION,
                min_visible: MIN_CAPTION_VISIBLE,
            },
            Self::WordPunch => CaptionLayout {
                max_words: 1,
                max_chars: 28,
                // Higher and much larger than a subtitle: one word carries the
                // whole frame, and sitting it on the bottom edge would put it
                // under the platform's own caption rail.
                pos_y: 0.72,
                size: 0.11,
                bold: true,
                min_line: MIN_WORD_CAPTION,
                min_visible: MIN_WORD_VISIBLE,
            },
        }
    }
}

/// A [`CaptionStyle`]'s numbers with any per-call override applied — what
/// [`Timeline::captions`] actually works from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptionLayout {
    /// Most words on one caption line.
    pub max_words: usize,
    /// Most characters on one caption line; the tighter of the two limits wins.
    pub max_chars: usize,
    /// Vertical position as a fraction of frame height.
    pub pos_y: f64,
    /// Font height as a fraction of frame height.
    pub size: f64,
    /// Whether the text is drawn bold.
    pub bold: bool,
    /// Shortest a line may be before it merges into a neighbour.
    pub min_line: f64,
    /// Shortest a line clipped by a cut may be before it is dropped.
    pub min_visible: f64,
}

/// How a transcript is turned into on-screen captions. Everything but the style
/// is an *override*: omit a field and it follows the style, so asking for
/// [`CaptionStyle::WordPunch`] on its own gets the whole look rather than one
/// word left at subtitle size in the subtitle position.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptionOptions {
    /// The look; defaults to [`CaptionStyle::Lines`].
    #[serde(default)]
    pub style: CaptionStyle,
    /// Most words on one caption line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_words: Option<usize>,
    /// Most characters on one caption line; the tighter of the two limits wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
    /// Vertical position as a fraction of frame height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos_y: Option<f64>,
    /// Font height as a fraction of frame height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
}

impl CaptionOptions {
    /// A style with no overrides.
    pub fn styled(style: CaptionStyle) -> Self {
        Self {
            style,
            ..Self::default()
        }
    }

    /// The numbers to caption with: the style's, with any override that is
    /// actually usable applied over them.
    pub fn resolve(self) -> CaptionLayout {
        let base = self.style.layout();
        CaptionLayout {
            max_words: self.max_words.map_or(base.max_words, |v| v.max(1)),
            max_chars: self.max_chars.map_or(base.max_chars, |v| v.max(1)),
            pos_y: overridden(self.pos_y, base.pos_y, 0.0, 1.0),
            size: overridden(self.size, base.size, 0.005, 0.5),
            ..base
        }
    }
}

/// Roughly how wide one character is as a fraction of the font size, measured
/// off `drawtext`'s default face. Real caption text runs 0.44–0.75 depending on
/// the word; 0.6 sits above the 0.52–0.55 that *long* text averages, and long
/// text is the only kind that ever reaches the cap.
const CHAR_ADVANCE: f64 = 0.6;

/// How much of the frame width a caption may take.
const CAPTION_WIDTH: f64 = 0.9;

/// How far into an *empty* timeline imported caption cues may reach (seconds).
/// A cut has an end that cues are clipped to; with nothing on the timeline yet
/// there is none, and a file's times are then bounded by the day instead of by
/// whatever a hand-edited file says.
pub const EMPTY_CUT_WINDOW: f64 = 24.0 * 3600.0;

/// The frame captions assume when the project has not picked one. A timeline
/// cannot see its assets, so it cannot derive the footage default `export_format`
/// would use — and 16:9 is wide enough that the fit below never binds, which is
/// what keeps an unframed project captioned exactly as it was before.
const DEFAULT_CAPTION_ASPECT: f64 = 16.0 / 9.0;

/// Shrink a caption's size (a fraction of frame height) until its text fits
/// across a frame of `aspect` (width / height).
///
/// `drawtext` neither wraps nor scales: text wider than the frame is simply
/// drawn off both edges. A 9:16 frame is barely half as wide as it is tall, so
/// the social shape this whole feature is for is exactly where a long word runs
/// off — and `fontsize` cannot be an expression over `text_w`, since the width
/// is what depends on the size. So the fit is estimated here from the character
/// count, which is the only measurement available before the filter runs.
fn fit_size(text: &str, size: f64, aspect: f64) -> f64 {
    let chars = text.chars().count().max(1) as f64;
    size.min(CAPTION_WIDTH * aspect / (chars * CHAR_ADVANCE))
}

/// Apply an optional override, ignoring one that is not a finite number and
/// clamping the rest into range.
fn overridden(v: Option<f64>, base: f64, lo: f64, hi: f64) -> f64 {
    match v {
        Some(v) if v.is_finite() => v.clamp(lo, hi),
        _ => base,
    }
}

/// Break a transcript line into caption-sized groups of words. Greedy: take
/// words until either limit would be exceeded, always at least one (a single
/// word longer than `max_chars` is its own line rather than being cut in half).
fn chunk_words(text: &str, layout: CaptionLayout) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut words = 0usize;
    for word in text.split_whitespace() {
        let extra = if current.is_empty() {
            word.chars().count()
        } else {
            word.chars().count() + 1
        };
        let fits = words < layout.max_words && current.chars().count() + extra <= layout.max_chars;
        if !current.is_empty() && !fits {
            out.push(std::mem::take(&mut current));
            words = 0;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        words += 1;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Spread `span` across `chunks` in proportion to how much text each carries,
/// then merge away any line too short to read. Character share is the honest
/// approximation available here: neither speech backend reports word timings
/// (`TranscriptSegment` has only a start and an end), so within a segment the
/// speaker is assumed to be at a steady pace.
///
/// The merge is repeated — join the first too-short line to its shorter
/// neighbour, re-time everything, look again — because joining changes every
/// line's share. What that costs is the *scan*, not the bookkeeping: weights and
/// their total are kept as they change (a merge adds exactly the joining space)
/// and a scan stops at the first line that is too short, so a pass allocates
/// nothing and the text is only copied for the lines actually merged. With a
/// cue of a thousand one-word lines that is the difference between a millisecond
/// and a second — imported files make such cues possible.
fn time_chunks(chunks: Vec<String>, span: TimeRange, min: f64) -> Vec<(TimeRange, String)> {
    let mut chunks = chunks;
    let duration = (span.end - span.start).max(0.0);
    let mut weights: Vec<f64> = chunks.iter().map(|c| c.chars().count().max(1) as f64).collect();
    let mut total: f64 = weights.iter().sum();
    // The end of chunk `i` of `n`, given where the previous one ended.
    let end_of = |weights: &[f64], total: f64, at: f64, i: usize| {
        if i + 1 == weights.len() {
            span.end
        } else {
            let share = if total > 0.0 { weights[i] / total } else { 1.0 };
            at + duration * share
        }
    };
    // A whole segment shorter than `min` is one line, not a merge loop.
    while chunks.len() >= 2 {
        let mut at = span.start;
        let mut short = None;
        for i in 0..chunks.len() {
            let end = end_of(&weights, total, at, i);
            if end - at < min {
                short = Some(i);
                break;
            }
            at = end;
        }
        let Some(i) = short else { break };
        // Merge into the shorter neighbour so the joined line stays as close to
        // the requested width as the timing allows.
        let merge_back = i > 0 && (i + 1 == chunks.len() || chunks[i - 1].chars().count() <= chunks[i + 1].chars().count());
        let into = if merge_back { i - 1 } else { i };
        let moved = chunks.remove(into + 1);
        chunks[into] = format!("{}{}{}", chunks[into], ' ', moved);
        let moved_weight = weights.remove(into + 1);
        weights[into] += moved_weight + 1.0;
        total += 1.0;
    }
    let mut timed: Vec<(TimeRange, String)> = Vec::with_capacity(chunks.len());
    let mut at = span.start;
    for (i, text) in chunks.into_iter().enumerate() {
        let end = end_of(&weights, total, at, i);
        timed.push((TimeRange { start: at, end }, text));
        at = end;
    }
    timed
}

/// One caption line on its way to the screen: when, what, and which input it
/// was cut from. `origin` is what lets an import say *which cue* fell outside the
/// cut or lost its slot to another — transcript captioning never reads it.
struct CaptionLine {
    range: TimeRange,
    text: String,
    origin: usize,
}

/// Chunk `text` over `span` into caption lines and keep what lies inside
/// `window`. The span is chunked *whole* and each line is clipped afterwards, so
/// a sentence that is cut in half captions only the half still in the cut — and
/// a line left with less than a readable sliver is not kept at all.
fn push_caption_lines(
    lines: &mut Vec<CaptionLine>,
    text: &str,
    span: TimeRange,
    window: (f64, f64),
    origin: usize,
    layout: CaptionLayout,
) {
    for (range, chunk) in time_chunks(chunk_words(text, layout), span, layout.min_line) {
        let start = range.start.max(window.0);
        let end = range.end.min(window.1);
        if end - start < layout.min_visible {
            continue;
        }
        lines.push(CaptionLine {
            range: TimeRange { start, end },
            text: chunk,
            origin,
        });
    }
}

/// Project timed text that is in a clip's **source** time through every clip that
/// shows its footage. `segments_for` answers which text belongs to an asset — a
/// transcript map for [`Timeline::captions`], one imported file for
/// [`Timeline::place_cues`]. `rendered` must already be [`Timeline::for_render`]
/// so a muted track and a disabled clip are as uncaptioned as they are unheard.
fn project_through_clips<'a>(
    rendered: &Timeline,
    segments_for: impl Fn(Uuid) -> Option<&'a [TranscriptSegment]>,
    layout: CaptionLayout,
) -> Vec<CaptionLine> {
    let mut lines: Vec<CaptionLine> = Vec::new();
    for track in &rendered.tracks {
        for clip in &track.clips {
            let Some(segments) = segments_for(clip.asset_id) else {
                continue;
            };
            let window = (clip.timeline_start, clip.timeline_end());
            for (origin, seg) in segments.iter().enumerate() {
                let text = seg.text.trim();
                let timed = seg.start.is_finite() && seg.end.is_finite() && seg.end > seg.start;
                if text.is_empty() || !timed || !clip.covers_source(seg.start, seg.end) {
                    continue;
                }
                let span = clip.source_span_to_timeline(seg.start, seg.end);
                push_caption_lines(&mut lines, text, span, window, origin, layout);
            }
        }
    }
    lines
}

/// How lines that start at the same moment are ordered — which of them gets the
/// slot and which waits behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimultaneousLines {
    /// Alphabetically. All a transcript has: its segments are in no meaningful
    /// order against each other, and this is what it has always done.
    ByText,
    /// In the order the input gave them. An imported file's cues are in the
    /// order its author wrote them, so two cues at one timecode keep that order
    /// instead of whichever sorts first.
    ByOrigin,
}

/// Settle chunked lines into the caption lane: ordered, de-duplicated, never two
/// on screen at once, each sized to fit the frame. Returns each overlay with the
/// `origin` of the line it came from.
fn settle_caption_lines(
    mut lines: Vec<CaptionLine>,
    layout: CaptionLayout,
    aspect: f64,
    simultaneous: SimultaneousLines,
) -> Vec<(TextOverlay, usize)> {
    lines.sort_by(|a, b| a.range.start.total_cmp(&b.range.start).then_with(|| a.text.cmp(&b.text)));
    // The same words can reach two clips — `extract_audio` leaves the picture
    // and its detached audio both referencing the asset — and drawing one
    // caption twice is drawing it bolder, not twice.
    lines.dedup_by(|a, b| a.text == b.text && (a.range.start - b.range.start).abs() < 1e-3);
    if simultaneous == SimultaneousLines::ByOrigin {
        // Stable, so everything else keeps the order it was just given.
        lines.sort_by(|a, b| a.range.start.total_cmp(&b.range.start).then_with(|| a.origin.cmp(&b.origin)));
    }
    // Captions are one lane of text at one screen position, so two at once is
    // two unreadable ones. The same footage reaching the cut twice — a
    // callback shot, or a full source parked under the edit — otherwise
    // collides with whatever is already on screen. First line in wins the
    // slot; the next starts where it ends, or is dropped if nothing readable
    // is left of it.
    let mut placed: Vec<CaptionLine> = Vec::with_capacity(lines.len());
    for line in lines {
        let start = placed
            .last()
            .map_or(line.range.start, |prev| line.range.start.max(prev.range.end));
        if line.range.end - start < layout.min_visible {
            continue;
        }
        placed.push(CaptionLine {
            range: TimeRange {
                start,
                end: line.range.end,
            },
            ..line
        });
    }
    placed
        .into_iter()
        .map(|line| {
            let size = fit_size(&line.text, layout.size, aspect);
            let mut o = TextOverlay::new(line.text, line.range.start.max(0.0), line.range.end);
            o.pos_y = layout.pos_y;
            o.size = size;
            o.bold = layout.bold;
            o.bg = Some("black@0.5".to_string());
            o.generated = true;
            (o, line.origin)
        })
        .collect()
}

/// Which clock an imported caption file's times are on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaptionTimeBase {
    /// The file was made for the finished cut: its times *are* timeline time.
    /// What a subtitle file normally is.
    #[default]
    Timeline,
    /// The file times one source — a transcript, or subtitles for the uncut
    /// footage. Each cue is projected through every clip of this asset, exactly
    /// as a transcript segment is, so it follows trims, reorders and speed.
    Source(Uuid),
}

impl CaptionTimeBase {
    /// The base a caller asked for by name (`timeline` / `source`), plus the
    /// asset a `source` base is about. Both surfaces take these two loose
    /// arguments, so the rules for combining them live in one place: an asset
    /// alone implies `source`; neither implies `timeline`; and a contradiction
    /// is an error rather than one of the two being quietly preferred.
    pub fn resolve(base: Option<&str>, asset_id: Option<Uuid>) -> Result<Self> {
        let named = base.map(|b| b.trim().to_ascii_lowercase());
        match (named.as_deref(), asset_id) {
            (None | Some("" | "timeline"), None) => Ok(Self::Timeline),
            (None | Some("" | "source"), Some(asset)) => Ok(Self::Source(asset)),
            (Some("source"), None) => Err(Error::InvalidArgument(
                "base \"source\" needs asset_id: the asset whose footage the cue times belong to".to_string(),
            )),
            (Some("timeline"), Some(_)) => Err(Error::InvalidArgument(
                "asset_id only applies to base \"source\"; cues in timeline time are not tied to an asset".to_string(),
            )),
            (Some(other), _) => Err(Error::InvalidArgument(format!(
                "unknown caption time base {other:?}; expected \"timeline\" or \"source\""
            ))),
        }
    }
}

/// Imported cues laid onto the cut: the overlays, and an account of every cue.
/// `placed + dropped_outside + dropped_short + dropped_overlap` is the number of
/// cues offered.
#[derive(Debug, Clone, Default)]
pub struct CaptionPlacement {
    pub overlays: Vec<TextOverlay>,
    /// Cues that put at least one caption on screen.
    pub placed: usize,
    /// Cues that never met the cut: past its end or before its start, timing
    /// footage no (rendered) clip shows, or not a usable cue at all.
    pub dropped_outside: usize,
    /// Cues that did meet the cut, but for a moment too short to read — their own
    /// length, or the sliver of them left inside its edge (or a clip's).
    pub dropped_short: usize,
    /// Cues that did reach the cut but lost their slot — the same words at the
    /// same moment as an earlier cue, or wholly under one that was already on
    /// screen. Captions are one lane of text.
    pub dropped_overlap: usize,
}

impl Timeline {
    /// The frame captions are fitted to: the project's own, else 16:9.
    fn caption_aspect(&self) -> f64 {
        self.format
            .map_or(DEFAULT_CAPTION_ASPECT, |d| f64::from(d.width) / f64::from(d.height))
    }

    /// Caption overlays for the cut as it currently stands.
    ///
    /// The point of doing this over the timeline rather than over an asset: a
    /// transcript is in **source** time, an overlay is in **timeline** time, and
    /// between them sit every trim, every reorder, every speed change and every
    /// silence the editor removed. Each segment is projected through the clips
    /// that actually show its footage ([`Clip::source_span_to_timeline`]), so
    /// captions land on the words that survived the cut — and words that did not
    /// survive get no caption at all.
    ///
    /// Reads through [`Timeline::for_render`], so a muted track and a disabled
    /// clip are as uncaptioned as they are unheard.
    pub fn captions(&self, transcripts: &HashMap<Uuid, Vec<TranscriptSegment>>, opts: CaptionOptions) -> Vec<TextOverlay> {
        let layout = opts.resolve();
        let rendered = self.for_render();
        let lines = project_through_clips(&rendered, |asset| transcripts.get(&asset).map(Vec::as_slice), layout);
        settle_caption_lines(lines, layout, self.caption_aspect(), SimultaneousLines::ByText)
            .into_iter()
            .map(|(overlay, _)| overlay)
            .collect()
    }

    /// Lay the cues of an imported subtitle file onto the cut as caption
    /// overlays — [`Timeline::captions`] for text that did not come from a
    /// transcript.
    ///
    /// Everything after the time mapping is shared with transcript captioning
    /// (chunking to the style's line length, the flicker floors, one lane with
    /// no two lines at once, fitting to the delivery frame), so an imported set
    /// looks and behaves like a generated one. The mapping is the only part that
    /// differs:
    ///
    /// * [`CaptionTimeBase::Source`] *is* transcript captioning — the cues are
    ///   projected through the asset's clips, honoring trim, speed, reverse and
    ///   `for_render`.
    /// * [`CaptionTimeBase::Timeline`] takes the cue times as they stand. They
    ///   are clipped to the length the cut renders at (a subtitle file for the
    ///   whole film outruns a short cut), and — unlike source time — a muted
    ///   track does not silence them: the file captions the finished cut, not
    ///   one clip's sound. With nothing on the timeline yet there is no end to
    ///   run past, so the window is [`EMPTY_CUT_WINDOW`] instead.
    ///
    /// Simultaneous cues keep the order the input gave them (transcripts, which
    /// have none, sort alphabetically). Every cue is accounted for in exactly one
    /// bucket of the result.
    pub fn place_cues(&self, cues: &[TranscriptSegment], base: CaptionTimeBase, opts: CaptionOptions) -> CaptionPlacement {
        let layout = opts.resolve();
        let rendered = self.for_render();
        let cut_end = rendered.duration();
        let window_end = if cut_end > 0.0 { cut_end } else { EMPTY_CUT_WINDOW };
        let usable = |cue: &TranscriptSegment| {
            !cue.text.trim().is_empty() && cue.start.is_finite() && cue.end.is_finite() && cue.end > cue.start
        };
        let lines = match base {
            CaptionTimeBase::Source(asset) => project_through_clips(&rendered, |id| (id == asset).then_some(cues), layout),
            CaptionTimeBase::Timeline => {
                let mut lines = Vec::new();
                for (origin, cue) in cues.iter().enumerate().filter(|(_, cue)| usable(cue)) {
                    let span = TimeRange {
                        start: cue.start,
                        end: cue.end,
                    };
                    push_caption_lines(&mut lines, cue.text.trim(), span, (0.0, window_end), origin, layout);
                }
                lines
            }
        };
        let reached: HashSet<usize> = lines.iter().map(|l| l.origin).collect();
        // A cue that produced nothing either never met the cut, or met it for too
        // short a moment to read — different complaints, so different counts.
        let source_clips: Vec<&Clip> = match base {
            CaptionTimeBase::Source(asset) => rendered
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .filter(|c| c.asset_id == asset)
                .collect(),
            CaptionTimeBase::Timeline => Vec::new(),
        };
        let meets_the_cut = |cue: &TranscriptSegment| {
            usable(cue)
                && match base {
                    CaptionTimeBase::Timeline => cue.end > 0.0 && cue.start < window_end,
                    CaptionTimeBase::Source(_) => source_clips.iter().any(|c| c.covers_source(cue.start, cue.end)),
                }
        };
        let dropped_short = cues
            .iter()
            .enumerate()
            .filter(|(origin, cue)| !reached.contains(origin) && meets_the_cut(cue))
            .count();
        let settled = settle_caption_lines(lines, layout, self.caption_aspect(), SimultaneousLines::ByOrigin);
        let kept: HashSet<usize> = settled.iter().map(|(_, origin)| *origin).collect();
        CaptionPlacement {
            placed: kept.len(),
            dropped_outside: cues.len() - reached.len() - dropped_short,
            dropped_short,
            dropped_overlap: reached.len() - kept.len(),
            overlays: settled.into_iter().map(|(overlay, _)| overlay).collect(),
        }
    }
}

/// A single non-destructive edit referencing a source range of an asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: Uuid,
    pub asset_id: Uuid,
    /// In-point in the source asset (seconds).
    pub source_in: f64,
    /// Out-point in the source asset (seconds).
    pub source_out: f64,
    /// Position of the clip on the timeline (seconds).
    pub timeline_start: f64,
    /// Linear gain applied to this clip (1.0 = unchanged).
    pub volume: f32,
    /// Fade-in duration at the clip's start (seconds); 0.0 = no fade. Applied to
    /// both picture (fade from black) and audio (fade from silence) at export.
    #[serde(default)]
    pub fade_in: f64,
    /// Fade-out duration at the clip's end (seconds); 0.0 = no fade.
    #[serde(default)]
    pub fade_out: f64,
    /// Playback rate (1.0 = unchanged). > 1.0 speeds up, < 1.0 slows down, and a
    /// negative value plays the source in reverse. The clip's timeline duration
    /// is its source span divided by the magnitude of the speed.
    #[serde(default = "one")]
    pub speed: f64,
    /// Geometric transform (scale / position / crop / rotation / opacity).
    #[serde(default)]
    pub transform: Transform,
    /// Color correction (brightness / contrast / saturation / gamma).
    #[serde(default)]
    pub color: Color,
    /// Transition blending this clip's start with the preceding clip, if any.
    #[serde(default)]
    pub transition_in: Option<Transition>,
    /// Video effects applied in order at export (blur, chroma key, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<VideoEffect>,
    /// Audio effects applied in order at export (EQ, compressor, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<AudioEffect>,
    /// Transform animation. Empty = the static `transform` is used; otherwise the
    /// engine interpolates these keyframes to animate scale / position / rotation
    /// / opacity over the clip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<Keyframe>,
    /// Per-property animation: any one number of the transform, the colour or the clip's
    /// volume with keys of its own ([`PropertyTrack`]). A property with a track is driven by
    /// it, whatever `keyframes` says; one without keeps reading the bundle (transform numbers)
    /// or its static value. Empty for every clip saved before channels existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<PropertyTrack>,
    /// Reprojection of 360 source footage, when this clip references a spherical
    /// asset. `None` for ordinary flat video (and for a 360 clip the user has
    /// explicitly un-reframed, to work in the raw projection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reframe: Option<Reframe>,
    /// A shape cut out of this clip, making the rest transparent so a lower
    /// track shows through. `None` is the whole frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Mask>,
    /// Crops for delivery shapes other than the one the project is cut for —
    /// what lets one cut render as a 9:16 Reel *and* a 1:1 post with each shot
    /// framed for each. Written by the multi-format export's framing pass, read
    /// only by [`Timeline::for_delivery`]; the project frame's own crop stays in
    /// `transform`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub framings: Vec<Framing>,
    /// Whether this clip renders. A disabled clip keeps its place on the
    /// timeline (and its trims, effects and keyframes) but is dropped before the
    /// render graph is built — the per-clip counterpart of muting a track.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub enabled: bool,
    /// The link group this clip belongs to: clips sharing a `link_id` are one
    /// piece of material — a picture and the sound that goes with it — and an
    /// edit to one is carried to the others (see [`Timeline::link_partners`] and
    /// the `links` module). At most one clip of a group per track. Never part of
    /// the render graph; `None` (omitted from the file) is an unlinked clip, which
    /// is every clip a project made before links existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<Uuid>,
    /// Whether the clip plays the audio of its *own* asset. A video clip's
    /// footage normally carries its sound with it; `false` mutes just that —
    /// the picture is untouched — which is how a clip's sound is **detached**
    /// onto an audio track (`Project::detach_audio`) without being heard twice.
    /// Defaulted and omitted at `true`, so every graph written before the field
    /// existed is byte-identical.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub source_audio: bool,
}

fn yes() -> bool {
    true
}

fn is_yes(b: &bool) -> bool {
    *b
}

/// Smallest speed magnitude allowed, to keep clip durations finite.
pub const MIN_SPEED: f64 = 0.01;

impl Clip {
    /// A new clip with default volume, no fades, full speed and identity
    /// transform / color and no transition.
    pub fn new(asset_id: Uuid, source_in: f64, source_out: f64, timeline_start: f64) -> Self {
        Self {
            id: Uuid::new_v4(),
            asset_id,
            source_in,
            source_out,
            timeline_start,
            volume: 1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 1.0,
            transform: Transform::default(),
            color: Color::default(),
            transition_in: None,
            effects: Vec::new(),
            audio: Vec::new(),
            keyframes: Vec::new(),
            channels: Vec::new(),
            reframe: None,
            mask: None,
            framings: Vec::new(),
            enabled: true,
            link_id: None,
            source_audio: true,
        }
    }

    /// The crop this clip carries for a delivery shape, if it was framed for it.
    pub fn framing_for(&self, ratio: (u32, u32)) -> Option<&Framing> {
        self.framings.iter().find(|f| f.ratio() == ratio)
    }

    /// Record the crop for one delivery shape, replacing an earlier one for the
    /// same shape. Returns whether anything changed.
    pub fn set_framing(&mut self, framing: Framing) -> bool {
        match self.framings.iter_mut().find(|f| f.ratio() == framing.ratio()) {
            Some(existing) if *existing == framing => false,
            Some(existing) => {
                *existing = framing;
                true
            }
            None => {
                self.framings.push(framing);
                true
            }
        }
    }

    /// A new clip that also reframes, when `asset` is 360 footage — the shape
    /// every clip-creating op should use so a spherical source lands on the
    /// timeline already looking like ordinary video.
    pub fn for_asset(asset: &Asset, source_in: f64, source_out: f64, timeline_start: f64) -> Self {
        let mut clip = Self::new(asset.id, source_in, source_out, timeline_start);
        clip.reframe = asset.projection().map(Reframe::new);
        clip
    }

    /// True when any number of the clip's transform is keyed (the bundle's keys or a
    /// property's own track), i.e. the picture is animated.
    pub fn is_animated(&self) -> bool {
        Property::TRANSFORM.iter().any(|p| self.is_keyed(*p))
    }

    /// True when the keyframes move the clip's *scale*, i.e. the picture the
    /// export hands to `overlay` changes size from one frame to the next.
    ///
    /// That is a different thing from [`Clip::is_animated`]: a clip keyed only on
    /// position, rotation or opacity keeps a constant picture size, which every
    /// filter in its chain can be built around. A zoom that actually moves cannot
    /// be: most filters read the frame size once, when the graph is configured, so
    /// the export restructures the chain around it (`video_clip_chain` puts the
    /// size-changing `scale` last). The test is on the keyed values, not on the
    /// expression the engine writes.
    pub fn zoom_animated(&self) -> bool {
        // Against the first key *in time*, which is the one the engine's expression holds
        // before the clip's first moment (the stored order is not guaranteed to be sorted).
        let keys = self.property_keys(Property::Scale);
        keys.first()
            .is_some_and(|first| keys.iter().any(|k| (k.value - first.value).abs() > 1e-9))
    }

    /// The clip's keyframes sorted by time (the stored order is kept sorted by
    /// the editing op, but render code must not assume it).
    pub fn sorted_keyframes(&self) -> Vec<Keyframe> {
        let mut k = self.keyframes.clone();
        k.sort_by(|a, b| a.time.total_cmp(&b.time));
        k
    }

    /// One animatable channel of the keyframes as the polyline every renderer draws
    /// ([`eased_points`]): what [`Clip::transform_at`] interpolates and what the export's
    /// per-frame expression is written from, so the two cannot disagree.
    pub fn keyframe_channel(&self, get: fn(&Keyframe) -> f64) -> Vec<(f64, f64)> {
        let keys: Vec<(f64, f64, Easing)> = self.sorted_keyframes().iter().map(|k| (k.time, get(k), k.easing)).collect();
        eased_points(&keys)
    }

    /// Put `key` into the animation. A key within a microsecond of an existing one replaces it
    /// and keeps how that one leaves (re-keying a moment moves the pose, not the shape). A key
    /// that lands inside a segment splits it ([`Easing::split`]): the key before it eases into
    /// the new one along the first part of the curve and the new one eases out along the rest —
    /// a hold stays held through the new key rather than turning the rest of it into a ramp. The
    /// key's own `easing` is replaced when it replaces or splits a key; a key before the first
    /// or after the last keeps the one it came with.
    pub fn insert_keyframe(&mut self, mut key: Keyframe) {
        const SAME: f64 = 1e-6;
        let sorted = self.sorted_keyframes();
        if let Some(old) = sorted.iter().find(|k| (k.time - key.time).abs() <= SAME) {
            key.easing = old.easing;
        } else if let Some(w) = sorted.windows(2).find(|w| w[0].time < key.time && key.time < w[1].time) {
            let u = (key.time - w[0].time) / (w[1].time - w[0].time);
            let (before, after) = w[0].easing.split(u);
            key.easing = after;
            if let Some(a) = self.keyframes.iter_mut().find(|k| k.time == w[0].time) {
                a.easing = before;
            }
        }
        self.keyframes.retain(|k| (k.time - key.time).abs() > SAME);
        self.keyframes.push(key);
        self.keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
    }

    /// Sample the (possibly animated) transform at `local` seconds from the
    /// clip's start: the static [`Transform`] with its animatable numbers
    /// (scale / position / rotation / opacity) overridden by the interpolated
    /// value of every one that is keyed ([`Clip::property_at`]). Used by the still / preview
    /// path, which cannot evaluate the export's per-frame expressions.
    pub fn transform_at(&self, local: f64) -> Transform {
        let mut t = self.transform;
        if !self.is_animated() {
            return t;
        }
        t.scale = self.property_at(Property::Scale, local);
        t.pos_x = self.property_at(Property::PosX, local);
        t.pos_y = self.property_at(Property::PosY, local);
        t.rotation = self.property_at(Property::Rotation, local);
        t.opacity = self.property_at(Property::Opacity, local);
        t
    }

    /// Sample the (possibly animated) reframe at `local` seconds from the clip's
    /// start. `None` when the clip is not reframed. Used by the still / preview
    /// path, which cannot drive `v360` with runtime commands the way export does.
    pub fn reframe_at(&self, local: f64) -> Option<ResolvedReframe> {
        self.reframe.as_ref().map(|r| r.sample(local))
    }

    /// Length of the referenced source span (seconds), ignoring speed.
    pub fn source_duration(&self) -> f64 {
        (self.source_out - self.source_in).max(0.0)
    }

    /// Speed magnitude, clamped away from zero (direction dropped).
    pub fn speed_mag(&self) -> f64 {
        self.speed.abs().max(MIN_SPEED)
    }

    /// True when the clip plays its source in reverse.
    pub fn is_reversed(&self) -> bool {
        self.speed < 0.0
    }

    /// Duration on the timeline (seconds), i.e. the source span retimed by speed.
    pub fn duration(&self) -> f64 {
        self.source_duration() / self.speed_mag()
    }

    pub fn timeline_end(&self) -> f64 {
        self.timeline_start + self.duration()
    }

    /// Where a source timestamp of this clip lands on the timeline (seconds).
    /// Honors speed and reverse, so an analysis marker maps to the moment it is
    /// actually heard.
    pub fn source_to_timeline(&self, source: f64) -> f64 {
        let offset = if self.is_reversed() {
            self.source_out - source
        } else {
            source - self.source_in
        };
        self.timeline_start + offset / self.speed_mag()
    }

    /// The source timestamp playing at timeline time `time` — the inverse of
    /// [`Clip::source_to_timeline`], honoring speed and reverse. Not clamped to the
    /// clip's window.
    pub fn timeline_to_source(&self, time: f64) -> f64 {
        let along = (time - self.timeline_start) * self.speed_mag();
        if self.is_reversed() {
            self.source_out - along
        } else {
            self.source_in + along
        }
    }

    /// True when any part of the source span `[from, to)` is inside this clip's
    /// source window — i.e. whether this clip actually shows that footage.
    pub fn covers_source(&self, from: f64, to: f64) -> bool {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        hi.min(self.source_out) > lo.max(self.source_in)
    }

    /// Where a source span lands on the timeline, as an ordered range. The ends
    /// are mapped through [`Clip::source_to_timeline`] and **not** clamped to the
    /// clip, so a span that starts before the in-point maps to a time before
    /// `timeline_start` — the caller decides what to do with the part that was
    /// trimmed away. A reversed clip swaps the ends, which is why the result is
    /// ordered rather than built from `from`/`to` directly.
    pub fn source_span_to_timeline(&self, from: f64, to: f64) -> TimeRange {
        let a = self.source_to_timeline(from);
        let b = self.source_to_timeline(to);
        TimeRange {
            start: a.min(b),
            end: a.max(b),
        }
    }

    /// Unused footage either side of the clip's source window, in **timeline**
    /// seconds, as `(head, tail)`: how far the clip's start / end could be pulled
    /// out before the source runs dry. A reversed clip plays the window
    /// backwards, so its start is the source's *out* side and the two swap. A
    /// still (`limit` infinite — see [`Asset::source_limit`]) has no footage to
    /// run out of, so both are unbounded.
    fn handles(&self, limit: f64) -> (f64, f64) {
        if limit.is_infinite() {
            return (f64::INFINITY, f64::INFINITY);
        }
        let mag = self.speed_mag();
        let before = self.source_in.max(0.0) / mag;
        let after = (limit - self.source_out).max(0.0) / mag;
        if self.is_reversed() {
            (after, before)
        } else {
            (before, after)
        }
    }

    /// Move the clip's end by `by` timeline seconds (positive lengthens),
    /// writing the source window the way the clip plays it: a forward clip's
    /// out-point, a reversed clip's in-point. `looping` — a still — writes only
    /// the out-point whichever way it plays: a still's window is its length, and
    /// this way it can never reach below zero.
    fn move_tail(&mut self, by: f64, looping: bool) {
        let shift = by * self.speed_mag();
        if self.is_reversed() && !looping {
            self.source_in -= shift;
        } else {
            self.source_out += shift;
        }
    }

    /// Move the clip's start by `by` timeline seconds — positive shortens it from
    /// the front, negative pulls the start earlier and the clip longer — so its
    /// **end stays where it was**. The source window follows as in
    /// [`Clip::move_tail`], and the animation rides with the content
    /// ([`Clip::rebase_animation`]).
    fn move_head(&mut self, by: f64, looping: bool) {
        self.rebase_animation(by);
        let shift = by * self.speed_mag();
        if self.is_reversed() || looping {
            self.source_out -= shift;
        } else {
            self.source_in += shift;
        }
        self.timeline_start += by;
    }

    /// Re-time the clip's animation after its start moved by `by` timeline
    /// seconds. Transform and reframe keyframes are clip-local, so when the head
    /// moves they have to move with it or the animation slides off the footage it
    /// was written against: a key that fired two seconds into the shot still
    /// fires on that moment of it.
    ///
    /// Shortened from the front (`by > 0`) the keys inside the removed span are
    /// gone, so the pose the clip now opens on is pinned as a key at 0 (what
    /// [`Timeline::slice`] does for a range export) and later keys shift back.
    /// Pulled earlier (`by < 0`) every key shifts later, and the new head holds the
    /// first key's pose — which is what interpolation does before the first key.
    fn rebase_animation(&mut self, by: f64) {
        if by > 0.0 {
            if !self.keyframes.is_empty() {
                let pose = self.transform_at(by);
                let mut kfs = vec![Keyframe::from_transform(0.0, &pose)];
                // Cut inside an eased segment: the rest of it is kept exactly. A hold keeps
                // holding to the next key; a curve's remaining pieces become plain keys (the
                // curve *is* those pieces, see `Easing`).
                let sorted = self.sorted_keyframes();
                let segment = sorted
                    .windows(2)
                    .find(|w| w[0].time <= by && by < w[1].time && w[1].time - w[0].time >= 1e-9);
                if let Some(&[a, b]) = segment {
                    match a.easing {
                        Easing::Linear => {}
                        Easing::Hold => kfs[0].easing = Easing::Hold,
                        curved => {
                            for j in 1..EASE_STEPS {
                                let u = j as f64 / EASE_STEPS as f64;
                                let at = a.time + (b.time - a.time) * u;
                                if at > by {
                                    kfs.push(a.toward(&b, curved.curve(u), at - by));
                                }
                            }
                        }
                    }
                }
                kfs.extend(
                    sorted
                        .iter()
                        .filter(|k| k.time > by)
                        .map(|k| Keyframe { time: k.time - by, ..*k }),
                );
                self.keyframes = kfs;
            }
            if self.reframe.as_ref().is_some_and(|r| r.is_animated()) {
                let pose = self.reframe_at(by).expect("clip reframes");
                let rf = self.reframe.as_mut().expect("clip reframes");
                let mut kfs = vec![ReframeKeyframe::from_pose(0.0, &pose)];
                kfs.extend(
                    rf.keyframes
                        .iter()
                        .filter(|k| k.time > by)
                        .map(|k| ReframeKeyframe { time: k.time - by, ..*k }),
                );
                rf.keyframes = kfs;
            }
        } else if by < 0.0 {
            for k in &mut self.keyframes {
                k.time -= by;
            }
            if let Some(rf) = self.reframe.as_mut() {
                for k in &mut rf.keyframes {
                    k.time -= by;
                }
            }
        }
        if by != 0.0 {
            self.rebase_channels(by);
        }
    }

    /// Hold both fades inside the clip: an edit that shortens a clip must not
    /// leave a fade longer than what is left of it. Only ever shortens one.
    fn clamp_fades(&mut self) {
        let duration = self.duration();
        self.fade_in = self.fade_in.min(duration);
        self.fade_out = self.fade_out.min(duration);
    }
}

/// Tempo estimates below this confidence are ignored when building a beat grid
/// — the same gate the timeline ruler uses to decide whether to draw beat ticks.
pub const BEAT_MIN_CONFIDENCE: f64 = 0.25;

/// Shortest clip a beat alignment may leave behind (seconds).
pub const MIN_BEAT_CLIP: f64 = 0.05;

/// The beat nearest `time` within `tolerance`, from an ascending beat grid.
pub fn nearest_beat(beats: &[f64], time: f64, tolerance: f64) -> Option<f64> {
    if tolerance <= 0.0 {
        return None;
    }
    let after = beats.partition_point(|b| *b < time);
    beats[after.saturating_sub(1)..beats.len().min(after + 1)]
        .iter()
        .copied()
        .filter(|b| (b - time).abs() <= tolerance)
        .min_by(|a, b| (a - time).abs().total_cmp(&(b - time).abs()))
}

/// Half the median beat interval — the widest tolerance that still has a single
/// answer, so every cut moves to the beat it is already closest to.
pub fn default_beat_tolerance(beats: &[f64]) -> f64 {
    let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    if gaps.is_empty() {
        return 0.0;
    }
    gaps.sort_by(f64::total_cmp);
    gaps[gaps.len() / 2] / 2.0
}

/// A single timeline lane holding clips of one kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: Uuid,
    /// When set, this track's audio is ducked under the rest of the mix on
    /// export: sidechain compression keyed by the non-ducked tracks, so e.g. a
    /// music bed dips automatically under dialogue.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub duck: bool,
    /// Silenced (audio) or hidden (video) — the track's clips are dropped before
    /// the render graph is built, so it neither exports nor previews.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
    /// Soloed. While any track of a kind is soloed, the others of that kind are
    /// treated as muted. Kinds solo independently, so soloing a music bed does
    /// not blank the picture.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub solo: bool,
    /// Locked against editing. Purely an editing guard — a locked track still
    /// renders; the GUI refuses to drag, trim or razor its clips.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// The track fader: a linear gain riding every clip on the track *after* its
    /// own volume and effect chain, the way a channel strip works — so pulling a
    /// music bed down does not change what its compressor was reacting to.
    /// 1.0 is unity. Defaulted, so a project written before there was a fader
    /// reads back at unity and renders identically.
    #[serde(default = "unity_gain", skip_serializing_if = "is_unity_gain")]
    pub volume: f32,
    /// Stereo placement, -1 (hard left) to 1 (hard right); 0 is centre. Applied
    /// as a balance (see [`Track::pan_gains`] — deliberately *not* a
    /// constant-power law), so panning a track never makes it louder — and it
    /// is a no-op on a mono delivery, where there is nowhere to pan to.
    #[serde(default, skip_serializing_if = "is_centred")]
    pub pan: f32,
    pub kind: StreamKind,
    pub name: String,
    #[serde(default)]
    pub clips: Vec<Clip>,
}

fn unity_gain() -> f32 {
    1.0
}

fn is_unity_gain(v: &f32) -> bool {
    (*v - 1.0).abs() < f32::EPSILON
}

fn is_centred(v: &f32) -> bool {
    v.abs() < f32::EPSILON
}

impl Track {
    /// An empty track: not ducked, muted, soloed or locked, at unity and centred.
    pub fn new(kind: StreamKind, name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            duck: false,
            muted: false,
            solo: false,
            locked: false,
            volume: 1.0,
            pan: 0.0,
            kind,
            name: name.into(),
            clips: Vec::new(),
        }
    }

    /// The left / right gains for this track's `pan`, as a fraction of unity.
    ///
    /// A **balance**, not a constant-power pan: the side you turn towards stays
    /// at unity and the other is attenuated away. A constant-power law would
    /// boost the near side by 3 dB at the extremes, which is right for placing a
    /// mono source in a field and wrong for leaning a finished stereo track —
    /// nudging a music bed left should not make it louder. Centre is exactly
    /// `(1, 1)`, so an untouched track is bit-for-bit what it always was.
    pub fn pan_gains(&self) -> (f64, f64) {
        let p = self.pan.clamp(-1.0, 1.0) as f64;
        if p < 0.0 {
            (1.0, 1.0 + p)
        } else {
            (1.0 - p, 1.0)
        }
    }

    /// End time of the last clip on this track (seconds).
    pub fn end(&self) -> f64 {
        self.clips.iter().map(Clip::timeline_end).fold(0.0, f64::max)
    }

    /// Recompute clip positions so the track is gapless and in clip order.
    pub fn reflow(&mut self) {
        let mut cursor = 0.0;
        for clip in &mut self.clips {
            clip.timeline_start = cursor;
            cursor += clip.duration();
        }
    }

    /// Order clips left-to-right by their timeline position. Used after a
    /// free-positioning move so the track stays a well-ordered, non-overlapping
    /// lane.
    pub fn sort_by_start(&mut self) {
        self.clips.sort_by(|a, b| a.timeline_start.total_cmp(&b.timeline_start));
    }

    /// Ripple every cut on this track onto the nearest beat within `tolerance`.
    /// Each clip is retrimmed at its outgoing edge (so the cut lands on a beat)
    /// and the rest of the track follows, keeping the original gaps — a gap's
    /// own incoming cut snaps too. `source_limit` caps each asset's source time
    /// (its duration; `INFINITY` for a still, which loops), so a clip is only
    /// stretched as far as it has footage. Returns how many cuts moved.
    pub fn align_cuts_to_beats(&mut self, beats: &[f64], tolerance: f64, source_limit: &HashMap<Uuid, f64>) -> usize {
        self.sort_by_start();
        let mut moved = 0;
        let mut cursor = 0.0; // end of the previous clip after alignment
        let mut previous_end = 0.0; // ...and where it ended before
        for clip in &mut self.clips {
            let gap = (clip.timeline_start - previous_end).max(0.0);
            previous_end = clip.timeline_end();

            let mut start = cursor + gap;
            if gap > 1e-6 {
                if let Some(beat) = nearest_beat(beats, start, tolerance) {
                    let snapped = beat.max(cursor);
                    if (snapped - start).abs() > 1e-6 {
                        moved += 1;
                        start = snapped;
                    }
                }
            }

            let speed = clip.speed_mag();
            let mut duration = clip.duration();
            if let Some(beat) = nearest_beat(beats, start + duration, tolerance) {
                let limit = source_limit.get(&clip.asset_id).copied().unwrap_or(f64::INFINITY);
                let source_left = if clip.is_reversed() {
                    clip.source_out
                } else {
                    (limit - clip.source_in).max(0.0)
                };
                let available = source_left / speed;
                let wanted = (beat - start).clamp(MIN_BEAT_CLIP, available.max(MIN_BEAT_CLIP));
                if (wanted - duration).abs() > 1e-6 {
                    moved += 1;
                    duration = wanted;
                }
            }

            clip.timeline_start = start;
            if clip.is_reversed() {
                clip.source_in = (clip.source_out - duration * speed).max(0.0);
            } else {
                clip.source_out = clip.source_in + duration * speed;
            }
            cursor = clip.timeline_end();
        }
        moved
    }
}

/// Who made an edit. The MCP server sets this to [`EditSource::Agent`]; the
/// desktop app leaves the default [`EditSource::User`]; the seq-0 baseline is
/// [`EditSource::System`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditSource {
    User,
    Agent,
    System,
}

impl EditSource {
    pub fn as_str(self) -> &'static str {
        match self {
            EditSource::User => "user",
            EditSource::Agent => "agent",
            EditSource::System => "system",
        }
    }
}

/// One entry in the timeline edit history (a stored snapshot of the timeline).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    pub seq: i64,
    pub label: String,
    pub source: EditSource,
    pub created_at: DateTime<Utc>,
    /// `true` for the revision currently applied to the live timeline.
    pub current: bool,
}

/// A batch of edits held back from the live timeline for the user to review.
///
/// This is what makes an agent safe to leave running on someone's cut: while a
/// staging session is open, every agent edit lands here instead of on the
/// timeline the user is looking at, and nothing moves under them until they
/// accept it. The user's own edits are unaffected and keep going straight to the
/// live timeline — which is what `stale` reports, since a proposal branched from
/// a cut that has since moved on would replace that newer work rather than build
/// on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedEdit {
    /// History seq the proposal was branched from.
    pub base_seq: i64,
    /// The task the agent was working when staging began, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<Uuid>,
    /// The agent's own description of what it is proposing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Labels of the individual edits, in the order they were staged.
    pub edits: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The live timeline has moved on since `base_seq`.
    pub stale: bool,
    /// What applying it would do to the cut.
    pub diff: TimelineDiff,
}

/// A named point on the timeline. Purely an annotation — it renders nothing —
/// but it gives the user and the agent a shared vocabulary for places in the
/// cut ("the laugh at 01:12"), which timestamps alone do not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: Uuid,
    /// Position on the timeline, seconds.
    pub time: f64,
    pub name: String,
    /// Optional CSS color for the ruler chip; the UI picks a default when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// How a clip's picture is fitted to an output frame of a different shape.
///
/// This is what makes a vertical or square delivery usable. `Contain` letterboxes,
/// which is right when the delivery matches the footage and wrong the moment it
/// doesn't: a 16:9 shot rendered at 1080x1920 becomes a 1080x608 strip in a
/// mostly-black frame — technically the whole picture, and not something anyone
/// would post. `Cover` fills the frame and throws away the overflow instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Scale to fit inside the frame and pad the remainder with black. The
    /// default, and the historical behaviour.
    #[default]
    Contain,
    /// Scale to cover the frame and crop what hangs over the edges.
    Cover,
}

impl Fit {
    pub fn as_str(self) -> &'static str {
        match self {
            Fit::Contain => "contain",
            Fit::Cover => "cover",
        }
    }
}

/// The frame the project is cut *for* — the shape of the thing being delivered.
///
/// Without one, a timeline's shape is whatever the first video clip happens to
/// be, and a vertical delivery exists only as a resolution typed into the export
/// dialog. That is backwards for short-form work: the 9:16 crop decides which
/// half of every shot survives, so it has to be visible while the cut is made,
/// not discovered in the rendered file. Setting this makes the preview, the
/// scrubbed still, the streamed playback and the export all render the same
/// frame — an explicit export `resolution` still wins, so nothing about the
/// existing dialog changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Delivery {
    pub width: u32,
    pub height: u32,
    /// How footage of a different shape meets that frame. `Cover` is the useful
    /// default for a reframed delivery; `Contain` keeps the whole picture.
    #[serde(default)]
    pub fit: Fit,
}

impl Delivery {
    /// Even-clamped, and never zero — the dimensions reach a filtergraph.
    pub fn new(width: u32, height: u32, fit: Fit) -> Self {
        let even = |v: u32| v.max(2) & !1;
        Self {
            width: even(width),
            height: even(height),
            fit,
        }
    }

    pub fn aspect(&self) -> f64 {
        self.width as f64 / self.height.max(1) as f64
    }

    /// The frame's shape reduced to lowest terms — `(9, 16)` for 1080x1920 —
    /// which is what a per-clip [`Framing`] is keyed by, so a 720x1280 render
    /// and a 1080x1920 one share the crop.
    pub fn ratio(&self) -> (u32, u32) {
        reduce_ratio(self.width, self.height)
    }

    /// The shape as people write it: `9:16`.
    pub fn ratio_label(&self) -> String {
        let (w, h) = self.ratio();
        format!("{w}:{h}")
    }

    /// The delivery frame a shape name stands for: `"9:16"`, `"1:1"`, `"4:5"`,
    /// `"16:9"` — the sizes the app's delivery picker uses, with the fit that
    /// picker pairs them with (a vertical or square frame fills and crops, the
    /// landscape one keeps the whole picture) — or an explicit `WxH`, which
    /// covers when it is not 16:9. `None` for anything else.
    pub fn parse(name: &str) -> Option<Delivery> {
        let name = name.trim();
        let (w, h) = match name {
            "9:16" | "vertical" | "portrait" => (1080, 1920),
            "1:1" | "square" => (1080, 1080),
            "4:5" => (1080, 1350),
            "16:9" | "landscape" => (1920, 1080),
            _ => {
                let (w, h) = name.split_once(['x', 'X', '×'])?;
                (w.trim().parse().ok()?, h.trim().parse().ok()?)
            }
        };
        if w == 0 || h == 0 {
            return None;
        }
        let fit = if reduce_ratio(w, h) == (16, 9) {
            Fit::Contain
        } else {
            Fit::Cover
        };
        Some(Delivery::new(w, h, fit))
    }
}

fn reduce_ratio(w: u32, h: u32) -> (u32, u32) {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    let d = gcd(w, h).max(1);
    (w / d, h / d)
}

/// A crop a clip carries for **one** delivery shape it is not being cut in.
///
/// Smart crop writes its result into `Transform.crop_*`, which is right for the
/// frame the project is cut for and wrong for every other: the crop that keeps
/// the subject in a 9:16 Reel throws the subject away in a 1:1 post, and
/// re-framing for the second shape overwrote the first. A `Framing` is that
/// second answer kept beside the first, keyed by the reduced shape, so one cut
/// can be delivered at several frames with each shot framed for each. It is
/// only read by [`Timeline::for_delivery`] — the ordinary render of the project
/// frame never sees it, so a project with none renders byte-identically.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Framing {
    /// The delivery shape this crop is for, in lowest terms — `(9, 16)`.
    pub aspect_w: u32,
    pub aspect_h: u32,
    /// Fraction of the source cropped from each edge.
    pub crop_left: f64,
    pub crop_right: f64,
    pub crop_top: f64,
    pub crop_bottom: f64,
}

impl Framing {
    pub fn new(ratio: (u32, u32), crop: &CropFrame) -> Self {
        Self {
            aspect_w: ratio.0,
            aspect_h: ratio.1,
            crop_left: crop.left,
            crop_right: crop.right,
            crop_top: crop.top,
            crop_bottom: crop.bottom,
        }
    }

    pub fn ratio(&self) -> (u32, u32) {
        (self.aspect_w, self.aspect_h)
    }

    pub fn ratio_label(&self) -> String {
        format!("{}:{}", self.aspect_w, self.aspect_h)
    }

    fn crop(&self) -> (f64, f64, f64, f64) {
        (self.crop_left, self.crop_right, self.crop_top, self.crop_bottom)
    }
}

/// A coarse map of where a shot's *content* is: `rows`×`cols` non-negative
/// weights sampled across a source window, row-major.
///
/// Built by [`crate::engine::salience_map`] from a handful of tiny grayscale
/// frames — per cell, the edge energy of the picture plus how much it moved.
/// That combination is what makes it usable on both kinds of shot a social cut
/// is made of: a locked-off talking head has no motion but plenty of facial
/// detail against a soft background, and a handheld follow has both. It is
/// deliberately *not* face detection — no model to ship, no licence to carry,
/// and the answer only has to be good enough to beat a centre crop, which is
/// what the alternative actually is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SalienceMap {
    pub cols: usize,
    pub rows: usize,
    /// `rows * cols` weights, row-major.
    pub cells: Vec<f32>,
}

/// How far the salient window is allowed to pull away from centre before the
/// pull has to be *earned*. Scored against the window's share of total
/// salience, so a flat map (an evenly-lit wide shot, a gradient, black) resolves
/// to the centre crop rather than to whichever edge won by rounding.
const CENTER_BIAS: f64 = 0.25;

/// Candidate window positions evaluated across the cropped axis. The window
/// edges are interpolated within a bucket, so this is finer than `cols`.
const CROP_SEARCH_STEPS: usize = 240;

/// Aspect ratios within this relative tolerance are the same shape — 1920x1080
/// into a 1280x720 frame needs no crop, and neither does 1080x1350 into 4:5.
const ASPECT_TOLERANCE: f64 = 0.01;

/// A crop window as the per-edge source fractions [`Transform`] takes.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CropFrame {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    /// How far the window sits from a plain centre crop, as a fraction of the
    /// travel available to it (0.0 = dead centre, 1.0 = hard against an edge).
    /// Reported so a caller can say *why* the shot moved.
    pub offset: f64,
}

impl CropFrame {
    /// The centred crop keeping `keep` of `axis` — what a `Cover` fit does on
    /// its own, and the answer whenever the content gives no reason to move.
    fn centered(keep: f64, horizontal: bool) -> Self {
        Self::at(0.5 * (1.0 - keep), keep, horizontal, 0.0)
    }

    fn at(start: f64, keep: f64, horizontal: bool, offset: f64) -> Self {
        let (near, far) = (start, (1.0 - start - keep).max(0.0));
        if horizontal {
            Self {
                left: near,
                right: far,
                top: 0.0,
                bottom: 0.0,
                offset,
            }
        } else {
            Self {
                left: 0.0,
                right: 0.0,
                top: near,
                bottom: far,
                offset,
            }
        }
    }

    /// Whether this window is (near enough) the plain centre crop.
    pub fn is_centered(&self) -> bool {
        self.offset.abs() < 1e-6
    }
}

/// Whether footage of this shape has to lose part of itself to fill a frame of
/// `target_aspect`. False when the two are the same shape within
/// [`ASPECT_TOLERANCE`] — 1920x1080 into a 1280x720 frame keeps all of itself —
/// and when either shape is nonsense.
pub fn needs_crop(source_w: u32, source_h: u32, target_aspect: f64) -> bool {
    let source_aspect = source_w as f64 / source_h.max(1) as f64;
    if !source_aspect.is_finite() || source_aspect <= 0.0 || !target_aspect.is_finite() || target_aspect <= 0.0 {
        return false;
    }
    ((source_aspect - target_aspect) / target_aspect).abs() > ASPECT_TOLERANCE
}

impl SalienceMap {
    pub fn new(cols: usize, rows: usize, cells: Vec<f32>) -> Self {
        Self { cols, rows, cells }
    }

    fn is_valid(&self) -> bool {
        self.cols > 0 && self.rows > 0 && self.cells.len() == self.cols * self.rows
    }

    /// Salience collapsed onto one axis: per column when `horizontal`, else per
    /// row. Negative weights are clamped away so a bad sample can't subtract.
    fn axis(&self, horizontal: bool) -> Vec<f64> {
        let n = if horizontal { self.cols } else { self.rows };
        let mut out = vec![0.0; n];
        for (i, cell) in self.cells.iter().enumerate() {
            let bucket = if horizontal { i % self.cols } else { i / self.cols };
            out[bucket] += (*cell as f64).max(0.0);
        }
        out
    }

    /// The crop that frames this shot's content for `target_aspect`.
    ///
    /// `None` when the source is already that shape — there is nothing to
    /// choose, and writing a no-op crop into every clip would only be noise in
    /// the inspector. Otherwise the long axis is cropped to the target ratio and
    /// the window is placed where the content is, which is the whole point: a
    /// 16:9 interview with the subject on the left third loses their head to a
    /// centre crop, and that is the default every other path here would take.
    pub fn crop_for(&self, source_w: u32, source_h: u32, target_aspect: f64) -> Option<CropFrame> {
        if !needs_crop(source_w, source_h, target_aspect) {
            return None;
        }
        let source_aspect = source_w as f64 / source_h.max(1) as f64;
        // Wider than the frame → crop the width; taller → crop the height.
        let horizontal = source_aspect > target_aspect;
        let keep = if horizontal {
            target_aspect / source_aspect
        } else {
            source_aspect / target_aspect
        }
        .clamp(0.01, 1.0);

        if !self.is_valid() {
            return Some(CropFrame::centered(keep, horizontal));
        }
        let weights = self.axis(horizontal);
        let total: f64 = weights.iter().sum();
        if total <= 0.0 {
            return Some(CropFrame::centered(keep, horizontal));
        }

        let travel = 1.0 - keep;
        let mut best = (f64::NEG_INFINITY, 0.5 * travel);
        for step in 0..=CROP_SEARCH_STEPS {
            let start = travel * step as f64 / CROP_SEARCH_STEPS as f64;
            let share = window_sum(&weights, start, start + keep) / total;
            let drift = ((start + 0.5 * keep) - 0.5).abs() * 2.0;
            let score = share - CENTER_BIAS * drift;
            if score > best.0 {
                best = (score, start);
            }
        }

        let start = best.1;
        // Report — and store — the exact centre when the search landed on it, so
        // an unmoved shot reads as unmoved instead of as a 0.4% pan.
        let offset = if travel > 1e-9 { (start / travel - 0.5) * 2.0 } else { 0.0 };
        if offset.abs() < 0.02 {
            return Some(CropFrame::centered(keep, horizontal));
        }
        Some(CropFrame::at(start, keep, horizontal, offset))
    }
}

/// Salience between two positions on a 0..1 axis, with the end buckets counted
/// by the fraction of them the window actually covers — so sliding the window
/// by less than a bucket changes the score smoothly instead of in steps.
fn window_sum(weights: &[f64], from: f64, to: f64) -> f64 {
    let n = weights.len() as f64;
    let (from, to) = (from.clamp(0.0, 1.0) * n, to.clamp(0.0, 1.0) * n);
    let mut sum = 0.0;
    for (i, w) in weights.iter().enumerate() {
        let (lo, hi) = (i as f64, i as f64 + 1.0);
        let overlap = to.min(hi) - from.max(lo);
        if overlap > 0.0 {
            sum += w * overlap;
        }
    }
    sum
}

/// The last stage of the mix: what every track has been summed into, before
/// the delivery's loudness normalisation.
///
/// A fader and a safety limiter, nothing more — the point is that the *finished*
/// mix can be pulled down or kept under a ceiling without touching a single
/// track, clip or effect. In the export graph it sits after the final sum (and
/// the duck bus) and **before** `loudnorm`, so a normalised delivery is
/// normalised from the level the master was left at.
///
/// Every field is defaulted, and [`MasterBus::is_default`] keeps a project that
/// never touched the master from writing a `master` key at all, so such a
/// project reads and renders exactly as it did.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MasterBus {
    /// The master fader: a linear gain on the finished mix (`1.0` is unity).
    /// [`set_master_volume`](crate::Project::set_master_volume) clamps it to
    /// `0..=`[`MASTER_MAX_VOLUME`].
    #[serde(default = "unity_master_volume")]
    pub volume: f64,
    /// A lookahead limiter holding the finished mix under `ceiling_db`.
    #[serde(default)]
    pub limiter: bool,
    /// Where the limiter stops the signal, in dBFS (so `-1.0` keeps the mix a
    /// decibel under full scale). Only read while `limiter` is on, but kept
    /// while it is off, so switching the limiter back on returns to the
    /// ceiling that was chosen.
    ///
    /// A **sample-peak** ceiling: the limiter does not oversample, so the
    /// *true* peak of the result can sit a fraction of a dB higher
    /// ([`Project::levels`](crate::Project::levels) measures it).
    #[serde(default = "default_master_ceiling")]
    pub ceiling_db: f64,
}

/// The top of the master fader: +12 dB, as far as a fader has any business going
/// (the track faders stop at the same place).
pub const MASTER_MAX_VOLUME: f64 = 4.0;
/// The lowest ceiling the limiter can be given: `alimiter` takes a linear
/// `limit` of at least 0.0625, which is -24.08 dB.
pub const MASTER_MIN_CEILING_DB: f64 = -24.0;
/// The default limiter ceiling, in dBFS: `loudnorm`'s own true-peak target. The limiter
/// holds the *sample* peak, and the peak between samples runs above it (an 11 kHz tone
/// at a -1 dBFS ceiling read -0.2 dBTP, one at 15 kHz +0.1), so the default leaves a
/// half decibel under the -1 dBTP that platforms ask for.
pub const MASTER_DEFAULT_CEILING_DB: f64 = -1.5;

fn unity_master_volume() -> f64 {
    1.0
}

fn default_master_ceiling() -> f64 {
    MASTER_DEFAULT_CEILING_DB
}

impl Default for MasterBus {
    fn default() -> Self {
        Self {
            volume: 1.0,
            limiter: false,
            ceiling_db: MASTER_DEFAULT_CEILING_DB,
        }
    }
}

impl MasterBus {
    /// Nothing set: what a project that never touched the master holds, and the
    /// only value that is not written to the `.kerf` file.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether the master leaves the mix alone — unity gain and no limiter. A
    /// neutral master adds nothing to the export graph, which is what keeps
    /// every graph from before the master existed byte-identical (a stored
    /// `ceiling_db` with the limiter off is not neutral by `is_default`, but is
    /// by this).
    pub fn is_neutral(&self) -> bool {
        !self.limiter && (self.safe_volume() - 1.0).abs() <= f64::EPSILON
    }

    /// The fader as the graph will use it: finite, within `0..=`
    /// [`MASTER_MAX_VOLUME`]. A `.kerf` file never goes through the op that
    /// clamps, and `NaN` or a negative gain must not reach `volume=`.
    pub fn safe_volume(&self) -> f64 {
        if self.volume.is_finite() {
            self.volume.clamp(0.0, MASTER_MAX_VOLUME)
        } else {
            1.0
        }
    }

    /// The ceiling as the graph will use it: finite, within
    /// [`MASTER_MIN_CEILING_DB`]`..=0`.
    pub fn safe_ceiling_db(&self) -> f64 {
        if self.ceiling_db.is_finite() {
            self.ceiling_db.clamp(MASTER_MIN_CEILING_DB, 0.0)
        } else {
            MASTER_DEFAULT_CEILING_DB
        }
    }

    /// The limiter's ceiling as a linear amplitude (`alimiter`'s `limit`).
    pub fn limit_linear(&self) -> f64 {
        10f64.powf(self.safe_ceiling_db() / 20.0)
    }

    /// The master fader in dB, or `None` at zero gain (silence).
    pub fn volume_db(&self) -> Option<f64> {
        let v = self.safe_volume();
        (v > 0.0).then(|| 20.0 * v.log10())
    }
}

fn master_is_default(master: &MasterBus) -> bool {
    master.is_default()
}

/// The non-destructive timeline (EDL): a set of multi-kind tracks, the text
/// overlays (titles / lower-thirds / captions) drawn over the composited
/// picture, and the user's markers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeline {
    pub tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TextOverlay>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    /// The frame this cut is being made for. `None` (the default, and every
    /// timeline saved before this existed) keeps the historical behaviour:
    /// the shape follows the first video clip's footage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Delivery>,
    /// The master bus: the fader and limiter the finished mix passes through.
    /// Defaulted and not written while untouched (see [`MasterBus`]).
    #[serde(default, skip_serializing_if = "master_is_default")]
    pub master: MasterBus,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Timeline {
    /// A fresh timeline with one video and one audio track.
    pub fn new() -> Self {
        Self {
            tracks: vec![Track::new(StreamKind::Video, "V1"), Track::new(StreamKind::Audio, "A1")],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: MasterBus::default(),
        }
    }

    pub fn overlay(&self, id: Uuid) -> Option<&TextOverlay> {
        self.overlays.iter().find(|o| o.id == id)
    }

    pub fn track(&self, id: Uuid) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: Uuid) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// The id of the first track of a given kind, if any.
    pub fn first_track_of(&self, kind: StreamKind) -> Option<Uuid> {
        self.tracks.iter().find(|t| t.kind == kind).map(|t| t.id)
    }

    /// Beat timestamps of the audio tracks mapped onto the timeline, ascending
    /// and de-duplicated. `tempos` supplies each asset's cached tempo; estimates
    /// below [`BEAT_MIN_CONFIDENCE`] are ignored, so a non-rhythmic source
    /// contributes nothing rather than a grid of noise.
    pub fn beat_grid(&self, tempos: &HashMap<Uuid, Tempo>) -> Vec<f64> {
        let mut times = Vec::new();
        for track in &self.tracks {
            if track.kind != StreamKind::Audio {
                continue;
            }
            for clip in &track.clips {
                let Some(tempo) = tempos.get(&clip.asset_id) else {
                    continue;
                };
                if tempo.confidence < BEAT_MIN_CONFIDENCE || tempo.bpm <= 0.0 {
                    continue;
                }
                for &beat in &tempo.beats {
                    if beat >= clip.source_in && beat <= clip.source_out {
                        times.push(clip.source_to_timeline(beat));
                    }
                }
            }
        }
        times.sort_by(f64::total_cmp);
        // Overlapping clips of one asset repeat the same beats; drop the copies.
        times.dedup_by(|a, b| (*a - *b).abs() <= 0.005);
        times
    }

    /// Find a clip by id, returning `(track_index, clip_index)`.
    pub fn locate(&self, clip_id: Uuid) -> Option<(usize, usize)> {
        for (ti, track) in self.tracks.iter().enumerate() {
            if let Some(ci) = track.clips.iter().position(|c| c.id == clip_id) {
                return Some((ti, ci));
            }
        }
        None
    }

    pub fn clip(&self, clip_id: Uuid) -> Option<&Clip> {
        self.locate(clip_id).map(|(ti, ci)| &self.tracks[ti].clips[ci])
    }

    /// Total timeline duration (seconds).
    pub fn duration(&self) -> f64 {
        self.tracks.iter().map(Track::end).fold(0.0, f64::max)
    }

    /// Whether any track of `kind` is soloed. While one is, the rest of that
    /// kind are silent/hidden — and the kinds solo independently, so soloing a
    /// music bed does not blank the picture.
    pub fn has_solo(&self, kind: StreamKind) -> bool {
        self.tracks.iter().any(|t| t.kind == kind && t.solo)
    }

    /// Whether this track's clips reach the render at all: muted tracks never
    /// do, and while any track of its kind is soloed, only the soloed ones do.
    pub fn track_renders(&self, track: &Track) -> bool {
        !track.muted && (!self.has_solo(track.kind) || track.solo)
    }

    /// The timeline as it should actually be rendered: muted (and solo-shadowed)
    /// tracks and disabled clips removed.
    ///
    /// Filtering here rather than inside the graph builders is deliberate. The
    /// export and still paths index clips by a flat position that `plan_inputs`,
    /// the `ClipFx` table and every `[v{n}]` label agree on, so dropping clips
    /// mid-graph would mean renumbering all of it. Handing those builders a
    /// timeline that simply does not contain the silenced clips keeps them — and
    /// their tests — untouched.
    ///
    /// Empty tracks are kept: a track carries `duck`, which the audio mix reads
    /// even when the track contributes nothing.
    pub fn for_render(&self) -> Timeline {
        Timeline {
            tracks: self
                .tracks
                .iter()
                .map(|track| Track {
                    clips: if self.track_renders(track) {
                        track.clips.iter().filter(|c| c.enabled).cloned().collect()
                    } else {
                        Vec::new()
                    },
                    ..track.clone()
                })
                .collect(),
            overlays: self.overlays.clone(),
            markers: self.markers.clone(),
            format: self.format,
            master: self.master,
        }
    }

    /// The same cut delivered at another frame: a copy whose format is
    /// `delivery` and whose clips wear the crop they carry for *that* shape.
    ///
    /// This is the multi-format export's whole trick, and it is the same one
    /// `for_render` uses — change the timeline, not the graph. A clip framed for
    /// the shape ([`Clip::framing_for`]) swaps that crop in for its transform's;
    /// one that never was keeps whatever crop it has, since the alternative is
    /// throwing away a crop someone made by hand. Generated captions are re-fit
    /// to the new aspect the way [`Timeline::captions`] fit them to the old one
    /// (a 9:16 frame is half as wide, and `drawtext` draws off the edge rather
    /// than wrapping); a typed title is left alone. Delivering the shape the
    /// project is already cut for changes only the size.
    pub fn for_delivery(&self, delivery: Delivery) -> Timeline {
        let ratio = delivery.ratio();
        let same_shape = self.format.is_some_and(|f| f.ratio() == ratio);
        let aspect = delivery.aspect();
        Timeline {
            tracks: self
                .tracks
                .iter()
                .map(|track| Track {
                    clips: track
                        .clips
                        .iter()
                        .map(|clip| {
                            let mut clip = clip.clone();
                            if !same_shape {
                                if let Some(f) = clip.framing_for(ratio).copied() {
                                    let t = &mut clip.transform;
                                    (t.crop_left, t.crop_right, t.crop_top, t.crop_bottom) = f.crop();
                                }
                            }
                            clip
                        })
                        .collect(),
                    ..track.clone()
                })
                .collect(),
            overlays: self
                .overlays
                .iter()
                .map(|o| {
                    let mut o = o.clone();
                    if o.generated && !same_shape {
                        o.size = fit_size(&o.text, o.size, aspect);
                    }
                    o
                })
                .collect(),
            markers: self.markers.clone(),
            format: Some(delivery),
            // The same mix at every frame: a variant changes the picture only.
            master: self.master,
        }
    }

    /// A copy containing only `[start, end)`, shifted so `start` lands at 0 —
    /// the sub-timeline a range export renders. Clips overlapping the window
    /// edges are cut down (source window and keyframes adjusted, honoring speed
    /// and reverse); fades and transitions belonging to a removed edge are
    /// dropped; overlays are clipped and shifted the same way. A clip cut at
    /// the front keeps its animated pose by sampling a replacement keyframe at
    /// the new start.
    pub fn slice(&self, start: f64, end: f64) -> Timeline {
        let mut out = Timeline {
            tracks: Vec::with_capacity(self.tracks.len()),
            overlays: Vec::new(),
            // Markers inside the window come along, shifted like everything else;
            // without this a range export would desync every one of them.
            markers: self
                .markers
                .iter()
                .filter(|m| m.time >= start && m.time < end)
                .map(|m| Marker {
                    time: m.time - start,
                    ..m.clone()
                })
                .collect(),
            // A slice is still the same delivery: range export and playback both
            // build from one, and either would otherwise fall back to footage shape.
            format: self.format,
            // ...and the same mix: a range export of a limited master is limited.
            master: self.master,
        };
        for track in &self.tracks {
            let mut t = Track {
                clips: Vec::new(),
                ..track.clone()
            };
            for clip in &track.clips {
                let (cs, ce) = (clip.timeline_start, clip.timeline_end());
                if ce <= start || cs >= end {
                    continue;
                }
                let mut c = clip.clone();
                let mag = c.speed_mag();
                let cut_front = (start - cs).max(0.0);
                let cut_back = (ce - end).max(0.0);
                if cut_front > 0.0 {
                    if c.is_reversed() {
                        c.source_out -= cut_front * mag;
                    } else {
                        c.source_in += cut_front * mag;
                    }
                    c.fade_in = 0.0;
                    c.transition_in = None;
                    // Animation re-timed to the new start, the pose it lands on pinned
                    // as a key at 0 (transform and reframe camera alike).
                    c.rebase_animation(cut_front);
                }
                if cut_back > 0.0 {
                    if c.is_reversed() {
                        c.source_in += cut_back * mag;
                    } else {
                        c.source_out -= cut_back * mag;
                    }
                    c.fade_out = 0.0;
                }
                c.timeline_start = (cs - start).max(0.0);
                t.clips.push(c);
            }
            out.tracks.push(t);
        }
        for o in &self.overlays {
            if o.end <= start || o.start >= end {
                continue;
            }
            let mut ov = o.clone();
            let cut_front = (start - o.start).max(0.0);
            if cut_front > 0.0 && !ov.keyframes.is_empty() {
                let (pos_x, pos_y, opacity) = o.sample(start);
                let mut kfs = vec![TextKeyframe {
                    time: 0.0,
                    pos_x,
                    pos_y,
                    opacity,
                }];
                kfs.extend(ov.keyframes.iter().filter(|k| k.time > cut_front).map(|k| TextKeyframe {
                    time: k.time - cut_front,
                    ..*k
                }));
                ov.keyframes = kfs;
            }
            ov.start = (o.start - start).max(0.0);
            ov.end = (o.end.min(end) - start).max(ov.start);
            out.overlays.push(ov);
        }
        out
    }
}

mod channels;
use channels::channel_changes;
pub use channels::{key_polyline, Property, PropertyKey, PropertyTrack, MAX_CHANNEL_VOLUME};

mod links;
pub use links::{Detached, DetachedMany, SkippedDetach};

mod music_fit;
pub use music_fit::{
    music_fit_clips, plan_music_fit, to_samples, MusicFit, MusicFitReport, MusicSegment, FIT_FADE_S, MAX_FIT_BARS,
    SPLICE_CROSSFADE_S,
};

// ---- ripple ----------------------------------------------------------------

impl Timeline {
    /// The cut after a **ripple edit**: `self` is what an edit left behind and
    /// `before` is what it started from. Clips are matched by id, the way
    /// [`Timeline::diff`] matches them, and each track is looked at on its own.
    ///
    /// The one idea is that an edit which changes how much footage sits ahead of
    /// a clip should carry that clip along, with every gap in front of it kept.
    /// The track's *followers* — clips the edit left starting where they started
    /// (same id, same start; a clip trimmed in place follows what was done ahead
    /// of it too) — are shifted by the net change in length of what the edit did
    /// ahead of them:
    ///
    /// | the edit… | followers at or after… | move by |
    /// |---|---|---|
    /// | changes a clip's length (right trim, speed, a left trim) | its old end | the change in its length |
    /// | removes a clip from the timeline | its old end | minus its length |
    /// | adds a clip onto footage that was there | the new clip's start | the new clip's length |
    ///
    /// Everything else is deliberately left alone:
    ///
    /// * **A left-edge trim keeps the clip's start.** A left trim is committed as
    ///   a new `source_in` *and* a later `timeline_start`, so that the right edge
    ///   stays put (the non-ripple behaviour). In a ripple edit the conventional
    ///   thing happens instead: the clip stays where it started, its length
    ///   changes, and the followers move by that change — so the result is the
    ///   same whether or not the caller moved `timeline_start`. This only applies
    ///   when the trim is the *whole* edit on the track; a bulk edit that moves
    ///   several clips has no single edit point to hold still.
    /// * **An add that fits is not an insert.** A clip put onto free space — the
    ///   end of the track, a gap it fits in — moves nothing. Only a clip that
    ///   lands on footage that was there pushes it: that footage and everything
    ///   after it moves right by the new clip's length (adjacent adds count as
    ///   one). A clip that lands in the *middle* of another cannot be resolved
    ///   without splitting it, which ripple never does, so that track is left as
    ///   the edit made it.
    /// * **Splits do not shift.** The cut-off half is an add over the footage the
    ///   shortened half just gave up, and the two cancel exactly.
    /// * **Moves do not ripple**, within a track or across tracks: a clip
    ///   arriving from or leaving for another track, or merely changing its
    ///   start, is not a change in how much footage sits ahead of anything.
    /// * **An edit that already rippled is not rippled twice.** A clip the edit
    ///   itself moved is not a follower, so `ripple_delete` and `cut_clip_range`
    ///   (whose later clips are all moved) come back unchanged.
    /// * **Tracks are independent — except for linked clips.** A ripple on `V1`
    ///   does not move the rest of `A1`, but a clip it moved takes its *linked
    ///   partners* along by the same amount (the sync lock, see
    ///   `Timeline::conform_links`); a **locked** track never moves (an edit to it
    ///   is left as made). **Overlays and markers do not move** either; they are
    ///   timeline-level, not track-level, and rippling titles can come later.
    ///
    /// **It never produces an overlap.** If shifting the followers (or restoring
    /// a left-trimmed start) would leave a clip the edit or the ripple touched
    /// overlapping another, or starting before 0, that track is returned exactly
    /// as the edit made it — no ripple for that track, rather than a broken lane.
    /// Overlaps between two clips nothing touched (an old project) are not this
    /// function's business and block nothing.
    ///
    /// Pure, and the identity when nothing about any track's timing changed.
    pub fn ripple_from(&self, before: &Timeline) -> Timeline {
        self.ripple_from_with(before, true)
    }

    /// [`Timeline::ripple_from`] with the sync lock for linked material made
    /// explicit: with `links` on, the clips the ripple moved take their linked
    /// partners on other tracks along ([`Timeline::conform_links`], best effort — a
    /// conform that would be refused leaves the plain per-track ripple, and the
    /// edit's own sync guard says why) so the picture's sound stays with it even
    /// when the sound's own track had nothing to ripple. With `links` off, tracks
    /// are independent.
    pub fn ripple_from_with(&self, before: &Timeline, links: bool) -> Timeline {
        self.ripple_from_anchored(before, links, &HashSet::new())
    }

    /// [`Timeline::ripple_from_with`] told which clips the edit **named**
    /// (`anchors`), so the sync lock knows whose track speaks for a linked group
    /// when the partners' tracks were rippled by different amounts.
    pub fn ripple_from_anchored(&self, before: &Timeline, links: bool, anchors: &HashSet<Uuid>) -> Timeline {
        let out = self.ripple_lanes(before);
        if links {
            let mut conformed = out.clone();
            if conformed
                .conform_links_noted(before, anchors, &HashMap::new(), Some(self), &mut Vec::new())
                .is_ok()
            {
                return conformed;
            }
        }
        out
    }

    /// The per-track half of [`Timeline::ripple_from`]: every unlocked track
    /// rippled on its own, nothing carried between them.
    pub fn ripple_lanes(&self, before: &Timeline) -> Timeline {
        let in_before: HashSet<Uuid> = before.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).collect();
        let in_after: HashSet<Uuid> = self.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).collect();
        let mut out = self.clone();
        for track in &mut out.tracks {
            let Some(prior) = before.track(track.id) else {
                continue;
            };
            if track.locked || prior.locked {
                continue;
            }
            if let Some(rippled) = ripple_track(track, prior, &in_before, &in_after) {
                *track = rippled;
            }
        }
        out
    }
}

/// The rippled version of one track, or `None` when there is nothing to do or
/// the ripple would leave the lane illegal. `in_before` / `in_after` hold every
/// clip id on any track of each timeline, which is what tells a clip that left
/// (or arrived) from one that was deleted (or created).
fn ripple_track(after: &Track, before: &Track, in_before: &HashSet<Uuid>, in_after: &HashSet<Uuid>) -> Option<Track> {
    let prior: HashMap<Uuid, &Clip> = before.clips.iter().map(|c| (c.id, c)).collect();
    let here: HashSet<Uuid> = after.clips.iter().map(|c| c.id).collect();

    // Clips the edit left starting where they started: they follow whatever the
    // edit did ahead of them, whether or not their own length changed too.
    // `(index in `after`, where it stood, whether it is untouched)`.
    let mut anchored: Vec<(usize, f64, bool)> = Vec::new();
    let mut resized: Vec<(usize, &Clip)> = Vec::new(); // same id, new length (and the old clip)
    let mut added: Vec<usize> = Vec::new();
    let mut other_edits = 0usize; // moves, arrivals, departures

    for (i, clip) in after.clips.iter().enumerate() {
        match prior.get(&clip.id) {
            Some(was) => {
                let same_start = !num_changed(was.timeline_start, clip.timeline_start);
                let same_length = !num_changed(was.duration(), clip.duration());
                if same_start {
                    anchored.push((i, was.timeline_start, same_length));
                }
                match (same_start, same_length) {
                    (_, false) => resized.push((i, was)),
                    (false, true) => other_edits += 1,
                    (true, true) => {}
                }
            }
            None if !in_before.contains(&clip.id) => added.push(i),
            None => other_edits += 1,
        }
    }
    let removed: Vec<&Clip> = before.clips.iter().filter(|c| !in_after.contains(&c.id)).collect();
    other_edits += before
        .clips
        .iter()
        .filter(|c| in_after.contains(&c.id) && !here.contains(&c.id))
        .count();

    // What the edit did to the amount of footage ahead of the clips after it:
    // `(where, by how much)`, in before-timeline terms.
    let mut events: Vec<(f64, f64)> = Vec::new();
    for (i, was) in &resized {
        events.push((was.timeline_end(), after.clips[*i].duration() - was.duration()));
    }
    for clip in &removed {
        events.push((clip.timeline_end(), -clip.duration()));
    }
    // Adjacent adds are one insertion; it only counts when it landed on footage
    // that was there (a clip that fits in free space moves nothing).
    let mut spans: Vec<(f64, f64)> = added
        .iter()
        .map(|&i| (after.clips[i].timeline_start, after.clips[i].timeline_end()))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut chains: Vec<(f64, f64)> = Vec::new();
    for (start, end) in spans {
        match chains.last_mut() {
            Some(last) if start <= last.1 + DIFF_EPS => last.1 = last.1.max(end),
            _ => chains.push((start, end)),
        }
    }
    for (lo, hi) in chains {
        if before
            .clips
            .iter()
            .any(|c| spans_overlap((lo, hi), (c.timeline_start, c.timeline_end())))
        {
            events.push((lo, hi - lo));
        }
    }

    let mut clips = after.clips.clone();
    // Untouched clips are pristine until shifted; everything else the edit
    // touched is not.
    let mut pristine = vec![false; clips.len()];
    let mut changed = false;

    // A left-edge trim that held the right edge still: the clip keeps its start.
    // Only when it is the whole edit — see `ripple_from`.
    if let [(i, was)] = resized.as_slice() {
        let sole = removed.is_empty() && added.is_empty() && other_edits == 0;
        if sole && !num_changed(was.timeline_end(), clips[*i].timeline_end()) {
            clips[*i].timeline_start = was.timeline_start;
            changed = true;
        }
    }

    for (i, stood, untouched) in anchored {
        pristine[i] = untouched;
        let shift: f64 = events
            .iter()
            .filter(|(at, _)| *at <= stood + DIFF_EPS)
            .map(|(_, by)| by)
            .sum();
        if shift.abs() > DIFF_EPS {
            clips[i].timeline_start += shift;
            pristine[i] = false;
            changed = true;
        }
    }
    if !changed || !lane_is_legal(&clips, &pristine) {
        return None;
    }
    let mut track = Track { clips, ..after.clone() };
    track.sort_by_start();
    Some(track)
}

/// Whether a lane is fit to keep after a ripple: nothing the edit or the ripple
/// touched starts before 0 or overlaps another clip. Two `pristine` clips
/// overlapping is old news and does not count.
fn lane_is_legal(clips: &[Clip], pristine: &[bool]) -> bool {
    if clips
        .iter()
        .zip(pristine)
        .any(|(c, pristine)| !pristine && c.timeline_start < -DIFF_EPS)
    {
        return false;
    }
    let mut order: Vec<usize> = (0..clips.len()).collect();
    order.sort_by(|&a, &b| clips[a].timeline_start.total_cmp(&clips[b].timeline_start));
    for (pos, &i) in order.iter().enumerate() {
        let end = clips[i].timeline_end();
        for &j in &order[pos + 1..] {
            if clips[j].timeline_start >= end - DIFF_EPS {
                break;
            }
            if !pristine[i] || !pristine[j] {
                return false;
            }
        }
    }
    true
}

/// Do two spans share any time? Touching end-to-start is not an overlap, and a
/// microsecond of float noise from a JSON round-trip does not count either.
fn spans_overlap(a: (f64, f64), b: (f64, f64)) -> bool {
    a.0 < b.1 - DIFF_EPS && b.0 < a.1 - DIFF_EPS
}

// ---- multi-clip edits --------------------------------------------------------

/// One clip's destination in a group move ([`Timeline::move_clips`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipMove {
    pub clip_id: Uuid,
    /// Where the clip starts afterwards (seconds) — absolute, not a delta, so a
    /// caller that snaps to frames or beats decides the exact landing spot.
    pub timeline_start: f64,
    /// The track it lands on, which must be the same kind as the one it leaves.
    /// Omitted, the clip stays on its track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<Uuid>,
}

impl Timeline {
    /// Move several clips at once, **all or nothing**: every destination is
    /// checked before anything moves, so an error leaves the timeline as it was.
    ///
    /// The group is checked as a group. Clips moving together never collide with
    /// the places they are leaving (nudging abutting clips by a second is legal,
    /// where moving them one at a time would trip over each other), but they must
    /// not overlap each other or any clip that is *not* moving, on whichever
    /// track they land. A clip may change track only to another of the same
    /// kind, a locked track — as source or destination — refuses the whole move,
    /// and a clip may appear once. A start before 0 is an error rather than a
    /// clamp: clamping each clip separately would quietly reshape the group, and
    /// the caller knows how far the group may slide.
    ///
    /// Returns the moved clips in request order. Moves never ripple.
    pub fn move_clips(&mut self, moves: &[ClipMove]) -> Result<Vec<Clip>> {
        if moves.is_empty() {
            return Err(Error::InvalidArgument("no clips to move".to_string()));
        }
        // (clip, destination track index, start, duration) — resolved up front.
        let mut plan: Vec<(Uuid, usize, f64, f64)> = Vec::with_capacity(moves.len());
        let mut moving: HashSet<Uuid> = HashSet::with_capacity(moves.len());
        for m in moves {
            if !moving.insert(m.clip_id) {
                return Err(Error::InvalidArgument(format!("clip {} appears more than once", m.clip_id)));
            }
            if !m.timeline_start.is_finite() {
                return Err(Error::InvalidArgument("timeline_start must be a finite number".to_string()));
            }
            if m.timeline_start < -DIFF_EPS {
                return Err(Error::InvalidArgument(format!(
                    "clip {} would start before the beginning of the timeline",
                    m.clip_id
                )));
            }
            let (from, ci) = self.locate(m.clip_id).ok_or(Error::ClipNotFound(m.clip_id))?;
            let to = match m.track_id {
                Some(id) => self.tracks.iter().position(|t| t.id == id).ok_or(Error::TrackNotFound(id))?,
                None => from,
            };
            if self.tracks[to].kind != self.tracks[from].kind {
                return Err(Error::InvalidArgument(
                    "cannot move a clip to a track of a different kind".to_string(),
                ));
            }
            for ti in [from, to] {
                if self.tracks[ti].locked {
                    return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[ti].name)));
                }
            }
            plan.push((
                m.clip_id,
                to,
                m.timeline_start.max(0.0),
                self.tracks[from].clips[ci].duration(),
            ));
        }

        // Where everything lands: the moved clips against the ones staying put,
        // and against each other.
        for (i, (_, to, start, duration)) in plan.iter().enumerate() {
            let span = (*start, *start + *duration);
            let track = &self.tracks[*to];
            let hits_staying = track
                .clips
                .iter()
                .any(|c| !moving.contains(&c.id) && spans_overlap(span, (c.timeline_start, c.timeline_end())));
            let hits_moving = plan
                .iter()
                .enumerate()
                .any(|(j, (_, other_to, other_start, other_duration))| {
                    j != i && other_to == to && spans_overlap(span, (*other_start, *other_start + *other_duration))
                });
            if hits_staying || hits_moving {
                return Err(Error::InvalidArgument(format!(
                    "moved clips would overlap on track {} at {}",
                    track.name,
                    fmt_time(*start)
                )));
            }
        }

        // Everything checks out: lift the clips, set them down, re-order the lanes.
        let mut lifted: HashMap<Uuid, Clip> = HashMap::with_capacity(plan.len());
        for track in &mut self.tracks {
            let (take, keep): (Vec<Clip>, Vec<Clip>) = std::mem::take(&mut track.clips)
                .into_iter()
                .partition(|c| moving.contains(&c.id));
            track.clips = keep;
            lifted.extend(take.into_iter().map(|c| (c.id, c)));
        }
        let mut landed = Vec::with_capacity(plan.len());
        for (id, to, start, _) in &plan {
            let mut clip = lifted.remove(id).expect("every planned clip was located above");
            clip.timeline_start = *start;
            landed.push(clip.clone());
            self.tracks[*to].clips.push(clip);
        }
        for (_, to, _, _) in &plan {
            self.tracks[*to].sort_by_start();
        }
        Ok(landed)
    }

    /// Remove several clips at once, all or nothing: an unknown id, or a clip on
    /// a locked track, refuses the lot. A clip named twice is removed once.
    /// Leaves gaps — under ripple mode the caller's [`Timeline::ripple_from`]
    /// closes them, per track. Returns how many clips were removed.
    pub fn remove_clips(&mut self, ids: &[Uuid]) -> Result<usize> {
        if ids.is_empty() {
            return Err(Error::InvalidArgument("no clips to remove".to_string()));
        }
        for id in ids {
            let (ti, _) = self.locate(*id).ok_or(Error::ClipNotFound(*id))?;
            if self.tracks[ti].locked {
                return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[ti].name)));
            }
        }
        let doomed: HashSet<Uuid> = ids.iter().copied().collect();
        for track in &mut self.tracks {
            track.clips.retain(|c| !doomed.contains(&c.id));
        }
        Ok(doomed.len())
    }
}

// ---- edit modes: roll, slip, slide, split-and-remove -------------------------

/// Shortest a clip an edit-mode op may leave behind (seconds) — the same floor
/// the timeline's edge-trim handles hold.
pub const MIN_EDIT_CLIP: f64 = 0.05;

/// Two clip edges closer than this are one edge. It is the engine's own test for
/// a transition partner (`transition_fx`), so a cut that blends is a cut that
/// can be rolled, and a clip that "touches" its neighbour here is one the render
/// treats as touching.
pub const ADJACENT_EPS: f64 = 1e-3;

/// How far each asset's footage reaches, for the edit modes that move a clip's
/// source window over it: [`Asset::source_limit`] per asset id (infinite for a
/// still).
pub type SourceLimits = HashMap<Uuid, f64>;

// The doc comments on this type are what an MCP client reads in the tool schema,
// so they are written for a model, not as rustdoc links.
/// Which half of a split to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SplitSide {
    /// Remove everything before the split point (trims the clip's start to it).
    Left,
    /// Remove everything after the split point (trims the clip's end to it).
    Right,
}

impl SplitSide {
    pub fn as_str(self) -> &'static str {
        match self {
            SplitSide::Left => "left",
            SplitSide::Right => "right",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "left" => Some(SplitSide::Left),
            "right" => Some(SplitSide::Right),
            _ => None,
        }
    }
}

/// What a roll, slip or slide did: how far it was asked to go, how far it
/// actually went (it clamps to the footage and the neighbours rather than
/// refusing), and the clips it changed, as they stand afterwards, in timeline
/// order.
#[derive(Debug, Clone, Serialize)]
pub struct EditOutcome {
    pub requested: f64,
    pub applied: f64,
    /// `applied` is not what was asked for.
    pub clamped: bool,
    pub clips: Vec<Clip>,
}

impl EditOutcome {
    fn new(requested: f64, applied: f64, clips: Vec<Clip>) -> Self {
        Self {
            requested,
            applied,
            clamped: num_changed(requested, applied),
            clips,
        }
    }
}

/// How far an edit-mode op may go each way, in the units of its `delta`, and
/// what stops it. The ops clamp to it, and a UI that drags one reads it to hold
/// the pointer where the edit would go no further.
#[derive(Debug, Clone, PartialEq)]
pub struct DeltaRange {
    /// Furthest it may go in the negative direction (≤ 0).
    pub min: f64,
    /// Furthest it may go in the positive direction (≥ 0).
    pub max: f64,
    why_min: String,
    why_max: String,
}

impl DeltaRange {
    fn open() -> Self {
        Self {
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
            why_min: String::new(),
            why_max: String::new(),
        }
    }

    /// Cap the positive direction at `room` seconds (a negative room is none).
    fn up_to(mut self, room: f64, why: impl Into<String>) -> Self {
        let room = room.max(0.0);
        if room < self.max {
            self.max = room;
            self.why_max = why.into();
        }
        self
    }

    /// Cap the negative direction at `room` seconds (a magnitude).
    fn down_to(mut self, room: f64, why: impl Into<String>) -> Self {
        let room = -room.max(0.0);
        if room > self.min {
            self.min = room;
            self.why_min = why.into();
        }
        self
    }

    /// `delta` held inside the range. When that leaves nothing to do it is an
    /// error that names what stopped it, so an edit that would change nothing
    /// never records a revision.
    fn resolve(&self, delta: f64, verb: &str) -> Result<f64> {
        let applied = delta.clamp(self.min, self.max);
        if applied.abs() > DIFF_EPS {
            return Ok(applied);
        }
        let (way, why) = if delta > 0.0 {
            ("later", &self.why_max)
        } else {
            ("earlier", &self.why_min)
        };
        Err(Error::InvalidArgument(format!("cannot {verb} {way}: {why}")))
    }
}

/// A `delta` is a finite, non-zero number of seconds.
fn check_delta(delta: f64) -> Result<()> {
    if !delta.is_finite() {
        return Err(Error::InvalidArgument("delta must be a finite number of seconds".to_string()));
    }
    if delta.abs() <= DIFF_EPS {
        return Err(Error::InvalidArgument("delta is zero — there is nothing to move".to_string()));
    }
    Ok(())
}

fn footage_of(footage: &SourceLimits, clip: &Clip) -> Result<f64> {
    footage
        .get(&clip.asset_id)
        .copied()
        .ok_or(Error::AssetNotFound(clip.asset_id))
}

/// One clip's cut in a group split-and-remove ([`Timeline::split_remove_clips`]):
/// the clip and the timeline time it is cut at.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipCut {
    pub clip_id: Uuid,
    /// Timeline seconds, inside the clip.
    pub at: f64,
}

/// A slide's neighbour on one side: its index in the lane and whether it touches
/// the clip.
type Neighbour = Option<(usize, bool)>;

/// Close a cut exactly. A roll or a slide computes one side of a cut from a source
/// window (`start + (out - in) / speed`) and the other from `start + delta`, and the
/// two disagree by a few ulps (±7e-15 s in about one case in thirteen): invisible to
/// a render, but a strict overlap test — `Project::move_clip`'s — reads it as one
/// clip lying on the next. So when `follower` starts where `leader` ends to within
/// float noise ([`DIFF_EPS`]), it is set to start *exactly* at `leader.timeline_end()`,
/// the very expression the overlap checks compare against. A genuine gap or overlap
/// (a cut that was merely within [`ADJACENT_EPS`]) is real data and is left alone.
fn weld(leader: &Clip, follower: &mut Clip) {
    let end = leader.timeline_end();
    if (follower.timeline_start - end).abs() <= DIFF_EPS {
        follower.timeline_start = end;
    }
}

/// [`weld`]'s mirror, for the edge the edit left where it was: `clip`'s far end
/// meets a clip that did not move (it starts at `limit`), and the window arithmetic
/// can leave `clip.timeline_end()` a few ulps *past* it (observed up to ~6e-14 s).
/// That overshoot, if it is float noise ([`DIFF_EPS`]), is taken back by shortening the
/// window point the end is written on by what it overshoots — and by a single ulp when
/// that is too small to move the point — until the end is no longer past `limit`: a
/// change of ~1e-14 s to the source window, on a clip whose neighbour cannot be moved
/// to meet it. `looping` as in [`Clip::move_tail`].
fn fit_end(clip: &mut Clip, limit: f64, looping: bool) {
    for _ in 0..16 {
        let over = clip.timeline_end() - limit;
        if over <= 0.0 || over > DIFF_EPS {
            return;
        }
        let by = over * clip.speed_mag();
        if clip.is_reversed() && !looping {
            // The end of a reversed clip is its in-point: raise it to shorten.
            let moved = clip.source_in + by;
            clip.source_in = if moved == clip.source_in {
                clip.source_in.next_up()
            } else {
                moved
            };
        } else {
            let moved = clip.source_out - by;
            clip.source_out = if moved == clip.source_out {
                clip.source_out.next_down()
            } else {
                moved
            };
        }
    }
}

impl Timeline {
    /// Find a clip for an edit: it must exist and its track must not be locked.
    fn editable_clip(&self, clip_id: Uuid) -> Result<(usize, usize)> {
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        if self.tracks[ti].locked {
            return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[ti].name)));
        }
        Ok((ti, ci))
    }

    /// Where the first clip of lane `ti` that starts at or after `end` (less the
    /// adjacency tolerance) starts, skipping the lane indices in `skip` — the clip an
    /// edit's far edge runs into, which the edit did not move.
    fn start_after(&self, ti: usize, end: f64, skip: &[usize]) -> Option<f64> {
        self.tracks[ti]
            .clips
            .iter()
            .enumerate()
            .filter(|(i, c)| !skip.contains(i) && c.timeline_start >= end - ADJACENT_EPS)
            .map(|(_, c)| c.timeline_start)
            .min_by(f64::total_cmp)
    }

    // ---- roll ---------------------------------------------------------------

    /// Everything a roll needs, checked: the lane, the two clips' indices in it,
    /// and how far the cut may move.
    fn roll_plan(&self, clip_a: Uuid, clip_b: Uuid, footage: &SourceLimits) -> Result<(usize, usize, usize, DeltaRange)> {
        if clip_a == clip_b {
            return Err(Error::InvalidArgument("a cut lies between two different clips".to_string()));
        }
        let (ta, ia) = self.editable_clip(clip_a)?;
        let (tb, ib) = self.editable_clip(clip_b)?;
        if ta != tb {
            return Err(Error::InvalidArgument(format!(
                "clips are on different tracks ({} and {}) — a roll moves the cut between two clips of one track",
                self.tracks[ta].name, self.tracks[tb].name
            )));
        }
        let (a, b) = (&self.tracks[ta].clips[ia], &self.tracks[ta].clips[ib]);
        let gap = b.timeline_start - a.timeline_end();
        if gap.abs() >= ADJACENT_EPS {
            return Err(Error::InvalidArgument(
                if (a.timeline_start - b.timeline_end()).abs() < ADJACENT_EPS {
                    "clip_a must be the earlier clip: a roll moves the cut where clip_a ends and clip_b begins".to_string()
                } else if gap > 0.0 {
                    format!("the clips are not adjacent — there is a {gap:.2}s gap between them, and a roll needs a shared cut")
                } else {
                    format!(
                        "the clips are not adjacent — they overlap by {:.2}s, and a roll needs a shared cut",
                        -gap
                    )
                },
            ));
        }
        let (_, tail_a) = a.handles(footage_of(footage, a)?);
        let (head_b, _) = b.handles(footage_of(footage, b)?);
        let range = DeltaRange::open()
            .up_to(tail_a, "the outgoing clip has no footage left to extend into")
            .up_to(b.duration() - MIN_EDIT_CLIP, "the incoming clip would be shorter than 0.05s")
            .down_to(a.duration() - MIN_EDIT_CLIP, "the outgoing clip would be shorter than 0.05s")
            .down_to(head_b, "the incoming clip has no footage left to extend into");
        Ok((ta, ia, ib, range))
    }

    /// How far the cut between `clip_a` and `clip_b` may move each way — the
    /// bounds [`Timeline::roll_edit`] clamps to.
    pub fn roll_range(&self, clip_a: Uuid, clip_b: Uuid, footage: &SourceLimits) -> Result<DeltaRange> {
        self.roll_plan(clip_a, clip_b, footage).map(|plan| plan.3)
    }

    /// **Roll** the cut between two adjacent clips: `clip_a`'s end and `clip_b`'s
    /// start both move by `delta` seconds (positive is later), so the pair covers
    /// the same stretch of timeline and nothing after it moves. The footage
    /// changes hands at the cut — one clip gains what the other gives up.
    ///
    /// `clip_a` must be the earlier clip and the two must touch (within
    /// [`ADJACENT_EPS`]) on one unlocked track. The roll **clamps** rather than
    /// refuses — to the footage each clip has left to extend into (honoring speed
    /// and direction: a reversed clip's outgoing edge is its in-point, and a still
    /// has footage without limit) and to [`MIN_EDIT_CLIP`] for the clip that
    /// shrinks — and errors only when the clamp leaves nothing to move.
    ///
    /// Everything else about both clips is kept. `clip_b`'s head moves, so its
    /// keyframes shift with the content (`Clip::rebase_animation`); `clip_a`'s
    /// head does not, so its animation stays. Fades are held inside what is left
    /// of a clip that shrank. A transition into `clip_b` stays with the cut it
    /// blends. All or nothing: an error leaves the timeline untouched.
    pub fn roll_edit(&mut self, clip_a: Uuid, clip_b: Uuid, delta: f64, footage: &SourceLimits) -> Result<EditOutcome> {
        check_delta(delta)?;
        let (ti, ia, ib, range) = self.roll_plan(clip_a, clip_b, footage)?;
        let applied = range.resolve(delta, "roll the cut")?;
        let (mut a, mut b) = (self.tracks[ti].clips[ia].clone(), self.tracks[ti].clips[ib].clone());
        let (looping_a, looping_b) = (footage_of(footage, &a)?.is_infinite(), footage_of(footage, &b)?.is_infinite());
        let far = b.timeline_end();
        a.move_tail(applied, looping_a);
        b.move_head(applied, looping_b);
        weld(&a, &mut b);
        if let Some(limit) = self.start_after(ti, far, &[ia, ib]) {
            fit_end(&mut b, limit, looping_b);
        }
        a.clamp_fades();
        b.clamp_fades();
        self.tracks[ti].clips[ia] = a.clone();
        self.tracks[ti].clips[ib] = b.clone();
        Ok(EditOutcome::new(delta, applied, vec![a, b]))
    }

    // ---- slip ---------------------------------------------------------------

    fn slip_plan(&self, clip_id: Uuid, footage: &SourceLimits) -> Result<(usize, usize, DeltaRange)> {
        let (ti, ci) = self.editable_clip(clip_id)?;
        let clip = &self.tracks[ti].clips[ci];
        let limit = footage_of(footage, clip)?;
        if limit.is_infinite() {
            return Err(Error::InvalidArgument(
                "a still image has no footage to slip — it looks the same at every moment".to_string(),
            ));
        }
        // Source seconds the window could move earlier / later.
        let earlier = clip.source_in.max(0.0);
        let later = (limit - clip.source_out).max(0.0);
        let (no_later, no_earlier) = (
            "there is no footage left after the clip's out-point",
            "there is no footage left before the clip's in-point",
        );
        // Positive `delta` is "starts later in its own footage", which is the
        // *lower* source time for a reversed clip: the two directions swap.
        let range = if clip.is_reversed() {
            DeltaRange::open().up_to(earlier, no_earlier).down_to(later, no_later)
        } else {
            DeltaRange::open().up_to(later, no_later).down_to(earlier, no_earlier)
        };
        Ok((ti, ci, range))
    }

    /// How far `clip_id`'s footage may slip each way, in source seconds — the
    /// bounds [`Timeline::slip_clip`] clamps to. Errors for a still, which has
    /// nothing to slip.
    pub fn slip_range(&self, clip_id: Uuid, footage: &SourceLimits) -> Result<DeltaRange> {
        self.slip_plan(clip_id, footage).map(|plan| plan.2)
    }

    /// **Slip** a clip: change which part of its footage it shows without moving
    /// it or changing its length. The source window (`source_in` / `source_out`)
    /// shifts by `delta` **source** seconds — not timeline seconds, so at 2× speed
    /// a 1 s slip moves the picture half a second — and nothing on the timeline
    /// moves.
    ///
    /// Positive `delta` means the clip starts **later in its own footage**: you
    /// see material that comes later. For a clip playing forward that moves the
    /// window up (`source_in` and `source_out` both grow). A reversed clip plays
    /// its window backwards, so later in its footage is *lower* source time: the
    /// window moves the mirrored way (both shrink). The sign therefore always
    /// means the same thing on screen, whichever way the clip plays.
    ///
    /// Clamps to the asset's footage (the window cannot leave `0..duration`) and
    /// errors when that leaves nothing to move. A still has no footage to slip and
    /// is an error. The keyframes (transform and reframe) are clip-local and the
    /// timing is unchanged, so they stay exactly as they are; so do the fades.
    pub fn slip_clip(&mut self, clip_id: Uuid, delta: f64, footage: &SourceLimits) -> Result<EditOutcome> {
        check_delta(delta)?;
        let (ti, ci, range) = self.slip_plan(clip_id, footage)?;
        let applied = range.resolve(delta, "slip the footage")?;
        let clip = &mut self.tracks[ti].clips[ci];
        let shift = if clip.is_reversed() { -applied } else { applied };
        clip.source_in += shift;
        clip.source_out += shift;
        Ok(EditOutcome::new(delta, applied, vec![clip.clone()]))
    }

    // ---- slide --------------------------------------------------------------

    /// The clip's place in its lane — `(track, clip, previous, next)`, the
    /// neighbours by start time and whether each touches the clip — and how far
    /// it may slide.
    fn slide_plan(&self, clip_id: Uuid, footage: &SourceLimits) -> Result<(usize, usize, Neighbour, Neighbour, DeltaRange)> {
        let (ti, ci) = self.editable_clip(clip_id)?;
        let clips = &self.tracks[ti].clips;
        let clip = &clips[ci];
        let mut order: Vec<usize> = (0..clips.len()).collect();
        order.sort_by(|&a, &b| clips[a].timeline_start.total_cmp(&clips[b].timeline_start));
        let pos = order.iter().position(|&i| i == ci).expect("the clip is in its own lane");
        let prev = pos
            .checked_sub(1)
            .map(|p| order[p])
            .map(|i| (i, (clip.timeline_start - clips[i].timeline_end()).abs() < ADJACENT_EPS));
        let next = order
            .get(pos + 1)
            .map(|&i| (i, (clips[i].timeline_start - clip.timeline_end()).abs() < ADJACENT_EPS));

        let mut range = DeltaRange::open();
        // Later: a touching previous clip grows to fill what the clip leaves, a
        // touching next clip is pushed back; a next clip across a gap is left alone.
        if let Some((i, true)) = prev {
            let (_, tail) = clips[i].handles(footage_of(footage, &clips[i])?);
            range = range.up_to(tail, "the previous clip has no footage left to extend into");
        }
        match next {
            Some((i, true)) => {
                range = range.up_to(
                    clips[i].duration() - MIN_EDIT_CLIP,
                    "the next clip would be shorter than 0.05s",
                );
            }
            Some((i, false)) => {
                range = range.up_to(
                    clips[i].timeline_start - clip.timeline_end(),
                    "the next clip is not touching this one, so it is left alone and the clip can only use the free space before it",
                );
            }
            None => {}
        }
        // Earlier: the mirror image.
        match prev {
            Some((i, true)) => {
                range = range.down_to(
                    clips[i].duration() - MIN_EDIT_CLIP,
                    "the previous clip would be shorter than 0.05s",
                );
            }
            Some((i, false)) => {
                range = range.down_to(
                    clip.timeline_start - clips[i].timeline_end(),
                    "the previous clip is not touching this one, so it is left alone and the clip can only use the free space after it",
                );
            }
            None => {
                range = range.down_to(clip.timeline_start, "the clip is already at the start of the timeline");
            }
        }
        if let Some((i, true)) = next {
            let (head, _) = clips[i].handles(footage_of(footage, &clips[i])?);
            range = range.down_to(head, "the next clip has no footage left to extend into");
        }
        Ok((ti, ci, prev, next, range))
    }

    /// How far `clip_id` may slide each way, in timeline seconds — the bounds
    /// [`Timeline::slide_clip`] clamps to.
    pub fn slide_range(&self, clip_id: Uuid, footage: &SourceLimits) -> Result<DeltaRange> {
        self.slide_plan(clip_id, footage).map(|plan| plan.4)
    }

    /// **Slide** a clip along its track, keeping its content: it moves by `delta`
    /// timeline seconds (positive is later) and the neighbours give way — the
    /// previous clip's end and the next clip's start both move by `delta`, so the
    /// clip's own source window, its length and the length of the stretch it and
    /// its neighbours span are unchanged.
    ///
    /// The exact rules, for a lane with gaps:
    ///
    /// * A neighbour that **touches** the clip (within [`ADJACENT_EPS`]) follows
    ///   its edge: sliding later extends the previous clip and trims the next;
    ///   sliding earlier does the reverse. It is bounded by that neighbour's
    ///   footage (`Clip::handles`, speed and direction honored, a still
    ///   unbounded) when it grows and by [`MIN_EDIT_CLIP`] when it shrinks.
    /// * A neighbour **across a gap** is never touched — the slide cannot trim a
    ///   clip it is not touching. The clip moves through the free space and stops
    ///   where it would meet that neighbour.
    /// * With no previous clip the clip cannot go before 0; with no next clip it
    ///   can slide later without limit — and then the track's end moves with it,
    ///   the one case where the overall length changes, since there is no next
    ///   clip to give way.
    ///
    /// Like the other modes it **clamps** and errors only when nothing can move.
    /// The slid clip is only repositioned, so its keyframes and fades stay; a next
    /// clip whose start moved has its keyframes shift with its content
    /// (`Clip::rebase_animation`). All or nothing.
    pub fn slide_clip(&mut self, clip_id: Uuid, delta: f64, footage: &SourceLimits) -> Result<EditOutcome> {
        check_delta(delta)?;
        let (ti, ci, prev, next, range) = self.slide_plan(clip_id, footage)?;
        let applied = range.resolve(delta, "slide the clip")?;
        let mut moved = self.tracks[ti].clips[ci].clone();
        // Where the edit's far edge was — the end of the last clip it changes — and
        // which clips are the edit's own, so what that edge runs into can be found.
        let mut far = moved.timeline_end();
        let mut skip = vec![ci];
        moved.timeline_start += applied;
        let mut prev_clip = None;
        if let Some((i, true)) = prev {
            let mut p = self.tracks[ti].clips[i].clone();
            let looping = footage_of(footage, &p)?.is_infinite();
            p.move_tail(applied, looping);
            weld(&p, &mut moved);
            p.clamp_fades();
            skip.push(i);
            prev_clip = Some((i, p));
        }
        let mut next_clip = None;
        if let Some((i, true)) = next {
            let mut n = self.tracks[ti].clips[i].clone();
            let looping = footage_of(footage, &n)?.is_infinite();
            far = n.timeline_end();
            n.move_head(applied, looping);
            weld(&moved, &mut n);
            n.clamp_fades();
            skip.push(i);
            next_clip = Some((i, n, looping));
        }
        if let Some(limit) = self.start_after(ti, far, &skip) {
            match next_clip.as_mut() {
                Some((_, n, looping)) => fit_end(n, limit, *looping),
                None => {
                    let looping = footage.get(&moved.asset_id).is_some_and(|l| l.is_infinite());
                    fit_end(&mut moved, limit, looping);
                }
            }
        }
        let mut out = Vec::new();
        if let Some((i, p)) = prev_clip {
            self.tracks[ti].clips[i] = p.clone();
            out.push(p);
        }
        self.tracks[ti].clips[ci] = moved.clone();
        out.push(moved);
        if let Some((i, n, _)) = next_clip {
            self.tracks[ti].clips[i] = n.clone();
            out.push(n);
        }
        Ok(EditOutcome::new(delta, applied, out))
    }

    // ---- split and remove ---------------------------------------------------

    /// **Split and remove**: cut a clip at timeline time `at` and throw one half
    /// away — "trim the start / end to the playhead" in one edit. Returns the
    /// half that stays, which **keeps the clip's id** (so a selection survives and
    /// [`Timeline::ripple_from`] reads the edit for what it is, an ordinary trim).
    ///
    /// Leaving `Right` shortens the clip at its end. Leaving `Left`, the clip's
    /// start moves up to `at` so its end stays put — the same shape a left-edge
    /// trim has — which leaves the gap where the removed half was; under ripple
    /// mode the caller's `ripple_from` closes it, holding the clip's start and
    /// pulling the rest of the track in (see [`Timeline::ripple_from`]).
    ///
    /// The clip keeps what belonged to the half that stayed and drops what
    /// belonged to the half that went: a new hard edge carries no fade and no
    /// transition, so removing the left drops `fade_in` and `transition_in` (the
    /// transition blended the old start, which is gone — the same half a plain
    /// split leaves it off), and removing the right drops `fade_out`; the other
    /// fade is held inside what is left. Keyframes ride with the content when the
    /// head moved. `at` must lie inside the clip and leave at least
    /// [`MIN_EDIT_CLIP`] of it; the track must be unlocked.
    pub fn split_remove(&mut self, clip_id: Uuid, at: f64, side: SplitSide) -> Result<Clip> {
        if !at.is_finite() {
            return Err(Error::InvalidArgument("the split point must be a finite time".to_string()));
        }
        let (ti, ci) = self.editable_clip(clip_id)?;
        let mut clip = self.tracks[ti].clips[ci].clone();
        let (start, end) = (clip.timeline_start, clip.timeline_end());
        if at <= start + DIFF_EPS || at >= end - DIFF_EPS {
            return Err(Error::InvalidArgument(format!(
                "the split point {} is not inside the clip ({}–{})",
                fmt_time(at),
                fmt_time(start),
                fmt_time(end)
            )));
        }
        let kept = match side {
            SplitSide::Left => end - at,
            SplitSide::Right => at - start,
        };
        if kept < MIN_EDIT_CLIP - DIFF_EPS {
            return Err(Error::InvalidArgument(format!(
                "that would leave only {kept:.2}s of the clip — remove the clip instead"
            )));
        }
        match side {
            SplitSide::Left => {
                clip.move_head(at - start, false);
                clip.timeline_start = at;
                clip.fade_in = 0.0;
                clip.transition_in = None;
            }
            SplitSide::Right => {
                clip.move_tail(at - end, false);
                clip.fade_out = 0.0;
            }
        }
        clip.clamp_fades();
        self.tracks[ti].clips[ci] = clip.clone();
        Ok(clip)
    }
}

impl Timeline {
    /// **Split and remove** on several clips at once — the playhead trim of a
    /// selection, V1 and its A1 partner together — as one edit: every cut in `cuts`
    /// is [`Timeline::split_remove`] with the same `side`, and the survivors come
    /// back in request order.
    ///
    /// All or nothing: an unknown clip, a locked track, a cut outside its clip or
    /// one that would leave under [`MIN_EDIT_CLIP`] refuses the whole group and the
    /// timeline is exactly as it was. **At most one clip per track**, and a clip once:
    /// a playhead is inside one clip of a lane, and under ripple mode a lane trimmed
    /// at two places has no single edit point to hold still (see
    /// [`Timeline::ripple_from`]) — so each track ripples on its own, from its own
    /// one cut. Different tracks may be cut at different times.
    pub fn split_remove_clips(&mut self, cuts: &[ClipCut], side: SplitSide) -> Result<Vec<Clip>> {
        if cuts.is_empty() {
            return Err(Error::InvalidArgument("no clips to cut".to_string()));
        }
        let mut seen_clips = HashSet::new();
        let mut seen_tracks = HashSet::new();
        for cut in cuts {
            let (ti, _) = self.locate(cut.clip_id).ok_or(Error::ClipNotFound(cut.clip_id))?;
            if !seen_clips.insert(cut.clip_id) {
                return Err(Error::InvalidArgument(format!("clip {} appears more than once", cut.clip_id)));
            }
            if !seen_tracks.insert(ti) {
                return Err(Error::InvalidArgument(format!(
                    "two of the clips are on track {} — a group trim cuts one clip per track",
                    self.tracks[ti].name
                )));
            }
        }
        // Cut a copy, so a refusal part-way through leaves the timeline untouched.
        let mut scratch = self.clone();
        let kept = cuts
            .iter()
            .map(|cut| scratch.split_remove(cut.clip_id, cut.at, side))
            .collect::<Result<Vec<_>>>()?;
        *self = scratch;
        Ok(kept)
    }
}

/// Lifecycle of a task in the agent queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Waiting for an agent to claim it.
    Queued,
    /// Claimed by an agent and in progress.
    Working,
    /// The agent finished; the resulting edit is staged for the user to review.
    Ready,
    /// Reviewed and accepted by the user.
    Done,
    /// The agent could not complete it.
    Failed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Working => "working",
            TaskStatus::Ready => "ready",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => TaskStatus::Queued,
            "working" => TaskStatus::Working,
            "ready" => TaskStatus::Ready,
            "done" => TaskStatus::Done,
            "failed" => TaskStatus::Failed,
            _ => return None,
        })
    }
}

/// A unit of work in the agent queue. A human (or a planning agent) enqueues a
/// `prompt`; a connected LLM claims it over MCP, performs timeline edits through
/// the same engine the GUI uses, then marks it `ready` (or `failed`). Kerf never
/// edits on its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub prompt: String,
    pub status: TaskStatus,
    /// The agent's summary on completion, or the error message on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---- diff ------------------------------------------------------------------

/// Below this, two timeline values are the same value — the timeline round-trips
/// through JSON on every edit, so an exact compare would be honest but noisy.
const DIFF_EPS: f64 = 1e-6;

fn num_changed(a: f64, b: f64) -> bool {
    (a - b).abs() > DIFF_EPS
}

/// `m:ss.d`, the way an editor reads a timeline position.
pub(crate) fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0);
    let m = (s / 60.0).floor();
    format!("{}:{:04.1}", m as i64, s - m * 60.0)
}

/// A signed shift of footage at the precision a slip is made at — a frame is 0.03 s,
/// which one decimal would print as `+0.0s` — and three decimals when even two would
/// round a real shift to nothing.
fn fmt_shift(secs: f64) -> String {
    let secs = secs + 0.0; // -0.0 is 0
    let places = if secs != 0.0 && (secs.abs() * 100.0).round() == 0.0 {
        3
    } else {
        2
    };
    format!("{}{secs:.places$}s", if secs >= 0.0 { "+" } else { "" })
}

fn fmt_delta(secs: f64) -> String {
    if secs >= 0.0 {
        format!("+{secs:.1}s")
    } else {
        format!("{secs:.1}s")
    }
}

/// What one [`DiffEntry`] is about. The UI groups and tints by this; the kinds
/// are deliberately editorial (a *retrim* is a different thing to review than a
/// *move*) rather than one generic "clip changed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    TrackAdded,
    TrackRemoved,
    TrackChanged,
    ClipAdded,
    ClipRemoved,
    ClipMoved,
    ClipRetrimmed,
    ClipChanged,
    OverlayAdded,
    OverlayRemoved,
    OverlayChanged,
    MarkerAdded,
    MarkerRemoved,
    MarkerChanged,
    FormatChanged,
    MasterChanged,
}

impl DiffKind {
    /// Whether this entry adds, removes, or alters something — the three tints a
    /// diff needs.
    pub fn polarity(self) -> &'static str {
        match self {
            DiffKind::TrackAdded | DiffKind::ClipAdded | DiffKind::OverlayAdded | DiffKind::MarkerAdded => "added",
            DiffKind::TrackRemoved | DiffKind::ClipRemoved | DiffKind::OverlayRemoved | DiffKind::MarkerRemoved => "removed",
            _ => "changed",
        }
    }
}

/// One change between two timelines, phrased for a human reviewing a cut.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffEntry {
    pub kind: DiffKind,
    /// One line, e.g. `Trimmed clip on V1 at 0:04.0 — 4.0s → 2.5s (-1.5s)`.
    pub summary: String,
    /// Field-level specifics behind a `*Changed` entry, e.g. `volume 100% → 40%`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_id: Option<Uuid>,
    /// Where on the timeline to look, so the reviewer can jump straight to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
}

impl DiffEntry {
    fn new(kind: DiffKind, summary: String) -> Self {
        Self {
            kind,
            summary,
            detail: None,
            track_id: None,
            clip_id: None,
            at: None,
        }
    }

    fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    fn on_track(mut self, id: Uuid) -> Self {
        self.track_id = Some(id);
        self
    }

    fn on_clip(mut self, id: Uuid) -> Self {
        self.clip_id = Some(id);
        self
    }

    fn at(mut self, time: f64) -> Self {
        self.at = Some(time);
        self
    }
}

/// What a set of edits did to a cut: the individual changes plus the two numbers
/// an editor checks first — how long it is now, and how many clips it has.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineDiff {
    pub entries: Vec<DiffEntry>,
    pub duration_before: f64,
    pub duration_after: f64,
    pub clips_before: usize,
    pub clips_after: usize,
}

impl TimelineDiff {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A single line: how many changes and what they did to the runtime.
    pub fn headline(&self) -> String {
        if self.entries.is_empty() {
            return "No changes".to_string();
        }
        let n = self.entries.len();
        let mut s = format!("{n} change{}", if n == 1 { "" } else { "s" });
        if num_changed(self.duration_before, self.duration_after) {
            s.push_str(&format!(
                " · {} → {} ({})",
                fmt_time(self.duration_before),
                fmt_time(self.duration_after),
                fmt_delta(self.duration_after - self.duration_before)
            ));
        } else {
            s.push_str(&format!(" · {}", fmt_time(self.duration_after)));
        }
        if self.clips_before != self.clips_after {
            s.push_str(&format!(" · {} → {} clips", self.clips_before, self.clips_after));
        }
        s
    }

    /// The headline followed by one line per change — what an agent reads back
    /// and what the review card renders.
    pub fn summary(&self) -> String {
        let mut out = self.headline();
        for e in &self.entries {
            out.push_str("\n  • ");
            out.push_str(&e.summary);
            if let Some(d) = &e.detail {
                out.push_str(" (");
                out.push_str(d);
                out.push(')');
            }
        }
        out
    }
}

fn clip_index(timeline: &Timeline) -> HashMap<Uuid, (&Track, &Clip)> {
    let mut map = HashMap::new();
    for track in &timeline.tracks {
        for clip in &track.clips {
            map.insert(clip.id, (track, clip));
        }
    }
    map
}

fn kind_name(kind: StreamKind) -> &'static str {
    match kind {
        StreamKind::Video => "video",
        StreamKind::Audio => "audio",
        StreamKind::Subtitle => "subtitle",
        StreamKind::Data => "data",
    }
}

fn joined(parts: Vec<String>) -> Option<String> {
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

fn track_changes(before: &Track, after: &Track) -> Option<String> {
    let mut parts = Vec::new();
    if before.name != after.name {
        parts.push(format!("renamed {} → {}", before.name, after.name));
    }
    for (was, is, on, off) in [
        (before.muted, after.muted, "muted", "unmuted"),
        (before.solo, after.solo, "soloed", "unsoloed"),
        (before.locked, after.locked, "locked", "unlocked"),
        (before.duck, after.duck, "ducking on", "ducking off"),
    ] {
        if was != is {
            parts.push((if is { on } else { off }).to_string());
        }
    }
    // The mixer strip: without these an agent proposal that only rides a fader
    // or a pan diffs as empty and apply_staged throws it away.
    if (before.volume - after.volume).abs() > DIFF_EPS as f32 {
        parts.push(format!("level {:.0}% → {:.0}%", before.volume * 100.0, after.volume * 100.0));
    }
    if (before.pan - after.pan).abs() > DIFF_EPS as f32 {
        parts.push(format!("pan {:.2} → {:.2}", before.pan, after.pan));
    }
    joined(parts)
}

fn transform_changes(before: &Transform, after: &Transform) -> Vec<String> {
    let mut parts = Vec::new();
    for (label, a, b) in [
        ("scale", before.scale, after.scale),
        ("x", before.pos_x, after.pos_x),
        ("y", before.pos_y, after.pos_y),
        ("rotation", before.rotation, after.rotation),
        ("opacity", before.opacity, after.opacity),
    ] {
        if num_changed(a, b) {
            parts.push(format!("{label} {a:.2} → {b:.2}"));
        }
    }
    if before.has_crop() != after.has_crop()
        || num_changed(before.crop_left, after.crop_left)
        || num_changed(before.crop_right, after.crop_right)
        || num_changed(before.crop_top, after.crop_top)
        || num_changed(before.crop_bottom, after.crop_bottom)
    {
        parts.push(if after.has_crop() { "cropped" } else { "crop cleared" }.to_string());
    }
    parts
}

fn color_changes(before: &Color, after: &Color) -> Vec<String> {
    let mut parts = Vec::new();
    for (label, a, b) in [
        ("brightness", before.brightness, after.brightness),
        ("contrast", before.contrast, after.contrast),
        ("saturation", before.saturation, after.saturation),
        ("gamma", before.gamma, after.gamma),
        ("temperature", before.temperature, after.temperature),
    ] {
        if num_changed(a, b) {
            parts.push(format!("{label} {a:.2} → {b:.2}"));
        }
    }
    parts
}

fn effect_list<T>(effects: &[T], name: impl Fn(&T) -> &'static str) -> String {
    if effects.is_empty() {
        "none".to_string()
    } else {
        effects.iter().map(name).collect::<Vec<_>>().join("+")
    }
}

fn reframe_changes(before: Option<&Reframe>, after: Option<&Reframe>) -> Vec<String> {
    match (before, after) {
        (None, None) => Vec::new(),
        (None, Some(_)) => vec!["reframe added".to_string()],
        (Some(_), None) => vec!["reframe cleared".to_string()],
        (Some(a), Some(b)) => {
            if a == b {
                return Vec::new();
            }
            let mut parts = Vec::new();
            for (label, x, y) in [
                ("yaw", a.yaw, b.yaw),
                ("pitch", a.pitch, b.pitch),
                ("roll", a.roll, b.roll),
                ("fov", a.fov, b.fov),
            ] {
                if num_changed(x, y) {
                    parts.push(format!("{label} {x:.0}° → {y:.0}°"));
                }
            }
            if a.keyframes.len() != b.keyframes.len() {
                parts.push(format!("reframe keyframes {} → {}", a.keyframes.len(), b.keyframes.len()));
            } else if a.keyframes != b.keyframes {
                parts.push("reframe keyframes retimed".to_string());
            }
            if parts.is_empty() {
                parts.push("reframe changed".to_string());
            }
            parts
        }
    }
}

fn clip_changes(before: &Clip, after: &Clip) -> Option<String> {
    let mut parts = Vec::new();
    if before.asset_id != after.asset_id {
        parts.push("different source asset".to_string());
    }
    if (before.volume - after.volume).abs() > DIFF_EPS as f32 {
        parts.push(format!("volume {:.0}% → {:.0}%", before.volume * 100.0, after.volume * 100.0));
    }
    for (label, a, b) in [
        ("fade in", before.fade_in, after.fade_in),
        ("fade out", before.fade_out, after.fade_out),
    ] {
        if num_changed(a, b) {
            parts.push(format!("{label} {a:.2}s → {b:.2}s"));
        }
    }
    if num_changed(before.speed, after.speed) {
        parts.push(format!("speed {:.2}× → {:.2}×", before.speed, after.speed));
    }
    if before.enabled != after.enabled {
        parts.push(if after.enabled { "re-enabled" } else { "disabled" }.to_string());
    }
    // Links and the detached-sound flag are edits too: without these a proposal
    // that only links two clips diffs as empty and `apply_staged` discards it.
    if before.source_audio != after.source_audio {
        parts.push(if after.source_audio { "own sound on" } else { "own sound off" }.to_string());
    }
    if before.link_id != after.link_id {
        parts.push(
            match (before.link_id, after.link_id) {
                (_, None) => "unlinked",
                (None, Some(_)) => "linked",
                (Some(_), Some(_)) => "relinked",
            }
            .to_string(),
        );
    }
    parts.extend(transform_changes(&before.transform, &after.transform));
    parts.extend(color_changes(&before.color, &after.color));
    // A framing pass before a multi-format export is an edit like any other,
    // and one an agent proposal has to be able to show.
    let framed: Vec<String> = after
        .framings
        .iter()
        .filter(|f| before.framing_for(f.ratio()) != Some(*f))
        .map(Framing::ratio_label)
        .collect();
    if !framed.is_empty() {
        parts.push(format!("framed for {}", framed.join(", ")));
    }
    if before.transition_in != after.transition_in {
        parts.push(match &after.transition_in {
            None => "transition removed".to_string(),
            Some(t) => format!("transition {} {:.2}s", t.kind.as_str(), t.duration),
        });
    }
    if before.effects != after.effects {
        parts.push(format!(
            "video effects {} → {}",
            effect_list(&before.effects, VideoEffect::name),
            effect_list(&after.effects, VideoEffect::name)
        ));
    }
    if before.audio != after.audio {
        parts.push(format!(
            "audio effects {} → {}",
            effect_list(&before.audio, AudioEffect::name),
            effect_list(&after.audio, AudioEffect::name)
        ));
    }
    if before.keyframes.len() != after.keyframes.len() {
        parts.push(format!("keyframes {} → {}", before.keyframes.len(), after.keyframes.len()));
    } else if before.keyframes != after.keyframes {
        // Keys that only leave their poses differently were not moved or retimed.
        let plain = |k: &Keyframe| Keyframe {
            easing: Easing::Linear,
            ..*k
        };
        let pairs = || before.keyframes.iter().zip(&after.keyframes);
        if pairs().any(|(a, b)| plain(a) != plain(b)) {
            parts.push("keyframes retimed".to_string());
        }
        let eased = pairs().filter(|(a, b)| a.easing != b.easing).count();
        if eased > 0 {
            parts.push(format!(
                "easing changed on {eased} keyframe{}",
                if eased == 1 { "" } else { "s" }
            ));
        }
    }
    parts.extend(channel_changes(before, after));
    parts.extend(reframe_changes(before.reframe.as_ref(), after.reframe.as_ref()));
    if before.mask != after.mask {
        parts.push(match &after.mask {
            None => "mask cleared".to_string(),
            Some(m) => format!("masked ({})", m.shape.as_str()),
        });
    }
    joined(parts)
}

fn overlay_changes(before: &TextOverlay, after: &TextOverlay) -> Option<String> {
    let mut parts = Vec::new();
    if before.text != after.text {
        parts.push(format!("text “{}” → “{}”", before.text, after.text));
    }
    if num_changed(before.start, after.start) || num_changed(before.end, after.end) {
        parts.push(format!(
            "timing {}–{} → {}–{}",
            fmt_time(before.start),
            fmt_time(before.end),
            fmt_time(after.start),
            fmt_time(after.end)
        ));
    }
    if num_changed(before.pos_x, after.pos_x) || num_changed(before.pos_y, after.pos_y) {
        parts.push(format!(
            "position {:.2},{:.2} → {:.2},{:.2}",
            before.pos_x, before.pos_y, after.pos_x, after.pos_y
        ));
    }
    if num_changed(before.size, after.size) {
        parts.push(format!("size {:.3} → {:.3}", before.size, after.size));
    }
    if before.color != after.color {
        parts.push(format!("color {} → {}", before.color, after.color));
    }
    if before.bg != after.bg {
        parts.push("box changed".to_string());
    }
    if before.font != after.font {
        parts.push("font changed".to_string());
    }
    if before.bold != after.bold {
        parts.push(if after.bold { "bold" } else { "not bold" }.to_string());
    }
    if before.keyframes != after.keyframes {
        parts.push(format!("keyframes {} → {}", before.keyframes.len(), after.keyframes.len()));
    }
    joined(parts)
}

/// What differs between two master buses, phrased like the track strip's
/// (`level 100% → 70%`), or `None` when the mix would come out the same. The
/// ceiling only counts while the limiter is on afterwards — a stored ceiling
/// the limiter is not using changes nothing anyone can hear.
fn master_changes(before: &MasterBus, after: &MasterBus) -> Option<String> {
    let mut parts = Vec::new();
    if (before.volume - after.volume).abs() > DIFF_EPS {
        parts.push(format!("level {:.0}% → {:.0}%", before.volume * 100.0, after.volume * 100.0));
    }
    if before.limiter != after.limiter {
        parts.push(if after.limiter {
            format!("limiter on at {:.1} dB", after.ceiling_db)
        } else {
            "limiter off".to_string()
        });
    } else if after.limiter && (before.ceiling_db - after.ceiling_db).abs() > DIFF_EPS {
        parts.push(format!(
            "limiter ceiling {:.1} → {:.1} dB",
            before.ceiling_db, after.ceiling_db
        ));
    }
    joined(parts)
}

fn fmt_delivery(d: &Delivery) -> String {
    format!("{}x{} ({})", d.width, d.height, d.fit.as_str())
}

impl Timeline {
    /// How many clips stand somewhere else here than in `before`: a different
    /// start on their track, or a different track. Matched by id, so a clip that
    /// exists on only one side is not counted. This is how a caller learns whether
    /// an edit *rippled* — ripple is the only thing that moves a clip a remove or a
    /// retime did not touch — rather than assuming it did because ripple mode was
    /// on: a locked track never moves, and a ripple that would leave a lane
    /// overlapping is declined, both silently.
    pub fn clips_moved_since(&self, before: &Timeline) -> usize {
        let was = clip_index(before);
        clip_index(self)
            .into_iter()
            .filter(|(id, (track, clip))| {
                was.get(id)
                    .is_some_and(|(t, c)| t.id != track.id || num_changed(c.timeline_start, clip.timeline_start))
            })
            .count()
    }

    /// What changed between this timeline and `after`, phrased for a human
    /// reviewing a cut.
    ///
    /// Everything is matched by id — clips keep theirs across a move, a trim and
    /// a retime — so a reordered track reads as the handful of moves it is
    /// rather than as every clip having been replaced. Pure, and the single
    /// source of truth behind both the staged-edit review card and
    /// [`crate::project::Project::diff_revisions`].
    pub fn diff(&self, after: &Timeline) -> TimelineDiff {
        let before_clips = clip_index(self);
        let after_clips = clip_index(after);

        let mut tracks = Vec::new();
        let mut clips = Vec::new();

        for track in &after.tracks {
            match self.track(track.id) {
                None => tracks.push(
                    DiffEntry::new(
                        DiffKind::TrackAdded,
                        format!("Added {} track {}", kind_name(track.kind), track.name),
                    )
                    .on_track(track.id),
                ),
                Some(before) => {
                    if let Some(detail) = track_changes(before, track) {
                        tracks.push(
                            DiffEntry::new(DiffKind::TrackChanged, format!("Changed track {}", track.name))
                                .detail(detail)
                                .on_track(track.id),
                        );
                    }
                }
            }
        }
        for track in &self.tracks {
            if after.track(track.id).is_none() {
                let n = track.clips.len();
                tracks.push(
                    DiffEntry::new(
                        DiffKind::TrackRemoved,
                        format!(
                            "Removed {} track {} ({n} clip{})",
                            kind_name(track.kind),
                            track.name,
                            if n == 1 { "" } else { "s" }
                        ),
                    )
                    .on_track(track.id),
                );
            }
        }

        // Clips whose whole track went away are already covered by the track
        // entry above; listing each of them again would bury the one change the
        // reviewer actually has to judge.
        for track in &self.tracks {
            if after.track(track.id).is_none() {
                continue;
            }
            for clip in &track.clips {
                if after_clips.contains_key(&clip.id) {
                    continue;
                }
                clips.push(
                    DiffEntry::new(
                        DiffKind::ClipRemoved,
                        format!(
                            "Removed clip from {} at {} ({:.1}s)",
                            track.name,
                            fmt_time(clip.timeline_start),
                            clip.duration()
                        ),
                    )
                    .on_track(track.id)
                    .on_clip(clip.id)
                    .at(clip.timeline_start),
                );
            }
        }

        for track in &after.tracks {
            for clip in &track.clips {
                let Some((before_track, before_clip)) = before_clips.get(&clip.id) else {
                    clips.push(
                        DiffEntry::new(
                            DiffKind::ClipAdded,
                            format!(
                                "Added clip to {} at {} ({:.1}s)",
                                track.name,
                                fmt_time(clip.timeline_start),
                                clip.duration()
                            ),
                        )
                        .on_track(track.id)
                        .on_clip(clip.id)
                        .at(clip.timeline_start),
                    );
                    continue;
                };
                if before_track.id != track.id {
                    clips.push(
                        DiffEntry::new(
                            DiffKind::ClipMoved,
                            format!(
                                "Moved clip from {} to {} at {}",
                                before_track.name,
                                track.name,
                                fmt_time(clip.timeline_start)
                            ),
                        )
                        .on_track(track.id)
                        .on_clip(clip.id)
                        .at(clip.timeline_start),
                    );
                } else if num_changed(before_clip.timeline_start, clip.timeline_start) {
                    clips.push(
                        DiffEntry::new(
                            DiffKind::ClipMoved,
                            format!(
                                "Moved clip on {} — {} → {}",
                                track.name,
                                fmt_time(before_clip.timeline_start),
                                fmt_time(clip.timeline_start)
                            ),
                        )
                        .on_track(track.id)
                        .on_clip(clip.id)
                        .at(clip.timeline_start),
                    );
                }
                if num_changed(before_clip.source_in, clip.source_in) || num_changed(before_clip.source_out, clip.source_out) {
                    let (was, is) = (before_clip.source_duration(), clip.source_duration());
                    // A window that moved without changing length is a slip, not a
                    // trim: "4.0s → 4.0s (+0.0s)" would read as nothing having happened.
                    let summary = if num_changed(was, is) {
                        format!(
                            "Trimmed clip on {} at {} — {was:.1}s → {is:.1}s ({})",
                            track.name,
                            fmt_time(clip.timeline_start),
                            fmt_delta(is - was)
                        )
                    } else {
                        // Signed as `slip_clip` documents it: + is *later in its own
                        // footage*, which for a reversed clip is the window moving down.
                        let moved = clip.source_in - before_clip.source_in;
                        let later = if clip.is_reversed() { -moved } else { moved };
                        format!(
                            "Slipped clip on {} at {} — footage {} (in-point {:.2}s → {:.2}s)",
                            track.name,
                            fmt_time(clip.timeline_start),
                            fmt_shift(later),
                            before_clip.source_in,
                            clip.source_in
                        )
                    };
                    clips.push(
                        DiffEntry::new(DiffKind::ClipRetrimmed, summary)
                            .on_track(track.id)
                            .on_clip(clip.id)
                            .at(clip.timeline_start),
                    );
                }
                if let Some(detail) = clip_changes(before_clip, clip) {
                    clips.push(
                        DiffEntry::new(
                            DiffKind::ClipChanged,
                            format!("Adjusted clip on {} at {}", track.name, fmt_time(clip.timeline_start)),
                        )
                        .detail(detail)
                        .on_track(track.id)
                        .on_clip(clip.id)
                        .at(clip.timeline_start),
                    );
                }
            }
        }

        let mut rest = Vec::new();
        for overlay in &after.overlays {
            match self.overlay(overlay.id) {
                None => rest.push(
                    DiffEntry::new(
                        DiffKind::OverlayAdded,
                        format!(
                            "Added text “{}” at {}–{}",
                            overlay.text,
                            fmt_time(overlay.start),
                            fmt_time(overlay.end)
                        ),
                    )
                    .at(overlay.start),
                ),
                Some(before) => {
                    if let Some(detail) = overlay_changes(before, overlay) {
                        rest.push(
                            DiffEntry::new(DiffKind::OverlayChanged, format!("Changed text “{}”", overlay.text))
                                .detail(detail)
                                .at(overlay.start),
                        );
                    }
                }
            }
        }
        for overlay in &self.overlays {
            if after.overlay(overlay.id).is_none() {
                rest.push(
                    DiffEntry::new(
                        DiffKind::OverlayRemoved,
                        format!("Removed text “{}” at {}", overlay.text, fmt_time(overlay.start)),
                    )
                    .at(overlay.start),
                );
            }
        }

        let before_markers: HashMap<Uuid, &Marker> = self.markers.iter().map(|m| (m.id, m)).collect();
        for marker in &after.markers {
            match before_markers.get(&marker.id) {
                None => rest.push(
                    DiffEntry::new(
                        DiffKind::MarkerAdded,
                        format!("Added marker “{}” at {}", marker.name, fmt_time(marker.time)),
                    )
                    .at(marker.time),
                ),
                Some(before) => {
                    let mut parts = Vec::new();
                    if before.name != marker.name {
                        parts.push(format!("renamed “{}” → “{}”", before.name, marker.name));
                    }
                    if num_changed(before.time, marker.time) {
                        parts.push(format!("moved {} → {}", fmt_time(before.time), fmt_time(marker.time)));
                    }
                    if before.color != marker.color {
                        parts.push("recolored".to_string());
                    }
                    if let Some(detail) = joined(parts) {
                        rest.push(
                            DiffEntry::new(DiffKind::MarkerChanged, format!("Changed marker “{}”", marker.name))
                                .detail(detail)
                                .at(marker.time),
                        );
                    }
                }
            }
        }
        for marker in &self.markers {
            if !after.markers.iter().any(|m| m.id == marker.id) {
                rest.push(
                    DiffEntry::new(
                        DiffKind::MarkerRemoved,
                        format!("Removed marker “{}” at {}", marker.name, fmt_time(marker.time)),
                    )
                    .at(marker.time),
                );
            }
        }

        if self.format != after.format {
            let summary = match (&self.format, &after.format) {
                (None, Some(d)) => format!("Set the delivery frame to {}", fmt_delivery(d)),
                (Some(d), None) => format!("Cleared the delivery frame (was {})", fmt_delivery(d)),
                (Some(a), Some(b)) => format!("Delivery frame {} → {}", fmt_delivery(a), fmt_delivery(b)),
                (None, None) => unreachable!(),
            };
            rest.push(DiffEntry::new(DiffKind::FormatChanged, summary));
        }
        // The master bus: without this an agent proposal that only rides the
        // master fader or turns the limiter on diffs as empty, and apply_staged
        // throws it away (the same trap the track faders had).
        if let Some(detail) = master_changes(&self.master, &after.master) {
            rest.push(DiffEntry::new(DiffKind::MasterChanged, "Changed the master bus".to_string()).detail(detail));
        }

        tracks.append(&mut clips);
        tracks.append(&mut rest);
        TimelineDiff {
            entries: tracks,
            duration_before: self.duration(),
            duration_after: after.duration(),
            clips_before: before_clips.len(),
            clips_after: after_clips.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chroma_grid_follows_the_pixel_format() {
        let sub = |w, h| Some(Subsampling { log2_w: w, log2_h: h });
        for (name, want) in [
            ("yuv420p", sub(1, 1)),
            ("yuvj420p", sub(1, 1)),
            ("yuv420p10le", sub(1, 1)),
            ("yuv420p16le", sub(1, 1)),
            ("nv12", sub(1, 1)),
            ("p010le", sub(1, 1)),
            ("yuv422p", sub(1, 0)),
            ("yuv422p10le", sub(1, 0)),
            ("nv16", sub(1, 0)),
            ("p210le", sub(1, 0)),
            ("yuv440p", sub(0, 1)),
            ("yuv411p", sub(2, 0)),
            ("yuv410p", sub(2, 2)),
            ("yuv444p", sub(0, 0)),
            ("yuv444p12le", sub(0, 0)),
            ("p410le", sub(0, 0)),
            ("gray", sub(0, 0)),
            ("gray10le", sub(0, 0)),
            ("gbrp", sub(0, 0)),
            ("rgb24", sub(0, 0)),
            ("bgr0", sub(0, 0)),
            ("rgb48le", sub(0, 0)),
            // Not byte-aligned, palettised, packed YCbCr, alpha, nonsense: not known.
            ("rgb565le", None),
            ("rgb4", None),
            ("pal8", None),
            ("uyvy422", None),
            ("yuva420p", None),
            ("yuv420p11le", None),
            ("", None),
        ] {
            assert_eq!(pix_fmt_subsampling(name), want, "{name}");
        }
        // The rounding itself: down to a whole chroma sample.
        assert_eq!((Subsampling::YUV420.round_w(11), Subsampling::YUV420.round_h(7)), (10, 6));
        assert_eq!((Subsampling::NONE.round_w(11), Subsampling::NONE.round_h(7)), (11, 7));
        assert_eq!(Subsampling { log2_w: 2, log2_h: 0 }.round_w(11), 8);
    }

    #[test]
    fn the_pixel_format_allow_list_knows_opaque_formats_and_nothing_else() {
        use PixLayout::*;
        for (name, layout) in [
            ("yuv420p", Yuv420),
            ("yuvj420p", Yuv420),
            ("yuv420p10le", Yuv420),
            ("yuv420p10be", Yuv420),
            ("yuv420p9le", Yuv420),
            ("nv12", Yuv420),
            ("nv21", Yuv420),
            ("p010le", Yuv420),
            ("yuv420p12le", OtherYuv),
            ("yuv420p16le", OtherYuv),
            ("p016le", OtherYuv),
            ("yuv422p", OtherYuv),
            ("yuv444p", OtherYuv),
            ("yuvj444p", OtherYuv),
            ("yuv422p10le", OtherYuv),
            ("yuv444p16le", OtherYuv),
            ("yuv411p", OtherYuv),
            ("yuyv422", OtherYuv),
            ("gray", Gray),
            ("gray10le", Gray),
            ("gray16le", Gray),
            ("rgb24", Rgb),
            ("bgr24", Rgb),
            ("bgr0", Rgb),
            ("0rgb", Rgb),
            ("rgb48le", Rgb),
            ("gbrp", Rgb),
            ("gbrp10le", Rgb),
            ("rgb565le", Rgb),
            ("x2rgb10le", Rgb),
        ] {
            assert_eq!(pix_fmt_layout(name), Some(layout), "{name}");
            assert!(!pix_fmt_has_alpha(name), "{name} is opaque");
        }
        // Alpha-carrying, palettised and unlisted names are all `None` — including the
        // ones a deny-list forgot (`ayuv`, `vuya`, the `rgb32` aliases).
        for name in [
            "yuva420p",
            "yuva444p10le",
            "gbrap",
            "gbrap16le",
            "rgba",
            "bgra",
            "argb",
            "abgr",
            "rgba64le",
            "ya8",
            "ya16le",
            "ayuv",
            "vuya",
            "ayuv64le",
            "rgb32",
            "bgr32",
            "pal8",
            "grayf32le",
            "something_new",
            "",
        ] {
            assert_eq!(pix_fmt_layout(name), None, "{name}");
        }
        for name in [
            "yuva420p", "gbrap", "rgba", "ayuv", "vuya", "rgb32", "bgr32", "pal8", "ya8", "argb64le",
        ] {
            assert!(pix_fmt_has_alpha(name), "{name} carries alpha");
        }
        // `has_alpha` is three-valued: yes, no, and "not recorded or not known".
        let mut s = StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(1),
            height: Some(1),
            fps: None,
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: None,
            color_space: None,
        };
        assert_eq!(s.has_alpha(), None);
        s.pix_fmt = Some("yuv420p".into());
        assert_eq!(s.has_alpha(), Some(false));
        s.pix_fmt = Some("ayuv".into());
        assert_eq!(s.has_alpha(), Some(true));
        s.pix_fmt = Some("something_new".into());
        assert_eq!(s.has_alpha(), None);
    }

    #[test]
    fn a_stream_saved_before_rotation_and_colour_were_probed_still_loads_as_sdr() {
        let old = r#"{"index":0,"kind":"video","codec":"hevc","width":1920,"height":1080,"fps":30.0}"#;
        let s: StreamInfo = serde_json::from_str(old).unwrap();
        assert_eq!((s.rotation, s.color_transfer.clone(), s.hdr()), (0, None, None));
        // And an ordinary stream serializes exactly as it did.
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"index":0,"kind":"video","codec":"hevc","width":1920,"height":1080,"fps":30.0}"#
        );
    }

    #[test]
    fn hdr_is_a_property_of_video_streams_with_an_hdr_transfer() {
        let mut s: StreamInfo = serde_json::from_str(r#"{"index":0,"kind":"video","codec":"hevc"}"#).unwrap();
        for (tag, want) in [
            ("arib-std-b67", Some(Hdr::Hlg)),
            ("smpte2084", Some(Hdr::Pq)),
            ("bt709", None),
            ("smpte170m", None),
        ] {
            s.color_transfer = Some(tag.into());
            assert_eq!(s.hdr(), want, "{tag}");
        }
        s.color_transfer = Some("arib-std-b67".into());
        s.kind = StreamKind::Audio;
        assert_eq!(s.hdr(), None);
    }

    #[test]
    fn a_proxied_asset_is_sdr_and_keeps_the_rest_of_its_metadata() {
        let streams = r#"[{"index":0,"kind":"video","codec":"hevc","width":1080,"height":1920,"rotation":-90,"color_transfer":"arib-std-b67","color_primaries":"bt2020"}]"#;
        let asset = Asset {
            id: Uuid::new_v4(),
            path: "/a.mov".into(),
            name: "a.mov".into(),
            duration: 3.0,
            streams: serde_json::from_str(streams).unwrap(),
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
        };
        assert_eq!(asset.hdr(), Some(Hdr::Hlg));
        let proxied = asset.as_sdr_proxy();
        assert_eq!(proxied.hdr(), None);
        assert_eq!(
            (
                proxied.streams[0].width,
                proxied.streams[0].height,
                proxied.streams[0].rotation
            ),
            (Some(1080), Some(1920), -90)
        );
        assert_eq!(asset.hdr(), Some(Hdr::Hlg), "the original is untouched");
    }

    fn clip_at(start: f64, dur: f64) -> Clip {
        Clip::new(Uuid::new_v4(), 0.0, dur, start)
    }

    fn track(kind: StreamKind, name: &str, clips: Vec<Clip>) -> Track {
        Track {
            clips,
            ..Track::new(kind, name)
        }
    }

    #[test]
    fn a_zoom_is_animated_only_when_the_keyed_scale_moves() {
        let key = |time: f64, scale: f64, pos_x: f64| Keyframe {
            time,
            scale,
            pos_x,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        };
        let mut clip = clip_at(0.0, 4.0);
        // Nothing keyed, and one key (a held pose): the picture never changes size.
        assert!(!clip.zoom_animated());
        clip.keyframes = vec![key(0.0, 1.5, 0.0)];
        assert!(!clip.zoom_animated());
        // Position moves, scale does not: animated, but not a zoom.
        clip.keyframes = vec![key(0.0, 0.5, -0.2), key(2.0, 0.5, 0.2)];
        assert!(clip.is_animated() && !clip.zoom_animated());
        // Float noise is not a zoom (it cannot change the size by a pixel).
        clip.keyframes = vec![key(0.0, 0.5, 0.0), key(2.0, 0.5 + 1e-12, 0.0)];
        assert!(!clip.zoom_animated());
        // Any key off the first one is, wherever it sits and whatever the order.
        clip.keyframes = vec![key(0.0, 0.5, 0.0), key(1.0, 0.5, 0.0), key(2.0, 0.75, 0.0)];
        assert!(clip.zoom_animated());
        clip.keyframes = vec![key(2.0, 0.75, 0.0), key(0.0, 0.5, 0.0)];
        assert!(clip.zoom_animated());
        // Held against the first key in *time*: a stored order that puts a later key first
        // gives the same answer, at the edge of the tolerance too.
        clip.keyframes = vec![key(1.0, 0.5 + 0.8e-9, 0.0), key(0.0, 0.5, 0.0), key(2.0, 0.5 + 1.6e-9, 0.0)];
        assert!(
            clip.zoom_animated(),
            "1.6e-9 from the first key in time, though 0.8e-9 from the stored first"
        );
        clip.keyframes = vec![key(1.0, 0.5 + 0.8e-9, 0.0), key(0.0, 0.5, 0.0)];
        assert!(!clip.zoom_animated());
    }

    #[test]
    fn beat_grid_maps_source_beats_onto_the_timeline() {
        // A music clip cut from 2s into the source and placed at 10s: a beat at
        // source 3.0 is heard at 11.0, and beats outside the window never sound.
        let asset = Uuid::new_v4();
        let clip = Clip::new(asset, 2.0, 6.0, 10.0);
        let tl = Timeline {
            tracks: vec![track(StreamKind::Audio, "A1", vec![clip])],
            ..Timeline::new()
        };
        let tempos = HashMap::from([(
            asset,
            Tempo {
                bpm: 120.0,
                beats: vec![0.0, 1.0, 3.0, 5.0, 8.0],
                confidence: 0.5,
            },
        )]);
        assert_eq!(tl.beat_grid(&tempos), vec![11.0, 13.0]);
    }

    #[test]
    fn beat_grid_ignores_a_low_confidence_tempo() {
        let asset = Uuid::new_v4();
        let tl = Timeline {
            tracks: vec![track(StreamKind::Audio, "A1", vec![Clip::new(asset, 0.0, 4.0, 0.0)])],
            ..Timeline::new()
        };
        let tempos = HashMap::from([(
            asset,
            Tempo {
                bpm: 120.0,
                beats: vec![0.5, 1.0],
                confidence: BEAT_MIN_CONFIDENCE - 0.01,
            },
        )]);
        assert!(tl.beat_grid(&tempos).is_empty());
    }

    #[test]
    fn nearest_beat_takes_the_closest_within_tolerance() {
        let beats = [0.0, 0.5, 1.0, 1.5];
        assert_eq!(nearest_beat(&beats, 0.6, 0.25), Some(0.5));
        assert_eq!(nearest_beat(&beats, 0.9, 0.25), Some(1.0));
        assert_eq!(nearest_beat(&beats, 0.75, 0.1), None);
        assert_eq!(nearest_beat(&beats, 9.0, 0.25), None);
        assert_eq!(default_beat_tolerance(&beats), 0.25);
    }

    #[test]
    fn align_cuts_to_beats_ripples_every_cut_onto_the_grid() {
        let asset = Uuid::new_v4();
        let beats: Vec<f64> = (0..=20).map(|i| i as f64 * 0.5).collect();
        let limits = HashMap::from([(asset, 10.0)]);
        let mut t = track(
            StreamKind::Video,
            "V1",
            vec![Clip::new(asset, 0.0, 1.1, 0.0), Clip::new(asset, 4.0, 4.9, 1.1)],
        );

        assert_eq!(t.align_cuts_to_beats(&beats, 0.25, &limits), 2);
        assert_eq!(t.clips[0].timeline_start, 0.0);
        assert_eq!(t.clips[0].source_out, 1.0, "the first cut moved back onto the beat");
        assert_eq!(t.clips[1].timeline_start, 1.0, "the next clip rippled with it");
        assert_eq!(t.clips[1].timeline_end(), 2.0, "and its own cut landed on a beat too");
        assert_eq!(t.clips[1].source_in, 4.0, "trimming happens at the outgoing edge");

        // Already aligned: nothing moves, so running it twice is a no-op.
        assert_eq!(t.align_cuts_to_beats(&beats, 0.25, &limits), 0);
    }

    #[test]
    fn align_cuts_to_beats_keeps_gaps_and_respects_the_source() {
        let asset = Uuid::new_v4();
        let beats: Vec<f64> = (0..=20).map(|i| i as f64 * 0.5).collect();
        // Only 1.2s of footage left after source_in, so the clip cannot stretch
        // to the 1.5 beat its end is nearest.
        let limits = HashMap::from([(asset, 1.2)]);
        let mut t = track(
            StreamKind::Video,
            "V1",
            vec![Clip::new(asset, 0.0, 0.4, 0.0), Clip::new(asset, 0.0, 1.4, 0.9)],
        );

        t.align_cuts_to_beats(&beats, 0.25, &limits);
        assert_eq!(t.clips[0].timeline_end(), 0.5);
        assert_eq!(t.clips[1].timeline_start, 1.0, "the 0.5s gap survived, snapped to a beat");
        assert_eq!(t.clips[1].source_out, 1.2, "stretched only as far as there is footage");
    }

    #[test]
    fn align_cuts_to_beats_trims_a_reversed_clip_at_its_outgoing_edge() {
        // Played backwards the timeline tail is the *start* of the source, so
        // shortening the clip must move source_in, not source_out.
        let asset = Uuid::new_v4();
        let beats: Vec<f64> = (0..=20).map(|i| i as f64 * 0.5).collect();
        let mut clip = Clip::new(asset, 1.0, 2.1, 0.0);
        clip.speed = -1.0;
        let mut t = track(StreamKind::Video, "V1", vec![clip]);

        t.align_cuts_to_beats(&beats, 0.25, &HashMap::from([(asset, 10.0)]));
        assert_eq!(t.clips[0].source_out, 2.1);
        assert!((t.clips[0].source_in - 1.1).abs() < 1e-9);
        assert!((t.clips[0].timeline_end() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn for_render_drops_muted_tracks_and_disabled_clips() {
        let mut disabled = clip_at(0.0, 2.0);
        disabled.enabled = false;
        let tl = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0), disabled]),
                Track {
                    muted: true,
                    ..track(StreamKind::Audio, "A1", vec![clip_at(0.0, 2.0)])
                },
            ],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        };
        let r = tl.for_render();
        // The disabled clip is gone but the enabled one stays.
        assert_eq!(r.tracks[0].clips.len(), 1);
        // A muted track keeps its row (it still carries `duck`) but loses its clips.
        assert_eq!(r.tracks.len(), 2);
        assert!(r.tracks[1].clips.is_empty());
        // Filtering never touches the original.
        assert_eq!(tl.tracks[0].clips.len(), 2);
    }

    #[test]
    fn solo_shadows_other_tracks_of_the_same_kind_only() {
        let tl = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0)]),
                Track {
                    solo: true,
                    ..track(StreamKind::Video, "V2", vec![clip_at(0.0, 2.0)])
                },
                track(StreamKind::Audio, "A1", vec![clip_at(0.0, 2.0)]),
            ],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        };
        let r = tl.for_render();
        assert!(r.tracks[0].clips.is_empty(), "unsoloed video track is shadowed");
        assert_eq!(r.tracks[1].clips.len(), 1, "soloed video track renders");
        // Soloing a video track must not blank unrelated audio.
        assert_eq!(r.tracks[2].clips.len(), 1, "audio is unaffected by a video solo");
    }

    #[test]
    fn a_muted_track_stays_muted_even_when_soloed() {
        let tl = Timeline {
            tracks: vec![Track {
                muted: true,
                solo: true,
                ..track(StreamKind::Audio, "A1", vec![clip_at(0.0, 2.0)])
            }],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        };
        assert!(tl.for_render().tracks[0].clips.is_empty());
    }

    fn framed(ratio: (u32, u32), left: f64, right: f64) -> Framing {
        Framing {
            aspect_w: ratio.0,
            aspect_h: ratio.1,
            crop_left: left,
            crop_right: right,
            crop_top: 0.0,
            crop_bottom: 0.0,
        }
    }

    #[test]
    fn delivery_shapes_reduce_and_parse() {
        assert_eq!(Delivery::new(1080, 1920, Fit::Cover).ratio(), (9, 16));
        assert_eq!(Delivery::new(720, 1280, Fit::Cover).ratio(), (9, 16));
        assert_eq!(Delivery::new(1080, 1350, Fit::Cover).ratio_label(), "4:5");
        let v = Delivery::parse("9:16").unwrap();
        assert_eq!((v.width, v.height, v.fit), (1080, 1920, Fit::Cover));
        let l = Delivery::parse("16:9").unwrap();
        assert_eq!((l.width, l.height, l.fit), (1920, 1080, Fit::Contain));
        let custom = Delivery::parse("1440x1800").unwrap();
        assert_eq!((custom.width, custom.height, custom.fit), (1440, 1800, Fit::Cover));
        assert_eq!(Delivery::parse("3840x2160").unwrap().fit, Fit::Contain);
        assert!(Delivery::parse("wide").is_none());
        assert!(Delivery::parse("0x10").is_none());
    }

    #[test]
    fn a_framing_replaces_its_own_shape_and_keeps_the_others() {
        let mut clip = clip_at(0.0, 2.0);
        assert!(clip.set_framing(framed((9, 16), 0.1, 0.5)));
        assert!(clip.set_framing(framed((1, 1), 0.2, 0.3)));
        assert!(!clip.set_framing(framed((9, 16), 0.1, 0.5)), "unchanged is not a change");
        assert!(clip.set_framing(framed((9, 16), 0.3, 0.3)));
        assert_eq!(clip.framings.len(), 2);
        assert_eq!(clip.framing_for((9, 16)).unwrap().crop_left, 0.3);
        assert_eq!(clip.framing_for((1, 1)).unwrap().crop_left, 0.2);
        assert!(clip.framing_for((4, 5)).is_none());
    }

    #[test]
    fn for_delivery_swaps_in_the_crop_for_that_shape() {
        let mut clip = clip_at(0.0, 2.0);
        // The project is cut 9:16 and the clip carries that crop as its transform.
        clip.transform.crop_left = 0.1;
        clip.transform.crop_right = 0.5836;
        clip.set_framing(framed((1, 1), 0.2, 0.3));
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![clip])],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: Some(Delivery::new(1080, 1920, Fit::Cover)),
            master: Default::default(),
        };

        let square = tl.for_delivery(Delivery::new(1080, 1080, Fit::Cover));
        let t = square.tracks[0].clips[0].transform;
        assert_eq!((t.crop_left, t.crop_right), (0.2, 0.3));
        assert_eq!(square.format.unwrap().ratio(), (1, 1));

        // No framing for 4:5: the crop it has is kept rather than thrown away.
        let portrait = tl.for_delivery(Delivery::new(1080, 1350, Fit::Cover));
        let t = portrait.tracks[0].clips[0].transform;
        assert_eq!((t.crop_left, t.crop_right), (0.1, 0.5836));

        // The project's own shape at another size changes only the size.
        let small = tl.for_delivery(Delivery::new(720, 1280, Fit::Cover));
        assert_eq!(small.tracks[0].clips[0].transform, tl.tracks[0].clips[0].transform);
        assert_eq!((small.format.unwrap().width, small.format.unwrap().height), (720, 1280));

        // Never touches the original.
        assert_eq!(tl.tracks[0].clips[0].transform.crop_left, 0.1);
    }

    #[test]
    fn for_delivery_refits_generated_captions_only() {
        let long = "a caption line that is wide";
        let mut caption = TextOverlay::new(long, 0.0, 1.0);
        caption.size = 0.05;
        caption.generated = true;
        let mut title = TextOverlay::new(long, 0.0, 1.0);
        title.size = 0.05;
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0)])],
            overlays: vec![caption, title],
            markers: Vec::new(),
            format: Some(Delivery::new(1920, 1080, Fit::Contain)),
            master: Default::default(),
        };
        let vertical = tl.for_delivery(Delivery::new(1080, 1920, Fit::Cover));
        let fitted = vertical.overlays[0].size;
        assert!(fitted < 0.05, "a 9:16 frame is too narrow for the line at 5%");
        assert!(
            long.chars().count() as f64 * CHAR_ADVANCE * fitted <= CAPTION_WIDTH * (1080.0 / 1920.0) + 1e-9,
            "fits across the frame"
        );
        assert_eq!(vertical.overlays[1].size, 0.05, "a typed title is not resized");
    }

    #[test]
    fn a_new_framing_reads_in_the_diff() {
        let before = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0)])],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        };
        let mut after = before.clone();
        after.tracks[0].clips[0].set_framing(framed((9, 16), 0.1, 0.5));
        after.tracks[0].clips[0].set_framing(framed((1, 1), 0.2, 0.3));
        let diff = before.diff(&after);
        assert_eq!(diff.entries.len(), 1, "{diff:?}");
        let detail = diff.entries[0].detail.clone().unwrap_or_default();
        assert!(detail.contains("framed for 9:16, 1:1"), "{detail}");
        assert!(after.diff(&after).entries.is_empty());
    }

    #[test]
    fn for_render_is_a_no_op_on_an_untouched_timeline() {
        let tl = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0), clip_at(2.0, 1.0)]),
                track(StreamKind::Audio, "A1", vec![clip_at(0.0, 3.0)]),
            ],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        };
        let r = tl.for_render();
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            serde_json::to_string(&tl).unwrap(),
            "an ordinary timeline must reach the graph builders unchanged"
        );
    }

    #[test]
    fn enabled_defaults_to_true_for_clips_saved_before_the_field_existed() {
        // Old projects have no `enabled` key; they must not silently stop rendering.
        let clip: Clip = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000001",
                "asset_id":"00000000-0000-0000-0000-000000000002",
                "source_in":0.0,"source_out":1.0,"timeline_start":0.0,"volume":1.0}"#,
        )
        .unwrap();
        assert!(clip.enabled);
        let t: Track =
            serde_json::from_str(r#"{"id":"00000000-0000-0000-0000-000000000003","kind":"video","name":"V1","clips":[]}"#)
                .unwrap();
        assert!(!t.muted && !t.solo && !t.locked);
    }

    // ---- keyframe easing -----------------------------------------------------------

    fn eased(time: f64, scale: f64, easing: Easing) -> Keyframe {
        Keyframe {
            easing,
            ..Keyframe::from_transform(
                time,
                &Transform {
                    scale,
                    ..Transform::default()
                },
            )
        }
    }

    #[test]
    fn easing_curves_start_at_zero_end_at_one_and_never_go_back() {
        let curves = [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::Bezier {
                x1: 0.2,
                y1: 0.9,
                x2: 0.3,
                y2: 0.1,
            },
        ];
        for e in curves {
            assert!(e.curve(0.0).abs() < 1e-9, "{e:?}");
            assert!((e.curve(1.0) - 1.0).abs() < 1e-9, "{e:?}");
            let mut last = 0.0;
            for i in 1..=100 {
                let v = e.curve(f64::from(i) / 100.0);
                assert!(v >= last - 1e-9 && v <= 1.0 + 1e-9, "{e:?} at {i}: {v} after {last}");
                last = v;
            }
        }
        // The named curves are CSS's: ease-in is slow at first, ease-out slow at the end.
        assert!(Easing::EaseIn.curve(0.25) < 0.25 && Easing::EaseOut.curve(0.25) > 0.25);
        assert!((Easing::EaseInOut.curve(0.5) - 0.5).abs() < 1e-6);
        // A hold stays put until the next key.
        assert_eq!(Easing::Hold.curve(0.99), 0.0);
        // Control points outside the unit square are held to it (no overshoot).
        let wild = Easing::Bezier {
            x1: 0.5,
            y1: 3.0,
            x2: 0.5,
            y2: -2.0,
        };
        assert!((0..=20).all(|i| (0.0..=1.0).contains(&wild.curve(f64::from(i) / 20.0))));
    }

    /// The same numbers `frontend/src/lib/easing.test.ts` pins for its mirror: the two
    /// implementations do the same arithmetic in the same order, so they agree exactly.
    #[test]
    fn easing_curves_match_the_frontend_mirror_bit_for_bit() {
        let bezier = Easing::Bezier {
            x1: 0.2,
            y1: 0.9,
            x2: 0.3,
            y2: 0.1,
        };
        assert_eq!(Easing::EaseIn.curve(0.25), 0.093_464_650_718_456_97);
        assert_eq!(Easing::EaseOut.curve(0.25), 0.378_138_130_824_722_36);
        assert_eq!(Easing::EaseInOut.curve(0.3), 0.187_395_906_704_947_93);
        assert_eq!(bezier.curve(0.5), 0.551_329_682_020_157_3);
        assert_eq!(Easing::EaseInOut.curve(7.0 / 12.0), 0.641_173_603_406_141_9);
    }

    #[test]
    fn linear_keys_are_their_own_polyline_and_eased_ones_are_twelve_pieces() {
        let lin = [(0.0, 1.0, Easing::Linear), (2.0, 3.0, Easing::Linear)];
        assert_eq!(eased_points(&lin), vec![(0.0, 1.0), (2.0, 3.0)]);
        let ease = [(0.0, 1.0, Easing::EaseInOut), (2.0, 3.0, Easing::Linear)];
        let pts = eased_points(&ease);
        assert_eq!(pts.len(), EASE_STEPS + 1);
        assert_eq!(pts.first(), Some(&(0.0, 1.0)));
        assert_eq!(pts.last(), Some(&(2.0, 3.0)));
        // A hold is a step at the next key.
        let hold = [(0.0, 1.0, Easing::Hold), (2.0, 3.0, Easing::Linear)];
        assert_eq!(eased_points(&hold), vec![(0.0, 1.0), (2.0, 1.0), (2.0, 3.0)]);
        assert_eq!(interpolate(&eased_points(&hold), 1.999), Some(1.0));
        assert_eq!(interpolate(&eased_points(&hold), 2.0), Some(3.0));
    }

    #[test]
    fn transform_at_follows_each_keys_outgoing_easing() {
        let mut clip = clip_at(0.0, 10.0);
        clip.keyframes = vec![
            eased(0.0, 1.0, Easing::EaseIn),
            eased(2.0, 3.0, Easing::Hold),
            eased(4.0, 5.0, Easing::Linear),
            eased(6.0, 1.0, Easing::Linear),
        ];
        // Slow out of the first key, at the key values on the keys.
        assert!(clip.transform_at(0.5).scale < 1.5);
        assert!((clip.transform_at(2.0).scale - 3.0).abs() < 1e-12);
        // Held from 2 to 4, then the jump.
        assert!((clip.transform_at(3.9).scale - 3.0).abs() < 1e-12);
        assert!((clip.transform_at(4.0).scale - 5.0).abs() < 1e-12);
        // A linear segment is exactly linear.
        assert!((clip.transform_at(5.0).scale - 3.0).abs() < 1e-12);
    }

    #[test]
    fn keys_saved_before_easing_existed_are_linear_and_linear_is_not_written() {
        let k: Keyframe = serde_json::from_str(r#"{"time":1.0,"scale":2.0}"#).unwrap();
        assert_eq!(k.easing, Easing::Linear);
        assert!(!serde_json::to_string(&k).unwrap().contains("easing"));
        let b = eased(
            0.0,
            1.0,
            Easing::Bezier {
                x1: 0.1,
                y1: 0.2,
                x2: 0.3,
                y2: 0.4,
            },
        );
        let json = serde_json::to_string(&b).unwrap();
        assert!(
            json.contains(r#""easing":{"bezier":{"x1":0.1,"y1":0.2,"x2":0.3,"y2":0.4}}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<Keyframe>(&json).unwrap(), b);
        let hold = serde_json::to_string(&eased(0.0, 1.0, Easing::EaseInOut)).unwrap();
        assert!(hold.contains(r#""easing":"ease_in_out""#), "{hold}");
    }

    #[test]
    fn cutting_into_an_eased_segment_keeps_the_curve_exactly() {
        for easing in [Easing::EaseInOut, Easing::Hold, Easing::EaseOut] {
            let mut original = clip_at(0.0, 8.0);
            original.keyframes = vec![
                eased(0.0, 1.0, easing),
                eased(3.0, 4.0, Easing::EaseIn),
                eased(6.0, 2.0, Easing::Linear),
            ];
            let tl = Timeline {
                tracks: vec![track(StreamKind::Video, "V1", vec![original.clone()])],
                overlays: Vec::new(),
                markers: Vec::new(),
                format: None,
                master: Default::default(),
            };
            // A cut inside the first (eased) segment, not on one of its pieces.
            let from = 1.1;
            let sliced = tl.slice(from, 8.0);
            let c = &sliced.tracks[0].clips[0];
            for i in 0..=120 {
                let t = f64::from(i) * 0.05;
                let (a, b) = (c.transform_at(t).scale, original.transform_at(from + t).scale);
                assert!((a - b).abs() < 1e-9, "{easing:?} at {t}: sliced {a}, original {b}");
            }
        }
    }

    /// A head cut that lands exactly on a key used to read as "before the segment": the key's
    /// outgoing easing was dropped and a hold became a ramp from its value to the next key's.
    #[test]
    fn cutting_exactly_on_a_key_keeps_the_easing_that_leaves_it() {
        let bezier = Easing::Bezier {
            x1: 0.2,
            y1: 0.9,
            x2: 0.3,
            y2: 0.1,
        };
        for easing in [Easing::EaseInOut, Easing::Hold, bezier, Easing::EaseOut] {
            let mut original = clip_at(0.0, 8.0);
            original.keyframes = vec![
                eased(0.0, 1.0, Easing::Linear),
                eased(3.0, 4.0, easing),
                eased(6.0, 2.0, Easing::Linear),
            ];
            let tl = Timeline {
                tracks: vec![track(StreamKind::Video, "V1", vec![original.clone()])],
                overlays: Vec::new(),
                markers: Vec::new(),
                format: None,
                master: Default::default(),
            };
            let id = original.id;
            // The range cut, and the edit that trims the head and moves the clip: same result.
            let sliced = tl.slice(3.0, 8.0).tracks[0].clips[0].clone();
            let mut trimmed = tl.clone();
            let trimmed_clip = trimmed.split_remove(id, 3.0, SplitSide::Left).unwrap();
            for c in [sliced, trimmed_clip] {
                // Between the pieces of a curve and a hold's step, never on one.
                for i in 0..60 {
                    let t = (f64::from(i) + 0.5) * 0.05;
                    let (a, b) = (c.transform_at(t).scale, original.transform_at(3.0 + t).scale);
                    assert!((a - b).abs() < 1e-9, "{easing:?} at {t}: cut {a}, original {b}");
                }
                if easing == Easing::Hold {
                    assert_eq!(c.keyframes[0].easing, Easing::Hold);
                    assert_eq!(c.transform_at(2.9).scale, 4.0);
                }
            }
        }
    }

    /// The curve `easing` draws over `[0, 1]`, as the two halves of a split at `u` draw it.
    fn split_deviation(easing: Easing, u: f64) -> f64 {
        let (before, after) = easing.split(u);
        let v = easing.curve(u);
        let mut worst: f64 = 0.0;
        for i in 0..=20 {
            let w = f64::from(i) / 20.0;
            // The first part stretched back to its size in the whole, then the second.
            worst = worst.max((v * before.curve(w) - easing.curve(u * w)).abs());
            worst = worst.max((v + (1.0 - v) * after.curve(w) - easing.curve(u + (1.0 - u) * w)).abs());
        }
        worst
    }

    #[test]
    fn splitting_an_easing_draws_the_same_curve_in_two_pieces() {
        let at = [0.05, 0.3, 0.5, 0.7, 0.95];
        // Every preset the editor offers, exactly...
        let named = [
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::Bezier {
                x1: 0.2,
                y1: 0.9,
                x2: 0.3,
                y2: 1.0,
            },
            Easing::Bezier {
                x1: 0.45,
                y1: 0.05,
                x2: 0.55,
                y2: 0.95,
            },
            Easing::Bezier {
                x1: 0.1,
                y1: 0.6,
                x2: 0.4,
                y2: 1.0,
            },
        ];
        for easing in named {
            for u in at {
                assert!(split_deviation(easing, u) < 1e-9, "{easing:?} split at {u}");
            }
        }
        // ... and so is any bezier whose control points rise, over a grid of them.
        let grid = [0.0, 0.25, 0.5, 0.75, 1.0];
        for x1 in grid {
            for y1 in grid {
                for x2 in grid.into_iter().filter(|x2| *x2 >= x1) {
                    for y2 in grid.into_iter().filter(|y2| *y2 >= y1) {
                        let easing = Easing::Bezier { x1, y1, x2, y2 };
                        for u in at {
                            let off = split_deviation(easing, u);
                            assert!(off < 1e-4, "{easing:?} split at {u} is {off} off");
                        }
                    }
                }
            }
        }
        // An S that turns back needs a control point outside the square: re-fitted, not exact.
        let s_curve = Easing::Bezier {
            x1: 0.2,
            y1: 0.9,
            x2: 0.3,
            y2: 0.1,
        };
        let off = at.map(|u| split_deviation(s_curve, u)).into_iter().fold(0.0, f64::max);
        assert!((1e-4..0.05).contains(&off), "{off}");
        // Not curves: a line stays a line, a hold stays held on both sides.
        assert_eq!(Easing::Linear.split(0.4), (Easing::Linear, Easing::Linear));
        assert_eq!(Easing::Hold.split(0.4), (Easing::Hold, Easing::Hold));
        // A plateau at the start has nothing to draw before the new key.
        let late = Easing::Bezier {
            x1: 0.0,
            y1: 0.0,
            x2: 0.0,
            y2: 0.0,
        };
        assert_eq!(late.split(1e-12).0, Easing::Linear);
    }

    /// The numbers `frontend/src/lib/easing.test.ts` pins for `splitEasing`.
    #[test]
    fn easing_splits_match_the_frontend_mirror_bit_for_bit() {
        let bezier = |x1, y1, x2, y2| Easing::Bezier { x1, y1, x2, y2 };
        assert_eq!(
            Easing::EaseInOut.split(0.3),
            (
                bezier(0.387_469_928_603_703_55, 0.0, 0.708_554_541_138_148_6, 0.408_751_946_428_076),
                bezier(
                    0.326_400_214_186_121_75,
                    0.356_304_039_690_523_16,
                    0.566_058_540_830_177_7,
                    1.0
                ),
            )
        );
        assert_eq!(
            Easing::EaseIn.split(0.25),
            (
                bezier(
                    0.317_162_150_132_499_54,
                    0.0,
                    0.657_134_238_921_790_6,
                    0.381_326_296_917_524_73
                ),
                bezier(0.491_095_208_371_804_6, 0.274_086_177_201_424_27, 1.0, 1.0),
            )
        );
    }

    #[test]
    fn a_key_added_inside_a_segment_splits_it_and_leaves_the_motion_where_it_was() {
        let bezier = Easing::Bezier {
            x1: 0.2,
            y1: 0.9,
            x2: 0.3,
            y2: 1.0,
        };
        for easing in [Easing::EaseInOut, Easing::EaseIn, bezier, Easing::Hold] {
            let mut clip = clip_at(0.0, 10.0);
            clip.keyframes = vec![eased(1.0, 1.0, easing), eased(5.0, 3.0, Easing::Linear)];
            let before = clip.clone();
            // Pin the present pose a third of the way in, as the editor does.
            let pose = Keyframe::from_transform(2.3, &clip.transform_at(2.3));
            clip.insert_keyframe(pose);
            assert_eq!(clip.keyframes.len(), 3);
            for i in 0..=80 {
                // Never on a hold's step. The two are the same curve drawn in different pieces,
                // so they differ by how a steep curve is drawn in 12 of them (about 0.014 of a
                // range of 2 for the steepest here); a linear key in the middle is 0.3 off.
                let t = (f64::from(i) + 0.5) * 0.05;
                let (a, b) = (clip.transform_at(t).scale, before.transform_at(t).scale);
                assert!((a - b).abs() < 0.03, "{easing:?} at {t}: now {a}, was {b}");
            }
            if easing == Easing::Hold {
                assert_eq!(
                    (clip.keyframes[0].easing, clip.keyframes[1].easing),
                    (Easing::Hold, Easing::Hold)
                );
                assert_eq!(clip.transform_at(4.9).scale, 1.0);
            } else {
                assert!(matches!(clip.keyframes[0].easing, Easing::Bezier { .. }));
                assert!(matches!(clip.keyframes[1].easing, Easing::Bezier { .. }));
            }
            assert_eq!(clip.keyframes[2].easing, Easing::Linear);
        }
    }

    #[test]
    fn a_key_outside_every_segment_or_on_a_key_changes_no_neighbours_shape() {
        let mut clip = clip_at(0.0, 10.0);
        clip.keyframes = vec![eased(1.0, 1.0, Easing::EaseIn), eased(5.0, 3.0, Easing::Hold)];
        // Before the first and after the last there is no segment to split: the key comes as it is
        // and the ones next to it are untouched.
        clip.insert_keyframe(eased(0.0, 2.0, Easing::Hold));
        clip.insert_keyframe(eased(8.0, 2.0, Easing::EaseOut));
        let shapes: Vec<_> = clip.keyframes.iter().map(|k| (k.time, k.easing)).collect();
        assert_eq!(
            shapes,
            vec![
                (0.0, Easing::Hold),
                (1.0, Easing::EaseIn),
                (5.0, Easing::Hold),
                (8.0, Easing::EaseOut)
            ]
        );
        // On a key: the pose moves, the way it leaves does not.
        clip.insert_keyframe(eased(1.0000004, 9.0, Easing::Linear));
        assert_eq!((clip.keyframes[1].scale, clip.keyframes[1].easing), (9.0, Easing::EaseIn));
        assert_eq!(clip.keyframes.len(), 4);
    }

    #[test]
    fn slice_shifts_markers_into_the_window_and_drops_the_rest() {
        let mk = |t: f64, n: &str| Marker {
            id: Uuid::new_v4(),
            time: t,
            name: n.into(),
            color: None,
        };
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 20.0)])],
            overlays: Vec::new(),
            markers: vec![mk(1.0, "before"), mk(4.0, "inside"), mk(9.0, "after")],
            format: None,
            master: Default::default(),
        };
        let s = tl.slice(3.0, 7.0);
        let names: Vec<_> = s.markers.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["inside"], "only markers within the window survive");
        // Shifted with everything else — otherwise a range export desyncs them.
        assert!((s.markers[0].time - 1.0).abs() < 1e-9, "{}", s.markers[0].time);
    }

    #[test]
    fn slice_carries_track_flags_through() {
        let tl = Timeline {
            tracks: vec![Track {
                muted: true,
                locked: true,
                duck: true,
                ..track(StreamKind::Audio, "A1", vec![clip_at(0.0, 10.0)])
            }],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        };
        let s = tl.slice(2.0, 6.0);
        assert!(s.tracks[0].muted && s.tracks[0].locked && s.tracks[0].duck);
    }

    #[test]
    fn diff_says_when_only_the_easing_of_keys_changed() {
        let mut clip = clip_at(0.0, 4.0);
        clip.keyframes = vec![
            eased(0.0, 1.0, Easing::Linear),
            eased(2.0, 2.0, Easing::Linear),
            eased(3.0, 3.0, Easing::Linear),
        ];
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![clip])],
            ..Timeline::new()
        };
        let summary = |after: &Timeline| tl.diff(after).entries[0].detail.clone().unwrap_or_default();
        let mut one = tl.clone();
        one.tracks[0].clips[0].keyframes[0].easing = Easing::Hold;
        assert!(summary(&one).contains("easing changed on 1 keyframe"), "{}", summary(&one));
        assert!(!summary(&one).contains("retimed"), "{}", summary(&one));
        let mut two = one.clone();
        two.tracks[0].clips[0].keyframes[1].easing = Easing::EaseOut;
        assert!(summary(&two).contains("easing changed on 2 keyframes"), "{}", summary(&two));
        // A key that moved is still a retime, and both are said when both happened.
        let mut moved = two.clone();
        moved.tracks[0].clips[0].keyframes[2].time = 3.5;
        assert!(summary(&moved).contains("keyframes retimed"), "{}", summary(&moved));
        assert!(
            summary(&moved).contains("easing changed on 2 keyframes"),
            "{}",
            summary(&moved)
        );
        let mut only_moved = tl.clone();
        only_moved.tracks[0].clips[0].keyframes[2].time = 3.5;
        assert!(summary(&only_moved).contains("keyframes retimed"), "{}", summary(&only_moved));
        assert!(!summary(&only_moved).contains("easing"), "{}", summary(&only_moved));
    }

    #[test]
    fn an_untouched_timeline_diffs_to_nothing() {
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0), clip_at(4.0, 3.0)])],
            ..Timeline::new()
        };
        let diff = tl.diff(&tl.clone());
        assert!(diff.is_empty());
        assert_eq!(diff.headline(), "No changes");
        assert_eq!(diff.clips_before, 2);
    }

    #[test]
    fn diff_names_the_move_the_trim_and_the_removal() {
        let tl = Timeline {
            tracks: vec![track(
                StreamKind::Video,
                "V1",
                vec![clip_at(0.0, 4.0), clip_at(4.0, 3.0), clip_at(7.0, 2.0)],
            )],
            ..Timeline::new()
        };
        let mut after = tl.clone();
        {
            let clips = &mut after.tracks[0].clips;
            clips[0].source_out = 2.5; // retrim
            clips[1].timeline_start = 5.0; // move
            clips.remove(2); // cut
        }

        let diff = tl.diff(&after);
        let kinds: Vec<DiffKind> = diff.entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![DiffKind::ClipRemoved, DiffKind::ClipRetrimmed, DiffKind::ClipMoved]
        );
        assert_eq!(diff.clips_before, 3);
        assert_eq!(diff.clips_after, 2);
        // The retrim reports the source window, and the entry carries the clip
        // so the reviewer can jump to it.
        assert!(
            diff.entries[1].summary.contains("4.0s → 2.5s (-1.5s)"),
            "{}",
            diff.entries[1].summary
        );
        assert_eq!(diff.entries[1].clip_id, Some(after.tracks[0].clips[0].id));
        assert!(
            diff.entries[2].summary.contains("0:04.0 → 0:05.0"),
            "{}",
            diff.entries[2].summary
        );
        // The headline leads with what the edit did to the runtime.
        assert_eq!(diff.headline(), "3 changes · 0:09.0 → 0:08.0 (-1.0s) · 3 → 2 clips");
    }

    #[test]
    fn a_removed_track_is_one_change_not_one_per_clip() {
        let tl = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)]),
                track(StreamKind::Audio, "A1", vec![clip_at(0.0, 4.0), clip_at(4.0, 4.0)]),
            ],
            ..Timeline::new()
        };
        let mut after = tl.clone();
        after.tracks.remove(1);

        let diff = tl.diff(&after);
        assert_eq!(diff.entries.len(), 1);
        assert_eq!(diff.entries[0].kind, DiffKind::TrackRemoved);
        assert!(
            diff.entries[0].summary.contains("A1 (2 clips)"),
            "{}",
            diff.entries[0].summary
        );
    }

    #[test]
    fn a_clip_dragged_to_another_track_reads_as_one_move() {
        let mut tl = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)]),
                track(StreamKind::Video, "V2", vec![]),
            ],
            ..Timeline::new()
        };
        tl.tracks[1].kind = StreamKind::Video;
        let mut after = tl.clone();
        let clip = after.tracks[0].clips.remove(0);
        after.tracks[1].clips.push(clip);

        let diff = tl.diff(&after);
        assert_eq!(diff.entries.len(), 1);
        assert_eq!(diff.entries[0].kind, DiffKind::ClipMoved);
        assert!(
            diff.entries[0].summary.contains("from V1 to V2"),
            "{}",
            diff.entries[0].summary
        );
    }

    #[test]
    fn diff_details_what_changed_on_a_clip() {
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)])],
            ..Timeline::new()
        };
        let mut after = tl.clone();
        {
            let c = &mut after.tracks[0].clips[0];
            c.volume = 0.4;
            c.speed = 2.0;
            c.effects.push(VideoEffect::Vignette);
            c.enabled = false;
        }

        let diff = tl.diff(&after);
        assert_eq!(diff.entries.len(), 1);
        assert_eq!(diff.entries[0].kind, DiffKind::ClipChanged);
        let detail = diff.entries[0].detail.clone().unwrap();
        assert!(detail.contains("volume 100% → 40%"), "{detail}");
        assert!(detail.contains("speed 1.00× → 2.00×"), "{detail}");
        assert!(detail.contains("disabled"), "{detail}");
        assert!(detail.contains("video effects none → vignette"), "{detail}");
    }

    #[test]
    fn diff_sees_a_mask_and_the_track_mix() {
        // An agent proposal that only masks a clip or rides a fader must not
        // diff as empty — apply_staged discards an empty proposal.
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)])],
            ..Timeline::new()
        };
        let mut after = tl.clone();
        after.tracks[0].clips[0].mask = Some(Mask::default());
        let diff = tl.diff(&after);
        assert_eq!(diff.entries.len(), 1, "{diff:?}");
        assert!(diff.entries[0].detail.as_deref().unwrap().contains("masked (rect)"));

        let mut after = tl.clone();
        after.tracks[0].volume = 0.5;
        after.tracks[0].pan = -0.3;
        let diff = tl.diff(&after);
        assert_eq!(diff.entries.len(), 1, "{diff:?}");
        let detail = diff.entries[0].detail.clone().unwrap();
        assert!(detail.contains("level 100% → 50%"), "{detail}");
        assert!(detail.contains("pan 0.00 → -0.30"), "{detail}");
    }

    #[test]
    fn diff_covers_overlays_markers_and_the_delivery_frame() {
        let tl = Timeline::new();
        let mut after = tl.clone();
        after.overlays.push(TextOverlay::new("Hello", 1.0, 3.0));
        after.markers.push(Marker {
            id: Uuid::new_v4(),
            time: 72.0,
            name: "the laugh".to_string(),
            color: None,
        });
        after.format = Some(Delivery::new(1080, 1920, Fit::Cover));

        let diff = tl.diff(&after);
        let kinds: Vec<DiffKind> = diff.entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![DiffKind::OverlayAdded, DiffKind::MarkerAdded, DiffKind::FormatChanged]
        );
        assert!(diff.entries[1].summary.contains("1:12.0"), "{}", diff.entries[1].summary);
        assert!(
            diff.entries[2].summary.contains("1080x1920 (cover)"),
            "{}",
            diff.entries[2].summary
        );
        // Every entry lands in the rendered summary the agent reads back.
        assert_eq!(diff.summary().lines().count(), 4);
    }

    // ---- smart crop ---------------------------------------------------------

    /// A map whose salience sits in one horizontal band of `cols`, so a test can
    /// say "the subject is on the left third" and nothing else.
    fn map_with_column_band(cols: usize, from: usize, to: usize) -> SalienceMap {
        let rows = 4;
        let mut cells = vec![0.01f32; cols * rows];
        for r in 0..rows {
            for c in from..to {
                cells[r * cols + c] = 1.0;
            }
        }
        SalienceMap::new(cols, rows, cells)
    }

    #[test]
    fn a_matching_aspect_needs_no_crop() {
        let map = map_with_column_band(32, 0, 32);
        // 1080x1920 delivered at 9:16, and 1920x1080 at a 1280x720 frame.
        assert!(map.crop_for(1080, 1920, 1080.0 / 1920.0).is_none());
        assert!(map.crop_for(1920, 1080, 1280.0 / 720.0).is_none());
    }

    #[test]
    fn a_vertical_delivery_crops_width_toward_the_subject() {
        // 16:9 footage into a 9:16 frame with the subject in the left third.
        let map = map_with_column_band(48, 4, 16);
        let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
        assert_eq!((crop.top, crop.bottom), (0.0, 0.0));
        // 9:16 of 16:9 keeps 0.3164 of the width; the rest is cut.
        assert!((crop.left + crop.right - (1.0 - 1080.0 * 1080.0 / (1920.0 * 1920.0))).abs() < 1e-6);
        // The kept window contains the band, which a centre crop would miss.
        assert!(crop.left < 4.0 / 48.0 && 1.0 - crop.right > 16.0 / 48.0, "{crop:?}");
        assert!(!crop.is_centered());
        assert!(crop.offset < 0.0, "a left-hand subject pulls the window left");
    }

    #[test]
    fn flat_salience_falls_back_to_the_centre_crop() {
        let map = map_with_column_band(48, 0, 48);
        let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
        assert!((crop.left - crop.right).abs() < 1e-9, "{crop:?}");
        assert!(crop.is_centered());
    }

    #[test]
    fn an_off_centre_subject_still_loses_to_a_hard_pull_it_cannot_earn() {
        // Salience a hair off centre: the centre bias should hold the window put
        // rather than pan for a rounding difference.
        let map = map_with_column_band(48, 23, 27);
        let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
        assert!(crop.is_centered(), "{crop:?}");
    }

    #[test]
    fn a_landscape_delivery_crops_height_of_vertical_footage() {
        // 9:16 footage into 16:9, subject in the top rows.
        let cols = 4;
        let rows = 32;
        let mut cells = vec![0.01f32; cols * rows];
        for r in 2..8 {
            for c in 0..cols {
                cells[r * cols + c] = 1.0;
            }
        }
        let map = SalienceMap::new(cols, rows, cells);
        let crop = map.crop_for(1080, 1920, 1920.0 / 1080.0).expect("crops");
        assert_eq!((crop.left, crop.right), (0.0, 0.0));
        // The search grid is finer than a bucket but not exact, so allow the top
        // edge to land a hair inside the band it is framing.
        assert!(crop.top <= 2.0 / 32.0 + 0.01 && 1.0 - crop.bottom > 8.0 / 32.0, "{crop:?}");
    }

    #[test]
    fn a_map_with_nothing_in_it_still_yields_the_centre_crop() {
        for map in [
            SalienceMap::default(),
            SalienceMap::new(8, 2, vec![0.0; 16]),
            SalienceMap::new(8, 2, vec![0.0; 3]),
        ] {
            let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
            assert!(crop.is_centered(), "{crop:?}");
            assert!((crop.left - crop.right).abs() < 1e-9);
        }
    }

    #[test]
    fn a_degenerate_aspect_is_refused_rather_than_guessed() {
        let map = map_with_column_band(8, 0, 4);
        assert!(map.crop_for(1920, 1080, 0.0).is_none());
        assert!(map.crop_for(1920, 1080, f64::NAN).is_none());
        assert!(map.crop_for(0, 1080, 1.0).is_none());
    }

    fn seg(start: f64, end: f64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start,
            end,
            text: text.to_string(),
        }
    }

    fn captioned(timeline: &Timeline, asset: Uuid, segments: Vec<TranscriptSegment>) -> Vec<(String, f64, f64)> {
        let mut map = HashMap::new();
        map.insert(asset, segments);
        timeline
            .captions(&map, CaptionOptions::default())
            .into_iter()
            .map(|o| (o.text, (o.start * 100.0).round() / 100.0, (o.end * 100.0).round() / 100.0))
            .collect()
    }

    fn one_clip(clip: Clip) -> Timeline {
        let mut track = Track::new(StreamKind::Video, "V1");
        track.clips = vec![clip];
        Timeline {
            tracks: vec![track],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        }
    }

    #[test]
    fn source_span_maps_through_trim_speed_and_reverse() {
        let asset = Uuid::new_v4();
        // Trimmed: the asset's 10s starts at the clip's in-point, placed at 4s.
        let mut clip = Clip::new(asset, 10.0, 20.0, 4.0);
        let r = clip.source_span_to_timeline(12.0, 14.0);
        assert!((r.start - 6.0).abs() < 1e-9, "{r:?}");
        assert!((r.end - 8.0).abs() < 1e-9, "{r:?}");

        // Double speed halves the distance from the in-point.
        clip.speed = 2.0;
        let r = clip.source_span_to_timeline(12.0, 14.0);
        assert!((r.start - 5.0).abs() < 1e-9, "{r:?}");
        assert!((r.end - 6.0).abs() < 1e-9, "{r:?}");

        // Reversed: the source's tail is heard first, and the range stays ordered.
        clip.speed = -1.0;
        let r = clip.source_span_to_timeline(12.0, 14.0);
        assert!((r.start - 10.0).abs() < 1e-9, "{r:?}");
        assert!((r.end - 12.0).abs() < 1e-9, "{r:?}");
        assert!(r.end > r.start);
    }

    #[test]
    fn covers_source_is_the_clips_own_window() {
        let clip = Clip::new(Uuid::new_v4(), 10.0, 20.0, 0.0);
        assert!(clip.covers_source(12.0, 14.0));
        assert!(clip.covers_source(8.0, 11.0), "straddling the in-point still shows");
        assert!(!clip.covers_source(0.0, 10.0), "ending exactly at the in-point shows nothing");
        assert!(!clip.covers_source(20.0, 25.0));
    }

    // ---- the master bus ---------------------------------------------------

    #[test]
    fn an_untouched_master_is_not_written_and_an_old_file_reads_back_neutral() {
        let tl = Timeline::new();
        let json = serde_json::to_string(&tl).unwrap();
        assert!(
            !json.contains("master"),
            "a default master must not change the saved JSON: {json}"
        );
        // A timeline saved before the master existed.
        let old: Timeline = serde_json::from_str(r#"{"tracks":[]}"#).unwrap();
        assert!(old.master.is_default());
        assert!(old.master.is_neutral());
    }

    #[test]
    fn a_master_round_trips_and_a_partial_one_fills_from_the_defaults() {
        let mut tl = Timeline::new();
        tl.master = MasterBus {
            volume: 0.7,
            limiter: true,
            ceiling_db: -3.0,
        };
        let back: Timeline = serde_json::from_str(&serde_json::to_string(&tl).unwrap()).unwrap();
        assert_eq!(back.master, tl.master);
        let partial: Timeline = serde_json::from_str(r#"{"tracks":[],"master":{"limiter":true}}"#).unwrap();
        assert_eq!(
            partial.master,
            MasterBus {
                volume: 1.0,
                limiter: true,
                ceiling_db: -1.5
            }
        );
    }

    #[test]
    fn the_master_is_neutral_until_it_changes_the_mix() {
        let mut m = MasterBus::default();
        assert!(m.is_neutral() && m.is_default());
        // A stored ceiling the limiter is not using changes nothing in the graph...
        m.ceiling_db = -6.0;
        assert!(m.is_neutral());
        // ...but is still something to save.
        assert!(!m.is_default());
        m.limiter = true;
        assert!(!m.is_neutral());
        m.limiter = false;
        m.volume = 0.5;
        assert!(!m.is_neutral());
        m.volume = 0.0;
        assert!(!m.is_neutral(), "a muted master is a master");
    }

    #[test]
    fn a_master_from_a_hand_edited_file_is_made_safe_for_the_graph() {
        let m = MasterBus {
            volume: f64::NAN,
            limiter: true,
            ceiling_db: f64::INFINITY,
        };
        assert_eq!(m.safe_volume(), 1.0);
        assert_eq!(m.safe_ceiling_db(), MASTER_DEFAULT_CEILING_DB);
        let m = MasterBus {
            volume: 99.0,
            limiter: true,
            ceiling_db: -90.0,
        };
        assert_eq!(m.safe_volume(), MASTER_MAX_VOLUME);
        assert_eq!(m.safe_ceiling_db(), MASTER_MIN_CEILING_DB);
        let m = MasterBus {
            volume: -3.0,
            ..MasterBus::default()
        };
        assert_eq!(m.safe_volume(), 0.0);
        assert!((MasterBus::default().limit_linear() - 0.841_395_142).abs() < 1e-6, "-1.5 dB");
    }

    #[test]
    fn the_master_follows_the_cut_into_every_render_of_it() {
        let master = MasterBus {
            volume: 0.5,
            limiter: true,
            ceiling_db: -2.0,
        };
        let tl = Timeline {
            tracks: vec![track(StreamKind::Audio, "A1", vec![clip_at(0.0, 8.0)])],
            master,
            ..Timeline::new()
        };
        assert_eq!(tl.for_render().master, master, "muted tracks are dropped, the mix is not");
        assert_eq!(
            tl.slice(2.0, 6.0).master,
            master,
            "a range export of a limited master is limited"
        );
        assert_eq!(
            tl.for_delivery(Delivery::new(1080, 1920, Fit::Cover)).master,
            master,
            "a variant changes the picture, not the mix"
        );
    }

    #[test]
    fn diff_sees_the_master_bus() {
        // An agent proposal that only rides the master fader or turns the limiter
        // on must not diff as empty — apply_staged discards an empty proposal.
        let tl = Timeline::new();
        assert!(tl.diff(&tl.clone()).is_empty());

        let mut after = tl.clone();
        after.master.volume = 0.7;
        let diff = tl.diff(&after);
        assert_eq!(diff.entries.len(), 1, "{diff:?}");
        assert_eq!(diff.entries[0].kind, DiffKind::MasterChanged);
        assert_eq!(diff.entries[0].detail.as_deref(), Some("level 100% → 70%"));

        let mut limited = tl.clone();
        limited.master.limiter = true;
        let detail = tl.diff(&limited).entries[0].detail.clone().unwrap();
        assert_eq!(detail, "limiter on at -1.5 dB");
        // Moving the ceiling matters only while the limiter is on.
        let mut moved = limited.clone();
        moved.master.ceiling_db = -3.0;
        assert_eq!(
            limited.diff(&moved).entries[0].detail.as_deref(),
            Some("limiter ceiling -1.5 → -3.0 dB")
        );
        let mut parked = tl.clone();
        parked.master.ceiling_db = -3.0;
        assert!(
            tl.diff(&parked).is_empty(),
            "a ceiling nothing is using is not a change to review"
        );
        // Switching off reads as that, whatever the ceiling.
        assert_eq!(limited.diff(&tl).entries[0].detail.as_deref(), Some("limiter off"));
    }

    // ---- levels -----------------------------------------------------------

    fn reading(i: Option<f64>, tp: Option<f64>) -> LevelReading {
        LevelReading {
            integrated_lufs: i,
            true_peak_dbtp: tp,
            ..LevelReading::default()
        }
    }

    fn strip(name: &str, peak: f64) -> TrackLevels {
        TrackLevels {
            track_id: Uuid::new_v4(),
            name: name.into(),
            kind: StreamKind::Audio,
            ducked: false,
            heard: true,
            level: Some(LevelReading {
                peak_dbfs: Some(peak),
                ..LevelReading::default()
            }),
        }
    }

    #[test]
    fn the_levels_notes_judge_the_mix_against_the_streaming_target() {
        let notes = |i, tp| Levels::new(10.0, Some(reading(i, tp)), Vec::new(), false, &MasterBus::default()).notes;
        assert!(notes(Some(-14.0), Some(-3.0))[0].contains("close to"), "on target");
        assert!(notes(Some(-13.2), Some(-3.0))[0].contains("close to"), "within a LU of it");
        assert!(notes(Some(-9.0), Some(-3.0))[0].contains("5.0 LU over"), "too loud");
        assert!(notes(Some(-19.0), Some(-3.0))[0].contains("5.0 LU under"), "too quiet");
        assert!(
            notes(Some(-16.5), Some(-3.0))[0].contains("close to"),
            "a little under is fine"
        );
        assert!(notes(None, None)[0].contains("silent"));
        // Over the true-peak ceiling is its own note, and names the fix.
        let hot = notes(Some(-14.0), Some(0.4));
        assert_eq!(hot.len(), 2, "{hot:?}");
        assert!(
            hot[1].contains("0.4 dBTP") && hot[1].contains("set_master_limiter"),
            "{hot:?}"
        );
        assert_eq!(notes(Some(-14.0), Some(-1.0)).len(), 1, "exactly at the ceiling is allowed");
    }

    #[test]
    fn the_levels_notes_name_a_track_over_full_scale() {
        let tracks = vec![strip("A1", -6.0), strip("Music", 1.5)];
        let bus = MasterBus::default();
        let notes = Levels::new(10.0, Some(reading(Some(-14.0), Some(-3.0))), tracks, false, &bus).notes;
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[1].contains("Music") && notes[1].contains("+1.5 dBFS"), "{notes:?}");
        // The mix is float, so a hot strip is not a clip yet: the note must not claim one.
        assert!(!notes[1].contains("will clip"), "{notes:?}");
        // No audio at all is one plain note.
        assert_eq!(
            Levels::new(0.0, None, Vec::new(), false, &bus).notes,
            ["The cut has no audio to measure."]
        );
    }

    #[test]
    fn a_true_peak_over_the_ceiling_with_the_limiter_on_lowers_the_ceiling() {
        // The limiter holds the sample peak, so "turn on the limiter" is advice already
        // taken: an agent following it again would loop. It is told how far to lower the
        // ceiling instead — the overshoot past -1 dBTP, and half a decibel more.
        let on = |ceiling_db| MasterBus {
            limiter: true,
            ceiling_db,
            ..MasterBus::default()
        };
        let notes = |tp, bus: &MasterBus| Levels::new(10.0, Some(reading(Some(-14.0), Some(tp))), Vec::new(), false, bus).notes;

        // 11 kHz at a -1 dBFS ceiling, as measured: -0.2 dBTP is 0.8 over, so -2.3.
        let n = notes(-0.2, &on(-1.0));
        assert_eq!(n.len(), 2, "{n:?}");
        assert!(!n[1].contains("Turn on"), "{n:?}");
        assert!(
            n[1].contains("-0.2 dBTP") && n[1].contains("-1.0 dBFS") && n[1].contains("about -2.3 dBFS"),
            "{n:?}"
        );
        assert!(n[1].contains("set_master_limiter"), "{n:?}");
        // Word for word the string in the frontend's `levels.test.ts` (the harness mirrors this).
        assert_eq!(
            n[1],
            "True peak -0.2 dBTP is over the -1 dBTP platforms ask for and can clip when re-encoded. The master limiter is already \
             on, but it holds the sample peak at -1.0 dBFS and the peak between samples runs above that. Lower its ceiling to \
             about -2.3 dBFS (set_master_limiter)."
        );
        // From the default ceiling: 0.4 dBTP is 1.4 over.
        assert!(
            notes(0.4, &on(MASTER_DEFAULT_CEILING_DB))[1].contains("about -3.4 dBFS"),
            "from -1.5"
        );
        // Never below what the limiter can be given; at the floor there is nothing to lower.
        assert!(notes(10.0, &on(-20.0))[1].contains("about -24.0 dBFS"));
        let floor = notes(0.4, &on(MASTER_MIN_CEILING_DB));
        assert!(
            floor[1].contains("lowest ceiling") && !floor[1].contains("about"),
            "{floor:?}"
        );
        // A ceiling a `.kerf` file stored out of range reads as the engine clamps it.
        assert!(notes(0.4, &on(f64::NAN))[1].contains("about -3.4 dBFS"));
        // With the limiter off the advice is still to turn it on, and a stored ceiling alone is not "on".
        let off = MasterBus {
            ceiling_db: -6.0,
            ..MasterBus::default()
        };
        assert_eq!(
            notes(0.4, &MasterBus::default())[1],
            "True peak 0.4 dBTP is over the -1 dBTP platforms ask for and can clip when re-encoded. Turn on the master limiter \
             (set_master_limiter) or lower the master."
        );
        assert!(notes(0.4, &off)[1].contains("Turn on the master limiter"));
        // Under the line: no note, limiter or not.
        assert_eq!(notes(-1.0, &on(-1.5)).len(), 1);
    }

    #[test]
    fn a_track_pan_is_a_balance_and_centre_is_exactly_unity() {
        let mut t = Track::new(StreamKind::Audio, "A1");
        assert_eq!(t.pan_gains(), (1.0, 1.0), "an untouched track must not be touched");
        t.pan = -1.0;
        assert_eq!(t.pan_gains(), (1.0, 0.0), "hard left keeps the left at unity");
        t.pan = 1.0;
        assert_eq!(t.pan_gains(), (0.0, 1.0));
        t.pan = -0.5;
        assert_eq!(t.pan_gains(), (1.0, 0.5));
        // Never a boost: leaning a finished stereo track must not make it louder.
        for p in [-1.0, -0.5, 0.0, 0.25, 1.0] {
            t.pan = p;
            let (l, r) = t.pan_gains();
            assert!(l <= 1.0 && r <= 1.0, "pan {p} boosted to ({l}, {r})");
        }
        // Out of range is clamped rather than inverted.
        t.pan = 9.0;
        assert_eq!(t.pan_gains(), (0.0, 1.0));
    }

    #[test]
    fn every_transition_kind_round_trips_and_knows_its_family() {
        for k in TransitionKind::ALL {
            assert_eq!(TransitionKind::parse(k.as_str()), Some(k), "{k:?} must survive the wire");
            assert!(
                TransitionKind::wire_names().contains(k.as_str()),
                "{k:?} must be listed for a caller"
            );
            // Exactly one family each: a dip has a colour and never moves, a
            // motion transition moves and never dips, a dissolve does neither.
            assert!(
                !(k.dip_color().is_some() && k.slide_from().is_some()),
                "{k:?} cannot both dip and travel"
            );
            assert_eq!(k.dip_color().is_some(), !k.overlaps(), "{k:?}: only a dip skips the overlap");
            assert!(!k.pushes() || k.slide_from().is_some(), "{k:?}: a push must have a direction");
        }
        // A slide and its push travel the same way; the difference is what
        // happens to the outgoing clip, not where the incoming one comes from.
        assert_eq!(TransitionKind::SlideLeft.slide_from(), TransitionKind::PushLeft.slide_from());
        assert!(!TransitionKind::SlideLeft.pushes() && TransitionKind::PushLeft.pushes());
        assert_eq!(TransitionKind::parse("nonsense"), None);
    }

    #[test]
    fn captions_follow_a_trimmed_and_moved_clip() {
        let asset = Uuid::new_v4();
        // The interesting case: the transcript says 30s, the cut says 0s.
        let timeline = one_clip(Clip::new(asset, 30.0, 34.0, 0.0));
        let lines = captioned(&timeline, asset, vec![seg(30.0, 34.0, "one two three four")]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, "one two three four");
        // Timeline time, not the transcript's 30.0.
        assert!(lines[0].1.abs() < 1e-9, "{lines:?}");
        assert!((lines[0].2 - 4.0).abs() < 1e-9, "{lines:?}");
    }

    #[test]
    fn a_sentence_cut_in_half_only_captions_what_survived() {
        let asset = Uuid::new_v4();
        // A four-word line spoken over 0..4s, but the cut keeps only 0..2s.
        let timeline = one_clip(Clip::new(asset, 0.0, 2.0, 0.0));
        let lines = captioned(
            &timeline,
            asset,
            vec![seg(0.0, 4.0, "alpha bravo charlie delta echo foxtrot")],
        );
        assert!(!lines.is_empty());
        // Nothing runs past the end of the clip that carries it.
        for (text, start, end) in &lines {
            assert!(*end <= 2.0 + 1e-9, "{text:?} runs to {end} past the clip");
            assert!(*start >= -1e-9);
        }
        // The words spoken in the discarded half are gone.
        assert!(!lines.iter().any(|(t, _, _)| t.contains("foxtrot")), "{lines:?}");
    }

    #[test]
    fn long_segments_split_into_readable_lines() {
        let asset = Uuid::new_v4();
        let timeline = one_clip(Clip::new(asset, 0.0, 8.0, 0.0));
        let lines = captioned(
            &timeline,
            asset,
            vec![seg(0.0, 8.0, "Today we are talking about non-destructive editing in Kerf")],
        );
        assert!(lines.len() > 1, "a ten-word sentence should not be one caption: {lines:?}");
        for (text, _, _) in &lines {
            assert!(text.split_whitespace().count() <= 4, "{text:?} is too many words");
        }
        // The lines are contiguous, in order, and cover the segment.
        assert!(lines[0].1.abs() < 1e-9);
        assert!((lines.last().unwrap().2 - 8.0).abs() < 1e-9);
        for pair in lines.windows(2) {
            assert!(pair[1].1 >= pair[0].1);
        }
        // Rejoining the lines gives the sentence back, word for word.
        let rejoined = lines.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>().join(" ");
        assert_eq!(rejoined, "Today we are talking about non-destructive editing in Kerf");
    }

    #[test]
    fn fast_speech_merges_rather_than_flickering() {
        let asset = Uuid::new_v4();
        // Eight words in 0.9s: split four ways each line would last ~0.22s.
        let timeline = one_clip(Clip::new(asset, 0.0, 0.9, 0.0));
        let lines = captioned(&timeline, asset, vec![seg(0.0, 0.9, "a b c d e f g h")]);
        for (text, start, end) in &lines {
            assert!(
                end - start >= MIN_CAPTION - 1e-6 || lines.len() == 1,
                "{text:?} flashes for {}s",
                end - start
            );
        }
        // No words were lost to the merging.
        let rejoined = lines.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>().join(" ");
        assert_eq!(rejoined, "a b c d e f g h");
    }

    #[test]
    fn captions_are_ordered_by_the_cut_not_by_the_source() {
        let asset = Uuid::new_v4();
        // The second half of the source is cut to play first.
        let mut track = Track::new(StreamKind::Video, "V1");
        track.clips = vec![Clip::new(asset, 10.0, 12.0, 0.0), Clip::new(asset, 0.0, 2.0, 2.0)];
        let timeline = Timeline {
            tracks: vec![track],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        };
        let lines = captioned(&timeline, asset, vec![seg(0.0, 2.0, "first"), seg(10.0, 12.0, "second")]);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0].0, "second", "the reordered cut leads with the later words");
        assert_eq!(lines[1].0, "first");
        assert!(lines[0].1.abs() < 1e-9);
        assert!((lines[1].1 - 2.0).abs() < 1e-9);
    }

    #[test]
    fn two_captions_never_share_the_screen() {
        let asset = Uuid::new_v4();
        // The same footage twice in the cut at different offsets — a callback
        // shot, or a full source parked under the edit. Both would caption the
        // same words on top of each other.
        let mut track = Track::new(StreamKind::Video, "V1");
        track.clips = vec![Clip::new(asset, 3.0, 12.0, 0.0), Clip::new(asset, 0.0, 12.0, 0.0)];
        let timeline = Timeline {
            tracks: vec![track],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        };
        let lines = captioned(
            &timeline,
            asset,
            vec![seg(0.0, 6.0, "alpha bravo charlie"), seg(6.0, 12.0, "delta echo foxtrot")],
        );
        assert!(lines.len() > 1, "{lines:?}");
        for pair in lines.windows(2) {
            assert!(
                pair[1].1 >= pair[0].2 - 1e-6,
                "{:?} starts before {:?} is off screen",
                pair[1],
                pair[0]
            );
        }
    }

    #[test]
    fn word_punch_puts_one_word_on_screen_at_a_time() {
        let asset = Uuid::new_v4();
        let timeline = one_clip(Clip::new(asset, 0.0, 5.0, 0.0));
        let mut map = HashMap::new();
        map.insert(asset, vec![seg(0.0, 5.0, "alpha bravo charlie delta echo")]);
        let punched = timeline.captions(&map, CaptionOptions::styled(CaptionStyle::WordPunch));
        assert_eq!(
            punched.iter().map(|o| o.text.as_str()).collect::<Vec<_>>(),
            ["alpha", "bravo", "charlie", "delta", "echo"]
        );
        // The whole look, not just the word count: the line style would leave
        // one word at subtitle size on the bottom edge.
        let layout = CaptionStyle::WordPunch.layout();
        assert!(punched
            .iter()
            .all(|o| o.bold && o.size == layout.size && o.pos_y == layout.pos_y));
        // Each word hands the screen to the next with no gap and no overlap.
        for pair in punched.windows(2) {
            assert!((pair[1].start - pair[0].end).abs() < 1e-9, "{:?}", (&pair[0], &pair[1]));
        }
        // The default style is untouched by any of this.
        let lines = timeline.captions(&map, CaptionOptions::default());
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().all(|o| !o.bold));
    }

    #[test]
    fn a_word_too_short_to_read_joins_its_neighbour() {
        let asset = Uuid::new_v4();
        let timeline = one_clip(Clip::new(asset, 0.0, 2.0, 0.0));
        let mut map = HashMap::new();
        // "a" is one character of thirty, so its character share is ~0.07s —
        // two frames, which is a flicker rather than a word.
        map.insert(asset, vec![seg(0.0, 2.0, "a fairly quickly spoken sentence")]);
        let punched = timeline.captions(&map, CaptionOptions::styled(CaptionStyle::WordPunch));
        assert!(
            punched.iter().all(|o| o.end - o.start >= MIN_WORD_CAPTION - 1e-6),
            "{:?}",
            punched.iter().map(|o| (&o.text, o.end - o.start)).collect::<Vec<_>>()
        );
        assert_eq!(punched[0].text, "a fairly", "the flicker merges instead of being dropped");
    }

    #[test]
    fn an_override_moves_one_number_and_leaves_the_style_alone() {
        let asset = Uuid::new_v4();
        let timeline = one_clip(Clip::new(asset, 0.0, 5.0, 0.0));
        let mut map = HashMap::new();
        map.insert(asset, vec![seg(0.0, 5.0, "alpha bravo charlie delta echo")]);
        let opts = CaptionOptions {
            size: Some(0.2),
            ..CaptionOptions::styled(CaptionStyle::WordPunch)
        };
        let punched = timeline.captions(&map, opts);
        assert_eq!(punched.len(), 5, "still one word each");
        assert!(punched.iter().all(|o| o.size == 0.2 && o.bold));
        assert!(punched.iter().all(|o| o.pos_y == CaptionStyle::WordPunch.layout().pos_y));
        // An unusable override falls back to the style rather than through it.
        let junk = CaptionOptions {
            size: Some(f64::NAN),
            pos_y: Some(9.0),
            ..CaptionOptions::styled(CaptionStyle::WordPunch)
        };
        let layout = junk.resolve();
        assert_eq!(layout.size, CaptionStyle::WordPunch.layout().size);
        assert_eq!(layout.pos_y, 1.0);
    }

    #[test]
    fn a_long_word_is_shrunk_to_fit_a_vertical_frame() {
        let asset = Uuid::new_v4();
        let mut timeline = one_clip(Clip::new(asset, 0.0, 4.0, 0.0));
        let mut map = HashMap::new();
        map.insert(asset, vec![seg(0.0, 4.0, "non-destructive editing")]);
        let opts = CaptionOptions::styled(CaptionStyle::WordPunch);
        let full = CaptionStyle::WordPunch.layout().size;

        // Unframed, so 16:9 — wide enough that nothing is shrunk, which is what
        // keeps every project that never picked a frame captioned as it was.
        let wide = timeline.captions(&map, opts);
        assert!(wide.iter().all(|o| o.size == full), "{wide:?}");

        // 9:16 is barely half as wide as it is tall, and `drawtext` neither
        // wraps nor scales: the long word would be drawn off both edges.
        timeline.format = Some(Delivery::new(1080, 1920, Fit::Cover));
        let tall = timeline.captions(&map, opts);
        let long = tall.iter().find(|o| o.text == "non-destructive").expect("the long word");
        let short = tall.iter().find(|o| o.text == "editing").expect("the short word");
        assert!(long.size < full, "the long word shrinks: {}", long.size);
        assert_eq!(short.size, full, "a word that already fits is left alone");
        let aspect = 1080.0 / 1920.0;
        assert!(
            long.text.chars().count() as f64 * CHAR_ADVANCE * long.size <= CAPTION_WIDTH * aspect + 1e-9,
            "still overflows: {}",
            long.size
        );
    }

    #[test]
    fn a_muted_track_is_not_captioned() {
        let asset = Uuid::new_v4();
        let mut timeline = one_clip(Clip::new(asset, 0.0, 4.0, 0.0));
        assert!(!captioned(&timeline, asset, vec![seg(0.0, 4.0, "heard")]).is_empty());
        timeline.tracks[0].muted = true;
        assert!(captioned(&timeline, asset, vec![seg(0.0, 4.0, "heard")]).is_empty());
    }

    #[test]
    fn the_same_words_on_two_tracks_are_captioned_once() {
        let asset = Uuid::new_v4();
        // What `extract_audio` leaves behind: picture and detached audio, both
        // referencing the same asset over the same source window.
        let mut video = Track::new(StreamKind::Video, "V1");
        video.clips = vec![Clip::new(asset, 0.0, 3.0, 0.0)];
        let mut audio = Track::new(StreamKind::Audio, "A1");
        audio.clips = vec![Clip::new(asset, 0.0, 3.0, 0.0)];
        let timeline = Timeline {
            tracks: vec![video, audio],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        };
        let lines = captioned(&timeline, asset, vec![seg(0.0, 3.0, "only once")]);
        assert_eq!(lines.len(), 1, "{lines:?}");
    }

    // ---- imported captions ----------------------------------------------------

    /// A cut with one `len`-second clip of `asset` from its start.
    fn cut_of(asset: Uuid, len: f64) -> Timeline {
        one_clip(Clip::new(asset, 0.0, len, 0.0))
    }

    fn rounded(overlays: &[TextOverlay]) -> Vec<(String, f64, f64)> {
        overlays
            .iter()
            .map(|o| {
                (
                    o.text.clone(),
                    (o.start * 100.0).round() / 100.0,
                    (o.end * 100.0).round() / 100.0,
                )
            })
            .collect()
    }

    /// Every cue offered is in exactly one bucket — the invariant the summary's
    /// numbers are built on.
    fn accounted(p: &CaptionPlacement, offered: usize) {
        assert_eq!(
            p.placed + p.dropped_outside + p.dropped_short + p.dropped_overlap,
            offered,
            "{p:?}"
        );
    }

    #[test]
    fn cues_in_timeline_time_land_where_the_file_says() {
        let timeline = cut_of(Uuid::new_v4(), 10.0);
        let cues = [seg(1.0, 3.0, "Hello there"), seg(4.0, 6.0, "General Kenobi")];
        let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
        assert_eq!(
            rounded(&p.overlays),
            [("Hello there".into(), 1.0, 3.0), ("General Kenobi".into(), 4.0, 6.0)]
        );
        accounted(&p, 2);
        assert_eq!((p.placed, p.dropped_outside, p.dropped_overlap), (2, 0, 0));
        // Written as generated captions in the style's own look, so Recaption,
        // Clear and the delivery re-fit all treat them as captions.
        let layout = CaptionStyle::Lines.layout();
        assert!(p
            .overlays
            .iter()
            .all(|o| o.generated && o.pos_y == layout.pos_y && o.bg.is_some()));
    }

    #[test]
    fn an_imported_cue_is_chunked_exactly_like_a_transcript_segment() {
        // The refactor's whole point: one chunking / timing / fitting path. A
        // cue in timeline time over an identity clip must come out the same as
        // the same words as a transcript segment of that clip — in both styles,
        // and on a tall frame where the fit binds.
        let asset = Uuid::new_v4();
        let mut timeline = cut_of(asset, 12.0);
        timeline.format = Some(Delivery::new(1080, 1920, Fit::Cover));
        let line = seg(
            0.5,
            11.0,
            "Today we are talking about non-destructive editing in Kerf, and a few extraordinarily long words",
        );
        let mut map = HashMap::new();
        map.insert(asset, vec![line.clone()]);
        for style in [CaptionStyle::Lines, CaptionStyle::WordPunch] {
            let opts = CaptionOptions::styled(style);
            let from_transcript = timeline.captions(&map, opts);
            let from_cues = timeline
                .place_cues(std::slice::from_ref(&line), CaptionTimeBase::Timeline, opts)
                .overlays;
            assert!(from_cues.len() > 1, "{style:?} should split a long cue");
            assert_eq!(from_cues.len(), from_transcript.len(), "{style:?}");
            for (a, b) in from_cues.iter().zip(&from_transcript) {
                assert_eq!(
                    (&a.text, a.start, a.end, a.size, a.pos_y, a.bold, &a.bg, a.generated),
                    (&b.text, b.start, b.end, b.size, b.pos_y, b.bold, &b.bg, b.generated),
                    "{style:?}"
                );
            }
        }
        // And the same asset-based placement is `captions` itself.
        let via_source = timeline.place_cues(&[line], CaptionTimeBase::Source(asset), CaptionOptions::default());
        assert_eq!(
            rounded(&via_source.overlays),
            rounded(&timeline.captions(&map, CaptionOptions::default()))
        );
    }

    #[test]
    fn imported_captions_shrink_to_fit_a_vertical_frame_and_take_the_style() {
        let mut timeline = cut_of(Uuid::new_v4(), 6.0);
        timeline.format = Some(Delivery::new(1080, 1920, Fit::Cover));
        let cues = [seg(0.0, 6.0, "non-destructive editing")];
        let p = timeline.place_cues(
            &cues,
            CaptionTimeBase::Timeline,
            CaptionOptions::styled(CaptionStyle::WordPunch),
        );
        let layout = CaptionStyle::WordPunch.layout();
        let long = p
            .overlays
            .iter()
            .find(|o| o.text == "non-destructive")
            .expect("the long word");
        let short = p.overlays.iter().find(|o| o.text == "editing").expect("the short word");
        assert!(long.size < layout.size, "the long word shrinks to fit 9:16: {}", long.size);
        assert_eq!(short.size, layout.size);
        assert!(p.overlays.iter().all(|o| o.bold && o.pos_y == layout.pos_y));
        // An override still moves one number.
        let opts = CaptionOptions {
            pos_y: Some(0.5),
            ..CaptionOptions::styled(CaptionStyle::WordPunch)
        };
        let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, opts);
        assert!(p.overlays.iter().all(|o| o.pos_y == 0.5 && o.bold));
    }

    #[test]
    fn timeline_cues_past_the_end_of_the_cut_are_dropped_and_the_straddler_clipped() {
        let timeline = cut_of(Uuid::new_v4(), 10.0);
        let cues = [
            seg(2.0, 4.0, "inside"),
            seg(9.0, 12.0, "straddles the end"),
            seg(10.0, 12.0, "starts at the end"),
            seg(20.0, 25.0, "long gone"),
        ];
        let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
        accounted(&p, 4);
        assert_eq!((p.placed, p.dropped_outside), (2, 2));
        let last = p.overlays.last().unwrap();
        assert!(last.text.contains("end") && (last.end - 10.0).abs() < 1e-9, "{last:?}");
        assert!(p.overlays.iter().all(|o| o.end <= 10.0 + 1e-9));
        // A sliver under the readable floor at the very end is not kept either.
        let sliver = timeline.place_cues(
            &[seg(9.95, 12.0, "barely")],
            CaptionTimeBase::Timeline,
            CaptionOptions::default(),
        );
        assert!(sliver.overlays.is_empty());
        // It met the cut, for too short a moment to read: not "outside" it.
        assert_eq!((sliver.dropped_outside, sliver.dropped_short), (0, 1));
        accounted(&sliver, 1);
    }

    #[test]
    fn timeline_cues_on_an_empty_timeline_have_no_end_to_run_past() {
        let timeline = Timeline::new();
        let cues = [seg(0.0, 2.0, "first"), seg(600.0, 602.0, "much later")];
        let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
        assert_eq!((p.placed, p.dropped_outside), (2, 0));
    }

    #[test]
    fn a_muted_track_silences_source_cues_but_not_timeline_cues() {
        let asset = Uuid::new_v4();
        let mut timeline = cut_of(asset, 6.0);
        let mut music = Track::new(StreamKind::Audio, "A1");
        music.clips = vec![Clip::new(Uuid::new_v4(), 0.0, 20.0, 0.0)];
        timeline.tracks.push(music);
        let cues = [seg(1.0, 3.0, "heard")];
        // Source time is a claim about footage: a muted clip shows nothing.
        timeline.tracks[0].muted = true;
        let src = timeline.place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default());
        assert!(src.overlays.is_empty());
        assert_eq!(src.dropped_outside, 1);
        // Timeline time is a claim about the finished cut. With the picture muted
        // the cut renders no further than its remaining content — here, the
        // music — and the cue is still inside it.
        let tl = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
        assert_eq!(tl.placed, 1);
        // A disabled clip is as absent as a muted track.
        timeline.tracks[0].muted = false;
        timeline.tracks[0].clips[0].enabled = false;
        assert!(timeline
            .place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default())
            .overlays
            .is_empty());
    }

    #[test]
    fn source_cues_follow_trim_move_speed_and_reverse() {
        let asset = Uuid::new_v4();
        let cues = [seg(12.0, 14.0, "alpha beta")];
        let place = |clip: Clip| {
            one_clip(clip)
                .place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default())
                .overlays
        };
        // Trimmed to 10..20 and moved to 4s: the cue's 12..14 is 6..8.
        let trimmed = Clip::new(asset, 10.0, 20.0, 4.0);
        assert_eq!(rounded(&place(trimmed.clone())), [("alpha beta".into(), 6.0, 8.0)]);
        // Double speed halves the distance from the in-point.
        let fast = Clip {
            speed: 2.0,
            ..trimmed.clone()
        };
        assert_eq!(rounded(&place(fast)), [("alpha beta".into(), 5.0, 6.0)]);
        // Reversed: the source's tail is heard first and the range stays ordered.
        let reversed = Clip {
            speed: -1.0,
            ..trimmed.clone()
        };
        assert_eq!(rounded(&place(reversed)), [("alpha beta".into(), 10.0, 12.0)]);
        // A cue entirely before the in-point shows nothing.
        let early = one_clip(trimmed).place_cues(
            &[seg(0.0, 5.0, "trimmed away")],
            CaptionTimeBase::Source(asset),
            CaptionOptions::default(),
        );
        assert!(early.overlays.is_empty());
        assert_eq!(early.dropped_outside, 1);
    }

    #[test]
    fn a_source_cue_cut_in_half_only_captions_what_survived() {
        let asset = Uuid::new_v4();
        let timeline = one_clip(Clip::new(asset, 0.0, 2.0, 0.0));
        let cues = [seg(0.0, 4.0, "alpha bravo charlie delta echo foxtrot")];
        let p = timeline.place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default());
        assert!(p.overlays.iter().all(|o| o.end <= 2.0 + 1e-9), "{:?}", p.overlays);
        assert!(!p.overlays.iter().any(|o| o.text.contains("foxtrot")), "{:?}", p.overlays);
        assert_eq!(rounded(&p.overlays).len(), 1);
        assert_eq!(p.placed, 1);

        // Several lines survive when more of the cue does: placed counts cues,
        // not the lines a long one becomes.
        let longer = one_clip(Clip::new(asset, 0.0, 6.0, 0.0));
        let cues = [seg(
            0.0,
            8.0,
            "one two three four five six seven eight nine ten eleven twelve",
        )];
        let p = longer.place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default());
        assert!(p.overlays.len() > 1, "{:?}", p.overlays);
        assert_eq!(p.placed, 1, "one cue, however many lines it became");
        accounted(&p, 1);
    }

    #[test]
    fn source_cues_reach_every_clip_of_the_asset_and_only_that_asset() {
        let asset = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mut track = Track::new(StreamKind::Video, "V1");
        // The same footage twice (a callback shot) with someone else's in between.
        track.clips = vec![
            Clip::new(asset, 0.0, 4.0, 0.0),
            Clip::new(other, 0.0, 4.0, 4.0),
            Clip::new(asset, 0.0, 4.0, 8.0),
        ];
        let timeline = Timeline {
            tracks: vec![track],
            ..Timeline::new()
        };
        let p = timeline.place_cues(
            &[seg(1.0, 3.0, "said twice")],
            CaptionTimeBase::Source(asset),
            CaptionOptions::default(),
        );
        assert_eq!(
            rounded(&p.overlays),
            [("said twice".into(), 1.0, 3.0), ("said twice".into(), 9.0, 11.0)]
        );
        assert_eq!(p.placed, 1, "still one cue");
        accounted(&p, 1);
    }

    #[test]
    fn imported_cues_never_share_the_screen() {
        let timeline = cut_of(Uuid::new_v4(), 20.0);
        let cues = [
            seg(0.0, 4.0, "alpha beta"),
            // Starts under the first: waits for it to end.
            seg(2.0, 6.0, "gamma delta"),
            // Wholly under what is already on screen: nothing readable is left.
            seg(3.0, 3.4, "buried"),
            // The same words at the same moment as an earlier cue.
            seg(8.0, 10.0, "twice over"),
            seg(8.0, 10.0, "twice over"),
        ];
        let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
        accounted(&p, 5);
        assert_eq!((p.placed, p.dropped_outside, p.dropped_overlap), (3, 0, 2), "{p:?}");
        assert_eq!(
            rounded(&p.overlays),
            [
                ("alpha beta".into(), 0.0, 4.0),
                ("gamma delta".into(), 4.0, 6.0),
                ("twice over".into(), 8.0, 10.0)
            ]
        );
        for pair in p.overlays.windows(2) {
            assert!(pair[1].start >= pair[0].end - 1e-9);
        }
    }

    #[test]
    fn unusable_cues_count_as_not_placed_rather_than_vanishing() {
        let asset = Uuid::new_v4();
        let timeline = cut_of(asset, 10.0);
        let cues = [
            seg(1.0, 2.0, "fine"),
            seg(3.0, 3.0, "zero length"),
            seg(5.0, 4.0, "backwards"),
            seg(6.0, 7.0, "   "),
            seg(f64::NAN, 8.0, "not a time"),
        ];
        for base in [CaptionTimeBase::Timeline, CaptionTimeBase::Source(asset)] {
            let p = timeline.place_cues(&cues, base, CaptionOptions::default());
            accounted(&p, 5);
            assert_eq!((p.placed, p.dropped_outside), (1, 4), "{base:?}");
        }
    }

    #[test]
    fn a_cue_that_meets_the_cut_too_briefly_is_short_not_outside() {
        let asset = Uuid::new_v4();
        let timeline = one_clip(Clip::new(asset, 10.0, 20.0, 0.0));
        let cues = [
            seg(11.0, 13.0, "readable"),
            // Wholly on the footage, but a blink long.
            seg(14.0, 14.05, "a blink"),
            // Starts before the kept footage and leaves a sliver of it.
            seg(9.0, 10.05, "just inside"),
            // Not on the kept footage at all.
            seg(30.0, 32.0, "elsewhere"),
        ];
        let p = timeline.place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default());
        accounted(&p, 4);
        assert_eq!(
            (p.placed, p.dropped_short, p.dropped_outside, p.dropped_overlap),
            (1, 2, 1, 0),
            "{p:?}"
        );
        // On the timeline clock the same split: before 0 and after the end are
        // outside; a blink or a sliver at an edge is short.
        let cut = cut_of(asset, 10.0);
        let cues = [
            seg(1.0, 3.0, "readable"),
            seg(4.0, 4.05, "a blink"),
            seg(-5.0, 0.05, "starts early"),
            seg(-5.0, -1.0, "before the start"),
            seg(9.96, 14.0, "at the end"),
            seg(10.0, 12.0, "after the end"),
        ];
        let p = cut.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
        accounted(&p, 6);
        assert_eq!((p.placed, p.dropped_short, p.dropped_outside), (1, 3, 2), "{p:?}");
    }

    #[test]
    fn simultaneous_imported_cues_keep_the_order_of_the_file() {
        let asset = Uuid::new_v4();
        let timeline = cut_of(asset, 10.0);
        // Two cues at one timecode: the one written first holds the slot.
        let cues = [seg(1.0, 4.0, "zebra"), seg(1.0, 4.0, "apple")];
        let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
        assert_eq!(rounded(&p.overlays), [("zebra".into(), 1.0, 4.0)]);
        accounted(&p, 2);
        assert_eq!(p.dropped_overlap, 1);
        // …through a clip as well…
        let p = timeline.place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default());
        assert_eq!(rounded(&p.overlays), [("zebra".into(), 1.0, 4.0)]);
        // …and an identical pair is still collapsed even with another between.
        let cues = [seg(1.0, 3.0, "same"), seg(1.0, 3.0, "other"), seg(1.0, 3.0, "same")];
        let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
        accounted(&p, 3);
        assert_eq!(p.placed, 1, "{p:?}");
        // A transcript has no order of its own and still sorts alphabetically —
        // the behavior every existing cut was captioned with.
        let mut map = HashMap::new();
        map.insert(asset, cues[..1].to_vec());
        map.insert(asset, vec![seg(1.0, 4.0, "zebra"), seg(1.0, 4.0, "apple")]);
        let from_transcript = timeline.captions(&map, CaptionOptions::default());
        assert_eq!(rounded(&from_transcript), [("apple".into(), 1.0, 4.0)]);
    }

    /// The line timer exactly as it was before it was made cheaper: everything
    /// recomputed — and every chunk cloned — on every pass.
    fn reference_time_chunks(chunks: Vec<String>, span: TimeRange, min: f64) -> Vec<(TimeRange, String)> {
        let mut chunks = chunks;
        let duration = (span.end - span.start).max(0.0);
        loop {
            let weights: Vec<f64> = chunks.iter().map(|c| c.chars().count().max(1) as f64).collect();
            let total: f64 = weights.iter().sum();
            let mut timed: Vec<(TimeRange, String)> = Vec::with_capacity(chunks.len());
            let mut at = span.start;
            for (i, text) in chunks.iter().enumerate() {
                let share = if total > 0.0 { weights[i] / total } else { 1.0 };
                let end = if i + 1 == chunks.len() {
                    span.end
                } else {
                    at + duration * share
                };
                timed.push((TimeRange { start: at, end }, text.clone()));
                at = end;
            }
            if chunks.len() < 2 {
                return timed;
            }
            let short = timed.iter().position(|(r, _)| r.end - r.start < min);
            let Some(i) = short else { return timed };
            let merge_back = i > 0 && (i + 1 == chunks.len() || chunks[i - 1].chars().count() <= chunks[i + 1].chars().count());
            let into = if merge_back { i - 1 } else { i };
            let moved = chunks.remove(into + 1);
            chunks[into] = format!("{}{}{}", chunks[into], ' ', moved);
        }
    }

    #[test]
    fn the_cheaper_line_timer_gives_exactly_the_old_answers() {
        // A deterministic pseudo-random sweep: chunk counts from none to dozens,
        // chunk widths from one letter to a long word, spans from nothing to
        // minutes and starting anywhere, every floor in use. Bit-for-bit equal,
        // since it decides where every transcript caption starts.
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        for case in 0..6_000 {
            let count = next(60) as usize;
            let chunks: Vec<String> = (0..count)
                .map(|_| {
                    let width = if next(5) == 0 { 30 } else { 8 };
                    "x".repeat(1 + next(width) as usize)
                })
                .collect();
            let start = (next(100_000) as f64) / 100.0 - 50.0;
            let len = match next(4) {
                0 => 0.0,
                1 => (next(100) as f64) / 1000.0,
                2 => (next(3000) as f64) / 100.0,
                _ => (next(100_000) as f64) / 10.0,
            };
            let span = TimeRange { start, end: start + len };
            let min = [MIN_CAPTION, MIN_WORD_CAPTION, 0.0, 5.0][next(4) as usize];
            let old = reference_time_chunks(chunks.clone(), span, min);
            let new = time_chunks(chunks, span, min);
            assert_eq!(old.len(), new.len(), "case {case}");
            for (a, b) in old.iter().zip(&new) {
                assert_eq!(
                    (a.0.start.to_bits(), a.0.end.to_bits(), &a.1),
                    (b.0.start.to_bits(), b.0.end.to_bits(), &b.1),
                    "case {case}: {span:?} min {min}"
                );
            }
        }
    }

    #[test]
    fn a_thousand_word_cue_in_word_punch_places_quickly() {
        // The worst import the caps allow: `MAX_CAPTION_WORDS` one-letter words in
        // cues as long as `MAX_CUE_CHARS` lets them be, each over a few seconds,
        // so every word is a flicker that has to be merged. Merging within a cue
        // is still quadratic in its word count (4x the words costs ~16x), which is
        // why a cue and an import are capped: the worst case is bounded, not the
        // algorithm. It runs in well under half a second unoptimized; the limit is
        // ~20x that because the suite runs in parallel on a busy machine — it has
        // to catch a lost cap or a much slower merge, never the load. (The old
        // timer rebuilt and cloned every chunk per merge: about 3 s for this.)
        use crate::captions_import::{MAX_CAPTION_WORDS, MAX_CUE_CHARS};
        let words_per_cue = MAX_CUE_CHARS.div_ceil(2);
        let cue_count = MAX_CAPTION_WORDS / words_per_cue;
        let asset = Uuid::new_v4();
        let timeline = cut_of(asset, cue_count as f64 * 10.0);
        let text = vec!["a"; words_per_cue].join(" ");
        assert!(text.chars().count() <= MAX_CUE_CHARS);
        let cues: Vec<TranscriptSegment> = (0..cue_count)
            .map(|i| seg(i as f64 * 10.0, i as f64 * 10.0 + 6.0, &text))
            .collect();
        let started = std::time::Instant::now();
        let p = timeline.place_cues(
            &cues,
            CaptionTimeBase::Timeline,
            CaptionOptions::styled(CaptionStyle::WordPunch),
        );
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_secs(8), "placing took {took:?}");
        accounted(&p, cue_count);
        assert_eq!(p.placed, cue_count);
        // Nothing was lost to the merging: every word is still there.
        let words: usize = p.overlays.iter().map(|o| o.text.split_whitespace().count()).sum();
        assert_eq!(words, cue_count * words_per_cue);
    }

    #[test]
    fn placement_leaves_the_timeline_it_reads_alone() {
        let asset = Uuid::new_v4();
        let timeline = cut_of(asset, 10.0);
        let before = serde_json::to_string(&timeline).unwrap();
        timeline.place_cues(
            &[seg(0.0, 3.0, "x y z")],
            CaptionTimeBase::Timeline,
            CaptionOptions::default(),
        );
        assert_eq!(serde_json::to_string(&timeline).unwrap(), before);
    }

    #[test]
    fn a_time_base_resolves_from_loose_arguments_and_refuses_contradictions() {
        let asset = Uuid::new_v4();
        let resolve = CaptionTimeBase::resolve;
        assert_eq!(resolve(None, None).unwrap(), CaptionTimeBase::Timeline);
        assert_eq!(resolve(Some("timeline"), None).unwrap(), CaptionTimeBase::Timeline);
        assert_eq!(resolve(Some(" Timeline "), None).unwrap(), CaptionTimeBase::Timeline);
        assert_eq!(resolve(Some(""), None).unwrap(), CaptionTimeBase::Timeline);
        assert_eq!(
            resolve(None, Some(asset)).unwrap(),
            CaptionTimeBase::Source(asset),
            "an asset alone implies source"
        );
        assert_eq!(resolve(Some("SOURCE"), Some(asset)).unwrap(), CaptionTimeBase::Source(asset));
        assert!(resolve(Some("source"), None).unwrap_err().to_string().contains("asset_id"));
        assert!(resolve(Some("timeline"), Some(asset))
            .unwrap_err()
            .to_string()
            .contains("only applies"));
        let err = resolve(Some("sideways"), None).unwrap_err().to_string();
        assert!(err.contains("\"timeline\" or \"source\""), "{err}");
    }

    // ---- ripple ---------------------------------------------------------------

    /// A clip of `dur` seconds placed at `start`, cut from `[0, dur)` of nothing
    /// in particular.
    fn rclip(start: f64, dur: f64) -> Clip {
        Clip::new(Uuid::nil(), 0.0, dur, start)
    }

    /// One video track `V1` holding `clips`.
    fn one_lane(clips: Vec<Clip>) -> Timeline {
        Timeline {
            tracks: vec![track(StreamKind::Video, "V1", clips)],
            ..Timeline::new()
        }
    }

    /// Each clip of track `ti` as `(start, end)`, in lane order.
    fn spans_of(timeline: &Timeline, ti: usize) -> Vec<(f64, f64)> {
        timeline.tracks[ti]
            .clips
            .iter()
            .map(|c| (c.timeline_start, c.timeline_end()))
            .collect()
    }

    fn same_cut(a: &Timeline, b: &Timeline) -> bool {
        serde_json::to_string(a).unwrap() == serde_json::to_string(b).unwrap()
    }

    /// Run `edit` on a copy of `before` and ripple the result.
    fn rippled(before: &Timeline, edit: impl FnOnce(&mut Timeline)) -> (Timeline, Timeline) {
        let mut after = before.clone();
        edit(&mut after);
        let out = after.ripple_from(before);
        (after, out)
    }

    /// `a [0,4)  b [5,8)  c [10,12)` — a gap of 1 before `b` and 2 before `c`.
    fn gapped() -> (Timeline, [Uuid; 3]) {
        let (a, b, c) = (rclip(0.0, 4.0), rclip(5.0, 3.0), rclip(10.0, 2.0));
        let ids = [a.id, b.id, c.id];
        (one_lane(vec![a, b, c]), ids)
    }

    fn clip_mut(t: &mut Timeline, id: Uuid) -> &mut Clip {
        let (ti, ci) = t.locate(id).unwrap();
        &mut t.tracks[ti].clips[ci]
    }

    #[test]
    fn a_right_trim_that_shortens_pulls_later_clips_left_and_keeps_every_gap() {
        let (before, [a, ..]) = gapped();
        let (_, out) = rippled(&before, |t| clip_mut(t, a).source_out = 3.0);
        assert_eq!(spans_of(&out, 0), vec![(0.0, 3.0), (4.0, 7.0), (9.0, 11.0)]);
    }

    #[test]
    fn a_right_trim_that_lengthens_pushes_later_clips_right() {
        let (before, [a, ..]) = gapped();
        let (after, out) = rippled(&before, |t| clip_mut(t, a).source_out = 6.0);
        // Un-rippled, the lengthened clip would sit on top of `b`.
        assert_eq!(spans_of(&after, 0)[1], (5.0, 8.0));
        assert_eq!(spans_of(&out, 0), vec![(0.0, 6.0), (7.0, 10.0), (12.0, 14.0)]);
    }

    #[test]
    fn a_left_trim_keeps_the_clips_start_whether_or_not_the_right_edge_was_held() {
        let (before, [_, b, _]) = gapped();
        // The GUI's left-edge trim: later in-point, and a later start so the right
        // edge (8.0) stays put. Ripple keeps the *start* and pulls the rest in.
        let (after, held) = rippled(&before, |t| {
            let c = clip_mut(t, b);
            c.source_in = 1.0;
            c.timeline_start = 6.0;
        });
        assert_eq!(spans_of(&after, 0)[1], (6.0, 8.0));
        assert_eq!(spans_of(&held, 0), vec![(0.0, 4.0), (5.0, 7.0), (9.0, 11.0)]);

        // The same trim without moving the start gives the same cut.
        let (_, unmoved) = rippled(&before, |t| clip_mut(t, b).source_in = 1.0);
        assert!(same_cut(&held, &unmoved));
    }

    #[test]
    fn a_left_trim_that_lengthens_also_keeps_the_start() {
        // `b` is cut from [2, 5) of its source, so it has a second of handle on the left.
        let (a, b, c) = (rclip(0.0, 4.0), Clip::new(Uuid::nil(), 2.0, 5.0, 5.0), rclip(10.0, 2.0));
        let b_id = b.id;
        let before = one_lane(vec![a, b, c]);
        let (after, out) = rippled(&before, |t| {
            let c = clip_mut(t, b_id);
            c.source_in = 1.0;
            c.timeline_start = 4.0; // right edge held at 8.0
        });
        assert_eq!(spans_of(&after, 0)[1], (4.0, 8.0), "un-rippled it would sit on `a`");
        assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (5.0, 9.0), (11.0, 13.0)]);
    }

    #[test]
    fn a_speed_change_is_a_length_change() {
        let (before, [_, b, _]) = gapped();
        let (_, out) = rippled(&before, |t| clip_mut(t, b).speed = 2.0); // 3.0s -> 1.5s
        assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (5.0, 6.5), (8.5, 10.5)]);
    }

    #[test]
    fn removing_a_clip_closes_its_span_and_keeps_the_gaps_either_side() {
        let (before, [_, b, _]) = gapped();
        let (_, out) = rippled(&before, |t| t.tracks[0].clips.retain(|c| c.id != b));
        // 1s of gap before `b` and 2s after it: 3s between `a` and `c`, as there was.
        assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (7.0, 9.0)]);
    }

    #[test]
    fn removing_several_clips_closes_each_span_once() {
        let clips = vec![rclip(0.0, 2.0), rclip(2.0, 2.0), rclip(6.0, 2.0), rclip(10.0, 2.0)];
        let (a, c) = (clips[0].id, clips[2].id);
        let before = one_lane(clips);
        let (_, out) = rippled(&before, |t| t.tracks[0].clips.retain(|k| k.id != a && k.id != c));
        assert_eq!(spans_of(&out, 0), vec![(0.0, 2.0), (6.0, 8.0)]);
    }

    #[test]
    fn a_clip_added_onto_footage_pushes_it_and_everything_after_right() {
        let clips = vec![rclip(0.0, 4.0), rclip(4.0, 4.0), rclip(8.0, 4.0)];
        let before = one_lane(clips);
        // Dropped at the cut between `a` and `b`: a 2s insert.
        let (after, out) = rippled(&before, |t| t.tracks[0].clips.push(rclip(4.0, 2.0)));
        assert_eq!(after.tracks[0].clips[1].timeline_start, 4.0, "un-rippled it overlaps `b`");
        assert_eq!(
            spans_of(&out, 0),
            vec![(0.0, 4.0), (4.0, 6.0), (6.0, 10.0), (10.0, 14.0)],
            "kept in lane order, nothing overlapping"
        );
    }

    #[test]
    fn a_clip_added_over_the_head_of_a_clip_in_a_gap_pushes_by_its_whole_length() {
        let before = one_lane(vec![rclip(0.0, 2.0), rclip(10.0, 2.0), rclip(14.0, 2.0)]);
        // 5s at 8.0 runs into the head of the clip at 10.0.
        let (_, out) = rippled(&before, |t| t.tracks[0].clips.push(rclip(8.0, 5.0)));
        assert_eq!(spans_of(&out, 0), vec![(0.0, 2.0), (8.0, 13.0), (15.0, 17.0), (19.0, 21.0)]);
    }

    #[test]
    fn a_clip_that_fits_moves_nothing() {
        let before = one_lane(vec![rclip(0.0, 2.0), rclip(10.0, 2.0)]);
        // Into the gap, and onto the end of the track.
        let (after, out) = rippled(&before, |t| {
            t.tracks[0].clips.push(rclip(4.0, 2.0));
            t.tracks[0].clips.push(rclip(12.0, 3.0));
        });
        assert!(same_cut(&after, &out));
    }

    #[test]
    fn a_clip_added_inside_another_clip_is_left_as_the_edit_made_it() {
        // Resolving it would take a split, which ripple never does.
        let before = one_lane(vec![rclip(0.0, 10.0), rclip(10.0, 4.0)]);
        let (after, out) = rippled(&before, |t| t.tracks[0].clips.push(rclip(4.0, 2.0)));
        assert!(same_cut(&after, &out), "no ripple for the track — not half of one");
    }

    #[test]
    fn adjacent_adds_push_once_by_their_combined_length() {
        let before = one_lane(vec![rclip(0.0, 4.0), rclip(4.0, 4.0)]);
        let (_, out) = rippled(&before, |t| {
            t.tracks[0].clips.push(rclip(4.0, 2.0));
            t.tracks[0].clips.push(rclip(6.0, 3.0));
        });
        assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (4.0, 6.0), (6.0, 9.0), (9.0, 13.0)]);
    }

    #[test]
    fn replacing_a_clip_in_place_moves_nothing() {
        let (before, [_, b, _]) = gapped();
        let (after, out) = rippled(&before, |t| {
            t.tracks[0].clips.retain(|c| c.id != b);
            t.tracks[0].clips.insert(1, rclip(5.0, 3.0));
        });
        assert!(same_cut(&after, &out), "-3s for the removal and +3s for the add cancel");
    }

    #[test]
    fn a_split_shifts_nothing() {
        let (before, [a, ..]) = gapped();
        let (after, out) = rippled(&before, |t| {
            // What `Project::split_at` does at 1.5.
            let left = clip_mut(t, a);
            let mut right = left.clone();
            right.id = Uuid::new_v4();
            right.timeline_start = 1.5;
            right.source_in = 1.5;
            left.source_out = 1.5;
            t.tracks[0].clips.insert(1, right);
        });
        assert_eq!(after.tracks[0].clips.len(), 4);
        assert!(same_cut(&after, &out));
    }

    #[test]
    fn a_move_does_not_ripple_within_a_track_or_across_tracks() {
        let (mut before, [a, b, _]) = gapped();
        before
            .tracks
            .insert(1, track(StreamKind::Video, "V2", vec![rclip(20.0, 2.0)]));

        // Within the track: `a` slides into the gap.
        let (after, out) = rippled(&before, |t| clip_mut(t, a).timeline_start = 0.5);
        assert!(same_cut(&after, &out));

        // Across tracks: `b` goes up to V2. V1 does not close behind it.
        let (after, out) = rippled(&before, |t| {
            let (ti, ci) = t.locate(b).unwrap();
            let moved = t.tracks[ti].clips.remove(ci);
            t.tracks[1].clips.insert(0, moved);
        });
        assert!(same_cut(&after, &out));
    }

    #[test]
    fn tracks_ripple_independently() {
        let (a, b, c) = (rclip(0.0, 4.0), rclip(5.0, 3.0), rclip(10.0, 2.0));
        let a_id = a.id;
        let audio = vec![rclip(0.0, 4.0), rclip(5.0, 3.0), rclip(10.0, 2.0)];
        let before = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![a, b, c]),
                track(StreamKind::Audio, "A1", audio),
            ],
            ..Timeline::new()
        };
        let (_, out) = rippled(&before, |t| clip_mut(t, a_id).source_out = 3.0);
        assert_eq!(spans_of(&out, 0), vec![(0.0, 3.0), (4.0, 7.0), (9.0, 11.0)]);
        assert_eq!(spans_of(&out, 1), spans_of(&before, 1), "no sync lock: the audio stays put");
    }

    #[test]
    fn a_locked_track_never_moves() {
        let (mut before, [a, ..]) = gapped();
        before.tracks[0].locked = true;
        let (after, out) = rippled(&before, |t| clip_mut(t, a).source_out = 3.0);
        assert!(same_cut(&after, &out));
    }

    #[test]
    fn an_edit_that_already_rippled_is_not_rippled_twice() {
        let (before, [_, b, _]) = gapped();
        // `Project::ripple_delete` of `b`: removed, and `c` closed up by 3.
        let (after, out) = rippled(&before, |t| {
            t.tracks[0].clips.retain(|c| c.id != b);
            t.tracks[0].clips[1].timeline_start -= 3.0;
        });
        assert_eq!(spans_of(&after, 0), vec![(0.0, 4.0), (7.0, 9.0)]);
        assert!(same_cut(&after, &out));

        // `Project::cut_clip_range` of the middle second of `a`: the head keeps
        // the id, the tail is a new clip, and everything later is moved left 1s.
        let (before, [a, ..]) = gapped();
        let (after, out) = rippled(&before, |t| {
            let head = clip_mut(t, a);
            let mut tail = head.clone();
            head.source_out = 1.0;
            tail.id = Uuid::new_v4();
            tail.source_in = 2.0;
            tail.timeline_start = 1.0;
            t.tracks[0].clips.insert(1, tail);
            for c in t.tracks[0].clips.iter_mut().skip(2) {
                c.timeline_start -= 1.0;
            }
        });
        assert_eq!(spans_of(&after, 0), vec![(0.0, 1.0), (1.0, 3.0), (4.0, 7.0), (9.0, 11.0)]);
        assert!(same_cut(&after, &out));
    }

    #[test]
    fn two_trims_in_one_edit_accumulate_down_the_track() {
        let (before, [a, b, _]) = gapped();
        let (_, out) = rippled(&before, |t| {
            clip_mut(t, a).source_out = 3.0; // -1s
            clip_mut(t, b).source_out = 2.0; // -1s, and `b` itself follows `a`
        });
        assert_eq!(spans_of(&out, 0), vec![(0.0, 3.0), (4.0, 6.0), (8.0, 10.0)]);
    }

    #[test]
    fn a_ripple_that_would_overlap_is_not_applied() {
        // V1: a [0,4)  r [4,6)  f [6,8).  The edit deletes `r` and drops a clip
        // from V2 into its place. Closing `f` up behind `r` would land it on top.
        let (a, r, f) = (rclip(0.0, 4.0), rclip(4.0, 2.0), rclip(6.0, 2.0));
        let (r_id, m) = (r.id, rclip(0.0, 2.0));
        let m_id = m.id;
        let before = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![a, r, f]),
                track(StreamKind::Video, "V2", vec![m]),
            ],
            ..Timeline::new()
        };
        let (after, out) = rippled(&before, |t| {
            t.tracks[0].clips.retain(|c| c.id != r_id);
            let mut moved = t.tracks[1].clips.remove(0);
            assert_eq!(moved.id, m_id);
            moved.timeline_start = 4.0;
            t.tracks[0].clips.insert(1, moved);
        });
        assert!(same_cut(&after, &out), "left exactly as the edit made it");
        assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (4.0, 6.0), (6.0, 8.0)]);
    }

    #[test]
    fn a_ripple_that_would_start_a_clip_before_zero_is_not_applied() {
        // An old project with overlapping clips: deleting both `r1` and `r2` would
        // close `f` up by 8s, which is more than there is room for.
        let (r1, r2, f) = (rclip(0.0, 4.0), rclip(1.0, 4.0), rclip(5.0, 2.0));
        let (r1_id, r2_id) = (r1.id, r2.id);
        let before = one_lane(vec![r1, r2, f]);
        let (after, out) = rippled(&before, |t| t.tracks[0].clips.retain(|c| c.id != r1_id && c.id != r2_id));
        assert!(same_cut(&after, &out));
        assert_eq!(spans_of(&out, 0), vec![(5.0, 7.0)]);
    }

    #[test]
    fn an_old_overlap_elsewhere_in_the_track_blocks_nothing() {
        // `a` and `a2` already overlap and nothing touches them.
        let (a, a2, b, c) = (rclip(0.0, 4.0), rclip(2.0, 4.0), rclip(10.0, 2.0), rclip(14.0, 2.0));
        let b_id = b.id;
        let before = one_lane(vec![a, a2, b, c]);
        let (_, out) = rippled(&before, |t| t.tracks[0].clips.retain(|k| k.id != b_id));
        assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (2.0, 6.0), (12.0, 14.0)]);
    }

    #[test]
    fn overlays_and_markers_stay_where_they_were() {
        let (mut before, [a, ..]) = gapped();
        before.overlays.push(TextOverlay::new("title", 6.0, 9.0));
        before.markers.push(Marker {
            id: Uuid::new_v4(),
            time: 7.0,
            name: "beat".to_string(),
            color: None,
        });
        let (_, out) = rippled(&before, |t| clip_mut(t, a).source_out = 3.0);
        assert_eq!(out.overlays[0].start, 6.0);
        assert_eq!(out.markers[0].time, 7.0);
        assert_eq!(spans_of(&out, 0)[1], (4.0, 7.0), "while the clips did move");
    }

    #[test]
    fn an_edit_that_changes_no_timing_comes_back_untouched() {
        let (before, [a, ..]) = gapped();
        let (after, out) = rippled(&before, |t| {
            clip_mut(t, a).volume = 0.4;
            t.tracks[0].muted = true;
        });
        assert!(same_cut(&after, &out));
        assert!(same_cut(&before.ripple_from(&before), &before));
    }

    #[test]
    fn rippling_twice_is_rippling_once() {
        let (before, [_, b, _]) = gapped();
        let (after, once) = rippled(&before, |t| clip_mut(t, b).source_out = 1.0);
        let twice = once.ripple_from(&before);
        assert!(same_cut(&once, &twice));
        assert!(!same_cut(&after, &once));
    }

    // ---- multi-clip moves and removals ------------------------------------------

    fn mv(clip: &Clip, start: f64) -> ClipMove {
        ClipMove {
            clip_id: clip.id,
            timeline_start: start,
            track_id: None,
        }
    }

    fn invalid(result: Result<Vec<Clip>>) -> String {
        match result {
            Err(Error::InvalidArgument(why)) => why,
            other => panic!("expected an InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn a_group_moves_together_and_may_pass_through_the_places_it_is_leaving() {
        // Three abutting clips nudged by a second: each lands on its neighbour's
        // old spot, which moving them one at a time would reject.
        let clips = vec![rclip(0.0, 2.0), rclip(2.0, 2.0), rclip(4.0, 2.0)];
        let mut t = one_lane(clips.clone());
        let moved = t
            .move_clips(&[mv(&clips[2], 5.0), mv(&clips[0], 1.0), mv(&clips[1], 3.0)])
            .unwrap();
        assert_eq!(
            moved.iter().map(|c| c.timeline_start).collect::<Vec<_>>(),
            vec![5.0, 1.0, 3.0],
            "reported in request order"
        );
        assert_eq!(spans_of(&t, 0), vec![(1.0, 3.0), (3.0, 5.0), (5.0, 7.0)], "lane re-sorted");
    }

    #[test]
    fn clips_can_swap_places() {
        let clips = vec![rclip(0.0, 2.0), rclip(2.0, 2.0)];
        let mut t = one_lane(clips.clone());
        t.move_clips(&[mv(&clips[0], 2.0), mv(&clips[1], 0.0)]).unwrap();
        assert_eq!(t.tracks[0].clips[0].id, clips[1].id);
        assert_eq!(spans_of(&t, 0), vec![(0.0, 2.0), (2.0, 4.0)]);
    }

    #[test]
    fn a_group_can_change_tracks_of_the_same_kind() {
        let a = rclip(0.0, 2.0);
        let mut t = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![a.clone()]),
                track(StreamKind::Video, "V2", vec![rclip(0.0, 1.0)]),
                track(StreamKind::Audio, "A1", vec![]),
            ],
            ..Timeline::new()
        };
        let (v2, a1) = (t.tracks[1].id, t.tracks[2].id);

        let up = ClipMove {
            track_id: Some(v2),
            ..mv(&a, 1.0)
        };
        t.move_clips(&[up]).unwrap();
        assert!(t.tracks[0].clips.is_empty());
        assert_eq!(spans_of(&t, 1), vec![(0.0, 1.0), (1.0, 3.0)]);

        let wrong_kind = ClipMove {
            track_id: Some(a1),
            ..mv(&a, 1.0)
        };
        assert!(invalid(t.move_clips(&[wrong_kind])).contains("different kind"));
    }

    #[test]
    fn a_move_that_cannot_land_refuses_the_whole_group_and_changes_nothing() {
        let clips = vec![rclip(0.0, 2.0), rclip(2.0, 2.0), rclip(10.0, 2.0)];
        let mut t = one_lane(clips.clone());
        let untouched = t.clone();
        let nowhere = Uuid::new_v4();

        // One good move and one that lands on the clip that is staying.
        let why = invalid(t.move_clips(&[mv(&clips[0], 5.0), mv(&clips[1], 9.5)]));
        assert!(why.contains("overlap") && why.contains("V1"), "{why}");
        // Two moved clips on top of each other.
        assert!(invalid(t.move_clips(&[mv(&clips[0], 5.0), mv(&clips[1], 6.0)])).contains("overlap"));
        // The same clip twice, a start before 0, a start that is not a number.
        assert!(invalid(t.move_clips(&[mv(&clips[0], 5.0), mv(&clips[0], 6.0)])).contains("more than once"));
        assert!(invalid(t.move_clips(&[mv(&clips[0], -1.0)])).contains("before the beginning"));
        assert!(invalid(t.move_clips(&[mv(&clips[0], f64::NAN)])).contains("finite"));
        assert!(invalid(t.move_clips(&[])).contains("no clips"));
        // An unknown clip and an unknown track.
        assert!(matches!(
            t.move_clips(&[mv(&clips[0], 5.0), mv(&rclip(0.0, 1.0), 6.0)]),
            Err(Error::ClipNotFound(_))
        ));
        assert!(matches!(
            t.move_clips(&[ClipMove {
                track_id: Some(nowhere),
                ..mv(&clips[0], 5.0)
            }]),
            Err(Error::TrackNotFound(id)) if id == nowhere
        ));
        // A locked track, as the source…
        let mut locked_lane = t.clone();
        locked_lane.tracks[0].locked = true;
        assert!(invalid(locked_lane.move_clips(&[mv(&clips[0], 5.0)])).contains("locked"));
        // …and as the destination.
        let mut two = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![clips[0].clone()]),
                Track {
                    locked: true,
                    ..Track::new(StreamKind::Video, "V2")
                },
            ],
            ..Timeline::new()
        };
        let v2 = two.tracks[1].id;
        let to_locked = ClipMove {
            track_id: Some(v2),
            ..mv(&clips[0], 0.0)
        };
        assert!(invalid(two.move_clips(&[to_locked])).contains("V2 is locked"));

        assert!(same_cut(&t, &untouched), "every refusal left the lane exactly as it was");
    }

    #[test]
    fn removing_several_clips_is_all_or_nothing() {
        let clips = vec![rclip(0.0, 2.0), rclip(2.0, 2.0), rclip(4.0, 2.0)];
        let mut t = one_lane(clips.clone());

        // A clip named twice goes once.
        assert_eq!(t.remove_clips(&[clips[0].id, clips[2].id, clips[0].id]).unwrap(), 2);
        assert_eq!(spans_of(&t, 0), vec![(2.0, 4.0)], "gaps are left");

        // One unknown id refuses the lot.
        let before = t.clone();
        assert!(matches!(
            t.remove_clips(&[clips[1].id, Uuid::new_v4()]),
            Err(Error::ClipNotFound(_))
        ));
        assert!(same_cut(&t, &before));
        assert!(matches!(t.remove_clips(&[]), Err(Error::InvalidArgument(_))));

        // So does a locked track.
        t.tracks[0].locked = true;
        assert!(matches!(t.remove_clips(&[clips[1].id]), Err(Error::InvalidArgument(why)) if why.contains("locked")));
        assert!(t.tracks[0].locked && t.tracks[0].clips.len() == 1);
    }

    #[test]
    fn clips_moved_since_says_whether_an_edit_rippled() {
        let (t, [a, b, c]) = gapped();
        assert_eq!(t.clips_moved_since(&t), 0);

        // A bare remove leaves a gap; ripple closes it and b and c follow.
        let (bare, out) = rippled(&t, |tl| tl.tracks[0].clips.retain(|x| x.id != a));
        assert_eq!(bare.clips_moved_since(&t), 0);
        assert_eq!(out.clips_moved_since(&t), 2);

        // Removing the last clip has nothing after it to move: ripple on, nothing rippled.
        let (_, out) = rippled(&t, |tl| tl.tracks[0].clips.retain(|x| x.id != c));
        assert_eq!(out.clips_moved_since(&t), 0);

        // A locked track never moves, so the same removal moves nothing there.
        let mut locked = t.clone();
        locked.tracks[0].locked = true;
        let (_, out) = rippled(&locked, |tl| tl.tracks[0].clips.retain(|x| x.id != a));
        assert_eq!(out.clips_moved_since(&locked), 0);

        // A clip on another track counts, a clip that exists on one side only does not.
        let mut two = t;
        two.tracks.push(track(StreamKind::Video, "V2", vec![]));
        let mut moved = two.clone();
        let clip = moved.tracks[0].clips.remove(1);
        assert_eq!(clip.id, b);
        moved.tracks[1].clips.push(clip);
        assert_eq!(moved.clips_moved_since(&two), 1);
        moved.tracks[1].clips.push(rclip(0.0, 1.0));
        assert_eq!(moved.clips_moved_since(&two), 1, "an added clip is not a moved one");
    }

    // ---- edit modes: roll, slip, slide, split-and-remove ------------------------

    /// Footage of `secs` for the asset every `sclip` / `rclip` points at.
    fn footage(secs: f64) -> SourceLimits {
        HashMap::from([(Uuid::nil(), secs)])
    }

    /// A clip of the shared test asset: source `[si, so)` placed at `start`.
    fn sclip(si: f64, so: f64, start: f64) -> Clip {
        Clip::new(Uuid::nil(), si, so, start)
    }

    fn window(c: &Clip) -> (f64, f64) {
        (c.source_in, c.source_out)
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} is not {b}");
    }

    /// The reason an op refused, with the timeline it was run on shown unchanged.
    fn refused<T: std::fmt::Debug>(result: Result<T>) -> String {
        match result {
            Err(Error::InvalidArgument(why)) => why,
            other => panic!("expected an InvalidArgument, got {other:?}"),
        }
    }

    fn kf(time: f64, scale: f64) -> Keyframe {
        Keyframe {
            time,
            scale,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        }
    }

    fn scales(c: &Clip) -> Vec<(f64, f64)> {
        c.keyframes.iter().map(|k| (k.time, k.scale)).collect()
    }

    /// `a [0,4)` src 10..14 abutting `b [4,8)` src 20..24.
    fn cut_pair() -> (Timeline, Uuid, Uuid) {
        let (a, b) = (sclip(10.0, 14.0, 0.0), sclip(20.0, 24.0, 4.0));
        let ids = (a.id, b.id);
        (one_lane(vec![a, b]), ids.0, ids.1)
    }

    #[test]
    fn a_roll_moves_the_cut_and_leaves_the_stretch_it_spans_alone() {
        let (mut t, a, b) = cut_pair();
        let out = t.roll_edit(a, b, 1.0, &footage(60.0)).unwrap();
        assert_eq!((out.requested, out.applied, out.clamped), (1.0, 1.0, false));
        assert_eq!(out.clips.len(), 2);
        let (ca, cb) = (&t.tracks[0].clips[0], &t.tracks[0].clips[1]);
        assert_eq!(window(ca), (10.0, 15.0), "the outgoing clip gained a second");
        assert_eq!(window(cb), (21.0, 24.0), "the incoming one gave it up");
        assert_eq!(spans_of(&t, 0), vec![(0.0, 5.0), (5.0, 8.0)], "the pair still spans 0..8");

        // And back past where it started: a negative delta rolls the cut earlier.
        t.roll_edit(a, b, -2.5, &footage(60.0)).unwrap();
        let (ca, cb) = (&t.tracks[0].clips[0], &t.tracks[0].clips[1]);
        assert_eq!(window(ca), (10.0, 12.5));
        assert_eq!(window(cb), (18.5, 24.0));
        assert_eq!(spans_of(&t, 0), vec![(0.0, 2.5), (2.5, 8.0)]);
    }

    #[test]
    fn a_roll_clamps_to_the_footage_each_clip_has_left_and_says_so() {
        // A 6 s asset: `a` (0..4) has 2 s after its out-point, `b` (1..5) has 1 s before its in-point.
        let (a, b) = (sclip(0.0, 4.0, 0.0), sclip(1.0, 5.0, 4.0));
        let (ia, ib) = (a.id, b.id);
        let t = one_lane(vec![a, b]);

        let mut later = t.clone();
        let out = later.roll_edit(ia, ib, 5.0, &footage(6.0)).unwrap();
        assert_eq!((out.requested, out.applied, out.clamped), (5.0, 2.0, true));
        assert_eq!(window(&later.tracks[0].clips[0]), (0.0, 6.0), "all the footage `a` had left");
        assert_eq!(later.tracks[0].clips[1].timeline_start, 6.0);

        let mut earlier = t.clone();
        let out = earlier.roll_edit(ia, ib, -5.0, &footage(6.0)).unwrap();
        assert_eq!((out.applied, out.clamped), (-1.0, true));
        assert_eq!(
            window(&earlier.tracks[0].clips[1]),
            (0.0, 5.0),
            "all the footage `b` had before it"
        );
        assert_eq!(spans_of(&earlier, 0), vec![(0.0, 3.0), (3.0, 8.0)]);

        // The range an interactive drag holds the pointer to is the same numbers.
        let range = t.roll_range(ia, ib, &footage(6.0)).unwrap();
        assert_eq!((range.min, range.max), (-1.0, 2.0));
    }

    #[test]
    fn a_roll_never_leaves_a_clip_shorter_than_the_floor() {
        let (a, b) = (sclip(10.0, 11.0, 0.0), sclip(20.0, 21.0, 1.0));
        let (ia, ib) = (a.id, b.id);
        let t = one_lane(vec![a, b]);

        let mut later = t.clone();
        near(later.roll_edit(ia, ib, 5.0, &footage(60.0)).unwrap().applied, 0.95);
        near(later.tracks[0].clips[1].duration(), MIN_EDIT_CLIP);

        let mut earlier = t;
        near(earlier.roll_edit(ia, ib, -5.0, &footage(60.0)).unwrap().applied, -0.95);
        near(earlier.tracks[0].clips[0].duration(), MIN_EDIT_CLIP);
    }

    #[test]
    fn a_roll_honors_speed() {
        // `a` runs at 2x over 0..8 (4 s on the timeline) in a 10 s asset: 2 source
        // seconds left are 1 timeline second. `b` runs at half speed.
        let mut a = sclip(0.0, 8.0, 0.0);
        a.speed = 2.0;
        let mut b = sclip(2.0, 3.0, 4.0); // 1 source s at 0.5x: 2 s long
        b.speed = 0.5;
        let (ia, ib) = (a.id, b.id);
        let t = one_lane(vec![a, b]);

        let mut later = t.clone();
        let out = later.roll_edit(ia, ib, 3.0, &footage(10.0)).unwrap();
        assert_eq!((out.applied, out.clamped), (1.0, true));
        let (ca, cb) = (&later.tracks[0].clips[0], &later.tracks[0].clips[1]);
        assert_eq!(window(ca), (0.0, 10.0), "one timeline second at 2x is two source seconds");
        near(cb.source_in, 2.5); // one timeline second at 0.5x is half a source second
        assert_eq!((cb.timeline_start, cb.duration()), (5.0, 1.0));

        let mut earlier = t;
        earlier.roll_edit(ia, ib, -1.0, &footage(10.0)).unwrap();
        let (ca, cb) = (&earlier.tracks[0].clips[0], &earlier.tracks[0].clips[1]);
        assert_eq!(window(ca), (0.0, 6.0));
        near(cb.source_in, 1.5);
        assert_eq!((cb.timeline_start, cb.duration()), (3.0, 3.0));
    }

    #[test]
    fn a_roll_honors_reverse_the_outgoing_edge_is_the_in_point() {
        // Both clips play backwards over 5..9. A reversed clip's *end* is its
        // in-point, so `a` extends downwards (5 s of footage below it), and `b`'s
        // start is its out-point.
        let (mut a, mut b) = (sclip(5.0, 9.0, 0.0), sclip(5.0, 9.0, 4.0));
        a.speed = -1.0;
        b.speed = -1.0;
        let (ia, ib) = (a.id, b.id);
        let t = one_lane(vec![a, b]);

        let mut later = t.clone();
        let out = later.roll_edit(ia, ib, 2.0, &footage(60.0)).unwrap();
        assert_eq!(out.applied, 2.0);
        assert_eq!(window(&later.tracks[0].clips[0]), (3.0, 9.0), "a: in-point down by 2");
        assert_eq!(window(&later.tracks[0].clips[1]), (5.0, 7.0), "b: out-point down by 2");
        assert_eq!(spans_of(&later, 0), vec![(0.0, 6.0), (6.0, 8.0)]);

        let mut earlier = t;
        earlier.roll_edit(ia, ib, -2.0, &footage(60.0)).unwrap();
        assert_eq!(window(&earlier.tracks[0].clips[0]), (7.0, 9.0), "a: in-point up by 2");
        assert_eq!(window(&earlier.tracks[0].clips[1]), (5.0, 11.0), "b: out-point up by 2");

        // The handle is the one on the *playing* side: here `a` has 1 s below its
        // in-point and `b` only half a second above its out-point.
        let (mut a, mut b) = (sclip(1.0, 5.0, 0.0), sclip(5.0, 59.5, 4.0));
        a.speed = -1.0;
        b.speed = -1.0;
        let (ia, ib) = (a.id, b.id);
        let t = one_lane(vec![a, b]);
        let range = t.roll_range(ia, ib, &footage(60.0)).unwrap();
        assert_eq!((range.min, range.max), (-0.5, 1.0));
    }

    #[test]
    fn a_roll_through_a_still_has_footage_without_limit_and_never_a_negative_window() {
        let still = Uuid::new_v4();
        let limits: SourceLimits = HashMap::from([(Uuid::nil(), 60.0), (still, f64::INFINITY)]);

        // The still is the incoming clip: rolling earlier lengthens it freely, written
        // on its out-point so the window never reaches below 0.
        let (a, b) = (sclip(10.0, 15.0, 0.0), Clip::new(still, 0.0, 5.0, 5.0));
        let (ia, ib) = (a.id, b.id);
        let mut t = one_lane(vec![a, b]);
        t.roll_edit(ia, ib, -2.0, &limits).unwrap();
        let cb = &t.tracks[0].clips[1];
        assert_eq!((cb.timeline_start, cb.duration()), (3.0, 7.0));
        assert_eq!(window(cb), (0.0, 7.0));

        // The still is the outgoing clip: nothing stops it growing but the other clip.
        let (a, b) = (Clip::new(still, 0.0, 5.0, 0.0), sclip(10.0, 15.0, 5.0));
        let (ia, ib) = (a.id, b.id);
        let mut t = one_lane(vec![a, b]);
        near(t.roll_edit(ia, ib, 20.0, &limits).unwrap().applied, 4.95);
        assert_eq!(window(&t.tracks[0].clips[0]).0, 0.0);
    }

    #[test]
    fn a_roll_needs_two_adjacent_clips_in_order_on_one_unlocked_track() {
        let (t, a, b) = cut_pair();
        let f = footage(60.0);
        let run = |name: &str, tl: &Timeline, ia: Uuid, ib: Uuid, delta: f64| {
            let mut copy = tl.clone();
            let why = refused(copy.roll_edit(ia, ib, delta, &f));
            assert!(same_cut(&copy, tl), "{name}: a refused roll must change nothing");
            why
        };

        assert!(run("swapped", &t, b, a, 1.0).contains("clip_a must be the earlier clip"));
        assert!(run("itself", &t, a, a, 1.0).contains("two different clips"));
        assert!(run("zero", &t, a, b, 0.0).contains("zero"));
        assert!(run("nan", &t, a, b, f64::NAN).contains("finite"));

        // A gap, an overlap.
        let mut gapped = t.clone();
        gapped.tracks[0].clips[1].timeline_start = 5.0;
        assert!(run("gap", &gapped, a, b, 1.0).contains("1.00s gap"));
        let mut overlapped = t.clone();
        overlapped.tracks[0].clips[1].timeline_start = 3.5;
        assert!(run("overlap", &overlapped, a, b, 1.0).contains("overlap by 0.50s"));

        // Different tracks.
        let two = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![t.tracks[0].clips[0].clone()]),
                track(StreamKind::Video, "V2", vec![t.tracks[0].clips[1].clone()]),
            ],
            ..Timeline::new()
        };
        assert!(run("tracks", &two, a, b, 1.0).contains("different tracks (V1 and V2)"));

        // Locked, and unknown ids.
        let mut locked = t.clone();
        locked.tracks[0].locked = true;
        assert!(run("locked", &locked, a, b, 1.0).contains("V1 is locked"));
        let mut copy = t.clone();
        assert!(matches!(
            copy.roll_edit(a, Uuid::new_v4(), 1.0, &f),
            Err(Error::ClipNotFound(_))
        ));

        // An asset the footage table does not know cannot be clamped.
        let mut copy = t;
        assert!(matches!(
            copy.roll_edit(a, b, 1.0, &SourceLimits::new()),
            Err(Error::AssetNotFound(_))
        ));
    }

    #[test]
    fn a_roll_that_clamps_to_nothing_is_an_error_that_says_why() {
        // `a` already ends at the end of its asset; `b` starts at the start of its.
        let (a, b) = (sclip(0.0, 6.0, 0.0), sclip(0.0, 3.0, 6.0));
        let (ia, ib) = (a.id, b.id);
        let mut t = one_lane(vec![a, b]);
        let before = t.clone();
        let why = refused(t.roll_edit(ia, ib, 1.0, &footage(6.0)));
        assert!(
            why.contains("cannot roll the cut later") && why.contains("outgoing clip has no footage left"),
            "{why}"
        );
        let why = refused(t.roll_edit(ia, ib, -1.0, &footage(6.0)));
        assert!(
            why.contains("cannot roll the cut earlier") && why.contains("incoming clip has no footage left"),
            "{why}"
        );
        assert!(same_cut(&t, &before));
    }

    #[test]
    fn two_clips_a_hair_apart_still_share_a_cut() {
        // The engine treats edges within a millisecond as touching (transition_fx).
        let (mut t, a, b) = cut_pair();
        t.tracks[0].clips[1].timeline_start = 4.0005;
        let out = t.roll_edit(a, b, 1.0, &footage(60.0)).unwrap();
        assert_eq!(out.applied, 1.0);
        near(t.tracks[0].clips[1].timeline_start, 5.0005);

        let (mut t, a, b) = cut_pair();
        t.tracks[0].clips[1].timeline_start = 4.002;
        assert!(refused(t.roll_edit(a, b, 1.0, &footage(60.0))).contains("not adjacent"));
    }

    #[test]
    fn a_roll_carries_the_incoming_clips_animation_with_its_footage() {
        let (mut t, a, b) = cut_pair();
        t.tracks[0].clips[0].keyframes = vec![kf(0.0, 1.0), kf(2.0, 3.0)];
        t.tracks[0].clips[1].keyframes = vec![kf(0.0, 1.0), kf(2.0, 2.0)];
        let mut rf = Reframe::new(Projection::Equirect);
        rf.keyframes = vec![
            ReframeKeyframe {
                time: 0.0,
                yaw: 0.0,
                pitch: 0.0,
                roll: 0.0,
                fov: 100.0,
            },
            ReframeKeyframe {
                time: 2.0,
                yaw: 90.0,
                pitch: 0.0,
                roll: 0.0,
                fov: 100.0,
            },
        ];
        t.tracks[0].clips[1].reframe = Some(rf);

        // Rolled later, b loses its first second: the pose it now opens on is
        // pinned at 0 and the later key moves up.
        let mut later = t.clone();
        later.roll_edit(a, b, 1.0, &footage(60.0)).unwrap();
        assert_eq!(scales(&later.tracks[0].clips[1]), vec![(0.0, 1.5), (1.0, 2.0)]);
        let rk: Vec<(f64, f64)> = later.tracks[0].clips[1]
            .reframe
            .as_ref()
            .unwrap()
            .keyframes
            .iter()
            .map(|k| (k.time, k.yaw))
            .collect();
        assert_eq!(rk, vec![(0.0, 45.0), (1.0, 90.0)], "the camera moves with the footage too");
        assert_eq!(
            scales(&later.tracks[0].clips[0]),
            vec![(0.0, 1.0), (2.0, 3.0)],
            "the outgoing clip's head did not move"
        );

        // Rolled earlier, b gains a second at its head: every key shifts later.
        let mut earlier = t;
        earlier.roll_edit(a, b, -1.0, &footage(60.0)).unwrap();
        assert_eq!(scales(&earlier.tracks[0].clips[1]), vec![(1.0, 1.0), (3.0, 2.0)]);
    }

    #[test]
    fn a_roll_keeps_fades_inside_a_clip_that_shrank_and_the_transition_on_the_cut() {
        let (mut t, a, b) = cut_pair();
        t.tracks[0].clips[0].fade_out = 2.0;
        t.tracks[0].clips[0].fade_in = 0.5;
        t.tracks[0].clips[1].fade_in = 3.0;
        let transition = Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        };
        t.tracks[0].clips[1].transition_in = Some(transition);

        t.roll_edit(a, b, -3.9, &footage(60.0)).unwrap(); // a is 0.1 s now
        let (ca, cb) = (&t.tracks[0].clips[0], &t.tracks[0].clips[1]);
        near(ca.fade_out, 0.1);
        near(ca.fade_in, 0.1);
        assert_eq!(cb.fade_in, 3.0, "b grew, so its fade stays");
        assert_eq!(cb.transition_in, Some(transition));

        t.roll_edit(a, b, 10.0, &footage(60.0)).unwrap(); // b shrinks to the floor
        near(t.tracks[0].clips[1].fade_in, MIN_EDIT_CLIP);
    }

    #[test]
    fn a_slip_moves_the_window_and_nothing_on_the_timeline() {
        let mut c = sclip(10.0, 14.0, 3.0);
        c.keyframes = vec![kf(0.0, 1.0), kf(2.0, 2.0)];
        c.fade_in = 0.5;
        let id = c.id;
        let mut t = one_lane(vec![c.clone()]);

        let out = t.slip_clip(id, 2.0, &footage(60.0)).unwrap();
        assert_eq!((out.applied, out.clamped), (2.0, false));
        let slipped = &t.tracks[0].clips[0];
        assert_eq!(window(slipped), (12.0, 16.0));
        assert_eq!((slipped.timeline_start, slipped.duration()), (3.0, 4.0));
        // Clip-local animation and the fades are timed to the clip, not to the footage.
        assert_eq!(slipped.keyframes, c.keyframes);
        assert_eq!(slipped.fade_in, 0.5);

        t.slip_clip(id, -5.0, &footage(60.0)).unwrap();
        assert_eq!(window(&t.tracks[0].clips[0]), (7.0, 11.0));
    }

    #[test]
    fn a_slip_clamps_to_the_asset_and_errors_at_the_edge() {
        let c = sclip(10.0, 14.0, 0.0);
        let id = c.id;
        let t = one_lane(vec![c]);

        let mut up = t.clone();
        let out = up.slip_clip(id, 10.0, &footage(20.0)).unwrap();
        assert_eq!((out.applied, out.clamped), (6.0, true));
        assert_eq!(window(&up.tracks[0].clips[0]), (16.0, 20.0));
        let why = refused(up.slip_clip(id, 1.0, &footage(20.0)));
        assert!(
            why.contains("cannot slip the footage later") && why.contains("after the clip's out-point"),
            "{why}"
        );

        let mut down = t.clone();
        near(down.slip_clip(id, -50.0, &footage(20.0)).unwrap().applied, -10.0);
        assert_eq!(window(&down.tracks[0].clips[0]), (0.0, 4.0));
        let why = refused(down.slip_clip(id, -1.0, &footage(20.0)));
        assert!(
            why.contains("cannot slip the footage earlier") && why.contains("before the clip's in-point"),
            "{why}"
        );

        let range = t.slip_range(id, &footage(20.0)).unwrap();
        assert_eq!((range.min, range.max), (-10.0, 6.0));
    }

    #[test]
    fn a_slip_is_in_source_seconds_whatever_the_speed() {
        let mut c = sclip(10.0, 14.0, 0.0);
        c.speed = 2.0; // 4 source seconds in 2 timeline seconds
        let id = c.id;
        let mut t = one_lane(vec![c]);
        t.slip_clip(id, 1.0, &footage(60.0)).unwrap();
        let slipped = &t.tracks[0].clips[0];
        assert_eq!(window(slipped), (11.0, 15.0), "one source second");
        assert_eq!((slipped.timeline_start, slipped.timeline_end()), (0.0, 2.0));
    }

    #[test]
    fn a_reversed_clip_slips_the_mirrored_way_so_the_sign_means_the_same_on_screen() {
        // Positive = starts later in its own footage. A reversed clip's footage
        // runs high to low, so "later" is the lower source time.
        let mut c = sclip(10.0, 14.0, 0.0);
        c.speed = -1.0;
        let id = c.id;
        let t = one_lane(vec![c]);

        let mut later = t.clone();
        later.slip_clip(id, 3.0, &footage(20.0)).unwrap();
        assert_eq!(
            window(&later.tracks[0].clips[0]),
            (7.0, 11.0),
            "positive moves the window down"
        );
        let mut earlier = t.clone();
        earlier.slip_clip(id, -3.0, &footage(20.0)).unwrap();
        assert_eq!(window(&earlier.tracks[0].clips[0]), (13.0, 17.0), "negative moves it up");

        // The footage left is on the opposite side for each sign.
        let range = t.slip_range(id, &footage(20.0)).unwrap();
        assert_eq!((range.min, range.max), (-6.0, 10.0));
        let mut clamped = t.clone();
        near(clamped.slip_clip(id, 100.0, &footage(20.0)).unwrap().applied, 10.0);
        assert_eq!(window(&clamped.tracks[0].clips[0]), (0.0, 4.0));
        let mut clamped = t;
        near(clamped.slip_clip(id, -100.0, &footage(20.0)).unwrap().applied, -6.0);
        assert_eq!(window(&clamped.tracks[0].clips[0]), (16.0, 20.0));
    }

    #[test]
    fn a_still_has_nothing_to_slip_and_the_other_refusals() {
        let still = Uuid::new_v4();
        let limits: SourceLimits = HashMap::from([(Uuid::nil(), 60.0), (still, f64::INFINITY)]);
        let c = Clip::new(still, 0.0, 5.0, 0.0);
        let id = c.id;
        let mut t = one_lane(vec![c]);
        assert!(refused(t.slip_clip(id, 1.0, &limits)).contains("still image"));
        assert!(refused(t.slip_range(id, &limits).map(|_| ())).contains("still image"));

        let (mut t, a, _) = cut_pair();
        assert!(refused(t.slip_clip(a, 0.0, &footage(60.0))).contains("zero"));
        assert!(refused(t.slip_clip(a, f64::INFINITY, &footage(60.0))).contains("finite"));
        assert!(matches!(
            t.slip_clip(Uuid::new_v4(), 1.0, &footage(60.0)),
            Err(Error::ClipNotFound(_))
        ));
        t.tracks[0].locked = true;
        assert!(refused(t.slip_clip(a, 1.0, &footage(60.0))).contains("V1 is locked"));
    }

    /// `p [0,4)` src 10..14, `c [4,7)` src 30..33, `n [7,12)` src 20..25, abutting.
    fn three() -> (Timeline, [Uuid; 3]) {
        let (p, c, n) = (sclip(10.0, 14.0, 0.0), sclip(30.0, 33.0, 4.0), sclip(20.0, 25.0, 7.0));
        let ids = [p.id, c.id, n.id];
        (one_lane(vec![p, c, n]), ids)
    }

    #[test]
    fn a_slide_moves_the_clip_and_the_neighbours_give_way() {
        let (t, [p, c, n]) = three();

        let mut later = t.clone();
        let out = later.slide_clip(c, 1.0, &footage(60.0)).unwrap();
        assert_eq!((out.requested, out.applied, out.clamped), (1.0, 1.0, false));
        assert_eq!(
            out.clips.iter().map(|x| x.id).collect::<Vec<_>>(),
            vec![p, c, n],
            "in timeline order"
        );
        assert_eq!(
            spans_of(&later, 0),
            vec![(0.0, 5.0), (5.0, 8.0), (8.0, 12.0)],
            "the three still span 0..12"
        );
        let cl = &later.tracks[0].clips;
        assert_eq!(window(&cl[0]), (10.0, 15.0), "the previous clip's out moved");
        assert_eq!(window(&cl[1]), (30.0, 33.0), "the slid clip's content did not");
        assert_eq!(window(&cl[2]), (21.0, 25.0), "the next clip's in moved");

        let mut earlier = t;
        earlier.slide_clip(c, -1.5, &footage(60.0)).unwrap();
        assert_eq!(spans_of(&earlier, 0), vec![(0.0, 2.5), (2.5, 5.5), (5.5, 12.0)]);
        let cl = &earlier.tracks[0].clips;
        assert_eq!(window(&cl[0]), (10.0, 12.5));
        assert_eq!(window(&cl[1]), (30.0, 33.0));
        assert_eq!(window(&cl[2]), (18.5, 25.0));
    }

    #[test]
    fn a_slide_clamps_to_the_neighbours_footage_and_floor() {
        let (t, [_, c, _]) = three();

        // Later: the next clip (5 s) may only shrink to the floor.
        let mut later = t.clone();
        let out = later.slide_clip(c, 10.0, &footage(60.0)).unwrap();
        assert_eq!((out.applied, out.clamped), (4.95, true));
        near(later.tracks[0].clips[2].duration(), MIN_EDIT_CLIP);
        // Earlier: the previous clip (4 s) may only shrink to the floor.
        let mut earlier = t.clone();
        near(earlier.slide_clip(c, -10.0, &footage(60.0)).unwrap().applied, -3.95);
        near(earlier.tracks[0].clips[0].duration(), MIN_EDIT_CLIP);

        // A 14 s asset: the previous clip (10..14) has no footage after it to grow into.
        let mut dry = t.clone();
        let why = refused(dry.slide_clip(c, 1.0, &footage(14.0)));
        assert!(
            why.contains("cannot slide the clip later") && why.contains("previous clip has no footage left"),
            "{why}"
        );
        // …but the other way is fine, and the next clip's footage before 20 is.
        near(dry.slide_clip(c, -1.0, &footage(14.0)).unwrap().applied, -1.0);

        // The next clip's head handle bounds the earlier direction too (n starts at source 20).
        let (mut tight, ids) = three();
        tight.tracks[0].clips[2].source_in = 0.5;
        let out = tight.slide_clip(ids[1], -2.0, &footage(60.0)).unwrap();
        assert_eq!((out.applied, out.clamped), (-0.5, true));
        let range = t.slide_range(c, &footage(60.0)).unwrap();
        assert_eq!((range.min, range.max), (-3.95, 4.95));
    }

    #[test]
    fn a_slide_beside_a_gap_trims_only_the_neighbour_that_touches() {
        // p [0,4) touches c [4,7); n starts at 9 — a 2 s gap after c.
        let (p, c, n) = (sclip(10.0, 14.0, 0.0), sclip(30.0, 33.0, 4.0), sclip(20.0, 25.0, 9.0));
        let ids = [p.id, c.id, n.id];
        let t = one_lane(vec![p, c, n]);

        let mut later = t.clone();
        let out = later.slide_clip(ids[1], 1.0, &footage(60.0)).unwrap();
        assert_eq!(
            out.clips.iter().map(|x| x.id).collect::<Vec<_>>(),
            vec![ids[0], ids[1]],
            "n is not touched"
        );
        let cl = &later.tracks[0].clips;
        assert_eq!(window(&cl[0]), (10.0, 15.0), "p grew to follow the clip");
        assert_eq!(cl[1].timeline_start, 5.0);
        assert_eq!(
            (cl[2].timeline_start, window(&cl[2])),
            (9.0, (20.0, 25.0)),
            "n stayed exactly as it was"
        );

        // It stops where it would meet n, which it is not touching, rather than trimming it.
        let mut far = t.clone();
        let out = far.slide_clip(ids[1], 5.0, &footage(60.0)).unwrap();
        assert_eq!((out.applied, out.clamped), (2.0, true));
        assert_eq!(far.tracks[0].clips[2].timeline_start, 9.0);
        assert_eq!(far.tracks[0].clips[1].timeline_end(), 9.0);

        // The other way the gap just grows; p gives way as before.
        let mut earlier = t;
        earlier.slide_clip(ids[1], -1.0, &footage(60.0)).unwrap();
        assert_eq!(spans_of(&earlier, 0), vec![(0.0, 3.0), (3.0, 6.0), (9.0, 14.0)]);
    }

    #[test]
    fn a_slide_after_a_gap_leaves_the_previous_clip_alone() {
        // p [0,3), a 1 s gap, c [4,7) touching n [7,12).
        let (p, c, n) = (sclip(10.0, 13.0, 0.0), sclip(30.0, 33.0, 4.0), sclip(20.0, 25.0, 7.0));
        let ids = [p.id, c.id, n.id];
        let t = one_lane(vec![p, c, n]);

        let mut earlier = t.clone();
        let out = earlier.slide_clip(ids[1], -0.5, &footage(60.0)).unwrap();
        assert_eq!(out.clips.iter().map(|x| x.id).collect::<Vec<_>>(), vec![ids[1], ids[2]]);
        let cl = &earlier.tracks[0].clips;
        assert_eq!((cl[0].timeline_end(), window(&cl[0])), (3.0, (10.0, 13.0)), "p untouched");
        assert_eq!(cl[1].timeline_start, 3.5);
        assert_eq!(
            (cl[2].timeline_start, window(&cl[2])),
            (6.5, (19.5, 25.0)),
            "n extends earlier"
        );

        let mut far = t.clone();
        let out = far.slide_clip(ids[1], -3.0, &footage(60.0)).unwrap();
        assert_eq!(
            (out.applied, out.clamped),
            (-1.0, true),
            "stops at p, which it is not touching"
        );
        assert_eq!(far.tracks[0].clips[1].timeline_start, 3.0);

        // Later, the gap grows and n is trimmed.
        let mut later = t;
        later.slide_clip(ids[1], 1.0, &footage(60.0)).unwrap();
        assert_eq!(spans_of(&later, 0), vec![(0.0, 3.0), (5.0, 8.0), (8.0, 12.0)]);
    }

    #[test]
    fn a_slide_at_the_ends_of_a_track() {
        // First clip: nothing before it, so it cannot go below 0…
        let (c, n) = (sclip(30.0, 33.0, 2.0), sclip(20.0, 25.0, 5.0));
        let ids = [c.id, n.id];
        let mut t = one_lane(vec![c, n]);
        let out = t.slide_clip(ids[0], -5.0, &footage(60.0)).unwrap();
        assert_eq!((out.applied, out.clamped), (-2.0, true));
        assert_eq!(spans_of(&t, 0), vec![(0.0, 3.0), (3.0, 10.0)], "n extended earlier to follow");
        let why = refused(t.slide_clip(ids[0], -1.0, &footage(60.0)));
        assert!(why.contains("already at the start of the timeline"), "{why}");
        // …and sliding it later opens a gap before it while n gives way.
        t.slide_clip(ids[0], 1.0, &footage(60.0)).unwrap();
        assert_eq!(spans_of(&t, 0), vec![(1.0, 4.0), (4.0, 10.0)]);

        // Last clip: nothing after it to give way, so the track's end moves with it.
        let (p, c) = (sclip(10.0, 14.0, 0.0), sclip(30.0, 33.0, 4.0));
        let ids = [p.id, c.id];
        let mut t = one_lane(vec![p, c]);
        assert_eq!(t.duration(), 7.0);
        let out = t.slide_clip(ids[1], 3.0, &footage(60.0)).unwrap();
        assert_eq!(out.applied, 3.0);
        assert_eq!(spans_of(&t, 0), vec![(0.0, 7.0), (7.0, 10.0)]);
        assert_eq!(t.duration(), 10.0);

        // Free space on both sides: a plain move, bounded by what it would run into.
        let (p, c, n) = (sclip(10.0, 11.0, 0.0), sclip(30.0, 33.0, 3.0), sclip(20.0, 21.0, 9.0));
        let ids = [p.id, c.id, n.id];
        let mut t = one_lane(vec![p, c, n]);
        let out = t.slide_clip(ids[1], 5.0, &footage(60.0)).unwrap();
        assert_eq!((out.applied, out.clips.len()), (3.0, 1));
        assert_eq!(spans_of(&t, 0), vec![(0.0, 1.0), (6.0, 9.0), (9.0, 10.0)]);
    }

    #[test]
    fn a_slide_honors_the_neighbours_speed_and_direction() {
        // p plays backwards (its end is its in-point), n at 2x.
        let mut p = sclip(10.0, 14.0, 0.0);
        p.speed = -1.0;
        let c = sclip(30.0, 33.0, 4.0);
        let mut n = sclip(20.0, 30.0, 7.0); // 10 source s at 2x: 5 s
        n.speed = 2.0;
        let ids = [p.id, c.id, n.id];
        let mut t = one_lane(vec![p, c, n]);

        t.slide_clip(ids[1], 2.0, &footage(60.0)).unwrap();
        let cl = &t.tracks[0].clips;
        assert_eq!(
            window(&cl[0]),
            (8.0, 14.0),
            "the reversed clip's end is its in-point: down by 2"
        );
        assert_eq!(cl[1].timeline_start, 6.0);
        assert_eq!(
            (cl[2].timeline_start, window(&cl[2])),
            (9.0, (24.0, 30.0)),
            "2 timeline seconds at 2x is 4 source"
        );
        assert_eq!(spans_of(&t, 0), vec![(0.0, 6.0), (6.0, 9.0), (9.0, 12.0)]);
    }

    #[test]
    fn a_slide_moves_the_next_clips_animation_with_its_footage_and_not_the_slid_clips() {
        let (mut t, [p, c, n]) = three();
        for (id, keys) in [
            (p, vec![kf(0.0, 1.0), kf(2.0, 3.0)]),
            (c, vec![kf(0.0, 1.0), kf(1.0, 3.0)]),
            (n, vec![kf(0.0, 1.0), kf(4.0, 4.0)]),
        ] {
            let (ti, ci) = t.locate(id).unwrap();
            t.tracks[ti].clips[ci].keyframes = keys;
        }
        t.slide_clip(c, 1.0, &footage(60.0)).unwrap();
        let cl = &t.tracks[0].clips;
        assert_eq!(scales(&cl[0]), vec![(0.0, 1.0), (2.0, 3.0)]);
        assert_eq!(
            scales(&cl[1]),
            vec![(0.0, 1.0), (1.0, 3.0)],
            "the slid clip is only repositioned"
        );
        assert_eq!(scales(&cl[2]), vec![(0.0, 1.75), (3.0, 4.0)], "n lost its first second");
    }

    #[test]
    fn a_slide_through_a_still_neighbour_extends_it_without_limit() {
        let still = Uuid::new_v4();
        let limits: SourceLimits = HashMap::from([(Uuid::nil(), 60.0), (still, f64::INFINITY)]);
        // The still is the previous clip: it can grow as far as the slide goes.
        let (p, c, n) = (
            Clip::new(still, 0.0, 4.0, 0.0),
            sclip(30.0, 33.0, 4.0),
            sclip(20.0, 25.0, 7.0),
        );
        let ids = [p.id, c.id, n.id];
        let mut t = one_lane(vec![p, c, n]);
        near(t.slide_clip(ids[1], 10.0, &limits).unwrap().applied, 4.95);
        assert_eq!(window(&t.tracks[0].clips[0]), (0.0, 8.95));

        // The still is the next clip: pulled earlier it lengthens, keeping a window that starts at 0.
        let (p, c, n) = (
            sclip(10.0, 14.0, 0.0),
            sclip(30.0, 33.0, 4.0),
            Clip::new(still, 0.0, 5.0, 7.0),
        );
        let ids = [p.id, c.id, n.id];
        let mut t = one_lane(vec![p, c, n]);
        near(t.slide_clip(ids[1], -2.0, &limits).unwrap().applied, -2.0);
        assert_eq!(window(&t.tracks[0].clips[2]), (0.0, 7.0));
        assert_eq!(t.tracks[0].clips[2].timeline_start, 5.0);
    }

    #[test]
    fn slide_errors() {
        let (t, [_, c, _]) = three();
        let mut copy = t.clone();
        assert!(refused(copy.slide_clip(c, 0.0, &footage(60.0))).contains("zero"));
        assert!(refused(copy.slide_clip(c, f64::NAN, &footage(60.0))).contains("finite"));
        assert!(matches!(
            copy.slide_clip(Uuid::new_v4(), 1.0, &footage(60.0)),
            Err(Error::ClipNotFound(_))
        ));
        assert!(matches!(
            copy.slide_clip(c, 1.0, &SourceLimits::new()),
            Err(Error::AssetNotFound(_))
        ));
        assert!(same_cut(&copy, &t), "none of them changed anything");
        copy.tracks[0].locked = true;
        assert!(refused(copy.slide_clip(c, 1.0, &footage(60.0))).contains("V1 is locked"));
    }

    #[test]
    fn split_remove_right_shortens_the_clip_at_its_end() {
        let mut c = sclip(10.0, 20.0, 5.0); // [5, 15)
        c.fade_in = 1.0;
        c.fade_out = 2.0;
        let (id, keys) = (c.id, vec![kf(0.0, 1.0), kf(8.0, 3.0)]);
        c.keyframes = keys.clone();
        let mut t = one_lane(vec![c]);

        let kept = t.split_remove(id, 9.0, SplitSide::Right).unwrap();
        assert_eq!(kept.id, id, "the surviving half keeps the clip's identity");
        assert_eq!(window(&kept), (10.0, 14.0));
        assert_eq!((kept.timeline_start, kept.timeline_end()), (5.0, 9.0));
        assert_eq!(kept.fade_in, 1.0);
        assert_eq!(kept.fade_out, 0.0, "the fade belonged to the end that was removed");
        assert_eq!(kept.keyframes, keys, "the head did not move, so the animation did not");
        assert_eq!(t.tracks[0].clips.len(), 1);
        assert_eq!(t.tracks[0].clips[0].timeline_end(), 9.0);
    }

    #[test]
    fn split_remove_left_moves_the_start_up_and_drops_what_belonged_to_the_old_start() {
        let mut c = sclip(10.0, 20.0, 5.0); // [5, 15)
        c.fade_in = 1.0;
        c.fade_out = 2.0;
        c.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let id = c.id;
        let mut t = one_lane(vec![c]);

        let kept = t.split_remove(id, 9.0, SplitSide::Left).unwrap();
        assert_eq!(kept.id, id);
        assert_eq!(window(&kept), (14.0, 20.0));
        assert_eq!(
            (kept.timeline_start, kept.timeline_end()),
            (9.0, 15.0),
            "the right edge stays put"
        );
        assert_eq!(kept.fade_in, 0.0);
        assert_eq!(kept.transition_in, None);
        assert_eq!(kept.fade_out, 2.0, "the end is still the clip's own");
    }

    #[test]
    fn split_remove_honors_speed_and_reverse() {
        // A reversed clip at 2x over 10..20: 5 s on the timeline, starting at its out-point.
        let mut c = sclip(10.0, 20.0, 0.0);
        c.speed = -2.0;
        let id = c.id;
        let t = one_lane(vec![c]);

        let mut left = t.clone();
        let kept = left.split_remove(id, 2.0, SplitSide::Left).unwrap();
        assert_eq!(
            window(&kept),
            (10.0, 16.0),
            "2 timeline seconds at 2x is 4 source off the out-point"
        );
        assert_eq!((kept.timeline_start, kept.duration()), (2.0, 3.0));

        let mut right = t;
        let kept = right.split_remove(id, 2.0, SplitSide::Right).unwrap();
        assert_eq!(window(&kept), (16.0, 20.0), "…and off the in-point for the end");
        assert_eq!((kept.timeline_start, kept.duration()), (0.0, 2.0));
    }

    #[test]
    fn split_remove_left_carries_the_animation_with_the_footage_that_stays() {
        let mut c = sclip(0.0, 8.0, 0.0);
        c.keyframes = vec![kf(0.0, 1.0), kf(4.0, 5.0)];
        let id = c.id;
        let mut t = one_lane(vec![c]);
        let kept = t.split_remove(id, 1.0, SplitSide::Left).unwrap();
        assert_eq!(
            scales(&kept),
            vec![(0.0, 2.0), (3.0, 5.0)],
            "the pose at the cut is pinned, the rest shifts"
        );
    }

    #[test]
    fn split_remove_holds_the_fade_inside_what_is_left() {
        let mut c = sclip(0.0, 4.0, 0.0);
        c.fade_in = 3.0;
        let id = c.id;
        let mut t = one_lane(vec![c]);
        let kept = t.split_remove(id, 1.0, SplitSide::Right).unwrap();
        assert_eq!(kept.fade_in, 1.0, "a 3 s fade cannot outlast a 1 s clip");
    }

    #[test]
    fn split_remove_only_cuts_inside_the_clip_and_leaves_something() {
        let c = sclip(0.0, 4.0, 2.0); // [2, 6)
        let id = c.id;
        let t = one_lane(vec![c]);
        let run = |at: f64, side: SplitSide| {
            let mut copy = t.clone();
            let why = refused(copy.split_remove(id, at, side));
            assert!(same_cut(&copy, &t), "a refusal changes nothing");
            why
        };
        assert!(run(1.0, SplitSide::Left).contains("not inside the clip (0:02.0–0:06.0)"));
        assert!(run(7.0, SplitSide::Right).contains("not inside the clip"));
        assert!(
            run(2.0, SplitSide::Left).contains("not inside the clip"),
            "the clip's own start is not a cut"
        );
        assert!(run(6.0, SplitSide::Right).contains("not inside the clip"));
        assert!(run(f64::NAN, SplitSide::Left).contains("finite"));
        assert!(run(5.99, SplitSide::Left).contains("only 0.01s of the clip"));
        assert!(run(2.02, SplitSide::Right).contains("only 0.02s of the clip"));

        // The part that goes can be as small as it likes; only what stays has a floor.
        let mut copy = t.clone();
        near(copy.split_remove(id, 2.02, SplitSide::Left).unwrap().duration(), 3.98);

        let mut locked = t.clone();
        locked.tracks[0].locked = true;
        assert!(refused(locked.split_remove(id, 4.0, SplitSide::Left)).contains("V1 is locked"));
        let mut copy = t;
        assert!(matches!(
            copy.split_remove(Uuid::new_v4(), 4.0, SplitSide::Left),
            Err(Error::ClipNotFound(_))
        ));
    }

    #[test]
    fn a_split_remove_under_ripple_closes_the_gap_through_the_standard_path() {
        // `a [0,4)  b [5,8)  c [10,12)`, ripple applied the way `edit_timeline` does.
        let (before, [_, b, _]) = gapped();
        let (_, right) = rippled(&before, |t| {
            t.split_remove(b, 6.0, SplitSide::Right).unwrap();
        });
        assert_eq!(
            spans_of(&right, 0),
            vec![(0.0, 4.0), (5.0, 6.0), (8.0, 10.0)],
            "c followed the 2 s that went"
        );

        let (bare, left) = rippled(&before, |t| {
            t.split_remove(b, 6.0, SplitSide::Left).unwrap();
        });
        assert_eq!(
            spans_of(&bare, 0),
            vec![(0.0, 4.0), (6.0, 8.0), (10.0, 12.0)],
            "without ripple the gap stays"
        );
        assert_eq!(
            spans_of(&left, 0),
            vec![(0.0, 4.0), (5.0, 7.0), (9.0, 11.0)],
            "with it b holds its start and the rest of the track closes in behind"
        );
    }

    #[test]
    fn a_range_slice_pins_the_pose_at_a_cut_front_the_way_a_head_move_does() {
        // `Timeline::slice` shares `rebase_animation` with the edit modes: a clip cut
        // at the front keeps its animated pose as a key at 0, later keys shift back,
        // and the fade and transition that belonged to the lost head go.
        let mut c = sclip(0.0, 8.0, 0.0);
        c.keyframes = vec![kf(0.0, 1.0), kf(4.0, 5.0)];
        c.fade_in = 1.0;
        let t = one_lane(vec![c]);
        let s = t.slice(1.0, 8.0);
        let cut = &s.tracks[0].clips[0];
        assert_eq!(scales(cut), vec![(0.0, 2.0), (3.0, 5.0)]);
        assert_eq!((cut.timeline_start, cut.source_in, cut.fade_in), (0.0, 1.0, 0.0));
    }

    /// A tiny deterministic generator, so the fuzz below is the same on every run.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
        }
        fn between(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * self.next()
        }
    }

    /// A lane of `n` clips of random length, speed (and direction) and window in a
    /// 600 s asset — the float values a real cut is made of, where `start + (out - in)
    /// / speed` is not exact — laid end to end, with an occasional real gap.
    fn random_lane(rng: &mut Lcg, n: usize) -> Timeline {
        let mut clips = Vec::new();
        let mut cursor = 0.0;
        for _ in 0..n {
            let mut c = sclip(0.0, 0.0, 0.0);
            c.speed = [1.0, 1.0, 2.0, 0.5, 1.5, -1.0, -2.0, 0.37][(rng.next() * 8.0) as usize % 8];
            c.source_in = rng.between(30.0, 400.0);
            c.source_out = c.source_in + rng.between(0.4, 7.0) * c.speed_mag();
            if rng.next() < 0.25 {
                cursor += rng.between(0.2, 2.0);
            }
            c.timeline_start = cursor;
            cursor = c.timeline_end();
            clips.push(c);
        }
        one_lane(clips)
    }

    /// Every junction where a clip starts *before* the one ahead of it ends, by float
    /// noise — what a strict overlap test (`Project::move_clip`'s) reads as one clip
    /// lying on the next — as `(index of the later clip, how far)`.
    fn noise_overlaps(t: &Timeline) -> Vec<(usize, f64)> {
        let clips = &t.tracks[0].clips;
        (1..clips.len())
            .filter_map(|i| {
                let over = clips[i - 1].timeline_end() - clips[i].timeline_start;
                (over > 0.0 && over < ADJACENT_EPS).then_some((i, over))
            })
            .collect()
    }

    #[test]
    fn roll_and_slide_leave_no_float_residue_between_clips() {
        // A roll or slide computes one side of a cut from a window and the other from
        // `start + delta`; left alone they disagree by a few ulps (±6e-14 s) in a large
        // share of cases, which a strict overlap test reads as clips lying on each other.
        // Every junction of the lane — the cut itself and the edge the edit runs into —
        // has to come out clean, for requests both inside the range and far past it.
        let mut rng = Lcg(7);
        let mut tried = 0;
        for _ in 0..3000 {
            let t = random_lane(&mut rng, 5);
            assert!(noise_overlaps(&t).is_empty());
            let ids: Vec<Uuid> = t.tracks[0].clips.iter().map(|c| c.id).collect();
            let at = 1 + (rng.next() * 3.0) as usize;
            let delta = if rng.next() < 0.3 {
                rng.between(-9.0, 9.0)
            } else {
                rng.between(-1.5, 1.5)
            };
            for op in 0..2 {
                let mut u = t.clone();
                let out = if op == 0 {
                    u.roll_edit(ids[at - 1], ids[at], delta, &footage(600.0))
                } else {
                    u.slide_clip(ids[at], delta, &footage(600.0))
                };
                let Ok(out) = out else { continue };
                tried += 1;
                assert!(noise_overlaps(&u).is_empty(), "op {op} by {delta}: {:?}", noise_overlaps(&u));
                // …and nothing but noise moved to get there.
                let (was, now) = (&t.tracks[0].clips, &u.tracks[0].clips);
                for (w, n) in was.iter().zip(now) {
                    let edited = out.clips.iter().any(|c| c.id == w.id);
                    if !edited {
                        assert_eq!(
                            (w.timeline_start, w.source_in, w.source_out),
                            (n.timeline_start, n.source_in, n.source_out)
                        );
                    }
                }
                if op == 0 {
                    near(now[at].timeline_start, was[at].timeline_start + out.applied);
                    near(now[at].timeline_end(), was[at].timeline_end());
                    near(now[at - 1].timeline_end(), was[at - 1].timeline_end() + out.applied);
                } else {
                    near(now[at].timeline_start, was[at].timeline_start + out.applied);
                    near(now[at].duration(), was[at].duration());
                }
            }
        }
        assert!(tried > 4000, "the fuzz should mostly be legal edits, got {tried}");
    }

    #[test]
    fn a_cut_a_hair_apart_keeps_its_real_gap_and_only_float_noise_is_welded() {
        // 0.5 ms is data (within the engine's tolerance for a touching cut), not noise.
        let (mut t, a, b) = cut_pair();
        t.tracks[0].clips[1].timeline_start = 4.0005;
        t.roll_edit(a, b, 1.0, &footage(60.0)).unwrap();
        assert_eq!(t.tracks[0].clips[1].timeline_start, 5.0005);

        // Exactly closed: starting where the leader's computed end is, bit for bit.
        let (mut t, a, b) = cut_pair();
        t.roll_edit(a, b, 0.1, &footage(60.0)).unwrap();
        assert_eq!(t.tracks[0].clips[1].timeline_start, t.tracks[0].clips[0].timeline_end());
    }

    #[test]
    fn a_slip_diff_has_the_precision_of_a_frame_and_the_sign_the_op_documents() {
        let slip = |mut c: Clip, delta: f64| {
            let id = c.id;
            c.timeline_start = 3.0;
            let before = one_lane(vec![c]);
            let mut after = before.clone();
            after.slip_clip(id, delta, &footage(60.0)).unwrap();
            before.diff(&after).entries[0].summary.clone()
        };
        // One frame at 30 fps is 0.033 s: one decimal printed it as `footage -0.0s
        // (in-point 10.0s → 10.0s)`, which says nothing happened.
        let frame = 1.0 / 30.0;
        let said = slip(sclip(10.0, 14.0, 0.0), frame);
        assert!(said.contains("footage +0.03s (in-point 10.00s → 10.03s)"), "{said}");
        let said = slip(sclip(10.0, 14.0, 0.0), -frame);
        assert!(said.contains("footage -0.03s (in-point 10.00s → 9.97s)"), "{said}");
        // Smaller than two decimals can show: three, rather than a zero.
        let said = slip(sclip(10.0, 14.0, 0.0), 0.001);
        assert!(said.contains("footage +0.001s"), "{said}");

        // A reversed clip's positive slip is the window moving *down*, and is still
        // the `+` the op documents (later in its own footage).
        let mut reversed = sclip(10.0, 14.0, 0.0);
        reversed.speed = -1.0;
        let said = slip(reversed.clone(), 3.0);
        assert!(said.contains("footage +3.00s (in-point 10.00s → 7.00s)"), "{said}");
        let said = slip(reversed, -3.0);
        assert!(said.contains("footage -3.00s (in-point 10.00s → 13.00s)"), "{said}");
    }

    #[test]
    fn times_and_deltas_round_half_to_even_on_the_exact_value() {
        // The TS mirror (`formatTime`) has to agree with this to the digit: Rust rounds
        // an exact binary tie to the even digit and anything else by its exact value.
        assert_eq!(fmt_time(4.25), "0:04.2");
        assert_eq!(fmt_time(4.75), "0:04.8");
        assert_eq!(fmt_time(0.25), "0:00.2");
        assert_eq!(fmt_time(72.25), "1:12.2");
        assert_eq!(fmt_time(0.35), "0:00.3", "0.35 is a hair under a tie");
        assert_eq!(fmt_time(0.45), "0:00.5", "0.45 is a hair over one");
        assert_eq!(fmt_delta(-0.25), "-0.2s");
        assert_eq!(fmt_delta(0.75), "+0.8s");
        assert_eq!(format!("{:.2}", 0.125), "0.12");
        assert_eq!(format!("{:.2}", 0.375), "0.38");
    }

    /// V1 `v [0,6)` over A1 `a [1,7)` — a picture and its sound, not quite in step.
    fn picture_and_sound() -> (Timeline, Uuid, Uuid) {
        let (v, a) = (sclip(10.0, 16.0, 0.0), sclip(10.0, 16.0, 1.0));
        let ids = (v.id, a.id);
        let t = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![v]),
                track(StreamKind::Audio, "A1", vec![a]),
            ],
            ..Timeline::new()
        };
        (t, ids.0, ids.1)
    }

    #[test]
    fn a_group_split_remove_cuts_every_track_in_one_edit() {
        let (mut t, v, a) = picture_and_sound();
        let kept = t
            .split_remove_clips(
                &[ClipCut { clip_id: v, at: 3.0 }, ClipCut { clip_id: a, at: 3.0 }],
                SplitSide::Left,
            )
            .unwrap();
        assert_eq!(kept.iter().map(|c| c.id).collect::<Vec<_>>(), vec![v, a], "in request order");
        assert_eq!((kept[0].timeline_start, kept[0].source_in), (3.0, 13.0));
        assert_eq!((kept[1].timeline_start, kept[1].source_in), (3.0, 12.0));
        assert_eq!(spans_of(&t, 0), vec![(3.0, 6.0)]);
        assert_eq!(spans_of(&t, 1), vec![(3.0, 7.0)]);

        // Each track is cut at its own time, and the other side works the same way.
        let (mut t, v, a) = picture_and_sound();
        t.split_remove_clips(
            &[ClipCut { clip_id: v, at: 2.0 }, ClipCut { clip_id: a, at: 4.5 }],
            SplitSide::Right,
        )
        .unwrap();
        assert_eq!(spans_of(&t, 0), vec![(0.0, 2.0)]);
        assert_eq!(spans_of(&t, 1), vec![(1.0, 4.5)]);
    }

    #[test]
    fn a_group_split_remove_is_all_or_nothing() {
        let (t, v, a) = picture_and_sound();
        let cut = |clip_id, at| ClipCut { clip_id, at };
        let run = |name: &str, cuts: &[ClipCut]| {
            let mut copy = t.clone();
            let why = refused(copy.split_remove_clips(cuts, SplitSide::Left));
            assert!(same_cut(&copy, &t), "{name}: a refused group changes nothing");
            why
        };
        // The second cut is bad, the first would have been fine.
        assert!(run("outside", &[cut(v, 3.0), cut(a, 0.5)]).contains("not inside the clip"));
        assert!(run("floor", &[cut(v, 3.0), cut(a, 6.99)]).contains("only 0.01s"));
        assert!(run("empty", &[]).contains("no clips"));
        assert!(run("twice", &[cut(v, 3.0), cut(v, 4.0)]).contains("more than once"));

        // One clip per track.
        let mut crowded = t.clone();
        let extra = sclip(0.0, 2.0, 7.0);
        let extra_id = extra.id;
        crowded.tracks[0].clips.push(extra);
        let mut copy = crowded.clone();
        let why = refused(copy.split_remove_clips(&[cut(v, 3.0), cut(extra_id, 7.5)], SplitSide::Right));
        assert!(why.contains("one clip per track") && why.contains("V1"), "{why}");
        assert!(same_cut(&copy, &crowded));

        let mut locked = t.clone();
        locked.tracks[1].locked = true;
        let mut copy = locked.clone();
        assert!(refused(copy.split_remove_clips(&[cut(v, 3.0), cut(a, 3.0)], SplitSide::Left)).contains("A1 is locked"));
        assert!(same_cut(&copy, &locked), "the unlocked track was not cut either");

        let mut copy = t.clone();
        assert!(matches!(
            copy.split_remove_clips(&[cut(v, 3.0), cut(Uuid::new_v4(), 3.0)], SplitSide::Left),
            Err(Error::ClipNotFound(_))
        ));
    }

    #[test]
    fn a_group_split_remove_ripples_each_track_on_its_own() {
        // Behind each clip a follower: V1 `v [0,6) w [8,10)`, A1 `a [1,7) x [9,11)`.
        let (mut before, v, a) = picture_and_sound();
        before.tracks[0].clips.push(sclip(0.0, 2.0, 8.0));
        before.tracks[1].clips.push(sclip(0.0, 2.0, 9.0));
        let (after, ripple) = rippled(&before, |t| {
            t.split_remove_clips(
                &[ClipCut { clip_id: v, at: 2.0 }, ClipCut { clip_id: a, at: 5.0 }],
                SplitSide::Left,
            )
            .unwrap();
        });
        // Without ripple the gaps stay; with it each track closes by what *it* lost
        // (V1 2 s, A1 4 s) and the cut clips hold their own starts.
        assert_eq!(spans_of(&after, 0), vec![(2.0, 6.0), (8.0, 10.0)]);
        assert_eq!(spans_of(&ripple, 0), vec![(0.0, 4.0), (6.0, 8.0)]);
        assert_eq!(spans_of(&after, 1), vec![(5.0, 7.0), (9.0, 11.0)]);
        assert_eq!(spans_of(&ripple, 1), vec![(1.0, 3.0), (5.0, 7.0)]);
    }

    #[test]
    fn a_slip_reads_as_a_slip_in_a_diff_not_a_zero_second_trim() {
        let (t, a, _) = cut_pair();
        let mut slipped = t.clone();
        slipped.slip_clip(a, 2.0, &footage(60.0)).unwrap();
        let diff = t.diff(&slipped);
        assert_eq!(diff.entries.len(), 1);
        assert_eq!(diff.entries[0].kind, DiffKind::ClipRetrimmed);
        assert!(
            diff.entries[0].summary.contains("Slipped clip on V1 at 0:00.0"),
            "{}",
            diff.entries[0].summary
        );
        assert!(diff.entries[0].summary.contains("+2.00s"), "{}", diff.entries[0].summary);
        assert!(
            diff.entries[0].summary.contains("in-point 10.00s → 12.00s"),
            "{}",
            diff.entries[0].summary
        );

        let mut rolled = t.clone();
        rolled.roll_edit(a, t.tracks[0].clips[1].id, 1.0, &footage(60.0)).unwrap();
        assert!(t.diff(&rolled).entries.iter().any(|e| e.summary.starts_with("Trimmed clip")));
    }
}
