//! `checker/p/gblur-plan`: the plan's use of the `gblur` rule, with no FFmpeg and no adapter: which planes of a layer it judges, at which
//! slice threads, for which vector width, and when a blur is judged at all. The rule itself is `checker_p_gblur_rule.rs`'s; the FFmpeg
//! half is `kerf-gpu`'s `checker_p_*` suites.
//!
//! * a plain layer of any size is refused as "a blur on a plane" exactly when a plane of it (luma, and chroma at half size rounded up) is
//!   a hazard at the compositor's pinned slice threads (`GpuCaps::graph_threads`) and the probe's vector width (`blur_lanes`; `0` judges
//!   both);
//! * a blur of sigma 0 is judged all the same, with the blur cap or without it (the kernel runs on its planes), but for the one hazard
//!   that cannot change a sample of the identity: more slice threads than rows, whose empty postscale slice scales by 1 and clips again
//!   (`gblur::plane_hazard_for`; the sigma-0 expectations of this file were changed for it); a layer with no blur is not judged;
//! * without a pin the plan judges the range the machine's budget gives (`cpu_graph_thread_range`) and, for a still whose every layer is
//!   an image or a proxy (one thread, `single_frame_args`), exactly one thread; an export frame (`PlanMode::Motion`) is never one
//!   thread, and a pin wins over all of it.
#![allow(clippy::print_stderr)]

use chrono::Utc;
use kerf_core::{
    Asset, Clip, EffectKinds, ExportOptions, GpuCaps, MediaResolver, OriginalMedia, PlanRequest, Planner, RenderPlan,
    SourceMedia, StreamInfo, StreamKind, Timeline, Track, Unsupported, VideoEffect,
};
use std::sync::Mutex;
use uuid::Uuid;

/// What the kernel gets wrong on a plane (the control flow of `vf_gblur.c` and `x86/vf_gblur.asm`, simulated; the same as
/// `checker_p_gblur_rule.rs`).
///
/// `identity` is a blur of sigma 0, where the empty postscale slice (more threads than rows) scales a vector by 1 and clips it again,
/// which is the sample it was: not a hazard.
fn hazard_of(w: u32, h: u32, t: u32, lanes: u32, identity: bool) -> bool {
    let (w, h, t, lanes) = (i64::from(w), i64::from(h), i64::from(t.max(1)), i64::from(lanes));
    if w <= 1 || h <= 1 {
        return true;
    }
    let jobs = h.min(t);
    for job in 0..jobs {
        let rows = h * (job + 1) / jobs - h * job / jobs;
        if rows >= lanes {
            let (mut x, mut done) = (w - 1, 0);
            loop {
                done += lanes;
                x -= lanes;
                if x < lanes {
                    break;
                }
            }
            if done > w - 1 {
                return true;
            }
        }
    }
    let jobs = (w * h).min(t);
    !identity && (0..jobs).any(|job| h * (job + 1) / jobs - h * job / jobs == 0)
}

fn picture_is_hazard(size: (u32, u32), threads: std::ops::RangeInclusive<u32>, lanes: u32) -> bool {
    picture_is_hazard_of(size, threads, lanes, false)
}

fn picture_is_hazard_of((w, h): (u32, u32), threads: std::ops::RangeInclusive<u32>, lanes: u32, identity: bool) -> bool {
    let widths: &[u32] = if lanes == 0 { &[8, 16] } else { &[lanes] };
    threads.into_iter().any(|t| {
        [(w, h), (w.div_ceil(2), h.div_ceil(2))]
            .into_iter()
            .any(|(pw, ph)| widths.iter().any(|&l| hazard_of(pw, ph, t, l, identity)))
    })
}

fn stream(w: u32, h: u32, image: bool) -> StreamInfo {
    StreamInfo {
        index: 0,
        kind: StreamKind::Video,
        codec: if image { "png" } else { "h264" }.into(),
        width: Some(w),
        height: Some(h),
        fps: Some(30.0),
        sample_rate: None,
        channels: None,
        image,
        projection: None,
        rotation: 0,
        color_transfer: None,
        color_primaries: None,
        pix_fmt: Some("yuv420p".into()),
        color_space: None,
        color_range: Some("tv".into()),
        lead: 0.0,
        time_base: None,
    }
}

