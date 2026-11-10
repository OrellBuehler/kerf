//! `checker/svg/probe` and `checker/svg/cache`: whether the floating-title probe has teeth, how long it can take, and whether the picture
//! cache is bounded, correct under contention and forgiving of a failure for exactly as long as it says.
//!
//! The probe answers once per process, so every configuration is a child process of this test binary (re-run by name with
//! `CHECKER_SVG_CHILD` set) under `KERF_FFMPEG=<a spy>`: a shell script that runs the real FFmpeg, after rewriting the arguments of the
//! run that blends (`overlay=`), stalling it, failing it, cutting its output short or slowing every run, and logs each run's class
//! (`base`, `convert`, `overlay`) and arguments. The log is the evidence.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_svg_probe -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
#![cfg(unix)]
#![allow(clippy::print_stderr, clippy::print_stdout)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kerf_gpu::svg::{PictureCache, Weighed};
use kerf_gpu::GpuError;

#[path = "common/child.rs"]
mod child;

const CHILD: &str = "CHECKER_SVG_CHILD";

fn scratch() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-svg-probe");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The spy: the real FFmpeg (`CHECKER_SVG_REAL`) run after the rewrite the mode asks for. Written once per process to a file of its own and
/// renamed into place (two parent tests run on two threads and must not rewrite a script one of them is executing: `ETXTBSY`).
///
/// * class: `overlay` when an argument holds `overlay=`, `convert` when one holds `format=yuva420p`, else `base`;
/// * `CHECKER_SVG_SED`: a `sed` expression applied to the `overlay` run's graph;
/// * `CHECKER_SVG_EXTRA_OVERLAY`: arguments put ahead of the `overlay` run's;
/// * `CHECKER_SVG_EXTRA_ALL`: arguments put ahead of every run's;
/// * `CHECKER_SVG_STALL`, `CHECKER_SVG_FAIL`, `CHECKER_SVG_TRUNC`: a class (or `all`) whose run hangs (`exec sleep`, so a kill reaches it),
///   exits 1, or has its output cut to 1000 bytes;
/// * `CHECKER_SVG_DELAY`: seconds slept before every run.
fn spy() -> PathBuf {
    static SPY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    SPY.get_or_init(|| {
        let dir = scratch();
        let path = dir.join(format!("ffmpeg-spy-{}.sh", std::process::id()));
        let staged = dir.join(format!("ffmpeg-spy-{}.sh.new", std::process::id()));
        let script = r#"#!/bin/sh
class=base
for a in "$@"; do
  case "$a" in
    *overlay=*) class=overlay ;;
    *format=yuva420p*) [ "$class" = base ] && class=convert ;;
  esac
done
printf '%s %s\n' "$class" "$*" >> "$CHECKER_SVG_LOG"
[ -n "$CHECKER_SVG_DELAY" ] && sleep "$CHECKER_SVG_DELAY"
hit() { [ "$1" = all ] || [ "$1" = "$class" ]; }
if [ -n "$CHECKER_SVG_STALL" ] && hit "$CHECKER_SVG_STALL"; then exec sleep 3601.37; fi
if [ -n "$CHECKER_SVG_FAIL" ] && hit "$CHECKER_SVG_FAIL"; then echo "spy: failing" >&2; exit 1; fi
if [ "$class" = overlay ] && [ -n "$CHECKER_SVG_SED" ]; then
  n=$#; i=0
  while [ $i -lt $n ]; do
    a="$1"; shift
    case "$a" in *overlay=*) a=$(printf '%s' "$a" | sed -e "$CHECKER_SVG_SED") ;; esac
    set -- "$@" "$a"
    i=$((i+1))
  done
fi
extra="$CHECKER_SVG_EXTRA_ALL"
[ "$class" = overlay ] && extra="$extra $CHECKER_SVG_EXTRA_OVERLAY"
if [ -n "$CHECKER_SVG_TRUNC" ] && hit "$CHECKER_SVG_TRUNC"; then
  "$CHECKER_SVG_REAL" $extra "$@" | head -c 1000
  exit 0
fi
exec "$CHECKER_SVG_REAL" $extra "$@"
"#;
        std::fs::write(&staged, script).unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&staged, &path).unwrap();
        path
    })
    .clone()
}

