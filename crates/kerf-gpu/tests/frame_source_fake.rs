//! `FrameSource` against an `ffmpeg` that misbehaves: one that hangs on a file, one that dies on
//! it, one that closes its output and never exits, one whose second run shows other pictures at
//! the pts of the first. A request must fail promptly — the caller renders that frame through
//! FFmpeg — and never wait on a child that will not answer, and nothing under the source's lock
//! waits on one either. Its own binary, because it points `KERF_FFMPEG` at a wrapper for the
//! whole process.
//!
//! Needs a real `ffmpeg` on `PATH` (the wrapper hands every other file to it) and a POSIX
//! shell. `#[ignore]`d; run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test frame_source_fake -- --ignored
//! ```

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use kerf_core::{Asset, Clip, CompositeColorPolicy, ExportOptions, Project, RenderPlan, StreamKind, Timeline, Track};
use kerf_gpu::{decode_layer, FrameSource, FrameSourceConfig, GpuError, Hint};

/// The ffmpeg `KERF_FFMPEG` named before the wrapper replaced it (the wrapper hands most things on
/// to it, and the clips are made with it).
fn real_ffmpeg() -> &'static str {
    static REAL: OnceLock<String> = OnceLock::new();
    REAL.get_or_init(kerf_core::ffmpeg_path)
}

/// A clip named `name` (decodable by the real ffmpeg, so it probes) and the asset of it.
fn clip(dir: &Path, name: &str) -> Asset {
    let p = dir.join(name);
    let out = Command::new(real_ffmpeg())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=160x90:rate=25:duration=2")
        .args(["-c:v", "mpeg4", "-pix_fmt", "yuv420p"])
        .arg(&p)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    Project::probe_asset(&p).expect("probe")
}

fn layer(a: &Asset, t: f64) -> kerf_core::PlanLayer {
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![Clip::new(a.id, 0.0, a.duration, 0.0)],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    };
    RenderPlan::at(
        &tl,
        std::slice::from_ref(a),
        &ExportOptions::default(),
        t,
        CompositeColorPolicy::FixedBt601,
    )
    .expect("plan")
    .layers
    .remove(0)
}

/// The wrapper: the real ffmpeg for everything but a *decode run* (the argv holds `showinfo`) of
/// the files below, and a one-shot of `hang.mp4` / `die.mp4`. Installed once for the process.
///
/// * `hang.mp4` sleeps without a word, `die.mp4` exits 1 at once.
/// * `closed.mp4` writes three frames, closes its output and never exits.
/// * `bframe.mp4`: a run writes two I/P frames, then a B-frame, and sleeps.
/// * `slow.mp4`: a run at the seek 1.99 s (past its end) takes a second and writes nothing.
/// * `conflict.mp4`: its first run writes frames 3..10 of one picture, a later one frames 0..10 of
///   another (the same pts, other pixels) and then sleeps, as a container that guesses its pts does.
fn wrapper() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("frame-source-fake");
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("conflict.count"));
        let real = real_ffmpeg();
        let wrapper = dir.join("ffmpeg-wrapper.sh");
        let script = r#"#!/bin/sh
dir=$(dirname "$0")
frame() { # pts, n, picture value, picture type
  echo "[Parsed_showinfo_0 @ 0x1] n:$2 pts:$1 pts_time:0 duration:1 i:P iskey:0 type:${4:-I} " >&2
  printf 'FRAME\n'
  head -c 21600 /dev/zero | tr '\0' "\\$(printf '%03o' $3)"
}
case "$*" in
  *hang.mp4*) exec sleep 600 ;;
  *die.mp4*) echo 'refusing' >&2; exit 1 ;;
  *closed.mp4*showinfo*)
    echo '[Parsed_showinfo_0 @ 0x1] config in time_base: 1/25, frame_rate: 25/1' >&2
    printf 'YUV4MPEG2 W160 H90 F25:1 Ip A1:1 C420jpeg\n'
    frame 0 0 60; frame 1 1 60; frame 2 2 60
    exec >&-
    exec sleep 600 ;;
  *bframe.mp4*showinfo*)
    echo '[Parsed_showinfo_0 @ 0x1] config in time_base: 1/25, frame_rate: 25/1' >&2
    printf 'YUV4MPEG2 W160 H90 F25:1 Ip A1:1 C420jpeg\n'
    frame 0 0 60; frame 1 1 60; frame 2 2 60; frame 3 3 60 B; frame 4 4 60
    exec sleep 600 ;;
  *-ss\ 1.990000\ -i\ *slow.mp4*showinfo*) sleep 1; exit 0 ;;
  *conflict.mp4*showinfo*)
    n=$(cat "$dir/conflict.count" 2>/dev/null || echo 0); n=$((n + 1)); echo $n > "$dir/conflict.count"
    first=0; [ $n -eq 1 ] && first=3
    echo '[Parsed_showinfo_0 @ 0x1] config in time_base: 1/25, frame_rate: 25/1' >&2
    printf 'YUV4MPEG2 W160 H90 F25:1 Ip A1:1 C420jpeg\n'
    p=$first
    while [ $p -lt 10 ]; do frame $p $((p - first)) $((40 + n * 30)); p=$((p + 1)); done
    exec sleep 600 ;;
