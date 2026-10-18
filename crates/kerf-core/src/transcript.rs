//! Word-level transcript timing and speaker labels — the pure part.
//!
//! A [`TranscriptSegment`] carries its [`Word`]s when they are known. The in-process
//! whisper backend reports token timings ([`words_from_tokens`]); the ffmpeg `whisper`
//! filter reports none, so its words are spread over the line by character share and
//! marked `estimated` ([`estimate_words`]). Either way [`refine`] then tightens each word to
//! the speech energy ([`crate::engine::vad`]) and labels speakers
//! ([`crate::engine::diarize`]) from the same 16 kHz decode.

use std::borrow::Cow;

use crate::engine::{diarize, vad};
use crate::model::{TimeRange, TranscriptSegment, Word};

/// The rate [`refine`] wants its samples at.
pub const SAMPLE_RATE: u32 = crate::engine::mel::RATE;

/// `seg`'s words spread over its span in proportion to their character length, with no
/// gaps, each marked `estimated`. A line with no usable span or no words has none.
pub fn estimate_words(seg: &TranscriptSegment) -> Vec<Word> {
    let texts: Vec<&str> = seg.text.split_whitespace().collect();
    let span = seg.end - seg.start;
    if texts.is_empty() || !(seg.start.is_finite() && span.is_finite() && span > 0.0) {
        return Vec::new();
    }
    let weights: Vec<f64> = texts.iter().map(|t| t.chars().count().max(1) as f64).collect();
    let total: f64 = weights.iter().sum();
    let mut acc = 0.0;
    texts
        .iter()
        .zip(&weights)
        .enumerate()
        .map(|(i, (text, w))| {
            let start = seg.start + span * acc / total;
            acc += w;
            let end = if i + 1 == texts.len() {
                seg.end
            } else {
                seg.start + span * acc / total
            };
            Word {
                text: (*text).to_string(),
                start,
                end,
                estimated: true,
                speaker: seg.speaker.clone(),
                ..Word::default()
            }
        })
        .collect()
}

/// The words of `seg`: its own, or [`estimate_words`] for a line that has none.
pub fn words_of(seg: &TranscriptSegment) -> Cow<'_, [Word]> {
    if seg.words.is_empty() {
        Cow::Owned(estimate_words(seg))
    } else {
        Cow::Borrowed(&seg.words)
    }
}

/// Give every segment without words estimated ones.
pub fn ensure_words(segments: &mut [TranscriptSegment]) {
    for seg in segments.iter_mut().filter(|s| s.words.is_empty()) {
        seg.words = estimate_words(seg);
    }
}

/// One recognizer token, as raw bytes (a token may end mid-character), with its times in
/// seconds and its probability.
#[derive(Debug, Clone)]
pub struct TokenTiming {
    pub bytes: Vec<u8>,
    pub start: f64,
    pub end: f64,
    pub p: f32,
}

/// Assemble words from a line's text tokens (special tokens already left out): a token
/// starting with a space starts a word, any other continues one, so punctuation stays on
/// its word. A word runs from its first token's start to its last token's end, clamped to
/// `[seg_start, seg_end]` and kept in order; its confidence is the mean token probability.
pub fn words_from_tokens(tokens: &[TokenTiming], seg_start: f64, seg_end: f64) -> Vec<Word> {
    struct Acc {
        bytes: Vec<u8>,
        start: f64,
        end: f64,
        p: f32,
        n: usize,
    }
    let mut groups: Vec<Acc> = Vec::new();
    for t in tokens {
        let starts_word = t.bytes.first() == Some(&b' ') || groups.is_empty();
        match groups.last_mut() {
            Some(g) if !starts_word => {
                g.bytes.extend_from_slice(&t.bytes);
                g.end = t.end;
                g.p += t.p;
                g.n += 1;
            }
            _ => groups.push(Acc {
                bytes: t.bytes.clone(),
                start: t.start,
                end: t.end,
                p: t.p,
                n: 1,
            }),
        }
    }
    let (lo, hi) = (seg_start, seg_end.max(seg_start));
    let mut at = lo;
    let mut out = Vec::with_capacity(groups.len());
    for g in groups {
        let text = String::from_utf8_lossy(&g.bytes).trim().to_string();
        if text.is_empty() {
            continue;
        }
        let start = g.start.clamp(lo, hi).max(at);
        let end = g.end.clamp(lo, hi).max(start);
        at = start;
        out.push(Word {
            text,
            start,
            end,
            confidence: Some((g.p / g.n as f32).clamp(0.0, 1.0)),
            ..Word::default()
        });
    }
    out
}

