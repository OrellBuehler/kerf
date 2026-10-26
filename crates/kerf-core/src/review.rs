//! Craft defects in a cut — the mistakes an editor hears or sees on the first watch and an agent,
//! which never watches, cannot.
//!
//! [`crate::platform`] judges a cut against where it is going (length, frame shape); this judges
//! the editing itself: a clip that flashes for two frames, a cut that clips a word, sound slammed
//! into sound with no fade, the same shot resuming a moment later, captions left behind by a later
//! trim, black where the picture runs out. Pure: [`review`] reads a working timeline, its assets
//! and their cached analysis ([`Project::review_inputs`](crate::Project::review_inputs)) and says
//! what it found, each finding with the tool that fixes it.
//!
//! Every check is tuned to stay quiet on ordinary edits, so the thresholds are constants here, and
//! a check that needs analysis the footage does not have is reported as skipped rather than guessed.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::model::{AnalysisKind, Asset, AssetAnalysis, Clip, StreamKind, Timeline, TranscriptSegment};
use crate::platform::Severity;
use crate::transcript::words_of;

/// A visible clip shorter than this many frames at the export rate is a flash frame.
pub const FLASH_FRAMES: f64 = 3.0;
/// How far inside a measured word's bounds (seconds) a cut must fall to count as inside it —
/// recognizer timings are a few tens of milliseconds loose.
pub const WORD_TOLERANCE: f64 = 0.04;
/// The same for a word whose times were estimated (spread over its line by character share).
pub const ESTIMATED_WORD_TOLERANCE: f64 = 0.12;
/// A "word" longer than this (seconds) is a recognizer that stretched one over a pause, not a word
/// a cut can land inside.
pub const MAX_WORD_SECS: f64 = 1.5;
/// A clip edge this close (seconds) to a stretch of silence counts as silent.
pub const EDGE_SILENCE_MARGIN: f64 = 0.05;
/// Two clips of one shot whose source windows are at most this far apart (seconds) are a jump cut;
/// further apart, the picture has usually moved on enough to read as a new shot.
pub const JUMP_MAX_SOURCE_GAP: f64 = 8.0;
/// A change of scale this large (a fraction) across a cut is a punch-in, the deliberate cover for
/// a jump cut.
pub const PUNCH_IN_SCALE: f64 = 0.08;
/// How far (seconds) a caption may sit from the words it shows and still be over them — captions
/// hold half a second past a pause and the one-lane rule can push a line's start later.
pub const CAPTION_SLACK: f64 = 0.6;
/// How far past a clip's edge a caption cut short by that edge may find the rest of its words.
const CAPTION_REACH: f64 = 10.0;
/// Fewer than half of a caption's words spoken under it, and it is no longer over its words.
pub const CAPTION_MATCH: f64 = 0.5;
/// This many words in a row that captioning would cover and the captions do not is speech the
/// caption set never saw.
pub const UNCAPTIONED_MIN_WORDS: usize = 3;
/// Black shorter than this many frames is rounding, not a gap.
pub const MIN_GAP_FRAMES: f64 = 0.5;

/// One check [`review`] can run, and the `kind` of what it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// A visible clip only a few frames long.
    FlashFrame,
    /// A sounding clip's edge falls inside a word of its transcript.
    MidWordCut,
    /// Sound meets sound on one lane with no fade, crossfade or transition.
    HardAudioCut,
    /// One shot resumes a moment later on the same lane.
    JumpCut,
    /// Generated captions no longer sit over the words being spoken.
    StaleCaptions,
    /// No picture while the cut plays.
    BlackGap,
    /// A stabilized clip that plays and exports as shot: its camera was never measured, or the
    /// measuring failed.
    Unstabilized,
}

impl Check {
    pub const ALL: [Check; 7] = [
        Check::FlashFrame,
        Check::MidWordCut,
        Check::HardAudioCut,
        Check::JumpCut,
        Check::StaleCaptions,
        Check::BlackGap,
        Check::Unstabilized,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Check::FlashFrame => "flash_frame",
            Check::MidWordCut => "mid_word_cut",
            Check::HardAudioCut => "hard_audio_cut",
            Check::JumpCut => "jump_cut",
            Check::StaleCaptions => "stale_captions",
            Check::BlackGap => "black_gap",
            Check::Unstabilized => "unstabilized",
        }
    }

    pub fn parse(name: &str) -> Option<Check> {
        Check::ALL.into_iter().find(|c| c.name() == name.trim())
    }

    /// The checks `names` asks for, in [`Check::ALL`] order; none named is every check. An unknown
    /// name is an [`Error::InvalidArgument`] listing the known ones.
    pub fn parse_list<S: AsRef<str>>(names: &[S]) -> Result<Vec<Check>> {
        if names.is_empty() {
            return Ok(Check::ALL.to_vec());
        }
        let mut wanted = HashSet::new();
        for name in names {
            let name = name.as_ref();
            wanted.insert(Check::parse(name).ok_or_else(|| {
                Error::InvalidArgument(format!(
                    "unknown check `{name}`; the checks are {}",
                    Check::ALL.map(Check::name).join(", ")
                ))
            })?);
        }
        Ok(Check::ALL.into_iter().filter(|c| wanted.contains(c)).collect())
    }
}

/// One defect, phrased for a person, placed on the timeline and paired with its fix.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewIssue {
    pub kind: Check,
    pub severity: Severity,
    /// Timeline seconds where it is.
    pub time: f64,
    /// Where it ends, for a span.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    /// The clips involved (a picture and its linked sound report once, with both).
    pub clip_ids: Vec<Uuid>,
    pub message: String,
    /// The Kerf tools that address it.
    pub fix: String,
}

/// A check that could not run, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkippedCheck {
    pub check: Check,
    pub reason: String,
}

/// What [`review`] found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CutReview {
    /// In timeline order.
    pub issues: Vec<ReviewIssue>,
    pub counts_by_kind: BTreeMap<Check, usize>,
    /// The checks that ran.
    pub checked: Vec<Check>,
    pub skipped: Vec<SkippedCheck>,
}

/// What a review reads, resolved under the project lock: the working timeline, the assets and the
/// cached analysis of the assets the timeline uses.
#[derive(Debug, Clone)]
pub struct ReviewInputs {
    pub timeline: Timeline,
    pub assets: Vec<Asset>,
    pub analyses: HashMap<Uuid, AssetAnalysis>,
    /// The stabilized clips, for [`Check::Unstabilized`]: where each stands is read from the
    /// stabilization analyses kept beside the footage when the review runs (the lock released).
    pub stabilization: Vec<crate::project::StabilizeStatusInput>,
}

