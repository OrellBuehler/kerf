//! Checks on the values a caller hands in: numbers, colours, effects and keyframes.

use super::*;

/// Refuse a number that is `NaN` or infinite, naming what it was for.
pub(super) fn finite(what: &str, v: f64) -> Result<()> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(Error::InvalidArgument(format!("{what} must be a finite number")))
    }
}

/// A colour the user or an agent typed, before it is stored: one FFmpeg reads (a name, `#rrggbb`,
/// `0xrrggbb`, with an optional `@alpha`) — nothing that could carry a filter option or a style.
pub(super) fn check_color(what: &str, color: &str) -> Result<()> {
    if engine::valid_color(color) {
        Ok(())
    } else {
        Err(Error::InvalidArgument(format!(
            "{what} {color:?} is not a colour — use a name like white or a hex value like #ffcc00"
        )))
    }
}

pub(super) fn validate_video_effect(e: &VideoEffect) -> Result<()> {
    match e {
        // The ranges are those of the filters the effects become (`gblur`, `unsharp`, `chromakey`).
        VideoEffect::Blur { sigma } => {
            if !sigma.is_finite() || !(0.0..=1024.0).contains(sigma) {
                return Err(Error::InvalidArgument("blur sigma must be within 0..=1024".to_string()));
            }
        }
        VideoEffect::Sharpen { amount } => {
            if !amount.is_finite() || !(-2.0..=5.0).contains(amount) {
                return Err(Error::InvalidArgument("sharpen amount must be within -2..=5".to_string()));
            }
        }
        VideoEffect::ChromaKey {
            color,
            similarity,
            blend,
        } => {
            check_color("chroma key color", color)?;
            if !(0.00001..=1.0).contains(similarity) {
                return Err(Error::InvalidArgument(
                    "chroma key similarity must be within 0.00001..=1.0".to_string(),
                ));
            }
            if !(0.0..=1.0).contains(blend) {
                return Err(Error::InvalidArgument(
                    "chroma key blend must be within 0.0..=1.0".to_string(),
                ));
            }
        }
        VideoEffect::Grayscale | VideoEffect::Invert | VideoEffect::Vignette => {}
    }
    Ok(())
}

/// Every audio effect parameter against the range of the FFmpeg filter it becomes (`highpass`,
/// `lowpass`, `equalizer`, `acompressor`, `agate`, `afftdn` — `ffmpeg -h filter=<name>`): outside it the
/// export fails on the filter's own error, long after the edit was accepted. The dB parameters
/// are converted to the linear values the filters take (`acompressor`'s threshold is
/// 0.000976563..1, its makeup 1..64; `agate`'s threshold 0..1), which bounds them in dB.
pub(super) fn validate_audio_effect(e: &AudioEffect) -> Result<()> {
    let within = |v: f64, name: &str, unit: &str, lo: f64, hi: f64| {
        if v.is_finite() && (lo..=hi).contains(&v) {
            Ok(())
        } else {
            Err(Error::InvalidArgument(format!(
                "{name} must be within {lo}..={hi}{unit}, got {v}"
            )))
        }
    };
    // A frequency or a width has to be above zero (the filters' range starts there).
    let above_zero = |v: f64, name: &str, unit: &str, hi: f64| {
        if v.is_finite() && v > 0.0 && v <= hi {
            Ok(())
        } else {
            Err(Error::InvalidArgument(format!(
                "{name} must be above 0 and at most {hi}{unit}, got {v}"
            )))
        }
    };
    match e {
        AudioEffect::Highpass { hz } | AudioEffect::Lowpass { hz } => above_zero(*hz, "frequency", " Hz", 999_999.0)?,
        AudioEffect::Equalizer { hz, width, gain_db } => {
            above_zero(*hz, "frequency", " Hz", 999_999.0)?;
            above_zero(*width, "width", " Hz", 99_999.0)?;
            within(*gain_db, "gain_db", " dB", -900.0, 900.0)?;
        }
        AudioEffect::Compressor {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_db,
        } => {
            within(*threshold_db, "compressor threshold_db", " dB", -60.0, 0.0)?;
            within(*ratio, "compressor ratio", "", 1.0, 20.0)?;
            within(*attack_ms, "compressor attack_ms", " ms", 0.01, 2000.0)?;
            within(*release_ms, "compressor release_ms", " ms", 0.01, 9000.0)?;
            within(*makeup_db, "compressor makeup_db", " dB", 0.0, 36.0)?;
        }
        AudioEffect::Gate { threshold_db } => within(*threshold_db, "gate threshold_db", " dB", -100.0, 0.0)?,
        AudioEffect::Denoise {
            reduction_db, floor_db, ..
        } => {
            within(*reduction_db, "denoise reduction_db", " dB", 0.01, 97.0)?;
            within(*floor_db, "denoise floor_db", " dB", -80.0, -20.0)?;
        }
    }
    Ok(())
}

