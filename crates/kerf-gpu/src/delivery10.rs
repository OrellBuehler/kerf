//! The export of a 10-bit delivery (`yuv420p10le`, `yuv422p10le`: what ProRes 422 is given), reproduced to the sample.
//!
//! `overlay` composes 8-bit `yuva420p` pictures only, and a graph whose terminal format is 10-bit is not 10-bit in the middle: measured on the
//! export's own text with `-v debug`, on 6.1.1 and 9.0.2 alike, the chain of a plain clip is
//!
//! 1. the fit `scale` (`scale=W:H:force_original_aspect_ratio=increase`, the Cover `crop` after it): the format negotiation hands it the
//!    delivery's format, so it **resizes and converts in one call**, `yuv420p -> yuv420p10le` (or `yuv422p10le`: a 4:2:0 chroma plane is
//!    resampled `H/2 -> H` rows in the same pass). The horizontal pass is the 8-bit one (a 15-bit intermediate), the vertical pass writes 10
//!    bits: `yuv2planeX_10` (12-bit weights, rounded at bit 16, shifted by 17), exact on every SIMD build because it keeps 32-bit sums, so the
//!    tables are the SIMD build's (padded) and the arithmetic is the C one's;
//! 2. `fade` (a clip's own, a dip to black), **in 10 bits**: `((v - black) * factor + (black << 16) + 32768) >> 16`, black 64, chroma
//!    about 512 (the 8-bit chroma constant is nine short of that; the 10-bit one is not);
//! 3. an auto-inserted `scale` into the format `overlay` takes (`yuva420p`): from `yuv420p10le` it is swscale's `planarCopyWrapper` with an
//!    **ordered 2x2 dither** (`ff_dither_8x8_128 >> 5`: `[[1, 2], [3, 0]]`) added before the shift by two, the row index of which is the picture's
//!    on FFmpeg 9 and **the row of the slice** on FFmpeg 6.1 (the scaler's slice threads cut the destination in slices of
//!    `FFALIGN(ceil(rows / threads), 2)` luma rows, and a chroma plane's slice starts on an odd row where that is odd, which flips its
//!    pattern); from `yuv422p10le` it is the generic scaler: the luma is the same pattern (`(v << 5 + dither) >> 7`, the 8x8 table's row
//!    `y & 7`), the chroma a 2:1 vertical shrink whose 8-bit output is x86's `yuv2yuvX` with that dither as its start constant
//!    (`(dither + 8 * (taps - 1)) >> 4`, the table's column `(x + 3 * plane) & 7`) on every row but the last;
//! 4. `overlay` of the opaque layer (a copy: the alpha plane is 255), `drawtext` on the composite, both on `yuva420p` as in an 8-bit export;
//! 5. an auto-inserted `scale` out of `yuva420p` into the delivery's format: `v << 2` for 4:2:0, and for 4:2:2 the luma `v << 2` and each chroma
//!    plane resampled `H/2 -> H` rows into 10 bits, as in 1.
//!
//! The picture the encoder is given is therefore not the 8-bit chain's, and not its composite converted: the layer has been resampled and
//! faded at 10 bits and dithered down before it was blended. [`layer_planes`] and [`to_ten_bit`] are that chain in the integers it is made of
//! (a CPU model: the shader passes of `composite.wgsl` are held to it), and [`ten_bit`] measures that this FFmpeg's graph is the model, on
//! the export's own filter text over toy cuts, once per process — never assumed and never keyed on a version string. The layouts it matched
//! (and whether this FFmpeg's 4:2:0 dither restarts at each slice) are [`measured_kinds`], which `Compositor::caps` hands the plan.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kerf_core::layer_geometry::{Rect, ScaleStage};
use kerf_core::{
    Clip, Delivery, ExportOptions, Fit, OriginalMedia, PlanRequest, Planner, Project, StreamKind, TenBitKinds, TenBitLayout,
    Timeline, Track, FADE_FULL,
};

use crate::measure::{self, Budget, Probe, Timing, Verdict};
use crate::scaler::{self, Kernel, Kernels};
use crate::sws::{self, Scaler};

/// swscale's `ff_dither_8x8_128`: the ordered dither every 8-bit output of a deeper picture adds before its shift.
pub const DITHER_8X8_128: [[u32; 8]; 8] = [
    [36, 68, 60, 92, 34, 66, 58, 90],
    [100, 4, 124, 28, 98, 2, 122, 26],
    [52, 84, 44, 76, 50, 82, 42, 74],
    [116, 20, 108, 12, 114, 18, 106, 10],
    [32, 64, 56, 88, 38, 70, 62, 94],
    [96, 0, 120, 24, 102, 6, 126, 30],
    [48, 80, 40, 72, 54, 86, 46, 78],
    [112, 16, 104, 8, 118, 22, 110, 14],
];

/// The black level `fade` takes for a 10-bit YCbCr picture (`16 << 2`), and the middle of its chroma (`128 << 2`).
pub const BLACK: i64 = 64;
pub const MIDDLE: i64 = 512;

