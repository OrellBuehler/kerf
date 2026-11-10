//! `checker/scalers/probe/...`: what the scaler probe does when one of its children fails, hangs or returns another picture than the
//! model's, and which names it then leaves out. The base scaler (the unflagged scale and the bicubic and bilinear chains) and each named
//! scaler (its chains, and Lanczos's `sin` check) are separate questions with a backoff of their own: a child of a name that fails or hangs
//! leaves that name out and asks it again alone, a child of the base leaves the whole answer `Unknown`, a name whose picture differs is out
//! for good, and a name never looks at a model that is not the one the answer stands on.
//!
//! The probe is the process's (`kerf_gpu::scaler::kernels()`), so each scenario runs in a child of this test binary with `KERF_FFMPEG`
//! swapped for a shell wrapper that misbehaves for the chains it names and passes every other through to the real FFmpeg. The child
//! follows a list of steps (`KERF_SC_STEPS`: `call`, `sleep=<ms>`, `rm=<marker>`, `touch=<marker>`, `par=<n>`, `stale`, `poll=<ms>`) and says
//! what came back and how many FFmpegs each question has run so far. Needs `ffmpeg` and a shell; no adapter. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_scalers_probe -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kerf_core::ScalerKinds;
use kerf_gpu::scaler::{self, Kernels};

const STEPS: &str = "KERF_SC_STEPS";
const DIR: &str = "KERF_SC_DIR";
const LOG: &str = "KERF_SC_LOG";

const ALL: &str = "bilinear=1 lanczos=1 neighbor=1 spline=1";

/// `ALL` without `kind`.
fn without(kind: &str) -> String {
    ALL.replace(&format!("{kind}=1"), &format!("{kind}=0"))
}

fn line_of(k: Kernels) -> String {
    match k {
        Kernels::Match(m) => {
            let b = |kind| u8::from(m.named.contains(kind));
            format!(
                "Match kernel={:?} bilinear={} lanczos={} neighbor={} spline={}",
                m.kernel,
                b(ScalerKinds::BILINEAR),
                b(ScalerKinds::LANCZOS),
                b(ScalerKinds::NEIGHBOR),
                b(ScalerKinds::SPLINE)
            )
        }
        Kernels::Differs => "Differs".to_string(),
        Kernels::Unknown => "Unknown".to_string(),
    }
}

/// The scalers a step names: `none`, `all`, or names joined by `+`.
fn names_of(arg: &str) -> ScalerKinds {
    match arg {
        "none" => ScalerKinds::NONE,
        "all" => ScalerKinds::ALL,
        other => other.split('+').fold(ScalerKinds::NONE, |kinds, name| {
            kinds.union(ScalerKinds::of_name(name).unwrap_or_else(|| panic!("no scaler {name}")))
        }),
    }
}

/// The threads this process has (Linux; 0 elsewhere).
fn thread_count() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("Threads:").and_then(|n| n.trim().parse().ok()))
        })
        .unwrap_or(0)
}

/// How many FFmpegs the wrapper has been started as, in all and by the chain they carry.
fn counts() -> String {
    let text = std::env::var_os(LOG)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let n = |needle: &str| text.lines().filter(|l| l.contains(needle)).count();
    format!(
        "runs={} n_spline={} n_neighbor={} n_lanczos={} n_sin={}",
        text.lines().count(),
        n("flags=spline"),
        n("flags=neighbor"),
        n("flags=lanczos"),
        n("sin(")
    )
}

fn say(tag: &str, k: Kernels, took: Duration) {
    eprintln!(
        "{tag} {} ms={} {} exact={}",
        line_of(k),
        took.as_millis(),
        counts(),
        k.exact()
    );
}

