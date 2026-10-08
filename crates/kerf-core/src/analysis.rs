//! Pluggable analysis providers.
//!
//! Transcription (e.g. `whisper-rs` or an external service), scene detection,
//! and silence detection are abstracted behind traits so concrete engines can
//! be swapped in without touching the rest of the core.

use crate::engine::whisper;
use crate::error::{Error, Result};
use crate::model::{AnalysisKind, Asset, AssetAnalysis, Loudness, Rhythm, TimeRange, TranscriptSegment};

/// A step of an analysis pass, reported as it runs.
///
/// Analysis used to be a single opaque wait, which was tolerable while it was a
/// handful of ffmpeg passes. Transcription changes that: the first run may
/// download a few hundred megabytes and then spend minutes on inference, and a
/// spinner with no numbers on it is indistinguishable from a hang.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AnalysisProgress {
    /// Machine-readable step name — `waiting` (queued behind a preview proxy),
    /// `silence`, `scenes`, `loudness`, `rhythm`, `download_model`, `transcribe`,
    /// `done`.
    pub stage: String,
    /// How far along this step is, when it can say. Steps that cannot report
    /// (a single ffmpeg pass that only ends) leave it `None`.
    pub fraction: Option<f64>,
    /// A short human-readable note, e.g. `"84 MB / 142 MB"`.
    pub detail: Option<String>,
}

impl AnalysisProgress {
    pub fn stage(stage: &str) -> Self {
        Self {
            stage: stage.to_string(),
            fraction: None,
            detail: None,
        }
    }

    pub fn with_fraction(stage: &str, fraction: f64) -> Self {
        Self {
            stage: stage.to_string(),
            fraction: Some(fraction.clamp(0.0, 1.0)),
            detail: None,
        }
    }
}

/// A progress sink for an analysis pass.
pub type ProgressFn<'a> = &'a mut dyn FnMut(AnalysisProgress);

/// Told about the analysis found so far — the patch of everything finished — after each
/// step that finishes, so a caller can cache it then rather than after the last.
pub type StepFn<'a> = &'a mut dyn FnMut(&AssetAnalysis);

/// What a step reports while it waits for the machine's heavy-job slot.
fn waiting(wait: crate::engine::cpu::Wait) -> AnalysisProgress {
    AnalysisProgress {
        stage: "waiting".to_string(),
        fraction: None,
        detail: Some(wait.message().to_string()),
    }
}

/// Wait for the heavy-job slot in the background lane, saying why through `progress`, and
/// give up if `cancel` turns true while queued.
fn wait_for_slot(progress: ProgressFn, cancel: CancelFn) -> Result<crate::engine::cpu::Lease> {
    crate::engine::cpu::lease_waiting(&mut |w| progress(waiting(w)), cancel)
}

/// Polled while an analysis pass runs; once it returns true the pass gives up
/// with [`Error::Cancelled`] instead of finishing.
///
/// Analysis is not a few seconds of ffmpeg any more — the first transcription
/// downloads a model and then runs inference for a good fraction of the media's
/// duration — so it has to be abandonable. The same shape as the export's
/// cancel callback.
pub type CancelFn<'a> = &'a dyn Fn() -> bool;

/// The no-op cancel, for callers that never abandon a pass.
pub(crate) const NEVER_CANCEL: CancelFn<'static> = &|| false;

/// Detects silent spans in an asset's audio.
pub trait SilenceDetector: Send + Sync {
    fn detect_silence(&self, asset: &Asset) -> Result<Vec<TimeRange>>;
}

/// Detects scene-change timestamps in an asset's video.
pub trait SceneDetector: Send + Sync {
    fn detect_scenes(&self, asset: &Asset) -> Result<Vec<f64>>;
}

/// Produces a timecoded transcript from an asset's audio.
///
/// Transcription is the one analysis step that can take minutes and, on its
/// first run, download a model — so unlike its siblings it reports progress.
pub trait Transcriber: Send + Sync {
    fn transcribe(&self, asset: &Asset, progress: ProgressFn, cancel: CancelFn) -> Result<Vec<TranscriptSegment>>;
}

/// Measures EBU R128 loudness of an asset's audio (`None` for silent / video-only
/// assets).
pub trait LoudnessAnalyzer: Send + Sync {
    fn measure(&self, asset: &Asset) -> Result<Option<Loudness>>;
}

/// Derives rhythm metadata — onset timestamps, tempo / beat grid, and the
/// speech/music class — from an asset's audio. One trait for the three results
/// because they share the decoded PCM: a provider can (and the ffmpeg one does)
/// produce all of them from a single decode.
pub trait RhythmAnalyzer: Send + Sync {
    fn analyze_rhythm(&self, asset: &Asset) -> Result<Rhythm>;
}

/// Silence detection backed by FFmpeg's `silencedetect` filter (run via the
/// `ffmpeg` binary, so no dev libraries are required).
pub struct FfmpegSilenceDetector {
    /// Threshold below which audio counts as silent, in dBFS (e.g. `-30.0`).
    pub noise_db: f64,
    /// Shortest silent span to report, in seconds.
    pub min_silence: f64,
}

impl Default for FfmpegSilenceDetector {
    fn default() -> Self {
        Self {
            noise_db: -30.0,
            min_silence: 0.5,
        }
    }
}

impl SilenceDetector for FfmpegSilenceDetector {
    fn detect_silence(&self, asset: &Asset) -> Result<Vec<TimeRange>> {
        crate::engine::detect_silence(std::path::Path::new(&asset.path), self.noise_db, self.min_silence)
    }
}

/// Scene-change detection backed by FFmpeg's `select='gt(scene,t)'` filter.
pub struct FfmpegSceneDetector {
    /// Scene-score threshold in `0.0..=1.0`; higher = fewer, stronger cuts.
    pub threshold: f64,
}

impl Default for FfmpegSceneDetector {
    fn default() -> Self {
        Self { threshold: 0.4 }
    }
}

impl SceneDetector for FfmpegSceneDetector {
    fn detect_scenes(&self, asset: &Asset) -> Result<Vec<f64>> {
        crate::engine::detect_scenes(std::path::Path::new(&asset.path), self.threshold)
    }
}

/// Loudness measurement backed by FFmpeg's `loudnorm` analysis pass (run via the
/// `ffmpeg` binary, so no dev libraries are required).
pub struct FfmpegLoudnessAnalyzer;

impl LoudnessAnalyzer for FfmpegLoudnessAnalyzer {
    fn measure(&self, asset: &Asset) -> Result<Option<Loudness>> {
        if !asset.has_audio() {
            return Ok(None);
        }
        let loudness = crate::engine::measure_loudness(std::path::Path::new(&asset.path))?;
        // Silent material measures as non-finite LUFS, which is not meaningful
        // (and would not round-trip through JSON): treat it as no measurement.
        Ok(loudness.integrated_lufs.is_finite().then_some(loudness))
    }
}

/// Rhythm analysis (onsets, tempo, speech/music class) backed by light DSP on
/// PCM decoded once with the `ffmpeg` binary, so no dev libraries are required.
pub struct FfmpegRhythmAnalyzer {
    /// Onset adaptive-threshold std-dev multiplier; higher = fewer, stronger
    /// onsets.
    pub sensitivity: f64,
}

impl Default for FfmpegRhythmAnalyzer {
    fn default() -> Self {
        Self { sensitivity: 1.5 }
    }
}

impl RhythmAnalyzer for FfmpegRhythmAnalyzer {
    fn analyze_rhythm(&self, asset: &Asset) -> Result<Rhythm> {
        if !asset.has_audio() {
            return Ok(Rhythm::default());
        }
        crate::engine::analyze_rhythm(std::path::Path::new(&asset.path), self.sensitivity)
    }
}

