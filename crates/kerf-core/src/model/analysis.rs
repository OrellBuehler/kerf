//! What analysis finds in an asset: levels, loudness, rhythm, the beat grid and the stored result.

use super::*;

/// A half-open time range `[start, end)` in seconds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

/// A transcript line with timecodes (seconds).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// The line's words with their own source times, when known (see [`crate::transcript`]).
    /// Empty in a transcript made before words were recorded and in imported captions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
    /// Who speaks the line (`S1`, `S2`, …), when speakers were labelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

/// One word of a [`TranscriptSegment`], in the asset's source time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// The recognizer's confidence, 0..1, when it reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// The times were spread over the line by character share (the backend reports
    /// none), then tightened to the speech energy.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

/// EBU R128 loudness measurement of an asset's audio, from a single `loudnorm`
/// analysis pass. Lets an agent level a clip to a target or balance a voiceover
/// against a music bed instead of guessing at a linear gain.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Loudness {
    /// Integrated (program) loudness in LUFS.
    pub integrated_lufs: f64,
    /// Loudness range (LRA) in LU.
    pub loudness_range: f64,
    /// Maximum true peak in dBTP.
    pub true_peak_dbtp: f64,
    /// Gating threshold used for the measurement, in LUFS.
    pub threshold_lufs: f64,
}

/// The streaming-delivery loudness target, in LUFS — the level the export's
/// `loudnorm` normalises to, and what [`Levels`] judges a mix against.
pub const LEVELS_TARGET_LUFS: f64 = -14.0;
/// The true-peak ceiling platforms ask of a delivery, in dBTP.
pub const LEVELS_TRUE_PEAK_CEILING_DBTP: f64 = -1.0;
/// How much further than the overshoot the notes advise lowering a limiter's ceiling:
/// the peak between samples moves with the signal, so landing exactly on the line is
/// not landing under it.
const LEVELS_CEILING_MARGIN_DB: f64 = 0.5;

/// What one `ebur128` meter read over a stretch of the mix. Every number is
/// `None` where there is nothing to report — a silent stretch has no integrated
/// loudness and a peak of minus infinity, neither of which JSON can carry.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct LevelReading {
    /// Integrated (programme) loudness in LUFS, gated per EBU R128. `None` when
    /// nothing in the span was loud enough to count.
    pub integrated_lufs: Option<f64>,
    /// Loudness range in LU: how far the loud and quiet passages sit apart.
    pub loudness_range_lu: Option<f64>,
    /// The loudest short-term (3 s window) loudness reached, in LUFS. `None` for
    /// a span under three seconds, where the first full window never closes.
    pub short_term_max_lufs: Option<f64>,
    /// The highest sample, in dBFS.
    pub peak_dbfs: Option<f64>,
    /// The highest *true* (inter-sample) peak in dBTP — what a re-encode can
    /// clip on, and so what the master is judged on for delivery.
    pub true_peak_dbtp: Option<f64>,
}

/// One track's reading: the strip's output, after its fader and pan and ahead of
/// the duck bus and the master.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackLevels {
    pub track_id: Uuid,
    pub name: String,
    pub kind: StreamKind,
    /// The track is flagged to duck under the rest of the mix. Its reading is
    /// taken *before* the duck, so it is as loud as the track is on its own.
    pub ducked: bool,
    /// Whether the track reaches the render at all — `false` for a muted track,
    /// or one a solo shadows, which is why it has no `level`.
    pub heard: bool,
    pub level: Option<LevelReading>,
}

/// How loud the cut is: the finished mix and each track that feeds it, measured
/// in one pass over the audio the export would render.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Levels {
    /// Seconds of the cut measured (the range asked for, clamped to the cut).
    pub duration: f64,
    /// The finished mix — master fader, limiter and, when asked for, `loudnorm`
    /// included. `None` when the cut has no audio to measure.
    pub master: Option<LevelReading>,
    pub tracks: Vec<TrackLevels>,
    /// Whether the measurement ran through `loudnorm`, as an export with
    /// normalisation on would.
    pub loudnorm: bool,
    /// The streaming loudness target the notes judge against.
    pub target_lufs: f64,
    /// What the numbers mean for delivery, phrased as advice (over target,
    /// over the true-peak ceiling, a track clipping the sum).
    pub notes: Vec<String>,
}