/// Review the cut as it renders (muted tracks and disabled clips left out) at the rate an export
/// writes it, running `checks` and keeping what falls in `range` (timeline seconds, clamped the way
/// a range export clamps it; `None` for the whole cut).
pub fn review(inputs: &ReviewInputs, range: Option<(f64, f64)>, checks: &[Check]) -> CutReview {
    let (fps, _) = crate::delivery_rate(&inputs.timeline, &inputs.assets);
    let rendered = inputs.timeline.for_render();
    let span = rendered.export_span(range);
    let cut = Cut {
        assets: inputs.assets.iter().map(|a| (a.id, a)).collect(),
        analyses: &inputs.analyses,
        frame: 1.0 / fps.max(1.0),
        fps: fps.max(1.0),
        rendered: &rendered,
    };
    let mut issues = Vec::new();
    let mut checked = Vec::new();
    let mut skipped = Vec::new();
    for &check in checks {
        let outcome = match check {
            Check::FlashFrame => cut.flash_frames(&mut issues),
            Check::MidWordCut => cut.mid_word_cuts(&mut issues),
            Check::HardAudioCut => cut.hard_audio_cuts(&mut issues),
            Check::JumpCut => cut.jump_cuts(&mut issues),
            Check::StaleCaptions => cut.stale_captions(span, &mut issues),
            Check::BlackGap => cut.black_gaps(&mut issues),
            Check::Unstabilized => {
                let statuses = crate::Project::stabilization_statuses(&inputs.stabilization);
                unstabilized(&inputs.timeline, &statuses, &mut issues);
                Ok(())
            }
        };
        match outcome {
            Ok(()) => checked.push(check),
            Err(reason) => skipped.push(SkippedCheck { check, reason }),
        }
    }
    let assets_of: HashMap<Uuid, Uuid> = rendered
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| (c.id, c.asset_id))
        .collect();
    let mut issues = merge_duplicates(issues, &assets_of);
    if let Some((start, end)) = span {
        issues.retain(|i| i.end.unwrap_or(i.time) >= start - 1e-9 && i.time <= end + 1e-9);
    }
    issues.sort_by(|a, b| a.time.total_cmp(&b.time).then(a.kind.cmp(&b.kind)));
    let mut counts_by_kind = BTreeMap::new();
    for issue in &issues {
        *counts_by_kind.entry(issue.kind).or_insert(0) += 1;
    }
    CutReview {
        issues,
        counts_by_kind,
        checked,
        skipped,
    }
}

/// One defect found twice — a picture and its own sound doubled on an audio lane, a linked pair cut
/// together — is one issue naming every clip: the same kind at the same moment in the same footage.
fn merge_duplicates(issues: Vec<ReviewIssue>, assets_of: &HashMap<Uuid, Uuid>) -> Vec<ReviewIssue> {
    let mut out: Vec<ReviewIssue> = Vec::with_capacity(issues.len());
    let mut seen: HashMap<(Check, i64, Vec<Uuid>), usize> = HashMap::new();
    for issue in issues {
        let mut footage: Vec<Uuid> = issue.clip_ids.iter().filter_map(|id| assets_of.get(id).copied()).collect();
        footage.sort();
        footage.dedup();
        let key = (issue.kind, (issue.time * 1000.0).round() as i64, footage);
        match seen.get(&key) {
            Some(&i) => {
                for id in issue.clip_ids {
                    if !out[i].clip_ids.contains(&id) {
                        out[i].clip_ids.push(id);
                    }
                }
            }
            None => {
                seen.insert(key, out.len());
                out.push(issue);
            }
        }
    }
    out
}

/// [`Check::Unstabilized`]: each stabilized clip whose `statuses` say it will play as shot.
fn unstabilized(timeline: &Timeline, statuses: &[crate::stabilize::ClipStabilizeStatus], issues: &mut Vec<ReviewIssue>) {
    for u in crate::stabilize::unsteady_clips(timeline, statuses, None) {
        issues.push(ReviewIssue {
            kind: Check::Unstabilized,
            severity: Severity::Warning,
            time: u.start,
            end: Some(u.end),
            clip_ids: vec![u.clip_id],
            message: format!(
                "This clip is set to be stabilized but plays and exports as shot: {}",
                u.reason
            ),
            fix: "stabilize_clip (measures the camera; stabilization_status says where it stands), or \
                  stabilize_clip with enabled: false to stop stabilizing it"
                .into(),
        });
    }
}

fn secs(t: f64) -> String {
    format!("{t:.2}s")
}

/// Where a clip's first frame comes from in its source.
fn head_source(c: &Clip) -> f64 {
    if c.is_reversed() {
        c.source_out
    } else {
        c.source_in
    }
}

/// Where a clip's last frame comes from in its source.
fn tail_source(c: &Clip) -> f64 {
    if c.is_reversed() {
        c.source_in
    } else {
        c.source_out
    }
}

/// How far apart the speeds of two neighbouring clips are where they meet: the difference of their
/// speeds, or, when either changes its rate over its length, of the speeds at the cut (a clip that plays
/// the other way round is infinitely far).
fn pace_gap(a: &Clip, b: &Clip) -> f64 {
    if !a.is_ramped() && !b.is_ramped() {
        return (a.speed - b.speed).abs();
    }
    if a.is_reversed() != b.is_reversed() {
        return f64::INFINITY;
    }
    (a.speed_at(a.duration()) - b.speed_at(0.0)).abs()
}

fn in_silence(analysis: &AssetAnalysis, t: f64) -> bool {
    analysis
        .silence_segments
        .iter()
        .any(|r| r.start - EDGE_SILENCE_MARGIN <= t && t <= r.end + EDGE_SILENCE_MARGIN)
}

/// Lowercase letters and digits of a word, for comparing captions with transcripts.
fn token(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace().map(token).filter(|t| !t.is_empty()).collect()
}

/// A transcript word as it plays in the cut.
struct SpokenWord {
    token: String,
    /// Timeline seconds of its middle, through the clip it plays in (not clamped to the clip).
    mid: f64,
    clip: usize,
    /// Inside the clip's window, i.e. heard.
    heard: bool,
}

struct Cut<'a> {
    rendered: &'a Timeline,
    assets: HashMap<Uuid, &'a Asset>,
    analyses: &'a HashMap<Uuid, AssetAnalysis>,
    fps: f64,
    frame: f64,
}

