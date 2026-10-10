//! The composited still inside a fade, a dip, a dissolve, a slide or a push.
//!
//! The still decodes one frame per visible clip and has no timeline clock, so the export's
//! time-domain steps (`setpts`, `fps`, `fade`, the overlay's travel expression) have no
//! counterpart in it. This module supplies the three things that make a paused frame show
//! what the file shows at the same moment, each decided by the same numbers the export
//! graph prints ([`ClipTiming`]) and evaluated at the output frame the still's time falls
//! in ([`Rational::frame_containing`]):
//!
//! * **fades** — the export's own `fade` filters, run on a frame whose timestamp is that
//!   output frame in the `1/fps` time base the export's `fade` reads (a `fade` counts
//!   frames, see [`FadeStep::progress_at_frame`]), then put back to zero for the overlay;
//! * **tails** — an outgoing clip keeps drawing past its end, under the clip that replaces
//!   it, from the source it borrows;
//! * **travel** — a slide or push moves the clip, evaluated at the graph's own frame time.
//!
//! A layer with none of them gets an empty [`StillFades`] and no travel, so the argv of
//! every still that is not inside a transition is what it always was.

use crate::clip_timing::{transition_fx, ClipTiming, FadeStep, FadeTint, Rational};
use crate::model::{Asset, Timeline};
use crate::render_plan::{active_video_clips, active_video_clips_with_tails, ActiveClip};

use super::fade_filter;

/// The fades of one layer that are still changing its picture at the still's frame, and
/// how to run them on one frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct StillFades {
    steps: Vec<FadeStep>,
    /// The output frame the fades are counted in, and the rate of the `fps` filter whose
    /// time base the export's `fade` reads. `None` with no steps.
    moment: Option<(i64, Rational)>,
    /// The source has an alpha channel of its own, which the picture keeps to the overlay.
    alpha_source: bool,
}

impl StillFades {
    /// No fade is changing the picture: the chain is the one a still always had.
    pub(super) fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The pixel format the export's chain ends in when it needs no alpha plane of its own
    /// (`format=` after the fades). It decides what the fades run on: `fade` is flexible
    /// about its format, and left to negotiation it takes the `yuva420p` the overlay asks
    /// for, where a limited-range black is 0 and not 16 — a fade to black in the still
    /// reached a darker black than the file's, up to 17 levels off on the way there.
    pub(super) fn terminal_format(&self) -> &'static str {
        if self.alpha_source {
            "yuva420p"
        } else {
            "yuv420p"
        }
    }

    /// A dissolve's ramp is the alpha plane, which the chain has to have established.
    pub(super) fn needs_alpha(&self) -> bool {
        self.steps.iter().any(|s| s.tint == FadeTint::Alpha)
    }

    /// The filters to run at the end of the chain, where the export's fades are: the frame
    /// is stamped as output frame `k` of a `1/fps` stream, faded by the export's own
    /// `fade` filters, and moved back to zero (the overlay shows a secondary frame only
    /// from its timestamp on, and the canvas it lands on starts at zero).
    pub(super) fn filters(&self) -> Vec<String> {
        let Some((frame, fps)) = self.moment.filter(|_| !self.steps.is_empty()) else {
            return Vec::new();
        };
        let mut out = vec![format!("settb=expr={}/{}", fps.den, fps.num), format!("setpts={frame}")];
        out.extend(self.steps.iter().map(fade_filter));
        out.push("setpts=PTS-STARTPTS".to_string());
        out
    }
}

/// One video layer of a still: the clip and where in its source the frame is, plus what a
/// transition does to it at this frame.
pub(super) struct StillLayer<'a> {
    pub active: ActiveClip<'a>,
    pub fades: StillFades,
    /// The travel of a slide or push in frame widths and heights; `(0, 0)` for a clip that
    /// is not moving.
    pub motion: (f64, f64),
}