/// Report a model download as analysis progress, so the caller only ever sees
/// one progress shape. Shared by both whisper backends.
fn model_progress(progress: ProgressFn<'_>) -> impl FnMut(whisper::DownloadProgress) + '_ {
    |p: whisper::DownloadProgress| {
        let mb = |b: u64| format!("{:.0} MB", b as f64 / (1024.0 * 1024.0));
        progress(AnalysisProgress {
            stage: "download_model".to_string(),
            fraction: p.fraction(),
            detail: Some(match p.total {
                Some(total) => format!("{} / {}", mb(p.downloaded), mb(total)),
                None => mb(p.downloaded),
            }),
        });
    }
}

/// Speech-to-text through the `ffmpeg` binary's `whisper` filter (FFmpeg 8.0+,
/// built `--enable-whisper`).
///
/// This is the zero-toolchain backend: no dev libraries, no whisper.cpp build,
/// nothing linked into Kerf — the same "drive the binaries" bargain as the rest
/// of the CLI engine. It is only selected when
/// [`whisper::filter_available`] says the local ffmpeg actually has the filter.
pub struct WhisperFilterTranscriber {
    /// Spoken language hint (e.g. `"en"`); `None` lets whisper auto-detect.
    pub language: Option<String>,
}

impl Transcriber for WhisperFilterTranscriber {
    fn transcribe(&self, asset: &Asset, progress: ProgressFn, cancel: CancelFn) -> Result<Vec<TranscriptSegment>> {
        let model = whisper::ensure_model(&mut model_progress(progress), cancel)?;
        // After the download, which is not heavy work and must not hold the slot: the
        // inference below nests inside this lease.
        let _slot = wait_for_slot(progress, cancel)?;
        progress(AnalysisProgress::with_fraction("transcribe", 0.0));
        whisper::transcribe(
            std::path::Path::new(&asset.path),
            &model,
            self.language.as_deref(),
            asset.duration,
            &mut |f| progress(AnalysisProgress::with_fraction("transcribe", f)),
            cancel,
        )
    }
}

/// In-process speech-to-text via `whisper-rs`. Audio is decoded to 16 kHz mono
/// with the `ffmpeg` binary, then transcribed with a ggml model — which
/// [`whisper::ensure_model`] downloads on first use, so this needs no manual
/// setup either.
#[cfg(feature = "whisper")]
pub struct WhisperTranscriber {
    /// Spoken language hint (e.g. `"en"`); `None` lets whisper auto-detect.
    pub language: Option<String>,
}

#[cfg(feature = "whisper")]
impl Transcriber for WhisperTranscriber {
    fn transcribe(&self, asset: &Asset, progress: ProgressFn, cancel: CancelFn) -> Result<Vec<TranscriptSegment>> {
        let model = whisper::ensure_model(&mut model_progress(progress), cancel)?;
        let _slot = wait_for_slot(progress, cancel)?;
        progress(AnalysisProgress::with_fraction("transcribe", 0.0));
        if cancel() {
            return Err(Error::Cancelled);
        }
        let samples = crate::engine::decode_audio_16k_mono(std::path::Path::new(&asset.path))?;
        let language = self.language.clone();
        // In-process inference is as CPU-hungry as the ffmpeg filter backend, so
        // it takes the same heavy-job slot and the same share of the cores. The
        // lease is held here while the worker below does the work, which is what
        // the join makes safe.
        let cpu = crate::engine::cpu::lease();
        let threads = cpu.threads();

        // whisper-rs wants a `'static` progress callback, and `full()` blocks
        // for the whole inference — so run it on a worker and pump percentages
        // back over a channel instead of holding a borrow across the call.
        let (tx, rx) = std::sync::mpsc::channel::<i32>();
        let worker = std::thread::spawn(move || run_whisper_rs(samples, model, language, threads, tx));
        for pct in rx {
            progress(AnalysisProgress::with_fraction("transcribe", pct as f64 / 100.0));
        }
        let segments = worker
            .join()
            .map_err(|_| Error::Engine("whisper: transcription thread panicked".to_string()))??;
        progress(AnalysisProgress::with_fraction("transcribe", 1.0));
        Ok(segments)
    }
}

/// The blocking whisper-rs inference, run on its own thread by
/// [`WhisperTranscriber::transcribe`]; `progress` receives 0..=100 percentages.
#[cfg(feature = "whisper")]
fn run_whisper_rs(
    samples: Vec<f32>,
    model: std::path::PathBuf,
    language: Option<String>,
    threads: usize,
    progress: std::sync::mpsc::Sender<i32>,
) -> Result<Vec<TranscriptSegment>> {
    use crate::error::Error;
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    let ctx = WhisperContext::new_with_params(&model, WhisperContextParameters::default())
        .map_err(|e| Error::Engine(format!("whisper: failed to load model: {e}")))?;
    let mut state = ctx.create_state().map_err(|e| Error::Engine(format!("whisper: {e}")))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    // whisper.cpp otherwise sizes its thread pool from the machine, not from
    // what Kerf was told it may use.
    params.set_n_threads(threads.clamp(1, i32::MAX as usize) as i32);
    if let Some(lang) = &language {
        params.set_language(Some(lang));
    }
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_progress_callback_safe(move |pct: i32| {
        // The receiver is gone only if the caller gave up; dropping is fine.
        let _ = progress.send(pct);
    });

    state
        .full(params, &samples)
        .map_err(|e| Error::Engine(format!("whisper: inference failed: {e}")))?;

    let mut segments = Vec::new();
    for i in 0..state.full_n_segments() {
        let Some(segment) = state.get_segment(i) else { continue };
        // Lossy: a mid-word byte sequence whisper emitted badly should cost one
        // replacement character, not the whole transcript.
        let text = segment
            .to_str_lossy()
            .map_err(|e| Error::Engine(format!("whisper: {e}")))?
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        // whisper timestamps are in centiseconds.
        segments.push(TranscriptSegment {
            start: segment.start_timestamp() as f64 / 100.0,
            end: segment.end_timestamp() as f64 / 100.0,
            text,
        });
    }
    Ok(segments)
}

/// Which speech-to-text backend a transcription would use, and whether its model
/// is already on disk. Surfaced to the GUI and to an agent over MCP so the
/// reason a transcript is empty is visible rather than guessed at.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct TranscriptionStatus {
    /// `libwhisper` (in-process), `ffmpeg_filter`, or `none`.
    pub backend: String,
    /// Whether transcription can run at all in this build / install.
    pub available: bool,
    /// Whether transcription is one of the analyses that run by themselves (the
    /// Settings toggle). Off, nothing transcribes until you ask for it.
    pub enabled: bool,
    /// The whisper.cpp model name in use, when one is being managed for the
    /// user (`None` when `KERF_WHISPER_MODEL` names a file directly).
    pub model: Option<String>,
    /// The model file, once it is on disk.
    pub model_path: Option<String>,
    /// Whether that file is present. When false, the next transcription starts
    /// by downloading roughly `approx_download_bytes`.
    pub model_ready: bool,
    pub approx_download_bytes: Option<u64>,
    /// Every model that can be downloaded, smallest first.
    pub models: Vec<whisper::ModelInfo>,
    /// Why transcription is unavailable, when it is.
    pub reason: Option<String>,
}

