//! SMPTE timecode at the timeline's rate: formatting and typed entry.
//!
//! The rate is the timeline's `f64` (a probed `30000/1001` is `29.97002997…`, an export preset's
//! `29.97`), so drop-frame is decided by closeness to `30000/1001` and `60000/1001`, not by
//! equality. A timecode second is the nominal count (30 at 29.97, 24 at 23.976); drop-frame skips
//! the labels `;00` and `;01` (`;00`–`;03` at 59.94) at the start of every minute but each tenth, so
//! the label keeps up with the clock. Pure, and mirrored by `frontend/src/lib/timecode.ts`.

/// Frames are held to this on the way in and out: past it the hours field outgrows any cut, and
/// every intermediate stays an exact integer in an `f64` (the TS mirror's arithmetic).
pub const MAX_TIMECODE_FRAME: i64 = 999_999_999_999;

/// A typed field longer than this is refused rather than overflowing.
const MAX_FIELD_DIGITS: usize = 9;

/// How frames are counted into a timecode: frames per timecode second and frames dropped per
/// minute (0 for non-drop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimecodeRate {
    pub base: i64,
    pub drop: i64,
}

/// The frames a timecode second holds at `fps`: 30 at 29.97, 30 for an unusable rate.
pub fn nominal_rate(fps: f64) -> i64 {
    if fps.is_finite() && fps > 0.0 {
        (fps.round() as i64).clamp(1, 1000)
    } else {
        30
    }
}

/// Whether `fps` is an NTSC rate that counts drop-frame: 29.97 or 59.94.
pub fn is_drop_frame_rate(fps: f64) -> bool {
    [30.0, 60.0].iter().any(|base| (fps - base * 1000.0 / 1001.0).abs() < 0.01)
}

impl TimecodeRate {
    /// The count at `fps`; `drop_frame` asks for drop-frame where the rate has it.
    pub fn new(fps: f64, drop_frame: bool) -> Self {
        let base = nominal_rate(fps);
        let drop = if drop_frame && is_drop_frame_rate(fps) { base / 15 } else { 0 };
        Self { base, drop }
    }

    pub fn is_drop(self) -> bool {
        self.drop > 0
    }

    /// `(hours, minutes, seconds, frames)` of a frame count (held to `0..=MAX_TIMECODE_FRAME`).
    pub fn fields(self, frame: i64) -> [i64; 4] {
        let mut n = frame.clamp(0, MAX_TIMECODE_FRAME);
        let base = self.base;
        if self.is_drop() {
            let per_min = base * 60 - self.drop;
            let per_ten = per_min * 10 + self.drop;
            let (tens, rest) = (n / per_ten, n % per_ten);
            n += self.drop * 9 * tens;
            if rest > self.drop {
                n += self.drop * ((rest - self.drop) / per_min);
            }
        }
        [n / (base * 3600), n / (base * 60) % 60, n / base % 60, n % base]
    }

    /// The frame count of a label. A drop-frame label that does not exist (`00;01;00;00`) is the
    /// first one of its minute that does.
    pub fn frames(self, h: i64, m: i64, s: i64, f: i64) -> i64 {
        let mut f = f;
        if self.is_drop() && m % 10 != 0 && s == 0 && f < self.drop {
            f = self.drop;
        }
        let mut n = ((h * 3600 + m * 60 + s) * self.base) + f;
        if self.is_drop() {
            let minutes = h * 60 + m;
            n -= self.drop * (minutes - minutes / 10);
        }
        n
    }

    /// `HH:MM:SS:FF`, or `HH;MM;SS;FF` when counting drop-frame.
    pub fn format(self, frame: i64) -> String {
        let sep = if self.is_drop() { ';' } else { ':' };
        let [h, m, s, f] = self.fields(frame);
        format!("{h:02}{sep}{m:02}{sep}{s:02}{sep}{f:02}")
    }

