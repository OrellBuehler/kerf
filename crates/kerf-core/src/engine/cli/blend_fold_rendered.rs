//! A run of blended clips through the real `ffmpeg` (`#[ignore]`d), on FFmpeg 6.1 and 9.0.2 alike.
//!
//! On 9.0.2 a blended clip's fork of the composite nested inside the last one's costs ~3.3 times what that one did
//! (8 clips took 3 s, 11 took 77 s, 12 gave no frame in 250 s; 6.1 stayed linear), so a run of seven or more on one
//! track is folded into one fork ([`blend::fold_runs`]). These hold it to the pictures: every frame of a folded export
//! equals the frame the clips draw each on their own (the neighbours that share its frame with them, in the unaligned
//! case), equals the nested graph's where that completes, and the export finishes within a bounded time.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored blend_fold_rendered`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build).

#![allow(clippy::print_stderr)] // what each case measured, for whoever runs the ignored ones

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::rendered::scratch;
use super::segments::*;
use super::*;
use crate::clip_timing::ClipFx;
use crate::engine::test_support::{make_clip, timeline_of, video_track, StatusBounded};
use crate::model::BlendMode;

const W: u32 = 192;
const H: u32 = 108;
const FPS: f64 = 30.0;

/// The most a case may take on this machine, shared with other builds. Folded, 40 clips take ~6 s on 9.0.2.
const BOUND: Duration = Duration::from_secs(120);

const MODES: [BlendMode; 9] = [
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::HardLight,
    BlendMode::Add,
    BlendMode::Darken,
    BlendMode::Lighten,
    BlendMode::Difference,
    BlendMode::Exclusion,
];

/// A lossless all-intra clip of lavfi source `lavfi`, `secs` long.
fn source(dir: &Path, name: &str, lavfi: &str, secs: u32) -> Asset {
    let path = dir.join(format!("{name}.mp4"));
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("{lavfi}=s={W}x{H}:r=30:d={secs},format=yuv420p"))
        .args(["-c:v", "libx264", "-crf", "0", "-g", "1"])
        .arg(&path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());
    // As imported: the probe records the file's tick, which the guard reads.
    let asset = crate::project::Project::probe_asset(&path).expect("probe");
    assert!(asset.streams[0].time_base.is_some());
    asset
}

/// A base clip with something to blend with, and one picture for every blended clip to draw (one file: one input,
/// fanned out with `split`, which is the shape that hung).
struct Case {
    base: Asset,
    src: Asset,
    clips: Vec<Clip>,
    frames: u64,
}

impl Case {
    fn new(dir: &Path, bounds: &[f64], frames: u64) -> Case {
        let secs = (frames / 30 + 2) as u32;
        let base = source(dir, "base", "testsrc", secs);
        let src = source(dir, "src", "testsrc2", 2);
        // Clip i is drawn from `bounds[i]` to `bounds[i + 1]`, in frames.
        let clips = bounds
            .windows(2)
            .enumerate()
            .map(|(i, w)| {
                let mut clip = make_clip(src.id, 0.5, 0.5 + (w[1] - w[0]) / FPS, w[0] / FPS);
                clip.blend_mode = MODES[i % MODES.len()];
                clip.transform.scale = 0.7;
                clip.transform.pos_x = 0.1;
                clip.transform.opacity = 0.43;
                clip
            })
            .collect();
        Case {
            base,
            src,
            clips,
            frames,
        }
    }

    /// `n` clips of two frames, each starting on the frame the last ends.
    fn butted(dir: &Path, n: usize) -> Case {
        let bounds: Vec<f64> = (0..=n).map(|i| 2.0 * i as f64).collect();
        Case::new(dir, &bounds, 2 * n as u64)
    }

    /// `n` clips of five frames whose cuts fall at 0.3 of a frame: the clip before is drawn on the frame the next starts on.
    fn unaligned(dir: &Path, n: usize) -> Case {
        let bounds: Vec<f64> = (0..=n).map(|i| if i == 0 { 0.0 } else { 5.0 * i as f64 + 0.3 }).collect();
        Case::new(dir, &bounds, 5 * n as u64 + 1)
    }

    fn assets(&self) -> Vec<Asset> {
        vec![self.base.clone(), self.src.clone()]
    }

