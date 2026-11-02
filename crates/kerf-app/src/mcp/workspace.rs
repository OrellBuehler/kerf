//! `kerf-app --headless --workspace DIR`: every file an MCP client names lies inside DIR.
//!
//! For a server that remote agents reach, which have no business with the rest of its disk. A path
//! is resolved against the workspace when relative, folded (`.` and `..`) and checked to lie inside
//! it **before** the filesystem is asked anything, then checked again with every symbolic link
//! followed (the deepest part that exists, for an output not written yet). What a tool then reads or
//! writes is that resolved path. Without a workspace nothing here runs.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use rmcp::ErrorData as McpError;

/// The directory a headless server is confined to.
#[derive(Debug)]
pub(crate) struct Workspace {
    /// The directory with every symbolic link resolved: what every path must lie in.
    root: String,
    /// The directory as it was named, when that is another spelling of it (`/var/x` for
    /// `/private/var/x`): an absolute path given in that spelling is let through to the real check.
    named: Option<String>,
}

impl Workspace {
    /// The workspace at `dir` (absolute): an existing directory.
    pub(crate) fn open(dir: &Path) -> Result<Self, String> {
        let shown = dir.display();
        match std::fs::metadata(dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(format!("--workspace {shown} is not a directory")),
            Err(e) => return Err(format!("--workspace {shown}: {e}")),
        }
        let real = std::fs::canonicalize(dir).map_err(|e| format!("--workspace {shown}: {e}"))?;
        let root = real
            .to_str()
            .map(|r| plain(r).to_string())
            .ok_or_else(|| format!("--workspace {shown}: the path is not valid UTF-8"))?;
        // A filesystem root (or a share Windows names only in its verbatim spelling) is no confinement.
        if !absolute_parts(&root, cfg!(windows)).is_some_and(|(_, parts)| !parts.is_empty()) {
            return Err(format!(
                "--workspace {shown} must be a directory below the root of a local drive, not {root}"
            ));
        }
        let named = dir.to_str().and_then(|d| inside(d, "", cfg!(windows))).filter(|n| *n != root);
        Ok(Self { root, named })
    }

    /// Whether `path` (absolute; it need not exist) lies in the workspace once its links are
    /// followed: what Kerf's own files must not.
    pub(crate) fn holds(&self, path: &Path) -> bool {
        let real = kerf_core::paths::canonical(path);
        PathBuf::from(plain(&real.to_string_lossy())).starts_with(&self.root)
    }

    /// A speech model a client may select: one of the named models, never a model file (the engine
    /// reads a name with a separator or `.bin` as a path, and its status says whether that file exists).
    pub(crate) fn check_speech_model(&self, name: &str) -> Result<(), String> {
        let names = kerf_core::speech_model_names();
        if names.contains(&name) {
            return Ok(());
        }
        Err(format!(
            "speech model {name:?}: this server is confined to the workspace ({}) and takes only a model name ({})",
            self.root,
            names.join(", ")
        ))
    }

    /// Refuse a project that names a file outside the workspace: an asset's file or capture
    /// files, a LUT anywhere in its history, a speech model file. Kerf's own generated media
    /// (voiceovers, music, sound effects, stitches and stems) is where Kerf put it. Reads the
    /// disk: call it with no lock held, before the project is taken in.
    pub(crate) fn check_project(&self, project: &kerf_core::Project) -> Result<(), String> {
        let refused = |what: &str, why: String| format!("the project cannot be opened here: {what} {why}");
        let kerf_dirs = kerf_media_dirs();
        for file in project.referenced_files().map_err(|e| e.to_string())? {
            if is_relative(&file, cfg!(windows)) {
                return Err(refused(&format!("{file:?}"), "is a relative path".to_string()));
            }
            let generated = kerf_dirs.iter().any(|dir| within(dir, &file));
            if !generated {
                self.resolve(&file)
                    .map_err(|e| refused("it names a file outside the workspace:", e))?;
            }
        }
        let model = project.meta(crate::SPEECH_MODEL_KEY).map_err(|e| e.to_string())?;
        if let Some(model) = model.filter(|m| !m.trim().is_empty()) {
            self.check_speech_model(model.trim()).map_err(|e| refused("its", e))?;
        }
        Ok(())
    }

    /// The directory, symbolic links resolved.
    pub(crate) fn root(&self) -> &str {
        &self.root
    }

