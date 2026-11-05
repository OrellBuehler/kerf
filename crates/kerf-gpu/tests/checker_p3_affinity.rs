//! `checker/p3/affinity`: what FFmpeg's graph will run on, as the plan judges it, through the real path (an export of a cut, the plan's
//! verdict on a frame of it, the GPU's planes against the export's) when the machine is not the one the process started on.
//!
//! * `checker_p3_affinity_a_blur_export_is_exact_or_refused_through_changes_of_the_affinity`: a child under `taskset -c 0-3` that exports
//!   and judges and draws a blurred noise clip of 12 columns and 16 to 94 rows, then narrows **its own thread's** affinity from outside
//!   (`taskset -p`: one CPU, two, three, back to four, one other), exporting and judging again after each, at budgets 100, 75 and 50 %. No
//!   frame the plan clears may differ from FFmpeg's export in a sample; the frames it refuses are drawn anyway where a thread count makes
//!   the plan draw them, and must not be the export's.
//! * `checker_p3_affinity_a_failing_sched_getaffinity_is_no_count_and_no_assumed_thread_count`: a child with `sched_getaffinity` forbidden
//!   by a seccomp filter (which FFmpeg's children inherit: `av_cpu_count` says one then; the same would be a host with more CPUs than a
//!   `cpu_set_t` holds). The count must be `None`, and what the plan clears of a blur and of a chroma key (which the compositor draws
//!   at one slice thread count, `Compositor::graph_threads_for`) must be the export's.
//! * `checker_p3_scrub_*`: the OpenMP binding variables in the environment Kerf is started in (`OMP_PROC_BIND`, `OMP_PLACES`,
//!   `GOMP_CPU_AFFINITY`, `KMP_AFFINITY`, many values each, and all together) do not change the core count the FFmpeg the engine spawns
//!   reads, which is the count the plan reproduces; and a wrapper standing in for `ffmpeg` and `ffprobe` logs the environment of every
//!   child that the probes, an export, a still and a frame decode start, none of which holds one of them.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_p3_affinity -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr)]

#[path = "common/checker_p3.rs"]
mod p3;

use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use kerf_core::{Clip, ExportOptions, StreamKind, Timeline, Track, VideoEffect};
use p3::{blurred, child, compositor, effect_clip, gpu, lavfi_clip, motion_frame, narrow_thread, noise_clip, one_track, rerun};

static SERIAL: Mutex<()> = Mutex::new(());

const CHILD: &str = "CHECKER_P3_AFFINITY_CHILD";
const LIMIT: Duration = Duration::from_secs(1500);

/// Canvases whose chroma and luma planes sit either side of `gblur`'s narrow-block rule (12 columns: 16 rows a slice is a vector of rows
/// over a narrow plane) for one to six slice threads.
const CANVASES: [(u32, u32); 7] = [(12, 16), (12, 31), (12, 46), (12, 64), (12, 76), (12, 94), (12, 111)];

/// The masks the child's thread goes through (the first is what the parent started it on).
const STAGES: [&str; 6] = ["0-3", "0", "0,1", "0-2", "0-3", "1-2"];

fn cpus_of(mask: &str) -> usize {
    mask.split(',')
        .map(|p| match p.split_once('-') {
            Some((a, b)) => b.parse::<usize>().unwrap() - a.parse::<usize>().unwrap() + 1,
            None => 1,
        })
        .sum()
}

// ---- the children ------------------------------------------------------------------------------------------------------------------------

#[test]
#[ignore = "run by the parents below, in a process of its own"]
fn checker_p3_affinity_child() {
    match std::env::var(CHILD).as_deref() {
        Ok("mid") => child_mid(),
        Ok("none") => child_none(),
        Ok("scrub") => child_scrub(),
        Ok("wrapped") => child_wrapped(),
        _ => {}
    }
}

fn child_mid() {
    for (stage, mask) in STAGES.iter().enumerate() {
        if stage > 0 {
            narrow_thread(mask);
        }
        let count = kerf_core::ffmpeg_cpu_count();
        let range = kerf_core::cpu_graph_thread_range();
        for (w, h) in CANVASES {
            let asset = noise_clip(w, h, "yuv420p");
            let tl = one_track(vec![blurred(&asset, 3.0, 0.3)]);
            let out = motion_frame(&format!("aff-{stage}-{w}x{h}"), &tl, std::slice::from_ref(&asset), 30.0, 2);
            eprintln!(
                "CHECKER-P3 aff: stage {stage} mask {mask} cpus {} count {count:?} range {range:?} cores {} canvas {w}x{h} {}: {out:?}",
                cpus_of(mask),
                kerf_core::cpu_cores(),
                out.word()
            );
        }
    }
}

