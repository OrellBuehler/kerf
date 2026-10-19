//! GPU playback driven end to end without a window: a real device (lavapipe is enough), compositor
//! and cursors over real media, a screen that records what it was asked to do, and a page that asks
//! for the frames the way the real one does (`play`, a forward request per frame).
//! `#[ignore]`d like every test that needs an adapter and an FFmpeg:
//! `cargo test -p kerf-app --no-default-features -- --ignored play_tests`.

use std::path::PathBuf;
use std::sync::OnceLock;

use kerf_core::TextKeyframe;

use super::drive_tests::{exclusive, media, preview, probed, project, wait_open, ScreenLog};
use super::*;

fn key(session: u64) -> PlayKey {
    PlayKey {
        client: "page".into(),
        session,
    }
}

const FPS: f64 = 30.0;

/// A transport stream, which runs are not trusted for.
fn transport() -> &'static PathBuf {
    static TS: OnceLock<PathBuf> = OnceLock::new();
    TS.get_or_init(|| {
        let dir = std::env::temp_dir().join("kerf-app-drive-media-v1");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.ts");
        if !path.exists() {
            let part = dir.join(format!("part-{}-c.ts", std::process::id()));
            let out = kerf_core::ffmpeg_command()
                .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-f", "lavfi", "-i"])
                .arg("testsrc2=size=640x360:rate=30:duration=3")
                .args([
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "mpegts",
                ])
                .arg(&part)
                .output()
                .expect("run ffmpeg");
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            std::fs::rename(&part, &path).unwrap();
        }
        path
    })
}

/// A preview that has passed its self-test, with a session's first request already made (it is the
/// stream's: the check was pending).
fn ready(log: &Arc<Mutex<ScreenLog>>, inputs: &PlanInputs) -> GpuPreview {
    let preview = preview(log);
    let first = preview.play(inputs, &key(1), 0.0, false).expect("an answer");
    assert_eq!(first.renderer, "stream", "{:?}", first.reasons);
    wait_open(&preview);
    preview
}

