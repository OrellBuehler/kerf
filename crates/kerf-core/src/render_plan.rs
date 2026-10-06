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
use crate::model::{Asset, Clip, Color, StreamInfo, StreamKind, Timeline, Transform};

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
/// display. A plan carries one, because FFmpeg composites in YUV *without*
/// converting between its layers' own matrices and converts the **result**
/// once — so there is exactly one matrix per frame, not one per layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvMatrix {
    /// BT.601 (Kr 0.299, Kb 0.114).
    Bt601,
    /// BT.709 (Kr 0.2126, Kb 0.0722).
    Bt709,
}

impl YuvMatrix {
    /// `(Kr, Kb)`, the luma weights of red and blue.
    pub fn weights(self) -> (f64, f64) {
        match self {
            YuvMatrix::Bt601 => (0.299, 0.114),
            YuvMatrix::Bt709 => (0.2126, 0.0722),
        }
    }
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
    /// The matrix the composited frame is converted with. **BT.601 limited
    /// range**, because that is what FFmpeg does today and parity is the point:
    /// the still graph hands the PNG / JPEG encoder a composited `yuv420p` frame
    /// whose colorspace is unspecified (the black base it is drawn onto carries
    /// none), and swscale reads "unspecified" as BT.601 — measured, for
    /// BT.709-tagged, BT.601-tagged and untagged sources alike. An export is
    /// not converted at all, so a player shows it as BT.709 (HD); the two
    /// disagree by a few levels on saturated colour. Moving this to BT.709 is a
    /// decision for the day the GPU path *replaces* the FFmpeg preview.
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
}

impl PlanStream {
    fn of(s: &StreamInfo) -> Option<Self> {
        Some(Self {
            width: s.width.filter(|w| *w > 0)?,
            height: s.height.filter(|h| *h > 0)?,
            rotation: s.rotation,
            fps: s.fps,
            codec: s.codec.clone(),
            color_transfer: s.color_transfer.clone(),
            color_primaries: s.color_primaries.clone(),
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

impl RenderPlan {
    /// The plan for timeline time `t`: what `timeline_frame` / `export_still`
    /// would draw. Like them it renders the cut as [`Timeline::for_render`]
    /// sees it, so a muted or solo-shadowed track is as absent here as there.
    ///
    /// Errors the way the still does: a clip whose asset is not in `assets`.
    pub fn at(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, t: f64) -> Result<RenderPlan> {
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

        Ok(RenderPlan {
            time: t,
            canvas: PlanCanvas {
                width: geom.width,
                height: geom.height,
                fit: geom.fit,
                scaler: geom.scaler,
                matrix: YuvMatrix::Bt601,
            },
            layers,
            unsupported,
        })
    }

    /// Whether the compositor draws this frame exactly. `false` means "render it
    /// through FFmpeg"; [`RenderPlan::unsupported_reasons`] says why.
    pub fn gpu_supported(&self) -> bool {
        self.unsupported.is_empty()
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
        RenderPlan::at(tl, assets, &ExportOptions::default(), t).unwrap()
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
        let p = RenderPlan::at(&tl, std::slice::from_ref(&a), &opts, 1.0).unwrap();
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
            RenderPlan::at(&tl, &[], &ExportOptions::default(), 0.5),
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
        ];
        for (name, edit) in cases {
            let (ok, why) = supported_with(edit);
            assert!(!ok && !why.is_empty(), "{name} must not be drawn on the GPU");
        }
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
        let p = RenderPlan::at(&tl, std::slice::from_ref(&a), &with("bicubic"), 1.0).unwrap();
        assert!(p.gpu_supported());
        let p = RenderPlan::at(&tl, std::slice::from_ref(&a), &with("lanczos"), 1.0).unwrap();
        assert!(!p.gpu_supported());
    }
}
