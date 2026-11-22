//! A media file as the project knows it: its streams, pixel formats and the asset record.

use super::*;

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
    /// The sample range the stream declares, as ffprobe names it: `"pc"` (full range) or `"tv"`
    /// (limited); `None` when it declares none, **and for an asset probed before the tag was
    /// recorded** (schema v10), which a renderer must read as "not known" and not as limited. The
    /// tag is the truth about the range: ffprobe's `yuvj…` names only the 8-bit formats FFmpeg
    /// itself calls full range, while a VP9 (or HEVC, AV1) file flagged `pc` probes as plain
    /// `yuv420p` and FFmpeg runs its raw samples through a graph. Stored as ffprobe says it, so an
    /// RGB or gray picture reads `pc` too (nothing limited to tell it from); only a YCbCr format's
    /// word is a flag. Defaulted (and omitted when unset) so older `.kerf` JSON still deserializes;
    /// this rides in the `streams` JSON column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_range: Option<String>,
    /// Seconds between the start of the file's picture and sound and this video stream's
    /// first frame, when the video starts after the sound: the stream's `start_time` less the
    /// earliest video or audio stream's (not the container's, which a text track can pull
    /// earlier), above a millisecond and below the duration (a start at or past the end is a
    /// broken one and reads 0). The export seeks and places the picture by it, so a clip cut
    /// from inside the lead keeps the source's sync (see `clip_timing::picture_delay`).
    /// Defaulted (and omitted at 0) so older `.kerf` JSON still deserializes, as an
    /// ordinary start.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub lead: f64,
    /// The video stream's time base, `(numerator, denominator)` seconds a tick, as the file declares it (ffprobe's
    /// `time_base`: a millisecond in Matroska, 1 / 600 in much of QuickTime, the frame in AVI, 1 / 90000 in transport
    /// streams, a fraction of a frame in most MP4). The export's `setpts` truncates a clip's start to a whole tick of it, so
    /// where a clip's first frame can land depends on it (`ClipFx::time_base`); clips of a stream whose tick is not known are
    /// not folded together (`blend::fold_runs`). `None` for an asset stored before it was recorded, for a still, and for
    /// sound. An export that could fold a run of such an asset's blended clips asks the file and the project keeps the answer
    /// (`engine::time_base_inputs`, `Project::apply_time_bases`); importing the file again does not probe it, and a probe
    /// that fails leaves `None`. Defaulted (and omitted when unknown) so older `.kerf` JSON still deserializes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_base: Option<(u32, u32)>,
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

/// Whether `name` is a YCbCr or gray format deeper than 8 bits per component (`yuv420p10le`,
/// `yuv444p12`, `p010le`, `gray10le`, ...). RGB formats have no range to be unsure of and are
/// not counted.
pub fn pix_fmt_is_high_bit(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let t = n.strip_suffix("le").or_else(|| n.strip_suffix("be")).unwrap_or(&n);
    if matches!(
        t,
        "p010" | "p012" | "p016" | "p210" | "p212" | "p216" | "p410" | "p412" | "p416"
    ) {
        return true;
    }
    let digits = t.chars().rev().take_while(char::is_ascii_digit).count();
    let (stem, tail) = t.split_at(t.len() - digits);
    matches!(tail.parse::<u32>(), Ok(9 | 10 | 12 | 14 | 16))
        && (stem == "gray" || (stem.starts_with("yuv") && stem.ends_with('p')))
}

/// Whether a colour-matrix tag **states** one: `unknown`, `unspecified` and `reserved` (a value no
/// matrix has, which FFmpeg reads as it reads none) do not, and neither does no tag at all. Every
/// other name does, `gbr` (RGB, "identity") included: a file that says anything about its matrix is
/// not untagged. Whether a stream's tag is the *file's* word to a graph is
/// [`StreamInfo::names_a_matrix`]'s question.
pub fn matrix_tag_is_stated(tag: Option<&str>) -> bool {
    tag.is_some_and(|t| !matches!(t.trim(), "" | "unknown" | "unspecified" | "reserved"))
}

