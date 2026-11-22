//! Insta360 dual-lens files stitched into one equirectangular video.

use super::*;

// ---- insta360 dual-lens stitching ------------------------------------------

/// The equirect frame a lens pair is stitched into. Each lens is a square
/// fisheye covering [`STITCH_FOV`] degrees, so a 3072x3072 lens carries about
/// 16 px per degree — roughly 5800 px around the full circle. 5760x2880 is
/// therefore the size that keeps the capture's detail without inventing any
/// (and is what Insta360 Studio itself exports a 5.7K capture at).
const STITCH_WIDTH: u32 = 5760;
const STITCH_HEIGHT: u32 = 2880;

/// Per-lens field of view, in degrees. Insta360's lenses overshoot the
/// hemisphere so the two circles overlap at the seam; `v360` needs the real
/// figure or the halves meet with a gap.
const STITCH_FOV: u32 = 190;

/// The lens token of an Insta360 capture file name and the name of its other
/// lens: a capture is written as a *pair* of files whose second-to-last
/// underscore-separated token is `00` (front) or `10` (rear) —
/// `VID_20220625_140410_00_008.mp4` / `..._10_008.mp4`.
///
/// Matched token-wise rather than by searching for `_00_`, so a capture whose
/// date or time happens to contain those digits can't be misread.
pub(crate) fn insta360_lens(file_name: &str) -> Option<(&'static str, String)> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("mp4") || !stem.starts_with("VID_") {
        return None;
    }
    // `VID_<date>_<time>_<lens>_<sequence>` — anything shorter, or with an empty
    // trailing sequence, is not a capture file whatever its middle tokens say.
    let mut tokens: Vec<&str> = stem.split('_').collect();
    if tokens.len() < 4 || tokens.last().is_none_or(|t| t.is_empty()) {
        return None;
    }
    let lens_at = tokens.len() - 2;
    let (lens, other) = match tokens[lens_at] {
        "00" => ("00", "10"),
        "10" => ("10", "00"),
        _ => return None,
    };
    tokens[lens_at] = other;
    Some((lens, format!("{}.{ext}", tokens.join("_"))))
}

/// The display name for a stitched pair: the capture name with the lens token
/// dropped, so the bin shows one `VID_20220625_140410_008.mp4` rather than
/// whichever lens file happened to be imported.
pub(crate) fn insta360_pair_name(file_name: &str) -> Option<String> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    insta360_lens(file_name)?;
    let mut tokens: Vec<&str> = stem.split('_').collect();
    tokens.remove(tokens.len() - 2);
    Some(format!("{}.{ext}", tokens.join("_")))
}

