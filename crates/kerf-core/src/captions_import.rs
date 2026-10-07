//! Reading subtitle files into timed cues — the front half of caption import.
//!
//! Everything here is pure: text in, [`ParsedCaptions`] out, in the file's *own*
//! clock. Putting those cues on the cut (which clock they are on, chunking them
//! to a readable line, fitting them to the frame) is [`Timeline::place_cues`],
//! which reuses the transcript-captioning code so an imported set looks like a
//! generated one; [`Project::import_captions`] makes it one revision.
//!
//! Two formats, both read *tolerantly* — subtitle files in the wild are mostly
//! hand-edited, re-muxed and machine-converted, and an importer that rejects a
//! file for a missing blank line is an importer nobody uses:
//!
//! * **SubRip** (`.srt`): a BOM, CRLF or old-Mac CR line ends, missing or absurd
//!   indices, `,` or `.` before the milliseconds, several text lines, `<i>` /
//!   `<b>` / `<font>` markup, and `{\an8}`-style overrides some tools leave in.
//! * **ASS / SSA** (`.ass`, `.ssa`): the `[Events]` `Format:` line decides which
//!   column is which (so reordered fields work), `Dialogue:` lines carry the
//!   cues, and `{…}` override blocks, drawing commands, `\N` / `\n` / `\h` are
//!   resolved. `Comment:` lines, styles and positioning are not imported — where
//!   a caption sits is the caption *style's* decision, not the file's.
//!
//! What cannot be read is skipped and **counted**, never fatal: a cue whose time
//! cannot be read, one with no text or no duration, a stray line outside any cue.
//!
//! [`Timeline::place_cues`]: crate::model::Timeline::place_cues
//! [`Project::import_captions`]: crate::project::Project::import_captions

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The largest subtitle file Kerf will read. A feature film's SRT is ~100 KB;
/// the path comes from a picker (or an agent) that could aim at anything.
pub const MAX_CAPTION_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// The most cues one import may carry. A three-hour film has ~3,500; the whole
/// timeline is one JSON blob rewritten on every edit, so an unbounded set is a
/// tax on every later operation.
pub const MAX_CAPTION_CUES: usize = 10_000;

/// The longest a single cue's text may be (characters). Real cues are a few dozen;
/// a longer one is a broken file, and chunking one into thousands of one-word
/// lines is where the merge pass in the line timer would crawl.
pub const MAX_CUE_CHARS: usize = 2_000;

/// A subtitle format Kerf can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptionFormat {
    /// SubRip (`.srt`).
    Srt,
    /// Advanced / classic SubStation Alpha (`.ass`, `.ssa`).
    Ass,
}

impl CaptionFormat {
    /// The wire name (`srt` / `ass`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Ass => "ass",
        }
    }

    /// A format by its name or file extension (`srt`, `ass`, `ssa`; any case,
    /// with or without a leading dot).
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
            "srt" | "subrip" => Some(Self::Srt),
            "ass" | "ssa" => Some(Self::Ass),
            _ => None,
        }
    }

    /// The optional `format` argument both surfaces take: absent (or blank) is
    /// "guess from the text", anything else must name a format we read.
    pub fn from_arg(name: Option<&str>) -> Result<Option<Self>> {
        match name.map(str::trim).filter(|n| !n.is_empty()) {
            None => Ok(None),
            Some(n) => Self::parse(n).map(Some).ok_or_else(|| {
                Error::InvalidArgument(format!(
                    "unknown subtitle format {n:?}; expected \"srt\" or \"ass\" (\"ssa\" is read as ass)"
                ))
            }),
        }
    }

    /// The format a path's extension names.
    pub fn from_extension(path: &Path) -> Option<Self> {
        path.extension().and_then(|e| e.to_str()).and_then(Self::parse)
    }

    /// Guess a format from the text itself, for a caller that has no file name
    /// (the browser harness) or whose file name lies. An ASS / SSA file always
    /// has a `[Script Info]` or `[Events]` section; failing that, the `-->` of an
    /// SRT timing line wins over a bare `Dialogue:` line, which is the only
    /// thing a stripped ASS fragment has and also something a subtitle could say.
    pub fn detect(text: &str) -> Self {
        let mut arrow = false;
        let mut dialogue = false;
        for line in text.trim_start_matches('\u{feff}').lines() {
            let line = line.trim();
            if line.eq_ignore_ascii_case("[script info]") || line.eq_ignore_ascii_case("[events]") {
                return Self::Ass;
            }
            arrow |= line.contains("-->");
            dialogue |= line.len() >= 9 && line.as_bytes()[..9].eq_ignore_ascii_case(b"dialogue:");
        }
        if dialogue && !arrow {
            Self::Ass
        } else {
            Self::Srt
        }
    }
}

/// One cue of a subtitle file, in the **file's own time** (seconds).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImportedCue {
    pub start: f64,
    pub end: f64,
    /// The cue's words with markup removed; a line break in the file is `\n`.
    pub text: String,
}

