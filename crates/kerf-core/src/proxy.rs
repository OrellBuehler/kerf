//! Preview proxies as something the user can see and steer.
//!
//! The engine has always built an all-intra, downscaled proxy per video asset so a
//! scrub decodes one keyframe (`engine::generate_proxy`). This module is the layer
//! above it, UI-agnostic like the rest of the core:
//!
//! * the **settings** the engine reads — how wide proxies are ([`ProxySize`]) and
//!   which file a preview decodes ([`PreviewSource`]);
//! * a **status** per asset ([`ProxyStatus`]): queued, building with a fraction,
//!   ready, failed with its reason, off, or not needed;
//! * the **queue**: every proxy is queued here, waits in the *high* lane of the heavy-job
//!   gate (so it runs before analysis, see `engine::cpu`), reports its progress and can
//!   be rebuilt or deleted. The adapters only pass an [`Notify`] that turns each status
//!   change into an event.
//!
//! The registry keeps what is *in flight* (queued, building, failed this session). What
//! is *done* is the file on disk — a proxy is ready when `ready_proxy` finds it, which
//! is what the preview itself asks — so a status can never claim a proxy the preview
//! would not use.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::engine;
use crate::engine::cpu;
use crate::error::{Error, Result};
use crate::model::{Asset, StreamKind};

// ---- settings the engine reads ----------------------------------------------

/// How wide the preview proxy of flat footage is, in pixels across — or none at all.
/// 1280 is what Kerf has always made and its cache stays valid; the size is part of
/// the cache key, so the others are different files. Spherical footage keeps the same
/// ratio (3072 for 1280) because reframing throws most of the frame away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxySize {
    /// No proxies are made, and previews decode the original.
    Off,
    W720,
    W1080,
    #[default]
    W1280,
}

impl ProxySize {
    pub const ALL: [ProxySize; 4] = [ProxySize::Off, ProxySize::W720, ProxySize::W1080, ProxySize::W1280];

    /// The width in pixels, `0` for [`ProxySize::Off`].
    pub fn px(self) -> u32 {
        match self {
            ProxySize::Off => 0,
            ProxySize::W720 => 720,
            ProxySize::W1080 => 1080,
            ProxySize::W1280 => 1280,
        }
    }

    /// The size a stored number means. Anything that is not exactly one of the four
    /// rounds up to the next (a hand-edited 900 is 1080); past 1280 is 1280.
    pub fn from_px(px: u32) -> Self {
        match px {
            0 => ProxySize::Off,
            1..=720 => ProxySize::W720,
            721..=1080 => ProxySize::W1080,
            _ => ProxySize::W1280,
        }
    }
}

impl Serialize for ProxySize {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_u32(self.px())
    }
}

impl<'de> Deserialize<'de> for ProxySize {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        // Lenient on purpose: a preference file with a size this build does not offer
        // must not be thrown out whole for it.
        u32::deserialize(d).map(ProxySize::from_px)
    }
}

/// Which file a preview decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewSource {
    /// Always the original, full resolution. Proxies are not built (they would not be
    /// used) — rebuild one by hand and it waits for you to switch back.
    Original,
    /// Only the proxy: a frame whose proxy is not ready yet is not decoded from the
    /// original, the preview says it is waiting and shows the proxy's progress.
    ProxyOnly,
    /// The proxy once it is ready, the original until then. Also what a stored value
    /// this build does not know reads as.
    #[default]
    #[serde(other)]
    Auto,
}

static PROXY_SIZE: AtomicU8 = AtomicU8::new(3);
static PREVIEW_SOURCE: AtomicU8 = AtomicU8::new(0);

pub fn proxy_size() -> ProxySize {
    ProxySize::ALL[PROXY_SIZE.load(Ordering::Relaxed) as usize % ProxySize::ALL.len()]
}

/// Set the proxy size. A **change** abandons every proxy queued or building — they are at the
/// old width, which nothing will use now — and the caller queues what is missing at the new
/// one (a proxy already on disk at the old size stays there, unused). Off is a change like any
/// other, and makes no new ones.
pub fn set_proxy_size(size: ProxySize) {
    let before = proxy_size();
    let index = ProxySize::ALL.iter().position(|s| *s == size).unwrap_or(3);
    PROXY_SIZE.store(index as u8, Ordering::Relaxed);
    if size_change_abandons_queue(before, size) {
        cancel_all();
    }
    if size != ProxySize::Off {
        engine::set_proxy_base_width(size.px());
    }
}

