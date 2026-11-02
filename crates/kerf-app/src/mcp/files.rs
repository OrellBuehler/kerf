//! Media in and out of a hosted Kerf: `kerf-app --headless --workspace DIR` over HTTP serves the
//! workspace's files next to `/mcp`, behind the same bearer token and the same `Host` / `Origin`
//! checks, and the tools `list_files`, `file_url` and `import_url` say what is there and where.
//!
//! `PUT /files/<path>` streams the body into DIR (a `.part` beside the file, renamed into place once
//! whole; an existing file only with `?overwrite=1`, never one the open project uses), `GET` streams a
//! file out (one `Range` honoured) or lists a directory (`/files/<dir>/` or `?list`), `DELETE` removes
//! a file the project does not use. Every path is relative to DIR, plain (no `.`, `..` or empty part),
//! resolved by the workspace and refused when any part of it is a symbolic link. No route exists
//! without a workspace, and the project lock is held only to read what the project uses.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use kerf_core::Project;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{schemars, tool, tool_router, ErrorData as McpError};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use super::{blocking, json, server, KerfMcp, Workspace};

/// The largest file an upload or a URL import writes, unless `--max-upload-gb` says otherwise.
pub(crate) const DEFAULT_MAX_UPLOAD: u64 = 20 * kerf_core::cache_cap::GIB;

/// The most entries a listing returns.
const MAX_ENTRIES: usize = 5000;

/// The tools that exist only for a server with a workspace.
pub(super) const WORKSPACE_TOOLS: [&str; 3] = ["list_files", "file_url", "import_url"];

/// How a headless run with a workspace moves media: where clients reach `/files`, how big a file may
/// be, and whether `import_url` may fetch.
#[derive(Clone, Debug)]
pub(crate) struct MediaIo {
    /// `--public-url` / `KERF_PUBLIC_URL`: the server as clients reach it (behind a TLS proxy, with
    /// its path prefix), without a trailing `/`.
    pub public_url: Option<String>,
    pub max_upload_bytes: u64,
    pub allow_url_import: bool,
}

impl Default for MediaIo {
    fn default() -> Self {
        Self {
            public_url: None,
            max_upload_bytes: DEFAULT_MAX_UPLOAD,
            allow_url_import: false,
        }
    }
}

/// `--public-url`'s value, checked: `http(s)://host[:port][/prefix]`, no query, fragment or
/// credentials; the trailing `/` dropped.
pub(crate) fn check_public_url(url: &str) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let uri: axum::http::Uri = url.parse().map_err(|e| format!("{url:?} is not a URL: {e}"))?;
    let scheme_ok = matches!(uri.scheme_str(), Some("http" | "https"));
    let authority = uri.authority().map(|a| a.as_str()).unwrap_or_default();
    if !scheme_ok || authority.is_empty() || authority.contains('@') || uri.query().is_some() || url.contains('#') {
        return Err(format!(
            "{url:?} is not a base URL: give http(s)://host[:port][/prefix], e.g. https://kerf.example.com/kerf"
        ));
    }
    Ok(url.to_string())
}

/// The host of a `--public-url`, for the `Host` allow-list.
pub(crate) fn public_host(url: &str) -> Option<String> {
    let uri: axum::http::Uri = url.parse().ok()?;
    Some(uri.host()?.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase())
}

/// The project the routes protect, set by `server::start` when there is a workspace.
static PROJECT: OnceLock<Arc<Mutex<Project>>> = OnceLock::new();

pub(super) fn serve_project(project: Arc<Mutex<Project>>) {
    let _ = PROJECT.set(project);
}

/// Why a file request is refused, and the HTTP status that says so.
#[derive(Debug)]
pub(super) enum FileError {
    Bad(String),
    Forbidden(String),
    Missing(String),
    Conflict(String),
    TooLarge(String),
    Full(String),
    Failed(String),
}

impl FileError {
    fn status(&self) -> StatusCode {
        match self {
            FileError::Bad(_) => StatusCode::BAD_REQUEST,
            FileError::Forbidden(_) => StatusCode::FORBIDDEN,
            FileError::Missing(_) => StatusCode::NOT_FOUND,
            FileError::Conflict(_) => StatusCode::CONFLICT,
            FileError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            FileError::Full(_) => StatusCode::INSUFFICIENT_STORAGE,
            FileError::Failed(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &str {
        match self {
            FileError::Bad(m)
            | FileError::Forbidden(m)
            | FileError::Missing(m)
            | FileError::Conflict(m)
            | FileError::TooLarge(m)
            | FileError::Full(m)
            | FileError::Failed(m) => m,
        }
    }

    fn io(what: &str, e: &std::io::Error) -> Self {
        if disk_full(e) {
            FileError::Full(format!("the disk is full: {what} was not written"))
        } else {
            FileError::Failed(format!("{what}: {e}"))
        }
    }

    fn mcp(self) -> McpError {
        match self {
            FileError::Failed(m) => McpError::internal_error(m, None),
            other => McpError::invalid_params(other.message().to_string(), None),
        }
    }
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl IntoResponse for FileError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.message() }).to_string();
        (self.status(), [(header::CONTENT_TYPE, "application/json")], body).into_response()
    }
}

