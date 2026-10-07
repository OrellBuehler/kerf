//! What one rendered frame is made of: the canvas and the ordered video layers
//! visible at a timeline time, with everything already resolved.
//!
//! Two renderers draw a frame of the cut — the FFmpeg filter graph
//! (`engine::cli::build_still_args`, and the export / playback graphs) and the
//! wgpu compositor in `kerf-gpu` — and they must agree on *which* clips are on
//! screen, *where in the source* each one is, and *what pose* it holds. That
//! agreement is this module: pure, unit-tested, and consumed by both, so
//! neither re-derives it. `build_still_args` takes its active-clip list from
//! [`active_video_clips`], the same function [`RenderPlan::at`] is built on.
//!
//! The plan is deliberately a *description*, not a renderer. It carries only
//! what a compositor needs for the features the GPU path renders today (layers
//! in track order, resolved source time, sampled [`Transform`], [`Color`], the
//! delivery canvas and fit), and [`RenderPlan::gpu_supported`] says "no" — with
//! the reasons — for anything it does not carry, so that frame goes through
//! FFmpeg instead of being drawn wrong.

use uuid::Uuid;

use crate::engine::{render_geometry, ExportOptions, Fit};
use crate::error::{Error, Result};
use crate::layer_geometry::LayerGeometry;
use crate::model::{pix_fmt_layout, Asset, Clip, Color, PixLayout, StreamInfo, StreamKind, Timeline, Transform};

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
    /// Index of the track in the timeline the clip sits on (bottom first).
    pub track: usize,
}

