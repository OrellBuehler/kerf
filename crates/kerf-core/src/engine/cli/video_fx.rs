//! A clip's colour correction and effects as filters.

use super::*;

pub(super) fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// The `eq` filter for a clip's color correction, shared by the export and
/// still chains. `temperature` becomes opposing red/blue per-channel gammas —
/// `eq` has no white-balance knob, but shifting midtone gamma per channel
/// warms/cools convincingly; ±1.0 maps to a ±30% gamma split. The channel
/// gammas are omitted at 0 so a temperature-free clip's graph stays
/// byte-identical to before the field existed.
pub(super) fn eq_filter(c: &Color) -> String {
    let mut f = format!(
        "eq=brightness={}:contrast={}:saturation={}:gamma={}",
        c.brightness, c.contrast, c.saturation, c.gamma
    );
    if let Some((gamma_r, gamma_b)) = c.temperature_gammas() {
        f.push_str(&format!(":gamma_r={}:gamma_b={}", fnum(gamma_r), fnum(gamma_b)));
    }
    f
}

/// A keyed number's curve, or — not keyed — its static value as a one-point curve (which
/// [`keyframe_expr`] writes as that number).
pub(super) fn curve_or_static(clip: &Clip, prop: Property) -> Vec<(f64, f64)> {
    if clip.is_keyed(prop) {
        clip.property_curve(prop)
    } else {
        vec![(0.0, clip.static_value(prop))]
    }
}

/// The `eq` filter of a clip with a keyed colour number: `eval=frame`, and each keyed number
/// an expression of the frame's time `t` (the frame's timestamp, which `setpts` has put on the
/// timeline, so clip-local time is `t` minus the clip's start, as every other keyed expression
/// here). Numbers that are not keyed stay the static ones. The temperature is the
/// opposing gammas [`Color::temperature_gammas`] makes of it, `1 + 0.3 t` and `1 - 0.3 t`
/// (the curve's values are held to -1..=1, so the clamp that function applies is already
/// in them). Every expression is quoted — it has commas — and `eq` evaluates them per frame
/// on FFmpeg 4.4 through 9.0 alike.
pub(super) fn eq_filter_keyed(clip: &Clip) -> String {
    let c = &clip.color;
    let expr = |prop: Property| keyframe_expr(&clip.property_curve(prop), "t", clip.timeline_start);
    let number = |prop: Property, fixed: f64| {
        if clip.is_keyed(prop) {
            format!("'{}'", expr(prop))
        } else {
            fixed.to_string()
        }
    };
    let mut f = format!(
        "eq=brightness={}:contrast={}:saturation={}:gamma={}",
        number(Property::Brightness, c.brightness),
        number(Property::Contrast, c.contrast),
        number(Property::Saturation, c.saturation),
        number(Property::Gamma, c.gamma)
    );
    if clip.is_keyed(Property::Temperature) {
        let e = expr(Property::Temperature);
        f.push_str(&format!(":gamma_r='1+0.3*({e})':gamma_b='1-0.3*({e})'"));
    } else if let Some((gamma_r, gamma_b)) = c.temperature_gammas() {
        f.push_str(&format!(":gamma_r={}:gamma_b={}", fnum(gamma_r), fnum(gamma_b)));
    }
    f.push_str(":eval=frame");
    f
}

/// The colour [`eq_filter_keyed`]`(clip)` evaluates to at the frame time `t` (`pts * av_q2d(tb)` of the
/// `fps` grid the filter sits behind): each keyed number its curve through [`keyframe_value`], the same
/// arithmetic as the quoted expression `av_expr` parses back, the unkeyed ones the clip's own. A renderer
/// that draws the filter's tables with no FFmpeg (`eq` rebuilds them every frame) is given these numbers;
/// `Clip::color_at` is the model's value of the same curve, to within a rounding, and is the still's.
pub(crate) fn keyed_color_value(clip: &Clip, t: f64) -> Color {
    let value = |prop: Property, fixed: f64| {
        if clip.is_keyed(prop) {
            keyframe_value(&clip.property_curve(prop), t, clip.timeline_start)
        } else {
            fixed
        }
    };
    let c = &clip.color;
    Color {
        brightness: value(Property::Brightness, c.brightness),
        contrast: value(Property::Contrast, c.contrast),
        saturation: value(Property::Saturation, c.saturation),
        gamma: value(Property::Gamma, c.gamma),
        temperature: value(Property::Temperature, c.temperature),
    }
}

/// The filter for a non-alpha video effect, or `None` for chroma key (which
/// establishes alpha and is emitted separately, after the alpha plane exists).
pub(super) fn video_effect_filter(e: &VideoEffect) -> Option<String> {
    Some(match e {
        VideoEffect::Blur { sigma } => blur_filter(*sigma),
        VideoEffect::Sharpen { amount } => {
            format!("unsharp=luma_msize_x=5:luma_msize_y=5:luma_amount={}", fnum(*amount))
        }
        VideoEffect::Grayscale => "hue=s=0".to_string(),
        VideoEffect::Invert => "negate".to_string(),
        VideoEffect::Vignette => "vignette".to_string(),
        VideoEffect::ChromaKey { .. } => return None,
    })
}

/// The `chromakey` filter for a chroma-key effect, or `None` for any other.
pub(super) fn chroma_filter(e: &VideoEffect) -> Option<String> {
    match e {
        VideoEffect::ChromaKey {
            color,
            similarity,
            blend,
        } => Some(chroma_key_filter(color, *similarity, *blend)),
        _ => None,
    }
}

/// A chroma key as the graph writes it (`chromakey`, the colour made safe). For a renderer's probe, which runs the very text.
pub fn chroma_key_filter(color: &str, similarity: f64, blend: f64) -> String {
    format!(
        "chromakey={}:{}:{}",
        safe_color(color, "green"),
        fnum(similarity),
        fnum(blend)
    )
}