/// Which analyses run **by themselves** — when media is imported — and which do not.
///
/// This is the switch behind Settings › Analysis (and the old "transcribe speech"
/// toggle, which is the `transcript` flag now). `enabled` is the master: off, nothing
/// is analyzed on import. The per-kind flags also decide what an `analyze_asset` call
/// that names no steps runs, so an agent cannot fetch a 148 MB speech model for
/// someone who turned transcription off — an explicit `steps` list overrides them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AutoAnalysis {
    /// Analyze new media on import at all.
    pub enabled: bool,
    pub silence: bool,
    pub scenes: bool,
    pub loudness: bool,
    pub rhythm: bool,
    pub transcript: bool,
}

impl Default for AutoAnalysis {
    fn default() -> Self {
        Self::ALL_ON
    }
}

impl AutoAnalysis {
    pub const ALL_ON: AutoAnalysis = AutoAnalysis {
        enabled: true,
        silence: true,
        scenes: true,
        loudness: true,
        rhythm: true,
        transcript: true,
    };

    /// Whether `kind` is one of the analyses that run by themselves.
    pub fn wants(&self, kind: AnalysisKind) -> bool {
        match kind {
            AnalysisKind::Silence => self.silence,
            AnalysisKind::Scenes => self.scenes,
            AnalysisKind::Loudness => self.loudness,
            AnalysisKind::Rhythm => self.rhythm,
            AnalysisKind::Transcript => self.transcript,
        }
    }

    /// The kinds that run by themselves, in pass order (empty when the master is off).
    pub fn kinds(&self) -> Vec<AnalysisKind> {
        if !self.enabled {
            return Vec::new();
        }
        AnalysisKind::ALL.into_iter().filter(|k| self.wants(*k)).collect()
    }

    /// The kinds the per-kind toggles allow, whatever the master says — what an
    /// `analyze_asset` call that names no steps runs.
    pub fn allowed_kinds(&self) -> Vec<AnalysisKind> {
        AnalysisKind::ALL.into_iter().filter(|k| self.wants(*k)).collect()
    }
}

static AUTO_ANALYSIS: std::sync::RwLock<AutoAnalysis> = std::sync::RwLock::new(AutoAnalysis::ALL_ON);

/// The automatic-analysis settings in force.
pub fn auto_analysis() -> AutoAnalysis {
    *AUTO_ANALYSIS.read().unwrap_or_else(|e| e.into_inner())
}

/// Put new automatic-analysis settings in force. A pass already running finishes what
/// it was doing.
pub fn set_auto_analysis(settings: AutoAnalysis) {
    *AUTO_ANALYSIS.write().unwrap_or_else(|e| e.into_inner()) = settings;
}

/// Whether a speech-to-text backend exists, and if not, why.
pub fn transcription_backend() -> std::result::Result<&'static str, String> {
    if cfg!(feature = "whisper") {
        Ok("libwhisper")
    } else if whisper::filter_available() {
        Ok("ffmpeg_filter")
    } else {
        Err(
            "no speech-to-text backend: this build has no in-process whisper, and the `ffmpeg` \
             binary has no `whisper` filter (FFmpeg 8.0+ built with --enable-whisper)"
                .to_string(),
        )
    }
}

/// Describe the speech-to-text backend this build would use.
pub fn transcription_status() -> TranscriptionStatus {
    let (backend, available, reason) = match transcription_backend() {
        Ok(backend) => (backend, true, None),
        Err(reason) => ("none", false, Some(reason)),
    };
    let (model, model_path) = match whisper::configured_model() {
        whisper::ModelChoice::Named(name) => {
            let path = whisper::model_path(&name);
            (Some(name), path)
        }
        whisper::ModelChoice::File(path) => (None, Some(path)),
    };
    let ready = whisper::ready_model().is_some();
    TranscriptionStatus {
        backend: backend.to_string(),
        available,
        enabled: auto_analysis().transcript,
        approx_download_bytes: (!ready)
            .then(|| model.as_deref().and_then(whisper::model_info).map(|m| m.approx_bytes))
            .flatten(),
        model,
        model_path: model_path.map(|p| p.to_string_lossy().into_owned()),
        model_ready: ready,
        models: whisper::MODELS.to_vec(),
        reason,
    }
}

/// The transcript of a voiceover Kerf generated: its script, with the sentence
/// timings the synthesizer measured. Never a speech model's guess at audio whose
/// words are already known exactly.
struct VoiceoverScript;

impl Transcriber for VoiceoverScript {
    fn transcribe(&self, asset: &Asset, _progress: ProgressFn, _cancel: CancelFn) -> Result<Vec<TranscriptSegment>> {
        Ok(asset.voiceover.as_ref().map(|v| v.segments.clone()).unwrap_or_default())
    }
}

/// A no-op provider returning empty results. Useful as a default and for tests.
pub struct NullAnalyzer;

impl SilenceDetector for NullAnalyzer {
    fn detect_silence(&self, _asset: &Asset) -> Result<Vec<TimeRange>> {
        Ok(Vec::new())
    }
}

impl SceneDetector for NullAnalyzer {
    fn detect_scenes(&self, _asset: &Asset) -> Result<Vec<f64>> {
        Ok(Vec::new())
    }
}

impl Transcriber for NullAnalyzer {
    fn transcribe(&self, _asset: &Asset, _progress: ProgressFn, _cancel: CancelFn) -> Result<Vec<TranscriptSegment>> {
        Ok(Vec::new())
    }
}

impl LoudnessAnalyzer for NullAnalyzer {
    fn measure(&self, _asset: &Asset) -> Result<Option<Loudness>> {
        Ok(None)
    }
}

impl RhythmAnalyzer for NullAnalyzer {
    fn analyze_rhythm(&self, _asset: &Asset) -> Result<Rhythm> {
        Ok(Rhythm::default())
    }
}

/// A transcriber for a build with no speech-to-text backend: asking it for a transcript
/// is an error that says why, where [`NullAnalyzer`] would answer "no speech".
struct UnavailableTranscriber(String);

impl Transcriber for UnavailableTranscriber {
    fn transcribe(&self, _asset: &Asset, _progress: ProgressFn, _cancel: CancelFn) -> Result<Vec<TranscriptSegment>> {
        Err(Error::Engine(self.0.clone()))
    }
}

/// A bundle of analysis providers to run against an asset.
pub struct AnalysisProviders<'a> {
    pub silence: &'a dyn SilenceDetector,
    pub scene: &'a dyn SceneDetector,
    pub transcriber: &'a dyn Transcriber,
    pub loudness: &'a dyn LoudnessAnalyzer,
    pub rhythm: &'a dyn RhythmAnalyzer,
}

impl<'a> AnalysisProviders<'a> {
    /// All providers wired to [`NullAnalyzer`].
    pub fn null(null: &'a NullAnalyzer) -> Self {
        Self {
            silence: null,
            scene: null,
            transcriber: null,
            loudness: null,
            rhythm: null,
        }
    }
}

/// The transcription backend for this build: the in-process one when the
/// `whisper` feature is compiled in, otherwise FFmpeg's `whisper` filter when
/// the local binary has it, otherwise nothing.
///
/// The in-process backend wins when both are present: it is the one Kerf ships,
/// so its behaviour is the same on every machine, whereas the filter's exists
/// only if whoever built that ffmpeg opted into it.
#[cfg(feature = "whisper")]
fn default_transcriber() -> Option<Box<dyn Transcriber>> {
    Some(Box::new(WhisperTranscriber {
        language: configured_language(),
    }))
}

#[cfg(not(feature = "whisper"))]
fn default_transcriber() -> Option<Box<dyn Transcriber>> {
    whisper::filter_available().then(|| {
        Box::new(WhisperFilterTranscriber {
            language: configured_language(),
        }) as Box<dyn Transcriber>
    })
}

/// The spoken-language hint from `KERF_WHISPER_LANGUAGE` (e.g. `de`); unset
/// means let whisper auto-detect.
fn configured_language() -> Option<String> {
    std::env::var("KERF_WHISPER_LANGUAGE").ok().filter(|l| !l.is_empty())
}

