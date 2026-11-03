//! `Timeline::has_white_dip` against the export's own timeline (`Timeline::for_render`): a dip to white is in the graph exactly where
//! `for_render` keeps a clip that comes in through one, so a renderer that asks for the conversions the dip needs (`prepare_timeline`)
//! asks whenever the export has one and never for one that is not in it. Every arrangement of muted, soloed (of either kind), disabled and
//! dipping clips over a few tracks is checked against that oracle, and the cases that must be caught are named.

use kerf_core::{Clip, StreamKind, Timeline, Track, Transition, TransitionKind};
use uuid::Uuid;

fn clip(dip: Option<TransitionKind>, enabled: bool) -> Clip {
    let mut c = Clip::new(Uuid::new_v4(), 0.0, 1.0, 0.0);
    c.enabled = enabled;
    c.transition_in = dip.map(|kind| Transition { kind, duration: 0.3 });
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

/// What the export has: a clip that comes in through a dip to white on a video track `for_render` keeps.
fn oracle(tl: &Timeline) -> bool {
    tl.for_render()
        .tracks
        .iter()
        .filter(|t| t.kind == StreamKind::Video)
        .flat_map(|t| &t.clips)
        .any(|c| c.transition_in.as_ref().is_some_and(|t| t.kind == TransitionKind::DipToWhite))
}

const WHITE: Option<TransitionKind> = Some(TransitionKind::DipToWhite);

#[test]
fn checker_b_m_a_dip_on_a_track_that_renders_is_caught() {
    use StreamKind::{Audio, Video};
    let plain = || clip(None, true);
    let dip = || clip(WHITE, true);
    // Alone, second on its track, on the upper track, among black dips and crossfades.
    for tl in [
        timeline(vec![track(Video, vec![dip()], false, false)]),
        timeline(vec![track(Video, vec![plain(), dip()], false, false)]),
        timeline(vec![
            track(Video, vec![plain()], false, false),
            track(Video, vec![plain(), dip()], false, false),
        ]),
        timeline(vec![track(
            Video,
            vec![
                clip(Some(TransitionKind::DipToBlack), true),
                clip(Some(TransitionKind::Crossfade), true),
                dip(),
            ],
            false,
            false,
        )]),
        // A soloed audio track shadows no video track: the dip on a video track still renders.
        timeline(vec![
            track(Audio, vec![plain()], false, true),
            track(Video, vec![plain(), dip()], false, false),
        ]),
        // A muted video track beside one that renders the dip.
        timeline(vec![
            track(Video, vec![dip()], true, false),
            track(Video, vec![dip()], false, false),
        ]),
        // Two soloed video tracks: the dip is on one of them.
        timeline(vec![
            track(Video, vec![plain()], false, true),
            track(Video, vec![dip()], false, true),
        ]),
        // The only soloed video track holds it.
        timeline(vec![
            track(Video, vec![plain()], false, false),
            track(Video, vec![plain(), dip()], false, true),
        ]),
        // An enabled dip beside a disabled one.
        timeline(vec![track(Video, vec![clip(WHITE, false), dip()], false, false)]),
    ] {
        assert!(oracle(&tl), "the oracle is wrong about {tl:?}");
        assert!(tl.has_white_dip(), "a dip the export has was missed: {tl:?}");
    }
}

#[test]
fn checker_b_m_a_dip_the_export_does_not_have_is_not_asked_for() {
    use StreamKind::{Audio, Video};
    let plain = || clip(None, true);
    for tl in [
        timeline(vec![track(Video, vec![clip(WHITE, true)], true, false)]),
        timeline(vec![track(Video, vec![clip(WHITE, false)], false, false)]),
        timeline(vec![
            track(Video, vec![plain()], false, true),
            track(Video, vec![clip(WHITE, true)], false, false),
        ]),
        // A soloed video track shadows the audio tracks of no kind but its own; the dip is on a shadowed video track.
        timeline(vec![
            track(Video, vec![plain()], false, true),
            track(Audio, vec![plain()], false, false),
            track(Video, vec![plain(), clip(WHITE, true)], false, false),
        ]),
        timeline(vec![track(Video, vec![plain(), plain()], false, false)]),
        timeline(vec![]),
    ] {
        assert!(!oracle(&tl), "the oracle is wrong about {tl:?}");
        assert!(!tl.has_white_dip(), "a dip the export does not have was asked for: {tl:?}");
    }
}

/// Every arrangement of up to three tracks of either kind, each muted or not, soloed or not, with up to two clips, each enabled or
/// not and coming in through a white dip, a black one or nothing.
#[test]
fn checker_b_m_has_white_dip_is_the_exports_over_every_arrangement() {
    let kinds = [StreamKind::Video, StreamKind::Audio];
    let transitions = [None, WHITE, Some(TransitionKind::DipToBlack)];
    // One track: kind, muted, solo, then up to two clips (enabled, transition).
    let mut single: Vec<Track> = Vec::new();
    for &kind in &kinds {
        for muted in [false, true] {
            for solo in [false, true] {
                single.push(track(kind, vec![], muted, solo));
                for &ta in &transitions {
                    for ea in [true, false] {
                        single.push(track(kind, vec![clip(ta, ea)], muted, solo));
                        for &tb in &transitions {
                            for eb in [true, false] {
                                single.push(track(kind, vec![clip(ta, ea), clip(tb, eb)], muted, solo));
                            }
                        }
                    }
                }
            }
        }
    }
    let (mut checked, mut dips) = (0usize, 0usize);
    // Pairs of tracks over every single, and a deterministic sample of triples.
    for a in &single {
        for b in &single {
            let tl = timeline(vec![a.clone(), b.clone()]);
            let want = oracle(&tl);
            assert_eq!(tl.has_white_dip(), want, "{tl:?}");
            checked += 1;
            dips += usize::from(want);
        }
    }
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as usize
    };
    for _ in 0..20_000 {
        let tl = timeline(vec![
            single[next() % single.len()].clone(),
            single[next() % single.len()].clone(),
            single[next() % single.len()].clone(),
        ]);
        let want = oracle(&tl);
        assert_eq!(tl.has_white_dip(), want, "{tl:?}");
        checked += 1;
        dips += usize::from(want);
    }
    assert!(
        dips > 1000 && checked - dips > 1000,
        "{dips} of {checked} have a dip: the sweep is not telling anything"
    );
}
