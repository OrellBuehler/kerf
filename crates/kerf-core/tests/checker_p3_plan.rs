//! `checker/p3/plan`: which sigma the plan lifts the empty-slice hazard for, from the outside: **only a sigma that is exactly 0** (`0.0` and
//! `-0.0`; `fnum` writes both as `0`). Every other sigma is judged as the blur it is or refused, however tiny it is and whatever the
//! coefficients `set_params` derives from it: a denormal, `1e-50` (a `float` of 0 inside the filter), `1e-12`, `1e-9`, `1e-5`, `1e-3`; a
//! mixed list of blurs is judged by the first that is not exactly 0; a sigma `gblur` refuses (NaN, infinity, negative, over 1024) or the
//! compositor does not compute (about 1.06e-8 to 1.29e-8) is refused at every pin, with and without the blur cap.
#![allow(clippy::print_stderr)]

use chrono::Utc;
use kerf_core::{
    Asset, Clip, EffectKinds, ExportOptions, GpuCaps, PlanRequest, Planner, RenderPlan, StreamInfo, StreamKind, Timeline, Track,
    Unsupported, VideoEffect,
};
use uuid::Uuid;

/// What the kernel gets wrong on a plane, from the loops themselves. `identity`: the blur is `nu` 0 with both scales 1.
fn hazard(w: u32, h: u32, threads: u32, lanes: u32, identity: bool) -> bool {
    let (w, h, t, lanes) = (i64::from(w), i64::from(h), i64::from(threads.max(1)), i64::from(lanes));
    // The horizontal and vertical kernels read a row (or a column) below the first one too often when there is only one.
    if w < 2 || h < 2 {
        return true;
    }
    // Horizontal: `min(h, t)` slices of `h * job / jobs` rows. A slice of `lanes` rows or more takes the blocked path, whose first block
    // of `lanes` columns runs before the test for the row's end: it overshoots the row when `w - 1 < lanes`.
    let jobs = h.min(t);
    for job in 0..jobs {
        let rows = h * (job + 1) / jobs - h * job / jobs;
        if rows >= lanes && w - 1 < lanes {
            return true;
        }
    }
    // Postscale: `min(w * h, t)` slices; one of no rows still scales a vector. Harmless when it scales by 1.
    let jobs = (w * h).min(t);
    !identity && (0..jobs).any(|job| h * (job + 1) / jobs - h * job / jobs == 0)
}

fn picture_hazard(picture: (u32, u32), threads: u32, lanes: u32, identity: bool) -> bool {
    let widths: &[u32] = if lanes == 0 { &[8, 16] } else { &[lanes][..] };
    let (w, h) = picture;
    [(w, h), (w.div_ceil(2), h.div_ceil(2))]
        .into_iter()
        .any(|(pw, ph)| widths.iter().any(|&l| hazard(pw, ph, threads, l, identity)))
}

fn asset(w: u32, h: u32) -> Asset {
    Asset {
        id: Uuid::new_v4(),
        path: "/media/a.mp4".into(),
        name: "a".into(),
        duration: 20.0,
        streams: vec![StreamInfo {
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
            color_range: Some("tv".into()),
            lead: 0.0,
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    }
}

fn plan_of(a: &Asset, sigmas: &[f64]) -> RenderPlan {
    let mut clip = Clip::new(a.id, 0.0, 10.0, 0.0);
    clip.effects = sigmas.iter().map(|&sigma| VideoEffect::Blur { sigma }).collect();
    let timeline = Timeline {
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V1")
        }],
        ..Timeline::new()
    };
    let opts = ExportOptions {
        fps: Some(30.0),
        ..ExportOptions::default()
    };
    Planner::new(
        &timeline,
        std::slice::from_ref(a),
        &opts,
        PlanRequest::motion(kerf_core::CompositeColorPolicy::FixedBt601),
    )
    .expect("planner")
    .at_frame(30)
    .expect("plan")
}

fn caps(blur: bool, lanes: u8, pin: u32) -> GpuCaps {
    let mut caps = GpuCaps::A0;
    caps.motion = true;
    caps.effects = if blur { EffectKinds::BLUR } else { EffectKinds::NONE };
    caps.blur_lanes = lanes;
    caps.graph_threads = Some(pin);
    caps
}

/// `(refused as a blur on a plane, every other reason)`.
fn verdict(plan: &RenderPlan, caps: &GpuCaps) -> (bool, Vec<String>) {
    let why = plan.reasons(caps, plan.size(u32::MAX));
    let is_plane = |u: &Unsupported| matches!(u, Unsupported::Effects(_, r) if r.starts_with("a blur on a plane"));
    (
        why.iter().any(is_plane),
        why.iter().filter(|u| !is_plane(u)).map(ToString::to_string).collect(),
    )
}

