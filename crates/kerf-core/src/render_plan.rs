//! What one rendered frame is made of: the canvas and the ordered video layers
//! visible at a timeline time, with everything already resolved.
//!
//! Two renderers draw a frame of the cut — the FFmpeg filter graph
//! (`engine::cli::build_still_args`, and the export / playback graphs) and the
//! wgpu compositor in `kerf-gpu` — and they must agree on *which* clips are on
//! screen, *where in the source* each one is, and *what pose* it holds. That
//! agreement is this module and [`crate::planner`]: pure, unit-tested, and consumed
//! by both, so neither re-derives it. `build_still_args` takes its active-clip list
//! from [`active_video_clips`], the same arithmetic [`Planner`] indexes.
//!
//! The plan is a *description*, not a renderer, and it is complete: a layer carries
//! its sampled transform and colour, its mask, effects, reframe camera, HDR flag and
//! the per-frame state of its transitions ([`LayerFx`]), and the plan carries the
//! live text. What a compositor does *not* draw is not decided here as stored text:
//! [`RenderPlan::reasons`] is a pure function of these fields and a [`GpuCaps`], so
//! that frame goes through FFmpeg instead of being drawn wrong.
//!
//! **Two modes.** [`PlanMode::Still`] is the contract the GPU path started with:
//! what `build_still_args` draws at a time `t` (the transform sampled once, fades and
//! transitions left out, text drawn statically). [`PlanMode::Motion`] is the export
//! graph at output frame `k`: see [`Planner`].

use uuid::Uuid;

use crate::clip_timing::{FadeEdge, FadeStep, FadeTint, Rational};
use crate::engine::Fit;
use crate::error::Result;
use crate::layer_geometry::{LayerGeometry, Placement};
use crate::model::{
    pix_fmt_layout, pix_fmt_subsampling, Asset, Clip, Color, Hdr, Mask, PixLayout, Projection, ResolvedReframe, StreamInfo,
    StreamKind, Subsampling, Timeline, Transform, VideoEffect,
};
use crate::plan_caps::{EffectKinds, GpuCaps, LayerRef, Unsupported};
use crate::planner::{PlanRequest, Planner};
use crate::ExportOptions;

/// A video clip visible at a timeline time, paired with where in its source the
/// frame comes from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ActiveClip<'a> {
    pub clip: &'a Clip,
    /// The source time to decode, honoring speed / reverse and clamped to the
    /// asset's duration (a seek past the end would decode nothing).
    pub source_time: f64,
    /// Seconds from the clip's start — the time keyframes are sampled at.
    pub local_time: f64,
}

impl ActiveClip<'_> {
    /// The clip's transform sampled at this instant, so a keyframed clip shows
    /// its pose rather than the static one.
    pub(crate) fn transform(&self) -> Transform {
        self.clip.transform_at(self.local_time)
    }
}

/// The source time a clip shows at timeline time `t`: honors speed and reverse and
/// is clamped to the asset (`asset_duration`), because a seek past the end would
/// decode nothing. It is not limited to the clip's own `source_in..source_out`: a
/// clip playing on under a transition borrows the handle beyond it.
pub(crate) fn clip_source_time(clip: &Clip, asset_duration: f64, t: f64) -> f64 {
    let off = (t - clip.timeline_start) * clip.speed_mag();
    let raw = if clip.is_reversed() {
        clip.source_out - off
    } else {
        clip.source_in + off
    };
    raw.clamp(0.0, asset_duration.max(0.0))
}

/// The video clips whose timeline span contains `t`, in composite order: tracks
/// in list order (later tracks draw on top) and clips within a track in
/// timeline order. `timeline` is expected to be the **rendered** timeline
/// ([`Timeline::for_render`]): muting, solo and disabled clips are applied
/// before the plan, not inside it.
///
/// This is the computation `build_still_args` always made inline; it lives here
/// so the GPU plan shares it instead of copying it.
pub(crate) fn active_video_clips<'a>(timeline: &'a Timeline, assets: &[Asset], t: f64) -> Vec<ActiveClip<'a>> {
    let asset_of = |id| assets.iter().find(|a: &&Asset| a.id == id);
    let mut active = Vec::new();
    for track in &timeline.tracks {
        if track.kind != StreamKind::Video {
            continue;
        }
        let mut order: Vec<usize> = (0..track.clips.len()).collect();
        order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
        for &ci in &order {
            let clip = &track.clips[ci];
            if t < clip.timeline_start || t >= clip.timeline_end() {
                continue;
            }
            let dur = asset_of(clip.asset_id).map(|a| a.duration).unwrap_or(clip.source_out);
            active.push(ActiveClip {
                clip,
                source_time: clip_source_time(clip, dur, t),
                local_time: (t - clip.timeline_start).max(0.0),
            });
        }
    }
    active
}

/// The size a still of a `frame_w` x `frame_h` delivery frame is rendered at:
/// the delivery aspect capped to `max_width`, both sides even (yuv420p). One
/// function for the FFmpeg still and the GPU render, so a preview at a given
/// width is the same size from either.
pub fn still_size(frame_w: u32, frame_h: u32, max_width: u32) -> (u32, u32) {
    let ow = (max_width.min(frame_w).max(2)) & !1;
    let oh = ((((ow as u64) * (frame_h as u64)) / (frame_w.max(1) as u64)) as u32).max(2) & !1;
    (ow, oh)
}

/// The YUV→RGB matrix applied when the composited frame is converted for
/// display. A plan carries one, because FFmpeg composites in YUV and converts the
/// **result** once — so there is exactly one matrix per frame, not one per layer.
/// Which one it is depends on the FFmpeg: [`CompositeColorPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvMatrix {
    /// BT.601 (Kr 0.299, Kb 0.114).
    Bt601,
    /// BT.709 (Kr 0.2126, Kb 0.0722).
    Bt709,
    /// BT.2020 non-constant luminance (Kr 0.2627, Kb 0.0593).
    Bt2020,
}

impl YuvMatrix {
    /// `(Kr, Kb)`, the luma weights of red and blue.
    pub fn weights(self) -> (f64, f64) {
        match self {
            YuvMatrix::Bt601 => (0.299, 0.114),
            YuvMatrix::Bt709 => (0.2126, 0.0722),
            YuvMatrix::Bt2020 => (0.2627, 0.0593),
        }
    }
}

/// How this FFmpeg picks the YCbCr matrix of the composite — the matrix the one
/// final conversion to RGB (and so the picture a still or a preview shows) uses.
/// It differs between FFmpeg versions, so it is a property of the FFmpeg in use,
/// measured rather than guessed from a version string: see
/// [`composite_color_policy`](crate::composite_color_policy), which probes it once
/// per process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeColorPolicy {
    /// The composite is untagged — the black canvas carries no colourspace — and
    /// every layer is blended as it is: one matrix, **BT.601**, whatever the
    /// layers were tagged. FFmpeg 6.
    FixedBt601,
    /// Colourspace is negotiated across the overlay chain (FFmpeg 9): the
    /// **bottom layer's tag** becomes the composite's matrix, and a layer tagged
    /// otherwise is converted into it by the scaler — an arithmetic the compositor
    /// does not reproduce, so such a stack is refused. The matrix of a stack whose
    /// layers all agree follows their tag.
    BottomLayerTag,
    /// The probe could not tell which of the two this FFmpeg is (it would not run,
    /// timed out, or answered something unreadable) — and no guess is safe: the
    /// two disagree exactly on BT.709 and BT.2020 footage, so drawing it under
    /// either would be wrong on the FFmpeg the other describes. What both agree on
    /// is a stack whose layers are all BT.601-class (BT.601, SMPTE 170M, BT.470 BG,
    /// untagged, RGB): that is drawn, as BT.601, and **everything else is
    /// refused**, so the frame goes through FFmpeg.
    Unknown,
}

/// The steepest shrink of one scale stage the compositor claims to match: swscale's
/// vertical scaler on x86 is not bit-exact with the C arithmetic the compositor
/// follows, and the difference grows with the number of input samples per output
/// one — one level up to about 4:1, two to three up to 20:1, up to five at 40:1
/// (`the_scaler_matches_ffmpegs_scale_plane_by_plane`). Past it the figure is not
/// measured (a 58:1 shrink of a 4K test pattern reads 16 levels off in RGB), so the
/// frame goes through FFmpeg.
pub const MAX_SHRINK: u32 = 40;

/// Which graph a plan describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanMode {
    /// What `build_still_args` draws at a time `t`: the transform sampled at that
    /// instant, no fades or transitions in the picture, text drawn statically. The
    /// contract the GPU path started with, and what every `RenderPlan::at` returns.
    Still,
    /// What the export graph draws at output frame `k` (`Planner::at_frame`): the
    /// graph's own time for every expression it evaluates, outgoing clips playing on
    /// their tails, fades and transitions timed from the clip, keyframed clips
    /// placed by the overlay rather than padded, text on `between(t,start,end)`.
    Motion,
}

