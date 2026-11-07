//! A `yuv420p` picture taken from one YCbCr matrix to another the way swscale does it, as arithmetic: the scalar
//! reference of the compositor's conversion passes, and the probe that says whether this FFmpeg converts as it does.
//!
//! FFmpeg 9 negotiates the colourspace along the overlay chain (`CompositeColorPolicy::BottomLayerTag`): the first
//! clip's tag is the composite's and an input tagged otherwise gets an auto-inserted `scale` between the clip's chain
//! (its last `format`) and `overlay`. swscale has no YCbCr-to-YCbCr matrix change, so `sws_setColorspaceDetails` builds
//! two contexts and goes **through RGB** ("YUV color matrix differs for YUV->YUV, using intermediate RGB to convert",
//! `utils.c`), unscaled here (the chain has made the picture the size it is overlaid at):
//!
//! * `yuv420p -> bgr24` with the *source's* coefficients: the x86 SSSE3 kernel the grade's way in is
//!   ([`crate::grade::X86Kernel`], held to FFmpeg there);
//! * then `bgr24 -> yuv420p` with the *destination's*, **not** by the generic scaler the grade's way out is: an
//!   unscaled `bgr24 -> yuv420p` with an even width has a converter of its own, `bgr24ToYv12Wrapper` ->
//!   `ff_rgb24toyv12` (`rgb2rgb`), which is C on every row on the FFmpeg 9 pinned here, and on an x86 build with the
//!   inline MMX one (`rgb24toyv12_mmxext`) the first two rows and the last two go through C and every row between
//!   through different arithmetic (two truncations in luma, `pavgb` roundings in chroma):
//!   * C: `Y = ((ry*r + gy*g + by*b) >> 15) + 16`; chroma from the 2x2 block's channel means taken with `>> 2`
//!     (`U = ((ru*rx + gu*gx + bu*bx) >> 15) + 128`).
//!
//! **Where in the chain.** The conversion sits ahead of the chain's last `format`, on the picture the earlier filters
//! (the fit `scale`, `pad`) made: a resized layer is scaled **then** converted (measured: 0 of 10240 luma samples differ
//! with the order the other way round at 9931). A still's graph has no `format`, and its last `scale` converts the matrix in
//! the same pass as its resize, which is not this: it is swscale's cascade at the smaller of the two sizes
//! ([`scaled_to_rgb24`], [`scale_and_change_matrix`], probed by [`scaled`]). **Whose matrix is the composite's** is the chain's, not the frame's: the
//! first clip on an alpha plane (a clip entering through a dissolve, a keyed zoom or position, or a source with an alpha channel: nothing in its chain forces a converter), else the first clip, and every clip
//! of another matrix is converted into it once, wherever on the timeline ([`kerf_core::PlanCanvas::cut_matrix`], measured
//! with three clips in every combination of plain and dissolving). A **dissolving** clip's chain ends in `format=yuva420p`
//! and its last `scale` converts the matrix itself in the same pass as its resize, as in a still: where that scale does not
//! resize the picture it is the unscaled cascade (measured), and where it shrinks it the scaled one, `bgr24` at the
//! destination's size.
//!
//! The coefficients are `Rgb2Yuv`'s ([`crate::roundtrip`]): `fill_rgb2yuv_table` of the destination's matrix.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use kerf_core::YuvMatrix;

use crate::grade::{yuv420_to_rgb, X86Kernel};
use crate::roundtrip::{Rgb2Yuv, Yuv2Rgb};
use crate::sws::{self, Scaler};

/// Which `ff_rgb24toyv12` a build runs: the C one on every row, or the x86 `rgb24toyv12_mmxext`, which does the
/// first two rows and the last two itself in C and every row between in MMX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    C,
    Mmx,
}

/// `ff_rgb24toyv12_c` and `rgb24toyv12_mmxext` on one `w` x `h` RGB picture (`rgb` is `r, g, b` per pixel, which is
/// `bgr24`'s bytes read the other way round): the planes of a `yuv420p`. Even sides only (the converter's own
/// condition; the plan refuses the rest).
pub fn rgb_to_yuv420_24(rgb: &[u8], w: usize, h: usize, matrix: YuvMatrix, variant: Variant) -> [Vec<u8>; 3] {
    let k = Rgb2Yuv::new(matrix);
    let px = |x: usize, y: usize| -> [i32; 3] { std::array::from_fn(|c| i32::from(rgb[(y * w + x) * 3 + c])) };
    let (cw, ch) = (w / 2, h / 2);
    let mut luma = vec![0u8; w * h];
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    for y in 0..h {
        // The first two rows and the last two go through the C function; the rows between, the MMX loop.
        let c_rows = variant == Variant::C || y < 2 || y + 2 >= h;
        for x in 0..w {
            let [r, g, b] = px(x, y);
            luma[y * w + x] = if c_rows {
                (((k.ry * r + k.gy * g + k.by * b) >> 15) + 16) as u8
            } else {
                let t = ((b * k.by + g * k.gy) >> 8) + ((r * k.ry) >> 8);
                // (`packssdw` to 16 bits, `psraw 7`, `packuswb`, `paddusb 16`.)
                ((t >> 7).clamp(0, 255) + 16).min(255) as u8
            };
        }
    }
    for cy in 0..ch {
        let c_row = variant == Variant::C || cy == 0 || cy + 1 == ch;
        for cx in 0..cw {
            let p = [
                px(2 * cx, 2 * cy),
                px(2 * cx + 1, 2 * cy),
                px(2 * cx, 2 * cy + 1),
                px(2 * cx + 1, 2 * cy + 1),
            ];
            let [rx, gx, bx] = if c_row {
                // `(b11 + b12 + b21 + b22) >> 2`.
                std::array::from_fn(|c| (p[0][c] + p[1][c] + p[2][c] + p[3][c]) >> 2)
            } else {
                // `pavgb` of the two rows, then of the two columns: each rounds up.
                std::array::from_fn(|c| {
                    let top = (p[0][c] + p[2][c] + 1) >> 1;
                    let right = (p[1][c] + p[3][c] + 1) >> 1;
                    (top + right + 1) >> 1
                })
            };
            let chroma = |kr: i32, kg: i32, kb: i32| -> u8 {
                if c_row {
                    // (An `unsigned int` in the C: the low byte of the sum, which the table keeps inside 16..240.)
                    (((kr * rx + kg * gx + kb * bx) >> 15) + 128) as u8
                } else {
                    let t = ((bx * kb + gx * kg) >> 8) + ((rx * kr) >> 8);
                    ((t >> 7).clamp(-128, 127) + 128) as u8
                }
            };
            u[cy * cw + cx] = chroma(k.ru, k.gu, k.bu);
            v[cy * cw + cx] = chroma(k.rv, k.gv, k.bv);
        }
    }
    [luma, u, v]
}