/// What reading a subtitle file produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedCaptions {
    /// Usable cues, ordered by start time (a file's own order is not trusted:
    /// ASS dialogue is routinely grouped by style or layer).
    pub cues: Vec<ImportedCue>,
    /// Entries that could not be used: a cue with an unreadable time, no text,
    /// or no duration; an ASS `Dialogue:` line with too few fields; a stray line
    /// outside any cue.
    pub skipped: usize,
}

// ---- timestamps -------------------------------------------------------------

/// `[h:]m:s` with an optional `,` or `.` fraction, in seconds. The fraction is a
/// *decimal fraction* whatever its width — SRT's `,5`, `,50` and `,500` all mean
/// half a second, and ASS writes centiseconds the same way. Negative times,
/// anything non-numeric and absurdly long fields are not times.
fn parse_timestamp(token: &str) -> Option<f64> {
    let token = token.trim();
    if token.is_empty() || token.len() > 40 {
        return None;
    }
    let (clock, frac) = match token.find([',', '.']) {
        Some(i) => (&token[..i], &token[i + 1..]),
        None => (token, ""),
    };
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut seconds = 0.0_f64;
    let mut fields = 0;
    for part in clock.split(':') {
        if part.is_empty() || part.len() > 6 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        seconds = seconds * 60.0 + f64::from(part.parse::<u32>().ok()?);
        fields += 1;
    }
    if !(2..=3).contains(&fields) {
        return None;
    }
    if !frac.is_empty() {
        seconds += format!("0.{}", &frac[..frac.len().min(9)]).parse::<f64>().ok()?;
    }
    seconds.is_finite().then_some(seconds)
}

// ---- text cleanup -----------------------------------------------------------

/// Tags SubRip files carry (and WebVTT's cousins). Anything else in angle
/// brackets — `<laughs>`, `<3`, `a < b` — is the author's text, not markup.
const MARKUP_TAGS: &[&str] = &[
    "a", "b", "br", "c", "div", "em", "font", "i", "lang", "p", "rp", "rt", "ruby", "s", "span", "strike", "strong", "u", "v",
];

