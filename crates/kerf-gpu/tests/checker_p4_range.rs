//! `checker/p4/range`: a slice-thread count that is a range and not one number, through the real path (an export or a still of FFmpeg's, the
//! plan's verdict, the GPU's planes against them), and the budget's cap when the machine moves under it.
//!
//! * `checker_p4_range_a_count_the_plan_does_not_know_is_a_range_nothing_assumes_one_number_of`: **the plan's thread cannot read the
//!   affinity** (a seccomp filter on that one thread; the export runs on a worker started before, so FFmpeg counts the machine's four CPUs
//!   and runs its graph on five slice threads, as on a system whose count the plan does not reproduce) and the export is run on 1, 2, 3, 4,
//!   5, 7, 8, 12 and 16 slice threads (a wrapper puts `-filter_threads N -filter_complex_threads N` ahead of the engine's own arguments).
//!   Every blur frame the plan clears must be exact against every one of them, a refusal must be one a drawing at some count shows exact
//!   (when drawn anyway the frame differs), a chroma key is refused at 100 % and **drawn and exact where a budget caps the graph** (the cap
//!   is a count the plan knows), stills of images and proxies (one thread, whatever the machine says) are drawn and exact, and stills of
//!   footage follow the export.
//! * `checker_p4_range_a_failed_affinity_read_is_the_one_thread_graph`: the same with the filter on every thread, so the FFmpeg that
//!   inherits it counts one CPU (`av_cpu_count` keeps its initial 1) and runs on one slice thread: every blur and key frame the plan
//!   clears is the real one-thread graph's, to the sample.
//! * `checker_p4_range_no_other_feature_depends_on_the_slice_threads`: Kerf's own exports of every effect, colour, grade, transform,
//!   keyframed move, fade, transition, blend mode, mask and fit are made on 1, 2, 3, 5, 8 and 16 slice threads and compared byte for byte.
//!   The two features known to depend on the count (a chroma key, a blur on a narrow plane) are the controls: the sweep must see them
//!   differ. Anything else that differs is a feature the plan has to judge over the range.
//! * `checker_p4_budget_*`: the slice threads the plan says (`cpu_graph_thread_range`) against the arguments the engine gives the FFmpeg it
//!   starts (a wrapper logs them, and the cores FFmpeg itself detects in the same place), at budgets 10 to 100 %, after the affinity of the
//!   thread narrows (`taskset -p`) and after a cpuset is resized under the process, and under a cgroup CPU quota that hides cores from
//!   this process and not from FFmpeg.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_p4_range -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr)]

#[path = "common/checker_p3.rs"]
mod p3;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use kerf_core::{
    Asset, BlendMode, Clip, Color, CurvePoint, Curves, Delivery, ExportOptions, Fit, Grade, Keyframe, Mask, MaskShape,
    StreamKind, Timeline, Track, Transform, Transition, TransitionKind, VideoEffect, Wheel, Wheels,
};
use p3::{
    blurred, compositor, draw_anyway, effect_clip, exported_frame, forbid_sched_getaffinity, gpu, judge, lavfi_clip,
    motion_frame, narrow_thread, noise_clip, noise_clip_secs, one_track, still_outcome_via, write_wrapper, Judged, Outcome,
    Worker, FFMPEG_STILL,
};

static SERIAL: Mutex<()> = Mutex::new(());

const CHILD: &str = "CHECKER_P4_CHILD";
const LIMIT: Duration = Duration::from_secs(3300);

/// Runs `f` with the wrapper told to start FFmpeg on `n` slice threads (0: leave FFmpeg's own).
fn with_threads<T>(n: u32, f: impl FnOnce() -> T) -> T {
    if n == 0 {
        return f();
    }
    std::env::set_var("P3_FILTER_THREADS", n.to_string());
    let out = f();
    std::env::remove_var("P3_FILTER_THREADS");
    out
}