/// The child: follows `KERF_SC_STEPS`.
#[test]
#[ignore = "run by the scenario tests below, in a child process"]
fn checker_scalers_probe_child() {
    let Ok(steps) = std::env::var(STEPS) else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os(DIR).expect("a scenario directory"));
    let started = Instant::now();
    let mut calls = 0;
    for step in steps.split(';').filter(|s| !s.is_empty()) {
        let (what, arg) = step.split_once('=').unwrap_or((step, ""));
        match what {
            "call" => {
                calls += 1;
                let t = Instant::now();
                let k = scaler::kernels();
                say(&format!("RESULT{calls}"), k, t.elapsed());
            }
            "callfor" => {
                calls += 1;
                let t = Instant::now();
                let k = scaler::kernels_for(names_of(arg));
                say(&format!("RESULT{calls}"), k, t.elapsed());
            }
            // Ask (every 100 ms, as callers do) until every name is in the answer or `arg` ms have passed.
            "settle" => {
                calls += 1;
                let t = Instant::now();
                let until = t + Duration::from_millis(arg.parse().unwrap());
                let mut k = scaler::kernels();
                while Instant::now() < until && !line_of(k).ends_with(ALL) {
                    std::thread::sleep(Duration::from_millis(100));
                    k = scaler::kernels();
                }
                say(&format!("RESULT{calls}"), k, t.elapsed());
            }
            "threads" => eprintln!("THREADS {}", thread_count()),
            "exec" => std::fs::set_permissions(dir.join("ffmpeg-wrapper"), std::fs::Permissions::from_mode(0o755)).unwrap(),
            // `first:delay:second`: a caller asks for the names `first`, and `delay` ms later another for the names `second`.
            "pair" => {
                calls += 1;
                let mut parts = arg.split(':');
                let (first, delay, second) = (
                    names_of(parts.next().unwrap()),
                    parts.next().unwrap().parse::<u64>().unwrap(),
                    names_of(parts.next().unwrap()),
                );
                let (a, b) = std::thread::scope(|scope| {
                    let a = scope.spawn(|| {
                        let t = Instant::now();
                        (scaler::kernels_for(first), t.elapsed())
                    });
                    std::thread::sleep(Duration::from_millis(delay));
                    let b = scope.spawn(|| {
                        let t = Instant::now();
                        (scaler::kernels_for(second), t.elapsed())
                    });
                    (a.join().unwrap(), b.join().unwrap())
                });
                eprintln!("RESULT{calls}A {} ms={}", line_of(a.0), a.1.as_millis());
                eprintln!("RESULT{calls}B {} ms={}", line_of(b.0), b.1.as_millis());
                say(
                    &format!("RESULT{calls}C"),
                    scaler::kernels_for(ScalerKinds::NONE),
                    Duration::ZERO,
                );
            }
            // `threads:ms:remove_at:marker`: many callers ask for different names for `ms`; the marker goes at `remove_at`. What they are
            // told must never go back: a name once in an answer stays in the thread's next ones, no call that starts after an answer was
            // given is told Unknown, the kernel never changes, and `bilinear` (the base's own) is always in.
            "stress" => {
                calls += 1;
                let mut parts = arg.split(':');
                let threads: usize = parts.next().unwrap().parse().unwrap();
                let ms: u64 = parts.next().unwrap().parse().unwrap();
                let remove_at: u64 = parts.next().unwrap().parse().unwrap();
                let marker = parts.next().unwrap().to_string();
                let sets = [
                    ScalerKinds::NONE,
                    ScalerKinds::LANCZOS,
                    ScalerKinds::SPLINE,
                    ScalerKinds::ALL,
                    ScalerKinds::NEIGHBOR.union(ScalerKinds::SPLINE),
                    ScalerKinds::BILINEAR,
                ];
                let answered = AtomicBool::new(false);
                let violations = AtomicUsize::new(0);
                let kernel: Mutex<Option<String>> = Mutex::new(None);
                let asked = AtomicUsize::new(0);
                std::thread::scope(|scope| {
                    scope.spawn(|| {
                        std::thread::sleep(Duration::from_millis(remove_at));
                        let _ = std::fs::remove_file(dir.join(&marker));
                    });
                    for i in 0..threads {
                        let (answered, violations, kernel, asked) = (&answered, &violations, &kernel, &asked);
                        scope.spawn(move || {
                            let begin = Instant::now();
                            let mut held = ScalerKinds::NONE;
                            let mut j = i;
                            while begin.elapsed() < Duration::from_millis(ms) {
                                let before = answered.load(Ordering::SeqCst);
                                let told = scaler::kernels_for(sets[j % sets.len()]);
                                j += 1;
                                asked.fetch_add(1, Ordering::SeqCst);
                                match told {
                                    Kernels::Match(m) => {
                                        let mut bad = !m.named.contains(ScalerKinds::BILINEAR) || !m.named.contains(held);
                                        let mut seen = kernel.lock().unwrap();
                                        bad |=
                                            seen.get_or_insert_with(|| format!("{:?}", m.kernel)) != &format!("{:?}", m.kernel);
                                        held = held.union(m.named);
                                        answered.store(true, Ordering::SeqCst);
                                        if bad {
                                            violations.fetch_add(1, Ordering::SeqCst);
                                        }
                                    }
                                    _ if before => {
                                        violations.fetch_add(1, Ordering::SeqCst);
                                    }
                                    _ => {}
                                }
                                std::thread::sleep(Duration::from_millis(20));
                            }
                        });
                    }
                });
                eprintln!(
                    "STRESS threads={threads} asked={} violations={}",
                    asked.load(Ordering::SeqCst),
                    violations.load(Ordering::SeqCst)
                );
                say(&format!("RESULT{calls}"), scaler::kernels(), Duration::ZERO);
            }
            "sleep" => std::thread::sleep(Duration::from_millis(arg.parse().unwrap())),
            "rm" => {
                let _ = std::fs::remove_file(dir.join(arg));
            }
            "touch" => std::fs::write(dir.join(arg), "x").unwrap(),
            // `n` callers at once, each asking for the process's probe: what each of them was told.
            "par" => {
                calls += 1;
                let n: usize = arg.parse().unwrap();
                let told: Vec<(Kernels, Duration)> = std::thread::scope(|scope| {
                    let runs: Vec<_> = (0..n)
                        .map(|_| {
                            scope.spawn(|| {
                                let t = Instant::now();
                                (scaler::kernels(), t.elapsed())
                            })
                        })
                        .collect();
                    runs.into_iter().map(|r| r.join().unwrap()).collect()
                });
                for (i, (k, took)) in told.iter().enumerate() {
                    eprintln!("PAR{calls}.{i} {} ms={}", line_of(*k), took.as_millis());
                }
                say(&format!("RESULT{calls}"), scaler::kernels(), Duration::ZERO);
            }
            // A caller starts the probe; a second one arrives a second later (the wrapper treats the children it starts after `late` exists
            // as another FFmpeg's: the C kernel), and the second one finishes first.
            "stale" => {
                calls += 1;
                let (a, b) = std::thread::scope(|scope| {
                    let first = scope.spawn(|| {
                        let t = Instant::now();
                        (scaler::kernels(), t.elapsed())
                    });
                    std::thread::sleep(Duration::from_millis(1000));
                    std::fs::write(dir.join("late"), "x").unwrap();
                    let second = scope.spawn(|| {
                        let t = Instant::now();
                        (scaler::kernels(), t.elapsed())
                    });
                    (first.join().unwrap(), second.join().unwrap())
                });
                eprintln!("RESULT{calls}A {} ms={}", line_of(a.0), a.1.as_millis());
                eprintln!("RESULT{calls}B {} ms={}", line_of(b.0), b.1.as_millis());
                say(&format!("RESULT{calls}C"), scaler::kernels(), Duration::ZERO);
            }
            // Ask every 200 ms for `arg` ms and say when a run was started.
            "poll" => {
                let until = Instant::now() + Duration::from_millis(arg.parse().unwrap());
                let mut last = counts();
                while Instant::now() < until {
                    let _ = scaler::kernels();
                    let now = counts();
                    if now != last {
                        eprintln!("TICK at={} {now}", started.elapsed().as_millis());
                        last = now;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
            // The routes of moving stills under the named scalers, as the still probe measured them, and what a plan of a moving still of each
            // kind asks of them.
            "route" => route_report(&dir),
            other => panic!("no step {other}"),
        }
    }
}

const ROUTE_NAMES: [(ScalerKinds, &str); 4] = [
    (ScalerKinds::BILINEAR, "bilinear"),
    (ScalerKinds::LANCZOS, "lanczos"),
    (ScalerKinds::NEIGHBOR, "neighbor"),
    (ScalerKinds::SPLINE, "spline"),
];

/// A still of `pix_fmt` (`file`), probed.
fn still_of(dir: &Path, file: &str, pix_fmt: &str) -> kerf_core::Asset {
    let path = dir.join(file);
    let out = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=1:duration=1",
        ])
        .args(["-frames:v", "1", "-pix_fmt", pix_fmt])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    kerf_core::Project::probe_asset(&path).expect("probe the still")
}

