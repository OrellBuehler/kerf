//! Stabilization: what the camera did, measured once per clip, and the correction each frame gets.
//!
//! A stabilized clip ([`crate::model::Stabilize`]) is moved frame by frame against its camera's
//! shake. **vid.stab** (FFmpeg's `vidstabdetect` / `vidstabtransform`, built with libvidstab) does it
//! in two passes: the first measures how each frame moved from the one before, the second smooths
//! the camera's path and moves every frame onto the smooth one. FFmpeg's own `deshake` needs no
//! first pass and is the fallback where vid.stab is missing ([`crate::engine`]'s probe, once).
//!
//! **The measurement is a cache, not an edit.** The analysis of a clip's source window (plus
//! [`RANGE_MARGIN`] either side) is a [`MotionAnalysis`] in
//! `<cache>/kerf/stabilize/<asset id>/<digest>.json`: the time of every frame read, and vid.stab's
//! relative motion of each — the transforms its `vidstabtransform` computes from the first pass's
//! local motions, read back from its `debug` file. The original's path, size and modified time are
//! kept with it, so a replaced file is not corrected with another's motion. Trims and splits inside
//! the window keep using it; a clip that moved past it needs analysing again ([`StabilizeStatus`]).
//!
//! **The correction is computed here, not by vid.stab's second pass** — that is what lets a preview
//! that starts in the middle of a clip, a still, a proxy and the export all move a frame the same
//! way. vid.stab smooths over the whole stream it is given and makes its zoom from all of it, so a
//! stream cut at the playhead (a preview) or one frame long (a still) would get another correction.
//! Instead [`corrections`] smooths the clip's path once, over the whole analysis, exactly as
//! vid.stab's gaussian camera path does (pinned against it by an `#[ignore]`d render), and each
//! graph hands `vidstabtransform` only the **absolute** corrections of the frames it reads
//! (`relative=0:smoothing=0`, vid.stab's old transforms format, sub-pixel numbers), scaled to the
//! frame size it decodes — a proxy's too. The zoom is fixed for the clip (the setting's, or the
//! least that hides the edges over the clip's own frames), never vid.stab's per-stream optimum.
//!
//! **Which frame is which.** vid.stab numbers frames from the first it is given, so the graph must
//! know which frame of the analysis its stream opens on. It does not trust the seek for that: a
//! clip's input is opened [`SEEK_PAD`] frames early (`clip_timing::clip_input_seek`), and the chain
//! trims at the midpoint between the analysis' frames, half a frame from any of them, so the first
//! frame kept is the one the slice of corrections starts with — on the proxy too, whose frame times
//! may sit a tick off the original's.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{Clip, Stabilize, MAX_STABILIZE_ZOOM};

/// The shape of a stored analysis; one of another number is analysed again.
pub const ANALYSIS_VERSION: u32 = 1;

/// Seconds of footage analysed past each end of a clip's window: the smoothing reads up to a second
/// either side of a frame, and a trim or a transition's tail may reach a little further.
pub const RANGE_MARGIN: f64 = 2.0;

/// The longest window one analysis may cover (an hour): the detect pass keeps one line per frame.
pub const MAX_ANALYSIS_SECONDS: f64 = 3600.0;

/// How far before a frame a window may start and still open on it: what a time printed to the
/// microsecond, or a frame-snapped float, is off by. Far below any frame's length.
pub const FRAME_TOLERANCE: f64 = 1e-4;

/// How many source frames early a stabilized clip's input is opened, so its chain can trim at a
/// point between two frames of the analysis whatever the seek lands on (see the module docs).
pub const SEEK_PAD: f64 = 3.0;

/// What a stabilized clip's chain reads when the source's frame rate is not known.
const UNKNOWN_PERIOD: f64 = 0.1;

/// The camera's motion through a stretch of footage, as vid.stab measured it (see the module docs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MotionAnalysis {
    pub version: u32,
    /// The original file it was measured on: its path, size and modified time (`path|len|mtime`).
    pub source: String,
    /// The size of the frames measured, as decoded (turned upright).
    pub width: u32,
    pub height: u32,
    /// The source time of every frame measured, in order.
    pub times: Vec<f64>,
    /// How the camera moved into each frame from the one before, `[x, y, angle]` in pixels of the
    /// measured frame and radians (vid.stab's relative transform; its zoom is not used).
    pub motions: Vec<[f64; 3]>,
}

