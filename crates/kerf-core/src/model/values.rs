//! The values an edit takes, held to the ranges the edit holds them to. `Project`'s setters call
//! these before they change anything, and whatever shows a value before it is written (the GPU
//! preview's drag frames) calls the same ones, so it never shows a value the edit would refuse.

use super::{Asset, Clip, Color, TextKeyframe, TextOverlay, Transform};
use crate::error::{Error, Result};

/// How far the footage of a clip may be asked past the asset's length (seconds): the float noise
/// of a duration that went through a probe and a JSON round trip.
const SOURCE_EPS: f64 = 1e-3;

fn invalid(why: &str) -> Error {
    Error::InvalidArgument(why.to_string())
}

impl Transform {
    /// The numbers `Project::set_transform` takes, each `None` left as it is.
    #[allow(clippy::too_many_arguments)]
    pub fn check_values(
        scale: Option<f64>,
        pos_x: Option<f64>,
        pos_y: Option<f64>,
        rotation: Option<f64>,
        opacity: Option<f64>,
        crop_left: Option<f64>,
        crop_right: Option<f64>,
        crop_top: Option<f64>,
        crop_bottom: Option<f64>,
    ) -> Result<()> {
        if scale.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            return Err(invalid("scale must be a finite value > 0"));
        }
        if [pos_x, pos_y, rotation].into_iter().flatten().any(|v| !v.is_finite()) {
            return Err(invalid("position and rotation must be finite numbers"));
        }
        if opacity.is_some_and(|v| !(0.0..=1.0).contains(&v)) {
            return Err(invalid("opacity must be within 0.0..=1.0"));
        }
        if [crop_left, crop_right, crop_top, crop_bottom]
            .into_iter()
            .flatten()
            .any(|c| !(0.0..1.0).contains(&c))
        {
            return Err(invalid("crop fractions must be within 0.0..1.0"));
        }
        Ok(())
    }

    /// A transform an edit leaves is refused when its crop takes the whole picture away.
    pub fn check_crop(&self) -> Result<()> {
        if self.crop_left + self.crop_right >= 1.0 || self.crop_top + self.crop_bottom >= 1.0 {
            return Err(invalid("crop removes the entire frame"));
        }
        Ok(())
    }
}

impl Color {
    /// The numbers `Project::set_color` takes, each `None` left as it is.
    pub fn check_values(
        brightness: Option<f64>,
        contrast: Option<f64>,
        saturation: Option<f64>,
        gamma: Option<f64>,
        temperature: Option<f64>,
    ) -> Result<()> {
        if brightness.is_some_and(|v| !(-1.0..=1.0).contains(&v)) {
            return Err(invalid("brightness must be within -1.0..=1.0"));
        }
        if contrast.is_some_and(|v| !(0.0..=4.0).contains(&v)) {
            return Err(invalid("contrast must be within 0.0..=4.0"));
        }
        if saturation.is_some_and(|v| !(0.0..=3.0).contains(&v)) {
            return Err(invalid("saturation must be within 0.0..=3.0"));
        }
        if gamma.is_some_and(|v| !(0.1..=10.0).contains(&v)) {
            return Err(invalid("gamma must be within 0.1..=10.0"));
        }
        if temperature.is_some_and(|v| !(-1.0..=1.0).contains(&v)) {
            return Err(invalid("temperature must be within -1.0..=1.0"));
        }
        Ok(())
    }
}

impl Clip {
    /// The fades `Project::set_fade` takes.
    pub fn check_fades(fade_in: Option<f64>, fade_out: Option<f64>) -> Result<()> {
        if [fade_in, fade_out].into_iter().flatten().any(|v| !v.is_finite() || v < 0.0) {
            return Err(invalid("fade duration must be a number >= 0"));
        }
        Ok(())
    }

    /// The numbers `Project::trim` takes, before the clip's own are looked at.
    pub fn check_trim(source_in: Option<f64>, source_out: Option<f64>, timeline_start: Option<f64>) -> Result<()> {
        if [source_in, source_out, timeline_start]
            .into_iter()
            .flatten()
            .any(|v| !v.is_finite())
        {
            return Err(invalid("source_in, source_out and timeline_start must be finite numbers"));
        }
        if source_in.is_some_and(|v| v < 0.0) {
            return Err(invalid("source_in must be >= 0"));
        }
        Ok(())
    }
}

impl Asset {
    /// A clip of this asset may ask for its footage only as far as it goes (a still has no end).
    pub fn check_source_out(&self, source_out: f64) -> Result<()> {
        if source_out > self.source_limit() + SOURCE_EPS {
            return Err(Error::InvalidArgument(format!(
                "source_out {source_out:.3} s is past the end of \"{}\" ({:.3} s long)",
                self.name, self.duration
            )));
        }
        Ok(())
    }
}

impl TextOverlay {
    /// The numbers `Project::update_overlay` takes, each `None` left as it is.
    pub fn check_values(
        start: Option<f64>,
        end: Option<f64>,
        pos_x: Option<f64>,
        pos_y: Option<f64>,
        size: Option<f64>,
    ) -> Result<()> {
        if size.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            return Err(invalid("size must be a finite value > 0"));
        }
        if [start, end, pos_x, pos_y].into_iter().flatten().any(|v| !v.is_finite()) {
            return Err(invalid("start, end and position must be finite numbers"));
        }
        Ok(())
    }

    /// An overlay an edit leaves must end after it starts.
    pub fn check_span(&self) -> Result<()> {
        if self.end <= self.start {
            return Err(invalid("overlay end must be after start"));
        }
        Ok(())
    }
}

impl TextKeyframe {
    /// A key `Project::set_overlay_keyframes` takes.
    pub fn check(&self) -> Result<()> {
        if !self.time.is_finite() || self.time < 0.0 {
            return Err(invalid("keyframe time must be >= 0"));
        }
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(invalid("opacity must be within 0.0..=1.0"));
        }
        if !self.pos_x.is_finite() || !self.pos_y.is_finite() {
            return Err(invalid("keyframe position must be finite"));
        }
        Ok(())
    }
}
