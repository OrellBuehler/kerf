//! What an export is asked for: its options, and the checks and tables they are held to.

use super::*;

// ---- export ----------------------------------------------------------------

/// Output container / muxer. Authoritative over the output path extension; it
/// gates the codec allow-lists, faststart, the gif palette pipeline and whether
/// a video / audio stream is produced at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    #[default]
    Mp4,
    Mov,
    Mkv,
    Webm,
    Gif,
    Mp3,
    M4a,
    Wav,
    Flac,
}

impl Container {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
            Self::Gif => "gif",
            Self::Mp3 => "mp3",
            Self::M4a => "m4a",
            Self::Wav => "wav",
            Self::Flac => "flac",
        }
    }
    /// mp4 / mov / m4a benefit from a front-loaded moov atom; nothing else does.
    pub fn supports_faststart(self) -> bool {
        matches!(self, Self::Mp4 | Self::Mov | Self::M4a)
    }
    /// Audio-only containers never carry a video stream.
    pub fn is_audio_only(self) -> bool {
        matches!(self, Self::Mp3 | Self::M4a | Self::Wav | Self::Flac)
    }
    /// Gif is the only video-only container (no audio stream).
    pub fn is_video_only(self) -> bool {
        matches!(self, Self::Gif)
    }
    pub fn video_codecs(self) -> &'static [&'static str] {
        // Hardware encoders sit alongside the software ones — they emit the same
        // h264 / hevc / av1 bitstreams, so a container accepts a codec's HW
        // variants wherever it accepts the software one.
        match self {
            Self::Mp4 => &[
                "libx264",
                "libx265",
                "libsvtav1",
                "h264_nvenc",
                "hevc_nvenc",
                "av1_nvenc",
                "h264_qsv",
                "hevc_qsv",
                "av1_qsv",
                "h264_videotoolbox",
                "hevc_videotoolbox",
                "h264_amf",
                "hevc_amf",
                "av1_amf",
                "h264_vaapi",
                "hevc_vaapi",
                "av1_vaapi",
            ],
            Self::Mov => &[
                "prores_ks",
                "libx264",
                "libx265",
                "h264_nvenc",
                "hevc_nvenc",
                "h264_qsv",
                "hevc_qsv",
                "h264_videotoolbox",
                "hevc_videotoolbox",
                "h264_amf",
                "hevc_amf",
                "h264_vaapi",
                "hevc_vaapi",
            ],
            Self::Mkv => &[
                "libx264",
                "libx265",
                "libvpx-vp9",
                "libsvtav1",
                "h264_nvenc",
                "hevc_nvenc",
                "av1_nvenc",
                "h264_qsv",
                "hevc_qsv",
                "av1_qsv",
                "h264_videotoolbox",
                "hevc_videotoolbox",
                "h264_amf",
                "hevc_amf",
                "av1_amf",
                "h264_vaapi",
                "hevc_vaapi",
                "av1_vaapi",
            ],
            Self::Webm => &["libvpx-vp9", "libsvtav1", "av1_nvenc", "av1_qsv", "av1_amf", "av1_vaapi"],
            Self::Gif => &["gif"],
            _ => &[],
        }
    }
    pub fn audio_codecs(self) -> &'static [&'static str] {
        match self {
            Self::Mp4 => &["aac", "alac"],
            Self::Mov => &["aac", "alac", "pcm_s16le", "pcm_s24le"],
            Self::Mkv => &["aac", "libopus", "libmp3lame", "flac", "pcm_s16le"],
            Self::Webm => &["libopus"],
            Self::Mp3 => &["libmp3lame"],
            Self::M4a => &["aac", "alac"],
            Self::Wav => &["pcm_s16le", "pcm_s24le"],
            Self::Flac => &["flac"],
            Self::Gif => &[],
        }
    }
    pub fn video_ok(self, codec: &str) -> bool {
        self.video_codecs().contains(&codec)
    }
    pub fn audio_ok(self, codec: &str) -> bool {
        self.audio_codecs().contains(&codec)
    }
}

