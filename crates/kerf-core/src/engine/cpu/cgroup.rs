//! The CPU quota of this process's cgroup, as `std::thread::available_parallelism` applies it on Linux, mirrored so that [`super::cores`]
//! can take the affinity and the quota from **one** read of the affinity mask instead of asking the standard library, which reads the mask
//! again by itself.
//!
//! Mirrored from `library/std/src/sys/thread/unix.rs` (`available_parallelism` and its `cgroups` module) of Rust 1.99.1 (`58cf8f9c3`;
//! 1.95, the minimum version, and `master` have the same text when this was written), behaviour for behaviour including the corners,
//! **Linux only** (this module is compiled on no other system, where `cores()` is `available_parallelism` itself), with two deliberate
//! deviations from the standard library:
//!
//! * a `/proc/self/cgroup` line whose path field is empty (`0::`, with no `/`): std slices `path[1..]` and panics; here it is the root
//!   group;
//! * a hierarchy mounted at `/` (a v1 `cpu` controller that `/proc/self/mountinfo` reports at `/`): std's walk up the ancestors
//!   (`while path.starts_with(mount)` and `PathBuf::pop`, which returns `false` on `/` and leaves it) **never ends**; here a `pop` that
//!   cannot go higher ends the walk, in both versions of the hierarchy. Kerf's `cores()` is asked on every spawn and every judgement,
//!   and nothing waits forever.
//!
//! What is mirrored:
//!
//! * the process's place in the hierarchy comes from `/proc/self/cgroup`: a v1 line whose controllers include `cpu` wins over a v2 (`0::`)
//!   line; a line without a third field forgets what was found so far;
//! * cgroup v2: `/sys/fs/cgroup/<path>/cgroup.controllers` must exist; `cpu.max` of the group and of each ancestor up to the mount, the
//!   smallest `limit / period` (an integer division: 1.5 CPUs are 1, 2.5 are 2; `max` is no limit; a period of 0 is ignored). A
//!   `cpu.max` that cannot be read is skipped, one that is empty or has no period **ends the walk** with what was found below it;
//! * cgroup v1: `cpu.cfs_quota_us` / `cpu.cfs_period_us` of the group and of each ancestor, under `/sys/fs/cgroup/cpu`, else
//!   `/sys/fs/cgroup/cpu,cpuacct`, else the mount point `/proc/self/mountinfo` names for a `cgroup` filesystem with the `cpu` option
//!   (adjusted for a bind mount of a sub-directory); a value that does not parse as an unsigned number (`-1`: no quota) is skipped;
//! * anything that cannot be read or does not make sense is **no quota** (`usize::MAX`); the caller takes `max(1, quota)`.
//!
//! The standard library may change these corners between releases; `cores_are_available_parallelism` (a test on whatever host runs it) and
//! the ignored `cores_follow_available_parallelism_under_cgroup_quotas_and_a_cpuset` hold the mirror to the toolchain in use.
//! A quota that changes while it is being read is not accounted for.

use std::borrow::Cow;
use std::ffi::OsString;
use std::fs::{exists, File};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::str::from_utf8;

#[derive(PartialEq, Clone, Copy, Debug)]
enum Version {
    V1,
    V2,
}

/// Where the files are (the system's, or a tree a test builds).
pub(super) struct Roots {
    /// `/proc/self/cgroup`.
    pub cgroup: PathBuf,
    /// `/proc/self/mountinfo`.
    pub mountinfo: PathBuf,
    /// `/sys/fs/cgroup`, where a v2 hierarchy is mounted.
    pub v2_mount: PathBuf,
    /// `/sys/fs/cgroup/cpu` and `/sys/fs/cgroup/cpu,cpuacct`, the places a v1 `cpu` controller is usually mounted.
    pub v1_mounts: [PathBuf; 2],
}

impl Roots {
    pub(super) fn system() -> Self {
        Self {
            cgroup: "/proc/self/cgroup".into(),
            mountinfo: "/proc/self/mountinfo".into(),
            v2_mount: "/sys/fs/cgroup".into(),
            v1_mounts: ["/sys/fs/cgroup/cpu".into(), "/sys/fs/cgroup/cpu,cpuacct".into()],
        }
    }
}

/// The CPU quota of this process's cgroup in whole CPUs, rounded down; `usize::MAX` if there is none or it cannot be determined.
pub(super) fn quota() -> usize {
    quota_in(&Roots::system())
}

