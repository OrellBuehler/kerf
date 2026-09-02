//! Speech-to-text: ggml model provisioning, plus transcription driven through
//! the `ffmpeg` **binary**.
//!
//! Kerf's text-based editing surface — the transcript tab, sentence-level
//! `cut_clip_range`, captions, SRT export, and the transcript an agent reads
//! over MCP — all hang off one field, [`crate::model::AssetAnalysis::transcript`].
//! Filling it used to need a build with the `whisper` feature *and* a
//! hand-downloaded model named by `KERF_WHISPER_MODEL`, so in practice it was
//! always empty. Two pieces here fix that:
//!
//! * [`ensure_model`] downloads a ggml model into the OS cache directory the
//!   first time one is needed, streaming progress, so nothing is installed by
//!   hand. Both backends share it.
//! * [`transcribe`] runs FFmpeg 8.0's native `whisper` audio filter, so a build
//!   with no whisper.cpp toolchain at all still transcribes — provided the
//!   `ffmpeg` binary was configured `--enable-whisper` ([`filter_available`]
//!   probes that once per process).
//!
//! Neither piece needs the FFmpeg *dev* libraries, so both live in the
//! always-compiled CLI half of the engine.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

use super::cli::{bg_command, command, ffmpeg_bin, launch_err};
use super::cpu;
use crate::error::{Error, Result};
use crate::model::TranscriptSegment;

/// The model downloaded when nothing else is configured. `base` is the smallest
/// multilingual model that produces usable sentence segmentation on ordinary
/// interview audio, and at ~148 MB it is a tolerable first-run download; the
/// `.en` variants are more accurate but silently mistranscribe every other
/// language, which is the wrong default for an editor that does not know what
/// its user shoots in.
pub const DEFAULT_MODEL: &str = "base";

/// A downloadable ggml speech model.
#[derive(Debug, Clone, Copy, serde::Serialize, schemars::JsonSchema)]
pub struct ModelInfo {
    /// whisper.cpp model name, e.g. `base.en` — also the download's file stem.
    pub name: &'static str,
    /// Roughly how large the download is. Only for labelling a UI: the real
    /// size comes from the response's `Content-Length`.
    pub approx_bytes: u64,
    /// Whether the model understands languages other than English.
    pub multilingual: bool,
}

const MB: u64 = 1024 * 1024;

/// The models Kerf offers to download, smallest first.
pub const MODELS: &[ModelInfo] = &[
    ModelInfo {
        name: "tiny",
        approx_bytes: 75 * MB,
        multilingual: true,
    },
    ModelInfo {
        name: "tiny.en",
        approx_bytes: 75 * MB,
        multilingual: false,
    },
    ModelInfo {
        name: "base",
        approx_bytes: 142 * MB,
        multilingual: true,
    },
    ModelInfo {
        name: "base.en",
        approx_bytes: 142 * MB,
        multilingual: false,
    },
    ModelInfo {
        name: "small",
        approx_bytes: 466 * MB,
        multilingual: true,
    },
    ModelInfo {
        name: "small.en",
        approx_bytes: 466 * MB,
        multilingual: false,
    },
    ModelInfo {
        name: "medium",
        approx_bytes: 1500 * MB,
        multilingual: true,
    },
    ModelInfo {
        name: "medium.en",
        approx_bytes: 1500 * MB,
        multilingual: false,
    },
    ModelInfo {
        name: "large-v3-turbo",
        approx_bytes: 1600 * MB,
        multilingual: true,
    },
];

/// Look up a known model by name.
pub fn model_info(name: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.name == name)
}

/// Where a named model is cached: `<cache>/kerf/models/ggml-<name>.bin`.
/// `None` when the OS exposes no cache directory.
pub fn model_path(name: &str) -> Option<PathBuf> {
    Some(
        dirs::cache_dir()?
            .join("kerf")
            .join("models")
            .join(format!("ggml-{name}.bin")),
    )
}