/// Whether moving the proxy size from `before` to `after` abandons what is queued or building
/// (pure): any change does, because those jobs are at the old width.
fn size_change_abandons_queue(before: ProxySize, after: ProxySize) -> bool {
    before != after
}

pub fn preview_source_setting() -> PreviewSource {
    match PREVIEW_SOURCE.load(Ordering::Relaxed) {
        1 => PreviewSource::Original,
        2 => PreviewSource::ProxyOnly,
        _ => PreviewSource::Auto,
    }
}

pub fn set_preview_source_setting(source: PreviewSource) {
    PREVIEW_SOURCE.store(
        match source {
            PreviewSource::Auto => 0,
            PreviewSource::Original => 1,
            PreviewSource::ProxyOnly => 2,
        },
        Ordering::Relaxed,
    );
    if source == PreviewSource::Original {
        cancel_all();
    }
}

/// The source mode in force: with proxies off there is nothing but the original.
pub fn effective_preview_source() -> PreviewSource {
    effective_source(proxy_size(), preview_source_setting())
}

/// [`effective_preview_source`] for given settings (pure).
pub fn effective_source(size: ProxySize, source: PreviewSource) -> PreviewSource {
    if size == ProxySize::Off {
        PreviewSource::Original
    } else {
        source
    }
}

/// Whether proxies are built on their own (an import, a project open): not when they
/// would never be used.
pub fn proxies_wanted() -> bool {
    effective_preview_source() != PreviewSource::Original
}

/// Whether `asset` is the kind that gets a proxy: it has a picture that moves.
pub fn needs_proxy(asset: &Asset) -> bool {
    proxy_exemption(asset).is_none()
}

/// Why `asset` gets no proxy, or `None` when it does.
fn proxy_exemption(asset: &Asset) -> Option<&'static str> {
    if asset.is_image() {
        Some("a still image decodes in one step")
    } else if !asset.streams.iter().any(|s| s.kind == StreamKind::Video) {
        Some("audio only: there is no picture to decode")
    } else {
        None
    }
}

// ---- status ------------------------------------------------------------------

/// Where an asset's proxy stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyPhase {
    /// A still or an audio-only file: it gets none.
    NotNeeded,
    /// None is made, by choice: proxies are off, previews use the original, or this
    /// asset's was deleted. `reason` says which.
    Off,
    /// Wanted and not there, and nothing is making it (yet).
    Missing,
    /// Waiting for the machine's heavy-job slot.
    Queued,
    /// Being encoded; `fraction` says how far.
    Building,
    /// On disk, and what the preview decodes (`bytes` is its size).
    Ready,
    /// The last attempt failed; `reason` is why.
    Failed,
}

/// An asset's proxy, as the bin, the status bar and an agent read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyStatus {
    pub asset_id: Uuid,
    pub state: ProxyPhase,
    /// How far a build is, `0.0..=1.0` — only while [`ProxyPhase::Building`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
    /// Why it is off, not needed or failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// How wide the proxy is (or would be), pixels across.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// The proxy file's size on disk, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// How long the running build has taken, and how long it should take still.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<f64>,
}

impl ProxyStatus {
    fn new(asset_id: Uuid, state: ProxyPhase) -> Self {
        Self {
            asset_id,
            state,
            fraction: None,
            reason: None,
            width: None,
            bytes: None,
            elapsed_secs: None,
            eta_secs: None,
        }
    }

    fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    fn with_width(mut self, width: u32) -> Self {
        self.width = Some(width);
        self
    }
}

/// What an asset's status is worked out from: the asset and whether its proxy was
/// deleted on purpose (kept in the project, see `Project::proxy_declined`).
#[derive(Debug, Clone)]
pub struct ProxyInput {
    pub asset: Asset,
    pub declined: bool,
}

/// One entry of the in-flight registry.
#[derive(Clone)]
enum Live {
    Queued,
    Building { fraction: f64, started: Instant },
    Failed(String),
}

struct Entry {
    /// Which submission this is: a worker only touches the entry it was given, never
    /// the one a rebuild put in its place.
    gen: u64,
    cancel: Arc<AtomicBool>,
    live: Live,
}

type JobKey = (String, u32);

fn registry() -> &'static Mutex<HashMap<JobKey, Entry>> {
    static JOBS: OnceLock<Mutex<HashMap<JobKey, Entry>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn jobs() -> std::sync::MutexGuard<'static, HashMap<JobKey, Entry>> {
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

fn live_of(key: &JobKey) -> Option<Live> {
    jobs().get(key).map(|e| e.live.clone())
}