impl Cut<'_> {
    fn analysis(&self, asset: Uuid) -> Option<&AssetAnalysis> {
        self.analyses.get(&asset)
    }

    /// Whether the clip plays sound: its own asset's, not detached, not faded to nothing.
    fn sounds(&self, clip: &Clip) -> bool {
        clip.source_audio && self.assets.get(&clip.asset_id).is_some_and(|a| a.has_audio())
    }

    /// Whether the clip draws a picture: footage, a still or a generated title.
    fn visible(&self, clip: &Clip) -> bool {
        clip.generator.is_some()
            || self
                .assets
                .get(&clip.asset_id)
                .is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Video))
    }

    fn is_footage(&self, clip: &Clip) -> bool {
        clip.generator.is_none()
            && self
                .assets
                .get(&clip.asset_id)
                .is_some_and(|a| !a.is_image() && a.streams.iter().any(|s| s.kind == StreamKind::Video))
    }

    /// Each track's clips in timeline order, with the track's name.
    fn lanes(&self) -> Vec<(&str, StreamKind, Vec<&Clip>)> {
        self.rendered
            .tracks
            .iter()
            .map(|t| {
                let mut clips: Vec<&Clip> = t.clips.iter().collect();
                clips.sort_by(|a, b| a.timeline_start.total_cmp(&b.timeline_start));
                (t.name.as_str(), t.kind, clips)
            })
            .collect()
    }

    /// Whether `b` starts where `a` ends, within half a frame.
    fn meet(&self, a: &Clip, b: &Clip) -> bool {
        (b.timeline_start - a.timeline_end()).abs() <= self.frame / 2.0
    }

    /// Whether `b` plays on from where `a` stopped — a split nobody took anything out of, which
    /// is no cut at all to the eye or the ear.
    fn continues(&self, a: &Clip, b: &Clip) -> bool {
        let tolerance = self.frame / 2.0 * a.speed_at(a.duration());
        a.asset_id == b.asset_id && pace_gap(a, b) < 1e-9 && (head_source(b) - tail_source(a)).abs() <= tolerance
    }

    fn flash_frames(&self, issues: &mut Vec<ReviewIssue>) -> std::result::Result<(), String> {
        for (lane, kind, clips) in self.lanes() {
            if kind != StreamKind::Video {
                continue;
            }
            for clip in clips.into_iter().filter(|c| self.visible(c)) {
                let frames = clip.duration() * self.fps;
                if frames >= FLASH_FRAMES - 1e-6 {
                    continue;
                }
                issues.push(ReviewIssue {
                    kind: Check::FlashFrame,
                    severity: Severity::Warning,
                    time: clip.timeline_start,
                    end: Some(clip.timeline_end()),
                    clip_ids: vec![clip.id],
                    message: format!(
                        "A clip on {lane} at {} is {:.1} frames long at {} fps — it flashes on screen.",
                        secs(clip.timeline_start),
                        frames,
                        crate::model::fmt_fps(self.fps)
                    ),
                    fix: "remove_clips to take it out, or trim / roll_edit to give it a readable length".to_string(),
                });
            }
        }
        Ok(())
    }

    fn mid_word_cuts(&self, issues: &mut Vec<ReviewIssue>) -> std::result::Result<(), String> {
        let mut transcribed = false;
        for (lane, _, clips) in self.lanes() {
            for (i, clip) in clips.iter().enumerate() {
                if !self.sounds(clip) {
                    continue;
                }
                let Some(analysis) = self.analysis(clip.asset_id).filter(|a| !a.transcript.is_empty()) else {
                    continue;
                };
                transcribed = true;
                let duration = self.assets.get(&clip.asset_id).map_or(f64::INFINITY, |a| a.duration);
                let joined_before = i > 0 && self.meet(clips[i - 1], clip) && self.continues(clips[i - 1], clip);
                let joined_after = clips
                    .get(i + 1)
                    .is_some_and(|next| self.meet(clip, next) && self.continues(clip, next));
                let edges = [
                    ("starts", clip.timeline_start, head_source(clip), joined_before),
                    ("ends", clip.timeline_end(), tail_source(clip), joined_after),
                ];
                for (verb, time, source, joined) in edges {
                    // The file's own first and last moments are not a cut anyone made.
                    if joined || source <= WORD_TOLERANCE || source >= duration - WORD_TOLERANCE {
                        continue;
                    }
                    if analysis.done(AnalysisKind::Silence) && in_silence(analysis, source) {
                        continue;
                    }
                    let Some((word, start, end, estimated)) = word_at(&analysis.transcript, source) else {
                        continue;
                    };
                    let (severity, note) = if estimated {
                        (Severity::Tip, " (word times estimated — confirm with get_waveform_range)")
                    } else {
                        (Severity::Warning, "")
                    };
                    issues.push(ReviewIssue {
                        kind: Check::MidWordCut,
                        severity,
                        time,
                        end: None,
                        clip_ids: vec![clip.id],
                        message: format!(
                            "A clip on {lane} {verb} at {} inside the word \"{word}\" ({start:.2}–{end:.2}s in its source), so the word is clipped{note}.",
                            secs(time)
                        ),
                        fix: "trim the edge (or roll_edit the cut between two clips) to the word's boundary — get_transcript with words: true gives the bounds, and cut_clip_range removes whole words".to_string(),
                    });
                }
            }
        }
        if transcribed {
            Ok(())
        } else {
            Err("no sounding clip's footage has a transcript; analyze_asset with steps [\"transcript\"] first".to_string())
        }
    }

    fn hard_audio_cuts(&self, issues: &mut Vec<ReviewIssue>) -> std::result::Result<(), String> {
        let mut judged = 0usize;
        let mut unjudged: Vec<(f64, Uuid, Uuid)> = Vec::new();
        for (lane, _, clips) in self.lanes() {
            for pair in clips.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if !self.meet(a, b) || !self.sounds(a) || !self.sounds(b) || self.continues(a, b) {
                    continue;
                }
                if b.transition_in.is_some() || a.fade_out > 0.0 || b.fade_in > 0.0 {
                    continue;
                }
                if a.volume_at(a.duration()) <= 1e-3 || b.volume_at(0.0) <= 1e-3 {
                    continue;
                }
                let silence = |c: &Clip| self.analysis(c.asset_id).filter(|x| x.done(AnalysisKind::Silence));
                let (Some(sa), Some(sb)) = (silence(a), silence(b)) else {
                    unjudged.push((b.timeline_start, a.id, b.id));
                    continue;
                };
                judged += 1;
                if in_silence(sa, tail_source(a)) || in_silence(sb, head_source(b)) {
                    continue;
                }
                issues.push(ReviewIssue {
                    kind: Check::HardAudioCut,
                    severity: Severity::Warning,
                    time: b.timeline_start,
                    end: None,
                    clip_ids: vec![a.id, b.id],
                    message: format!(
                        "Sound runs straight into sound at {} on {lane} with no fade or crossfade — it may click.",
                        secs(b.timeline_start)
                    ),
                    fix: "set_fade a few hundredths of a second (0.02–0.05) on both sides, or set_transition crossfade on the second clip".to_string(),
                });
            }
        }
        if judged == 0 && !unjudged.is_empty() {
            return Err(format!(
                "{} hard cut(s) between sounding clips, but their footage has no silence analysis; analyze_asset with steps [\"silence\"] first",
                unjudged.len()
            ));
        }
        if let Some(&(time, a, b)) = unjudged.first() {
            issues.push(ReviewIssue {
                kind: Check::HardAudioCut,
                severity: Severity::Tip,
                time,
                end: None,
                clip_ids: vec![a, b],
                message: format!(
                    "{} hard cut(s) between sounding clips, the first at {}, were not judged: their footage has no silence analysis.",
                    unjudged.len(),
                    secs(time)
                ),
                fix: "analyze_asset with steps [\"silence\"] and run review_cut again".to_string(),
            });
        }
        Ok(())
    }

    fn jump_cuts(&self, issues: &mut Vec<ReviewIssue>) -> std::result::Result<(), String> {
        for (lane, kind, clips) in self.lanes() {
            if kind != StreamKind::Video {
                continue;
            }
            for pair in clips.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if a.asset_id != b.asset_id || !self.is_footage(a) || !self.meet(a, b) || b.transition_in.is_some() {
                    continue;
                }
                if a.is_reversed() || b.is_reversed() || pace_gap(a, b) > 1e-6 {
                    continue;
                }
                let gap = b.source_in - a.source_out;
                if gap <= self.frame * a.speed_at(a.duration()) || gap > JUMP_MAX_SOURCE_GAP {
                    continue;
                }
                if reframed(a, b) {
                    continue;
                }
                let new_shot = self.analysis(a.asset_id).is_some_and(|x| {
                    x.scene_changes
                        .iter()
                        .any(|&s| s >= a.source_out - self.frame && s <= b.source_in + self.frame)
                });
                if new_shot {
                    continue;
                }
                issues.push(ReviewIssue {
                    kind: Check::JumpCut,
                    severity: Severity::Tip,
                    time: b.timeline_start,
                    end: None,
                    clip_ids: vec![a.id, b.id],
                    message: format!(
                        "Jump cut at {} on {lane}: the same shot resumes {gap:.2}s later in its source, so the picture jumps. Fine if deliberate.",
                        secs(b.timeline_start)
                    ),
                    fix: "set_transform to punch in (scale about 1.15) on one side, set_transition crossfade, or cover the cut with a cutaway on a track above (add_clip_to_timeline)".to_string(),
                });
            }
        }
        Ok(())
    }

    fn black_gaps(&self, issues: &mut Vec<ReviewIssue>) -> std::result::Result<(), String> {
        let mut pictures: Vec<(f64, f64, Uuid)> = self
            .rendered
            .tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .flat_map(|t| t.clips.iter())
            .filter(|c| self.visible(c))
            .map(|c| (c.timeline_start, c.timeline_end(), c.id))
            .collect();
        if pictures.is_empty() {
            return Ok(());
        }
        pictures.sort_by(|a, b| a.0.total_cmp(&b.0));
        // A title over black is a title card, not a hole.
        let titles: Vec<(f64, f64)> = self
            .rendered
            .svg_overlays
            .iter()
            .map(|o| (o.start, o.end))
            .chain(
                self.rendered
                    .overlays
                    .iter()
                    .filter(|o| !o.generated)
                    .map(|o| (o.start, o.end)),
            )
            .collect();
        let end = self.rendered.duration();
        let min_gap = self.frame * MIN_GAP_FRAMES;
        let mut cursor = 0.0f64;
        let mut before: Option<Uuid> = None;
        let mut holes: Vec<(f64, f64, Option<Uuid>, Option<Uuid>)> = Vec::new();
        for &(start, stop, id) in &pictures {
            if start - cursor > min_gap {
                holes.push((cursor, start, before, Some(id)));
            }
            if stop >= cursor {
                cursor = stop;
                before = Some(id);
            }
        }
        if end - cursor > min_gap {
            holes.push((cursor, end, before, None));
        }
        for (start, stop, before, after) in holes {
            if titles.iter().any(|&(a, b)| a <= start + min_gap && b >= stop - min_gap) {
                continue;
            }
            let tail = after.is_none();
            issues.push(ReviewIssue {
                kind: Check::BlackGap,
                severity: if tail { Severity::Tip } else { Severity::Warning },
                time: start,
                end: Some(stop),
                clip_ids: before.into_iter().chain(after).collect(),
                message: if tail {
                    format!(
                        "The picture ends at {} but the sound runs on to {}, over black.",
                        secs(start),
                        secs(stop)
                    )
                } else {
                    format!("Black from {} to {}: no clip has a picture there.", secs(start), secs(stop))
                },
                fix: if tail {
                    "trim the sound to end with the picture, or extend the last shot (trim, or add_clip_to_timeline)".to_string()
                } else {
                    "move_clips or ripple_delete to close it, trim a neighbour to cover it, or fill it with a clip or a title"
                        .to_string()
                },
            });
        }
        Ok(())
    }

    fn stale_captions(&self, span: Option<(f64, f64)>, issues: &mut Vec<ReviewIssue>) -> std::result::Result<(), String> {
        let captions: Vec<_> = self.rendered.overlays.iter().filter(|o| o.generated).collect();
        if captions.is_empty() {
            return Err("no generated captions on the cut".to_string());
        }
        let transcripts: HashMap<Uuid, Vec<TranscriptSegment>> = self
            .rendered
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter_map(|c| {
                let a = self.analysis(c.asset_id)?;
                (!a.transcript.is_empty()).then(|| (c.asset_id, a.transcript.clone()))
            })
            .collect();
        if transcripts.is_empty() {
            return Err("none of the footage in the cut has a transcript to compare the captions with".to_string());
        }
        let clips: Vec<&Clip> = self.rendered.tracks.iter().flat_map(|t| t.clips.iter()).collect();
        let mut spoken: Vec<SpokenWord> = Vec::new();
        for (index, clip) in clips.iter().enumerate() {
            let Some(segments) = transcripts.get(&clip.asset_id) else {
                continue;
            };
            let window = (clip.timeline_start, clip.timeline_end());
            for seg in segments.iter().filter(|s| clip.covers_source(s.start, s.end)) {
                for w in words_of(seg).iter() {
                    let token = token(&w.text);
                    if token.is_empty() || !(w.start.is_finite() && w.end.is_finite()) {
                        continue;
                    }
                    let mid = clip.source_to_timeline((w.start + w.end) / 2.0);
                    if mid < window.0 - CAPTION_REACH || mid > window.1 + CAPTION_REACH {
                        continue;
                    }
                    spoken.push(SpokenWord {
                        token,
                        mid,
                        clip: index,
                        heard: mid >= window.0 && mid <= window.1,
                    });
                }
            }
        }
        spoken.sort_by(|a, b| a.mid.total_cmp(&b.mid));
        // What a caption could have been made from: runs of the transcripts' words.
        let mut pairs: HashSet<(String, String)> = HashSet::new();
        let mut singles: HashSet<String> = HashSet::new();
        for segments in transcripts.values() {
            let words: Vec<String> = segments
                .iter()
                .flat_map(|s| words_of(s).iter().map(|w| token(&w.text)).collect::<Vec<_>>())
                .filter(|t| !t.is_empty())
                .collect();
            for pair in words.windows(2) {
                pairs.insert((pair[0].clone(), pair[1].clone()));
            }
            singles.extend(words);
        }
        let wanted = |a: f64, b: f64| span.is_none_or(|(s, e)| b >= s && a <= e);
        let mut known = 0usize;
        let mut stale: Vec<(f64, f64, Uuid)> = Vec::new();
        for caption in captions.iter().filter(|o| wanted(o.start, o.end)) {
            let words = tokens(&caption.text);
            if words.is_empty() {
                continue;
            }
            let from_transcript = match words.as_slice() {
                [one] => singles.contains(one),
                many => many.windows(2).all(|p| pairs.contains(&(p[0].clone(), p[1].clone()))),
            };
            if !from_transcript {
                continue;
            }
            known += 1;
            let over = |w: &SpokenWord| {
                let clip = clips[w.clip];
                let lo = if (caption.start - clip.timeline_start).abs() < 1e-3 {
                    caption.start - CAPTION_REACH
                } else {
                    caption.start - CAPTION_SLACK
                };
                let hi = if (caption.end - clip.timeline_end()).abs() < 1e-3 {
                    caption.end + CAPTION_REACH
                } else {
                    caption.end + CAPTION_SLACK
                };
                w.mid >= lo && w.mid <= hi
            };
            let near = &spoken[spoken.partition_point(|w| w.mid < caption.start - CAPTION_REACH)
                ..spoken.partition_point(|w| w.mid <= caption.end + CAPTION_REACH)];
            let matched = words
                .iter()
                .filter(|t| near.iter().any(|w| &w.token == *t && over(w)))
                .count();
            if (matched as f64) < words.len() as f64 * CAPTION_MATCH {
                stale.push((caption.start, caption.end, caption.id));
            }
        }
        if known == 0 {
            if span.is_some() && !captions.iter().any(|o| wanted(o.start, o.end)) {
                return Ok(());
            }
            return Err(
                "the captions do not come from the footage's transcripts (imported, or another language), so there is nothing to compare them with"
                    .to_string(),
            );
        }
        let fix = "generate_captions again in the same style (it replaces the generated set): captions are placed in timeline time and do not follow later edits";
        if let (Some(first), Some(last)) = (stale.first(), stale.last()) {
            issues.push(ReviewIssue {
                kind: Check::StaleCaptions,
                severity: Severity::Warning,
                time: first.0,
                end: Some(last.1.max(first.1)),
                clip_ids: Vec::new(),
                message: format!(
                    "{} of {} captions no longer sit over the words they show (the first at {}) — the cut changed after captioning.",
                    stale.len(),
                    known,
                    secs(first.0)
                ),
                fix: fix.to_string(),
            });
        }
        if !stale.is_empty() {
            return Ok(());
        }
        // Speech captioning would cover now that no caption covers: material added after captioning.
        let expected = Spans::new(
            self.rendered
                .captions(&transcripts, crate::model::CaptionOptions::default())
                .iter()
                .map(|o| (o.start, o.end)),
        );
        let stored = Spans::new(captions.iter().map(|o| (o.start, o.end)));
        let mut heard: Vec<&SpokenWord> = spoken
            .iter()
            .filter(|w| w.heard && wanted(w.mid, w.mid) && expected.cover(w.mid, 0.0))
            .collect();
        heard.dedup_by(|a, b| a.token == b.token && (a.mid - b.mid).abs() < 1e-3);
        let mut runs: Vec<(f64, f64, usize)> = Vec::new();
        let mut run: Option<(f64, f64, usize)> = None;
        for w in heard {
            if stored.cover(w.mid, CAPTION_SLACK) {
                runs.extend(run.take());
            } else {
                let r = run.get_or_insert((w.mid, w.mid, 0));
                r.1 = w.mid;
                r.2 += 1;
            }
        }
        runs.extend(run);
        let runs: Vec<_> = runs.into_iter().filter(|r| r.2 >= UNCAPTIONED_MIN_WORDS).collect();
        if let (Some(first), Some(last)) = (runs.first(), runs.last()) {
            let words: usize = runs.iter().map(|r| r.2).sum();
            issues.push(ReviewIssue {
                kind: Check::StaleCaptions,
                severity: Severity::Warning,
                time: first.0,
                end: Some(last.1),
                clip_ids: Vec::new(),
                message: format!(
                    "{words} spoken words have no caption (the first at {}) — footage added after captioning.",
                    secs(first.0)
                ),
                fix: fix.to_string(),
            });
        }
        Ok(())
    }
}

