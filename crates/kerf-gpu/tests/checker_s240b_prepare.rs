//! `checker/s240b`, the render bar's caveat: `prepare_timeline` sees a timeline's clips and not their tags, so a cut of SMPTE 240M footage is
//! judged by a compositor whose round trip may not have been measured yet. Nothing may be **drawn wrong** then; a frame refused is FFmpeg's and
//! fine. One test, because each probe's answer is cached once per process: a fresh process is what "unmeasured" is.
//!
//! The same plans are put to a compositor that has measured nothing (its caps, its render, the frame the render hands out if it hands one out),
//! then after a timeline that holds only a translucent *untagged* clip was prepared (which measures the round trip without ever seeing a 240M
//! tag), then after `prepare` was given the plan itself.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_s240b_prepare -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use kerf_core::{
    Asset, Clip, CompositeColorPolicy, ExportOptions, ImageFormat, Project, RenderPlan, StreamKind, Timeline, Track,
};
use kerf_gpu::roundtrip_probe::{self, Translucency};
use kerf_gpu::{Compositor, Gpu, GpuOptions};

fn make(dir: &Path, name: &str, graph: &str) -> Asset {
    let path = dir.join(name);
    let made = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", graph])
        .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .expect("run ffmpeg");
    assert!(made.success(), "ffmpeg made {name}");
    Project::probe_asset(&path).expect("probe")
}