struct Found {
    verdict: String,
    elapsed: Duration,
    log: Vec<String>,
}

impl Found {
    fn runs(&self, class: &str) -> usize {
        self.log.iter().filter(|l| l.starts_with(&format!("{class} "))).count()
    }
}

/// The probe in a process of its own, under the spy with `env`.
fn probe_in_a_child(tag: &str, env: &[(&str, &str)]) -> Found {
    let real = kerf_core::ffmpeg_path();
    let log = scratch().join(format!("log-{tag}.txt"));
    let _ = std::fs::remove_file(&log);
    let exe = std::env::current_exe().expect("the test binary");
    let mut cmd = Command::new(exe);
    cmd.args([
        "checker_svg_probe_the_child",
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD, "1")
    .env("KERF_FFMPEG", spy())
    .env("CHECKER_SVG_REAL", real)
    .env("CHECKER_SVG_LOG", &log);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = child::output_within(cmd, Duration::from_secs(400)).unwrap_or_else(|e| panic!("{tag}: the child {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{tag}: the child failed:\n{stdout}\n{stderr}");
    let said = stdout
        .lines()
        .chain(stderr.lines())
        .find_map(|l| l.split_once("CHECKER_SVG_VERDICT ").map(|(_, rest)| rest));
    let said = said.unwrap_or_else(|| panic!("{tag}: the child said nothing:\n{stdout}\n{stderr}"));
    let (millis, verdict) = said.split_once(' ').expect("millis and verdict");
    let log = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect();
    let found = Found {
        verdict: verdict.to_string(),
        elapsed: Duration::from_millis(millis.parse().expect("millis")),
        log,
    };
    eprintln!(
        "{tag}: {} after {:.1} s; runs: base {}, convert {}, overlay {}",
        found.verdict,
        found.elapsed.as_secs_f64(),
        found.runs("base"),
        found.runs("convert"),
        found.runs("overlay")
    );
    found
}

/// The child: asks for the probe and says what it found.
#[test]
#[ignore = "run by the tests above in a process of its own"]
fn checker_svg_probe_the_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let t0 = Instant::now();
    let verdict = kerf_gpu::svg::svg();
    println!("CHECKER_SVG_VERDICT {} {verdict:?}", t0.elapsed().as_millis());
}

fn leftover_stalls() -> usize {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return 0;
    };
    dir.flatten()
        .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
        .filter(|c| String::from_utf8_lossy(c).contains("3601.37"))
        .count()
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_svg_probe_matches_this_ffmpeg_in_a_bounded_number_of_children() {
    let found = probe_in_a_child("pass", &[]);
    assert_eq!(found.verdict, "Match");
    assert!(
        found.runs("overlay") >= 10,
        "the probe blends at several offsets: {}",
        found.runs("overlay")
    );
    assert!(found.log.len() <= 30, "about twenty short children: {}", found.log.len());
    assert!(found.elapsed < Duration::from_secs(40), "{:?}", found.elapsed);
}

/// The model is the same on every instruction set FFmpeg can choose: the probe is `Match` with the C loops, without SSE4 (the overlay's
/// `blend_row` kernels) and without AVX2 / AVX-512 (swscale's), for the conversion and the blend alike.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_svg_probe_matches_on_every_instruction_set_ffmpeg_can_pick() {
    for (tag, flags) in [
        ("cpu-c", "-cpuflags 0"),
        ("cpu-no-sse4", "-cpuflags -sse4.1-sse4.2"),
        ("cpu-no-avx2", "-cpuflags -avx2-avx512"),
        ("cpu-no-ssse3", "-cpuflags -ssse3"),
        ("cpu-mmx-sse2", "-cpuflags mmx+sse+sse2"),
    ] {
        let found = probe_in_a_child(tag, &[("CHECKER_SVG_EXTRA_ALL", flags)]);
        assert_eq!(found.verdict, "Match", "{tag} ({flags})");
    }
}