/// Whether a write failed because the disk (or the user's quota) is full.
pub(super) fn disk_full(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded)
}

/// `%XX` escapes decoded; a bad escape or bytes that are not UTF-8 are an error.
pub(super) fn percent_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| format!("{text:?} has a bad %-escape"))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| format!("{text:?} is not UTF-8 once decoded"))
}

/// `rel` with each part %-escaped for a URL path (letters, digits and `-._~` kept, `/` between parts).
fn percent_encode(rel: &str) -> String {
    let mut out = String::new();
    for b in rel.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Refuse a relative path that is not plain: absolute, a `.`, `..` or empty part, a backslash or a
/// control character. `""` is the workspace itself.
pub(super) fn check_plain(rel: &str) -> Result<(), String> {
    if rel.is_empty() {
        return Ok(());
    }
    let refused = rel.starts_with('/')
        || rel.contains('\\')
        || rel.chars().any(char::is_control)
        || rel
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || (cfg!(windows) && part.contains(':')));
    if refused {
        return Err(format!(
            "{rel:?} is not a plain path relative to the workspace: no leading `/`, no `.`, `..` or empty parts, no `\\`"
        ));
    }
    Ok(())
}

/// The workspace-relative path a `/files…` request path names, decoded and checked plain, and
/// whether it asks for a directory (`/files`, `/files/`, a trailing `/`).
pub(super) fn request_path(uri_path: &str) -> Result<(String, bool), String> {
    let rest = uri_path
        .strip_prefix("/files")
        .ok_or_else(|| format!("{uri_path:?} is not under /files"))?;
    let rest = match rest {
        "" => "",
        _ => rest
            .strip_prefix('/')
            .ok_or_else(|| format!("{uri_path:?} is not under /files/"))?,
    };
    let decoded = percent_decode(rest)?;
    let dir = decoded.is_empty() || decoded.ends_with('/');
    let rel = decoded.strip_suffix('/').unwrap_or(&decoded).to_string();
    check_plain(&rel)?;
    Ok((rel, dir))
}

/// What a `Range` header asks of a file of `len` bytes.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ranged {
    /// No range, one that does not parse, or several: the whole file.
    Whole,
    /// Bytes `start..=end`.
    Part(u64, u64),
    /// A range that lies past the end.
    Unsatisfiable,
}

/// Read a `Range` header (`bytes=a-b`, `bytes=a-`, `bytes=-n`) against a file of `len` bytes. One
/// range is served; several, or a malformed one, are ignored for the whole file (as RFC 9110 allows).
pub(super) fn parse_range(value: Option<&str>, len: u64) -> Ranged {
    let Some(spec) = value.and_then(|v| v.trim().strip_prefix("bytes=")) else {
        return Ranged::Whole;
    };
    if spec.contains(',') {
        return Ranged::Whole;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return Ranged::Whole;
    };
    let number = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u64>().ok())
            .flatten()
    };
    match (first.trim(), last.trim()) {
        ("", suffix) => match number(suffix) {
            Some(0) => Ranged::Unsatisfiable,
            Some(_) if len == 0 => Ranged::Unsatisfiable,
            Some(n) => Ranged::Part(len.saturating_sub(n), len - 1),
            None => Ranged::Whole,
        },
        (start, end) => {
            let Some(start) = number(start) else {
                return Ranged::Whole;
            };
            let end = match end {
                "" => None,
                end => match number(end) {
                    Some(end) => Some(end),
                    None => return Ranged::Whole,
                },
            };
            if end.is_some_and(|end| end < start) {
                return Ranged::Whole;
            }
            if start >= len {
                return Ranged::Unsatisfiable;
            }
            Ranged::Part(start, end.unwrap_or(len - 1).min(len - 1))
        }
    }
}

/// A Content-Type for a file by its extension.
pub(super) fn content_type(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mxf" => "application/mxf",
        "ts" | "mts" | "m2ts" => "video/mp2t",
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "aac" => "audio/aac",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "srt" => "application/x-subrip",
        "vtt" => "text/vtt",
        "json" | "otio" => "application/json",
        "csv" => "text/csv",
        "txt" | "edl" => "text/plain; charset=utf-8",
        "xml" | "fcpxml" => "application/xml",
        _ => "application/octet-stream",
    }
}

/// Where a write to `dest` goes until it is whole: `<name>.<8 hex>.part` beside it.
pub(super) fn part_path(dest: &Path) -> PathBuf {
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tag = &uuid::Uuid::new_v4().simple().to_string()[..8];
    dest.with_file_name(format!("{name}.{tag}.part"))
}

