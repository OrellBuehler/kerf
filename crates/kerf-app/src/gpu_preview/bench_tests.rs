//! Timings of the frame path (`KERF_BENCH=1`, `#[ignore]`d):
//!
//! ```text
//! KERF_BENCH=1 KERF_BENCH_OUT=/some/dir cargo test -p kerf-app --no-default-features --release \
//!     --lib -- --ignored --nocapture --test-threads=1 bench_tests
//! ```
//!
//! * `the_inputs_of_a_frame`: what resolving one frame's inputs cost per frame before (the project lock,
//!   the proxy-wait check, the working timeline parsed from its JSON, every asset read and the disk asked
//!   for each one's proxy) against a held snapshot (an epoch compared, an `Arc` cloned), on projects of
//!   three sizes.
//! * `a_drag_through_the_frame_path`: a forward drag of the playhead over 1280x720 all-intra clips,
//!   frame by frame through [`GpuPreview`] with a screen that waits for the GPU to finish each frame:
//!   request and answer one at a time with the inputs taken for every frame (what the page and the
//!   backend did), the same with a held snapshot, and requests posted at the pointer's pace into the
//!   latest-wins mailbox. A device that is lavapipe (a CPU rasteriser) makes the GPU part of a frame
//!   tens of milliseconds; the part this work changed is what is around it.

#![allow(clippy::print_stderr)]

use std::fmt::Write as _;

use kerf_core::{Asset, Clip, StreamKind, Timeline, Track};
use uuid::Uuid;

use super::drive_tests::{self, ScreenLog};
use super::*;

fn bench_on() -> bool {
    std::env::var_os("KERF_BENCH").is_some()
}

/// Print `text` and, with `KERF_BENCH_OUT=<dir>`, write it to `<dir>/<name>`.
fn report_to(name: &str, text: &str) {
    eprintln!("{text}");
    if let Some(dir) = std::env::var_os("KERF_BENCH_OUT") {
        let dir = PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(name), text);
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 * p) as usize).min(v.len() - 1)]
}

/// A file-backed project of `assets` assets and `clips` clips on four video tracks.
fn big_project(dir: &Path, assets: usize, clips: usize) -> Project {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let project = Project::create(dir.join(format!("bench-{assets}-{clips}-{n}.kerf"))).expect("a project");
    let template = drive_tests::probed(&drive_tests::media()[0]);
    // Assets whose files are there (hard links of one clip), so a plan can be made from the project.
    let made: Vec<Asset> = (0..assets)
        .map(|n| {
            let path = dir.join(format!("clip{n:03}.mkv"));
            let source = drive_tests::media()[0].clone();
            if !path.exists() {
                std::fs::hard_link(&source, &path)
                    .or_else(|_| std::fs::copy(&source, &path).map(|_| ()))
                    .expect("a copy of the clip");
            }
            Asset {
                id: Uuid::new_v4(),
                path: path.to_string_lossy().into_owned(),
                name: format!("clip{n:03}.mkv"),
                ..template.clone()
            }
        })
        .collect();
    for asset in &made {
        // (A path is stored once: what comes back is the asset the project holds.)
        assert_eq!(project.insert_or_get_asset(asset).expect("asset").id, asset.id);
    }
    let per_track = clips / 4;
    let tracks = (0..4)
        .map(|track| Track {
            clips: (0..per_track)
                .map(|n| Clip::new(made[(track + n) % made.len()].id, 0.0, 2.0, n as f64 * 2.0))
                .collect(),
            ..Track::new(StreamKind::Video, format!("V{}", track + 1))
        })
        .collect();
    let timeline = Timeline {
        tracks,
        overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    };
    project.save_timeline(&timeline).expect("a timeline");
    project
}

