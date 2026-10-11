//! `kerf_gpu::text` against FFmpeg's own `drawtext`, on pictures rather than through the compositor:
//! the rasterisation, the colour, the blend arithmetic and the box, plane by plane. Needs `ffmpeg`
//! (`KERF_FFMPEG`), no adapter. `#[ignore]`d like the parity suite; run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test text -- --ignored
//! ```
//!
//! Each case draws an overlay over a busy `yuv420p` picture twice — once by `drawtext` (the filter
//! the still runs, [`kerf_core::text_still_filter`]) and once by rasterising it with
//! [`kerf_core::text_raster_graph`] and blending the masks with the scalar reference — and compares
//! every sample of every plane. Run it on both FFmpegs: 6.1 draws with BT.601 whatever the picture
//! is tagged (its `drawtext` ignores the tag), 9.0 with the tag.

#![allow(clippy::print_stderr)]

use std::process::{Command, Stdio};

use kerf_core::{PlanText, YuvMatrix};
use kerf_gpu::text::{self, Planes};

const W: usize = 640;
const H: usize = 360;

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// What the picture is tagged as, and so what FFmpeg 9's `drawtext` converts colours with.
#[derive(Clone, Copy)]
enum Tag {
    None,
    Bt709,
    Bt2020,
}

impl Tag {
    fn filter(self) -> &'static str {
        match self {
            Tag::None => "",
            Tag::Bt709 => ",setparams=colorspace=bt709:range=tv",
            Tag::Bt2020 => ",setparams=colorspace=bt2020nc:range=tv",
        }
    }

    /// The matrix `drawtext` converts the text colour with on this FFmpeg: 6.1 never reads the tag.
    fn matrix(self) -> YuvMatrix {
        if kerf_core::composite_color_policy() == kerf_core::CompositeColorPolicy::FixedBt601 {
            return YuvMatrix::Bt601;
        }
        match self {
            Tag::None => YuvMatrix::Bt601,
            Tag::Bt709 => YuvMatrix::Bt709,
            Tag::Bt2020 => YuvMatrix::Bt2020,
        }
    }
}

