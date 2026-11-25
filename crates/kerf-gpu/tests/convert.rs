//! The conversion of a layer into the composite's matrix (`kerf_gpu::convert`, FFmpeg 9's auto-inserted `scale`),
//! plane for plane, on pictures of random samples.
//!
//! * the compositor's passes (`Compositor::convert_yuv`) against the scalar reference they follow
//!   ([`kerf_gpu::convert::change_matrix`], [`kerf_gpu::convert::change_matrix_alpha`]) — `#[ignore]`d: needs an
//!   adapter, not FFmpeg;
//! * the reference against FFmpeg itself, through the overlay chain of an export (`#[ignore]`d: needs `ffmpeg`, and only
//!   a build that negotiates the colourspace along the chain (`CompositeColorPolicy::BottomLayerTag`) converts);
//! * the same for a `scale` that resizes a picture as it takes it from one matrix to another (swscale's cascade through a
//!   `bgr24` picture of the destination's size, `scale_and_change_matrix`): the passes against the reference, and the reference
//!   against FFmpeg's own `scale` on pictures of random samples.

#![allow(clippy::print_stderr)]

use std::sync::{Arc, OnceLock};

use kerf_core::YuvMatrix;
use kerf_gpu::convert::{change_matrix, scale_and_change_matrix, Packed, Pic, Variant};
use kerf_gpu::{Compositor, Gpu, GpuOptions, YuvFrame};

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| Compositor::new(gpu()).expect("a compositor"))
}

/// Noise in all three planes, from a seed: every Y, U, V combination, clipped RGB included.
fn noise(w: u32, h: u32, seed: u64) -> YuvFrame {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 24) as u8
    };
    let (cw, ch) = (w / 2, h / 2);
    YuvFrame {
        width: w,
        height: h,
        y: (0..w * h).map(|_| next()).collect(),
        u: (0..cw * ch).map(|_| next()).collect(),
        v: (0..cw * ch).map(|_| next()).collect(),
    }
}

const MATRICES: [YuvMatrix; 3] = [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020];

fn planes(f: &YuvFrame) -> [&[u8]; 3] {
    [&f.y, &f.u, &f.v]
}

fn differing(got: &YuvFrame, want: &[Vec<u8>; 3]) -> [usize; 3] {
    let d = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(x, y)| x != y).count();
    [d(&got.y, &want[0]), d(&got.u, &want[1]), d(&got.v, &want[2])]
}

#[test]
#[ignore = "needs a GPU adapter: the parity job runs it on lavapipe"]
fn the_passes_are_the_reference_for_every_pair_of_matrices_and_size() {
    for (w, h) in [(2, 2), (4, 2), (16, 16), (34, 18), (96, 64), (130, 40), (640, 360)] {
        for (seed, (from, to)) in MATRICES
            .iter()
            .flat_map(|f| MATRICES.iter().map(move |t| (*f, *t)))
            .filter(|(f, t)| f != t)
            .enumerate()
        {
            let frame = noise(w, h, 1 + seed as u64 + u64::from(w));
            let got = compositor().convert_yuv(&frame, from, to).expect("the passes");
            let want = change_matrix(planes(&frame), w as usize, h as usize, from, to, Variant::C);
            assert_eq!(differing(&got, &want), [0; 3], "{w}x{h} {from:?} -> {to:?}");
        }
    }
}

#[test]
#[ignore = "needs a GPU adapter: the parity job runs it on lavapipe"]
fn an_odd_picture_is_refused_by_the_passes() {
    let frame = YuvFrame {
        width: 3,
        height: 2,
        y: vec![16; 6],
        u: vec![128; 2],
        v: vec![128; 2],
    };
    assert!(compositor().convert_yuv(&frame, YuvMatrix::Bt601, YuvMatrix::Bt709).is_err());
}

/// What FFmpeg itself does, through the real overlay chain (the probe's graphs), on a build that negotiates: held to
/// the model on every picture the probe looks at. On a build that does not (FFmpeg 6 composites everything as BT.601),
/// the answer is the policy's, and the conversion is not in play.
#[test]
#[ignore = "needs ffmpeg"]
fn this_ffmpeg_converts_between_matrices_as_the_model_does() {
    use kerf_core::CompositeColorPolicy;
    let policy = kerf_core::composite_color_policy();
    let measured = kerf_gpu::convert::kernels();
    eprintln!("{}: policy {policy:?}, conversion {measured:?}", kerf_core::ffmpeg_path());
    if policy == CompositeColorPolicy::BottomLayerTag {
        assert_eq!(measured, kerf_gpu::convert::Kernels::Match, "{}", kerf_core::ffmpeg_path());
        assert_eq!(kerf_gpu::convert::measured(), kerf_gpu::convert::Kernels::Match);
    }
}

