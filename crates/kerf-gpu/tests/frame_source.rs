//! `FrameSource` against the real `ffmpeg`: frames from long-lived runs are the frames A0's
//! one-shot decode returns, byte for byte, and the runs are reused the way the router says.
//!
//! Needs `ffmpeg` on `PATH` (or `KERF_FFMPEG`); no GPU. `#[ignore]`d; run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test frame_source -- --ignored
//! ```

#![allow(clippy::print_stderr)] // the runs' statistics, for whoever runs the suite

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use kerf_core::{
    Asset, Clip, CompositeColorPolicy, ExportOptions, Pick, PlanLayer, Project, RenderPlan, StreamKind, Timeline, Track,
};
use kerf_gpu::frame_source::self_test;
use kerf_gpu::{decode_layer, FrameSource, FrameSourceConfig, Hint};

fn ffmpeg(args: &[&str]) {
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Media {
    /// 29.97 fps H.264 in mp4 (1/30000), a GOP of 30: seeks land inside GOPs.
    cfr: Asset,
    /// The same picture with a variable frame rate, in matroska (1/1000): every third gap is three
    /// frames long, so a seek into one reads as a landing a whole frame late.
    vfr: Asset,
    /// The CFR picture with timestamps that wander by 100 ticks of 1/90000 (mp4): variable, but
    /// no seek lands a frame late.
    jitter: Asset,
    /// The CFR clip as a transport stream (1/90000, a container start of 1.4 s).
    ts: Asset,
    /// 25 fps H.264 with B-frames and **open GOPs** (a keyframe every 50 frames whose leading
    /// B-frames cannot be decoded from it): `-ss` into a keyframe's leading frames lands later.
    open_gop: Asset,
    /// 25 fps HEVC (x265 defaults: B-frames, open GOPs) in mp4.
    hevc: Asset,
    /// 25 fps H.264, one keyframe every 100 frames, as a transport stream: `-ss` lands on the
    /// next keyframe.
    long_gop_ts: Asset,
}

fn media() -> &'static Media {
    static M: OnceLock<Media> = OnceLock::new();
    M.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("frame-source-media");
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, extra: &[&str]| -> PathBuf {
            let p = dir.join(name);
            let mut args = vec![
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=30000/1001:duration=3",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "30",
                "-pix_fmt",
                "yuv420p",
            ];
            args.extend_from_slice(extra);
            args.push(p.to_str().unwrap());
            ffmpeg(&args);
            p
        };
        let cfr = make("cfr.mp4", &["-video_track_timescale", "30000"]);
        // Frames bunched and spread: every third gap is three times as long.
        let vfr = make(
            "vfr.mkv",
            &[
                "-vf",
                "settb=1/1000,setpts='(N+2*floor(N/3))*33/TB/1000'",
                kerf_core::fps_mode_flag(),
                "passthrough",
            ],
        );
        let ts = make("clip.ts", &[]);
        let jitter = make(
            "jitter.mp4",
            &[
                "-vf",
                "settb=1/90000,setpts='N*3003+100*mod(N,3)'",
                kerf_core::fps_mode_flag(),
                "passthrough",
                "-video_track_timescale",
                "90000",
            ],
        );
        // Fixtures the one-shot decode treats differently from "the frame at that time": a seek
        // into leading frames or short of a keyframe lands later.
        let make_25 = |name: &str, codec: &[&str]| -> PathBuf {
            let p = dir.join(name);
            let mut args = vec!["-f", "lavfi", "-i", "testsrc2=size=320x180:rate=25:duration=8"];
            args.extend_from_slice(codec);
            args.extend(["-pix_fmt", "yuv420p"]);
            args.push(p.to_str().unwrap());
            ffmpeg(&args);
            p
        };
        let open_gop = make_25(
            "open-gop.mp4",
            &[
                "-c:v",
                "libx264",
                "-x264-params",
                "open-gop=1:keyint=50:min-keyint=50:bframes=3:scenecut=0",
            ],
        );
        let hevc = make_25(
            "hevc.mp4",
            &[
                "-c:v",
                "libx265",
                "-x265-params",
                "keyint=50:min-keyint=50:scenecut=0:log-level=none",
            ],
        );
        let long_gop_ts = make_25(
            "long-gop.ts",
            &[
                "-c:v",
                "libx264",
                "-x264-params",
                "keyint=100:min-keyint=100:bframes=0:scenecut=0",
                "-output_ts_offset",
                "1.4",
            ],
        );
        let probe = |p: &Path| Project::probe_asset(p).unwrap_or_else(|e| panic!("probe {}: {e}", p.display()));
        Media {
            cfr: probe(&cfr),
            vfr: probe(&vfr),
            jitter: probe(&jitter),
            ts: probe(&ts),
            open_gop: probe(&open_gop),
            hevc: probe(&hevc),
            long_gop_ts: probe(&long_gop_ts),
        }
    })
}

