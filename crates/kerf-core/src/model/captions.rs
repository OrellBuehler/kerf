//! Captions: a transcript laid out into lines and placed on the cut.

use super::*;

/// Render a transcript as a SubRip (`.srt`) subtitle document.
pub fn transcript_to_srt(segments: &[TranscriptSegment]) -> String {
    fn ts(seconds: f64) -> String {
        let s = seconds.max(0.0);
        let ms = (s * 1000.0).round() as u64;
        let (h, rem) = (ms / 3_600_000, ms % 3_600_000);
        let (m, rem) = (rem / 60_000, rem % 60_000);
        let (sec, milli) = (rem / 1000, rem % 1000);
        format!("{h:02}:{m:02}:{sec:02},{milli:03}")
    }
    let mut out = String::new();
    for (i, seg) in segments.iter().enumerate() {
        out.push_str(&format!(
            "{n}\n{start} --> {end}\n{text}\n\n",
            n = i + 1,
            start = ts(seg.start),
            end = ts(seg.end),
            text = seg.text.trim(),
        ));
    }
    out
}

/// Shortest a generated caption line is allowed to stay on screen (seconds).
/// Splitting a fast sentence strictly by character share can hand a two-letter
/// chunk a couple of frames, which reads as a flicker rather than as a word, so
/// chunks below this are merged back into a neighbour instead.
pub const MIN_CAPTION: f64 = 0.45;

/// How much of a caption line has to survive a cut for it to be kept (seconds).
/// A line whose words were trimmed away leaves a sliver of overlap at the clip
/// edge; showing it would caption footage that is no longer there.
pub const MIN_CAPTION_VISIBLE: f64 = 0.15;

/// The same two floors for [`CaptionStyle::WordPunch`], where a line *is* one
/// word. Held to [`MIN_CAPTION`] every short word would merge into a neighbour
/// and the style would collapse back into [`CaptionStyle::Lines`]; words still
/// merge — a one-letter word's character share is a couple of frames — just far
/// later.
pub const MIN_WORD_CAPTION: f64 = 0.12;
pub const MIN_WORD_VISIBLE: f64 = 0.06;

/// How long a word-timed caption line stays up after its last word when a pause follows
/// (seconds): long enough to finish reading it, short enough that the pause reads as one.
pub const CAPTION_HOLD: f64 = 0.5;

/// The shape a generated caption set takes on screen.
///
/// Two, because they are consumed differently. A subtitle line is *read*: it
/// holds still long enough to take several words in at once. The one-word form
/// is *watched* — each word lands on the beat of the speech, which is the look
/// social captions have converged on and most of why a muted feed video holds
/// attention. It is not a font choice: the word count, the size, the position
/// and the floors that stop a line flickering all move together, so it is one
/// decision rather than four.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptionStyle {
    /// A few words at a time, held as a subtitle line low in the frame.
    #[default]
    Lines,
    /// One word at a time, large and bold, cut in and out on the word.
    WordPunch,
}

impl CaptionStyle {
    /// The style's own numbers, before any per-call override.
    pub(super) fn layout(self) -> CaptionLayout {
        match self {
            Self::Lines => CaptionLayout {
                max_words: 4,
                max_chars: 28,
                pos_y: 0.88,
                size: 0.05,
                bold: false,
                min_line: MIN_CAPTION,
                min_visible: MIN_CAPTION_VISIBLE,
            },
            Self::WordPunch => CaptionLayout {
                max_words: 1,
                max_chars: 28,
                // Higher and much larger than a subtitle: one word carries the
                // whole frame, and sitting it on the bottom edge would put it
                // under the platform's own caption rail.
                pos_y: 0.72,
                size: 0.11,
                bold: true,
                min_line: MIN_WORD_CAPTION,
                min_visible: MIN_WORD_VISIBLE,
            },
        }
    }
}

