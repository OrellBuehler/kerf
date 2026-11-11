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
use crate::eq;
use crate::error::Result;
use crate::frame_pick::Pick;
use crate::layer_geometry::{Keyed, LayerGeometry, Placement};
use crate::model::{
    pix_fmt_is_high_bit, pix_fmt_layout, pix_fmt_subsampling, Asset, BlendMode, Clip, Color, FitOrder, Grade, Hdr, Mask,
    PixLayout, Projection, ResolvedReframe, StreamInfo, StreamKind, Subsampling, Timeline, Transform, VideoEffect,
};
use crate::perspective::{PlacementFrame, PlacementMap, Pose};
use crate::plan_caps::{
    BlendExtras, BlendKinds, DipWhy, EffectKinds, GpuCaps, GradeKinds, LayerRef, ScalerKinds, SvgWhy, Unsupported,
};
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

/// The source time a clip shows at timeline time `t`: honors speed (a ramp's map) and reverse and
/// is clamped to the asset (`asset_duration`), because a seek past the end would
/// decode nothing. It is not limited to the clip's own `source_in..source_out`: a
/// clip playing on under a transition borrows the handle beyond it.
pub(crate) fn clip_source_time(clip: &Clip, asset_duration: f64, t: f64) -> f64 {
    let off = clip.along(t - clip.timeline_start);
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
            // A title's document runs on past its out-point (a transition's tail draws it there).
            let dur = if clip.is_generated() {
                crate::model::MAX_SVG_SECONDS
            } else {
                asset_of(clip.asset_id).map(|a| a.duration).unwrap_or(clip.source_out)
            };
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
    /// otherwise is converted into it by the scaler — an arithmetic a compositor
    /// draws only with `GpuCaps::matrix_convert` (swscale's x86 kernel in, `ff_rgb24toyv12` out, measured
    /// on this FFmpeg), and only for the layers `RenderPlan::convert_refusal` accepts; without it such a stack
    /// is refused ([`Unsupported::MixedMatrices`]). The matrix of a stack whose
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

/// The steepest shrink of one scale stage the compositor draws: swscale builds a filter table up to 63:1 (a table for a shrink
/// of 64:1 or more would need a second scaler, which `table_aligned` does not make). The scalar model and the shader are held
/// to FFmpeg's own sample for sample over a sweep of shrinks up to there (`tests/scaler.rs`: 1280x720 down by 2:1 to 63:1, odd
/// outputs, the unflagged and the `bicubic` scaler) and in the frames of `tests/parity.rs`; a ratio between the sweep's steps is
/// the same arithmetic on another table, not another measurement. Past the limit the frame goes through FFmpeg.
pub const MAX_SHRINK: u32 = 63;

/// [`MAX_SHRINK`] for a delivery that names `lanczos`: its window is six source samples per output sample against bicubic's four, so
/// the table reaches swscale's 256 taps (and the second scaler it cascades) from 43.4:1 (the model has a table at every size swept
/// to 43:1 and none from 44:1). One ratio under the sweep's edge is the limit, the way 63 is for bicubic's 63.7.
pub const MAX_SHRINK_LANCZOS: u32 = 42;

/// [`MAX_SHRINK`] for a delivery that names `spline`: a window of twenty samples per output sample, trimmed; no table from 19.6:1
/// (every size swept to 19:1 has one). One under the edge, as [`MAX_SHRINK_LANCZOS`].
pub const MAX_SHRINK_SPLINE: u32 = 18;

/// The steepest shrink of one scale stage the compositor draws when the delivery names `scaler` (the engine's spelling; `None` is no name):
/// [`MAX_SHRINK`], less for the two whose windows are wider. A `neighbor` table is one tap and a `bilinear` one two, so those have
/// the limit the comparison was run to.
pub fn max_shrink_for(scaler: Option<&str>) -> u32 {
    match scaler {
        Some("lanczos") => MAX_SHRINK_LANCZOS,
        Some("spline") => MAX_SHRINK_SPLINE,
        _ => MAX_SHRINK,
    }
}

/// Whether `from` samples shrunk to `to` is steeper than `limit`:1 (in 64 bits: a hostile size times 63 overflows 32).
fn shrinks_past(from: u32, to: u32, limit: u32) -> bool {
    u64::from(from) > u64::from(limit) * u64::from(to)
}

/// Whether `from` samples shrunk to `to` is steeper than [`MAX_SHRINK`]:1.
fn shrinks_past_limit(from: u32, to: u32) -> bool {
    shrinks_past(from, to, MAX_SHRINK)
}

/// The longest side of a copy FFmpeg makes of a still that a compositor takes, and that it takes even (its planes are 4:2:0): a bound of
/// the decode (`kerf_gpu::source::MAX_SIDE` is this).
pub const MAX_COPY_SIDE: u32 = 8192;

/// The storage buffer every device has (WebGPU's default limit, 128 MiB): the per-pixel placement table of a frame over it
/// (`cw * ch * 4` bytes) is refused by the plan whatever the device, and one over a device's own smaller limit by the draw.
pub const MAX_TABLE_BYTES: u64 = 128 << 20;

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
    /// A [`PlanMode::Motion`] plan's cut holds inputs of more than one colour matrix
    /// ([`PlanStream::matrix`], the graph's BT.709 label included; RGB pictures and pictures of no known matrix
    /// aside), wherever they are on the timeline. FFmpeg 9 negotiates the matrix across the whole graph and
    /// converts every input of another matrix into the bottom layer's, so what a layer of the cut goes through
    /// is not decided by the layers on screen with it ([`composite_matrix`] judges those). Always `false` in a
    /// Still plan, whose graph is the layers on screen.
    pub mixed_matrices_cut: bool,
    /// A [`PlanMode::Motion`] plan's cut of mixed matrices under [`CompositeColorPolicy::BottomLayerTag`]: the matrix FFmpeg 9
    /// makes the composite's for every frame of the cut. Read from FFmpeg 9's format negotiation (`avfiltergraph.c`: the colour
    /// space lists of an `overlay`'s pads are one list, a `scale` has two, and each list is reduced to the single value its filter's
    /// input holds, filters in the order the graph was written, the converters it inserts coming after every one of them) and
    /// measured on 9.0.2 with real files (three clips in every combination, either matrix): a clip whose chain already ends in
    /// `yuva420p`, the format `overlay`'s second input takes, with nothing in it forcing a converter, needs none before the overlay,
    /// and its fit `scale` makes the composite's matrix its own tag; every other clip gets a converter `scale` whose input is its tag
    /// and whose output is the composite's. So the composite takes the tag of **the first such clip**, in the order the graph writes its
    /// chains (a track's clips in start-time order, tracks in list order), else of the first clip; a clip tagged otherwise is converted
    /// into it once, wherever it is on the timeline: by its converter after the chain, or inside its fit `scale` where it is on the
    /// plane. `None` outside such a cut, and in one whose deciding clip is no YCbCr picture of a known matrix (then
    /// [`Unsupported::MixedMatrices`] refuses every frame).
    pub cut_matrix: Option<YuvMatrix>,
    /// A [`PlanMode::Motion`] plan's cut holds a clip with a blend mode (`blend` is another filter, in RGB, with its own
    /// negotiation), or a clip on the alpha plane whose chain carries something that may put a converter into it (an opacity
    /// below 1, whose RGB round trip is one, a rotation, a mask, a chroma key, a colour correction, a grade, an effect, a tone map,
    /// a reprojection, a fade a placed chain runs on `yuv420p` first: only a dissolve, a keyed zoom or position and a source with
    /// an alpha channel are measured to leave none), wherever on the timeline. Which clip decides the composite's matrix ([`PlanCanvas::cut_matrix`]) was not read for such a cut, and a cut of
    /// mixed matrices holding one is [`Unsupported::MixedMatrices`] in every frame.
    pub unread_decider_in_cut: bool,
    /// A [`PlanMode::Motion`] plan's cut holds a YCbCr clip whose matrix is known and is not BT.601, wherever on the timeline.
    /// FFmpeg 9 takes an RGB picture into YCbCr with the **cut's** matrix, so a frame of the picture alone is converted with that
    /// one, where the stack on screen says BT.601: an RGB picture in such a cut is [`Unsupported::RgbInStack`] in every frame.
    pub cut_non_601: bool,
    /// The matrix tag the graph's **blend chains** name for their conversions into RGB and back (`engine::blend_chains`):
    /// the bottom layer's tag, BT.709 for a layer the graph labels so, `None` for an untagged one (which names nothing:
    /// BT.601 on every build). Decided over the whole cut for a [`PlanMode::Motion`] plan (the lowest video track's
    /// first picture) and over the layers on screen for a Still (the graph is those). It is the matrix the blend is drawn
    /// in, whatever the composite's own is ([`PlanCanvas::matrix`]).
    pub blend_tag: Option<&'static str>,
    /// The bottom layer states a matrix tag no table of the blend chain is proven for ([`crate::model::matrix_tag_is_unlisted`]:
    /// `bt2020c`, `ycgco`, `ictcp`, ...), so [`PlanCanvas::blend_tag`] is `None` for it as it is for an untagged one and the two are told
    /// apart here. FFmpeg 9 cannot export such a cut, FFmpeg 6 draws it as BT.601: a blend is drawn for neither ([`RenderPlan::blend_matrix`]).
    pub blend_tag_unlisted: bool,
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
    /// The sample range the stream declares (`pc` full, `tv` limited), `None` when it says none or the
    /// asset was probed before the tag was recorded: not known, which is not the same as limited.
    pub color_range: Option<String>,
    /// The export graph **labels this picture BT.709** (`setparams`): it is untagged HD YCbCr
    /// beside an input of the same graph that names a matrix (`model::bt709_labels`), decided over
    /// the original's stream and the inputs of the graph the plan mirrors (every clip of the cut for
    /// a Motion plan, the layers on screen for a Still). The label is the matrix FFmpeg converts
    /// the picture with inside the graph, whatever [`PlanStream::color_space`] says. `false` for
    /// the stream of any plan that did not set it.
    pub label_bt709: bool,
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
    /// (swscale's default; a `reserved` value is read as none) — and for an RGB picture, which
    /// is converted to YCbCr with that default. A picture the graph labels BT.709
    /// ([`PlanStream::label_bt709`]) is BT.709 whatever its file says.
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
        if self.label_bt709 {
            return Some(YuvMatrix::Bt709);
        }
        match self.color_space.as_deref() {
            None | Some("smpte170m") | Some("bt470bg") | Some("reserved") => Some(YuvMatrix::Bt601),
            Some("bt709") => Some(YuvMatrix::Bt709),
            Some("bt2020nc") => Some(YuvMatrix::Bt2020),
            Some(_) => None,
        }
    }

    /// The matrix tag the graph's grade hands the picture's RGB back with: the label when the graph labels the
    /// picture BT.709, else the stream's tag when it names a YCbCr matrix the grade knows
    /// ([`crate::model::frame_matrix_tag`]). `None` for an untagged picture, which is BT.601 both ways and
    /// gets no `scale` back out of RGB; a tagged one is converted back by an explicit `scale` to 4:4:4
    /// (FFmpeg 6.1 would convert back as BT.601 and leave the tag unknown), which the 4:2:0 conversion
    /// that follows turns into 4:2:0 in a second step ([`PlanLayer::grade_route`]).
    pub fn grade_tag(&self) -> Option<&'static str> {
        if self.label_bt709 {
            return Some("bt709");
        }
        crate::model::frame_matrix_tag(self.color_space.as_deref(), self.pix_fmt.as_deref())
    }

    /// The native chroma grid `crop`, `pad` and the Cover crop round to for this
    /// picture ([`pix_fmt_subsampling`]); `None` when the pixel format was never
    /// recorded or is not one whose rounding was measured.
    pub fn subsampling(&self) -> Option<Subsampling> {
        self.pix_fmt.as_deref().and_then(pix_fmt_subsampling)
    }

    /// Whether the file flags the picture full range (`color_range=pc`), in a YCbCr format. `None` and
    /// `tv` are not a flag: an asset probed before the range was recorded (schema v10) says nothing,
    /// and is judged as it always was, by its name ([`PlanStream::maybe_full_range`]). An RGB or gray
    /// picture is no flag either: ffprobe prints `pc` for every one of them on every FFmpeg (an RGB or
    /// gray format has no limited range to be told from), so the word carries nothing there.
    pub fn flagged_full_range(&self) -> bool {
        self.color_range.as_deref() == Some("pc") && matches!(self.layout(), Some(PixLayout::Yuv420 | PixLayout::OtherYuv))
    }

    /// A `yuv420p` picture flagged full range (a VP9, HEVC or AV1 file; ffprobe names the format
    /// `yuvj420p` only for the codecs whose decoder says so). Its format is the one every overlay takes, so
    /// nothing converts its range ahead of the graph's filters, and its **raw samples** reach `fade`, `eq`,
    /// `vignette`, `chromakey` and the rest on every FFmpeg (measured 6.1.1 and 9.0.2); a `yuvj420p` is converted at the
    /// decode by FFmpeg 6 and only FFmpeg 9 keeps its raw values. The compositor's decode converts to
    /// limited range first, so the two disagree wherever a filter reads the values.
    pub fn raw_full_range(&self) -> bool {
        self.flagged_full_range() && self.pix_fmt.as_deref() == Some("yuv420p")
    }

    /// Whether the picture may be full range **by its name or its flag**: ffprobe names a full-range 8-bit
    /// 4:2:0 / 4:2:2 / 4:4:4 picture `yuvj…` (a JPEG, an mjpeg, an H.264 file flagged `pc`), an
    /// asset that never recorded its format is not known not to be, and a `yuv420p` flagged `pc`
    /// ([`PlanStream::raw_full_range`]) is. The range *tag* is the truth, but an asset probed before it was
    /// recorded has none, so a deeper-than-8-bit file flagged `pc` that predates it still reads
    /// `yuv420p10le` and is not caught here; see [`PlanStream::maybe_full_range_in`].
    pub fn maybe_full_range(&self) -> bool {
        self.raw_full_range()
            || self
                .pix_fmt
                .as_deref()
                .is_none_or(|f| f.to_ascii_lowercase().starts_with("yuvj"))
    }

    /// [`PlanStream::maybe_full_range`], and under a policy that is not
    /// [`CompositeColorPolicy::FixedBt601`] (FFmpeg 9 and what could not be measured) **every YCbCr picture
    /// flagged `pc` in any format, and every deeper-than-8-bit YCbCr or gray format whose range is not
    /// recorded**: 9.0.2 handles a `pc`-tagged one as it does `yuvj` (raw values kept, the whole export
    /// flagged full range; measured Y 16 off on 10-bit), and a file probed before the flag was recorded
    /// does not say. FFmpeg 6 handled the high-bit pictures measured exactly whatever their tag, so there
    /// the name alone decides, as before; a `yuv420p` flagged `pc` is the exception it handles raw.
    pub fn maybe_full_range_in(&self, policy: CompositeColorPolicy) -> bool {
        self.maybe_full_range()
            || (policy != CompositeColorPolicy::FixedBt601
                && (self.flagged_full_range() || self.pix_fmt.as_deref().is_some_and(pix_fmt_is_high_bit)))
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
            color_range: s.color_range.clone(),
            label_bt709: false,
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
    /// the playback stream) then zooms **last** (the placement, [`Animated::places`]), after `fps`
    /// and after every effect, mask, `rotate` and fade, which act on the picture at its fit
    /// size and are magnified with it; the FFmpeg still orders a moving zoom the same way.
    /// A compositor that draws a moving zoom has to draw that order, not "zoom, then the
    /// rest" as it does a constant one, so a moving zoom in an export frame is refused until
    /// it says it does (`GpuCaps::keyed_zoom`), and in a still when something follows it. The
    /// zoom itself is [`Animated::places`], drawn by a compositor that sets `GpuCaps::placement`.
    pub zooms: bool,
    /// Some colour number is keyed: the export's `eq` is written per frame (`eval=frame`),
    /// off a colour sampled at the frame's time. A still plan samples it too and draws that
    /// colour like a static one; an export frame carries the numbers `eq` evaluates at that frame
    /// ([`PlanLayer::color`]) and is refused until the compositor says it draws them
    /// (`GpuCaps::keyed_color`). The clip has an `eq` in its chain on every frame of it, whether
    /// or not this frame's numbers are neutral (`RenderPlan::graded`).
    pub color: bool,
    /// Some number of the mask is keyed: the export's mask `geq` reads it per frame. A still plan
    /// draws the mask sampled at its instant ([`Clip::mask_at`]) as a static one; an export frame
    /// is refused (no compositor pass draws a moving mask yet).
    pub mask: bool,
    /// The crop is keyed ([`Clip::crop_animated`]): the export and the FFmpeg still read the
    /// picture through its window with a `perspective` (bilinear, fixed-point), which no
    /// compositor pass draws, so every frame of it is refused.
    pub crop: bool,
    /// A still image whose picture moves ([`Clip::picture_moves`]): the export and the FFmpeg still
    /// draw its fit, zoom, position and crop in one `perspective` of a copy of the still scaled once
    /// (`engine::cli::still_motion`), which no compositor pass draws, so it is refused.
    pub still_motion: bool,
    /// The zoom or the position moves ([`Clip::placement_animated`]): the export, the playback stream
    /// and the FFmpeg still place the picture on the canvas with a `perspective` at sub-pixel
    /// positions (`pad`, `perspective`, `crop`: [`crate::perspective`]). A compositor that sets
    /// `GpuCaps::placement` reproduces that to the sample ([`RenderPlan::pose`] is the pose it draws); without it
    /// every frame of such a layer is refused (`Unsupported::KeyedPlacement`).
    pub places: bool,
    /// The clip has a live behaviour ([`Clip::has_behaviors`]: a sway, breath, spin, drift or shake on
    /// top of its pose). The plan samples the exact pose it draws ([`PlanLayer::transform`], the
    /// behaviours included) and the other flags here say what the pose's chain is. A breath, a drift and
    /// a shake move the zoom or the position, which the placement draws, so a compositor with
    /// `GpuCaps::behaviors` takes them; one that turns the picture (a sway, a spin: [`Animated::rotates`]) is a rotation,
    /// which no pass draws to the sample. Every frame of a layer that a compositor does not take is refused
    /// (`Unsupported::Behavior`).
    pub behaves: bool,
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
    /// Speed magnitude: the clip's mean over its length for a speed ramp.
    pub speed: f64,
    pub reversed: bool,
    /// The speed moves over the clip (`Clip::speed_ramp`): `speed` is its average, the pick names no
    /// frame, and the compositor refuses the layer ([`Unsupported::SpeedRamp`]).
    pub ramped: bool,
    /// The clip is stabilized (`Clip::stabilize`): each frame is moved by its analysis, which the
    /// compositor does not draw ([`Unsupported::Stabilized`]).
    pub stabilized: bool,
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
    /// The colour correction at this frame. A keyed colour is `Clip::color_at` in a still plan (drawn
    /// like a static one); in a Motion plan it is what the export's `eq` evaluates at the `fps` grid's
    /// time (`PlanCanvas::pick_fps`, `engine::keyed_color_value`: each keyed number through the
    /// expression's own arithmetic, the others the clip's), which is what `eq` builds its tables from.
    pub color: Color,
    /// The clip's curves, wheels and LUT. No compositor draws them yet: a layer with any of them
    /// is refused ([`Unsupported::Grade`]).
    pub grade: Grade,
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
    /// How the clip composites onto the layers below. Anything but `Normal` is FFmpeg's `blend` in RGB, which a compositor
    /// draws for the modes and the layers its caps name (`GpuCaps::blend`, `GpuCaps::blend_extras`) and otherwise leaves to
    /// FFmpeg: [`Unsupported::BlendMode`] with its cause.
    pub blend: BlendMode,
    pub reframe: Option<PlanReframe>,
    pub fx: LayerFx,
    /// `Some` for a clip with a keyed transform or colour number.
    pub animated: Option<Animated>,
    /// A still image on a Ken Burns move that the export takes through its own route ([`StillLayer`]); `None` for
    /// every other layer, including a moving still the general chain draws (a zoom or pan that leaves the frame
    /// bare: the placement).
    pub still: Option<StillLayer>,
    /// The picture ahead of a moving zoom's placement, as FFmpeg makes it ([`FitCopy`]): for a still image whose zoom or position moves and
    /// whose chain is only the fit (no colour, grade, effect, mask, rotation or opacity), where the general chain scales, crops and
    /// converts it ahead of the placement and the compositor has FFmpeg do those, instead of its own scaler and decode.
    pub fit: Option<FitCopy>,
    /// How the graph fits the picture to the frame and applies the clip's static zoom
    /// ([`Clip::fit_order`]; `Fitted` for a still that samples a pose to the identity, which the
    /// still draws as the plain fit). Anything but `Fitted` takes the picture from the source in
    /// one resample, which no compositor pass draws: [`Unsupported::FoldedFit`].
    pub fit_order: FitOrder,
    /// The alpha the export's `geq` writes at this frame for an opacity keyed below 1:
    /// `(int)(opacity(T) * 255)`, the curve evaluated in the expression's own arithmetic at the
    /// clip's `fps` time. `None` in a still plan, and for a clip whose opacity has no key below 1.
    pub keyed_alpha: Option<u8>,
    /// The curve's value itself, in `f64`, that [`PlanLayer::keyed_alpha`] truncates (`opacity(T)`): under a mask the one `geq`
    /// multiplies it with the mask's own `f64` keep before it truncates (`(keep)*(opacity(T))*alpha(X,Y)`), which no byte alpha can
    /// give back. `Some` exactly where `keyed_alpha` is.
    pub keyed_opacity: Option<f64>,
    /// The clip takes the delivery's blurred fill ([`Fit::Blur`]) wherever its picture does not fill the frame: set
    /// when the canvas fits over a blurred copy and the clip is placed by the plain fit, not see-through
    /// (`engine::cli::blur_fill_clip`). The size is the picture the graph judges the frame's shape by (the original's
    /// displayed size, whichever file is decoded); whether the picture leaves bars is the render size's to say
    /// ([`RenderPlan::blur_fill`]).
    pub blur_fill: Option<(u32, u32)>,
    pub timing: PlanTiming,
}

/// The route a moving still takes through the graph ([`crate::still_motion`]): the numbers that make its scaled copy
/// and its `perspective`, as the export (the clip's frames) or the FFmpeg still (this instant) write them.
#[derive(Debug, Clone, PartialEq)]
pub struct StillLayer {
    /// What does not change over the clip: the picture, the fit, the static crop.
    pub stat: crate::still_motion::StillStatic,
    /// The clip's numbers at this instant (the zoom, the position, the crop's window).
    pub sample: crate::still_motion::StillSample,
    /// The numbers `perspective`'s expressions evaluate to at this frame.
    pub pose: crate::still_motion::StillPose,
    /// The delivery's scaler flags for the scaled copy (`:flags=...`, empty when it names none).
    pub flags: String,
    /// The move as the graph draws it: an export frame's is the clip's own, made over every frame it is drawn on at the
    /// delivery's frame; a still's is made for this instant at the size it renders at, which is not known when the cut is
    /// planned ([`RenderPlan::for_render`] works it out; until then it is `None` and nothing is drawn from the layer).
    pub export: Option<crate::still_motion::StillMotion>,
    /// `export` is the clip's own (an export frame's plan), not one made for a render.
    pub clip_wide: bool,
}

impl StillLayer {
    /// The move at a render of `size`, or `None` where the graph does not take this route there: an export frame is drawn at
    /// the delivery's frame only, and the still at `size` makes its scaled copy for the one instant it draws
    /// (`StillMotion::build`), which can fail where the clip's frames would not.
    pub fn geometry(&self, size: (u32, u32)) -> Option<crate::still_motion::StillMotion> {
        match &self.export {
            // An export frame's move is the delivery's, and so is the still's once worked out for a render: both are drawn at the
            // frame they were made for. (A still plan not yet worked out for a size is judged by what the render would make.)
            Some(m) => (m.frame == size).then(|| m.clone()),
            None => crate::still_motion::StillMotion::build(&self.stat, size, &[self.sample]),
        }
    }

    /// The filters that scale the still once and cut the window any frame reads, as the graph writes them.
    pub fn once(&self, motion: &crate::still_motion::StillMotion) -> String {
        motion.once(&self.flags)
    }
}

/// The picture a still's placement is laid from, as FFmpeg makes it: the scale(s) and crop(s) of the chain's fit, written out at the
/// sizes the layer's geometry says, then the conversion to `yuva420p` the graph's negotiation gives the chain. FFmpeg makes it
/// (once, for a still) in the same pass that resizes it, so a resize of a picture that is not 8-bit 4:2:0 (RGB, full range), the
/// chroma grid of a crop and the scaler's own arithmetic are FFmpeg's and not the compositor's.
#[derive(Debug, Clone, PartialEq)]
pub struct FitCopy {
    /// The filters, from the file's picture to the one the placement reads (without the conversion).
    pub filters: String,
    /// The shape of that text: the engine's own for a Cover frame (behind a static crop or not), the geometry's `scale` stages after a
    /// guard, or those stages with a `crop` of their own besides the guard (`Cropped`: a text no toy cut of a probe takes, drawn by none).
    pub shape: crate::plan_caps::FitShape,
    /// The picture they make.
    pub size: (u32, u32),
    /// The frame it was made for: the render's size (an export frame's is the delivery's).
    pub frame: (u32, u32),
}

/// How FFmpeg carries a graded layer's picture through the RGB filters and back, for a picture whose route is
/// reproduced ([`PlanLayer::grade_route`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GradeRoute {
    /// The matrix the picture is converted out of YCbCr with and back into: the frame's own tag, BT.601 for a
    /// picture with none (`smpte170m` and `bt470bg` are BT.601's coefficients).
    pub matrix: YuvMatrix,
    /// The graph ends the grade in an explicit `scale` back to YCbCr 4:4:4 (a tagged picture: FFmpeg 6.1 would
    /// otherwise convert back as BT.601 and drop the tag), which the next filter's conversion turns into 4:2:0
    /// with a scale of its own. An untagged picture goes `rgb24` straight back to 4:2:0.
    pub via_444: bool,
}

impl PlanLayer {
    /// The route of this layer's grade, or `None` for a picture whose colour matrix has no measured round
    /// trip (BT.2020, `fcc`, `smpte240m`).
    pub fn grade_route(&self) -> Option<GradeRoute> {
        let tag = self.stream.grade_tag();
        let matrix = match tag {
            None | Some("smpte170m" | "bt470bg") => YuvMatrix::Bt601,
            Some("bt709") => YuvMatrix::Bt709,
            Some(_) => return None,
        };
        Some(GradeRoute {
            matrix,
            via_444: tag.is_some(),
        })
    }
}

/// Why a layer's alpha plane (a mask's `geq`, a chroma key's `chromakey`) is refused beside what the layer also does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AlphaPlaneRefusal {
    /// A rotation turns the plane with the picture, and a partly transparent edge with it.
    Rotated,
    /// A blend mode on a layer below full opacity: the blend chain's probe carries the opaque plane of a constant opacity alone.
    BlendedBelowFull,
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
    /// The animated SVG titles on screen at this frame, in the order they are laid (under the
    /// text). A floating title carries its picture's description ([`crate::SvgTitle`]: the artwork, the fonts, the
    /// placement, the opacity and the document's clock at this frame) and is drawn by a compositor that measured FFmpeg's
    /// `overlay` of a picture ([`GpuCaps::svg`]; [`Unsupported::SvgOverlay`] otherwise). A title clip on screen (a generated clip on
    /// a video track) is named here too, by its clip id, after the floating ones, with no picture, and is not a layer: no
    /// compositor draws it ([`Unsupported::Svg`]) and FFmpeg does, from the pictures resvg made.
    pub svg: Vec<crate::PlanSvg>,
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

/// What of a clip's [`Grade`] a compositor with `caps` does not draw, or `None` for an identity grade or one it
/// draws every part of.
fn grade_refusal(grade: &Grade, caps: &GpuCaps) -> Option<&'static str> {
    if grade.lut().is_some() && !caps.grade.contains(GradeKinds::LUT) {
        Some("a 3D LUT")
    } else if grade.curves().is_some() && !caps.grade.contains(GradeKinds::CURVES) {
        Some("curves")
    } else if grade.wheels().is_some() && !caps.grade.contains(GradeKinds::WHEELS) {
        Some("colour wheels")
    } else {
        None
    }
}

/// Why a compositor with `caps` does not draw `effect`: it is not one it draws (the detail says
/// what it is, and why that one is hard), or its value is one FFmpeg's own filter refuses (`gblur`
/// takes a sigma of 0 to 1024, `unsharp` an amount of -2 to 5), so the frame goes to FFmpeg, which
/// fails it the same way.
fn effect_refusal(effect: &VideoEffect, caps: &GpuCaps) -> Option<&'static str> {
    // (A blur of sigma 0 is `gblur`'s identity on every build: `set_params` makes `nu` 0 and both scales 1.)
    let identity_blur = matches!(effect, VideoEffect::Blur { sigma } if *sigma == 0.0);
    if !caps.effects.contains(EffectKinds::of(effect)) && !identity_blur {
        return Some(match effect {
            VideoEffect::Blur { .. } => "a blur",
            VideoEffect::Sharpen { .. } => "a sharpen",
            VideoEffect::Grayscale => "grayscale",
            VideoEffect::Invert => "invert",
            VideoEffect::Vignette => "a vignette",
            VideoEffect::ChromaKey { .. } => "a chroma key",
        });
    }
    match effect {
        VideoEffect::Blur { sigma } if !(sigma.is_finite() && (0.0..=1024.0).contains(sigma)) => {
            Some("a blur sigma outside 0 to 1024, which gblur refuses")
        }
        VideoEffect::Blur { sigma } if crate::gblur::gaussian(*sigma).is_none() => {
            Some("a blur sigma whose coefficients gblur's own arithmetic leaves outside what the compositor computes (a tiny sigma: its `nu` cancels to above 1)")
        }
        VideoEffect::Sharpen { amount } if !(amount.is_finite() && (-2.0..=5.0).contains(amount)) => {
            Some("a sharpen amount outside -2 to 5, which unsharp refuses")
        }
        VideoEffect::ChromaKey { similarity, blend, .. }
            if !(similarity.is_finite()
                && (0.00001..=1.0).contains(similarity)
                && blend.is_finite()
                && (0.0..=1.0).contains(blend)) =>
        {
            Some("a chroma key similarity outside 0.00001 to 1 or blend outside 0 to 1, which chromakey refuses")
        }
        _ => None,
    }
}

/// The frame a layer's blur, sharpen and vignette run on: the padded one of a letterbox (bars included), else the picture.
fn effect_frame(geom: &LayerGeometry) -> (u32, u32) {
    if geom.matte {
        geom.layer
    } else {
        geom.picture
    }
}

/// The fewest rows a frame has for `unsharp` to be the model's: it is exact from 8 rows (measured on 6.1.1 and 9.0.2, whatever the width) and not
/// on 2, 4 or 6, with bars or without.
pub const UNSHARP_MIN_ROWS: u32 = 8;

/// Whether a grade on a letterbox has no `eq` ahead of it and the picture sits at an odd offset on either axis (`(canvas - picture) / 2` odd),
/// where FFmpeg's `pad` runs in RGB and keeps the odd offset. Not over a blurred fill (composed in YUV, exact at any offset).
fn letterbox_offset_is_odd_without_eq(plan: &RenderPlan, layer: &PlanLayer, geom: &LayerGeometry) -> bool {
    let odd = |canvas: u32, picture: u32| (canvas.saturating_sub(picture) / 2) % 2 == 1;
    geom.matte
        && layer.blur_fill.is_none()
        && !plan.eq_in_chain(layer)
        && (odd(geom.layer.0, geom.picture.0) || odd(geom.layer.1, geom.picture.1))
}

/// The largest angle, in radians, [`quarter_turn`] looks at: past it `rotate`'s `int` angle (`res * 65536 * 16`) is out of range (a conversion
/// C leaves undefined, and the builds do differently), and `int_sin`'s argument could overflow its own `int`.
const MAX_TURN_RADIANS: f64 = 1000.0;

/// The `rotate` filter's `int_sin` (`vf_rotate.c`, the same in FFmpeg 6.1 and 9.0): the sine of `a / 2^20` radians as `2^16` times a Taylor
/// series to the fifth term, in integers. It is **not** the sine: near a multiple of a quarter turn it comes out a unit off (`c = 1`,
/// `s = 65536` at 41 quarters), and the picture then lands between samples.
fn rotate_int_sin(mut a: i64) -> i64 {
    const FIXP2: i64 = 1 << 20;
    const INT_PI: i64 = 3_294_199;
    if a < 0 {
        a = INT_PI - a;
    }
    a %= 2 * INT_PI;
    if a >= INT_PI * 3 / 2 {
        a -= 2 * INT_PI;
    }
    if a >= INT_PI / 2 {
        a = INT_PI - a;
    }
    let a2 = (a * a) / FIXP2;
    let mut res = 0;
    for i in [2, 4, 6, 8, 10] {
        res += a;
        a = -a * a2 / (FIXP2 * i * (i + 1));
    }
    (res + 8) >> 4
}

/// How many quarter turns clockwise (0 to 3) `rotate` turns a picture by when its angle is `angle` radians, if it lands on whole samples
/// there; `None` for any angle it does not, which the compositor does not draw (the filter's edge is a fixed-point sample against a float one).
///
/// The filter has two paths, and an angle is a quarter turn only where one of them is exact:
/// * its `simple_rotate` fast path, taken when the angle is within `FLT_EPSILON` of 0, π/2, π or 3π/2 (and the box is the picture's, or its
///   swap: `rotw` / `roth` come out so for any angle this near);
/// * its general path, which walks the output with `c` and `s`, **`int_sin` of the angle truncated to `res * 65536 * 16`**
///   ([`rotate_int_sin`]). Only where those come out as exactly 0 and ±65536 does every output sample read one input sample whole. They do
///   for the multiples of 90 degrees from -43 to 40 quarters and then here and there (41, 42 and 45 on are a unit off, and 43, 44, -46, -47
///   are not): the error of the series and of `INT_PI` grows with the angle, so no range is claimed past what the arithmetic says, and no
///   angle past [`MAX_TURN_RADIANS`] (about 636 quarters), where the conversion to `int` is not defined.
///
/// The answer of FFmpeg 6.1.1 and 9.0.2 for every quarter turn from -1310 to 1310 is this function's (`tests/checker_e_turn.rs`, against the
/// export's own `rotate` text on pictures of odd and even sides; the many-turns cases of `tests/motion_parity.rs` hold the plan to it). An
/// angle must also be within `FLT_EPSILON` radians (the filter's own tolerance for its fast path, 1.19e-7) of a multiple of a quarter turn to
/// count: both paths are exact there, by the arithmetic above, and 90.00001 degrees (1.7e-7 radians off) is a rotation like 91 is, whatever the
/// integer arithmetic makes of it.
pub fn quarter_turn(angle: f64) -> Option<u8> {
    use std::f64::consts::{FRAC_PI_2, PI};
    if !angle.is_finite() || angle.abs() > MAX_TURN_RADIANS {
        return None;
    }
    let flt_epsilon = f64::from(f32::EPSILON);
    let nearest = (angle / FRAC_PI_2).round();
    if (angle - nearest * FRAC_PI_2).abs() >= flt_epsilon {
        return None;
    }
    let turn = nearest.rem_euclid(4.0) as u8;
    // The fast path: `fabs(angle - target) < FLT_EPSILON`, for the four targets in [0, 2π) only.
    if [0.0, FRAC_PI_2, PI, 3.0 * PI / 2.0]
        .iter()
        .any(|target| (angle - target).abs() < flt_epsilon)
    {
        return Some(turn);
    }
    // The general path: `angle_int = res * FIXP * 16` (a power of two: exact), `s = int_sin(angle_int)`, `c = int_sin(angle_int + INT_PI / 2)`.
    let angle_int = (angle * 65536.0 * 16.0) as i64;
    let (s, c) = (rotate_int_sin(angle_int), rotate_int_sin(angle_int + 3_294_199 / 2));
    let want = [(65536, 0), (0, 65536), (-65536, 0), (0, -65536)][usize::from(turn)];
    ((c, s) == want).then_some(turn)
}