/// A Ken Burns move on `asset` (in from 1.2x towards the top left to the frame: the slideshow's own, which the plan takes the route for),
/// delivered into a 16:9 frame that names `scaler`.
fn ken_burns_plan(asset: &kerf_core::Asset, scaler: Option<&str>) -> kerf_core::RenderPlan {
    let key = |time: f64, scale: f64, pos_x: f64, pos_y: f64| kerf_core::Keyframe {
        time,
        scale,
        pos_x,
        pos_y,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    };
    let mut clip = kerf_core::Clip::new(asset.id, 0.0, 0.9, 0.0);
    clip.keyframes = vec![key(0.0, 1.2, 0.04, 0.03), key(0.9, 1.0, 0.0, 0.0)];
    let tl = kerf_core::Timeline {
        tracks: vec![kerf_core::Track {
            clips: vec![clip],
            ..kerf_core::Track::new(kerf_core::StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Some(kerf_core::Delivery::new(640, 360, kerf_core::Fit::Cover)),
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let opts = kerf_core::ExportOptions {
        scaler: scaler.map(str::to_string),
        ..kerf_core::ExportOptions::default()
    };
    kerf_core::RenderPlan::at(
        &tl,
        std::slice::from_ref(asset),
        &opts,
        0.4,
        kerf_core::composite_color_policy(),
    )
    .expect("a plan")
}

fn route_report(dir: &Path) {
    let measured = kerf_gpu::still_probe::all();
    let kinds = [
        ("yuvj420p", kerf_core::StillKinds::YUVJ420P),
        ("rgb24", kerf_core::StillKinds::RGB24),
        ("yuv420p", kerf_core::StillKinds::YUV420P),
    ];
    for (label, kind) in kinds {
        let bits: Vec<String> = ROUTE_NAMES
            .iter()
            .map(|(n, text)| format!("{text}={}", u8::from(measured.contains_named_route(kind, *n))))
            .collect();
        eprintln!("ROUTE {label} {}", bits.join(" "));
    }
    // What a plan of a moving still of each kind asks of those bits: the caps of a compositor that draws everything but whose moving
    // stills are exactly what was measured.
    let mut caps = kerf_gpu::Compositor::CAPS;
    caps.still_motion = measured;
    caps.scalers = ScalerKinds::ALL;
    let stills = [
        ("rgb24", still_of(dir, "route.png", "rgb24")),
        ("yuv420p", still_of(dir, "route.tif", "yuv420p")),
    ];
    for (label, asset) in &stills {
        let pix = asset.streams.iter().find_map(|s| s.pix_fmt.clone()).unwrap_or_default();
        for scaler in [
            None,
            Some("bicubic"),
            Some("bilinear"),
            Some("lanczos"),
            Some("neighbor"),
            Some("spline"),
        ] {
            let plan = ken_burns_plan(asset, scaler);
            let route = plan.layers.iter().any(|l| plan.takes_still_route(l));
            let why = plan.reasons(&caps, plan.size(u32::MAX));
            let refused = why.iter().any(|u| matches!(u, kerf_core::Unsupported::StillMotion(..)));
            eprintln!(
                "PLAN {label} {pix} {} route={} refused={}",
                scaler.unwrap_or("default"),
                u8::from(route),
                u8::from(refused)
            );
        }
    }
}

struct Run {
    lines: Vec<String>,
    dir: PathBuf,
}

impl Run {
    fn line(&self, tag: &str) -> &str {
        self.lines
            .iter()
            .find(|l| l.split(' ').next() == Some(tag))
            .unwrap_or_else(|| panic!("{tag} not printed: {:#?}", self.lines))
    }

    /// What a line says came back: from after the tag to the figures (for a match, the names without the kernel).
    fn answer(&self, tag: &str) -> String {
        let l = self.line(tag).strip_prefix(tag).unwrap_or_default().trim_start();
        let l = l.split(" ms=").next().unwrap_or(l);
        match l.strip_prefix("Match ") {
            Some(m) => m.split_once(' ').map_or(m.to_string(), |(_, named)| named.to_string()),
            None => l.to_string(),
        }
    }

    fn kernel(&self, tag: &str) -> String {
        let l = self.line(tag);
        let at = l.find("kernel=").map_or(0, |i| i + 7);
        l[at..].split(' ').next().unwrap_or_default().to_string()
    }

    fn number(&self, tag: &str, key: &str) -> u128 {
        let l = self.line(tag);
        let at = l.find(key).unwrap_or_else(|| panic!("{key} in {l}")) + key.len();
        l[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap()
    }

    fn runs(&self, tag: &str) -> u128 {
        self.number(tag, " runs=")
    }

    /// The lines that start with `prefix`.
    fn all(&self, prefix: &str) -> Vec<&String> {
        self.lines.iter().filter(|l| l.starts_with(prefix)).collect()
    }
}

/// Run the child (`steps`) under a wrapper of `body` (shell, after the log line and before the real FFmpeg; `$KERF_SC_DIR/<marker>` are the
/// files the steps `touch` and `rm`), with the markers `markers` there at the start.
fn scenario(name: &str, body: &str, steps: &str, markers: &[&str]) -> Run {
    let real = kerf_core::ffmpeg_path();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-sc-probe-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for m in markers {
        std::fs::write(dir.join(m), "x").unwrap();
    }
    let wrapper = dir.join("ffmpeg-wrapper");
    let log = dir.join("runs.log");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho \"$$ $*\" >> \"{log}\"\nREAL='{real}'\n{body}\nexec \"$REAL\" \"$@\"\n",
            log = log.display()
        ),
    )
    .unwrap();
    let mode = if markers.contains(&"noexec") { 0o644 } else { 0o755 };
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(mode)).unwrap();
    let started = Instant::now();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "checker_scalers_probe_child",
        ])
        .env(STEPS, steps)
        .env(DIR, &dir)
        .env("KERF_FFMPEG", &wrapper)
        .env(LOG, &log)
        .stdin(Stdio::null())
        .output()
        .expect("run the child");
    let text = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    let lines: Vec<String> = text
        .lines()
        .filter(|l| {
            ["RESULT", "PAR", "TICK", "ROUTE", "PLAN", "THREADS", "STRESS"]
                .iter()
                .any(|p| l.starts_with(p))
        })
        .map(str::to_string)
        .collect();
    eprintln!("{name}: {lines:#?} (child took {:?})", started.elapsed());
    assert!(out.status.success(), "{name}: the child failed: {text}");
    Run { lines, dir }
}