/// The whole change of matrix of an opaque picture: `planes` (`w` x `h`, 4:2:0, even sides) read as `from`, through the
/// x86 `yuv420p -> bgr24` kernel and `bgr24 -> yuv420p` with `to`'s coefficients.
pub fn change_matrix(planes: [&[u8]; 3], w: usize, h: usize, from: YuvMatrix, to: YuvMatrix, variant: Variant) -> [Vec<u8>; 3] {
    rgb_to_yuv420_24(&yuv420_to_rgb(planes, w, h, from), w, h, to, variant)
}

// ---- the scaled conversion -----------------------------------------------------------------------

/// How the vertical pass of a scale that ends in `bgr24` is run for a row: the x86 inline MMX `yuv2bgr24_X`
/// (`YSCALEYUV2RGBX`: `pmulhw` of the 15-bit intermediates and the 12-bit weights, the chroma and luma terms on 16-bit lanes) on every
/// row but the last two, the C `yuv2rgb_X_c_template` (a 19-bit sum, swscale's tables) on those, which `swscale.c` hands back
/// to C (`dstY >= dstH - 2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packed {
    /// The C function on every row.
    C,
    /// MMX on the rows before the last two.
    Mmx,
}

/// The planes of a picture and how large it is.
#[derive(Debug, Clone, Copy)]
pub struct Pic<'a> {
    pub planes: [&'a [u8]; 3],
    pub size: (usize, usize),
}

