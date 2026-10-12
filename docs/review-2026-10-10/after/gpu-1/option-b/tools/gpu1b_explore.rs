// Measurement harness for GPU-1 option B (not built by the workspace). Copy into crates/kerf-core/examples/ and run:
//   cargo build -p kerf-core --no-default-features --example gpu1b_explore
//   gpu1b_explore <media dir from mkmedia.sh> <out dir> <subject: u.mp4 (untagged) | t.mp4 (tagged bt709)>
// PLANONLY=1 prints only the GPU plan's verdicts ("PLAN case orig|prev policy drawn|refused: why") per policy.
// Compare two builds (main and the label) by running it from each; `cmp.py` tabulates PSNR / worst level error.
use kerf_core::*;
use std::path::{Path, PathBuf};

fn tl(tracks: Vec<Vec<Clip>>, overlays: Vec<TextOverlay>) -> Timeline {
    Timeline {
        tracks: tracks
            .into_iter()
            .enumerate()
            .map(|(i, clips)| Track { clips, ..Track::new(StreamKind::Video, format!("V{}", i + 1)) })
            .collect(),
        overlays,
        markers: Vec::new(),
        format: Some(Delivery::new(640, 360, Fit::Contain)),
        master: Default::default(),
    }
}

fn clip(a: &Asset, f: impl FnOnce(&mut Clip)) -> Clip {
    let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
    f(&mut c);
    c
}

fn pip(c: &mut Clip) {
    c.transform = Transform { scale: 0.5, pos_x: 0.2, pos_y: -0.1, ..Transform::default() };
}

fn proxied(a: &Asset) -> Asset {
    if a.is_image() {
        return a.clone();
    }
    generate_proxy(Path::new(&a.path), proxy_width(a.projection())).expect("proxy");
    Project::preview_assets_of(vec![a.clone()]).remove(0)
}