/// Which video rate-control branch [`build_export_args`] emits. Ignored for
/// `prores_ks` (driven by the ProRes profile) and `gif` (palette pipeline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RateControl {
    /// Constant quality: `-crf N` (libvpx-vp9 also gets `-b:v 0`). The default.
    #[default]
    Crf,
    /// Single-pass average bitrate: `-b:v X` (+ optional `-maxrate`/`-bufsize`).
    Bitrate,
    /// Two-pass average bitrate (two ffmpeg runs sharing a passlog).
    TwoPass,
    /// Per-codec lossless: x264/x265/svt-av1 `-crf 0`; libvpx-vp9 `-lossless 1`.
    Lossless,
}

/// Which ffmpeg invocation [`build_export_args`] is emitting for. Injected so
/// the builder stays pure (no knowledge of the platform null device or the temp
/// passlog file — [`render_with`] supplies those).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PassPhase {
    /// One-shot encode (every mode except two-pass).
    #[default]
    Single,
    /// Two-pass analysis pass: `-pass 1`, video-only, discarded output.
    First,
    /// Two-pass final pass: `-pass 2`, real output.
    Second,
}

/// Everything the export menu can drive. `Default` reproduces the original
/// hard-coded behaviour byte-for-byte (no `-c:v`/`-c:a`/`-crf`, no faststart),
/// so the legacy [`render`] path and the existing unit tests are unaffected.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", default)]
pub struct ExportOptions {
    /// Target container / muxer.
    pub container: Container,

    /// `-c:v` value. `None` lets ffmpeg pick the encoder from the container
    /// (legacy behaviour); audio-only containers ignore it.
    pub video_codec: Option<String>,
    /// `-c:a` value. `None` lets ffmpeg pick from the container.
    pub audio_codec: Option<String>,

    /// Video rate-control mode.
    pub rate_control: RateControl,
    /// `-crf N` (Crf / Lossless modes). `None` keeps the encoder default.
    pub crf: Option<u32>,
    /// `-b:v` token, e.g. "8M" / "2500k". Required for Bitrate / TwoPass.
    pub video_bitrate: Option<String>,
    /// `-maxrate` VBV cap (bitrate modes).
    pub max_rate: Option<String>,
    /// `-bufsize` VBV buffer (pairs with `max_rate`).
    pub buf_size: Option<String>,

    /// `-preset` for x264/x265 (named) and svt-av1 (numeric); reinterpreted as
    /// `-cpu-used` for libvpx-vp9.
    pub preset: Option<String>,
    /// ProRes quality `-profile:v 0..5` (prores_ks only).
    pub prores_profile: Option<u8>,
    /// `-tune` (x264 / x265 only).
    pub tune: Option<String>,
    /// `-profile:v` for h264 / hevc (not ProRes).
    pub profile_v: Option<String>,

    /// `-pix_fmt` AND the filtergraph terminal `format=` (dual-write). `None`
    /// keeps the yuv420p path. yuv420p requires even dimensions.
    pub pix_fmt: Option<String>,

    /// `-hwaccel` for input **decode** on export ("auto", "cuda", "vaapi",
    /// "videotoolbox", "qsv", "d3d11va", …). `None` / "none" decodes in software
    /// (the default, byte-for-byte as before). Independent of the encoder: GPU
    /// decode composes with a software encode. Frames are downloaded to system
    /// memory (no `-hwaccel_output_format`) so the CPU filtergraph still runs.
    pub hwaccel: Option<String>,

    /// Output WxH, baked into the filtergraph. Even-clamped.
    pub resolution: Option<(u32, u32)>,
    /// How footage of a different shape is fitted to that frame — letterboxed
    /// (the default), filled and cropped, or letterboxed over a blurred copy of
    /// itself. Only matters when the two differ.
    pub fit: Fit,
    /// Output fps, baked into the filtergraph; never emits `-r`. `None` is the project's
    /// delivery frame rate (`Delivery::fps`) when it has one, else the first clip with a
    /// picture's.
    pub fps: Option<f64>,
    /// `scale=…:flags=` scaler (bicubic / bilinear / lanczos / neighbor / spline).
    pub scaler: Option<String>,
    /// Forced audio sample rate, via the graph `aformat` (not `-ar`).
    pub audio_sample_rate: Option<u32>,
    /// Forced channel count, via the graph `aformat` (not `-ac`).
    pub audio_channels: Option<u16>,

    /// `-b:a` token (lossy codecs only).
    pub audio_bitrate: Option<String>,
    /// `-compression_level` for flac.
    pub flac_compression: Option<u8>,
    /// When false the audio map is dropped and `-an` emitted.
    pub include_audio: bool,