fn timeline(layers: &[(&Asset, f64)]) -> Timeline {
    Timeline {
        tracks: layers
            .iter()
            .enumerate()
            .map(|(i, (a, opacity))| {
                let mut c = Clip::new(a.id, 0.0, 0.5, 0.0);
                c.transform.opacity = *opacity;
                Track {
                    clips: vec![c],
                    ..Track::new(StreamKind::Video, format!("V{}", i + 1))
                }
            })
            .collect(),
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

#[derive(Debug)]
#[allow(dead_code)]
enum Outcome {
    /// The plan or the render refuses.
    Refused(String),
    /// Drawn, and FFmpeg's to the sample (the planes, and the RGB the render handed out if it handed any).
    Exact,
    /// Drawn, and not FFmpeg's.
    Differs(String),
}

fn ffmpeg_pipe(args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// What the compositor does with the stack `tl`, judged against FFmpeg's still.
fn judge(compositor: &Compositor, dir: &Path, tl: &Timeline, assets: &[Asset], policy: CompositeColorPolicy) -> Outcome {
    let opts = ExportOptions::default();
    let plan = RenderPlan::at(tl, assets, &opts, 0.25, policy).expect("plan");
    let size = plan.size(u32::MAX);
    let plan = plan.for_render(size);
    let (w, h) = (size.0 as usize, size.1 as usize);
    let why: Vec<String> = plan
        .reasons(&compositor.caps(), size)
        .iter()
        .map(ToString::to_string)
        .collect();
    // The render says no for itself when the plan did.
    let rendered = compositor.render_plan(&plan, size);
    let mut figures = Vec::new();
    if let Ok((frame, _)) = &rendered {
        let png = dir.join("reference.png");
        kerf_core::export_still(tl, assets, &opts, 0.25, &png, ImageFormat::Png, 0).expect("FFmpeg still");
        let reference = {
            let out = Command::new(kerf_core::ffmpeg_path())
                .args(["-hide_banner", "-loglevel", "error", "-i"])
                .arg(&png)
                .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
                .output()
                .expect("ffmpeg");
            out.stdout
        };
        let rgb: Vec<u8> = frame
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        let differ = rgb.iter().zip(&reference).filter(|(a, b)| a != b).count();
        figures.push(format!(
            "the RGB the render handed out: {differ} samples differ from the still"
        ));
        if differ != 0 {
            return Outcome::Differs(figures.join("; "));
        }
    }
    if !why.is_empty() {
        return match rendered {
            Err(e) => Outcome::Refused(format!("plan: {}; render: {e}", why.join("; "))),
            Ok(_) => Outcome::Differs(format!(
                "the plan refuses ({}) and the render draws it all the same, and it is FFmpeg's RGB: {}",
                why.join("; "),
                figures.join("; ")
            )),
        };
    }
    let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
    let planes = compositor.composite_yuv(&plan, &frames, size).expect("the GPU's planes");
    let (_, _, want) =
        kerf_core::timeline_frame_yuv_cancellable(tl, assets, &opts, 0.25, size.0, &|| false).expect("the still's planes");
    let raw: Vec<u8> = [&planes.y, &planes.u, &planes.v].into_iter().flatten().copied().collect();
    let got = ffmpeg_pipe(
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{w}x{h}"),
            "-i",
            "pipe:0",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv444p",
            "pipe:1",
        ],
        &raw,
    );
    assert_eq!(got.len(), want.len());
    let differing = got.iter().zip(&want).filter(|(a, b)| a != b).count();
    if differing == 0 {
        Outcome::Exact
    } else {
        Outcome::Differs(format!("{differing} samples of the planes differ from the still's"))
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_s240b_prepare_a_240m_cut_is_never_drawn_wrong_before_it_is_measured() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("s240b-prepare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let geq =
        "nullsrc=size=320x180:rate=30:duration=1,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'";
    let a240 = make(&dir, "a240.mkv", &format!("{geq},setparams=colorspace=smpte240m:range=tv"));
    let b240 = make(
        &dir,
        "b240.mkv",
        &format!("{geq},geq=lum='255-lum(X,Y)':cb='cb(X,Y)':cr='255-cr(X,Y)',setparams=colorspace=smpte240m:range=tv"),
    );
    let none = make(
        &dir,
        "none.mkv",
        &format!("{geq},setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown"),
    );
    assert_eq!(a240.streams[0].color_space.as_deref(), Some("smpte240m"));
    let policy = kerf_core::composite_color_policy();
    assert_ne!(policy, CompositeColorPolicy::Unknown);
    assert_eq!(
        roundtrip_probe::measured(),
        Translucency::Unknown,
        "a probe ran before the test asked"
    );

    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let compositor = Compositor::new(gpu).expect("a compositor");
    let cases: Vec<(&str, Vec<(&Asset, f64)>)> = vec![
        ("an opaque 240M picture", vec![(&a240, 1.0)]),
        ("a translucent 240M picture", vec![(&a240, 0.5)]),
        ("a translucent 240M picture over 240M", vec![(&b240, 1.0), (&a240, 0.6)]),
        ("a translucent 240M picture over untagged", vec![(&none, 1.0), (&a240, 0.6)]),
        ("a translucent untagged picture over 240M", vec![(&a240, 1.0), (&none, 0.6)]),
        ("an opaque 240M picture over untagged", vec![(&none, 1.0), (&a240, 1.0)]),
    ];
    let mut report = Vec::new();
    let mut bad = Vec::new();
    let mut run = |phase: &str, report: &mut Vec<String>| {
        for (name, layers) in &cases {
            let tl = timeline(layers);
            let assets: Vec<Asset> = layers.iter().map(|(a, _)| (*a).clone()).collect();
            let outcome = judge(&compositor, &dir, &tl, &assets, policy);
            eprintln!("S240B prepare [{phase}] {name}: {outcome:?}");
            report.push(format!("[{phase}] {name}: {outcome:?}"));
            if let Outcome::Differs(why) = &outcome {
                bad.push(format!("[{phase}] {name}: {why}"));
            }
        }
    };

    // Nothing measured.
    run("unmeasured", &mut report);
    assert_eq!(
        roundtrip_probe::measured(),
        Translucency::Unknown,
        "drawing measured the round trip"
    );
    assert!(
        !compositor.caps().smpte240m,
        "the caps hold 240M before the round trip was measured"
    );

    // A timeline of one translucent untagged clip: the round trip is measured, the tag never seen.
    kerf_gpu::prepare_timeline(&timeline(&[(&none, 0.5)]));
    assert_ne!(
        roundtrip_probe::measured(),
        Translucency::Unknown,
        "a translucent clip left the round trip unmeasured"
    );
    let matched = roundtrip_probe::measured() == Translucency::Match;
    assert_eq!(compositor.caps().smpte240m, matched, "the cap is the round trip's verdict");
    run("round trip measured by an untagged clip", &mut report);

    // `prepare` with a plan that holds the tag.
    for (_, layers) in &cases {
        let tl = timeline(layers);
        let assets: Vec<Asset> = layers.iter().map(|(a, _)| (*a).clone()).collect();
        let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 0.25, policy).expect("plan");
        kerf_gpu::prepare(&plan);
    }
    run("prepared", &mut report);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(bad.is_empty(), "drawn and not FFmpeg's:\n{}", bad.join("\n"));
}