/// The layers of the still of `timeline` (the rendered one) at `t`, in composite order.
///
/// An outgoing clip playing on under a transition is a layer for as long as its tail lasts.
/// `fps` is the delivery rate; one FFmpeg would refuse leaves the still as it was, drawing
/// the clips of their own span and no transition.
pub(super) fn still_layers<'a>(timeline: &'a Timeline, assets: &[Asset], fps: f64, t: f64) -> Vec<StillLayer<'a>> {
    let (Some(canvas), Some(pick)) = (Rational::from_fps(fps), Rational::from_fps_filter(fps)) else {
        return active_video_clips(timeline, assets, t)
            .into_iter()
            .map(|active| StillLayer {
                active,
                fades: StillFades::default(),
                motion: (0.0, 0.0),
            })
            .collect();
    };
    let fx = transition_fx(timeline, assets);
    let tails: Vec<f64> = fx.iter().map(|f| f.tail).collect();
    let frame = canvas.frame_containing(t);
    let graph_time = canvas.frame_time(frame);
    let frame = i64::try_from(frame).unwrap_or(i64::MAX);
    active_video_clips_with_tails(timeline, assets, t, &tails)
        .into_iter()
        .map(|active| {
            let timing = ClipTiming::new(active.clip, &fx[active.flat]);
            // Which fades are in progress is the still plan's own question (`LayerFx::in_progress`,
            // on the canvas rate), so the frames the plan refuses are the frames drawn here.
            let steps: Vec<FadeStep> = timing
                .fades()
                .into_iter()
                .filter(|s| s.progress_at_frame(frame, canvas.num, canvas.den) < 1.0)
                .collect();
            // The graph's time for the frame, not `t`: the overlay truncates the travel to whole
            // pixels, and a `t` an ulp off the frame's own time can land two pixels away.
            let motion = timing
                .motion_keys()
                .map_or((0.0, 0.0), |keys| keys.at(graph_time - active.clip.timeline_start));
            StillLayer {
                active,
                fades: StillFades {
                    moment: (!steps.is_empty()).then_some((frame, pick)),
                    steps,
                    alpha_source: fx[active.flat].alpha,
                },
                motion,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{make_clip, single, test_asset, video_stream};
    use crate::model::{Transition, TransitionKind};

    fn asset() -> Asset {
        test_asset(vec![video_stream(1920, 1080, 24.0)])
    }

    fn layers_of<'a>(tl: &'a Timeline, asset: &Asset, t: f64) -> Vec<StillLayer<'a>> {
        still_layers(tl, std::slice::from_ref(asset), 24.0, t)
    }

    /// Two adjacent 10 s clips of `asset` (source 0..10 and 20..30), the second entering
    /// through `kind` over `secs`.
    fn pair(asset: &Asset, kind: TransitionKind, secs: f64) -> Timeline {
        let a = make_clip(asset.id, 0.0, 10.0, 0.0);
        let mut b = make_clip(asset.id, 20.0, 30.0, 10.0);
        b.transition_in = Some(Transition { kind, duration: secs });
        single(vec![a, b])
    }

    #[test]
    fn a_plain_cut_has_no_fades_no_travel_and_the_layers_active_video_clips_returns() {
        let asset = asset();
        let tl = single(vec![
            make_clip(asset.id, 0.0, 10.0, 0.0),
            make_clip(asset.id, 20.0, 30.0, 10.0),
        ]);
        for t in [0.0, 3.3, 9.99, 10.0, 17.5] {
            let layers = layers_of(&tl, &asset, t);
            let plain = active_video_clips(&tl, std::slice::from_ref(&asset), t);
            assert_eq!(layers.len(), plain.len(), "t = {t}");
            for (l, p) in layers.iter().zip(&plain) {
                assert_eq!((l.active.clip.id, l.active.source_time), (p.clip.id, p.source_time));
                assert_eq!((l.fades.clone(), l.motion), (StillFades::default(), (0.0, 0.0)));
                assert!(l.fades.filters().is_empty() && !l.fades.needs_alpha());
            }
        }
    }

    #[test]
    fn a_dissolve_draws_the_outgoing_clip_on_its_tail_under_the_incoming_clips_ramp() {
        let asset = asset();
        let tl = pair(&asset, TransitionKind::Crossfade, 1.0);
        // Frame 252 of 24 fps is 10.5 s: half way through the one second dissolve.
        let layers = layers_of(&tl, &asset, 10.5);
        assert_eq!(layers.len(), 2);
        let (out, inc) = (&layers[0], &layers[1]);
        assert_eq!(out.active.clip.timeline_start, 0.0);
        assert!(
            (out.active.source_time - 10.5).abs() < 1e-9,
            "the tail borrows the handle past the cut"
        );
        assert!(out.fades.filters().is_empty());
        assert!(inc.fades.needs_alpha());
        assert_eq!(
            inc.fades.filters(),
            vec![
                "settb=expr=1/24".to_string(),
                "setpts=252".to_string(),
                "fade=t=in:st=10:d=1:alpha=1".to_string(),
                "setpts=PTS-STARTPTS".to_string(),
            ]
        );
        // Before the dissolve there is the outgoing clip alone; after it, the incoming one.
        assert_eq!(layers_of(&tl, &asset, 9.9).len(), 1);
        let after = layers_of(&tl, &asset, 11.0);
        assert_eq!(after.len(), 1);
        assert!(after[0].fades.filters().is_empty(), "a finished ramp is not drawn");
        assert_eq!(layers_of(&tl, &asset, 11.5).len(), 1);
    }

    #[test]
    fn a_dip_fades_the_clip_out_before_the_cut_and_the_next_one_in_after_it() {
        let asset = asset();
        let tl = pair(&asset, TransitionKind::DipToBlack, 2.0);
        let before = layers_of(&tl, &asset, 9.5);
        assert_eq!(before.len(), 1);
        assert_eq!(
            before[0].fades.filters()[2],
            "fade=t=out:st=9:d=1",
            "the outgoing side fades over the second before the cut"
        );
        let after = layers_of(&tl, &asset, 10.25);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].fades.filters()[2], "fade=t=in:st=10:d=1");
        assert!(!after[0].fades.needs_alpha(), "a dip fades the picture itself");
        assert!(layers_of(&tl, &asset, 5.0)[0].fades.filters().is_empty());
        let white = pair(&asset, TransitionKind::DipToWhite, 2.0);
        assert_eq!(
            layers_of(&white, &asset, 10.25)[0].fades.filters()[2],
            "fade=t=in:st=10:d=1:c=white"
        );
    }

    #[test]
    fn a_clips_own_fades_are_drawn_until_they_are_done() {
        let asset = asset();
        let mut clip = make_clip(asset.id, 0.0, 4.0, 5.0);
        clip.fade_in = 0.5;
        clip.fade_out = 1.0;
        let tl = single(vec![clip]);
        let at = |t: f64| layers_of(&tl, &asset, t)[0].fades.filters();
        assert_eq!(at(5.0)[2], "fade=t=in:st=5:d=0.5");
        assert_eq!(at(5.25)[1], "setpts=126");
        assert!(at(5.5).is_empty(), "the fade-in is over at its last frame");
        assert!(at(7.0).is_empty());
        assert_eq!(at(8.5)[2], "fade=t=out:st=8:d=1");
    }

    #[test]
    fn a_slide_moves_the_incoming_clip_and_a_push_carries_the_outgoing_one_out() {
        let asset = asset();
        let slide = pair(&asset, TransitionKind::SlideLeft, 2.0);
        let layers = layers_of(&slide, &asset, 11.0);
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].motion, (0.0, 0.0), "a slide covers the outgoing clip where it sits");
        assert!((layers[1].motion.0 - 0.5).abs() < 1e-9 && layers[1].motion.1 == 0.0);
        assert!(layers[1].fades.filters().is_empty(), "a slide is not a fade");
        let push = pair(&asset, TransitionKind::PushUp, 2.0);
        let layers = layers_of(&push, &asset, 11.0);
        assert!((layers[0].motion.1 + 0.5).abs() < 1e-9);
        assert!((layers[1].motion.1 - 0.5).abs() < 1e-9);
        // Once the travel is done, nothing moves and the outgoing clip is gone.
        let done = layers_of(&push, &asset, 12.5);
        assert_eq!((done.len(), done[0].motion), (1, (0.0, 0.0)));
    }

    #[test]
    fn a_frame_rate_ffmpeg_refuses_leaves_the_still_without_transitions() {
        let asset = asset();
        let tl = pair(&asset, TransitionKind::Crossfade, 1.0);
        let layers = still_layers(&tl, std::slice::from_ref(&asset), f64::NAN, 10.5);
        assert_eq!(layers.len(), 1);
        assert!(layers[0].fades.filters().is_empty());
    }

    #[test]
    fn the_frame_a_still_is_in_decides_how_far_into_the_fade_it_is() {
        let asset = asset();
        let tl = pair(&asset, TransitionKind::Crossfade, 1.0);
        let setpts = |t: f64| layers_of(&tl, &asset, t)[1].fades.filters()[1].clone();
        // Frame 240 starts the ramp; a time inside a frame is the frame it falls in.
        assert_eq!(setpts(10.0), "setpts=240");
        assert_eq!(setpts(10.0 + 0.9 / 24.0), "setpts=240");
        assert_eq!(setpts(10.0 + 1.1 / 24.0), "setpts=241");
        // NTSC: the rate the graph prints is the rational the `fps` filter parses.
        let layers = still_layers(&tl, std::slice::from_ref(&asset), 29.97, 10.5);
        assert_eq!(layers[1].fades.filters()[0], "settb=expr=100/2997");
    }
}