fn duration(a: &Asset) -> f64 {
    a.duration
}

/// The layer a one-clip timeline of `a` shows at source time `t`.
fn layer(a: &Asset, t: f64) -> PlanLayer {
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![Clip::new(a.id, 0.0, duration(a), 0.0)],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    };
    let plan = RenderPlan::at(
        &tl,
        std::slice::from_ref(a),
        &ExportOptions::default(),
        t,
        CompositeColorPolicy::FixedBt601,
    )
    .expect("plan");
    plan.layers.into_iter().next().expect("a layer")
}

/// A layer asking for exactly source time `t` (past the clip's end too).
fn at(a: &Asset, t: f64) -> PlanLayer {
    let mut l = layer(a, 0.0);
    l.source_time = t;
    l.pick = Pick::AtOrAfter(t);
    l
}

/// [`at`], read as a proxy would be: the router reads a run forward only 24 frames for it.
fn at_proxy(a: &Asset, t: f64) -> PlanLayer {
    let mut l = at(a, t);
    l.source.proxy = true;
    l
}

fn source() -> std::sync::Arc<FrameSource> {
    FrameSource::new(FrameSourceConfig::default())
}

/// A seeded xorshift, so a failure names a reproducible time.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_self_test_passes_on_this_ffmpeg() {
    self_test().expect("the run self-test");
}

