//! The files a delivery hands on besides the video: the cut's captions as a
//! subtitle file, the cut itself as OTIO / EDL for another editor, and the
//! markers as a CSV or YouTube chapters. Each is the usual lock-free shape:
//! [`Project::delivery_inputs`] snapshots what it needs under the lock, the
//! static `*_document` builds the text (pure) and [`Project::write_document`]
//! writes it with the lock released. Also the marker edits that carry a
//! marker's duration, comment and kind.

use std::path::Path;

use uuid::Uuid;

use super::Project;
use crate::captions_export::{cut_captions, write_captions, SubtitleFormat};
use crate::error::{Error, Result};
use crate::interchange::markers::{markers_csv, youtube_chapters, MarkerListFormat};
use crate::interchange::{self, InterchangeFormat, InterchangeOptions, Report};
use crate::model::{Asset, Marker, MarkerPatch, Timeline};

/// What a delivery side-file is built from, snapshotted under the lock.
#[derive(Debug, Clone)]
pub struct DeliveryInputs {
    /// The working timeline (an agent's proposal while it has one).
    pub timeline: Timeline,
    /// The original assets, never proxies.
    pub assets: Vec<Asset>,
    /// The frame rate the cut renders at.
    pub rate: f64,
    /// The project's name (its file's), or `Untitled`.
    pub title: String,
}

/// A side-file written, and what its format could not carry.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct ExportedFile {
    pub path: String,
    pub report: Report,
}

impl Project {
    /// Change any of a marker's fields as one `Update marker` revision; each
    /// `None` in `patch` leaves that field alone.
    pub fn edit_marker(&self, marker_id: Uuid, patch: MarkerPatch) -> Result<Marker> {
        self.edit_timeline("Update marker", |timeline| {
            let marker = timeline
                .markers
                .iter_mut()
                .find(|m| m.id == marker_id)
                .ok_or_else(|| Error::InvalidArgument(format!("no marker {marker_id}")))?;
            marker.apply(patch)?;
            let out = marker.clone();
            timeline.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
            Ok(out)
        })
    }

    /// [`Project::add_marker`] with the rest of a marker's fields (`patch.time`
    /// and `patch.name`, when given, win over `time` and `name`).
    pub fn add_marker_with(&self, time: f64, name: String, patch: MarkerPatch) -> Result<Marker> {
        let mut marker = Marker {
            id: Uuid::new_v4(),
            time,
            name,
            ..Default::default()
        };
        marker.apply(MarkerPatch {
            time: patch.time.or(Some(time)),
            ..patch
        })?;
        self.edit_timeline("Add marker", move |timeline| {
            timeline.markers.push(marker.clone());
            timeline.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
            Ok(marker)
        })
    }

