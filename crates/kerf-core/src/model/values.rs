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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{test_asset, video_stream};
    use crate::model::StreamInfo;

    /// What a check said: `""` for a value it took, else the reason it gave.
    fn said(result: Result<()>) -> String {
        match result {
            Ok(()) => String::new(),
            Err(Error::InvalidArgument(why)) => why,
            Err(other) => panic!("not an invalid argument: {other}"),
        }
    }

    #[test]
    fn each_check_takes_the_ends_of_its_range_and_refuses_past_them_with_its_reason() {
        let tf = |f: fn(f64) -> [Option<f64>; 9], v: f64| {
            let [a, b, c, d, e, g, h, i, j] = f(v);
            said(Transform::check_values(a, b, c, d, e, g, h, i, j))
        };
        let scale = |v| [Some(v), None, None, None, None, None, None, None, None];
        let pos = |v| [None, Some(v), None, None, None, None, None, None, None];
        let rotation = |v| [None, None, None, Some(v), None, None, None, None, None];
        let opacity = |v| [None, None, None, None, Some(v), None, None, None, None];
        let crop = |v| [None, None, None, None, None, None, None, Some(v), None];
        let col = |f: fn(f64) -> [Option<f64>; 5], v: f64| {
            let [a, b, c, d, e] = f(v);
            said(Color::check_values(a, b, c, d, e))
        };
        let brightness = |v| [Some(v), None, None, None, None];
        let contrast = |v| [None, Some(v), None, None, None];
        let saturation = |v| [None, None, Some(v), None, None];
        let gamma = |v| [None, None, None, Some(v), None];
        let temperature = |v| [None, None, None, None, Some(v)];
        let key = |time: f64, opacity: f64, x: f64| {
            said(
                TextKeyframe {
                    time,
                    pos_x: x,
                    pos_y: 0.5,
                    opacity,
                }
                .check(),
            )
        };

        let cases: Vec<(&str, String, &str)> = vec![
            ("scale 0.001", tf(scale, 0.001), ""),
            ("scale 0", tf(scale, 0.0), "scale must be a finite value > 0"),
            ("scale NaN", tf(scale, f64::NAN), "scale must be a finite value > 0"),
            ("position -5", tf(pos, -5.0), ""),
            (
                "position inf",
                tf(pos, f64::INFINITY),
                "position and rotation must be finite numbers",
            ),
            (
                "rotation NaN",
                tf(rotation, f64::NAN),
                "position and rotation must be finite numbers",
            ),
            ("opacity 0", tf(opacity, 0.0), ""),
            ("opacity 1", tf(opacity, 1.0), ""),
            ("opacity 1.01", tf(opacity, 1.01), "opacity must be within 0.0..=1.0"),
            ("crop 0", tf(crop, 0.0), ""),
            ("crop 0.99", tf(crop, 0.99), ""),
            ("crop 1", tf(crop, 1.0), "crop fractions must be within 0.0..1.0"),
            ("crop -0.01", tf(crop, -0.01), "crop fractions must be within 0.0..1.0"),
            ("brightness -1", col(brightness, -1.0), ""),
            ("brightness 1.1", col(brightness, 1.1), "brightness must be within -1.0..=1.0"),
            ("contrast 4", col(contrast, 4.0), ""),
            ("contrast 4.1", col(contrast, 4.1), "contrast must be within 0.0..=4.0"),
            ("saturation 3", col(saturation, 3.0), ""),
            (
                "saturation -0.1",
                col(saturation, -0.1),
                "saturation must be within 0.0..=3.0",
            ),
            ("gamma 0.1", col(gamma, 0.1), ""),
            ("gamma 10.1", col(gamma, 10.1), "gamma must be within 0.1..=10.0"),
            ("temperature 1", col(temperature, 1.0), ""),
            (
                "temperature -1.5",
                col(temperature, -1.5),
                "temperature must be within -1.0..=1.0",
            ),
            ("fade 0", said(Clip::check_fades(Some(0.0), None)), ""),
            (
                "fade -1",
                said(Clip::check_fades(None, Some(-1.0))),
                "fade duration must be a number >= 0",
            ),
            (
                "fade inf",
                said(Clip::check_fades(Some(f64::INFINITY), None)),
                "fade duration must be a number >= 0",
            ),
            ("source_in 0", said(Clip::check_trim(Some(0.0), None, None)), ""),
            (
                "source_in -0.1",
                said(Clip::check_trim(Some(-0.1), None, None)),
                "source_in must be >= 0",
            ),
            (
                "timeline_start NaN",
                said(Clip::check_trim(None, None, Some(f64::NAN))),
                "source_in, source_out and timeline_start must be finite numbers",
            ),
            (
                "size 0.01",
                said(TextOverlay::check_values(None, None, None, None, Some(0.01))),
                "",
            ),
            (
                "size 0",
                said(TextOverlay::check_values(None, None, None, None, Some(0.0))),
                "size must be a finite value > 0",
            ),
            (
                "title end inf",
                said(TextOverlay::check_values(None, Some(f64::INFINITY), None, None, None)),
                "start, end and position must be finite numbers",
            ),
            ("key at 0", key(0.0, 1.0, 0.5), ""),
            ("key at -1", key(-1.0, 1.0, 0.5), "keyframe time must be >= 0"),
            ("key opacity 1.5", key(0.0, 1.5, 0.5), "opacity must be within 0.0..=1.0"),
            ("key x NaN", key(0.0, 1.0, f64::NAN), "keyframe position must be finite"),
        ];
        for (case, got, want) in cases {
            assert_eq!(got, want, "{case}");
        }
    }

    #[test]
    fn a_crop_a_title_and_a_footage_end_are_checked_whole() {
        let crop = |l: f64, r: f64| {
            said(
                Transform {
                    crop_left: l,
                    crop_right: r,
                    ..Transform::default()
                }
                .check_crop(),
            )
        };
        assert_eq!(crop(0.49, 0.5), "");
        assert_eq!(crop(0.5, 0.5), "crop removes the entire frame");

        let mut title = TextOverlay::new("Hi", 1.0, 2.0);
        assert_eq!(said(title.check_span()), "");
        title.end = 1.0;
        assert_eq!(said(title.check_span()), "overlay end must be after start");

        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        assert_eq!(said(asset.check_source_out(100.001)), "", "within the probe's float noise");
        assert_eq!(
            said(asset.check_source_out(100.01)),
            "source_out 100.010 s is past the end of \"x.mp4\" (100.000 s long)"
        );
        let still = test_asset(vec![StreamInfo {
            image: true,
            ..video_stream(640, 480, 25.0)
        }]);
        assert_eq!(said(still.check_source_out(1e6)), "", "a still has no end");
    }
}