    /// A typed timecode as a frame count, the way an NLE's timecode field reads it:
    /// - `01:02:03:04`, with any of `:;.,` between the fields, and fewer fields fill from the right
    ///   (`1.00` is a second);
    /// - bare digits are right-aligned pairs, `HHMMSSFF` (`1000` is `00:00:10:00`);
    /// - a leading `+` / `-` is relative to `current`: `+15` is 15 frames on, `-1.00` a second back.
    ///
    /// A field past its range (`90` frames) carries into the next before a drop-frame label is
    /// read, as in Premiere. `None` for what is not a timecode (fields that are all empty, `:`, too);
    /// the result is held to `0..=MAX_TIMECODE_FRAME`.
    pub fn parse(self, input: &str, current: i64) -> Option<i64> {
        let text = input.trim();
        let (sign, body) = match text.as_bytes().first()? {
            b'+' => (Some(1), &text[1..]),
            b'-' => (Some(-1), &text[1..]),
            _ => (None, text),
        };
        let digits = |s: &str| s.len() <= MAX_FIELD_DIGITS && s.bytes().all(|b| b.is_ascii_digit());
        let field = |s: &str| if s.is_empty() { Some(0) } else { s.parse::<i64>().ok() };
        let parts: Vec<&str> = body.split([':', ';', '.', ',']).collect();
        let fields: Vec<i64> = if parts.len() == 1 {
            let bare = parts[0];
            if bare.is_empty() || !bare.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            if sign.is_some() {
                if !digits(bare) {
                    return None;
                }
                vec![bare.parse().ok()?]
            } else {
                if bare.len() > 8 {
                    return None;
                }
                let mut pairs = Vec::new();
                let mut end = bare.len();
                while end > 0 {
                    let start = end.saturating_sub(2);
                    pairs.push(bare[start..end].parse().ok()?);
                    end = start;
                }
                pairs.reverse();
                pairs
            }
        } else {
            if parts.len() > 4 || !parts.iter().all(|p| digits(p)) || parts.iter().all(|p| p.is_empty()) {
                return None;
            }
            parts.iter().map(|p| field(p)).collect::<Option<_>>()?
        };
        let frames = if sign.is_some() && parts.len() == 1 {
            fields[0]
        } else {
            let mut hmsf = [0; 4];
            hmsf[4 - fields.len()..].copy_from_slice(&fields);
            // A field past its range carries into the next, then the label is read as a label.
            let [h, m, s, f] = hmsf;
            let (s, f) = (s + f / self.base, f % self.base);
            let (m, s) = (m + s / 60, s % 60);
            let (h, m) = (h + m / 60, m % 60);
            self.frames(h, m, s, f)
        };
        let frame = match sign {
            Some(sign) => current.clamp(0, MAX_TIMECODE_FRAME) + sign * frames.min(MAX_TIMECODE_FRAME),
            None => frames,
        };
        Some(frame.clamp(0, MAX_TIMECODE_FRAME))
    }
}

/// The frame shown at `seconds`: the one it falls inside, counted at `fps` (30 when unusable).
pub fn frame_at(seconds: f64, fps: f64) -> i64 {
    let rate = if fps.is_finite() && fps > 0.0 { fps } else { 30.0 };
    let t = if seconds.is_finite() { seconds.max(0.0) } else { 0.0 };
    ((t * rate + 1e-7).floor() as i64).min(MAX_TIMECODE_FRAME)
}

