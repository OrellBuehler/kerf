//! Whether this FFmpeg's `pad`, `perspective` and `crop` place a picture as the compositor's model does
//! ([`kerf_core::perspective`], [`crate::placement`]) — measured once per process on the placement chain **as the
//! engine writes it** ([`kerf_core::placement_probe_chain`]: the still's and the export's forms), never assumed and never
//! keyed on a version string. `GpuCaps::placement` and `GpuCaps::behaviors` are set only on a [`Placement::Match`].
//!
//! The model is exact against FFmpeg 4.4, 6.1 and 9.0 on the machines this was built on. A third build (a distro's 5, 7 or 8,
//! another `swscale`, another libm) is not known to be: a rounding of `pad`'s offsets, of `perspective`'s fixed-point
//! table, or of an expression's `sin` that lands elsewhere would be drawn wrong, sample for sample, on every moving zoom.
//! So the probe runs the chain on tiny frames, over noise with an alpha plane, and holds the planes to the model with no
//! tolerance:
//!
//! * **shapes**: a picture the size of the canvas, smaller, larger, with an odd width, and a thin one (`pad`'s rounding of
//!   its width and offset to the 4:2:0 grid, `crop`'s even offsets);
//! * **poses**: no zoom, a held one, an enlarging and a shrinking one; positions on a rounding tie (`lrint` of an exact
//!   half; the canvas is a power of two so every number is dyadic), non-dyadic ones, and ones off the frame (the clamped
//!   edge, `pad`'s transparent border); each pose in both forms of the filter (`eval=init`, the still's, and `eval=frame`,
//!   the export's);
//! * **motion**: a keyed zoom and position with a breath, a drift and a shake, as the export writes them over the frame count
//!   `perspective` has (`in`), against what the plan evaluates for the same frames
//!   ([`kerf_core::placement_probe_motion`]): the count's origin, `eval=frame` re-evaluating, and the machine's
//!   `sin` and `cos` against the app's.
//!
//! A probe that could not tell (FFmpeg would not run, timed out, wrote the wrong amount, ran out of its one deadline) is
//! [`Placement::Unknown`]: it is asked again after a backoff and nothing placed is drawn until it answers. **One probe runs
//! at a time** whoever asks (the render bar, a frame's [`crate::prepare`], a play session's primer, the self-test: the others
//! wait for it and take its answer, [`crate::measure`]), **inside one deadline** ([`TIMING`]) however many children it
//! takes, and each child's graph stays under the engine's argv rule ([`kerf_core::GRAPH_ARG_MAX`]: the poses of a shape are
//! stacked in as many runs as that takes, not in one 31 KB command line next to Windows' 32,767).

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use kerf_core::perspective::{PlacementFrame, PlacementMap, Pose};

use crate::measure::{self, Budget, Probe, Timing, Verdict};
use crate::placement::{reference, Picture, Placed};

/// What the probe found: [`Verdict::Match`] (the chain places a picture as the model does, in every case),
/// [`Verdict::Differs`] (it does not: the detail names the first case) or [`Verdict::Unknown`].
pub type Placement = Verdict;

/// One FFmpeg run is bounded at 20 s, the whole probe (a few dozen runs of tens of milliseconds) at 45 s, and a probe that
/// could not tell is not asked again before 30 s.
const TIMING: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(45),
    per_run: Duration::from_secs(20),
};

static STATE: Probe = Probe::new();

/// What is known without running anything: the measured answer, else [`Placement::Unknown`] (not measured yet, backing off
/// after a probe that could not tell, or held back by a self-test). What the compositor's caps are made from.
pub fn measured() -> Placement {
    measure::known(&STATE)
}

/// The answer, measured on the first call: a few dozen short `ffmpeg` children, one after the other (tens of milliseconds
/// each, each bounded by 20 s, all of them by 45 s), with no lock held — a blocking thread's, ahead of the frame that needs it
/// ([`crate::prepare`]). A caller that arrives while the probe runs waits for it (no longer than its deadline) and takes its
/// answer. `Unknown` is not remembered but for a backoff.
pub fn placement() -> Placement {
    measure::cached(&STATE, Instant::now(), TIMING, probe)
}