/// Every pid a stalled wrapper wrote down (`$KERF_SC_DIR/pids`) is gone.
fn assert_no_stalled_child_left(r: &Run, what: &str) {
    let pids = std::fs::read_to_string(r.dir.join("pids")).unwrap_or_default();
    assert!(!pids.trim().is_empty(), "{what}: the stall was never reached");
    for pid in pids.split_whitespace() {
        let alive = Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(!alive, "{what}: the stalled child {pid} outlived the probe");
    }
}

/// A rewrite of every argument that matches `from` into `to` (the chain then returns another picture than the one the probe asks about).
fn rewrite(from: &str, to: &str) -> String {
    format!(
        "n=$#\ni=0\nwhile [ $i -lt $n ]; do\n  a=$1; shift\n  case \"$a\" in *'{from}'*) a=$(printf '%s' \"$a\" | sed 's|{from}|{to}|g');; esac\n  set -- \"$@\" \"$a\"\n  i=$((i+1))\ndone"
    )
}

/// The wrapper body that makes a child exit 1 while the marker `fail_<name>` exists: for the children whose arguments carry `needle`.
fn fails(name: &str, needle: &str) -> String {
    format!("case \"$*\" in *'{needle}'*) if [ -e \"$KERF_SC_DIR/fail_{name}\" ]; then exit 1; fi;; esac")
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_runs_twenty_children_and_proves_every_name() {
    let r = scenario("plain", "", "call;call", &[]);
    assert_eq!(r.answer("RESULT1"), ALL, "{:?}", r.lines);
    assert_eq!(r.runs("RESULT1"), 20, "the probe's children");
    assert_eq!(r.runs("RESULT2"), 20, "a measured answer is kept");
    assert!(r.number("RESULT1", " ms=") < 20_000, "{:?}", r.lines);
}

/// A hung child is killed at its deadline; of a name it leaves that name out, of a base chain the whole answer `Unknown`.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_child_that_hangs_is_killed_at_its_deadline_and_leaves_only_its_own_question_open() {
    for (name, needle, expected) in [
        ("hang_sin", "sin(", without("lanczos")),
        ("hang_lanczos", "flags=lanczos", without("lanczos")),
        ("hang_spline", "flags=spline", without("spline")),
        ("hang_neighbor", "flags=neighbor", without("neighbor")),
        // A base child: the scaler is unknown, as it was.
        ("hang_base", "flags=bilinear", "Unknown".to_string()),
    ] {
        let r = scenario(
            name,
            &format!("case \"$*\" in *'{needle}'*) echo $$ > \"$KERF_SC_DIR/pid\"; exec sleep 120;; esac"),
            "call;call",
            &[],
        );
        assert_eq!(r.answer("RESULT1"), expected, "{name}: {:?}", r.lines);
        assert_eq!(r.line("RESULT1").ends_with("exact=true"), expected != "Unknown", "{name}");
        let ms = r.number("RESULT1", " ms=");
        assert!(
            (19_000..60_000).contains(&ms),
            "{name}: killed at the 20 s deadline, not at once and not after the stall: {ms} ms"
        );
        let pid = std::fs::read_to_string(r.dir.join("pid")).unwrap_or_default();
        let pid = pid.trim();
        assert!(!pid.is_empty(), "{name}: the stall was never reached");
        let alive = Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(!alive, "{name}: the stalled child {pid} outlived the probe");
        assert_eq!(r.answer("RESULT2"), expected, "{name}");
        assert_eq!(r.runs("RESULT1"), r.runs("RESULT2"), "{name}: not run again at once");
    }
}

/// A name whose picture differs is out for good: repaired, it is not asked again.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_name_whose_picture_differs_is_out_for_good_and_the_only_one() {
    for name in ["lanczos", "spline", "neighbor"] {
        // (`flags=<name>` of the chains becomes bicubic while the marker is there: another picture than the name's model.)
        let body = format!(
            "if [ -e \"$KERF_SC_DIR/diff_{name}\" ]; then\n{}\nfi",
            rewrite(&format!("flags={name}"), "flags=bicubic")
        );
        let r = scenario(
            &format!("diff_{name}"),
            &body,
            &format!("call;call;rm=diff_{name};sleep=5600;call"),
            &[&format!("diff_{name}")],
        );
        let expect = without(name);
        assert_eq!(r.answer("RESULT1"), expect, "{name}: {:?}", r.lines);
        assert_eq!(r.answer("RESULT2"), expect, "{name}");
        assert_eq!(r.answer("RESULT3"), expect, "{name}: a name that differed is not asked again");
        assert_eq!(r.runs("RESULT3"), 20, "{name}: no run after the backoff");
    }
}

/// A `sin` that is not ours leaves Lanczos out alone, at every argument and at a single one of the grid's 4096.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_sin_that_is_not_ours_leaves_lanczos_out_alone() {
    for (name, from, to) in [
        ("sin_differs", "*0.0023)", "*0.00230000000000001)"),
        (
            "sin_differs_at_one_argument",
            "*0.0023)",
            "*0.0023+eq(floor(X/8)+64*Y,1000)*0.00000000000001)",
        ),
    ] {
        let r = scenario(name, &rewrite(from, to), "call;call", &[]);
        assert_eq!(r.answer("RESULT1"), without("lanczos"), "{name}: {:?}", r.lines);
        assert_eq!(r.answer("RESULT2"), without("lanczos"), "{name}");
    }
}

/// The backoff of a name doubles: a name that keeps failing is asked again at about 5 s and then 10 s after that.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_name_that_keeps_failing_backs_off_5_then_10_seconds() {
    let r = scenario(
        "doubling",
        &fails("neighbor", "flags=neighbor"),
        "call;poll=19000",
        &["fail_neighbor"],
    );
    assert_eq!(r.answer("RESULT1"), without("neighbor"));
    let ticks: Vec<u128> = r
        .all("TICK")
        .iter()
        .map(|l| l["TICK at=".len()..].split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert_eq!(ticks.len(), 2, "two retries in 19 s: {:?}", r.lines);
    let (first, second) = (ticks[0], ticks[1]);
    assert!((5_000..9_000).contains(&first), "the first retry after 5 s: {first} ms");
    assert!(
        (9_500..13_000).contains(&(second - first)),
        "the second 10 s later: {} ms",
        second - first
    );
    // Only the name's own children are ever asked again.
    let last = r.all("TICK").last().copied().unwrap().clone();
    assert!(last.contains("runs=26"), "{last}");
}

