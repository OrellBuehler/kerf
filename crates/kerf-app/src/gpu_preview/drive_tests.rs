//! The frame path driven end to end without a window: a real device (lavapipe is enough), a real
//! compositor and frame source over real media, a screen that only records what it was asked to do.
//! `#[ignore]`d like every test that needs an adapter and an FFmpeg:
//! `cargo test -p kerf-app --no-default-features -- --ignored drive_tests`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use kerf_core::{Asset, CompositeColorPolicy};
use kerf_gpu::GpuOptions;

use super::tests::{visible, with};
use super::*;

/// What the screen was asked to do.
#[derive(Default)]
pub(super) struct ScreenLog {
    pub(super) presented: Vec<(u32, u32)>,
    resizes: Vec<(u32, u32)>,
    /// One entry per frame: whether the frame was presented without waiting for the refresh.
    pub(super) skips: Vec<bool>,
    /// When each frame was presented.
    pub(super) at: Vec<Instant>,
    /// Wait for the GPU to finish the frame, as a presented frame is on screen only then.
    pub(super) complete_with: Option<Arc<Gpu>>,
}

pub(super) struct NullScreen {
    pub(super) log: Arc<Mutex<ScreenLog>>,
}

impl Screen for NullScreen {
    fn resize(&mut self, size: (u32, u32)) -> Result<(), GpuError> {
        lock(&self.log).resizes.push(size);
        Ok(())
    }

    fn skip_vsync(&mut self, on: bool) -> Result<(), GpuError> {
        lock(&self.log).skips.push(on);
        Ok(())
    }

    fn present(&mut self, frame: &RenderedFrame, _dest: PixelRect, _surround: Surround) -> Result<(), GpuError> {
        let complete = lock(&self.log).complete_with.clone();
        if let Some(gpu) = complete {
            frame.read_back(&gpu)?;
        }
        let mut log = lock(&self.log);
        log.presented.push((frame.width, frame.height));
        log.at.push(Instant::now());
        Ok(())
    }
}

pub(super) fn passing() -> SelfTestReport {
    SelfTestReport {
        cases: Vec::new(),
        policy: CompositeColorPolicy::FixedBt601,
        elapsed: Duration::from_millis(1),
    }
}

/// Two short all-intra clips, made once per machine (a part file under this process's name, then a
/// rename, so processes running together do not read one another's half).
pub(super) fn media() -> &'static [PathBuf; 2] {
    static MEDIA: OnceLock<[PathBuf; 2]> = OnceLock::new();
    MEDIA.get_or_init(|| {
        let dir = std::env::temp_dir().join("kerf-app-drive-media-v1");
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, lavfi: &str| -> PathBuf {
            let path = dir.join(name);
            if path.exists() {
                return path;
            }
            let part = dir.join(format!("part-{}-{name}", std::process::id()));
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
                .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p"])
                .arg(&part)
                .output()
                .expect("run ffmpeg");
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            std::fs::rename(&part, &path).unwrap();
            path
        };
        [
            make("a.mkv", "testsrc2=size=640x360:rate=30:duration=3"),
            make("b.mkv", "gradients=size=640x360:rate=30:duration=3:speed=0.00001"),
        ]
    })
}

pub(super) fn probed(path: &Path) -> Asset {
    Project::probe_asset(path).expect("probe")
}

/// A project with the two clips one after the other, a second each (a cut at 1 s).
pub(super) fn project() -> Project {
    let project = Project::open_in_memory().expect("a project");
    for path in media() {
        let asset = project.insert_or_get_asset(&probed(path)).expect("asset");
        project.cut_clip(asset.id, 0.0, 1.0).expect("a clip");
    }
    project
}

/// These tests move the process's plan epoch (every project they open does) and one of them reads
/// it, so they take turns.
pub(super) fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    lock(&TURN)
}

/// An epoch of this file's own, for the tests that must not see the bumps of the other tests of the
/// process (every project opened anywhere moves the real one).
static OWN_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn own_epoch() -> u64 {
    OWN_EPOCH.load(Ordering::SeqCst)
}

pub(super) fn taker(project: Project) -> (TakeInputs, Arc<Mutex<Project>>) {
    let shared = Arc::new(Mutex::new(project));
    let again = Arc::clone(&shared);
    (Arc::new(move || Taken::from(&lock(&again))), shared)
}

/// [`taker`] tagging what it takes with this file's own epoch.
fn taker_on_own_epoch(project: Project) -> (TakeInputs, Arc<Mutex<Project>>) {
    let shared = Arc::new(Mutex::new(project));
    let again = Arc::clone(&shared);
    (Arc::new(move || Taken::tagged(&lock(&again), own_epoch())), shared)
}

/// A preview whose backend is a real device, compositor and frame source and a screen that records.
pub(super) fn preview(log: &Arc<Mutex<ScreenLog>>) -> GpuPreview {
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    preview_on(log, gpu)
}

