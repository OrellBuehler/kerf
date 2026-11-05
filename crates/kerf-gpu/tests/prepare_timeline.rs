//! `prepare_timeline` measures, once and ahead of any frame, what the whole cut can need: a timeline of plain clips measures
//! neither the blend chain nor the placement nor the planar conversion; one with a blended clip and one whose zoom moves
//! measures all three. One test, because each answer is cached once per process: a fresh process is what "unmeasured" is.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test prepare_timeline -- --ignored --nocapture
//! ```

use kerf_core::{BlendKinds, BlendMode, Clip, Keyframe, StreamKind, Timeline, Track};
use kerf_gpu::{blend, conversion, placement_probe};
use uuid::Uuid;

fn timeline(edit: impl Fn(&mut Clip)) -> Timeline {
    let mut clip = Clip::new(Uuid::new_v4(), 0.0, 1.0, 0.0);
    edit(&mut clip);
    Timeline {
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn a_timeline_is_primed_once_for_its_blends_and_its_placements_and_not_for_others() {
    assert_eq!(blend::measured(), [BlendKinds::NONE; 2], "a probe ran before the test asked");
    assert_eq!(placement_probe::measured(), placement_probe::Placement::Unknown);
    assert_eq!(conversion::measured(), conversion::Conversion::Unknown);

    // Plain clips, a held zoom: nothing the probes decide.
    kerf_gpu::prepare_timeline(&timeline(|c| c.transform.scale = 1.4));
    assert_eq!(
        blend::measured(),
        [BlendKinds::NONE; 2],
        "a timeline without a blend measured the blend chain"
    );
    assert_eq!(
        placement_probe::measured(),
        placement_probe::Placement::Unknown,
        "a held zoom measured the placement"
    );
    assert_eq!(
        conversion::measured(),
        conversion::Conversion::Unknown,
        "a held zoom measured the conversion"
    );

    // A blended clip: the blend chain, and only it.
    kerf_gpu::prepare_timeline(&timeline(|c| c.blend_mode = BlendMode::Multiply));
    assert_ne!(
        blend::measured(),
        [BlendKinds::NONE; 2],
        "a blended clip left the blend chain unmeasured"
    );
    assert_eq!(
        placement_probe::measured(),
        placement_probe::Placement::Unknown,
        "a blend measured the placement"
    );

    // A moving zoom: the placement and the conversion of a planar picture under it.
    let key = |time: f64, scale: f64| Keyframe {
        time,
        scale,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    };
    kerf_gpu::prepare_timeline(&timeline(|c| c.keyframes = vec![key(0.0, 1.0), key(1.0, 1.6)]));
    assert_ne!(
        placement_probe::measured(),
        placement_probe::Placement::Unknown,
        "a moving zoom left the placement unmeasured"
    );
    assert_ne!(
        conversion::measured(),
        conversion::Conversion::Unknown,
        "a moving zoom left the planar conversion unmeasured"
    );
}