/// Move the whole file `part` to `dest`: replacing what is there with `overwrite`, else refused when
/// anything is (a hard link, which fails on an existing name, then the `.part` removed).
pub(super) fn place(part: &Path, dest: &Path, overwrite: bool) -> Result<(), FileError> {
    let shown = dest.display();
    if overwrite {
        return std::fs::rename(part, dest).map_err(|e| FileError::io(&format!("{shown}"), &e));
    }
    match std::fs::hard_link(part, dest) {
        Ok(()) => {
            let _ = std::fs::remove_file(part);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(exists(dest)),
        // A filesystem without hard links: the check and the rename are as close as it allows.
        Err(_) if std::fs::symlink_metadata(dest).is_ok() => Err(exists(dest)),
        Err(_) => std::fs::rename(part, dest).map_err(|e| FileError::io(&format!("{shown}"), &e)),
    }
}

fn exists(dest: &Path) -> FileError {
    FileError::Conflict(format!(
        "{} exists; pass overwrite to replace it, or pick another name",
        dest.display()
    ))
}

/// A path the client named, as `(absolute, relative to the workspace)`: resolved in it, and for a
/// request path (`plain`) refused when any part of it is a symbolic link (the resolved path is then
/// exactly the workspace joined with it).
pub(super) fn locate(ws: &Workspace, asked: &str, plain: bool) -> Result<(PathBuf, String), FileError> {
    let resolved = PathBuf::from(ws.resolve(asked).map_err(FileError::Forbidden)?);
    let root = Path::new(ws.root());
    let rel = resolved
        .strip_prefix(root)
        .map_err(|_| FileError::Forbidden(format!("{asked:?} is outside the workspace")))?;
    let rel: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let rel = rel.join("/");
    if plain && rel != asked {
        return Err(FileError::Forbidden(format!(
            "{asked:?} goes through a symbolic link; files are moved only by their own path"
        )));
    }
    Ok((resolved, rel))
}

/// One entry of a listing.
#[derive(Debug, serde::Serialize)]
pub(super) struct Entry {
    name: String,
    size: u64,
    modified: Option<String>,
    is_dir: bool,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct Listing {
    /// The directory, relative to the workspace (`.` for the workspace itself).
    path: String,
    entries: Vec<Entry>,
    /// Entries left out past the first [`MAX_ENTRIES`].
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated: Option<usize>,
}

/// The entries of the directory `dir` (absolute, `rel` in the workspace), by name; symbolic links are
/// left out (they are not served).
pub(super) fn list_dir(dir: &Path, rel: &str) -> Result<Listing, FileError> {
    let shown = if rel.is_empty() { "." } else { rel };
    let meta = std::fs::symlink_metadata(dir).map_err(|_| FileError::Missing(format!("{shown} does not exist")))?;
    if !meta.is_dir() {
        return Err(FileError::Bad(format!("{shown} is a file, not a directory")));
    }
    let read = std::fs::read_dir(dir).map_err(|e| FileError::Failed(format!("{shown}: {e}")))?;
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if meta.file_type().is_symlink() {
                return None;
            }
            Some(Entry {
                name: entry.file_name().to_str()?.to_string(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                modified: meta
                    .modified()
                    .ok()
                    .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                is_dir: meta.is_dir(),
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let left_out = entries.len().saturating_sub(MAX_ENTRIES);
    entries.truncate(MAX_ENTRIES);
    Ok(Listing {
        path: shown.to_string(),
        entries,
        truncated: (left_out > 0).then_some(left_out),
    })
}

/// What the open project uses at `path` (absolute, resolved), if anything: an asset's source or
/// capture file, or the project's own file. Reads the project under its lock, compares with it released.
fn in_use(project: &Mutex<Project>, path: &Path) -> Option<String> {
    let protected = crate::lock_user(project).output_path_inputs();
    let protected = match protected {
        Ok(protected) => protected,
        Err(e) => return Some(format!("unknown: the open project could not be read ({e})")),
    };
    let wanted = kerf_core::paths::canonical(path);
    protected
        .files
        .into_iter()
        .find(|(file, _)| kerf_core::paths::canonical(file) == wanted)
        .map(|(_, what)| what)
}

/// Refuse `path` while the open project uses it.
async fn refuse_in_use(path: &Path, rel: &str, doing: &str) -> Result<(), FileError> {
    let Some(project) = PROJECT.get().cloned() else {
        return Ok(());
    };
    let path = path.to_path_buf();
    let used = tokio::task::spawn_blocking(move || in_use(&project, &path))
        .await
        .map_err(|e| FileError::Failed(e.to_string()))?;
    match used {
        Some(what) => Err(FileError::Conflict(format!(
            "{rel} is {what} in the open project; it cannot be {doing} while the project uses it"
        ))),
        None => Ok(()),
    }
}

// ---- the routes ---------------------------------------------------------------

/// What the routes need.
struct Routes {
    workspace: Arc<Workspace>,
    max_upload: u64,
    hosts: Vec<String>,
}

/// The `/files` routes of a listener on `addr`, when the run has a workspace (and so a project to
/// protect); `None` otherwise, and the server is `/mcp` alone.
pub(super) fn router(addr: &str) -> Option<axum::Router> {
    let workspace = server::workspace()?;
    PROJECT.get()?;
    let state = Arc::new(Routes {
        workspace,
        max_upload: server::media().max_upload_bytes,
        hosts: server::hosts(addr),
    });
    Some(
        axum::Router::new()
            .route("/files", axum::routing::any(files))
            .route("/files/", axum::routing::any(files))
            .route("/files/{*path}", axum::routing::any(files))
            .with_state(state),
    )
}

/// What rmcp checks on `/mcp`, checked here too: a `Host` on the allow-list, and no `Origin` at all.
/// The refusal, or `None` for a request that passes.
fn refused_headers(hosts: &[String], request: &Request) -> Option<Response> {
    let refuse = |why: &'static str| Some((StatusCode::FORBIDDEN, why).into_response());
    if request.headers().contains_key(header::ORIGIN) {
        return refuse("Forbidden: Origin header is not allowed");
    }
    if hosts.is_empty() {
        return None;
    }
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| axum::http::uri::Authority::try_from(h).ok())
        .or_else(|| request.uri().authority().cloned());
    let Some(host) = host else {
        return Some((StatusCode::BAD_REQUEST, "Bad Request: missing Host header").into_response());
    };
    let bare = |h: &str| h.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    let name = bare(host.host());
    let allowed = hosts
        .iter()
        .any(|allowed| match axum::http::uri::Authority::try_from(allowed.trim()) {
            Ok(a) => bare(a.host()) == name && a.port_u16().is_none_or(|p| host.port_u16() == Some(p)),
            Err(_) => bare(allowed.trim()) == name,
        });
    if allowed {
        None
    } else {
        refuse("Forbidden: Host header is not allowed")
    }
}

/// Whether the query has `name` switched on (`?overwrite`, `?overwrite=1`, `=true`).
fn flag(query: Option<&str>, name: &str) -> bool {
    query.unwrap_or("").split('&').any(|pair| match pair.split_once('=') {
        Some((key, value)) => key == name && matches!(value, "1" | "true" | "yes"),
        None => pair == name,
    })
}

async fn files(State(routes): State<Arc<Routes>>, request: Request) -> Response {
    if let Some(refused) = refused_headers(&routes.hosts, &request) {
        return refused;
    }
    let (rel, dir) = match request_path(request.uri().path()) {
        Ok(found) => found,
        Err(why) => return FileError::Bad(why).into_response(),
    };
    let query = request.uri().query().map(str::to_string);
    let answer = match *request.method() {
        Method::GET | Method::HEAD => {
            let head = request.method() == Method::HEAD;
            get(&routes, &rel, dir || flag(query.as_deref(), "list"), request.headers(), head).await
        }
        Method::PUT => put(&routes, &rel, dir, flag(query.as_deref(), "overwrite"), request).await,
        Method::DELETE => delete(&routes, &rel).await,
        _ => {
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                [(header::ALLOW, "GET, HEAD, PUT, DELETE")],
                "GET, HEAD, PUT or DELETE",
            )
                .into_response()
        }
    };
    answer.unwrap_or_else(IntoResponse::into_response)
}

async fn get(routes: &Routes, rel: &str, listing: bool, headers: &HeaderMap, head: bool) -> Result<Response, FileError> {
    let (path, rel) = locate(&routes.workspace, rel, true)?;
    let meta = tokio::fs::symlink_metadata(&path)
        .await
        .map_err(|_| FileError::Missing(format!("{} does not exist", if rel.is_empty() { "." } else { &rel })))?;
    if listing || meta.is_dir() {
        let listed = tokio::task::spawn_blocking(move || list_dir(&path, &rel))
            .await
            .map_err(|e| FileError::Failed(e.to_string()))??;
        let body = serde_json::to_string(&listed).map_err(|e| FileError::Failed(e.to_string()))?;
        return Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response());
    }
    if !meta.is_file() {
        return Err(FileError::Bad(format!("{rel} is not a regular file")));
    }
    let mut file = open_no_follow(&path)
        .await
        .map_err(|e| FileError::Failed(format!("{rel}: {e}")))?;
    let len = file
        .metadata()
        .await
        .map_err(|e| FileError::Failed(format!("{rel}: {e}")))?
        .len();
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let name = Path::new(&rel)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, content_type(&name))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_DISPOSITION, attachment(&name));
    let (start, count) = match parse_range(range, len) {
        Ranged::Whole => (0, len),
        Ranged::Part(start, end) => {
            response = response
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
            (start, end - start + 1)
        }
        Ranged::Unsatisfiable => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{len}"))],
            )
                .into_response())
        }
    };
    response = response.header(header::CONTENT_LENGTH, count);
    if head {
        return response.body(Body::empty()).map_err(|e| FileError::Failed(e.to_string()));
    }
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|e| FileError::Failed(format!("{rel}: {e}")))?;
    let stream = tokio_util::io::ReaderStream::with_capacity(tokio::io::AsyncReadExt::take(file, count), 256 * 1024);
    response
        .body(Body::from_stream(stream))
        .map_err(|e| FileError::Failed(e.to_string()))
}

