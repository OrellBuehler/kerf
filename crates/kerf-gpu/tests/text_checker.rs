//! `checker/d/…`: an independent check of the text work package (a synthetic-bold border and print under
//! 12 px drawn as the sequence of glyph bitmaps `drawtext` blends). Needs `ffmpeg` (`KERF_FFMPEG`), no adapter.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test text_checker -- --ignored --nocapture
//! ```
//!
//! Every case draws an overlay over a busy `yuv420p` picture twice, once by `drawtext` (the filter the still
//! runs) and once by `kerf_gpu::text::raster` + the scalar reference, and compares **every sample of every
//! plane**: no tolerance, no band. What `raster` hands back is classified three ways, and a draw that is not
//! `drawtext`'s is a failure whichever way it came:
//!
//! * **sequence** — the bitmaps were taken apart and proven (`Raster::exact`);
//! * **sum** — they were not, and the one-mask-of-the-sum fallback was blended (`text::sum_fallbacks`);
//! * **refused** — `GpuError`, FFmpeg draws the frame.
//!
//! The fallback is what these cases aim at: a draw that is *not* exact must be a refusal.

#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use kerf_core::{PlanText, YuvMatrix};
use kerf_gpu::text::{self, Planes};

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

#[derive(Clone, Copy, Debug)]
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
    w: usize,
    h: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

fn canvas(w: usize, h: usize, tag: Tag, then: &str) -> Picture {
    let graph = format!("testsrc2=s={w}x{h}:d=1:r=1,format=yuv420p{}{then}[o]", tag.filter());
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
        "yuv420p",
        "-",
    ]);
    let (luma, chroma) = (w * h, w * h / 4);
    assert_eq!(raw.len(), luma + 2 * chroma);
    Picture {
        w,
        h,
        y: raw[..luma].to_vec(),
        u: raw[luma..luma + chroma].to_vec(),
        v: raw[luma + chroma..].to_vec(),
    }
}

fn base_picture(w: usize, h: usize, tag: Tag) -> Picture {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    // (width, height, tag) -> the plain canvas
    type Bases = Mutex<HashMap<(usize, usize, u8), std::sync::Arc<Picture>>>;
    static BASES: OnceLock<Bases> = OnceLock::new();
    let key = (w, h, tag as u8);
    let cached = BASES.get_or_init(Default::default).lock().unwrap().get(&key).cloned();
    let pic = cached.unwrap_or_else(|| {
        let made = std::sync::Arc::new(canvas(w, h, tag, ""));
        BASES.get_or_init(Default::default).lock().unwrap().insert(key, made.clone());
        made
    });
    Picture {
        w,
        h,
        y: pic.y.clone(),
        u: pic.u.clone(),
        v: pic.v.clone(),
    }
}

#[derive(Clone)]
struct Case {
    name: String,
    text: String,
    px: f64,
    pos: (f64, f64),
    alpha: f64,
    color: String,
    bg: Option<String>,
    font: Option<PathBuf>,
    bold: bool,
    tag: Tag,
    canvas: (usize, usize),
}

impl Case {
    fn new(name: impl Into<String>, text: impl Into<String>, px: f64) -> Self {
        Self {
            name: name.into(),
            text: text.into(),
            px,
            pos: (0.4137, 0.5821),
            alpha: 1.0,
            color: "white".into(),
            bg: None,
            font: kerf_core::fonts::default_font_file(false),
            bold: false,
            tag: Tag::None,
            canvas: (640, 360),
        }
    }

    fn font(mut self, f: &Path) -> Self {
        self.font = Some(f.to_path_buf());
        self
    }

    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn at(mut self, x: f64, y: f64) -> Self {
        self.pos = (x, y);
        self
    }

    fn color(mut self, c: &str, alpha: f64) -> Self {
        self.color = c.into();
        self.alpha = alpha;
        self
    }

    fn boxed(mut self, bg: &str) -> Self {
        self.bg = Some(bg.into());
        self
    }

    fn tag(mut self, t: Tag) -> Self {
        self.tag = t;
        self
    }

    fn canvas(mut self, w: usize, h: usize) -> Self {
        self.canvas = (w, h);
        self
    }

    fn plan_text(&self) -> PlanText {
        PlanText {
            id: uuid::Uuid::nil(),
            text: self.text.clone(),
            size: self.px / self.canvas.1 as f64,
            pos: self.pos,
            alpha: self.alpha,
            color: self.color.clone(),
            bg: self.bg.clone(),
            font_file: self.font.clone(),
            synthetic_bold: self.bold,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Path_ {
    Sequence,
    Sum,
    Refused(String),
}

struct Outcome {
    path: Path_,
    changed: usize,
    differing: usize,
    worst: i32,
    first: Option<(char, usize, usize)>,
    millis: u128,
    montage: Option<PathBuf>,
}

fn parity_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/parity");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .chars()
        .take(80)
        .collect()
}

/// A gray montage of the neighbourhood of the first differing luma / chroma samples: drawtext's, ours, the
/// difference (x32).
fn montage(case: &Case, real: &Picture, mine: &Picture, differing: (usize, usize)) -> PathBuf {
    let (w, h) = (real.w, real.h);
    let (cx, cy) = differing;
    let (x0, y0) = (
        cx.saturating_sub(48).min(w - 96.min(w)),
        cy.saturating_sub(24).min(h - 48.min(h)),
    );
    let (pw, ph) = (96.min(w), 48.min(h));
    let scale = 4;
    let panel = pw * scale;
    let mut rgb = vec![0u8; panel * 3 * ph * scale * 3];
    for y in 0..ph * scale {
        for x in 0..pw * scale {
            let (sx, sy) = (x0 + x / scale, y0 + y / scale);
            let i = sy * w + sx;
            let a = real.y[i];
            let b = mine.y[i];
            let d = (i32::from(a) - i32::from(b)).unsigned_abs().min(8) as u8 * 31;
            let ci = (sy / 2) * (w / 2) + sx / 2;
            let cd = (i32::from(real.u[ci]) - i32::from(mine.u[ci]))
                .unsigned_abs()
                .max((i32::from(real.v[ci]) - i32::from(mine.v[ci])).unsigned_abs())
                .min(8) as u8
                * 31;
            for (p, px) in [[a, a, a], [b, b, b], [d.max(cd), d, cd]].into_iter().enumerate() {
                let o = (y * panel * 3 + p * panel + x) * 3;
                rgb[o..o + 3].copy_from_slice(&px);
            }
        }
    }
    let path = parity_dir().join(format!("checker-d-{}.png", slug(&case.name)));
    let size = format!("{}x{}", panel * 3, ph * scale);
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            &size,
            "-i",
            "pipe:0",
        ])
        .arg(&path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(&rgb).unwrap();
    }
    assert!(child.wait().unwrap().success());
    path
}