/// Random, forward and backward times over three files, each answered as the one-shot decode
/// answers. What each leg tests (the stats it prints say which path answered): `cfr` is answered
/// from runs; `vfr` falls back to one-shot at its first seek into a gap of a frame or more (79 of
/// its 80 answers are one-shot, equal and no faster: the fallback is what it tests); `ts` is a
/// container runs are not trusted for, so every answer is one-shot (and its container start of
/// 1.4 s is read by the one-shot as it always was). `files_that_runs_are_trusted_for_answer_from_runs_in_every_order`
/// is the leg for runs answering a variable-rate file.
#[test]
#[ignore = "needs ffmpeg"]
fn frames_from_runs_are_the_one_shot_decodes_byte_for_byte() {
    let m = media();
    for (name, a) in [("cfr", &m.cfr), ("vfr", &m.vfr), ("ts", &m.ts)] {
        let src = source();
        assert!(src.runs_enabled(), "the run path is off on this ffmpeg");
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let dur = duration(a);
        // Random times, then a forward walk and a backward one, which reuse and restart runs.
        let mut times: Vec<f64> = (0..50).map(|_| (rng.next() * dur * 1e6).round() / 1e6).collect();
        times.extend((0..20).map(|k| 1.0 + f64::from(k) / 29.97));
        times.extend((0..10).map(|k| 2.0 - f64::from(k) * 0.05));
        for t in times {
            let l = at(a, t);
            let reference = decode_layer(&l).unwrap_or_else(|e| panic!("{name} @ {t}: one-shot: {e}"));
            let got = src
                .frame(&l, Hint::Scrub)
                .unwrap_or_else(|e| panic!("{name} @ {t}: frame source: {e}"));
            assert_eq!(
                got.as_deref(),
                reference.as_ref(),
                "{name} @ {t}: the frame source and the one-shot decode differ"
            );
        }
        let stats = src.stats();
        eprintln!("{name}: {stats:?}");
        match name {
            "cfr" => assert!(
                stats.spawned > 0 && stats.cache.hits > 0 && stats.distrusted == 0,
                "{stats:?}"
            ),
            "ts" => assert!(stats.spawned == 0 && stats.oneshots >= 70, "{stats:?}"),
            _ => assert!(stats.oneshots > 0, "{stats:?}"),
        }
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn playback_reads_one_run_forward() {
    let a = &media().cfr;
    let src = source();
    for k in 0..60 {
        let t = 0.5 + f64::from(k) * 1001.0 / 30000.0;
        let got = src.frame(&at(a, t), Hint::Forward { fps: 29.97 }).expect("a frame");
        assert!(got.is_some(), "no frame at {t}");
    }
    let stats = src.stats();
    assert_eq!(stats.spawned, 1, "{stats:?}");
    assert_eq!(stats.oneshots, 0, "{stats:?}");
}

#[test]
#[ignore = "needs ffmpeg"]
fn two_clips_of_one_file_play_from_two_runs() {
    let a = &media().cfr;
    let src = source();
    for k in 0..20 {
        let step = f64::from(k) * 1001.0 / 30000.0;
        // 54 frames apart: further than a proxy's run is read forward.
        for base in [0.2, 2.0] {
            src.frame(&at_proxy(a, base + step), Hint::Forward { fps: 29.97 })
                .expect("a frame");
        }
    }
    let stats = src.stats();
    assert_eq!(stats.spawned, 2, "{stats:?}");
    assert_eq!(stats.replaced, 0, "{stats:?}");
}

#[test]
#[ignore = "needs ffmpeg"]
fn an_exact_frame_takes_no_run_from_anyone() {
    let a = &media().cfr;
    let src = source();
    src.frame(&at_proxy(a, 0.3), Hint::Scrub).expect("a frame");
    let before = src.stats();
    assert_eq!((before.spawned, before.runs), (1, 1), "{before:?}");
    // Far from the run: no run can read forward to it, and an exact frame starts none.
    let exact = src.frame(&at_proxy(a, 2.5), Hint::Exact).expect("a frame");
    assert_eq!(exact, decode_layer(&at(a, 2.5)).unwrap().map(std::sync::Arc::new));
    let after = src.stats();
    assert_eq!(after.spawned, 1, "{after:?}");
    assert_eq!(after.oneshots, before.oneshots + 1, "{after:?}");
    assert_eq!(after.runs, 1, "{after:?}");
}

#[test]
#[ignore = "needs ffmpeg"]
fn past_the_last_frame_there_is_no_frame_and_it_is_remembered() {
    let a = &media().cfr;
    let src = source();
    let past = duration(a) + 0.5;
    assert_eq!(decode_layer(&at(a, past)).unwrap(), None);
    assert_eq!(src.frame(&at(a, past), Hint::Scrub).unwrap(), None);
    // Read to the end, then a time past it is answered without starting a run.
    for k in 0..12 {
        let t = duration(a) - 0.4 + f64::from(k) * 1001.0 / 30000.0;
        src.frame(&at(a, t), Hint::Forward { fps: 29.97 }).expect("a frame");
    }
    let spawned = src.stats().spawned;
    assert_eq!(src.frame(&at(a, past + 1.0), Hint::Scrub).unwrap(), None);
    assert_eq!(src.stats().spawned, spawned);
}

#[test]
#[ignore = "needs ffmpeg"]
fn layers_of_the_same_frame_share_one_decode() {
    let a = &media().cfr;
    let src = source();
    let l = at(a, 1.0);
    let frames = src.frames(&[l.clone(), l, at(a, 1.1)], Hint::Scrub).expect("frames");
    assert_eq!(frames.len(), 3);
    let (x, y) = (frames[0].as_ref().unwrap(), frames[1].as_ref().unwrap());
    assert!(std::sync::Arc::ptr_eq(x, y), "two layers of one frame decoded twice");
    assert_eq!(frames[2].as_deref(), decode_layer(&at(a, 1.1)).unwrap().as_ref());
}

#[test]
#[ignore = "needs ffmpeg"]
fn concurrent_requests_for_one_file_get_the_frames_they_asked_for() {
    let m = media();
    let src = source();
    // Six threads, two files, overlapping and far-apart times, a mix of hints: whatever runs
    // they share, start or replace, each frame must be the one asked for.
    std::thread::scope(|scope| {
        for worker in 0..6u64 {
            let src = &src;
            scope.spawn(move || {
                let a = if worker % 2 == 0 { &m.cfr } else { &m.vfr };
                let mut rng = Rng(0x2545_f491_4f6c_dd1d ^ worker.wrapping_mul(0x9e37_79b9));
                for k in 0..25 {
                    let t = if k % 5 == 0 {
                        (rng.next() * duration(a) * 1e6).round() / 1e6
                    } else {
                        0.4 + f64::from(k) * 0.05 + worker as f64 * 0.01
                    };
                    let hint = match k % 3 {
                        0 => Hint::Scrub,
                        1 => Hint::Forward { fps: 29.97 },
                        _ => Hint::Exact,
                    };
                    let l = at_proxy(a, t);
                    let got = match src.frame(&l, hint) {
                        Ok(f) => f,
                        // Every run in use is an answer too: the caller renders it with FFmpeg.
                        Err(kerf_gpu::GpuError::Busy(_)) => continue,
                        Err(e) => panic!("worker {worker} @ {t}: {e}"),
                    };
                    let reference = decode_layer(&l).expect("one-shot");
                    assert_eq!(got.as_deref(), reference.as_ref(), "worker {worker} @ {t} ({hint:?})");
                }
            });
        }
    });
    eprintln!("{:?}", src.stats());
}

/// The frame source against the one-shot decode over `times`, in order, through **one** source
/// (so each answer depends on what the walk cached before it): the times at which they differ.
fn disagreements(
    a: &Asset,
    src: &FrameSource,
    reference: &mut std::collections::HashMap<u64, Option<kerf_gpu::YuvFrame>>,
    times: &[f64],
    hint: Hint,
) -> Vec<String> {
    let mut bad = Vec::new();
    for &t in times {
        let l = at(a, t);
        let want = reference
            .entry(t.to_bits())
            .or_insert_with(|| decode_layer(&l).unwrap_or_else(|e| panic!("@ {t}: one-shot: {e}")));
        let got = src.frame(&l, hint).unwrap_or_else(|e| panic!("@ {t}: frame source: {e}"));
        if got.as_deref() != want.as_ref() {
            bad.push(format!("{t:.4}"));
        }
    }
    bad
}

/// `from..to` in steps of a fiftieth of a second: every frame time of a 25 fps clip and the
/// instant between each.
fn steps(from: f64, to: f64) -> Vec<f64> {
    let (a, b) = ((from * 50.0).round() as i64, (to * 50.0).round() as i64);
    (a..b).map(|k| k as f64 / 50.0).collect()
}

/// Files the one-shot decode treats differently from "the frame at that time", and a source
/// that must still answer with the one-shot decode's frame for each, whatever it has read
/// before: with B-frames and open GOPs a seek into the frames a keyframe leads (2.00 s is a
/// keyframe, 1.96 s is the frame before it) returns the keyframe, where a run that read through
/// from an earlier keyframe has the frame itself (49, where the one-shot returns 50); a
/// transport stream lands on the next keyframe whatever is asked, and a run from 0 did not.
/// Forward, backward and random order, one source each, every answer equal to the one-shot's.
#[test]
#[ignore = "needs ffmpeg"]
fn files_whose_seeks_do_not_land_on_the_frame_at_that_time_are_decoded_as_the_one_shot_does() {
    let m = media();
    // (name, file, the window around its keyframes to walk, runs are expected to start)
    let cases = [
        ("open-gop", &m.open_gop, true),
        ("hevc", &m.hevc, true),
        ("long-gop-ts", &m.long_gop_ts, false),
    ];
    for (name, a, starts_runs) in cases {
        let mut forward = steps(1.5, 2.5);
        forward.extend(steps(5.5, 6.5));
        let mut backward = forward.clone();
        backward.reverse();
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let random: Vec<f64> = (0..30).map(|_| (rng.next() * duration(a) * 1e6).round() / 1e6).collect();
        let mut reference = std::collections::HashMap::new();
        for (order, times) in [("forward", &forward), ("backward", &backward), ("random", &random)] {
            let src = source();
            assert!(src.runs_enabled(), "the run path is off on this ffmpeg");
            let bad = disagreements(a, &src, &mut reference, times, Hint::Scrub);
            let stats = src.stats();
            eprintln!("{name} {order}: {} of {} differ; {stats:?}", bad.len(), times.len());
            assert!(bad.is_empty(), "{name} {order}: differs from the one-shot at {bad:?}");
            if starts_runs {
                // Not vacuous: a run was started and found out why it cannot be trusted.
                assert!(stats.spawned > 0 && stats.distrusted == 1, "{name} {order}: {stats:?}");
            } else {
                assert_eq!(
                    stats.spawned, 0,
                    "{name} {order}: a transport stream starts no run: {stats:?}"
                );
            }
        }
    }
}

/// The same walks over a file a run can be proved equal for: runs are what answers, and they are
/// still the one-shot's frames, in every order.
#[test]
#[ignore = "needs ffmpeg"]
fn files_that_runs_are_trusted_for_answer_from_runs_in_every_order() {
    let m = media();
    for (name, a) in [("cfr", &m.cfr), ("jitter", &m.jitter)] {
        let dur = duration(a);
        let forward: Vec<f64> = (0..60).map(|k| 0.7 + f64::from(k) / 29.97).collect();
        let mut backward = forward.clone();
        backward.reverse();
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let random: Vec<f64> = (0..30).map(|_| (rng.next() * dur * 1e6).round() / 1e6).collect();
        let mut reference = std::collections::HashMap::new();
        for (order, times) in [("forward", &forward), ("backward", &backward), ("random", &random)] {
            let src = source();
            let bad = disagreements(a, &src, &mut reference, times, Hint::Scrub);
            let stats = src.stats();
            eprintln!("{name} {order}: {} of {} differ; {stats:?}", bad.len(), times.len());
            assert!(bad.is_empty(), "{name} {order}: differs from the one-shot at {bad:?}");
            assert!(
                stats.spawned > 0 && stats.cache.hits > 0 && stats.distrusted == 0,
                "{name} {order}: runs answered nothing: {stats:?}"
            );
        }
    }
}

/// A run parked at the frame it was asked for has made no progress for as long as it sat there;
/// asking it for more starts its silence clock then. Before, the reaper killed it within a tick of
/// the request: the silence it was judged by was the time it spent parked.
#[test]
#[ignore = "needs ffmpeg"]
fn a_parked_run_that_is_asked_again_is_not_killed_for_the_time_it_was_parked() {
    let a = &media().cfr;
    let src = FrameSource::new(FrameSourceConfig {
        frame_timeout: std::time::Duration::from_millis(400),
        ..FrameSourceConfig::default()
    });
    assert!(src.runs_enabled());
    src.frame(&at(a, 0.5), Hint::Scrub).expect("the first frame");
    assert_eq!(src.stats().spawned, 1);
    // Parked past the silence the reaper allows a run that is wanted (several of its ticks).
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let l = at(a, 0.9);
    let got = src.frame(&l, Hint::Scrub).expect("the same run, read on");
    assert_eq!(got.as_deref(), decode_layer(&l).unwrap().as_ref());
    let stats = src.stats();
    assert_eq!((stats.spawned, stats.reused), (1, 1), "{stats:?}");
}
