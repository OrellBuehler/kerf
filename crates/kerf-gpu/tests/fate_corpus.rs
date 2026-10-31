//! FFmpeg's FATE sample corpus as hostile input for everything that reads a file on the GPU path: the
//! probe, the one-shot decode (`decode_layer`), the `FrameSource` (routes, runs, the frame cache) and a
//! `FrameCursor` (the export's picks). The samples are the odd files the codecs' own tests are made
//! of: odd and tiny sizes, 4:2:2 / 4:4:4 / 4:4:0, 10 and 12 bit, gray, alpha (VP8, ProRes 4444, PNG), `yuvj`,
//! interlaced, a display matrix, a stream that changes size or format part-way, a timestamp that starts
//! below zero, container oddities (AVI, FLV, IVF, OBU, elementary streams) and files cut short.
//!
//! The contract each file is held to: **it decodes at the size it probed, or it is an error** (the frame
//! goes to FFmpeg), never a panic and never a hang (a file gets [`BOUND`]; a frame is also checked to be
//! consistent planes, and equal between the one-shot and the `FrameSource` when both give one). What a
//! file does with each stage (a frame, no frame, an error and which) is printed and written to
//! `target/parity/fate-corpus-<ffmpeg version>.txt`; it is a finding, not a failure, that the decoder of
//! one build lacks a codec.
//!
//! The samples are **downloaded** from <https://fate-suite.ffmpeg.org/> into `target/fate-suite/` (a few MB,
//! kept between runs) and never committed: the suite's files carry mixed licences. `#[ignore]`d (needs
//! `ffmpeg`, `ffprobe`, `curl` and a network); run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test fate_corpus -- --ignored --nocapture
//! ```
//!
//! (`KERF_FFMPEG` / `KERF_FFPROBE` for another build.) No adapter is needed: this is the read side only.

#![allow(clippy::print_stderr)]

use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kerf_core::{
    composite_color_policy, Asset, Clip, ExportOptions, OriginalMedia, PlanLayer, PlanRequest, Planner, Project, RenderPlan,
    StreamKind, Timeline, Track,
};
use kerf_gpu::{decode_layer, Compositor, CursorConfig, FrameSource, FrameSourceConfig, Hint, YuvFrame};

const BASE: &str = "https://fate-suite.ffmpeg.org/";

/// How long one file may take in all, probe to cursor. The decode kills a child after 30 s itself, so a file past
/// this is a hang in our code (or a machine that is not keeping up: run it again alone).
const BOUND: Duration = Duration::from_secs(240);

/// The sample files: path under the suite's root, and what is odd about it.
const SAMPLES: &[(&str, &str)] = &[
    ("vp9-test-vectors/vp90-2-02-size-08x08.webm", "8x8, the smallest picture"),
    (
        "vp9-test-vectors/vp90-2-03-size-198x202.webm",
        "198x202: chroma 99x101, both odd",
    ),
    ("vp9-test-vectors/vp91-2-04-yuv444.webm", "4:4:4 8 bit"),
    ("vp9-test-vectors/vp91-2-04-yuv422.webm", "4:2:2 8 bit"),
    ("vp9-test-vectors/vp93-2-20-10bit-yuv422.webm", "4:2:2 10 bit"),
    (
        "vp9-test-vectors/vp90-2-05-resize.ivf",
        "the picture changes size part-way, in IVF",
    ),
    ("h264-444/444_10bit_cabac.h264", "4:4:4 10 bit, an elementary stream"),
    (
        "h264/reinit-large_420_8-to-small_420_8.h264",
        "the stream restarts at another size",
    ),
    ("prores/gray.mov", "gray ProRes"),
    ("prores/prores4444_with_transparency.mov", "ProRes 4444 with alpha"),
    ("vp8_alpha/vp8_video_with_alpha.webm", "VP8 with an alpha plane"),
    ("mov/displaymatrix.mov", "a display matrix (rotated by a tag)"),
    ("mov/moviedispmat.mp4", "a display matrix in an mp4"),
    ("mov/mov_neg_first_pts_discard.mov", "the first timestamp is below zero"),
    ("jpg/Landscape_5.jpg", "a JPEG (yuvj, EXIF orientation)"),
    ("jpg/12bpp.jpg", "a 12 bit JPEG"),
    ("png1/lena-rgba.png", "a PNG with alpha"),
    ("png1/lena-gray8.png", "a gray PNG"),
    ("dv/dvcprohd_1080i50.mov", "interlaced DVCPRO HD"),
    ("mpeg2/mpeg2_field_encoding.ts", "interlaced MPEG-2 in a transport stream"),
    ("mjpeg/mjpeg_field_order.avi", "MJPEG with a field order, in AVI"),
    ("mkv/test7_cut.mkv", "matroska, cut from a longer file"),
    (
        "flv/Enigma_Principles_of_Lust-part.flv",
        "FLV: a millisecond clock, frames at uneven gaps",
    ),
    ("av1/still_picture_rgb.obu", "an AV1 still in RGB, a bare OBU"),
    ("gif/m4nb.gif", "a palette GIF"),
];

