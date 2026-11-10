//! Independent check of the blend fold, part 2: the real `ffmpeg` (`#[ignore]`d; `KERF_FFMPEG` / `KERF_FFPROBE` pick the build).
//!
//! * `check_exact*`: a folded export is frame for frame the nested graph's (`framemd5` of `[outv]`).
//! * `check_guard*`: the frames each blended clip's layer and alpha reach, read out of the graph itself, against the slots the
//!   guard (`drawn_slots`) claims, and whether two clips the guard folds share a frame.
//! * `check_backstop*`: whole exports on layouts that nest forks, timed, against the whole nested export on 6.1.
//! * `check_big*`: hundreds of clips in one run.
//!
//! Environment: `KERF_CHECK_OUT` (results, montages), `KERF_CHECK_MEDIA` (cached sources), `KERF_CHECK_ONLY` (substring of case
//! names to run), `KERF_CHECK_NESTED_MAX` (most blended clips whose nested graph is run).

#![allow(clippy::print_stderr)]
#![allow(clippy::too_many_arguments)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::fold_dump::Rng;
use super::segments::*;
use super::*;
use crate::engine::test_support::{make_clip, timeline_of, video_track};
use crate::model::{BlendMode, Keyframe, Mask, Transition, TransitionKind, VideoEffect};
use crate::project::Project;

const BOUND: Duration = Duration::from_secs(240);

// ---- environment ----------------------------------------------------------------------------------------------

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("KERF_CHECK_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kerf-fold-check-out"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn media_dir() -> PathBuf {
    let dir = std::env::var_os("KERF_CHECK_MEDIA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kerf-fold-check-media"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn only(name: &str) -> bool {
    std::env::var("KERF_CHECK_ONLY").map_or(true, |f| f.split(',').any(|f| name.contains(f)))
}

fn ffmpeg_major() -> u32 {
    let out = command(&ffmpeg_bin()).arg("-version").output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim_start_matches("ffmpeg version ")
        .trim_start_matches('n')
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or(0)
}

fn nested_max() -> usize {
    std::env::var("KERF_CHECK_NESTED_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if ffmpeg_major() >= 9 { 8 } else { 14 })
}

fn version_line() -> String {
    let out = command(&ffmpeg_bin()).arg("-version").output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").to_string()
}

// ---- media ----------------------------------------------------------------------------------------------------

/// The ffmpeg that makes the sources (`ffmpeg` on the `PATH`; `KERF_CHECK_GEN_FFMPEG`), the same for both builds under test.
fn gen_bin() -> String {
    std::env::var("KERF_CHECK_GEN_FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

fn run_bounded(cmd: &mut std::process::Command, limit: Duration, what: &str) -> std::result::Result<String, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{what}: spawn: {e}"))?;
    let started = Instant::now();
    let mut stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
        text
    });
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let text = reader.join().unwrap_or_default();
            return if status.success() {
                Ok(text)
            } else {
                Err(format!("{what}: {status}\n{}", tail(&text)))
            };
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(format!("{what}: no end after {} s; killed", limit.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(12)..].join("\n")
}

/// A made source: `size`, `fps` (the text lavfi is given), `secs`, in `ext` encoded by `enc`, after the lavfi `filter`.
fn media(name: &str, source: &str, size: (u32, u32), fps: &str, secs: u32, filter: &str, ext: &str, enc: &[&str]) -> Asset {
    let path = media_dir().join(format!("{name}.{ext}"));
    if !path.exists() {
        let graph = if filter.is_empty() {
            format!("{source}=s={}x{}:r={fps}:d={secs}", size.0, size.1)
        } else {
            format!("{source}=s={}x{}:r={fps}:d={secs},{filter}", size.0, size.1)
        };
        let tmp = media_dir().join(format!("tmp-{}-{name}.{ext}", std::process::id()));
        let mut cmd = std::process::Command::new(gen_bin());
        cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &graph])
            .args(enc)
            .arg(&tmp);
        run_bounded(&mut cmd, Duration::from_secs(300), &format!("make {name}")).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
    }
    Project::probe_asset(&path).unwrap_or_else(|e| panic!("probe {name}: {e}"))
}

const LOSSLESS: [&str; 6] = ["-c:v", "libx264", "-crf", "0", "-g", "1"];

fn plain(name: &str, source: &str, fps: &str, ext: &str) -> Asset {
    let mut enc = LOSSLESS.to_vec();
    enc.extend(["-pix_fmt", "yuv420p"]);
    match ext {
        "mov600" => {
            enc.extend(["-video_track_timescale", "600"]);
            media(name, source, (192, 108), fps, 8, "format=yuv420p", "mov", &enc)
        }
        "avi" => media(
            name,
            source,
            (192, 108),
            fps,
            8,
            "format=yuv420p",
            "avi",
            &["-c:v", "ffv1", "-pix_fmt", "yuv420p"],
        ),
        _ => media(name, source, (192, 108), fps, 8, "format=yuv420p", ext, &enc),
    }
}

// ---- timelines ------------------------------------------------------------------------------------------------

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

pub(super) struct Build<'a> {
    pub fps: f64,
    pub canvas: (u32, u32),
    pub base: &'a Asset,
    pub src: &'a Asset,
}

impl Build<'_> {
    /// A blended clip drawn from frame position `from` to `to` of the canvas (fractions allowed), reading the source from
    /// `0.3 + 0.07 * i`.
    pub fn clip(&self, i: usize, from: f64, to: f64) -> Clip {
        let len = (to - from) / self.fps;
        let sin = 0.3 + 0.07 * (i % 20) as f64;
        let mut clip = make_clip(self.src.id, sin, sin + len, from / self.fps);
        clip.blend_mode = MODES[i % MODES.len()];
        clip.transform.scale = 0.8;
        clip.transform.pos_x = 0.05;
        clip.transform.opacity = 0.7;
        clip
    }

    /// Clips butted on `bounds` (frame positions), each tweaked by `tweak(i, clip)`.
    pub fn run(&self, bounds: &[f64], mut tweak: impl FnMut(usize, &mut Clip)) -> Vec<Clip> {
        bounds
            .windows(2)
            .enumerate()
            .map(|(i, w)| {
                let mut c = self.clip(i, w[0], w[1]);
                tweak(i, &mut c);
                c
            })
            .collect()
    }

    pub fn timeline(&self, tracks: Vec<Vec<Clip>>, frames: f64) -> Timeline {
        let mut all = vec![video_track(vec![make_clip(self.base.id, 0.0, frames / self.fps, 0.0)])];
        all.extend(tracks.into_iter().map(video_track));
        timeline_of(all)
    }

    /// The options of a lossless intermediate (what the planner's parts and the app's checks write), so that the graph
    /// built for `framemd5` is the one the encoder is given.
    pub fn opts(&self) -> ExportOptions {
        ExportOptions {
            container: Container::Mkv,
            video_codec: Some("libx264".into()),
            rate_control: RateControl::Lossless,
            fps: Some(self.fps),
            resolution: Some(self.canvas),
            ..ExportOptions::default()
        }
    }
}

/// `n` clips of `each` frames from frame `at`.
fn even(n: usize, at: f64, each: f64) -> Vec<f64> {
    (0..=n).map(|i| at + i as f64 * each).collect()
}

fn export_args(tl: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Vec<String> {
    let mut args = build_export_args(tl, assets, "unused.mkv", opts).expect("build the export argv");
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args.truncate(graph + 1);
    args
}

fn spill(args: &mut [String], tag: &str) -> GraphScript {
    externalize_filter_complex(args, tag).unwrap()
}

fn graph_of_args(args: &[String]) -> &str {
    let i = args.iter().position(|a| a == "-filter_complex").unwrap();
    &args[i + 1]
}

fn parse_md5(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split(", ").collect();
            format!("{}:{}", f[2].trim(), f[f.len() - 1].trim())
        })
        .collect()
}

