//! Which paths Kerf will read media from and write renders to.
//!
//! FFmpeg treats `http:`, `concat:`, `pipe:`, `file:` … as protocols, so a "path" an
//! agent hands to import can make it fetch a URL or read a different file than the one
//! named; and an export whose output is a file the project still needs (a source, a
//! proxy, the project database) destroys it. Both are refused here, once, for every
//! surface. Pure apart from the filesystem lookups, and unit-tested.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Whether `s` starts with something FFmpeg would read as a protocol: a scheme of two or
/// more letters, digits, `+`, `-` or `.` followed by `:`. A single letter is a Windows
/// drive (`C:\clip.mp4`), which is a path.
pub fn has_protocol_prefix(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once(':') else {
        return false;
    };
    scheme.len() >= 2
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// A path media may be imported from: absolute, not a protocol URL, and an existing
/// regular file (a pipe or a device is not media, and `pipe:` / `concat:` strings are not
/// files at all). Relative paths are refused because what they name depends on the
/// process's working directory, not on anything the caller can see.
pub fn validate_import_path(path: &Path) -> Result<()> {
    let text = path.to_string_lossy();
    if has_protocol_prefix(&text) {
        return Err(Error::InvalidArgument(format!(
            "{text} is a URL or protocol, not a file — import a file on this computer"
        )));
    }
    if !path.is_absolute() {
        return Err(Error::InvalidArgument(format!(
            "{text} is a relative path — give the full path of the file"
        )));
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err(Error::InvalidArgument(format!("{text} is not a regular file"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::InvalidArgument(format!("{text} does not exist"))),
        Err(e) => Err(Error::InvalidArgument(format!("cannot read {text}: {e}"))),
    }
}

/// `path` made absolute and symlink-free as far as it exists: a file that is not there yet
/// (an output about to be written) resolves through its directory.
pub fn canonical(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |dir| dir.join(path))
    };
    // `..` and `.` that a missing directory kept the filesystem from resolving are folded textually.
    let absolute = fold_dots(&absolute);
    // Resolve the deepest ancestor that exists and keep the missing tail as written, so
    // `/var/x/missing/out.mp4` on macOS still reads as `/private/var/x/missing/out.mp4`.
    let mut tail = Vec::new();
    let mut dir = absolute.as_path();
    while let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) {
        tail.push(name);
        if let Ok(real) = std::fs::canonicalize(parent) {
            return tail.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        dir = parent;
    }
    absolute
}

/// `path` with its `.` and `..` components resolved textually.
fn fold_dots(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether two paths are the same file: the same canonical path, the same file reached
/// through a hard link (Unix), or the same name in another case on a filesystem that
/// ignores it (Windows, macOS).
pub fn same_file(a: &Path, b: &Path) -> bool {
    let (ca, cb) = (canonical(a), canonical(b));
    if ca == cb {
        return true;
    }
    #[cfg(any(windows, target_os = "macos"))]
    if ca.to_string_lossy().eq_ignore_ascii_case(&cb.to_string_lossy()) {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (std::fs::metadata(&ca), std::fs::metadata(&cb)) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
    }
    false
}

/// How much of each end of a file [`same_content`] reads: enough that two takes of the same
/// length differ somewhere in it, little enough to be instant on a local disk.
pub const FINGERPRINT_BYTES: u64 = 1 << 20;

/// Whether two files have the same size, or `None` when either cannot be stat'ed (a file that is
/// gone is not "different", it is unknown). A stat each.
pub fn same_size(a: &Path, b: &Path) -> Option<bool> {
    let (a, b) = (std::fs::metadata(a).ok()?, std::fs::metadata(b).ok()?);
    Some(a.len() == b.len())
}

/// Whether two files are, as far as a cheap look can tell, the same bytes: the same size and the
/// same hash of their first and last [`FINGERPRINT_BYTES`] (the whole file, when it is shorter
/// than both). `None` when either cannot be read. It reads the files, so it is not for use under
/// the project lock.
pub fn same_content(a: &Path, b: &Path) -> Option<bool> {
    if !same_size(a, b)? {
        return Some(false);
    }
    Some(fingerprint(a).ok()? == fingerprint(b).ok()?)
}

/// SHA-256 of the head and the tail of `path`, with its length mixed in.
fn fingerprint(path: &Path) -> std::io::Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut hash = Sha256::new();
    hash.update(len.to_le_bytes());
    let mut chunk = Vec::new();
    file.by_ref().take(FINGERPRINT_BYTES).read_to_end(&mut chunk)?;
    hash.update(&chunk);
    if len > FINGERPRINT_BYTES {
        chunk.clear();
        let tail = FINGERPRINT_BYTES.min(len - FINGERPRINT_BYTES);
        file.seek(SeekFrom::Start(len - tail))?;
        file.by_ref().take(tail).read_to_end(&mut chunk)?;
        hash.update(&chunk);
    }
    Ok(hash.finalize().to_vec())
}

/// Whether `path` lies inside `dir` (both canonicalised).
fn within(path: &Path, dir: &Path) -> bool {
    canonical(path).starts_with(canonical(dir))
}

/// What an output must not overwrite.
#[derive(Debug, Default, Clone)]
pub struct Protected {
    /// Files the project reads: an asset's source, with what it names.
    pub files: Vec<(PathBuf, String)>,
    /// Directories nothing of the user's belongs in (the proxy / waveform cache).
    pub dirs: Vec<(PathBuf, String)>,
}

/// Refuse `output` if it is one of the `protected` files, or inside a protected directory.
pub fn check_output(output: &Path, protected: &Protected) -> Result<()> {
    if output.as_os_str().is_empty() {
        return Err(Error::InvalidArgument("the output path is empty".to_string()));
    }
    for (file, what) in &protected.files {
        if same_file(output, file) {
            return Err(Error::InvalidArgument(format!(
                "{} is {what} — exporting there would overwrite it; choose another file name",
                output.display()
            )));
        }
    }
    for (dir, what) in &protected.dirs {
        if within(output, dir) {
            return Err(Error::InvalidArgument(format!(
                "{} is inside {what} — choose a folder of your own",
                output.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kerf-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_tail_under_a_symlinked_directory_resolves_through_the_link() {
        let real = dir("real");
        let link = real.with_file_name(format!("kerf-paths-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let resolved = canonical(&link.join("missing").join("deeper").join("out.mp4"));
        assert_eq!(resolved, std::fs::canonicalize(&real).unwrap().join("missing/deeper/out.mp4"));
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&real);
    }

    #[test]
    fn protocol_prefixes_are_told_from_drive_letters() {
        for url in [
            "http://x/y.mp4",
            "https://x",
            "concat:/a|/b",
            "pipe:0",
            "file:/etc/passwd",
            "rtmp://h/s",
        ] {
            assert!(has_protocol_prefix(url), "{url}");
        }
        for path in ["C:\\clips\\a.mp4", "C:/clips/a.mp4", "/home/me/a:b.mp4", "clip.mp4", "/tmp/x"] {
            assert!(!has_protocol_prefix(path), "{path}");
        }
    }

    #[test]
    fn two_files_are_the_same_only_when_their_ends_and_size_agree() {
        let d = dir("same-content");
        let big = |name: &str, f: &dyn Fn(usize) -> u8, n: usize| {
            let p = d.join(name);
            std::fs::write(&p, (0..n).map(f).collect::<Vec<u8>>()).unwrap();
            p
        };
        let n = 3 * FINGERPRINT_BYTES as usize;
        let a = big("a.bin", &|i| (i % 251) as u8, n);
        let same = big("same.bin", &|i| (i % 251) as u8, n);
        // Differs in the middle, which a head-and-tail look does not see: documented, cheap.
        let middle = big("mid.bin", &|i| if i == n / 2 { 7 } else { (i % 251) as u8 }, n);
        let head = big("head.bin", &|i| if i == 10 { 7 } else { (i % 251) as u8 }, n);
        let tail = big("tail.bin", &|i| if i == n - 10 { 7 } else { (i % 251) as u8 }, n);
        let shorter = big("short.bin", &|i| (i % 251) as u8, n - 1);

        assert_eq!(same_content(&a, &same), Some(true));
        assert_eq!(same_content(&a, &head), Some(false));
        assert_eq!(same_content(&a, &tail), Some(false));
        assert_eq!(same_content(&a, &shorter), Some(false), "a different size is different");
        assert_eq!(same_content(&a, &middle), Some(true));
        assert_eq!(same_size(&a, &shorter), Some(false));
        assert_eq!(same_size(&a, &middle), Some(true));

        // A file shorter than one window is read whole.
        let (x, y) = (
            big("x.bin", &|i| i as u8, 100),
            big("y.bin", &|i| (i as u8).wrapping_add(1), 100),
        );
        assert_eq!(same_content(&x, &y), Some(false));
        // A missing file is unknown, not different.
        assert_eq!(same_content(&a, &d.join("gone.bin")), None);
        assert_eq!(same_size(&d.join("gone.bin"), &a), None);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn import_wants_an_absolute_path_to_a_regular_file() {
        let d = dir("import");
        let file = d.join("a.mp4");
        std::fs::write(&file, b"x").unwrap();
        assert!(validate_import_path(&file).is_ok());

        for bad in ["clip.mp4", "./clip.mp4", "http://example.com/a.mp4", "concat:/a|/b", "pipe:0"] {
            assert!(validate_import_path(Path::new(bad)).is_err(), "{bad}");
        }
        assert!(validate_import_path(&d).is_err(), "a directory is not media");
        assert!(validate_import_path(&d.join("missing.mp4")).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn an_output_may_not_land_on_a_protected_file_or_in_a_protected_directory() {
        let d = dir("output");
        let source = d.join("source.mp4");
        std::fs::write(&source, b"x").unwrap();
        let cache = d.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let protected = Protected {
            files: vec![(source.clone(), "the source of an imported clip".to_string())],
            dirs: vec![(cache.clone(), "Kerf's cache".to_string())],
        };

        assert!(check_output(&source, &protected).is_err());
        // The same file through a `..` detour.
        let detour = d.join("cache").join("..").join("source.mp4");
        assert!(check_output(&detour, &protected).is_err());
        assert!(check_output(&cache.join("proxy.mp4"), &protected).is_err());
        assert!(check_output(&d.join("out.mp4"), &protected).is_ok());
        assert!(check_output(Path::new(""), &protected).is_err());

        #[cfg(unix)]
        {
            let link = d.join("link.mp4");
            std::os::unix::fs::symlink(&source, &link).unwrap();
            assert!(
                check_output(&link, &protected).is_err(),
                "a symlink to the source is the source"
            );
            let hard = d.join("hard.mp4");
            std::fs::hard_link(&source, &hard).unwrap();
            assert!(
                check_output(&hard, &protected).is_err(),
                "a hard link to the source is the source"
            );
        }
        let _ = std::fs::remove_dir_all(d);
    }
}
