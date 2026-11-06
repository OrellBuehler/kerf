//! `checker/p5/torn`: `graph_thread_range` under a cap (`stable_range`: FFmpeg's count read on both sides of the cores, the pair read again
//! until it is settled) held to one rule: **the range is one machine's answer, whatever the affinity does between the reads**; or, where
//! the machine never settles, the wide fallback `1..=max(16, the most cores read)`.
//!
//! * `checker_p5_torn_a_scripted_machine_*`: the affinity is **scripted read by read** (an `LD_PRELOAD` shim answers each
//!   `sched_getaffinity` of the test process with the next CPU count of a script, `x` is a failing call, and the last entry repeats), so a
//!   flipper tuned to the read timing is the limit case of an adversary: it changes the machine at exactly the instants of its choosing.
//!   Steady state, one flip, a flip and flip back inside the settle window, alternations of every phase, every cycle of one to four CPUs up
//!   to length six at every phase, the failed call, and the retry exhaustion are tried against the real function. The reads a call
//!   uses are counted (the hot path: 1 at 100 %, 3 under a cap on a machine that does not change).
//! * `checker_p5_torn_a_scripted_machine_drives_the_plan_*`: the plan's verdicts on a keyed and a blurred layer under the scripts: the key
//!   is refused where the range is a range (the fallback included) and drawn only at a count that is one machine's.
//! * `checker_p5_torn_real_flippers_*`: another thread changes the reader's affinity with `sched_setaffinity` as fast as it can, in bursts
//!   timed from the reader's progress, between two, three and four masks.
//! * `checker_p5_torn_a_cgroup_*`: a cpuset resized under the process while a cgroup CPU quota binds.
//!
//! ```text
//! cargo test -p kerf-core --no-default-features --test checker_p5_torn -- --ignored --nocapture
//! ```
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use kerf_core::{
    Asset, Clip, EffectKinds, ExportOptions, GpuCaps, PlanRequest, Planner, RenderPlan, StreamInfo, StreamKind, Timeline, Track,
    VideoEffect,
};
use uuid::Uuid;

const CHILD: &str = "CHECKER_P5_CHILD";

// ---- the answers of one machine, written out here -------------------------------------------------------------------------------------------

/// FFmpeg's default slice threads for `count` CPUs.
fn default_threads(count: usize) -> usize {
    if count > 1 {
        (count + 1).min(16)
    } else {
        1
    }
}

/// `threads_for`: the share of `cores` at `percent`, at least one and at most all.
fn share(cores: usize, percent: u8) -> usize {
    ((cores as f64 * f64::from(percent) / 100.0).round()).clamp(1.0, cores as f64) as usize
}

/// The range a machine of `count` CPUs under a quota of `quota` CPUs gives at `percent` (a cap): the budget's where it caps, else FFmpeg's
/// default for the count.
fn machine(count: usize, quota: usize, percent: u8) -> (usize, usize) {
    let cores = count.min(quota);
    let budget = share(cores, percent);
    if budget < cores {
        (budget, budget)
    } else {
        (default_threads(count), default_threads(count))
    }
}

// ---- the shim ----------------------------------------------------------------------------------------------------------------------------------

const SHIM: &str = r#"
#define _GNU_SOURCE
#include <errno.h>
#include <dlfcn.h>
#include <sched.h>
#include <stdlib.h>
#include <string.h>
static int (*real_fn)(pid_t, size_t, cpu_set_t *);
static unsigned long calls, pos;
static int values[4096];
static int n_values, active;
/* "4,1,x,40": the CPU counts the next calls of sched_getaffinity report, x for a call that fails; the last entry repeats. NULL: off. */
void p5_shim_set(const char *script) {
    n_values = 0; pos = 0; calls = 0; active = 0;
    if (!script) return;
    const char *p = script;
    while (*p && n_values < 4096) {
        if (*p == 'x') { values[n_values++] = -1; p++; }
        else { values[n_values++] = (int)strtol(p, (char **)&p, 10); }
        if (*p == ',') p++;
    }
    active = n_values > 0;
}
unsigned long p5_shim_calls(void) { return calls; }
int sched_getaffinity(pid_t pid, size_t size, cpu_set_t *mask) {
    if (!active) {
        if (!real_fn) real_fn = dlsym(RTLD_NEXT, "sched_getaffinity");
        return real_fn(pid, size, mask);
    }
    int v = values[pos < (unsigned long)n_values ? pos : (unsigned long)n_values - 1];
    pos++; calls++;
    if (v < 0) { errno = EPERM; return -1; }
    memset(mask, 0, size);
    for (int c = 0; c < v && (size_t)c < size * 8; c++) CPU_SET_S(c, size, mask);
    return 0;
}
"#;

fn build_shim(dir: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let src = dir.join("shim.c");
    let lib = dir.join("shim.so");
    std::fs::write(&src, SHIM).ok()?;
    let ok = Command::new("cc")
        .args(["-shared", "-fPIC", "-O1", "-o"])
        .arg(&lib)
        .arg(&src)
        .arg("-ldl")
        .status()
        .ok()?
        .success();
    ok.then_some(lib)
}

struct Shim {
    set: unsafe extern "C" fn(*const libc::c_char),
    calls: unsafe extern "C" fn() -> libc::c_ulong,
}

impl Shim {
    fn find() -> Option<Self> {
        // SAFETY: plain symbol lookups of two functions the preloaded library defines, with the signatures it gives them.
        unsafe {
            let set = libc::dlsym(libc::RTLD_DEFAULT, c"p5_shim_set".as_ptr());
            let calls = libc::dlsym(libc::RTLD_DEFAULT, c"p5_shim_calls".as_ptr());
            if set.is_null() || calls.is_null() {
                return None;
            }
            Some(Self {
                set: std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn(*const libc::c_char)>(set),
                calls: std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn() -> libc::c_ulong>(calls),
            })
        }
    }

    fn script(&self, script: Option<&str>) {
        let text = script.map(|s| std::ffi::CString::new(s).unwrap());
        // SAFETY: the library copies what it needs out of the string before returning.
        unsafe { (self.set)(text.as_ref().map_or(std::ptr::null(), |t| t.as_ptr())) }
    }

