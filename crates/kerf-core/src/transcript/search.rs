//! Library-wide transcript search: where does anyone say this, and is it in the cut.
//!
//! Text is folded before it is compared ([`terms`]): lower case, diacritics and apostrophes
//! dropped, every other non-alphanumeric character a word break, and each character of a script
//! written without spaces (Han, kana, Thai, …) a word of its own. A phrase matches across word and
//! line boundaries by the words' own times ([`find_matches`]); a line without words is matched on
//! estimated ones ([`super::words_of`]). [`Project::transcript_search_inputs`] reads the cached
//! transcripts under the project lock without parsing them; [`search_transcripts`] parses and
//! searches with it released.
//!
//! [`Project::transcript_search_inputs`]: crate::Project::transcript_search_inputs

use std::borrow::Cow;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;
use unicode_script::{Script, UnicodeScript};
use uuid::Uuid;

use super::words_of;
use crate::error::{Error, Result};
use crate::model::{AnalysisKind, Timeline, TranscriptSegment, Word};

/// Hits per page when the caller does not say, and the most it may ask for.
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 500;
/// Characters of a hit's context, the match included.
pub const CONTEXT_CHARS: usize = 200;
/// Words a query may have.
pub const MAX_TERMS: usize = 64;
/// Ids of the assets without a transcript an answer names.
const MISSING_IDS: usize = 10;

/// How the words of a query must appear.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    /// The words in order, next to each other — across lines too.
    #[default]
    Phrase,
    /// Every word, in any order, within one line.
    AllWords,
    /// Any one of the words; each occurrence is a hit.
    AnyWord,
}

