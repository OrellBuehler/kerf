//! Generated clips: a clip on a video track whose picture is made rather than decoded.
//!
//! A [`Generator`] rides on [`Clip::generator`]; the clip keeps everything else a clip has — its
//! place on the track, its window, transitions, fades, transform keys, colour and effects — so a title
//! dips to white into the next shot, takes a dissolve, extends the cut and moves on a keyed path like any
//! footage. Its `asset_id` is [`GENERATED_ASSET`] (the nil id, which no asset has): nothing looks a
//! generated clip up in the library, and whatever does by mistake finds no asset rather than the wrong one.
//!
//! **Its source is the generator's own clock**: `source_in` / `source_out` are seconds of the document
//! (`source_in` is what a head trim or a slice cut off the front, as `SvgOverlay::offset` is for a floating
//! title), the speed is always 1 and it never plays backwards. It has no sound. It may run as long as a
//! floating title may ([`MAX_SVG_SECONDS`]); past the end of its animations the document holds its last
//! state, so extending the clip holds the end of the title.
//!
//! One kind so far, [`Generator::Title`]: an SVG title drawn by the same rasteriser as the floating ones
//! (`engine/cli/svg_layer.rs`). Another kind is another variant (the JSON is tagged by `kind`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{fmt_time, Clip, StreamKind, Timeline, MAX_SVG_SECONDS};

/// The `asset_id` of every generated clip: the nil id, which no imported asset has.
pub const GENERATED_ASSET: Uuid = Uuid::nil();

/// What a generated clip draws. Tagged by `kind` in the JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Generator {
    /// An animated SVG title.
    Title(TitleClip),
}

/// An animated SVG title on a video track. `svg` is the **sanitised** source (stored == rendered, as for
/// an `SvgOverlay`), drawn "contain" into the whole frame; the clip's transform places it from there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleClip {
    #[serde(default)]
    pub name: String,
    pub svg: String,
    /// The motion template the title was made from, with its controls and values (`None` for custom
    /// artwork), exactly as on a floating title: a value is spliced into `svg`, not kept beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<crate::template::TemplateInstance>,
}

impl TitleClip {
    /// A name to show: the given one, else the template's, else a placeholder.
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            self.name.trim().to_string()
        } else if let Some(t) = &self.template {
            t.name.clone()
        } else {
            "SVG".to_string()
        }
    }
}

impl Generator {
    /// The artwork the generator draws.
    pub fn svg(&self) -> &str {
        match self {
            Generator::Title(t) => &t.svg,
        }
    }

    /// The name to show for it.
    pub fn display_name(&self) -> String {
        match self {
            Generator::Title(t) => t.display_name(),
        }
    }
}

impl Clip {
    /// A title on a track from `start` for `duration` seconds, its document played from 0.
    pub fn title(title: TitleClip, start: f64, duration: f64) -> Self {
        let mut clip = Clip::new(GENERATED_ASSET, 0.0, duration, start);
        clip.generator = Some(Generator::Title(title));
        clip
    }

    /// Whether the clip's picture is generated ([`Clip::generator`]) rather than cut from an asset.
    pub fn is_generated(&self) -> bool {
        self.generator.is_some()
    }

    /// The title this clip draws, when it is one.
    pub fn title_clip(&self) -> Option<&TitleClip> {
        match &self.generator {
            Some(Generator::Title(t)) => Some(t),
            None => None,
        }
    }

    /// [`Clip::title_clip`], to change.
    pub fn title_clip_mut(&mut self) -> Option<&mut TitleClip> {
        match &mut self.generator {
            Some(Generator::Title(t)) => Some(t),
            None => None,
        }
    }

    /// The document's clock at clip-local time `local` (seconds since the clip started).
    pub fn generator_time(&self, local: f64) -> f64 {
        self.source_in + local.max(0.0)
    }
}

impl Timeline {
    /// The clip `id` when it is a generated title.
    pub fn title_clip(&self, id: Uuid) -> Option<(&Clip, &TitleClip)> {
        self.clip(id).and_then(|c| c.title_clip().map(|t| (c, t)))
    }

    /// Every generated title of the timeline's video tracks, in track-then-storage order.
    pub fn title_clips(&self) -> impl Iterator<Item = (&Clip, &TitleClip)> {
        self.tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .flat_map(|t| t.clips.iter())
            .filter_map(|c| c.title_clip().map(|t| (c, t)))
    }

    /// The sources of every animated title the cut draws: the floating ones and the ones on tracks.
    pub fn title_sources(&self) -> impl Iterator<Item = &str> {
        self.svg_overlays.iter().map(|o| o.svg.as_str()).chain(
            self.tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter_map(|c| c.generator.as_ref().map(Generator::svg)),
        )
    }
}

/// What a generated clip must not be, as `(what, why)`: on a track that is not video, at another speed,
/// linked, before its document's start or past [`MAX_SVG_SECONDS`]. Empty for a clip of an asset.
pub(super) fn generated_violations(clip: &Clip, kind: StreamKind) -> Vec<&'static str> {
    let mut out = Vec::new();
    if clip.generator.is_none() {
        return out;
    }
    if kind != StreamKind::Video {
        out.push("is a title, which belongs on a video track");
    }
    if clip.asset_id != GENERATED_ASSET {
        out.push("is a title and also names an asset");
    }
    if clip.speed != 1.0 {
        out.push("is a title, which plays at its own speed");
    }
    if clip.link_id.is_some() {
        out.push("is a title, which cannot be linked");
    }
    if clip.source_in < 0.0 || clip.source_out > MAX_SVG_SECONDS {
        out.push("is a title that runs outside its first 24 hours");
    }
    out
}

