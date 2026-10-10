#![allow(clippy::print_stderr, clippy::print_stdout)]
use std::path::Path;
use std::time::{Duration, Instant};

mod coverage;
use kerf_core::{
    composite_color_policy, Asset, Clip, Delivery, ExportOptions, Fit, OriginalMedia, PlanRequest, Planner, Project, StreamKind,
    Timeline, Track, Transform,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, Hint};

fn cpu_secs() -> f64 {
    // utime + stime of this process, in seconds (clock ticks = 100/s).
    let s = std::fs::read_to_string("/proc/self/stat").unwrap();
    let rest = &s[s.rfind(')').unwrap() + 2..];
    let f: Vec<&str> = rest.split(' ').collect();
    (f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap()) / 100.0
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

struct Stats(Vec<f64>);
impl Stats {
    fn med(&self) -> f64 {
        let mut v = self.0.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }
    fn p95(&self) -> f64 {
        let mut v = self.0.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[(v.len() * 95 / 100).min(v.len() - 1)]
    }
    fn min(&self) -> f64 {
        self.0.iter().cloned().fold(f64::MAX, f64::min)
    }
    fn mean(&self) -> f64 {
        self.0.iter().sum::<f64>() / self.0.len() as f64
    }
    fn line(&self, name: &str) -> String {
        format!("{name}: min {:.2} med {:.2} mean {:.2} p95 {:.2} ms", self.min(), self.med(), self.mean(), self.p95())
    }
}

fn track(assets: &[(&Asset, Transform)], dur: f64, n: usize) -> Vec<Track> {
    let _ = n;
    assets
        .iter()
        .enumerate()
        .map(|(i, (a, tf))| {
            let off = if i == 2 { 4.0 } else { 0.0 };
            let mut c = Clip::new(a.id, off, off + dur.min(a.duration - 0.2 - off), 0.0);
            c.transform = *tf;
            let _ = n;
            Track {
                clips: vec![c],
                ..Track::new(StreamKind::Video, format!("V{}", i + 1))
            }
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("all");
    let proxies = Path::new("/home/orell/.cache/kerf/proxies");
    let a720_25 = Project::probe_asset(&proxies.join("af5e3ad1363126f9.mp4")).expect("probe");
    let a960_30 = Project::probe_asset(&proxies.join("696e11ba56ff4c27.mp4")).expect("probe");
    let a720_60 = Project::probe_asset(&proxies.join("14a1e25d934e14e5.mp4")).expect("probe");
    let assets = vec![a720_25.clone(), a960_30.clone(), a720_60.clone()];

    if mode == "cut" {
        let policy = composite_color_policy();
        let gpu = Gpu::new(GpuOptions::for_tests()).expect("adapter");
        let comp = Compositor::new(gpu.clone()).expect("compositor");
        // A (af5e, 25 fps) 0..3 s, then B (696e, 30 fps) from 2 s: a cut between two assets at t = 3 s
        let mut ca = Clip::new(a720_25.id, 0.0, 3.0, 0.0);
        ca.transform = Transform::default();
        let cb = Clip::new(a960_30.id, 2.0, 5.0, 3.0);
        let tl = Timeline {
            tracks: vec![Track { clips: vec![ca, cb], ..Track::new(StreamKind::Video, "V1") }],
            overlays: vec![],
            markers: vec![],
            format: Some(Delivery::new(1280, 720, Fit::Contain)),
            master: Default::default(),
        };
        let planner = Planner::new(&tl, &assets, &ExportOptions::default(), PlanRequest::still(policy).with_media(&OriginalMedia)).unwrap();
        let p0 = planner.at_frame(0).unwrap();
        let size = p0.size(960);
        let fps = f64::from(p0.canvas.fps.num) / f64::from(p0.canvas.fps.den);
        for round in 0..3 {
            let src = FrameSource::new(FrameSourceConfig::default());
            let mut waits = vec![];
            for k in 50..110u64 {
                let plan = planner.at_frame(k).unwrap();
                let t0 = Instant::now();
                let (frame, tm) = comp.render_plan_texture_with(&plan, size, &src, Hint::Forward { fps }).expect("render");
                let _ = frame.read_back(&gpu);
                waits.push((k, ms(tm.decode), ms(t0.elapsed())));
            }
            let cut_k = (3.0 * fps).round() as u64;
            let worst = waits.iter().cloned().fold((0, 0.0, 0.0), |a, b| if b.1 > a.1 { b } else { a });
            println!("round {round}: cut at frame {cut_k}; decode wait around the cut (frames {}..{}): {}", cut_k - 2, cut_k + 3, waits.iter().filter(|w| w.0 >= cut_k - 2 && w.0 < cut_k + 3).map(|w| format!("k{}={:.0}ms", w.0, w.1)).collect::<Vec<_>>().join(" "));
            println!("          worst decode wait in the span: frame {} {:.0} ms (total {:.0} ms); other frames' median {:.1} ms", worst.0, worst.1, worst.2, { let mut v: Vec<f64> = waits.iter().map(|w| w.1).collect(); v.sort_by(|a,b| a.partial_cmp(b).unwrap()); v[v.len()/2] });
            src.release_all();
        }
        return;
    }
    if mode == "mixed" {
        let policy = composite_color_policy();
        println!("composite colour policy of this ffmpeg: {policy:?}");
        let pip = Transform { scale: 0.4, pos_x: 0.3, pos_y: -0.25, ..Transform::default() };
        let base_untagged = &a720_25; // af5e: untagged
        let tagged = &a720_60; // 14a1: tv / bt709
        for (name, base, over) in [("untagged base + untagged PiP", base_untagged, &a960_30), ("untagged base + bt709 PiP", base_untagged, tagged), ("bt709 base + untagged PiP", tagged, base_untagged)] {
            let tl = Timeline {
                tracks: track(&[(base, Transform::default()), (over, pip)], 5.0, 0),
                overlays: vec![],
                markers: vec![],
                format: None,
                master: Default::default(),
            };
            let pl = Planner::new(&tl, &assets, &ExportOptions::default(), PlanRequest::still(policy).with_media(&OriginalMedia)).unwrap();
            let plan = pl.at_frame(25).unwrap();
            let rs = plan.reasons(&kerf_core::GpuCaps::A0, plan.size(540));
            println!("{name}: matrices {:?} / {:?} -> refused: {}", plan.layers[0].stream.matrix(), plan.layers[1].stream.matrix(), if rs.is_empty() { "no".to_string() } else { rs.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("; ") });
        }
        return;
    }
    if mode == "probes" {
        for f in ["af5e3ad1363126f9", "696e11ba56ff4c27", "14a1e25d934e14e5"] {
            let path = proxies.join(format!("{f}.mp4"));
            let t0 = Instant::now();
            let a = kerf_core::source_is_indexed_container(&path);
            let t1 = Instant::now();
            let b = kerf_core::source_seek_points_are_keyframes(&path);
            let t2 = Instant::now();
            let c = kerf_core::source_is_indexed_container(&path);
            let t3 = Instant::now();
            println!("{f}: indexed={a} first {:.0} ms; seek-points keyframes={b} {:.0} ms; repeat (cached) {:.3} ms ({c})", ms(t1 - t0), ms(t2 - t1), ms(t3 - t2));
        }
        return;
    }
    if mode == "coverage" {
        coverage::run(&a720_25, &a960_30);
        return;
    }
    let t0 = Instant::now();
    let policy = composite_color_policy();
    println!("cold start: composite_color_policy() {:.0} ms ({policy:?})", ms(t0.elapsed()));
    let t0 = Instant::now();
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("adapter");
    println!("cold start: Gpu::new (instance+adapter+device) {:.0} ms ({:?})", ms(t0.elapsed()), gpu.adapter_info().name);
    let t0 = Instant::now();
    let comp = Compositor::new(gpu.clone()).expect("compositor");
    println!("cold start: Compositor::new (shaders+pipelines) {:.0} ms", ms(t0.elapsed()));

    let pip = Transform { scale: 0.4, pos_x: 0.3, pos_y: -0.25, opacity: 0.8, ..Transform::default() };
    let pip2 = Transform { scale: 0.3, pos_x: -0.3, pos_y: 0.25, rotation: 8.0, ..Transform::default() };
    let id = Transform::default();

    struct Case {
        name: &'static str,
        delivery: Option<Delivery>,
        layers: Vec<(Asset, Transform)>,
        width: u32,
    }
    let cases = vec![
        Case { name: "1 layer 720p25 -> 16:9 canvas, 960w render", delivery: None, layers: vec![(a720_25.clone(), id)], width: 960 },
        Case { name: "1 layer 720p25 -> 16:9 canvas, 430w render", delivery: None, layers: vec![(a720_25.clone(), id)], width: 430 },
        Case {
            name: "1 layer 720p25 cover into 1080x1920 (9:16), render 540w",
            delivery: Some(Delivery::new(1080, 1920, Fit::Cover)),
            layers: vec![(a720_25.clone(), id)],
            width: 540,
        },
        Case {
            name: "3 layers (base + 2 PiP, one 80% opaque, one rotated), 960w",
            delivery: None,
            layers: vec![(a720_25.clone(), id), (a960_30.clone(), pip), (a720_25.clone(), pip2)],
            width: 960,
        },
    ];

    for case in &cases {
        if mode != "all" && !case.name.contains(mode) {
            continue;
        }
        let refs: Vec<(&Asset, Transform)> = case.layers.iter().map(|(a, t)| (a, *t)).collect();
        let tl = Timeline {
            tracks: track(&refs, 7.0, 0),
            overlays: Vec::new(),
            markers: Vec::new(),
            format: case.delivery,
            master: Default::default(),
        };
        let planner = Planner::new(&tl, &assets, &ExportOptions::default(), PlanRequest::still(policy).with_media(&OriginalMedia)).expect("planner");
        let p0 = planner.at_frame(0).expect("plan");
        let fps = p0.canvas.fps;
        let size = p0.size(case.width);
        println!("\n## {}\ncanvas {}x{} @ {}/{} render {}x{} layers {}", case.name, p0.canvas.width, p0.canvas.height, fps.num, fps.den, size.0, size.1, p0.layers.len());

        if case.name.starts_with("1 layer 720p25 -> 16:9 canvas, 960w") {
            let src = FrameSource::new(FrameSourceConfig::default());
            let plan = planner.at_frame(10).unwrap();
            let t0 = Instant::now();
            let (frame, tm) = comp.render_plan_texture_with(&plan, size, &src, Hint::Scrub).expect("render");
            let _ = frame.read_back(&gpu).unwrap();
            println!("first frame ever (new FrameSource: self-test + probes + spawn + decode): total {:.0} ms (decode {:.0}, composite record {:.1})", ms(t0.elapsed()), ms(tm.decode), ms(tm.composite));
            let plan = planner.at_frame(11).unwrap();
            let t0 = Instant::now();
            let (frame, tm) = comp.render_plan_texture_with(&plan, size, &src, Hint::Scrub).expect("render");
            let _ = frame.read_back(&gpu).unwrap();
            println!("second frame (warm run): total {:.0} ms (decode {:.1})", ms(t0.elapsed()), ms(tm.decode));
            src.release_all();
        }
        // --- planning cost per frame
        let mut plan_t = Vec::new();
        for k in 0..200u64 {
            let t0 = Instant::now();
            let _ = planner.at_frame(k).unwrap();
            plan_t.push(ms(t0.elapsed()));
        }
        println!("{}", Stats(plan_t).line("plan.at_frame (Planner reused)"));
        let mut plan_new = Vec::new();
        for _ in 0..50 {
            let t0 = Instant::now();
            let pl = Planner::new(&tl, &assets, &ExportOptions::default(), PlanRequest::still(policy).with_media(&OriginalMedia)).unwrap();
            let _ = pl.at(1.0).unwrap();
            plan_new.push(ms(t0.elapsed()));
        }
        println!("{}", Stats(plan_new).line("Planner::new + at (per-call, as the app does)"));

        // --- forward playback, free-running
        for hint_name in ["forward"] {
            let src = FrameSource::new(FrameSourceConfig::default());
            let n = 150u64;
            let (mut dec, mut rec, mut done, mut tot) = (vec![], vec![], vec![], vec![]);
            let cpu0 = cpu_secs();
            let wall0 = Instant::now();
            for k in 0..n {
                let plan = planner.at_frame(k).unwrap();
                let t0 = Instant::now();
                let (frame, tm) = comp
                    .render_plan_texture_with(&plan, size, &src, Hint::Forward { fps: f64::from(fps.num) / f64::from(fps.den) })
                    .expect("render");
                let t1 = Instant::now();
                let _ = frame.read_back(&gpu).expect("read back");
                let t2 = Instant::now();
                dec.push(ms(tm.decode));
                rec.push(ms(tm.composite));
                done.push(ms(t2 - t1));
                tot.push(ms(t2 - t0));
            }
            let wall = wall0.elapsed().as_secs_f64();
            let cpu = cpu_secs() - cpu0;
            println!("[{hint_name} free-run {n} frames] {:.1} fps wall, process CPU {:.1} ms/frame (parent only; ffmpeg children not counted)", n as f64 / wall, cpu * 1e3 / n as f64);
            println!("  {}", Stats(dec).line("decode wait (FrameSource, incl cache)"));
            println!("  {}", Stats(rec).line("composite record+submit (CPU)"));
            println!("  {}", Stats(done).line("GPU completion + readback (lavapipe = CPU raster)"));
            println!("  {}", Stats(tot).line("total/frame"));
            println!("  stats {:?}", src.stats());
            src.release_all();
        }
        // --- scrub: random jumps
        {
            let src = FrameSource::new(FrameSourceConfig::default());
            let n = 60u64;
            let (mut dec, mut rec, mut tot) = (vec![], vec![], vec![]);
            let mut k = 10u64;
            for i in 0..n {
                k = (k * 37 + 101 + i * 13) % 170 + 5;
                let plan = planner.at_frame(k).unwrap();
                let t0 = Instant::now();
                let (frame, tm) = comp.render_plan_texture_with(&plan, size, &src, Hint::Scrub).expect("render");
                let _ = frame.read_back(&gpu).expect("rb");
                dec.push(ms(tm.decode));
                rec.push(ms(tm.composite));
                tot.push(ms(t0.elapsed()));
            }
            println!("[scrub random jumps, {n} frames, after warm-up skip first]");
            println!("  {}", Stats(dec[1..].to_vec()).line("decode wait"));
            println!("  {}", Stats(rec[1..].to_vec()).line("composite record+submit"));
            println!("  {}", Stats(tot[1..].to_vec()).line("total"));
            println!("  stats {:?}", src.stats());
            src.release_all();
        }
        // --- cached (paused) re-render: same frame again
        {
            let src = FrameSource::new(FrameSourceConfig::default());
            let plan = planner.at_frame(50).unwrap();
            let _ = comp.render_plan_texture_with(&plan, size, &src, Hint::Scrub).unwrap();
            let (mut rec, mut tot) = (vec![], vec![]);
            for _ in 0..40 {
                let t0 = Instant::now();
                let (frame, tm) = comp.render_plan_texture_with(&plan, size, &src, Hint::Scrub).unwrap();
                rec.push(ms(tm.composite));
                let _ = frame.read_back(&gpu).unwrap();
                tot.push(ms(t0.elapsed()));
            }
            println!("[paused re-render of a cached frame]");
            println!("  {}", Stats(rec).line("composite record+submit"));
            println!("  {}", Stats(tot).line("total incl completion"));
            src.release_all();
        }
    }
}