/// What a `yuv420p` picture of `src` size turns into when swscale scales it to `dst` and converts it to `bgr24` in the one
/// context (`sws_scale` with a `bgr24` destination): `Some(rgb)`, `r, g, b` per pixel, or `None` for a size the model does not
/// follow (an odd side; a luma vertical filter of one or two taps, which swscale hands to other output functions; a ratio
/// beyond one pass). A chroma filter of one tap (a 2:1 shrink of the rows) is the general output function's too.
///
/// * Horizontal: the luma `src.0 -> dst.0` and the chroma `src.0 / 2 -> dst.0 / 2` (the destination RGB has no chroma
///   of its own; one sample serves two pixels), 15 bits, bicubic tables of an x86 build (filter sizes rounded up to 4).
/// * Vertical: the luma `src.1 -> dst.1` and the chroma **`src.1 / 2 -> dst.1`** (an RGB destination has a chroma row for
///   every row), tables rounded up to 2 taps.
/// * Out: `kernel` per row ([`Packed`]).
pub fn scaled_to_rgb24(pic: Pic, dst: (usize, usize), matrix: YuvMatrix, kernel: Packed) -> Option<Vec<u8>> {
    let (sw, sh) = pic.size;
    let (dw, dh) = dst;
    if sw % 2 != 0 || sh % 2 != 0 || dw % 2 != 0 || dh % 2 != 0 || dw == 0 || dh == 0 {
        return None;
    }
    let (scw, sch, dcw) = (sw / 2, sh / 2, dw / 2);
    let [py, pu, pv] = pic.planes;
    if py.len() != sw * sh || pu.len() != scw * sch || pv.len() != scw * sch {
        return None;
    }
    let (ah, av) = (sws::SIMD_ALIGN_HORIZONTAL, sws::SIMD_ALIGN_VERTICAL);
    let table =
        |src: usize, dst: usize, one: i32, align: usize| sws::table_aligned(Scaler::Bicubic, src as u32, dst as u32, one, align);
    let hl = table(sw, dw, sws::ONE_HORIZONTAL, ah)?;
    let hc = table(scw, dcw, sws::ONE_HORIZONTAL, ah)?;
    let vl = table(sh, dh, sws::ONE_VERTICAL, av)?;
    let vc = table(sch, dh, sws::ONE_VERTICAL, av)?;
    if vl.size <= 2 {
        return None;
    }
    // The horizontal pass, per row of the source: 15 bits, clipped at the top.
    let hscale = |plane: &[u8], pw: usize, rows: usize, out_w: usize, f: &sws::Filter| -> Vec<i32> {
        let mut mid = vec![0i32; out_w * rows];
        for y in 0..rows {
            for x in 0..out_w {
                let acc: i32 = (0..f.size)
                    .map(|j| {
                        let col = (f.pos[x] as usize + j).min(pw - 1);
                        f.coeff[x * f.size + j] * i32::from(plane[y * pw + col])
                    })
                    .sum();
                mid[y * out_w + x] = (acc >> 7).min(32767);
            }
        }
        mid
    };
    let my = hscale(py, sw, sh, dw, &hl);
    let mu = hscale(pu, scw, sch, dcw, &hc);
    let mv = hscale(pv, scw, sch, dcw, &hc);
    let mmx = X86Kernel::new(matrix);
    let c = Yuv2Rgb::new(matrix);
    let mut out = Vec::with_capacity(dw * dh * 3);
    for i in 0..dh {
        let simd = kernel == Packed::Mmx && i + 2 < dh;
        for x in 0..dw {
            let lum =
                |_: ()| (0..vl.size).map(|j| (vl.coeff[i * vl.size + j], my[(vl.pos[i] as usize + j).min(sh - 1) * dw + x]));
            let chr = |plane: &[i32]| {
                (0..vc.size)
                    .map(|j| {
                        (
                            vc.coeff[i * vc.size + j],
                            plane[(vc.pos[i] as usize + j).min(sch - 1) * dcw + x / 2],
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let (u_taps, v_taps) = (chr(&mu), chr(&mv));
            if simd {
                // (`vRounder` is 4 on every lane.)
                let acc = |taps: &mut dyn Iterator<Item = (i32, i32)>| 4 + taps.map(|(c, s)| (c * s) >> 16).sum::<i32>();
                let y8 = acc(&mut lum(()));
                let u8_ = acc(&mut u_taps.iter().copied());
                let v8 = acc(&mut v_taps.iter().copied());
                out.extend(mmx.rgb_lanes(y8, u8_, v8));
            } else {
                let acc = |taps: &mut dyn Iterator<Item = (i32, i32)>| {
                    (1i32 << 18).wrapping_add(taps.map(|(c, s)| c.wrapping_mul(s)).fold(0i32, i32::wrapping_add))
                };
                let y = acc(&mut lum(())) >> 19;
                let u = acc(&mut u_taps.iter().copied()) >> 19;
                let v = acc(&mut v_taps.iter().copied()) >> 19;
                out.extend(c.rgb_unclipped(y, u, v).map(|c| c as u8));
            }
        }
    }
    Some(out)
}

/// The whole change of matrix of a `yuv420p` picture swscale scales to `dst` in the same `scale`: **through the cascade**
/// ([`scaled_to_rgb24`], then `ff_rgb24toyv12` on the `bgr24` intermediate at the destination's size), which is what
/// a destination smaller than the source gives (`sws_setColorspaceDetails` makes the intermediate the smaller of the two).
pub fn scale_and_change_matrix(
    pic: Pic,
    dst: (usize, usize),
    from: YuvMatrix,
    to: YuvMatrix,
    kernel: Packed,
) -> Option<[Vec<u8>; 3]> {
    if pic.size.0 * pic.size.1 <= dst.0 * dst.1 {
        return None;
    }
    let rgb = scaled_to_rgb24(pic, dst, from, kernel)?;
    Some(rgb_to_yuv420_24(&rgb, dst.0, dst.1, to, Variant::C))
}

// ---- the probe -----------------------------------------------------------------------------------

/// Whether this FFmpeg converts a layer into the composite's matrix as [`change_matrix`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernels {
    /// A picture through the overlay chain's conversion came back as the model computes it.
    Match,
    /// It did not: the chain that differs.
    Differs(&'static str),
    /// Not known: not measured yet, or FFmpeg would not run or timed out (asked again after a backoff).
    Unknown,
}

impl Kernels {
    /// The compositor converts a layer into the composite's matrix on this machine.
    pub fn converts(self) -> bool {
        self == Self::Match
    }
}

/// The probe picture: tall enough for the MMX loop's rows (it leaves the first two and the last two to C).
const PROBE: (usize, usize) = (256, 16);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

fn tag(matrix: &str) -> &'static str {
    match matrix {
        "bt709" => ",setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709",
        "bt2020nc" => ",setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt2020-10",
        _ => "",
    }
}

/// The probe picture: a polynomial of the coordinates, mod 256, in all three planes (not `random()`, which restarts
/// for every slice thread), `matrix` as its tag.
fn source(matrix: &str) -> String {
    source_sized(matrix, PROBE)
}

/// [`source`] at another size.
fn source_sized(matrix: &str, (w, h): (usize, usize)) -> String {
    format!(
        "nullsrc=s={w}x{h}:r=1:d=1,format=yuv420p,\
         geq=lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17,256)'{}",
        tag(matrix)
    )
}

/// The overlay chain of an export with two clips: a `bottom` picture, and above it a `top` one that covers it, tagged
/// otherwise; the frame is the top picture as FFmpeg takes it into the bottom's matrix.
fn overlay_graph(bottom: &str, top: &str) -> String {
    let (w, h) = PROBE;
    format!(
        "color=c=black:s={w}x{h}:r=1:d=1,format=yuv420p[vbase];{},format=yuv420p[v0];[vbase][v0]overlay[o0];{},format=yuv420p[v1];[o0][v1]overlay",
        source(bottom),
        source(top)
    )
}

/// The overlay chain of an export with **three clips on one track**, laid side by side on one canvas so that every clip's
/// picture is seen: a plain BT.709 clip, then a clip whose chain ends in `yuva420p` (what a dissolve's chain ends in) with no
/// tag, then a plain BT.709 clip again. The inputs are **files** (`[0:v]` and `[2:v]` the tagged one, `[1:v]` the untagged
/// one): a tag set in the graph by `setparams` is another constraint in the negotiation and moves who decides (measured: the
/// first clip does, then). The composite's matrix is the tag of the first clip on an alpha plane (the middle one: nothing in
/// its chain forces a converter), and the plain clips are converted into it ([`kerf_core::PlanCanvas::cut_matrix`] is that
/// rule: `Planner` reads it off the graph's shape, and this is the check that this FFmpeg negotiates it so).
fn decider_graph() -> String {
    let (w, h) = PROBE;
    // The export's identity fit of a picture that fills the frame.
    let fit = format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1");
    format!(
        "color=c=black:s={}x{h}:r=1:d=1,format=yuv420p[vbase];[0:v]{fit},fps=1,format=yuv420p[v0];[vbase][v0]overlay=x=0:y=0[o0];\
         [1:v]{fit},fps=1,format=yuva420p[v1];[o0][v1]overlay=x={w}:y=0[o1];[2:v]{fit},fps=1,format=yuv420p[v2];[o1][v2]overlay=x={}:y=0",
        3 * w,
        2 * w
    )
}

/// A scratch directory under the temp dir, removed when dropped.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Option<Self> {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kerf-gpu-convert-probe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The probe picture as a lossless one-frame file (FFV1 in Matroska) carrying `matrix` as its tag: what the export's inputs are.
fn make_file(path: &std::path::Path, matrix: &str) -> Option<()> {
    make_file_sized(path, matrix, PROBE)
}

/// [`make_file`] at another size.
fn make_file_sized(path: &std::path::Path, matrix: &str, size: (usize, usize)) -> Option<()> {
    let mut args = args_for(&[], &source_sized(matrix, size));
    // (`args` writes a rawvideo pipe; a file is the same graph into an encoder.)
    args.truncate(args.iter().position(|a| a == "-map")?);
    args.extend(["-map", "[o]", "-frames:v", "1", "-c:v", "ffv1", "-pix_fmt", "yuv420p", "-y"].map(String::from));
    args.push(path.to_string_lossy().into_owned());
    kerf_core::limit_frame_args(&mut args, 2);
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(&args);
    crate::source::run_child(cmd, 1 << 16, PROBE_TIMEOUT).ok().map(drop)
}

/// Tile `k` (of three side by side) of a `yuv420p` frame of `3 * PROBE.0` x `PROBE.1`, as the planes of a `PROBE` picture.
fn tile(frame: &[u8], k: usize) -> Vec<u8> {
    let (w, h) = PROBE;
    let (y, rest) = frame.split_at(3 * w * h);
    let (u, v) = rest.split_at(3 * w * h / 4);
    let rows = |plane: &[u8], pw: usize, rows: usize| -> Vec<u8> {
        (0..rows)
            .flat_map(|r| plane[r * 3 * pw + k * pw..r * 3 * pw + (k + 1) * pw].iter().copied())
            .collect()
    };
    let mut out = rows(y, w, h);
    out.extend(rows(u, w / 2, h / 2));
    out.extend(rows(v, w / 2, h / 2));
    out
}

fn args(graph: &str) -> Vec<String> {
    args_for(&[], graph)
}

/// [`args`] over input `files`.
fn args_for(files: &[std::path::PathBuf], graph: &str) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"]
        .into_iter()
        .map(String::from)
        .collect();
    for f in files {
        a.extend(["-i".to_string(), f.to_string_lossy().into_owned()]);
    }
    a.push("-filter_complex".to_string());
    a.push(format!("{graph}[o]"));
    a.extend(
        [
            "-map",
            "[o]",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "pipe:1",
        ]
        .map(String::from),
    );
    a
}

fn planes_of(frame: &[u8]) -> [&[u8]; 3] {
    let (w, h) = PROBE;
    let (y, rest) = frame.split_at(w * h);
    let (u, v) = rest.split_at(w * h / 4);
    [y, u, v]
}

fn measure() -> Option<Kernels> {
    let (w, h) = PROBE;
    let yuv = w * h * 3 / 2;
    // The cheap round first, three short children at once: the source, the picture's conversion into the bottom layer's matrix
    // (two pairs of matrices). A machine that converts another way settles here, `Differs` for good, without the files below.
    // (graph, the length of its frame)
    let graphs = [
        (source("none"), yuv),
        (overlay_graph("bt709", "none"), yuv),
        (overlay_graph("bt2020nc", "bt709"), yuv),
    ];
    let share = graphs.len();
    // The graphs are `-filter_complex` ones: that is how the product writes them, and where the negotiation happens.
    let frames: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
        let runs: Vec<_> = graphs
            .iter()
            .map(|(graph, len)| {
                scope.spawn(move || {
                    let mut args = args(graph);
                    kerf_core::limit_frame_args(&mut args, share);
                    let mut cmd = kerf_core::ffmpeg_command();
                    cmd.args(&args);
                    crate::source::run_child(cmd, len + 1, PROBE_TIMEOUT).ok()
                })
            })
            .collect();
        runs.into_iter().map(|run| run.join().ok().flatten()).collect()
    });
    let [Some(source), Some(opaque), Some(opaque_2020)] = &frames[..] else {
        return None;
    };
    if source.len() != yuv {
        return None;
    }
    let src = planes_of(source);
    let want = |from, to, variant| change_matrix(src, w, h, from, to, variant).concat();
    // The overlay chain converts the top picture into the bottom's matrix.
    if *opaque != want(YuvMatrix::Bt601, YuvMatrix::Bt709, Variant::C)
        || *opaque_2020 != want(YuvMatrix::Bt709, YuvMatrix::Bt2020, Variant::C)
    {
        return Some(if *opaque == want(YuvMatrix::Bt601, YuvMatrix::Bt709, Variant::Mmx) {
            Kernels::Differs("an opaque picture goes through the MMX rgb24toyv12")
        } else {
            Kernels::Differs("an opaque picture's conversion into the bottom layer's matrix")
        });
    }
    // Only a machine that converts as modelled is asked who decides: two tagged files (what an export's inputs are), then the
    // three-clip graph over them. A failure here is `Unknown` (asked again after a backoff), not a verdict.
    let scratch = Scratch::new()?;
    let (tagged, untagged) = (scratch.0.join("tagged.mkv"), scratch.0.join("untagged.mkv"));
    let made: [Option<()>; 2] = std::thread::scope(|scope| {
        [(&tagged, "bt709"), (&untagged, "none")]
            .map(|(path, matrix)| scope.spawn(move || make_file(path, matrix)))
            .map(|m| m.join().ok().flatten())
    });
    if made.iter().any(Option::is_none) {
        return None;
    }
    let three = {
        let mut args = args_for(&[tagged.clone(), untagged, tagged], &decider_graph());
        kerf_core::limit_frame_args(&mut args, 1);
        let mut cmd = kerf_core::ffmpeg_command();
        cmd.args(&args);
        crate::source::run_child(cmd, 3 * yuv + 1, PROBE_TIMEOUT).ok()?
    };
    if three.len() != 3 * yuv {
        return None;
    }
    // Whose matrix is the composite's, in the shape the planner's decider reads: the first clip on an alpha plane (the middle
    // one, untagged = BT.601) keeps its picture, and the two plain BT.709 clips are converted into BT.601.
    let into_601 = want(YuvMatrix::Bt709, YuvMatrix::Bt601, Variant::C);
    if tile(&three, 0) != into_601 || tile(&three, 1) != *source || tile(&three, 2) != into_601 {
        return Some(Kernels::Differs(
            "the composite's matrix is not the first clip on an alpha plane's, as the planner's decider reads the graph",
        ));
    }
    Some(Kernels::Match)
}