    /// `-movflags +faststart` (mp4 / mov / m4a only).
    pub faststart: bool,
    /// `paletteuse=dither=` for gif.
    pub gif_dither: Option<String>,
    /// gif `-loop 0` (true, infinite) vs `-loop -1` (false, play once).
    pub gif_loop: bool,
    /// `-metadata title=`.
    pub metadata_title: Option<String>,
    /// Render only this timeline span (seconds), e.g. the GUI's in/out marks;
    /// the output starts at `range.start`. `None` renders the whole timeline.
    pub range: Option<crate::model::TimeRange>,
    /// Normalize the final mix to -14 LUFS (single-pass `loudnorm`, the
    /// streaming-platform target) before encoding. The same as
    /// `loudness: Some(LoudnessPreset::Youtube)`.
    pub loudnorm: bool,
    /// Normalize the final mix to a platform's loudness target. Wins over `loudnorm`.
    pub loudness: Option<LoudnessPreset>,
    /// Reach the loudness target with one static gain instead of `loudnorm`'s moving one:
    /// the mix is measured first (the export's own graph, as `get_levels` reads it), then
    /// raised or lowered by the difference and held under the target's true peak by an
    /// oversampled limiter. The dynamics stay as mixed; only peaks the gain pushes over the
    /// ceiling are limited. Read only with a target (`loudness` or `loudnorm`).
    pub loudness_static: bool,
    /// The gain static normalization applies, in dB: measured by the render itself before
    /// it builds the graph, never taken from a caller.
    #[serde(skip)]
    #[schemars(skip)]
    pub loudness_gain_db: Option<f64>,
    /// Mux the cut's captions in as a soft subtitle stream (`mov_text` in mp4 / mov,
    /// `webvtt` in webm, `srt` in mkv). Refused for a container that carries none.
    pub embed_captions: bool,
    /// Write the cut's captions beside the export, one file per format
    /// (`cut.mp4` → `cut.srt` / `cut.vtt`).
    pub caption_sidecars: Vec<crate::captions_export::SubtitleFormat>,
    /// Replace caption files already beside the output (the MCP `export` tool's `overwrite`,
    /// or the GUI's confirmation); otherwise an export that would is refused.
    pub overwrite_sidecars: bool,
    /// Leave the generated captions out of the picture (typed titles are still drawn), for
    /// a delivery that carries them as a subtitle track or a file instead.
    pub skip_burned_captions: bool,
    /// Set by the render, never by a caller: the SubRip file of the cut's captions
    /// to mux in (see `side_files`).
    #[serde(skip)]
    pub subtitle_input: Option<String>,
    /// Set by the render, never by a caller: the FFMETADATA file of the cut's
    /// chapter markers to mux in.
    #[serde(skip)]
    pub chapters_input: Option<String>,
}

/// The most static loudness normalization raises or lowers a mix, in dB: a near-silent
/// cut is not lifted into a wall of noise.
pub const LOUDNESS_MAX_GAIN_DB: f64 = 30.0;

/// How far under the target's true-peak ceiling the static normalization's limiter is set:
/// resampling the 4x limiter's output back down rings a little over what it held.
pub(super) const STATIC_TP_MARGIN_DB: f64 = 0.3;

/// A delivery's loudness target: integrated loudness and the true-peak ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoudnessPreset {
    /// YouTube: -14 LUFS, -1 dBTP.
    Youtube,
    /// Spotify: -14 LUFS, -1 dBTP.
    Spotify,
    /// Apple Music / Podcasts: -16 LUFS, -1 dBTP.
    Apple,
    /// EBU R128 broadcast: -23 LUFS, -1 dBTP.
    Broadcast,
}

impl LoudnessPreset {
    pub fn lufs(self) -> f64 {
        match self {
            Self::Youtube | Self::Spotify => -14.0,
            Self::Apple => -16.0,
            Self::Broadcast => -23.0,
        }
    }

    pub fn true_peak_db(self) -> f64 {
        -1.0
    }