impl Levels {
    /// Put a measurement together with the tracks it came from and judge it, given
    /// the master bus the mix went through (the advice for a hot peak depends on
    /// whether its limiter is already on).
    pub fn new(duration: f64, master: Option<LevelReading>, tracks: Vec<TrackLevels>, loudnorm: bool, bus: &MasterBus) -> Self {
        let notes = level_notes(master.as_ref(), &tracks, bus);
        Self {
            duration,
            master,
            tracks,
            loudnorm,
            target_lufs: LEVELS_TARGET_LUFS,
            notes,
        }
    }
}

/// The advice behind [`Levels::notes`], pure so the thresholds are tested.
fn level_notes(master: Option<&LevelReading>, tracks: &[TrackLevels], bus: &MasterBus) -> Vec<String> {
    let Some(master) = master else {
        return vec!["The cut has no audio to measure.".to_string()];
    };
    let mut notes = Vec::new();
    match master.integrated_lufs {
        None => notes.push("The mix is silent.".to_string()),
        Some(i) if i - LEVELS_TARGET_LUFS > 1.0 => notes.push(format!(
            "Integrated loudness {i:.1} LUFS is {:.1} LU over the {LEVELS_TARGET_LUFS:.0} LUFS streaming target — platforms \
             will turn it down. Lower the master or the loudest track, or export with loudnorm.",
            i - LEVELS_TARGET_LUFS
        )),
        Some(i) if LEVELS_TARGET_LUFS - i > 3.0 => notes.push(format!(
            "Integrated loudness {i:.1} LUFS is {:.1} LU under the {LEVELS_TARGET_LUFS:.0} LUFS streaming target — it will \
             sound quiet beside other posts. Raise the master or the tracks, or export with loudnorm.",
            LEVELS_TARGET_LUFS - i
        )),
        Some(i) => notes.push(format!(
            "Integrated loudness {i:.1} LUFS is close to the {LEVELS_TARGET_LUFS:.0} LUFS streaming target."
        )),
    }
    if let Some(tp) = master.true_peak_dbtp.filter(|tp| *tp > LEVELS_TRUE_PEAK_CEILING_DBTP) {
        let over = format!(
            "True peak {tp:.1} dBTP is over the {LEVELS_TRUE_PEAK_CEILING_DBTP:.0} dBTP platforms ask for and can clip when \
             re-encoded."
        );
        if bus.limiter {
            // A sample-peak limiter leaves the peak between samples over its ceiling: the fix
            // is true-peak limiting, or the ceiling taken down by the overshoot and a margin.
            // A true-peak limiter already holds it, so only a lower ceiling is left.
            let ceiling = bus.safe_ceiling_db();
            let lower = (ceiling - (tp - LEVELS_TRUE_PEAK_CEILING_DBTP) - LEVELS_CEILING_MARGIN_DB).max(MASTER_MIN_CEILING_DB);
            notes.push(match (bus.true_peak, lower < ceiling) {
                (false, true) => format!(
                    "{over} The master limiter is already on, but it holds the sample peak at {ceiling:.1} dBFS and the peak \
                     between samples runs above that. Turn on true-peak limiting (set_master_true_peak), or lower its \
                     ceiling to about {lower:.1} dBFS (set_master_limiter)."
                ),
                (false, false) => format!(
                    "{over} The master limiter is already on at its lowest ceiling. Turn on true-peak limiting \
                     (set_master_true_peak), or lower the master or the loudest track."
                ),
                (true, true) => format!(
                    "{over} The master limiter is already on and holds the true peak at {ceiling:.1} dBFS. Lower its \
                     ceiling to about {lower:.1} dBFS (set_master_limiter)."
                ),
                (true, false) => format!(
                    "{over} The master limiter is already on, holding the true peak at its lowest ceiling. Lower the \
                     master or the loudest track."
                ),
            });
        } else {
            notes.push(format!(
                "{over} Turn on the master limiter (set_master_limiter) with true-peak limiting (set_master_true_peak), \
                 or lower the master."
            ));
        }
    }
    for track in tracks {
        if let Some(peak) = track.level.and_then(|l| l.peak_dbfs).filter(|p| *p > 0.0) {
            notes.push(format!(
                "Track {} peaks at {peak:+.1} dBFS before the master, over full scale. Nothing clips until the mix is written, \
                 so the master can still bring it under (its fader or limiter); otherwise lower this track's fader.",
                track.name
            ));
        }
    }
    notes
}

