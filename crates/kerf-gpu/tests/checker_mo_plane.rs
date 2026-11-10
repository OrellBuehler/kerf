//! `checker/mo/plane`: the alpha plane one `geq` writes for a mask under a keyed opacity, `a='(keep)*(curve)*alpha(X,Y)'`, against
//! `kerf_core::mask_alpha_plane_keyed`, attacked from outside: the expression is written here from `build_clip_chain`'s text (not taken from
//! the engine), the sizes are odd and tiny as well as ordinary, the masks are drawn at random and at the places a shape's edge falls on a sample
//! boundary, and the curve's value is a number FFmpeg's `av_expr` parses back to the `f64` printed: points of the 255 grid and their neighbours
//! (where `keep * curve * 255` is within an ulp of an integer for some `keep`), the hundredths and thousandths, values a lerp makes, and random
//! doubles. Every sample of every plane is compared with no tolerance, on this FFmpeg (`KERF_FFMPEG` picks the pinned one). Needs no adapter.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_mo_plane -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

use std::process::{Command, Stdio};

use kerf_core::{Mask, MaskShape};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }

    fn unit(&mut self) -> f64 {
        self.next() as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn fnum(v: f64) -> String {
    let s = format!("{v}");
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

/// The keep of a mask as `geq` reads it: both axes scaled so the edge is at distance 1, `max` for a rectangle and `hypot` for an ellipse, a
/// hard edge `lte(d,1)` up to a feather of 1e-6 and a ramp `clip((1-d)/feather,0,1)` inside the edge above it, `1-` for an inverted mask.
fn keep(mask: &Mask) -> String {
    let m = mask.normalized();
    let dx = format!("(X-{}*W)/({}*W)", fnum(m.x), fnum(m.width / 2.0));
    let dy = format!("(Y-{}*H)/({}*H)", fnum(m.y), fnum(m.height / 2.0));
    let d = match m.shape {
        MaskShape::Rect => format!("max(abs({dx})\\,abs({dy}))"),
        MaskShape::Ellipse => format!("hypot({dx}\\,{dy})"),
    };
    let inside = if m.feather <= 1e-6 {
        format!("lte({d}\\,1)")
    } else {
        format!("clip((1-{d})/{}\\,0\\,1)", fnum(m.feather))
    };
    if m.inverted {
        format!("(1-{inside})")
    } else {
        inside
    }
}

fn geq(mask: &Mask, curve: f64) -> String {
    format!(
        "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='({})*({})*alpha(X,Y)'",
        keep(mask),
        fnum(curve)
    )
}

/// FFmpeg's alpha planes for `curves` under `mask` on a `w` x `h` frame, one `geq` each, all in one run (`split`, `hstack`; `curves.len()` even).
fn geq_planes(w: u32, h: u32, mask: &Mask, curves: &[f64]) -> Vec<Vec<u8>> {
    let n = curves.len();
    assert!(n.is_multiple_of(2));
    let (ew, eh) = (w + w % 2, h + h % 2);
    let outs: String = (0..n).map(|i| format!("[a{i}]")).collect();
    let mut graph = format!("color=c=black:s={ew}x{eh}:r=1:d=1,scale={w}:{h},format=yuva420p,split={n}{outs}");
    for (i, c) in curves.iter().enumerate() {
        graph += &format!(";[a{i}]{}[o{i}]", geq(mask, *c));
    }
    graph += ";";
    graph += &(0..n).map(|i| format!("[o{i}]")).collect::<String>();
    graph += &format!("hstack=inputs={n},format=yuva420p[out]");
    let run = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-filter_complex_threads",
            "1",
        ])
        .args(["-filter_complex", &graph])
        .args(["-map", "[out]"])
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuva420p", "-"])
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(run.status.success(), "{}\n{graph}", String::from_utf8_lossy(&run.stderr));
    let (wt, ht) = ((w as usize) * n, h as usize);
    let chroma = wt.div_ceil(2) * ht.div_ceil(2);
    assert_eq!(run.stdout.len(), wt * ht * 2 + chroma * 2, "{w}x{h} x {n}");
    let alpha = &run.stdout[wt * ht + chroma * 2..];
    (0..n)
        .map(|i| {
            (0..ht)
                .flat_map(|y| alpha[y * wt + i * w as usize..y * wt + (i + 1) * w as usize].iter().copied())
                .collect()
        })
        .collect()
}