// ---- the scaled conversion's probe ---------------------------------------------------------------

/// Whether this FFmpeg's `scale`, when it resizes a picture and takes it from one matrix to another in the one call, runs the
/// cascade [`scale_and_change_matrix`] follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scaled {
    /// The probe's scaled pictures, through the export's own chains, came back as the model computes them.
    Match,
    /// They did not: what differs.
    Differs(&'static str),
    /// Not known: not measured yet, or FFmpeg would not run or timed out (asked again after a backoff).
    Unknown,
}

impl Scaled {
    /// The compositor draws a layer whose last scale resizes it and converts its matrix on this machine.
    pub fn converts(self) -> bool {
        self == Self::Match
    }
}

/// The canvas of the scaled probe's graphs: two tiles side by side, the first the clip that decides the composite's matrix, the
/// second the clip whose fit `scale` resizes it and converts its matrix.
const CANVAS: (usize, usize) = (64, 48);

/// The scaled probe's pictures, by the clip that is resized: `(size of the file, the picture the fit scale makes of it)`. A
/// square, which a letterbox fits, and a tall one.
const RESIZED: [((usize, usize), (usize, usize)); 2] = [((160, 160), (48, 48)), ((96, 192), (24, 48))];

/// The export's chains for a decider and a resized clip over the canvas: the chains of [`decider_graph`], `plane` (the clips are
/// on the alpha plane: `fps`, `format=yuva420p`, a dissolving clip's) or not (a still's: the overlay asks for `yuva420p` itself).
fn scaled_graph(plane: bool) -> String {
    let (w, h) = CANVAS;
    let fit = |w, h| format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1");
    let tail = if plane { ",fps=1,format=yuva420p" } else { "" };
    format!(
        "color=c=black:s={}x{h}:r=1:d=1,format=yuv420p[vbase];[0:v]{}{tail}[v0];[vbase][v0]overlay=x=0:y=0[o0];[1:v]{}{tail}[v1];[o0][v1]overlay=x={w}:y=0",
        2 * w,
        fit(w, h),
        fit(w, h)
    )
}

