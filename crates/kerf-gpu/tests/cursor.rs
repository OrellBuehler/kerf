//! `FrameCursor` on real clips: for every output frame of a clip, the frame a cursor returns is
//! the frame `Pick::select` names over the whole file — which kerf-core's `picked.rs` holds to
//! the export's rendered frames. The source numbers its own frames (16 bits in luma blocks,
//! lossless), so "the frame" is checked by its pixels, not by an index the cursor reports.
//!
//! Needs `ffmpeg` and `ffprobe` on `PATH` (or `KERF_FFMPEG` / `KERF_FFPROBE`); no GPU.
//! `#[ignore]`d; run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test cursor -- --ignored
//! ```

#![allow(clippy::print_stderr)] // the cases' summary, for whoever runs the suite

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use kerf_core::{FpsPick, Pick, Rational, SourceFrames};
use kerf_gpu::cursor::picks_through;
use kerf_gpu::{CursorConfig, FrameCursor, YuvFrame};

const W: u32 = 128;
const H: u32 = 16;

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

fn ffprobe() -> String {
    std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".into())
}

/// A file's frames as ffprobe states them: pts, time base, last frame's duration.
struct Probed {
    pts: Vec<i64>,
    time_base: Rational,
    last_duration: i64,
}

impl Probed {
    fn frames(&self) -> SourceFrames<'_> {
        SourceFrames {
            pts: &self.pts,
            time_base: self.time_base,
            start_us: 0,
            last_duration: self.last_duration,
        }
    }
}

fn probe(path: &Path) -> Probed {
    let out = Command::new(ffprobe())
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=time_base"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe");
    let tb = String::from_utf8_lossy(&out.stdout);
    let (num, den) = tb.trim().split_once('/').expect("a time base");
    let time_base = Rational::new(num.parse().unwrap(), den.parse().unwrap()).unwrap();
    let out = Command::new(ffprobe())
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=best_effort_timestamp,duration",
        ])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe");
    let mut pts = Vec::new();
    let mut last_duration = 0;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut f = line.split(',');
        pts.push(f.next().unwrap().trim().parse::<i64>().expect("a pts"));
        last_duration = f.next().and_then(|d| d.trim().parse().ok()).unwrap_or(0);
    }
    Probed {
        pts,
        time_base,
        last_duration,
    }
}

/// The frame number a numbered frame carries: 16 blocks of 8 px across, bit `i` bright.
fn number(f: &YuvFrame) -> u32 {
    let row = (H as usize / 2) * W as usize;
    (0..16).fold(0, |n, bit| {
        let x = bit * 8 + 4;
        if f.y[row + x] > 128 {
            n | (1 << bit)
        } else {
            n
        }
    })
}

struct Clip {
    path: PathBuf,
    probed: Probed,
}

/// 120 frames at 30 fps, each carrying its own number (see [`number`]).
fn numbered_source() -> String {
    format!("nullsrc=s={W}x{H}:r=30:d=4,format=yuv420p,geq=lum='if(mod(floor(N/pow(2,floor(X/8))),2),235,16)':cb=128:cr=128")
}

/// Lossless (FFV1 in matroska) numbered frames, 120 of them: `cfr` at 30 fps, `vfr` with every
/// third gap three frames long, and `repeated-pts` with frames sharing a timestamp.
fn clips() -> &'static [(&'static str, Clip)] {
    static C: OnceLock<Vec<(&'static str, Clip)>> = OnceLock::new();
    C.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cursor-media");
        std::fs::create_dir_all(&dir).unwrap();
        let numbered = numbered_source();
        let make = |name: &'static str, extra: &[&str]| -> (&'static str, Clip) {
            let path = dir.join(format!("{name}.mkv"));
            let mut args = vec!["-f", "lavfi", "-i", numbered.as_str()];
            args.extend_from_slice(extra);
            args.extend(["-c:v", "ffv1", path.to_str().unwrap()]);
            ffmpeg(&args);
            let probed = probe(&path);
            (name, Clip { path, probed })
        };
        vec![
            make("cfr", &[]),
            // Every third gap three frames long, on the 1/30 grid.
            make(
                "vfr",
                &[
                    "-vf",
                    "setpts='(N+2*floor(N/3))/(30*TB)'",
                    kerf_core::fps_mode_flag(),
                    "passthrough",
                ],
            ),
            // The same spacing in milliseconds, which the encoder's 1/30 time base rounds into
            // repeated timestamps (frames 31 and 32 both at 1667): a file the pick takes as it is.
            make(
                "repeated-pts",
                &[
                    "-vf",
                    "settb=1/1000,setpts='(N+2*floor(N/3))*33/TB/1000'",
                    kerf_core::fps_mode_flag(),
                    "passthrough",
                ],
            ),
        ]
    })
}

