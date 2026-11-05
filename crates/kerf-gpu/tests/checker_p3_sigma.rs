//! `checker/p3/sigma`: only a sigma of exactly 0 is the identity the plan lifts a hazard for; every other sigma is judged as the blur it is
//! (or refused), measured against the real `gblur` on the build `KERF_FFMPEG` names (6.1.1 by default) and through the whole path.
//!
//! * `checker_p3_sigma_the_filter_text_and_the_real_filter_for_every_odd_sigma`: the text the engine writes (`kerf_core::blur_filter`:
//!   `-0.0` is written `0`, a denormal as a decimal of three hundred digits, NaN as `NaN`) run through FFmpeg on a plane the kernel has no
//!   hazard on and on planes it has: the output of sigma 0 and `-0.0` is the input; every sigma the plan can clear is the compositor's model
//!   (`effects::blur_plane`), to the sample; what FFmpeg does with the text it must refuse (NaN, out of range) is printed.
//! * `checker_p3_sigma_the_plan_lifts_nothing_but_zero`: the plan's verdict (`picture_hazard_for` as the plan calls it) on the planes whose
//!   only hazard is the empty postscale slice: lifted at `0.0` and `-0.0` and kept at every other sigma, however tiny, in a mix as well.
//!   And what the real filter does on those planes (`EmptySlices`, `8` or `16` slice threads): a refusal of a tiny sigma that FFmpeg draws
//!   as the input is counted and printed (a refusal wider than the kernel, kept on purpose).
//! * `checker_p3_sigma_gpu_against_ffmpeg_export_and_still`: an export frame and a still of noise with each sigma, on a canvas the kernel has
//!   no hazard on and on one whose chroma has fewer rows than the graph has threads (and 40x6, 64x4): no frame the plan clears may differ
//!   from FFmpeg's in a sample; sigma 0 is drawn on the second kind, a tiny sigma is refused there (and drawn anyway, reported).
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_p3_sigma -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
#![allow(clippy::print_stderr)]

#[path = "common/checker_p3.rs"]
mod p3;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use kerf_core::gblur::{gaussian, picture_hazard_for};
use kerf_gpu::effects::{blur_plane, gaussian as model_gaussian};
use p3::{blurred, motion_frame, noise_clip, one_track, still_outcome, Outcome};

static SERIAL: Mutex<()> = Mutex::new(());

const TIMEOUT: Duration = Duration::from_secs(60);

/// The sigmas worth a case: zero in both spellings, the denormals and the smallest normals of both widths, the ladder of tiny values that
/// cancel (`1 + 2 lambda - sqrt(1 + 4 lambda)`) and the edge of the domain the compositor draws, and plain ones.
fn sigmas() -> Vec<f64> {
    vec![
        0.0,
        -0.0,
        5e-324,
        f64::MIN_POSITIVE,
        1e-300,
        1e-50,
        1.4e-45,
        1.2e-38,
        1e-20,
        1e-12,
        1e-9,
        1e-8,
        1.05e-8,
        1.1e-8,
        1.29e-8,
        1.3e-8,
        1e-7,
        1e-6,
        1e-5,
        1e-4,
        1e-3,
        0.01,
        0.5,
        3.0,
        1024.0,
    ]
}

fn lcg(seed: u64, n: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2b99_17f3_0c5a_6d41;
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

type Planes = [Vec<u8>; 3];

fn dims(w: usize, h: usize) -> [(usize, usize); 3] {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    [(w, h), (cw, ch), (cw, ch)]
}

/// `w` x `h` noise through `filter` on `threads` slice threads (a complex graph, as an export's), `None` when FFmpeg would not run it.
fn run_filter(w: usize, h: usize, filter: &str, threads: usize) -> Option<(Planes, Planes)> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let planes = [
        lcg(1 + (w * 131 + h) as u64, w * h),
        lcg(2 + (w * 137 + h) as u64, cw * ch),
        lcg(3 + (w * 139 + h) as u64, cw * ch),
    ];
    let raw: Vec<u8> = planes.iter().flatten().copied().collect();
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
    ])
    .args(["-s", &format!("{w}x{h}"), "-i", "pipe:0"])
    .args(["-filter_complex", &format!("[0:v]{filter}[v]"), "-map", "[v]"])
    .args([
        "-filter_complex_threads",
        &threads.to_string(),
        "-filter_threads",
        &threads.to_string(),
    ])
    .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"]);
    let out = kerf_core::run_piped_until(&mut cmd, raw, Instant::now() + TIMEOUT)?;
    if out.len() != w * h + 2 * cw * ch {
        return None;
    }
    let (y, rest) = out.split_at(w * h);
    let (u, v) = rest.split_at(cw * ch);
    Some((planes, [y.to_vec(), u.to_vec(), v.to_vec()]))
}

