use std::collections::BTreeMap;

use kerf_core::model::{Color, Fit, Keyframe, Mask, MaskShape, Transition, TransitionKind, VideoEffect};
use kerf_core::{
    composite_color_policy, Asset, Clip, Delivery, EffectKinds, ExportOptions, GpuCaps, OriginalMedia, PlanRequest, Planner,
    StreamKind, TextOverlay, Timeline, Track, Transform,
};

fn clip(a: &Asset, src_in: f64, dur: f64, start: f64) -> Clip {
    let src_in = src_in.min((a.duration - dur - 0.1).max(0.0));
    Clip::new(a.id, src_in, src_in + dur, start)
}

fn captions(from: f64, to: f64, font: Option<&str>, boxed: bool) -> Vec<TextOverlay> {
    // chunks of ~2.2 s with a 0.2 s breath between them, like generate_captions on a talking head
    let mut out = vec![];
    let mut t = from;
    let mut i = 0;
    while t + 1.0 < to {
        let len = 1.6 + (i % 3) as f64 * 0.5;
        let end = (t + len).min(to);
        let mut o = TextOverlay::new(format!("caption {i}"), t, end);
        o.font = font.map(String::from);
        if boxed {
            o.bg = Some("black@0.5".into());
        }
        out.push(o);
        t = end + 0.2;
        i += 1;
    }
    out
}

pub struct Arch {
    pub name: &'static str,
    pub tl: Timeline,
}

pub fn archetypes(base: &Asset, side: &Asset, font: Option<&str>) -> Vec<Arch> {
    let mut v = vec![];

    // T1: talking-head short, 9:16, 15 clips x 4 s, 0.1 s fades on every clip edge, captions over ~85%
    {
        let mut clips = vec![];
        for i in 0..15 {
            let mut c = clip(base, (i % 6) as f64, 4.0, i as f64 * 4.0);
            c.fade_in = 0.1;
            c.fade_out = 0.1;
            c.transform = Transform { crop_left: 0.2, crop_right: 0.2, ..Transform::default() };
            clips.push(c);
        }
        let tl = Timeline {
            tracks: vec![Track { clips, ..Track::new(StreamKind::Video, "V1") }],
            overlays: captions(1.0, 59.0, font, true),
            markers: vec![],
            format: Some(Delivery::new(1080, 1920, Fit::Cover)),
            master: Default::default(),
        };
        v.push(Arch { name: "T1 talking-head short 9:16 (60 s, 15 clips, 0.1 s clip fades, ~85% captions)", tl });
    }
    // T1b: same without clip fades
    {
        let mut a = v[0].tl.clone();
        for c in &mut a.tracks[0].clips {
            c.fade_in = 0.0;
            c.fade_out = 0.0;
        }
        v.push(Arch { name: "T1b same, no clip fades (hard cuts), captions", tl: a });
        let mut b = v[0].tl.clone();
        b.overlays.clear();
        v.push(Arch { name: "T1c same, clip fades, no captions", tl: b });
    }
    // T2: montage 16:9 with transitions + graded + 2 titles
    {
        let mut clips = vec![];
        let n = 18;
        let dur = 2.5;
        for i in 0..n {
            let mut c = clip(base, (i % 8) as f64, dur, i as f64 * dur);
            c.color = Color { contrast: 1.1, saturation: 1.2, ..Color::default() };
            if i > 0 {
                let kind = match i % 10 {
                    0..=5 => Some(TransitionKind::Crossfade),
                    6 | 7 => Some(TransitionKind::DipToBlack),
                    8 => Some(TransitionKind::SlideLeft),
                    _ => None,
                };
                c.transition_in = kind.map(|kind| Transition { kind, duration: if kind == TransitionKind::Crossfade { 0.5 } else { 0.4 } });
            }
            clips.push(c);
        }
        clips[0].fade_in = 1.0;
        clips[n - 1].fade_out = 1.0;
        let total = n as f64 * dur;
        let mut t1 = TextOverlay::new("Summer 2026", 0.5, 3.5);
        let mut t2 = TextOverlay::new("The end", total - 3.5, total - 0.5);
        for t in [&mut t1, &mut t2] {
            t.font = font.map(String::from);
            t.bg = Some("black@0.5".into());
        }
        let tl = Timeline {
            tracks: vec![Track { clips, ..Track::new(StreamKind::Video, "V1") }],
            overlays: vec![t1, t2],
            markers: vec![],
            format: Some(Delivery::new(1920, 1080, Fit::Cover)),
            master: Default::default(),
        };
        v.push(Arch { name: "T2 montage 16:9 (45 s, 18 clips, 55% dissolves/12% dips/6% slides, graded, 2 titles)", tl });
    }
    // T3: tutorial with webcam PiP, lower thirds, keyed zooms, one masked+blurred clip
    {
        let mut v1 = vec![];
        for i in 0..6 {
            let mut c = clip(base, (i % 5) as f64, 15.0, i as f64 * 15.0);
            if i < 5 {
                // slow push-in
                c.keyframes = vec![
                    Keyframe { time: 0.0, scale: 1.0, pos_x: 0.0, pos_y: 0.0, rotation: 0.0, opacity: 1.0, easing: Default::default() },
                    Keyframe { time: 15.0, scale: 1.25, pos_x: -0.05, pos_y: 0.0, rotation: 0.0, opacity: 1.0, easing: Default::default() },
                ];
            }
            if i == 3 {
                c.mask = Some(Mask { shape: MaskShape::Rect, x: 0.5, y: 0.5, width: 0.6, height: 0.6, feather: 0.1, inverted: false });
                c.effects = vec![VideoEffect::Blur { sigma: 6.0 }];
            }
            v1.push(c);
        }
        let mut v2 = vec![];
        for i in 0..4 {
            let mut c = clip(side, 0.0, 20.0, i as f64 * 22.0);
            c.transform = Transform { scale: 0.25, pos_x: 0.35, pos_y: 0.3, ..Transform::default() };
            v2.push(c);
        }
        let mut overlays = vec![];
        for i in 0..5 {
            let mut t = TextOverlay::new(format!("Step {i}"), 3.0 + i as f64 * 17.0, 7.0 + i as f64 * 17.0);
            t.font = font.map(String::from);
            overlays.push(t);
        }
        let tl = Timeline {
            tracks: vec![Track { clips: v1, ..Track::new(StreamKind::Video, "V1") }, Track { clips: v2, ..Track::new(StreamKind::Video, "V2") }],
            overlays,
            markers: vec![],
            format: Some(Delivery::new(1920, 1080, Fit::Cover)),
            master: Default::default(),
        };
        v.push(Arch { name: "T3 tutorial 16:9 (90 s, screen + webcam PiP, 5 lower thirds, 5 keyed push-ins, 1 mask+blur)", tl });
    }
    v
}