/// Sizes the cascade is held to: shrinks of both axes, one axis only, a mild shrink, a steep one, wide, tall and the phone clip of
/// the ledger's T5. All even (the model follows nothing else) and with more than two taps in every vertical table.
const SCALED: [((u32, u32), (u32, u32)); 14] = [
    ((128, 96), (40, 30)),
    ((64, 48), (62, 46)),
    ((64, 48), (60, 40)),
    ((96, 64), (90, 50)),
    ((200, 100), (150, 70)),
    ((200, 100), (10, 80)),
    ((100, 200), (80, 10)),
    ((222, 126), (110, 62)),
    ((500, 100), (40, 80)),
    ((100, 500), (200, 20)),
    ((640, 360), (640, 100)),
    ((1280, 720), (538, 302)),
    ((1920, 1080), (540, 302)),
    ((1080, 1920), (170, 302)),
];

fn scaled_reference(frame: &YuvFrame, size: (u32, u32), from: YuvMatrix, to: YuvMatrix) -> [Vec<u8>; 3] {
    let pic = Pic {
        planes: planes(frame),
        size: (frame.width as usize, frame.height as usize),
    };
    scale_and_change_matrix(pic, (size.0 as usize, size.1 as usize), from, to, Packed::Mmx).expect("a size the model follows")
}

#[test]
#[ignore = "needs a GPU adapter: the parity job runs it on lavapipe"]
fn the_scaling_passes_are_the_reference_for_every_pair_of_matrices_and_size() {
    for (i, ((w, h), size)) in SCALED.into_iter().enumerate() {
        for (j, (from, to)) in MATRICES
            .iter()
            .flat_map(|f| MATRICES.iter().map(move |t| (*f, *t)))
            .filter(|(f, t)| f != t)
            .enumerate()
        {
            // (The big pictures with two pairs only: a software adapter draws them slowly.)
            if w * h > 300_000 && j > 1 {
                continue;
            }
            let frame = noise(w, h, 100 + (i * 7 + j) as u64);
            let got = compositor().scale_convert_yuv(&frame, size, from, to).expect("the passes");
            let want = scaled_reference(&frame, size, from, to);
            assert_eq!(
                differing(&got, &want),
                [0; 3],
                "{w}x{h} -> {}x{} {from:?} -> {to:?}",
                size.0,
                size.1
            );
        }
    }
}

#[test]
#[ignore = "needs a GPU adapter: the parity job runs it on lavapipe"]
fn a_picture_the_cascade_does_not_follow_is_refused_by_the_passes() {
    let frame = noise(64, 48, 3);
    // Odd sides, a picture that grows, and one whose luma rows are not scaled.
    for size in [(31, 24), (32, 25), (80, 60), (32, 48), (64, 49)] {
        assert!(
            compositor()
                .scale_convert_yuv(&frame, size, YuvMatrix::Bt709, YuvMatrix::Bt601)
                .is_err(),
            "{size:?}"
        );
    }
}