impl MotionAnalysis {
    /// Whether the frames measured are usable: as many motions as times, times increasing.
    pub fn is_sound(&self) -> bool {
        self.version == ANALYSIS_VERSION
            && !self.times.is_empty()
            && self.times.len() == self.motions.len()
            && self.width > 0
            && self.height > 0
            && self.times.windows(2).all(|w| w[1] > w[0])
            && self.times.iter().all(|t| t.is_finite())
            && self.motions.iter().flatten().all(|v| v.is_finite())
    }

    /// The typical length of a frame (the median spacing), or [`UNKNOWN_PERIOD`] for one frame.
    pub fn period(&self) -> f64 {
        let mut steps: Vec<f64> = self.times.windows(2).map(|w| w[1] - w[0]).collect();
        if steps.is_empty() {
            return UNKNOWN_PERIOD;
        }
        steps.sort_by(f64::total_cmp);
        steps[steps.len() / 2]
    }

    /// The index of the first frame at or after `t` (within [`FRAME_TOLERANCE`]); `len` when none is.
    pub fn first_at(&self, t: f64) -> usize {
        self.times.partition_point(|&f| f < t - FRAME_TOLERANCE)
    }

    /// Whether the clip's own source window `[start, end)` lies inside what was measured.
    pub fn covers(&self, start: f64, end: f64) -> bool {
        let (Some(&first), Some(&last)) = (self.times.first(), self.times.last()) else {
            return false;
        };
        first <= start + FRAME_TOLERANCE && last + self.period() >= end - FRAME_TOLERANCE
    }

    /// The smoothing radius, in frames, for `strength`: up to one second of frames either side.
    pub fn radius(&self, strength: f64) -> usize {
        // The rate to a thousandth: a median spacing of `k / 25` is not exactly a 25th.
        let fps = (1000.0 / self.period()).round() / 1000.0;
        (strength.clamp(0.0, 1.0) * fps).round().clamp(0.0, 500.0) as usize
    }

    /// Where `point` (a source time) falls between frames: the midpoint before frame `k`, half a
    /// frame from both (before the first, half a typical frame).
    pub fn between(&self, k: usize) -> f64 {
        match k {
            0 => self.times[0] - self.period() / 2.0,
            k if k >= self.times.len() => self.times[self.times.len() - 1] + self.period() / 2.0,
            k => (self.times[k - 1] + self.times[k]) / 2.0,
        }
    }
}

/// Each frame's correction (`[x, y, angle]`, absolute) for a camera path smoothed over `radius`
/// frames either side: vid.stab's gaussian camera path (`cameraPathGaussian`, relative transforms,
/// its zoom left out as its `vidstabtransform` leaves it), operation for operation, so a frame is
/// moved as vid.stab's own second pass would move it. `radius` 0 corrects nothing.
pub fn corrections(motions: &[[f64; 3]], radius: usize) -> Vec<[f64; 3]> {
    if radius == 0 || motions.is_empty() {
        return vec![[0.0; 3]; motions.len()];
    }
    let mut path: Vec<[f64; 3]> = Vec::with_capacity(motions.len());
    let mut t = motions[0];
    path.push(t);
    for m in &motions[1..] {
        t = [m[0] + t[0], m[1] + t[1], m[2] + t[2]];
        path.push(t);
    }
    let mu = radius;
    let s = mu * 2 + 1;
    let sigma2 = (mu as f64 / 2.0).powi(2);
    let mut kernel = vec![0.0f64; s];
    for i in 0..=mu {
        let v = (-((i as f64 - mu as f64).powi(2)) / sigma2).exp();
        kernel[i] = v;
        kernel[s - i - 1] = v;
    }
    let len = path.len() as isize;
    path.iter()
        .enumerate()
        .map(|(i, own)| {
            let mut weight = 0.0;
            let mut avg = [0.0f64; 3];
            for (k, w) in kernel.iter().enumerate() {
                let idx = i as isize + k as isize - mu as isize;
                if idx >= 0 && idx < len {
                    weight += w;
                    let p = path[idx as usize];
                    avg = [avg[0] + p[0] * w, avg[1] + p[1] * w, avg[2] + p[2] * w];
                }
            }
            let inv = 1.0 / weight;
            let avg = [avg[0] * inv, avg[1] * inv, avg[2] * inv];
            [own[0] - avg[0], own[1] - avg[1], own[2] - avg[2]]
        })
        .collect()
}

