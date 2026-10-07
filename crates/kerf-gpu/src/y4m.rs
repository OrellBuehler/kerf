//! A streaming `yuv4mpegpipe` reader: the frames of one decode run, one at a time, read
//! **straight into the plane buffers** that become a [`YuvFrame`] (no whole-stream buffer, no
//! second copy, no zero-fill first).
//!
//! The stream states its own size (`W..H..`), and that is compared with the size the probe
//! said **before a plane is allocated**: a JPEG with an EXIF orientation probes as 480x270 and
//! decodes as 270x480, the same number of bytes and a sheared mess if taken on trust. A
//! mismatch is [`Y4mError::Size`] (`GpuError::Unsupported`: that frame goes through FFmpeg), and
//! because nothing past the header is read for it, the memory a run can be made to ask for is
//! bounded by the probe, not by what the child writes.
//!
//! The layout must be 8-bit 4:2:0 (`C420`, `C420jpeg`, `C420mpeg2`, `C420paldv`, or no `C` tag,
//! which the format reads as `420jpeg`; FFmpeg 6.1 writes `mpeg2` and 9.0 `jpeg` for the same
//! limited-range clip). `C420p10` starts with "420" too and is **not** 8-bit.
//!
//! The end of the stream is a state, not an error: no bytes at all (a seek past the last
//! frame) or a header with no frame after it is "no frame", and a stream that ends *between*
//! frames is finished; one that ends inside a header or a frame is [`Y4mError::Truncated`].
//! Partial reads are `Read`'s business (`read_to_end` over a `take` loops, and retries
//! `Interrupted`); a pipe hands over a few KiB at a time.

use std::io::{ErrorKind, Read};

use crate::gpu::GpuError;
use crate::source::{chroma_size, yuv420p_len, YuvFrame, MAX_SIDE};

/// The longest stream header accepted (FFmpeg writes about 80 bytes).
const MAX_HEADER: usize = 4096;
/// The longest `FRAME` line accepted (it is `FRAME`, or `FRAME` and a few parameters).
const MAX_FRAME_LINE: usize = 256;

/// Why a frame could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Y4mError {
    /// Not a `yuv4mpegpipe` stream, or a line that is not what the format says.
    Malformed(String),
    /// A layout that is not 8-bit 4:2:0, or a size the compositor does not take.
    Format(String),
    /// The stream's picture is not the size the probe said.
    Size { found: (u32, u32), expected: (u32, u32) },
    /// The stream ended inside a header or a frame.
    Truncated(String),
    /// Reading failed.
    Io(String),
}

impl std::fmt::Display for Y4mError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(why) | Self::Format(why) | Self::Truncated(why) | Self::Io(why) => f.write_str(why),
            Self::Size { found, expected } => write!(
                f,
                "it decodes as {}x{} but was probed as {}x{} (an EXIF or display orientation the probe did not apply)",
                found.0, found.1, expected.0, expected.1
            ),
        }
    }
}

impl std::error::Error for Y4mError {}

/// What the caller does about it: a size or layout the compositor does not draw is
/// `Unsupported` (FFmpeg renders that frame), anything else is a failed decode.
impl From<Y4mError> for GpuError {
    fn from(e: Y4mError) -> Self {
        match e {
            Y4mError::Size { .. } | Y4mError::Format(_) => GpuError::Unsupported(e.to_string()),
            Y4mError::Malformed(_) | Y4mError::Truncated(_) | Y4mError::Io(_) => GpuError::Decode(e.to_string()),
        }
    }
}

/// Reads the frames of one `yuv4mpegpipe` stream. The header is read with the first frame.
pub struct Y4mReader<R> {
    from: R,
    expected: (u32, u32),
    started: bool,
    frames: u64,
}

impl<R: Read> Y4mReader<R> {
    /// A reader of a stream whose picture must be `expected` (width, height) — the probe's.
    pub fn new(from: R, expected: (u32, u32)) -> Self {
        Self {
            from,
            expected,
            started: false,
            frames: 0,
        }
    }

    /// How many frames have been read: the `n` of the frame `showinfo` numbers the same way.
    pub fn frames_read(&self) -> u64 {
        self.frames
    }