/// The status of an asset's proxy (touches the disk and, the first time per file, runs
/// the cached `ffprobe` that keys the proxy — call it off the project lock).
pub fn status(input: &ProxyInput) -> ProxyStatus {
    status_under(input, proxy_size(), effective_preview_source())
}

/// [`status`] under given settings (`mode` already resolved against the size).
fn status_under(input: &ProxyInput, size: ProxySize, mode: PreviewSource) -> ProxyStatus {
    let asset = &input.asset;
    if let Some(why) = proxy_exemption(asset) {
        return ProxyStatus::new(asset.id, ProxyPhase::NotNeeded).with_reason(why);
    }
    let width = engine::proxy_width(asset.projection());
    let path = Path::new(&asset.path);
    let ready = engine::ready_proxy(path, width);
    let bytes = ready.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len());
    let mut out = ProxyStatus::new(asset.id, ProxyPhase::Missing).with_width(width);
    out.bytes = bytes;

    if size == ProxySize::Off {
        out.state = ProxyPhase::Off;
        out.reason = Some("proxies are off in Settings; previews decode the original".to_string());
        return out;
    }
    // A build the user asked for by hand is shown whatever the preview source says (Always
    // original builds none by itself, but a Rebuild is a request, and its progress is news).
    let live = live_of(&(asset.path.clone(), width));
    match &live {
        Some(Live::Queued) => {
            out.state = ProxyPhase::Queued;
            return out;
        }
        Some(Live::Building { fraction, started }) => {
            out.state = ProxyPhase::Building;
            out.fraction = Some(*fraction);
            out.elapsed_secs = Some(started.elapsed().as_secs_f64());
            out.eta_secs = eta(*fraction, started.elapsed());
            return out;
        }
        _ => {}
    }
    if mode == PreviewSource::Original {
        out.state = ProxyPhase::Off;
        out.reason = Some("previews are set to always use the original".to_string());
        return out;
    }
    if let Some(Live::Failed(reason)) = live {
        if ready.is_none() {
            out.state = ProxyPhase::Failed;
            out.reason = Some(reason);
            return out;
        }
    }
    if ready.is_some() {
        out.state = ProxyPhase::Ready;
    } else if input.declined {
        out.state = ProxyPhase::Off;
        out.reason = Some("you deleted this proxy; rebuild it to bring it back".to_string());
    }
    out
}

/// Time left in a build that is `fraction` done after `elapsed` (once there is enough
/// of it to extrapolate from).
fn eta(fraction: f64, elapsed: Duration) -> Option<f64> {
    (fraction > 0.02).then(|| elapsed.as_secs_f64() * (1.0 - fraction) / fraction)
}

/// The statuses of several assets at once.
pub fn statuses(inputs: &[ProxyInput]) -> Vec<ProxyStatus> {
    inputs.iter().map(status).collect()
}

/// A preview frame that cannot be made yet because **Proxy only** is on and the proxy
/// of a clip in it is not ready.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProxyWait {
    pub asset_id: Uuid,
    pub name: String,
    pub status: ProxyStatus,
}

/// What every error and message about a preview waiting for proxies starts with — the page
/// tells such a refusal from a failure by it (`isProxyWaitMessage` in `proxy-info.ts`).
pub const PROXY_WAIT_PREFIX: &str = "waiting for the preview proxy";

impl ProxyWait {
    /// What the preview says (and an error carries) while it waits.
    pub fn message(&self) -> String {
        format!("{PROXY_WAIT_PREFIX} of {} — {}", self.name, self.how())
    }

    fn how(&self) -> String {
        match self.status.state {
            ProxyPhase::Building => match self.status.fraction {
                Some(f) => format!("building it ({}%)", (f * 100.0).round() as u32),
                None => "building it".to_string(),
            },
            ProxyPhase::Queued => "queued".to_string(),
            ProxyPhase::Failed => format!("it failed: {}", self.status.reason.as_deref().unwrap_or("unknown error")),
            _ => "not built yet".to_string(),
        }
    }
}

/// One sentence for everything a preview is waiting on (`None` when it is waiting on nothing):
/// the first clip by name and how far its proxy is, and how many more there are.
pub fn waits_message(waits: &[ProxyWait]) -> Option<String> {
    let first = waits.first()?;
    let more = match waits.len() - 1 {
        0 => String::new(),
        n => format!(" and {n} more"),
    };
    Some(format!(
        "{PROXY_WAIT_PREFIX} of {}{more} — {} (Settings › Preview › Preview source is Proxy only)",
        first.name,
        first.how()
    ))
}

