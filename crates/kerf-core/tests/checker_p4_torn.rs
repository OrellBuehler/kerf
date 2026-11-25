//! `checker/p4/torn`: the range the plan judges a graph over, read while the affinity of the reading thread is flipped under it (another
//! thread calls `sched_setaffinity` on its id, which is what a cpuset resize does to a task between two of its system calls).
//!
//! `graph_thread_range` reads FFmpeg's count (one `sched_getaffinity`) and, under a cap, the cores (a second one, with the cgroup's quota),
//! so a flip between the two reads can give a pair that belongs to no single affinity. The cores and the budget are one read (the budget is
//! derived from the cores read, never from a second), so the cap is always a share of the cores it is compared with; what can still tear
//! is the pair (count, cores). This test records which answers occur: each answer must be one of the two machines' (`valid`) or the one a
//! count of the wide machine with the cores of the narrow one gives (`torn`, reported); at 100 % (one read) none is torn.
//!
//! ```text
//! cargo test -p kerf-core --no-default-features --test checker_p4_torn -- --ignored --nocapture
//! ```
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn tid() -> i32 {
    // SAFETY: `gettid` takes no arguments and cannot fail.
    unsafe { libc::syscall(libc::SYS_gettid) as i32 }
}

fn set_affinity(tid: i32, cpus: &[usize]) -> bool {
    // SAFETY: the set is plain bits and the call only reads it.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &c in cpus {
            libc::CPU_SET(c, &mut set);
        }
        libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
    }
}

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

/// The range for `cpus` CPUs at `percent`, written out here.
fn range(cpus: usize, percent: u8) -> (usize, usize) {
    let default = |count: usize| if count > 1 { (count + 1).min(16) } else { 1 };
    let budget = ((cpus as f64 * f64::from(percent) / 100.0).round()).clamp(1.0, cpus as f64) as usize;
    if percent < 100 && budget < cpus {
        (budget, budget)
    } else {
        (default(cpus), default(cpus))
    }
}

fn read_while_flipping(percent: u8) -> BTreeMap<(usize, usize), usize> {
    kerf_core::set_cpu_percent(percent);
    let all = allowed();
    let wide = all.len();
    let narrow = &all[..1];
    let reader_tid = Arc::new(AtomicI32::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let flipper = {
        let (reader_tid, stop, all) = (reader_tid.clone(), stop.clone(), all.clone());
        let narrow = narrow.to_vec();
        std::thread::spawn(move || {
            while reader_tid.load(Ordering::Acquire) == 0 {
                std::thread::yield_now();
            }
            let t = reader_tid.load(Ordering::Acquire);
            let mut wide_now = true;
            while !stop.load(Ordering::Acquire) {
                wide_now = !wide_now;
                set_affinity(t, if wide_now { &all } else { &narrow });
                std::thread::sleep(Duration::from_micros(20));
            }
        })
    };
    let reader = std::thread::spawn({
        let (reader_tid, stop) = (reader_tid, stop);
        move || {
            reader_tid.store(tid(), Ordering::Release);
            let mut seen: BTreeMap<(usize, usize), usize> = BTreeMap::new();
            for _ in 0..300_000 {
                *seen.entry(kerf_core::cpu_graph_thread_range()).or_default() += 1;
            }
            stop.store(true, Ordering::Release);
            seen
        }
    });
    let seen = reader.join().expect("the reader ends");
    flipper.join().expect("the flipper ends");
    let _ = wide;
    seen
}

#[test]
#[ignore = "an adversarial race, run on purpose: it flips a thread's affinity every 20 microseconds"]
fn checker_p4_torn_the_range_is_one_machines_while_the_affinity_flips() {
    let all = allowed();
    if all.len() < 2 || !set_affinity(0, &all) || kerf_core::ffmpeg_cpu_count().is_none() {
        eprintln!("CHECKER-P4 torn: fewer than two CPUs, or no count here: not run");
        return;
    }
    let was = kerf_core::cpu_percent();
    let wide = all.len();
    let mut torn_total = 0usize;
    for percent in [100u8, 90, 75, 50, 25, 10] {
        let seen = read_while_flipping(percent);
        let valid = [range(wide, percent), range(1, percent)];
        let torn = (range(wide, percent).0.max(range(1, percent).0), 0);
        let torn_answer = {
            // The count of the wide machine, the cores of the narrow one.
            let default = |count: usize| if count > 1 { (count + 1).min(16) } else { 1 };
            (default(wide), default(wide))
        };
        let _ = torn;
        let mut torn_here = 0usize;
        let mut strange = Vec::new();
        for (answer, n) in &seen {
            if valid.contains(answer) {
                continue;
            }
            if *answer == torn_answer {
                torn_here += n;
            } else {
                strange.push((*answer, *n));
            }
        }
        eprintln!(
            "CHECKER-P4 torn: {percent}%: {seen:?}; valid {valid:?}; torn (count of {wide}, cores of 1) {torn_answer:?} x{torn_here}"
        );
        assert!(
            strange.is_empty(),
            "{percent}%: answers of no affinity and not the torn pair: {strange:?}"
        );
        if percent == 100 {
            assert_eq!(torn_here, 0, "at 100% the range is one read of the affinity: nothing tears");
        }
        torn_total += torn_here;
    }
    kerf_core::set_cpu_percent(was);
    eprintln!("CHECKER-P4 torn: {torn_total} answers of 1 800 000 were the pair of two affinities");
}
