//! The timestamps of a decode run: what FFmpeg's `showinfo` filter says on stderr about every
//! frame it passes, parsed as they arrive.
//!
//! A y4m frame carries no timestamp, and a frame's identity is its **pts in the stream's own
//! ticks** (the cache key; `pts_time` keeps six digits and is not enough), so a run is started
//! with `-vf showinfo=checksum=0,scale=...` and `-hide_banner -nostats -nostdin -loglevel info`,
//! and stderr is read as it is written. **Plain `showinfo` checksums every frame**, which costs
//! 65 % more decode time on FFmpeg 6.1 and 40 % on 9.0; `checksum=0` is free. This is a log
//! channel, not an API, so the parser is deliberately narrow and everything it cannot be sure of
//! is an error (the run is killed and the frame goes through FFmpeg):
//!
//! * a line is a frame only if it holds `] n:<n> pts:<pts>` (the regex
//!   `\] n:\s*(\d+) pts:\s*(-?\d+|NOPTS)`); nothing else on stderr is looked at, which is what
//!   makes the stream's banner, the `User Data=` hex dump an x264 SEI becomes in `showinfo`'s side
//!   data and the closing statistics harmless;
//! * frames are paired with y4m frames **by `n`** — the filter counts from 0 — so a skipped or
//!   repeated `n` (a filter graph that was rebuilt mid-stream numbers again from 0) is an error;
//! * the time base is read from **every** `config in time_base: a/b` line, the pts are meaningless
//!   without it, a frame before the first one is an error, and a time base that *changes* is too;
//! * `pts:NOPTS` is an error: a frame with no timestamp has no identity (the decoder's
//!   best-effort timestamps are what the filter graph sees and what is printed, so a container
//!   with no frame pts — AVI — still has them);
//! * `duration:` is read too. Its last value is the **last frame's own duration**, which
//!   `SourceFrames::last_duration` needs to know where a window to the end of the file ends
//!   (`0`, or absent, is unknown). Under `-copyts -start_at_zero` the pts are already relative to
//!   the container's start, so `SourceFrames::start_us` is `0`.
//!
//! Verified on FFmpeg 6.1.1 and 9.0.2 (the real stderr of both is under
//! `tests/fixtures/showinfo/`), which print these lines identically; anything else disables the
//! path in the process, never the app.

use kerf_core::Rational;

/// One frame as `showinfo` reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShowFrame {
    /// The frame's number in the run, from 0: the y4m frame it belongs to.
    pub n: u64,
    /// Its presentation timestamp in ticks of `time_base`.
    pub pts: i64,
    /// Its duration in the same ticks; `0` when unknown.
    pub duration: i64,
    /// The time base in force when the line was printed.
    pub time_base: Rational,
}

/// Why a run's timestamps cannot be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShowinfoError {
    /// The frame has no timestamp.
    NoPts { n: u64 },
    /// A frame line came before any `config in time_base`.
    NoTimeBase { n: u64 },
    /// A time base that is not a ratio of positive numbers.
    BadTimeBase(String),
    /// The time base changed mid-run.
    TimeBaseChanged { from: Rational, to: Rational },
    /// Frames are numbered consecutively from 0; this one is not the next.
    OutOfSequence { expected: u64, got: u64 },
    /// A frame line whose numbers do not fit.
    Unreadable(String),
}

impl std::fmt::Display for ShowinfoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPts { n } => write!(f, "frame {n} has no timestamp"),
            Self::NoTimeBase { n } => write!(f, "frame {n} was reported before its time base"),
            Self::BadTimeBase(text) => write!(f, "unusable time base {text:?}"),
            Self::TimeBaseChanged { from, to } => {
                write!(
                    f,
                    "the time base changed from {}/{} to {}/{}",
                    from.num, from.den, to.num, to.den
                )
            }
            Self::OutOfSequence { expected, got } => write!(f, "expected frame {expected}, got {got}"),
            Self::Unreadable(line) => write!(f, "unreadable frame line {line:?}"),
        }
    }
}

impl std::error::Error for ShowinfoError {}

/// Reads a run's stderr one line at a time. See the [module](self).
#[derive(Debug, Default)]
pub struct ShowinfoParser {
    time_base: Option<Rational>,
    next: u64,
}