/// The frame a plan renders into.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanCanvas {
    /// The delivery frame (`export_format`: the project frame, else the shape of
    /// the footage, else an explicit resolution). Previews render a downscale of
    /// it — see [`still_size`].
    pub width: u32,
    pub height: u32,
    /// How footage of another shape meets the frame.
    pub fit: Fit,
    /// The export's `scale` flags (`bicubic` when none is chosen), as typed.
    pub scaler: Option<String>,
    /// The matrix the composited frame is converted with, limited range, as this
    /// FFmpeg does it ([`CompositeColorPolicy`]): **BT.601** when the composite is
    /// untagged (FFmpeg 6 — measured for BT.709-tagged, BT.601-tagged and untagged
    /// sources alike), or, where colourspace is negotiated along the overlay chain
    /// (FFmpeg 9), the matrix the layers share. An export is not converted at all,
    /// so a player shows it as BT.709 (HD); the two disagree by a few levels on
    /// saturated colour. Moving this to BT.709 is a decision for the day the GPU
    /// path *replaces* the FFmpeg preview.
    pub matrix: YuvMatrix,
    /// The composite colour policy the matrix was decided under — kept so
    /// [`RenderPlan::reasons`] can say why a stack is refused without re-planning.
    pub policy: CompositeColorPolicy,
    /// The output rate, as the rational FFmpeg parses the graph's `fps=` text to.
    pub fps: Rational,
    /// The delivery's terminal pixel format. A [`PlanMode::Motion`] frame is only
    /// drawn for 8-bit 4:2:0 (`yuv420p`); a preview still is always that.
    pub pix_fmt: String,
    /// The delivery is a gif (palettegen / paletteuse on the composite).
    pub gif: bool,
}

/// What the compositor needs to know about a layer's source picture.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanStream {
    /// The **displayed** size: every FFmpeg decode autorotates, so these are
    /// already the dimensions of the pixels a decode returns.
    pub width: u32,
    pub height: u32,
    /// How far the container turns the picture to show it upright, in degrees
    /// (informational — the decode has already applied it).
    pub rotation: i16,
    pub fps: Option<f64>,
    pub codec: String,
    /// Transfer / primaries as ffprobe names them. HDR is decided from these.
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    /// ffprobe's pixel format, when the asset recorded it. A renderer that draws
    /// only opaque pictures refuses an alpha one from this; `None` means "not
    /// recorded" and is the decoder's to find out (see `kerf-gpu`'s `source`).
    pub pix_fmt: Option<String>,
    /// The YCbCr matrix the stream declares, as ffprobe names it.
    pub color_space: Option<String>,
}

impl PlanStream {
    /// The layout of the picture, from the probed pixel format; `None` when it was
    /// never recorded (an asset saved before it was) or is not a known-opaque one.
    pub fn layout(&self) -> Option<PixLayout> {
        self.pix_fmt.as_deref().and_then(pix_fmt_layout)
    }

    /// The matrix FFmpeg converts this picture with *inside* a graph (the frame's
    /// own, which `scale` takes from its colourspace tag): BT.709 and BT.2020 as
    /// tagged, BT.601 for the SMPTE 170M / BT.470 BG tags **and for none at all**
    /// (swscale's default) — and for an RGB picture, which is converted to YCbCr
    /// with that default.
    ///
    /// `None` when it cannot be known: a matrix the compositor has no coefficients
    /// for, or an asset whose pixel format was never recorded. For such an asset
    /// "no tag" means *either* untagged *or* "probed before tags were read", and a
    /// BT.709 clip from an old project must not be taken for BT.601.
    pub fn matrix(&self) -> Option<YuvMatrix> {
        self.pix_fmt.as_ref()?;
        if self.layout() == Some(PixLayout::Rgb) {
            return Some(YuvMatrix::Bt601);
        }
        match self.color_space.as_deref() {
            None | Some("smpte170m") | Some("bt470bg") => Some(YuvMatrix::Bt601),
            Some("bt709") => Some(YuvMatrix::Bt709),
            Some("bt2020nc") => Some(YuvMatrix::Bt2020),
            Some(_) => None,
        }
    }

    /// The native chroma grid `crop`, `pad` and the Cover crop round to for this
    /// picture ([`pix_fmt_subsampling`]); `None` when the pixel format was never
    /// recorded or is not one whose rounding was measured.
    pub fn subsampling(&self) -> Option<Subsampling> {
        self.pix_fmt.as_deref().and_then(pix_fmt_subsampling)
    }

    /// Whether the picture may be full range: ffprobe names every full-range 4:2:0 /
    /// 4:2:2 / 4:4:4 picture `yuvj…` (a JPEG, an mjpeg, an H.264 file flagged `pc`),
    /// and an asset that never recorded its format is not known not to be.
    pub fn maybe_full_range(&self) -> bool {
        self.pix_fmt
            .as_deref()
            .is_none_or(|f| f.to_ascii_lowercase().starts_with("yuvj"))
    }

    /// An RGB-family picture (see [`PixLayout::Rgb`]).
    pub fn is_rgb(&self) -> bool {
        self.layout() == Some(PixLayout::Rgb)
    }

    pub(crate) fn of(s: &StreamInfo) -> Option<Self> {
        Some(Self {
            width: s.width.filter(|w| *w > 0)?,
            height: s.height.filter(|h| *h > 0)?,
            rotation: s.rotation,
            fps: s.fps,
            codec: s.codec.clone(),
            color_transfer: s.color_transfer.clone(),
            color_primaries: s.color_primaries.clone(),
            pix_fmt: s.pix_fmt.clone(),
            color_space: s.color_space.clone(),
        })
    }
}

/// Which of a keyframed clip's channels move. The export graph builds a keyframed
/// clip differently from a static one whatever its pose at one instant ([`Placement`]),
/// and what it needs a renderer to refuse depends on which channels the keys drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Animated {
    /// Some key turns the picture (so the graph has a `rotate`, in a `hypot(iw,ih)` box).
    pub rotates: bool,
    /// Some key is below full opacity (so the graph has a `geq` alpha).
    pub opacity: bool,
    /// The keys' scales differ: the zoom moves. The graph's `scale eval=frame` sits before
    /// `fps`, so it is read at the *source* frame's time (a slower source holds each size for
    /// several output frames), and FFmpeg does not reliably show it at all: a filter after it
    /// that cannot take a mid-stream size change holds the first frame's size (`geq`,
    /// `colorchannelmixer`, and the converter in front of `overlay` for a chain without an
    /// alpha plane; see `rendered.rs`). Nothing here decides which, so a moving zoom in an
    /// export frame is refused.
    pub zooms: bool,
}

/// What a clip's transitions and fades do to one layer at this frame, **per layer
/// and as the graph has it**: there is no transition node. A dissolve is two
/// ordinary layers (the outgoing clip playing on its [`tail`](Self::tail), the
/// incoming one on an alpha ramp), a dip is a fade out and a fade in, a slide or push
/// is a [`motion`](Self::motion) offset on one or both.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LayerFx {
    /// Every fade of the clip's picture, timed on the timeline: its own fades, the
    /// dips, a dissolve's ramp ([`FadeTint::Alpha`]). Steps, not a number — how much of
    /// the picture is left is [`LayerFx::strength`], which counts frames the way
    /// FFmpeg's `fade` does.
    pub fades: Vec<FadeStep>,
    /// The travel a slide or push gives the layer at this frame, in frame widths and
    /// heights (exact: the overlay truncates it to the 4:2:0 pixel grid, see
    /// [`MotionKeys::at`](crate::clip_timing::MotionKeys::at)). Added to the layer's
    /// own position.
    pub motion: (f64, f64),
    /// The frame is past the clip's own end: it is playing on, from source it
    /// borrowed from its handle, under the clip that replaces it.
    pub tail: bool,
}

impl LayerFx {
    /// What the steps of one tint leave of the picture at output frame `frame`:
    /// `1` untouched, `0` gone (a fade-in rises, a fade-out falls). The product of
    /// the tint's steps, as the chain applies them one after another.
    pub fn strength(&self, tint: FadeTint, frame: i64, fps: Rational) -> f64 {
        self.fades
            .iter()
            .filter(|s| s.tint == tint)
            .map(|s| s.progress_at_frame(frame, fps.num, fps.den))
            .product()
    }

    /// The first fade still changing the picture at `frame`: its edge, and whether
    /// it is a dissolve's alpha ramp.
    pub fn in_progress(&self, frame: i64, fps: Rational) -> Option<(FadeEdge, bool)> {
        self.fades
            .iter()
            .find(|s| s.progress_at_frame(frame, fps.num, fps.den) < 1.0)
            .map(|s| (s.edge, s.tint == FadeTint::Alpha))
    }

    /// A slide or push is moving the layer.
    pub fn travels(&self) -> bool {
        self.motion != (0.0, 0.0)
    }
}

/// What the source-frame pick needs of a layer, which is not what its decoded
/// `source_time` says: FFmpeg's `fps` filter picks the frame for output frame `k`
/// by the rule `the last frame with pts < ws + s * ((k + 1/2) / fps - start)`
/// (mirrored over the window for reverse), so the pick wants the window, the speed
/// and the start — and the plan's `frame` and `canvas.fps`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanTiming {
    /// The overlay's `enable` window on the timeline, tail included. A [`PlanMode::Motion`]
    /// plan holds every layer whose window contains the frame time (end included, as
    /// `between` has it): **candidates**. An equal-rate source has no frame at the
    /// window's end and the export draws nothing there; which layers are really drawn
    /// is the pick's to say.
    pub window: (f64, f64),
    /// The source window the chain trims (`clip_source_window`, tail included).
    pub source_window: (f64, f64),
    /// Speed magnitude.
    pub speed: f64,
    pub reversed: bool,
}

