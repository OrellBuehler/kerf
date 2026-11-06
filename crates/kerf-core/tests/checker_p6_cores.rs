//! `checker/p6/cores`: `kerf_core::cpu_cores()` (the cores every budget is a share of: `cores` of `engine::cpu::machine`, taken from one read of
//! the affinity mask with the cgroup quota of `engine/cpu/cgroup.rs`, a mirror of the standard library's) held to
//! `std::thread::available_parallelism()` on the toolchain in use: **equal, in every hierarchy and under every mask.** A difference is a
//! budget that changed for a user.
//!
//! * `checker_p6_cores_equal_available_parallelism_on_fake_cgroup_trees`: the process is started in a private mount namespace
//!   (`unshare -m`) with `/sys/fs/cgroup`, `/proc/<pid>/cgroup` and `/proc/<pid>/mountinfo` replaced by files of the test's own, so the
//!   standard library and the mirror read the same invented hierarchy: cgroup v2 and v1 (one-CPU rounding down, `max`, odd periods, a
//!   period of 0, files that are empty, garbage, directories or missing, ancestors in every order of size, a walk that ends early, deep
//!   chains, paths with spaces and bytes that are not UTF-8, the mount points of v1 found through `mountinfo`, bind mounts of a
//!   sub-directory, hybrid files with both lines, lines with no third field), each under affinity masks of one, two, three and four CPUs.
//! * `checker_p6_cores_equal_available_parallelism_under_real_cgroup_v1_quotas`: real cgroups (nested ancestors, odd periods, no quota
//!   anywhere or at one level) the kernel enforces, where the host lets this process make them.
//!
//! ```text
//! cargo test -p kerf-core --no-default-features --test checker_p6_cores -- --ignored --nocapture
//! ```
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr, clippy::semicolon_if_nothing_returned)]

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD: &str = "CHECKER_P6_CHILD";

// ---- the child ---------------------------------------------------------------------------------------------------------------------------------

#[test]
#[ignore = "run by the parents below, in a process of its own"]
fn checker_p6_child() {
    if std::env::var(CHILD).is_err() {
        return;
    }
    let kerf = kerf_core::cpu_cores();
    let budget = kerf_core::cpu_threads();
    let standard = std::panic::catch_unwind(|| std::thread::available_parallelism().map_or(1, std::num::NonZero::get));
    let std_text = standard.map_or("panic".to_string(), |n| n.to_string());
    eprintln!(
        "P6 kerf={kerf} std={std_text} count={} budget={budget}",
        kerf_core::ffmpeg_cpu_count().map_or(-1, |c| c as i64)
    );
}

fn field(line: &str, key: &str) -> String {
    line.split(&format!("{key}="))
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or("?")
        .to_string()
}

fn run(cmd: &mut Command, limit: Duration) -> Option<(bool, String)> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    let (mut out, mut err) = (child.stdout.take()?, child.stderr.take()?);
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
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let text: String = readers.into_iter().map(|r| r.join().unwrap_or_default()).collect();
    Some((status.is_some_and(|s| s.success()), text))
}