/// The 2x2 dither of the 10 -> 8 bit shift by two: `ff_dither_8x8_128 >> 5` for every row and column of the 8x8 table.
pub fn dither(row: usize, x: usize) -> u32 {
    DITHER_8X8_128[row & 7][x & 7] >> 5
}

/// What a plane of 10-bit samples is: a width, a height and `width * height` samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plane10 {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u16>,
}

/// The planes the encoder is handed: Y, U and V in 10 bits, in the layout of the delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planes10 {
    pub layout: TenBitLayout,
    pub y: Plane10,
    pub u: Plane10,
    pub v: Plane10,
}

/// The stage a chroma plane of a layer goes through, when the stage of its luma is `stage` (picture coordinates in luma samples): the plane of a
/// 4:2:0 source read at half the window, resampled to half the width and, for 4:2:0, half the height of the result, for 4:2:2 its full height.
pub fn chroma_stage(stage: &ScaleStage, layout: TenBitLayout) -> ScaleStage {
    let half = |v: u32| v.div_ceil(2);
    let src = Rect {
        x: stage.src.x / 2,
        y: stage.src.y / 2,
        w: half(stage.src.w),
        h: half(stage.src.h),
    };
    match layout {
        TenBitLayout::P420 => ScaleStage {
            src,
            scaled: (half(stage.scaled.0), half(stage.scaled.1)),
            keep: Rect {
                x: stage.keep.x / 2,
                y: stage.keep.y / 2,
                w: half(stage.keep.w),
                h: half(stage.keep.h),
            },
        },
        TenBitLayout::P422 => ScaleStage {
            src,
            scaled: (half(stage.scaled.0), stage.scaled.1),
            keep: Rect {
                x: stage.keep.x / 2,
                y: stage.keep.y,
                w: half(stage.keep.w),
                h: stage.keep.h,
            },
        },
    }
}

/// The stage of the fit `scale` of a clip delivered in `layout`, given the geometry's (`stage`, which crops on the 4:2:0 grid): the Cover crop after
/// it runs on the delivery's own chroma grid, so in 4:2:2, which has all its chroma rows, the row offset is half the rows cut (the geometry's is
/// rounded down to even); the column offset is rounded to even in both. The rows cut are even where the sides are (the plan refuses the others).
pub fn layout_stage(stage: &ScaleStage, layout: TenBitLayout) -> ScaleStage {
    match layout {
        TenBitLayout::P420 => *stage,
        TenBitLayout::P422 => ScaleStage {
            keep: Rect {
                y: stage.scaled.1.saturating_sub(stage.keep.h) / 2,
                ..stage.keep
            },
            ..*stage
        },
    }
}

/// The stage of a picture the fit does not resize or crop.
pub fn whole_stage(w: u32, h: u32) -> ScaleStage {
    ScaleStage {
        src: Rect::whole(w, h),
        scaled: (w, h),
        keep: Rect::whole(w, h),
    }
}

/// One plane of a stage in 10 bits, as swscale writes it: the window of `src` (`pw` samples a row) resampled by the tables of `scaler` (padded for
/// `kernel`'s SIMD build) into a 15-bit intermediate, and the vertical pass to 10 bits (`yuv2planeX_10`: `(1 << 16) + sum(weight * sample) >> 17`),
/// of the part of the result the stage keeps. `None` for a ratio swscale needs two scalers for.
pub fn scale_to_10(src: &[u8], pw: usize, stage: &ScaleStage, scaler: Scaler, kernel: Kernel) -> Option<Plane10> {
    let (align_h, align_v) = kernel.alignment();
    let (sx, sy, sw, sh) = (
        stage.src.x as usize,
        stage.src.y as usize,
        stage.src.w as usize,
        stage.src.h as usize,
    );
    let hf = sws::table_aligned(scaler, stage.src.w, stage.scaled.0, sws::ONE_HORIZONTAL, align_h)?;
    let vf = sws::table_aligned(scaler, stage.src.h, stage.scaled.1, sws::ONE_VERTICAL, align_v)?;
    let (kx, ky, kw, kh) = (
        stage.keep.x as usize,
        stage.keep.y as usize,
        stage.keep.w as usize,
        stage.keep.h as usize,
    );
    // The horizontal pass: the kept columns of every row of the window.
    let mut mid = vec![0i32; kw * sh];
    for row in 0..sh {
        for x in 0..kw {
            let i = x + kx;
            let acc: i32 = (0..hf.size)
                .map(|j| {
                    let col = (hf.pos[i] as usize + j).min(sw - 1);
                    hf.coeff[i * hf.size + j] * i32::from(src[(sy + row) * pw + sx + col])
                })
                .sum();
            mid[row * kw + x] = (acc >> 7).min(32767);
        }
    }
    let mut data = vec![0u16; kw * kh];
    for y in 0..kh {
        let i = y + ky;
        for x in 0..kw {
            let acc: i64 = (1 << 16)
                + (0..vf.size)
                    .map(|j| {
                        let row = (vf.pos[i] as usize + j).min(sh - 1);
                        i64::from(vf.coeff[i * vf.size + j]) * i64::from(mid[row * kw + x])
                    })
                    .sum::<i64>();
            data[y * kw + x] = (acc >> 17).clamp(0, 1023) as u16;
        }
    }
    Some(Plane10 { w: kw, h: kh, data })
}