/// A base that differs is permanent, and no name is asked for it, then or later (there is no model to compare it with).
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_base_that_differs_asks_no_name() {
    let r = scenario(
        "base_differs",
        &rewrite("scale=300:200:flags=bilinear", "scale=300:200:flags=neighbor"),
        "call;call;sleep=5600;call",
        &[],
    );
    assert_eq!(r.answer("RESULT1"), "Differs", "{:?}", r.lines);
    assert!(r.line("RESULT1").ends_with("exact=false"));
    assert_eq!(r.runs("RESULT1"), 20);
    assert_eq!(r.answer("RESULT3"), "Differs");
    assert_eq!(r.runs("RESULT3"), 20, "nothing is asked again, of the base or of a name");
}

/// The routes of moving stills under a named scaler are in the caps only for the kinds and names the still probe measured, one bit each:
/// every one of the two kinds it probes and none of the kind it does not, whatever the name; and when the children of one name's toy cut fail,
/// that name's bits are missing (for both kinds) and no other's, and a plan of a moving still asks for exactly the bit of its kind and name.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_route_is_in_the_caps_only_where_it_was_measured() {
    let r = scenario("route_all", "", "route", &[]);
    for kind in ["yuvj420p", "rgb24"] {
        let l = r.lines.iter().find(|l| l.starts_with(&format!("ROUTE {kind} "))).unwrap();
        assert!(l.contains("bilinear=1 lanczos=1 neighbor=1 spline=1"), "{l}");
    }
    let l = r.lines.iter().find(|l| l.starts_with("ROUTE yuv420p ")).unwrap();
    assert!(
        l.contains("bilinear=0 lanczos=0 neighbor=0 spline=0"),
        "a kind with no probe has no route: {l}"
    );
    for name in ["bilinear", "lanczos", "neighbor", "spline"] {
        let l = r
            .lines
            .iter()
            .find(|l| l.starts_with(&format!("PLAN rgb24 rgb24 {name} ")))
            .unwrap();
        assert!(
            l.contains("route=1 refused=0"),
            "an RGB still on the route under {name} is drawn: {l}"
        );
        // A picture of a kind nothing is measured for is FFmpeg's, under every name (and under none).
        let l = r
            .lines
            .iter()
            .find(|l| l.starts_with(&format!("PLAN yuv420p yuv420p {name} ")))
            .unwrap();
        assert!(l.contains("refused=1"), "{l}");
    }
    for name in ["default", "bicubic"] {
        let l = r
            .lines
            .iter()
            .find(|l| l.starts_with(&format!("PLAN rgb24 rgb24 {name} ")))
            .unwrap();
        assert!(l.contains("refused=0"), "{l}");
        let l = r
            .lines
            .iter()
            .find(|l| l.starts_with(&format!("PLAN yuv420p yuv420p {name} ")))
            .unwrap();
        assert!(l.contains("refused=1"), "{l}");
    }
    // The toy cut of one name fails (any FFmpeg that carries `:flags=neighbor` exits 1): that name has no route, the others keep theirs.
    let r = scenario(
        "route_neighbor_fails",
        &fails("neighbor", "flags=neighbor"),
        "route",
        &["fail_neighbor"],
    );
    for kind in ["yuvj420p", "rgb24"] {
        let l = r.lines.iter().find(|l| l.starts_with(&format!("ROUTE {kind} "))).unwrap();
        assert!(l.contains("bilinear=1 lanczos=1 neighbor=0 spline=1"), "{l}");
    }
    let l = r.lines.iter().find(|l| l.starts_with("PLAN rgb24 rgb24 neighbor ")).unwrap();
    assert!(l.contains("route=1 refused=1"), "{l}");
    for name in ["bilinear", "lanczos", "spline"] {
        let l = r
            .lines
            .iter()
            .find(|l| l.starts_with(&format!("PLAN rgb24 rgb24 {name} ")))
            .unwrap();
        assert!(l.contains("route=1 refused=0"), "{l}");
    }
}

/// A failing child of a name leaves that name out alone and the base's answer standing (a match, exact). Its retry is the name's own children
/// (three, four for Lanczos with its `sin`), once, on a thread nobody waits for: the call that finds the name due is told what is known at
/// once, and the name joins the set when the retry answers.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_name_whose_child_fails_is_left_out_alone_and_asked_again_alone() {
    for (name, needle, kind, retry) in [
        ("spline", "flags=spline", "spline", 3),
        ("neighbor", "flags=neighbor", "neighbor", 3),
        ("lanczos", "flags=lanczos", "lanczos", 4),
        ("sin", "sin(", "lanczos", 4),
    ] {
        let r = scenario(
            &format!("fail_{name}"),
            &fails(name, needle),
            &format!("call;call;rm=fail_{name};sleep=5600;call;call;settle=30000;sleep=11000;call"),
            &[&format!("fail_{name}")],
        );
        assert_eq!(r.answer("RESULT1"), without(kind), "{name}: {:?}", r.lines);
        assert!(r.line("RESULT1").ends_with("exact=true"), "{name}: the base stands");
        assert_eq!(r.runs("RESULT1"), 20, "{name}");
        assert_eq!(r.answer("RESULT2"), without(kind), "{name}");
        assert_eq!(r.runs("RESULT2"), 20, "{name}: asked again within the backoff");
        // The call that finds it due is told what is known at once: the retry is not its own.
        assert_eq!(r.answer("RESULT3"), without(kind), "{name}: {:?}", r.lines);
        assert!(
            r.number("RESULT3", " ms=") < 3_000,
            "{name}: the caller waited for the retry: {:?}",
            r.lines
        );
        // A second call during the retry does not start another (one flight).
        assert_eq!(r.answer("RESULT4"), without(kind), "{name}");
        // ... and the name joins when the retry answers: its own children, once, and no other.
        assert_eq!(r.answer("RESULT5"), ALL, "{name}: {:?}", r.lines);
        assert_eq!(
            r.runs("RESULT5"),
            20 + retry,
            "{name}: the retry is the name's own children and no other, once"
        );
        // Proven, it is not asked again.
        assert_eq!(r.runs("RESULT6"), 20 + retry, "{name}");
    }
}

