//! Lane B round five, the two things that need a process of their own (a probe that has not run, an FFmpeg that is a script):
//! a render never runs a probe and `prepare` opens the dip to white (R2), and a killed child whose grandchild holds its pipes is
//! not waited for (R7). Each test starts this binary again with the state it needs and reads what the inner test printed.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_b_r5_process -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kerf_core::{Asset, Clip, ExportOptions, Project, StreamKind, Timeline, Track, Transition, TransitionKind};
use kerf_gpu::{Compositor, Gpu, GpuOptions};

const INNER: &str = "KERF_CHECKER_R5_INNER";

fn scratch() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-b-r5-process");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// This test binary run again on the one test `name`, with `env`; its output.
fn rerun(name: &str, env: &[(&str, &str)]) -> (std::process::ExitStatus, String, Duration) {
    let mut cmd = Command::new(std::env::current_exe().expect("this binary"));
    cmd.args(["--ignored", "--exact", name, "--nocapture", "--test-threads=1"])
        .env(INNER, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let started = Instant::now();
    let out = cmd.output().expect("run this binary");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status, text, started.elapsed())
}

fn inner() -> bool {
    std::env::var_os(INNER).is_some()
}

fn make_clip(path: &Path, seed: u32) -> Asset {
    if !path.exists() {
        let tmp = path.with_extension("tmp.mkv");
        let lavfi = format!(
            "nullsrc=size=64x36:rate=30:duration=0.8,format=yuv420p,\
             geq=lum='mod(random(1)*255+{o},256)':cb='mod(random(2)*255+{o},256)':cr='mod(random(3)*255+{o},256)'",
            o = seed * 53
        );
        let status = Command::new(kerf_core::ffmpeg_path())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &lavfi])
            .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p"])
            .arg(&tmp)
            .stdin(Stdio::null())
            .status()
            .expect("run ffmpeg");
        assert!(status.success());
        std::fs::rename(&tmp, path).unwrap();
    }
    Project::probe_asset(path).expect("probe")
}

fn yuv444_of(drawn: &kerf_gpu::YuvFrame, (w, h): (u32, u32)) -> Vec<u8> {
    use std::io::Write;
    let mut raw = drawn.y.clone();
    raw.extend_from_slice(&drawn.u);
    raw.extend_from_slice(&drawn.v);
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
        ])
        .args([
            "-s",
            &format!("{w}x{h}"),
            "-i",
            "pipe:0",
            "-filter_complex",
            "[0:v]format=yuv444p[o]",
        ])
        .args(["-map", "[o]", "-frames:v", "1", "-f", "rawvideo", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&raw);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    out.stdout
}