/// The `framemd5` (`pts:md5`) of every frame of the export graph's `[outv]`, the lossless picture too when `keep` is named, and
/// the seconds it took. `fold` pins [`fold_min`] while the argv is built (`usize::MAX`: nested).
fn export_md5s(
    tl: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    fold: Option<usize>,
    tag: &str,
    limit: Duration,
) -> std::result::Result<(Vec<String>, Duration), String> {
    let mut args = match fold {
        Some(min) => with_fold_min(min, || export_args(tl, assets, opts)),
        None => export_args(tl, assets, opts),
    };
    let out = out_dir().join(format!("{tag}.md5"));
    args.extend(["-map", "[outv]", "-fps_mode", "passthrough", "-f", "framemd5", "-y"].map(String::from));
    args.push(out.to_string_lossy().into_owned());
    let _script = spill(&mut args, tag);
    let started = Instant::now();
    let mut cmd = command(&ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostats"]).args(&args);
    run_bounded(&mut cmd, limit, tag)?;
    let took = started.elapsed();
    let md5s = parse_md5(&out);
    let _ = std::fs::remove_file(&out);
    Ok((md5s, took))
}

/// The lossless picture of the export graph's `[outv]`, for a montage.
fn export_video(tl: &Timeline, assets: &[Asset], opts: &ExportOptions, fold: Option<usize>, path: &Path) {
    let mut args = match fold {
        Some(min) => with_fold_min(min, || export_args(tl, assets, opts)),
        None => export_args(tl, assets, opts),
    };
    args.extend(["-map", "[outv]", "-fps_mode", "passthrough", "-c:v", "ffv1", "-y"].map(String::from));
    args.push(path.to_string_lossy().into_owned());
    let _script = spill(&mut args, "video");
    let mut cmd = command(&ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostats"]).args(&args);
    let _ = run_bounded(&mut cmd, BOUND, "export video");
}

fn clips_of(tl: &Timeline) -> Vec<(usize, usize, Clip)> {
    let mut flat = 0;
    let mut out = Vec::new();
    for (ti, track) in tl.tracks.iter().enumerate() {
        for clip in &track.clips {
            out.push((flat, ti, clip.clone()));
            flat += 1;
        }
    }
    out
}

fn folds_of(tl: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Vec<Vec<usize>> {
    let cut = prepared_cut(tl, opts);
    let fx = transition_fx(&cut, assets);
    let fmt = export_format(&cut, assets, opts);
    blend_folds(&cut, assets, &fx, fmt.fps, &|_| true)
}

fn blended_count(tl: &Timeline) -> usize {
    tl.tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.blend_mode.ffmpeg().is_some())
        .count()
}

// ---- 1. exactness ---------------------------------------------------------------------------------------------

struct Case {
    name: String,
    tl: Timeline,
    assets: Vec<Asset>,
    opts: ExportOptions,
}

#[derive(Debug, Default)]
struct Outcome {
    runs: Vec<usize>,
    frames: usize,
    differing: Vec<usize>,
    skipped: Option<String>,
    error: Option<String>,
    fold_s: f64,
    nested_s: f64,
}

impl Outcome {
    fn failed(&self) -> bool {
        !self.differing.is_empty() || self.error.is_some()
    }
}

fn montage(nested: &Path, folded: &Path, frame: usize, out: &Path) {
    let select = format!("select=eq(n\\,{frame}),setpts=PTS-STARTPTS");
    let graph = format!(
        "[0:v]{select},split=2[a1][a2];[1:v]{select},split=2[b1][b2];[a1][b1]hstack[ab];[a2][b2]blend=all_mode=difference,eq=brightness=0.2:contrast=6[d];[ab][d]hstack=2[o]"
    );
    let mut cmd = std::process::Command::new(gen_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(nested)
        .arg("-i")
        .arg(folded)
        .args(["-filter_complex", &graph, "-map", "[o]", "-frames:v", "1"])
        .arg(out);
    let _ = run_bounded(&mut cmd, Duration::from_secs(60), "montage");
}

fn check_exact(case: &Case) -> Outcome {
    let mut out = Outcome::default();
    let runs = folds_of(&case.tl, &case.assets, &case.opts);
    out.runs = runs.iter().map(Vec::len).collect();
    let tag = case.name.replace([' ', '/'], "_");
    let a = out_dir().join(format!("{tag}.nested.mkv"));
    let b = out_dir().join(format!("{tag}.folded.mkv"));
    let (folded, fold_took) = match export_md5s(&case.tl, &case.assets, &case.opts, None, &format!("{tag}-f"), BOUND) {
        Ok(v) => v,
        Err(e) => {
            out.error = Some(e);
            return out;
        }
    };
    out.frames = folded.len();
    out.fold_s = fold_took.as_secs_f64();
    if runs.is_empty() {
        out.skipped = Some("no run folds (graph is the nested one)".into());
        let nested_graph =
            graph_of_args(&with_fold_min(usize::MAX, || export_args(&case.tl, &case.assets, &case.opts))).to_string();
        let graph = graph_of_args(&export_args(&case.tl, &case.assets, &case.opts)).to_string();
        if nested_graph != graph {
            out.error = Some("no run folds but the graph differs from the nested one".into());
        }
        return out;
    }
    if blended_count(&case.tl) > nested_max() {
        out.skipped = Some(format!(
            "{} blended clips: nested not run on this build",
            blended_count(&case.tl)
        ));
        return out;
    }
    let (nested, nested_took) = match export_md5s(
        &case.tl,
        &case.assets,
        &case.opts,
        Some(usize::MAX),
        &format!("{tag}-n"),
        BOUND,
    ) {
        Ok(v) => v,
        Err(e) => {
            out.error = Some(e);
            return out;
        }
    };
    out.nested_s = nested_took.as_secs_f64();
    if nested.len() != folded.len() {
        out.error = Some(format!("{} nested frames, {} folded", nested.len(), folded.len()));
        return out;
    }
    out.differing = (0..nested.len()).filter(|&k| nested[k] != folded[k]).collect();
    if let Some(&first) = out.differing.first() {
        let png = out_dir().join(format!("{tag}.frame{first}.png"));
        export_video(&case.tl, &case.assets, &case.opts, Some(usize::MAX), &a);
        export_video(&case.tl, &case.assets, &case.opts, None, &b);
        montage(&a, &b, first, &png);
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }
    out
}

fn report(cases: &[Case], results: &mut Vec<(String, Outcome)>) {
    for case in cases {
        if !only(&case.name) {
            continue;
        }
        let o = check_exact(case);
        eprintln!(
            "{:<46} runs {:?} frames {:>3} differing {:>3}{}{}  fold {:.1}s nested {:.1}s{}",
            case.name,
            o.runs,
            o.frames,
            o.differing.len(),
            o.differing.first().map(|f| format!(" (from {f})")).unwrap_or_default(),
            o.error
                .as_ref()
                .map(|e| format!("  ERROR {}", e.lines().next().unwrap_or("")))
                .unwrap_or_default(),
            o.fold_s,
            o.nested_s,
            o.skipped.as_ref().map(|s| format!("  [{s}]")).unwrap_or_default()
        );
        results.push((case.name.clone(), o));
    }
}

fn finish(results: &[(String, Outcome)]) {
    let failed: Vec<String> = results
        .iter()
        .filter(|(_, o)| o.failed())
        .map(|(n, o)| {
            format!(
                "{n}: {} frames differ (from {:?}) {:?}",
                o.differing.len(),
                o.differing.first(),
                o.error
            )
        })
        .collect();
    let folded = results
        .iter()
        .filter(|(_, o)| !o.runs.is_empty() && o.skipped.is_none())
        .count();
    eprintln!(
        "{} cases, {} compared folded-vs-nested, {} with no fold, {} failed",
        results.len(),
        folded,
        results.iter().filter(|(_, o)| o.runs.is_empty()).count(),
        failed.len()
    );
    assert!(failed.is_empty(), "FAIL:\n{}", failed.join("\n"));
}

struct Fixtures {
    mp4_30: Asset,
    base30: Asset,
}

fn fixtures() -> Fixtures {
    Fixtures {
        mp4_30: plain("mp4_30", "testsrc2", "30", "mp4"),
        base30: plain("base_30", "testsrc", "30", "mp4"),
    }
}

fn simple(name: &str, fps: f64, base: &Asset, src: &Asset, tracks: Vec<Vec<Clip>>, frames: f64) -> Case {
    let b = Build {
        fps,
        canvas: (192, 108),
        base,
        src,
    };
    Case {
        name: name.to_string(),
        tl: b.timeline(tracks, frames),
        assets: vec![base.clone(), src.clone()],
        opts: b.opts(),
    }
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn check_exact_modes_opacity_fades() {
    eprintln!("{}", version_line());
    let fx = fixtures();
    let mut cases = Vec::new();
    let mk = |fps: f64| Build {
        fps,
        canvas: (192, 108),
        base: &fx.base30,
        src: &fx.mp4_30,
    };
    let b = mk(30.0);
    let n = 8;
    for (m, mode) in MODES.iter().enumerate() {
        let clips = b.run(&even(n, 0.0, 2.0), |_, c| c.blend_mode = *mode);
        cases.push(simple(
            &format!("mode-{m}-{mode:?}"),
            30.0,
            &fx.base30,
            &fx.mp4_30,
            vec![clips],
            20.0,
        ));
    }
    cases.push(simple(
        "modes-rotating",
        30.0,
        &fx.base30,
        &fx.mp4_30,
        vec![b.run(&even(n, 0.0, 2.0), |_, _| {})],
        20.0,
    ));
    for (label, opacity) in [("opaque", 1.0), ("half", 0.5), ("faint", 0.02)] {
        let clips = b.run(&even(n, 0.0, 3.0), |_, c| c.transform.opacity = opacity);
        cases.push(simple(
            &format!("opacity-{label}"),
            30.0,
            &fx.base30,
            &fx.mp4_30,
            vec![clips],
            28.0,
        ));
    }
    let keyed = b.run(&even(n, 0.0, 4.0), |i, c| {
        let d = c.duration();
        c.keyframes = vec![
            Keyframe {
                time: 0.0,
                scale: 0.8,
                pos_x: 0.05,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 0.1,
                easing: Default::default(),
            },
            Keyframe {
                time: d,
                scale: 0.8,
                pos_x: 0.05 + 0.01 * i as f64,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
                easing: Default::default(),
            },
        ];
    });
    cases.push(simple("opacity-keyed", 30.0, &fx.base30, &fx.mp4_30, vec![keyed], 36.0));
    let faded = b.run(&even(n, 0.0, 4.0), |_, c| {
        c.fade_in = 2.0 / 30.0;
        c.fade_out = 2.0 / 30.0;
    });
    cases.push(simple("fades", 30.0, &fx.base30, &fx.mp4_30, vec![faded], 36.0));
    let mut results = Vec::new();
    report(&cases, &mut results);
    finish(&results);
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn check_exact_transitions_speed_masks() {
    eprintln!("{}", version_line());
    let fx = fixtures();
    let b = Build {
        fps: 30.0,
        canvas: (192, 108),
        base: &fx.base30,
        src: &fx.mp4_30,
    };
    let mut cases = Vec::new();
    let plain_clip = |from: f64, to: f64| {
        let mut c = make_clip(fx.mp4_30.id, 4.0, 4.0 + (to - from) / 30.0, from / 30.0);
        c.transform.scale = 0.9;
        c
    };
    let xfade = |d: f64, kind: TransitionKind| Transition {
        kind,
        duration: d / 30.0,
    };
    // A plain clip, then a run whose first clip dissolves in over its tail, and a plain clip dissolving in over the last's tail.
    for (label, kind) in [
        ("crossfade", TransitionKind::Crossfade),
        ("dip", TransitionKind::DipToBlack),
        ("slide", TransitionKind::SlideLeft),
        ("push", TransitionKind::PushLeft),
    ] {
        let mut run = b.run(&even(8, 10.0, 3.0), |_, _| {});
        run[0].transition_in = Some(xfade(4.0, kind));
        let mut after = plain_clip(34.0, 44.0);
        after.transition_in = Some(xfade(4.0, kind));
        let mut track = vec![plain_clip(0.0, 10.0)];
        track.extend(run.clone());
        track.push(after);
        cases.push(simple(
            &format!("transition-edges-{label}"),
            30.0,
            &fx.base30,
            &fx.mp4_30,
            vec![track],
            50.0,
        ));
        // Mid-run: the dissolve overlaps the clip before it, so the guard cuts the run there.
        let mut mid = b.run(&even(9, 0.0, 3.0), |_, _| {});
        mid[4].transition_in = Some(xfade(2.0, kind));
        cases.push(simple(
            &format!("transition-mid-{label}"),
            30.0,
            &fx.base30,
            &fx.mp4_30,
            vec![mid],
            30.0,
        ));
    }
    for (label, speed) in [
        ("2x", 2.0),
        ("half", 0.5),
        ("1.5x", 1.5),
        ("quarter", 0.25),
        ("rev", -1.0),
        ("rev2x", -2.0),
    ] {
        let mut at = 0.0;
        let clips: Vec<Clip> = (0..8)
            .map(|i| {
                let frames = 3.0;
                let mut c = b.clip(i, at, at + frames);
                if i % 2 == 1 {
                    c.speed = speed;
                    c.source_out = c.source_in + speed.abs() * frames / 30.0;
                }
                at += frames;
                c
            })
            .collect();
        cases.push(simple(
            &format!("speed-{label}"),
            30.0,
            &fx.base30,
            &fx.mp4_30,
            vec![clips],
            26.0,
        ));
    }
    let masked = b.run(&even(8, 0.0, 3.0), |i, c| {
        c.mask = Some(Mask {
            inverted: i % 2 == 0,
            ..Mask::default()
        });
    });
    cases.push(simple("mask", 30.0, &fx.base30, &fx.mp4_30, vec![masked], 26.0));
    for (label, effect) in [
        ("blur", VideoEffect::Blur { sigma: 3.0 }),
        ("sharpen", VideoEffect::Sharpen { amount: 1.0 }),
        ("gray", VideoEffect::Grayscale),
        ("invert", VideoEffect::Invert),
        ("vignette", VideoEffect::Vignette),
        (
            "chroma",
            VideoEffect::ChromaKey {
                color: "0x00ff00".into(),
                similarity: 0.3,
                blend: 0.1,
            },
        ),
    ] {
        let clips = b.run(&even(8, 0.0, 3.0), |_, c| c.effects.push(effect.clone()));
        cases.push(simple(
            &format!("effect-{label}"),
            30.0,
            &fx.base30,
            &fx.mp4_30,
            vec![clips],
            26.0,
        ));
    }
    let rotated = b.run(&even(8, 0.0, 3.0), |i, c| {
        c.transform.rotation = 7.0 * i as f64;
        c.transform.crop_left = 0.1;
    });
    cases.push(simple("rotate-crop", 30.0, &fx.base30, &fx.mp4_30, vec![rotated], 26.0));
    let moving = b.run(&even(8, 0.0, 4.0), |i, c| {
        let d = c.duration();
        c.keyframes = vec![
            Keyframe {
                time: 0.0,
                scale: 0.5,
                pos_x: -0.2,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 0.9,
                easing: Default::default(),
            },
            Keyframe {
                time: d,
                scale: 0.9,
                pos_x: 0.2,
                pos_y: 0.1 * i as f64 / 8.0,
                rotation: 0.0,
                opacity: 0.9,
                easing: Default::default(),
            },
        ];
    });
    cases.push(simple("moving-zoom", 30.0, &fx.base30, &fx.mp4_30, vec![moving], 34.0));
    let mut results = Vec::new();
    report(&cases, &mut results);
    finish(&results);
}

/// Cuts a hair either side of a frame, in the half-frame band and past it, on a source of each container, at each rate: eight clips
/// butted with every cut at one offset (the guard folds a run where the offset leaves the cuts disjoint, and the fold is then held to
/// the nested graph, frame for frame).
#[test]
#[ignore = "needs the ffmpeg binary"]
fn check_exact_cut_positions() {
    eprintln!("{}", version_line());
    let base = plain("base_30", "testsrc", "30", "mp4");
    let fracs: [(&str, f64); 14] = [
        ("on", 0.0),
        ("p1e-9", 1e-9),
        ("m1e-9", -1e-9),
        ("p.001", 0.001),
        ("m.001", -0.001),
        ("h-1e-9", 0.5 - 1e-9),
        ("h", 0.5),
        ("h+1e-9", 0.5 + 1e-9),
        ("p.549", 0.549),
        ("p.55", 0.55),
        ("p.5501", 0.5501),
        ("p.56", 0.56),
        ("p.9", 0.9),
        ("p.9999", 0.9999),
    ];
    let rates: [(&str, f64, &str); 7] = [
        ("24", 24.0, "24"),
        ("25", 25.0, "25"),
        ("30", 30.0, "30"),
        ("23.976", 24000.0 / 1001.0, "24000/1001"),
        ("29.97", 30000.0 / 1001.0, "30000/1001"),
        ("59.94", 60000.0 / 1001.0, "60000/1001"),
        ("50", 50.0, "50"),
    ];
    let exts: Vec<&str> = std::env::var("KERF_CHECK_EXTS")
        .unwrap_or_else(|_| "mkv,mp4".into())
        .leak()
        .split(',')
        .collect();
    let mut results = Vec::new();
    for (rname, fps, lavfi) in rates {
        for ext in exts.iter().copied() {
            let src = plain(&format!("cut_{rname}_{ext}"), "testsrc2", lavfi, ext);
            let b = Build {
                fps,
                canvas: (192, 108),
                base: &base,
                src: &src,
            };
            for (label, f) in fracs {
                let bounds: Vec<f64> = (0..=8).map(|i| if i == 0 { 0.0 } else { 3.0 * i as f64 + f }).collect();
                let clips = b.run(&bounds, |_, _| {});
                let case = simple(&format!("cuts-{rname}-{ext}-{label}"), fps, &base, &src, vec![clips], 28.0);
                report(&[case], &mut results);
            }
        }
    }
    finish(&results);
}

// ---- 2. the guard ---------------------------------------------------------------------------------------------

/// The frames (slots of the canvas' `fps`) each blended clip's layer reaches, and where its placed alpha is non-zero, read out of
/// the export's own graph: everything but the clips' chains up to `alphaextract` is cut out, and the layer and the alpha taken
/// with `showinfo` / `signalstats`.
struct Taps {
    layer: BTreeMap<usize, BTreeSet<i64>>,
    alpha: BTreeMap<usize, BTreeSet<i64>>,
}

fn pads_of(stmt: &str) -> (Vec<String>, Vec<String>) {
    // Leading `[a][b]` are inputs, trailing `[c][d]` outputs.
    let mut ins = Vec::new();
    let mut rest = stmt;
    while rest.starts_with('[') {
        let end = rest.find(']').unwrap();
        ins.push(rest[1..end].to_string());
        rest = &rest[end + 1..];
    }
    let mut outs = Vec::new();
    let mut rest = stmt;
    while rest.ends_with(']') {
        let start = rest.rfind('[').unwrap();
        outs.push(rest[start + 1..rest.len() - 1].to_string());
        rest = &rest[..start];
    }
    outs.reverse();
    (ins, outs)
}

fn reduced_graph(graph: &str, flats: &[usize]) -> String {
    let mut stmts: Vec<String> = graph.split(';').map(String::from).collect();
    let is_fork = |pads: &[String]| pads.iter().any(|p| p.starts_with("bld") && p.ends_with('w'));
    // The composite forks are replaced by one fork of the canvas: a clip's alpha does not depend on what it is laid on.
    stmts.retain(|s| !is_fork(&pads_of(s).1));
    let arms: String = flats.iter().map(|f| format!("[bld{f}w]")).collect();
    stmts.push(format!("[vbase]split={}{arms}", flats.len().max(1)));
    let io: Vec<(Vec<String>, Vec<String>)> = stmts.iter().map(|s| pads_of(s)).collect();
    let mut producer: HashMap<String, usize> = HashMap::new();
    for (i, (_, outs)) in io.iter().enumerate() {
        for o in outs {
            producer.insert(o.clone(), i);
        }
    }
    let mut need: Vec<String> = Vec::new();
    for f in flats {
        need.push(format!("bld{f}lc"));
        need.push(format!("bld{f}aa"));
    }
    let mut keep: BTreeSet<usize> = BTreeSet::new();
    while let Some(pad) = need.pop() {
        let Some(&i) = producer.get(&pad) else { continue };
        if keep.insert(i) {
            need.extend(io[i].0.iter().cloned());
        }
    }
    let consumed: BTreeSet<&String> = keep.iter().flat_map(|&i| io[i].0.iter()).collect();
    let mut out: Vec<String> = Vec::new();
    let mut taps: Vec<String> = Vec::new();
    for &i in &keep {
        out.push(stmts[i].clone());
        for o in &io[i].1 {
            if consumed.contains(o) {
                continue;
            }
            if let Some(f) = flats.iter().find(|f| o == &format!("bld{f}lc")) {
                taps.push(format!("[{o}]showinfo@p{f}[tp{f}]"));
            } else if let Some(f) = flats.iter().find(|f| o == &format!("bld{f}aa")) {
                taps.push(format!(
                    "[{o}]signalstats@s{f},metadata@m{f}=mode=print:key=lavfi.signalstats.YMAX[ta{f}]"
                ));
            } else {
                taps.push(format!("[{o}]nullsink"));
            }
        }
    }
    out.extend(taps);
    out.join(";")
}

fn read_taps(log: &str, fps: f64) -> Taps {
    let mut taps = Taps {
        layer: BTreeMap::new(),
        alpha: BTreeMap::new(),
    };
    let slot = |t: f64| (t * fps).round() as i64;
    let mut pending: HashMap<usize, i64> = HashMap::new();
    for line in log.lines() {
        if let Some(rest) = line.strip_prefix("[showinfo@p") {
            let f: usize = rest.split(' ').next().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
            if let Some(t) = line
                .split("pts_time:")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse::<f64>().ok())
            {
                taps.layer.entry(f).or_default().insert(slot(t));
            }
        } else if let Some(rest) = line.strip_prefix("[metadata@m") {
            let f: usize = rest.split(' ').next().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
            if let Some(t) = line
                .split("pts_time:")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse::<f64>().ok())
            {
                pending.insert(f, slot(t));
            } else if let Some(v) = line.split("YMAX=").nth(1).and_then(|s| s.trim().parse::<f64>().ok()) {
                if let Some(s) = pending.get(&f) {
                    taps.alpha.entry(f).or_default();
                    if v > 0.0 {
                        taps.alpha.get_mut(&f).unwrap().insert(*s);
                    }
                }
            }
        }
    }
    taps
}

fn rng_pick<T: Copy>(rng: &mut Rng, of: &[T]) -> T {
    rng.pick(of)
}

/// What the guard says and what the graph does for one timeline.
struct Guard {
    name: String,
    slots: BTreeMap<usize, (i64, i64)>,
    folded: Vec<Vec<usize>>,
    taps: Taps,
}

fn guard_case(case: &Case) -> std::result::Result<Guard, String> {
    let cut = prepared_cut(&case.tl, &case.opts);
    let fx = transition_fx(&cut, &case.assets);
    let fmt = export_format(&cut, &case.assets, &case.opts);
    let rate = Rational::from_fps(fmt.fps).unwrap();
    let all = clips_of(&cut);
    let mut slots = BTreeMap::new();
    for (flat, _, clip) in &all {
        if clip.blend_mode.ffmpeg().is_some() && !clip.is_generated() {
            let asset = case.assets.iter().find(|a| a.id == clip.asset_id);
            if asset.is_some_and(|a| a.is_image()) {
                continue;
            }
            slots.insert(*flat, ClipTiming::new(clip, &fx[*flat]).drawn_slots(rate));
        }
    }
    let folded = folds_of(&case.tl, &case.assets, &case.opts);
    let args = export_args(&case.tl, &case.assets, &case.opts);
    let flats: Vec<usize> = slots.keys().copied().collect();
    let graph = reduced_graph(graph_of_args(&args), &flats);
    let mut a = args;
    let i = a.iter().position(|x| x == "-filter_complex").unwrap();
    a[i + 1] = graph;
    for f in &flats {
        a.extend(["-map".to_string(), format!("[tp{f}]")]);
        a.extend(["-map".to_string(), format!("[ta{f}]")]);
    }
    a.extend(["-f", "null", "-"].map(String::from));
    let tag = format!("tap-{}", case.name.replace([' ', '/'], "_"));
    let _script = spill(&mut a, &tag);
    let mut cmd = command(&ffmpeg_bin());
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "info"]).args(&a);
    let log = run_bounded(&mut cmd, BOUND, &tag)?;
    Ok(Guard {
        name: case.name.clone(),
        slots,
        folded,
        taps: read_taps(&log, fmt.fps),
    })
}

/// `(violations of the claim, collisions of folded clips)` for one case.
fn judge(g: &Guard) -> (Vec<String>, Vec<String>) {
    let mut violations = Vec::new();
    let mut collisions = Vec::new();
    for (flat, (first, end)) in &g.slots {
        let outside = |set: Option<&BTreeSet<i64>>| -> Vec<i64> {
            set.map(|s| s.iter().copied().filter(|k| k < first || k >= end).collect())
                .unwrap_or_default()
        };
        let layer = outside(g.taps.layer.get(flat));
        let alpha = outside(g.taps.alpha.get(flat));
        if !layer.is_empty() || !alpha.is_empty() {
            violations.push(format!(
                "{}: clip {flat}: guard {first}..{end}, layer {:?}, alpha {:?}; outside layer {layer:?} alpha {alpha:?}",
                g.name,
                g.taps.layer.get(flat).map(|s| (s.first().copied(), s.last().copied())),
                g.taps.alpha.get(flat).map(|s| (s.first().copied(), s.last().copied())),
            ));
        }
    }
    for run in &g.folded {
        for (i, a) in run.iter().enumerate() {
            for b in &run[i + 1..] {
                let (Some(sa), Some(sb)) = (g.taps.alpha.get(a), g.taps.alpha.get(b)) else {
                    continue;
                };
                let shared: Vec<i64> = sa.intersection(sb).copied().collect();
                if !shared.is_empty() {
                    collisions.push(format!("{}: folded clips {a} and {b} both draw frames {shared:?}", g.name));
                }
                let (la, lb) = (g.taps.layer.get(a), g.taps.layer.get(b));
                if let (Some(la), Some(lb)) = (la, lb) {
                    let shared: Vec<i64> = la.intersection(lb).copied().collect();
                    if !shared.is_empty() && sa.intersection(sb).next().is_none() {
                        collisions.push(format!(
                            "{}: folded clips {a} and {b} layers share frames {shared:?} (alpha does not)",
                            g.name
                        ));
                    }
                }
            }
        }
    }
    (violations, collisions)
}

/// Opaque butted clips with cuts on every offset from a frame, on each rate and container: where does a clip land?
#[test]
#[ignore = "needs the ffmpeg binary"]
fn check_guard_sweep() {
    eprintln!("{}", version_line());
    let base = plain("base_30", "testsrc", "30", "mp4");
    let seeds: u64 = std::env::var("KERF_CHECK_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let rates: [(&str, f64, &str); 10] = [
        ("24", 24.0, "24"),
        ("25", 25.0, "25"),
        ("30", 30.0, "30"),
        ("23.976", 24000.0 / 1001.0, "24000/1001"),
        ("29.97", 30000.0 / 1001.0, "30000/1001"),
        ("59.94", 60000.0 / 1001.0, "60000/1001"),
        ("50", 50.0, "50"),
        ("60", 60.0, "60"),
        ("120", 120.0, "120"),
        ("100", 100.0, "100"),
    ];
    let exts: Vec<&str> = std::env::var("KERF_CHECK_EXTS")
        .unwrap_or_else(|_| "mkv,mp4,mov600,avi".into())
        .leak()
        .split(',')
        .collect();
    let (mut violations, mut collisions, mut cases) = (Vec::new(), Vec::new(), 0);
    let mut rng = Rng(0xfeed_beef_1234_5678);
    for (rname, fps, lavfi) in rates {
        for ext in exts.iter().copied() {
            let src = plain(&format!("cut_{rname}_{ext}"), "testsrc2", lavfi, ext);
            for seed in 0..seeds {
                let n = 7 + rng.next(2) as usize;
                let mut bounds = vec![rng.next(3) as f64];
                for _ in 0..n {
                    let last = *bounds.last().unwrap();
                    let frac = match rng.next(6) {
                        0 => 0.0,
                        1 => 0.5 + 0.05 * (rng.next(40) as f64 / 40.0) + 1e-3 * rng.next(6) as f64,
                        2 => 0.55 + rng.next(120) as f64 / 1000.0,
                        3 => rng_pick(&mut rng, &[-1e-9, 1e-9, 0.001, 0.999, 0.9999, 0.5 - 1e-9, 0.5 + 1e-9]),
                        _ => rng.next(1000) as f64 / 1000.0,
                    };
                    let len = 2.0 + rng.next(4) as f64;
                    bounds.push((last + len).floor() + frac);
                }
                let b = Build {
                    fps,
                    canvas: (192, 108),
                    base: &base,
                    src: &src,
                };
                let features = std::env::var_os("KERF_CHECK_FEATURES").is_some();
                let clips = b.run(&bounds, |i, c| {
                    c.transform.opacity = 1.0;
                    c.transform.scale = 1.0;
                    c.transform.pos_x = 0.0;
                    if features {
                        match (seed + i as u64) % 9 {
                            0 => {
                                let d = c.duration();
                                c.speed = 2.0;
                                c.source_out = c.source_in + 2.0 * d;
                            }
                            1 => {
                                let d = c.duration();
                                c.speed = 0.5;
                                c.source_out = c.source_in + 0.5 * d;
                            }
                            2 => {
                                c.transition_in = Some(Transition {
                                    kind: TransitionKind::Crossfade,
                                    duration: 1.5 / fps,
                                });
                            }
                            3 => {
                                c.transition_in = Some(Transition {
                                    kind: TransitionKind::SlideLeft,
                                    duration: 2.0 / fps,
                                });
                            }
                            4 => {
                                let d = c.duration();
                                c.keyframes = vec![
                                    Keyframe {
                                        time: 0.0,
                                        scale: 1.0,
                                        pos_x: -0.1,
                                        pos_y: 0.0,
                                        rotation: 0.0,
                                        opacity: 1.0,
                                        easing: Default::default(),
                                    },
                                    Keyframe {
                                        time: d,
                                        scale: 1.0,
                                        pos_x: 0.1,
                                        pos_y: 0.0,
                                        rotation: 0.0,
                                        opacity: 1.0,
                                        easing: Default::default(),
                                    },
                                ];
                            }
                            5 => c.fade_in = 1.0 / fps,
                            6 => c.speed = -1.0,
                            _ => {}
                        }
                    }
                });
                let frames = bounds.last().unwrap() + 4.0;
                let case = simple(&format!("g-{rname}-{ext}-{seed}"), fps, &base, &src, vec![clips], frames);
                if !only(&case.name) {
                    continue;
                }
                cases += 1;
                match guard_case(&case) {
                    Ok(g) => {
                        let (v, c) = judge(&g);
                        if !c.is_empty() {
                            let o = check_exact(&case);
                            eprintln!(
                                "COLLISION {}: bounds {:?}: folded-vs-nested {} of {} frames differ (first {:?}) {:?}",
                                g.name,
                                bounds,
                                o.differing.len(),
                                o.frames,
                                o.differing.first(),
                                o.error
                            );
                        }
                        violations.extend(v);
                        collisions.extend(c);
                    }
                    Err(e) => violations.push(format!("{}: {e}", case.name)),
                }
            }
        }
    }
    eprintln!(
        "{cases} timelines; {} guard violations; {} collisions of folded clips",
        violations.len(),
        collisions.len()
    );
    for v in violations.iter().take(40) {
        eprintln!("violation {v}");
    }
    for c in collisions.iter().take(40) {
        eprintln!("COLLISION {c}");
    }
    assert!(
        collisions.is_empty(),
        "{} folded pairs share a frame:\n{}",
        collisions.len(),
        collisions.join("\n")
    );
}

// ---- 4. byte identity -----------------------------------------------------------------------------------------

/// For the diff against the base commit's argv (`fold_dump`): per seed, the runs the head folds, the argv with folding
/// pinned off, and the planner's parts with the blend limit lifted and as shipped.
#[test]
#[ignore = "writes $KERF_CLASS_OUT"]
fn check_class_dump() {
    let Some(out) = std::env::var_os("KERF_CLASS_OUT") else {
        return;
    };
    let seeds: u64 = std::env::var("KERF_DUMP_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let lifted = SegmentLimits {
        blends: usize::MAX,
        ..SEGMENT_LIMITS
    };
    let mut lines = String::new();
    for seed in 0..seeds {
        let (tl, assets, opts) = super::fold_dump::seeded(seed, None);
        let runs: Vec<usize> = folds_of(&tl, &assets, &opts).iter().map(Vec::len).collect();
        let nested = with_fold_min(usize::MAX, || build_export_args(&tl, &assets, "out.mkv", &opts).ok());
        let parts = |l: SegmentLimits| plan_export(&tl, &assets, &opts, l).map(|p| format!("{:?}", p.parts));
        let whole_forks = {
            let cut = prepared_cut(&tl, &opts);
            let fmt = export_format(&cut, &assets, &opts);
            let rate = Rational::from_fps(fmt.fps).unwrap();
            let frames = canvas_frames(cut.duration(), rate);
            let fx = transition_fx(&cut, &assets);
            open_cost(picture_clips(&cut, &assets, &fx, &fmt, rate, frames).iter()).2
        };
        lines.push_str(
            &serde_json::to_string(&serde_json::json!({
                "seed": seed, "runs": runs, "args_nested": nested, "parts_lifted": parts(lifted),
                "parts": parts(SEGMENT_LIMITS), "whole_forks": whole_forks, "blended": blended_count(&tl),
            }))
            .unwrap(),
        );
        lines.push('\n');
    }
    std::fs::write(out, lines).unwrap();
}

/// Every pad the fold names is written once and read once, whatever the layout (runs sharing a track or not, parts).
fn assert_pads_unique(graph: &str, what: &str) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for stmt in graph.split(';') {
        let (ins, outs) = pads_of(stmt);
        for p in ins.into_iter().chain(outs) {
            *counts.entry(p).or_default() += 1;
        }
    }
    for (pad, n) in &counts {
        if pad.starts_with("bld") {
            assert_eq!(*n, 2, "{what}: pad [{pad}] appears {n} times");
        }
    }
}

