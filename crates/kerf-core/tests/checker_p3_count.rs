//! `checker/p3/count`: the count `kerf_core::ffmpeg_cpu_count()` says FFmpeg reads and the range of slice threads the plan judges a blur
//! over, held to the kernel's own account of the thread's affinity (no FFmpeg in the way) at every change of it, and to what FFmpeg's
//! `av_cpu_count` does when its call fails.
//!
//! * `checker_p3_count_follows_every_change_of_the_threads_affinity`: a thread narrows and widens its own affinity with
//!   `sched_setaffinity` (what `taskset -p` and a cpuset resize do to a task); after each change the count is what
//!   `/proc/thread-self/status` lists, and the range is the one FFmpeg's default (`min(count + 1, 16)`, one on one CPU) or the budget's cap
//!   gives, at budgets 100, 75, 50, 25 and 10 %.
//! * `checker_p3_count_a_failing_call_is_no_count_and_its_range_holds_the_one_cpu_ffmpeg_then_counts`: a child with `sched_getaffinity`
//!   failing (a seccomp filter; a host with more CPUs than a `cpu_set_t` holds is the same call failing): `av_cpu_count` says **one** then
//!   (`int nb_cpus = 1;` and the call's result is only used when it succeeds, in 6.1.1 and 9.0.2 alike), so the FFmpeg that inherits the
//!   filter runs its graph on one slice thread. The count must be `None`, and the range the plan judges over must contain that one.
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr)]

use std::process::{Command, Stdio};

const CHILD: &str = "CHECKER_P3_COUNT_CHILD";

extern "C" {
    fn sched_getaffinity(pid: i32, size: usize, mask: *mut u8) -> i32;
    fn sched_setaffinity(pid: i32, size: usize, mask: *const u8) -> i32;
    fn prctl(option: i32, a2: u64, a3: u64, a4: u64, a5: u64) -> i32;
    fn syscall(num: i64, ...) -> i64;
}

/// The CPUs the calling thread may run on.
fn allowed() -> Vec<usize> {
    let mut mask = [0u8; 128];
    // SAFETY: the buffer is the size the call is told.
    let ok = unsafe { sched_getaffinity(0, mask.len(), mask.as_mut_ptr()) } == 0;
    assert!(ok, "sched_getaffinity");
    (0..mask.len() * 8).filter(|&c| mask[c / 8] & (1 << (c % 8)) != 0).collect()
}

/// The calling thread restricted to `cpus`.
fn restrict(cpus: &[usize]) -> bool {
    let mut mask = [0u8; 128];
    for &c in cpus {
        mask[c / 8] |= 1 << (c % 8);
    }
    // SAFETY: the buffer is the size the call is told.
    unsafe { sched_setaffinity(0, mask.len(), mask.as_ptr()) == 0 }
}

/// `Cpus_allowed_list` of this thread, as the kernel reports it.
fn listed() -> usize {
    let status = std::fs::read_to_string("/proc/thread-self/status").expect("/proc/thread-self/status");
    let list = status
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
        .expect("Cpus_allowed_list")
        .trim()
        .to_string();
    list.split(',')
        .map(|part| match part.trim().split_once('-') {
            Some((a, b)) => b.parse::<usize>().unwrap() - a.parse::<usize>().unwrap() + 1,
            None => 1,
        })
        .sum()
}

/// FFmpeg's default slice threads for `counted` CPUs (`avpriv_slicethread_create`), written out here.
fn ffmpeg_default(counted: usize) -> usize {
    if counted > 1 {
        (counted + 1).min(16)
    } else {
        1
    }
}

/// The budget's threads at `percent` of `cores`, written out here (`threads_for`).
fn budget(cores: usize, percent: u8) -> usize {
    ((cores as f64 * f64::from(percent) / 100.0).round() as usize).clamp(1, cores)
}