    /// Snapshot what the side-file exports read: the working timeline, the
    /// original assets, the frame rate the cut renders at and the project's name.
    pub fn delivery_inputs(&self) -> Result<DeliveryInputs> {
        let timeline = self.working_timeline()?;
        let assets = self.list_assets()?;
        let rate = crate::engine::render_geometry(&timeline.for_render(), &assets, &crate::engine::ExportOptions::default()).fps;
        let title = self
            .path()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_string());
        Ok(DeliveryInputs {
            timeline,
            assets,
            rate,
            title,
        })
    }

    /// The cut's captions as a subtitle file (see [`cut_captions`]). Refused when
    /// the cut has none.
    pub fn captions_document(timeline: &Timeline, format: SubtitleFormat) -> Result<String> {
        let cues = cut_captions(&timeline.for_render());
        if cues.is_empty() {
            return Err(Error::InvalidArgument(
                "the cut has no captions to export; generate or import captions first".to_string(),
            ));
        }
        Ok(write_captions(&cues, format))
    }

    /// The cut as an OTIO or EDL document. `video_track` picks the EDL's track
    /// (0 = the first video track).
    pub fn interchange_document(
        inputs: &DeliveryInputs,
        format: InterchangeFormat,
        video_track: usize,
    ) -> Result<(String, Report)> {
        if !inputs.timeline.tracks.iter().any(|t| !t.clips.is_empty()) {
            return Err(Error::InvalidArgument("the cut has no clips to export".to_string()));
        }
        let opts = InterchangeOptions {
            rate: inputs.rate,
            title: inputs.title.clone(),
            video_track,
        };
        interchange::export(format, &inputs.timeline, &inputs.assets, &opts)
    }

    /// The markers as a CSV or as YouTube chapter text. Refused when there are none.
    pub fn markers_document(inputs: &DeliveryInputs, format: MarkerListFormat) -> Result<(String, Report)> {
        let rendered = inputs.timeline.for_render();
        let markers = &rendered.markers;
        if markers.is_empty() {
            return Err(Error::InvalidArgument("the cut has no markers to export".to_string()));
        }
        match format {
            MarkerListFormat::Csv => Ok((markers_csv(markers, inputs.rate)?, Report::default())),
            MarkerListFormat::Youtube => {
                let (text, report) = youtube_chapters(markers, rendered.duration());
                if text.is_empty() {
                    return Err(Error::InvalidArgument(
                        "no marker falls inside the cut, so there are no chapters".to_string(),
                    ));
                }
                Ok((text, report))
            }
        }
    }

    /// Write a side-file. Run with the project lock released.
    pub fn write_document(path: &Path, text: &str) -> Result<()> {
        std::fs::write(path, text).map_err(|e| Error::InvalidArgument(format!("cannot write {}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clip, MarkerKind, StreamKind, TextOverlay};

    #[test]
    fn markers_carry_duration_comment_and_kind_through_one_revision() {
        let project = Project::open_in_memory().unwrap();
        let m = project
            .add_marker_with(
                2.0,
                "Intro".into(),
                MarkerPatch {
                    kind: Some(MarkerKind::Chapter),
                    comment: Some("first\u{7} line\n".into()),
                    duration: Some(3.0),
                    ..MarkerPatch::default()
                },
            )
            .unwrap();
        assert_eq!(
            (m.kind, m.comment.as_str(), m.duration),
            (MarkerKind::Chapter, "first line", 3.0)
        );
        let edited = project
            .edit_marker(
                m.id,
                MarkerPatch {
                    time: Some(4.0),
                    kind: Some(MarkerKind::Comment),
                    duration: Some(0.0),
                    comment: Some(String::new()),
                    ..MarkerPatch::default()
                },
            )
            .unwrap();
        assert_eq!(
            (edited.time, edited.kind, edited.duration, edited.name.as_str()),
            (4.0, MarkerKind::Comment, 0.0, "Intro")
        );
        let bad = MarkerPatch {
            duration: Some(-1.0),
            ..MarkerPatch::default()
        };
        assert!(project.edit_marker(m.id, bad).is_err());
        assert!(project
            .edit_marker(
                m.id,
                MarkerPatch {
                    comment: Some("x".repeat(crate::model::MAX_MARKER_COMMENT + 1)),
                    ..MarkerPatch::default()
                }
            )
            .is_err());
        let json = serde_json::to_string(&project.timeline().unwrap().markers[0]).unwrap();
        assert!(
            !json.contains("duration") && !json.contains("kind") && !json.contains("comment"),
            "{json}"
        );
        let diff = project.timeline().unwrap();
        assert_eq!(diff.markers[0].time, 4.0);
    }

    #[test]
    fn side_files_are_refused_when_there_is_nothing_to_write() {
        let project = Project::open_in_memory().unwrap();
        let inputs = project.delivery_inputs().unwrap();
        assert_eq!(inputs.title, "Untitled");
        assert!(Project::captions_document(&inputs.timeline, SubtitleFormat::Srt).is_err());
        assert!(Project::interchange_document(&inputs, InterchangeFormat::Edl, 0).is_err());
        assert!(Project::markers_document(&inputs, MarkerListFormat::Csv).is_err());

        let mut tl = Timeline::new();
        tl.tracks[0].clips.push(Clip::new(Uuid::new_v4(), 0.0, 4.0, 0.0));
        tl.overlays.push(TextOverlay {
            generated: true,
            ..TextOverlay::new("hello", 1.0, 2.0)
        });
        assert_eq!(
            Project::captions_document(&tl, SubtitleFormat::Vtt).unwrap(),
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhello\n\n"
        );
        assert_eq!(tl.tracks[0].kind, StreamKind::Video);
    }
}