    /// The `loudnorm` filter that meets this target. Its `TP` is half a decibel under
    /// the ceiling: single-pass `loudnorm` overshoots its own true-peak target by about
    /// that much (the reason the plain `loudnorm` option asks for -1.5).
    pub(super) fn filter(self) -> String {
        format!("loudnorm=I={}:TP={}:LRA=11", self.lufs(), self.true_peak_db() - 0.5)
    }
}

impl ExportOptions {
    /// The caption files an export with these options writes beside `output` (`caption_sidecars`).
    pub fn caption_sidecar_paths(&self, output: &Path) -> Vec<PathBuf> {
        side_files::sidecar_paths(output, self).into_iter().map(|(_, p)| p).collect()
    }
}

impl Default for ExportOptions {
    // Reproduces the pre-existing argv exactly: no codecs, no crf, no faststart.
    fn default() -> Self {
        Self {
            container: Container::Mp4,
            video_codec: None,
            audio_codec: None,
            rate_control: RateControl::Crf,
            crf: None,
            video_bitrate: None,
            max_rate: None,
            buf_size: None,
            preset: None,
            prores_profile: None,
            tune: None,
            profile_v: None,
            pix_fmt: None,
            hwaccel: None,
            resolution: None,
            fit: Fit::Contain,
            fps: None,
            scaler: None,
            audio_sample_rate: None,
            audio_channels: None,
            audio_bitrate: None,
            flac_compression: None,
            include_audio: true,
            faststart: false,
            gif_dither: None,
            gif_loop: true,
            metadata_title: None,
            range: None,
            loudnorm: false,
            loudness: None,
            loudness_static: false,
            loudness_gain_db: None,
            embed_captions: false,
            caption_sidecars: Vec::new(),
            overwrite_sidecars: false,
            skip_burned_captions: false,
            subtitle_input: None,
            chapters_input: None,
        }
    }
}

/// The validated export range from `opts`, clamped to the timeline. `None`
/// when absent or empty after clamping (which falls back to a full export).
pub(super) fn effective_range(timeline: &Timeline, opts: &ExportOptions) -> Option<(f64, f64)> {
    timeline.export_span(opts.range.map(|r| (r.start, r.end)))
}

/// Whether a bitrate token like "8M" / "2500k" / "800000" is well-formed.
fn valid_bitrate(s: &str) -> bool {
    let s = s.trim();
    let digits = match s.char_indices().find(|(_, c)| !(c.is_ascii_digit() || *c == '.')) {
        Some((i, c)) => {
            // The only allowed trailing char is a single k/K/M unit suffix.
            if !matches!(c, 'k' | 'K' | 'm' | 'M') || i + c.len_utf8() != s.len() {
                return false;
            }
            &s[..i]
        }
        None => s,
    };
    !digits.is_empty() && digits.parse::<f64>().map(|v| v > 0.0).unwrap_or(false)
}

/// The `-tune` values each encoder accepts. x265 notably lacks x264's `film` /
/// `stillimage`; feeding an unknown tune makes the encoder fail to initialise.
pub(super) fn video_tunes(vc: &str) -> &'static [&'static str] {
    match vc {
        "libx264" => &[
            "film",
            "animation",
            "grain",
            "stillimage",
            "zerolatency",
            "fastdecode",
            "psnr",
            "ssim",
        ],
        "libx265" => &["psnr", "ssim", "grain", "zerolatency", "fastdecode", "animation"],
        _ => &[],
    }
}

/// `-pix_fmt` / filtergraph `format=` values the graph builders will emit. A
/// closed set, unlike a colour: `pix_fmt`/`scaler`/`gif_dither` reach the graph
/// unquoted (see `ExportFormat::scale_flags`, `export_format`, the gif
/// `paletteuse` chain), so an out-of-list value is dropped rather than passed
/// through — a `.kerf` file or an MCP call is untrusted the same as anything
/// else, and this must hold even when `validate_export` was never run.
pub(super) const PIX_FMTS: &[&str] = &[
    "yuv420p",
    "yuv422p",
    "yuv444p",
    "yuva420p",
    "yuva422p",
    "yuva444p",
    "yuv420p10le",
    "yuv422p10le",
    "yuv444p10le",
    "yuva420p10le",
    "yuva422p10le",
    "yuva444p10le",
    "yuv420p12le",
    "yuv422p12le",
    "yuv444p12le",
    "yuvj420p",
    "yuvj422p",
    "yuvj444p",
    "nv12",
    "nv21",
    "p010le",
    "rgb24",
    "bgr24",
    "rgba",
    "bgra",
    "gray",
];