fn child_args(cmd: &mut Command) {
    let exe = std::env::current_exe().expect("the test binary");
    cmd.arg(exe)
        .args(["checker_p6_child", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1");
}

// ---- fake trees --------------------------------------------------------------------------------------------------------------------------------

/// A set of files laid out like `/sys/fs/cgroup`, `/proc/<pid>/cgroup` and `/proc/<pid>/mountinfo`, and a mount point for v1 hierarchies
/// that `mountinfo` names, in a directory of its own.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("kerf-p6-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sys")).unwrap();
        std::fs::create_dir_all(root.join("mnt")).unwrap();
        std::fs::write(root.join("cgroup"), "").unwrap();
        std::fs::write(root.join("mountinfo"), "").unwrap();
        Self { root }
    }

    fn put(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn dir(&self, rel: &str) {
        std::fs::create_dir_all(self.root.join(rel)).unwrap();
    }

    fn cgroup(&self, bytes: &[u8]) {
        std::fs::write(self.root.join("cgroup"), bytes).unwrap();
    }

    fn mountinfo(&self, text: &str) {
        std::fs::write(self.root.join("mountinfo"), text).unwrap();
    }

    /// The path of the mount point directory `mnt/` (what a `mountinfo` line names).
    fn mnt(&self) -> String {
        self.root.join("mnt").display().to_string()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A v2 group at `rel` (under the mount) with a `cpu.max` of `max` (`None`: the file is missing, `Some(None)` a directory in its place), and
/// the file that says the group is on the v2 hierarchy.
fn v2_level(t: &Tree, rel: &str, max: Option<Option<&str>>) {
    t.put(&format!("sys/{rel}/cgroup.controllers"), "cpu\n");
    match max {
        None => {}
        Some(None) => t.dir(&format!("sys/{rel}/cpu.max")),
        Some(Some(text)) => t.put(&format!("sys/{rel}/cpu.max"), text),
    }
}

/// A v1 group at `rel` under `base` (`sys/cpu`, `sys/cpu,cpuacct` or `mnt`) with quota and period files.
fn v1_level(t: &Tree, base: &str, rel: &str, quota: Option<&str>, period: Option<&str>) {
    t.dir(&format!("{base}/{rel}"));
    if let Some(q) = quota {
        t.put(&format!("{base}/{rel}/cpu.cfs_quota_us"), q);
    }
    if let Some(p) = period {
        t.put(&format!("{base}/{rel}/cpu.cfs_period_us"), p);
    }
}

struct Scenario {
    name: &'static str,
    /// The quota a person would read off the files, in whole CPUs (`usize::MAX`: none), or `None` where the case is about the two
    /// implementations disagreeing about nothing in particular (the standard library may even panic).
    expect: Option<usize>,
    build: Box<dyn Fn(&Tree)>,
}

fn scenario(name: &'static str, expect: Option<usize>, build: impl Fn(&Tree) + 'static) -> Scenario {
    Scenario {
        name,
        expect,
        build: Box::new(build),
    }
}

const NONE: usize = usize::MAX;

fn mount_line(root: &str, point: &str, opts: &str, fs: &str) -> String {
    format!("36 35 98:0 {root} {point} rw,noatime master:1 - {fs} cgroup {opts}\n")
}

fn scenarios() -> Vec<Scenario> {
    let mut v: Vec<Scenario> = Vec::new();
    // ---- cgroup v2
    let v2 = |name: &'static str, expect: Option<usize>, cgroup: &'static str, f: fn(&Tree)| {
        scenario(name, expect, move |t| {
            t.cgroup(cgroup.as_bytes());
            f(t);
        })
    };
    v.push(v2("v2: two CPUs", Some(2), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("200000 100000\n")))
    }));
    v.push(v2("v2: half a CPU is none, then one", Some(0), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("50000 100000\n")))
    }));
    v.push(v2("v2: 1.5 CPUs round down", Some(1), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("150000 100000\n")))
    }));
    v.push(v2("v2: 2.99999 CPUs round down", Some(2), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("299999 100000\n")))
    }));
    v.push(v2("v2: max", Some(NONE), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("max 100000\n")))
    }));
    v.push(v2("v2: max alone", Some(NONE), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("max\n")))
    }));
    v.push(v2("v2: an empty cpu.max", Some(NONE), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("")))
    }));
    v.push(v2("v2: a cpu.max of a newline", Some(NONE), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("\n")))
    }));
    v.push(v2("v2: garbage", Some(NONE), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("abc def\n")))
    }));
    v.push(v2("v2: a period of 0", Some(NONE), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("100000 0\n")))
    }));
    v.push(v2("v2: a limit of 0", Some(0), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("0 100000\n")))
    }));
    v.push(v2("v2: two spaces between the fields", Some(NONE), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("200000  100000\n")))
    }));
    v.push(v2("v2: a tab between the fields", None, "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("200000\t100000\n")))
    }));
    v.push(v2("v2: only the first line counts", Some(2), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("200000 100000\n300000 100000\n")))
    }));
    v.push(v2("v2: a limit of usize::MAX over a period of 1", None, "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("18446744073709551615 1\n")))
    }));
    v.push(v2("v2: a limit that overflows", Some(NONE), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("99999999999999999999 100000\n")))
    }));
    v.push(v2("v2: the ancestor is smaller", Some(1), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("300000 100000\n")));
        v2_level(t, "a", Some(Some("100000 100000\n")));
        v2_level(t, "", Some(Some("max 100000\n")));
    }));
    v.push(v2("v2: the ancestor is larger", Some(1), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("100000 100000\n")));
        v2_level(t, "a", Some(Some("300000 100000\n")));
        v2_level(t, "", Some(Some("400000 100000\n")));
    }));
    v.push(v2("v2: odd periods down the chain", Some(0), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("150000 70000\n")));
        v2_level(t, "a", Some(Some("99999 33333\n")));
        v2_level(t, "", Some(Some("5 7\n")));
    }));
    v.push(v2("v2: max below, a limit above", Some(2), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("max 100000\n")));
        v2_level(t, "a", Some(Some("200000 100000\n")));
    }));
    v.push(v2(
        "v2: a cpu.max that is a directory, a limit above",
        Some(2),
        "0::/a/b\n",
        |t| {
            v2_level(t, "a/b", Some(None));
            v2_level(t, "a", Some(Some("200000 100000\n")));
        },
    ));
    v.push(v2("v2: no cpu.max in the group, a limit above", Some(2), "0::/a/b\n", |t| {
        v2_level(t, "a/b", None);
        v2_level(t, "a", Some(Some("200000 100000\n")));
    }));
    v.push(v2("v2: an empty cpu.max ends the walk", Some(4), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("400000 100000\n")));
        v2_level(t, "a", Some(Some("")));
        v2_level(t, "", Some(Some("100000 100000\n")));
    }));
    v.push(v2(
        "v2: a cpu.max without a period ends the walk",
        Some(4),
        "0::/a/b\n",
        |t| {
            v2_level(t, "a/b", Some(Some("400000 100000\n")));
            v2_level(t, "a", Some(Some("200000\n")));
            v2_level(t, "", Some(Some("100000 100000\n")));
        },
    ));
    v.push(v2("v2: garbage above does not end the walk", Some(1), "0::/a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("400000 100000\n")));
        v2_level(t, "a", Some(Some("junk junk\n")));
        v2_level(t, "", Some(Some("100000 100000\n")));
    }));
    v.push(v2("v2: the root group", Some(2), "0::/\n", |t| {
        v2_level(t, "", Some(Some("200000 100000\n")))
    }));
    v.push(v2("v2: no cgroup.controllers in the group", Some(NONE), "0::/a/b\n", |t| {
        t.put("sys/a/b/cpu.max", "100000 100000\n");
        t.put("sys/a/cgroup.controllers", "cpu\n");
    }));
    v.push(v2("v2: a group that does not exist", Some(NONE), "0::/a/b/c\n", |t| {
        v2_level(t, "a/b", Some(Some("100000 100000\n")));
    }));
    v.push(v2("v2: a trailing slash", Some(2), "0::/a/b/\n", |t| {
        v2_level(t, "a/b", Some(Some("200000 100000\n")))
    }));
    v.push(v2("v2: a double slash", None, "0::/a//b\n", |t| {
        v2_level(t, "a/b", Some(Some("200000 100000\n")))
    }));
    v.push(v2("v2: a dot-dot", None, "0::/a/../a/b\n", |t| {
        v2_level(t, "a/b", Some(Some("200000 100000\n")));
        v2_level(t, "a", Some(Some("100000 100000\n")));
    }));
    v.push(v2("v2: six levels", Some(2), "0::/d1/d2/d3/d4/d5/d6\n", |t| {
        for (rel, max) in [
            ("d1/d2/d3/d4/d5/d6", "600000 100000\n"),
            ("d1/d2/d3/d4/d5", "max 100000\n"),
            ("d1/d2/d3/d4", "300000 100000\n"),
            ("d1/d2/d3", "500000 100000\n"),
            ("d1/d2", "250000 100000\n"),
            ("d1", "700000 100000\n"),
            ("", "800000 100000\n"),
        ] {
            v2_level(t, rel, Some(Some(max)));
        }
    }));
    v.push(scenario("v2: bytes that are not UTF-8 in the path", Some(2), |t| {
        t.cgroup(b"0::/a/\xffb\n");
        let name = OsString::from_vec(b"a/\xffb".to_vec());
        let dir = t.root.join("sys").join(&name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cgroup.controllers"), "cpu\n").unwrap();
        std::fs::write(dir.join("cpu.max"), "200000 100000\n").unwrap();
    }));
    v.push(v2("v2: a space in the path", Some(2), "0::/a b\n", |t| {
        v2_level(t, "a b", Some(Some("200000 100000\n")))
    }));
    v.push(v2("v2: no newline at the end of the cgroup file", Some(2), "0::/a/b", |t| {
        v2_level(t, "a/b", Some(Some("200000 100000\n")));
    }));
    v.push(v2(
        "v2: a line with no path (the standard library indexes past it)",
        None,
        "0::",
        |t| {
            v2_level(t, "", Some(Some("200000 100000\n")));
        },
    ));
    // ---- cgroup v1
    let v1 = |name: &'static str, expect: Option<usize>, f: fn(&Tree)| {
        scenario(name, expect, move |t| {
            t.cgroup(b"3:cpu,cpuacct:/x/y\n");
            f(t);
        })
    };
    v.push(v1("v1: two CPUs", Some(2), |t| {
        v1_level(t, "sys/cpu", "x/y", Some("200000\n"), Some("100000\n"))
    }));
    v.push(v1("v1: -1 is no quota", Some(NONE), |t| {
        v1_level(t, "sys/cpu", "x/y", Some("-1\n"), Some("100000\n"))
    }));
    v.push(v1("v1: the ancestors, the smallest", Some(1), |t| {
        v1_level(t, "sys/cpu", "x/y", Some("300000"), Some("100000"));
        v1_level(t, "sys/cpu", "x", Some("100000"), Some("100000"));
        v1_level(t, "sys/cpu", "", Some("-1"), Some("100000"));
    }));
    v.push(v1("v1: odd periods down the chain", Some(1), |t| {
        v1_level(t, "sys/cpu", "x/y", Some("150000"), Some("70000"));
        v1_level(t, "sys/cpu", "x", Some("45000"), Some("30000"));
        v1_level(t, "sys/cpu", "", Some("100000"), Some("33333"));
    }));
    v.push(v1("v1: a period of 0", Some(NONE), |t| {
        v1_level(t, "sys/cpu", "x/y", Some("200000"), Some("0"))
    }));
    v.push(v1("v1: white space around the numbers", Some(2), |t| {
        v1_level(t, "sys/cpu", "x/y", Some(" 200000 \n"), Some("\t100000\n\n"));
    }));
    v.push(v1("v1: a quota that is garbage", Some(NONE), |t| {
        v1_level(t, "sys/cpu", "x/y", Some("abc"), Some("100000"))
    }));
    v.push(v1("v1: no period file", Some(NONE), |t| {
        v1_level(t, "sys/cpu", "x/y", Some("200000"), None)
    }));
    v.push(v1("v1: a quota file that is a directory", Some(NONE), |t| {
        v1_level(t, "sys/cpu", "x/y", None, Some("100000"));
        t.dir("sys/cpu/x/y/cpu.cfs_quota_us");
    }));
    v.push(v1("v1: only the cpu,cpuacct mount", Some(2), |t| {
        v1_level(t, "sys/cpu,cpuacct", "x/y", Some("200000"), Some("100000"));
    }));
    v.push(v1("v1: both mounts: the first that holds the group", Some(3), |t| {
        v1_level(t, "sys/cpu", "x/y", Some("300000"), Some("100000"));
        v1_level(t, "sys/cpu,cpuacct", "x/y", Some("100000"), Some("100000"));
    }));
    v.push(v1("v1: the first mount lacks the group", Some(2), |t| {
        t.dir("sys/cpu/other");
        v1_level(t, "sys/cpu,cpuacct", "x/y", Some("200000"), Some("100000"));
    }));
    v.push(v1("v1: the mount point mountinfo names", Some(2), |t| {
        v1_level(t, "mnt", "x/y", Some("200000"), Some("100000"));
        t.mountinfo(&format!(
            "20 1 0:5 / /proc rw - proc proc rw\n{}",
            mount_line("/", &t.mnt(), "rw,cpu,cpuacct", "cgroup")
        ));
    }));
    v.push(v1("v1: a bind mount of /x", Some(2), |t| {
        v1_level(t, "mnt", "y", Some("200000"), Some("100000"));
        t.mountinfo(&mount_line("/x", &t.mnt(), "rw,cpu", "cgroup"));
    }));
    v.push(v1(
        "v1: a bind mount of a directory that does not hold the group",
        Some(NONE),
        |t| {
            v1_level(t, "mnt", "y", Some("200000"), Some("100000"));
            t.mountinfo(&mount_line("/z", &t.mnt(), "rw,cpu", "cgroup"));
        },
    ));
    v.push(v1("v1: mountinfo lines of other kinds first", Some(2), |t| {
        v1_level(t, "mnt", "x/y", Some("200000"), Some("100000"));
        t.mountinfo(&format!(
            "{}{}{}",
            mount_line("/", "/nowhere", "rw,memory", "cgroup"),
            mount_line("/", "/also-nowhere", "rw,cpu", "cgroup2"),
            mount_line("/", &t.mnt(), "rw,cpuacct,cpu", "cgroup")
        ));
    }));
    v.push(v1("v1: a malformed mountinfo line", Some(NONE), |t| {
        v1_level(t, "mnt", "x/y", Some("200000"), Some("100000"));
        t.mountinfo("1 2 3\n");
    }));
    // ---- the cgroup file itself
    let file = |name: &'static str, expect: Option<usize>, cgroup: &'static str, f: fn(&Tree)| {
        scenario(name, expect, move |t| {
            t.cgroup(cgroup.as_bytes());
            f(t);
        })
    };
    let both: fn(&Tree) = |t| {
        v1_level(t, "sys/cpu", "x/y", Some("300000"), Some("100000"));
        v2_level(t, "a/b", Some(Some("100000 100000\n")));
    };
    v.push(file(
        "hybrid: the v1 line wins over the v2 line",
        Some(3),
        "2:cpu:/x/y\n0::/a/b\n",
        both,
    ));
    v.push(file(
        "hybrid: a v1 line after a v2 line wins",
        Some(3),
        "0::/a/b\n2:cpu:/x/y\n",
        both,
    ));
    v.push(file(
        "hybrid: only a v2 line and a v1 line without cpu",
        Some(1),
        "4:memory:/m\n0::/a/b\n",
        both,
    ));
    v.push(file("hybrid: cpuset is not cpu", Some(1), "5:cpuset:/x/y\n0::/a/b\n", both));
    v.push(file(
        "hybrid: a later line without a third field forgets the v1 line",
        Some(NONE),
        "2:cpu:/x/y\n1:cpu\n",
        both,
    ));
    v.push(file(
        "hybrid: a v2 line without a path forgets nothing it had",
        Some(NONE),
        "2:cpu:/x/y\n0:\n",
        both,
    ));
    v.push(file("a systemd name only", Some(NONE), "1:name=systemd:/\n", both));
    v.push(file("an empty cgroup file", Some(NONE), "", both));
    v.push(file("blank lines", Some(1), "\n\n0::/a/b\n\n", both));
    v.push(file("a v1 line whose path is the root", Some(NONE), "2:cpu:/\n", |t| {
        v1_level(t, "sys/cpu", "", Some("-1"), Some("100000"));
    }));
    v
}