/// What [`search_transcripts`] looks for.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TranscriptQuery {
    pub query: String,
    /// Only these assets; `None` searches the whole library.
    #[serde(default)]
    pub asset_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    pub speaker: Option<String>,
    #[serde(default)]
    pub mode: MatchMode,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One clip of the cut that uses (part of) a hit, in timeline seconds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CutUse {
    pub clip_id: Uuid,
    pub track_id: Uuid,
    pub start: f64,
    pub end: f64,
    /// The clip holds only part of the hit; `start`/`end` are that part.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
    /// The clip is disabled: on the timeline but not played.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscriptHit {
    pub asset_id: Uuid,
    pub asset_name: String,
    /// Source seconds: from the first matched word's start to the last one's end.
    pub start: f64,
    pub end: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// The matched words as transcribed.
    pub text: String,
    /// The line(s) around the match, the match between « and », cut to ~200 characters.
    pub context: String,
    /// The words' times were spread over their line rather than reported.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub words_estimated: bool,
    /// Where the cut uses this source range; empty when it is unused.
    pub in_cut: Vec<CutUse>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MissingTranscripts {
    pub count: usize,
    /// The first few of them.
    pub asset_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscriptSearch {
    pub mode: MatchMode,
    pub total: usize,
    pub offset: usize,
    pub returned: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    /// Assets whose transcript was searched.
    pub searched_assets: usize,
    /// Assets with sound (or named in `asset_ids`) that have not been transcribed.
    pub assets_without_transcript: MissingTranscripts,
    pub hits: Vec<TranscriptHit>,
}

/// An asset as the search sees it.
#[derive(Debug, Clone)]
pub struct SearchAsset {
    pub id: Uuid,
    pub name: String,
    pub has_audio: bool,
    /// The caller asked for it by id.
    pub named: bool,
}

/// What [`search_transcripts`] needs from the project: the assets in library order, their cached
/// analysis as stored (JSON, parsed outside the lock) and the working timeline.
#[derive(Debug, Clone)]
pub struct TranscriptSearchInputs {
    pub assets: Vec<SearchAsset>,
    pub cached: HashMap<Uuid, String>,
    pub timeline: Timeline,
}

/// The part of a cached analysis a search reads.
#[derive(Deserialize)]
struct CachedTranscript {
    #[serde(default)]
    transcript: Vec<TranscriptSegment>,
    #[serde(default)]
    ran: Vec<AnalysisKind>,
}

fn is_apostrophe(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{02BC}' | '`' | '\u{00B4}' | '\u{2032}')
}

/// A script written without spaces between words: each of its characters is a term.
fn unspaced(c: char) -> bool {
    matches!(
        c.script(),
        Script::Han | Script::Hiragana | Script::Katakana | Script::Thai | Script::Lao | Script::Khmer | Script::Myanmar
    )
}

/// Diacritics are dropped from letters of these scripts; elsewhere a mark is part of the letter.
fn folds_marks(script: Script) -> bool {
    matches!(script, Script::Latin | Script::Greek | Script::Cyrillic)
}

/// Folded text: the terms, one buffer, each with the word it came from.
#[derive(Default)]
struct Folded {
    buf: String,
    terms: Vec<Term>,
}

struct Term {
    start: usize,
    end: usize,
    seg: usize,
    word: usize,
}

impl Folded {
    fn push(&mut self, text: &str, seg: usize, word: usize) {
        if text.is_ascii() {
            let mut start = self.buf.len();
            for b in text.bytes() {
                if b.is_ascii_alphanumeric() {
                    self.buf.push(b.to_ascii_lowercase() as char);
                } else if !is_apostrophe(b as char) {
                    self.close(start, seg, word);
                    start = self.buf.len();
                }
            }
            self.close(start, seg, word);
        } else {
            self.push_chars(text.nfd(), seg, word);
        }
    }

    fn push_chars(&mut self, chars: impl Iterator<Item = char>, seg: usize, word: usize) {
        let mut start = self.buf.len();
        let mut base = Script::Common;
        let mut single = false;
        for c in chars {
            if is_combining_mark(c) {
                if !folds_marks(base) && self.buf.len() > start {
                    self.buf.push(c);
                }
                continue;
            }
            if is_apostrophe(c) {
                continue;
            }
            if !c.is_alphanumeric() {
                self.close(start, seg, word);
                start = self.buf.len();
                base = Script::Common;
                continue;
            }
            let alone = unspaced(c);
            if alone || single {
                self.close(start, seg, word);
                start = self.buf.len();
            }
            single = alone;
            base = c.script();
            self.buf.extend(c.to_lowercase());
        }
        self.close(start, seg, word);
    }

    fn close(&mut self, start: usize, seg: usize, word: usize) {
        if self.buf.len() > start {
            self.terms.push(Term {
                start,
                end: self.buf.len(),
                seg,
                word,
            });
        }
    }

    fn term(&self, i: usize) -> &str {
        &self.buf[self.terms[i].start..self.terms[i].end]
    }
}

/// The terms of `text` as a search compares them.
pub fn terms(text: &str) -> Vec<String> {
    let mut f = Folded::default();
    f.push(text, 0, 0);
    (0..f.terms.len()).map(|i| f.term(i).to_string()).collect()
}

/// A match in one transcript: its first and last word as `(segment, word)`.
#[derive(Debug, Clone, PartialEq)]
pub struct TextMatch {
    pub start: f64,
    pub end: f64,
    pub speaker: Option<String>,
    pub estimated: bool,
    pub from: (usize, usize),
    pub to: (usize, usize),
}

/// The words of `from..=to` across segments.
fn span<'a>(words: &'a [Cow<'_, [Word]>], from: (usize, usize), to: (usize, usize)) -> impl Iterator<Item = &'a Word> {
    (from.0..=to.0).flat_map(move |s| {
        let ws = &words[s];
        let a = if s == from.0 { from.1 } else { 0 };
        let b = if s == to.0 { to.1 + 1 } else { ws.len() };
        ws[a.min(ws.len())..b.min(ws.len())].iter()
    })
}

/// Every place `query` (already folded, [`terms`]) is said in `segments`, in transcript order;
/// only `speaker`'s when one is named (the speaker of a match is its first word's).
pub fn find_matches(segments: &[TranscriptSegment], query: &[String], mode: MatchMode, speaker: Option<&str>) -> Vec<TextMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let words: Vec<Cow<'_, [Word]>> = segments.iter().map(words_of).collect();
    let mut f = Folded::default();
    let mut line_end = Vec::with_capacity(segments.len());
    for (s, ws) in words.iter().enumerate() {
        for (w, word) in ws.iter().enumerate() {
            f.push(&word.text, s, w);
        }
        line_end.push(f.terms.len());
    }
    let mut unique: Vec<&str> = Vec::new();
    for q in query {
        if !unique.contains(&q.as_str()) {
            unique.push(q);
        }
    }
    let at = |i: usize| (f.terms[i].seg, f.terms[i].word);
    let mut spans: Vec<((usize, usize), (usize, usize))> = Vec::new();
    match mode {
        MatchMode::Phrase => {
            let k = query.len();
            let mut i = 0;
            while i + k <= f.terms.len() {
                if (0..k).all(|j| f.term(i + j) == query[j]) {
                    spans.push((at(i), at(i + k - 1)));
                    i += k;
                } else {
                    i += 1;
                }
            }
        }
        MatchMode::AnyWord => {
            for i in 0..f.terms.len() {
                if unique.contains(&f.term(i)) && spans.last().is_none_or(|l| l.0 != at(i)) {
                    spans.push((at(i), at(i)));
                }
            }
        }
        MatchMode::AllWords => {
            let mut first = 0;
            for &last in &line_end {
                let hits: Vec<(usize, usize)> = (first..last)
                    .filter_map(|i| unique.iter().position(|q| *q == f.term(i)).map(|q| (i, q)))
                    .collect();
                first = last;
                let mut best: Option<(usize, usize)> = None;
                let mut count = vec![0usize; unique.len()];
                let (mut have, mut l) = (0, 0);
                for r in 0..hits.len() {
                    count[hits[r].1] += 1;
                    if count[hits[r].1] == 1 {
                        have += 1;
                    }
                    while have == unique.len() {
                        let (a, b) = (hits[l].0, hits[r].0);
                        if best.is_none_or(|(x, y)| b - a < y - x) {
                            best = Some((a, b));
                        }
                        count[hits[l].1] -= 1;
                        if count[hits[l].1] == 0 {
                            have -= 1;
                        }
                        l += 1;
                    }
                }
                if let Some((a, b)) = best {
                    spans.push((at(a), at(b)));
                }
            }
        }
    }
    let want = speaker.map(str::trim).filter(|s| !s.is_empty());
    spans
        .into_iter()
        .filter_map(|(from, to)| {
            let first = &words[from.0][from.1];
            let sp = first.speaker.clone().or_else(|| segments[from.0].speaker.clone());
            if want.is_some_and(|w| !sp.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(w))) {
                return None;
            }
            let (mut end, mut estimated) = (first.end, false);
            for w in span(&words, from, to) {
                end = end.max(w.end);
                estimated |= w.estimated;
            }
            Some(TextMatch {
                start: first.start,
                end,
                speaker: sp,
                estimated,
                from,
                to,
            })
        })
        .collect()
}

fn join(words: &[Word]) -> String {
    words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
}

/// `s`'s last `n` characters, cut at a space when there is one, after an ellipsis.
fn tail(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len <= n {
        return s.to_string();
    }
    let cut: String = s.chars().skip(len - n).collect();
    let cut = cut.split_once(' ').map_or(cut.as_str(), |(_, rest)| rest);
    format!("…{}", cut.trim_start())
}

/// `s`'s first `n` characters, cut at a space when there is one, before an ellipsis.
fn head(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let cut: String = s.chars().take(n).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(keep, _)| keep);
    format!("{}…", cut.trim_end())
}

/// A match's words as transcribed, and its context: the line(s) it is in with the match between
/// « and », cut to about `max` characters around it.
pub fn describe(segments: &[TranscriptSegment], m: &TextMatch, max: usize) -> (String, String) {
    let words: Vec<Cow<'_, [Word]>> = segments[m.from.0..=m.to.0].iter().map(words_of).collect();
    let (first, last) = (&words[0], &words[words.len() - 1]);
    let text = span(&words, (0, m.from.1), (m.to.0 - m.from.0, m.to.1))
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let before = join(&first[..m.from.1.min(first.len())]);
    let after = join(&last[(m.to.1 + 1).min(last.len())..]);
    let room = max.saturating_sub(text.chars().count() + 2);
    let (b, a) = (before.chars().count(), after.chars().count());
    let keep_b = b.min(room.saturating_sub(a.min(room / 2)));
    let (before, after) = (tail(&before, keep_b), head(&after, room - keep_b));
    let mut context = String::new();
    if !before.is_empty() && before != "…" {
        context.push_str(&before);
        context.push(' ');
    }
    context.push('«');
    context.push_str(&head(&text, max));
    context.push('»');
    if !after.is_empty() && after != "…" {
        context.push(' ');
        context.push_str(&after);
    }
    (text, context)
}

/// Where `timeline` uses `[start, end)` of `asset`'s source: each clip showing any of it, with
/// the part it shows in timeline seconds.
pub fn cut_uses(timeline: &Timeline, asset: Uuid, start: f64, end: f64) -> Vec<CutUse> {
    let mut out = Vec::new();
    for track in &timeline.tracks {
        for clip in track.clips.iter().filter(|c| c.asset_id == asset) {
            if !clip.covers_source(start, end) {
                continue;
            }
            let (a, b) = (start.max(clip.source_in), end.min(clip.source_out));
            let r = clip.source_span_to_timeline(a, b);
            out.push(CutUse {
                clip_id: clip.id,
                track_id: track.id,
                start: r.start,
                end: r.end,
                partial: a > start || b < end,
                disabled: !clip.enabled,
            });
        }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// The lock-free half of a transcript search: parse each asset's cached transcript, match the
/// query, and describe the page asked for (`offset` / `limit`, [`DEFAULT_LIMIT`] ≤ [`MAX_LIMIT`]).
pub fn search_transcripts(mut inputs: TranscriptSearchInputs, q: &TranscriptQuery) -> Result<TranscriptSearch> {
    let query = terms(&q.query);
    if query.is_empty() {
        return Err(Error::InvalidArgument("the query has no words to look for".to_string()));
    }
    if query.len() > MAX_TERMS {
        return Err(Error::InvalidArgument(format!(
            "the query has {} words; at most {MAX_TERMS}",
            query.len()
        )));
    }
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut searched = 0;
    let mut missing = MissingTranscripts::default();
    let mut total = 0;
    let mut hits = Vec::new();
    for asset in &inputs.assets {
        let cached = match inputs.cached.remove(&asset.id) {
            Some(json) => serde_json::from_str::<CachedTranscript>(&json)?,
            None => CachedTranscript {
                transcript: Vec::new(),
                ran: Vec::new(),
            },
        };
        if !cached.ran.contains(&AnalysisKind::Transcript) && cached.transcript.is_empty() {
            if asset.has_audio || asset.named {
                missing.count += 1;
                if missing.asset_ids.len() < MISSING_IDS {
                    missing.asset_ids.push(asset.id);
                }
            }
            continue;
        }
        searched += 1;
        let mut found = find_matches(&cached.transcript, &query, q.mode, q.speaker.as_deref());
        found.sort_by(|a, b| a.start.total_cmp(&b.start));
        let page_from = offset.saturating_sub(total).min(found.len());
        let page_to = (offset.saturating_add(limit)).saturating_sub(total).min(found.len());
        for m in &found[page_from..page_to.max(page_from)] {
            let (text, context) = describe(&cached.transcript, m, CONTEXT_CHARS);
            hits.push(TranscriptHit {
                asset_id: asset.id,
                asset_name: asset.name.clone(),
                start: m.start,
                end: m.end,
                speaker: m.speaker.clone(),
                text,
                context,
                words_estimated: m.estimated,
                in_cut: cut_uses(&inputs.timeline, asset.id, m.start, m.end),
            });
        }
        total += found.len();
    }
    let end = offset.saturating_add(hits.len());
    Ok(TranscriptSearch {
        mode: q.mode,
        total,
        offset,
        returned: hits.len(),
        next_offset: (end < total).then_some(end),
        searched_assets: searched,
        assets_without_transcript: missing,
        hits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clip, StreamKind, Track};

    fn seg(start: f64, end: f64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start,
            end,
            text: text.into(),
            ..Default::default()
        }
    }

    /// A line whose words are reported, one second each from `start`, by `speaker`.
    fn timed(start: f64, text: &str, speaker: &str) -> TranscriptSegment {
        let words: Vec<Word> = text
            .split_whitespace()
            .enumerate()
            .map(|(i, t)| Word {
                text: t.into(),
                start: start + i as f64,
                end: start + i as f64 + 0.8,
                speaker: Some(speaker.into()),
                ..Word::default()
            })
            .collect();
        TranscriptSegment {
            start,
            end: start + words.len() as f64,
            text: text.into(),
            words,
            speaker: Some(speaker.into()),
        }
    }

    fn q(s: &str) -> Vec<String> {
        terms(s)
    }

    #[test]
    fn folding_ignores_case_punctuation_apostrophes_and_accents() {
        assert_eq!(terms("Don’t  STOP, café-Crème!"), vec!["dont", "stop", "cafe", "creme"]);
        assert_eq!(terms("don't"), terms("dont"));
        assert_eq!(terms("Ünïcödé"), vec!["unicode"]);
        assert_eq!(terms("«Größe» 3.5"), vec!["große", "3", "5"]);
        assert!(terms(" — ... ").is_empty());
    }

    #[test]
    fn unspaced_scripts_are_a_term_per_character_and_keep_their_marks() {
        assert_eq!(terms("東京タワー"), vec!["東", "京", "タ", "ワ", "ー"]);
        // Katakana with a voicing mark decomposes; the mark stays with its letter.
        assert_eq!(terms("ガ").len(), 1);
        assert_eq!(terms("ガ"), terms("ガ"));
        // Devanagari vowel signs are part of the letter, not dropped.
        assert_ne!(terms("का"), terms("क"));
    }

    #[test]
    fn a_phrase_matches_across_lines_with_word_times() {
        let segs = vec![
            timed(0.0, "we went to the", "S1"),
            timed(10.0, "Lake yesterday. The lake", "S2"),
        ];
        let m = find_matches(&segs, &q("the lake"), MatchMode::Phrase, None);
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].start, m[0].end), (3.0, 10.8));
        assert_eq!((m[0].from, m[0].to), ((0, 3), (1, 0)));
        assert_eq!(m[0].speaker.as_deref(), Some("S1"));
        assert_eq!((m[1].start, m[1].end), (12.0, 13.8));
        assert!(!m[0].estimated);
        let (text, context) = describe(&segs, &m[0], CONTEXT_CHARS);
        assert_eq!(text, "the Lake");
        assert_eq!(context, "we went to «the Lake» yesterday. The lake");
    }

    #[test]
    fn a_phrase_in_a_line_without_words_is_estimated() {
        let segs = vec![seg(0.0, 2.0, "hello big world")];
        let m = find_matches(&segs, &q("BIG world"), MatchMode::Phrase, None);
        assert_eq!(m.len(), 1);
        assert!(m[0].estimated);
        assert!((m[0].start - 10.0 / 13.0).abs() < 1e-12);
        assert_eq!(m[0].end, 2.0);
    }

    #[test]
    fn all_words_takes_the_tightest_window_of_one_line() {
        let segs = vec![
            timed(0.0, "red fish blue fish and a red boat", "S1"),
            timed(20.0, "red", "S1"),
            timed(30.0, "boat", "S1"),
        ];
        let m = find_matches(&segs, &q("boat red"), MatchMode::AllWords, None);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].from, m[0].to), ((0, 6), (0, 7)));
        assert_eq!((m[0].start, m[0].end), (6.0, 7.8));
    }

    #[test]
    fn any_word_hits_every_occurrence_once_per_word() {
        let segs = vec![timed(0.0, "cat dog cat-cat bird", "S1")];
        let m = find_matches(&segs, &q("cat bird"), MatchMode::AnyWord, None);
        let at: Vec<(usize, usize)> = m.iter().map(|m| m.from).collect();
        assert_eq!(at, vec![(0, 0), (0, 2), (0, 3)]);
    }

    #[test]
    fn the_speaker_filter_goes_by_the_first_word() {
        let segs = vec![timed(0.0, "hello there", "S1"), timed(5.0, "hello again", "S2")];
        let m = find_matches(&segs, &q("hello"), MatchMode::Phrase, Some("s2"));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].start, 5.0);
        assert!(find_matches(&segs, &q("hello"), MatchMode::Phrase, Some("S9")).is_empty());
    }

    #[test]
    fn context_is_trimmed_around_the_match() {
        let long = (0..120).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
        let segs = vec![timed(0.0, &long, "S1")];
        let m = find_matches(&segs, &q("w60"), MatchMode::Phrase, None);
        let (_, context) = describe(&segs, &m[0], CONTEXT_CHARS);
        assert!(context.chars().count() <= CONTEXT_CHARS + 2, "{context}");
        assert!(context.starts_with('…') && context.ends_with('…'));
        assert!(context.contains("w59 «w60» w61"));
        let m = find_matches(&segs, &q("w1"), MatchMode::Phrase, None);
        let (_, context) = describe(&segs, &m[0], CONTEXT_CHARS);
        assert!(context.starts_with("w0 «w1» w2"), "{context}");
    }

    fn clip(asset: Uuid, source_in: f64, source_out: f64, timeline_start: f64, speed: f64) -> Clip {
        Clip {
            speed,
            ..Clip::new(asset, source_in, source_out, timeline_start)
        }
    }

    #[test]
    fn cut_uses_map_source_to_timeline_and_flag_partial_use() {
        let asset = Uuid::new_v4();
        let mut track = Track::new(StreamKind::Video, "V1");
        track.clips.push(clip(asset, 10.0, 20.0, 100.0, 1.0));
        track.clips.push(clip(asset, 14.0, 30.0, 0.0, 2.0));
        track.clips.push(clip(Uuid::new_v4(), 0.0, 50.0, 200.0, 1.0));
        let mut off = clip(asset, 40.0, 50.0, 300.0, 1.0);
        off.enabled = false;
        track.clips.push(off);
        let mut timeline = Timeline::new();
        timeline.tracks.push(track);
        let uses = cut_uses(&timeline, asset, 12.0, 16.0);
        assert_eq!(uses.len(), 2);
        assert_eq!((uses[0].start, uses[0].end, uses[0].partial), (0.0, 1.0, true));
        assert_eq!((uses[1].start, uses[1].end, uses[1].partial), (102.0, 106.0, false));
        assert!(cut_uses(&timeline, asset, 30.0, 35.0).is_empty());
        assert!(cut_uses(&timeline, asset, 41.0, 42.0)[0].disabled);
    }

    fn library(n: usize, lines: impl Fn(usize) -> Vec<TranscriptSegment>) -> TranscriptSearchInputs {
        let mut assets = Vec::new();
        let mut cached = HashMap::new();
        for i in 0..n {
            let id = Uuid::new_v4();
            assets.push(SearchAsset {
                id,
                name: format!("a{i}"),
                has_audio: true,
                named: false,
            });
            let transcript = lines(i);
            if !transcript.is_empty() {
                let json = serde_json::json!({ "asset_id": id, "silence_segments": [], "transcript": transcript });
                cached.insert(id, json.to_string());
            }
        }
        TranscriptSearchInputs {
            assets,
            cached,
            timeline: Timeline::new(),
        }
    }

    fn query(text: &str) -> TranscriptQuery {
        TranscriptQuery {
            query: text.into(),
            ..TranscriptQuery::default()
        }
    }

    #[test]
    fn a_search_pages_in_library_then_time_order_and_reports_coverage() {
        let inputs = library(4, |i| match i {
            1 => Vec::new(),
            _ => vec![timed(5.0, "say the word", "S1"), timed(0.0, "the word first", "S1")],
        });
        let ids: Vec<Uuid> = inputs.assets.iter().map(|a| a.id).collect();
        let all = search_transcripts(inputs.clone(), &query("the WORD")).unwrap();
        assert_eq!((all.total, all.returned, all.next_offset), (6, 6, None));
        assert_eq!(all.searched_assets, 3);
        assert_eq!(all.assets_without_transcript.count, 1);
        assert_eq!(all.assets_without_transcript.asset_ids, vec![ids[1]]);
        let order: Vec<(Uuid, f64)> = all.hits.iter().map(|h| (h.asset_id, h.start)).collect();
        assert_eq!(order[..2], [(ids[0], 0.0), (ids[0], 6.0)]);
        assert_eq!(order[2].0, ids[2]);
        let page = search_transcripts(
            inputs,
            &TranscriptQuery {
                offset: Some(1),
                limit: Some(2),
                ..query("the word")
            },
        )
        .unwrap();
        assert_eq!((page.total, page.returned, page.next_offset), (6, 2, Some(3)));
        assert_eq!((page.hits[0].asset_id, page.hits[1].asset_id), (ids[0], ids[2]));
    }

    #[test]
    fn an_empty_query_is_refused() {
        let err = search_transcripts(library(1, |_| Vec::new()), &query(" ?! ")).unwrap_err();
        assert!(err.to_string().contains("no words"), "{err}");
    }

    #[test]
    fn a_transcript_that_ran_and_found_nothing_counts_as_searched() {
        let mut inputs = library(1, |_| Vec::new());
        let id = inputs.assets[0].id;
        inputs
            .cached
            .insert(id, serde_json::json!({ "asset_id": id, "ran": ["transcript"] }).to_string());
        let found = search_transcripts(inputs, &query("x")).unwrap();
        assert_eq!((found.searched_assets, found.assets_without_transcript.count), (1, 0));
    }
}