/// `scale=…:flags=` scalers `swscale` accepts, matching the export dialog's choices.
pub(super) const SCALERS: &[&str] = &["bicubic", "bilinear", "lanczos", "neighbor", "spline"];

/// `paletteuse=dither=` modes for the gif pipeline.
pub(super) const GIF_DITHERS: &[&str] = &[
    "bayer",
    "heckbert",
    "floyd_steinberg",
    "sierra2",
    "sierra2_4a",
    "sierra3",
    "burkes",
    "atkinson",
    "none",
];

/// The hardware-encoder family a `-c:v` value belongs to, read from its ffmpeg
/// suffix. Software encoders (libx264 / libx265 / libsvtav1 / libvpx-vp9) and
/// the prores / gif pipelines are [`EncFamily::Software`]. The family decides how
/// the constant-quality knob, VBV caps and speed preset are spelled — each HW
/// encoder names the same intent differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EncFamily {
    Software,
    Nvenc,
    Qsv,
    VideoToolbox,
    Amf,
    Vaapi,
}

pub(super) fn enc_family(vc: &str) -> EncFamily {
    if vc.ends_with("_nvenc") {
        EncFamily::Nvenc
    } else if vc.ends_with("_qsv") {
        EncFamily::Qsv
    } else if vc.ends_with("_videotoolbox") {
        EncFamily::VideoToolbox
    } else if vc.ends_with("_amf") {
        EncFamily::Amf
    } else if vc.ends_with("_vaapi") {
        EncFamily::Vaapi
    } else {
        EncFamily::Software
    }
}

/// Whether the export hands `opts`' picture to a VAAPI encoder, which takes only frames
/// uploaded to the device: the graph ends in `format=nv12,hwupload` and the argv names the
/// device. (A GIF is paletted, so a VAAPI codec on one is ignored like any other.)
pub(super) fn uploads_to_vaapi(opts: &ExportOptions) -> bool {
    opts.container != Container::Gif
        && opts
            .video_codec
            .as_deref()
            .is_some_and(|vc| enc_family(vc) == EncFamily::Vaapi)
}

/// At the x264 CRF's number a hardware H.264 / HEVC encoder's constant QP spends about twice
/// the bits. Measured on a Radeon RX 7900 XT, 4K to 1080p, against a lossless render: x264
/// medium CRF 20 is 5.59 MB / 45.1 dB where `h264_amf` at constant QP 20 (the old 1:1
/// spelling) was 10.7 MB / 48.9 dB, and `h264_amf` / `hevc_amf` at QP 26 5.3-5.6 MB / 45.5-46.1 dB.
const HW_QP_OFFSET: u32 = 6;

/// AV1's quantiser index runs 0..255; the dialog's AV1 CRF scale (SVT-AV1's) 0..63.
const AV1_QP_PER_CRF: u32 = 4;

/// The constant QP an AMF or VAAPI encoder gets for the software-scale `crf` the options
/// carry. The one AV1 point measured (`av1_amf` QP 90: 6.41 MB / 46.4 dB) sits on the
/// high-quality side of x264 CRF 20; the rest of the AV1 line is the scale's slope.
pub(super) fn hw_qp(vc: &str, crf: u32) -> u32 {
    if vc.starts_with("av1_") {
        crf.saturating_mul(AV1_QP_PER_CRF).min(255)
    } else {
        crf.saturating_add(HW_QP_OFFSET).min(51)
    }
}

/// Whether `vc` produces an H.264 bitstream (software or any HW family).
pub(super) fn is_h264(vc: &str) -> bool {
    vc == "libx264" || vc.starts_with("h264_")
}

/// Whether `vc` produces an HEVC bitstream (software or any HW family) — these
/// need the `hvc1` tag in mp4 / mov so QuickTime / iOS will play them.
pub(super) fn is_hevc(vc: &str) -> bool {
    vc == "libx265" || vc.starts_with("hevc_")
}

/// Map a 0..51 CRF (lower = better) onto VideoToolbox's 1..100 constant-quality
/// scale (higher = better). VideoToolbox has no CRF; this preserves the intent
/// so the same `crf` field drives every encoder.
pub(super) fn crf_to_vt_quality(crf: u32) -> u32 {
    let crf = crf.min(51) as f64;
    (((1.0 - crf / 51.0) * 100.0).round() as u32).clamp(1, 100)
}