/// A picture that comes back short is no answer (the name is open, not different): its retry recovers it.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_picture_that_comes_back_short_is_open_and_not_different() {
    let body = "case \"$*\" in *'flags=lanczos'*) if [ -e \"$KERF_SC_DIR/short\" ]; then \"$REAL\" \"$@\" | head -c 1000; exit 0; fi;; esac";
    let r = scenario("short", body, "call;rm=short;sleep=5600;call;settle=30000", &["short"]);
    assert_eq!(r.answer("RESULT1"), without("lanczos"), "{:?}", r.lines);
    assert!(r.line("RESULT1").ends_with("exact=true"));
    assert_eq!(r.answer("RESULT2"), without("lanczos"), "told at once: {:?}", r.lines);
    assert_eq!(r.answer("RESULT3"), ALL, "{:?}", r.lines);
    assert_eq!(
        r.runs("RESULT3"),
        24,
        "the lanczos question alone, three chains and the `sin`"
    );
}

/// Two names that fail apart have a backoff apart: one repaired, it joins at its retry while the other is backing off for twice as long,
/// asked again alone then. (Each retry is on a thread of its own; the callers are told what is known, and the names join later.)
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_two_names_back_off_apart() {
    let body = format!("{}\n{}", fails("spline", "flags=spline"), fails("neighbor", "flags=neighbor"));
    // t=0 both fail (retry at 5 s); spline is repaired at 1 s; the call at 8 s retries both in the background: spline joins, neighbor fails
    // again (the retry 10 s later); a call 3 s after that asks nothing; the call at 20 s retries neighbor alone.
    let r = scenario(
        "two_names",
        &body,
        "call;sleep=1000;rm=fail_spline;sleep=6000;call;sleep=3000;call;rm=fail_neighbor;call;sleep=9000;call;settle=30000",
        &["fail_spline", "fail_neighbor"],
    );
    assert_eq!(
        r.answer("RESULT1"),
        without("spline").replace("neighbor=1", "neighbor=0"),
        "{:?}",
        r.lines
    );
    assert_eq!(r.runs("RESULT1"), 20);
    // Told at once: both still out. Then spline is in and neighbor is not.
    assert_eq!(
        r.answer("RESULT2"),
        without("spline").replace("neighbor=1", "neighbor=0"),
        "{:?}",
        r.lines
    );
    assert!(r.number("RESULT2", " ms=") < 3_000);
    assert_eq!(r.answer("RESULT3"), without("neighbor"), "{:?}", r.lines);
    assert_eq!(r.runs("RESULT3"), 26, "spline's three and neighbor's three, nothing else");
    assert_eq!(r.number("RESULT3", "n_spline="), 6);
    assert_eq!(r.number("RESULT3", "n_neighbor="), 6);
    assert_eq!(r.answer("RESULT4"), without("neighbor"), "backing off for twice as long");
    assert_eq!(r.runs("RESULT4"), 26, "not asked");
    assert_eq!(r.answer("RESULT5"), without("neighbor"), "told at once: {:?}", r.lines);
    assert_eq!(r.answer("RESULT6"), ALL, "{:?}", r.lines);
    assert_eq!(r.runs("RESULT6"), 29, "neighbor alone");
    assert_eq!(r.number("RESULT6", "n_spline="), 6, "spline, proven, is not asked again");
}

/// A base that does not come back with a name that fails with it: nothing about the name is counted against it (it had no model), the base's
/// backoff stands alone (the base and the names, never asked, are the caller's own); when the base is back and the name still fails, the name
/// backs off alone and its retry is the name's own, in the background.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_base_that_is_open_leaves_the_names_asked_with_it_uncounted() {
    let body = format!("{}\n{}", fails("base", "flags=bilinear"), fails("spline", "flags=spline"));
    let r = scenario(
        "base_then_name",
        &body,
        "call;call;rm=fail_base;sleep=5600;call;rm=fail_spline;sleep=5600;call;settle=30000",
        &["fail_base", "fail_spline"],
    );
    assert_eq!(r.answer("RESULT1"), "Unknown", "{:?}", r.lines);
    assert_eq!(r.answer("RESULT2"), "Unknown");
    assert_eq!(r.runs("RESULT2"), 20);
    // The base is back: it and every name are asked together, by the caller (they are its own questions); spline still fails.
    assert_eq!(r.answer("RESULT3"), without("spline"), "{:?}", r.lines);
    assert!(r.line("RESULT3").ends_with("exact=true"));
    assert_eq!(r.runs("RESULT3"), 40);
    // Spline after its own backoff of 5 s (its first failure was counted in round three): in the background, told at once, then in.
    assert_eq!(r.answer("RESULT4"), without("spline"), "told at once: {:?}", r.lines);
    assert_eq!(r.answer("RESULT5"), ALL, "{:?}", r.lines);
    assert_eq!(r.runs("RESULT5"), 43);
}

/// Callers that arrive together share one measurement (one flight: the others wait for it, and are told what it found), a failure all of them
/// see is counted once, and no caller is told an answer that an earlier one was told more of. (Four callers, not the four-fold run.)
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_callers_that_arrive_together_count_a_failure_once() {
    // A name's child fails for four callers at once.
    let r = scenario(
        "par_name",
        &fails("spline", "flags=spline"),
        "par=4;rm=fail_spline;sleep=5600;call;settle=30000",
        &["fail_spline"],
    );
    let told = r.all("PAR1.");
    assert_eq!(told.len(), 4);
    for l in &told {
        assert!(l.contains("Match kernel="), "{l}");
        assert!(l.contains(&without("spline")), "{l}");
    }
    assert_eq!(r.runs("RESULT1"), 20, "one measurement for four callers");
    assert_eq!(r.answer("RESULT1"), without("spline"));
    // One failure counted (a backoff of 5 s, not of 5 min): the name is retried, alone and once, at the next call after 5.6 s.
    assert_eq!(r.answer("RESULT2"), without("spline"), "told at once: {:?}", r.lines);
    assert_eq!(r.answer("RESULT3"), ALL, "{:?}", r.lines);
    assert_eq!(r.runs("RESULT3"), 23, "spline alone, once");
    // The base's children fail for four callers at once: all are told Unknown; the failure is counted once and measured once.
    let r = scenario(
        "par_base",
        &fails("base", "flags=bilinear"),
        "par=4;rm=fail_base;sleep=5600;call",
        &["fail_base"],
    );
    for l in r.all("PAR1.") {
        assert!(l.contains("Unknown"), "{l}");
    }
    assert_eq!(r.runs("RESULT1"), 20, "one measurement for four callers");
    assert_eq!(r.answer("RESULT1"), "Unknown");
    assert_eq!(r.answer("RESULT2"), ALL, "{:?}", r.lines);
    assert_eq!(r.runs("RESULT2"), 40, "everything, once");
}