/// Each segment's speaker: the one holding most of its word-time (a line with no labelled
/// words keeps what it has).
pub fn assign_segment_speakers(segments: &mut [TranscriptSegment]) {
    for seg in segments.iter_mut() {
        let mut time: Vec<(&str, f64)> = Vec::new();
        for w in &seg.words {
            let Some(sp) = w.speaker.as_deref() else { continue };
            let d = (w.end - w.start).max(1e-3);
            match time.iter_mut().find(|(s, _)| *s == sp) {
                Some((_, t)) => *t += d,
                None => time.push((sp, d)),
            }
        }
        if let Some((sp, _)) = time.iter().max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.cmp(a.0))) {
            seg.speaker = Some((*sp).to_string());
        }
    }
}

/// The label of speaker `index` (0-based): `S1`, `S2`, ….
pub fn speaker_label(index: usize) -> String {
    format!("S{}", index + 1)
}

/// Tighten every word to the speech energy and label speakers, from `samples` — mono at
/// [`SAMPLE_RATE`], the transcript's own time base. Lines without words get estimated
/// ones first. Returns how many speakers were found.
pub fn refine(samples: &[f32], segments: &mut [TranscriptSegment]) -> usize {
    ensure_words(segments);
    let levels = vad::Levels::of(samples);
    for seg in segments.iter_mut() {
        levels.tighten(&mut seg.words);
    }
    let mut order: Vec<(usize, usize)> = segments
        .iter()
        .enumerate()
        .flat_map(|(s, seg)| (0..seg.words.len()).map(move |w| (s, w)))
        .collect();
    order.sort_by(|a, b| segments[a.0].words[a.1].start.total_cmp(&segments[b.0].words[b.1].start));
    let spans: Vec<(f64, f64)> = order
        .iter()
        .map(|&(s, w)| (segments[s].words[w].start, segments[s].words[w].end))
        .collect();
    let (labels, speakers) = diarize::diarize(samples, &spans, &diarize::Params::default());
    for (&(s, w), label) in order.iter().zip(labels) {
        segments[s].words[w].speaker = Some(speaker_label(label));
    }
    assign_segment_speakers(segments);
    speakers
}

/// One speaker's stretches of an asset, in source time.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct SpeakerRanges {
    pub speaker: String,
    /// When they speak, in source seconds: their words, joined across pauses shorter than
    /// the gap asked for.
    pub ranges: Vec<TimeRange>,
    /// Seconds of their words.
    pub speaking: f64,
}

/// Every labelled speaker's ranges (only `speaker`'s when given), by first appearance.
/// Words of one speaker less than `join_gap` apart are one range.
pub fn speaker_ranges(segments: &[TranscriptSegment], speaker: Option<&str>, join_gap: f64) -> Vec<SpeakerRanges> {
    let mut words: Vec<(String, f64, f64)> = Vec::new();
    for seg in segments {
        for w in words_of(seg).iter() {
            let Some(sp) = w.speaker.clone().or_else(|| seg.speaker.clone()) else {
                continue;
            };
            if speaker.is_some_and(|want| !want.eq_ignore_ascii_case(&sp)) || w.end <= w.start {
                continue;
            }
            words.push((sp, w.start, w.end));
        }
    }
    words.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut out: Vec<SpeakerRanges> = Vec::new();
    for (sp, start, end) in words {
        let i = match out.iter().position(|r| r.speaker == sp) {
            Some(i) => i,
            None => {
                out.push(SpeakerRanges {
                    speaker: sp,
                    ranges: Vec::new(),
                    speaking: 0.0,
                });
                out.len() - 1
            }
        };
        let entry = &mut out[i];
        entry.speaking += end - start;
        match entry.ranges.last_mut() {
            Some(last) if start - last.end < join_gap => last.end = last.end.max(end),
            _ => entry.ranges.push(TimeRange { start, end }),
        }
    }
    out
}

/// Filler words matched when the caller names none.
pub const DEFAULT_FILLERS: &[&str] = &["um", "uh", "umm", "uhm", "erm", "er", "ah", "hmm", "mm", "you know", "i mean"];

/// A filler found in a transcript, in source time.
#[derive(Debug, Clone, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct FillerWord {
    /// The words as transcribed.
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// Index of the segment it is in.
    pub segment: usize,
    /// Its times were estimated rather than reported by the recognizer.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
}

