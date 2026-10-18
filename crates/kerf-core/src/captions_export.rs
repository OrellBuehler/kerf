//! Writing the cut's captions out as a subtitle file — the back half of the
//! caption round trip ([`crate::captions_import`] is the front).
//!
//! Pure: a timeline in, text out. The captions are the timeline's *generated*
//! overlays (transcript captions and imported subtitle files; a typed title is
//! not a caption), which are already in timeline time — `Timeline::captions` and
//! `Timeline::place_cues` projected them through trims, reorders and speed when
//! they were written — so a cue here is exactly the line the burned-in overlay
//! draws, clipped to the cut's length as the export clips it.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::Timeline;

/// A subtitle format Kerf writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleFormat {
    /// SubRip (`.srt`).
    Srt,
    /// WebVTT (`.vtt`).
    Vtt,
}

impl SubtitleFormat {
    /// The file extension (and wire name).
    pub fn ext(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Vtt => "vtt",
        }
    }

    /// A format by name or extension (`srt`, `vtt`, `webvtt`; any case, optional dot).
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
            "srt" | "subrip" => Some(Self::Srt),
            "vtt" | "webvtt" => Some(Self::Vtt),
            _ => None,
        }
    }

    /// The format a path's extension names, or an error saying which ones exist.
    pub fn from_path(path: &std::path::Path) -> Result<Self> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(Self::parse)
            .ok_or_else(|| Error::InvalidArgument(format!("{} is not a .srt or .vtt path", path.display())))
    }
}

/// One caption of the cut, in timeline seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptionCue {
    pub start: f64,
    pub end: f64,
    /// The lines of the caption, joined with `\n`.
    pub text: String,
}

/// The shortest cue written (seconds); anything shorter rounds to nothing.
const MIN_CUE: f64 = 0.001;

/// The captions of `timeline` as they appear in the cut: its generated overlays,
/// in order, clipped to `[0, duration]` (an empty cut has none), their text with
/// control characters removed and blank lines dropped.
pub fn cut_captions(timeline: &Timeline) -> Vec<CaptionCue> {
    let duration = timeline.duration();
    let mut cues: Vec<CaptionCue> = timeline
        .overlays
        .iter()
        .filter(|o| o.generated)
        .filter_map(|o| {
            let start = o.start.max(0.0);
            let end = o.end.min(duration);
            let text = clean_text(&o.text);
            (start.is_finite() && end - start >= MIN_CUE && !text.is_empty()).then_some(CaptionCue { start, end, text })
        })
        .collect();
    cues.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.end.total_cmp(&b.end)));
    cues
}

