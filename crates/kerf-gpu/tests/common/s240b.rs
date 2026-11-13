//! Footage of the `checker/s240b` families (SMPTE 240M, attacked from outside), shared by `parity.rs` and `motion_parity.rs`.

#![allow(dead_code)]

use std::path::Path;

use kerf_core::{Asset, Project};

/// What a tag name stands for: the `setparams` colour space, primaries and transfer, and the range where the name states one.
///
/// * `none`: nothing stated;
/// * `240`: the ledger's archive footage, the matrix and `tv`, no primaries or transfer;
/// * `240all`: matrix, primaries and transfer all `smpte240m`;
/// * `240p`: **primaries and transfer only** (no matrix);
/// * `240m709`: `smpte240m` matrix on BT.709 primaries and transfer;
/// * `709trc240`: BT.709 matrix and primaries on the `smpte240m` transfer;
/// * `709prim240`: BT.709 matrix on `smpte240m` primaries and transfer;
/// * `240pc`: the matrix and the full range flag;
/// * `709`, `2020`, `170m`, `470bg`, `fcc`, `2020c`, `ycgco`: those matrices with their usual primaries.
fn parts(tag: &str) -> (&'static str, &'static str, &'static str, Option<&'static str>) {
    match tag {
        "240" => ("smpte240m", "unknown", "unknown", Some("tv")),
        "240all" => ("smpte240m", "smpte240m", "smpte240m", None),
        "240p" => ("unknown", "smpte240m", "smpte240m", None),
        "240m709" => ("smpte240m", "bt709", "bt709", None),
        "709trc240" => ("bt709", "bt709", "smpte240m", None),
        "709prim240" => ("bt709", "smpte240m", "smpte240m", None),
        "240pc" => ("smpte240m", "unknown", "unknown", Some("pc")),
        "240tv" => ("smpte240m", "smpte240m", "smpte240m", Some("tv")),
        "709" => ("bt709", "bt709", "bt709", None),
        "2020" => ("bt2020nc", "bt2020", "bt709", None),
        "170m" => ("smpte170m", "smpte170m", "smpte170m", None),
        "470bg" => ("bt470bg", "bt470bg", "bt470bg", None),
        "fcc" => ("fcc", "unknown", "unknown", None),
        "2020c" => ("bt2020c", "bt2020", "bt709", None),
        "ycgco" => ("ycgco", "unknown", "unknown", None),
        _ => ("unknown", "unknown", "unknown", None),
    }
}

/// The `setparams` that tags a picture with the tag `name` stands for.
pub fn setparams(name: &str) -> String {
    let (space, prim, trc, range) = parts(name);
    let range = range.map(|r| format!(":range={r}")).unwrap_or_default();
    format!("setparams=colorspace={space}:color_primaries={prim}:color_trc={trc}{range}")
}

/// The matrix name a probe must read back for the tag (`None`: stated nothing).
pub fn space_of(name: &str) -> Option<&'static str> {
    let (space, ..) = parts(name);
    (space != "unknown").then_some(space)
}

/// The picture a `look` draws on a `base` frame (`yuv420p`, `yuv422p`, `yuv444p`) of `w` x `h`; every plane written.
///
/// * `noise`: a different random value in every sample of every plane (every Y, U, V triple turns up);
/// * `cube`: blocks of 3, 5 and 7 samples taking the corners of the YCbCr cube, **out of the legal range included** (0 and 255 in every
///   plane), so the RGB a conversion makes clips in every channel;
/// * `stripes`: a flat-ish luma and **one-chroma-sample stripes** at the extremes of Cb and Cr;
/// * `corners`: eight vertical bars, the corners of the legal cube (luma 16 / 235, chroma 16 / 240);
/// * `ramp`: smooth in all three planes;
/// * `card`: a natural-looking test card;
/// * `exhaustive`: every (Y, U, V) triple of the 8-bit cube once in a 4096 x 4096 frame (a chroma block per (U, V), 64 of them per pair, the four
///   luma samples of each holding 4 of the 256 levels).
fn source(look: &str, base: &str, w: u32, h: u32, rate: u32, secs: f64) -> String {
    let nullsrc = format!("nullsrc=size={w}x{h}:rate={rate}:duration={secs},format={base}");
    match look {
        "cube" => format!(
            "{nullsrc},geq=lum='if(mod(floor(X/3)+floor(Y/3),2),255,0)':cb='if(mod(floor(X/5)+floor(Y/5),2),255,0)':cr='if(mod(floor(X/7)+floor(Y/4),2),255,0)'"
        ),
        "stripes" => format!("{nullsrc},geq=lum='96+mod(X*7+Y*3,64)':cb='if(mod(X,2),16,240)':cr='if(mod(Y,2),16,240)'"),
        "corners" => format!(
            "{nullsrc},geq=lum='if(mod(floor(X*8/W),2),235,16)':cb='if(mod(floor(X*8/W/2),2),240,16)':cr='if(mod(floor(X*8/W/4),2),240,16)'"
        ),
        "ramp" => format!("{nullsrc},geq=lum='16+219*X/W':cb='16+224*Y/H':cr='16+224*mod(3*X/W+2*Y/H,1)'"),
        "card" => format!(
            "testsrc2=size={}x{}:rate={rate}:duration={secs},crop=w={w}:h={h}:x=0:y=0:exact=1,format={base}",
            w.next_multiple_of(2),
            h.next_multiple_of(2)
        ),
        "exhaustive" => format!(
            "{nullsrc},geq=lum='mod(4*(floor(X/512)+8*floor(Y/512))+2*mod(Y,2)+mod(X,2),256)':cb='mod(X,256)':cr='mod(Y,256)'"
        ),
        _ => format!("{nullsrc},geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"),
    }
}

