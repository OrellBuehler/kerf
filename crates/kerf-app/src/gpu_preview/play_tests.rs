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
