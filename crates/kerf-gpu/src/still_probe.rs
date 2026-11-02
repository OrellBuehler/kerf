//! Whether this FFmpeg draws a moving still ([`kerf_core::still_motion`]) as the compositor's model does, per kind of picture
//! — measured once per process on the export's own graph and the still's, never assumed and never keyed on a version string.
//! `GpuCaps::still_motion` holds the kinds that matched.
//!
//! The compositor does not scale the picture itself: it asks FFmpeg for the copy the graph makes of the file
//! (`scale` to the copy's size with the delivery's scaler, `crop` to the window, converted to `yuva420p`: [`crate::source`]),
//! and draws the `perspective` that moves it from its own tables ([`kerf_core::perspective`]). What this measures is
//! therefore everything round that: that the filters **in the graph** make the copy the standalone decode makes (the
//! negotiation that gives the scaler its output format, the range and RGB conversion done inside it), that the corners
//! the plan evaluates are those the graph's expressions do (the pose of a frame, the frame count `perspective` has,
//! `eval=frame` re-evaluating), and that the table the model builds from them is the filter's. A toy cut of a noise
//! picture of the kind (a JPEG for `yuvj420p`, a PNG for `rgb24`; a landscape and a portrait one) is exported by the engine's
//! own graph ([`kerf_core::export_graph`]) and drawn as the FFmpeg still ([`kerf_core::still_graph`]), and every plane of
//! every frame is held to the model with no tolerance. The toy cut is drawn two ways: **inside the frame** (the moving still's own
//! route, one `perspective` of a scaled copy) and **off the picture's edge** (a move that leaves the frame bare: the graph's fit,
//! then the placement, with FFmpeg making the fit picture for the compositor, [`kerf_core::FitCopy`]). Any difference is [`Verdict::Differs`] for that kind (it names
//! the first), a run that fails, writes the wrong amount or runs out of its deadline is [`Verdict::Unknown`]: asked again
//! after a backoff, and the kind is not in the caps until it answers. One probe runs at a time per kind, whoever asks
//! ([`crate::measure`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kerf_core::perspective::{PlacementFrame, PlacementMap};
use kerf_core::{
    Clip, Delivery, ExportOptions, Fit, Keyframe, OriginalMedia, PlanRequest, Planner, Project, RenderPlan, StillKinds,
    StreamKind, Timeline, Track,
};

use crate::measure::{self, Budget, Probe, Timing, Verdict};
use crate::placement::{reference, Picture};

/// One FFmpeg run is bounded at 20 s, the whole probe of a kind (a dozen runs of tens of milliseconds) at 45 s, and a probe that
/// could not tell is not asked again before 30 s.
const TIMING: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(45),
    per_run: Duration::from_secs(20),
};

static JPEG: Probe = Probe::new();
static RGB: Probe = Probe::new();

/// The canvas of the toy cut.
const CANVAS: (u32, u32) = (64, 36);
/// The frames of the toy cut's export.
const FRAMES: u32 = 12;
const FPS: f64 = 30.0;

/// What is known without running anything: the kinds measured to match.
pub fn measured() -> StillKinds {
    let mut kinds = StillKinds::NONE;
    if measure::known(&JPEG) == Verdict::Match {
        kinds = kinds.union(StillKinds::YUVJ420P);
    }
    if measure::known(&RGB) == Verdict::Match {
        kinds = kinds.union(StillKinds::RGB24);
    }
    kinds
}

/// The kinds that match, measured on the first call (a dozen short `ffmpeg` children a kind, one after the other, each bounded by
/// 20 s and a kind by 45 s): a blocking thread's, ahead of the frame that needs it ([`crate::prepare`]). A caller that arrives
/// while a probe runs waits for it and takes its answer.
pub fn kinds() -> StillKinds {
    let _ = jpeg();
    let _ = rgb();
    measured()
}

/// The verdict for a JPEG (`yuvj420p`).
pub fn jpeg() -> Verdict {
    measure::cached(&JPEG, Instant::now(), TIMING, |budget| probe(budget, Kind::Jpeg))
}

/// The verdict for an RGB picture (a PNG, `rgb24`).
pub fn rgb() -> Verdict {
    measure::cached(&RGB, Instant::now(), TIMING, |budget| probe(budget, Kind::Rgb))
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Jpeg,
    Rgb,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Jpeg => "yuvj420p",
            Self::Rgb => "rgb24",
        }
    }
}