/// Validate an option set against the timeline's available streams, returning a
/// list of human-readable problems (empty = OK). Pure; called by the pre-launch
/// guard in [`render_with`] and mirrored client-side by the export dialog.
pub fn validate_export(opts: &ExportOptions, has_video: bool, has_audio: bool) -> Vec<String> {
    let mut issues = Vec::new();
    let c = opts.container;
    let want_video = has_video && !c.is_audio_only();
    let want_audio = has_audio && !c.is_video_only() && opts.include_audio;

    if c.is_audio_only() && !has_audio {
        issues.push(format!(
            "{} is audio-only, but the timeline has no audio.",
            c.ext().to_uppercase()
        ));
    }
    if c.is_video_only() && !has_video {
        issues.push("GIF export needs video, but the timeline has no video.".to_string());
    }
    if !want_video && !want_audio {
        issues.push("These settings would export nothing.".to_string());
    }
    if want_video {
        if let Some(vc) = opts.video_codec.as_deref() {
            if !c.video_ok(vc) {
                issues.push(format!("{vc} can't go in a .{} file.", c.ext()));
            }
        }
        let rate_mode = !matches!(opts.video_codec.as_deref(), Some("prores_ks") | Some("gif"));
        if rate_mode && matches!(opts.rate_control, RateControl::Bitrate | RateControl::TwoPass) && opts.video_bitrate.is_none() {
            issues.push("A target video bitrate is required for bitrate / two-pass.".to_string());
        }
        if let Some(vc) = opts.video_codec.as_deref() {
            if matches!(opts.rate_control, RateControl::TwoPass) && enc_family(vc) != EncFamily::Software {
                issues.push(format!(
                    "Two-pass encoding isn't supported for hardware encoder {vc}; use crf or bitrate."
                ));
            }
        }
        if let (Some(vc), Some(t)) = (opts.video_codec.as_deref(), opts.tune.as_deref()) {
            if matches!(vc, "libx264" | "libx265") && !t.is_empty() && !video_tunes(vc).contains(&t) {
                issues.push(format!("tune \"{t}\" is not valid for {vc}."));
            }
        }
    }
    if let Some(b) = opts.video_bitrate.as_deref() {
        if !valid_bitrate(b) {
            issues.push(format!("Invalid video bitrate \"{b}\"."));
        }
    }
    for (label, v) in [("max rate", &opts.max_rate), ("buffer size", &opts.buf_size)] {
        if let Some(b) = v.as_deref() {
            if !valid_bitrate(b) {
                issues.push(format!("Invalid {label} \"{b}\"."));
            }
        }
    }
    if let Some(pf) = opts.pix_fmt.as_deref() {
        if !PIX_FMTS.contains(&pf) {
            issues.push(format!("Unsupported pixel format \"{pf}\"."));
        }
    }
    if want_video && uploads_to_vaapi(opts) && !matches!(opts.pix_fmt.as_deref(), None | Some("yuv420p" | "nv12")) {
        issues.push("VAAPI encoders take 8-bit 4:2:0 (yuv420p) here.".to_string());
    }
    if let Some(s) = opts.scaler.as_deref() {
        if !SCALERS.contains(&s) {
            issues.push(format!("Unsupported scaler \"{s}\"."));
        }
    }
    if let Some(d) = opts.gif_dither.as_deref() {
        if !GIF_DITHERS.contains(&d) {
            issues.push(format!("Unsupported gif dither \"{d}\"."));
        }
    }
    if want_audio {
        if let Some(ac) = opts.audio_codec.as_deref() {
            if !c.audio_ok(ac) {
                issues.push(format!("{ac} can't go in a .{} file.", c.ext()));
            }
        }
        if let Some(b) = opts.audio_bitrate.as_deref() {
            if !valid_bitrate(b) {
                issues.push(format!("Invalid audio bitrate \"{b}\"."));
            }
        }
    }
    if opts.embed_captions && side_files::subtitle_codec(c).is_none() {
        issues.push(format!(
            "A .{} file can't carry subtitles; write them beside the export instead.",
            c.ext()
        ));
    }
    issues
}