/// `child` run in a private mount namespace where the three files are the tree's, under the affinity mask `mask`.
fn run_in(tree: &Tree, mask: Option<&str>) -> Option<String> {
    let script = format!(
        "mount --bind '{sys}' /sys/fs/cgroup && mount --bind '{cg}' /proc/$$/cgroup && mount --bind '{mi}' /proc/$$/mountinfo && exec \"$@\"",
        sys = tree.root.join("sys").display(),
        cg = tree.root.join("cgroup").display(),
        mi = tree.root.join("mountinfo").display()
    );
    let mut cmd = Command::new("unshare");
    cmd.args(["-m", "sh", "-c", &script, "sh"]);
    if let Some(mask) = mask {
        cmd.args(["taskset", "-c", mask]);
    }
    child_args(&mut cmd);
    let (_, text) = run(&mut cmd, Duration::from_secs(60))?;
    text.lines().find(|l| l.starts_with("P6 ")).map(str::to_string)
}

#[test]
#[ignore = "needs root, unshare and taskset (a private mount namespace)"]
fn checker_p6_cores_equal_available_parallelism_on_fake_cgroup_trees() {
    // Is a namespace to be had?
    {
        let t = Tree::new("probe");
        t.cgroup(b"0::/a/b\n");
        v2_level(&t, "a/b", Some(Some("200000 100000\n")));
        match run_in(&t, None) {
            Some(line) if field(&line, "std") == "2" || field(&line, "std") == "1" => {}
            other => {
                eprintln!("CHECKER-P6 cores: no private mount namespace here ({other:?}): not run");
                return;
            }
        }
    }
    let all = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let masks: Vec<Option<&str>> = vec![None, Some("0"), Some("0-1"), Some("0-2")];
    let (mut failures, mut compared, mut expected_ok, mut expected_off, mut binding, mut std_panics) =
        (Vec::new(), 0usize, 0usize, 0usize, 0usize, 0usize);
    for (i, sc) in scenarios().iter().enumerate() {
        for mask in &masks {
            let t = Tree::new(&format!("s{i}"));
            (sc.build)(&t);
            let Some(line) = run_in(&t, *mask) else {
                failures.push(format!("{} under {mask:?}: the child said nothing", sc.name));
                continue;
            };
            let (kerf, std_text, count) = (field(&line, "kerf"), field(&line, "std"), field(&line, "count"));
            compared += 1;
            if std_text == "panic" {
                std_panics += 1;
                eprintln!(
                    "CHECKER-P6 cores: (info) {}: the standard library panics; cores {kerf}",
                    sc.name
                );
                continue;
            }
            if kerf != std_text {
                failures.push(format!(
                    "{} under {mask:?} ({count} CPUs): cores() {kerf}, available_parallelism {std_text}",
                    sc.name
                ));
            }
            if let (Some(quota), Ok(count)) = (sc.expect, count.parse::<usize>()) {
                let want = count.min(quota.max(1));
                if std_text == want.to_string() {
                    expected_ok += 1;
                    binding += usize::from(quota < count);
                } else {
                    expected_off += 1;
                    eprintln!(
                        "CHECKER-P6 cores: (info) {} under {mask:?}: the files say a quota of {quota}, the standard library has {std_text} of {count} CPUs",
                        sc.name
                    );
                }
            }
        }
    }
    eprintln!(
        "CHECKER-P6 cores: {compared} (hierarchy, mask) pairs on a host of {all} CPUs: cores() equals available_parallelism in all but {} of them; the files' reading agrees with the standard library in {expected_ok} ({binding} with a quota that binds), differs in {expected_off}; the library panics in {std_panics}",
        failures.len()
    );
    assert!(
        binding > 60,
        "too few cases had a quota that binds ({binding}): the battery would prove nothing"
    );
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

// ---- real cgroups ------------------------------------------------------------------------------------------------------------------------------

struct Group {
    dir: PathBuf,
}

impl Group {
    fn top(hierarchy: &str, tag: &str) -> Option<Self> {
        let root = Path::new("/sys/fs/cgroup").join(hierarchy);
        if !root.is_dir() {
            return None;
        }
        let dir = root.join(format!("kerf-p6-{}-{tag}", std::process::id()));
        std::fs::create_dir(&dir).ok()?;
        Some(Self { dir })
    }

    fn sub(&self, name: &str) -> Option<Self> {
        let dir = self.dir.join(name);
        std::fs::create_dir(&dir).ok()?;
        Some(Self { dir })
    }

    fn write(&self, file: &str, value: &str) -> bool {
        std::fs::write(self.dir.join(file), value).is_ok()
    }

    fn quota(&self, quota: &str, period: &str) -> bool {
        // (A period is not to make a quota larger than a child's: free the quota first, set it last.)
        let (first, second) = if quota == "-1" {
            (("cpu.cfs_quota_us", quota), ("cpu.cfs_period_us", period))
        } else {
            (("cpu.cfs_period_us", period), ("cpu.cfs_quota_us", quota))
        };
        self.write(first.0, first.1) && self.write(second.0, second.1)
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.dir);
    }
}

