//! `checker/p2/gblur-plan`: the plan's lifted sigma-0 hazard, tried to be broken with no FFmpeg and no adapter. The real-FFmpeg half is
//! `kerf-gpu`'s `checker_p2_sigma0.rs`.
//!
//! The plan clears `EmptySlices` (more slice threads than rows) for a blur that is the identity, and only that:
//!
//! * the plan's verdict equals an independent simulation of the kernel's loops (`vf_gblur.c`'s slicing, `x86/vf_gblur.asm`'s block loop),
//!   with the empty postscale slice left out only for the identity, over a grid of pictures, **every pin from 1 to 64** and all three
//!   widths of the probe's answer (`blur_lanes` 0, 8, 16): `OneSample` and `NarrowBlocks` stay refused at sigma 0, `EmptySlices` does not;
//! * only sigma exactly 0 is lifted (the NaN path of `set_params`, the same on every build): a small sigma whose derived coefficients
//!   `set_params` makes the same numbers (`1e-9`, `1e-5`: a cancellation in the build's `double` arithmetic the blur probe does not measure)
//!   is a blur like any other, as is one the cancellation of `1 + 2 lambda - sqrt(1 + 4 lambda)` puts outside what the compositor draws at
//!   all (`1.1e-8`); this file's expectations for the tiny sigmas were changed from "lifted" for it;
//! * two blurs on a layer: the one that is not the identity decides;
//! * with or without the blur cap: an identity blur is judged either way, any other only with the cap.
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
            time_base: None,
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

/// The sizes: even (the picture is the stream's), small enough to be every hazard and large enough not to be.
const SIZES: [(u32, u32); 22] = [
    (2, 2),
    (4, 4),
    (6, 4),
    (8, 8),
    (8, 40),
    (12, 16),
    (12, 64),
    (16, 8),
    (16, 16),
    (16, 32),
    (18, 18),
    (24, 32),
    (34, 34),
    (40, 4),
    (40, 6),
    (40, 30),
    (64, 14),
    (64, 28),
    (66, 34),
    (640, 28),
    (640, 360),
    (1920, 1080),
];

#[test]
fn checker_p2_gblur_plan_a_sigma_zero_blur_is_refused_for_the_kernels_reads_past_the_row_and_not_for_the_empty_slice_at_every_pin(
) {
    let (mut lifted, mut kept, mut cleared) = (0usize, 0usize, 0usize);
    for (w, h) in SIZES {
        let a = asset(w, h);
        for sigma in [0.0, 1e-9, 1e-12] {
            let plan = plan_of(&a, &[sigma]);
            for lanes in [0u8, 8, 16] {
                for pin in 1..=64u32 {
                    for blur_cap in [true, false] {
                        let (plane, others) = verdict(&plan, &caps(blur_cap, lanes, pin));
                        // (Only sigma exactly 0 is lifted, and only it is judged without the blur cap; a tiny sigma is a blur like another.)
                        let zero = sigma == 0.0;
                        let judged = blur_cap || zero;
                        let want = judged && picture_hazard((w, h), pin, u32::from(lanes), zero);
                        assert_eq!(
                            plane, want,
                            "{w}x{h} sigma {sigma}, cap {blur_cap}, {lanes} lanes, {pin} threads"
                        );
                        // (Without the blur cap only a blur of exactly 0 is drawn; a tiny sigma that is the same numbers is the cap's to allow.)
                        assert_eq!(
                            others.is_empty(),
                            blur_cap || sigma == 0.0,
                            "{w}x{h} sigma {sigma}, cap {blur_cap}: {others:?}"
                        );
                        let with_slices = picture_hazard((w, h), pin, u32::from(lanes), false);
                        lifted += usize::from(zero && with_slices && !want);
                        kept += usize::from(want);
                        cleared += usize::from(judged && !want);
                    }
                }
            }
        }
    }
    eprintln!(
        "CHECKER-P2 gblur-plan: sigma 0 cleared {cleared}, refused {kept}, of which {lifted} would be refused but for the lift"
    );
    assert!(lifted > 500 && kept > 500 && cleared > 500);
}

