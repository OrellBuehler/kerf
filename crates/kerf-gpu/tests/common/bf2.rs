//! Footage of the `checker/bf2` families (the blurred fill behind a converted layer), shared by `parity.rs` and `motion_parity.rs`.

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
        "bt2020nc" => ("bt2020nc", "bt2020", "bt2020-10"),
        "bt2020c" => ("bt2020c", "bt2020", "bt2020-10"),
        "fcc" => ("fcc", "unknown", "unknown"),
        "smpte240m" => ("smpte240m", "smpte240m", "smpte240m"),
        _ => ("unknown", "unknown", "unknown"),
    };
    format!("setparams=colorspace={matrix}:color_primaries={primaries}:color_trc={trc}{range}")
}

/// Footage of the checker/bf2 families, lossless, `secs` seconds at `rate` frames per second, made once per build and kept under a name that holds every
/// parameter. `look`: `cube` (flat blocks that take the corners of the YCbCr cube, most of them outside RGB, so the fill's blur leaves large
/// flat regions of out-of-gamut colour that the RGB hop clips), `ramp` (a diagonal through the whole of the cube), `edges`, `noise` or a test
/// card. `pix`: `yuv420p`, `yuv422p`, `yuv444p`, `yuv420p10le`, `gray`, `yuv420p-pc` (8-bit 4:2:0 flagged full range), `yuvj420p` (MJPEG),
/// `rgb24` or `yuva420p`.
pub fn footage(dir: &Path, (w, h): (u32, u32), look: &str, tag: &str, pix: &str, rate: u32, secs: u32) -> Asset {
    let ext = if matches!(pix, "yuvj420p" | "yuva420p") {
        "mkv"
    } else {
        "mp4"
    };
    let path = dir.join(format!("{w}x{h}-{look}-{tag}-{pix}-{rate}-{secs}.{ext}"));
    if !path.exists() {
        let b = ((w.min(h) / 8).max(4)) & !1;
        let source = match look {
            "cube" => format!(
                "nullsrc=size={w}x{h}:rate={rate}:duration={secs},format=yuv420p,geq=lum='16+219*mod(floor(X/{b}),2)':cb='16+224*mod(floor(Y/{b}),2)':cr='16+224*mod(floor(X/{})+floor(Y/{}),2)'",
                b + 2,
                2 * b - 2
            ),
            "ramp" => format!(
                "nullsrc=size={w}x{h}:rate={rate}:duration={secs},format=yuv420p,geq=lum='16+219*X/W':cb='16+224*Y/H':cr='16+224*mod(3*X/W+2*Y/H,1)'"
            ),
            "edges" => format!(
                "nullsrc=size={w}x{h}:rate={rate}:duration={secs},format=yuv420p,geq=lum='if(gt(mod(floor(X/3)+floor(Y/5)+floor(N/3),2),0),255,0)':cb='if(gt(mod(floor(X/7)+floor(N/5),2),0),240,16)':cr='if(gt(mod(floor(Y/9),2),0),240,16)'"
            ),
            "noise" => format!(
                "nullsrc=size={w}x{h}:rate={rate}:duration={secs},format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
            ),
            _ => format!("testsrc2=size={w}x{h}:rate={rate}:duration={secs}"),
        };
        let range = (pix == "yuv420p-pc").then_some("pc");
        let (format, codec): (String, &[&str]) = match pix {
            "yuvj420p" => ("format=yuvj420p".into(), &["-c:v", "mjpeg", "-q:v", "3"]),
            "yuva420p" => (
                "format=yuva420p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(gt(X,W/3),255,70)'".into(),
                &["-c:v", "ffv1"],
            ),
            "rgb24" => (
                "format=rgb24".into(),
                &["-c:v", "libx264rgb", "-preset", "ultrafast", "-qp", "0", "-g", "10"],
            ),
            _ => (
                format!("format={}", pix.trim_end_matches("-pc")),
                &["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"],
            ),
        };
        let tagged = if matches!(pix, "yuvj420p" | "yuva420p" | "rgb24") {
            String::new()
        } else {
            format!(",{}", self::tag(tag, range))
        };
        let pix_out = pix.trim_end_matches("-pc");
        let mut args: Vec<&str> = vec!["-f", "lavfi", "-i"];
        let graph = format!("{source},{format}{tagged}");
        args.push(&graph);
        args.extend_from_slice(codec);
        if range.is_some() {
            args.extend(["-color_range", "pc"]);
        }
        args.extend(["-pix_fmt", pix_out, path.to_str().unwrap()]);
        crate::common::ffmpeg(&args);
    }
    Project::probe_asset(&path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}

/// A phone clip as a phone stores it: [`footage`] at the stored size `(w, h)`, stream-copied with a display matrix that turns it by `degrees`, so
/// that it is shown (and FFmpeg's `autorotate` makes it) `h` wide and `w` tall for 90 and 270.
pub fn rotated(dir: &Path, stored: (u32, u32), look: &str, tag: &str, degrees: i32, rate: u32, secs: u32) -> Asset {
    let base = footage(dir, stored, look, tag, "yuv420p", rate, secs);
    let (w, h) = stored;
    let path = dir.join(format!("{w}x{h}-{look}-{tag}-turned{degrees}-{rate}-{secs}.mp4"));
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

/// [`footage`] re-encoded losslessly with its chroma sample location stated as `location` (`left`, `center`, `topleft`, `top`, `bottomleft`,
/// `bottom`): the same samples, a different position of the chroma on the luma grid for whatever reads it.
pub fn located(dir: &Path, size: (u32, u32), look: &str, tag: &str, location: &str, rate: u32, secs: u32) -> Asset {
    let base = footage(dir, size, look, tag, "yuv420p", rate, secs);
    let (w, h) = size;
    let path = dir.join(format!("{w}x{h}-{look}-{tag}-at{location}-{rate}-{secs}.mp4"));
    if !path.exists() {
        crate::common::ffmpeg(&[
            "-i",
            &base.path,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-g",
            "10",
            "-pix_fmt",
            "yuv420p",
            "-chroma_sample_location",
            location,
            path.to_str().unwrap(),
        ]);
    }
    Project::probe_asset(&path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}
