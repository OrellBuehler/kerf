//! The delivery frame and rate of an export.

use super::*;

/// The single output shape every clip is normalized to before `concat`. The
/// `concat` filter requires identical resolution / frame rate / sample format
/// across its inputs, and `concat`'s `a=1` requires every segment to carry
/// audio — so clips from a video-only asset get synthesized silence.
#[derive(Debug, Clone)]
pub(super) struct ExportFormat {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) fps: f64,
    pub(super) sample_rate: u32,
    pub(super) channels: u16,
    /// Terminal pixel format: argv `-pix_fmt` and the filtergraph terminal
    /// `format=` are kept in sync through this single field.
    pub(super) pix_fmt: String,
    /// Optional `scale=…:flags=` scaler.
    pub(super) scaler: Option<String>,
    /// Letterbox or fill-and-crop when the footage and the frame differ in shape.
    pub(super) fit: Fit,
}

impl Default for ExportFormat {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30.0,
            sample_rate: 48_000,
            channels: 2,
            pix_fmt: "yuv420p".to_string(),
            scaler: None,
            fit: Fit::Contain,
        }
    }
}

impl ExportFormat {
    pub(super) fn channel_layout(&self) -> &'static str {
        if self.channels <= 1 {
            "mono"
        } else {
            "stereo"
        }
    }
    /// The `:flags=…` suffix to append to a `scale` filter, or empty. An
    /// out-of-list value (a stale `.kerf` file, a hand-crafted MCP call) is
    /// dropped rather than spliced in unquoted — see [`SCALERS`].
    pub(super) fn scale_flags(&self) -> String {
        match self.scaler.as_deref() {
            Some(s) if SCALERS.contains(&s) => format!(":flags={s}"),
            _ => String::new(),
        }
    }
}

/// The frame this timeline actually renders at: the project's delivery format
/// when one is set, otherwise the shape its footage gives it. What a readiness
/// check has to compare a platform's expectations against.
pub fn delivery_frame(timeline: &Timeline, assets: &[Asset]) -> (u32, u32) {
    let f = export_format(timeline, assets, &ExportOptions::default());
    (f.width, f.height)
}

/// Where a render's frame rate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FpsSource {
    /// The export options named one (`ExportOptions::fps`).
    Options,
    /// The project's delivery frame states one (`Delivery::fps`).
    Delivery,
    /// Neither did, so it is the rate of the first clip with a picture — in footage of mixed
    /// rates, whichever comes first.
    Footage,
    /// Nothing to follow: no clip with a picture, so the 30 fps default.
    Default,
}

/// The frame rate the cut renders at when the export names none — the delivery frame's, else the
/// first picture's — and which of the two it was. What a stream plays at, a still is placed on and
/// an export writes by default; `timeline_summary` shows it so the default is not a surprise.
pub fn delivery_rate(timeline: &Timeline, assets: &[Asset]) -> (f64, FpsSource) {
    let (f, source) = export_format_sourced(&timeline.for_render(), assets, &ExportOptions::default());
    (f.fps, source)
}

/// The picture an export writes, as it will be rendered: what the options and the project's
/// delivery frame made of the footage's own, reported back so a caller sees the size and the rate
/// it got rather than the ones it hoped for.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct ExportedFrame {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Where `fps` came from. `footage` and `default` are the cases a caller who cares about the
    /// rate should set `options.fps`, or the delivery frame's `fps`, for.
    pub fps_source: FpsSource,
}

