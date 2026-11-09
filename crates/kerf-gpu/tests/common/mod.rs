use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// `text` at `path`, for a fixture the tests of one binary share. `std::fs::write` truncates before it writes, and a reader
/// that opens the file in between (the plan's LUT read, FFmpeg's) finds it empty: a `.cube` read that way is not a usable LUT,
/// so one renderer leaves it out and the other does not. A file that already holds `text` is left alone, else it is written
/// beside `path` and renamed over it, one test at a time.
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

/// A short stable key of the FFmpeg these tests drive: its path, its `-version` text (the release and the configuration it was built
/// with) and, for a binary named by a path, its size and modification time. Two builds, or one build behind a wrapper that adds flags
/// (`-cpuflags 0`), get two keys; the same binary always the same one.
#[allow(dead_code)]
pub fn build_key() -> &'static str {
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(|| {
        let path = kerf_core::ffmpeg_path();
        let mut text = path.clone().into_bytes();
        text.push(0);
        if let Ok(out) = Command::new(&path)
            .arg("-version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
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

/// The directory a suite keeps the media it synthesizes (and the stills FFmpeg renders to compare with) in: `name` under the target's
/// temp directory, **kept apart per FFmpeg build** ([`build_key`]). The files are made by whichever FFmpeg is running (a tag, a
/// container field or a default differs between releases), so two runs of different builds in one directory rewrote each other's
/// files under each other's feet. Created if it is not there.
#[allow(dead_code)]
pub fn media_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    dir
}

/// Run the FFmpeg these tests drive with `args` (`-y` and a quiet log are added) and return its stdout. An output **file** (the last
/// argument, when it is a path with an extension) is written beside its name and renamed over it, so a process reading the file while
/// another one makes it again finds the old file or the new one, never half of one.
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
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(&run)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    if let (Some(part), Some(output)) = (part.filter(|p| p.exists()), output) {
        // (Where another process holds the file open and the rename is refused, that process's file is the same one.)
        if std::fs::rename(&part, output).is_err() {
            let _ = std::fs::remove_file(&part);
            assert!(Path::new(output).exists(), "ffmpeg {args:?}: no output at {output}");
        }
    }
    out.stdout
}