fn asset(name: &str, w: u32, h: u32, image: bool) -> Asset {
    Asset {
        id: Uuid::new_v4(),
        path: format!("/media/{name}.mp4"),
        name: name.into(),
        duration: 20.0,
        streams: vec![stream(w, h, image)],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    }
}

/// Every asset decodes from a (pretend) preview proxy.
#[derive(Debug)]
struct Proxies;

impl MediaResolver for Proxies {
    fn resolve(&self, asset: &Asset) -> SourceMedia {
        let mut media = SourceMedia::original(asset);
        media.path = format!("/cache/kerf/proxies/{}.mp4", asset.name);
        media.proxy = true;
        media
    }
}

/// A timeline of one track per asset, a blur on each clip, all at `t = 1`.
fn timeline(assets: &[Asset], sigma: Option<f64>) -> Timeline {
    Timeline {
        tracks: assets
            .iter()
            .enumerate()
            .map(|(n, a)| {
                let mut clip = Clip::new(a.id, 0.0, 10.0, 0.0);
                if let Some(sigma) = sigma {
                    clip.effects = vec![VideoEffect::Blur { sigma }];
                }
                Track {
                    clips: vec![clip],
                    ..Track::new(StreamKind::Video, format!("V{}", n + 1))
                }
            })
            .collect(),
        ..Timeline::new()
    }
}

fn opts() -> ExportOptions {
    ExportOptions {
        fps: Some(30.0),
        ..ExportOptions::default()
    }
}

fn still(assets: &[Asset], sigma: Option<f64>, media: &dyn MediaResolver) -> RenderPlan {
    Planner::new(
        &timeline(assets, sigma),
        assets,
        &opts(),
        PlanRequest::still(kerf_core::CompositeColorPolicy::FixedBt601).with_media(media),
    )
    .expect("planner")
    .at(1.0)
    .expect("plan")
}

fn motion(assets: &[Asset], sigma: Option<f64>) -> RenderPlan {
    Planner::new(
        &timeline(assets, sigma),
        assets,
        &opts(),
        PlanRequest::motion(kerf_core::CompositeColorPolicy::FixedBt601),
    )
    .expect("planner")
    .at_frame(30)
    .expect("plan")
}

fn caps(blur: bool, lanes: u8, pin: Option<u32>) -> GpuCaps {
    let mut caps = GpuCaps::A0;
    caps.motion = true;
    caps.effects = if blur { EffectKinds::BLUR } else { EffectKinds::NONE };
    caps.blur_lanes = lanes;
    caps.graph_threads = pin;
    caps
}

/// The plan's reasons: whether it refuses a blur as one on a plane, and whether it refuses anything else.
fn verdict(plan: &RenderPlan, caps: &GpuCaps) -> (bool, Vec<String>) {
    let why = plan.reasons(caps, plan.size(u32::MAX));
    let plane = why
        .iter()
        .any(|u| matches!(u, Unsupported::Effects(_, r) if r.starts_with("a blur on a plane")));
    let others = why
        .iter()
        .filter(|u| !matches!(u, Unsupported::Effects(_, r) if r.starts_with("a blur on a plane")))
        .map(ToString::to_string)
        .collect();
    (plane, others)
}

