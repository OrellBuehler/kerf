//! Whether this FFmpeg resizes an 8-bit planar 4:4:4 picture into the composite as the compositor's model does
//! ([`crate::scaler::scale_plane`] over the full-resolution chroma) — measured once per process on the **export's own graph and the
//! still's** ([`kerf_core::export_graph`], [`kerf_core::still_graph`]) over toy cuts, never assumed and never keyed on a version string.
//! `GpuCaps::resize_444` is set only on a [`Resize444::Match`].
//!
//! FFmpeg's fit `scale` of a `yuv444p` clip converts it to 4:2:0 in the same swscale call that resizes it (the chain after it
//! is 4:2:0 throughout: the format negotiation hands the `scale` the composite's format), so each chroma plane is scaled from the
//! full-resolution picture, `W x H` to `w/2 x h/2`, by the tables the luma uses. The compositor decodes such a layer as 4:4:4
//! ([`kerf_core::PlanLayer::wide_chroma`]) and scales its chroma the same way. What this measures is that the filters **in the graph**
//! do that one call: that no second conversion stands between the scale and the overlay, that the window the engine's `crop` leaves
//! (and the Cover crop after the scale) is the one the plan's geometry says, and that the arithmetic of the call is the model's,
//! which `scaler::kernels()` has found for this FFmpeg's scaler (the probe is `Unknown` until it has).
//!
//! The toy cuts are noise `yuv444p` files in a lossless codec, cut into small canvases: a Cover shrink of the same aspect, one that
//! crops, the engine's static `crop` ahead of the fit, a Contain letterbox, an enlargement (the chroma axis the scale does not
//! resize), a steep shrink, and the delivery naming `bicubic`. Every plane of the picture each draws -- three frames of the export
//! and the FFmpeg still -- is held to the model with no tolerance. A run that fails or runs out of the deadline is
//! [`Resize444::Unknown`]: asked again after a backoff, and nothing is drawn from 4:4:4 planes until it answers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kerf_core::{
    Clip, Delivery, ExportOptions, Fit, GpuCaps, OriginalMedia, PlanRequest, Planner, Project, RenderPlan, StreamKind, Timeline,
    Track,
};

use crate::measure::{self, Budget, Probe, Timing, Verdict};
use crate::scaler::{self, Kernels};

/// What the probe found: [`Verdict::Match`] (this FFmpeg's one-call resize of a 4:4:4 picture is the model, in every case),
/// [`Verdict::Differs`] (it is not: the detail names the first case) or [`Verdict::Unknown`].
pub type Resize444 = Verdict;

/// One FFmpeg run is bounded at 20 s, the whole probe (a few dozen runs of tens of milliseconds) at 45 s, and a probe that could not
/// tell is not asked again before 30 s.
const TIMING: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(45),
    per_run: Duration::from_secs(20),
};

static STATE: Probe = Probe::new();

/// What is known without running anything: the measured answer, else [`Verdict::Unknown`]. What the compositor's caps are made from.
pub fn measured() -> Resize444 {
    measure::known(&STATE)
}

/// The answer, measured on the first call: some twenty short `ffmpeg` children one after the other, with no lock held -- a blocking
/// thread's, ahead of the frame that needs it ([`crate::prepare`]). A caller that arrives while the probe runs waits for it (no
/// longer than its deadline) and takes its answer. `Unknown` is not remembered but for a backoff.
pub fn resize_444() -> Resize444 {
    measure::cached(&STATE, Instant::now(), TIMING, probe)
}

/// One toy cut: the picture and the canvas it is cut into, and how.
struct Toy {
    source: (u32, u32),
    canvas: (u32, u32),
    fit: Fit,
    /// The clip's static crop `(left, right, top, bottom)`, fractions of the picture.
    crop: (f64, f64, f64, f64),
    /// The delivery's scaler, by name.
    scaler: Option<&'static str>,
    /// The clip's zoom and position are keyed (the graph is the placement chain, whose head converts the fit's picture to `yuva420p`): only the
    /// first frame is compared, where the pose is the identity and the placement leaves the picture as the fit made it.
    keyed: bool,
}

const fn toy(source: (u32, u32), canvas: (u32, u32), fit: Fit) -> Toy {
    Toy {
        source,
        canvas,
        fit,
        crop: (0.0, 0.0, 0.0, 0.0),
        scaler: None,
        keyed: false,
    }
}

