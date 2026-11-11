//! `checker/ko2/plane`: what FFmpeg does to a real `chromakey` alpha plane afterwards, against the arithmetic the compositor draws it with, attacked
//! from outside. The chains are written here from `build_clip_chain`'s text (`format=yuva420p`, the engine's own `chromakey` text, then either
//! `colorchannelmixer=aa=<opacity>` and `format=yuva420p` for a constant opacity, or one `geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='(curve)*alpha(X,Y)'`
//! for a keyed one), run over pictures made so that the key's alpha takes many levels (a chroma ramp on both axes, noise on top), at sizes odd and
//! even. The key branch and the opacity's branches come out of one graph (one `split` after `format=yuva420p`), so they have the same slice threads,
//! which the key's alpha depends on. Every sample of every plane is compared, with no tolerance, on this FFmpeg (`KERF_FFMPEG` picks the pinned one).
//! Needs no adapter.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_ko2_plane -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 3) as u8).collect()
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

fn bits(x: f64, by: i64) -> f64 {
    f64::from_bits((x.to_bits() as i64 + by) as u64)
}

/// A `w` x `h` raw `yuv420p` picture whose chroma is a ramp on both axes with noise (the key's distance, a 3x3 average, then takes many values) over
/// noisy luma.
fn picture(w: usize, h: usize, seed: u64) -> Vec<u8> {
    let mut rng = Lcg(seed);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let noise = rng.bytes(2 * cw * ch);
    let ramp = |n: usize, i: usize, x: u8| ((i * 255 / (n - 1).max(1)) as i32 + i32::from(x) % 9 - 4).clamp(0, 255) as u8;
    let mut raw = rng.bytes(w * h);
    for cy in 0..ch {
        for cx in 0..cw {
            raw.push(ramp(cw, cx, noise[cy * cw + cx]));
        }
    }
    for cy in 0..ch {
        for cx in 0..cw {
            raw.push(ramp(ch, cy, noise[cw * ch + cy * cw + cx]));
        }
    }
    raw
}

/// FFmpeg's `yuva420p` planes for each of `branches` (a filter chain each, run on the keyed picture from one `split`), the key alone as the first.
/// `prefix` is the chain ahead of the split (`format=yuva420p,chromakey=...`).
fn run_branches(w: usize, h: usize, raw: &[u8], prefix: &str, branches: &[String]) -> Vec<[Vec<u8>; 4]> {
    let n = branches.len();
    static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "checker-ko2-plane-{}-{}",
        std::process::id(),
        RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let outs: String = (0..n).map(|i| format!("[s{i}]")).collect();
    let mut graph = format!("[0:v]{prefix},split={n}{outs}");
    for (i, b) in branches.iter().enumerate() {
        graph += &format!(";[s{i}]{b}[o{i}]");
    }
    let mut cmd = Command::new(kerf_core::ffmpeg_path());
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{w}x{h}"),
            "-i",
            "pipe:0",
        ])
        .args(["-filter_complex", &graph]);
    for i in 0..n {
        cmd.args([
            "-map",
            &format!("[o{i}]"),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuva420p",
        ])
        .arg(dir.join(format!("o{i}.yuv")));
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let data = raw.to_vec();
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    assert!(out.status.success(), "{}\n{graph}", String::from_utf8_lossy(&out.stderr));
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let planes = (0..n)
        .map(|i| {
            let b = std::fs::read(dir.join(format!("o{i}.yuv"))).unwrap();
            assert_eq!(b.len(), 2 * w * h + 2 * cw * ch, "{w}x{h} branch {i}");
            let (y, rest) = b.split_at(w * h);
            let (u, rest) = rest.split_at(cw * ch);
            let (v, a) = rest.split_at(cw * ch);
            [y.to_vec(), u.to_vec(), v.to_vec(), a.to_vec()]
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    planes
}

fn split_input(raw: &[u8], w: usize, h: usize) -> [Vec<u8>; 3] {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    [
        raw[..w * h].to_vec(),
        raw[w * h..w * h + cw * ch].to_vec(),
        raw[w * h + cw * ch..].to_vec(),
    ]
}

/// A plane read as 6.1's `geq` reads it: the last column and row are the ones before them (`w` and `h` at least 2).
fn penultimate(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| src[y.min(h - 2) * w + x.min(w - 2)])
        .collect()
}

fn count(a: &[u8], b: &[u8]) -> (usize, i32, Option<usize>) {
    let mut n = 0;
    let mut max = 0;
    let mut first = None;
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        let d = (i32::from(*x) - i32::from(*y)).abs();
        if d != 0 {
            n += 1;
            max = max.max(d);
            first.get_or_insert(i);
        }
    }
    (n, max, first)
}