/// `fade` on one 10-bit sample at `factor` (below [`FADE_FULL`]): the 8-bit arithmetic of `fade_luma` with the black level and the middle of the
/// 10-bit range, and a chroma constant that is `(512 << 16) + 32768` (the 8-bit one is nine short).
pub fn fade_sample(v: u16, factor: u32, chroma: bool) -> u16 {
    let mid = if chroma { MIDDLE } else { BLACK };
    (((i64::from(v) - mid) * i64::from(factor) + (mid << 16) + 32768) >> 16).clamp(0, 1023) as u16
}

/// `fade` over a plane, once per factor below [`FADE_FULL`], in order (the chain has one filter for each).
pub fn fade_plane(plane: &mut Plane10, factors: &[u32], chroma: bool) {
    for &factor in factors.iter().filter(|f| **f < FADE_FULL) {
        for v in &mut plane.data {
            *v = fade_sample(*v, factor, chroma);
        }
    }
}

/// How the unscaled 10 -> 8 bit conversion of a 4:2:0 chroma plane indexes the dither's rows: the picture's (`0`), or the row within a slice of
/// `n` chroma rows ([`kerf_core::ten_bit_dither_rows`]).
pub fn dither_row(y: usize, slice_rows: u32) -> usize {
    if slice_rows == 0 {
        y
    } else {
        y % slice_rows as usize
    }
}

/// A plane taken from 10 bits to 8 by `planarCopyWrapper`'s shift with the 2x2 dither: `(v + dither) >> 2`, clipped. `slice_rows` as in
/// [`dither_row`] (always `0` for luma: its slices start on even rows).
pub fn dither_plane(plane: &Plane10, slice_rows: u32) -> Vec<u8> {
    let mut out = vec![0u8; plane.w * plane.h];
    for y in 0..plane.h {
        let row = dither_row(y, slice_rows);
        for x in 0..plane.w {
            out[y * plane.w + x] = ((u32::from(plane.data[y * plane.w + x]) + dither(row, x)) >> 2).min(255) as u8;
        }
    }
    out
}

/// A 4:2:2 chroma plane (`cw` x `h`) taken to the 4:2:0 plane `yuva420p` has (`cw` x `h / 2`): the generic scaler, a 10-bit input
/// (`hScale16To15` of an unscaled row is `v << 5`), the 2:1 bicubic vertical table and the 8-bit output with the 8x8 dither of the row
/// `y & 7` and the column `(x + offset) & 7` (`offset` 3 for V, 0 for U): x86's `yuv2yuvX` on every row but the last (`+ (dither + 8 * (taps
/// - 1)) >> 4`, each tap's product `>> 16`, the sum `>> 3`), the C kernel on that one; or the C kernel on all of them for a build without it.
pub fn chroma_to_420(plane: &Plane10, offset: usize, kernel: Kernel) -> Option<Vec<u8>> {
    let (cw, h) = (plane.w, plane.h);
    let dh = h / 2;
    let (_, align_v) = kernel.alignment();
    let vf = sws::table_aligned(Scaler::Bicubic, h as u32, dh as u32, sws::ONE_VERTICAL, align_v)?;
    let mut out = vec![0u8; cw * dh];
    for i in 0..dh {
        let simd = kernel.simd_vertical() && vf.size > 1 && i + 1 < dh;
        for x in 0..cw {
            let d = DITHER_8X8_128[i & 7][(x + offset) & 7] as i32;
            let taps = (0..vf.size).map(|j| {
                let row = (vf.pos[i] as usize + j).min(h - 1);
                (vf.coeff[i * vf.size + j], i32::from(plane.data[row * cw + x]) << 5)
            });
            out[i * cw + x] = if simd {
                let acc: i32 = ((d + 8 * (vf.size as i32 - 1)) >> 4) + taps.map(|(c, s)| (c * s) >> 16).sum::<i32>();
                (acc >> 3).clamp(0, 255) as u8
            } else {
                let acc: i64 = (i64::from(d) << 12) + taps.map(|(c, s)| i64::from(c) * i64::from(s)).sum::<i64>();
                (acc >> 19).clamp(0, 255) as u8
            };
        }
    }
    Some(out)
}

/// What a layer of a 10-bit delivery goes through between its decode and the `overlay`.
#[derive(Debug, Clone)]
pub struct LayerChain {
    pub layout: TenBitLayout,
    /// The fit `scale`'s stage ([`whole_stage`] when the picture is neither resized nor cropped: the scale is still in the graph, as the
    /// conversion).
    pub stage: ScaleStage,
    /// The factors of the clip's fades that are running, in the order the chain has them.
    pub fades: Vec<u32>,
    /// The rows of a chroma slice the 4:2:0 conversion's dither restarts at; `0`: the picture's rows.
    pub slice_rows: u32,
    pub scaler: Scaler,
    pub kernel: Kernel,
}