/// Footage of the `checker/s240b` families, made once per build and kept under a name that holds every parameter, and checked to be tagged
/// the way the case says (a container that drops a tag would make the case something else).
///
/// `pix`: `yuv420p`, `yuvj420p` (H.264 flagged full range, which the decoder names so), `yuv422p`, `yuv444p`, `yuv440p`, `yuv420p10le`, `yuv422p10le`, `gray`.
/// `codec`: `ffv1` (lossless), `x264` (lossless), `x264lossy` (CRF 28, B-frames, a long GOP), `x265`, `vp9`, `mpeg2`, `mpeg4`.
pub fn footage(dir: &Path, (w, h): (u32, u32), look: &str, tag: &str, pix: &str, codec: &str, (rate, secs): (u32, f64)) -> Asset {
    let ext = match codec {
        "x264" | "x264lossy" | "x265" => "mp4",
        _ => "mkv",
    };
    let path = dir.join(format!("{w}x{h}-{look}-{tag}-{pix}-{codec}-{rate}-{secs}.{ext}"));
    if !path.exists() {
        let base = match pix {
            "yuv422p" | "yuvj422p" | "yuv422p10le" => "yuv422p",
            "yuv444p" | "yuvj444p" => "yuv444p",
            "yuv440p" => "yuv440p",
            _ => "yuv420p",
        };
        let out_pix = if pix == "yuvj420p" { "yuv420p" } else { pix };
        let mut graph = source(look, base, w, h, rate, secs);
        if base != out_pix {
            graph = format!("{graph},format={out_pix}");
        }
        graph = format!("{graph},{}", setparams(tag));
        let codec_args: &[&str] = match codec {
            "x264" => &["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"],
            "x264lossy" => &["-c:v", "libx264", "-preset", "medium", "-crf", "28", "-bf", "3", "-g", "24"],
            "x265" => &[
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-crf",
                "14",
                "-g",
                "12",
                "-tag:v",
                "hvc1",
            ],
            "vp9" => &[
                "-c:v",
                "libvpx-vp9",
                "-crf",
                "20",
                "-b:v",
                "0",
                "-g",
                "12",
                "-deadline",
                "realtime",
                "-cpu-used",
                "8",
            ],
            "mpeg2" => &["-c:v", "mpeg2video", "-qscale:v", "2", "-g", "12"],
            "mpeg4" => &["-c:v", "mpeg4", "-qscale:v", "2", "-g", "12"],
            _ => &["-c:v", "ffv1"],
        };
        let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", &graph];
        args.extend_from_slice(codec_args);
        let range = parts(tag).3;
        if let Some(r) = range {
            args.extend(["-color_range", if r == "pc" { "pc" } else { "tv" }]);
        }
        args.extend(["-pix_fmt", out_pix, path.to_str().unwrap()]);
        crate::common::ffmpeg(&args);
    }
    let asset = Project::probe_asset(&path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()));
    let stream = &asset.streams[0];
    assert_eq!((stream.width, stream.height), (Some(w), Some(h)), "{}", path.display());
    let space = stream
        .color_space
        .as_deref()
        .filter(|s| !matches!(*s, "unknown" | "unspecified"));
    // (A decoder that writes its own default tag over the file's, MJPEG's, is not a footage of this family.)
    assert_eq!(
        space,
        space_of(tag),
        "{}: the file reads back {:?} for the tag `{tag}`",
        path.display(),
        stream.color_space
    );
    asset
}

/// [`footage`] with the common parameters: 30 frames per second, one second, lossless FFV1, 8-bit 4:2:0.
pub fn plain(dir: &Path, size: (u32, u32), look: &str, tag: &str) -> Asset {
    footage(dir, size, look, tag, "yuv420p", "ffv1", (30, 1.0))
}