/// Files cut short: (source in [`SAMPLES`], the share of its bytes kept, minus a few so the cut is mid-packet).
const TRUNCATED: &[(&str, f64)] = &[
    ("mpeg2/mpeg2_field_encoding.ts", 0.6),
    ("dv/dvcprohd_1080i50.mov", 0.4),
    ("mkv/test7_cut.mkv", 0.5),
    ("mov/moviedispmat.mp4", 0.6),
];

fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().to_path_buf()
}

fn suite_dir() -> PathBuf {
    target_dir().join("fate-suite")
}

/// `rel` of the suite in `target/fate-suite/`, downloaded unless an earlier run left it. Written under another name
/// and renamed, so a transfer that was cut off leaves nothing a later run trusts. A download that fails is a failure of the
/// test, not a skip: the corpus is what it is for.
fn fetch(rel: &str) -> PathBuf {
    let path = suite_dir().join(rel);
    if path.metadata().is_ok_and(|m| m.len() > 0) {
        return path;
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let part = path.with_file_name(format!(
        "part-{}-{}",
        std::process::id(),
        path.file_name().unwrap().to_string_lossy()
    ));
    // The proxy of a sandbox or a flaky link cuts a transfer short now and then (curl 18): a few tries.
    let mut last = String::new();
    for _ in 0..4 {
        let out = Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location", "--proto", "=https"])
            .args(["--connect-timeout", "30", "--max-time", "300", "--output"])
            .arg(&part)
            .arg(format!("{BASE}{rel}"))
            .stdin(Stdio::null())
            .output()
            .expect("run curl");
        if out.status.success() && part.metadata().is_ok_and(|m| m.len() > 0) {
            last.clear();
            break;
        }
        last = String::from_utf8_lossy(&out.stderr).into_owned();
        let _ = std::fs::remove_file(&part);
    }
    assert!(last.is_empty(), "download {BASE}{rel}: {last}");
    std::fs::rename(&part, &path).unwrap();
    path
}

/// The first `keep` of the bytes of `from`, less seven, as `truncated/<name>` of the suite.
fn truncated(from: &Path, keep: f64) -> PathBuf {
    truncated_into(&suite_dir().join("truncated"), from, keep)
}

fn truncated_into(dir: &Path, from: &Path, keep: f64) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(from.file_name().unwrap());
    let bytes = std::fs::read(from).unwrap();
    let n = ((bytes.len() as f64 * keep) as usize).saturating_sub(7).max(1);
    std::fs::write(&path, &bytes[..n]).unwrap();
    path
}

/// What one stage did with a file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    /// A frame at the probed size.
    Frame((u32, u32)),
    /// Nothing is drawn (a time past the end).
    NoFrame,
    /// The stage failed or refused, and why.
    Error(String),
    /// The stage was not reached.
    Skipped(String),
}

impl Stage {
    fn show(&self) -> String {
        match self {
            Stage::Frame((w, h)) => format!("frame {w}x{h}"),
            Stage::NoFrame => "no frame".to_string(),
            Stage::Error(e) => format!("error: {}", clip_text(e, 230)),
            Stage::Skipped(why) => format!("- ({})", clip_text(why, 230)),
        }
    }

