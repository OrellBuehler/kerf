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
    Planner::new(
        &timeline,
        std::slice::from_ref(&a),
        &opts,
        PlanRequest::motion(kerf_core::CompositeColorPolicy::FixedBt601),
    )
    .expect("planner")
    .at_frame(30)
    .expect("plan")
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

fn scripted_child() {
    let shim = Shim::find().expect("the shim is preloaded");
    let failures: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let check = |name: &str, got: ((usize, usize), u64), want: (usize, usize), reads: Option<u64>| {
        let ok = got.0 == want && reads.is_none_or(|r| r == got.1);
        eprintln!(
            "CHECKER-P5 scripted: {name}: range {:?} in {} reads (wanted {want:?}{})",
            got.0,
            got.1,
            reads.map_or(String::new(), |r| format!(" in {r}"))
        );
        if !ok {
            failures.borrow_mut().push(format!(
                "{name}: {:?} in {} reads, wanted {want:?}{}",
                got.0,
                got.1,
                reads.map_or(String::new(), |r| format!(" in {r}"))
            ));
        }
    };
    // The hot path.
    check("100% reads the count once", shim.read("4", 100), (5, 5), Some(1));
    check("100%, one CPU", shim.read("1", 100), (1, 1), Some(1));
    check(
        "a call that fails at 100% is the range 1..=16",
        shim.read("x", 100),
        (1, 16),
        Some(1),
    );
    check(
        "a steady capped machine takes three reads",
        shim.read("4", 75),
        (3, 3),
        Some(3),
    );
    check("a steady capped machine of one CPU", shim.read("1", 75), (1, 1), Some(3));
    check("a steady capped machine, 50 %", shim.read("4", 50), (2, 2), Some(3));
    check("a steady capped machine, 25 %", shim.read("4", 25), (1, 1), Some(3));
    check(
        "a cap that is all the cores caps nothing",
        shim.read("4", 99),
        (5, 5),
        Some(3),
    );
    check(
        "a failed call under a cap: the cores say what is capped",
        shim.read("x", 75),
        (3, 3),
        Some(3),
    );
    // One change, and a change and its undoing, inside the settle window.
    check("W then N for good: the narrow machine", shim.read("4,1", 75), (1, 1), None);
    check("N then W for good: the wide machine", shim.read("1,4", 75), (3, 3), None);
    check("W, N, W, W: a flip and its undoing", shim.read("4,1,4", 75), (3, 3), None);
    check("W, N, W, N, N", shim.read("4,1,4,1", 75), (1, 1), None);
    check("N, W, N, N: a flip of the cores alone", shim.read("1,4,1", 75), (1, 1), None);
    check("W, W, N for good", shim.read("4,4,1", 75), (1, 1), None);
    check("W, N, N, N", shim.read("4,1,1", 75), (1, 1), None);
    // The sequences that would pair the count of one machine with the cores of another, if a settle took them: five reads of a strict
    // alternation, at both phases (the count, the cores, the count, the cores again, the count).
    let machines_75 = [machine(4, usize::MAX, 75), machine(1, usize::MAX, 75)];
    for (name, script) in [
        ("W, N, W, N, W (then W)", "4,1,4,1,4"),
        ("W, N, W, N, W, N, W...", &cycle_script(&[4, 1], 0)),
        ("N, W, N, W, N, W, N...", &cycle_script(&[4, 1], 1)),
        ("W, N, W, N, W (then N)", "4,1,4,1,4,1"),
    ] {
        let got = shim.read(script, 75);
        eprintln!(
            "CHECKER-P5 scripted: {name}: range {:?} in {} reads; the machines' are {machines_75:?}",
            got.0, got.1
        );
        if !machines_75.contains(&got.0) && got.0 != (1, 16) {
            failures.borrow_mut().push(format!(
                "{name}: {:?}, the range of no machine (the wide one's is {:?}, the narrow one's {:?}): the count of one affinity with the cores of another",
                got.0, machines_75[0], machines_75[1]
            ));
        }
    }
    // Every cycle of one to four CPUs up to length six, at every phase and a spread of budgets: each answer is one of the cycle's machines'
    // or the wide fallback.
    let states = [1usize, 2, 3, 4];
    let (mut tried, mut torn, mut fallback) = (0usize, BTreeMap::<Vec<usize>, ((usize, usize), usize, u8)>::new(), 0usize);
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
                    let (got, _) = shim.read(&cycle_script(&cycle, phase), percent);
                    let valid: Vec<(usize, usize)> = cycle.iter().map(|&k| machine(k, usize::MAX, percent)).collect();
                    let widest = (1, 16usize.max(*cycle.iter().max().unwrap()));
                    if got == widest && !valid.contains(&got) {
                        fallback += 1;
                    } else if !valid.contains(&got) {
                        torn.entry(cycle.clone()).or_insert((got, phase, percent));
                    }
                }
            }
        }
    }
    let mut shortest: Vec<_> = torn.iter().collect();
    shortest.sort_by_key(|(c, _)| c.len());
    eprintln!(
        "CHECKER-P5 scripted: {tried} cycle/phase/budget combinations: {} cycles gave the range of no machine, {fallback} answers were the fallback",
        torn.len()
    );
    for (cycle, (got, phase, percent)) in shortest.iter().take(6) {
        eprintln!("CHECKER-P5 scripted: torn: cycle {cycle:?} phase {phase} at {percent}%: {got:?}");
    }
    if !torn.is_empty() {
        let (cycle, (got, phase, percent)) = shortest[0];
        failures.borrow_mut().push(format!(
            "{} cycles of scripted affinity gave a range that is no machine's, the shortest {cycle:?} at phase {phase}, {percent}%: {got:?}",
            torn.len()
        ));
    }
    // The exhaustion: a machine that never settles is judged over every count it showed.
    let exhausting = cycle_script(&[2, 4, 2], 0);
    check(
        "a machine that never settles: 1..=16",
        shim.read(&exhausting, 75),
        (1, 16),
        None,
    );
    // (A machine of 40 CPUs caps its graph at 30 threads at 75 %: the range has to reach it.)
    let wide = cycle_script(&[4, 40, 1], 0);
    check(
        "... with 40 CPUs seen by the cores read: 1..=40",
        shim.read(&wide, 75),
        (1, 40),
        None,
    );
    check("... 1..=40 at 50 % too", shim.read(&wide, 50), (1, 40), None);
    let counted = cycle_script(&[4, 1, 40], 0);
    check(
        "... with 40 CPUs seen only by the count reads: a machine of 40 caps at 30 threads, the range has to reach them",
        shim.read(&counted, 75),
        (1, 40),
        None,
    );
    // A machine that stays unsettled for a few reads and then holds still is judged as the machine it became.
    let late = "1,1,2,1,1,2,1,1,2,2";
    check(
        "unsettled for three attempts, then 2 CPUs: the machine it became",
        shim.read(late, 75),
        machine(2, usize::MAX, 75),
        None,
    );
    check("... 50 %", shim.read(late, 50), machine(2, usize::MAX, 50), None);
    let failures = failures.into_inner();
    if failures.is_empty() {
        eprintln!("CHECKER-P5 scripted: all held");
    } else {
        for f in &failures {
            eprintln!("CHECKER-P5 scripted FAILURE: {f}");
        }
        panic!("{} failures", failures.len());
    }
}

