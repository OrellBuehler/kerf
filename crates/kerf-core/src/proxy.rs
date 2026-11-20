//! Preview proxies as something the user can see and steer.
//!
//! The engine has always built an all-intra, downscaled proxy per video asset so a
//! scrub decodes one keyframe (`engine::generate_proxy`). This module is the layer
//! above it, UI-agnostic like the rest of the core:
//!
//! * the **settings** the engine reads — how wide proxies are ([`ProxySize`]), which
//!   file a preview decodes ([`PreviewSource`]) and which encoder builds them
//!   ([`ProxyEncoder`]: x264 on the cores, or the opt-in **Fast (GPU)** pipeline, whose
//!   availability on this machine is [`fast_gpu_status`]);
//! * a **status** per asset ([`ProxyStatus`]): queued, building with a fraction,
//!   ready, failed with its reason, off, or not needed;
//! * the **queue**: every proxy is queued here, waits in the *high* lane of the heavy-job
//!   gate (so it runs before analysis, see `engine::cpu`), reports its progress and can
//!   be rebuilt or deleted. The adapters only pass an [`Notify`] that turns each status
//!   change into an event;
//! * the **line** the workers pull from, which is ordered (what the user asked for next,
//!   then what a preview is waiting on, then what the timeline uses, then the rest), can be
//!   paused and cancelled a job at a time, and is read whole as a [`ProxyQueue`] with an
//!   estimate of how long it will take ([`queue`]).
//!
//! The registry keeps what is *in flight* (queued, building, failed this session). What
//! is *done* is the file on disk — a proxy is ready when `ready_proxy` finds it, which
//! is what the preview itself asks — so a status can never claim a proxy the preview
//! would not use.

mod estimate;
mod schedule;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::engine;
use crate::engine::cpu;
use crate::error::{Error, Result};
use crate::model::{Asset, StreamKind, Timeline};

pub use schedule::QueueListener;
use schedule::{Claimed, JobInfo, Queued, Shared};

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

/// Which encoder builds the preview proxies. An app preference (`proxy_encoder` in the settings file),
/// not part of a project, and **off by default**: x264 on the cores is what Kerf has always made.
/// A name this build does not know is an error here; the settings file reads it field by field
/// (`kerf-app`'s `settings::parse`), so the setting is its default and the stored text is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyEncoder {
    /// x264 on the cores' one slot, hardware decode only.
    #[default]
    Software,
    /// "Fast (GPU)": decode, scale and encode on the graphics hardware with no frame downloaded,
    /// for the sources the machine's pipeline is known to handle (see `engine::cli::proxy_fast`: HDR,
    /// a late-starting or rotated video, interlaced or non-square pictures, MPEG-TS and every
    /// source the hardware decoder is not known to have stay on x264), on the hardware's slot beside
    /// the cores' (two builds at once). A proxy it builds is checked against the source's stream and
    /// built again by x264 if it fails or is not what x264 would have made. Where this machine has no
    /// pipeline that passes its check ([`fast_gpu_status`]) it changes nothing.
    FastGpu,
}

static PROXY_SIZE: AtomicU8 = AtomicU8::new(3);
static PREVIEW_SOURCE: AtomicU8 = AtomicU8::new(0);
static PROXY_ENCODER: AtomicU8 = AtomicU8::new(0);

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
        abandon_all();
    }
    if size != ProxySize::Off {
        engine::set_proxy_base_width(size.px());
    }
    crate::plan_epoch::bump_plan_epoch();
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
        abandon_all();
    }
    crate::plan_epoch::bump_plan_epoch();
}

pub fn proxy_encoder() -> ProxyEncoder {
    match PROXY_ENCODER.load(Ordering::Relaxed) {
        1 => ProxyEncoder::FastGpu,
        _ => ProxyEncoder::Software,
    }
}

/// Set the proxy encoder. Takes effect for the next proxy to be built (one being built or already on
/// disk is left as it is: the proxy keeps its cache name whichever encoder wrote it, and a Rebuild
/// is what makes an existing one again). Turning **Fast (GPU) on** starts the machine's check in the
/// background ([`fast_gpu_status`]) and gives a pipeline that stopped working this session another
/// chance; a change also moves how many workers pull from the line and what the waiting jobs are
/// labelled and estimated as.
pub fn set_proxy_encoder(encoder: ProxyEncoder) {
    let code = match encoder {
        ProxyEncoder::Software => 0,
        ProxyEncoder::FastGpu => 1,
    };
    let before = PROXY_ENCODER.swap(code, Ordering::Relaxed);
    engine::set_proxy_fast_gpu(encoder == ProxyEncoder::FastGpu);
    if before != code {
        let line = shared();
        line.wake();
        retag_waiting(line);
        line.touch();
    }
}

/// What the settings page shows of **Fast (GPU)**: whether this machine has a pipeline that works
/// (it is checked once per process, in the background, by running the whole pipeline on a small
/// generated clip), which one, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FastGpuStatus {
    /// A vendor's pipeline passed the check and has not stopped working since.
    pub available: bool,
    /// The check has finished. Until it has `available` is `false` and `reason` says "not checked yet":
    /// ask again in a moment.
    pub checked: bool,
    /// The vendor ("NVIDIA NVENC", "AMD AMF", "Intel Quick Sync", "VAAPI") that passed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<String>,
    /// Why not: nothing passed (each vendor's reason), or the one that passed stopped working.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// [`FastGpuStatus`] now. Cheap and cached: it never waits, and the first call starts the check on
/// a thread of its own (so it answers "not checked yet" until that thread is done).
pub fn fast_gpu_status() -> FastGpuStatus {
    let status = engine::fast_pipeline_status();
    FastGpuStatus {
        available: status.available,
        checked: status.checked,
        pipeline: status.pipeline.map(str::to_string),
        reason: status.reason,
    }
}

/// Whether a proxy queued now is likely to be built by the Fast (GPU) pipeline: it is the setting, and
/// the check has not said no (it may still be running).
fn fast_likely() -> bool {
    if proxy_encoder() != ProxyEncoder::FastGpu {
        return false;
    }
    let status = engine::fast_pipeline_status();
    status.available || !status.checked
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
    (fraction > estimate::ETA_FROM).then(|| elapsed.as_secs_f64() * (1.0 - fraction) / fraction)
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

/// Which encode a job runs on, and so which measured rate its estimate is made from, and which
/// slot it takes. A job is labelled with the encode it is **actually** running (it says so when it
/// holds its slot: a proxy the Fast (GPU) pipeline did not take, or failed, is `software`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pipeline {
    /// x264 on the cores' one heavy-job slot, with hardware decode where there is one.
    #[default]
    Software,
    /// The opt-in Fast (GPU) pipeline: decode, scale and encode on the hardware, on the hardware's
    /// slot (up to [`cpu::MAX_GPU_JOBS`] at once, beside the cores' job).
    Gpu,
}

impl Pipeline {
    pub const ALL: [Pipeline; 2] = [Pipeline::Software, Pipeline::Gpu];

    /// How many builds of this pipeline run at once with `workers` workers. The software encode
    /// takes the cores' one heavy-job slot, so a second worker only has a job waiting at the gate:
    /// the estimates divide by this, not by the worker count, or they would promise a speed-up
    /// that never comes. The GPU pipeline has a slot of its own with room for [`cpu::MAX_GPU_JOBS`]
    /// builds, so it uses the workers it has up to that many.
    pub fn parallelism(self, workers: usize) -> usize {
        match self {
            Pipeline::Software => 1,
            Pipeline::Gpu => workers.clamp(1, cpu::MAX_GPU_JOBS),
        }
    }

    /// The name the logs and the JSON use.
    pub fn as_str(self) -> &'static str {
        match self {
            Pipeline::Software => "software",
            Pipeline::Gpu => "gpu",
        }
    }

    fn of(used: engine::ProxyPipeline) -> Self {
        match used {
            engine::ProxyPipeline::Software => Pipeline::Software,
            engine::ProxyPipeline::Gpu(_) => Pipeline::Gpu,
        }
    }
}

/// How many proxy encodes may be taken off the queue at once, given the setting (pure, unit-tested):
/// `KERF_PROXY_WORKERS` outright when it is set, else one, or [`cpu::MAX_GPU_JOBS`] while Fast (GPU)
/// is on and likely (`fast`) with the hardware lane on (`gpu_lane`: two builds hold the hardware's
/// slots at once, and a software job among them waits at the cores' gate beside them). The heavy-job
/// gate lets one software encode run at a time whatever this says, so more workers than the pipeline
/// has room for only mean more queued encodes already past their ffprobe.
fn workers_for(forced: Option<usize>, fast: bool, gpu_lane: bool) -> usize {
    match forced {
        Some(n) => n.max(1),
        None if fast && gpu_lane => cpu::MAX_GPU_JOBS,
        None => 1,
    }
}

fn forced_workers() -> Option<usize> {
    std::env::var("KERF_PROXY_WORKERS").ok().and_then(|v| v.parse::<usize>().ok())
}

/// How many workers pull from the line now (see [`workers_for`]); read each time a worker looks for
/// a job, so the setting takes effect at once.
fn workers() -> usize {
    workers_for(forced_workers(), fast_likely(), cpu::gpu_lane())
}

thread_local! {
    /// The line a test's thread uses instead of the process's (which has real workers that would
    /// try to encode the tests' made-up files).
    #[cfg(test)]
    static TEST_LINE: Cell<Option<&'static Shared>> = const { Cell::new(None) };
}

