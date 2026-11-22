//! The files a delivery hands on besides the video: the cut's captions as a
//! subtitle file, the cut itself as OTIO / EDL for another editor, and the
//! markers as a CSV or YouTube chapters. Each is the usual lock-free shape:
//! [`Project::delivery_inputs`] snapshots what it needs under the lock, the
//! static `*_document` builds the text (pure) and [`Project::write_document`]
//! writes it with the lock released. Also the marker edits that carry a
//! marker's duration, comment and kind, and the size an export will have
//! ([`Project::export_size_inputs`] then the static
//! [`Project::estimate_export_size`], the same shape).

use std::path::Path;

use uuid::Uuid;

use super::Project;
use crate::captions_export::{cut_captions, write_captions, SubtitleFormat};
use crate::engine::{self, ExportOptions, ExportSizeEstimate, SizeBounds, SizeMeasureProgress};
use crate::error::{Error, Result};
use crate::interchange::markers::{markers_csv, youtube_chapters, MarkerListFormat};
use crate::interchange::{self, InterchangeFormat, InterchangeOptions, Report};
use crate::model::{Asset, Delivery, Marker, MarkerPatch, Timeline};
use crate::platform::PlatformReport;

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

/// What an export-size estimate is made from, snapshotted under the lock: the working timeline
/// and the original assets. The options the dialog is changing are not in it (every change
/// re-runs [`Project::estimate_export_size`] over the same snapshot).
#[derive(Debug, Clone)]
pub struct ExportSizeInputs {
    /// The working timeline (an agent's proposal while it has one).
    pub timeline: Timeline,
    /// The original assets, never proxies.
    pub assets: Vec<Asset>,
}