/// The least zoom, in percent, that keeps the moved edges out of view for nearly every frame of
/// `fixes` (vid.stab's static optimal zoom: the 1st and 99th percentile of the shifts, rotation left
/// out), on a `width` x `height` frame, held to [`MAX_STABILIZE_ZOOM`].
pub fn auto_zoom(fixes: &[[f64; 3]], width: u32, height: u32) -> f64 {
    if fixes.is_empty() || width == 0 || height == 0 {
        return 0.0;
    }
    let extent = |axis: usize| {
        let mut v: Vec<f64> = fixes.iter().map(|f| f[axis]).collect();
        v.sort_by(f64::total_cmp);
        let cut = v.len() / 100;
        let (lo, hi) = (v[cut], v[v.len() - 1 - cut]);
        hi.max(lo.abs()).max(0.0)
    };
    let zx = 2.0 * extent(0) / f64::from(width);
    let zy = 2.0 * extent(1) / f64::from(height);
    (100.0 * zx.max(zy)).clamp(0.0, MAX_STABILIZE_ZOOM)
}

/// vid.stab's old transforms format for `fixes`, scaled by `sx` / `sy` (a frame decoded at another
/// size than the one measured): `0 x y angle 0 0` per frame, the order being all it reads.
pub fn transforms_text(fixes: &[[f64; 3]], sx: f64, sy: f64) -> String {
    let mut out = String::with_capacity(fixes.len() * 40);
    for f in fixes {
        out.push_str(&format!("0 {:.6} {:.6} {:.9} 0 0\n", f[0] * sx, f[1] * sy, f[2]));
    }
    out
}

/// How one chain reads a stabilized clip with vid.stab: where it trims (a source time between two
/// frames of the analysis) and the corrections of the frames from there, in a file Kerf wrote.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Slice {
    /// The source time the chain trims from: between the frame before and the first it corrects.
    pub start: f64,
    /// The corrections, one line a frame from the first kept.
    pub file: PathBuf,
    /// The zoom, in percent, `vidstabtransform` enlarges every frame by.
    pub zoom: f64,
}

/// The size a stabilized clip's input decodes to and how it relates to what was measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct InputFrame {
    pub width: u32,
    pub height: u32,
}

/// The analysis of `asset_id`'s footage that covers `clip`'s source window, measured on `original`
/// when that is known (the file the input reads; `None` for a proxy, which cannot be compared).
pub(crate) fn analysis_for(asset_id: Uuid, original: Option<&Path>, start: f64, end: f64) -> Option<Arc<MotionAnalysis>> {
    #[cfg(test)]
    if let Some(found) = pinned::analysis(asset_id) {
        return found.filter(|a| a.covers(start, end));
    }
    let stamp = original.map(crate::engine::source_key);
    let dir = asset_dir(asset_id)?;
    let mut best: Option<(SystemTime, Arc<MotionAnalysis>)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some((modified, analysis)) = read_cached(&path) else {
            continue;
        };
        if stamp.as_ref().is_some_and(|s| *s != analysis.source) || !analysis.covers(start, end) {
            continue;
        }
        if best.as_ref().is_none_or(|(at, _)| modified > *at) {
            best = Some((modified, analysis));
        }
    }
    best.map(|(_, a)| a)
}

