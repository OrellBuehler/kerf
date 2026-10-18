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
    match (absolute.parent(), absolute.file_name()) {
        (Some(dir), Some(name)) => std::fs::canonicalize(dir).map_or(absolute.clone(), |dir| dir.join(name)),
        _ => absolute,
    }
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
