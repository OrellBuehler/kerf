//! Whether this FFmpeg draws a moving still ([`kerf_core::still_motion`]) as the compositor's model does, per kind of picture and
//! per chain — measured once per process on the export's own graph and the still's, never assumed and never keyed on a version
//! string. `GpuCaps::still_motion` holds the chains of each kind that matched.
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
//! every frame is held to the model with no tolerance **where the layer's alpha is settled** (0 or 255; for the chroma, a block
//! the picture covers entirely or not at all): the samples at the edge of a picture panned off the frame are `overlay`'s blend
//! of the placement's alpha ramp over the base, which is the placement's and the blend's, not the copy's, and which the parity
//! cases hold to FFmpeg's frame sample for sample (`a_move_that_pans_off_the_picture_*`, `checker_a_placement`).
//!
//! **A chain is a thing of its own** ([`kerf_core::StillChain`]): the filter text FFmpeg is given for the copy has the shapes of
//! the moving still's own route (one `scale`, one `crop`), of the engine's fit of a Cover frame (behind the clip's static `crop`
//! or not) and of the geometry's stages after a guard (a Contain frame, a held zoom), each with the delivery's scaler named or
//! not. The probe of a kind first measures its two **base** chains (the move inside the frame and the engine's fit, the default
//! scaler: what a plain Ken Burns slideshow takes), and every other chain is measured when a plan needs it ([`ensure`]), by a
//! toy cut that takes that chain and no other (the plan of each toy is asked which chain it takes, and a toy that takes another
//! is a difference). Any difference is [`Verdict::Differs`] for that chain (it names the first), a run that fails, writes the
//! wrong amount or runs out of its deadline is [`Verdict::Unknown`]: asked again after a backoff, and the chain is not in the
//! caps until it answers. One probe runs at a time per slot, whoever asks ([`crate::measure`]).
//!
//! **The move inside the frame under a named scaler** (`bilinear`, `lanczos`, `neighbor`, `spline`: `:flags=<name>` in every `scale` of the
//! text) is measured the same way, one slot per kind and name ([`verdict_route`]): the toy cuts of [`StillChain::inside`] exported with
//! `scaler: <name>`, the plan of each asked that its layer takes the route under that name
//! ([`RenderPlan::still_route_scaler`]). The compositor does not scale anything for it either (FFmpeg makes the copy, with the name in
//! its text); what is measured is that the graph's chain makes the copy the standalone decode makes and the `perspective` is the model's. A
//! match is [`StillKinds::named_route`] in the caps. The fit pictures of a placement are not measured under a name.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kerf_core::perspective::{PlacementFrame, PlacementMap};
use kerf_core::{
    Clip, Delivery, ExportOptions, Fit, FitShape, Keyframe, OriginalMedia, PlanRequest, Planner, Project, RenderPlan,
    ScalerKinds, StillChain, StillKinds, StreamKind, Timeline, Track,
};

use crate::measure::{self, Budget, Probe, Timing, Verdict};
use crate::placement::{reference, Picture};

/// One FFmpeg run is bounded at 20 s, the whole probe of a slot (a dozen runs of tens of milliseconds) at 45 s, and a probe that
/// could not tell is not asked again before 30 s.
const TIMING: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(45),
    per_run: Duration::from_secs(20),
};

/// The kinds of picture that have a probe.
const KINDS: [Kind; 2] = [Kind::Jpeg, Kind::Rgb];

/// How many slots a kind has: its base chains together, then each other chain on its own.
const SLOTS: usize = StillChain::COUNT - 1;

/// One slot per kind and chain group, each measured once: `[kind][slot]`.
static PROBES: [[Probe; SLOTS]; 2] = [[const { Probe::new() }; SLOTS], [const { Probe::new() }; SLOTS]];

/// The scaler names a delivery can give besides `bicubic` (the engine's `SCALERS`), as the caps and the text spell them: the move inside the
/// frame is measured under each.
const ROUTE_NAMES: [(ScalerKinds, &str); 4] = [
    (ScalerKinds::BILINEAR, "bilinear"),
    (ScalerKinds::LANCZOS, "lanczos"),
    (ScalerKinds::NEIGHBOR, "neighbor"),
    (ScalerKinds::SPLINE, "spline"),
];