/// The whisper.cpp model repository on Hugging Face. Overridable with
/// `KERF_WHISPER_MODEL_URL` (a directory URL, no trailing slash) for an offline
/// mirror or an internal cache.
fn model_base_url() -> String {
    std::env::var("KERF_WHISPER_MODEL_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "https://huggingface.co/ggerganov/whisper.cpp/resolve/main".to_string())
}

fn model_url(name: &str) -> String {
    format!("{}/ggml-{name}.bin", model_base_url())
}

/// Which model transcription should use.
#[derive(Debug, Clone)]
pub enum ModelChoice {
    /// A model file the user supplied by path; used as-is, never downloaded.
    File(PathBuf),
    /// A whisper.cpp model name to resolve in (and download into) the cache.
    Named(String),
}

/// The model chosen at runtime (by the GUI's picker, restored from the project),
/// which outranks the environment. Process-global because the transcribers are
/// free functions with no project handle — the adapter sets it once at startup
/// and whenever the user picks.
fn model_override() -> &'static Mutex<Option<String>> {
    static OVERRIDE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

/// Choose the speech model for this process; `None` falls back to the
/// environment and then to [`DEFAULT_MODEL`]. Accepts a model name or a path,
/// exactly like `KERF_WHISPER_MODEL`.
pub fn set_model(choice: Option<&str>) {
    let mut guard = model_override().lock().unwrap_or_else(|e| e.into_inner());
    *guard = choice.map(str::to_string).filter(|c| !c.is_empty());
}

/// Resolve the configured model into a [`ModelChoice`].
///
/// Precedence is the runtime choice ([`set_model`]), then `KERF_WHISPER_MODEL`,
/// then [`DEFAULT_MODEL`]. That variable historically held a *path* to a ggml
/// file and still may — anything that exists on disk, or that looks like a path
/// rather than a bare model name, is taken literally so existing setups keep
/// working. A bare name (`small.en`) instead selects one of [`MODELS`] to fetch.
pub fn configured_model() -> ModelChoice {
    let chosen = model_override()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .or_else(|| std::env::var("KERF_WHISPER_MODEL").ok())
        .filter(|v| !v.is_empty());
    let Some(raw) = chosen else {
        return ModelChoice::Named(DEFAULT_MODEL.to_string());
    };
    let as_path = Path::new(&raw);
    if as_path.exists() || raw.contains(['/', '\\']) || raw.ends_with(".bin") {
        ModelChoice::File(as_path.to_path_buf())
    } else {
        ModelChoice::Named(raw)
    }
}

/// The model file to transcribe with **if it is already on disk**, without
/// downloading anything. Used to decide whether transcription can run right now.
pub fn ready_model() -> Option<PathBuf> {
    match configured_model() {
        ModelChoice::File(p) => p.is_file().then_some(p),
        ModelChoice::Named(name) => model_path(&name).filter(|p| p.is_file()),
    }
}

/// How far a model download has got.
#[derive(Debug, Clone, Copy)]
pub struct DownloadProgress {
    pub downloaded: u64,
    /// The full size, when the server declared one.
    pub total: Option<u64>,
}

impl DownloadProgress {
    pub fn fraction(&self) -> Option<f64> {
        self.total
            .filter(|t| *t > 0)
            .map(|t| (self.downloaded as f64 / t as f64).clamp(0.0, 1.0))
    }
}

/// The model file to transcribe with, downloading it first if it isn't cached.
///
/// A user-supplied path is never fetched — if it is missing that is a
/// configuration error, not something to paper over with a different model.
/// `cancel` is polled while downloading: a model is hundreds of megabytes, and
/// without this, asking analysis to stop meant waiting out a fetch it had
/// already started. The partial `.part` file is deliberately *kept* on a
/// cancel, so the next attempt resumes it.
pub fn ensure_model(progress: &mut dyn FnMut(DownloadProgress), cancel: &dyn Fn() -> bool) -> Result<PathBuf> {
    match configured_model() {
        ModelChoice::File(p) => {
            if p.is_file() {
                Ok(p)
            } else {
                Err(Error::Engine(format!(
                    "KERF_WHISPER_MODEL points at {}, which does not exist",
                    p.display()
                )))
            }
        }
        ModelChoice::Named(name) => download_model_cancellable(&name, progress, cancel),
    }
}

/// Fetch model `name` into the cache if it isn't there yet, returning its path.
///
/// The download streams into a `.part` file next to the destination and is
/// renamed into place only once complete, so an interrupted or concurrent fetch
/// can never leave a truncated file that later runs would load as a model. A
/// leftover `.part` is resumed with a range request rather than restarted —
/// these are hundreds of megabytes.
pub fn download_model(name: &str, progress: &mut dyn FnMut(DownloadProgress)) -> Result<PathBuf> {
    download_model_cancellable(name, progress, &|| false)
}

/// [`download_model`], polling `cancel` as it streams.
pub fn download_model_cancellable(
    name: &str,
    progress: &mut dyn FnMut(DownloadProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    if model_info(name).is_none() {
        return Err(Error::Engine(format!(
            "unknown speech model '{name}'; expected one of: {}",
            MODELS.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")
        )));
    }
    let dst = model_path(name).ok_or_else(|| Error::Engine("no cache directory available for speech models".to_string()))?;
    if dst.is_file() {
        return Ok(dst);
    }
    let parent = dst
        .parent()
        .ok_or_else(|| Error::Engine("speech model cache path has no parent".to_string()))?;
    std::fs::create_dir_all(parent).map_err(|e| Error::Engine(format!("could not create model cache dir: {e}")))?;

    let tmp = dst.with_extension(format!("{}.part", std::process::id()));
    let url = model_url(name);
    tracing::info!(model = name, %url, "downloading speech model");
    // A cancel keeps the `.part` file: it is a valid prefix of the model, and
    // the next attempt resumes it with a range request. Any other failure is a
    // file we can't trust, so it goes.
    stream_to_file(&url, &tmp, progress, cancel).inspect_err(|e| {
        if !matches!(e, Error::Cancelled) {
            let _ = std::fs::remove_file(&tmp);
        }
    })?;

    // A model that isn't one (an HTML error page, a truncated CDN response)
    // would otherwise only fail much later, inside whisper, as an unreadable
    // error about tensors.
    verify_ggml(&tmp)?;

    // Another process may have finished the same download meanwhile.
    if dst.is_file() {
        let _ = std::fs::remove_file(&tmp);
        return Ok(dst);
    }
    std::fs::rename(&tmp, &dst).map_err(|e| Error::Engine(format!("could not finalize speech model download: {e}")))?;
    tracing::info!(model = name, path = %dst.display(), "speech model ready");
    Ok(dst)
}

/// Stream `url` into `tmp`, resuming a partial file when one is there.
fn stream_to_file(url: &str, tmp: &Path, progress: &mut dyn FnMut(DownloadProgress), cancel: &dyn Fn() -> bool) -> Result<()> {
    let have = std::fs::metadata(tmp).map(|m| m.len()).unwrap_or(0);
    // A connect timeout but no global one: reaching the host should fail fast
    // (a firewalled or DNS-blackholed mirror otherwise stalls for minutes before
    // admitting it), while the transfer itself is legitimately allowed to run for
    // as long as a gigabyte takes.
    let mut request = ureq::get(url)
        .config()
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .build();
    if have > 0 {
        request = request.header("Range", &format!("bytes={have}-"));
    }
    // Name the URL: the fetch is redirected to a CDN host, so which lookup or
    // connect failed is otherwise invisible — and a resolver that works for the
    // browser (DNS-over-HTTPS, the system proxy) is not the one this uses.
    let mut response = request.call().map_err(|e| {
        Error::Engine(format!(
            "could not download speech model from {url}: {e} (Kerf resolves names through the OS resolver and \
             honours HTTPS_PROXY, not the browser's DNS or proxy settings; KERF_WHISPER_MODEL_URL points it at a mirror)"
        ))
    })?;

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Engine(format!("could not download speech model: HTTP {status}")));
    }
    // 206 means the server honoured the range and we append; anything else (a
    // plain 200) restarts the file from scratch.
    let resuming = have > 0 && status == 206;
    let offset = if resuming { have } else { 0 };
    let total = response.body().content_length().map(|len| len + offset);

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resuming)
        .truncate(!resuming)
        .open(tmp)
        .map_err(|e| Error::Engine(format!("could not open model download file: {e}")))?;

    let mut reader = response.body_mut().as_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut downloaded = offset;
    progress(DownloadProgress { downloaded, total });
    let mut last_report = downloaded;
    loop {
        if cancel() {
            // Flush what we have so the `.part` file is a usable prefix to
            // resume from rather than however much happened to reach the OS.
            let _ = file.flush();
            return Err(Error::Cancelled);
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| Error::Engine(format!("speech model download failed: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| Error::Engine(format!("could not write speech model: {e}")))?;
        downloaded += n as u64;
        // Report about every megabyte — one event per 256 KB read would flood
        // the IPC channel to the webview for no extra information.
        if downloaded - last_report >= MB {
            last_report = downloaded;
            progress(DownloadProgress { downloaded, total });
        }
    }
    file.flush()
        .map_err(|e| Error::Engine(format!("could not write speech model: {e}")))?;
    progress(DownloadProgress {
        downloaded,
        total: total.or(Some(downloaded)),
    });

    if let Some(total) = total {
        if downloaded != total {
            return Err(Error::Engine(format!(
                "speech model download is incomplete ({downloaded} of {total} bytes)"
            )));
        }
    }
    Ok(())
}

/// Check `path` starts with the ggml container magic, so an error page saved
/// under a `.bin` name is rejected here instead of deep inside whisper.
fn verify_ggml(path: &Path) -> Result<()> {
    let mut head = [0u8; 4];
    let mut file = std::fs::File::open(path).map_err(|e| Error::Engine(format!("could not read downloaded model: {e}")))?;
    file.read_exact(&mut head)
        .map_err(|e| Error::Engine(format!("downloaded model is truncated: {e}")))?;
    // whisper.cpp writes the magic as a little-endian u32 of "ggml", which lands
    // on disk as "lmgg"; accept either byte order.
    if &head == b"lmgg" || &head == b"ggml" {
        Ok(())
    } else {
        Err(Error::Engine(
            "downloaded speech model is not a ggml file (the download URL may be wrong or behind a login)".to_string(),
        ))
    }
}

// ---- transcription via the ffmpeg `whisper` filter --------------------------

/// Whether the `ffmpeg` binary on this machine has the `whisper` filter, probed
/// once and cached. FFmpeg gained it in 8.0 and only builds it with
/// `--enable-whisper`, so most installs answer `false` and Kerf falls back to
/// the in-process `whisper` feature.
pub fn filter_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let bin = ffmpeg_bin();
        let ok = command(&bin)
            .args(["-hide_banner", "-loglevel", "quiet", "-h", "filter=whisper"])
            .stdin(Stdio::null())
            .output()
            .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("whisper AVOptions"))
            .unwrap_or(false);
        tracing::debug!(available = ok, "probed ffmpeg for the whisper filter");
        ok
    })
}