// ---- what is running, what failed ---------------------------------------------

/// What the session knows about an asset's analysis beyond what is cached: the step
/// running now and the steps whose last attempt failed (a cached analysis only holds
/// what succeeded).
#[derive(Default)]
struct Runs {
    running: Option<AnalysisKind>,
    failed: std::collections::HashMap<AnalysisKind, String>,
}

fn runs() -> std::sync::MutexGuard<'static, std::collections::HashMap<uuid::Uuid, Runs>> {
    static RUNS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<uuid::Uuid, Runs>>> = std::sync::OnceLock::new();
    RUNS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Marks a step as running for as long as it lives.
struct Running(uuid::Uuid);

impl Running {
    fn start(asset_id: uuid::Uuid, kind: AnalysisKind) -> Self {
        runs().entry(asset_id).or_default().running = Some(kind);
        Running(asset_id)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(r) = runs().get_mut(&self.0) {
            r.running = None;
        }
    }
}

fn note_result(asset_id: uuid::Uuid, kind: AnalysisKind, failure: Option<&str>) {
    let mut runs = runs();
    let entry = runs.entry(asset_id).or_default();
    match failure {
        Some(reason) => {
            entry.failed.insert(kind, reason.to_string());
        }
        None => {
            entry.failed.remove(&kind);
        }
    }
}

/// Where one kind of an asset's analysis stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisState {
    /// Ran, and is cached.
    Done,
    /// Has not run, and would on import.
    NotRun,
    /// Its step is running now.
    Running,
    /// The last attempt failed; `reason` says why.
    Failed,
    /// Has not run and will not by itself: switched off in Settings, or no backend.
    Off,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AnalysisKindStatus {
    pub kind: AnalysisKind,
    pub state: AnalysisState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// An asset's analysis, kind by kind — what the bin's chips, the inspector and an
/// agent read. Always one entry per kind, in pass order.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AnalysisStatus {
    pub asset_id: uuid::Uuid,
    pub kinds: Vec<AnalysisKindStatus>,
}

/// [`analysis_status`] from its parts (pure, unit-tested). Precedence: a running step
/// is running; cached data is done (and a stale failure is moot); then a recorded
/// failure; then off (switched off, the master off, or no backend); else not run.
fn status_from(
    asset_id: uuid::Uuid,
    stored: Option<&AssetAnalysis>,
    running: Option<AnalysisKind>,
    failed: &std::collections::HashMap<AnalysisKind, String>,
    auto: &AutoAnalysis,
    transcript_unavailable: Option<&str>,
) -> AnalysisStatus {
    let kinds = AnalysisKind::ALL
        .into_iter()
        .map(|kind| {
            let (state, reason) = if running == Some(kind) {
                (AnalysisState::Running, None)
            } else if stored.is_some_and(|a| a.done(kind)) {
                (AnalysisState::Done, None)
            } else if let Some(reason) = failed.get(&kind) {
                (AnalysisState::Failed, Some(reason.clone()))
            } else if kind == AnalysisKind::Transcript && transcript_unavailable.is_some() {
                (AnalysisState::Off, transcript_unavailable.map(str::to_string))
            } else if !auto.enabled {
                (AnalysisState::Off, Some("analysis on import is off in Settings".to_string()))
            } else if !auto.wants(kind) {
                (AnalysisState::Off, Some("switched off in Settings › Analysis".to_string()))
            } else {
                (AnalysisState::NotRun, None)
            };
            AnalysisKindStatus { kind, state, reason }
        })
        .collect();
    AnalysisStatus { asset_id, kinds }
}

/// The per-kind status of an asset's analysis, given what is cached for it.
pub fn analysis_status(asset_id: uuid::Uuid, stored: Option<&AssetAnalysis>) -> AnalysisStatus {
    let (running, failed) = {
        let runs = runs();
        runs.get(&asset_id).map(|r| (r.running, r.failed.clone())).unwrap_or_default()
    };
    let unavailable = transcription_backend().err();
    status_from(asset_id, stored, running, &failed, &auto_analysis(), unavailable.as_deref())
}

// ---- running steps ------------------------------------------------------------

/// What a run of analysis steps produced.
#[derive(Debug, Default)]
pub struct StepsRun {
    /// The results of the steps that **finished**, with those steps in `ran`: merge it
    /// into the cached analysis ([`AssetAnalysis::merge`]). A step that was cancelled or
    /// failed is not in it and changes nothing.
    pub patch: AssetAnalysis,
    /// Steps that failed, with why. The rest of the run carries on without them.
    pub failed: Vec<(AnalysisKind, String)>,
    /// The run was abandoned before it finished.
    pub cancelled: bool,
}

impl StepsRun {
    /// The patch as a result: cancelled is [`Error::Cancelled`], and a run in which every step
    /// failed is the first failure. A partial failure is the patch (the failures are in
    /// [`StepsRun::failed`] and the status).
    pub fn into_result(self) -> Result<AssetAnalysis> {
        if self.cancelled {
            return Err(Error::Cancelled);
        }
        if self.patch.ran.is_empty() {
            if let Some((kind, reason)) = self.failed.into_iter().next() {
                return Err(Error::Engine(format!("{} failed: {reason}", kind.name())));
            }
        }
        Ok(self.patch)
    }

