//! The placement of a moving zoom or position: FFmpeg's `pad`, `perspective` and `crop`, in their own
//! arithmetic (`engine::cli::placement_chain`).
//!
//! A clip whose zoom or position moves ends its chain in a placement: the picture (a `yuva420p`
//! frame, at the size the rest of the chain made it) is padded with transparency to the canvas and
//! two samples past itself on every side, resampled by `perspective` (bilinear, 8 bits of sub-pixel
//! position, `eval=frame`) so that canvas pixel `X` reads the picture at `(X + 0.5 - c) / s + w/2 - 0.5`,
//! and cut back to the canvas round the same centre. This module is the part of that a renderer needs
//! and a plan can judge, pure and in `f64` operation for operation as the filter writes it: the sizes
//! `pad` and `crop` make, the four corners the expressions evaluate to, the filter's projective matrix
//! (`calc_persp_luts`, `sense=source`) and the table of sample positions it builds from it, one pair
//! of fixed-point coordinates (1/256 of a luma sample) per pixel.
//!
//! The filter's matrix is built with `lrint(256 * (x0*x + x1*y + x2) / (x6*x + x7*y + x8))` for
//! every pixel of the frame. For corners that form an axis-aligned rectangle (these always do) `x6`
//! and `x3` come out exactly zero, so `v` depends on the row alone and `x6 * x` adds nothing, and `x1`
//! and `x7` are zero or **rounding noise** (`(x0 - x1) - x0 + x1` is not always 0 in `f64`): the
//! column of a pixel decides its `u` unless the noise moves the product across a rounding boundary.
//! [`PlacementMap::new`] takes the per-axis table when it can prove no pixel of the window rounds
//! the other way (the value at the window's first and last row, widened by far more than the noise,
//! stays inside one rounding interval) and builds the filter's whole table, pixel by pixel, otherwise.
//! [`PlacementMap::exact`] is always the latter: the reference the first is held to.
//!
//! Nothing here touches a picture; the resampler is the renderer's (`kerf-gpu`'s `placement`).

/// `SUB_PIXEL_BITS` of `vf_perspective.c`: the position is kept in 1/256 of a sample.
pub const SUB_PIXEL_BITS: u32 = 8;
const SUB_PIXELS: f64 = (1 << SUB_PIXEL_BITS) as f64;

/// A position in sub-pixels above this is refused: `lrint` of it leaves the 32 bits the filter's tables
/// keep (a `long` on Linux and macOS, 32 bits on Windows), and past it the result is not the filter's.
const REACH: f64 = (1u32 << 21) as f64;

/// Where the placement puts the picture: the zoom about its centre (`None`: the picture keeps its
/// size, only the position moves) and the position of that centre from the canvas', in canvas
/// widths and heights (a slide's or push's travel included).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub scale: Option<f64>,
    pub pos: (f64, f64),
}

/// The frames of the placement chain for a picture of `input` samples on a `canvas`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementFrame {
    /// The picture as `pad` leaves it: the input less an odd last column (the border it draws over
    /// starts on the 4:2:0 grid). The height is the input's: **an odd one is not reproduced** (see
    /// [`PlacementFrame::exact_for`]).
    pub picture: (u32, u32),
    /// `pad`'s margin on each side, even.
    pub margin: (u32, u32),
    /// `perspective`'s frame (the padded one): `picture` plus a margin on each side.
    pub frame: (u32, u32),
    /// Where `crop` takes the canvas from in that frame, even.
    pub crop: (u32, u32),
    pub canvas: (u32, u32),
}

impl PlacementFrame {
    /// Whether the chain's frames are the filters' for a picture of this size. `pad` draws its right
    /// border from the 4:2:0 grid over the picture's last column when the width is odd (every build,
    /// whether it copies the picture or pads in place), but it draws its bottom bar over the last **row**
    /// of an odd-height picture only when it pads in place, which depends on how the graph allocated the
    /// frame, and 9.0.2 keeps the row there where 4.4.2 loses it (`color=size=6x5`, padded in place: 4 rows
    /// on 4.4.2, 5 on 9.0.2; copied, 3 of 3 on both). So an odd height is FFmpeg's.
    pub fn exact_for(input: (u32, u32)) -> bool {
        input.1.is_multiple_of(2)
    }