#[test]
fn checker_p2_gblur_plan_a_blur_that_is_not_the_identity_keeps_every_hazard_at_every_pin() {
    for (w, h) in SIZES {
        let a = asset(w, h);
        // 1e-6 is small and not the identity (the cancellation leaves `nu` 2.2e-4 and a gain of 2e17); the rest are plain blurs.
        // (1e-5 and 1e-9 derive the identity's numbers but are not sigma 0: not lifted.)
        for sigma in [1e-9, 1e-6, 1e-5, 0.001, 0.5, 2.0, 40.0] {
            let plan = plan_of(&a, &[sigma]);
            for lanes in [0u8, 8, 16] {
                for pin in 1..=64u32 {
                    let (plane, others) = verdict(&plan, &caps(true, lanes, pin));
                    let want = picture_hazard((w, h), pin, u32::from(lanes), false);
                    assert_eq!(plane, want, "{w}x{h} sigma {sigma}, {lanes} lanes, {pin} threads");
                    // (A sigma the compositor cannot draw is refused for itself as well: nothing else may be.)
                    assert!(others.is_empty(), "{w}x{h} sigma {sigma}: {others:?}");
                }
            }
        }
    }
}

#[test]
fn checker_p2_gblur_plan_the_identity_is_decided_from_the_derived_coefficients() {
    // 640x28 pinned to 16 threads: the chroma has 14 rows, so the only hazard is the empty slice.
    let a = asset(640, 28);
    let at16 = |blur_cap: bool| caps(blur_cap, 16, 16);
    for (sigma, identity) in [
        (0.0, true),
        (1e-9, true),
        (1e-12, true),
        (1e-5, true),
        (1e-4, true),
        (1e-6, false),
        (3.162_277_660_168_379e-8, false),
        (0.001, false),
        (0.5, false),
        (2.0, false),
    ] {
        let g = kerf_core::gblur::gaussian(sigma).expect("in the domain");
        assert_eq!(g.is_identity(), identity, "sigma {sigma}: {g:?}");
        let plan = plan_of(&a, &[sigma]);
        // Without the cap only an identity blur is judged (and drawn); with it every blur is.
        let (plane_with, others_with) = verdict(&plan, &at16(true));
        let (plane_without, others_without) = verdict(&plan, &at16(false));
        // (Only sigma exactly 0 is lifted from the empty slice; a tiny sigma with the same coefficients is judged like any blur.)
        assert_eq!(plane_with, sigma != 0.0, "sigma {sigma}, cap");
        assert!(
            !plane_without,
            "sigma {sigma}: without the cap a blur is refused for the cap, not judged"
        );
        assert!(others_with.is_empty(), "sigma {sigma}: {others_with:?}");
        // (Only a blur of exactly 0 is drawn without the cap; a tiny sigma that is the same numbers is the cap's to allow.)
        assert_eq!(
            others_without.is_empty(),
            sigma == 0.0,
            "sigma {sigma}, no cap: {others_without:?}"
        );
    }
    // 1.1e-8: `1 + 2 lambda - sqrt(1 + 4 lambda)` cancels to a `nu` outside what the compositor draws, whatever the pin.
    let g = kerf_core::gblur::gblur_coefficients(1.1e-8).expect("a sigma gblur takes");
    assert!(!g.is_identity() && !g.in_shader_domain(), "{g:?}");
    for pin in [1u32, 16, 17] {
        let plan = plan_of(&a, &[1.1e-8]);
        for blur_cap in [true, false] {
            let why = plan.reasons(&caps(blur_cap, 16, pin), plan.size(u32::MAX));
            assert!(!why.is_empty(), "sigma 1.1e-8 at {pin} threads, cap {blur_cap}: drawn");
        }
    }
}

#[test]
fn checker_p2_gblur_plan_two_blurs_on_a_layer_are_judged_by_the_one_that_is_not_the_identity() {
    let a = asset(640, 28);
    let at16 = caps(true, 16, 16);
    for (sigmas, plane) in [
        (vec![0.0, 0.0], false),
        (vec![0.0, 1e-9], true),
        (vec![0.0, 2.0], true),
        (vec![2.0, 0.0], true),
        (vec![0.0, 0.0, 0.5], true),
        (vec![1e-9, 0.0, 1e-12], true),
        (vec![0.0, 0.0, 0.0], false),
    ] {
        let plan = plan_of(&a, &sigmas);
        assert_eq!(verdict(&plan, &at16).0, plane, "{sigmas:?}");
    }
    // With no blur cap an identity blur is drawn, a mixed pair is not drawn at all.
    let no_cap = caps(false, 16, 16);
    assert!(plan_of(&a, &[0.0, 0.0]).reasons(&no_cap, (640, 28)).is_empty());
    assert!(!plan_of(&a, &[0.0, 2.0]).reasons(&no_cap, (640, 28)).is_empty());
}