/// Time spans sorted by start, with the latest end so far beside each, to ask "is `t` inside any
/// of them" in a binary search.
struct Spans {
    spans: Vec<(f64, f64)>,
    reach: Vec<f64>,
}

impl Spans {
    fn new(spans: impl Iterator<Item = (f64, f64)>) -> Self {
        let mut spans: Vec<(f64, f64)> = spans.collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let reach = spans
            .iter()
            .scan(f64::NEG_INFINITY, |end, s| {
                *end = end.max(s.1);
                Some(*end)
            })
            .collect();
        Spans { spans, reach }
    }

    /// Whether some span, widened by `slack` each side, holds `t`.
    fn cover(&self, t: f64, slack: f64) -> bool {
        let n = self.spans.partition_point(|s| s.0 <= t + slack);
        n > 0 && self.reach[n - 1] >= t - slack
    }
}

/// The word of `segments` a cut at source time `t` falls inside, with its bounds and whether its
/// times were estimated.
fn word_at(segments: &[TranscriptSegment], t: f64) -> Option<(String, f64, f64, bool)> {
    segments
        .iter()
        .filter(|s| s.start - MAX_WORD_SECS <= t && t <= s.end + MAX_WORD_SECS)
        .find_map(|seg| {
            words_of(seg).iter().find_map(|w| {
                let length = w.end - w.start;
                let tolerance = if w.estimated {
                    ESTIMATED_WORD_TOLERANCE
                } else {
                    WORD_TOLERANCE
                };
                (length > 0.0 && length <= MAX_WORD_SECS && w.start + tolerance < t && t < w.end - tolerance)
                    .then(|| (w.text.trim().to_string(), w.start, w.end, w.estimated))
            })
        })
}