/// The interpolation `v360` resamples with: the export's, or the still's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReframeInterp {
    /// The still and the preview (`line`, for speed).
    Line,
    /// The export (`cubic`, sharper edges on a wide reframe).
    Cubic,
}

/// A layer's 360 reframe camera at this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanReframe {
    /// The camera sampled on its curve at the frame's time. The export graph does not
    /// hold that curve: `v360` keeps the pose its `sendcmd` schedule last set (a 0.05
    /// degree gate, values rounded to four decimals, an unmoving channel at its static
    /// value), which is what a pass that draws a reframe will have to replay.
    pub pose: ResolvedReframe,
    pub interp: ReframeInterp,
}

/// One text overlay live at this frame, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanText {
    pub id: Uuid,
    pub text: String,
    /// Font height as a fraction of the frame height.
    pub size: f64,
    /// The text's centre, in frame fractions, sampled at this frame.
    pub pos: (f64, f64),
    /// Opacity, 0..1 (`TextOverlay::sample`).
    pub alpha: f64,
    /// A valid colour (else the safe default), as FFmpeg names it.
    pub color: String,
    /// The box behind the text; `None` when there is none or its colour is invalid.
    pub bg: Option<String>,
    /// The font file `drawtext` would use, resolved on this machine. `None` is FFmpeg's
    /// default font, which no other renderer can reproduce: never drawn.
    pub font_file: Option<std::path::PathBuf>,
    /// No bold face was found for a bold request, and `drawtext` thickens the glyphs
    /// with a same-colour border instead.
    pub synthetic_bold: bool,
}

/// A layer whose source has no picture of a known size: not drawable under any caps.
pub type Pictureless = LayerRef;

/// One video layer of a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanLayer {
    pub clip_id: Uuid,
    pub asset_id: Uuid,
    /// Index of the timeline track the layer comes from (0 is the bottom one).
    pub track: usize,
    pub path: String,
    /// A still image: decoded once, never seeked.
    pub is_image: bool,
    /// The source time to decode (speed / reverse honored, clamped to the asset).
    pub source_time: f64,
    /// Seconds from the clip's start; keyframes were sampled here. A [`PlanMode::Motion`]
    /// plan samples at the graph's own frame time, which is not `frame / fps`.
    pub clip_time: f64,
    pub stream: PlanStream,
    /// The transform **sampled** at this instant.
    pub transform: Transform,
    pub color: Color,
    /// The asset's name, for messages.
    pub name: String,
    /// The asset's spherical projection, if it is 360 footage.
    pub projection: Option<Projection>,
    /// The footage is HDR and is tone-mapped to SDR, **before** the geometry in a
    /// still and **after** the fit `scale` (and `fps`) in the export graph.
    pub hdr: Option<Hdr>,
    /// The clip's effects in order; a chroma key's colour is already a safe one.
    pub effects: Vec<VideoEffect>,
    /// The clip's mask, normalized; fractions of the *layer* frame.
    pub mask: Option<Mask>,
    pub reframe: Option<PlanReframe>,
    pub fx: LayerFx,
    /// `Some` for a keyframed clip.
    pub animated: Option<Animated>,
    pub timing: PlanTiming,
}

/// The ordered video layers visible at one timeline time, on their canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderPlan {
    /// The (non-negative) timeline time the plan is for. For [`PlanMode::Motion`], the
    /// exact start of the output frame's slot (`k * den / num`).
    pub time: f64,
    pub mode: PlanMode,
    /// The output frame: exact for [`PlanMode::Motion`], the frame nearest `time` for
    /// a still (which is what fades are counted in).
    pub frame: i64,
    pub canvas: PlanCanvas,
    /// Bottom layer first — the order they are composited in.
    pub layers: Vec<PlanLayer>,
    /// The text overlays live at this frame, in the order they are drawn (on top).
    pub overlays: Vec<PlanText>,
    /// Clips that are on screen but whose source has no picture to draw.
    pub pictureless: Vec<Pictureless>,
}

/// The matrix the composite is converted with under `policy`, and the reasons a
/// stack cannot be drawn under it.
///
/// [`CompositeColorPolicy::FixedBt601`] is BT.601, always. Under
/// [`CompositeColorPolicy::BottomLayerTag`] it is the matrix every layer shares —
/// FFmpeg 9 takes it from the bottom layer's tag and converts the others into it,
/// which is reproduced only when there is nothing to convert: layers of different
/// matrices are refused, as is a layer whose matrix is unknown, and an RGB picture
/// in a stack that is not BT.601 (the converter that makes it YCbCr takes whatever
/// matrix the neighbours negotiated, and the result depends on the order).
/// [`CompositeColorPolicy::Unknown`] draws only what those two agree on — a stack
/// that is BT.601 throughout — and refuses a stack of any other matrix.
pub(crate) fn composite_matrix(layers: &[PlanLayer], policy: CompositeColorPolicy) -> (YuvMatrix, Vec<Unsupported>) {
    let mut unsupported = Vec::new();
    if policy == CompositeColorPolicy::FixedBt601 {
        return (YuvMatrix::Bt601, unsupported);
    }
    let mut shared: Option<YuvMatrix> = None;
    let mut mixed = false;
    let mut rgb = false;
    for (n, layer) in layers.iter().enumerate() {
        if layer.stream.is_rgb() {
            rgb = true;
            continue;
        }
        match layer.stream.matrix() {
            None => unsupported.push(Unsupported::MatrixUnknown(n)),
            Some(m) => match shared {
                None => shared = Some(m),
                Some(first) if first != m => mixed = true,
                Some(_) => {}
            },
        }
    }
    if mixed {
        unsupported.push(Unsupported::MixedMatrices);
    }
    let matrix = shared.unwrap_or(YuvMatrix::Bt601);
    if policy == CompositeColorPolicy::Unknown && matrix != YuvMatrix::Bt601 {
        unsupported.push(Unsupported::UnmeasuredPolicy(matrix));
    }
    if rgb && matrix != YuvMatrix::Bt601 {
        unsupported.push(Unsupported::RgbInStack(matrix));
    }
    (matrix, unsupported)
}

impl RenderPlan {
    /// The plan for timeline time `t`: what `timeline_frame` / `export_still`
    /// would draw ([`PlanMode::Still`]). Like them it renders the cut as
    /// [`Timeline::for_render`] sees it, so a muted or solo-shadowed track is as
    /// absent here as there.
    ///
    /// Errors the way the still does: a clip whose asset is not in `assets`. It
    /// builds a [`Planner`] for the one call; a caller that plans many frames of the
    /// same cut holds one.
    pub fn at(
        timeline: &Timeline,
        assets: &[Asset],
        opts: &ExportOptions,
        t: f64,
        color: CompositeColorPolicy,
    ) -> Result<RenderPlan> {
        Planner::new(
            timeline,
            assets,
            opts,
            PlanRequest {
                mode: PlanMode::Still,
                color,
            },
        )?
        .at(t)
    }

    /// Why a compositor with `caps` cannot draw this frame at `size`
    /// ([`RenderPlan::size`]) exactly — empty when it can. A **pure function of the
    /// plan's fields**: nothing was decided while planning, so the same plan answers
    /// for every [`GpuCaps`], and a refusal is a value ([`Unsupported`]) whose
    /// `Display` is the message. Ask it before decoding anything.
    pub fn reasons(&self, caps: &GpuCaps, size: (u32, u32)) -> Vec<Unsupported> {
        let mut reasons = self.size_free_reasons(caps);
        for (n, layer) in self.layers.iter().enumerate() {
            let geom = match self.layer_geometry(layer, size) {
                Ok(g) => g,
                Err(e) => {
                    reasons.push(Unsupported::Geometry(n, e.to_string()));
                    continue;
                }
            };
            // FFmpeg scales a picture in the format it has; the compositor scales the
            // 8-bit 4:2:0 a decode reduces it to, which is the same picture only when
            // that is what it already was (or gray, which has no chroma). For anything
            // else the chroma is interpolated from different samples — FFmpeg's real
            // ones, the compositor's already averaged away: a 2x enlargement of 4:4:4
            // is 27 levels off, of RGB 69, and a shrink as mild as 1.05-1.5x still
            // reads flat max 8-9 (4:2:2 at 0.9x is over the limit) and 32 levels on
            // edges. No band of ratios was measured strictly inside the limits on both
            // FFmpegs for busy chroma, so none is claimed: any resize is FFmpeg's. An
            // unrecorded format is not known to be 4:2:0.
            if let Some(stage) = geom
                .stages
                .iter()
                .find(|s| s.src.w > MAX_SHRINK * s.scaled.0 || s.src.h > MAX_SHRINK * s.scaled.1)
            {
                reasons.push(Unsupported::Shrink(n, (stage.src.w, stage.src.h), stage.scaled));
            }
            let resizing = geom.stages.iter().find(|s| s.scaled != (s.src.w, s.src.h));
            if let Some(stage) = resizing.filter(|_| !matches!(layer.stream.layout(), Some(PixLayout::Yuv420 | PixLayout::Gray)))
            {
                reasons.push(Unsupported::FormatResize(
                    n,
                    (stage.src.w, stage.src.h),
                    stage.scaled,
                    layer.stream.pix_fmt.clone().unwrap_or_else(|| "not recorded".to_string()),
                ));
            }
        }
        reasons
    }

