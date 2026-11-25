//! Footage of the `checker/444b` families (a 4:4:4 picture the fit resizes, attacked from outside), shared by `parity.rs` and `motion_parity.rs`.

#![allow(dead_code)]

use std::path::Path;

use kerf_core::{Asset, Project};

/// The `setparams` that tags a picture with `tag` (a matrix name FFmpeg knows; `none` states nothing), and `range` where it is `pc`.
pub fn tag(tag: &str, range: Option<&str>) -> String {
    let range = range.map(|r| format!(":range={r}")).unwrap_or_default();
    let (matrix, primaries, trc) = match tag {
        "bt709" => ("bt709", "bt709", "bt709"),
        "smpte170m" => ("smpte170m", "smpte170m", "smpte170m"),
        "bt470bg" => ("bt470bg", "bt470bg", "bt470bg"),
        // (The BT.709 *label* of a file that states primaries and transfer but no matrix, and the matrix without them.)
        "p709" => ("unknown", "bt709", "bt709"),
        "m709" => ("bt709", "unknown", "unknown"),
        "bt2020nc" => ("bt2020nc", "bt2020", "bt2020-10"),
        "bt2020c" => ("bt2020c", "bt2020", "bt2020-10"),
        "fcc" => ("fcc", "unknown", "unknown"),
        "smpte240m" => ("smpte240m", "smpte240m", "smpte240m"),
        _ => ("unknown", "unknown", "unknown"),
    };
    format!("setparams=colorspace={matrix}:color_primaries={primaries}:color_trc={trc}{range}")
}

/// The picture a `look` draws, as a `geq` over a `yuv444p` frame of `w` x `h` (all three planes written, full-resolution chroma):
///
/// * `noise`: hard everywhere, a different polynomial in each plane (a scaler's taps show);
/// * `stripes`: a flat-ish luma and **one-sample chroma stripes** at the extremes (Cb alternates by column, Cr by row): the detail a 4:2:0
///   decode averages away and a 4:4:4 scale keeps;
/// * `cube`: blocks of three and five samples (so they straddle the 2x2 chroma blocks) that take the corners of the YCbCr cube, **out of
///   the legal range included** (luma 0 and 255, chroma 0 and 255): a scaler's overshoot clips there;
/// * `ramp`: smooth in all three planes;
/// * `edges`: single-sample bars of luma and chroma at coprime periods and moving with the frame;
/// * `card`: a natural-looking test card (`testsrc2`, converted).
fn source(look: &str, w: u32, h: u32, rate: u32, secs: u32) -> String {
    let base = format!("nullsrc=size={w}x{h}:rate={rate}:duration={secs},format=yuv444p");
    match look {
        "stripes" => format!(
            "{base},geq=lum='96+mod(X*7+Y*3+N*5,64)':cb='if(mod(X,2),16,240)':cr='if(mod(Y+N,2),16,240)'"
        ),
        "cube" => format!(
            "{base},geq=lum='if(mod(floor(X/3)+N,2),255,0)':cb='if(mod(floor(Y/3),2),255,0)':cr='if(mod(floor(X/5)+floor(Y/5),2),255,0)'"
        ),
        "ramp" => format!("{base},geq=lum='16+219*X/W':cb='16+224*Y/H':cr='16+224*mod(3*X/W+2*Y/H+N/50,1)'"),
        "edges" => format!(
            "{base},geq=lum='if(gt(mod(floor(X/1)+floor(Y/2)+N,3),0),235,16)':cb='if(gt(mod(X+floor(N/2),5),1),240,16)':cr='if(gt(mod(Y*3+X,7),2),16,240)'"
        ),
        "card" => format!("testsrc2=size={w}x{h}:rate={rate}:duration={secs},format=yuv444p"),
        _ => format!(
            "{base},geq=lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29+N*3,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3+N*5,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17+N*7,256)'"
        ),
    }
}

