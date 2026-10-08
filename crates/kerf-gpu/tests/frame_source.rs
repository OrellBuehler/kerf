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
    /// The same picture with a variable frame rate, in matroska (1/1000).
    vfr: Asset,
    /// The CFR clip as a transport stream (1/90000, a container start of 1.4 s).
    ts: Asset,
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
                "-fps_mode",
                "passthrough",
            ],
        );
        let ts = make("clip.ts", &[]);
        let probe = |p: &Path| Project::probe_asset(p).unwrap_or_else(|e| panic!("probe {}: {e}", p.display()));
        Media {
            cfr: probe(&cfr),
            vfr: probe(&vfr),
            ts: probe(&ts),
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
        eprintln!("{name}: {:?}", src.stats());
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