/// The codecs whose decoders write `bt470bg` on everything they decode, whatever the file says. A tag
/// from one of them is the decoder's default, not the file's word. `mjpeg`, `amv`, `speedhq` and
/// `jpegls` (`rgb24`, `gray` and `gray16le` alike) were probed on FFmpeg 6.1.1 and 9.0.2 with ~100
/// encoder / pixel-format pairs, and no other encodable codec did. `mjpegb`, `sp5x`, `thp` and
/// `smvjpeg` are decode-only, so there is no file to probe: they share MJPEG's `ff_mjpeg_decode_init`,
/// which sets the tag. `mxpeg` (decode-only too) is listed on the same assumption, which costs nothing
/// if it is wrong: a codec that writes no tag has nothing to exclude.
pub(crate) const DEFAULT_TAG_CODECS: [&str; 9] = [
    "mjpeg", "amv", "speedhq", "jpegls", "mjpegb", "sp5x", "thp", "smvjpeg", "mxpeg",
];

/// Whether a picture is **untagged HD YCbCr**: HD (mpv's rule, `width >= 1280 || height > 576`, on
/// the source's displayed size), YCbCr on the pixel-format allow-list (alpha, grey and RGB are not),
/// not `yuvj…` (JFIF and a webcam's MJPEG are BT.601 by definition), naming no matrix and not HDR
/// (which is tone-mapped to a BT.709 picture). What every player shows as BT.709 and FFmpeg reads as
/// BT.601 — see [`bt709_labels`].
pub fn untagged_hd_yuv(color_space: Option<&str>, pix_fmt: &str, width: u32, height: u32, hdr: bool) -> bool {
    if hdr || matrix_tag_is_stated(color_space) || pix_fmt.to_ascii_lowercase().starts_with("yuvj") {
        return false;
    }
    matches!(pix_fmt_layout(pix_fmt), Some(PixLayout::Yuv420 | PixLayout::OtherYuv)) && (width >= 1280 || height > 576)
}