/// One slot per kind and name, each measured once: `[kind][name]` (the order of [`ROUTE_NAMES`]).
static ROUTE_PROBES: [[Probe; ROUTE_NAMES.len()]; 2] = [
    [const { Probe::new() }; ROUTE_NAMES.len()],
    [const { Probe::new() }; ROUTE_NAMES.len()],
];

/// The canvas of the toy cut.
const CANVAS: (u32, u32) = (64, 36);
/// The frames of the toy cut's export.
const FRAMES: u32 = 12;
const FPS: f64 = 30.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    fn index(self) -> usize {
        match self {
            Self::Jpeg => 0,
            Self::Rgb => 1,
        }
    }

    fn set(self) -> StillKinds {
        match self {
            Self::Jpeg => StillKinds::YUVJ420P,
            Self::Rgb => StillKinds::RGB24,
        }
    }

    fn of(kinds: StillKinds) -> Option<Self> {
        KINDS.into_iter().find(|k| k.set() == kinds)
    }
}

/// Which slot measures `chain`: the two base chains share slot 0, every other chain has one.
fn slot_of(chain: StillChain) -> usize {
    debug_assert!(chain.measurable(), "{chain:?} has no slot: no toy cut takes it");
    if chain.is_base() {
        0
    } else {
        chain.index() - 1
    }
}

/// The chains a slot answers for.
fn chains_of(slot: usize) -> Vec<StillChain> {
    StillChain::ALL.into_iter().filter(|c| slot_of(*c) == slot).collect()
}

/// What is known without running anything: the chains measured to match, of every kind.
pub fn measured() -> StillKinds {
    let mut set = StillKinds::NONE;
    for kind in KINDS {
        for (slot, probe) in PROBES[kind.index()].iter().enumerate() {
            if measure::known(probe) == Verdict::Match {
                for chain in chains_of(slot) {
                    set = set.union(kind.set().chain(chain));
                }
            }
        }
        for ((name, _), probe) in ROUTE_NAMES.iter().zip(&ROUTE_PROBES[kind.index()]) {
            if measure::known(probe) == Verdict::Match {
                set = set.union(kind.set().named_route(*name));
            }
        }
    }
    set
}

/// The verdict for `chain` of `kind` (a kind constant, [`StillKinds::YUVJ420P`] or [`StillKinds::RGB24`]), measured on the first call;
/// `Unknown` for a kind with no probe.
pub fn verdict(kind: StillKinds, chain: StillChain) -> Verdict {
    // (A chain no toy cut takes, [`StillChain::measurable`], is never measured: not known, and never a match.)
    let Some(kind) = Kind::of(kind).filter(|_| chain.measurable()) else {
        return Verdict::Unknown;
    };
    let slot = slot_of(chain);
    measure::cached(&PROBES[kind.index()][slot], Instant::now(), TIMING, |budget| {
        probe(budget, kind, &chains_of(slot))
    })
}

/// The verdict for the move inside the frame of `kind` under the scaler `name` (one of [`ROUTE_NAMES`]), measured on the first call;
/// `Unknown` for a kind with no probe and for a name that is not one of the four.
pub fn verdict_route(kind: StillKinds, name: ScalerKinds) -> Verdict {
    let (Some(kind), Some(at)) = (Kind::of(kind), ROUTE_NAMES.iter().position(|(n, _)| *n == name)) else {
        return Verdict::Unknown;
    };
    measure::cached(&ROUTE_PROBES[kind.index()][at], Instant::now(), TIMING, |budget| {
        probe_route(budget, kind, at)
    })
}

/// The routes `needs` names (a kind and the scaler the delivery names), measured after the kind's base chains: a blocking thread's, off
/// any lock, ahead of the frame that needs them ([`crate::prepare`]). Returns what is measured.
pub fn ensure_routes(needs: &[(StillKinds, ScalerKinds)]) -> StillKinds {
    for &(kind, name) in needs {
        let _ = verdict(kind, StillChain::inside(false));
        let _ = verdict_route(kind, name);
    }
    measured()
}