// ---- the queue ---------------------------------------------------------------

/// Told about every change of an asset's proxy. The adapters turn it into the
/// `proxy-progress` event.
pub type Notify = Arc<dyn Fn(&ProxyStatus) + Send + Sync>;

struct Queued {
    asset_id: Uuid,
    path: String,
    width: u32,
    duration: f64,
    gen: u64,
    cancel: Arc<AtomicBool>,
    reservation: cpu::Reservation,
    notify: Notify,
}

/// How many proxy encodes may be taken off the queue at once. The heavy-job gate lets
/// one run at a time whatever this says, so more workers only means more queued
/// encodes already past their ffprobe.
fn workers() -> usize {
    std::env::var("KERF_PROXY_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|n| n.max(1))
        .unwrap_or(1)
}

fn sender() -> &'static std::sync::mpsc::Sender<Queued> {
    static QUEUE: OnceLock<std::sync::mpsc::Sender<Queued>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Queued>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..workers() {
            let rx = Arc::clone(&rx);
            std::thread::spawn(move || loop {
                // Hold the lock only to dequeue, so the other workers can pull the next.
                let job = match rx.lock() {
                    Ok(guard) => guard.recv(),
                    Err(_) => break,
                };
                let Ok(job) = job else { break };
                run(job);
            });
        }
        tx
    })
}

static GENERATION: AtomicU64 = AtomicU64::new(1);

/// How often a build may tell anyone how far it has got: whichever of a percent of
/// progress or a second comes first.
const NOTIFY_FRACTION: f64 = 0.01;
const NOTIFY_EVERY: Duration = Duration::from_secs(1);

/// What a worker needs to settle a job whose body died.
struct JobMeta {
    key: JobKey,
    gen: u64,
    asset_id: Uuid,
    width: u32,
    notify: Notify,
}

/// Run `work` for the job `meta` describes; if it **panics**, that job is `Failed` with the
/// panic's text and the worker lives to take the next one — a panic in one asset's encode
/// (a decoder crate, an arithmetic overflow) must not leave the queue with no worker and every
/// later proxy `queued` for the rest of the session.
fn run_guarded(meta: JobMeta, work: impl FnOnce()) {
    if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
        let what = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        let reason = format!("the proxy encode crashed: {what}");
        tracing::error!(%reason, "preview proxy worker panicked");
        finish(&meta.key, meta.gen, Some(reason.clone()));
        let s = ProxyStatus::new(meta.asset_id, ProxyPhase::Failed)
            .with_width(meta.width)
            .with_reason(reason);
        (meta.notify)(&s);
    }
}

fn run(job: Queued) {
    let meta = JobMeta {
        key: (job.path.clone(), job.width),
        gen: job.gen,
        asset_id: job.asset_id,
        width: job.width,
        notify: job.notify.clone(),
    };
    run_guarded(meta, || build(job));
}

fn build(job: Queued) {
    let key: JobKey = (job.path.clone(), job.width);
    if job.cancel.load(Ordering::SeqCst) {
        return;
    }
    // The job stays `Queued` until it holds the machine's slot: the encode's first report
    // (0.0) comes when it does, so a proxy waiting behind an export is not shown at 0%.
    let started = Instant::now();
    let building = |fraction: f64| {
        let mut s = ProxyStatus::new(job.asset_id, ProxyPhase::Building).with_width(job.width);
        s.fraction = Some(fraction);
        s.elapsed_secs = Some(started.elapsed().as_secs_f64());
        s.eta_secs = eta(fraction, started.elapsed());
        s
    };
    let mut last: Option<(f64, Instant)> = None;
    let cancel = job.cancel.clone();
    let result = engine::generate_proxy_with(
        Path::new(&job.path),
        job.width,
        engine::ProxyRun {
            reservation: Some(job.reservation),
            duration: Some(job.duration),
            progress: &mut |fraction| {
                set_live(&key, job.gen, Live::Building { fraction, started });
                let due = last.is_none_or(|(at, when)| fraction - at >= NOTIFY_FRACTION || when.elapsed() >= NOTIFY_EVERY);
                if due {
                    last = Some((fraction, Instant::now()));
                    (job.notify)(&building(fraction));
                }
            },
            cancel: &move || cancel.load(Ordering::SeqCst),
        },
    );
    match result {
        Ok(proxy) => {
            finish(&key, job.gen, None);
            let mut s = ProxyStatus::new(job.asset_id, ProxyPhase::Ready).with_width(job.width);
            s.bytes = std::fs::metadata(&proxy).ok().map(|m| m.len());
            s.elapsed_secs = Some(started.elapsed().as_secs_f64());
            (job.notify)(&s);
        }
        // Whoever cancelled it (a delete, a rebuild, proxies turned off) says what the
        // status is now.
        Err(Error::Cancelled) => finish(&key, job.gen, None),
        Err(e) => {
            let reason = e.to_string();
            tracing::warn!(error = %e, path = %job.path, "preview proxy generation failed");
            finish(&key, job.gen, Some(reason.clone()));
            let s = ProxyStatus::new(job.asset_id, ProxyPhase::Failed)
                .with_width(job.width)
                .with_reason(reason);
            (job.notify)(&s);
        }
    }
}