    /// The base under the clips named (`None`: none).
    fn timeline(&self, only: &[usize]) -> Timeline {
        let clips = only.iter().map(|&i| self.clips[i].clone()).collect();
        timeline_of(vec![
            video_track(vec![make_clip(self.base.id, 0.0, self.frames as f64 / FPS, 0.0)]),
            video_track(clips),
        ])
    }

    /// The output frames clip `i` can be drawn on.
    fn slots(&self, i: usize) -> std::ops::Range<usize> {
        // (No transition here: the fx is the source's tick alone.)
        let fx = ClipFx {
            time_base: self.src.streams[0].time_base,
            ..ClipFx::default()
        };
        let (first, end) = ClipTiming::new(&self.clips[i], &fx).drawn_slots(Rational::from_fps(FPS).unwrap());
        first as usize..(end as usize).min(self.frames as usize)
    }
}

fn opts() -> ExportOptions {
    ExportOptions {
        fps: Some(FPS),
        resolution: Some((W, H)),
        ..ExportOptions::default()
    }
}

/// Run `cmd`, killing it past `limit`; the seconds it took. What it says on stderr goes to `{dir}/{what}.err`, and is
/// the panic's message when it fails.
fn run_within(cmd: &mut Command, limit: Duration, dir: &Path, what: &str) -> Duration {
    run_sampled(cmd, limit, dir, what).0
}

/// [`run_within`] that also answers the most threads the process held at once, as far as it could be seen: sampled from
/// `/proc` every 20 ms on Linux, 0 elsewhere.
fn run_sampled(cmd: &mut Command, limit: Duration, dir: &Path, what: &str) -> (Duration, usize) {
    let errors = dir.join(format!("{what}.err"));
    let started = Instant::now();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("spawn ffmpeg");
    let mut peak = 0;
    loop {
        if cfg!(target_os = "linux") {
            let status = std::fs::read_to_string(format!("/proc/{}/status", child.id())).unwrap_or_default();
            let held = status
                .lines()
                .find_map(|l| l.strip_prefix("Threads:"))
                .and_then(|n| n.trim().parse::<usize>().ok());
            peak = peak.max(held.unwrap_or(0));
        }
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "{what}: ffmpeg failed\n{}",
                std::fs::read_to_string(&errors).unwrap_or_default()
            );
            return (started.elapsed(), peak);
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what}: no end after {}s; killed it", limit.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read_md5s(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| l.rsplit(", ").next().unwrap().to_string())
        .collect()
}

/// The `framemd5` of every frame of `timeline`'s export graph (`[outv]` before the encoder), and the time it took.
fn export_md5s(timeline: &Timeline, assets: &[Asset], dir: &Path, tag: &str, limit: Duration) -> (Vec<String>, Duration) {
    let (md5s, took, _) = export_md5s_peak(timeline, assets, dir, tag, limit);
    (md5s, took)
}

/// [`export_md5s`] that also answers the most threads ffmpeg held (see [`run_sampled`]). It is spawned with the caps an export
/// is spawned with (`spawn_caps`: the budget's, and a wide graph's), not with the bare argv.
fn export_md5s_peak(
    timeline: &Timeline,
    assets: &[Asset],
    dir: &Path,
    tag: &str,
    limit: Duration,
) -> (Vec<String>, Duration, usize) {
    let out = dir.join(format!("{tag}.md5"));
    let mut args = build_export_args(timeline, assets, "unused.mkv", &opts()).unwrap();
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args.truncate(graph + 1);
    args.extend(["-map", "[outv]", "-fps_mode", "passthrough", "-f", "framemd5", "-y"].map(String::from));
    args.push(out.to_string_lossy().into_owned());
    let _script = externalize_filter_complex(&mut args, tag).unwrap();
    spawn_caps(&mut args, crate::engine::cpu::budget(), &[]);
    let (took, peak) = run_sampled(
        command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-nostats"])
            .args(&args),
        limit,
        dir,
        tag,
    );
    (read_md5s(&out), took, peak)
}

/// The `framemd5` of the picture of a file.
fn file_md5s(path: &Path, dir: &Path, tag: &str) -> Vec<String> {
    let out = dir.join(format!("{tag}.md5"));
    run_within(
        command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(path)
            .args(["-map", "0:v:0", "-f", "framemd5", "-y"])
            .arg(&out),
        BOUND,
        dir,
        tag,
    );
    read_md5s(&out)
}