    /// The reasons that do not depend on the render size: what the plan holds,
    /// judged against `caps`. [`RenderPlan::reasons`] adds the geometry's.
    pub fn size_free_reasons(&self, caps: &GpuCaps) -> Vec<Unsupported> {
        let mut out: Vec<Unsupported> = Vec::new();
        let (frame, fps) = (self.frame, self.canvas.fps);
        if self.mode == PlanMode::Motion {
            if !caps.motion {
                out.push(Unsupported::Motion);
            }
            if self.canvas.pix_fmt != "yuv420p" {
                out.push(Unsupported::Delivery(self.canvas.pix_fmt.clone()));
            }
            if self.canvas.gif {
                out.push(Unsupported::Gif);
            }
        }
        out.extend(self.pictureless.iter().cloned().map(Unsupported::NoPicture));
        for (n, layer) in self.layers.iter().enumerate() {
            let at = || LayerRef {
                index: n,
                name: layer.name.clone(),
            };
            let stream = &layer.stream;
            if layer.hdr.is_some() && !caps.hdr {
                out.push(Unsupported::Hdr(at()));
            }
            // Alpha is composited by FFmpeg (a ProRes 4444 title, a VP9 or FFV1
            // clip with transparency, a GIF); the compositor draws opaque 4:2:0
            // and would flatten it onto black without a word. The probed pixel
            // format must be on the allow-list of known-opaque ones — a name a
            // deny-list never thought of (`ayuv`, `vuya`, the `rgb32` aliases) is
            // refused too. `None` (never recorded) is the decoder's to find out.
            if let Some(fmt) = stream.pix_fmt.as_deref() {
                if crate::model::pix_fmt_has_alpha(fmt) {
                    out.push(Unsupported::AlphaPicture(at(), fmt.to_string()));
                } else if pix_fmt_layout(fmt).is_none() {
                    out.push(Unsupported::NotKnownOpaque(at(), fmt.to_string()));
                }
            }
            // Below full opacity FFmpeg takes the layer through RGB and back (see
            // `kerf-gpu`'s `roundtrip`), out of YCbCr with the picture's own matrix,
            // which has to be known for the arithmetic to be reproduced. A keyframed
            // clip's opacity is a `geq` alpha instead, refused below.
            let keyed = self.mode == PlanMode::Motion && layer.animated.is_some();
            if layer.transform.opacity < 1.0 && stream.matrix().is_none() && !keyed {
                let detail = match (stream.pix_fmt.as_deref(), stream.color_space.as_deref()) {
                    (None, _) => "unknown (probed before the pixel format and tags were recorded)".to_string(),
                    (Some(_), Some(tag)) => format!("`{tag}`, which the compositor has no coefficients for"),
                    (Some(_), None) => "unknown".to_string(),
                };
                out.push(Unsupported::TranslucentMatrix(at(), detail));
            }
            // Colour correction (`eq`) runs on the picture as it is in the graph.
            // FFmpeg 9 hands it a full-range picture's raw values and converts the
            // range later; the compositor converts to limited range first, as the
            // decode does. The results differ (34 to 42 dB on a graded `yuvj420p`
            // clip, in every knob), so a full-range picture is not graded on the GPU
            // — and a picture whose format was never recorded is not known not to be
            // full-range.
            if !layer.color.is_identity() && stream.maybe_full_range() {
                let detail = match stream.pix_fmt.as_deref() {
                    Some(f) => format!("a full-range picture ({f}), which FFmpeg grades before converting its range"),
                    None => "a picture whose pixel format was never recorded (it may be full range)".to_string(),
                };
                out.push(Unsupported::GradedFullRange(at(), detail));
            }
            if layer.effects.iter().any(|e| !caps.effects.contains(EffectKinds::of(e))) {
                out.push(Unsupported::Effects(at()));
            }
            if layer.mask.is_some() && !caps.mask {
                out.push(Unsupported::Mask(at()));
            }
            if (layer.reframe.is_some() || layer.projection.is_some()) && !caps.reframe {
                out.push(Unsupported::Reframe(at()));
            }
            // The still ignores fades and transitions (it shows the frame each
            // visible clip contributes); the export and the streamed playback do
            // not. A frame inside one is a frame the compositor cannot draw like
            // either until it draws fades, so it is FFmpeg's.
            if let Some((edge, dissolve)) = layer.fx.in_progress(frame, fps).filter(|_| !caps.fades) {
                out.push(Unsupported::Fade(at(), edge, dissolve));
            }
            if layer.fx.travels() && !caps.transitions {
                out.push(Unsupported::Travel(at()));
            }
            if layer.fx.tail && !caps.transitions {
                out.push(Unsupported::Tail(at()));
            }
            if keyed && layer.animated.is_some_and(|a| a.opacity) && !caps.keyed_opacity {
                out.push(Unsupported::KeyedOpacity(at()));
            }
            if keyed && layer.animated.is_some_and(|a| a.zooms) && !caps.keyed_zoom {
                out.push(Unsupported::KeyedZoom(at()));
            }
        }
        if !self.overlays.is_empty() {
            if !caps.text {
                out.push(Unsupported::Text);
            } else if self.overlays.iter().any(|o| o.font_file.is_none()) {
                out.push(Unsupported::TextWithoutFont);
            }
        }
        // The compositor implements swscale's default (bicubic) only.
        if let Some(s) = self.canvas.scaler.as_deref().filter(|s| !s.eq_ignore_ascii_case("bicubic")) {
            out.push(Unsupported::Scaler(s.to_string()));
        }
        let policy = self.canvas.policy;
        out.extend(composite_matrix(&self.layers, policy).1);
        // FFmpeg 9 negotiates the *range* along the overlay chain as it does the matrix:
        // the bottom layer's decides, and a graded layer above a full-range picture is
        // 40 to 44 dB off (a limited-range clip over a limited-range one is exact). Where
        // the composite's colour is negotiated, or not known, grading in a stack that
        // holds a full-range picture is FFmpeg's.
        if policy != CompositeColorPolicy::FixedBt601 && self.layers.iter().any(|l| l.stream.maybe_full_range()) {
            for (n, layer) in self.layers.iter().enumerate() {
                if !layer.color.is_identity() && !layer.stream.maybe_full_range() {
                    out.push(Unsupported::GradedInFullRangeStack(n));
                }
            }
        }
        out
    }

    /// Whether the A0 compositor draws this frame exactly, **judged without the render
    /// size**. `false` means "render it through FFmpeg"; [`RenderPlan::unsupported_reasons`]
    /// says why. A `true` is necessary, not sufficient: what depends on the size the
    /// frame is rendered at (a resize of a picture whose chroma is not 4:2:0, a
    /// translucent layer of odd size, a crop that leaves nothing) is
    /// [`RenderPlan::gpu_supported_at`]'s. A compositor with other abilities asks
    /// [`RenderPlan::reasons`] with its own [`GpuCaps`].
    pub fn gpu_supported(&self) -> bool {
        self.size_free_reasons(&GpuCaps::A0).is_empty()
    }

    /// Why the A0 compositor cannot draw the frame at `size`, as messages.
    pub fn unsupported_reasons_at(&self, size: (u32, u32)) -> Vec<String> {
        self.reasons(&GpuCaps::A0, size).iter().map(ToString::to_string).collect()
    }

    /// Where a layer's picture lands when the frame is rendered at `size`, worked out
    /// the way FFmpeg's `crop` / `scale` / `pad` / `overlay` work it out.
    ///
    /// The first `crop` rounds to the picture's **native** chroma grid
    /// ([`PlanStream::subsampling`]), and so does the Cover crop when the transform's
    /// own `scale` follows it; the chain converts to 4:2:0 in its last `scale`, so
    /// `pad` and the Cover crop of a single-`scale` chain are on the 4:2:0 grid. The
    /// compositor's planes are 4:2:0, so a layer whose chroma is finer than that can
    /// be placed exactly only where the two grids agree: luma is always
    /// exact, but a crop window that starts or ends between two 4:2:0 chroma samples
    /// of a 4:2:2, 4:4:4 or RGB picture puts its chroma a pixel off (measured 30 to
    /// 38 dB over the whole frame) — refused, unless the picture is gray and has no
    /// chroma to misplace. A picture whose grid is not known is accepted only where
    /// every grid gives the same geometry ([`LayerGeometry::resolve_any_grid`]).
    pub fn layer_geometry(
        &self,
        layer: &PlanLayer,
        size: (u32, u32),
    ) -> std::result::Result<LayerGeometry, crate::layer_geometry::GeometryError> {
        let src = (layer.stream.width, layer.stream.height);
        let (fit, tf) = (self.canvas.fit, &layer.transform);
        let place = self.placement(layer);
        let Some(native) = layer.stream.subsampling() else {
            return LayerGeometry::resolve_any_grid_with(src, size, fit, tf, place);
        };
        let geom = LayerGeometry::resolve_with(src, size, fit, tf, native, place)?;
        if native != Subsampling::YUV420
            && layer.stream.layout() != Some(PixLayout::Gray)
            && geom != LayerGeometry::resolve_with(src, size, fit, tf, Subsampling::YUV420, place)?
        {
            return Err(crate::layer_geometry::GeometryError(format!(
                "its crop window or Cover offset starts or ends between two 4:2:0 chroma samples of a picture whose chroma is finer ({}), which the compositor's 4:2:0 planes cannot place",
                layer.stream.pix_fmt.as_deref().unwrap_or("?")
            )));
        }
        Ok(geom)
    }

