//! The `fps` pick against the frames the export really draws (`#[ignore]`d: they drive the
//! real `ffmpeg` and `ffprobe`, on 6.1 and 9.0 alike).
//!
//! `fps_pick` claims to be the choice FFmpeg's `fps` filter makes, rounding and all, and a
//! string-level test cannot say whether it is. These render **clips whose frames number
//! themselves** through the real export graph and compare, for every output frame, the number
//! on the pixels with the frame `fps_pick` names over the file's own timestamps (read with
//! `ffprobe`, as a decoder's index would hold them):
//!
//! * speed 0.5 / 1 / 1.5 / 2 / 4, forward and reverse, in 24 / 25 / 29.97 / 30 / 60 / 23.976 fps
//!   exports of 10 / 24 / 25 / 29.97 / 30 / 60 / 23.976 fps sources — including the NTSC
//!   rationals (`2997/100`, `2997/125`) whose slot boundaries are not on any decimal;
//! * the clip's phase against the output grid, from on it to most of a frame past (where "the
//!   frame containing the time" and the filter part), a seek off the frame grid, a window that
//!   reaches the end of the file, a window of one frame;
//! * other containers and time bases (matroska's millisecond ticks, a transport stream with a
//!   container start of its own) and a variable-frame-rate source, forward and reversed;
//! * the proxy of a source whose video starts late (`proxy/late-video-start`): the pick over a
//!   head-padded proxy's frames is what the preview graph renders from it, at the head (the
//!   pad's clone dropped) and deeper in.
//!
//! A frame number of 0 is the black canvas: the clip is **not drawn**, which is how the plan's
//! candidates at a closing edge are resolved (`None` from the pick). Every clip sits over a
//! black filler clip that keeps the canvas running past its end, so the frames after it are
//! compared too.
//!
//! What the matching needed, in the order it was found (each is in `frame_pick.rs`): `setpts`
//! truncates its result to a tick (`(int64_t)d`, not a rounding); the stream's end is where the
//! frame that `trim` dropped would have landed, and `fps` draws nothing from there on.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored picked`
//! (and again with `KERF_FFMPEG` / `KERF_FFPROBE` at the pinned build.)

#![allow(clippy::print_stderr)]

use std::path::Path;

use uuid::Uuid;

use super::rendered::{export_frames_sized, scratch};
use super::*;
use crate::clip_timing::Rational;
use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track, ProxyGuard, StatusBounded};
use crate::frame_pick::SourceFrames;
use crate::media::{MediaResolver, ProxyMedia};
use crate::model::{Asset, Timeline};
use crate::planner::{PlanRequest, Planner};
use crate::render_plan::CompositeColorPolicy;

const W: u32 = 64;
const H: u32 = 32;

/// Bit `k` of the frame number plus one is the `k`-th eighth of the picture's width, white for
/// 1 (so no frame reads as the black canvas, which is 0).
const BARCODE: &str = "geq=lum='if(bitand(trunc((N+1)/pow(2,trunc(X*8/W))),1),235,16)':cb=128:cr=128";

/// The code a rendered frame carries on pixel row `y`: `frame number + 1`, or 0 for black
/// (nothing drawn).
fn code(frame: &[u8], y: u32) -> u32 {
    let row = (y * W) as usize * 3;
    (0..8u32)
        .filter(|k| frame[row + ((2 * k + 1) * W / 16) as usize * 3] > 125)
        .map(|k| 1u32 << k)
        .sum()
}

fn run(args: &[&str]) {
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success(), "ffmpeg {args:?}");
}

/// A lossless clip as a source: its asset and the number of each frame in file order.
struct Source {
    asset: Asset,
    numbers: Vec<u32>,
}

/// How a source is written.
#[derive(Clone, Copy)]
struct Kind {
    /// Every frame whose number mod 5 is 3 is left out, so the timestamps have gaps.
    vfr: bool,
    /// The container's extension.
    ext: &'static str,
}

const MP4: Kind = Kind { vfr: false, ext: "mp4" };