/// Frame `k` (of two side by side, `CANVAS` each) of a `yuv420p` picture, cut to the window at `(x, y)` of size `(w, h)`, as planes.
fn window(frame: &[u8], k: usize, (x, y): (usize, usize), (w, h): (usize, usize)) -> [Vec<u8>; 3] {
    let (cw, ch) = CANVAS;
    let (fw, fh) = (2 * cw, ch);
    let (luma, rest) = frame.split_at(fw * fh);
    let (u, v) = rest.split_at(fw * fh / 4);
    let cut = |plane: &[u8], pw: usize, x0: usize, y0: usize, w: usize, h: usize| -> Vec<u8> {
        (y0..y0 + h)
            .flat_map(|r| plane[r * pw + x0..r * pw + x0 + w].iter().copied())
            .collect()
    };
    let x0 = k * cw + x;
    [
        cut(luma, fw, x0, y, w, h),
        cut(u, fw / 2, x0 / 2, y / 2, w / 2, h / 2),
        cut(v, fw / 2, x0 / 2, y / 2, w / 2, h / 2),
    ]
}

/// One of the scaled probe's runs: the file of the clip that decides, the file that is resized, and which graph.
struct ScaledRun {
    decider: std::path::PathBuf,
    resized: std::path::PathBuf,
    /// The matrix of the decider's tag and the resized clip's (what `want` converts between).
    from: YuvMatrix,
    to: YuvMatrix,
    shape: usize,
    plane: bool,
}