    /// The file `path` a client named, as this server reads or writes it: relative to the workspace
    /// or absolute, and inside it once every symbolic link is followed.
    pub(crate) fn resolve(&self, path: &str) -> Result<String, String> {
        // A network share is refused before any filesystem call, as everywhere an agent names a file.
        crate::check_input_path(path, cfg!(windows))?;
        if looks_like_url(path, cfg!(windows)) {
            return Err(format!(
                "{path:?} is a URL or protocol, not a file: name a file in the workspace ({})",
                self.root
            ));
        }
        let folded = std::iter::once(&self.root)
            .chain(&self.named)
            .find_map(|root| inside(root, path, cfg!(windows)))
            .ok_or_else(|| self.outside(path))?;
        let real = self.confine(Path::new(&folded), path)?;
        real.to_str()
            .map(|r| plain(r).to_string())
            .ok_or_else(|| format!("{path:?} resolves to a path that is not valid UTF-8"))
    }

    /// `path` with its deepest existing part's symbolic links followed, refused unless that lies
    /// in the workspace. A part that is there but cannot be followed (a link that leads nowhere,
    /// which a write would follow anywhere) is refused.
    fn confine(&self, path: &Path, asked: &str) -> Result<PathBuf, String> {
        follow(Path::new(&self.root), path).map_err(|escape| match escape {
            Escape::Outside => self.outside(asked),
            Escape::Unfollowable(at) => format!(
                "{asked:?} cannot be followed inside the workspace ({}): {} is a link that leads nowhere or \
                 cannot be read",
                self.root,
                at.display()
            ),
        })
    }

    fn outside(&self, path: &str) -> String {
        format!(
            "{path:?} is outside the workspace ({}): this server reads and writes files only inside it. Give a path \
             relative to the workspace (e.g. \"media/clip.mp4\") or an absolute one inside it",
            self.root
        )
    }

    /// A tool's answer with every absolute path inside the workspace that starts a JSON string shown
    /// relative to it, so the client can pass it back. The text is not re-serialised: the
    /// workspace's spelling (JSON-escaped, with a trailing separator) is cut after the quote.
    pub(crate) fn relative_in(&self, text: String) -> String {
        let mut text = text;
        for root in std::iter::once(&self.root).chain(&self.named) {
            if let Cow::Owned(changed) = relative_json(&text, root, cfg!(windows)) {
                text = changed;
            }
        }
        text
    }
}

/// Why a path does not stay in a directory once its links are followed.
enum Escape {
    Outside,
    /// A part that is there but cannot be followed: a link that leads nowhere, which a write
    /// would follow anywhere.
    Unfollowable(PathBuf),
}

/// `path` with its deepest existing part's symbolic links followed, refused unless that lies in
/// `root` (canonical).
fn follow(root: &Path, path: &Path) -> Result<PathBuf, Escape> {
    let mut tail = Vec::new();
    let mut at = path;
    loop {
        match std::fs::canonicalize(at) {
            Ok(real) => {
                let real = PathBuf::from(plain(&real.to_string_lossy()));
                if !real.starts_with(root) {
                    return Err(Escape::Outside);
                }
                return Ok(tail.iter().rev().fold(real, |acc, part| acc.join(part)));
            }
            Err(_) if std::fs::symlink_metadata(at).is_ok() => return Err(Escape::Unfollowable(at.to_path_buf())),
            Err(_) => match (at.parent(), at.file_name()) {
                (Some(parent), Some(name)) => {
                    tail.push(name);
                    at = parent;
                }
                _ => return Err(Escape::Outside),
            },
        }
    }
}

/// Where Kerf writes the media it makes itself, which a project's assets may name.
fn kerf_media_dirs() -> Vec<PathBuf> {
    let data = dirs::data_dir().map(|d| d.join("kerf"));
    let generated = ["music", "voiceovers", "sfx"]
        .into_iter()
        .filter_map(|name| Some(data.as_ref()?.join(name)));
    generated.chain(dirs::cache_dir().map(|d| d.join("kerf"))).collect()
}

/// Whether `file` (absolute) lies in `dir`, by its spelling and once its links are followed.
fn within(dir: &Path, file: &str) -> bool {
    let Some(dir_text) = dir.to_str() else { return false };
    let Some(folded) = inside(dir_text, file, cfg!(windows)) else {
        return false;
    };
    match std::fs::canonicalize(dir) {
        Ok(real) => follow(&PathBuf::from(plain(&real.to_string_lossy())), Path::new(&folded)).is_ok(),
        // Nothing is there, so nothing in it leads anywhere.
        Err(_) => true,
    }
}