/// The numbered frames in an mp4 on a one-**microsecond** time base whose timestamps are the
/// instants truncated (`floor(N * 10^6 / 30)`: frame 2 at 66666, where 2/30 s is 66666.67 us), so
/// a seek spelled to the nearest microsecond (66667) lands a frame after the one FFmpeg
/// reads `-ss 0.06666666666666667` (truncated to 66666) on.
fn microsecond_clip() -> &'static Clip {
    static C: OnceLock<Clip> = OnceLock::new();
    C.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cursor-media");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("microseconds.mp4");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            numbered_source().as_str(),
            "-vf",
            "settb=1/1000000,setpts='floor(N*1000000/30)'",
            kerf_core::fps_mode_flag(),
            "passthrough",
            "-c:v",
            "libx264",
            "-qp",
            "0",
            "-g",
            "1",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-enc_time_base",
            "1/1000000",
            "-video_track_timescale",
            "1000000",
            path.to_str().unwrap(),
        ]);
        let probed = probe(&path);
        Clip { path, probed }
    })
}

fn config() -> CursorConfig {
    CursorConfig {
        hwaccel: None,
        ..CursorConfig::default()
    }
}

/// The output frames a clip of `window` at speed `speed` placed at `start` spans at `fps`,
/// with a frame of margin either side.
fn output_frames(window: (f64, f64), start: f64, speed: f64, fps: Rational) -> std::ops::Range<u64> {
    let rate = f64::from(fps.num) / f64::from(fps.den);
    let end = start + (window.1 - window.0) / speed;
    let first = ((start * rate).floor() as u64).saturating_sub(1);
    let last = (end * rate).ceil() as u64 + 2;
    first..last
}

#[test]
#[ignore = "needs ffmpeg and ffprobe"]
fn a_cursor_shows_the_frame_the_export_picks_at_every_output_frame() {
    let rates = [
        Rational::new(30, 1).unwrap(),
        Rational::new(24, 1).unwrap(),
        Rational::new(2997, 100).unwrap(),
        Rational::new(60, 1).unwrap(),
    ];
    let mut cases = 0;
    let mut frames_checked = 0;
    for (name, clip) in clips() {
        let all = clip.probed.frames();
        assert_eq!(all.pts.len(), 120, "{name}: the numbered source");
        // The premise of the clip: it does repeat a timestamp, and the others do not.
        let repeats = all.pts.windows(2).filter(|w| w[0] == w[1]).count();
        assert_eq!(repeats > 0, *name == "repeated-pts", "{name}: {repeats} repeated timestamps");
        for &speed in &[0.5, 1.0, 1.5, 2.0, 4.0] {
            for reverse in [false, true] {
                // A window off the grid, one from the start, one to the end of the file.
                for (n, &window) in [(0.37, 2.11), (0.0, 1.3), (2.9, 4.0)].iter().enumerate() {
                    let fps = rates[(cases + n) % rates.len()];
                    let start = [0.0, 0.2, 1.0 / 3.0][n];
                    let pick = FpsPick {
                        speed,
                        reverse,
                        window,
                        start,
                        frame: 0,
                        fps,
                        drop_first: false,
                        image: None,
                    };
                    let mut cursor = FrameCursor::open(clip.path.to_str().unwrap(), (W, H), &Pick::Fps(pick), config())
                        .unwrap_or_else(|e| panic!("{name}: open: {e}"));
                    let ks = output_frames(window, start, speed, fps);
                    picks_through(&mut cursor, pick, ks, |k, frame| {
                        let expected = Pick::Fps(FpsPick { frame: k, ..pick }).select(&all);
                        let shown = frame.as_deref().map(number);
                        assert_eq!(
                            shown,
                            expected.map(|i| i as u32),
                            "{name} speed {speed} reverse {reverse} window {window:?} start {start} fps {}/{} frame {k}",
                            fps.num,
                            fps.den
                        );
                        frames_checked += 1;
                    })
                    .unwrap_or_else(|e| panic!("{name} speed {speed} reverse {reverse} {window:?}: {e}"));
                    // A forward clip never holds more than a frame or two.
                    if !reverse {
                        assert!(cursor.frames_held() <= 2, "{name}: {} frames held", cursor.frames_held());
                    }
                    cases += 1;
                }
            }
        }
    }
    eprintln!("{cases} clips, {frames_checked} output frames");
}