    /// One word for the summary table.
    fn word(&self) -> &'static str {
        match self {
            Stage::Frame(_) => "frame",
            Stage::NoFrame => "none",
            Stage::Error(_) => "error",
            Stage::Skipped(_) => "-",
        }
    }

    fn decoded(&self) -> bool {
        matches!(self, Stage::Frame(_))
    }
}

fn clip_text(s: &str, max: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        format!("{}...", one_line.chars().take(max - 3).collect::<String>())
    }
}

/// Everything a file did, and what it broke.
#[derive(Debug)]
struct Outcome {
    name: String,
    /// `h264 yuv420p 320x240 rot 90`, or the probe's error.
    probe: String,
    /// `draws` or the plan's refusals.
    plan: String,
    one_shot: Stage,
    source: Stage,
    cursor: Stage,
    /// Breaches of the contract: a frame that is not at the probed size or not consistent planes, a run's
    /// frame that is not the one-shot's.
    violations: Vec<String>,
    secs: f64,
}

impl Outcome {
    /// The file's name where its path stood in a message: the path is long and says nothing the name does not.
    fn tidy(mut self, path: &Path) -> Self {
        let (full, name) = (path.display().to_string(), self.name.clone());
        for stage in [&mut self.one_shot, &mut self.source, &mut self.cursor] {
            if let Stage::Error(e) | Stage::Skipped(e) = stage {
                *e = e.replace(&full, &name);
            }
        }
        self.probe = self.probe.replace(&full, &name);
        self.plan = self.plan.replace(&full, &name);
        self
    }
}

/// A frame is at the probed size and its planes add up.
fn check_frame(stage: &str, frame: &YuvFrame, probed: (u32, u32), violations: &mut Vec<String>) -> Stage {
    if (frame.width, frame.height) != probed {
        violations.push(format!(
            "{stage}: a {}x{} frame for a picture probed {}x{}",
            frame.width, frame.height, probed.0, probed.1
        ));
    }
    if !frame.is_consistent() {
        violations.push(format!(
            "{stage}: the planes of a {}x{} frame do not add up",
            frame.width, frame.height
        ));
    }
    Stage::Frame((frame.width, frame.height))
}