/// The line the workers pull from, started with its workers on first use.
fn shared() -> &'static Shared {
    #[cfg(test)]
    if let Some(line) = TEST_LINE.with(Cell::get) {
        return line;
    }
    static LINE: OnceLock<&'static Shared> = OnceLock::new();
    LINE.get_or_init(|| {
        // Threads for the most that may ever pull at once; how many may is `workers()`, asked each time.
        let threads = forced_workers().unwrap_or(1).max(cpu::MAX_GPU_JOBS);
        let line: &'static Shared = Box::leak(Box::new(Shared::new(threads, workers)));
        for _ in 0..threads {
            std::thread::spawn(move || loop {
                run(line, line.claim());
            });
        }
        line
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

fn run(line: &Shared, claimed: Claimed) {
    let key = claimed.job.info.key.clone();
    let gen = claimed.job.gen;
    let meta = JobMeta {
        key: key.clone(),
        gen,
        asset_id: claimed.job.info.asset_id,
        width: claimed.job.info.width,
        notify: claimed.job.notify.clone(),
    };
    let mut back = None;
    run_guarded(meta, || back = build(line, claimed));
    finished(line, &key, gen, back);
}

/// A worker is done with a job: whichever way it ended, the line forgets it; a job that stepped
/// back (see [`Claimed::give_way`]) takes its old place again unless it was cancelled meanwhile.
fn finished(line: &Shared, key: &JobKey, gen: u64, back: Option<Queued>) {
    {
        let registry = jobs();
        let mut waiting = line.lock();
        let asked = waiting.release(key, gen);
        if let Some(job) = back {
            if !job.cancel.load(Ordering::SeqCst) && registry.get(key).is_some_and(|e| e.gen == gen) {
                waiting.step_back(job, asked);
            }
        }
    }
    line.wake();
    line.touch();
}

/// Encode a job's proxy. `Some(job)` when the job never started and is to go back in line: the
/// queue was paused, or a job that goes before it arrived, while it waited for the machine.
fn build(line: &Shared, claimed: Claimed) -> Option<Queued> {
    let Claimed {
        mut job,
        give_way,
        holds_slot,
    } = claimed;
    let key = job.info.key.clone();
    if job.cancel.load(Ordering::SeqCst) {
        return None;
    }
    // The job stays `Queued` until it holds the machine's slot: the encode's first report
    // (0.0) comes when it does, so a proxy waiting behind an export is not shown at 0%, and its
    // clock starts then, not at the wait (a build that waited ten minutes is not ten minutes slow).
    let started: Cell<Option<Instant>> = Cell::new(None);
    let building = |fraction: f64, since: Instant| {
        let mut s = ProxyStatus::new(job.info.asset_id, ProxyPhase::Building).with_width(job.info.width);
        s.fraction = Some(fraction);
        s.elapsed_secs = Some(since.elapsed().as_secs_f64());
        s.eta_secs = eta(fraction, since.elapsed());
        s
    };
    let mut last: Option<(f64, Instant)> = None;
    let cancel = job.cancel.clone();
    // The encode the build ended up on: a proxy the Fast (GPU) pipeline failed is built again by x264,
    // and its time and its rate are x264's.
    let used: Cell<Option<engine::ProxyPipeline>> = Cell::new(None);
    let result = engine::generate_proxy_with(
        Path::new(&job.info.key.0),
        job.info.width,
        engine::ProxyRun {
            reservation: job.reservation.take(),
            duration: Some(job.info.duration),
            progress: &mut |fraction| {
                // A cancelled build says nothing more: its asset's status is whatever the canceller
                // sent, and a late "building" would put it back on the screen.
                if job.cancel.load(Ordering::SeqCst) {
                    return;
                }
                let since = match started.get() {
                    Some(since) => since,
                    None => {
                        let now = Instant::now();
                        started.set(Some(now));
                        holds_slot.store(true, Ordering::SeqCst);
                        line.touch();
                        now
                    }
                };
                set_live(
                    &key,
                    job.gen,
                    Live::Building {
                        fraction,
                        started: since,
                    },
                );
                let due = last.is_none_or(|(at, when)| fraction - at >= NOTIFY_FRACTION || when.elapsed() >= NOTIFY_EVERY);
                if due {
                    last = Some((fraction, Instant::now()));
                    (job.notify)(&building(fraction, since));
                }
            },
            // Until the encode holds the slot a job also stands aside for a paused queue or a
            // job that goes first; a running encode is only ever stopped by a cancel.
            cancel: &|| cancel.load(Ordering::SeqCst) || (!holds_slot.load(Ordering::SeqCst) && give_way.load(Ordering::SeqCst)),
            // Each attempt says which encode it is: the job is labelled with it, and its clock starts
            // again (a rebuild in software is timed as software, not as the GPU attempt before it).
            pipeline: &mut |now| {
                used.set(Some(now));
                started.set(None);
                line.lock().set_pipeline(&key, job.gen, Pipeline::of(now));
                line.touch();
            },
        },
    );
    match result {
        Ok(proxy) => {
            let wall = started.get().map(|since| since.elapsed().as_secs_f64());
            if let Some(wall) = wall {
                record_build(line, &job.info, used.get().unwrap_or(engine::ProxyPipeline::Software), wall);
            }
            finish(&key, job.gen, None);
            let mut s = ProxyStatus::new(job.info.asset_id, ProxyPhase::Ready).with_width(job.info.width);
            s.bytes = std::fs::metadata(&proxy).ok().map(|m| m.len());
            s.elapsed_secs = wall;
            (job.notify)(&s);
            crate::cache_cap::nudge();
            None
        }
        // Whoever cancelled it (a delete, a rebuild, proxies turned off) says what the
        // status is now. A job nobody cancelled that never started is one that stood aside.
        Err(Error::Cancelled) => {
            if job.cancel.load(Ordering::SeqCst) || holds_slot.load(Ordering::SeqCst) {
                finish(&key, job.gen, None);
                None
            } else {
                Some(job)
            }
        }
        Err(e) => {
            let reason = e.to_string();
            tracing::warn!(error = %e, path = %job.info.key.0, "preview proxy generation failed");
            finish(&key, job.gen, Some(reason.clone()));
            let s = ProxyStatus::new(job.info.asset_id, ProxyPhase::Failed)
                .with_width(job.info.width)
                .with_reason(reason);
            (job.notify)(&s);
            None
        }
    }
}

/// A proxy was built by `used`: count its speed toward the estimates of that pipeline and log it.
fn record_build(line: &Shared, info: &JobInfo, used: engine::ProxyPipeline, wall_secs: f64) {
    let (width, height) = info.source;
    let cost = estimate::pixel_seconds(width, height, info.duration);
    line.lock().rates.record(Pipeline::of(used), cost, wall_secs);
    let round = |v: f64| (v * 100.0).round() / 100.0;
    let source = format!("{width}x{height}");
    tracing::info!(
        pipeline = Pipeline::of(used).as_str(),
        encoder = used.encoder(),
        %source,
        media_secs = round(info.duration),
        wall_secs = round(wall_secs),
        speed = round(info.duration / wall_secs.max(f64::EPSILON)),
        "preview proxy built"
    );
}

/// The pipeline a proxy queued now is estimated and labelled as, until it starts and says which it
/// is running (it may be refused the GPU: see `engine::cli::proxy_fast`).
fn predicted_pipeline() -> Pipeline {
    if fast_likely() {
        Pipeline::Gpu
    } else {
        Pipeline::Software
    }
}

/// The setting that decides how waiting jobs are built changed: relabel them.
fn retag_waiting(line: &Shared) {
    let predicted = predicted_pipeline();
    line.lock().retag_pending(predicted);
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

/// Why a proxy is being queued, which decides what a cancelled or failed one before it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    /// The adapter on its own (an import, a project open): a job somebody cancelled stays cancelled.
    Auto,
    /// A rebuild, asked for.
    Rebuild,
    /// A rebuild, asked for, to be done before the rest.
    Next,
}

fn job_key(asset: &Asset) -> JobKey {
    (asset.path.clone(), engine::proxy_width(asset.projection()))
}

/// The size of the picture in pixels (what a source costs to encode), `(0, 0)` when it has none.
fn source_size(asset: &Asset) -> (u32, u32) {
    asset
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Video)
        .and_then(|s| Some((s.width?, s.height?)))
        .unwrap_or((0, 0))
}

/// Queue `asset`'s proxy at the size now in force, if it is not already queued or
/// building. Returns the status it starts from. The high-lane place in front of
/// analysis is taken here, before the caller can start any.
fn submit(asset: &Asset, notify: Notify, ask: Ask) -> ProxyStatus {
    let line = shared();
    let key = job_key(asset);
    let mut s = ProxyStatus::new(asset.id, ProxyPhase::Queued).with_width(key.1);
    {
        let mut registry = jobs();
        let mut waiting = line.lock();
        if ask != Ask::Auto {
            waiting.forgive(&key);
        }
        match registry.get(&key).map(|e| e.live.clone()) {
            Some(Live::Queued) => {
                if ask == Ask::Next && waiting.ask_next(&key) {
                    line.touch();
                }
                return s;
            }
            Some(Live::Building { fraction, started }) => {
                s.state = ProxyPhase::Building;
                s.fraction = Some(fraction);
                s.elapsed_secs = Some(started.elapsed().as_secs_f64());
                return s;
            }
            Some(Live::Failed(_)) | None => {}
        }
        let gen = GENERATION.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        registry.insert(
            key.clone(),
            Entry {
                gen,
                cancel: cancel.clone(),
                live: Live::Queued,
            },
        );
        let info = JobInfo {
            key: key.clone(),
            asset_id: asset.id,
            name: asset.name.clone(),
            source: source_size(asset),
            width: key.1,
            duration: asset.duration,
            pipeline: predicted_pipeline(),
            seq: waiting.next_seq(),
            next: 0,
        };
        let reservation = (!waiting.is_paused()).then(cpu::reserve);
        waiting.push(Queued {
            info,
            gen,
            cancel,
            reservation,
            notify: notify.clone(),
        });
        if ask == Ask::Next {
            waiting.ask_next(&key);
        }
    }
    line.wake();
    line.touch();
    notify(&s);
    s
}

/// Queue the proxy of an asset that has just arrived (an import, a project open): a
/// no-op for a still or an audio-only file, when proxies are not wanted, when this
/// asset's was deleted on purpose (`declined`), when somebody cancelled its build this session
/// (a Rebuild or a Build next asks again) and when it is already on disk — that is
/// what makes it cheap to call for every asset of a project being opened.
pub fn queue_auto(input: &ProxyInput, notify: Notify) -> Option<ProxyStatus> {
    if !needs_proxy(&input.asset) || !proxies_wanted() || input.declined {
        return None;
    }
    let key = job_key(&input.asset);
    if shared().lock().was_cancelled(&key) {
        return None;
    }
    if engine::ready_proxy(Path::new(&input.asset.path), key.1).is_some() {
        return None;
    }
    Some(submit(&input.asset, notify, Ask::Auto))
}

/// The refusals of a manual build (a rebuild, a build next): the file needs no proxy, or proxies
/// are off.
fn refuse_manual_build(asset: &Asset) -> Result<()> {
    if let Some(why) = proxy_exemption(asset) {
        return Err(Error::InvalidArgument(format!("{} gets no proxy: {why}", asset.name)));
    }
    if proxy_size() == ProxySize::Off {
        return Err(Error::InvalidArgument(
            "proxies are turned off (Settings › Preview › Proxy size)".to_string(),
        ));
    }
    Ok(())
}

/// Build `asset`'s proxy again from the original: the current one (and any being
/// built) is dropped first. A manual action, so it ignores a deleted proxy's
/// "declined" mark (the caller clears it) and a cancel of this session — but not the Off
/// size, which has nothing to build.
pub fn rebuild(asset: &Asset, notify: Notify) -> Result<ProxyStatus> {
    refuse_manual_build(asset)?;
    drop_proxies(asset);
    Ok(submit(asset, notify, Ask::Rebuild))
}

/// Build `asset`'s proxy before everything else that is waiting: the job goes to the front of the
/// line (the latest ask first when several are made), and one the user had cancelled or deleted
/// is wanted again (the caller clears a deleted proxy's "declined" mark, as for [`rebuild`]). A
/// proxy already on disk is left as it is and reported ready; one being built is not interrupted
/// or restarted. Refused like [`rebuild`]: a file that needs none, proxies off.
pub fn build_next(asset: &Asset, notify: Notify) -> Result<ProxyStatus> {
    refuse_manual_build(asset)?;
    if engine::ready_proxy(Path::new(&asset.path), engine::proxy_width(asset.projection())).is_some() {
        let input = ProxyInput {
            asset: asset.clone(),
            declined: false,
        };
        return Ok(status_under(&input, proxy_size(), PreviewSource::Auto));
    }
    Ok(submit(asset, notify, Ask::Next))
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
    let keys: Vec<JobKey> = widths.iter().map(|w| (asset.path.clone(), *w)).collect();
    withdraw(&keys, false);
    widths
        .into_iter()
        .map(|width| engine::remove_proxy_files(Path::new(&asset.path), width))
        .sum()
}

/// A job taken out of the registry and the line: its key and the asset it was for, if the line
/// still knew it.
type Withdrawn = (JobKey, Option<Uuid>);

/// Take `keys` out of the registry (whatever they were doing, a failure included) and out of the
/// line, and tell any worker that has one to stop it. A job still waiting for a worker is dropped
/// on the spot, and with it the place it held in front of analysis; one a worker has gives its
/// place back when the worker notices (within a quarter of a second) and a running encode kills
/// ffmpeg and removes its `.part`. With `mark` the keys are remembered as cancelled by the user,
/// so nothing queues them by itself again this session.
fn withdraw_locked(
    registry: &mut HashMap<JobKey, Entry>,
    waiting: &mut schedule::Scheduler,
    keys: &[JobKey],
    mark: bool,
) -> Vec<Withdrawn> {
    let mut assets = waiting.remove(keys);
    let mut done = Vec::new();
    for key in keys {
        let entry = registry.remove(key);
        if let Some(entry) = &entry {
            entry.cancel.store(true, Ordering::SeqCst);
        }
        let asset_id = assets.remove(key);
        if entry.is_some() || asset_id.is_some() {
            if mark {
                waiting.mark_cancelled(key.clone());
            }
            done.push((key.clone(), asset_id));
        }
    }
    done
}

fn withdraw(keys: &[JobKey], mark: bool) -> Vec<Withdrawn> {
    let line = shared();
    let done = {
        let mut registry = jobs();
        let mut waiting = line.lock();
        withdraw_locked(&mut registry, &mut waiting, keys, mark)
    };
    if !done.is_empty() {
        line.touch();
    }
    done
}

/// Cancel the queued or building jobs `wanted` picks, for the user's sake (see [`cancel`]).
fn cancel_matching(wanted: impl Fn(&JobKey) -> bool) -> Vec<Withdrawn> {
    let line = shared();
    let done = {
        let mut registry = jobs();
        let mut waiting = line.lock();
        let keys: Vec<JobKey> = registry
            .iter()
            .filter(|(key, entry)| wanted(key) && matches!(entry.live, Live::Queued | Live::Building { .. }))
            .map(|(key, _)| key.clone())
            .collect();
        withdraw_locked(&mut registry, &mut waiting, &keys, true)
    };
    if !done.is_empty() {
        line.touch();
    }
    done
}

/// Cancel `asset`'s proxy job, queued or building. Whether there was one.
///
/// The job goes back to *not built*: not failed, and not queued again by itself this session —
/// [`queue_auto`] skips it until a [`rebuild`] or a [`build_next`] asks for it, or a change of
/// proxy size or preview source starts the queue afresh. Its place in front of analysis is given
/// back at once if it was waiting; a build in flight is killed within a quarter of a second and
/// removes its partial file. Nothing is told through the job's `Notify`: the caller knows which
/// asset it cancelled and sends the new [`status`].
pub fn cancel(asset: &Asset) -> bool {
    let key = job_key(asset);
    !cancel_matching(|k| *k == key).is_empty()
}

/// Cancel every queued or building job, as the user's Cancel all: each goes back to *not built*,
/// none is queued again by itself this session (see [`cancel`]), and failures stay as they are.
/// Returns the assets whose job it was, for the caller to send their new statuses. A build in
/// flight is killed within a quarter of a second. A pause is not lifted.
pub fn cancel_all() -> Vec<Uuid> {
    cancel_matching(|_| true).into_iter().filter_map(|(_, id)| id).collect()
}

/// Abandon everything (a change of proxy size or preview source: the jobs are at a width nothing
/// uses, or for previews that no longer want them): queued, building and failed, silently, and
/// forget what the user cancelled, since the adapter now queues what is missing afresh.
fn abandon_all() {
    abandon(|_| true);
}

/// [`abandon_all`] for the jobs `wanted` picks.
fn abandon(wanted: impl Fn(&JobKey) -> bool) {
    let line = shared();
    {
        let mut registry = jobs();
        let mut waiting = line.lock();
        let keys: Vec<JobKey> = registry.keys().filter(|key| wanted(key)).cloned().collect();
        withdraw_locked(&mut registry, &mut waiting, &keys, false);
        waiting.forget_cancelled(&wanted);
    }
    line.touch();
}

/// Pause the queue: the build in flight finishes, and nothing new starts until [`resume`]. A job a
/// worker had taken but that is still waiting for the machine (behind an export, say) steps back
/// into the line; the places the waiting jobs held in front of analysis are given up, so
/// analysis is not made to wait for proxies that are not coming. Jobs may still be queued
/// meanwhile. Whether the state changed.
pub fn pause() -> bool {
    set_paused(true)
}

/// Lift [`pause`]: the line takes its places again and the workers go on. Whether the state
/// changed.
pub fn resume() -> bool {
    set_paused(false)
}

fn set_paused(paused: bool) -> bool {
    let line = shared();
    let changed = line.lock().set_paused(paused);
    if changed {
        line.wake();
        line.touch();
    }
    changed
}

/// Whether the queue is paused.
pub fn is_paused() -> bool {
    shared().lock().is_paused()
}

/// Re-rank the line: the assets a preview is waiting on (Proxy only: the clips under the
/// playhead with no proxy yet) go before the assets the timeline uses, and those before the
/// rest; what somebody asked to build next goes before all of them. Within a class the order is
/// the order the jobs arrived. Both sets replace the ones before, so pass both every time (an
/// empty set ranks nothing); resolve them under the project lock and call this after releasing
/// it — it only takes the queue's own short lock. A job a worker has taken but that is still
/// waiting for the machine steps back for one that now goes before it; a build under way is never
/// interrupted.
pub fn rerank(waits: &HashSet<Uuid>, used: &HashSet<Uuid>) {
    let line = shared();
    if line.lock().rerank(waits, used) {
        line.touch();
    }
}

/// The assets the cut uses where a preview decodes them: every clip on a video track that is not
/// generated (a title has no file). What [`rerank`] takes for `used`; `Project::proxy_used_assets`
/// reads it from the timeline being worked on.
pub fn used_assets(timeline: &Timeline) -> HashSet<Uuid> {
    timeline
        .tracks
        .iter()
        .filter(|t| t.kind == StreamKind::Video)
        .flat_map(|t| t.clips.iter())
        .filter(|c| !c.is_generated())
        .map(|c| c.asset_id)
        .collect()
}

/// The assets of `waits` (what `Project::proxy_waits` says a preview is waiting on): what
/// [`rerank`] takes for `waits`.
pub fn waiting_assets(waits: &[ProxyWait]) -> HashSet<Uuid> {
    waits.iter().map(|w| w.asset_id).collect()
}

/// Call `listener` whenever the queue changes in a way a panel showing it would redraw for: a job
/// joins, ends, starts building or is cancelled, the order changes ([`rerank`], [`build_next`]),
/// the queue is paused or resumed, or a failure appears or goes. Not for a build's progress (that
/// is the per-job [`Notify`]) — and the estimate drifts with it, so a panel reads [`queue`] again
/// on a progress event too if it wants a live ETA.
///
/// Register once, at startup; a second call replaces the first. The listener is called from a
/// thread of its own, never under a lock of the queue's, and is **coalesced**: the first change
/// after a quiet moment is told at once and the ones that follow within a quarter of a second as
/// a single call after it, so an import of a hundred assets is a call or two, not a hundred.
/// It carries no payload; the adapter emits an event and the page reads [`queue`].
pub fn set_queue_listener(listener: QueueListener) {
    shared().listen(listener);
}

// ---- the queue, read ------------------------------------------------------------

/// Whether a job of the line is being encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    /// Holds the machine's slot and is being encoded.
    Building,
    /// Waiting: for its turn, or (when a worker has it) for the machine.
    Queued,
}