/// Sigmas that are refused whatever the picture and the threads (the filter refuses them, or the compositor's domain does not hold them).
fn refused_for_itself(sigma: f64) -> bool {
    !sigma.is_finite() || !(0.0..=1024.0).contains(&sigma) || (1.06e-8..=1.29e-8).contains(&sigma)
}

const SIGMAS: [f64; 31] = [
    0.0,
    -0.0,
    5e-324,
    f64::MIN_POSITIVE,
    1e-300,
    1e-50,
    1.4e-45,
    1.2e-38,
    1e-20,
    1e-12,
    1e-9,
    1e-8,
    1.05e-8,
    1.1e-8,
    1.2e-8,
    1.29e-8,
    1.3e-8,
    1e-7,
    1e-6,
    1e-5,
    1e-3,
    0.5,
    3.0,
    1024.0,
    1024.5,
    -1e-9,
    -1.0,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
    5000.0,
];

const PICTURES: [(u32, u32); 12] = [
    (2, 2),
    (4, 4),
    (8, 8),
    (12, 16),
    (12, 64),
    (16, 4),
    (40, 6),
    (64, 4),
    (64, 14),
    (640, 28),
    (640, 360),
    (1920, 1080),
];

#[test]
fn checker_p3_plan_only_a_sigma_of_exactly_zero_is_lifted_from_the_empty_slice_at_every_pin() {
    let (mut lifted, mut judged_as_blur, mut domain) = (0usize, 0usize, 0usize);
    for (w, h) in PICTURES {
        let a = asset(w, h);
        for sigma in SIGMAS {
            let plan = plan_of(&a, &[sigma]);
            let zero = sigma == 0.0;
            for lanes in [0u8, 8, 16] {
                for pin in (1..=20u32).chain([32, 64, 128]) {
                    for blur_cap in [true, false] {
                        let (plane, others) = verdict(&plan, &caps(blur_cap, lanes, pin));
                        let in_domain = !refused_for_itself(sigma);
                        // Judged when the cap allows blurs, and without it for sigma exactly 0 alone (it is drawn as no pass).
                        let judged = blur_cap || zero;
                        let want = judged && picture_hazard((w, h), pin, u32::from(lanes), zero);
                        assert_eq!(
                            plane, want,
                            "{w}x{h} sigma {sigma:e}, cap {blur_cap}, {lanes} lanes, {pin} threads"
                        );
                        let drawable_alone = in_domain && (blur_cap || zero);
                        assert_eq!(
                            others.is_empty(),
                            drawable_alone,
                            "{w}x{h} sigma {sigma:e}, cap {blur_cap}: other reasons {others:?}"
                        );
                        lifted += usize::from(zero && judged && !want && picture_hazard((w, h), pin, u32::from(lanes), false));
                        judged_as_blur += usize::from(!zero && judged && want);
                        domain += usize::from(!in_domain);
                    }
                }
            }
        }
    }
    eprintln!("CHECKER-P3 plan: {lifted} verdicts lifted at sigma 0, {judged_as_blur} refused as a plain blur, {domain} refused for the sigma itself");
    assert!(lifted > 500 && judged_as_blur > 5000 && domain > 500);
}

#[test]
fn checker_p3_plan_a_list_of_blurs_is_judged_by_the_first_that_is_not_exactly_zero() {
    // 64x4 pinned to 16 threads: the only hazard is the empty slice (luma 4 rows, chroma 2).
    let a = asset(64, 4);
    let at16 = caps(true, 16, 16);
    for (sigmas, refused) in [
        (vec![0.0], false),
        (vec![-0.0], false),
        (vec![0.0, -0.0, 0.0], false),
        (vec![-0.0, 5e-324], true),
        (vec![5e-324, -0.0], true),
        (vec![0.0, 1e-50], true),
        (vec![0.0, 1e-9, 0.0], true),
        (vec![0.0, 0.0, 1e-5], true),
        (vec![1e-12], true),
        (vec![0.0, 3.0], true),
    ] {
        let plan = plan_of(&a, &sigmas);
        let (plane, others) = verdict(&plan, &at16);
        assert_eq!(plane, refused, "{sigmas:?}");
        assert!(others.is_empty(), "{sigmas:?}: {others:?}");
        // Without the cap only a list of zeros is drawn at all.
        let no_cap = caps(false, 16, 16);
        let drawn = plan.reasons(&no_cap, plan.size(u32::MAX)).is_empty();
        assert_eq!(drawn, sigmas.iter().all(|s| *s == 0.0), "{sigmas:?} without the cap");
    }
    // A sigma the filter refuses beside a zero: the list is refused.
    for bad in [f64::NAN, f64::INFINITY, -1e-9, 1024.5, 1.1e-8] {
        for cap in [true, false] {
            let plan = plan_of(&a, &[0.0, bad]);
            assert!(
                !plan.reasons(&caps(cap, 16, 1), plan.size(u32::MAX)).is_empty(),
                "[0, {bad:e}] cap {cap}"
            );
        }
    }
}