#[test]
#[ignore = "needs ffmpeg, taskset and a GPU adapter (lavapipe is enough)"]
fn checker_p3_affinity_a_blur_export_is_exact_or_refused_through_changes_of_the_affinity() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !p3::have_taskset() {
        eprintln!("CHECKER-P3 aff: no taskset on this host: not run");
        return;
    }
    let mut failures = Vec::new();
    let mut refused_at_one = 0usize;
    let mut exact_total = 0usize;
    for percent in ["100", "75", "50"] {
        let (ok, text) = rerun(
            "checker_p3_affinity_child",
            Some("0-3"),
            &[(CHILD, "mid"), ("KERF_CPU_PERCENT", percent)],
            LIMIT,
        );
        let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("CHECKER-P3 aff:")).collect();
        eprintln!(
            "CHECKER-P3 aff: budget {percent}%: {} frames judged, child ok {ok}",
            lines.len()
        );
        if !ok || lines.len() != STAGES.len() * CANVASES.len() {
            failures.push(format!("budget {percent}%: the child failed or said too little:\n{text}"));
            continue;
        }
        for l in lines {
            let short = l.split(": Differs").next().unwrap_or(l);
            if l.contains(" DIFFERS:") {
                failures.push(format!("budget {percent}%: {l}"));
            } else if l.contains(" EXACT:") {
                exact_total += 1;
            } else if l.contains("REFUSED-BUT-EXACT-DRAWN-ANYWAY") {
                eprintln!("CHECKER-P3 aff (info) budget {percent}%: refusal the draw does not need: {short}");
            }
            if l.contains("cpus 1 ") && l.contains(" REFUSED") {
                refused_at_one += 1;
            }
        }
    }
    eprintln!("CHECKER-P3 aff: {exact_total} frames drawn and exact, {refused_at_one} refused on one CPU");
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    assert!(
        exact_total > 0 && refused_at_one > 0,
        "the sweep never met both a draw and a refusal"
    );
}

// ---- sched_getaffinity fails ----------------------------------------------------------------------------------------------------------------

/// Makes `sched_getaffinity` fail with `EPERM` for every thread of this process and every child it starts (a seccomp filter).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn forbid_sched_getaffinity() -> bool {
    #[repr(C)]
    struct Filter {
        code: u16,
        jt: u8,
        jf: u8,
        k: u32,
    }
    #[repr(C)]
    struct Prog {
        len: u16,
        filter: *const Filter,
    }
    extern "C" {
        fn prctl(option: i32, a2: u64, a3: u64, a4: u64, a5: u64) -> i32;
        fn syscall(num: i64, ...) -> i64;
    }
    #[cfg(target_arch = "x86_64")]
    const GETAFFINITY: u32 = 204;
    #[cfg(target_arch = "x86_64")]
    const SECCOMP: i64 = 317;
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const GETAFFINITY: u32 = 123;
    #[cfg(target_arch = "aarch64")]
    const SECCOMP: i64 = 277;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc000_00b7;
    const LD_W_ABS: u16 = 0x20;
    const JEQ_K: u16 = 0x15;
    const RET_K: u16 = 0x06;
    const ALLOW: u32 = 0x7fff_0000;
    const ERRNO_EPERM: u32 = 0x0005_0000 | 1;
    let prog = [
        Filter {
            code: LD_W_ABS,
            jt: 0,
            jf: 0,
            k: 4,
        },
        Filter {
            code: JEQ_K,
            jt: 1,
            jf: 0,
            k: ARCH,
        },
        Filter {
            code: RET_K,
            jt: 0,
            jf: 0,
            k: ALLOW,
        },
        Filter {
            code: LD_W_ABS,
            jt: 0,
            jf: 0,
            k: 0,
        },
        Filter {
            code: JEQ_K,
            jt: 0,
            jf: 1,
            k: GETAFFINITY,
        },
        Filter {
            code: RET_K,
            jt: 0,
            jf: 0,
            k: ERRNO_EPERM,
        },
        Filter {
            code: RET_K,
            jt: 0,
            jf: 0,
            k: ALLOW,
        },
    ];
    let fprog = Prog {
        len: prog.len() as u16,
        filter: prog.as_ptr(),
    };
    // SAFETY: plain system calls with a program that outlives them; `TSYNC` (1) applies the filter to every thread of the process.
    unsafe { prctl(38, 1, 0, 0, 0) == 0 && syscall(SECCOMP, 1u64, 1u64, &fprog as *const Prog) == 0 }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn forbid_sched_getaffinity() -> bool {
    false
}

