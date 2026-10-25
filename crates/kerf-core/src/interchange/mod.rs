//! Handing the cut to another editor: **OpenTimelineIO** (`.otio`) and
//! **CMX 3600 EDL** (`.edl`), plus the marker lists (CSV, YouTube chapters,
//! FFMETADATA chapters) that ride along with a delivery.
//!
//! Pure: a timeline and its assets in, text out — no file I/O. A format that
//! cannot say something Kerf can never fails over it; it writes what it can and
//! says what it dropped or approximated in a [`Report`], one line per kind of
//! loss with a count, so a cut with forty faded clips reports one line, not forty.
//! Media references are the **original** files (`Asset::path`), never proxies.

pub mod edl;
pub mod markers;
pub mod otio;

#[cfg(test)]
mod golden;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{Asset, Clip, Timeline};

/// An interchange format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InterchangeFormat {
    /// OpenTimelineIO JSON (`.otio`).
    Otio,
    /// CMX 3600 edit decision list (`.edl`).
    Edl,
}

impl InterchangeFormat {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Otio => "otio",
            Self::Edl => "edl",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
            "otio" | "opentimelineio" => Some(Self::Otio),
            "edl" | "cmx" | "cmx3600" => Some(Self::Edl),
            _ => None,
        }
    }
}

/// How serious a [`ReportEntry`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Nothing was lost; worth knowing (an assumption made, where something went).
    Info,
    /// Something was dropped or approximated.
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReportEntry {
    pub level: Level,
    pub message: String,
    /// How many times it happened.
    pub count: u32,
}

/// What an export could not carry exactly. Identical messages merge into one
/// entry with a count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Report {
    pub entries: Vec<ReportEntry>,
}

impl Report {
    pub fn push(&mut self, level: Level, message: impl Into<String>) {
        let message = message.into();
        match self.entries.iter_mut().find(|e| e.level == level && e.message == message) {
            Some(e) => e.count = e.count.saturating_add(1),
            None => self.entries.push(ReportEntry {
                level,
                message,
                count: 1,
            }),
        }
    }

    pub fn warn(&mut self, message: impl Into<String>) {
        self.push(Level::Warning, message);
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.push(Level::Info, message);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn has_warnings(&self) -> bool {
        self.entries.iter().any(|e| e.level == Level::Warning)
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for e in &self.entries {
            let level = match e.level {
                Level::Info => "info",
                Level::Warning => "warning",
            };
            if e.count > 1 {
                writeln!(f, "{level}: {} (x{})", e.message, e.count)?;
            } else {
                writeln!(f, "{level}: {}", e.message)?;
            }
        }
        Ok(())
    }
}

/// What an export needs beyond the cut.
#[derive(Debug, Clone, PartialEq)]
pub struct InterchangeOptions {
    /// The timeline's frame rate: what an EDL quantizes to and OTIO times count in.
    pub rate: f64,
    /// The document's name (EDL `TITLE:`, OTIO timeline name).
    pub title: String,
    /// EDL only: which video track (0 = the first, V1). An EDL holds one.
    pub video_track: usize,
}

/// `timeline` written in `format`, with what it could not carry.
pub fn export(
    format: InterchangeFormat,
    timeline: &Timeline,
    assets: &[Asset],
    opts: &InterchangeOptions,
) -> Result<(String, Report)> {
    let mut report = Report::default();
    let text = match format {
        InterchangeFormat::Otio => otio::export(timeline, assets, opts, &mut report)?,
        InterchangeFormat::Edl => edl::export(timeline, assets, opts, &mut report)?,
    };
    Ok((text, report))
}

/// A frame rate and the SMPTE timecode it counts in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Timebase {
    /// Frames per second, exact for the NTSC rates (`30000/1001`).
    pub fps: f64,
    /// The whole frames a timecode second counts (30 for 29.97).
    pub base: i64,
    /// Drop-frame timecode (29.97 and 59.94).
    pub drop: bool,
}

impl Timebase {
    pub fn new(rate: f64) -> Result<Self> {
        if !rate.is_finite() || !(1.0..=1000.0).contains(&rate) {
            return Err(Error::InvalidArgument(format!("frame rate {rate} is not between 1 and 1000")));
        }
        let base = rate.round() as i64;
        let ntsc = (rate - base as f64).abs() > 1e-3 && (rate * 1.001 - base as f64).abs() < 0.01;
        let fps = if ntsc { base as f64 * 1000.0 / 1001.0 } else { rate };
        Ok(Self {
            fps,
            base,
            drop: ntsc && base % 30 == 0,
        })
    }

    /// The frame `seconds` falls on (nearest).
    pub fn frames(&self, seconds: f64) -> i64 {
        (seconds * self.fps).round() as i64
    }

    /// Whether `seconds` is a whole frame.
    pub fn on_frame(&self, seconds: f64) -> bool {
        let f = seconds * self.fps;
        (f - f.round()).abs() < 1e-6
    }

    /// Frames dropped from the count each minute (but every tenth) in drop-frame.
    fn dropped(&self) -> i64 {
        if self.drop {
            self.base / 15
        } else {
            0
        }
    }

    /// The frame count of `hh:mm:ss:ff`.
    pub fn frames_of(&self, h: i64, m: i64, s: i64, f: i64) -> i64 {
        let minutes = h * 60 + m;
        (h * 3600 + m * 60 + s) * self.base + f - self.dropped() * (minutes - minutes / 10)
    }