/// Coarse content class of an asset's audio. Heuristic (energy continuity +
/// zero-crossing-rate variability), so it is a hint, not a trained classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioClass {
    /// Predominantly spoken word (gappy energy, variable ZCR).
    Speech,
    /// Predominantly music (continuous energy, steady ZCR).
    Music,
    /// Both present (e.g. dialogue over a music bed).
    Mixed,
    /// Could not be determined.
    Unknown,
}

/// An [`AudioClass`] verdict with a confidence in 0.0–1.0.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AudioClassification {
    pub class: AudioClass,
    pub confidence: f64,
}

/// Estimated tempo and beat grid for an asset's audio. Best-effort: derived by
/// autocorrelating the onset envelope, so it is most reliable on percussive
/// music and may land on a tempo octave — gate on `confidence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tempo {
    /// Estimated tempo in beats per minute.
    pub bpm: f64,
    /// Beat timestamps in seconds across the asset.
    pub beats: Vec<f64>,
    /// How periodic the audio is, 0.0–1.0 (the normalized autocorrelation peak).
    pub confidence: f64,
    /// Bar starts in seconds, when a fixed bar grid could be fitted (see
    /// [`MusicAnalysis`]); empty otherwise. With a grid, `beats` are its beats too.
    #[serde(default)]
    pub downbeats: Vec<f64>,
}

impl Tempo {
    /// The tempo read off a fitted bar grid: its beats and downbeats across `duration`
    /// (the few ms a first beat may sit before 0 dropped), and at least
    /// [`BEAT_MIN_CONFIDENCE`] — the fit already refused anything without a steady pulse.
    pub fn from_grid(grid: &BeatGrid, duration: f64, confidence: f64) -> Self {
        Self {
            bpm: grid.bpm(),
            beats: grid.beats(duration).into_iter().filter(|t| *t >= 0.0).collect(),
            confidence: confidence.max(BEAT_MIN_CONFIDENCE),
            downbeats: grid.downbeats(duration).into_iter().filter(|t| *t >= 0.0).collect(),
        }
    }
}

/// A fixed beat grid fitted to a piece of music: beat `k` is at `phase_s + k * period_s`,
/// and the beats `downbeat_offset`, `downbeat_offset + beats_per_bar`, … start bars.
/// `phase_s` is the first beat: it lies in `[-0.02, period_s - 0.02)`, so a beat on the
/// file's first sample that the fit places a hair early is still the first beat.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BeatGrid {
    pub period_s: f64,
    pub phase_s: f64,
    pub downbeat_offset: u32,
    pub beats_per_bar: u32,
}

impl BeatGrid {
    pub fn bpm(&self) -> f64 {
        60.0 / self.period_s
    }

    pub fn bar_s(&self) -> f64 {
        self.period_s * self.beats_per_bar as f64
    }

    /// The first downbeat (at most a few ms before 0).
    pub fn first_downbeat(&self) -> f64 {
        self.phase_s + self.downbeat_offset as f64 * self.period_s
    }

    /// Where bar `k` starts (counted from the first downbeat).
    pub fn bar_start(&self, k: usize) -> f64 {
        self.first_downbeat() + k as f64 * self.bar_s()
    }

    /// How many whole bars fit between the first downbeat and `duration`.
    pub fn whole_bars(&self, duration: f64) -> usize {
        let span = duration - self.first_downbeat();
        if span <= 0.0 || self.bar_s() <= 0.0 {
            return 0;
        }
        // A bar that ends within a microsecond of the end still counts as whole.
        ((span + 1e-6) / self.bar_s()).floor() as usize
    }

    /// Every beat in `[0, duration)`.
    pub fn beats(&self, duration: f64) -> Vec<f64> {
        if self.period_s <= 0.0 {
            return Vec::new();
        }
        (0..)
            .map(|k| self.phase_s + k as f64 * self.period_s)
            .take_while(|t| *t < duration)
            .collect()
    }

    /// Every downbeat in `[0, duration)`.
    pub fn downbeats(&self, duration: f64) -> Vec<f64> {
        if self.bar_s() <= 0.0 {
            return Vec::new();
        }
        (0..).map(|k| self.bar_start(k)).take_while(|t| *t < duration).collect()
    }
}

/// Two phrases of `bars` bars, starting at bars `a` and `b` (counted from the first
/// downbeat), whose bars all match in harmony — jumping from one to the other is a
/// splice the ear does not hear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhraseMatch {
    pub a: usize,
    pub b: usize,
    pub bars: usize,
}