/// The doubles that make `curve * level` land on an integer or a hair off it, as the geq's product sees them: the fractions `n / d` for every
/// `d` up to 255 and the doubles either side of each, the 255 grid and its neighbours, the hundredths and the thousandths, and the ends.
fn curves() -> Vec<f64> {
    let mut v = vec![0.0, 1.0, bits(1.0, -1), bits(1.0, -2), 0.5, bits(0.5, -1), bits(0.5, 1)];
    let mut fractions = Vec::new();
    for d in 2..=255u32 {
        for n in 1..d {
            let c = f64::from(n) / f64::from(d);
            let hairline = (1..=255u32).any(|level| {
                let exact = c * f64::from(level);
                let near = exact.round();
                exact != near && (exact - near).abs() < 1e-9
            });
            if hairline {
                fractions.push(c);
            }
        }
    }
    fractions.sort_by(|a, b| a.total_cmp(b));
    fractions.dedup();
    let stride = (fractions.len() / 260).max(1);
    for c in fractions.iter().step_by(stride) {
        for by in [-1, 0, 1] {
            v.push(bits(*c, by));
        }
    }
    for k in (1..=255u32).step_by(5) {
        let c = f64::from(k) / 255.0;
        for by in [-1, 0, 1] {
            v.push(bits(c, by));
        }
    }
    for k in (1..=100u32).step_by(3) {
        v.push(f64::from(k) / 100.0);
    }
    v.retain(|c| *c <= 1.0);
    v.sort_by(|a, b| a.total_cmp(b));
    v.dedup();
    v
}

/// `geq=...:a='(curve)*alpha(X,Y)'` over a real key's alpha: every plane of every branch is the filter's, for the curves of [`curves`], at several
/// sizes (the planes `geq` hands on are the input's with the last column and row replaced on 6.1) and several keys (a wide blend so the alpha
/// takes most levels, a narrow one, a hard edge). The expected alpha is `trunc(curve * level)` of the key's level read where `geq` reads it.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_ko2_plane_the_geq_over_a_real_keys_alpha_is_the_truncated_product_of_curve_and_level() {
    let edge = kerf_core::geq_edge();
    assert_ne!(edge, kerf_core::GeqEdge::Unknown, "the geq's edge could not be measured");
    let all = curves();
    eprintln!("checker/ko2/plane: {} curves, geq edge {edge:?}", all.len());
    let mut compared = 0usize;
    let mut levels_seen = [false; 256];
    for (si, (w, h)) in [(98usize, 62usize), (97, 61), (34, 18), (66, 6), (4, 4), (5, 7), (250, 40)]
        .into_iter()
        .enumerate()
    {
        let raw = picture(w, h, 0x9e37_79b9 + si as u64);
        let [y, u, v] = split_input(&raw, w, h);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        for (color, sim, blend) in [
            ("0x808080", 0.05, 1.0),
            ("0x00b140", 0.1, 0.3),
            ("0x1a8cd8", 0.02, 0.6),
            ("green", 0.3, 0.0),
        ] {
            let prefix = format!("format=yuva420p,{}", kerf_core::chroma_key_filter(color, sim, blend));
            for chunk in all.chunks(40) {
                let mut branches = vec!["format=yuva420p".to_string()];
                branches.extend(
                    chunk
                        .iter()
                        .map(|c| format!("geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='({})*alpha(X,Y)'", fnum(*c))),
                );
                let out = run_branches(w, h, &raw, &prefix, &branches);
                let key = &out[0];
                assert!(
                    key[0] == y && key[1] == u && key[2] == v,
                    "{w}x{h} {color}: chromakey changed the colour planes"
                );
                key[3].iter().for_each(|l| levels_seen[usize::from(*l)] = true);
                let read = |plane: &[u8], pw: usize, ph: usize| -> Vec<u8> {
                    match edge {
                        kerf_core::GeqEdge::Penultimate if pw >= 2 && ph >= 2 => penultimate(plane, pw, ph),
                        _ => plane.to_vec(),
                    }
                };
                if edge == kerf_core::GeqEdge::Penultimate && (cw < 2 || ch < 2) {
                    continue;
                }
                let (ey, eu, ev) = (read(&y, w, h), read(&u, cw, ch), read(&v, cw, ch));
                let key_read = read(&key[3], w, h);
                for (c, got) in chunk.iter().zip(&out[1..]) {
                    let want_alpha: Vec<u8> = key_read.iter().map(|l| (*c * f64::from(*l)) as u8).collect();
                    for (name, want, have) in [
                        ("Y", &ey, &got[0]),
                        ("U", &eu, &got[1]),
                        ("V", &ev, &got[2]),
                        ("A", &want_alpha, &got[3]),
                    ] {
                        let (n, max, first) = count(want, have);
                        let pw = if name == "U" || name == "V" { cw } else { w };
                        assert_eq!(
                            n, 0,
                            "{w}x{h} {color} {sim}/{blend} curve {c}: plane {name}: {n} samples differ (max {max}, first at {:?})",
                            first.map(|i| (i % pw, i / pw))
                        );
                    }
                    compared += 1;
                }
            }
        }
    }
    let distinct = levels_seen.iter().filter(|s| **s).count();
    eprintln!("checker/ko2/plane: {compared} (picture, key, curve) branches, 4 planes each, 0 samples differ; the keys' alpha took {distinct} distinct levels");
    assert!(distinct >= 250, "the keys must take most levels: {distinct}");
}

