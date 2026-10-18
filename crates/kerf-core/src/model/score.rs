//! A **music score**: the structured description of a piece an agent or a user writes and Kerf
//! renders through a SoundFont synthesizer ([`crate::engine::compose`]). Pure and unit-tested.
//!
//! A score is a tempo, a key, a [`MusicStyle`] and a list of sections (intro, verse, build, drop,
//! breakdown, outro) of whole bars with one chord per bar, plus timed [`ScoreHit`]s (an impact, a
//! riser, a dropout, a whoosh) for moments in the picture. 4/4 only. [`expand`] turns it into
//! note events for five roles with section-aware density; [`plan_score_to_picture`] nudges the
//! tempo and adds hits so the music lands on the picture's cuts. Everything is deterministic: the
//! same score is the same notes, down to the humanization (a small seeded generator).
//!
//! The score rides on the asset it produced (`Asset.music_score`), which is what lets it be
//! edited and re-composed, and what the asset's tempo / bar analysis is written from directly:
//! the grid is exact because the audio was made on it.

mod align;
mod arrange;
mod chord;

use serde::{Deserialize, Serialize};

pub use align::{plan_score_to_picture, AlignOptions, AlignReport, BeatReading, CutAlignment, DEFAULT_MAX_TEMPO_CHANGE};
pub use arrange::{expand, Arrangement, Makeup, Mix, NoteEvent, Role, Sidechain, ROLES};
pub use chord::{Chord, Colour, Key, Mode, Quality};

use super::{AudioClass, AudioClassification, BeatGrid, MusicAnalysis, PhraseMatch, Rhythm, Tempo};
use crate::error::{Error, Result};

/// The slowest and fastest tempo a score may have, beats per minute.
pub const MIN_BPM: f64 = 40.0;
pub const MAX_BPM: f64 = 240.0;
/// Limits that keep a score renderable in memory and a prompt sane.
pub const MAX_SECTIONS: usize = 100;
pub const MAX_SECTION_BARS: u32 = 128;
pub const MAX_SCORE_BARS: u32 = 400;
/// The longest piece, seconds: ten minutes (the render holds the whole stereo mix in memory, 230 MB).
pub const MAX_SCORE_SECONDS: f64 = 600.0;
pub const MAX_HITS: usize = 256;
pub const MAX_CHORDS_PER_SECTION: usize = 64;

/// How long the sound rings on after the last bar, seconds (shortened at fast tempos so it is
/// never a whole extra bar): the final chord's release and the reverb. A score lasts
/// [`duration_s`] — its bars plus this.
pub const RELEASE_TAIL_S: f64 = 1.5;

/// The sound of a piece: GM programs per role, the drum pattern, the bass and comping rules,
/// swing, role gains and how hard the kick pumps the bass and pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MusicStyle {
    /// Four-on-the-floor, marimba stabs, warm pad, pan-flute hook, pumping bass (100-120 BPM).
    TropicalHouse,
    /// Backbeat, fingered bass, piano, strings (100-124 BPM).
    Pop,
    /// Lazy swung boom-bap, Rhodes sevenths, vibraphone (70-90 BPM).
    Lofi,
    /// Strings, horns, timpani and big hits; builds and drops are swells (60-100 BPM).
    Cinematic,
    /// A wash of pad and harp with no real beat (55-80 BPM).
    Ambient,
    /// Power-chord guitar, picked bass, live drums (100-150 BPM).
    Rock,
}

impl MusicStyle {
    pub const ALL: [MusicStyle; 6] = [
        MusicStyle::TropicalHouse,
        MusicStyle::Pop,
        MusicStyle::Lofi,
        MusicStyle::Cinematic,
        MusicStyle::Ambient,
        MusicStyle::Rock,
    ];