/// How much audio the filter buffers before running one inference pass, in
/// seconds. The filter's own default is 3, which chops speech into three-second
/// windows and transcribes each with no surrounding context — whisper's accuracy
/// collapses. 30 s is the window the model was trained on.
const QUEUE_SECS: u32 = 30;

/// Escape a value for use inside an ffmpeg filter argument list.
fn escape_filter_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '\'' | ':' | ',' | ';' | '=' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Build the ffmpeg argument list that transcribes `input` into an SRT file at
/// `destination`. Pure so it can be unit-tested without the filter present.
///
/// `model` and `destination` are **bare file names**: the caller runs ffmpeg
/// with its working directory set to the folder holding both. Filter arguments
/// use `:` as their separator and `\` as an escape, so an absolute path (above
/// all a Windows `C:\...` one) has to be escaped into unreadability to survive
/// the parser — sidestepping it with the process's working directory keeps the
/// graph legible and removes a whole class of quoting bug.
fn build_transcribe_args(input: &str, model: &str, destination: &str, language: Option<&str>) -> Vec<String> {
    let filter = format!(
        // The filter only accepts 16 kHz mono float, so constrain the format
        // explicitly rather than relying on lavfi's auto-inserted resampler.
        "aresample=16000,aformat=sample_fmts=flt:channel_layouts=mono,whisper=model={model}:language={language}:queue={QUEUE_SECS}:destination={destination}:format=srt",
        model = escape_filter_value(model),
        language = escape_filter_value(language.unwrap_or("auto")),
        destination = escape_filter_value(destination),
    );
    vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-i".to_string(),
        input.to_string(),
        "-vn".to_string(),
        "-map".to_string(),
        "0:a:0".to_string(),
        "-af".to_string(),
        filter,
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ]
}