type Build = Box<dyn Fn(&Asset) -> (Timeline, Vec<Asset>)>;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(&a[1]);
    let out = PathBuf::from(&a[2]);
    let subject = a[3].clone();
    std::fs::create_dir_all(&out).unwrap();
    let probe = |n: &str| Project::probe_asset(&dir.join(n)).unwrap();
    let top = probe(&subject);
    let b = probe("b709.mp4");
    let sd601 = probe("sd601.mp4");
    let hd601 = probe("hd601.mp4");
    let hlg = probe("hlg.mp4");
    let sdu = probe("sdu.mp4");
    let logo = probe("logo.png");
    let rgb = probe("rgb.mkv");
    let mjpeg = probe("mjpeg.mkv");
    let alpha = probe("alpha.mkv");
    let mut cases: Vec<(&str, Build)> = Vec::new();
    cases.push(("single-opaque", Box::new(|t| (tl(vec![vec![clip(t, |_| {})]], vec![]), vec![t.clone()]))));
    cases.push(("single-pip", Box::new(|t| (tl(vec![vec![clip(t, pip)]], vec![]), vec![t.clone()]))));
    cases.push(("single-opacity", Box::new(|t| (tl(vec![vec![clip(t, |c| c.transform.opacity = 0.6)]], vec![]), vec![t.clone()]))));
    cases.push(("single-text", Box::new(|t| {
        let mut o = TextOverlay::new("Colour", 0.0, 1.0);
        o.color = "#ff2010".into();
        o.pos_y = 0.5;
        (tl(vec![vec![clip(t, |_| {})]], vec![o]), vec![t.clone()])
    })));
    let b1 = b.clone();
    cases.push(("mixed-over-709", Box::new(move |t| (tl(vec![vec![clip(&b1, |_| {})], vec![clip(t, pip)]], vec![]), vec![b1.clone(), t.clone()]))));
    let b1 = b.clone();
    cases.push(("mixed-under-709", Box::new(move |t| (tl(vec![vec![clip(t, |_| {})], vec![clip(&b1, pip)]], vec![]), vec![t.clone(), b1.clone()]))));
    let b1 = b.clone();
    cases.push(("mixed-over-709-opacity", Box::new(move |t| (tl(vec![vec![clip(&b1, |_| {})], vec![clip(t, |c| { pip(c); c.transform.opacity = 0.6 })]], vec![]), vec![b1.clone(), t.clone()]))));
    let b1 = b.clone();
    cases.push(("mixed-709-opacity-over", Box::new(move |t| (tl(vec![vec![clip(t, |_| {})], vec![clip(&b1, |c| { pip(c); c.transform.opacity = 0.6 })]], vec![]), vec![t.clone(), b1.clone()]))));
    let h = hlg.clone();
    cases.push(("mixed-over-hlg", Box::new(move |t| (tl(vec![vec![clip(&h, |_| {})], vec![clip(t, pip)]], vec![]), vec![h.clone(), t.clone()]))));
    let h = hlg.clone();
    cases.push(("mixed-under-hlg", Box::new(move |t| (tl(vec![vec![clip(t, |_| {})], vec![clip(&h, pip)]], vec![]), vec![t.clone(), h.clone()]))));
    let l = logo.clone();
    cases.push(("png-over-hd", Box::new(move |t| (tl(vec![vec![clip(t, |_| {})], vec![clip(&l, |c| { pip(c); c.source_out = 1.0 })]], vec![]), vec![t.clone(), l.clone()]))));
    let l = logo.clone();
    cases.push(("hd-over-png", Box::new(move |t| (tl(vec![vec![clip(&l, |c| c.source_out = 1.0)], vec![clip(t, pip)]], vec![]), vec![l.clone(), t.clone()]))));
    let s = sdu.clone();
    cases.push(("hd-over-sd-untagged", Box::new(move |t| (tl(vec![vec![clip(&s, |_| {})], vec![clip(t, pip)]], vec![]), vec![s.clone(), t.clone()]))));
    let s = sdu.clone();
    cases.push(("hd-under-sd-untagged", Box::new(move |t| (tl(vec![vec![clip(t, |_| {})], vec![clip(&s, pip)]], vec![]), vec![t.clone(), s.clone()]))));
    let s = sd601.clone();
    cases.push(("hd-over-sd601", Box::new(move |t| (tl(vec![vec![clip(&s, |_| {})], vec![clip(t, pip)]], vec![]), vec![s.clone(), t.clone()]))));
    let s = hd601.clone();
    cases.push(("hd-over-hd601", Box::new(move |t| (tl(vec![vec![clip(&s, |_| {})], vec![clip(t, pip)]], vec![]), vec![s.clone(), t.clone()]))));
    let s = rgb.clone();
    cases.push(("hd-over-rgb", Box::new(move |t| (tl(vec![vec![clip(&s, |_| {})], vec![clip(t, pip)]], vec![]), vec![s.clone(), t.clone()]))));
    let s = mjpeg.clone();
    let b1 = b.clone();
    cases.push(("mjpeg-over-709", Box::new(move |_| (tl(vec![vec![clip(&b1, |_| {})], vec![clip(&s, pip)]], vec![]), vec![b1.clone(), s.clone()]))));
    let s = alpha.clone();
    let b1 = b.clone();
    cases.push(("alpha-over-709", Box::new(move |_| (tl(vec![vec![clip(&b1, |_| {})], vec![clip(&s, pip)]], vec![]), vec![b1.clone(), s.clone()]))));
    let b1 = b.clone();
    cases.push(("sequential-709-then-hd", Box::new(move |t| {
        let first = clip(&b1, |c| { c.source_out = 0.3; });
        let second = clip(t, |c| { c.source_in = 0.3; c.source_out = 1.0; c.timeline_start = 0.3; });
        (tl(vec![vec![first, second]], vec![]), vec![b1.clone(), t.clone()])
    })));
    let only = std::env::var("ONLY").ok();
    for (name, build) in &cases {
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        for kind in ["orig", "prev"] {
            let (timeline, mut assets) = build(&top);
            if kind == "prev" {
                assets = assets.iter().map(proxied).collect();
            }
            for policy in [CompositeColorPolicy::BottomLayerTag, CompositeColorPolicy::FixedBt601, CompositeColorPolicy::Unknown] {
                let plan = RenderPlan::at(&timeline, &assets, &ExportOptions::default(), 0.5, policy).expect("plan");
                let why: Vec<String> = plan.unsupported_reasons_at(plan.size(u32::MAX));
                println!("PLAN {name} {kind} {policy:?} {}", if why.is_empty() { "drawn".to_string() } else { format!("refused: {}", why.join("; ").chars().take(70).collect::<String>()) });
            }
            if std::env::var_os("PLANONLY").is_some() {
                continue;
            }
            let png = out.join(format!("{name}-{kind}.png"));
            export_still(&timeline, &assets, &ExportOptions::default(), 0.5, &png, ImageFormat::Png, 0).expect("still");
            let jpg = Project::composite_timeline_frame(&timeline, &assets, 0.5, 640, 2).expect("jpeg still");
            std::fs::write(out.join(format!("{name}-{kind}.jpg")), jpg).unwrap();
            let mp4 = out.join(format!("{name}-{kind}.mp4"));
            let opts = ExportOptions {
                rate_control: RateControl::Lossless,
                video_codec: Some("libx264".into()),
                fps: Some(10.0),
                ..ExportOptions::default()
            };
            render_with(&timeline, &assets, &mp4, &opts).expect("export");
            let mut first: Option<Vec<u8>> = None;
            let _ = stream_preview(&timeline, &assets, 0.0, 10.0, &mut |f| {
                first = Some(f.jpeg);
                false
            });
            if let Some(j) = first {
                std::fs::write(out.join(format!("{name}-{kind}.stream.jpg")), j).unwrap();
            }
        }
        println!("{name}");
    }
    println!("policy {:?}", composite_color_policy());
}