#[test]
fn check_pads_are_unique_in_whole_exports_and_parts() {
    let mut folded = 0;
    for seed in 0..1500u64 {
        let (tl, assets, opts) = super::fold_dump::seeded(seed, None);
        let args = build_export_args(&tl, &assets, "out.mkv", &opts).unwrap();
        assert_pads_unique(graph_of_args(&args), &format!("seed {seed}"));
        if let Some(plan) = plan_export(&tl, &assets, &opts, SEGMENT_LIMITS) {
            for (i, part) in plan.parts.iter().enumerate() {
                let mut a = Vec::new();
                push_part_picture(&plan, &assets, &opts, part, &[], &mut a).unwrap();
                assert_pads_unique(graph_of_args(&a), &format!("seed {seed} part {i}"));
                folded += usize::from(graph_of_args(&a).contains("bldf"));
            }
        }
    }
    eprintln!("{folded} part graphs with a fold");
}

/// Two runs on one track and runs on several, in one graph and in parts: no pad twice.
#[test]
fn check_pads_unique_for_runs_sharing_tracks() {
    let base = super::fold_dump::fixture_assets();
    let (basea, srca) = (&base[0], &base[1]);
    let b = Build {
        fps: 30.0,
        canvas: (640, 360),
        base: basea,
        src: srca,
    };
    let two_on_a_track: Vec<Clip> = {
        let mut v = b.run(&even(9, 0.0, 2.0), |_, _| {});
        let mut between = b.clip(0, 18.0, 20.0);
        between.blend_mode = BlendMode::Normal;
        v.push(between);
        let mut second = b.run(&even(9, 20.0, 2.0), |_, _| {});
        v.append(&mut second);
        v
    };
    let tl = b.timeline(vec![two_on_a_track.clone(), two_on_a_track.clone(), two_on_a_track], 60.0);
    let assets = vec![basea.clone(), srca.clone()];
    let opts = b.opts();
    let runs = folds_of(&tl, &assets, &opts);
    assert_eq!(runs.len(), 6, "{runs:?}");
    let args = build_export_args(&tl, &assets, "out.mkv", &opts).unwrap();
    assert_pads_unique(graph_of_args(&args), "whole");
    let limits = SegmentLimits {
        blends: 3,
        ..SEGMENT_LIMITS
    };
    let plan = plan_export(&tl, &assets, &opts, limits).expect("cut into parts");
    for part in &plan.parts {
        let mut a = Vec::new();
        push_part_picture(&plan, &assets, &opts, part, &[], &mut a).unwrap();
        assert_pads_unique(graph_of_args(&a), &format!("{part:?}"));
    }
}

