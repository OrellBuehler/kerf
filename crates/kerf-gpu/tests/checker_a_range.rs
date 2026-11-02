//! The render-time range probe (`kerf_core::source_is_full_range`, asked by the compositor under a placement when an asset
//! did not record its range and the composite's range is negotiated), checked on its own: what it says of files of every
//! flag and format, that it is bounded by its timeout, remembers a failure and an answer, and that a probe hung on one file
//! does not hold up the probe of another.
//!
//! One test, in a process of its own: it points `KERF_FFPROBE` at a wrapper (a file whose name has `hang` in it makes the
//! wrapper sleep, every call is counted) and the environment is not changed under another test's feet. Needs `ffmpeg` and
//! `ffprobe`; `#[ignore]`d.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_a_range -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kerf_core::{Clip, CompositeColorPolicy, ExportOptions, Keyframe, Project, RenderPlan, StreamKind, Timeline, Track};
use kerf_gpu::{Compositor, Gpu, GpuOptions};

fn ffmpeg(args: &[&str]) {
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A `testsrc2` file of one second.
fn make(dir: &Path, name: &str, pix: &str, codec: &[&str], flags: &[&str]) -> PathBuf {
    let p = dir.join(name);
    let mut args = vec!["-f", "lavfi", "-i", "testsrc2=size=64x36:rate=30:duration=1"];
    args.extend_from_slice(codec);
    args.extend_from_slice(&["-pix_fmt", pix]);
    args.extend_from_slice(flags);
    args.push(p.to_str().unwrap());
    ffmpeg(&args);
    p
}

fn calls(log: &Path) -> usize {
    std::fs::read_to_string(log).map_or(0, |t| t.lines().count())
}

#[test]
#[ignore = "needs ffmpeg and ffprobe"]
fn checker_a_the_render_time_range_probe_reads_the_file_and_is_bounded() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-a-range-probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let real = std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string());
    let log = dir.join("calls.log");
    let wrapper = dir.join("ffprobe-wrapper.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/bash\necho \"$*\" >> '{}'\ncase \"$*\" in *hang*) exec sleep 120;; esac\nexec '{real}' \"$@\"\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

    let ffv1 = ["-c:v", "ffv1"];
    let x264 = ["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"];
    // (name, file, what the probe must say)
    let mut cases: Vec<(&str, PathBuf, Option<bool>)> = vec![
        (
            "yuv420p pc",
            make(&dir, "a.mkv", "yuv420p", &ffv1, &["-color_range", "pc"]),
            Some(true),
        ),
        (
            "yuv420p tv",
            make(&dir, "b.mkv", "yuv420p", &ffv1, &["-color_range", "tv"]),
            Some(false),
        ),
        ("yuv420p untagged", make(&dir, "c.mkv", "yuv420p", &ffv1, &[]), Some(false)),
        (
            "yuv422p pc",
            make(&dir, "d.mkv", "yuv422p", &ffv1, &["-color_range", "pc"]),
            Some(true),
        ),
        (
            "yuv422p tv",
            make(&dir, "e.mkv", "yuv422p", &ffv1, &["-color_range", "tv"]),
            Some(false),
        ),
        (
            "yuv444p pc",
            make(&dir, "f.mkv", "yuv444p", &ffv1, &["-color_range", "pc"]),
            Some(true),
        ),
        (
            "yuv444p tv",
            make(&dir, "g.mkv", "yuv444p", &ffv1, &["-color_range", "tv"]),
            Some(false),
        ),
        ("yuv444p untagged", make(&dir, "h.mkv", "yuv444p", &ffv1, &[]), Some(false)),
        (
            "gray pc",
            make(&dir, "i.mkv", "gray", &ffv1, &["-color_range", "pc"]),
            Some(true),
        ),
        (
            "yuv420p10le pc",
            make(&dir, "k.mkv", "yuv420p10le", &ffv1, &["-color_range", "pc"]),
            Some(true),
        ),
        (
            "yuv420p10le tv",
            make(&dir, "l.mkv", "yuv420p10le", &ffv1, &["-color_range", "tv"]),
            Some(false),
        ),
        (
            "yuv444p10le pc",
            make(&dir, "m.mkv", "yuv444p10le", &ffv1, &["-color_range", "pc"]),
            Some(true),
        ),
        (
            "h264 yuv420p untagged",
            make(&dir, "n.mp4", "yuv420p", &x264, &[]),
            Some(false),
        ),
        (
            "h264 yuv420p tv, set",
            make(&dir, "o.mp4", "yuv420p", &x264, &["-color_range", "tv"]),
            Some(false),
        ),
        ("h264 yuvj420p", make(&dir, "p.mp4", "yuvj420p", &x264, &[]), Some(true)),
        (
            "h264, the stream (VUI) says full range and the container does not",
            make(
                &dir,
                "q.mp4",
                "yuv420p",
                &x264,
                &["-bsf:v", "h264_metadata=video_full_range_flag=1"],
            ),
            Some(true),
        ),
        (
            "mjpeg yuvj420p",
            make(&dir, "r.mkv", "yuvj420p", &["-c:v", "mjpeg", "-qscale:v", "1"], &[]),
            Some(true),
        ),
        (
            "mjpeg yuvj444p",
            make(&dir, "s.mkv", "yuvj444p", &["-c:v", "mjpeg", "-qscale:v", "1"], &[]),
            Some(true),
        ),
    ];
    // A text file, a missing file, a file with no video stream.
    std::fs::write(dir.join("t.mkv"), b"not a media file").unwrap();
    cases.push(("not a media file", dir.join("t.mkv"), None));
    cases.push(("missing", dir.join("missing.mkv"), None));
    let audio = dir.join("u.wav");
    ffmpeg(&["-f", "lavfi", "-i", "sine=duration=1", audio.to_str().unwrap()]);
    cases.push(("audio only", audio, Some(false)));

    // (ffprobe 9 calls every gray picture full range, whatever the tag says; 6.1 reads the tag: it must just say something.)
    let gray_tv = make(&dir, "j.mkv", "gray", &ffv1, &["-color_range", "tv"]);
    // From here every ffprobe is the wrapper's.
    std::env::set_var("KERF_FFPROBE", &wrapper);
    let mut fails = Vec::new();
    let gray = kerf_core::source_is_full_range(&gray_tv);
    eprintln!("checker/a/range probe gray tv -> {gray:?}");
    if gray.is_none() {
        fails.push("gray tv: the probe could not tell".to_string());
    }
    for (name, path, want) in &cases {
        let before = calls(&log);
        let started = Instant::now();
        let got = kerf_core::source_is_full_range(path);
        let again = kerf_core::source_is_full_range(path);
        eprintln!(
            "checker/a/range probe {name:<68} -> {got:?} ({:.0} ms; {} ffprobe run(s) for two asks)",
            started.elapsed().as_secs_f64() * 1000.0,
            calls(&log) - before
        );
        if got != *want {
            fails.push(format!("{name}: the probe says {got:?}, expected {want:?}"));
        }
        if again != got {
            fails.push(format!("{name}: asked twice, {got:?} then {again:?}"));
        }
        // An answer is kept (one ffprobe for the two asks); a failure is remembered too.
        if calls(&log) - before != 1 {
            fails.push(format!("{name}: {} ffprobe runs for two asks", calls(&log) - before));
        }
    }

    // A probe that hangs: bounded by the probe's timeout (20 s), killed, remembered for a minute.
    let hung = dir.join("hang.mkv");
    std::fs::copy(&cases[0].1, &hung).unwrap();
    let other = cases[1].1.clone();
    let before = calls(&log);
    let started = Instant::now();
    let waiter_path = hung.clone();
    let first = std::thread::spawn(move || {
        let t = Instant::now();
        (kerf_core::source_is_full_range(&waiter_path), t.elapsed())
    });
    // While it hangs, another file is probed without waiting for it, and the same file's second asker waits for the first.
    std::thread::sleep(Duration::from_millis(1500));
    let t = Instant::now();
    let probed_other = kerf_core::source_is_full_range(&dir.join("e.mkv"));
    let other_took = t.elapsed();
    let _ = other;
    let waiter_path = hung.clone();
    let second = std::thread::spawn(move || {
        let t = Instant::now();
        (kerf_core::source_is_full_range(&waiter_path), t.elapsed())
    });
    let (first_says, first_took) = first.join().unwrap();
    let (second_says, second_took) = second.join().unwrap();
    let t = Instant::now();
    let third = kerf_core::source_is_full_range(&hung);
    let third_took = t.elapsed();
    eprintln!(
        "checker/a/range hung probe: first {first_says:?} after {:.1} s; another file {probed_other:?} in {:.0} ms meanwhile; \
         a second asker {second_says:?} after {:.1} s; asked again {third:?} in {:.0} ms; ffprobe runs on the hung file: {}; total {:.1} s",
        first_took.as_secs_f64(),
        other_took.as_secs_f64() * 1000.0,
        second_took.as_secs_f64(),
        third_took.as_secs_f64() * 1000.0,
        calls(&log) - before,
        started.elapsed().as_secs_f64()
    );
    if first_says.is_some() || !(Duration::from_secs(15)..=Duration::from_secs(30)).contains(&first_took) {
        fails.push(format!(
            "the hung probe: {first_says:?} after {first_took:?} (expected None at about 20 s)"
        ));
    }
    if probed_other != Some(false) || other_took > Duration::from_secs(5) {
        fails.push(format!(
            "a probe of another file during the hang: {probed_other:?} after {other_took:?}"
        ));
    }
    if second_says.is_some() || second_took > Duration::from_secs(30) {
        fails.push(format!(
            "the second asker of the hung file: {second_says:?} after {second_took:?}"
        ));
    }
    if third.is_some() || third_took > Duration::from_secs(1) {
        fails.push(format!(
            "the hung file asked again: {third:?} after {third_took:?} (a failure is remembered)"
        ));
    }
    if calls(&log) - before != 1 {
        fails.push(format!("the hung file was probed {} times", calls(&log) - before));
    }
    // The hung process is killed (nothing of ours is left sleeping).
    std::thread::sleep(Duration::from_millis(300));
    let left = Command::new("pgrep")
        .args(["-f", "sleep 120"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if !left.is_empty() {
        fails.push(format!("the timed-out probe was not killed: {left}"));
    }

    // The compositor's use of it: a still of a moving zoom over an asset that did not record its range, under a composite whose
    // range is negotiated (named by the plan, whatever FFmpeg this is). A file that says full range is refused, one that says
    // nothing or limited is drawn, one the probe cannot answer for is refused (after the probe's timeout, once).
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    let compositor = Compositor::new(gpu).expect("a compositor");
    let _ = kerf_gpu::conversion::conversion();
    // (The asset is probed from a file the wrapper lets through, then pointed at the file under test: `Project::probe_asset`
    // would itself wait on a hung ffprobe.)
    let still_of = |path: &Path| -> Result<(), String> {
        let mut asset = Project::probe_asset(&dir.join("b.mkv")).expect("probe the asset");
        asset.path = path.to_string_lossy().into_owned();
        for st in &mut asset.streams {
            st.color_range = None;
        }
        let mut z = Clip::new(asset.id, 0.0, 1.0, 0.0);
        let key = |time: f64, scale: f64| Keyframe {
            time,
            scale,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        };
        z.keyframes = vec![key(0.0, 1.0), key(1.0, 1.6)];
        let tl = Timeline {
            tracks: vec![Track {
                clips: vec![z],
                ..Track::new(StreamKind::Video, "V1".to_string())
            }],
            overlays: Vec::new(),
            svg_overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
            thumbnails: Vec::new(),
        };
        let plan = RenderPlan::at(
            &tl,
            &[asset],
            &ExportOptions::default(),
            0.5,
            CompositeColorPolicy::BottomLayerTag,
        )
        .expect("plan");
        compositor
            .render_plan(&plan, plan.size(u32::MAX))
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    let hung2 = dir.join("hang2.mkv");
    std::fs::copy(&cases[0].1, &hung2).unwrap();
    for (name, path, drawn, says) in [
        ("limited (tv)", dir.join("b.mkv"), true, ""),
        ("nothing said (untagged)", dir.join("c.mkv"), true, ""),
        ("h264 untagged", dir.join("n.mp4"), true, ""),
        (
            "full range (pc)",
            dir.join("a.mkv"),
            false,
            "full-range picture its asset did not record",
        ),
        (
            "4:4:4 full range",
            dir.join("f.mkv"),
            false,
            "full-range picture its asset did not record",
        ),
        (
            "the file would not say (probe hung)",
            hung2.clone(),
            false,
            "the file would not say",
        ),
    ] {
        let started = Instant::now();
        let got = still_of(&path);
        eprintln!(
            "checker/a/range compositor {name:<40} -> {} ({:.1} s)",
            match &got {
                Ok(()) => "drawn".to_string(),
                Err(e) => format!("refused: {}", e.chars().take(110).collect::<String>()),
            },
            started.elapsed().as_secs_f64()
        );
        match (&got, drawn) {
            (Ok(()), true) => {}
            (Err(e), false) if e.contains(says) => {}
            _ => fails.push(format!("{name}: {got:?} (drawn expected: {drawn}, refusal to say {says:?})")),
        }
    }
    // The hung file is not probed again by the next frame (a failure is remembered): the refusal is immediate.
    let started = Instant::now();
    let again = still_of(&hung2);
    if again.is_ok() || started.elapsed() > Duration::from_secs(3) {
        fails.push(format!("the hung file's next frame: {again:?} after {:?}", started.elapsed()));
    }

    std::env::set_var("KERF_FFPROBE", &real);
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}