/// A name that hangs again is retried off the caller's path: the call that finds it due is told what is known at once, the retry's children
/// are killed at their deadline (no process and no thread outlives it), and the backoff doubles.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_name_that_hangs_is_retried_off_the_callers_path_and_leaks_nothing() {
    let r = scenario(
        "hang_retry",
        "case \"$*\" in *'flags=spline'*) echo $$ >> \"$KERF_SC_DIR/pids\"; exec sleep 120;; esac",
        "threads;call;call;sleep=5600;call;sleep=23000;call;sleep=1000;threads",
        &[],
    );
    assert_eq!(r.answer("RESULT1"), without("spline"), "{:?}", r.lines);
    assert!(r.number("RESULT1", " ms=") >= 19_000, "{:?}", r.lines);
    assert_eq!(r.number("RESULT2", " ms="), 0, "asked again within the backoff: no wait");
    // The caller after the backoff pays nothing for the child's deadline: the retry is on a thread of its own.
    assert_eq!(r.answer("RESULT3"), without("spline"));
    assert!(r.number("RESULT3", " ms=") < 3_000, "{:?}", r.lines);
    assert!((20..=23).contains(&r.runs("RESULT3")), "{:?}", r.lines);
    // ... the retry is spline's three children, once, killed at their deadline (the wait above is longer), and the backoff is 10 s now.
    assert_eq!(r.answer("RESULT4"), without("spline"));
    assert_eq!(r.runs("RESULT4"), 23, "{:?}", r.lines);
    assert!(r.number("RESULT4", " ms=") < 3_000);
    assert_no_stalled_child_left(&r, "hang_retry");
    // No thread is left behind: the detached retry's thread has ended with it.
    let threads: Vec<usize> = r
        .all("THREADS")
        .iter()
        .map(|l| l["THREADS ".len()..].trim().parse().unwrap())
        .collect();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[0], threads[1], "threads before and after: {threads:?}");
}

/// A caller never waits for a name it did not ask for: the base alone and each name asked for are measured, and a name that hangs is waited
/// for only by the caller that asks for it.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_caller_never_waits_for_a_name_it_did_not_ask_for() {
    let hang = "case \"$*\" in *'flags=spline'*) echo $$ >> \"$KERF_SC_DIR/pids\"; exec sleep 120;; esac";
    let r = scenario(
        "ask_for",
        hang,
        "callfor=none;callfor=lanczos;callfor=neighbor+lanczos;callfor=none;callfor=spline;callfor=none",
        &[],
    );
    // The base alone: its ten children, and no name is measured.
    assert_eq!(
        r.answer("RESULT1"),
        without("lanczos")
            .replace("neighbor=1", "neighbor=0")
            .replace("spline=1", "spline=0")
    );
    assert_eq!(r.runs("RESULT1"), 10, "{:?}", r.lines);
    assert!(r.number("RESULT1", " ms=") < 15_000);
    // Lanczos: its four children (the spline's hang is not touched); neighbor: its three.
    assert_eq!(r.answer("RESULT2"), without("neighbor").replace("spline=1", "spline=0"));
    assert_eq!(r.runs("RESULT2"), 14);
    assert!(r.number("RESULT2", " ms=") < 15_000);
    assert_eq!(r.answer("RESULT3"), without("spline"));
    assert_eq!(r.runs("RESULT3"), 17);
    assert_eq!(r.number("RESULT3", "n_spline="), 0, "spline was never asked for");
    assert_eq!(r.runs("RESULT4"), 17);
    assert!(r.number("RESULT4", " ms=") < 3_000);
    // The one caller that asks for spline waits for its children (killed at their deadline); no one else does.
    assert_eq!(r.answer("RESULT5"), without("spline"));
    assert!(r.number("RESULT5", " ms=") >= 19_000, "{:?}", r.lines);
    assert_eq!(r.runs("RESULT5"), 20);
    assert!(r.number("RESULT6", " ms=") < 3_000);
    assert_no_stalled_child_left(&r, "ask_for");
}

/// A caller that needs only the base is woken when the base is in, while another caller's names (one of them hanging) are still running;
/// a caller that asks for the hanging name too waits for it, no longer than the measurement's bound.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_base_only_caller_is_woken_while_the_names_of_another_still_run_and_a_follower_is_bounded() {
    let hang = "case \"$*\" in *'flags=spline'*) echo $$ >> \"$KERF_SC_DIR/pids\"; exec sleep 120;; esac";
    let r = scenario("woken", hang, "pair=all:300:none", &[]);
    let (a, b) = (r.number("RESULT1A", " ms="), r.number("RESULT1B", " ms="));
    assert!(
        a >= 19_000,
        "the caller that asked for every name waits for the hanging one: {a} ms"
    );
    assert!(
        b < 8_000,
        "the base-only caller is woken when the base is in, not held by spline: {b} ms"
    );
    // (Told what is known when the base is in: bilinear is the base's own, and no name has joined yet.)
    let told = r.answer("RESULT1B");
    assert!(told.starts_with("bilinear=1") && told.ends_with("spline=0"), "{:?}", r.lines);
    assert_eq!(r.runs("RESULT1C"), 20, "one flight: {:?}", r.lines);
    // A follower of a hung name is bounded: it waits for the flight and no longer (the children's deadline is 20 s, the flight's bound 25).
    let r = scenario("follower", hang, "pair=all:2000:spline", &[]);
    let (a, b) = (r.number("RESULT1A", " ms="), r.number("RESULT1B", " ms="));
    assert!((19_000..27_000).contains(&a), "{a} ms");
    assert!(
        (15_000..26_000).contains(&b),
        "a follower waits for the hung flight, no longer than its bound: {b} ms"
    );
    assert_eq!(r.runs("RESULT1C"), 20, "the follower measured nothing: {:?}", r.lines);
    // A follower of a hung *base* likewise.
    let r = scenario(
        "follower_base",
        "case \"$*\" in *'flags=bilinear'*) echo $$ >> \"$KERF_SC_DIR/pids\"; exec sleep 120;; esac",
        "pair=all:1000:none",
        &[],
    );
    let (a, b) = (r.number("RESULT1A", " ms="), r.number("RESULT1B", " ms="));
    assert!((19_000..27_000).contains(&a), "{a} ms");
    assert!((15_000..26_000).contains(&b), "{b} ms");
    assert_eq!(r.answer("RESULT1A"), "Unknown");
    assert_eq!(r.answer("RESULT1B"), "Unknown");
    assert_eq!(r.runs("RESULT1C"), 20, "the follower measured nothing: {:?}", r.lines);
    assert_no_stalled_child_left(&r, "follower_base");
}