/// [`preview`] on a device the caller opened.
pub(super) fn preview_on(log: &Arc<Mutex<ScreenLog>>, gpu: Arc<Gpu>) -> GpuPreview {
    let log = Arc::clone(log);
    let factory: Box<Factory> = Box::new(move |_| {
        Ok(Backend {
            screen: Box::new(NullScreen { log: Arc::clone(&log) }),
            compositor: Arc::new(Compositor::new(Arc::clone(&gpu)).map_err(|e| e.to_string())?),
            source: FrameSource::new(FrameSourceConfig::default()),
            gpu: Arc::clone(&gpu),
            host: Arc::new(Mutex::new(Host::Null)),
        })
    });
    let mut preview = with(
        Resolution {
            technique: Some(Technique::Child),
            note: None,
        },
        factory,
    );
    preview.checker = Arc::new(|_| Ok(passing()));
    preview.set_enabled(true);
    visible(&preview);
    preview
}

/// Wait for the self-test the first frame started to give its verdict.
pub(super) fn wait_open(preview: &GpuPreview) {
    let key = {
        let state = lock(&preview.render);
        AdapterKey::of(&state.backend.as_ref().expect("a backend").gpu)
    };
    let until = Instant::now() + Duration::from_secs(30);
    while !matches!(preview.check.gate(&key), Gate::Open) {
        assert!(Instant::now() < until, "the self-test never gave a verdict");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_first_frame_waits_for_the_self_test_and_the_ones_after_it_are_the_gpus() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let preview = preview(&log);
    let inputs = PlanInputs::of(&project()).expect("inputs");

    // The first frame that wants the GPU starts the check and is FFmpeg's, with the reason.
    let first = preview.frame(&inputs, Want::at(0.5), 320).expect("a frame");
    assert_eq!(first.renderer, "ffmpeg");
    assert!(first.reasons[0].contains("checking the GPU"), "{:?}", first.reasons);
    assert!(lock(&log).presented.is_empty());
    wait_open(&preview);

    let second = preview.frame(&inputs, Want::at(0.5), 320).expect("a frame");
    assert_eq!(second.renderer, "gpu", "{:?}", second.reasons);
    let timings = second.timings.expect("timings");
    assert_eq!(lock(&log).presented, vec![(timings.width, timings.height)]);
    assert_eq!(lock(&log).skips, vec![false], "a click is shown with vsync");

    // What was counted.
    let d = preview.diagnostics();
    assert_eq!((d.tally.gpu_frames, d.tally.ffmpeg_frames), (1, 1));
    assert_eq!(d.tally.refusals[0].kind, "self_test_pending");
    assert!(d.self_test.unwrap().starts_with("passed"));
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_self_test_that_fails_keeps_every_frame_on_ffmpeg_with_its_reason() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let mut preview = preview(&log);
    preview.checker = Arc::new(|_| {
        Err(SelfTestError::Mismatch(SelfTestReport {
            cases: Vec::new(),
            policy: CompositeColorPolicy::BottomLayerTag,
            elapsed: Duration::from_millis(1),
        }))
    });
    let inputs = PlanInputs::of(&project()).expect("inputs");
    let first = preview.frame(&inputs, Want::at(0.5), 320).expect("a frame");
    assert_eq!(first.renderer, "ffmpeg");
    let until = Instant::now() + Duration::from_secs(30);
    while !lock(&preview.info)
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("off for this session"))
    {
        assert!(Instant::now() < until, "no verdict");
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..3 {
        let frame = preview.frame(&inputs, Want::at(0.5), 320).expect("a frame");
        assert_eq!(frame.renderer, "ffmpeg");
        assert!(
            frame.reasons[0].contains("does not draw what this FFmpeg draws"),
            "{:?}",
            frame.reasons
        );
    }
    assert!(lock(&log).presented.is_empty(), "the GPU drew nothing");
    assert!(preview.status().reason.unwrap().contains("off for this session"));
    assert!(preview.diagnostics().self_test.unwrap().starts_with("closed"));
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_render_lock_is_not_held_while_a_frame_waits_for_its_decode() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let preview = Arc::new(preview(&log));
    let inputs = Arc::new(PlanInputs::of(&project()).expect("inputs"));
    // Build the backend and pass the check with a first frame.
    let _ = preview.frame(&inputs, Want::at(0.5), 320);
    wait_open(&preview);

    // The next frame stops where it would decode.
    let (reached_tx, reached_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    *lock(&preview.before_decode) = Some(Arc::new(move || {
        let _ = reached_tx.send(());
        let _ = lock(&go_rx).recv_timeout(Duration::from_secs(30));
    }));
    let frame = {
        let (preview, inputs) = (Arc::clone(&preview), Arc::clone(&inputs));
        std::thread::spawn(move || preview.frame(&inputs, Want::at(0.5), 320))
    };
    reached_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the frame reached its decode");

    // With the frame waiting: the render lock is free, and the setting can go off and the backend be
    // torn down without waiting for it (it used to wait for the decode, up to its 5 s timeout).
    assert!(preview.render.try_lock().is_ok(), "the render lock is held across the decode");
    let t0 = Instant::now();
    assert!(preview.set_enabled(false));
    preview.teardown_if_off();
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "the teardown waited for the frame: {:?}",
        t0.elapsed()
    );
    assert!(lock(&preview.render).backend.is_none());

    // The frame that was waiting finds the preview gone: FFmpeg's, nothing presented.
    go_tx.send(()).unwrap();
    let result = frame.join().unwrap().expect("a frame");
    assert_eq!(result.renderer, "ffmpeg");
    assert!(lock(&log).presented.is_empty());
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_drag_posts_a_request_a_move_and_the_newest_is_the_one_that_is_drawn() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let mut preview = preview(&log);
    preview.snapshots = PlanSnapshots::new(Duration::from_secs(600), own_epoch);
    let preview = Arc::new(preview);
    let (take, _project) = taker_on_own_epoch(project());
    let ask = |t: f64| {
        let request = FrameRequest::new(Arc::clone(&take), t, 320, false);
        preview.post(request, |job| {
            std::thread::spawn(job);
        })
    };
    // Warm: backend built, check passed, runs started.
    let warm = ask(0.1);
    let first = warm.blocking_recv().expect("answered");
    assert!(matches!(first, Delivery::Done(Ok(_))));
    wait_open(&preview);
    lock(&log).presented.clear();
    lock(&log).skips.clear();

    // A drag: 60 requests 3 ms apart (faster than a frame is drawn), each a step along the clip.
    let mut answers = Vec::new();
    for n in 0..60 {
        answers.push(ask(0.1 + f64::from(n) * 0.012));
        std::thread::sleep(Duration::from_millis(3));
    }
    let mut done = Vec::new();
    let mut superseded = 0;
    for (n, answer) in answers.into_iter().enumerate() {
        match answer.blocking_recv().expect("answered") {
            Delivery::Done(result) => done.push((n, result.expect("a frame"))),
            Delivery::Superseded => superseded += 1,
            Delivery::Abandoned => panic!("the worker died"),
        }
    }
    assert!(superseded > 0, "a drag faster than the frames are drawn drops some");
    assert_eq!(
        done.last().map(|(n, _)| *n),
        Some(59),
        "the last request of the drag is drawn"
    );
    assert!(
        done.iter().all(|(_, r)| r.renderer == "gpu"),
        "{:?}",
        done.iter().map(|(_, r)| &r.reasons).collect::<Vec<_>>()
    );
    assert_eq!(
        lock(&log).presented.len(),
        done.len(),
        "every frame drawn was presented, no other"
    );
    // Close requests are a drag: from the second one on the screen skips the refresh wait.
    let skips = lock(&log).skips.clone();
    assert_eq!(skips.len(), done.len());
    assert!(
        skips.last().copied().unwrap_or(false),
        "the drag's frames are presented without vsync: {skips:?}"
    );

    let d = preview.diagnostics();
    assert_eq!(d.requests.posted, 61);
    assert_eq!(d.requests.superseded, superseded);
    assert_eq!(d.snapshots.taken, 1, "the project was read once for the whole drag");
    assert_eq!(
        d.snapshots.served as usize,
        done.len(),
        "and every request that was drawn was served from the snapshot"
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn an_edit_makes_the_next_frame_take_the_inputs_again() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let preview = Arc::new(preview(&log));
    let (take, shared) = taker(project());
    let ask = |t: f64| {
        let request = FrameRequest::new(Arc::clone(&take), t, 320, false);
        preview
            .post(request, |job| {
                std::thread::spawn(job);
            })
            .blocking_recv()
            .expect("answered")
    };
    for n in 0..5 {
        assert!(matches!(ask(0.1 * f64::from(n)), Delivery::Done(Ok(_))));
    }
    assert_eq!(preview.diagnostics().snapshots.taken, 1);
    // An edit through the project's own API.
    let clip = lock(&shared).working_timeline().unwrap().tracks[0].clips[0].id;
    lock(&shared).set_clip_enabled(clip, false).expect("an edit");
    assert!(matches!(ask(0.5), Delivery::Done(Ok(_))));
    assert_eq!(
        preview.diagnostics().snapshots.taken,
        2,
        "the edit made the inputs be taken again"
    );
    assert!(matches!(ask(0.6), Delivery::Done(Ok(_))));
    assert_eq!(preview.diagnostics().snapshots.taken, 2);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_frame_the_plan_refuses_is_counted_by_kind() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let preview = preview(&log);
    let inputs = PlanInputs::of(&project()).expect("inputs");
    let _ = preview.frame(&inputs, Want::at(0.5), 320);
    wait_open(&preview);
    // The page has a title box over the picture, and this surface sits above the page.
    let result = preview
        .frame(
            &inputs,
            Want {
                overlays: true,
                ..Want::at(0.5)
            },
            320,
        )
        .expect("a frame");
    assert_eq!(result.renderer, "ffmpeg");
    let d = preview.diagnostics();
    assert!(
        d.tally.refusals.iter().any(|r| r.kind == "overlays" && r.count == 1),
        "{:?}",
        d.tally
    );
    assert!(d.tally.line().contains("overlays 1"));
}