impl ShowinfoParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// The time base in force, once a `config in time_base` line has been seen.
    pub fn time_base(&self) -> Option<Rational> {
        self.time_base
    }

    /// Frames reported so far.
    pub fn frames_seen(&self) -> u64 {
        self.next
    }

    /// One line of stderr (without its newline): the frame it reports, if it reports one.
    pub fn line(&mut self, line: &str) -> Result<Option<ShowFrame>, ShowinfoError> {
        if let Some((_, rest)) = line.split_once("config in time_base:") {
            return self.config(rest).map(|()| None);
        }
        let Some((_, rest)) = line.split_once("] n:") else {
            return Ok(None);
        };
        let Some((n, rest)) = number(rest.trim_start()) else {
            return Ok(None);
        };
        let Some(rest) = rest.strip_prefix(" pts:") else {
            return Ok(None);
        };
        let rest = rest.trim_start();
        let n: u64 = n.parse().map_err(|_| ShowinfoError::Unreadable(line.into()))?;
        if rest.starts_with("NOPTS") {
            return Err(ShowinfoError::NoPts { n });
        }
        let Some((pts, rest)) = number(rest) else {
            return Ok(None);
        };
        let pts: i64 = pts.parse().map_err(|_| ShowinfoError::Unreadable(line.into()))?;
        let duration = rest
            .split_once(" duration:")
            .and_then(|(_, d)| number(d.trim_start()))
            .and_then(|(d, _)| d.parse().ok())
            .unwrap_or(0);
        let time_base = self.time_base.ok_or(ShowinfoError::NoTimeBase { n })?;
        if n != self.next {
            return Err(ShowinfoError::OutOfSequence {
                expected: self.next,
                got: n,
            });
        }
        self.next += 1;
        Ok(Some(ShowFrame {
            n,
            pts,
            duration,
            time_base,
        }))
    }

    /// [`ShowinfoParser::line`] for bytes off a pipe: stderr carries file names and tags, which
    /// are not always UTF-8, and one such line must not end the reading.
    pub fn line_lossy(&mut self, bytes: &[u8]) -> Result<Option<ShowFrame>, ShowinfoError> {
        self.line(&String::from_utf8_lossy(bytes))
    }

    fn config(&mut self, rest: &str) -> Result<(), ShowinfoError> {
        let text = rest.trim_start().split([',', ' ']).next().unwrap_or("");
        let parsed = text.split_once('/').and_then(|(num, den)| {
            let (num, den) = (num.parse().ok()?, den.parse().ok()?);
            Rational::new(num, den)
        });
        let to = parsed.ok_or_else(|| ShowinfoError::BadTimeBase(text.into()))?;
        match self.time_base {
            Some(from) if from != to => Err(ShowinfoError::TimeBaseChanged { from, to }),
            _ => {
                self.time_base = Some(to);
                Ok(())
            }
        }
    }
}

