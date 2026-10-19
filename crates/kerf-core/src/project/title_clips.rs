//! Titles on a track: an animated SVG title (custom artwork or a motion template) as a generated clip
//! ([`crate::model::Generator::Title`]) on a video track, so it takes transitions and fades, extends the
//! cut and is placed and keyed like any clip.
//!
//! The same lock-free shape as the floating titles: the artwork is read (custom SVG through
//! `SvgDoc::parse`, a template through `Template::prepare` / `TemplateEditInputs::prepare`) **before**
//! the project lock is taken, and what runs under it only places the clip, through `edit_timeline` — so
//! ripple, the linked-clip rules, the history and an agent's staging apply as to any clip.

use uuid::Uuid;

use super::{Project, SvgOverlayResult, MAX_SVG_SECONDS};
use crate::error::{Error, Result};
use crate::model::{too_many_titles, Clip, StreamKind, Timeline, TitleClip, Track, MAX_TITLE_CLIPS};
use crate::svg::SvgDoc;
use crate::template::{PreparedTemplate, PreparedValues};

/// Where a new title clip goes and what it is called; each `None` takes the default.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TitleClipPlacement {
    /// The video track. By default the lowest video track with nothing in the way over the title's span
    /// (an end card after the last shot lands beside it on V1, ready for a transition), else a new video
    /// track on top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<Uuid>,
    /// The title's name; the template's by default, else none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A title clip as written, the track it is on, and what the sanitiser or the machine could not honour.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TitleClipResult {
    pub clip: Clip,
    pub track_id: Uuid,
    pub warnings: Vec<String>,
}

/// Fields of a title clip's content to change; `None` leaves one alone. Its timing, placement and look are
/// the clip's and change through the clip's own edits.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TitleClipPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New custom artwork, sanitised as on add. It replaces the title's and so ends a template (the controls
    /// point into the old artwork).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub svg: Option<String>,
}

/// The title a template-values edit changed: a floating one or one on a track.
#[derive(Debug, Clone)]
pub enum TitleEdited {
    Overlay(Box<SvgOverlayResult>),
    Clip(Box<TitleClipResult>),
}

impl TitleEdited {
    pub fn warnings(&self) -> &[String] {
        match self {
            TitleEdited::Overlay(o) => &o.warnings,
            TitleEdited::Clip(c) => &c.warnings,
        }
    }
}

/// A title clip's length: finite, longer than nothing, within the first 24 hours.
fn check_title_span(start: f64, duration: f64) -> Result<()> {
    if !(start.is_finite() && duration.is_finite()) {
        return Err(Error::InvalidArgument(
            "start and duration must be numbers of seconds".to_string(),
        ));
    }
    if duration <= 0.0 {
        return Err(Error::InvalidArgument("a title runs for more than no time".to_string()));
    }
    if duration > MAX_SVG_SECONDS {
        return Err(Error::InvalidArgument(format!(
            "a title runs within its first {} hours",
            MAX_SVG_SECONDS / 3600.0
        )));
    }
    Ok(())
}

/// Put `clip` on the track `placement` names, or the lowest unlocked video track free over its span, or a
/// new video track on top (named after the count, as `add_track` names one). Returns the track's id.
fn place(timeline: &mut Timeline, track_id: Option<Uuid>, clip: Clip) -> Result<Uuid> {
    let (start, end) = (clip.timeline_start, clip.timeline_end());
    let free = |t: &Track| !t.locked && t.clips.iter().all(|c| c.timeline_end() <= start || c.timeline_start >= end);
    let tid = match track_id {
        Some(id) => {
            timeline.ensure_track_editable(id)?;
            let track = timeline.track(id).ok_or(Error::TrackNotFound(id))?;
            if track.kind != StreamKind::Video {
                return Err(Error::InvalidArgument(format!(
                    "{} is not a video track; a title goes on a video track",
                    track.name
                )));
            }
            id
        }
        None => match timeline.tracks.iter().find(|t| t.kind == StreamKind::Video && free(t)) {
            Some(t) => t.id,
            None => {
                let count = timeline.tracks.iter().filter(|t| t.kind == StreamKind::Video).count();
                let track = Track::new(StreamKind::Video, format!("V{}", count + 1));
                let id = track.id;
                let at = timeline
                    .tracks
                    .iter()
                    .rposition(|t| t.kind == StreamKind::Video)
                    .map_or(0, |i| i + 1);
                timeline.tracks.insert(at, track);
                id
            }
        },
    };
    let track = timeline.track_mut(tid).ok_or(Error::TrackNotFound(tid))?;
    track.clips.push(clip);
    track.sort_by_start();
    Ok(tid)
}