/// The toy cut's pictures: a landscape one and a portrait one, whose Cover fit crops a different axis, and (for the placement's fit)
/// a small one the fit enlarges.
const SHAPES: [(u32, u32); 3] = [(96, 64), (64, 96), (40, 28)];

/// How the toy cut's move takes the picture through the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// In from 1.3x to just over the frame: nothing hangs off it ([`kerf_core::still_motion`]).
    Inside,
    /// From the frame's own size, panned off its edge: the general chain's fit and placement.
    Edge,
}

/// A scratch directory of the probe's own, removed when it is dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(kind: Kind) -> Option<Self> {
        let dir = std::env::temp_dir().join(format!("kerf-still-probe-{}-{}", std::process::id(), kind.name()));
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn probe(budget: &Budget, kind: Kind) -> Verdict {
    let Some(scratch) = Scratch::new(kind) else {
        return Verdict::Unknown;
    };
    let policy = kerf_core::composite_color_policy();
    if policy == kerf_core::CompositeColorPolicy::Unknown {
        // The colour policy is part of the plan the model is built from: not known, not judged.
        return Verdict::Unknown;
    }
    for (n, shape) in SHAPES.into_iter().enumerate() {
        for route in [Route::Inside, Route::Edge] {
            // (The small picture is the fit's: a move inside the frame wants a picture the canvas does not enlarge past recognition.)
            if route == Route::Inside && shape.0 < 64 {
                continue;
            }
            match shape_matches(budget, &scratch.0, kind, shape, route, n, policy) {
                Ok(None) => {}
                Ok(Some(difference)) => {
                    return Verdict::Differs(format!("{} {}x{} {route:?}: {difference}", kind.name(), shape.0, shape.1));
                }
                Err(()) => return Verdict::Unknown,
            }
        }
    }
    Verdict::Match
}

/// The asset an import would make of `file`, within the probe's deadline: the engine's `ffprobe` reads have timeouts of their own, but not
/// the probe's one, so it is asked on a thread that is left behind if the deadline comes first.
fn probed(budget: &Budget, file: &Path) -> Result<kerf_core::Asset, ()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let file = file.to_path_buf();
    std::thread::Builder::new()
        .name("kerf-still-probe-asset".into())
        .spawn(move || {
            let _ = tx.send(Project::probe_asset(&file));
        })
        .map_err(|_| ())?;
    rx.recv_timeout(budget.remaining()).map_err(|_| ())?.map_err(|_| ())
}

/// The graph that makes a noise picture of the kind: a gradient with noise over it, smooth where a codec keeps it and busy where a
/// scaler shows its taps.
fn noise_graph(kind: Kind, (w, h): (u32, u32)) -> String {
    let busy = "(X*255/W+Y*255/H)/2+random(1)*70-35";
    match kind {
        Kind::Jpeg => format!(
            "nullsrc=size={w}x{h}:rate=1:duration=1,format=rgb24,geq=r='clip({busy},0,255)':g='clip(255-X*255/W+random(2)*60-30,0,255)':b='clip(Y*255/H+random(3)*90-45,0,255)'"
        ),
        Kind::Rgb => format!(
            "nullsrc=size={w}x{h}:rate=1:duration=1,format=rgb24,geq=r='random(1)*255':g='clip({busy},0,255)':b='random(3)*255'"
        ),
    }
}