    fn calls(&self) -> u64 {
        // SAFETY: reads a counter.
        unsafe { (self.calls)() }
    }

    /// The range read while the script plays (at `percent`) and the reads it used.
    fn read(&self, script: &str, percent: u8) -> ((usize, usize), u64) {
        kerf_core::set_cpu_percent(percent);
        self.script(Some(script));
        let range = kerf_core::cpu_graph_thread_range();
        let used = self.calls();
        self.script(None);
        (range, used)
    }
}

/// The script of `cycle` repeated (rotated by `phase`), long enough to outlast the retries.
fn cycle_script(cycle: &[usize], phase: usize) -> String {
    (0..4000)
        .map(|i| cycle[(i + phase) % cycle.len()].to_string())
        .collect::<Vec<_>>()
        .join(",")
}

// ---- running a child ---------------------------------------------------------------------------------------------------------------------------

fn run_child(test: &str, preload: Option<&Path>, procs: &[PathBuf], envs: &[(&str, String)], limit: Duration) -> (bool, String) {
    let exe = std::env::current_exe().expect("the test binary");
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(r#"while [ "$1" != -- ]; do echo $$ > "$1" || exit 99; shift; done; shift; exec "$@""#)
        .arg("sh");
    cmd.args(procs);
    cmd.arg("--").arg(&exe);
    cmd.args([test, "--exact", "--ignored", "--nocapture", "--test-threads=1"]);
    if let Some(lib) = preload {
        cmd.env("LD_PRELOAD", lib);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("the child starts");
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let readers = [
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = out.read_to_string(&mut s);
            s
        }),
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            s
        }),
    ];
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let text: String = readers.into_iter().map(|r| r.join().unwrap_or_default()).collect();
    (status.is_some_and(|s| s.success()), text)
}

// ---- the plans ---------------------------------------------------------------------------------------------------------------------------------

fn asset(w: u32, h: u32) -> Asset {
    Asset {
        id: Uuid::new_v4(),
        path: "/media/a.mp4".into(),
        name: "a".into(),
        duration: 20.0,
        streams: vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(w),
            height: Some(h),
            fps: Some(30.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: Some("yuv420p".into()),
            color_space: None,
            color_range: Some("tv".into()),
            lead: 0.0,
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    }
}

fn plan_with(w: u32, h: u32, effects: Vec<VideoEffect>) -> RenderPlan {
    plan_in(w, h, effects, false)
}

/// The plan of one frame: an export frame (`still` false) or a paused still, whose graph runs on one thread when every input is a proxy.
fn plan_in(w: u32, h: u32, effects: Vec<VideoEffect>, still: bool) -> RenderPlan {
    let a = asset(w, h);
    let mut clip = Clip::new(a.id, 0.0, 10.0, 0.0);
    clip.effects = effects;
    let timeline = Timeline {
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V1")
        }],
        ..Timeline::new()
    };
    let opts = ExportOptions {
        fps: Some(30.0),
        ..ExportOptions::default()
    };
    let policy = kerf_core::CompositeColorPolicy::FixedBt601;
    let request = if still {
        PlanRequest::still(policy)
    } else {
        PlanRequest::motion(policy)
    };
    let planner = Planner::new(&timeline, std::slice::from_ref(&a), &opts, request).expect("planner");
    if still {
        planner.at(1.0).expect("plan")
    } else {
        planner.at_frame(30).expect("plan")
    }
}

fn key() -> VideoEffect {
    VideoEffect::ChromaKey {
        color: "green".into(),
        similarity: 0.15,
        blend: 0.1,
    }
}

fn caps() -> GpuCaps {
    let mut caps = GpuCaps::A0;
    caps.motion = true;
    caps.effects = EffectKinds::ALL;
    caps.blur_lanes = 16;
    caps
}

// ---- the scripted machine ----------------------------------------------------------------------------------------------------------------------

/// The cgroup quota the child runs under, in whole CPUs (the parent puts it in a cgroup with a binding quota), else none.
fn quota_env() -> usize {
    std::env::var("CHECKER_P5_QUOTA")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX)
}

fn host_online() -> usize {
    // SAFETY: `sysconf` takes a name and reads a system value.
    usize::try_from(unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) }).unwrap_or(1)
}

/// The range of a machine whose count is not known, with `cores` cores, at `percent`: the cap where there is one, else the whole range.
fn unknown(cores: usize, percent: u8) -> (usize, usize) {
    let budget = share(cores, percent);
    if budget < cores {
        (budget, budget)
    } else {
        (1, 16)
    }
}

/// What `script` run once at `percent` says, the reads of the affinity it took, and the answer expected of the machine its first entry is.
struct Run {
    range: (usize, usize),
    used: u64,
}

fn first_entry(script: &str) -> Option<usize> {
    script.split(',').next().and_then(|v| v.parse().ok())
}