/// The toys. Every side is even (the route is judged for such sizes alone), and every ratio is one the plan draws.
fn toys() -> [Toy; 10] {
    [
        // The whole picture scaled to the frame, a ratio that is not whole.
        toy((160, 90), (96, 54), Fit::Cover),
        // An aspect that crops the scaled picture.
        toy((128, 96), (64, 36), Fit::Cover),
        // The engine's static crop ahead of the fit (a region of the screen): a window of the picture, scaled.
        Toy {
            crop: (0.25, 0.25, 1.0 / 18.0, 1.0 / 3.0),
            ..toy((192, 108), (64, 36), Fit::Cover)
        },
        // A letterbox: bars the `pad` fills round the scaled picture.
        toy((96, 72), (64, 36), Fit::Contain),
        // An enlargement, whose chroma has a horizontal and a vertical axis of ratio 1:1.
        toy((32, 18), (64, 36), Fit::Cover),
        // The luma 2:1 and the chroma 4:1.
        toy((128, 72), (64, 36), Fit::Cover),
        // A steep shrink: luma 10:1, chroma 20:1.
        toy((320, 180), (32, 18), Fit::Cover),
        // The delivery names `bicubic`.
        Toy {
            scaler: Some("bicubic"),
            ..toy((160, 90), (64, 36), Fit::Cover)
        },
        // A zoom and a position that move: the placement chain.
        Toy {
            keyed: true,
            ..toy((160, 90), (96, 54), Fit::Cover)
        },
        Toy {
            keyed: true,
            ..toy((128, 96), (64, 36), Fit::Cover)
        },
    ]
}

/// How many frames of the export are compared.
const FRAMES: u32 = 3;
const FPS: f64 = 30.0;