/// A [`CaptionStyle`]'s numbers with any per-call override applied — what
/// [`Timeline::captions`] actually works from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptionLayout {
    /// Most words on one caption line.
    pub max_words: usize,
    /// Most characters on one caption line; the tighter of the two limits wins.
    pub max_chars: usize,
    /// Vertical position as a fraction of frame height.
    pub pos_y: f64,
    /// Font height as a fraction of frame height.
    pub size: f64,
    /// Whether the text is drawn bold.
    pub bold: bool,
    /// Shortest a line may be before it merges into a neighbour.
    pub min_line: f64,
    /// Shortest a line clipped by a cut may be before it is dropped.
    pub min_visible: f64,
}

/// How a transcript is turned into on-screen captions. Everything but the style
/// is an *override*: omit a field and it follows the style, so asking for
/// [`CaptionStyle::WordPunch`] on its own gets the whole look rather than one
/// word left at subtitle size in the subtitle position.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptionOptions {
    /// The look; defaults to [`CaptionStyle::Lines`].
    #[serde(default)]
    pub style: CaptionStyle,
    /// Most words on one caption line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_words: Option<usize>,
    /// Most characters on one caption line; the tighter of the two limits wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
    /// Vertical position as a fraction of frame height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos_y: Option<f64>,
    /// Font height as a fraction of frame height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
}

impl CaptionOptions {
    /// A style with no overrides.
    pub fn styled(style: CaptionStyle) -> Self {
        Self {
            style,
            ..Self::default()
        }
    }

    /// The numbers to caption with: the style's, with any override that is
    /// actually usable applied over them.
    pub fn resolve(self) -> CaptionLayout {
        let base = self.style.layout();
        CaptionLayout {
            max_words: self.max_words.map_or(base.max_words, |v| v.max(1)),
            max_chars: self.max_chars.map_or(base.max_chars, |v| v.max(1)),
            pos_y: overridden(self.pos_y, base.pos_y, 0.0, 1.0),
            size: overridden(self.size, base.size, 0.005, 0.5),
            ..base
        }
    }
}

/// Roughly how wide one character is as a fraction of the font size, measured
/// off `drawtext`'s default face (first DejaVu Sans, now the shipped Geist, which
/// is narrower: 0.44–0.5 for a sentence in the regular face and 0.47–0.53 in the
/// bold, against DejaVu's 0.49–0.52 and 0.56–0.6). Real caption text runs
/// 0.44–0.75 depending on the word; 0.6 sits above the 0.52–0.55 that *long* text
/// averages, and long text is the only kind that ever reaches the cap.
pub(super) const CHAR_ADVANCE: f64 = 0.6;

/// How much of the frame width a caption may take.
pub(super) const CAPTION_WIDTH: f64 = 0.9;

/// How far into an *empty* timeline imported caption cues may reach (seconds).
/// A cut has an end that cues are clipped to; with nothing on the timeline yet
/// there is none, and a file's times are then bounded by the day instead of by
/// whatever a hand-edited file says.
pub const EMPTY_CUT_WINDOW: f64 = 24.0 * 3600.0;

/// The frame captions assume when the project has not picked one. A timeline
/// cannot see its assets, so it cannot derive the footage default `export_format`
/// would use — and 16:9 is wide enough that the fit below never binds, which is
/// what keeps an unframed project captioned exactly as it was before.
const DEFAULT_CAPTION_ASPECT: f64 = 16.0 / 9.0;

/// Shrink a caption's size (a fraction of frame height) until its text fits
/// across a frame of `aspect` (width / height).
///
/// `drawtext` neither wraps nor scales: text wider than the frame is simply
/// drawn off both edges. A 9:16 frame is barely half as wide as it is tall, so
/// the social shape this whole feature is for is exactly where a long word runs
/// off — and `fontsize` cannot be an expression over `text_w`, since the width
/// is what depends on the size. So the fit is estimated here from the character
/// count, which is the only measurement available before the filter runs.
pub(super) fn fit_size(text: &str, size: f64, aspect: f64) -> f64 {
    let chars = text.chars().count().max(1) as f64;
    size.min(CAPTION_WIDTH * aspect / (chars * CHAR_ADVANCE))
}

/// Apply an optional override, ignoring one that is not a finite number and
/// clamping the rest into range.
fn overridden(v: Option<f64>, base: f64, lo: f64, hi: f64) -> f64 {
    match v {
        Some(v) if v.is_finite() => v.clamp(lo, hi),
        _ => base,
    }
}

