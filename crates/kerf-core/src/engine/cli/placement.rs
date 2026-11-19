//! Where a clip sits in the frame: position, scale, crop and mask, and the numbers they run on.

use super::*;

/// Format an f64 for an ffmpeg filter argument / expression (Rust's default
/// `{}` avoids scientific notation for the ranges used here; `-0` is normalized).
pub(super) fn fnum(v: f64) -> String {
    let s = format!("{v}");
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

/// The overlay offset a motion transition puts on a clip, as `(x, y)`
/// expressions in frame widths and heights over **timeline** time — or `None`
/// when the clip does not move, which is what keeps every non-motion graph
/// byte-identical.
///
/// Both halves are ordinary keyframes ([`ClipTiming::motion_keys`] decides them),
/// so this is [`keyframe_expr`] twice over rather than a second expression
/// language.
pub(super) fn motion_expr(clip: &Clip, fx: &ClipFx) -> Option<(String, String)> {
    let keys = ClipTiming::new(clip, fx).motion_keys()?;
    let start = clip.timeline_start;
    Some((keyframe_expr(&keys.x, "t", start), keyframe_expr(&keys.y, "t", start)))
}

/// The `geq` that cuts a clip to its [`Mask`]: the picture is passed through
/// untouched and only the alpha plane is rewritten, so what is outside the shape
/// (or inside it, `inverted`) becomes transparent and a lower track shows
/// through.
///
/// One expression covers both shapes. Each axis is scaled so the shape's edge
/// sits at distance 1, and the shapes differ only in how the two axes combine —
/// `max` gives a rectangle, `hypot` an ellipse. Feathering is then a ramp over
/// the last `feather` of that distance, measured *inside* the edge so a softened
/// mask never grows beyond the shape that was drawn.
///
/// `geq` is per-pixel and therefore slow, the same cost keyframed opacity
/// already pays; a mask is worth it and a full-frame one is simply not written.
pub(super) fn mask_filter(mask: &Mask) -> String {
    format!(
        "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='({keep})*alpha(X,Y)'",
        keep = mask_keep_expr(mask)
    )
}

/// The `geq` slot a keyed opacity beside a keyed mask is kept in (the mask's numbers take
/// `0..5`, in [`Property::MASK`] order).
const OPACITY_SLOT: usize = Property::MASK.len();

/// The alpha of a clip whose mask has a keyed number: [`mask_keep_expr`] with each keyed number
/// its curve over `geq`'s `T`, times a keyed opacity's curve when there is one (`opacity`), so a
/// frame of it is [`mask_keep_expr`] of [`Clip::mask_at`] that moment.
///
/// `geq` evaluates its expression for every pixel, and a curve is a tree of comparisons, so
/// the curves are evaluated **once a row**: at its first pixel (`X` 0) each is stored in a slot
/// (`st`) and every other pixel reads it back (`ld`). A slice thread has its own copy of the
/// expression and so its own slots, and walks its rows from `X` 0. A feather keyed to nothing is
/// the hard edge a static mask with no feather gets.
pub(super) fn keyed_mask_alpha(clip: &Clip, mask: &Mask, opacity: Option<&str>) -> String {
    let m = mask.normalized();
    let mut stores: Vec<String> = Property::MASK
        .iter()
        .enumerate()
        .filter(|(_, p)| clip.is_keyed(**p))
        .map(|(slot, p)| {
            format!(
                "st({slot},{})",
                keyframe_expr(&clip.property_curve(*p), "T", clip.timeline_start)
            )
        })
        .collect();
    if let Some(op) = opacity {
        stores.push(format!("st({OPACITY_SLOT},{op})"));
    }
    let slot = |p: Property| Property::MASK.iter().position(|q| *q == p).unwrap_or(0);
    let n = |p: Property, v: f64| {
        if clip.is_keyed(p) {
            format!("ld({})", slot(p))
        } else {
            fnum(v)
        }
    };
    let half = |p: Property, v: f64| {
        if clip.is_keyed(p) {
            format!("ld({})/2", slot(p))
        } else {
            fnum(v / 2.0)
        }
    };
    let dx = format!(
        "(X-{cx}*W)/({rw}*W)",
        cx = n(Property::MaskX, m.x),
        rw = half(Property::MaskWidth, m.width)
    );
    let dy = format!(
        "(Y-{cy}*H)/({rh}*H)",
        cy = n(Property::MaskY, m.y),
        rh = half(Property::MaskHeight, m.height)
    );
    let d = match m.shape {
        MaskShape::Rect => format!("max(abs({dx})\\,abs({dy}))"),
        MaskShape::Ellipse => format!("hypot({dx}\\,{dy})"),
    };
    let inside = if clip.is_keyed(Property::MaskFeather) {
        let f = n(Property::MaskFeather, m.feather);
        format!("if(lte({f}\\,0.000001)\\,lte({d}\\,1)\\,clip((1-{d})/{f}\\,0\\,1))")
    } else if m.feather <= 1e-6 {
        format!("lte({d}\\,1)")
    } else {
        format!("clip((1-{d})/{f}\\,0\\,1)", f = fnum(m.feather))
    };
    let keep = if m.inverted { format!("(1-{inside})") } else { inside };
    let op = if opacity.is_some() {
        format!("*ld({OPACITY_SLOT})")
    } else {
        String::new()
    };
    format!(
        "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(eq(X,0),{stores},0);({keep}){op}*alpha(X,Y)'",
        stores = stores.join("+")
    )
}

/// The corners `perspective` (`sense=source`: the picture's points its output's corners `(0,0)` ..
/// `(W,H)` read from) is given for the animated crop's window ([`Clip::crop_window_at`]): `side` of
/// each side of the picture, placed by the pans in the room left over. Output pixel `x` reads the
/// picture at `left * W + (x + 0.5) * side - 0.5` — pixel centres, as a scale reads them, so the
/// neutral window reads every pixel where it is — which puts the top-left corner at
/// `((1 + pan) * W / 2 - 0.5) * (1 - side)` and the bottom-right one at
/// `W - ((1 - pan) * W / 2 + 0.5) * (1 - side)` (the same point plus `side * W`, written so each
/// corner holds one copy of each curve). The same text for the export's curves and the still's
/// numbers, so both evaluate one formula.
pub(super) fn crop_corners(side: &str, pan_x: &str, pan_y: &str) -> String {
    let x0 = format!("((1+{pan_x})*W/2-0.5)*(1-{side})");
    let y0 = format!("((1+{pan_y})*H/2-0.5)*(1-{side})");
    let x1 = format!("W-((1-{pan_x})*W/2+0.5)*(1-{side})");
    let y1 = format!("H-((1-{pan_y})*H/2+0.5)*(1-{side})");
    format!("x0='{x0}':y0='{y0}':x1='{x1}':y1='{y0}':x2='{x0}':y2='{y1}':x3='{x1}':y3='{y1}'")
}

/// The `fps` ahead of a keyed crop: with its first frame's slot pinned (`start_time`, the time the
/// chain's `setpts` puts its first frame at), so that slot is known here — `perspective` has no
/// clock, only a count of its frames (`in`, from 1), and frame `in` is output frame
/// `first + in - 1`. Returns the filter and that first frame: `fps` rounds `start_time`, read as
/// whole microseconds, to its own ticks half away from zero.
pub(super) fn keyed_crop_fps(fps: f64, first: f64) -> (String, Rational, i64) {
    let rate = Rational::from_fps_filter(fps).unwrap_or(Rational { num: 30, den: 1 });
    let micros = (first * 1e6) as i64;
    let slot = crate::frame_pick::rescale_near(i128::from(micros), i128::from(rate.num), 1_000_000 * i128::from(rate.den)) as i64;
    (format!("fps=fps={fps}:start_time={first}"), rate, slot)
}

/// The `perspective` that draws a keyed crop: [`crop_corners`] of the window's curves over the
/// time of the frame (`(first + in - 1) * den / num`, the time the graph evaluates output frame
/// `first + in - 1` at), re-evaluated every frame. It keeps the picture's size — that is why the
/// crop is keyed as a zoom and a pan — and reads it bilinearly, a fixed lookup per frame instead
/// of an expression per pixel.
pub(super) fn crop_remap_filter(clip: &Clip, rate: Rational, first: i64) -> String {
    let t = perspective_time(rate, first);
    let n = |p: Property| {
        if clip.is_keyed(p) {
            format!("({})", keyframe_expr(&clip.property_curve(p), &t, clip.timeline_start))
        } else {
            fnum(p.neutral())
        }
    };
    format!(
        "perspective={}:interpolation=linear:eval=frame",
        crop_corners(
            &format!("(1/{})", n(Property::CropZoom)),
            &n(Property::CropX),
            &n(Property::CropY)
        )
    )
}

/// The time a `perspective` evaluates output frame `first + in - 1` at (it has no clock, only
/// `in`, its count of frames from 1; see [`keyed_crop_fps`]).
fn perspective_time(rate: Rational, first: i64) -> String {
    let frame = match first - 1 {
        0 => "in".to_string(),
        a if a > 0 => format!("in+{a}"),
        a => format!("in-{}", -a),
    };
    format!("(({frame})*{}/{})", rate.den, rate.num)
}

/// The last stage of a clip whose zoom or position moves ([`Clip::placement_animated`]): the
/// picture is laid on a canvas-sized frame, zoomed about its centre and moved, in **one bilinear
/// resample at sub-pixel positions**, and the overlay then puts that frame at `0:0`.
///
/// A per-frame `scale` makes whole-pixel sizes and the overlay's `x`/`y` truncate to even
/// pixels in 4:2:0, so a slow zoom-pan (a still at 1.10 to 1.17 over 3.5 s) moved in irregular
/// one- and two-pixel steps. Here every frame is a fixed lookup (`perspective`, `eval=frame`,
/// as a keyed crop is drawn) and the size never changes. The keyed numbers are curves over the
/// time of the frame `perspective` counts ([`perspective_time`]); the travel of a slide or push
/// is added to the position. `alpha`: the chain already has an alpha plane.
pub(super) fn placement_filters(
    clip: &Clip,
    fx: &ClipFx,
    fmt: &ExportFormat,
    alpha: bool,
    rate: Rational,
    first: i64,
) -> Vec<String> {
    let t = perspective_time(rate, first);
    let start = clip.timeline_start;
    let curve = |pts: &[(f64, f64)]| keyframe_expr(pts, &t, start);
    let scale = clip.zoom_animated().then(|| driven_expr(clip, Property::Scale, &t));
    let (mut px, mut py) = (driven_expr(clip, Property::PosX, &t), driven_expr(clip, Property::PosY, &t));
    if let Some(keys) = ClipTiming::new(clip, fx).motion_keys() {
        px = format!("({px})+({})", curve(&keys.x));
        py = format!("({py})+({})", curve(&keys.y));
    }
    placement_chain(scale.as_deref(), (&px, &py), (fmt.width, fmt.height), alpha, true)
}

/// The filters of a placement ([`placement_filters`]; the scrubbed still's, with the numbers of its
/// moment): the picture zoomed by `scale` (`None`: not at all) about its centre, moved to the
/// canvas' centre plus `pos` (in canvas widths and heights), on a `canvas`-sized `yuva420p` frame.
///
/// The picture is padded with transparency (`black@0`), centred, to at least the canvas and two
/// pixels past itself on every side, so what `perspective` reads past the picture (it clamps to
/// the edge) is transparent; it is then cut to the canvas round the same centre. Both offsets are
/// even whole numbers (`2*ceil(..)`, `2*floor(..)`), so neither `pad` nor `crop` rounds them, and
/// `perspective` (which knows only its own frame size) computes the same window. Its corners map
/// pixel centres: canvas pixel `X` reads the picture at `(X + 0.5 - cx) / s + w/2 - 0.5`.
pub(super) fn placement_chain(
    scale: Option<&str>,
    pos: (&str, &str),
    canvas: (u32, u32),
    alpha: bool,
    per_frame: bool,
) -> Vec<String> {
    let (cw, ch) = canvas;
    // One axis: the picture position its output pixel `edge` (`0` or the frame's `W` / `H`) reads.
    let corner = |edge: &str, dim: &str, canvas: u32, pos: &str| {
        let centre = format!("{}+({pos})*{canvas}", fnum(f64::from(canvas) / 2.0));
        let off = format!("{edge}+0.5-2*floor(({dim}-{canvas})/4)-({centre})");
        match scale {
            Some(s) => format!("({off})/({s})+{dim}/2-0.5"),
            None => format!("{off}+{dim}/2-0.5"),
        }
    };
    let (x0, x1) = (corner("0", "W", cw, pos.0), corner("W", "W", cw, pos.0));
    let (y0, y1) = (corner("0", "H", ch, pos.1), corner("H", "H", ch, pos.1));
    let mut p = Vec::new();
    if !alpha {
        p.push("format=yuva420p".to_string());
    }
    p.extend([
        format!(
            "pad=w='iw+4*ceil(max(0,{cw}-iw)/4)+4':h='ih+4*ceil(max(0,{ch}-ih)/4)+4':\
             x='2*ceil(max(0,{cw}-iw)/4)+2':y='2*ceil(max(0,{ch}-ih)/4)+2':color=black@0"
        ),
        format!(
            "perspective=x0='{x0}':y0='{y0}':x1='{x1}':y1='{y0}':x2='{x0}':y2='{y1}':x3='{x1}':y3='{y1}':\
             interpolation=linear{}",
            if per_frame { ":eval=frame" } else { "" }
        ),
        format!("crop=w={cw}:h={ch}:x='2*floor((iw-{cw})/4)':y='2*floor((ih-{ch})/4)'"),
    ]);
    p
}

/// The placement chain **as the export and the still write it** (`per_frame`: the export's `eval=frame`), for the
/// compositor's probe of what this machine's `pad`, `perspective` and `crop` do with it: the text is
/// [`placement_chain`]'s, joined, so a probe measures the filters the graph really holds and cannot drift from them.
pub fn placement_probe_chain(scale: Option<&str>, pos: (&str, &str), canvas: (u32, u32), per_frame: bool) -> String {
    placement_chain(scale, pos, canvas, false, per_frame).join(",")
}

/// A moving placement as the export writes it, and the numbers the plan evaluates for it ([`placement_probe_motion`]).
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementProbeMotion {
    /// The zoom's expression over `in` (the frame count `perspective` has), the position's two.
    pub scale: String,
    pub pos: (String, String),
    /// What the plan evaluates for frame `in = k + 1` (index `k`): the zoom and the position.
    pub poses: Vec<(f64, (f64, f64))>,
}

/// A clip whose zoom and position are keyed and which breathes, drifts and shakes at 30 fps, as
/// [`placement_filters`] writes it (the same [`driven_expr`] over the same `in`-time) and as the plan evaluates it
/// ([`Clip::transform_at`]) for the first `frames` frames of the graph. What the compositor's probe measures
/// the filters against: the expression's `sin` and `cos` are the machine's libm's, the plan's the app's.
pub fn placement_probe_motion(frames: u32) -> PlacementProbeMotion {
    use crate::model::{Behavior, PropertyKey};
    let rate = Rational { num: 30, den: 1 };
    let mut clip = Clip::new(uuid::Uuid::nil(), 0.0, 10.0, 0.0);
    clip.set_property_keys(
        Property::Scale,
        vec![
            PropertyKey::new(0.0, 1.1),
            PropertyKey::new(0.2, 2.3),
            PropertyKey::new(0.4, 0.7),
        ],
    );
    clip.set_property_keys(Property::PosX, vec![PropertyKey::new(0.0, -0.2), PropertyKey::new(0.3, 0.15)]);
    clip.set_property_keys(Property::PosY, vec![PropertyKey::new(0.0, 0.1), PropertyKey::new(0.3, -0.25)]);
    clip.behaviors = vec![
        Behavior::Breathe {
            amount: 0.12,
            speed: 0.8,
            phase: 1.25,
        },
        Behavior::Drift {
            speed: 0.15,
            angle: -33.0,
            phase: 0.5,
        },
        Behavior::Shake {
            amount: 0.013,
            speed: 3.1,
            seed: 4_000_000_001,
            phase: 0.5,
        },
    ];
    let t = perspective_time(rate, 0);
    let poses = (0..frames)
        .map(|k| {
            // The graph's `((in-1)*den/num)`, in the filter's own double arithmetic.
            let local = (f64::from(k) * f64::from(rate.den)) / f64::from(rate.num);
            let at = clip.transform_at(local);
            (at.scale, (at.pos_x, at.pos_y))
        })
        .collect();
    PlacementProbeMotion {
        scale: driven_expr(&clip, Property::Scale, &t),
        pos: (driven_expr(&clip, Property::PosX, &t), driven_expr(&clip, Property::PosY, &t)),
        poses,
    }
}

/// The 0..1 "keep" expression at the heart of [`mask_filter`], separated out so
/// a clip with both a mask and keyframed opacity can fold the two into one
/// `geq` pass instead of paying the per-pixel cost twice.
pub(super) fn mask_keep_expr(mask: &Mask) -> String {
    let m = mask.normalized();
    let dx = format!("(X-{cx}*W)/({rw}*W)", cx = fnum(m.x), rw = fnum(m.width / 2.0));
    let dy = format!("(Y-{cy}*H)/({rh}*H)", cy = fnum(m.y), rh = fnum(m.height / 2.0));
    let d = match m.shape {
        MaskShape::Rect => format!("max(abs({dx})\\,abs({dy}))"),
        MaskShape::Ellipse => format!("hypot({dx}\\,{dy})"),
    };
    let inside = if m.feather <= 1e-6 {
        format!("lte({d}\\,1)")
    } else {
        format!("clip((1-{d})/{f}\\,0\\,1)", f = fnum(m.feather))
    };
    if m.inverted {
        format!("(1-{inside})")
    } else {
        inside
    }
}

/// A scale below this is one a picture can be truncated to nothing by: `scale` reads a
/// width or height that evaluates to 0 as "unset" and keeps the *input's* size, so a clip
/// zoomed to 0.0004 of a frame would snap to full size instead of vanishing. The scale
/// expressions only say `max(1, ...)` below it, so every ordinary graph is as it was.
pub(super) const TINY_SCALE: f64 = crate::layer_geometry::TINY_SCALE;

/// The `scale` that zooms a clip's picture by `factor`: a plain number for a constant zoom,
/// else (`keyed`) an expression in `t` that is re-evaluated every frame.
///
/// `tiny` (a factor that can reach [`TINY_SCALE`]) keeps both sides at 1 px or more, and
/// `even` (a tone-map follows: `zscale` refuses an odd size in 4:2:0) rounds them down to
/// an even number of pixels. Neither is written when it is not needed.
pub(super) fn zoom_scale(factor: &str, keyed: bool, tiny: bool, even: bool, sf: &str) -> String {
    let side = |axis: &str| {
        let e = if keyed {
            format!("{axis}*({factor})")
        } else {
            format!("{axis}*{factor}")
        };
        match (even, tiny) {
            (true, _) => format!("max(2,2*trunc(({e})/2))"),
            (false, true) => format!("max(1,{e})"),
            (false, false) => e,
        }
    };
    let (w, h) = (side("iw"), side("ih"));
    if keyed {
        format!("scale=w='{w}':h='{h}':eval=frame{sf}")
    } else if even || tiny {
        format!("scale=w='{w}':h='{h}'{sf}")
    } else {
        format!("scale={w}:{h}{sf}")
    }
}

/// The one `scale` that fits a picture to a `w`x`h` frame and zooms it by `factor` in the same
/// resample ([`FitOrder::Folded`] with `decrease`, [`FitOrder::Uncropped`] with `increase`): the
/// frame times the zoom is the box the aspect-preserving fit is made in, so the picture is taken
/// from the source once instead of being fitted to the frame and the fitted picture enlarged. A
/// zoom of 1 is the frame's own box.
///
/// `tone_mapped` (a `zscale` follows, which refuses an odd size in 4:2:0) rounds the box and the
/// result to even sizes, and a zoom under [`TINY_SCALE`] keeps the box at a pixel or more, as
/// [`zoom_scale`] does. Neither is written when it is not needed.
pub(super) fn fit_scale(w: u32, h: u32, factor: f64, aspect: &str, tone_mapped: bool, sf: &str) -> String {
    let zoomed = (factor - 1.0).abs() > 1e-9;
    let tiny = zoomed && factor < TINY_SCALE;
    let side = |n: u32| {
        let e = if zoomed { format!("{n}*{factor}") } else { n.to_string() };
        match (tone_mapped && zoomed, tiny) {
            (true, _) => format!("max(2,2*trunc(({e})/2))"),
            (false, true) => format!("max(1,{e})"),
            (false, false) => e,
        }
    };
    let (bw, bh) = (side(w), side(h));
    let even = if tone_mapped { ":force_divisible_by=2" } else { "" };
    if (tone_mapped && zoomed) || tiny {
        format!("scale=w='{bw}':h='{bh}':force_original_aspect_ratio={aspect}{even}{sf}")
    } else {
        format!("scale={bw}:{bh}:force_original_aspect_ratio={aspect}{even}{sf}")
    }
}

/// The `crop` that keeps an [`FitOrder::Uncropped`] picture — the cover of the frame times the
/// zoom, which can be several frames wide — to what any frame of the clip can show, so the
/// overlay is not handed a needlessly large picture (pure).
///
/// The window is **centred on the picture**, so the overlay's own `(W-w)/2` still puts the picture's
/// centre where it would have put the whole one, and it is cut in steps of four pixels
/// (`iw - 4q`), so an even picture stays even and each side loses an even amount, which the
/// overlay's rounding down to even then leaves alone. Its half-extent is how far the canvas
/// reaches from the picture's centre: half the frame plus the furthest the picture is ever moved
/// (`pan`: the position's greatest value and a slide or push's travel, in frame fractions), turned
/// with the picture (`rotation`: the angle of a static one, the circle round the frame when the
/// rotation is keyed) and given `margin` pixels for what reads past the edge of the window (a
/// blur). Where the picture is smaller than that, it is not cut at all.
pub(super) fn bound_crop(frame: (u32, u32), pan: (f64, f64), rotation: Rotated, margin: f64) -> String {
    let ax = f64::from(frame.0) * (0.5 + pan.0);
    let ay = f64::from(frame.1) * (0.5 + pan.1);
    let (wx, wy) = match rotation {
        Rotated::No => (ax, ay),
        Rotated::By(angle) => (
            ax * angle.cos().abs() + ay * angle.sin().abs(),
            ax * angle.sin().abs() + ay * angle.cos().abs(),
        ),
        Rotated::Keyed => (ax.hypot(ay), ax.hypot(ay)),
    };
    // (A turn of exactly 90 degrees leaves a cosine of 1e-16; the epsilon keeps it from costing a pixel.)
    // (A number that is not one — a hand-edited file — keeps the whole picture, as it never cuts.)
    let need = |half: f64| {
        let v = 2.0 * half + 2.0 * margin - 1e-6;
        if v.is_finite() {
            v.ceil().clamp(2.0, 1e9) as u64
        } else {
            1_000_000_000
        }
    };
    format!(
        "crop=w='iw-4*floor(max(0,iw-{nw})/4)':h='ih-4*floor(max(0,ih-{nh})/4)'",
        nw = need(wx),
        nh = need(wy)
    )
}

/// How an [`FitOrder::Uncropped`] clip turns its picture, for [`bound_crop`].
#[derive(Clone, Copy)]
pub(super) enum Rotated {
    No,
    /// A static rotation, in radians.
    By(f64),
    /// A keyed rotation that turns the picture: any angle.
    Keyed,
}