/// Which inputs of one filter graph the export labels **BT.709** (`setparams=colorspace=bt709`),
/// given each input's first video stream (`None` for an input with no picture).
///
/// FFmpeg 9 negotiates the colour matrix along the graph and converts every layer that is tagged
/// otherwise into the bottom layer's, and it reads a picture that names no matrix as BT.601. An
/// untagged HD clip beside a BT.709 one is therefore *converted* (28 dB off), though every player
/// shows it as BT.709 like its neighbour. So an untagged HD input is labelled BT.709 **when another
/// input in the graph names a matrix** (a tag, or HDR, which the graph tone-maps to a BT.709
/// picture), and only then: a graph with no such input, a single clip, and everything that is not
/// untagged HD YCbCr (an SD picture, MJPEG, RGB, grey, alpha, a still image) is built as it always
/// was. The label moves no pixel by itself; it states what the picture is.
///
/// The decision is the **graph's**, not a frame's: the export, the scrub window and the playback
/// stream decide over the clips of their own cut, a still over the layers on screen. The preview
/// decodes proxies but is decided from the **original's** stream, so a proxy smaller than HD (a
/// small proxy size in Settings) is labelled as its original is.
pub fn bt709_labels(inputs: &[Option<&StreamInfo>]) -> Vec<bool> {
    let named = inputs.iter().flatten().any(|s| s.names_a_matrix());
    inputs
        .iter()
        .map(|s| named && s.is_some_and(|s| s.is_untagged_hd_yuv()))
        .collect()
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

    /// Whether this video stream **names a YCbCr matrix** to a graph that holds it: HDR (tone-mapped to
    /// a tagged BT.709 picture) or a tag that is the *file's* word ([`matrix_tag_is_stated`]).
    ///
    /// The principle: **a tag a decoder writes by default is not the file's word.** Probed on both
    /// FFmpegs, those are `gbr` on any PNG (`rgb24`, `rgba`, `gray`) and, depending on the encoder that
    /// made it, on RGB video; `bt470bg` on a JPEG and a WebP; and `bt470bg` from the decoders of
    /// [`DEFAULT_TAG_CODECS`] (MJPEG video and its relatives, an AMV player's clips, NDI's SpeedHQ,
    /// JPEG-LS), whatever their pixel format. An explicit tag on any other codec is the file's,
    /// `yuvj…` full-range H.264 included (FFmpeg's h264 decoder names every full-range stream
    /// `yuvj420p`, and a camera's `pc` + `bt709` is a matrix).
    ///
    /// This is *not* the mirror of [`StreamInfo::is_untagged_hd_yuv`]'s exclusions: grey, alpha and SD
    /// pictures are never labelled but do name a matrix when tagged, and that asymmetry is intended
    /// (a tagged SD clip is a stated matrix; it just is not HD). What the two share is only that a
    /// still image, RGB and a decoder's default are nobody's word.
    pub fn names_a_matrix(&self) -> bool {
        self.kind == StreamKind::Video && (self.hdr().is_some() || (self.tag_is_the_files() && self.has_a_yuv_tag()))
    }

    /// The tag is a matrix a YCbCr picture states: not `gbr` (RGB, "identity"), and not on an RGB
    /// picture, which has no YCbCr matrix to name whoever tagged it.
    pub(crate) fn has_a_yuv_tag(&self) -> bool {
        matrix_tag_is_stated(self.color_space.as_deref())
            && self.color_space.as_deref().map(str::trim) != Some("gbr")
            && self.pix_fmt.as_deref().and_then(pix_fmt_layout) != Some(PixLayout::Rgb)
    }

    /// Whether the tag can be the file's own: not a still image's and not written by one of the
    /// decoders that tag everything they decode ([`DEFAULT_TAG_CODECS`]).
    fn tag_is_the_files(&self) -> bool {
        !self.image && !DEFAULT_TAG_CODECS.iter().any(|c| self.codec.eq_ignore_ascii_case(c))
    }

    /// Whether this video stream is **untagged HD YCbCr** ([`untagged_hd_yuv`]) and a moving
    /// picture: a still image is not (a lossy WebP is BT.601 by its format, not by a convention).
    pub fn is_untagged_hd_yuv(&self) -> bool {
        self.kind == StreamKind::Video
            && !self.image
            && untagged_hd_yuv(
                self.color_space.as_deref(),
                self.pix_fmt.as_deref().unwrap_or(""),
                self.width.unwrap_or(0),
                self.height.unwrap_or(0),
                self.hdr().is_some(),
            )
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
    /// The score this audio was composed from, when Kerf rendered it (`compose_music`) rather
    /// than the user importing it. `None` for ordinary media. Editing the score and composing
    /// again replaces the clip's audio; the asset's bar and tempo analysis is written from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub music_score: Option<MusicScore>,
    /// The spec this audio was synthesized from, when Kerf made it (`add_sound_effects`) rather
    /// than the user importing it. `None` for ordinary media. Reshaping the effect renders a new
    /// one from an edited spec and swaps the clip's audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound_effect: Option<SfxSpec>,
    /// The labels on the asset (`location: Cumbuco`, `jump`), for finding it and for building a cut by
    /// a key. Set with [`crate::Project::edit_asset_tags`], never by probing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
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
    /// tone-mapped to SDR wherever it is decoded. Of the **first** video stream, the one every
    /// graph input (`[i:v]`) and the BT.709 label ([`bt709_labels`]) read: a second video stream
    /// (cover art, a thumbnail) neither tone-maps the first nor names a matrix for it.
    pub fn hdr(&self) -> Option<Hdr> {
        self.streams
            .iter()
            .find(|s| s.kind == StreamKind::Video)
            .and_then(StreamInfo::hdr)
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