#[test]
fn checker_p_gblur_plan_a_plain_layer_is_refused_exactly_where_a_plane_of_it_is_a_hazard_at_the_pinned_threads_and_width() {
    let mut seen = [0usize; 2];
    // Even sizes: the picture is the stream's.
    for (w, h) in [
        (2u32, 2u32),
        (4, 4),
        (8, 8),
        (8, 16),
        (12, 16),
        (16, 8),
        (16, 16),
        (16, 32),
        (18, 18),
        (24, 32),
        (32, 16),
        (32, 30),
        (34, 34),
        (40, 4),
        (40, 6),
        (64, 6),
        (64, 14),
        (64, 16),
        (64, 28),
        (64, 30),
        (64, 64),
        (66, 34),
        (640, 360),
        (640, 28),
    ] {
        let a = asset("a", w, h, false);
        let plan = motion(std::slice::from_ref(&a), Some(2.0));
        for lanes in [0u8, 8, 16] {
            for pin in [1u32, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 64] {
                let (plane, others) = verdict(&plan, &caps(true, lanes, Some(pin)));
                let want = picture_is_hazard((w, h), pin..=pin, u32::from(lanes));
                assert_eq!(plane, want, "{w}x{h}, {lanes} lanes, pinned to {pin} threads");
                assert!(others.is_empty(), "{w}x{h}: {others:?}");
                seen[usize::from(want)] += 1;
            }
        }
    }
    eprintln!("CHECKER-P plan: clear {}, refused {}", seen[0], seen[1]);
    assert!(seen[0] > 300 && seen[1] > 300);
}

#[test]
fn checker_p_gblur_plan_sigma_zero_is_judged_with_and_without_the_blur_cap_and_a_layer_without_a_blur_is_not() {
    for (w, h, pin, lanes) in [
        (12u32, 16u32, 1u32, 16u8),
        (64, 14, 16, 16),
        (64, 64, 1, 16),
        (640, 360, 16, 0),
        (8, 8, 1, 8),
    ] {
        let a = asset("a", w, h, false);
        let want_zero = picture_is_hazard_of((w, h), pin..=pin, u32::from(lanes), true);
        // Sigma 0 with the cap, and without it (drawn as the identity it is, when the kernel runs it right).
        let zero = motion(std::slice::from_ref(&a), Some(0.0));
        for blur_cap in [true, false] {
            let (plane, others) = verdict(&zero, &caps(blur_cap, lanes, Some(pin)));
            assert_eq!(
                plane, want_zero,
                "{w}x{h} sigma 0, cap {blur_cap}, {pin} threads, {lanes} lanes"
            );
            assert!(others.is_empty(), "{w}x{h} sigma 0, cap {blur_cap}: {others:?}");
        }
        // Another sigma without the cap is refused as a blur, not as a plane.
        let two = motion(std::slice::from_ref(&a), Some(2.0));
        let (plane, others) = verdict(&two, &caps(false, lanes, Some(pin)));
        assert!(!plane, "{w}x{h}: a blur the caps leave out is not judged by its planes");
        assert!(others.iter().any(|r| r.contains("a blur")), "{w}x{h}: {others:?}");
        // No blur, no judgement.
        let none = motion(std::slice::from_ref(&a), None);
        assert_eq!(verdict(&none, &caps(true, lanes, Some(pin))), (false, Vec::new()), "{w}x{h}");
    }
}

static BUDGET: Mutex<()> = Mutex::new(());

#[test]
fn checker_p_gblur_plan_the_threads_judged_are_the_pin_else_one_for_a_still_of_images_or_proxies_else_the_machines_range() {
    let _budget = BUDGET.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let was = kerf_core::cpu_percent();
    let (footage, image) = (asset("footage", 64, 64, false), asset("image", 64, 64, true));
    let both = [footage.clone(), image.clone()];
    let clear = caps(true, 0, None);
    for percent in [10u8, 50, 75, 100] {
        kerf_core::set_cpu_percent(percent);
        let (lo, hi) = kerf_core::cpu_graph_thread_range();
        let machine = (u32::try_from(lo).unwrap().max(1), u32::try_from(hi).unwrap().max(1));
        let range = |plan: &RenderPlan, caps: &GpuCaps| plan.graph_thread_range(caps);
        // A still of images: one thread. Of footage, of a footage and an image, and any export frame: the machine's.
        assert_eq!(
            range(&still(std::slice::from_ref(&image), Some(2.0), &OriginalMedia), &clear),
            (1, 1),
            "{percent}%"
        );
        assert_eq!(
            range(&still(std::slice::from_ref(&footage), Some(2.0), &OriginalMedia), &clear),
            machine,
            "{percent}%"
        );
        assert_eq!(
            range(&still(&both, Some(2.0), &OriginalMedia), &clear),
            machine,
            "{percent}%: one layer of footage"
        );
        assert_eq!(
            range(&motion(std::slice::from_ref(&image), Some(2.0)), &clear),
            machine,
            "{percent}%: an export frame"
        );
        assert_eq!(
            range(&motion(std::slice::from_ref(&footage), Some(2.0)), &clear),
            machine,
            "{percent}%"
        );
        // A still of proxies: one thread; of a proxy and an image: one; of a proxy and footage that has none: the machine's.
        let proxies = still(std::slice::from_ref(&footage), Some(2.0), &Proxies);
        assert!(proxies.layers.iter().all(|l| l.source.proxy));
        assert_eq!(range(&proxies, &clear), (1, 1), "{percent}%");
        let proxy_and_image = still(&both, Some(2.0), &Proxies);
        assert_eq!(range(&proxy_and_image, &clear), (1, 1), "{percent}%");
        // A pin wins, over a still of images and an export frame alike (and a pin of 0 is one thread).
        for pin in [1u32, 3, 16, 64] {
            for plan in [
                still(std::slice::from_ref(&image), Some(2.0), &OriginalMedia),
                motion(&both, Some(2.0)),
            ] {
                assert_eq!(range(&plan, &caps(true, 0, Some(pin))), (pin, pin), "{percent}%");
            }
        }
        assert_eq!(range(&proxies, &caps(true, 0, Some(0))), (1, 1));
    }
    kerf_core::set_cpu_percent(was);
}