    /// SMPTE timecode of a frame count; drop-frame writes `;` before the frames.
    pub fn timecode(&self, frame: i64) -> String {
        let neg = frame < 0;
        let mut n = frame.unsigned_abs() as i64;
        let drop = self.dropped();
        if drop > 0 {
            let per_min = self.base * 60 - drop;
            let per_ten = per_min * 10 + drop;
            let (tens, rem) = (n / per_ten, n % per_ten);
            n += drop * 9 * tens;
            if rem > drop {
                n += drop * ((rem - drop) / per_min);
            }
        }
        let f = n % self.base;
        let s = (n / self.base) % 60;
        let m = (n / (self.base * 60)) % 60;
        let h = n / (self.base * 3600);
        let sep = if self.drop { ';' } else { ':' };
        let width = if self.base >= 100 { 3 } else { 2 };
        format!("{}{h:02}:{m:02}:{s:02}{sep}{f:0width$}", if neg { "-" } else { "" })
    }
}

/// A `file://` URL for an absolute path (Windows drive and UNC paths included).
pub(crate) fn file_url(path: &str) -> String {
    fn encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for &b in s.as_bytes() {
            if b.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }
    let p = path.replace('\\', "/");
    let p = if let Some(unc) = p.strip_prefix("//?/UNC/") {
        format!("//{unc}")
    } else if let Some(local) = p.strip_prefix("//?/") {
        local.to_string()
    } else {
        p
    };
    let b = p.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/';
    if drive {
        format!("file:///{}", encode(&p))
    } else if let Some(unc) = p.strip_prefix("//") {
        format!("file://{}", encode(unc))
    } else if p.starts_with('/') {
        format!("file://{}", encode(&p))
    } else {
        encode(&p)
    }
}

/// One line of text: control characters dropped, line breaks a space.
pub(crate) fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// The one rate an interchange format can hold for `clip`: its speed, or — for a clip whose rate
/// changes over its length, which no format here can write — its average, so the event still covers
/// the footage it plays, in its direction.
pub(crate) fn export_speed(clip: &Clip) -> f64 {
    if clip.is_ramped() {
        clip.mean_speed().copysign(clip.speed)
    } else {
        clip.speed
    }
}

/// What a clip carries that an interchange format has no field for, one name
/// per kind (so the report counts clips per kind).
pub(crate) fn clip_extras(clip: &Clip) -> Vec<&'static str> {
    let mut out = Vec::new();
    if clip.fade_in > 0.0 || clip.fade_out > 0.0 {
        out.push("fades");
    }
    if !clip.transform.is_identity() || !clip.keyframes.is_empty() {
        out.push("transforms and keyframes");
    }
    if !clip.color.is_identity() {
        out.push("colour corrections");
    }
    if !clip.effects.is_empty() || !clip.audio.is_empty() {
        out.push("effects");
    }
    if !clip.channels.is_empty() {
        out.push("animated properties");
    }
    if clip.mask.is_some() {
        out.push("masks");
    }
    if clip.reframe.is_some() {
        out.push("360 reframes");
    }
    if (clip.volume - 1.0).abs() > f32::EPSILON {
        out.push("clip volume");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_merge_identical_messages() {
        let mut r = Report::default();
        r.warn("fades are not carried");
        r.info("fades are not carried");
        r.warn("fades are not carried");
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.entries[0].count, 2);
        assert!(r.has_warnings());
        assert_eq!(
            r.to_string(),
            "warning: fades are not carried (x2)\ninfo: fades are not carried\n"
        );
    }

    #[test]
    fn timecode_counts_drop_frame_like_smpte() {
        let df = Timebase::new(29.97).unwrap();
        assert!(df.drop);
        assert_eq!(df.timecode(0), "00:00:00;00");
        assert_eq!(df.timecode(1799), "00:00:59;29");
        assert_eq!(df.timecode(1800), "00:01:00;02");
        assert_eq!(df.timecode(17982), "00:10:00;00");
        assert_eq!(df.frames_of(1, 0, 0, 0), 107_892);
        assert_eq!(df.timecode(107_892), "01:00:00;00");
        for f in [0, 1, 1799, 1800, 1801, 17_981, 17_982, 107_891, 107_893, 500_000] {
            let tc = df.timecode(f);
            let p: Vec<i64> = tc.split([':', ';']).map(|x| x.parse().unwrap()).collect();
            assert_eq!(df.frames_of(p[0], p[1], p[2], p[3]), f, "{tc}");
        }
        let df60 = Timebase::new(59.94).unwrap();
        assert!(df60.drop);
        assert_eq!(df60.timecode(3600), "00:01:00;04");
        let ndf = Timebase::new(23.976).unwrap();
        assert!(!ndf.drop);
        assert_eq!(ndf.base, 24);
        assert_eq!(ndf.timecode(ndf.frames_of(1, 0, 0, 0)), "01:00:00:00");
        assert_eq!(Timebase::new(25.0).unwrap().timecode(25 * 3661 + 3), "01:01:01:03");
        assert!(Timebase::new(0.0).is_err());
        assert!(Timebase::new(f64::NAN).is_err());
    }

    #[test]
    fn file_urls_encode_paths() {
        assert_eq!(file_url("/Users/me/My Clip.mov"), "file:///Users/me/My%20Clip.mov");
        assert_eq!(file_url("C:\\Media\\a b.mxf"), "file:///C:/Media/a%20b.mxf");
        assert_eq!(file_url("\\\\?\\C:\\Media\\a.mp4"), "file:///C:/Media/a.mp4");
        assert_eq!(file_url("\\\\server\\share\\a.mp4"), "file://server/share/a.mp4");
        assert_eq!(file_url("/a/ü#?.mov"), "file:///a/%C3%BC%23%3F.mov");
    }
}