#[test]
#[ignore = "needs ffmpeg and ffprobe"]
fn a_still_pick_through_a_cursor_is_the_seeked_frame_and_before_at_the_start_is_nothing() {
    let (_, clip) = &clips()[0];
    let all = clip.probed.frames();
    for t in [0.0, 0.5, 1.0 + 1.0 / 60.0, 3.98] {
        let mut cursor = FrameCursor::open(clip.path.to_str().unwrap(), (W, H), &Pick::AtOrAfter(t), config()).expect("open");
        let got = cursor.pick(&Pick::AtOrAfter(t)).expect("a pick").as_deref().map(number);
        assert_eq!(got, Pick::AtOrAfter(t).select(&all).map(|i| i as u32), "t = {t}");
    }
    // Before the first frame of a run from the start: nothing, as over the whole file.
    let mut cursor = FrameCursor::open(clip.path.to_str().unwrap(), (W, H), &Pick::Before(0.0), config()).expect("open");
    assert_eq!(cursor.pick(&Pick::Before(0.0)).expect("a pick"), None);
    // Before the first frame of a run that began later: only an earlier run has it.
    let mut cursor = FrameCursor::open(clip.path.to_str().unwrap(), (W, H), &Pick::Before(1.0), config()).expect("open");
    assert!(cursor.pick(&Pick::Before(1.0)).is_err());
}

#[test]
#[ignore = "needs ffmpeg and ffprobe"]
fn a_reversed_window_over_the_cap_is_refused_and_picks_go_forward_only() {
    let (_, clip) = &clips()[0];
    let pick = FpsPick {
        speed: 1.0,
        reverse: true,
        window: (0.0, 3.0),
        start: 0.0,
        frame: 0,
        fps: Rational::new(30, 1).unwrap(),
        drop_first: false,
        image: None,
    };
    // 90 frames of 3 KB each against a cap of ten frames.
    let small = CursorConfig {
        window_cap_bytes: 10 * (W * H * 3 / 2) as usize,
        ..config()
    };
    let mut cursor = FrameCursor::open(clip.path.to_str().unwrap(), (W, H), &Pick::Fps(pick), small).expect("open");
    let err = cursor.pick(&Pick::Fps(pick)).expect_err("over the cap");
    assert!(matches!(err, kerf_gpu::GpuError::Unsupported(_)), "{err}");

    // Forward: frame 40 then frame 10 — the second's pixels were dropped.
    let fwd = FpsPick { reverse: false, ..pick };
    let mut cursor = FrameCursor::open(clip.path.to_str().unwrap(), (W, H), &Pick::Fps(fwd), config()).expect("open");
    assert_eq!(
        cursor
            .pick(&Pick::Fps(FpsPick { frame: 40, ..fwd }))
            .unwrap()
            .as_deref()
            .map(number),
        Some(40)
    );
    assert!(cursor.pick(&Pick::Fps(FpsPick { frame: 10, ..fwd })).is_err());
    // The same frame again is fine.
    assert_eq!(
        cursor
            .pick(&Pick::Fps(FpsPick { frame: 40, ..fwd }))
            .unwrap()
            .as_deref()
            .map(number),
        Some(40)
    );
}

#[test]
#[ignore = "needs ffmpeg and ffprobe"]
fn a_window_starting_between_two_microseconds_is_seeked_as_the_export_seeks() {
    let clip = microsecond_clip();
    let all = clip.probed.frames();
    assert_eq!(
        all.time_base,
        Rational::new(1, 1_000_000).unwrap(),
        "the premise: a microsecond time base"
    );
    assert_eq!(all.pts.len(), 120);
    assert_eq!(
        &all.pts[1..4],
        &[33_333, 66_666, 100_000],
        "the premise: truncated timestamps"
    );
    let fps = Rational::new(30, 1).unwrap();
    let mut rounded_up_past_a_frame = 0;
    let mut frames_checked = 0;
    for k in 1..60usize {
        let start = k as f64 / 30.0;
        // The still's spelling would start the run after the frame the export keeps.
        if (start * 1e6).round() as i64 > all.pts[k] {
            rounded_up_past_a_frame += 1;
        }
        let pick = FpsPick {
            speed: 1.0,
            reverse: false,
            window: (start, start + 1.0),
            start: 0.0,
            frame: 0,
            fps,
            drop_first: false,
            image: None,
        };
        let mut cursor = FrameCursor::open(clip.path.to_str().unwrap(), (W, H), &Pick::Fps(pick), config())
            .unwrap_or_else(|e| panic!("open at {start}: {e}"));
        picks_through(&mut cursor, pick, 0..33, |frame, shown| {
            let expected = Pick::Fps(FpsPick { frame, ..pick }).select(&all);
            assert_eq!(
                shown.as_deref().map(number),
                expected.map(|i| i as u32),
                "window start {k}/30 s, output frame {frame}"
            );
            frames_checked += 1;
        })
        .unwrap_or_else(|e| panic!("window start {k}/30 s: {e}"));
    }
    assert!(
        rounded_up_past_a_frame > 10,
        "only {rounded_up_past_a_frame} starts distinguish the spellings"
    );
    eprintln!("{frames_checked} output frames, {rounded_up_past_a_frame} starts the nearest-microsecond spelling gets wrong");
}
