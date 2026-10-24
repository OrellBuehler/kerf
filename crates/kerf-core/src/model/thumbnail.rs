//! Thumbnails: pictures designed beside the video and never drawn in it — a frame of the cut as the
//! background, cropped to the thumbnail's shape, under a stack of layers of its own (a headline, a
//! template, artwork, a logo).
//!
//! A thumbnail lives on the [`Timeline`] (`Timeline.thumbnails`), so an edit of one goes through
//! `edit_timeline` like a marker's: it stages for an agent, undoes, and diffs as one
//! `thumbnails_changed` entry. Nothing that renders the cut reads it. Pure and unit-tested; the
//! picture is drawn by `crate::thumbnail`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Delivery, SvgPresetParams, Timeline};
use crate::error::{Error, Result};
use crate::template::TemplateInstance;

/// The most thumbnails a project keeps.
pub const MAX_THUMBNAILS: usize = 20;
/// The most layers one thumbnail stacks.
pub const MAX_THUMBNAIL_LAYERS: usize = 12;
/// The smallest and the largest side of a thumbnail, in pixels.
pub const MIN_THUMBNAIL_SIDE: u32 = 16;
pub const MAX_THUMBNAIL_SIDE: u32 = 4096;
/// The longest side the background frame is rendered at to give a crop its pixels.
pub const MAX_BACKGROUND_SIDE: u32 = 7680;
/// The shape names [`parse_thumbnail_size`] knows, with their sizes.
pub const THUMBNAIL_SIZES: [(&str, u32, u32); 5] = [
    ("youtube", 1280, 720),
    ("vertical", 1080, 1920),
    ("square", 1080, 1080),
    ("4:5", 1080, 1350),
    ("16:9", 1920, 1080),
];

/// Which of the video's own graphics the background frame keeps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VideoGraphics {
    /// The footage alone: no titles, text or captions.
    None,
    /// Titles and text as the video shows them, without captions.
    #[default]
    Titles,
    /// Everything the video shows, captions too.
    All,
}

impl VideoGraphics {
    /// `timeline` with only the graphics this keeps (and no thumbnails).
    pub fn background_of(self, timeline: &Timeline) -> Timeline {
        let mut tl = timeline.clone();
        tl.thumbnails.clear();
        match self {
            VideoGraphics::All => {}
            VideoGraphics::Titles => tl.overlays.retain(|o| !o.generated),
            VideoGraphics::None => {
                tl.overlays.clear();
                tl.svg_overlays.clear();
                for track in &mut tl.tracks {
                    track.clips.retain(|c| c.generator.is_none());
                }
            }
        }
        tl
    }
}

/// One picture drawn over the background: sanitised SVG fit "contain", centred, into the rectangle
/// `x, y, width, height` (fractions of the thumbnail), as an animated title is into the frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThumbnailLayer {
    pub id: Uuid,
    #[serde(default)]
    pub name: String,
    /// The sanitised source: what is stored is what is drawn.
    pub svg: String,
    /// Set for a headline (`svg::render_headline`), whose artwork is made again from these when they change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<SvgPresetParams>,
    /// Set for a layer made from a motion template: its controls and their values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<TemplateInstance>,
    /// The moment of the artwork's own clock that is drawn, in seconds; `None` is the moment it shows most
    /// (its settled state, before any animation out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub opacity: f64,
}

