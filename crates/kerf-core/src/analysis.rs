//! Pluggable analysis providers.
//!
//! Transcription (e.g. `whisper-rs` or an external service), scene detection,
//! and silence detection are abstracted behind traits so concrete engines can
//! be swapped in without touching the rest of the core.

use crate::error::Result;
use crate::model::{Asset, AssetAnalysis, TimeRange, TranscriptSegment};

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

/// A bundle of analysis providers to run against an asset.
pub struct AnalysisProviders<'a> {
    pub silence: &'a dyn SilenceDetector,
    pub scene: &'a dyn SceneDetector,
    pub transcriber: &'a dyn Transcriber,
}

impl<'a> AnalysisProviders<'a> {
    /// All providers wired to [`NullAnalyzer`].
    pub fn null(null: &'a NullAnalyzer) -> Self {
        Self {
            silence: null,
            scene: null,
            transcriber: null,
        }
    }
}

/// Run every configured provider and assemble an [`AssetAnalysis`].
pub fn analyze(asset: &Asset, providers: &AnalysisProviders) -> Result<AssetAnalysis> {
    Ok(AssetAnalysis {
        asset_id: asset.id,
        silence_segments: providers.silence.detect_silence(asset)?,
        scene_changes: providers.scene.detect_scenes(asset)?,
        transcript: providers.transcriber.transcribe(asset)?,
    })
}