impl Project {
    /// Add custom SVG artwork as a title clip from `start` for `duration` seconds (by default how long its
    /// animations run, held to 2–30 s, else four: [`SvgDoc::default_span`]). `doc` is the artwork read
    /// already ([`SvgDoc::parse`]), so nothing here parses and it is cheap under the project lock. One
    /// `Add title clip` edit, staged for an agent; with ripple on, a title put onto footage pushes the rest
    /// of its track along, and without it one that would land on a clip is refused.
    pub fn add_title_clip_parsed(
        &self,
        doc: SvgDoc,
        start: f64,
        duration: Option<f64>,
        placement: TitleClipPlacement,
    ) -> Result<TitleClipResult> {
        let duration = duration.unwrap_or_else(|| doc.default_span());
        let title = TitleClip {
            name: placement.name.as_deref().map(str::trim).unwrap_or_default().to_string(),
            svg: doc.sanitized().to_string(),
            template: None,
        };
        self.put_title(
            "Add title clip".to_string(),
            title,
            start,
            duration,
            placement.track_id,
            doc.warnings().to_vec(),
            |_| Ok(()),
        )
    }

    /// Add a template as a title clip: `prepared` is [`crate::Template::prepare`]'s answer. It runs from
    /// `start` for `duration` seconds, by default the template's own length; its fonts are stored in the
    /// project and registered for drawing, as `apply_template_parsed` does for a floating title. One
    /// `Add template title “Name”` edit, staged for an agent.
    pub fn apply_template_clip_parsed(
        &self,
        prepared: PreparedTemplate,
        start: f64,
        duration: Option<f64>,
        placement: TitleClipPlacement,
    ) -> Result<TitleClipResult> {
        self.check_font_room(&prepared.fonts)?;
        let duration = duration.unwrap_or(prepared.duration);
        let name = placement
            .name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| prepared.name.clone());
        let title = TitleClip {
            name,
            svg: prepared.doc.sanitized().to_string(),
            template: Some(prepared.instance),
        };
        let carried = prepared.fonts;
        let label = format!("Add template title \u{201c}{}\u{201d}", title.name);
        let result = self.put_title(label, title, start, duration, placement.track_id, prepared.warnings, |p| {
            p.store_fonts(&carried)
        })?;
        self.hold_fonts(&carried);
        Ok(result)
    }

    /// The edit both adds are: check the span, place the clip, run `also` inside the edit's transaction.
    #[allow(clippy::too_many_arguments)]
    fn put_title(
        &self,
        label: String,
        title: TitleClip,
        start: f64,
        duration: f64,
        track_id: Option<Uuid>,
        warnings: Vec<String>,
        also: impl FnOnce(&Project) -> Result<()>,
    ) -> Result<TitleClipResult> {
        check_title_span(start, duration)?;
        let clip = Clip::title(title, start.max(0.0), duration);
        let clip_id = clip.id;
        // The new clip may not land on another clip of its track (checked after the ripple).
        self.edit_lanes.borrow_mut().push(clip_id);
        self.edit_timeline(&label, |timeline| {
            let titles = timeline
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .filter(|c| c.is_generated())
                .count();
            if titles >= MAX_TITLE_CLIPS {
                return Err(Error::InvalidArgument(too_many_titles()));
            }
            also(self)?;
            let track_id = place(timeline, track_id, clip)?;
            let clip = timeline.clip(clip_id).cloned().ok_or(Error::ClipNotFound(clip_id))?;
            Ok(TitleClipResult {
                clip,
                track_id,
                warnings,
            })
        })
    }

    /// Change a title clip's name or artwork. `doc` is [`TitleClipPatch::svg`] read already and replaces the
    /// artwork (`patch.svg` is not looked at again; with `doc` `None` a `patch.svg` is parsed here, for a
    /// caller with no lock). New artwork ends a template. One `Update title clip` edit, staged for an agent;
    /// the clip keeps its id, place, timing and look.
    pub fn update_title_clip_parsed(&self, clip_id: Uuid, patch: TitleClipPatch, doc: Option<SvgDoc>) -> Result<TitleClipResult> {
        let doc = match (doc, &patch.svg) {
            (Some(doc), _) => Some(doc),
            (None, Some(svg)) => Some(SvgDoc::parse(svg)?),
            (None, None) => None,
        };
        if patch.name.is_none() && doc.is_none() {
            return Err(Error::InvalidArgument("nothing to change: give a name or svg".to_string()));
        }
        self.edit_timeline("Update title clip", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let track_id = timeline.tracks[ti].id;
            let clip = &mut timeline.tracks[ti].clips[ci];
            let title = clip.title_clip_mut().ok_or_else(|| not_a_title(clip_id))?;
            if let Some(name) = patch.name {
                title.name = name.trim().to_string();
            }
            let mut warnings = Vec::new();
            if let Some(doc) = &doc {
                title.svg = doc.sanitized().to_string();
                title.template = None;
                warnings = doc.warnings().to_vec();
            }
            Ok(TitleClipResult {
                clip: clip.clone(),
                track_id,
                warnings,
            })
        })
    }

    /// Change the values of a template title, floating or on a track: `prepared` is
    /// [`crate::TemplateEditInputs::prepare`]'s answer for `title_id` (from [`Project::template_edit_inputs`]).
    /// Stored only if the title is still the one it was made from. One `Set template values` edit.
    pub fn set_title_values_parsed(&self, title_id: Uuid, prepared: PreparedValues) -> Result<TitleEdited> {
        if self.working_timeline()?.svg_overlay(title_id).is_some() {
            return self
                .set_template_values_parsed(title_id, prepared)
                .map(|o| TitleEdited::Overlay(Box::new(o)));
        }
        self.edit_timeline("Set template values", move |timeline| {
            let (ti, ci) = timeline.locate(title_id).ok_or(Error::SvgOverlayNotFound(title_id))?;
            let track_id = timeline.tracks[ti].id;
            let clip = &mut timeline.tracks[ti].clips[ci];
            let title = clip.title_clip_mut().ok_or_else(|| not_a_title(title_id))?;
            if title.template.is_none() {
                return Err(Error::InvalidArgument(format!(
                    "\u{201c}{}\u{201d} is not a template title, so it has no values to set",
                    title.display_name()
                )));
            }
            if title.svg != prepared.base_svg {
                return Err(Error::InvalidArgument(
                    "the title changed while its values were being set; try again".to_string(),
                ));
            }
            title.svg = prepared.doc.sanitized().to_string();
            if let Some(instance) = title.template.as_mut() {
                instance.values = prepared.values;
            }
            Ok(TitleEdited::Clip(Box::new(TitleClipResult {
                clip: clip.clone(),
                track_id,
                warnings: prepared.warnings,
            })))
        })
    }

    /// [`Project::add_title_clip_parsed`] for SVG not yet read. **Tests only**: reading runs the sanitiser,
    /// which a surface must not do under the project lock.
    #[cfg(test)]
    pub fn add_title_clip(
        &self,
        svg: &str,
        start: f64,
        duration: Option<f64>,
        placement: TitleClipPlacement,
    ) -> Result<TitleClipResult> {
        self.add_title_clip_parsed(SvgDoc::parse(svg)?, start, duration, placement)
    }

    /// [`Project::set_title_values_parsed`] with the preparing done here. **Tests only.**
    #[cfg(test)]
    pub fn set_title_values(&self, title_id: Uuid, values: &std::collections::BTreeMap<String, String>) -> Result<TitleEdited> {
        let prepared = self.template_edit_inputs(title_id)?.prepare(values)?;
        self.set_title_values_parsed(title_id, prepared)
    }
}