/// The picture `render_with_progress(timeline, assets, .., opts, ..)` writes: the frame of the cut as
/// the export renders it (a range sliced, muted tracks gone). `None` when it writes no picture (an
/// audio container, or a cut with no video clip).
pub fn exported_frame(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Option<ExportedFrame> {
    let cut = export_timeline(timeline, opts);
    let has_video = cut.tracks.iter().any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    if opts.container.is_audio_only() || !has_video {
        return None;
    }
    let (f, fps_source) = export_format_sourced(&cut, assets, opts);
    Some(ExportedFrame {
        width: f.width,
        height: f.height,
        fps: f.fps,
        fps_source,
    })
}

/// [`exported_frame`] of one file of a multi-format export: the cut delivered at `variant`'s frame
/// (which keeps the project's frame rate, [`Timeline::for_delivery`]) with `opts` as
/// `render_variants` takes them.
pub fn variant_frame(
    timeline: &Timeline,
    assets: &[Asset],
    variant: &ExportVariant,
    opts: &ExportOptions,
) -> Option<ExportedFrame> {
    let per_variant = ExportOptions {
        resolution: Some((variant.delivery.width, variant.delivery.height)),
        fit: variant.delivery.fit,
        ..opts.clone()
    };
    exported_frame(&timeline.for_delivery(variant.delivery), assets, &per_variant)
}

/// The slice of [`ExportFormat`] a renderer of one *frame* needs — the canvas
/// and how footage meets it — taken from the same `export_format` the still and
/// the export use, so a second renderer (the GPU compositor, through
/// [`crate::render_plan::RenderPlan`]) cannot drift from them on what the frame
/// is.
#[derive(Debug, Clone)]
pub(crate) struct RenderGeometry {
    pub width: u32,
    pub height: u32,
    pub fit: Fit,
    /// The export's `scaler` as chosen (`None` is swscale's bicubic default).
    pub scaler: Option<String>,
    /// The output rate, exactly as the graph's `fps=` / `color=r=` print it (`{}` of
    /// this `f64`, which FFmpeg parses back to a rational).
    pub fps: f64,
    /// The delivery's terminal pixel format (`format=` in the graph).
    pub pix_fmt: String,
}

pub(crate) fn render_geometry(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> RenderGeometry {
    let f = export_format(timeline, assets, opts);
    RenderGeometry {
        width: f.width,
        height: f.height,
        fit: f.fit,
        scaler: f.scaler,
        fps: f.fps,
        pix_fmt: f.pix_fmt,
    }
}

/// Derive the output shape from the first clip (across all tracks) that carries
/// a video stream and the first that carries audio, falling back to 1080p30
/// stereo defaults. The project's delivery frame (size, fit and frame rate) comes
/// next, and `opts` carrying resolution or fps overrides win over all of it.
pub(super) fn export_format(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> ExportFormat {
    export_format_sourced(timeline, assets, opts).0
}

/// [`export_format`], and where its frame rate came from.
pub(super) fn export_format_sourced(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> (ExportFormat, FpsSource) {
    let mut fps_source = FpsSource::Default;
    let stream_of = |clip: &crate::model::Clip, kind: StreamKind| {
        assets
            .iter()
            .find(|a| a.id == clip.asset_id)
            .and_then(|a| a.streams.iter().find(|s| s.kind == kind))
    };
    let clips = || timeline.tracks.iter().flat_map(|t| t.clips.iter());

    let mut fmt = ExportFormat::default();
    if let Some((clip, v)) = clips().find_map(|c| stream_of(c, StreamKind::Video).map(|s| (c, s))) {
        // A reframed clip's source dimensions describe the *sphere*, not the
        // deliverable — inheriting them would export a 16:9 reframe of a 5.7K
        // Insta360 capture at 5760x2880. Keep the 1080p default instead and let
        // `opts.resolution` override as usual. Keyed off the reframe rather than
        // the projection, so an un-reframed 360 clip still behaves as before.
        let reframes_to_flat = clip
            .reframe
            .as_ref()
            .is_some_and(|r| r.output == crate::model::Projection::Flat);
        if let (false, Some(w), Some(h)) = (reframes_to_flat, v.width, v.height) {
            fmt.width = w;
            fmt.height = h;
        }
        if let Some(f) = v.fps.filter(|f| *f > 0.0) {
            fmt.fps = f;
            fps_source = FpsSource::Footage;
        }
    }
    if let Some(a) = clips().find_map(|c| stream_of(c, StreamKind::Audio)) {
        if let Some(r) = a.sample_rate.filter(|r| *r > 0) {
            fmt.sample_rate = r;
        }
        if let Some(c) = a.channels.filter(|c| *c > 0) {
            fmt.channels = c;
        }
    }
    // The project's delivery frame sits between the footage and an explicit
    // export resolution: it overrides "whatever the first clip is" so the
    // preview, the still and the export agree on the shape being cut for, and
    // yields to a resolution typed into the export dialog so a one-off render
    // at a different size still works. Its frame rate, when it states one, is
    // the same: the footage's rate is only the default of a cut that never
    // chose (in mixed footage, whichever clip is first), and `opts.fps` below
    // still wins over it.
    if let Some(d) = timeline.format {
        fmt.width = d.width;
        fmt.height = d.height;
        if let Some(fps) = d.rate() {
            fmt.fps = fps;
            fps_source = FpsSource::Delivery;
        }
    }
    if let Some((w, h)) = opts.resolution {
        fmt.width = w;
        fmt.height = h;
    }
    if let Some(f) = opts.fps.filter(|f| *f > 0.0) {
        fmt.fps = f;
        fps_source = FpsSource::Options;
    }
    if let Some(r) = opts.audio_sample_rate.filter(|r| *r > 0) {
        fmt.sample_rate = r;
    }
    if let Some(c) = opts.audio_channels.filter(|c| *c > 0) {
        fmt.channels = c;
    }
    // libopus only encodes at 48 kHz; force it regardless of source / override.
    if opts.audio_codec.as_deref() == Some("libopus") {
        fmt.sample_rate = 48_000;
    }
    // yuv420p (and most 4:2:0 formats) require even dimensions, so clamp both
    // source-derived and custom sizes before they reach `scale=`/`color=s=`.
    fmt.width = (fmt.width & !1).max(2);
    fmt.height = (fmt.height & !1).max(2);
    // An out-of-list `pix_fmt` (see [`PIX_FMTS`]) is treated as if none were
    // given, falling through to the same ProRes default a bare `None` gets —
    // never spliced into the graph terminal `format=` unchecked.
    if let Some(pf) = opts.pix_fmt.as_deref().filter(|pf| PIX_FMTS.contains(pf)) {
        fmt.pix_fmt = pf.to_string();
    } else if opts.video_codec.as_deref() == Some("prores_ks") {
        // ProRes cannot encode 4:2:0; default a None pix_fmt to 10-bit 4:2:2 (or
        // 4:4:4 for the 4444 profiles) so the graph terminal doesn't silently
        // decimate a 10-bit / 4:2:2 source to 8-bit 4:2:0 before the encode.
        fmt.pix_fmt = if matches!(opts.prores_profile, Some(4) | Some(5)) {
            "yuva444p10le".to_string()
        } else {
            "yuv422p10le".to_string()
        };
    }
    fmt.scaler = opts.scaler.clone();
    // Same precedence for the fit: a Cover delivery crops in the preview exactly
    // as it will on export, but an explicit non-default `opts.fit` still wins.
    fmt.fit = match (timeline.format, opts.fit) {
        (Some(d), Fit::Contain) => d.fit,
        (_, f) => f,
    };
    (fmt, fps_source)
}