/// The toy cut: one still on the slideshow's own move (in from 1.3x near the top left to just over the frame), Cover into a small
/// 16:9 canvas at 30 fps.
fn toy(asset: &kerf_core::Asset, route: Route) -> (Timeline, ExportOptions) {
    let end = f64::from(FRAMES) / FPS;
    let mut c = Clip::new(asset.id, 0.0, end, 0.0);
    let key = |time: f64, scale: f64, pos_x: f64, pos_y: f64| Keyframe {
        easing: Default::default(),
        time,
        scale,
        pos_x,
        pos_y,
        rotation: 0.0,
        opacity: 1.0,
    };
    c.keyframes = match route {
        Route::Inside => vec![key(0.0, 1.3, 0.03, 0.02), key(end, 1.05, -0.01, 0.0)],
        Route::Edge => vec![key(0.0, 1.0, -0.04, 0.0), key(end, 1.2, 0.04, -0.02)],
    };
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![c],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(CANVAS.0, CANVAS.1, Fit::Cover).with_fps(Some(FPS)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    (
        tl,
        ExportOptions {
            fps: Some(FPS),
            ..ExportOptions::default()
        },
    )
}

/// `Ok(None)`: every plane of every frame of the export and of the still is the model's. `Ok(Some(why))`: the first
/// difference. `Err`: the probe could not tell.
fn shape_matches(
    budget: &Budget,
    dir: &Path,
    kind: Kind,
    shape: (u32, u32),
    route: Route,
    n: usize,
    policy: kerf_core::CompositeColorPolicy,
) -> Result<Option<String>, ()> {
    let ext = match kind {
        Kind::Jpeg => "jpg",
        Kind::Rgb => "png",
    };
    let file = dir.join(format!("still{n}-{route:?}.{ext}"));
    let path = file.to_str().ok_or(())?;
    let graph = noise_graph(kind, shape);
    let mut args = vec!["-f", "lavfi", "-i", &graph, "-frames:v", "1"];
    match kind {
        Kind::Jpeg => args.extend(["-pix_fmt", "yuvj420p", "-q:v", "2"]),
        Kind::Rgb => args.extend(["-pix_fmt", "rgb24"]),
    }
    args.push(path);
    budget.ffmpeg(&args, Vec::new()).ok_or(())?;
    let asset = probed(budget, &file)?;
    let (tl, opts) = toy(&asset, route);
    let assets = std::slice::from_ref(&asset);
    let (cw, ch) = (CANVAS.0 as usize, CANVAS.1 as usize);
    let frame_len = cw * ch * 3 / 2;

    // The export: the engine's own graph over the file as an input, every frame as planes.
    let graph = kerf_core::export_graph(&tl, assets, &opts).map_err(|_| ())?.ok_or(())?;
    let fps = format!("{FPS}");
    let frames = FRAMES.to_string();
    let raw = budget
        .ffmpeg(
            &[
                "-framerate",
                &fps,
                "-i",
                path,
                "-filter_complex",
                &graph,
                "-map",
                "[outv]",
                "-frames:v",
                &frames,
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "pipe:1",
            ],
            Vec::new(),
        )
        .ok_or(())?;
    if raw.len() != frame_len * FRAMES as usize {
        return Err(());
    }
    let planner = Planner::new(&tl, assets, &opts, PlanRequest::motion(policy).with_media(&OriginalMedia)).map_err(|_| ())?;
    let mut copy = None;
    for k in 0..u64::from(FRAMES) {
        let plan = planner.at_frame(k).map_err(|_| ())?;
        let want = &raw[k as usize * frame_len..(k as usize + 1) * frame_len];
        if let Some(why) = differs(budget, &plan, want, route, &mut copy)? {
            return Ok(Some(format!("export frame {k}: {why}")));
        }
    }

    // The FFmpeg still, mid-move.
    let t = 0.5 * f64::from(FRAMES) / FPS;
    let graph = kerf_core::still_graph(&tl, assets, &opts, t).map_err(|_| ())?.ok_or(())?;
    let raw = budget
        .ffmpeg(
            &[
                "-i",
                path,
                "-filter_complex",
                &graph,
                "-map",
                "[outv]",
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
        .ok_or(())?;
    if raw.len() != frame_len {
        return Err(());
    }
    let planner = Planner::new(&tl, assets, &opts, PlanRequest::still(policy).with_media(&OriginalMedia)).map_err(|_| ())?;
    let plan = planner.at(t).map_err(|_| ())?.for_render(CANVAS);
    let mut copy = None;
    if let Some(why) = differs(budget, &plan, &raw, route, &mut copy)? {
        return Ok(Some(format!("still: {why}")));
    }
    Ok(None)
}

/// The frame the model draws for `plan` held to `want` (Y, U, V of the canvas): `Ok(None)` when they are the same, else what
/// differs. The copy FFmpeg makes of the file is asked for once and kept in `copy` for the frames that share it.
fn differs(
    budget: &Budget,
    plan: &RenderPlan,
    want: &[u8],
    route: Route,
    copy: &mut Option<(String, Vec<u8>)>,
) -> Result<Option<String>, ()> {
    let [layer] = plan.layers.as_slice() else {
        return Ok(Some(format!("{} layers where the toy cut has one", plan.layers.len())));
    };
    let inside = plan.takes_still_route(layer);
    if inside != (route == Route::Inside) || (route == Route::Edge && !plan.fits_by_ffmpeg(layer)) {
        return Ok(Some(format!(
            "the plan takes the {} route for a move {route:?}",
            if inside { "moving still's" } else { "placement's" }
        )));
    }
    let Some(made) = crate::source::copy_of(layer).map_err(|_| ())? else {
        return Ok(Some("the plan has no copy for FFmpeg to make".into()));
    };
    let (ww, wh) = made.size;
    let planes = match copy {
        Some((have, planes)) if *have == made.filters => planes.clone(),
        _ => {
            let args = crate::source::still_copy_args(layer).map_err(|_| ())?;
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let raw = budget.ffmpeg(&args, Vec::new()).ok_or(())?;
            let (w, h) = (ww as usize, wh as usize);
            if raw.len() != w * h * 5 / 2 {
                return Err(());
            }
            let planes = raw[..w * h * 3 / 2].to_vec();
            *copy = Some((made.filters, planes.clone()));
            planes
        }
    };
    let (luma, chroma) = ((ww * wh) as usize, (ww / 2 * (wh / 2)) as usize);
    let picture = Picture {
        size: (ww, wh),
        y: &planes[..luma],
        u: &planes[luma..luma + chroma],
        v: &planes[luma + chroma..],
        a: None,
        opaque: 255,
    };
    let canvas = (plan.canvas.width, plan.canvas.height);
    let map = match route {
        Route::Inside => {
            let (Some(still), Some(motion)) = (layer.still.as_ref(), layer.still.as_ref().and_then(|s| s.export.as_ref())) else {
                return Ok(Some("the plan has no move for the render".into()));
            };
            PlacementMap::from_corners(PlacementFrame::window((ww, wh), motion.frame), &motion.corners(&still.pose))
        }
        Route::Edge => {
            let Some(pose) = plan.pose(layer) else {
                return Ok(Some("the plan places no pose".into()));
            };
            if !PlacementFrame::exact_for((ww, wh)) {
                return Ok(Some("a fit picture the placement does not take".into()));
            }
            PlacementMap::new(PlacementFrame::new((ww, wh), canvas), &pose)
        }
    };
    let map = match map {
        Ok(map) => map,
        Err(error) => return Ok(Some(format!("no table for the move: {error}"))),
    };
    let placed = reference(&map, &picture);
    // The layer is laid over black by `overlay`: a sample the layer covers (alpha 255 over its 2x2 block) is the layer's, one it does not
    // (alpha 0) is the base's, and the samples in between are the blend's — the edge of a picture panned off the frame, which the
    // compositor's blend pass is held to elsewhere and this does not repeat.
    let (cw, ch) = (canvas.0 as usize, canvas.1 as usize);
    let (cl, cc) = (cw * ch, (cw / 2) * (ch / 2));
    let mut differing = [0usize; 3];
    for ((alpha, luma), seen) in placed.a.iter().zip(&placed.y).zip(&want[..cl]) {
        let expected = match *alpha {
            255 => *luma,
            0 => 16,
            _ => continue,
        };
        differing[0] += usize::from(expected != *seen);
    }
    for cy in 0..ch / 2 {
        for cx in 0..cw / 2 {
            let block = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(dx, dy)| placed.a[(2 * cy + dy) * cw + 2 * cx + dx]);
            let covered = block.iter().all(|a| *a == 255);
            if !covered && block.iter().any(|a| *a != 0) {
                continue;
            }
            let at = cy * (cw / 2) + cx;
            for (n, (plane, base)) in [(&placed.u, 128u8), (&placed.v, 128u8)].into_iter().enumerate() {
                let expected = if covered { plane[at] } else { base };
                differing[1 + n] += usize::from(expected != want[cl + n * cc + at]);
            }
        }
    }
    for (name, count) in ["Y", "U", "V"].into_iter().zip(differing) {
        if count != 0 {
            return Ok(Some(format!("{count} samples of the {name} plane")));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_in_the_caps_before_a_probe_has_run() {
        assert_eq!(measured(), StillKinds::NONE);
    }
}
