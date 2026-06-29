//! Pluggable analysis providers.
//!
//! Transcription (e.g. `whisper-rs` or an external service), scene detection,
//! and silence detection are abstracted behind traits so concrete engines can
//! be swapped in without touching the rest of the core.

use crate::error::Result;
use crate::model::{Asset, AssetAnalysis, AudioClassification, Loudness, Tempo, TimeRange, TranscriptSegment};

/// Detects silent spans in an asset's audio.
pub trait SilenceDetector: Send + Sync {
    fn detect_silence(&self, asset: &Asset) -> Result<Vec<TimeRange>>;
}

/// Detects scene-change timestamps in an asset's video.
pub trait SceneDetector: Send + Sync {
    fn detect_scenes(&self, asset: &Asset) -> Result<Vec<f64>>;
}

/// Produces a timecoded transcript from an asset's audio.
pub trait Transcriber: Send + Sync {
    fn transcribe(&self, asset: &Asset) -> Result<Vec<TranscriptSegment>>;
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

/// Local speech-to-text via `whisper-rs`. Audio is decoded to 16 kHz mono with
/// the `ffmpeg` binary, then transcribed with a ggml model.
#[cfg(feature = "whisper")]
pub struct WhisperTranscriber {
    /// Path to a ggml whisper model file (e.g. `ggml-base.en.bin`).
    pub model_path: std::path::PathBuf,
    /// Spoken language hint (e.g. `"en"`); `None` lets whisper auto-detect.
    pub language: Option<String>,
}

#[cfg(feature = "whisper")]
impl Transcriber for WhisperTranscriber {
    fn transcribe(&self, asset: &Asset) -> Result<Vec<TranscriptSegment>> {
        use crate::error::Error;
        use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

        let samples = crate::engine::decode_audio_16k_mono(std::path::Path::new(&asset.path))?;

        let ctx = WhisperContext::new_with_params(&self.model_path.to_string_lossy(), WhisperContextParameters::default())
            .map_err(|e| Error::Engine(format!("whisper: failed to load model: {e}")))?;
        let mut state = ctx.create_state().map_err(|e| Error::Engine(format!("whisper: {e}")))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        if let Some(lang) = &self.language {
            params.set_language(Some(lang));
        }
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, &samples)
            .map_err(|e| Error::Engine(format!("whisper: inference failed: {e}")))?;

        let n = state.full_n_segments().map_err(|e| Error::Engine(format!("whisper: {e}")))?;
        let mut segments = Vec::new();
        for i in 0..n {
            let text = state
                .full_get_segment_text(i)
                .map_err(|e| Error::Engine(format!("whisper: {e}")))?;
            let t0 = state
                .full_get_segment_t0(i)
                .map_err(|e| Error::Engine(format!("whisper: {e}")))?;
            let t1 = state
                .full_get_segment_t1(i)
                .map_err(|e| Error::Engine(format!("whisper: {e}")))?;
            // whisper timestamps are in centiseconds.
            segments.push(TranscriptSegment {
                start: t0 as f64 / 100.0,
                end: t1 as f64 / 100.0,
                text: text.trim().to_string(),
            });
        }
        Ok(segments)
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
    fn transcribe(&self, _asset: &Asset) -> Result<Vec<TranscriptSegment>> {
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

/// Run the default analysis providers (FFmpeg silence + scene detection, and
/// `whisper` transcription when the feature is on and `KERF_WHISPER_MODEL` is
/// set) against an asset's media file and assemble an [`AssetAnalysis`].
///
/// This is the heavy, ffmpeg-bound part of [`crate::project::Project::analyze_asset`],
/// pulled out as a free function so a caller holding the shared `Project` lock
/// can release it before running this and re-acquire it only to cache the
/// result.
pub fn analyze_asset_media(asset: &Asset) -> Result<AssetAnalysis> {
    let silence = FfmpegSilenceDetector::default();
    let scene = FfmpegSceneDetector::default();
    let loudness = FfmpegLoudnessAnalyzer;
    let onset = FfmpegOnsetDetector::default();
    let tempo = FfmpegTempoDetector;
    let classifier = HeuristicAudioClassifier;
    let null = NullAnalyzer;

    #[cfg(feature = "whisper")]
    let whisper = std::env::var("KERF_WHISPER_MODEL")
        .ok()
        .filter(|m| !m.is_empty())
        .map(|m| WhisperTranscriber {
            model_path: m.into(),
            language: None,
        });
    #[cfg(feature = "whisper")]
    let transcriber: &dyn Transcriber = whisper.as_ref().map(|w| w as &dyn Transcriber).unwrap_or(&null);
    #[cfg(not(feature = "whisper"))]
    let transcriber: &dyn Transcriber = &null;

    let providers = AnalysisProviders {
        silence: &silence,
        scene: &scene,
        transcriber,
        loudness: &loudness,
        onset: &onset,
        tempo: &tempo,
        classifier: &classifier,
    };
    analyze(asset, &providers)
}

/// Run every configured provider and assemble an [`AssetAnalysis`].
pub fn analyze(asset: &Asset, providers: &AnalysisProviders) -> Result<AssetAnalysis> {
    Ok(AssetAnalysis {
        asset_id: asset.id,
        silence_segments: providers.silence.detect_silence(asset)?,
        scene_changes: providers.scene.detect_scenes(asset)?,
        transcript: providers.transcriber.transcribe(asset)?,
        loudness: providers.loudness.measure(asset)?,
        onsets: providers.onset.detect_onsets(asset)?,
        tempo: providers.tempo.detect_tempo(asset)?,
        audio_class: providers.classifier.classify(asset)?,
    })
}
