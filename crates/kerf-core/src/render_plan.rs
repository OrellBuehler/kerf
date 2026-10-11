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
//! what `build_still_args` draws at a time `t` (the transform sampled once, text drawn
//! statically) — and, inside a fade, a dip, a dissolve, a slide or a push, the export's own
//! frame as `engine::cli::still_fx` draws it: the fades still changing the picture, the
//! travel, and the outgoing clip on its tail. [`PlanMode::Motion`]
//! is the export graph at output frame `k`: see [`Planner`].

use uuid::Uuid;

use crate::clip_timing::{FadeEdge, FadeStep, FadeTint, Rational, FADE_FULL};
use crate::engine::Fit;
use crate::error::Result;
use crate::frame_pick::Pick;
use crate::layer_geometry::{Keyed, LayerGeometry, Placement};
use crate::model::{
    pix_fmt_is_high_bit, pix_fmt_layout, pix_fmt_subsampling, Asset, Clip, Color, Hdr, Mask, PixLayout, Projection,
    ResolvedReframe, StreamInfo, StreamKind, Subsampling, Timeline, Transform, VideoEffect,
};
use crate::plan_caps::{DipWhy, EffectKinds, GpuCaps, LayerRef, Unsupported};
use crate::planner::{PlanRequest, Planner};
use crate::ExportOptions;

/// A video clip visible at a timeline time, paired with where in its source the
/// frame comes from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ActiveClip<'a> {
    pub clip: &'a Clip,
    /// The clip's index in storage order across all tracks: what `transition_fx` is indexed by.
    pub flat: usize,
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

    /// The clip's colour sampled at this instant (a keyed number is its curve's value).
    pub(crate) fn color(&self) -> crate::model::Color {
        self.clip.color_at(self.local_time)
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
    active_video_clips_with_tails(timeline, assets, t, &[], t)
}

/// [`active_video_clips`] with a clip kept on screen for `tails[flat]` seconds past its end
/// (an outgoing clip playing on under a transition: `ClipFx::tail`), its tail window judged at
/// `tail_at` instead of `t`. The export decides a frame's tail from the frame's own time, which
/// is the start of its slot and so can be before the `t` a still is asked for: a window that
/// closes inside a frame is still open for that frame. A `flat` past the end of `tails` has no
/// tail, so an empty slice is `active_video_clips` itself.
pub(crate) fn active_video_clips_with_tails<'a>(
    timeline: &'a Timeline,
    assets: &[Asset],
    t: f64,
    tails: &[f64],
    tail_at: f64,
) -> Vec<ActiveClip<'a>> {
    let asset_of = |id| assets.iter().find(|a: &&Asset| a.id == id);
    let mut active = Vec::new();
    let mut base = 0;
    for track in &timeline.tracks {
        let first = base;
        base += track.clips.len();
        if track.kind != StreamKind::Video {
            continue;
        }
        let mut order: Vec<usize> = (0..track.clips.len()).collect();
        order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
        for &ci in &order {
            let clip = &track.clips[ci];
            let tail = tails.get(first + ci).copied().unwrap_or(0.0);
            let on =
                t >= clip.timeline_start && (t < clip.timeline_end() || (tail > 0.0 && tail_at < clip.timeline_end() + tail));
            if !on {
                continue;
            }
            let dur = asset_of(clip.asset_id).map(|a| a.duration).unwrap_or(clip.source_out);
            active.push(ActiveClip {
                clip,
                flat: first + ci,
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

/// The smallest font a text pass is held to, in pixels (`drawtext`'s `fontsize`, the frame's height
/// times the overlay's `size`, rounded). `drawtext` blends each glyph's bitmap in turn and a mask
/// holds all of them at once; the two agree to rounding where glyphs do not share a pixel, and
/// small text is all shared pixels: measured against `drawtext`'s own planes, 3 to 11 px text
/// reads up to 7 levels off in chroma at a dozen samples, 12 px and up at most 2
/// (`the_reference_blend_is_drawtexts_own_for_small_text`). A frame with smaller text goes through FFmpeg.
pub const MIN_TEXT_PX: u32 = 12;

/// Which graph a plan describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanMode {
    /// What `build_still_args` draws at a time `t`: the transform sampled at that
    /// instant, text drawn statically, the frame the still's `-ss` lands on, and inside a
    /// transition the export's frame as the still draws it (the fades still changing the
    /// picture counted at the output frame `t` falls in, the slide or push travel at the
    /// graph's time for that frame, an outgoing clip on its tail). The contract the GPU
    /// path started with, and what every `RenderPlan::at` returns.
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
    /// The output rate as the `color=r=` canvas parses the graph's text to: the clock the
    /// overlay (and so every `t` the graph evaluates) runs on.
    pub fps: Rational,
    /// The rate as the `fps=` filter of each clip parses it — the grid its frames are placed
    /// on, which the source-frame pick works from. The same as [`PlanCanvas::fps`] for every
    /// rate with small terms (see [`Rational`]).
    pub pick_fps: Rational,
    /// The delivery's terminal pixel format. A [`PlanMode::Motion`] frame is only
    /// drawn for 8-bit 4:2:0 (`yuv420p`); a preview still is always that.
    pub pix_fmt: String,
    /// The delivery is a gif (palettegen / paletteuse on the composite).
    pub gif: bool,
    /// The cut holds a clip whose picture may be full range ([`PlanStream::maybe_full_range_in`]
    /// under this plan's policy), wherever it is on the timeline: the export's range is one for the
    /// whole stream.
    pub full_range_cut: bool,
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

    /// Whether the picture may be full range **by its name**: ffprobe names a full-range 8-bit
    /// 4:2:0 / 4:2:2 / 4:4:4 picture `yuvj…` (a JPEG, an mjpeg, an H.264 file flagged `pc`), and an
    /// asset that never recorded its format is not known not to be. This is a heuristic: the
    /// range *tag* (`color_range`) is the truth, and [`StreamInfo`] does not record it, so a
    /// deeper-than-8-bit file flagged `pc` still reads `yuv420p10le` and is not caught here; see
    /// [`PlanStream::maybe_full_range_in`].
    pub fn maybe_full_range(&self) -> bool {
        self.pix_fmt
            .as_deref()
            .is_none_or(|f| f.to_ascii_lowercase().starts_with("yuvj"))
    }

    /// [`PlanStream::maybe_full_range`], and under a policy that is not
    /// [`CompositeColorPolicy::FixedBt601`] (FFmpeg 9 and what could not be measured) **every
    /// deeper-than-8-bit YCbCr or gray format too**: the name says nothing about its range there,
    /// and 9.0.2 handles a `pc`-tagged one as it does `yuvj` (raw values kept, the whole export
    /// flagged full range; measured Y 16 off on 10-bit). FFmpeg 6 handled the high-bit pictures
    /// measured exactly whatever their tag, so there the name alone decides, as before.
    pub fn maybe_full_range_in(&self, policy: CompositeColorPolicy) -> bool {
        self.maybe_full_range()
            || (policy != CompositeColorPolicy::FixedBt601 && self.pix_fmt.as_deref().is_some_and(pix_fmt_is_high_bit))
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
///
/// A layer has one when anything it is drawn from is keyed: a transform number
/// ([`Animated::keys`]) or a colour number ([`Animated::color`]) — the clip's volume is not
/// a picture and is not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Animated {
    /// Which transform numbers are keyed; `None` when none is (a clip animated in colour only
    /// is placed as a static one).
    pub keys: Option<Keyed>,
    /// Some key turns the picture (so the graph has a `rotate`, in a `hypot(iw,ih)` box).
    pub rotates: bool,
    /// Some key is below full opacity (so the graph has a `geq` alpha).
    pub opacity: bool,
    /// The keys' scales differ ([`Clip::zoom_animated`]): the zoom moves. The export (and
    /// the playback stream) then runs the clip's `scale eval=frame` **last**, after `fps`
    /// and after every effect, mask, `rotate` and fade, which act on the picture at its fit
    /// size and are magnified with it; the FFmpeg still orders a moving zoom the same way.
    /// A compositor that draws a moving zoom has to draw that order, not "zoom, then the
    /// rest" as it does a constant one, so a moving zoom in an export frame is refused until
    /// it says it does (`GpuCaps::keyed_zoom`), and in a still when something follows it.
    pub zooms: bool,
    /// Some colour number is keyed: the export's `eq` is written per frame (`eval=frame`),
    /// off a colour sampled at the frame's time. A still plan samples it too and draws that
    /// colour like a static one; an export frame is refused until the compositor says it draws
    /// one (`GpuCaps::keyed_color`).
    pub color: bool,
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
            .find(|s| s.factor_at_frame(frame, fps.num, fps.den) < FADE_FULL)
            .map(|s| (s.edge, s.tint == FadeTint::Alpha))
    }

    /// A slide or push is moving the layer.
    pub fn travels(&self) -> bool {
        self.motion != (0.0, 0.0)
    }
}

/// What the layer's clip does in time, beside the [`Pick`] that carries what the source-frame
/// choice needs (`FpsPick`: the export's `fps` filter does not pick "the frame containing
/// `source_time`", see `frame_pick.rs`). The window here is the overlay's `enable` one;
/// the speed and direction repeat what the pick holds, for a reader that wants them without
/// matching on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanTiming {
    /// The overlay's `enable` window on the timeline, tail included. A [`PlanMode::Motion`]
    /// plan holds every layer whose window contains the frame time (end included, as
    /// `between` has it): **candidates**. An equal-rate source has no frame at the
    /// window's end and the export draws nothing there; which layers are really drawn
    /// is the pick's to say ([`PlanLayer::pick`]: `None` is no frame).
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
    /// The font file `drawtext` is given: the family the overlay names if it is installed, else the
    /// shipped default (`fonts::resolve_text_font`, the graph's own resolution). `None` only when
    /// the default could not be written to disk, which leaves `drawtext` FFmpeg's own default font,
    /// which no other renderer can reproduce: never drawn.
    pub font_file: Option<std::path::PathBuf>,
    /// No bold face was found for a bold request, and `drawtext` thickens the glyphs
    /// with a same-colour border instead.
    pub synthetic_bold: bool,
}

/// A layer whose source has no picture of a known size: not drawable under any caps.
pub type Pictureless = LayerRef;

/// Which file a layer decodes: what [`PlanLayer::path`] and [`PlanLayer::stream`] describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PlanSource {
    /// The file is a generated preview proxy rather than the asset's own, so
    /// [`PlanLayer::stream`] is the proxy's picture (see [`crate::MediaResolver`]).
    pub proxy: bool,
}

impl PlanSource {
    /// The asset's own file.
    pub const ORIGINAL: Self = Self { proxy: false };
}

/// One video layer of a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanLayer {
    pub clip_id: Uuid,
    pub asset_id: Uuid,
    /// Index of the timeline track the layer comes from (0 is the bottom one).
    pub track: usize,
    /// The file the layer decodes: the asset's own, or its proxy ([`PlanLayer::source`]).
    pub path: String,
    pub source: PlanSource,
    /// A still image: decoded once, never seeked.
    pub is_image: bool,
    /// The source time to decode (speed / reverse honored, clamped to the asset).
    pub source_time: f64,
    /// Which frame of the file the layer shows: the still's `-ss` pick, or the export's `fps`
    /// pick ([`Pick::Fps`], in a [`PlanMode::Motion`] plan) — a decoder resolves it against the
    /// frames it has. Which candidates at a clip's closing edge are drawn at all is its answer
    /// too: `None` is no frame.
    pub pick: Pick,
    /// Seconds from the clip's start; keyframes were sampled here. A [`PlanMode::Motion`]
    /// plan samples at the graph's own frame time, which is not `frame / fps`.
    pub clip_time: f64,
    /// The picture **the decoded file** carries (`path`): a proxy's size and pixel format, not
    /// the original's.
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
    /// `Some` for a clip with a keyed transform or colour number.
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
    /// Clips that are on screen but whose source has no picture to draw. `index` is the
    /// number of drawn layers below it: the slot in `layers` it would have taken.
    pub pictureless: Vec<Pictureless>,
}