/// Break a transcript line into caption-sized groups of words. Greedy: take
/// words until either limit would be exceeded, always at least one (a single
/// word longer than `max_chars` is its own line rather than being cut in half).
fn chunk_words(text: &str, layout: CaptionLayout) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut words = 0usize;
    for word in text.split_whitespace() {
        let extra = if current.is_empty() {
            word.chars().count()
        } else {
            word.chars().count() + 1
        };
        let fits = words < layout.max_words && current.chars().count() + extra <= layout.max_chars;
        if !current.is_empty() && !fits {
            out.push(std::mem::take(&mut current));
            words = 0;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        words += 1;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Spread `span` across `chunks` in proportion to how much text each carries,
/// then merge away any line too short to read. Character share is the honest
/// approximation for a line without word timings (an imported cue, or a
/// transcript made before words were recorded): within it the speaker is
/// assumed to be at a steady pace. A line with words goes through [`word_chunks`].
///
/// The merge is repeated — join the first too-short line to its shorter
/// neighbour, re-time everything, look again — because joining changes every
/// line's share. What that costs is the *scan*, not the bookkeeping: weights and
/// their total are kept as they change (a merge adds exactly the joining space)
/// and a scan stops at the first line that is too short, so a pass allocates
/// nothing and the text is only copied for the lines actually merged. With a
/// cue of a thousand one-word lines that is the difference between a millisecond
/// and a second — imported files make such cues possible.
pub(super) fn time_chunks(chunks: Vec<String>, span: TimeRange, min: f64) -> Vec<(TimeRange, String)> {
    let mut chunks = chunks;
    let duration = (span.end - span.start).max(0.0);
    let mut weights: Vec<f64> = chunks.iter().map(|c| c.chars().count().max(1) as f64).collect();
    let mut total: f64 = weights.iter().sum();
    // The end of chunk `i` of `n`, given where the previous one ended.
    let end_of = |weights: &[f64], total: f64, at: f64, i: usize| {
        if i + 1 == weights.len() {
            span.end
        } else {
            let share = if total > 0.0 { weights[i] / total } else { 1.0 };
            at + duration * share
        }
    };
    // A whole segment shorter than `min` is one line, not a merge loop.
    while chunks.len() >= 2 {
        let mut at = span.start;
        let mut short = None;
        for i in 0..chunks.len() {
            let end = end_of(&weights, total, at, i);
            if end - at < min {
                short = Some(i);
                break;
            }
            at = end;
        }
        let Some(i) = short else { break };
        // Merge into the shorter neighbour so the joined line stays as close to
        // the requested width as the timing allows.
        let merge_back = i > 0 && (i + 1 == chunks.len() || chunks[i - 1].chars().count() <= chunks[i + 1].chars().count());
        let into = if merge_back { i - 1 } else { i };
        let moved = chunks.remove(into + 1);
        chunks[into] = format!("{}{}{}", chunks[into], ' ', moved);
        let moved_weight = weights.remove(into + 1);
        weights[into] += moved_weight + 1.0;
        total += 1.0;
    }
    let mut timed: Vec<(TimeRange, String)> = Vec::with_capacity(chunks.len());
    let mut at = span.start;
    for (i, text) in chunks.into_iter().enumerate() {
        let end = end_of(&weights, total, at, i);
        timed.push((TimeRange { start: at, end }, text));
        at = end;
    }
    timed
}

/// Caption lines of a transcript line whose words carry their own (source) times:
/// grouped like [`chunk_words`], each line running from its first word's start to the
/// next line's — or, across a pause, to its last word's end plus [`CAPTION_HOLD`] (the
/// last line no further than the segment's end) — and a line shorter than `min` merged
/// into its shorter neighbour, as [`time_chunks`] does. Each line comes with the span its
/// words are spoken in (no hold), which is what has to survive a cut for it to show.
fn word_chunks(words: &[Word], seg_end: f64, layout: CaptionLayout) -> Vec<(TimeRange, TimeRange, String)> {
    let words: Vec<&Word> = words
        .iter()
        .filter(|w| w.start.is_finite() && w.end.is_finite() && !w.text.trim().is_empty())
        .collect();
    // Each group is the index of its first and last word.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut chars = 0usize;
    for (i, w) in words.iter().enumerate() {
        let len = w.text.trim().chars().count();
        match groups.last_mut() {
            Some((first, last)) if *last + 1 - *first < layout.max_words && chars + len < layout.max_chars => {
                *last = i;
                chars += len + 1;
            }
            _ => {
                groups.push((i, i));
                chars = len;
            }
        }
    }
    let span_of = |groups: &[(usize, usize)], g: usize| {
        let (first, last) = groups[g];
        let start = words[first].start;
        let last_end = words[last].end.max(start);
        let end = match groups.get(g + 1) {
            Some(&(next, _)) => words[next].start.min(last_end + CAPTION_HOLD),
            None => (last_end + CAPTION_HOLD).min(seg_end.max(last_end)),
        };
        TimeRange {
            start,
            end: end.max(start),
        }
    };
    let text_of = |(first, last): (usize, usize)| {
        words[first..=last]
            .iter()
            .map(|w| w.text.trim())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let chars_of = |g: (usize, usize)| text_of(g).chars().count();
    while groups.len() >= 2 {
        let Some(i) = (0..groups.len()).find(|&g| {
            let r = span_of(&groups, g);
            r.end - r.start < layout.min_line
        }) else {
            break;
        };
        let merge_back = i > 0 && (i + 1 == groups.len() || chars_of(groups[i - 1]) <= chars_of(groups[i + 1]));
        let into = if merge_back { i - 1 } else { i };
        let (_, last) = groups.remove(into + 1);
        groups[into].1 = last;
    }
    (0..groups.len())
        .map(|g| {
            let shown = span_of(&groups, g);
            let (first, last) = groups[g];
            let spoken = TimeRange {
                start: words[first].start,
                end: words[last].end.max(words[first].start),
            };
            (shown, spoken, text_of(groups[g]))
        })
        .collect()
}

/// One caption line on its way to the screen: when, what, and which input it
/// was cut from. `origin` is what lets an import say *which cue* fell outside the
/// cut or lost its slot to another — transcript captioning never reads it.
struct CaptionLine {
    range: TimeRange,
    text: String,
    origin: usize,
}

/// Chunk `text` over `span` into caption lines and keep what lies inside
/// `window`. The span is chunked *whole* and each line is clipped afterwards, so
/// a sentence that is cut in half captions only the half still in the cut — and
/// a line left with less than a readable sliver is not kept at all.
fn push_caption_lines(
    lines: &mut Vec<CaptionLine>,
    text: &str,
    span: TimeRange,
    window: (f64, f64),
    origin: usize,
    layout: CaptionLayout,
) {
    for (range, chunk) in time_chunks(chunk_words(text, layout), span, layout.min_line) {
        let start = range.start.max(window.0);
        let end = range.end.min(window.1);
        if end - start < layout.min_visible {
            continue;
        }
        lines.push(CaptionLine {
            range: TimeRange { start, end },
            text: chunk,
            origin,
        });
    }
}

/// Project timed text that is in a clip's **source** time through every clip that
/// shows its footage. `segments_for` answers which text belongs to an asset — a
/// transcript map for [`Timeline::captions`], one imported file for
/// [`Timeline::place_cues`]. `rendered` must already be [`Timeline::for_render`]
/// so a muted track and a disabled clip are as uncaptioned as they are unheard.
fn project_through_clips<'a>(
    rendered: &Timeline,
    segments_for: impl Fn(Uuid) -> Option<&'a [TranscriptSegment]>,
    layout: CaptionLayout,
) -> Vec<CaptionLine> {
    let mut lines: Vec<CaptionLine> = Vec::new();
    for (_, clip, origin, seg) in segments_in_cut(rendered, segments_for) {
        let text = seg.text.trim();
        let window = (clip.timeline_start, clip.timeline_end());
        if !seg.words.is_empty() {
            for (shown, spoken, chunk) in word_chunks(&seg.words, seg.end, layout) {
                let said = clip.source_span_to_timeline(spoken.start, spoken.end);
                let span = clip.source_span_to_timeline(shown.start, shown.end);
                let start = span.start.max(window.0);
                let end = span.end.min(window.1);
                if said.end.min(window.1) > said.start.max(window.0) && end - start >= layout.min_visible {
                    lines.push(CaptionLine {
                        range: TimeRange { start, end },
                        text: chunk,
                        origin,
                    });
                }
            }
            continue;
        }
        let span = clip.source_span_to_timeline(seg.start, seg.end);
        push_caption_lines(&mut lines, text, span, window, origin, layout);
    }
    lines
}

/// Every timed, non-empty segment that meets the source window of a clip of `rendered` showing
/// its asset, with the track and clip it reaches the cut through and its index among
/// `segments_for(asset)` — in track order, then clip order, then segment order. What
/// [`Timeline::captions`] and [`Timeline::cut_transcript`] both map through.
pub(super) fn segments_in_cut<'t, 'a>(
    rendered: &'t Timeline,
    segments_for: impl Fn(Uuid) -> Option<&'a [TranscriptSegment]>,
) -> Vec<(&'t Track, &'t Clip, usize, &'a TranscriptSegment)> {
    let mut found = Vec::new();
    for track in &rendered.tracks {
        for clip in &track.clips {
            let Some(segments) = segments_for(clip.asset_id) else {
                continue;
            };
            for (origin, seg) in segments.iter().enumerate() {
                let timed = seg.start.is_finite() && seg.end.is_finite() && seg.end > seg.start;
                if !seg.text.trim().is_empty() && timed && clip.covers_source(seg.start, seg.end) {
                    found.push((track, clip, origin, seg));
                }
            }
        }
    }
    found
}

/// How lines that start at the same moment are ordered — which of them gets the
/// slot and which waits behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimultaneousLines {
    /// Alphabetically. All a transcript has: its segments are in no meaningful
    /// order against each other, and this is what it has always done.
    ByText,
    /// In the order the input gave them. An imported file's cues are in the
    /// order its author wrote them, so two cues at one timecode keep that order
    /// instead of whichever sorts first.
    ByOrigin,
}

/// Settle chunked lines into the caption lane: ordered, de-duplicated, never two
/// on screen at once, each sized to fit the frame. Returns each overlay with the
/// `origin` of the line it came from.
fn settle_caption_lines(
    mut lines: Vec<CaptionLine>,
    layout: CaptionLayout,
    aspect: f64,
    simultaneous: SimultaneousLines,
) -> Vec<(TextOverlay, usize)> {
    lines.sort_by(|a, b| a.range.start.total_cmp(&b.range.start).then_with(|| a.text.cmp(&b.text)));
    // The same words can reach two clips — `extract_audio` leaves the picture
    // and its detached audio both referencing the asset — and drawing one
    // caption twice is drawing it bolder, not twice.
    lines.dedup_by(|a, b| a.text == b.text && (a.range.start - b.range.start).abs() < 1e-3);
    if simultaneous == SimultaneousLines::ByOrigin {
        // Stable, so everything else keeps the order it was just given.
        lines.sort_by(|a, b| a.range.start.total_cmp(&b.range.start).then_with(|| a.origin.cmp(&b.origin)));
    }
    // Captions are one lane of text at one screen position, so two at once is
    // two unreadable ones. The same footage reaching the cut twice — a
    // callback shot, or a full source parked under the edit — otherwise
    // collides with whatever is already on screen. First line in wins the
    // slot; the next starts where it ends, or is dropped if nothing readable
    // is left of it.
    let mut placed: Vec<CaptionLine> = Vec::with_capacity(lines.len());
    for line in lines {
        let start = placed
            .last()
            .map_or(line.range.start, |prev| line.range.start.max(prev.range.end));
        if line.range.end - start < layout.min_visible {
            continue;
        }
        placed.push(CaptionLine {
            range: TimeRange {
                start,
                end: line.range.end,
            },
            ..line
        });
    }
    placed
        .into_iter()
        .map(|line| {
            let size = fit_size(&line.text, layout.size, aspect);
            let mut o = TextOverlay::new(line.text, line.range.start.max(0.0), line.range.end);
            o.pos_y = layout.pos_y;
            o.size = size;
            o.bold = layout.bold;
            o.bg = Some("black@0.5".to_string());
            o.generated = true;
            (o, line.origin)
        })
        .collect()
}

/// Which clock an imported caption file's times are on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaptionTimeBase {
    /// The file was made for the finished cut: its times *are* timeline time.
    /// What a subtitle file normally is.
    #[default]
    Timeline,
    /// The file times one source — a transcript, or subtitles for the uncut
    /// footage. Each cue is projected through every clip of this asset, exactly
    /// as a transcript segment is, so it follows trims, reorders and speed.
    Source(Uuid),
}