/// The probe has teeth: a blend that is anything but `overlay`'s own is a `Differs`, run through the very plumbing the probe uses.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_svg_probe_notices_a_different_overlay() {
    for (tag, sed) in [
        // one level off in luma everywhere
        ("one-level", r"s/\[o\]$/[oo];[oo]lutyuv=y=val+1[o]/"),
        // premultiplied-alpha arithmetic
        ("premultiplied", r"s/eof_action=pass/eof_action=pass:alpha=premultiplied/"),
        // a 4:4:4 blend taken back to 4:2:0
        ("yuv444", r"s/eof_action=pass/eof_action=pass:format=yuv444/"),
        // laid two pixels off
        ("two-pixels", r"s/overlay=x=\(-\{0,1\}[0-9]*\)/overlay=x=\1+2/"),
        // one level off in a single chroma sample of the first row
        (
            "one-chroma",
            r"s/\[o\]$/[oo];[oo]geq=lum='lum(X,Y)':cb='cb(X,Y)+eq(X,40)*eq(Y,3)':cr='cr(X,Y)'[o]/",
        ),
    ] {
        let found = probe_in_a_child(&format!("teeth-{tag}"), &[("CHECKER_SVG_SED", sed)]);
        assert!(found.verdict.starts_with("Differs"), "{tag}: {}", found.verdict);
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_svg_probe_with_other_conversion_flags_for_the_blend_alone_is_not_a_match() {
    let found = probe_in_a_child(
        "teeth-sws",
        &[("CHECKER_SVG_EXTRA_OVERLAY", "-sws_flags lanczos+accurate_rnd+full_chroma_int")],
    );
    // (If this FFmpeg does not apply `-sws_flags` to a `-filter_complex` graph's own scaler, nothing differs and the answer is right.)
    eprintln!("teeth-sws: {}", found.verdict);
    assert!(
        found.verdict == "Match" || found.verdict.starts_with("Differs"),
        "{}",
        found.verdict
    );
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_svg_probe_cannot_tell_when_a_child_stalls_fails_or_is_cut_short_and_does_not_wait_for_ever() {
    // A stalled blend: killed at its 20 s, the probe is `Unknown` (never a match) within its 45 s.
    let found = probe_in_a_child("stall-overlay", &[("CHECKER_SVG_STALL", "overlay")]);
    assert_eq!(found.verdict, "Unknown");
    assert!(found.elapsed < Duration::from_secs(60), "{:?}", found.elapsed);
    assert!(
        found.elapsed >= Duration::from_secs(15),
        "the child is given its time: {:?}",
        found.elapsed
    );
    // A stalled conversion, and a stalled first child.
    let found = probe_in_a_child("stall-convert", &[("CHECKER_SVG_STALL", "convert")]);
    assert_eq!(found.verdict, "Unknown");
    assert!(found.elapsed < Duration::from_secs(60), "{:?}", found.elapsed);
    let found = probe_in_a_child("stall-all", &[("CHECKER_SVG_STALL", "all")]);
    assert_eq!(found.verdict, "Unknown");
    assert!(found.elapsed < Duration::from_secs(60), "{:?}", found.elapsed);
    // A child that fails; one whose output stops short.
    for (tag, env) in [
        ("fail-overlay", [("CHECKER_SVG_FAIL", "overlay")]),
        ("fail-convert", [("CHECKER_SVG_FAIL", "convert")]),
        ("fail-base", [("CHECKER_SVG_FAIL", "base")]),
        ("trunc-overlay", [("CHECKER_SVG_TRUNC", "overlay")]),
        ("trunc-convert", [("CHECKER_SVG_TRUNC", "convert")]),
        ("trunc-base", [("CHECKER_SVG_TRUNC", "base")]),
    ] {
        let found = probe_in_a_child(tag, &env);
        assert_eq!(found.verdict, "Unknown", "{tag}");
        assert!(found.elapsed < Duration::from_secs(30), "{tag}: {:?}", found.elapsed);
    }
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(leftover_stalls(), 0, "a killed stall left its child running");
}

/// Every child is slow, none is slow enough to be killed: the probe as a whole is bounded all the same.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_svg_probe_is_bounded_as_a_whole_when_every_child_is_slow() {
    let found = probe_in_a_child("slow-all", &[("CHECKER_SVG_DELAY", "7")]);
    assert_eq!(found.verdict, "Unknown");
    assert!(found.elapsed < Duration::from_secs(60), "{:?}", found.elapsed);
    assert!(
        found.log.len() < 12,
        "no more children than the deadline allows: {}",
        found.log.len()
    );
}

// ---- the cache -------------------------------------------------------------------------------------------------------------

struct Blob(usize);

impl Weighed for Blob {
    fn bytes(&self) -> usize {
        self.0
    }
}

fn failing(why: &str) -> Result<Blob, GpuError> {
    Err(GpuError::Unsupported(why.to_string()))
}

/// A picture that could not be made is not made again by the frames that were waiting for it: one child failed, the failure is remembered,
/// and the waiters take it (the cache says "one flight per picture" and "a failure remembered for 20 s").
#[test]
#[ignore = "a cache test"]
fn checker_svg_cache_waiters_on_a_failing_picture_take_its_failure() {
    let cache = PictureCache::<Blob>::with_limits(8, 1 << 20);
    let tried = AtomicUsize::new(0);
    let t0 = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..5 {
            scope.spawn(|| {
                let r = cache.get("k", || {
                    tried.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(300));
                    failing("the child failed")
                });
                assert!(r.is_err());
            });
        }
    });
    eprintln!(
        "5 threads asked for one failing picture: made {} time(s) in {:?}",
        tried.load(Ordering::Relaxed),
        t0.elapsed()
    );
    assert_eq!(tried.load(Ordering::Relaxed), 1, "every waiter spawned the child again");
}