/// Schemes FFmpeg (or a URL) reads as something other than a file in the workspace.
const URL_SCHEMES: &[&str] = &[
    "async",
    "cache",
    "concat",
    "concatf",
    "crypto",
    "data",
    "fd",
    "file",
    "ftp",
    "gopher",
    "gophers",
    "hls",
    "http",
    "https",
    "httpproxy",
    "icecast",
    "ipfs",
    "ipns",
    "md5",
    "mmsh",
    "mmst",
    "pipe",
    "prompeg",
    "rtmp",
    "rtmpe",
    "rtmps",
    "rtmpt",
    "rtmpte",
    "rtmpts",
    "rtp",
    "rtsp",
    "sctp",
    "sftp",
    "smb",
    "srt",
    "srtp",
    "subfile",
    "tcp",
    "tee",
    "tls",
    "udp",
    "udplite",
    "unix",
    "zmq",
];

/// Whether `path` reads as a URL or an FFmpeg protocol rather than a file. On Windows any
/// `scheme:` prefix of two or more characters (a file name holds no `:` there); on Unix, where
/// `take1:final.mp4` is a file name, a scheme followed by `//` or one FFmpeg knows.
fn looks_like_url(path: &str, windows: bool) -> bool {
    if windows {
        return kerf_core::paths::has_protocol_prefix(path);
    }
    let Some((scheme, rest)) = path.split_once(':') else {
        return false;
    };
    let is_scheme = scheme.len() >= 2
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    is_scheme && (rest.starts_with("//") || URL_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()))
}

/// What playlist or reference format a media file to import is, by its name and first bytes:
/// FFmpeg follows the files such a one names (an HLS playlist's segments, a DASH manifest's, a
/// concat list's, an SDP session's streams) wherever they are, and probing it already does. So
/// with a workspace one is refused before FFmpeg sees it.
pub(crate) fn reference_format(name: &str, head: &[u8]) -> Option<&'static str> {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let by_name = match ext.as_str() {
        "m3u8" | "m3u" => Some("an HLS / M3U playlist"),
        "mpd" => Some("a DASH manifest"),
        "ffconcat" | "concat" => Some("an FFmpeg concat list"),
        "sdp" => Some("an SDP session description"),
        "avs" | "vpy" => Some("a frameserver script"),
        "xspf" | "pls" => Some("a playlist"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    let text = String::from_utf8_lossy(head);
    let start = text.trim_start_matches('\u{feff}').trim_start();
    let lower = start.to_ascii_lowercase();
    if lower.starts_with("#extm3u") {
        Some("an HLS / M3U playlist")
    } else if lower.starts_with("ffconcat version") {
        Some("an FFmpeg concat list")
    } else if lower.starts_with("v=0") || lower.lines().any(|l| l.starts_with("c=in ip")) {
        Some("an SDP session description")
    } else if lower.contains("<mpd") {
        Some("a DASH manifest")
    } else if lower.contains("<compositionplaylist") {
        Some("an IMF composition playlist")
    } else {
        None
    }
}

/// Refuse importing `path` (resolved in the workspace) when it is a playlist or reference format
/// ([`reference_format`]). Reads the file's first 4 KiB: for the blocking pool, before FFmpeg runs.
pub(crate) fn refuse_reference_media(path: &str) -> Result<(), String> {
    use std::io::Read;
    let mut head = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(4096).read_to_end(&mut head);
    }
    match reference_format(path, &head) {
        Some(kind) => Err(format!(
            "{path:?} is {kind}, which makes FFmpeg read the files it names wherever they are; this server is \
             confined to its workspace, so import the media files themselves"
        )),
        None => Ok(()),
    }
}

/// A path the client named, as the tool uses it: unchanged without a workspace, else resolved in it.
pub(crate) fn confine(workspace: Option<&Workspace>, path: &str) -> Result<String, McpError> {
    match workspace {
        None => Ok(path.to_string()),
        Some(ws) => ws.resolve(path).map_err(|e| McpError::invalid_params(e, None)),
    }
}