/// The vid.stab [`Slice`] a chain reading `clip` over the source `window` uses, or `None` when the
/// clip has no analysis covering it (it plays as shot until it is analysed). `input` is the frame
/// size the chain decodes when that is not the size measured (a proxy's); `original` the file it
/// reads when that is the original.
pub(crate) fn slice_for(
    clip: &Clip,
    setting: &Stabilize,
    window: (f64, f64),
    original: Option<&Path>,
    input: Option<InputFrame>,
) -> Option<Slice> {
    let setting = setting.normalized();
    let analysis = analysis_for(clip.asset_id, original, clip.source_in, clip.source_out)?;
    // A window that opens before the frames measured (a reversed clip's long tail) cannot be numbered.
    if window.0 < analysis.times[0] - FRAME_TOLERANCE {
        return None;
    }
    let k0 = analysis.first_at(window.0);
    if k0 >= analysis.times.len() {
        return None;
    }
    let k1 = (analysis.first_at(window.1) + 1).min(analysis.times.len());
    let fixes = corrections(&analysis.motions, analysis.radius(setting.strength));
    let zoom = setting.zoom.unwrap_or_else(|| {
        // The clip's own frames, whatever window this chain reads: one zoom for the clip everywhere.
        let lo = analysis.first_at(clip.source_in).min(fixes.len() - 1);
        let hi = analysis.first_at(clip.source_out).clamp(lo + 1, fixes.len());
        auto_zoom(&fixes[lo..hi], analysis.width, analysis.height)
    });
    let (sx, sy) = input.map_or((1.0, 1.0), |input| {
        (
            f64::from(input.width) / f64::from(analysis.width),
            f64::from(input.height) / f64::from(analysis.height),
        )
    });
    let file = slice_file(clip.asset_id, &fixes[k0..k1.max(k0 + 1)], sx, sy, (k0, k1))?;
    Some(Slice {
        // Half a frame before the first, or nearer to it when the frame before is far off (a dropped
        // stretch of a variable-rate file): never back past the seek, three frames before.
        start: analysis.between(k0).max(analysis.times[k0] - analysis.period() / 2.0),
        file,
        zoom: (zoom * 1000.0).round() / 1000.0,
    })
}

/// The file holding `fixes` in vid.stab's old format, written once into the cache (`None` when no
/// directory can be written: the chain then leaves the stabilization out).
fn slice_file(asset_id: Uuid, fixes: &[[f64; 3]], sx: f64, sy: f64, span: (usize, usize)) -> Option<PathBuf> {
    #[cfg(test)]
    if pinned::active() {
        return Some(PathBuf::from(format!(
            "/kerf-test/stabilize/{asset_id}-{}-{}-{:.4}x{:.4}.trf",
            span.0, span.1, sx, sy
        )));
    }
    let _ = (asset_id, span);
    static PRUNED: std::sync::Once = std::sync::Once::new();
    PRUNED.call_once(|| {
        if let Some(dir) = slices_dir() {
            prune_slices(&dir, SystemTime::now());
        }
    });
    let text = transforms_text(fixes, sx, sy);
    let name = format!("{:016x}.trf", crate::color_lut::digest(text.as_bytes()));
    crate::color_lut::write_once(&slices_dir()?, &name, text.as_bytes())
}

/// Where every analysis and slice is kept: `<cache>/kerf/stabilize`, else a private directory in
/// the temp one.
fn root_dir() -> Option<PathBuf> {
    dirs::cache_dir()
        .map(|d| d.join("kerf").join("stabilize"))
        .or_else(|| crate::fonts::private_dir_named(&std::env::temp_dir(), "kerf-stabilize"))
}

fn asset_dir(asset_id: Uuid) -> Option<PathBuf> {
    root_dir().map(|d| d.join(asset_id.to_string()))
}

fn slices_dir() -> Option<PathBuf> {
    root_dir().map(|d| d.join("slices"))
}