/// Transcribe `path`'s first audio stream with the ffmpeg `whisper` filter,
/// using the ggml model at `model`. `duration` (the asset's length in seconds)
/// only scales the reported progress; pass `0.0` if it is unknown.
///
/// Requires [`filter_available`]; callers pick this backend only when it is.
pub fn transcribe(
    path: &Path,
    model: &Path,
    language: Option<&str>,
    duration: f64,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<TranscriptSegment>> {
    let model_dir = model
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let model_file = model
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Engine("speech model path has no file name".to_string()))?;

    // The SRT lands next to the model, in Kerf's own cache directory, so the
    // whole ffmpeg invocation can run with that as its working directory and
    // neither path has to survive filter-argument escaping.
    let out_name = format!("kerf-transcript-{}-{:x}.srt", std::process::id(), fnv_time());
    let out_path = model_dir.join(&out_name);
    let _cleanup = TempFile(out_path.clone());

    // ffmpeg resolves a relative input against its working directory, which we
    // are about to move; make the input absolute so it still points at the media.
    let input = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let input = input
        .to_str()
        .ok_or_else(|| Error::Engine("asset path is not valid UTF-8".to_string()))?
        .to_string();

    let mut args = build_transcribe_args(&input, model_file, &out_name, language);
    let bin = ffmpeg_bin();
    // Minutes of inference over the whole file — the single heaviest thing an
    // analysis pass does, and the one worth keeping off the user's other cores.
    let cpu = cpu::lease();
    cpu::limit_args(&mut args, cpu.threads());
    tracing::info!(path = %path.display(), model = %model.display(), "transcribing with the ffmpeg whisper filter");

    let mut child = bg_command(&bin)
        .current_dir(&model_dir)
        .arg("-progress")
        .arg("pipe:1")
        .arg("-stats_period")
        .arg("1")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;

    // Drain stderr on a side thread: whisper logs steadily, and a full pipe
    // would deadlock the stdout progress read.
    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    let stdout = child.stdout.take().expect("stdout piped");
    // `-stats_period 1` makes ffmpeg write a progress block every second, so the
    // cancel is polled about that often — inference on a long take runs for
    // minutes and is the one wait worth being able to abandon.
    let mut cancelled = false;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if cancel() {
            cancelled = true;
            let _ = child.kill();
            break;
        }
        if line == "progress=end" {
            break;
        }
        if duration > 0.0 {
            if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<i64>().ok()) {
                progress((us.max(0) as f64 / 1_000_000.0 / duration).clamp(0.0, 1.0));
            }
        }
    }

    let status = child.wait().map_err(|e| Error::Engine(format!("ffmpeg wait failed: {e}")))?;
    let stderr_text = stderr_handle.join().unwrap_or_default();
    if cancelled {
        return Err(Error::Cancelled);
    }
    if !status.success() {
        let mut tail: Vec<&str> = stderr_text.lines().rev().take(12).collect();
        tail.reverse();
        return Err(Error::Engine(format!(
            "whisper transcription failed: {}",
            tail.join("\n").trim()
        )));
    }
    progress(1.0);

    // No output file at all means the filter ran but never produced a segment —
    // silent or music-only audio, which is an empty transcript, not an error.
    let srt = match std::fs::read_to_string(&out_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Error::Engine(format!("could not read transcript output: {e}"))),
    };
    Ok(parse_srt(&srt))
}

