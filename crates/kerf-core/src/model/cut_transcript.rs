//! The transcript of the cut: every cached transcript mapped through the clips that play it,
//! into timeline time — what the edited cut says, to read it and cut it by its words.
//!
//! It walks the cut the way captions do ([`segments_in_cut`] over [`Timeline::for_render`]), so a
//! muted (or solo-shadowed) track and a disabled clip are as unread as they are unheard, and
//! only clips that play their own sound (`source_audio`, as the export's mix decides) are read: a
//! picture whose sound was detached says nothing, the detached audio clip says it. Each word
//! belongs to the clip that plays its **midpoint** (the source window is half-open), with its
//! times clipped to that clip — so a word split by a cut is read once, in the clip that keeps
//! most of it, and a sliver of a word a trim left behind is not read at all.

use std::collections::HashMap;

use serde::Serialize;
use uuid::Uuid;

use super::{segments_in_cut, Clip, Timeline, TranscriptSegment, Word};
use crate::transcript::words_of;

/// How close two lines of one asset must start (timeline and source seconds) to be one line
/// heard twice: a picture still playing its sound beside a clip of the same sound.
const SAME_LINE_EPS: f64 = 1e-3;

/// One transcript line as the cut plays it: the words of it one clip keeps.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CutLine {
    /// Timeline seconds: from the first kept word's start to the last one's end.
    pub start: f64,
    pub end: f64,
    /// The kept words; the line's own text when the clip keeps all of them.
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    pub clip_id: Uuid,
    pub track_id: Uuid,
    pub asset_id: Uuid,
    /// The same span in the asset's own seconds — what `cut_clip_range` takes.
    pub source_start: f64,
    pub source_end: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<CutWord>,
}

/// One word of a [`CutLine`], in timeline and in source seconds. A reversed clip plays its words
/// backwards, so they keep the line's order with their times running down.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CutWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub source_start: f64,
    pub source_end: f64,
    /// The times were spread over the line by character share rather than reported.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
}

/// `w` as `clip` plays it, if the clip plays its midpoint.
fn word_in_clip(clip: &Clip, w: &Word) -> Option<CutWord> {
    let text = w.text.trim();
    if text.is_empty() || !w.start.is_finite() || !w.end.is_finite() {
        return None;
    }
    let (lo, hi) = (w.start.min(w.end), w.start.max(w.end));
    let mid = (lo + hi) / 2.0;
    if mid < clip.source_in || mid >= clip.source_out {
        return None;
    }
    let (from, to) = (lo.max(clip.source_in), hi.min(clip.source_out));
    let at = clip.source_span_to_timeline(from, to);
    Some(CutWord {
        text: text.to_string(),
        start: at.start,
        end: at.end,
        source_start: from,
        source_end: to,
        estimated: w.estimated,
    })
}

/// The part of `seg` that `clip` plays, if any of its words.
fn line_in_clip(clip: &Clip, track_id: Uuid, seg: &TranscriptSegment) -> Option<CutLine> {
    let all = words_of(seg);
    let words: Vec<CutWord> = all.iter().filter_map(|w| word_in_clip(clip, w)).collect();
    if words.is_empty() {
        return None;
    }
    let hull = |f: fn(&CutWord) -> (f64, f64)| {
        words
            .iter()
            .map(f)
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), (a, b)| (lo.min(a), hi.max(b)))
    };
    let (start, end) = hull(|w| (w.start, w.end));
    let (source_start, source_end) = hull(|w| (w.source_start, w.source_end));
    let text = if words.len() == all.len() {
        seg.text.trim().to_string()
    } else {
        words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    };
    Some(CutLine {
        start,
        end,
        text,
        speaker: seg.speaker.clone(),
        clip_id: clip.id,
        track_id,
        asset_id: clip.asset_id,
        source_start,
        source_end,
        words,
    })
}