fn plan_child() {
    let shim = Shim::find().expect("the shim is preloaded");
    let mut failures = Vec::new();
    let caps = caps();
    let keyed = plan_with(64, 64, vec![key()]);
    let blurred = plan_with(64, 64, vec![VideoEffect::Blur { sigma: 3.0 }]);
    let wants_key_refused = |reasons: &[kerf_core::Unsupported]| {
        reasons
            .iter()
            .any(|u| u.to_string().contains("slice-thread count is not known"))
    };
    let hazard = |reasons: &[kerf_core::Unsupported]| reasons.iter().any(|u| u.to_string().contains("a blur on a plane"));
    let judge = |script: &str, percent: u8| {
        kerf_core::set_cpu_percent(percent);
        shim.script(Some(script));
        let (k, b) = (
            keyed.reasons(&caps, keyed.size(u32::MAX)),
            blurred.reasons(&caps, blurred.size(u32::MAX)),
        );
        let exact = keyed.exact_graph_threads(&caps);
        shim.script(None);
        (k, b, exact)
    };
    // The wide fallback: the key refused, the blur judged over 1..=40 (40 threads leave a plane of 32 rows with empty slices).
    let (k, b, exact) = judge(&cycle_script(&[4, 40, 1], 0), 75);
    eprintln!(
        "CHECKER-P5 plan: fallback 1..=40: key refused {} blur refused {} exact {exact:?}",
        wants_key_refused(&k),
        hazard(&b)
    );
    if !wants_key_refused(&k) || exact.is_some() {
        failures.push(format!(
            "fallback 1..=40: the keyed layer was not refused ({k:?}, exact {exact:?})"
        ));
    }
    if !hazard(&b) {
        failures.push("fallback 1..=40: a 64x64 blur was cleared over a range that holds 40 slice threads".into());
    }
    // The fallback of the usual width: 64x64 is clear at every count to 16.
    let (k, b, exact) = judge(&cycle_script(&[2, 4, 2], 0), 75);
    eprintln!(
        "CHECKER-P5 plan: fallback 1..=16: key refused {} blur refused {} exact {exact:?}",
        wants_key_refused(&k),
        hazard(&b)
    );
    if !wants_key_refused(&k) || exact.is_some() {
        failures.push("fallback 1..=16: the keyed layer was not refused".into());
    }
    if hazard(&b) {
        failures.push("fallback 1..=16: a 64x64 blur was refused where every count to 16 is clear".into());
    }
    // A steady machine: the key is drawn at the cap, the blur is cleared.
    let (k, b, exact) = judge("4", 75);
    if wants_key_refused(&k) || hazard(&b) || exact != Some(3) {
        failures.push(format!(
            "a steady machine of 4 CPUs at 75%: key {k:?}, blur {b:?}, exact {exact:?}"
        ));
    }
    // The torn sequence: W N W N W. The key must be refused or drawn at one machine's count (3 for the wide, 1 for the narrow), never 5.
    for (name, script) in [
        ("W,N,W,N,W then W", "4,1,4,1,4".to_string()),
        ("strict alternation", cycle_script(&[4, 1], 0)),
    ] {
        let (k, _, exact) = judge(&script, 75);
        eprintln!(
            "CHECKER-P5 plan: {name}: key refused {} exact {exact:?} (the machines cap at 3 and 1)",
            wants_key_refused(&k)
        );
        if let Some(t) = exact {
            if t != 3 && t != 1 {
                failures.push(format!(
                    "{name}: the keyed layer is drawn at {t} slice threads, a count of neither machine (they run 3 and 1)"
                ));
            }
        }
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

fn flip_run(percent: u8, flip: Flip, reads: usize) -> BTreeMap<(usize, usize), usize> {
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
            let mut seen: BTreeMap<(usize, usize), usize> = BTreeMap::new();
            for _ in 0..reads {
                progress.fetch_add(1, Ordering::AcqRel);
                *seen.entry(kerf_core::cpu_graph_thread_range()).or_default() += 1;
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
            let seen = flip_run(percent, flip, reads);
            let valid: Vec<(usize, usize)> = (1..=count).map(|k| machine(k, usize::MAX, percent)).collect();
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
fn checker_p5_torn_a_scripted_machine_gives_one_machines_range_or_the_wide_fallback() {
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
fn checker_p5_torn_a_scripted_machine_drives_the_plan_to_refuse_the_key_and_judge_the_blur_over_every_count() {
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
