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
        svg_overlays: Vec::new(),
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

type Answer = (u64, Instant, Delivery<Result<PreviewFrameResult, String>>);

/// How long the playhead rests after a play is primed before Play is pressed in the bench's "rested" start.
const RESTED: Duration = Duration::from_millis(600);

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

// ---- GPU playback ---------------------------------------------------------------------------------

/// CPU seconds (user + system) used by this process and by the children it has waited for
/// (`/proc/self/stat`; Linux, `CLK_TCK` 100): the cursors' `ffmpeg`s are counted when they are reaped.
fn cpu_seconds() -> (f64, f64) {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // Fields 14 to 17 (utime, stime, cutime, cstime), after the `(comm)` that may hold spaces.
    let fields: Vec<f64> = stat
        .rsplit(')')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .skip(11)
        .take(4)
        .filter_map(|f| f.parse::<f64>().ok())
        .map(|ticks| ticks / 100.0)
        .collect();
    match fields.as_slice() {
        [u, s, cu, cs] => (u + s, cu + cs),
        _ => (0.0, 0.0),
    }
}

/// The project a play bench plays: the two proxies (5 s each) as a hard cut with one layer on screen at a
/// time (`heavy` false) or, `heavy`, as a 0.6 s dissolve with a translucent picture-in-picture, a rotated one
/// and a title over them for the whole of it.
fn play_project(clips: &[PathBuf; 2], heavy: bool) -> Project {
    use kerf_core::{Clip, TextOverlay, Transition, TransitionKind};
    let project = Project::open_in_memory().expect("a project");
    let a = project.insert_or_get_asset(&drive_tests::probed(&clips[0])).expect("asset");
    let b = project.insert_or_get_asset(&drive_tests::probed(&clips[1])).expect("asset");
    let video = |clips: Vec<Clip>, name: &str| Track {
        clips,
        ..Track::new(StreamKind::Video, name)
    };
    let mut second = Clip::new(b.id, 0.0, 5.0, 5.0);
    let mut tracks = Vec::new();
    let mut overlays = Vec::new();
    if heavy {
        second.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.6,
        });
        let mut pip = Clip::new(b.id, 0.5, 6.0, 0.0);
        pip.transform.scale = 0.4;
        pip.transform.pos_x = 0.25;
        pip.transform.pos_y = -0.2;
        pip.transform.opacity = 0.8;
        let mut turned = Clip::new(a.id, 1.0, 6.0, 0.0);
        turned.transform.scale = 0.3;
        turned.transform.pos_x = -0.3;
        turned.transform.pos_y = 0.25;
        turned.transform.rotation = 8.0;
        tracks.push(video(vec![Clip::new(a.id, 0.0, 5.0, 0.0), second], "V1"));
        tracks.push(video(vec![pip], "V2"));
        tracks.push(video(vec![turned], "V3"));
        overlays.push(TextOverlay::new("A heavy cut", 0.5, 9.5));
    } else {
        tracks.push(video(vec![Clip::new(a.id, 0.0, 5.0, 0.0), second], "V1"));
    }
    let timeline = Timeline {
        tracks,
        overlays,
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    };
    project.save_timeline(&timeline).expect("a timeline");
    project
}

/// What one simulated play of a cut came to.
struct Played {
    name: String,
    /// Seconds of the cut played.
    seconds: f64,
    drawn: usize,
    repeats: usize,
    superseded: usize,
    /// How far from the frame's own time it reached the screen, ms (positive: late).
    offsets: Vec<f64>,
    /// The first frame, from the first request to its answer.
    first_ms: f64,
    trips: Vec<f64>,
    /// CPU seconds: this process, and the cursors.
    process_cpu: f64,
    children_cpu: f64,
    /// Where the GPU path's own parts of a frame went, ms (medians).
    decode_ms: f64,
    record_ms: f64,
    present_ms: f64,
}