/// A render reads what was measured and never runs a probe: before `prepare` the grade's conversions are unknown, so the dip to
/// white is not in the caps and the plan refuses it, and drawing it anyway is `Unsupported` (and still no probe has run); after
/// `prepare` it is drawn, and the still equals FFmpeg's to the sample.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); starts this binary again"]
fn checker_b_r5_a_render_never_probes_and_prepare_opens_the_dip() {
    if !inner() {
        let (status, text, _) = rerun("checker_b_r5_a_render_never_probes_and_prepare_opens_the_dip", &[]);
        eprintln!("{text}");
        assert!(status.success(), "the inner run failed:\n{text}");
        assert!(text.contains("R5 PROCESS DONE"), "{text}");
        return;
    }
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let comp = Compositor::new(gpu).expect("a compositor");
    let dir = scratch();
    let a = make_clip(&dir.join("a.mkv"), 1);
    let b = make_clip(&dir.join("b.mkv"), 2);
    let mut dipped = Clip::new(b.id, 0.0, 0.8, 0.4);
    dipped.transition_in = Some(Transition {
        kind: TransitionKind::DipToWhite,
        duration: 0.3,
    });
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![Clip::new(a.id, 0.0, 0.4, 0.0), dipped],
            ..Track::new(StreamKind::Video, "V1")
        }],
        ..Timeline::new()
    };
    let assets = [a, b];
    let opts = ExportOptions::default();
    let policy = kerf_core::composite_color_policy();
    // (`composite_color_policy` is the core's own probe; the GPU's have not run.)
    assert_eq!(
        kerf_gpu::grade::measured(),
        kerf_gpu::grade::Kernels::Unknown,
        "a probe ran before the test asked"
    );
    assert_eq!(kerf_gpu::scaler::measured(), kerf_gpu::scaler::Kernels::Unknown);
    assert!(!comp.caps().dip_white, "the dip is in the caps before it was measured");
    assert!(!comp.caps().exact_scaler);
    let times = [0.32, 0.4, 0.45, 0.55];
    let mut checked = 0;
    for t in times {
        let plan = kerf_core::RenderPlan::at(&tl, &assets, &opts, t, policy).expect("plan");
        let size = plan.size(u32::MAX);
        let why: Vec<String> = plan.reasons(&comp.caps(), size).iter().map(ToString::to_string).collect();
        assert!(why.iter().any(|r| r.contains("a dip to white")), "t={t}: {why:?}");
        let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
        let err = comp
            .composite_yuv(&plan, &frames, size)
            .expect_err("the render drew a frame its plan refuses");
        assert!(matches!(err, kerf_gpu::GpuError::Unsupported(_)), "{err}");
        assert_eq!(
            kerf_gpu::grade::measured(),
            kerf_gpu::grade::Kernels::Unknown,
            "t={t}: the render ran the probe"
        );
        assert_eq!(
            kerf_gpu::scaler::measured(),
            kerf_gpu::scaler::Kernels::Unknown,
            "t={t}: the render ran the scaler probe"
        );
        // `prepare` runs what the plan needs, off the render, once.
        let started = Instant::now();
        kerf_gpu::prepare(&plan);
        eprintln!("prepare at t={t}: {:?}", started.elapsed());
        assert_eq!(
            kerf_gpu::grade::measured(),
            kerf_gpu::grade::Kernels::Match,
            "prepare did not measure the grade's conversions"
        );
        assert!(
            matches!(kerf_gpu::scaler::measured(), kerf_gpu::scaler::Kernels::Match(_)),
            "prepare did not measure the scaler"
        );
        assert!(comp.caps().dip_white);
        let why: Vec<String> = plan.reasons(&comp.caps(), size).iter().map(ToString::to_string).collect();
        assert!(why.is_empty(), "t={t}: {why:?}");
        let drawn = comp
            .composite_yuv(&plan, &frames, size)
            .expect("the dip is drawn after prepare");
        let (w, h, want) =
            kerf_core::timeline_frame_yuv_cancellable(&tl, &assets, &opts, t, size.0, &|| false).expect("the still");
        assert_eq!((w, h), size);
        let got = yuv444_of(&drawn, size);
        assert_eq!(want.len(), got.len());
        let differing = want.iter().zip(&got).filter(|(x, y)| x != y).count();
        assert_eq!(differing, 0, "t={t}: {differing} samples differ");
        checked += 1;
        // The next time of the loop starts from the measured state, so the "before" asserts above are for the first only.
        if checked == 1 {
            break;
        }
    }
    // The remaining times, measured now, must be drawn exactly with no further prepare.
    for t in &times[1..] {
        let plan = kerf_core::RenderPlan::at(&tl, &assets, &opts, *t, policy).expect("plan");
        let size = plan.size(u32::MAX);
        assert!(plan.reasons(&comp.caps(), size).is_empty(), "t={t}");
        let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
        let drawn = comp.composite_yuv(&plan, &frames, size).expect("drawn");
        let (_, _, want) = kerf_core::timeline_frame_yuv_cancellable(&tl, &assets, &opts, *t, size.0, &|| false).expect("still");
        assert!(want == yuv444_of(&drawn, size), "t={t}: not exact");
    }
    eprintln!("R5 PROCESS DONE");
}