/// The memory of a failure ends after 20 s and the next ask makes the picture.
#[test]
#[ignore = "a cache test that waits 21 s"]
fn checker_svg_cache_a_failure_is_forgotten_after_twenty_seconds() {
    let cache = PictureCache::<Blob>::with_limits(8, 1 << 20);
    let tried = AtomicUsize::new(0);
    let attempt = |ok: bool| {
        cache.get("k", || {
            tried.fetch_add(1, Ordering::Relaxed);
            if ok {
                Ok(Blob(10))
            } else {
                failing("the child failed")
            }
        })
    };
    assert!(attempt(false).is_err());
    std::thread::sleep(Duration::from_secs(10));
    assert!(attempt(true).is_err(), "still remembered at 10 s");
    assert_eq!(tried.load(Ordering::Relaxed), 1);
    std::thread::sleep(Duration::from_secs(10) + Duration::from_millis(300));
    assert!(attempt(true).is_ok(), "forgotten after 20 s");
    assert_eq!(tried.load(Ordering::Relaxed), 2);
    assert!(attempt(true).is_ok());
    assert_eq!(tried.load(Ordering::Relaxed), 2, "and then it is held");
}

/// The failures kept are bounded and one key's failure is not another's.
#[test]
#[ignore = "a cache test"]
fn checker_svg_cache_a_failure_is_the_keys_own_and_the_memory_of_them_is_bounded() {
    let cache = PictureCache::<Blob>::with_limits(8, 1 << 20);
    for n in 0..200 {
        assert!(cache.get(&format!("bad{n}"), || failing("no")).is_err());
    }
    assert!(cache.get("good", || Ok(Blob(1))).is_ok());
    assert_eq!(cache.held(), (1, 1));
    // The oldest failures were let go (64 kept): the first key is asked again, the last is not.
    let again = AtomicUsize::new(0);
    let _ = cache.get("bad0", || {
        again.fetch_add(1, Ordering::Relaxed);
        failing("no")
    });
    let last = AtomicUsize::new(0);
    let _ = cache.get("bad199", || {
        last.fetch_add(1, Ordering::Relaxed);
        failing("no")
    });
    assert_eq!((again.load(Ordering::Relaxed), last.load(Ordering::Relaxed)), (1, 0));
}