impl Played {
    fn line(&mut self) -> String {
        let within = self.offsets.iter().filter(|o| (0.0..=33.4).contains(*o)).count();
        let frames = self.drawn.max(1) as f64;
        format!(
            "{:<34} {:>2.0} s: {:>3} drawn {:>3} repeats {:>3} replaced | first frame {:>6.1} ms | frame time to screen p50 {:>6.1} p95 {:>6.1} max {:>6.1} ms ({:>3.0}% inside the frame's own slot) | round trip p50 {:>5.1} ms | CPU per drawn frame: process {:>5.1} ms, cursors {:>5.1} ms | decode wait {:>4.1} record {:>4.1} present {:>5.1} ms",
            self.name,
            self.seconds,
            self.drawn,
            self.repeats,
            self.superseded,
            self.first_ms,
            median(&mut self.offsets.clone()),
            percentile(&mut self.offsets.clone(), 0.95),
            self.offsets.iter().copied().fold(f64::MIN, f64::max),
            100.0 * within as f64 / self.offsets.len().max(1) as f64,
            median(&mut self.trips.clone()),
            self.process_cpu / frames * 1e3,
            self.children_cpu / frames * 1e3,
            self.decode_ms,
            self.record_ms,
            self.present_ms,
        )
    }
}

/// The page, simulated: a 60 Hz tick reads a clock that runs from `from` at the moment the session starts, asks
/// for the picture of `clock + median round trip` once per frame of the cut (the first request alone until it is
/// answered), through the real mailbox, and every answer is timed against the frame's own time.
fn play_through(name: &str, preview: &Arc<GpuPreview>, take: &TakeInputs, from: f64, seconds: f64, width: u32) -> Played {
    const FPS: f64 = 30.0;
    let (cpu0, kids0) = cpu_seconds();
    let (tx, rx) = mpsc::channel::<Answer>();
    let session = 77;
    let started = Instant::now();
    let tick = Duration::from_micros(16_667);
    let mut next = started;
    let mut seq = 0u64;
    let (mut last_asked, mut decided, mut sent_at) = (None::<u64>, false, std::collections::HashMap::new());
    let mut trips: Vec<f64> = Vec::new();
    let (mut drawn, mut repeats, mut superseded) = (0usize, 0usize, 0usize);
    let (mut offsets, mut first_ms) = (Vec::new(), 0.0);
    let (mut decode, mut record, mut present) = (Vec::new(), Vec::new(), Vec::new());
    let mut answers = |rx: &mpsc::Receiver<Answer>,
                       decided: &mut bool,
                       trips: &mut Vec<f64>,
                       sent_at: &std::collections::HashMap<u64, Instant>| {
        while let Ok((n, at, delivery)) = rx.try_recv() {
            match delivery {
                Delivery::Superseded => superseded += 1,
                Delivery::Abandoned => panic!("the worker died"),
                Delivery::Done(result) => {
                    let result = result.expect("an answer");
                    assert_eq!(result.renderer, "gpu", "{:?}", result.reasons);
                    let play = result.play.expect("play");
                    *decided = true;
                    if play.repeat {
                        repeats += 1;
                        continue;
                    }
                    drawn += 1;
                    let trip = ms(at.saturating_duration_since(sent_at[&n]));
                    if first_ms == 0.0 {
                        first_ms = trip;
                    }
                    trips.push(trip);
                    // The frame's own time, on the clock that ran from `from` when the session started.
                    let own = play.start + play.frame as f64 / FPS - from;
                    offsets.push(ms(at.saturating_duration_since(started)) - own * 1e3);
                    if let Some(t) = result.timings {
                        decode.push(t.decode_ms);
                        record.push(t.composite_ms);
                        present.push(t.present_ms);
                    }
                }
            }
        }
    };
    loop {
        next += tick;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
        answers(&rx, &mut decided, &mut trips, &sent_at);
        let clock = from + started.elapsed().as_secs_f64();
        if clock - from > seconds {
            break;
        }
        let recent: Vec<f64> = trips.iter().rev().take(9).copied().collect();
        let trip = if recent.is_empty() {
            0.0
        } else {
            median(&mut recent.clone()) / 1e3
        };
        let want = clock + trip.min(0.25);
        let frame = (want * FPS + 1e-6).floor() as u64;
        if last_asked.is_some_and(|l| frame <= l) || (!decided && last_asked.is_some()) {
            continue;
        }
        last_asked = Some(frame);
        seq += 1;
        let request = FrameRequest::new(Arc::clone(take), want, width, false)
            .ordered("page", seq)
            .forward("page", session);
        let sent = Instant::now();
        sent_at.insert(seq, sent);
        let answer = preview.post(request, |job| {
            std::thread::spawn(job);
        });
        let tx = tx.clone();
        let n = seq;
        std::thread::spawn(move || {
            if let Ok(delivery) = answer.blocking_recv() {
                let _ = tx.send((n, Instant::now(), delivery));
            }
        });
    }
    // Let the last answers in.
    std::thread::sleep(Duration::from_millis(400));
    answers(&rx, &mut decided, &mut trips, &sent_at);
    preview.end_play();
    let (cpu1, kids1) = cpu_seconds();
    Played {
        name: name.to_string(),
        seconds,
        drawn,
        repeats,
        superseded,
        offsets,
        first_ms,
        trips,
        process_cpu: cpu1 - cpu0,
        children_cpu: kids1 - kids0,
        decode_ms: median(&mut decode),
        record_ms: median(&mut record),
        present_ms: median(&mut present),
    }
}