/// A lossless `W`x`H` clip of `secs` seconds at `fps` (the text ffmpeg is given), frame `n`
/// carrying the code `n + 1`.
fn numbered(dir: &Path, fps: &str, secs: u32, kind: Kind) -> Source {
    let name = format!("numbered-{fps}-{secs}{}.{}", if kind.vfr { "-vfr" } else { "" }, kind.ext);
    let path = dir.join(name);
    let select = if kind.vfr { ",select='not(eq(mod(n,5),3))'" } else { "" };
    if !path.exists() {
        let graph = format!("color=c=black:s={W}x{H}:r={fps}:d={secs},format=yuv420p,{BARCODE}{select}");
        let out = path.to_string_lossy().into_owned();
        run(&[
            "-f",
            "lavfi",
            "-i",
            &graph,
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-qp",
            "0",
            // An all-intra transport stream: FFmpeg 6 seeks into the middle of a GOP sloppily.
            "-g",
            if kind.ext == "ts" { "1" } else { "15" },
            "-pix_fmt",
            "yuv420p",
            "-fps_mode",
            "passthrough",
            &out,
        ]);
    }
    let rate: f64 = fps.parse().unwrap();
    let pts = source_frames(&path).pts;
    let numbers: Vec<u32> = if kind.vfr {
        (0..(f64::from(secs) * rate).round() as u32).filter(|n| n % 5 != 3).collect()
    } else {
        (0..pts.len() as u32).collect()
    };
    assert_eq!(numbers.len(), pts.len(), "{fps} fps: {} frames written", pts.len());
    let mut asset = test_asset(vec![video_stream(W, H, rate)]);
    asset.streams[0].pix_fmt = Some("yuv420p".into());
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = probed_duration(&path);
    Source { asset, numbers }
}

/// A black clip that keeps the canvas running under the one being tested.
fn filler(dir: &Path) -> Asset {
    let path = dir.join("filler.mp4");
    if !path.exists() {
        let graph = format!("color=c=black:s={W}x{H}:r=30:d=8,format=yuv420p");
        run(&[
            "-f",
            "lavfi",
            "-i",
            &graph,
            "-c:v",
            "libx264",
            "-qp",
            "0",
            "-pix_fmt",
            "yuv420p",
            &path.to_string_lossy(),
        ]);
    }
    let mut asset = test_asset(vec![video_stream(W, H, 30.0)]);
    asset.streams[0].pix_fmt = Some("yuv420p".into());
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 8.0;
    asset
}

fn ffprobe_json(path: &Path, entries: &str) -> serde_json::Value {
    let out = command(&ffprobe_bin())
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            entries,
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("ffprobe json")
}

fn probed_duration(path: &Path) -> f64 {
    ffprobe_json(path, "format=duration")["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse().ok())
        .expect("a duration")
}

/// What `ffprobe` states of `path`'s first video stream's frames.
pub(super) struct Probed {
    /// Presentation timestamps (ticks, ascending).
    pub pts: Vec<i64>,
    pub time_base: Rational,
    /// The container's start in microseconds.
    pub start_us: i64,
    /// The last frame's own duration in ticks (0 when ffprobe says none).
    pub last_duration: i64,
}

impl Probed {
    pub(super) fn frames(&self) -> SourceFrames<'_> {
        SourceFrames {
            pts: &self.pts,
            time_base: self.time_base,
            start_us: self.start_us,
            last_duration: self.last_duration,
        }
    }
}

