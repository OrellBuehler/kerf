//! `FrameCursor` against an `ffmpeg` that is a shell script: it writes a numbered 2x2 `y4m` stream
//! and the `showinfo` lines a real run would, so the cursor's own bookkeeping (which pixels it
//! holds, how it ends a run, what it refuses) is tested with no FFmpeg and no GPU. Its own
//! binary, because it points `KERF_FFMPEG` at the script for the whole process.
//!
//! Needs a POSIX shell. Not `#[ignore]`d.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Once;
use std::time::Duration;

use kerf_core::{Color, FpsPick, LayerFx, Pick, PlanLayer, PlanSource, PlanStream, PlanTiming, Rational, Transform};
use kerf_gpu::cursor::picks_through;
use kerf_gpu::{CursorConfig, FrameCursor, GpuError};
use uuid::Uuid;

/// What the script does, by the name of the file it is asked to decode: `many.mp4` is 400 frames
/// numbered 0.. (frame `n` has luma `n % 256` at its first sample, pts `n` on 1/30), `stuck.mp4`
/// three frames, then it closes its output and never exits, `wide.mp4` a 4x4 header and then
/// silence, `spawned.mp4` is `many.mp4` that leaves `spawned.marker` beside the script.
const SCRIPT: &str = r#"#!/bin/sh
header() {
  echo '[Parsed_showinfo_0 @ 0x1] config in time_base: 1/30, frame_rate: 30/1' >&2
  printf "YUV4MPEG2 W$1 H$1 F30:1 Ip A1:1 C420jpeg\n"
}
frame() {
  echo "[Parsed_showinfo_0 @ 0x1] n:$1 pts:$1 pts_time:0 duration:1 duration_time:0" >&2
  printf 'FRAME\n'
  printf "\\$(printf '%03o' $(($1 % 256)))\\000\\000\\000\\000\\000"
}
many() {
  header 2
  n=0
  while [ $n -lt 400 ]; do frame $n; n=$((n + 1)); done
}
case "$*" in
  *many.mp4*) many ;;
  *spawned.mp4*) : > "$(dirname "$0")/spawned.marker"; many ;;
  *stuck.mp4*)
    header 2
    frame 0; frame 1; frame 2
    exec >&-
    exec sleep 600 ;;
  *wide.mp4*)
    header 4
    exec sleep 600 ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#;

fn dir() -> PathBuf {
    static ONCE: Once = Once::new();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cursor-fake");
    ONCE.call_once(|| {
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("ffmpeg-fake.sh");
        std::fs::write(&script, SCRIPT).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("KERF_FFMPEG", &script);
        std::env::set_var("KERF_HWACCEL", "none");
    });
    dir
}

fn config(window_cap_frames: usize) -> CursorConfig {
    CursorConfig {
        hwaccel: None,
        first_frame_timeout: Duration::from_secs(10),
        frame_timeout: Duration::from_secs(1),
        window_cap_bytes: window_cap_frames * 6,
    }
}

fn pick(frame: u64) -> FpsPick {
    FpsPick {
        speed: 1.0,
        reverse: false,
        window: (0.0, 10.0),
        start: 0.0,
        frame,
        fps: Rational::new(30, 1).unwrap(),
        drop_first: false,
        image: None,
    }
}

fn path(name: &str) -> String {
    dir().join(name).to_string_lossy().into_owned()
}

#[test]
fn skipped_output_frames_are_not_held_by_a_forward_cursor() {
    // Ten frames of cap: before, picking output frame 200 held every frame read on the way and
    // failed at the tenth with "a long reversed clip".
    let first = pick(200);
    let mut cursor = FrameCursor::open(&path("many.mp4"), (2, 2), &Pick::Fps(first), config(10)).expect("open");
    let mut shown = Vec::new();
    for k in [200, 201, 202, 250, 251, 299, 300, 399] {
        let frame = cursor
            .pick(&Pick::Fps(FpsPick { frame: k, ..first }))
            .unwrap_or_else(|e| panic!("output frame {k}: {e}"));
        shown.push((k, frame.map(|f| f.y[0])));
        assert!(cursor.frames_held() <= 2, "{} frames held at {k}", cursor.frames_held());
    }
    // The window is 10 s of 30 fps: output frames from 300 on are past it.
    assert_eq!(
        shown,
        [
            (200, Some(200)),
            (201, Some(201)),
            (202, Some(202)),
            (250, Some(250)),
            (251, Some(251)),
            (299, Some(43)), // frame 299, luma 299 % 256
            (300, None),
            (399, None),
        ]
    );
    assert!(cursor.frames_read() >= 300, "the cursor read through the skipped frames");

    // `picks_through` hands every frame to its callback as it is decided.
    let mut cursor = FrameCursor::open(&path("many.mp4"), (2, 2), &Pick::Fps(first), config(10)).expect("open");
    let mut lumas = Vec::new();
    picks_through(&mut cursor, first, 120..126, |_, frame| lumas.push(frame.map(|f| f.y[0]))).expect("picks");
    assert_eq!(lumas, (120..126).map(Some).collect::<Vec<_>>());
}