    /// The next frame, or `None` at the end of the stream (see the [module](self)).
    pub fn next_frame(&mut self) -> Result<Option<YuvFrame>, Y4mError> {
        if !self.started {
            let Some(header) = read_line(&mut self.from, MAX_HEADER)? else {
                return Ok(None);
            };
            let found = parse_header(&header)?;
            if found != self.expected {
                return Err(Y4mError::Size {
                    found,
                    expected: self.expected,
                });
            }
            let (w, h) = found;
            if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || yuv420p_len(w, h).is_none() {
                return Err(Y4mError::Format(format!(
                    "a {w}x{h} picture (the compositor takes 1 to {MAX_SIDE} px a side)"
                )));
            }
            self.started = true;
        }
        let Some(marker) = read_line(&mut self.from, MAX_FRAME_LINE)? else {
            return Ok(None);
        };
        if marker != b"FRAME" && !marker.starts_with(b"FRAME ") {
            return Err(Y4mError::Malformed("expected a FRAME marker".into()));
        }
        let (w, h) = self.expected;
        let (cw, ch) = chroma_size(w, h);
        let plane = |from: &mut R, w: u32, h: u32| read_plane(from, w as usize * h as usize);
        let frame = YuvFrame {
            width: w,
            height: h,
            y: plane(&mut self.from, w, h)?,
            u: plane(&mut self.from, cw, ch)?,
            v: plane(&mut self.from, cw, ch)?,
        };
        self.frames += 1;
        Ok(Some(frame))
    }
}

/// `n` bytes into a new buffer, read directly into its spare capacity.
fn read_plane<R: Read>(from: &mut R, n: usize) -> Result<Vec<u8>, Y4mError> {
    let mut plane = Vec::with_capacity(n);
    let got = from.by_ref().take(n as u64).read_to_end(&mut plane).map_err(io_error)?;
    if got == n {
        Ok(plane)
    } else {
        Err(Y4mError::Truncated(format!(
            "the stream ended {got} bytes into a {n}-byte plane"
        )))
    }
}

fn io_error(e: std::io::Error) -> Y4mError {
    Y4mError::Io(format!("reading the decoded frames: {e}"))
}

/// One line without its newline; `None` when the stream ends before its first byte. Bytes are
/// read one at a time, deliberately unbuffered: a buffer would swallow the start of the planes
/// that follow, and a header and a `FRAME` marker are under a hundred bytes.
fn read_line<R: Read>(from: &mut R, max: usize) -> Result<Option<Vec<u8>>, Y4mError> {
    let mut line = Vec::new();
    let mut byte = [0u8];
    loop {
        match from.read(&mut byte) {
            Ok(0) if line.is_empty() => return Ok(None),
            Ok(0) => return Err(Y4mError::Truncated("the stream ended inside a header line".into())),
            Ok(_) if byte[0] == b'\n' => return Ok(Some(line)),
            Ok(_) if line.len() >= max => return Err(Y4mError::Malformed(format!("a header line longer than {max} bytes"))),
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(io_error(e)),
        }
    }
}