fn scripted_child() {
    let shim = Shim::find().expect("the shim is preloaded");
    let quota = quota_env();
    let host = host_online();
    let failures: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let fail = |message: String| {
        eprintln!("CHECKER-P5 scripted FAILURE: {message}");
        failures.borrow_mut().push(message);
    };
    let run = |script: &str, percent: u8| {
        let (range, used) = shim.read(script, percent);
        Run { range, used }
    };
    // What a readable mask of `k` CPUs gives, and what a failed read gives (the cores are the standard library's: it reads the mask again,
    // and the host's CPUs where that fails too).
    let machine_of = |k: usize, percent: u8| machine(k, quota, percent);
    let unknown_of = |cores: usize, percent: u8| unknown(cores.min(quota).max(1), percent);
    // The hot path: one read of the mask, at 100 % and under a cap, whatever the quota.
    for (name, script, percent) in [
        ("100%", "4", 100u8),
        ("100%, one CPU", "1", 100),
        ("capped at 75%", "4", 75),
        ("capped, one CPU", "1", 75),
        ("capped at 50%", "4", 50),
        ("capped at 25%", "4", 25),
        ("a cap that is all the cores caps nothing", "4", 99),
        ("a mask of 40 CPUs", "40", 75),
    ] {
        let got = run(script, percent);
        let want = if percent >= 100 {
            (
                default_threads(first_entry(script).unwrap()),
                default_threads(first_entry(script).unwrap()),
            )
        } else {
            machine_of(first_entry(script).unwrap(), percent)
        };
        eprintln!(
            "CHECKER-P5 scripted: {name}: {:?} in {} read(s) (wanted {want:?} in 1)",
            got.range, got.used
        );
        if got.range != want || got.used != 1 {
            fail(format!("{name}: {:?} in {} reads, wanted {want:?} in 1", got.range, got.used));
        }
    }
    // A failed read: no count; at 100 % one read and the whole range, under a cap the standard library's cores (a second read).
    let got = run("x", 100);
    eprintln!(
        "CHECKER-P5 scripted: a failed read at 100%: {:?} in {} read(s)",
        got.range, got.used
    );
    if got.range != (1, 16) || got.used != 1 {
        fail(format!(
            "a failed read at 100%: {:?} in {} reads, wanted (1, 16) in 1",
            got.range, got.used
        ));
    }
    for percent in [75u8, 50, 25] {
        let got = run("x", percent);
        let want = unknown_of(host, percent);
        eprintln!(
            "CHECKER-P5 scripted: a failed read at {percent}%: {:?} in {} read(s) (wanted {want:?})",
            got.range, got.used
        );
        if got.range != want {
            fail(format!("a failed read at {percent}%: {:?}, wanted {want:?}", got.range));
        }
    }
    // An empty mask (some old kernels return one for a thread whose affinity was never set) is no answer: no count, and the cores the
    // standard library has (it reads again, and falls back to the host's CPUs on an empty mask too).
    for (script, percent, reads) in [("0", 100u8, 1u64), ("0", 75, 2), ("0,4", 75, 2), ("0,1", 50, 2)] {
        let got = run(script, percent);
        let second = script.split(',').nth(1).and_then(|v| v.parse::<usize>().ok()).unwrap_or(host);
        let want = if percent >= 100 {
            (1, 16)
        } else {
            unknown_of(second, percent)
        };
        eprintln!(
            "CHECKER-P5 scripted: an empty mask ({script}) at {percent}%: {:?} in {} read(s) (wanted {want:?} in {reads})",
            got.range, got.used
        );
        if got.range != want || got.used != reads {
            fail(format!(
                "an empty mask ({script}) at {percent}%: {:?} in {} reads, wanted {want:?} in {reads}",
                got.range, got.used
            ));
        }
    }
    // `cores()` and the budget are one read each.
    for script in ["4", "1", "40"] {
        kerf_core::set_cpu_percent(75);
        shim.script(Some(script));
        let cores = kerf_core::cpu_cores();
        let used_cores = shim.calls();
        let budget = kerf_core::cpu_threads();
        let used_budget = shim.calls() - used_cores;
        shim.script(None);
        let k: usize = script.parse().unwrap();
        eprintln!(
            "CHECKER-P5 scripted: cores of {script}: {cores} in {used_cores} read(s), the budget {budget} in {used_budget}"
        );
        if cores != k.min(quota) || used_cores != 1 || used_budget != 1 || budget != share(k.min(quota), 75) {
            fail(format!(
                "cores/budget of a mask of {script}: {cores} in {used_cores} reads, {budget} in {used_budget}"
            ));
        }
    }
    // Changes of the affinity between the reads of a call cannot reach it: the call makes one. Every flip, undoing and alternation that
    // used to pair the count of one affinity with the cores of another is answered as the machine its single read saw.
    let machines = |script_states: &[usize], percent: u8| -> Vec<(usize, usize)> {
        script_states.iter().map(|&k| machine_of(k, percent)).collect()
    };
    for (name, script, states) in [
        ("W then N for good", "4,1".to_string(), vec![4usize, 1]),
        ("N then W for good", "1,4".to_string(), vec![4, 1]),
        ("W, N, W, then W: a flip and its undoing", "4,1,4".to_string(), vec![4, 1]),
        ("W, N, W, N, then N", "4,1,4,1".to_string(), vec![4, 1]),
        (
            "W, N, W, N, W: the five reads that tore the settled pair",
            "4,1,4,1,4".to_string(),
            vec![4, 1],
        ),
        ("W, N, W, N, W, N...", cycle_script(&[4, 1], 0), vec![4, 1]),
        ("N, W, N, W, N, W...", cycle_script(&[4, 1], 1), vec![4, 1]),
        ("W, W, N", "4,4,1".to_string(), vec![4, 1]),
        ("40, 4, 1", cycle_script(&[40, 4, 1], 0), vec![40, 4, 1]),
        (
            "4, 1, 40 (a 40-CPU state the count reads alone would see)",
            cycle_script(&[4, 1, 40], 0),
            vec![4, 1, 40],
        ),
        ("4, 40, 1", cycle_script(&[4, 40, 1], 0), vec![4, 40, 1]),
    ] {
        for percent in [75u8, 50, 25] {
            let got = run(&script, percent);
            let valid = machines(&states, percent);
            let first = machine_of(first_entry(&script).unwrap(), percent);
            if !valid.contains(&got.range) || got.range != first || got.used != 1 {
                fail(format!(
                    "{name} at {percent}%: {:?} in {} reads; the machines' are {valid:?}, the first read's {first:?}",
                    got.range, got.used
                ));
            }
        }
        eprintln!("CHECKER-P5 scripted: {name}: the first read's machine at 75/50/25%, one read");
    }
    // Every cycle of one to four CPUs up to length six, at every phase and a spread of budgets: one read, and the machine it saw.
    let states = [1usize, 2, 3, 4];
    let (mut tried, mut torn, mut extra_reads) = (0usize, BTreeMap::<Vec<usize>, ((usize, usize), usize, u8)>::new(), 0usize);
    for len in 1..=6usize {
        let mut cycle = vec![0usize; len];
        for code in 0..states.len().pow(len as u32) {
            let mut c = code;
            for slot in cycle.iter_mut() {
                *slot = states[c % states.len()];
                c /= states.len();
            }
            for phase in 0..len {
                for percent in [75u8, 50, 25] {
                    tried += 1;
                    let got = run(&cycle_script(&cycle, phase), percent);
                    let valid = machines(&cycle, percent);
                    if !valid.contains(&got.range) || got.range != machine_of(cycle[phase], percent) {
                        torn.entry(cycle.clone()).or_insert((got.range, phase, percent));
                    }
                    if got.used != 1 {
                        extra_reads += 1;
                    }
                }
            }
        }
    }
    eprintln!(
        "CHECKER-P5 scripted: {tried} cycle/phase/budget combinations: {} cycles gave the range of no machine, {extra_reads} took more than one read",
        torn.len()
    );
    if let Some((cycle, (got, phase, percent))) = torn.iter().next() {
        fail(format!(
            "{} cycles of scripted affinity gave a range that is not the first read's machine's, e.g. {cycle:?} at phase {phase}, {percent}%: {got:?}",
            torn.len()
        ));
    }
    if extra_reads > 0 {
        fail(format!(
            "{extra_reads} reads of a readable mask took more than one read of the affinity"
        ));
    }
    // Cycles with a failing call among them: the answer is a machine's (a readable first read), or the cap of the cores the standard library
    // reads next (a failed first read: the second read is the library's own, the only second read of the shape).
    let states = [1usize, 2, 4, usize::MAX];
    let (mut tried, mut bad, mut second_reads) = (0usize, Vec::<String>::new(), 0usize);
    for len in 1..=5usize {
        let mut cycle = vec![0usize; len];
        for code in 0..states.len().pow(len as u32) {
            let mut c = code;
            for slot in cycle.iter_mut() {
                *slot = states[c % states.len()];
                c /= states.len();
            }
            for phase in 0..len {
                for percent in [75u8, 50] {
                    tried += 1;
                    let script = (0..400)
                        .map(|i| match cycle[(i + phase) % len] {
                            usize::MAX => "x".to_string(),
                            k => k.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    let got = run(&script, percent);
                    let numeric: Vec<usize> = cycle.iter().copied().filter(|&k| k != usize::MAX).collect();
                    let mut valid = machines(&numeric, percent);
                    valid.extend(numeric.iter().map(|&k| unknown_of(k, percent)));
                    valid.push(unknown_of(host, percent));
                    valid.push((1, 16));
                    if got.used == 2 {
                        second_reads += 1;
                    }
                    if !valid.contains(&got.range) || got.used > 2 {
                        bad.push(format!(
                            "{cycle:?} phase {phase} at {percent}%: {:?} in {} reads",
                            got.range, got.used
                        ));
                    }
                }
            }
        }
    }
    eprintln!(
        "CHECKER-P5 scripted: {tried} cycles with failing reads: {} answers of no machine, {second_reads} took the standard library's second read",
        bad.len()
    );
    if let Some(b) = bad.first() {
        fail(format!(
            "{} answers of cycles with failing reads are of no machine, e.g. {b}",
            bad.len()
        ));
    }
    // The failed read's second read, tried: a count that is unknown with the cores of a mask read afterwards (information; the cap is the
    // engine's own at its spawn, which reads the same way).
    for (script, percent) in [("x,1", 75u8), ("x,40", 75), ("x,4", 75), ("x,x", 75)] {
        let got = run(script, percent);
        eprintln!(
            "CHECKER-P5 scripted: (info) {script} at {percent}%: {:?} in {} reads; the cap of cores read by the library after the failed read",
            got.range, got.used
        );
    }
    let failures = failures.into_inner();
    if failures.is_empty() {
        eprintln!("CHECKER-P5 scripted: all held");
    } else {
        panic!("{} failures", failures.len());
    }
}

fn plan_child() {
    let shim = Shim::find().expect("the shim is preloaded");
    let quota = quota_env();
    let host = host_online();
    let mut failures: Vec<String> = Vec::new();
    let caps = caps();
    let mut pinned = caps;
    pinned.graph_threads = Some(3);
    let keyed = plan_with(64, 64, vec![key()]);
    let both = plan_with(64, 64, vec![key(), VideoEffect::Blur { sigma: 3.0 }]);
    let blurred = plan_with(50, 50, vec![VideoEffect::Blur { sigma: 3.0 }]);
    let plain = plan_with(64, 64, vec![]);
    let mut single = plan_in(64, 64, vec![key()], true);
    single.layers[0].source.proxy = true;
    let not_known = |reasons: &[kerf_core::Unsupported]| {
        reasons
            .iter()
            .any(|u| u.to_string().contains("slice-thread count is not known"))
    };
    let hazard = |reasons: &[kerf_core::Unsupported]| reasons.iter().any(|u| u.to_string().contains("a blur on a plane"));
    // A judgement of `plan` for `caps` while `script` plays at `percent`: its reasons and the reads of the affinity it took.
    let judge = |plan: &RenderPlan, caps: &GpuCaps, script: &str, percent: u8| {
        kerf_core::set_cpu_percent(percent);
        shim.script(Some(script));
        let reasons = plan.reasons(caps, plan.size(u32::MAX));
        let used = shim.calls();
        shim.script(None);
        (reasons, used)
    };
    let exact = |plan: &RenderPlan, caps: &GpuCaps, script: &str, percent: u8| {
        kerf_core::set_cpu_percent(percent);
        shim.script(Some(script));
        let exact = plan.exact_graph_threads(caps);
        let used = shim.calls();
        shim.script(None);
        (exact, used)
    };
    // One judgement is one read of the machine, however many layers and effects ask for the range, and none where nothing needs it.
    for (name, plan, caps, want) in [
        ("a keyed layer", &keyed, &caps, 1u64),
        ("a keyed and blurred layer (one range for both)", &both, &caps, 1),
        ("a blurred layer", &blurred, &caps, 1),
        ("a layer with neither", &plain, &caps, 0),
        ("a keyed layer, the compositor pinned", &keyed, &pinned, 0),
        ("a keyed and blurred layer, the compositor pinned", &both, &pinned, 0),
        ("a keyed layer of a single frame of proxies", &single, &caps, 0),
    ] {
        for script in ["4", "1,4,1,4,1,4"] {
            let (reasons, used) = judge(plan, caps, script, 75);
            eprintln!(
                "CHECKER-P5 plan: {name}, script {script}: {used} read(s) (wanted {want}); {} reasons",
                reasons.len()
            );
            if used != want {
                failures.push(format!(
                    "{name} (script {script}): {used} reads of the affinity, wanted {want}"
                ));
            }
        }
    }
    for (name, plan, caps, want) in [
        ("exact threads", &keyed, &caps, 1u64),
        ("exact threads, pinned", &keyed, &pinned, 0),
        ("exact threads, a single frame", &single, &caps, 0),
    ] {
        let (_, used) = exact(plan, caps, "4", 75);
        if used != want {
            failures.push(format!("{name}: {used} reads, wanted {want}"));
        }
    }
    // Each machine's verdicts: the key is drawn at the cap of the mask the read saw, and the blur is judged over that cap.
    for k in [1usize, 2, 3, 4, 8, 40] {
        for percent in [75u8, 50, 25] {
            let (lo, hi) = machine(k, quota, percent);
            let (reasons, _) = judge(&keyed, &caps, &k.to_string(), percent);
            let (drawn_at, _) = exact(&keyed, &caps, &k.to_string(), percent);
            let want_refused = lo != hi;
            if not_known(&reasons) != want_refused || drawn_at != (lo == hi).then_some(lo as u32) {
                failures.push(format!(
                    "a mask of {k} CPUs at {percent}%: key refused {} exact {drawn_at:?}, the machine's range is {:?}",
                    not_known(&reasons),
                    (lo, hi)
                ));
            }
            let (reasons, _) = judge(&blurred, &caps, &k.to_string(), percent);
            let want = kerf_core::gblur::picture_hazard_for((50, 50), lo as u32..=hi as u32, 16, false).is_some();
            if hazard(&reasons) != want {
                failures.push(format!(
                    "a 50x50 blur on a mask of {k} CPUs at {percent}%: refused {}, the range {:?} says {want}",
                    hazard(&reasons),
                    (lo, hi)
                ));
            }
        }
    }
    // A failed read: no count; the key is refused where the cores the library reads next leave a range, drawn at the cap where they cap.
    for percent in [100u8, 75, 50, 25] {
        let (reasons, _) = judge(&keyed, &caps, "x", percent);
        let range = if percent >= 100 {
            (1, 16)
        } else {
            unknown(host.min(quota).max(1), percent)
        };
        if not_known(&reasons) != (range.0 != range.1) {
            failures.push(format!(
                "a failed read at {percent}%: key refused {}, the range is {range:?}",
                not_known(&reasons)
            ));
        }
    }
    // The sequences that tore the settled pair: the key is refused or drawn at a count of the machine the read saw, never between two.
    for (name, script) in [
        ("W,N,W,N,W then W", "4,1,4,1,4".to_string()),
        ("strict alternation", cycle_script(&[4, 1], 0)),
        ("the other phase", cycle_script(&[4, 1], 1)),
    ] {
        let (drawn_at, _) = exact(&keyed, &caps, &script, 75);
        let machines = [machine(4, quota, 75), machine(1, quota, 75)];
        if let Some(t) = drawn_at {
            if !machines.iter().any(|m| m.0 == t as usize && m.1 == t as usize) {
                failures.push(format!(
                    "{name}: the key is drawn at {t} slice threads, the machines' ranges are {machines:?}"
                ));
            }
        }
        eprintln!("CHECKER-P5 plan: {name}: key drawn at {drawn_at:?}; the machines' ranges are {machines:?}");
    }
    if failures.is_empty() {
        eprintln!("CHECKER-P5 plan: all held");
    } else {
        for f in &failures {
            eprintln!("CHECKER-P5 plan FAILURE: {f}");
        }
        panic!("{} failures", failures.len());
    }
}

/// The cap the engine writes into an FFmpeg argv at spawn (`limit_ffmpeg_args`, the one-process share): the thread count it gave
/// (`-filter_threads`), or `None` for no cap.
fn argv_cap(args: &[String]) -> Option<usize> {
    args.windows(2)
        .find(|w| w[0] == "-filter_threads")
        .and_then(|w| w[1].parse().ok())
}

/// The argv of a machine of `k` CPUs under the quota: a cap of the budget's share of the cores where that is fewer than the cores.
fn machine_argv(k: usize, quota: usize, percent: u8) -> Option<usize> {
    let cores = k.min(quota).max(1);
    let n = share(cores, percent);
    (n < cores).then_some(n)
}

/// The slice threads FFmpeg's graph runs on for an argv cap `cap` when it is started on a machine of `spawn` CPUs: the cap, else its own
/// default for the CPUs it counts.
fn effective_threads(cap: Option<usize>, spawn: usize) -> usize {
    cap.unwrap_or_else(|| default_threads(spawn))
}

fn limit_child() {
    let shim = Shim::find().expect("the shim is preloaded");
    let quota = quota_env();
    let (mut tried, mut torn, mut extra_reads) = (0usize, Vec::<String>::new(), 0usize);
    for a in [1usize, 2, 3, 4, 8] {
        for b in [1usize, 2, 3, 4, 8] {
            for percent in [75u8, 50, 25] {
                kerf_core::set_cpu_percent(percent);
                let mut args: Vec<String> = ["-i", "in.mkv", "out.mkv"].map(String::from).to_vec();
                shim.script(Some(&format!("{a},{b}")));
                kerf_core::limit_ffmpeg_args(&mut args, 1);
                let used = shim.calls();
                shim.script(None);
                let got = argv_cap(&args);
                tried += 1;
                if used != 1 {
                    extra_reads += 1;
                }
                // The graph FFmpeg runs, started on the machine the second read saw (where the affinity stays): the cap written, else its
                // own default. A machine's own is its cap, else its default. A pairing of two reads shows where the cap written is neither.
                let effective = effective_threads(got, b);
                let of = |k: usize| effective_threads(machine_argv(k, quota, percent), k);
                if effective != of(a) && effective != of(b) {
                    torn.push(format!(
                        "reads {a} then {b} at {percent}%: the cap written is {got:?}, a graph of {effective} threads; a machine of {a} runs {}, one of {b} runs {}",
                        of(a),
                        of(b)
                    ));
                }
            }
        }
    }
    eprintln!(
        "CHECKER-P5 limit: {tried} (first read, second read, budget) argv caps: {} of them the cap of no machine, {extra_reads} took more than one read of the affinity",
        torn.len()
    );
    for t in torn.iter().take(5) {
        eprintln!("CHECKER-P5 limit: torn: {t}");
    }
    let mut failures = Vec::new();
    if !torn.is_empty() {
        failures.push(format!(
            "{} argv caps are the cap of no machine ({} first), the budget's threads of one read compared with the cores of another",
            torn.len(),
            torn[0]
        ));
    }
    if extra_reads > 0 {
        failures.push(format!(
            "{extra_reads} of {tried} caps took two reads of the affinity (`budget_threads()` then `cores()`)"
        ));
    }
    if !failures.is_empty() {
        for f in &failures {
            eprintln!("CHECKER-P5 limit FAILURE: {f}");
        }
        panic!("{} failures", failures.len());
    }
}

// ---- real flippers -----------------------------------------------------------------------------------------------------------------------------

fn allowed() -> Vec<usize> {
    // SAFETY: the set is plain bits, filled by the call.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        assert_eq!(
            libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set),
            0
        );
        (0..libc::CPU_SETSIZE as usize)
            .filter(|&c| libc::CPU_ISSET(c, &set))
            .collect()
    }
}