/// Remove `<i>` / `</font>`-style markup from one line; `<br>` becomes a line
/// break. Angle brackets that are not a known tag are kept.
fn strip_markup(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let tag = after.find('>').filter(|&close| close <= 256).map(|close| &after[..close]);
        let name = tag.map(|t| {
            t.trim_start_matches('/')
                .split(|c: char| !c.is_ascii_alphanumeric())
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
        });
        match (tag, name) {
            (Some(inner), Some(name)) if !inner.contains('<') && MARKUP_TAGS.contains(&name.as_str()) => {
                if name == "br" {
                    out.push('\n');
                }
                rest = &after[inner.len() + 1..];
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Remove `{…}` blocks. With `only_overrides`, only the ones that start `{\` —
/// SubRip's own `{laughs}` is text, an ASS `{comment}` is not. An unterminated
/// `{` is literal text.
fn strip_braces(line: &str, only_overrides: bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if !only_overrides || after.starts_with('\\') => rest = &after[close + 1..],
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Tidy one text line: collapse runs of whitespace (including the no-break
/// spaces tools like to leave) to single spaces.
fn tidy(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A cue's lines cleaned and joined, or empty when nothing is left to show.
fn join_lines<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    lines
        .flat_map(str::lines)
        .map(tidy)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- SubRip -----------------------------------------------------------------

fn normalize_newlines(text: &str) -> String {
    text.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n")
}

/// An SRT index line: digits and nothing else, however many.
fn is_index(line: &str) -> bool {
    !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit())
}

/// `00:00:01,000 --> 00:00:03,500 [positioning]` → `(start, end)`.
fn parse_srt_timing(line: &str) -> Option<(f64, f64)> {
    let (left, right) = line.split_once("-->")?;
    // `--->` is a typo that turns up; the extra dash lands on the left.
    let start = parse_timestamp(left.trim().trim_end_matches('-'))?;
    let end = parse_timestamp(right.split_whitespace().next()?)?;
    Some((start, end))
}

/// A cue being collected: its time if that could be read, and its text lines.
struct Block<'a> {
    time: Option<(f64, f64)>,
    body: Vec<&'a str>,
}

/// Read SubRip text. Never fails: what is unusable is counted in
/// [`ParsedCaptions::skipped`].
///
/// A cue starts at a line with `-->` and runs to the next blank line *or* the
/// next timing line, whichever comes first — a file with no blank lines between
/// cues is common, and the index line that then sits at the end of the previous
/// cue's text is handed back to the cue it belongs to.
pub fn parse_srt(text: &str) -> ParsedCaptions {
    let text = normalize_newlines(text);
    let mut out = ParsedCaptions::default();

    fn finish(block: Block<'_>, out: &mut ParsedCaptions) {
        let Some((start, end)) = block.time else {
            out.skipped += 1;
            return;
        };
        let cleaned = block
            .body
            .iter()
            .map(|l| strip_braces(&strip_markup(l), true))
            .collect::<Vec<_>>();
        let text = join_lines(cleaned.iter().map(String::as_str));
        if text.is_empty() || end <= start || text.chars().count() > MAX_CUE_CHARS {
            out.skipped += 1;
            return;
        }
        out.cues.push(ImportedCue { start, end, text });
    }

    let mut current: Option<Block<'_>> = None;
    for raw in text.split('\n') {
        let line = raw.trim();
        if line.contains("-->") {
            let time = parse_srt_timing(line);
            // A `-->` line that does not parse, inside a cue that did, is that
            // cue's text ("a --> b"), not the start of a broken one.
            let inside_good_cue = current.as_ref().is_some_and(|b| b.time.is_some());
            if time.is_some() || !inside_good_cue {
                if let Some(mut previous) = current.take() {
                    if previous.body.last().is_some_and(|l| is_index(l)) {
                        previous.body.pop();
                    }
                    finish(previous, &mut out);
                }
                current = Some(Block { time, body: Vec::new() });
                continue;
            }
        }
        if line.is_empty() {
            if let Some(block) = current.take() {
                finish(block, &mut out);
            }
            continue;
        }
        match current.as_mut() {
            Some(block) => block.body.push(line),
            // Outside any cue only an index is expected.
            None if is_index(line) => {}
            None => out.skipped += 1,
        }
    }
    if let Some(block) = current.take() {
        finish(block, &mut out);
    }
    out.cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

// ---- ASS / SSA --------------------------------------------------------------

/// Which column of a `Dialogue:` line is which, from the `[Events]` `Format:`.
#[derive(Debug, Clone, Copy)]
struct EventColumns {
    fields: usize,
    start: usize,
    end: usize,
    text: usize,
}

impl EventColumns {
    /// What a file with no `Format:` line is read as. ASS (`Layer, Start, End,
    /// Style, Name, MarginL, MarginR, MarginV, Effect, Text`) and SSA v4
    /// (`Marked` for `Layer`) agree on where the three columns we need are.
    const DEFAULT: Self = Self {
        fields: 10,
        start: 1,
        end: 2,
        text: 9,
    };

    fn from_format(value: &str) -> Result<Self> {
        let names: Vec<String> = value.split(',').map(|n| n.trim().to_ascii_lowercase()).collect();
        let find = |name: &str| names.iter().position(|n| n == name);
        match (find("start"), find("end"), find("text")) {
            (Some(start), Some(end), Some(text)) => Ok(Self {
                fields: names.len(),
                start,
                end,
                text,
            }),
            _ => Err(Error::InvalidArgument(
                "the [Events] Format line of this ASS file has no Start, End and Text column".to_string(),
            )),
        }
    }
}

/// The visible text of an ASS `Text` field: override blocks and vector drawings
/// removed, `\N` / `\n` line breaks, `\h` a space.
fn clean_ass_text(raw: &str) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut drawing = false;
    let mut rest = raw;
    while let Some(open) = rest.find('{') {
        if !drawing {
            text.push_str(&rest[..open]);
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            // Unterminated: libass shows it, so it is text.
            if !drawing {
                text.push_str(&rest[open..]);
            }
            rest = "";
            break;
        };
        // `\p1` starts a vector drawing — its "text" is path commands — and
        // `\p0` ends it. `\pos` and `\pbo` share the prefix and are not that.
        for tag in after[..close].split('\\').skip(1) {
            if let Some(level) = tag.strip_prefix('p') {
                if !level.is_empty() && level.bytes().all(|b| b.is_ascii_digit()) {
                    drawing = !level.trim_start_matches('0').is_empty();
                }
            }
        }
        rest = &after[close + 1..];
    }
    if !drawing {
        text.push_str(rest);
    }

    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\\', Some('N' | 'n')) => {
                chars.next();
                out.push('\n');
            }
            ('\\', Some('h')) => {
                chars.next();
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    join_lines(std::iter::once(out.as_str()))
}

/// Read ASS / SSA text.
///
/// Only `Dialogue:` lines inside `[Events]` are cues (a headerless fragment of
/// them is read too); `Comment:` lines, styles, `Picture:` / `Sound:` / `Movie:`
/// / `Command:` events and `;` comments are ignored without being counted — they
/// are not malformed, they are not captions. A `Dialogue:` line that cannot be
/// read is [`skipped`](ParsedCaptions::skipped).
///
/// Errors only when the file's own `Format:` line leaves out Start, End or Text,
/// since no column can then be trusted.
pub fn parse_ass(text: &str) -> Result<ParsedCaptions> {
    let text = normalize_newlines(text);
    let mut out = ParsedCaptions::default();
    let mut section: Option<String> = None;
    let mut columns = EventColumns::DEFAULT;
    for raw in text.split('\n') {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('!') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = Some(line[1..line.len() - 1].trim().to_ascii_lowercase());
            continue;
        }
        // `Format:` also heads the styles section; only the events one counts.
        if !matches!(section.as_deref(), None | Some("events")) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "format" => columns = EventColumns::from_format(value)?,
            "dialogue" => {
                let fields: Vec<&str> = value.trim_start().splitn(columns.fields, ',').collect();
                if fields.len() < columns.fields {
                    out.skipped += 1;
                    continue;
                }
                let start = parse_timestamp(fields[columns.start]);
                let end = parse_timestamp(fields[columns.end]);
                let text = clean_ass_text(fields[columns.text]);
                match (start, end) {
                    (Some(start), Some(end)) if end > start && !text.is_empty() && text.chars().count() <= MAX_CUE_CHARS => {
                        out.cues.push(ImportedCue { start, end, text });
                    }
                    _ => out.skipped += 1,
                }
            }
            _ => {}
        }
    }
    out.cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    Ok(out)
}