    /// Whether the A0 compositor draws this frame exactly when rendered at `size`.
    pub fn gpu_supported_at(&self, size: (u32, u32)) -> bool {
        self.reasons(&GpuCaps::A0, size).is_empty()
    }

    /// Why the frame cannot be drawn by the A0 compositor — empty when it can (the
    /// size-free reasons, as messages).
    pub fn unsupported_reasons(&self) -> Vec<String> {
        self.size_free_reasons(&GpuCaps::A0).iter().map(ToString::to_string).collect()
    }

    /// How the graph places this layer beyond its sampled transform ([`Placement`]):
    /// the keyframed clip of an export frame, and whatever a transition moves it by.
    /// A still's layer is placed by its sampled transform alone.
    pub fn placement(&self, layer: &PlanLayer) -> Placement {
        Placement {
            keyframed: layer.animated.filter(|_| self.mode == PlanMode::Motion).map(|a| a.rotates),
            offset: layer.fx.motion,
        }
    }

    /// The size a render of this plan at `max_width` comes out — what the FFmpeg
    /// still of the same width is ([`still_size`]).
    pub fn size(&self, max_width: u32) -> (u32, u32) {
        still_size(self.canvas.width, self.canvas.height, max_width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::model::{Delivery, Keyframe, Mask, MaskShape, Reframe, Track, Transition, TransitionKind, VideoEffect};
    use chrono::Utc;

    fn video(w: u32, h: u32) -> StreamInfo {
        StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(w),
            height: Some(h),
            fps: Some(30.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: Some("yuv420p".into()),
            color_space: None,
        }
    }

    fn asset(name: &str, duration: f64, streams: Vec<StreamInfo>) -> Asset {
        Asset {
            id: Uuid::new_v4(),
            path: format!("/media/{name}.mp4"),
            name: name.into(),
            duration,
            streams,
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
        }
    }

    fn timeline(tracks: Vec<Vec<Clip>>) -> Timeline {
        let mut tl = Timeline::new();
        tl.tracks = tracks
            .into_iter()
            .map(|clips| {
                let mut t = Track::new(StreamKind::Video, "V");
                t.clips = clips;
                t
            })
            .collect();
        tl
    }

    fn plan(tl: &Timeline, assets: &[Asset], t: f64) -> RenderPlan {
        RenderPlan::at(tl, assets, &ExportOptions::default(), t, CompositeColorPolicy::FixedBt601).unwrap()
    }

    #[test]
    fn a_gap_is_a_black_frame_the_gpu_can_draw() {
        let a = asset("a", 10.0, vec![video(1280, 720)]);
        let tl = timeline(vec![vec![Clip::new(a.id, 0.0, 4.0, 2.0)]]);
        let p = plan(&tl, std::slice::from_ref(&a), 1.0);
        assert!(p.layers.is_empty());
        assert!(p.gpu_supported(), "{:?}", p.unsupported_reasons());
        // The canvas follows the footage when no project frame is set.
        assert_eq!((p.canvas.width, p.canvas.height), (1280, 720));
        // ...and the clip appears at its start but not at its end (half-open).
        assert_eq!(plan(&tl, std::slice::from_ref(&a), 2.0).layers.len(), 1);
        assert!(plan(&tl, std::slice::from_ref(&a), 6.0).layers.is_empty());
    }

    #[test]
    fn layers_come_bottom_track_first_and_in_timeline_order() {
        let a = asset("a", 20.0, vec![video(640, 360)]);
        let b = asset("b", 20.0, vec![video(640, 360)]);
        let tl = timeline(vec![
            vec![Clip::new(a.id, 0.0, 10.0, 0.0)],
            vec![Clip::new(b.id, 0.0, 10.0, 0.0), Clip::new(a.id, 0.0, 2.0, 20.0)],
        ]);
        let p = plan(&tl, &[a.clone(), b.clone()], 1.0);
        assert_eq!(p.layers.len(), 2);
        assert_eq!((p.layers[0].asset_id, p.layers[0].track), (a.id, 0));
        assert_eq!(
            (p.layers[1].asset_id, p.layers[1].track),
            (b.id, 1),
            "later tracks draw on top"
        );
    }

    #[test]
    fn source_time_honors_in_point_speed_reverse_and_the_asset_length() {
        let a = asset("a", 12.0, vec![video(640, 360)]);
        // 5..15 at 2x: the clip lasts 5s; at local 1.5s the source is at 5 + 3.
        let mut fast = Clip::new(a.id, 5.0, 15.0, 10.0);
        fast.speed = 2.0;
        let tl = timeline(vec![vec![fast]]);
        let p = plan(&tl, std::slice::from_ref(&a), 11.5);
        assert!((p.layers[0].source_time - 8.0).abs() < 1e-9);
        assert!((p.layers[0].clip_time - 1.5).abs() < 1e-9);
        // Reversed: counts down from source_out.
        let mut rev = Clip::new(a.id, 2.0, 8.0, 0.0);
        rev.speed = -1.0;
        let tl = timeline(vec![vec![rev]]);
        assert!((plan(&tl, std::slice::from_ref(&a), 1.0).layers[0].source_time - 7.0).abs() < 1e-9);
        // A clip whose out-point is past the asset's end clamps rather than
        // asking for a frame that does not exist.
        let long = Clip::new(a.id, 10.0, 30.0, 0.0);
        let tl = timeline(vec![vec![long]]);
        assert!((plan(&tl, std::slice::from_ref(&a), 15.0).layers[0].source_time - 12.0).abs() < 1e-9);
    }

    #[test]
    fn a_still_image_is_flagged_and_never_seeks_past_its_length() {
        let mut s = video(800, 600);
        s.image = true;
        let img = asset("pic", 5.0, vec![s]);
        let tl = timeline(vec![vec![Clip::new(img.id, 0.0, 5.0, 0.0)]]);
        let p = plan(&tl, std::slice::from_ref(&img), 3.0);
        assert!(p.layers[0].is_image);
        assert!(p.gpu_supported());
    }

    #[test]
    fn transform_is_sampled_from_keyframes_at_clip_time() {
        let a = asset("a", 20.0, vec![video(640, 360)]);
        let mut clip = Clip::new(a.id, 0.0, 10.0, 4.0);
        clip.keyframes = vec![
            Keyframe {
                time: 0.0,
                scale: 1.0,
                pos_x: 0.0,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
            },
            Keyframe {
                time: 2.0,
                scale: 0.5,
                pos_x: 0.2,
                pos_y: 0.0,
                rotation: 10.0,
                opacity: 0.0,
            },
        ];
        let tl = timeline(vec![vec![clip]]);
        // Clip-local 1.0s, halfway: the sampled pose is the midpoint.
        let t = plan(&tl, std::slice::from_ref(&a), 5.0).layers[0].transform;
        assert!((t.scale - 0.75).abs() < 1e-9 && (t.pos_x - 0.1).abs() < 1e-9);
        assert!((t.rotation - 5.0).abs() < 1e-9 && (t.opacity - 0.5).abs() < 1e-9);
    }

    #[test]
    fn muted_solo_shadowed_and_disabled_never_reach_the_plan() {
        let a = asset("a", 20.0, vec![video(640, 360)]);
        let b = asset("b", 20.0, vec![video(640, 360)]);
        let mut tl = timeline(vec![
            vec![Clip::new(a.id, 0.0, 10.0, 0.0)],
            vec![Clip::new(b.id, 0.0, 10.0, 0.0)],
        ]);
        let assets = [a.clone(), b.clone()];
        assert_eq!(plan(&tl, &assets, 1.0).layers.len(), 2);
        // Muting V2 hides it.
        tl.tracks[1].muted = true;
        let p = plan(&tl, &assets, 1.0);
        assert_eq!(p.layers.len(), 1);
        assert_eq!(p.layers[0].asset_id, a.id);
        // Soloing V2 (while muted is off) shadows V1.
        tl.tracks[1].muted = false;
        tl.tracks[1].solo = true;
        let p = plan(&tl, &assets, 1.0);
        assert_eq!(p.layers.len(), 1);
        assert_eq!(p.layers[0].asset_id, b.id);
        // A disabled clip is dropped like a muted track.
        tl.tracks[1].solo = false;
        tl.tracks[0].clips[0].enabled = false;
        let p = plan(&tl, &assets, 1.0);
        assert_eq!(p.layers.len(), 1);
        assert_eq!(p.layers[0].asset_id, b.id);
    }

    #[test]
    fn the_canvas_is_the_project_frame_and_its_fit() {
        let a = asset("a", 10.0, vec![video(1920, 1080)]);
        let mut tl = timeline(vec![vec![Clip::new(a.id, 0.0, 10.0, 0.0)]]);
        tl.format = Some(Delivery::new(1080, 1920, Fit::Cover));
        let p = plan(&tl, std::slice::from_ref(&a), 1.0);
        assert_eq!((p.canvas.width, p.canvas.height, p.canvas.fit), (1080, 1920, Fit::Cover));
        // An explicit resolution still wins over the project frame, like export.
        let opts = ExportOptions {
            resolution: Some((720, 1280)),
            ..ExportOptions::default()
        };
        let p = RenderPlan::at(&tl, std::slice::from_ref(&a), &opts, 1.0, CompositeColorPolicy::FixedBt601).unwrap();
        assert_eq!((p.canvas.width, p.canvas.height), (720, 1280));
    }

    #[test]
    fn still_size_caps_to_the_width_and_stays_even() {
        assert_eq!(still_size(1920, 1080, 640), (640, 360));
        assert_eq!(still_size(1920, 1080, u32::MAX), (1920, 1080));
        assert_eq!(still_size(1080, 1920, 541), (540, 960));
        // Never below 2x2 (a zero-sized canvas is not a graph).
        assert_eq!(still_size(1920, 1080, 0), (2, 2));
    }

    #[test]
    fn an_unknown_asset_is_an_error_like_the_still() {
        let tl = timeline(vec![vec![Clip::new(Uuid::new_v4(), 0.0, 1.0, 0.0)]]);
        assert!(matches!(
            RenderPlan::at(&tl, &[], &ExportOptions::default(), 0.5, CompositeColorPolicy::FixedBt601),
            Err(Error::AssetNotFound(_))
        ));
    }

    // ---- gpu_supported ------------------------------------------------------

    /// An edit to the timeline and its asset, applied before the plan is built.
    type Edit = Box<dyn FnOnce(&mut Timeline, &mut Asset)>;

    fn supported_with(edit: impl FnOnce(&mut Timeline, &mut Asset)) -> (bool, Vec<String>) {
        let mut a = asset("a", 20.0, vec![video(640, 360)]);
        let mut tl = timeline(vec![vec![Clip::new(a.id, 0.0, 10.0, 0.0)]]);
        edit(&mut tl, &mut a);
        let p = plan(&tl, std::slice::from_ref(&a), 5.0);
        (p.gpu_supported(), p.unsupported_reasons())
    }

    #[test]
    fn a_plain_frame_is_supported() {
        let (ok, why) = supported_with(|_, _| {});
        assert!(ok, "{why:?}");
    }

    #[test]
    fn everything_the_compositor_does_not_draw_is_refused_with_a_reason() {
        let cases: Vec<(&str, Edit)> = vec![
            (
                "effects",
                Box::new(|tl, _| {
                    tl.tracks[0].clips[0].effects.push(VideoEffect::Blur { sigma: 2.0 });
                }),
            ),
            (
                "mask",
                Box::new(|tl, _| {
                    tl.tracks[0].clips[0].mask = Some(Mask {
                        shape: MaskShape::Rect,
                        x: 0.5,
                        y: 0.5,
                        width: 0.5,
                        height: 0.5,
                        feather: 0.0,
                        inverted: false,
                    });
                }),
            ),
            (
                "reframe",
                Box::new(|tl, _| {
                    tl.tracks[0].clips[0].reframe = Some(Reframe::new(crate::model::Projection::Equirect));
                }),
            ),
            (
                "HDR",
                Box::new(|_, a| a.streams[0].color_transfer = Some("arib-std-b67".into())),
            ),
            ("fade-in", Box::new(|tl, _| tl.tracks[0].clips[0].fade_in = 6.0)),
            ("fade-out", Box::new(|tl, _| tl.tracks[0].clips[0].fade_out = 6.0)),
            (
                "transition",
                Box::new(|tl, _| {
                    tl.tracks[0].clips[0].transition_in = Some(Transition {
                        kind: TransitionKind::Crossfade,
                        duration: 8.0,
                    });
                }),
            ),
            (
                "text overlay",
                Box::new(|tl, _| {
                    tl.overlays.push(crate::model::TextOverlay::new("hi", 0.0, 10.0));
                }),
            ),
            ("no picture", Box::new(|_, a| a.streams[0].width = None)),
            (
                "alpha",
                Box::new(|_, a| {
                    a.streams[0].pix_fmt = Some("yuva420p".into());
                }),
            ),
        ];
        for (name, edit) in cases {
            let (ok, why) = supported_with(edit);
            assert!(!ok && !why.is_empty(), "{name} must not be drawn on the GPU");
        }
    }

    #[test]
    fn only_a_pixel_format_that_says_alpha_is_refused() {
        for (fmt, refused) in [
            ("yuv420p", false),
            ("yuv420p10le", false),
            ("yuvj420p", false),
            ("yuv444p", false),
            ("gray", false),
            ("rgb24", false),
            ("yuva420p", true),
            ("yuva444p10le", true),
            ("gbrap", true),
            ("rgba", true),
            ("bgra", true),
            ("argb", true),
            ("ya8", true),
            ("pal8", true),
        ] {
            let (ok, why) = supported_with(|_, a| a.streams[0].pix_fmt = Some(fmt.into()));
            assert_eq!(!ok, refused, "{fmt}: {why:?}");
        }
        // An asset saved before the pixel format was recorded is not refused
        // here — the decoder looks at the pixels.
        let (ok, why) = supported_with(|_, a| a.streams[0].pix_fmt = None);
        assert!(ok, "{why:?}");
    }

    #[test]
    fn opacity_below_one_needs_a_matrix_the_compositor_has() {
        // The round trip through RGB uses the stream's own matrix, so a tag the
        // compositor has no coefficients for refuses *only* a translucent layer.
        for (tag, refused) in [
            (None, false),
            (Some("bt709"), false),
            (Some("smpte170m"), false),
            (Some("bt470bg"), false),
            (Some("bt2020nc"), false),
            (Some("bt2020c"), true),
            (Some("smpte240m"), true),
            (Some("fcc"), true),
            (Some("ycgco"), true),
        ] {
            let (ok, why) = supported_with(|tl, a| {
                a.streams[0].color_space = tag.map(Into::into);
                tl.tracks[0].clips[0].keyframes = vec![];
                tl.tracks[0].clips[0].transform.opacity = 0.5;
            });
            assert_eq!(!ok, refused, "{tag:?}: {why:?}");
            let (ok, why) = supported_with(|_, a| a.streams[0].color_space = tag.map(Into::into));
            assert!(ok, "opaque layers do not care about the tag ({tag:?}): {why:?}");
        }
    }

    /// A plan over one clip per `(pix_fmt, tag)`, bottom first.
    fn stack(policy: CompositeColorPolicy, layers: &[(&str, Option<&str>)]) -> RenderPlan {
        let assets: Vec<Asset> = layers
            .iter()
            .map(|(fmt, tag)| {
                let mut v = video(640, 360);
                v.pix_fmt = Some((*fmt).into());
                v.color_space = tag.map(Into::into);
                asset("a", 20.0, vec![v])
            })
            .collect();
        let tl = timeline(assets.iter().map(|a| vec![Clip::new(a.id, 0.0, 10.0, 0.0)]).collect());
        RenderPlan::at(&tl, &assets, &ExportOptions::default(), 1.0, policy).unwrap()
    }

    #[test]
    fn the_fixed_policy_converts_every_composite_as_bt601() {
        for tag in [None, Some("bt709"), Some("bt2020nc")] {
            let p = stack(
                CompositeColorPolicy::FixedBt601,
                &[("yuv420p", tag), ("yuv420p", Some("bt709"))],
            );
            assert_eq!(p.canvas.matrix, YuvMatrix::Bt601);
            assert!(p.gpu_supported(), "{:?}", p.unsupported_reasons());
        }
    }

    #[test]
    fn under_negotiation_the_composite_follows_the_layers_tag() {
        use CompositeColorPolicy::BottomLayerTag as B;
        for (tag, want) in [
            (None, YuvMatrix::Bt601),
            (Some("smpte170m"), YuvMatrix::Bt601),
            (Some("bt470bg"), YuvMatrix::Bt601),
            (Some("bt709"), YuvMatrix::Bt709),
            (Some("bt2020nc"), YuvMatrix::Bt2020),
        ] {
            let p = stack(B, &[("yuv420p", tag)]);
            assert_eq!(
                (p.canvas.matrix, p.gpu_supported()),
                (want, true),
                "{tag:?}: {:?}",
                p.unsupported_reasons()
            );
            // ...for a whole stack that agrees, whatever the layers' formats.
            let p = stack(B, &[("yuv420p", tag), ("yuv444p", tag), ("yuv420p10le", tag), ("gray", tag)]);
            assert_eq!(
                (p.canvas.matrix, p.gpu_supported()),
                (want, true),
                "{tag:?}: {:?}",
                p.unsupported_reasons()
            );
        }
        // No layers: an untagged canvas.
        let p = stack(B, &[]);
        assert_eq!(p.canvas.matrix, YuvMatrix::Bt601);
        assert!(p.gpu_supported());
    }

    #[test]
    fn under_negotiation_layers_that_disagree_are_refused_not_converted_wrong() {
        use CompositeColorPolicy::BottomLayerTag as B;
        // FFmpeg 9 converts the top into the bottom's matrix with arithmetic the
        // compositor does not reproduce — in either order, and for 601 vs untagged
        // there is nothing to convert.
        for layers in [
            [("yuv420p", Some("bt709")), ("yuv420p", None)],
            [("yuv420p", None), ("yuv420p", Some("bt709"))],
            [("yuv420p", Some("bt709")), ("yuv420p", Some("bt2020nc"))],
        ] {
            let p = stack(B, &layers);
            assert!(!p.gpu_supported(), "{layers:?}");
            assert!(p.unsupported_reasons().iter().any(|r| r.contains("different YCbCr matrices")));
        }
        let p = stack(B, &[("yuv420p", None), ("yuv420p", Some("smpte170m"))]);
        assert!(p.gpu_supported(), "{:?}", p.unsupported_reasons());
        // A matrix with no coefficients here, and an asset that never recorded its tags.
        let p = stack(B, &[("yuv420p", Some("smpte240m"))]);
        assert!(p.unsupported_reasons().iter().any(|r| r.contains("colour matrix is unknown")));
        // RGB pictures take the stack's matrix in FFmpeg's converter: only a BT.601
        // stack is one the compositor can reproduce.
        let p = stack(B, &[("rgb24", None), ("yuv420p", None)]);
        assert!(p.gpu_supported(), "{:?}", p.unsupported_reasons());
        let p = stack(B, &[("yuv420p", Some("bt709")), ("rgb24", None)]);
        assert!(p.unsupported_reasons().iter().any(|r| r.contains("RGB picture")));
        let p = stack(B, &[("rgb24", None), ("yuv420p", Some("bt709"))]);
        assert!(p.unsupported_reasons().iter().any(|r| r.contains("RGB picture")));
    }

    #[test]
    fn a_policy_that_could_not_be_measured_draws_only_what_every_ffmpeg_agrees_on() {
        use CompositeColorPolicy::Unknown as U;
        // FFmpeg 6 and 9 both convert a BT.601-class stack as BT.601: drawn.
        for tag in [None, Some("smpte170m"), Some("bt470bg")] {
            let p = stack(U, &[("yuv420p", tag), ("yuv444p", tag), ("rgb24", None), ("gray", tag)]);
            assert_eq!(
                (p.canvas.matrix, p.gpu_supported()),
                (YuvMatrix::Bt601, true),
                "{tag:?}: {:?}",
                p.unsupported_reasons()
            );
        }
        // BT.709 and BT.2020 are what the two disagree on, so no guess is safe: a
        // single such layer sends the frame to FFmpeg, whichever FFmpeg it is.
        for tag in ["bt709", "bt2020nc"] {
            let p = stack(U, &[("yuv420p", Some(tag))]);
            assert!(!p.gpu_supported(), "{tag}");
            assert!(
                p.unsupported_reasons().iter().any(|r| r.contains("could not be measured")),
                "{tag}: {:?}",
                p.unsupported_reasons()
            );
            let p = stack(U, &[("yuv420p", None), ("yuv420p", Some(tag))]);
            assert!(!p.gpu_supported(), "{tag} over untagged");
            let p = stack(U, &[("yuv420p", Some(tag)), ("yuv420p", None)]);
            assert!(!p.gpu_supported(), "{tag} under untagged");
        }
        // The same stacks that are drawn under the policy that is measured.
        let p = stack(CompositeColorPolicy::FixedBt601, &[("yuv420p", Some("bt709"))]);
        assert!(p.gpu_supported());
        // An asset that never recorded its tags is not known to be BT.601.
        let (ok, why) = {
            let mut v = video(640, 360);
            v.pix_fmt = None;
            let a = asset("a", 20.0, vec![v]);
            let tl = timeline(vec![vec![Clip::new(a.id, 0.0, 10.0, 0.0)]]);
            let p = RenderPlan::at(&tl, &[a], &ExportOptions::default(), 1.0, U).unwrap();
            (p.gpu_supported(), p.unsupported_reasons())
        };
        assert!(!ok && why.iter().any(|r| r.contains("matrix is unknown")), "{why:?}");
    }

    #[test]
    fn an_asset_that_never_recorded_its_pixel_format_has_an_unknown_matrix() {
        // `color_space: None` means "untagged" for a recorded asset and "not read
        // yet" for an old one: BT.709 footage from an old project at opacity < 1
        // must not be taken for BT.601.
        let (ok, why) = supported_with(|tl, a| {
            a.streams[0].pix_fmt = None;
            tl.tracks[0].clips[0].transform.opacity = 0.5;
        });
        assert!(!ok && why.iter().any(|r| r.contains("probed before")), "{why:?}");
        // An opaque layer does not care, under the fixed policy.
        let (ok, why) = supported_with(|_, a| a.streams[0].pix_fmt = None);
        assert!(ok, "{why:?}");
        // Under negotiation the composite's matrix depends on every tag.
        let mut v = video(640, 360);
        v.pix_fmt = None;
        let a = asset("a", 20.0, vec![v]);
        let tl = timeline(vec![vec![Clip::new(a.id, 0.0, 10.0, 0.0)]]);
        let p = RenderPlan::at(
            &tl,
            &[a],
            &ExportOptions::default(),
            1.0,
            CompositeColorPolicy::BottomLayerTag,
        )
        .unwrap();
        assert!(!p.gpu_supported());
    }

    #[test]
    fn a_pixel_format_off_the_allow_list_is_refused_even_if_it_does_not_look_like_alpha() {
        for fmt in ["ayuv", "vuya", "rgb32", "bgr32", "ayuv64le", "something_new"] {
            let (ok, why) = supported_with(|_, a| a.streams[0].pix_fmt = Some(fmt.into()));
            assert!(!ok, "{fmt}: {why:?}");
        }
    }

    #[test]
    fn what_depends_on_the_render_size_is_decided_with_it() {
        let with_fmt = |fmt: Option<&str>, scale: f64, opacity: f64| {
            let mut v = video(640, 360);
            v.pix_fmt = fmt.map(Into::into);
            let a = asset("a", 20.0, vec![v]);
            let mut c = Clip::new(a.id, 0.0, 10.0, 0.0);
            c.transform.scale = scale;
            c.transform.opacity = opacity;
            let tl = timeline(vec![vec![c]]);
            plan(&tl, std::slice::from_ref(&a), 1.0)
        };
        // Resizing a picture FFmpeg scales in another format is refused — enlarging
        // and shrinking alike, a mild shrink included; 4:2:0 and gray are not, and
        // neither is a picture left at its size, whatever the format.
        for (fmt, scale, refused) in [
            ("yuv420p", 2.0, false),
            ("yuv420p10le", 2.0, false),
            ("gray", 2.0, false),
            ("yuv444p", 1.5, true),
            ("yuv422p", 2.0, true),
            ("bgr0", 2.0, true),
            ("rgb24", 2.0, true),
            ("yuv420p12le", 2.0, true),
            ("yuv444p", 1.0, false),
            ("yuv444p", 0.5, true),
            ("yuv444p", 0.9, true),
            ("yuv422p", 0.9, true),
            ("rgb24", 0.5, true),
            ("yuv420p", 0.5, false),
            ("yuv420p10le", 0.9, false),
            ("gray", 0.5, false),
        ] {
            let p = with_fmt(Some(fmt), scale, 1.0);
            let why = p.unsupported_reasons_at(p.size(u32::MAX));
            assert_eq!(!why.is_empty(), refused, "{fmt} x{scale}: {why:?}");
        }
        // A format that was never recorded is not known to be 4:2:0, in either direction.
        for scale in [2.0, 0.5] {
            let p = with_fmt(None, scale, 1.0);
            assert!(!p.gpu_supported_at(p.size(u32::MAX)), "x{scale}");
        }
        let p = with_fmt(None, 1.0, 1.0);
        assert!(p.gpu_supported_at(p.size(u32::MAX)));
        // The size decides: the same 640x360 source into a 1920x1080 frame is a
        // resize at full size and a plain copy at preview width 640.
        let a = {
            let mut v = video(640, 360);
            v.pix_fmt = Some("yuv444p".into());
            asset("a", 20.0, vec![v])
        };
        let tl = {
            let mut t = timeline(vec![vec![Clip::new(a.id, 0.0, 10.0, 0.0)]]);
            t.format = Some(crate::model::Delivery::new(1920, 1080, Fit::Contain));
            t
        };
        let p = plan(&tl, std::slice::from_ref(&a), 1.0);
        assert!(!p.gpu_supported_at(p.size(u32::MAX)));
        assert!(p.gpu_supported_at(p.size(640)));
        // A shrink steeper than the scaler comparison covers is FFmpeg's: 640 px to
        // 13 is past 40:1, 640 px to 19 is not.
        let p = with_fmt(Some("yuv420p"), 0.02, 1.0);
        let why = p.unsupported_reasons_at(p.size(u32::MAX));
        assert!(why.iter().any(|r| r.contains("steeper than the 40:1")), "{why:?}");
        let p = with_fmt(Some("yuv420p"), 0.03, 1.0);
        assert!(p.gpu_supported_at(p.size(u32::MAX)));
        // A translucent layer of odd size, at the size it is rendered.
        let p = with_fmt(Some("yuv420p"), 0.33, 0.5);
        let why = p.unsupported_reasons_at(p.size(u32::MAX));
        assert!(why.iter().any(|r| r.contains("odd size")), "{why:?}");
        let p = with_fmt(Some("yuv420p"), 0.5, 0.5);
        assert!(p.gpu_supported_at(p.size(u32::MAX)));
    }

    #[test]
    fn colour_correction_on_a_full_range_picture_is_refused() {
        let graded = |fmt: Option<&str>, grade: bool| {
            supported_with(|tl, a| {
                a.streams[0].pix_fmt = fmt.map(Into::into);
                if grade {
                    tl.tracks[0].clips[0].color.gamma = 1.4;
                }
            })
        };
        // A graded `yuvj420p` clip (a JPEG, an mjpeg or full-range H.264 file — ffprobe
        // names all of them so) is FFmpeg's: 34 to 42 dB off on FFmpeg 9.
        let (ok, why) = graded(Some("yuvj420p"), true);
        assert!(!ok && why.iter().any(|r| r.contains("full-range")), "{why:?}");
        // Ungraded, it is drawn (the compositor converts the range as the decode does)...
        let (ok, why) = graded(Some("yuvj420p"), false);
        assert!(ok, "{why:?}");
        // ...and a graded limited-range clip is.
        let (ok, why) = graded(Some("yuv420p"), true);
        assert!(ok, "{why:?}");
        // An asset that never recorded its format is not known not to be full range.
        let (ok, why) = graded(None, true);
        assert!(!ok && why.iter().any(|r| r.contains("never recorded")), "{why:?}");
        let (ok, why) = graded(None, false);
        assert!(ok, "{why:?}");
    }

    #[test]
    fn grading_in_a_stack_with_a_full_range_picture_is_refused_where_range_is_negotiated() {
        let stacked = |policy: CompositeColorPolicy, bottom: &str, top: &str, graded_top: bool| {
            let mut vb = video(640, 360);
            vb.pix_fmt = Some(bottom.into());
            let mut vt = video(640, 360);
            vt.pix_fmt = Some(top.into());
            let (ab, at) = (asset("b", 20.0, vec![vb]), asset("t", 20.0, vec![vt]));
            let mut ct = Clip::new(at.id, 0.0, 10.0, 0.0);
            if graded_top {
                ct.color.gamma = 1.4;
            }
            let tl = timeline(vec![vec![Clip::new(ab.id, 0.0, 10.0, 0.0)], vec![ct]]);
            let p = RenderPlan::at(&tl, &[ab, at], &ExportOptions::default(), 1.0, policy).unwrap();
            p.unsupported_reasons()
        };
        let refused = |why: &[String]| why.iter().any(|r| r.contains("stack with a full-range picture"));
        use CompositeColorPolicy::{BottomLayerTag, FixedBt601, Unknown};
        // FFmpeg 9: a graded limited-range layer above (or below) a full-range one.
        assert!(refused(&stacked(BottomLayerTag, "yuvj420p", "yuv420p", true)));
        assert!(refused(&stacked(Unknown, "yuvj420p", "yuv420p", true)));
        // ...exact where nothing is graded, and where nothing is full range.
        assert!(!refused(&stacked(BottomLayerTag, "yuvj420p", "yuv420p", false)));
        assert!(!refused(&stacked(BottomLayerTag, "yuv420p", "yuv420p", true)));
        // FFmpeg 6 does not negotiate it: measured exact.
        assert!(!refused(&stacked(FixedBt601, "yuvj420p", "yuv420p", true)));
    }

    #[test]
    fn a_crop_between_420_chroma_samples_of_a_finer_picture_is_refused() {
        let planned = |fmt: Option<&str>, crop: (f64, f64)| {
            let mut v = video(640, 360);
            v.pix_fmt = fmt.map(Into::into);
            let a = asset("a", 20.0, vec![v]);
            let mut c = Clip::new(a.id, 0.0, 10.0, 0.0);
            // Windows of 640 x 360 starting at (crop.0, crop.1) pixels; the right
            // and bottom edges are left alone, so the window is as odd as its origin.
            c.transform.crop_left = crop.0 / 640.0;
            c.transform.crop_top = crop.1 / 360.0;
            let mut tl = timeline(vec![vec![c]]);
            tl.format = Some(crate::model::Delivery::new(320, 180, Fit::Contain));
            let p = plan(&tl, std::slice::from_ref(&a), 1.0);
            p.unsupported_reasons_at(p.size(u32::MAX))
        };
        let refused = |why: &[String]| why.iter().any(|r| r.contains("4:2:0 chroma samples"));
        // (format, x odd?, y odd?) -> refused? A picture is placed exactly where
        // FFmpeg's native rounding and the 4:2:0 planes agree.
        for (fmt, odd_x, odd_y, want) in [
            ("yuv420p", true, true, false),
            ("yuv420p10le", true, true, false),
            // 4:2:2 rounds columns to even like 4:2:0, and keeps odd rows.
            ("yuv422p", true, false, false),
            ("yuv422p", false, true, true),
            ("yuv422p", true, true, true),
            ("yuv444p", true, false, true),
            ("yuv444p", false, true, true),
            ("bgr0", true, true, true),
            ("rgb24", false, true, true),
            // Gray has no chroma to misplace: luma is exact on any window.
            ("gray", true, true, false),
            // Nothing odd, nothing to round: drawn whatever the format.
            ("yuv444p", false, false, false),
            ("rgb24", false, false, false),
        ] {
            let why = planned(Some(fmt), (if odd_x { 11.0 } else { 10.0 }, if odd_y { 7.0 } else { 6.0 }));
            assert_eq!(refused(&why), want, "{fmt} odd_x={odd_x} odd_y={odd_y}: {why:?}");
        }
        // A format that was never recorded, or whose rounding was not measured, is
        // refused wherever the grids disagree.
        let why = planned(None, (11.0, 7.0));
        assert!(why.iter().any(|r| r.contains("rounds differently")), "{why:?}");
        // (A format off the table: bit-packed RGB has more to its crop than a grid.)
        let why = planned(Some("rgb565le"), (11.0, 7.0));
        assert!(why.iter().any(|r| r.contains("rounds differently")), "{why:?}");
        // 10-bit 4:4:4 is on it, and is refused as the 8-bit one is.
        let why = planned(Some("yuv444p10le"), (11.0, 7.0));
        assert!(refused(&why), "{why:?}");
        // (The frame is a resize of the picture, which is refused for another reason;
        // what is asserted is that nothing about the crop is.)
        let why = planned(None, (0.0, 0.0));
        assert!(!why.iter().any(|r| r.contains("rounds differently")), "{why:?}");
    }

    #[test]
    fn the_matrix_follows_the_tag_and_defaults_to_bt601_like_swscale() {
        let of = |tag: Option<&str>| {
            let mut s = PlanStream::of(&video(640, 360)).unwrap();
            s.color_space = tag.map(Into::into);
            s.matrix()
        };
        assert_eq!(of(None), Some(YuvMatrix::Bt601));
        assert_eq!(of(Some("bt709")), Some(YuvMatrix::Bt709));
        assert_eq!(of(Some("bt2020nc")), Some(YuvMatrix::Bt2020));
        assert_eq!(of(Some("smpte240m")), None);
    }

    #[test]
    fn a_fade_or_transition_only_matters_while_it_runs() {
        // The fade-in covers 0..1s; at t=5 the clip is at full strength.
        let (ok, why) = supported_with(|tl, _| tl.tracks[0].clips[0].fade_in = 1.0);
        assert!(ok, "{why:?}");
        let (ok, why) = supported_with(|tl, _| {
            tl.tracks[0].clips[0].transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: 1.0,
            });
        });
        assert!(ok, "{why:?}");
    }

    #[test]
    fn only_the_default_scaler_is_drawn() {
        let a = asset("a", 10.0, vec![video(640, 360)]);
        let tl = timeline(vec![vec![Clip::new(a.id, 0.0, 10.0, 0.0)]]);
        let with = |s: &str| ExportOptions {
            scaler: Some(s.into()),
            ..ExportOptions::default()
        };
        let p = RenderPlan::at(
            &tl,
            std::slice::from_ref(&a),
            &with("bicubic"),
            1.0,
            CompositeColorPolicy::FixedBt601,
        )
        .unwrap();
        assert!(p.gpu_supported());
        let p = RenderPlan::at(
            &tl,
            std::slice::from_ref(&a),
            &with("lanczos"),
            1.0,
            CompositeColorPolicy::FixedBt601,
        )
        .unwrap();
        assert!(!p.gpu_supported());
    }
}