    /// The chain's frames: `pad=w='iw+4*ceil(max(0,cw-iw)/4)+4':x='2*ceil(max(0,cw-iw)/4)+2'` (the
    /// width and the offset rounded down to even: `ff_draw_round_to_sub`; the right border starts at the
    /// rounded width), then `crop=cw:ch:x='2*floor((iw-cw)/4)'`. Meant for [`PlacementFrame::exact_for`] sizes.
    pub fn new(input: (u32, u32), canvas: (u32, u32)) -> Self {
        let axis = |i: u32, c: u32| {
            let k = c.saturating_sub(i).div_ceil(4);
            let margin = 2 * k + 2;
            let picture = i & !1;
            let frame = (i + 2 * margin) & !1;
            (picture, margin, frame, 2 * ((frame - c.min(frame)) / 4))
        };
        let (pw, mx, fw, cx) = axis(input.0, canvas.0);
        let (ph, my, fh, cy) = axis(input.1, canvas.1);
        let ph = if input.1.is_multiple_of(2) { ph } else { input.1 };
        Self {
            picture: (pw, ph),
            margin: (mx, my),
            frame: (fw, fh),
            crop: (cx, cy),
            canvas,
        }
    }

    /// The frames of a `perspective` that reads a picture with no `pad` and no margin (a moving still's window,
    /// [`crate::still_motion`]): the picture is the whole frame, and the first `canvas` samples of the result are
    /// kept (`crop=W:H:0:0`).
    pub fn window(picture: (u32, u32), canvas: (u32, u32)) -> Self {
        Self {
            picture,
            margin: (0, 0),
            frame: picture,
            crop: (0, 0),
            canvas,
        }
    }

    /// The four corners the `perspective` expressions evaluate to, top-left, top-right, bottom-left,
    /// bottom-right, each `[x, y]`: the position in the padded frame that the corner of the frame reads,
    /// worked out in the order the expression writes it (`(edge + 0.5 - crop - centre) / s + dim/2 - 0.5`).
    pub fn corners(&self, pose: &Pose) -> [[f64; 2]; 4] {
        let corner = |edge: f64, dim: u32, canvas: u32, crop: u32, pos: f64| {
            let centre = f64::from(canvas) / 2.0 + pos * f64::from(canvas);
            let off = edge + 0.5 - f64::from(crop) - centre;
            match pose.scale {
                Some(s) => off / s + f64::from(dim) / 2.0 - 0.5,
                None => off + f64::from(dim) / 2.0 - 0.5,
            }
        };
        let (w, h) = (f64::from(self.frame.0), f64::from(self.frame.1));
        let (x0, x1) = (
            corner(0.0, self.frame.0, self.canvas.0, self.crop.0, pose.pos.0),
            corner(w, self.frame.0, self.canvas.0, self.crop.0, pose.pos.0),
        );
        let (y0, y1) = (
            corner(0.0, self.frame.1, self.canvas.1, self.crop.1, pose.pos.1),
            corner(h, self.frame.1, self.canvas.1, self.crop.1, pose.pos.1),
        );
        [[x0, y0], [x1, y0], [x0, y1], [x1, y1]]
    }

    /// Whether the pose is one the filter's fixed-point tables hold: finite corners no farther than
    /// [`REACH`] samples from the picture (so `lrint` of a position stays inside 32 bits on every platform).
    pub fn reachable(&self, pose: &Pose) -> bool {
        pose.scale.is_none_or(|s| s.is_finite() && s > 0.0)
            && pose.pos.0.is_finite()
            && pose.pos.1.is_finite()
            && Self::corners_reachable(&self.corners(pose))
    }

    /// Whether four corners (the filter's `x0..y3`) are finite and no farther than [`REACH`] samples from the picture.
    pub fn corners_reachable(corners: &[[f64; 2]; 4]) -> bool {
        corners.iter().flatten().all(|c| c.is_finite() && c.abs() <= REACH)
    }
}

/// What the placement cannot be reproduced for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PlacementError(pub String);