/// A leading integer (`-` allowed) and what follows it.
fn number(s: &str) -> Option<(&str, &str)> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    let len = digits.bytes().take_while(u8::is_ascii_digit).count();
    (len > 0).then(|| s.split_at(s.len() - digits.len() + len))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frames and the time base of one captured stderr.
    fn parse(stderr: &str) -> (Vec<ShowFrame>, Option<Rational>) {
        let mut p = ShowinfoParser::new();
        let frames = stderr
            .lines()
            .filter_map(|l| p.line(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect();
        (frames, p.time_base())
    }

    fn summary(frames: &[ShowFrame]) -> Vec<(u64, i64, i64)> {
        frames.iter().map(|f| (f.n, f.pts, f.duration)).collect()
    }

    macro_rules! fixture {
        ($build:literal, $clip:literal) => {
            include_str!(concat!(
                "../tests/fixtures/showinfo/ffmpeg-",
                $build,
                "-",
                $clip,
                ".stderr"
            ))
        };
    }

    /// Real stderr of the production flag set (`-hide_banner -nostats -nostdin -loglevel info
    /// -copyts -start_at_zero -ss <T> -i clip -an -sn -dn -map 0:v:0 -vf
    /// showinfo=checksum=0,scale=out_range=tv -fps_mode passthrough -f yuv4mpegpipe -pix_fmt
    /// yuv420p pipe:1`) of both FFmpegs, on short x264 all-intra clips. Both builds print the
    /// same. (To regenerate: clips from `color=c=gray:s=16x16:r=<fps>:d=<s>,format=yuv420p,
    /// geq=lum='16+8*N':cb=128:cr=128` through `-c:v libx264 -qp 0 -g 1 -bf 0` — mp4 24 fps
    /// 0.25 s, matroska 30 fps 0.4 s, mpegts 25 fps 0.28 s with `-output_ts_offset 1.4 -muxdelay
    /// 0 -muxpreload 0` — then the flags above on each FFmpeg, stderr to the file, its trailing
    /// spaces stripped as the repo's hygiene hook does.) Both print: the banner, `Stream mapping`, the `Output #0` block *between* the first frames, a
    /// `side data` line and a long hex `User Data=` line per SEI, and the closing statistics.
    #[test]
    fn the_real_stderr_of_both_ffmpegs_gives_the_same_frames() {
        let clips = [
            // 24 fps in an mp4 (1/12288), from the start and from `-ss 0.1` (which drops the
            // frames before the seek point: the first frame reported is number 0 at frame 3's pts).
            (
                fixture!("6.1.1", "a_mp4_seek0"),
                fixture!("9.0.2", "a_mp4_seek0"),
                (1, 12_288),
                vec![
                    (0, 0, 512),
                    (1, 512, 512),
                    (2, 1024, 512),
                    (3, 1536, 512),
                    (4, 2048, 512),
                    (5, 2560, 512),
                ],
            ),
            (
                fixture!("6.1.1", "a_mp4_seek100ms"),
                fixture!("9.0.2", "a_mp4_seek100ms"),
                (1, 12_288),
                vec![(0, 1536, 512), (1, 2048, 512), (2, 2560, 512)],
            ),
            // 30 fps in matroska (1/1000): gaps of 33 and 34, and the last frame lasts 33 although
            // the gap before it was 34 (what `SourceFrames::last_duration` is for).
            (
                fixture!("6.1.1", "b_mkv_seek0"),
                fixture!("9.0.2", "b_mkv_seek0"),
                (1, 1000),
                [0, 33, 67, 100, 133, 167, 200, 233, 267, 300, 333, 367]
                    .iter()
                    .enumerate()
                    .map(|(n, pts)| (n as u64, *pts, 33))
                    .collect(),
            ),
            // 25 fps in an MPEG transport stream (1/90000) that starts 1.4 s in: the pts are
            // already relative to the container's start, and `-ss 0.12` is the frame at 0.12 s.
            (
                fixture!("6.1.1", "c_ts_seek120ms"),
                fixture!("9.0.2", "c_ts_seek120ms"),
                (1, 90_000),
                vec![(0, 10_800, 3600), (1, 14_400, 3600), (2, 18_000, 3600), (3, 21_600, 3600)],
            ),
        ];
        for (v6, v9, tb, want) in clips {
            let (a, tb_a) = parse(v6);
            let (b, tb_b) = parse(v9);
            let tb = Rational::new(tb.0, tb.1);
            assert_eq!((tb_a, tb_b), (tb, tb));
            assert_eq!(summary(&a), want);
            assert_eq!(a, b, "FFmpeg 6.1.1 and 9.0.2 report the same frames");
            assert!(a.iter().all(|f| Some(f.time_base) == tb));
        }
    }

    #[test]
    fn nothing_but_the_frame_lines_and_the_time_base_is_read() {
        let mut p = ShowinfoParser::new();
        for noise in [
            "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'n: 3 pts: 4.mp4':",
            "  Stream #0:0[0x1](und): Video: h264, yuv420p, 32x32, 24 fps, 24 tbr, 12288 tbn (default)",
            "[Parsed_showinfo_0 @ 0x5] config out time_base: 0/0, frame_rate: 0/0",
            "[Parsed_showinfo_0 @ 0x5]   side data - H.26[45] User Data Unregistered SEI message: UUID=dc45e9bd",
            "[Parsed_showinfo_0 @ 0x5] User Data=78323634202d20636f7265",
            "[Parsed_showinfo_0 @ 0x5] color_range:unknown color_space:unknown color_primaries:unknown color_trc:unknown",
            "[h264 @ 0x5] n: 5 no pts here",
            "frame=    3 fps=0.0 q=-0.0 Lsize=       1KiB time=00:00:00.00 bitrate=N/A speed=   0x",
            "",
        ] {
            assert_eq!(p.line(noise), Ok(None), "{noise}");
        }
        assert_eq!((p.frames_seen(), p.time_base()), (0, None));
    }

    #[test]
    fn a_frame_line_reads_whatever_the_padding_and_the_sign() {
        let mut p = ShowinfoParser::new();
        p.line("[Parsed_showinfo_0 @ 0x1] config in time_base: 1/30000, frame_rate: 30000/1001")
            .unwrap();
        let mut frame = |line: &str| p.line(line).unwrap().map(|f| (f.n, f.pts, f.duration));
        assert_eq!(
            frame("[P @ 0x1] n:   0 pts:     -1001 pts_time:-0.0333667 duration:   1001 duration_time:0.0333667 fmt:y"),
            Some((0, -1001, 1001))
        );
        assert_eq!(
            frame("[P @ 0x1] n:1 pts:123456789012 pts_time:4115226 duration:0 duration_time:0 fmt:y"),
            Some((1, 123_456_789_012, 0))
        );
        // No duration (an FFmpeg that does not print it) is unknown, not an error.
        assert_eq!(
            frame("[P @ 0x1] n:   2 pts:   3003 pts_time:0.1001 fmt:y"),
            Some((2, 3003, 0))
        );
        // `duration_time:` is not `duration:`.
        assert_eq!(
            frame("[P @ 0x1] n:   3 pts:   4004 duration_time:0.0333667 fmt:y"),
            Some((3, 4004, 0))
        );
    }

    #[test]
    fn a_frame_without_a_timestamp_a_time_base_or_its_turn_is_an_error() {
        let config = |tb: &str| format!("[Parsed_showinfo_0 @ 0x1] config in time_base: {tb}, frame_rate: 24/1");
        let frame = |n: u64, pts: &str| {
            format!("[Parsed_showinfo_0 @ 0x1] n:{n:4} pts:{pts:>7} pts_time:0 duration:    512 duration_time:0")
        };
        let mut p = ShowinfoParser::new();
        // A frame before the time base says nothing the pts can be read against.
        assert_eq!(p.line(&frame(0, "0")), Err(ShowinfoError::NoTimeBase { n: 0 }));
        p.line(&config("1/12288")).unwrap();
        assert_eq!(p.line(&frame(0, "NOPTS")), Err(ShowinfoError::NoPts { n: 0 }));
        assert!(p.line(&frame(0, "0")).unwrap().is_some());
        // Numbers are consecutive: a gap, and a repeat, are both a count that no longer matches.
        assert_eq!(
            p.line(&frame(2, "1024")),
            Err(ShowinfoError::OutOfSequence { expected: 1, got: 2 })
        );
        assert_eq!(
            p.line(&frame(0, "0")),
            Err(ShowinfoError::OutOfSequence { expected: 1, got: 0 })
        );
        assert!(p.line(&frame(1, "512")).unwrap().is_some());
        // The same time base again is fine (a graph that was configured twice); another is not.
        p.line(&config("1/12288")).unwrap();
        assert!(p.line(&frame(2, "1024")).unwrap().is_some());
        let changed = p.line(&config("1/90000")).unwrap_err();
        assert_eq!(
            changed,
            ShowinfoError::TimeBaseChanged {
                from: Rational { num: 1, den: 12_288 },
                to: Rational { num: 1, den: 90_000 }
            }
        );
        assert!(changed.to_string().contains("1/12288") && changed.to_string().contains("1/90000"));
        // A rebuilt graph renumbers from 0.
        assert_eq!(
            p.line(&frame(0, "0")),
            Err(ShowinfoError::OutOfSequence { expected: 3, got: 0 })
        );
        for bad in ["0/0", "1/0", "0/1", "abc", "1/x", "-1/24", "99999999999/1"] {
            assert!(
                matches!(ShowinfoParser::new().line(&config(bad)), Err(ShowinfoError::BadTimeBase(_))),
                "{bad}"
            );
        }
        // A number that does not fit is unreadable, not a wrapped timestamp.
        let mut p = ShowinfoParser::new();
        p.line(&config("1/24")).unwrap();
        assert!(matches!(
            p.line(&frame(0, "99999999999999999999")),
            Err(ShowinfoError::Unreadable(_))
        ));
    }

    #[test]
    fn a_line_that_is_not_utf8_is_read_lossily_not_dropped() {
        let mut p = ShowinfoParser::new();
        p.line_lossy(b"[Parsed_showinfo_0 @ 0x1] config in time_base: 1/1000, frame_rate: 30/1")
            .unwrap();
        let mut line = b"[Parsed_showinfo_0 @ 0x1] n:   0 pts:     33 pts_time:0.033 duration:     33 title=\xe9\xe8".to_vec();
        let f = p.line_lossy(&line).unwrap().unwrap();
        assert_eq!((f.pts, f.duration), (33, 33));
        line.splice(.., b"title \xe9\xe8 \xff".iter().copied());
        assert_eq!(p.line_lossy(&line), Ok(None));
    }
}