/// The planes `overlay` is handed for a clip: `src` is the decoded 8-bit 4:2:0 picture (`w` x `h`, chroma `ceil(w / 2)` x `ceil(h / 2)`).
/// `None` for a ratio swscale needs two scalers for.
pub fn layer_planes(src: [&[u8]; 3], (w, _h): (usize, usize), chain: &LayerChain) -> Option<[Vec<u8>; 3]> {
    let cw = w.div_ceil(2);
    let stage = layout_stage(&chain.stage, chain.layout);
    let luma = scale_to_10(src[0], w, &stage, chain.scaler, chain.kernel)?;
    let cstage = chroma_stage(&stage, chain.layout);
    let mut u = scale_to_10(src[1], cw, &cstage, chain.scaler, chain.kernel)?;
    let mut v = scale_to_10(src[2], cw, &cstage, chain.scaler, chain.kernel)?;
    let mut luma = luma;
    fade_plane(&mut luma, &chain.fades, false);
    fade_plane(&mut u, &chain.fades, true);
    fade_plane(&mut v, &chain.fades, true);
    let y8 = dither_plane(&luma, 0);
    let (u8p, v8p) = match chain.layout {
        TenBitLayout::P420 => (dither_plane(&u, chain.slice_rows), dither_plane(&v, chain.slice_rows)),
        TenBitLayout::P422 => (chroma_to_420(&u, 0, chain.kernel)?, chroma_to_420(&v, 3, chain.kernel)?),
    };
    Some([y8, u8p, v8p])
}

/// The planes the encoder is handed for a finished composite (`w` x `h` 8-bit 4:2:0, chroma `w / 2` x `h / 2`): `yuva420p` taken to the
/// delivery's format by the auto-inserted `scale` at the end of the graph. `None` for a ratio swscale needs two scalers for.
pub fn to_ten_bit(planes: [&[u8]; 3], (w, h): (usize, usize), layout: TenBitLayout, kernel: Kernel) -> Option<Planes10> {
    let (cw, ch) = (w / 2, h / 2);
    let shifted = |p: &[u8], pw: usize, ph: usize| Plane10 {
        w: pw,
        h: ph,
        data: p.iter().map(|v| u16::from(*v) << 2).collect(),
    };
    let y = shifted(planes[0], w, h);
    match layout {
        TenBitLayout::P420 => Some(Planes10 {
            layout,
            y,
            u: shifted(planes[1], cw, ch),
            v: shifted(planes[2], cw, ch),
        }),
        TenBitLayout::P422 => {
            let up = ScaleStage {
                src: Rect::whole(cw as u32, ch as u32),
                scaled: (cw as u32, h as u32),
                keep: Rect::whole(cw as u32, h as u32),
            };
            Some(Planes10 {
                layout,
                y,
                u: scale_to_10(planes[1], cw, &up, Scaler::Bicubic, kernel)?,
                v: scale_to_10(planes[2], cw, &up, Scaler::Bicubic, kernel)?,
            })
        }
    }
}

/// The black the graph starts from, as `overlay` is handed it: `color=c=black,format=<delivery>` is **not** 8-bit black taken up. Its luma is `64`,
/// but its chroma is `514` (swscale's RGB to YCbCr rounds black's chroma two above the middle), and the auto-inserted `scale` into `yuva420p` takes
/// that down with the dither: a checkerboard of `128` and `129` (`(514 + dither) >> 2`), by the same conversions as a clip's picture. The planes of
/// a frame where no clip covers the canvas, and what is under a clip that does not fill it.
pub fn base_planes((w, h): (usize, usize), layout: TenBitLayout, slice_rows: u32, kernel: Kernel) -> Option<[Vec<u8>; 3]> {
    let flat = |pw: usize, ph: usize, v: u16| Plane10 {
        w: pw,
        h: ph,
        data: vec![v; pw * ph],
    };
    let (cw, ch) = (w / 2, h / 2);
    let y = dither_plane(&flat(w, h, BASE_LUMA), 0);
    Some(match layout {
        TenBitLayout::P420 => {
            let c = flat(cw, ch, BASE_CHROMA);
            [y, dither_plane(&c, slice_rows), dither_plane(&c, slice_rows)]
        }
        TenBitLayout::P422 => {
            let c = flat(cw, h, BASE_CHROMA);
            [y, chroma_to_420(&c, 0, kernel)?, chroma_to_420(&c, 3, kernel)?]
        }
    })
}

/// The luma and the chroma `color=c=black` writes into a 10-bit picture (measured on both FFmpegs, in either layout).
pub const BASE_LUMA: u16 = 64;
pub const BASE_CHROMA: u16 = 514;

// ---- the probe -----------------------------------------------------------------------------------