/// A word as a filler list compares it: lower case, no surrounding punctuation.
fn normalize(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// The fillers (`fillers`, else [`DEFAULT_FILLERS`]) in `segments`, by word bounds. A filler
/// may be a phrase ("you know"), matched across consecutive words of one line.
pub fn filler_words(segments: &[TranscriptSegment], fillers: &[String]) -> Vec<FillerWord> {
    let phrases: Vec<Vec<String>> = if fillers.is_empty() {
        DEFAULT_FILLERS
            .iter()
            .map(|f| f.split_whitespace().map(normalize).collect())
            .collect()
    } else {
        fillers
            .iter()
            .map(|f| f.split_whitespace().map(normalize).collect())
            .collect()
    };
    let phrases: Vec<Vec<String>> = phrases
        .into_iter()
        .filter(|p: &Vec<String>| !p.is_empty() && p.iter().all(|w| !w.is_empty()))
        .collect();
    let mut out = Vec::new();
    for (si, seg) in segments.iter().enumerate() {
        let words = words_of(seg);
        let norm: Vec<String> = words.iter().map(|w| normalize(&w.text)).collect();
        let mut i = 0;
        while i < words.len() {
            let hit = phrases
                .iter()
                .filter(|p| norm.len() >= i + p.len() && norm[i..i + p.len()] == p[..])
                .map(Vec::len)
                .max();
            match hit {
                Some(len) => {
                    let span = &words[i..i + len];
                    out.push(FillerWord {
                        text: span.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "),
                        start: span[0].start,
                        end: span[len - 1].end,
                        segment: si,
                        estimated: span.iter().any(|w| w.estimated),
                    });
                    i += len;
                }
                None => i += 1,
            }
        }
    }
    out
}

/// `segments` without their words, for a reader that wants the lines alone.
pub fn without_words(segments: &[TranscriptSegment]) -> Vec<TranscriptSegment> {
    segments
        .iter()
        .map(|s| TranscriptSegment {
            words: Vec::new(),
            ..s.clone()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start,
            end,
            text: text.into(),
            ..Default::default()
        }
    }

    fn word(text: &str, start: f64, end: f64, speaker: &str) -> Word {
        Word {
            text: text.into(),
            start,
            end,
            speaker: Some(speaker.into()),
            ..Word::default()
        }
    }

    #[test]
    fn estimated_words_share_the_line_by_character_length() {
        let words = estimate_words(&seg(1.0, 3.0, "a bbb"));
        assert_eq!(words.len(), 2);
        assert!(words.iter().all(|w| w.estimated));
        assert_eq!((words[0].text.as_str(), words[0].start, words[0].end), ("a", 1.0, 1.5));
        assert_eq!((words[1].text.as_str(), words[1].start, words[1].end), ("bbb", 1.5, 3.0));
    }

    #[test]
    fn no_span_or_no_text_estimates_nothing() {
        assert!(estimate_words(&seg(2.0, 2.0, "hi")).is_empty());
        assert!(estimate_words(&seg(0.0, 1.0, "   ")).is_empty());
        assert!(estimate_words(&seg(f64::NAN, 1.0, "hi")).is_empty());
    }

    #[test]
    fn ensure_words_keeps_real_ones() {
        let mut segs = vec![seg(0.0, 1.0, "one two"), seg(1.0, 2.0, "three")];
        segs[1].words = vec![word("three", 1.2, 1.6, "S1")];
        ensure_words(&mut segs);
        assert_eq!(segs[0].words.len(), 2);
        assert_eq!(segs[1].words[0].start, 1.2);
        assert!(!segs[1].words[0].estimated);
    }

    #[test]
    fn tokens_join_into_words_on_leading_spaces() {
        let t = |s: &str, a: f64, b: f64| TokenTiming {
            bytes: s.as_bytes().to_vec(),
            start: a,
            end: b,
            p: 0.5,
        };
        let words = words_from_tokens(
            &[
                t(" Ho", 0.0, 0.2),
                t("use", 0.2, 0.4),
                t(",", 0.4, 0.4),
                t(" world", 0.5, 0.9),
                t(" x", 2.0, 9.0),
            ],
            0.1,
            1.5,
        );
        let got: Vec<(&str, f64, f64)> = words.iter().map(|w| (w.text.as_str(), w.start, w.end)).collect();
        assert_eq!(got, vec![("House,", 0.1, 0.4), ("world", 0.5, 0.9), ("x", 1.5, 1.5)]);
        assert_eq!(words[0].confidence, Some(0.5));
        assert!(words.iter().all(|w| !w.estimated));
    }

    #[test]
    fn a_character_split_across_tokens_survives() {
        let bytes = "ï".as_bytes();
        let t = |b: &[u8]| TokenTiming {
            bytes: b.to_vec(),
            start: 0.0,
            end: 0.1,
            p: 1.0,
        };
        let words = words_from_tokens(&[t(b" na"), t(&bytes[..1]), t(&bytes[1..]), t(b"ve")], 0.0, 1.0);
        assert_eq!(words[0].text, "naïve");
    }

    #[test]
    fn the_segment_speaker_is_the_one_with_most_time() {
        let mut segs = vec![seg(0.0, 3.0, "a b c")];
        segs[0].words = vec![
            word("a", 0.0, 0.5, "S2"),
            word("b", 0.5, 1.0, "S2"),
            word("c", 1.0, 3.0, "S1"),
        ];
        assign_segment_speakers(&mut segs);
        assert_eq!(segs[0].speaker.as_deref(), Some("S1"));
    }

    #[test]
    fn speaker_ranges_join_short_pauses_and_filter() {
        let mut a = seg(0.0, 4.0, "a b c d");
        a.words = vec![
            word("a", 0.0, 0.5, "S1"),
            word("b", 0.6, 1.0, "S1"),
            word("c", 1.2, 2.0, "S2"),
            word("d", 3.0, 3.5, "S1"),
        ];
        let all = speaker_ranges(&[a.clone()], None, 0.5);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].speaker, "S1");
        let r: Vec<(f64, f64)> = all[0].ranges.iter().map(|r| (r.start, r.end)).collect();
        assert_eq!(r, vec![(0.0, 1.0), (3.0, 3.5)]);
        assert!((all[0].speaking - 1.4).abs() < 1e-9);
        let s2 = speaker_ranges(&[a], Some("s2"), 0.5);
        assert_eq!(s2.len(), 1);
        assert_eq!((s2[0].ranges[0].start, s2[0].ranges[0].end), (1.2, 2.0));
    }

    #[test]
    fn a_segment_speaker_labels_its_unlabelled_words() {
        let mut a = seg(0.0, 2.0, "one two");
        a.speaker = Some("S3".into());
        let r = speaker_ranges(&[a], None, 0.1);
        assert_eq!(r[0].speaker, "S3");
        assert_eq!((r[0].ranges[0].start, r[0].ranges[0].end), (0.0, 2.0));
    }

    #[test]
    fn fillers_are_found_by_word_bounds_and_phrases() {
        let mut a = seg(0.0, 4.0, "So, um, you know it works.");
        a.words = vec![
            word("So,", 0.0, 0.3, "S1"),
            word("um,", 0.5, 0.8, "S1"),
            word("you", 1.0, 1.2, "S1"),
            word("know", 1.2, 1.5, "S1"),
            word("it", 2.0, 2.2, "S1"),
            word("works.", 2.2, 2.8, "S1"),
        ];
        let found = filler_words(&[a.clone()], &[]);
        let got: Vec<(&str, f64, f64)> = found.iter().map(|f| (f.text.as_str(), f.start, f.end)).collect();
        assert_eq!(got, vec![("um,", 0.5, 0.8), ("you know", 1.0, 1.5)]);
        let only = filler_words(&[a], &["UM".to_string()]);
        assert_eq!(only.len(), 1);
        assert!(!only[0].estimated);
    }

    #[test]
    fn fillers_in_a_line_without_words_are_estimated() {
        let found = filler_words(&[seg(0.0, 1.0, "uh hello")], &[]);
        assert_eq!(found.len(), 1);
        assert!(found[0].estimated);
        assert_eq!(found[0].start, 0.0);
        assert!((found[0].end - 2.0 / 7.0).abs() < 1e-12);
    }

    #[test]
    fn old_transcripts_load_and_serialize_unchanged() {
        let json = r#"{"start":0.0,"end":1.0,"text":"hi"}"#;
        let s: TranscriptSegment = serde_json::from_str(json).unwrap();
        assert!(s.words.is_empty() && s.speaker.is_none());
        assert_eq!(serde_json::to_string(&s).unwrap(), json);
    }

    #[test]
    fn refine_labels_every_word_and_line() {
        // One steady voice-ish tone with gaps; one speaker, words tightened inside the line.
        let mut samples = vec![0.0001f32; 3 * 16_000];
        for (i, s) in samples.iter_mut().enumerate() {
            let t = i as f32 / 16_000.0;
            if (0.5..1.0).contains(&t) || (1.5..2.5).contains(&t) {
                *s = 0.3 * (2.0 * std::f32::consts::PI * 220.0 * t).sin();
            }
        }
        let mut segs = vec![seg(0.0, 3.0, "hello there")];
        let n = refine(&samples, &mut segs);
        assert_eq!(n, 1);
        assert!(segs[0]
            .words
            .iter()
            .all(|w| w.speaker.as_deref() == Some("S1") && w.estimated));
        assert_eq!(segs[0].speaker.as_deref(), Some("S1"));
        assert!(segs[0].words[0].start >= 0.5 - 1e-9, "{:?}", segs[0].words);
        assert!(segs[0].words[1].end <= 2.5 + 1e-9, "{:?}", segs[0].words);
    }
}