esac
exec 'REAL' "$@"
"#
        .replace("REAL", real);
        std::fs::write(&wrapper, script).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("KERF_FFMPEG", &wrapper);
        std::env::set_var("KERF_HWACCEL", "none");
        dir
    })
}

#[test]
#[ignore = "needs ffmpeg and sh"]
fn a_run_that_hangs_or_dies_fails_the_request_promptly() {
    let dir = wrapper();
    let hang = clip(dir, "hang.mp4");
    let die = clip(dir, "die.mp4");
    let fine = clip(dir, "fine.mp4");

    let src = FrameSource::new(FrameSourceConfig {
        first_frame_timeout: Duration::from_secs(1),
        request_timeout: Duration::from_secs(3),
        ..FrameSourceConfig::default()
    });
    assert!(src.runs_enabled(), "the self-test should pass through the wrapper");

    // A healthy file still decodes through the wrapper.
    assert!(src.frame(&layer(&fine, 0.5), Hint::Scrub).expect("a frame").is_some());

    // Hangs: the reaper kills the silent run, and the request fails well inside its deadline.
    let t0 = Instant::now();
    let err = src.frame(&layer(&hang, 0.5), Hint::Scrub).expect_err("a hung run");
    assert!(matches!(err, GpuError::Decode(_)), "{err}");
    assert!(t0.elapsed() < Duration::from_secs(5), "waited {:?}", t0.elapsed());
    assert_eq!(
        src.stats().runs,
        1,
        "the hung run is gone, the healthy one stays: {:?}",
        src.stats()
    );

    // Dies at once: a failed start, not an end of file — an error, not "no frame".
    let t0 = Instant::now();
    let err = src.frame(&layer(&die, 0.5), Hint::Scrub).expect_err("a dead run");
    assert!(matches!(err, GpuError::Decode(_)), "{err}");
    assert!(
        err.to_string().contains("refusing"),
        "the error carries ffmpeg's stderr: {err}"
    );
    assert!(t0.elapsed() < Duration::from_secs(3), "waited {:?}", t0.elapsed());
}

/// A run that closes its output and never exits used to be waited for under its child's lock, so
/// the `kill` of whoever stopped it — with the source's lock held — waited too, and every request
/// to the source with it.
#[test]
#[ignore = "needs ffmpeg and sh"]
fn a_run_that_closes_its_output_and_never_exits_blocks_nothing_that_stops_it() {
    let dir = wrapper();
    let closed = clip(dir, "closed.mp4");
    let src = FrameSource::new(FrameSourceConfig::default());
    assert!(src.runs_enabled());
    // The run delivers its three frames, then its output ends and its reader waits for the exit:
    // a request for a frame past them is what reads that far.
    let asking = std::thread::spawn({
        let src = std::sync::Arc::clone(&src);
        let l = layer(&closed, 0.4);
        move || {
            let _ = src.frame(&l, Hint::Scrub);
        }
    });
    std::thread::sleep(Duration::from_millis(1500));

    // `release` stops the file's runs with the source's lock held, and the next call needs it.
    let (tx, rx) = std::sync::mpsc::channel();
    let stopper = std::thread::spawn({
        let src = std::sync::Arc::clone(&src);
        let source = kerf_gpu::frame_cache::SourceId::of(Path::new(&closed.path));
        move || {
            src.release(source);
            let _ = tx.send(src.stats());
        }
    });
    let stats = rx
        .recv_timeout(Duration::from_secs(4))
        .expect("stopping a run that never exits must not wait for it");
    stopper.join().unwrap();
    assert_eq!(stats.runs, 0, "{stats:?}");
    let _ = asking.join();
}

