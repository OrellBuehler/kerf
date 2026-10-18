//! The render bar: how the Preview will play each span of the cut, from what the engine already
//! decides when it plays.
//!
//! The cut is planned as a play session from its start would plan it ([`PlayCut`]: the export's
//! Motion plan of the slice the stream plays) and every frame of its grid is judged:
//!
//! * [`BarState::Stall`] — playback stalls or cannot start there, whoever plays it: a clip whose
//!   preview proxy it waits for under Proxy only (the stream is refused), a reversed clip (both the
//!   stream's `reverse` and the GPU's cursor read its whole source window before its first frame),
//!   a frame that cannot be planned (a clip whose media is gone).
//! * With the GPU preview on: [`BarState::Gpu`] where the compositor draws the frame exactly and in
//!   time, [`BarState::Stream`] where it hands over to the FFmpeg stream — a plan refusal
//!   ([`RenderPlan::reasons`](crate::render_plan::RenderPlan::reasons) with the compositor's caps, at
//!   the size the Preview renders) or a title that moves ([`PlayRefusal::MovingText`]).
//! * With it off (the default) the stream plays everything else, and nothing is said about it.
//!
//! What only the running GPU preview knows (the self-test, a container whose seeks are not
//! trusted, a surface or backend that is down) is not here: a span shown green can still be the
//! stream's in a session.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::media::MediaResolver;
use crate::model::{Asset, Timeline};
use crate::plan_caps::GpuCaps;
use crate::play_plan::{PlayCut, PlayRefusal};
use crate::render_plan::{still_size, CompositeColorPolicy};

