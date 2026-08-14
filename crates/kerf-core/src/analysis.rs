//! Pluggable analysis providers.
//!
//! Transcription (e.g. `whisper-rs` or an external service), scene detection,
//! and silence detection are abstracted behind traits so concrete engines can
//! be swapped in without touching the rest of the core.

use crate::engine::whisper;
use crate::error::Result;
use crate::model::{Asset, AssetAnalysis, AudioClassification, Loudness, Tempo, TimeRange, TranscriptSegment};

/// A step of an analysis pass, reported as it runs.
///
/// Analysis used to be a single opaque wait, which was tolerable while it was a
/// handful of ffmpeg passes. Transcription changes that: the first run may
/// download a few hundred megabytes and then spend minutes on inference, and a
/// spinner with no numbers on it is indistinguishable from a hang.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AnalysisProgress {
    /// Machine-readable step name — `silence`, `scenes`, `loudness`, `rhythm`,
    /// `download_model`, `transcribe`, `done`.
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
    fn transcribe(&self, asset: &Asset, progress: ProgressFn) -> Result<Vec<TranscriptSegment>>;
}

/// Measures EBU R128 loudness of an asset's audio (`None` for silent / video-only
/// assets).
pub trait LoudnessAnalyzer: Send + Sync {
    fn measure(&self, asset: &Asset) -> Result<Option<Loudness>>;
}

/// Detects onset (transient) timestamps in an asset's audio.
pub trait OnsetDetector: Send + Sync {
    fn detect_onsets(&self, asset: &Asset) -> Result<Vec<f64>>;
}

/// Estimates tempo and a beat grid for an asset's audio (`None` when it has no
/// usable rhythm).
pub trait TempoDetector: Send + Sync {
    fn detect_tempo(&self, asset: &Asset) -> Result<Option<Tempo>>;
}