#[test]
#[ignore = "timings; KERF_BENCH=1"]
fn the_inputs_of_a_frame() {
    let _turn = drive_tests::exclusive();
    if !bench_on() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("kerf-app-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut out = String::from("# the inputs of one preview frame, per frame (microseconds, median / p95 of 3000)\n");
    let _ = writeln!(
        out,
        "# before = lock + proxy_waits + working timeline (JSON parse) + preview assets + assets; after = held snapshot + proxy_wait"
    );
    for (assets, clips) in [(2usize, 8usize), (12, 120), (40, 480)] {
        let shared = Arc::new(Mutex::new(big_project(&dir, assets, clips)));
        let snapshots = PlanSnapshots::new(Duration::from_secs(3600), kerf_core::plan_epoch);
        let held = snapshots.keep(Taken::from(&lock(&shared)).expect("inputs").resolve());
        let json_bytes = serde_json::to_string(&held.timeline).unwrap().len();
        let (mut before, mut after, mut miss) = (Vec::new(), Vec::new(), Vec::new());
        for n in 0..3000 {
            let t = (n % 400) as f64 * 0.0333;
            let t0 = Instant::now();
            {
                let mut project = lock(&shared);
                project.set_actor(kerf_core::EditSource::User);
                let waits = project.proxy_waits(t, Some(t)).expect("waits");
                std::hint::black_box(kerf_core::proxy::waits_message(&waits));
                let (timeline, preview_assets) = project.timeline_frame_inputs().expect("inputs");
                let assets = project.list_assets().expect("assets");
                std::hint::black_box((timeline, preview_assets, assets));
            }
            before.push(us(t0.elapsed()));
            // The new path when the snapshot is stale: the same reads, then resolved off the lock.
            let t0 = Instant::now();
            let rebuilt = Taken::from(&lock(&shared)).expect("inputs").resolve();
            std::hint::black_box(&rebuilt);
            miss.push(us(t0.elapsed()));
            let t0 = Instant::now();
            let fresh = snapshots.fresh(Instant::now()).expect("fresh");
            std::hint::black_box(fresh.proxy_wait(t).expect("wait"));
            after.push(us(t0.elapsed()));
        }
        let _ = writeln!(
            out,
            "{assets:>3} assets {clips:>4} clips (timeline JSON {:>6} bytes): before {:>8.1} / {:>8.1} us   after {:>6.2} / {:>6.2} us   ({:.0}x)   a stale snapshot rebuilt: {:>8.1} / {:>8.1} us",
            json_bytes,
            median(&mut before.clone()),
            percentile(&mut before, 0.95),
            median(&mut after.clone()),
            percentile(&mut after, 0.95),
            median(&mut before.clone()) / median(&mut after.clone()).max(1e-9),
            median(&mut miss.clone()),
            percentile(&mut miss, 0.95),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    report_to("bench-inputs.txt", &out);
}

/// 1280x720 all-intra x264, the shape of a preview proxy.
fn proxy_clips(dir: &Path) -> [PathBuf; 2] {
    let make = |name: &str, lavfi: &str| -> PathBuf {
        let path = dir.join(name);
        let out = kerf_core::ffmpeg_command()
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-y",
                "-f",
                "lavfi",
                "-i",
                lavfi,
            ])
            .args([
                "-c:v", "libx264", "-g", "1", "-crf", "20", "-preset", "veryfast", "-pix_fmt", "yuv420p",
            ])
            .arg(&path)
            .output()
            .expect("run ffmpeg");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        path
    };
    [
        make("p1.mp4", "testsrc2=size=1280x720:rate=30:duration=6"),
        make("p2.mp4", "gradients=size=1280x720:rate=30:duration=6:speed=0.00001"),
    ]
}

/// One project with the two proxies back to back, 5 s each.
fn proxy_project(clips: &[PathBuf; 2]) -> Project {
    let project = Project::open_in_memory().expect("a project");
    for path in clips {
        let asset = project.insert_or_get_asset(&drive_tests::probed(path)).expect("asset");
        project.cut_clip(asset.id, 0.0, 5.0).expect("a clip");
    }
    project
}

struct Drag {
    name: String,
    /// Frame requests made.
    requests: usize,
    /// Frames the GPU drew.
    drawn: usize,
    /// From the first request to the last frame on screen.
    wall: Duration,
    /// Request arrival to presented, for the frames that were drawn.
    latency: Vec<f64>,
    /// Inputs taken from the project (the lock and the parse) while the drag ran.
    taken: u64,
    /// The GPU path's own parts of a frame (decode wait, recording and submitting, presenting).
    parts: Vec<GpuTimings>,
    /// What getting the frame's inputs took (steady rows only), microseconds.
    inputs_us: Vec<f64>,
}

impl Drag {
    fn line(&mut self) -> String {
        let part = |f: fn(&GpuTimings) -> f64| median(&mut self.parts.iter().map(f).collect::<Vec<_>>());
        let (decode, record, present) = (part(|t| t.decode_ms), part(|t| t.composite_ms), part(|t| t.present_ms));
        format!(
            "{:<66} {:>3} requests {:>3} drawn, last on screen at {:>6.0} ms ({:>5.1} per drawn); request to screen p50 {:>6.1} p95 {:>6.1} ms; decode {decode:.1} record {record:.1} present+completion {present:.1} ms; inputs taken {}x{}",
            self.name,
            self.requests,
            self.drawn,
            ms(self.wall),
            ms(self.wall) / self.drawn.max(1) as f64,
            median(&mut self.latency.clone()),
            percentile(&mut self.latency, 0.95),
            self.taken,
            if self.inputs_us.is_empty() {
                String::new()
            } else {
                format!(" (getting them: {:.0} us)", median(&mut self.inputs_us.clone()))
            },
        )
    }
}

fn warmed_preview(log: &Arc<Mutex<ScreenLog>>, gpu: Arc<Gpu>, inputs: &PlanInputs, width: u32, wait: bool) -> GpuPreview {
    lock(log).complete_with = wait.then(|| Arc::clone(&gpu));
    let preview = drive_tests::preview_on(log, gpu);
    // The backend, the check, the runs and the shaders warm: a few frames at the start of the drag's range.
    for n in 0..4 {
        let _ = preview.frame(inputs, Want::at(0.2 + f64::from(n) * 0.033), width);
    }
    drive_tests::wait_open(&preview);
    for n in 0..6 {
        let _ = preview.frame(inputs, Want::at(0.3 + f64::from(n) * 0.033), width);
    }
    lock(log).presented.clear();
    lock(log).at.clear();
    preview
}

/// What a drag is drawn at: how wide, whether the screen waits for the GPU to finish a frame, and where
/// in the cut it starts.
#[derive(Clone, Copy)]
struct View {
    width: u32,
    wait: bool,
    from: f64,
    /// The project is 120 clips on four tracks (12 assets that are not there), not the two proxies.
    big: bool,
}

const STEPS: usize = 60;
const STEP: f64 = 1.0 / 30.0;

/// Every position of a drag drawn one after the other, nothing between them but `gap`: the cost of a
/// frame at rest. With `forget`, the inputs are taken from the project for every frame.
fn steady(name: String, gpu: &Arc<Gpu>, clips: &[PathBuf; 2], view: View, forget: bool, gap: Duration) -> Drag {
    let View { width, wait, from, big } = view;
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let (take, _project) = drive_tests::taker(if big {
        big_project(clips[0].parent().unwrap(), 12, 120)
    } else {
        proxy_project(clips)
    });
    let first = take().expect("inputs").resolve();
    let preview = warmed_preview(&log, Arc::clone(gpu), &first, width, wait);
    let taken0 = preview.snapshots.stats().taken;
    let t0 = Instant::now();
    let (mut latency, mut parts, mut inputs_us) = (Vec::new(), Vec::new(), Vec::new());
    for n in 0..STEPS {
        let asked = Instant::now();
        if forget {
            preview.snapshots.forget();
        }
        let got = Instant::now();
        preview.inputs(&take).expect("inputs");
        inputs_us.push(us(got.elapsed()));
        let request = FrameRequest::new(Arc::clone(&take), from + n as f64 * STEP, width, false);
        let answer = preview.serve(&request).expect("a frame");
        assert_eq!(answer.renderer, "gpu", "{:?}", answer.reasons);
        parts.extend(answer.timings);
        latency.push(ms(asked.elapsed()));
        std::thread::sleep(gap);
    }
    Drag {
        name,
        requests: STEPS,
        drawn: STEPS,
        wall: t0.elapsed(),
        latency,
        taken: preview.snapshots.stats().taken - taken0,
        parts,
        inputs_us,
    }
}

/// What the page and the backend did: the pointer moves the playhead a frame every `pace`, the page
/// asks for the newest position when its last answer is in (one request in flight, `gap` for the round
/// trip) and the backend takes the inputs from the project for every frame.
fn page_single_flight(name: String, gpu: &Arc<Gpu>, clips: &[PathBuf; 2], view: View, pace: Duration, gap: Duration) -> Drag {
    let View { width, wait, from, big } = view;
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let (take, _project) = drive_tests::taker(if big {
        big_project(clips[0].parent().unwrap(), 12, 120)
    } else {
        proxy_project(clips)
    });
    let first = take().expect("inputs").resolve();
    let preview = warmed_preview(&log, Arc::clone(gpu), &first, width, wait);
    let taken0 = preview.snapshots.stats().taken;
    let (tx, rx) = mpsc::channel::<(usize, Instant)>();
    let t0 = Instant::now();
    let pointer = std::thread::spawn(move || {
        for n in 0..STEPS {
            let _ = tx.send((n, Instant::now()));
            std::thread::sleep(pace);
        }
    });
    let (mut asked, mut parts) = (Vec::new(), Vec::new());
    while let Ok(mut latest) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            latest = newer;
        }
        preview.snapshots.forget();
        let request = FrameRequest::new(Arc::clone(&take), from + latest.0 as f64 * STEP, width, false);
        let answer = preview.serve(&request).expect("a frame");
        assert_eq!(answer.renderer, "gpu", "{:?}", answer.reasons);
        parts.extend(answer.timings);
        asked.push(latest);
        std::thread::sleep(gap);
        if latest.0 == STEPS - 1 {
            break;
        }
    }
    pointer.join().unwrap();
    let shown = lock(&log).at.clone();
    let latency: Vec<f64> = asked
        .iter()
        .zip(&shown)
        .map(|((_, arrived), at)| ms(at.saturating_duration_since(*arrived)))
        .collect();
    Drag {
        name,
        requests: STEPS,
        drawn: asked.len(),
        wall: shown.last().map_or(Duration::ZERO, |at| at.saturating_duration_since(t0)),
        latency,
        taken: preview.snapshots.stats().taken - taken0,
        parts,
        inputs_us: Vec::new(),
    }
}

