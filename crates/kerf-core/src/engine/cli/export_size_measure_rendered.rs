//! The measured size estimate held to real full exports (`#[ignore]`d: they drive the real `ffmpeg`).
//!
//! This module is the helpers the measured suites share and the **smoke test** CI runs on every engine lane: one measurement of a short
//! homogeneous cut, held to its band with no assertion on how long it took. The calibration suite (`export_size_measure_calibration`) is
//! `--skip`ped there and gated at run time to the FFmpeg it was tuned on.
//!
//! `KERF_FFMPEG=<ffmpeg> KERF_FFPROBE=<ffprobe> cargo test -p kerf-core --no-default-features -- --ignored export_size_measure_rendered`

#![allow(clippy::print_stderr)] // what each case measured, for whoever runs the ignored ones

use std::time::{Duration, Instant};

use super::export_size_rendered::{crf_case, has_encoders, source, Class, Scratch};
use super::*;
use crate::engine::test_support::{make_clip, timeline_of, video_track};

/// One cut of consecutive clips, each of its own asset.
pub(super) fn cut_of(assets: &[Asset]) -> Timeline {
    let mut at = 0.0;
    let clips = assets
        .iter()
        .map(|a| {
            let clip = make_clip(a.id, 0.0, a.duration, at);
            at += a.duration;
            clip
        })
        .collect();
    timeline_of(vec![video_track(clips)])
}

/// What a case measured against the full export.
pub(super) struct Row {
    pub(super) what: String,
    pub(super) real: u64,
    pub(super) estimate: SizeEstimate,
    pub(super) took: Duration,
    pub(super) windows: usize,
    pub(super) planned: usize,
}

impl Row {
    pub(super) fn inside(&self) -> bool {
        self.estimate.low_bytes <= self.real && self.real <= self.estimate.high_bytes
    }

    pub(super) fn width(&self) -> f64 {
        self.estimate.high_bytes as f64 / self.estimate.low_bytes.max(1) as f64
    }

    pub(super) fn print(&self) {
        let e = &self.estimate;
        eprintln!(
            "{:<52} real {:>10} in {:>10} .. {:>10}  x{:>5.2}  {:>5.1} s  {}/{} windows  {:?} {}",
            self.what,
            self.real,
            e.low_bytes,
            e.high_bytes,
            self.width(),
            self.took.as_secs_f64(),
            self.windows,
            self.planned,
            e.basis,
            if self.inside() { "ok" } else { "OUTSIDE" }
        );
    }
}

/// Measure, then export in full, `timeline` with `opts`: the estimate, the file's real size, what the measurement cost.
pub(super) fn compare(dir: &Path, what: &str, timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Row {
    let (mut windows, mut planned) = (0, 0);
    let started = Instant::now();
    let measured = measure_export_size(
        timeline,
        assets,
        opts,
        &[],
        DEFAULT_MEASURE_BUDGET,
        &mut |p| (windows, planned) = (p.windows_done, p.windows_planned),
        &|| false,
    )
    .expect("measure");
    let took = started.elapsed();
    let estimate = measured.files[0].estimate.clone();
    let out = dir.join(format!(
        "out-{}.{}",
        what.replace([' ', '/', ':', '(', ')', ','], "_"),
        opts.container.ext()
    ));
    let status = render_with_progress(timeline, assets, &out, opts, &mut |_| {}, &|| false).expect("export");
    assert_eq!(status, RenderStatus::Completed, "{what}");
    let real = std::fs::metadata(&out).expect("a file").len();
    let _ = std::fs::remove_file(&out);
    Row {
        what: what.to_string(),
        real,
        estimate,
        took,
        windows,
        planned,
    }
}

pub(super) fn finish(rows: &[Row]) {
    for row in rows {
        row.print();
    }
    let outside: Vec<String> = rows
        .iter()
        .filter(|r| !r.inside())
        .map(|r| {
            format!(
                "{}: {} outside {}..{}",
                r.what, r.real, r.estimate.low_bytes, r.estimate.high_bytes
            )
        })
        .collect();
    assert!(
        outside.is_empty(),
        "the measured band missed {} of {} files:\n{}",
        outside.len(),
        rows.len(),
        outside.join("\n")
    );
}

/// The three sections in each of the orders that matter: soft first, noisy first, noisy last.
pub(super) fn sections(dir: &Path, frame: (u32, u32), secs: f64, sound: bool) -> Vec<(&'static str, Vec<Asset>)> {
    let [soft, plain, grainy] = [Class::Soft, Class::Plain, Class::Grainy].map(|c| source(dir, c, frame, 30.0, secs, sound));
    vec![
        ("soft>plain>grainy", vec![soft.clone(), plain.clone(), grainy.clone()]),
        ("grainy>soft>plain", vec![grainy.clone(), soft.clone(), plain.clone()]),
        ("plain>grainy>soft", vec![plain, grainy, soft]),
    ]
}

/// A cut of twelve seconds is measured (not whole), and the real export is inside the band: whatever FFmpeg wrote it, on any machine. No
/// assertion on the time it took: the budget is long, so a loaded runner still finishes every window.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn smoke_a_measurement_of_even_footage_holds_the_whole_file() {
    if !has_encoders(&["libx264"]) {
        eprintln!("no libx264 in this ffmpeg: nothing to measure");
        return;
    }
    let scratch = Scratch::new("measured-smoke");
    let dir = &scratch.0;
    let asset = source(dir, Class::Plain, (320, 180), 30.0, 12.0, false);
    let tl = cut_of(std::slice::from_ref(&asset));
    let opts = crf_case("libx264", Container::Mp4, 23, Some("medium"));
    let mut windows = (0, 0);
    let measured = measure_export_size(
        &tl,
        std::slice::from_ref(&asset),
        &opts,
        &[],
        Duration::from_secs(120),
        &mut |p| windows = (p.windows_done, p.windows_planned),
        &|| false,
    )
    .expect("measure");
    let e = &measured.files[0].estimate;
    assert_eq!(e.basis, SizeBasis::Measured, "{}", e.note);
    assert_eq!(windows.0, windows.1, "every window ran in a long budget");
    let out = dir.join("smoke.mp4");
    let status = render_with_progress(&tl, std::slice::from_ref(&asset), &out, &opts, &mut |_| {}, &|| false).expect("export");
    assert_eq!(status, RenderStatus::Completed);
    let real = std::fs::metadata(&out).expect("a file").len();
    eprintln!(
        "smoke: real {real} in {} .. {} ({} windows)",
        e.low_bytes, e.high_bytes, windows.0
    );
    assert!(
        e.low_bytes <= real && real <= e.high_bytes,
        "{real} outside {}..{}",
        e.low_bytes,
        e.high_bytes
    );
    assert!(e.note.contains("typical of the cut"), "{}", e.note);
}