impl ThumbnailLayer {
    /// What kind of artwork the layer holds, for a reader that does not need the source.
    pub fn kind(&self) -> &'static str {
        if self.text.is_some() {
            "text"
        } else if self.template.is_some() {
            "template"
        } else {
            "svg"
        }
    }

    /// The rectangle the layer covers on a `tw` x `th` thumbnail, in pixels (top-left corner, which
    /// may lie off the picture, and size), or `None` when none of it is on the picture.
    pub fn rect_px(&self, tw: u32, th: u32) -> Option<(i64, i64, u32, u32)> {
        let (fw, fh) = (f64::from(tw), f64::from(th));
        let px = |v: f64, of: f64| {
            let p = (v * of).round();
            p.is_finite().then_some(p)
        };
        let (x, y) = (px(self.x, fw)?, px(self.y, fh)?);
        let (w, h) = (px(self.width, fw)?.max(1.0), px(self.height, fh)?.max(1.0));
        if x >= fw || y >= fh || x + w <= 0.0 || y + h <= 0.0 {
            return None;
        }
        Some((x as i64, y as i64, w as u32, h as u32))
    }
}

/// Where a new layer goes when the call places it nowhere: a headline across the lower part of the
/// picture, anything else over the whole of it.
pub fn default_placement(headline: bool) -> (f64, f64, f64, f64) {
    if headline {
        (0.05, 0.62, 0.9, 0.3)
    } else {
        (0.0, 0.0, 1.0, 1.0)
    }
}

/// A thumbnail: a frame of the cut at `time`, cropped to `width` x `height` around a focus point, under
/// its own layers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thumbnail {
    pub id: Uuid,
    #[serde(default)]
    pub name: String,
    /// The background frame, in timeline seconds.
    pub time: f64,
    pub width: u32,
    pub height: u32,
    /// Where the crop is centred when the frame has another shape, as fractions of the frame (0.5 is
    /// the middle); the crop stays inside the frame.
    pub focus_x: f64,
    pub focus_y: f64,
    #[serde(default)]
    pub graphics: VideoGraphics,
    /// Drawn bottom first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<ThumbnailLayer>,
}

impl Thumbnail {
    pub fn layer(&self, id: Uuid) -> Option<&ThumbnailLayer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// The part of a `fw` x `fh` frame the thumbnail shows, `(x, y, w, h)` in frame pixels: the largest
    /// rectangle of its shape, centred on the focus as far as the frame allows.
    pub fn crop_in(&self, fw: f64, fh: f64) -> (f64, f64, f64, f64) {
        let aspect = f64::from(self.width.max(1)) / f64::from(self.height.max(1));
        let (cw, ch) = if fw / fh > aspect {
            (fh * aspect, fh)
        } else {
            (fw, fw / aspect)
        };
        let place = |focus: f64, of: f64, size: f64| (focus.clamp(0.0, 1.0) * of - size / 2.0).clamp(0.0, (of - size).max(0.0));
        (place(self.focus_x, fw, cw), place(self.focus_y, fh, ch), cw, ch)
    }

    /// How much larger than the delivery `frame` the background is rendered, so the crop has a pixel
    /// for each of the thumbnail's: at least 1, and never past [`MAX_BACKGROUND_SIDE`] on the long side.
    pub fn background_scale(&self, frame: (u32, u32)) -> f64 {
        let (fw, fh) = (f64::from(frame.0.max(1)), f64::from(frame.1.max(1)));
        let (_, _, cw, _) = self.crop_in(fw, fh);
        let wanted = (f64::from(self.width) / cw).max(1.0);
        let cap = (f64::from(MAX_BACKGROUND_SIDE) / fw.max(fh)).max(1.0);
        wanted.min(cap)
    }

    /// The field values a stored thumbnail must hold (a project file can hold anything).
    pub fn check(&self) -> Result<()> {
        check_time(self.time)?;
        check_size(self.width, self.height)?;
        check_focus(self.focus_x, self.focus_y)?;
        if self.layers.len() > MAX_THUMBNAIL_LAYERS {
            return Err(invalid(format!("a thumbnail stacks at most {MAX_THUMBNAIL_LAYERS} layers")));
        }
        for l in &self.layers {
            check_layer_rect(l.x, l.y, l.width, l.height, l.opacity)?;
            if let Some(at) = l.at {
                check_moment(at)?;
            }
        }
        Ok(())
    }
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::InvalidArgument(msg.into())
}

