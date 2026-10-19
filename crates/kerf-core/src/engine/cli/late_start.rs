//! A source whose video starts after its audio, exported: the picture and the sound of a clip
//! keep the source's sync (`#[ignore]`d: they drive the real `ffmpeg`).
//!
//! The source has a white flash and a click at the same moment of the file, its video starting
//! `lead` seconds after its audio (an MOV edit list, Matroska timestamps). Every clip cut from it
//! is rendered through the export graph and the white frame and the click are found on the
//! timeline: they must land together, where the clip puts that moment of the source. Before the
//! fix a clip cut from inside the lead showed its picture `lead - source_in` early.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored late_start`
//! (and again with `KERF_FFMPEG` / `KERF_FFPROBE` at the pinned build.)

#![allow(clippy::print_stderr)]

use std::path::Path;

use super::rendered::scratch;
use super::*;
use crate::engine::test_support::{make_clip, single, StatusBounded};
use crate::project::Project;

const FPS: f64 = 25.0;
const RATE: usize = 48_000;
const W: u32 = 64;
const H: u32 = 36;
/// How many frames the flash lasts, so a sped-up clip cannot skip all of it.
const MARK: f64 = 4.0;

fn run_ffmpeg(args: &[String]) -> bool {
    command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status_bounded()
        .is_ok_and(|s| s.success())
}

/// A source whose video starts `lead` seconds after its audio, with [`MARK`] white frames from
/// the video's own second 1 (the file's `lead + 1`) and a click as long at the same moment of
/// the file. Probed like an import.
fn late_source(dir: &Path, lead: f64, ext: &str) -> Asset {
    let video = dir.join("flash.mkv");
    let audio = dir.join(format!("click-{lead}.wav"));
    let out = dir.join(format!("late-{lead}.{ext}"));
    let at = lead + 1.0;
    let s = |p: &Path| p.to_string_lossy().into_owned();
    assert!(
        video.exists()
            || run_ffmpeg(&[
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                format!(
                    "color=black:s={W}x{H}:r={FPS}:d=5,drawbox=enable='between(n,{FPS},{last})':c=white:t=fill",
                    last = FPS + MARK - 1.0
                ),
                "-c:v".into(),
                "libx264".into(),
                "-g".into(),
                "1".into(),
                "-bf".into(),
                "0".into(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                s(&video),
            ])
    );
    assert!(run_ffmpeg(&[
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!(
            "aevalsrc='if(gte(t,{at})*lt(t,{end}),0.5*sin(2*PI*1000*t),0)':s={RATE}:d=6",
            end = at + MARK / FPS
        ),
        "-c:a".into(),
        "pcm_s16le".into(),
        s(&audio),
    ]));
    assert!(run_ffmpeg(&[
        "-itsoffset".into(),
        lead.to_string(),
        "-i".into(),
        s(&video),
        "-i".into(),
        s(&audio),
        "-map".into(),
        "0:v".into(),
        "-map".into(),
        "1:a".into(),
        "-c".into(),
        "copy".into(),
        s(&out),
    ]));
    Project::probe_asset(&out).expect("probe the late source")
}

/// The export of `timeline` as grey frames at [`FPS`] and mono samples at [`RATE`]: the time of
/// the first white frame and of the first sample of the click.
fn flash_and_click(timeline: &Timeline, assets: &[Asset], dir: &Path, tag: &str) -> (Option<f64>, Option<f64>) {
    let opts = ExportOptions {
        resolution: Some((W, H)),
        fps: Some(FPS),
        ..ExportOptions::default()
    };
    let (v, a) = (dir.join(format!("{tag}.gray")), dir.join(format!("{tag}.f32")));
    let mut args = build_export_args(timeline, assets, "unused.mkv", &opts).unwrap();
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args.truncate(graph + 1);
    args.extend(
        [
            "-map",
            "[outv]",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
        ]
        .map(String::from),
    );
    args.push(v.to_string_lossy().into_owned());
    args.extend(["-map", "[outa]", "-f", "f32le", "-ac", "1", "-ar", "48000"].map(String::from));
    args.push(a.to_string_lossy().into_owned());
    let run = command(&ffmpeg_bin())
        .arg("-y")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    let frames = std::fs::read(&v).unwrap();
    let flash = frames
        .chunks((W * H) as usize)
        .position(|f| f.iter().map(|p| u64::from(*p)).sum::<u64>() / u64::from(W * H) > 128)
        .map(|k| k as f64 / FPS);
    let samples = std::fs::read(&a).unwrap();
    let click = samples
        .as_chunks::<4>()
        .0
        .iter()
        .position(|b| f32::from_le_bytes(*b).abs() > 0.05)
        .map(|n| n as f64 / RATE as f64);
    let _ = std::fs::remove_file(&v);
    let _ = std::fs::remove_file(&a);
    (flash, click)
}

#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn a_late_starting_source_keeps_its_sync_in_the_export() {
    let dir = scratch("late-start");
    let mut failures = Vec::new();
    for ext in ["mov", "mkv"] {
        for lead in [0.07, 0.5] {
            let asset = late_source(&dir, lead, ext);
            let assets = vec![asset.clone()];
            let at = lead + 1.0;
            // (source_in, source_out, timeline_start, speed)
            let cases = [
                (0.0, 3.0, 0.0, 1.0),
                (0.0, 3.0, 1.0, 1.0),
                (lead / 2.0, 3.0, 0.0, 1.0),
                (0.0, 3.0, 0.0, 2.0),
                (0.0, 2.0, 0.0, 0.5),
                (lead + 0.3, 3.0, 0.0, 1.0),
                (0.0, 3.0, 0.0, -1.0),
                (lead / 2.0, 3.0, 0.5, -2.0),
            ];
            for (si, so, ts, speed) in cases {
                let mut clip = make_clip(asset.id, si, so, ts);
                clip.speed = speed;
                let s = f64::abs(speed);
                // Forward, the moment plays `(at - si) / s` into the clip. Reversed, the click
                // starts where the flash's end plays, and the picture plays from the
                // last frame before the window's end, so the white frame starts that far from it.
                let last = lead + ((so - lead) * FPS).ceil() / FPS - 1.0 / FPS;
                let mark = MARK / FPS;
                let (flash_at, click_at) = if speed > 0.0 {
                    (ts + (at - si) / s, ts + (at - si) / s)
                } else {
                    (ts + (last - (at + mark - 1.0 / FPS)) / s, ts + (so - at - mark) / s)
                };
                let tag = format!("{ext}-{lead}-{si}-{ts}-{speed}");
                let (flash, click) = flash_and_click(&single(vec![clip]), &assets, &dir, &tag);
                eprintln!("{tag}: flash {flash:?} (want {flash_at:.4}), click {click:?} (want {click_at:.4})");
                let near = |got: Option<f64>, want: f64, tol: f64| got.is_some_and(|t| (t - want).abs() <= tol);
                // `atempo` spreads the click over its overlap windows, and a Matroska seek lands
                // the sound up to a block late whatever the lead (a seek past it): the click is
                // held to a sample-exact time only where neither is in the way.
                let exact_sound = s == 1.0 && !(ext == "mkv" && si >= lead);
                if !near(flash, flash_at, 0.6 * s.max(1.0) / FPS) || (exact_sound && !near(click, click_at, 0.004)) {
                    failures.push(format!(
                        "{tag}: flash {flash:?} (want {flash_at:.4}), click {click:?} (want {click_at:.4})"
                    ));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "out of sync:\n{}", failures.join("\n"));
}
