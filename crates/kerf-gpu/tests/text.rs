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
    let raster = text::raster(t, (W as u32, H as u32)).expect("rasterise");
    assert!(
        raster.exact || t.text.trim().is_empty() || t.alpha <= 0.0,
        "the bitmaps of {:?} were not taken apart (the sum of them was blended)",
        t.text
    );
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

fn check(name: &str, tag: Tag, t: &PlanText) {
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
    assert!(
        o.differing == 0,
        "{name}: {} samples differ from drawtext's, by up to {}",
        o.differing,
        o.worst
    );
}

#[test]
#[ignore = "needs ffmpeg"]
fn colours_parse_as_ffmpeg_reads_them() {
    // Every name FFmpeg knows, through the `color` source (which parses it with `av_parse_color`: 4.4.2's
    // `drawbox` writes 254 for `red` on an `rgb24` canvas), and the hex and alpha spellings through the blend.
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
            &format!("color=c={name}:s=4x4:d=1:r=1,format=rgb24"),
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
    check("plain", Tag::None, &plain);
    for (name, tag) in [("bt709", Tag::Bt709), ("bt2020", Tag::Bt2020)] {
        check(&format!("plain, {name} picture"), tag, &plain);
    }
    let mut yellow = overlay("Colour #ffcc00");
    yellow.color = "#ffcc00".into();
    check("hex colour", Tag::None, &yellow);
    check("hex colour, bt709 picture", Tag::Bt709, &yellow);
    let mut red = overlay("RED");
    red.color = "red".into();
    check("a named colour", Tag::None, &red);

    let mut boxed = overlay("Boxed caption Ay");
    boxed.size = 0.06;
    boxed.pos = (0.5, 0.82);
    boxed.bg = Some("black@0.5".into());
    check("box black@0.5", Tag::None, &boxed);
    check("box black@0.5, bt709 picture", Tag::Bt709, &boxed);
    let mut opaque_box = overlay("Solid box");
    opaque_box.bg = Some("0x203040".into());
    opaque_box.pos = (0.37, 0.41);
    check("opaque box at odd offsets", Tag::None, &opaque_box);

    let mut small = overlay("x");
    small.size = 0.025;
    small.pos = (0.333, 0.777);
    check("one small glyph", Tag::None, &small);

    let mut big = overlay("Big");
    big.size = 0.4;
    big.pos = (0.3, 0.5);
    check("a glyph larger than the box would be", Tag::None, &big);

    let mut cyrillic = overlay("Привет мир, Łódź");
    cyrillic.pos = (0.5, 0.3);
    check("Cyrillic and Latin Extended", Tag::None, &cyrillic);
    let mut tofu = overlay("日本語 Ελλάδα");
    tofu.pos = (0.5, 0.2);
    check("glyphs the font does not have", Tag::None, &tofu);
    let mut escapes = overlay("50% off: it's a \\ \"deal\", {x} [y]");
    escapes.size = 0.05;
    check("escapes", Tag::None, &escapes);
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
        check(name, Tag::None, &t);
        t.bg = Some("black@0.3".into());
        check(&format!("{name}, boxed"), Tag::None, &t);
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
        check(&format!("opacity {alpha}"), Tag::None, &t);
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn glyphs_that_overlap_are_blended_in_turn_to_the_sample() {
    // `drawtext` blends each glyph's bitmap in turn, and so does the compositor: where two touch the same
    // pixel that is two blends here as there. Kerned tight text, opaque or translucent, and the outline boxes
    // of glyphs the font lacks (which the sum of the bitmaps got a level off at a sample or two) are
    // `drawtext`'s to the sample.
    for (name, color, text, size) in [
        ("kerned pairs, opaque", "white", "AVATAR To Ty Te Wa", 0.12),
        ("kerned pairs, translucent", "white@0.6", "AVATAR To Ty Te Wa", 0.12),
        ("tight text, translucent", "yellow@0.5", "fi ffi Tj rn mm", 0.15),
        ("missing glyphs, opaque", "white", "日本語 Ελλάδα", 0.1),
        ("missing glyphs, translucent", "white@0.5", "日本語 Ελλάδα", 0.1),
    ] {
        let mut t = overlay(text);
        t.color = color.into();
        t.size = size;
        check(name, Tag::None, &t);
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_reference_blend_is_drawtexts_own_for_small_text() {
    // From 3 px up the blend is `drawtext`'s to the sample: below 12 px glyphs share pixels all along the
    // line (the sum of their masks was up to 10 levels of chroma from the sequence of their blends), and
    // from 12 px up a level or two at a few samples.
    for px in 3..=24u32 {
        let mut t = overlay("Hello, World. Ay Te Wa");
        t.size = (f64::from(px) + 0.4) / H as f64;
        t.pos = (0.4137, 0.5821);
        let o = compare(Tag::None, &t);
        eprintln!("{px:>2} px: {} samples differ, worst {}", o.differing, o.worst);
        assert!(
            o.differing == 0,
            "{px} px: {} samples differ by up to {}",
            o.differing,
            o.worst
        );
    }
}

/// A font installed on this machine, if any of `names` is (the shipped Geist is always there, so the
/// hinting of a second and third face is the check on the dictionary, not the font).
fn installed(names: &[&str]) -> Option<std::path::PathBuf> {
    let dirs = [
        "/usr/share/fonts/truetype/dejavu",
        "/usr/share/fonts/truetype/ubuntu",
        "/usr/share/fonts/truetype/liberation",
        "/usr/share/fonts/TTF",
        "/Library/Fonts",
        "/System/Library/Fonts/Supplemental",
        "C:\\Windows\\Fonts",
    ];
    names
        .iter()
        .find_map(|n| dirs.iter().map(|d| std::path::Path::new(d).join(n)).find(|p| p.exists()))
}

fn bold_overlay(text: &str, px: u32) -> PlanText {
    let mut t = overlay(text);
    t.synthetic_bold = true;
    t.size = f64::from(px) / H as f64;
    t
}

#[test]
#[ignore = "needs ffmpeg"]
fn a_synthetic_bold_border_is_blended_bitmap_by_bitmap_as_drawtext_does() {
    // `drawtext` thickens a bold request in a face with no bold face by blending each glyph's border
    // bitmap in turn and then each glyph's fill: bitmaps overlap along every letter gap, and a sum of
    // them is up to 33 levels of chroma off. Every sample of every plane must be `drawtext`'s own.
    for px in [10u32, 14, 20, 30, 45, 67, 100] {
        for (name, text) in [
            ("sentence", "Hello, World. Ay Te Wa"),
            ("tight pairs", "AVATAR rn mm ll il"),
            ("ligatures", "office fi fl ffi -> =>"),
        ] {
            let mut t = bold_overlay(text, px);
            t.pos = (0.5137, 0.4821);
            check(&format!("bold {name} {px} px"), Tag::None, &t);
        }
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn a_border_keeps_its_alpha_colour_box_and_edges() {
    let mut yellow = bold_overlay("Summer sale 50% off", 36);
    yellow.color = "yellow@0.7".into();
    yellow.alpha = 0.6;
    check("translucent yellow border", Tag::None, &yellow);
    check("translucent yellow border, bt709", Tag::Bt709, &yellow);
    let mut boxed = bold_overlay("Boxed bold caption", 28);
    boxed.bg = Some("black@0.5".into());
    boxed.pos = (0.5, 0.85);
    check("bold in a box", Tag::None, &boxed);
    for (name, pos) in [
        ("off the right edge", (0.97, 0.3)),
        ("off the left edge", (0.02, 0.5)),
        ("off the bottom", (0.5, 0.99)),
        ("off the top", (0.5, 0.01)),
        ("a corner", (1.0, 1.0)),
    ] {
        let mut t = bold_overlay("Bold text clipped here", 30);
        t.pos = pos;
        check(&format!("bold {name}"), Tag::None, &t);
        t.bg = Some("black@0.3".into());
        check(&format!("bold {name}, boxed"), Tag::None, &t);
    }
    // Odd positions, quarter pixels of every phase.
    for i in 0..8 {
        let mut t = bold_overlay("Quarter pixels", 24);
        t.pos = (0.3 + f64::from(i) * 0.0137, 0.4 + f64::from(i) * 0.0231);
        check(&format!("bold at a quarter pixel phase {i}"), Tag::None, &t);
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn other_faces_are_taken_apart_too() {
    let faces = [
        installed(&["DejaVuSans.ttf"]),
        installed(&["DejaVuSerif.ttf"]),
        installed(&["Ubuntu-R.ttf", "LiberationSans-Regular.ttf"]),
        installed(&["UbuntuMono-R.ttf", "DejaVuSansMono.ttf"]),
    ];
    let mut tried = 0;
    for face in faces.into_iter().flatten() {
        for (px, bold) in [(11u32, false), (16, false), (16, true), (40, true)] {
            let mut t = if bold {
                bold_overlay("Hamburgefonstiv, jumps over fi ffi lazy dogs", px)
            } else {
                overlay("Hamburgefonstiv, jumps over fi ffi lazy dogs")
            };
            t.size = f64::from(px) / H as f64;
            t.font_file = Some(face.clone());
            t.pos = (0.5, 0.45);
            check(
                &format!("{} {px} px{}", face.display(), if bold { " bold" } else { "" }),
                Tag::None,
                &t,
            );
        }
        tried += 1;
    }
    eprintln!("{tried} installed faces");
}

#[test]
#[ignore = "needs ffmpeg"]
fn odd_strings_are_taken_apart_exactly_or_refused_never_drawn_wrong() {
    // What the sequence cannot be recovered for is an error (the frame is FFmpeg's) or, where one mask was
    // exact enough, the sum; what it is recovered for is `drawtext`'s to the sample. Leading, trailing and
    // doubled blanks, lone glyphs, long runs, scripts the shaper reorders, combining marks and emoji.
    let strings = [
        " leading blank",
        "trailing blank ",
        "a  doubled  blank",
        "   ",
        "x",
        ".",
        "iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii",
        "WWWWWWWWWWWWWWWW",
        "e\u{301} combining a\u{308}",
        "\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}",
        "mixed \u{05e9}\u{05dc}\u{05d5}\u{05dd} text",
        "tab\there",
        "naïve café — “quotes” … ✓",
        "emoji \u{1f3ac} here",
        "ﬁ ligature chars ﬃ",
        "a%b\\c'd:e",
    ];
    let mut exact = 0;
    for (i, text) in strings.iter().enumerate() {
        for (px, bold) in [(9u32, false), (24, true)] {
            let mut t = if bold { bold_overlay(text, px) } else { overlay(text) };
            t.size = f64::from(px) / H as f64;
            t.pos = (0.5 + f64::from(i as u32) * 0.001, 0.45);
            let Ok(raster) = text::raster(&t, (W as u32, H as u32)) else {
                eprintln!("{text:?} at {px} px{}: refused", if bold { " bold" } else { "" });
                continue;
            };
            if !raster.exact {
                eprintln!("{text:?} at {px} px: the sum (one mask was enough)");
                continue;
            }
            check(&format!("{text:?} {px} px"), Tag::None, &t);
            exact += 1;
        }
    }
    assert!(
        exact >= strings.len(),
        "only {exact} of {} strings were taken apart",
        strings.len() * 2
    );
}

#[test]
#[ignore = "needs ffmpeg"]
fn a_border_on_an_rgb_picture_is_blended_per_sample_in_all_three_channels() {
    // A frame with no layer is drawn in RGB: the same bitmaps, the same order, every channel as luma.
    let t = bold_overlay("RGB, no layer: fi ffi Ay", 28);
    let raster = text::raster(&t, (W as u32, H as u32)).expect("rasterise");
    assert!(raster.exact);
    let blends = text::blends(&t, &raster, YuvMatrix::Bt601, true).expect("blends");
    let mut base = vec![vec![0x30u8; W * H]; 3];
    let want: Vec<Vec<u8>> = {
        let graph = format!(
            "color=c=0x303030:s={W}x{H}:d=1:r=1,format=rgb24,{}[o]",
            kerf_core::text_still_filter(&t, H as u32)
        );
        let raw = ffmpeg(&[
            "-filter_complex",
            &graph,
            "-map",
            "[o]",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ]);
        (0..3)
            .map(|c| raw.as_chunks::<3>().0.iter().map(|p| p[c]).collect())
            .collect()
    };
    let [r, g, b] = &mut base[..] else { unreachable!() };
    let mut pic = Planes {
        w: W,
        h: H,
        subsampled: false,
        planes: [r, g, b],
    };
    text::blend_all(&mut pic, &raster, &blends);
    for (c, (got, want)) in base.iter().zip(&want).enumerate() {
        let wrong = got.iter().zip(want).filter(|(a, b)| a != b).count();
        assert_eq!(wrong, 0, "channel {c}: {wrong} samples differ from drawtext's");
    }
}