/// What every frame of the case's export has to be: the base where no clip is drawn, and where one is, the frame of the
/// export of that clip and the clips beside it alone (nested blends, a few of them, which every build draws quickly).
fn expected(case: &Case, dir: &Path) -> Vec<String> {
    let assets = case.assets();
    let (mut want, _) = export_md5s(&case.timeline(&[]), &assets, dir, "base", BOUND);
    assert_eq!(want.len() as u64, case.frames);
    for i in 0..case.clips.len() {
        let near: Vec<usize> = (i.saturating_sub(1)..(i + 2).min(case.clips.len())).collect();
        let (alone, _) = export_md5s(&case.timeline(&near), &assets, dir, &format!("near{i}"), BOUND);
        assert_eq!(alone.len(), want.len());
        for k in case.slots(i) {
            want[k].clone_from(&alone[k]);
        }
    }
    want
}

fn assert_frames(got: &[String], want: &[String], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: frames");
    let wrong: Vec<usize> = (0..got.len()).filter(|&k| got[k] != want[k]).collect();
    assert!(
        wrong.is_empty(),
        "{what}: {} of {} frames differ, from frame {}",
        wrong.len(),
        got.len(),
        wrong[0]
    );
}

fn version() -> String {
    let out = command(&ffmpeg_bin()).arg("-version").output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").to_string()
}

/// FFmpeg 9 and later nest a fork's work with the depth of the forks around it.
fn nested_is_slow() -> bool {
    let v = version();
    let major = v
        .trim_start_matches("ffmpeg version ")
        .trim_start_matches('n')
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|m| m.parse::<u32>().ok())
        .unwrap_or(0);
    major >= 9
}