/// Whether the framing changes across a cut — a punch-in, a move, a keyed zoom or a new 360
/// view — which is how a jump cut is made deliberate.
fn reframed(a: &Clip, b: &Clip) -> bool {
    let (ta, tb) = (&a.transform, &b.transform);
    let scale = (ta.scale - tb.scale).abs() / ta.scale.abs().max(1e-6);
    let moved = [
        (ta.pos_x, tb.pos_x),
        (ta.pos_y, tb.pos_y),
        (ta.crop_left, tb.crop_left),
        (ta.crop_right, tb.crop_right),
        (ta.crop_top, tb.crop_top),
        (ta.crop_bottom, tb.crop_bottom),
        (ta.rotation / 90.0, tb.rotation / 90.0),
    ]
    .iter()
    .any(|(x, y)| (x - y).abs() > 0.01);
    scale >= PUNCH_IN_SCALE
        || moved
        || a.is_animated()
        || b.is_animated()
        || a.crop_animated()
        || b.crop_animated()
        || a.reframe != b.reframe
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{audio_stream, av_asset, img_asset, test_asset};
    use crate::model::{CaptionOptions, CaptionStyle, TextOverlay, TimeRange, Transition, TransitionKind, Word};

    /// Eight words, one every half second from 0, each 0.4 s long: a cut at x.45 is between two.
    fn speech(asset: Uuid) -> AssetAnalysis {
        let words: Vec<Word> = ["one", "two", "three", "four", "five", "six", "seven", "eight"]
            .iter()
            .enumerate()
            .map(|(i, w)| Word {
                text: format!(" {w}"),
                start: i as f64 * 0.5,
                end: i as f64 * 0.5 + 0.4,
                ..Word::default()
            })
            .collect();
        let segment = |range: std::ops::Range<usize>| TranscriptSegment {
            start: words[range.start].start,
            end: words[range.end - 1].end,
            text: words[range.clone()]
                .iter()
                .map(|w| w.text.trim())
                .collect::<Vec<_>>()
                .join(" "),
            words: words[range].to_vec(),
            speaker: None,
        };
        AssetAnalysis {
            asset_id: asset,
            transcript: vec![segment(0..4), segment(4..8)],
            ran: vec![AnalysisKind::Transcript],
            ..AssetAnalysis::default()
        }
    }

    fn inputs(timeline: Timeline, assets: Vec<Asset>, analyses: Vec<AssetAnalysis>) -> ReviewInputs {
        ReviewInputs {
            timeline,
            assets,
            analyses: analyses.into_iter().map(|a| (a.asset_id, a)).collect(),
            stabilization: Vec::new(),
        }
    }

    fn found(inputs: &ReviewInputs, check: Check) -> Vec<ReviewIssue> {
        review(inputs, None, &[check]).issues
    }

    /// A stabilized clip whose camera was never measured exports as shot: said, with the tool that
    /// measures it. One that is steadied (or cannot be told apart from shot, without vid.stab) is not.
    #[test]
    fn a_stabilized_clip_with_no_analysis_is_flagged() {
        let asset = av_asset(Uuid::from_u128(77), 10.0);
        let mut clip = Clip::new(asset.id, 0.0, 3.0, 1.0);
        clip.stabilize = Some(crate::model::Stabilize::default());
        let mut r = inputs(on_v1(vec![clip.clone()]), vec![asset.clone()], vec![]);
        r.stabilization = vec![crate::project::StabilizeStatusInput {
            clip_id: clip.id,
            stabilize: clip.stabilize,
            unsupported: None,
            asset_id: asset.id,
            path: Some(std::path::PathBuf::from("/nowhere/kerf-review-unsteady.mp4")),
            window: (0.0, 3.0),
        }];
        crate::engine::with_vidstab(true, || {
            let issues = found(&r, Check::Unstabilized);
            assert_eq!(issues.len(), 1, "{issues:?}");
            assert_eq!((issues[0].time, issues[0].end), (1.0, Some(4.0)));
            assert_eq!(issues[0].severity, Severity::Warning);
            assert!(issues[0].fix.contains("stabilize_clip"), "{}", issues[0].fix);
            assert!(review(&r, Some((0.0, 0.5)), &[Check::Unstabilized]).issues.is_empty());
        });
        crate::engine::with_vidstab(false, || assert!(found(&r, Check::Unstabilized).is_empty()));
    }

    fn on_v1(clips: Vec<Clip>) -> Timeline {
        let mut t = Timeline::new();
        t.tracks[0].clips = clips;
        t
    }

    #[test]
    fn a_clip_under_three_frames_flashes_and_one_of_three_does_not() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let flash = Clip::new(a.id, 10.0, 10.0 + 2.0 / 30.0, 0.0);
        let held = Clip::new(a.id, 20.0, 20.0 + 3.0 / 30.0, 1.0);
        let r = inputs(on_v1(vec![flash.clone(), held]), vec![a], vec![]);
        let issues = found(&r, Check::FlashFrame);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].clip_ids, vec![flash.id]);
        assert_eq!(issues[0].severity, Severity::Warning);
        assert!(
            issues[0].message.contains("2.0 frames") && issues[0].message.contains("30 fps"),
            "{}",
            issues[0].message
        );
        assert!(issues[0].fix.contains("remove_clips"));
    }

    #[test]
    fn a_flash_frame_is_counted_at_the_delivery_rate() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        // Two frames at 30 fps is five at 75.
        let mut t = on_v1(vec![Clip::new(a.id, 10.0, 10.0 + 2.0 / 30.0, 0.0)]);
        t.format = Some(
            crate::model::Delivery::new(1920, 1080, crate::model::Fit::Cover)
                .with_fps(Some(75.0))
                .unwrap(),
        );
        assert!(found(&inputs(t, vec![a], vec![]), Check::FlashFrame).is_empty());
    }

    #[test]
    fn a_short_sound_on_an_audio_track_is_not_a_flash_frame() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let mut t = Timeline::new();
        t.tracks[1].clips = vec![Clip::new(a.id, 10.0, 10.02, 0.0)];
        assert!(found(&inputs(t, vec![a], vec![]), Check::FlashFrame).is_empty());
    }

    #[test]
    fn a_cut_inside_a_word_is_found_and_one_between_words_is_not() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        // In at 1.2 (inside "three", 1.0–1.4); out at 3.45 (between "seven" and "eight").
        let clip = Clip::new(a.id, 1.2, 3.45, 5.0);
        let r = inputs(on_v1(vec![clip]), vec![a.clone()], vec![speech(a.id)]);
        let issues = found(&r, Check::MidWordCut);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!((issues[0].time, issues[0].severity), (5.0, Severity::Warning));
        assert!(issues[0].message.contains("\"three\""), "{}", issues[0].message);
        assert!(issues[0].fix.contains("get_transcript"));

        // Within the recognizer's slack of the word's start is not inside it.
        let r = inputs(
            on_v1(vec![Clip::new(a.id, 1.02, 3.45, 5.0)]),
            vec![a.clone()],
            vec![speech(a.id)],
        );
        assert!(found(&r, Check::MidWordCut).is_empty());
    }

    #[test]
    fn a_mid_word_edge_is_found_through_the_clip_speed() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        // At double speed the out-point 2.7 (inside "six", 2.5–2.9) plays at 1 + 2.7 / 2.
        let mut clip = Clip::new(a.id, 0.0, 2.7, 1.0);
        clip.speed = 2.0;
        let issues = found(
            &inputs(on_v1(vec![clip]), vec![a.clone()], vec![speech(a.id)]),
            Check::MidWordCut,
        );
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!((issues[0].time - 2.35).abs() < 1e-9, "{}", issues[0].time);
        // Reversed, the clip's start plays the out-point.
        let mut clip = Clip::new(a.id, 0.45, 2.7, 1.0);
        clip.speed = -1.0;
        let issues = found(
            &inputs(on_v1(vec![clip]), vec![a.clone()], vec![speech(a.id)]),
            Check::MidWordCut,
        );
        assert_eq!(issues.iter().map(|i| i.time).collect::<Vec<_>>(), vec![1.0]);
    }

    #[test]
    fn a_split_nothing_was_taken_out_of_is_not_a_cut() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let left = Clip::new(a.id, 0.45, 1.2, 0.0);
        let right = Clip::new(a.id, 1.2, 3.45, 0.75);
        let r = inputs(on_v1(vec![left, right]), vec![a.clone()], vec![speech(a.id)]);
        assert!(
            review(&r, None, &Check::ALL).issues.is_empty(),
            "{:?}",
            review(&r, None, &Check::ALL).issues
        );
    }

    #[test]
    fn an_estimated_word_is_a_tip_that_says_so_and_silence_overrules_a_word() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let mut analysis = speech(a.id);
        for w in analysis.transcript.iter_mut().flat_map(|s| s.words.iter_mut()) {
            w.estimated = true;
        }
        let r = inputs(on_v1(vec![Clip::new(a.id, 1.2, 3.45, 0.0)]), vec![a.clone()], vec![analysis]);
        let issues = found(&r, Check::MidWordCut);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Tip);
        assert!(issues[0].message.contains("estimated"), "{}", issues[0].message);

        let mut analysis = speech(a.id);
        analysis.silence_segments = vec![TimeRange { start: 1.1, end: 1.3 }];
        analysis.ran.push(AnalysisKind::Silence);
        let r = inputs(on_v1(vec![Clip::new(a.id, 1.2, 3.45, 0.0)]), vec![a], vec![analysis]);
        assert!(found(&r, Check::MidWordCut).is_empty());
    }

    #[test]
    fn mid_word_cuts_are_skipped_without_a_transcript() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let r = review(
            &inputs(on_v1(vec![Clip::new(a.id, 1.2, 3.0, 0.0)]), vec![a], vec![]),
            None,
            &[Check::MidWordCut],
        );
        assert!(r.checked.is_empty());
        assert_eq!(r.skipped[0].check, Check::MidWordCut);
        assert!(r.skipped[0].reason.contains("transcript"), "{}", r.skipped[0].reason);
    }

    #[test]
    fn a_picture_and_its_linked_sound_report_a_cut_once() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let link = Some(Uuid::new_v4());
        let mut picture = Clip::new(a.id, 1.2, 3.45, 0.0);
        picture.link_id = link;
        picture.source_audio = false;
        let mut sound = Clip::new(a.id, 1.2, 3.45, 0.0);
        sound.link_id = link;
        let mut t = on_v1(vec![picture]);
        t.tracks[1].clips = vec![sound.clone()];
        let issues = found(&inputs(t, vec![a.clone()], vec![speech(a.id)]), Check::MidWordCut);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].clip_ids, vec![sound.id]);

        // The same sound playing twice (picture and an audio copy) is one defect naming both.
        let picture = Clip::new(a.id, 1.2, 3.45, 0.0);
        let copy = Clip::new(a.id, 1.2, 3.45, 0.0);
        let mut t = on_v1(vec![picture.clone()]);
        t.tracks[1].clips = vec![copy.clone()];
        let issues = found(&inputs(t, vec![a.clone()], vec![speech(a.id)]), Check::MidWordCut);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].clip_ids, vec![picture.id, copy.id]);
    }

    fn sounding(asset: Uuid, silences: &[(f64, f64)]) -> AssetAnalysis {
        AssetAnalysis {
            asset_id: asset,
            silence_segments: silences.iter().map(|&(start, end)| TimeRange { start, end }).collect(),
            ran: vec![AnalysisKind::Silence],
            ..AssetAnalysis::default()
        }
    }

    #[test]
    fn sound_slammed_into_sound_is_a_hard_cut_unless_faded_or_silent() {
        let (a, b) = (av_asset(Uuid::new_v4(), 100.0), av_asset(Uuid::new_v4(), 100.0));
        let pair = |fade: f64, transition: bool| {
            let first = Clip::new(a.id, 10.0, 12.0, 0.0);
            let mut second = Clip::new(b.id, 30.0, 32.0, 2.0);
            second.fade_in = fade;
            if transition {
                second.transition_in = Some(Transition {
                    kind: TransitionKind::Crossfade,
                    duration: 0.2,
                });
            }
            let mut t = Timeline::new();
            t.tracks[1].clips = vec![first, second];
            t
        };
        let analyses = || vec![sounding(a.id, &[]), sounding(b.id, &[])];
        let issues = found(
            &inputs(pair(0.0, false), vec![a.clone(), b.clone()], analyses()),
            Check::HardAudioCut,
        );
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(
            (issues[0].time, issues[0].severity, issues[0].clip_ids.len()),
            (2.0, Severity::Warning, 2)
        );
        assert!(issues[0].fix.contains("set_fade") && issues[0].fix.contains("set_transition"));

        for t in [pair(0.03, false), pair(0.0, true)] {
            assert!(found(&inputs(t, vec![a.clone(), b.clone()], analyses()), Check::HardAudioCut).is_empty());
        }
        // The outgoing clip ends in silence: nothing to click.
        let quiet = vec![sounding(a.id, &[(11.5, 13.0)]), sounding(b.id, &[])];
        assert!(found(
            &inputs(pair(0.0, false), vec![a.clone(), b.clone()], quiet),
            Check::HardAudioCut
        )
        .is_empty());
        // A picture without sound meeting one with sound is no audio cut.
        let mute = test_asset(vec![crate::engine::test_support::video_stream(1920, 1080, 30.0)]);
        let mut t = on_v1(vec![Clip::new(mute.id, 0.0, 2.0, 0.0), Clip::new(b.id, 30.0, 32.0, 2.0)]);
        t.tracks[1].clips.clear();
        assert!(found(
            &inputs(
                t,
                vec![mute.clone(), b.clone()],
                vec![sounding(mute.id, &[]), sounding(b.id, &[])]
            ),
            Check::HardAudioCut
        )
        .is_empty());
    }

    #[test]
    fn hard_cuts_without_silence_analysis_are_skipped_or_counted() {
        let (a, b, c) = (
            av_asset(Uuid::new_v4(), 100.0),
            av_asset(Uuid::new_v4(), 100.0),
            av_asset(Uuid::new_v4(), 100.0),
        );
        let mut t = Timeline::new();
        t.tracks[1].clips = vec![Clip::new(a.id, 10.0, 12.0, 0.0), Clip::new(b.id, 30.0, 32.0, 2.0)];
        let r = review(
            &inputs(t.clone(), vec![a.clone(), b.clone()], vec![]),
            None,
            &[Check::HardAudioCut],
        );
        assert_eq!(r.skipped.len(), 1);
        assert!(r.skipped[0].reason.contains("silence"), "{}", r.skipped[0].reason);

        t.tracks[1].clips.push(Clip::new(c.id, 40.0, 42.0, 4.0));
        let r = review(
            &inputs(
                t,
                vec![a.clone(), b.clone(), c],
                vec![sounding(a.id, &[]), sounding(b.id, &[])],
            ),
            None,
            &[Check::HardAudioCut],
        );
        assert_eq!(r.checked, vec![Check::HardAudioCut]);
        let severities: Vec<_> = r.issues.iter().map(|i| i.severity).collect();
        assert_eq!(severities, vec![Severity::Warning, Severity::Tip], "{:?}", r.issues);
    }

    #[test]
    fn the_same_shot_resuming_a_moment_later_is_a_jump_cut_tip() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let jump = |gap: f64, scale: f64| {
            let first = Clip::new(a.id, 10.0, 12.0, 0.0);
            let mut second = Clip::new(a.id, 12.0 + gap, 14.0 + gap, 2.0);
            second.transform.scale = scale;
            on_v1(vec![first, second])
        };
        let issues = found(&inputs(jump(1.0, 1.0), vec![a.clone()], vec![]), Check::JumpCut);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!((issues[0].time, issues[0].severity), (2.0, Severity::Tip));
        assert!(
            issues[0].message.contains("1.00s later") && issues[0].message.contains("deliberate"),
            "{}",
            issues[0].message
        );

        // Contiguous, far apart, punched in: not a jump cut.
        for t in [jump(0.0, 1.0), jump(20.0, 1.0), jump(1.0, 1.15)] {
            assert!(found(&inputs(t, vec![a.clone()], vec![]), Check::JumpCut).is_empty());
        }
        // A scene change between the two windows: the file cut to another shot there.
        let scenes = AssetAnalysis {
            asset_id: a.id,
            scene_changes: vec![12.5],
            ..AssetAnalysis::default()
        };
        assert!(found(&inputs(jump(1.0, 1.0), vec![a.clone()], vec![scenes]), Check::JumpCut).is_empty());
        // A still repeated is not a shot resuming.
        let img = img_asset(Uuid::new_v4());
        let t = on_v1(vec![Clip::new(img.id, 0.0, 2.0, 0.0), Clip::new(img.id, 3.0, 5.0, 2.0)]);
        assert!(found(&inputs(t, vec![img], vec![]), Check::JumpCut).is_empty());
    }

    #[test]
    fn a_ramped_clip_is_compared_at_the_speed_it_ends_and_begins_at() {
        use crate::model::SpeedKey;
        let a = av_asset(Uuid::new_v4(), 100.0);
        let crawl = |at: f64, source_in: f64| {
            let mut c = Clip::new(a.id, source_in, source_in + 1.0, at);
            c.speed_ramp = vec![SpeedKey::new(0.0, 0.5)];
            c
        };
        // Both crawl at half speed (2 s each) and the second resumes a second later in the footage: a jump.
        let t = on_v1(vec![crawl(0.0, 10.0), crawl(2.0, 12.0)]);
        let issues = found(&inputs(t, vec![a.clone()], vec![]), Check::JumpCut);
        assert_eq!(issues.len(), 1, "{issues:?}");
        // One at full speed is not the same shot carrying on.
        let mut fast = Clip::new(a.id, 12.0, 13.0, 2.0);
        fast.speed = 1.0;
        let t = on_v1(vec![crawl(0.0, 10.0), fast]);
        assert!(found(&inputs(t, vec![a.clone()], vec![]), Check::JumpCut).is_empty());
        // A ramped clip split in two plays on across the cut: nothing jumps, nothing is a "hard cut".
        let mut ramp_clip = Clip::new(a.id, 10.0, 16.0, 0.0);
        ramp_clip.speed_ramp = vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(4.0, 0.25)];
        let mut t = on_v1(vec![ramp_clip.clone()]);
        let (left, right) = t.split_clip(ramp_clip.id, 2.0).unwrap();
        assert!(found(&inputs(t.clone(), vec![a.clone()], vec![]), Check::JumpCut).is_empty());
        let r = review(&inputs(t, vec![a.clone()], vec![]), None, &[Check::HardAudioCut]);
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert_eq!(left.timeline_end(), right.timeline_start);
    }

    #[test]
    fn black_between_pictures_is_a_gap_and_after_the_last_a_tail() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let music = test_asset(vec![audio_stream(48_000, 2)]);
        let mut t = on_v1(vec![Clip::new(a.id, 0.0, 2.0, 0.0), Clip::new(a.id, 10.0, 12.0, 3.0)]);
        t.tracks[1].clips = vec![Clip::new(music.id, 0.0, 8.0, 0.0)];
        let r = inputs(t.clone(), vec![a.clone(), music.clone()], vec![]);
        let issues = found(&r, Check::BlackGap);
        assert_eq!(issues.len(), 2, "{issues:?}");
        assert_eq!(
            (issues[0].time, issues[0].end, issues[0].severity),
            (2.0, Some(3.0), Severity::Warning)
        );
        assert_eq!(issues[0].clip_ids.len(), 2);
        assert_eq!(
            (issues[1].time, issues[1].end, issues[1].severity),
            (5.0, Some(8.0), Severity::Tip)
        );

        // A title over the hole makes it a title card, and sound that stops with the picture leaves no tail.
        let mut titled = t.clone();
        titled.overlays.push(TextOverlay::new("Chapter two", 1.9, 3.1));
        titled.tracks[1].clips[0].source_out = 4.0;
        assert!(found(
            &inputs(titled.clone(), vec![a.clone(), music.clone()], vec![]),
            Check::BlackGap
        )
        .is_empty());
        // Float drift between two clips is not a gap.
        titled.overlays.clear();
        titled.tracks[0].clips[1].timeline_start = 2.000_4;
        assert!(found(&inputs(titled, vec![a, music.clone()], vec![]), Check::BlackGap).is_empty());

        // A cut with no picture at all has nothing to go black.
        let mut sound_only = Timeline::new();
        sound_only.tracks[1].clips = vec![Clip::new(music.id, 0.0, 8.0, 0.0)];
        assert!(found(&inputs(sound_only, vec![music], vec![]), Check::BlackGap).is_empty());
    }

    fn captioned(timeline: &mut Timeline, analyses: &[AssetAnalysis], style: CaptionStyle) {
        let transcripts = analyses.iter().map(|a| (a.asset_id, a.transcript.clone())).collect();
        timeline.overlays = timeline.captions(&transcripts, CaptionOptions::styled(style));
        assert!(!timeline.overlays.is_empty());
    }

    #[test]
    fn fresh_captions_are_not_stale_in_either_style() {
        let (a, b) = (av_asset(Uuid::new_v4(), 100.0), av_asset(Uuid::new_v4(), 100.0));
        for style in [CaptionStyle::Lines, CaptionStyle::WordPunch] {
            // A cut that clips lines at both ends of each clip, at speed, plus a reversed one.
            let mut fast = Clip::new(b.id, 0.7, 3.2, 1.75);
            fast.speed = 1.5;
            let mixed = on_v1(vec![Clip::new(a.id, 0.45, 2.2, 0.0), fast]);
            // "four five" cut out of the middle of one take, the way a silence or filler cut leaves it.
            let trimmed = on_v1(vec![Clip::new(a.id, 0.0, 1.45, 0.0), Clip::new(a.id, 2.45, 3.9, 1.45)]);
            for mut t in [mixed, trimmed] {
                let analyses = vec![speech(a.id), speech(b.id)];
                captioned(&mut t, &analyses, style);
                let r = review(
                    &inputs(t, vec![a.clone(), b.clone()], analyses),
                    None,
                    &[Check::StaleCaptions],
                );
                assert_eq!(r.checked, vec![Check::StaleCaptions]);
                assert!(r.issues.is_empty(), "{style:?}: {:?}", r.issues);
            }
        }
    }

    #[test]
    fn captions_left_behind_by_a_later_edit_are_stale() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let analyses = vec![speech(a.id)];
        let mut t = on_v1(vec![Clip::new(a.id, 0.0, 4.0, 0.0)]);
        captioned(&mut t, &analyses, CaptionStyle::WordPunch);
        // The clip moved two seconds later after captioning.
        t.tracks[0].clips[0].timeline_start = 2.0;
        let issues = found(&inputs(t.clone(), vec![a.clone()], analyses.clone()), Check::StaleCaptions);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].severity, Severity::Warning);
        assert!(issues[0].message.contains("no longer sit over"), "{}", issues[0].message);
        assert!(issues[0].fix.contains("generate_captions"));

        // Footage added after captioning has speech no caption covers.
        let mut t = on_v1(vec![Clip::new(a.id, 0.0, 2.0, 0.0)]);
        captioned(&mut t, &analyses, CaptionStyle::Lines);
        t.tracks[0].clips.push(Clip::new(a.id, 2.0, 4.0, 2.0));
        let issues = found(&inputs(t, vec![a], analyses.clone()), Check::StaleCaptions);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].message.contains("have no caption"), "{}", issues[0].message);
    }

    #[test]
    fn captions_from_elsewhere_are_not_compared() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let mut t = on_v1(vec![Clip::new(a.id, 0.0, 4.0, 0.0)]);
        let mut cue = TextOverlay::new("Bonjour tout le monde", 0.0, 2.0);
        cue.generated = true;
        t.overlays.push(cue);
        let r = review(
            &inputs(t.clone(), vec![a.clone()], vec![speech(a.id)]),
            None,
            &[Check::StaleCaptions],
        );
        assert!(r.issues.is_empty());
        assert!(r.skipped[0].reason.contains("do not come from"), "{}", r.skipped[0].reason);
        // A typed title is not a caption; with none generated there is nothing to check.
        t.overlays[0].generated = false;
        let r = review(&inputs(t, vec![a.clone()], vec![speech(a.id)]), None, &[Check::StaleCaptions]);
        assert!(r.skipped[0].reason.contains("no generated captions"));
    }

    #[test]
    fn a_range_keeps_what_falls_in_it() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let t = on_v1(vec![
            Clip::new(a.id, 10.0, 10.05, 0.0),
            Clip::new(a.id, 20.0, 25.0, 0.05),
            Clip::new(a.id, 30.0, 30.05, 5.05),
        ]);
        let r = inputs(t, vec![a], vec![]);
        assert_eq!(review(&r, None, &[Check::FlashFrame]).issues.len(), 2);
        let ranged = review(&r, Some((4.0, 6.0)), &[Check::FlashFrame]);
        assert_eq!(ranged.issues.iter().map(|i| i.time).collect::<Vec<_>>(), vec![5.05]);
        assert_eq!(ranged.counts_by_kind.get(&Check::FlashFrame), Some(&1));
    }

    #[test]
    fn checks_are_named_and_an_unknown_name_is_refused() {
        assert_eq!(Check::parse_list::<&str>(&[]).unwrap(), Check::ALL.to_vec());
        assert_eq!(
            Check::parse_list(&["jump_cut", "flash_frame", "jump_cut"]).unwrap(),
            vec![Check::FlashFrame, Check::JumpCut]
        );
        let e = Check::parse_list(&["jumpcut"]).unwrap_err().to_string();
        assert!(e.contains("jumpcut") && e.contains("stale_captions"), "{e}");
        for c in Check::ALL {
            assert_eq!(serde_json::to_value(c).unwrap(), c.name());
        }
    }

    #[test]
    fn the_answer_is_counted_by_kind_and_says_what_ran() {
        let a = av_asset(Uuid::new_v4(), 100.0);
        let r = review(
            &inputs(on_v1(vec![Clip::new(a.id, 10.0, 10.05, 0.0)]), vec![a], vec![]),
            None,
            &Check::ALL,
        );
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["counts_by_kind"]["flash_frame"], 1);
        assert_eq!(json["issues"][0]["kind"], "flash_frame");
        assert!(json["issues"][0].get("end").is_some());
        assert!(json["checked"].as_array().unwrap().contains(&"jump_cut".into()));
        let skipped: Vec<_> = json["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["check"].clone())
            .collect();
        assert!(
            skipped.contains(&"mid_word_cut".into()) && skipped.contains(&"stale_captions".into()),
            "{skipped:?}"
        );
    }
}