// ---- 3. the backstop ------------------------------------------------------------------------------------------

struct Layout {
    name: &'static str,
    tl: Timeline,
}

/// `n` blended clips of `len` frames from frame `at`, each starting `step` frames after the last (`step == len`: butted), on
/// cuts that fall `frac` of a frame past the frame (so that neighbours share one).
fn strip(b: &Build, n: usize, at: f64, len: f64, step: f64, frac: f64, first: usize) -> Vec<Clip> {
    (0..n)
        .map(|i| {
            let from = at + i as f64 * step + if i == 0 { 0.0 } else { frac };
            b.clip(first + i, from, at + i as f64 * step + len + frac)
        })
        .collect()
}

fn layouts(b: &Build) -> Vec<Layout> {
    let tl = |tracks: Vec<Vec<Clip>>| b.timeline(tracks, 60.0);
    let long = |i: usize, from: f64, to: f64| b.clip(i, from, to);
    let mut out = vec![
        Layout {
            name: "u14-unaligned",
            tl: tl(vec![strip(b, 14, 0.0, 5.0, 4.0, 0.3, 0)]),
        },
        Layout {
            name: "r2x10",
            tl: tl(vec![strip(b, 10, 0.0, 2.0, 2.0, 0.0, 0), strip(b, 10, 0.0, 2.0, 2.0, 0.0, 3)]),
        },
        Layout {
            name: "r3x10",
            tl: tl((0..3).map(|k| strip(b, 10, 0.0, 6.0, 6.0, 0.0, k)).collect()),
        },
        Layout {
            name: "r4x10",
            tl: tl((0..4).map(|k| strip(b, 10, 0.0, 6.0, 6.0, 0.0, k)).collect()),
        },
        Layout {
            name: "r2x10-l4unaligned",
            tl: tl({
                let mut t: Vec<Vec<Clip>> = (0..2).map(|k| strip(b, 10, 0.0, 6.0, 6.0, 0.0, k)).collect();
                t.extend((0..4).map(|k| vec![long(k, 8.0 + 5.0 * k as f64 + 0.3, 40.0 + 3.0 * k as f64)]));
                t
            }),
        },
        Layout {
            name: "r1x30-l4",
            tl: tl({
                let mut t = vec![strip(b, 30, 0.0, 2.0, 2.0, 0.0, 0)];
                t.extend((0..4).map(|k| vec![long(k + 1, 4.0 * k as f64 + 0.3, 56.0)]));
                t
            }),
        },
        Layout {
            name: "r1x10-l3",
            tl: tl({
                let mut t = vec![strip(b, 10, 0.0, 6.0, 6.0, 0.0, 0)];
                t.extend((0..3).map(|k| vec![long(k + 1, 4.0 * k as f64 + 0.3, 56.0)]));
                t
            }),
        },
        Layout {
            name: "r2x10-l0-b",
            tl: tl(vec![strip(b, 10, 0.0, 6.0, 6.0, 0.0, 0), strip(b, 10, 2.0, 6.0, 6.0, 0.0, 5)]),
        },
        Layout {
            name: "l6-whole",
            tl: tl((0..6).map(|k| vec![long(k, 2.0 * k as f64 + 0.3, 58.0)]).collect()),
        },
        Layout {
            name: "l8-whole",
            tl: tl((0..8).map(|k| vec![long(k, 2.0 * k as f64 + 0.3, 58.0)]).collect()),
        },
        Layout {
            name: "l10-staggered",
            tl: tl(vec![(0..10)
                .map(|k| long(k, 5.0 * k as f64 + 0.3, 5.0 * k as f64 + 20.0))
                .collect()]),
        },
        Layout {
            name: "serial-runs-6x8",
            tl: tl(vec![{
                let mut v = Vec::new();
                for r in 0..6 {
                    let at = 10.0 * r as f64;
                    v.extend(strip(b, 8, at, 1.0, 1.0, 0.0, r));
                    let mut plain = b.clip(r, at + 8.0, at + 10.0);
                    plain.blend_mode = BlendMode::Normal;
                    v.push(plain);
                }
                v
            }]),
        },
        Layout {
            name: "interleaved-3-tracks",
            tl: tl((0..3)
                .map(|k| {
                    let mut v = strip(b, 8, 0.0, 3.0, 3.0, 0.0, k);
                    v.extend((0..4).map(|j| {
                        long(
                            j + k,
                            24.0 + 9.0 * j as f64 + 0.3 * k as f64,
                            30.0 + 9.0 * j as f64 + 0.3 * k as f64 + 4.0,
                        )
                    }));
                    v
                })
                .collect()),
        },
        Layout {
            name: "r2x10-l4-same-tracks",
            tl: tl(vec![
                {
                    let mut v = strip(b, 10, 0.0, 2.0, 2.0, 0.0, 0);
                    v.extend((0..2).map(|j| long(j, 24.0 + 12.0 * j as f64 + 0.3, 30.0 + 12.0 * j as f64)));
                    v
                },
                {
                    let mut v = strip(b, 10, 0.0, 2.0, 2.0, 0.0, 4);
                    v.extend((0..2).map(|j| long(j + 2, 26.0 + 12.0 * j as f64 + 0.3, 33.0 + 12.0 * j as f64)));
                    v
                },
            ]),
        },
        Layout {
            name: "r2x100-ish",
            tl: tl(vec![strip(b, 60, 0.0, 1.0, 1.0, 0.0, 0), strip(b, 60, 0.0, 1.0, 1.0, 0.0, 7)]),
        },
        Layout {
            name: "stagger3-runs",
            tl: tl((0..3).map(|k| strip(b, 8, 7.0 * k as f64, 3.0, 3.0, 0.0, k)).collect()),
        },
        Layout {
            name: "alt4-tracks",
            tl: tl((0..4)
                .map(|k| {
                    let mut v = strip(b, 7, 0.0, 2.0, 2.0, 0.0, k);
                    v.push({
                        let mut c = long(k, 14.0 + 0.3, 40.0 + k as f64);
                        c.blend_mode = MODES[(k + 3) % MODES.len()];
                        c
                    });
                    v.extend(strip(b, 7, 44.0 + k as f64, 2.0, 2.0, 0.0, k + 4));
                    v
                })
                .collect()),
        },
        Layout {
            name: "l9-whole",
            tl: tl((0..9).map(|k| vec![long(k, 2.0 * k as f64 + 0.3, 58.0)]).collect()),
        },
        Layout {
            name: "l12-whole",
            tl: tl((0..12).map(|k| vec![long(k, 2.0 * k as f64 + 0.3, 58.0)]).collect()),
        },
        Layout {
            name: "straddle-run",
            tl: tl({
                let mut t = vec![strip(b, 30, 0.0, 2.0, 2.0, 0.0, 0)];
                t.extend((0..4).map(|k| vec![long(k + 1, 31.0 + k as f64 + 0.3, 58.0)]));
                t
            }),
        },
        Layout {
            name: "r1x60-l3",
            tl: tl({
                let mut t = vec![strip(b, 60, 0.0, 1.0, 1.0, 0.0, 0)];
                t.extend((0..3).map(|k| vec![long(k + 1, 3.0 * k as f64 + 0.3, 57.0)]));
                t
            }),
        },
    ];
    out.retain(|l| only(l.name));
    out
}