pub fn check_time(time: f64) -> Result<()> {
    if !time.is_finite() || time < 0.0 {
        return Err(invalid("a thumbnail's time is a number of timeline seconds, at least 0"));
    }
    Ok(())
}

pub fn check_size(width: u32, height: u32) -> Result<()> {
    let ok = |v: u32| (MIN_THUMBNAIL_SIDE..=MAX_THUMBNAIL_SIDE).contains(&v);
    if !ok(width) || !ok(height) {
        return Err(invalid(format!(
            "a thumbnail is {MIN_THUMBNAIL_SIDE} to {MAX_THUMBNAIL_SIDE} pixels a side, not {width}x{height}"
        )));
    }
    Ok(())
}

pub fn check_focus(x: f64, y: f64) -> Result<()> {
    if ![x, y].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
        return Err(invalid("focus_x and focus_y are fractions of the frame, within 0..=1"));
    }
    Ok(())
}

pub fn check_moment(at: f64) -> Result<()> {
    if !at.is_finite() || !(0.0..=super::MAX_SVG_SECONDS).contains(&at) {
        return Err(invalid("a layer's `at` is a moment of its artwork, in seconds from 0"));
    }
    Ok(())
}

pub fn check_layer_rect(x: f64, y: f64, width: f64, height: f64, opacity: f64) -> Result<()> {
    if ![x, y, width, height, opacity].iter().all(|v| v.is_finite()) {
        return Err(invalid("x, y, width, height and opacity must be numbers"));
    }
    if !(-1.0..=2.0).contains(&x) || !(-1.0..=2.0).contains(&y) {
        return Err(invalid("x and y are fractions of the thumbnail, within -1..=2"));
    }
    if width <= 0.0 || height <= 0.0 || width > 3.0 || height > 3.0 {
        return Err(invalid("width and height are fractions of the thumbnail, > 0 and <= 3"));
    }
    if !(0.0..=1.0).contains(&opacity) {
        return Err(invalid("opacity must be within 0.0..=1.0"));
    }
    Ok(())
}

/// The size a shape name or `WxH` stands for: `youtube` (1280x720), `vertical` (1080x1920),
/// `square` (1080x1080), `4:5` (1080x1350), `16:9` (1920x1080), any shape [`Delivery::parse`] reads, or
/// `1280x720`. Checked against the side limits.
pub fn parse_thumbnail_size(name: &str) -> Result<(u32, u32)> {
    let name = name.trim();
    let size = THUMBNAIL_SIZES
        .iter()
        .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
        .map(|&(_, w, h)| (w, h))
        .or_else(|| Delivery::parse(name).map(|d| (d.width, d.height)))
        .ok_or_else(|| {
            invalid(format!(
                "\u{201c}{name}\u{201d} is not a thumbnail size: youtube, vertical, square, 4:5, 16:9 or WxH like 1280x720"
            ))
        })?;
    check_size(size.0, size.1)?;
    Ok(size)
}

/// The moment of an animation to draw by default, from `(t, coverage)` samples in time order: the
/// latest one within 1 % of the most covered, so a title that settles and then animates out is drawn
/// settled. `0` for no samples.
pub fn settled_moment(samples: &[(f64, f64)]) -> f64 {
    let most = samples.iter().map(|s| s.1).fold(0.0, f64::max);
    samples.iter().rev().find(|s| s.1 >= most * 0.99).map_or(0.0, |s| s.0)
}

impl Timeline {
    pub fn thumbnail(&self, id: Uuid) -> Option<&Thumbnail> {
        self.thumbnails.iter().find(|t| t.id == id)
    }

    /// The sources of every thumbnail layer: what keeps a font a template brought in the project.
    pub fn thumbnail_sources(&self) -> impl Iterator<Item = &str> {
        self.thumbnails.iter().flat_map(|t| t.layers.iter().map(|l| l.svg.as_str()))
    }
}