/// A marker's colour reaches an inline style in the ruler: a colour spec, nothing else (an empty
/// one clears it).
fn check_marker_color(patch: &MarkerPatch) -> Result<()> {
    match patch.color.as_deref() {
        Some(c) if !c.is_empty() => super::check_color("marker color", c),
        _ => Ok(()),
    }
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
        check_marker_color(&patch)?;
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
        check_marker_color(&patch)?;
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

    /// Snapshot what an export-size estimate reads: the working timeline (so an agent is judged
    /// on its own proposal) and the original assets. Cheap, and the only part that needs the lock.
    pub fn export_size_inputs(&self) -> Result<ExportSizeInputs> {
        Ok(ExportSizeInputs {
            timeline: self.working_timeline()?,
            assets: self.list_assets()?,
        })
    }

    /// What an export of the snapshot with `opts` will weigh, file by file: the one file, or with
    /// `deliveries` (the multi-format export's frames) one per delivery. Pure arithmetic over
    /// the arguments (no ffmpeg, no file read), so it runs without the lock. The options that
    /// change it are the container, the encoders, the quality or bitrate, the preset, tune, profile and pixel
    /// format, the size, the frame rate, the audio settings, `include_audio`, `embed_captions` and the
    /// `range`; see `engine::estimate_size`.
    pub fn estimate_export_size(inputs: &ExportSizeInputs, opts: &ExportOptions, deliveries: &[Delivery]) -> ExportSizeEstimate {
        engine::estimate_export(&inputs.timeline, &inputs.assets, opts, deliveries)
    }

    /// [`Project::estimate_export_size`] **measured**: the model's answer, with every file it could only
    /// bound (constant quality, ProRes, GIF, lossless, hardware) replaced by what sample encodes of the
    /// snapshot's cut with `opts` came to (`engine::measure_export_size`: a few one-second windows spread over
    /// the cut, extrapolated, a band from how much they differ). Takes `budget` at most in all
    /// (`engine::DEFAULT_MEASURE_BUDGET`), tells `progress` after each window and stops at `cancel`
    /// ([`Error::Cancelled`]). Static like the estimate and for the same reason: it runs ffmpeg, so it takes
    /// the snapshot, never the project, and holds no lock; reads the originals; an encode that cannot be
    /// made is the model's answer with the reason in its note.
    pub fn measure_export_size(
        inputs: &ExportSizeInputs,
        opts: &ExportOptions,
        deliveries: &[Delivery],
        budget: std::time::Duration,
        progress: &mut dyn FnMut(SizeMeasureProgress),
        cancel: &dyn Fn() -> bool,
    ) -> Result<ExportSizeEstimate> {
        engine::measure_export_size(&inputs.timeline, &inputs.assets, opts, deliveries, budget, progress, cancel)
    }

    /// [`Project::platform_report`] with the size an export is estimated at (`SizeEstimate::bounds`
    /// of the file being judged; `None` leaves size out): a platform with a recorded file-size cap
    /// then warns when the top of the range is over it and errors when the bottom is.
    pub fn platform_report_sized(
        &self,
        frame: Option<(u32, u32)>,
        range: Option<(f64, f64)>,
        size: Option<SizeBounds>,
    ) -> Result<PlatformReport> {
        Ok(crate::platform::report_sized(&self.cut_summary_in(frame, range)?, size))
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
    use crate::engine::{Container, SizeBasis};
    use crate::model::{Clip, EditSource, Fit, MarkerKind, StreamKind, TextOverlay};

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
    fn web_opts() -> ExportOptions {
        ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            crf: Some(20),
            audio_codec: Some("aac".into()),
            audio_bitrate: Some("192k".into()),
            ..ExportOptions::default()
        }
    }

    #[test]
    fn an_export_size_is_computed_from_a_snapshot_without_the_project() {
        let project = Project::sample().unwrap();
        let inputs = project.export_size_inputs().unwrap();
        // The snapshot is all the estimate needs: it does not reach back into the project.
        drop(project);
        let est = Project::estimate_export_size(&inputs, &web_opts(), &[]);
        assert_eq!(est.files.len(), 1);
        let file = &est.files[0];
        assert_eq!(file.estimate.basis, SizeBasis::Modelled);
        assert_eq!(file.duration_secs, inputs.timeline.for_render().duration());
        assert!(file.frame.is_some() && file.estimate.low_bytes > 0);
        assert_eq!(
            est,
            engine::estimate_export(&inputs.timeline, &inputs.assets, &web_opts(), &[])
        );
        // The same snapshot answers every change of options, and the multi-format run per frame.
        let bitrate = ExportOptions {
            rate_control: engine::RateControl::Bitrate,
            video_bitrate: Some("8M".into()),
            ..web_opts()
        };
        assert_eq!(
            Project::estimate_export_size(&inputs, &bitrate, &[]).files[0].estimate.basis,
            SizeBasis::Targeted
        );
        let frames = [Delivery::new(1080, 1920, Fit::Cover), Delivery::new(1080, 1080, Fit::Cover)];
        let many = Project::estimate_export_size(&inputs, &web_opts(), &frames);
        assert_eq!(many.files.len(), 2);
        assert!(many.total.is_some_and(|t| t.high_bytes > many.files[0].estimate.high_bytes));
    }

    #[test]
    fn measuring_what_the_model_knows_exactly_is_the_model_and_runs_nothing() {
        let project = Project::sample().unwrap();
        let inputs = project.export_size_inputs().unwrap();
        // A bitrate target is arithmetic: no sample encode, whatever the stand-in would have said.
        let opts = ExportOptions {
            rate_control: engine::RateControl::Bitrate,
            video_bitrate: Some("4M".into()),
            ..web_opts()
        };
        let model = Project::estimate_export_size(&inputs, &opts, &[]);
        assert_eq!(model.files[0].estimate.basis, SizeBasis::Targeted);
        let mut heard = 0;
        let measured = Project::measure_export_size(
            &inputs,
            &opts,
            &[],
            std::time::Duration::from_secs(5),
            &mut |_| heard += 1,
            &|| false,
        )
        .unwrap();
        assert_eq!(measured, model);
        assert_eq!(heard, 0);
        // A stop before anything ran is a stop, not an estimate.
        let crf = web_opts();
        let stopped = Project::measure_export_size(&inputs, &crf, &[], std::time::Duration::from_secs(5), &mut |_| {}, &|| true);
        assert!(matches!(stopped, Err(Error::Cancelled)), "{stopped:?}");
    }

    #[test]
    fn an_agent_is_sized_on_its_own_proposal() {
        let mut project = Project::sample().unwrap();
        let live = project.export_size_inputs().unwrap().timeline.duration();
        project.set_agent_review(true);
        project.set_actor(EditSource::Agent);
        let last = project
            .working_timeline()
            .unwrap()
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .max_by(|a, b| a.timeline_end().total_cmp(&b.timeline_end()))
            .unwrap()
            .id;
        project.remove(last).unwrap();
        let proposal = project.export_size_inputs().unwrap();
        assert!(
            proposal.timeline.duration() < live,
            "{} < {live}",
            proposal.timeline.duration()
        );
        project.set_actor(EditSource::User);
        assert_eq!(
            project.export_size_inputs().unwrap().timeline.duration(),
            live,
            "the live cut is untouched"
        );
    }

    #[test]
    fn the_platform_report_takes_the_estimate_it_is_given() {
        let project = Project::sample().unwrap();
        let plain = project.platform_report(None, None).unwrap();
        assert_eq!(project.platform_report_sized(None, None, None).unwrap(), plain);
        let huge = SizeBounds {
            low_bytes: 40_000_000_000,
            high_bytes: 90_000_000_000,
            floor_bytes: 40_000_000_000,
        };
        let sized = project.platform_report_sized(None, None, Some(huge)).unwrap();
        assert_eq!(sized.cut, plain.cut);
        let tiktok = sized.targets.iter().find(|c| c.target == "tiktok").unwrap();
        let capped = project_duration_has_a_tiktok_cap(&plain);
        assert_eq!(
            tiktok.issues.iter().any(|i| i.kind == crate::platform::IssueKind::Size),
            capped
        );
        // Targets with no cap recorded answer as before.
        for c in sized.targets.iter().filter(|c| c.target != "tiktok") {
            assert_eq!(c, plain.targets.iter().find(|p| p.target == c.target).unwrap());
        }
    }

    /// Whether the sample cut is short enough for a TikTok cap to be recorded for it.
    fn project_duration_has_a_tiktok_cap(report: &PlatformReport) -> bool {
        crate::platform::target("tiktok")
            .unwrap()
            .size_limit_for(report.cut.duration)
            .is_some()
    }
}