/// Least recently used out, by count and by bytes, and a hit counts as a use.
#[test]
#[ignore = "a cache test"]
fn checker_svg_cache_eviction_is_least_recently_used_by_count_and_by_bytes() {
    let cache = PictureCache::<Blob>::with_limits(4, 1000);
    let made = AtomicUsize::new(0);
    let get = |k: &str, n: usize| {
        cache
            .get(k, || {
                made.fetch_add(1, Ordering::Relaxed);
                Ok(Blob(n))
            })
            .unwrap()
    };
    for k in ["a", "b", "c", "d"] {
        get(k, 10);
    }
    assert_eq!(made.load(Ordering::Relaxed), 4);
    get("a", 10); // a is now the newest
    get("e", 10); // evicts b (the oldest)
    assert_eq!(cache.held(), (4, 40));
    let before = made.load(Ordering::Relaxed);
    for k in ["a", "c", "d", "e"] {
        get(k, 10);
    }
    assert_eq!(made.load(Ordering::Relaxed), before, "a, c, d, e are held");
    get("b", 10);
    assert_eq!(made.load(Ordering::Relaxed), before + 1, "b was the one let go");
    // By bytes: 600 + 600 > 1000, the older goes; a lone picture over the limit is kept (it is the one asked for).
    let cache = PictureCache::<Blob>::with_limits(100, 1000);
    cache.get("x", || Ok(Blob(600))).unwrap();
    cache.get("y", || Ok(Blob(600))).unwrap();
    assert_eq!(cache.held(), (1, 600));
    cache.get("huge", || Ok(Blob(5000))).unwrap();
    assert_eq!(cache.held(), (1, 5000), "the one asked for is kept; the rest go");
    cache.get("small", || Ok(Blob(10))).unwrap();
    assert_eq!(cache.held(), (1, 10), "and the huge one is let go by the next");
}

/// A picture that panics while it is made leaves nothing behind and the next ask makes it.
#[test]
#[ignore = "a cache test"]
fn checker_svg_cache_a_maker_that_panics_leaves_no_trace() {
    let cache = Arc::new(PictureCache::<Blob>::with_limits(4, 1 << 20));
    let c = Arc::clone(&cache);
    let r = std::thread::spawn(move || {
        let _ = c.get("p", || panic!("a resvg panic"));
    })
    .join();
    assert!(r.is_err());
    let got = cache.get("p", || Ok(Blob(3))).unwrap();
    assert_eq!(got.bytes(), 3);
    assert_eq!(cache.held(), (1, 3));
}

/// Many threads on a few keys with small limits: nothing deadlocks, nothing is made while held, and the bytes charged are the bytes held.
#[test]
#[ignore = "a cache test"]
fn checker_svg_cache_under_contention_keeps_its_books() {
    let cache = Arc::new(PictureCache::<Blob>::with_limits(5, 700));
    let t0 = Instant::now();
    std::thread::scope(|scope| {
        for t in 0..8u32 {
            let cache = Arc::clone(&cache);
            scope.spawn(move || {
                let mut s = t.wrapping_mul(7919).wrapping_add(13);
                for _ in 0..3000 {
                    s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let key = format!("k{}", (s >> 16) % 12);
                    let size = 100 + ((s >> 8) % 3) as usize * 50;
                    let got = cache.get(&key, || Ok(Blob(size))).unwrap();
                    assert!(got.bytes() >= 100);
                }
            });
        }
    });
    assert!(t0.elapsed() < Duration::from_secs(30), "{:?}", t0.elapsed());
    let (n, bytes) = cache.held();
    assert!(n <= 5, "{n} held of 5");
    assert!(bytes <= 700, "{bytes} bytes of 700");
    assert!(bytes >= n * 100 && bytes <= n * 200, "{n} pictures charged {bytes}");
}

/// A `clear` while pictures are being made must not leave the books out: afterwards what is charged is what is held.
#[test]
#[ignore = "a cache test"]
fn checker_svg_cache_a_clear_during_fills_leaves_the_books_right() {
    let cache = Arc::new(PictureCache::<Blob>::with_limits(50, 1 << 20));
    std::thread::scope(|scope| {
        for t in 0..4u32 {
            let cache = Arc::clone(&cache);
            scope.spawn(move || {
                for i in 0..400u32 {
                    let key = format!("t{t}-{}", i % 7);
                    let _ = cache.get(&key, || {
                        std::thread::sleep(Duration::from_millis(1));
                        Ok(Blob(100))
                    });
                }
            });
        }
        let clearer = Arc::clone(&cache);
        scope.spawn(move || {
            for _ in 0..200 {
                std::thread::sleep(Duration::from_millis(2));
                clearer.clear();
            }
        });
    });
    let (n, bytes) = cache.held();
    assert_eq!(bytes, n * 100, "{n} pictures held but {bytes} bytes charged");
}