/// A script for an FFmpeg: leaves a grandchild holding its pipes and floods stdout, so the probe's child overflows its cap and is
/// killed. The probe must give its answer when the child is killed, not when the grandchild lets go of the pipes.
#[cfg(unix)]
#[test]
#[ignore = "starts this binary again with a script for an FFmpeg"]
fn checker_b_r5_a_killed_child_whose_grandchild_holds_the_pipes_is_not_waited_for() {
    use std::os::unix::fs::PermissionsExt;
    let name = "checker_b_r5_a_killed_child_whose_grandchild_holds_the_pipes_is_not_waited_for";
    if !inner() {
        let script = scratch().join("fake-ffmpeg.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 9 &\nexec yes\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (status, text, took) = rerun(name, &[("KERF_FFMPEG", script.to_str().unwrap())]);
        eprintln!("{text}\nthe inner run took {took:?}");
        assert!(status.success(), "{text}");
        assert!(
            took < Duration::from_secs(6),
            "the probe waited for the grandchild's pipes: {took:?}"
        );
        return;
    }
    let started = Instant::now();
    let scaler = kerf_gpu::scaler::kernels();
    let convert = kerf_gpu::convert::kernels();
    eprintln!(
        "probes against the script: scaler {scaler:?}, convert {convert:?} in {:?}",
        started.elapsed()
    );
    assert!(!scaler.exact(), "{scaler:?}");
    assert!(!convert.converts(), "{convert:?}");
}

/// Prints the FFmpeg graph of a dip to white on a clip with an effect, for reading the graph the effect sits in (a diagnostic:
/// nothing is judged). The export runs through a script that logs its arguments and then runs the real FFmpeg.
#[cfg(unix)]
#[test]
#[ignore = "a diagnostic: prints the export graph of a dip to white through an effect"]
fn checker_b_r5_print_the_graph_of_a_dip_through_an_effect() {
    use std::os::unix::fs::PermissionsExt;
    let name = "checker_b_r5_print_the_graph_of_a_dip_through_an_effect";
    if !inner() {
        let dir = scratch();
        let real = kerf_core::ffmpeg_path();
        let log = dir.join("argv.log");
        let _ = std::fs::remove_file(&log);
        let script = dir.join("logging-ffmpeg.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexec {} \"$@\"\n",
                log.display(),
                real
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (status, text, _) = rerun(name, &[("KERF_FFMPEG", script.to_str().unwrap())]);
        assert!(status.success(), "{text}");
        for line in std::fs::read_to_string(&log).unwrap_or_default().lines() {
            if let Some(i) = line.find("-filter_complex ") {
                let graph = &line[i + 16..];
                let graph = graph.split(" -map ").next().unwrap_or(graph);
                eprintln!("GRAPH {graph}");
            }
        }
        return;
    }
    use kerf_core::VideoEffect as E;
    let dir = scratch();
    let a = make_clip(&dir.join("a.mkv"), 1);
    let b = make_clip(&dir.join("b.mkv"), 2);
    for (label, effect) in [
        ("invert", E::Invert),
        ("vignette", E::Vignette),
        ("blur", E::Blur { sigma: 1.5 }),
    ] {
        let mut dipped = Clip::new(b.id, 0.0, 0.8, 0.8);
        dipped.transition_in = Some(Transition {
            kind: TransitionKind::DipToWhite,
            duration: 0.4,
        });
        dipped.effects = vec![effect];
        let tl = Timeline {
            tracks: vec![Track {
                clips: vec![Clip::new(a.id, 0.0, 0.8, 0.0), dipped],
                ..Track::new(StreamKind::Video, "V1")
            }],
            ..Timeline::new()
        };
        let out = dir.join(format!("graph-{label}.mkv"));
        kerf_core::render_with(&tl, &[a.clone(), b.clone()], &out, &ExportOptions::default()).expect("render");
        let _ = std::fs::remove_file(&out);
    }
}

/// The same through the deadline: an FFmpeg that says nothing and does not finish (its grandchild holds the pipes for a minute) is
/// killed at the probe's deadline, and the probe answers then, not when the grandchild lets go.
#[cfg(unix)]
#[test]
#[ignore = "starts this binary again with a script for an FFmpeg; takes the probe's 20 s deadline"]
fn checker_b_r5_a_child_killed_at_its_deadline_whose_grandchild_holds_the_pipes_is_not_waited_for() {
    use std::os::unix::fs::PermissionsExt;
    let name = "checker_b_r5_a_child_killed_at_its_deadline_whose_grandchild_holds_the_pipes_is_not_waited_for";
    if !inner() {
        let script = scratch().join("silent-ffmpeg.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 75 &\nexec sleep 100\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (status, text, took) = rerun(name, &[("KERF_FFMPEG", script.to_str().unwrap())]);
        eprintln!("{text}\nthe inner run took {took:?}");
        assert!(status.success(), "{text}");
        assert!(
            took < Duration::from_secs(50),
            "the probe waited for the grandchild's pipes: {took:?}"
        );
        return;
    }
    let started = Instant::now();
    let scaler = kerf_gpu::scaler::kernels();
    eprintln!("the scaler probe against the script: {scaler:?} in {:?}", started.elapsed());
    assert!(!scaler.exact(), "{scaler:?}");
}

/// `prepare_timeline` measures what a cut needs, ahead of any frame and no more: the grade's conversions for a grade, a dip to
/// white (also on a muted track: the helper reads every clip) or a build that converts between matrices, the scaler for any cut, the
/// matrix conversion where the build negotiates, `drawtext`'s layout for a cut with text. Each cut is a fresh process (the probes
/// are cached once per process).
#[test]
#[ignore = "needs ffmpeg; starts this binary again for each cut"]
fn checker_b_r6_prepare_timeline_measures_what_the_cut_needs() {
    let name = "checker_b_r6_prepare_timeline_measures_what_the_cut_needs";
    let cases = ["plain", "dip", "dip on a muted track", "graded", "text", "dip and text"];
    if !inner() {
        for case in cases {
            let (status, text, _) = rerun(name, &[("KERF_R6_CASE", case)]);
            eprintln!(
                "{case}: {}",
                text.lines().filter(|l| l.starts_with("R6 ")).collect::<Vec<_>>().join(" | ")
            );
            assert!(status.success(), "{case}:\n{text}");
            assert!(text.contains("R6 PREPARE DONE"), "{case}:\n{text}");
        }
        return;
    }
    let case = std::env::var("KERF_R6_CASE").expect("a case");
    let id = uuid_like();
    let mut plain = Clip::new(id, 0.0, 1.0, 0.0);
    let mut dipped = Clip::new(id, 0.0, 1.0, 1.0);
    dipped.transition_in = Some(Transition {
        kind: TransitionKind::DipToWhite,
        duration: 0.4,
    });
    let mut tracks = vec![Track {
        clips: vec![plain.clone()],
        ..Track::new(StreamKind::Video, "V1")
    }];
    let mut timeline = Timeline {
        tracks: tracks.clone(),
        ..Timeline::new()
    };
    match case.as_str() {
        "dip" | "dip and text" => timeline.tracks[0].clips.push(dipped),
        "dip on a muted track" => {
            tracks.push(Track {
                clips: vec![dipped],
                muted: true,
                ..Track::new(StreamKind::Video, "V2")
            });
            timeline.tracks = tracks;
        }
        "graded" => {
            plain.grade.curves = Some(kerf_core::Curves {
                master: vec![
                    kerf_core::CurvePoint { x: 0.0, y: 0.05 },
                    kerf_core::CurvePoint { x: 1.0, y: 0.95 },
                ],
                red: Vec::new(),
                green: Vec::new(),
                blue: Vec::new(),
            });
            timeline.tracks[0].clips = vec![plain];
        }
        _ => {}
    }
    if case.contains("text") {
        timeline.overlays.push(kerf_core::TextOverlay::new("caption", 0.1, 0.9));
    }
    use kerf_gpu::{convert, grade, scaler, text};
    let policy = kerf_core::composite_color_policy();
    let converts = policy == kerf_core::CompositeColorPolicy::BottomLayerTag;
    assert_eq!(
        grade::measured(),
        grade::Kernels::Unknown,
        "{case}: measured before it was asked"
    );
    assert_eq!(scaler::measured(), scaler::Kernels::Unknown, "{case}");
    assert_eq!(convert::measured(), convert::Kernels::Unknown, "{case}");
    assert!(text::measured_model().is_none(), "{case}");
    kerf_gpu::prepare_timeline(&timeline);
    assert!(
        matches!(scaler::measured(), scaler::Kernels::Match(_)),
        "{case}: the scaler is measured for any cut"
    );
    let needs_grade = converts || case.contains("dip") || case == "graded";
    assert_eq!(
        grade::measured() == grade::Kernels::Match,
        needs_grade,
        "{case}: the grade's conversions (policy {policy:?}): {:?}",
        grade::measured()
    );
    if !needs_grade {
        assert_eq!(
            grade::measured(),
            grade::Kernels::Unknown,
            "{case}: a probe nothing asked for ran"
        );
    }
    assert_eq!(
        convert::measured() != convert::Kernels::Unknown,
        converts,
        "{case}: the matrix conversion is measured where the build negotiates: {:?}",
        convert::measured()
    );
    assert_eq!(
        text::measured_model().is_some(),
        case.contains("text"),
        "{case}: drawtext's layout"
    );
    eprintln!(
        "R6 {case}: policy {policy:?}, grade {:?}, convert {:?}, text {}",
        grade::measured(),
        convert::measured(),
        text::measured_model().is_some()
    );
    eprintln!("R6 PREPARE DONE");
}

fn uuid_like() -> uuid::Uuid {
    uuid::Uuid::new_v4()
}
