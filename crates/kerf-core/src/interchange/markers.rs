//! Marker lists for a delivery: a review CSV, YouTube chapter text and the
//! FFMETADATA chapter list the export muxes in.

use serde::{Deserialize, Serialize};

use super::{one_line, Report, Timebase};
use crate::error::Result;
use crate::model::{Marker, MarkerKind};

/// A marker list a delivery can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MarkerListFormat {
    /// Every marker as a spreadsheet row (`.csv`).
    Csv,
    /// YouTube chapter lines for a video description (`.txt`).
    Youtube,
}

impl MarkerListFormat {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Youtube => "txt",
        }
    }
}

/// Markers in time order (stable for markers at the same time).
fn sorted(markers: &[Marker]) -> Vec<&Marker> {
    let mut out: Vec<&Marker> = markers.iter().collect();
    out.sort_by(|a, b| a.time.total_cmp(&b.time));
    out
}

/// One CSV cell: quoted, quotes doubled — and, for free text, a leading `'`
/// when it starts like a spreadsheet formula (`=`, `+`, `-`, `@`, a tab or a CR,
/// or one of the four after leading whitespace), so opening the file runs nothing.
fn csv_cell(out: &mut String, value: &str, text: bool) {
    out.push('"');
    let formula = value.starts_with(['=', '+', '-', '@', '\t', '\r']) || value.trim_start().starts_with(['=', '+', '-', '@']);
    if text && formula {
        out.push('\'');
    }
    for c in value.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
}

/// The markers as a CSV (RFC 4180, CRLF rows): name, kind, start and end
/// timecode at `rate`, start and duration in seconds, comment and colour.
pub fn markers_csv(markers: &[Marker], rate: f64) -> Result<String> {
    let tb = Timebase::new(rate)?;
    let mut out = String::from("Name,Kind,Start,End,Start Seconds,Duration Seconds,Comment,Color\r\n");
    for m in sorted(markers) {
        let cells: [(String, bool); 8] = [
            (m.name.clone(), true),
            (m.kind.as_str().to_string(), false),
            (tb.timecode(tb.frames(m.time)), false),
            (tb.timecode(tb.frames(m.end())), false),
            (format!("{:.3}", m.time), false),
            (format!("{:.3}", m.duration), false),
            (m.comment.clone(), true),
            (m.color.clone().unwrap_or_default(), true),
        ];
        for (i, (value, text)) in cells.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            csv_cell(&mut out, value, *text);
        }
        out.push_str("\r\n");
    }
    Ok(out)
}

/// The markers that make chapters: the chapter markers, or every marker when
/// none is one.
fn chapter_markers(markers: &[Marker]) -> Vec<&Marker> {
    let chapters: Vec<&Marker> = sorted(markers)
        .into_iter()
        .filter(|m| m.kind == MarkerKind::Chapter)
        .collect();
    if chapters.is_empty() {
        sorted(markers)
    } else {
        chapters
    }
}

fn youtube_time(seconds: i64, hours: bool) -> String {
    let (h, m, s) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if hours {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{}:{s:02}", seconds / 60)
    }
}

/// YouTube's chapter list for a description: `0:00 Intro`, one line per chapter,
/// from the chapter markers (every marker when none is a chapter) inside the cut.
/// YouTube wants the first at `0:00`, at least three, each ten seconds or longer:
/// a list that does not start at zero gets an `Intro` line, and the other two
/// rules are reported rather than enforced.
pub fn youtube_chapters(markers: &[Marker], duration: f64) -> (String, Report) {
    let mut report = Report::default();
    let hours = duration >= 3600.0;
    let mut lines: Vec<(i64, String)> = Vec::new();
    for (i, m) in chapter_markers(markers).into_iter().enumerate() {
        if m.time >= duration {
            report.warn("markers past the end of the cut were left out");
            continue;
        }
        let at = m.time.max(0.0).floor() as i64;
        if lines.last().is_some_and(|(t, _)| *t == at) {
            report.warn("markers in the same second as the one before were left out");
            continue;
        }
        let name = one_line(&m.name);
        let name = if name.is_empty() { format!("Chapter {}", i + 1) } else { name };
        lines.push((at, name));
    }
    if lines.is_empty() {
        report.warn("the cut has no markers to make chapters from");
        return (String::new(), report);
    }
    if lines[0].0 > 0 {
        lines.insert(0, (0, "Intro".to_string()));
        report.info("YouTube chapters start at 0:00, so an Intro chapter was added");
    }
    if lines.len() < 3 {
        report.warn("YouTube shows chapters only when there are at least three");
    }
    let ends: Vec<f64> = lines.iter().skip(1).map(|(t, _)| *t as f64).chain([duration]).collect();
    for ((start, _), end) in lines.iter().zip(ends) {
        if end - (*start as f64) < 10.0 {
            report.warn("YouTube needs every chapter to be at least 10 seconds long");
        }
    }
    let text = lines
        .iter()
        .map(|(t, name)| format!("{} {name}\n", youtube_time(*t, hours)))
        .collect();
    (text, report)
}