/// `calc_persp_luts`: the nine numbers of the projective map, `sense=source`, written as the C does
/// (`ref[i]` are the corners, `w` and `h` the frame's sides).
fn matrix(r: &[[f64; 2]; 4], w: f64, h: f64) -> [f64; 9] {
    let x6 = ((r[0][0] - r[1][0] - r[2][0] + r[3][0]) * (r[2][1] - r[3][1])
        - (r[0][1] - r[1][1] - r[2][1] + r[3][1]) * (r[2][0] - r[3][0]))
        * h;
    let x7 = ((r[0][1] - r[1][1] - r[2][1] + r[3][1]) * (r[1][0] - r[3][0])
        - (r[0][0] - r[1][0] - r[2][0] + r[3][0]) * (r[1][1] - r[3][1]))
        * w;
    let q = (r[1][0] - r[3][0]) * (r[2][1] - r[3][1]) - (r[2][0] - r[3][0]) * (r[1][1] - r[3][1]);
    let x0 = q * (r[1][0] - r[0][0]) * h + x6 * r[1][0];
    let x1 = q * (r[2][0] - r[0][0]) * w + x7 * r[2][0];
    let x2 = q * r[0][0] * w * h;
    let x3 = q * (r[1][1] - r[0][1]) * h + x6 * r[1][1];
    let x4 = q * (r[2][1] - r[0][1]) * w + x7 * r[2][1];
    let x5 = q * r[0][1] * w * h;
    let x8 = q * w * h;
    [x0, x1, x2, x3, x4, x5, x6, x7, x8]
}

/// `lrint(SUB_PIXELS * (a*x + b*y + c) / (d*x + e*y + f))`, evaluated as the C does, as the real number
/// (its rounding is the caller's).
fn position(m: &[f64; 9], row: usize, x: f64, y: f64) -> f64 {
    let k = row * 3;
    let num = m[k] * x + m[k + 1] * y + m[k + 2];
    let den = m[6] * x + m[7] * y + m[8];
    (SUB_PIXELS * num) / den
}

fn lrint(v: f64) -> Option<i32> {
    // (`round_ties_even` is `lrint` in the default rounding mode; past 2^30 the filter's table is not
    // something to copy: it is `long` assigned to `int`.)
    (v.is_finite() && v.abs() < f64::from(1u32 << 30)).then(|| v.round_ties_even() as i32)
}

/// Whether every real number in `[lo, hi]` (widened by `slack`) rounds to the same integer, and which.
fn one_rounding(lo: f64, hi: f64, slack: f64) -> Option<i32> {
    let (a, b) = (lo.min(hi) - slack, lo.max(hi) + slack);
    if !(a.is_finite() && b.is_finite()) || a.abs() >= f64::from(1u32 << 30) || b.abs() >= f64::from(1u32 << 30) {
        return None;
    }
    let k = (a + 0.5).floor();
    (a > k - 0.5 && b < k + 0.5).then_some(k as i32)
}

/// The sample positions `perspective` computes for the pixels of the canvas window, in 1/256 of a
/// luma sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tables {
    /// `u` depends on the column and `v` on the row: `canvas.0` and `canvas.1` entries.
    Axes { u: Vec<i32>, v: Vec<i32> },
    /// One pair per pixel, row by row (`canvas.0 * canvas.1` each): the filter's whole table.
    Pixels { u: Vec<i32>, v: Vec<i32> },
}

/// The placement's map: the frames and the positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementMap {
    pub frame: PlacementFrame,
    pub tables: Tables,
}

impl PlacementMap {
    /// The map of `pose`, as small as it can be made without a pixel of it differing from the filter's
    /// ([`PlacementMap::exact`]).
    pub fn new(frame: PlacementFrame, pose: &Pose) -> Result<Self, PlacementError> {
        if !frame.reachable(pose) {
            return Err(too_far());
        }
        Self::from_corners(frame, &frame.corners(pose))
    }

