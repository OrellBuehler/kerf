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
//! the same pass as its resize, which is not this. **Whose matrix is the composite's** is the chain's, not the frame's: the
//! first clip on an alpha plane (a clip entering through a dissolve, a keyed zoom or position, or a source with an alpha channel: nothing in its chain forces a converter), else the first clip, and every clip
//! of another matrix is converted into it once, wherever on the timeline ([`kerf_core::PlanCanvas::cut_matrix`], measured
//! with three clips in every combination of plain and dissolving). A **dissolving** clip's chain ends in `format=yuva420p`
//! and its last `scale` converts the matrix itself in the same pass as its resize, as in a still: such a clip is converted by
//! the model only where that scale does not resize the picture (identical-size scale: the unscaled cascade, measured).
//!
//! The coefficients are `Rgb2Yuv`'s ([`crate::roundtrip`]): `fill_rgb2yuv_table` of the destination's matrix.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use kerf_core::YuvMatrix;

use crate::grade::yuv420_to_rgb;
use crate::roundtrip::Rgb2Yuv;

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
    let (w, h) = PROBE;
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
    let mut args = args_for(&[], &source(matrix));
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

struct State {
    measured: Option<Kernels>,
    failures: u32,
    retry_at: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State {
    measured: None,
    failures: 0,
    retry_at: None,
});

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether this FFmpeg converts a layer into the composite's matrix as [`change_matrix`] does. The first call runs
/// the probe: three short `ffmpeg` children at once, and only where those match two that make the tagged files and then the three-clip graph over them (each bounded by 20 s) with no lock held, a blocking thread's, ahead of
/// the frame that needs it ([`crate::prepare`]). [`Kernels::Unknown`] is not remembered: the probe is run again after a
/// backoff.
pub fn kernels() -> Kernels {
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

fn cached(state: &Mutex<State>, now: Instant, measure: impl FnOnce() -> Option<Kernels>) -> Kernels {
    {
        let s = lock(state);
        if let Some(k) = s.measured {
            return k;
        }
        if s.retry_at.is_some_and(|t| now < t) {
            return Kernels::Unknown;
        }
    }
    let measured = measure();
    let mut s = lock(state);
    if let Some(k) = s.measured {
        return k;
    }
    match measured {
        Some(k) => {
            if let Kernels::Differs(why) = k {
                tracing::warn!(
                    why,
                    "this FFmpeg converts a layer into the composite's matrix another way than the GPU does; \
                     stacks of mixed matrices are drawn by FFmpeg"
                );
            }
            s.measured = Some(k);
            k
        }
        None if s.retry_at.is_some_and(|t| t > now) => Kernels::Unknown,
        None => {
            s.failures += 1;
            let wait = backoff(s.failures);
            s.retry_at = Some(Instant::now() + wait);
            tracing::warn!(
                failures = s.failures,
                retry_in_secs = wait.as_secs(),
                "could not measure FFmpeg's conversion between colour matrices; stacks of mixed matrices are not drawn on the GPU until it can"
            );
            Kernels::Unknown
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
        let state = Mutex::new(State {
            measured: None,
            failures: 0,
            retry_at: None,
        });
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