    /// The snake_case name used in JSON.
    pub fn name(self) -> &'static str {
        match self {
            MusicStyle::TropicalHouse => "tropical_house",
            MusicStyle::Pop => "pop",
            MusicStyle::Lofi => "lofi",
            MusicStyle::Cinematic => "cinematic",
            MusicStyle::Ambient => "ambient",
            MusicStyle::Rock => "rock",
        }
    }

    /// A name for a person.
    pub fn label(self) -> &'static str {
        match self {
            MusicStyle::TropicalHouse => "Tropical house",
            MusicStyle::Pop => "Pop",
            MusicStyle::Lofi => "Lo-fi",
            MusicStyle::Cinematic => "Cinematic",
            MusicStyle::Ambient => "Ambient",
            MusicStyle::Rock => "Rock",
        }
    }

    /// A tempo that suits the style (what a caller that has no opinion starts from).
    pub fn default_bpm(self) -> f64 {
        match self {
            MusicStyle::TropicalHouse => 110.0,
            MusicStyle::Pop => 112.0,
            MusicStyle::Lofi => 80.0,
            MusicStyle::Cinematic => 76.0,
            MusicStyle::Ambient => 68.0,
            MusicStyle::Rock => 124.0,
        }
    }
}

/// What a section is for. It decides which instruments play and how dense they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    /// Sparse: pad and a hint of the groove, a teaser of the hook late in the section.
    Intro,
    /// The steady groove under the story.
    Verse,
    /// A rise into the next section: pulsing bass, a snare roll that quickens, a riser.
    Build,
    /// Everything at once: the full beat, bass, chords and the hook.
    Drop,
    /// The beat and bass fall away; pad, keys and hook carry it.
    Breakdown,
    /// The piece winds down and the last bar resolves on the tonic chord.
    Outro,
}

impl SectionKind {
    pub fn name(self) -> &'static str {
        match self {
            SectionKind::Intro => "intro",
            SectionKind::Verse => "verse",
            SectionKind::Build => "build",
            SectionKind::Drop => "drop",
            SectionKind::Breakdown => "breakdown",
            SectionKind::Outro => "outro",
        }
    }

    /// How loud and busy a section of this kind is when it states no `energy`.
    pub fn default_energy(self) -> f64 {
        match self {
            SectionKind::Intro => 0.3,
            SectionKind::Verse => 0.5,
            SectionKind::Build => 0.7,
            SectionKind::Drop => 1.0,
            SectionKind::Breakdown => 0.3,
            SectionKind::Outro => 0.3,
        }
    }
}

/// A run of whole bars with a chord per bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScoreSection {
    pub kind: SectionKind,
    /// Length in bars, at least 1.
    pub bars: u32,
    /// One chord per bar, as names (`C`, `Em`, `F#m7`, `Gsus4`, `D/F#`) or roman numerals in the
    /// score's key (`I`, `V`, `vi`, `IV`, `ii7`, `bVII`). A list shorter than `bars` cycles.
    pub chords: Vec<String>,
    /// 0..=1: how loud and busy the section is. Defaults by kind (intro 0.3, verse 0.5, build
    /// 0.7, drop 1.0, breakdown 0.3, outro 0.3); a verse given 0.9 plays with a drop's density.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy: Option<f64>,
}

/// A moment the picture asks of the music.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    /// A crash, a timpani on the tonic and a low boom at `at_s`.
    Impact,
    /// A rising sweep (about one bar) that peaks at `at_s`.
    Riser,
    /// Everything but the effects goes silent for one beat from `at_s`.
    Dropout,
    /// A short sweep (about a second) that peaks at `at_s`.
    Whoosh,
}

impl HitKind {
    pub fn name(self) -> &'static str {
        match self {
            HitKind::Impact => "impact",
            HitKind::Riser => "riser",
            HitKind::Dropout => "dropout",
            HitKind::Whoosh => "whoosh",
        }
    }
}

/// A sound effect placed at a time in the music.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScoreHit {
    /// Seconds from the start of the music.
    pub at_s: f64,
    pub kind: HitKind,
}