/// One job of the line, as a panel shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedProxy {
    pub asset_id: Uuid,
    pub name: String,
    pub state: QueueState,
    /// How far a build is, `0.0..=1.0`; `0.0` while queued.
    pub fraction: f64,
    /// Its place in the order the jobs run, from 1: a build under way is 1.
    pub position: usize,
    /// The source's length in seconds.
    pub duration: f64,
    /// The source's picture size in pixels (what the estimate weighs it by).
    pub width: u32,
    pub height: u32,
    pub pipeline: Pipeline,
    /// Seconds from now until this job is done, once there is a rate to say it by. Counts the
    /// jobs before it only; a paused queue has none for the jobs it is not building.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<f64>,
}

/// A proxy that failed this session, to be shown with a Rebuild.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailedProxy {
    pub asset_id: Uuid,
    pub name: String,
    pub reason: String,
}

/// The whole queue at one moment: what is next, how long it will take, why nothing moves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyQueue {
    /// Every job in the order it will run, the builds under way first.
    pub jobs: Vec<QueuedProxy>,
    /// How many jobs there are (`jobs.len()`).
    pub remaining: usize,
    /// Seconds from now until the last of them is done; absent with no rate yet, with nothing
    /// queued, and while paused with jobs waiting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<f64>,
    /// One worker's speed in source pixel-seconds (width x height x seconds) built per wall
    /// second, smoothed over this session's builds; absent until one has run far enough to say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    /// The pipeline the rate and the estimates are of.
    pub pipeline: Pipeline,
    pub paused: bool,
    /// What the next job is waiting for when something else has the machine: the sentence
    /// completes "waiting for ...". Absent while the slot is free, the queue is paused or empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<String>,
    /// The proxies that failed this session (of the assets asked about), with why.
    pub failed: Vec<FailedProxy>,
}

/// What a proxy waits for, in words ([`cpu::proxy_held_up_by_on`]).
fn held_up_by(lane: cpu::Priority) -> &'static str {
    match lane {
        cpu::Priority::Foreground => "an export or another job you started",
        cpu::Priority::Proxy => "another proxy",
        cpu::Priority::Background => "analysis that is already running",
    }
}

/// The queue now: [`queue`] under the settings in force.
///
/// `inputs` are the project's assets ([`ProxyInput`]s, resolved under the project lock and handed
/// over with it released, as for [`statuses`]): a job of one of them is shown under the asset's
/// current name and size and a failure of one is listed. A job of an asset that is not in
/// `inputs` (another project's, still being built) is shown under what it was queued with.
pub fn queue(inputs: &[ProxyInput]) -> ProxyQueue {
    snapshot(shared(), inputs, proxy_size(), effective_preview_source())
}

/// One job of the line with what the registry says of it.
struct Seen {
    line: schedule::Line,
    fraction: f64,
    /// When it took the slot, for a build under way.
    building: Option<Instant>,
}