/// A directory the detect passes work in (their files have plain names, so no path is escaped
/// into a filter): made fresh, removed by the caller.
pub(crate) fn work_dir() -> Option<PathBuf> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = root_dir()?.join("work").join(format!("{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Keep `analysis` of `asset_id`'s footage over `range`, replacing an earlier one of the same
/// window. Written atomically, so a reader finds the whole file or none.
pub(crate) fn store(asset_id: Uuid, range: (f64, f64), analysis: &MotionAnalysis) -> crate::error::Result<PathBuf> {
    use crate::error::Error;
    let dir = asset_dir(asset_id).ok_or_else(|| Error::Engine("no directory to keep the stabilization analysis in".into()))?;
    std::fs::create_dir_all(&dir).map_err(|e| Error::Engine(format!("could not keep the stabilization analysis: {e}")))?;
    let key = format!("{}|{:.6}|{:.6}|{ANALYSIS_VERSION}", analysis.source, range.0, range.1);
    let name = format!("{:016x}.json", crate::color_lut::digest(key.as_bytes()));
    let bytes = serde_json::to_vec(analysis).map_err(|e| Error::Engine(e.to_string()))?;
    crate::color_lut::write_once(&dir, &name, &bytes)
        .ok_or_else(|| Error::Engine("could not write the stabilization analysis".into()))
}

/// Forget every analysis of `asset_id` (the asset was removed or its file replaced).
pub fn forget(asset_id: Uuid) {
    if let Some(dir) = asset_dir(asset_id) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The analyses read so far, by file: parsed once while the file stays as it was.
type Parsed = (u64, SystemTime, Arc<MotionAnalysis>);

fn read_cached(path: &Path) -> Option<(SystemTime, Arc<MotionAnalysis>)> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Parsed>>> = OnceLock::new();
    let meta = std::fs::metadata(path).ok()?;
    let (len, modified) = (meta.len(), meta.modified().ok()?);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((l, m, a)) = cache.lock().unwrap_or_else(PoisonError::into_inner).get(path) {
        if *l == len && *m == modified {
            return Some((modified, a.clone()));
        }
    }
    let analysis: MotionAnalysis = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if !analysis.is_sound() {
        return None;
    }
    let analysis = Arc::new(analysis);
    let mut map = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if map.len() >= 256 {
        map.clear();
    }
    map.insert(path.to_path_buf(), (len, modified, analysis.clone()));
    Some((modified, analysis))
}

/// A slice unused for this long is deleted on the first lookup of a run.
const SLICE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

fn prune_slices(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > SLICE_MAX_AGE);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// ---- where a clip's stabilization stands -------------------------------------------------------

/// Where a clip's stabilization stands, for the Inspector and an agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StabilizeStatus {
    /// The clip is not stabilized.
    Off,
    /// The clip cannot be: a still, a title, 360 footage, a clip of sound.
    Unsupported { reason: String },
    /// This FFmpeg has no vid.stab: the clip is steadied by `deshake`, which needs no analysis.
    Basic,
    /// It has not been analysed (or was trimmed past what was): it plays as shot until it is.
    Missing,
    /// Its analysis waits for the machine.
    Queued,
    /// Its analysis is running.
    Analyzing { fraction: f64 },
    /// Its last analysis failed; it plays as shot.
    Failed { reason: String },
    /// It is analysed: the preview and the export move every frame.
    Ready,
}

/// One clip's [`StabilizeStatus`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipStabilizeStatus {
    pub clip_id: Uuid,
    #[serde(flatten)]
    pub status: StabilizeStatus,
}

/// What an analysis of a clip is doing now, by clip (this session's: a restart forgets it).
#[derive(Debug, Clone, PartialEq)]
enum Job {
    Queued,
    Running(f64),
    Failed(String),
}

fn jobs() -> &'static Mutex<HashMap<Uuid, Job>> {
    static JOBS: OnceLock<Mutex<HashMap<Uuid, Job>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_job(clip: Uuid, job: Option<Job>) {
    let mut map = jobs().lock().unwrap_or_else(PoisonError::into_inner);
    match job {
        Some(job) => {
            map.insert(clip, job);
        }
        None => {
            map.remove(&clip);
        }
    }
}

pub(crate) fn job_queued(clip: Uuid) {
    set_job(clip, Some(Job::Queued));
}

pub(crate) fn job_progress(clip: Uuid, fraction: f64) {
    set_job(clip, Some(Job::Running(fraction.clamp(0.0, 1.0))));
}

/// The analysis of `clip` ended: done or stopped forgets it, a failure is remembered with why.
pub(crate) fn job_ended(clip: Uuid, failure: Option<String>) {
    set_job(clip, failure.map(Job::Failed));
}

/// The state this session's analysis of `clip` is in, as a status (`None`: none is running or failed).
pub(crate) fn job_status(clip: Uuid) -> Option<StabilizeStatus> {
    jobs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&clip)
        .map(|job| match job {
            Job::Queued => StabilizeStatus::Queued,
            Job::Running(f) => StabilizeStatus::Analyzing { fraction: *f },
            Job::Failed(why) => StabilizeStatus::Failed { reason: why.clone() },
        })
}