/// General MIDI programs (0..=127) that replace a style's instruments. `drums` is the drum kit's
/// program (0 standard, 8 room, 16 power, 24 electronic, 25 808/909, 32 jazz, 40 brush, 48
/// orchestral).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstrumentOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drums: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bass: Option<u8>,
    /// The sustained chords.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad: Option<u8>,
    /// The rhythmic chords (stabs, arpeggios, power chords).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<u8>,
    /// The hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<u8>,
}

impl InstrumentOverrides {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A piece of music, in 4/4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MusicScore {
    /// Beats per minute, 40..=240.
    pub bpm: f64,
    /// The key: a tonic and an optional `m` — `G`, `F#m`, `Bb`, `Ebm`.
    pub key: String,
    pub style: MusicStyle,
    pub sections: Vec<ScoreSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hits: Vec<ScoreHit>,
    /// Varies the humanization and the hook; the same score and seed are the same audio.
    #[serde(default)]
    pub seed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruments: Option<InstrumentOverrides>,
}

/// Where a section sits in the piece.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectionSpan {
    pub index: usize,
    pub kind: SectionKind,
    /// The bar the section starts at (0-based).
    pub start_bar: u32,
    pub bars: u32,
    pub start_s: f64,
    pub end_s: f64,
    /// The section's energy, stated or by kind.
    pub energy: f64,
}

/// A score checked and read: its key, the chord of every bar of every section, and the sections'
/// places in time.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub key: Key,
    /// `chords[section][bar]` **as played**: each section's list cycled to its length, with the
    /// last bar of an outro that ends the piece replaced by the tonic chord (it resolves) — so the
    /// notes, the analysis chroma and the bar before's approach note all agree.
    pub chords: Vec<Vec<Chord>>,
    pub spans: Vec<SectionSpan>,
    pub total_bars: u32,
}

fn invalid(msg: String) -> Error {
    Error::InvalidArgument(msg)
}

/// Seconds in one bar at `bpm`.
pub fn bar_seconds(bpm: f64) -> f64 {
    4.0 * 60.0 / bpm
}

impl MusicScore {
    /// A short title for the asset: `Lo-fi · Am · 82 BPM`.
    pub fn title(&self) -> String {
        format!("{} · {} · {} BPM", self.style.label(), self.key.trim(), self.bpm.round())
    }

    /// Seconds in one bar.
    pub fn bar_s(&self) -> f64 {
        bar_seconds(self.bpm)
    }

    /// Bars in all.
    pub fn total_bars(&self) -> u32 {
        self.sections.iter().map(|s| s.bars).sum()
    }

    /// The render's length: every bar plus the release tail ([`RELEASE_TAIL_S`], at most 0.9 of
    /// a bar). Valid for any bpm; the sections are not checked.
    pub fn duration_s(&self) -> f64 {
        duration_s(self)
    }