fn snapshot(line: &Shared, inputs: &[ProxyInput], size: ProxySize, mode: PreviewSource) -> ProxyQueue {
    let (lines, paused, finished) = {
        let waiting = line.lock();
        let finished: HashMap<Pipeline, Option<f64>> = Pipeline::ALL.iter().map(|p| (*p, waiting.rates.finished(*p))).collect();
        (waiting.lines(), waiting.is_paused(), finished)
    };
    // The registry says how far each is, and no longer knows a job cancelled since.
    let mut seen: Vec<Seen> = Vec::with_capacity(lines.len());
    {
        let registry = jobs();
        for l in lines {
            let Some(entry) = registry.get(&l.info.key) else { continue };
            if entry.gen != l.gen || l.cancel.load(Ordering::SeqCst) {
                continue;
            }
            match &entry.live {
                Live::Building { fraction, started } => seen.push(Seen {
                    line: l,
                    fraction: *fraction,
                    building: Some(*started),
                }),
                Live::Queued => seen.push(Seen {
                    line: l,
                    fraction: 0.0,
                    building: None,
                }),
                Live::Failed(_) => {}
            }
        }
    }
    // What is being built goes first; the line already is in run order.
    seen.sort_by_key(|s| s.building.is_none());

    let known: HashMap<Uuid, &ProxyInput> = inputs.iter().map(|i| (i.asset.id, i)).collect();
    let mut loads = Vec::with_capacity(seen.len());
    let mut live: HashMap<Pipeline, Vec<(f64, f64)>> = HashMap::new();
    let mut rows = Vec::with_capacity(seen.len());
    for s in &seen {
        let info = &s.line.info;
        let (name, (width, height), duration) = match known.get(&info.asset_id) {
            Some(input) => (input.asset.name.clone(), source_size(&input.asset), input.asset.duration),
            None => (info.name.clone(), info.source, info.duration),
        };
        let cost = estimate::pixel_seconds(width, height, duration);
        if let Some(sample) = s
            .building
            .and_then(|since| estimate::live_sample(cost, s.fraction, since.elapsed().as_secs_f64()))
        {
            live.entry(info.pipeline).or_default().push(sample);
        }
        loads.push(estimate::Load {
            pixel_seconds: cost,
            fraction: s.fraction,
            running: s.building.is_some(),
        });
        rows.push((info, name, width, height, duration));
    }
    // Each pipeline is its own queue with its own speed: the cores' one slot and the hardware's two
    // are different machines, so a job is timed by the jobs of its pipeline ahead of it and the rate
    // that pipeline has measured. (The line is in run order within each.)
    let rates: HashMap<Pipeline, Option<f64>> = Pipeline::ALL
        .iter()
        .map(|p| {
            let done = finished.get(p).copied().flatten();
            (*p, estimate::current_rate(done, live.get(p).map_or(&[][..], Vec::as_slice)))
        })
        .collect();
    let workers = line.workers();
    let mut etas: Vec<Option<f64>> = vec![None; loads.len()];
    for p in Pipeline::ALL {
        let at: Vec<usize> = (0..seen.len()).filter(|i| rows[*i].0.pipeline == p).collect();
        if at.is_empty() {
            continue;
        }
        let mine: Vec<estimate::Load> = at.iter().map(|i| loads[*i]).collect();
        // With the hardware lane off a "GPU" build takes the cores' slot, one at a time.
        let parallel = if p == Pipeline::Gpu && !cpu::gpu_lane() {
            1
        } else {
            p.parallelism(workers)
        };
        for (i, eta) in at.iter().zip(estimate::finish_times(&mine, rates[&p], parallel, !paused)) {
            etas[*i] = eta;
        }
    }
    // What the rate and the pipeline of the queue say: those of the job at its head (the one being
    // built, else the next), or of what a proxy queued now would be built by when nothing waits.
    let pipeline = rows.first().map_or_else(predicted_pipeline, |row| row.0.pipeline);
    let rate = rates[&pipeline];
    let total = if etas.is_empty() || etas.iter().any(Option::is_none) {
        None
    } else {
        etas.iter().flatten().copied().reduce(f64::max)
    };
    let jobs: Vec<QueuedProxy> = seen
        .iter()
        .zip(rows)
        .zip(&etas)
        .enumerate()
        .map(|(at, ((s, (info, name, width, height, duration)), eta_secs))| QueuedProxy {
            asset_id: info.asset_id,
            name,
            state: if s.building.is_some() {
                QueueState::Building
            } else {
                QueueState::Queued
            },
            fraction: s.fraction,
            position: at + 1,
            duration,
            width,
            height,
            pipeline: info.pipeline,
            eta_secs: *eta_secs,
        })
        .collect();

    // A worker has a job and has not got the machine for it: say what has (the slot its pipeline asks
    // for: a build on the hardware's waits for foreground work only, never for another proxy).
    let waiting_on = seen
        .iter()
        .find(|s| !paused && s.line.claimed && s.building.is_none())
        .and_then(|s| {
            let slot = if s.line.info.pipeline == Pipeline::Gpu && cpu::gpu_lane() {
                cpu::Resource::Gpu
            } else {
                cpu::Resource::Cpu
            };
            cpu::proxy_held_up_by_on(slot)
        })
        .map(|lane| held_up_by(lane).to_string());

    let mut failed = Vec::new();
    for input in inputs.iter().filter(|i| needs_proxy(&i.asset)) {
        if !matches!(live_of(&job_key(&input.asset)), Some(Live::Failed(_))) {
            continue;
        }
        let status = status_under(input, size, mode);
        if status.state == ProxyPhase::Failed {
            failed.push(FailedProxy {
                asset_id: input.asset.id,
                name: input.asset.name.clone(),
                reason: status.reason.unwrap_or_default(),
            });
        }
    }

    ProxyQueue {
        remaining: jobs.len(),
        jobs,
        eta_secs: total,
        rate,
        pipeline,
        paused,
        waiting_on,
        failed,
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
            color_range: None,
            lead: 0.0,
            time_base: None,
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
            music_score: None,
            sound_effect: None,
            tags: Vec::new(),
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

    // ---- the line, through the public calls ---------------------------------------------
    //
    // These run on a line of their own with no workers (`TestLine`): the process's has real ones,
    // which would take the made-up files below to the encoder. They hold the gate's test lock
    // because a queued job holds a place in the heavy-job queue and the gate's own tests count them.

    struct TestLine {
        line: &'static Shared,
        tag: String,
        _gate: std::sync::MutexGuard<'static, ()>,
    }

    impl TestLine {
        fn new(tag: &str) -> Self {
            Self::with_notify_interval(tag, schedule::QUEUE_NOTIFY_EVERY)
        }

        fn with_notify_interval(tag: &str, every: Duration) -> Self {
            Self::build(tag, 1, every)
        }

        fn build(tag: &str, workers: usize, every: Duration) -> Self {
            let gate = cpu::test_lock();
            let line: &'static Shared = Box::leak(Box::new(Shared::with_notify_interval(workers, every)));
            TEST_LINE.with(|l| l.set(Some(line)));
            assert!(!cpu::high_pending(), "a place was left held by an earlier test");
            Self {
                line,
                tag: format!("q-{tag}"),
                _gate: gate,
            }
        }

        /// A 1080p video of 60 s (see [`asset`]) of this test's own.
        fn video(&self, name: &str) -> Asset {
            asset(vec![stream(StreamKind::Video, false)], &format!("{}-{name}", self.tag))
        }

        /// A 4K video of 30 s.
        fn uhd(&self, name: &str) -> Asset {
            let mut s = stream(StreamKind::Video, false);
            (s.width, s.height) = (Some(3840), Some(2160));
            let mut a = asset(vec![s], &format!("{}-{name}", self.tag));
            a.duration = 30.0;
            a
        }

        fn names(&self, inputs: &[ProxyInput]) -> Vec<String> {
            queue(inputs).jobs.into_iter().map(|j| j.name).collect()
        }
    }

    impl Drop for TestLine {
        fn drop(&mut self) {
            TEST_LINE.with(|l| l.set(None));
            self.line.lock().clear_pending();
            let tag = format!("-{}-", self.tag);
            jobs().retain(|key, _| !key.0.contains(&tag));
        }
    }

    fn quiet() -> Notify {
        Arc::new(|_| {})
    }

    fn inputs(assets: &[&Asset]) -> Vec<ProxyInput> {
        assets.iter().map(|a| input((*a).clone())).collect()
    }

    fn called(assets: &[&Asset]) -> Vec<String> {
        assets.iter().map(|a| a.name.clone()).collect()
    }

    fn ids(assets: &[&Asset]) -> HashSet<Uuid> {
        assets.iter().map(|a| a.id).collect()
    }

    #[test]
    fn the_line_runs_what_was_asked_for_then_what_a_preview_waits_on_then_what_the_cut_uses_then_the_rest() {
        let t = TestLine::new("order");
        let all: Vec<Asset> = ["a", "b", "c", "d", "e", "f"].iter().map(|n| t.video(n)).collect();
        let [a, b, c, d, e, f] = &all[..] else { unreachable!() };
        for asset in &all {
            assert!(submit(asset, quiet(), Ask::Auto).state == ProxyPhase::Queued);
        }
        let inputs = inputs(&all.iter().collect::<Vec<_>>());
        let name = |a: &Asset| a.name.clone();
        let order = |t: &TestLine| t.names(&inputs);
        assert_eq!(order(&t), [a, b, c, d, e, f].map(name), "arrival order to begin with");

        // The cut uses b, d and e; the preview waits on e and f.
        rerank(&ids(&[e, f]), &ids(&[b, d, e]));
        assert_eq!(order(&t), [e, f, b, d, a, c].map(name));

        // The playhead moves: the preview now waits on c alone.
        rerank(&ids(&[c]), &ids(&[b, d, e]));
        assert_eq!(order(&t), [c, b, d, e, a, f].map(name));

        // Asking for a to be built next goes before all of it, and a later ask before that.
        build_next(a, quiet()).unwrap();
        assert_eq!(order(&t), [a, c, b, d, e, f].map(name));
        build_next(f, quiet()).unwrap();
        assert_eq!(order(&t), [f, a, c, b, d, e].map(name), "the latest ask is first");
        // A ranking after an ask does not undo it.
        rerank(&ids(&[]), &ids(&[]));
        assert_eq!(order(&t), [f, a, b, c, d, e].map(name));
        // The positions say the same.
        let jobs = queue(&inputs).jobs;
        assert_eq!(jobs.iter().map(|j| j.position).collect::<Vec<_>>(), [1, 2, 3, 4, 5, 6]);

        // And that is the order the workers take them in.
        let mut taken = Vec::new();
        while let Some(claimed) = t.line.lock().claim_next() {
            taken.push(claimed.job.info.name);
        }
        assert_eq!(taken, [f, a, b, c, d, e].map(name));
    }

    #[test]
    fn a_job_queued_by_an_import_after_the_ranking_is_ranked_by_it() {
        let t = TestLine::new("late");
        let (a, b) = (t.video("a"), t.video("b"));
        submit(&a, quiet(), Ask::Auto);
        rerank(&ids(&[]), &ids(&[&b]));
        // b arrives later but the cut uses it.
        submit(&b, quiet(), Ask::Auto);
        assert_eq!(t.names(&inputs(&[&a, &b])), called(&[&b, &a]));
    }

    #[test]
    fn building_the_next_one_of_a_job_a_worker_has_takes_nothing_from_it() {
        let t = TestLine::new("held");
        let (a, b) = (t.video("a"), t.video("b"));
        for asset in [&a, &b] {
            submit(asset, quiet(), Ask::Auto);
        }
        let held = t.line.lock().claim_next().unwrap();
        assert_eq!(held.job.info.name, a.name);
        build_next(&b, quiet()).unwrap();
        // a has not started: it steps back for b.
        assert!(held.give_way.load(Ordering::SeqCst));
        // Once it has the slot, an ask for b waits behind it.
        held.give_way.store(false, Ordering::SeqCst);
        held.holds_slot.store(true, Ordering::SeqCst);
        set_live(
            &held.job.info.key,
            held.job.gen,
            Live::Building {
                fraction: 0.1,
                started: Instant::now(),
            },
        );
        build_next(&a, quiet()).unwrap();
        build_next(&b, quiet()).unwrap();
        assert!(!held.give_way.load(Ordering::SeqCst));
        let q = queue(&inputs(&[&a, &b]));
        assert_eq!(
            q.jobs.iter().map(|j| (j.name.as_str(), j.state)).collect::<Vec<_>>(),
            [(a.name.as_str(), QueueState::Building), (b.name.as_str(), QueueState::Queued)]
        );
    }

    #[test]
    fn building_next_what_is_ready_changes_nothing_and_what_is_not_wanted_is_refused() {
        let t = TestLine::new("refuse");
        let still = asset(vec![stream(StreamKind::Video, true)], &format!("{}-still", t.tag));
        assert!(build_next(&still, quiet()).is_err());
        assert!(queue(&[]).jobs.is_empty());
    }

    #[test]
    fn cancelling_one_returns_its_place_records_no_failure_and_is_not_queued_again_by_itself() {
        let t = TestLine::new("cancel");
        let (a, b) = (t.video("a"), t.video("b"));
        let all = inputs(&[&a, &b]);
        for i in &all {
            assert!(queue_auto(i, quiet()).is_some());
        }
        assert!(cpu::high_pending(), "two places are held in front of analysis");

        assert!(cancel(&a));
        // The job is gone from the queue and is not a failure: its asset is simply not built.
        let q = queue(&all);
        assert_eq!(q.jobs.iter().map(|j| j.name.clone()).collect::<Vec<_>>(), called(&[&b]));
        assert!(q.failed.is_empty());
        assert_eq!(status(&all[0]).state, ProxyPhase::Missing);
        // Cancelling what is not queued is nothing.
        assert!(!cancel(&a));
        assert!(!cancel(&t.video("never-queued")));

        // The import, a project open and the like do not queue it again by themselves...
        assert!(queue_auto(&all[0], quiet()).is_none());
        assert_eq!(status(&all[0]).state, ProxyPhase::Missing);
        // ...while the other one, which nobody cancelled, still is queued by them (a no-op: it is).
        assert_eq!(queue_auto(&all[1], quiet()).unwrap().state, ProxyPhase::Queued);
        assert_eq!(queue(&all).jobs.len(), 1);

        // Its place was given back at once: cancelling the other one leaves none held.
        assert!(cancel(&b));
        assert!(!cpu::high_pending());
        assert!(queue(&all).jobs.is_empty());

        // A build next asks for it on purpose: it is queued again, at the front.
        build_next(&b, quiet()).unwrap();
        assert!(cpu::high_pending());
        assert_eq!(t.names(&all), called(&[&b]));
        // So does a rebuild (which also drops the file it has, none here).
        assert!(queue_auto(&all[0], quiet()).is_none(), "a is still cancelled");
        rebuild(&a, quiet()).unwrap();
        assert_eq!(t.names(&all), called(&[&b, &a]));
        // And it is a job like any other again: queue_auto leaves it be.
        assert_eq!(queue_auto(&all[0], quiet()).unwrap().state, ProxyPhase::Queued);
        assert_eq!(queue(&all).jobs.len(), 2);
    }

    #[test]
    fn cancelling_a_job_a_worker_has_stops_it_and_gives_its_place_back_when_it_notices() {
        let t = TestLine::new("running");
        let (a, b) = (t.video("a"), t.video("b"));
        let all = inputs(&[&a, &b]);
        for asset in [&a, &b] {
            submit(asset, quiet(), Ask::Auto);
        }
        // A worker has a; it is encoding.
        let claimed = t.line.lock().claim_next().unwrap();
        claimed.holds_slot.store(true, Ordering::SeqCst);
        set_live(
            &claimed.job.info.key,
            claimed.job.gen,
            Live::Building {
                fraction: 0.4,
                started: Instant::now(),
            },
        );
        assert_eq!(queue(&all).jobs[0].state, QueueState::Building);

        assert!(cancel(&a));
        // It is told to stop (the encode kills ffmpeg and removes its partial file), and the queue
        // no longer lists it though the worker has not yet noticed.
        assert!(claimed.job.cancel.load(Ordering::SeqCst));
        assert_eq!(t.names(&all), called(&[&b]));
        assert_eq!(status(&all[0]).state, ProxyPhase::Missing);
        // The worker ends it as a cancel (no failure) and the line forgets it.
        finish(&claimed.job.info.key, claimed.job.gen, None);
        let key = claimed.job.info.key.clone();
        finished(t.line, &key, claimed.job.gen, None);
        assert_eq!(status(&all[0]).state, ProxyPhase::Missing);
        assert!(queue(&all).failed.is_empty());
        // The other job is the one in line, still holding its place.
        assert!(cpu::high_pending());
        assert_eq!(t.line.lock().claim_next().unwrap().job.info.name, b.name);
    }

    #[test]
    fn cancel_all_returns_every_asset_and_none_comes_back_by_itself_until_a_setting_changes() {
        let t = TestLine::new("all");
        let all: Vec<Asset> = ["a", "b", "c"].iter().map(|n| t.video(n)).collect();
        let ins = inputs(&all.iter().collect::<Vec<_>>());
        // c failed earlier: a failure is not a job, and Cancel all leaves it be.
        queue_auto(&ins[2], quiet());
        let claimed = t.line.lock().claim_next().unwrap();
        finish(&claimed.job.info.key, claimed.job.gen, Some("boom".to_string()));
        let key = claimed.job.info.key.clone();
        finished(t.line, &key, claimed.job.gen, None);
        drop(claimed);
        queue_auto(&ins[0], quiet());
        queue_auto(&ins[1], quiet());
        assert_eq!(queue(&ins).jobs.len(), 2);

        let mut cancelled = cancel_all();
        cancelled.sort();
        let mut want = vec![all[0].id, all[1].id];
        want.sort();
        assert_eq!(cancelled, want);
        assert!(!cpu::high_pending());
        let q = queue(&ins);
        assert!(q.jobs.is_empty());
        assert_eq!(q.failed.len(), 1);
        assert_eq!(q.failed[0].asset_id, all[2].id);
        assert!(ins.iter().take(2).all(|i| queue_auto(i, quiet()).is_none()));
        // A change of settings starts the queue afresh: what was cancelled is wanted again.
        let tag = format!("-{}-", t.tag);
        abandon(|key| key.0.contains(&tag));
        assert!(ins.iter().take(2).all(|i| queue_auto(i, quiet()).is_some()));
        // ...and abandoning took the failure along: nothing remembers it, and it is queued again too.
        assert!(queue(&ins).failed.is_empty());
        assert!(queue_auto(&ins[2], quiet()).is_some());
        assert_eq!(queue(&ins).jobs.len(), 3);
    }

    #[test]
    fn pausing_stops_what_has_not_started_and_frees_analysis_and_resuming_goes_on() {
        let t = TestLine::new("pause");
        let (a, b, c) = (t.video("a"), t.video("b"), t.video("c"));
        let ins = inputs(&[&a, &b, &c]);
        submit(&a, quiet(), Ask::Auto);
        submit(&b, quiet(), Ask::Auto);
        assert!(cpu::high_pending());
        // A worker has a and is waiting for the machine (behind an export, say); the place it
        // holds is the one its wait for the slot was given.
        let mut waiting = t.line.lock().claim_next().unwrap();
        let place = waiting.job.reservation.take();
        assert!(!waiting.give_way.load(Ordering::SeqCst));

        assert!(pause());
        assert!(!pause(), "already paused");
        assert!(is_paused());
        assert!(
            waiting.give_way.load(Ordering::SeqCst),
            "the job waiting for the machine steps back"
        );
        assert!(t.line.lock().claim_next().is_none(), "nothing new starts");
        // The queued job's place is given up at once; the waiting worker's is its own, and goes
        // when its wait ends (within a quarter of a second).
        assert!(cpu::high_pending());
        drop(place);
        assert!(!cpu::high_pending(), "paused proxies must not hold analysis back");
        let q = queue(&ins);
        assert!(q.paused);
        assert_eq!(q.jobs.len(), 2);
        assert!(q.waiting_on.is_none());

        // A job that arrives meanwhile waits and holds no place either.
        submit(&c, quiet(), Ask::Auto);
        assert!(!cpu::high_pending());
        assert_eq!(queue(&ins).jobs.len(), 3);

        // The worker steps back: the job is queued again where it was, and still `queued`.
        let key = waiting.job.info.key.clone();
        finished(t.line, &key, waiting.job.gen, Some(waiting.job));
        assert_eq!(status(&ins[0]).state, ProxyPhase::Queued);
        assert_eq!(t.names(&ins), called(&[&a, &b, &c]));
        assert!(!cpu::high_pending());

        assert!(resume());
        assert!(!resume());
        assert!(!is_paused());
        assert!(cpu::high_pending(), "the three places are taken again");
        assert_eq!(t.line.lock().claim_next().unwrap().job.info.name, a.name);
    }

    #[test]
    fn a_paused_queue_has_a_time_for_the_build_under_way_and_none_for_the_rest() {
        let t = TestLine::new("paused-eta");
        let (a, b) = (t.video("a"), t.video("b"));
        let ins = inputs(&[&a, &b]);
        for asset in [&a, &b] {
            submit(asset, quiet(), Ask::Auto);
        }
        let claimed = t.line.lock().claim_next().unwrap();
        claimed.holds_slot.store(true, Ordering::SeqCst);
        set_live(
            &claimed.job.info.key,
            claimed.job.gen,
            Live::Building {
                fraction: 0.5,
                started: Instant::now() - Duration::from_secs(10),
            },
        );
        let cost = estimate::pixel_seconds(1920, 1080, 60.0);
        t.line.lock().rates.record(Pipeline::Software, cost, 20.0);
        let running = queue(&ins);
        assert!(running.eta_secs.is_some() && running.jobs.iter().all(|j| j.eta_secs.is_some()));
        pause();
        let paused = queue(&ins);
        assert!(paused.jobs[0].eta_secs.is_some(), "the build under way still has a time");
        assert_eq!(paused.jobs[1].eta_secs, None);
        assert_eq!(paused.eta_secs, None, "the queue as a whole does not, while it is paused");
        resume();
    }

    #[test]
    fn the_estimate_weighs_a_clip_by_its_pixels_and_counts_the_build_under_way() {
        let t = TestLine::new("estimate");
        let (a, uhd, b) = (t.video("a"), t.uhd("uhd"), t.video("b"));
        let ins = inputs(&[&a, &uhd, &b]);
        for asset in [&a, &uhd, &b] {
            submit(asset, quiet(), Ask::Auto);
        }
        // No build has run: no rate, no estimate, and no guess.
        let q = queue(&ins);
        assert_eq!((q.rate, q.eta_secs), (None, None));
        assert!(q.jobs.iter().all(|j| j.eta_secs.is_none()));
        assert_eq!(q.pipeline, Pipeline::Software);
        assert_eq!(q.remaining, 3);

        // a is being built, halfway after 30 s: past 2% so its own speed is the rate (no finished one
        // yet) — 1920 x 1080 x 30 pixel-seconds in 30 s.
        let claimed = t.line.lock().claim_next().unwrap();
        claimed.holds_slot.store(true, Ordering::SeqCst);
        set_live(
            &claimed.job.info.key,
            claimed.job.gen,
            Live::Building {
                fraction: 0.5,
                started: Instant::now() - Duration::from_secs(30),
            },
        );
        let q = queue(&ins);
        let rate = q.rate.expect("a build past 2% says the rate");
        let hd_30 = 1920.0 * 1080.0 * 30.0;
        assert!((rate - hd_30 / 30.0).abs() / rate < 0.01, "{rate}");
        let eta = |name: &str| q.jobs.iter().find(|j| j.name == name).unwrap().eta_secs.unwrap();
        // The rest of a: 30 s. The 4K clip is 30 s long and four times the pixels: 60 s more than
        // 30 s of 1080p. b is 60 s of 1080p: 60 s.
        assert!((eta(&a.name) - 30.0).abs() < 1.0, "{}", eta(&a.name));
        assert!((eta(&uhd.name) - 30.0 - 120.0).abs() < 2.0, "{}", eta(&uhd.name));
        assert!((eta(&b.name) - 30.0 - 120.0 - 60.0).abs() < 3.0, "{}", eta(&b.name));
        assert_eq!(q.eta_secs, Some(eta(&b.name)));
        // The build under way is first, with its fraction and position.
        assert_eq!(q.jobs[0].name, a.name);
        assert_eq!(
            (q.jobs[0].state, q.jobs[0].fraction, q.jobs[0].position),
            (QueueState::Building, 0.5, 1)
        );
        assert_eq!((q.jobs[1].width, q.jobs[1].height, q.jobs[1].duration), (3840, 2160, 30.0));

        // A finished build sets the rate it ran at.
        t.line.lock().rates.record(Pipeline::Software, hd_30, 10.0);
        let q = queue(&ins);
        assert!(q.rate.unwrap() > rate, "faster than the running one, which was slower");
    }

    #[test]
    fn more_workers_do_not_shorten_the_estimate_of_an_encode_that_has_one_slot() {
        let t = TestLine::build("workers", 2, schedule::QUEUE_NOTIFY_EVERY);
        let (a, b) = (t.video("a"), t.video("b"));
        for asset in [&a, &b] {
            submit(asset, quiet(), Ask::Auto);
        }
        t.line.lock().rates.record(Pipeline::Software, 1920.0 * 1080.0 * 60.0, 10.0);
        let q = queue(&inputs(&[&a, &b]));
        // 10 s each, one after the other: the second worker would be waiting at the gate.
        let etas: Vec<f64> = q.jobs.iter().map(|j| j.eta_secs.unwrap()).collect();
        assert!((etas[0] - 10.0).abs() < 1e-6 && (etas[1] - 20.0).abs() < 1e-6, "{etas:?}");
        assert_eq!(Pipeline::Software.parallelism(2), 1);
    }

    // ---- Fast (GPU): the setting, the pipelines of the queue ---------------------------------------------

    #[test]
    fn the_encoder_setting_is_snake_case_and_off_by_default_and_a_name_it_does_not_know_is_an_error() {
        assert_eq!(ProxyEncoder::default(), ProxyEncoder::Software);
        assert_eq!(serde_json::to_string(&ProxyEncoder::Software).unwrap(), "\"software\"");
        assert_eq!(serde_json::to_string(&ProxyEncoder::FastGpu).unwrap(), "\"fast_gpu\"");
        for (stored, read) in [
            ("\"software\"", ProxyEncoder::Software),
            ("\"fast_gpu\"", ProxyEncoder::FastGpu),
        ] {
            assert_eq!(serde_json::from_str::<ProxyEncoder>(stored).unwrap(), read, "{stored}");
        }
        // Not guessed at: the settings file reads a value like these as the default and keeps the text
        // (`settings::parse`), so a newer build's choice survives a save by this one.
        for unknown in ["\"hardware_everything\"", "\"\"", "1", "null"] {
            assert!(serde_json::from_str::<ProxyEncoder>(unknown).is_err(), "{unknown}");
        }
    }

    #[test]
    fn the_gpu_pipeline_has_a_slot_with_room_for_two_and_the_software_one_has_one() {
        assert_eq!(Pipeline::default(), Pipeline::Software);
        for workers in 1..=8 {
            assert_eq!(Pipeline::Software.parallelism(workers), 1, "the cores' one slot");
        }
        assert_eq!(Pipeline::Gpu.parallelism(1), 1);
        assert_eq!(Pipeline::Gpu.parallelism(2), 2);
        assert_eq!(
            Pipeline::Gpu.parallelism(8),
            cpu::MAX_GPU_JOBS,
            "never more than the hardware's slots"
        );
        assert_eq!(Pipeline::Gpu.parallelism(0), 1);
        assert_eq!(serde_json::to_string(&Pipeline::Gpu).unwrap(), "\"gpu\"");
        assert_eq!(serde_json::from_str::<Pipeline>("\"gpu\"").unwrap(), Pipeline::Gpu);
        assert_eq!((Pipeline::Software.as_str(), Pipeline::Gpu.as_str()), ("software", "gpu"));
        assert_eq!(Pipeline::of(engine::ProxyPipeline::Software), Pipeline::Software);
        assert_eq!(
            Pipeline::of(engine::ProxyPipeline::Gpu(engine::FastVendor::Amf)),
            Pipeline::Gpu
        );
    }

    #[test]
    fn the_queue_runs_two_workers_only_while_fast_gpu_is_on_and_likely_and_the_override_wins() {
        // Software stays one worker; Fast (GPU) with the hardware lane on is two.
        assert_eq!(workers_for(None, false, true), 1);
        assert_eq!(workers_for(None, true, true), cpu::MAX_GPU_JOBS);
        // With the lane off a "GPU" build is a job on the cores' one slot: a second worker would only wait.
        assert_eq!(workers_for(None, true, false), 1);
        // KERF_PROXY_WORKERS says how many, whatever else is true (at least one).
        for (fast, lane) in [(false, true), (true, true), (true, false)] {
            assert_eq!(workers_for(Some(4), fast, lane), 4);
            assert_eq!(workers_for(Some(0), fast, lane), 1);
        }
    }

    #[test]
    fn a_gpu_job_is_timed_by_its_own_rate_and_two_of_them_run_at_once() {
        let t = TestLine::build("gpu-estimates", 2, schedule::QUEUE_NOTIFY_EVERY);
        let (a, b, c, d) = (t.video("a"), t.video("b"), t.video("c"), t.video("d"));
        let hd_60 = 1920.0 * 1080.0 * 60.0;
        // a and b are GPU jobs, c and d software: the line is in arrival order.
        submit(&a, quiet(), Ask::Auto);
        submit(&b, quiet(), Ask::Auto);
        t.line.lock().retag_pending(Pipeline::Gpu);
        submit(&c, quiet(), Ask::Auto);
        submit(&d, quiet(), Ask::Auto);
        let ins = inputs(&[&a, &b, &c, &d]);
        // No rate for either yet: no guess.
        assert!(queue(&ins).jobs.iter().all(|j| j.eta_secs.is_none()));
        // The GPU builds a clip in 2 s and x264 in 10: each pipeline is its own queue with its own speed.
        t.line.lock().rates.record(Pipeline::Gpu, hd_60, 2.0);
        t.line.lock().rates.record(Pipeline::Software, hd_60, 10.0);
        let q = queue(&ins);
        let eta = |name: &str| q.jobs.iter().find(|j| j.name == name).unwrap().eta_secs.unwrap();
        // Two GPU jobs side by side (the hardware's two slots), the software ones one after the other.
        assert!(
            (eta(&a.name) - 2.0).abs() < 1e-6 && (eta(&b.name) - 2.0).abs() < 1e-6,
            "{q:?}"
        );
        assert!(
            (eta(&c.name) - 10.0).abs() < 1e-6 && (eta(&d.name) - 20.0).abs() < 1e-6,
            "{q:?}"
        );
        // All done when the slowest is: the software queue.
        assert_eq!(q.eta_secs, Some(20.0));
        // The queue says what its head is built by and at what speed.
        assert_eq!((q.pipeline, q.rate), (Pipeline::Gpu, Some(hd_60 / 2.0)));
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(json["pipeline"], "gpu");
        assert_eq!(
            json["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|j| j["pipeline"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["gpu", "gpu", "software", "software"]
        );
        let back: ProxyQueue = serde_json::from_value(json).unwrap();
        assert_eq!(back, q);
    }

    #[test]
    fn a_job_a_worker_says_it_builds_in_software_is_labelled_and_timed_as_software() {
        let t = TestLine::new("gpu-fallback");
        let a = t.video("a");
        submit(&a, quiet(), Ask::Auto);
        t.line.lock().retag_pending(Pipeline::Gpu);
        let claimed = t.line.lock().claim_next().unwrap();
        let ins = inputs(&[&a]);
        assert_eq!(queue(&ins).jobs[0].pipeline, Pipeline::Gpu);
        // The GPU attempt failed and x264 took the proxy: the worker is told, the queue says so.
        t.line.lock().set_pipeline(
            &claimed.job.info.key,
            claimed.job.gen,
            Pipeline::of(engine::ProxyPipeline::Software),
        );
        let q = queue(&ins);
        assert_eq!((q.jobs[0].pipeline, q.pipeline), (Pipeline::Software, Pipeline::Software));
        // And what it took to build counts toward the software rate, not the GPU's.
        record_build(t.line, &claimed.job.info, engine::ProxyPipeline::Software, 12.0);
        assert!(t.line.lock().rates.finished(Pipeline::Software).is_some());
        assert!(t.line.lock().rates.finished(Pipeline::Gpu).is_none());
    }

    #[test]
    fn turning_fast_gpu_on_relabels_what_waits_and_turning_it_off_puts_it_back() {
        let t = TestLine::new("gpu-setting");
        let _flags = engine::fast_test_lock();
        let (a, b) = (t.video("a"), t.video("b"));
        let ins = inputs(&[&a, &b]);
        submit(&a, quiet(), Ask::Auto);
        assert_eq!(proxy_encoder(), ProxyEncoder::Software);
        assert_eq!(queue(&ins).jobs[0].pipeline, Pipeline::Software);
        // On, on a machine whose check found a pipeline: the waiting job will be a GPU one, and so will the next.
        engine::with_pinned_fast_check(Some(engine::FastVendor::Amf), || {
            set_proxy_encoder(ProxyEncoder::FastGpu);
            assert_eq!(proxy_encoder(), ProxyEncoder::FastGpu);
            assert!(engine::proxy_fast_gpu_is_on());
            assert_eq!(queue(&ins).jobs[0].pipeline, Pipeline::Gpu);
            submit(&b, quiet(), Ask::Auto);
            assert_eq!(queue(&ins).jobs[1].pipeline, Pipeline::Gpu);
            assert_eq!(t.line.workers(), 1, "this line has the one worker it was made with");
            assert!(fast_gpu_status().available);
            assert_eq!(fast_gpu_status().pipeline.as_deref(), Some("AMD AMF"));
            set_proxy_encoder(ProxyEncoder::Software);
            assert!(!engine::proxy_fast_gpu_is_on());
            assert!(queue(&ins).jobs.iter().all(|j| j.pipeline == Pipeline::Software));
        });
        // On, on a machine where nothing passed the check: nothing is labelled GPU.
        engine::with_pinned_fast_check(None, || {
            set_proxy_encoder(ProxyEncoder::FastGpu);
            assert!(queue(&ins).jobs.iter().all(|j| j.pipeline == Pipeline::Software));
            let status = fast_gpu_status();
            assert!(!status.available && status.checked && status.pipeline.is_none());
            assert!(status.reason.unwrap().contains("nothing passed"));
            assert!(!fast_likely());
            set_proxy_encoder(ProxyEncoder::Software);
        });
        assert_eq!(proxy_encoder(), ProxyEncoder::Software);
    }

    #[test]
    fn the_fast_gpu_status_is_json_with_no_keys_for_what_it_does_not_know() {
        let found = FastGpuStatus {
            available: true,
            checked: true,
            pipeline: Some("NVIDIA NVENC".to_string()),
            reason: None,
        };
        let json = serde_json::to_value(&found).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"available": true, "checked": true, "pipeline": "NVIDIA NVENC"})
        );
        let not_yet = FastGpuStatus {
            available: false,
            checked: false,
            pipeline: None,
            reason: Some("not checked yet".to_string()),
        };
        let json = serde_json::to_value(&not_yet).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"available": false, "checked": false, "reason": "not checked yet"})
        );
        assert_eq!(serde_json::from_value::<FastGpuStatus>(json).unwrap(), not_yet);
    }

    #[test]
    fn a_job_waits_on_an_export_and_the_queue_says_so() {
        let t = TestLine::new("waiting");
        let a = t.video("a");
        let ins = inputs(&[&a]);
        // An analysis has the slot (taken before the proxy is queued: a queued proxy would keep it
        // out, which is the point of its place).
        let analysis = cpu::lease_priority(cpu::Priority::Background);
        submit(&a, quiet(), Ask::Auto);
        // Not claimed by a worker: nothing is waiting for the machine, the job is just in line.
        assert_eq!(queue(&ins).waiting_on, None);
        let claimed = t.line.lock().claim_next().unwrap();
        let said = queue(&ins).waiting_on.expect("an analysis holds the slot");
        assert!(said.contains("analysis"), "{said}");
        drop(analysis);
        // The slot is free: the worker is about to take it, it waits for nothing.
        assert_eq!(queue(&ins).waiting_on, None);
        let export = cpu::lease_priority(cpu::Priority::Foreground);
        let said = queue(&ins).waiting_on.expect("an export holds the slot");
        assert!(said.contains("export"), "{said}");
        // A paused queue is not waiting for anything: the worker steps back.
        t.line.lock().set_paused(true);
        assert_eq!(queue(&ins).waiting_on, None);
        t.line.lock().set_paused(false);
        drop(export);
        assert_eq!(queue(&ins).waiting_on, None);
        // Once the build has the slot it waits for nothing, whatever else is queued behind it.
        claimed.holds_slot.store(true, Ordering::SeqCst);
        set_live(
            &claimed.job.info.key,
            claimed.job.gen,
            Live::Building {
                fraction: 0.0,
                started: Instant::now(),
            },
        );
        let export = cpu::lease_priority(cpu::Priority::Foreground);
        assert_eq!(queue(&ins).waiting_on, None);
        drop(export);
    }

    #[test]
    fn a_failed_proxy_is_listed_with_its_reason_until_it_is_rebuilt() {
        let t = TestLine::new("failed");
        let (a, b) = (t.video("a"), t.video("b"));
        let ins = inputs(&[&a, &b]);
        for asset in [&a, &b] {
            submit(asset, quiet(), Ask::Auto);
        }
        let claimed = t.line.lock().claim_next().unwrap();
        finish(
            &claimed.job.info.key,
            claimed.job.gen,
            Some("could not generate preview proxy: broken".to_string()),
        );
        let key = claimed.job.info.key.clone();
        finished(t.line, &key, claimed.job.gen, None);
        let q = queue(&ins);
        assert_eq!(q.jobs.len(), 1, "a failure is not a job");
        assert_eq!(q.failed.len(), 1);
        assert_eq!((q.failed[0].asset_id, q.failed[0].name.as_str()), (a.id, a.name.as_str()));
        assert!(q.failed[0].reason.contains("broken"));
        // An asset the project does not hold is not listed (another project's failure).
        assert!(queue(&ins[1..]).failed.is_empty());
        // A rebuild is the way out.
        rebuild(&a, quiet()).unwrap();
        let q = queue(&ins);
        assert!(q.failed.is_empty());
        assert_eq!(q.jobs.len(), 2);
    }

    #[test]
    fn the_queue_tells_its_listener_of_every_change_of_order_state_or_jobs_and_not_of_a_ranking_that_moves_nothing() {
        let t = TestLine::with_notify_interval("listener", Duration::from_millis(40));
        let told = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = told.clone();
        t.line.listen(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        let settle = || {
            // The collector tells at once, then once more for what came in its interval.
            std::thread::sleep(Duration::from_millis(250));
            told.load(Ordering::SeqCst)
        };
        let (a, b) = (t.video("a"), t.video("b"));
        submit(&a, quiet(), Ask::Auto);
        submit(&b, quiet(), Ask::Auto);
        let after_submits = settle();
        assert!((1..=2).contains(&after_submits), "{after_submits}");

        // A ranking that changes the order is told; the same one again is not.
        rerank(&ids(&[&b]), &ids(&[]));
        let ranked = settle();
        assert_eq!(ranked, after_submits + 1);
        rerank(&ids(&[&b]), &ids(&[]));
        assert_eq!(settle(), ranked, "nothing moved");
        // A ranking of nothing that is here moves nothing either.
        rerank(&ids(&[]), &ids(&[]));
        assert_eq!(settle(), ranked + 1, "b went back behind a");
        rerank(&HashSet::from([Uuid::new_v4()]), &ids(&[]));
        assert_eq!(settle(), ranked + 1, "an asset that is not queued moves nothing");

        let before = told.load(Ordering::SeqCst);
        pause();
        assert_eq!(settle(), before + 1);
        resume();
        assert_eq!(settle(), before + 2);
        cancel(&a);
        assert_eq!(settle(), before + 3);
        // Nothing to cancel: nothing to tell.
        cancel(&a);
        assert_eq!(settle(), before + 3);
        build_next(&b, quiet()).unwrap();
        assert_eq!(settle(), before + 4);
        // A job that is already first is no change either.
        build_next(&b, quiet()).unwrap();
        assert!(settle() <= before + 5);
    }

    #[test]
    fn the_cut_s_assets_are_those_on_video_tracks_that_are_not_generated() {
        use crate::model::{Clip, Generator, TitleClip, Track};
        let (video, overlay, audio_only, title_asset) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut timeline = Timeline::new();
        let mut v1 = Track::new(StreamKind::Video, "V1");
        v1.clips.push(Clip::new(video, 0.0, 5.0, 0.0));
        v1.clips.push(Clip::new(video, 5.0, 8.0, 5.0));
        let mut title = Clip::new(title_asset, 0.0, 2.0, 0.0);
        title.generator = Some(Generator::Title(TitleClip {
            name: "t".to_string(),
            svg: "<svg/>".to_string(),
            template: None,
        }));
        v1.clips.push(title);
        let mut v2 = Track::new(StreamKind::Video, "V2");
        v2.clips.push(Clip::new(overlay, 0.0, 3.0, 1.0));
        let mut a1 = Track::new(StreamKind::Audio, "A1");
        a1.clips.push(Clip::new(audio_only, 0.0, 3.0, 0.0));
        timeline.tracks = vec![v1, v2, a1];
        assert_eq!(used_assets(&timeline), HashSet::from([video, overlay]));
        assert!(used_assets(&Timeline::new()).is_empty());

        let waits = [ProxyWait {
            asset_id: video,
            name: "v".to_string(),
            status: ProxyStatus::new(video, ProxyPhase::Queued),
        }];
        assert_eq!(waiting_assets(&waits), HashSet::from([video]));
    }

    #[test]
    fn the_queue_is_json_with_snake_case_names_and_no_keys_for_what_it_does_not_know() {
        let t = TestLine::new("json");
        let a = t.video("a");
        submit(&a, quiet(), Ask::Auto);
        let q = queue(&inputs(&[&a]));
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(json["remaining"], 1);
        assert_eq!(json["paused"], false);
        assert_eq!(json["pipeline"], "software");
        assert!(json.get("eta_secs").is_none() && json.get("rate").is_none() && json.get("waiting_on").is_none());
        assert_eq!(json["failed"], serde_json::json!([]));
        let job = &json["jobs"][0];
        assert_eq!(job["state"], "queued");
        assert_eq!(job["pipeline"], "software");
        assert_eq!(
            (job["position"].as_u64(), job["width"].as_u64(), job["height"].as_u64()),
            (Some(1), Some(1920), Some(1080))
        );
        assert_eq!(job["duration"], 60.0);
        assert_eq!(job["fraction"], 0.0);
        assert_eq!(job["asset_id"], a.id.to_string());
        assert!(job.get("eta_secs").is_none());
        let back: ProxyQueue = serde_json::from_value(json).unwrap();
        assert_eq!(back, q);
        assert_eq!(serde_json::to_string(&QueueState::Building).unwrap(), "\"building\"");
    }

    #[test]
    fn a_job_that_stepped_back_is_dropped_when_it_was_cancelled_while_it_did() {
        let t = TestLine::new("stepback-cancel");
        let a = t.video("a");
        submit(&a, quiet(), Ask::Auto);
        let claimed = t.line.lock().claim_next().unwrap();
        assert!(cancel(&a));
        let key = claimed.job.info.key.clone();
        finished(t.line, &key, claimed.job.gen, Some(claimed.job));
        assert!(queue(&inputs(&[&a])).jobs.is_empty());
        assert!(!cpu::high_pending(), "a cancelled job that stepped back holds no place");
        assert!(t.line.lock().claim_next().is_none());
    }

    #[test]
    fn a_finished_build_is_logged_once_with_its_speed_and_counted_toward_the_rate() {
        struct Capture(Arc<Mutex<Vec<(tracing::Level, String)>>>);
        impl tracing::Subscriber for Capture {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, event: &tracing::Event<'_>) {
                struct Fields(String);
                impl tracing::field::Visit for Fields {
                    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                        self.0.push_str(&format!("{}={value:?} ", field.name()));
                    }
                }
                let mut fields = Fields(String::new());
                event.record(&mut fields);
                self.0.lock().unwrap().push((*event.metadata().level(), fields.0));
            }
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        let t = TestLine::new("logged");
        let a = t.video("a");
        submit(&a, quiet(), Ask::Auto);
        let claimed = t.line.lock().claim_next().unwrap();
        let logged = Arc::new(Mutex::new(Vec::new()));
        // 60 s of 1080p built in 12 s: five seconds of footage per second.
        tracing::subscriber::with_default(Capture(logged.clone()), || {
            record_build(t.line, &claimed.job.info, engine::ProxyPipeline::Software, 12.0);
        });
        let logged = logged.lock().unwrap();
        assert_eq!(logged.len(), 1, "{logged:?}");
        let (level, line) = &logged[0];
        assert_eq!(*level, tracing::Level::INFO);
        for want in [
            "pipeline=\"software\"",
            "encoder=\"libx264\"",
            "source=1920x1080",
            "media_secs=60.0",
            "wall_secs=12.0",
            "speed=5.0",
            "message=preview proxy built",
        ] {
            assert!(line.contains(want), "{want} is not in {line}");
        }
        // And it is the rate: 1920 x 1080 x 60 pixel-seconds in 12 s.
        let rate = queue(&inputs(&[&a])).rate.unwrap();
        assert!((rate - 1920.0 * 1080.0 * 5.0).abs() < 1.0, "{rate}");
        // A build with no clock to its name is neither logged nor counted as a rate.
        let before = t.line.lock().rates.finished(Pipeline::Software);
        t.line.lock().rates.record(Pipeline::Software, 1.0, 0.0);
        assert_eq!(t.line.lock().rates.finished(Pipeline::Software), before);
    }

    /// The whole thing with the real workers, the real gate and ffmpeg: the order the builds come
    /// out in, a pause that lets a build finish and starts nothing new, a worker that steps back
    /// while it waits behind an export, a cancel that leaves no file, and a speed that is measured.
    ///
    /// `cargo test -p kerf-core --no-default-features --lib -- --ignored the_real_queue`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn the_real_queue_builds_in_the_order_asked_for_pauses_between_builds_and_cancels_cleanly() {
        use crate::engine::test_support::StatusBounded;

        struct Cleanup {
            assets: Vec<Asset>,
            dir: std::path::PathBuf,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                resume();
                cancel_all();
                for asset in &self.assets {
                    delete(asset);
                }
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }

        let dir = std::env::temp_dir().join(format!("kerf-proxy-queue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ffmpeg = engine::ffmpeg_path();
        let make = |name: &str, size: &str, secs: u32| -> Asset {
            let path = dir.join(format!("{name}.mp4"));
            let made = std::process::Command::new(&ffmpeg)
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
                .arg(format!("testsrc2=s={size}:r=30:d={secs}"))
                .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
                .arg(&path)
                .status_bounded()
                .expect("run ffmpeg");
            assert!(made.success(), "could not make {name}");
            crate::Project::open_in_memory().unwrap().import_asset(&path).unwrap()
        };
        let [a, b, c, d, e] = ["a", "b", "c", "d", "e"].map(|n| make(n, "640x360", 3));
        let slow = make("slow", "1920x1080", 40);
        let mut cleanup = Cleanup {
            assets: vec![a.clone(), b.clone(), c.clone(), d.clone(), e.clone(), slow.clone()],
            dir: dir.clone(),
        };
        let every = |assets: &[&Asset]| assets.iter().map(|x| input((*x).clone())).collect::<Vec<_>>();
        let all = every(&[&a, &b, &c, &d, &e, &slow]);
        let wait = |what: &str, mut ok: Box<dyn FnMut() -> bool + '_>| {
            let until = Instant::now() + Duration::from_secs(150);
            while !ok() {
                assert!(Instant::now() < until, "timed out waiting for {what}: {:?}", queue(&all));
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let ready_order: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));
        let notify = {
            let order = ready_order.clone();
            let lens: Notify = Arc::new(move |s: &ProxyStatus| {
                if s.state == ProxyPhase::Ready {
                    order.lock().unwrap().push(s.asset_id);
                }
            });
            lens
        };
        assert!(!is_paused());
        assert!(queue(&all).jobs.is_empty(), "the process's queue starts empty");

        // 1. Order. Paused, three are queued in the order a, b, c; c is asked for, b is waited on.
        pause();
        for x in [&a, &b, &c] {
            assert!(queue_auto(&input(x.clone()), notify.clone()).is_some());
        }
        build_next(&c, notify.clone()).unwrap();
        rerank(&ids(&[&b]), &ids(&[&a]));
        let q = queue(&all);
        assert!(q.paused && q.rate.is_none() && q.eta_secs.is_none());
        assert_eq!(q.jobs.iter().map(|j| j.asset_id).collect::<Vec<_>>(), [c.id, b.id, a.id]);
        resume();
        wait("the three builds", Box::new(|| ready_order.lock().unwrap().len() == 3));
        assert_eq!(*ready_order.lock().unwrap(), [c.id, b.id, a.id]);
        assert!(queue(&all).jobs.is_empty());
        let rate = queue(&all).rate.expect("a finished build says how fast they go");
        assert!(rate > 0.0);
        for x in [&a, &b, &c] {
            assert_eq!(status(&input(x.clone())).state, ProxyPhase::Ready);
        }

        // 2. A worker waiting behind an export steps back when the queue is paused, and takes the
        // job again on resume.
        let export = cpu::lease_priority(cpu::Priority::Foreground);
        queue_auto(&input(d.clone()), notify.clone()).unwrap();
        wait(
            "the worker to wait behind the export",
            Box::new(|| queue(&all).waiting_on.is_some_and(|w| w.contains("export"))),
        );
        let q = queue(&all);
        assert_eq!(
            (q.jobs.len(), q.jobs[0].state, q.jobs[0].fraction),
            (1, QueueState::Queued, 0.0)
        );
        assert!(
            q.jobs[0].eta_secs.is_some(),
            "a rate is known now, so a queued job has a time"
        );
        pause();
        // The worker notices within a quarter of a second.
        wait("the worker to step back", Box::new(|| shared().lock().claimed() == 0));
        assert_eq!(queue(&all).waiting_on, None);
        assert_eq!(status(&input(d.clone())).state, ProxyPhase::Queued);
        resume();
        wait("the worker to wait again", Box::new(|| queue(&all).waiting_on.is_some()));
        // ...and steps back for a job that goes before it: e is asked for while d waits.
        queue_auto(&input(e.clone()), notify.clone()).unwrap();
        build_next(&e, notify.clone()).unwrap();
        wait(
            "e to be the one the worker waits with",
            Box::new(|| {
                let q = queue(&all);
                q.waiting_on.is_some() && q.jobs[0].asset_id == e.id
            }),
        );
        drop(export);
        wait("d and e", Box::new(|| ready_order.lock().unwrap().len() == 5));
        assert_eq!(ready_order.lock().unwrap()[3..], [e.id, d.id], "the one asked for went first");

        // 3. A pause lets the build under way finish and starts nothing new.
        queue_auto(&input(slow.clone()), notify.clone()).unwrap();
        wait(
            "slow to be under way",
            Box::new(|| {
                queue(&all)
                    .jobs
                    .first()
                    .is_some_and(|j| j.state == QueueState::Building && j.fraction > 0.05)
            }),
        );
        // b's proxy exists already: rebuild it so there is something queued behind slow.
        rebuild(&b, notify.clone()).unwrap();
        pause();
        let q = queue(&all);
        assert!(q.paused);
        assert_eq!(q.jobs[0].asset_id, slow.id);
        assert!(q.jobs[0].eta_secs.is_some() && q.jobs[1].eta_secs.is_none());
        wait("slow to finish", Box::new(|| ready_order.lock().unwrap().len() == 6));
        std::thread::sleep(Duration::from_millis(1500));
        let q = queue(&all);
        assert_eq!(q.jobs.len(), 1, "{q:?}");
        assert_eq!((q.jobs[0].asset_id, q.jobs[0].state), (b.id, QueueState::Queued));
        assert!(!is_ready(&b), "nothing new started while paused");
        resume();
        wait("b to be rebuilt", Box::new(|| ready_order.lock().unwrap().len() == 7));

        // 4. Cancelling a running build leaves no file, no failure, and nothing queues it again.
        rebuild(&slow, notify.clone()).unwrap();
        wait(
            "slow to be under way again",
            Box::new(|| {
                queue(&all)
                    .jobs
                    .first()
                    .is_some_and(|j| j.state == QueueState::Building && j.fraction > 0.05)
            }),
        );
        let dst = engine::proxy_path(Path::new(&slow.path), engine::proxy_width(slow.projection())).unwrap();
        let stem = dst
            .file_name()
            .unwrap()
            .to_string_lossy()
            .split('.')
            .next()
            .unwrap()
            .to_string();
        let parts = |dst: &Path| -> Vec<std::path::PathBuf> {
            std::fs::read_dir(dst.parent().unwrap())
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.file_name().unwrap().to_string_lossy().to_string();
                    n.starts_with(&stem) && n.ends_with(".part")
                })
                .collect()
        };
        assert!(!parts(&dst).is_empty(), "the encode is writing its temp file");
        let ready_before = ready_order.lock().unwrap().len();
        assert!(cancel(&slow));
        wait(
            "the encode to be killed",
            Box::new(|| parts(&dst).is_empty() && queue(&all).jobs.is_empty()),
        );
        let s = status(&input(slow.clone()));
        assert_eq!(s.state, ProxyPhase::Missing, "{s:?}");
        assert!(!dst.exists());
        assert!(queue(&all).failed.is_empty());
        assert!(
            queue_auto(&input(slow.clone()), notify.clone()).is_none(),
            "not queued again by itself"
        );
        assert_eq!(
            ready_order.lock().unwrap().len(),
            ready_before,
            "a cancelled build is not a finished one"
        );
        // Asking again brings it back, which takes the slot the cancelled build gave up (and it is
        // cancelled once more to end the test).
        build_next(&slow, notify.clone()).unwrap();
        wait(
            "slow to start again",
            Box::new(|| queue(&all).jobs.first().is_some_and(|j| j.state == QueueState::Building)),
        );
        assert!(cancel(&slow));
        wait("the encode to be killed again", Box::new(|| parts(&dst).is_empty()));
        cleanup.assets.push(slow);
    }

    fn is_ready(asset: &Asset) -> bool {
        engine::ready_proxy(Path::new(&asset.path), engine::proxy_width(asset.projection())).is_some()
    }
}