pub(super) fn quota_in(roots: &Roots) -> usize {
    let mut buf = Vec::with_capacity(128);
    let Some(()) = File::open(&roots.cgroup)
        .ok()
        .and_then(|mut f| f.read_to_end(&mut buf).ok())
        .map(drop)
    else {
        return usize::MAX;
    };
    // Our place in the hierarchy.
    let mut found: Option<(Vec<u8>, Version)> = None;
    for line in buf.split(|&c| c == b'\n') {
        let mut fields = line.splitn(3, |&c| c == b':');
        // The second field lists the controllers for v1 and is empty for v2.
        let version = match fields.nth(1) {
            Some(b"") => Version::V2,
            Some(controllers) if from_utf8(controllers).is_ok_and(|c| c.split(',').any(|c| c == "cpu")) => Version::V1,
            _ => continue,
        };
        // A v1 line that was found trumps v2: it names its controllers.
        if found.is_some() && version == Version::V2 {
            continue;
        }
        // (A line without a path forgets what was found.)
        found = fields
            .last()
            .map(|path| (path.get(1..).unwrap_or_default().to_vec(), version));
    }
    let Some((group_path, version)) = found else {
        return usize::MAX;
    };
    let group_path = PathBuf::from(OsString::from_vec(group_path));
    match version {
        Version::V1 => quota_v1(roots, &group_path),
        Version::V2 => quota_v2(roots, &group_path),
    }
}

fn quota_v2(roots: &Roots, group_path: &Path) -> usize {
    let mut quota = usize::MAX;
    let mut path = PathBuf::with_capacity(128);
    let mut read_buf = String::with_capacity(20);
    let mount = roots.v2_mount.as_path();
    path.push(mount);
    path.push(group_path);
    path.push("cgroup.controllers");
    // Not looking at a cgroup2 hierarchy.
    if matches!(exists(&path), Err(_) | Ok(false)) {
        return usize::MAX;
    }
    path.pop();
    while path.starts_with(mount) {
        path.push("cpu.max");
        read_buf.clear();
        if File::open(&path).and_then(|mut f| f.read_to_string(&mut read_buf)).is_ok() {
            // (An empty file, or one without a period, ends the walk with the quota found so far.)
            let Some(raw) = read_buf.lines().next() else {
                return quota;
            };
            let mut raw = raw.split(' ');
            let (Some(limit), Some(period)) = (raw.next(), raw.next()) else {
                return quota;
            };
            // (A period of 0 is ignored.)
            if let (Ok(limit), Ok(period)) = (limit.parse::<usize>(), period.parse::<usize>()) {
                if let Some(cpus) = limit.checked_div(period) {
                    quota = quota.min(cpus);
                }
            }
        }
        // (The file name, then the directory; a hierarchy at `/` has nowhere higher to go: the walk ends there, where std's never does.)
        if !path.pop() || !path.pop() {
            break;
        }
    }
    quota
}

fn quota_v1(roots: &Roots, group_path: &Path) -> usize {
    let mut quota = usize::MAX;
    let mut path = PathBuf::with_capacity(128);
    let mut read_buf = String::with_capacity(20);
    // The usual places, then the mount point mountinfo names.
    for strategy in 0..3 {
        let (mount, group_path): (Cow<'_, Path>, Cow<'_, Path>) = match strategy {
            0 => (Cow::Borrowed(roots.v1_mounts[0].as_path()), Cow::Borrowed(group_path)),
            1 => (Cow::Borrowed(roots.v1_mounts[1].as_path()), Cow::Borrowed(group_path)),
            _ => match find_mountpoint(&roots.mountinfo, group_path) {
                Some((mount, trimmed)) => (Cow::Owned(PathBuf::from(mount)), Cow::Owned(trimmed)),
                None => continue,
            },
        };
        path.clear();
        path.push(mount.as_ref());
        path.push(group_path.as_ref());
        // We guessed the mount wrongly.
        if matches!(exists(&path), Err(_) | Ok(false)) {
            continue;
        }
        while path.starts_with(mount.as_ref()) {
            let mut parse_file = |name| {
                path.push(name);
                read_buf.clear();
                let f = File::open(&path);
                path.pop();
                f.ok()?.read_to_string(&mut read_buf).ok()?;
                read_buf.trim().parse::<usize>().ok()
            };
            let limit = parse_file("cpu.cfs_quota_us");
            let period = parse_file("cpu.cfs_period_us");
            if let Some(cpus) = limit.zip(period).and_then(|(limit, period)| limit.checked_div(period)) {
                quota = quota.min(cpus);
            }
            // (A mount at `/` has no ancestor above it: the walk ends, where std's never does.)
            if !path.pop() {
                break;
            }
        }
        break;
    }
    quota
}