/// A one-clip timeline of `asset`.
fn timeline(asset: &Asset) -> Timeline {
    let secs = if asset.duration > 0.0 { asset.duration } else { 1.0 };
    Timeline {
        tracks: vec![Track {
            clips: vec![Clip::new(asset.id, 0.0, secs, 0.0)],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

/// The reads of one file: probe, plan, one-shot decode, `FrameSource`, `FrameCursor`. Runs on its own thread.
fn read(path: &Path) -> Outcome {
    let started = Instant::now();
    let mut out = Outcome {
        name: path.file_name().unwrap().to_string_lossy().into_owned(),
        probe: String::new(),
        plan: String::new(),
        one_shot: Stage::Skipped("not probed".into()),
        source: Stage::Skipped("not probed".into()),
        cursor: Stage::Skipped("not probed".into()),
        violations: Vec::new(),
        secs: 0.0,
    };
    let finish = |mut out: Outcome| {
        out.secs = started.elapsed().as_secs_f64();
        out
    };
    let asset = match Project::probe_asset(path) {
        Ok(a) => a,
        Err(e) => {
            out.probe = format!("error: {}", clip_text(&e.to_string(), 230));
            return finish(out);
        }
    };
    let Some(video) = asset.streams.iter().find(|s| s.kind == StreamKind::Video) else {
        out.probe = "no video stream".to_string();
        return finish(out);
    };
    out.probe = format!(
        "{} {} {}x{}{}{}",
        video.codec,
        video.pix_fmt.as_deref().unwrap_or("?"),
        video.width.unwrap_or(0),
        video.height.unwrap_or(0),
        if video.rotation != 0 {
            format!(" rot {}", video.rotation)
        } else {
            String::new()
        },
        if video.image { " image" } else { "" },
    );

    let policy = composite_color_policy();
    let tl = timeline(&asset);
    let assets = std::slice::from_ref(&asset);
    let secs = asset.duration.max(0.0);
    // The layer a still plan draws, moved to `t`: the one-shot decode and the runs are asked for any time, past the end too.
    let layer_at = |t: f64| -> Result<PlanLayer, String> {
        let plan = RenderPlan::at(&tl, assets, &ExportOptions::default(), 0.0, policy).map_err(|e| e.to_string())?;
        let mut layer = plan
            .layers
            .into_iter()
            .next()
            .ok_or_else(|| "the plan has no layer".to_string())?;
        layer.source_time = t;
        layer.pick = kerf_core::Pick::AtOrAfter(t);
        Ok(layer)
    };
    let first = match RenderPlan::at(&tl, assets, &ExportOptions::default(), 0.0, policy) {
        Ok(plan) => plan,
        Err(e) => {
            out.plan = format!("error: {}", clip_text(&e.to_string(), 230));
            out.one_shot = Stage::Skipped("no plan".into());
            out.source = Stage::Skipped("no plan".into());
            out.cursor = Stage::Skipped("no plan".into());
            return finish(out);
        }
    };
    let why = first.reasons(&Compositor::CAPS, first.size(540));
    out.plan = if why.is_empty() {
        "draws".to_string()
    } else {
        let mut kinds: Vec<&str> = why.iter().map(|u| u.kind()).collect();
        kinds.dedup();
        format!("refused: {}", kinds.join(" "))
    };
    let Some(probed) = first.layers.first().map(|l| (l.stream.width, l.stream.height)) else {
        out.one_shot = Stage::Skipped("the plan holds no layer".into());
        out.source = Stage::Skipped("the plan holds no layer".into());
        out.cursor = Stage::Skipped("the plan holds no layer".into());
        return finish(out);
    };

    // The one-shot decode and the frame source at three times: the start, the middle, and past the end.
    let times = [0.0, secs / 2.0, secs + 2.0];
    let source = FrameSource::new(FrameSourceConfig::default());
    let (mut one_shot, mut runs) = (Vec::new(), Vec::new());
    for t in times {
        let layer = match layer_at(t) {
            Ok(l) => l,
            Err(e) => {
                one_shot.push(Stage::Error(e.clone()));
                runs.push(Stage::Error(e));
                continue;
            }
        };
        let shot = decode_layer(&layer);
        let from_runs = source.frame(&layer, Hint::Exact);
        if let (Ok(Some(a)), Ok(Some(b))) = (&shot, &from_runs) {
            if **b != *a {
                out.violations
                    .push(format!("t={t:.2}: the frame source's frame is not the one-shot decode's"));
            }
        }
        one_shot.push(match shot {
            Ok(Some(f)) => check_frame("one-shot", &f, probed, &mut out.violations),
            Ok(None) => Stage::NoFrame,
            Err(e) => Stage::Error(e.to_string()),
        });
        runs.push(match from_runs {
            Ok(Some(f)) => check_frame("frame source", &f, probed, &mut out.violations),
            Ok(None) => Stage::NoFrame,
            Err(e) => Stage::Error(e.to_string()),
        });
    }
    // The stage's headline: a frame if any time gave one, else the first error, else no frame.
    let headline = |stages: Vec<Stage>| {
        stages
            .iter()
            .find(|s| s.decoded())
            .or_else(|| stages.iter().find(|s| matches!(s, Stage::Error(_))))
            .cloned()
            .unwrap_or(Stage::NoFrame)
    };
    out.one_shot = headline(one_shot);
    out.source = headline(runs);

    // The export's picks: a cursor over the first output frames of a motion plan.
    out.cursor = (|| {
        let planner = match Planner::new(
            &tl,
            assets,
            &ExportOptions::default(),
            PlanRequest::motion(policy).with_media(&OriginalMedia),
        ) {
            Ok(p) => p,
            Err(e) => return Stage::Error(e.to_string()),
        };
        let layer = match planner.at_frame(0).map(|p| p.layers.into_iter().next()) {
            Ok(Some(l)) => l,
            Ok(None) => return Stage::Skipped("no layer in the first frame".into()),
            Err(e) => return Stage::Error(e.to_string()),
        };
        let mut cursor = match source.cursor(&layer, CursorConfig::default()) {
            Ok(c) => c,
            Err(e) => return Stage::Error(e.to_string()),
        };
        let mut seen = Stage::NoFrame;
        for k in 0..6 {
            let Some(pick) = planner
                .at_frame(k)
                .ok()
                .and_then(|p| p.layers.into_iter().next())
                .map(|l| l.pick)
            else {
                break;
            };
            match cursor.pick(&pick) {
                Ok(Some(f)) => seen = check_frame("cursor", &f, probed, &mut out.violations),
                Ok(None) => {}
                Err(e) => return Stage::Error(e.to_string()),
            }
        }
        seen
    })();
    finish(out)
}

impl Outcome {
    /// A file whose reads did not finish: `what` is the violation.
    fn failed(name: &str, what: String, secs: f64) -> Self {
        Outcome {
            name: name.to_string(),
            probe: "-".into(),
            plan: "-".into(),
            one_shot: Stage::Skipped(what.clone()),
            source: Stage::Skipped(what.clone()),
            cursor: Stage::Skipped(what.clone()),
            violations: vec![what],
            secs,
        }
    }
}

/// `work` on a thread that is waited for no longer than `bound`, its panic caught: a hang or a panic is an
/// outcome with a violation, so the other files still run.
fn bounded_by(name: &str, bound: Duration, work: impl FnOnce() -> Outcome + Send + 'static) -> Outcome {
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(work)));
    });
    match rx.recv_timeout(bound) {
        Ok(Ok(outcome)) => {
            let _ = worker.join();
            outcome
        }
        Ok(Err(panic)) => {
            let what = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_default();
            Outcome::failed(name, format!("PANIC: {what}"), 0.0)
        }
        Err(_) => Outcome::failed(
            name,
            format!("HANG: no answer in {:.1} s", bound.as_secs_f64()),
            bound.as_secs_f64(),
        ),
    }
}