/// "title “Name”", for the diff and the messages.
pub(super) fn title_label(clip: &Clip) -> Option<String> {
    clip.generator
        .as_ref()
        .map(|g| format!("title \u{201c}{}\u{201d}", g.display_name()))
}

/// What differs between two generated clips' content, for the diff: the name, the template's values or
/// the artwork.
pub(super) fn generator_changes(before: &Clip, after: &Clip) -> Vec<String> {
    let mut parts = Vec::new();
    match (before.title_clip(), after.title_clip()) {
        (None, None) => {}
        (None, Some(_)) => parts.push("now a title".to_string()),
        (Some(_), None) => parts.push("no longer a title".to_string()),
        (Some(a), Some(b)) => {
            if a.name != b.name {
                parts.push(format!("name \u{201c}{}\u{201d} \u{2192} \u{201c}{}\u{201d}", a.name, b.name));
            }
            if a.svg != b.svg {
                let values = template_value_changes(a, b);
                if values.is_empty() {
                    parts.push("artwork changed".to_string());
                } else {
                    parts.extend(values);
                }
            }
            if a.template.is_some() && b.template.is_none() {
                parts.push("no longer a template title".to_string());
            }
        }
    }
    parts
}

fn template_value_changes(before: &TitleClip, after: &TitleClip) -> Vec<String> {
    let (Some(was), Some(now)) = (&before.template, &after.template) else {
        return Vec::new();
    };
    now.controls
        .iter()
        .filter_map(|c| {
            let (a, b) = (was.values.get(&c.id), now.values.get(&c.id));
            (a != b).then(|| {
                format!(
                    "{} \u{201c}{}\u{201d} \u{2192} \u{201c}{}\u{201d}",
                    c.label,
                    a.map_or("", String::as_str),
                    b.map_or("", String::as_str)
                )
            })
        })
        .collect()
}

/// "Added title “Name” to V2 at 0:01.0 (4.0s)".
pub(super) fn added_line(clip: &Clip, track: &str) -> Option<String> {
    title_label(clip).map(|label| {
        format!(
            "Added {label} to {track} at {} ({:.1}s)",
            fmt_time(clip.timeline_start),
            clip.duration()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    fn title(name: &str) -> TitleClip {
        TitleClip {
            name: name.into(),
            svg: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 9\"/>".into(),
            template: None,
        }
    }

    #[test]
    fn a_title_clip_is_a_clip_of_no_asset_reading_its_documents_clock() {
        let mut clip = Clip::title(title("Hi"), 2.0, 4.0);
        assert!(clip.is_generated());
        assert_eq!(clip.asset_id, GENERATED_ASSET);
        assert_eq!((clip.source_in, clip.source_out, clip.timeline_start), (0.0, 4.0, 2.0));
        clip.source_in = 1.5;
        assert_eq!(clip.generator_time(0.5), 2.0);
        assert_eq!(clip.title_clip().unwrap().display_name(), "Hi");
    }

    #[test]
    fn the_json_tags_the_kind_and_a_plain_clip_writes_nothing_new() {
        let clip = Clip::title(title("Hi"), 0.0, 4.0);
        let json = serde_json::to_value(&clip).unwrap();
        assert_eq!(json["generator"]["kind"], "title");
        assert_eq!(json["generator"]["name"], "Hi");
        assert!(json["generator"].get("template").is_none());
        let back: Clip = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), json);
        let plain = serde_json::to_value(Clip::new(Uuid::new_v4(), 0.0, 1.0, 0.0)).unwrap();
        assert!(plain.get("generator").is_none());
    }

    #[test]
    fn a_title_off_a_video_track_or_sped_up_is_refused() {
        let mut t = Timeline::new();
        let a = t.first_track_of(StreamKind::Audio).unwrap();
        t.track_mut(a).unwrap().clips.push(Clip::title(title("x"), 0.0, 2.0));
        assert!(t.check().unwrap_err().to_string().contains("belongs on a video track"));

        let mut t = Timeline::new();
        let v = t.first_track_of(StreamKind::Video).unwrap();
        let mut clip = Clip::title(title("x"), 0.0, 2.0);
        clip.speed = 2.0;
        t.track_mut(v).unwrap().clips.push(clip);
        assert!(t.check().unwrap_err().to_string().contains("plays at its own speed"));

        let mut t = Timeline::new();
        let mut clip = Clip::title(title("x"), 0.0, 2.0);
        clip.source_out = MAX_SVG_SECONDS + 1.0;
        t.tracks.push(Track::new(StreamKind::Video, "V2"));
        t.tracks.last_mut().unwrap().clips.push(clip);
        assert!(t.check().unwrap_err().to_string().contains("first 24 hours"));
    }

    #[test]
    fn the_title_sources_are_the_floating_titles_and_the_ones_on_tracks() {
        let mut t = Timeline::new();
        let v = t.first_track_of(StreamKind::Video).unwrap();
        t.track_mut(v).unwrap().clips.push(Clip::title(title("x"), 0.0, 2.0));
        assert_eq!(t.title_sources().count(), 1);
        assert_eq!(t.title_clips().count(), 1);
    }
}
