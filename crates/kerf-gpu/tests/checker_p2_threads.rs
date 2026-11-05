//! `checker/p2/threads`: the slice threads the plan judges a blur over are the ones FFmpeg runs, measured on the real binary.
//!
//! `RenderPlan::graph_thread_range` is exact where `kerf_core::ffmpeg_cpu_count()` reproduces `av_cpu_count` (Linux:
//! `CPU_COUNT(sched_getaffinity(0))`, the same code in 6.1 and 9.0). Here that is held to FFmpeg's own graph, not to a formula: the
//! number of slice threads a graph ran on is **measured** from what `gblur` does with it, with no race and no model of the threads in
//! the way. A 12-wide picture of `15 * t` rows is a vector of 16 rows over a narrow plane for `t` slices exactly when `15 * t + 1` rows
//! is, so FFmpeg's output is the recursion's at `15 * t` rows and is not at `15 * t + 1` for one thread count `t` and no other (chroma, on
//! the 8-lane kernel: `14 * t` and `14 * t + 1`). Both graph kinds, at the engine's own thread flags (`limit_ffmpeg_args`).
//!
//! * `checker_p2_threads_the_plans_range_is_the_graph_ffmpeg_runs_under_every_affinity_and_budget`: the test binary re-run under
//!   `taskset -c 0`, `0,1`, `0-2` and `0-3` at budgets 100, 75, 50, 25 and 10 %: the plan's range is `(t, t)` for the `t` measured, which is
//!   `min(cpus + 1, 16)` (one on one cpu) uncapped and the budget's where the engine caps the graph.
//! * `checker_p2_threads_the_plan_follows_an_affinity_that_changes`: the process's affinity narrows after the plan first read it (a cpuset
//!   resize by a container runtime, `taskset -p` on a running process: what the kernel does to a task in either case is the same
//!   `cpus_allowed` change). FFmpeg, started afterwards, counts the new affinity. **The count is read at each judgement**
//!   (`ffmpeg_cpu_count`), so the plan follows it; the test asserts that the plan judges the threads FFmpeg runs on, and reports the picture
//!   that would be cleared and not the recursion if it did not.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_p2_threads -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr)]

use std::process::Command;
use std::time::{Duration, Instant};

use kerf_gpu::effects::{blur_plane, gaussian};

#[path = "common/child.rs"]
mod child;

const CHILD: &str = "CHECKER_P2_THREADS_CHILD";
const TIMEOUT: Duration = Duration::from_secs(60);

fn lcg(seed: u64, n: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2b99_17f3_0c5a_6d41;
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Graph {
    Simple,
    Complex,
}

type Planes = [Vec<u8>; 3];

/// `w` x `h` yuv420p noise through `gblur=sigma=3` in an argv the engine's thread caps have been spliced into (`limit_ffmpeg_args`: the
/// head flags and the encoder's, as `cpu::limit_args` writes them for an export) and no thread flag of this test's own: the input and the
/// output. `None` when FFmpeg would not run it.
fn blurred(w: usize, h: usize, graph: Graph, caps: bool) -> Option<(Planes, Planes)> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let planes = [
        lcg(1 + (w * 131 + h) as u64, w * h),
        lcg(2 + (w * 137 + h) as u64, cw * ch),
        lcg(3 + (w * 139 + h) as u64, cw * ch),
    ];
    let raw: Vec<u8> = planes.iter().flatten().copied().collect();
    let filter = kerf_core::blur_filter(3.0);
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-s",
    ]
    .map(String::from)
    .into_iter()
    .chain([format!("{w}x{h}"), "-i".into(), "pipe:0".into()])
    .collect();
    match graph {
        Graph::Simple => args.extend(["-vf".into(), filter]),
        Graph::Complex => args.extend([
            "-filter_complex".into(),
            format!("[0:v]{filter}[v]"),
            "-map".into(),
            "[v]".into(),
        ]),
    }
    args.extend(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"].map(String::from));
    if caps {
        kerf_core::limit_ffmpeg_args(&mut args, 1);
    }
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(&args);
    let out = kerf_core::run_piped_until(&mut cmd, raw, Instant::now() + TIMEOUT)?;
    if out.len() != w * h + 2 * cw * ch {
        return None;
    }
    let (y, rest) = out.split_at(w * h);
    let (u, v) = rest.split_at(cw * ch);
    Some((planes, [y.to_vec(), u.to_vec(), v.to_vec()]))
}