/// `\\?\C:\…`, the verbatim spelling Windows' canonical paths come in, as `C:\…`.
fn plain(path: &str) -> &str {
    let b = path.as_bytes();
    let drive = b.len() >= 7 && &b[..4] == b"\\\\?\\" && b[4].is_ascii_alphabetic() && b[5] == b':' && b[6] == b'\\';
    if drive {
        &path[4..]
    } else {
        path
    }
}

fn is_separator(c: char, windows: bool) -> bool {
    c == '/' || (windows && c == '\\')
}

/// An absolute path's root (`""` on Unix, the drive on Windows) and its parts, `.` and `..`
/// folded. `None` for a path that is not absolute in a form a workspace takes (on Windows a drive
/// path: not `\dir` or `C:dir`, whose drive is the process's), climbs above its root, or holds a
/// part Windows would read as something else (`a:stream`, a trailing dot or space).
fn absolute_parts(path: &str, windows: bool) -> Option<(&str, Vec<&str>)> {
    let path = if windows { plain_any(path) } else { path };
    let (root, rest) = if windows {
        let b = path.as_bytes();
        let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
        if !drive {
            return None;
        }
        (&path[..2], &path[3..])
    } else {
        ("", path.strip_prefix('/')?)
    };
    let mut parts = Vec::new();
    for part in rest.split(|c| is_separator(c, windows)) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ if windows && (part.contains(':') || part.ends_with(['.', ' '])) => return None,
            _ => parts.push(part),
        }
    }
    Some((root, parts))
}

/// [`plain`] for either separator: `\\?\C:\` and `//?/C:/`.
fn plain_any(path: &str) -> &str {
    let b = path.as_bytes();
    let sep = |i: usize| matches!(b.get(i), Some(b'\\' | b'/'));
    let verbatim = sep(0) && sep(1) && b.get(2) == Some(&b'?') && sep(3);
    let drive = b.get(4).is_some_and(u8::is_ascii_alphabetic) && b.get(5) == Some(&b':') && sep(6);
    if verbatim && drive {
        &path[4..]
    } else {
        path
    }
}

fn is_relative(path: &str, windows: bool) -> bool {
    let mut chars = path.chars();
    match chars.next() {
        None => true,
        Some(c) if is_separator(c, windows) => false,
        Some(c) => !(windows && c.is_ascii_alphabetic() && chars.next() == Some(':')),
    }
}