/// The first B-frame a run shows ends the run and marks the file (B-frames are shown out of decode
/// order, and a seek into the frames a keyframe leads returns the keyframe): no run is trusted on
/// it again, a cursor is refused, and the real decode answers. Held with a fake `ffmpeg` so that it
/// is the run's check that is exercised, whatever the seek-point probe makes of a real file.
#[test]
#[ignore = "needs ffmpeg and sh"]
fn a_run_that_shows_a_b_frame_marks_the_file() {
    let dir = wrapper();
    let bframe = clip(dir, "bframe.mp4");
    let src = FrameSource::new(FrameSourceConfig::default());
    assert!(src.runs_enabled());
    let cursor_of = |l: &kerf_core::PlanLayer| src.cursor(l, kerf_gpu::CursorConfig::default());
    assert!(
        cursor_of(&layer(&bframe, 0.5)).is_ok(),
        "nothing is known against the file yet"
    );
    // The run reads on to frame 4, passing the B-frame that is frame 3; the request is answered by
    // the one-shot decode once the file is marked.
    let l = layer(&bframe, 4.0 / 25.0);
    let got = src.frame(&l, Hint::Scrub).expect("a frame");
    assert_eq!(got.as_deref(), decode_layer(&l).unwrap().as_ref());
    let until = Instant::now() + Duration::from_secs(10);
    while src.stats().distrusted == 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    let stats = src.stats();
    assert_eq!(stats.distrusted, 1, "the B-frame was not noticed: {stats:?}");
    let l = layer(&bframe, 0.5);
    let got = src.frame(&l, Hint::Scrub).expect("a frame");
    assert_eq!(got.as_deref(), decode_layer(&l).unwrap().as_ref());
    assert!(matches!(cursor_of(&l), Err(GpuError::Unsupported(_))), "a cursor was opened");
}

/// Another run's pictures at the pts an earlier run cached: the run fails, the file is decoded
/// one-shot from then on (and that is the real frame), and nothing panics on a run's thread.
#[test]
#[ignore = "needs ffmpeg and sh"]
fn a_run_whose_pictures_differ_from_the_cached_ones_fails_and_the_file_goes_one_shot() {
    let dir = wrapper();
    let conflict = clip(dir, "conflict.mp4");
    let src = FrameSource::new(FrameSourceConfig::default());
    assert!(src.runs_enabled());
    // Run 1 caches frame 3 (the picture it wrote there); the second request, going backwards,
    // starts run 2, which writes other pictures at 0..10 and meets the conflict at frame 3.
    src.frame(&layer(&conflict, 3.0 / 25.0), Hint::Scrub).expect("run 1's frame");
    src.frame(&layer(&conflict, 1.0 / 25.0), Hint::Forward { fps: 25.0 })
        .expect("run 2's frame");
    let until = Instant::now() + Duration::from_secs(10);
    while src.stats().distrusted == 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    let stats = src.stats();
    assert_eq!(stats.distrusted, 1, "the conflict was not noticed: {stats:?}");

    // From now on the real decode answers.
    let l = layer(&conflict, 0.5);
    let got = src.frame(&l, Hint::Scrub).expect("a frame");
    assert_eq!(got.as_deref(), decode_layer(&l).unwrap().as_ref());
    assert!(src.stats().oneshots > 0);
}

/// A request made before a file's time base is known joins the run another request has starting
/// (it cannot tell where that run is). When that run ends with no frame, because *its* seek was
/// past the end of the file, the joiner got "no frame" for a time well inside it.
#[test]
#[ignore = "needs ffmpeg and sh"]
fn a_request_that_joined_another_requests_run_is_not_answered_by_that_runs_empty_end() {
    let dir = wrapper();
    let slow = clip(dir, "slow.mp4");
    let src = FrameSource::new(FrameSourceConfig::default());
    assert!(src.runs_enabled());
    let past = std::thread::spawn({
        let src = std::sync::Arc::clone(&src);
        let l = layer(&slow, 1.99);
        move || src.frame(&l, Hint::Scrub)
    });
    std::thread::sleep(Duration::from_millis(300));
    let l = layer(&slow, 0.5);
    let got = src.frame(&l, Hint::Scrub).expect("a frame");
    assert!(got.is_some(), "no frame for a time inside the file");
    assert_eq!(got.as_deref(), decode_layer(&l).unwrap().as_ref());
    // The other request's own answer: its seek was past the last frame.
    assert!(past.join().unwrap().expect("a request past the end").is_none());
}