impl CaptionTimeBase {
    /// The base a caller asked for by name (`timeline` / `source`), plus the
    /// asset a `source` base is about. Both surfaces take these two loose
    /// arguments, so the rules for combining them live in one place: an asset
    /// alone implies `source`; neither implies `timeline`; and a contradiction
    /// is an error rather than one of the two being quietly preferred.
    pub fn resolve(base: Option<&str>, asset_id: Option<Uuid>) -> Result<Self> {
        let named = base.map(|b| b.trim().to_ascii_lowercase());
        match (named.as_deref(), asset_id) {
            (None | Some("" | "timeline"), None) => Ok(Self::Timeline),
            (None | Some("" | "source"), Some(asset)) => Ok(Self::Source(asset)),
            (Some("source"), None) => Err(Error::InvalidArgument(
                "base \"source\" needs asset_id: the asset whose footage the cue times belong to".to_string(),
            )),
            (Some("timeline"), Some(_)) => Err(Error::InvalidArgument(
                "asset_id only applies to base \"source\"; cues in timeline time are not tied to an asset".to_string(),
            )),
            (Some(other), _) => Err(Error::InvalidArgument(format!(
                "unknown caption time base {other:?}; expected \"timeline\" or \"source\""
            ))),
        }
    }
}

/// Imported cues laid onto the cut: the overlays, and an account of every cue.
/// `placed + dropped_outside + dropped_short + dropped_overlap` is the number of
/// cues offered.
#[derive(Debug, Clone, Default)]
pub struct CaptionPlacement {
    pub overlays: Vec<TextOverlay>,
    /// Cues that put at least one caption on screen.
    pub placed: usize,
    /// Cues that never met the cut: past its end or before its start, timing
    /// footage no (rendered) clip shows, or not a usable cue at all.
    pub dropped_outside: usize,
    /// Cues that did meet the cut, but for a moment too short to read — their own
    /// length, or the sliver of them left inside its edge (or a clip's).
    pub dropped_short: usize,
    /// Cues that did reach the cut but lost their slot — the same words at the
    /// same moment as an earlier cue, or wholly under one that was already on
    /// screen. Captions are one lane of text.
    pub dropped_overlap: usize,
}

