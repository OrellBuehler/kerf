//! A text the compositor refuses after trying it is not tried again on every frame of a scrub. Needs `ffmpeg`
//! (`KERF_FFMPEG`) and a shell; no adapter. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test text_refusal_memory -- --ignored --nocapture
//! ```
//!
//! The one test here swaps `KERF_FFMPEG` for a wrapper that counts its runs and passes every one through (the
//! first, the rasterisation of the text, with its bright samples a level darker: the glyph bitmaps cannot then
//! be proven to leave that picture), so it runs alone in its own process.

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use kerf_core::PlanText;
use kerf_gpu::{text, GpuError};

fn caption(words: &str) -> PlanText {
    PlanText {
        id: uuid::Uuid::nil(),
        text: words.into(),
        size: 24.0 / 360.0,
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
fn a_refused_text_costs_no_ffmpeg_run_the_second_time() {
    let real = kerf_core::ffmpeg_path();
    assert!(
        text::build().is_some(),
        "{real}: a build the compositor knows (probed before counting)"
    );
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("text-refusal-memory");
    std::fs::create_dir_all(&dir).unwrap();
    let runs = dir.join("runs");
    std::fs::write(&runs, "").unwrap();
    let wrapper = dir.join("ffmpeg-that-counts");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho run >> '{runs}'\n\
             if [ \"$(wc -l < '{runs}')\" -eq 1 ]; then '{real}' \"$@\" | LC_ALL=C tr '\\200-\\377' '\\177-\\376'; exit 0; fi\n\
             exec '{real}' \"$@\"\n",
            runs = runs.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("KERF_FFMPEG", &wrapper);
    let counted = || std::fs::read_to_string(&runs).unwrap().lines().count();

    let t = caption("A caption the proof turns down");
    let first = text::raster(&t, (640, 360)).expect_err("bitmaps that do not leave FFmpeg's picture are refused");
    let after_first = counted();
    eprintln!("first: {first} after {after_first} runs");
    assert!(
        matches!(&first, GpuError::Unsupported(w) if w.contains("not reproduced exactly")),
        "{first}"
    );
    assert!(after_first > 0, "the first ask ran FFmpeg");

    for ask in 2..=4 {
        let again = text::raster(&t, (640, 360)).expect_err("and refused again");
        assert!(
            matches!(&again, GpuError::Unsupported(w) if w.contains("not tried again")),
            "ask {ask}: {again}"
        );
        assert_eq!(counted(), after_first, "ask {ask} ran FFmpeg for a text already refused");
    }
    // The message is the refusal's own, said once.
    let again = text::raster(&t, (640, 360)).unwrap_err().to_string();
    assert_eq!(again.matches("not rendered on the GPU").count(), 1, "{again}");
}