/// A scratch directory of the probe's own, removed when it is dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Option<Self> {
        let dir = std::env::temp_dir().join(format!("kerf-resize444-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Noise in all three planes, from the coordinates (integers, so the same picture on every build): hard everywhere, which is where a
/// scaler's taps show.
fn noise_graph((w, h): (u32, u32)) -> String {
    format!(
        "nullsrc=size={w}x{h}:rate={FPS}:duration=1,format=yuv444p,\
         geq=lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17,256)'"
    )
}

fn probe(budget: &Budget) -> Verdict {
    let model = match scaler::kernels() {
        Kernels::Match(model) => model,
        Kernels::Differs => return Verdict::Differs("this FFmpeg's scaler is not the model the 4:4:4 resize is built on".into()),
        Kernels::Unknown => return Verdict::Unknown,
    };
    let policy = kerf_core::composite_color_policy();
    if policy == kerf_core::CompositeColorPolicy::Unknown {
        // The colour policy is part of the plan the toy is judged by: not known, not judged.
        return Verdict::Unknown;
    }
    let Some(scratch) = Scratch::new() else {
        return Verdict::Unknown;
    };
    for (n, toy) in toys().iter().enumerate() {
        match matches(budget, &scratch.0, n, toy, model, policy) {
            Ok(None) => {}
            Ok(Some(why)) => return Verdict::Differs(why),
            Err(why) => {
                tracing::warn!(why, "the 4:4:4 resize probe could not tell");
                return Verdict::Unknown;
            }
        }
    }
    Verdict::Match
}

/// The caps the toys are judged by: the route on, with the scaler it stands on.
fn caps() -> GpuCaps {
    let mut caps = GpuCaps::A0;
    caps.resize_444 = true;
    caps.exact_scaler = true;
    caps
}

/// `Ok(None)`: every plane of every frame the export and the still make of the toy is the model's. `Ok(Some(why))`: the first
/// difference. `Err`: the probe could not tell.
fn matches(
    budget: &Budget,
    dir: &Path,
    n: usize,
    toy: &Toy,
    model: scaler::Model,
    policy: kerf_core::CompositeColorPolicy,
) -> Result<Option<String>, String> {
    let file = dir.join(format!("toy{n}.mkv"));
    let path = file.to_str().ok_or("a scratch path that is not text")?;
    let graph = noise_graph(toy.source);
    budget
        .ffmpeg(
            &[
                "-f",
                "lavfi",
                "-i",
                &graph,
                "-frames:v",
                "3",
                "-c:v",
                "ffv1",
                "-pix_fmt",
                "yuv444p",
                path,
            ],
            Vec::new(),
        )
        .ok_or("ffmpeg did not make the toy's picture")?;
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
                "yuv444p",
                "pipe:1",
            ],
            Vec::new(),
        )
        .ok_or("ffmpeg did not decode the toy's picture")?;
    if planes.len() != sw * sh * 3 {
        return Err(format!("{} bytes of the toy's picture, not {}", planes.len(), sw * sh * 3));
    }

    let (cw, ch) = (toy.canvas.0 as usize, toy.canvas.1 as usize);
    let frame_len = cw * ch * 3 / 2;
    let mut clip = Clip::new(asset.id, 0.0, f64::from(FRAMES) / FPS, 0.0);
    (
        clip.transform.crop_left,
        clip.transform.crop_right,
        clip.transform.crop_top,
        clip.transform.crop_bottom,
    ) = toy.crop;
    if toy.keyed {
        let key = |time: f64, scale: f64, pos: (f64, f64)| kerf_core::Keyframe {
            time,
            scale,
            pos_x: pos.0,
            pos_y: pos.1,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        };
        clip.keyframes = vec![key(0.0, 1.0, (0.0, 0.0)), key(0.1, 1.3, (0.05, 0.02))];
    }
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(toy.canvas.0, toy.canvas.1, toy.fit).with_fps(Some(FPS)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let opts = ExportOptions {
        fps: Some(FPS),
        scaler: toy.scaler.map(str::to_string),
        ..ExportOptions::default()
    };
    let assets = std::slice::from_ref(&asset);
    let name = format!(
        "{}x{} -> {}x{} {:?}{}{}{}",
        toy.source.0,
        toy.source.1,
        toy.canvas.0,
        toy.canvas.1,
        toy.fit,
        if toy.crop == (0.0, 0.0, 0.0, 0.0) { "" } else { ", cropped" },
        toy.scaler.map_or(String::new(), |s| format!(", {s}")),
        if toy.keyed { ", moving" } else { "" },
    );

    // The export: the engine's own graph over the file as an input, every frame as planes.
    let export = kerf_core::export_graph(&tl, assets, &opts)
        .map_err(|e| e.to_string())?
        .ok_or("the toy cut has no graph")?;
    let frames = FRAMES.to_string();
    let raw = budget
        .ffmpeg(
            &[
                "-i",
                path,
                "-filter_complex",
                &export,
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
        .ok_or("ffmpeg did not export the toy")?;
    if raw.len() != frame_len * FRAMES as usize {
        return Err(format!(
            "the export is {} bytes, not {}",
            raw.len(),
            frame_len * FRAMES as usize
        ));
    }
    let planner =
        Planner::new(&tl, assets, &opts, PlanRequest::motion(policy).with_media(&OriginalMedia)).map_err(|e| e.to_string())?;
    for k in 0..u64::from(if toy.keyed { 1 } else { FRAMES }) {
        let plan = planner.at_frame(k).map_err(|e| e.to_string())?;
        let want = &raw[k as usize * frame_len..(k as usize + 1) * frame_len];
        if let Some(why) = differs(&plan, want, &planes, toy, model)? {
            return Ok(Some(format!("{name}: export frame {k}: {why}")));
        }
    }

    // The FFmpeg still.
    let t = if toy.keyed { 0.0 } else { 0.5 * f64::from(FRAMES) / FPS };
    let still = kerf_core::still_graph(&tl, assets, &opts, t)
        .map_err(|e| e.to_string())?
        .ok_or("the toy cut has no still graph")?;
    let raw = budget
        .ffmpeg(
            &[
                "-i",
                path,
                "-filter_complex",
                &still,
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
        .ok_or("ffmpeg did not draw the toy's still")?;
    if raw.len() != frame_len {
        return Err(format!("the still is {} bytes, not {frame_len}", raw.len()));
    }
    let planner =
        Planner::new(&tl, assets, &opts, PlanRequest::still(policy).with_media(&OriginalMedia)).map_err(|e| e.to_string())?;
    let plan = planner.at(t).map_err(|e| e.to_string())?.for_render(toy.canvas);
    Ok(differs(&plan, &raw, &planes, toy, model)?.map(|why| format!("{name}: still: {why}")))
}

/// The picture the model draws for `plan` held to `want` (Y, U, V of the canvas): `Ok(None)` when they are the same, else what differs.
/// `source` is the toy's three full-size planes.
fn differs(plan: &RenderPlan, want: &[u8], source: &[u8], toy: &Toy, model: scaler::Model) -> Result<Option<String>, String> {
    let canvas = (plan.canvas.width, plan.canvas.height);
    let [layer] = plan.layers.as_slice() else {
        return Ok(Some(format!("{} layers where the toy cut has one", plan.layers.len())));
    };
    // The toy has to take the route it stands for: a cut that takes another proves nothing about this one.
    let geom = plan.layer_geometry(layer, canvas).map_err(|e| e.to_string())?;
    if let Some(why) = plan.resize_444_refusal(layer, &geom, &caps()) {
        return Ok(Some(format!("the plan does not take the toy from its 4:4:4 planes: {why}")));
    }
    let [stage] = geom.stages.as_slice() else {
        return Ok(Some("a toy that is not one scale".into()));
    };
    let flag = toy.scaler.and_then(scaler::named);
    let scaler = model.scaler(flag);
    let (sw, sh) = (layer.stream.width as usize, layer.stream.height as usize);
    let plane_len = sw * sh;
    if source.len() != 3 * plane_len {
        return Err("the toy's planes are not the stream's size".into());
    }
    let (cw, ch) = (canvas.0 as usize, canvas.1 as usize);
    let (want_y, rest) = want.split_at(cw * ch);
    let (want_u, want_v) = rest.split_at(cw * ch / 4);
    let (at_x, at_y) = (geom.picture_at.0 as usize, geom.picture_at.1 as usize);
    let (shows_w, shows_h) = (geom.picture_shows.0 as usize, geom.picture_shows.1 as usize);
    for (p, (name, expect, plane_w)) in [("Y", want_y, cw), ("U", want_u, cw / 2), ("V", want_v, cw / 2)]
        .into_iter()
        .enumerate()
    {
        let chroma = p > 0;
        let div = if chroma { 2 } else { 1 };
        // The window of the picture the scale reads, from the plane at its own size.
        let (x, y, w, h) = (
            stage.src.x as usize,
            stage.src.y as usize,
            stage.src.w as usize,
            stage.src.h as usize,
        );
        let plane = &source[p * plane_len..(p + 1) * plane_len];
        let mut window = Vec::with_capacity(w * h);
        for row in y..y + h {
            window.extend_from_slice(&plane[row * sw + x..row * sw + x + w]);
        }
        let scaled = (stage.scaled.0 as usize / div, stage.scaled.1 as usize / div);
        let rows = if chroma { 1 } else { 2 };
        let Some(out) = scaler::scale_plane(&window, (w, h), scaled, scaler, model.kernel, rows) else {
            return Ok(Some("the model has no table for the toy".into()));
        };
        // The part the Cover crop keeps.
        let (kx, ky) = (stage.keep.x as usize / div, stage.keep.y as usize / div);
        let (kw, kh) = (stage.keep.w as usize / div, stage.keep.h as usize / div);
        // ... laid where `pad` puts it, as much of it as shows.
        let (ox, oy) = (at_x / div, at_y / div);
        let (vw, vh) = (shows_w / div, shows_h / div);
        let mut first: Option<(usize, usize, u8, u8)> = None;
        let mut count = 0usize;
        for row in 0..vh.min(kh) {
            for col in 0..vw.min(kw) {
                let got = out[(ky + row) * scaled.0 + kx + col];
                let wanted = expect[(oy + row) * plane_w + ox + col];
                if got != wanted {
                    count += 1;
                    first.get_or_insert((col, row, got, wanted));
                }
            }
        }
        if let Some((col, row, got, wanted)) = first {
            return Ok(Some(format!(
                "{name}: {count} samples differ, the first at ({col}, {row}): the model's {got}, FFmpeg's {wanted}"
            )));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The memory of the probe is the shared one's ([`crate::measure`]): a probe that could not tell is asked again after its
    /// backoff, and what it finds is kept.
    #[test]
    fn the_probe_is_bounded_like_its_siblings() {
        assert_eq!(TIMING.backoff, Duration::from_secs(30));
        assert!(TIMING.per_run < TIMING.overall);
        let state = Probe::new();
        let t0 = Instant::now();
        assert_eq!(measure::known(&state), Verdict::Unknown);
        assert_eq!(measure::cached(&state, t0, TIMING, |_| Verdict::Unknown), Verdict::Unknown);
        assert_eq!(measure::known(&state), Verdict::Unknown);
        let after = Instant::now() + TIMING.backoff + Duration::from_secs(1);
        assert_eq!(measure::cached(&state, after, TIMING, |_| Verdict::Match), Verdict::Match);
        assert_eq!(
            measure::cached(&state, after, TIMING, |_| panic!("measured twice")),
            Verdict::Match
        );
    }

    /// Every toy has even sides and takes a ratio the plan draws.
    #[test]
    fn the_toys_are_even_and_within_the_chroma_shrink_limit() {
        for toy in toys() {
            for side in [toy.source.0, toy.source.1, toy.canvas.0, toy.canvas.1] {
                assert_eq!(side % 2, 0, "{:?}", toy.source);
            }
            assert!(u64::from(toy.source.0) <= u64::from(kerf_core::MAX_SHRINK) * u64::from(toy.canvas.0 / 2));
        }
    }
}