/// The bar-level structure of a piece of music: its beat grid, the harmony of each
/// whole bar and the repeating phrases between them. What "fit music to length"
/// plans its splices from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MusicAnalysis {
    pub grid: BeatGrid,
    /// Length of the analysed audio in seconds.
    pub duration: f64,
    /// L2-normalized 12-bin chroma (C = 0 … B = 11, A4 = 440 Hz) of every whole bar,
    /// from the first downbeat. All zeros for a silent bar.
    pub bar_chroma: Vec<[f32; 12]>,
    /// Repeating 8- and 4-bar phrases (`a < b`), the legal splice points.
    pub phrases: Vec<PhraseMatch>,
}

/// Everything the rhythm analysis pass derives from one decoded PCM stream:
/// onsets, tempo and the speech/music class. Bundled because the three share
/// the decode (and onsets/tempo the onset envelope) — computing them together
/// costs one full-file ffmpeg decode instead of three.
#[derive(Debug, Clone, Default)]
pub struct Rhythm {
    pub onsets: Vec<f64>,
    pub tempo: Option<Tempo>,
    pub audio_class: Option<AudioClassification>,
    pub music: Option<MusicAnalysis>,
}

/// One kind of analysis an asset can have: each is a step of its own, can be run alone
/// and is cached independently (see [`AssetAnalysis::ran`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisKind {
    /// Silent spans (`silence_segments`).
    Silence,
    /// Scene changes (`scene_changes`).
    Scenes,
    /// EBU R128 loudness (`loudness`).
    Loudness,
    /// Onsets, tempo and the speech/music class (`onsets`, `tempo`, `audio_class`).
    Rhythm,
    /// Speech-to-text (`transcript`).
    #[serde(alias = "transcription", alias = "transcribe")]
    Transcript,
}

impl AnalysisKind {
    /// Every kind, in the order a pass runs them: the cheap ones the timeline draws
    /// first, transcription — by far the slowest — last.
    pub const ALL: [AnalysisKind; 5] = [
        AnalysisKind::Silence,
        AnalysisKind::Scenes,
        AnalysisKind::Loudness,
        AnalysisKind::Rhythm,
        AnalysisKind::Transcript,
    ];

    /// Whether the step reads the audio: everything but scene detection. A picture with no
    /// sound has none of it to find — and nothing to fail on.
    pub fn needs_audio(self) -> bool {
        self != AnalysisKind::Scenes
    }

    /// Whether the step has anything to find in a synthesized sound effect: only its silence and
    /// loudness. It has no picture to cut, no beat to read and no words to hear.
    pub fn fits_effect(self) -> bool {
        matches!(self, AnalysisKind::Silence | AnalysisKind::Loudness)
    }

    /// The wire name (`silence`, `scenes`, `loudness`, `rhythm`, `transcript`).
    pub fn name(self) -> &'static str {
        match self {
            AnalysisKind::Silence => "silence",
            AnalysisKind::Scenes => "scenes",
            AnalysisKind::Loudness => "loudness",
            AnalysisKind::Rhythm => "rhythm",
            AnalysisKind::Transcript => "transcript",
        }
    }

    /// The kind a name (or an alias an agent is likely to try) means.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "silence" | "silences" => Some(AnalysisKind::Silence),
            "scenes" | "scene" | "scene_changes" => Some(AnalysisKind::Scenes),
            "loudness" | "lufs" => Some(AnalysisKind::Loudness),
            "rhythm" | "tempo" | "beat" | "beats" | "onsets" => Some(AnalysisKind::Rhythm),
            "transcript" | "transcription" | "transcribe" | "speech" => Some(AnalysisKind::Transcript),
            _ => None,
        }
    }

    /// A list of names (`all` meaning every kind) as kinds, in pass order and without
    /// repeats. An empty list is an error: asking for no steps is a mistake, not a no-op.
    pub fn parse_list<S: AsRef<str>>(names: &[S]) -> Result<Vec<AnalysisKind>> {
        let mut wanted = std::collections::HashSet::new();
        for name in names {
            let name = name.as_ref();
            if name.trim().eq_ignore_ascii_case("all") {
                wanted.extend(AnalysisKind::ALL);
            } else {
                wanted.insert(AnalysisKind::parse(name).ok_or_else(|| {
                    Error::InvalidArgument(format!(
                        "unknown analysis step `{name}`; expected any of silence, scenes, loudness, rhythm, transcript, or all"
                    ))
                })?);
            }
        }
        if wanted.is_empty() {
            return Err(Error::InvalidArgument(
                "no analysis steps were named; pass some of silence, scenes, loudness, rhythm, transcript, or all".to_string(),
            ));
        }
        Ok(AnalysisKind::ALL.into_iter().filter(|k| wanted.contains(k)).collect())
    }
}