fn not_a_title(clip_id: Uuid) -> Error {
    Error::InvalidArgument(format!("the clip {clip_id} is not a title"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{EditSource, Transition, TransitionKind, GENERATED_ASSET};
    use crate::template::fixtures::template;

    const ART: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 160 90"><rect id="box" width="40" height="20" fill="#e63946"><animate attributeName="x" from="0" to="120" dur="2s" fill="freeze"/></rect></svg>"##;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn video(p: &Project) -> Uuid {
        p.timeline().unwrap().first_track_of(StreamKind::Video).unwrap()
    }

    #[test]
    fn a_title_is_not_put_on_a_named_locked_track_and_the_default_passes_it_by() {
        let p = Project::open_in_memory().unwrap();
        let v1 = video(&p);
        p.set_track_locked(v1, true).unwrap();
        let named = p.add_title_clip(
            ART,
            1.0,
            None,
            TitleClipPlacement {
                track_id: Some(v1),
                ..Default::default()
            },
        );
        assert!(
            matches!(&named, Err(Error::InvalidArgument(why)) if why == "track V1 is locked"),
            "{named:?}"
        );
        let placed = p.add_title_clip(ART, 1.0, None, TitleClipPlacement::default()).unwrap();
        assert_ne!(placed.track_id, v1);
    }

    #[test]
    fn a_title_clip_lands_on_the_lowest_free_video_track_with_its_documents_span() {
        let p = Project::open_in_memory().unwrap();
        let done = p.add_title_clip(ART, 1.0, None, TitleClipPlacement::default()).unwrap();
        assert_eq!(done.track_id, video(&p));
        assert_eq!(done.clip.asset_id, GENERATED_ASSET);
        assert_eq!((done.clip.timeline_start, done.clip.source_in), (1.0, 0.0));
        assert_eq!(done.clip.source_out, 2.0, "the animation's own length, held to at least 2 s");
        let tl = p.timeline().unwrap();
        assert_eq!(tl.title_clips().count(), 1);
        assert_eq!(p.history().unwrap().last().unwrap().label, "Add title clip");
    }

    #[test]
    fn a_title_over_footage_goes_on_a_new_track_on_top_and_an_explicit_track_must_be_video() {
        let p = Project::open_in_memory().unwrap();
        let first = p.add_title_clip(ART, 0.0, Some(4.0), TitleClipPlacement::default()).unwrap();
        let second = p.add_title_clip(ART, 1.0, Some(2.0), TitleClipPlacement::default()).unwrap();
        assert_ne!(first.track_id, second.track_id);
        let tl = p.timeline().unwrap();
        let track = tl.track(second.track_id).unwrap();
        assert_eq!((track.kind, track.name.as_str()), (StreamKind::Video, "V2"));
        let audio = tl.first_track_of(StreamKind::Audio).unwrap();
        let err = p
            .add_title_clip(
                ART,
                9.0,
                None,
                TitleClipPlacement {
                    track_id: Some(audio),
                    name: None,
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("not a video track"), "{err}");
        let err = p
            .add_title_clip(
                ART,
                1.0,
                None,
                TitleClipPlacement {
                    track_id: Some(first.track_id),
                    name: None,
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("overlap"), "{err}");
    }

    #[test]
    fn a_timeline_holds_at_most_fifty_titles_on_its_tracks_and_a_split_past_that_is_refused() {
        let p = Project::open_in_memory().unwrap();
        let ids: Vec<Uuid> = (0..MAX_TITLE_CLIPS)
            .map(|i| {
                p.add_title_clip(ART, i as f64 * 2.0, Some(1.0), TitleClipPlacement::default())
                    .unwrap()
                    .clip
                    .id
            })
            .collect();
        let err = p
            .add_title_clip(ART, 200.0, Some(1.0), TitleClipPlacement::default())
            .unwrap_err();
        assert!(err.to_string().contains("at most 50 titles"), "{err}");
        let err = p.split_at(ids[0], 0.5).unwrap_err();
        assert!(err.to_string().contains("at most 50 titles"), "{err}");
        assert_eq!(
            p.timeline().unwrap().title_clips().count(),
            MAX_TITLE_CLIPS,
            "nothing was added"
        );
        p.remove(ids[0]).unwrap();
        p.split_at(ids[1], 2.5).unwrap();
    }

    #[test]
    fn a_title_clip_trims_extends_and_takes_a_dip_to_white_like_any_clip() {
        let p = Project::open_in_memory().unwrap();
        let a = p
            .add_title_clip(ART, 0.0, Some(2.0), TitleClipPlacement::default())
            .unwrap()
            .clip;
        let b = p
            .add_title_clip(
                ART,
                2.0,
                Some(2.0),
                TitleClipPlacement {
                    track_id: Some(video(&p)),
                    name: Some("End card".into()),
                },
            )
            .unwrap()
            .clip;
        let extended = p.trim(b.id, None, Some(10.0), None).unwrap();
        assert_eq!(
            extended.source_out, 10.0,
            "a title holds its last state as long as it is asked to"
        );
        p.set_transition(
            b.id,
            Some(Transition {
                kind: TransitionKind::DipToWhite,
                duration: 1.0,
            }),
        )
        .unwrap();
        let (left, right) = p.split_at(a.id, 1.0).unwrap();
        assert_eq!((left.source_out, right.source_in), (1.0, 1.0));
        assert!(right.is_generated(), "both halves draw the title");
        let err = p.set_speed(b.id, 2.0).unwrap_err();
        assert!(err.to_string().contains("plays at its own speed"), "{err}");
        let err = p.trim(b.id, None, Some(MAX_SVG_SECONDS + 1.0), None).unwrap_err();
        assert!(err.to_string().contains("24 hours"), "{err}");
    }

    #[test]
    fn renaming_or_replacing_the_artwork_of_a_title_clip_keeps_its_place() {
        let p = Project::open_in_memory().unwrap();
        let clip = p
            .add_title_clip(ART, 3.0, Some(2.0), TitleClipPlacement::default())
            .unwrap()
            .clip;
        let renamed = p
            .update_title_clip_parsed(
                clip.id,
                TitleClipPatch {
                    name: Some(" Lower third ".into()),
                    svg: None,
                },
                None,
            )
            .unwrap();
        assert_eq!(renamed.clip.title_clip().unwrap().name, "Lower third");
        assert_eq!(renamed.clip.timeline_start, 3.0);
        let other = ART.replace("#e63946", "#00ff00");
        let replaced = p
            .update_title_clip_parsed(
                clip.id,
                TitleClipPatch {
                    name: None,
                    svg: Some(other),
                },
                None,
            )
            .unwrap();
        assert!(replaced.clip.title_clip().unwrap().svg.contains("#00ff00"));
        let diff = p.revision_diff(p.history().unwrap().last().unwrap().seq).unwrap();
        assert_eq!(diff.entries[0].detail.as_deref(), Some("artwork changed"));
        let plain = p.add_title_clip(ART, 9.0, None, TitleClipPlacement::default()).unwrap().clip;
        assert!(p
            .update_title_clip_parsed(plain.id, TitleClipPatch::default(), None)
            .unwrap_err()
            .to_string()
            .contains("nothing to change"));
    }

    #[test]
    fn a_template_on_a_track_keeps_its_controls_and_its_values_change_in_one_edit() {
        let p = Project::open_in_memory().unwrap();
        let t = template();
        let done = p
            .apply_template_clip_parsed(t.prepare(&values(&[])).unwrap(), 0.0, None, TitleClipPlacement::default())
            .unwrap();
        let title = done.clip.title_clip().unwrap();
        assert_eq!(title.name, t.manifest.name);
        assert_eq!(done.clip.source_out, t.manifest.duration);
        assert!(title.template.is_some());
        let edited = p.set_title_values(done.clip.id, &values(&[("tagline", "Bye")])).unwrap();
        let TitleEdited::Clip(edited) = edited else {
            panic!("a clip was edited");
        };
        let title = edited.clip.title_clip().unwrap();
        assert_eq!(title.template.as_ref().unwrap().values["tagline"], "Bye");
        assert!(title.svg.contains(">Bye<"));
        let entry = &p.revision_diff(p.history().unwrap().last().unwrap().seq).unwrap().entries[0];
        assert!(entry.detail.as_deref().unwrap().contains("\u{201c}Bye\u{201d}"), "{entry:?}");
        let source = p.template_source(done.clip.id).unwrap();
        assert_eq!(source.svg, title.svg);
    }

    #[test]
    fn an_agents_title_clip_is_staged() {
        let mut p = Project::open_in_memory().unwrap();
        p.set_actor(EditSource::Agent);
        p.add_title_clip(ART, 0.0, None, TitleClipPlacement::default()).unwrap();
        assert!(p.staged().unwrap().is_some());
        assert_eq!(p.timeline().unwrap().title_clips().count(), 0, "the live cut is untouched");
        assert_eq!(p.working_timeline().unwrap().title_clips().count(), 1);
    }

    /// Three titles abutting on V1: [0, 2), [2, 4), [4, 6), each from the start of its document.
    fn three(p: &Project) -> [Uuid; 3] {
        [0.0, 2.0, 4.0].map(|at| {
            p.add_title_clip(ART, at, Some(2.0), TitleClipPlacement::default())
                .unwrap()
                .clip
                .id
        })
    }

    fn span(p: &Project, id: Uuid) -> (f64, f64, f64) {
        let tl = p.timeline().unwrap();
        let c = tl.clip(id).unwrap();
        (c.timeline_start, c.source_in, c.source_out)
    }

    #[test]
    fn a_title_slips_within_its_document() {
        let p = Project::open_in_memory().unwrap();
        let [_, b, _] = three(&p);
        let out = p.slip_clip(b, 1.5).unwrap();
        assert!(!out.clamped);
        assert_eq!(span(&p, b), (2.0, 1.5, 3.5), "later in the animation, same place on the cut");
        let out = p.slip_clip(b, -3.0).unwrap();
        assert!(out.clamped, "no earlier than the document's start");
        assert_eq!(out.applied, -1.5);
        assert_eq!(span(&p, b), (2.0, 0.0, 2.0));
    }

    #[test]
    fn a_title_slides_between_titles_and_its_neighbours_follow() {
        let p = Project::open_in_memory().unwrap();
        let [a, b, c] = three(&p);
        let out = p.slide_clip(b, 0.5).unwrap();
        assert!(!out.clamped);
        assert_eq!(span(&p, a), (0.0, 0.0, 2.5), "the title before holds its last state longer");
        assert_eq!(span(&p, b), (2.5, 0.0, 2.0));
        assert_eq!(
            span(&p, c),
            (4.5, 0.5, 2.0),
            "the title after starts later into its animation"
        );
        let out = p.slide_clip(b, -1.0).unwrap();
        assert!(out.clamped, "the title after has only half a second of document before it");
        assert_eq!(out.applied, -0.5);
        assert_eq!(span(&p, c), (4.0, 0.0, 2.0));
    }

    #[test]
    fn the_cut_between_two_titles_rolls_as_far_as_the_later_ones_document_reaches() {
        let p = Project::open_in_memory().unwrap();
        let [a, b, _] = three(&p);
        let out = p.roll_edit(a, b, 1.0).unwrap();
        assert!(!out.clamped);
        assert_eq!(span(&p, a), (0.0, 0.0, 3.0));
        assert_eq!(span(&p, b), (3.0, 1.0, 2.0));
        let out = p.roll_edit(a, b, -2.0).unwrap();
        assert!(out.clamped, "the later title cannot start before its document does");
        assert_eq!(out.applied, -1.0);
        assert_eq!((span(&p, a), span(&p, b)), ((0.0, 0.0, 2.0), (2.0, 0.0, 2.0)));
    }

    #[test]
    fn a_title_has_no_sound_to_detach_sync_or_measure() {
        let p = Project::open_in_memory().unwrap();
        let first = p
            .add_title_clip(ART, 0.0, Some(4.0), TitleClipPlacement::default())
            .unwrap()
            .clip
            .id;
        let over = p
            .add_title_clip(ART, 1.0, Some(2.0), TitleClipPlacement::default())
            .unwrap()
            .clip
            .id;
        let err = p.detach_audio(first).unwrap_err().to_string();
        assert!(err.contains("a title has no sound to detach"), "{err}");
        let err = p.detach_audio_clips(&[first, over]).unwrap_err().to_string();
        assert!(err.contains("a title has no sound to detach"), "{err}");
        let err = p.audio_sync_inputs(first, over).unwrap_err().to_string();
        assert!(err.contains("is a title, which has no sound to sync by"), "{err}");
        let req = crate::BandBalanceRequest {
            clip_id: Some(first),
            ..Default::default()
        };
        let err = p.band_balance_inputs(&req).map(|_| ()).unwrap_err().to_string();
        assert!(err.contains("a title, which has no sound"), "{err}");
    }

    /// V1: footage [0, 4), then a title [4, 10).
    fn footage_then_title() -> (Timeline, Uuid, Uuid) {
        let mut tl = Timeline::new();
        let v = tl.first_track_of(StreamKind::Video).unwrap();
        let footage = Clip::new(Uuid::new_v4(), 0.0, 4.0, 0.0);
        let title = Clip::title(
            TitleClip {
                name: "End card".into(),
                svg: ART.into(),
                template: None,
            },
            4.0,
            6.0,
        );
        let ids = (footage.id, title.id);
        tl.track_mut(v).unwrap().clips.extend([footage, title]);
        (tl, ids.0, ids.1)
    }

    #[test]
    fn a_batch_detach_skips_a_title_and_says_why() {
        let (mut tl, footage, title) = footage_then_title();
        let done = tl.detach_audio_many(&[title, footage], &|_| true).unwrap();
        assert_eq!(done.detached.len(), 1);
        assert_eq!(done.skipped.len(), 1);
        assert_eq!(done.skipped[0].clip_id, title);
        assert!(done.skipped[0].reason.contains("a title has no sound"), "{:?}", done.skipped);
    }

    #[test]
    fn a_levels_range_past_the_sound_is_refused_though_a_title_runs_on() {
        let (tl, _, _) = footage_then_title();
        assert_eq!((tl.for_render().duration(), tl.untitled_duration()), (10.0, 4.0));
        let range = crate::model::TimeRange { start: 5.0, end: 6.0 };
        let err = Project::measure_levels(&tl, &[], Some(range), false, &|| false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("the cut ends at 4.0s before its titles"), "{err}");
        assert!(tl.range_past_the_sound(3.0).is_none());
    }
}