#[test]
#[ignore = "needs a writable cgroup v1 cpu hierarchy"]
fn checker_p6_cores_equal_available_parallelism_under_real_cgroup_v1_quotas() {
    let Some(top) = Group::top("cpu", "top") else {
        eprintln!("CHECKER-P6 cores: no writable cgroup v1 cpu hierarchy here: not run");
        return;
    };
    // (parent quota, period, child quota, period, grandchild quota, period, the quota a person reads off, in whole CPUs). The kernel keeps a
    // child's bandwidth within its parent's, so a quota is never larger below than above: a smaller one below, or none ("-1"), is the only
    // thing a real hierarchy holds (the fake trees hold the other order).
    let cases: [(&str, &str, &str, &str, &str, &str, usize); 14] = [
        ("-1", "100000", "200000", "100000", "-1", "100000", 2),
        ("200000", "100000", "-1", "100000", "-1", "100000", 2),
        ("300000", "100000", "100000", "100000", "-1", "100000", 1),
        ("100000", "33333", "45000", "30000", "-1", "100000", 1),
        ("2500", "1000", "-1", "100000", "-1", "100000", 2),
        ("6000", "1000", "5000", "1000", "4500", "1000", 4),
        ("1000", "1000000", "-1", "100000", "-1", "100000", 0),
        ("200000", "100000", "-1", "100000", "150000", "100000", 1),
        ("-1", "100000", "-1", "100000", "-1", "100000", NONE),
        ("500000", "100000", "300000", "100000", "250000", "100000", 2),
        ("90000", "30000", "65000", "23000", "-1", "100000", 2),
        ("100000", "7000", "-1", "100000", "-1", "100000", 14),
        ("-1", "100000", "-1", "100000", "100000", "100000", 1),
        ("400000", "100000", "399999", "100000", "150001", "100000", 1),
    ];
    let (mut failures, mut compared, mut skipped) = (Vec::new(), 0usize, 0usize);
    for (n, (pq, pp, cq, cp, gq, gp, expect)) in cases.into_iter().enumerate() {
        let Some(child) = top.sub(&format!("c{n}")) else {
            skipped += 1;
            continue;
        };
        let Some(grand) = child.sub("g") else {
            skipped += 1;
            continue;
        };
        // (Children first: a parent's quota may be refused while a child's is larger, in either order of writes.)
        // (The top, then below it: a child cannot be given more than its parent has. The top is first set free of the last case.)
        let reset = top.quota("-1", "100000");
        if !(reset && top.quota(pq, pp) && child.quota(cq, cp) && grand.quota(gq, gp)) {
            eprintln!("CHECKER-P6 cores: (info) case {n}: the kernel refused the quotas {pq}/{pp}, {cq}/{cp}, {gq}/{gp}");
            skipped += 1;
            continue;
        }
        for mask in [None, Some("0"), Some("0-1"), Some("0-2")] {
            let mut cmd = Command::new("sh");
            cmd.args([
                "-c",
                &format!("echo $$ > '{}/cgroup.procs' || exit 99; exec \"$@\"", grand.dir.display()),
                "sh",
            ]);
            if let Some(mask) = mask {
                cmd.args(["taskset", "-c", mask]);
            }
            child_args(&mut cmd);
            let Some((_, text)) = run(&mut cmd, Duration::from_secs(60)) else {
                failures.push(format!("case {n}: the child did not run"));
                continue;
            };
            let Some(line) = text.lines().find(|l| l.starts_with("P6 ")) else {
                failures.push(format!("case {n} under {mask:?}: the child said nothing:\n{text}"));
                continue;
            };
            let (kerf, standard, count) = (field(line, "kerf"), field(line, "std"), field(line, "count"));
            compared += 1;
            eprintln!(
                "CHECKER-P6 cores: real cgroups {pq}/{pp} > {cq}/{cp} > {gq}/{gp} under {mask:?} ({count} CPUs): cores() {kerf}, available_parallelism {standard}"
            );
            if kerf != standard {
                failures.push(format!(
                    "case {n} under {mask:?}: cores() {kerf}, available_parallelism {standard}"
                ));
            }
            if let Ok(count) = count.parse::<usize>() {
                let want = count.min(expect.max(1));
                if standard != want.to_string() {
                    eprintln!("CHECKER-P6 cores: (info) case {n} under {mask:?}: a quota of {expect} over {count} CPUs reads {want}, the standard library has {standard}");
                }
            }
        }
        drop(grand);
        drop(child);
    }
    drop(top);
    eprintln!("CHECKER-P6 cores: {compared} real-cgroup (quota, mask) pairs, {skipped} cases the host would not make");
    assert!(compared >= 24, "too few real-cgroup cases ran ({compared})");
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}