    /// Check the score and read it: the key, a chord per bar, the sections in time. The error
    /// names the offending section or chord.
    pub fn resolve(&self) -> Result<Resolved> {
        if !self.bpm.is_finite() || !(MIN_BPM..=MAX_BPM).contains(&self.bpm) {
            return Err(invalid(format!("bpm must be {MIN_BPM}..={MAX_BPM} (got {})", self.bpm)));
        }
        let key = Key::parse(&self.key).map_err(invalid)?;
        if self.sections.is_empty() {
            return Err(invalid("the score needs at least one section".to_string()));
        }
        if self.sections.len() > MAX_SECTIONS {
            return Err(invalid(format!("a score has at most {MAX_SECTIONS} sections")));
        }
        let bar_s = self.bar_s();
        let colour = arrange::colour_of(self.style);
        let mut chords = Vec::with_capacity(self.sections.len());
        let mut spans = Vec::with_capacity(self.sections.len());
        let mut bar = 0u32;
        for (i, section) in self.sections.iter().enumerate() {
            let at = format!("sections[{i}] ({})", section.kind.name());
            if section.bars == 0 || section.bars > MAX_SECTION_BARS {
                return Err(invalid(format!(
                    "{at}: bars must be 1..={MAX_SECTION_BARS} (got {})",
                    section.bars
                )));
            }
            if let Some(e) = section.energy {
                if !e.is_finite() || !(0.0..=1.0).contains(&e) {
                    return Err(invalid(format!("{at}: energy must be 0..=1 (got {e})")));
                }
            }
            if section.chords.is_empty() {
                return Err(invalid(format!("{at}: needs at least one chord")));
            }
            if section.chords.len() > MAX_CHORDS_PER_SECTION {
                return Err(invalid(format!("{at}: at most {MAX_CHORDS_PER_SECTION} chords")));
            }
            let parsed = section
                .chords
                .iter()
                .map(|c| Chord::parse(c, key).map(|c| c.coloured(colour)))
                .collect::<std::result::Result<Vec<_>, String>>()
                .map_err(|e| invalid(format!("{at}: {e}")))?;
            chords.push((0..section.bars as usize).map(|b| parsed[b % parsed.len()]).collect());
            spans.push(SectionSpan {
                index: i,
                kind: section.kind,
                start_bar: bar,
                bars: section.bars,
                start_s: bar as f64 * bar_s,
                end_s: (bar + section.bars) as f64 * bar_s,
                energy: section.energy.unwrap_or_else(|| section.kind.default_energy()),
            });
            bar += section.bars;
            if bar > MAX_SCORE_BARS {
                return Err(invalid(format!("a score has at most {MAX_SCORE_BARS} bars")));
            }
        }
        // A piece that ends on an outro ends on the tonic chord, whatever the last bar was written as.
        if self.sections.last().is_some_and(|s| s.kind == SectionKind::Outro) {
            if let Some(last) = chords.last_mut().and_then(|c: &mut Vec<Chord>| c.last_mut()) {
                *last = key.tonic_chord().coloured(colour);
            }
        }
        let duration = duration_s(self);
        if duration > MAX_SCORE_SECONDS {
            return Err(invalid(format!(
                "the score lasts {duration:.0} s; the longest is {MAX_SCORE_SECONDS:.0} s"
            )));
        }
        if self.hits.len() > MAX_HITS {
            return Err(invalid(format!("a score has at most {MAX_HITS} hits")));
        }
        for (i, hit) in self.hits.iter().enumerate() {
            if !hit.at_s.is_finite() || hit.at_s < 0.0 || hit.at_s > duration {
                return Err(invalid(format!(
                    "hits[{i}] ({}): at_s must be 0..={duration:.2} seconds (got {})",
                    hit.kind.name(),
                    hit.at_s
                )));
            }
        }
        if let Some(i) = &self.instruments {
            for (role, program) in [
                ("drums", i.drums),
                ("bass", i.bass),
                ("pad", i.pad),
                ("keys", i.keys),
                ("lead", i.lead),
            ] {
                if program.is_some_and(|p| p > 127) {
                    return Err(invalid(format!("instruments.{role}: a General MIDI program is 0..=127")));
                }
            }
        }
        Ok(Resolved {
            key,
            chords,
            spans,
            total_bars: bar,
        })
    }

    /// Whether the score is one the renderer accepts.
    pub fn validate(&self) -> Result<()> {
        self.resolve().map(|_| ())
    }

    /// The sections in time. Errors as [`MusicScore::resolve`].
    pub fn section_spans(&self) -> Result<Vec<SectionSpan>> {
        Ok(self.resolve()?.spans)
    }

    /// The beat grid the audio is made on: beat 0 at 0 s, a bar every four beats. Exact.
    pub fn beat_grid(&self) -> BeatGrid {
        BeatGrid {
            period_s: 60.0 / self.bpm,
            phase_s: 0.0,
            downbeat_offset: 0,
            beats_per_bar: 4,
        }
    }

    /// Every bar start, seconds.
    pub fn downbeats(&self) -> Vec<f64> {
        let bar_s = self.bar_s();
        (0..self.total_bars()).map(|b| b as f64 * bar_s).collect()
    }

    /// Every beat, seconds.
    pub fn beats(&self) -> Vec<f64> {
        let beat = 60.0 / self.bpm;
        (0..self.total_bars() * 4).map(|b| b as f64 * beat).collect()
    }