/// One line on what changed between two lists of thumbnails, for the diff; `None` when nothing did.
pub(super) fn thumbnail_changes(before: &[Thumbnail], after: &[Thumbnail]) -> Option<String> {
    let mut parts = Vec::new();
    for t in after {
        match before.iter().find(|b| b.id == t.id) {
            None => parts.push(format!("added “{}”", t.name)),
            Some(b) if b != t => parts.push(format!("changed “{}”", t.name)),
            Some(_) => {}
        }
    }
    for b in before.iter().filter(|b| !after.iter().any(|t| t.id == b.id)) {
        parts.push(format!("removed “{}”", b.name));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clip, Generator, StreamKind, TextOverlay, TitleClip};

    fn thumb(w: u32, h: u32) -> Thumbnail {
        Thumbnail {
            id: Uuid::new_v4(),
            name: "T".into(),
            time: 1.0,
            width: w,
            height: h,
            focus_x: 0.5,
            focus_y: 0.5,
            graphics: VideoGraphics::default(),
            layers: Vec::new(),
        }
    }

    fn layer(x: f64, y: f64, w: f64, h: f64) -> ThumbnailLayer {
        ThumbnailLayer {
            id: Uuid::new_v4(),
            name: String::new(),
            svg: String::new(),
            text: None,
            template: None,
            at: None,
            x,
            y,
            width: w,
            height: h,
            opacity: 1.0,
        }
    }

    #[test]
    fn the_crop_is_the_largest_of_the_shape_centred_on_the_focus_and_kept_inside() {
        let mut t = thumb(1080, 1920);
        assert_eq!(t.crop_in(1920.0, 1080.0), (656.25, 0.0, 607.5, 1080.0));
        t.focus_x = 0.0;
        assert_eq!(t.crop_in(1920.0, 1080.0).0, 0.0);
        t.focus_x = 0.9;
        assert_eq!(t.crop_in(1920.0, 1080.0).0, 1920.0 - 607.5);
        let same = thumb(1280, 720);
        assert_eq!(same.crop_in(1920.0, 1080.0), (0.0, 0.0, 1920.0, 1080.0));
        let wide = thumb(1280, 720);
        let (x, y, w, h) = wide.crop_in(1080.0, 1920.0);
        assert_eq!((x, w), (0.0, 1080.0));
        assert!((h - 607.5).abs() < 1e-9 && (y - 656.25).abs() < 1e-9);
    }

    #[test]
    fn the_background_is_rendered_large_enough_for_the_crop_and_no_larger_than_the_cap() {
        assert_eq!(thumb(1280, 720).background_scale((1920, 1080)), 1.0);
        let k = thumb(1080, 1920).background_scale((1920, 1080));
        assert!((k - 1920.0 / 1080.0).abs() < 1e-9, "{k}");
        assert!((thumb(4096, 4096).background_scale((3840, 2160)) - 4096.0 / 2160.0).abs() < 1e-9);
        assert_eq!(thumb(2000, 4000).background_scale((6000, 1000)), 1.28);
        assert_eq!(thumb(16, 16).background_scale((7680, 4320)), 1.0);
    }

    #[test]
    fn a_layer_rectangle_rounds_like_a_title_and_is_dropped_when_off_the_picture() {
        assert_eq!(layer(0.05, 0.62, 0.9, 0.3).rect_px(1280, 720), Some((64, 446, 1152, 216)));
        assert_eq!(layer(-0.5, 0.0, 0.6, 1.0).rect_px(100, 100), Some((-50, 0, 60, 100)));
        assert_eq!(layer(1.0, 0.0, 0.5, 0.5).rect_px(100, 100), None);
        assert_eq!(layer(-0.5, 0.0, 0.5, 0.5).rect_px(100, 100), None);
        assert_eq!(layer(0.0, 0.0, 0.001, 0.001).rect_px(100, 100), Some((0, 0, 1, 1)));
    }

    #[test]
    fn sizes_are_named_shapes_or_wxh_within_the_limits() {
        assert_eq!(parse_thumbnail_size("youtube").unwrap(), (1280, 720));
        assert_eq!(parse_thumbnail_size(" Vertical ").unwrap(), (1080, 1920));
        assert_eq!(parse_thumbnail_size("square").unwrap(), (1080, 1080));
        assert_eq!(parse_thumbnail_size("4:5").unwrap(), (1080, 1350));
        assert_eq!(parse_thumbnail_size("640x360").unwrap(), (640, 360));
        assert!(parse_thumbnail_size("banner").is_err());
        assert!(parse_thumbnail_size("8000x100").is_err());
        assert!(parse_thumbnail_size("0x0").is_err());
    }

    #[test]
    fn the_settled_moment_is_the_last_fully_shown_one() {
        assert_eq!(settled_moment(&[]), 0.0);
        let fade_up = [
            (0.0, 0.0),
            (0.5, 0.6),
            (1.0, 1.0),
            (2.0, 0.999),
            (3.0, 1.0),
            (3.8, 0.4),
            (4.0, 0.0),
        ];
        assert_eq!(settled_moment(&fade_up), 3.0);
        assert_eq!(settled_moment(&[(0.0, 0.0), (1.0, 0.0)]), 1.0);
    }

    #[test]
    fn the_background_keeps_only_the_graphics_asked_for() {
        let mut tl = Timeline::new();
        let mut caption = TextOverlay::new("said", 0.0, 1.0);
        caption.generated = true;
        tl.overlays = vec![TextOverlay::new("Title", 0.0, 1.0), caption];
        let v1 = tl.first_track_of(StreamKind::Video).unwrap();
        let mut card = Clip::new(crate::model::GENERATED_ASSET, 0.0, 2.0, 0.0);
        card.generator = Some(Generator::Title(TitleClip {
            name: "Card".into(),
            svg: "<svg/>".into(),
            template: None,
        }));
        tl.track_mut(v1).unwrap().clips.push(card);
        tl.thumbnails.push(thumb(1280, 720));
        let titles = VideoGraphics::Titles.background_of(&tl);
        assert_eq!(titles.overlays.len(), 1);
        assert_eq!(titles.overlays[0].text, "Title");
        assert!(titles.thumbnails.is_empty());
        assert_eq!(titles.tracks[0].clips.len(), 1);
        let none = VideoGraphics::None.background_of(&tl);
        assert!(none.overlays.is_empty() && none.tracks[0].clips.is_empty());
        assert_eq!(VideoGraphics::All.background_of(&tl).overlays.len(), 2);
    }

    #[test]
    fn a_stored_thumbnail_is_checked_and_round_trips() {
        let mut t = thumb(1280, 720);
        t.layers.push(layer(0.1, 0.1, 0.5, 0.5));
        let json = serde_json::to_string(&t).unwrap();
        assert!(
            json.contains("\"graphics\":\"titles\"") && !json.contains("\"text\""),
            "{json}"
        );
        let back: Thumbnail = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
        assert!(t.check().is_ok());
        t.focus_x = 1.5;
        assert!(t.check().is_err());
        t.focus_x = 0.5;
        t.layers[0].opacity = 2.0;
        assert!(t.check().is_err());
        t.layers[0].opacity = 1.0;
        t.time = f64::NAN;
        assert!(t.check().is_err());
    }

    #[test]
    fn the_diff_line_names_what_was_added_changed_and_removed() {
        let a = thumb(1280, 720);
        let mut b = a.clone();
        b.time = 3.0;
        let c = thumb(1080, 1920);
        assert_eq!(thumbnail_changes(&[a.clone()], &[a.clone()]), None);
        assert_eq!(
            thumbnail_changes(&[a.clone()], &[b, c]).as_deref(),
            Some("changed “T”, added “T”")
        );
        assert_eq!(thumbnail_changes(&[a], &[]).as_deref(), Some("removed “T”"));
    }
}
