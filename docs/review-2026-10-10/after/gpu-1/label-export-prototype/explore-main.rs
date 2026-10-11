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
    let proxy = generate_proxy(Path::new(&a.path), proxy_width(a.projection())).expect("proxy");
    let mut p = a.clone();
    p.path = proxy.to_string_lossy().into_owned();
    for s in p.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
        if s.color_transfer.as_deref() == Some("arib-std-b67") || s.color_transfer.as_deref() == Some("smpte2084") {
            s.color_space = Some("bt709".into());
        }
        if s.color_transfer.as_deref() == Some("arib-std-b67") || s.color_transfer.as_deref() == Some("smpte2084") {
            s.color_transfer = None;
            s.color_primaries = None;
        }
    }
    p
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(&a[1]);
    let out = PathBuf::from(&a[2]);
    std::fs::create_dir_all(&out).unwrap();
    let probe = |n: &str| Project::probe_asset(&dir.join(n)).unwrap();
    let u = probe("u.mp4"); // untagged HD
    let t = probe("t.mp4"); // the same, labelled bt709
    let b = probe("b709.mp4"); // BT.709 tagged detail
    let sd = probe("sd601.mp4"); // SD smpte170m
    let hlg = probe("hlg.mp4");
    let sdu = probe("sdu.mp4");
    let logo = probe("logo.png");
    // (name, builder given the "top" asset and the other)
    type Build = Box<dyn Fn(&Asset, &Asset) -> (Timeline, Vec<Asset>)>;
    let mut cases: Vec<(&str, Build)> = Vec::new();
    cases.push(("single-opaque", Box::new(|top, _| (tl(vec![vec![clip(top, |_| {})]], vec![]), vec![top.clone()]))));
    cases.push(("single-pip", Box::new(|top, _| (tl(vec![vec![clip(top, pip)]], vec![]), vec![top.clone()]))));
    cases.push(("single-opacity", Box::new(|top, _| (tl(vec![vec![clip(top, |c| c.transform.opacity = 0.6)]], vec![]), vec![top.clone()]))));
    cases.push(("single-pip-opacity", Box::new(|top, _| (tl(vec![vec![clip(top, |c| { pip(c); c.transform.opacity = 0.6 })]], vec![]), vec![top.clone()]))));
    cases.push(("single-colour", Box::new(|top, _| (tl(vec![vec![clip(top, |c| c.color = Color { saturation: 1.4, brightness: 0.05, ..Color::default() })]], vec![]), vec![top.clone()]))));
    cases.push(("single-text", Box::new(|top, _| {
        let mut o = TextOverlay::new("Colour", 0.0, 1.0);
        o.color = "#ff2010".into();
        o.pos_y = 0.5;
        (tl(vec![vec![clip(top, |_| {})]], vec![o]), vec![top.clone()])
    })));
    cases.push(("single-rotate", Box::new(|top, _| (tl(vec![vec![clip(top, |c| { pip(c); c.transform.rotation = 12.0 })]], vec![]), vec![top.clone()]))));
    cases.push(("single-fade", Box::new(|top, _| (tl(vec![vec![clip(top, |c| c.fade_in = 0.6)]], vec![]), vec![top.clone()]))));
    cases.push(("single-mask", Box::new(|top, _| (tl(vec![vec![clip(top, |c| c.mask = Some(serde_json::from_str::<Mask>("{}").unwrap()))]], vec![]), vec![top.clone()]))));
    cases.push(("single-grayscale", Box::new(|top, _| (tl(vec![vec![clip(top, |c| c.effects.push(VideoEffect::Vignette))]], vec![]), vec![top.clone()]))));
    cases.push(("mixed-over-709", Box::new(|top, other| (tl(vec![vec![clip(other, |_| {})], vec![clip(top, pip)]], vec![]), vec![other.clone(), top.clone()]))));
    cases.push(("mixed-under-709", Box::new(|top, other| (tl(vec![vec![clip(top, |_| {})], vec![clip(other, pip)]], vec![]), vec![top.clone(), other.clone()]))));
    cases.push(("mixed-over-709-opacity", Box::new(|top, other| (tl(vec![vec![clip(other, |_| {})], vec![clip(top, |c| { pip(c); c.transform.opacity = 0.6 })]], vec![]), vec![other.clone(), top.clone()]))));
    let h2 = hlg.clone();
    cases.push(("mixed-over-hlg", Box::new(move |top, _| (tl(vec![vec![clip(&h2, |_| {})], vec![clip(top, pip)]], vec![]), vec![h2.clone(), top.clone()]))));
    let h3 = hlg.clone();
    cases.push(("mixed-under-hlg", Box::new(move |top, _| (tl(vec![vec![clip(top, |_| {})], vec![clip(&h3, pip)]], vec![]), vec![top.clone(), h3.clone()]))));
    let (l1, l2) = (logo.clone(), logo.clone());
    cases.push(("mixed-hd-under-png", Box::new(move |top, _| (tl(vec![vec![clip(top, |_| {})], vec![clip(&l1, |c| { pip(c); c.source_out = 1.0 })]], vec![]), vec![top.clone(), l1.clone()]))));
    cases.push(("mixed-png-under-hd", Box::new(move |top, _| (tl(vec![vec![clip(&l2, |c| c.source_out = 1.0)], vec![clip(top, pip)]], vec![]), vec![l2.clone(), top.clone()]))));
    let (s1, s2) = (sdu.clone(), sdu.clone());
    cases.push(("mixed-hd-over-sd-untagged", Box::new(move |top, _| (tl(vec![vec![clip(&s1, |_| {})], vec![clip(top, pip)]], vec![]), vec![s1.clone(), top.clone()]))));
    cases.push(("mixed-hd-under-sd-untagged", Box::new(move |top, _| (tl(vec![vec![clip(top, |_| {})], vec![clip(&s2, pip)]], vec![]), vec![top.clone(), s2.clone()]))));
    let b2 = b.clone();
    cases.push(("sequential-709-then-hd", Box::new(move |top, _| {
        let first = clip(&b2, |c| { c.source_out = 0.3; });
        let second = clip(top, |c| { c.source_in = 0.3; c.source_out = 1.0; c.timeline_start = 0.3; });
        (tl(vec![vec![first, second]], vec![]), vec![b2.clone(), top.clone()])
    })));
    let sd2 = sd.clone();
    cases.push(("mixed-over-sd601", Box::new(move |top, _| (tl(vec![vec![clip(&sd2, |_| {})], vec![clip(top, pip)]], vec![]), vec![sd2.clone(), top.clone()]))));
    let only = std::env::var("ONLY").ok();
    for (name, build) in &cases {
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        let preview = std::env::var("PREVIEW").is_ok();
        for (label, top) in [("U", &u), ("T", if std::env::var("PREVIEW").is_ok() { &u } else { &t })] {
            let (timeline, mut assets) = build(top, &b);
            if preview {
                // U = the originals, T = the preview's assets (proxy files, the original's metadata)
                if label == "T" {
                    assets = assets.iter().map(proxied).collect();
                }
            }
            let png = out.join(format!("{name}-{label}.png"));
            export_still(&timeline, &assets, &ExportOptions::default(), 0.5, &png, ImageFormat::Png, 0).expect("still");
            let jpg = Project::composite_timeline_frame(&timeline, &assets, 0.5, 640, 4).expect("jpeg still");
            std::fs::write(out.join(format!("{name}-{label}.jpg")), jpg).unwrap();
            let mp4 = out.join(format!("{name}-{label}.mp4"));
            let opts = ExportOptions {
                rate_control: RateControl::Lossless,
                video_codec: Some("libx264".into()),
                fps: Some(10.0),
                ..ExportOptions::default()
            };
            render_with(&timeline, &assets, &mp4, &opts).expect("export");
        }
        println!("{name}");
    }
    println!("policy {:?}", composite_color_policy());
    let _ = Path::new("");
}
