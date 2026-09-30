//! Fetching the large files Kerf downloads on first use — speech models, the
//! voiceover model and its runtime — into the OS cache directory.
//!
//! Every one of them is tens to hundreds of megabytes and optional, so the
//! shape is the same for all: stream into a `.part` file beside the
//! destination, resume a leftover one with a range request, poll a cancel
//! callback per chunk (keeping the partial file so the next attempt resumes
//! it), check the result really is the file it claims to be, and only then
//! rename it into place — so a truncated or interrupted fetch can never be
//! loaded later as if it were complete.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

const MB: u64 = 1024 * 1024;

/// How far a download has got.
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

/// One file to fetch.
pub struct Download<'a> {
    pub url: &'a str,
    pub dst: &'a Path,
    /// What the file is, for messages: `"speech model"`.
    pub what: &'a str,
    /// The environment variable that points this download at a mirror, named
    /// in the error a failed connection produces.
    pub mirror_env: &'a str,
    /// Rejects a file that is not what was asked for — an HTML error page, a
    /// truncated CDN response — before it is renamed into place.
    pub verify: &'a dyn Fn(&Path) -> Result<()>,
}

/// Fetch `download.url` to `download.dst` unless it is already there, returning
/// the destination.
pub fn fetch(download: &Download, progress: &mut dyn FnMut(DownloadProgress), cancel: &dyn Fn() -> bool) -> Result<PathBuf> {
    let Download { url, dst, what, .. } = *download;
    if dst.is_file() {
        return Ok(dst.to_path_buf());
    }
    let parent = dst
        .parent()
        .ok_or_else(|| Error::Engine(format!("{what} cache path has no parent")))?;
    std::fs::create_dir_all(parent).map_err(|e| Error::Engine(format!("could not create {what} cache dir: {e}")))?;

    let tmp = dst.with_extension(format!("{}.part", std::process::id()));
    tracing::info!(what, %url, "downloading");
    // A cancel keeps the `.part` file: it is a valid prefix of the download,
    // and the next attempt resumes it with a range request. Any other failure
    // is a file we can't trust, so it goes.
    stream_to_file(download, &tmp, progress, cancel).inspect_err(|e| {
        if !matches!(e, Error::Cancelled) {
            let _ = std::fs::remove_file(&tmp);
        }
    })?;
    (download.verify)(&tmp).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;

    // Another process may have finished the same download meanwhile.
    if dst.is_file() {
        let _ = std::fs::remove_file(&tmp);
        return Ok(dst.to_path_buf());
    }
    std::fs::rename(&tmp, dst).map_err(|e| Error::Engine(format!("could not finalize {what} download: {e}")))?;
    tracing::info!(what, path = %dst.display(), "download ready");
    Ok(dst.to_path_buf())
}

/// Stream `download.url` into `tmp`, resuming a partial file when one is there.
pub(super) fn stream_to_file(
    download: &Download,
    tmp: &Path,
    progress: &mut dyn FnMut(DownloadProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    let Download {
        url, what, mirror_env, ..
    } = *download;
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
            "could not download {what} from {url}: {e} (Kerf resolves names through the OS resolver and \
             honours HTTPS_PROXY, not the browser's DNS or proxy settings; {mirror_env} points it at a mirror)"
        ))
    })?;

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Engine(format!("could not download {what}: HTTP {status}")));
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
        .map_err(|e| Error::Engine(format!("could not open {what} download file: {e}")))?;

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
            .map_err(|e| Error::Engine(format!("{what} download failed: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| Error::Engine(format!("could not write {what}: {e}")))?;
        downloaded += n as u64;
        // Report about every megabyte — one event per 256 KB read would flood
        // the IPC channel to the webview for no extra information.
        if downloaded - last_report >= MB {
            last_report = downloaded;
            progress(DownloadProgress { downloaded, total });
        }
    }
    file.flush()
        .map_err(|e| Error::Engine(format!("could not write {what}: {e}")))?;
    progress(DownloadProgress {
        downloaded,
        total: total.or(Some(downloaded)),
    });

    if let Some(total) = total {
        if downloaded != total {
            return Err(Error::Engine(format!(
                "{what} download is incomplete ({downloaded} of {total} bytes)"
            )));
        }
    }
    Ok(())
}