/// `stream_preview` for `seconds`, as `start_playback` runs it: the time to the first frame, and what the
/// `ffmpeg` it ran cost per frame.
fn stream_through(name: &str, inputs: &PlanInputs, from: f64, seconds: f64, width: u32) -> (f64, usize, f64, String) {
    let (_, kids0) = cpu_seconds();
    let started = Instant::now();
    let mut first = None;
    let mut frames = 0usize;
    let request = kerf_core::PlaybackRequest {
        fps: None,
        max_width: Some(width),
    };
    kerf_core::stream_preview_with(&inputs.timeline, &inputs.preview_assets, from, request, &mut |_| {
        first.get_or_insert_with(|| started.elapsed());
        frames += 1;
        started.elapsed().as_secs_f64() < seconds
    })
    .expect("the stream");
    let (_, kids1) = cpu_seconds();
    let per_frame = (kids1 - kids0) / frames.max(1) as f64 * 1e3;
    (
        first.map_or(0.0, ms),
        frames,
        per_frame,
        format!(
            "{name}: {frames} frames in {seconds:.0} s, first frame {:.1} ms, ffmpeg CPU per frame {per_frame:.1} ms",
            first.map_or(0.0, ms)
        ),
    )
}

/// How long after a `ready` answer the page begins the session that takes the play back: it asks
/// `READY_LEAD` (0.6 s) ahead and begins a first frame before the time made ready.
const TAKEN_AFTER: Duration = Duration::from_millis(500);