/// Deletes its path on drop, so a failed or panicking transcription doesn't
/// leave stray SRT files in the cache directory.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A cheap per-call nonce for temp file names (two transcriptions in one process
/// must not collide on a shared destination).
fn fnv_time() -> u64 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ (n.wrapping_mul(0x9e37_79b9_7f4a_7c15))
}

/// Parse SubRip into transcript segments, ignoring malformed entries.
///
/// This reads what the `whisper` filter wrote, so it only has to cope with that
/// dialect — but it is deliberately lenient (blank-line separated blocks, an
/// optional numeric index, `-->` timings, one or more text lines) because a
/// single odd block should cost one line of transcript, not the whole run.
pub fn parse_srt(text: &str) -> Vec<TranscriptSegment> {
    let mut segments = Vec::new();
    for block in text.split("\n\n") {
        let block = block.replace("\r\n", "\n");
        let mut lines = block.trim().lines().filter(|l| !l.trim().is_empty());
        let Some(first) = lines.next() else { continue };
        // The index line is optional; when the first line has no arrow it is one.
        let (timing, body_first) = if first.contains("-->") {
            (first, None)
        } else {
            match lines.next() {
                Some(second) if second.contains("-->") => (second, None),
                other => (first, other),
            }
        };
        let Some((start, end)) = parse_srt_timing(timing) else {
            continue;
        };
        let text: String = body_first
            .into_iter()
            .chain(lines)
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" ");
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        segments.push(TranscriptSegment { start, end, text });
    }
    segments
}