/// "detected N logical cores", from the FFmpeg the engine spawns.
fn ffmpeg_detects() -> Option<usize> {
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args([
        "-nostdin",
        "-hide_banner",
        "-v",
        "debug",
        "-f",
        "lavfi",
        "-i",
        "nullsrc=s=64x64:d=0.1,gblur=sigma=1",
        "-f",
        "null",
        "-",
    ]);
    let out = child::output_within(cmd, Duration::from_secs(60)).ok()?;
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("detected ")?.split_whitespace().next()?.parse().ok())
}

fn keyed_cut() -> (Timeline, Vec<kerf_core::Asset>) {
    let bars = lavfi_clip("keyed-bars-64x64", "testsrc2=size=64x64:rate=30:duration=0.4,format=yuv420p");
    let noise = noise_clip(64, 64, "yuv420p");
    let fg = effect_clip(
        &noise,
        vec![VideoEffect::ChromaKey {
            color: "0x808080".into(),
            similarity: 0.25,
            blend: 0.2,
        }],
        0.3,
    );
    let under = Clip::new(bars.id, 0.0, 0.3, 0.0);
    let tl = Timeline {
        tracks: vec![
            Track {
                clips: vec![under],
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

fn child_none() {
    let _ = gpu();
    assert!(forbid_sched_getaffinity(), "could not install the seccomp filter");
    let count = kerf_core::ffmpeg_cpu_count();
    let range = kerf_core::cpu_graph_thread_range();
    eprintln!(
        "CHECKER-P3 none: count {count:?} range {range:?} cores {} graph_threads {} ffmpeg detects {:?}",
        kerf_core::cpu_cores(),
        kerf_core::cpu_graph_threads(),
        ffmpeg_detects()
    );
    for (w, h) in CANVASES {
        let asset = noise_clip(w, h, "yuv420p");
        let tl = one_track(vec![blurred(&asset, 3.0, 0.3)]);
        let out = motion_frame(&format!("none-{w}x{h}"), &tl, std::slice::from_ref(&asset), 30.0, 2);
        eprintln!("CHECKER-P3 none: blur canvas {w}x{h} {}: {out:?}", out.word());
    }
    let (tl, assets) = keyed_cut();
    let out = motion_frame("none-keyed", &tl, &assets, 30.0, 2);
    eprintln!("CHECKER-P3 none: chroma key over bars 64x64 {}: {out:?}", out.word());
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p3_affinity_a_failing_sched_getaffinity_is_no_count_and_no_assumed_thread_count() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (ok, text) = rerun(
        "checker_p3_affinity_child",
        None,
        &[(CHILD, "none"), ("KERF_CPU_PERCENT", "100")],
        LIMIT,
    );
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("CHECKER-P3 none:")).collect();
    for l in &lines {
        eprintln!("{l}");
    }
    assert!(
        ok && lines.len() > CANVASES.len(),
        "the child failed or said too little:\n{text}"
    );
    let mut failures = Vec::new();
    let head = lines[0];
    if !head.contains("count None") {
        failures.push(format!("the count is not None when the call fails: {head}"));
    }
    for l in &lines[1..] {
        if l.contains(" DIFFERS:") {
            failures.push(format!(
                "the plan cleared a frame and the GPU's draw differs from the export: {l}"
            ));
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

// ---- the environment of a child --------------------------------------------------------------------------------------------------------------

fn child_scrub() {
    let engine = ffmpeg_detects();
    let mut plain = Command::new(kerf_core::ffmpeg_path());
    plain.args([
        "-nostdin",
        "-hide_banner",
        "-v",
        "debug",
        "-f",
        "lavfi",
        "-i",
        "nullsrc=s=64x64:d=0.1,gblur=sigma=1",
        "-f",
        "null",
        "-",
    ]);
    let plain = child::output_within(plain, Duration::from_secs(60)).ok().and_then(|out| {
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .find_map(|l| l.strip_prefix("detected ")?.split_whitespace().next()?.parse::<usize>().ok())
    });
    eprintln!(
        "CHECKER-P3 scrub: plan_count {:?} engine_ffmpeg_detects {engine:?} unscrubbed_ffmpeg_detects {plain:?}",
        kerf_core::ffmpeg_cpu_count()
    );
}

/// The settings of the four variables the engine removes, and others around them that must not change the count either.
fn scrub_cases() -> Vec<Vec<(&'static str, &'static str)>> {
    let mut cases: Vec<Vec<(&str, &str)>> = Vec::new();
    for v in [
        "true",
        "close",
        "spread",
        "master",
        "primary",
        "TRUE",
        "true,close",
        "spread,close",
        "1",
        "false",
    ] {
        cases.push(vec![("OMP_PROC_BIND", v)]);
    }
    for v in [
        "cores",
        "threads",
        "sockets",
        "numa_domains",
        "{0}",
        "{0}:4",
        "{0:2}",
        "cores(1)",
        "threads(2)",
    ] {
        cases.push(vec![("OMP_PLACES", v)]);
    }
    for v in ["0", "0-3", "3 2 1 0", "0-3:2", "1,2", "2"] {
        cases.push(vec![("GOMP_CPU_AFFINITY", v)]);
    }
    for v in [
        "compact",
        "scatter",
        "granularity=fine,compact",
        "verbose,none",
        "explicit,proclist=[0]",
    ] {
        cases.push(vec![("KMP_AFFINITY", v)]);
    }
    cases.push(vec![
        ("OMP_PROC_BIND", "true"),
        ("OMP_PLACES", "cores"),
        ("GOMP_CPU_AFFINITY", "0"),
        ("KMP_AFFINITY", "compact"),
    ]);
    // (Tuning, not binding: they stay, and the count stays.)
    for (k, v) in [
        ("OMP_NUM_THREADS", "1"),
        ("OMP_THREAD_LIMIT", "1"),
        ("OMP_WAIT_POLICY", "passive"),
        ("OMP_DYNAMIC", "true"),
        ("GOMP_SPINCOUNT", "0"),
        ("OMP_DISPLAY_AFFINITY", "true"),
        ("OMP_DISPLAY_ENV", "true"),
    ] {
        cases.push(vec![(k, v)]);
    }
    cases
}

#[test]
#[ignore = "needs ffmpeg and taskset"]
fn checker_p3_scrub_the_openmp_binding_variables_do_not_change_the_count_ffmpeg_reads() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !p3::have_taskset() {
        eprintln!("CHECKER-P3 scrub: no taskset on this host: not run");
        return;
    }
    let (mut failures, mut unscrubbed_narrowed) = (Vec::new(), 0usize);
    let cases = scrub_cases();
    for env in &cases {
        let mut envs: Vec<(&str, &str)> = vec![(CHILD, "scrub"), ("KERF_CPU_PERCENT", "100")];
        envs.extend(env.iter().copied());
        let (ok, text) = rerun("checker_p3_affinity_child", Some("0-3"), &envs, Duration::from_secs(120));
        let Some(line) = text.lines().find(|l| l.starts_with("CHECKER-P3 scrub:")) else {
            failures.push(format!("{env:?}: the child said nothing (ok {ok}):\n{text}"));
            continue;
        };
        eprintln!("{env:?}: {line}");
        let get = |key: &str| -> String {
            line.split(&format!("{key} "))
                .nth(1)
                .unwrap_or("?")
                .split_whitespace()
                .next()
                .unwrap_or("?")
                .to_string()
        };
        let (plan, engine, plain) = (
            get("plan_count"),
            get("engine_ffmpeg_detects"),
            get("unscrubbed_ffmpeg_detects"),
        );
        if plan != "Some(4)" || engine != "Some(4)" {
            failures.push(format!(
                "{env:?}: the plan counts {plan}, the FFmpeg the engine spawns detects {engine} (4 CPUs are allowed)"
            ));
        }
        if plain != "Some(4)" {
            unscrubbed_narrowed += 1;
        }
    }
    eprintln!(
        "CHECKER-P3 scrub: {} environments tried; an FFmpeg started without the engine's scrub counted fewer than 4 in {unscrubbed_narrowed} of them (the control: only a libgomp build does)",
        cases.len()
    );
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

// ---- every spawn ----------------------------------------------------------------------------------------------------------------------------

fn child_wrapped() {
    let log = std::env::var("CHECKER_P3_LOG").expect("the log");
    // The control: a child started without the engine shows the wrapper logs what it sees.
    let direct = Command::new(std::env::var("KERF_FFMPEG").expect("the wrapper"))
        .arg("-version")
        .output()
        .expect("the wrapper runs");
    assert!(direct.status.success());
    // ffprobe, an export, a still, a frame decode, the probes.
    let asset = noise_clip(12, 64, "yuv420p");
    let tl = one_track(vec![blurred(&asset, 3.0, 0.3)]);
    let _ = motion_frame("wrapped-blur", &tl, std::slice::from_ref(&asset), 30.0, 2);
    let still =
        kerf_core::timeline_frame_yuv_cancellable(&tl, std::slice::from_ref(&asset), &ExportOptions::default(), 0.1, 12, &|| {
            false
        });
    eprintln!("CHECKER-P3 wrapped: still ok {}", still.is_ok());
    let (keyed, assets) = keyed_cut();
    let _ = motion_frame("wrapped-keyed", &keyed, &assets, 30.0, 2);
    let _ = compositor(None);
    eprintln!("CHECKER-P3 wrapped: log {log}");
}

#[test]
#[ignore = "needs ffmpeg, taskset and a GPU adapter (lavapipe is enough)"]
fn checker_p3_scrub_no_child_the_engine_or_the_compositor_starts_holds_a_binding_variable() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = p3::scratch("checker-p3").join(format!("wrap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("calls.log");
    let _ = std::fs::remove_file(&log);
    let real_ffmpeg = kerf_core::ffmpeg_path();
    let real_ffprobe = std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".into());
    let write = |name: &str, real: &str| {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        let script = format!(
            "#!/bin/sh\nprintf 'CALL {name} %s\\n' \"$(env | grep -E '^(OMP_PROC_BIND|OMP_PLACES|GOMP_CPU_AFFINITY|KMP_AFFINITY)=' | tr '\\n' ' ')\" >> \"$CHECKER_P3_LOG\"\nexec '{real}' \"$@\"\n"
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let ffmpeg = write("ffmpeg", &real_ffmpeg);
    let ffprobe = write("ffprobe", &real_ffprobe);
    let (ok, text) = rerun(
        "checker_p3_affinity_child",
        Some("0-3"),
        &[
            (CHILD, "wrapped"),
            ("KERF_CPU_PERCENT", "100"),
            ("CHECKER_P3_LOG", log.to_str().unwrap()),
            ("KERF_FFMPEG", ffmpeg.to_str().unwrap()),
            ("KERF_FFPROBE", ffprobe.to_str().unwrap()),
            ("OMP_PROC_BIND", "true"),
            ("OMP_PLACES", "cores"),
            ("GOMP_CPU_AFFINITY", "0-3"),
            ("KMP_AFFINITY", "compact"),
        ],
        LIMIT,
    );
    assert!(ok, "the child failed:\n{text}");
    let calls = std::fs::read_to_string(&log).expect("the wrapper's log");
    let lines: Vec<&str> = calls.lines().collect();
    let (ffmpegs, ffprobes) = (
        lines.iter().filter(|l| l.starts_with("CALL ffmpeg ")).count(),
        lines.iter().filter(|l| l.starts_with("CALL ffprobe ")).count(),
    );
    eprintln!(
        "CHECKER-P3 wrapped: {} children logged ({ffmpegs} ffmpeg, {ffprobes} ffprobe)",
        lines.len()
    );
    assert!(
        ffmpegs >= 20 && ffprobes >= 2,
        "too few children went through the wrapper: {ffmpegs} / {ffprobes}"
    );
    // The first call is the control (started by the test, not the engine): it shows the wrapper sees the variables.
    assert!(
        lines[0].contains("OMP_PROC_BIND=true") && lines[0].contains("KMP_AFFINITY=compact"),
        "the control did not show the variables: {}",
        lines[0]
    );
    let held: Vec<&&str> = lines[1..].iter().filter(|l| l.contains('=')).collect();
    assert!(
        held.is_empty(),
        "{} of {} children were started with a binding variable, the first: {}",
        held.len(),
        lines.len() - 1,
        held[0]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