fn bits(x: f64, by: i64) -> f64 {
    f64::from_bits((x.to_bits() as i64 + by) as u64)
}

/// The curve values a case is run at.
fn curve_values(rng: &mut Lcg, n: usize) -> Vec<f64> {
    let mut v = Vec::new();
    while v.len() < n {
        let pick = rng.below(9);
        let k = rng.below(256) as f64;
        let x = match pick {
            0 => k / 255.0,
            1 => bits(k / 255.0, 1),
            2 => bits(k / 255.0, -1),
            3 => (rng.below(101) as f64) / 100.0,
            4 => (rng.below(1001) as f64) / 1000.0,
            5 => rng.unit(),
            // a lerp's value between two keys at a frame time
            6 => {
                let (a, b) = (rng.unit(), rng.unit());
                let (t0, t1, t) = (0.0, 1.0, rng.below(31) as f64 / 30.0);
                a + (b - a) * (t - t0) / (t1 - t0)
            }
            7 => [0.0, 1.0, bits(1.0, -1), 5e-324, 1e-17][rng.below(5)],
            _ => (rng.below(97) as f64) / 96.0,
        };
        v.push(x.clamp(0.0, 1.0));
    }
    if v.len() % 2 == 1 {
        v.push(0.5);
    }
    v
}

fn random_mask(rng: &mut Lcg) -> Mask {
    let shape = if rng.below(2) == 0 {
        MaskShape::Rect
    } else {
        MaskShape::Ellipse
    };
    let grid = |rng: &mut Lcg| (rng.below(41) as f64) / 40.0;
    let size = |rng: &mut Lcg| match rng.below(4) {
        0 => 0.01 + rng.unit() * 1.99,
        1 => [0.01, 0.02, 0.25, 0.5, 0.6, 0.9, 1.0, 1.5, 2.0][rng.below(9)],
        2 => (1 + rng.below(40)) as f64 / 20.0,
        _ => rng.unit(),
    };
    let feather = match rng.below(6) {
        0 => 0.0,
        1 => [1e-6, 1.0000001e-6, 2e-6, 0.999999, 1.0, 1e-3, 1e-5][rng.below(7)],
        2 => (1 + rng.below(20)) as f64 / 20.0,
        3 => rng.unit(),
        4 => (1 + rng.below(96)) as f64 / 96.0,
        _ => 0.5,
    };
    Mask {
        shape,
        x: if rng.below(2) == 0 { grid(rng) } else { rng.unit() },
        y: if rng.below(2) == 0 { grid(rng) } else { rng.unit() },
        width: size(rng),
        height: size(rng),
        feather,
        inverted: rng.below(2) == 0,
    }
}

/// Whether the compositor refuses a `geq` on a `w` x `h` frame on this FFmpeg: a build whose `geq` reads a plane's last column and row one in
/// (6.1) has nothing to read from in a chroma plane under two samples a side, and reads outside the plane there.
fn refused_size(w: u32, h: u32) -> bool {
    kerf_core::geq_edge() == kerf_core::GeqEdge::Penultimate && (w.div_ceil(2) < 2 || h.div_ceil(2) < 2)
}