/// One `fade` filter of a layer's chain at this frame: what it fades ([`FadeTint`]), from which
/// side, and the integer it multiplies by ([`FadeStep::factor_at_frame`]). A factor of
/// [`FADE_FULL`] is a fade that is not changing the picture at this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FadeOp {
    pub tint: FadeTint,
    pub edge: FadeEdge,
    pub factor: u32,
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

/// Why a compositor with `caps` does not draw `effect`: it is not one it draws (the detail says
/// what it is, and why that one is hard), or its value is one FFmpeg's own filter refuses (`gblur`
/// takes a sigma of 0 to 1024, `unsharp` an amount of -2 to 5), so the frame goes to FFmpeg, which
/// fails it the same way.
fn effect_refusal(effect: &VideoEffect, caps: &GpuCaps) -> Option<&'static str> {
    if !caps.effects.contains(EffectKinds::of(effect)) {
        return Some(match effect {
            VideoEffect::Blur { .. } => "a blur",
            VideoEffect::Sharpen { .. } => "a sharpen",
            VideoEffect::Grayscale => "grayscale",
            VideoEffect::Invert => "invert",
            VideoEffect::Vignette => "a vignette, whose dither is a counter that runs on from one frame to the next",
            VideoEffect::ChromaKey { .. } => "a chroma key, whose alpha is a float colour distance",
        });
    }
    match effect {
        VideoEffect::Blur { sigma } if !(sigma.is_finite() && (0.0..=1024.0).contains(sigma)) => {
            Some("a blur sigma outside 0 to 1024, which gblur refuses")
        }
        VideoEffect::Sharpen { amount } if !(amount.is_finite() && (-2.0..=5.0).contains(amount)) => {
            Some("a sharpen amount outside -2 to 5, which unsharp refuses")
        }
        _ => None,
    }
}

/// The largest sharpen amount the plan draws on a picture the compositor scaled ahead of it: its error
/// there is the scaler's level times the filter's gain, `1 + 2 * amount` levels of luma, which is two
/// at this amount, the bound of a scaled blur.
const MAX_SHARPEN_AFTER_SCALE: f64 = 0.5;