fn parse_srt_timing(line: &str) -> Option<(f64, f64)> {
    let (a, b) = line.split_once("-->")?;
    Some((parse_srt_time(a.trim())?, parse_srt_time(b.trim())?))
}

/// `HH:MM:SS,mmm` (SubRip's comma) or `HH:MM:SS.mmm`, with the hours optional.
fn parse_srt_time(s: &str) -> Option<f64> {
    let s = s.split_whitespace().next()?.replace(',', ".");
    let mut secs = 0.0;
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    for part in &parts {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_filters_srt_output() {
        let srt = "1\n00:00:00,000 --> 00:00:02,480\n Hello there.\n\n\
                   2\n00:00:02,480 --> 00:00:05,000\n General Kenobi.\n\n";
        let segs = parse_srt(srt);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "Hello there.");
        assert!((segs[0].end - 2.48).abs() < 1e-9);
        assert_eq!(segs[1].text, "General Kenobi.");
        assert!((segs[1].start - 2.48).abs() < 1e-9);
    }

    #[test]
    fn joins_multi_line_cues_and_skips_junk() {
        let srt = "1\n00:00:01,000 --> 00:00:03,000\nfirst line\nsecond line\n\n\
                   nonsense block\n\n\
                   3\n00:01:00,500 --> 00:01:02,000\nlater\n";
        let segs = parse_srt(srt);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "first line second line");
        assert!((segs[1].start - 60.5).abs() < 1e-9);
    }

    #[test]
    fn parses_crlf_and_hour_less_timings() {
        let srt = "00:01,000 --> 00:02,000\r\nno index here\r\n";
        let segs = parse_srt(srt);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "no index here");
        assert!((segs[0].start - 1.0).abs() < 1e-9);
    }

    #[test]
    fn empty_output_is_an_empty_transcript() {
        assert!(parse_srt("").is_empty());
        assert!(parse_srt("\n\n\n").is_empty());
    }

    #[test]
    fn transcribe_args_keep_paths_out_of_the_filter_graph() {
        let args = build_transcribe_args("/media/take 1.mp4", "ggml-base.bin", "out.srt", Some("en"));
        let filter = args.iter().find(|a| a.contains("whisper=")).expect("whisper filter");
        assert!(filter.contains("model=ggml-base.bin"));
        assert!(filter.contains("destination=out.srt"));
        assert!(filter.contains("language=en"));
        assert!(filter.contains("format=srt"));
        assert!(filter.starts_with("aresample=16000,aformat="));
        // The input path is an argv entry, never part of the graph.
        assert!(args.contains(&"/media/take 1.mp4".to_string()));
        assert!(!filter.contains("take 1.mp4"));
    }

    #[test]
    fn transcribe_args_default_to_auto_language() {
        let args = build_transcribe_args("in.mp4", "ggml-base.bin", "out.srt", None);
        assert!(args.iter().any(|a| a.contains("language=auto")));
    }

    #[test]
    fn filter_values_escape_separators() {
        assert_eq!(escape_filter_value("ggml-base.bin"), "ggml-base.bin");
        assert_eq!(escape_filter_value("odd:name,1.bin"), "odd\\:name\\,1.bin");
    }

    /// `set_model` is process-global, so this test owns it start to finish and
    /// puts it back; it must be the only one touching the override.
    #[test]
    fn the_runtime_choice_outranks_the_default_and_can_be_a_path() {
        set_model(Some("small.en"));
        assert!(matches!(configured_model(), ModelChoice::Named(n) if n == "small.en"));

        // Anything path-shaped is taken literally rather than looked up.
        set_model(Some("/models/my-own.bin"));
        assert!(matches!(configured_model(), ModelChoice::File(p) if p.ends_with("my-own.bin")));

        // Clearing falls back (no env var is set under the test harness).
        set_model(None);
        assert!(matches!(configured_model(), ModelChoice::Named(n) if n == DEFAULT_MODEL));
    }

    #[test]
    fn a_bare_name_selects_a_cached_model_and_a_path_is_literal() {
        // Not using env vars here (they are process-global and racy under a test
        // harness) — just the shape of what `configured_model` decides.
        assert!(model_info("base").is_some());
        assert!(model_info("nope").is_none());
        assert!(model_path("base").is_none_or(|p| p.ends_with("ggml-base.bin")));
    }

    /// A one-connection-at-a-time HTTP/1.1 server over a byte slice, enough to
    /// exercise [`stream_to_file`] without the network: it honours (or
    /// deliberately ignores) `Range`, and can under-deliver against its own
    /// `Content-Length` to stand in for a dropped connection.
    fn spawn_server(body: Vec<u8>, ranges: bool, short_by: usize) -> String {
        use std::io::{BufRead, BufReader, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = format!("http://{}/model.bin", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let Ok(peek) = stream.try_clone() else { break };
                let mut reader = BufReader::new(peek);
                let mut start = 0usize;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        if ranges {
                            start = v.trim().trim_end_matches('-').parse().unwrap_or(0);
                        }
                    }
                }
                let slice = &body[start.min(body.len())..];
                let status = if start > 0 { "206 Partial Content" } else { "200 OK" };
                let declared = slice.len();
                let header = format!("HTTP/1.1 {status}\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&slice[..declared.saturating_sub(short_by)]);
                let _ = stream.flush();
            }
        });
        addr
    }

    /// A body big enough to cross the progress-reporting threshold twice.
    fn fake_model(len: usize) -> Vec<u8> {
        let mut body = b"lmgg".to_vec();
        body.extend((0..len - 4).map(|i| (i % 251) as u8));
        body
    }

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kerf-model-test-{}-{tag}-{:x}.part", std::process::id(), fnv_time()))
    }

    #[test]
    fn streams_a_download_and_reports_progress() {
        let body = fake_model(3 * 1024 * 1024);
        let url = spawn_server(body.clone(), true, 0);
        let tmp = temp_path("plain");
        let _cleanup = TempFile(tmp.clone());

        let mut seen = Vec::new();
        stream_to_file(&url, &tmp, &mut |p| seen.push(p), &|| false).expect("download");

        assert_eq!(std::fs::read(&tmp).expect("read"), body);
        verify_ggml(&tmp).expect("magic");
        assert_eq!(seen.last().expect("progress").fraction(), Some(1.0));
        // Intermediate reports, not just the bookends.
        assert!(seen.len() > 2, "expected streaming progress, got {seen:?}");
    }

    /// Cancelling mid-download has to leave something worth resuming: the whole
    /// point of abandoning a 148 MB fetch is not paying for it twice.
    #[test]
    fn a_cancelled_download_keeps_a_resumable_part_file() {
        let body = fake_model(3 * 1024 * 1024);
        let url = spawn_server(body.clone(), true, 0);
        let tmp = temp_path("cancel");
        let _cleanup = TempFile(tmp.clone());

        // Cancel as soon as the first megabyte has been reported.
        let mut seen = 0u64;
        let cancelled = std::cell::Cell::new(false);
        let err = stream_to_file(
            &url,
            &tmp,
            &mut |p| {
                seen = p.downloaded;
                if seen > 0 {
                    cancelled.set(true);
                }
            },
            &|| cancelled.get(),
        )
        .expect_err("cancelled");

        assert!(matches!(err, Error::Cancelled), "expected Cancelled, got {err:?}");
        let on_disk = std::fs::metadata(&tmp).expect("part file").len();
        assert!(
            on_disk > 0 && on_disk < body.len() as u64,
            "expected a partial file, got {on_disk}"
        );
        assert_eq!(std::fs::read(&tmp).expect("read"), body[..on_disk as usize]);
    }

    #[test]
    fn resumes_a_partial_download_instead_of_restarting() {
        let body = fake_model(2 * 1024 * 1024);
        let url = spawn_server(body.clone(), true, 0);
        let tmp = temp_path("resume");
        let _cleanup = TempFile(tmp.clone());
        // A previous run got half way before dying.
        std::fs::write(&tmp, &body[..body.len() / 2]).expect("seed");

        let mut first = None;
        stream_to_file(
            &url,
            &tmp,
            &mut |p| {
                first.get_or_insert(p);
            },
            &|| false,
        )
        .expect("resume");

        assert_eq!(std::fs::read(&tmp).expect("read"), body);
        // The resumed run starts its count from what was already on disk, so the
        // reported fraction picks up mid-way rather than restarting at zero.
        let first = first.expect("progress");
        assert_eq!(first.downloaded, (body.len() / 2) as u64);
        assert_eq!(first.total, Some(body.len() as u64));
    }

    #[test]
    fn a_server_that_ignores_range_restarts_the_file() {
        let body = fake_model(512 * 1024);
        let url = spawn_server(body.clone(), false, 0);
        let tmp = temp_path("norange");
        let _cleanup = TempFile(tmp.clone());
        // Stale bytes that must not be prepended to the fresh 200 response.
        std::fs::write(&tmp, vec![0xffu8; 4096]).expect("seed");

        stream_to_file(&url, &tmp, &mut |_| {}, &|| false).expect("download");
        assert_eq!(std::fs::read(&tmp).expect("read"), body);
    }

    #[test]
    fn a_truncated_download_is_an_error_not_a_model() {
        let body = fake_model(256 * 1024);
        let url = spawn_server(body, true, 4096);
        let tmp = temp_path("short");
        let _cleanup = TempFile(tmp.clone());

        assert!(stream_to_file(&url, &tmp, &mut |_| {}, &|| false).is_err());
    }

    #[test]
    fn a_downloaded_error_page_is_rejected() {
        let tmp = temp_path("html");
        let _cleanup = TempFile(tmp.clone());
        std::fs::write(&tmp, b"<!DOCTYPE html><title>404</title>").expect("write");
        let err = verify_ggml(&tmp).expect_err("not a model");
        assert!(err.to_string().contains("not a ggml file"), "{err}");
    }

    /// Hits the network and writes ~75 MB into the model cache, so it is not part
    /// of the normal run:
    /// `cargo test -p kerf-core --no-default-features -- --ignored downloads_a_real_model`
    #[test]
    #[ignore = "downloads a real model over the network"]
    fn downloads_a_real_model() {
        let mut seen = Vec::new();
        let path = download_model("tiny", &mut |p| seen.push(p)).expect("download");
        assert!(path.is_file());
        verify_ggml(&path).expect("a real ggml file");
        assert!(seen.last().is_some_and(|p| p.fraction() == Some(1.0)), "{seen:?}");
        // A second call is a cache hit, not a second download.
        let again = download_model("tiny", &mut |_| panic!("should not re-download")).expect("cached");
        assert_eq!(again, path);
    }

    #[test]
    fn download_progress_fraction_is_bounded() {
        let p = DownloadProgress {
            downloaded: 50,
            total: Some(100),
        };
        assert_eq!(p.fraction(), Some(0.5));
        assert_eq!(
            DownloadProgress {
                downloaded: 5,
                total: None
            }
            .fraction(),
            None
        );
        assert_eq!(
            DownloadProgress {
                downloaded: 5,
                total: Some(0)
            }
            .fraction(),
            None
        );
    }
}