#[test]
fn a_reversed_window_over_the_cap_is_still_refused() {
    // Reading ahead without a pick being decided must keep a reversed clip's whole window.
    let reversed = FpsPick {
        reverse: true,
        window: (0.0, 3.0),
        ..pick(0)
    };
    let mut cursor = FrameCursor::open(&path("many.mp4"), (2, 2), &Pick::Fps(reversed), config(10)).expect("open");
    let err = cursor.pick(&Pick::Fps(reversed)).expect_err("over the cap");
    assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
}

#[test]
fn a_run_that_closes_its_output_and_does_not_exit_is_killed_not_waited_for() {
    let first = pick(50);
    let mut cursor = FrameCursor::open(&path("stuck.mp4"), (2, 2), &Pick::Fps(first), config(10)).expect("open");
    // The pick needs the end of the file (three frames only): `wait()` on this run never returns.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(cursor.pick(&Pick::Fps(first)).map(|_| ()));
    });
    let result = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the cursor gave up on a run that never exited");
    let err = result.expect_err("a run that did not exit is a failure, not an end of file");
    assert!(matches!(err, GpuError::Decode(_)), "{err}");
    assert!(err.to_string().contains("did not exit"), "{err}");
}

#[test]
fn a_picture_of_another_size_is_unsupported_not_a_failed_decode() {
    let first = pick(0);
    let mut cursor = FrameCursor::open(&path("wide.mp4"), (2, 2), &Pick::Fps(first), config(10)).expect("open");
    let err = cursor.pick(&Pick::Fps(first)).expect_err("a 4x4 stream for a 2x2 clip");
    assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
}

fn layer(path: String, pick: Pick) -> PlanLayer {
    PlanLayer {
        clip_id: Uuid::nil(),
        asset_id: Uuid::nil(),
        track: 0,
        path,
        source: PlanSource::ORIGINAL,
        pick,
        is_image: false,
        source_time: 0.0,
        clip_time: 0.0,
        stream: PlanStream {
            width: 2,
            height: 2,
            rotation: 0,
            fps: Some(30.0),
            codec: "h264".into(),
            color_transfer: None,
            color_primaries: None,
            pix_fmt: Some("yuv420p".into()),
            color_space: None,
        },
        transform: Transform::default(),
        color: Color::default(),
        name: "a".into(),
        projection: None,
        hdr: None,
        effects: Vec::new(),
        mask: None,
        reframe: None,
        fx: LayerFx::default(),
        animated: None,
        timing: PlanTiming {
            window: (0.0, 10.0),
            source_window: (0.0, 10.0),
            speed: 1.0,
            reversed: false,
        },
    }
}

#[test]
fn a_before_pick_is_refused_before_any_ffmpeg_is_spawned() {
    let marker = dir().join("spawned.marker");
    let _ = std::fs::remove_file(&marker);
    let err = FrameCursor::for_layer(&layer(path("spawned.mp4"), Pick::Before(1.0)), config(10))
        .map(|_| ())
        .expect_err("Before cannot be served from a cursor's start");
    assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
    assert!(!marker.exists(), "an ffmpeg was spawned for a pick that could not be served");

    // The control: the same file with a pick the cursor serves does run the script.
    let mut cursor = FrameCursor::for_layer(&layer(path("spawned.mp4"), Pick::AtOrAfter(0.0)), config(10)).expect("open");
    assert!(cursor.pick(&Pick::AtOrAfter(0.0)).expect("a pick").is_some());
    assert!(marker.exists(), "the script leaves its marker when it runs");
}