    /// One line per failed step, for an error message or a log.
    pub fn failure_summary(&self) -> Option<String> {
        (!self.failed.is_empty()).then(|| {
            self.failed
                .iter()
                .map(|(kind, reason)| format!("{}: {reason}", kind.name()))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }
}

/// The kinds a call that names no steps runs: the ones the per-kind toggles allow,
/// without transcription when there is no backend for it.
pub fn default_kinds() -> Vec<AnalysisKind> {
    let mut kinds = auto_analysis().allowed_kinds();
    if transcription_backend().is_err() {
        kinds.retain(|k| *k != AnalysisKind::Transcript);
    }
    kinds
}

/// The steps a request for `requested` (the [`default_kinds`] when `None`) actually runs on
/// `asset`: a file with no audio has none of silence, loudness, rhythm or speech to find, so
/// those are left out rather than run to nothing or fail, and a still image has nothing to
/// analyze at all. An empty answer is an error that says why — a caller that names no steps
/// while everything is switched off, or one that asks a silent file for its speech, is told,
/// not handed an empty result.
pub fn resolve_steps(asset: &Asset, requested: Option<&[AnalysisKind]>) -> Result<Vec<AnalysisKind>> {
    if asset.is_image() {
        return Err(Error::InvalidArgument(format!(
            "{} is a still image: there is nothing to analyze",
            asset.name
        )));
    }
    let wanted = requested.map(<[AnalysisKind]>::to_vec).unwrap_or_else(default_kinds);
    let has_audio = asset.has_audio();
    let kinds: Vec<AnalysisKind> = AnalysisKind::ALL
        .into_iter()
        .filter(|k| wanted.contains(k) && (has_audio || !k.needs_audio()))
        .collect();
    if !kinds.is_empty() {
        return Ok(kinds);
    }
    Err(Error::InvalidArgument(if wanted.is_empty() {
        "no analysis steps to run: every analysis is switched off in Settings › Analysis (name steps — silence, scenes, loudness, rhythm, transcript — to run them anyway)".to_string()
    } else {
        format!(
            "{} has no audio, so {} cannot run on it; only scene detection applies",
            asset.name,
            AnalysisKind::ALL
                .into_iter()
                .filter(|k| wanted.contains(k))
                .map(|k| k.name())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }))
}

/// Run `kinds` (the [`default_kinds`] when `None`, see [`resolve_steps`]) against an asset's
/// media with the default providers: FFmpeg silence / scene / loudness / rhythm detection, and
/// speech-to-text through whichever whisper backend this build has.
///
/// This is the heavy, ffmpeg-bound part of analysis, a free function so a caller
/// holding the shared `Project` lock can release it before running this and take it
/// again only to cache the result ([`crate::project::Project::merge_analysis`]).
///
/// Only the steps named run, in pass order, in the **background lane** of the heavy-job queue
/// (behind exports and preview proxies, saying so while they wait); the result is a patch
/// holding what they found, to merge into whatever is already cached. `on_step` hears the
/// patch after each step that finishes. The check for `cancel` lands between steps, while
/// queued, *and* inside transcription, where the wait is: a model download and then
/// inference for minutes.
pub fn analyze_asset_steps(
    asset: &Asset,
    kinds: Option<&[AnalysisKind]>,
    progress: ProgressFn,
    on_step: StepFn,
    cancel: CancelFn,
) -> Result<StepsRun> {
    let kinds = resolve_steps(asset, kinds)?;
    let silence = FfmpegSilenceDetector::default();
    let scene = FfmpegSceneDetector::default();
    let loudness = FfmpegLoudnessAnalyzer;
    let rhythm = FfmpegRhythmAnalyzer::default();

    // Built only when asked for: finding out whether the ffmpeg binary has the whisper
    // filter spawns it once, which a run that does not transcribe has no use for.
    let transcriber: Box<dyn Transcriber> = if !kinds.contains(&AnalysisKind::Transcript) {
        Box::new(NullAnalyzer)
    } else if asset.voiceover.is_some() {
        Box::new(VoiceoverScript)
    } else {
        default_transcriber()
            .unwrap_or_else(|| Box::new(UnavailableTranscriber(transcription_backend().err().unwrap_or_default())))
    };
    let providers = AnalysisProviders {
        silence: &silence,
        scene: &scene,
        transcriber: transcriber.as_ref(),
        loudness: &loudness,
        rhythm: &rhythm,
    };
    Ok(analyze_steps(asset, &kinds, &providers, progress, on_step, cancel))
}

/// [`analyze_asset_steps`] over given providers (and the steps as given, unresolved).
pub fn analyze_steps(
    asset: &Asset,
    kinds: &[AnalysisKind],
    providers: &AnalysisProviders,
    progress: ProgressFn,
    on_step: StepFn,
    cancel: CancelFn,
) -> StepsRun {
    crate::engine::cpu::in_background(|| run_steps(asset, kinds, providers, progress, on_step, cancel))
}

fn run_steps(
    asset: &Asset,
    kinds: &[AnalysisKind],
    providers: &AnalysisProviders,
    progress: ProgressFn,
    on_step: StepFn,
    cancel: CancelFn,
) -> StepsRun {
    let mut run = StepsRun {
        patch: AssetAnalysis {
            asset_id: asset.id,
            ..AssetAnalysis::default()
        },
        ..StepsRun::default()
    };
    for kind in AnalysisKind::ALL.into_iter().filter(|k| kinds.contains(k)) {
        if cancel() {
            run.cancelled = true;
            break;
        }
        // Wait for the machine's one heavy-job slot here rather than inside the step, so the
        // wait can be said out loud — "a preview proxy goes first" — and Stop works while the
        // step is still queued. The step's own lease then nests. Transcription takes its
        // own, after its model download, which is not heavy work and must not hold the slot
        // while it fetches.
        let _slot = if kind == AnalysisKind::Transcript {
            None
        } else {
            match wait_for_slot(progress, cancel) {
                Ok(slot) => Some(slot),
                Err(_) => {
                    run.cancelled = true;
                    break;
                }
            }
        };
        // Stopped while queued: do not start a whole-file pass the user has just abandoned.
        if cancel() {
            run.cancelled = true;
            break;
        }
        progress(AnalysisProgress::stage(match kind {
            AnalysisKind::Transcript => "transcribe",
            other => other.name(),
        }));
        let step = Running::start(asset.id, kind);
        let outcome = match kind {
            AnalysisKind::Silence => providers
                .silence
                .detect_silence(asset)
                .map(|v| run.patch.silence_segments = v),
            AnalysisKind::Scenes => providers.scene.detect_scenes(asset).map(|v| run.patch.scene_changes = v),
            AnalysisKind::Loudness => providers.loudness.measure(asset).map(|v| run.patch.loudness = v),
            // One provider call for onsets + tempo + class: they share a single decode.
            AnalysisKind::Rhythm => providers.rhythm.analyze_rhythm(asset).map(|r| {
                run.patch.onsets = r.onsets;
                run.patch.tempo = r.tempo;
                run.patch.audio_class = r.audio_class;
                run.patch.music = r.music;
            }),
            AnalysisKind::Transcript => providers
                .transcriber
                .transcribe(asset, progress, cancel)
                .map(|v| run.patch.transcript = v),
        };
        drop(step);
        match outcome {
            Ok(()) => {
                note_result(asset.id, kind, None);
                run.patch.ran.push(kind);
                on_step(&run.patch);
            }
            Err(Error::Cancelled) => {
                run.cancelled = true;
                break;
            }
            Err(e) => {
                let reason = e.to_string();
                tracing::warn!(kind = kind.name(), error = %reason, "analysis step failed");
                note_result(asset.id, kind, Some(&reason));
                run.failed.push((kind, reason));
            }
        }
    }
    progress(AnalysisProgress::stage("done"));
    run
}

/// Run the default steps and return what they found (the whole of a first analysis).
/// Errors when the run was cancelled or no step finished.
pub fn analyze_asset_media(asset: &Asset) -> Result<AssetAnalysis> {
    analyze_asset_steps(asset, None, &mut |_| {}, &mut |_| {}, NEVER_CANCEL)?.into_result()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AudioClass, AudioClassification, Tempo};
    use std::collections::HashMap;
    use uuid::Uuid;

    fn asset() -> Asset {
        crate::engine::test_support::test_asset(vec![
            crate::engine::test_support::video_stream(1920, 1080, 30.0),
            crate::engine::test_support::audio_stream(48_000, 2),
        ])
    }

    /// Providers that record which of them ran and answer with canned data — or fail.
    struct Fake {
        ran: std::sync::Mutex<Vec<&'static str>>,
        fail: Option<AnalysisKind>,
        cancel_at: Option<AnalysisKind>,
    }

    impl Fake {
        fn new() -> Self {
            Fake {
                ran: std::sync::Mutex::new(Vec::new()),
                fail: None,
                cancel_at: None,
            }
        }

        fn step(&self, kind: AnalysisKind) -> Result<()> {
            self.ran.lock().unwrap().push(kind.name());
            if self.cancel_at == Some(kind) {
                return Err(Error::Cancelled);
            }
            if self.fail == Some(kind) {
                return Err(Error::Engine(format!("{} broke", kind.name())));
            }
            Ok(())
        }
    }

    struct Probe<'a>(&'a Fake, AnalysisKind);

    impl SilenceDetector for Probe<'_> {
        fn detect_silence(&self, _: &Asset) -> Result<Vec<TimeRange>> {
            self.0.step(self.1)?;
            Ok(vec![TimeRange { start: 1.0, end: 2.0 }])
        }
    }
    impl SceneDetector for Probe<'_> {
        fn detect_scenes(&self, _: &Asset) -> Result<Vec<f64>> {
            self.0.step(self.1)?;
            Ok(vec![5.0, 9.0])
        }
    }
    impl LoudnessAnalyzer for Probe<'_> {
        fn measure(&self, _: &Asset) -> Result<Option<Loudness>> {
            self.0.step(self.1)?;
            Ok(Some(Loudness {
                integrated_lufs: -16.0,
                loudness_range: 5.0,
                true_peak_dbtp: -1.0,
                threshold_lufs: -26.0,
            }))
        }
    }
    impl RhythmAnalyzer for Probe<'_> {
        fn analyze_rhythm(&self, _: &Asset) -> Result<Rhythm> {
            self.0.step(self.1)?;
            Ok(Rhythm {
                onsets: vec![0.5],
                tempo: Some(Tempo {
                    bpm: 120.0,
                    beats: vec![0.0, 0.5],
                    downbeats: Vec::new(),
                    confidence: 0.9,
                }),
                audio_class: Some(AudioClassification {
                    class: AudioClass::Music,
                    confidence: 0.8,
                }),
                music: None,
            })
        }
    }
    impl Transcriber for Probe<'_> {
        fn transcribe(&self, _: &Asset, _: ProgressFn, _: CancelFn) -> Result<Vec<TranscriptSegment>> {
            self.0.step(self.1)?;
            Ok(vec![TranscriptSegment {
                start: 0.0,
                end: 1.0,
                text: "hello".to_string(),
            }])
        }
    }

    fn run(fake: &Fake, asset: &Asset, kinds: &[AnalysisKind]) -> StepsRun {
        let (si, sc, lo, rh, tr) = (
            Probe(fake, AnalysisKind::Silence),
            Probe(fake, AnalysisKind::Scenes),
            Probe(fake, AnalysisKind::Loudness),
            Probe(fake, AnalysisKind::Rhythm),
            Probe(fake, AnalysisKind::Transcript),
        );
        let providers = AnalysisProviders {
            silence: &si,
            scene: &sc,
            transcriber: &tr,
            loudness: &lo,
            rhythm: &rh,
        };
        analyze_steps(asset, kinds, &providers, &mut |_| {}, &mut |_| {}, NEVER_CANCEL)
    }

    #[test]
    fn only_the_named_steps_run_and_in_pass_order() {
        let _gate = crate::engine::cpu::test_lock();
        let fake = Fake::new();
        let a = asset();
        let out = run(&fake, &a, &[AnalysisKind::Transcript, AnalysisKind::Silence]);
        assert_eq!(
            *fake.ran.lock().unwrap(),
            ["silence", "transcript"],
            "pass order, nothing else"
        );
        assert_eq!(out.patch.ran, [AnalysisKind::Silence, AnalysisKind::Transcript]);
        assert_eq!(out.patch.silence_segments.len(), 1);
        assert_eq!(out.patch.transcript.len(), 1);
        assert!(out.patch.scene_changes.is_empty() && out.patch.loudness.is_none());
        assert!(out.failed.is_empty() && !out.cancelled);
    }

    #[test]
    fn a_partial_result_merges_without_wiping_the_other_kinds() {
        let _gate = crate::engine::cpu::test_lock();
        let a = asset();
        // Everything cached from an earlier full pass…
        let mut cached = run(&Fake::new(), &a, &AnalysisKind::ALL).patch;
        assert_eq!(cached.done_kinds(), AnalysisKind::ALL);
        // …then silence is re-run and finds nothing this time, and loudness is untouched.
        struct Quiet;
        impl SilenceDetector for Quiet {
            fn detect_silence(&self, _: &Asset) -> Result<Vec<TimeRange>> {
                Ok(Vec::new())
            }
        }
        let null = NullAnalyzer;
        let mut providers = AnalysisProviders::null(&null);
        providers.silence = &Quiet;
        let patch = analyze_steps(
            &a,
            &[AnalysisKind::Silence],
            &providers,
            &mut |_| {},
            &mut |_| {},
            NEVER_CANCEL,
        )
        .patch;
        cached.merge(&patch);
        assert!(cached.silence_segments.is_empty(), "the re-run replaced silence");
        assert_eq!(cached.scene_changes, [5.0, 9.0], "scenes kept");
        assert!(cached.loudness.is_some() && cached.tempo.is_some() && !cached.transcript.is_empty());
        assert!(cached.done(AnalysisKind::Silence), "ran and found nothing is still done");
    }

    #[test]
    fn a_failed_step_is_reported_and_the_rest_still_land() {
        let _gate = crate::engine::cpu::test_lock();
        let mut fake = Fake::new();
        fake.fail = Some(AnalysisKind::Scenes);
        let a = asset();
        let out = run(&fake, &a, &AnalysisKind::ALL);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].0, AnalysisKind::Scenes);
        assert!(out.failed[0].1.contains("scenes broke"));
        assert_eq!(
            out.patch.ran,
            [
                AnalysisKind::Silence,
                AnalysisKind::Loudness,
                AnalysisKind::Rhythm,
                AnalysisKind::Transcript
            ]
        );
        assert!(out.patch.scene_changes.is_empty());
        assert!(out.failure_summary().unwrap().starts_with("scenes:"));
        // …and the session remembers it, until a later run succeeds.
        let status = analysis_status(a.id, Some(&out.patch));
        let scenes = status.kinds.iter().find(|k| k.kind == AnalysisKind::Scenes).unwrap();
        assert_eq!(scenes.state, AnalysisState::Failed);
        assert!(scenes.reason.as_ref().unwrap().contains("broke"));
        let ok = run(&Fake::new(), &a, &[AnalysisKind::Scenes]);
        assert!(ok.failed.is_empty());
        let status = analysis_status(a.id, Some(&ok.patch));
        assert_eq!(
            status.kinds.iter().find(|k| k.kind == AnalysisKind::Scenes).unwrap().state,
            AnalysisState::Done
        );
    }

    #[test]
    fn a_step_says_it_is_waiting_for_a_proxy_and_does_not_start_until_the_proxy_is_done() {
        let _gate = crate::engine::cpu::test_lock();
        // A preview proxy is queued: analysis that starts now goes behind it, and says so.
        let reservation = crate::engine::cpu::reserve();
        let stages = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, Option<String>)>::new()));
        let seen = stages.clone();
        let asset = asset();
        let worker = std::thread::spawn(move || {
            let fake = Fake::new();
            let (si, sc, lo, rh, tr) = (
                Probe(&fake, AnalysisKind::Silence),
                Probe(&fake, AnalysisKind::Scenes),
                Probe(&fake, AnalysisKind::Loudness),
                Probe(&fake, AnalysisKind::Rhythm),
                Probe(&fake, AnalysisKind::Transcript),
            );
            let providers = AnalysisProviders {
                silence: &si,
                scene: &sc,
                transcriber: &tr,
                loudness: &lo,
                rhythm: &rh,
            };
            analyze_steps(
                &asset,
                &[AnalysisKind::Silence],
                &providers,
                &mut |p| seen.lock().unwrap().push((p.stage, p.detail)),
                &mut |_| {},
                NEVER_CANCEL,
            )
        });
        let start = std::time::Instant::now();
        while stages.lock().unwrap().is_empty() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "no word from the waiting step"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        {
            let said = stages.lock().unwrap();
            assert_eq!(
                said.len(),
                1,
                "only the wait was announced, the step has not started: {said:?}"
            );
            assert_eq!(said[0].0, "waiting");
            assert!(said[0].1.as_deref().unwrap().contains("proxy"), "{said:?}");
        }
        // The proxy takes the slot, finishes, and the analysis goes.
        drop(reservation.lease());
        let run = worker.join().unwrap();
        assert_eq!(run.patch.ran, [AnalysisKind::Silence]);
        let said: Vec<String> = stages.lock().unwrap().iter().map(|(s, _)| s.clone()).collect();
        assert_eq!(said, ["waiting", "silence", "done"]);
    }

    /// A step stopped while it is still queued behind another job never starts: not at the
    /// start of the loop, not in the queue, and not in the instant the slot comes free.
    #[test]
    fn stop_works_while_a_step_is_queued_and_a_stopped_step_never_starts_its_pass() {
        let _gate = crate::engine::cpu::test_lock();
        let held = crate::engine::cpu::lease();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stages = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let fake = std::sync::Arc::new(Fake::new());
        let worker = {
            let (cancel, stages, fake) = (cancel.clone(), stages.clone(), fake.clone());
            let a = asset();
            std::thread::spawn(move || {
                let (si, sc, lo, rh, tr) = (
                    Probe(&fake, AnalysisKind::Silence),
                    Probe(&fake, AnalysisKind::Scenes),
                    Probe(&fake, AnalysisKind::Loudness),
                    Probe(&fake, AnalysisKind::Rhythm),
                    Probe(&fake, AnalysisKind::Transcript),
                );
                let providers = AnalysisProviders {
                    silence: &si,
                    scene: &sc,
                    transcriber: &tr,
                    loudness: &lo,
                    rhythm: &rh,
                };
                analyze_steps(
                    &a,
                    &[AnalysisKind::Silence, AnalysisKind::Scenes],
                    &providers,
                    &mut |p| stages.lock().unwrap().push(p.stage),
                    &mut |_| {},
                    &|| cancel.load(std::sync::atomic::Ordering::SeqCst),
                )
            })
        };
        let start = std::time::Instant::now();
        while stages.lock().unwrap().is_empty() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "the queued step said nothing"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(stages.lock().unwrap()[0], "waiting", "it says it is queued");
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        let run = worker.join().unwrap();
        drop(held);
        assert!(run.cancelled && run.patch.ran.is_empty());
        assert!(
            fake.ran.lock().unwrap().is_empty(),
            "no pass ran: {:?}",
            fake.ran.lock().unwrap()
        );
    }

    #[test]
    fn a_stop_that_lands_as_the_slot_comes_free_does_not_start_the_pass() {
        let _gate = crate::engine::cpu::test_lock();
        let fake = Fake::new();
        // The slot is free, so the queue wait never polls the flag: the loop's check before the
        // wait is the first call and the one right after the slot is taken the second. Stop on
        // the second.
        let calls = std::cell::Cell::new(0);
        let (si, sc, lo, rh, tr) = (
            Probe(&fake, AnalysisKind::Silence),
            Probe(&fake, AnalysisKind::Scenes),
            Probe(&fake, AnalysisKind::Loudness),
            Probe(&fake, AnalysisKind::Rhythm),
            Probe(&fake, AnalysisKind::Transcript),
        );
        let providers = AnalysisProviders {
            silence: &si,
            scene: &sc,
            transcriber: &tr,
            loudness: &lo,
            rhythm: &rh,
        };
        let run = analyze_steps(
            &asset(),
            &[AnalysisKind::Silence],
            &providers,
            &mut |_| {},
            &mut |_| {},
            &|| {
                calls.set(calls.get() + 1);
                calls.get() >= 2
            },
        );
        assert!(run.cancelled);
        assert!(fake.ran.lock().unwrap().is_empty(), "the pass started after the stop");
    }

    #[test]
    fn the_caller_hears_the_analysis_after_each_step_that_finishes() {
        let _gate = crate::engine::cpu::test_lock();
        let fake = Fake::new();
        let (si, sc, lo, rh, tr) = (
            Probe(&fake, AnalysisKind::Silence),
            Probe(&fake, AnalysisKind::Scenes),
            Probe(&fake, AnalysisKind::Loudness),
            Probe(&fake, AnalysisKind::Rhythm),
            Probe(&fake, AnalysisKind::Transcript),
        );
        let providers = AnalysisProviders {
            silence: &si,
            scene: &sc,
            transcriber: &tr,
            loudness: &lo,
            rhythm: &rh,
        };
        let mut heard: Vec<Vec<AnalysisKind>> = Vec::new();
        analyze_steps(
            &asset(),
            &[AnalysisKind::Silence, AnalysisKind::Scenes, AnalysisKind::Loudness],
            &providers,
            &mut |_| {},
            &mut |so_far| heard.push(so_far.ran.clone()),
            NEVER_CANCEL,
        );
        // Each call carries everything finished so far, so a cache written after each one holds
        // the cheap results while the slow ones run.
        assert_eq!(
            heard,
            [
                vec![AnalysisKind::Silence],
                vec![AnalysisKind::Silence, AnalysisKind::Scenes],
                vec![AnalysisKind::Silence, AnalysisKind::Scenes, AnalysisKind::Loudness]
            ]
        );
    }

    #[test]
    fn a_file_with_no_audio_is_not_asked_for_sound_and_nothing_to_run_is_an_error() {
        let _gate = crate::engine::cpu::test_lock();
        use crate::engine::test_support::{img_asset, test_asset, video_stream};
        let silent = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        // Named steps: the audio ones are left out, scene detection stays.
        assert_eq!(
            resolve_steps(&silent, Some(&AnalysisKind::ALL)).unwrap(),
            [AnalysisKind::Scenes],
            "silent b-roll gets no silence, loudness, rhythm or transcript to fail on"
        );
        // Only audio steps named: an error that says why, not an empty run.
        let err = resolve_steps(&silent, Some(&[AnalysisKind::Transcript, AnalysisKind::Silence]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no audio") && err.contains("silence, transcript"), "{err}");
        // A file with sound keeps everything named, in pass order.
        let with_sound = asset();
        assert_eq!(
            resolve_steps(&with_sound, Some(&[AnalysisKind::Transcript, AnalysisKind::Rhythm])).unwrap(),
            [AnalysisKind::Rhythm, AnalysisKind::Transcript]
        );
        // A still has nothing to analyze.
        let still = img_asset(Uuid::new_v4());
        assert!(resolve_steps(&still, Some(&AnalysisKind::ALL))
            .unwrap_err()
            .to_string()
            .contains("still image"));
        // Nothing named and nothing switched on: told so, with the way out.
        let off = AutoAnalysis {
            silence: false,
            scenes: false,
            loudness: false,
            rhythm: false,
            transcript: false,
            ..AutoAnalysis::default()
        };
        let previous = auto_analysis();
        set_auto_analysis(off);
        let none = resolve_steps(&with_sound, None);
        set_auto_analysis(previous);
        let err = none.unwrap_err().to_string();
        assert!(
            err.contains("switched off in Settings") && err.contains("name steps"),
            "{err}"
        );
    }

    #[test]
    fn a_run_where_everything_fails_is_an_error() {
        let _gate = crate::engine::cpu::test_lock();
        let mut fake = Fake::new();
        fake.fail = Some(AnalysisKind::Silence);
        let err = run(&fake, &asset(), &[AnalysisKind::Silence]).into_result().unwrap_err();
        assert!(err.to_string().contains("silence failed"), "{err}");
    }

    #[test]
    fn a_cancelled_step_caches_nothing_and_ends_the_run() {
        let _gate = crate::engine::cpu::test_lock();
        let mut fake = Fake::new();
        fake.cancel_at = Some(AnalysisKind::Loudness);
        let out = run(&fake, &asset(), &AnalysisKind::ALL);
        assert!(out.cancelled);
        // What finished before the cancel is kept; the cancelled step and the rest are not.
        assert_eq!(out.patch.ran, [AnalysisKind::Silence, AnalysisKind::Scenes]);
        assert!(out.patch.loudness.is_none() && out.patch.transcript.is_empty());
        assert_eq!(
            *fake.ran.lock().unwrap(),
            ["silence", "scenes", "loudness"],
            "nothing ran after it"
        );
        assert!(matches!(out.into_result(), Err(Error::Cancelled)));

        // Cancelled before the first step: nothing ran at all.
        let fake = Fake::new();
        let a = asset();
        let (si, sc, lo, rh, tr) = (
            Probe(&fake, AnalysisKind::Silence),
            Probe(&fake, AnalysisKind::Scenes),
            Probe(&fake, AnalysisKind::Loudness),
            Probe(&fake, AnalysisKind::Rhythm),
            Probe(&fake, AnalysisKind::Transcript),
        );
        let providers = AnalysisProviders {
            silence: &si,
            scene: &sc,
            transcriber: &tr,
            loudness: &lo,
            rhythm: &rh,
        };
        let out = analyze_steps(&a, &AnalysisKind::ALL, &providers, &mut |_| {}, &mut |_| {}, &|| true);
        assert!(out.cancelled && out.patch.ran.is_empty() && fake.ran.lock().unwrap().is_empty());
    }

    #[test]
    fn an_analysis_cached_before_kinds_were_recorded_counts_its_data_as_done() {
        // The JSON of an older cache: no `ran` field.
        let legacy: AssetAnalysis = serde_json::from_str(&format!(
            r#"{{"asset_id":"{}","silence_segments":[{{"start":1.0,"end":2.0}}],"scene_changes":[],"transcript":[],"loudness":null,"onsets":[],"tempo":null,"audio_class":null}}"#,
            Uuid::new_v4()
        ))
        .unwrap();
        assert!(legacy.ran.is_empty());
        assert_eq!(legacy.done_kinds(), [AnalysisKind::Silence]);
        // Nothing is written for the field until a step records itself.
        assert!(!serde_json::to_string(&legacy).unwrap().contains("\"ran\""));
        // A step run on top keeps the legacy kind done and records the new one.
        let mut merged = legacy;
        merged.merge(&AssetAnalysis {
            ran: vec![AnalysisKind::Scenes],
            scene_changes: vec![3.0],
            ..AssetAnalysis::default()
        });
        assert_eq!(merged.done_kinds(), [AnalysisKind::Silence, AnalysisKind::Scenes]);
        assert_eq!(merged.silence_segments.len(), 1, "silence untouched");
        let back: AssetAnalysis = serde_json::from_str(&serde_json::to_string(&merged).unwrap()).unwrap();
        assert_eq!(back.done_kinds(), merged.done_kinds());
    }

    #[test]
    fn step_names_parse_with_the_aliases_an_agent_tries() {
        assert_eq!(AnalysisKind::parse("Transcription"), Some(AnalysisKind::Transcript));
        assert_eq!(AnalysisKind::parse("tempo"), Some(AnalysisKind::Rhythm));
        assert_eq!(AnalysisKind::parse("scene_changes"), Some(AnalysisKind::Scenes));
        assert_eq!(AnalysisKind::parse("video"), None);
        assert_eq!(
            AnalysisKind::parse_list(&["transcript", "silence", "silence"]).unwrap(),
            [AnalysisKind::Silence, AnalysisKind::Transcript],
            "pass order, no repeats"
        );
        assert_eq!(AnalysisKind::parse_list(&["all"]).unwrap(), AnalysisKind::ALL);
        let err = AnalysisKind::parse_list(&["silence", "colour"]).unwrap_err().to_string();
        assert!(err.contains("`colour`") && err.contains("transcript"), "{err}");
        assert!(AnalysisKind::parse_list::<&str>(&[]).is_err());
        // The wire names serialize as the aliases-free form.
        assert_eq!(serde_json::to_string(&AnalysisKind::Transcript).unwrap(), "\"transcript\"");
        assert_eq!(
            serde_json::from_str::<AnalysisKind>("\"transcription\"").unwrap(),
            AnalysisKind::Transcript
        );
    }

    #[test]
    fn the_automatic_set_follows_the_toggles_and_the_master_only_governs_import() {
        let mut auto = AutoAnalysis::default();
        assert_eq!(auto.kinds(), AnalysisKind::ALL);
        auto.transcript = false;
        auto.scenes = false;
        assert_eq!(
            auto.kinds(),
            [AnalysisKind::Silence, AnalysisKind::Loudness, AnalysisKind::Rhythm]
        );
        auto.enabled = false;
        assert!(auto.kinds().is_empty(), "master off: nothing on import");
        assert_eq!(
            auto.allowed_kinds(),
            [AnalysisKind::Silence, AnalysisKind::Loudness, AnalysisKind::Rhythm],
            "…but a call that names no steps still honours the per-kind toggles"
        );
        // An older settings file with none of the fields reads as everything on.
        let read: AutoAnalysis = serde_json::from_str("{}").unwrap();
        assert_eq!(read, AutoAnalysis::ALL_ON);
        let read: AutoAnalysis = serde_json::from_str(r#"{"transcript": false}"#).unwrap();
        assert!(read.enabled && read.silence && !read.transcript);
    }

    #[test]
    fn the_status_names_one_state_per_kind() {
        let id = Uuid::new_v4();
        let stored = AssetAnalysis {
            asset_id: id,
            ran: vec![AnalysisKind::Silence, AnalysisKind::Scenes],
            ..AssetAnalysis::default()
        };
        let mut failed = HashMap::new();
        failed.insert(AnalysisKind::Loudness, "ffmpeg exited with 1".to_string());
        // Rhythm is running; transcription is switched off.
        let auto = AutoAnalysis {
            transcript: false,
            ..AutoAnalysis::default()
        };
        let status = status_from(id, Some(&stored), Some(AnalysisKind::Rhythm), &failed, &auto, None);
        let of = |k: AnalysisKind| status.kinds.iter().find(|s| s.kind == k).unwrap();
        assert_eq!(status.kinds.len(), 5);
        assert_eq!(of(AnalysisKind::Silence).state, AnalysisState::Done);
        assert_eq!(of(AnalysisKind::Scenes).state, AnalysisState::Done);
        assert_eq!(of(AnalysisKind::Loudness).state, AnalysisState::Failed);
        assert!(of(AnalysisKind::Loudness).reason.as_ref().unwrap().contains("exited"));
        assert_eq!(of(AnalysisKind::Rhythm).state, AnalysisState::Running);
        assert_eq!(of(AnalysisKind::Transcript).state, AnalysisState::Off);
        assert!(of(AnalysisKind::Transcript).reason.as_ref().unwrap().contains("Settings"));

        // Nothing cached, everything on: all "not run"; master off: all "off".
        let none = status_from(id, None, None, &HashMap::new(), &AutoAnalysis::default(), None);
        assert!(none.kinds.iter().all(|k| k.state == AnalysisState::NotRun));
        let master_off = AutoAnalysis {
            enabled: false,
            ..AutoAnalysis::default()
        };
        let off = status_from(id, None, None, &HashMap::new(), &master_off, None);
        assert!(off.kinds.iter().all(|k| k.state == AnalysisState::Off));
        // A done kind stays done whatever the toggles say, and a failure that a later success
        // cached over is moot.
        let done = status_from(id, Some(&stored), None, &failed, &master_off, None);
        assert_eq!(done.kinds[0].state, AnalysisState::Done);
        // No backend: off, with the reason, and not "not run".
        let nobackend = status_from(
            id,
            None,
            None,
            &HashMap::new(),
            &AutoAnalysis::default(),
            Some("no speech-to-text backend"),
        );
        assert_eq!(nobackend.kinds[4].state, AnalysisState::Off);
        assert_eq!(nobackend.kinds[4].reason.as_deref(), Some("no speech-to-text backend"));
        assert_eq!(nobackend.kinds[0].state, AnalysisState::NotRun);
    }
}