/// Twelve, twenty and forty clips, one after another on the frame, over one source: folded, each export ends in a
/// bounded time, and every frame is the one its clip draws alone.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn butted_blended_clips_export_in_a_bounded_time_as_each_clip_alone_draws_them() {
    let dir = scratch("blend-fold-butted");
    eprintln!("{}", version());
    for n in [12, 20, 40] {
        let case = Case::butted(&dir, n);
        let tl = case.timeline(&(0..n).collect::<Vec<_>>());
        let folds = {
            let cut = prepared_cut(&tl, &opts());
            blend_folds(&cut, &case.assets(), &transition_fx(&cut, &case.assets()), FPS, &|_| true)
        };
        assert_eq!(folds, vec![(1..=n).collect::<Vec<_>>()], "{n} butted clips fold into one run");
        let (got, took) = export_md5s(&tl, &case.assets(), &dir, &format!("fold{n}"), BOUND);
        eprintln!(
            "{n} butted blended clips: {} frames in {:.1} s",
            got.len(),
            took.as_secs_f64()
        );
        assert_frames(&got, &expected(&case, &dir), &format!("{n} butted clips"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The fold is the nested graph's pictures, frame for frame, where the nested graph finishes: eight clips on every
/// build, twelve where that is not the 9.0 slowdown.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_fold_is_frame_for_frame_the_nested_graph() {
    let dir = scratch("blend-fold-nested");
    eprintln!("{}", version());
    let sizes: &[usize] = if nested_is_slow() { &[7, 8] } else { &[7, 8, 12] };
    for &n in sizes {
        let case = Case::butted(&dir, n);
        let tl = case.timeline(&(0..n).collect::<Vec<_>>());
        let (folded, folded_took) = export_md5s(&tl, &case.assets(), &dir, &format!("f{n}"), BOUND);
        let (nested, nested_took) = with_fold_min(usize::MAX, || export_md5s(&tl, &case.assets(), &dir, &format!("n{n}"), BOUND));
        eprintln!(
            "{n} clips: folded {:.1} s, nested {:.1} s, {} frames",
            folded_took.as_secs_f64(),
            nested_took.as_secs_f64(),
            folded.len()
        );
        assert_frames(&folded, &nested, &format!("{n} clips, folded against nested"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Seeded layouts of seven to nine blended clips on a track with a plain clip under and over them: cuts on the frame and
/// between frames (where the next clip's nearest slot is the one after the last's), gaps, fades, speed, opacity, size and
/// place. Folded, each is the nested graph's pictures frame for frame.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn seeded_layouts_fold_to_the_nested_pictures() {
    let dir = scratch("blend-fold-sweep");
    eprintln!("{}", version());
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n.max(1)
    };
    let base = source(&dir, "base", "testsrc", 6);
    let src = source(&dir, "src", "testsrc2", 4);
    let assets = vec![base.clone(), src.clone()];
    for case in 0..8 {
        let n = 7 + next(3) as usize;
        let mut at = next(3) as f64;
        let mut clips = Vec::new();
        for i in 0..n {
            let frames = 2.0 + next(4) as f64;
            let mut end = at.floor() + frames;
            // Half the cuts fall between frames, past the half, where the next clip's nearest slot is the one after.
            if next(2) == 0 {
                end += 0.6 + next(35) as f64 / 100.0;
            }
            let mut clip = make_clip(src.id, 0.5, 0.5 + (end - at) / FPS, at / FPS);
            clip.blend_mode = MODES[next(MODES.len() as u64) as usize];
            clip.transform.opacity = 0.3 + next(70) as f64 / 100.0;
            clip.transform.scale = 0.5 + next(50) as f64 / 100.0;
            clip.transform.pos_x = (next(40) as f64 - 20.0) / 100.0;
            clip.transform.pos_y = (next(40) as f64 - 20.0) / 100.0;
            if next(3) == 0 {
                clip.fade_in = 1.0 / FPS;
            }
            if next(3) == 0 {
                clip.fade_out = 1.0 / FPS;
            }
            if i % 4 == 2 {
                clip.speed = 2.0;
                clip.source_out = clip.source_in + 2.0 * (end - at) / FPS;
            }
            clips.push(clip);
            // Sometimes a gap; the next clip's start is the end of this one.
            at = end + if next(4) == 0 { 1.0 + next(2) as f64 } else { 0.0 };
        }
        let length = clips.last().map_or(0.0, Clip::timeline_end) + 3.0 / FPS;
        let mut under = make_clip(src.id, 0.0, length / 2.0, 0.0);
        under.transform.scale = 0.6;
        under.transform.pos_x = -0.2;
        let mut over = make_clip(src.id, 1.0, 1.0 + length / 3.0, length / 3.0);
        over.transform.scale = 0.4;
        over.transform.pos_x = 0.25;
        over.transform.pos_y = 0.25;
        let tl = timeline_of(vec![
            video_track(vec![make_clip(base.id, 0.0, length, 0.0)]),
            video_track(vec![under]),
            video_track(clips),
            video_track(vec![over]),
        ]);
        let folds = {
            let cut = prepared_cut(&tl, &opts());
            blend_folds(&cut, &assets, &transition_fx(&cut, &assets), FPS, &|_| true)
        };
        assert_eq!(folds.len(), 1, "case {case}: the run folds");
        assert_eq!(folds[0].len(), n, "case {case}");
        let (folded, folded_took) = export_md5s(&tl, &assets, &dir, &format!("sf{case}"), BOUND);
        let (nested, nested_took) = with_fold_min(usize::MAX, || export_md5s(&tl, &assets, &dir, &format!("sn{case}"), BOUND));
        eprintln!(
            "case {case}, {n} clips: folded {:.1} s, nested {:.1} s, {} frames",
            folded_took.as_secs_f64(),
            nested_took.as_secs_f64(),
            folded.len()
        );
        assert_frames(&folded, &nested, &format!("case {case}, folded against nested"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A hundred clips: one split of a hundred and one, a hundred stages and a chain of a hundred overlays configure and run, in the
/// threads a process may hold.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_hundred_butted_blended_clips_fold_within_ffmpegs_limits() {
    let dir = scratch("blend-fold-hundred");
    eprintln!("{}", version());
    let case = Case::butted(&dir, 100);
    let tl = case.timeline(&(0..100).collect::<Vec<_>>());
    let (got, took, threads) = export_md5s_peak(&tl, &case.assets(), &dir, "hundred", Duration::from_secs(600));
    eprintln!(
        "100 butted blended clips: {} frames in {:.1} s, {threads} threads at most",
        got.len(),
        took.as_secs_f64()
    );
    assert_eq!(got.len(), 200);
    // About 1,000 swscale contexts of their own pools: 3,600 threads on 4 cores uncapped, which a process on macOS may not hold
    // (the swscaler fails to initialise and the encoder with it). The graph's cap keeps them to a few hundred.
    assert!(threads < 600, "{threads} threads at once");
    // Spot-check a clip in each tenth against its own export alone.
    let assets = case.assets();
    for i in (3..100).step_by(10) {
        let (alone, _) = export_md5s(&case.timeline(&[i]), &assets, &dir, &format!("one{i}"), BOUND);
        for k in case.slots(i) {
            assert_eq!(got[k], alone[k], "clip {i}, frame {k}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Cuts that share a frame cannot fold; past the limit the export is cut into parts, each with few enough nested, and it
/// finishes in a bounded time on every build.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn blended_clips_that_cannot_fold_export_in_parts_in_a_bounded_time() {
    let dir = scratch("blend-fold-parts");
    eprintln!("{}", version());
    let case = Case::unaligned(&dir, 14);
    let assets = case.assets();
    let tl = case.timeline(&(0..14).collect::<Vec<_>>());
    let lossless = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..opts()
    };
    let mut plan = plan_export(&tl, &assets, &lossless, SEGMENT_LIMITS).expect("fourteen nested blends are cut");
    assert!(plan.parts.len() >= 3, "{:?}", plan.parts);
    plan.scratch = Some(dir.join("parts"));
    let out = dir.join("parts.mkv");
    let started = Instant::now();
    let status = render_in_parts(
        &plan,
        &assets,
        &out,
        &lossless,
        &mut |_| {},
        &|| started.elapsed() > BOUND,
        &std::cell::Cell::new(false),
    )
    .unwrap();
    let took = started.elapsed();
    assert_eq!(status, RenderStatus::Completed, "no end after {}s", BOUND.as_secs());
    eprintln!(
        "14 blended clips whose cuts share a frame: {} parts, {} frames in {:.1} s",
        plan.parts.len(),
        plan.frames,
        took.as_secs_f64()
    );
    let got = file_md5s(&out, &dir, "parts");
    assert_frames(&got, &expected(&case, &dir), "the export in parts");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The unaligned case again, whole, where it can be: on a build whose nested forks are cheap (6.1), the export in one run
/// is the export in parts.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn parts_of_nested_blends_are_the_whole_exports_frames() {
    if nested_is_slow() {
        eprintln!("{}: the nested whole is the slowdown itself; skipped", version());
        return;
    }
    let dir = scratch("blend-fold-parts-whole");
    let case = Case::unaligned(&dir, 14);
    let assets = case.assets();
    let tl = case.timeline(&(0..14).collect::<Vec<_>>());
    let (whole, took) = export_md5s(&tl, &assets, &dir, "whole", BOUND);
    eprintln!("14 nested blends in one run: {:.1} s", took.as_secs_f64());
    assert_frames(&whole, &expected(&case, &dir), "the export in one run");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Folded runs nest with each other too (3 of ten clips take 10 s on 9.0.2, 4 over 120 s): four runs on four tracks, each
/// in its own stretch of the cut, are cut into parts by the planner, one run in each, and the export ends in a bounded time
/// with every frame what its run draws alone.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn runs_on_separate_tracks_export_in_parts_in_a_bounded_time() {
    let dir = scratch("blend-fold-runs");
    eprintln!("{}", version());
    let case = Case::butted(&dir, 10);
    let assets = case.assets();
    let (stretch, runs) = (20usize, 4usize);
    let frames = stretch * runs;
    let track = |r: usize| -> Vec<Clip> {
        case.clips
            .iter()
            .map(|c| {
                let mut c = c.clone();
                c.timeline_start += (r * stretch) as f64 / FPS;
                c
            })
            .collect()
    };
    let timeline = |only: &[usize]| {
        let mut tracks = vec![video_track(vec![make_clip(case.base.id, 0.0, frames as f64 / FPS, 0.0)])];
        tracks.extend(only.iter().map(|&r| video_track(track(r))));
        timeline_of(tracks)
    };
    let tl = timeline(&(0..runs).collect::<Vec<_>>());
    let lossless = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..opts()
    };
    let mut plan = plan_export(&tl, &assets, &lossless, SEGMENT_LIMITS).expect("four runs in series are cut");
    assert!(plan.parts.len() >= 2, "{:?}", plan.parts);
    for part in &plan.parts {
        let (_, _, forks) = open_cost(plan.clips.iter().filter(|c| c.first < part.end() && c.last >= part.first));
        assert!(forks <= SEGMENT_LIMITS.blends, "{part:?}: {forks}");
    }
    plan.scratch = Some(dir.join("parts"));
    let out = dir.join("parts.mkv");
    let started = Instant::now();
    let status = render_in_parts(
        &plan,
        &assets,
        &out,
        &lossless,
        &mut |_| {},
        &|| started.elapsed() > BOUND,
        &std::cell::Cell::new(false),
    )
    .unwrap();
    assert_eq!(status, RenderStatus::Completed, "no end after {}s", BOUND.as_secs());
    eprintln!(
        "{runs} runs of ten clips on {runs} tracks: {} parts, {frames} frames in {:.1} s",
        plan.parts.len(),
        started.elapsed().as_secs_f64()
    );
    let got = file_md5s(&out, &dir, "runs");
    let (mut want, _) = export_md5s(&timeline(&[]), &assets, &dir, "runs-base", BOUND);
    for r in 0..runs {
        let (alone, _) = export_md5s(&timeline(&[r]), &assets, &dir, &format!("runs-{r}"), BOUND);
        for k in r * stretch..(r + 1) * stretch {
            want[k].clone_from(&alone[k]);
        }
    }
    assert_frames(&got, &want, "four runs in parts");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An asset stored before schema v21 has no tick, and importing its file again does not probe it: the export asks the file
/// (`ffprobe`, bounded), builds the graph with the answer, and the project keeps it. The tick is the one the import records,
/// the pictures are the folded export's, and a second look finds nothing to ask.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn an_export_backfills_the_tick_of_an_asset_stored_without_one() {
    use crate::project::Project;
    let dir = scratch("blend-fold-backfill");
    eprintln!("{}", version());
    let case = Case::butted(&dir, 12);
    let probed = case.assets();
    let mut old = probed.clone();
    for asset in &mut old {
        asset.streams[0].time_base = None;
    }
    let tl = case.timeline(&(0..12).collect::<Vec<_>>());
    let project = Project::open_in_memory().unwrap();
    for asset in &old {
        project.insert_asset(asset).unwrap();
    }
    // The export's own opts: the fps and the size this file is built at.
    let wanted = time_base_inputs(&tl, &old, &opts());
    assert_eq!(
        wanted,
        vec![(case.src.id, case.src.path)],
        "the file of the run, and not the base"
    );
    let found = probe_time_bases(&wanted);
    assert_eq!(found.len(), 1);
    assert_eq!(
        Some(found[0].time_base),
        probed[1].streams[0].time_base,
        "the tick the import records"
    );
    assert_eq!(project.apply_time_bases(&found).unwrap(), 1);
    let stored = project.list_assets().unwrap();
    assert_eq!(stored[1].streams[0].time_base, probed[1].streams[0].time_base);
    assert_eq!(
        stored[0].streams[0].time_base, None,
        "the base is in no run: it was not asked"
    );
    assert!(time_base_inputs(&tl, &stored, &opts()).is_empty(), "nothing left to ask");
    // The graph built from the backfilled assets folds; from the stored ones it did not.
    let graph = |assets: &[Asset]| {
        let args = build_export_args(&tl, assets, "unused.mkv", &opts()).unwrap();
        args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone()
    };
    assert!(!graph(&old).contains("bldf"));
    assert!(graph(&with_time_bases(&old, &found)).contains("bldf"));
    // Rendered through the real entry point with the assets as the old project holds them: it asks, folds and writes the same
    // pictures as the export of the probed assets.
    let lossless = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..opts()
    };
    let out = dir.join("backfilled.mkv");
    let started = Instant::now();
    let status = render_with_progress(&tl, &old, &out, &lossless, &mut |_| {}, &|| started.elapsed() > BOUND).unwrap();
    assert_eq!(status, RenderStatus::Completed, "no end after {}s", BOUND.as_secs());
    eprintln!(
        "12 butted clips of an asset with no tick, rendered: {:.1} s",
        started.elapsed().as_secs_f64()
    );
    let got = file_md5s(&out, &dir, "backfilled");
    let (want, _) = export_md5s(&tl, &probed, &dir, "probed", BOUND);
    assert_frames(&got, &want, "the backfilled export against the probed assets'");
    let _ = std::fs::remove_dir_all(&dir);
}