    /// The bar structure of the audio, written from the score instead of measured: the exact
    /// grid, each bar's chroma from its chord, and the phrases that repeat. `duration` is the
    /// rendered file's length (a bar beyond the score is silent).
    pub fn music_analysis(&self, duration: f64) -> Result<MusicAnalysis> {
        let resolved = self.resolve()?;
        let grid = self.beat_grid();
        let bars = grid.whole_bars(duration);
        let mut bar_chroma: Vec<[f32; 12]> = Vec::with_capacity(bars);
        for chords in &resolved.chords {
            for chord in chords {
                let mut chroma = [0.0_f32; 12];
                for (k, interval) in chord.intervals().iter().enumerate() {
                    let weight = if k == 0 { 1.5 } else { 1.0 };
                    chroma[((chord.root + interval) % 12) as usize] += weight;
                }
                chroma[chord.bass as usize] += 0.5;
                let norm = chroma.iter().map(|v| v * v).sum::<f32>().sqrt();
                chroma.iter_mut().for_each(|v| *v /= norm);
                bar_chroma.push(chroma);
            }
        }
        bar_chroma.resize(bars, [0.0; 12]);
        bar_chroma.truncate(bars);
        let phrases: Vec<PhraseMatch> =
            crate::engine::music::phrase_matches(&bar_chroma, crate::engine::music::SPLICE_SIMILARITY);
        Ok(MusicAnalysis {
            grid,
            duration,
            bar_chroma,
            phrases,
        })
    }

    /// What the rhythm analysis step finds, written from the score: the beats and bars as a
    /// full-confidence tempo, the bar structure, the kick and hit times as onsets, and the
    /// class (music).
    pub fn rhythm(&self, duration: f64) -> Result<Rhythm> {
        let music = self.music_analysis(duration)?;
        let arrangement = expand(self)?;
        let mut onsets: Vec<f64> = arrangement
            .kicks
            .iter()
            .copied()
            .chain(self.hits.iter().map(|h| h.at_s))
            .chain(self.downbeats())
            .filter(|t| *t < duration)
            .collect();
        onsets.sort_by(f64::total_cmp);
        onsets.dedup_by(|a, b| (*a - *b).abs() < 0.02);
        Ok(Rhythm {
            onsets,
            tempo: Some(Tempo::from_grid(&music.grid, duration, 1.0)),
            audio_class: Some(AudioClassification {
                class: AudioClass::Music,
                confidence: 1.0,
            }),
            music: Some(music),
        })
    }

    /// The same score with `seed` replaced.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Whether any instrument is overridden.
    pub fn has_overrides(&self) -> bool {
        self.instruments.as_ref().is_some_and(|i| !i.is_empty())
    }
}

/// The length of the render of `score`: its bars plus the release tail.
pub fn duration_s(score: &MusicScore) -> f64 {
    let bar_s = score.bar_s();
    score.total_bars() as f64 * bar_s + RELEASE_TAIL_S.min(0.9 * bar_s)
}

/// A song that uses every kind of section, for tests and calibration.
#[cfg(test)]
pub(crate) use arrange::style_shares;