/// The `(front, rear)` lens files of the Insta360 capture `path` belongs to, if
/// it is one: a square video frame (one fisheye circle per file) whose sibling
/// lens is on disk next to it. Either half resolves to the same canonical pair,
/// so importing either file stitches — and caches — the same sphere.
pub fn insta360_pair(path: &Path, width: Option<u32>, height: Option<u32>) -> Option<(PathBuf, PathBuf)> {
    let (w, h) = (width?, height?);
    if w == 0 || w != h {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let (lens, sibling_name) = insta360_lens(name)?;
    let sibling = path.with_file_name(sibling_name);
    if !sibling.is_file() {
        return None;
    }
    let this = path.to_path_buf();
    Some(if lens == "00" { (this, sibling) } else { (sibling, this) })
}

/// Cache key for a stitched pair: both lens files' identity plus a version salt,
/// so changing the stitch recipe below invalidates everything stitched by an
/// older build instead of silently reusing it.
fn stitch_key(front: &Path, rear: &Path) -> String {
    format!("{}||{}||v1", source_key(front), source_key(rear))
}

/// Where the stitched equirect for a lens pair lives (whether or not it exists
/// yet): `<cache>/kerf/stitched/<hash>.mp4`. Alongside the proxy cache rather
/// than next to the originals — capture media is routinely on a read-only or
/// ejected SD card, and a deterministic key lets a re-import reuse the stitch
/// instead of spending minutes re-encoding it.
pub fn stitched_path(front: &Path, rear: &Path) -> Option<PathBuf> {
    let dir = crate::paths::cache_dir()?.join("kerf").join("stitched");
    Some(dir.join(format!("{:016x}.mp4", fnv1a(&stitch_key(front, rear)))))
}

/// Build the ffmpeg argument list (pure, unit-tested) that stitches the two
/// fisheye lens files into one equirectangular video at `dst`.
///
/// `hstack` packs the pair into the dual-fisheye layout `v360` expects (front
/// left), `roll=180` corrects the sensor orientation — Insta360 records both
/// lenses upside down — and `shortest` resolves the few-frames-different
/// durations the two files are written with. The front file's audio is copied
/// through untouched; its telemetry/subtitle stream is dropped.
///
/// CRF 15 rather than a lossless or visually-lossy setting: this file becomes
/// the effective source for every later reframe and export, so it must not be
/// the quality floor, while `veryfast` keeps a capture's import to minutes.
/// With a hardware `encoder` the same quality intent is mapped per family (see
/// [`quality_args`]) and the multi-minute re-encode moves onto the GPU;
/// `hw_decode` accelerates the two lens decodes the same way.
pub(crate) fn build_stitch_args(front: &str, rear: &str, dst: &str, encoder: &str, hw_decode: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
    ];
    // `-hwaccel` is an input option: emit it before each lens file's `-i`.
    for lens in [front, rear] {
        if let Some(hw) = hw_decode {
            args.push("-hwaccel".to_string());
            args.push(hw.to_string());
        }
        args.push("-i".to_string());
        args.push(lens.to_string());
    }
    args.extend([
        "-filter_complex".to_string(),
        format!(
            "[0:v][1:v]hstack=shortest=1,v360=dfisheye:e:ih_fov={STITCH_FOV}:iv_fov={STITCH_FOV}:roll=180:w={STITCH_WIDTH}:h={STITCH_HEIGHT}[v]"
        ),
        "-map".to_string(),
        "[v]".to_string(),
        "-map".to_string(),
        "0:a?".to_string(),
        "-c:v".to_string(),
        encoder.to_string(),
    ]);
    args.extend(quality_args(encoder, 15));
    args.extend([
        "-pix_fmt".to_string(),
        encode_pix_fmt(encoder).to_string(),
        "-c:a".to_string(),
        "copy".to_string(),
        "-shortest".to_string(),
        // The encode writes a `.part` temp file, whose extension tells ffmpeg
        // nothing — name the muxer instead of letting it guess.
        "-f".to_string(),
        "mp4".to_string(),
        dst.to_string(),
    ]);
    args
}

/// The hardware encoder for stitching, when available: an **HEVC** one, because
/// the 5760-wide equirect frame exceeds H.264 NVENC's 4096-wide ceiling while
/// every HEVC hardware encoder handles 8K. `None` falls back to libx264.
fn stitch_hw_encoder() -> Option<&'static str> {
    if !HW_ENCODE_OK.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    HW_ENCODER_CANDIDATES
        .iter()
        .find(|e| e.starts_with("hevc_") && hw_encoders().iter().any(|h| h == *e))
        .copied()
}

/// The stitches in flight, by the cache file each is making. Importing both lens files at once
/// (the obvious thing to do in a file dialog) would otherwise run the same multi-minute encode
/// twice and throw one away; the loser of the race waits for the claim and then finds the
/// finished file in the cache. A claim is a *name in a set*, not a lock held across the encode:
/// the mutex is taken for an insert or a remove, never while ffmpeg runs.
struct Stitching {
    running: Mutex<std::collections::HashSet<PathBuf>>,
    finished: std::sync::Condvar,
}

fn stitching() -> &'static Stitching {
    static STITCHING: OnceLock<Stitching> = OnceLock::new();
    STITCHING.get_or_init(|| Stitching {
        running: Mutex::new(std::collections::HashSet::new()),
        finished: std::sync::Condvar::new(),
    })
}

/// A pair's stitch claim; dropping it lets the next waiter go.
pub(super) struct StitchClaim(PathBuf);

impl Drop for StitchClaim {
    fn drop(&mut self) {
        let stitching = stitching();
        stitching.running.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
        stitching.finished.notify_all();
    }
}