/// The mount point of a v1 `cgroup` filesystem with the `cpu` option, and `group_path` below what that mount shows (a bind mount of a
/// sub-directory is skipped over).
fn find_mountpoint(mountinfo: &Path, group_path: &Path) -> Option<(String, PathBuf)> {
    let mut reader = BufReader::new(File::open(mountinfo).ok()?);
    let mut line = String::with_capacity(256);
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let line = line.trim();
        let mut items = line.split(' ');
        let sub_path = items.nth(3)?;
        let mount_point = items.next()?;
        let mount_opts = items.next_back()?;
        let filesystem_type = items.nth_back(1)?;
        if filesystem_type != "cgroup" || !mount_opts.split(',').any(|opt| opt == "cpu") {
            continue;
        }
        let sub_path = Path::new(sub_path).strip_prefix("/").ok()?;
        if !group_path.starts_with(sub_path) {
            // A bind mount of a sub-directory that does not hold our cgroup.
            continue;
        }
        let trimmed = group_path.strip_prefix(sub_path).ok()?;
        return Some((mount_point.to_owned(), trimmed.to_path_buf()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A tree of files laid out like the system's, in a directory of its own.
    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("kerf-cgroup-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn put(&self, rel: &str, text: &str) {
            let path = self.root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn roots(&self) -> Roots {
            Roots {
                cgroup: self.root.join("proc/self/cgroup"),
                mountinfo: self.root.join("proc/self/mountinfo"),
                v2_mount: self.root.join("sys/fs/cgroup"),
                v1_mounts: [
                    self.root.join("sys/fs/cgroup/cpu"),
                    self.root.join("sys/fs/cgroup/cpu,cpuacct"),
                ],
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_v2_quota_is_the_smallest_limit_over_period_of_the_group_and_its_ancestors_rounded_down() {
        let t = Tree::new("v2");
        t.put("proc/self/cgroup", "0::/a/b\n");
        t.put("sys/fs/cgroup/a/b/cgroup.controllers", "cpu\n");
        // 1.5 CPUs are 1.
        t.put("sys/fs/cgroup/a/b/cpu.max", "150000 100000\n");
        t.put("sys/fs/cgroup/a/cpu.max", "800000 100000\n");
        assert_eq!(quota_in(&t.roots()), 1);
        // 2.5 are 2, and an ancestor below the group's limit wins.
        t.put("sys/fs/cgroup/a/b/cpu.max", "250000 100000\n");
        assert_eq!(quota_in(&t.roots()), 2);
        t.put("sys/fs/cgroup/a/cpu.max", "100000 100000\n");
        assert_eq!(quota_in(&t.roots()), 1);
        // `max` is no limit at that level, and the ancestor's is still taken.
        t.put("sys/fs/cgroup/a/b/cpu.max", "max 100000\n");
        t.put("sys/fs/cgroup/a/cpu.max", "300000 100000\n");
        assert_eq!(quota_in(&t.roots()), 3);
        // A period of 0 is ignored; a file that is not there is skipped.
        t.put("sys/fs/cgroup/a/b/cpu.max", "100000 0\n");
        assert_eq!(quota_in(&t.roots()), 3);
        fs::remove_file(t.root.join("sys/fs/cgroup/a/b/cpu.max")).unwrap();
        assert_eq!(quota_in(&t.roots()), 3);
        // A file with no period ends the walk with what was found below it (nothing: no quota), as the standard library's does.
        t.put("sys/fs/cgroup/a/b/cpu.max", "max\n");
        assert_eq!(quota_in(&t.roots()), usize::MAX);
        // ... and with one found below it, that one.
        t.put("sys/fs/cgroup/a/b/cpu.max", "200000 100000\n");
        t.put("sys/fs/cgroup/a/cpu.max", "");
        assert_eq!(quota_in(&t.roots()), 2);
        // Not a cgroup2 hierarchy: no quota.
        fs::remove_file(t.root.join("sys/fs/cgroup/a/b/cgroup.controllers")).unwrap();
        assert_eq!(quota_in(&t.roots()), usize::MAX);
    }

    #[test]
    fn a_v1_quota_is_the_smallest_cfs_quota_over_period_and_a_missing_quota_is_none() {
        let t = Tree::new("v1");
        t.put("proc/self/cgroup", "4:cpu,cpuacct:/g/h\n3:memory:/x\n0::/y\n");
        let cpu = |rel: &str| format!("sys/fs/cgroup/cpu/{rel}");
        t.put(&cpu("g/h/cpu.cfs_quota_us"), "250000\n");
        t.put(&cpu("g/h/cpu.cfs_period_us"), "100000\n");
        t.put(&cpu("g/cpu.cfs_quota_us"), "-1\n");
        t.put(&cpu("g/cpu.cfs_period_us"), "100000\n");
        assert_eq!(quota_in(&t.roots()), 2);
        t.put(&cpu("g/cpu.cfs_quota_us"), "100000\n");
        assert_eq!(quota_in(&t.roots()), 1);
        // -1 is no limit.
        t.put(&cpu("g/h/cpu.cfs_quota_us"), "-1\n");
        t.put(&cpu("g/cpu.cfs_quota_us"), "-1\n");
        assert_eq!(quota_in(&t.roots()), usize::MAX);
        // The v1 line trumps the v2 one even when it comes later or earlier (a v2 line is only taken if nothing else was found).
        t.put("proc/self/cgroup", "0::/y\n4:cpu:/g/h\n");
        t.put(&cpu("g/h/cpu.cfs_quota_us"), "300000\n");
        assert_eq!(quota_in(&t.roots()), 3);
    }

    #[test]
    fn a_v1_cpu_controller_mounted_elsewhere_is_found_through_mountinfo_and_a_bind_mount_is_skipped_over() {
        let t = Tree::new("mountinfo");
        t.put("proc/self/cgroup", "4:cpu,cpuacct:/sub/g\n");
        let mount = t.root.join("mnt/cg");
        t.put(
            "proc/self/mountinfo",
            &format!(
                "20 1 0:5 / /proc rw - proc proc rw\n36 25 0:30 /sub {} rw,nosuid - cgroup cgroup rw,cpu,cpuacct\n",
                mount.display()
            ),
        );
        // The bind mount shows /sub as its root: our group is `g` below it.
        t.put("mnt/cg/g/cpu.cfs_quota_us", "200000\n");
        t.put("mnt/cg/g/cpu.cfs_period_us", "100000\n");
        assert_eq!(quota_in(&t.roots()), 2);
        // A mount of another sub-directory holds no cgroup of ours.
        t.put(
            "proc/self/mountinfo",
            &format!("36 25 0:30 /other {} rw - cgroup cgroup rw,cpu,cpuacct\n", mount.display()),
        );
        assert_eq!(quota_in(&t.roots()), usize::MAX);
    }

    /// A v1 `cpu` controller mounted at `/` (mountinfo `36 25 0:30 / / rw - cgroup cgroup rw,cpu`) and a process in its root (`4:cpu:/`): the
    /// walk up the ancestors has nowhere higher than `/` to go, and std's never ends. This ends, with no quota (there is no such file
    /// at `/`), in bounded time.
    #[test]
    fn a_v1_hierarchy_mounted_at_the_root_ends_the_walk_instead_of_hanging() {
        let t = Tree::new("rootmount");
        t.put("proc/self/cgroup", "4:cpu:/\n");
        t.put("proc/self/mountinfo", "36 25 0:30 / / rw - cgroup cgroup rw,cpu\n");
        let roots = t.roots();
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(quota_in(&roots));
        });
        let quota = result
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("the walk up a hierarchy mounted at / ends");
        assert_eq!(quota, usize::MAX);
    }

    #[test]
    fn nothing_readable_is_no_quota() {
        let t = Tree::new("none");
        assert_eq!(quota_in(&t.roots()), usize::MAX, "no /proc/self/cgroup");
        t.put("proc/self/cgroup", "12:devices:/\n");
        assert_eq!(quota_in(&t.roots()), usize::MAX, "no cpu line");
        t.put("proc/self/cgroup", "0::/\n");
        assert_eq!(quota_in(&t.roots()), usize::MAX, "no cgroup2 mount");
    }
}