#[cfg(test)]
pub(crate) fn sample_song(style: MusicStyle) -> MusicScore {
    tests::song(style)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(in crate::model::score) fn section(kind: SectionKind, bars: u32, chords: &[&str]) -> ScoreSection {
        ScoreSection {
            kind,
            bars,
            chords: chords.iter().map(|c| c.to_string()).collect(),
            energy: None,
        }
    }

    /// A small song that uses every kind of section.
    pub(in crate::model::score) fn song(style: MusicStyle) -> MusicScore {
        MusicScore {
            bpm: 120.0,
            key: "Am".to_string(),
            style,
            sections: vec![
                section(SectionKind::Intro, 4, &["i", "VI"]),
                section(SectionKind::Verse, 8, &["i", "VI", "III", "VII"]),
                section(SectionKind::Build, 4, &["iv", "iv", "V", "V"]),
                section(SectionKind::Drop, 8, &["i", "VI", "III", "VII"]),
                section(SectionKind::Breakdown, 4, &["VI", "VII"]),
                section(SectionKind::Drop, 8, &["i", "VI", "III", "VII"]),
                section(SectionKind::Outro, 4, &["i", "VI", "iv", "V"]),
            ],
            hits: Vec::new(),
            seed: 7,
            instruments: None,
        }
    }

    #[test]
    fn a_valid_score_resolves_a_chord_per_bar_and_the_sections_in_time() {
        let score = song(MusicStyle::Pop);
        let r = score.resolve().unwrap();
        assert_eq!(r.total_bars, 40);
        assert_eq!(r.chords[0].len(), 4);
        // The two-chord list cycles over the four bars.
        assert_eq!(r.chords[0][2], r.chords[0][0]);
        assert_eq!(r.chords[0][0].root, 9);
        assert_eq!(r.chords[0][1].root, 5);
        assert_eq!(r.spans[1].start_bar, 4);
        assert!((r.spans[1].start_s - 8.0).abs() < 1e-9);
        assert_eq!(r.spans[3].energy, 1.0);
        assert_eq!(r.spans[0].energy, 0.3);
    }

    #[test]
    fn invalid_scores_name_the_section_or_chord_at_fault() {
        let mut s = song(MusicStyle::Pop);
        s.sections[1].chords[2] = "Hm".to_string();
        let e = s.validate().unwrap_err().to_string();
        assert!(e.contains("sections[1] (verse)") && e.contains("\"Hm\""), "{e}");

        let mut s = song(MusicStyle::Pop);
        s.sections[2].bars = 0;
        assert!(s.validate().unwrap_err().to_string().contains("sections[2] (build): bars"));

        let mut s = song(MusicStyle::Pop);
        s.sections[0].chords.clear();
        assert!(s
            .validate()
            .unwrap_err()
            .to_string()
            .contains("sections[0] (intro): needs at least one chord"));

        let mut s = song(MusicStyle::Pop);
        s.sections[3].energy = Some(1.5);
        assert!(s.validate().unwrap_err().to_string().contains("energy"));

        let mut s = song(MusicStyle::Pop);
        s.bpm = 300.0;
        assert!(s.validate().unwrap_err().to_string().contains("bpm"));
        s.bpm = f64::NAN;
        assert!(s.validate().is_err());

        let mut s = song(MusicStyle::Pop);
        s.key = "Z".to_string();
        assert!(s.validate().unwrap_err().to_string().contains("not a key"));

        let mut s = song(MusicStyle::Pop);
        s.sections.clear();
        assert!(s.validate().is_err());

        let mut s = song(MusicStyle::Pop);
        s.hits.push(ScoreHit {
            at_s: 9999.0,
            kind: HitKind::Impact,
        });
        assert!(s.validate().unwrap_err().to_string().contains("hits[0] (impact)"));

        let mut s = song(MusicStyle::Pop);
        s.instruments = Some(InstrumentOverrides {
            lead: Some(200),
            ..Default::default()
        });
        assert!(s.validate().unwrap_err().to_string().contains("instruments.lead"));
    }

    #[test]
    fn a_score_cannot_outgrow_the_renderer() {
        let mut s = song(MusicStyle::Pop);
        s.bpm = 40.0;
        s.sections = vec![section(SectionKind::Drop, 128, &["C"]); 3];
        let e = s.validate().unwrap_err().to_string();
        assert!(e.contains("longest"), "{e}");
    }

    #[test]
    fn duration_is_the_bars_plus_a_tail_shorter_than_a_bar() {
        let s = song(MusicStyle::Pop);
        assert!((s.duration_s() - (40.0 * 2.0 + RELEASE_TAIL_S)).abs() < 1e-9);
        // At 240 BPM a bar is 1 s: the tail is 0.9 of it, never a whole extra bar.
        let mut fast = song(MusicStyle::Pop);
        fast.bpm = 240.0;
        assert!((fast.duration_s() - (40.0 * 1.0 + 0.9)).abs() < 1e-9);
        assert_eq!(fast.beat_grid().whole_bars(fast.duration_s()), 40);
        assert_eq!(s.beat_grid().whole_bars(s.duration_s()), 40);
    }

    #[test]
    fn the_grid_and_downbeats_come_straight_from_the_score() {
        let s = song(MusicStyle::Pop);
        let g = s.beat_grid();
        assert_eq!(g.bpm(), 120.0);
        assert_eq!(g.first_downbeat(), 0.0);
        let d = s.downbeats();
        assert_eq!(d.len(), 40);
        assert_eq!(d[3], 6.0);
        assert_eq!(s.beats().len(), 160);
        assert_eq!(s.beats()[5], 2.5);
    }

    #[test]
    fn the_analysis_is_written_from_the_score() {
        let s = song(MusicStyle::Pop);
        let duration = s.duration_s();
        let m = s.music_analysis(duration).unwrap();
        assert_eq!(m.bar_chroma.len(), 40);
        // Bar 0 is Am: A, C and E carry the chroma.
        assert!(m.bar_chroma[0][9] > 0.4 && m.bar_chroma[0][0] > 0.3 && m.bar_chroma[0][4] > 0.3);
        assert!(m.bar_chroma[0][6] == 0.0);
        // The outro's last bar is written as V (E major, with a G sharp) but plays the tonic, and so
        // does the chroma the analysis is written from.
        let last = m.bar_chroma[39];
        assert!(last[9] > 0.4 && last[0] > 0.3 && last[4] > 0.3 && last[8] == 0.0, "{last:?}");
        assert_eq!(s.resolve().unwrap().chords[6][3].root, 9);
        let norm: f32 = m.bar_chroma[0].iter().map(|v| v * v).sum();
        assert!((norm - 1.0).abs() < 1e-5);
        // The verse and the first drop repeat the same four chords: a phrase match.
        assert!(
            m.phrases.iter().any(|p| p.a == 4 && p.b == 16 && p.bars >= 4),
            "{:?}",
            m.phrases
        );

        let r = s.rhythm(duration).unwrap();
        let t = r.tempo.unwrap();
        assert_eq!(t.bpm, 120.0);
        assert!(t.confidence >= 1.0 - 1e-9);
        // Every bar, and the one that would follow the last (the tail runs into it).
        assert_eq!(t.downbeats.len(), 41);
        assert_eq!(t.downbeats[39], 78.0);
        assert_eq!(r.audio_class.unwrap().class, AudioClass::Music);
        assert!(!r.onsets.is_empty());
        assert!(r.onsets.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn a_score_round_trips_through_json_in_snake_case() {
        let mut s = song(MusicStyle::TropicalHouse);
        s.hits.push(ScoreHit {
            at_s: 8.0,
            kind: HitKind::Riser,
        });
        s.instruments = Some(InstrumentOverrides {
            lead: Some(73),
            ..Default::default()
        });
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"tropical_house\"") && json.contains("\"riser\"") && json.contains("\"at_s\""));
        assert!(!json.contains("energy"), "unset energy is omitted: {json}");
        let back: MusicScore = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        // A minimal score from an agent: no hits, no seed.
        let min = r#"{"bpm":100,"key":"G","style":"pop","sections":[{"kind":"verse","bars":2,"chords":["I","V"]}]}"#;
        let m: MusicScore = serde_json::from_str(min).unwrap();
        assert_eq!(m.seed, 0);
        assert!(m.hits.is_empty() && m.instruments.is_none());
        m.validate().unwrap();
    }

    #[test]
    fn every_style_has_a_name_and_a_tempo_in_range() {
        for style in MusicStyle::ALL {
            assert!((MIN_BPM..=MAX_BPM).contains(&style.default_bpm()));
            let json = serde_json::to_string(&style).unwrap();
            assert_eq!(json, format!("\"{}\"", style.name()));
        }
    }
}