/// A value of an FFMETADATA file: `=`, `;`, `#`, `\` and line breaks escaped,
/// other control characters dropped.
fn ffmetadata_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '=' | ';' | '#' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The chapter markers of a cut `duration` long as an FFMETADATA file, for the
/// export to mux in as chapters; `None` when it has none (the argv then stays as
/// it was). Each chapter runs to the next one (or the end of the cut), or for
/// its own duration when that is shorter. Only `Chapter` markers count here: a
/// review comment is not a chapter of the delivered video.
pub fn ffmetadata_chapters(markers: &[Marker], duration: f64) -> Option<String> {
    let chapters: Vec<&Marker> = sorted(markers)
        .into_iter()
        .filter(|m| m.kind == MarkerKind::Chapter && m.time >= 0.0 && m.time < duration)
        .collect();
    let mut out = String::from(";FFMETADATA1\n");
    let mut any = false;
    for (i, m) in chapters.iter().enumerate() {
        let next = chapters.get(i + 1).map_or(duration, |n| n.time.min(duration));
        let end = if m.duration > 0.0 { m.end().min(next) } else { next };
        let (start_ms, end_ms) = ((m.time * 1000.0).round() as i64, (end * 1000.0).round() as i64);
        if end_ms <= start_ms {
            continue;
        }
        let name = one_line(&m.name);
        let title = if name.is_empty() { format!("Chapter {}", i + 1) } else { name };
        out.push_str(&format!(
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART={start_ms}\nEND={end_ms}\ntitle={}\n",
            ffmetadata_escape(&title)
        ));
        any = true;
    }
    any.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn marker(time: f64, name: &str, kind: MarkerKind) -> Marker {
        Marker {
            id: Uuid::from_u128(time.to_bits() as u128),
            time,
            name: name.to_string(),
            kind,
            ..Default::default()
        }
    }

    #[test]
    fn the_csv_quotes_every_cell_and_defuses_formulas() {
        let mut a = marker(1.5, "=HYPERLINK(\"x\")", MarkerKind::Chapter);
        a.comment = "two\nlines, \"quoted\"".into();
        a.duration = 2.0;
        a.color = Some("#ff0000".into());
        let mut b = marker(0.0, " +1", MarkerKind::Comment);
        b.comment = "\tcmd".into();
        let c = marker(3.0, "@SUM(1)", MarkerKind::Comment);
        let d = marker(4.0, "-1", MarkerKind::Comment);
        let e = marker(5.0, "\rx", MarkerKind::Comment);
        let csv = markers_csv(&[a, b, c, d, e], 25.0).unwrap();
        assert_eq!(
            csv,
            "Name,Kind,Start,End,Start Seconds,Duration Seconds,Comment,Color\r\n\
             \"' +1\",\"comment\",\"00:00:00:00\",\"00:00:00:00\",\"0.000\",\"0.000\",\"'\tcmd\",\"\"\r\n\
             \"'=HYPERLINK(\"\"x\"\")\",\"chapter\",\"00:00:01:13\",\"00:00:03:13\",\"1.500\",\"2.000\",\"two\nlines, \"\"quoted\"\"\",\"#ff0000\"\r\n\
             \"'@SUM(1)\",\"comment\",\"00:00:03:00\",\"00:00:03:00\",\"3.000\",\"0.000\",\"\",\"\"\r\n\
             \"'-1\",\"comment\",\"00:00:04:00\",\"00:00:04:00\",\"4.000\",\"0.000\",\"\",\"\"\r\n\
             \"'\rx\",\"comment\",\"00:00:05:00\",\"00:00:05:00\",\"5.000\",\"0.000\",\"\",\"\"\r\n"
        );
        assert!(markers_csv(&[], 0.0).is_err());
    }

    #[test]
    fn youtube_chapters_start_at_zero_and_report_what_youtube_refuses() {
        let markers = [
            marker(75.4, "Part two", MarkerKind::Chapter),
            marker(12.0, "Setup", MarkerKind::Chapter),
            marker(30.0, "a note", MarkerKind::Comment),
            marker(75.9, "same second", MarkerKind::Chapter),
            marker(400.0, "past the end", MarkerKind::Chapter),
        ];
        let (text, report) = youtube_chapters(&markers, 120.0);
        assert_eq!(text, "0:00 Intro\n0:12 Setup\n1:15 Part two\n");
        assert_eq!(report.entries.len(), 3, "{report}");
        assert!(!report.to_string().contains("10 seconds"), "{report}");

        let (text, report) = youtube_chapters(
            &[
                marker(0.0, "", MarkerKind::Comment),
                marker(3700.0, "late", MarkerKind::Comment),
            ],
            3705.0,
        );
        assert_eq!(text, "0:00:00 Chapter 1\n1:01:40 late\n");
        assert!(report.to_string().contains("at least three"), "{report}");
        assert!(report.to_string().contains("10 seconds"), "{report}");

        let (text, report) = youtube_chapters(&[], 10.0);
        assert!(text.is_empty() && report.has_warnings());
    }

    #[test]
    fn ffmetadata_chapters_tile_the_cut_and_escape_titles() {
        let mut short = marker(10.0, "short; =#\\", MarkerKind::Chapter);
        short.duration = 2.0;
        let markers = [
            marker(0.0, "Intro\nline", MarkerKind::Chapter),
            short,
            marker(5.0, "a comment", MarkerKind::Comment),
            marker(20.0, "", MarkerKind::Chapter),
            marker(30.0, "past", MarkerKind::Chapter),
        ];
        assert_eq!(
            ffmetadata_chapters(&markers, 25.5).unwrap(),
            ";FFMETADATA1\n\
             [CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=10000\ntitle=Intro line\n\
             [CHAPTER]\nTIMEBASE=1/1000\nSTART=10000\nEND=12000\ntitle=short\\; \\=\\#\\\\\n\
             [CHAPTER]\nTIMEBASE=1/1000\nSTART=20000\nEND=25500\ntitle=Chapter 3\n"
        );
        assert_eq!(ffmetadata_chapters(&[marker(1.0, "c", MarkerKind::Comment)], 10.0), None);
        assert_eq!(ffmetadata_chapters(&[], 10.0), None);
    }
}