fn clean_text(text: &str) -> String {
    text.split(['\n', '\r', '\u{2028}', '\u{2029}'])
        .map(|l| l.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whole milliseconds of a cue, the end kept at least a millisecond past the start.
fn cue_ms(cue: &CaptionCue) -> (u64, u64) {
    let start = (cue.start.max(0.0) * 1000.0).round() as u64;
    let end = ((cue.end.max(0.0) * 1000.0).round() as u64).max(start + 1);
    (start, end)
}

fn clock(ms: u64, sep: char) -> String {
    let (h, rem) = (ms / 3_600_000, ms % 3_600_000);
    let (m, rem) = (rem / 60_000, rem % 60_000);
    format!("{h:02}:{m:02}:{:02}{sep}{:03}", rem / 1000, rem % 1000)
}

/// A SubRip document: numbered from 1, `,` before the milliseconds. A `-->` in
/// the text becomes `->` so no line of text reads as a timing line.
pub fn write_srt(cues: &[CaptionCue]) -> String {
    let mut out = String::new();
    for (i, cue) in cues.iter().enumerate() {
        let (start, end) = cue_ms(cue);
        out.push_str(&format!("{}\n{} --> {}\n", i + 1, clock(start, ','), clock(end, ',')));
        for line in cue.text.lines() {
            out.push_str(&line.replace("-->", "->"));
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// A WebVTT document: the `WEBVTT` signature, `.` before the milliseconds, and
/// `&`, `<` and `>` written as references so the text is never read as markup.
pub fn write_vtt(cues: &[CaptionCue]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for cue in cues {
        let (start, end) = cue_ms(cue);
        out.push_str(&format!("{} --> {}\n", clock(start, '.'), clock(end, '.')));
        for line in cue.text.lines() {
            out.push_str(&line.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"));
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// `cues` written in `format`.
pub fn write_captions(cues: &[CaptionCue], format: SubtitleFormat) -> String {
    match format {
        SubtitleFormat::Srt => write_srt(cues),
        SubtitleFormat::Vtt => write_vtt(cues),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captions_import::{parse_srt, parse_vtt};
    use crate::model::{Clip, StreamKind, TextOverlay, Track};
    use uuid::Uuid;

    fn caption(text: &str, start: f64, end: f64) -> TextOverlay {
        TextOverlay {
            generated: true,
            ..TextOverlay::new(text, start, end)
        }
    }

    fn cut(seconds: f64, overlays: Vec<TextOverlay>) -> Timeline {
        let mut track = Track::new(StreamKind::Video, "V1");
        track.clips.push(Clip::new(Uuid::new_v4(), 0.0, seconds, 0.0));
        Timeline {
            tracks: vec![track],
            overlays,
            ..Timeline::new()
        }
    }

    #[test]
    fn the_cut_captions_are_the_generated_overlays_clipped_to_the_cut() {
        let tl = cut(
            10.0,
            vec![
                caption("second", 4.0, 6.0),
                TextOverlay::new("a typed title", 0.0, 3.0),
                caption("first\r\n\n line\u{0}", -1.0, 2.0),
                caption("past the end", 9.5, 12.0),
                caption("after it", 10.0, 11.0),
                caption("   ", 7.0, 8.0),
            ],
        );
        let cues = cut_captions(&tl);
        assert_eq!(
            cues,
            vec![
                CaptionCue {
                    start: 0.0,
                    end: 2.0,
                    text: "first\nline".into()
                },
                CaptionCue {
                    start: 4.0,
                    end: 6.0,
                    text: "second".into()
                },
                CaptionCue {
                    start: 9.5,
                    end: 10.0,
                    text: "past the end".into()
                },
            ]
        );
        assert!(cut_captions(&Timeline::new()).is_empty());
    }

    #[test]
    fn srt_and_vtt_are_written_exactly() {
        let cues = vec![
            CaptionCue {
                start: 0.0,
                end: 1.2346,
                text: "Hello & <you>\nline --> two".into(),
            },
            CaptionCue {
                start: 3723.5,
                end: 3723.5004,
                text: "an hour".into(),
            },
        ];
        assert_eq!(
            write_srt(&cues),
            "1\n00:00:00,000 --> 00:00:01,235\nHello & <you>\nline -> two\n\n2\n01:02:03,500 --> 01:02:03,501\nan hour\n\n"
        );
        assert_eq!(
            write_vtt(&cues),
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.235\nHello &amp; &lt;you&gt;\nline --&gt; two\n\n01:02:03.500 --> 01:02:03.501\nan hour\n\n"
        );
    }

    #[test]
    fn written_captions_read_back_through_the_importers() {
        let cues = vec![
            CaptionCue {
                start: 1.0,
                end: 2.5,
                text: "one\ntwo".into(),
            },
            CaptionCue {
                start: 3.0,
                end: 4.0,
                text: "a < b & c".into(),
            },
        ];
        for parsed in [parse_srt(&write_srt(&cues)), parse_vtt(&write_vtt(&cues))] {
            assert_eq!(parsed.skipped, 0);
            let back: Vec<_> = parsed.cues.iter().map(|c| (c.start, c.end, c.text.as_str())).collect();
            assert_eq!(back, vec![(1.0, 2.5, "one\ntwo"), (3.0, 4.0, "a < b & c")]);
        }
    }

    #[test]
    fn formats_parse_by_name_and_path() {
        assert_eq!(SubtitleFormat::parse(".VTT"), Some(SubtitleFormat::Vtt));
        assert_eq!(SubtitleFormat::parse("subrip"), Some(SubtitleFormat::Srt));
        assert_eq!(SubtitleFormat::parse("ass"), None);
        assert_eq!(
            SubtitleFormat::from_path(std::path::Path::new("/x/cut.srt")).unwrap(),
            SubtitleFormat::Srt
        );
        assert!(SubtitleFormat::from_path(std::path::Path::new("/x/cut.txt")).is_err());
    }
}