/// Draw the case by `drawtext` and by the compositor's raster + reference blend; compare every plane.
fn run(case: &Case) -> Outcome {
    let (w, h) = case.canvas;
    let t = case.plan_text();
    let real = canvas(w, h, case.tag, &format!(",{}", kerf_core::text_still_filter(&t, h as u32)));
    let base = base_picture(w, h, case.tag);
    let mut mine = base_picture(w, h, case.tag);
    let sums = text::sum_fallbacks();
    let started = Instant::now();
    let raster = match text::raster(&t, (w as u32, h as u32)) {
        Ok(r) => r,
        Err(e) => {
            return Outcome {
                path: Path_::Refused(e.to_string()),
                changed: 0,
                differing: 0,
                worst: 0,
                first: None,
                millis: started.elapsed().as_millis(),
                montage: None,
            }
        }
    };
    let millis = started.elapsed().as_millis();
    let path = if raster.exact {
        Path_::Sequence
    } else if text::sum_fallbacks() > sums {
        Path_::Sum
    } else {
        Path_::Sequence
    };
    let blends = match text::blends(&t, &raster, case.tag.matrix(), false) {
        Ok(b) => b,
        Err(e) => {
            return Outcome {
                path: Path_::Refused(e.to_string()),
                changed: 0,
                differing: 0,
                worst: 0,
                first: None,
                millis,
                montage: None,
            }
        }
    };
    {
        let mut pic = Planes {
            w,
            h,
            subsampled: true,
            planes: [&mut mine.y, &mut mine.u, &mut mine.v],
        };
        text::blend_all(&mut pic, &raster, &blends);
    }
    let mut out = Outcome {
        path,
        changed: 0,
        differing: 0,
        worst: 0,
        first: None,
        millis,
        montage: None,
    };
    let mut first_xy = None;
    for (name, m, r, b, pw) in [
        ('Y', &mine.y, &real.y, &base.y, w),
        ('U', &mine.u, &real.u, &base.u, w / 2),
        ('V', &mine.v, &real.v, &base.v, w / 2),
    ] {
        out.changed += r.iter().zip(b).filter(|(a, b)| a != b).count();
        for (i, (a, b)) in m.iter().zip(r).enumerate() {
            let d = (i32::from(*a) - i32::from(*b)).abs();
            if d != 0 {
                out.differing += 1;
                out.worst = out.worst.max(d);
                if out.first.is_none() {
                    out.first = Some((name, i % pw, i / pw));
                    let scale = if name == 'Y' { 1 } else { 2 };
                    first_xy = Some(((i % pw) * scale, (i / pw) * scale));
                }
            }
        }
    }
    if let Some(xy) = first_xy {
        out.montage = Some(montage(case, &real, &mine, xy));
    }
    out
}

/// What the cases of one test found wrong.
#[derive(Default)]
struct Ledger {
    wrong: Vec<String>,
    sequence: usize,
    sum: usize,
    refused: usize,
    sum_exact: usize,
    slowest: (u128, String),
}

impl Ledger {
    fn run(&mut self, case: &Case) -> Outcome {
        let o = run(case);
        let label = match &o.path {
            Path_::Sequence => {
                self.sequence += 1;
                "sequence".to_string()
            }
            Path_::Sum => {
                self.sum += 1;
                if o.differing == 0 {
                    self.sum_exact += 1;
                }
                "SUM".to_string()
            }
            Path_::Refused(why) => {
                self.refused += 1;
                format!("refused ({})", why.chars().take(60).collect::<String>())
            }
        };
        if o.millis > self.slowest.0 {
            self.slowest = (o.millis, case.name.clone());
        }
        eprintln!(
            "{:<58} {:<10} {:>6} ms  changed {:>6}  differ {:>4}  worst {}{}{}",
            case.name,
            label,
            o.millis,
            o.changed,
            o.differing,
            o.worst,
            o.first.map(|(p, x, y)| format!("  first {p}({x},{y})")).unwrap_or_default(),
            o.montage.as_ref().map(|m| format!("  {}", m.display())).unwrap_or_default(),
        );
        if o.differing > 0 {
            self.wrong.push(format!(
                "{}: [{}] {} samples differ from drawtext's, worst {}, first {:?}, montage {:?}",
                case.name,
                label,
                o.differing,
                o.worst,
                o.first,
                o.montage.as_ref().map(|m| m.display().to_string())
            ));
        }
        o
    }