fn asked(preview: &GpuPreview, inputs: &PlanInputs, session: u64, frame: u64) -> PreviewFrameResult {
    preview
        .play(inputs, &key(session), frame as f64 / FPS + 0.001, false)
        .expect("an answer")
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < until, "gave up waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_session_draws_the_frames_asked_for_skips_the_ones_a_newer_request_replaced_and_crosses_a_cut() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let inputs = PlanInputs::of(&project()).expect("inputs");
    let preview = preview(&log);

    // The first request of a session while the self-test is pending: the stream plays it.
    let first = preview.play(&inputs, &key(1), 0.0, false).expect("an answer");
    assert_eq!(first.renderer, "stream");
    assert!(first.reasons[0].contains("checking the GPU"), "{:?}", first.reasons);
    assert!(first.play.is_none() && first.frame.is_none());
    assert!(lock(&log).presented.is_empty());
    wait_open(&preview);

    // The same session asks again once the check passed: it begins, and frame 0 is on the screen.
    let begin = preview.play(&inputs, &key(1), 0.001, false).expect("an answer");
    assert_eq!(begin.renderer, "gpu", "{:?}", begin.reasons);
    let info = begin.play.expect("a session");
    assert_eq!((info.session, info.frame, info.repeat), (1, 0, false));
    assert_eq!(info.start, 0.0);
    assert!(info.ahead.is_none(), "{:?}", info.ahead);
    assert!(begin.timings.is_some());
    assert_eq!(lock(&log).presented.len(), 1);

    // Frames in order, one request each.
    for frame in 1..=10 {
        let r = asked(&preview, &inputs, 1, frame);
        assert_eq!(r.renderer, "gpu", "frame {frame}: {:?}", r.reasons);
        assert_eq!(r.play.expect("play").frame, frame);
    }
    assert_eq!(lock(&log).presented.len(), 11);
    // Frames of a session are shown with the refresh, not as a drag.
    assert!(lock(&log).skips.iter().all(|skip| !skip), "{:?}", lock(&log).skips);

    // The same frame again (the page asks per animation frame, the clip has 30 frames a second).
    let again = asked(&preview, &inputs, 1, 10);
    let play = again.play.expect("play");
    assert!(play.repeat && play.frame == 10 && again.timings.is_none());
    assert_eq!(lock(&log).presented.len(), 11, "nothing was drawn");
    // A step back of a frame (the latency estimate moved) is the frame on the screen too.
    let back = asked(&preview, &inputs, 1, 9).play.expect("play");
    assert!(back.repeat && back.frame == 10);

    // Frames 11..19 were replaced by newer requests: asking for 20 skips them.
    let jump = asked(&preview, &inputs, 1, 20);
    assert_eq!(jump.renderer, "gpu");
    assert_eq!(preview.diagnostics().play.skipped, 9);

    // The second clip starts at frame 30: its cursor is opened ahead of the cut by the prefetch.
    wait_for("the next clip's cursor to be waiting", || {
        lock(&preview.play).as_ref().is_some_and(|s| s.waiting() == 1)
    });
    for frame in 21..=40 {
        let r = asked(&preview, &inputs, 1, frame);
        assert_eq!(r.renderer, "gpu", "frame {frame}: {:?}", r.reasons);
    }
    assert_eq!(
        lock(&preview.play).as_ref().map(PlaySession::waiting),
        Some(0),
        "the cut took the cursor that was waiting"
    );

    let d = preview.diagnostics();
    assert_eq!(d.play.sessions, 1);
    assert_eq!(d.play.declined, 1);
    assert_eq!((d.play.frames, d.play.repeats), (1 + 10 + 1 + 20, 2));
    assert_eq!(d.tally.gpu_frames, d.play.frames);

    // Anything that is not part of the session ends it: the paused frame after a pause.
    assert!(lock(&preview.play).is_some());
    let still = preview.frame(&inputs, Want::at(0.5), 320).expect("a frame");
    assert_eq!(still.renderer, "gpu");
    assert!(lock(&preview.play).is_none(), "the session ended with the paused frame");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_session_is_the_streams_for_what_the_gpu_does_not_draw_in_time() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let project = project();
    // A title that moves, from 0.5 s: drawn exactly, but one rasterisation a frame.
    let title = project.add_overlay("Sliding".into(), 0.5, 1.5).expect("a title");
    project
        .set_overlay_keyframes(
            title.id,
            vec![
                TextKeyframe {
                    time: 0.0,
                    pos_x: 0.2,
                    pos_y: 0.8,
                    opacity: 1.0,
                },
                TextKeyframe {
                    time: 1.0,
                    pos_x: 0.8,
                    pos_y: 0.8,
                    opacity: 1.0,
                },
            ],
        )
        .expect("keyframes");
    let inputs = PlanInputs::of(&project).expect("inputs");
    // A session the plan refuses never reaches the check, so a frame starts it.
    let preview = super::drive_tests::preview(&log);
    let _ = preview.frame(&inputs, Want::at(1.8), 320);
    wait_open(&preview);

    // From 0 s the title is within the five seconds ahead: the whole session is the stream's.
    let r = preview.play(&inputs, &key(2), 0.0, false).expect("an answer");
    assert_eq!(r.renderer, "stream");
    assert!(r.reasons[0].contains("Sliding"), "{:?}", r.reasons);
    assert!(lock(&log).presented.is_empty());
    let d = preview.diagnostics();
    assert!(d.tally.refusals.iter().any(|r| r.kind == "moving_text"), "{:?}", d.tally);

    // A page that has something to draw over the picture, on a surface above the page.
    let r = preview.play(&inputs, &key(3), 1.6, true).expect("an answer");
    assert_eq!(r.renderer, "stream");
    assert!(r.reasons[0].contains("sits above the page"), "{:?}", r.reasons);
    // Past the title the same cut plays on the GPU.
    let r = preview.play(&inputs, &key(4), 1.6, false).expect("an answer");
    assert_eq!(r.renderer, "gpu", "{:?}", r.reasons);
    assert_eq!(preview.diagnostics().play.declined, 2, "the title and the overlays");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_refusal_that_comes_into_view_is_reported_with_where_the_stream_starts() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    // 1 s of a clip runs are trusted for, then a transport stream, which they are not.
    let project = Project::open_in_memory().expect("a project");
    let a = project.insert_or_get_asset(&probed(&media()[0])).expect("asset");
    project.cut_clip(a.id, 0.0, 1.0).expect("a clip");
    let ts = project.insert_or_get_asset(&probed(transport())).expect("asset");
    project.cut_clip(ts.id, 0.0, 1.0).expect("a clip");
    let inputs = PlanInputs::of(&project).expect("inputs");
    let preview = ready(&log, &inputs);

    // The plan draws all of it (it cannot know what the files are); the session begins.
    let begin = preview.play(&inputs, &key(2), 0.0, false).expect("an answer");
    assert_eq!(begin.renderer, "gpu", "{:?}", begin.reasons);
    // The prefetch opens the clip two seconds ahead, finds it untrusted, and the answers say so:
    // where the first frame is that the GPU will not draw, and where a stream starts.
    let mut ahead = None;
    wait_for("the prefetch to find the transport stream", || {
        ahead = asked(&preview, &inputs, 2, 1).play.and_then(|p| p.ahead);
        ahead.is_some()
    });
    let ahead = ahead.expect("ahead");
    assert_eq!(ahead.kind, "cursor");
    assert!((ahead.at - 1.0).abs() < 1.0 / FPS, "{}", ahead.at);
    assert!(ahead.handover <= ahead.at && ahead.handover > 0.9, "{}", ahead.handover);
    assert!(ahead.why.contains("c.ts"), "{}", ahead.why);

    // And a frame in that clip, if the page asked for one, ends the session in the stream's favour.
    let past = asked(&preview, &inputs, 2, 31);
    assert_eq!(past.renderer, "stream", "{:?}", past.reasons);
    let at = past.play.and_then(|p| p.ahead).expect("where to start");
    assert!(at.handover <= 31.0 / FPS + 0.001, "{}", at.handover);
    assert!(lock(&preview.play).is_none(), "the session is over");
    assert_eq!(preview.diagnostics().play.handed_over, 1);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_change_a_seek_another_session_and_turning_it_off_each_end_the_session() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let (take, shared) = super::drive_tests::taker(project());
    let preview = Arc::new(preview(&log));
    let inputs = PlanInputs::of(&lock(&shared)).expect("inputs");
    let _ = preview.play(&inputs, &key(1), 0.0, false);
    wait_open(&preview);

    let begin = preview
        .play(&inputs, &key(1), 0.0, false)
        .expect("an answer")
        .play
        .expect("play");
    assert_eq!(begin.start, 0.0);
    asked(&preview, &inputs, 1, 5);
    let sessions = || preview.diagnostics().play.sessions;
    assert_eq!(sessions(), 1);

    // A page that seeks without saying so: more than a second back starts the session over, from there.
    asked(&preview, &inputs, 1, 40);
    let seeked = preview.play(&inputs, &key(1), 0.2, false).expect("an answer");
    let seeked = seeked.play.expect("play");
    assert!((seeked.start - 0.2).abs() < 1.0 / FPS && seeked.frame == 0, "{seeked:?}");
    assert_eq!(sessions(), 2, "a new session began");

    // Another session replaces it.
    let other = preview
        .play(&inputs, &key(2), 0.4, false)
        .expect("an answer")
        .play
        .expect("play");
    assert_eq!(other.session, 2);
    assert!((other.start - 0.4).abs() < 1.0 / FPS);

    // An edit moves the plan epoch: the next request of the same session plans the new cut.
    let clip = lock(&shared).working_timeline().unwrap().tracks[0].clips[0].id;
    lock(&shared).set_clip_enabled(clip, false).expect("an edit");
    let edited = preview.inputs(&take).expect("inputs after the edit");
    let after = preview.play(&edited, &key(2), 1.2, false).expect("an answer");
    assert_eq!(after.renderer, "gpu", "{:?}", after.reasons);
    assert!(
        (after.play.expect("play").start - 1.2).abs() < 1.0 / FPS,
        "a session of the edited cut began"
    );

    // Turning the setting off ends it, and a request after that is not drawn.
    assert!(lock(&preview.play).is_some());
    assert!(preview.set_enabled(false));
    preview.teardown_if_off();
    assert!(lock(&preview.play).is_none());
    let off = preview.play(&edited, &key(2), 1.3, false).expect("an answer");
    assert_eq!(off.renderer, "stream");
    assert!(off.reasons[0].contains("off"), "{:?}", off.reasons);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_playhead_that_rests_primes_the_play_from_it_and_play_starts_from_cursors_that_are_open() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let (take, shared) = super::drive_tests::taker(project());
    let preview = Arc::new(preview(&log));
    let ask = |t: f64| {
        let request = FrameRequest::new(Arc::clone(&take), t, 320, false);
        preview
            .post(request, |job| {
                std::thread::spawn(job);
            })
            .blocking_recv()
            .expect("answered")
    };
    // The first paused frame builds the backend and starts the check; the one after it is the GPU's.
    assert!(matches!(ask(0.1), Delivery::Done(Ok(_))));
    wait_open(&preview);
    let Delivery::Done(Ok(frame)) = ask(0.5) else {
        panic!("a frame")
    };
    assert_eq!(frame.renderer, "gpu", "{:?}", frame.reasons);

    // Cold: a play from a playhead nothing was made ready for.
    let inputs = preview.inputs(&take).expect("inputs");
    assert!(lock(&preview.primed).is_none(), "the playhead has not rested yet");
    let cold = preview.play(&inputs, &key(1), 0.5, false).expect("an answer");
    assert_eq!(cold.renderer, "gpu", "{:?}", cold.reasons);
    let cold_decode = cold.timings.expect("timings").decode_ms;
    preview.end_play();

    // The playhead rests on a frame of the cut: after a moment the play from it is made ready, with
    // the cursor of the frame on the screen open.
    let Delivery::Done(Ok(frame)) = ask(0.2) else {
        panic!("a frame")
    };
    assert_eq!(frame.renderer, "gpu");
    wait_for("the play to be primed", || lock(&preview.primed).is_some());
    let waiting = lock(&preview.primed).as_ref().map(|p| p.motion.feed().waiting());
    assert_eq!(waiting, Some(1), "the cursor of the clip on the screen is open");

    // Play from a little before it (the picture is asked for what is audible) adopts it. The pointer takes a
    // moment to get to the button, and that is what the `ffmpeg` of the cursor has to decode its first frames.
    std::thread::sleep(Duration::from_millis(600));
    let inputs = preview.inputs(&take).expect("inputs");
    let warm = preview.play(&inputs, &key(2), 0.2 - 0.04, false).expect("an answer");
    assert_eq!(warm.renderer, "gpu", "{:?}", warm.reasons);
    assert!(lock(&preview.primed).is_none(), "the play took it");
    let warm_decode = warm.timings.expect("timings").decode_ms;
    assert!(
        warm_decode < cold_decode * 0.8,
        "the first frame waited {warm_decode:.1} ms for cursors that were open (cold: {cold_decode:.1} ms)"
    );
    // It is the session of the playhead: frame 0 is the paused frame.
    let info = warm.play.expect("play");
    assert_eq!((info.frame, info.start), (0, 6.0 / FPS));
    preview.end_play();

    // Another paused frame, a change, and a play from somewhere else each leave nothing primed.
    assert!(matches!(ask(0.2), Delivery::Done(Ok(_))));
    wait_for("the play to be primed", || lock(&preview.primed).is_some());
    assert!(matches!(ask(0.7), Delivery::Done(Ok(_))));
    assert!(lock(&preview.primed).is_none(), "a different paused frame");
    wait_for("the play from the new playhead to be primed", || {
        lock(&preview.primed).is_some()
    });
    let clip = lock(&shared).working_timeline().unwrap().tracks[0].clips[1].id;
    lock(&shared).set_clip_enabled(clip, false).expect("an edit");
    let edited = preview.inputs(&take).expect("inputs after the edit");
    // The edit moved the plan epoch: what was primed is for the cut as it was.
    let after = preview.play(&edited, &key(3), 0.7 - 0.04, false).expect("an answer");
    assert_eq!(after.renderer, "gpu", "{:?}", after.reasons);
    assert!(lock(&preview.primed).is_none());
    preview.end_play();

    // A play started well after the primed start does not adopt it either (the cursors would read
    // forward to it), and nothing is left primed.
    assert!(matches!(ask(0.2), Delivery::Done(Ok(_))));
    wait_for("the play to be primed", || lock(&preview.primed).is_some());
    let edited = preview.inputs(&take).expect("inputs");
    let late = preview.play(&edited, &key(4), 0.9, false).expect("an answer");
    assert_eq!(late.renderer, "gpu", "{:?}", late.reasons);
    assert!(lock(&preview.primed).is_none());

    // The setting off drops it and the work waiting to make one.
    preview.end_play();
    assert!(matches!(ask(0.2), Delivery::Done(Ok(_))));
    wait_for("the play to be primed", || lock(&preview.primed).is_some());
    assert!(preview.set_enabled(false));
    preview.teardown_if_off();
    assert!(lock(&preview.primed).is_none());
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_session_nobody_asks_for_any_more_frees_its_cursors_and_its_thread() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let (take, shared) = super::drive_tests::taker(project());
    let preview = Arc::new(preview(&log));
    // The page's requests set this; the direct calls of a test do not.
    let _ = preview.me.set(Arc::downgrade(&preview));
    preview.idle_ms.store(500, Ordering::SeqCst);
    let inputs = PlanInputs::of(&lock(&shared)).expect("inputs");
    let _ = preview.play(&inputs, &key(1), 0.0, false);
    wait_open(&preview);
    let _ = take;

    assert_eq!(preview.play(&inputs, &key(1), 0.0, false).expect("an answer").renderer, "gpu");
    asked(&preview, &inputs, 1, 5);
    wait_for("the next clip's cursor to be waiting", || {
        lock(&preview.play).as_ref().is_some_and(|s| s.waiting() == 1)
    });
    let feed = lock(&preview.play).as_ref().map(PlaySession::feed).expect("a session");
    assert!(super::busy::working() >= 1, "the session's prefetch thread is running");

    // The page hands over to the stream (or is refused, or gives the GPU up) and asks for nothing more.
    wait_for("the idle session to be ended", || lock(&preview.play).is_none());
    assert_eq!(feed.waiting(), 0, "its idle cursors were killed");
    wait_for("the prefetch thread to end", || super::busy::working() == 0);
    assert_eq!(preview.diagnostics().play.frames, 2, "what it did is counted");

    // A page that asks again after that begins another session.
    let again = preview.play(&inputs, &key(1), 0.4, false).expect("an answer");
    assert_eq!(again.renderer, "gpu", "{:?}", again.reasons);
    assert_eq!(preview.diagnostics().play.sessions, 2);
    // A session that is asked for is not ended: the deadline moves with its requests.
    for frame in 13..20 {
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(asked(&preview, &inputs, 1, frame).renderer, "gpu");
    }
    assert!(lock(&preview.play).is_some());
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_clip_runs_are_not_trusted_for_under_the_playhead_is_the_streams_from_the_first_request() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let project = Project::open_in_memory().expect("a project");
    let ts = project.insert_or_get_asset(&probed(transport())).expect("asset");
    project.cut_clip(ts.id, 0.0, 1.0).expect("a clip");
    let a = project.insert_or_get_asset(&probed(&media()[0])).expect("asset");
    project.cut_clip(a.id, 0.0, 1.0).expect("a clip");
    let inputs = PlanInputs::of(&project).expect("inputs");
    // A session the clip refuses never reaches the check, so a paused frame (of the other clip) starts it.
    let preview = super::drive_tests::preview(&log);
    let _ = preview.frame(&inputs, Want::at(1.5), 320);
    wait_open(&preview);

    let hides = super::HIDES.load(Ordering::SeqCst);
    let declined = preview.diagnostics().play.declined;
    // The plan draws it (it cannot know what the file is); the frame source says no before a frame is tried.
    let r = preview.play(&inputs, &key(2), 0.0, false).expect("an answer");
    assert_eq!(r.renderer, "stream", "{:?}", r.reasons);
    assert!(r.reasons[0].contains("c.ts"), "{:?}", r.reasons);
    assert!(r.play.is_none(), "the page plays the stream from its playhead");
    assert!(lock(&log).presented.is_empty(), "nothing was drawn");
    assert!(lock(&preview.play).is_none());
    assert_eq!(preview.diagnostics().play.declined, declined + 1);
    assert_eq!(super::HIDES.load(Ordering::SeqCst), hides, "the surface was not touched");
    // The other clip, past the first, is the GPU's.
    let past = preview.play(&inputs, &key(3), 1.2, false).expect("an answer");
    assert_eq!(past.renderer, "gpu", "{:?}", past.reasons);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_play_frame_that_fails_leaves_the_surface_up_for_the_stream_to_take_over() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let dir = std::env::temp_dir().join(format!("kerf-app-play-gone-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (here, gone) = (dir.join("here.mkv"), dir.join("gone.mkv"));
    std::fs::copy(&media()[0], &here).unwrap();
    let project = Project::open_in_memory().expect("a project");
    let asset = project.insert_or_get_asset(&probed(&here)).expect("asset");
    project.cut_clip(asset.id, 0.0, 2.0).expect("a clip");
    let inputs = PlanInputs::of(&project).expect("inputs");
    let preview = ready(&log, &inputs);

    // The file is taken away after the session has been looked at and before its first frame opens it.
    let moved = (here, gone);
    *lock(&preview.before_decode) = Some(Arc::new(move || {
        let _ = std::fs::rename(&moved.0, &moved.1);
    }));
    let hides = super::HIDES.load(Ordering::SeqCst);
    let r = preview.play(&inputs, &key(2), 0.0, false).expect("an answer");
    assert_eq!(r.renderer, "stream", "{:?}", r.reasons);
    let at = r.play.and_then(|p| p.ahead).expect("where the stream starts");
    assert!(at.handover <= 0.001, "{}", at.handover);
    assert_eq!(
        super::HIDES.load(Ordering::SeqCst),
        hides,
        "a play frame that fails does not hide the picture the stream takes over from"
    );
    // A still frame that fails does: the page then shows FFmpeg's picture.
    *lock(&preview.before_decode) = None;
    let _ = preview.frame(&inputs, Want::at(0.5), 320);
    assert!(
        super::HIDES.load(Ordering::SeqCst) > hides,
        "a failed still hides the surface"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_frame_at_or_after_the_first_one_the_gpu_does_not_draw_is_not_attempted() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    // Eight seconds, and a title that moves from 6 s: beyond the five seconds a session looks ahead at the start.
    let project = Project::open_in_memory().expect("a project");
    let a = project.insert_or_get_asset(&probed(&media()[0])).expect("asset");
    let b = project.insert_or_get_asset(&probed(&media()[1])).expect("asset");
    project.cut_clip(a.id, 0.0, 3.0).expect("a clip");
    project.cut_clip(b.id, 0.0, 3.0).expect("a clip");
    project.cut_clip(a.id, 0.0, 2.0).expect("a clip");
    let title = project.add_overlay("Sliding".into(), 6.0, 7.0).expect("a title");
    let key_at = |time: f64, pos_x: f64| TextKeyframe {
        time,
        pos_x,
        pos_y: 0.8,
        opacity: 1.0,
    };
    project
        .set_overlay_keyframes(title.id, vec![key_at(0.0, 0.2), key_at(1.0, 0.8)])
        .expect("keyframes");
    let inputs = PlanInputs::of(&project).expect("inputs");
    let preview = ready(&log, &inputs);

    let begin = preview.play(&inputs, &key(2), 0.0, false).expect("an answer");
    assert_eq!(begin.renderer, "gpu", "{:?}", begin.reasons);
    // The title comes into the five seconds ahead as the playhead moves: the answers say so.
    let mid = asked(&preview, &inputs, 2, 45);
    let ahead = mid.play.and_then(|p| p.ahead).expect("the title is in view");
    assert_eq!(ahead.kind, "moving_text");
    assert!((ahead.at - 6.0).abs() < 1.0 / FPS, "{}", ahead.at);
    // A frame before it is the GPU's.
    assert_eq!(asked(&preview, &inputs, 2, 120).renderer, "gpu");
    let presented = lock(&log).presented.len();
    // A frame inside the title (the page's hand-over was late) is not attempted: nothing is drawn.
    let late = asked(&preview, &inputs, 2, 190);
    assert_eq!(late.renderer, "stream", "{:?}", late.reasons);
    assert!(late.reasons[0].contains("Sliding"), "{:?}", late.reasons);
    let info = late.play.and_then(|p| p.ahead).expect("where the stream starts");
    assert_eq!(info.kind, "moving_text");
    assert!(info.handover <= info.at);
    assert_eq!(lock(&log).presented.len(), presented, "no frame was drawn for it");
    assert!(lock(&preview.play).is_none(), "the session is over");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_play_being_served_or_held_is_not_primed_over() {
    let _turn = exclusive();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    let (take, shared) = super::drive_tests::taker(project());
    let preview = Arc::new(preview(&log));
    let inputs = PlanInputs::of(&lock(&shared)).expect("inputs");
    let _ = preview.play(&inputs, &key(1), 0.0, false);
    wait_open(&preview);
    let _ = take;
    assert!(matches!(
        preview.frame(&inputs, Want::at(0.2), 320).map(|r| r.renderer),
        Ok("gpu")
    ));
    // Priming from the playhead the paused frame rests at works...
    assert!(preview.prime(&inputs, 0.2));
    *lock(&preview.primed) = None;
    // ...but not while a request of a play is being served (the session is out of its slot while a
    // frame is drawn, which is when a priming that was already under way would have stored its own)...
    preview.serving.fetch_add(1, Ordering::SeqCst);
    assert!(!preview.prime(&inputs, 0.2));
    assert!(lock(&preview.primed).is_none());
    preview.serving.fetch_sub(1, Ordering::SeqCst);
    // ...nor with a session held.
    assert_eq!(preview.play(&inputs, &key(2), 0.2, false).expect("an answer").renderer, "gpu");
    assert!(!preview.prime(&inputs, 0.2));
    assert!(lock(&preview.primed).is_none());
}

/// The keys of a title that slides across the frame over `seconds`.
fn slide(seconds: f64) -> Vec<TextKeyframe> {
    let key = |time: f64, pos_x: f64| TextKeyframe {
        time,
        pos_x,
        pos_y: 0.8,
        opacity: 1.0,
    };
    vec![key(0.0, 0.2), key(seconds, 0.8)]
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_play_the_stream_took_over_is_taken_back_where_the_gpu_draws_again_from_cursors_made_ready() {
    let _turn = exclusive();
    let (preview, log, _gpu) = super::drive_tests::keeping();
    // Nine seconds of video, and a title that slides from 0.5 s to 1.5 s.
    let project = Project::open_in_memory().expect("a project");
    for path in [&media()[0], &media()[1], &media()[0]] {
        let asset = project.insert_or_get_asset(&probed(path)).expect("asset");
        project.cut_clip(asset.id, 0.0, 3.0).expect("a clip");
    }
    let title = project.add_overlay("Sliding".into(), 0.5, 1.5).expect("a title").id;
    project.set_overlay_keyframes(title, slide(1.0)).expect("keyframes");
    let inputs = PlanInputs::of(&project).expect("inputs");
    let _ = preview.frame(&inputs, Want::at(2.0), 320);
    wait_open(&preview);
    lock(&log).presented.clear();

    // From 0 s the title is within the horizon: the stream plays, and the answer says where the GPU stops.
    let declined = preview.play(&inputs, &key(2), 0.0, false).expect("an answer");
    assert_eq!(declined.renderer, "stream");
    let ahead = declined.play.and_then(|p| p.ahead).expect("where the GPU stops");
    assert_eq!(ahead.kind, "moving_text");
    assert!((ahead.at - 0.5).abs() < 1.0 / FPS, "{}", ahead.at);

    // The stream plays; the page asks where the GPU can take the play back. The first frame after the title
    // starts a run it draws, and the play from there is made ready: nothing is drawn.
    let ready = preview.ready_play(&inputs, &key(3), 0.2, false).expect("an answer");
    assert_eq!(ready.renderer, "ready", "{:?}", ready.reasons);
    let resume = ready.play.and_then(|p| p.resume).expect("where");
    assert!((1.5..1.5 + 2.0 / FPS).contains(&resume), "{resume}");
    assert!(lock(&log).presented.is_empty(), "a ready request draws nothing");
    assert_eq!(
        lock(&preview.primed).as_ref().map(|p| p.motion.feed().waiting()),
        Some(1),
        "the cursor of the frame it resumes at is open"
    );

    // The page's session from there adopts it.
    let taken = preview.play(&inputs, &key(3), resume + 0.001, false).expect("an answer");
    assert_eq!(taken.renderer, "gpu", "{:?}", taken.reasons);
    assert!(lock(&preview.primed).is_none(), "the session took what was made ready");
    let drawn = super::drive_tests::picture(&log);
    preview.end_play();
    // The same frame as a play begun there cold.
    let cold = preview.play(&inputs, &key(4), resume + 0.001, false).expect("an answer");
    assert_eq!(cold.renderer, "gpu", "{:?}", cold.reasons);
    assert!(
        super::drive_tests::picture(&log) == drawn,
        "the play taken back draws what a play begun there draws"
    );
    preview.end_play();

    // A title that slides for longer than the reach: not yet, and where the GPU stops is said again.
    project
        .update_overlay(title, None, None, Some(8.0), None, None, None, None, None, None, None)
        .expect("a longer title");
    project.set_overlay_keyframes(title, slide(7.5)).expect("keyframes");
    let inputs = PlanInputs::of(&project).expect("inputs");
    let not_yet = preview.ready_play(&inputs, &key(5), 1.0, false).expect("an answer");
    assert_eq!(not_yet.renderer, "stream");
    let info = not_yet.play.expect("not yet, with where the GPU stops");
    assert_eq!(info.resume, None);
    assert_eq!(info.ahead.map(|a| a.kind), Some("moving_text"));
    assert!(lock(&preview.primed).is_none());

    // With the setting off there is no play to take back.
    assert!(preview.set_enabled(false));
    let off = preview.ready_play(&inputs, &key(6), 8.2, false).expect("an answer");
    assert_eq!(off.renderer, "stream");
    assert!(off.play.is_none(), "{:?}", off.reasons);
}