/// [`placement`] for the runtime self-test, which draws a placed frame only on a `Match`. A probe that cannot tell is run once
/// more after the backoff (and an earlier failure's backoff is waited out, not taken for the answer); one that cannot tell
/// then either holds the placement back for the process until a self-test that can tell has run, so the frames the GPU draws
/// are never ones its check skipped.
pub(crate) fn placement_for_selftest() -> Placement {
    measure::checked(&STATE, TIMING, probe)
}

/// A deterministic generator.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// A `yuva420p` picture of noise.
struct Noise {
    size: (u32, u32),
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    a: Vec<u8>,
}

impl Noise {
    fn new(rng: &mut Lcg, size: (u32, u32)) -> Self {
        let (w, h) = (size.0 as usize, size.1 as usize);
        let c = w.div_ceil(2) * h.div_ceil(2);
        Self {
            size,
            y: rng.bytes(w * h),
            u: rng.bytes(c),
            v: rng.bytes(c),
            a: rng.bytes(w * h),
        }
    }

    fn picture(&self) -> Picture<'_> {
        Picture {
            size: self.size,
            y: &self.y,
            u: &self.u,
            v: &self.v,
            a: Some(&self.a),
            opaque: 255,
        }
    }

    /// The raw frame `rawvideo` reads: Y, U, V, A.
    fn raw(&self) -> Vec<u8> {
        [&self.y, &self.u, &self.v, &self.a].into_iter().flatten().copied().collect()
    }
}

/// One pose in one form of the filter.
#[derive(Debug, Clone, Copy)]
struct Case {
    pose: Pose,
    per_frame: bool,
}

/// The poses of a canvas that is a power of two: positions on a rounding tie (`256 * u` an exact half: the position a multiple
/// of `1 / (512 * canvas)`, the zoom a power of two), then ones that are not dyadic, and ones off the frame. `full`: every
/// zoom, else three.
fn poses(canvas: (u32, u32), full: bool) -> Vec<Case> {
    let (cw, ch) = (f64::from(canvas.0), f64::from(canvas.1));
    let tie = |mx: i64, my: i64| (mx as f64 / (512.0 * cw), my as f64 / (512.0 * ch));
    let mut out = Vec::new();
    let scales: &[Option<f64>] = if full {
        &[None, Some(1.0), Some(2.0), Some(0.5), Some(4.0), Some(0.25)]
    } else {
        &[None, Some(2.0), Some(0.5)]
    };
    for (i, &scale) in scales.iter().enumerate() {
        for (j, (mx, my)) in [(1, 0), (3, 1), (255, 129), (-257, 511), (0, 0)].into_iter().enumerate() {
            out.push(Case {
                pose: Pose { scale, pos: tie(mx, my) },
                // Both forms of the filter, spread over the poses.
                per_frame: (i + j) % 3 == 0,
            });
        }
    }
    for (scale, pos, per_frame) in [
        (Some(1.37), (0.123, -0.0371), false),
        (Some(0.62), (-0.31, 0.2), true),
        (Some(3.1), (0.42, 0.37), false),
        (None, (0.0731, -0.4), true),
        (Some(1.0), (0.5, 0.0), false),
        // Off the frame, the clamped edge and `pad`'s transparent border.
        (Some(1.5), (1.5, 0.0), false),
        (Some(0.2), (-1.2, 0.9), true),
        (Some(1.0), (-2.5, 2.5), false),
        (Some(40.0), (0.3, -0.3), true),
        (Some(0.03), (0.0, 0.0), false),
    ] {
        out.push(Case {
            pose: Pose { scale, pos },
            per_frame,
        });
    }
    out
}

/// One shape: the canvas, the picture and whether to try every pose.
type Shape = ((u32, u32), (u32, u32), bool);

/// The shapes. All of an even height, the one the model reproduces.
const SHAPES: [Shape; 7] = [
    ((64, 32), (64, 32), true),
    ((64, 32), (32, 16), true),
    ((64, 32), (62, 32), true),
    ((64, 32), (80, 46), false),
    ((64, 32), (17, 12), false),
    ((66, 40), (65, 40), false),
    ((96, 54), (24, 54), false),
];

/// How many frames of motion are compared.
const MOTION_FRAMES: u32 = 12;