/// [`read`] of `path` within [`BOUND`].
fn bounded(path: &Path) -> Outcome {
    let owned = path.to_path_buf();
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    bounded_by(&name, BOUND, move || read(&owned).tidy(&owned))
}

fn ffmpeg_version() -> String {
    let out = Command::new(kerf_core::ffmpeg_path())
        .arg("-version")
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("ffmpeg")
        .to_string()
}

#[test]
#[ignore = "needs ffmpeg, ffprobe, curl and a network"]
fn every_fate_sample_decodes_at_the_probed_size_or_fails_with_an_error() {
    let mut files: Vec<(PathBuf, String)> = SAMPLES.iter().map(|(rel, what)| (fetch(rel), (*what).to_string())).collect();
    for (rel, keep) in TRUNCATED {
        let name = Path::new(rel).file_name().unwrap().to_string_lossy().into_owned();
        files.push((
            truncated(&fetch(rel), *keep),
            format!("{name} cut at {:.0}% of its bytes", keep * 100.0),
        ));
    }
    let version_line = ffmpeg_version();
    let mut report = String::new();
    let _ = writeln!(report, "{version_line}");
    let _ = writeln!(
        report,
        "samples from {BASE} in target/fate-suite/; the contract: a frame at the probed size, or an error\n"
    );
    let mut outcomes = vec![];
    for (path, what) in &files {
        let outcome = bounded(path);
        let _ = writeln!(report, "{}  ({:.1} s)  {what}", outcome.name, outcome.secs);
        let _ = writeln!(report, "    probe:        {}", outcome.probe);
        let _ = writeln!(report, "    plan:         {}", outcome.plan);
        let _ = writeln!(report, "    one-shot:     {}", outcome.one_shot.show());
        let _ = writeln!(report, "    frame source: {}", outcome.source.show());
        let _ = writeln!(report, "    cursor:       {}", outcome.cursor.show());
        for v in &outcome.violations {
            let _ = writeln!(report, "    VIOLATION: {v}");
        }
        outcomes.push(outcome);
    }
    let _ = writeln!(
        report,
        "\n{:<44} {:>5} {:<10} {:<8} {:<8} {:<8}  plan",
        "file", "secs", "one-shot", "source", "cursor", "violated"
    );
    for o in &outcomes {
        let _ = writeln!(
            report,
            "{:<44} {:>5.1} {:<10} {:<8} {:<8} {:<8}  {}",
            o.name,
            o.secs,
            o.one_shot.word(),
            o.source.word(),
            o.cursor.word(),
            if o.violations.is_empty() { "" } else { "YES" },
            o.plan
        );
    }
    let total: u64 = files.iter().filter_map(|(p, _)| p.metadata().ok()).map(|m| m.len()).sum();
    let decoded = outcomes.iter().filter(|o| o.one_shot.decoded()).count();
    let _ = writeln!(
        report,
        "{} files ({} KiB), {} decoded a frame one-shot, {} through the frame source, {} through a cursor",
        files.len(),
        total >> 10,
        decoded,
        outcomes.iter().filter(|o| o.source.decoded()).count(),
        outcomes.iter().filter(|o| o.cursor.decoded()).count(),
    );
    eprintln!("{report}");
    let dir = target_dir().join("parity");
    std::fs::create_dir_all(&dir).unwrap();
    let token: String = version_line
        .split_whitespace()
        .nth(2)
        .unwrap_or("")
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let token = if token.is_empty() { "unknown".to_string() } else { token };
    std::fs::write(dir.join(format!("fate-corpus-{token}.txt")), &report).unwrap();

    let violations: Vec<String> = outcomes
        .iter()
        .flat_map(|o| o.violations.iter().map(move |v| format!("{}: {v}", o.name)))
        .collect();
    assert!(violations.is_empty(), "the contract was broken:\n{}", violations.join("\n"));
    // The harness itself: if nothing decodes, the corpus did not run.
    assert!(
        decoded * 2 >= files.len(),
        "only {decoded} of {} samples decoded a frame one-shot:\n{report}",
        files.len()
    );
    assert!(total < 150 << 20, "the corpus is {} MiB", total >> 20);
}