/// `Content-Disposition` for a download: the name with anything but printable ASCII (and `"`, `\`) made `_`.
fn attachment(name: &str) -> HeaderValue {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    HeaderValue::from_str(&format!("attachment; filename=\"{safe}\"")).unwrap_or(HeaderValue::from_static("attachment"))
}

async fn open_no_follow(path: &Path) -> std::io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    options.open(path).await
}

/// The `.part` an upload writes, removed unless it was put in place: a request dropped mid-body (the
/// client went away) leaves nothing behind.
struct Part(Option<PathBuf>);

impl Part {
    fn path(&self) -> &Path {
        self.0.as_deref().expect("a part until it is placed")
    }
}

impl Drop for Part {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn put(routes: &Routes, rel: &str, dir: bool, overwrite: bool, request: Request) -> Result<Response, FileError> {
    if rel.is_empty() || dir {
        return Err(FileError::Bad(
            "PUT names a file: /files/<path relative to the workspace>".to_string(),
        ));
    }
    let too_large = || {
        FileError::TooLarge(format!(
            "an upload may be at most {} bytes on this server (--max-upload-gb)",
            routes.max_upload
        ))
    };
    let announced = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if announced.is_some_and(|n| n > routes.max_upload) {
        return Err(too_large());
    }
    let (path, rel) = locate(&routes.workspace, rel, true)?;
    let existed = match tokio::fs::symlink_metadata(&path).await {
        Ok(meta) if meta.is_dir() => return Err(FileError::Conflict(format!("{rel} is a directory"))),
        Ok(_) if !overwrite => return Err(exists(Path::new(&rel))),
        Ok(_) => true,
        Err(_) => false,
    };
    if existed {
        refuse_in_use(&path, &rel, "replaced").await?;
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| FileError::io(&format!("the directory of {rel}"), &e))?;
        // A directory made by another request meanwhile may be a link: the path is checked again.
        locate(&routes.workspace, &rel, true)?;
    }
    let part = Part(Some(part_path(&path)));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(part.path())
        .await
        .map_err(|e| FileError::io(&rel, &e))?;
    let mut body = request.into_body().into_data_stream();
    let mut written = 0u64;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| FileError::Bad(format!("the upload of {rel} broke off: {e}")))?;
        written += chunk.len() as u64;
        if written > routes.max_upload {
            return Err(too_large());
        }
        file.write_all(&chunk).await.map_err(|e| FileError::io(&rel, &e))?;
    }
    if announced.is_some_and(|n| n != written) {
        return Err(FileError::Bad(format!(
            "the upload of {rel} ended after {written} of {} bytes",
            announced.unwrap_or_default()
        )));
    }
    file.flush().await.map_err(|e| FileError::io(&rel, &e))?;
    file.sync_all().await.map_err(|e| FileError::io(&rel, &e))?;
    drop(file);
    // The project may have taken the file in while the body arrived.
    if existed || tokio::fs::symlink_metadata(&path).await.is_ok() {
        refuse_in_use(&path, &rel, "replaced").await?;
    }
    let mut part = part;
    let from = part.0.take().expect("a part");
    let placed = {
        let (from, to) = (from.clone(), path.clone());
        tokio::task::spawn_blocking(move || place(&from, &to, overwrite))
            .await
            .map_err(|e| FileError::Failed(e.to_string()))
    };
    if !matches!(placed, Ok(Ok(()))) {
        let _ = tokio::fs::remove_file(&from).await;
    }
    placed?.map_err(|e| match e {
        FileError::Conflict(_) => exists(Path::new(&rel)),
        other => other,
    })?;
    tracing::info!(target: "mcp", path = %rel, bytes = written, "file uploaded into the workspace");
    let status = if existed { StatusCode::OK } else { StatusCode::CREATED };
    let body = serde_json::json!({ "path": rel, "size": written, "replaced": existed }).to_string();
    Ok((status, [(header::CONTENT_TYPE, "application/json")], body).into_response())
}