/// Whether a clockwise `angle` in radians is a quarter turn, half a turn or three quarters that the `rotate` filter lands on whole samples
/// for: [`quarter_turn`] has an answer.
pub fn is_quarter_turn(angle: f64) -> bool {
    quarter_turn(angle).is_some()
}

/// The worst-case error, in levels, the plan allows a one-level difference from FFmpeg's upstream to grow to through what is drawn after
/// it: the steepest step of a grade's tables (`eq::table_steps`) and the sharpens (which start it at one, or at the grade's step). The
/// only upstream difference left is a **scaler that is not measured to be swscale's** (`GpuCaps::exact_scaler` false: the compositor then
/// draws the C kernel, a level from the x86 one); where the scaler is measured there is nothing to amplify and none of this applies, and a
/// blur is no source: it is `gblur`'s own recursion to the sample on the planes its kernel is the recursion for (`gblur::KernelHazard`
/// refuses the rest), or not in the caps at all. Two is the bound of a gentle grade on a scaled layer; one sharpen of 0.5 reaches it
/// exactly.
pub const MAX_AMPLIFIED_ERROR: u32 = 2;

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
        // `vignette` works on the picture in its own format: another layout has other planes (and its dither,
        // one step per sample, runs another course), RGB another arithmetic, a deeper one another range.
        VideoEffect::Vignette
            if stream.pix_fmt.as_deref() != Some("yuv420p") || stream.maybe_full_range_in(policy) =>
        {
            Some(if stream.pix_fmt.as_deref() == Some("yuv420p") {
                "a vignette on a yuv420p picture flagged full range (color_range=pc: FFmpeg vignettes its raw values)"
            } else {
                "a vignette on a picture that is not 8-bit 4:2:0 yuv420p (FFmpeg vignettes it in its own format)"
            })
        }
        // `chromakey` reads the chroma of the `yuva420p` FFmpeg converts the picture to; the compositor's is the
        // decode's 4:2:0, the same picture only when that is what it was.
        VideoEffect::ChromaKey { .. }
            if stream.pix_fmt.as_deref() != Some("yuv420p") || stream.maybe_full_range_in(policy) =>
        {
            Some(if stream.pix_fmt.as_deref() == Some("yuv420p") {
                "a chroma key on a yuv420p picture flagged full range (color_range=pc: FFmpeg keys its raw chroma)"
            } else {
                "a chroma key on a picture that is not 8-bit 4:2:0 yuv420p (FFmpeg keys the chroma it converts it to)"
            })
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
    /// `Display` is the message. Ask it before decoding anything. **Every** cause is listed, not the
    /// first that was found (a backlog of refusals ranks them by what lifting one alone would draw): a
    /// layer whose geometry is refused still gets its other reasons, judged on the picture's own chroma
    /// grid, and a grade on a letterboxed layer the scaler resizes is a `Matte` and a `GradeWith`.
    pub fn reasons(&self, caps: &GpuCaps, size: (u32, u32)) -> Vec<Unsupported> {
        self.reasons_over(caps, size, &crate::cpu_graph_thread_range)
    }

    /// [`RenderPlan::reasons`] for a machine whose graphs can run on the slice threads `machine` says (the tests give it one that is not
    /// the host's).
    fn reasons_over(&self, caps: &GpuCaps, size: (u32, u32), machine: &dyn Fn() -> (usize, usize)) -> Vec<Unsupported> {
        let mut reasons = self.size_free_reasons(caps);
        // (The slice threads the graph runs on: asked of the machine once for the whole judgement, and only if no pin and no single frame
        // of cheap inputs decides them, so the layers of one judgement see one range.)
        let threads = std::cell::OnceCell::new();
        for (n, layer) in self.layers.iter().enumerate() {
            let geom = match self.layer_geometry_resolved(layer, size) {
                Ok(g) => g,
                Err(e) => {
                    reasons.push(Unsupported::Geometry(n, e.to_string()));
                    // The frame is refused whatever follows. The layer's other reasons are still listed, judged
                    // on the geometry of the picture's own chroma grid (4:2:0 where it is not known), so that a
                    // ranking of reasons sees what else refuses the layer once this one is lifted. A geometry
                    // that cannot be made on any grid leaves nothing to judge them by.
                    match self.native_grid_geometry(layer, size) {
                        Some(g) => g,
                        None => continue,
                    }
                }
            };
            // The export draws a moving still at the delivery's frame; the FFmpeg still makes its scaled copy for the instant
            // and the size it renders, which can leave the route (the general chain draws it there).
            if self.takes_still_route(layer) {
                match layer.still.as_ref().and_then(|s| s.geometry(size).map(|m| (s, m))) {
                    None => reasons.push(Unsupported::StillMotion(
                        self.layer_ref(n, layer),
                        format!(
                            "at a render of {}x{} (an export frame's scaled copy is made for the delivery's frame, and the still makes its own for the size it renders)",
                            size.0, size.1
                        ),
                    )),
                    // What the draw would refuse of the copy and the move at this size, decided here so that a span and the render bar
                    // do not promise it (the draw-time guards stay).
                    Some((still, motion)) => {
                        if let Some(why) = Self::still_draw_refusal(still, &motion, size) {
                            reasons.push(Unsupported::StillMotion(self.layer_ref(n, layer), why));
                        }
                    }
                }
            }
            // The blurred fill behind a picture that leaves bars: FFmpeg's thumbnail chain, drawn by the compositor
            // where it can, to the level (`GpuCaps::blur_fill`, with an exact scaler under it).
            if geom.matte {
                if let Some(why) = self.blur_fill_refusal(caps, layer, size) {
                    let layer_ref = self.layer_ref(n, layer);
                    match why {
                        Some(detail) => reasons.push(Unsupported::BlurFillWith(layer_ref, detail)),
                        None if !reasons.contains(&Unsupported::BlurFill) => reasons.push(Unsupported::BlurFill),
                        None => {}
                    }
                }
            }
            if let OddPad::Refused(why) = self.odd_pad(layer, &geom, size) {
                reasons.push(Unsupported::Geometry(n, why));
            }
            // A `yuv420p` picture flagged full range, letterboxed, in an export: `pad` draws its black in the range the picture
            // is flagged (limited black, 16), and the graph converts the picture's range to the overlay's afterwards when this
            // FFmpeg does not negotiate it (the policy probe says which: only `BottomLayerTag` is measured to leave the bars
            // alone): the bars read Y 30 where the compositor's matte is 16 (14 levels, the picture itself exact). A still's `scale`
            // converts the range ahead of the `pad`, and its bars are 16.
            if self.mode == PlanMode::Motion
                && geom.matte
                && layer.stream.raw_full_range()
                && self.canvas.policy != CompositeColorPolicy::BottomLayerTag
            {
                reasons.push(Unsupported::Geometry(
                    n,
                    "a letterboxed `yuv420p` picture flagged full range: this FFmpeg's export converts its range after the `pad` has drawn the bars in limited black (they read Y 30 where the compositor's are 16)".into(),
                ));
            }
            // `colorchannelmixer` leaves the picture in RGBA, `rotate` turns it there, and the way back to 4:2:0 comes after the turn:
            // swscale's chroma pairs two pixels of a row and filters over the rows, in a window that is not symmetric (a different sum
            // each way), so after a quarter turn it pairs other samples than the compositor's round trip, which comes before the turn,
            // does (chroma tens of levels off on the cases measured), and after a half turn it filters the rows the other way round (one
            // level of chroma on about a sixth of a percent of the samples: 4,631 of 2.1 million on `opacity/turned 180`, 6.1.1).
            // An upright layer is exact. What an exact route needs: turn the RGB texture ahead of the way back and compose the planes
            // it makes without turning them again.
            if geom.translucent
                && geom
                    .rotation
                    .is_some_and(|r| quarter_turn(r.angle).is_some_and(|turn| turn != 0))
            {
                reasons.push(Unsupported::Rotation(
                    self.layer_ref(n, layer),
                    "a turn of a layer below full opacity (FFmpeg turns the RGBA picture and converts it to 4:2:0 afterwards: the chroma pairs other samples, and the rows are filtered the other way round)",
                ));
            }
            // The round trip a constant opacity below 1 takes (`colorchannelmixer` only takes RGB) is the compositor's only where this
            // FFmpeg's conversions were measured to be the model's: otherwise it is a level of chroma off, and FFmpeg's.
            if caps.translucent_approximate && self.through_rgba(layer) {
                reasons.push(Unsupported::TranslucentApproximate(self.layer_ref(n, layer)));
            }
            // ... and a picture with an odd side takes other routes through swscale (the unscaled converter leaves an odd width's
            // last column unwritten, an odd height takes the scaler, and the way back reads the pixel past an odd row on 6.1 and
            // scales the row on 9.0): the compositor reproduces them where its probe found the build's, for the layers it was held to.
            if let Some(size) = geom.translucent_odd() {
                if let Some(why) = self.translucent_odd_refusal(caps, layer, &geom, size) {
                    reasons.push(Unsupported::Geometry(n, why));
                }
            }
            // A scaler that is not measured to be swscale's draws a resized layer a level off: not exact, so FFmpeg's.
            if caps.scaler_approximate && geom.stages.iter().any(|s| s.scaled != (s.src.w, s.src.h)) {
                reasons.push(Unsupported::ScalerApproximate(self.layer_ref(n, layer)));
            }
            // A layer converted into the composite's matrix is converted on a 4:2:0 grid: an odd side reads past the line, and
            // bars that meet the picture off the grid share a chroma block with it (the conversion averages a 2x2 block).
            if self.canvas.policy == CompositeColorPolicy::BottomLayerTag
                && caps.matrix_convert
                && !layer.stream.is_rgb()
                && layer.stream.matrix().is_some_and(|m| m != self.canvas.matrix)
                && self.convert_refusal(layer).is_none()
            {
                // A still's graph has no `format` ahead of the overlay, and a dissolving clip's chain ends in `format=yuva420p`
                // (an alpha plane): the conversion is made by the chain's last `scale` itself, in one pass with the resize, which is
                // not the conversion of the picture it made (measured on 9.0.2: 47398 of 230400 luma samples differ on a half-size
                // picture-in-picture; a dissolving phone clip shrunk into a landscape frame, 22 % of the luma, up to 17 levels). A plain
                // export chain ends in `format=yuv420p` and the conversion is a `scale` of its own after the chain.
                if (self.mode == PlanMode::Still || self.on_alpha_plane(layer))
                    && geom.stages.last().is_some_and(|s| s.scaled != (s.src.w, s.src.h))
                {
                    if let Some(why) = self.scaled_convert_refusal(layer, &geom, caps) {
                        reasons.push(Unsupported::MixedMatricesWith(self.layer_ref(n, layer), why));
                    }
                }
                let off_grid = |(w, h): (u32, u32)| w % 2 != 0 || h % 2 != 0;
                // Over a blurred fill the converted picture is the whole filled frame (its size is the layer's); else it is the picture,
                // whose 2x2 blocks never share a sample with the bars only when it and its offset are even.
                let filled = layer.blur_fill.is_some() && geom.matte;
                // The bars of a letterbox are black, which no matrix changes, unless a pointwise effect has been through them (invert
                // makes them 239, 127, 127): converted with the picture they would have to be materialized.
                let effect_on_bars = geom.matte && !filled && !layer.effects.is_empty();
                if off_grid(geom.layer)
                    || (!filled && (off_grid(geom.picture) || (geom.matte && off_grid(geom.picture_at))))
                    || effect_on_bars
                    || geom.rotation.is_some()
                {
                    reasons.push(Unsupported::MixedMatricesWith(
                        self.layer_ref(n, layer),
                        "a picture of odd size, whose bars meet it off the 4:2:0 grid or carry an effect",
                    ));
                }
            }
            // The placement reads the picture through `pad` and `perspective`: an odd height is not the
            // filters' (see `PlacementFrame::exact_for`), and a pose whose sample positions leave the
            // fixed-point table's range is not either.
            if caps.placement {
                if let Some(pose) = self.pose(layer) {
                    if !PlacementFrame::exact_for(geom.picture) {
                        reasons.push(Unsupported::Geometry(
                            n,
                            format!(
                                "an odd-height picture ({}x{}) under a placement (FFmpeg's pad keeps or loses its last row by how the graph allocated the frame, and the builds differ)",
                                geom.picture.0, geom.picture.1
                            ),
                        ));
                    } else if !PlacementFrame::new(geom.picture, size).reachable(&pose) {
                        reasons.push(Unsupported::Geometry(
                            n,
                            "a placement whose sample positions leave the fixed-point range of FFmpeg's perspective filter"
                                .into(),
                        ));
                    }
                }
            }
            // `pad` hands everything after it the whole padded frame, bars included. A pointwise
            // effect (a grade, grayscale, invert) is a table, and the bars go through it like the
            // picture, and a mask is cut from the whole padded frame (the compositor's layer is that
            // frame); a blur, a sharpen and a vignette see the bars as picture, and run on a frame of the canvas' size
            // (the vignette's centre is the frame's, a blur runs the bars into the edge). A compositor that lays the frame out whole
            // (`caps.matte_effects`) draws them where the chain is one it was held to (`padded_refusal`); a chroma key
            // writes an alpha plane from that frame, and is FFmpeg's.
            if geom.matte {
                let padded = caps.matte_effects;
                let why = padded.then(|| self.padded_refusal(layer)).flatten();
                for effect in &layer.effects {
                    let what = match effect {
                        VideoEffect::Blur { .. } => "a blur",
                        VideoEffect::Sharpen { .. } => "a sharpen",
                        VideoEffect::Vignette => "a vignette",
                        VideoEffect::ChromaKey { .. } => "a chroma key",
                        _ => continue,
                    };
                    if matches!(effect, VideoEffect::ChromaKey { .. }) || !padded {
                        reasons.push(Unsupported::Matte(self.layer_ref(n, layer), what));
                        break;
                    }
                    if let Some(why) = why {
                        reasons.push(Unsupported::Effects(self.layer_ref(n, layer), why));
                        break;
                    }
                }
            }
            // A grade runs in RGB on the picture at the size the chain has made it, and what the compositor
            // reproduces is that conversion at the picture's own size: the scaler's one-level difference from
            // swscale's (its vertical filter has an x86 kernel the shader does not follow) is carried through the
            // conversions and a table (an identity grade turns it into 3 levels of luma and 2 of chroma on noise,
            // gamma and curves into 4), so a layer the compositor resizes is FFmpeg's. The bars of a letterbox
            // are part of the padded frame the grade sees (they are graded, and their chroma meets the
            // picture's in the conversion back), an odd side makes the conversions read past the line.
            if !layer.grade.is_identity() && grade_refusal(&layer.grade, caps).is_none() {
                let at = || self.layer_ref(n, layer);
                // Every cause is listed, not the first: lifting one must show the next.
                if geom.matte && !(caps.matte_effects && self.padded_refusal(layer).is_none()) {
                    reasons.push(Unsupported::Matte(at(), "a grade"));
                }
                if geom.stages.iter().any(|s| s.scaled != (s.src.w, s.src.h)) {
                    if !caps.exact_scaler {
                        reasons.push(Unsupported::GradeWith(
                            at(),
                            "on a layer the compositor's scaler resizes (its one-level difference from swscale's grows to 3 or 4 levels through the RGB conversions)",
                        ));
                    } else if !self.eq_in_chain(layer) {
                        reasons.push(Unsupported::GradeWith(
                            at(),
                            "on a layer the scaler resizes with no `eq` ahead of the grade (the fit `scale` converts to RGB in the same pass, from chroma the compositor has already scaled)",
                        ));
                    }
                }
                if geom.picture.0 % 2 == 1 || geom.picture.1 % 2 == 1 {
                    reasons.push(Unsupported::GradeWith(
                        at(),
                        "on a picture with an odd side (swscale's conversions to and from RGB read past the line)",
                    ));
                }
                // With no `eq` ahead of the grade the fit `scale` outputs RGB, so `pad` runs in RGB, where `ff_draw_round_to_sub` rounds nothing and the
                // picture sits at the unrounded `(ow - iw) / 2`; the compositor pads at the even offset (the YUV `pad`'s). A fill is composed in YUV.
                if letterbox_offset_is_odd_without_eq(self, layer, &geom) {
                    reasons.push(Unsupported::GradeWith(
                        at(),
                        "on a letterbox with no `eq` ahead of it at an odd offset (the fit `scale` outputs RGB and `pad` keeps the picture at the odd offset, where the compositor pads at the even one)",
                    ));
                }
            }
            // A sharpen is exact, but what it sharpens is exactly FFmpeg's only where the scaler is: a compositor whose
            // scaler is not measured to be swscale's draws the C kernel, a level off the x86 one. `unsharp`
            // (`s + (((s - blur) * amount) >> 16)`) carries that level through at worst `1 + 2 * amount` levels of luma at
            // an edge sample (3 at 1.5, 7 at 3, 11 at 5), and the `>> 16` floors, so every sharpen after the first adds up
            // to a level of its own however small its amount (two of 0.2 reach 3, three of 0.1 reach 4). From the scale on
            // (a scale ahead of the effects: every stage, or the fit scale when a moving zoom comes last) the worst case is
            // followed through the sharpens that count, `err += ceil(2 * err * |amount|)` from 1 (from the luma step of the
            // grade between them where the layer is graded), and the frame is drawn only while it stays within two levels:
            // one sharpen of at most 0.5, and no second one. A sharpen of nothing at `unsharp`'s 16-bit precision counts
            // for nothing; with a scaler that is swscale's to the sample (`caps.exact_scaler`), or with no scale upstream, a
            // sharpen is exact at any amount, after a blur as well (the blur is `gblur`'s to the sample or not drawn).
            let scaled = geom.stages.iter().any(|s| s.scaled != (s.src.w, s.src.h));
            // The `eq` is next in the chain after those scales (the compositor applies its tables in the vertical
            // pass of the last of them), and they multiply the scaler's level: two pictures a level apart are, after
            // them, as far apart as the tables' steepest step between neighbouring entries (`eq::table_steps`,
            // the whole table: the scaler's overshoot at a hard edge reaches the bottom of the range, where a
            // table leaving 0 for its first entry steps by 256 * (1/255)^(1/gamma) at a black point of 0 and by
            // more where a contrast or a negative brightness parks it higher). Up to two is the bound of a
            // scaled blur and `GRADED_SCALED`'s (contrast and saturation up to 2, a gamma toward 1 or a little
            // over: 1.2); past it the step is not bounded (12 and 19 levels measured at the far ends of every
            // range), so the frame is FFmpeg's. A plan with a keyed colour judges each frame's numbers, and
            // the luma step is where a sharpen after the grade starts its own worst case from, below.
            let steps = if scaled { eq::table_steps(&layer.color) } else { [1; 3] };
            let steepest = steps.into_iter().max().unwrap_or(1);
            // (Where the scaler is exact there is no level for the tables to multiply: no refusal for how steep they are.)
            if steepest > MAX_AMPLIFIED_ERROR && !caps.exact_scaler {
                reasons.push(Unsupported::GradeSteep(self.layer_ref(n, layer), steepest));
            }
            // (Where the scaler is exact it is no source of a difference, and neither is a blur the compositor draws: it is `gblur`'s
            // own recursion to the sample, `GpuCaps::effects` holding it only where this FFmpeg's was measured to be.)
            let mut err = if scaled && !caps.exact_scaler {
                f64::from(steps[0]).max(1.0)
            } else {
                0.0
            };
            let mut sharpened = false;
            for effect in &layer.effects {
                match effect {
                    VideoEffect::Sharpen { amount } if err > 0.0 && (f64::from(*amount as f32) * 65536.0) as i32 != 0 => {
                        err += (2.0 * err * amount.abs()).ceil();
                        sharpened = true;
                    }
                    _ => {}
                }
            }
            // A chroma key's alpha is a distance in the chroma over `blend`: a level of difference in a chroma
            // sample moves it by up to 1/blend levels (a blend of 0 flips it from 0 to 255), so it is drawn only on
            // chroma that is FFmpeg's to the sample. It runs after every effect, and the chroma it reads is what a scale
            // leaves: swscale's to the sample where the scaler is measured (`caps.exact_scaler`, as for the sharpen's and the
            // grade's rules), a level off where it is not (a blur the compositor draws is `gblur`'s to the sample, and a sharpen
            // leaves the chroma alone, the tables are exact).
            let chroma_keyed = layer.effects.iter().any(|e| matches!(e, VideoEffect::ChromaKey { .. }));
            if chroma_keyed && caps.effects.contains(EffectKinds::CHROMA_KEY) && scaled && !caps.exact_scaler {
                reasons.push(Unsupported::Effects(
                    self.layer_ref(n, layer),
                    "a chroma key after a scale: its alpha is a distance in the chroma, which the scale's difference from FFmpeg's moves by up to 1/blend levels",
                ));
            }
            // `chromakey` starts its window over at each slice of the graph, so its alpha depends on the number of slice threads and is
            // drawn at one count (`Compositor::graph_threads_for`): where the plan knows the count (a pin, a single frame, the budget's
            // cap, a core count it reproduces) that is FFmpeg's, and where it only knows a range (the count is not reproduced: another
            // system, or a failed `sched_getaffinity`, where FFmpeg counts one) no count of it is assumed.
            if chroma_keyed && caps.effects.contains(EffectKinds::CHROMA_KEY) {
                let range = *threads.get_or_init(|| self.graph_thread_range_in(caps.graph_threads, machine));
                if crate::engine::cpu::one_number(range).is_none() {
                    reasons.push(Unsupported::Effects(
                        self.layer_ref(n, layer),
                        "a chroma key over a graph whose slice-thread count is not known: its window starts over at each slice",
                    ));
                }
            }
            // The key's window keeps what it last read where the picture ends; a picture under two samples a
            // side has no such row or column (the compositor's rule for it needs one).
            let (pw, ph) = geom.picture;
            if chroma_keyed && caps.effects.contains(EffectKinds::CHROMA_KEY) && (pw < 2 || ph < 2) {
                reasons.push(Unsupported::Effects(
                    self.layer_ref(n, layer),
                    "a chroma key on a picture narrower or shorter than 2",
                ));
            }
            // `gblur`'s own kernels are not the recursion on every plane shape at every slice-thread count (a plane too few rows tall for
            // the threads, one a vector wide, one a sample across: `gblur::KernelHazard`): the plane is the picture's luma and half-size
            // chroma, the threads the graph this frame stands in for runs on, the width of the kernel the one the probe found. A blur of
            // sigma exactly 0 (the identity: `set_params` divides 0 by 0, the same NaN on every build) is judged too, because the kernel
            // runs all the same, but without the one hazard that cannot change a sample of it (`gblur::plane_hazard_for`: an empty
            // postscale slice scales by 1 and clips again, which is `x` for every sample of an 8-bit plane, in any interleaving); what
            // reads past the row or the heap stays refused. A tiny sigma whose coefficients come out as the same numbers is not lifted:
            // that is a cancellation in `double` arithmetic the blur probe does not measure, and 0 is the only sigma that does not
            // depend on it.
            let blurs = || {
                layer.effects.iter().filter_map(|e| {
                    if let VideoEffect::Blur { sigma } = e {
                        Some(*sigma)
                    } else {
                        None
                    }
                })
            };
            let identity = |sigma: f64| sigma == 0.0;
            let judged = caps.effects.contains(EffectKinds::BLUR) || blurs().any(identity);
            if judged && blurs().next().is_some() {
                let (lo, hi) = *threads.get_or_init(|| self.graph_thread_range_in(caps.graph_threads, machine));
                let all_identity = blurs().all(identity);
                // (The frame the blur runs on: the padded one of a letterbox, bars included.)
                let frame = effect_frame(&geom);
                if let Some(found) = crate::gblur::picture_hazard_for(frame, lo..=hi, u32::from(caps.blur_lanes), all_identity) {
                    reasons.push(Unsupported::Effects(self.layer_ref(n, layer), found.hazard.reason()));
                }
            }
            // `unsharp` is not the model on a frame of 2, 4 or 6 rows (exact from 8, whatever the width; with no bars as well: the frame the filter sees
            // is the padded one of a letterbox).
            if caps.effects.contains(EffectKinds::SHARPEN)
                && layer.effects.iter().any(|e| matches!(e, VideoEffect::Sharpen { .. }))
                && effect_frame(&geom).1 < UNSHARP_MIN_ROWS
            {
                reasons.push(Unsupported::Effects(
                    self.layer_ref(n, layer),
                    "a sharpen on a frame under 8 rows high (`unsharp` is not the compositor's model there: measured off on 2, 4 and 6 rows, exact from 8; unproven below)",
                ));
            }
            if sharpened && err > f64::from(MAX_AMPLIFIED_ERROR) {
                reasons.push(Unsupported::Effects(
                    self.layer_ref(n, layer),
                    "a sharpen after a scale: it amplifies its one-level difference from FFmpeg's by 1 + 2|amount| and rounds up a level more with each further sharpen, past two levels",
                ));
            }
            // A blended layer is drawn where a parity case holds the blend chain to FFmpeg's: a picture over an even frame (the
            // conversions are 4:2:0 pairs), at its own size or resized (the layer is the fit pass's: the scaler is swscale's wherever it
            // is in the caps, `ScalerApproximate` otherwise, and the blend chain begins after it), upright or turned by a quarter, opaque
            // or below full opacity (the round trip's layer, where the blend chain was measured to carry it: `BlendExtras::TRANSLUCENT`,
            // in `size_free_reasons`). A rotation by another angle (its edge is a fixed-point sample) and a letterbox (its bars are
            // transparent in the blend and opaque in the compositor) are refused.
            if !layer.blend.is_normal() {
                let why = |w: &'static str| Unsupported::BlendMode(self.layer_ref(n, layer), layer.blend, w);
                // A turn of exactly a quarter lands on whole samples (the `rotate` filter's fixed-point sample is exact there); any
                // other angle has an edge that is a fixed-point sample against the compositor's float one, and a keyed rotation
                // is another box.
                if geom.rotation.is_some_and(|r| !is_quarter_turn(r.angle))
                    || (self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.rotates))
                {
                    reasons.push(why("on a rotated layer (the edge is a fixed-point sample)"));
                }
                if geom.matte {
                    reasons.push(why("on a letterboxed layer (its bars are transparent in the blend)"));
                }
            }
            // FFmpeg scales a picture in the format it has; the compositor scales the
            // 8-bit 4:2:0 a decode reduces it to, which is the same picture only when
            // that is what it already was. For anything else the chroma is interpolated from different samples — FFmpeg's real
            // ones, the compositor's already averaged away: a 2x enlargement of 4:4:4
            // is 27 levels off, of RGB 69, and a shrink as mild as 1.05-1.5x still
            // reads flat max 8-9 (4:2:2 at 0.9x is over the limit) and 32 levels on
            // edges. No band of ratios was measured strictly inside the limits on both
            // FFmpegs for busy chroma, so none is claimed: any resize is FFmpeg's. An
            // unrecorded format is not known to be 4:2:0.
            // FFmpeg makes the fit picture of a still whose placement it draws (`FitCopy`): its scaler, its range and RGB conversion
            // and the chroma grid of its crops are FFmpeg's, so the refusals about the compositor's own are not this layer's.
            let fit_made = self.fits_by_ffmpeg(layer);
            if fit_made && self.scaler_named_other() {
                reasons.push(Unsupported::StillMotion(
                    self.layer_ref(n, layer),
                    format!(
                        "a delivery that names the `{}` scaler (the fit picture FFmpeg makes carries it, and its chains are measured with the default scaler and with `bicubic` named)",
                        self.canvas.scaler.as_deref().unwrap_or_default()
                    ),
                ));
            } else if fit_made {
                match self.fit_copy(layer, size) {
                    None => reasons.push(Unsupported::StillMotion(
                        self.layer_ref(n, layer),
                        if layer.fit.is_some() {
                            format!(
                                "at a render of {}x{} (its fit picture was made for another size)",
                                size.0, size.1
                            )
                        } else {
                            format!("at a render of {}x{} (no fit picture can be made for it)", size.0, size.1)
                        },
                    )),
                    Some(fit) => {
                        // The chain its text is: measured for this kind, in this shape, with this scaler, or FFmpeg's.
                        // (A kind not measured at all is refused by `size_free_reasons`.)
                        let chain = crate::plan_caps::StillChain::edge(fit.shape, self.scaler_named_bicubic());
                        let kind = self.still_kind(layer).filter(|k| caps.still_motion.contains(*k));
                        if kind.is_some_and(|k| !caps.still_motion.contains_chain(k, chain)) {
                            let pix_fmt = layer.stream.pix_fmt.as_deref().unwrap_or("?");
                            reasons.push(Unsupported::StillMotion(
                                self.layer_ref(n, layer),
                                if chain.measurable() {
                                    format!(
                                        "a {pix_fmt} picture whose fit picture ({:?} text{}) this compositor has not measured FFmpeg making on this FFmpeg",
                                        fit.shape,
                                        if chain.bicubic { ", the bicubic scaler named" } else { "" },
                                    )
                                } else {
                                    format!(
                                        "a {pix_fmt} picture whose fit picture ({:?} text, a `crop` of its own besides the guard) is not measurable: no toy cut of any probe takes that text, so this compositor has not measured FFmpeg making it, on any FFmpeg, and never will",
                                        fit.shape,
                                    )
                                },
                            ));
                        }
                        if let Some(why) = Self::copy_size_refusal(fit.size) {
                            reasons.push(Unsupported::StillMotion(self.layer_ref(n, layer), why));
                        }
                    }
                }
            }
            let shrink_limit = max_shrink_for(self.canvas.scaler.as_deref());
            if let Some(stage) = geom
                .stages
                .iter()
                .find(|s| shrinks_past(s.src.w, s.scaled.0, shrink_limit) || shrinks_past(s.src.h, s.scaled.1, shrink_limit))
                .filter(|_| !fit_made)
            {
                reasons.push(Unsupported::Shrink(n, (stage.src.w, stage.src.h), stage.scaled, shrink_limit));
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
                let resized = geom.stages.iter().any(|s| s.scaled != (s.src.w, s.src.h));
                let odd = geom.picture.0 % 2 == 1 || geom.picture.1 % 2 == 1;
                for why in [
                    geom.matte.then_some(DipWhy::Letterboxed),
                    odd.then_some(DipWhy::OddSize(geom.picture.0, geom.picture.1)),
                    resized.then_some(DipWhy::Resized),
                ]
                .into_iter()
                .flatten()
                {
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
            // range and then scales. In an export's planes the chroma reads up to 15 levels off (FFmpeg 6 as
            // much as 9), a fade running on it takes it further (14 flat), and a still's planes are off too (a
            // JPEG half-size over video: Y 12, U and V 16, over a fifth of the frame; an enlargement 14 / 21):
            // the two agree outside the picture's sharp edges only, and that is not a sample for sample match.
            if layer.stream.maybe_full_range() && !fit_made && geom.stages.iter().any(|s| s.scaled != (s.src.w, s.src.h)) {
                let where_ = if self.mode == PlanMode::Motion {
                    "in an export frame"
                } else if self.fade_ops(layer).iter().any(|o| o.factor < FADE_FULL) {
                    "inside a fade, a dip or a dissolve"
                } else {
                    "in a still"
                };
                reasons.push(Unsupported::FullRangeResized(
                    LayerRef {
                        index: n,
                        name: layer.name.clone(),
                    },
                    where_,
                ));
            }
            // The exact scaler is proven for planar 8-bit 4:2:0 only. A 10-bit picture is scaled by FFmpeg at 10 bits and dithered
            // once at the end (off by one in Y, U and V on 6.1.1), and a gray one has its luma scaled in `gray` and converted after
            // (Y up to 10 off over half the plane on 9.0.2): neither is the 8-bit 4:2:0 the decode hands the compositor, and
            // semi-planar `nv12` has no case at all.
            let resizing = geom.stages.iter().find(|s| s.scaled != (s.src.w, s.src.h));
            let eight_bit_420 = layer
                .stream
                .pix_fmt
                .as_deref()
                .is_some_and(|f| matches!(f.to_ascii_lowercase().as_str(), "yuv420p" | "yuvj420p"));
            if let Some(stage) = resizing.filter(|_| !fit_made).filter(|_| !eight_bit_420) {
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
            let why = self.overlays.iter().find_map(|o| {
                let px = (f64::from(size.1) * o.size).round().max(1.0) as u32;
                crate::text_sequence_limit(&o.text, Some(px))
            });
            if let Some(why) = why {
                reasons.push(Unsupported::TextSequence(why));
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
        // (A blurred fill is judged per layer and by the render size, in `reasons`: whether a picture leaves bars to
        // fill is the size's to say.)
        out.extend(self.pictureless.iter().cloned().map(Unsupported::NoPicture));
        for (n, layer) in self.layers.iter().enumerate() {
            let at = || LayerRef {
                index: n,
                name: layer.name.clone(),
            };
            let stream = &layer.stream;
            // A ramped clip's frame is chosen by the export's retime expression, whose `fps` pick the
            // plan does not model (`PlanLayer::pick` names none), and a still of it would show the
            // frame the map says where the file shows the one its slot has come to: FFmpeg draws it.
            if layer.timing.ramped {
                out.push(Unsupported::SpeedRamp(at()));
            }
            // A stabilized clip's frames are moved by vid.stab's transform (or `deshake`) in the chain:
            // nothing the compositor draws is that picture, so FFmpeg draws it.
            if layer.timing.stabilized {
                out.push(Unsupported::Stabilized(at()));
            }
            // A blend mode is FFmpeg's `blend` over the composite in RGB, through two placements and a conversion
            // each way (`engine::blend_chains`): drawn for the modes in `caps.blend`, in the matrix the graph names
            // (`blend_matrix`), at the level only where the rest of the chain is exact: see the other causes in `reasons`.
            if !layer.blend.is_normal() {
                if !caps.blend.contains(BlendKinds::of(layer.blend)) {
                    out.push(Unsupported::BlendMode(
                        at(),
                        layer.blend,
                        "no compositor pass draws this mode on this FFmpeg",
                    ));
                }
                match self.blend_matrix() {
                    None => out.push(Unsupported::BlendMode(
                        at(),
                        layer.blend,
                        "the composite's matrix is not BT.601, BT.709 or BT.2020, the ones the conversions are reproduced for (a bottom layer's tag with no proven table included)",
                    )),
                    Some(YuvMatrix::Bt2020) if !caps.blend_extras.contains(BlendExtras::BT2020) => {
                        out.push(Unsupported::BlendMode(
                            at(),
                            layer.blend,
                            "a BT.2020 composite (not measured to be the model's on this FFmpeg)",
                        ));
                    }
                    Some(_) => {}
                }
                // A tag with no table (`bt2020c`, `ycgco`, `ictcp`, ...) is judged by an allow-list of the ones proven, on the clip itself too:
                // FFmpeg 9 cannot export such a clip, and FFmpeg 6 reads it as it reads an untagged one.
                if layer.hdr.is_none()
                    && !layer.stream.label_bt709
                    && crate::model::matrix_tag_is_unlisted(layer.stream.color_space.as_deref(), layer.stream.pix_fmt.as_deref())
                {
                    out.push(Unsupported::BlendMode(
                        at(),
                        layer.blend,
                        "a clip tagged with a colour matrix the blend's conversions have no proven table for",
                    ));
                }
                // A clip below full opacity takes the RGB round trip (`colorchannelmixer`) ahead of the blend's `split`, where the chain
                // converts the same `argb` picture twice (once for its colour, once for its alpha plane): the same planes as the
                // round trip makes of it only where this FFmpeg's chain was measured to (`BlendExtras::TRANSLUCENT`: the export's own
                // text on noise); on any other it is FFmpeg's.
                if self.through_rgba(layer) && !caps.blend_extras.contains(BlendExtras::TRANSLUCENT) {
                    out.push(Unsupported::BlendMode(
                        at(),
                        layer.blend,
                        "on a layer below full opacity (this FFmpeg's blend chain is not measured to carry the RGB round trip's layer as the model does)",
                    ));
                }
                // An RGB picture reaches the graph as RGB and is converted by `format=yuva420p`, where the compositor has the decode's
                // 4:2:0: the same picture on the builds where the probe says so (FFmpeg 4.4's is Y 3 / UV 11 off, and its chain does not match), and a blend carries a
                // difference through.
                if layer.stream.layout() == Some(PixLayout::Rgb) && !caps.blend_extras.contains(BlendExtras::RGB_PICTURE) {
                    out.push(Unsupported::BlendMode(
                        at(),
                        layer.blend,
                        "an RGB picture (the graph converts it, and this FFmpeg's conversion is not the decode's 4:2:0)",
                    ));
                }
                // A blended clip's chain puts `format=yuva420p` (and the transparent bars' `pad`) ahead of the grade, so FFmpeg's RGB
                // filters run `yuva420p -> argb -> yuva420p` where the grade's model is the `yuv420p -> rgb24` path: another conversion
                // each way (Y 11 / UV 9 off, every case).
                if !layer.grade.is_identity() {
                    out.push(Unsupported::BlendMode(
                        at(),
                        layer.blend,
                        "a grade under a blend (its RGB round trip runs on yuva420p there, another conversion than the grade's model)",
                    ));
                }
                // A moving zoom or position ends the clip's chain in `pad`, `perspective` and `crop`: the blend lays that picture by the
                // very `overlay` it places a clip with, and no case holds the pair to FFmpeg's (the blend's pass is the plain layer's, at its
                // own size), so a blended clip is not placed.
                // (A moving still on the export's own route has no pose: its `perspective` is the same stage, and as little held.)
                if self.pose(layer).is_some() || self.takes_still_route(layer) {
                    out.push(Unsupported::BlendMode(
                        at(),
                        layer.blend,
                        "a moving zoom or position (the placement's picture is not one the blend chain is held to)",
                    ));
                }
            }
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
            if self.graded(layer) && stream.maybe_full_range_in(self.canvas.policy) {
                let detail = match stream.pix_fmt.as_deref() {
                    Some(f) if stream.raw_full_range() => {
                        format!("a picture flagged full range ({f}, color_range=pc), whose raw values FFmpeg grades before converting its range")
                    }
                    Some(f) if stream.maybe_full_range() => {
                        format!("a full-range picture ({f}), which FFmpeg grades before converting its range")
                    }
                    Some(f) => format!("a picture deeper than 8 bits ({f}) whose range is not recorded (it may be full range)"),
                    None => "a picture whose pixel format was never recorded (it may be full range)".to_string(),
                };
                out.push(Unsupported::GradedFullRange(at(), detail));
            }
            // Curves (`lutrgb`), wheels (`colorbalance`) and a LUT (`lut3d`) run in RGB, through a round
            // trip FFmpeg negotiates; no compositor pass or parity case draws them.
            let grade_drawn = if let Some(what) = grade_refusal(&layer.grade, caps) {
                out.push(Unsupported::Grade(at(), what));
                false
            } else {
                !layer.grade.is_identity()
            };
            if let Some(what) = layer
                .effects
                .iter()
                .find_map(|e| effect_refusal(e, caps).or_else(|| effect_on_picture_refusal(e, stream, self.canvas.policy)))
            {
                out.push(Unsupported::Effects(at(), what));
            }
            let chroma_keyed = layer.effects.iter().any(|e| matches!(e, VideoEffect::ChromaKey { .. }));
            if layer.mask.is_some() {
                if !caps.mask {
                    out.push(Unsupported::Mask(at()));
                } else if let Some(what) = self
                    .mask_refusal(layer)
                    .or_else(|| chroma_keyed.then_some("over a chroma key (geq multiplies the key's alpha)"))
                {
                    out.push(Unsupported::MaskWith(at(), what));
                }
            }
            // The key's alpha goes through what a mask's would: the compositor does not turn it, and a blend mode's chain does not carry
            // it below full opacity. (A constant opacity below 1 maps the key's levels through `colorchannelmixer`'s table exactly as a
            // mask's.)
            if chroma_keyed && caps.effects.contains(EffectKinds::CHROMA_KEY) {
                if let Some(why) = self.alpha_plane_refusal(layer) {
                    out.push(Unsupported::Effects(
                        at(),
                        match why {
                            AlphaPlaneRefusal::Rotated => "a chroma key on a rotated layer",
                            AlphaPlaneRefusal::BlendedBelowFull => "a chroma key on a blended layer below full opacity",
                        },
                    ));
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
            // converted `yuv420p -> rgb24 -> yuv420p` around it: reproduced, exactly, for a plain chain on an
            // 8-bit limited-range 4:2:0 picture (the grade's conversions, where the probe says they are this
            // FFmpeg's; every other format was measured 9 to 21 levels off, gray 2 on one picture and not
            // claimed). On an alpha plane FFmpeg takes another RGB format, and a black fade of the clip that
            // runs ahead of it is a filter of its own in the YUV.
            let ops = self.fade_ops(layer);
            if grade_drawn {
                if let Some(what) = self.grade_chain_refusal(layer, &ops) {
                    out.push(Unsupported::GradeWith(at(), what));
                }
            }
            if self.white_round_trip(layer) {
                let why = if !caps.dip_white {
                    Some(DipWhy::NoRoundTrip)
                } else if self.on_alpha_plane(layer) {
                    Some(DipWhy::AlphaPlane)
                } else if ops.iter().any(|o| o.tint == FadeTint::Black && o.factor < FADE_FULL) {
                    Some(DipWhy::BlackFade)
                } else if stream.pix_fmt.as_deref() != Some("yuv420p") {
                    Some(DipWhy::Format)
                } else if stream.raw_full_range() {
                    // (The round trip runs on every frame of the clip of an export, and in the still's while the fade runs; both
                    // convert in the range the picture is flagged.)
                    Some(DipWhy::FullRange)
                } else if layer
                    .effects
                    .iter()
                    .any(|e| matches!(e, VideoEffect::Invert | VideoEffect::Vignette))
                {
                    // (Of the effects, only `negate` and `vignette` list an RGB format: the `scale` ahead of them is the one that
                    // converts, so they run in RGB. `gblur`, `unsharp`, `hue` and `eq` list none, and stay in the YUV.)
                    Some(DipWhy::RgbEffect)
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
            // fade-in reads Y 30 where black is 16). FFmpeg 6 converts a `yuvj` picture at the decode, as
            // the compositor does, but not a `yuv420p` flagged `pc`, whose format is already the
            // overlay's: that one is refused under every policy, in an export frame as in a still. (Under
            // FFmpeg 9 an export frame is refused for the cut it is in, below.)
            let negotiated = self.canvas.policy != CompositeColorPolicy::FixedBt601;
            let raw_kept = if self.mode == PlanMode::Still {
                (negotiated && stream.maybe_full_range_in(self.canvas.policy)) || stream.raw_full_range()
            } else {
                !negotiated && stream.raw_full_range()
            };
            if raw_kept && ops.iter().any(|o| o.factor < FADE_FULL) {
                out.push(Unsupported::FullRangeNegotiated(at()));
            } else if self.mode == PlanMode::Still && negotiated && (stream.maybe_full_range() || stream.flagged_full_range()) {
                // Not only inside a fade: a plain full-range picture in a still keeps its range through FFmpeg 9's graph, which converts it
                // last, so the still's own planes are full-range ones and the PNG's RGB is read from them (measured on a plain
                // `yuvj420p` clip: planes up to 17 levels from the compositor's limited-range composite, RGB 3 to 4 levels, 205,000 samples).
                // FFmpeg 6 converts a `yuvj` picture at the decode, as the compositor does. (An export refuses the whole cut: `FullRangeCut`.)
                out.push(Unsupported::FullRangeStill(at()));
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
            if self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.mask) {
                out.push(Unsupported::KeyedMask(at()));
            }
            if layer.animated.is_some_and(|a| a.crop) {
                out.push(Unsupported::KeyedCrop(at()));
            }
            // A moving still the export takes through its own route (the scaled copy and one `perspective`): drawn
            // where the compositor measured this FFmpeg's chain for the picture (`GpuCaps::still_motion`), and
            // judged by that route alone: the fit, the zoom and the position are inside the `perspective`, so
            // none of the placement's, the fit order's or the keyed zoom's checks applies to it.
            let route_b = self.takes_still_route(layer);
            if layer.animated.is_some_and(|a| a.still_motion) {
                if let Some(why) = self.still_route_refusal(layer, caps, &ops) {
                    out.push(Unsupported::StillMotion(at(), why));
                }
            }
            if layer.animated.is_some_and(|a| a.places) && !caps.placement && !route_b {
                out.push(Unsupported::KeyedPlacement(at()));
            }
            // A moving still that is neither the route's nor a fit FFmpeg makes (colour, a grade, an effect, a mask, a rotation, a
            // translucent layer, a dip to white, a picture kind with no chain measured): the general chain would draw it as footage,
            // from a decode and a scaler no case holds a still to, so it stays FFmpeg's.
            if layer.is_image && layer.animated.is_some_and(|a| a.places) && !route_b && !self.fits_by_ffmpeg(layer) {
                out.push(Unsupported::StillMotion(
                    at(),
                    "a moving zoom or position on a still with colour, a grade, an effect, a mask, a rotation, a translucent layer, a dip to white or a picture kind no chain was measured for, which no case holds to FFmpeg's"
                        .into(),
                ));
            }
            if self.pose(layer).is_some() && caps.placement {
                let fmt = stream.pix_fmt.clone().unwrap_or_else(|| "not recorded".to_string());
                // 8-bit planar 4:4:4 and 4:2:2, limited range: converted in the graph as the decode converts them where a
                // compositor measured that (`GpuCaps::placed_chroma`).
                let planar = matches!(fmt.to_ascii_lowercase().as_str(), "yuv444p" | "yuv422p");
                let other_layout = !matches!(stream.layout(), Some(PixLayout::Yuv420 | PixLayout::Gray));
                // A still FFmpeg makes the fit picture of (`fits_by_ffmpeg`) is converted by the graph's own scaler, as the chain does,
                // in the pass that resizes it: the compositor's decode and scaler do not stand between, but a build whose chain it has
                // not measured does.
                let fits = self.fits_by_ffmpeg(layer);
                if fits && !self.still_kind(layer).is_some_and(|k| caps.still_motion.contains(k)) {
                    out.push(Unsupported::StillMotion(
                        at(),
                        format!("a {fmt} picture whose fit this compositor has not measured FFmpeg making on this FFmpeg"),
                    ));
                }
                if other_layout && !fits && !(caps.placed_chroma && planar) {
                    out.push(Unsupported::PlacedFormat(at(), fmt));
                } else if self.canvas.policy != CompositeColorPolicy::FixedBt601 && stream.maybe_full_range_in(self.canvas.policy)
                {
                    // Where the composite's range is negotiated along the graph (9.0.2), the range conversion inside
                    // the chain's `format=yuva420p` is not the decode's: a full-range 8-bit or 10-bit 4:2:0 picture reads
                    // up to 5 levels off in a still (3 to 5 on `yuvj420p`, 4 to 5 on 10-bit flagged `pc`), a full-range
                    // 4:4:4 or 4:2:2 one 5 to 8; exact on 4.4.2. The check is the layout's own and the lifted 4:4:4 /
                    // 4:2:2 are not exempt from it.
                    out.push(Unsupported::PlacedFormat(at(), format!("full-range {fmt}")));
                } else if caps.file_range
                    && self.canvas.policy != CompositeColorPolicy::FixedBt601
                    && stream.color_range.is_none()
                {
                    // An asset probed before the range flag was recorded: only its file can say, and a host that asks the
                    // file ahead (`GpuCaps::file_range`) has left the answer in the cache. Full range, or no answer (a probe that
                    // failed or has not run), is what the compositor refuses at draw time, so a session, the render bar and a
                    // hand-over see it before the frame instead of a frame into the play.
                    match crate::engine::source_is_full_range_known(std::path::Path::new(&layer.path)) {
                        Some(false) => {}
                        Some(true) => out.push(Unsupported::PlacedFormat(
                            at(),
                            format!("full-range {fmt} (its file flags color_range=pc; its asset did not record it)"),
                        )),
                        None => out.push(Unsupported::PlacedFormat(
                            at(),
                            format!("{fmt} of a range its asset did not record and its file has not said (not asked ahead, or would not say)"),
                        )),
                    }
                }
            }
            // A live behaviour is drawn as the pose it gives (`PlanLayer::transform`) where that is a moving zoom or position
            // (breathe, drift, shake: the placement draws them); one that turns the picture (sway, spin) is a rotation, which
            // no compositor draws to the sample.
            if layer.animated.is_some_and(|a| a.behaves && (!caps.behaviors || a.rotates)) {
                out.push(Unsupported::Behavior(at()));
            }
            if let Some(what) = self.rotation_refusal(layer) {
                out.push(Unsupported::Rotation(at(), what));
            }
            // The graph fits and zooms in one `scale` from the source; the compositor's geometry (and
            // `LayerGeometry`, which the Preview's handles are placed by) still fits to the frame and
            // scales that, which is another picture. Nothing draws the new one yet.
            match layer.fit_order {
                _ if route_b => {}
                FitOrder::Fitted => {}
                FitOrder::Folded => out.push(Unsupported::FoldedFit(at(), "a zoom in under Contain")),
                FitOrder::Uncropped => out.push(Unsupported::FoldedFit(at(), "a Cover clip that zooms or pans")),
            }
            if layer.animated.is_some_and(|a| a.places && !a.zooms && !a.still_motion) && caps.placement {
                // A position that moves is placed after the same chain a moving zoom is: the rotation, the opacity and
                // the dip to white are the ones the placement is not drawn behind (a grade is: it hands the placement
                // its 4:2:0 planes).
                if let Some(what) = self.zoom_order_refusal(layer, &ops) {
                    out.push(Unsupported::ZoomBehind(at(), what));
                }
            }
            if layer.animated.is_some_and(|a| a.zooms) && !route_b {
                if caps.keyed_zoom {
                    // The compositor draws a grade, the effects and the mask it draws, and the fades
                    // at the fit size and places the picture after them. A rotation, an opacity below 1 and a dip to
                    // white are what it does not draw ahead of the placement: the placement would act on the rotated
                    // alpha plane, on the RGB round trip's or on the RGB frame the fade leaves, and no case holds
                    // any of them to FFmpeg's.
                    if let Some(what) = self.zoom_order_refusal(layer, &ops) {
                        out.push(Unsupported::ZoomBehind(at(), what));
                    }
                } else if keyed {
                    out.push(Unsupported::KeyedZoom(at()));
                } else if !layer.color.is_identity()
                    || !layer.grade.is_identity()
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
            }
        }
        // resvg draws an animated title and FFmpeg overlays its pictures: a floating one is drawn where the compositor measured
        // that overlay, a title clip (a clip chain on an alpha picture) by no pass.
        if self.svg.iter().any(|s| s.title.is_none()) {
            out.push(Unsupported::Svg);
        }
        if self.svg.iter().any(|s| s.title.is_some()) {
            out.extend(self.svg_overlay_refusal(caps));
        }
        // The delivery's scaler: `bicubic` (or none) is drawn wherever the scaler is exact at all; the others are drawn where the compositor's
        // tables for the name were measured to be this FFmpeg's (`caps.scalers`: `libm`'s `sin` and a compiler's float contraction are
        // the machine's), and a name the engine's list does not have is never drawn. (Here the case is ignored on purpose, unlike
        // `scaler_named_bicubic`: a spelling the engine's list does not have — `Bicubic` — writes no flags, so FFmpeg runs the build's
        // default scaler, which is the one the compositor draws; it is the chain that must not be taken for a named scaler's. The name is
        // refused only if it is another's.)
        if let Some(s) = self.canvas.scaler.as_deref().filter(|s| !s.eq_ignore_ascii_case("bicubic")) {
            if !ScalerKinds::of_name(s).is_some_and(|kind| caps.scalers.contains(kind)) {
                out.push(Unsupported::Scaler(s.to_string()));
            }
        }
        let policy = self.canvas.policy;
        out.extend(self.matrix_reasons(caps));
        // FFmpeg 9 negotiates the *range* along the overlay chain as it does the matrix:
        // the bottom layer's decides, and a graded layer above a full-range picture is
        // 40 to 44 dB off (a limited-range clip over a limited-range one is exact). Where
        // the composite's colour is negotiated, or not known, grading in a stack that
        // holds a full-range picture is FFmpeg's.
        if policy != CompositeColorPolicy::FixedBt601 && self.layers.iter().any(|l| l.stream.maybe_full_range_in(policy)) {
            for (n, layer) in self.layers.iter().enumerate() {
                if self.graded(layer) && !layer.stream.maybe_full_range_in(policy) {
                    out.push(Unsupported::GradedInFullRangeStack(n));
                }
            }
        }
        out
    }

    /// Why a compositor with `caps` does not draw the floating titles of this frame: it did not measure FFmpeg's `overlay` of a picture
    /// ([`GpuCaps::svg`]); there is no clip under them (the black base is not a YUV composite); or the composite is not BT.601 (FFmpeg 9
    /// converts the picture with the composite's matrix). **The first cause only** is listed, as the ledger counts a frame once.
    fn svg_overlay_refusal(&self, caps: &GpuCaps) -> Option<Unsupported> {
        let why = if !caps.svg {
            SvgWhy::Unmeasured
        } else if self.layers.is_empty() {
            SvgWhy::NoLayer
        } else if self.canvas.matrix != YuvMatrix::Bt601 {
            SvgWhy::Matrix(self.canvas.matrix)
        } else {
            return None;
        };
        Some(Unsupported::SvgOverlay(why))
    }

    fn layer_ref(&self, index: usize, layer: &PlanLayer) -> LayerRef {
        LayerRef {
            index,
            name: layer.name.clone(),
        }
    }

    /// Whether the layer's chain has an `eq` that does something to the picture: a colour that is not the
    /// identity, or in an export frame a **keyed** one, whose `eq` (`eval=frame`) is in the chain on every
    /// frame of the clip, neutral numbers or not. A refusal about grading a picture (a full-range one,
    /// whose range FFmpeg 9 converts after the `eq`) judges the clip, so that a keyed colour is not drawn
    /// by one renderer on the frames its curve passes through neutral and by the other on the rest.
    fn graded(&self, layer: &PlanLayer) -> bool {
        !layer.color.is_identity()
            || !layer.grade.is_identity()
            || (self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.color))
    }

    /// Whether an `eq` sits in the layer's chain, after the fit `scale` and ahead of the grade: it takes planar YCbCr
    /// only, which stops the format negotiation there. (Without one the fit `scale`, the filters that pass any format
    /// along and the grade's `lut3d` / `lutrgb` / `colorbalance` — RGB only — agree on RGB for the scale's output,
    /// which scales and converts in one pass, from chroma another way than scale-then-convert.) A colour that is not
    /// the identity writes one; an export frame's **keyed** colour writes it on every frame.
    pub fn eq_in_chain(&self, layer: &PlanLayer) -> bool {
        !layer.color.is_identity() || (self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.color))
    }

    /// Why a compositor does not draw this layer's rotation: the only turn it draws is one `rotate` lands on whole samples for
    /// ([`quarter_turn`]); any other angle has an edge that is a fixed-point sample with an apron of its own (a rotated layer's edge
    /// differed from a float model's by up to 117 levels of luma and 169 of chroma, with or without a keyed opacity), and a rotation a key or
    /// a behaviour moves is another box (`hypot(iw,ih)`) turned by an expression per frame. A clip whose keyed rotation never turns has no
    /// `rotate` in the graph at all.
    fn rotation_refusal(&self, layer: &PlanLayer) -> Option<&'static str> {
        let animated = layer.animated.filter(|_| self.mode == PlanMode::Motion);
        if animated.is_some_and(|a| a.rotates) {
            Some("a rotation that a key or a behaviour moves (a per-frame angle in a box of its own, with an edge that is a fixed-point sample)")
        } else if animated.and_then(|a| a.keys).is_some_and(|k| k.rotation) {
            None
        } else if layer.transform.rotation != 0.0 && quarter_turn(layer.transform.rotation.to_radians()).is_none() {
            Some("a rotation that is not a quarter, half or three quarters of a turn FFmpeg's rotate lands on whole samples for (any other angle has an edge that is a fixed-point sample)")
        } else {
            None
        }
    }

    /// Why a compositor that draws this layer's grade parts still does not draw its chain. FFmpeg's grade
    /// runs in RGB: the picture goes `yuv420p -> rgb24` (swscale's x86 kernel, reproduced to the level), through
    /// `lut3d`, `lutrgb` and `colorbalance` in turn, and back to 4:2:0 by a scale whose vertical filter has
    /// its own x86 kernel (a tagged picture first to 4:4:4 by an explicit `scale`, then to 4:2:0 by the next
    /// filter's). All of it is reproduced for the one picture it was measured on, 8-bit limited-range
    /// `yuv420p` at an even size, and only where nothing between the grade and the next 4:2:0 conversion
    /// sees the RGB (or 4:4:4) picture: a video effect or a black or white fade there runs on it in the format
    /// FFmpeg negotiates. (A moving zoom or position after the grade is not in the way: the placement reads the 4:2:0
    /// planes the grade hands on, and every grade is exact behind it.)
    fn grade_chain_refusal(&self, layer: &PlanLayer, ops: &[FadeOp]) -> Option<&'static str> {
        if layer.stream.pix_fmt.as_deref() != Some("yuv420p") {
            Some("on a picture that is not 8-bit 4:2:0 yuv420p (FFmpeg grades it in the format it negotiates for that picture)")
        } else if layer.grade_route().is_none() {
            Some("on a picture whose colour matrix has no measured round trip (BT.601, BT.709 and untagged are drawn)")
        } else if layer.effects.iter().any(|e| !matches!(e, VideoEffect::ChromaKey { .. })) {
            Some("under video effects (FFmpeg runs them on the RGB or 4:4:4 picture it negotiates after the grade)")
        } else if self.white_round_trip(layer) || ops.iter().any(|o| o.tint != FadeTint::Alpha && o.factor < FADE_FULL) {
            Some("while a fade to black or white runs on it (FFmpeg fades the RGB picture the grade leaves)")
        } else if self.canvas.mixed_matrices_cut && self.canvas.policy != CompositeColorPolicy::FixedBt601 {
            Some("in a cut that mixes colour matrices (FFmpeg 9 converts the picture the grade leaves into the bottom layer's matrix inside the graph, on every frame of the clip, not only where the two are on screen together)")
        } else {
            None
        }
    }

    /// What keeps a compositor with `caps` from drawing the stack's colour: [`composite_matrix`]'s reasons, less the
    /// mixed matrices of FFmpeg 9 ([`CompositeColorPolicy::BottomLayerTag`]) that it converts — every layer whose matrix is not
    /// the composite's ([`PlanCanvas::matrix`]: the bottom layer's tag, or in a cut the first clip's) goes through
    /// swscale's two-step conversion into it, which the compositor reproduces for a plain opaque `yuv420p` picture
    /// (and a dissolving one), measured on this FFmpeg's graph (`GpuCaps::matrix_convert`) and refused for the rest
    /// ([`RenderPlan::convert_refusal`]).
    fn matrix_reasons(&self, caps: &GpuCaps) -> Vec<Unsupported> {
        let policy = self.canvas.policy;
        let (_, mut why) = composite_matrix(&self.layers, policy);
        if policy != CompositeColorPolicy::BottomLayerTag {
            return why;
        }
        why.retain(|u| !matches!(u, Unsupported::MixedMatrices | Unsupported::RgbInStack(_)));
        let composite = self.canvas.matrix;
        if self.layers.iter().any(|l| l.stream.is_rgb()) && (composite != YuvMatrix::Bt601 || self.canvas.cut_non_601) {
            why.push(Unsupported::RgbInStack(composite));
        }
        // The deciding clip of a mixed cut is read off the graph's shape (`PlanCanvas::cut_matrix`), and a compositor whose conversion
        // is not measured on this FFmpeg has not had that reading checked either (the probe runs the three-clip graph in the same
        // pass as the conversions, and settles on neither unless both match): no layer of such a cut is drawn, the deciding clip's
        // frames included.
        if self.mode == PlanMode::Motion
            && self.canvas.mixed_matrices_cut
            && (self.canvas.cut_matrix.is_none() || self.canvas.unread_decider_in_cut || !caps.matrix_convert)
        {
            why.push(Unsupported::MixedMatrices);
        }
        let converting = |l: &PlanLayer| !l.stream.is_rgb() && l.stream.matrix().is_some_and(|m| m != composite);
        let any_converted = self.layers.iter().any(converting);
        for (n, layer) in self.layers.iter().enumerate() {
            // A grade in a stack of mixed matrices, wherever it is: its chain is the RGB filters and the way back, and what the
            // negotiation makes of the stack's matrix around it is not measured (an untagged graded clip under a BT.709 one is
            // 686205 luma samples off the conversion of the layer above it alone).
            if any_converted && !layer.grade.is_identity() && !converting(layer) {
                why.push(Unsupported::MixedMatricesWith(
                    self.layer_ref(n, layer),
                    "a grade in a stack of mixed matrices",
                ));
            }
            if !converting(layer) {
                continue;
            }
            let at = self.layer_ref(n, layer);
            if !caps.matrix_convert {
                why.push(Unsupported::MixedMatricesWith(at, "a compositor with no measured conversion"));
            } else if let Some(detail) = self.convert_refusal(layer) {
                why.push(Unsupported::MixedMatricesWith(at, detail));
            }
        }
        why
    }

    /// Why a layer whose last `scale` resizes it **and** converts its matrix in the same call (a still's, a dissolving clip's: the
    /// chain has no `format` ahead of the overlay, or ends in `yuva420p`) is not drawn by a compositor with `caps`, or `None`.
    /// swscale then cascades `yuv420p -> bgr24` at the destination's size and back (`ScaleStage::cascade_refusal` for the pictures
    /// it is reproduced for, `GpuCaps::matrix_convert_scaled` for the builds it was measured on), which neither a conversion after
    /// the resize nor one before it is. Judged for a layer [`RenderPlan::convert_refusal`] has passed.
    pub fn scaled_convert_refusal(&self, layer: &PlanLayer, geom: &LayerGeometry, caps: &GpuCaps) -> Option<&'static str> {
        const RESIZES: &str = "a layer whose last scale resizes it in a still or while it dissolves (the scale converts the matrix in the same pass)";
        if !caps.matrix_convert_scaled {
            return Some(RESIZES);
        }
        // The cascade's tables are the default scaler's (and `bicubic`'s): a delivery that names another puts its flags in this very `scale`.
        if self.scaler_named_other() {
            return Some(
                "a layer whose last scale resizes it and converts the matrix at once, in a delivery that names a scaler (the cascade is reproduced with the default scaler's tables)",
            );
        }
        let stage = geom.stages.last()?;
        if layer.stream.pix_fmt.as_deref() != Some("yuv420p") {
            Some("a layer whose last scale resizes it and converts its format and matrix in one pass (a picture that is not 4:2:0)")
        } else if layer.blur_fill.is_some() {
            Some("a blurred fill behind a layer whose last scale resizes it and converts the matrix")
        } else {
            stage.cascade_refusal()
        }
    }

    /// Why a layer whose matrix is not the composite's cannot be converted into it by the compositor, or `None`. FFmpeg 9
    /// puts a `scale` between the layer's chain and the overlay, after the chain's last `format`, and swscale converts a
    /// `yuv420p` picture through `bgr24` with the source's matrix and back with the destination's (or, on an alpha
    /// plane, `bgra` and the generic scaler); what is reproduced is that conversion of an opaque (or dissolving) 8-bit
    /// limited-range picture at the size the chain has made it. Anything else of the chain that touches the picture
    /// before it, or whose own format the conversion would change, stays FFmpeg's.
    pub fn convert_refusal(&self, layer: &PlanLayer) -> Option<&'static str> {
        let motion = self.mode == PlanMode::Motion;
        // (A 4:2:2 or 4:4:4 picture reaches the conversion as the 4:2:0 the decode makes of it, the way a `format=yuv420p` ahead of
        // the converter does: measured exact beside the plain ones.)
        if !matches!(layer.stream.pix_fmt.as_deref(), Some("yuv420p" | "yuv422p" | "yuv444p")) || layer.is_image {
            Some("a picture that is not 8-bit 4:2:0, 4:2:2 or 4:4:4 yuv video")
        } else if !motion && layer.stream.pix_fmt.as_deref() != Some("yuv420p") {
            // (A still has no `format` ahead of the overlay: the fit `scale` takes the picture to 4:2:0 and into the composite's matrix in
            // one pass, through RGB from the 4:4:4 samples.)
            Some("a 4:2:2 or 4:4:4 picture in a still (its scale converts the format and the matrix in one pass)")
        } else if layer.stream.maybe_full_range_in(self.canvas.policy) {
            Some("a picture that may be full range")
        } else if !layer.grade.is_identity() {
            Some("a grade (its way back converts the picture into the bottom layer's matrix itself)")
        } else if layer
            .effects
            .iter()
            .any(|e| !motion || !matches!(e, VideoEffect::Invert | VideoEffect::Grayscale))
        {
            // (Invert and grayscale are tables the export's fit `scale` applies ahead of the converter `scale`; a still's chain has
            // no such converter, and its composite is not the conversion of the effect's picture: measured, 470555 samples differ.)
            Some("a video effect other than invert and grayscale in an export")
        } else if layer.mask.is_some() {
            Some("a mask")
        } else if layer.transform.rotation != 0.0 || layer.transform.opacity < 1.0 || layer.keyed_alpha.is_some() {
            Some("a rotation or an opacity below 1 (an alpha plane the conversion also reads)")
        } else if !layer.color.is_identity() || self.eq_in_chain(layer) {
            Some("colour correction")
        } else if layer.hdr.is_some() || layer.reframe.is_some() || layer.projection.is_some() {
            Some("an HDR, reframed or 360 picture")
        } else if layer.blur_fill.is_some() && self.on_alpha_plane(layer) {
            // (A plain layer over a blurred fill is converted after the whole frame, fill included. A dissolving one has its fill's two
            // scales tied to the composite's colour space, which convert inside themselves.)
            Some("a blurred fill behind a layer that dissolves")
        } else if motion && layer.animated.is_some() {
            Some("a keyed number")
        } else if self.white_round_trip(layer) || self.through_rgba(layer) {
            Some("a dip to white")
        } else if self.fade_ops(layer).iter().any(|o| {
            // A plain chain of an export fades the picture ahead of its converter `scale`: the compositor fades the planes, then
            // converts them. A still's, and a chain on the alpha plane (whose fade sits after its fit `scale`, behind a ramp), are
            // not measured.
            o.tint != FadeTint::Alpha
                && o.factor < FADE_FULL
                && !(motion && o.tint == FadeTint::Black && !self.on_alpha_plane(layer))
        }) {
            Some(
                "a fade to black running on it in a still or on the alpha plane (FFmpeg fades the picture before the conversion)",
            )
        } else {
            None
        }
    }

    /// Why a compositor that draws masks still does not draw this layer's: the mask is `geq`
    /// rewriting the alpha plane, ahead of `colorchannelmixer` (the RGB round trip a constant opacity below 1
    /// takes, which is arithmetic on that alpha: `lrint(level * opacity)`, the clip's `fade alpha=1` steps,
    /// then the conversion back to `yuva420p`, drawn level by level, [`crate::layer_geometry::ffmpeg_alpha_of`], where
    /// the translucent round trip itself is) and `rotate` (which turns the alpha plane with
    /// the picture, a partly transparent edge included). The opacity judged is the sampled one, the
    /// frame's own: a clip whose keys hold 1 here has no alpha to round-trip (a keyed opacity as such
    /// is `KeyedOpacity`'s). In an export frame the `geq` that writes a keyed opacity is the mask's
    /// own (`(keep)*(curve)*alpha(X,Y)`, one product in `f64`, [`crate::mask_alpha_plane_keyed`]; the plan carries the curve's value,
    /// [`PlanLayer::keyed_opacity`]): there is no round trip then (the graph has no `colorchannelmixer` for a keyed opacity). A blend
    /// mode on a masked layer below full opacity, either way, is beyond the blend chain's probe, which carries the opaque plane of
    /// a constant opacity alone. **A chroma key's alpha plane follows the same rule** (`chromakey` writes it where the `geq` writes a
    /// mask's): `colorchannelmixer` maps a key's levels as a mask's, and a keyed opacity's `geq` multiplies the curve into them.
    fn mask_refusal(&self, layer: &PlanLayer) -> Option<&'static str> {
        self.alpha_plane_refusal(layer).map(|why| match why {
            AlphaPlaneRefusal::Rotated => "on a rotated layer",
            AlphaPlaneRefusal::BlendedBelowFull => "on a blended layer below full opacity",
        })
    }

    /// What a layer whose alpha plane is a mask's or a key's cannot have (see [`RenderPlan::mask_refusal`], which words it for a mask).
    fn alpha_plane_refusal(&self, layer: &PlanLayer) -> Option<AlphaPlaneRefusal> {
        let rotated =
            layer.transform.rotation != 0.0 || (self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.rotates));
        // (A clip with a keyed number samples its opacity too, so `transform.opacity` is the curve's value in an export frame: below 1 is
        // the constant round trip or the keyed `geq`, which `through_rgba` and `keyed_alpha` tell apart, and both are drawn.)
        let below_full = layer.transform.opacity < 1.0 || layer.keyed_alpha.is_some_and(|a| a < 255);
        if rotated {
            Some(AlphaPlaneRefusal::Rotated)
        } else if below_full && !layer.blend.is_normal() {
            Some(AlphaPlaneRefusal::BlendedBelowFull)
        } else {
            None
        }
    }

    /// What is ahead of a moving zoom or position that a compositor with `keyed_zoom` still does not draw: a
    /// rotation and an opacity below 1 (the placement would act on the rotated alpha plane or on the RGB round
    /// trip's, neither of which a case holds to FFmpeg's), and a dip to white (the placement chain's
    /// `format=yuva420p` takes the RGB frame `fade c=white` leaves and converts it to 4:2:0 in the same pass, which neither FFmpeg 6.1 nor
    /// 9.0 does as a conversion of the converted picture: up to 14 levels off on 6.1, 3 on 9.0). A grade is not here: the placement reads the
    /// 4:2:0 planes the grade hands on, and every grade (wheels, curves, LUTs of 17, 33 and 2 points, all three)
    /// is exact behind it on 6.1.1 and 9.0.2, in an export frame and in a still.
    fn zoom_order_refusal(&self, layer: &PlanLayer, ops: &[FadeOp]) -> Option<&'static str> {
        let rotated =
            layer.transform.rotation != 0.0 || (self.mode == PlanMode::Motion && layer.animated.is_some_and(|a| a.rotates));
        if rotated {
            Some("a rotation")
        } else if layer.transform.opacity < 1.0 || layer.keyed_alpha.is_some_and(|a| a < 255) {
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

    /// Whether FFmpeg makes this layer's fit picture for the compositor ([`FitCopy`]): a still image whose zoom or position moves and
    /// that the export draws as footage with a placement (not through [`RenderPlan::takes_still_route`]), with nothing between its
    /// fit and the placement that the compositor would have to draw on a picture it did not scale (colour, a grade, an effect, a mask,
    /// a rotation, an opacity below 1, a dip to white).
    pub fn fits_by_ffmpeg(&self, layer: &PlanLayer) -> bool {
        layer.is_image
            && layer.still.is_none()
            && layer.animated.is_some_and(|a| a.places && !a.still_motion)
            && !self.eq_in_chain(layer)
            && layer.grade.is_identity()
            && layer.effects.is_empty()
            && layer.mask.is_none()
            && layer.transform.rotation == 0.0
            && layer.transform.opacity >= 1.0
            && layer.keyed_alpha.is_none()
            && !layer.animated.is_some_and(|a| a.rotates || a.opacity)
            && !self.white_round_trip(layer)
            && layer.reframe.is_none()
            && layer.hdr.is_none()
            && layer.stream.subsampling().is_some()
            && self.still_kind(layer).is_some()
    }

    /// The kind of picture a compositor measures the moving still's chains for ([`crate::StillKinds`]), if `layer`'s is one: a
    /// JPEG (`yuvj420p`) or an RGB picture (`rgb24`), not one flagged full range in another format.
    pub fn still_kind(&self, layer: &PlanLayer) -> Option<crate::plan_caps::StillKinds> {
        layer
            .stream
            .pix_fmt
            .as_deref()
            .and_then(crate::plan_caps::StillKinds::of_pix_fmt)
            .filter(|k| *k != crate::plan_caps::StillKinds::YUV420P && !layer.stream.raw_full_range())
    }

    /// Whether the delivery names the `bicubic` scaler (`:flags=bicubic` in every `scale` of a moving still's text).
    fn scaler_named_bicubic(&self) -> bool {
        // (The engine writes `:flags=` for a name in its own list, spelled as it is — `SCALERS`, `scale_flags` — and validates an export's
        // name against it: `Bicubic` is no name there, and names no flags in a still's graph.)
        self.canvas.scaler.as_deref() == Some("bicubic")
    }

    /// Whether the delivery names a scaler other than `bicubic`, spelled as the engine's list has it: `:flags=<name>` is then part of
    /// every `scale` of a moving still's text, in chains no probe has measured (a chain is measured with the default scaler and with
    /// `bicubic` named), so such a still is FFmpeg's whatever the compositor's own scaler is measured to be; **except the move inside
    /// the frame** ([`RenderPlan::takes_still_route`]), which is measured under each name ([`RenderPlan::still_route_scaler`]).
    fn scaler_named_other(&self) -> bool {
        self.canvas
            .scaler
            .as_deref()
            .is_some_and(|s| ScalerKinds::of_name(s).is_some())
    }

    /// The scaler a delivery names besides `bicubic` (`bilinear`, `lanczos`, `neighbor` or `spline`, spelled as the engine's list has it) when
    /// `layer` is a moving still drawn by the route: its move inside the frame is then a chain of its own, `scale=...:flags=<name>`
    /// everywhere, which a compositor measures per kind of picture and name ([`crate::StillKinds::named_route`]). `None` for any other layer
    /// and for a delivery that names no other scaler (the chain is [`RenderPlan::still_chain`]'s). The fit picture of a placement has no
    /// such measurement: it is FFmpeg's under any name ([`RenderPlan::scaler_named_other`]).
    pub fn still_route_scaler(&self, layer: &PlanLayer) -> Option<ScalerKinds> {
        if !self.takes_still_route(layer) {
            return None;
        }
        self.canvas.scaler.as_deref().and_then(ScalerKinds::of_name)
    }

    /// The chain FFmpeg makes `layer`'s moving-still picture by at a render of `size` ([`crate::StillChain`]): the route's own scaled copy, or the
    /// fit picture of a placement in the shape its text has. `None` for a layer neither route draws (or whose fit picture cannot be made, or
    /// whose chain a delivery's named scaler changes to one nothing measured: the route under such a name is
    /// [`RenderPlan::still_route_scaler`]'s).
    pub fn still_chain(&self, layer: &PlanLayer, size: (u32, u32)) -> Option<crate::plan_caps::StillChain> {
        if self.scaler_named_other() {
            return None;
        }
        let bicubic = self.scaler_named_bicubic();
        if self.takes_still_route(layer) {
            return Some(crate::plan_caps::StillChain::inside(bicubic));
        }
        let fit = self.fit_copy(layer, size)?;
        Some(crate::plan_caps::StillChain::edge(fit.shape, bicubic))
    }

    /// The fit picture of `layer` at a render of `size`: the stamped one when it was made for that size, else what it would be made as.
    /// `None` for a layer FFmpeg does not make it for, and for one the geometry cannot place.
    pub fn fit_copy(&self, layer: &PlanLayer, size: (u32, u32)) -> Option<FitCopy> {
        if !self.fits_by_ffmpeg(layer) || self.scaler_named_other() {
            return None;
        }
        if let Some(fit) = &layer.fit {
            return (fit.frame == size).then(|| fit.clone());
        }
        let geom = self.layer_geometry(layer, size).ok()?;
        // (The delivery's scaler is the default one or bicubic named: a fit picture is not made for any other, `scaler_named_other`. The engine
        // writes the flags of a name in its own list, spelled as it is: `scaler_named_bicubic`.)
        let flags = if self.scaler_named_bicubic() { ":flags=bicubic" } else { "" };
        // A Cover frame fitted by the one `scale` and `crop` the engine writes (a static crop first, the cover of the frame, cut to it): the
        // graph's own text, which is the graph's for a picture of whatever size the file is when it is decoded. The probe's sizes are
        // not in it, so a file replaced since it was probed, or one the probe saw turned, is fitted as the graph fits it.
        let t = &layer.transform;
        if self.canvas.fit == Fit::Cover
            && geom.stages.len() == 1
            && geom.picture == size
            && (geom.stages[0].keep.w, geom.stages[0].keep.h) == size
        {
            let mut filters = Vec::new();
            if t.has_crop() {
                let cw = (1.0 - t.crop_left - t.crop_right).max(0.0);
                let ch = (1.0 - t.crop_top - t.crop_bottom).max(0.0);
                filters.push(format!(
                    "crop=w=iw*{cw}:h=ih*{ch}:x=iw*{cl}:y=ih*{ct}",
                    cl = t.crop_left,
                    ct = t.crop_top
                ));
            }
            filters.push(format!(
                "scale={}:{}:force_original_aspect_ratio=increase{flags}",
                size.0, size.1
            ));
            filters.push(format!("crop={}:{}", size.0, size.1));
            return Some(FitCopy {
                filters: filters.join(","),
                shape: if t.has_crop() {
                    crate::plan_caps::FitShape::EngineCrop
                } else {
                    crate::plan_caps::FitShape::Engine
                },
                size,
                frame: size,
            });
        }
        let whole = |r: &crate::layer_geometry::Rect, w: u32, h: u32| r.x == 0 && r.y == 0 && r.w == w && r.h == h;
        let (mut w, mut h) = (layer.stream.width, layer.stream.height);
        // The sizes are the probe's: a picture that decodes at another size (an EXIF orientation the probe did not apply, a file replaced since)
        // would be fitted by the graph to sizes of its own, so the copy opens with a `crop` whose width is the probed one only for a picture of
        // exactly the probed size and nothing for any other, which FFmpeg refuses ("Invalid too big or non positive size"), a larger picture
        // included. It is `exact`: a 4:2:0 picture's `crop` is otherwise cut to the chroma grid, and an odd picture would lose the row or the
        // column the graph's own `scale` keeps.
        let mut filters: Vec<String> = vec![format!("crop=w='if(eq(iw,{w})*eq(ih,{h}),{w},0)':h={h}:x=0:y=0:exact=1")];
        // Whether the text holds a `crop` besides the guard: a text no toy cut of the probe takes (`FitShape::Cropped`).
        let mut cropped = false;
        for stage in &geom.stages {
            if !whole(&stage.src, w, h) {
                cropped = true;
                filters.push(format!(
                    "crop={}:{}:{}:{}",
                    stage.src.w, stage.src.h, stage.src.x, stage.src.y
                ));
            }
            filters.push(format!("scale={}:{}{flags}", stage.scaled.0, stage.scaled.1));
            if !whole(&stage.keep, stage.scaled.0, stage.scaled.1) {
                cropped = true;
                filters.push(format!(
                    "crop={}:{}:{}:{}",
                    stage.keep.w, stage.keep.h, stage.keep.x, stage.keep.y
                ));
            }
            (w, h) = (stage.keep.w, stage.keep.h);
        }
        Some(FitCopy {
            filters: filters.join(","),
            shape: if cropped {
                crate::plan_caps::FitShape::Cropped
            } else {
                crate::plan_caps::FitShape::Staged
            },
            size: geom.picture,
            frame: size,
        })
    }

    /// This plan with the fit pictures FFmpeg makes ([`FitCopy`]) worked out for `size`.
    fn stamp_fits(mut self, size: (u32, u32)) -> Self {
        let fits: Vec<Option<FitCopy>> = self
            .layers
            .iter()
            .map(|l| if l.fit.is_none() { self.fit_copy(l, size) } else { None })
            .collect();
        for (layer, fit) in self.layers.iter_mut().zip(fits) {
            if fit.is_some() {
                layer.fit = fit;
            }
        }
        self
    }

    /// Whether the export's own route for a moving still is this layer's: a still image on a Ken Burns move whose picture
    /// covers the frame at every frame ([`crate::still_motion`]). Its fit, zoom and position are one `perspective` of a
    /// scaled copy, so no stage of the general chain (the fit's crop, the zoom's `scale`, the placement) is its.
    pub fn takes_still_route(&self, layer: &PlanLayer) -> bool {
        layer.still.is_some() && layer.animated.is_some_and(|a| a.still_motion)
    }

    /// Why the copy FFmpeg makes of a still, `size` a side, cannot be taken by a compositor: the sides it takes are even and up to
    /// [`MAX_COPY_SIDE`].
    fn copy_size_refusal((w, h): (u32, u32)) -> Option<String> {
        if w == 0 || h == 0 || w > MAX_COPY_SIDE || h > MAX_COPY_SIDE || w % 2 != 0 || h % 2 != 0 {
            Some(format!(
                "a {w}x{h} copy (the compositor takes even sides up to {MAX_COPY_SIDE} px)"
            ))
        } else {
            None
        }
    }

    /// What drawing a moving still's route at `size` would refuse of the copy and the move, which the plan knows: the copy's sides, the
    /// `perspective` the move's four corners make and the size of the tables the draw would hold.
    ///
    /// The `perspective` check is `PlacementMap::corners_have_a_map` (constant time: corners the filter's fixed-point range holds and a
    /// map that is not degenerate; `from_corners` fails on nothing else for a rectangle, which a zoom and a pan always make). **The tables
    /// are not always small**: `from_corners` keeps one entry per column and per row (`cw + ch`) while every position rounds the same way
    /// along a column, and falls back to the filter's per-pixel table (`cw * ch` entries) when one does not (about one move in three
    /// thousand, and the plan cannot tell which without building them). The draw refuses a table over the device's storage buffer, so the
    /// plan refuses the frame whose per-pixel table would be over the 128 MiB every device has ([`MAX_TABLE_BYTES`]): a bound of the
    /// frame's size, in constant time, that the draw never exceeds (a device's smaller buffer is still the draw's to refuse).
    fn still_draw_refusal(still: &StillLayer, motion: &crate::still_motion::StillMotion, size: (u32, u32)) -> Option<String> {
        let (_, _, w, h) = motion.window;
        if let Some(why) = Self::copy_size_refusal((w, h)) {
            return Some(why);
        }
        let table = u64::from(size.0) * u64::from(size.1) * 4;
        if table > MAX_TABLE_BYTES {
            return Some(format!(
                "a {}x{} frame, whose per-pixel placement table ({table} bytes) would be over the {MAX_TABLE_BYTES}-byte storage buffer every device has",
                size.0, size.1
            ));
        }
        PlacementMap::corners_have_a_map(PlacementFrame::window((w, h), size), &motion.corners(&still.pose))
            .err()
            .map(|e| format!("a move whose perspective has no table ({e})"))
    }

    /// Why a compositor with `caps` cannot draw `layer`'s moving still, or `None` (the render size is judged apart, in
    /// [`RenderPlan::reasons`]): the picture must be a kind it measured, and nothing may follow the move that was not
    /// held to FFmpeg's (colour, a grade, an effect, a mask, a translucent layer or a dip to white).
    fn still_route_refusal(&self, layer: &PlanLayer, caps: &GpuCaps, ops: &[FadeOp]) -> Option<String> {
        if !self.takes_still_route(layer) {
            return Some("the plan holds no route for it".into());
        }
        // (A name besides `bicubic`: `:flags=<name>` is part of every `scale` of the still's graph. Its move inside the frame is measured
        // under that name, per kind of picture, below.)
        let named = self.still_route_scaler(layer);
        let stream = &layer.stream;
        // Where the composite's range is negotiated FFmpeg keeps a full-range picture's raw values through the graph and converts the
        // composite last (and flags a whole export full range): not what a compositor on limited-range planes draws, whatever the chain
        // the picture takes (`FullRangeCut`, `FullRangeNegotiated`, `PlacedFormat`). The move's own chain matches the model on 9.0.2
        // (the probe says so for a JPEG moving inside the frame); the composite it is laid on is what does not.
        if self.canvas.policy != CompositeColorPolicy::FixedBt601 && stream.maybe_full_range_in(self.canvas.policy) {
            return Some("a full-range picture where the composite's range is negotiated".into());
        }
        let Some(kind) = self.still_kind(layer) else {
            let fmt = stream.pix_fmt.as_deref().unwrap_or("not recorded");
            return Some(format!(
                "a {fmt} picture{}",
                if stream.raw_full_range() { " flagged full range" } else { "" }
            ));
        };
        if !caps.still_motion.contains(kind) {
            return Some(format!(
                "a {} picture this compositor has not measured the chain of on this FFmpeg",
                stream.pix_fmt.as_deref().unwrap_or("?")
            ));
        }
        // ... and the chain this delivery's scaler gives it (`:flags=bicubic` is part of every `scale` of the text; for any other name the
        // move inside the frame is measured under that name).
        if let Some(name) = named {
            if !caps.still_motion.contains_named_route(kind, name) {
                return Some(format!(
                    "a {} picture whose move inside the frame this compositor has not measured FFmpeg making with the `{}` scaler named on this FFmpeg",
                    stream.pix_fmt.as_deref().unwrap_or("?"),
                    self.canvas.scaler.as_deref().unwrap_or_default()
                ));
            }
        } else if !caps
            .still_motion
            .contains_chain(kind, crate::plan_caps::StillChain::inside(self.scaler_named_bicubic()))
        {
            return Some(format!(
                "a {} picture this compositor has not measured the chain of with the bicubic scaler named on this FFmpeg",
                stream.pix_fmt.as_deref().unwrap_or("?")
            ));
        }
        if self.eq_in_chain(layer)
            || !layer.grade.is_identity()
            || !layer.effects.is_empty()
            || layer.mask.is_some()
            || layer.transform.opacity < 1.0
            || layer.keyed_alpha.is_some()
            || self.white_round_trip(layer)
            || ops.iter().any(|o| o.tint == FadeTint::White)
        {
            return Some("with colour, a grade, an effect, a mask, a translucent layer or a dip to white".into());
        }
        None
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
    ///
    /// A still's `pad` of an odd-height picture `scale` did not resample drops its odd last row ([`RenderPlan::odd_pad`]):
    /// `picture_shows` says so.
    pub fn layer_geometry(
        &self,
        layer: &PlanLayer,
        size: (u32, u32),
    ) -> std::result::Result<LayerGeometry, crate::layer_geometry::GeometryError> {
        let mut geom = self.layer_geometry_resolved(layer, size)?;
        if matches!(self.odd_pad(layer, &geom, size), OddPad::Drops) {
            geom.picture_shows.1 &= !1;
        }
        Ok(geom)
    }

    /// [`RenderPlan::layer_geometry`] as [`LayerGeometry`] resolves it, before the odd row of a still's `pad` is settled.
    fn layer_geometry_resolved(
        &self,
        layer: &PlanLayer,
        size: (u32, u32),
    ) -> std::result::Result<LayerGeometry, crate::layer_geometry::GeometryError> {
        let src = (layer.stream.width, layer.stream.height);
        let (fit, tf) = (self.canvas.fit, &layer.transform);
        let place = self.placement(layer);
        // A moving still comes out of its `perspective` at the frame's size, fitted, zoomed and moved: a frame-sized
        // picture, of a 4:2:0 `yuva420p` whatever the file was, that nothing in the general chain acted on.
        if self.takes_still_route(layer) {
            let tf = Transform {
                opacity: tf.opacity,
                ..Transform::default()
            };
            let place = Placement {
                keyframed: None,
                ..place
            };
            return LayerGeometry::resolve_with(size, size, fit, &tf, Subsampling::YUV420, place);
        }
        let Some(native) = layer.stream.subsampling() else {
            return LayerGeometry::resolve_any_grid_with(src, size, fit, tf, place);
        };
        let geom = LayerGeometry::resolve_with(src, size, fit, tf, native, place)?;
        // (FFmpeg crops a picture it makes for the compositor on the picture's own chroma grid, as the graph does.)
        if native != Subsampling::YUV420 && layer.stream.layout() != Some(PixLayout::Gray) && !self.fits_by_ffmpeg(layer) {
            let on_420 = LayerGeometry::resolve_with(src, size, fit, tf, Subsampling::YUV420, place)?;
            // The grids agree on where the picture goes but for the odd last row: a native format that is not 4:2:0 is converted
            // by `scale` into the buffer `pad` hands it, and `pad` fills in place (the row goes), where a 4:2:0 picture is forwarded
            // untouched and copied. That is the native grid's geometry, the one drawn (the compositor's planes are the decode's
            // 4:2:0 either way), and no crop window is between two chroma samples.
            let same_place = LayerGeometry {
                picture_shows: on_420.picture_shows,
                ..geom.clone()
            };
            if same_place != on_420 {
                return Err(crate::layer_geometry::GeometryError(format!(
                    "its crop window or Cover offset starts or ends between two 4:2:0 chroma samples of a picture whose chroma is finer ({}), which the compositor's 4:2:0 planes cannot place",
                    layer.stream.pix_fmt.as_deref().unwrap_or("?")
                )));
            }
        }
        Ok(geom)
    }

    /// The thumbnail chain of `layer`'s blurred fill at render size `size`, when the layer takes one: the canvas fits
    /// over a blurred copy, the clip is placed by the plain fit ([`PlanLayer::blur_fill`]) and its picture is not the
    /// shape of the frame (the graph's own test, on the product of the sides). `Ok(None)` for a layer that does not,
    /// `Err` for one the thumbnail or `boxblur` could not be run for as the sizes fall.
    pub fn blur_fill(
        &self,
        layer: &PlanLayer,
        size: (u32, u32),
    ) -> std::result::Result<Option<crate::blur_fill::BlurFill>, crate::layer_geometry::GeometryError> {
        let Some(shape) = layer.blur_fill.filter(|_| self.canvas.fit == Fit::Blur) else {
            return Ok(None);
        };
        if !crate::engine::picture_has_other_shape(Some(shape), size) {
            return Ok(None);
        }
        crate::blur_fill::BlurFill::resolve((layer.stream.width, layer.stream.height), size).map(Some)
    }

    /// Why `layer`'s blurred fill is not drawn by a compositor with `caps` (`None`: it is, or there is none to draw);
    /// `Some(None)` is a compositor without the pass (or without the exact scaler under it), `Some(Some(detail))` the
    /// layer or the sizes.
    fn blur_fill_refusal(&self, caps: &GpuCaps, layer: &PlanLayer, size: (u32, u32)) -> Option<Option<&'static str>> {
        let fill = match self.blur_fill(layer, size) {
            Ok(Some(fill)) => fill,
            Ok(None) => return None,
            Err(_) => return Some(Some(
                "a frame too small for the thumbnail's box blur, or a picture whose aspect makes the thumbnail's cover absurd",
            )),
        };
        if !caps.blur_fill || !caps.exact_scaler {
            return Some(None);
        }
        // `boxblur`, `scale` and `overlay` take the picture in its own format: the compositor's planes are the
        // decode's 8-bit limited-range 4:2:0 (the fill of anything else is another picture), an HDR picture is
        // tone-mapped ahead of the fit and composes in 10 bits.
        if layer.hdr.is_some() {
            return Some(Some("an HDR picture (the fill composes in 10 bits, ahead of the tone-map)"));
        }
        if layer.stream.pix_fmt.as_deref() != Some("yuv420p") {
            return Some(Some(
                "a picture that is not 8-bit `yuv420p` (the thumbnail is scaled and blurred in the picture's own format)",
            ));
        }
        if layer.stream.maybe_full_range() {
            return Some(Some("a full-range picture (FFmpeg scales and blurs its raw values)"));
        }
        if shrinks_past_limit(fill.source.0, fill.cover.0) || shrinks_past_limit(fill.source.1, fill.cover.1) {
            return Some(Some("a picture shrunk past 63:1 into the thumbnail"));
        }
        None
    }

    /// [`RenderPlan::layer_geometry`] on the picture's own chroma grid (4:2:0 where it is not known), with no
    /// check that the compositor's 4:2:0 planes can place it: what [`RenderPlan::reasons`] judges a layer's
    /// other reasons by once the geometry itself is refused. `None` where no grid gives a geometry.
    fn native_grid_geometry(&self, layer: &PlanLayer, size: (u32, u32)) -> Option<LayerGeometry> {
        let src = (layer.stream.width, layer.stream.height);
        let grid = layer.stream.subsampling().unwrap_or(Subsampling::YUV420);
        LayerGeometry::resolve_with(src, size, self.canvas.fit, &layer.transform, grid, self.placement(layer)).ok()
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
            placed: layer
                .animated
                .filter(|a| a.places && !a.still_motion)
                .map(|a| crate::layer_geometry::Placed { zoom: a.zooms }),
        }
    }

    /// Where the placement puts `layer`'s picture at this frame, for a clip whose zoom or position moves
    /// ([`Animated::places`]; `None` for any other): the zoom if it is the one that moves (a held one is a
    /// `scale` ahead of the placement), and the position the chain's expressions evaluate to with a slide's or
    /// push's travel added, in canvas widths and heights — the numbers `engine::cli::placement_filters`
    /// writes into the `perspective` corners.
    pub fn pose(&self, layer: &PlanLayer) -> Option<Pose> {
        // (A moving still image is drawn by its own graph, `Animated::still_motion`, not by this chain.)
        let a = layer.animated.filter(|a| a.places && !a.still_motion)?;
        let (mx, my) = layer.fx.motion;
        Some(Pose {
            scale: a.zooms.then_some(layer.transform.scale),
            pos: (layer.transform.pos_x + mx, layer.transform.pos_y + my),
        })
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

    /// Why a layer below full opacity whose picture (`size`) has an odd side is not drawn, when it is not: the compositor has no model of this
    /// FFmpeg's odd sizes (`caps`), or the layer is one the model was not held to — a side under
    /// [`TRANSLUCENT_ODD_MIN_SIDE`](crate::layer_geometry::TRANSLUCENT_ODD_MIN_SIDE), a turn, a blend mode, a grade or an effect other
    /// than a chroma key (the key's alpha plane is the layer's, which the round trip maps level by level). (A translucent layer is never
    /// letterboxed: the identity pose that `pad`s a picture to the frame has full opacity.) Plain, colour-corrected, masked, keyed and dissolving layers are held to
    /// FFmpeg in `parity.rs` / `motion_parity.rs` (`odd translucent`). The text always begins as the refusal always did.
    fn translucent_odd_refusal(
        &self,
        caps: &GpuCaps,
        layer: &PlanLayer,
        geom: &LayerGeometry,
        (w, h): (u32, u32),
    ) -> Option<String> {
        let head = format!("a translucent layer of odd size {w}x{h}");
        let because = |why: &str| Some(format!("{head} ({why})"));
        if !caps.translucent_odd {
            return because("FFmpeg's RGB round trip reads past the picture");
        }
        if w.min(h) < crate::layer_geometry::TRANSLUCENT_ODD_MIN_SIDE {
            return because("a side under 16, where the chroma filter has two taps");
        }
        if geom.rotation.is_some() {
            return because("turned");
        }
        if !layer.blend.is_normal() {
            return because("under a blend mode");
        }
        if !layer.grade.is_identity() {
            return because("graded");
        }
        if layer.effects.iter().any(|e| !matches!(e, VideoEffect::ChromaKey { .. })) {
            return because("with an effect other than a chroma key");
        }
        None
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

    /// This plan for a render of `size`: the moving stills' scaled copies worked out for the instant at that size, as the FFmpeg
    /// still makes them (an export frame's are the clip's own and are not touched). A still plan's moving still is drawn only
    /// from a plan that has been through this at the size it is composited at.
    pub fn for_render(mut self, size: (u32, u32)) -> Self {
        for layer in &mut self.layers {
            if let Some(still) = layer.still.as_mut().filter(|s| !s.clip_wide) {
                still.export = crate::still_motion::StillMotion::build(&still.stat, size, &[still.sample]);
            }
            // (A fit picture stamped for another size is made again for this one.)
            if layer
                .fit
                .as_ref()
                .is_some_and(|f| f.frame != size && self.mode == PlanMode::Still)
            {
                layer.fit = None;
            }
        }
        self.stamp_fits(size)
    }

    /// The matrix a blended clip's conversions into RGB and back are made in ([`PlanCanvas::blend_tag`]): BT.601 for an untagged
    /// bottom layer and the `smpte170m` / `bt470bg` tags, BT.709 for the tag or the graph's label; `None` for any other
    /// (`fcc`, `smpte240m`), which the blend has no tables for; BT.2020 has them, in the caps where this FFmpeg was measured to match (`BlendExtras`).
    pub fn blend_matrix(&self) -> Option<YuvMatrix> {
        if self.canvas.blend_tag_unlisted {
            return None;
        }
        match self.canvas.blend_tag {
            None | Some("smpte170m" | "bt470bg") => Some(YuvMatrix::Bt601),
            Some("bt709") => Some(YuvMatrix::Bt709),
            Some("bt2020nc") => Some(YuvMatrix::Bt2020),
            Some(_) => None,
        }
    }

    /// The size a render of this plan at `max_width` comes out — what the FFmpeg
    /// still of the same width is ([`still_size`]).
    pub fn size(&self, max_width: u32) -> (u32, u32) {
        still_size(self.canvas.width, self.canvas.height, max_width)
    }

    /// Whether FFmpeg reads this frame as a single frame of cheap inputs (`cpu::single_frame_args`): a still whose every input is a proxy or
    /// an image runs its whole graph on one thread, at any budget. The one predicate the plan's blur judgement and the compositor's
    /// graph threads (`Compositor::graph_threads_for`) share.
    pub fn is_single_frame_graph(&self) -> bool {
        self.mode == PlanMode::Still && self.layers.iter().all(|l| l.source.proxy || l.is_image)
    }

    /// The slice threads the filter graph this frame stands in for can run on, as an inclusive range: exactly the compositor's pin
    /// (`caps.graph_threads`) when it has one; one for a still of proxies and images ([`RenderPlan::is_single_frame_graph`]); else the
    /// budget's where `limit_args` caps the graph, exactly; else FFmpeg's default for the cores it counts, exactly where that count is
    /// reproduced ([`crate::ffmpeg_cpu_count`]: Linux) and otherwise `1..=16` ([`crate::cpu_graph_thread_range`]: the count is not known,
    /// and where a failed call is the reason FFmpeg's is one). A filter whose kernel depends on the slice layout (`gblur`) is judged over
    /// it; one whose result depends on the count (`chromakey`) is drawn only where it is one number ([`RenderPlan::exact_graph_threads`]).
    pub fn graph_thread_range(&self, caps: &GpuCaps) -> (u32, u32) {
        self.graph_thread_range_pinned(caps.graph_threads)
    }

    /// [`RenderPlan::graph_thread_range`] for a compositor's `pin` (`GpuCaps::graph_threads`) when there are no caps at hand.
    pub fn graph_thread_range_pinned(&self, pin: Option<u32>) -> (u32, u32) {
        self.graph_thread_range_in(pin, &crate::cpu_graph_thread_range)
    }

    /// The slice threads of this frame's graph when they are one number, else `None`: what a count-dependent filter is drawn at.
    pub fn exact_graph_threads(&self, caps: &GpuCaps) -> Option<u32> {
        self.exact_graph_threads_pinned(caps.graph_threads)
    }

    /// [`RenderPlan::exact_graph_threads`] for a compositor's `pin` (`GpuCaps::graph_threads`): the count the compositor draws a keyed
    /// layer at, which the plan's judgement of it ([`RenderPlan::reasons`]) asks of the same range through `cpu::one_number`, the one place that decides it.
    pub fn exact_graph_threads_pinned(&self, pin: Option<u32>) -> Option<u32> {
        crate::engine::cpu::one_number(self.graph_thread_range_pinned(pin))
    }

    /// [`RenderPlan::graph_thread_range`] for a `pin` (`caps.graph_threads`) and a `machine` that says the range of the host's graphs, asked
    /// only when neither the pin nor a single frame of cheap inputs decides it (the machine is read from the system: not for nothing).
    pub(crate) fn graph_thread_range_in(&self, pin: Option<u32>, machine: &dyn Fn() -> (usize, usize)) -> (u32, u32) {
        if let Some(pinned) = pin {
            return (pinned.max(1), pinned.max(1));
        }
        if self.is_single_frame_graph() {
            return (1, 1);
        }
        let (lo, hi) = machine();
        (
            u32::try_from(lo).unwrap_or(1).max(1),
            u32::try_from(hi).unwrap_or(crate::MAX_DEFAULT_GRAPH_THREADS as u32).max(1),
        )
    }
}

impl RenderPlan {
    /// Why a blur, a sharpen, a vignette or a grade on a letterboxed layer (or one over a blurred fill) is not drawn on its padded frame,
    /// where the compositor lays that frame out whole (`GpuCaps::matte_effects`); `None`: the chain is one it was held to. The frame the
    /// filters see is the canvas' size, the picture on its bars (the black of `pad` through the `eq` and the pointwise effects ahead of
    /// them), and what runs on it is the filter at the size of that frame. (A letterboxed layer is the identity pose, so it is opaque and
    /// upright.) Not held: a picture in another format than 8-bit 4:2:0 (unproven: FFmpeg converts it at the fit `scale`, ahead of the pad,
    /// and no case holds that beside the padded frame on every format), a mask (cut from the same frame, with the `geq` edge rule on the padded
    /// planes) and a clip with keyed numbers.
    fn padded_refusal(&self, layer: &PlanLayer) -> Option<&'static str> {
        if layer.stream.pix_fmt.as_deref() != Some("yuv420p") {
            return Some("a blur, a sharpen or a vignette on a letterbox of a picture that is not 8-bit 4:2:0 yuv420p (unproven: FFmpeg's negotiation converts such a picture to 4:2:0 at the fit `scale`, ahead of the pad, and no case holds that beside the padded frame on every format and build)");
        }
        if layer.mask.is_some() {
            return Some("a blur, a sharpen or a vignette on a letterbox under a mask");
        }
        if layer.animated.is_some() {
            return Some("a blur, a sharpen or a vignette on a letterbox of a clip with keyed numbers");
        }
        None
    }
}

/// What `pad` does with the odd last row of a letterboxed picture `scale` did not resample (the geometry keeps that row).
enum OddPad {
    /// Nothing to settle (no such row), or the row stays: `pad` copies the picture.
    Unchanged,
    /// The row goes, and the chroma row under it: `pad` fills the bottom bar in place.
    Drops,
    /// Which one is the decoder's or the chain's to say: the frame is refused.
    Refused(String),
}

impl RenderPlan {
    /// Whether `pad` fills the bottom bar in place from the even-rounded height (the odd last row goes, and the chroma row under it)
    /// or copies the picture (the row stays) depends on who allocated the frame it was handed. At the frame's origin that is the
    /// decoder's and the frame's (measured on 9.0.2: FFV1 keeps the row, ffvhuff drops it), so the plan cannot say. Off the origin the
    /// decoder's frame has no room before it and `pad` copies it, unless a `scale` converted the picture into the buffer `pad` hands
    /// it, which is the graph's format negotiation: filters that share `pad`'s format list (`setsar`, `eq`) pass its constraint on to
    /// the next, and the first that restricts it (`format=yuva420p`, an `overlay` input) decides what `scale` converts to.
    ///
    /// * A **still**'s overlay takes only `yuva420p`: after a plain `scale,pad,setsar` a `yuv420p` picture is converted, and `pad`
    ///   fills in place (measured on both FFmpegs, FFV1 and ffvhuff, 5 to 31 rows short of the frame; the origin too, which stays
    ///   refused). A colour correction, a grade, an effect, a fade, a mask, another format or a clip with keyed numbers puts other
    ///   filters in the group, and the plan does not follow the negotiation through them: refused.
    /// * An **export**'s chain ends in `format=yuv420p`, which converts a 10-bit picture and not a `yuv420p` one; `format=yuva420p`
    ///   (a dissolve, a mask, a chroma key) converts every picture (a crossfade's incoming clip: 11 of 24 frames inexact before
    ///   it was refused). Any other format, and a chain on the alpha plane, are refused; a plain `yuv420p` picture is copied.
    fn odd_pad(&self, layer: &PlanLayer, geom: &LayerGeometry, size: (u32, u32)) -> OddPad {
        if !geom.matte
            || geom.picture.1.is_multiple_of(2)
            || geom.picture_shows.1 != geom.picture.1
            || matches!(self.blur_fill(layer, size), Ok(Some(_)))
        {
            return OddPad::Unchanged;
        }
        let (pw, ph) = geom.picture;
        let yuv420p = layer
            .stream
            .pix_fmt
            .as_deref()
            .is_some_and(|f| f.eq_ignore_ascii_case("yuv420p"));
        if geom.picture_at == (0, 0) {
            return OddPad::Refused(format!(
                "pad of a {pw}x{ph} picture with an odd height at the frame's origin, which `pad` fills in place (the odd last row goes) or copies (it stays) as the decoder's frame allows"
            ));
        }
        match self.mode {
            PlanMode::Still => {
                let plain = layer.color.is_identity()
                    && layer.grade.is_identity()
                    && layer.effects.is_empty()
                    && layer.mask.is_none()
                    && layer.fx.fades.is_empty()
                    && layer.hdr.is_none()
                    && layer.reframe.is_none()
                    && layer.animated.is_none()
                    && layer.blend.is_normal();
                if yuv420p && plain {
                    OddPad::Drops
                } else {
                    OddPad::Refused(format!(
                        "pad of a {pw}x{ph} picture with an odd height in a still whose chain is not a plain `yuv420p` picture's `scale,pad,setsar` (a colour correction, grade, effect, fade, mask or other format changes which filter converts the picture, and so whether `pad` fills in place and the odd last row goes)"
                    ))
                }
            }
            PlanMode::Motion => {
                if !yuv420p {
                    OddPad::Refused(format!(
                        "pad of a {pw}x{ph} picture with an odd height whose format ({}) is not `yuv420p`, which `scale` converts into the buffer `pad` hands it, and `pad` fills the bottom bar in place (the odd last row goes)",
                        layer.stream.pix_fmt.as_deref().unwrap_or("?")
                    ))
                } else if layer.mask.is_some()
                    || layer.effects.iter().any(|e| e.produces_alpha())
                    || layer.fx.fades.iter().any(|s| s.tint == FadeTint::Alpha)
                {
                    OddPad::Refused(format!(
                        "pad of a {pw}x{ph} picture with an odd height on the alpha plane (a dissolve, a mask or a chroma key), where `scale` converts the picture into the `yuva420p` that follows the `pad` inside the buffer `pad` hands it, and `pad` fills the bottom bar in place (the odd last row goes)"
                    ))
                } else if !layer.blend.is_normal() {
                    // (A blend mode is refused whole today; its chain converts the picture to RGB and back around `blend`, with
                    // `pad` in the group, and FFmpeg drops the odd row there — `checker_b_r4` `ROW` lines — so a later lift of the
                    // blend must not draw the row kept.)
                    OddPad::Refused(format!(
                        "pad of a {pw}x{ph} picture with an odd height under a blend mode, whose chain converts the picture through RGB and `pad` fills the bottom bar in place (the odd last row goes)"
                    ))
                } else {
                    OddPad::Unchanged
                }
            }
        }
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
            color_range: None,
            lead: 0.0,
            time_base: None,
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
            music_score: None,
            sound_effect: None,
            tags: Vec::new(),
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
            (Some("reserved"), false),
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
            (Some("reserved"), YuvMatrix::Bt601),
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
        // FFmpeg 9 converts the top into the bottom's matrix with arithmetic a compositor without
        // `GpuCaps::matrix_convert` does not reproduce — in either order, and for 601 vs untagged
        // there is nothing to convert.
        for layers in [
            [("yuv420p", Some("bt709")), ("yuv420p", None)],
            [("yuv420p", None), ("yuv420p", Some("bt709"))],
            [("yuv420p", Some("bt709")), ("yuv420p", Some("bt2020nc"))],
        ] {
            let p = stack(B, &layers);
            assert!(!p.gpu_supported(), "{layers:?}");
            assert!(p
                .unsupported_reasons()
                .iter()
                .any(|r| r.contains("other than the composite's")));
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

    /// Where the compositor's conversion is measured (`GpuCaps::matrix_convert`), a layer whose matrix is not the bottom's
    /// is drawn if it is a plain opaque `yuv420p` picture of even size, and refused for what the conversion does not
    /// cover, each with its own detail.
    #[test]
    fn where_the_conversion_is_measured_a_plain_layer_of_another_matrix_is_converted() {
        use CompositeColorPolicy::BottomLayerTag as B;
        let caps = GpuCaps {
            matrix_convert: true,
            ..GpuCaps::A0
        };
        let why = |p: &RenderPlan| p.reasons(&caps, p.size(u32::MAX));
        // Either order, any pair of matrices: drawn, the composite following the bottom layer's tag.
        for (layers, bottom) in [
            ([("yuv420p", Some("bt709")), ("yuv420p", None)], YuvMatrix::Bt709),
            ([("yuv420p", None), ("yuv420p", Some("bt709"))], YuvMatrix::Bt601),
            ([("yuv420p", Some("bt709")), ("yuv420p", Some("bt2020nc"))], YuvMatrix::Bt709),
            (
                [("yuv420p", Some("bt2020nc")), ("yuv420p", Some("smpte170m"))],
                YuvMatrix::Bt2020,
            ),
        ] {
            let p = stack(B, &layers);
            assert_eq!((p.canvas.matrix, why(&p)), (bottom, vec![]), "{layers:?}");
        }
        // A compositor with no conversion refuses them (the A0 caps).
        let p = stack(B, &[("yuv420p", Some("bt709")), ("yuv420p", None)]);
        assert!(matches!(
            p.reasons(&GpuCaps::A0, p.size(u32::MAX)).as_slice(),
            [Unsupported::MixedMatricesWith(_, "a compositor with no measured conversion")]
        ));
        // 4:2:2 and 4:4:4 reach an export's conversion as the 4:2:0 the decode makes of them (drawn: `motion_parity.rs`); a still's `scale`
        // converts them and the matrix in one pass.
        for fmt in ["yuv422p", "yuv444p"] {
            let p = stack(B, &[("yuv420p", Some("bt709")), (fmt, None)]);
            assert!(
                matches!(why(&p).as_slice(), [Unsupported::MixedMatricesWith(_, d)] if d.contains("in a still")),
                "{fmt}: {:?}",
                why(&p)
            );
        }
        // What the conversion does not cover keeps the frame FFmpeg's: another format, a grade, an effect, an opacity.
        for fmt in ["yuv420p10le", "gray", "yuvj420p"] {
            let p = stack(B, &[("yuv420p", Some("bt709")), (fmt, None)]);
            assert!(
                why(&p).iter().any(
                    |u| matches!(u, Unsupported::MixedMatricesWith(_, d) if d.contains("yuv video") || d.contains("full range"))
                ),
                "{fmt}: {:?}",
                why(&p)
            );
        }
        let mut p = stack(B, &[("yuv420p", Some("bt709")), ("yuv420p", None)]);
        p.layers[1].transform.opacity = 0.5;
        assert!(
            matches!(why(&p).as_slice(), [Unsupported::MixedMatricesWith(_, d)] if d.contains("opacity")),
            "{:?}",
            why(&p)
        );
        let mut p = stack(B, &[("yuv420p", Some("bt709")), ("yuv420p", None)]);
        p.layers[1].effects = vec![VideoEffect::Blur { sigma: 2.0 }];
        assert!(
            why(&p)
                .iter()
                .any(|u| matches!(u, Unsupported::MixedMatricesWith(_, d) if d.contains("effect"))),
            "{:?}",
            why(&p)
        );
        // Invert and grayscale are tables the export's scale applies ahead of the conversion: drawn there; a still's chain has no
        // converter of its own after them, and its composite is not the conversion of the effect's picture.
        for effect in [VideoEffect::Invert, VideoEffect::Grayscale] {
            let mut p = stack(B, &[("yuv420p", Some("bt709")), ("yuv420p", None)]);
            p.layers[1].effects = vec![effect];
            assert!(
                why(&p)
                    .iter()
                    .any(|u| matches!(u, Unsupported::MixedMatricesWith(_, d) if d.contains("effect"))),
                "{:?}",
                why(&p)
            );
            p.mode = PlanMode::Motion;
            assert!(
                why(&p).iter().all(|u| !matches!(u, Unsupported::MixedMatricesWith(..))),
                "{:?}",
                why(&p)
            );
        }
        // A grade on any layer of a stack of mixed matrices, the unconverted bottom one included.
        {
            use crate::model::{CurvePoint, Curves};
            let mut p = stack(B, &[("yuv420p", None), ("yuv420p", Some("bt709"))]);
            p.layers[0].grade = Grade {
                curves: Some(Curves {
                    master: vec![CurvePoint { x: 0.0, y: 0.1 }, CurvePoint { x: 1.0, y: 0.9 }],
                    ..Curves::default()
                }),
                ..Grade::default()
            };
            assert!(
                why(&p)
                    .iter()
                    .any(|u| matches!(u, Unsupported::MixedMatricesWith(_, d) if d.contains("grade in a stack"))),
                "{:?}",
                why(&p)
            );
        }
        // The layer of the bottom's own matrix needs no conversion whatever else it does.
        let mut p = stack(B, &[("yuv420p", Some("bt709")), ("yuv420p", Some("bt709"))]);
        p.layers[1].effects = vec![VideoEffect::Invert];
        assert!(why(&p).iter().all(|u| !matches!(u, Unsupported::MixedMatricesWith(..))));
        // An RGB picture in a stack that is not BT.601 is refused as it was; the composite follows the bottom layer's tag.
        let p = stack(B, &[("yuv420p", Some("bt709")), ("rgb24", None)]);
        assert!(why(&p).iter().any(|u| matches!(u, Unsupported::RgbInStack(YuvMatrix::Bt709))));
    }

    /// A still's last `scale` converts the matrix in the same call as its resize: swscale's cascade, which the compositor draws for a
    /// 4:2:0 picture that shrinks (`GpuCaps::matrix_convert_scaled`, measured), and FFmpeg keeps for the rest, each with its detail.
    #[test]
    fn a_layer_whose_last_scale_resizes_it_is_the_cascade_where_it_is_measured_and_shrinks() {
        use CompositeColorPolicy::BottomLayerTag as B;
        let converting = GpuCaps {
            matrix_convert: true,
            ..GpuCaps::A0
        };
        let cascading = GpuCaps {
            matrix_convert_scaled: true,
            ..converting
        };
        let pip = |scale: f64| {
            let mut p = stack(B, &[("yuv420p", Some("bt709")), ("yuv420p", None)]);
            p.layers[1].transform.scale = scale;
            p
        };
        let resize = |p: &RenderPlan, caps: &GpuCaps| {
            p.reasons(caps, p.size(u32::MAX))
                .into_iter()
                .filter_map(|u| match u {
                    Unsupported::MixedMatricesWith(_, d) => Some(d),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        // Half size: a shrink, drawn where the cascade is measured and refused where it is not.
        let p = pip(0.5);
        assert!(resize(&p, &cascading).is_empty(), "{:?}", resize(&p, &cascading));
        let without = resize(&p, &converting);
        assert!(
            matches!(without.as_slice(), [d] if d.contains("last scale resizes it in a still")),
            "{without:?}"
        );
        // A layer that grows (scale 2) is the cascade's second half in the scaler: not reproduced.
        let grown = resize(&pip(2.0), &cascading);
        assert!(matches!(grown.as_slice(), [d] if d.contains("does not shrink")), "{grown:?}");
        // A delivery that names one of the other scalers puts its flags in the cascading `scale`: the tables the cascade is reproduced with are
        // the default scaler's, so the layer is FFmpeg's, under each of the four names; `bicubic` named is the default's.
        for name in ["bilinear", "lanczos", "neighbor", "spline"] {
            let mut p = pip(0.5);
            p.canvas.scaler = Some(name.to_string());
            let why = resize(&p, &cascading);
            assert!(
                matches!(why.as_slice(), [d] if d.contains("names a scaler")),
                "{name}: {why:?}"
            );
        }
        for name in [None, Some("bicubic")] {
            let mut p = pip(0.5);
            p.canvas.scaler = name.map(str::to_string);
            assert!(resize(&p, &cascading).is_empty(), "{name:?}: {:?}", resize(&p, &cascading));
        }
        // A scale that does not resize it is the unscaled conversion, with or without the cascade.
        let p = pip(1.0);
        assert!(resize(&p, &converting).is_empty() && resize(&p, &cascading).is_empty());
        // 4:2:2 and 4:4:4 in a still are refused for their format, cascade or not; a blurred fill behind the picture is not drawn.
        let mut p = stack(B, &[("yuv420p", Some("bt709")), ("yuv420p", None)]);
        p.layers[1].transform.scale = 0.5;
        p.layers[1].blur_fill = Some((16, 9));
        assert!(
            resize(&p, &cascading).iter().any(|d| d.contains("blurred fill")),
            "{:?}",
            resize(&p, &cascading)
        );
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
        // An enlargement (`scale` over 1) is the frame's: the fit of the 640x360 picture into a frame that
        // many times larger. The clip's own zoom in is taken from the source in the fit's scale, which
        // the plan refuses on its own account (`FoldedFit`), whatever the picture.
        let with_fmt = |fmt: Option<&str>, scale: f64, opacity: f64| {
            let mut v = video(640, 360);
            v.pix_fmt = fmt.map(Into::into);
            let a = asset("a", 20.0, vec![v]);
            let mut c = Clip::new(a.id, 0.0, 10.0, 0.0);
            c.transform.scale = scale.min(1.0);
            c.transform.opacity = opacity;
            let mut tl = timeline(vec![vec![c]]);
            if scale > 1.0 {
                tl.format = Some(Delivery::new((640.0 * scale) as u32, (360.0 * scale) as u32, Fit::Contain));
            }
            plan(&tl, std::slice::from_ref(&a), 1.0)
        };
        // Resizing a picture FFmpeg scales in another format is refused — enlarging
        // and shrinking alike, a mild shrink included, and 10-bit 4:2:0 and gray with them (the exact scaler is
        // proven for 8-bit 4:2:0 only); a picture left at its size is not, whatever the format.
        for (fmt, scale, refused) in [
            ("yuv420p", 2.0, false),
            ("yuvj420p", 2.0, true),
            ("yuv420p10le", 2.0, true),
            ("gray", 2.0, true),
            ("gray10le", 2.0, true),
            ("nv12", 2.0, true),
            ("p010le", 0.9, true),
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
            ("yuv420p10le", 0.9, true),
            ("gray", 0.5, true),
            ("gray", 1.0, false),
            ("yuv420p10le", 1.0, false),
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
        // 6 is past 63:1, 640 px to 12 (53:1) and 19 are not.
        let p = with_fmt(Some("yuv420p"), 0.01, 1.0);
        let why = p.unsupported_reasons_at(p.size(u32::MAX));
        assert!(why.iter().any(|r| r.contains("steeper than the 63:1")), "{why:?}");
        let p = with_fmt(Some("yuv420p"), 0.02, 1.0);
        assert!(p.gpu_supported_at(p.size(u32::MAX)));
        let p = with_fmt(Some("yuv420p"), 0.03, 1.0);
        assert!(p.gpu_supported_at(p.size(u32::MAX)));
        // A translucent layer of odd size, at the size it is rendered.
        let p = with_fmt(Some("yuv420p"), 0.33, 0.5);
        let why = p.unsupported_reasons_at(p.size(u32::MAX));
        assert!(why.iter().any(|r| r.contains("odd size")), "{why:?}");
        let p = with_fmt(Some("yuv420p"), 0.5, 0.5);
        assert!(p.gpu_supported_at(p.size(u32::MAX)));
    }

    /// A layer below full opacity whose picture has an odd side is the compositor's only with a model of FFmpeg's odd sizes
    /// (`GpuCaps::translucent_odd`, which `kerf-gpu`'s probe sets) and only for the layers that model was held to.
    #[test]
    fn a_translucent_layer_of_odd_size_is_drawn_with_a_model_of_it_and_refused_beyond_what_it_was_held_to() {
        let odd_for = |why: &[Unsupported], tail: &str| {
            why.iter().any(|u| match u {
                Unsupported::Geometry(_, m) => m.starts_with("a translucent layer of odd size") && m.contains(tail),
                _ => false,
            })
        };
        let any_odd = |why: &[Unsupported]| {
            why.iter()
                .any(|u| matches!(u, Unsupported::Geometry(_, m) if m.starts_with("a translucent layer of odd size")))
        };
        let model = GpuCaps {
            translucent_odd: true,
            ..GpuCaps::A0
        };
        // 640x360 at 0.33 is 211x118 (an odd width), at 0.37 236x133 (an odd height); a 4:3 picture at 0.47 is 225x169 (both).
        let layer = |(w, h): (u32, u32), scale: f64, opacity: f64, edit: &dyn Fn(&mut Clip)| {
            let a = asset("a", 20.0, vec![video(w, h)]);
            let mut c = Clip::new(a.id, 0.0, 10.0, 0.0);
            c.transform.scale = scale;
            c.transform.opacity = opacity;
            edit(&mut c);
            let tl = timeline(vec![vec![c]]);
            plan(&tl, std::slice::from_ref(&a), 1.0)
        };
        let judge = |p: &RenderPlan, caps: &GpuCaps| p.reasons(caps, p.size(u32::MAX));
        for (src, scale, size) in [
            ((640, 360), 0.33, "211x118"),
            ((640, 360), 0.37, "236x133"),
            ((480, 360), 0.47, "225x169"),
        ] {
            let p = layer(src, scale, 0.5, &|_| {});
            // Without a model, as before: FFmpeg's.
            let why = judge(&p, &GpuCaps::A0);
            assert!(
                odd_for(&why, &format!("{size} (FFmpeg's RGB round trip reads past the picture)")),
                "{size}: {why:?}"
            );
            // With one, drawn.
            let why = judge(&p, &model);
            assert!(why.is_empty(), "{size}: {why:?}");
            // An opacity of 1 has no round trip to model (nor has a keyed one, a `geq` alpha: `layer_geometry`'s test).
            assert!(judge(&layer(src, scale, 1.0, &|_| {}), &GpuCaps::A0).is_empty(), "{size}");
        }
        // An even picture never asked for the model.
        assert!(judge(&layer((640, 360), 0.5, 0.5, &|_| {}), &GpuCaps::A0).is_empty());
        // The round trip being unmeasured is still its own refusal, and the model does not lift it.
        let p = layer((640, 360), 0.33, 0.5, &|_| {});
        let unmeasured = GpuCaps {
            translucent_approximate: true,
            ..model
        };
        let why = judge(&p, &unmeasured);
        assert!(
            why.iter().any(|u| matches!(u, Unsupported::TranslucentApproximate(_))),
            "{why:?}"
        );
        // A mask and a chroma key are drawn (the alpha plane the round trip maps); a colour correction too.
        let mask = Mask {
            shape: MaskShape::Ellipse,
            x: 0.5,
            y: 0.5,
            width: 0.6,
            height: 0.6,
            feather: 0.2,
            inverted: false,
        };
        let drawn = |p: &RenderPlan, what: &str| {
            let why = judge(p, &model);
            assert!(!any_odd(&why), "{what}: {why:?}");
        };
        drawn(&layer((640, 360), 0.33, 0.5, &|c| c.mask = Some(mask)), "a mask");
        drawn(
            &layer((640, 360), 0.33, 0.5, &|c| {
                c.effects = vec![VideoEffect::ChromaKey {
                    color: "green".into(),
                    similarity: 0.2,
                    blend: 0.2,
                }];
            }),
            "a chroma key",
        );
        drawn(
            &layer((640, 360), 0.33, 0.5, &|c| c.color.saturation = 1.3),
            "a colour correction",
        );
        // What the model was not held to stays FFmpeg's, each for what it has.
        let refused = |p: &RenderPlan, tail: &str| {
            let why = judge(p, &model);
            assert!(odd_for(&why, tail), "{tail}: {why:?}");
        };
        // ... a side under 16 (0.03 of 640x360 is 19x10: the chroma filter has two taps),
        refused(&layer((640, 360), 0.03, 0.5, &|_| {}), "19x10 (a side under 16");
        refused(&layer((640, 360), 0.025, 0.5, &|_| {}), "(a side under 16");
        // ... a turn, a blend mode, a grade and an effect other than a chroma key.
        refused(&layer((640, 360), 0.33, 0.5, &|c| c.transform.rotation = 33.0), "(turned)");
        refused(
            &layer((640, 360), 0.33, 0.5, &|c| c.blend_mode = crate::model::BlendMode::Multiply),
            "(under a blend mode)",
        );
        refused(
            &layer((640, 360), 0.33, 0.5, &|c| {
                c.grade.wheels = Some(crate::model::Wheels {
                    shadows: crate::model::Wheel {
                        x: 0.3,
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }),
            "(graded)",
        );
        for effect in [
            VideoEffect::Invert,
            VideoEffect::Grayscale,
            VideoEffect::Blur { sigma: 2.0 },
            VideoEffect::Vignette,
            VideoEffect::Sharpen { amount: 0.5 },
        ] {
            refused(
                &layer((640, 360), 0.33, 0.5, &|c| c.effects = vec![effect.clone()]),
                "(with an effect other than a chroma key)",
            );
        }
        // The size is the layer's own at the size it is rendered: even at one preview width, odd at another.
        let p = layer((640, 360), 0.5, 0.5, &|_| {});
        let at = |width: u32| {
            let size = p.size(width);
            let why = p.clone().for_render(size).reasons(&GpuCaps::A0, size);
            (size, why)
        };
        let (size, why) = at(640);
        assert!(!any_odd(&why), "{size:?}: {why:?}");
        let (size, why) = at(426);
        assert!(odd_for(&why, "211x119"), "{size:?}: {why:?}");
    }

    #[test]
    fn the_shrink_limit_is_judged_in_64_bits() {
        assert!(!shrinks_past_limit(63, 1));
        assert!(shrinks_past_limit(64, 1));
        assert!(!shrinks_past_limit(u32::MAX, u32::MAX));
        assert!(!shrinks_past_limit(u32::MAX, 1 << 27));
        assert!(shrinks_past_limit(u32::MAX, 1 << 26));
        assert!(shrinks_past_limit(1, 0));
    }

    #[test]
    fn pad_of_an_odd_picture_is_settled_by_the_chain_that_converts_it() {
        let opts = ExportOptions {
            fps: Some(30.0),
            ..ExportOptions::default()
        };
        let plan_of = |mode: PlanMode, fmt: &str, w: u32, h: u32, frame: (u32, u32), edit: &dyn Fn(&mut Clip)| {
            let mut stream = video(w, h);
            stream.pix_fmt = Some(fmt.into());
            let a = asset("a", 20.0, vec![stream]);
            let mut clip = Clip::new(a.id, 0.0, 10.0, 0.0);
            edit(&mut clip);
            let mut tl = timeline(vec![vec![clip]]);
            tl.format = Some(Delivery::new(frame.0, frame.1, Fit::Contain));
            let request = match mode {
                PlanMode::Still => PlanRequest::still(CompositeColorPolicy::FixedBt601),
                PlanMode::Motion => PlanRequest::motion(CompositeColorPolicy::FixedBt601),
            };
            let planner = Planner::new(&tl, std::slice::from_ref(&a), &opts, request).unwrap();
            match mode {
                PlanMode::Still => planner.at(0.5).unwrap(),
                PlanMode::Motion => planner.at_frame(15).unwrap(),
            }
        };
        let caps = GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            effects: EffectKinds::ALL,
            ..GpuCaps::A0
        };
        let why_of =
            |p: &RenderPlan| -> Vec<String> { p.reasons(&caps, p.size(u32::MAX)).iter().map(ToString::to_string).collect() };
        let shows = |p: &RenderPlan| p.layer_geometry(&p.layers[0], p.size(u32::MAX)).unwrap().picture_shows;
        let reasons = |mode, fmt: &str, w, h, frame| why_of(&plan_of(mode, fmt, w, h, frame, &|_| {}));
        let at_origin =
            |why: &[String]| matches!(why, [r] if r.contains("pad of a") && r.contains("odd height at the frame's origin"));
        let in_a_still = |why: &[String]| matches!(why, [r] if r.contains("pad of a") && r.contains("odd height in a still"));
        let not_yuv420p = |why: &[String]| matches!(why, [r] if r.contains("pad of a") && r.contains("is not `yuv420p`"));
        for mode in [PlanMode::Still, PlanMode::Motion] {
            // One row short of the frame, at the origin, scale forwarding the decoder's frame: `pad` fills in place or copies as the
            // decoder's buffer allows, and the odd last row goes or stays with it.
            for (w, h, frame) in [(64, 35, (64, 36)), (96, 53, (96, 54)), (96, 51, (96, 52))] {
                let why = reasons(mode, "yuv420p", w, h, frame);
                assert!(at_origin(&why), "{mode:?} {w}x{h}: {why:?}");
            }
            // Resampled it is always padded in place (a 62x35 picture is fitted to 64x36), and an even picture has no odd row.
            for (w, h, frame) in [
                (62, 35, (64, 36)),
                (64, 34, (64, 36)),
                (640, 360, (360, 640)),
                (64, 36, (64, 36)),
            ] {
                let p = plan_of(mode, "yuv420p", w, h, frame, &|_| {});
                assert!(why_of(&p).is_empty(), "{mode:?} {w}x{h}: {:?}", why_of(&p));
            }
        }
        // Off the origin an export's `pad` copies the decoder's `yuv420p` frame (the `format=yuv420p` after it is no conversion): the
        // row stays. A still's `scale` converts the picture into the `yuva420p` its overlay takes, inside the buffer `pad` hands it:
        // the odd row goes, and the geometry shows one row less. Measured: 64x5 to 64x33 in 64x36 and 64x31 / 64x33 in 64x40, both
        // builds, FFV1 and ffvhuff.
        for (w, h, frame) in [(64, 5, (64, 36)), (64, 13, (64, 36)), (64, 31, (64, 36)), (64, 33, (64, 40))] {
            let p = plan_of(PlanMode::Still, "yuv420p", w, h, frame, &|_| {});
            assert!(why_of(&p).is_empty(), "still {w}x{h}: {:?}", why_of(&p));
            assert_eq!(shows(&p), (w, h - 1), "still {w}x{h}");
            let p = plan_of(PlanMode::Motion, "yuv420p", w, h, frame, &|_| {});
            assert!(why_of(&p).is_empty(), "motion {w}x{h}: {:?}", why_of(&p));
            assert_eq!(shows(&p), (w, h), "motion {w}x{h}");
        }
        // The row a still keeps is the origin's, which is refused: its geometry is what it was.
        let p = plan_of(PlanMode::Still, "yuv420p", 64, 35, (64, 36), &|_| {});
        assert_eq!(shows(&p), (64, 35));
        // A still's other filters change which one converts the picture, and the plan does not follow the negotiation through them.
        let eq = Color {
            contrast: 1.2,
            ..Color::default()
        };
        type Edit = Box<dyn Fn(&mut Clip)>;
        let edits: Vec<(&str, Edit)> = vec![
            ("eq", Box::new(move |c| c.color = eq)),
            ("fade", Box::new(|c| c.fade_in = 1.0)),
            ("effect", Box::new(|c| c.effects = vec![VideoEffect::Invert])),
        ];
        for (label, edit) in &edits {
            let why = why_of(&plan_of(PlanMode::Still, "yuv420p", 64, 31, (64, 36), edit.as_ref()));
            assert!(in_a_still(&why), "still {label}: {why:?}");
            let why = why_of(&plan_of(PlanMode::Motion, "yuv420p", 64, 31, (64, 36), edit.as_ref()));
            assert!(why.is_empty(), "motion {label}: {why:?}");
        }
        for fmt in ["yuv420p10le", "yuvj420p", "nv12"] {
            let why = reasons(PlanMode::Still, fmt, 64, 31, (64, 36));
            assert!(in_a_still(&why), "still {fmt}: {why:?}");
            // An export converts a 10-bit picture into the buffer `pad` hands it (`format=yuv420p` follows), so its odd row goes too.
            let why = reasons(PlanMode::Motion, fmt, 64, 31, (64, 36));
            assert!(not_yuv420p(&why), "{fmt}: {why:?}");
            let why = reasons(PlanMode::Motion, fmt, 64, 30, (64, 36));
            assert!(why.is_empty(), "{fmt} even: {why:?}");
        }
        // A native format that is not 4:2:0, at the frame's width: `scale` converts it into the buffer `pad` hands it whatever the
        // chain, so the row goes (the native grid's geometry, which differs from the 4:2:0 grid's in that row alone), and the plan
        // does not call that a crop between two chroma samples.
        for fmt in ["yuv444p", "yuv422p", "rgb24", "gbrp"] {
            for mode in [PlanMode::Still, PlanMode::Motion] {
                for (h, frame) in [(31, (64, 36)), (35, (64, 36))] {
                    let p = plan_of(mode, fmt, 64, h, frame, &|_| {});
                    assert!(why_of(&p).is_empty(), "{mode:?} {fmt} 64x{h}: {:?}", why_of(&p));
                    assert_eq!(shows(&p), (64, h - 1), "{mode:?} {fmt} 64x{h}");
                }
            }
        }
        // A chain on the alpha plane (a crossfade's incoming clip, measured 11 of 24 frames inexact; a mask; a chroma key) has a
        // `format=yuva420p` after the `pad`, which `scale` converts the picture to inside the buffer `pad` hands it.
        let on_alpha_plane = |why: &[String]| why.iter().any(|r| r.contains("odd height on the alpha plane"));
        let a = asset("a", 20.0, vec![video(64, 31)]);
        let mut incoming = Clip::new(a.id, 0.0, 4.0, 2.0);
        incoming.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let mut tl = timeline(vec![vec![Clip::new(a.id, 0.0, 2.0, 0.0), incoming]]);
        tl.format = Some(Delivery::new(64, 36, Fit::Contain));
        let planner = Planner::new(
            &tl,
            std::slice::from_ref(&a),
            &opts,
            PlanRequest::motion(CompositeColorPolicy::FixedBt601),
        )
        .unwrap();
        for (frame, alpha_layers) in [(30, 0), (75, 1), (120, 1)] {
            let p = planner.at_frame(frame).unwrap();
            let why = why_of(&p);
            let n = why.iter().filter(|r| r.contains("odd height on the alpha plane")).count();
            assert_eq!(n, alpha_layers, "frame {frame}: {why:?}");
        }
        let motion = |mask: Option<Mask>, effects: Vec<VideoEffect>| {
            why_of(&plan_of(PlanMode::Motion, "yuv420p", 64, 31, (64, 36), &|c| {
                c.mask = mask;
                c.effects = effects.clone();
            }))
        };
        let rect = Mask {
            shape: MaskShape::Rect,
            x: 0.5,
            y: 0.5,
            width: 0.6,
            height: 0.6,
            feather: 0.0,
            inverted: false,
        };
        let key = VideoEffect::ChromaKey {
            color: "green".into(),
            similarity: 0.3,
            blend: 0.1,
        };
        assert!(on_alpha_plane(&motion(Some(rect), vec![])));
        assert!(on_alpha_plane(&motion(None, vec![key])));
        assert!(!on_alpha_plane(&motion(None, vec![VideoEffect::Invert])));
        assert!(motion(None, vec![]).is_empty());
        // A blend mode's chain (RGB round trip around `blend`) drops the row too: refused for the pad as well as the blend.
        let blended = why_of(&plan_of(PlanMode::Motion, "yuv420p", 64, 31, (64, 36), &|c| {
            c.blend_mode = crate::model::BlendMode::Multiply;
        }));
        assert!(blended.iter().any(|r| r.contains("under a blend mode")), "{blended:?}");
    }

    #[test]
    fn a_letterboxed_full_range_flagged_picture_is_refused_in_an_export_that_does_not_negotiate() {
        let reasons = |mode: PlanMode, policy: CompositeColorPolicy, range: Option<&str>, w: u32, h: u32| {
            let mut stream = video(w, h);
            stream.color_range = range.map(String::from);
            let a = asset("a", 20.0, vec![stream]);
            let mut tl = timeline(vec![vec![Clip::new(a.id, 0.0, 10.0, 0.0)]]);
            tl.format = Some(Delivery::new(64, 36, Fit::Contain));
            let opts = ExportOptions {
                fps: Some(30.0),
                ..ExportOptions::default()
            };
            let request = match mode {
                PlanMode::Still => PlanRequest::still(policy),
                PlanMode::Motion => PlanRequest::motion(policy),
            };
            let planner = Planner::new(&tl, std::slice::from_ref(&a), &opts, request).unwrap();
            let p = match mode {
                PlanMode::Still => planner.at(0.5).unwrap(),
                PlanMode::Motion => planner.at_frame(15).unwrap(),
            };
            let caps = GpuCaps {
                motion: true,
                ..GpuCaps::A0
            };
            p.reasons(&caps, p.size(u32::MAX))
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        };
        let flagged = |why: &[String]| why.iter().any(|r| r.contains("flagged full range"));
        use CompositeColorPolicy::{BottomLayerTag, FixedBt601, Unknown};
        for policy in [FixedBt601, Unknown] {
            // Even and odd, off the origin: the bars are the same.
            for (w, h) in [(64, 30), (64, 31), (64, 13)] {
                assert!(
                    flagged(&reasons(PlanMode::Motion, policy, Some("pc"), w, h)),
                    "{policy:?} {w}x{h}"
                );
            }
            // A limited-range or unflagged picture, a picture that fills the frame, and a still are not.
            assert!(!flagged(&reasons(PlanMode::Motion, policy, Some("tv"), 64, 30)));
            assert!(!flagged(&reasons(PlanMode::Motion, policy, None, 64, 30)));
            assert!(!flagged(&reasons(PlanMode::Motion, policy, Some("pc"), 64, 36)));
            assert!(!flagged(&reasons(PlanMode::Still, policy, Some("pc"), 64, 30)));
        }
        // An FFmpeg that negotiates the range leaves the bars as they are drawn.
        assert!(!flagged(&reasons(PlanMode::Motion, BottomLayerTag, Some("pc"), 64, 30)));
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

    /// `reasons` lists every cause of a refusal, not the first it finds: a ranking of refusals by the frames that lifting
    /// one alone would draw is only true if none hides behind another. The decision is the same either way (a frame with
    /// any reason is refused, and the first reason of each of these was already one).
    #[test]
    fn every_cause_of_a_refusal_is_listed_and_a_refused_geometry_hides_none() {
        use crate::model::{Wheel, Wheels};
        let all = GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            dip_white: true,
            keyed_zoom: true,
            effects: EffectKinds::ALL,
            grade: GradeKinds::ALL,
            ..GpuCaps::A0
        };
        // An RGB picture cropped between two 4:2:0 chroma samples (its geometry is refused) and fitted into a smaller
        // frame (a resize of a picture FFmpeg scales in its own format, refused for itself).
        let rgb = |edit: &dyn Fn(&mut Clip)| {
            let mut v = video(640, 360);
            v.pix_fmt = Some("rgb24".into());
            let plan = fading_plan_of(
                v,
                Some(Delivery::new(320, 180, Fit::Contain)),
                CompositeColorPolicy::FixedBt601,
                |c| {
                    c.transform.crop_left = 11.0 / 640.0;
                    c.transform.crop_top = 7.0 / 360.0;
                    edit(c);
                },
                0.5,
            );
            plan.reasons(&all, plan.size(u32::MAX))
        };
        let geometry = |u: &Unsupported| matches!(u, Unsupported::Geometry(0, m) if m.contains("4:2:0 chroma samples"));
        let format_resize = |u: &Unsupported| matches!(u, Unsupported::FormatResize(0, _, _, f) if f == "rgb24");
        let why = rgb(&|_| {});
        assert!(why.iter().any(geometry) && why.iter().any(format_resize), "{why:?}");
        // ... and a grade on it, and a dip to white: the checks that need the geometry run on the picture's own grid.
        let why = rgb(&|c| {
            c.grade.wheels = Some(Wheels {
                midtones: Wheel {
                    x: 0.2,
                    y: 0.1,
                    lightness: 0.05,
                },
                ..Wheels::default()
            });
            c.transition_in = Some(Transition {
                kind: TransitionKind::DipToWhite,
                duration: 2.0,
            });
        });
        assert!(why.iter().any(geometry) && why.iter().any(format_resize), "{why:?}");
        assert!(
            why.iter()
                .any(|u| matches!(u, Unsupported::GradeWith(_, m) if m.contains("resizes"))),
            "{why:?}"
        );
        assert!(
            why.iter().any(|u| matches!(u, Unsupported::DipToWhite(_, DipWhy::Resized))),
            "{why:?}"
        );
        // A picture whose crop is whole on every grid has no `Geometry` and the same other reasons.
        let mut v = video(640, 360);
        v.pix_fmt = Some("rgb24".into());
        let plain = fading_plan_of(
            v,
            Some(Delivery::new(320, 180, Fit::Contain)),
            CompositeColorPolicy::FixedBt601,
            |_| {},
            0.5,
        );
        let plain = plain.reasons(&all, plain.size(u32::MAX));
        assert!(matches!(plain.as_slice(), [Unsupported::FormatResize(..)]), "{plain:?}");
        // A grade on a letterboxed layer the scaler resizes is a Matte and a GradeWith, not the first alone.
        let grade = Grade {
            wheels: Some(Wheels {
                midtones: Wheel {
                    x: 0.2,
                    y: 0.1,
                    lightness: 0.05,
                },
                ..Wheels::default()
            }),
            ..Grade::default()
        };
        let p = fading_plan_of(
            video(360, 640),
            Some(Delivery::new(640, 360, Fit::Contain)),
            CompositeColorPolicy::FixedBt601,
            |c| c.grade = grade.clone(),
            0.5,
        );
        let why = p.reasons(&all, p.size(u32::MAX));
        assert!(why.iter().any(|u| matches!(u, Unsupported::Matte(_, "a grade"))), "{why:?}");
        assert!(
            why.iter()
                .any(|u| matches!(u, Unsupported::GradeWith(_, m) if m.contains("resizes"))),
            "{why:?}"
        );
        // The decision is the one the first reason made: refused with them, refused without.
        assert!(!p.gpu_supported_at(p.size(u32::MAX)));
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
        assert_eq!(of(Some("reserved")), Some(YuvMatrix::Bt601));
        assert_eq!(of(Some("smpte240m")), None);
    }

    #[test]
    fn a_labelled_picture_is_bt709_whatever_its_file_says() {
        let mut s = PlanStream::of(&video(640, 360)).unwrap();
        s.label_bt709 = true;
        for tag in [None, Some("smpte170m"), Some("reserved"), Some("bt709")] {
            s.color_space = tag.map(Into::into);
            assert_eq!(s.matrix(), Some(YuvMatrix::Bt709), "{tag:?}");
        }
        // A picture of an asset that never recorded its pixel format has no known matrix at all.
        s.pix_fmt = None;
        assert_eq!(s.matrix(), None);
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

    /// A placed layer whose asset did not record its range, where the composite's range is negotiated: the plan reads the
    /// cache only for a host that asks the file ahead (`GpuCaps::file_range`), and then refuses a file that is full range or
    /// that has not said, so a session, the render bar and a hand-over see what the compositor would refuse at draw time.
    #[test]
    fn a_placed_layer_with_no_recorded_range_is_judged_by_the_files_answer_only_where_the_host_asks_it() {
        let key = |time: f64, scale: f64| Keyframe {
            time,
            scale,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        };
        let caps = GpuCaps {
            motion: true,
            fades: true,
            keyed_zoom: true,
            placement: true,
            ..GpuCaps::A0
        };
        let asking = GpuCaps {
            file_range: true,
            ..caps
        };
        let plan_of = |name: &str, recorded: Option<&str>, policy: CompositeColorPolicy| {
            let mut stream = video(640, 360);
            stream.color_range = recorded.map(String::from);
            let a = asset(name, 20.0, vec![stream]);
            let mut clip = Clip::new(a.id, 0.0, 10.0, 0.0);
            clip.keyframes = vec![key(0.0, 1.0), key(8.0, 1.5)];
            let plan = RenderPlan::at(
                &timeline(vec![vec![clip]]),
                std::slice::from_ref(&a),
                &ExportOptions {
                    fps: Some(30.0),
                    ..ExportOptions::default()
                },
                4.0,
                policy,
            )
            .unwrap();
            let path = std::path::PathBuf::from(&plan.layers[0].path);
            (plan, path)
        };
        let asked = |plan: &RenderPlan, caps: &GpuCaps| {
            plan.reasons(caps, plan.size(u32::MAX))
                .into_iter()
                .filter_map(|u| match u {
                    Unsupported::PlacedFormat(_, why) => Some(why),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let negotiating = CompositeColorPolicy::BottomLayerTag;
        // A file nobody asked: a pure plan does not mind, a host that asks does not draw it.
        let (cold, _) = plan_of("range-cold", None, negotiating);
        assert!(asked(&cold, &caps).is_empty());
        let why = asked(&cold, &asking);
        assert!(matches!(why.as_slice(), [w] if w.contains("has not said")), "{why:?}");
        // A file that said limited is drawn; one that said full range is not.
        let (limited, path) = plan_of("range-limited", None, negotiating);
        crate::engine::seed_source_range(&path, false);
        assert!(asked(&limited, &asking).is_empty());
        let (full, path) = plan_of("range-full", None, negotiating);
        crate::engine::seed_source_range(&path, true);
        let why = asked(&full, &asking);
        assert!(
            matches!(why.as_slice(), [w] if w.starts_with("full-range") && w.contains("did not record")),
            "{why:?}"
        );
        assert!(asked(&full, &caps).is_empty(), "and only for a host that asks");
        // Where the composite's range is fixed, or the asset recorded it, the file is not asked about.
        let (fixed, _) = plan_of("range-fixed", None, CompositeColorPolicy::FixedBt601);
        assert!(asked(&fixed, &asking).is_empty());
        let (recorded, _) = plan_of("range-recorded", Some("tv"), negotiating);
        assert!(asked(&recorded, &asking).is_empty());
    }

    /// A blended clip is not placed: the placement ends the clip's chain in `pad`, `perspective` and `crop`, and no case holds the blend's two
    /// placements of that picture to FFmpeg's, so a blend over a moving zoom or position is FFmpeg's whatever the caps.
    #[test]
    fn a_blended_clip_whose_zoom_or_position_moves_is_refused_and_one_that_holds_still_is_not() {
        let key = |time: f64, scale: f64, pos_x: f64| Keyframe {
            time,
            scale,
            pos_x,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        };
        let caps = GpuCaps {
            motion: true,
            keyed_zoom: true,
            placement: true,
            blend: BlendKinds::ALL,
            blend_extras: BlendExtras::ALL,
            ..GpuCaps::A0
        };
        let blend_reasons = |edit: &dyn Fn(&mut Clip)| {
            let plan = fading_plan(
                |c| {
                    c.blend_mode = BlendMode::Multiply;
                    edit(c);
                },
                4.0,
            );
            plan.reasons(&caps, plan.size(u32::MAX))
                .into_iter()
                .filter(|u| u.kind() == "blend_mode")
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
        };
        assert!(blend_reasons(&|_| {}).is_empty());
        for edit in [
            &(|c: &mut Clip| c.keyframes = vec![key(0.0, 1.0, 0.0), key(8.0, 1.5, 0.1)]) as &dyn Fn(&mut Clip),
            &|c: &mut Clip| c.keyframes = vec![key(0.0, 1.0, -0.1), key(8.0, 1.0, 0.1)],
        ] {
            let why = blend_reasons(edit);
            assert!(why.iter().any(|w| w.contains("a moving zoom or position")), "{why:?}");
        }
    }

    /// The plan of a clip whose zoom (`zooming`) or position moves, judged by a compositor that draws the placement.
    #[test]
    fn a_placement_is_refused_for_what_it_is_not_held_to_and_drawn_otherwise() {
        let key = |time: f64, scale: f64, pos_x: f64, rotation: f64, opacity: f64| Keyframe {
            time,
            scale,
            pos_x,
            pos_y: 0.0,
            rotation,
            opacity,
            easing: Default::default(),
        };
        let caps = GpuCaps {
            motion: true,
            fades: true,
            keyed_zoom: true,
            placement: true,
            ..GpuCaps::A0
        };
        let judge = |stream: StreamInfo, delivery: Option<Delivery>, edit: &dyn Fn(&mut Clip), t: f64| {
            let plan = fading_plan_of(stream, delivery, CompositeColorPolicy::FixedBt601, |c| edit(c), t);
            let size = plan.size(u32::MAX);
            (plan.reasons(&caps, size), plan)
        };
        let zooming = |c: &mut Clip| c.keyframes = vec![key(0.0, 1.0, 0.0, 0.0, 1.0), key(8.0, 1.5, 0.1, 0.0, 1.0)];
        let panning = |c: &mut Clip| c.keyframes = vec![key(0.0, 1.0, -0.1, 0.0, 1.0), key(8.0, 1.0, 0.1, 0.0, 1.0)];
        // A zoom and a position that move are drawn; the plan carries the pose the placement draws.
        for edit in [&zooming as &dyn Fn(&mut Clip), &panning] {
            let (why, plan) = judge(video(640, 360), None, edit, 4.0);
            assert!(why.is_empty(), "{why:?}");
            let pose = plan.pose(&plan.layers[0]).expect("a pose");
            assert_eq!(pose.pos.0, plan.layers[0].transform.pos_x);
            assert_eq!(pose.scale.is_some(), plan.layers[0].animated.is_some_and(|a| a.zooms));
        }
        // ... and a clip that does not move has none.
        let (_, plan) = judge(video(640, 360), None, &|_| {}, 4.0);
        assert!(plan.pose(&plan.layers[0]).is_none());
        // The geometry stops at the picture: the zoom is no `scale` stage, and a held zoom keeps its stage.
        let (_, plan) = judge(video(640, 360), None, &zooming, 4.0);
        let geom = plan.layer_geometry(&plan.layers[0], (640, 360)).unwrap();
        assert_eq!(
            (geom.stages.len(), geom.picture, geom.layer, geom.origin),
            (1, (640, 360), (640, 360), (0, 0))
        );
        // An odd-height picture (a 640x203 picture in a 640x360 frame is not resized) is refused, an odd width is not.
        let frame = Some(Delivery::new(640, 360, Fit::Contain));
        let (why, _) = judge(video(640, 203), frame, &zooming, 4.0);
        assert!(
            matches!(why.as_slice(), [Unsupported::Geometry(_, w)] if w.contains("odd-height")),
            "{why:?}"
        );
        let (why, _) = judge(video(203, 360), frame, &zooming, 4.0);
        assert!(why.is_empty(), "{why:?}");
        // Another pixel format than 4:2:0 or gray: refused, not recorded too.
        for fmt in ["yuv444p", "yuv422p", "rgb24", "gbrp"] {
            let mut stream = video(640, 360);
            stream.pix_fmt = Some(fmt.into());
            let (why, _) = judge(stream.clone(), None, &zooming, 4.0);
            assert!(
                why.iter().any(|u| matches!(u, Unsupported::PlacedFormat(_, f) if f == fmt)),
                "{fmt}: {why:?}"
            );
            // Where the compositor measured that the graph converts planar 4:4:4 and 4:2:2 as the decode does, those two are
            // drawn; RGB and the rest of the family are not (not measured).
            let plan = fading_plan_of(stream, None, CompositeColorPolicy::FixedBt601, |c| zooming(c), 4.0);
            let why = plan.reasons(
                &GpuCaps {
                    placed_chroma: true,
                    ..caps
                },
                plan.size(u32::MAX),
            );
            let refused = why.iter().any(|u| matches!(u, Unsupported::PlacedFormat(..)));
            assert_eq!(refused, !matches!(fmt, "yuv444p" | "yuv422p"), "{fmt}: {why:?}");
        }
        for fmt in ["yuv420p", "yuv420p10le", "gray"] {
            let mut stream = video(640, 360);
            stream.pix_fmt = Some(fmt.into());
            let (why, _) = judge(stream, None, &zooming, 4.0);
            assert!(
                !why.iter().any(|u| matches!(u, Unsupported::PlacedFormat(..))),
                "{fmt}: {why:?}"
            );
        }
        let mut stream = video(640, 360);
        stream.pix_fmt = None;
        let (why, _) = judge(stream, None, &zooming, 4.0);
        assert!(why.iter().any(|u| matches!(u, Unsupported::PlacedFormat(..))), "{why:?}");
        // The lifted 4:4:4 and 4:2:2 are not exempt from it: flagged full range they are refused where the range is negotiated.
        for fmt in ["yuv444p", "yuv422p"] {
            let mut stream = video(640, 360);
            stream.pix_fmt = Some(fmt.into());
            stream.color_range = Some("pc".into());
            for (policy, refused) in [
                (CompositeColorPolicy::FixedBt601, false),
                (CompositeColorPolicy::BottomLayerTag, true),
                (CompositeColorPolicy::Unknown, true),
            ] {
                let plan = fading_plan_of(stream.clone(), None, policy, |c| zooming(c), 4.0);
                let why = plan.reasons(
                    &GpuCaps {
                        placed_chroma: true,
                        ..caps
                    },
                    plan.size(u32::MAX),
                );
                let placed = why
                    .iter()
                    .any(|u| matches!(u, Unsupported::PlacedFormat(_, f) if f.starts_with("full-range")));
                assert_eq!(placed, refused, "{fmt} {policy:?}: {why:?}");
            }
        }
        // A picture that may be full range: refused where the composite's range is negotiated, drawn where it is not.
        let mut stream = video(640, 360);
        stream.pix_fmt = Some("yuvj420p".into());
        for (policy, refused) in [
            (CompositeColorPolicy::FixedBt601, false),
            (CompositeColorPolicy::BottomLayerTag, true),
            (CompositeColorPolicy::Unknown, true),
        ] {
            let plan = fading_plan_of(stream.clone(), None, policy, |c| zooming(c), 4.0);
            let why = plan.reasons(&caps, plan.size(u32::MAX));
            let placed = why
                .iter()
                .any(|u| matches!(u, Unsupported::PlacedFormat(_, f) if f.starts_with("full-range")));
            assert_eq!(placed, refused, "{policy:?}: {why:?}");
        }
        // A pose out of the table's range.
        let (why, _) = judge(
            video(640, 360),
            None,
            &|c| c.keyframes = vec![key(0.0, 1.0, 0.0, 0.0, 1.0), key(8.0, 0.00001, 0.0, 0.0, 1.0)],
            9.0,
        );
        assert!(
            matches!(why.as_slice(), [Unsupported::Geometry(_, w)] if w.contains("fixed-point range")),
            "{why:?}"
        );
        // A rotation, an opacity below 1 and a dip to white ahead of the placement: a moving position as much as a zoom.
        let (why, _) = judge(
            video(640, 360),
            None,
            &|c| c.keyframes = vec![key(0.0, 1.0, -0.1, 90.0, 1.0), key(8.0, 1.0, 0.1, 90.0, 1.0)],
            4.0,
        );
        assert!(
            matches!(why.as_slice(), [Unsupported::ZoomBehind(_, w)] if w.contains("rotation")),
            "{why:?}"
        );
        let (why, _) = judge(
            video(640, 360),
            None,
            &|c| c.keyframes = vec![key(0.0, 1.0, -0.1, 0.0, 0.5), key(8.0, 1.0, 0.1, 0.0, 0.5)],
            4.0,
        );
        assert!(
            matches!(why.as_slice(), [Unsupported::ZoomBehind(_, w)] if w.contains("opacity")),
            "{why:?}"
        );
        // Without the ability every such layer is `KeyedPlacement` as before.
        let plan = fading_plan(zooming, 4.0);
        let why = plan.reasons(
            &GpuCaps {
                placement: false,
                ..caps
            },
            plan.size(u32::MAX),
        );
        assert!(why.iter().any(|u| matches!(u, Unsupported::KeyedPlacement(_))), "{why:?}");
        // A letterboxed picture at the identity pose in a still (frame 0 of a push-in that starts at 1): the still
        // pads it to the whole frame before the placement.
        let (why, _) = judge(video(480, 360), Some(Delivery::new(640, 360, Fit::Contain)), &zooming, 0.0);
        assert!(
            matches!(why.as_slice(), [Unsupported::Geometry(_, w)] if w.contains("letterboxed")),
            "{why:?}"
        );
        let (why, _) = judge(video(480, 360), Some(Delivery::new(640, 360, Fit::Contain)), &zooming, 4.0);
        assert!(why.is_empty(), "{why:?}");
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
                c.transform.rotation = 90.0;
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

    /// `gblur`'s own kernel is not the recursion on every plane shape at every slice-thread count (`gblur::KernelHazard`): the plan judges the
    /// picture's luma and chroma planes at the threads the graph runs on (the compositor's pin here), with the vector width the probe found
    /// (or both, when it did not say), and refuses a blur on a plane the kernel mishandles, sigma 0 included (the kernel runs all the same).
    #[test]
    fn a_blur_on_a_plane_gblurs_kernel_mishandles_is_refused_at_the_threads_the_graph_runs_on() {
        // (The last assertions compare with the machine's range, which the budget moves: the tests that move it hold this lock.)
        let _serial = crate::engine::cpu::test_lock();
        let caps = |lanes: u8, threads: u32| GpuCaps {
            effects: EffectKinds::BLUR,
            blur_lanes: lanes,
            graph_threads: Some(threads),
            ..GpuCaps::A0
        };
        let why = |w: u32, h: u32, sigma: f64, caps: &GpuCaps| {
            let plan = fading_plan_of(
                video(w, h),
                None,
                CompositeColorPolicy::FixedBt601,
                |c| {
                    c.effects = vec![VideoEffect::Blur { sigma }];
                },
                0.5,
            );
            plan.reasons(caps, plan.size(u32::MAX))
        };
        let refused = |w, h, sigma, caps: &GpuCaps| {
            why(w, h, sigma, caps)
                .iter()
                .any(|u| matches!(u, Unsupported::Effects(_, r) if r.starts_with("a blur on a plane")))
        };
        // A full-size picture is nobody's hazard, at any thread count a graph runs on.
        for t in [1, 2, 8, 16, 64] {
            assert!(!refused(640, 360, 2.0, &caps(0, t)), "640x360 at {t} threads");
        }
        // Fewer rows than threads: chroma of 14 rows against 16 threads (and not against 8); luma alone would not.
        assert!(refused(640, 28, 2.0, &caps(16, 16)));
        assert!(!refused(640, 28, 2.0, &caps(16, 8)));
        // Sigma 0 is the identity: judged, because the kernel runs, but not for the empty postscale slice, which scales by 1 and clips
        // again (`gblur::plane_hazard_for`); what reads past the row and the heap's one sample across stay refused.
        assert!(
            !refused(640, 28, 0.0, &caps(16, 16)),
            "sigma 0 under more threads than rows is the identity"
        );
        assert!(refused(640, 28, 0.5, &caps(16, 16)));
        // ... and only sigma exactly 0: a tiny sigma whose derived coefficients are the same numbers (`Gaussian::is_identity`) is a
        // cancellation in `double` arithmetic the probe does not measure, and is a blur like another.
        assert!(crate::gblur::gaussian(1e-9).is_some_and(|g| g.is_identity()));
        assert!(refused(640, 28, 1e-9, &caps(16, 16)), "a tiny sigma is not lifted");
        assert!(refused(2, 40, 0.0, &caps(16, 1)), "chroma one sample wide");
        assert!(refused(12, 16, 0.0, &caps(16, 1)), "a vector of rows over a narrow plane");
        // One thread: a vector of rows over a plane no wider than a vector; the width of the kernel decides which planes.
        assert!(refused(12, 16, 2.0, &caps(16, 1)), "AVX-512: luma 12x16");
        assert!(refused(12, 16, 2.0, &caps(8, 1)), "AVX2: chroma 6x8");
        assert!(refused(12, 16, 2.0, &caps(0, 1)));
        assert!(!refused(24, 8, 2.0, &caps(16, 1)));
        assert!(!refused(24, 8, 2.0, &caps(8, 1)));
        assert!(!refused(16, 8, 2.0, &caps(8, 1)), "luma 16 wide, chroma 8x4: clear");
        assert!(refused(16, 16, 2.0, &caps(16, 1)), "AVX-512: luma 16x16");
        assert!(refused(16, 16, 2.0, &caps(8, 1)), "AVX2: chroma 8x8");
        // A blur not asked for is no hazard; without the cap the blur is refused as a blur, not as a plane.
        let plan = fading_plan_of(video(12, 16), None, CompositeColorPolicy::FixedBt601, |_| {}, 0.5);
        assert!(plan.reasons(&caps(0, 1), plan.size(u32::MAX)).is_empty());
        let without = GpuCaps {
            effects: EffectKinds::NONE,
            ..caps(0, 1)
        };
        assert!(!refused(12, 16, 2.0, &without));
        assert!(
            refused(12, 16, 0.0, &without),
            "sigma 0 is drawn without the cap, but not on a plane its kernel reads past the row of"
        );
        assert!(
            !refused(
                640,
                28,
                0.0,
                &GpuCaps {
                    graph_threads: Some(16),
                    ..without
                }
            ),
            "... and not for the postscale slice"
        );
        // Two blurs on the layer: the one that is not the identity decides.
        let two = |a: f64, b: f64, caps: &GpuCaps| {
            let plan = fading_plan_of(
                video(640, 28),
                None,
                CompositeColorPolicy::FixedBt601,
                |c| {
                    c.effects = vec![VideoEffect::Blur { sigma: a }, VideoEffect::Blur { sigma: b }];
                },
                0.5,
            );
            plan.reasons(caps, plan.size(u32::MAX))
                .iter()
                .any(|u| matches!(u, Unsupported::Effects(_, r) if r.starts_with("a blur on a plane")))
        };
        let at16 = GpuCaps {
            graph_threads: Some(16),
            ..caps(0, 16)
        };
        assert!(!two(0.0, 0.0, &at16));
        assert!(two(0.0, 2.0, &at16));
        assert!(!refused(640, 360, 0.0, &without));
        // The range the graph can run on: pinned exactly; a still of proxies and images on one thread; else the machine's range.
        let plan = fading_plan_of(video(640, 360), None, CompositeColorPolicy::FixedBt601, |_| {}, 0.5);
        assert_eq!(plan.graph_thread_range(&caps(0, 6)), (6, 6));
        let unpinned = GpuCaps {
            graph_threads: None,
            ..caps(0, 1)
        };
        let (lo, hi) = plan.graph_thread_range(&unpinned);
        let machine = crate::cpu_graph_thread_range();
        assert_eq!((lo as usize, hi as usize), machine, "an original: the machine's range");
        assert!(1 <= lo && lo <= hi);
    }

    /// A still whose every input is a proxy or an image runs its graph on one thread whatever the budget and the cores (`single_frame_args`):
    /// the plan's range is exactly one, a pin still wins over it, and nothing else is one (an original, a mix, an export frame).
    #[test]
    fn a_still_of_proxies_and_images_runs_its_graph_on_one_thread() {
        let _serial = crate::engine::cpu::test_lock();
        let unpinned = GpuCaps {
            graph_threads: None,
            effects: EffectKinds::BLUR,
            ..GpuCaps::A0
        };
        let machine = {
            let (lo, hi) = crate::cpu_graph_thread_range();
            (lo as u32, hi as u32)
        };
        let still = || fading_plan_of(video(640, 360), None, CompositeColorPolicy::FixedBt601, |_| {}, 0.5);
        let mut plan = still();
        assert_eq!(plan.mode, PlanMode::Still);
        assert_eq!(plan.graph_thread_range(&unpinned), machine, "an original is the machine's");
        assert!(!plan.is_single_frame_graph());
        plan.layers[0].source.proxy = true;
        assert!(plan.is_single_frame_graph());
        assert_eq!(plan.graph_thread_range(&unpinned), (1, 1), "a proxy");
        plan.layers[0].source.proxy = false;
        plan.layers[0].is_image = true;
        assert_eq!(plan.graph_thread_range(&unpinned), (1, 1), "an image");
        // A pin wins over the single frame.
        assert_eq!(
            plan.graph_thread_range(&GpuCaps {
                graph_threads: Some(9),
                ..unpinned
            }),
            (9, 9)
        );
        // A frame with no layer has no input that is not cheap.
        plan.layers.clear();
        assert_eq!(plan.graph_thread_range(&unpinned), (1, 1));
        // An export frame runs the budget's graph whatever it reads.
        let mut export = still();
        export.layers[0].source.proxy = true;
        export.mode = PlanMode::Motion;
        assert!(!export.is_single_frame_graph());
        assert_eq!(export.graph_thread_range(&unpinned), machine);
        // One layer of originals among proxies is not a single frame of cheap inputs.
        let mut mixed = still();
        let layer = mixed.layers[0].clone();
        mixed.layers.push(layer);
        mixed.layers[0].source.proxy = true;
        assert!(!mixed.is_single_frame_graph());
        assert_eq!(mixed.graph_thread_range(&unpinned), machine);
    }

    /// A blur of sigma 0 is `gblur`'s identity on every build (`nu` 0, both scales 1), so a compositor whose caps leave the blur out
    /// (a build whose `gblur` is not the recursion it reproduces) still draws it; any other sigma is refused as a blur, and the
    /// sigmas whose coefficients `gblur`'s own cancellation leaves outside the shader's domain are refused by name, whatever the caps.
    #[test]
    fn a_blur_of_sigma_zero_is_drawn_without_the_blur_cap_and_a_cancelling_sigma_is_refused_with_it() {
        // (The caps of the tests that draw a blur are pinned to one graph thread, as here: they are not about the slice layout, which a
        // host with more cores than a small picture has chroma rows would otherwise put in play.)
        let without = GpuCaps {
            effects: EffectKinds::SHARPEN,
            graph_threads: Some(1),
            ..GpuCaps::A0
        };
        let with = GpuCaps {
            effects: EffectKinds::BLUR,
            graph_threads: Some(1),
            ..GpuCaps::A0
        };
        let reasons = |caps: &GpuCaps, sigma: f64| {
            let plan = fading_plan(|c| c.effects = vec![VideoEffect::Blur { sigma }], 0.5);
            plan.reasons(caps, plan.size(u32::MAX))
        };
        assert!(reasons(&without, 0.0).is_empty(), "{:?}", reasons(&without, 0.0));
        assert!(reasons(&with, 0.0).is_empty());
        let why = reasons(&without, 3.0);
        assert!(
            matches!(why.as_slice(), [Unsupported::Effects(_, w)] if *w == "a blur"),
            "{why:?}"
        );
        assert!(reasons(&with, 3.0).is_empty());
        // Sigma 0 is only the identity at 0: a tiny one is a blur like another.
        let why = reasons(&without, 1e-9);
        assert!(
            matches!(why.as_slice(), [Unsupported::Effects(_, w)] if *w == "a blur"),
            "{why:?}"
        );
        // The cancellation band (about 1.05e-8 to 1.29e-8): `nu` leaves [0, 1) in `gblur`'s own arithmetic.
        for sigma in [1.1e-8, 1.2e-8, 1.28e-8] {
            let g = crate::gblur::gblur_coefficients(sigma).expect("in range");
            assert!(!g.in_shader_domain(), "{sigma}: {g:?}");
            let why = reasons(&with, sigma);
            assert!(
                why.iter()
                    .any(|r| matches!(r, Unsupported::Effects(_, w) if w.contains("coefficients"))),
                "{sigma}: {why:?}"
            );
            assert!(!reasons(&without, sigma).is_empty(), "{sigma}");
        }
        // Their neighbours are drawn.
        for sigma in [1.0e-8 * 0.9, 1.4e-8, 2.0e-8, 1e-6] {
            assert!(reasons(&with, sigma).is_empty(), "{sigma}: {:?}", reasons(&with, sigma));
        }
    }

    #[test]
    fn what_the_compositor_cannot_do_with_a_mask_an_effect_or_a_moving_zoom_is_refused_with_its_own_reason() {
        use crate::model::Keyframe;
        let caps = GpuCaps {
            mask: true,
            keyed_zoom: true,
            // (What is ahead of a moving zoom, judged as if its placement were drawn.)
            placement: true,
            fades: true,
            effects: EffectKinds::BLUR
                .union(EffectKinds::SHARPEN)
                .union(EffectKinds::GRAYSCALE)
                .union(EffectKinds::INVERT),
            graph_threads: Some(1),
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
        // (A sharpen after the blur is of half strength: past it the blur's level is amplified, below.)
        assert!(reasons(&|c| c.effects = vec![
            VideoEffect::Blur { sigma: 3.0 },
            VideoEffect::Sharpen { amount: 0.5 },
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
        // A vignette and a chroma key, whose kinds these caps leave out: refused, the effect named.
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
        // A mask on a rotated layer is refused.
        let why = reasons(&|c| {
            c.mask = Some(mask);
            c.transform.rotation = 90.0;
        });
        assert!(
            matches!(why.as_slice(), [Unsupported::MaskWith(_, w)] if w.contains("rotated")),
            "{why:?}"
        );
        // ... one below full opacity is drawn (`colorchannelmixer` maps each level of the mask's alpha), in a blend mode it is not (the
        // blend chain carries the opaque plane of a constant opacity alone), and a chroma key's alpha is not a mask's.
        let why = reasons(&|c| {
            c.mask = Some(mask);
            c.transform.opacity = 0.5;
        });
        assert!(why.is_empty(), "{why:?}");
        let why = reasons(&|c| {
            c.mask = Some(mask);
            c.transform.opacity = 0.5;
            c.blend_mode = BlendMode::Multiply;
        });
        assert!(
            why.iter()
                .any(|r| matches!(r, Unsupported::MaskWith(_, w) if w.contains("blended layer below full opacity"))),
            "{why:?}"
        );
        // The caps that refuse a layer below full opacity (the round trip not measured) refuse it masked as well.
        let unmeasured = GpuCaps {
            translucent_approximate: true,
            translucent_odd: false,
            ..caps
        };
        let translucent = fading_plan(
            |c| {
                c.mask = Some(mask);
                c.transform.opacity = 0.5;
            },
            0.5,
        );
        let why = translucent.reasons(&unmeasured, translucent.size(u32::MAX));
        assert!(matches!(why.as_slice(), [Unsupported::TranslucentApproximate(_)]), "{why:?}");
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
        // ... and one that has fallen below 1 is a constant opacity of that value in a still (the sampled pose): drawn.
        let why = faded_by_keys(3.0);
        assert!(why.is_empty(), "{why:?}");
        // Without the ability: the mask is refused as a mask.
        let masked = fading_plan(|c| c.mask = Some(mask), 0.5);
        let why = masked.reasons(&GpuCaps { mask: false, ..caps }, masked.size(u32::MAX));
        assert!(matches!(why.as_slice(), [Unsupported::Mask(_)]), "{why:?}");
        // A moving zoom with a rotation or an opacity below 1 in front of it: refused whatever the caps.
        let why = reasons(&|c| {
            zoom(c);
            c.keyframes.iter_mut().for_each(|k| k.rotation = 90.0);
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
    fn a_vignette_is_drawn_on_a_yuv420p_picture_and_refused_on_a_letterbox_or_any_other_format() {
        let caps = GpuCaps {
            effects: EffectKinds::ALL,
            ..GpuCaps::A0
        };
        let vignette = |c: &mut Clip| c.effects = vec![VideoEffect::Grayscale, VideoEffect::Vignette];
        let of = |pix_fmt: Option<&str>| {
            let mut stream = video(640, 360);
            stream.pix_fmt = pix_fmt.map(str::to_string);
            let plan = fading_plan_of(stream, None, CompositeColorPolicy::FixedBt601, vignette, 0.5);
            plan.reasons(&caps, plan.size(u32::MAX))
        };
        assert_eq!(of(Some("yuv420p")), vec![]);
        for other in [
            Some("yuvj420p"),
            Some("yuv422p"),
            Some("yuv420p10le"),
            Some("gray"),
            Some("rgb24"),
            None,
        ] {
            let why = of(other);
            assert!(
                why.iter()
                    .any(|u| matches!(u, Unsupported::Effects(_, w) if w.contains("vignette"))),
                "{other:?}: {why:?}"
            );
        }
        // On a letterbox `vignette` sees the padded frame, bars included.
        let a = asset("a", 20.0, vec![video(640, 360)]);
        let mut clip = Clip::new(a.id, 0.0, 10.0, 0.0);
        vignette(&mut clip);
        let mut tl = timeline(vec![vec![clip]]);
        tl.format = Some(crate::model::Delivery::new(360, 640, Fit::Contain));
        let planned = plan(&tl, std::slice::from_ref(&a), 0.5);
        let why = planned.reasons(&caps, planned.size(u32::MAX));
        assert!(
            matches!(why.as_slice(), [Unsupported::Matte(_, w)] if w.contains("vignette")),
            "{why:?}"
        );
        // Without the kind, it is refused as before.
        let without = GpuCaps {
            effects: EffectKinds::BLUR
                .union(EffectKinds::SHARPEN)
                .union(EffectKinds::GRAYSCALE)
                .union(EffectKinds::INVERT),
            ..GpuCaps::A0
        };
        let plan = fading_plan(vignette, 0.5);
        let why = plan.reasons(&without, plan.size(u32::MAX));
        assert!(
            why.iter()
                .any(|u| matches!(u, Unsupported::Effects(_, w) if w.contains("vignette"))),
            "{why:?}"
        );
    }

    /// A blur, a sharpen and a vignette see a letterbox's padded frame, bars included. A compositor that lays that frame out whole
    /// (`GpuCaps::matte_effects`) draws them for an opaque 8-bit 4:2:0 picture with no mask and no keyed number, and the plan names
    /// the rest (`Effects`); without the cap, and for a chroma key (an alpha plane cut from that frame), it is a `Matte` as before.
    #[test]
    fn effects_on_a_letterbox_are_drawn_only_where_the_padded_frame_is_laid_out_and_the_chain_is_held() {
        use crate::model::{Mask, MaskShape};
        let drawn = GpuCaps {
            effects: EffectKinds::ALL,
            matte_effects: true,
            exact_scaler: true,
            mask: true,
            ..GpuCaps::A0
        };
        let without = GpuCaps {
            matte_effects: false,
            ..drawn
        };
        let tall = Some(Delivery::new(360, 640, Fit::Contain));
        let why = |caps: &GpuCaps, stream: StreamInfo, delivery: Option<Delivery>, edit: &dyn Fn(&mut Clip)| {
            let planned = fading_plan_of(stream, delivery, CompositeColorPolicy::FixedBt601, |c| edit(c), 0.5);
            planned.reasons(caps, planned.size(u32::MAX))
        };
        for (name, effect) in [
            ("a blur", VideoEffect::Blur { sigma: 3.0 }),
            ("a sharpen", VideoEffect::Sharpen { amount: 1.0 }),
            ("a vignette", VideoEffect::Vignette),
        ] {
            let on = |c: &mut Clip| c.effects = vec![VideoEffect::Invert, effect.clone()];
            assert_eq!(why(&drawn, video(640, 360), tall, &on), vec![], "{name}");
            // Without the pass, or on a picture that fills the frame, nothing changed.
            let w = why(&without, video(640, 360), tall, &on);
            assert!(
                matches!(w.as_slice(), [Unsupported::Matte(_, what)] if *what == name),
                "{name}: {w:?}"
            );
            assert_eq!(why(&drawn, video(640, 360), None, &on), vec![], "{name}: fills the frame");
        }
        // The effect among others: the first that sees the bars as picture is the one named.
        let w = why(&without, video(640, 360), tall, &|c| {
            c.effects = vec![
                VideoEffect::Grayscale,
                VideoEffect::Sharpen { amount: 1.0 },
                VideoEffect::Blur { sigma: 2.0 },
            ];
        });
        assert!(matches!(w.as_slice(), [Unsupported::Matte(_, "a sharpen")]), "{w:?}");
        // A chroma key writes an alpha plane from that frame: FFmpeg's, whatever the caps.
        let key = |c: &mut Clip| {
            c.effects = vec![VideoEffect::ChromaKey {
                color: "green".into(),
                similarity: 0.3,
                blend: 0.1,
            }];
        };
        let w = why(&drawn, video(640, 360), tall, &key);
        assert!(w.iter().any(|u| matches!(u, Unsupported::Matte(_, "a chroma key"))), "{w:?}");
        // What the chain is not held for: another format than 8-bit 4:2:0, a mask, a clip with keys.
        let blur = |c: &mut Clip| c.effects = vec![VideoEffect::Blur { sigma: 3.0 }];
        let mut deep = video(640, 360);
        deep.pix_fmt = Some("yuv420p10le".into());
        let w = why(&drawn, deep, tall, &blur);
        assert!(
            w.iter()
                .any(|u| matches!(u, Unsupported::Effects(_, m) if m.contains("not 8-bit 4:2:0 yuv420p"))),
            "{w:?}"
        );
        let w = why(&drawn, video(640, 360), tall, &|c| {
            blur(c);
            c.mask = Some(Mask {
                shape: MaskShape::Rect,
                x: 0.5,
                y: 0.5,
                width: 0.6,
                height: 0.6,
                feather: 0.1,
                inverted: false,
            });
        });
        assert!(
            w.iter()
                .any(|u| matches!(u, Unsupported::Effects(_, m) if m.contains("under a mask"))),
            "{w:?}"
        );
        // The blur is judged on the padded frame, not on the picture: a 160x2 picture's chroma planes are one row tall (a kernel hazard,
        // `gblur::KernelHazard::OneSample`) and the 160x400 frame it sits in has none, with the slice threads pinned at 4. Judged on the
        // picture, this would be refused.
        let pinned = GpuCaps {
            graph_threads: Some(4),
            blur_lanes: 16,
            ..drawn
        };
        let sliver = Some(Delivery::new(160, 400, Fit::Contain));
        assert_eq!(why(&pinned, video(160, 2), sliver, &blur), vec![]);
        // ... and the frame itself is judged: 2 samples across is a hazard whatever stands in it.
        let narrow = Some(Delivery::new(2, 640, Fit::Contain));
        let w = why(&pinned, video(640, 360), narrow, &blur);
        assert!(
            w.iter()
                .any(|u| matches!(u, Unsupported::Effects(_, m) if m.contains("a blur on a plane"))),
            "{w:?}"
        );
    }

    /// `unsharp` is not the model on a frame of 2, 4 or 6 rows, bars or not: the frame it runs on is the padded one of a letterbox, so a
    /// picture of a few rows in a tall frame is drawn and a frame of a few rows is not.
    #[test]
    fn a_sharpen_is_refused_on_a_frame_under_8_rows_and_judged_on_the_padded_frame() {
        let drawn = GpuCaps {
            effects: EffectKinds::ALL,
            matte_effects: true,
            exact_scaler: true,
            ..GpuCaps::A0
        };
        let sharpen = |c: &mut Clip| c.effects = vec![VideoEffect::Sharpen { amount: 1.5 }];
        let why = |stream: StreamInfo, delivery: Option<Delivery>| {
            let planned = fading_plan_of(stream, delivery, CompositeColorPolicy::FixedBt601, sharpen, 0.5);
            planned.reasons(&drawn, planned.size(u32::MAX))
        };
        let refused = |w: &[Unsupported]| {
            w.iter()
                .any(|u| matches!(u, Unsupported::Effects(_, m) if m.starts_with("a sharpen on a frame under 8 rows")))
        };
        // No bars: the frame is the picture.
        for rows in [2, 4, 6] {
            let w = why(video(64, rows), Some(Delivery::new(64, rows, Fit::Contain)));
            assert!(refused(&w), "{rows} rows: {w:?}");
        }
        assert_eq!(why(video(64, 8), Some(Delivery::new(64, 8, Fit::Contain))), vec![]);
        assert_eq!(why(video(64, 16), Some(Delivery::new(64, 16, Fit::Contain))), vec![]);
        // Bars: the frame is the padded one. A 4-row picture in a 400-row frame is drawn; a 4-row picture in a frame 4 rows high is not.
        let w = why(video(160, 4), Some(Delivery::new(160, 400, Fit::Contain)));
        assert!(!refused(&w), "{w:?}");
        let w = why(video(4, 4), Some(Delivery::new(400, 4, Fit::Contain)));
        assert!(refused(&w), "{w:?}");
        // Another effect on the same frame is not held for it.
        let planned = fading_plan_of(
            video(64, 4),
            Some(Delivery::new(64, 4, Fit::Contain)),
            CompositeColorPolicy::FixedBt601,
            |c| c.effects = vec![VideoEffect::Grayscale],
            0.5,
        );
        assert!(!refused(&planned.reasons(&drawn, planned.size(u32::MAX))));
    }

    /// A grade on a letterbox is drawn on its padded frame the same way, for a picture of even sides.
    #[test]
    fn a_grade_on_a_letterbox_is_drawn_for_an_even_picture_and_refused_for_an_odd_one() {
        let drawn = GpuCaps {
            grade: GradeKinds::ALL,
            matte_effects: true,
            exact_scaler: true,
            ..GpuCaps::A0
        };
        let without = GpuCaps {
            matte_effects: false,
            ..drawn
        };
        let curves = Grade {
            curves: Some(crate::model::Curves {
                master: vec![
                    crate::model::CurvePoint { x: 0.0, y: 0.05 },
                    crate::model::CurvePoint { x: 1.0, y: 0.95 },
                ],
                ..crate::model::Curves::default()
            }),
            ..Grade::default()
        };
        let why = |caps: &GpuCaps, stream: StreamInfo, delivery: Delivery| {
            let planned = fading_plan_of(
                stream,
                Some(delivery),
                CompositeColorPolicy::FixedBt601,
                |c| c.grade = curves.clone(),
                0.5,
            );
            planned.reasons(caps, planned.size(u32::MAX))
        };
        // 640x360 into 640x720: a picture of its own size and even, on bars.
        assert_eq!(why(&drawn, video(640, 360), Delivery::new(640, 720, Fit::Contain)), vec![]);
        let w = why(&without, video(640, 360), Delivery::new(640, 720, Fit::Contain));
        assert!(matches!(w.as_slice(), [Unsupported::Matte(_, "a grade")]), "{w:?}");
        // An odd picture (203 rows): the conversions read past the line of the picture the pad lays in the frame.
        let w = why(&drawn, video(640, 360), Delivery::new(360, 640, Fit::Contain));
        assert!(
            w.iter()
                .any(|u| matches!(u, Unsupported::GradeWith(_, m) if m.contains("odd side"))),
            "{w:?}"
        );
        assert!(!w.iter().any(|u| matches!(u, Unsupported::Matte(..))), "{w:?}");
    }

    /// With no `eq` ahead of a grade the fit `scale` outputs RGB, so `pad` runs in RGB and keeps the picture at the odd offset `(ow - iw) / 2`;
    /// the compositor pads at the even one. Refused at an odd offset on either axis with no `eq`; drawn with an `eq` (the pad stays in 4:2:0),
    /// at an even offset, and over a blurred fill (composed in YUV).
    #[test]
    fn a_grade_on_a_letterbox_with_no_eq_is_refused_at_an_odd_offset() {
        let drawn = GpuCaps {
            grade: GradeKinds::ALL,
            matte_effects: true,
            exact_scaler: true,
            ..GpuCaps::A0
        };
        let curves = Grade {
            curves: Some(crate::model::Curves {
                master: vec![
                    crate::model::CurvePoint { x: 0.0, y: 0.05 },
                    crate::model::CurvePoint { x: 1.0, y: 0.95 },
                ],
                ..crate::model::Curves::default()
            }),
            ..Grade::default()
        };
        let eq = Color {
            contrast: 1.1,
            ..Color::default()
        };
        let why = |stream: StreamInfo, delivery: Delivery, with_eq: bool| {
            let planned = fading_plan_of(
                stream,
                Some(delivery),
                CompositeColorPolicy::FixedBt601,
                |c| {
                    c.grade = curves.clone();
                    if with_eq {
                        c.color = eq;
                    }
                },
                0.5,
            );
            planned.reasons(&drawn, planned.size(u32::MAX))
        };
        let refused = |w: &[Unsupported]| {
            w.iter()
                .any(|u| matches!(u, Unsupported::GradeWith(_, m) if m.contains("at an odd offset")))
        };
        // 640x360 in 640x722: y 181. In 642x360: x 1. 1080x1350 in 1080x1920: y 285.
        for (stream, canvas) in [
            (video(640, 360), Delivery::new(640, 722, Fit::Contain)),
            (video(640, 360), Delivery::new(642, 360, Fit::Contain)),
            (video(1080, 1350), Delivery::new(1080, 1920, Fit::Contain)),
        ] {
            let w = why(stream.clone(), canvas, false);
            assert!(refused(&w), "{canvas:?}: {w:?}");
            // An `eq` ahead keeps the pad in 4:2:0.
            assert_eq!(why(stream.clone(), canvas, true), vec![], "{canvas:?} with an eq");
            // Over a blurred fill the frame is composed in YUV.
            let w = why(stream, Delivery::new(canvas.width, canvas.height, Fit::Blur), false);
            assert!(!refused(&w), "{canvas:?} filled: {w:?}");
        }
        // Even offsets: 640x360 in 640x724 (y 182), in 640x720 (y 180).
        for canvas in [Delivery::new(640, 724, Fit::Contain), Delivery::new(640, 720, Fit::Contain)] {
            assert_eq!(why(video(640, 360), canvas, false), vec![], "{canvas:?}");
        }
    }

    /// A `yuv420p` flagged full range reaches the graph's filters raw on every FFmpeg (measured 6.1.1 and
    /// 9.0.2), where the compositor's decode has converted it: so what reads the values is FFmpeg's, under
    /// any policy. The same picture with no flag, a `tv` one, or a file probed before the flag was recorded
    /// is drawn as it always was.
    #[test]
    fn a_yuv420p_flagged_full_range_is_refused_where_a_filter_reads_its_raw_samples() {
        let caps = GpuCaps {
            effects: EffectKinds::ALL,
            mask: true,
            fades: true,
            ..GpuCaps::A0
        };
        let stream = |fmt: &str, range: Option<&str>| StreamInfo {
            pix_fmt: Some(fmt.into()),
            color_range: range.map(Into::into),
            ..video(640, 360)
        };
        let why = |s: StreamInfo, policy: CompositeColorPolicy, edit: &dyn Fn(&mut Clip), t: f64| {
            let plan = fading_plan_of(s, None, policy, |c| edit(c), t);
            plan.reasons(&caps, plan.size(u32::MAX))
        };
        let key = || VideoEffect::ChromaKey {
            color: "green".into(),
            similarity: 0.15,
            blend: 0.1,
        };
        let effect = |why: &[Unsupported], what: &str| {
            why.iter()
                .any(|u| matches!(u, Unsupported::Effects(_, w) if w.contains(what)))
        };
        let policies = [
            CompositeColorPolicy::FixedBt601,
            CompositeColorPolicy::BottomLayerTag,
            CompositeColorPolicy::Unknown,
        ];
        for policy in policies {
            let flagged = || stream("yuv420p", Some("pc"));
            // Drawn plain where the still converts the range at the end, as the compositor does (FFmpeg 6); under a negotiating policy the
            // still's own planes are full-range ones and the picture is refused for it.
            let plain = why(flagged(), policy, &|_| {}, 0.5);
            if policy == CompositeColorPolicy::FixedBt601 {
                assert_eq!(plain, vec![], "{policy:?}");
            } else {
                assert!(
                    matches!(plain.as_slice(), [Unsupported::FullRangeStill(_)]),
                    "{policy:?}: {plain:?}"
                );
            }
            let vignette = why(flagged(), policy, &|c| c.effects = vec![VideoEffect::Vignette], 0.5);
            assert!(effect(&vignette, "flagged full range"), "{policy:?}: {vignette:?}");
            assert!(effect(&vignette, "vignette"), "{policy:?}: {vignette:?}");
            let keyed = why(flagged(), policy, &|c| c.effects = vec![key()], 0.5);
            assert!(effect(&keyed, "flagged full range"), "{policy:?}: {keyed:?}");
            assert!(effect(&keyed, "chroma key"), "{policy:?}: {keyed:?}");
            for table in [
                VideoEffect::Invert,
                VideoEffect::Blur { sigma: 3.0 },
                VideoEffect::Sharpen { amount: 1.0 },
            ] {
                let tabled = why(flagged(), policy, &|c| c.effects = vec![table.clone()], 0.5);
                assert!(effect(&tabled, "full-range"), "{policy:?} {table:?}: {tabled:?}");
            }
            // (The picture's range in a still, under FFmpeg 9, is its own reason beside these: `FullRangeStill`, above.)
            let other_than_still = |v: Vec<Unsupported>| -> Vec<Unsupported> {
                v.into_iter()
                    .filter(|u| !matches!(u, Unsupported::FullRangeStill(_)))
                    .collect()
            };
            let graded = other_than_still(why(flagged(), policy, &|c| c.color.gamma = 1.4, 0.5));
            assert!(
                matches!(graded.as_slice(), [Unsupported::GradedFullRange(_, d)] if d.contains("color_range=pc")),
                "{policy:?}: {graded:?}"
            );
            // Inside a running fade, under FFmpeg 6 as well (nothing converts its range ahead of `fade`);
            // drawn once the fade is over.
            let fading = why(flagged(), policy, &|c| c.fade_in = 1.0, 0.5);
            assert!(
                matches!(fading.as_slice(), [Unsupported::FullRangeNegotiated(_)]),
                "{policy:?}: {fading:?}"
            );
            assert_eq!(
                other_than_still(why(flagged(), policy, &|c| c.fade_in = 1.0, 5.0)),
                vec![],
                "{policy:?}"
            );
            // Not flagged, flagged limited, or probed before the flag: drawn as before.
            for range in [None, Some("tv")] {
                let tagged = || stream("yuv420p", range);
                assert_eq!(
                    why(tagged(), policy, &|c| c.effects = vec![VideoEffect::Vignette], 0.5),
                    vec![],
                    "{policy:?} {range:?}"
                );
                assert_eq!(
                    why(tagged(), policy, &|c| c.effects = vec![key()], 0.5),
                    vec![],
                    "{policy:?} {range:?}"
                );
                assert_eq!(
                    why(tagged(), policy, &|c| c.color.gamma = 1.4, 0.5),
                    vec![],
                    "{policy:?} {range:?}"
                );
                assert_eq!(
                    why(tagged(), policy, &|c| c.fade_in = 1.0, 0.5),
                    vec![],
                    "{policy:?} {range:?}"
                );
            }
        }
        // A `yuvj` picture is converted at the decode by FFmpeg 6 and fades as drawn there: its flag
        // (ffprobe prints `pc` for it too) adds nothing.
        let jpeg = || stream("yuvj420p", Some("pc"));
        assert_eq!(
            why(jpeg(), CompositeColorPolicy::FixedBt601, &|c| c.fade_in = 1.0, 0.5),
            vec![]
        );
        // Another format flagged `pc` is converted ahead of the graph by FFmpeg 6 (10-bit measured exact) and
        // handled raw by FFmpeg 9, which is told by the flag now rather than guessed from the depth.
        for fmt in ["yuv420p10le", "yuv422p", "yuv444p"] {
            // (A picture flagged `pc` in a still under a negotiated range is refused for it as well: `FullRangeStill`, judged elsewhere.)
            let graded = |range: Option<&str>, policy| {
                why(stream(fmt, range), policy, &|c| c.color.gamma = 1.4, 0.5)
                    .into_iter()
                    .filter(|u| !matches!(u, Unsupported::FullRangeStill(_)))
                    .collect::<Vec<_>>()
            };
            assert_eq!(graded(Some("pc"), CompositeColorPolicy::FixedBt601), vec![], "{fmt}");
            assert!(
                matches!(
                    graded(Some("pc"), CompositeColorPolicy::BottomLayerTag).as_slice(),
                    [Unsupported::GradedFullRange(..)]
                ),
                "{fmt}"
            );
        }
        // RGB and gray pictures read `pc` from ffprobe on every FFmpeg (nothing limited to be told from): no flag, so a
        // PNG or a gray clip is drawn, graded and in a cut, as before.
        for fmt in ["rgb24", "bgr0", "gbrp", "gray", "gray10le"] {
            for policy in policies {
                let graded = why(stream(fmt, Some("pc")), policy, &|c| c.color.gamma = 1.4, 0.5);
                let same = why(stream(fmt, None), policy, &|c| c.color.gamma = 1.4, 0.5);
                assert_eq!(graded, same, "{fmt} {policy:?}: a `pc` word on {fmt} changes nothing");
            }
        }
        // The depth heuristic stays for a file that does not say: deeper than 8 bits may be full range
        // where FFmpeg 9 keeps a tag's raw values, tagged limited or not.
        for range in [None, Some("tv")] {
            let graded = why(
                stream("yuv420p10le", range),
                CompositeColorPolicy::BottomLayerTag,
                &|c| c.color.gamma = 1.4,
                0.5,
            );
            assert!(
                matches!(graded.as_slice(), [Unsupported::GradedFullRange(..)]),
                "{range:?}: {graded:?}"
            );
        }
    }

    /// `chromakey` starts its window over at each slice of the graph, so its alpha is drawn at one count of slice threads. Where the plan
    /// knows the count (a pin, a single frame of proxies and images, a range of one number: the budget's cap, a core count it reproduces)
    /// the key is drawn; where the machine says only a range (the count is not reproduced: another system, or a failed
    /// `sched_getaffinity`, where FFmpeg counts one) no count of it is assumed and the key is refused, with the low end of the range
    /// never taken for the count.
    #[test]
    fn a_chroma_key_over_a_graph_whose_thread_count_is_not_known_is_refused() {
        // (The last assertions read the machine's range, which the budget moves: the tests that move it hold this lock.)
        let _serial = crate::engine::cpu::test_lock();
        let caps = GpuCaps {
            effects: EffectKinds::ALL,
            ..GpuCaps::A0
        };
        let keyed = |plain: bool| {
            fading_plan(
                |c| {
                    if !plain {
                        c.effects = vec![VideoEffect::ChromaKey {
                            color: "green".into(),
                            similarity: 0.15,
                            blend: 0.1,
                        }];
                    }
                },
                0.5,
            )
        };
        let not_known = |plan: &RenderPlan, caps: &GpuCaps, machine: (usize, usize)| {
            plan.reasons_over(caps, plan.size(u32::MAX), &move || machine)
                .iter()
                .any(|u| matches!(u, Unsupported::Effects(_, w) if w.contains("slice-thread count is not known")))
        };
        let plan = keyed(false);
        // The machine says a range: refused. One number, or a pin: drawn.
        assert!(not_known(&plan, &caps, (1, 16)));
        assert!(not_known(&plan, &caps, (5, 16)));
        assert!(!not_known(&plan, &caps, (5, 5)));
        assert!(!not_known(&plan, &caps, (1, 1)));
        let pinned = GpuCaps {
            graph_threads: Some(3),
            ..caps
        };
        assert!(!not_known(&plan, &pinned, (1, 16)), "a pin is the count");
        // A single frame of proxies and images runs on one thread whatever the machine says.
        let mut still = keyed(false);
        still.layers[0].source.proxy = true;
        assert!(still.is_single_frame_graph());
        assert!(!not_known(&still, &caps, (1, 16)));
        // A layer with no key is not judged by it, and the cap is the effect's: without it the key is refused as an effect.
        assert!(!not_known(&keyed(true), &caps, (1, 16)));
        let without = GpuCaps {
            effects: EffectKinds::NONE,
            ..GpuCaps::A0
        };
        assert!(!not_known(&plan, &without, (1, 16)));
        // The public range and the exact count agree.
        let (lo, hi) = plan.graph_thread_range(&caps);
        assert_eq!(plan.exact_graph_threads(&caps), (lo == hi).then_some(lo));
        assert_eq!(plan.exact_graph_threads(&pinned), Some(3));
    }

    #[test]
    fn a_chroma_key_is_drawn_only_on_the_chroma_ffmpeg_keys() {
        let caps = GpuCaps {
            effects: EffectKinds::ALL,
            mask: true,
            graph_threads: Some(1),
            ..GpuCaps::A0
        };
        let key = || VideoEffect::ChromaKey {
            color: "green".into(),
            similarity: 0.15,
            blend: 0.1,
        };
        let why_of = |stream: StreamInfo, delivery: Option<Delivery>, edit: &dyn Fn(&mut Clip)| {
            let plan = fading_plan_of(stream, delivery, CompositeColorPolicy::FixedBt601, |c| edit(c), 0.5);
            plan.reasons(&caps, plan.size(u32::MAX))
        };
        let why = |edit: &dyn Fn(&mut Clip)| why_of(video(640, 360), None, edit);
        let effects = |why: &[Unsupported], what: &str| {
            why.iter()
                .any(|u| matches!(u, Unsupported::Effects(_, w) if w.contains(what)))
        };
        // At the picture's size, after the exact effects (tables, a sharpen of the luma), a grade.
        assert_eq!(why(&|c| c.effects = vec![key()]), vec![]);
        assert_eq!(
            why(&|c| {
                c.color.saturation = 1.3;
                c.effects = vec![
                    VideoEffect::Invert,
                    VideoEffect::Sharpen { amount: 2.0 },
                    key(),
                    VideoEffect::Grayscale,
                ];
            }),
            vec![]
        );
        // A crop and a Cover that cut the picture without scaling it (the cropped 640x324 covers 640x300 as it
        // is), and a vignette ahead of the key (exact).
        assert_eq!(
            why_of(video(640, 360), Some(Delivery::new(640, 300, Fit::Cover)), &|c| {
                c.transform.crop_top = 0.1;
                c.effects = vec![key()];
            }),
            vec![]
        );
        assert_eq!(
            why_of(video(640, 360), Some(Delivery::new(640, 300, Fit::Cover)), &|c| c.effects =
                vec![key()]),
            vec![]
        );
        assert_eq!(why(&|c| c.effects = vec![VideoEffect::Vignette, key()]), vec![]);
        // Not with an option `chromakey` refuses itself.
        for (similarity, blend) in [(0.0, 0.1), (1.5, 0.1), (0.1, -0.1), (0.1, 1.01), (f64::NAN, 0.1)] {
            let refused = why(&|c| {
                c.effects = vec![VideoEffect::ChromaKey {
                    color: "green".into(),
                    similarity,
                    blend,
                }];
            });
            assert!(effects(&refused, "chromakey refuses"), "{similarity} {blend}: {refused:?}");
        }
        // Not on a picture under two samples a side (where the window's edge rules need a row and a column).
        let thin = why_of(video(640, 1), None, &|c| c.effects = vec![key()]);
        assert!(effects(&thin, "narrower or shorter than 2"), "{thin:?}");
        // Not after a scale (the key runs after every effect); after a blur it is drawn, wherever the blur is in the list: the
        // compositor's blur is `gblur`'s to the sample.
        assert!(effects(
            &why(&|c| {
                c.transform.scale = 0.6;
                c.effects = vec![key()];
            }),
            "chroma key after a scale"
        ));
        // ... unless the scaler is measured to be swscale's to the sample (`exact_scaler`, as the sharpen's and the grade's rules
        // have it): the chroma the key reads is then FFmpeg's, with any other effect before it.
        let exact = GpuCaps {
            exact_scaler: true,
            ..caps
        };
        for effects_before in [
            vec![],
            vec![VideoEffect::Blur { sigma: 2.0 }],
            vec![VideoEffect::Sharpen { amount: 1.5 }],
            vec![VideoEffect::Invert, VideoEffect::Grayscale],
            vec![VideoEffect::Blur { sigma: 3.0 }, VideoEffect::Sharpen { amount: 0.5 }],
        ] {
            for scale in [0.4985, 0.6, 0.9] {
                let plan = fading_plan_of(
                    video(640, 360),
                    None,
                    CompositeColorPolicy::FixedBt601,
                    |c| {
                        c.transform.scale = scale;
                        c.effects = effects_before.iter().cloned().chain([key()]).collect();
                    },
                    0.5,
                );
                assert_eq!(
                    plan.reasons(&exact, plan.size(u32::MAX)),
                    vec![],
                    "{effects_before:?} then a key at scale {scale}"
                );
                assert!(
                    effects(&plan.reasons(&caps, plan.size(u32::MAX)), "chroma key after a scale"),
                    "{effects_before:?} then a key at scale {scale}: refused where the scaler is not measured"
                );
            }
        }
        assert!(
            why(&|c| c.effects = vec![key(), VideoEffect::Blur { sigma: 2.0 }]).is_empty(),
            "a key beside a blur is drawn"
        );
        // Not on a letterbox, another pixel format, a rotated layer, or under a mask.
        let boxed = why_of(video(640, 360), Some(Delivery::new(360, 640, Fit::Contain)), &|c| {
            c.effects = vec![key()];
        });
        assert!(
            boxed
                .iter()
                .any(|u| matches!(u, Unsupported::Matte(_, w) if w.contains("chroma key"))),
            "{boxed:?}"
        );
        for pix_fmt in ["yuvj420p", "yuv422p", "yuv420p10le", "rgb24"] {
            let mut stream = video(640, 360);
            stream.pix_fmt = Some(pix_fmt.into());
            assert!(
                effects(&why_of(stream, None, &|c| c.effects = vec![key()]), "yuv420p"),
                "{pix_fmt}"
            );
        }
        assert!(effects(
            &why(&|c| {
                c.transform.rotation = 10.0;
                c.effects = vec![key()];
            }),
            "chroma key on a rotated layer"
        ));
        // ... a constant opacity below 1 is drawn (`colorchannelmixer` maps each level of the key's alpha as it does a mask's), and so is
        // the sampled opacity of a still; a blend mode on it is not (the blend chain carries the opaque plane alone), nor is a rotation.
        assert_eq!(
            why(&|c| {
                c.transform.opacity = 0.5;
                c.effects = vec![key()];
            }),
            vec![]
        );
        assert!(effects(
            &why(&|c| {
                c.transform.opacity = 0.5;
                c.blend_mode = BlendMode::Multiply;
                c.effects = vec![key()];
            }),
            "chroma key on a blended layer below full opacity"
        ));
        assert!(effects(
            &why(&|c| {
                c.transform.opacity = 0.5;
                c.transform.rotation = 90.0;
                c.effects = vec![key()];
            }),
            "chroma key on a rotated layer"
        ));
        // ... but not where the round trip is not measured to be the compositor's.
        let unmeasured = fading_plan(
            |c| {
                c.transform.opacity = 0.5;
                c.effects = vec![key()];
            },
            0.5,
        );
        let why_unmeasured = unmeasured.reasons(
            &GpuCaps {
                translucent_approximate: true,
                translucent_odd: false,
                ..caps
            },
            unmeasured.size(u32::MAX),
        );
        assert!(
            matches!(why_unmeasured.as_slice(), [Unsupported::TranslucentApproximate(_)]),
            "{why_unmeasured:?}"
        );
        let masked = why(&|c| {
            c.mask = Some(crate::model::Mask::default());
            c.effects = vec![key()];
        });
        assert!(
            matches!(masked.as_slice(), [Unsupported::MaskWith(_, w)] if w.contains("chroma key")),
            "{masked:?}"
        );
    }

    #[test]
    fn a_sharpen_that_amplifies_a_scale_past_two_levels_is_refused() {
        use crate::model::Keyframe;
        let caps = GpuCaps {
            effects: EffectKinds::BLUR
                .union(EffectKinds::SHARPEN)
                .union(EffectKinds::GRAYSCALE)
                .union(EffectKinds::INVERT),
            keyed_zoom: true,
            // (What is ahead of a moving zoom, judged as if its placement were drawn.)
            placement: true,
            graph_threads: Some(1),
            ..GpuCaps::A0
        };
        let sharp = |amount: f64| VideoEffect::Sharpen { amount };
        let blur = || VideoEffect::Blur { sigma: 3.0 };
        let reasons = |edit: &dyn Fn(&mut Clip), width: u32| {
            let plan = fading_plan(|c| edit(c), 0.5);
            plan.reasons(&caps, plan.size(width))
        };
        let refused = |why: &[Unsupported]| matches!(why, [Unsupported::Effects(_, w)] if w.contains("sharpen after a scale"));
        let pip = |effects: Vec<VideoEffect>| {
            move |c: &mut Clip| {
                c.transform.scale = 0.6;
                c.effects = effects.clone();
            }
        };
        let plain = |effects: Vec<VideoEffect>| move |c: &mut Clip| c.effects = effects.clone();
        // After a scale: an amount past 0.5 is refused (either sign), up to it drawn, a blur is not touched.
        for amount in [0.6, 1.5, 5.0, -1.0] {
            let why = reasons(&pip(vec![sharp(amount)]), u32::MAX);
            assert!(refused(&why), "{amount}: {why:?}");
        }
        for amount in [0.0, 0.3, 0.5, -0.5] {
            let why = reasons(&pip(vec![sharp(amount)]), u32::MAX);
            assert!(why.is_empty(), "{amount}: {why:?}");
        }
        assert!(reasons(&pip(vec![blur()]), u32::MAX).is_empty());
        // Sharpens compound, and each one after the first rounds up a level of its own however small: two
        // of 0.5, two of 0.2 or of 0.001, three of 0.1. A table beside them changes nothing.
        for amounts in [vec![0.5, 0.5], vec![0.2, 0.2], vec![0.001, 0.001], vec![0.1, 0.1, 0.1]] {
            let effects: Vec<VideoEffect> = amounts.iter().map(|a| sharp(*a)).collect();
            let why = reasons(&pip(effects.clone()), u32::MAX);
            assert!(refused(&why), "pip {amounts:?}: {why:?}");
            // (After a blur nothing is amplified: the blur the caps hold is `gblur`'s to the sample.)
            let blurred: Vec<VideoEffect> = std::iter::once(blur()).chain(effects).collect();
            let why = reasons(&plain(blurred), u32::MAX);
            assert!(why.is_empty(), "blur then {amounts:?}: {why:?}");
        }
        let why = reasons(&pip(vec![sharp(0.5), VideoEffect::Invert, sharp(0.5)]), u32::MAX);
        assert!(refused(&why), "{why:?}");
        // A sharpen of nothing counts for nothing: zero, or less than `unsharp`'s 16 bits hold (1/65536 is its step).
        assert!(reasons(&pip(vec![sharp(0.5), sharp(0.0)]), u32::MAX).is_empty());
        assert!(reasons(&pip(vec![sharp(0.0), sharp(0.5)]), u32::MAX).is_empty());
        assert!(reasons(&pip(vec![sharp(0.5), sharp(0.00001), sharp(-0.00001)]), u32::MAX).is_empty());
        assert!(reasons(&pip(vec![sharp(0.001)]), u32::MAX).is_empty());
        assert!(reasons(&plain(vec![blur(), sharp(0.0), sharp(0.5)]), u32::MAX).is_empty());
        // A blur is no source of a difference: a sharpen after one, scaled or not, is drawn at any amount, chained.
        for amount in [0.5, 0.6, 1.0, 5.0] {
            let why = reasons(&plain(vec![blur(), sharp(amount)]), u32::MAX);
            assert!(why.is_empty(), "blur then sharpen {amount}: {why:?}");
        }
        assert!(reasons(&plain(vec![blur(), VideoEffect::Grayscale, sharp(1.0)]), u32::MAX).is_empty());
        assert!(reasons(&plain(vec![blur(), sharp(0.5), sharp(0.5)]), u32::MAX).is_empty());
        assert!(reasons(&plain(vec![VideoEffect::Blur { sigma: 0.0 }, sharp(5.0)]), u32::MAX).is_empty());
        // The other way round, or with nothing upstream, the sharpens are exact: any amount, chained.
        assert!(reasons(&plain(vec![sharp(5.0)]), u32::MAX).is_empty());
        assert!(reasons(&plain(vec![sharp(0.5), sharp(0.5)]), u32::MAX).is_empty());
        assert!(reasons(&plain(vec![sharp(0.2), sharp(0.2), sharp(0.001)]), u32::MAX).is_empty());
        assert!(reasons(&plain(vec![sharp(3.0), blur()]), u32::MAX).is_empty());
        assert!(reasons(&plain(vec![sharp(2.0), VideoEffect::Invert, sharp(2.0), blur()]), u32::MAX).is_empty());
        // Rendered smaller the fit scale is a real one, and the picture is no longer exact.
        let why = reasons(&plain(vec![sharp(5.0)]), 480);
        assert!(refused(&why), "{why:?}");
        // A moving zoom comes last: only the fit scale is ahead of the effects.
        let zooming = |effects: Vec<VideoEffect>| {
            move |c: &mut Clip| {
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
                c.effects = effects.clone();
            }
        };
        let zoomed = zooming(vec![sharp(3.0)]);
        assert!(reasons(&zoomed, u32::MAX).is_empty(), "{:?}", reasons(&zoomed, u32::MAX));
        let why = reasons(&zoomed, 480);
        assert!(refused(&why), "{why:?}");
        // (a blur ahead of a zoom is no source of a difference either)
        let why = reasons(&zooming(vec![blur(), sharp(1.0)]), u32::MAX);
        assert!(why.is_empty(), "{why:?}");
        // Where the scaler is swscale's to the sample (`exact_scaler`) a scale is no source of a difference either: the same
        // sharpens on the same pip are drawn.
        let exact = GpuCaps {
            exact_scaler: true,
            ..caps
        };
        for amounts in [vec![5.0], vec![0.5, 0.5], vec![0.2, 0.2, 0.001]] {
            let effects: Vec<VideoEffect> = amounts.iter().map(|a| sharp(*a)).collect();
            let plan = fading_plan(|c| pip(effects.clone())(c), 0.5);
            let why = plan.reasons(&exact, plan.size(u32::MAX));
            assert!(why.is_empty(), "pip {amounts:?}: {why:?}");
        }
    }

    #[test]
    fn a_grade_whose_tables_amplify_the_scalers_level_past_two_is_refused_on_a_layer_it_resizes() {
        use crate::model::Keyframe;
        let caps = GpuCaps {
            effects: EffectKinds::SHARPEN.union(EffectKinds::BLUR).union(EffectKinds::INVERT),
            keyed_zoom: true,
            // (What is ahead of a moving zoom, judged as if its placement were drawn.)
            placement: true,
            ..GpuCaps::A0
        };
        let grade = |brightness: f64, contrast: f64, saturation: f64, gamma: f64, temperature: f64| Color {
            brightness,
            contrast,
            saturation,
            gamma,
            temperature,
        };
        let steepest = |why: &[Unsupported]| {
            why.iter().find_map(|u| match u {
                Unsupported::GradeSteep(_, n) => Some(*n),
                _ => None,
            })
        };
        let pip = |color: Color| {
            move |c: &mut Clip| {
                c.transform.scale = 0.6;
                c.color = color;
            }
        };
        let why_of = |edit: &dyn Fn(&mut Clip), width: u32| {
            let plan = fading_plan(|c| edit(c), 0.5);
            plan.reasons(&caps, plan.size(width))
        };
        // Up to a step of two between neighbouring table entries the scaler's level is two at worst, which is what a
        // scaled blur is held to; past it the frame is FFmpeg's, and the reason says how steep. (The boundary: contrast
        // 2 steps by exactly 2, 2.05 by 3; the chroma tables' contrast is the saturation, and a temperature is a power
        // of the chroma toward 1.)
        for (name, color, step) in [
            ("identity", Color::default(), None),
            ("brightness alone", grade(0.3, 1.0, 1.0, 1.0, 0.0), None),
            ("the montage's grade", grade(0.0, 1.1, 1.2, 1.0, 0.0), None),
            ("contrast 2", grade(0.0, 2.0, 1.0, 1.0, 0.0), None),
            ("contrast 2.05", grade(0.0, 2.05, 1.0, 1.0, 0.0), Some(3)),
            ("contrast 4", grade(0.0, 4.0, 1.0, 1.0, 0.0), Some(4)),
            ("saturation 2", grade(0.0, 1.0, 2.0, 1.0, 0.0), None),
            ("saturation 2.05", grade(0.0, 1.0, 2.05, 1.0, 0.0), Some(3)),
            ("saturation 3", grade(0.0, 1.0, 3.0, 1.0, 0.0), Some(3)),
            ("temperature 1", grade(0.0, 1.0, 1.0, 1.0, 1.0), None),
            ("temperature 1 and saturation 2", grade(0.0, 1.0, 2.0, 1.0, 1.0), Some(3)),
            // The dark end: a table that leaves 0 for its first entry steps from nothing to a power of one
            // sample, in the luma when the contrast or a negative brightness parks the black point above
            // the bottom of the range, and a brightness that lifts it clear of the table has no such step.
            ("gamma 1.2", grade(0.0, 1.0, 1.0, 1.2, 0.0), None),
            ("contrast 1.8 and gamma 1.2", grade(0.0, 1.8, 1.0, 1.2, 0.0), Some(4)),
            ("brightness -0.3 and gamma 1.2", grade(-0.3, 1.0, 1.0, 1.2, 0.0), None),
            ("brightness -0.3 and gamma 1.5", grade(-0.3, 1.0, 1.0, 1.5, 0.0), Some(4)),
            ("brightness -0.2 and gamma 2", grade(-0.2, 1.0, 1.0, 2.0, 0.0), Some(16)),
            ("contrast 1.8 and gamma 2", grade(0.0, 1.8, 1.0, 2.0, 0.0), Some(12)),
            ("brightness 0.3 and gamma 2", grade(0.3, 1.0, 1.0, 2.0, 0.0), None),
            ("the far end of every range", grade(0.3, 1.8, 2.0, 2.0, 1.0), Some(19)),
        ] {
            let why = why_of(&pip(color), u32::MAX);
            let got = steepest(&why);
            assert_eq!(got, step, "{name}: {why:?}");
            assert_eq!(why.len(), usize::from(step.is_some()), "{name}: {why:?}");
            if let Some(step) = step {
                assert!(
                    why[0].to_string().contains(&format!("step {step} levels")),
                    "{name}: {}",
                    why[0]
                );
                assert_eq!(why[0].kind(), "grade_steep");
            }
            // At the picture's own size nothing is resized, the tables are exact, and any grade is drawn.
            let why = why_of(&|c| c.color = color, u32::MAX);
            assert!(why.is_empty(), "{name} unscaled: {why:?}");
        }
        // The scaler is what feeds the tables: rendered smaller, the fit scale is a real one; a Cover or Contain fit
        // into another frame is too; and a crop alone is not.
        let steep = grade(0.0, 1.8, 1.0, 2.0, 0.0);
        let why = why_of(&|c| c.color = steep, 480);
        assert!(steepest(&why).is_some(), "{why:?}");
        let why = why_of(
            &|c| {
                c.color = steep;
                c.transform.crop_left = 0.1;
            },
            u32::MAX,
        );
        assert!(why.is_empty(), "a crop leaves the size: {why:?}");
        let plan = fading_plan_of(
            video(360, 640),
            Some(Delivery::new(640, 360, Fit::Contain)),
            CompositeColorPolicy::FixedBt601,
            |c| c.color = steep,
            0.5,
        );
        assert!(steepest(&plan.reasons(&caps, plan.size(u32::MAX))).is_some(), "letterboxed");
        // A moving zoom comes last: only the fit scale is ahead of the `eq`.
        let zooming = |color: Color| {
            move |c: &mut Clip| {
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
                c.color = color;
            }
        };
        let why = why_of(&zooming(steep), u32::MAX);
        assert!(
            why.is_empty(),
            "the grade runs at the fit size, which is the picture's: {why:?}"
        );
        let why = why_of(&zooming(steep), 480);
        assert!(steepest(&why).is_some(), "{why:?}");
        // The grade's step is where a sharpen after it starts from (it is luma only): a luma step of two
        // plus any sharpen is past two levels, a step of one is the plain scaler's.
        let sharpened = |color: Color, amount: f64| {
            let why = why_of(
                &|c| {
                    pip(color)(c);
                    c.effects = vec![VideoEffect::Sharpen { amount }];
                },
                u32::MAX,
            );
            why.is_empty()
        };
        assert!(sharpened(grade(0.2, 1.0, 1.0, 1.0, 0.0), 0.5));
        assert!(!sharpened(grade(0.2, 1.0, 1.0, 1.0, 0.0), 0.6));
        assert!(
            sharpened(grade(0.0, 1.0, 1.0, 1.0, 1.0), 0.5),
            "a chroma step is not the luma's"
        );
        assert!(!sharpened(grade(0.0, 1.1, 1.0, 1.0, 0.0), 0.1));
        assert!(!sharpened(grade(0.0, 1.1, 1.0, 1.0, 0.0), 0.5));
    }

    #[test]
    fn an_effect_the_picture_changes_the_meaning_of_is_refused() {
        let caps = GpuCaps {
            effects: EffectKinds::BLUR
                .union(EffectKinds::SHARPEN)
                .union(EffectKinds::GRAYSCALE)
                .union(EffectKinds::INVERT),
            graph_threads: Some(1),
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

    /// Where the compositor's scaler is measured to be swscale's (`caps.exact_scaler`) there is no level for a grade or an `eq` table to
    /// multiply: a graded layer the scaler resizes is drawn when an `eq` stands between the fit `scale` and the grade (without one the
    /// fit `scale` converts to RGB itself, in the same pass), and the steepness of the tables is no refusal. Without the exact scaler
    /// every one of them is refused as before.
    #[test]
    fn where_the_scaler_is_exact_a_graded_resized_layer_is_drawn_when_an_eq_precedes_the_grade() {
        use crate::model::{CurvePoint, Curves};
        let caps = |exact: bool| GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            grade: GradeKinds::ALL,
            exact_scaler: exact,
            ..GpuCaps::A0
        };
        let curves = Grade {
            curves: Some(Curves {
                master: vec![CurvePoint { x: 0.0, y: 0.1 }, CurvePoint { x: 1.0, y: 0.9 }],
                ..Curves::default()
            }),
            ..Grade::default()
        };
        let eq = Color {
            contrast: 1.08,
            saturation: 1.15,
            ..Color::default()
        };
        let steep = Color {
            contrast: 1.8,
            gamma: 2.0,
            ..Color::default()
        };
        let video = |w: u32, h: u32| StreamInfo {
            pix_fmt: Some("yuv420p".into()),
            ..crate::engine::test_support::video_stream(w, h, 30.0)
        };
        let why = |exact: bool, delivery: Option<Delivery>, edit: &dyn Fn(&mut Clip)| {
            let plan = fading_plan_of(video(640, 360), delivery, CompositeColorPolicy::FixedBt601, |c| edit(c), 0.5);
            plan.reasons(&caps(exact), plan.size(u32::MAX))
        };
        let pip = |color: Color, grade: &Grade| {
            let (color, grade) = (color, grade.clone());
            move |c: &mut Clip| {
                c.transform.scale = 0.6;
                c.color = color;
                c.grade = grade.clone();
            }
        };
        let resizes = |w: &[Unsupported]| {
            w.iter()
                .filter(|u| matches!(u, Unsupported::GradeWith(_, m) if m.contains("resizes")))
                .count()
        };
        // Not exact: refused whatever stands ahead of the grade.
        for color in [eq, Color::default()] {
            let w = why(false, None, &pip(color, &curves));
            assert_eq!(resizes(&w), 1, "{w:?}");
        }
        // Exact: drawn with an eq ahead of it, refused for the missing eq without one.
        assert_eq!(why(true, None, &pip(eq, &curves)), vec![]);
        let w = why(true, None, &pip(Color::default(), &curves));
        assert_eq!(resizes(&w), 1, "{w:?}");
        assert!(
            w.iter()
                .any(|u| matches!(u, Unsupported::GradeWith(_, m) if m.contains("no `eq`"))),
            "{w:?}"
        );
        // An enlargement and a Cover crop are resizes the same way; a layer at its own size needs no eq.
        let enlarged = Some(Delivery::new(960, 540, Fit::Contain));
        assert_eq!(
            why(true, enlarged, &|c| {
                c.color = eq;
                c.grade = curves.clone();
            }),
            vec![]
        );
        assert_eq!(resizes(&why(true, enlarged, &|c| c.grade = curves.clone())), 1);
        assert_eq!(why(true, None, &|c| c.grade = curves.clone()), vec![]);
        // The steepness of an eq on a resized layer: refused past two without the exact scaler, drawn with it.
        let w = why(false, None, &|c| {
            c.transform.scale = 0.6;
            c.color = steep;
        });
        assert!(w.iter().any(|u| matches!(u, Unsupported::GradeSteep(..))), "{w:?}");
        let w = why(true, None, &|c| {
            c.transform.scale = 0.6;
            c.color = steep;
        });
        assert_eq!(w, vec![]);
        // (A letterbox is still a matte the grade sees.)
        let w = why(true, Some(Delivery::new(1280, 360, Fit::Contain)), &|c| {
            c.color = eq;
            c.grade = curves.clone();
        });
        assert!(w.iter().any(|u| matches!(u, Unsupported::Matte(_, "a grade"))), "{w:?}");
    }

    /// The grade's chain, judged: what the compositor reproduces (8-bit limited-range `yuv420p` at an even size
    /// that is not resized, BT.601 / BT.709 / untagged, nothing after the grade that sees the RGB or 4:4:4 picture)
    /// and each thing it does not, with its own reason.
    #[test]
    fn a_grade_is_drawn_for_the_pictures_its_round_trip_was_measured_on_and_refused_elsewhere() {
        use crate::model::{ClipLut, CurvePoint, Curves, Wheel, Wheels};
        let all = GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            dip_white: true,
            keyed_zoom: true,
            effects: EffectKinds::ALL,
            grade: GradeKinds::ALL,
            graph_threads: Some(1),
            ..GpuCaps::A0
        };
        let wheels = Grade {
            wheels: Some(Wheels {
                midtones: Wheel {
                    x: 0.2,
                    y: 0.1,
                    lightness: 0.05,
                },
                ..Wheels::default()
            }),
            ..Grade::default()
        };
        let curves = Grade {
            curves: Some(Curves {
                master: vec![CurvePoint { x: 0.0, y: 0.1 }, CurvePoint { x: 1.0, y: 0.9 }],
                ..Curves::default()
            }),
            ..Grade::default()
        };
        let lut = Grade {
            lut: Some(ClipLut {
                path: "/looks/a.cube".into(),
                intensity: 1.0,
            }),
            ..Grade::default()
        };
        let why = |stream: StreamInfo, delivery: Option<Delivery>, edit: &dyn Fn(&mut Clip), t: f64| {
            let plan = fading_plan_of(
                stream,
                delivery,
                CompositeColorPolicy::FixedBt601,
                |c| {
                    c.grade = wheels.clone();
                    edit(c);
                },
                t,
            );
            plan.reasons(&all, plan.size(u32::MAX))
        };
        let with = |w: Vec<Unsupported>, what: &str| {
            assert!(
                matches!(w.as_slice(), [Unsupported::GradeWith(_, m)] if m.contains(what)),
                "{what}: {w:?}"
            );
        };
        // Drawn: each part, the three, a picture that names BT.601 or BT.709, a crop, an alpha plane.
        assert!(why(video(640, 360), None, &|_| {}, 0.5).is_empty());
        for g in [
            curves.clone(),
            lut.clone(),
            Grade {
                lut: lut.lut,
                curves: curves.curves.clone(),
                wheels: wheels.wheels,
            },
        ] {
            assert!(why(video(640, 360), None, &|c| c.grade = g.clone(), 0.5).is_empty());
        }
        for tag in ["bt709", "smpte170m", "bt470bg"] {
            let stream = StreamInfo {
                color_space: Some(tag.into()),
                ..video(640, 360)
            };
            assert!(why(stream, None, &|_| {}, 0.5).is_empty(), "{tag}");
        }
        assert!(why(video(640, 360), None, &|c| c.transform.opacity = 0.5, 0.5).is_empty());
        assert!(
            why(video(640, 360), None, &|c| c.fade_in = 2.0, 5.0).is_empty(),
            "past its fade-in"
        );
        // A compositor without a part is asked about nothing else of it.
        let plan = fading_plan_of(
            video(640, 360),
            None,
            CompositeColorPolicy::FixedBt601,
            |c| c.grade = curves.clone(),
            0.5,
        );
        let only_wheels = GpuCaps {
            grade: GradeKinds::WHEELS,
            ..all
        };
        assert!(matches!(
            plan.reasons(&only_wheels, plan.size(u32::MAX)).as_slice(),
            [Unsupported::Grade(_, "curves")]
        ));
        // Formats: anything but 8-bit 4:2:0 (FFmpeg grades it in the format it negotiates), one never recorded.
        for fmt in ["yuv420p10le", "yuv444p", "yuv422p", "gray", "rgb24", "bgr0"] {
            let stream = StreamInfo {
                pix_fmt: Some(fmt.into()),
                ..video(640, 360)
            };
            with(why(stream, None, &|_| {}, 0.5), "8-bit 4:2:0");
        }
        let w = why(
            StreamInfo {
                pix_fmt: None,
                ..video(640, 360)
            },
            None,
            &|_| {},
            0.5,
        );
        assert!(
            w.iter()
                .any(|u| matches!(u, Unsupported::GradeWith(_, m) if m.contains("8-bit 4:2:0")))
                && w.iter().any(|u| matches!(u, Unsupported::GradedFullRange(..))),
            "never recorded: {w:?}"
        );
        // A full-range picture is refused as every colour correction on one is.
        let full = StreamInfo {
            pix_fmt: Some("yuvj420p".into()),
            ..video(640, 360)
        };
        let w = why(full, None, &|_| {}, 0.5);
        assert!(
            w.iter().any(|u| matches!(u, Unsupported::GradedFullRange(..))),
            "a grade on a full-range picture: {w:?}"
        );
        // A matrix with no measured round trip.
        for tag in ["bt2020nc", "fcc", "smpte240m"] {
            let stream = StreamInfo {
                color_space: Some(tag.into()),
                ..video(640, 360)
            };
            with(why(stream, None, &|_| {}, 0.5), "colour matrix");
        }
        // Effects behind the grade run on the RGB (or 4:4:4) picture FFmpeg negotiates; a chroma key is behind `format=yuva420p`.
        for e in [
            VideoEffect::Blur { sigma: 3.0 },
            VideoEffect::Sharpen { amount: 0.5 },
            VideoEffect::Grayscale,
            VideoEffect::Invert,
            VideoEffect::Vignette,
        ] {
            with(
                why(video(640, 360), None, &|c| c.effects = vec![e.clone()], 0.5),
                "video effects",
            );
        }
        let keyed = why(
            video(640, 360),
            None,
            &|c| {
                c.effects = vec![VideoEffect::ChromaKey {
                    color: "green".into(),
                    similarity: 0.3,
                    blend: 0.1,
                }];
            },
            0.5,
        );
        assert!(keyed.is_empty(), "{keyed:?}");
        // A fade to black or white that is running, and a dip to white wherever it is in the chain; a dissolve is alpha.
        with(
            why(video(640, 360), None, &|c| c.fade_in = 2.0, 0.5),
            "fade to black or white",
        );
        with(
            why(video(640, 360), None, &|c| c.fade_out = 20.0, 0.5),
            "fade to black or white",
        );
        let dip = |c: &mut Clip| {
            c.transition_in = Some(Transition {
                kind: TransitionKind::DipToWhite,
                duration: 2.0,
            });
        };
        with(why(video(640, 360), None, &dip, 0.5), "fade to black or white");
        assert!(why(
            video(640, 360),
            None,
            &|c| c.transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: 2.0
            }),
            0.5
        )
        .is_empty());
        // A moving zoom.
        let key = |time: f64, scale: f64| Keyframe {
            time,
            scale,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        };
        let w = why(
            video(640, 360),
            None,
            &|c| c.keyframes = vec![key(0.0, 1.0), key(4.0, 1.5)],
            0.5,
        );
        // (A grade ahead of a moving zoom is drawn: the placement reads the planes the grade hands on, exact behind every grade.)
        assert!(!w.iter().any(|u| matches!(u, Unsupported::ZoomBehind(..))), "{w:?}");
        // The geometry: a resize (whatever the ratio), a letterbox, an odd picture.
        with(why(video(640, 360), None, &|c| c.transform.scale = 0.5, 0.5), "resizes");
        // (A zoom in is the fit's own resample, which the plan refuses for itself: `FoldedFit`.)
        let w = why(video(640, 360), None, &|c| c.transform.scale = 1.5, 0.5);
        assert!(
            w.iter().any(|u| matches!(u, Unsupported::FoldedFit(..)))
                && w.iter().any(|u| matches!(u, Unsupported::GradeWith(..))),
            "{w:?}"
        );
        with(
            why(video(640, 360), Some(Delivery::new(960, 540, Fit::Contain)), &|_| {}, 0.5),
            "resizes",
        );
        let w = why(video(360, 640), Some(Delivery::new(640, 360, Fit::Contain)), &|_| {}, 0.5);
        // (The bars are a matte, and the portrait picture is resized into the frame, and its width is odd: all three.)
        assert!(
            matches!(
                w.as_slice(),
                [Unsupported::Matte(_, "a grade"), Unsupported::GradeWith(_, resized), Unsupported::GradeWith(_, odd)]
                    if resized.contains("resizes") && odd.contains("odd side")
            ),
            "{w:?}"
        );
        // ... where a crop the canvas is cut to is not a resize, and an odd picture is (a canvas that is odd is the only way here).
        let mut plan = fading_plan_of(
            video(641, 361),
            Some(Delivery::new(640, 360, Fit::Contain)),
            CompositeColorPolicy::FixedBt601,
            |c| c.grade = wheels.clone(),
            0.5,
        );
        plan.canvas.width = 641;
        plan.canvas.height = 361;
        plan.layers[0].transform.pos_x = 0.05;
        with(plan.reasons(&all, (641, 361)), "odd side");
        // The route the conversions take.
        let route = |stream: StreamInfo, label: bool| {
            let mut plan = fading_plan_of(
                stream,
                None,
                CompositeColorPolicy::FixedBt601,
                |c| c.grade = wheels.clone(),
                0.5,
            );
            plan.layers[0].stream.label_bt709 = label;
            plan.layers[0].grade_route()
        };
        let tagged = |tag: &str| StreamInfo {
            color_space: Some(tag.into()),
            ..video(640, 360)
        };
        let r = |matrix, via_444| Some(GradeRoute { matrix, via_444 });
        assert_eq!(
            route(video(640, 360), false),
            r(YuvMatrix::Bt601, false),
            "untagged: BT.601 both ways"
        );
        assert_eq!(route(video(640, 360), true), r(YuvMatrix::Bt709, true), "the graph's label");
        assert_eq!(route(tagged("bt709"), false), r(YuvMatrix::Bt709, true));
        assert_eq!(route(tagged("smpte170m"), false), r(YuvMatrix::Bt601, true));
        assert_eq!(route(tagged("bt470bg"), false), r(YuvMatrix::Bt601, true));
        assert_eq!(
            route(tagged("reserved"), false),
            r(YuvMatrix::Bt601, false),
            "a reserved tag is none"
        );
        assert_eq!(route(tagged("bt2020nc"), false), None);
    }

    #[test]
    fn a_dip_to_white_is_drawn_for_the_pictures_the_round_trip_was_measured_on() {
        let all = GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            dip_white: true,
            graph_threads: Some(1),
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
            "BT.709 is drawn like BT.601: the conversions are the grade's, exact"
        );
        let dipped = |w: Vec<Unsupported>, what: DipWhy| {
            assert!(
                matches!(w.as_slice(), [Unsupported::DipToWhite(_, m)] if *m == what),
                "{what:?}: {w:?}"
            );
        };
        let dipped_all = |w: Vec<Unsupported>, whys: &[DipWhy]| {
            let got: Vec<&DipWhy> = w
                .iter()
                .filter_map(|u| match u {
                    Unsupported::DipToWhite(_, why) => Some(why),
                    _ => None,
                })
                .collect();
            assert_eq!(got, whys.iter().collect::<Vec<_>>(), "{w:?}");
            assert_eq!(got.len(), w.len(), "{w:?}");
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
        // A `yuv420p` flagged full range: the round trip runs in that range on every frame of the clip (Y 2, UV 1 off the
        // compositor's limited-range tables).
        let pc = StreamInfo {
            color_range: Some("pc".into()),
            ..video(640, 360)
        };
        // (A still inside the fade is refused for the range as well: `FullRangeNegotiated`.)
        let ranged = why(pc, None, 0.5);
        assert!(
            ranged
                .iter()
                .any(|u| matches!(u, Unsupported::DipToWhite(_, DipWhy::FullRange))),
            "{ranged:?}"
        );
        // Of the effects, `negate` and `vignette` list an RGB format, so the fit `scale` converts the picture to RGB ahead of them and
        // they run there (invert 3 levels off, vignette 47); `gblur`, `unsharp` and `hue` do not and stay in the YUV.
        let with_effect = |e: VideoEffect| {
            let plan = fading_plan_of(
                video(640, 360),
                None,
                CompositeColorPolicy::FixedBt601,
                |c| {
                    white(c);
                    c.effects = vec![e.clone()];
                },
                0.5,
            );
            plan.reasons(
                &GpuCaps {
                    effects: EffectKinds::ALL,
                    ..all
                },
                plan.size(u32::MAX),
            )
        };
        dipped(with_effect(VideoEffect::Invert), DipWhy::RgbEffect);
        dipped(with_effect(VideoEffect::Vignette), DipWhy::RgbEffect);
        for e in [
            VideoEffect::Blur { sigma: 1.5 },
            VideoEffect::Sharpen { amount: 0.8 },
            VideoEffect::Grayscale,
        ] {
            let why = with_effect(e.clone());
            assert!(why.is_empty(), "{e:?}: {why:?}");
        }
        // A letterboxed picture: FFmpeg pads before the round trip, so the bars whiten as well.
        let portrait = video(360, 640);
        let delivery = Some(Delivery::new(640, 360, Fit::Contain));
        // (Padded bars, a width that comes out odd, and a resize: each its own reason, in that order.)
        dipped_all(
            why(portrait.clone(), delivery, 0.5),
            &[DipWhy::Letterboxed, DipWhy::OddSize(203, 360), DipWhy::Resized],
        );
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
        dipped_all(
            on_layer(&|c| c.transform.scale = 0.4985),
            &[DipWhy::OddSize(319, 179), DipWhy::Resized],
        );
        // A layer the scaler resizes, whatever the ratio: the scale filter converts to RGB itself.
        dipped(on_layer(&|c| c.transform.scale = 0.5), DipWhy::Resized);
        let enlarged = fading_plan_of(
            video(640, 360),
            Some(Delivery::new(960, 540, Fit::Contain)),
            CompositeColorPolicy::FixedBt601,
            white,
            0.5,
        );
        dipped(enlarged.reasons(&all, enlarged.size(u32::MAX)), DipWhy::Resized);
        // (A zoom in is the fit's own resample, which the plan refuses for itself: `FoldedFit`.)
        let zoomed = on_layer(&|c| c.transform.scale = 1.5);
        assert!(
            zoomed.iter().any(|u| matches!(u, Unsupported::FoldedFit(..)))
                && zoomed
                    .iter()
                    .any(|u| matches!(u, Unsupported::DipToWhite(_, DipWhy::Resized))),
            "{zoomed:?}"
        );
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
            // (Not running, the picture is still a full-range one in a still: the plan refuses it for its range, not for a fade.)
            for t_fade in [(1.0, 5.0), (0.0, 0.5)] {
                let over = reasons(&plan(&full, policy, t_fade.0, t_fade.1));
                assert!(
                    matches!(over.as_slice(), [Unsupported::FullRangeStill(_)]),
                    "{policy:?}: a still, fade {t_fade:?}: {over:?}"
                );
            }
            assert!(
                reasons(&plan(&video(640, 360), policy, 1.0, 0.5)).is_empty(),
                "{policy:?}: a limited-range picture fades as drawn"
            );
        }
        // FFmpeg 6 converts the range at the decode: the compositor's order.
        assert!(reasons(&plan(&full, CompositeColorPolicy::FixedBt601, 1.0, 0.5)).is_empty());
        // ... unless the scaler resizes the picture, which converts the range inside the resize
        // (chroma up to 15 levels off): refused under either FFmpeg inside a fade and plain in a still alike.
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
        for policy in [CompositeColorPolicy::FixedBt601, CompositeColorPolicy::BottomLayerTag] {
            let still = reasons(&resized(&full, policy, 0.0, 0.5));
            assert!(
                still
                    .iter()
                    .any(|w| matches!(w, Unsupported::FullRangeResized(_, m) if m.contains("in a still"))),
                "{policy:?}: {still:?}"
            );
        }
    }

    #[test]
    fn a_layer_below_full_opacity_is_refused_where_its_round_trip_is_not_measured() {
        let unmeasured = GpuCaps {
            translucent_approximate: true,
            translucent_odd: false,
            blur_lanes: 0,
            graph_threads: None,
            ..GpuCaps::A0
        };
        let draws = |caps: &GpuCaps, edit: &dyn Fn(&mut Clip)| {
            let plan = fading_plan(|c| edit(c), 0.5);
            plan.reasons(caps, plan.size(u32::MAX))
        };
        let why = draws(&unmeasured, &|c| c.transform.opacity = 0.6);
        assert!(matches!(why.as_slice(), [Unsupported::TranslucentApproximate(_)]), "{why:?}");
        // The round trip is a constant opacity's: a full-opacity layer is not refused, and where the probe measured it, neither is
        // a translucent one.
        assert!(draws(&unmeasured, &|_| {}).is_empty());
        assert!(draws(&GpuCaps::A0, &|c| c.transform.opacity = 0.6).is_empty());
        // A dissolve's alpha ramp is not the round trip.
        assert!(draws(&unmeasured, &|c| {
            c.transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: 1.0,
            });
        })
        .iter()
        .all(|w| !matches!(w, Unsupported::TranslucentApproximate(_))));
    }

    /// A 640x360 clip delivered at its own size, with the options a test names.
    fn scaled_plan(opts: &ExportOptions) -> RenderPlan {
        let a = asset("a", 10.0, vec![video(640, 360)]);
        let tl = timeline(vec![vec![Clip::new(a.id, 0.0, 10.0, 0.0)]]);
        RenderPlan::at(&tl, std::slice::from_ref(&a), opts, 1.0, CompositeColorPolicy::FixedBt601).unwrap()
    }

    fn named(scaler: &str) -> ExportOptions {
        ExportOptions {
            scaler: Some(scaler.into()),
            ..ExportOptions::default()
        }
    }

    #[test]
    fn a_named_scaler_is_drawn_where_the_compositor_measured_its_tables() {
        // `bicubic` (and no name) is the scaler every compositor of the exact kind draws; an A0 compositor has the names none.
        for none_or_bicubic in [ExportOptions::default(), named("bicubic")] {
            assert!(scaled_plan(&none_or_bicubic).gpu_supported());
        }
        for name in ["bilinear", "lanczos", "neighbor", "spline"] {
            let p = scaled_plan(&named(name));
            assert!(!p.gpu_supported(), "{name}: A0 has none of the names");
            assert!(
                p.unsupported_reasons().iter().any(|r| r.contains(name)),
                "{name}: {:?}",
                p.unsupported_reasons()
            );
            // Each is drawn by a compositor whose caps carry **that** name, and by no other.
            let kind = ScalerKinds::of_name(name).expect("a kind");
            for have in [ScalerKinds::NONE, ScalerKinds::ALL.without(kind), kind] {
                let caps = GpuCaps {
                    scalers: have,
                    ..GpuCaps::A0
                };
                let refused = p
                    .size_free_reasons(&caps)
                    .iter()
                    .any(|r| matches!(r, Unsupported::Scaler(s) if s == name));
                assert_eq!(refused, !have.contains(kind), "{name} with {have:?}");
            }
            let caps = GpuCaps {
                scalers: ScalerKinds::ALL,
                ..GpuCaps::A0
            };
            assert!(
                p.size_free_reasons(&caps).is_empty(),
                "{name}: {:?}",
                p.size_free_reasons(&caps)
            );
        }
        // A name the engine's list does not have is never drawn, whatever the caps say (the engine writes no flags for one, and the case
        // of `bicubic` alone is forgiven: it names the default).
        let all = GpuCaps {
            scalers: ScalerKinds::ALL,
            ..GpuCaps::A0
        };
        for stranger in ["Lanczos", "LANCZOS", "area", "gauss", "x", "bilinear:x=1", ""] {
            let p = scaled_plan(&named(stranger));
            assert!(
                p.size_free_reasons(&all)
                    .iter()
                    .any(|r| matches!(r, Unsupported::Scaler(s) if s == stranger)),
                "{stranger}"
            );
        }
        assert!(scaled_plan(&named("Bicubic")).gpu_supported());
    }

    #[test]
    fn a_scaler_refusal_is_settled_only_for_a_name_no_probe_can_prove() {
        for name in ["lanczos", "spline", "neighbor", "bilinear"] {
            assert!(
                !Unsupported::Scaler(name.into()).frame_wide(),
                "{name}: a measurement lifts it"
            );
        }
        for name in ["Lanczos", "area", "", "bicubic:1"] {
            assert!(Unsupported::Scaler(name.into()).frame_wide(), "{name}");
        }
    }

    #[test]
    fn a_shrink_is_judged_against_the_limit_of_the_scaler_the_delivery_names() {
        assert_eq!(max_shrink_for(None), MAX_SHRINK);
        for name in ["bicubic", "bilinear", "neighbor", "nope", "Lanczos"] {
            assert_eq!(max_shrink_for(Some(name)), MAX_SHRINK, "{name}");
        }
        assert_eq!(max_shrink_for(Some("lanczos")), MAX_SHRINK_LANCZOS);
        assert_eq!(max_shrink_for(Some("spline")), MAX_SHRINK_SPLINE);
        const { assert!(MAX_SHRINK_SPLINE < MAX_SHRINK_LANCZOS && MAX_SHRINK_LANCZOS < MAX_SHRINK) };
        assert!(!shrinks_past(42, 1, MAX_SHRINK_LANCZOS) && shrinks_past(43, 1, MAX_SHRINK_LANCZOS));
        // A 1280x720 clip into smaller and smaller frames (Contain): 34 px wide is 37.6:1, 28 px is 45.7:1, 74 px is 17.3:1.
        let a = asset("a", 10.0, vec![video(1280, 720)]);
        let shrunk = |w: u32, h: u32, scaler: Option<&str>| {
            let mut tl = timeline(vec![vec![Clip::new(a.id, 0.0, 10.0, 0.0)]]);
            tl.format = Some(Delivery::new(w, h, Fit::Contain));
            let opts = ExportOptions {
                scaler: scaler.map(Into::into),
                ..ExportOptions::default()
            };
            let p = RenderPlan::at(&tl, std::slice::from_ref(&a), &opts, 1.0, CompositeColorPolicy::FixedBt601).unwrap();
            let caps = GpuCaps {
                scalers: ScalerKinds::ALL,
                ..GpuCaps::A0
            };
            p.reasons(&caps, (w, h)).iter().any(|r| matches!(r, Unsupported::Shrink(..)))
        };
        for scaler in [None, Some("bicubic"), Some("bilinear"), Some("neighbor")] {
            assert!(
                !shrunk(34, 20, scaler) && !shrunk(28, 16, scaler) && !shrunk(74, 41, scaler),
                "{scaler:?}"
            );
        }
        assert!(!shrunk(34, 20, Some("lanczos")) && shrunk(28, 16, Some("lanczos")) && !shrunk(74, 41, Some("lanczos")));
        assert!(shrunk(34, 20, Some("spline")) && shrunk(28, 16, Some("spline")) && !shrunk(74, 41, Some("spline")));
        assert!(shrunk(70, 39, Some("spline")), "18.3:1 across");
    }

    #[test]
    fn a_shrink_refusal_says_the_limit_of_the_scaler_that_refused() {
        let a = asset("a", 10.0, vec![video(1280, 720)]);
        let refusal = |scaler: Option<&str>| {
            let mut tl = timeline(vec![vec![Clip::new(a.id, 0.0, 10.0, 0.0)]]);
            tl.format = Some(Delivery::new(16, 9, Fit::Contain));
            let opts = ExportOptions {
                scaler: scaler.map(Into::into),
                ..ExportOptions::default()
            };
            let p = RenderPlan::at(&tl, std::slice::from_ref(&a), &opts, 1.0, CompositeColorPolicy::FixedBt601).unwrap();
            let caps = GpuCaps {
                scalers: ScalerKinds::ALL,
                ..GpuCaps::A0
            };
            p.reasons(&caps, (16, 9))
                .into_iter()
                .find(|r| matches!(r, Unsupported::Shrink(..)))
        };
        // (1280x720 into 16x9 is 80:1, past every name's limit.)
        for (scaler, limit) in [
            (None, 63),
            (Some("bicubic"), 63),
            (Some("bilinear"), 63),
            (Some("neighbor"), 63),
            (Some("lanczos"), 42),
            (Some("spline"), 18),
        ] {
            let why = refusal(scaler).unwrap_or_else(|| panic!("{scaler:?} is not refused"));
            assert!(
                matches!(why, Unsupported::Shrink(0, _, _, l) if l == limit),
                "{scaler:?}: {why:?}"
            );
            let text = why.to_string();
            assert!(text.contains(&format!("steeper than the {limit}:1")), "{scaler:?}: {text}");
            for other in [63, 42, 18].into_iter().filter(|o| *o != limit) {
                assert!(!text.contains(&format!("{other}:1")), "{scaler:?}: {text}");
            }
        }
    }

    #[test]
    fn a_still_that_moves_is_ffmpegs_where_the_delivery_names_a_scaler_its_chains_were_not_measured_with() {
        let a = asset("a", 10.0, vec![video(640, 360)]);
        let tl = timeline(vec![vec![Clip::new(a.id, 0.0, 10.0, 0.0)]]);
        let plan_with = |scaler: Option<&str>| {
            let opts = ExportOptions {
                scaler: scaler.map(Into::into),
                ..ExportOptions::default()
            };
            RenderPlan::at(&tl, std::slice::from_ref(&a), &opts, 1.0, CompositeColorPolicy::FixedBt601).unwrap()
        };
        // The chain's text names `bicubic` or nothing; every other name is `scale=...:flags=<name>` in a chain nobody measured.
        for (scaler, other) in [
            (None, false),
            (Some("bicubic"), false),
            (Some("Bicubic"), false),
            (Some("lanczos"), true),
            (Some("spline"), true),
            (Some("neighbor"), true),
            (Some("bilinear"), true),
        ] {
            let p = plan_with(scaler);
            assert_eq!(p.scaler_named_other(), other, "{scaler:?}");
            assert_eq!(p.scaler_named_bicubic(), scaler == Some("bicubic"), "{scaler:?}");
        }
        // (A footage layer is no still: the chain is the layer's own and the plan has none to give it.)
        let p = plan_with(Some("lanczos"));
        assert!(p.layers.iter().all(|l| p.still_chain(l, (640, 360)).is_none()));
    }

    /// The angles `rotate` turns a picture by whole samples for: the quarters whose integer sine and cosine are exact (measured on FFmpeg
    /// 6.1.1 and 9.0.2 for -1310 to 1310 quarters, `tests/motion_parity.rs`), the rest refused, whatever their float angle says.
    #[test]
    fn a_quarter_turn_is_an_angle_rotates_integer_sine_comes_out_exact_for() {
        let deg = |d: f64| quarter_turn(d.to_radians());
        // The set: -47, -46, -43 to 40, 43 and 44 quarters.
        let exact = |q: i32| (-47..=-46).contains(&q) || (-43..=40).contains(&q) || (43..=44).contains(&q);
        for q in -700..=700 {
            let want = exact(q).then_some(q.rem_euclid(4) as u8);
            assert_eq!(deg(f64::from(q) * 90.0), want, "{q} quarters");
        }
        for (d, turn) in [
            (90.0, 1),
            (180.0, 2),
            (270.0, 3),
            (-90.0, 3),
            (360.0, 0),
            (450.0, 1),
            (-450.0, 3),
            (3600.0, 0),
        ] {
            assert_eq!(deg(d), Some(turn), "{d}");
        }
        // A hair off a quarter (within `FLT_EPSILON` radians of one: the fast path's window) is that quarter; 90.00001 degrees is not one.
        let rad = |q: f64, off: f64| q * std::f64::consts::FRAC_PI_2 + off;
        for (q, off, turn) in [
            (1.0, 1e-7, 1),
            (1.0, -1e-7, 1),
            (2.0, 1.1e-7, 2),
            (3.0, -1.1e-7, 3),
            (0.0, 1e-7, 0),
            (0.0, -1e-7, 0),
            (5.0, 1e-7, 1),
            (-1.0, -1e-7, 3),
        ] {
            assert_eq!(quarter_turn(rad(q, off)), Some(turn), "{q} quarters {off:+e} rad");
        }
        for (q, off) in [(1.0, 1.3e-7), (1.0, -1.3e-7), (0.0, 2e-7), (2.0, 1e-6), (5.0, 1.745e-7)] {
            assert_eq!(quarter_turn(rad(q, off)), None, "{q} quarters {off:+e} rad");
        }
        for (d, turn) in [
            (90.000_000_01, 1),
            (89.999_999_99, 1),
            (-90.000_000_01, 3),
            (180.000_000_01, 2),
            (450.000_000_01, 1),
        ] {
            assert_eq!(deg(d), Some(turn), "{d}");
        }
        for d in [91.0, 89.99, 90.00001, 45.0, 0.001, 12.0, -30.0, 359.5, 3690.0] {
            assert_eq!(deg(d), None, "{d}");
        }
        // Too large for the filter's `int` angle, or not a number: never a quarter turn (and no overflow looking).
        for d in [
            1e300,
            -1e300,
            f64::MAX,
            f64::MIN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            1e18,
            1e7,
            4e9,
            90.0 * 2f64.powi(40),
            90.0 * 2f64.powi(40) + 0.0625,
            2f64.powi(60),
            58_000.0,
        ] {
            assert_eq!(deg(d), None, "{d}");
        }
        assert!(is_quarter_turn(std::f64::consts::PI) && !is_quarter_turn(1.0));
    }

    /// A layer below full opacity is turned in RGBA and converted to 4:2:0 after the turn, so a quarter or three quarters of a turn
    /// pairs other chroma samples than the compositor's round trip (which comes before the turn) does: refused, as a rotation.
    /// Upright, half-turned and opaque layers are not.
    #[test]
    fn a_turn_of_a_layer_below_full_opacity_is_refused_and_the_upright_layer_is_not() {
        let caps = GpuCaps {
            motion: true,
            ..GpuCaps::A0
        };
        let turned = |angle: f64, opacity: f64| {
            let plan = fading_plan(
                |c| {
                    c.transform.rotation = angle;
                    c.transform.opacity = opacity;
                },
                0.5,
            );
            let why = plan.reasons(&caps, plan.size(u32::MAX));
            why.into_iter()
                .filter(|u| u.kind() == "rotation")
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
        };
        for angle in [90.0, 180.0, 270.0, -90.0, -180.0, 450.0] {
            let why = turned(angle, 0.5);
            assert!(why.len() == 1 && why[0].contains("below full opacity"), "{angle}: {why:?}");
            assert!(turned(angle, 1.0).is_empty(), "{angle}: an opaque layer is drawn");
        }
        for angle in [0.0, 360.0] {
            assert!(turned(angle, 0.5).is_empty(), "{angle}: {:?}", turned(angle, 0.5));
        }
        // (A turn that is not a quarter has its own cause and only that one.)
        let why = turned(12.0, 0.5);
        assert!(why.len() == 1 && !why[0].contains("below full opacity"), "{why:?}");
    }

    #[test]
    fn a_rotation_that_is_not_a_proven_quarter_turn_is_refused_on_every_layer() {
        let caps = GpuCaps {
            motion: true,
            ..GpuCaps::A0
        };
        let rotation = |angle: f64| {
            let plan = fading_plan(|c| c.transform.rotation = angle, 0.5);
            let why = plan.reasons(&caps, plan.size(u32::MAX));
            assert_eq!(plan.gpu_supported(), why.is_empty(), "{angle}: {why:?}");
            why.into_iter().filter(|u| u.kind() == "rotation").count()
        };
        for angle in [90.0, 180.0, 270.0, -90.0, 360.0, 3600.0, 36.0 * 90.0, 43.0 * 90.0] {
            assert_eq!(rotation(angle), 0, "{angle}");
        }
        for angle in [
            12.0,
            -30.0,
            45.0,
            89.5,
            91.0,
            90.00001,
            41.0 * 90.0,
            42.0 * 90.0,
            400.0 * 90.0,
            1e7,
            1e300,
            f64::MAX,
        ] {
            assert_eq!(rotation(angle), 1, "{angle}");
        }
        // Blended too (the blend's own cause is named as well).
        let plan = fading_plan(
            |c| {
                c.blend_mode = BlendMode::Multiply;
                c.transform.rotation = 12.0;
            },
            0.5,
        );
        let kinds: Vec<_> = plan
            .reasons(&GpuCaps::A0, plan.size(u32::MAX))
            .iter()
            .map(Unsupported::kind)
            .collect();
        assert!(kinds.contains(&"rotation") && kinds.contains(&"blend_mode"), "{kinds:?}");
    }
}