fn set_live(key: &JobKey, gen: u64, live: Live) {
    if let Some(entry) = jobs().get_mut(key) {
        if entry.gen == gen {
            entry.live = live;
        }
    }
}

/// A job ended: drop its entry, or keep it as the failure the status reports.
fn finish(key: &JobKey, gen: u64, failure: Option<String>) {
    let mut jobs = jobs();
    if jobs.get(key).is_some_and(|e| e.gen == gen) {
        match failure {
            Some(reason) => {
                if let Some(entry) = jobs.get_mut(key) {
                    entry.live = Live::Failed(reason);
                }
            }
            None => {
                jobs.remove(key);
            }
        }
    }
}

/// Queue `asset`'s proxy at the size now in force, if it is not already queued or
/// building. Returns the status it starts from. The high-lane place in front of
/// analysis is taken here, before the caller can start any.
fn submit(asset: &Asset, notify: Notify) -> ProxyStatus {
    let width = engine::proxy_width(asset.projection());
    let key: JobKey = (asset.path.clone(), width);
    let mut s = ProxyStatus::new(asset.id, ProxyPhase::Queued).with_width(width);
    {
        let mut jobs = jobs();
        if let Some(entry) = jobs.get(&key) {
            match &entry.live {
                Live::Queued => return s,
                Live::Building { fraction, started } => {
                    s.state = ProxyPhase::Building;
                    s.fraction = Some(*fraction);
                    s.elapsed_secs = Some(started.elapsed().as_secs_f64());
                    return s;
                }
                Live::Failed(_) => {}
            }
        }
        let gen = GENERATION.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        jobs.insert(
            key,
            Entry {
                gen,
                cancel: cancel.clone(),
                live: Live::Queued,
            },
        );
        let queued = Queued {
            asset_id: asset.id,
            path: asset.path.clone(),
            width,
            duration: asset.duration,
            gen,
            cancel,
            reservation: cpu::reserve(),
            notify: notify.clone(),
        };
        if sender().send(queued).is_err() {
            tracing::warn!("preview proxy queue is closed");
        }
    }
    notify(&s);
    s
}

/// Queue the proxy of an asset that has just arrived (an import, a project open): a
/// no-op for a still or an audio-only file, when proxies are not wanted, when this
/// asset's was deleted on purpose (`declined`) and when it is already on disk — that is
/// what makes it cheap to call for every asset of a project being opened.
pub fn queue_auto(input: &ProxyInput, notify: Notify) -> Option<ProxyStatus> {
    if !needs_proxy(&input.asset) || !proxies_wanted() || input.declined {
        return None;
    }
    let width = engine::proxy_width(input.asset.projection());
    if engine::ready_proxy(Path::new(&input.asset.path), width).is_some() {
        return None;
    }
    Some(submit(&input.asset, notify))
}

/// Build `asset`'s proxy again from the original: the current one (and any being
/// built) is dropped first. A manual action, so it ignores a deleted proxy's
/// "declined" mark (the caller clears it) — but not the Off size, which has nothing to
/// build.
pub fn rebuild(asset: &Asset, notify: Notify) -> Result<ProxyStatus> {
    if let Some(why) = proxy_exemption(asset) {
        return Err(Error::InvalidArgument(format!("{} gets no proxy: {why}", asset.name)));
    }
    if proxy_size() == ProxySize::Off {
        return Err(Error::InvalidArgument(
            "proxies are turned off (Settings › Preview › Proxy size)".to_string(),
        ));
    }
    drop_proxies(asset);
    Ok(submit(asset, notify))
}

/// Delete `asset`'s proxy files at every size and stop any build of them. Returns
/// the bytes freed. The caller records that the user does not want it back
/// (`Project::set_proxy_declined`) and tells the UI with [`status`].
pub fn delete(asset: &Asset) -> u64 {
    drop_proxies(asset)
}

