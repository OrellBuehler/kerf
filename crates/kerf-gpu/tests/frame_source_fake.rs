//! `FrameSource` against an `ffmpeg` that misbehaves: one that hangs on a file, one that dies on
//! it. A request must fail promptly — the caller renders that frame through FFmpeg — and never
//! wait on a child that will not answer. Its own binary, because it points `KERF_FFMPEG` at a
//! wrapper for the whole process.
//!
//! Needs a real `ffmpeg` on `PATH` (the wrapper hands every other file to it) and a POSIX
//! shell. `#[ignore]`d; run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test frame_source_fake -- --ignored
//! ```

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kerf_core::{Asset, Clip, CompositeColorPolicy, ExportOptions, Project, RenderPlan, StreamKind, Timeline, Track};
use kerf_gpu::{FrameSource, FrameSourceConfig, GpuError, Hint};

/// A clip named `name` (decodable by the real ffmpeg, so it probes) and the asset of it.
fn clip(dir: &Path, name: &str) -> Asset {
    let p = dir.join(name);
    let out = Command::new(kerf_core::ffmpeg_path())
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

#[test]
#[ignore = "needs ffmpeg and sh"]
fn a_run_that_hangs_or_dies_fails_the_request_promptly() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("frame-source-fake");
    std::fs::create_dir_all(&dir).unwrap();
    let hang = clip(&dir, "hang.mp4");
    let die = clip(&dir, "die.mp4");
    let fine = clip(&dir, "fine.mp4");

    // The wrapper: the real ffmpeg for everything but a decode of `hang.mp4` (sleeps without a
    // word) or of `die.mp4` (exits 1 at once). The self-test's own clip goes through.
    let real = kerf_core::ffmpeg_path();
    let wrapper = dir.join("ffmpeg-wrapper.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n  *hang.mp4*) exec sleep 600 ;;\n  *die.mp4*) echo 'refusing' >&2; exit 1 ;;\nesac\nexec '{real}' \"$@\"\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("KERF_FFMPEG", &wrapper);
    std::env::set_var("KERF_HWACCEL", "none");

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