/// Cached, pluggable analysis results for an asset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssetAnalysis {
    pub asset_id: Uuid,
    #[serde(default)]
    pub silence_segments: Vec<TimeRange>,
    #[serde(default)]
    pub scene_changes: Vec<f64>,
    #[serde(default)]
    pub transcript: Vec<TranscriptSegment>,
    /// EBU R128 loudness of the asset's audio, when it has any. `None` until the
    /// asset is analyzed (and for silent / video-only assets).
    #[serde(default)]
    pub loudness: Option<Loudness>,
    /// Onset (transient) timestamps in seconds — moments where new sound energy
    /// arrives. Snap cut points to these to land edits on the beat.
    #[serde(default)]
    pub onsets: Vec<f64>,
    /// Estimated tempo and beat grid, when the audio is rhythmic enough. `None`
    /// for silent / video-only assets and non-rhythmic material.
    #[serde(default)]
    pub tempo: Option<Tempo>,
    /// Coarse speech/music classification of the audio. `None` for silent /
    /// video-only assets. Route ducking/leveling decisions off this.
    #[serde(default)]
    pub audio_class: Option<AudioClassification>,
    /// Bar-level structure (fitted beat grid, chroma, repeating phrases), when the
    /// audio has a steady enough pulse to fit one.
    #[serde(default)]
    pub music: Option<MusicAnalysis>,
    /// The kinds whose step has run to completion — the only way to tell "ran and
    /// found nothing" (no silence, no speech) from "never ran", both of which leave the
    /// data empty. Absent in an analysis cached before kinds were recorded: for those
    /// a kind counts as done when it has data ([`AssetAnalysis::done`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ran: Vec<AnalysisKind>,
}

impl AssetAnalysis {
    /// Whether `kind` has data in this analysis, whatever recorded it.
    fn has_data(&self, kind: AnalysisKind) -> bool {
        match kind {
            AnalysisKind::Silence => !self.silence_segments.is_empty(),
            AnalysisKind::Scenes => !self.scene_changes.is_empty(),
            AnalysisKind::Loudness => self.loudness.is_some(),
            AnalysisKind::Rhythm => !self.onsets.is_empty() || self.tempo.is_some() || self.audio_class.is_some(),
            AnalysisKind::Transcript => !self.transcript.is_empty(),
        }
    }

    /// Whether `kind` is done: its step ran (recorded in [`AssetAnalysis::ran`]), or —
    /// for an analysis from before that was recorded — it holds data of that kind.
    pub fn done(&self, kind: AnalysisKind) -> bool {
        self.ran.contains(&kind) || self.has_data(kind)
    }

    /// The kinds that are done, in pass order.
    pub fn done_kinds(&self) -> Vec<AnalysisKind> {
        AnalysisKind::ALL.into_iter().filter(|k| self.done(*k)).collect()
    }

    /// Fold `patch` — the results of the steps just run, with those steps in its
    /// `ran` — into this analysis, replacing the data of those kinds and leaving every
    /// other kind exactly as it was.
    pub fn merge(&mut self, patch: &AssetAnalysis) {
        for kind in AnalysisKind::ALL {
            if !patch.ran.contains(&kind) {
                continue;
            }
            match kind {
                AnalysisKind::Silence => self.silence_segments = patch.silence_segments.clone(),
                AnalysisKind::Scenes => self.scene_changes = patch.scene_changes.clone(),
                AnalysisKind::Loudness => self.loudness = patch.loudness,
                AnalysisKind::Rhythm => {
                    self.onsets = patch.onsets.clone();
                    self.tempo = patch.tempo.clone();
                    self.audio_class = patch.audio_class;
                    self.music = patch.music.clone();
                }
                AnalysisKind::Transcript => self.transcript = patch.transcript.clone(),
            }
            // The legacy kinds an old analysis counted by their data are recorded now,
            // so a step that finds nothing this time does not make it look unrun.
            if !self.ran.contains(&kind) {
                self.ran.push(kind);
            }
        }
        self.ran.sort();
    }
}
