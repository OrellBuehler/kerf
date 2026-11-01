//! The conversion of a layer into the composite's matrix (`kerf_gpu::convert`, FFmpeg 9's auto-inserted `scale`),
//! plane for plane, on pictures of random samples.
//!
//! * the compositor's passes (`Compositor::convert_yuv`) against the scalar reference they follow
//!   ([`kerf_gpu::convert::change_matrix`], [`kerf_gpu::convert::change_matrix_alpha`]) — needs an adapter, not FFmpeg;
//! * the reference against FFmpeg itself, through the overlay chain of an export (`#[ignore]`d: needs `ffmpeg`, and only
//!   a build that negotiates the colourspace along the chain (`CompositeColorPolicy::BottomLayerTag`) converts).

#![allow(clippy::print_stderr)]

use std::sync::{Arc, OnceLock};

use kerf_core::YuvMatrix;
use kerf_gpu::convert::{change_matrix, Variant};
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