/// Claim the right to make `dst`, waiting (in slices, so `cancel` is heard) while another
/// stitch of the same pair holds it. `None`: it finished while this one waited, and `dst` is
/// there to use.
pub(super) fn claim_stitch(dst: &Path, cancel: &dyn Fn() -> bool) -> Result<Option<StitchClaim>> {
    let stitching = stitching();
    let mut running = stitching.running.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if dst.is_file() {
            return Ok(None);
        }
        if running.insert(dst.to_path_buf()) {
            return Ok(Some(StitchClaim(dst.to_path_buf())));
        }
        if cancel() {
            return Err(Error::Cancelled);
        }
        running = stitching
            .finished
            .wait_timeout(running, std::time::Duration::from_millis(100))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// Stitch an Insta360 lens pair into a single equirectangular file, returning
/// its cached path; a pair already stitched returns immediately. `duration_hint`
/// (the capture's length in seconds) scales the `progress` bar. Blocking and
/// slow — a full re-encode — so callers run it off the project lock.
///
/// Like the proxy cache, the encode writes a per-process temp file and renames
/// it into place, so an interrupted or concurrent stitch never leaves a
/// half-written file behind for the next import to mistake for a finished one.
pub fn stitch_insta360(
    front: &Path,
    rear: &Path,
    duration_hint: f64,
    progress: &mut dyn FnMut(ExportProgress),
) -> Result<PathBuf> {
    stitch_insta360_with(front, rear, duration_hint, progress, &|| false)
}

/// [`stitch_insta360`] that can be abandoned: `cancel` is polled while the stitch waits for the
/// same pair's other stitch, while it waits for the heavy-job slot, and while ffmpeg encodes
/// (about twice a second); once true the encode is killed, its temp file removed and
/// [`Error::Cancelled`] returned. A stalled encode (no `-progress` for [`EXPORT_STALL`]) is an
/// error, as in an export.
pub fn stitch_insta360_with(
    front: &Path,
    rear: &Path,
    duration_hint: f64,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    let dst = stitched_path(front, rear)
        .ok_or_else(|| Error::Engine("no cache directory available for stitched 360 media".to_string()))?;
    if dst.is_file() {
        return Ok(dst);
    }

    // The same pair's other stitch holds the claim for minutes: wait for it in slices, so a Stop
    // works on the one that is only waiting, and take its result when it lands.
    let Some(_claim) = claim_stitch(&dst, cancel)? else {
        return Ok(dst);
    };

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Engine(format!("could not create stitch cache dir: {e}")))?;
    }
    let (Some(front_str), Some(rear_str)) = (front.to_str(), rear.to_str()) else {
        return Err(Error::Engine("lens file path is not valid UTF-8".to_string()));
    };
    let tmp = dst.with_extension(format!("{}.part", std::process::id()));
    let tmp_str = tmp
        .to_str()
        .ok_or_else(|| Error::Engine("stitch temp path is not valid UTF-8".to_string()))?;

    tracing::info!(front = %front.display(), rear = %rear.display(), "stitching insta360 lens pair");
    let attempt = |encoder: &str, hw_decode: Option<&str>, progress: &mut dyn FnMut(ExportProgress)| {
        let args = build_stitch_args(front_str, rear_str, tmp_str, encoder, hw_decode);
        run_ffmpeg_progress(
            &args,
            &dst,
            Bar {
                total: duration_hint.max(1e-9),
                offset: 0.0,
                width: 1.0,
                start: std::time::Instant::now(),
            },
            progress,
            cancel,
        )
    };
    // GPU-encode the stitch when a verified HEVC hardware encoder exists (the
    // full re-encode drops from minutes towards realtime); one failure falls
    // back to the software pipeline and disables hardware encodes.
    let hw_enc = stitch_hw_encoder();
    let hw_dec = decode_hwaccel();
    let mut result = attempt(hw_enc.unwrap_or("libx264"), hw_dec.as_deref(), progress);
    if result.is_err() && (hw_enc.is_some() || hw_dec.is_some()) {
        tracing::warn!(error = ?result.as_ref().err(), "hardware-accelerated stitch failed; retrying in software");
        if hw_enc.is_some() {
            HW_ENCODE_OK.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        let _ = std::fs::remove_file(&tmp);
        result = attempt("libx264", None, progress);
    }
    // A cancelled run is not a finished one: `run_ffmpeg_progress` reports it as `Ok`, and its
    // half-written file must not be renamed into the cache for the next import to find.
    if !matches!(result, Ok(RenderStatus::Completed)) {
        let _ = std::fs::remove_file(&tmp);
    }
    if matches!(result, Ok(RenderStatus::Cancelled)) {
        return Err(Error::Cancelled);
    }
    result?;
    if dst.is_file() {
        let _ = std::fs::remove_file(&tmp);
        return Ok(dst);
    }
    std::fs::rename(&tmp, &dst).map_err(|e| Error::Engine(format!("could not finalize stitched 360 media: {e}")))?;
    Ok(dst)
}