#[test]
fn checker_p3_count_follows_every_change_of_the_threads_affinity() {
    let was = kerf_core::cpu_percent();
    let result = std::thread::spawn(|| {
        let all = allowed();
        let n = all.len();
        let mut failures = Vec::new();
        let mut seen = 0usize;
        let sizes: Vec<usize> = [n, 1, 2.min(n), 3.min(n), n, 1, n.div_ceil(2), n]
            .into_iter()
            .filter(|&k| k >= 1)
            .collect();
        for k in sizes {
            if !restrict(&all[..k]) {
                return Err("sched_setaffinity is not allowed here".to_string());
            }
            for percent in [100u8, 75, 50, 25, 10] {
                kerf_core::set_cpu_percent(percent);
                let count = kerf_core::ffmpeg_cpu_count();
                let (cores, range) = (kerf_core::cpu_cores(), kerf_core::cpu_graph_thread_range());
                let want_count = listed();
                // (`cores` is the affinity with a cgroup's CPU quota applied: at most the affinity.)
                if count != Some(want_count) || want_count != k || cores > k || cores == 0 {
                    failures.push(format!(
                        "{k} CPUs at {percent}%: count {count:?}, listed {want_count}, cores {cores}"
                    ));
                }
                let b = budget(cores, percent);
                let want = if percent < 100 && b < cores {
                    (b, b)
                } else {
                    (ffmpeg_default(k), ffmpeg_default(k))
                };
                if range != want {
                    failures.push(format!(
                        "{k} CPUs at {percent}% (cores {cores}): range {range:?}, want {want:?}"
                    ));
                }
                seen += 1;
            }
        }
        if failures.is_empty() {
            Ok(seen)
        } else {
            Err(failures.join("\n"))
        }
    })
    .join()
    .expect("the thread ends");
    kerf_core::set_cpu_percent(was);
    match result {
        Ok(seen) => eprintln!("CHECKER-P3 count: {seen} (affinity, budget) pairs held"),
        Err(e) if e.contains("not allowed") => eprintln!("CHECKER-P3 count: {e}: not run"),
        Err(e) => panic!("{e}"),
    }
}

/// Makes `sched_getaffinity` fail with `EPERM` for every thread of this process (a seccomp filter).
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
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        return false;
    }
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
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
        // SAFETY: plain system calls with a program that outlives them; `TSYNC` (1) applies the filter to every thread.
        unsafe { prctl(38, 1, 0, 0, 0) == 0 && syscall(SECCOMP, 1u64, 1u64, &fprog as *const Prog) == 0 }
    }
}

#[test]
fn checker_p3_count_child() {
    if std::env::var(CHILD).is_err() {
        return;
    }
    if !forbid_sched_getaffinity() {
        eprintln!("CHECKER-P3 count child: no filter");
        return;
    }
    let (count, range) = (kerf_core::ffmpeg_cpu_count(), kerf_core::cpu_graph_thread_range());
    eprintln!(
        "CHECKER-P3 count child: count={} range={}-{} cores={} graph_threads={}",
        count.map_or(-1, |c| c as i64),
        range.0,
        range.1,
        kerf_core::cpu_cores(),
        kerf_core::cpu_graph_threads()
    );
}

#[test]
fn checker_p3_count_a_failing_call_is_no_count_and_its_range_holds_the_one_cpu_ffmpeg_then_counts() {
    let exe = std::env::current_exe().expect("the test binary");
    let out = Command::new(exe)
        .args(["checker_p3_count_child", "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("KERF_CPU_PERCENT", "100")
        .stdin(Stdio::null())
        .output()
        .expect("the child runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let Some(line) = text.lines().find(|l| l.starts_with("CHECKER-P3 count child:")) else {
        panic!("the child said nothing:\n{text}");
    };
    if line.contains("no filter") {
        eprintln!("CHECKER-P3 count: no seccomp filter on this architecture: not run");
        return;
    }
    eprintln!("{line}");
    let get = |key: &str| -> String {
        line.split(&format!("{key}="))
            .nth(1)
            .and_then(|r| r.split(' ').next())
            .unwrap_or_else(|| panic!("no {key} in {line}"))
            .to_string()
    };
    assert_eq!(get("count"), "-1", "the count is None when the call fails: {line}");
    let (lo, hi) = get("range")
        .split_once('-')
        .map(|(a, b)| (a.parse::<i64>().unwrap(), b.parse::<i64>().unwrap()))
        .expect("a range");
    // `av_cpu_count` starts from `nb_cpus = 1` and keeps it when `sched_getaffinity` fails: the FFmpeg that inherits the filter counts one
    // CPU and runs its graph on `1` slice thread, which the range the plan judges over must hold.
    assert!(
        lo <= 1 && 1 <= hi,
        "the plan judges a blur over {lo}..={hi} slice threads when the call fails; FFmpeg's graph then runs on 1 (`av_cpu_count` keeps its \
         initial 1): {line}"
    );
}