impl Timeline {
    /// The frame captions are fitted to: the project's own, else 16:9.
    fn caption_aspect(&self) -> f64 {
        self.format
            .map_or(DEFAULT_CAPTION_ASPECT, |d| f64::from(d.width) / f64::from(d.height))
    }

    /// Caption overlays for the cut as it currently stands.
    ///
    /// The point of doing this over the timeline rather than over an asset: a
    /// transcript is in **source** time, an overlay is in **timeline** time, and
    /// between them sit every trim, every reorder, every speed change and every
    /// silence the editor removed. Each segment is projected through the clips
    /// that actually show its footage ([`Clip::source_span_to_timeline`]), so
    /// captions land on the words that survived the cut — and words that did not
    /// survive get no caption at all.
    ///
    /// Reads through [`Timeline::for_render`], so a muted track and a disabled
    /// clip are as uncaptioned as they are unheard.
    pub fn captions(&self, transcripts: &HashMap<Uuid, Vec<TranscriptSegment>>, opts: CaptionOptions) -> Vec<TextOverlay> {
        let layout = opts.resolve();
        let rendered = self.for_render();
        let lines = project_through_clips(&rendered, |asset| transcripts.get(&asset).map(Vec::as_slice), layout);
        settle_caption_lines(lines, layout, self.caption_aspect(), SimultaneousLines::ByText)
            .into_iter()
            .map(|(overlay, _)| overlay)
            .collect()
    }

