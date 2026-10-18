//! Score to picture: make the music land on the cuts. Pure and deterministic.
//!
//! [`plan_score_to_picture`] takes the times of a picture's cuts (or markers), where the music
//! starts on the timeline, and a score; it may nudge the tempo a few percent so more cuts fall on
//! beats (a cut on a downbeat counts double), and it adds the hits that make a cut feel
//! intended: an **impact** on every cut that lands on a downbeat, and a **dropout** (a beat of
//! silence) and a **riser** before a drop that a cut lands on. It returns the new score and a
//! report of which cuts sat on beats and downbeats before and after.

use serde::{Deserialize, Serialize};

use super::{bar_seconds, HitKind, MusicScore, ScoreHit, SectionKind, MAX_BPM, MIN_BPM};
use crate::error::Result;

/// How far the tempo may move by default, as a fraction (6 %).
pub const DEFAULT_MAX_TEMPO_CHANGE: f64 = 0.06;

/// A cut counts as on the beat within this many seconds of it.
pub const ON_BEAT_TOLERANCE_S: f64 = 0.04;

/// The tempo search steps this finely, as a fraction of the tempo (0.05 %: a cut ten seconds in
/// moves 5 ms per step).
const SEARCH_STEP: f64 = 0.0005;

/// Hits closer than this to one already in the score are not added again.
const DUPLICATE_S: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlignOptions {
    /// The most the tempo may change, as a fraction (0.06 = ±6 %); 0 leaves it alone.
    pub max_tempo_change: f64,
    /// A cut within this many seconds of a beat is on it.
    pub tolerance_s: f64,
    /// Add the impacts, dropouts and risers (otherwise only the tempo is tuned).
    pub add_hits: bool,
}

impl Default for AlignOptions {
    fn default() -> Self {
        Self {
            max_tempo_change: DEFAULT_MAX_TEMPO_CHANGE,
            tolerance_s: ON_BEAT_TOLERANCE_S,
            add_hits: true,
        }
    }
}

/// Where one cut sits against the beat grid.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BeatReading {
    pub on_beat: bool,
    pub on_downbeat: bool,
    /// Seconds from the nearest beat (positive: the cut is late).
    pub offset_s: f64,
}

/// One cut, before and after.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CutAlignment {
    /// The time as given (on the timeline).
    pub cut_s: f64,
    /// The same moment in the music's own time.
    pub music_s: f64,
    pub before: BeatReading,
    pub after: BeatReading,
}

/// What [`plan_score_to_picture`] did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlignReport {
    /// The cuts that fall inside the music.
    pub cuts: usize,
    pub bpm_before: f64,
    pub bpm_after: f64,
    /// `bpm_after / bpm_before - 1`.
    pub tempo_change: f64,
    pub on_beat_before: usize,
    pub on_downbeat_before: usize,
    pub on_beat_after: usize,
    pub on_downbeat_after: usize,
    pub hits_added: Vec<ScoreHit>,
    pub details: Vec<CutAlignment>,
    pub notes: Vec<String>,
}

fn read(music_s: f64, bpm: f64, tolerance: f64) -> BeatReading {
    let period = 60.0 / bpm;
    let k = (music_s / period).round();
    let offset = music_s - k * period;
    let on_beat = offset.abs() <= tolerance;
    BeatReading {
        on_beat,
        on_downbeat: on_beat && (k as i64).rem_euclid(4) == 0,
        offset_s: offset,
    }
}

/// How well `bpm` fits the cuts: the weighted count of cuts on beats (a downbeat counts 2) and
/// the total distance of the cuts from their beats (the tie-break).
fn fit(cuts: &[f64], bpm: f64, tolerance: f64) -> (u32, f64) {
    let mut weight = 0;
    let mut distance = 0.0;
    for c in cuts {
        let r = read(*c, bpm, tolerance);
        weight += if r.on_downbeat {
            2
        } else if r.on_beat {
            1
        } else {
            0
        };
        distance += r.offset_s.abs();
    }
    (weight, distance)
}