fn dims(w: usize, h: usize) -> [(usize, usize); 3] {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    [(w, h), (cw, ch), (cw, ch)]
}

/// Whether every plane of the picture is the recursion's (the model's).
fn is_the_recursion(w: usize, h: usize, graph: Graph, caps: bool) -> Option<bool> {
    let g = gaussian(3.0).expect("sigma 3");
    let (planes, got) = blurred(w, h, graph, caps)?;
    Some(
        dims(w, h)
            .into_iter()
            .enumerate()
            .all(|(k, (pw, ph))| got[k] == blur_plane(&planes[k], pw, ph, &g)),
    )
}

/// The kernel's vector width (a 12x16 luma plane on one explicit thread: 16 where it is not the model's, 8 where the 6x8 chroma plane is not).
fn kernel_lanes() -> u32 {
    let g = gaussian(3.0).expect("sigma 3");
    let raw: Vec<u8> = [
        lcg(1 + (12 * 131 + 16) as u64, 12 * 16),
        lcg(2 + (12 * 137 + 16) as u64, 48),
        lcg(3 + (12 * 139 + 16) as u64, 48),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-filter_threads",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
    ])
    .args(["-s", "12x16", "-i", "pipe:0", "-vf", &kerf_core::blur_filter(3.0)])
    .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"]);
    let out = kerf_core::run_piped_until(&mut cmd, raw.clone(), Instant::now() + TIMEOUT).expect("FFmpeg runs");
    let luma_exact = out[..192] == blur_plane(&raw[..192], 12, 16, &g)[..];
    let chroma_exact = out[192..240] == blur_plane(&raw[192..240], 6, 8, &g)[..];
    match (luma_exact, chroma_exact) {
        (false, true) => 16,
        (true, false) => 8,
        other => panic!("no AVX2 or AVX-512 gblur kernel here: {other:?}"),
    }
}

/// The slice-thread counts `t` in 1 to 16 whose boundary FFmpeg's default graph sits on: the picture of `lane_rows * t` rows is the
/// recursion and the one of one row more is not. One count, when the graph's threads are one of those.
fn measured(graph: Graph, caps: bool, lanes: u32) -> Vec<usize> {
    // 12 wide: luma is a vector of rows over a narrow plane for the 16-lane kernel at rows > 15 t; chroma is, for the 8-lane kernel, at
    // chroma rows > 7 t, i.e. luma rows > 14 t.
    let per = if lanes == 16 { 15 } else { 14 };
    (1..=16)
        .filter(|&t| {
            let ok = is_the_recursion(12, per * t, graph, caps);
            let bad = is_the_recursion(12, per * t + 1, graph, caps);
            ok == Some(true) && bad == Some(false)
        })
        .collect()
}

fn child_line() -> String {
    let lanes = kernel_lanes();
    let (lo, hi) = kerf_core::cpu_graph_thread_range();
    let count = kerf_core::ffmpeg_cpu_count().map_or(-1, |c| c as i64);
    let simple = measured(Graph::Simple, true, lanes);
    let complex = measured(Graph::Complex, true, lanes);
    format!(
        "CHECKER-P2 threads child: count={count} cores={} range={lo}-{hi} simple={simple:?} complex={complex:?} lanes={lanes}",
        kerf_core::cpu_cores()
    )
}

#[derive(Debug)]
struct Said {
    count: i64,
    cores: usize,
    range: (usize, usize),
    simple: Vec<usize>,
    complex: Vec<usize>,
}