/// Footage of the `checker/444b` families, made once per build and kept under a name that holds every parameter.
///
/// `pix`: `yuv444p` (FFV1, lossless), `yuv444p-x264` (H.264 High 4:4:4 Predictive, lossless), `yuv444p-x264b` (the same, lossy, with B-frames and a long GOP), `yuv444p-x265` (HEVC 4:4:4, lossy), `yuv444p-pc` (`yuv444p` **flagged** full
/// range), `yuvj444p` (MJPEG), `yuva444p`, `gbrp`, `yuv444p10le`, `yuv422p`, `yuv440p`, `yuv420p` (for the layers beside them).
/// `tag`: a matrix name (`none` states nothing). `extra`: a chroma sample location (`left`, `center`, `topleft`, `top`, `bottomleft`,
/// `bottom`) or `sar43` (a sample aspect ratio of 4:3), or `""`.
pub fn footage(
    dir: &Path,
    (w, h): (u32, u32),
    look: &str,
    tag_name: &str,
    pix: &str,
    extra: &str,
    (rate, secs): (u32, u32),
) -> Asset {
    let ext = if matches!(pix, "yuv444p-x264" | "yuv444p-x264b" | "yuv444p-x265") {
        "mp4"
    } else {
        "mkv"
    };
    let path = dir.join(format!("{w}x{h}-{look}-{tag_name}-{pix}-{extra}-{rate}-{secs}.{ext}"));
    if !path.exists() {
        let pix_out = match pix {
            "yuv444p-x264" | "yuv444p-x264b" | "yuv444p-x265" | "yuv444p-pc" => "yuv444p",
            other => other,
        };
        let range = (pix == "yuv444p-pc").then_some("pc");
        let alpha = pix == "yuva444p";
        let mut graph = source(look, w, h, rate, secs);
        if alpha {
            graph = format!("{graph},format=yuva444p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(gt(X,W/3),255,70)'");
        } else if pix_out != "yuv444p" {
            graph = format!("{graph},format={pix_out}");
        }
        if !matches!(pix, "yuvj444p" | "yuva444p" | "gbrp") {
            graph = format!("{graph},{}", self::tag(tag_name, range));
        }
        if extra == "sar43" {
            graph = format!("{graph},setsar=4/3");
        }
        let codec: &[&str] = match pix {
            "yuv444p-x264" => &["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"],
            // Lossy, with B-frames and a long GOP: the frames are reordered, and the pictures are whatever the decoder makes of them.
            "yuv444p-x264b" => &["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-bf", "3", "-g", "24"],
            "yuv444p-x265" => &[
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-crf",
                "12",
                "-g",
                "12",
                "-tag:v",
                "hvc1",
            ],
            "yuvj444p" => &["-c:v", "mjpeg", "-qscale:v", "1"],
            _ => &["-c:v", "ffv1"],
        };
        let mut args: Vec<&str> = vec!["-f", "lavfi", "-i"];
        args.push(&graph);
        args.extend_from_slice(codec);
        if range.is_some() {
            args.extend(["-color_range", "pc"]);
        }
        if !extra.is_empty() && extra != "sar43" {
            args.extend(["-chroma_sample_location", extra]);
        }
        args.extend(["-pix_fmt", pix_out, path.to_str().unwrap()]);
        crate::common::ffmpeg(&args);
    }
    Project::probe_asset(&path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}

/// [`footage`] with the common parameters: 30 frames per second, one second, nothing extra.
pub fn plain(dir: &Path, size: (u32, u32), look: &str, tag_name: &str, pix: &str) -> Asset {
    footage(dir, size, look, tag_name, pix, "", (30, 1))
}

/// A phone clip as a phone stores it: [`footage`] at the stored size `(w, h)` (`yuv444p`, H.264), stream-copied with a display matrix that turns it
/// by `degrees`.
pub fn rotated(dir: &Path, stored: (u32, u32), look: &str, tag_name: &str, degrees: i32) -> Asset {
    let base = plain(dir, stored, look, tag_name, "yuv444p-x264");
    let (w, h) = stored;
    let path = dir.join(format!("{w}x{h}-{look}-{tag_name}-turned{degrees}.mp4"));
    if !path.exists() {
        crate::common::ffmpeg(&[
            "-display_rotation",
            &degrees.to_string(),
            "-i",
            &base.path,
            "-c",
            "copy",
            path.to_str().unwrap(),
        ]);
    }
    Project::probe_asset(&path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}