impl Timeline {
    /// What the cut says: each transcript line of `transcripts` (by asset, in source time) as the
    /// audible clips play it, in timeline time, ordered by start — lines of clips that overlap
    /// (several tracks at once) all come, each tagged with its clip and track; at one start, in
    /// track order. A line heard twice (the same asset playing from two clips at the same moment)
    /// comes once, from the first clip in track order. Every line carries its words
    /// (estimated ones for a line that has none); see the module docs for which words a clip keeps.
    pub fn cut_transcript(&self, transcripts: &HashMap<Uuid, Vec<TranscriptSegment>>) -> Vec<CutLine> {
        let rendered = self.for_render();
        let mut lines: Vec<CutLine> = segments_in_cut(&rendered, |asset| transcripts.get(&asset).map(Vec::as_slice))
            .into_iter()
            .filter(|(_, clip, ..)| clip.source_audio)
            .filter_map(|(track, clip, _, seg)| line_in_clip(clip, track.id, seg))
            .collect();
        lines.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut kept: Vec<CutLine> = Vec::with_capacity(lines.len());
        for line in lines {
            let heard = kept
                .iter()
                .rev()
                .take_while(|k| line.start - k.start < SAME_LINE_EPS)
                .any(|k| {
                    k.asset_id == line.asset_id
                        && (k.source_start - line.source_start).abs() < SAME_LINE_EPS
                        && k.text == line.text
                });
            if !heard {
                kept.push(line);
            }
        }
        kept
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{StreamKind, Track};

    fn timed(text: &str, words: &[(&str, f64, f64)]) -> TranscriptSegment {
        TranscriptSegment {
            start: words[0].1,
            end: words[words.len() - 1].2,
            text: text.to_string(),
            words: words
                .iter()
                .map(|(t, a, b)| Word {
                    text: t.to_string(),
                    start: *a,
                    end: *b,
                    ..Word::default()
                })
                .collect(),
            speaker: Some("S1".into()),
        }
    }

    fn timeline(tracks: Vec<(StreamKind, Vec<Clip>)>) -> Timeline {
        Timeline {
            tracks: tracks
                .into_iter()
                .enumerate()
                .map(|(i, (kind, clips))| {
                    let mut t = Track::new(kind, format!("T{i}"));
                    t.clips = clips;
                    t
                })
                .collect(),
            overlays: Vec::new(),
            svg_overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
            thumbnails: Vec::new(),
        }
    }

    fn sentence() -> TranscriptSegment {
        timed(
            "One two three four.",
            &[
                ("One", 10.0, 10.5),
                ("two", 10.5, 11.0),
                ("three", 11.0, 11.5),
                ("four.", 11.5, 12.0),
            ],
        )
    }

    fn read(t: &Timeline, asset: Uuid, segs: Vec<TranscriptSegment>) -> Vec<CutLine> {
        t.cut_transcript(&HashMap::from([(asset, segs)]))
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn a_trimmed_moved_clip_reads_in_timeline_time() {
        let asset = Uuid::new_v4();
        let clip = Clip::new(asset, 10.0, 12.0, 3.0);
        let t = timeline(vec![(StreamKind::Video, vec![clip.clone()])]);
        let lines = read(&t, asset, vec![sentence()]);
        assert_eq!(lines.len(), 1);
        let l = &lines[0];
        assert_eq!(l.text, "One two three four.");
        assert!(close(l.start, 3.0) && close(l.end, 5.0), "{l:?}");
        assert!(close(l.source_start, 10.0) && close(l.source_end, 12.0));
        assert_eq!((l.clip_id, l.asset_id, l.track_id), (clip.id, asset, t.tracks[0].id));
        assert_eq!(l.speaker.as_deref(), Some("S1"));
        assert!(close(l.words[2].start, 4.0) && close(l.words[2].source_start, 11.0));
    }

    #[test]
    fn a_word_belongs_to_the_clip_that_plays_its_midpoint() {
        let asset = Uuid::new_v4();
        // Cut at 11.2: "three" (11.0..11.5) has its midpoint 11.25 in the right half.
        let left = Clip::new(asset, 10.0, 11.2, 0.0);
        let right = Clip::new(asset, 11.2, 12.0, 1.2);
        let t = timeline(vec![(StreamKind::Audio, vec![left, right])]);
        let lines = read(&t, asset, vec![sentence()]);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["One two", "three four."]);
        // Clipped to the clip that keeps it.
        let three = &lines[1].words[0];
        assert!(close(three.source_start, 11.2) && close(three.start, 1.2), "{three:?}");
        assert!(close(lines[0].end, 1.0) && close(lines[0].source_end, 11.0));
    }

    #[test]
    fn a_sliver_of_a_word_is_not_read() {
        let asset = Uuid::new_v4();
        // Keeps 10.0..10.6: "two" (10.5..11.0) is mostly gone.
        let t = timeline(vec![(StreamKind::Audio, vec![Clip::new(asset, 10.0, 10.6, 0.0)])]);
        let lines = read(&t, asset, vec![sentence()]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "One");
    }

    #[test]
    fn speed_and_reverse_retime_the_words() {
        let asset = Uuid::new_v4();
        let mut fast = Clip::new(asset, 10.0, 12.0, 0.0);
        fast.speed = 2.0;
        let mut back = Clip::new(asset, 10.0, 12.0, 5.0);
        back.speed = -1.0;
        let t = timeline(vec![(StreamKind::Audio, vec![fast, back])]);
        let lines = read(&t, asset, vec![sentence()]);
        assert_eq!(lines.len(), 2);
        assert!(close(lines[0].start, 0.0) && close(lines[0].end, 1.0));
        assert!(close(lines[0].words[1].start, 0.25));
        // Reversed: "One" is heard last, at 6.5..7.0.
        let one = &lines[1].words[0];
        assert!(close(lines[1].start, 5.0) && close(lines[1].end, 7.0));
        assert!(close(one.start, 6.5) && close(one.end, 7.0), "{one:?}");
    }

    #[test]
    fn muted_tracks_disabled_clips_and_detached_pictures_say_nothing() {
        let asset = Uuid::new_v4();
        let mut picture = Clip::new(asset, 10.0, 12.0, 0.0);
        picture.source_audio = false;
        let sound = Clip::new(asset, 10.0, 12.0, 0.0);
        let mut off = Clip::new(asset, 10.0, 12.0, 20.0);
        off.enabled = false;
        let mut t = timeline(vec![
            (StreamKind::Video, vec![picture]),
            (StreamKind::Audio, vec![sound.clone(), off]),
            (StreamKind::Audio, vec![Clip::new(asset, 10.0, 12.0, 30.0)]),
        ]);
        t.tracks[2].muted = true;
        let lines = read(&t, asset, vec![sentence()]);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].clip_id, sound.id);
    }

    #[test]
    fn the_same_words_heard_from_two_clips_come_once() {
        let asset = Uuid::new_v4();
        let picture = Clip::new(asset, 10.0, 12.0, 0.0);
        let sound = Clip::new(asset, 10.0, 12.0, 0.0);
        let t = timeline(vec![
            (StreamKind::Video, vec![picture.clone()]),
            (StreamKind::Audio, vec![sound]),
        ]);
        let lines = read(&t, asset, vec![sentence()]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].clip_id, picture.id);
    }

    #[test]
    fn overlapping_tracks_interleave_by_start() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let t = timeline(vec![
            (StreamKind::Audio, vec![Clip::new(a, 10.0, 12.0, 1.0)]),
            (StreamKind::Audio, vec![Clip::new(b, 10.0, 12.0, 0.0)]),
        ]);
        let mut map = HashMap::from([(a, vec![sentence()])]);
        map.insert(b, vec![timed("Hi there.", &[("Hi", 10.0, 10.4), ("there.", 10.4, 11.0)])]);
        let lines = t.cut_transcript(&map);
        let read: Vec<(&str, Uuid)> = lines.iter().map(|l| (l.text.as_str(), l.track_id)).collect();
        assert_eq!(read, [("Hi there.", t.tracks[1].id), ("One two three four.", t.tracks[0].id)]);
    }

    #[test]
    fn a_line_without_words_is_cut_by_estimated_ones() {
        let asset = Uuid::new_v4();
        let seg = TranscriptSegment {
            start: 0.0,
            end: 4.0,
            text: "aaaa bbbb cccc dddd".into(),
            ..Default::default()
        };
        let t = timeline(vec![(StreamKind::Audio, vec![Clip::new(asset, 0.0, 2.0, 0.0)])]);
        let lines = read(&t, asset, vec![seg]);
        assert_eq!(lines[0].text, "aaaa bbbb");
        assert!(lines[0].words.iter().all(|w| w.estimated));
        assert!(close(lines[0].end, 2.0));
    }

    #[test]
    fn footage_outside_every_clip_and_untimed_lines_are_not_read() {
        let asset = Uuid::new_v4();
        let t = timeline(vec![(StreamKind::Audio, vec![Clip::new(asset, 0.0, 5.0, 0.0)])]);
        let blank = TranscriptSegment {
            start: 1.0,
            end: 2.0,
            text: "  ".into(),
            ..Default::default()
        };
        let untimed = TranscriptSegment {
            start: f64::NAN,
            end: 2.0,
            text: "nope".into(),
            ..Default::default()
        };
        assert!(read(&t, asset, vec![sentence(), blank, untimed]).is_empty());
        assert!(read(&t, Uuid::new_v4(), vec![sentence()]).is_empty());
    }
}