/// The same pointer into the latest-wins mailbox, inputs held in a snapshot.
fn mailbox(name: String, gpu: &Arc<Gpu>, clips: &[PathBuf; 2], view: View, pace: Duration) -> Drag {
    let View { width, wait, from, big } = view;
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let (take, _project) = drive_tests::taker(if big {
        big_project(clips[0].parent().unwrap(), 12, 120)
    } else {
        proxy_project(clips)
    });
    let first = take().expect("inputs").resolve();
    let preview = Arc::new(warmed_preview(&log, Arc::clone(gpu), &first, width, wait));
    let taken0 = preview.snapshots.stats().taken;
    let t0 = Instant::now();
    let mut posted = Vec::new();
    for n in 0..STEPS {
        let request = FrameRequest::new(Arc::clone(&take), from + n as f64 * STEP, width, false);
        let arrived = request.arrived;
        let answer = preview.post(request, |job| {
            std::thread::spawn(job);
        });
        posted.push((arrived, answer));
        std::thread::sleep(pace);
    }
    let (mut latency, mut parts) = (Vec::new(), Vec::new());
    let mut drawn = 0;
    for (arrived, answer) in posted {
        if let Delivery::Done(result) = answer.blocking_recv().expect("answered") {
            let result = result.expect("a frame");
            assert_eq!(result.renderer, "gpu", "{:?}", result.reasons);
            drawn += 1;
            parts.extend(result.timings);
            let shown = lock(&log).at.get(drawn - 1).copied().unwrap_or_else(Instant::now);
            latency.push(ms(shown.saturating_duration_since(arrived)));
        }
    }
    let last = lock(&log).at.last().copied().unwrap_or(t0);
    Drag {
        name,
        requests: STEPS,
        drawn,
        wall: last.saturating_duration_since(t0),
        latency,
        taken: preview.snapshots.stats().taken - taken0,
        parts,
        inputs_us: Vec::new(),
    }
}