/// Stop the jobs of `asset` and delete its proxy files at every size.
fn drop_proxies(asset: &Asset) -> u64 {
    let spherical = engine::proxy_width(asset.projection()) != engine::proxy_base_width();
    let mut widths: Vec<u32> = ProxySize::ALL
        .iter()
        .filter(|s| **s != ProxySize::Off)
        .map(|s| {
            let base = s.px();
            if spherical {
                engine::proxy_width_for(asset.projection(), base)
            } else {
                base
            }
        })
        .collect();
    widths.push(engine::proxy_width(asset.projection()));
    widths.sort_unstable();
    widths.dedup();
    let mut freed = 0;
    for width in widths {
        let key: JobKey = (asset.path.clone(), width);
        if let Some(entry) = jobs().remove(&key) {
            entry.cancel.store(true, Ordering::SeqCst);
        }
        freed += engine::remove_proxy_files(Path::new(&asset.path), width);
    }
    freed
}

/// Abandon everything queued or building (proxies were turned off, or are no longer
/// used). A build in flight is killed within a quarter of a second.
pub fn cancel_all() {
    for (_, entry) in jobs().drain() {
        entry.cancel.store(true, Ordering::SeqCst);
    }
}

// ---- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StreamInfo;

    fn stream(kind: StreamKind, image: bool) -> StreamInfo {
        StreamInfo {
            index: 0,
            kind,
            codec: "h264".to_string(),
            width: Some(1920),
            height: Some(1080),
            fps: Some(30.0),
            sample_rate: None,
            channels: None,
            image,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: None,
            color_space: None,
        }
    }

    fn asset(streams: Vec<StreamInfo>, tag: &str) -> Asset {
        Asset {
            id: Uuid::new_v4(),
            // A path no other test or run shares, so nothing is cached for it.
            path: format!("/kerf-proxy-status-{}-{tag}.mp4", std::process::id()),
            name: format!("{tag}.mp4"),
            duration: 60.0,
            streams,
            imported_at: chrono::Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
        }
    }

    fn input(asset: Asset) -> ProxyInput {
        ProxyInput { asset, declined: false }
    }

    #[test]
    fn the_sizes_are_numbers_and_an_odd_one_rounds_up() {
        assert_eq!(ProxySize::default().px(), 1280);
        for size in ProxySize::ALL {
            assert_eq!(ProxySize::from_px(size.px()), size);
            let back: ProxySize = serde_json::from_str(&serde_json::to_string(&size).unwrap()).unwrap();
            assert_eq!(back, size);
        }
        assert_eq!(serde_json::to_string(&ProxySize::Off).unwrap(), "0");
        // A preference file with a size this build does not offer is read, not refused.
        assert_eq!(ProxySize::from_px(900), ProxySize::W1080);
        assert_eq!(ProxySize::from_px(4000), ProxySize::W1280);
        assert_eq!(serde_json::from_str::<ProxySize>("640").unwrap(), ProxySize::W720);
    }

    #[test]
    fn an_unknown_preview_source_reads_as_auto() {
        assert_eq!(serde_json::to_string(&PreviewSource::ProxyOnly).unwrap(), "\"proxy_only\"");
        assert_eq!(
            serde_json::from_str::<PreviewSource>("\"original\"").unwrap(),
            PreviewSource::Original
        );
        assert_eq!(
            serde_json::from_str::<PreviewSource>("\"nonsense\"").unwrap(),
            PreviewSource::Auto
        );
    }

    #[test]
    fn proxies_off_means_the_original_whatever_the_source_says() {
        for source in [PreviewSource::Auto, PreviewSource::Original, PreviewSource::ProxyOnly] {
            assert_eq!(effective_source(ProxySize::Off, source), PreviewSource::Original);
            assert_eq!(effective_source(ProxySize::W1280, source), source);
        }
    }

    #[test]
    fn stills_and_audio_only_files_need_no_proxy() {
        let still = input(asset(vec![stream(StreamKind::Video, true)], "still"));
        let audio = input(asset(vec![stream(StreamKind::Audio, false)], "audio"));
        let video = input(asset(
            vec![stream(StreamKind::Video, false), stream(StreamKind::Audio, false)],
            "video",
        ));
        assert_eq!(status(&still).state, ProxyPhase::NotNeeded);
        assert!(status(&still).reason.unwrap().contains("still"));
        assert_eq!(status(&audio).state, ProxyPhase::NotNeeded);
        assert!(!needs_proxy(&still.asset) && !needs_proxy(&audio.asset) && needs_proxy(&video.asset));
        assert!(queue_auto(&still, Arc::new(|_| {})).is_none());
        assert!(rebuild(&audio.asset, Arc::new(|_| {})).is_err());
    }

    #[test]
    fn a_video_with_no_proxy_is_missing_until_something_makes_it_and_off_once_deleted() {
        let mut video = input(asset(vec![stream(StreamKind::Video, false)], "missing"));
        let s = status(&video);
        assert_eq!(s.state, ProxyPhase::Missing);
        assert_eq!(s.width, Some(engine::proxy_width(None)));
        assert!(s.bytes.is_none());
        video.declined = true;
        let s = status(&video);
        assert_eq!(s.state, ProxyPhase::Off);
        assert!(s.reason.unwrap().contains("deleted"));
    }

    #[test]
    fn the_registry_reports_a_build_and_a_failure_and_a_cancelled_job_leaves_nothing() {
        let video = input(asset(vec![stream(StreamKind::Video, false)], "registry"));
        let width = engine::proxy_width(None);
        let key: JobKey = (video.asset.path.clone(), width);
        let cancel = Arc::new(AtomicBool::new(false));
        jobs().insert(
            key.clone(),
            Entry {
                gen: 7,
                cancel,
                live: Live::Queued,
            },
        );
        assert_eq!(status(&video).state, ProxyPhase::Queued);

        set_live(
            &key,
            7,
            Live::Building {
                fraction: 0.4,
                started: Instant::now(),
            },
        );
        let s = status(&video);
        assert_eq!(s.state, ProxyPhase::Building);
        assert_eq!(s.fraction, Some(0.4));

        // A worker that was handed an older submission cannot overwrite a newer one.
        set_live(
            &key,
            6,
            Live::Building {
                fraction: 0.9,
                started: Instant::now(),
            },
        );
        assert_eq!(status(&video).fraction, Some(0.4));
        finish(&key, 6, Some("stale".to_string()));
        assert_eq!(status(&video).state, ProxyPhase::Building);

        finish(&key, 7, Some("ffmpeg exited with 1: broken".to_string()));
        let s = status(&video);
        assert_eq!(s.state, ProxyPhase::Failed);
        assert!(s.reason.unwrap().contains("broken"));

        finish(&key, 7, None);
        // `finish` keeps a failure only when told to; a success drops the entry.
        jobs().remove(&key);
        assert_eq!(status(&video).state, ProxyPhase::Missing);
    }

    #[test]
    fn deleting_stops_the_job_and_flags_its_cancel() {
        let video = asset(vec![stream(StreamKind::Video, false)], "delete");
        let width = engine::proxy_width(None);
        let key: JobKey = (video.path.clone(), width);
        let cancel = Arc::new(AtomicBool::new(false));
        jobs().insert(
            key.clone(),
            Entry {
                gen: 1,
                cancel: cancel.clone(),
                live: Live::Queued,
            },
        );
        assert_eq!(delete(&video), 0, "nothing on disk to free");
        assert!(cancel.load(Ordering::SeqCst), "the queued job was told to stand down");
        assert!(live_of(&key).is_none());
    }

    #[test]
    fn a_wait_says_which_clip_and_how_far_its_proxy_is() {
        let id = Uuid::new_v4();
        let mut s = ProxyStatus::new(id, ProxyPhase::Building);
        s.fraction = Some(0.426);
        let wait = ProxyWait {
            asset_id: id,
            name: "GOPR0042.MP4".to_string(),
            status: s,
        };
        let said = wait.message();
        assert!(said.contains("GOPR0042.MP4") && said.contains("43%"), "{said}");
        let failed = ProxyWait {
            status: ProxyStatus::new(id, ProxyPhase::Failed).with_reason("ffmpeg exited with 1"),
            ..wait
        };
        assert!(failed.message().contains("it failed: ffmpeg exited with 1"));
    }

    #[test]
    fn a_size_change_abandons_what_is_queued_at_the_old_width() {
        for from in ProxySize::ALL {
            for to in ProxySize::ALL {
                assert_eq!(size_change_abandons_queue(from, to), from != to, "{from:?} -> {to:?}");
            }
        }
        // Setting the size already in force is not a change: nothing is cancelled.
        let video = asset(vec![stream(StreamKind::Video, false)], "same-size");
        let key: JobKey = (video.path, engine::proxy_width(None));
        let cancel = Arc::new(AtomicBool::new(false));
        jobs().insert(
            key.clone(),
            Entry {
                gen: 1,
                cancel: cancel.clone(),
                live: Live::Queued,
            },
        );
        set_proxy_size(proxy_size());
        assert!(!cancel.load(Ordering::SeqCst), "the same size cancels nothing");
        jobs().remove(&key);
    }

    #[test]
    fn a_build_the_user_asked_for_shows_under_always_original_and_a_queued_one_stays_queued() {
        let video = input(asset(vec![stream(StreamKind::Video, false)], "manual-rebuild"));
        let width = engine::proxy_width(None);
        let key: JobKey = (video.asset.path.clone(), width);
        // Nothing queued, always-original: off (no proxies are built by themselves).
        let off = status_under(&video, ProxySize::W1280, PreviewSource::Original);
        assert_eq!(off.state, ProxyPhase::Off);
        assert!(off.reason.unwrap().contains("always use the original"));
        // A Rebuild is queued: its progress is shown, not hidden behind the setting.
        jobs().insert(
            key.clone(),
            Entry {
                gen: 3,
                cancel: Arc::new(AtomicBool::new(false)),
                live: Live::Queued,
            },
        );
        assert_eq!(
            status_under(&video, ProxySize::W1280, PreviewSource::Original).state,
            ProxyPhase::Queued
        );
        set_live(
            &key,
            3,
            Live::Building {
                fraction: 0.5,
                started: Instant::now(),
            },
        );
        let building = status_under(&video, ProxySize::W1280, PreviewSource::Original);
        assert_eq!((building.state, building.fraction), (ProxyPhase::Building, Some(0.5)));
        // Proxies off outranks everything: there is nothing to build.
        assert_eq!(
            status_under(&video, ProxySize::Off, PreviewSource::Original).state,
            ProxyPhase::Off
        );
        jobs().remove(&key);
    }

    #[test]
    fn a_panic_in_a_proxy_job_fails_that_job_and_not_the_worker() {
        let video = asset(vec![stream(StreamKind::Video, false)], "panics");
        let width = engine::proxy_width(None);
        let key: JobKey = (video.path.clone(), width);
        jobs().insert(
            key.clone(),
            Entry {
                gen: 9,
                cancel: Arc::new(AtomicBool::new(false)),
                live: Live::Building {
                    fraction: 0.3,
                    started: Instant::now(),
                },
            },
        );
        let told: Arc<Mutex<Vec<ProxyStatus>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = told.clone();
        let meta = JobMeta {
            key: key.clone(),
            gen: 9,
            asset_id: video.id,
            width,
            notify: Arc::new(move |s| sink.lock().unwrap().push(s.clone())),
        };
        // The call returns: the caller (the worker's loop) goes on to the next job.
        run_guarded(meta, || panic!("decoder blew up"));
        let told = told.lock().unwrap();
        assert_eq!(told.len(), 1);
        assert_eq!(told[0].state, ProxyPhase::Failed);
        assert!(
            told[0].reason.as_deref().unwrap().contains("decoder blew up"),
            "{:?}",
            told[0].reason
        );
        let input = input(video);
        let s = status_under(&input, ProxySize::W1280, PreviewSource::Auto);
        assert_eq!(s.state, ProxyPhase::Failed);
        jobs().remove(&key);
    }

    #[test]
    fn what_a_wait_says_carries_the_marker_the_page_recognizes_and_counts_the_rest() {
        let id = Uuid::new_v4();
        let wait = |name: &str, state: ProxyPhase, fraction: Option<f64>| {
            let mut status = ProxyStatus::new(id, state);
            status.fraction = fraction;
            ProxyWait {
                asset_id: id,
                name: name.to_string(),
                status,
            }
        };
        let one = wait("a.mp4", ProxyPhase::Building, Some(0.5));
        assert!(one.message().starts_with(PROXY_WAIT_PREFIX));
        assert_eq!(waits_message(&[]), None);
        let many = waits_message(&[
            one,
            wait("b.mp4", ProxyPhase::Queued, None),
            wait("c.mp4", ProxyPhase::Queued, None),
        ])
        .unwrap();
        assert!(many.starts_with(PROXY_WAIT_PREFIX), "{many}");
        assert!(many.contains("a.mp4 and 2 more") && many.contains("50%"), "{many}");
        // The setting is named with the same arrow as everywhere else.
        assert!(many.contains("Settings › Preview › Preview source"), "{many}");
        assert!(!many.contains("Preview >"), "{many}");
    }
}