/// This test binary run again for `test`, with `procs` (cgroup `cgroup.procs` files) joined first and `envs` added.
fn rerun_in(test: &str, procs: &[PathBuf], mask: Option<&str>, envs: &[(&str, String)], limit: Duration) -> (bool, String) {
    let exe = std::env::current_exe().expect("the test binary");
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(r#"while [ "$1" != -- ]; do echo $$ > "$1" || exit 99; shift; done; shift; exec "$@""#)
        .arg("sh");
    cmd.args(procs);
    cmd.arg("--");
    if let Some(mask) = mask {
        cmd.args(["taskset", "-c", mask]);
    }
    cmd.arg(&exe)
        .args([test, "--exact", "--ignored", "--nocapture", "--test-threads=1"]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = p3::child::output_within(cmd, limit).unwrap_or_else(|e| panic!("the child {test}: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// A control group of a cgroup v1 hierarchy that this test made (and removes), when the host lets it.
struct Group {
    dir: PathBuf,
}

impl Group {
    fn new(hierarchy: &str, tag: &str) -> Option<Self> {
        let root = Path::new("/sys/fs/cgroup").join(hierarchy);
        if !root.is_dir() {
            return None;
        }
        let dir = root.join(format!("kerf-p4-{}-{tag}", std::process::id()));
        std::fs::create_dir(&dir).ok()?;
        Some(Self { dir })
    }

    fn write(&self, file: &str, value: &str) -> bool {
        std::fs::write(self.dir.join(file), value).is_ok()
    }

    fn procs(&self) -> PathBuf {
        self.dir.join("cgroup.procs")
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// A CPU quota group of `cpus` CPUs (`cpu.cfs_quota_us` over `cpu.cfs_period_us`).
fn quota_group(cpus: f64) -> Option<Group> {
    let g = Group::new("cpu", &format!("q{}", (cpus * 100.0) as u32))?;
    if !g.write("cpu.cfs_period_us", "100000") || !g.write("cpu.cfs_quota_us", &format!("{}", (cpus * 100_000.0) as i64)) {
        return None;
    }
    Some(g)
}

/// A cpuset group on the CPUs `cpus` of the parent's memory nodes.
fn cpuset_group(cpus: &str) -> Option<Group> {
    let g = Group::new("cpuset", "set")?;
    let mems = std::fs::read_to_string("/sys/fs/cgroup/cpuset/cpuset.mems").ok()?;
    if !g.write("cpuset.cpus", cpus) || !g.write("cpuset.mems", mems.trim()) {
        return None;
    }
    Some(g)
}

// ---- the cases -----------------------------------------------------------------------------------------------------------------------------

/// Blur pictures `(w, h, sigma)` that sit either side of the kernel's rules for one to sixteen slice threads: planes of 15 to 40 rows
/// against sixteen threads, narrow planes, a wide one, a big sigma on a small picture, sigma 0 on pictures with fewer rows than threads.
fn blur_cases() -> Vec<(u32, u32, f64)> {
    vec![
        (64, 64, 3.0),
        (64, 64, 0.5),
        (96, 64, 6.0),
        (128, 96, 12.0),
        (160, 90, 2.0),
        (66, 34, 3.0),
        (34, 34, 3.0),
        (34, 32, 3.0),
        (50, 50, 1.5),
        (30, 30, 3.0),
        (64, 30, 3.0),
        (64, 28, 3.0),
        (40, 64, 3.0),
        (24, 64, 3.0),
        (16, 64, 3.0),
        (12, 64, 3.0),
        (12, 46, 3.0),
        (64, 6, 0.0),
        (40, 6, 0.0),
        (64, 4, 0.0),
        (64, 30, 0.0),
        (12, 64, 0.0),
    ]
}

/// A noise picture keyed over bars, both `w` x `h`.
fn keyed_cut(w: u32, h: u32) -> (Timeline, Vec<Asset>) {
    let bars = lavfi_clip(
        &format!("p4-bars-{w}x{h}"),
        &format!("testsrc2=size={w}x{h}:rate=30:duration=0.4,format=yuv420p"),
    );
    let noise = noise_clip(w, h, "yuv420p");
    let fg = effect_clip(
        &noise,
        vec![VideoEffect::ChromaKey {
            color: "0x808080".into(),
            similarity: 0.25,
            blend: 0.2,
        }],
        0.3,
    );
    let tl = Timeline {
        tracks: vec![
            Track {
                clips: vec![Clip::new(bars.id, 0.0, 0.3, 0.0)],
                ..Track::new(StreamKind::Video, "V1".to_string())
            },
            Track {
                clips: vec![fg],
                ..Track::new(StreamKind::Video, "V2".to_string())
            },
        ],
        ..Timeline::new()
    };
    (tl, vec![bars, noise])
}

// ---- a count the plan does not know ---------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Tally {
    exact: usize,
    refused: usize,
    failures: Vec<String>,
    unnecessary: Vec<String>,
    info: Vec<String>,
}

fn word(g: &[Option<p3::Gap>; 3]) -> String {
    format!("{g:?}")
}

fn unknown_count(split: bool) {
    // The worker exists before the calling thread loses the call, so what it starts (FFmpeg) counts the machine; with `split` false the
    // filter is on every thread and the FFmpeg that inherits it counts one CPU.
    let worker = split.then(Worker::new);
    assert!(forbid_sched_getaffinity(!split), "could not install the seccomp filter");
    assert_eq!(
        kerf_core::ffmpeg_cpu_count(),
        None,
        "the plan's thread cannot read the affinity"
    );
    let _ = gpu();
    let comp = compositor(None);
    assert!(
        comp.caps().effects.contains(kerf_core::EffectKinds::BLUR),
        "the blur probe did not match on this machine: nothing to hold"
    );
    let mode = if split { "split" } else { "failed" };
    let ns: Vec<u32> = if split { vec![1, 2, 3, 4, 5, 7, 8, 12, 16] } else { vec![0] };
    let export_at = |n: u32, slug: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions| {
        with_threads(n, || match &worker {
            Some(w) => w.export(slug, tl, assets, opts),
            None => p3::export(slug, tl, assets, opts),
        })
    };
    let still_at = |n: u32, tl: &Timeline, read: &[Asset], opts: &ExportOptions, t: f64, width: u32| -> Vec<u8> {
        with_threads(n, || match &worker {
            Some(w) => {
                let (tl, read, opts) = (tl.clone(), read.to_vec(), opts.clone());
                w.run(move || FFMPEG_STILL(&tl, &read, &opts, t, width))
            }
            None => FFMPEG_STILL(tl, read, opts, t, width),
        })
    };
    let mut tally = Tally::default();
    let cores = kerf_core::cpu_cores();
    for budget in [100u8, 75, 50, 25] {
        kerf_core::set_cpu_percent(budget);
        let range = kerf_core::cpu_graph_thread_range();
        let capped = budget < 100 && range.0 == range.1 && range.0 < cores;
        eprintln!("CHECKER-P4 {mode}: budget {budget}% range {range:?} cores {cores} capped {capped}");
        assert_eq!(
            capped,
            budget < 100,
            "a budget under 100% on {cores} cores caps the graph, and the range is the cap"
        );
        assert!(
            capped || range == (1, 16),
            "no count and no cap is the range 1..=16, got {range:?}"
        );
        let list: Vec<u32> = if capped { vec![0] } else { ns.clone() };
        // Blur pictures.
        for (w, h, sigma) in blur_cases() {
            let asset = noise_clip(w, h, "yuv420p");
            let tl = one_track(vec![blurred(&asset, sigma, 0.3)]);
            let case = format!("{mode} budget {budget}% blur {w}x{h} sigma {sigma}");
            let (mut refused_n, mut drawn_exact_n, mut drawn_none_n) = (0usize, 0usize, 0usize);
            for &n in &list {
                let ex = exported_frame(
                    &|s, t, a, o| export_at(n, s, t, a, o),
                    &format!("p4-{mode}-{budget}-{w}x{h}-{sigma}-{n}"),
                    &tl,
                    std::slice::from_ref(&asset),
                    30.0,
                    2,
                );
                match judge(&ex, &comp) {
                    Judged::Drawn(g) if g.iter().all(Option::is_none) => tally.exact += 1,
                    Judged::Drawn(g) => tally.failures.push(format!(
                        "{case} on {n} threads: the plan cleared it and the GPU differs: {}",
                        word(&g)
                    )),
                    Judged::Refused(why) => {
                        tally.refused += 1;
                        refused_n += 1;
                        if !why.iter().any(p3::is_plane_hazard) {
                            continue;
                        }
                        // Drawn anyway under the first thread count the plan clears it at.
                        let drawn = [1u32, 2, 3, 4, 5, 8, 16, 32, 64, 128].iter().find_map(|&t| {
                            let pinned = compositor(Some(t));
                            ex.plan
                                .reasons(&pinned.caps(), (ex.w as u32, ex.h as u32))
                                .is_empty()
                                .then(|| draw_anyway(&ex, &pinned))
                                .flatten()
                        });
                        match drawn {
                            Some(g) if g.iter().all(Option::is_none) => drawn_exact_n += 1,
                            Some(_) => {}
                            None => drawn_none_n += 1,
                        }
                    }
                }
            }
            if refused_n > 0 && drawn_exact_n == refused_n && drawn_none_n == 0 {
                let line = format!("{case}: refused on all {refused_n} runs and the draw anyway is exact on every one of them");
                // (A refusal is for the counts the plan cannot rule out: where the graph really runs on one, the one-thread frame is exact
                // by itself. Sigma 0 on a narrow plane is the kernel reading past the row, refused on purpose.)
                if split && sigma != 0.0 {
                    tally.unnecessary.push(line);
                } else {
                    tally.info.push(line);
                }
            }
        }
        // A blur on a layer placed in a larger canvas: the plane judged is the layer's own.
        {
            let base = noise_clip(64, 128, "yuv420p");
            let small = noise_clip(32, 128, "yuv420p");
            let mut pip = blurred(&small, 3.0, 0.3);
            pip.transform.scale = 0.4;
            let tl = Timeline {
                tracks: vec![
                    Track {
                        clips: vec![Clip::new(base.id, 0.0, 0.3, 0.0)],
                        ..Track::new(StreamKind::Video, "V1".to_string())
                    },
                    Track {
                        clips: vec![pip],
                        ..Track::new(StreamKind::Video, "V2".to_string())
                    },
                ],
                ..Timeline::new()
            };
            for &n in &list {
                let assets = [base.clone(), small.clone()];
                let ex = exported_frame(
                    &|s, t, a, o| export_at(n, s, t, a, o),
                    &format!("p4-{mode}-{budget}-pip-{n}"),
                    &tl,
                    &assets,
                    30.0,
                    2,
                );
                if let Judged::Drawn(g) = judge(&ex, &comp) {
                    if g.iter().all(Option::is_none) {
                        tally.exact += 1;
                    } else {
                        tally.failures.push(format!(
                            "{mode} budget {budget}% placed blur on {n} threads: the plan cleared it and the GPU differs: {}",
                            word(&g)
                        ));
                    }
                } else {
                    tally.refused += 1;
                }
            }
        }
        // A chroma key: refused where the count is a range, drawn and exact where the budget's cap is the count.
        for (w, h) in [(64u32, 64u32), (160, 96)] {
            let (tl, assets) = keyed_cut(w, h);
            for &n in &list {
                let ex = exported_frame(
                    &|s, t, a, o| export_at(n, s, t, a, o),
                    &format!("p4-{mode}-{budget}-key-{w}x{h}-{n}"),
                    &tl,
                    &assets,
                    30.0,
                    2,
                );
                let case = format!("{mode} budget {budget}% chroma key {w}x{h} on {n} threads");
                match judge(&ex, &comp) {
                    Judged::Drawn(g) if g.iter().all(Option::is_none) => {
                        tally.exact += 1;
                        if !capped {
                            tally
                                .failures
                                .push(format!("{case}: drawn where the slice-thread count is a range"));
                        }
                    }
                    Judged::Drawn(g) => tally
                        .failures
                        .push(format!("{case}: the plan cleared it and the GPU differs: {}", word(&g))),
                    Judged::Refused(why) => {
                        tally.refused += 1;
                        let known = why.iter().any(|u| u.to_string().contains("slice-thread count is not known"));
                        if capped {
                            tally
                                .failures
                                .push(format!("{case}: refused where the cap is the count: {why:?}"));
                        } else if !known {
                            tally
                                .failures
                                .push(format!("{case}: refused for another reason than the count: {why:?}"));
                        } else {
                            // The compositor itself draws no keyed layer at a count that is not one number.
                            if draw_anyway(&ex, &comp).is_some() {
                                tally
                                    .failures
                                    .push(format!("{case}: the compositor drew the keyed layer it was refused"));
                            }
                            // The refusal is necessary: drawn at any one count it differs from the export on some other count.
                            let mut differs = false;
                            for t in [1u32, 3, 5, 8, 16] {
                                let pinned = compositor(Some(t));
                                if let Some(g) = draw_anyway(&ex, &pinned) {
                                    differs |= g.iter().any(Option::is_some);
                                }
                            }
                            if !differs && n != 1 {
                                tally
                                    .unnecessary
                                    .push(format!("{case}: refused and every pinned count draws it exactly"));
                            }
                        }
                    }
                }
            }
        }
        // The render bar (the app's, judged for this compositor): a keyed span is the stream's where the count is a range and the GPU's
        // where a cap makes it one number.
        {
            let (tl, assets) = keyed_cut(64, 64);
            let request = kerf_core::render_bar::BarRequest {
                caps: Some(comp.caps()),
                width: 64,
                color: kerf_core::composite_color_policy(),
                media: &kerf_core::OriginalMedia,
                waits: Vec::new(),
                ask: None,
            };
            let bar = kerf_core::render_bar::render_bar(&tl, &assets, &request);
            let states: Vec<_> = bar.spans.iter().map(|sp| (sp.state, sp.kind.clone())).collect();
            let want = if capped {
                kerf_core::render_bar::BarState::Gpu
            } else {
                kerf_core::render_bar::BarState::Stream
            };
            if bar.spans.is_empty() || bar.spans.iter().any(|sp| sp.state != want) {
                tally.failures.push(format!(
                    "{mode} budget {budget}%: the render bar of a keyed cut is {states:?}, not all {want:?}"
                ));
            } else {
                eprintln!("CHECKER-P4 {mode}: budget {budget}% render bar of a keyed cut: {states:?}");
            }
        }
        // The key's own pass, asked directly: no alpha at a count that is not one number, the cap's where there is one.
        {
            let (tl, assets) = keyed_cut(64, 64);
            let opts = ExportOptions::default();
            let plan = kerf_core::RenderPlan::at(&tl, &assets, &opts, 0.1, kerf_core::composite_color_policy()).expect("plan");
            let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
            let frame = frames[1].as_ref().expect("the keyed layer's frame");
            let key = kerf_gpu::effects::ChromaKeyOptions {
                uv: kerf_gpu::effects::chroma_key_uv(kerf_core::color_rgb("0x808080").expect("colour")),
                similarity: 0.25,
                blend: 0.2,
            };
            let alpha = comp.chroma_key_alpha(frame, &key);
            if alpha.is_ok() != capped {
                tally.failures.push(format!(
                    "{mode} budget {budget}%: the key's alpha pass is {} where the cap is {capped}",
                    if alpha.is_ok() { "drawn" } else { "refused" }
                ));
            }
        }
        // Stills.
        if budget == 100 || !capped {
            for &n in &list {
                let reference = |tl: &Timeline, read: &[Asset], opts: &ExportOptions, t: f64, width: u32| {
                    still_at(n, tl, read, opts, t, width)
                };
                // Proxies keyed over proxies: FFmpeg's still runs on one slice thread (`single_frame_args`), the plan says so, so the key is
                // drawn at one and is exact.
                let (tl, originals) = keyed_cut(64, 64);
                let proxied: Vec<(Asset, Vec<PathBuf>)> = originals.iter().map(p3::proxy_of).collect();
                let reads: Vec<Asset> = proxied.iter().map(|(a, _)| a.clone()).collect();
                let case = format!("{mode} budget {budget}% still of proxies, chroma key, wrapper at {n}");
                match p3::still_outcome_with(&reference, &tl, (&originals, &reads), &kerf_core::ProxyMedia, 0.1) {
                    Outcome::Exact => tally.exact += 1,
                    other => tally.failures.push(format!("{case}: not drawn and exact: {other:?}")),
                }
                for (_, files) in &proxied {
                    for f in files {
                        let _ = std::fs::remove_file(f);
                    }
                }
                // Footage: the keyed still is refused with the range, the cleared blur stills are the still's.
                let (tl, assets) = keyed_cut(64, 64);
                let case = format!("{mode} budget {budget}% still of footage, chroma key, wrapper at {n}");
                match still_outcome_via(&reference, &tl, &assets, 0.1) {
                    Outcome::Other(why) if why.iter().any(|w| w.contains("slice-thread count is not known")) => {
                        tally.refused += 1;
                    }
                    other => tally
                        .failures
                        .push(format!("{case}: expected a refusal for the count: {other:?}")),
                }
                for (w, h, sigma) in [(64u32, 64u32, 3.0), (96, 64, 6.0), (66, 34, 3.0), (34, 34, 3.0)] {
                    let asset = noise_clip(w, h, "yuv420p");
                    let tl = one_track(vec![blurred(&asset, sigma, 0.3)]);
                    let case = format!("{mode} budget {budget}% still of footage, blur {w}x{h} sigma {sigma}, wrapper at {n}");
                    match still_outcome_via(&reference, &tl, std::slice::from_ref(&asset), 0.1) {
                        Outcome::Exact => tally.exact += 1,
                        Outcome::Refused { .. } => tally.refused += 1,
                        other => tally.failures.push(format!("{case}: {other:?}")),
                    }
                }
            }
        }
    }
    eprintln!(
        "CHECKER-P4 {mode}: {} frames drawn and exact, {} refused, {} failures, {} refusals the draw does not need",
        tally.exact,
        tally.refused,
        tally.failures.len(),
        tally.unnecessary.len()
    );
    for u in &tally.unnecessary {
        eprintln!("CHECKER-P4 {mode} UNNECESSARY: {u}");
    }
    for u in &tally.info {
        eprintln!("CHECKER-P4 {mode} (info) refusal the machine's own count would not need: {u}");
    }
    tally
        .failures
        .extend(tally.unnecessary.iter().map(|u| format!("unnecessary refusal: {u}")));
    assert!(
        tally.failures.is_empty(),
        "{} failures:\n{}",
        tally.failures.len(),
        tally.failures.join("\n")
    );
    assert!(
        tally.exact > 20 && tally.refused > 20,
        "the sweep should meet both draws and refusals"
    );
}

// ---- no other feature depends on the slice threads -----------------------------------------------------------------------------------------

fn lib_color() -> Color {
    Color {
        brightness: 0.08,
        contrast: 1.25,
        saturation: 1.4,
        gamma: 1.15,
        temperature: 0.3,
    }
}

/// `(name, control, timeline, assets)`: Kerf's features on 160x96 pictures, 0.4 s each.
fn sweep_cases() -> Vec<(String, bool, Timeline, Vec<Asset>)> {
    let a = noise_clip_secs(160, 96, 1.0);
    let t = lavfi_clip(
        "p4-testsrc-160x96-1s",
        "testsrc2=size=160x96:rate=30:duration=1,format=yuv420p",
    );
    let b = lavfi_clip("p4-bars-160x96-1s", "smptebars=size=160x96:rate=30:duration=1,format=yuv420p");
    let clip = |x: &Asset, at: f64| Clip::new(x.id, 0.2, 0.6, at);
    let tl = |clips: Vec<Clip>, fit: Option<Delivery>| Timeline {
        tracks: vec![Track {
            clips,
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        format: fit,
        ..Timeline::new()
    };
    let over = |base: Clip, top: Clip, fit: Option<Delivery>| Timeline {
        tracks: vec![
            Track {
                clips: vec![base],
                ..Track::new(StreamKind::Video, "V1".to_string())
            },
            Track {
                clips: vec![top],
                ..Track::new(StreamKind::Video, "V2".to_string())
            },
        ],
        format: fit,
        ..Timeline::new()
    };
    let narrow = noise_clip_secs(12, 64, 1.0);
    let assets = vec![a.clone(), t.clone(), b.clone(), narrow.clone()];
    let mut cases: Vec<(String, bool, Timeline, Vec<Asset>)> = Vec::new();
    let mut add =
        |name: &str, control: bool, timeline: Timeline| cases.push((name.to_string(), control, timeline, assets.clone()));
    // The controls: the two features that do depend on the count.
    {
        let mut key = clip(&a, 0.0);
        key.effects = vec![VideoEffect::ChromaKey {
            color: "0x808080".into(),
            similarity: 0.25,
            blend: 0.2,
        }];
        add("control chroma key over bars", true, over(clip(&b, 0.0), key, None));
    }
    {
        // 12 columns by 64 rows: the kernel's narrow-block and slice rules differ with the threads.
        let mut c = Clip::new(narrow.id, 0.2, 0.6, 0.0);
        c.effects = vec![VideoEffect::Blur { sigma: 3.0 }];
        add("control blur on a narrow picture", true, one_track(vec![c]));
    }
    let fx = |effects: Vec<VideoEffect>| {
        let mut c = clip(&a, 0.0);
        c.effects = effects;
        c
    };
    for (name, effects) in [
        ("blur 0.5", vec![VideoEffect::Blur { sigma: 0.5 }]),
        ("blur 6", vec![VideoEffect::Blur { sigma: 6.0 }]),
        ("blur 0", vec![VideoEffect::Blur { sigma: 0.0 }]),
        ("sharpen 1.5", vec![VideoEffect::Sharpen { amount: 1.5 }]),
        ("sharpen -1", vec![VideoEffect::Sharpen { amount: -1.0 }]),
        ("grayscale", vec![VideoEffect::Grayscale]),
        ("invert", vec![VideoEffect::Invert]),
        ("vignette", vec![VideoEffect::Vignette]),
        (
            "sharpen, blur, vignette, invert, grayscale",
            vec![
                VideoEffect::Sharpen { amount: 1.0 },
                VideoEffect::Blur { sigma: 2.0 },
                VideoEffect::Vignette,
                VideoEffect::Invert,
                VideoEffect::Grayscale,
            ],
        ),
    ] {
        add(&format!("effect {name}"), false, tl(vec![fx(effects)], None));
    }
    // Colour and grade.
    {
        let mut c = clip(&a, 0.0);
        c.color = lib_color();
        add("colour (eq, temperature)", false, tl(vec![c], None));
        let mut c = clip(&t, 0.0);
        c.grade = Grade {
            wheels: Some(Wheels {
                shadows: Wheel {
                    x: 0.3,
                    y: -0.2,
                    lightness: 0.1,
                },
                midtones: Wheel {
                    x: -0.2,
                    y: 0.4,
                    lightness: -0.05,
                },
                highlights: Wheel {
                    x: 0.1,
                    y: 0.1,
                    lightness: 0.2,
                },
            }),
            ..Grade::default()
        };
        add("grade wheels", false, tl(vec![c], None));
        let mut c = clip(&a, 0.0);
        c.grade = Grade {
            curves: Some(Curves {
                master: vec![
                    CurvePoint { x: 0.0, y: 0.05 },
                    CurvePoint { x: 0.5, y: 0.4 },
                    CurvePoint { x: 1.0, y: 0.95 },
                ],
                red: vec![
                    CurvePoint { x: 0.0, y: 0.0 },
                    CurvePoint { x: 0.6, y: 0.7 },
                    CurvePoint { x: 1.0, y: 1.0 },
                ],
                ..Curves::default()
            }),
            ..Grade::default()
        };
        add("grade curves", false, tl(vec![c], None));
        let mut c = clip(&t, 0.0);
        c.color = lib_color();
        c.grade = Grade {
            wheels: Some(Wheels {
                shadows: Wheel {
                    x: 0.2,
                    y: 0.2,
                    lightness: 0.0,
                },
                ..Wheels::default()
            }),
            curves: Some(Curves {
                master: vec![
                    CurvePoint { x: 0.0, y: 0.0 },
                    CurvePoint { x: 0.4, y: 0.5 },
                    CurvePoint { x: 1.0, y: 1.0 },
                ],
                ..Curves::default()
            }),
            ..Grade::default()
        };
        c.effects = vec![VideoEffect::Sharpen { amount: 0.8 }];
        add("colour, grade and sharpen together", false, tl(vec![c], None));
    }
    // Transforms and keyframes, alone and over a base.
    for (name, edit) in [
        (
            "scale 0.6",
            Box::new(|tr: &mut Transform| tr.scale = 0.6) as Box<dyn Fn(&mut Transform)>,
        ),
        ("scale 1.4", Box::new(|tr: &mut Transform| tr.scale = 1.4)),
        (
            "position",
            Box::new(|tr: &mut Transform| {
                tr.scale = 0.7;
                tr.pos_x = 0.15;
                tr.pos_y = -0.1;
            }),
        ),
        (
            "rotation 20",
            Box::new(|tr: &mut Transform| {
                tr.scale = 0.8;
                tr.rotation = 20.0;
            }),
        ),
        ("rotation 90", Box::new(|tr: &mut Transform| tr.rotation = 90.0)),
        (
            "opacity 0.5",
            Box::new(|tr: &mut Transform| {
                tr.scale = 0.8;
                tr.opacity = 0.5;
            }),
        ),
        (
            "crop",
            Box::new(|tr: &mut Transform| {
                tr.crop_left = 0.1;
                tr.crop_top = 0.2;
                tr.crop_right = 0.05;
            }),
        ),
    ] {
        let mut top = clip(&a, 0.0);
        edit(&mut top.transform);
        add(&format!("transform {name} over bars"), false, over(clip(&b, 0.0), top, None));
    }
    {
        let mut top = clip(&a, 0.0);
        let base = Transform {
            scale: 0.7,
            ..Transform::default()
        };
        top.transform = base;
        top.keyframes = vec![
            Keyframe {
                time: 0.0,
                scale: 0.5,
                pos_x: -0.2,
                pos_y: 0.1,
                rotation: 0.0,
                opacity: 0.4,
                ..Keyframe::from_transform(0.0, &base)
            },
            Keyframe {
                time: 0.4,
                scale: 1.1,
                pos_x: 0.2,
                pos_y: -0.1,
                rotation: 25.0,
                opacity: 1.0,
                ..Keyframe::from_transform(0.4, &base)
            },
        ];
        add(
            "keyframed move, turn and opacity over bars",
            false,
            over(clip(&b, 0.0), top, None),
        );
        let mut top = clip(&t, 0.0);
        let zoom = Transform::default();
        top.keyframes = vec![
            Keyframe {
                time: 0.0,
                scale: 1.0,
                ..Keyframe::from_transform(0.0, &zoom)
            },
            Keyframe {
                time: 0.4,
                scale: 1.5,
                ..Keyframe::from_transform(0.4, &zoom)
            },
        ];
        add("keyframed zoom alone", false, tl(vec![top], None));
    }
    // Fades.
    {
        let mut c = clip(&a, 0.0);
        c.fade_in = 0.15;
        c.fade_out = 0.15;
        add("fade in and out", false, tl(vec![c], None));
        let mut top = clip(&t, 0.0);
        top.fade_in = 0.2;
        top.transform.scale = 0.8;
        add("fade in over bars, scaled", false, over(clip(&b, 0.0), top, None));
    }
    // Transitions.
    for kind in [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::DipToWhite,
        TransitionKind::SlideLeft,
        TransitionKind::SlideRight,
        TransitionKind::SlideUp,
        TransitionKind::SlideDown,
        TransitionKind::PushLeft,
        TransitionKind::PushRight,
        TransitionKind::PushUp,
        TransitionKind::PushDown,
    ] {
        for scale in [1.0, 0.7] {
            let out = clip(&a, 0.0);
            let mut inc = clip(&t, 0.4);
            inc.transform.scale = scale;
            inc.transition_in = Some(Transition { kind, duration: 0.2 });
            add(
                &format!("transition {} scale {scale}", kind.as_str()),
                false,
                tl(vec![out, inc], None),
            );
        }
    }
    // Blend modes.
    for mode in BlendMode::ALL {
        let mut top = clip(&a, 0.0);
        top.blend_mode = mode;
        add(&format!("blend {}", mode.as_str()), false, over(clip(&t, 0.0), top, None));
    }
    // Masks.
    for (name, mask) in [
        (
            "rect",
            Mask {
                shape: MaskShape::Rect,
                x: 0.5,
                y: 0.5,
                width: 0.6,
                height: 0.5,
                feather: 0.15,
                inverted: false,
            },
        ),
        (
            "ellipse",
            Mask {
                shape: MaskShape::Ellipse,
                x: 0.4,
                y: 0.6,
                width: 0.5,
                height: 0.7,
                feather: 0.3,
                inverted: false,
            },
        ),
        (
            "inverted ellipse, hard",
            Mask {
                shape: MaskShape::Ellipse,
                x: 0.5,
                y: 0.5,
                width: 0.5,
                height: 0.5,
                feather: 0.0,
                inverted: true,
            },
        ),
    ] {
        let mut top = clip(&a, 0.0);
        top.mask = Some(mask);
        add(&format!("mask {name} over bars"), false, over(clip(&b, 0.0), top, None));
    }
    // Fits into other shapes (the blurred fill is `boxblur`).
    for fit in [Fit::Cover, Fit::Contain, Fit::Blur] {
        add(
            &format!("fit {fit:?} into 90x160"),
            false,
            tl(vec![clip(&t, 0.0)], Some(Delivery::new(90, 160, fit))),
        );
        add(&format!("fit {fit:?} into 200x120, zoomed"), false, {
            let mut c = clip(&a, 0.0);
            c.transform.scale = 1.2;
            tl(vec![c], Some(Delivery::new(200, 120, fit)))
        });
    }
    cases
}

/// The large set: 640x360 pictures into 960x540 and 540x960 frames, where a slice is tens of rows (the scaler's, the overlay's, the
/// transitions') and the sixteen threads of the last count each have rows to work on.
fn sweep_cases_large() -> Vec<(String, bool, Timeline, Vec<Asset>)> {
    let a = noise_clip_secs(640, 360, 1.0);
    let t = lavfi_clip(
        "p4-testsrc-640x360-1s",
        "testsrc2=size=640x360:rate=30:duration=1,format=yuv420p",
    );
    let b = lavfi_clip(
        "p4-bars-640x360-1s",
        "smptebars=size=640x360:rate=30:duration=1,format=yuv420p",
    );
    let assets = vec![a.clone(), t.clone(), b.clone()];
    let clip = |x: &Asset, at: f64| Clip::new(x.id, 0.2, 0.5, at);
    let frame = |w: u32, h: u32, fit: Fit| Some(Delivery::new(w, h, fit));
    let one = |c: Clip, fit: Option<Delivery>| Timeline {
        tracks: vec![Track {
            clips: vec![c],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        format: fit,
        ..Timeline::new()
    };
    let two = |base: Clip, top: Clip, fit: Option<Delivery>| Timeline {
        tracks: vec![
            Track {
                clips: vec![base],
                ..Track::new(StreamKind::Video, "V1".to_string())
            },
            Track {
                clips: vec![top],
                ..Track::new(StreamKind::Video, "V2".to_string())
            },
        ],
        format: fit,
        ..Timeline::new()
    };
    let mut cases: Vec<(String, bool, Timeline, Vec<Asset>)> = Vec::new();
    {
        let mut key = clip(&a, 0.0);
        key.effects = vec![VideoEffect::ChromaKey {
            color: "0x808080".into(),
            similarity: 0.25,
            blend: 0.2,
        }];
        cases.push((
            "control chroma key over bars".to_string(),
            true,
            two(clip(&b, 0.0), key, None),
            assets.clone(),
        ));
    }
    let mut add = |name: &str, timeline: Timeline| cases.push((name.to_string(), false, timeline, assets.clone()));
    for fit in [Fit::Cover, Fit::Contain, Fit::Blur] {
        add(
            &format!("large fit {fit:?} testsrc into 960x540"),
            one(clip(&t, 0.0), frame(960, 540, fit)),
        );
        add(
            &format!("large fit {fit:?} noise into 540x960"),
            one(clip(&a, 0.0), frame(540, 960, fit)),
        );
        add(
            &format!("large fit {fit:?} noise into 321x181"),
            one(clip(&a, 0.0), frame(322, 182, fit)),
        );
    }
    for (name, edit) in [
        (
            "scale 1.4",
            Box::new(|tr: &mut Transform| tr.scale = 1.4) as Box<dyn Fn(&mut Transform)>,
        ),
        ("scale 0.6", Box::new(|tr: &mut Transform| tr.scale = 0.6)),
        (
            "rotation 20",
            Box::new(|tr: &mut Transform| {
                tr.scale = 0.8;
                tr.rotation = 20.0;
            }),
        ),
        (
            "position and opacity",
            Box::new(|tr: &mut Transform| {
                tr.scale = 0.7;
                tr.pos_x = 0.2;
                tr.pos_y = 0.1;
                tr.opacity = 0.6;
            }),
        ),
    ] {
        let mut top = clip(&a, 0.0);
        edit(&mut top.transform);
        add(
            &format!("large transform {name} over bars in 960x540"),
            two(clip(&b, 0.0), top, frame(960, 540, Fit::Cover)),
        );
    }
    {
        let mut top = clip(&t, 0.0);
        let zoom = Transform::default();
        top.keyframes = vec![
            Keyframe {
                time: 0.0,
                scale: 1.0,
                ..Keyframe::from_transform(0.0, &zoom)
            },
            Keyframe {
                time: 0.3,
                scale: 1.6,
                pos_x: 0.1,
                rotation: 10.0,
                ..Keyframe::from_transform(0.3, &zoom)
            },
        ];
        add(
            "large keyframed zoom and turn into 960x540",
            one(top, frame(960, 540, Fit::Cover)),
        );
    }
    for kind in [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::DipToWhite,
        TransitionKind::SlideLeft,
        TransitionKind::SlideUp,
        TransitionKind::PushRight,
        TransitionKind::PushDown,
    ] {
        let out = clip(&a, 0.0);
        let mut inc = clip(&t, 0.3);
        inc.transform.scale = 0.7;
        inc.transition_in = Some(Transition { kind, duration: 0.2 });
        add(
            &format!("large transition {} into 960x540", kind.as_str()),
            Timeline {
                tracks: vec![Track {
                    clips: vec![out, inc],
                    ..Track::new(StreamKind::Video, "V1".to_string())
                }],
                format: frame(960, 540, Fit::Cover),
                ..Timeline::new()
            },
        );
    }
    for (name, effects) in [
        ("sharpen 1.5", vec![VideoEffect::Sharpen { amount: 1.5 }]),
        ("blur 6", vec![VideoEffect::Blur { sigma: 6.0 }]),
        ("vignette, grayscale", vec![VideoEffect::Vignette, VideoEffect::Grayscale]),
        ("invert", vec![VideoEffect::Invert]),
    ] {
        let mut c = clip(&a, 0.0);
        c.effects = effects;
        add(&format!("large effect {name}"), one(c, None));
    }
    for mode in [BlendMode::Multiply, BlendMode::Overlay, BlendMode::Difference, BlendMode::Add] {
        let mut top = clip(&a, 0.0);
        top.blend_mode = mode;
        add(&format!("large blend {}", mode.as_str()), two(clip(&t, 0.0), top, None));
    }
    {
        let mut top = clip(&a, 0.0);
        top.mask = Some(Mask {
            shape: MaskShape::Ellipse,
            x: 0.4,
            y: 0.6,
            width: 0.5,
            height: 0.7,
            feather: 0.3,
            inverted: false,
        });
        add("large mask ellipse over bars", two(clip(&b, 0.0), top, None));
        let mut c = clip(&t, 0.0);
        c.color = lib_color();
        c.grade = Grade {
            wheels: Some(Wheels {
                shadows: Wheel {
                    x: 0.3,
                    y: -0.2,
                    lightness: 0.1,
                },
                ..Wheels::default()
            }),
            ..Grade::default()
        };
        add("large colour and wheels", one(c, None));
        let mut c = clip(&a, 0.0);
        c.fade_in = 0.12;
        c.fade_out = 0.12;
        add("large fades", one(c, frame(960, 540, Fit::Cover)));
    }
    cases
}

/// The tiny set: pictures whose planes have fewer rows than the sixteen threads have slices (some slices are empty), narrow ones, and
/// ones a few samples across: the sizes where a filter's slice arithmetic meets its edges.
fn sweep_cases_tiny() -> Vec<(String, bool, Timeline, Vec<Asset>)> {
    let mut cases: Vec<(String, bool, Timeline, Vec<Asset>)> = Vec::new();
    for (w, h) in [(16u32, 16u32), (34, 20), (64, 6), (12, 64), (8, 8), (20, 4)] {
        let a = noise_clip_secs(w, h, 1.0);
        let t = lavfi_clip(
            &format!("p4-testsrc-{w}x{h}-1s"),
            &format!("testsrc2=size={w}x{h}:rate=30:duration=1,format=yuv420p"),
        );
        let b = lavfi_clip(
            &format!("p4-bars-{w}x{h}-1s"),
            &format!("smptebars=size={w}x{h}:rate=30:duration=1,format=yuv420p"),
        );
        let assets = vec![a.clone(), t.clone(), b.clone()];
        let clip = |x: &Asset, at: f64| Clip::new(x.id, 0.2, 0.5, at);
        let one = |c: Vec<Clip>| Timeline {
            tracks: vec![Track {
                clips: c,
                ..Track::new(StreamKind::Video, "V1".to_string())
            }],
            ..Timeline::new()
        };
        let two = |base: Clip, top: Clip| Timeline {
            tracks: vec![
                Track {
                    clips: vec![base],
                    ..Track::new(StreamKind::Video, "V1".to_string())
                },
                Track {
                    clips: vec![top],
                    ..Track::new(StreamKind::Video, "V2".to_string())
                },
            ],
            ..Timeline::new()
        };
        let size = format!("{w}x{h}");
        let mut add = |name: &str, control: bool, timeline: Timeline| {
            cases.push((format!("tiny {size} {name}"), control, timeline, assets.clone()));
        };
        if (w, h) == (16, 16) {
            let mut key = clip(&a, 0.0);
            key.effects = vec![VideoEffect::ChromaKey {
                color: "0x808080".into(),
                similarity: 0.25,
                blend: 0.2,
            }];
            add("control chroma key over bars", true, two(clip(&b, 0.0), key));
        }
        if (w, h) == (12, 64) {
            let mut c = clip(&a, 0.0);
            c.effects = vec![VideoEffect::Blur { sigma: 3.0 }];
            add("control blur", true, one(vec![c]));
        }
        for (name, effects) in [
            ("sharpen", vec![VideoEffect::Sharpen { amount: 1.5 }]),
            ("grayscale, invert", vec![VideoEffect::Grayscale, VideoEffect::Invert]),
            ("vignette", vec![VideoEffect::Vignette]),
        ] {
            let mut c = clip(&a, 0.0);
            c.effects = effects;
            add(&format!("effect {name}"), false, one(vec![c]));
        }
        {
            let mut c = clip(&t, 0.0);
            c.color = lib_color();
            c.fade_in = 0.1;
            c.fade_out = 0.1;
            add("colour and fades", false, one(vec![c]));
            let mut top = clip(&a, 0.0);
            top.transform.scale = 0.6;
            top.transform.rotation = 20.0;
            top.transform.opacity = 0.6;
            add("scaled, turned, translucent over bars", false, two(clip(&b, 0.0), top));
            let mut top = clip(&a, 0.0);
            top.blend_mode = BlendMode::Multiply;
            add("multiply over testsrc", false, two(clip(&t, 0.0), top));
            let mut top = clip(&a, 0.0);
            top.mask = Some(Mask {
                shape: MaskShape::Ellipse,
                x: 0.5,
                y: 0.5,
                width: 0.6,
                height: 0.6,
                feather: 0.2,
                inverted: false,
            });
            add("ellipse mask over bars", false, two(clip(&b, 0.0), top));
        }
        for kind in [
            TransitionKind::Crossfade,
            TransitionKind::DipToBlack,
            TransitionKind::SlideLeft,
            TransitionKind::PushUp,
        ] {
            let out = clip(&a, 0.0);
            let mut inc = clip(&t, 0.3);
            inc.transition_in = Some(Transition { kind, duration: 0.2 });
            add(&format!("transition {}", kind.as_str()), false, one(vec![out, inc]));
        }
    }
    cases
}

fn sweep_child() {
    let n: u32 = std::env::var("CHECKER_P4_N").expect("N").parse().expect("a number");
    let out = PathBuf::from(std::env::var("CHECKER_P4_OUT").expect("the output directory"));
    std::fs::create_dir_all(&out).unwrap();
    let cases = match std::env::var("CHECKER_P4_SET").as_deref() {
        Ok("large") => sweep_cases_large(),
        Ok("tiny") => sweep_cases_tiny(),
        _ => sweep_cases(),
    };
    eprintln!("CHECKER-P4 sweep: {} cases on {n} slice threads", cases.len());
    for (i, (name, _, timeline, assets)) in cases.iter().enumerate() {
        let opts = ExportOptions {
            fps: Some(30.0),
            ..ExportOptions::default()
        };
        let slug = format!("p4-sweep-{n}-{i}");
        let (w, h, yuv) = with_threads(n, || p3::export(&slug, timeline, assets, &opts));
        std::fs::write(out.join(format!("{i}.yuv")), &yuv).unwrap();
        std::fs::write(out.join(format!("{i}.txt")), format!("{w} {h} {name}")).unwrap();
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct Differ {
    plane: &'static str,
    frame: usize,
    n: usize,
    max: i32,
    first: (usize, usize),
}

fn compare(w: usize, h: usize, a: &[u8], b: &[u8]) -> Option<Differ> {
    if a.len() != b.len() {
        return Some(Differ {
            plane: "length",
            frame: 0,
            n: a.len().abs_diff(b.len()),
            max: 0,
            first: (0, 0),
        });
    }
    let frame_len = w * h * 3 / 2;
    let mut found: Option<Differ> = None;
    for (frame, (fa, fb)) in a.chunks(frame_len).zip(b.chunks(frame_len)).enumerate() {
        for (plane, range, pw) in [
            ("Y", 0..w * h, w),
            ("U", w * h..w * h + w * h / 4, w / 2),
            ("V", w * h + w * h / 4..frame_len, w / 2),
        ] {
            for (i, (x, y)) in fa[range.clone()].iter().zip(&fb[range]).enumerate() {
                if x != y {
                    let d = found.get_or_insert(Differ {
                        plane,
                        frame,
                        n: 0,
                        max: 0,
                        first: (i % pw, i / pw),
                    });
                    d.n += 1;
                    d.max = d.max.max((i32::from(*x) - i32::from(*y)).abs());
                }
            }
        }
        if found.is_some() {
            return found;
        }
    }
    None
}

// ---- the budget's cap against the arguments ------------------------------------------------------------------------------------------------

const PERCENTS: [u8; 6] = [10, 25, 50, 75, 90, 100];

fn budget_child() {
    let stages: Vec<String> = std::env::var("CHECKER_P4_STAGES")
        .unwrap_or_else(|_| "0-3;0;0,1;0-2;0-3;1-2".into())
        .split(';')
        .map(str::to_string)
        .collect();
    let how = std::env::var("CHECKER_P4_HOW").unwrap_or_else(|_| "thread".into());
    let asset = noise_clip(64, 64, "yuv420p");
    let tl = one_track(vec![blurred(&asset, 3.0, 0.3)]);
    let (keyed, keyed_assets) = keyed_cut(64, 64);
    let _ = gpu();
    let _ = compositor(None);
    for (stage, cpus) in stages.iter().enumerate() {
        if stage > 0 {
            if how == "thread" {
                narrow_thread(cpus);
            } else {
                let dir = std::env::var("CHECKER_P4_CPUSET").expect("the cpuset group");
                std::fs::write(Path::new(&dir).join("cpuset.cpus"), cpus).expect("resize the cpuset");
            }
        }
        for percent in PERCENTS {
            kerf_core::set_cpu_percent(percent);
            let (cores, count, range) = (
                kerf_core::cpu_cores(),
                kerf_core::ffmpeg_cpu_count(),
                kerf_core::cpu_graph_thread_range(),
            );
            let slug = format!("p4-bud-{stage}-{percent}");
            let _ = p3::export(&slug, &tl, std::slice::from_ref(&asset), &ExportOptions::default());
            eprintln!(
                "CHECKER-P4 budget: slug {slug} stage {stage} cpus {cpus} percent {percent} cores {cores} count {} range {}-{}",
                count.map_or(-1, |c| c as i64),
                range.0,
                range.1
            );
            // End to end: the keyed frame (drawn at the plan's one number or refused) and a blur, against FFmpeg's export.
            if matches!(percent, 25 | 75 | 100) {
                let key = motion_frame(&format!("p4-budkey-{stage}-{percent}"), &keyed, &keyed_assets, 30.0, 2);
                let blur = motion_frame(
                    &format!("p4-budblur-{stage}-{percent}"),
                    &tl,
                    std::slice::from_ref(&asset),
                    30.0,
                    2,
                );
                eprintln!(
                    "CHECKER-P4 budget e2e: stage {stage} cpus {cpus} percent {percent} cores {cores} key {} blur {}",
                    key.word(),
                    blur.word()
                );
            }
        }
    }
}

/// `threads_for`, written out here: the share of `cores` at `percent`, at least one and at most all.
fn share(cores: usize, percent: u8) -> usize {
    ((cores as f64 * f64::from(percent) / 100.0).round()).clamp(1.0, cores as f64) as usize
}

/// FFmpeg's graph threads for `count` CPUs with no flag (`avpriv_slicethread_create`).
fn default_threads(count: usize) -> usize {
    if count > 1 {
        (count + 1).min(16)
    } else {
        1
    }
}

fn field<'a>(line: &'a str, key: &str) -> &'a str {
    line.split(&format!("{key} "))
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or("?")
}

fn kv<'a>(line: &'a str, key: &str) -> &'a str {
    line.split(&format!("{key}="))
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or("?")
}

fn cpus_in(list: &str) -> usize {
    list.split(',')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('-') {
            Some((a, b)) => b.parse::<usize>().unwrap() - a.parse::<usize>().unwrap() + 1,
            None => 1,
        })
        .sum()
}

/// Reads the child's lines and the wrapper's log and holds each (stage, percent) to: the cap the engine wrote into FFmpeg's arguments is the
/// range the plan gave, and an uncapped graph is FFmpeg's own default for the cores FFmpeg itself detected.
fn check_budget(text: &str, log: &str, max_cores: Option<usize>) -> (usize, Vec<String>) {
    let mut failures = Vec::new();
    let mut checked = 0;
    let calls: Vec<&str> = log.lines().collect();
    for line in text.lines().filter(|l| l.starts_with("CHECKER-P4 budget: slug")) {
        let (slug, percent) = (field(line, "slug"), field(line, "percent").parse::<u8>().unwrap());
        let (cores, count): (usize, i64) = (field(line, "cores").parse().unwrap(), field(line, "count").parse().unwrap());
        let range: (usize, usize) = {
            let r = field(line, "range");
            let (a, b) = r.split_once('-').unwrap();
            (a.parse().unwrap(), b.parse().unwrap())
        };
        let needle = format!("/{slug}.mkv");
        let export_calls: Vec<&&str> = calls
            .iter()
            .filter(|c| c.contains(&needle) && c.contains("-filter_complex"))
            .collect();
        let Some(call) = export_calls.last() else {
            failures.push(format!("{line}: no export call in the wrapper's log"));
            continue;
        };
        let flag = |name: &str| -> Option<usize> {
            let args = call.split("args=").nth(1).unwrap_or("");
            args.split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .find(|w| w[0] == name)
                .and_then(|w| w[1].parse().ok())
        };
        let detected: usize = kv(call, "detected").parse().unwrap_or(0);
        let allowed = cpus_in(kv(call, "allowed"));
        if detected != allowed {
            failures.push(format!(
                "{line}: FFmpeg detects {detected} where its affinity lists {allowed}"
            ));
        }
        if let Some(m) = max_cores {
            if cores > m || cores == 0 {
                failures.push(format!("{line}: cores {cores} above the {m} the cgroup lets"));
            }
        }
        let budget = share(cores, percent);
        let capped = percent < 100 && budget < cores;
        let (ft, fct) = (flag("-filter_threads"), flag("-filter_complex_threads"));
        if capped {
            if ft != Some(budget) || fct != Some(budget) || range != (budget, budget) {
                failures.push(format!(
                    "{line}: capped at {budget} of {cores} cores, the engine wrote -filter_threads {ft:?} -filter_complex_threads {fct:?} and the plan says {range:?}"
                ));
            }
        } else {
            let d = default_threads(detected);
            if ft.is_some() || fct.is_some() || range != (d, d) || count != detected as i64 {
                failures.push(format!(
                    "{line}: not capped ({budget} of {cores} cores), FFmpeg detects {detected} and runs on {d}; the engine wrote {ft:?}/{fct:?}, the plan says {range:?}, the count it reproduces is {count}"
                ));
            }
        }
        checked += 1;
    }
    for line in text.lines().filter(|l| l.starts_with("CHECKER-P4 budget e2e:")) {
        if line.contains(" key DIFFERS") || line.contains(" blur DIFFERS") {
            failures.push(line.to_string());
        }
    }
    (checked, failures)
}

fn budget_run(label: &str, procs: &[PathBuf], mask: Option<&str>, extra: &[(&str, String)], max_cores: Option<usize>) {
    let dir = p3::scratch("checker-p4").join(format!("wrap-{}-{label}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("calls.log");
    let _ = std::fs::remove_file(&log);
    let wrapper = write_wrapper(&dir, &kerf_core::ffmpeg_path());
    let mut envs: Vec<(&str, String)> = vec![
        (CHILD, "budget".into()),
        ("P3_LOG", log.to_string_lossy().into_owned()),
        ("KERF_FFMPEG", wrapper.to_string_lossy().into_owned()),
    ];
    envs.extend(extra.iter().map(|(k, v)| (*k, v.clone())));
    let (ok, text) = rerun_in("checker_p4_child", procs, mask, &envs, LIMIT);
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(ok, "{label}: the child failed:\n{text}");
    let (checked, failures) = check_budget(&text, &calls, max_cores);
    for l in text.lines().filter(|l| l.starts_with("CHECKER-P4 budget")) {
        eprintln!("{label}: {l}");
    }
    eprintln!("CHECKER-P4 budget {label}: {checked} (affinity, budget) pairs held to the engine's arguments");
    assert!(checked >= 18, "{label}: too few pairs checked ({checked}):\n{text}");
    assert!(
        failures.is_empty(),
        "{label}: {} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the children --------------------------------------------------------------------------------------------------------------------------

#[test]
#[ignore = "run by the parents below, in a process of its own"]
fn checker_p4_child() {
    match std::env::var(CHILD).as_deref() {
        Ok("split") => unknown_count(true),
        Ok("failed") => unknown_count(false),
        Ok("sweep") => sweep_child(),
        Ok("budget") => budget_child(),
        _ => {}
    }
}

fn real_wrapper(tag: &str) -> (PathBuf, PathBuf) {
    let dir = p3::scratch("checker-p4").join(format!("wrap-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wrapper = write_wrapper(&dir, &kerf_core::ffmpeg_path());
    (dir, wrapper)
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p4_range_a_count_the_plan_does_not_know_is_a_range_nothing_assumes_one_number_of() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, wrapper) = real_wrapper("split");
    let (ok, text) = rerun_in(
        "checker_p4_child",
        &[],
        None,
        &[
            (CHILD, "split".into()),
            ("KERF_CPU_PERCENT", "100".into()),
            ("KERF_FFMPEG", wrapper.to_string_lossy().into_owned()),
        ],
        LIMIT,
    );
    for l in text.lines().filter(|l| l.starts_with("CHECKER-P4")) {
        eprintln!("{l}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "the child failed:\n{text}");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p4_range_a_failed_affinity_read_is_the_one_thread_graph() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (ok, text) = rerun_in(
        "checker_p4_child",
        &[],
        None,
        &[(CHILD, "failed".into()), ("KERF_CPU_PERCENT", "100".into())],
        LIMIT,
    );
    for l in text.lines().filter(|l| l.starts_with("CHECKER-P4")) {
        eprintln!("{l}");
    }
    assert!(ok, "the child failed:\n{text}");
}

fn sweep_driver(set: &str, counts: &[u32], min_cases: usize) {
    let (dir, wrapper) = real_wrapper(&format!("sweep-{set}"));
    let out = p3::scratch("checker-p4").join(format!("sweep-{set}-{}", std::process::id()));
    for &n in counts {
        let (ok, text) = rerun_in(
            "checker_p4_child",
            &[],
            None,
            &[
                (CHILD, "sweep".into()),
                ("KERF_CPU_PERCENT", "100".into()),
                ("KERF_FFMPEG", wrapper.to_string_lossy().into_owned()),
                ("CHECKER_P4_N", n.to_string()),
                ("CHECKER_P4_SET", set.to_string()),
                ("CHECKER_P4_OUT", out.join(n.to_string()).to_string_lossy().into_owned()),
            ],
            LIMIT,
        );
        assert!(ok, "the child on {n} threads failed:\n{text}");
    }
    let names = |n: u32| -> Vec<(usize, usize, String)> {
        let mut v = Vec::new();
        for i in 0.. {
            let Ok(t) = std::fs::read_to_string(out.join(n.to_string()).join(format!("{i}.txt"))) else {
                break;
            };
            let mut parts = t.splitn(3, ' ');
            v.push((
                parts.next().unwrap().parse().unwrap(),
                parts.next().unwrap().parse().unwrap(),
                parts.next().unwrap().to_string(),
            ));
        }
        v
    };
    let cases = names(counts[0]);
    let controls: Vec<&str> = cases
        .iter()
        .filter(|c| c.2.contains("control"))
        .map(|c| c.2.as_str())
        .collect();
    assert!(!controls.is_empty(), "the set has no control");
    let (mut failures, mut seen_controls, mut same) = (Vec::new(), BTreeMap::new(), 0usize);
    for (i, (w, h, name)) in cases.iter().enumerate() {
        let first = std::fs::read(out.join(counts[0].to_string()).join(format!("{i}.yuv"))).unwrap();
        let mut differing = Vec::new();
        for &n in &counts[1..] {
            let other = std::fs::read(out.join(n.to_string()).join(format!("{i}.yuv"))).unwrap();
            if let Some(d) = compare(*w, *h, &first, &other) {
                differing.push(format!("{} vs {n} threads: {d:?}", counts[0]));
            }
        }
        if controls.contains(&name.as_str()) {
            eprintln!(
                "CHECKER-P4 sweep {set}: control {name}: differs on {} of {} counts",
                differing.len(),
                counts.len() - 1
            );
            seen_controls.insert(name.clone(), differing.len());
        } else if differing.is_empty() {
            same += 1;
        } else {
            failures.push(format!("{name}: depends on the slice threads: {}", differing.join("; ")));
        }
    }
    eprintln!(
        "CHECKER-P4 sweep {set}: {same} of {} features identical on {} slice-thread counts",
        cases.len() - controls.len(),
        counts.len()
    );
    let _ = std::fs::remove_dir_all(&out);
    let _ = std::fs::remove_dir_all(&dir);
    for c in &controls {
        assert!(
            seen_controls.get(*c).copied().unwrap_or(0) > 0,
            "the control `{c}` did not differ on any thread count: the sweep cannot see a dependence"
        );
    }
    assert!(cases.len() >= min_cases, "{} cases", cases.len());
    assert!(
        failures.is_empty(),
        "{} features depend on the slice threads:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p4_range_no_other_feature_depends_on_the_slice_threads() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    sweep_driver("small", &[1, 2, 3, 5, 8, 16], 60);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p4_range_no_other_feature_depends_on_the_slice_threads_on_large_pictures() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    sweep_driver("large", &[1, 2, 5, 16], 30);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p4_range_no_other_feature_depends_on_the_slice_threads_on_tiny_pictures() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    sweep_driver("tiny", &[1, 2, 3, 5, 8, 16], 60);
}

#[test]
#[ignore = "needs ffmpeg, taskset and a GPU adapter (lavapipe is enough)"]
fn checker_p4_budget_the_cap_is_the_arguments_after_every_change_of_the_threads_affinity() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !p3::have_taskset() {
        eprintln!("CHECKER-P4 budget: no taskset on this host: not run");
        return;
    }
    budget_run("taskset", &[], Some("0-3"), &[("CHECKER_P4_HOW", "thread".into())], None);
}

#[test]
#[ignore = "needs ffmpeg, a writable cpuset cgroup and a GPU adapter (lavapipe is enough)"]
fn checker_p4_budget_the_cap_is_the_arguments_after_a_cpuset_is_resized_under_the_process() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(set) = cpuset_group("0-3") else {
        eprintln!("CHECKER-P4 budget: no writable cpuset cgroup on this host: not run");
        return;
    };
    budget_run(
        "cpuset",
        &[set.procs()],
        None,
        &[
            ("CHECKER_P4_HOW", "cpuset".into()),
            ("CHECKER_P4_CPUSET", set.dir.to_string_lossy().into_owned()),
        ],
        None,
    );
}

#[test]
#[ignore = "needs ffmpeg, a writable cpu cgroup and a GPU adapter (lavapipe is enough)"]
fn checker_p4_budget_the_cap_is_the_arguments_under_a_cpu_quota_ffmpeg_does_not_apply() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut ran = 0;
    for quota in [1.0, 2.0, 3.0, 1.5, 2.5] {
        let Some(group) = quota_group(quota) else {
            eprintln!("CHECKER-P4 budget: no writable cpu cgroup on this host: not run");
            return;
        };
        let max = quota.ceil() as usize;
        budget_run(
            &format!("quota{quota}"),
            &[group.procs()],
            Some("0-3"),
            &[
                ("CHECKER_P4_HOW", "thread".into()),
                ("CHECKER_P4_STAGES", "0-3;0-1;0-3".into()),
            ],
            Some(max),
        );
        ran += 1;
    }
    assert_eq!(ran, 5);
}