fn file_md5s(path: &Path, tag: &str) -> Vec<String> {
    let out = out_dir().join(format!("{tag}.fmd5"));
    let mut cmd = command(&ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-fps_mode", "passthrough", "-f", "framemd5", "-y"])
        .arg(&out);
    run_bounded(&mut cmd, BOUND, tag).unwrap();
    let list: Vec<String> = parse_md5(&out)
        .into_iter()
        .map(|l| l.rsplit(':').next().unwrap().to_string())
        .collect();
    let _ = std::fs::remove_file(&out);
    list
}

fn lossless_opts(b: &Build) -> ExportOptions {
    b.opts()
}

/// `KERF_CHECK_STAGE=ref` (6.1): the whole nested export of each layout, with no fold and no parts, written to
/// `bs-{name}.ref`. Otherwise (the build under test): `render_with_progress` as the app calls it, timed, its file read back, and
/// compared with the `ref` where there is one.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn check_backstop() {
    let v = version_line();
    eprintln!("{v}");
    let stage = std::env::var("KERF_CHECK_STAGE").unwrap_or_default();
    let limit: u64 = std::env::var("KERF_CHECK_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(150);
    let fx = fixtures();
    let b = Build {
        fps: 30.0,
        canvas: (192, 108),
        base: &fx.base30,
        src: &fx.mp4_30,
    };
    let assets = vec![fx.base30.clone(), fx.mp4_30.clone()];
    let mut slow = Vec::new();
    let mut unequal = Vec::new();
    for layout in layouts(&b) {
        let refpath = out_dir().join(format!("bs-{}.ref{}", layout.name, ffmpeg_major()));
        let blended = blended_count(&layout.tl);
        let runs = folds_of(&layout.tl, &assets, &b.opts())
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>();
        if stage == "ref" {
            if blended > nested_max() {
                eprintln!(
                    "REF  {:<24} blended {blended:>3}: nested whole not run on this build",
                    layout.name
                );
                continue;
            }
            export_video(
                &layout.tl,
                &assets,
                &b.opts(),
                Some(usize::MAX),
                &out_dir().join(format!("bs-{}.ref{}.mkv", layout.name, ffmpeg_major())),
            );
            match export_md5s(
                &layout.tl,
                &assets,
                &b.opts(),
                Some(usize::MAX),
                &format!("ref-{}", layout.name),
                Duration::from_secs(limit),
            ) {
                Ok((md5s, took)) => {
                    let lines: Vec<String> = md5s.iter().map(|l| l.rsplit(':').next().unwrap().to_string()).collect();
                    std::fs::write(&refpath, lines.join("\n")).unwrap();
                    eprintln!(
                        "REF  {:<24} blended {blended:>3} runs {runs:?}: {} frames, nested whole {:.1} s",
                        layout.name,
                        lines.len(),
                        took.as_secs_f64()
                    );
                }
                Err(e) => eprintln!(
                    "REF  {:<24} blended {blended:>3}: {}",
                    layout.name,
                    e.lines().next().unwrap_or("")
                ),
            }
            continue;
        }
        if stage == "refp" {
            // The same export with folding off: nested graphs in parts of at most six nested forks, by the backstop alone.
            let plan = with_fold_min(usize::MAX, || {
                plan_export(&layout.tl, &assets, &lossless_opts(&b), SEGMENT_LIMITS)
            });
            let parts = plan.as_ref().map_or(1, |p| p.parts.len());
            let out = out_dir().join(format!("bs-{}.refp{}.mkv", layout.name, ffmpeg_major()));
            let _ = std::fs::remove_file(&out);
            let started = Instant::now();
            let status = with_fold_min(usize::MAX, || {
                render_with_progress(&layout.tl, &assets, &out, &lossless_opts(&b), &mut |_| {}, &|| {
                    started.elapsed() > Duration::from_secs(limit)
                })
            });
            let took = started.elapsed().as_secs_f64();
            if matches!(status, Ok(RenderStatus::Completed)) {
                let got = file_md5s(&out, &format!("refp-{}", layout.name));
                std::fs::write(
                    out_dir().join(format!("bs-{}.refp{}", layout.name, ffmpeg_major())),
                    got.join("\n"),
                )
                .unwrap();
                eprintln!(
                    "REFP {:<24} blended {blended:>3} parts {parts}: {:.1} s; {} frames",
                    layout.name,
                    took,
                    got.len()
                );
            } else {
                eprintln!(
                    "REFP {:<24} blended {blended:>3} parts {parts}: {} after {took:.0} s",
                    layout.name,
                    if status.is_ok() {
                        "NO END".to_string()
                    } else {
                        format!("{status:?}")
                    }
                );
            }
            continue;
        }
        let plan = plan_export(&layout.tl, &assets, &lossless_opts(&b), SEGMENT_LIMITS);
        let parts = plan.as_ref().map_or(1, |p| p.parts.len());
        let out = out_dir().join(format!("bs-{}.cand.mkv", layout.name));
        let _ = std::fs::remove_file(&out);
        let started = Instant::now();
        let status = render_with_progress(&layout.tl, &assets, &out, &lossless_opts(&b), &mut |_| {}, &|| {
            started.elapsed() > Duration::from_secs(limit)
        });
        let took = started.elapsed().as_secs_f64();
        match status {
            Ok(RenderStatus::Completed) => {
                let got = file_md5s(&out, &format!("cand-{}", layout.name));
                let want = std::fs::read_to_string(&refpath)
                    .ok()
                    .map(|s| s.lines().map(String::from).collect::<Vec<_>>());
                let wantp = std::fs::read_to_string(out_dir().join(format!("bs-{}.refp{}", layout.name, ffmpeg_major())))
                    .ok()
                    .map(|s| s.lines().map(String::from).collect::<Vec<_>>());
                let judge_against = |name: &str, w: &Option<Vec<String>>, unequal: &mut Vec<&'static str>| match w {
                    Some(w) if *w == got => format!("EQUAL to {name}"),
                    Some(w) => {
                        let bad = (0..got.len().min(w.len())).filter(|&k| got[k] != w[k]).count();
                        unequal.push(layout.name);
                        format!("DIFFERS from {name}: {bad} of {} frames (it has {})", got.len(), w.len())
                    }
                    None => format!("no {name}"),
                };
                let verdict = format!(
                    "{}; {}",
                    judge_against("the whole nested export", &want, &mut unequal),
                    judge_against("the nested export in parts", &wantp, &mut unequal)
                );
                std::fs::write(
                    out_dir().join(format!("bs-{}.cand{}", layout.name, ffmpeg_major())),
                    got.join("\n"),
                )
                .unwrap();
                eprintln!(
                    "CAND {:<24} blended {blended:>3} runs {runs:?} parts {parts}: {:.1} s; {} frames; {verdict}",
                    layout.name,
                    took,
                    got.len()
                );
                if took > 60.0 {
                    slow.push((layout.name, took));
                }
            }
            Ok(RenderStatus::Cancelled) => {
                eprintln!(
                    "CAND {:<24} blended {blended:>3} runs {runs:?} parts {parts}: NO END after {limit} s",
                    layout.name
                );
                slow.push((layout.name, took));
            }
            Err(e) => eprintln!("CAND {:<24} ERROR {e}", layout.name),
        }
        let _ = std::fs::rename(&out, out_dir().join(format!("bs-{}.cand{}.mkv", layout.name, ffmpeg_major())));
    }
    eprintln!("slow (> 60 s): {slow:?}; unequal: {unequal:?}");
}