fn all_planes_equal(w: usize, h: usize, got: &Planes, want: impl Fn(usize, usize, usize) -> Vec<u8>) -> bool {
    dims(w, h)
        .into_iter()
        .enumerate()
        .all(|(k, (pw, ph))| got[k] == want(k, pw, ph))
}

/// The kernel's vector width on this machine (12x16 on one thread: 16 where the luma plane is not the model's, 8 where the chroma is not).
fn kernel_lanes() -> u32 {
    let g = model_gaussian(3.0).expect("sigma 3");
    let (planes, got) = run_filter(12, 16, &kerf_core::blur_filter(3.0), 1).expect("FFmpeg runs");
    let (luma, chroma) = (
        got[0] == blur_plane(&planes[0], 12, 16, &g),
        got[1] == blur_plane(&planes[1], 6, 8, &g),
    );
    match (luma, chroma) {
        (false, true) => 16,
        (true, false) => 8,
        other => panic!("no AVX2 or AVX-512 gblur kernel here: {other:?}"),
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_p3_sigma_the_filter_text_and_the_real_filter_for_every_odd_sigma() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        kerf_core::blur_filter(-0.0),
        kerf_core::blur_filter(0.0),
        "-0.0 is written as 0"
    );
    assert_eq!(kerf_core::blur_filter(0.0), "gblur=sigma=0");
    let mut failures = Vec::new();
    let (w, h) = (64usize, 64usize);
    for sigma in sigmas() {
        let text = kerf_core::blur_filter(sigma);
        let short: String = text.chars().take(60).collect();
        for threads in [1usize, 3] {
            let Some((planes, got)) = run_filter(w, h, &text, threads) else {
                eprintln!("CHECKER-P3 sigma: {short}... on {threads} threads: FFmpeg did not run it");
                continue;
            };
            let as_input = all_planes_equal(w, h, &got, |k, _, _| planes[k].clone());
            let g = model_gaussian(sigma);
            let as_model = g
                .as_ref()
                .map(|g| all_planes_equal(w, h, &got, |k, pw, ph| blur_plane(&planes[k], pw, ph, g)));
            eprintln!(
                "CHECKER-P3 sigma {sigma:e}: {short}... {threads} threads: input {as_input}, model {as_model:?} (the compositor draws {})",
                if sigma == 0.0 { "no pass" } else if g.is_some() { "a pass" } else { "nothing: refused" }
            );
            if sigma == 0.0 && !as_input {
                failures.push(format!("sigma {sigma:e} ({short}) on {threads} threads is not the input"));
            }
            if as_model == Some(false) {
                failures.push(format!(
                    "sigma {sigma:e} ({short}) on {threads} threads is not the compositor's model"
                ));
            }
        }
    }
    for text in [
        "gblur=sigma=NaN",
        "gblur=sigma=nan",
        "gblur=sigma=inf",
        "gblur=sigma=-1e-9",
        "gblur=sigma=1024.5",
        "gblur=sigma=-0",
    ] {
        match run_filter(16, 16, text, 1) {
            Some((planes, got)) => eprintln!("CHECKER-P3 sigma: {text} runs; the input unchanged: {}", got == planes),
            None => eprintln!("CHECKER-P3 sigma: {text} is refused by FFmpeg"),
        }
    }
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1e-9, -1.0, 1024.5, 5000.0] {
        assert!(gaussian(bad).is_none(), "{bad}: the plan takes a sigma the filter refuses");
    }
    assert!(gaussian(-0.0).is_some_and(|g| g.is_identity()));
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_p3_sigma_the_plan_lifts_nothing_but_zero() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let lanes = kernel_lanes();
    // Planes whose only hazard is the empty postscale slice: wide enough that no block runs past the row, rows to spare for none.
    let shapes: [(usize, usize, usize); 6] = [(40, 6, 8), (64, 4, 16), (16, 4, 8), (40, 6, 16), (64, 8, 16), (128, 2, 16)];
    let (mut wider, mut checked, mut failures) = (0usize, 0usize, Vec::new());
    let mut wider_at: Vec<String> = Vec::new();
    for (w, h, t) in shapes {
        for sigma in sigmas() {
            let picture = (w as u32, h as u32);
            let identity = sigma == 0.0;
            let hazard = picture_hazard_for(picture, t as u32..=t as u32, lanes, identity);
            let hazard_as_blur = picture_hazard_for(picture, t as u32..=t as u32, lanes, false);
            if !identity && hazard != hazard_as_blur {
                failures.push(format!(
                    "{w}x{h} on {t} threads, sigma {sigma:e}: not judged as a blur like another"
                ));
            }
            if identity && hazard.is_some() && hazard_as_blur.is_none() {
                failures.push(format!("{w}x{h} on {t} threads, sigma 0 refused where a blur is not"));
            }
            let text = kerf_core::blur_filter(sigma);
            // (Three runs of each: the empty slice races the real one.)
            let mut exact_as_input = true;
            let mut ran = 0;
            for _ in 0..3 {
                if let Some((planes, got)) = run_filter(w, h, &text, t) {
                    ran += 1;
                    exact_as_input &= all_planes_equal(w, h, &got, |k, _, _| planes[k].clone());
                }
            }
            if ran == 0 {
                continue;
            }
            checked += 1;
            if identity && hazard.is_none() && !exact_as_input {
                failures.push(format!(
                    "{w}x{h} on {t} threads, sigma {sigma:e}: cleared and FFmpeg's output is not the input"
                ));
            }
            if !identity && gaussian(sigma).is_some_and(|g| g.is_identity()) && hazard.is_some() && exact_as_input {
                wider += 1;
                wider_at.push(format!("{w}x{h}/{t}t/{sigma:e}"));
            }
        }
    }
    // Sigma 0 alone on a plane whose only hazard is the empty slice: lifted.
    assert!(
        picture_hazard_for((40, 6), 8..=8, lanes, true).is_none(),
        "sigma 0 alone: lifted"
    );
    eprintln!(
        "CHECKER-P3 sigma: {checked} (shape, sigma) cases run; {wider} refusals of a tiny sigma whose output FFmpeg makes the input (3 of 3 runs), e.g. {:?}",
        &wider_at[..wider_at.len().min(6)]
    );
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    assert!(wider > 0 || checked == 0, "no tiny sigma met a hazard: the sweep is empty");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p3_sigma_gpu_against_ffmpeg_export_and_still() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // (The app's default budget is 75 %: three slice threads on four cores. At 100 % an uncapped graph has `cores + 1`, which is what puts
    // an empty slice in the chroma of 40x6 and the luma of 64x4.)
    let was = kerf_core::cpu_percent();
    kerf_core::set_cpu_percent(100);
    let mut failures = Vec::new();
    let (mut exact, mut refused, mut refused_but_exact) = (0usize, 0usize, 0usize);
    let mut lifted_zero = 0usize;
    // 64x64: clear for every thread count up to 16. 40x6 and 64x4: the chroma has fewer rows than an uncapped graph has threads
    // (4 CPUs: five threads; the plan lifts the empty slice at sigma 0 and at sigma 0 alone).
    for (w, h) in [(64u32, 64u32), (40, 6), (64, 4)] {
        let asset = noise_clip(w, h, "yuv420p");
        for sigma in sigmas() {
            for kind in ["export", "still"] {
                let tl = one_track(vec![blurred(&asset, sigma, 0.3)]);
                let case = format!("{kind} {w}x{h} sigma {sigma:e}");
                let out = if kind == "export" {
                    motion_frame(
                        &format!("sigma-{w}x{h}-{}", sigma.to_bits()),
                        &tl,
                        std::slice::from_ref(&asset),
                        30.0,
                        2,
                    )
                } else {
                    still_outcome(&tl, std::slice::from_ref(&asset), 0.1)
                };
                eprintln!("CHECKER-P3 sigma gpu: {case}: {}", out.word());
                match &out {
                    Outcome::Exact => {
                        exact += 1;
                        if sigma == 0.0 && (h < 8) {
                            lifted_zero += 1;
                        }
                    }
                    Outcome::Differs(g) => failures.push(format!("{case}: the plan cleared it and the GPU differs: {g:?}")),
                    Outcome::Refused { drawn_anyway, .. } => {
                        refused += 1;
                        if drawn_anyway.as_ref().is_some_and(|g| g.iter().all(Option::is_none)) {
                            refused_but_exact += 1;
                        }
                        if sigma == 0.0 && h >= 4 && w >= 40 {
                            failures.push(format!(
                                "{case}: sigma 0 refused on a plane whose only hazard is the empty slice: {out:?}"
                            ));
                        }
                    }
                    Outcome::Other(why) => eprintln!("CHECKER-P3 sigma gpu: {case}: refused for another reason {why:?}"),
                }
                // A tiny nonzero sigma on a plane that is a hazard for a blur is not lifted.
                if sigma != 0.0 && h < 8 && matches!(out, Outcome::Exact) && kind == "export" {
                    let tiny = model_gaussian(sigma).is_some_and(|g| g.is_identity());
                    if tiny {
                        failures.push(format!(
                            "{case}: a tiny sigma was drawn on a plane the empty slice is a hazard for"
                        ));
                    }
                }
            }
        }
    }
    kerf_core::set_cpu_percent(was);
    eprintln!("CHECKER-P3 sigma gpu: {exact} exact, {refused} refused ({refused_but_exact} of them exact when drawn anyway), sigma 0 lifted in {lifted_zero}");
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    assert!(
        exact > 0 && refused > 0 && lifted_zero > 0,
        "the sweep should meet a draw, a refusal and the lift"
    );
}