fn set_affinity(tid: i32, cpus: &[usize]) {
    // SAFETY: the set is plain bits and the call only reads it.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &c in cpus {
            libc::CPU_SET(c, &mut set);
        }
        libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

fn spin(micros: f64) {
    let until = Instant::now() + Duration::from_secs_f64(micros / 1e6);
    while Instant::now() < until {
        std::hint::spin_loop();
    }
}

/// How a flipper moves the affinity of the reader.
#[derive(Clone, Copy, Debug)]
enum Flip {
    /// Sleep `micros` between flips of the reader between two masks.
    Sleep(u64),
    /// No pause at all between flips.
    Spin,
    /// After the reader starts a read, wait a random time and flip to the narrow mask and back after a random time, twice: a change and
    /// its undoing timed from the read's own progress.
    Burst,
    /// As fast as it can, through every mask of one CPU to all of them.
    Cycle,
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn flip_run<T: Ord + Send + 'static>(percent: u8, flip: Flip, reads: usize, read: fn() -> T) -> BTreeMap<T, usize> {
    kerf_core::set_cpu_percent(percent);
    let all = allowed();
    let masks: Vec<Vec<usize>> = (1..=all.len()).map(|k| all[..k].to_vec()).collect();
    let tid = Arc::new(AtomicI32::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));
    let flipper = {
        let (tid, stop, progress, masks) = (tid.clone(), stop.clone(), progress.clone(), masks);
        std::thread::spawn(move || {
            while tid.load(Ordering::Acquire) == 0 {
                std::thread::yield_now();
            }
            let t = tid.load(Ordering::Acquire);
            let (wide, narrow) = (masks.last().unwrap().clone(), masks[0].clone());
            let mut rng = 0x9e37_79b9_7f4a_7c15_u64 ^ (percent as u64) << 20;
            let mut seen = 0u64;
            let mut wide_now = true;
            while !stop.load(Ordering::Acquire) {
                match flip {
                    Flip::Sleep(us) => {
                        wide_now = !wide_now;
                        set_affinity(t, if wide_now { &wide } else { &narrow });
                        std::thread::sleep(Duration::from_micros(us));
                    }
                    Flip::Spin => {
                        wide_now = !wide_now;
                        set_affinity(t, if wide_now { &wide } else { &narrow });
                    }
                    Flip::Burst => {
                        let now = progress.load(Ordering::Acquire);
                        if now == seen {
                            std::hint::spin_loop();
                            continue;
                        }
                        seen = now;
                        for _ in 0..1 + xorshift(&mut rng) % 3 {
                            spin((xorshift(&mut rng) % 400) as f64 / 10.0);
                            set_affinity(t, &narrow);
                            spin((xorshift(&mut rng) % 120) as f64 / 10.0);
                            set_affinity(t, &wide);
                        }
                    }
                    Flip::Cycle => {
                        let m = &masks[(xorshift(&mut rng) % masks.len() as u64) as usize];
                        set_affinity(t, m);
                    }
                }
            }
            set_affinity(t, &wide);
        })
    };
    let reader = {
        let (tid, stop, progress) = (tid, stop, progress);
        std::thread::spawn(move || {
            // SAFETY: `gettid` takes no arguments and cannot fail.
            tid.store(unsafe { libc::gettid() }, Ordering::Release);
            let mut seen: BTreeMap<T, usize> = BTreeMap::new();
            for _ in 0..reads {
                progress.fetch_add(1, Ordering::AcqRel);
                *seen.entry(read()).or_default() += 1;
            }
            stop.store(true, Ordering::Release);
            seen
        })
    };
    let seen = reader.join().expect("the reader ends");
    flipper.join().expect("the flipper ends");
    seen
}