fn name_of(u: &kerf_core::Unsupported) -> String {
    let d = format!("{u:?}");
    d.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_string()
}

pub fn run(base: &Asset, side: &Asset) {
    let policy = composite_color_policy();
    let fonts = kerf_core::list_system_fonts();
    let font = ["DejaVu Sans", "Noto Sans", "Liberation Sans"]
        .iter()
        .find(|f| fonts.iter().any(|x| x == *f) && kerf_core::fonts::resolve_font_file(f, false).is_some())
        .map(|s| s.to_string());
    println!("explicit font used for the 'explicit font' runs: {font:?} (resolvable: {})", font.as_deref().is_some_and(|f| kerf_core::fonts::resolve_font_file(f, false).is_some()));
    let assets = vec![base.clone(), side.clone()];
    for (label, fontname) in [("default font (font: None, the model default)", None), ("explicit font", font.as_deref())] {
        println!("\n=================== titles/captions with {label} ===================");
        for arch in archetypes(base, side, fontname) {
            println!("\n### {}", arch.name);
            for (mode_name, req) in [("Still plan (paused/scrubbed frames)", PlanRequest::still(policy)), ("Motion plan (playback/export frames)", PlanRequest::motion(policy))] {
                let planner = match Planner::new(&arch.tl, &assets, &ExportOptions::default(), req.with_media(&OriginalMedia)) {
                    Ok(p) => p,
                    Err(e) => {
                        println!("  planner error: {e}");
                        continue;
                    }
                };
                let p0 = planner.at_frame(0).unwrap();
                let fps = p0.canvas.fps;
                let dur = arch.tl.duration();
                let size = p0.size(540);
                let frames = (dur * f64::from(fps.num) / f64::from(fps.den)).floor() as u64;
                let mut ladder: Vec<(&str, GpuCaps)> = vec![];
                let mut caps = GpuCaps::A0;
                ladder.push(("A0 (today)", caps));
                if mode_name.starts_with("Motion") {
                    caps.motion = true;
                    ladder.push(("+motion (Fps picks, per-frame geometry)", caps));
                    caps.fades = true;
                    ladder.push(("+fades (clip fades, dips, dissolve ramp)", caps));
                    caps.transitions = true;
                    ladder.push(("+transitions (slide/push travel, tails)", caps));
                }
                caps.text = true;
                ladder.push(("+text", caps));
                caps.mask = true;
                caps.effects = EffectKinds::ALL;
                ladder.push(("+mask +all effects", caps));
                caps.keyed_zoom = true;
                caps.keyed_opacity = true;
                caps.keyed_color = true;
                ladder.push(("+keyed zoom/opacity/colour", caps));
                println!("  -- {mode_name}: canvas {}x{} @ {}/{}, {frames} frames, render {}x{}", p0.canvas.width, p0.canvas.height, fps.num, fps.den, size.0, size.1);
                for (n, c) in &ladder {
                    let sp = planner.span(0.0, dur, size, c);
                    let ok: u64 = sp.runs().iter().filter(|r| r.supported).map(|r| r.frames.end - r.frames.start).sum();
                    let total: u64 = sp.runs().iter().map(|r| r.frames.end - r.frames.start).sum();
                    let runs = sp.runs().iter().filter(|r| r.supported).count();
                    let unsup_runs = sp.runs().iter().filter(|r| !r.supported).count();
                    println!("     {:<44} drawable {:5.1}% ({ok}/{total}); {runs} drawable stretches, {unsup_runs} refused stretches", n, 100.0 * ok as f64 / total.max(1) as f64);
                }
                // which reasons hit the A0 plan
                let mut hist: BTreeMap<String, (u64, u64)> = BTreeMap::new();
                let a0 = GpuCaps::A0;
                for k in 0..frames {
                    if let Ok(plan) = planner.at_frame(k) {
                        let rs = plan.reasons(&a0, size);
                        let mut seen = std::collections::BTreeSet::new();
                        for r in &rs {
                            seen.insert(name_of(r));
                        }
                        for s in &seen {
                            let e = hist.entry(s.clone()).or_default();
                            e.0 += 1;
                            if seen.len() == 1 {
                                e.1 += 1;
                            }
                        }
                    }
                }
                let mut h: Vec<_> = hist.into_iter().collect();
                h.sort_by_key(|(_, (a, _))| std::cmp::Reverse(*a));
                println!("     reasons at A0 (frames hit / frames where it is the only reason): {}", h.iter().map(|(k, (a, b))| format!("{k} {a}/{b}")).collect::<Vec<_>>().join(", "));
            }
        }
    }
}