fn ffmpeg_scale(frame: &YuvFrame, size: (u32, u32), from: &str, to: &str, pix_fmt: &str, threads: u32) -> Vec<u8> {
    use std::io::Write;
    use std::process::Stdio;
    let mut input = frame.y.clone();
    input.extend(&frame.u);
    input.extend(&frame.v);
    let vf = format!(
        "setparams=colorspace={from}:range=tv,scale={}:{}:out_color_matrix={to},format={pix_fmt}",
        size.0, size.1
    );
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(["-hide_banner", "-loglevel", "error", "-filter_threads"])
        .arg(threads.to_string())
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-s"])
        .arg(format!("{}x{}", frame.width, frame.height))
        .args([
            "-i",
            "-",
            "-vf",
            &vf,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            pix_fmt,
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("ffmpeg");
    let mut stdin = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    writer.join().expect("writer");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// What FFmpeg itself does, through a bare `scale` that names both matrices (the probe's chains are the export's own; this is the
/// model on many more pictures, at 1, 4 and 16 filter threads: swscale's own slices): held to the model on a build that cascades. On a build that does not (FFmpeg 6 never asks for
/// it, and its `scale` has one matrix), the answer is the probe's.
#[test]
#[ignore = "needs ffmpeg"]
fn this_ffmpeg_scales_and_converts_a_picture_as_the_model_does() {
    use kerf_core::CompositeColorPolicy;
    if kerf_core::composite_color_policy() != CompositeColorPolicy::BottomLayerTag {
        eprintln!(
            "{}: policy {:?}, nothing is converted",
            kerf_core::ffmpeg_path(),
            kerf_core::composite_color_policy()
        );
        return;
    }
    assert_eq!(
        kerf_gpu::convert::scaled(),
        kerf_gpu::convert::Scaled::Match,
        "{}",
        kerf_core::ffmpeg_path()
    );
    let mut compared = 0usize;
    for (i, ((w, h), size)) in SCALED.into_iter().enumerate() {
        let frame = noise(w, h, 200 + i as u64);
        for (from, to, from_tag, to_tag) in [
            (YuvMatrix::Bt709, YuvMatrix::Bt601, "bt709", "smpte170m"),
            (YuvMatrix::Bt601, YuvMatrix::Bt709, "smpte170m", "bt709"),
            (YuvMatrix::Bt2020, YuvMatrix::Bt709, "bt2020nc", "bt709"),
        ] {
            let want = scaled_reference(&frame, size, from, to);
            let n = (size.0 * size.1) as usize;
            // The destination has an alpha plane (a dissolving clip's chain ends in `yuva420p`) or not (a plain picture).
            for (pix_fmt, threads) in [("yuv420p", 1), ("yuva420p", 4), ("yuva420p", 16)] {
                let got = ffmpeg_scale(&frame, size, from_tag, to_tag, pix_fmt, threads);
                assert!(got.len() >= n * 3 / 2, "{w}x{h} -> {size:?}: {} bytes", got.len());
                let at = format!("{w}x{h} -> {size:?} {from:?} -> {to:?} {pix_fmt} at {threads} filter threads");
                assert_eq!(&got[..n], &want[0][..], "{at}: luma");
                assert_eq!(&got[n..n + n / 4], &want[1][..], "{at}: U");
                assert_eq!(&got[n + n / 4..n + n / 2], &want[2][..], "{at}: V");
                compared += 3 * n / 2;
            }
        }
    }
    eprintln!(
        "{}: {compared} samples of the cascade equal FFmpeg's",
        kerf_core::ffmpeg_path()
    );
}

// ---- the cascade's other half: a picture that is enlarged as it is converted ---------------------------------------------

/// The frames the blurred fill's up-scale is held to, each from its own thumbnail (`2 * floor(side / 16)`, at least 4): the ledger's phone
/// vlog (a 960x540 frame at the preview's and the export's sizes), tall and square frames, odd multiples of 16, the smallest the box blur
/// runs in, and 4K.
const FRAMES: [(u32, u32); 14] = [
    (32, 18),
    (34, 20),
    (64, 36),
    (100, 60),
    (126, 70),
    (202, 114),
    (320, 180),
    (360, 640),
    (540, 960),
    (960, 540),
    (1000, 1000),
    (1280, 720),
    (1920, 1080),
    (3840, 2160),
];

fn thumbnail_of((w, h): (u32, u32)) -> (u32, u32) {
    ((w / 16 * 2).max(4), (h / 16 * 2).max(4))
}

fn enlarged_reference(frame: &YuvFrame, size: (u32, u32), from: YuvMatrix, to: YuvMatrix) -> [Vec<u8>; 3] {
    let pic = Pic {
        planes: planes(frame),
        size: (frame.width as usize, frame.height as usize),
    };
    kerf_gpu::convert::enlarge_and_change_matrix(pic, (size.0 as usize, size.1 as usize), from, to)
        .expect("a size the model follows")
}

#[test]
#[ignore = "needs a GPU adapter: the parity job runs it on lavapipe"]
fn the_enlarging_passes_are_the_reference_for_every_pair_of_matrices_and_size() {
    let mut compared = 0usize;
    for (i, frame_size) in FRAMES.into_iter().enumerate() {
        let (w, h) = thumbnail_of(frame_size);
        for (j, (from, to)) in MATRICES
            .iter()
            .flat_map(|f| MATRICES.iter().map(move |t| (*f, *t)))
            .filter(|(f, t)| f != t)
            .enumerate()
        {
            // (The big frames with two pairs only: a software adapter draws them slowly.)
            if frame_size.0 * frame_size.1 > 300_000 && j > 1 {
                continue;
            }
            let thumb = noise(w, h, 300 + (i * 7 + j) as u64);
            let got = compositor()
                .enlarge_convert_yuv(&thumb, frame_size, from, to)
                .expect("the passes");
            let want = enlarged_reference(&thumb, frame_size, from, to);
            assert_eq!(differing(&got, &want), [0; 3], "{w}x{h} -> {frame_size:?} {from:?} -> {to:?}");
            compared += frame_size.0 as usize * frame_size.1 as usize * 3 / 2;
        }
    }
    eprintln!("{compared} samples of the enlarging passes equal the reference");
}

#[test]
#[ignore = "needs a GPU adapter: the parity job runs it on lavapipe"]
fn an_enlargement_the_cascade_does_not_follow_is_refused_by_the_passes() {
    let thumb = noise(16, 12, 5);
    // Odd sides, a picture that shrinks, one that grows on a single axis, and the same size.
    for size in [(129, 96), (128, 97), (8, 6), (32, 12), (16, 24), (16, 12)] {
        assert!(
            compositor()
                .enlarge_convert_yuv(&thumb, size, YuvMatrix::Bt709, YuvMatrix::Bt601)
                .is_err(),
            "{size:?}"
        );
    }
}

fn ffmpeg_scale_flagged(frame: &YuvFrame, size: (u32, u32), from: &str, to: &str, pix_fmt: &str, threads: u32) -> Vec<u8> {
    use std::io::Write;
    use std::process::Stdio;
    let mut input = frame.y.clone();
    input.extend(&frame.u);
    input.extend(&frame.v);
    let vf = format!(
        "setparams=colorspace={from}:range=tv,scale={}:{}:flags=bilinear:out_color_matrix={to},format={pix_fmt}",
        size.0, size.1
    );
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(["-hide_banner", "-loglevel", "error", "-filter_threads"])
        .arg(threads.to_string())
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-s"])
        .arg(format!("{}x{}", frame.width, frame.height))
        .args([
            "-i",
            "-",
            "-vf",
            &vf,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            pix_fmt,
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("ffmpeg");
    let mut stdin = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    writer.join().expect("writer");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// What FFmpeg itself does, through a bare `scale` that names both matrices and enlarges (`flags=bilinear`, the blurred fill's up-scale): held to
/// the model on pictures of random samples, at 1, 4 and 16 filter threads. Both builds convert so (swscale's `sws_setColorspaceDetails` is the
/// same code); whether a graph makes a scale convert is the policy's, and the probe's.
#[test]
#[ignore = "needs ffmpeg"]
fn this_ffmpeg_enlarges_and_converts_a_picture_as_the_model_does() {
    let mut compared = 0usize;
    for (i, frame_size) in FRAMES.into_iter().enumerate() {
        let (w, h) = thumbnail_of(frame_size);
        let thumb = noise(w, h, 400 + i as u64);
        for (from, to, from_tag, to_tag) in [
            (YuvMatrix::Bt709, YuvMatrix::Bt601, "bt709", "smpte170m"),
            (YuvMatrix::Bt601, YuvMatrix::Bt709, "smpte170m", "bt709"),
            (YuvMatrix::Bt2020, YuvMatrix::Bt709, "bt2020nc", "bt709"),
        ] {
            let want = enlarged_reference(&thumb, frame_size, from, to);
            let n = (frame_size.0 * frame_size.1) as usize;
            // The destination has an alpha plane (the fill's is `yuva420p`) or not.
            for (pix_fmt, threads) in [("yuv420p", 1), ("yuva420p", 4), ("yuva420p", 16)] {
                let got = ffmpeg_scale_flagged(&thumb, frame_size, from_tag, to_tag, pix_fmt, threads);
                assert!(got.len() >= n * 3 / 2, "{w}x{h} -> {frame_size:?}: {} bytes", got.len());
                let at = format!("{w}x{h} -> {frame_size:?} {from:?} -> {to:?} {pix_fmt} at {threads} filter threads");
                assert_eq!(&got[..n], &want[0][..], "{at}: luma");
                assert_eq!(&got[n..n + n / 4], &want[1][..], "{at}: U");
                assert_eq!(&got[n + n / 4..n + n / 2], &want[2][..], "{at}: V");
                compared += 3 * n / 2;
            }
        }
    }
    eprintln!(
        "{}: {compared} samples of the enlarging cascade equal FFmpeg's",
        kerf_core::ffmpeg_path()
    );
}