// ---- entry points -----------------------------------------------------------

/// Read subtitle text in `format`, or in whichever format it looks like when
/// `format` is `None`. Returns the format used.
///
/// Errors on text over [`MAX_CAPTION_FILE_BYTES`] (the guard for callers that
/// hand over text rather than a file), on more than [`MAX_CAPTION_CUES`] cues,
/// and on an ASS file whose `Format:` line is unusable; an unreadable *entry* is
/// never an error.
pub fn parse_captions(text: &str, format: Option<CaptionFormat>) -> Result<(CaptionFormat, ParsedCaptions)> {
    if text.len() as u64 > MAX_CAPTION_FILE_BYTES {
        return Err(too_large());
    }
    let format = format.unwrap_or_else(|| CaptionFormat::detect(text));
    let parsed = match format {
        CaptionFormat::Srt => parse_srt(text),
        CaptionFormat::Ass => parse_ass(text)?,
    };
    if parsed.cues.len() > MAX_CAPTION_CUES {
        return Err(Error::InvalidArgument(format!(
            "this file has {} cues; Kerf imports at most {MAX_CAPTION_CUES} at a time",
            parsed.cues.len()
        )));
    }
    Ok((format, parsed))
}

/// Windows-1252 for the bytes 0x80..=0x9F, where it differs from Latin-1 (which
/// has invisible control characters there). Bytes it leaves undefined map to the
/// control character Latin-1 would give them.
const WINDOWS_1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{02C6}', '\u{2030}',
    '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}', '\u{017D}', '\u{8F}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}',
    '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{9D}', '\u{017E}',
    '\u{0178}',
];

