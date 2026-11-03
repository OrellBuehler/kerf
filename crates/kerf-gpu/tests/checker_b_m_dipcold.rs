//! A dip to white and the conversions it needs (`prepare_timeline` measures `grade::kernels()` for a cut whose export has one, and the dip is
//! drawn only where that is `Match`): in a process that has measured nothing, a timeline whose only dip is one the export does not have (a muted
//! track, a track another one's solo shadows, a disabled clip) asks for nothing, and one whose dip is on a track that renders does ask. One
//! test, because the answer is kept for the life of the process. Where the composite follows the bottom layer's tag (9.0.2) a conversion between
//! matrices asks for the same measurement whatever the cut holds, so there only the second half is held to anything.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_b_m_dipcold -- --ignored --nocapture
//! ```

use kerf_core::{Clip, CompositeColorPolicy, StreamKind, Timeline, Track, Transition, TransitionKind};
use kerf_gpu::grade::Kernels;
use uuid::Uuid;

fn clip(dip: bool, enabled: bool) -> Clip {
    let mut c = Clip::new(Uuid::new_v4(), 0.0, 1.0, 0.0);
    c.enabled = enabled;
    if dip {
        c.transition_in = Some(Transition {
            kind: TransitionKind::DipToWhite,
            duration: 0.3,
        });
    }
    c
}

fn track(kind: StreamKind, clips: Vec<Clip>, muted: bool, solo: bool) -> Track {
    Track {
        clips,
        muted,
        solo,
        ..Track::new(kind, "T".to_string())
    }
}

fn timeline(tracks: Vec<Track>) -> Timeline {
    Timeline {
        tracks,
        ..Timeline::new()
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_b_m_a_dip_the_export_has_is_asked_for_and_one_it_does_not_have_is_not() {
    use StreamKind::{Audio, Video};
    assert_eq!(
        kerf_gpu::grade::measured(),
        Kernels::Unknown,
        "a probe ran before the test asked"
    );
    let negotiated = kerf_core::composite_color_policy() == CompositeColorPolicy::BottomLayerTag;
    // None of these dips is in the export's graph.
    let silent = [
        timeline(vec![track(Video, vec![clip(false, true), clip(true, true)], true, false)]),
        timeline(vec![track(Video, vec![clip(true, false)], false, false)]),
        timeline(vec![
            track(Video, vec![clip(false, true)], false, true),
            track(Video, vec![clip(false, true), clip(true, true)], false, false),
        ]),
    ];
    if !negotiated {
        for (i, tl) in silent.iter().enumerate() {
            assert!(!tl.for_render().has_white_dip(), "case {i}: the export has a dip");
            kerf_gpu::prepare_timeline(tl);
            assert_eq!(
                kerf_gpu::grade::measured(),
                Kernels::Unknown,
                "case {i}: a dip the export does not have asked for the conversions"
            );
        }
    }
    // A dip on a track that renders, beside a soloed audio track (which shadows no video track) and a muted track.
    let loud = timeline(vec![
        track(Audio, vec![clip(false, true)], false, true),
        track(Video, vec![clip(true, true)], true, false),
        track(Video, vec![clip(false, true), clip(true, true)], false, false),
    ]);
    assert!(loud.for_render().has_white_dip(), "the export has a dip");
    kerf_gpu::prepare_timeline(&loud);
    assert_ne!(
        kerf_gpu::grade::measured(),
        Kernels::Unknown,
        "a dip the export has did not ask for the conversions"
    );
    let caps = kerf_gpu::Compositor::caps_for(
        kerf_gpu::grade::measured(),
        kerf_gpu::scaler::measured(),
        kerf_gpu::convert::measured(),
    );
    assert_eq!(
        caps.dip_white,
        kerf_gpu::grade::measured() == Kernels::Match,
        "the dip is in the caps exactly where the conversions are measured to match"
    );
}