/// The first form of a number that parses back to the same `f64`.
fn num(x: f64) -> String {
    format!("{x}")
}

fn probe(budget: &Budget) -> Verdict {
    let mut rng = Lcg(0x0070_6c61_6365);
    for (canvas, input, full) in SHAPES {
        let noise = Noise::new(&mut rng, input);
        match stacked(budget, &noise, canvas, &poses(canvas, full)) {
            Ok(None) => {}
            Ok(Some(why)) => return Verdict::Differs(why),
            Err(why) => {
                tracing::warn!(why, "the placement probe could not tell");
                return Verdict::Unknown;
            }
        }
    }
    for (canvas, input) in [((64, 36), (64, 36)), ((64, 36), (40, 24)), ((66, 40), (65, 40))] {
        let noise = Noise::new(&mut rng, input);
        match in_motion(budget, &noise, canvas) {
            Ok(None) => {}
            Ok(Some(why)) => return Verdict::Differs(why),
            Err(why) => {
                tracing::warn!(why, "the placement probe could not tell");
                return Verdict::Unknown;
            }
        }
    }
    Verdict::Match
}

/// The first differing case of every pose of `cases` on one picture, stacked into as few runs as the argv rule allows
/// ([`runs`]): in a run the picture is split once per case, each branch placed by that case's chain, and the results laid
/// side by side in one frame. `Ok(None)`: all equal. `Err` when FFmpeg would not run or wrote something else than was asked.
fn stacked(budget: &Budget, noise: &Noise, canvas: (u32, u32), cases: &[Case]) -> Result<Option<String>, String> {
    let chains = chains(cases, canvas);
    for run in runs(&chains) {
        if let Some(why) = stack_run(budget, noise, canvas, &cases[run.clone()], &chains[run])? {
            return Ok(Some(why));
        }
    }
    Ok(None)
}

/// The placement chain of each case, as the engine writes it.
fn chains(cases: &[Case], canvas: (u32, u32)) -> Vec<String> {
    cases
        .iter()
        .map(|c| {
            let scale = c.pose.scale.map(num);
            kerf_core::placement_probe_chain(
                scale.as_deref(),
                (&num(c.pose.pos.0), &num(c.pose.pos.1)),
                canvas,
                c.per_frame,
            )
        })
        .collect()
}

/// The graph that places one picture by each of `chains` and stacks the results side by side.
fn stack_graph(chains: &[String]) -> String {
    let n = chains.len();
    let mut graph = format!("[0:v]split={n}");
    for i in 0..n {
        let _ = write!(graph, "[s{i}]");
    }
    graph.push(';');
    for (i, chain) in chains.iter().enumerate() {
        let _ = write!(graph, "[s{i}]{chain}[o{i}];");
    }
    for i in 0..n {
        let _ = write!(graph, "[o{i}]");
    }
    let _ = write!(graph, "hstack=inputs={n}[out]");
    graph
}

/// The cases cut into runs (ranges of them, in order) whose graph each stays within the engine's argv rule
/// ([`kerf_core::GRAPH_ARG_MAX`]): the chain of a pose is some 700 bytes and a shape has forty of them, 31 KB of command line
/// in one run, a kilobyte short of what Windows allows a process. A chain that is over the rule alone is a run of its own
/// (the run spills it to a file, [`Budget::ffmpeg`]).
fn runs(chains: &[String]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for end in 1..=chains.len() {
        let next_fits = end == chains.len() || stack_graph(&chains[start..=end]).len() <= kerf_core::GRAPH_ARG_MAX;
        if !next_fits {
            out.push(start..end);
            start = end;
        }
    }
    if start < chains.len() {
        out.push(start..chains.len());
    }
    out
}