/// Test-only: analyses handed to the graph builders without a disk (the golden oracle and the
/// builders' unit tests), and slice files named instead of written.
#[cfg(test)]
pub(crate) mod pinned {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        static PINNED: RefCell<Option<HashMap<Uuid, Arc<MotionAnalysis>>>> = const { RefCell::new(None) };
    }

    pub(crate) fn active() -> bool {
        PINNED.with(|p| p.borrow().is_some())
    }

    /// `Some(found)` while pinned (found or not), `None` when the disk is to be asked.
    pub(crate) fn analysis(asset: Uuid) -> Option<Option<Arc<MotionAnalysis>>> {
        PINNED.with(|p| p.borrow().as_ref().map(|m| m.get(&asset).cloned()))
    }

    /// Run `f` with `analyses` standing in for the disk on this thread.
    pub(crate) fn with<R>(analyses: HashMap<Uuid, Arc<MotionAnalysis>>, f: impl FnOnce() -> R) -> R {
        struct Reset(Option<HashMap<Uuid, Arc<MotionAnalysis>>>);
        impl Drop for Reset {
            fn drop(&mut self) {
                PINNED.with(|p| *p.borrow_mut() = self.0.take());
            }
        }
        let _reset = Reset(PINNED.with(|p| p.borrow_mut().replace(analyses)));
        f()
    }

    /// A made-up analysis of `seconds` of footage at `fps` from 0, `width` x `height`, its camera
    /// shaking by a few pixels (a fixed pattern of the frame number, no randomness and no libm).
    pub(crate) fn shaky(seconds: f64, fps: f64, width: u32, height: u32) -> MotionAnalysis {
        let n = (seconds * fps).round().max(1.0) as usize;
        MotionAnalysis {
            version: ANALYSIS_VERSION,
            source: "pinned".into(),
            width,
            height,
            times: (0..n).map(|k| k as f64 / fps).collect(),
            motions: (0..n)
                .map(|k| {
                    // Plain arithmetic, no libm: the golden oracle's argv carries the zoom it gives.
                    let wobble = |m: usize, by: usize| ((k * by) % m) as f64 - (m / 2) as f64;
                    [wobble(13, 7) * 0.5 + 0.5, wobble(11, 5) * 0.4, wobble(9, 4) * 0.0005]
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis() -> MotionAnalysis {
        pinned::shaky(4.0, 25.0, 640, 360)
    }

    #[test]
    fn frames_are_found_by_time_and_trimmed_between() {
        let a = analysis();
        assert!(a.is_sound());
        assert!((a.period() - 0.04).abs() < 1e-12);
        assert_eq!(a.first_at(0.0), 0);
        // A window on a frame opens on it, printed a microsecond late too; one past it opens on the next.
        assert_eq!(a.first_at(0.4), 10);
        assert_eq!(a.first_at(0.400001), 10);
        assert_eq!(a.first_at(0.401), 11);
        assert_eq!(a.first_at(99.0), a.times.len());
        assert!((a.between(10) - 0.38).abs() < 1e-12);
        assert!((a.between(0) + 0.02).abs() < 1e-12);
        assert!(a.covers(0.0, 3.99));
        assert!(!a.covers(-0.5, 1.0));
        assert!(!a.covers(1.0, 4.5));
        assert_eq!(a.radius(0.5), 13);
        assert_eq!(a.radius(0.0), 0);
    }

    #[test]
    fn a_still_camera_needs_no_correction_and_a_steady_pan_keeps_its_middle() {
        assert!(corrections(&[[0.0; 3]; 50], 10).iter().flatten().all(|v| *v == 0.0));
        assert!(corrections(&[[1.0, 2.0, 0.1]; 50], 0).iter().flatten().all(|v| *v == 0.0));
        // A constant pan is a straight path, which a symmetric kernel leaves where it is, away from the ends.
        let pan = corrections(&[[2.0, -1.0, 0.0]; 100], 10);
        for f in &pan[10..90] {
            assert!(f[0].abs() < 1e-9 && f[1].abs() < 1e-9, "{f:?}");
        }
        // A jolt is taken out: the frame that jumped is moved most of the way back.
        let mut jolt = vec![[0.0; 3]; 61];
        jolt[30] = [10.0, 0.0, 0.0];
        jolt[31] = [-10.0, 0.0, 0.0];
        let fixed = corrections(&jolt, 10);
        assert!(fixed[30][0] > 8.0, "{:?}", fixed[30]);
        assert!(fixed[0][0].abs() < 1e-3 && fixed[60][0].abs() < 1e-3);
    }

    #[test]
    fn the_correction_matches_vid_stabs_gaussian_path_numerically() {
        // Two frames and a radius of one: the kernel is [e^-4, 1, e^-4], renormalised at the ends.
        let fixed = corrections(&[[0.0, 0.0, 0.0], [3.0, 0.0, 0.0]], 1);
        let w = (-4.0f64).exp();
        let avg0 = (0.0 + 3.0 * w) / (1.0 + w);
        let avg1 = (0.0 * w + 3.0) / (w + 1.0);
        assert!((fixed[0][0] - (0.0 - avg0)).abs() < 1e-12);
        assert!((fixed[1][0] - (3.0 - avg1)).abs() < 1e-12);
    }

    #[test]
    fn the_auto_zoom_hides_the_largest_shift_and_is_capped() {
        let fixes: Vec<[f64; 3]> = (0..200).map(|k| [if k == 7 { 500.0 } else { 6.4 }, -3.6, 0.0]).collect();
        // One outlier in two hundred is cut; 6.4 px either side of a 640 px frame is 2%.
        assert!((auto_zoom(&fixes, 640, 360) - 2.0).abs() < 1e-9);
        assert_eq!(auto_zoom(&[[900.0, 0.0, 0.0]; 10], 640, 360), MAX_STABILIZE_ZOOM);
        assert_eq!(auto_zoom(&[], 640, 360), 0.0);
    }

    #[test]
    fn the_transforms_text_is_vid_stabs_old_format_scaled_to_the_frame_read() {
        let text = transforms_text(&[[2.0, -4.0, 0.01], [0.0, 0.0, 0.0]], 0.5, 0.25);
        assert_eq!(
            text,
            "0 1.000000 -1.000000 0.010000000 0 0\n0 0.000000 0.000000 0.000000000 0 0\n"
        );
    }

    #[test]
    fn a_slice_starts_between_frames_and_holds_the_frames_from_the_window() {
        let id = Uuid::from_u128(7);
        let mut clip = Clip::new(id, 1.0, 3.0, 0.0);
        clip.stabilize = Some(Stabilize::default());
        let analyses = HashMap::from([(id, Arc::new(analysis()))]);
        pinned::with(analyses, || {
            let input = Some(InputFrame { width: 320, height: 180 });
            let s = slice_for(&clip, &Stabilize::default(), (1.0, 3.1), None, input).unwrap();
            assert!((s.start - 0.98).abs() < 1e-9, "{s:?}");
            assert!(s.file.to_string_lossy().ends_with("-25-79-0.5000x0.5000.trf"), "{s:?}");
            assert!(s.zoom > 0.0 && s.zoom <= MAX_STABILIZE_ZOOM);
            let fixed = Stabilize {
                strength: 0.5,
                zoom: Some(7.5),
            };
            assert_eq!(slice_for(&clip, &fixed, (1.0, 3.1), None, input).unwrap().zoom, 7.5);
            // Before the frames measured, or a clip past them: nothing to number the frames by.
            assert!(slice_for(&clip, &fixed, (-1.0, 3.1), None, input).is_none());
            let mut long = clip.clone();
            long.source_out = 9.0;
            assert!(slice_for(&long, &fixed, (1.0, 9.0), None, input).is_none());
        });
        pinned::with(HashMap::new(), || {
            assert!(slice_for(&clip, &Stabilize::default(), (1.0, 3.0), None, None).is_none());
        });
    }

    #[test]
    fn a_job_reads_as_its_status_until_it_ends() {
        let clip = Uuid::new_v4();
        assert_eq!(job_status(clip), None);
        job_queued(clip);
        assert_eq!(job_status(clip), Some(StabilizeStatus::Queued));
        job_progress(clip, 0.25);
        assert_eq!(job_status(clip), Some(StabilizeStatus::Analyzing { fraction: 0.25 }));
        job_ended(clip, Some("no picture".into()));
        assert_eq!(
            job_status(clip),
            Some(StabilizeStatus::Failed {
                reason: "no picture".into()
            })
        );
        job_ended(clip, None);
        assert_eq!(job_status(clip), None);
    }

    #[test]
    fn the_status_json_is_tagged() {
        let s = ClipStabilizeStatus {
            clip_id: Uuid::nil(),
            status: StabilizeStatus::Analyzing { fraction: 0.5 },
        };
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"clip_id":"00000000-0000-0000-0000-000000000000","state":"analyzing","fraction":0.5}"#
        );
    }
}