/// Classifies an asset's audio as speech / music / mixed (`None` for silent /
/// video-only assets).
pub trait AudioClassifier: Send + Sync {
    fn classify(&self, asset: &Asset) -> Result<Option<AudioClassification>>;
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

/// Onset detection backed by light DSP (energy flux) on PCM decoded with the
/// `ffmpeg` binary, so no dev libraries are required.
pub struct FfmpegOnsetDetector {
    /// Adaptive-threshold std-dev multiplier; higher = fewer, stronger onsets.
    pub sensitivity: f64,
}

impl Default for FfmpegOnsetDetector {
    fn default() -> Self {
        Self { sensitivity: 1.5 }
    }
}

impl OnsetDetector for FfmpegOnsetDetector {
    fn detect_onsets(&self, asset: &Asset) -> Result<Vec<f64>> {
        if !asset.has_audio() {
            return Ok(Vec::new());
        }
        crate::engine::detect_onsets(std::path::Path::new(&asset.path), self.sensitivity)
    }
}

/// Tempo estimation backed by autocorrelation of the onset envelope (PCM decoded
/// with the `ffmpeg` binary), so no dev libraries are required.
pub struct FfmpegTempoDetector;

impl TempoDetector for FfmpegTempoDetector {
    fn detect_tempo(&self, asset: &Asset) -> Result<Option<Tempo>> {
        if !asset.has_audio() {
            return Ok(None);
        }
        crate::engine::detect_tempo(std::path::Path::new(&asset.path))
    }
}

/// Speech/music classification by light DSP on PCM decoded with the `ffmpeg`
/// binary, so no dev libraries are required.
pub struct HeuristicAudioClassifier;

impl AudioClassifier for HeuristicAudioClassifier {
    fn classify(&self, asset: &Asset) -> Result<Option<AudioClassification>> {
        if !asset.has_audio() {
            return Ok(None);
        }
        crate::engine::classify_audio(std::path::Path::new(&asset.path))
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
    fn transcribe(&self, asset: &Asset, progress: ProgressFn) -> Result<Vec<TranscriptSegment>> {
        let model = whisper::ensure_model(&mut model_progress(progress))?;
        progress(AnalysisProgress::with_fraction("transcribe", 0.0));
        whisper::transcribe(
            std::path::Path::new(&asset.path),
            &model,
            self.language.as_deref(),
            asset.duration,
            &mut |f| progress(AnalysisProgress::with_fraction("transcribe", f)),
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
    fn transcribe(&self, asset: &Asset, progress: ProgressFn) -> Result<Vec<TranscriptSegment>> {
        use crate::error::Error;

        let model = whisper::ensure_model(&mut model_progress(progress))?;
        progress(AnalysisProgress::with_fraction("transcribe", 0.0));
        let samples = crate::engine::decode_audio_16k_mono(std::path::Path::new(&asset.path))?;
        let language = self.language.clone();

        // whisper-rs wants a `'static` progress callback, and `full()` blocks
        // for the whole inference — so run it on a worker and pump percentages
        // back over a channel instead of holding a borrow across the call.
        let (tx, rx) = std::sync::mpsc::channel::<i32>();
        let worker = std::thread::spawn(move || run_whisper_rs(samples, model, language, tx));
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
    progress: std::sync::mpsc::Sender<i32>,
) -> Result<Vec<TranscriptSegment>> {
    use crate::error::Error;
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    let ctx = WhisperContext::new_with_params(&model, WhisperContextParameters::default())
        .map_err(|e| Error::Engine(format!("whisper: failed to load model: {e}")))?;
    let mut state = ctx.create_state().map_err(|e| Error::Engine(format!("whisper: {e}")))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
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

/// Describe the speech-to-text backend this build would use.
pub fn transcription_status() -> TranscriptionStatus {
    let (backend, available, reason) = if cfg!(feature = "whisper") {
        ("libwhisper", true, None)
    } else if whisper::filter_available() {
        ("ffmpeg_filter", true, None)
    } else {
        (
            "none",
            false,
            Some(
                "no speech-to-text backend: this build has no in-process whisper, and the `ffmpeg` \
                 binary has no `whisper` filter (FFmpeg 8.0+ built with --enable-whisper)"
                    .to_string(),
            ),
        )
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
    fn transcribe(&self, _asset: &Asset, _progress: ProgressFn) -> Result<Vec<TranscriptSegment>> {
        Ok(Vec::new())
    }
}

impl LoudnessAnalyzer for NullAnalyzer {
    fn measure(&self, _asset: &Asset) -> Result<Option<Loudness>> {
        Ok(None)
    }
}

impl OnsetDetector for NullAnalyzer {
    fn detect_onsets(&self, _asset: &Asset) -> Result<Vec<f64>> {
        Ok(Vec::new())
    }
}

impl TempoDetector for NullAnalyzer {
    fn detect_tempo(&self, _asset: &Asset) -> Result<Option<Tempo>> {
        Ok(None)
    }
}

impl AudioClassifier for NullAnalyzer {
    fn classify(&self, _asset: &Asset) -> Result<Option<AudioClassification>> {
        Ok(None)
    }
}

/// A bundle of analysis providers to run against an asset.
pub struct AnalysisProviders<'a> {
    pub silence: &'a dyn SilenceDetector,
    pub scene: &'a dyn SceneDetector,
    pub transcriber: &'a dyn Transcriber,
    pub loudness: &'a dyn LoudnessAnalyzer,
    pub onset: &'a dyn OnsetDetector,
    pub tempo: &'a dyn TempoDetector,
    pub classifier: &'a dyn AudioClassifier,
}

impl<'a> AnalysisProviders<'a> {
    /// All providers wired to [`NullAnalyzer`].
    pub fn null(null: &'a NullAnalyzer) -> Self {
        Self {
            silence: null,
            scene: null,
            transcriber: null,
            loudness: null,
            onset: null,
            tempo: null,
            classifier: null,
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

/// Run the default analysis providers (FFmpeg silence / scene / loudness /
/// rhythm detection, plus speech-to-text through whichever whisper backend this
/// build has) against an asset's media file and assemble an [`AssetAnalysis`].
///
/// This is the heavy, ffmpeg-bound part of [`crate::project::Project::analyze_asset`],
/// pulled out as a free function so a caller holding the shared `Project` lock
/// can release it before running this and re-acquire it only to cache the
/// result.
pub fn analyze_asset_media(asset: &Asset) -> Result<AssetAnalysis> {
    analyze_asset_media_with_progress(asset, &mut |_| {})
}

/// [`analyze_asset_media`], reporting each step as it starts and finishes.
///
/// Transcription in particular can download a model and then run for minutes,
/// so the adapters use this variant and forward the events to the GUI / an
/// agent; the silent wrapper above stays for callers that don't care.
pub fn analyze_asset_media_with_progress(asset: &Asset, progress: ProgressFn) -> Result<AssetAnalysis> {
    let silence = FfmpegSilenceDetector::default();
    let scene = FfmpegSceneDetector::default();
    let loudness = FfmpegLoudnessAnalyzer;
    let onset = FfmpegOnsetDetector::default();
    let tempo = FfmpegTempoDetector;
    let classifier = HeuristicAudioClassifier;
    let null = NullAnalyzer;

    let transcriber = default_transcriber();
    let transcriber: &dyn Transcriber = transcriber.as_deref().unwrap_or(&null);

    let providers = AnalysisProviders {
        silence: &silence,
        scene: &scene,
        transcriber,
        loudness: &loudness,
        onset: &onset,
        tempo: &tempo,
        classifier: &classifier,
    };
    analyze_with_progress(asset, &providers, progress)
}

/// Run every configured provider and assemble an [`AssetAnalysis`].
pub fn analyze(asset: &Asset, providers: &AnalysisProviders) -> Result<AssetAnalysis> {
    analyze_with_progress(asset, providers, &mut |_| {})
}

/// [`analyze`], announcing each step to `progress` as it begins.
pub fn analyze_with_progress(asset: &Asset, providers: &AnalysisProviders, progress: ProgressFn) -> Result<AssetAnalysis> {
    progress(AnalysisProgress::stage("silence"));
    let silence_segments = providers.silence.detect_silence(asset)?;
    progress(AnalysisProgress::stage("scenes"));
    let scene_changes = providers.scene.detect_scenes(asset)?;
    progress(AnalysisProgress::stage("loudness"));
    let loudness = providers.loudness.measure(asset)?;
    progress(AnalysisProgress::stage("rhythm"));
    let onsets = providers.onset.detect_onsets(asset)?;
    let tempo = providers.tempo.detect_tempo(asset)?;
    let audio_class = providers.classifier.classify(asset)?;
    // Transcription runs last: it is by far the slowest step, and the ones above
    // are what the timeline draws — markers and waveform regions appear as soon
    // as the caller caches this, rather than waiting behind minutes of inference.
    let transcript = providers.transcriber.transcribe(asset, progress)?;
    progress(AnalysisProgress::stage("done"));
    Ok(AssetAnalysis {
        asset_id: asset.id,
        silence_segments,
        scene_changes,
        transcript,
        loudness,
        onsets,
        tempo,
        audio_class,
    })
}