/// Angles are wrapped or clamped downstream (see [`Reframe::sample`]), so only
/// non-finite values are rejected here — a caller may legitimately pass 540°.
pub(super) fn validate_angle(name: &str, v: Option<f64>) -> Result<()> {
    match v {
        Some(v) if !v.is_finite() => Err(Error::InvalidArgument(format!("{name} must be a finite number of degrees"))),
        _ => Ok(()),
    }
}

pub(super) fn validate_fov(v: Option<f64>) -> Result<()> {
    match v {
        Some(v) if !v.is_finite() || !(MIN_FOV..=MAX_FOV).contains(&v) => Err(Error::InvalidArgument(format!(
            "field of view must be within {MIN_FOV}..={MAX_FOV} degrees"
        ))),
        _ => Ok(()),
    }
}

pub(super) fn validate_lens_fov(v: Option<f64>) -> Result<()> {
    match v {
        Some(v) if !v.is_finite() || !(1.0..=360.0).contains(&v) => Err(Error::InvalidArgument(
            "lens field of view must be within 1..=360 degrees".to_string(),
        )),
        _ => Ok(()),
    }
}

pub(super) fn validate_reframe_keyframe(k: &ReframeKeyframe) -> Result<()> {
    if !k.time.is_finite() || k.time < 0.0 {
        return Err(Error::InvalidArgument("keyframe time must be >= 0".to_string()));
    }
    validate_angle("yaw", Some(k.yaw))?;
    validate_angle("pitch", Some(k.pitch))?;
    validate_angle("roll", Some(k.roll))?;
    validate_fov(Some(k.fov))
}

pub(super) fn validate_keyframe(k: &Keyframe) -> Result<()> {
    if !k.time.is_finite() || k.time < 0.0 {
        return Err(Error::InvalidArgument("keyframe time must be >= 0".to_string()));
    }
    if !k.scale.is_finite() || k.scale <= 0.0 {
        return Err(Error::InvalidArgument(
            "keyframe scale must be a finite value > 0".to_string(),
        ));
    }
    if !(0.0..=1.0).contains(&k.opacity) {
        return Err(Error::InvalidArgument(
            "keyframe opacity must be within 0.0..=1.0".to_string(),
        ));
    }
    if ![k.pos_x, k.pos_y, k.rotation].iter().all(|v| v.is_finite()) {
        return Err(Error::InvalidArgument("keyframe values must be finite".to_string()));
    }
    validate_easing(&k.easing)
}

/// The longest track of keys a property may be given: an expression is written per segment
/// (a dozen pieces each when eased), and an agent's list is not otherwise bounded.
pub const MAX_CHANNEL_KEYS: usize = 1_000;

/// The furthest [`Project::copy_keyframes`] may shift keys (seconds): 100 hours.
pub(super) const MAX_KEY_OFFSET: f64 = 360_000.0;

/// A mask number is keyed only on a clip that has a mask: its keys would draw nothing, and the
/// caller would not know why (an empty list, which clears them, is always fine).
pub(super) fn check_keyable(clip: &Clip, prop: Property, keys: &[PropertyKey]) -> Result<()> {
    if prop.is_mask() && clip.mask.is_none() && !keys.is_empty() {
        return Err(Error::InvalidArgument(format!(
            "the clip has no mask to key the {} of: give it one first (set_mask)",
            prop.label()
        )));
    }
    Ok(())
}

pub(super) fn validate_property_keys(prop: Property, keys: &[PropertyKey]) -> Result<()> {
    if keys.len() > MAX_CHANNEL_KEYS {
        return Err(Error::InvalidArgument(format!(
            "at most {MAX_CHANNEL_KEYS} keyframes per property (got {})",
            keys.len()
        )));
    }
    for k in keys {
        if !k.time.is_finite() || k.time < 0.0 {
            return Err(Error::InvalidArgument("keyframe time must be >= 0".to_string()));
        }
        prop.check(k.value)?;
        validate_easing(&k.easing)?;
    }
    Ok(())
}

/// A bezier easing keeps both control points in the unit square: no overshoot, so an eased
/// value stays between its two keys (the engine clamps what it reads, but a stored value
/// outside would silently not be the curve it says).
pub(super) fn validate_easing(easing: &Easing) -> Result<()> {
    if let Easing::Bezier { x1, y1, x2, y2 } = *easing {
        if [x1, y1, x2, y2].iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
            return Err(Error::InvalidArgument(
                "bezier control points must be within 0.0..=1.0 (no overshoot)".to_string(),
            ));
        }
    }
    Ok(())
}