/// The opacities where `lrint(level * opacity)` ties or `(a * 256 + 127) / 255` rounds: the 255 grid's ties and the doubles either side, the ties of
/// `level * opacity` for the levels that make most (`(2j + 1) / (2n)`), the ends.
fn opacities() -> Vec<f64> {
    let mut v = vec![
        0.0,
        1e-9,
        0.001,
        0.5,
        bits(0.5, -1),
        bits(0.5, 1),
        0.999999,
        bits(1.0, -1),
        0.25,
        0.75,
        1.0 / 3.0,
        2.0 / 3.0,
    ];
    for k in (0..255u32).step_by(2) {
        let tie = (f64::from(k) + 0.5) / 255.0;
        for by in [-2, -1, 0, 1, 2] {
            v.push(bits(tie, by));
        }
    }
    for n in 1..=24u32 {
        for j in 0..n {
            let t = (2.0 * f64::from(j) + 1.0) / (2.0 * f64::from(n));
            if t < 1.0 {
                v.push(t);
                v.push(bits(t, -1));
                v.push(bits(t, 1));
            }
        }
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v.dedup();
    v.retain(|o| *o < 1.0);
    v
}

/// `colorchannelmixer=aa=<opacity>,format=yuva420p` (the RGB round trip) over a real key's alpha: the alpha plane that comes out is `lrint(level *
/// opacity)` scaled by 256/255 (written out here from `vf_colorchannelmixer.c`'s table and swscale's conversion back, and checked against
/// `kerf_core::layer_geometry::ffmpeg_alpha_of`, the compositor's map), and the colour planes that come out are those of the same chain over an opaque
/// alpha (the round trip does not see the alpha). Every opacity, picture and key.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_ko2_plane_the_round_trip_over_a_real_keys_alpha_maps_every_level_as_the_model_says() {
    let all = opacities();
    eprintln!("checker/ko2/plane: {} opacities", all.len());
    let mut compared = 0usize;
    let mut levels_seen = [false; 256];
    for (si, (w, h)) in [(98usize, 62usize), (250, 40)].into_iter().enumerate() {
        let raw = picture(w, h, 0x1234_5678 + si as u64);
        for (color, sim, blend) in [("0x808080", 0.05, 1.0), ("0x00b140", 0.1, 0.3), ("0x1a8cd8", 0.02, 0.6)] {
            let keyed_prefix = format!("format=yuva420p,{}", kerf_core::chroma_key_filter(color, sim, blend));
            for chunk in all.chunks(40) {
                let mut branches = vec!["format=yuva420p".to_string()];
                branches.extend(
                    chunk
                        .iter()
                        .map(|o| format!("{},format=yuva420p", kerf_core::opacity_filter(*o))),
                );
                let keyed = run_branches(w, h, &raw, &keyed_prefix, &branches);
                let opaque = run_branches(w, h, &raw, "format=yuva420p", &branches);
                keyed[0][3].iter().for_each(|l| levels_seen[usize::from(*l)] = true);
                for (i, o) in chunk.iter().enumerate() {
                    let want: Vec<u8> = keyed[0][3]
                        .iter()
                        .map(|l| {
                            let a = (f64::from(*l) * *o).round_ties_even() as u32;
                            ((a * 256 + 127) / 255).min(255) as u8
                        })
                        .collect();
                    let model: Vec<u8> = keyed[0][3]
                        .iter()
                        .map(|l| kerf_core::layer_geometry::ffmpeg_alpha_of(*l, *o, &[]))
                        .collect();
                    assert_eq!(
                        want, model,
                        "opacity {o}: the compositor's map is not the one written from the filters"
                    );
                    let got = &keyed[i + 1];
                    let (n, max, first) = count(&want, &got[3]);
                    assert_eq!(
                        n,
                        0,
                        "{w}x{h} {color} {sim}/{blend} opacity {o}: alpha: {n} samples differ (max {max}, first at {:?})",
                        first.map(|i| (i % w, i / w))
                    );
                    for (p, name) in ["Y", "U", "V"].into_iter().enumerate() {
                        let (n, max, first) = count(&opaque[i + 1][p], &got[p]);
                        assert_eq!(n, 0, "{w}x{h} {color} opacity {o}: plane {name} depends on the alpha plane: {n} differ (max {max}, first {first:?})");
                    }
                    compared += 1;
                }
            }
        }
    }
    let distinct = levels_seen.iter().filter(|s| **s).count();
    eprintln!("checker/ko2/plane: {compared} (picture, key, opacity) round trips, 4 planes each, 0 samples differ; the keys' alpha took {distinct} distinct levels");
    assert!(distinct >= 250, "the keys must take most levels: {distinct}");
}