struct Picture {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

fn planes(raw: &[u8]) -> Picture {
    let (luma, chroma) = (W * H, W * H / 4);
    assert_eq!(raw.len(), luma + 2 * chroma);
    Picture {
        y: raw[..luma].to_vec(),
        u: raw[luma..luma + chroma].to_vec(),
        v: raw[luma + chroma..].to_vec(),
    }
}

/// The canvas, optionally with a filter after it (`drawtext` for the reference).
fn canvas(tag: Tag, then: &str) -> Picture {
    let graph = format!("testsrc2=s={W}x{H}:d=1:r=1,format=yuv420p{}{then}[o]", tag.filter());
    planes(&ffmpeg(&[
        "-filter_complex",
        &graph,
        "-map",
        "[o]",
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-",
    ]))
}

fn overlay(text: &str) -> PlanText {
    PlanText {
        id: uuid::Uuid::nil(),
        text: text.into(),
        size: 0.1,
        pos: (0.5, 0.8),
        alpha: 1.0,
        color: "white".into(),
        bg: None,
        font_file: kerf_core::fonts::default_font_file(false),
        synthetic_bold: false,
    }
}

struct Outcome {
    changed: usize,
    differing: usize,
    worst: i32,
}

/// Draw `t` over the canvas by `drawtext` and by the reference; compare every plane.
fn compare(tag: Tag, t: &PlanText) -> Outcome {
    let real = canvas(tag, &format!(",{}", kerf_core::text_still_filter(t, H as u32)));
    let mut mine = canvas(tag, "");
    let base = canvas(tag, "");
    let graph = kerf_core::text_raster_graph(t, (W as u32, H as u32));
    let raster = text::rasterize(&graph).expect("rasterise");
    let blends = text::blends(t, &raster, tag.matrix(), false).expect("blends");
    let mut pic = Planes {
        w: W,
        h: H,
        subsampled: true,
        planes: [&mut mine.y, &mut mine.u, &mut mine.v],
    };
    text::blend_all(&mut pic, &raster, &blends);
    let mut out = Outcome {
        changed: 0,
        differing: 0,
        worst: 0,
    };
    for (m, r, b) in [
        (&mine.y, &real.y, &base.y),
        (&mine.u, &real.u, &base.u),
        (&mine.v, &real.v, &base.v),
    ] {
        out.changed += r.iter().zip(b).filter(|(a, b)| a != b).count();
        for (a, b) in m.iter().zip(r) {
            let d = (i32::from(*a) - i32::from(*b)).abs();
            out.differing += usize::from(d != 0);
            out.worst = out.worst.max(d);
        }
    }
    out
}

fn check(name: &str, tag: Tag, t: &PlanText, allow: Option<(usize, i32)>) {
    let o = compare(tag, t);
    eprintln!(
        "{name:<34} {} samples changed by the text, {} differ from drawtext's, worst {}",
        o.changed, o.differing, o.worst
    );
    let on_the_picture = t.pos.0 <= 1.5;
    assert!(
        o.changed > 0 || !on_the_picture || t.text.trim().is_empty() || t.alpha <= 0.0,
        "{name}: the text drew nothing"
    );
    match allow {
        None => assert!(
            o.differing == 0,
            "{name}: {} samples differ from drawtext's, by up to {}",
            o.differing,
            o.worst
        ),
        Some((count, worst)) => assert!(
            o.differing <= count && o.worst <= worst,
            "{name}: {} samples differ (allowed {count}), by up to {} (allowed {worst})",
            o.differing,
            o.worst
        ),
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn colours_parse_as_ffmpeg_reads_them() {
    // Every name FFmpeg knows, through `drawbox` onto an RGB canvas (opaque, so the box is the
    // colour itself), and the hex and alpha spellings through the blend.
    let names: Vec<&str> = [
        "black",
        "white",
        "red",
        "lime",
        "blue",
        "yellow",
        "cyan",
        "Magenta",
        "GoldenRod",
        "DARKGRAY",
    ]
    .to_vec();
    for name in names {
        let want = text::parse_color(name).unwrap_or_else(|| panic!("{name}"));
        let raw = ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=0x102030:s=4x4:d=1:r=1,format=rgb24,drawbox=x=0:y=0:w=4:h=4:color={name}:t=fill"),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ]);
        assert_eq!(&raw[..3], &want[..3], "{name}");
    }
    for (spelling, rgba) in [
        ("#ffcc00", Some([255, 204, 0, 255])),
        ("0x11223344", Some([0x11, 0x22, 0x33, 0x44])),
        ("white@0.5", Some([255, 255, 255, 127])),
        ("black@.9", Some([0, 0, 0, 229])),
        ("red@1", Some([255, 0, 0, 255])),
        ("yellow@0.333", Some([255, 255, 0, 84])),
        ("#fff", None),
        ("0xZZZZZZ", None),
        ("nosuchcolour", None),
        ("white@2", None),
        ("white@x", None),
    ] {
        assert_eq!(text::parse_color(spelling), rgba, "{spelling}");
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_reference_blend_is_drawtexts_own_for_opaque_text() {
    let plain = overlay("Hello World, Ay Te");
    check("plain", Tag::None, &plain, None);
    for (name, tag) in [("bt709", Tag::Bt709), ("bt2020", Tag::Bt2020)] {
        check(&format!("plain, {name} picture"), tag, &plain, None);
    }
    let mut yellow = overlay("Colour #ffcc00");
    yellow.color = "#ffcc00".into();
    check("hex colour", Tag::None, &yellow, None);
    check("hex colour, bt709 picture", Tag::Bt709, &yellow, None);
    let mut red = overlay("RED");
    red.color = "red".into();
    check("a named colour", Tag::None, &red, None);

    let mut boxed = overlay("Boxed caption Ay");
    boxed.size = 0.06;
    boxed.pos = (0.5, 0.82);
    boxed.bg = Some("black@0.5".into());
    check("box black@0.5", Tag::None, &boxed, None);
    check("box black@0.5, bt709 picture", Tag::Bt709, &boxed, None);
    let mut opaque_box = overlay("Solid box");
    opaque_box.bg = Some("0x203040".into());
    opaque_box.pos = (0.37, 0.41);
    check("opaque box at odd offsets", Tag::None, &opaque_box, None);

    let mut small = overlay("x");
    small.size = 0.025;
    small.pos = (0.333, 0.777);
    check("one small glyph", Tag::None, &small, None);

    let mut big = overlay("Big");
    big.size = 0.4;
    big.pos = (0.3, 0.5);
    check("a glyph larger than the box would be", Tag::None, &big, None);

    let mut cyrillic = overlay("Привет мир, Łódź");
    cyrillic.pos = (0.5, 0.3);
    check("Cyrillic and Latin Extended", Tag::None, &cyrillic, None);
    let mut tofu = overlay("日本語 Ελλάδα");
    tofu.pos = (0.5, 0.2);
    check("glyphs the font does not have", Tag::None, &tofu, Some((8, 1)));
    let mut escapes = overlay("50% off: it's a \\ \"deal\", {x} [y]");
    escapes.size = 0.05;
    check("escapes", Tag::None, &escapes, Some((8, 1)));
}

#[test]
#[ignore = "needs ffmpeg"]
fn text_over_the_edge_of_the_picture_is_clipped_as_drawtext_clips_it() {
    for (name, pos) in [
        ("off the right edge", (0.97, 0.3)),
        ("off the left edge", (0.02, 0.5)),
        ("off the bottom", (0.5, 0.99)),
        ("off the top", (0.5, 0.01)),
        ("a corner", (1.0, 1.0)),
        ("wholly outside", (3.0, 0.5)),
    ] {
        let mut t = overlay("Edge text clipped here");
        t.pos = pos;
        check(name, Tag::None, &t, None);
        t.bg = Some("black@0.3".into());
        check(&format!("{name}, boxed"), Tag::None, &t, None);
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn opacity_scales_the_colour_alpha_as_drawtext_does() {
    for alpha in [0.0, 0.2, 0.5, 0.99] {
        let mut t = overlay("Fading title");
        t.alpha = alpha;
        t.bg = Some("black@0.6".into());
        t.color = "yellow@0.9".into();
        check(&format!("opacity {alpha}"), Tag::None, &t, None);
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn glyphs_that_overlap_are_blended_as_one_and_that_is_only_rounding() {
    // `drawtext` blends each glyph's bitmap in turn, a mask here is all of them: where two touch the
    // same pixel that is one blend instead of two. Latin text kerned tightly never differs, opaque or
    // translucent; the outline boxes of glyphs the font lacks differ by a level at a sample or two.
    for (name, color, text, size, allow) in [
        ("kerned pairs, opaque", "white", "AVATAR To Ty Te Wa", 0.12, None),
        ("kerned pairs, translucent", "white@0.6", "AVATAR To Ty Te Wa", 0.12, None),
        ("tight text, translucent", "yellow@0.5", "fi ffi Tj rn mm", 0.15, None),
        ("missing glyphs, opaque", "white", "日本語 Ελλάδα", 0.1, Some((8, 1))),
        ("missing glyphs, translucent", "white@0.5", "日本語 Ελλάδα", 0.1, Some((8, 1))),
    ] {
        let mut t = overlay(text);
        t.color = color.into();
        t.size = size;
        check(name, Tag::None, &t, allow);
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_reference_blend_is_drawtexts_own_for_small_text() {
    // From `kerf_core::MIN_TEXT_PX` up the blend is `drawtext`'s to the rounding of the pixels two
    // glyph bitmaps share (a level or two in chroma at a few samples); below it, glyphs share
    // pixels all along the line and the sum of their masks is up to 7 levels from the sequence
    // of their blends. That is the line the plan draws.
    for px in 3..=24u32 {
        let mut t = overlay("Hello, World. Ay Te Wa");
        t.size = (f64::from(px) + 0.4) / H as f64;
        t.pos = (0.4137, 0.5821);
        let o = compare(Tag::None, &t);
        eprintln!("{px:>2} px: {} samples differ, worst {}", o.differing, o.worst);
        let bound = if px >= kerf_core::MIN_TEXT_PX { 2 } else { 8 };
        assert!(
            o.worst <= bound,
            "{px} px: {} samples differ by up to {}",
            o.differing,
            o.worst
        );
    }
}