fn real_flippers_child() {
    let all = allowed();
    if all.len() < 2 || kerf_core::ffmpeg_cpu_count().is_none() {
        eprintln!("CHECKER-P5 flippers: fewer than two CPUs: not run");
        return;
    }
    let count = all.len();
    let reads: usize = std::env::var("CHECKER_P5_READS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150_000);
    let (mut torn_total, mut fallback_total, mut total) = (0usize, 0usize, 0usize);
    let mut failures = Vec::new();
    for flip in [Flip::Sleep(20), Flip::Sleep(5), Flip::Spin, Flip::Burst, Flip::Cycle] {
        for percent in [75u8, 50, 25] {
            let seen = flip_run(percent, flip, reads, kerf_core::cpu_graph_thread_range);
            // (The machines the flipper moves the reader between: the wide mask and the narrow one, or every width in between.)
            let visited: Vec<usize> = if matches!(flip, Flip::Cycle) {
                (1..=count).collect()
            } else {
                vec![count, 1]
            };
            let valid: Vec<(usize, usize)> = visited.iter().map(|&k| machine(k, quota_env(), percent)).collect();
            let fallback = (1, 16.max(count));
            let (mut torn, mut fb) = (Vec::new(), 0usize);
            for (answer, n) in &seen {
                if valid.contains(answer) {
                } else if *answer == fallback {
                    fb += n;
                } else {
                    torn.push((*answer, *n));
                }
            }
            let n: usize = seen.values().sum();
            total += n;
            fallback_total += fb;
            torn_total += torn.iter().map(|t| t.1).sum::<usize>();
            eprintln!(
                "CHECKER-P5 flippers: {flip:?} {percent}%: {seen:?}; machines' {valid:?}; fallback x{fb}; of no machine {torn:?}"
            );
            if !torn.is_empty() {
                failures.push(format!("{flip:?} at {percent}%: answers of no machine: {torn:?}"));
            }
        }
    }
    eprintln!("CHECKER-P5 flippers: {total} reads, {torn_total} of no machine, {fallback_total} the wide fallback");
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

fn spawn_cap() -> Option<usize> {
    let mut args: Vec<String> = ["-i", "in.mkv", "out.mkv"].map(String::from).to_vec();
    kerf_core::limit_ffmpeg_args(&mut args, 1);
    argv_cap(&args)
}

fn real_limit_child() {
    let all = allowed();
    if all.len() < 2 {
        eprintln!("CHECKER-P5 limit flippers: fewer than two CPUs: not run");
        return;
    }
    let quota = quota_env();
    let reads: usize = std::env::var("CHECKER_P5_READS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    let (mut torn_total, mut total) = (0usize, 0usize);
    for flip in [Flip::Sleep(20), Flip::Spin, Flip::Burst, Flip::Cycle] {
        for percent in [75u8, 50, 25] {
            let seen = flip_run(percent, flip, reads, spawn_cap);
            let visited: Vec<usize> = if matches!(flip, Flip::Cycle) {
                (1..=all.len()).collect()
            } else {
                vec![all.len(), 1]
            };
            // (A cap written is a graph of that many threads; no cap is FFmpeg's own default for wherever it starts. A cap is of no
            // machine where it is neither a visited machine's cap nor the default of one.)
            let valid: Vec<usize> = visited
                .iter()
                .map(|&k| effective_threads(machine_argv(k, quota, percent), k))
                .collect();
            let torn: usize = seen
                .iter()
                .filter(|(cap, _)| cap.is_some_and(|n| !valid.contains(&n)))
                .map(|(_, n)| n)
                .sum();
            total += seen.values().sum::<usize>();
            torn_total += torn;
            eprintln!("CHECKER-P5 limit flippers: {flip:?} {percent}%: {seen:?}; machines' {valid:?}; of no machine x{torn}");
        }
    }
    eprintln!("CHECKER-P5 limit flippers: {total} spawn-time caps, {torn_total} the cap of no machine");
    assert_eq!(
        torn_total, 0,
        "{torn_total} of {total} caps written at spawn pair the budget of one read with the cores of another"
    );
}

// ---- a cpuset resized under a binding quota ---------------------------------------------------------------------------------------------------

struct Group {
    dir: PathBuf,
}

impl Group {
    fn new(hierarchy: &str, tag: &str) -> Option<Self> {
        let root = Path::new("/sys/fs/cgroup").join(hierarchy);
        if !root.is_dir() {
            return None;
        }
        let dir = root.join(format!("kerf-p5-{}-{tag}", std::process::id()));
        std::fs::create_dir(&dir).ok()?;
        Some(Self { dir })
    }

    fn write(&self, file: &str, value: &str) -> bool {
        std::fs::write(self.dir.join(file), value).is_ok()
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.dir);
    }
}

fn cgroup_child() {
    let reads: usize = std::env::var("CHECKER_P5_READS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000);
    let mut out = String::new();
    for percent in [75u8, 50, 25] {
        kerf_core::set_cpu_percent(percent);
        let mut seen: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        for _ in 0..reads {
            *seen.entry(kerf_core::cpu_graph_thread_range()).or_default() += 1;
        }
        let entries: Vec<String> = seen.iter().map(|((lo, hi), n)| format!("{lo}-{hi}x{n}")).collect();
        out.push_str(&format!("CHECKER-P5 cgroup: {percent} {}\n", entries.join(" ")));
    }
    eprint!("{out}");
}

// ---- the tests ---------------------------------------------------------------------------------------------------------------------------------

#[test]
#[ignore = "run by the parents below, in a process of its own"]
fn checker_p5_torn_child() {
    match std::env::var(CHILD).as_deref() {
        Ok("scripted") => scripted_child(),
        Ok("plan") => plan_child(),
        Ok("limit") => limit_child(),
        Ok("limit_flippers") => real_limit_child(),
        Ok("flippers") => real_flippers_child(),
        Ok("cgroup") => cgroup_child(),
        _ => {}
    }
}

fn shim_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kerf-p5-{}-{tag}", std::process::id()))
}

fn scripted(mode: &str) -> (bool, String) {
    let dir = shim_dir(mode);
    let Some(lib) = build_shim(&dir) else {
        eprintln!("CHECKER-P5: no C compiler on this host: not run");
        return (true, String::new());
    };
    let out = run_child(
        "checker_p5_torn_child",
        Some(&lib),
        &[],
        &[(CHILD, mode.to_string()), ("KERF_CPU_PERCENT", "75".into())],
        Duration::from_secs(600),
    );
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
#[ignore = "needs a C compiler (LD_PRELOAD shim)"]
fn checker_p5_torn_a_scripted_machine_gives_the_range_of_the_one_mask_the_call_reads() {
    let (ok, text) = scripted("scripted");
    for l in text.lines().filter(|l| l.starts_with("CHECKER-P5")) {
        eprintln!("{l}");
    }
    assert!(
        ok,
        "the scripted child failed:\n{}",
        text.lines()
            .filter(|l| l.contains("FAILURE") || l.contains("panicked"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
#[ignore = "needs a C compiler (LD_PRELOAD shim)"]
fn checker_p5_torn_a_scripted_machine_drives_the_plan_with_one_read_a_judgement() {
    let (ok, text) = scripted("plan");
    for l in text.lines().filter(|l| l.starts_with("CHECKER-P5")) {
        eprintln!("{l}");
    }
    assert!(
        ok,
        "the plan child failed:\n{}",
        text.lines()
            .filter(|l| l.contains("FAILURE") || l.contains("panicked"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
#[ignore = "needs a C compiler (LD_PRELOAD shim)"]
fn checker_p5_torn_the_cap_the_engine_writes_at_spawn_is_one_read_of_the_machine() {
    let (ok, text) = scripted("limit");
    for l in text.lines().filter(|l| l.starts_with("CHECKER-P5")) {
        eprintln!("{l}");
    }
    assert!(
        ok,
        "the limit child failed:\n{}",
        text.lines()
            .filter(|l| l.contains("FAILURE") || l.contains("panicked"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
#[ignore = "needs a C compiler (LD_PRELOAD shim) and a writable cpu cgroup: a quota that binds under the scripted affinity"]
fn checker_p5_torn_a_scripted_machine_under_a_binding_quota_gives_the_range_of_the_one_mask_the_call_reads() {
    let Some(quota) = Group::new("cpu", "qs") else {
        eprintln!("CHECKER-P5: no writable cpu cgroup on this host: not run");
        return;
    };
    if !(quota.write("cpu.cfs_period_us", "100000") && quota.write("cpu.cfs_quota_us", "200000")) {
        eprintln!("CHECKER-P5: the quota group could not be set up: not run");
        return;
    }
    let dir = shim_dir("quota");
    let Some(lib) = build_shim(&dir) else {
        eprintln!("CHECKER-P5: no C compiler on this host: not run");
        return;
    };
    for mode in ["scripted", "plan"] {
        let (ok, text) = run_child(
            "checker_p5_torn_child",
            Some(&lib),
            &[quota.dir.join("cgroup.procs")],
            &[
                (CHILD, mode.to_string()),
                ("KERF_CPU_PERCENT", "75".into()),
                ("CHECKER_P5_QUOTA", "2".into()),
            ],
            Duration::from_secs(600),
        );
        for l in text.lines().filter(|l| l.starts_with("CHECKER-P5")) {
            eprintln!("quota 2 {mode}: {l}");
        }
        assert!(
            ok,
            "the {mode} child under a quota of 2 failed:\n{}",
            text.lines()
                .filter(|l| l.contains("FAILURE") || l.contains("panicked"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "an adversarial race, run on purpose: it flips a thread's affinity as fast as it can"]
fn checker_p5_torn_real_flippers_never_tear_the_range() {
    let (ok, text) = run_child(
        "checker_p5_torn_child",
        None,
        &[],
        &[(CHILD, "flippers".into()), ("KERF_CPU_PERCENT", "75".into())],
        Duration::from_secs(1200),
    );
    for l in text.lines().filter(|l| l.starts_with("CHECKER-P5")) {
        eprintln!("{l}");
    }
    assert!(
        ok,
        "the flipper child failed:\n{}",
        text.lines()
            .filter(|l| l.contains("failures") || l.contains("of no machine"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
#[ignore = "an adversarial race, run on purpose: it flips a thread's affinity as fast as it can"]
fn checker_p5_torn_real_flippers_never_tear_the_cap_the_engine_writes_at_spawn() {
    let (ok, text) = run_child(
        "checker_p5_torn_child",
        None,
        &[],
        &[(CHILD, "limit_flippers".into()), ("KERF_CPU_PERCENT", "75".into())],
        Duration::from_secs(900),
    );
    for l in text.lines().filter(|l| l.starts_with("CHECKER-P5")) {
        eprintln!("{l}");
    }
    assert!(
        ok,
        "the spawn-time cap flippers child failed:\n{}",
        text.lines()
            .filter(|l| l.contains("panicked") || l.contains("caps"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
#[ignore = "needs a writable cpu and cpuset cgroup: a cpuset resized under a binding quota"]
fn checker_p5_torn_a_cgroup_cpuset_resized_under_a_binding_quota_never_tears_the_range() {
    let (Some(quota), Some(set)) = (Group::new("cpu", "q"), Group::new("cpuset", "s")) else {
        eprintln!("CHECKER-P5 cgroup: no writable cgroups on this host: not run");
        return;
    };
    let mems = std::fs::read_to_string("/sys/fs/cgroup/cpuset/cpuset.mems").unwrap_or_default();
    if !(quota.write("cpu.cfs_period_us", "100000")
        && quota.write("cpu.cfs_quota_us", "200000")
        && set.write("cpuset.cpus", "0-3")
        && set.write("cpuset.mems", mems.trim()))
    {
        eprintln!("CHECKER-P5 cgroup: the groups could not be set up: not run");
        return;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let flipper = {
        let (stop, cpus) = (stop.clone(), set.dir.join("cpuset.cpus"));
        std::thread::spawn(move || {
            let mut rng = 0x1234_5678_9abc_def1_u64;
            let masks = ["0-3", "0", "0-1", "0-2", "3", "0-3"];
            while !stop.load(Ordering::Acquire) {
                let m = masks[(xorshift(&mut rng) % masks.len() as u64) as usize];
                let _ = std::fs::write(&cpus, m);
            }
            let _ = std::fs::write(&cpus, "0-3");
        })
    };
    let procs = [quota.dir.join("cgroup.procs"), set.dir.join("cgroup.procs")];
    let (ok, text) = run_child(
        "checker_p5_torn_child",
        None,
        &procs,
        &[(CHILD, "cgroup".into()), ("KERF_CPU_PERCENT", "75".into())],
        Duration::from_secs(900),
    );
    stop.store(true, Ordering::Release);
    flipper.join().expect("the flipper ends");
    assert!(ok, "the child failed:\n{text}");
    let mut failures = Vec::new();
    let quota_cpus = 2usize;
    for l in text.lines().filter(|l| l.starts_with("CHECKER-P5 cgroup:")) {
        eprintln!("{l}");
        let percent: u8 = l.split_whitespace().nth(2).unwrap().parse().unwrap();
        let valid: Vec<(usize, usize)> = (1..=4).map(|k| machine(k, quota_cpus, percent)).collect();
        for entry in l.split_whitespace().skip(3) {
            let (range, n) = entry.split_once('x').expect("an entry");
            let (lo, hi) = range.split_once('-').expect("a range");
            let answer = (lo.parse::<usize>().unwrap(), hi.parse::<usize>().unwrap());
            if !valid.contains(&answer) && answer != (1, 16) {
                failures.push(format!(
                    "{percent}%: {answer:?} x{n}, the machines' (quota {quota_cpus}) are {valid:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}