async fn delete(routes: &Routes, rel: &str) -> Result<Response, FileError> {
    if rel.is_empty() {
        return Err(FileError::Bad("the workspace itself is not deleted".to_string()));
    }
    let (path, rel) = locate(&routes.workspace, rel, true)?;
    let meta = tokio::fs::symlink_metadata(&path)
        .await
        .map_err(|_| FileError::Missing(format!("{rel} does not exist")))?;
    if meta.is_dir() {
        tokio::fs::remove_dir(&path).await.map_err(|e| {
            if std::fs::read_dir(&path).is_ok_and(|mut d| d.next().is_some()) {
                FileError::Conflict(format!("{rel} is a directory that is not empty"))
            } else {
                FileError::Failed(format!("{rel}: {e}"))
            }
        })?;
    } else {
        refuse_in_use(&path, &rel, "deleted").await?;
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| FileError::Failed(format!("{rel}: {e}")))?;
    }
    tracing::info!(target: "mcp", path = %rel, "file deleted from the workspace");
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---- the tools ------------------------------------------------------------------

/// Whether this connection lists (and runs) `tool`, one of [`WORKSPACE_TOOLS`]: only with a
/// workspace; `file_url` only over HTTP, where the routes are; `import_url` only with `--allow-url-import`.
pub(super) fn offered(server: &KerfMcp, tool: &str) -> bool {
    if !WORKSPACE_TOOLS.contains(&tool) {
        return true;
    }
    if server.workspace.is_none() {
        return false;
    }
    match tool {
        "file_url" => server.stdio.is_none(),
        "import_url" => server::media().allow_url_import,
        _ => true,
    }
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ListFilesParams {
    #[schemars(description = "A directory relative to the workspace (default: the workspace itself)")]
    dir: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct FileUrlParams {
    #[schemars(description = "A file relative to the workspace: one there to download, or one to upload (it need not exist)")]
    path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ImportUrlParams {
    #[schemars(description = "The http(s) URL of a media file")]
    url: String,
    #[schemars(
        description = "Where in the workspace to save it (refused if a file is there). Default downloads/<the URL's file name>, numbered when that is taken"
    )]
    path: Option<String>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once; poll job_status (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

impl KerfMcp {
    fn workspace_or_refuse(&self) -> Result<Arc<Workspace>, McpError> {
        self.workspace
            .clone()
            .ok_or_else(|| McpError::invalid_params("tool not found", None))
    }

    /// Where `/files` is reached for this connection: `--public-url`, else the `Host` the request
    /// came in on, else the listener's own address.
    fn files_base(&self, context: &RequestContext<RoleServer>) -> String {
        if let Some(url) = server::media().public_url {
            return url;
        }
        let host = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.headers.get(header::HOST))
            .and_then(|h| h.to_str().ok())
            .filter(|h| axum::http::uri::Authority::try_from(*h).is_ok());
        match host {
            Some(host) => format!("http://{host}"),
            None => server::listening_base(),
        }
    }

    /// A free name for a download into `downloads/`: `name`, else `stem-2.ext`, `stem-3.ext`, ….
    fn free_download(ws: &Workspace, name: &str) -> Result<(PathBuf, String), McpError> {
        let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name);
        let ext = Path::new(name).extension().and_then(|e| e.to_str());
        for n in 1..1000 {
            let candidate = match (n, ext) {
                (1, _) => name.to_string(),
                (n, Some(ext)) => format!("{stem}-{n}.{ext}"),
                (n, None) => format!("{stem}-{n}"),
            };
            let (path, rel) = locate(ws, &format!("downloads/{candidate}"), true).map_err(FileError::mcp)?;
            if std::fs::symlink_metadata(&path).is_err() {
                return Ok((path, rel));
            }
        }
        Err(McpError::invalid_params(
            format!("downloads/ already holds {name} and 999 numbered copies; pass a path"),
            None,
        ))
    }
}

#[tool_router(router = tool_router_files, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "The files in a workspace directory: `entries` with `name`, `size` (bytes), `modified` and \
                       `is_dir`, by name (symbolic links are left out). Use it to find uploaded media and the exports \
                       you wrote. Only on a server with a workspace."
    )]
    async fn list_files(&self, Parameters(p): Parameters<ListFilesParams>) -> Result<String, McpError> {
        let ws = self.workspace_or_refuse()?;
        let dir = p.dir.unwrap_or_default();
        let listing = blocking(move || {
            let (path, rel) = locate(&ws, dir.trim().trim_end_matches('/'), false).map_err(FileError::mcp)?;
            list_dir(&path, &rel).map_err(FileError::mcp)
        })
        .await?;
        json(&listing)
    }

    #[tool(
        description = "The HTTP URL of a workspace file, for moving bytes in and out of this server: `GET` downloads \
                       it (a `Range` header is honoured), `PUT` uploads the request body to it (refused if a file is \
                       there unless `?overwrite=1`; never a file the open project uses), `DELETE` removes it. Every \
                       request needs the same `Authorization: Bearer <token>` header as this MCP connection (ask the \
                       user if you do not have it), e.g. `curl -H \"Authorization: Bearer $TOKEN\" -T clip.mp4 <url>` \
                       then import_asset the `path`; after an export, `curl -H ... -o cut.mp4 <url>`. Only on an HTTP \
                       server with a workspace."
    )]
    async fn file_url(
        &self,
        Parameters(p): Parameters<FileUrlParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let ws = self.workspace_or_refuse()?;
        let asked = p.path.trim().to_string();
        let (path, rel) = blocking(move || locate(&ws, &asked, false).map_err(FileError::mcp)).await?;
        if rel.is_empty() {
            return Err(McpError::invalid_params(
                "name a file in the workspace, not the workspace itself",
                None,
            ));
        }
        let meta = std::fs::symlink_metadata(&path).ok();
        let url = format!("{}/files/{}", self.files_base(&context), percent_encode(&rel));
        json(&serde_json::json!({
            "path": rel,
            "url": url,
            "exists": meta.is_some(),
            "is_dir": meta.as_ref().map(|m| m.is_dir()),
            "size": meta.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
            "header": "Authorization: Bearer <token>",
        }))
    }

    #[tool(
        description = "Download an http(s) URL into the workspace and import it like import_asset (answers the Asset; \
                       `path` is where the file landed). Only public addresses are fetched: a URL (or any redirect) \
                       to loopback, a private or link-local network or a metadata address is refused. Large files take \
                       a while: send a progressToken, cancel the request to stop it, or pass `background: true` to run it as a \
                       background job. Only on a server started with --allow-url-import."
    )]
    async fn import_url(
        &self,
        Parameters(p): Parameters<ImportUrlParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let ws = self.workspace_or_refuse()?;
        let media = server::media();
        if !media.allow_url_import {
            return Err(McpError::invalid_params(
                "this server does not fetch URLs (it was started without --allow-url-import): upload the file with \
                 file_url instead",
                None,
            ));
        }
        let uri = super::fetch::check_url(&p.url).map_err(|e| McpError::invalid_params(e, None))?;
        let url = p.url.trim().to_string();
        let asked = p.path.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        let (dest, rel) = blocking(move || match asked {
            Some(asked) => {
                let (path, rel) = locate(&ws, &asked, false).map_err(FileError::mcp)?;
                if rel.is_empty() || std::fs::symlink_metadata(&path).is_ok() {
                    return Err(FileError::Conflict(format!("{asked} exists; pick a path where nothing is")).mcp());
                }
                Ok((path, rel))
            }
            None => Self::free_download(&ws, &super::fetch::file_name(&uri)),
        })
        .await?;
        let (me, owner, generation) = (self.clone(), self.owner(), self.lock().generation());
        self.long_tool("import_url", p.background, &context, move |run| async move {
            let limits = super::fetch::Limits {
                max_bytes: media.max_upload_bytes,
                stall: super::fetch::STALL,
                permit: super::fetch::is_public,
            };
            let (fetching, target) = (run.clone(), dest.clone());
            blocking(move || {
                let mut progress = |written: u64, total: Option<u64>| {
                    let fraction = total.filter(|t| *t > 0).map(|t| written as f64 / t as f64 * 0.9);
                    fetching.progress(fraction, format!("downloading, {} MB", written / (1024 * 1024)));
                };
                super::fetch::download(&url, &target, &limits, &|| fetching.cancelled(), &mut progress)
                    .map_err(|e| McpError::invalid_params(e, None))?;
                tracing::info!(target: "mcp", %url, path = %target.display(), "downloaded into the workspace");
                // What import_asset refuses, refused here too; a file that will not be imported is not kept.
                let path = target.to_string_lossy().into_owned();
                let checked = crate::require_local_input_path(&path)
                    .and_then(|()| kerf_core::paths::validate_import_path(&target).map_err(|e| e.to_string()))
                    .and_then(|()| super::workspace::refuse_reference_media(&path));
                if let Err(why) = checked {
                    let _ = std::fs::remove_file(&target);
                    return Err(McpError::invalid_params(why, None));
                }
                Ok(())
            })
            .await?;
            run.progress(Some(0.9), format!("importing {rel}"));
            me.import_run(dest.to_string_lossy().into_owned(), run, owner, generation)
                .await
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_path_is_plain_and_relative() {
        assert_eq!(request_path("/files").unwrap(), (String::new(), true));
        assert_eq!(request_path("/files/").unwrap(), (String::new(), true));
        assert_eq!(
            request_path("/files/media/clip.mp4").unwrap(),
            ("media/clip.mp4".into(), false)
        );
        assert_eq!(request_path("/files/media/").unwrap(), ("media".into(), true));
        assert_eq!(request_path("/files/My%20Clip.mp4").unwrap(), ("My Clip.mp4".into(), false));
        assert_eq!(request_path("/files/%C3%A9t%C3%A9.wav").unwrap(), ("été.wav".into(), false));
        for refused in [
            "/files/../etc/passwd",
            "/files/a/../../b",
            "/files/%2E%2E/x",
            "/files/a%2F..%2F..%2Fb",
            "/files//etc/passwd",
            "/files/%2Fetc%2Fpasswd",
            "/files/a//b",
            "/files/./a",
            "/files/a\\b",
            "/files/a%5C..%5Cb",
            "/files/a%00b",
            "/files/a%0Ab",
            "/files/bad%zz",
            "/files/%FF",
            "/filesx/a",
            "/mcp",
        ] {
            assert!(request_path(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_range_is_one_span_of_the_file_or_the_whole_file() {
        let len = 1000;
        assert_eq!(parse_range(None, len), Ranged::Whole);
        assert_eq!(parse_range(Some("bytes=0-99"), len), Ranged::Part(0, 99));
        assert_eq!(parse_range(Some("bytes=500-"), len), Ranged::Part(500, 999));
        assert_eq!(parse_range(Some("bytes=-100"), len), Ranged::Part(900, 999));
        assert_eq!(parse_range(Some("bytes=-5000"), len), Ranged::Part(0, 999));
        assert_eq!(parse_range(Some("bytes=900-5000"), len), Ranged::Part(900, 999));
        assert_eq!(parse_range(Some(" bytes=0-0"), len), Ranged::Part(0, 0));
        assert_eq!(parse_range(Some("bytes=1000-"), len), Ranged::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-0"), len), Ranged::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-"), 0), Ranged::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-1"), 0), Ranged::Unsatisfiable);
        for ignored in [
            "bytes=0-1,5-6",
            "bytes=5-1",
            "bytes=a-b",
            "bytes=-",
            "bytes=1-x",
            "items=0-1",
            "bytes=+1-2",
            "0-1",
        ] {
            assert_eq!(parse_range(Some(ignored), len), Ranged::Whole, "{ignored}");
        }
    }

    #[test]
    fn percent_escapes_round_trip() {
        let rel = "media/My Clip #1 (été).mp4";
        let encoded = percent_encode(rel);
        assert!(
            !encoded.contains(' ') && !encoded.contains('#') && encoded.starts_with("media/"),
            "{encoded}"
        );
        assert_eq!(percent_decode(&encoded).unwrap(), rel);
        assert!(percent_decode("%4").is_err());
    }

    #[test]
    fn a_public_url_is_a_base_without_a_query() {
        assert_eq!(
            check_public_url("https://kerf.example.com/kerf/").unwrap(),
            "https://kerf.example.com/kerf"
        );
        assert_eq!(check_public_url("http://10.0.0.5:7777").unwrap(), "http://10.0.0.5:7777");
        for refused in [
            "kerf.example.com",
            "ftp://x.example",
            "https://x.example/?a=1",
            "https://u:p@x.example",
            "https://x.example/#a",
        ] {
            assert!(check_public_url(refused).is_err(), "{refused}");
        }
        assert_eq!(
            public_host("https://Kerf.Example.com/kerf").as_deref(),
            Some("kerf.example.com")
        );
        assert_eq!(public_host("http://[::1]:9/").as_deref(), Some("::1"));
    }

    #[test]
    fn a_media_file_has_its_type_and_anything_else_is_bytes() {
        assert_eq!(content_type("cut.MP4"), "video/mp4");
        assert_eq!(content_type("a.wav"), "audio/wav");
        assert_eq!(content_type("captions.srt"), "application/x-subrip");
        assert_eq!(content_type("noext"), "application/octet-stream");
        assert_eq!(content_type("cut.kerf"), "application/octet-stream");
    }

    #[test]
    fn placing_a_part_refuses_an_existing_file_unless_told_to_replace_it() {
        let dir = std::env::temp_dir().join(format!("kerf-place-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.mp4");
        let part = part_path(&dest);
        assert!(part.file_name().unwrap().to_str().unwrap().starts_with("a.mp4.") && part.extension().unwrap() == "part");
        std::fs::write(&part, b"one").unwrap();
        place(&part, &dest, false).unwrap();
        assert!(!part.exists());
        std::fs::write(&part, b"two").unwrap();
        assert!(matches!(place(&part, &dest, false), Err(FileError::Conflict(_))));
        assert_eq!(std::fs::read(&dest).unwrap(), b"one");
        place(&part, &dest, true).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_listing_names_files_and_directories_and_skips_links() {
        let dir = std::env::temp_dir().join(format!("kerf-list-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.wav"), b"12345").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", dir.join("link")).unwrap();
        let listed = list_dir(&dir, "").unwrap();
        assert_eq!(listed.path, ".");
        let names: Vec<(&str, u64, bool)> = listed.entries.iter().map(|e| (e.name.as_str(), e.size, e.is_dir)).collect();
        assert_eq!(names, vec![("b.wav", 5, false), ("sub", 0, true)]);
        assert!(listed.entries[0].modified.as_deref().is_some_and(|m| m.ends_with('Z')));
        assert!(matches!(list_dir(&dir.join("b.wav"), "b.wav"), Err(FileError::Bad(_))));
        assert!(matches!(list_dir(&dir.join("none"), "none"), Err(FileError::Missing(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_query_flag_is_on_bare_or_true() {
        assert!(flag(Some("overwrite=1"), "overwrite"));
        assert!(flag(Some("a=b&overwrite"), "overwrite"));
        assert!(flag(Some("overwrite=true"), "overwrite"));
        assert!(!flag(Some("overwrite=0"), "overwrite"));
        assert!(!flag(None, "overwrite"));
        assert!(!flag(Some("overwritex=1"), "overwrite"));
    }
}