// ---- 5. hundreds of clips in one run --------------------------------------------------------------------------

/// `KERF_CHECK_BIG` (default `100,200,400`): that many one-frame clips butted, folded into one run. The graph's size, whether
/// ffmpeg accepts it, the time, and spot checks against each clip's own export.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn check_big_runs() {
    eprintln!("{}", version_line());
    let sizes: Vec<usize> = std::env::var("KERF_CHECK_BIG")
        .unwrap_or_else(|_| "100,200,400".into())
        .split(',')
        .filter_map(|s| s.parse().ok())
        .collect();
    let fx = fixtures();
    let b = Build {
        fps: 30.0,
        canvas: (192, 108),
        base: &fx.base30,
        src: &fx.mp4_30,
    };
    let assets = vec![fx.base30.clone(), fx.mp4_30.clone()];
    for n in sizes {
        let clips = b.run(&even(n, 0.0, 1.0), |_, _| {});
        let tl = b.timeline(vec![clips], n as f64 + 2.0);
        let runs = folds_of(&tl, &assets, &b.opts());
        let args = export_args(&tl, &assets, &b.opts());
        let graph = graph_of_args(&args).to_string();
        let (md5s, took) = match export_md5s(&tl, &assets, &b.opts(), None, &format!("big{n}"), Duration::from_secs(900)) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "{n} clips: runs {:?} graph {} bytes: {}",
                    runs.iter().map(Vec::len).collect::<Vec<_>>(),
                    graph.len(),
                    e.lines().next().unwrap_or("")
                );
                continue;
            }
        };
        eprintln!(
            "{n} clips: runs {:?}, graph {} bytes ({} statements, spilled to a script: {}), {} frames in {:.1} s",
            runs.iter().map(Vec::len).collect::<Vec<_>>(),
            graph.len(),
            graph.split(';').count(),
            !args.iter().any(|a| a.starts_with("color="))
                && args.iter().any(|a| a == "-filter_complex_script" || a == "-/filter_complex"),
            md5s.len(),
            took.as_secs_f64()
        );
        // Each sampled clip's own export (one blend, nested) agrees with the run's frame for it.
        let step = (n / 5).max(1);
        for i in (step / 2..n).step_by(step) {
            let one = b.timeline(vec![vec![b.clip(i, i as f64, i as f64 + 1.0)]], n as f64 + 2.0);
            // The clip's mode in the run is MODES[i % 9]; the alone clip was built with the same index.
            let (alone, _) = export_md5s(&one, &assets, &b.opts(), None, &format!("big{n}-one{i}"), BOUND).unwrap();
            let slot = i + usize::from(false);
            let same = (slot..slot + 1).all(|k| alone[k] == md5s[k]);
            eprintln!(
                "  clip {i}: frame {slot} {}",
                if same {
                    "matches its own export"
                } else {
                    "DIFFERS from its own export"
                }
            );
        }
    }
}

// ---- known failures, replayed ---------------------------------------------------------------------------------