/// One run of [`stacked`]: `cases` and their `chains`.
fn stack_run(
    budget: &Budget,
    noise: &Noise,
    canvas: (u32, u32),
    cases: &[Case],
    chains: &[String],
) -> Result<Option<String>, String> {
    let n = cases.len();
    let graph = stack_graph(chains);
    let size = format!("{}x{}", noise.size.0, noise.size.1);
    let out = budget
        .ffmpeg(
            &[
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuva420p",
                "-s",
                &size,
                "-i",
                "pipe:0",
                "-filter_complex",
                &graph,
                "-map",
                "[out]",
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuva420p",
                "pipe:1",
            ],
            noise.raw(),
        )
        .ok_or("ffmpeg did not place the probe's pictures")?;
    let (cw, ch) = (canvas.0 as usize, canvas.1 as usize);
    let (sw, chroma_w, chroma_h) = (cw * n, cw * n / 2, ch / 2);
    let want = sw * ch * 2 + 2 * chroma_w * chroma_h;
    if out.len() != want {
        return Err(format!("ffmpeg wrote {} bytes for the stack, not {want}", out.len()));
    }
    let planes = split_planes(&out, (sw, ch), (chroma_w, chroma_h));
    for (i, c) in cases.iter().enumerate() {
        let got = unstack(&planes, i, (cw, ch));
        if let Some(why) = differs(&got, noise, canvas, &c.pose) {
            return Ok(Some(format!(
                "{canvas:?} <- {:?}, {:?}{}: {why}",
                noise.size,
                c.pose,
                if c.per_frame { ", eval=frame" } else { "" }
            )));
        }
    }
    Ok(None)
}

/// The frames of a moving, behaving placement, as the export writes it over `perspective`'s frame count, against what the plan
/// evaluates for them.
fn in_motion(budget: &Budget, noise: &Noise, canvas: (u32, u32)) -> Result<Option<String>, String> {
    let motion = kerf_core::placement_probe_motion(MOTION_FRAMES);
    let chain = kerf_core::placement_probe_chain(Some(&motion.scale), (&motion.pos.0, &motion.pos.1), canvas, true);
    let size = format!("{}x{}", noise.size.0, noise.size.1);
    let one = noise.raw();
    let mut input = Vec::with_capacity(one.len() * MOTION_FRAMES as usize);
    for _ in 0..MOTION_FRAMES {
        input.extend_from_slice(&one);
    }
    let frames = MOTION_FRAMES.to_string();
    let out = budget
        .ffmpeg(
            &[
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuva420p",
                "-framerate",
                "30",
                "-s",
                &size,
                "-i",
                "pipe:0",
                "-vf",
                &chain,
                "-frames:v",
                &frames,
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuva420p",
                "pipe:1",
            ],
            input,
        )
        .ok_or("ffmpeg did not place the probe's moving picture")?;
    let (cw, ch) = (canvas.0 as usize, canvas.1 as usize);
    let each = cw * ch * 2 + 2 * (cw / 2) * (ch / 2);
    if out.len() != each * MOTION_FRAMES as usize {
        return Err(format!(
            "ffmpeg wrote {} bytes for {MOTION_FRAMES} frames, not {}",
            out.len(),
            each * MOTION_FRAMES as usize
        ));
    }
    for (k, (scale, pos)) in motion.poses.iter().enumerate() {
        let planes = split_planes(&out[k * each..(k + 1) * each], (cw, ch), (cw / 2, ch / 2));
        let got = unstack(&planes, 0, (cw, ch));
        let pose = Pose {
            scale: Some(*scale),
            pos: *pos,
        };
        if let Some(why) = differs(&got, noise, canvas, &pose) {
            return Ok(Some(format!(
                "{canvas:?} <- {:?}, a moving zoom, position and behaviours at frame {}: {pose:?}: {why}",
                noise.size,
                k + 1
            )));
        }
    }
    Ok(None)
}

/// The four planes of a `yuva420p` frame: Y and A of `luma`, U and V of `chroma`.
fn split_planes(frame: &[u8], luma: (usize, usize), chroma: (usize, usize)) -> [&[u8]; 4] {
    let (l, c) = (luma.0 * luma.1, chroma.0 * chroma.1);
    [
        &frame[..l],
        &frame[l..l + c],
        &frame[l + c..l + 2 * c],
        &frame[l + 2 * c..2 * l + 2 * c],
    ]
}

/// Branch `i` of a frame that stacks canvases of `canvas` side by side (a frame that is not stacked is one branch): its four
/// planes.
fn unstack(planes: &[&[u8]; 4], i: usize, canvas: (usize, usize)) -> Placed {
    let (cw, ch) = canvas;
    let take = |plane: &[u8], w: usize, h: usize| -> Vec<u8> {
        let stride = plane.len() / h.max(1);
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            out.extend_from_slice(&plane[row * stride + i * w..row * stride + (i + 1) * w]);
        }
        out
    };
    Placed {
        y: take(planes[0], cw, ch),
        u: take(planes[1], cw / 2, ch / 2),
        v: take(planes[2], cw / 2, ch / 2),
        a: take(planes[3], cw, ch),
    }
}

