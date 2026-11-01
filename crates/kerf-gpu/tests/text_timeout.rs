//! `checker/d/…`: a measuring run that does not finish under load is a refusal, and is not tried again for a
//! while. Needs `ffmpeg` (`KERF_FFMPEG`) and a shell; no adapter. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test text_timeout -- --ignored --nocapture
//! ```
//!
//! The one test here swaps `KERF_FFMPEG` for a wrapper that stalls any run that measures a text (the graph with
//! its `[m0][m1]` inputs) and passes every other through, so it runs alone in its own process.

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use kerf_core::PlanText;
use kerf_gpu::{text, GpuError};

fn caption(words: &str) -> PlanText {
    PlanText {
        id: uuid::Uuid::nil(),
        text: words.into(),
        size: 16.0 / 360.0,
        pos: (0.5, 0.8),
        alpha: 1.0,
        color: "white".into(),
        bg: None,
        font_file: kerf_core::fonts::default_font_file(false),
        synthetic_bold: false,
    }
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_d_a_measure_that_stalls_is_killed_refused_and_not_tried_again() {
    let real = kerf_core::ffmpeg_path();
    assert!(
        text::build().is_some(),
        "{real}: a build the compositor knows (probed before the stall)"
    );
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-d-timeout");
    std::fs::create_dir_all(&dir).unwrap();
    let wrapper = dir.join("ffmpeg-that-stalls");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\ncase \"$*\" in *'[m0][m1]'*) exec sleep 30;; esac\nexec '{real}' \"$@\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("KERF_FFMPEG", &wrapper);

    let t = caption("A caption whose measure never comes back");
    let started = Instant::now();
    let err = text::raster(&t, (640, 360)).expect_err("a stalled measure is a refusal");
    let first = started.elapsed();
    eprintln!("first: {err} after {first:?}");
    assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
    assert!(
        first >= Duration::from_secs(4) && first < Duration::from_secs(12),
        "killed at its deadline, neither at once nor after the stall: {first:?}"
    );

    let started = Instant::now();
    let err = text::raster(&t, (640, 360)).expect_err("and refused again");
    let second = started.elapsed();
    eprintln!("second: {err} after {second:?}");
    assert!(
        matches!(&err, GpuError::Unsupported(w) if w.contains("not tried again")),
        "{err}"
    );
    assert!(second < Duration::from_secs(2), "not run again: {second:?}");

    // The same text at another position is the same measure: refused at once as well.
    let mut moved = t;
    moved.pos = (0.41, 0.77);
    let started = Instant::now();
    assert!(text::raster(&moved, (640, 360)).is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
}