    /// [`PlacementMap::new`] for corners the expressions evaluated to directly (the filter's `x0..y3`, top-left,
    /// top-right, bottom-left, bottom-right): a moving still's `perspective` has the four of them from its own
    /// expressions, not a pose on a canvas.
    pub fn from_corners(frame: PlacementFrame, corners: &[[f64; 2]; 4]) -> Result<Self, PlacementError> {
        let m = Self::setup_corners(&frame, corners)?;
        // The rectangle's `x6` and `x3` are exactly zero; anything else is not a case this reasons about.
        if m.matrix[6] != 0.0 || m.matrix[3] != 0.0 {
            return Self::exact_corners(frame, corners);
        }
        let (cw, ch) = frame.canvas;
        let (y_first, y_last) = (f64::from(frame.crop.1), f64::from(frame.crop.1 + ch - 1));
        let rows = |y: f64| position(&m.matrix, 1, 0.0, y);
        let v = (0..ch)
            .map(|j| lrint(rows(f64::from(frame.crop.1 + j))))
            .collect::<Option<Vec<_>>>();
        let Some(v) = v else {
            return Err(too_far());
        };
        let noisy = m.matrix[1] != 0.0 || m.matrix[7] != 0.0;
        let mut u = Vec::with_capacity(cw as usize);
        for i in 0..cw {
            let x = f64::from(frame.crop.0 + i);
            let a = position(&m.matrix, 0, x, y_first);
            if !noisy {
                match lrint(a) {
                    Some(c) => u.push(c),
                    None => return Err(too_far()),
                }
                continue;
            }
            // The noise is a few units in the last place of numbers a hundred thousand times smaller than
            // the bound; the window's ends carry the whole of the (monotonic) change between them.
            let b = position(&m.matrix, 0, x, y_last);
            let slack = 1e-6 + a.abs().max(b.abs()) * 1e-12;
            match one_rounding(a, b, slack) {
                Some(c) => u.push(c),
                None => return Self::exact_corners(frame, corners),
            }
        }
        Ok(Self {
            frame,
            tables: Tables::Axes { u, v },
        })
    }

    /// The filter's table over the window, pixel by pixel, each position computed as `calc_persp_luts`
    /// does.
    pub fn exact(frame: PlacementFrame, pose: &Pose) -> Result<Self, PlacementError> {
        if !frame.reachable(pose) {
            return Err(too_far());
        }
        Self::exact_corners(frame, &frame.corners(pose))
    }

    /// [`PlacementMap::exact`] for corners.
    pub fn exact_corners(frame: PlacementFrame, corners: &[[f64; 2]; 4]) -> Result<Self, PlacementError> {
        let m = Self::setup_corners(&frame, corners)?;
        let (cw, ch) = frame.canvas;
        let mut u = Vec::with_capacity(cw as usize * ch as usize);
        let mut v = Vec::with_capacity(cw as usize * ch as usize);
        for j in 0..ch {
            let y = f64::from(frame.crop.1 + j);
            for i in 0..cw {
                let x = f64::from(frame.crop.0 + i);
                match (lrint(position(&m.matrix, 0, x, y)), lrint(position(&m.matrix, 1, x, y))) {
                    (Some(a), Some(b)) => {
                        u.push(a);
                        v.push(b);
                    }
                    _ => return Err(too_far()),
                }
            }
        }
        Ok(Self {
            frame,
            tables: Tables::Pixels { u, v },
        })
    }

    fn setup_corners(frame: &PlacementFrame, r: &[[f64; 2]; 4]) -> Result<Setup, PlacementError> {
        if !PlacementFrame::corners_reachable(r) {
            return Err(too_far());
        }
        let matrix = matrix(r, f64::from(frame.frame.0), f64::from(frame.frame.1));
        // The filter divides by `x6*x + x7*y + x8`; a zero or non-finite one is a picture the pose
        // has made nothing of.
        if !matrix.iter().all(|v| v.is_finite()) || matrix[8] == 0.0 {
            return Err(PlacementError("a placement whose map is degenerate".into()));
        }
        Ok(Setup { matrix })
    }

    /// The pair the filter holds for the pixel `(x, y)` of the canvas (luma resolution), in 1/256 of a
    /// luma sample.
    pub fn entry(&self, x: u32, y: u32) -> (i32, i32) {
        let (cw, _) = self.frame.canvas;
        match &self.tables {
            Tables::Axes { u, v } => (u[x as usize], v[y as usize]),
            Tables::Pixels { u, v } => {
                let at = y as usize * cw as usize + x as usize;
                (u[at], v[at])
            }
        }
    }
}

struct Setup {
    matrix: [f64; 9],
}

