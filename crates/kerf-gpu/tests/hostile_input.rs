//! Hostile input for what the GPU path reads off ffmpeg: a `yuv4mpegpipe` stream on stdout and the
//! `showinfo` log on stderr. Seeded mutations (`kerf_core::hostile`) under `catch_unwind`: a mangled
//! stream or log is an error that sends the frame to FFmpeg, never a panic.

use kerf_core::hostile::assert_no_panic;
use kerf_gpu::showinfo::ShowinfoParser;
use kerf_gpu::y4m::Y4mReader;

const ROUNDS: usize = 1500;

/// A 4x2 stream of two frames, the second marker with a parameter.
fn y4m_stream() -> Vec<u8> {
    let mut v = b"YUV4MPEG2 W4 H2 F30000:1001 Ip A1:1 C420jpeg XYSCSS=420JPEG\n".to_vec();
    for (marker, fill) in [(&b"FRAME\n"[..], 16u8), (&b"FRAME Ixyz\n"[..], 200u8)] {
        v.extend_from_slice(marker);
        v.extend(std::iter::repeat_n(fill, 8 + 2 + 2));
    }
    v
}

#[test]
fn hostile_y4m_streams_never_panic() {
    assert_no_panic("y4m", &[&y4m_stream()], ROUNDS, |bytes| {
        for expected in [(4, 2), (0, 0), (u32::MAX, u32::MAX)] {
            let mut reader = Y4mReader::new(bytes, expected);
            for _ in 0..8 {
                match reader.next_frame() {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
            let _ = reader.frames_read();
        }
    });
}

const SHOWINFO: &str = "\
[Parsed_showinfo_0 @ 0x55d0] config in time_base: 1/30000, frame_rate: 30000/1001\n\
[Parsed_showinfo_0 @ 0x55d0] n:   0 pts:      0 pts_time:0       duration:   1001 duration_time:0.0333667 fmt:yuv420p type:I checksum:0\n\
\x1b[0;36m[Parsed_showinfo_0 @ 0x55d0] \x1b[0mn:   1 pts:   1001 pts_time:0.0333667 duration:   1001 type:P\n\
Input #0, mov,mp4, from '/media/] n:1 pts:0.mp4':\n\
[Parsed_showinfo_0 @ 0x55d0] n:   2 pts:   2002 pts_time:0.0667333 type:B\n\
[Parsed_showinfo_0 @ 0x55d0] n:   3 pts:NOPTS\n";

#[test]
fn hostile_showinfo_logs_never_panic() {
    assert_no_panic("showinfo", &[SHOWINFO.as_bytes()], ROUNDS, |bytes| {
        for mut parser in [ShowinfoParser::new(), ShowinfoParser::allowing_repeats()] {
            for line in bytes.split(|&b| b == b'\n') {
                let _ = parser.line_lossy(line);
            }
            let _ = (parser.time_base(), parser.frames_seen());
        }
    });
}