fn measure_scaled() -> Option<Scaled> {
    // The cascade is the conversion's second half; a build that does not convert as modelled has nothing to follow.
    match unscaled() {
        Kernels::Match => {}
        Kernels::Unknown => return None,
        Kernels::Differs(_) => {
            return Some(Scaled::Differs(
                "this FFmpeg's conversion between matrices is not the model's",
            ))
        }
    }
    let scratch = Scratch::new()?;
    // (name, tag, size): the deciders at the canvas' size, the resized clips at the files' sizes of `RESIZED`.
    let big = |shape: usize, name: &str, tag: &str| {
        (
            scratch.0.join(format!("{name}-{shape}.mkv")),
            tag.to_string(),
            RESIZED[shape].0,
        )
    };
    let small = |name: &str, tag: &str| (scratch.0.join(format!("{name}-small.mkv")), tag.to_string(), CANVAS);
    let files = [
        small("d709", "bt709"),
        small("d601", "none"),
        big(0, "r601", "none"),
        big(0, "r709", "bt709"),
        big(0, "r2020", "bt2020nc"),
        big(1, "r601", "none"),
    ];
    let made: Vec<Option<()>> = std::thread::scope(|scope| {
        files
            .iter()
            .map(|(path, tag, size)| scope.spawn(move || make_file_sized(path, tag, *size)))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|m| m.join().ok().flatten())
            .collect()
    });
    if made.iter().any(Option::is_none) {
        return None;
    }
    let path = |i: usize| files[i].0.clone();
    let runs = [
        // The chains of a dissolving clip (an alpha plane), each direction and BT.2020, and the tall picture.
        ScaledRun {
            decider: path(0),
            resized: path(2),
            from: YuvMatrix::Bt601,
            to: YuvMatrix::Bt709,
            shape: 0,
            plane: true,
        },
        ScaledRun {
            decider: path(1),
            resized: path(3),
            from: YuvMatrix::Bt709,
            to: YuvMatrix::Bt601,
            shape: 0,
            plane: true,
        },
        ScaledRun {
            decider: path(0),
            resized: path(4),
            from: YuvMatrix::Bt2020,
            to: YuvMatrix::Bt709,
            shape: 0,
            plane: true,
        },
        ScaledRun {
            decider: path(0),
            resized: path(5),
            from: YuvMatrix::Bt601,
            to: YuvMatrix::Bt709,
            shape: 1,
            plane: true,
        },
        // ... and the still's, where the overlay itself asks for `yuva420p`.
        ScaledRun {
            decider: path(0),
            resized: path(2),
            from: YuvMatrix::Bt601,
            to: YuvMatrix::Bt709,
            shape: 0,
            plane: false,
        },
        ScaledRun {
            decider: path(1),
            resized: path(3),
            from: YuvMatrix::Bt709,
            to: YuvMatrix::Bt601,
            shape: 0,
            plane: false,
        },
    ];
    // The reference pictures: the planes of a resized clip's source.
    let sources: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
        let jobs: Vec<_> = [("none", 0), ("bt709", 0), ("bt2020nc", 0), ("none", 1)]
            .into_iter()
            .map(|(tag, shape)| {
                scope.spawn(move || {
                    let size = RESIZED[shape].0;
                    let mut args = args(&source_sized(tag, size));
                    kerf_core::limit_frame_args(&mut args, 1);
                    let mut cmd = kerf_core::ffmpeg_command();
                    cmd.args(&args);
                    crate::source::run_child(cmd, size.0 * size.1 * 3 / 2 + 1, PROBE_TIMEOUT).ok()
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().ok().flatten()).collect()
    });
    let frames: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
        let jobs: Vec<_> = runs
            .iter()
            .map(|run| {
                scope.spawn(move || {
                    let mut args = args_for(&[run.decider.clone(), run.resized.clone()], &scaled_graph(run.plane));
                    kerf_core::limit_frame_args(&mut args, 2);
                    let mut cmd = kerf_core::ffmpeg_command();
                    cmd.args(&args);
                    let (w, h) = CANVAS;
                    crate::source::run_child(cmd, 2 * w * h * 3 / 2 + 1, PROBE_TIMEOUT).ok()
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().ok().flatten()).collect()
    });
    let (cw, ch) = CANVAS;
    for (run, frame) in runs.iter().zip(&frames) {
        let frame = frame.as_ref()?;
        if frame.len() != 2 * cw * ch * 3 / 2 {
            return None;
        }
        let (size, picture) = RESIZED[run.shape];
        let source = match (run.from, run.shape) {
            (YuvMatrix::Bt601, 0) => &sources[0],
            (YuvMatrix::Bt709, 0) => &sources[1],
            (YuvMatrix::Bt2020, 0) => &sources[2],
            _ => &sources[3],
        }
        .as_ref()?;
        let (y, rest) = source.split_at(size.0 * size.1);
        let (u, v) = rest.split_at(size.0 * size.1 / 4);
        let pic = Pic { planes: [y, u, v], size };
        let at = ((cw - picture.0) / 2, (ch - picture.1) / 2);
        let got = window(frame, 1, at, picture);
        let Some(want) = scale_and_change_matrix(pic, picture, run.from, run.to, Packed::Mmx) else {
            return Some(Scaled::Differs("the probe's own picture is one the model does not follow"));
        };
        if got != want {
            let c_only = scale_and_change_matrix(pic, picture, run.from, run.to, Packed::C);
            return Some(Scaled::Differs(if c_only.as_ref() == Some(&got) {
                "a resized picture is converted with the C output functions on every row"
            } else if run.plane {
                "a dissolving clip's resized picture is not the cascade the model computes"
            } else {
                "a still's resized picture is not the cascade the model computes"
            }));
        }
    }
    Some(Scaled::Match)
}

static SCALED: Mutex<State<Scaled>> = Mutex::new(State::new());

/// Whether this FFmpeg's `scale` that resizes a picture and converts its matrix in the one call is the cascade
/// [`scale_and_change_matrix`] follows. Runs the probe on its first call (the conversion's first; then the export's
/// own chains over a decider and a clip that is resized, a few `ffmpeg` children bounded by 20 s each, with no lock held): a
/// blocking thread's, ahead of the frame that needs it ([`crate::prepare`]). [`Scaled::Unknown`] is not remembered: the probe is run
/// again after a backoff.
pub fn scaled() -> Scaled {
    cached(&SCALED, Instant::now(), measure_scaled)
}

/// What is known without running anything: the measured answer, else [`Scaled::Unknown`]. What a plan's caps are made from
/// ([`crate::Compositor::caps`]).
pub fn scaled_measured() -> Scaled {
    lock(&SCALED).measured.unwrap_or(Scaled::Unknown)
}

/// What a probe of this module has found, and when it may be asked again.
struct State<T> {
    measured: Option<T>,
    failures: u32,
    retry_at: Option<Instant>,
}

impl<T> State<T> {
    const fn new() -> Self {
        Self {
            measured: None,
            failures: 0,
            retry_at: None,
        }
    }
}

/// The answer of a probe of this module: what it says while it does not know, and what is logged when it settles on a difference
/// or cannot run.
trait Verdict: Copy {
    const UNKNOWN: Self;
    fn settled(self);
    fn failed(failures: u32, retry_in_secs: u64);
}

impl Verdict for Kernels {
    const UNKNOWN: Self = Self::Unknown;

    fn settled(self) {
        if let Self::Differs(why) = self {
            tracing::warn!(
                why,
                "this FFmpeg converts a layer into the composite's matrix another way than the GPU does; \
                 stacks of mixed matrices are drawn by FFmpeg"
            );
        }
    }

    fn failed(failures: u32, retry_in_secs: u64) {
        tracing::warn!(
            failures,
            retry_in_secs,
            "could not measure FFmpeg's conversion between colour matrices; stacks of mixed matrices are not drawn on the GPU until it can"
        );
    }
}

impl Verdict for Scaled {
    const UNKNOWN: Self = Self::Unknown;

    fn settled(self) {
        if let Self::Differs(why) = self {
            tracing::warn!(
                why,
                "this FFmpeg resizes and converts a layer's matrix in one scale another way than the GPU does; \
                 such layers are drawn by FFmpeg"
            );
        }
    }

    fn failed(failures: u32, retry_in_secs: u64) {
        tracing::warn!(
            failures,
            retry_in_secs,
            "could not measure FFmpeg's scale that resizes a picture and converts its matrix; such layers are not drawn on the GPU until it can"
        );
    }
}

static STATE: Mutex<State<Kernels>> = Mutex::new(State::new());

fn lock<T>(state: &Mutex<State<T>>) -> std::sync::MutexGuard<'_, State<T>> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether this FFmpeg converts a layer into the composite's matrix as [`change_matrix`] does (and, where it does, whether its `scale`
/// that resizes as it converts is the cascade [`scale_and_change_matrix`] follows: [`scaled`]). The first call runs
/// the probe: three short `ffmpeg` children at once, and only where those match two that make the tagged files and then the three-clip graph over them (each bounded by 20 s) with no lock held, a blocking thread's, ahead of
/// the frame that needs it ([`crate::prepare`]). [`Kernels::Unknown`] is not remembered: the probe is run again after a
/// backoff.
pub fn kernels() -> Kernels {
    let kernels = unscaled();
    // Where the conversion matches, the same pass measures the scale that resizes as it converts ([`scaled`], which needs it).
    if kernels == Kernels::Match {
        let _ = scaled();
    }
    kernels
}

/// [`kernels`] without the scaled probe.
fn unscaled() -> Kernels {
    cached(&STATE, Instant::now(), measure)
}

/// What is known without running anything: the measured answer, else [`Kernels::Unknown`]. What a plan's caps are made
/// from ([`crate::Compositor::caps`]): a plan does not wait for a probe.
pub fn measured() -> Kernels {
    lock(&STATE).measured.unwrap_or(Kernels::Unknown)
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300))
}