#[test]
#[ignore = "timings; KERF_BENCH=1; needs ffmpeg and a GPU adapter"]
fn a_drag_through_the_frame_path() {
    let _turn = drive_tests::exclusive();
    if !bench_on() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("kerf-app-bench-drag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clips = proxy_clips(&dir);
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let mut out = format!(
        "# a forward drag of {STEPS} positions (a frame of timeline per pointer event) over 1280x720 all-intra clips on {}
         # (a software adapter makes the GPU part of a frame tens of milliseconds; the lines differ by what surrounds it)
         # steady: every position drawn in turn, at rest. before: the page's single flight (the newest position when the last answer is in,
         #   3 ms for the round trip) and the inputs taken from the project for every frame. after: the pointer into the latest-wins mailbox, inputs held.
",
        gpu.adapter_info().name
    );
    for (title, view) in [
        (
            "rendered 800 px wide; the screen waits for the GPU to finish each frame (lavapipe's raster is the frame)",
            View {
                width: 800,
                wait: true,
                from: 0.5,
                big: false,
            },
        ),
        (
            "rendered 800 px wide; CPU side only: the GPU's work is not waited for, as a real present does not",
            View {
                width: 800,
                wait: false,
                from: 0.5,
                big: false,
            },
        ),
        (
            "rendered 400 px wide over a gap in the cut (no layer: nothing to decode, the cheapest frame the path draws)",
            View {
                width: 400,
                wait: true,
                from: 11.0,
                big: false,
            },
        ),
        (
            "rendered 400 px wide over a gap in the cut, in a project of 120 clips on four tracks (the plan is made from all of them)",
            View {
                width: 400,
                wait: true,
                from: 130.0,
                big: true,
            },
        ),
    ] {
        let _ = writeln!(out, "\n## {title}");
        let mut rows = vec![
            steady(
                "steady, inputs taken per frame".to_string(),
                &gpu,
                &clips,
                view,
                true,
                Duration::ZERO,
            ),
            steady("steady, held snapshot".to_string(), &gpu, &clips, view, false, Duration::ZERO),
        ];
        for (hz, pace) in [(125, Duration::from_millis(8)), (60, Duration::from_millis(16))] {
            rows.push(page_single_flight(
                format!("before: page single-flight, pointer {hz} Hz"),
                &gpu,
                &clips,
                view,
                pace,
                Duration::from_millis(3),
            ));
            rows.push(mailbox(format!("after:  mailbox, pointer {hz} Hz"), &gpu, &clips, view, pace));
        }
        for row in &mut rows {
            let _ = writeln!(out, "{}", row.line());
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    report_to("bench-drag.txt", &out);
}