/// Decode a subtitle file's bytes to text.
///
/// A UTF-8 or UTF-16 byte-order mark says which; with none, the bytes are UTF-8
/// if they are valid UTF-8 and otherwise Latin-1 (read as Windows-1252, which
/// agrees with it everywhere but the 0x80–0x9F block and is what an "ANSI" file
/// from a Windows tool actually is). A file over [`MAX_CAPTION_FILE_BYTES`] is
/// refused.
pub fn decode_caption_bytes(bytes: &[u8]) -> Result<String> {
    if bytes.len() as u64 > MAX_CAPTION_FILE_BYTES {
        return Err(too_large());
    }
    let utf16 = |rest: &[u8], big_endian: bool| {
        // An odd trailing byte is half a code unit; it is dropped.
        let (pairs, _) = rest.as_chunks::<2>();
        let units: Vec<u16> = pairs
            .iter()
            .map(|&p| {
                if big_endian {
                    u16::from_be_bytes(p)
                } else {
                    u16::from_le_bytes(p)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Ok(String::from_utf8_lossy(rest).into_owned());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Ok(utf16(rest, false));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Ok(utf16(rest, true));
    }
    Ok(match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes
            .iter()
            .map(|&b| match b {
                0x80..=0x9F => WINDOWS_1252_HIGH[usize::from(b - 0x80)],
                _ => char::from(b),
            })
            .collect(),
    })
}

fn too_large() -> Error {
    Error::InvalidArgument(format!(
        "subtitle file is larger than {} MiB — not a subtitle file",
        MAX_CAPTION_FILE_BYTES >> 20
    ))
}

/// Read a subtitle file a person or an agent pointed at. It must be a `.srt`,
/// `.ass` or `.ssa` regular file of at most [`MAX_CAPTION_FILE_BYTES`] — the same
/// guards `read_text_file` puts on a theme, since the path is not Kerf's own —
/// and is decoded by [`decode_caption_bytes`]. Reading happens here, *before* the
/// project lock is taken, so a slow disk never stalls an edit.
///
/// Every failure is an [`Error::InvalidArgument`]: a missing file or the wrong
/// kind of one is the caller's to fix, not a fault in the engine.
pub fn read_caption_file(path: &Path) -> Result<String> {
    if CaptionFormat::from_extension(path).is_none() {
        return Err(Error::InvalidArgument(format!(
            "{} is not a subtitle file (expected .srt, .ass or .ssa)",
            path.display()
        )));
    }
    let unreadable = |e: std::io::Error| Error::InvalidArgument(format!("cannot read {}: {e}", path.display()));
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    if !meta.is_file() {
        return Err(Error::InvalidArgument(format!("{} is not a regular file", path.display())));
    }
    if meta.len() > MAX_CAPTION_FILE_BYTES {
        return Err(too_large());
    }
    // Bounded again on the read itself: the file can grow after the stat.
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(path)
        .map_err(unreadable)?
        .take(MAX_CAPTION_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    decode_caption_bytes(&bytes)
}

/// What an import did, in the numbers a caller reports.
///
/// Every usable cue ends up in exactly one of `placed`, `dropped_outside` or
/// `dropped_overlap`, so `cues == placed + dropped_outside + dropped_overlap`;
/// `captions` can exceed `placed` because a long cue is split into several lines.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ImportSummary {
    /// The format the file was read as.
    pub format: CaptionFormat,
    /// Usable cues read from the file.
    pub cues: usize,
    /// Cues that put at least one caption on screen.
    pub placed: usize,
    /// Caption overlays written (a long cue becomes several).
    pub captions: usize,
    /// Entries of the file that could not be used (see [`ParsedCaptions::skipped`]).
    pub skipped_lines: usize,
    /// Cues past the end of the cut, or timing footage no clip shows.
    pub dropped_outside: usize,
    /// Cues that lost their slot: captions are one lane, never two at once.
    pub dropped_overlap: usize,
    /// Earlier generated / imported captions this import replaced.
    pub replaced: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start: f64, end: f64, text: &str) -> ImportedCue {
        ImportedCue {
            start,
            end,
            text: text.to_string(),
        }
    }

    fn texts(parsed: &ParsedCaptions) -> Vec<&str> {
        parsed.cues.iter().map(|c| c.text.as_str()).collect()
    }

    // ---- timestamps ----

    #[test]
    fn timestamps_take_either_separator_and_any_fraction_width() {
        assert_eq!(parse_timestamp("00:00:01,500"), Some(1.5));
        assert_eq!(parse_timestamp("00:00:01.500"), Some(1.5));
        assert_eq!(parse_timestamp("0:00:01.50"), Some(1.5), "ASS centiseconds");
        assert_eq!(
            parse_timestamp("00:00:01,5"),
            Some(1.5),
            "a short fraction is a fraction, not milliseconds"
        );
        assert_eq!(parse_timestamp("01:02:03,004"), Some(3723.004));
        assert_eq!(parse_timestamp("01:30"), Some(90.0), "minutes:seconds");
        assert_eq!(parse_timestamp("00:00:02"), Some(2.0), "no fraction at all");
        assert_eq!(
            parse_timestamp("100:00:00,000"),
            Some(360_000.0),
            "hours past 99 are still hours"
        );
    }

    #[test]
    fn what_is_not_a_time_is_not_one() {
        for bad in [
            "",
            "garbage",
            "00:00:0a,000",
            "-00:00:01,000",
            "00:00:01,5x",
            "12",
            "1:2:3:4",
            "00::01",
            "9999999:00:00",
        ] {
            assert_eq!(parse_timestamp(bad), None, "{bad:?}");
        }
        // A fraction wider than a float cares about must not panic or overflow.
        assert!(parse_timestamp("00:00:01,123456789012345678901234567890").is_some());
    }

    // ---- SubRip ----

    #[test]
    fn a_plain_srt_reads() {
        let parsed =
            parse_srt("1\n00:00:01,000 --> 00:00:03,500\nHello there\n\n2\n00:00:04,000 --> 00:00:05,000\nGeneral Kenobi\n");
        assert_eq!(parsed.skipped, 0);
        assert_eq!(
            parsed.cues,
            vec![cue(1.0, 3.5, "Hello there"), cue(4.0, 5.0, "General Kenobi")]
        );
    }

    #[test]
    fn a_bom_and_crlf_and_cr_line_ends_are_tolerated() {
        let crlf = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,000\r\nOne\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nTwo\r\n";
        assert_eq!(texts(&parse_srt(crlf)), ["One", "Two"]);
        let cr = "1\r00:00:01,000 --> 00:00:02,000\rOne\r\r2\r00:00:03,000 --> 00:00:04,000\rTwo\r";
        assert_eq!(texts(&parse_srt(cr)), ["One", "Two"]);
        assert_eq!(parse_srt(crlf).skipped, 0);
    }

    #[test]
    fn indices_are_never_trusted() {
        let parsed = parse_srt(
            "99999999999999999999999999\n00:00:01,000 --> 00:00:02,000\nHuge index\n\n\
             7\n00:00:03,000 --> 00:00:04,000\nOut of order index\n\n\
             00:00:05,000 --> 00:00:06,000\nNo index at all\n\n\
             0\n00:00:07,000 --> 00:00:08,000\nIndex zero\n",
        );
        assert_eq!(parsed.skipped, 0);
        assert_eq!(
            texts(&parsed),
            ["Huge index", "Out of order index", "No index at all", "Index zero"]
        );
    }

    #[test]
    fn multi_line_cues_keep_their_line_breaks() {
        let parsed = parse_srt("1\n00:00:01,000 --> 00:00:03,000\nFirst line\nSecond line\n");
        assert_eq!(parsed.cues[0].text, "First line\nSecond line");
    }

    #[test]
    fn cues_with_no_blank_line_between_them_still_split_and_give_the_index_back() {
        let parsed = parse_srt(
            "1\n00:00:01,000 --> 00:00:02,000\nAlpha\n2\n00:00:03,000 --> 00:00:04,000\nBravo\n3\n00:00:05,000 --> 00:00:06,000\nCharlie",
        );
        assert_eq!(
            texts(&parsed),
            ["Alpha", "Bravo", "Charlie"],
            "the next cue's index is not Alpha's text"
        );
        assert_eq!(parsed.skipped, 0);
        // A number that really is a line of text is kept when no timing follows.
        let numeric = parse_srt("1\n00:00:01,000 --> 00:00:02,000\nThe year is\n1984\n");
        assert_eq!(numeric.cues[0].text, "The year is\n1984");
    }

    #[test]
    fn basic_markup_and_override_blocks_are_stripped() {
        let parsed = parse_srt(
            "1\n00:00:01,000 --> 00:00:03,000\n{\\an8}<i>Italic</i> and <b>bold</b> and <font color=\"#ff0000\">red</font><br>next\n\n\
             2\n00:00:04,000 --> 00:00:05,000\nKeep <laughs> and {braces} and 2 < 3 and <3\n",
        );
        assert_eq!(parsed.cues[0].text, "Italic and bold and red\nnext");
        assert_eq!(
            parsed.cues[1].text, "Keep <laughs> and {braces} and 2 < 3 and <3",
            "only real markup goes; the author's brackets stay"
        );
    }

    #[test]
    fn positioning_after_the_end_time_is_ignored() {
        let parsed = parse_srt("1\n00:00:01,000 --> 00:00:02,000  X1:100 X2:200 Y1:300 Y2:400\nHi\n");
        assert_eq!(parsed.cues, vec![cue(1.0, 2.0, "Hi")]);
        let dashed = parse_srt("1\n00:00:01,000 ---> 00:00:02,000\nHi\n");
        assert_eq!(dashed.cues, vec![cue(1.0, 2.0, "Hi")]);
    }

    #[test]
    fn overlapping_cues_are_all_kept_and_sorted() {
        let parsed = parse_srt(
            "1\n00:00:05,000 --> 00:00:08,000\nLate\n\n2\n00:00:01,000 --> 00:00:06,000\nEarly and long\n\n3\n00:00:02,000 --> 00:00:03,000\nInside\n",
        );
        assert_eq!(texts(&parsed), ["Early and long", "Inside", "Late"]);
    }

    #[test]
    fn zero_and_negative_durations_and_empty_cues_are_skipped_and_counted() {
        let parsed = parse_srt(
            "1\n00:00:01,000 --> 00:00:01,000\nZero\n\n\
             2\n00:00:05,000 --> 00:00:04,000\nBackwards\n\n\
             3\n00:00:06,000 --> 00:00:07,000\n\n\
             4\n00:00:08,000 --> 00:00:09,000\n{\\an8}<i></i>\n\n\
             5\n00:00:10,000 --> 00:00:11,000\nGood\n",
        );
        assert_eq!(texts(&parsed), ["Good"]);
        assert_eq!(parsed.skipped, 4);
    }

    #[test]
    fn a_broken_timing_line_costs_one_skip_not_its_text_lines() {
        let parsed =
            parse_srt("1\n00:00:01,000 --> nonsense\nThis cue\nhas two lines\n\n2\n00:00:03,000 --> 00:00:04,000\nStill read\n");
        assert_eq!(texts(&parsed), ["Still read"]);
        assert_eq!(parsed.skipped, 1, "one broken cue, however long its text");
    }

    #[test]
    fn a_stray_line_outside_any_cue_is_counted_and_an_arrow_in_text_is_text() {
        let parsed = parse_srt("WEBVTT\n\n1\n00:00:01,000 --> 00:00:03,000\nGo from a --> b\n");
        assert_eq!(parsed.skipped, 1, "the header is not a cue");
        assert_eq!(
            parsed.cues[0].text, "Go from a --> b",
            "an arrow that is not a time stays in the cue"
        );
    }

    #[test]
    fn an_absurdly_long_cue_is_skipped_not_chunked() {
        let long = "word ".repeat(MAX_CUE_CHARS);
        let parsed = parse_srt(&format!(
            "1\n00:00:01,000 --> 00:00:03,000\n{long}\n\n2\n00:00:04,000 --> 00:00:05,000\nFine\n"
        ));
        assert_eq!(texts(&parsed), ["Fine"]);
        assert_eq!(parsed.skipped, 1);
    }

    #[test]
    fn empty_and_cueless_input_is_an_empty_parse() {
        assert_eq!(parse_srt(""), ParsedCaptions::default());
        assert!(parse_srt("\u{feff}\n\n\n").cues.is_empty());
        assert!(parse_srt("just a note\nno cues here").cues.is_empty());
    }

    // ---- ASS ----

    const ASS: &str = "\u{feff}[Script Info]\n; a comment\nTitle: Test\nScriptType: v4.00+\n\n\
        [V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,20\n\n\
        [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
        Dialogue: 0,0:00:01.00,0:00:03.50,Default,,0,0,0,,Hello, world\n\
        Comment: 0,0:00:02.00,0:00:03.00,Default,,0,0,0,,not a caption\n\
        Dialogue: 0,0:00:04.00,0:00:05.00,Default,,0,0,0,,{\\an8\\i1}Top{\\i0}\\Nand bottom\n";

    #[test]
    fn a_plain_ass_reads_and_commas_in_the_text_survive() {
        let parsed = parse_ass(ASS).unwrap();
        assert_eq!(parsed.skipped, 0);
        assert_eq!(
            parsed.cues,
            vec![cue(1.0, 3.5, "Hello, world"), cue(4.0, 5.0, "Top\nand bottom")]
        );
    }

    #[test]
    fn the_format_line_decides_which_column_is_which() {
        let reordered = "[Events]\nFormat: Start, End, Text, Layer, Style\nDialogue: 0:00:01.00,0:00:02.00,Reordered,0,Default\n";
        // `Text` is not last here, so it only survives when it has no comma — and
        // the columns are still the ones the file named.
        let parsed = parse_ass(reordered).unwrap();
        assert_eq!(parsed.cues, vec![cue(1.0, 2.0, "Reordered")]);

        let ssa = "[Events]\nFormat: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
                   Dialogue: Marked=0,0:00:01.00,0:00:02.00,*Default,NTP,0000,0000,0000,,SSA line\n";
        assert_eq!(parse_ass(ssa).unwrap().cues, vec![cue(1.0, 2.0, "SSA line")]);

        let err = parse_ass("[Events]\nFormat: Layer, Style, Text\nDialogue: 0,Default,hi\n").unwrap_err();
        assert!(err.to_string().contains("Start, End and Text"), "{err}");
    }

    #[test]
    fn a_format_in_the_styles_section_does_not_steal_the_events_columns() {
        // The styles `Format:` has no Start/End/Text; reading it as the events
        // one would fail the whole file.
        assert_eq!(parse_ass(ASS).unwrap().cues.len(), 2);
    }

    #[test]
    fn a_headerless_run_of_dialogue_lines_reads_with_the_default_columns() {
        let parsed = parse_ass("Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Bare\n").unwrap();
        assert_eq!(parsed.cues, vec![cue(1.0, 2.0, "Bare")]);
    }

    #[test]
    fn ass_escapes_overrides_and_drawings_are_resolved() {
        let parsed = parse_ass(
            "[Events]\n\
             Dialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,One\\NTwo\\nThree\\hFour\n\
             Dialogue: 0,0:00:03.00,0:00:04.00,D,,0,0,0,,{\\pos(10,10)\\fad(100,100)\\k20}Karaoke {comment}text\n\
             Dialogue: 0,0:00:05.00,0:00:06.00,D,,0,0,0,,{\\p1}m 0 0 l 100 0 100 100{\\p0}\n\
             Dialogue: 0,0:00:07.00,0:00:08.00,D,,0,0,0,,Before{\\p1}m 0 0 l 1 1{\\p0}After\n\
             Dialogue: 0,0:00:09.00,0:00:10.00,D,,0,0,0,,Open { brace\n",
        )
        .unwrap();
        assert_eq!(parsed.cues[0].text, "One\nTwo\nThree Four");
        assert_eq!(parsed.cues[1].text, "Karaoke text");
        // The drawing-only line has no text and is skipped; a drawing in the
        // middle of a line leaves the words around it.
        assert_eq!(parsed.cues[2].text, "BeforeAfter");
        assert_eq!(parsed.cues[3].text, "Open { brace", "an unterminated brace is text");
        assert_eq!(parsed.skipped, 1);
    }

    #[test]
    fn ass_dialogue_is_reordered_by_time_and_malformed_lines_are_counted() {
        let parsed = parse_ass(
            "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
             Dialogue: 0,0:00:09.00,0:00:10.00,D,,0,0,0,,Last\n\
             Dialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,First\n\
             Dialogue: 0,0:00:03.00,0:00:03.00,D,,0,0,0,,Zero length\n\
             Dialogue: 0,garbage,0:00:05.00,D,,0,0,0,,Bad time\n\
             Dialogue: 0,0:00:06.00\n\
             Dialogue: 0,0:00:07.00,0:00:08.00,D,,0,0,0,,\n\
             Picture: 0,0:00:02.00,0:00:03.00,pic.png\n\
             Unknown: whatever\n",
        )
        .unwrap();
        assert_eq!(texts(&parsed), ["First", "Last"]);
        assert_eq!(parsed.skipped, 4, "zero length, bad time, too few fields, empty text");
    }

    // ---- detection and entry points ----

    #[test]
    fn the_format_is_guessed_from_the_text() {
        assert_eq!(CaptionFormat::detect(ASS), CaptionFormat::Ass);
        assert_eq!(CaptionFormat::detect("[events]\nDialogue: x"), CaptionFormat::Ass);
        assert_eq!(
            CaptionFormat::detect("1\n00:00:01,000 --> 00:00:02,000\nHi"),
            CaptionFormat::Srt
        );
        assert_eq!(
            CaptionFormat::detect("Dialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,hi"),
            CaptionFormat::Ass
        );
        // A subtitle that merely says "Dialogue:" is still a subtitle.
        assert_eq!(
            CaptionFormat::detect("1\n00:00:01,000 --> 00:00:02,000\nDialogue: a play"),
            CaptionFormat::Srt
        );
        assert_eq!(CaptionFormat::detect(""), CaptionFormat::Srt);
        assert_eq!(CaptionFormat::parse(".SSA"), Some(CaptionFormat::Ass));
        assert_eq!(CaptionFormat::parse("vtt"), None);
        assert_eq!(
            CaptionFormat::from_extension(Path::new("/a/b/Movie.SRT")),
            Some(CaptionFormat::Srt)
        );
        assert_eq!(CaptionFormat::from_extension(Path::new("/a/b/notes.txt")), None);
    }

    #[test]
    fn parse_captions_follows_a_named_format_over_a_guess_and_caps_the_cue_count() {
        let (format, parsed) = parse_captions(ASS, None).unwrap();
        assert_eq!((format, parsed.cues.len()), (CaptionFormat::Ass, 2));
        // Told it is SRT, an ASS file has no timing lines: nothing, not a panic.
        let (format, parsed) = parse_captions(ASS, Some(CaptionFormat::Srt)).unwrap();
        assert_eq!(format, CaptionFormat::Srt);
        assert!(parsed.cues.is_empty());

        let many: String = (0..=MAX_CAPTION_CUES)
            .map(|i| format!("{i}\n00:00:00,000 --> 00:00:01,000\nx\n\n"))
            .collect();
        let err = parse_captions(&many, None).unwrap_err();
        assert!(err.to_string().contains("at most"), "{err}");
    }

    // ---- bytes ----

    #[test]
    fn bytes_decode_by_bom_then_utf8_then_latin1() {
        assert_eq!(decode_caption_bytes("héllo ♪".as_bytes()).unwrap(), "héllo ♪");
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice("héllo".as_bytes());
        assert_eq!(decode_caption_bytes(&bom).unwrap(), "héllo");

        let utf16 = |s: &str, big: bool| {
            let mut out = if big { vec![0xFE, 0xFF] } else { vec![0xFF, 0xFE] };
            for u in s.encode_utf16() {
                out.extend_from_slice(&if big { u.to_be_bytes() } else { u.to_le_bytes() });
            }
            out
        };
        assert_eq!(
            decode_caption_bytes(&utf16("1\n00:00:01,000 --> 00:00:02,000\nhéllo ♪", false))
                .unwrap()
                .lines()
                .last(),
            Some("héllo ♪")
        );
        assert_eq!(decode_caption_bytes(&utf16("héllo ♪", true)).unwrap(), "héllo ♪");

        // "Café “quoted” – dash" in Windows-1252: not UTF-8, so Latin-1 it is.
        let ansi = [b'C', b'a', b'f', 0xE9, b' ', 0x93, b'q', 0x94, b' ', 0x96, b' ', b'd'];
        assert_eq!(decode_caption_bytes(&ansi).unwrap(), "Café “q” – d");

        // An odd trailing byte in UTF-16 is dropped, not a panic.
        assert_eq!(decode_caption_bytes(&[0xFF, 0xFE, b'h', 0, b'i']).unwrap(), "h");
        assert_eq!(decode_caption_bytes(&[]).unwrap(), "");
    }

    #[test]
    fn a_format_argument_is_optional_but_not_forgiving() {
        assert_eq!(CaptionFormat::from_arg(None).unwrap(), None);
        assert_eq!(CaptionFormat::from_arg(Some("  ")).unwrap(), None);
        assert_eq!(CaptionFormat::from_arg(Some("SSA")).unwrap(), Some(CaptionFormat::Ass));
        assert_eq!(CaptionFormat::from_arg(Some(".srt")).unwrap(), Some(CaptionFormat::Srt));
        let err = CaptionFormat::from_arg(Some("vtt")).unwrap_err().to_string();
        assert!(err.contains("\"srt\" or \"ass\""), "{err}");
    }

    #[test]
    fn oversized_text_is_refused_like_an_oversized_file() {
        let big = "a".repeat(MAX_CAPTION_FILE_BYTES as usize + 1);
        assert!(parse_captions(&big, None)
            .unwrap_err()
            .to_string()
            .contains("larger than 5 MiB"));
    }

    #[test]
    fn an_oversized_file_is_refused() {
        let big = vec![b'a'; MAX_CAPTION_FILE_BYTES as usize + 1];
        let err = decode_caption_bytes(&big).unwrap_err();
        assert!(err.to_string().contains("larger than 5 MiB"), "{err}");
    }

    #[test]
    fn reading_a_file_checks_extension_kind_and_size() {
        let dir = std::env::temp_dir().join(format!("kerf-captions-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let srt = dir.join("good.SRT");
        std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\nHi\n").unwrap();
        assert!(read_caption_file(&srt).unwrap().contains("Hi"));

        let txt = dir.join("notes.txt");
        std::fs::write(&txt, "x").unwrap();
        let err = read_caption_file(&txt).unwrap_err();
        assert!(err.to_string().contains(".srt, .ass or .ssa"), "{err}");

        let missing = dir.join("missing.srt");
        assert!(matches!(read_caption_file(&missing), Err(Error::InvalidArgument(_))));

        let folder = dir.join("folder.srt");
        std::fs::create_dir_all(&folder).unwrap();
        let err = read_caption_file(&folder).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");

        let big = dir.join("big.ass");
        std::fs::write(&big, vec![b'a'; MAX_CAPTION_FILE_BYTES as usize + 1]).unwrap();
        let err = read_caption_file(&big).unwrap_err();
        assert!(err.to_string().contains("larger than 5 MiB"), "{err}");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