/// The picture size of a `YUV4MPEG2` header line, once it is known to be 8-bit 4:2:0.
fn parse_header(line: &[u8]) -> Result<(u32, u32), Y4mError> {
    let text = std::str::from_utf8(line).map_err(|_| Y4mError::Malformed("a non-text stream header".into()))?;
    let mut tokens = text.split_ascii_whitespace();
    if tokens.next() != Some("YUV4MPEG2") {
        return Err(Y4mError::Malformed(format!("not a yuv4mpeg stream: {text:?}")));
    }
    let (mut width, mut height) = (None, None);
    for token in tokens {
        let Some(tag) = token.chars().next() else { continue };
        let value = &token[tag.len_utf8()..];
        match tag {
            'W' => width = value.parse::<u32>().ok(),
            'H' => height = value.parse::<u32>().ok(),
            // No `C` tag is `420jpeg`; `420p10` and the rest are other layouts.
            'C' if !matches!(value, "420" | "420jpeg" | "420mpeg2" | "420paldv") => {
                return Err(Y4mError::Format(format!("the stream is C{value}, not 8-bit 4:2:0")));
            }
            _ => {}
        }
    }
    match (width, height) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => Err(Y4mError::Malformed(format!("no picture size in {text:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream: `header`, then each frame as a `FRAME` line and its planes.
    fn stream(header: &str, frames: &[Vec<u8>]) -> Vec<u8> {
        let mut v = format!("{header}\n").into_bytes();
        for f in frames {
            v.extend_from_slice(b"FRAME\n");
            v.extend_from_slice(f);
        }
        v
    }

    /// The bytes of a `w` x `h` frame whose planes are filled with `y`, `u`, `v`.
    fn planes(w: u32, h: u32, (y, u, v): (u8, u8, u8)) -> Vec<u8> {
        let (cw, ch) = chroma_size(w, h);
        let mut f = vec![y; (w * h) as usize];
        f.extend(vec![u; (cw * ch) as usize]);
        f.extend(vec![v; (cw * ch) as usize]);
        f
    }

    fn read_all(bytes: &[u8], size: (u32, u32)) -> Result<Vec<YuvFrame>, Y4mError> {
        let mut r = Y4mReader::new(bytes, size);
        let mut out = Vec::new();
        while let Some(f) = r.next_frame()? {
            out.push(f);
        }
        assert_eq!(r.frames_read(), out.len() as u64);
        Ok(out)
    }

    /// Hands out one byte per `read`, and an `Interrupted` before every third: a pipe that dribbles.
    struct Dribble<'a>(&'a [u8], usize);

    impl Read for Dribble<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.1 += 1;
            if self.1.is_multiple_of(3) {
                return Err(ErrorKind::Interrupted.into());
            }
            let n = self.0.len().min(1).min(buf.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn frames_come_out_with_their_planes_split_and_the_header_read_once() {
        let bytes = stream(
            "YUV4MPEG2 W4 H2 F30:1 Ip A1:1 C420jpeg XYSCSS=420JPEG",
            &[planes(4, 2, (1, 2, 3)), planes(4, 2, (4, 5, 6))],
        );
        let frames = read_all(&bytes, (4, 2)).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!((frames[0].y.len(), frames[0].u.len(), frames[0].v.len()), (8, 2, 2));
        assert_eq!((frames[0].y[7], frames[0].u[1], frames[0].v[0]), (1, 2, 3));
        assert_eq!((frames[1].y[0], frames[1].u[0], frames[1].v[1]), (4, 5, 6));
        assert!(frames.iter().all(YuvFrame::is_consistent));
        // Odd sizes round the chroma up; `FRAME` may carry parameters.
        let mut odd = b"YUV4MPEG2 W3 H3 C420\nFRAME Ixyz\n".to_vec();
        odd.extend(planes(3, 3, (9, 8, 7)));
        let f = &read_all(&odd, (3, 3)).unwrap()[0];
        assert_eq!((f.y.len(), f.u.len(), f.chroma_size()), (9, 4, (2, 2)));
    }

    #[test]
    fn a_pipe_that_hands_over_a_byte_at_a_time_reads_the_same() {
        let bytes = stream(
            "YUV4MPEG2 W6 H4 C420mpeg2",
            &[planes(6, 4, (10, 20, 30)), planes(6, 4, (11, 21, 31))],
        );
        let mut r = Y4mReader::new(Dribble(&bytes, 0), (6, 4));
        let a = r.next_frame().unwrap().unwrap();
        let b = r.next_frame().unwrap().unwrap();
        assert!(r.next_frame().unwrap().is_none());
        assert_eq!(read_all(&bytes, (6, 4)).unwrap(), [a, b]);
    }

    #[test]
    fn the_end_of_the_stream_is_no_frame_unless_it_falls_inside_one() {
        // Nothing at all (a seek past the last frame), and a header with no frame.
        assert!(read_all(b"", (4, 2)).unwrap().is_empty());
        assert!(read_all(b"YUV4MPEG2 W4 H2\n", (4, 2)).unwrap().is_empty());
        // Ending between frames is finished, and stays finished.
        let bytes = stream("YUV4MPEG2 W4 H2", &[planes(4, 2, (1, 1, 1))]);
        let mut r = Y4mReader::new(&bytes[..], (4, 2));
        assert!(r.next_frame().unwrap().is_some());
        assert!(r.next_frame().unwrap().is_none());
        assert!(r.next_frame().unwrap().is_none());
        // Inside a header, a FRAME line or a plane it is an error (the header is 16 bytes, the
        // FRAME line 6 more, the planes 12).
        assert_eq!(bytes.len(), 16 + 6 + 12);
        for cut in [5, 16 + 2, 16 + 6, 16 + 6 + 3, bytes.len() - 1] {
            let r = read_all(&bytes[..cut], (4, 2));
            assert!(matches!(r, Err(Y4mError::Truncated(_))), "cut at {cut}: {r:?}");
        }
    }

    #[test]
    fn a_picture_that_is_not_the_probed_size_is_refused_before_its_planes_are_read() {
        let bytes = stream("YUV4MPEG2 W2 H4 C420", &[planes(2, 4, (1, 1, 1))]);
        let mut rest: &[u8] = &bytes;
        let mut r = Y4mReader::new(&mut rest, (4, 2));
        let err = r.next_frame().unwrap_err();
        assert_eq!(
            err,
            Y4mError::Size {
                found: (2, 4),
                expected: (4, 2)
            }
        );
        assert!(err.to_string().contains("2x4") && err.to_string().contains("4x2"));
        assert!(matches!(GpuError::from(err), GpuError::Unsupported(_)));
        // Only the header line went: the 12-byte frame and its marker are untouched.
        assert_eq!(rest.len(), 6 + 12);
    }

    #[test]
    fn only_8_bit_420_is_read() {
        let ok = |c: &str| read_all(&stream(&format!("YUV4MPEG2 W4 H2 {c}"), &[planes(4, 2, (0, 0, 0))]), (4, 2));
        for c in ["C420", "C420jpeg", "C420mpeg2", "C420paldv", "Ip"] {
            assert!(ok(c).is_ok(), "{c}");
        }
        for c in ["C420p10", "C420p12", "C422", "C444", "Cmono", "C420alpha"] {
            let e = ok(c).unwrap_err();
            assert!(matches!(e, Y4mError::Format(_)), "{c}: {e:?}");
            assert!(matches!(GpuError::from(e), GpuError::Unsupported(_)));
        }
        // A picture the compositor does not take, even when the probe agrees.
        assert!(matches!(read_all(b"YUV4MPEG2 W0 H0\n", (0, 0)), Err(Y4mError::Format(_))));
        let wide = format!("YUV4MPEG2 W{} H2\n", MAX_SIDE + 1);
        assert!(matches!(
            read_all(wide.as_bytes(), (MAX_SIDE + 1, 2)),
            Err(Y4mError::Format(_))
        ));
    }

    #[test]
    fn a_stream_that_is_not_y4m_is_malformed() {
        let e = |bytes: &[u8]| read_all(bytes, (4, 2)).unwrap_err();
        assert!(matches!(e(b"nonsense\nFRAME\n"), Y4mError::Malformed(_)));
        assert!(matches!(e(b"YUV4MPEG2 F30:1\n"), Y4mError::Malformed(_)), "no size");
        assert!(matches!(e(b"YUV4MPEG2 W4 H2\nNOPE\n"), Y4mError::Malformed(_)));
        assert!(matches!(e(b"YUV4MPEG2 W4 H2\nFRAMES\n"), Y4mError::Malformed(_)));
        assert!(matches!(e(b"YUV4MPEG2 W\xff H2\n"), Y4mError::Malformed(_)), "not text");
        let long = [b"YUV4MPEG2 W4 H2 X".as_slice(), &[b'a'; MAX_HEADER]].concat();
        assert!(matches!(e(&long), Y4mError::Malformed(_)), "a header that never ends");
        assert!(matches!(GpuError::from(e(b"junk\n")), GpuError::Decode(_)));
        assert!(matches!(GpuError::from(Y4mError::Truncated("x".into())), GpuError::Decode(_)));
    }

    /// Real output of both FFmpegs (`-vf showinfo=checksum=0,scale=out_range=tv ... -f
    /// yuv4mpegpipe -pix_fmt yuv420p`): a 16x16 ramp, three frames from `-ss 0.1`. The headers
    /// differ (`C420mpeg2` / `C420jpeg`), the pictures do not.
    #[test]
    fn real_streams_of_both_ffmpegs_read_the_same_pictures() {
        let v6 = &include_bytes!("../tests/fixtures/y4m/ffmpeg-6.1.1-a_mp4_seek100ms.y4m")[..];
        let v9 = &include_bytes!("../tests/fixtures/y4m/ffmpeg-9.0.2-a_mp4_seek100ms.y4m")[..];
        assert!(v6.starts_with(b"YUV4MPEG2 W16 H16 F24:1 Ip A1:1 C420mpeg2"));
        assert!(v9.starts_with(b"YUV4MPEG2 W16 H16 F24:1 Ip A1:1 C420jpeg"));
        let (a, b) = (read_all(v6, (16, 16)).unwrap(), read_all(v9, (16, 16)).unwrap());
        assert_eq!(a.len(), 3);
        assert_eq!(a, b);
        // The ramp: frame n is luma 16 + 8n, and `-ss 0.1` of 24 fps starts at frame 3.
        assert_eq!(a.iter().map(|f| f.y[0]).collect::<Vec<_>>(), [40, 48, 56]);
        assert!(a.iter().all(|f| f.is_consistent() && f.u.iter().all(|c| *c == 128)));
        // The same bytes through a stream that dribbles.
        let mut r = Y4mReader::new(Dribble(v9, 0), (16, 16));
        assert_eq!(r.next_frame().unwrap().unwrap(), b[0]);
    }
}