fn same_part(a: &str, b: &str, windows: bool) -> bool {
    if windows {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// `path` — relative to `root`, or absolute — as the absolute path it names, `.` and `..` folded,
/// when that lies in `root` (an absolute path); `None` when it does not. Pure: what is checked
/// before the filesystem is asked anything.
fn inside(root: &str, path: &str, windows: bool) -> Option<String> {
    let (root_drive, root_parts) = absolute_parts(root, windows)?;
    let sep = if windows { "\\" } else { "/" };
    let full = if is_relative(path, windows) {
        Cow::Owned(format!("{root}{sep}{path}"))
    } else {
        Cow::Borrowed(path)
    };
    let (drive, parts) = absolute_parts(&full, windows)?;
    let within = same_part(drive, root_drive, windows)
        && parts.len() >= root_parts.len()
        && root_parts.iter().zip(&parts).all(|(r, p)| same_part(r, p, windows));
    within.then(|| {
        let drive = if windows { drive.to_string() } else { String::new() };
        format!("{drive}{sep}{}", parts.join(sep))
    })
}

/// [`Workspace::relative_in`] for one spelling of the root (pure).
fn relative_json<'a>(text: &'a str, root: &str, windows: bool) -> Cow<'a, str> {
    let sep = if windows { '\\' } else { '/' };
    let prefix = format!("{}{sep}", root.trim_end_matches(|c| is_separator(c, windows)));
    let Ok(quoted) = serde_json::to_string(&prefix) else {
        return Cow::Borrowed(text);
    };
    let needle = &quoted[..quoted.len() - 1];
    if text.contains(needle) {
        Cow::Owned(text.replace(needle, "\""))
    } else {
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-workspace-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        PathBuf::from(plain(std::fs::canonicalize(dir).unwrap().to_str().unwrap()))
    }

    #[test]
    fn a_relative_path_lands_in_the_workspace_and_dots_are_folded() {
        for (path, folded) in [
            ("clip.mp4", Some("/ws/clip.mp4")),
            ("media/./clip.mp4", Some("/ws/media/clip.mp4")),
            ("media/../clip.mp4", Some("/ws/clip.mp4")),
            ("", Some("/ws")),
            ("..", None),
            ("../other/clip.mp4", None),
            ("media/../../etc/passwd", None),
            ("/ws/clip.mp4", Some("/ws/clip.mp4")),
            ("//ws//media/clip.mp4", Some("/ws/media/clip.mp4")),
            ("/ws/../etc/passwd", None),
            ("/wsx/clip.mp4", None),
            ("/etc/passwd", None),
            ("/../ws/clip.mp4", None),
        ] {
            assert_eq!(inside("/ws", path, false).as_deref(), folded, "{path}");
        }
        assert_eq!(inside("/", "etc/passwd", false).as_deref(), Some("/etc/passwd"));
        // A backslash is a file name's character on Unix, not a way out.
        assert_eq!(inside("/ws", "..\\x", false).as_deref(), Some("/ws/..\\x"));
    }

    #[test]
    fn windows_spellings_are_read_as_windows_reads_them() {
        let root = "C:\\Work\\ws";
        for (path, folded) in [
            ("clip.mp4", Some("C:\\Work\\ws\\clip.mp4")),
            ("media/clip.mp4", Some("C:\\Work\\ws\\media\\clip.mp4")),
            ("media\\..\\clip.mp4", Some("C:\\Work\\ws\\clip.mp4")),
            ("..\\clip.mp4", None),
            ("C:\\Work\\ws\\clip.mp4", Some("C:\\Work\\ws\\clip.mp4")),
            ("c:/work/WS/clip.mp4", Some("c:\\work\\WS\\clip.mp4")),
            ("\\\\?\\C:\\Work\\ws\\clip.mp4", Some("C:\\Work\\ws\\clip.mp4")),
            ("C:\\Work\\wsx\\clip.mp4", None),
            ("D:\\Work\\ws\\clip.mp4", None),
            ("C:\\Work\\ws\\..\\other.mp4", None),
            // The drive's own current directory, the current drive's root: not the workspace's.
            ("C:clip.mp4", None),
            ("\\Work\\ws\\clip.mp4", None),
            ("/Work/ws/clip.mp4", None),
            // An alternate data stream, and the trailing dots and spaces Windows drops.
            ("clip.mp4:hidden", None),
            ("media.\\clip.mp4", None),
            ("media \\clip.mp4", None),
            ("...\\clip.mp4", None),
        ] {
            assert_eq!(inside(root, path, true).as_deref(), folded, "{path}");
        }
        assert_eq!(plain("\\\\?\\C:\\Work\\ws"), "C:\\Work\\ws");
        assert_eq!(plain("\\\\?\\UNC\\host\\share"), "\\\\?\\UNC\\host\\share");
        assert_eq!(plain("/ws"), "/ws");
    }

    #[test]
    fn answers_show_paths_in_the_workspace_relative_to_it() {
        let text = r#"{"path":"/ws/media/a.mp4","workspace":"/ws","other":"/wsx/b.mp4","note":"see \"/ws/c.srt\""}"#;
        assert_eq!(
            relative_json(text, "/ws", false),
            r#"{"path":"media/a.mp4","workspace":"/ws","other":"/wsx/b.mp4","note":"see \"c.srt\""}"#
        );
        let untouched = r#"{"path":"/elsewhere/a.mp4"}"#;
        assert!(matches!(relative_json(untouched, "/ws", false), Cow::Borrowed(_)));
        let windows = r#"{"path":"C:\\Work\\ws\\a.mp4","dir":"C:\\Work\\ws"}"#;
        assert_eq!(
            relative_json(windows, "C:\\Work\\ws", true),
            r#"{"path":"a.mp4","dir":"C:\\Work\\ws"}"#
        );
        let parsed: serde_json::Value = serde_json::from_str(&relative_json(r#"{"p":"/ws/a \"b\".mp4"}"#, "/ws", false)).unwrap();
        assert_eq!(parsed["p"], "a \"b\".mp4");
    }

    #[test]
    fn a_workspace_resolves_inputs_and_outputs_inside_it() {
        let dir = temp();
        std::fs::create_dir_all(dir.join("media")).unwrap();
        std::fs::write(dir.join("media/clip.mp4"), b"x").unwrap();
        let ws = Workspace::open(&dir).unwrap();
        assert_eq!(ws.root(), dir.to_str().unwrap());
        let at = |parts: &[&str]| {
            parts
                .iter()
                .fold(dir.clone(), |p, part| p.join(part))
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(ws.resolve("media/clip.mp4").unwrap(), at(&["media", "clip.mp4"]));
        assert_eq!(ws.resolve(&at(&["media", "clip.mp4"])).unwrap(), at(&["media", "clip.mp4"]));
        // An output not written yet resolves through its directory, even one still to be made.
        assert_eq!(ws.resolve("out.mp4").unwrap(), at(&["out.mp4"]));
        assert_eq!(ws.resolve("renders/new/out.mp4").unwrap(), at(&["renders", "new", "out.mp4"]));
        for bad in [
            "../outside.mp4",
            "/etc/passwd",
            "media/../../x.mp4",
            "https://example.com/a.mp4",
            "concat:a|b",
        ] {
            let why = ws.resolve(bad).unwrap_err();
            assert!(why.contains("workspace"), "{bad}: {why}");
        }
        assert!(ws.resolve("\\\\server\\share\\clip.mp4").unwrap_err().contains("network"));
        let e = confine(Some(&ws), "/etc/passwd").unwrap_err();
        assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(confine(None, "clip.mp4").unwrap(), "clip.mp4");
        let json = |path: &str| format!("{{\"path\":{}}}", serde_json::to_string(path).unwrap());
        let relative = Path::new("media").join("clip.mp4");
        assert_eq!(
            ws.relative_in(json(&at(&["media", "clip.mp4"]))),
            json(relative.to_str().unwrap())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_out_of_the_workspace_is_refused() {
        use std::os::unix::fs::symlink;
        let (dir, elsewhere) = (temp(), temp());
        std::fs::write(elsewhere.join("secret.mp4"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        symlink(elsewhere.join("secret.mp4"), dir.join("file-link.mp4")).unwrap();
        symlink(&elsewhere, dir.join("dir-link")).unwrap();
        symlink(elsewhere.join("missing.mp4"), dir.join("dangling.mp4")).unwrap();
        symlink(dir.join("inner"), dir.join("inner-link")).unwrap();
        let ws = Workspace::open(&dir).unwrap();
        for bad in [
            "file-link.mp4",
            "dir-link/secret.mp4",
            "dir-link/new.mp4",
            "dir-link/new/out.mp4",
        ] {
            assert!(ws.resolve(bad).unwrap_err().contains("outside the workspace"), "{bad}");
        }
        // A write through a link that leads nowhere would land wherever it points.
        assert!(ws.resolve("dangling.mp4").unwrap_err().contains("leads nowhere"));
        // A link that stays inside is followed: the path used is the real one.
        assert_eq!(
            ws.resolve("inner-link/out.mp4").unwrap(),
            dir.join("inner/out.mp4").to_string_lossy()
        );
        // The workspace named through a link is the same workspace.
        let alias = elsewhere.join("alias");
        symlink(&dir, &alias).unwrap();
        let ws = Workspace::open(&alias).unwrap();
        assert_eq!(ws.root(), dir.to_str().unwrap());
        let through_alias = alias.join("out.mp4");
        assert_eq!(
            ws.resolve(through_alias.to_str().unwrap()).unwrap(),
            dir.join("out.mp4").to_string_lossy()
        );
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    #[test]
    fn a_url_is_a_scheme_with_slashes_or_one_ffmpeg_knows_and_a_colon_in_a_unix_name_is_not() {
        for url in [
            "https://example.com/a.mp4",
            "HTTP://x",
            "ftp://h/a",
            "concat:a|b",
            "file:clip.mp4",
            "data:,x",
        ] {
            assert!(looks_like_url(url, false), "{url}");
            assert!(looks_like_url(url, true), "{url}");
        }
        for path in [
            "take1:final.mp4",
            "clip.mp4",
            "/ws/a:b.mp4",
            "a:b",
            "C:\\clip.mp4",
            "./concat:x",
        ] {
            assert!(!looks_like_url(path, false), "{path}");
        }
        // Windows keeps the strict rule: no file name there holds a colon.
        assert!(looks_like_url("take1:final.mp4", true));
        assert!(!looks_like_url("C:\\clip.mp4", true));
    }

    #[test]
    fn a_playlist_or_reference_format_is_known_by_its_name_or_its_first_bytes() {
        for (name, head) in [
            ("list.m3u8", &b""[..]),
            ("LIST.M3U", b""),
            ("show.mpd", b""),
            ("cut.ffconcat", b""),
            ("cam.sdp", b""),
            ("script.vpy", b""),
            ("clip.mp4", b"#EXTM3U\n#EXT-X-VERSION:3\n/etc/passwd\n"),
            ("clip.mp4", b"\xef\xbb\xbf  #extm3u\n"),
            ("clip.mp4", b"ffconcat version 1.0\nfile /etc/passwd\n"),
            ("clip.mp4", b"v=0\no=- 0 0 IN IP4 127.0.0.1\n"),
            ("clip.mp4", b"o=-\nc=IN IP4 10.0.0.1\n"),
            ("clip.mp4", b"<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash\">"),
            ("clip.mov", b"<?xml?><CompositionPlaylist>"),
        ] {
            assert!(
                reference_format(name, head).is_some(),
                "{name}: {:?}",
                String::from_utf8_lossy(head)
            );
        }
        for (name, head) in [
            ("clip.mp4", &b"\0\0\0\x18ftypmp42"[..]),
            ("tone.wav", b"RIFF\x24\0\0\0WAVEfmt "),
            ("subs.srt", b"1\n00:00:00,000 --> 00:00:01,000\nv=0 is not at the start\n"),
            ("noext", b""),
        ] {
            assert!(reference_format(name, head).is_none(), "{name}");
        }
        let dir = temp();
        let playlist = dir.join("clip.mp4");
        std::fs::write(&playlist, b"#EXTM3U\n#EXTINF:1,\n/etc/hosts\n").unwrap();
        assert!(refuse_reference_media(playlist.to_str().unwrap())
            .unwrap_err()
            .contains("HLS"));
        std::fs::write(&playlist, b"\0\0\0\x18ftypmp42").unwrap();
        assert!(refuse_reference_media(playlist.to_str().unwrap()).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_workspace_takes_only_named_speech_models() {
        let dir = temp();
        let ws = Workspace::open(&dir).unwrap();
        assert!(ws.check_speech_model("base").is_ok());
        for bad in ["/etc/passwd", "../model.bin", "custom.bin", "nope"] {
            assert!(ws.check_speech_model(bad).unwrap_err().contains("model name"), "{bad}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_project_naming_a_file_outside_the_workspace_is_refused() {
        let (dir, elsewhere) = (temp(), temp());
        let ws = Workspace::open(&dir).unwrap();
        let project = kerf_core::Project::open_in_memory().unwrap();
        let asset = |path: &Path| -> kerf_core::Asset {
            serde_json::from_value(serde_json::json!({
                "id": uuid::Uuid::new_v4(),
                "path": path.to_string_lossy(),
                "name": "clip.mp4",
                "duration": 1.0,
                "streams": [],
                "imported_at": "2026-01-01T00:00:00Z",
            }))
            .unwrap()
        };
        project.insert_or_get_asset(&asset(&dir.join("clip.mp4"))).unwrap();
        assert!(ws.check_project(&project).is_ok());
        project.set_meta(crate::SPEECH_MODEL_KEY, "/etc/model.bin").unwrap();
        assert!(ws.check_project(&project).unwrap_err().contains("model name"));
        project.set_meta(crate::SPEECH_MODEL_KEY, "base").unwrap();
        assert!(ws.check_project(&project).is_ok());
        project.insert_or_get_asset(&asset(&elsewhere.join("secret.mp4"))).unwrap();
        let why = ws.check_project(&project).unwrap_err();
        assert!(why.contains("outside the workspace") && why.contains("secret.mp4"), "{why}");
        let relative = kerf_core::Project::open_in_memory().unwrap();
        relative.insert_or_get_asset(&asset(Path::new("clip.mp4"))).unwrap();
        assert!(ws.check_project(&relative).unwrap_err().contains("relative"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    #[test]
    fn a_workspace_must_be_a_directory_that_exists() {
        let dir = temp();
        std::fs::write(dir.join("file"), b"x").unwrap();
        assert!(Workspace::open(&dir.join("file")).unwrap_err().contains("not a directory"));
        assert!(Workspace::open(&dir.join("missing")).is_err());
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        assert!(Workspace::open(Path::new(root)).unwrap_err().contains("below the root"));
        assert!(Workspace::open(&dir).unwrap().holds(&dir.join("new").join("token")));
        assert!(!Workspace::open(&dir).unwrap().holds(&std::env::temp_dir().join("elsewhere")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