#[test]
fn a_panic_or_a_hang_in_a_read_is_a_violation_and_the_next_file_still_runs() {
    let panicked = bounded_by("p.mkv", Duration::from_secs(30), || panic!("index out of bounds"));
    assert_eq!(panicked.violations.len(), 1);
    assert!(
        panicked.violations[0].starts_with("PANIC: index out of bounds"),
        "{:?}",
        panicked.violations
    );

    let hung = bounded_by("h.mkv", Duration::from_millis(100), || {
        std::thread::sleep(Duration::from_secs(3));
        Outcome::failed("h.mkv", "never seen".into(), 0.0)
    });
    assert_eq!(hung.violations.len(), 1);
    assert!(hung.violations[0].starts_with("HANG"), "{:?}", hung.violations);

    let fine = bounded_by("f.mkv", Duration::from_secs(30), || {
        let mut o = Outcome::failed("f.mkv", String::new(), 0.0);
        o.violations.clear();
        o
    });
    assert!(fine.violations.is_empty());
}

#[test]
fn a_frame_is_held_to_the_probed_size_and_to_consistent_planes() {
    let frame = |w, h, y, c| YuvFrame {
        width: w,
        height: h,
        y: vec![0; y],
        u: vec![0; c],
        v: vec![0; c],
    };
    let mut v = Vec::new();
    check_frame("one-shot", &frame(4, 2, 8, 2), (4, 2), &mut v);
    assert!(v.is_empty(), "{v:?}");
    check_frame("one-shot", &frame(4, 2, 8, 2), (2, 4), &mut v);
    assert_eq!(v.len(), 1, "{v:?}");
    check_frame("cursor", &frame(4, 2, 7, 2), (4, 2), &mut v);
    assert_eq!(v.len(), 2, "{v:?}");
}

#[test]
fn a_cut_file_keeps_its_head_and_loses_a_few_bytes_of_the_cut() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fate-corpus-unit");
    std::fs::create_dir_all(&dir).unwrap();
    let from = dir.join("whole.bin");
    std::fs::write(&from, (0..=255u8).cycle().take(1000).collect::<Vec<_>>()).unwrap();
    let cut = truncated_into(&dir.join("truncated"), &from, 0.5);
    let bytes = std::fs::read(&cut).unwrap();
    assert_eq!(bytes.len(), 493);
    assert!(bytes.iter().enumerate().all(|(i, b)| *b == (i % 256) as u8));
    assert_eq!(cut.parent().unwrap().file_name().unwrap(), "truncated");
}
