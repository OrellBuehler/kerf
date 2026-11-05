//! `checker/e/r3`: the blend probe's child that does not answer is **not measured**: what it probes is not drawn, the verdict is not kept and
//! the probe is run again after its backoff, and a killed child's grandchild holding the pipe does not hold the probe. Needs `ffmpeg`
//! (`KERF_FFMPEG`) and a shell; no adapter. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_e_probe -- --ignored --nocapture
//! ```
//!
//! The one test here swaps `KERF_FFMPEG` for a wrapper that stalls one probe child (two of the darken probes') the first time only and
//! passes every other run through, so it runs alone in its own process.

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use kerf_core::BlendKinds;
use kerf_gpu::blend;

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_e_a_probe_child_that_stalls_is_not_measured_not_kept_and_retried() {
    let real = kerf_core::ffmpeg_path();
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (late, early) = (dir.join("late-once"), dir.join("early-once"));
    let wrapper = dir.join("ffmpeg-that-stalls-once");
    // Two children of the BT.601 / BT.709 / BT.2020 darken probes stall, once: the first is late (killed at the deadline), the second exits at
    // once and leaves a grandchild (`sleep 31`) holding its stdout, as a script around ffmpeg would; the third passes through.
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \"$*\" in *'c0_mode=darken'*)\n  if mkdir '{}' 2>/dev/null; then sleep 31 & wait; exit 1; fi\n  if mkdir '{}' 2>/dev/null; then (sleep 31 &); exit 0; fi;;\nesac\nexec '{real}' \"$@\"\n",
            late.display(),
            early.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("KERF_FFMPEG", &wrapper);

    assert_eq!(blend::measured(), [BlendKinds::NONE; 2], "nothing measured yet");

    let started = Instant::now();
    let first = blend::kernels();
    let first_took = started.elapsed();
    eprintln!("first probe: {first:?} after {first_took:?}");
    assert!(late.exists() && early.exists(), "the wrapper stalled two children");
    assert_eq!(first, [BlendKinds::NONE; 2], "a child that did not answer: nothing is drawn");
    assert_eq!(blend::measured(), [BlendKinds::NONE; 2], "and the verdict is not kept");
    assert!(
        first_took >= Duration::from_secs(15) && first_took < Duration::from_secs(26),
        "the probe waits for its deadline and no longer, whatever holds the pipe of the child it killed: {first_took:?}"
    );

    let started = Instant::now();
    assert_eq!(
        blend::kernels(),
        [BlendKinds::NONE; 2],
        "within the backoff it is not run again"
    );
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert_eq!(blend::measured(), [BlendKinds::NONE; 2]);

    std::thread::sleep(Duration::from_secs(6));
    let started = Instant::now();
    let again = blend::kernels();
    eprintln!(
        "retried probe: {again:?} (extras {:?}) after {:?}",
        blend::extras(),
        started.elapsed()
    );
    assert_eq!(
        again,
        [BlendKinds::ALL; 2],
        "after the backoff it is measured again, and this FFmpeg draws every mode"
    );
    assert_eq!(blend::measured(), again, "and now it is kept");

    let _ = std::process::Command::new("pkill").args(["-f", "sleep 31"]).status();
}