pub(super) fn source_frames(path: &Path) -> Probed {
    let json = ffprobe_json(
        path,
        "frame=pts,pkt_pts,duration,pkt_duration:stream=time_base:format=start_time",
    );
    let mut frames: Vec<(i64, i64)> = json["frames"]
        .as_array()
        .expect("frames")
        .iter()
        .map(|f| {
            let pts = f["pts"].as_i64().or_else(|| f["pkt_pts"].as_i64()).expect("a frame pts");
            let duration = f["duration"].as_i64().or_else(|| f["pkt_duration"].as_i64());
            (pts, duration.unwrap_or(0))
        })
        .collect();
    frames.sort_unstable();
    let tb = json["streams"][0]["time_base"].as_str().expect("time base");
    let (num, den) = tb.split_once('/').expect("a/b");
    let time_base = Rational::new(num.parse().unwrap(), den.parse().unwrap()).expect("time base");
    let start = json["format"]["start_time"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    Probed {
        last_duration: frames.last().map_or(0, |f| f.1),
        pts: frames.into_iter().map(|f| f.0).collect(),
        time_base,
        start_us: (start * 1e6).round() as i64,
    }
}

fn export_options(fps: f64) -> ExportOptions {
    ExportOptions {
        resolution: Some((W, H)),
        fps: Some(fps),
        ..ExportOptions::default()
    }
}

/// One clip over the filler: `source_in` for `secs` timeline seconds at `speed` (negative:
/// backwards), `start` seconds in. Returns the timeline and the clip's id.
fn timeline_of_clip(src: &Asset, filler: &Asset, source_in: f64, secs: f64, speed: f64, start: f64) -> (Timeline, Uuid) {
    let mut clip = make_clip(src.id, source_in, source_in + secs * speed.abs(), start);
    clip.speed = speed;
    let id = clip.id;
    let base = make_clip(filler.id, 0.0, start + secs + 0.6, 0.0);
    (timeline_of(vec![video_track(vec![base]), video_track(vec![clip])]), id)
}

/// What a comparison found: the frames where the render and the pick disagree, `(frame,
/// rendered code, planned code)`, and how many frames the clip was drawn on.
struct Found {
    wrong: Vec<(usize, u32, u32)>,
    drawn: usize,
}

/// Render `timeline` and read the clip's frame number off every output frame; plan the same
/// frames and ask the clip's layer's pick over `decoded`'s timestamps (`numbers` says what
/// each of its frames is). `render_assets` are what the export is rendered from, `plan_assets`
/// what the planner is given (with `resolver` choosing the file). A still image has no file
/// timeline (`decoded` is `None`): its picture is all white, code 255, and what is compared is
/// whether it is drawn.
#[allow(clippy::too_many_arguments)]
fn compare(
    timeline: &Timeline,
    clip: Uuid,
    render_assets: &[Asset],
    plan_assets: &[Asset],
    resolver: Option<&dyn MediaResolver>,
    decoded: Option<&Path>,
    numbers: &[u32],
    fps: f64,
    dir: &Path,
    tag: &str,
    row: u32,
) -> Found {
    let frames = export_frames_sized(timeline, render_assets, fps, (W, H), dir, tag);
    let mut request = PlanRequest::motion(CompositeColorPolicy::FixedBt601);
    if let Some(r) = resolver {
        request = request.with_media(r);
    }
    let planner = Planner::new(timeline, plan_assets, &export_options(fps), request).expect("plan");
    let probed = decoded.map(source_frames);
    if let Some(probed) = &probed {
        assert_eq!(probed.pts.len(), numbers.len(), "{tag}: the numbers of the frames");
    }
    let src = probed.as_ref().map_or(SourceFrames::NONE, Probed::frames);
    let mut found = Found {
        wrong: Vec::new(),
        drawn: 0,
    };
    for (k, frame) in frames.iter().enumerate() {
        let rendered = code(frame, row);
        let plan = planner.at_frame(k as u64).expect("plan");
        let planned = plan
            .layers
            .iter()
            .find(|l| l.clip_id == clip)
            .and_then(|l| l.pick.select(&src))
            .map_or(0, |i| if decoded.is_some() { numbers[i] + 1 } else { 255 });
        found.drawn += usize::from(rendered != 0);
        if rendered != planned {
            found.wrong.push((k, rendered, planned));
        }
    }
    let _ = std::fs::remove_file(dir.join(format!("{tag}.rgb")));
    found
}

/// One clip to check: a source, an export rate and how the clip plays.
struct Spec<'a> {
    label: String,
    src: &'a Source,
    export: &'a str,
    seek: f64,
    secs: f64,
    speed: f64,
    /// Output frames in, plus `phase` of a frame.
    phase: f64,
}

