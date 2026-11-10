use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// `text` at `path`, for a fixture the tests of one binary share. `std::fs::write` truncates before it writes, and a reader
/// that opens the file in between (the plan's LUT read, FFmpeg's) finds it empty: a `.cube` read that way is not a usable LUT,
/// so one renderer leaves it out and the other does not. A file that already holds `text` is left alone, else it is written
/// beside `path` and renamed over it, one test at a time.
#[allow(dead_code)]
pub fn write_fixture(path: &Path, text: &str) {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if std::fs::read_to_string(path).is_ok_and(|have| have == text) {
        return;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

/// How long `ffmpeg -version` may take before the key goes without it.
const VERSION_LIMIT: Duration = Duration::from_secs(30);
/// How long one of the suites' FFmpeg runs (synthesizing a clip, rendering a still) may take: far past the slowest on a loaded machine.
const FFMPEG_LIMIT: Duration = Duration::from_secs(900);

/// `cmd`'s output, the child killed (and waited for) if it has not exited after `limit`: a child that hangs ends the case with an error,
/// not the run. Its pipes are read as it runs, so a long output does not stop it.
#[allow(dead_code)]
pub fn output_within(cmd: &mut Command, limit: Duration) -> std::io::Result<Output> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let read = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    };
    let stdout = read(Box::new(child.stdout.take().expect("piped")));
    let stderr = read(Box::new(child.stderr.take().expect("piped")));
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let (stdout, stderr) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
    match status {
        Some(status) => Ok(Output { status, stdout, stderr }),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("no answer within {limit:?}; killed"),
        )),
    }
}

/// A short stable key of the FFmpeg these tests drive: its path, its `-version` text (the release and the configuration it was built
/// with, asked for at most [`VERSION_LIMIT`]) and, for a binary named by a path, its size and modification time. Two builds, or one
/// build behind a wrapper that adds flags (`-cpuflags 0`), get two keys; the same binary always the same one.
#[allow(dead_code)]
pub fn build_key() -> &'static str {
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(|| {
        let path = kerf_core::ffmpeg_path();
        let mut text = path.clone().into_bytes();
        text.push(0);
        if let Ok(out) = output_within(Command::new(&path).arg("-version").stdin(Stdio::null()), VERSION_LIMIT) {
            text.extend_from_slice(&out.stdout);
        }
        if let Ok(meta) = std::fs::metadata(&path) {
            let modified = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            text.extend_from_slice(format!("\0{}\0{modified}", meta.len()).as_bytes());
        }
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in text {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        format!("{:08x}", h & 0xffff_ffff)
    })
}

/// Where a suite keeps the media it synthesizes (and the stills FFmpeg renders to compare with): `name` under the target's temp
/// directory, **kept apart per FFmpeg build** ([`build_key`]). The files are made by whichever FFmpeg is running (a tag, a container
/// field or a default differs between releases), so two runs of different builds in one directory rewrote each other's files under each
/// other's feet. The directory is not made: that is [`media_dir`], and a case that asks whether something else made it (an export's
/// parts) asks of this path.
#[allow(dead_code)]
pub fn media_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", build_key()))
}

/// [`media_path`], made if it is not there.
#[allow(dead_code)]
pub fn media_dir(name: &str) -> PathBuf {
    let dir = media_path(name);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    dir
}

/// `bytes` at `path`, written beside it and renamed over it: a process that reads the file while another one makes it again finds the old
/// file or the new, never a truncated one (`std::fs::write` empties it first).
#[allow(dead_code)]
pub fn write_atomic(path: &Path, bytes: &[u8]) {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(
        "{name}.{}.{}.part",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, bytes).unwrap_or_else(|e| panic!("write {}: {e}", tmp.display()));
    std::fs::rename(&tmp, path).unwrap_or_else(|e| panic!("rename {} over {}: {e}", tmp.display(), path.display()));
}

/// A shell script at `path` that can be run: written under a name of its own, made executable, then renamed over `path`. Whoever runs
/// the script finds all of it, and `path` is never open for writing when it is run (a script written in place can be `ETXTBSY` to a
/// process that runs it while another still holds it open).
#[cfg(unix)]
#[allow(dead_code)]
pub fn write_script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(
        "{name}.{}.{}.part",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, text).unwrap_or_else(|e| panic!("write {}: {e}", tmp.display()));
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).expect("make the script executable");
    std::fs::rename(&tmp, path).unwrap_or_else(|e| panic!("rename {} over {}: {e}", tmp.display(), path.display()));
}

/// Run the FFmpeg these tests drive with `args` (`-y` and a quiet log are added) and return its stdout. An output **file** (the last
/// argument, when it is a path with an extension) is written beside its name and renamed over it, so a process reading the file while
/// another one makes it again finds the old file or the new one, never half of one; the file made on the way is removed when FFmpeg
/// fails or does not answer within [`FFMPEG_LIMIT`] (it is killed).
#[allow(dead_code)]
pub fn ffmpeg(args: &[&str]) -> Vec<u8> {
    static PARTS: AtomicUsize = AtomicUsize::new(0);
    let output = args.last().copied().filter(|a| {
        !a.starts_with('-')
            && !a.starts_with("pipe:")
            && !a.contains('%')
            && !a.contains("://")
            && Path::new(a).extension().is_some()
    });
    let part = output.map(|out| {
        let out = Path::new(out);
        let stem = out.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let ext = out.extension().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        out.with_file_name(format!(
            "{stem}.{}.{}.part.{ext}",
            std::process::id(),
            PARTS.fetch_add(1, Ordering::Relaxed)
        ))
    });
    let mut run: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    if let Some(part) = &part {
        *run.last_mut().expect("an output") = part.to_string_lossy().into_owned();
    }
    let result = output_within(
        Command::new(kerf_core::ffmpeg_path())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(&run)
            .stdin(Stdio::null()),
        FFMPEG_LIMIT,
    );
    let out = match result {
        Ok(out) if out.status.success() => out,
        failed => {
            if let Some(part) = &part {
                let _ = std::fs::remove_file(part);
            }
            match failed {
                Ok(out) => panic!("ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr)),
                Err(e) => panic!("ffmpeg {args:?}: {e}"),
            }
        }
    };
    if let (Some(part), Some(output)) = (part.filter(|p| p.exists()), output) {
        // (Where another process holds the file open and the rename is refused, that process's file is the same one.)
        if std::fs::rename(&part, output).is_err() {
            let _ = std::fs::remove_file(&part);
            assert!(Path::new(output).exists(), "ffmpeg {args:?}: no output at {output}");
        }
    }
    out.stdout
}