    fn finish(self, what: &str) {
        eprintln!(
            "{what}: {} sequence, {} sum ({} of them exact), {} refused; slowest first sight {} ms ({})",
            self.sequence, self.sum, self.sum_exact, self.refused, self.slowest.0, self.slowest.1
        );
        assert!(
            self.wrong.is_empty(),
            "{what}: {} cases are not drawtext's:\n{}",
            self.wrong.len(),
            self.wrong.join("\n")
        );
    }
}

const SENTENCE: &str = "Hello, World. Ay Te Wa fi ffi";

/// Fonts on this machine other than the shipped one: the plain faces of DejaVu / Ubuntu, and a few of the
/// host's, which a user can name (script faces whose letters join, a condensed grotesque, a symbol font).
fn faces() -> Vec<PathBuf> {
    let dirs = [
        "/usr/share/fonts/truetype/dejavu",
        "/usr/share/fonts/truetype/ubuntu",
        "/mnt/c/Windows/Fonts",
    ];
    let names = [
        "DejaVuSans.ttf",
        "DejaVuSerif.ttf",
        "DejaVuSansMono.ttf",
        "Ubuntu-R.ttf",
        "Ubuntu-C.ttf",
        "UbuntuMono-R.ttf",
        "arial.ttf",
        "ARIALN.TTF",
        "segoesc.ttf",
        "BRUSHSCI.TTF",
        "LHANDW.TTF",
        "MISTRAL.TTF",
        "Gabriola.ttf",
        "FORTE.TTF",
        "SCRIPTBL.TTF",
        "times.ttf",
        "consola.ttf",
        "ALGER.TTF",
        "CHILLER.TTF",
        "Nirmala.ttf",
        "LeelawUI.ttf",
        "seguiemj.ttf",
    ];
    names
        .iter()
        .filter_map(|n| dirs.iter().map(|d| Path::new(d).join(n)).find(|p| p.exists()))
        .collect()
}

fn face_name(p: &Path) -> String {
    p.file_stem().unwrap().to_string_lossy().into_owned()
}