impl ActiveClip<'_> {
    /// The clip's transform sampled at this instant, so a keyframed clip shows
    /// its pose rather than the static one.
    pub(crate) fn transform(&self) -> Transform {
        self.clip.transform_at(self.local_time)
    }
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
    for (track_index, track) in timeline.tracks.iter().enumerate() {
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
            let off = (t - clip.timeline_start) * clip.speed_mag();
            let raw = if clip.is_reversed() {
                clip.source_out - off
            } else {
                clip.source_in + off
            };
            let dur = asset_of(clip.asset_id).map(|a| a.duration).unwrap_or(clip.source_out);
            active.push(ActiveClip {
                clip,
                source_time: raw.clamp(0.0, dur.max(0.0)),
                local_time: (t - clip.timeline_start).max(0.0),
                track: track_index,
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
}

/// The steepest shrink of one scale stage the compositor claims to match: swscale's
/// vertical scaler on x86 is not bit-exact with the C arithmetic the compositor
/// follows, and the difference grows with the number of input samples per output
/// one — one level up to about 4:1, two to three up to 20:1, up to five at 40:1
/// (`the_scaler_matches_ffmpegs_scale_plane_by_plane`). Past it the figure is not
/// measured (a 58:1 shrink of a 4K test pattern reads 16 levels off in RGB), so the
/// frame goes through FFmpeg.
pub const MAX_SHRINK: u32 = 40;

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

    /// An RGB-family picture (see [`PixLayout::Rgb`]).
    pub fn is_rgb(&self) -> bool {
        self.layout() == Some(PixLayout::Rgb)
    }

    fn of(s: &StreamInfo) -> Option<Self> {
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
    /// Seconds from the clip's start; keyframes were sampled here.
    pub clip_time: f64,
    pub stream: PlanStream,
    /// The transform **sampled** at this instant.
    pub transform: Transform,
    pub color: Color,
}

/// The ordered video layers visible at one timeline time, on their canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderPlan {
    /// The (non-negative) timeline time the plan is for.
    pub time: f64,
    pub canvas: PlanCanvas,
    /// Bottom layer first — the order they are composited in.
    pub layers: Vec<PlanLayer>,
    unsupported: Vec<String>,
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
fn composite_matrix(layers: &[PlanLayer], policy: CompositeColorPolicy, unsupported: &mut Vec<String>) -> YuvMatrix {
    if policy == CompositeColorPolicy::FixedBt601 {
        return YuvMatrix::Bt601;
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
            None => unsupported.push(format!(
                "layer {n}: its colour matrix is unknown, and FFmpeg's composite takes its matrix from the layers' tags"
            )),
            Some(m) => match shared {
                None => shared = Some(m),
                Some(first) if first != m => mixed = true,
                Some(_) => {}
            },
        }
    }
    if mixed {
        unsupported.push(
            "layers tagged with different YCbCr matrices: FFmpeg converts them into the bottom layer's, which is not reproduced"
                .to_string(),
        );
    }
    let matrix = shared.unwrap_or(YuvMatrix::Bt601);
    if rgb && matrix != YuvMatrix::Bt601 {
        unsupported.push(format!(
            "an RGB picture in a {matrix:?} stack: FFmpeg converts it with the stack's matrix, which is not reproduced"
        ));
    }
    matrix
}

impl RenderPlan {
    /// The plan for timeline time `t`: what `timeline_frame` / `export_still`
    /// would draw. Like them it renders the cut as [`Timeline::for_render`]
    /// sees it, so a muted or solo-shadowed track is as absent here as there.
    ///
    /// Errors the way the still does: a clip whose asset is not in `assets`.
    pub fn at(
        timeline: &Timeline,
        assets: &[Asset],
        opts: &ExportOptions,
        t: f64,
        color: CompositeColorPolicy,
    ) -> Result<RenderPlan> {
        let rendered = timeline.for_render();
        let geom = render_geometry(&rendered, assets, opts);
        let t = t.max(0.0);
        let asset_of = |id| assets.iter().find(|a: &&Asset| a.id == id);

        let mut unsupported: Vec<String> = Vec::new();
        let mut layers = Vec::new();
        for (n, ac) in active_video_clips(&rendered, assets, t).iter().enumerate() {
            let clip = ac.clip;
            let asset = asset_of(clip.asset_id).ok_or(Error::AssetNotFound(clip.asset_id))?;
            let label = format!("layer {n} ({})", asset.name);
            let Some(stream) = asset
                .streams
                .iter()
                .find(|s| s.kind == StreamKind::Video)
                .and_then(PlanStream::of)
            else {
                unsupported.push(format!("{label}: the source has no video picture of a known size"));
                continue;
            };
            if asset.hdr().is_some() {
                unsupported.push(format!("{label}: HDR footage needs tone mapping"));
            }
            // Alpha is composited by FFmpeg (a ProRes 4444 title, a VP9 or FFV1
            // clip with transparency, a GIF); the compositor draws opaque 4:2:0
            // and would flatten it onto black without a word. The probed pixel
            // format must be on the allow-list of known-opaque ones — a name a
            // deny-list never thought of (`ayuv`, `vuya`, the `rgb32` aliases) is
            // refused too. `None` (never recorded) is the decoder's to find out.
            if let Some(fmt) = stream.pix_fmt.as_deref() {
                if crate::model::pix_fmt_has_alpha(fmt) {
                    unsupported.push(format!("{label}: the picture has an alpha channel ({fmt})"));
                } else if pix_fmt_layout(fmt).is_none() {
                    unsupported.push(format!(
                        "{label}: the pixel format {fmt} is not one known to be opaque (it may carry alpha)"
                    ));
                }
            }
            // Below full opacity FFmpeg takes the layer through RGB and back (see
            // `kerf-gpu`'s `roundtrip`), out of YCbCr with the picture's own matrix,
            // which has to be known for the arithmetic to be reproduced.
            let opacity = ac.transform().opacity;
            if opacity < 1.0 && stream.matrix().is_none() {
                unsupported.push(format!(
                    "{label}: opacity below 1 on a picture whose colour matrix is {}",
                    match (stream.pix_fmt.as_deref(), stream.color_space.as_deref()) {
                        (None, _) => "unknown (probed before the pixel format and tags were recorded)".to_string(),
                        (Some(_), Some(tag)) => format!("`{tag}`, which the compositor has no coefficients for"),
                        (Some(_), None) => "unknown".to_string(),
                    }
                ));
            }
            if !clip.effects.is_empty() {
                unsupported.push(format!("{label}: video effects"));
            }
            if clip.mask.is_some() {
                unsupported.push(format!("{label}: mask"));
            }
            if clip.reframe.is_some() || asset.projection().is_some() {
                unsupported.push(format!("{label}: 360 reframe"));
            }
            // The still ignores fades and transitions (it shows the frame each
            // visible clip contributes); the export and the streamed playback do
            // not. A frame inside one is a frame the GPU cannot yet draw like
            // either, so it is FFmpeg's.
            if clip.fade_in > 0.0 && ac.local_time < clip.fade_in {
                unsupported.push(format!("{label}: inside its fade-in"));
            }
            if clip.fade_out > 0.0 && t >= clip.timeline_end() - clip.fade_out {
                unsupported.push(format!("{label}: inside its fade-out"));
            }
            layers.push(PlanLayer {
                clip_id: clip.id,
                asset_id: clip.asset_id,
                track: ac.track,
                path: asset.path.clone(),
                is_image: asset.is_image(),
                source_time: ac.source_time,
                clip_time: ac.local_time,
                stream,
                transform: ac.transform(),
                color: clip.color,
            });
        }

        // A transition changes what the neighbours of the cut look like for its
        // whole length, including the clip that is *not* the one starting.
        for track in rendered.tracks.iter().filter(|tr| tr.kind == StreamKind::Video) {
            for clip in &track.clips {
                let Some(tr) = clip.transition_in else { continue };
                let d = tr.duration.max(0.0);
                if d > 0.0 && t >= clip.timeline_start - d / 2.0 && t < clip.timeline_start + d {
                    unsupported.push(format!("transition ({}) in progress", tr.kind.as_str()));
                }
            }
        }
        if rendered.overlays.iter().any(|o| t >= o.start && t < o.end) {
            unsupported.push("a text overlay is live".to_string());
        }
        // The compositor implements swscale's default (bicubic) only.
        if let Some(s) = geom.scaler.as_deref().filter(|s| !s.eq_ignore_ascii_case("bicubic")) {
            unsupported.push(format!("scaler '{s}'"));
        }

        let matrix = composite_matrix(&layers, color, &mut unsupported);

        Ok(RenderPlan {
            time: t,
            canvas: PlanCanvas {
                width: geom.width,
                height: geom.height,
                fit: geom.fit,
                scaler: geom.scaler,
                matrix,
            },
            layers,
            unsupported,
        })
    }

    /// Whether the compositor draws this frame exactly, **judged without the render
    /// size**. `false` means "render it through FFmpeg"; [`RenderPlan::unsupported_reasons`]
    /// says why. A `true` is necessary, not sufficient: what depends on the size the
    /// frame is rendered at (an enlargement of a picture whose chroma is not 4:2:0, a
    /// translucent layer of odd size, a crop that leaves nothing) is
    /// [`RenderPlan::gpu_supported_at`]'s.
    pub fn gpu_supported(&self) -> bool {
        self.unsupported.is_empty()
    }

    /// Why the frame cannot be drawn at `size` ([`RenderPlan::size`]): the size-free
    /// reasons of [`RenderPlan::unsupported_reasons`] plus those the geometry at this
    /// size adds. Pure; ask it before decoding anything.
    pub fn unsupported_reasons_at(&self, size: (u32, u32)) -> Vec<String> {
        let mut reasons = self.unsupported.clone();
        for (n, layer) in self.layers.iter().enumerate() {
            let src = (layer.stream.width, layer.stream.height);
            let geom = match LayerGeometry::resolve(src, size, self.canvas.fit, &layer.transform) {
                Ok(g) => g,
                Err(e) => {
                    reasons.push(format!("layer {n}: {e}"));
                    continue;
                }
            };
            // FFmpeg scales a picture in the format it has; the compositor scales
            // the 8-bit 4:2:0 a decode reduces it to. Shrinking agrees (the chroma
            // that is thrown away is thrown away either way); enlarging does not —
            // FFmpeg interpolates real chroma where the compositor interpolates
            // chroma that was already averaged away (a 2x enlargement of 4:4:4 is 27
            // levels off, of RGB 69). An unrecorded format is not known to be 4:2:0.
            if let Some(stage) = geom
                .stages
                .iter()
                .find(|s| s.src.w > MAX_SHRINK * s.scaled.0 || s.src.h > MAX_SHRINK * s.scaled.1)
            {
                reasons.push(format!(
                    "layer {n}: shrinks a {}x{} picture to {}x{}, steeper than the {MAX_SHRINK}:1 the scaler comparison covers",
                    stage.src.w, stage.src.h, stage.scaled.0, stage.scaled.1
                ));
            }
            let enlarging = geom.stages.iter().find(|s| s.scaled.0 > s.src.w || s.scaled.1 > s.src.h);
            if let Some(stage) = enlarging.filter(|_| !matches!(layer.stream.layout(), Some(PixLayout::Yuv420 | PixLayout::Gray)))
            {
                reasons.push(format!(
                    "layer {n}: enlarges a {}x{} picture to {}x{}, and its format ({}) is not 8/10-bit 4:2:0 or gray, which FFmpeg scales natively",
                    stage.src.w,
                    stage.src.h,
                    stage.scaled.0,
                    stage.scaled.1,
                    layer.stream.pix_fmt.as_deref().unwrap_or("not recorded")
                ));
            }
        }
        reasons
    }

    /// Whether the compositor draws this frame exactly when rendered at `size`.
    pub fn gpu_supported_at(&self, size: (u32, u32)) -> bool {
        self.unsupported_reasons_at(size).is_empty()
    }

    /// Why the frame cannot be drawn on the GPU yet — empty when it can.
    pub fn unsupported_reasons(&self) -> &[String] {
        &self.unsupported
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
        (p.gpu_supported(), p.unsupported_reasons().to_vec())
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
        // Enlarging a picture FFmpeg scales natively in another format is refused;
        // 4:2:0 and gray are not, and shrinking is not, whatever the format.
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
            ("yuv444p", 0.5, false),
            ("rgb24", 0.5, false),
        ] {
            let p = with_fmt(Some(fmt), scale, 1.0);
            let why = p.unsupported_reasons_at(p.size(u32::MAX));
            assert_eq!(!why.is_empty(), refused, "{fmt} x{scale}: {why:?}");
        }
        // A format that was never recorded is not known to be 4:2:0.
        let p = with_fmt(None, 2.0, 1.0);
        assert!(!p.gpu_supported_at(p.size(u32::MAX)));
        // The size decides: the same 640x360 source into a 1920x1080 frame is an
        // enlargement at full size and a plain copy at preview width 640.
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