/// The project's timecode of `seconds` at `fps`, drop-frame where the rate has it.
pub fn format_timecode(seconds: f64, fps: f64) -> String {
    TimecodeRate::new(fps, true).format(frame_at(seconds, fps))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC: f64 = 30000.0 / 1001.0;
    const NTSC60: f64 = 60000.0 / 1001.0;

    // Every case here is replayed by `frontend/src/lib/timecode.test.ts`.

    #[test]
    fn drop_frame_is_29_97_and_59_94_however_the_rate_is_written() {
        for fps in [NTSC, 29.97, 29.970_029_97, NTSC60, 59.94] {
            assert!(TimecodeRate::new(fps, true).is_drop(), "{fps}");
            assert!(!TimecodeRate::new(fps, false).is_drop(), "{fps}");
        }
        for fps in [
            23.976,
            24.0,
            25.0,
            30.0,
            50.0,
            60.0,
            119.88,
            0.0,
            -29.97,
            f64::NAN,
            f64::INFINITY,
        ] {
            assert!(!TimecodeRate::new(fps, true).is_drop(), "{fps}");
        }
        assert_eq!(TimecodeRate::new(NTSC, true), TimecodeRate { base: 30, drop: 2 });
        assert_eq!(TimecodeRate::new(59.94, true), TimecodeRate { base: 60, drop: 4 });
        assert_eq!(TimecodeRate::new(23.976, true), TimecodeRate { base: 24, drop: 0 });
        assert_eq!(nominal_rate(f64::NAN), 30);
        assert_eq!(nominal_rate(1e300), 1000);
    }

    #[test]
    fn drop_frame_labels() {
        let r = TimecodeRate::new(NTSC, true);
        for (frame, label) in [
            (0, "00;00;00;00"),
            (1799, "00;00;59;29"),
            (1800, "00;01;00;02"),
            (3598, "00;02;00;02"),
            (17981, "00;09;59;29"),
            (17982, "00;10;00;00"),
            (17983, "00;10;00;01"),
            (107_892, "01;00;00;00"),
        ] {
            assert_eq!(r.format(frame), label);
        }
        assert_eq!(TimecodeRate::new(NTSC, false).format(1800), "00:01:00:00");
        assert_eq!(TimecodeRate::new(NTSC60, true).format(3600), "00;01;00;04");
        assert_eq!(TimecodeRate::new(25.0, true).format(90_000), "01:00:00:00");
    }

    #[test]
    fn every_drop_frame_label_reads_back_as_its_frame() {
        for fps in [NTSC, NTSC60] {
            let r = TimecodeRate::new(fps, true);
            for frame in (0..40_000).chain(1_000_000..1_002_000) {
                let [h, m, s, f] = r.fields(frame);
                assert_eq!(r.frames(h, m, s, f), frame, "{fps} {frame}");
                assert_eq!(r.parse(&r.format(frame), 0), Some(frame), "{fps} {frame}");
            }
        }
    }

    #[test]
    fn a_dropped_label_is_the_first_frame_of_its_minute() {
        let r = TimecodeRate::new(NTSC, true);
        assert_eq!(r.parse("00;01;00;00", 0), Some(1800));
        assert_eq!(r.parse("00;01;00;01", 0), Some(1800));
        assert_eq!(r.parse("00;10;00;00", 0), Some(17982));
    }

    #[test]
    fn seconds_format_at_the_frame_they_fall_in() {
        assert_eq!(format_timecode(10.0, 25.0), "00:00:10:00");
        assert_eq!(format_timecode(1.0, NTSC), "00;00;00;29");
        assert_eq!(format_timecode(1.001, NTSC), "00;00;01;00");
        assert_eq!(format_timecode(60.06, NTSC), "00;01;00;02");
        assert_eq!(format_timecode(-3.0, 24.0), "00:00:00:00");
        assert_eq!(format_timecode(f64::NAN, 24.0), "00:00:00:00");
        assert_eq!(format_timecode(2.0, f64::NAN), "00:00:02:00");
        assert_eq!(frame_at(1e300, 30.0), MAX_TIMECODE_FRAME);
        assert_eq!(frame_at(2.0 / 3.0, 3.0), 2);
    }

    #[test]
    fn typed_forms() {
        let r = TimecodeRate::new(25.0, true);
        for (text, current, frame) in [
            ("00:00:10:00", 0, Some(250)),
            ("00;00;10;00", 0, Some(250)),
            ("0.0.10.0", 0, Some(250)),
            ("0,0,10,0", 0, Some(250)),
            ("1000", 0, Some(250)),
            ("10000", 0, Some(25 * 60)),
            ("1", 0, Some(1)),
            ("1.00", 0, Some(25)),
            ("1:", 0, Some(25)),
            (":5", 0, Some(5)),
            ("  12:00  ", 0, Some(300)),
            ("90", 0, Some(90)),
            ("1:90", 0, Some(115)),
            ("+15", 100, Some(115)),
            ("-15", 100, Some(85)),
            ("-1.00", 100, Some(75)),
            ("+1:00:00", 0, Some(1500)),
            ("+1:00:00:00", 0, Some(90_000)),
            ("-500", 100, Some(0)),
            ("01020304", 0, Some((3600 + 2 * 60 + 3) * 25 + 4)),
        ] {
            assert_eq!(r.parse(text, current), frame, "{text:?}");
        }
        for text in [
            "",
            " ",
            "+",
            "-",
            "abc",
            "1:2:3:4:5",
            "123456789",
            "1:a",
            "1 2",
            "0x10",
            "+0x1",
            "1e3",
            "１２",
            "1:+5",
            "--1",
            "1234567890:0",
            "+1234567890",
            ":",
            ".",
            "::",
            ":;.,",
            "+:",
            "-.",
            "+::",
        ] {
            assert_eq!(r.parse(text, 0), None, "{text:?}");
        }
    }

    #[test]
    fn typed_drop_frame() {
        let r = TimecodeRate::new(NTSC, true);
        assert_eq!(r.parse("00;01;00;02", 0), Some(1800));
        assert_eq!(r.parse("00:01:00:02", 0), Some(1800));
        assert_eq!(r.parse("1000200", 0), Some(107_952));
        assert_eq!(r.parse("+1.00", 10), Some(40));
        // A field past its range carries first, and the label it makes is read as drop-frame.
        assert_eq!(r.parse("0:0:0:45", 0), Some(45));
        assert_eq!(r.parse("0:0:60:0", 0), Some(1800));
        assert_eq!(r.parse("0:0:0:1800", 0), Some(1800));
        assert_eq!(r.parse("0:0:59:45", 0), r.parse("00;01;00;15", 0));
        assert_eq!(r.parse("0:70:0:0", 0), r.parse("1:10:0:0", 0));
        assert_eq!(r.parse("0:70:0:0", 0), Some(125_874));
    }

    #[test]
    fn huge_entries_are_held_not_wrapped() {
        let r = TimecodeRate::new(NTSC, true);
        assert_eq!(r.parse("999999999:59:59:29", 0), Some(MAX_TIMECODE_FRAME));
        assert_eq!(r.parse("+999999999", MAX_TIMECODE_FRAME), Some(MAX_TIMECODE_FRAME));
        assert_eq!(r.parse("-999999999", i64::MIN), Some(0));
        assert_eq!(r.format(i64::MAX), r.format(MAX_TIMECODE_FRAME));
        let r = TimecodeRate::new(1e300, true);
        assert_eq!(
            r.parse("999999999:999999999:999999999:999999999", 0),
            Some(MAX_TIMECODE_FRAME)
        );
    }
}