#[test]
#[ignore = "timings; KERF_BENCH=1; needs ffmpeg and a GPU adapter"]
fn a_play_session_through_the_frame_path() {
    let _turn = drive_tests::exclusive();
    if !bench_on() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("kerf-app-bench-play-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clips = proxy_clips(&dir);
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let mut out = format!(
        "# GPU playback over 1280x720 all-intra clips on {} (a software adapter: its raster is the frame time, so GPU timings here are not a real GPU's)\n\
         # first frame: the first request of a session to its answer. start: Play to the first frame on the screen, warm backend (no IPC: add 1 to 3 ms).\n\
         # A/V: how far from the frame's own time the frame reached the screen, on a clock that runs from the start of the play; the sound's output latency is not in it.\n",
        gpu.adapter_info().name
    );
    for (title, heavy, from, width) in [
        ("light: one layer, a hard cut at 5 s", false, 3.5, 960u32),
        (
            "heavy: base + dissolve + a translucent PiP + a rotated PiP + a title",
            true,
            3.5,
            960,
        ),
    ] {
        let _ = writeln!(out, "\n## {title}");
        let (take, _shared) = drive_tests::taker(play_project(&clips, heavy));
        let log = Arc::new(Mutex::new(ScreenLog::default()));
        lock(&log).complete_with = Some(Arc::clone(&gpu));
        let preview = Arc::new(drive_tests::preview_on(&log, Arc::clone(&gpu)));
        let inputs = preview.inputs(&take).expect("inputs");
        // Backend, check and shaders warm (the first session of a run is the stream's: the check is pending).
        let key = |n| PlayKey {
            client: "page".into(),
            session: n,
        };
        let _ = preview.play(&inputs, &key(1), 0.0, false);
        drive_tests::wait_open(&preview);
        for n in 0..3 {
            let answer = preview.play(&inputs, &key(2 + n), 0.2, false).expect("an answer");
            assert_eq!(answer.renderer, "gpu", "{:?}", answer.reasons);
        }
        preview.end_play();

        // Start latency: a play from a playhead nothing was made ready for (cold), from one that rested on a paused
        // frame long enough to be primed and then for `RESTED` more (the pointer going to the Play button), from
        // one played the moment the priming was done (no head start for the `ffmpeg`s), and the stream's first
        // JPEG for the same start. The GPU's own wait (`present`: a CPU rasteriser's raster) is kept apart, as
        // what a start costs on a device whose frames are not the CPU's.
        let (mut gpu_starts, mut primed_starts, mut quick_starts, mut stream_starts) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut gpu_decode, mut primed_decode, mut quick_decode) = (Vec::new(), Vec::new(), Vec::new());
        let (mut gpu_wait, mut primed_wait, mut quick_wait) = (Vec::new(), Vec::new(), Vec::new());
        // Taking a play back from the stream: the `ready` request (the scan from there, the play primed), then the
        // first frame of the session the page begins `READY_LEAD` later, which adopts it.
        let (mut ready_ms, mut taken_starts, mut taken_decode, mut taken_wait) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let primed_play = |at: f64, n: u64, dwell: Duration| {
            // The playhead rests on the paused frame at `at`; the play from it is made ready.
            let rest = FrameRequest::new(Arc::clone(&take), at, width, false);
            let _ = preview
                .post(rest, |job| {
                    std::thread::spawn(job);
                })
                .blocking_recv();
            let until = Instant::now() + Duration::from_secs(10);
            while lock(&preview.primed).is_none() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(lock(&preview.primed).is_some(), "the play was not primed");
            std::thread::sleep(dwell);
            let current = preview.inputs(&take).expect("inputs");
            let began = Instant::now();
            // The picture is asked for what is audible: a little before the playhead.
            let answer = preview.play(&current, &key(n), at - 0.04, false).expect("an answer");
            assert_eq!(answer.renderer, "gpu", "{:?}", answer.reasons);
            let started = ms(began.elapsed());
            preview.end_play();
            std::thread::sleep(Duration::from_millis(150));
            (started, answer.timings)
        };
        for n in 0..8u64 {
            let at = 0.4 + n as f64 * 0.37;
            let began = Instant::now();
            let answer = preview.play(&inputs, &key(10 + n), at, false).expect("an answer");
            assert_eq!(answer.renderer, "gpu", "{:?}", answer.reasons);
            gpu_starts.push(ms(began.elapsed()));
            gpu_decode.extend(answer.timings.map(|t| t.decode_ms));
            gpu_wait.extend(answer.timings.map(|t| t.present_ms));
            preview.end_play();
            std::thread::sleep(Duration::from_millis(150));

            let (started, timings) = primed_play(at, 100 + n, RESTED);
            primed_starts.push(started);
            primed_decode.extend(timings.map(|t| t.decode_ms));
            primed_wait.extend(timings.map(|t| t.present_ms));
            let (started, timings) = primed_play(at, 200 + n, Duration::ZERO);
            quick_starts.push(started);
            quick_decode.extend(timings.map(|t| t.decode_ms));
            quick_wait.extend(timings.map(|t| t.present_ms));

            let current = preview.inputs(&take).expect("inputs");
            let began = Instant::now();
            let ready = preview.ready_play(&current, &key(300 + n), at, false).expect("an answer");
            assert_eq!(ready.renderer, "ready", "{:?}", ready.reasons);
            ready_ms.push(ms(began.elapsed()));
            let resume = ready.play.and_then(|p| p.resume).expect("where");
            std::thread::sleep(TAKEN_AFTER);
            let began = Instant::now();
            let answer = preview.play(&current, &key(400 + n), resume, false).expect("an answer");
            assert_eq!(answer.renderer, "gpu", "{:?}", answer.reasons);
            assert!(lock(&preview.primed).is_none(), "the session adopted the play made ready");
            taken_starts.push(ms(began.elapsed()));
            taken_decode.extend(answer.timings.map(|t| t.decode_ms));
            taken_wait.extend(answer.timings.map(|t| t.present_ms));
            preview.end_play();
            std::thread::sleep(Duration::from_millis(150));

            let began = Instant::now();
            let mut first = None;
            let request = kerf_core::PlaybackRequest {
                fps: None,
                max_width: Some(width),
            };
            kerf_core::stream_preview_with(&inputs.timeline, &inputs.preview_assets, at, request, &mut |_| {
                first = Some(began.elapsed());
                false
            })
            .expect("the stream");
            stream_starts.push(first.map_or(0.0, ms));
            std::thread::sleep(Duration::from_millis(150));
        }
        let row = |label: &str, v: &mut Vec<f64>| {
            format!(
                "{label}: min {:>6.1} p50 {:>6.1} p95 {:>6.1} ms",
                v.iter().copied().fold(f64::MAX, f64::min),
                median(&mut v.clone()),
                percentile(v, 0.95)
            )
        };
        let net = |starts: &[f64], wait: &[f64]| -> Vec<f64> { starts.iter().zip(wait).map(|(s, w)| s - w).collect() };
        for (label, starts, decode, wait) in [
            ("cold   ", &gpu_starts, &mut gpu_decode, &gpu_wait),
            ("rested ", &primed_starts, &mut primed_decode, &primed_wait),
            ("instant", &quick_starts, &mut quick_decode, &quick_wait),
            ("taken back", &taken_starts, &mut taken_decode, &taken_wait),
        ] {
            let _ = writeln!(
                out,
                "start latency, GPU session {label} (Play to first frame): {}",
                row("", &mut starts.clone())
            );
            let _ = writeln!(
                out,
                "  ...of which the wait for the cursors' first frames:      {}",
                row("", decode)
            );
            let _ = writeln!(
                out,
                "  ...without the screen's wait for the GPU:                {}",
                row("", &mut net(starts, wait))
            );
        }
        let _ = writeln!(
            out,
            "start latency, stream_preview         (start to first JPEG): {}",
            row("", &mut stream_starts)
        );
        let _ = writeln!(
            out,
            "a play taken back from the stream: the ready request (scan + primed): {}",
            row("", &mut ready_ms)
        );

        // A play across the cut (5 s) with the page simulated, and the stream for the same span.
        let mut played = play_through(
            if heavy { "GPU session, heavy" } else { "GPU session, light" },
            &preview,
            &take,
            from,
            4.0,
            width,
        );
        let _ = writeln!(out, "{}", played.line());
        let (_, _, _, line) = stream_through(
            if heavy {
                "stream_preview, heavy"
            } else {
                "stream_preview, light"
            },
            &inputs,
            from,
            4.0,
            width,
        );
        let _ = writeln!(out, "{line}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    report_to("bench-play.txt", &out);
}

// ---- A3: scrubbing and gestures at 1080p ------------------------------------------------------------

/// Two 1920x1080 all-intra clips, 6 s each.
fn hd_clips(dir: &Path) -> [PathBuf; 2] {
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
        make("hd1.mp4", "testsrc2=size=1920x1080:rate=30:duration=6"),
        make("hd2.mp4", "gradients=size=1920x1080:rate=30:duration=6:speed=0.00001"),
    ]
}

/// A panel 1920 device pixels wide: the frame is rendered at 1920x1080.
fn hd_bounds(preview: &GpuPreview) {
    preview
        .set_bounds(&BoundsReport {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
            viewport_width: 2560.0,
            viewport_height: 1440.0,
            visible: true,
            matte: None,
            backdrop: None,
            seq: 0,
        })
        .expect("bounds");
}

struct Walk {
    name: String,
    /// Request to presented (the screen waits for the GPU to finish the frame), ms.
    latency: Vec<f64>,
    parts: Vec<GpuTimings>,
}

impl Walk {
    fn line(&mut self) -> String {
        let part = |f: fn(&GpuTimings) -> f64| median(&mut self.parts.iter().map(f).collect::<Vec<_>>());
        let (decode, record, present) = (part(|t| t.decode_ms), part(|t| t.composite_ms), part(|t| t.present_ms));
        let size = self.parts.first().map_or((0, 0), |t| (t.width, t.height));
        format!(
            "{:<58} {}x{} request to screen p50 {:>6.1} p95 {:>6.1} max {:>6.1} ms; decode {decode:.1} record {record:.1} present+completion {present:.1} ms",
            self.name,
            size.0,
            size.1,
            median(&mut self.latency.clone()),
            percentile(&mut self.latency.clone(), 0.95),
            self.latency.iter().copied().fold(0.0, f64::max),
        )
    }
}

/// `times` drawn one after the other through the frame path (`forward`: decoded reading ahead, as a
/// drag that walks forward is; `gap` between frames, the pointer's pace), each with the draft `draft`
/// gives it, if any.
fn hd_walk(
    name: &str,
    preview: &GpuPreview,
    take: &TakeInputs,
    times: &[f64],
    forward: bool,
    gap: Duration,
    draft: impl Fn(usize) -> Option<PreviewDraft>,
) -> Walk {
    let (mut latency, mut parts) = (Vec::new(), Vec::new());
    for (n, &t) in times.iter().enumerate() {
        let asked = Instant::now();
        let inputs = preview.inputs(take).expect("inputs");
        let want = Want {
            scrubbing: true,
            forward,
            ..Want::at(t)
        };
        let cut = match draft(n) {
            Some(d) => Cut::Drafted(d.apply(&inputs.timeline, &inputs.assets).expect("a draft")),
            None => Cut::Held,
        };
        let answer = preview.frame_in(&inputs, cut, want, 1920).expect("a frame");
        assert_eq!(answer.renderer, "gpu", "{:?}", answer.reasons);
        parts.extend(answer.timings);
        latency.push(ms(asked.elapsed()));
        std::thread::sleep(gap);
    }
    Walk {
        name: name.to_string(),
        latency,
        parts,
    }
}

#[test]
#[ignore = "timings; KERF_BENCH=1; needs ffmpeg and a GPU adapter"]
fn a_scrub_and_a_gesture_at_1080p() {
    let _turn = drive_tests::exclusive();
    if !bench_on() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("kerf-app-bench-hd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let clips = hd_clips(&dir);
    let mut out = format!(
        "# A3: the frame path at a 1920x1080 canvas, 1920x1080 all-intra x264 clips, on {}
# request to screen = the frame's inputs, plan, decode wait, composite and present, the screen waiting for
# the GPU to finish the frame (a software adapter's raster is most of it); 60 frames each, 16 ms apart
# (a pointer at 60 Hz), every frame drawn (no newer request replaces one)
",
        gpu.adapter_info().name
    );
    let step = 1.0 / 30.0;
    let forward: Vec<f64> = (0..60).map(|n| 0.5 + f64::from(n) * step).collect();
    let fast: Vec<f64> = (0..60).map(|n| 0.5 + f64::from(n) * 2.0 * step).collect();
    let backward: Vec<f64> = forward.iter().rev().copied().collect();
    let jumps: Vec<f64> = (0..60u32)
        .map(|n| [0.4, 3.1, 1.7, 4.6, 2.2, 0.9][(n % 6) as usize] + f64::from(n / 6) * step)
        .collect();
    let pace = Duration::from_millis(16);
    for wait in [true, false] {
        let _ = writeln!(
            out,
            "\n## {}",
            if wait {
                "the screen waits for the GPU to finish each frame (lavapipe's raster is the frame)"
            } else {
                "CPU side only: the GPU's work is not waited for, as a real present does not"
            }
        );
        let fresh = || {
            let log = Arc::new(Mutex::new(ScreenLog::default()));
            lock(&log).complete_with = wait.then(|| Arc::clone(&gpu));
            let preview = drive_tests::preview_on(&log, Arc::clone(&gpu));
            hd_bounds(&preview);
            let (take, project) = drive_tests::taker(proxy_project(&clips));
            let inputs = preview.inputs(&take).expect("inputs");
            let _ = preview.frame(&inputs, Want::at(0.1), 1920);
            drive_tests::wait_open(&preview);
            for n in 0..4 {
                let _ = preview.frame(&inputs, Want::at(0.2 + f64::from(n) * step), 1920);
            }
            (preview, take, project)
        };
        let none = |_: usize| None;
        let mut rows = Vec::new();
        {
            let (preview, take, _p) = fresh();
            rows.push(hd_walk(
                "scrub forward, a frame a step, as before (Scrub)",
                &preview,
                &take,
                &forward,
                false,
                pace,
                none,
            ));
        }
        {
            let (preview, take, _p) = fresh();
            rows.push(hd_walk(
                "scrub forward, a frame a step, read ahead (Forward)",
                &preview,
                &take,
                &forward,
                true,
                pace,
                none,
            ));
        }
        {
            let (preview, take, _p) = fresh();
            rows.push(hd_walk(
                "scrub forward, two frames a step, as before (Scrub)",
                &preview,
                &take,
                &fast,
                false,
                pace,
                none,
            ));
        }
        {
            let (preview, take, _p) = fresh();
            rows.push(hd_walk(
                "scrub forward, two frames a step, read ahead (Forward)",
                &preview,
                &take,
                &fast,
                true,
                pace,
                none,
            ));
        }
        {
            let (preview, take, _p) = fresh();
            rows.push(hd_walk(
                "scrub backward, a frame a step (Scrub)",
                &preview,
                &take,
                &backward,
                false,
                pace,
                none,
            ));
        }
        {
            let (preview, take, _p) = fresh();
            rows.push(hd_walk(
                "jumps across both clips (Scrub)",
                &preview,
                &take,
                &jumps,
                false,
                pace,
                none,
            ));
        }
        {
            let (preview, take, project) = fresh();
            let timeline = lock(&project).working_timeline().expect("a timeline");
            let track = timeline.tracks[0].id;
            let clip = timeline.tracks[0].clips[0].clone();
            let at = vec![1.0; 60];
            let scale = |n: usize| {
                let mut c = clip.clone();
                c.transform.scale = 0.5 + 0.5 * n as f64 / 59.0;
                c.color.brightness = 0.2 * n as f64 / 59.0;
                Some(PreviewDraft {
                    clips: vec![draft::DraftClip {
                        track_id: track,
                        clip: c,
                    }],
                    overlays: Vec::new(),
                })
            };
            rows.push(hd_walk(
                "Inspector drag: scale and brightness at a resting playhead",
                &preview,
                &take,
                &at,
                false,
                pace,
                scale,
            ));
            let held = |_: usize| {
                let mut c = clip.clone();
                c.transform.scale = 0.75;
                c.color.brightness = 0.1;
                Some(PreviewDraft {
                    clips: vec![draft::DraftClip {
                        track_id: track,
                        clip: c,
                    }],
                    overlays: Vec::new(),
                })
            };
            rows.push(hd_walk(
                "the same frame held: scale 0.75 and brightness 0.1 each time",
                &preview,
                &take,
                &at,
                false,
                pace,
                held,
            ));
        }
        {
            let (preview, take, project) = fresh();
            let timeline = lock(&project).working_timeline().expect("a timeline");
            let track = timeline.tracks[0].id;
            let second = timeline.tracks[0].clips[1].clone();
            let at = vec![5.5; 60];
            let slide = |n: usize| {
                let mut c = second.clone();
                c.timeline_start = 5.0 + 0.4 * n as f64 / 59.0;
                Some(PreviewDraft {
                    clips: vec![draft::DraftClip {
                        track_id: track,
                        clip: c,
                    }],
                    overlays: Vec::new(),
                })
            };
            rows.push(hd_walk(
                "timeline drag: the clip under the playhead moved later",
                &preview,
                &take,
                &at,
                false,
                pace,
                slide,
            ));
        }
        for row in &mut rows {
            let _ = writeln!(out, "{}", row.line());
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    report_to("bench-scrub-1080p.txt", &out);
}