/// Check every spec, print a summary, and fail listing each clip whose frames differ.
fn sweep(dir: &Path, filler: &Asset, specs: Vec<Spec>) {
    let (mut drawn, mut bad) = (0, Vec::new());
    let total = specs.len();
    for (n, s) in specs.iter().enumerate() {
        let fps: f64 = s.export.parse().unwrap();
        let start = (6.0 + s.phase) / fps;
        let (timeline, clip) = timeline_of_clip(&s.src.asset, filler, s.seek, s.secs, s.speed, start);
        let assets = [s.src.asset.clone(), filler.clone()];
        let found = compare(
            &timeline,
            clip,
            &assets,
            &assets,
            None,
            Some(Path::new(&s.src.asset.path)),
            &s.src.numbers,
            fps,
            dir,
            &format!("s{n}"),
            H / 2,
        );
        drawn += found.drawn;
        if !found.wrong.is_empty() {
            bad.push(format!(
                "{}: {} of {} frames differ, first (frame, rendered, planned) {:?}",
                s.label,
                found.wrong.len(),
                found.drawn,
                &found.wrong[..found.wrong.len().min(4)]
            ));
        }
    }
    eprintln!(
        "{total} clips, {drawn} drawn frames compared, {} clips with a mismatch",
        bad.len()
    );
    assert!(drawn > total, "the clips must be drawn");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

fn label(src: &str, export: &str, speed: f64, reverse: bool, phase: f64, seek: f64, secs: f64) -> String {
    format!(
        "{src} fps source -> {export} fps export, speed {speed}{}, phase {phase}, seek {seek}, {secs} s",
        if reverse { " reversed" } else { "" }
    )
}

/// Speed, reverse, phase, seek and rate, over the export graph. 4 sources x 4 exports x 4
/// speeds x 2 directions, each at a phase and a seek that rotate through the lists, so every
/// value of each is met with several of the others.
#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn the_fps_pick_is_the_frame_the_export_renders_over_speed_reverse_phase_and_rate() {
    let dir = scratch("pick-matrix");
    let filler = filler(&dir);
    let sources: Vec<(&str, Source)> = ["10", "24", "29.97", "30"]
        .into_iter()
        .map(|s| (s, numbered(&dir, s, 4, MP4)))
        .collect();
    let phases = [0.0, 0.3, 0.5, 0.7, 0.9, 1.5];
    let seeks = [0.0, 0.3, 0.35, 1.0];
    let mut specs = Vec::new();
    for (name, src) in &sources {
        for export in ["24", "29.97", "30", "23.976"] {
            for speed in [0.5, 1.0, 1.5, 2.0] {
                for reverse in [false, true] {
                    let n = specs.len();
                    let (phase, seek) = (phases[n % phases.len()], seeks[n % seeks.len()]);
                    specs.push(Spec {
                        label: label(name, export, speed, reverse, phase, seek, 1.0),
                        src,
                        export,
                        seek,
                        secs: 1.0,
                        speed: if reverse { -speed } else { speed },
                        phase,
                    });
                }
            }
        }
    }
    sweep(&dir, &filler, specs);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every phase of the output grid, exactly on the frame and up to most of one past it, at
/// the rate pairs where the slot boundaries fall on a tie (10 -> 30 at speed 1.5 or 2 puts
/// source frames exactly half a slot away) or on a rational no decimal spells (29.97,
/// 23.976), at speeds from 0.5 to 4, forward and reversed, from the head of the file.
#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn the_fps_pick_is_the_frame_the_export_renders_at_every_phase_and_slot_boundary() {
    let dir = scratch("pick-phase");
    let filler = filler(&dir);
    let pairs = [
        ("10", "30"),
        ("24", "30"),
        ("30", "29.97"),
        ("29.97", "30"),
        ("23.976", "29.97"),
        ("60", "24"),
        ("25", "25"),
        ("25", "60"),
        ("29.97", "23.976"),
    ];
    let mut sources: Vec<(&str, Source)> = Vec::new();
    for (src, _) in pairs {
        if !sources.iter().any(|s| s.0 == src) {
            sources.push((src, numbered(&dir, src, 4, MP4)));
        }
    }
    let mut specs = Vec::new();
    for (name, export) in pairs {
        let src = &sources.iter().find(|s| s.0 == name).unwrap().1;
        for speed in [0.5, 1.5, 2.0, 4.0] {
            for reverse in [false, true] {
                for phase in [0.0, 0.2, 0.5, 0.8] {
                    specs.push(Spec {
                        label: label(name, export, speed, reverse, phase, 0.0, 0.75),
                        src,
                        export,
                        seek: 0.0,
                        secs: 0.75,
                        speed: if reverse { -speed } else { speed },
                        phase,
                    });
                }
            }
        }
    }
    sweep(&dir, &filler, specs);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Other containers and time bases, and a variable frame rate. Matroska counts in
/// milliseconds (a 29.97 fps frame is never on a tick), a transport stream in 90 kHz and
/// starts 1.4 s into its own clock, and a VFR file has gaps no rate describes — the
/// reversed clip's timestamps stay in forward order, so the gaps do not mirror.
#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn the_fps_pick_is_the_frame_the_export_renders_from_other_containers_and_a_variable_rate() {
    let dir = scratch("pick-containers");
    let filler = filler(&dir);
    let mkv = Kind { vfr: false, ext: "mkv" };
    let ts = Kind { vfr: false, ext: "ts" };
    let vfr = Kind { vfr: true, ext: "mp4" };
    let vfr_mkv = Kind { vfr: true, ext: "mkv" };
    let sources = [
        ("30 mkv", numbered(&dir, "30", 4, mkv)),
        ("29.97 mkv", numbered(&dir, "29.97", 4, mkv)),
        ("25 ts", numbered(&dir, "25", 4, ts)),
        ("30 vfr", numbered(&dir, "30", 4, vfr)),
        ("30 vfr mkv", numbered(&dir, "30", 4, vfr_mkv)),
    ];
    let mut specs = Vec::new();
    for (name, src) in &sources {
        for export in ["30", "24", "29.97"] {
            for speed in [0.5, 1.0, 2.0] {
                for reverse in [false, true] {
                    let n = specs.len();
                    let (phase, seek) = ([0.0, 0.4, 0.8][n % 3], [0.0, 0.5, 1.0][n % 3]);
                    specs.push(Spec {
                        label: label(name, export, speed, reverse, phase, seek, 1.0),
                        src,
                        export,
                        seek,
                        secs: 1.0,
                        speed: if reverse { -speed } else { speed },
                        phase,
                    });
                }
            }
        }
    }
    sweep(&dir, &filler, specs);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The ends of a file and of a window: a clip that plays to the very end of its footage (the
/// stream ends with the file — the last frame's own duration past it — not with a frame `trim`
/// dropped), a window of one frame and of two, forward, reversed and slowed, in time bases whose
/// frame gaps are not their last frame's duration.
#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn the_fps_pick_is_the_frame_the_export_renders_at_the_end_of_the_file_and_in_tiny_windows() {
    let dir = scratch("pick-ends");
    let filler = filler(&dir);
    // Matroska counts in milliseconds: a 30 fps clip's gaps alternate 33 and 34 ticks and its last
    // frame lasts 33, so the stream's end at the end of the file is the frame's own duration.
    let mkv = Kind { vfr: false, ext: "mkv" };
    let vfr_mkv = Kind { vfr: true, ext: "mkv" };
    let sources = [
        ("10", numbered(&dir, "10", 4, MP4)),
        ("30", numbered(&dir, "30", 4, MP4)),
        ("30 mkv", numbered(&dir, "30", 4, mkv)),
        ("29.97 mkv", numbered(&dir, "29.97", 4, mkv)),
        ("30 vfr mkv", numbered(&dir, "30", 4, vfr_mkv)),
    ];
    let mut specs = Vec::new();
    for (name, src) in &sources {
        for export in ["30", "24"] {
            for speed in [1.0, 0.5, 2.0] {
                for reverse in [false, true] {
                    let signed = if reverse { -speed } else { speed };
                    // To the end of the file: the window closes where the footage does.
                    let duration = src.asset.duration;
                    let secs = 0.8 / speed;
                    specs.push(Spec {
                        label: label(name, export, speed, reverse, 0.3, duration - 0.8, secs) + " (to the end)",
                        src,
                        export,
                        seek: duration - 0.8,
                        secs,
                        speed: signed,
                        phase: 0.3,
                    });
                    // One and two source frames, from the middle of the file and the last of it.
                    for frames in [1.0, 2.0] {
                        let window = frames / src.asset.streams[0].fps.unwrap();
                        for seek in [1.0, duration - window] {
                            specs.push(Spec {
                                label: label(name, export, speed, reverse, 0.6, seek, window / speed) + " (tiny)",
                                src,
                                export,
                                seek,
                                secs: window / speed,
                                speed: signed,
                                phase: 0.6,
                            });
                        }
                    }
                }
            }
        }
    }
    sweep(&dir, &filler, specs);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A clip that plays on under a dissolve (its `tail`: the window `trim` is given reaches past
/// the clip's end, forward, or below its start, reversed) is drawn by the same rule, with the
/// longer window. The incoming clip is a black one at half size, so along the top of the frame
/// only the outgoing clip is to be seen, whole, while the dissolve runs and after its ramp.
#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn the_fps_pick_follows_a_clip_playing_on_under_a_dissolve() {
    let dir = scratch("pick-tail");
    let filler = filler(&dir);
    let src = numbered(&dir, "30", 4, MP4);
    let (mut drawn, mut tails, mut bad) = (0, 0, Vec::new());
    for export in ["30", "29.97", "24"] {
        let fps: f64 = export.parse().unwrap();
        for speed in [1.0, 0.5, 2.0, -1.0, -0.5, -2.0] {
            for phase in [0.0, 0.5] {
                let start = (6.0 + phase) / fps;
                let (mut timeline, outgoing) = timeline_of_clip(&src.asset, &filler, 1.0, 1.0, speed, start);
                let mut incoming = make_clip(filler.id, 0.0, 2.0, start + 1.0);
                incoming.transform.scale = 0.5;
                incoming.transition_in = Some(crate::model::Transition {
                    kind: crate::model::TransitionKind::Crossfade,
                    duration: 0.6,
                });
                timeline.tracks[1].clips.push(incoming);
                timeline.tracks[0].clips[0].source_out = start + 4.0;
                let assets = [src.asset.clone(), filler.clone()];
                let tag = format!("tail-{export}-{speed}-{phase}");
                let found = compare(
                    &timeline,
                    outgoing,
                    &assets,
                    &assets,
                    None,
                    Some(Path::new(&src.asset.path)),
                    &src.numbers,
                    fps,
                    &dir,
                    &tag,
                    2,
                );
                drawn += found.drawn;
                let planner = Planner::new(
                    &timeline,
                    &assets,
                    &export_options(fps),
                    PlanRequest::motion(CompositeColorPolicy::FixedBt601),
                )
                .unwrap();
                tails += usize::from(
                    (0..(start * fps) as u64 + 3 * fps as u64)
                        .any(|k| planner.at_frame(k).unwrap().layers.iter().any(|l| l.fx.tail)),
                );
                if !found.wrong.is_empty() {
                    bad.push(format!(
                        "{export} fps, speed {speed}, phase {phase}: {} of {} frames differ, first {:?}",
                        found.wrong.len(),
                        found.drawn,
                        &found.wrong[..found.wrong.len().min(4)]
                    ));
                }
            }
        }
    }
    eprintln!(
        "{drawn} frames compared, {tails} clips with a tail, {} with a mismatch",
        bad.len()
    );
    assert!(tails >= 30, "the clips must play on: {tails}");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A white still, `W`x`H`.
fn still(dir: &Path) -> Asset {
    let path = dir.join("still.png");
    if !path.exists() {
        let graph = format!("color=c=white:s={W}x{H},format=rgb24");
        run(&["-f", "lavfi", "-i", &graph, "-frames:v", "1", &path.to_string_lossy()]);
    }
    let mut asset = test_asset(vec![crate::engine::test_support::image_stream(W, H)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = crate::model::DEFAULT_IMAGE_DURATION;
    asset
}

/// A still image is read as `-loop 1 -framerate <fps> -t <end>`, with the chain's `trim`
/// absolute, and is drawn by the same rule as footage: its closing edge is the frame the loop's
/// run ends on, not the frame its `enable` window closes on. Lengths from a tenth of a second to
/// two and a half, every phase of the grid, cut from `source_in` 0 and 0.25 (which starts the
/// still on the first frame at or after a quarter second), at six rates and three speeds.
#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn a_still_image_is_drawn_on_the_frames_its_loop_runs_on() {
    let dir = scratch("pick-still");
    let filler = filler(&dir);
    let image = still(&dir);
    let (mut drawn, mut bad, mut cases) = (0, Vec::new(), 0);
    for export in ["24", "25", "29.97", "30", "60", "23.976"] {
        let fps: f64 = export.parse().unwrap();
        for secs in [0.1, 0.4, 1.0, 2.5] {
            for phase in [0.0, 0.3, 0.5, 0.9] {
                for seek in [0.0, 0.25] {
                    let speed = [1.0, 0.5, 2.0][cases % 3];
                    cases += 1;
                    let start = (6.0 + phase) / fps;
                    let (timeline, clip) = timeline_of_clip(&image, &filler, seek, secs, speed, start);
                    let assets = [image.clone(), filler.clone()];
                    let tag = format!("still{cases}");
                    let found = compare(&timeline, clip, &assets, &assets, None, None, &[], fps, &dir, &tag, H / 2);
                    drawn += found.drawn;
                    if !found.wrong.is_empty() {
                        bad.push(format!(
                            "{export} fps, {secs} s at speed {speed}, phase {phase}, source_in {seek}: {} of {} frames differ, first (frame, rendered, planned) {:?}",
                            found.wrong.len(),
                            found.drawn,
                            &found.wrong[..found.wrong.len().min(4)]
                        ));
                    }
                }
            }
        }
    }
    eprintln!("{cases} stills, {drawn} drawn frames compared, {} with a mismatch", bad.len());
    assert!(drawn > cases, "the stills must be drawn");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A source whose video starts `lead` seconds after its audio, the way a camera that opens its
/// microphone first writes it: the container starts at the audio, and the video's first frame
/// is `lead` in. The video is a numbered clip (`lead` 0 is an ordinary file). `None` when this
/// ffmpeg cannot write it.
fn late_numbered(dir: &Path, name: &str, lead: f64) -> Option<Source> {
    let video = numbered(dir, "30", 4, MP4);
    let (audio, out) = (dir.join("tone.m4a"), dir.join(name));
    let ok = |args: &[&str]| {
        command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .status_bounded()
            .is_ok_and(|s| s.success())
    };
    let tone = [
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=8",
        "-c:a",
        "aac",
        audio.to_str()?,
    ];
    let mux = [
        "-itsoffset",
        &lead.to_string(),
        "-i",
        &video.asset.path,
        "-i",
        audio.to_str()?,
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c",
        "copy",
        out.to_str()?,
    ];
    (audio.exists() || ok(&tone)).then_some(())?;
    ok(&mux).then(|| {
        let mut asset = video.asset;
        asset.path = out.to_string_lossy().into_owned();
        asset.duration = probed_duration(&out);
        Source {
            asset,
            numbers: video.numbers,
        }
    })
}

/// `proxy/late-video-start`. A source whose video starts late gets a proxy whose head is
/// padded: one clone of the first frame at time zero, every timestamp kept
/// (`generate_proxy`, `.lead.mp4`), so a seek into the proxy lands where the original's does.
/// The graph drops the clone from a clip read from the start (`trim=start_frame=1`); a
/// renderer that decodes the proxy has to do the same or show the first frame `lead` seconds
/// early. So: for a head clip, a clip from deep in the file and one from an off-grid time, the
/// frame the **preview graph** renders from the proxy (the proxy-swapped assets, as
/// `Project::preview_assets` makes them) is the frame the pick names over the proxy's own
/// timestamps, planned through `ProxyMedia` — which is also where the plan's stream becomes
/// the proxy's, read from the sidecar `generate_proxy` wrote.
#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn proxy_late_video_start_picks_what_the_preview_graph_renders() {
    let dir = scratch("pick-proxy");
    let filler = filler(&dir);
    let mut padded_seen = 0;
    for (i, lead) in [0.0, 0.08, 1.5].into_iter().enumerate() {
        let Some(source) = late_numbered(&dir, &format!("late{i}.mp4"), lead) else {
            eprintln!("skipped: this ffmpeg cannot make the late-starting test clip");
            return;
        };
        let original = &source.asset;
        let proxy = generate_proxy(Path::new(&original.path), PROXY_MAX_WIDTH).expect("proxy");
        let _cleanup = ProxyGuard(proxy.clone());
        let padded = crate::clip_timing::is_head_padded_proxy(&proxy.to_string_lossy());
        assert_eq!(padded, lead > 0.0, "lead {lead}: {proxy:?}");
        padded_seen += usize::from(padded);

        // The sidecar describes the proxy file, so a plan needs no probe of it.
        let sidecar = read_proxy_sidecar(&proxy).expect("generate_proxy wrote a sidecar");
        let probed = probe(&proxy)
            .unwrap()
            .streams
            .into_iter()
            .find(|s| s.kind == StreamKind::Video);
        assert_eq!(Some(sidecar), probed, "lead {lead}");
        // A proxy made before sidecars existed costs one probe, which is written back.
        std::fs::remove_file(proxy.with_extension("json")).unwrap();
        assert_eq!(read_proxy_sidecar(&proxy), None);
        assert_eq!(proxy_video_info(&proxy), probed, "lead {lead}");
        assert_eq!(read_proxy_sidecar(&proxy), probed, "the probe is written back");

        // The proxy's frames: the clone of the first frame at 0, then the footage.
        let mut numbers = source.numbers.clone();
        if padded {
            numbers.insert(0, 0);
            let probed = source_frames(&proxy);
            let (pts, tb) = (&probed.pts, probed.time_base);
            assert_eq!(pts[0], 0, "lead {lead}");
            let first = pts[1] as f64 * f64::from(tb.num) / f64::from(tb.den);
            assert!((first - lead).abs() < 0.002, "lead {lead}: the footage starts at {first}");
        }
        let media = ProxyMedia.resolve(original);
        assert!(media.proxy && media.path == proxy.to_string_lossy());
        // `Project::preview_assets`: the proxy-swapped asset keeps the original's size.
        let mut preview = media.decoded(original);
        preview.streams[0].width = original.streams[0].width;
        preview.streams[0].height = original.streams[0].height;
        let render = [preview, filler.clone()];
        let plan = [original.clone(), filler.clone()];
        for (seek, speed) in [(0.0, 1.0), (0.0, -1.0), (1.0, 1.0), (0.35, 1.0), (0.0, 0.5), (1.0, -2.0)] {
            let (timeline, clip) = timeline_of_clip(original, &filler, seek, 1.0, speed, 6.0 / 30.0);
            let tag = format!("proxy{i}-{seek}-{speed}");
            let found = compare(
                &timeline,
                clip,
                &render,
                &plan,
                Some(&ProxyMedia),
                Some(&proxy),
                &numbers,
                30.0,
                &dir,
                &tag,
                H / 2,
            );
            // (A window that ends before the video begins draws nothing, in the original too.)
            assert!(
                found.drawn > 0 || seek + speed.abs() <= lead,
                "lead {lead}, seek {seek}, speed {speed}"
            );
            assert!(
                found.wrong.is_empty(),
                "lead {lead}, seek {seek}, speed {speed}: (frame, rendered, planned) {:?}",
                &found.wrong[..found.wrong.len().min(6)]
            );
            // The plan says which file it decodes and what the pick does about the clone.
            let planner = Planner::new(
                &timeline,
                &plan,
                &export_options(30.0),
                PlanRequest::motion(CompositeColorPolicy::FixedBt601).with_media(&ProxyMedia),
            )
            .unwrap();
            let layer = planner
                .at_frame(12)
                .unwrap()
                .layers
                .into_iter()
                .find(|l| l.clip_id == clip)
                .unwrap();
            assert!(layer.source.proxy && layer.path == proxy.to_string_lossy());
            let drops = matches!(layer.pick, crate::frame_pick::Pick::Fps(p) if p.drop_first);
            assert_eq!(drops, padded && seek == 0.0, "lead {lead}, seek {seek}");
        }
    }
    assert_eq!(padded_seen, 2);
    let _ = std::fs::remove_dir_all(&dir);
}