fn too_far() -> PlacementError {
    PlacementError("a placement whose sample positions leave the fixed-point range of FFmpeg's perspective filter".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frames_are_pads_and_crops() {
        // A picture the size of the canvas: two samples of margin on each side, cropped back from two.
        let f = PlacementFrame::new((1920, 1080), (1920, 1080));
        assert_eq!(
            (f.picture, f.margin, f.frame, f.crop),
            ((1920, 1080), (2, 2), (1924, 1084), (2, 2))
        );
        // A picture much smaller than the canvas grows the margin to hold the canvas.
        let f = PlacementFrame::new((480, 270), (1920, 1080));
        assert_eq!(f.margin, (2 * 360 + 2, 2 * 203 + 2));
        assert_eq!(f.frame, (480 + 2 * f.margin.0, 270 + 2 * f.margin.1));
        assert_eq!((f.frame.0 - 1920) % 4, 0);
        assert_eq!(f.crop, (2 * ((f.frame.0 - 1920) / 4), 2 * ((f.frame.1 - 1080) / 4)));
        // An odd width loses its last column to the border, and the frame is even.
        let f = PlacementFrame::new((361, 204), (722, 640));
        assert_eq!(f.picture, (360, 204));
        assert!(
            f.frame.0.is_multiple_of(2)
                && f.frame.1.is_multiple_of(2)
                && f.margin.0.is_multiple_of(2)
                && f.crop.0.is_multiple_of(2)
        );
        assert!(PlacementFrame::exact_for((361, 204)) && !PlacementFrame::exact_for((360, 203)));
    }

    #[test]
    fn a_picture_at_rest_is_read_where_it_is() {
        for (input, canvas) in [
            ((1920, 1080), (1920, 1080)),
            ((640, 360), (640, 360)),
            ((400, 300), (640, 360)),
        ] {
            let f = PlacementFrame::new(input, canvas);
            let pose = Pose {
                scale: Some(1.0),
                pos: (0.0, 0.0),
            };
            let map = PlacementMap::new(f, &pose).unwrap();
            // Canvas pixel (X, Y) reads the padded frame at (X + (W - cw) / 2, Y + (H - ch) / 2): whole samples.
            let (cx, cy) = (f.canvas.0 / 2, f.canvas.1 / 2);
            let (u, v) = map.entry(cx, cy);
            assert_eq!((u & 255, v & 255), (0, 0), "{input:?} in {canvas:?}");
            assert_eq!(u >> 8, (cx + (f.frame.0 - canvas.0) / 2) as i32, "{input:?} in {canvas:?}");
            assert_eq!(v >> 8, (cy + (f.frame.1 - canvas.1) / 2) as i32, "{input:?} in {canvas:?}");
        }
    }

    /// A tiny deterministic generator (the tests need many poses, not good ones).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
        }
    }

    #[test]
    fn the_per_axis_tables_are_the_filters_whole_table_wherever_they_are_taken() {
        let mut rng = Lcg(0x9E3779B97F4A7C15);
        let (mut axes, mut full) = (0, 0);
        for n in 0..4000 {
            let canvas = [(64u32, 36u32), (96, 54), (160, 90), (128, 72)][n % 4];
            let input = match n % 5 {
                0 => canvas,
                1 => (canvas.0 / 2, canvas.1 / 2),
                2 => (canvas.0 - 1, canvas.1 - 1),
                3 => (canvas.0 * 3 / 4, canvas.1),
                _ => (canvas.0 + 8, canvas.1 + 6),
            };
            let f = PlacementFrame::new(input, canvas);
            let pose = Pose {
                scale: (n % 7 != 0).then(|| 0.2 + rng.next() * 3.0),
                pos: ((rng.next() - 0.5) * 0.8, (rng.next() - 0.5) * 0.8),
            };
            let fast = PlacementMap::new(f, &pose).unwrap();
            let slow = PlacementMap::exact(f, &pose).unwrap();
            match fast.tables {
                Tables::Axes { .. } => axes += 1,
                Tables::Pixels { .. } => full += 1,
            }
            for y in 0..canvas.1 {
                for x in 0..canvas.0 {
                    assert_eq!(
                        fast.entry(x, y),
                        slow.entry(x, y),
                        "{input:?} in {canvas:?} {pose:?} at ({x}, {y})"
                    );
                }
            }
        }
        assert!(axes > 3900, "the per-axis tables should be the rule: {axes} against {full}");
    }

    #[test]
    fn structured_poses_whose_positions_sit_on_a_rounding_boundary_are_exact() {
        // Scales and positions whose sample positions are exact multiples of 1/512 of a sample, so that
        // a position can sit on a tie; the table must still be the filter's.
        for scale in [
            None,
            Some(0.5),
            Some(1.0),
            Some(2.0),
            Some(4.0),
            Some(8.0),
            Some(1.0 / 3.0),
            Some(1.5),
        ] {
            for pos in [(0.0, 0.0), (0.25, -0.125), (1.0 / 1024.0, 1.0 / 2048.0), (-0.5, 0.5)] {
                let f = PlacementFrame::new((128, 72), (128, 72));
                let pose = Pose { scale, pos };
                let fast = PlacementMap::new(f, &pose).unwrap();
                let slow = PlacementMap::exact(f, &pose).unwrap();
                for y in 0..72 {
                    for x in 0..128 {
                        assert_eq!(fast.entry(x, y), slow.entry(x, y), "{pose:?} at ({x}, {y})");
                    }
                }
            }
        }
    }

    #[test]
    fn a_pose_beyond_the_fixed_point_range_is_refused() {
        let f = PlacementFrame::new((1920, 1080), (1920, 1080));
        let ok = |scale: Option<f64>, pos: (f64, f64)| f.reachable(&Pose { scale, pos });
        assert!(ok(Some(0.001), (0.0, 0.0)));
        assert!(!ok(Some(1e-6), (0.0, 0.0)));
        assert!(!ok(Some(f64::NAN), (0.0, 0.0)));
        assert!(!ok(Some(0.0), (0.0, 0.0)));
        assert!(!ok(Some(-1.0), (0.0, 0.0)));
        assert!(!ok(None, (f64::INFINITY, 0.0)));
        assert!(!ok(None, (4000.0, 0.0)));
        assert!(PlacementMap::new(
            f,
            &Pose {
                scale: Some(1e-6),
                pos: (0.0, 0.0)
            }
        )
        .is_err());
    }

    #[test]
    fn a_window_has_no_margin_and_reads_the_whole_frame() {
        let f = PlacementFrame::window((120, 80), (64, 36));
        assert_eq!(
            (f.picture, f.margin, f.frame, f.crop, f.canvas),
            ((120, 80), (0, 0), (120, 80), (0, 0), (64, 36))
        );
        assert!(PlacementFrame::corners_reachable(&[
            [0.0, 0.0],
            [120.0, 0.0],
            [0.0, 80.0],
            [120.0, 80.0]
        ]));
        assert!(!PlacementFrame::corners_reachable(&[
            [f64::NAN, 0.0],
            [1.0, 0.0],
            [0.0, 1.0],
            [1.0, 1.0]
        ]));
        assert!(!PlacementFrame::corners_reachable(&[
            [0.0, 0.0],
            [1e9, 0.0],
            [0.0, 1.0],
            [1.0, 1.0]
        ]));
    }

    #[test]
    fn corners_that_are_the_frames_own_read_every_pixel_where_it_is() {
        let f = PlacementFrame::window((120, 80), (64, 36));
        let corners = [[0.0, 0.0], [120.0, 0.0], [0.0, 80.0], [120.0, 80.0]];
        let m = PlacementMap::from_corners(f, &corners).unwrap();
        // A rectangle's table is one position per column and one per row, in 1/256 of a sample.
        assert!(matches!(m.tables, Tables::Axes { .. }));
        for (x, y) in [(0, 0), (5, 7), (63, 35)] {
            assert_eq!(m.entry(x, y), (256 * x as i32, 256 * y as i32), "({x}, {y})");
        }
        // Stretched to half the frame, a pixel reads half a sample further each.
        let half = [[0.0, 0.0], [60.0, 0.0], [0.0, 40.0], [60.0, 40.0]];
        let m = PlacementMap::from_corners(f, &half).unwrap();
        assert_eq!(m.entry(10, 4), (256 * 10 / 2, 256 * 4 / 2));
    }

    #[test]
    fn from_corners_is_new_over_the_corners_of_a_pose_and_refuses_what_leaves_the_tables() {
        let frame = PlacementFrame::new((64, 36), (64, 36));
        let pose = Pose {
            scale: Some(1.3),
            pos: (0.02, -0.01),
        };
        let a = PlacementMap::new(frame, &pose).unwrap();
        let b = PlacementMap::from_corners(frame, &frame.corners(&pose)).unwrap();
        assert_eq!(a, b);
        let exact = PlacementMap::exact_corners(frame, &frame.corners(&pose)).unwrap();
        for y in 0..36 {
            for x in 0..64 {
                assert_eq!(a.entry(x, y), exact.entry(x, y), "({x}, {y})");
            }
        }
        let far = [[0.0, 0.0], [1e10, 0.0], [0.0, 1.0], [1.0, 1.0]];
        assert!(PlacementMap::from_corners(frame, &far).is_err());
        let degenerate = [[1.0, 1.0]; 4];
        assert!(PlacementMap::from_corners(frame, &degenerate).is_err());
    }
}