fn parse(line: &str) -> Said {
    let get = |key: &str| {
        let at = line.find(&format!("{key}=")).unwrap_or_else(|| panic!("no {key} in {line}")) + key.len() + 1;
        let rest = &line[at..];
        let end = if rest.starts_with('[') {
            rest.find(']').expect("a closed list") + 1
        } else {
            rest.find(' ').unwrap_or(rest.len())
        };
        rest[..end].to_string()
    };
    let list = |s: String| -> Vec<usize> {
        s.trim_matches(|c| c == '[' || c == ']')
            .split(',')
            .filter(|p| !p.trim().is_empty())
            .map(|p| p.trim().parse().unwrap())
            .collect()
    };
    let (lo, hi) = get("range")
        .split_once('-')
        .map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap()))
        .unwrap();
    Said {
        count: get("count").parse().unwrap(),
        cores: get("cores").parse().unwrap(),
        range: (lo, hi),
        simple: list(get("simple")),
        complex: list(get("complex")),
    }
}

fn in_a_child(test: &str, taskset: &str, percent: u8, mode: &str) -> String {
    in_a_child_with(test, taskset, percent, mode, &[])
}

/// [`in_a_child`] with `env` added to the child's environment (which its FFmpeg children inherit).
fn in_a_child_with(test: &str, taskset: &str, percent: u8, mode: &str, env: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("the test binary");
    let mut cmd = Command::new("taskset");
    cmd.args(["-c", taskset]).arg(&exe);
    cmd.args([test, "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env(CHILD, mode)
        .env("KERF_CPU_PERCENT", percent.to_string());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = child::output_within(cmd, Duration::from_secs(600))
        .unwrap_or_else(|e| panic!("taskset {taskset} {percent}%: the child {e}"));
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(
        out.status.success(),
        "taskset {taskset} {percent}%: the child failed:\n{stdout}\n{stderr}"
    );
    stderr
        .lines()
        .chain(stdout.lines())
        .find(|l| l.contains("CHECKER-P2 threads child:"))
        .unwrap_or_else(|| panic!("the child said nothing:\n{stdout}\n{stderr}"))
        .to_string()
}

#[test]
#[ignore = "run by the parents below, in a process of its own"]
fn checker_p2_threads_the_child() {
    match std::env::var(CHILD).as_deref() {
        Ok("measure") => eprintln!("{}", child_line()),
        Ok("stale") => eprintln!("{}", stale_line()),
        _ => {}
    }
}

#[test]
#[ignore = "needs ffmpeg and taskset"]
fn checker_p2_threads_the_plans_range_is_the_graph_ffmpeg_runs_under_every_affinity_and_budget() {
    let mut failures = Vec::new();
    for (set, cpus) in [("0", 1usize), ("0,1", 2), ("0-2", 3), ("0-3", 4)] {
        for percent in [100u8, 75, 50, 25, 10] {
            let said = parse(&in_a_child("checker_p2_threads_the_child", set, percent, "measure"));
            // (The budget: `threads_for`, written out. A graph is capped where it is under 100 % and short of the cores.)
            let budget = ((cpus as f64 * f64::from(percent) / 100.0).round() as usize).clamp(1, cpus);
            let capped = percent < 100 && budget < cpus;
            let want = if capped {
                budget
            } else if cpus > 1 {
                (cpus + 1).min(16)
            } else {
                1
            };
            eprintln!("CHECKER-P2 threads: taskset {set} at {percent}%: {said:?}, want {want}");
            if said.count != cpus as i64 {
                failures.push(format!(
                    "taskset {set}: ffmpeg_cpu_count is {}, the affinity has {cpus}",
                    said.count
                ));
            }
            if said.cores != cpus {
                failures.push(format!("taskset {set}: cpu_cores is {}", said.cores));
            }
            if said.range != (want, want) {
                failures.push(format!(
                    "taskset {set} at {percent}%: the plan judges {:?}, FFmpeg's graph is {want}",
                    said.range
                ));
            }
            if said.simple != [want] {
                failures.push(format!(
                    "taskset {set} at {percent}%: the simple graph's measured threads are {:?}, not {want}",
                    said.simple
                ));
            }
            if said.complex != [want] {
                failures.push(format!(
                    "taskset {set} at {percent}%: the complex graph's measured threads are {:?}, not {want}",
                    said.complex
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

/// The stale child: reads the plan's range under `0-3`, narrows its own thread's affinity (from outside, as a cpuset resize does:
/// `taskset -p` on the thread), runs FFmpeg, and says what the plan judged and what FFmpeg did.
fn stale_line() -> String {
    let lanes = kernel_lanes();
    let before = kerf_core::cpu_graph_thread_range();
    let count_before = kerf_core::ffmpeg_cpu_count();
    let tid = std::fs::read_link("/proc/thread-self")
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .expect("the thread id");
    let moved = Command::new("taskset")
        .args(["-p", "-c", "0", &tid])
        .output()
        .expect("taskset");
    assert!(
        moved.status.success(),
        "taskset -p: {}",
        String::from_utf8_lossy(&moved.stderr)
    );
    let after = kerf_core::cpu_graph_thread_range();
    let ffmpeg_runs = measured(Graph::Simple, false, lanes);
    // A 12x64 picture: no plane is a hazard for the plan's range, and the one thread FFmpeg now runs on makes a vector of rows of it.
    let (lo, hi) = after;
    let hazard = kerf_core::gblur::picture_hazard((12, 64), lo as u32..=hi as u32, 0).is_some();
    let exact = is_the_recursion(12, 64, Graph::Simple, false);
    format!(
        "CHECKER-P2 threads child: count={} cores={} range={}-{} simple={ffmpeg_runs:?} complex=[] lanes={lanes} before={}-{} hazard={hazard} exact={exact:?}",
        count_before.map_or(-1, |c| c as i64),
        kerf_core::cpu_cores(),
        after.0,
        after.1,
        before.0,
        before.1
    )
}

#[test]
#[ignore = "needs ffmpeg and taskset"]
fn checker_p2_threads_the_plan_follows_an_affinity_that_changes() {
    let line = in_a_child("checker_p2_threads_the_child", "0-3", 100, "stale");
    eprintln!("{line}");
    let said = parse(&line);
    let judged_clear = line.contains("hazard=false");
    let drawn_exact = line.contains("exact=Some(true)");
    assert_eq!(
        said.simple,
        [1],
        "FFmpeg started after the affinity narrowed to one cpu runs its graph on one thread"
    );
    assert_eq!(
        said.range,
        (1, 1),
        "the plan judges {:?}, FFmpeg runs one thread; a 12x64 blur is then cleared by the plan ({judged_clear}) and the recursion on FFmpeg: {drawn_exact}",
        said.range
    );
}

/// The environment FFmpeg's own process is started in can change what it counts: the distro build links `libgomp` (through
/// `libavfilter`, `libavformat` and `libavdevice`), which binds the process's initial thread to the first place of `GOMP_CPU_AFFINITY`,
/// `OMP_PLACES` or `OMP_PROC_BIND` as it loads, so `sched_getaffinity` in FFmpeg reads **one cpu** where Kerf's own process, which does not
/// load `libgomp`, reads all of them. FFmpeg then runs its graph on one thread and the plan judges `min(cpus + 1, 16)`. The pinned build
/// links no `libgomp` and is unaffected. The test sets each variable in the environment Kerf is started in and holds the plan's range to the
/// threads FFmpeg measurably runs.
#[test]
#[ignore = "needs ffmpeg and taskset"]
fn checker_p2_threads_the_plan_follows_the_environment_the_openmp_runtime_reads() {
    let mut failures = Vec::new();
    for env in [
        ("OMP_PROC_BIND", "true"),
        ("OMP_PLACES", "cores"),
        ("GOMP_CPU_AFFINITY", "0-3"),
    ] {
        let said = parse(&in_a_child_with(
            "checker_p2_threads_the_child",
            "0-3",
            100,
            "measure",
            &[env],
        ));
        eprintln!("CHECKER-P2 threads: {env:?}: {said:?}");
        if said.simple != [said.range.0] || said.complex != [said.range.0] || said.range.0 != said.range.1 {
            failures.push(format!(
                "{env:?}: the plan judges {:?}, FFmpeg's graph runs {:?} (simple) and {:?} (complex) threads",
                said.range, said.simple, said.complex
            ));
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}