/// The chains of the kinds `needs` names, measured (each kind's base chains first, then the chain asked for, a slot once): a blocking
/// thread's, off any lock, ahead of the frame that needs them ([`crate::prepare`]). Nothing else is measured: a project of PNGs does not
/// pay for the JPEGs' probe, nor a plain slideshow for a crop's. Returns what is measured.
pub fn ensure(needs: &[(StillKinds, StillChain)]) -> StillKinds {
    for &(kind, chain) in needs.iter().filter(|(_, chain)| chain.measurable()) {
        let _ = verdict(kind, StillChain::inside(false));
        if !chain.is_base() {
            let _ = verdict(kind, chain);
        }
    }
    measured()
}

/// The base chains of both kinds, measured on the first call (a dozen short `ffmpeg` children a kind, one after the other, each bounded by
/// 20 s and a slot by 45 s). A caller that arrives while a probe runs waits for it and takes its answer.
pub fn kinds() -> StillKinds {
    let _ = jpeg();
    let _ = rgb();
    measured()
}

/// Every chain of both kinds and every route under a named scaler, measured (a self-test's and a ledger's: [`ensure`] and [`ensure_routes`]
/// measure only what a plan needs).
pub fn all() -> StillKinds {
    for kind in KINDS {
        for chain in StillChain::ALL {
            let _ = verdict(kind.set(), chain);
        }
        for (name, _) in ROUTE_NAMES {
            let _ = verdict_route(kind.set(), name);
        }
    }
    measured()
}

/// The verdict for a JPEG's base chains (`yuvj420p`).
pub fn jpeg() -> Verdict {
    verdict(StillKinds::YUVJ420P, StillChain::inside(false))
}

/// The verdict for an RGB picture's base chains (a PNG, `rgb24`).
pub fn rgb() -> Verdict {
    verdict(StillKinds::RGB24, StillChain::inside(false))
}

/// The toys' pictures, by `Case::shape`: 0 a landscape and 1 a portrait one (a Cover fit crops a different axis of each, a Contain frame
/// letterboxes one and pillarboxes the other), 2 a small one the engine's fit enlarges, 3 one of the canvas' own shape (a held zoom out under
/// Cover is a second stage only for a picture the cover does not hang over the frame, else the graph fits and zooms in one `scale`, which the
/// plan refuses).
const SHAPES: [(u32, u32); 4] = [(96, 64), (64, 96), (40, 28), (128, 72)];

/// One toy cut of a chain: the picture's shape, the frame's fit and a zoom the clip holds (a held zoom is a second stage of the fit).
#[derive(Debug, Clone, Copy)]
struct Case {
    shape: usize,
    fit: Fit,
    held: Option<f64>,
}

/// The toy cuts that take `chain`. A move inside the frame wants a picture the canvas does not enlarge past recognition; the staged fit is
/// a Contain frame (the picture alone, with bars) and a held zoom out (a second stage).
fn cases(chain: StillChain) -> Vec<Case> {
    let cover = |shape| Case {
        shape,
        fit: Fit::Cover,
        held: None,
    };
    match chain.fit {
        None => vec![cover(0), cover(1)],
        Some(FitShape::Engine | FitShape::EngineCrop) => vec![cover(0), cover(1), cover(2)],
        // (No toy cut takes a text with a `crop` of its own: that chain is not measured.)
        Some(FitShape::Cropped) => Vec::new(),
        Some(FitShape::Staged) => vec![
            Case {
                shape: 0,
                fit: Fit::Contain,
                held: None,
            },
            Case {
                shape: 1,
                fit: Fit::Contain,
                held: None,
            },
            Case {
                shape: 3,
                fit: Fit::Cover,
                held: Some(0.5),
            },
        ],
    }
}

/// A scratch directory of the probe's own, removed when it is dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(kind: Kind, slot: usize) -> Option<Self> {
        let dir = std::env::temp_dir().join(format!("kerf-still-probe-{}-{}-{slot}", std::process::id(), kind.name()));
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn probe(budget: &Budget, kind: Kind, chains: &[StillChain]) -> Verdict {
    let slot = chains.first().map_or(0, |c| slot_of(*c));
    let subjects: Vec<Subject> = chains.iter().map(|c| Subject::Chain(*c)).collect();
    probe_subjects(budget, kind, slot, &subjects)
}