/// What follows from the range: a picture a hazard at one thread and not at the machine's counts is refused as a still of an image or a
/// proxy and drawn as footage, and the other way round (empty slices) for a picture short of rows.
#[test]
fn checker_p_gblur_plan_the_same_picture_is_judged_differently_as_an_image_and_as_footage() {
    let _budget = BUDGET.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let was = kerf_core::cpu_percent();
    kerf_core::set_cpu_percent(75);
    let (lo, hi) = kerf_core::cpu_graph_thread_range();
    kerf_core::set_cpu_percent(was);
    let cores = kerf_core::cpu_cores();
    // (75% of fewer cores caps nothing or leaves too few threads: the range is then FFmpeg's default, which this test is not about.)
    if cores < 4 {
        eprintln!("CHECKER-P plan: {cores} cores, nothing to compare");
        return;
    }
    assert_eq!(lo, hi, "75% of {cores} cores caps the graph");
    assert!(lo >= 3, "{lo} threads");
    let lo32 = u32::try_from(lo).unwrap();
    kerf_core::set_cpu_percent(75);
    // 12x16 under 16 lanes: a vector of rows over a narrow plane at one thread, rows enough for `lo` threads to cut it.
    let narrow = asset("narrow", 12, 16, false);
    let narrow_image = asset("narrow", 12, 16, true);
    let c = caps(true, 16, None);
    assert!(
        verdict(&still(std::slice::from_ref(&narrow_image), Some(2.0), &OriginalMedia), &c).0,
        "an image: one thread"
    );
    assert!(
        verdict(&still(std::slice::from_ref(&narrow), Some(2.0), &Proxies), &c).0,
        "a proxy: one thread"
    );
    assert!(!picture_is_hazard((12, 16), lo32..=lo32, 16));
    assert!(
        !verdict(&still(std::slice::from_ref(&narrow), Some(2.0), &OriginalMedia), &c).0,
        "footage: {lo} threads"
    );
    // 64 wide and `2 * (lo - 1)` rows: the chroma has fewer rows than the threads, which an image's single thread does not mind.
    let short_rows = u32::try_from(2 * (lo - 1)).unwrap();
    let short = asset("short", 64, short_rows, false);
    let short_image = asset("short", 64, short_rows, true);
    assert!(picture_is_hazard((64, short_rows), lo32..=lo32, 0));
    assert!(
        verdict(&still(std::slice::from_ref(&short), Some(2.0), &OriginalMedia), &c).0,
        "footage: {lo} threads"
    );
    assert!(
        !verdict(&still(std::slice::from_ref(&short_image), Some(2.0), &OriginalMedia), &c).0,
        "an image: one thread"
    );
    kerf_core::set_cpu_percent(was);
}