/// Frames judged at most; a longer cut is judged every `n`th frame.
pub const MAX_BAR_FRAMES: u64 = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarState {
    /// The GPU preview draws it.
    Gpu,
    /// The FFmpeg stream plays it, the GPU preview having handed over.
    Stream,
    /// Playback stalls or does not start here.
    Stall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BarSpan {
    pub start: f64,
    pub end: f64,
    pub state: BarState,
    /// A stable key for the reason (`Unsupported::kind`, `reversed`, `proxy_wait`, …).
    pub kind: String,
    /// The reason in words, as of the span's first frame.
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderBar {
    /// Whether the spans were judged for the GPU preview (else only stalls are listed).
    pub gpu_preview: bool,
    pub spans: Vec<BarSpan>,
}

/// What the bar is judged for.
pub struct BarRequest<'a> {
    /// What the GPU preview's compositor draws, `None` when the GPU preview is off.
    pub caps: Option<GpuCaps>,
    /// The width the Preview renders at (the frame is the delivery's shape at that width).
    pub width: u32,
    pub color: CompositeColorPolicy,
    /// What a play session decodes (`ProxyMedia`).
    pub media: &'a dyn MediaResolver,
    /// Assets whose preview proxy playback waits for, with what the Preview says about it.
    pub waits: Vec<(Uuid, String)>,
}

type Verdict = (BarState, String, String);

fn judge(cut: &PlayCut, k: u64, size: (u32, u32), req: &BarRequest<'_>) -> Option<Verdict> {
    let plan = match cut.at_frame(k) {
        Ok(plan) => plan,
        Err(e) => {
            let why = PlayRefusal::NoPlan(e.to_string());
            return Some((BarState::Stall, why.kind().to_string(), why.to_string()));
        }
    };
    if let Some((_, msg)) = req.waits.iter().find(|(id, _)| plan.layers.iter().any(|l| l.asset_id == *id)) {
        return Some((BarState::Stall, "proxy_wait".to_string(), msg.clone()));
    }
    let playing = cut.playing_refusal(&plan);
    if let Some(why @ PlayRefusal::Reversed(_)) = &playing {
        return Some((BarState::Stall, why.kind().to_string(), why.to_string()));
    }
    let caps = req.caps?;
    if let Some(u) = plan.reasons(&caps, size).into_iter().next() {
        return Some((BarState::Stream, u.kind().to_string(), u.to_string()));
    }
    if let Some(why) = playing {
        return Some((BarState::Stream, why.kind().to_string(), why.to_string()));
    }
    Some((BarState::Gpu, "gpu".to_string(), "the GPU preview draws it".to_string()))
}

/// The render bar of `timeline` (as it renders) for `req`. See the [module](self).
pub fn render_bar(timeline: &Timeline, assets: &[Asset], req: &BarRequest<'_>) -> RenderBar {
    let mut bar = RenderBar {
        gpu_preview: req.caps.is_some(),
        spans: Vec::new(),
    };
    let end = timeline.for_render().duration();
    if end.is_nan() || end <= 0.0 {
        return bar;
    }
    let cut = match PlayCut::new(timeline, assets, 0.0, req.color, req.media) {
        Ok(cut) => cut,
        Err(e) => {
            if bar.gpu_preview {
                bar.spans.push(BarSpan {
                    start: 0.0,
                    end,
                    state: BarState::Stream,
                    kind: "no_plan".to_string(),
                    reason: e.to_string(),
                });
            }
            return bar;
        }
    };
    let size = still_size(cut.frame().0, cut.frame().1, req.width);
    let last = cut.frame_at(end - 1e-9);
    let stride = (last + 1).div_ceil(MAX_BAR_FRAMES).max(1);
    let mut k = 0;
    while k <= last {
        let (start, next) = (cut.time_of(k), (k + stride).min(last + 1));
        let span_end = if next > last { end } else { cut.time_of(next) };
        if let Some((state, kind, reason)) = judge(&cut, k, size, req) {
            match bar.spans.last_mut() {
                Some(prev) if prev.state == state && prev.kind == kind && (prev.end - start).abs() < 1e-9 => {
                    prev.end = span_end;
                }
                _ => bar.spans.push(BarSpan {
                    start,
                    end: span_end,
                    state,
                    kind,
                    reason,
                }),
            }
        }
        k = next;
    }
    bar
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track};
    use crate::media::OriginalMedia;
    use crate::model::{TextKeyframe, TextOverlay, Transition, TransitionKind};

    const FIXED: CompositeColorPolicy = CompositeColorPolicy::FixedBt601;

    fn caps() -> GpuCaps {
        GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            text: true,
            ..GpuCaps::A0
        }
    }

    fn asset() -> Asset {
        let mut a = test_asset(vec![video_stream(1280, 720, 30.0)]);
        a.streams[0].pix_fmt = Some("yuv420p".into());
        a
    }

    fn bar(tl: &Timeline, assets: &[Asset], caps: Option<GpuCaps>, waits: Vec<(Uuid, String)>) -> RenderBar {
        render_bar(
            tl,
            assets,
            &BarRequest {
                caps,
                width: 640,
                color: FIXED,
                media: &OriginalMedia,
                waits,
            },
        )
    }

    fn states(b: &RenderBar) -> Vec<(BarState, String)> {
        b.spans.iter().map(|s| (s.state, s.kind.clone())).collect()
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1.0 / 30.0 + 1e-9
    }

    #[test]
    fn a_plain_cut_is_the_gpus_and_says_nothing_with_the_gpu_preview_off() {
        let a = asset();
        let tl = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, 2.0, 0.0)])]);
        let on = bar(&tl, std::slice::from_ref(&a), Some(caps()), Vec::new());
        assert!(on.gpu_preview);
        assert_eq!(states(&on), vec![(BarState::Gpu, "gpu".to_string())]);
        assert_eq!((on.spans[0].start, on.spans[0].end), (0.0, 2.0));
        let off = bar(&tl, &[a], None, Vec::new());
        assert!(!off.gpu_preview);
        assert!(off.spans.is_empty());
    }

    #[test]
    fn a_reversed_clip_stalls_whoever_plays_it() {
        let a = asset();
        let mut back = make_clip(a.id, 0.0, 1.0, 1.0);
        back.speed = -1.0;
        let tl = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, 1.0, 0.0), back])]);
        for caps in [None, Some(caps())] {
            let b = bar(&tl, std::slice::from_ref(&a), caps, Vec::new());
            let stall: Vec<&BarSpan> = b.spans.iter().filter(|s| s.state == BarState::Stall).collect();
            assert_eq!(stall.len(), 1, "{:?}", b.spans);
            assert_eq!(stall[0].kind, "reversed");
            assert!(near(stall[0].start, 1.0) && stall[0].end == 2.0, "{:?}", stall[0]);
            assert!(stall[0].reason.contains("backwards"), "{}", stall[0].reason);
        }
        let on = bar(&tl, &[a], Some(caps()), Vec::new());
        assert_eq!(on.spans[0].state, BarState::Gpu);
        assert!(near(on.spans[0].end, 1.0));
    }

    #[test]
    fn what_the_compositor_refuses_is_the_streams_with_its_reason() {
        let a = asset();
        let first = make_clip(a.id, 0.0, 1.0, 0.0);
        let mut second = make_clip(a.id, 2.0, 3.0, 1.0);
        second.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        let tl = timeline_of(vec![video_track(vec![first, second])]);
        let no_transitions = GpuCaps {
            transitions: false,
            ..caps()
        };
        let b = bar(&tl, std::slice::from_ref(&a), Some(no_transitions), Vec::new());
        let kinds: Vec<BarState> = b.spans.iter().map(|s| s.state).collect();
        assert_eq!(kinds.first(), Some(&BarState::Gpu));
        assert_eq!(kinds.last(), Some(&BarState::Gpu));
        let stream: Vec<&BarSpan> = b.spans.iter().filter(|s| s.state == BarState::Stream).collect();
        assert!(!stream.is_empty(), "{:?}", b.spans);
        assert!(stream.iter().all(|s| s.start > 0.5 && s.end < 1.6), "{stream:?}");
        assert!(!stream[0].reason.is_empty());
        // Spans tile the cut without gaps or overlaps.
        for w in b.spans.windows(2) {
            assert!((w[0].end - w[1].start).abs() < 1e-9, "{:?}", b.spans);
        }
        assert_eq!(b.spans.last().unwrap().end, tl.duration());
        // The A0 compositor plays no motion at all.
        let a0 = bar(&tl, &[a], Some(GpuCaps::A0), Vec::new());
        assert_eq!(states(&a0), vec![(BarState::Stream, "motion".to_string())]);
    }

    #[test]
    fn a_moving_title_is_handed_to_the_stream_but_not_a_stall() {
        let a = asset();
        let mut tl = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, 2.0, 0.0)])]);
        let mut title = TextOverlay::new("Hello", 0.5, 1.0);
        title.keyframes = vec![
            TextKeyframe {
                time: 0.0,
                pos_x: 0.2,
                pos_y: 0.8,
                opacity: 1.0,
            },
            TextKeyframe {
                time: 0.5,
                pos_x: 0.6,
                pos_y: 0.8,
                opacity: 1.0,
            },
        ];
        tl.overlays = vec![title];
        let on = bar(&tl, std::slice::from_ref(&a), Some(caps()), Vec::new());
        let stream: Vec<&BarSpan> = on.spans.iter().filter(|s| s.state == BarState::Stream).collect();
        assert_eq!(stream.len(), 1, "{:?}", on.spans);
        assert_eq!(stream[0].kind, "moving_text");
        assert!(near(stream[0].start, 0.5) && near(stream[0].end, 1.0), "{:?}", stream[0]);
        assert!(bar(&tl, &[a], None, Vec::new()).spans.is_empty());
    }

    #[test]
    fn a_clip_waiting_for_its_proxy_stalls_its_span() {
        let (a, b) = (asset(), asset());
        let tl = timeline_of(vec![video_track(vec![
            make_clip(a.id, 0.0, 1.0, 0.0),
            make_clip(b.id, 0.0, 1.0, 1.0),
        ])]);
        let waits = vec![(b.id, "waiting for the preview proxy of b".to_string())];
        let off = bar(&tl, &[a, b], None, waits);
        assert_eq!(off.spans.len(), 1);
        assert_eq!(off.spans[0].kind, "proxy_wait");
        assert_eq!(off.spans[0].state, BarState::Stall);
        assert!(near(off.spans[0].start, 1.0) && off.spans[0].end == 2.0);
    }

    #[test]
    fn nothing_to_play_is_an_empty_bar_and_a_cut_the_gpu_cannot_plan_is_the_streams() {
        assert!(bar(&Timeline::default(), &[], Some(caps()), Vec::new()).spans.is_empty());
        let mut fast = test_asset(vec![video_stream(1280, 720, 120.0)]);
        fast.streams[0].pix_fmt = Some("yuv420p".into());
        let tl = timeline_of(vec![video_track(vec![make_clip(fast.id, 0.0, 1.0, 0.0)])]);
        let on = bar(&tl, std::slice::from_ref(&fast), Some(caps()), Vec::new());
        assert_eq!(states(&on), vec![(BarState::Stream, "no_plan".to_string())]);
        assert!(on.spans[0].reason.contains("120"), "{}", on.spans[0].reason);
        assert!(bar(&tl, &[fast], None, Vec::new()).spans.is_empty());
    }

    #[test]
    fn a_long_cut_is_judged_every_nth_frame_and_still_tiles() {
        let a = asset();
        let hours = 2.0 * 3600.0;
        let tl = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, hours, 0.0)])]);
        let mut long = a;
        long.duration = hours;
        let on = bar(&tl, &[long], Some(caps()), Vec::new());
        assert_eq!(states(&on), vec![(BarState::Gpu, "gpu".to_string())]);
        assert_eq!(on.spans[0].end, hours);
    }

    #[test]
    fn the_bar_serializes_snake_case() {
        let json = serde_json::to_value(BarSpan {
            start: 0.0,
            end: 1.0,
            state: BarState::Stall,
            kind: "reversed".into(),
            reason: "r".into(),
        })
        .unwrap();
        assert_eq!(json["state"], "stall");
    }
}