/// Layouts the sweep found where a folded pair shares a frame, and an ordinary one: frame-aligned cuts on a 30 fps timeline over
/// a 25 fps AVI (whose time base is the frame, 40 ms, so `setpts` truncates a start to a source frame).
#[test]
#[ignore = "needs the ffmpeg binary"]
fn check_exact_known_collisions() {
    eprintln!("{}", version_line());
    let base = plain("base_30", "testsrc", "30", "mp4");
    let cases: Vec<(&str, f64, &str, &str, Vec<f64>)> = vec![
        ("avi25-on-30-aligned", 30.0, "25", "avi", even(8, 3.0, 3.0)),
        ("avi30-on-30-aligned", 30.0, "30", "avi", even(8, 3.0, 3.0)),
        (
            "avi24-unaligned",
            24.0,
            "24",
            "avi",
            vec![0.0, 5.877, 9.79, 11.000000001, 13.901, 16.0, 18.604, 21.519],
        ),
        (
            "avi25-unaligned",
            25.0,
            "25",
            "avi",
            vec![0.0, 5.626, 9.999, 12.614, 17.9999, 19.975, 23.901, 28.467],
        ),
        (
            "mkv120",
            120.0,
            "120",
            "mkv",
            vec![0.0, 3.000000001, 7.588, 9.557, 12.999, 17.999, 19.9999, 24.0, 28.112],
        ),
        (
            "mkv5994",
            60000.0 / 1001.0,
            "60000/1001",
            "mkv",
            vec![1.0, 4.554, 8.618, 10.552, 15.000000001, 17.0, 21.999999999, 24.563, 26.0],
        ),
        (
            "mov600-5994",
            60000.0 / 1001.0,
            "60000/1001",
            "mov600",
            vec![2.0, 4.9999, 6.569, 9.65, 14.9999, 17.73, 21.656, 24.53, 27.37],
        ),
        (
            "mp4-5994-same-bounds",
            60000.0 / 1001.0,
            "60000/1001",
            "mp4",
            vec![1.0, 4.554, 8.618, 10.552, 15.000000001, 17.0, 21.999999999, 24.563, 26.0],
        ),
        (
            "mp4-120-same-bounds",
            120.0,
            "120",
            "mp4",
            vec![0.0, 3.000000001, 7.588, 9.557, 12.999, 17.999, 19.9999, 24.0, 28.112],
        ),
    ];
    let mut results = Vec::new();
    for (name, fps, lavfi, ext, bounds) in cases {
        let src = plain(&format!("cut_{name}"), "testsrc2", lavfi, ext);
        let b = Build {
            fps,
            canvas: (192, 108),
            base: &base,
            src: &src,
        };
        let clips = b.run(&bounds, |_, c| {
            c.transform.opacity = 1.0;
            c.transform.scale = 1.0;
            c.transform.pos_x = 0.0;
        });
        let frames = bounds.last().unwrap() + 4.0;
        let case = simple(&format!("known-{name}"), fps, &base, &src, vec![clips], frames);
        if !only(&case.name) {
            continue;
        }
        if let Ok(g) = guard_case(&case) {
            let (v, c) = judge(&g);
            for line in v.iter().chain(c.iter()) {
                eprintln!("  {line}");
            }
        }
        report(&[case], &mut results);
    }
    finish(&results);
}

/// For each rate and container: butted opaque clips whose cuts all lie in the band past half a frame (where the guard folds a cut that
/// is not on a frame) or on the frame, `KERF_CHECK_SEEDS` timelines of eight clips (default 8): how many cuts have a folded pair that
/// share a frame, read from the graph.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn check_guard_band() {
    eprintln!("{}", version_line());
    let base = plain("base_30", "testsrc", "30", "mp4");
    let seeds: usize = std::env::var("KERF_CHECK_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let rates: [(&str, f64, &str); 12] = [
        ("24", 24.0, "24"),
        ("25", 25.0, "25"),
        ("30", 30.0, "30"),
        ("23.976", 24000.0 / 1001.0, "24000/1001"),
        ("29.97", 30000.0 / 1001.0, "30000/1001"),
        ("50", 50.0, "50"),
        ("59.94", 60000.0 / 1001.0, "60000/1001"),
        ("60", 60.0, "60"),
        ("72", 72.0, "72"),
        ("100", 100.0, "100"),
        ("120", 120.0, "120"),
        ("240", 240.0, "240"),
    ];
    let exts: Vec<&str> = std::env::var("KERF_CHECK_EXTS")
        .unwrap_or_else(|_| "mkv,mp4,mov600,avi".into())
        .leak()
        .split(',')
        .collect();
    let aligned = std::env::var_os("KERF_CHECK_ALIGNED").is_some();
    let mut table = Vec::new();
    for (rname, fps, lavfi) in rates {
        for ext in exts.iter().copied() {
            let name = format!("band-{rname}-{ext}");
            if !only(&name) {
                continue;
            }
            let src = plain(&format!("cut_{rname}_{ext}"), "testsrc2", lavfi, ext);
            let b = Build {
                fps,
                canvas: (192, 108),
                base: &base,
                src: &src,
            };
            let (mut cuts, mut early, mut clash, mut folded_cuts) = (0usize, 0usize, 0usize, 0usize);
            for seed in 0..seeds {
                let mut bounds = vec![2.0];
                for i in 0..8 {
                    let frac = if aligned {
                        0.0
                    } else {
                        0.5501 + (((seed * 8 + i) * 37) % 450) as f64 / 1000.0
                    };
                    bounds.push(2.0 + 4.0 * (i + 1) as f64 + frac);
                }
                let clips = b.run(&bounds, |_, c| {
                    c.transform.opacity = 1.0;
                    c.transform.scale = 1.0;
                    c.transform.pos_x = 0.0;
                });
                let case = simple(&name, fps, &base, &src, vec![clips], bounds.last().unwrap() + 4.0);
                let Ok(g) = guard_case(&case) else { continue };
                let (v, c) = judge(&g);
                cuts += 7;
                early += v.len();
                clash += c.len();
                folded_cuts += g.folded.iter().map(|r| r.len() - 1).sum::<usize>();
            }
            eprintln!(
                "{name:<22} cuts {cuts:>3} folded-adjacent {folded_cuts:>3} claim violations {early:>3} shared frames {clash:>3}"
            );
            table.push((name, folded_cuts, early, clash));
        }
    }
    let bad: Vec<String> = table
        .iter()
        .filter(|t| t.3 > 0)
        .map(|t| format!("{} ({} pairs)", t.0, t.3))
        .collect();
    assert!(bad.is_empty(), "folded pairs share a frame: {}", bad.join(", "));
}

#[test]
#[ignore = "prints the argv after the graph"]
fn check_dump_args() {
    let fx = fixtures();
    let b = Build {
        fps: 30.0,
        canvas: (192, 108),
        base: &fx.base30,
        src: &fx.mp4_30,
    };
    let assets = vec![fx.base30.clone(), fx.mp4_30.clone()];
    let tl = b.timeline((0..3).map(|k| vec![b.clip(k, 2.0 * k as f64 + 0.3, 20.0)]).collect(), 30.0);
    let args = build_export_args(&tl, &assets, "out.mkv", &b.opts()).unwrap();
    let i = args.iter().position(|a| a == "-filter_complex").unwrap();
    eprintln!("BEFORE: {:?}", &args[..i]);
    eprintln!("GRAPH: {}", args[i + 1]);
    eprintln!("AFTER: {:?}", &args[i + 2..]);
}

// ---- unit checks that hold the guard and the planner's numbers (no ffmpeg) ------------------------------------

fn fake_build(fps: f64) -> (Vec<Asset>, f64) {
    let a = super::fold_dump::fixture_assets();
    (vec![a[0].clone(), a[1].clone()], fps)
}

fn fake_opts(fps: f64) -> ExportOptions {
    ExportOptions {
        fps: Some(fps),
        resolution: Some((640, 360)),
        ..ExportOptions::default()
    }
}

/// How many of `n` blended clips butted at `frac` of a frame past the frame (every cut but the first) fold, at `fps`.
fn folded_at(fps: f64, frac: f64, n: usize) -> usize {
    let (assets, fps) = fake_build(fps);
    let b = Build {
        fps,
        canvas: (640, 360),
        base: &assets[0],
        src: &assets[1],
    };
    let bounds: Vec<f64> = (0..=n).map(|i| if i == 0 { 0.0 } else { 5.0 * i as f64 + frac }).collect();
    let tl = b.timeline(vec![b.run(&bounds, |_, _| {})], 5.0 * n as f64 + 5.0);
    folds_of(&tl, &assets, &fake_opts(fps)).iter().map(Vec::len).sum()
}

/// A cut `frac` of a frame past a frame folds exactly where the next clip's slot cannot be the last one's: the guard's slack band
/// (half a frame, and 0.05) is where the answer flips.
#[test]
fn check_the_slack_band_flips_at_a_half_frame_and_a_twentieth() {
    for fps in [24.0, 25.0, 30.0, 30000.0 / 1001.0, 60.0] {
        for (frac, folds) in [
            (0.0, true),
            (1e-9, true),
            (1e-3, false),
            (0.3, false),
            (0.5, false),
            (0.52, false),
            (0.549, false),
            (0.5501, true),
            (0.56, true),
            (0.9, true),
            (0.999, true),
        ] {
            let got = folded_at(fps, frac, 9);
            assert_eq!(
                got > 0,
                folds,
                "{fps} fps, a cut {frac} of a frame past the frame: {got} clips fold"
            );
        }
    }
}

/// `n` blended clips of `len` frames on each of `tracks` tracks, every one starting at the same frame as its neighbour on the next
/// track, so that the tracks are open at once.
fn stacked(assets: &[Asset], fps: f64, tracks: Vec<Vec<f64>>) -> Timeline {
    let b = Build {
        fps,
        canvas: (640, 360),
        base: &assets[0],
        src: &assets[1],
    };
    let end = tracks.iter().flat_map(|t| t.last().copied()).fold(0.0, f64::max);
    b.timeline(
        tracks
            .iter()
            .enumerate()
            .map(|(k, bounds)| b.run(bounds, |i, c| c.blend_mode = MODES[(i + k) % 9]))
            .collect(),
        end,
    )
}

fn parts_of(tl: &Timeline, assets: &[Asset], fps: f64) -> Option<usize> {
    plan_export(tl, assets, &fake_opts(fps), SEGMENT_LIMITS).map(|p| p.parts.len())
}

/// What the cost model lets run whole: 6 forks of the composite. A run is 3 (it measured 1.6 s alone, 2.9 s for two, 10 s for
/// three, over 120 s for four, on 9.0.2).
#[test]
fn check_the_cost_model_prices_a_run_at_three_and_the_limit_is_six() {
    let (assets, fps) = fake_build(30.0);
    let run = |k: usize| even(10, 0.0, 6.0).iter().map(|f| f + 0.0 * k as f64).collect::<Vec<f64>>();
    let loose = |k: usize| vec![8.0 + 5.0 * k as f64 + 0.3, 56.0];
    // Runs alone, side by side.
    for (runs, whole) in [(1, true), (2, true), (3, false), (4, false)] {
        let tl = stacked(&assets, fps, (0..runs).map(run).collect());
        assert_eq!(folds_of(&tl, &assets, &fake_opts(fps)).len(), runs);
        assert_eq!(parts_of(&tl, &assets, fps).is_none(), whole, "{runs} runs of ten");
    }
    // A run and the clips of their own beside it: 3 + 3 fit, 3 + 4 do not.
    for (own, whole) in [(0, true), (3, true), (4, false), (6, false)] {
        let mut tracks = vec![run(0)];
        tracks.extend((0..own).map(loose));
        let tl = stacked(&assets, fps, tracks);
        assert_eq!(parts_of(&tl, &assets, fps).is_none(), whole, "a run and {own} of their own");
    }
    // Clips of their own: 6 fit. Seven and nine, each coming on screen after the one before and staying to the end, are all on
    // screen together at the end: no part nests fewer than the whole, and a cut that lowers no part is not made (9.0.2: parts
    // were slower than the whole export for these layouts, 8 layers 15 s against 18 to 22 s).
    for (own, whole) in [(5, true), (6, true), (7, true), (9, true)] {
        let tl = stacked(&assets, fps, (0..own).map(loose).collect());
        assert_eq!(parts_of(&tl, &assets, fps).is_none(), whole, "{own} nested clips");
    }
}