/// What an effect does to this kind of picture that the compositor, which works on the 4:2:0 limited-range
/// planes the decode hands it, does not: FFmpeg runs the filter on the picture as it is in the graph, and the
/// conversion to 4:2:0 comes after. An invert of a gray picture negates its luma and has no chroma to negate
/// (the compositor's neutral chroma would go to 127); a full-range picture reaches the filter with its raw
/// values and is converted to limited range afterwards (an invert is off by up to 8 levels, a sharpen's
/// overshoot by 26: measured on 9.0.2), and a picture whose format was never recorded may be either, as may
/// a deeper-than-8-bit one where FFmpeg 9 keeps a `pc` tag's raw values ([`PlanStream::maybe_full_range_in`]).
/// Grayscale (chroma to 128, luma untouched) is the same on all of them, and so are the other formats the
/// decode converts (4:2:2, 4:4:4, RGB; 10-bit under `FixedBt601`), to the decode's own rounding.
fn effect_on_picture_refusal(effect: &VideoEffect, stream: &PlanStream, policy: CompositeColorPolicy) -> Option<&'static str> {
    match effect {
        VideoEffect::Invert | VideoEffect::Blur { .. } | VideoEffect::Sharpen { .. } if stream.maybe_full_range_in(policy) => {
            Some("a blur, a sharpen or an invert on a full-range picture, or one whose pixel format was never recorded (FFmpeg runs it on the raw values and converts the range afterwards)")
        }
        VideoEffect::Invert if stream.layout() == Some(PixLayout::Gray) => {
            Some("an invert on a gray picture (FFmpeg negates its luma only)")
        }
        _ => None,
    }
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
        Planner::new(timeline, assets, opts, PlanRequest::still(color))?.at(t)
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
            // `pad` hands everything after it the whole padded frame, bars included. A pointwise
            // effect (a grade, grayscale, invert) is a table, and the bars go through it like the
            // picture, and a mask is cut from the whole padded frame (the compositor's layer is that
            // frame); a blur and a sharpen see the bars as picture, which it does not draw.
            if geom.matte {
                if let Some(what) = layer.effects.iter().find_map(|e| match e {
                    VideoEffect::Blur { .. } => Some("a blur"),
                    VideoEffect::Sharpen { .. } => Some("a sharpen"),
                    _ => None,
                }) {
                    reasons.push(Unsupported::Matte(self.layer_ref(n, layer), what));
                }
            }
            // A sharpen is exact, but what it sharpens is the picture after the scales ahead of it
            // (every stage, or the fit scale when a moving zoom comes last), and the compositor's
            // scaler is one level off FFmpeg's. `unsharp` amplifies that level: at worst
            // `1 + 2 * amount` levels of luma at an edge sample (measured 3 at 1.5, 7 at 3, 11 at 5),
            // so only an amount up to 0.5 stays within the two levels a scaled blur does.
            let zoom_last = layer.animated.is_some_and(|a| a.zooms) && geom.stages.len() == 2;
            let before_effects = if zoom_last { 1 } else { geom.stages.len() };
            if geom.stages[..before_effects].iter().any(|s| s.scaled != (s.src.w, s.src.h))
                && layer
                    .effects
                    .iter()
                    .any(|e| matches!(e, VideoEffect::Sharpen { amount } if amount.abs() > MAX_SHARPEN_AFTER_SCALE))
            {
                reasons.push(Unsupported::Effects(
                    self.layer_ref(n, layer),
                    "a sharpen above 0.5 on a picture scaled ahead of it, which amplifies the scaler's one-level difference to 1 + 2 x amount levels at an edge",
                ));
            }
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
            // The round trip of a dip to white is the fade's own: the picture goes to `rgb24` and back
            // around it, and what the compositor can draw is the picture at the size the chain makes
            // it. The black bars of a letterboxed layer are padded in before the round trip, so
            // FFmpeg whitens them with the picture. A picture with an odd side makes the round trip
            // read past the line (the chroma pairs reach one pixel beyond it, into memory nothing
            // initialised). And a layer the scaler resizes is converted by the `scale` filter itself,
            // which negotiates the fade's `rgb24` as its output: scaling and converting in one pass,
            // with chroma interpolated from other samples than the compositor's scale-then-convert
            // (a half-size layer reads Y 19, UV 28 off at the picture's sharp edges; a crop is exact).
            if caps.dip_white && self.white_round_trip(layer) {
                let why = if geom.matte {
                    Some(DipWhy::Letterboxed)
                } else if geom.picture.0 % 2 == 1 || geom.picture.1 % 2 == 1 {
                    Some(DipWhy::OddSize(geom.picture.0, geom.picture.1))
                } else if geom.stages.iter().any(|s| s.scaled != (s.src.w, s.src.h)) {
                    Some(DipWhy::Resized)
                } else {
                    None
                };
                if let Some(why) = why {
                    reasons.push(Unsupported::DipToWhite(
                        LayerRef {
                            index: n,
                            name: layer.name.clone(),
                        },
                        why,
                    ));
                }
            }
            // A full-range picture the scaler resizes: swscale converts the range *inside* the
            // resize, from the chroma it has not yet scaled, where the compositor converts the
            // range and then scales. Plain, the two agree outside the picture's sharp edges (a still
            // is held to that); in an export's planes the chroma reads up to 15 levels off (FFmpeg 6 as
            // much as 9), and a fade running on it takes the flat error past the bound too (14).
            if layer.stream.maybe_full_range() && geom.stages.iter().any(|s| s.scaled != (s.src.w, s.src.h)) {
                let where_ = if self.mode == PlanMode::Motion {
                    Some("in an export frame")
                } else if self.fade_ops(layer).iter().any(|o| o.factor < FADE_FULL) {
                    Some("inside a fade, a dip or a dissolve")
                } else {
                    None
                };
                if let Some(where_) = where_ {
                    reasons.push(Unsupported::FullRangeResized(
                        LayerRef {
                            index: n,
                            name: layer.name.clone(),
                        },
                        where_,
                    ));
                }
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
        // `drawtext`'s `fontsize` is the frame's height times the overlay's size, rounded.
        if caps.text {
            let smallest = self
                .overlays
                .iter()
                .map(|o| (f64::from(size.1) * o.size).round().max(1.0) as u32)
                .min();
            if let Some(px) = smallest.filter(|px| *px < MIN_TEXT_PX) {
                reasons.push(Unsupported::TextTooSmall(px));
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
            if self.canvas.fps != self.canvas.pick_fps && !self.layers.is_empty() {
                out.push(Unsupported::PickRate {
                    canvas: self.canvas.fps,
                    clips: self.canvas.pick_fps,
                });
            }
            // FFmpeg 9 keeps a full-range picture's raw values through the graph and flags the whole
            // export full range: its own planes stay raw (17 levels off a compositor that converts to
            // limited range first) and **every limited-range frame of the cut is converted to full range**
            // (a black frame reads Y 0, not 16; a test pattern 76, not 81). That is a fact about the cut,
            // not about the layers in this frame: a frame before the clip, after it, on another track, or a
            // gap, is as far off. FFmpeg 6 converts at the decode, as the compositor does.
            if self.canvas.full_range_cut && self.canvas.policy != CompositeColorPolicy::FixedBt601 {
                out.push(Unsupported::FullRangeCut);
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
            // clip's opacity is a `geq` alpha instead, refused below. Only a **keyed opacity** is
            // that: a clip with some other number keyed (a position, a zoom) and a static opacity
            // below 1 still takes the round trip, so it still needs the matrix.
            let keyed = self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.keys.is_some());
            let alpha_keyed = self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.keys.is_some_and(|k| k.opacity));
            if layer.transform.opacity < 1.0 && stream.matrix().is_none() && !alpha_keyed {
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
            if !layer.color.is_identity() && stream.maybe_full_range_in(self.canvas.policy) {
                let detail = match stream.pix_fmt.as_deref() {
                    Some(f) if stream.maybe_full_range() => {
                        format!("a full-range picture ({f}), which FFmpeg grades before converting its range")
                    }
                    Some(f) => format!("a picture deeper than 8 bits ({f}) whose range is not recorded (it may be full range)"),
                    None => "a picture whose pixel format was never recorded (it may be full range)".to_string(),
                };
                out.push(Unsupported::GradedFullRange(at(), detail));
            }
            if let Some(what) = layer
                .effects
                .iter()
                .find_map(|e| effect_refusal(e, caps).or_else(|| effect_on_picture_refusal(e, stream, self.canvas.policy)))
            {
                out.push(Unsupported::Effects(at(), what));
            }
            if layer.mask.is_some() {
                if !caps.mask {
                    out.push(Unsupported::Mask(at()));
                } else if let Some(what) = self.mask_refusal(layer) {
                    out.push(Unsupported::MaskWith(at(), what));
                }
            }
            if (layer.reframe.is_some() || layer.projection.is_some()) && !caps.reframe {
                out.push(Unsupported::Reframe(at()));
            }
            // A fade, a dip, a dissolve, a slide or a push: both modes hold them (the FFmpeg still
            // draws the export's frame inside one, `engine::cli::still_fx`), and a compositor says
            // whether it draws them. The `fade` filter of a still runs on the timebase of the clip's
            // `fps=` filter and the plan counts frames on the canvas's; where the two parse the rate to
            // different rationals the count is not the plan's.
            let in_progress = layer.fx.in_progress(frame, fps);
            if let Some((edge, dissolve)) = in_progress.filter(|_| !caps.fades) {
                out.push(Unsupported::Fade(at(), edge, dissolve));
            }
            if in_progress.is_some()
                && self.mode == PlanMode::Still
                && self.canvas.fps != self.canvas.pick_fps
                && !out.iter().any(|u| matches!(u, Unsupported::PickRate { .. }))
            {
                out.push(Unsupported::PickRate {
                    canvas: self.canvas.fps,
                    clips: self.canvas.pick_fps,
                });
            }
            // A dip to white is a filter in the export's chain for every frame of the clip, and in
            // the still's while the fade runs. `fade c=white` only takes RGB, so the picture is
            // converted `yuv420p -> rgb24 -> yuv420p` around it: reproduced for a plain chain on an
            // 8-bit limited-range 4:2:0 picture (the conversion's own kernel is within 2 levels of the
            // tables the compositor follows; every other format was measured 9 to 21 levels off,
            // gray 2 on one picture and not claimed). On an alpha plane FFmpeg takes another RGB
            // format, and a black fade of the clip that runs ahead of it is a filter of its own in
            // the YUV.
            let ops = self.fade_ops(layer);
            if self.white_round_trip(layer) {
                let why = if !caps.dip_white {
                    Some(DipWhy::NoRoundTrip)
                } else if self.on_alpha_plane(layer) {
                    Some(DipWhy::AlphaPlane)
                } else if ops.iter().any(|o| o.tint == FadeTint::Black && o.factor < FADE_FULL) {
                    Some(DipWhy::BlackFade)
                } else if stream.pix_fmt.as_deref() != Some("yuv420p") {
                    Some(DipWhy::Format)
                } else if !matches!(stream.matrix(), Some(YuvMatrix::Bt601 | YuvMatrix::Bt709)) {
                    Some(DipWhy::Matrix)
                } else {
                    None
                };
                if let Some(why) = why {
                    out.push(Unsupported::DipToWhite(at(), why));
                }
            }
            // FFmpeg 9 keeps a full-range picture's raw values in the graph and converts the range at
            // the end: `fade` darkens them toward the limited-range black 16 first (frame 0 of a
            // fade-in reads Y 30 where black is 16). FFmpeg 6 converts at the decode, as the compositor
            // does. (An export frame is refused for the cut it is in, below.)
            if self.mode == PlanMode::Still
                && self.canvas.policy != CompositeColorPolicy::FixedBt601
                && stream.maybe_full_range_in(self.canvas.policy)
                && ops.iter().any(|o| o.factor < FADE_FULL)
            {
                out.push(Unsupported::FullRangeNegotiated(at()));
            }
            if self.through_rgba(layer) && ops.iter().any(|o| o.tint != FadeTint::Alpha && o.factor < FADE_FULL) {
                out.push(Unsupported::FadeOnTranslucent(at()));
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
            if self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.color) && !caps.keyed_color {
                out.push(Unsupported::KeyedColor(at()));
            }
            if layer.animated.is_some_and(|a| a.zooms) {
                if caps.keyed_zoom {
                    // The compositor draws a grade, the effects and the mask it draws, and the fades
                    // at the fit size and zooms after them. A rotation and an opacity below 1 are
                    // the two it does not order that way: the rotated picture's alpha and the RGB
                    // round trip would both have to be scaled with the zoom.
                    if let Some(what) = self.zoom_order_refusal(layer, &ops) {
                        out.push(Unsupported::ZoomBehind(at(), what));
                    }
                } else if keyed {
                    out.push(Unsupported::KeyedZoom(at()));
                } else if !layer.color.is_identity()
                    || layer.transform.rotation != 0.0
                    || layer.transform.opacity < 1.0
                    || layer.mask.is_some()
                    || !layer.effects.is_empty()
                {
                    // A still's moving zoom is only the one stage when nothing follows it.
                    out.push(Unsupported::ZoomBehind(at(), "a grade, rotation, opacity, mask or effect"));
                }
            }
        }
        if !self.overlays.is_empty() {
            if !caps.text {
                out.push(Unsupported::Text);
            } else {
                if self.overlays.iter().any(|o| o.font_file.is_none()) {
                    out.push(Unsupported::TextWithoutFont);
                }
                if self.overlays.iter().any(|o| o.synthetic_bold) {
                    out.push(Unsupported::TextBorder);
                }
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
        if policy != CompositeColorPolicy::FixedBt601 && self.layers.iter().any(|l| l.stream.maybe_full_range_in(policy)) {
            for (n, layer) in self.layers.iter().enumerate() {
                if !layer.color.is_identity() && !layer.stream.maybe_full_range_in(policy) {
                    out.push(Unsupported::GradedInFullRangeStack(n));
                }
            }
        }
        out
    }

    fn layer_ref(&self, index: usize, layer: &PlanLayer) -> LayerRef {
        LayerRef {
            index,
            name: layer.name.clone(),
        }
    }

    /// Why a compositor that draws masks still does not draw this layer's: the mask is `geq`
    /// rewriting the alpha plane, ahead of `colorchannelmixer` (the RGB round trip an opacity below 1
    /// takes, which is arithmetic on that alpha) and `rotate` (which turns the alpha plane with
    /// the picture, a partly transparent edge included). The opacity judged is the sampled one, the
    /// frame's own: a clip whose keys hold 1 here has no alpha to round-trip (a keyed opacity as such
    /// is `KeyedOpacity`'s).
    fn mask_refusal(&self, layer: &PlanLayer) -> Option<&'static str> {
        let rotated =
            layer.transform.rotation != 0.0 || (self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.rotates));
        if rotated {
            Some("on a rotated layer")
        } else if layer.transform.opacity < 1.0 {
            Some("on a layer below full opacity")
        } else {
            None
        }
    }

    /// What is ahead of a moving zoom that a compositor with `keyed_zoom` still does not draw: a
    /// rotation and an opacity below 1 (their alpha plane and RGB round trip would be scaled with
    /// the zoom), and a dip to white (the zoom's `scale` takes the RGB frame `fade c=white` leaves
    /// and converts it to 4:2:0 in the same pass, which neither FFmpeg 6.1 nor 9.0 does as a scale of
    /// the converted picture: up to 14 levels off on 6.1, 3 on 9.0).
    fn zoom_order_refusal(&self, layer: &PlanLayer, ops: &[FadeOp]) -> Option<&'static str> {
        let rotated =
            layer.transform.rotation != 0.0 || (self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.rotates));
        if rotated {
            Some("a rotation")
        } else if layer.transform.opacity < 1.0 {
            Some("an opacity below 1")
        } else if ops
            .iter()
            .any(|o| o.tint == FadeTint::White && (self.mode == PlanMode::Motion || o.factor < FADE_FULL))
        {
            Some("a dip to white")
        } else {
            None
        }
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
            keyframed: layer.animated.filter(|_| self.mode == PlanMode::Motion).and_then(|a| a.keys),
            offset: layer.fx.motion,
        }
    }

    /// What the `tint` fades of `layer` leave of its picture at this plan's frame
    /// ([`LayerFx::strength`]): `1` untouched, `0` gone.
    pub fn strength(&self, layer: &PlanLayer, tint: FadeTint) -> f64 {
        layer.fx.strength(tint, self.frame, self.canvas.fps)
    }

    /// The `fade` filters of `layer`'s chain at this frame, **in the order the chain applies them**
    /// (its own fade-in and out merged with a dip to black's, then a dip to white's, then a
    /// dissolve's alpha ramp), each with the integer factor it multiplies by
    /// ([`FadeStep::factor_at_frame`]). One whose factor is [`FADE_FULL`] is not changing the
    /// picture at this frame; a compositor skips it, except that in an export frame a dip to white
    /// is a filter that is there for every frame of the clip (the RGB round trip around it is what
    /// is drawn, whatever the factor).
    pub fn fade_ops(&self, layer: &PlanLayer) -> Vec<FadeOp> {
        let fps = self.canvas.fps;
        layer
            .fx
            .fades
            .iter()
            .map(|s| FadeOp {
                tint: s.tint,
                edge: s.edge,
                factor: s.factor_at_frame(self.frame, fps.num, fps.den),
            })
            .collect()
    }

    /// Whether the clip's chain takes the `yuv420p -> rgb24 -> yuv420p` round trip a dip to white
    /// needs at this frame: in an export it is a filter of every frame of the clip, in a still only
    /// while the fade runs.
    pub fn white_round_trip(&self, layer: &PlanLayer) -> bool {
        let fps = self.canvas.fps;
        layer.fx.fades.iter().any(|s| {
            s.tint == FadeTint::White
                && (self.mode == PlanMode::Motion || s.factor_at_frame(self.frame, fps.num, fps.den) < FADE_FULL)
        })
    }

    /// Whether the clip's picture is in RGBA when its fades run: an opacity below 1 is a
    /// `colorchannelmixer`, which only takes RGB, and what follows it (`rotate`, `fade`) takes RGBA
    /// as it is until the `overlay` asks for `yuva420p`. A keyed opacity is a `geq` alpha instead.
    pub fn through_rgba(&self, layer: &PlanLayer) -> bool {
        let keyed = self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.keys.is_some_and(|k| k.opacity));
        !keyed && layer.transform.opacity < 1.0
    }

    /// Whether the clip's chain is on a `yuva420p` picture at its fades, as the graph that made this
    /// plan builds it: `fade` takes a black of 0 there and 16 on `yuv420p`, and a dip to white
    /// converts through another RGB format. The export puts a chain on an alpha plane for a rotation,
    /// an opacity below 1 (either one *keyed* too, whatever its value at this frame), a mask, a chroma
    /// key and for the dissolve the clip enters through (the whole clip, not only its ramp); the still
    /// builds the chain for the pose it samples, so a keyed number counts only by its value.
    pub fn on_alpha_plane(&self, layer: &PlanLayer) -> bool {
        let motion = self.mode == PlanMode::Motion;
        let keys = layer.animated.filter(|_| motion).and_then(|a| a.keys);
        let keyed_rotation = keys.is_some_and(|k| k.rotation);
        let keyed_opacity = keys.is_some_and(|k| k.opacity);
        let animated = layer.animated.filter(|_| motion);
        (!keyed_rotation && layer.transform.rotation != 0.0)
            || (!keyed_opacity && layer.transform.opacity < 1.0)
            || animated.is_some_and(|a| a.rotates || a.opacity)
            || layer.mask.is_some()
            || layer.effects.iter().any(|e| e.produces_alpha())
            || layer.fx.fades.iter().any(|s| s.tint == FadeTint::Alpha)
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
    fn a_clip_with_a_tail_stays_on_screen_past_its_end_and_flat_counts_every_track() {
        let a = asset("a", 20.0, vec![video(640, 360)]);
        let mut tl = timeline(vec![vec![Clip::new(a.id, 0.0, 4.0, 0.0), Clip::new(a.id, 8.0, 12.0, 4.0)]]);
        // An audio track ahead of the picture shifts the picture clips' storage index.
        let mut sound = Track::new(StreamKind::Audio, "A");
        sound.clips = vec![Clip::new(a.id, 0.0, 9.0, 0.0)];
        tl.tracks.insert(0, sound);
        let assets = std::slice::from_ref(&a);
        let tails = [0.0, 1.5, 0.0];
        let at = |t: f64| {
            active_video_clips_with_tails(&tl, assets, t, &tails, t)
                .iter()
                .map(|c| (c.flat, (c.source_time * 1000.0).round() as i64))
                .collect::<Vec<_>>()
        };
        // The first picture clip is flat index 1; its tail keeps it under the next one, on the
        // source past its out point, until the tail ends.
        assert_eq!(at(3.0), vec![(1, 3000)]);
        assert_eq!(at(4.5), vec![(1, 4500), (2, 8500)]);
        assert_eq!(at(5.49), vec![(1, 5490), (2, 9490)]);
        assert_eq!(at(5.5), vec![(2, 9500)]);
        // Without tails (or with a table that stops short) it is `active_video_clips`.
        let plain = active_video_clips(&tl, assets, 4.5);
        assert_eq!(plain.len(), 1);
        assert_eq!(active_video_clips_with_tails(&tl, assets, 4.5, &[0.0], 4.5).len(), 1);
        // The tail is judged at `tail_at`: a window that closes inside a frame is open for it.
        let at = |t, tail_at| active_video_clips_with_tails(&tl, assets, t, &tails, tail_at).len();
        assert_eq!((at(5.52, 5.5), at(5.52, 5.49)), (1, 2));
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
                easing: Default::default(),
            },
            Keyframe {
                time: 2.0,
                scale: 0.5,
                pos_x: 0.2,
                pos_y: 0.0,
                rotation: 10.0,
                opacity: 0.0,
                easing: Default::default(),
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

    /// The still plan of `edit`ed clip `a` (0..10 s on the timeline) at `t`.
    fn fading_plan(edit: impl FnOnce(&mut Clip), t: f64) -> RenderPlan {
        fading_plan_of(video(640, 360), None, CompositeColorPolicy::FixedBt601, edit, t)
    }

    fn fading_plan_of(
        stream: StreamInfo,
        delivery: Option<Delivery>,
        policy: CompositeColorPolicy,
        edit: impl FnOnce(&mut Clip),
        t: f64,
    ) -> RenderPlan {
        let a = asset("a", 20.0, vec![stream]);
        let mut clip = Clip::new(a.id, 0.0, 10.0, 0.0);
        edit(&mut clip);
        let mut tl = timeline(vec![vec![clip]]);
        tl.format = delivery;
        RenderPlan::at(
            &tl,
            std::slice::from_ref(&a),
            &ExportOptions {
                fps: Some(30.0),
                ..ExportOptions::default()
            },
            t,
            policy,
        )
        .unwrap()
    }

    #[test]
    fn a_fades_ops_carry_the_integer_the_filter_multiplies_by_in_chain_order() {
        // Fade-in over 1 s (30 frames of 30 fps), fade-out over the last 0.5 s: at frame 15 the
        // in-fade is half way (65535 * 15 / 30, truncated), the out-fade has not begun.
        let plan = fading_plan(
            |c| {
                c.fade_in = 1.0;
                c.fade_out = 0.5;
            },
            0.5,
        );
        assert_eq!(plan.frame, 15);
        let ops = plan.fade_ops(&plan.layers[0]);
        assert_eq!(
            ops,
            vec![
                FadeOp {
                    tint: FadeTint::Black,
                    edge: FadeEdge::In,
                    factor: 32767
                },
                FadeOp {
                    tint: FadeTint::Black,
                    edge: FadeEdge::Out,
                    factor: FADE_FULL
                },
            ]
        );
        // The last frames: the out-fade counts down to 0 on frame 300, and the in-fade is full.
        let plan = fading_plan(
            |c| {
                c.fade_in = 1.0;
                c.fade_out = 0.5;
            },
            9.9,
        );
        let ops = plan.fade_ops(&plan.layers[0]);
        assert_eq!((ops[0].factor, ops[1].factor), (FADE_FULL, 65535 - 65535 * 12 / 15));
    }

    #[test]
    fn the_chain_is_on_an_alpha_plane_for_a_rotation_an_opacity_and_a_dissolve() {
        let on = |edit: &dyn Fn(&mut Clip)| {
            let plan = fading_plan(|c| edit(c), 0.5);
            plan.on_alpha_plane(&plan.layers[0])
        };
        assert!(!on(&|_| {}));
        assert!(!on(&|c| c.fade_in = 1.0), "a plain fade is on yuv420p, black 16");
        assert!(on(&|c| c.transform.rotation = 10.0));
        assert!(on(&|c| c.transform.opacity = 0.5));
        assert!(on(&|c| c.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0
        })));
        // A dip is a fade of the picture itself: no alpha plane.
        assert!(!on(&|c| c.transition_in = Some(Transition {
            kind: TransitionKind::DipToBlack,
            duration: 1.0
        })));
        // Only an opacity below 1 leaves the picture in RGBA at its fades.
        let plan = fading_plan(|c| c.transform.opacity = 0.5, 0.5);
        assert!(plan.through_rgba(&plan.layers[0]));
        let plan = fading_plan(|c| c.transform.rotation = 10.0, 0.5);
        assert!(!plan.through_rgba(&plan.layers[0]));
    }

    #[test]
    fn what_the_compositor_cannot_do_with_a_fade_is_refused_with_its_own_reason() {
        let all = GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            dip_white: true,
            ..GpuCaps::A0
        };
        let reasons = |edit: &dyn Fn(&mut Clip), t: f64| {
            let plan = fading_plan(|c| edit(c), t);
            plan.reasons(&all, plan.size(u32::MAX))
        };
        // A black fade on a translucent layer runs on the RGBA picture, alpha included.
        let why = reasons(
            &|c| {
                c.transform.opacity = 0.5;
                c.fade_in = 1.0;
            },
            0.5,
        );
        assert!(matches!(why.as_slice(), [Unsupported::FadeOnTranslucent(_)]), "{why:?}");
        // ... but not once it is over (nothing runs), and a dissolve's ramp only touches alpha.
        assert!(reasons(
            &|c| {
                c.transform.opacity = 0.5;
                c.fade_in = 1.0;
            },
            2.0
        )
        .is_empty());
        assert!(reasons(
            &|c| {
                c.transform.opacity = 0.5;
                c.transition_in = Some(Transition {
                    kind: TransitionKind::Crossfade,
                    duration: 1.0,
                });
            },
            0.5
        )
        .is_empty());
        // A dip to white is drawn on a plain chain only: an alpha plane and a black fade running
        // ahead of it are FFmpeg's.
        let white = |c: &mut Clip| {
            c.transition_in = Some(Transition {
                kind: TransitionKind::DipToWhite,
                duration: 2.0,
            });
        };
        assert!(reasons(&white, 0.5).is_empty());
        let why = reasons(
            &|c| {
                white(c);
                c.transform.rotation = 10.0;
            },
            0.5,
        );
        assert!(
            matches!(why.as_slice(), [Unsupported::DipToWhite(_, DipWhy::AlphaPlane)]),
            "{why:?}"
        );
        let why = reasons(
            &|c| {
                white(c);
                c.fade_in = 2.0;
            },
            0.5,
        );
        assert!(
            matches!(why.as_slice(), [Unsupported::DipToWhite(_, DipWhy::BlackFade)]),
            "{why:?}"
        );
        // Without the ability the frame is refused for it, once the fade is running.
        let plan = fading_plan(white, 0.5);
        let why = plan.reasons(&GpuCaps { dip_white: false, ..all }, plan.size(u32::MAX));
        assert!(
            matches!(why.as_slice(), [Unsupported::DipToWhite(_, DipWhy::NoRoundTrip)]),
            "{why:?}"
        );
        // A still plan has no dip to white in its chain once the fade is over; an export frame always has.
        let plan = fading_plan(white, 5.0);
        assert!(plan
            .reasons(&GpuCaps { dip_white: false, ..all }, plan.size(u32::MAX))
            .is_empty());
    }

    #[test]
    fn what_the_compositor_cannot_do_with_a_mask_an_effect_or_a_moving_zoom_is_refused_with_its_own_reason() {
        use crate::model::Keyframe;
        let caps = GpuCaps {
            mask: true,
            keyed_zoom: true,
            fades: true,
            effects: EffectKinds::BLUR
                .union(EffectKinds::SHARPEN)
                .union(EffectKinds::GRAYSCALE)
                .union(EffectKinds::INVERT),
            ..GpuCaps::A0
        };
        let mask = Mask {
            shape: MaskShape::Ellipse,
            x: 0.5,
            y: 0.5,
            width: 0.5,
            height: 0.5,
            feather: 0.1,
            inverted: false,
        };
        let reasons = |edit: &dyn Fn(&mut Clip)| {
            let plan = fading_plan(|c| edit(c), 0.5);
            plan.reasons(&caps, plan.size(u32::MAX))
        };
        // What it draws: a mask, the four effects, all of them at once, and a moving zoom in front of which they are.
        let zoom = |c: &mut Clip| {
            let key = |time: f64, scale: f64| Keyframe {
                time,
                scale,
                pos_x: 0.0,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
                easing: Default::default(),
            };
            c.keyframes = vec![key(0.0, 1.0), key(4.0, 1.5)];
        };
        assert!(reasons(&|c| c.mask = Some(mask)).is_empty());
        assert!(reasons(&|c| c.effects = vec![
            VideoEffect::Blur { sigma: 3.0 },
            VideoEffect::Sharpen { amount: 1.0 },
            VideoEffect::Grayscale,
            VideoEffect::Invert,
        ])
        .is_empty());
        assert!(reasons(&|c| {
            zoom(c);
            c.mask = Some(mask);
            c.color.contrast = 1.2;
            c.fade_in = 2.0;
            c.effects = vec![VideoEffect::Blur { sigma: 3.0 }];
        })
        .is_empty());
        // A vignette (its dither is a counter that runs on across frames) and a chroma key (a float colour
        // distance): FFmpeg's, named.
        for (e, what) in [
            (VideoEffect::Vignette, "vignette"),
            (
                VideoEffect::ChromaKey {
                    color: "green".into(),
                    similarity: 0.1,
                    blend: 0.1,
                },
                "chroma key",
            ),
        ] {
            let why = reasons(&|c| c.effects = vec![VideoEffect::Blur { sigma: 3.0 }, e.clone()]);
            assert!(
                matches!(why.as_slice(), [Unsupported::Effects(_, w)] if w.contains(what)),
                "{why:?}"
            );
        }
        // A value the filter's own option refuses.
        for e in [
            VideoEffect::Blur { sigma: 2000.0 },
            VideoEffect::Blur { sigma: f64::NAN },
            VideoEffect::Sharpen { amount: 6.0 },
            VideoEffect::Sharpen { amount: -3.0 },
        ] {
            let why = reasons(&|c| c.effects = vec![e.clone()]);
            assert!(matches!(why.as_slice(), [Unsupported::Effects(..)]), "{e:?}: {why:?}");
        }
        // A mask on a rotated layer, and on one below full opacity.
        let why = reasons(&|c| {
            c.mask = Some(mask);
            c.transform.rotation = 10.0;
        });
        assert!(
            matches!(why.as_slice(), [Unsupported::MaskWith(_, w)] if w.contains("rotated")),
            "{why:?}"
        );
        let why = reasons(&|c| {
            c.mask = Some(mask);
            c.transform.opacity = 0.5;
        });
        assert!(
            matches!(why.as_slice(), [Unsupported::MaskWith(_, w)] if w.contains("opacity")),
            "{why:?}"
        );
        // The opacity that counts is the sampled one: a clip whose keys hold 1 at this time is drawn.
        let faded_by_keys = |t: f64| {
            let key = |time: f64, opacity: f64| Keyframe {
                time,
                scale: 1.0,
                pos_x: 0.0,
                pos_y: 0.0,
                rotation: 0.0,
                opacity,
                easing: Default::default(),
            };
            let plan = fading_plan(
                |c| {
                    c.mask = Some(mask);
                    c.keyframes = vec![key(0.0, 1.0), key(1.0, 1.0), key(4.0, 0.5)];
                },
                t,
            );
            plan.reasons(&caps, plan.size(u32::MAX))
        };
        assert!(faded_by_keys(0.5).is_empty(), "{:?}", faded_by_keys(0.5));
        let why = faded_by_keys(3.0);
        assert!(
            matches!(why.as_slice(), [Unsupported::MaskWith(_, w)] if w.contains("opacity")),
            "{why:?}"
        );
        // Without the ability: the mask is refused as a mask.
        let masked = fading_plan(|c| c.mask = Some(mask), 0.5);
        let why = masked.reasons(&GpuCaps { mask: false, ..caps }, masked.size(u32::MAX));
        assert!(matches!(why.as_slice(), [Unsupported::Mask(_)]), "{why:?}");
        // A moving zoom with a rotation or an opacity below 1 in front of it: refused whatever the caps.
        let why = reasons(&|c| {
            zoom(c);
            c.keyframes.iter_mut().for_each(|k| k.rotation = 12.0);
        });
        assert!(
            matches!(why.as_slice(), [Unsupported::ZoomBehind(_, w)] if w.contains("rotation")),
            "{why:?}"
        );
        let why = reasons(&|c| {
            zoom(c);
            c.keyframes.iter_mut().for_each(|k| k.opacity = 0.7);
        });
        assert!(
            matches!(why.as_slice(), [Unsupported::ZoomBehind(_, w)] if w.contains("opacity")),
            "{why:?}"
        );
        // ... and a dip to white (the zoom's `scale` takes FFmpeg's RGB frame and converts it in the same pass).
        let dip = fading_plan(
            |c| {
                zoom(c);
                c.transition_in = Some(Transition {
                    kind: TransitionKind::DipToWhite,
                    duration: 2.0,
                });
            },
            0.5,
        );
        let why = dip.reasons(
            &GpuCaps {
                transitions: true,
                dip_white: true,
                ..caps
            },
            dip.size(u32::MAX),
        );
        // (A zoom resizes the picture, which the dip to white refuses on its own account too.)
        assert!(
            why.iter()
                .any(|u| matches!(u, Unsupported::ZoomBehind(_, w) if w.contains("dip to white"))),
            "{why:?}"
        );
        // On a letterbox (a 640x360 picture in a vertical frame) the padded frame is what a blur sees; a
        // table is the same on the bars as on the picture, and a mask is cut from the whole padded frame.
        let a = asset("a", 20.0, vec![video(640, 360)]);
        let letterboxed = |edit: &dyn Fn(&mut Clip)| {
            let mut clip = Clip::new(a.id, 0.0, 10.0, 0.0);
            edit(&mut clip);
            let mut tl = timeline(vec![vec![clip]]);
            tl.format = Some(crate::model::Delivery::new(360, 640, Fit::Contain));
            let planned = plan(&tl, std::slice::from_ref(&a), 0.5);
            planned.reasons(&caps, planned.size(u32::MAX))
        };
        assert!(letterboxed(&|c| c.mask = Some(mask)).is_empty());
        let why = letterboxed(&|c| c.effects = vec![VideoEffect::Blur { sigma: 3.0 }]);
        assert!(
            matches!(why.as_slice(), [Unsupported::Matte(_, w)] if w.contains("blur")),
            "{why:?}"
        );
        assert!(letterboxed(&|c| c.effects = vec![VideoEffect::Invert, VideoEffect::Grayscale]).is_empty());
    }

    #[test]
    fn a_strong_sharpen_is_refused_on_a_picture_scaled_ahead_of_it() {
        use crate::model::Keyframe;
        let caps = GpuCaps {
            effects: EffectKinds::BLUR
                .union(EffectKinds::SHARPEN)
                .union(EffectKinds::GRAYSCALE)
                .union(EffectKinds::INVERT),
            keyed_zoom: true,
            ..GpuCaps::A0
        };
        let sharp = |amount: f64| VideoEffect::Sharpen { amount };
        let reasons = |edit: &dyn Fn(&mut Clip), width: u32| {
            let plan = fading_plan(|c| edit(c), 0.5);
            plan.reasons(&caps, plan.size(width))
        };
        let refused = |why: &[Unsupported]| matches!(why, [Unsupported::Effects(_, w)] if w.contains("sharpen above 0.5"));
        let pip = |effects: Vec<VideoEffect>| {
            move |c: &mut Clip| {
                c.transform.scale = 0.6;
                c.effects = effects.clone();
            }
        };
        // A scaled layer: an amount past 0.5 is refused (either sign), up to it drawn, a blur is not touched.
        for amount in [0.6, 1.5, 5.0, -1.0] {
            let why = reasons(&pip(vec![sharp(amount)]), u32::MAX);
            assert!(refused(&why), "{amount}: {why:?}");
        }
        for amount in [0.0, 0.3, 0.5, -0.5] {
            let why = reasons(&pip(vec![sharp(amount)]), u32::MAX);
            assert!(why.is_empty(), "{amount}: {why:?}");
        }
        assert!(reasons(&pip(vec![VideoEffect::Blur { sigma: 3.0 }]), u32::MAX).is_empty());
        // A chain: a sharpen after a blur or before one, and grayscale beside it, on a scaled layer.
        for effects in [
            vec![VideoEffect::Blur { sigma: 3.0 }, sharp(1.5)],
            vec![sharp(1.5), VideoEffect::Blur { sigma: 3.0 }],
            vec![VideoEffect::Grayscale, sharp(2.0)],
        ] {
            let why = reasons(&pip(effects.clone()), u32::MAX);
            assert!(refused(&why), "{effects:?}: {why:?}");
        }
        // Unscaled it is exact: the picture at its own size, any amount...
        let plain = |c: &mut Clip| c.effects = vec![sharp(5.0)];
        assert!(reasons(&plain, u32::MAX).is_empty());
        // ...but rendered smaller (the fit scale is a real one then), refused.
        let why = reasons(&plain, 480);
        assert!(refused(&why), "{why:?}");
        // A moving zoom comes last: only the fit scale is ahead of the effects.
        let zooming = |c: &mut Clip| {
            let key = |time: f64, scale: f64| Keyframe {
                time,
                scale,
                pos_x: 0.0,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
                easing: Default::default(),
            };
            c.keyframes = vec![key(0.0, 1.0), key(4.0, 1.5)];
            c.effects = vec![sharp(3.0)];
        };
        assert!(reasons(&zooming, u32::MAX).is_empty(), "{:?}", reasons(&zooming, u32::MAX));
        let why = reasons(&zooming, 480);
        assert!(refused(&why), "{why:?}");
    }

    #[test]
    fn an_effect_the_picture_changes_the_meaning_of_is_refused() {
        let caps = GpuCaps {
            effects: EffectKinds::BLUR
                .union(EffectKinds::SHARPEN)
                .union(EffectKinds::GRAYSCALE)
                .union(EffectKinds::INVERT),
            ..GpuCaps::A0
        };
        let reasons_in = |policy: CompositeColorPolicy, fmt: Option<&str>, effect: VideoEffect| {
            let mut a = asset("a", 20.0, vec![video(640, 360)]);
            a.streams[0].pix_fmt = fmt.map(Into::into);
            let mut clip = Clip::new(a.id, 0.0, 10.0, 0.0);
            clip.effects = vec![effect];
            let planned = RenderPlan::at(
                &timeline(vec![vec![clip]]),
                std::slice::from_ref(&a),
                &ExportOptions::default(),
                0.5,
                policy,
            )
            .unwrap();
            planned.reasons(&caps, planned.size(u32::MAX))
        };
        let reasons = |fmt: Option<&str>, effect: VideoEffect| reasons_in(CompositeColorPolicy::FixedBt601, fmt, effect);
        let effects = || {
            [
                VideoEffect::Blur { sigma: 3.0 },
                VideoEffect::Sharpen { amount: 1.0 },
                VideoEffect::Grayscale,
                VideoEffect::Invert,
            ]
        };
        // Limited-range 4:2:0 and the formats the decode converts to it: all drawn.
        for fmt in ["yuv420p", "yuv420p10le", "yuv422p", "yuv444p", "bgr0", "rgb24"] {
            for e in effects() {
                let why = reasons(Some(fmt), e.clone());
                assert!(why.is_empty(), "{fmt} {e:?}: {why:?}");
            }
        }
        // Full range (a JPEG, an mjpeg, an H.264 file flagged `pc`) or never recorded: an invert, a blur and a
        // sharpen are FFmpeg's; grayscale is the same on any range.
        for fmt in [Some("yuvj420p"), Some("yuvj444p"), None] {
            for e in effects() {
                let why = reasons(fmt, e.clone());
                match e {
                    VideoEffect::Grayscale => assert!(why.is_empty(), "{fmt:?}: {why:?}"),
                    _ => assert!(
                        matches!(why.as_slice(), [Unsupported::Effects(_, w)] if w.contains("full-range")),
                        "{fmt:?} {e:?}: {why:?}"
                    ),
                }
            }
        }
        // A deeper-than-8-bit picture may carry a `pc` tag FFmpeg 9 keeps the raw values of: where the range is
        // negotiated it is refused like a full-range one (grayscale apart), under `FixedBt601` it is drawn.
        for policy in [CompositeColorPolicy::BottomLayerTag, CompositeColorPolicy::Unknown] {
            for e in effects() {
                let why = reasons_in(policy, Some("yuv420p10le"), e.clone());
                match e {
                    VideoEffect::Grayscale => assert!(why.is_empty(), "{policy:?}: {why:?}"),
                    _ => assert!(
                        matches!(why.as_slice(), [Unsupported::Effects(_, w)] if w.contains("full-range")),
                        "{policy:?} {e:?}: {why:?}"
                    ),
                }
            }
        }
        // Gray: no chroma for FFmpeg's negate to touch, so only an invert is refused.
        for e in effects() {
            let why = reasons(Some("gray"), e.clone());
            match e {
                VideoEffect::Invert => assert!(
                    matches!(why.as_slice(), [Unsupported::Effects(_, w)] if w.contains("gray")),
                    "{why:?}"
                ),
                _ => assert!(why.is_empty(), "{e:?}: {why:?}"),
            }
        }
    }

    #[test]
    fn a_dip_to_white_is_drawn_for_the_pictures_the_round_trip_was_measured_on() {
        let all = GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            dip_white: true,
            ..GpuCaps::A0
        };
        let white = |c: &mut Clip| {
            c.transition_in = Some(Transition {
                kind: TransitionKind::DipToWhite,
                duration: 2.0,
            });
        };
        let why = |stream: StreamInfo, delivery: Option<Delivery>, t: f64| {
            let plan = fading_plan_of(stream, delivery, CompositeColorPolicy::FixedBt601, white, t);
            plan.reasons(&all, plan.size(u32::MAX))
        };
        assert!(why(video(640, 360), None, 0.5).is_empty());
        let tagged = |space: &str| StreamInfo {
            color_space: Some(space.into()),
            ..video(640, 360)
        };
        assert!(
            why(tagged("bt709"), None, 0.5).is_empty(),
            "BT.709 was measured: 3 / 2 levels"
        );
        let dipped = |w: Vec<Unsupported>, what: DipWhy| {
            assert!(
                matches!(w.as_slice(), [Unsupported::DipToWhite(_, m)] if *m == what),
                "{what:?}: {w:?}"
            );
        };
        dipped(why(tagged("bt2020nc"), None, 0.5), DipWhy::Matrix);
        // Every other format was measured 9 to 21 levels off the round trip: RGB (a PNG), 10-bit,
        // 4:4:4, 4:2:2, gray and full range, and one whose format was never recorded.
        for fmt in ["rgb24", "yuv420p10le", "yuv444p", "yuv422p", "gray", "yuvj420p"] {
            let stream = StreamInfo {
                pix_fmt: Some(fmt.into()),
                ..video(640, 360)
            };
            dipped(why(stream, None, 0.5), DipWhy::Format);
        }
        let unrecorded = StreamInfo {
            pix_fmt: None,
            ..video(640, 360)
        };
        dipped(why(unrecorded, None, 0.5), DipWhy::Format);
        // A letterboxed picture: FFmpeg pads before the round trip, so the bars whiten as well.
        let portrait = video(360, 640);
        let delivery = Some(Delivery::new(640, 360, Fit::Contain));
        dipped(why(portrait.clone(), delivery, 0.5), DipWhy::Letterboxed);
        // ... but not in a still once the fade is over: no round trip is in its chain then.
        assert!(why(portrait.clone(), delivery, 5.0).is_empty());
        let plan = fading_plan_of(portrait, delivery, CompositeColorPolicy::FixedBt601, |_| {}, 0.5);

        assert!(plan.reasons(&all, plan.size(u32::MAX)).is_empty(), "no dip, no round trip");
        // A picture with an odd side (a layer scaled to 319x179): the round trip reads past it.
        let on_layer = |edit: &dyn Fn(&mut Clip)| {
            let plan = fading_plan_of(
                video(640, 360),
                None,
                CompositeColorPolicy::FixedBt601,
                |c| {
                    white(c);
                    edit(c);
                },
                0.5,
            );
            plan.reasons(&all, plan.size(u32::MAX))
        };
        dipped(on_layer(&|c| c.transform.scale = 0.4985), DipWhy::OddSize(319, 179));
        // A layer the scaler resizes, whatever the ratio: the scale filter converts to RGB itself.
        dipped(on_layer(&|c| c.transform.scale = 0.5), DipWhy::Resized);
        dipped(on_layer(&|c| c.transform.scale = 1.5), DipWhy::Resized);
        // ... where a crop that leaves the picture its size is exact, and so is a layer that is not resized.
        assert!(on_layer(&|c| {
            c.transform.crop_left = 0.25;
            c.transform.crop_right = 0.25;
        })
        .is_empty());
        // A compositor without the ability is not asked about the geometry.
        let plan = fading_plan_of(video(640, 360), None, CompositeColorPolicy::FixedBt601, white, 0.5);
        assert!(matches!(
            plan.reasons(&GpuCaps { dip_white: false, ..all }, plan.size(u32::MAX))
                .as_slice(),
            [Unsupported::DipToWhite(_, DipWhy::NoRoundTrip)]
        ));
    }

    #[test]
    fn a_full_range_picture_is_not_faded_where_ffmpeg_9_keeps_its_raw_values() {
        let all = GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            dip_white: true,
            ..GpuCaps::A0
        };
        let full = StreamInfo {
            pix_fmt: Some("yuvj420p".into()),
            ..video(640, 360)
        };
        let plan = |stream: &StreamInfo, policy, fade: f64, t: f64| {
            fading_plan_of(stream.clone(), None, policy, |c| c.fade_in = fade, t)
        };
        let reasons = |p: &RenderPlan| p.reasons(&all, p.size(u32::MAX));
        let policies = [CompositeColorPolicy::BottomLayerTag, CompositeColorPolicy::Unknown];
        for policy in policies {
            let running = reasons(&plan(&full, policy, 1.0, 0.5));
            assert!(
                matches!(running.as_slice(), [Unsupported::FullRangeNegotiated(_)]),
                "{policy:?}: {running:?}"
            );
            assert!(
                reasons(&plan(&full, policy, 1.0, 5.0)).is_empty(),
                "{policy:?}: a still with the fade over"
            );
            assert!(
                reasons(&plan(&full, policy, 0.0, 0.5)).is_empty(),
                "{policy:?}: no fade, no raw values"
            );
            assert!(
                reasons(&plan(&video(640, 360), policy, 1.0, 0.5)).is_empty(),
                "{policy:?}: a limited-range picture fades as drawn"
            );
        }
        // FFmpeg 6 converts the range at the decode: the compositor's order.
        assert!(reasons(&plan(&full, CompositeColorPolicy::FixedBt601, 1.0, 0.5)).is_empty());
        // ... unless the scaler resizes the picture, which converts the range inside the resize
        // (chroma up to 15 levels off): refused under either FFmpeg inside a fade, and drawn plain in a still.
        let resized = |stream: &StreamInfo, policy, fade: f64, t: f64| {
            fading_plan_of(
                stream.clone(),
                None,
                policy,
                |c| {
                    c.fade_in = fade;
                    c.transform.scale = 0.5;
                },
                t,
            )
        };
        for policy in [CompositeColorPolicy::FixedBt601, CompositeColorPolicy::BottomLayerTag] {
            let why = reasons(&resized(&full, policy, 1.0, 0.5));
            assert!(
                why.iter()
                    .any(|w| matches!(w, Unsupported::FullRangeResized(_, m) if m.contains("fade"))),
                "{policy:?}: {why:?}"
            );
            assert!(
                reasons(&resized(&video(640, 360), policy, 1.0, 0.5)).is_empty(),
                "{policy:?}: limited range is exact"
            );
        }
        assert!(reasons(&resized(&full, CompositeColorPolicy::FixedBt601, 0.0, 0.5)).is_empty());
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