// ---- the fallback: inputs the sequence cannot take apart, in a font and size the sum was thought exact for ---------

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_a_tab_in_unbordered_print_of_12_px_and_up() {
    let mut l = Ledger::default();
    for px in [12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0, 20.0, 24.0, 32.0] {
        l.run(&Case::new(format!("tab {px}px"), "Hello,\tWorld. Ay Te Wa fi ffi", px));
        l.run(&Case::new(format!("tab, trailing {px}px"), "Ay Te Wa To AVATAR\t", px));
        l.run(&Case::new(format!("tab, leading {px}px"), "\tAy Te Wa To AVATAR", px));
    }
    l.finish("tab");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_a_long_text_in_unbordered_print_of_12_px_and_up() {
    let mut l = Ledger::default();
    let long = "Ay Te Wa To AVATAR fi ffi rn mm Tj. ".repeat(6);
    assert!(long.chars().count() > 160);
    for px in [12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 19.0, 24.0] {
        l.run(
            &Case::new(format!("long {px}px, {} chars", long.chars().count()), &long, px)
                .canvas(1920, 1080)
                .at(0.5, 0.9),
        );
    }
    let nearly = "Ay Te Wa To AVATAR fi ffi rn mm Tj. ".repeat(4).trim_end().to_string();
    for n in [159usize, 160, 161] {
        let text: String = nearly.chars().cycle().take(n).collect();
        l.run(
            &Case::new(format!("{n} chars at 14px"), text, 14.0)
                .canvas(1920, 1080)
                .at(0.5, 0.9),
        );
    }
    l.finish("long");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_text_the_shaper_does_not_take_one_glyph_per_character() {
    let mut l = Ledger::default();
    let strings = [
        "e\u{301}a\u{308}o\u{302} Ay Te Wa",
        "Ay Te Wa Ae\u{301}",
        "n\u{303}a\u{300}u\u{308} fi ffi",
        "Zalgo: a\u{0300}\u{0301}\u{0302}\u{0303} Te",
        "\u{200b}Zero\u{200b}width\u{200b}",
        "Soft\u{00ad}hyphen Ay",
        "No\u{00a0}break\u{00a0}space Te",
        "Joiner a\u{200d}b\u{200c}c Te",
        "Emoji \u{1f3ac}\u{fe0f} \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} Ay",
        "Flags \u{1f1e8}\u{1f1ed} \u{1f1e9}\u{1f1ea} Te",
        "\u{feff}BOM at the start Ay",
        "Marks \u{200f}rtl\u{200e} Te",
        "Digits \u{0661}\u{0662}\u{0663} 123 Wa",
        "\u{0915}\u{094d}\u{0937} \u{0928}\u{092e}\u{0938}\u{094d}\u{0924}\u{0947}",
        "\u{0e2a}\u{0e27}\u{0e31}\u{0e2a}\u{0e14}\u{0e35}",
        "\u{d55c}\u{ad6d}\u{c5b4} \u{65e5}\u{672c}\u{8a9e} Ay Te",
    ];
    for px in [12.0, 14.0, 16.0, 19.0, 24.0] {
        for (i, s) in strings.iter().enumerate() {
            l.run(&Case::new(format!("odd#{i} {px}px"), *s, px));
        }
    }
    l.finish("shaper");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_other_faces_in_unbordered_print_of_12_px_and_up() {
    let mut l = Ledger::default();
    let scripts = [
        ("latin", SENTENCE.to_string()),
        (
            "arabic",
            "\u{0645}\u{0631}\u{062d}\u{0628}\u{0627} \u{0628}\u{0627}\u{0644}\u{0639}\u{0627}\u{0644}\u{0645}".to_string(),
        ),
        (
            "hebrew",
            "\u{05e9}\u{05dc}\u{05d5}\u{05dd} \u{05e2}\u{05d5}\u{05dc}\u{05dd}".to_string(),
        ),
        (
            "greek",
            "\u{0393}\u{03b5}\u{03b9}\u{03ac} \u{03c3}\u{03bf}\u{03c5} \u{039a}\u{03cc}\u{03c3}\u{03bc}\u{03b5} Ay".to_string(),
        ),
        ("mixed", "Hello \u{0645}\u{0631}\u{062d}\u{0628}\u{0627} World".to_string()),
    ];
    for face in faces() {
        for px in [12.0, 14.0, 17.0, 19.0, 28.0] {
            for (script, text) in &scripts {
                l.run(&Case::new(format!("{} {script} {px}px", face_name(&face)), text, px).font(&face));
            }
        }
    }
    l.finish("faces");
}

// ---- recovered sequences must still be drawtext's -------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_small_print_and_borders_in_every_face_size_and_phase() {
    let mut l = Ledger::default();
    let mut faces = faces();
    faces.insert(0, kerf_core::fonts::default_font_file(false).unwrap());
    for face in &faces {
        for px in [3.0, 5.0, 7.0, 9.0, 11.0, 11.4, 11.5] {
            for (i, (x, y)) in [(0.4137, 0.5821), (0.5, 0.5), (0.3333, 0.7777)].into_iter().enumerate() {
                l.run(
                    &Case::new(format!("{} small {px}px #{i}", face_name(face)), SENTENCE, px)
                        .font(face)
                        .at(x, y),
                );
            }
        }
        for px in [10.0, 13.0, 21.0, 36.0] {
            l.run(
                &Case::new(format!("{} border {px}px", face_name(face)), SENTENCE, px)
                    .font(face)
                    .bold(),
            );
            l.run(
                &Case::new(format!("{} border, translucent, boxed {px}px", face_name(face)), SENTENCE, px)
                    .font(face)
                    .bold()
                    .color("yellow@0.6", 0.7)
                    .boxed("black@0.4"),
            );
        }
    }
    l.finish("small print and borders");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_borders_and_small_print_with_odd_strings() {
    let mut l = Ledger::default();
    let strings = [
        " ",
        "i",
        "W",
        "ll",
        "--",
        "...",
        "AVAVAVAVAVAVAV",
        "Tj Tj Tj Tj Tj",
        "j,j,j,j,j,j",
        "fi fl ff ffi ffl ft fj",
        "-> => <= != == === >= ::",
        "e\u{301}a\u{308}",
        "tab\there",
        "Q Q Q Q g g g g y y y y",
        "ÅÄÖåäöÉéÈè ÇçÑñ",
        "\u{65e5}\u{672c}\u{8a9e} \u{d55c}\u{ad6d}\u{c5b4}",
        "\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}",
        "%{pts} \\n \\\\ 100% 'quoted' \"double\" : ; , [ ] { }",
    ];
    for (i, s) in strings.iter().enumerate() {
        for (px, bold) in [
            (4.0, false),
            (9.0, false),
            (11.0, false),
            (12.0, false),
            (10.0, true),
            (24.0, true),
            (60.0, true),
        ] {
            let mut c = Case::new(format!("odd#{i} {px}px{}", if bold { " bold" } else { "" }), *s, px);
            if bold {
                c = c.bold();
            }
            l.run(&c);
        }
    }
    l.finish("odd strings");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_colours_opacity_pictures_and_edges_for_small_print_and_borders() {
    let mut l = Ledger::default();
    for tag in [Tag::None, Tag::Bt709, Tag::Bt2020] {
        for (color, alpha) in [
            ("white", 1.0),
            ("black", 1.0),
            ("0x203040", 0.5),
            ("yellow@0.37", 0.83),
            ("red", 0.01),
            ("#ffffff", 0.99),
            ("lime@0.5", 0.5),
        ] {
            for (px, bold) in [(7.0, false), (11.0, false), (12.0, false), (18.0, true)] {
                let mut c = Case::new(
                    format!("{tag:?} {color} a{alpha} {px}px{}", if bold { " bold" } else { "" }),
                    SENTENCE,
                    px,
                )
                .color(color, alpha)
                .tag(tag);
                if bold {
                    c = c.bold();
                }
                l.run(&c);
            }
        }
    }
    for (name, pos) in [
        ("off the right edge", (0.97, 0.3)),
        ("off the left edge", (0.02, 0.5)),
        ("off the bottom", (0.5, 0.99)),
        ("off the top", (0.5, 0.01)),
        ("a corner", (1.0, 1.0)),
        ("the other corner", (0.0, 0.0)),
        ("wholly outside", (3.0, 0.5)),
        ("negative", (-0.4, 0.5)),
    ] {
        for (px, bold, bg) in [
            (7.0, false, None),
            (14.0, true, None),
            (9.0, false, Some("black@0.3")),
            (20.0, true, Some("white@0.6")),
        ] {
            let mut c = Case::new(
                format!(
                    "{name} {px}px{}{}",
                    if bold { " bold" } else { "" },
                    if bg.is_some() { " boxed" } else { "" }
                ),
                SENTENCE,
                px,
            )
            .at(pos.0, pos.1);
            if bold {
                c = c.bold();
            }
            if let Some(b) = bg {
                c = c.boxed(b);
            }
            l.run(&c);
        }
    }
    l.finish("colours, opacity, edges");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_sizes_across_the_whole_range_and_canvases() {
    let mut l = Ledger::default();
    for (cw, ch) in [
        (640usize, 360usize),
        (1280, 720),
        (1920, 1080),
        (360, 640),
        (1080, 1920),
        (426, 240),
        (2, 2),
        (16, 16),
    ] {
        for px in [1.0, 2.0, 3.0, 6.0, 10.5, 11.49, 11.5, 12.0, 12.49, 19.9, 33.0, 71.0] {
            if (px as usize) > ch {
                continue;
            }
            l.run(&Case::new(format!("{cw}x{ch} {px}px"), "Tiny: Ay Te", px).canvas(cw, ch));
            l.run(
                &Case::new(format!("{cw}x{ch} {px}px bold"), "Tiny: Ay Te", px)
                    .canvas(cw, ch)
                    .bold(),
            );
        }
    }
    l.finish("sizes and canvases");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_a_sweep_of_positions_for_the_proof_of_unbordered_print() {
    // The proof places the bitmaps at `x64 = (int)((w * px - text_w / 2) * 64)`; a position whose quarter-pixel
    // phase the model gets wrong fails the proof, and where one mask would do the sum is drawn instead of a refusal.
    let mut l = Ledger::default();
    for px in [12.0, 14.0, 17.0, 19.0, 23.0] {
        for i in 0..48 {
            let x = 0.31 + f64::from(i) * 0.001_37;
            l.run(&Case::new(format!("sweep x {px}px #{i}"), SENTENCE, px).at(x, 0.5));
        }
        for j in 0..48 {
            let y = 0.4 + f64::from(j) * 0.0031;
            l.run(&Case::new(format!("sweep y {px}px #{j}"), SENTENCE, px).at(0.5, y));
        }
    }
    l.finish("position sweep");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_large_print_in_overhanging_and_joining_faces() {
    // A bitmap that overhangs its advance by more than the dictionary's invisible-box margin, a face whose
    // letters join (their bitmaps overlap along the whole line), and a text so wide its measuring canvas is
    // past 16 384 px: each ends the sequence, and the draw is then the sum, or a refusal.
    let mut l = Ledger::default();
    for face in faces() {
        for px in [40.0, 64.0, 96.0, 140.0] {
            l.run(
                &Case::new(format!("{} large {px}px", face_name(&face)), "Ay fj ff Tj Qg", px)
                    .font(&face)
                    .canvas(1920, 1080)
                    .at(0.5, 0.5),
            );
        }
    }
    let wide = "Ay fj ff Tj Qg rn mm wwww ".repeat(4);
    for face in faces() {
        for px in [96.0, 130.0] {
            l.run(
                &Case::new(
                    format!("{} wide {px}px, {} chars", face_name(&face), wide.chars().count()),
                    &wide,
                    px,
                )
                .font(&face)
                .canvas(1920, 1080)
                .at(0.5, 0.5),
            );
        }
    }
    l.finish("large print");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_first_sight_of_a_caption_with_many_distinct_glyphs() {
    // Latency, not exactness: report, do not fail on it. A caption of 150 distinct glyphs is 150 dictionary
    // units at four phases, and up to five baseline candidates on a shaped build.
    let mut l = Ledger::default();
    let latin: String = ('\u{00c0}'..='\u{017f}').collect();
    let cyr: String = ('\u{0410}'..='\u{044f}').collect();
    let greek: String = ('\u{0391}'..='\u{03c9}').filter(|c| c.is_alphabetic()).collect();
    let dejavu = Path::new("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf");
    let mixed = format!("{latin}{cyr}{greek}");
    let distinct = mixed.chars().collect::<std::collections::HashSet<_>>().len();
    for (name, text, px) in [
        ("latin ext, 150+ distinct", mixed.chars().take(150).collect::<String>(), 16.0),
        ("cyrillic+greek, 120 distinct", format!("{cyr}{greek}"), 14.0),
        (
            "cjk, 120 distinct (tofu in Geist)",
            ('\u{4e00}'..).take(120).collect::<String>(),
            14.0,
        ),
    ] {
        let _ = distinct;
        l.run(&Case::new(format!("geist {name}"), &text, px).canvas(1920, 1080).at(0.5, 0.9));
        if dejavu.exists() {
            l.run(
                &Case::new(format!("dejavu {name}"), &text, px)
                    .canvas(1920, 1080)
                    .at(0.5, 0.9)
                    .font(dejavu),
            );
            l.run(
                &Case::new(format!("dejavu bold-border {name}"), &text, px)
                    .canvas(1920, 1080)
                    .at(0.5, 0.9)
                    .font(dejavu)
                    .bold(),
            );
        }
    }
    eprintln!("slowest first sight: {} ms ({})", l.slowest.0, l.slowest.1);
    l.finish("first sight");
}

// ---- refusal necessity: draw what is refused anyway, and see whether it was exact ------------------------------

/// The overlay drawn as one mask of the sum of its bitmaps (what a build whose layout is unknown, or a text the
/// sequence cannot be recovered for, would blend), compared with `drawtext` sample for sample:
/// `(differing, worst)`.
fn drawn_anyway(case: &Case) -> (usize, i32) {
    let (w, h) = case.canvas;
    let t = case.plan_text();
    let real = canvas(w, h, case.tag, &format!(",{}", kerf_core::text_still_filter(&t, h as u32)));
    let mut mine = base_picture(w, h, case.tag);
    let raster = text::rasterize(&kerf_core::text_raster_graph(&t, (w as u32, h as u32))).expect("rasterise");
    assert!(!raster.exact, "the sum is not the sequence");
    let blends = text::blends(&t, &raster, case.tag.matrix(), false).expect("blends");
    let mut pic = Planes {
        w,
        h,
        subsampled: true,
        planes: [&mut mine.y, &mut mine.u, &mut mine.v],
    };
    text::blend_all(&mut pic, &raster, &blends);
    let (mut differing, mut worst) = (0, 0);
    for (m, r) in [(&mine.y, &real.y), (&mine.u, &real.u), (&mine.v, &real.v)] {
        for (a, b) in m.iter().zip(r) {
            let d = (i32::from(*a) - i32::from(*b)).abs();
            differing += usize::from(d != 0);
            worst = worst.max(d);
        }
    }
    (differing, worst)
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_what_the_plan_and_the_render_refuse_would_not_have_been_exact() {
    // Refusals the change keeps or adds: the plan's `TextBorder` / `TextTooSmall` on a build whose layout is
    // unknown (the sum is all it could draw), and the render's, for a bordered or small text whose bitmaps the
    // sequence could not be recovered for. Drawn anyway as the sum, each must differ from `drawtext`; one that
    // does not is a refusal wider than it has to be (reported, not failed).
    let mut cases: Vec<Case> = Vec::new();
    for px in [3.0, 5.0, 7.0, 9.0, 11.0] {
        cases.push(Case::new(format!("plan: small {px}px"), SENTENCE, px));
        cases.push(Case::new(format!("plan: small {px}px, translucent"), SENTENCE, px).color("white@0.6", 0.9));
    }
    for px in [10.0, 14.0, 20.0, 36.0, 64.0] {
        cases.push(Case::new(format!("plan: border {px}px"), SENTENCE, px).bold());
    }
    for px in [9.0, 24.0] {
        for bold in [false, true] {
            for (name, text) in [
                ("tab", "tab\there Ay Te"),
                ("combining", "e\u{301}a\u{308} Ay Te Wa"),
                ("long", "Ay Te Wa To AVATAR fi ffi rn mm Tj. "),
            ] {
                let text = if name == "long" { text.repeat(6) } else { text.to_string() };
                let mut c = Case::new(format!("render: {name} {px}px{}", if bold { " bold" } else { "" }), text, px);
                if name == "long" {
                    c = c.canvas(1920, 1080).at(0.5, 0.9);
                }
                if bold {
                    c = c.bold();
                }
                if !(px >= 12.0 && !bold) {
                    cases.push(c);
                }
            }
        }
    }
    let mut unnecessary = Vec::new();
    for c in &cases {
        let (differing, worst) = drawn_anyway(c);
        let refused = text::raster(&c.plan_text(), (c.canvas.0 as u32, c.canvas.1 as u32)).is_err();
        eprintln!(
            "{:<44} {} drawn as the sum: {differing} samples differ, worst {worst}",
            c.name,
            if c.name.starts_with("plan") {
                "(plan)  "
            } else if refused {
                "(render)"
            } else {
                "(drawn!)"
            }
        );
        if differing == 0 {
            unnecessary.push(c.name.clone());
        }
    }
    eprintln!("refusals that would have been exact as the sum: {unnecessary:?}");
}

// ---- colours: the conversion of the text colour to the picture's matrix --------------------------------------

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_colours_the_editor_can_pick_are_converted_as_drawtext_converts_them() {
    // Not part of the sequence work, but part of what the text pass must draw exactly: `ff_draw_color` is integer
    // arithmetic on a build before 7 and float on 9 (and the compositor models one conversion). A solid colour
    // at full opacity shows an off-by-one in every sample the glyphs cover.
    let mut l = Ledger::default();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut colours: Vec<String> = [
        "lime", "red", "blue", "yellow", "cyan", "magenta", "gray", "orange", "pink", "purple", "teal", "navy", "olive",
        "silver", "maroon",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for _ in 0..60 {
        let v = next();
        colours.push(format!("#{:02x}{:02x}{:02x}", v & 0xff, (v >> 8) & 0xff, (v >> 16) & 0xff));
    }
    for (i, c) in colours.iter().enumerate() {
        l.run(&Case::new(format!("colour {c} #{i}"), "HHHH", 40.0).color(c, 1.0));
    }
    for tag in [Tag::Bt709, Tag::Bt2020] {
        for c in ["lime", "#ff8000", "#12ab34", "#fedcba"] {
            l.run(&Case::new(format!("colour {c} {tag:?}"), "HHHH", 40.0).color(c, 1.0).tag(tag));
        }
    }
    l.finish("colours");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_which_layout_this_build_has() {
    eprintln!("ffmpeg {}: layout {:?}", kerf_core::ffmpeg_path(), text::model());
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_the_shipped_font_one_latin_extended_character_at_a_time() {
    // Which single characters of Latin-1 Supplement / Latin Extended-A make the sequence unrecoverable in the font
    // every title and caption defaults to, so that the draw is the sum (or a refusal)?
    let mut l = Ledger::default();
    for c in ('\u{00c0}'..='\u{017f}').filter(|c| c.is_alphabetic()) {
        l.run(&Case::new(
            format!("A{c}Ay U+{:04X} 16px", c as u32),
            format!("Ay {c}Te {c}"),
            16.0,
        ));
    }
    l.finish("latin extended");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_the_shipped_font_is_taken_apart_on_this_build() {
    // The work package claims small print and a synthetic border are drawn, and ordinary text exactly: in the
    // shipped font, on a build the compositor says it knows, each must come back as the recovered sequence.
    let mut l = Ledger::default();
    let mut not_taken_apart = Vec::new();
    for (name, px, bold) in [
        ("print", 5.0, false),
        ("print", 9.0, false),
        ("caption", 12.0, false),
        ("caption", 16.0, false),
        ("caption", 19.0, false),
        ("title", 36.0, false),
        ("bold border", 10.0, true),
        ("bold border", 28.0, true),
    ] {
        let mut c = Case::new(format!("{name} {px}px"), SENTENCE, px);
        if bold {
            c = c.bold();
        }
        if l.run(&c).path != Path_::Sequence {
            not_taken_apart.push(c.name.clone());
        }
    }
    eprintln!("layout {:?}; not taken apart: {not_taken_apart:?}", text::model());
    assert!(
        not_taken_apart.is_empty(),
        "{}: {not_taken_apart:?} were refused or drawn as the sum",
        kerf_core::ffmpeg_path()
    );
    l.finish("shipped font");
}

// ---- round two: the sequence is proven or refused. Hunt for a draw that is not exact. ---------------------------

impl Ledger {
    fn merge(&mut self, other: Ledger) {
        self.wrong.extend(other.wrong);
        self.sequence += other.sequence;
        self.sum += other.sum;
        self.refused += other.refused;
        self.sum_exact += other.sum_exact;
        if other.slowest.0 > self.slowest.0 {
            self.slowest = other.slowest;
        }
    }
}

/// Every font file under the usual directories that `drawtext` opens, one path per file.
fn every_font_file() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in ["/usr/share/fonts/truetype", "/mnt/c/Windows/Fonts"] {
        let mut stack = vec![PathBuf::from(dir)];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x.eq_ignore_ascii_case("ttf") || x.eq_ignore_ascii_case("otf"))
                    && !p.to_string_lossy().contains(['\'', ':', ' ', '\\'])
                {
                    found.push(p);
                }
            }
        }
    }
    found.sort();
    found
}

/// Does `drawtext` draw with this file at all (it refuses bitmap-only and some collection files)?
fn drawtext_opens(face: &Path, px: f64) -> bool {
    let graph = format!(
        "color=c=black:s=64x64:d=1:r=1,drawtext=text=ab:fontsize={px}:fontcolor=white:fontfile='{}'",
        face.display()
    );
    Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i", &graph])
        .args(["-frames:v", "1", "-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Does `drawtext` itself draw this case (it refuses some faces at some sizes)?
fn reference_draws(c: &Case) -> bool {
    let (w, h) = c.canvas;
    let graph = format!(
        "testsrc2=s={w}x{h}:d=1:r=1,format=yuv420p,{}[o]",
        kerf_core::text_still_filter(&c.plan_text(), h as u32)
    );
    Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-filter_complex", &graph])
        .args(["-map", "[o]", "-frames:v", "1", "-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_every_installed_font_file_taken_apart_exactly_or_refused() {
    // Whatever a user can name: kerning in GPOS, mark positioning, joining forms, ligatures, swashes, colour
    // glyphs. A text is `drawtext`'s to the sample or it is refused; a refusal is not a failure.
    let strings: [(&str, String, f64, bool); 5] = [
        ("latin", "Hamburgefonstiv, Ay Te Wa fi ffi fl -> 1/2".into(), 15.0, false),
        ("rtl", "Hello \u{0645}\u{0631}\u{062d}\u{0628}\u{0627} \u{05e9}\u{05dc}\u{05d5}\u{05dd} World".into(), 17.0, false),
        ("marks", "e\u{301}a\u{308}o\u{302} \u{05e9}\u{05b8}\u{05c1}\u{05dc}\u{05d5}\u{05b9}\u{05dd} Ay".into(), 20.0, false),
        ("indic", "\u{0915}\u{094d}\u{0937} \u{0928}\u{092e}\u{0938}\u{094d}\u{0924}\u{0947} \u{0e2a}\u{0e27}\u{0e31}\u{0e2a}\u{0e14}\u{0e35}".into(), 18.0, false),
        ("border", "Hamburgefonstiv fi ffi Ay Te".into(), 22.0, true),
    ];
    let faces: Vec<PathBuf> = every_font_file().into_iter().filter(|f| drawtext_opens(f, 16.0)).collect();
    eprintln!("{} font files drawtext opens", faces.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let total = std::sync::Mutex::new(Ledger::default());
    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| {
                let mut l = Ledger::default();
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(face) = faces.get(i) else { break };
                    for (kind, text, px, bold) in &strings {
                        let mut c = Case::new(format!("{} {kind} {px}px", face_name(face)), text, *px).font(face);
                        if *bold {
                            c = c.bold();
                        }
                        // FFmpeg's own errors (a strike of bitmaps at this size, a symbol face with no space glyph):
                        // nothing to compare with.
                        if !reference_draws(&c) {
                            continue;
                        }
                        l.run(&c);
                    }
                }
                total.lock().unwrap().merge(l);
            });
        }
    });
    total.into_inner().unwrap().finish("every font file");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_the_colour_model_the_build_has_is_drawtexts_for_thousands_of_colours() {
    // The compositor probes whether `ff_draw_color` is float or fixed point and converts every colour by that
    // model. Boxes of 3000 colours (corners, grays, a pseudo-random walk) are drawn by `drawtext` on a black
    // canvas in each tag; the planes must be the model's conversion of the colour, exactly.
    let Some(build) = text::build() else {
        panic!(
            "{}: the compositor knows neither this build's layout nor its colours",
            kerf_core::ffmpeg_path()
        );
    };
    eprintln!("{}: {:?}", kerf_core::ffmpeg_path(), build);
    let mut colours: Vec<[u8; 3]> = Vec::new();
    for r in [0u8, 255] {
        for g in [0u8, 255] {
            for b in [0u8, 255] {
                colours.push([r, g, b]);
            }
        }
    }
    for v in (0..=255u8).step_by(5) {
        colours.push([v, v, v]);
    }
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    while colours.len() < 3000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        colours.push([seed as u8, (seed >> 8) as u8, (seed >> 16) as u8]);
    }
    const TILE: usize = 64;
    const COLS: usize = 20;
    const PER_GRAPH: usize = 300;
    let mut wrong: Vec<String> = Vec::new();
    for tag in [Tag::None, Tag::Bt709, Tag::Bt2020] {
        for chunk in colours.chunks(PER_GRAPH) {
            let rows = chunk.len().div_ceil(COLS);
            let (w, h) = (COLS * TILE, rows * TILE);
            let sample = PlanText {
                id: uuid::Uuid::nil(),
                text: "x".into(),
                size: 16.0 / h as f64,
                pos: (0.0, 0.0),
                alpha: 1.0,
                color: "white".into(),
                bg: None,
                font_file: kerf_core::fonts::default_font_file(false),
                synthetic_bold: false,
            };
            let draws: Vec<String> = chunk
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let (x, y) = ((i % COLS) * TILE + 12, (i / COLS) * TILE + 12);
                    kerf_core::text_piece_filter(
                        &sample,
                        h as u32,
                        &kerf_core::TextPiece {
                            text: "x",
                            lines: false,
                            font: "white@0",
                            border: None,
                            boxed: Some((&format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]), 8)),
                        },
                        &[&format!("x={x}"), &format!("y={y}")],
                    )
                })
                .collect();
            let graph = format!(
                "color=c=black:s={w}x{h}:r=1:d=1,format=yuv420p{},{}[o]",
                tag.filter(),
                draws.join(",")
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
                "yuv420p",
                "-",
            ]);
            let (ys, cs) = (w * h, w * h / 4);
            assert_eq!(raw.len(), ys + 2 * cs);
            for (i, c) in chunk.iter().enumerate() {
                let (px, py) = ((i % COLS) * TILE + 14, (i / COLS) * TILE + 14);
                let got = [
                    raw[py * w + px],
                    raw[ys + (py / 2) * (w / 2) + px / 2],
                    raw[ys + cs + (py / 2) * (w / 2) + px / 2],
                ];
                let Some(want) = text::draw_color_with(build.colour, *c, tag.matrix()) else {
                    wrong.push(format!("{tag:?}: no conversion modelled for {c:?}"));
                    continue;
                };
                if got != want {
                    wrong.push(format!(
                        "{tag:?} {c:?}: drawtext made {got:?}, the model {:?} makes {want:?}",
                        build.colour
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} conversions differ:\n{}",
        wrong.len(),
        colours.len() * 3,
        wrong[..wrong.len().min(20)].join("\n")
    );
}

#[test]
fn checker_d_the_fixed_point_colour_conversion_is_libavutils_for_every_one_of_the_16_million_colours() {
    // FNV-1a over (Y, U, V) of every (r, g, b) in r, g, b order, from libavutil's own `RGB_TO_Y_CCIR` /
    // `RGB_TO_U_CCIR` / `RGB_TO_V_CCIR` (n4.4.2 and n6.1.1 `libavutil/colorspace.h`, identical), compiled
    // with `gcc -O2 -I <ffmpeg>` and a loop over all colours: 0x1a05042583ea00b3.
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for r in 0..=255u8 {
        for g in 0..=255u8 {
            for b in 0..=255u8 {
                for c in text::draw_color_ccir([r, g, b]) {
                    h ^= u64::from(c);
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
        }
    }
    assert_eq!(h, 0x1a05_0425_83ea_00b3, "the fixed-point conversion is not libavutil's");
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_d_oversized_text_blank_text_and_every_kind_of_space() {
    let mut l = Ledger::default();
    // Glyphs larger than the picture, at the corners and past them: the clip window and the dictionary's cell.
    for px in [150.0, 200.0, 360.0, 480.0, 720.0] {
        for text in ["Ag", "WAVE"] {
            for (x, y) in [(0.5, 0.5), (0.1, 0.9), (0.9, 0.1), (1.2, 0.5)] {
                for (bold, bg) in [(false, None), (true, Some("black@0.5"))] {
                    let mut c = Case::new(format!("{text} {px}px at ({x}, {y}) {bold} {}", bg.is_some()), text, px).at(x, y);
                    if bold {
                        c = c.bold();
                    }
                    if let Some(bg) = bg {
                        c = c.boxed(bg);
                    }
                    l.run(&c);
                }
            }
        }
    }
    // Spaces `char::is_whitespace` knows and a font may or may not have a glyph for (drawn as its .notdef box).
    let spaces = [
        '\u{00a0}', '\u{1680}', '\u{2000}', '\u{2002}', '\u{2003}', '\u{2007}', '\u{2009}', '\u{200a}', '\u{202f}', '\u{205f}',
        '\u{3000}', '\u{200b}', '\u{2060}', '\u{feff}', '\u{00ad}', '\u{180e}', '\u{0085}', '\u{000b}', '\u{000c}',
    ];
    for c in spaces {
        for px in [8.0, 18.0, 40.0] {
            l.run(&Case::new(
                format!("space U+{:04X} {px}px", c as u32),
                format!("A{c}B Ay{c}{c}Te"),
                px,
            ));
            l.run(
                &Case::new(format!("space U+{:04X} {px}px boxed bold", c as u32), format!("{c}A{c}"), px)
                    .boxed("black@0.5")
                    .bold(),
            );
        }
    }
    // Text that is only blanks, with and without the box behind it.
    for text in [" ", "   ", "\u{a0}", "\u{3000}", "\u{200b}", "\r\n", "\u{2028}"] {
        for bg in [None, Some("black@0.5")] {
            for px in [9.0, 24.0] {
                let mut c = Case::new(format!("blank {:?} {px}px {}", text, bg.is_some()), text, px);
                if let Some(bg) = bg {
                    c = c.boxed(bg);
                }
                l.run(&c);
            }
        }
    }
    // Control characters `drawtext` is never given, and line breaks it is given as blanks.
    for text in [
        "x\u{7}y\u{1b}z Ay",
        "line one\nline two\r\nthree\rfour",
        "a\u{2028}b\u{2029}c",
        "\u{0}Nul\u{0}Te",
    ] {
        for px in [10.0, 22.0] {
            l.run(&Case::new(format!("control {text:?} {px}px"), text, px));
        }
    }
    l.finish("oversized, blank, spaces");
}