/// Returns the samples compared; a plane that differs is a failure unless the compositor refuses the size (`refused`, counted).
fn compare(w: u32, h: u32, mask: &Mask, curves: &[f64], failures: &mut Vec<String>, refused: &mut usize) -> usize {
    let planes = geq_planes(w, h, mask, curves);
    let mut compared = 0;
    for (curve, got) in curves.iter().zip(&planes) {
        let want = kerf_core::mask_alpha_plane_keyed(mask, w, h, *curve);
        compared += want.len();
        if let Some(first) = (0..want.len()).find(|&i| got[i] != want[i]) {
            let n = (0..want.len()).filter(|&i| got[i] != want[i]).count();
            if refused_size(w, h) {
                *refused += 1;
                continue;
            }
            failures.push(format!(
                "{w}x{h} {mask:?} curve {curve:e} ({:#018x}): {n} samples differ, first at ({}, {}): geq {} ours {}",
                curve.to_bits(),
                first % w as usize,
                first / w as usize,
                got[first],
                want[first]
            ));
        }
    }
    compared
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn checker_mo_plane_a_keyed_mask_plane_is_the_geq_plane_on_random_masks_sizes_and_curves() {
    let mut rng = Lcg(0x005e_ed0f_c0ff_ee00);
    let sizes: [(u32, u32); 14] = [
        (2, 2),
        (1, 1),
        (3, 1),
        (1, 7),
        (7, 255),
        (255, 7),
        (33, 17),
        (64, 36),
        (97, 61),
        (129, 65),
        (211, 118),
        (320, 180),
        (641, 359),
        (640, 360),
    ];
    let (mut compared, mut cases) = (0usize, 0usize);
    let mut failures = Vec::new();
    let mut refused = 0usize;
    for round in 0..40 {
        for &(w, h) in &sizes {
            // (The big frames are slow: a geq per sample.)
            if w * h > 100_000 && round % 8 != 0 {
                continue;
            }
            let mask = random_mask(&mut rng);
            let curves = curve_values(&mut rng, if w * h > 100_000 { 4 } else { 16 });
            compared += compare(w, h, &mask, &curves, &mut failures, &mut refused);
            cases += curves.len();
        }
    }
    eprintln!("checker/mo/plane: {cases} (mask, size, curve) planes, {compared} alpha samples compared with geq's, {} planes differ ({refused} more, on sizes the compositor refuses on this build)", failures.len());
    let mut by_size: std::collections::BTreeMap<String, usize> = Default::default();
    for f in &failures {
        *by_size.entry(f.split(' ').next().unwrap_or("").to_string()).or_default() += 1;
    }
    eprintln!("checker/mo/plane: differing planes by size {by_size:?}");
    assert!(failures.is_empty(), "{}", failures[..failures.len().min(12)].join("\n"));
}

/// The knife's edge, searched for: for masks whose keep is a multiple of 1/q on a stretch of the frame (a rectangle's feather `f` over a frame
/// of width `W` makes keep a multiple of `1/(f * W * width / 2)` along a row) and curves `c / 255` and `c / q`, the product is an integer
/// on paper at many samples; the last bit of keep and of the curve decides each. A thousand such pairs, a 640x360 frame each is too slow, so
/// the frame is the stretch (a 192x20 one of the same width ratio).
#[test]
#[ignore = "needs the ffmpeg binary"]
fn checker_mo_plane_the_curves_and_feathers_whose_product_is_an_integer_on_paper() {
    let mut rng = Lcg(0xfeed_beef_1234);
    let (mut compared, mut cases) = (0usize, 0usize);
    let mut failures = Vec::new();
    let mut refused = 0usize;
    for (w, h) in [(192u32, 20u32), (320, 12), (160, 10), (96, 8), (200, 10), (240, 16)] {
        for round in 0..60 {
            // A feather that makes keep a multiple of 1/q: the ramp is (1 - d) / feather with d = |X - cx*W| / (rw*W).
            let q = [24.0, 32.0, 48.0, 64.0, 72.0, 96.0, 100.0, 120.0, 128.0, 160.0, 192.0][round % 11];
            let width = [0.6, 0.9, 0.5, 0.8, 0.75, 1.0][rng.below(6)];
            let feather = [0.5, 0.7, 0.25, 0.4, 1.0, 0.3, 0.1][rng.below(7)];
            let mask = Mask {
                shape: if round % 5 == 0 { MaskShape::Ellipse } else { MaskShape::Rect },
                x: 0.5,
                y: 0.5,
                width,
                height: 0.9,
                feather,
                inverted: rng.below(2) == 0,
            };
            let mut curves: Vec<f64> = Vec::new();
            for _ in 0..8 {
                let c = (1 + rng.below(255)) as f64;
                curves.push(c / 255.0);
                curves.push(c / q);
            }
            for c in &mut curves {
                *c = c.clamp(0.0, 1.0);
            }
            // The neighbours of each in the last bit.
            let near: Vec<f64> = curves.iter().take(4).flat_map(|c| [bits(*c, 1), bits(*c, -1)]).collect();
            curves.extend(near);
            if curves.len() % 2 == 1 {
                curves.push(0.5);
            }
            compared += compare(w, h, &mask, &curves, &mut failures, &mut refused);
            cases += curves.len();
        }
    }
    eprintln!("checker/mo/plane knife: {cases} planes, {compared} alpha samples compared with geq's, {} planes differ ({refused} more, on sizes the compositor refuses on this build)", failures.len());
    assert!(failures.is_empty(), "{}", failures[..failures.len().min(12)].join("\n"));
}