/// What the probe found: [`Verdict::Match`] (at least one layout is the model, and [`measured_kinds`] names which), [`Verdict::Differs`] (none
/// is; the detail names the first case) or [`Verdict::Unknown`].
pub type TenBit = Verdict;

const TIMING: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(60),
    per_run: Duration::from_secs(20),
};

static STATE: Probe = Probe::new();
static KINDS: Mutex<TenBitKinds> = Mutex::new(TenBitKinds::NONE);
static DETAILS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Where the model and this FFmpeg's export parted, one line for each layout and way of indexing the dither that was not the model's.
pub fn details() -> Vec<String> {
    DETAILS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// What is known without running anything: the layouts this FFmpeg's 10-bit export was measured to be the model for (and whether its 4:2:0
/// dither restarts at each slice), else none. What the compositor's caps are made from.
pub fn measured_kinds() -> TenBitKinds {
    match measure::known(&STATE) {
        Verdict::Match => *KINDS.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        _ => TenBitKinds::NONE,
    }
}

/// The verdict without running anything.
pub fn measured() -> TenBit {
    measure::known(&STATE)
}

/// The answer, measured on the first call: about forty short `ffmpeg` children (one per toy, layout and thread count, the toy's picture and its
/// probe), one after the other, with no lock held -- a blocking thread's, ahead of the frame that needs it ([`crate::prepare`]). A caller that
/// arrives while it runs waits for it and takes its answer. `Unknown` is not remembered but for a backoff.
pub fn ten_bit() -> TenBit {
    measure::cached(&STATE, Instant::now(), TIMING, probe)
}

/// One toy cut: a noise picture cut into a canvas, with its fades.
struct Toy {
    source: (u32, u32),
    canvas: (u32, u32),
    /// Seconds of fade-in and fade-out (0: none).
    fades: (f64, f64),
    /// A second clip after a gap the length of the first: the frames where nothing covers the canvas are the black the graph starts from.
    gap: bool,
}

/// The cuts: an enlargement, an identity (the format conversion alone), a shrink, a Cover crop (4:3 into 16:9) and two canvases of the heights
/// at which the slices of two and four threads start on an odd chroma row (36 and 52 rows), so that a build that restarts its dither at each
/// slice shows itself.
const TOYS: [Toy; 8] = [
    Toy {
        source: (160, 90),
        canvas: (256, 144),
        fades: (0.1, 0.1),
        gap: false,
    },
    Toy {
        source: (96, 64),
        canvas: (96, 64),
        fades: (0.1, 0.1),
        gap: false,
    },
    Toy {
        source: (320, 180),
        canvas: (160, 90),
        fades: (0.1, 0.0),
        gap: false,
    },
    Toy {
        source: (128, 96),
        canvas: (192, 108),
        fades: (0.0, 0.1),
        gap: false,
    },
    Toy {
        source: (160, 90),
        canvas: (64, 36),
        fades: (0.1, 0.1),
        gap: false,
    },
    Toy {
        source: (180, 104),
        canvas: (90, 52),
        fades: (0.1, 0.1),
        gap: false,
    },
    Toy {
        source: (160, 90),
        canvas: (96, 54),
        fades: (0.1, 0.0),
        gap: true,
    },
    Toy {
        source: (128, 96),
        canvas: (64, 36),
        fades: (0.0, 0.1),
        gap: true,
    },
];

/// Slice thread counts the toys run on (the graph's `-filter_complex_threads`).
const THREADS: [u32; 3] = [1, 2, 4];
const FPS: f64 = 30.0;
const FRAMES: u32 = 10;

/// A scratch directory of the probe's own, removed when it is dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Option<Self> {
        let dir = std::env::temp_dir().join(format!("kerf-delivery10-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Noise in all three planes, from the coordinates (integers, so the same picture on every build).
fn noise_graph((w, h): (u32, u32)) -> String {
    format!(
        "nullsrc=size={w}x{h}:rate={FPS}:duration=1,format=yuv420p,\
         geq=lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17,256)'"
    )
}

fn probe(budget: &Budget) -> Verdict {
    let kernel = match scaler::kernels() {
        Kernels::Match(model) => model.kernel,
        Kernels::Differs => return Verdict::Differs("this FFmpeg's scaler is not the model the 10-bit chain is built on".into()),
        Kernels::Unknown => return Verdict::Unknown,
    };
    let policy = kerf_core::composite_color_policy();
    if policy == kerf_core::CompositeColorPolicy::Unknown {
        return Verdict::Unknown;
    }
    let Some(scratch) = Scratch::new() else {
        return Verdict::Unknown;
    };
    let mut kinds = TenBitKinds::NONE;
    let mut first_difference: Option<String> = None;
    for layout in [TenBitLayout::P420, TenBitLayout::P422] {
        // For each way a build can index the 4:2:0 dither: did every run agree with it?
        let (mut absolute, mut sliced) = (true, true);
        let mut detail: Option<String> = None;
        let mut sliced_detail: Option<String> = None;
        for (n, toy) in TOYS.iter().enumerate() {
            for threads in THREADS {
                match run(budget, &scratch.0, n, toy, layout, threads, kernel, policy) {
                    Ok(found) => {
                        if absolute && found.absolute.is_some() {
                            detail.get_or_insert_with(|| found.absolute.clone().unwrap_or_default());
                            absolute = false;
                        }
                        if sliced && found.sliced.is_some() {
                            sliced_detail.get_or_insert_with(|| found.sliced.clone().unwrap_or_default());
                            sliced = false;
                        }
                    }
                    Err(why) => {
                        tracing::warn!(why, "the 10-bit delivery probe could not tell");
                        return Verdict::Unknown;
                    }
                }
            }
        }
        // (A 4:2:2 conversion has no slice dependence: the sliced model is the absolute one for it.)
        if !absolute {
            let mut details = DETAILS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            details.push(format!(
                "indexed by the picture's rows: {}",
                detail.clone().unwrap_or_default()
            ));
            if !sliced {
                details.push(format!(
                    "indexed by the slice's rows: {}",
                    sliced_detail.clone().unwrap_or_default()
                ));
            }
        }
        if absolute {
            kinds = kinds.union(layout.kind());
        } else if sliced && layout == TenBitLayout::P420 {
            kinds = kinds.union(layout.kind()).union(TenBitKinds::PER_SLICE);
        } else if first_difference.is_none() {
            first_difference = sliced_detail.or(detail);
        }
    }
    if kinds.contains(TenBitKinds::P420) || kinds.contains(TenBitKinds::P422) {
        *KINDS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = kinds;
        Verdict::Match
    } else {
        Verdict::Differs(first_difference.unwrap_or_else(|| "no layout matched".into()))
    }
}

/// What one run of a toy found: how the planes differ from the model that indexes the 4:2:0 dither by the picture's rows, and from the one
/// that restarts it at each slice (`None`: the same).
struct Found {
    absolute: Option<String>,
    sliced: Option<String>,
}

/// The caps the toys are judged by: the 10-bit chain on, and the scaler under it exact.
fn caps() -> kerf_core::GpuCaps {
    let mut caps = kerf_core::GpuCaps::A0;
    caps.motion = true;
    caps.fades = true;
    caps.exact_scaler = true;
    caps.ten_bit = TenBitKinds::P420.union(TenBitKinds::P422);
    caps
}

/// The export's own graph over a toy, every frame as the delivery's planes, held to the model.
#[allow(clippy::too_many_arguments)]
fn run(
    budget: &Budget,
    dir: &Path,
    n: usize,
    toy: &Toy,
    layout: TenBitLayout,
    threads: u32,
    kernel: Kernel,
    policy: kerf_core::CompositeColorPolicy,
) -> Result<Found, String> {
    let file = dir.join(format!("toy{n}.mkv"));
    let path = file.to_str().ok_or("a scratch path that is not text")?;
    if !file.exists() {
        budget
            .ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    &noise_graph(toy.source),
                    "-frames:v",
                    &(FRAMES + 2).to_string(),
                    "-c:v",
                    "ffv1",
                    "-pix_fmt",
                    "yuv420p",
                    path,
                ],
                Vec::new(),
            )
            .ok_or("ffmpeg did not make the toy's picture")?;
    }
    let asset = Project::probe_asset_with(&file, &|| budget.expired()).map_err(|e| format!("the toy's probe: {e}"))?;
    let (sw, sh) = (toy.source.0 as usize, toy.source.1 as usize);
    let planes = budget
        .ffmpeg(
            &[
                "-i",
                path,
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "pipe:1",
            ],
            Vec::new(),
        )
        .ok_or("ffmpeg did not decode the toy's picture")?;
    if planes.len() != sw * sh * 3 / 2 {
        return Err(format!(
            "{} bytes of the toy's picture, not {}",
            planes.len(),
            sw * sh * 3 / 2
        ));
    }
    let mut clip = Clip::new(asset.id, 0.0, f64::from(FRAMES) / FPS, 0.0);
    clip.fade_in = toy.fades.0;
    clip.fade_out = toy.fades.1;
    let mut clips = vec![clip];
    if toy.gap {
        clips.push(Clip::new(asset.id, 0.0, f64::from(FRAMES) / FPS, f64::from(2 * FRAMES) / FPS));
    }
    let total = if toy.gap { 3 * FRAMES } else { FRAMES };
    let tl = Timeline {
        tracks: vec![Track {
            clips,
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(toy.canvas.0, toy.canvas.1, Fit::Cover).with_fps(Some(FPS)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let opts = ExportOptions {
        fps: Some(FPS),
        pix_fmt: Some(layout.pix_fmt().to_string()),
        ..ExportOptions::default()
    };
    let assets = std::slice::from_ref(&asset);
    let graph = kerf_core::export_graph(&tl, assets, &opts)
        .map_err(|e| e.to_string())?
        .ok_or("the toy cut has no graph")?;
    let (cw, ch) = (toy.canvas.0 as usize, toy.canvas.1 as usize);
    let (ccw, cch) = (cw / 2, if layout == TenBitLayout::P422 { ch } else { ch / 2 });
    let frame_len = (cw * ch + 2 * ccw * cch) * 2;
    let frames = total.to_string();
    let thread_arg = threads.to_string();
    let raw = budget
        .ffmpeg(
            &[
                "-i",
                path,
                "-filter_complex_threads",
                &thread_arg,
                "-filter_complex",
                &graph,
                "-map",
                "[outv]",
                "-frames:v",
                &frames,
                "-f",
                "rawvideo",
                "-pix_fmt",
                layout.pix_fmt(),
                "pipe:1",
            ],
            Vec::new(),
        )
        .ok_or("ffmpeg did not export the toy")?;
    if raw.len() != frame_len * total as usize {
        return Err(format!(
            "the export is {} bytes, not {}",
            raw.len(),
            frame_len * total as usize
        ));
    }
    let planner =
        Planner::new(&tl, assets, &opts, PlanRequest::motion(policy).with_media(&OriginalMedia)).map_err(|e| e.to_string())?;
    let mut found = Found {
        absolute: None,
        sliced: None,
    };
    let (yp, rest) = planes.split_at(sw * sh);
    let (up, vp) = rest.split_at(sw * sh / 4);
    for k in 0..u64::from(total) {
        let plan = planner.at_frame(k).map_err(|e| e.to_string())?;
        // The frames the clips' pictures fill. (The plan also keeps, at a clip's edges, candidates for a picture the source may or may not have
        // one more of: that is the decode's to say, and the toy's gap is the frames between its two clips.)
        let covered = !toy.gap || k < u64::from(FRAMES) || k >= u64::from(2 * FRAMES);
        let canvas = (plan.canvas.width, plan.canvas.height);
        // The toy has to be a layer the plan draws from the model: a cut it refuses proves nothing about the chain it stands for.
        if let Some(why) = plan
            .reasons(&caps(), canvas)
            .into_iter()
            .find(|u| matches!(u, kerf_core::Unsupported::DeliveryWith(..)))
        {
            return Err(format!("the plan refuses the toy: {why}"));
        }
        let (stage, fades) = match plan.layers.as_slice() {
            _ if !covered => (whole_stage(sw as u32, sh as u32), Vec::new()),
            [layer] => {
                let geom = plan.layer_geometry(layer, canvas).map_err(|e| e.to_string())?;
                let stage = geom
                    .stages
                    .first()
                    .copied()
                    .unwrap_or_else(|| whole_stage(sw as u32, sh as u32));
                let fades: Vec<u32> = plan
                    .fade_ops(layer)
                    .into_iter()
                    .map(|op| op.factor)
                    .filter(|f| *f < FADE_FULL)
                    .collect();
                (stage, fades)
            }
            other => return Err(format!("{} layers where the toy cut has one", other.len())),
        };
        let want = &raw[k as usize * frame_len..(k as usize + 1) * frame_len];
        for sliced in [false, true] {
            if (if sliced { &found.sliced } else { &found.absolute }).is_some() {
                continue;
            }
            let chain = LayerChain {
                layout,
                stage,
                fades: fades.clone(),
                slice_rows: if sliced {
                    kerf_core::ten_bit_dither_rows(threads, canvas.1)
                } else {
                    0
                },
                scaler: Scaler::Bicubic,
                kernel,
            };
            // (Nothing on the canvas: the black the graph starts from.)
            let layer8 = if covered {
                layer_planes([yp, up, vp], (sw, sh), &chain)
            } else {
                base_planes((cw, ch), layout, chain.slice_rows, kernel)
            };
            let Some(layer8) = layer8 else {
                return Err("the model has no table for the toy".into());
            };
            let Some(out) = to_ten_bit([&layer8[0], &layer8[1], &layer8[2]], (cw, ch), layout, kernel) else {
                return Err("the model has no table for the toy's canvas".into());
            };
            let why = differs(&out, want).map(|d| {
                format!(
                    "{}x{} -> {}x{} {} frame {k} on {threads} threads: {d}",
                    toy.source.0,
                    toy.source.1,
                    toy.canvas.0,
                    toy.canvas.1,
                    layout.pix_fmt()
                )
            });
            if sliced {
                found.sliced = why;
            } else {
                found.absolute = why;
            }
        }
        if found.absolute.is_some() && found.sliced.is_some() {
            break;
        }
    }
    Ok(found)
}

/// How the model's planes differ from FFmpeg's (`want`: Y, U, V as little-endian 16-bit samples), `None` when they do not.
fn differs(out: &Planes10, want: &[u8]) -> Option<String> {
    let mut at = 0;
    for (name, plane) in [("Y", &out.y), ("U", &out.u), ("V", &out.v)] {
        let mut count = 0usize;
        let mut first = None;
        for (i, v) in plane.data.iter().enumerate() {
            let w = u16::from_le_bytes([want[at + 2 * i], want[at + 2 * i + 1]]);
            if w != *v {
                count += 1;
                first.get_or_insert((i % plane.w, i / plane.w, *v, w));
            }
        }
        at += plane.data.len() * 2;
        if let Some((x, y, model, ffmpeg)) = first {
            return Some(format!(
                "{name}: {count} samples differ, the first at ({x}, {y}): the model's {model}, FFmpeg's {ffmpeg}"
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 2x2 pattern the 4:2:0 conversion adds is the 8x8 table's rows and columns shifted down by five, whatever the row: that is why the
    /// luma of a 4:2:2 picture (`(v << 5 + table) >> 7`) and a 4:2:0 picture's planes (`(v + pattern) >> 2`) come out the same.
    #[test]
    fn the_two_by_two_pattern_is_the_eight_by_eight_tables_shifted_down() {
        for row in 0..16 {
            for x in 0..16 {
                let pattern = [[1u32, 2], [3, 0]][row & 1][x & 1];
                assert_eq!(dither(row, x), pattern, "{row} {x}");
                for v in 0..1024u32 {
                    assert_eq!((v + pattern) >> 2, ((v << 5) + DITHER_8X8_128[row & 7][x & 7]) >> 7);
                }
            }
        }
    }

    /// An 8-bit picture that is neither resized nor faded goes through the 10-bit chain unchanged (`v << 2`, then the dither's at most 3).
    #[test]
    fn a_plain_4_2_0_picture_is_the_identity() {
        let (w, h) = (16, 8);
        let planes: Vec<Vec<u8>> = [w * h, w * h / 4, w * h / 4]
            .iter()
            .enumerate()
            .map(|(p, n)| (0..*n).map(|i| (i * 7 + p * 31) as u8).collect())
            .collect();
        for slice_rows in [0, 3] {
            let chain = LayerChain {
                layout: TenBitLayout::P420,
                stage: whole_stage(w as u32, h as u32),
                fades: Vec::new(),
                slice_rows,
                scaler: Scaler::Bicubic,
                kernel: Kernel::X86,
            };
            let out = layer_planes([&planes[0], &planes[1], &planes[2]], (w, h), &chain).expect("tables");
            assert_eq!(out.to_vec(), planes);
        }
    }

    /// A fade at a factor of 65535 - 1 or less never raises a sample, and one at the black level's own factor leaves the black level alone.
    #[test]
    fn a_fade_moves_toward_black() {
        for factor in [0, 1, 5461, 32768, 60069, 65534] {
            for v in 0..1024u16 {
                assert!(fade_sample(v, factor, false) <= v.max(64), "{v} {factor}");
            }
            assert_eq!(fade_sample(64, factor, false), 64);
            assert_eq!(fade_sample(512, factor, true), 512);
        }
        assert_eq!(fade_sample(940, 0, false), 64);
        assert_eq!(fade_sample(1023, 0, true), 512);
    }

    /// The 4:2:2 chroma conversion of a flat plane stays within the dither: a level in 10 bits is a quarter of a level in 8, and the pattern adds
    /// less than one, so every sample is within one of that level, whatever the kernel.
    #[test]
    fn a_flat_422_chroma_plane_stays_flat() {
        for level in [0u16, 64, 512, 1020, 1023] {
            let plane = Plane10 {
                w: 16,
                h: 12,
                data: vec![level; 16 * 12],
            };
            for kernel in [Kernel::C, Kernel::X86] {
                for offset in [0, 3] {
                    let out = chroma_to_420(&plane, offset, kernel).expect("table");
                    // (x86's kernel truncates each tap's product, so a sample can land one below.)
                    let low = u32::from(level >> 2);
                    assert!(
                        out.iter()
                            .all(|v| (low.saturating_sub(1)..=(low + 1).min(255)).contains(&u32::from(*v))),
                        "{level} {kernel:?} {offset}: {out:?}"
                    );
                }
            }
        }
    }

    /// The memory of the probe is the shared one's ([`crate::measure`]).
    #[test]
    fn the_probe_is_bounded_like_its_siblings() {
        assert_eq!(TIMING.backoff, Duration::from_secs(30));
        assert!(TIMING.per_run < TIMING.overall);
    }

    /// Every toy has even sides and a slice layout that tells the dither's two ways apart for some thread count.
    #[test]
    fn the_toys_are_even_and_one_of_them_flips_the_dither_at_each_slice() {
        for toy in TOYS {
            for side in [toy.source.0, toy.source.1, toy.canvas.0, toy.canvas.1] {
                assert_eq!(side % 2, 0, "{:?}", toy.source);
            }
        }
        let flips = |threads: u32| TOYS.iter().any(|t| kerf_core::ten_bit_dither_rows(threads, t.canvas.1) != 0);
        assert!(flips(2) && flips(4));
        assert!(!TOYS.iter().any(|t| kerf_core::ten_bit_dither_rows(1, t.canvas.1) != 0));
    }
}