/// Fit `score` to a picture.
///
/// `cut_times_s` are the cuts on the timeline and `music_start_s` where the music starts there.
/// The tempo is searched in steps of 0.05 % within `±max_tempo_change` for the one that puts the
/// most cuts on beats; it only changes when that beats the tempo the score has, and among the
/// equally good tempos in the nearest run to the original it takes the one that centres the cuts
/// on the grid. Cuts outside the music are ignored.
pub fn plan_score_to_picture(
    score: &MusicScore,
    cut_times_s: &[f64],
    music_start_s: f64,
    opts: &AlignOptions,
) -> Result<(MusicScore, AlignReport)> {
    let resolved = score.resolve()?;
    let duration = score.duration_s();
    let mut cuts: Vec<(f64, f64)> = cut_times_s
        .iter()
        .copied()
        .filter(|t| t.is_finite())
        .map(|t| (t, t - music_start_s))
        .filter(|(_, m)| *m >= 0.0 && *m <= duration)
        .collect();
    cuts.sort_by(|a, b| a.1.total_cmp(&b.1));
    cuts.dedup_by(|a, b| (a.1 - b.1).abs() < 1e-6);
    let music: Vec<f64> = cuts.iter().map(|c| c.1).collect();
    let tol = opts.tolerance_s.max(0.0);
    let mut notes = Vec::new();

    let bpm_before = score.bpm;
    let mut bpm_after = bpm_before;
    let max = opts.max_tempo_change.clamp(0.0, 0.5);
    if max > 0.0 && !music.is_empty() {
        let steps = (max / SEARCH_STEP).round() as i64;
        let candidates: Vec<(f64, u32, f64)> = (-steps..=steps)
            .map(|k| bpm_before * (1.0 + k as f64 * SEARCH_STEP))
            .map(|bpm| {
                let (w, d) = fit(&music, bpm, tol);
                (bpm, w, d)
            })
            .collect();
        let valid = |bpm: f64| (MIN_BPM..=MAX_BPM).contains(&bpm);
        let baseline = fit(&music, bpm_before, tol).0;
        let best = candidates
            .iter()
            .filter(|c| valid(c.0))
            .map(|c| c.1)
            .max()
            .unwrap_or(baseline);
        if best > baseline {
            // The best-scoring tempo nearest the original, then its whole run of equally good
            // neighbours, then the one in it that centres the cuts.
            let nearest = candidates
                .iter()
                .enumerate()
                .filter(|(_, c)| valid(c.0) && c.1 == best)
                .min_by(|(_, a), (_, b)| {
                    (a.0 - bpm_before)
                        .abs()
                        .total_cmp(&(b.0 - bpm_before).abs())
                        .then(a.0.total_cmp(&b.0))
                })
                .map(|(i, _)| i);
            if let Some(i) = nearest {
                let (mut lo, mut hi) = (i, i);
                while lo > 0 && candidates[lo - 1].1 == best && valid(candidates[lo - 1].0) {
                    lo -= 1;
                }
                while hi + 1 < candidates.len() && candidates[hi + 1].1 == best && valid(candidates[hi + 1].0) {
                    hi += 1;
                }
                let pick = candidates[lo..=hi]
                    .iter()
                    .min_by(|a, b| {
                        a.2.total_cmp(&b.2)
                            .then((a.0 - bpm_before).abs().total_cmp(&(b.0 - bpm_before).abs()))
                    })
                    .map(|c| c.0)
                    .unwrap_or(bpm_before);
                bpm_after = pick;
            }
        }
        if (bpm_after - bpm_before).abs() < 1e-12 && best <= baseline {
            notes.push("the tempo already fits the cuts as well as any within the limit".to_string());
        }
    } else if music.is_empty() {
        notes.push("no cut falls inside the music: nothing to align".to_string());
    }

    let mut aligned = score.clone();
    aligned.bpm = bpm_after;
    let period = 60.0 / bpm_after;

    let mut hits_added = Vec::new();
    if opts.add_hits {
        let existing =
            |hits: &[ScoreHit], kind: HitKind, at: f64| hits.iter().any(|h| h.kind == kind && (h.at_s - at).abs() < DUPLICATE_S);
        let new_spans = aligned.section_spans()?;
        let total = aligned.duration_s();
        // An impact on every cut that sits on a downbeat.
        for m in &music {
            let r = read(*m, bpm_after, tol);
            if r.on_downbeat {
                let at = (m / (4.0 * period)).round() * 4.0 * period;
                if at <= total && !existing(&aligned.hits, HitKind::Impact, at) {
                    let hit = ScoreHit {
                        at_s: at,
                        kind: HitKind::Impact,
                    };
                    aligned.hits.push(hit);
                    hits_added.push(hit);
                }
            }
        }
        // Before a drop that a cut lands on: a beat of silence, and a riser into it.
        for (i, span) in new_spans.iter().enumerate() {
            if span.kind != SectionKind::Drop || span.start_s <= period {
                continue;
            }
            if !music.iter().any(|m| (m - span.start_s).abs() <= period) {
                continue;
            }
            let after_build = i > 0 && new_spans[i - 1].kind == SectionKind::Build;
            let dropout = ScoreHit {
                at_s: span.start_s - period,
                kind: HitKind::Dropout,
            };
            if !existing(&aligned.hits, HitKind::Dropout, dropout.at_s) {
                aligned.hits.push(dropout);
                hits_added.push(dropout);
            }
            // A build already ends in a riser of its own.
            let riser = ScoreHit {
                at_s: span.start_s,
                kind: HitKind::Riser,
            };
            if !after_build
                && !existing(&aligned.hits, HitKind::Riser, riser.at_s)
                && span.start_s >= bar_seconds(bpm_after) * 0.5
            {
                aligned.hits.push(riser);
                hits_added.push(riser);
            }
        }
        aligned.hits.sort_by(|a, b| a.at_s.total_cmp(&b.at_s));
    }
    let _ = resolved;

    let details: Vec<CutAlignment> = cuts
        .iter()
        .map(|(cut, m)| CutAlignment {
            cut_s: *cut,
            music_s: *m,
            before: read(*m, bpm_before, tol),
            after: read(*m, bpm_after, tol),
        })
        .collect();
    let count = |f: &dyn Fn(&CutAlignment) -> bool| details.iter().filter(|d| f(d)).count();
    let report = AlignReport {
        cuts: details.len(),
        bpm_before,
        bpm_after,
        tempo_change: bpm_after / bpm_before - 1.0,
        on_beat_before: count(&|d| d.before.on_beat),
        on_downbeat_before: count(&|d| d.before.on_downbeat),
        on_beat_after: count(&|d| d.after.on_beat),
        on_downbeat_after: count(&|d| d.after.on_downbeat),
        hits_added,
        details,
        notes,
    };
    Ok((aligned, report))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{section, song};
    use super::super::MusicStyle;
    use super::*;

    #[test]
    fn a_picture_already_on_the_grid_leaves_the_tempo_alone() {
        let score = song(MusicStyle::Pop);
        // 120 BPM: a bar is 2 s. Cuts at 4, 8 and 12 s are downbeats.
        let (aligned, report) = plan_score_to_picture(&score, &[4.0, 8.0, 12.0], 0.0, &AlignOptions::default()).unwrap();
        assert_eq!(aligned.bpm, 120.0);
        assert_eq!(report.cuts, 3);
        assert_eq!(report.on_downbeat_before, 3);
        assert_eq!(report.on_downbeat_after, 3);
        assert_eq!(report.tempo_change, 0.0);
        assert!(report.notes.iter().any(|n| n.contains("already fits")));
    }

    #[test]
    fn a_nudged_tempo_puts_more_cuts_on_beats() {
        let score = song(MusicStyle::Pop);
        // Cuts every 2.08 s (a bar at 115.4 BPM): 4% off the 2 s bars, inside the 6% limit.
        let cuts: Vec<f64> = (1..=8).map(|k| 2.08 * k as f64).collect();
        let (aligned, report) = plan_score_to_picture(&score, &cuts, 0.0, &AlignOptions::default()).unwrap();
        assert!(report.on_beat_before < report.on_beat_after, "{report:?}");
        assert_eq!(report.on_downbeat_after, 8, "{report:?}");
        assert!((aligned.bpm - 240.0 / 2.08).abs() < 0.2, "bpm {}", aligned.bpm);
        assert!(report.tempo_change < 0.0 && report.tempo_change > -0.06);
        // The cuts sit on the bars to within a few ms, not at the edge of the tolerance.
        for d in &report.details {
            assert!(d.after.offset_s.abs() < 0.012, "{d:?}");
        }
        // An impact on every downbeat cut, on the grid.
        let impacts: Vec<f64> = aligned
            .hits
            .iter()
            .filter(|h| h.kind == HitKind::Impact)
            .map(|h| h.at_s)
            .collect();
        assert_eq!(impacts.len(), 8);
        let bar = 240.0 / aligned.bpm;
        for (k, t) in impacts.iter().enumerate() {
            assert!((t - bar * (k as f64 + 1.0) * 1.0).abs() < 0.03, "{t}");
        }
    }

    #[test]
    fn the_tempo_never_moves_further_than_allowed_or_out_of_range() {
        let score = song(MusicStyle::Pop);
        // 2.4 s apart is 20% slower than the bars: out of reach of a 6% nudge.
        let cuts: Vec<f64> = (1..=6).map(|k| 2.4 * k as f64).collect();
        let (aligned, _) = plan_score_to_picture(&score, &cuts, 0.0, &AlignOptions::default()).unwrap();
        assert!((aligned.bpm / 120.0 - 1.0).abs() <= 0.06 + 1e-9);
        let (same, _) = plan_score_to_picture(
            &score,
            &cuts,
            0.0,
            &AlignOptions {
                max_tempo_change: 0.0,
                ..AlignOptions::default()
            },
        )
        .unwrap();
        assert_eq!(same.bpm, 120.0);
        let mut slow = song(MusicStyle::Pop);
        slow.bpm = 41.0;
        let (a, _) = plan_score_to_picture(&slow, &[5.0, 10.0, 15.0], 0.0, &AlignOptions::default()).unwrap();
        assert!(a.bpm >= 40.0);
    }

    #[test]
    fn the_music_start_shifts_the_cuts_into_music_time() {
        let score = song(MusicStyle::Pop);
        // The music starts at 10 s on the timeline: the cuts at 14 and 18 are the music's 4 and 8.
        let (_, report) = plan_score_to_picture(&score, &[2.0, 14.0, 18.0, 9999.0], 10.0, &AlignOptions::default()).unwrap();
        assert_eq!(report.cuts, 2);
        assert_eq!(report.details[0].music_s, 4.0);
        assert_eq!(report.details[0].cut_s, 14.0);
        assert_eq!(report.on_downbeat_after, 2);
    }

    #[test]
    fn a_cut_on_a_drop_gets_a_dropout_and_a_riser_before_it() {
        let mut score = song(MusicStyle::Pop);
        // Intro 4 bars, then a drop at 8 s (4 bars of 2 s): no build before it.
        score.sections = vec![
            section(SectionKind::Intro, 4, &["Am"]),
            section(SectionKind::Drop, 4, &["Am"]),
        ];
        let (aligned, report) = plan_score_to_picture(&score, &[8.0], 0.0, &AlignOptions::default()).unwrap();
        let kinds: Vec<(HitKind, f64)> = aligned.hits.iter().map(|h| (h.kind, h.at_s)).collect();
        assert!(kinds.contains(&(HitKind::Impact, 8.0)), "{kinds:?}");
        assert!(kinds.contains(&(HitKind::Dropout, 7.5)), "{kinds:?}");
        assert!(kinds.contains(&(HitKind::Riser, 8.0)), "{kinds:?}");
        assert_eq!(report.hits_added.len(), 3);

        // After a build, the build's own riser is enough: no second one.
        score.sections = vec![
            section(SectionKind::Intro, 2, &["Am"]),
            section(SectionKind::Build, 2, &["Am"]),
            section(SectionKind::Drop, 4, &["Am"]),
        ];
        let (aligned, _) = plan_score_to_picture(&score, &[8.0], 0.0, &AlignOptions::default()).unwrap();
        assert!(aligned.hits.iter().any(|h| h.kind == HitKind::Dropout));
        assert!(!aligned.hits.iter().any(|h| h.kind == HitKind::Riser));

        // A cut far from any drop gets neither.
        let (aligned, _) = plan_score_to_picture(&score, &[2.0], 0.0, &AlignOptions::default()).unwrap();
        assert!(aligned.hits.iter().all(|h| h.kind == HitKind::Impact));
    }

    #[test]
    fn hits_are_not_added_twice_and_can_be_switched_off() {
        let score = song(MusicStyle::Pop);
        let (once, _) = plan_score_to_picture(&score, &[4.0], 0.0, &AlignOptions::default()).unwrap();
        let (twice, report) = plan_score_to_picture(&once, &[4.0], 0.0, &AlignOptions::default()).unwrap();
        assert_eq!(once.hits, twice.hits);
        assert!(report.hits_added.is_empty());
        let (bare, report) = plan_score_to_picture(
            &score,
            &[4.0],
            0.0,
            &AlignOptions {
                add_hits: false,
                ..AlignOptions::default()
            },
        )
        .unwrap();
        assert!(bare.hits.is_empty() && report.hits_added.is_empty());
    }

    #[test]
    fn no_cuts_is_a_quiet_no_op() {
        let score = song(MusicStyle::Pop);
        let (aligned, report) = plan_score_to_picture(&score, &[], 0.0, &AlignOptions::default()).unwrap();
        assert_eq!(aligned, score);
        assert_eq!(report.cuts, 0);
        assert!(!report.notes.is_empty());
    }

    #[test]
    fn the_search_is_deterministic() {
        let score = song(MusicStyle::Pop);
        let cuts = [1.93, 4.07, 6.2, 8.31, 10.5, 12.44];
        let a = plan_score_to_picture(&score, &cuts, 0.0, &AlignOptions::default()).unwrap();
        let b = plan_score_to_picture(&score, &cuts, 0.0, &AlignOptions::default()).unwrap();
        assert_eq!(a, b);
    }
}