/// A part that holds one clip of a run does not fold it; two do.
#[test]
fn check_a_part_folds_what_two_or_more_of_a_run_it_holds() {
    let (assets, fps) = fake_build(30.0);
    let b = Build {
        fps,
        canvas: (640, 360),
        base: &assets[0],
        src: &assets[1],
    };
    let tl = b.timeline(vec![b.run(&even(9, 0.0, 3.0), |_, _| {})], 30.0);
    let cut = prepared_cut(&tl, &fake_opts(fps));
    let fx = transition_fx(&cut, &assets);
    let fmt = export_format(&cut, &assets, &fake_opts(fps));
    let of = |keep: &[usize]| blend_folds(&cut, &assets, &fx, fmt.fps, &|f| keep.contains(&f));
    assert_eq!(of(&[1, 2, 3, 4, 5, 6, 7, 8, 9]), vec![vec![1, 2, 3, 4, 5, 6, 7, 8, 9]]);
    assert_eq!(of(&[4, 5, 6]), vec![vec![4, 5, 6]]);
    assert_eq!(of(&[4, 5]), vec![vec![4, 5]]);
    assert!(of(&[4]).is_empty());
    assert!(of(&[]).is_empty());
    assert!(of(&[0, 4]).is_empty(), "the base clip is not blended");
}

/// `KERF_CHECK_DUMP_DIR`: the inputs, and the nested and the folded graph, of `n` blended clips of two frames every `gap` frames
/// over a base of `total` frames (`KERF_CHECK_SPEC=n,gap,total`, default `8,200,1800`), for a memory measurement by hand.
#[test]
#[ignore = "writes $KERF_CHECK_DUMP_DIR"]
fn check_dump_graph() {
    let Some(dir) = std::env::var_os("KERF_CHECK_DUMP_DIR").map(PathBuf::from) else {
        return;
    };
    let spec: Vec<f64> = std::env::var("KERF_CHECK_SPEC")
        .unwrap_or_else(|_| "8,200,1800".into())
        .split(',')
        .filter_map(|s| s.parse().ok())
        .collect();
    let (n, gap, total) = (spec[0] as usize, spec[1], spec[2]);
    let base = media(
        "mem_base",
        "testsrc",
        (640, 360),
        "30",
        70,
        "format=yuv420p",
        "mp4",
        &["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"],
    );
    let src = media(
        "mem_src",
        "testsrc2",
        (640, 360),
        "30",
        20,
        "format=yuv420p",
        "mp4",
        &["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"],
    );
    let b = Build {
        fps: 30.0,
        canvas: (640, 360),
        base: &base,
        src: &src,
    };
    let clips: Vec<Clip> = (0..n).map(|i| b.clip(i, i as f64 * gap, i as f64 * gap + 2.0)).collect();
    let tl = b.timeline(vec![clips], total);
    let assets = vec![base.clone(), src.clone()];
    for (name, min) in [("nested", Some(usize::MAX)), ("folded", None)] {
        let args = match min {
            Some(m) => with_fold_min(m, || export_args(&tl, &assets, &b.opts())),
            None => export_args(&tl, &assets, &b.opts()),
        };
        let i = args.iter().position(|a| a == "-filter_complex").unwrap();
        std::fs::write(dir.join(format!("{name}.graph")), &args[i + 1]).unwrap();
        std::fs::write(dir.join("inputs.json"), serde_json::to_string(&args[..i]).unwrap()).unwrap();
    }
}

/// Sources of other pixel formats and odd sizes (4:2:2, 10-bit, 4:4:4, RGB, full range, alpha) through a run of eight, folded
/// against nested; odd canvas sizes too.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn check_exact_pixel_formats_and_sizes() {
    eprintln!("{}", version_line());
    let base = plain("base_30", "testsrc", "30", "mp4");
    let alpha_filter = "format=yuva444p10le,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(lt(mod(X,32),16),1023,300)'";
    let variants: Vec<(&str, Asset)> = vec![
        (
            "yuv422p",
            media(
                "v_422",
                "testsrc2",
                (192, 108),
                "30",
                8,
                "format=yuv422p",
                "mp4",
                &["-c:v", "libx264", "-crf", "0", "-g", "1", "-pix_fmt", "yuv422p"],
            ),
        ),
        (
            "yuv420p10",
            media(
                "v_10",
                "testsrc2",
                (192, 108),
                "30",
                8,
                "format=yuv420p10le",
                "mkv",
                &["-c:v", "ffv1", "-pix_fmt", "yuv420p10le"],
            ),
        ),
        (
            "yuv444p",
            media(
                "v_444",
                "testsrc2",
                (192, 108),
                "30",
                8,
                "format=yuv444p",
                "mkv",
                &["-c:v", "ffv1", "-pix_fmt", "yuv444p"],
            ),
        ),
        (
            "rgb",
            media(
                "v_rgb",
                "testsrc2",
                (192, 108),
                "30",
                8,
                "format=bgr0",
                "mkv",
                &["-c:v", "ffv1", "-pix_fmt", "bgr0"],
            ),
        ),
        (
            "yuvj420p",
            media(
                "v_j",
                "testsrc2",
                (192, 108),
                "30",
                8,
                "format=yuvj420p",
                "mp4",
                &["-c:v", "libx264", "-crf", "0", "-g", "1", "-pix_fmt", "yuvj420p"],
            ),
        ),
        (
            "prores-alpha",
            media(
                "v_pa",
                "testsrc2",
                (192, 108),
                "30",
                8,
                alpha_filter,
                "mov",
                &["-c:v", "prores_ks", "-profile:v", "4444", "-pix_fmt", "yuva444p10le"],
            ),
        ),
        (
            "odd-161x91",
            media(
                "v_odd",
                "testsrc2",
                (161, 91),
                "30",
                8,
                "format=yuv444p",
                "mkv",
                &["-c:v", "ffv1", "-pix_fmt", "yuv444p"],
            ),
        ),
        (
            "4x3-in-16x9",
            media(
                "v_43",
                "testsrc2",
                (160, 120),
                "30",
                8,
                "format=yuv420p",
                "mp4",
                &["-c:v", "libx264", "-crf", "0", "-g", "1", "-pix_fmt", "yuv420p"],
            ),
        ),
    ];
    let mut results = Vec::new();
    for (label, src) in &variants {
        for canvas in [(192u32, 108u32), (191, 107)] {
            let b = Build {
                fps: 30.0,
                canvas,
                base: &base,
                src,
            };
            let clips = b.run(&even(8, 0.0, 3.0), |_, _| {});
            let mut case = simple(
                &format!("pix-{label}-{}x{}", canvas.0, canvas.1),
                30.0,
                &base,
                src,
                vec![clips],
                28.0,
            );
            case.opts = b.opts();
            report(&[case], &mut results);
        }
    }
    finish(&results);
}

/// A run is the clips of one track: tracks that follow one another in time are two runs, not one of both.
#[test]
fn check_a_run_does_not_cross_to_a_track_that_starts_after_it_ends() {
    let on = |track: usize, first: i64| FoldClip {
        track,
        slots: Some((first, first + 2)),
    };
    let mut clips: Vec<FoldClip> = (0..7).map(|i| on(0, 2 * i)).collect();
    clips.extend((0..7).map(|i| on(1, 14 + 2 * i)));
    assert_eq!(fold_runs(&clips, 7), vec![0..7, 7..14]);
    // Six and eight do not make a run of fourteen either.
    let mut mixed: Vec<FoldClip> = (0..6).map(|i| on(0, 2 * i)).collect();
    mixed.extend((0..8).map(|i| on(1, 12 + 2 * i)));
    assert_eq!(fold_runs(&mixed, 7), vec![6..14]);
}

/// A run over eight different files (eight inputs, no `split` fan-out), over two files alternating, and a track whose clips are
/// stored out of timeline order: the same pictures folded and nested.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn check_exact_distinct_inputs_and_storage_order() {
    eprintln!("{}", version_line());
    let base = plain("base_30", "testsrc", "30", "mp4");
    let names = [
        "testsrc",
        "testsrc2",
        "smptebars",
        "smptehdbars",
        "rgbtestsrc",
        "yuvtestsrc",
        "pal75bars",
        "pal100bars",
    ];
    let files: Vec<Asset> = names.iter().map(|n| plain(&format!("d_{n}"), n, "30", "mp4")).collect();
    let mut assets = vec![base.clone()];
    assets.extend(files.iter().cloned());
    let mut results = Vec::new();
    let one = |label: &str, pick: &dyn Fn(usize) -> usize, reverse: bool| {
        let b = Build {
            fps: 30.0,
            canvas: (192, 108),
            base: &base,
            src: &files[0],
        };
        let mut clips = b.run(&even(8, 0.0, 3.0), |_, _| {});
        for (i, c) in clips.iter_mut().enumerate() {
            c.asset_id = files[pick(i)].id;
        }
        if reverse {
            clips.reverse();
        }
        Case {
            name: label.to_string(),
            tl: b.timeline(vec![clips], 28.0),
            assets: assets.clone(),
            opts: b.opts(),
        }
    };
    let cases = vec![
        one("inputs-eight-files", &|i| i, false),
        one("inputs-two-files-alternating", &|i| i % 2, false),
        one("inputs-one-file-stored-in-reverse", &|_| 0, true),
        one("inputs-eight-files-stored-in-reverse", &|i| i, true),
    ];
    report(&cases, &mut results);
    finish(&results);
}

/// A comma inside the parentheses of a filter's expression has to be inside quotes, or the graph parser reads it as the end of the
/// filter: in every statement of every graph of the corpus, folded or not.
fn unquoted_comma_in_parens(stmt: &str) -> Option<usize> {
    let (mut quoted, mut escaped, mut depth) = (false, false, 0i32);
    for (i, c) in stmt.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth > 0 => return Some(i),
            _ => {}
        }
    }
    None
}

#[test]
fn check_commas_in_expressions_are_quoted_in_every_graph() {
    let (mut graphs, mut folded) = (0, 0);
    for seed in 0..2500u64 {
        let (tl, assets, opts) = super::fold_dump::seeded(seed, None);
        for min in [None, Some(usize::MAX)] {
            let args = match min {
                Some(m) => with_fold_min(m, || export_args(&tl, &assets, &opts)),
                None => export_args(&tl, &assets, &opts),
            };
            let graph = graph_of_args(&args);
            graphs += 1;
            folded += usize::from(graph.contains("bldf"));
            for stmt in graph.split(';') {
                assert_eq!(unquoted_comma_in_parens(stmt), None, "seed {seed}: {stmt}");
            }
        }
    }
    assert!(folded > 100, "{folded} of {graphs} graphs were folded");
}