fn cached<T: Verdict>(state: &Mutex<State<T>>, now: Instant, measure: impl FnOnce() -> Option<T>) -> T {
    {
        let s = lock(state);
        if let Some(k) = s.measured {
            return k;
        }
        if s.retry_at.is_some_and(|t| now < t) {
            return T::UNKNOWN;
        }
    }
    let measured = measure();
    let mut s = lock(state);
    if let Some(k) = s.measured {
        return k;
    }
    match measured {
        Some(k) => {
            k.settled();
            s.measured = Some(k);
            k
        }
        None if s.retry_at.is_some_and(|t| t > now) => T::UNKNOWN,
        None => {
            s.failures += 1;
            let wait = backoff(s.failures);
            s.retry_at = Some(Instant::now() + wait);
            T::failed(s.failures, wait.as_secs());
            T::UNKNOWN
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn the_mmx_variant_differs_from_the_c_one_only_between_the_first_and_last_rows() {
        let (w, h) = (16, 16);
        let rgb = noise(w * h * 3, 7);
        let c = rgb_to_yuv420_24(&rgb, w, h, YuvMatrix::Bt709, Variant::C);
        let mmx = rgb_to_yuv420_24(&rgb, w, h, YuvMatrix::Bt709, Variant::Mmx);
        assert_ne!(c[0], mmx[0]);
        // The first two and last two luma rows, and the first and last chroma row, are the C function's in both.
        assert_eq!(c[0][..2 * w], mmx[0][..2 * w]);
        assert_eq!(c[0][(h - 2) * w..], mmx[0][(h - 2) * w..]);
        assert_eq!(c[1][..w / 2], mmx[1][..w / 2]);
        assert_eq!(c[1][(h / 2 - 1) * (w / 2)..], mmx[1][(h / 2 - 1) * (w / 2)..]);
    }

    /// The plan says a scale is the cascade (`ScaleStage::cascade_refusal`) only where the model follows it: no layer is drawn that
    /// the passes would then refuse.
    #[test]
    fn the_plan_judges_the_cascade_by_what_the_model_follows() {
        use kerf_core::layer_geometry::{Rect, ScaleStage};
        let mut judged = 0;
        for sw in (6..=96).step_by(10) {
            for sh in (6..=96).step_by(10) {
                for dw in (4..=96).step_by(6) {
                    for dh in (4..=96).step_by(6) {
                        let stage = ScaleStage {
                            src: Rect::whole(sw, sh),
                            scaled: (dw, dh),
                            keep: Rect::whole(dw, dh),
                        };
                        let planes = [
                            vec![0u8; (sw * sh) as usize],
                            vec![0u8; (sw * sh / 4) as usize],
                            vec![0u8; (sw * sh / 4) as usize],
                        ];
                        let pic = Pic {
                            planes: [&planes[0], &planes[1], &planes[2]],
                            size: (sw as usize, sh as usize),
                        };
                        let modelled = scale_and_change_matrix(
                            pic,
                            (dw as usize, dh as usize),
                            YuvMatrix::Bt709,
                            YuvMatrix::Bt601,
                            Packed::Mmx,
                        );
                        if stage.cascade_refusal().is_none() {
                            assert!(
                                modelled.is_some(),
                                "{sw}x{sh} -> {dw}x{dh}: the plan draws it, the model does not follow it"
                            );
                            judged += 1;
                        }
                    }
                }
            }
        }
        assert!(judged > 100, "{judged}");
    }

    #[test]
    fn a_scale_that_grows_or_leaves_the_rows_alone_is_not_the_cascade() {
        use kerf_core::layer_geometry::{Rect, ScaleStage};
        let stage = |src: (u32, u32), dst: (u32, u32)| ScaleStage {
            src: Rect::whole(src.0, src.1),
            scaled: dst,
            keep: Rect::whole(dst.0, dst.1),
        };
        assert!(stage((640, 360), (320, 180)).cascade_refusal().is_none());
        assert!(stage((1080, 1920), (170, 302)).cascade_refusal().is_none());
        assert!(
            stage((640, 360), (100, 360)).cascade_refusal().is_some(),
            "luma rows not scaled"
        );
        assert!(stage((640, 360), (960, 540)).cascade_refusal().is_some(), "grows");
        assert!(stage((1080, 1920), (1920, 1080)).cascade_refusal().is_some(), "same area");
        assert!(stage((641, 360), (320, 180)).cascade_refusal().is_some(), "odd source");
        assert!(stage((640, 360), (321, 180)).cascade_refusal().is_some(), "odd result");
        let cropped = ScaleStage {
            keep: Rect {
                x: 2,
                y: 0,
                w: 100,
                h: 180,
            },
            ..stage((640, 360), (320, 180))
        };
        assert!(cropped.cascade_refusal().is_some(), "cropped result");
    }

    #[test]
    fn a_black_picture_is_black_in_every_matrix() {
        let (w, h) = (8, 8);
        let (y, u, v) = (vec![16u8; w * h], vec![128u8; w * h / 4], vec![128u8; w * h / 4]);
        for from in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
            for to in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
                let out = change_matrix([&y, &u, &v], w, h, from, to, Variant::C);
                assert_eq!(out[0], y, "{from:?} -> {to:?}");
                assert_eq!(out[1], u);
                assert_eq!(out[2], v);
            }
        }
    }

    #[test]
    fn a_measured_answer_is_kept_and_an_unknown_one_waits() {
        let state = Mutex::new(State::new());
        let now = Instant::now();
        assert_eq!(cached(&state, now, || None), Kernels::Unknown);
        // Asked again inside the backoff: not measured again.
        assert_eq!(
            cached(&state, now, || panic!("measured inside the backoff")),
            Kernels::Unknown
        );
        assert_eq!(
            cached(&state, now + Duration::from_secs(6), || Some(Kernels::Match)),
            Kernels::Match
        );
        assert_eq!(
            cached(&state, now + Duration::from_secs(7), || panic!("measured twice")),
            Kernels::Match
        );
        assert!(Kernels::Match.converts() && !Kernels::Unknown.converts() && !Kernels::Differs("x").converts());
    }
    /// The scaled probe is kept like the conversion's: a measured answer for good, an unknown one only until the backoff has run, and
    /// a difference is no failure.
    #[test]
    fn a_measured_cascade_is_kept_and_an_unknown_one_waits() {
        let state = Mutex::new(State::<Scaled>::new());
        let now = Instant::now();
        assert_eq!(cached(&state, now, || None), Scaled::Unknown);
        assert_eq!(cached(&state, now, || panic!("measured inside the backoff")), Scaled::Unknown);
        let differs = Scaled::Differs("a cascade of another kind");
        assert_eq!(cached(&state, now + Duration::from_secs(6), || Some(differs)), differs);
        assert_eq!(
            cached(&state, now + Duration::from_secs(7), || panic!("measured twice")),
            differs
        );
        assert!(Scaled::Match.converts() && !differs.converts() && !Scaled::Unknown.converts());
        // (Nothing has been measured in a unit test: the process answers `Unknown`, which the caps leave out.)
        assert!(!scaled_measured().converts());
    }

    #[test]
    fn a_tile_of_three_is_the_probe_picture_in_its_columns() {
        let (w, h) = PROBE;
        // Every sample holds its tile's number: 3w x h luma, then the two 3w/2 x h/2 chroma planes.
        let plane = |pw: usize, ph: usize| -> Vec<u8> { (0..pw * ph).map(|i| ((i % pw) / (pw / 3)) as u8).collect() };
        let mut frame = plane(3 * w, h);
        frame.extend(plane(3 * w / 2, h / 2));
        frame.extend(plane(3 * w / 2, h / 2));
        for k in 0..3 {
            let t = tile(&frame, k);
            assert_eq!(t.len(), w * h * 3 / 2);
            assert!(t.iter().all(|&v| usize::from(v) == k), "tile {k}");
        }
    }
}