/// A second caller who arrives while the first is measuring waits for that measurement and is told its answer: it measures nothing, and so
/// can never be told another kernel's (the wrapper runs every child started after `late` exists with `-cpuflags 0`, the C scaler: none is
/// started).
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_late_caller_follows_the_flight_and_measures_nothing_of_its_own() {
    let body = "if [ -e \"$KERF_SC_DIR/late\" ]; then\n  exec \"$REAL\" -cpuflags 0 \"$@\"\nfi\nsleep 3";
    let r = scenario("follows", body, "stale", &[]);
    let (a, b) = (r.number("RESULT1A", " ms="), r.number("RESULT1B", " ms="));
    assert!(
        b >= 1_500,
        "the late caller waited for the flight ({b} ms, the flight {a} ms)"
    );
    assert!(b < a + 1_500, "... and no longer ({b} ms, the flight {a} ms)");
    for tag in ["RESULT1A", "RESULT1B", "RESULT1C"] {
        assert_eq!(r.kernel(tag), "X86", "{tag}: {:?}", r.lines);
        assert_eq!(r.answer(tag), ALL, "{tag}: {:?}", r.lines);
    }
    assert_eq!(r.runs("RESULT1C"), 20, "twenty children for two callers: {:?}", r.lines);
}

/// A retry that lands after the FFmpeg changed under the process (its children now run another kernel: the wrapper's `late`, `-cpuflags 0`)
/// is judged through the model that stands: the name is **never proven on the strength of another kernel's pictures**; it is out, for good,
/// and nothing is asked again. In both directions (the model X86 and the retry C, the model C and the retry X86).
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_retry_of_another_kernel_is_never_believed_and_is_out_for_good() {
    let body = format!(
        "{}\nif [ -e \"$KERF_SC_DIR/late\" ]; then exec \"$REAL\" -cpuflags 0 \"$@\"; fi",
        fails("spline", "flags=spline")
    );
    // The model stands as X86; the retry's children are C.
    let r = scenario(
        "kernel_x86_then_c",
        &body,
        "call;touch=late;rm=fail_spline;sleep=5600;call;settle=15000;sleep=12000;call",
        &["fail_spline"],
    );
    assert_eq!(r.kernel("RESULT1"), "X86");
    assert_eq!(r.answer("RESULT1"), without("spline"));
    assert_eq!(
        r.answer("RESULT3"),
        without("spline"),
        "not proven on a C kernel's pictures: {:?}",
        r.lines
    );
    assert_eq!(r.kernel("RESULT3"), "X86", "the model that stands does not change");
    assert_eq!(r.runs("RESULT3"), 23);
    assert_eq!(r.answer("RESULT4"), without("spline"));
    assert_eq!(r.runs("RESULT4"), 23, "out for good: not asked again");
    // The model stands as C (the children of the first measurement are C); the retry's are X86.
    let r = scenario(
        "kernel_c_then_x86",
        &body,
        "call;rm=late;rm=fail_spline;sleep=5600;call;settle=15000;sleep=12000;call",
        &["fail_spline", "late"],
    );
    assert_eq!(r.kernel("RESULT1"), "C");
    assert_eq!(r.answer("RESULT1"), without("spline"));
    assert_eq!(r.answer("RESULT3"), without("spline"), "{:?}", r.lines);
    assert_eq!(r.kernel("RESULT3"), "C");
    assert_eq!(r.runs("RESULT4"), 23, "out for good: not asked again");
}

/// Many callers at once (the previews and the players of a busy machine) asking for different names while a name fails and is repaired: every
/// answer is the one that stands (the kernel never changes, `bilinear` is always in, a name that was in an answer is in that thread's next, no
/// call that starts after an answer was given is told Unknown), the measurements are what a single flight makes of them (not a run more), and
/// the name joins by its retry.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_many_callers_are_told_answers_that_never_go_back_and_the_runs_are_the_single_flights() {
    let r = scenario(
        "stress",
        &fails("spline", "flags=spline"),
        "threads;stress=8:12000:4000:fail_spline;settle=20000;sleep=1000;threads",
        &["fail_spline"],
    );
    let stress = r.line("STRESS");
    assert!(stress.contains("violations=0"), "{stress}");
    let asked: usize = stress
        .split("asked=")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(asked >= 100, "callers asked {asked} times");
    assert_eq!(r.answer("RESULT1"), ALL, "{:?}", r.lines);
    // Base 10, lanczos 4, neighbor 3, spline 3 (failed), spline's retry 3: nothing twice.
    assert_eq!(r.runs("RESULT1"), 23, "{:?}", r.lines);
    assert_eq!(r.number("RESULT1", "n_spline="), 6);
    assert_eq!(r.runs("RESULT2"), 23, "settled: nothing is asked again");
    let threads: Vec<usize> = r
        .all("THREADS")
        .iter()
        .map(|l| l["THREADS ".len()..].trim().parse().unwrap())
        .collect();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[0], threads[1], "no thread left behind: {threads:?}");
}

/// An FFmpeg that cannot even be started (the wrapper is not executable at first): every child fails to spawn, the base is Unknown, nobody
/// waits, and the failure is counted once; when it can be started the base and the names are measured.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_an_ffmpeg_that_cannot_be_started_is_a_failure_counted_once_and_recovers() {
    let r = scenario("noexec", "", "call;call;exec;sleep=5600;call;call", &["noexec"]);
    assert_eq!(r.answer("RESULT1"), "Unknown", "{:?}", r.lines);
    assert!(r.number("RESULT1", " ms=") < 5_000);
    assert_eq!(r.answer("RESULT2"), "Unknown");
    assert!(r.number("RESULT2", " ms=") < 1_000);
    assert_eq!(r.runs("RESULT2"), 0, "nothing could run");
    assert_eq!(r.answer("RESULT3"), ALL, "{:?}", r.lines);
    assert_eq!(r.runs("RESULT3"), 20, "the base and the names, once");
    assert_eq!(r.runs("RESULT4"), 20);
}