/// What differs between FFmpeg's `got` and the model's placement of `noise` by `pose`, or `None` when nothing does.
fn differs(got: &Placed, noise: &Noise, canvas: (u32, u32), pose: &Pose) -> Option<String> {
    let frame = PlacementFrame::new(noise.size, canvas);
    let map = match PlacementMap::new(frame, pose) {
        Ok(map) => map,
        // A pose the model refuses is no pose of the probe's to compare: it is a bug in the probe, and says so.
        Err(e) => return Some(format!("the model refuses the probe's own pose: {e}")),
    };
    let want = reference(&map, &noise.picture());
    let off = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(x, y)| x != y).count();
    let counts = [
        off(&got.y, &want.y),
        off(&got.u, &want.u),
        off(&got.v, &want.v),
        off(&got.a, &want.a),
    ];
    if counts == [0; 4] {
        return None;
    }
    let max = [(&got.y, &want.y), (&got.u, &want.u), (&got.v, &want.v), (&got.a, &want.a)]
        .iter()
        .flat_map(|(a, b)| a.iter().zip(b.iter()).map(|(x, y)| x.abs_diff(*y)))
        .max()
        .unwrap_or(0);
    Some(format!(
        "{} samples differ from the model (Y/U/V/A {counts:?}, max {max})",
        counts.iter().sum::<usize>()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe's own bookkeeping, with no FFmpeg: a stack of pictures is cut back into the pictures it was made of, and
    /// the model's refusal of a pose is reported rather than hidden.
    #[test]
    fn a_stack_is_cut_back_into_its_branches() {
        let (cw, ch, n) = (4usize, 2usize, 3usize);
        // Branch b's sample at (x, y) is b*100 + y*10 + x in luma and alpha, +5 in chroma.
        let build = |w: usize, h: usize, add: usize| -> Vec<u8> {
            let mut p = Vec::new();
            for y in 0..h {
                for b in 0..n {
                    for x in 0..w {
                        p.push((b * 100 + y * 10 + x + add) as u8);
                    }
                }
            }
            p
        };
        let (y, u, v, a) = (
            build(cw, ch, 0),
            build(cw / 2, ch / 2, 5),
            build(cw / 2, ch / 2, 6),
            build(cw, ch, 1),
        );
        let mut frame = Vec::new();
        for plane in [&y, &u, &v, &a] {
            frame.extend_from_slice(plane);
        }
        let planes = split_planes(&frame, (cw * n, ch), (cw * n / 2, ch / 2));
        for b in 0..n {
            let got = unstack(&planes, b, (cw, ch));
            let at = |add: usize, w: usize, h: usize| -> Vec<u8> {
                (0..h)
                    .flat_map(|y| (0..w).map(move |x| (b * 100 + y * 10 + x + add) as u8))
                    .collect()
            };
            assert_eq!(got.y, at(0, cw, ch), "branch {b}");
            assert_eq!(got.u, at(5, cw / 2, ch / 2), "branch {b}");
            assert_eq!(got.v, at(6, cw / 2, ch / 2), "branch {b}");
            assert_eq!(got.a, at(1, cw, ch), "branch {b}");
        }
        // One frame is one branch.
        let one: Vec<u8> = (0..(cw * ch * 2 + 2 * (cw / 2) * (ch / 2)) as u8).collect();
        let planes = split_planes(&one, (cw, ch), (cw / 2, ch / 2));
        let got = unstack(&planes, 0, (cw, ch));
        assert_eq!(got.y, &one[..cw * ch]);
        assert_eq!(got.a, &one[cw * ch + 2 * (cw / 2) * (ch / 2)..]);
    }

    /// Every graph the probe hands FFmpeg stays within the engine's argv rule, so that no run of it is a 31 KB command line, and
    /// the runs of a shape cover its poses once each, in order.
    #[test]
    fn no_run_of_the_probe_is_a_command_line_over_the_engines_argv_rule() {
        let mut most = 0;
        let mut runs_in_all = 0;
        for (canvas, _, full) in SHAPES {
            let cases = poses(canvas, full);
            let chains = chains(&cases, canvas);
            let cut = runs(&chains);
            runs_in_all += cut.len();
            let covered: Vec<usize> = cut.iter().flat_map(|r| r.clone()).collect();
            assert_eq!(
                covered,
                (0..cases.len()).collect::<Vec<_>>(),
                "{canvas:?}: every pose once, in order"
            );
            for run in cut {
                let graph = stack_graph(&chains[run.clone()]);
                assert!(
                    graph.len() <= kerf_core::GRAPH_ARG_MAX,
                    "{canvas:?} {run:?}: a {}-byte graph",
                    graph.len()
                );
                assert!(graph.contains("perspective"), "the export's own chain");
                most = most.max(graph.len());
            }
        }
        // (The poses were one 31 KB graph per shape; the motion probe's `-vf` chain is one pose.)
        assert!(
            most > kerf_core::GRAPH_ARG_MAX / 2,
            "the runs are not cut finer than they need to be: {most}"
        );
        assert!(runs_in_all < 40, "{runs_in_all} runs");
        let motion = kerf_core::placement_probe_motion(MOTION_FRAMES);
        let chain = kerf_core::placement_probe_chain(Some(&motion.scale), (&motion.pos.0, &motion.pos.1), (64, 36), true);
        assert!(chain.len() < kerf_core::GRAPH_ARG_MAX, "{} bytes", chain.len());
        // A chain over the rule alone is a run of its own, and the others are packed around it.
        let big = vec!["x".repeat(kerf_core::GRAPH_ARG_MAX + 1)];
        assert_eq!(runs(&big), vec![0..1]);
        let mixed = vec!["a".to_string(), "x".repeat(kerf_core::GRAPH_ARG_MAX + 1), "b".to_string()];
        assert_eq!(runs(&mixed), vec![0..1, 1..2, 2..3]);
    }

    /// Every pose the probe tries is one the model takes, so that "the model refuses it" is never what a `Differs` says.
    #[test]
    fn every_pose_the_probe_tries_is_one_the_model_draws() {
        for (canvas, input, full) in SHAPES {
            assert!(PlacementFrame::exact_for(input), "{input:?}: an odd height is not reproduced");
            for c in poses(canvas, full) {
                let frame = PlacementFrame::new(input, canvas);
                PlacementMap::new(frame, &c.pose).unwrap_or_else(|e| panic!("{canvas:?} <- {input:?} {:?}: {e}", c.pose));
            }
        }
        let motion = kerf_core::placement_probe_motion(MOTION_FRAMES);
        assert_eq!(motion.poses.len(), MOTION_FRAMES as usize);
        for (scale, pos) in &motion.poses {
            let frame = PlacementFrame::new((64, 36), (64, 36));
            let pose = Pose {
                scale: Some(*scale),
                pos: *pos,
            };
            PlacementMap::new(frame, &pose).unwrap_or_else(|e| panic!("{pose:?}: {e}"));
        }
        // And a probe tries a tie, an odd width and a pose off the frame at all.
        let all: Vec<Case> = poses((64, 32), true);
        assert!(all.iter().any(|c| c.pose.pos == (1.0 / (512.0 * 64.0), 0.0)));
        assert!(all.iter().any(|c| c.pose.pos.0.abs() > 1.0));
        assert!(all.iter().any(|c| c.per_frame) && all.iter().any(|c| !c.per_frame));
        assert!(SHAPES.iter().any(|(_, input, _)| input.0 % 2 == 1));
    }

    /// The probe on the FFmpeg in use (`#[ignore]`d: it needs one). It must be able to tell, and say the same twice.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn this_ffmpeg_places_a_picture_as_the_model_does_or_the_probe_says_where_not() {
        let started = Instant::now();
        let first = probe(&Budget::new(&TIMING));
        eprintln!(
            "placement probe: {first:?} in {:.0} ms",
            started.elapsed().as_secs_f64() * 1e3
        );
        assert_ne!(first, Verdict::Unknown, "the probe could not run");
        assert_eq!(first, probe(&Budget::new(&TIMING)));
    }
}