    /// Lay the cues of an imported subtitle file onto the cut as caption
    /// overlays — [`Timeline::captions`] for text that did not come from a
    /// transcript.
    ///
    /// Everything after the time mapping is shared with transcript captioning
    /// (chunking to the style's line length, the flicker floors, one lane with
    /// no two lines at once, fitting to the delivery frame), so an imported set
    /// looks and behaves like a generated one. The mapping is the only part that
    /// differs:
    ///
    /// * [`CaptionTimeBase::Source`] *is* transcript captioning — the cues are
    ///   projected through the asset's clips, honoring trim, speed, reverse and
    ///   `for_render`.
    /// * [`CaptionTimeBase::Timeline`] takes the cue times as they stand. They
    ///   are clipped to the length the cut renders at (a subtitle file for the
    ///   whole film outruns a short cut), and — unlike source time — a muted
    ///   track does not silence them: the file captions the finished cut, not
    ///   one clip's sound. With nothing on the timeline yet there is no end to
    ///   run past, so the window is [`EMPTY_CUT_WINDOW`] instead.
    ///
    /// Simultaneous cues keep the order the input gave them (transcripts, which
    /// have none, sort alphabetically). Every cue is accounted for in exactly one
    /// bucket of the result.
    pub fn place_cues(&self, cues: &[TranscriptSegment], base: CaptionTimeBase, opts: CaptionOptions) -> CaptionPlacement {
        let layout = opts.resolve();
        let rendered = self.for_render();
        let cut_end = rendered.duration();
        let window_end = if cut_end > 0.0 { cut_end } else { EMPTY_CUT_WINDOW };
        let usable = |cue: &TranscriptSegment| {
            !cue.text.trim().is_empty() && cue.start.is_finite() && cue.end.is_finite() && cue.end > cue.start
        };
        let lines = match base {
            CaptionTimeBase::Source(asset) => project_through_clips(&rendered, |id| (id == asset).then_some(cues), layout),
            CaptionTimeBase::Timeline => {
                let mut lines = Vec::new();
                for (origin, cue) in cues.iter().enumerate().filter(|(_, cue)| usable(cue)) {
                    let span = TimeRange {
                        start: cue.start,
                        end: cue.end,
                    };
                    push_caption_lines(&mut lines, cue.text.trim(), span, (0.0, window_end), origin, layout);
                }
                lines
            }
        };
        let reached: HashSet<usize> = lines.iter().map(|l| l.origin).collect();
        // A cue that produced nothing either never met the cut, or met it for too
        // short a moment to read — different complaints, so different counts.
        let source_clips: Vec<&Clip> = match base {
            CaptionTimeBase::Source(asset) => rendered
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .filter(|c| c.asset_id == asset)
                .collect(),
            CaptionTimeBase::Timeline => Vec::new(),
        };
        let meets_the_cut = |cue: &TranscriptSegment| {
            usable(cue)
                && match base {
                    CaptionTimeBase::Timeline => cue.end > 0.0 && cue.start < window_end,
                    CaptionTimeBase::Source(_) => source_clips.iter().any(|c| c.covers_source(cue.start, cue.end)),
                }
        };
        let dropped_short = cues
            .iter()
            .enumerate()
            .filter(|(origin, cue)| !reached.contains(origin) && meets_the_cut(cue))
            .count();
        let settled = settle_caption_lines(lines, layout, self.caption_aspect(), SimultaneousLines::ByOrigin);
        let kept: HashSet<usize> = settled.iter().map(|(_, origin)| *origin).collect();
        CaptionPlacement {
            placed: kept.len(),
            dropped_outside: cues.len() - reached.len() - dropped_short,
            dropped_short,
            dropped_overlap: reached.len() - kept.len(),
            overlays: settled.into_iter().map(|(overlay, _)| overlay).collect(),
        }
    }
}
