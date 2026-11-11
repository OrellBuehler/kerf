//! The independent checker's plane-level sweep of the odd-size round trip (no GPU): the layer FFmpeg hands `overlay` -- the export's own
//! `format=yuva420p,colorchannelmixer=aa=..,format=yuva420p` text -- against `kerf_gpu::roundtrip`'s scalar model, sample for sample, over
//! sizes the probe does not look at (it looks at 47x31, 47x32 and 48x31), contents that make every branch of the arithmetic happen (noise,
//! saturated extremes the 32-bit full-chroma route wraps on, smooth ramps), every alpha level, every matrix, the graph's slice threads and the
//! SIMD kernel sets of other machines.
//!
//! `#[ignore]`d (needs ffmpeg), local only (CI does not run this target). Run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_odd2_model -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
//!
//! A figure that is not zero is a failure; the sweeps list every size that differs before they fail.

#![allow(clippy::print_stderr)]

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::Stdio;

use kerf_core::layer_geometry::ffmpeg_alpha_of;
use kerf_core::YuvMatrix;
use kerf_gpu::roundtrip::{self, OddWidth};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }

    fn byte(&mut self) -> u8 {
        self.next() as u8
    }

    fn pick(&mut self, from: &[u8]) -> u8 {
        from[self.next() as usize % from.len()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Content {
    /// Every sample random.
    Noise,
    /// Luma and chroma from the extremes (out of range ones too), so the table clips and the full-chroma route wraps.
    Extremes,
    /// A smooth ramp both ways (no content for the bicubic overshoot to ring on).
    Smooth,
}

#[derive(Clone)]
struct Picture {
    size: (usize, usize),
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

fn picture(content: Content, (w, h): (usize, usize), seed: u64) -> Picture {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut rng = Lcg(0x9e37_79b9_7f4a_7c15 ^ seed.wrapping_mul(0x1000_0000_01b3));
    let (y, u, v) = match content {
        Content::Noise => (
            (0..w * h).map(|_| rng.byte()).collect(),
            (0..cw * ch).map(|_| rng.byte()).collect(),
            (0..cw * ch).map(|_| rng.byte()).collect(),
        ),
        Content::Extremes => (
            (0..w * h).map(|_| rng.pick(&[0, 16, 17, 100, 235, 236, 255])).collect(),
            (0..cw * ch).map(|_| rng.pick(&[0, 16, 128, 240, 255, 1, 254])).collect(),
            (0..cw * ch).map(|_| rng.pick(&[0, 16, 128, 240, 255, 1, 254])).collect(),
        ),
        Content::Smooth => (
            (0..w * h)
                .map(|i| (16 + (i % w) * 219 / w.max(1) + (i / w) * 3) as u8)
                .collect(),
            (0..cw * ch).map(|i| (32 + (i % cw) * 190 / cw.max(1)) as u8).collect(),
            (0..cw * ch).map(|i| (224 - (i / cw) * 190 / ch.max(1)) as u8).collect(),
        ),
    };
    Picture { size: (w, h), y, u, v }
}

/// Every alpha level in a picture, each in different company: a stride coprime with 256 per row.
fn levels((w, h): (usize, usize), seed: u64) -> Vec<u8> {
    (0..w * h)
        .map(|i| {
            let (x, r) = (i % w, i / w);
            (x * 167 + r * 91 + seed as usize * 13 + (i / 256)) as u8
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct Case {
    why: &'static str,
    opacity: f64,
    tag: Option<&'static str>,
    forward: YuvMatrix,
    masked: bool,
}

const CASES: [Case; 12] = [
    Case {
        why: "untagged 0.6",
        opacity: 0.6,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: false,
    },
    Case {
        why: "untagged 0.999",
        opacity: 0.999,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: false,
    },
    Case {
        why: "untagged 0.04",
        opacity: 0.04,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: false,
    },
    Case {
        why: "bt709 0.6",
        opacity: 0.6,
        tag: Some("bt709"),
        forward: YuvMatrix::Bt709,
        masked: false,
    },
    Case {
        why: "bt2020nc 0.7",
        opacity: 0.7,
        tag: Some("bt2020nc"),
        forward: YuvMatrix::Bt2020,
        masked: false,
    },
    Case {
        why: "smpte170m 0.5",
        opacity: 0.5,
        tag: Some("smpte170m"),
        forward: YuvMatrix::Bt601,
        masked: false,
    },
    Case {
        why: "bt470bg 0.3",
        opacity: 0.3,
        tag: Some("bt470bg"),
        forward: YuvMatrix::Bt601,
        masked: false,
    },
    Case {
        why: "masked untagged 0.5",
        opacity: 0.5,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: true,
    },
    Case {
        why: "masked untagged 0.85",
        opacity: 0.85,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: true,
    },
    Case {
        why: "masked untagged 0.33",
        opacity: 0.33,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: true,
    },
    Case {
        why: "masked bt709 0.999",
        opacity: 0.999,
        tag: Some("bt709"),
        forward: YuvMatrix::Bt709,
        masked: true,
    },
    Case {
        why: "masked bt2020nc 0.61",
        opacity: 0.61,
        tag: Some("bt2020nc"),
        forward: YuvMatrix::Bt2020,
        masked: true,
    },
];

fn graph(case: &Case) -> String {
    let mut parts = Vec::new();
    if let Some(tag) = case.tag {
        parts.push(format!("setparams=colorspace={tag}"));
    }
    parts.push("format=yuva420p".to_string());
    parts.push(kerf_core::opacity_filter(case.opacity));
    parts.push("format=yuva420p".to_string());
    parts.join(",")
}

/// FFmpeg's layer: `[Y, U, V, A]`. `extra` goes ahead of the input (`-cpuflags`, `-filter_threads`).
fn ffmpeg_layer(case: &Case, pic: &Picture, alpha: &[u8], extra: &[&str]) -> Vec<Vec<u8>> {
    let (w, h) = pic.size;
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let size = format!("{w}x{h}");
    let pix = if case.masked { "yuva420p" } else { "yuv420p" };
    let mut raw = [&pic.y, &pic.u, &pic.v].into_iter().flatten().copied().collect::<Vec<u8>>();
    if case.masked {
        raw.extend_from_slice(alpha);
    }
    let graph = graph(case);
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(extra)
        .args(["-f", "rawvideo", "-pix_fmt", pix, "-s", &size, "-i", "pipe:0", "-vf", &graph])
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuva420p", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&raw);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    assert!(
        out.status.success(),
        "ffmpeg {size} {graph}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (luma, chroma) = (w * h, cw * ch);
    assert_eq!(out.stdout.len(), 2 * luma + 2 * chroma, "{size}");
    let o = out.stdout;
    vec![
        o[..luma].to_vec(),
        o[luma..luma + chroma].to_vec(),
        o[luma + chroma..luma + 2 * chroma].to_vec(),
        o[luma + 2 * chroma..].to_vec(),
    ]
}

fn model(case: &Case, pic: &Picture, alpha: &[u8], back: YuvMatrix, odd: OddWidth) -> Option<Vec<Vec<u8>>> {
    let (w, h) = pic.size;
    let rgb = roundtrip::yuv_to_rgb([&pic.y, &pic.u, &pic.v], (w, h), case.forward)?;
    let [y, u, v] = roundtrip::rgb_to_yuv(&rgb, (w, h), back, odd)?;
    let mut a: Vec<u8> = alpha.iter().map(|l| ffmpeg_alpha_of(*l, case.opacity, &[])).collect();
    if roundtrip::zero_last_column(w, h) {
        for r in 0..h {
            a[r * w + w - 1] = 0;
        }
    }
    Some(vec![y, u, v, a])
}

/// Per plane: how many samples differ, the largest difference, the first (x, y).
fn compare(size: (usize, usize), got: &[Vec<u8>], want: &[Vec<u8>]) -> Vec<(usize, i32, Option<(usize, usize)>)> {
    let (w, h) = size;
    let cw = w.div_ceil(2);
    let _ = h;
    got.iter()
        .zip(want)
        .enumerate()
        .map(|(p, (g, m))| {
            let pw = if p == 0 || p == 3 { w } else { cw };
            let mut n = 0;
            let mut max = 0;
            let mut first = None;
            for (i, (a, b)) in g.iter().zip(m).enumerate() {
                if a != b {
                    n += 1;
                    max = max.max((i32::from(*a) - i32::from(*b)).abs());
                    first.get_or_insert((i % pw, i / pw));
                }
            }
            (n, max, first)
        })
        .collect()
}

fn describe(figures: &[(usize, i32, Option<(usize, usize)>)]) -> String {
    let mut s = String::new();
    for (name, (n, max, first)) in ["Y", "U", "V", "A"].iter().zip(figures) {
        if *n > 0 {
            let _ = write!(s, " {name}: {n} differ (max {max}, first {first:?});");
        }
    }
    s
}

/// What the layer of `case` on a picture of `size` comes to, against the model: `Ok` when it is the model's for one of the backs the case
/// allows, else the figures of the nearest. `odd` is the route the build is known to take.
fn layer_vs_model(
    case: &Case,
    content: Content,
    size: (usize, usize),
    seed: u64,
    odd: OddWidth,
    extra: &[&str],
) -> Result<(), String> {
    let pic = picture(content, size, seed);
    let alpha = if case.masked {
        levels(size, seed)
    } else {
        vec![255; size.0 * size.1]
    };
    let got = ffmpeg_layer(case, &pic, &alpha, extra);
    let backs: &[YuvMatrix] = if case.forward == YuvMatrix::Bt601 {
        &[YuvMatrix::Bt601]
    } else {
        &[YuvMatrix::Bt601, case.forward]
    };
    let mut worst = None;
    for back in backs {
        let Some(want) = model(case, &pic, &alpha, *back, odd) else {
            return Err("the model has no filter table for this size".into());
        };
        let figures = compare(size, &got, &want);
        let total: usize = figures.iter().map(|f| f.0).sum();
        if total == 0 {
            return Ok(());
        }
        if worst.as_ref().is_none_or(|(t, _)| total < *t) {
            worst = Some((total, describe(&figures)));
        }
    }
    Err(worst.map(|w| w.1).unwrap_or_default())
}

/// The route this build takes: the one the compositor's probe picked.
fn route() -> OddWidth {
    let found = kerf_gpu::roundtrip_probe::odd_translucency();
    eprintln!("this FFmpeg's odd sizes: {found:?}");
    found.model().unwrap_or_else(|| panic!("the probe found no model: {found:?}"))
}

fn contents() -> [Content; 3] {
    [Content::Noise, Content::Extremes, Content::Smooth]
}

/// Runs `jobs` on two threads, collecting the failures.
fn parallel(jobs: Vec<(String, Box<dyn Fn() -> Result<(), String> + Send>)>) -> (usize, Vec<String>) {
    let total = jobs.len();
    let queue = std::sync::Mutex::new(jobs.into_iter());
    let failures = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..2 {
            s.spawn(|| loop {
                let next = queue.lock().unwrap().next();
                let Some((name, job)) = next else { break };
                if let Err(why) = job() {
                    failures.lock().unwrap().push(format!("{name}:{why}"));
                }
            });
        }
    });
    (total, failures.into_inner().unwrap())
}

fn finish(family: &str, total: usize, failures: Vec<String>) {
    eprintln!("{family}: {total} layers compared, {} differ from the model", failures.len());
    for f in &failures {
        eprintln!("DIFFERS {f}");
    }
    assert!(
        failures.is_empty(),
        "{family}: {} of {total} layers are not the model's",
        failures.len()
    );
}

type Job = (String, Box<dyn Fn() -> Result<(), String> + Send>);

fn job(
    name: String,
    case: Case,
    content: Content,
    size: (usize, usize),
    seed: u64,
    odd: OddWidth,
    extra: Vec<&'static str>,
) -> Job {
    (
        name,
        Box::new(move || layer_vs_model(&case, content, size, seed, odd, &extra)),
    )
}

/// Every odd width from 17 up to 131 beside heights on either side of the chroma filter's breakpoints, and every odd height beside widths of
/// both parities: the picture sizes a user's scale factor lands on, rather than the three the probe looks at.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_odd2_model_every_odd_width_and_height() {
    let odd = route();
    let mut jobs: Vec<Job> = Vec::new();
    let mut n = 0usize;
    let mut add = |size: (usize, usize)| {
        let k = n;
        n += 1;
        // Four cases a size, rotating through the twelve, and a different content for each.
        for j in 0..4 {
            let case = CASES[(k * 4 + j * 3 + k / 12) % CASES.len()];
            let content = contents()[(k + j) % 3];
            jobs.push(job(
                format!("{}x{} {} {content:?}", size.0, size.1, case.why),
                case,
                content,
                size,
                (k * 7 + j) as u64,
                odd,
                vec![],
            ));
        }
    };
    for w in (17..=131).step_by(2) {
        for h in [16, 17, 18, 19, 31, 32, 33, 36, 47, 64, 65] {
            add((w, h));
        }
    }
    for h in (17..=131).step_by(2) {
        for w in [16, 17, 18, 32, 33, 48, 49, 98] {
            add((w, h));
        }
    }
    let (total, failures) = parallel(jobs);
    finish("every odd width and height", total, failures);
}

/// Every one of the twelve cases on a handful of sizes (the sizes the compositor's own layers take most often), all three contents.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_odd2_model_every_case_on_the_sizes_a_scale_factor_lands_on() {
    let odd = route();
    let mut jobs: Vec<Job> = Vec::new();
    let sizes = [
        (211, 118),
        (236, 133),
        (225, 169),
        (95, 169),
        (441, 248),
        (633, 356),
        (321, 241),
        (177, 99),
        (17, 17),
        (17, 16),
        (16, 17),
        (31, 31),
        (47, 31),
        (47, 32),
        (48, 31),
        (255, 255),
        (257, 129),
        (301, 17),
        (17, 301),
    ];
    for (k, size) in sizes.into_iter().enumerate() {
        for case in CASES {
            for content in contents() {
                jobs.push(job(
                    format!("{}x{} {} {content:?}", size.0, size.1, case.why),
                    case,
                    content,
                    size,
                    k as u64,
                    odd,
                    vec![],
                ));
            }
        }
    }
    let (total, failures) = parallel(jobs);
    finish("every case on the common sizes", total, failures);
}

/// The sizes of a 540 px preview and a 1080p export: the picture is a few hundred to 1900 samples a side, the graph's scale slices it across
/// threads (`ff_sws_slice_worker`; a slice starts at an odd row of the RGB picture, at an even one of the planar), and the filter tables are long.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_odd2_model_large_pictures_and_slice_threads() {
    let odd = route();
    let mut jobs: Vec<Job> = Vec::new();
    let sizes = [
        (597, 335),
        (1733, 975),
        (1919, 1079),
        (1921, 1081),
        (1081, 1919),
        (1920, 1079),
        (1919, 1080),
        (961, 541),
        (959, 539),
        (480, 271),
    ];
    for (k, size) in sizes.into_iter().enumerate() {
        for threads in ["1", "2", "3", "4", "8"] {
            let case = CASES[(k + threads.len() * 3) % CASES.len()];
            let content = contents()[k % 3];
            jobs.push((
                format!("{}x{} {} {content:?} filter_threads {threads}", size.0, size.1, case.why),
                Box::new(move || {
                    layer_vs_model(
                        &case,
                        content,
                        size,
                        k as u64,
                        odd,
                        &["-filter_threads", threads, "-threads", threads],
                    )
                }),
            ));
        }
    }
    let (total, failures) = parallel(jobs);
    finish("large pictures and slice threads", total, failures);
}

/// Sizes under 16 samples a side, which the plan refuses: where the model is FFmpeg's anyway the refusal is wider than it has to be
/// (reported, not judged: the plan's line is a choice, and the figure is what the checker's report carries).
#[test]
#[ignore = "needs ffmpeg"]
fn checker_odd2_model_small_pictures_where_the_plan_refuses() {
    let odd = route();
    let mut exact = Vec::new();
    let mut inexact = Vec::new();
    for w in 1..=20usize {
        for h in 1..=20usize {
            if w % 2 == 0 && h % 2 == 0 || w.min(h) >= 16 {
                continue;
            }
            let mut bad = Vec::new();
            for (k, content) in contents().into_iter().enumerate() {
                for case in [CASES[0], CASES[3], CASES[7]] {
                    if let Err(why) = layer_vs_model(&case, content, (w, h), k as u64, odd, &[]) {
                        bad.push(format!("{} {content:?}:{why}", case.why));
                    }
                }
            }
            if bad.is_empty() {
                exact.push((w, h));
            } else {
                inexact.push(format!("{w}x{h}: {}", bad[0]));
            }
        }
    }
    eprintln!("small odd pictures the model gets exactly (refused all the same): {exact:?}");
    for i in &inexact {
        eprintln!("small odd picture not the model's: {i}");
    }
    eprintln!("{} small odd sizes exact, {} not", exact.len(), inexact.len());
}

/// Other machines run other kernels: with the SIMD sets switched off one by one, a size the probe does not look at must not be the model's
/// while the probe's three are (that would be a machine the probe passes and the layers are wrong on). Reports per flag set which sizes are the
/// model's; fails only on a flag set that passes the probe's sizes and differs elsewhere.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_odd2_model_other_kernel_sets_differ_at_the_probes_sizes_or_not_at_all() {
    let odd = route();
    let probe_sizes = [(47usize, 31usize), (47, 32), (48, 31)];
    let wide_sizes = [
        (211usize, 118usize),
        (236, 133),
        (225, 169),
        (441, 248),
        (321, 241),
        (95, 169),
        (17, 17),
        (33, 33),
        (131, 65),
    ];
    let flag_sets: [&[&str]; 10] = [
        &[],
        &["-cpuflags", "-avx512"],
        &["-cpuflags", "-avx2"],
        &["-cpuflags", "-avx"],
        &["-cpuflags", "-sse4.2"],
        &["-cpuflags", "-sse4.1"],
        &["-cpuflags", "-ssse3"],
        &["-cpuflags", "-sse3"],
        &["-cpuflags", "-sse2"],
        &["-cpuflags", "0"],
    ];
    let mut gaps = Vec::new();
    for flags in flag_sets {
        let run = |sizes: &[(usize, usize)]| -> usize {
            let mut bad = 0;
            for (k, size) in sizes.iter().enumerate() {
                for case in [CASES[0], CASES[1], CASES[3], CASES[7], CASES[8], CASES[10]] {
                    for content in [Content::Noise, Content::Extremes] {
                        if layer_vs_model(&case, content, *size, k as u64, odd, flags).is_err() {
                            bad += 1;
                        }
                    }
                }
            }
            bad
        };
        let (at_probe, elsewhere) = (run(&probe_sizes), run(&wide_sizes));
        eprintln!("kernel set {flags:?}: {at_probe} layers differ at the probe's sizes, {elsewhere} elsewhere");
        if at_probe == 0 && elsewhere > 0 {
            gaps.push(format!(
                "{flags:?}: {elsewhere} layers differ elsewhere while the probe's sizes match"
            ));
        }
    }
    assert!(gaps.is_empty(), "a kernel set the probe would pass: {gaps:?}");
}