/// The move inside the frame of `kind` under the scaler `ROUTE_NAMES[at]`.
fn probe_route(budget: &Budget, kind: Kind, at: usize) -> Verdict {
    let (name, text) = ROUTE_NAMES[at];
    probe_subjects(budget, kind, SLOTS + at, &[Subject::Route(name, text)])
}

/// What a toy cut takes: a chain (the delivery's scaler is the default one or `bicubic`, as the chain says) or the move inside the frame under
/// a scaler named by its text.
#[derive(Debug, Clone, Copy)]
enum Subject {
    Chain(StillChain),
    Route(ScalerKinds, &'static str),
}

impl Subject {
    /// The chain whose text the toy takes: the route's own for a named scaler.
    fn chain(self) -> StillChain {
        match self {
            Self::Chain(c) => c,
            Self::Route(..) => StillChain::inside(false),
        }
    }

    /// The scaler the export names.
    fn scaler(self) -> Option<String> {
        match self {
            Self::Chain(c) => c.bicubic.then(|| "bicubic".to_string()),
            Self::Route(_, name) => Some(name.to_string()),
        }
    }
}

fn probe_subjects(budget: &Budget, kind: Kind, slot: usize, subjects: &[Subject]) -> Verdict {
    let Some(scratch) = Scratch::new(kind, slot) else {
        return Verdict::Unknown;
    };
    let policy = kerf_core::composite_color_policy();
    if policy == kerf_core::CompositeColorPolicy::Unknown {
        // The colour policy is part of the plan the model is built from: not known, not judged.
        return Verdict::Unknown;
    }
    let mut n = 0;
    for &subject in subjects {
        let chain = subject.chain();
        for case in cases(chain) {
            n += 1;
            match shape_matches(budget, &scratch.0, kind, subject, case, n, policy) {
                Ok(None) => {}
                Ok(Some(difference)) => {
                    let (w, h) = SHAPES[case.shape];
                    return Verdict::Differs(format!(
                        "{} {w}x{h} {subject:?} ({:?}{}): {difference}",
                        kind.name(),
                        case.fit,
                        case.held.map_or(String::new(), |h| format!(", held zoom {h}"))
                    ));
                }
                Err(()) => return Verdict::Unknown,
            }
        }
    }
    Verdict::Match
}

/// The asset an import would make of `file`, within the probe's deadline: the engine's `ffprobe` read is polled for the deadline and
/// killed with it.
fn probed(budget: &Budget, file: &Path) -> Result<kerf_core::Asset, ()> {
    Project::probe_asset_with(file, &|| budget.expired()).map_err(|_| ())
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

/// The toy cut: one still on the slideshow's own move (in from 1.3x near the top left to just over the frame; or, for the fit picture of a
/// placement, from the frame's own size panned off its edge), into a small 16:9 canvas at 30 fps, as `subject` has it.
fn toy(asset: &kerf_core::Asset, subject: Subject, case: Case) -> (Timeline, ExportOptions) {
    let chain = subject.chain();
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
    c.keyframes = match (chain.fit, case.held) {
        (None, _) => vec![key(0.0, 1.3, 0.03, 0.02), key(end, 1.05, -0.01, 0.0)],
        (Some(_), Some(held)) => vec![key(0.0, held, -0.04, 0.0), key(end, held, 0.04, -0.02)],
        (Some(_), None) => vec![key(0.0, 1.0, -0.04, 0.0), key(end, 1.2, 0.04, -0.02)],
    };
    if chain.fit == Some(FitShape::EngineCrop) {
        c.transform.crop_left = 0.1;
        c.transform.crop_top = 0.05;
    }
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![c],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(CANVAS.0, CANVAS.1, case.fit).with_fps(Some(FPS)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    (
        tl,
        ExportOptions {
            fps: Some(FPS),
            scaler: subject.scaler(),
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
    subject: Subject,
    case: Case,
    n: usize,
    policy: kerf_core::CompositeColorPolicy,
) -> Result<Option<String>, ()> {
    let ext = match kind {
        Kind::Jpeg => "jpg",
        Kind::Rgb => "png",
    };
    let file = dir.join(format!("still{n}.{ext}"));
    let path = file.to_str().ok_or(())?;
    let graph = noise_graph(kind, SHAPES[case.shape]);
    let mut args = vec!["-f", "lavfi", "-i", &graph, "-frames:v", "1"];
    match kind {
        Kind::Jpeg => args.extend(["-pix_fmt", "yuvj420p", "-q:v", "2"]),
        Kind::Rgb => args.extend(["-pix_fmt", "rgb24"]),
    }
    args.push(path);
    budget.ffmpeg(&args, Vec::new()).ok_or(())?;
    let asset = probed(budget, &file)?;
    let (tl, opts) = toy(&asset, subject, case);
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
        if let Some(why) = differs(budget, &plan, want, subject, &mut copy)? {
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
    if let Some(why) = differs(budget, &plan, &raw, subject, &mut copy)? {
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
    subject: Subject,
    copy: &mut Option<(String, Vec<u8>)>,
) -> Result<Option<String>, ()> {
    let chain = subject.chain();
    let [layer] = plan.layers.as_slice() else {
        return Ok(Some(format!("{} layers where the toy cut has one", plan.layers.len())));
    };
    let canvas = (plan.canvas.width, plan.canvas.height);
    // The toy has to take the chain it stands for: a cut that takes another proves nothing about this one.
    match subject {
        Subject::Chain(chain) => {
            let took = plan.still_chain(layer, canvas);
            if took != Some(chain) {
                return Ok(Some(format!("the plan takes the {took:?} chain for a toy of {chain:?}")));
            }
        }
        Subject::Route(name, text) => {
            let took = plan.still_route_scaler(layer);
            if took != Some(name) {
                return Ok(Some(format!(
                    "the plan takes the route under {took:?} for a toy of the `{text}` scaler ({:?} chain)",
                    plan.still_chain(layer, canvas)
                )));
            }
        }
    }
    // ... and be a fit the plan draws at all (the graph fits and zooms in one `scale` for the orders it refuses).
    if layer.fit_order != kerf_core::FitOrder::Fitted {
        return Ok(Some(format!(
            "the toy is a {:?} fit, which the plan does not draw",
            layer.fit_order
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
            // (A copy that comes out another size than the plan says is a difference, not a run that failed: the geometry is not FFmpeg's.)
            if raw.len() != w * h * 5 / 2 {
                return Ok(Some(format!(
                    "the copy FFmpeg made is {} bytes of yuva420p where the plan's {w}x{h} is {}",
                    raw.len(),
                    w * h * 5 / 2
                )));
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
    let map = match chain.fit {
        None => {
            let (Some(still), Some(motion)) = (layer.still.as_ref(), layer.still.as_ref().and_then(|s| s.export.as_ref())) else {
                return Ok(Some("the plan has no move for the render".into()));
            };
            PlacementMap::from_corners(PlacementFrame::window((ww, wh), motion.frame), &motion.corners(&still.pose))
        }
        Some(_) => {
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

    #[test]
    fn a_slot_answers_for_the_chains_that_share_it_and_every_chain_has_one() {
        let mut seen = vec![0; SLOTS];
        for chain in StillChain::ALL {
            let slot = slot_of(chain);
            assert!(slot < SLOTS, "{chain:?}");
            seen[slot] += 1;
            assert!(chains_of(slot).contains(&chain));
        }
        assert_eq!(seen, [2, 1, 1, 1, 1, 1, 1]);
        // The base chains are measured together, every other chain alone.
        assert_eq!(
            chains_of(0),
            [StillChain::inside(false), StillChain::edge(FitShape::Engine, false)]
        );
        // Each chain has toy cuts, and the staged fit's are the frames and the zoom that make a second stage.
        for chain in StillChain::ALL {
            assert!(!cases(chain).is_empty(), "{chain:?}");
        }
    }

    #[test]
    fn the_names_a_route_is_measured_under_are_the_engines_and_a_toy_cut_takes_each() {
        let mut seen = ScalerKinds::NONE;
        for (kind, text) in ROUTE_NAMES {
            // (The caps' kinds and the text the export writes are the same names.)
            assert_eq!(ScalerKinds::of_name(text), Some(kind), "{text}");
            assert!(!seen.contains(kind));
            seen = seen.union(kind);
            let subject = Subject::Route(kind, text);
            assert_eq!(subject.scaler().as_deref(), Some(text));
            assert_eq!(subject.chain(), StillChain::inside(false));
            assert!(!cases(subject.chain()).is_empty());
            // ... and none of them is a bit the chains already hold.
            for k in KINDS {
                assert_ne!(k.set().named_route(kind), StillKinds::NONE);
                assert!(!k.set().all_chains().contains(k.set().named_route(kind)));
            }
        }
        assert_eq!(seen, ScalerKinds::ALL);
        // The default scaler's and bicubic's chains are named as they were.
        assert_eq!(Subject::Chain(StillChain::inside(false)).scaler(), None);
        assert_eq!(Subject::Chain(StillChain::inside(true)).scaler().as_deref(), Some("bicubic"));
        // A name that is none of the four, and a kind with no probe, are `Unknown`, and nothing is measured for them.
        assert_eq!(verdict_route(StillKinds::RGB24, ScalerKinds::NONE), Verdict::Unknown);
        assert_eq!(verdict_route(StillKinds::RGB24, ScalerKinds::ALL), Verdict::Unknown);
        assert_eq!(verdict_route(StillKinds::YUV420P, ScalerKinds::LANCZOS), Verdict::Unknown);
    }

    /// Every route under a named scaler, of both kinds: what a toy cut of a Ken Burns move exported with `scaler: <name>` shows. An RGB picture's
    /// is the model's; so is a JPEG's where the composite's range is fixed (6.1.1), and where it is negotiated (9.0.2) the plan refuses the
    /// layer for its full range anyway. None is one the probe could not tell.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn every_route_under_a_named_scaler_is_measured_by_a_toy_that_takes_it() {
        let mut failures = Vec::new();
        for kind in KINDS {
            for (name, text) in ROUTE_NAMES {
                let v = verdict_route(kind.set(), name);
                eprintln!("{} route under {text}: {v:?}", kind.name());
                if v != Verdict::Match {
                    failures.push(format!("{} {text}: {v:?}", kind.name()));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
        for (name, _) in ROUTE_NAMES {
            assert!(measured().contains_named_route(StillKinds::RGB24, name));
            assert!(measured().contains_named_route(StillKinds::YUVJ420P, name));
        }
    }

    /// Every chain of an RGB picture is one this FFmpeg draws as the model does, and so is every chain of a JPEG's where the composite's range is
    /// fixed (6.1.1); where it is negotiated (9.0.2) FFmpeg keeps a JPEG's full range through the placement's graph, a difference of the fit
    /// picture's chain that the plan refuses the layer for anyway. No chain is one the probe could not tell, and a toy cut for each takes the chain
    /// it stands for (the plan of each is held to FFmpeg's export and still).
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn every_chain_of_both_kinds_is_measured_by_a_toy_that_takes_it() {
        let fixed = kerf_core::composite_color_policy() == kerf_core::CompositeColorPolicy::FixedBt601;
        let mut failures = Vec::new();
        for kind in KINDS {
            for chain in StillChain::ALL {
                let v = verdict(kind.set(), chain);
                eprintln!("{} {chain:?}: {v:?}", kind.name());
                let must_match = kind == Kind::Rgb || fixed;
                if v == Verdict::Unknown || (must_match && v != Verdict::Match) {
                    failures.push(format!("{} {chain:?}: {v:?}", kind.name()));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
        assert!(measured().contains(StillKinds::RGB24.all_chains()));
        assert_eq!(measured().contains(StillKinds::YUVJ420P.all_chains()), fixed);
        // A kind with no probe is measured by none.
        assert!(!measured().contains(StillKinds::YUV420P));
    }
}
