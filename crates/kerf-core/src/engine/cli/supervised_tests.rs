//! Children that cannot be waited on for ever: `output_supervised` (cancel, stall, wall-clock
//! bound), `reap`, `discard_partial` and the way a failed `silencedetect` run is read.

use super::*;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::Duration;

fn sh(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", script]);
    cmd
}

fn exit(code: i32) -> ExitStatus {
    ExitStatus::from_raw(code << 8)
}

fn collected(code: i32, stderr: &str) -> Collected {
    Collected {
        status: exit(code),
        stdout: Vec::new(),
        stderr: stderr.to_string(),
        stalled: false,
        cancelled: false,
        timed_out: false,
    }
}

// ---- output_supervised ---------------------------------------------------------

#[test]
fn a_cancel_kills_the_child_and_says_so() {
    let started = Instant::now();
    let out = output_supervised(sh("exec sleep 30"), Duration::MAX, None, &|| {
        started.elapsed() > Duration::from_millis(300)
    })
    .expect("spawn");
    assert!(out.cancelled && !out.succeeded());
    assert!(!out.stalled && !out.timed_out);
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn a_stderr_that_goes_quiet_is_killed_with_what_it_said() {
    let started = Instant::now();
    let out = output_supervised(
        sh("echo reading the file >&2; exec sleep 30"),
        Duration::from_millis(500),
        None,
        &|| false,
    )
    .expect("spawn");
    assert!(out.stalled && !out.succeeded());
    assert!(out.stderr.contains("reading the file"), "{:?}", out.stderr);
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn progress_lines_keep_a_slow_run_alive_and_stay_out_of_stderr() {
    // 1.2 s of work against a 0.6 s stall bound: only the report lines keep it alive.
    let out = output_supervised(
        sh("i=0; while [ $i -lt 12 ]; do echo out_time_us=$i >&2; sleep 0.1; i=$((i+1)); done; echo finished >&2"),
        Duration::from_millis(600),
        None,
        &|| false,
    )
    .expect("spawn");
    assert!(out.succeeded(), "{:?}", out.stderr);
    assert_eq!(out.stderr, "finished\n");
}

#[test]
fn a_wall_clock_bound_ends_a_run_that_keeps_talking() {
    let started = Instant::now();
    let out = output_supervised(
        sh("while true; do echo frame=1 >&2; sleep 0.05; done"),
        Duration::from_secs(60),
        Some(Duration::from_millis(600)),
        &|| false,
    )
    .expect("spawn");
    assert!(out.timed_out && !out.stalled && !out.succeeded());
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn a_run_that_ends_in_time_gives_status_stdout_and_stderr() {
    let out = output_supervised(
        sh("echo out; echo err >&2; exit 3"),
        Duration::MAX,
        Some(Duration::from_secs(30)),
        &|| false,
    )
    .expect("spawn");
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(out.stdout, b"out\n");
    assert_eq!(out.stderr, "err\n");
    assert!(!out.succeeded() && !out.cancelled && !out.stalled && !out.timed_out);
}

// ---- reap ----------------------------------------------------------------------

#[test]
fn a_child_that_closed_its_pipes_and_never_exits_is_killed_after_the_grace() {
    let mut child = sh("exec sleep 30").stdin(Stdio::null()).spawn().expect("spawn");
    let started = Instant::now();
    let status = reap_within(&mut child, Duration::from_millis(300)).expect("reaped");
    assert!(!status.success());
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn a_child_that_has_exited_is_reaped_at_once() {
    let mut child = sh("exit 0").stdin(Stdio::null()).spawn().expect("spawn");
    let started = Instant::now();
    assert!(reap_within(&mut child, Duration::from_secs(30)).expect("reaped").success());
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

// ---- discard_partial -----------------------------------------------------------

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-discard-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("out.mp4")
}

#[test]
fn a_file_this_run_created_is_discarded() {
    let path = scratch("created");
    let before = file_mtime(&path);
    assert_eq!(before, None);
    std::fs::write(&path, b"half a render").unwrap();
    discard_partial(&path, before);
    assert!(!path.exists());
}

#[test]
fn an_earlier_export_this_run_never_touched_is_kept() {
    let path = scratch("untouched");
    std::fs::write(&path, b"the finished export").unwrap();
    let before = file_mtime(&path);
    // A cancel while the export was still queued, or an option error: ffmpeg never opened it.
    discard_partial(&path, before);
    assert_eq!(std::fs::read(&path).unwrap(), b"the finished export");
}

#[test]
fn an_earlier_export_this_run_overwrote_is_discarded() {
    let path = scratch("overwritten");
    std::fs::write(&path, b"the finished export").unwrap();
    let before = file_mtime(&path);
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_modified(before.unwrap() + Duration::from_secs(5)).unwrap();
    discard_partial(&path, before);
    assert!(!path.exists());
}

#[test]
fn a_render_that_fails_or_is_cancelled_removes_its_partial_and_one_that_completes_keeps_it() {
    for (outcome, kept) in [
        (Ok(RenderStatus::Completed), true),
        (Ok(RenderStatus::Cancelled), false),
        (Err(Error::Engine("ffmpeg exited with 1".into())), false),
    ] {
        let path = scratch("wrapper");
        let _ = std::fs::remove_file(&path);
        let describe = format!("{outcome:?}");
        let result = discarding_partial(&path, || {
            std::fs::write(&path, b"partial").unwrap();
            outcome
        });
        assert_eq!(path.exists(), kept, "{describe}");
        assert_eq!(result.is_err(), describe.starts_with("Err"), "the outcome is passed through");
    }
}

#[test]
fn a_render_that_never_opened_its_output_leaves_the_earlier_file_alone() {
    let path = scratch("never-opened");
    std::fs::write(&path, b"the finished export").unwrap();
    let result = discarding_partial(&path, || {
        Err(Error::InvalidArgument("timeline has no clips to export".into()))
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"the finished export");
}

// ---- silencedetect's exit status -------------------------------------------------

const SILENCE_LOG: &str =
    "[silencedetect @ 0x1] silence_start: 1.5\n[silencedetect @ 0x1] silence_end: 2.5 | silence_duration: 1\n";

#[test]
fn a_clean_run_gives_the_spans_it_printed() {
    let spans = silence_from_run(&collected(0, SILENCE_LOG), Path::new("a.wav")).expect("spans");
    assert_eq!(spans.len(), 1);
    assert_eq!((spans[0].start, spans[0].end), (1.5, 2.5));
}

#[test]
fn a_decode_that_failed_is_an_error_not_no_silence() {
    let run = collected(1, "a.wav: Invalid data found when processing input\n");
    let err = silence_from_run(&run, Path::new("a.wav")).expect_err("a failed decode");
    let text = err.to_string();
    assert!(
        text.contains("silence detection failed") && text.contains("Invalid data found"),
        "{text}"
    );
    // Whatever it had printed before dying is not an answer either.
    let partial = collected(1, &format!("{SILENCE_LOG}a.wav: Invalid data found\n"));
    assert!(silence_from_run(&partial, Path::new("a.wav")).is_err());
}

#[test]
fn a_file_with_no_audio_stream_has_no_silence() {
    let run = collected(1, "Output file #0 does not contain any stream\n");
    assert!(silence_from_run(&run, Path::new("v.mp4")).expect("no audio").is_empty());
}

#[test]
fn a_stalled_or_cancelled_run_is_reported_as_such() {
    let mut run = collected(1, "reading\n");
    run.stalled = true;
    let err = silence_from_run(&run, Path::new("a.wav")).expect_err("stalled").to_string();
    assert!(err.contains("stalled") && err.contains("reading"), "{err}");
    let mut run = collected(1, "");
    run.cancelled = true;
    assert!(matches!(silence_from_run(&run, Path::new("a.wav")), Err(Error::Cancelled)));
}

// ---- the stitch claim ----------------------------------------------------------------

#[test]
fn a_second_stitch_of_a_pair_waits_for_the_first_and_can_be_stopped_while_it_does() {
    let dst = std::env::temp_dir().join(format!("kerf-stitch-claim-{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&dst);
    let first = claim_stitch(&dst, &|| false).expect("claim").expect("nothing there yet");

    let started = Instant::now();
    let waiting = claim_stitch(&dst, &|| started.elapsed() > Duration::from_millis(300));
    assert!(matches!(waiting, Err(Error::Cancelled)), "{:?}", waiting.map(|c| c.is_some()));
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());

    // The first stitch finishing hands the next one the claim…
    drop(first);
    let second = claim_stitch(&dst, &|| false).expect("claim").expect("free again");

    // …and a stitch that waited while the file landed uses it rather than encoding again.
    let waiter = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| claim_stitch(&dst, &|| false));
        std::thread::sleep(Duration::from_millis(250));
        std::fs::write(&dst, b"stitched").unwrap();
        drop(second);
        waiter.join().unwrap()
    });
    assert!(
        waiter.expect("no error").is_none(),
        "the file was there, so there is nothing to make"
    );
    let _ = std::fs::remove_file(&dst);
}

// ---- smart crop: progress and stop ----------------------------------------------------

fn crop_job(dir: &Path, name: &str) -> crate::project::SmartCropJob {
    crate::project::SmartCropJob {
        clip_id: uuid::Uuid::new_v4(),
        path: dir.join(name),
        start: 0.0,
        end: 1.0,
        width: 1920,
        height: 1080,
    }
}

#[test]
fn smart_crop_reports_each_clip_and_a_stop_is_not_a_skipped_clip() {
    use crate::project::{FramingPlan, Project, SmartCropPlan};

    let _gate = cpu::test_lock();
    let dir = std::env::temp_dir().join("kerf-no-such-media");
    let plan = SmartCropPlan {
        target_aspect: 9.0 / 16.0,
        jobs: vec![crop_job(&dir, "a.mp4"), crop_job(&dir, "b.mp4")],
    };
    // Nothing is readable there: every clip is skipped, so the pass fails with the first error,
    // having reported each one it got through.
    let mut seen = Vec::new();
    assert!(Project::sample_smart_crops_with(&plan, &mut |f| seen.push(f), &|| false).is_err());
    assert_eq!(seen, [0.0, 0.5, 1.0]);

    // A stop ends the pass with Cancelled — not with the first clip's "could not read" — and
    // before the second clip is started.
    let mut seen = Vec::new();
    let stopped = Project::sample_smart_crops_with(&plan, &mut |f| seen.push(f), &|| true);
    assert!(matches!(stopped, Err(Error::Cancelled)), "{stopped:?}");
    assert_eq!(seen, [0.0]);

    let framing = FramingPlan {
        ratios: vec![(9, 16)],
        jobs: plan.jobs,
    };
    let mut seen = Vec::new();
    assert!(Project::sample_framings_with(&framing, &mut |f| seen.push(f), &|| false).is_err());
    assert_eq!(seen, [0.0, 0.5, 1.0]);
    let mut seen = Vec::new();
    let stopped = Project::sample_framings_with(&framing, &mut |f| seen.push(f), &|| true);
    assert!(matches!(stopped, Err(Error::Cancelled)), "{stopped:?}");
    assert_eq!(seen, [0.0]);
}

// ---- the real binary (`-- --ignored`) ---------------------------------------------

mod real {
    use super::*;
    use crate::engine::test_support::StatusBounded;

    fn make(dir: &Path, name: &str, args: &[&str]) -> PathBuf {
        let path = dir.join(name);
        let status = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(status.success(), "could not make {name}");
        path
    }

    fn dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-supervised-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `cargo test -p kerf-core --no-default-features -- --ignored silence_detection`
    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn silence_detection_finds_the_gap_and_fails_on_a_file_that_does_not_decode() {
        let dir = dir("silence");
        // 1 s of tone, 1 s of silence, 1 s of tone.
        let wav = make(
            &dir,
            "gap.wav",
            &[
                "-f",
                "lavfi",
                "-i",
                "aevalsrc='if(between(t,1,2),0,0.5*sin(2*PI*440*t))':s=16000:d=3",
            ],
        );
        let spans = detect_silence_cancellable(&wav, -30.0, 0.5, &|| false).expect("silence");
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert!(
            (spans[0].start - 1.0).abs() < 0.1 && (spans[0].end - 2.0).abs() < 0.1,
            "{spans:?}"
        );

        // Not media at all: used to read as "no silence".
        let junk = dir.join("junk.wav");
        std::fs::write(&junk, vec![0x42u8; 4096]).unwrap();
        let err = detect_silence_cancellable(&junk, -30.0, 0.5, &|| false).expect_err("a failed decode");
        assert!(err.to_string().contains("silence detection failed"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_video_with_no_audio_has_no_silence_and_no_loudness_to_fail_on() {
        let dir = dir("silent-video");
        let mp4 = make(
            &dir,
            "v.mp4",
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=10:duration=1",
                "-pix_fmt",
                "yuv420p",
            ],
        );
        assert!(detect_silence_cancellable(&mp4, -30.0, 0.5, &|| false)
            .expect("no audio")
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn scene_detection_fails_on_a_file_that_does_not_decode_and_finds_nothing_in_audio_only() {
        let dir = dir("scenes-status");
        let junk = dir.join("junk.mp4");
        std::fs::write(&junk, vec![0x42u8; 4096]).unwrap();
        let err = detect_scenes_cancellable(&junk, 0.4, &|| false).expect_err("a failed decode");
        assert!(err.to_string().contains("scene detection failed"), "{err}");
        // The proxy that cannot be read falls back to the original, which is the one that fails.
        assert!(detect_scenes_preferring_cancellable(&junk, Some(&junk), 0.4, &|| false).is_err());
        let wav = make(&dir, "a.wav", &["-f", "lavfi", "-i", "sine=duration=1"]);
        assert!(detect_scenes_cancellable(&wav, 0.4, &|| false)
            .expect("audio only")
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_cancel_stops_each_whole_file_read_before_it_finishes() {
        let dir = dir("cancel");
        // Long enough that a decode is still running when the cancel lands (and short to make).
        let wav = make(
            &dir,
            "long.wav",
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=8000:duration=3600",
                "-c:a",
                "pcm_s16le",
            ],
        );
        let video = make(
            &dir,
            "long.mp4",
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=30:duration=900",
                "-pix_fmt",
                "yuv420p",
                "-preset",
                "ultrafast",
            ],
        );
        let t = |limit: u64| {
            let started = Instant::now();
            move || started.elapsed() > Duration::from_millis(limit)
        };
        let started = Instant::now();
        assert!(matches!(
            detect_silence_cancellable(&wav, -30.0, 0.5, &t(400)),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            crate::engine::audio::measure_loudness_cancellable(&wav, &t(400)),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            crate::engine::audio::analyze_rhythm_cancellable(&wav, 1.5, &t(400)),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            detect_scenes_cancellable(&video, 0.4, &t(400)),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            detect_scenes_preferring_cancellable(&video, Some(&video), 0.4, &t(400)),
            Err(Error::Cancelled)
        ));
        assert!(
            matches!(salience_map_cancellable(&video, 0.0, 900.0, &t(150)), Err(Error::Cancelled)),
            "salience"
        );
        assert!(started.elapsed() < Duration::from_secs(60), "{:?}", started.elapsed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn probe_is_bounded_and_reads_a_real_file() {
        let dir = dir("probe");
        let wav = make(&dir, "a.wav", &["-f", "lavfi", "-i", "sine=duration=1"]);
        let probed = probe(&wav).expect("probe");
        assert!(probed.duration > 0.9);
        assert!(probe(&dir.join("missing.wav")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn waveform_and_the_pcm_decodes_still_read_audio_through_the_pump() {
        let dir = dir("pcm");
        let wav = make(
            &dir,
            "a.wav",
            &["-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-c:a", "pcm_s16le"],
        );
        let peaks = waveform(&wav, 20, 8_000).expect("waveform");
        assert_eq!(peaks.len(), 20);
        assert!(peaks.iter().all(|p| *p > 0.1), "{peaks:?}");
        let pcm = decode_audio_mono_f32(&wav, 8_000, &|| false).expect("decode");
        assert!((pcm.len() as i64 - 16_000).abs() < 400, "{}", pcm.len());
        assert!(matches!(decode_audio_mono_f32(&wav, 8_000, &|| true), Err(Error::Cancelled)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reads an agent asks for — a waveform, a pyramid window, an energy curve, a contact sheet,
    /// a composited still — stop when the request that asked is dropped, and what they stopped is
    /// not kept: the same call afterwards decodes and answers.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored agent_reads_stop`
    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn agent_reads_stop_when_their_request_is_dropped() {
        use crate::engine::{energy_envelope_cancellable, waveform_range_of_cancellable};
        let dir = dir("agent-reads");
        let wav = make(
            &dir,
            "a.wav",
            &["-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-c:a", "pcm_s16le"],
        );
        assert!(matches!(
            waveform_cancellable(&wav, 20, 8_000, &|| true),
            Err(Error::Cancelled)
        ));
        assert_eq!(waveform_cancellable(&wav, 20, 8_000, &|| false).unwrap().len(), 20);

        assert!(matches!(
            waveform_range_of_cancellable(&wav, 0.0, 1.0, 10, &|| true),
            Err(Error::Cancelled)
        ));
        let window = waveform_range_of_cancellable(&wav, 0.0, 1.0, 10, &|| false).expect("a stopped decode cached nothing");
        assert_eq!(window.buckets, 10);

        assert!(matches!(
            energy_envelope_cancellable(&wav, 10, 8_000, &|| true),
            Err(Error::Cancelled)
        ));
        assert_eq!(energy_envelope_cancellable(&wav, 10, 8_000, &|| false).unwrap().len(), 10);

        let (timeline, assets) = long_cut(&dir, 2);
        let video = PathBuf::from(&assets[0].path);
        assert!(matches!(
            contact_sheet_cancellable(&video, 0.0, 2.0, 2, 2, 80, 5, &|| true),
            Err(Error::Cancelled)
        ));
        let (sheet, times) = contact_sheet_cancellable(&video, 0.0, 2.0, 2, 2, 80, 5, &|| false).expect("sheet");
        assert!(!sheet.is_empty() && times.len() == 4);

        let opts = ExportOptions::default();
        assert!(matches!(
            timeline_frame_cancellable(&timeline, &assets, &opts, 0.5, 320, 4, &|| true),
            Err(Error::Cancelled)
        ));
        assert!(!timeline_frame_cancellable(&timeline, &assets, &opts, 0.5, 320, 4, &|| false)
            .expect("still")
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- exports: what a cancel or a failure leaves behind --------------------------------

    use crate::engine::test_support::{av_asset, make_clip, timeline_of, video_stream, video_track};
    use uuid::Uuid;

    /// A long enough 640x360 source that an export is still running when the cancel lands.
    fn long_cut(dir: &Path, seconds: u32) -> (Timeline, Vec<Asset>) {
        let media = make(
            dir,
            "src.mp4",
            &[
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size=640x360:rate=30:duration={seconds}"),
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
            ],
        );
        let mut asset = av_asset(Uuid::new_v4(), f64::from(seconds));
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(640, 360, 30.0)];
        let timeline = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, f64::from(seconds), 0.0)])]);
        (timeline, vec![asset])
    }

    /// `cargo test -p kerf-core --no-default-features -- --ignored cancelled_export`
    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_cancelled_export_removes_what_it_wrote_and_spares_a_file_it_never_touched() {
        let _gate = crate::engine::cpu::test_lock();
        let dir = dir("export-cancel");
        let (timeline, assets) = long_cut(&dir, 120);
        let opts = ExportOptions::default();

        // Cancelled once the encode has begun: ffmpeg opened the output, so the partial goes.
        let out = dir.join("fresh.mp4");
        let seen = std::cell::Cell::new(false);
        let status = render_with_progress(
            &timeline,
            &assets,
            &out,
            &opts,
            &mut |p| {
                if p.fraction > 0.0 && p.waiting.is_none() {
                    seen.set(true);
                }
            },
            &|| seen.get(),
        )
        .expect("cancel is a status");
        assert_eq!(status, RenderStatus::Cancelled);
        assert!(!out.exists(), "the half-written file was left behind");

        // Cancelled while still queued behind another job: ffmpeg never ran, and the export
        // from last week still sitting at that path is not this run's to delete.
        let earlier = dir.join("earlier.mp4");
        std::fs::write(&earlier, b"last week's export").unwrap();
        let release = std::sync::atomic::AtomicBool::new(false);
        let holding = std::sync::atomic::AtomicBool::new(false);
        let started = Instant::now();
        let status = std::thread::scope(|scope| {
            scope.spawn(|| {
                let _slot = cpu::lease_waiting(&mut |_| {}, &|| false).expect("the slot");
                holding.store(true, std::sync::atomic::Ordering::SeqCst);
                while !release.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            while !holding.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let status = render_with_progress(&timeline, &assets, &earlier, &opts, &mut |_| {}, &|| {
                started.elapsed() > Duration::from_millis(600)
            });
            release.store(true, std::sync::atomic::Ordering::SeqCst);
            status
        })
        .expect("cancel is a status");
        assert_eq!(status, RenderStatus::Cancelled);
        assert_eq!(std::fs::read(&earlier).unwrap(), b"last week's export");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failure before ffmpeg opened the output (a source it cannot read) is not a reason to
    /// delete the export from last week that still sits at that path.
    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_failed_export_that_never_opened_its_output_spares_the_earlier_file() {
        let _gate = crate::engine::cpu::test_lock();
        let dir = dir("export-fail");
        let (timeline, mut assets) = long_cut(&dir, 4);
        let junk = dir.join("junk.mp4");
        std::fs::write(&junk, vec![0u8; 2048]).unwrap();
        assets[0].path = junk.to_string_lossy().into_owned();
        let out = dir.join("earlier.mp4");
        std::fs::write(&out, b"last week's export").unwrap();
        let err = render_with_progress(&timeline, &assets, &out, &ExportOptions::default(), &mut |_| {}, &|| false)
            .expect_err("a source that cannot be read");
        assert!(!err.to_string().is_empty());
        assert_eq!(std::fs::read(&out).unwrap(), b"last week's export");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_cancelled_batch_keeps_the_finished_variants_and_removes_the_one_in_flight() {
        let _gate = crate::engine::cpu::test_lock();
        let dir = dir("variants-cancel");
        let (timeline, assets) = long_cut(&dir, 90);
        let base = dir.join("cut.mp4");
        let variants = vec![
            ExportVariant::beside(&base, Delivery::new(90, 160, Fit::Cover)),
            ExportVariant::beside(&base, Delivery::new(200, 200, Fit::Cover)),
        ];
        // The first variant is short (a 3 s window of the cut); cancel once the second is under way.
        let opts = ExportOptions {
            range: Some(crate::model::TimeRange { start: 0.0, end: 3.0 }),
            ..ExportOptions::default()
        };
        let second = std::cell::Cell::new(false);
        let VariantsRun {
            status, completed: done, ..
        } = render_variants(
            &timeline,
            &assets,
            &variants,
            &opts,
            false,
            &mut |p| {
                if p.variant == 1 && p.fraction > 0.5 {
                    second.set(true);
                }
            },
            &|| second.get(),
        )
        .expect("cancel is a status");
        assert_eq!(status, RenderStatus::Cancelled);
        assert_eq!(done, 1, "one variant was complete");
        assert!(variants[0].output.is_file(), "the finished variant stays");
        assert!(!variants[1].output.exists(), "the one in flight is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A batch that fails before any variant's output was opened (options that cannot encode)
    /// leaves the files already at those paths alone.
    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_failed_batch_spares_the_files_already_at_its_paths() {
        let _gate = crate::engine::cpu::test_lock();
        let dir = dir("variants-fail");
        let (timeline, assets) = long_cut(&dir, 4);
        let base = dir.join("cut.mp4");
        let variants = vec![
            ExportVariant::beside(&base, Delivery::new(90, 160, Fit::Cover)),
            ExportVariant::beside(&base, Delivery::new(200, 200, Fit::Cover)),
        ];
        for v in &variants {
            std::fs::write(&v.output, b"earlier").unwrap();
        }
        let opts = ExportOptions {
            video_bitrate: Some("not a bitrate".into()),
            ..ExportOptions::default()
        };
        for together in [false, true] {
            render_variants(&timeline, &assets, &variants, &opts, together, &mut |_| {}, &|| false).expect_err("invalid options");
        }
        for v in &variants {
            assert_eq!(std::fs::read(&v.output).unwrap(), b"earlier", "{}", v.output.display());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_cancelled_cover_is_an_error_and_leaves_no_file() {
        let dir = dir("cover");
        let (timeline, assets) = long_cut(&dir, 3);
        let path = dir.join("cover.jpg");
        let err = export_still_cancellable(
            &timeline,
            &assets,
            &ExportOptions::default(),
            1.0,
            &path,
            ImageFormat::Jpeg,
            2,
            &|| true,
        )
        .expect_err("cancelled");
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert!(!path.exists());
        // And uncancelled it is the same call that writes the file.
        export_still_cancellable(
            &timeline,
            &assets,
            &ExportOptions::default(),
            1.0,
            &path,
            ImageFormat::Jpeg,
            2,
            &|| false,
        )
        .expect("cover");
        assert!(path.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stitch that is stopped leaves neither the cached file nor its temp behind.
    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_cancelled_stitch_leaves_nothing_in_the_cache() {
        let dir = dir("stitch");
        let lens = |name: &str| {
            make(
                &dir,
                name,
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=720x720:rate=30:duration=6",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                ],
            )
        };
        let front = lens("VID_20250101_000000_00_001.mp4");
        let rear = lens("VID_20250101_000000_10_001.mp4");
        let dst = stitched_path(&front, &rear).expect("cache dir");
        let _ = std::fs::remove_file(&dst);
        let started = Instant::now();
        let mut ticks = 0;
        let err = stitch_insta360_with(&front, &rear, 6.0, &mut |_| ticks += 1, &|| {
            started.elapsed() > Duration::from_millis(1500)
        })
        .expect_err("cancelled");
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(60), "{:?}", started.elapsed());
        assert!(!dst.exists(), "a cancelled stitch must not be cached");
        let leftovers: Vec<_> = std::fs::read_dir(dst.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with(&dst.file_stem().unwrap().to_string_lossy().into_owned()) && name.ends_with(".part")
            })
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
