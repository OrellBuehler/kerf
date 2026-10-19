//! A title's text as the user shapes it: the box a text wraps or shrinks to, the animation it plays, and
//! whether its text is drawn as glyph outlines or with its fonts. For any title but a preset (drawn from its parameters), floating or
//! on a track, custom artwork or a template.
//!
//! The same lock-free shape as a template's values: [`Project::title_text_inputs`] reads the title under
//! the lock, [`TitleTextInputs`] makes the new artwork without it (the text form, the change, the
//! sanitiser, the text engine), and [`Project::set_title_text_parsed`] stores it if the title is still the
//! one it was made from.

use std::collections::BTreeMap;

use uuid::Uuid;

use super::title_clips::TitleClipResult;
use super::{Project, SvgOverlayResult, TitleEdited};
use crate::error::{Error, Result};
use crate::fonts;
use crate::svg::{has_outlines, restore_text, set_text_animation, set_text_box, SvgDoc, TextAnimation, TextBox};
use crate::template::{read_values, validate_controls_shape, TemplateControl};

/// How a title's text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TitleTextForm {
    /// As the paths of its glyphs: the same on every machine, whatever fonts it has, and the only form in
    /// which a text box wraps or shrinks. A text the engine cannot draw exactly stays text.
    Outlines,
    /// With its fonts, by the renderer.
    Text,
}

/// What changing a title's text needs from the project: the title as it is now. Taken under the lock
/// ([`Project::title_text_inputs`], a cheap read), worked on without it.
#[derive(Debug, Clone)]
pub struct TitleTextInputs {
    pub title_id: Uuid,
    svg: String,
    /// The controls of a template title, to read its values back.
    controls: Option<Vec<TemplateControl>>,
}

/// New artwork for a title's text, sanitised and read. [`Project::set_title_text_parsed`] stores it if the
/// title is still the one it was made from.
#[derive(Debug, Clone)]
pub struct PreparedText {
    base_svg: String,
    doc: SvgDoc,
    /// A template title's values, read back from the new artwork.
    values: Option<BTreeMap<String, String>>,
    warnings: Vec<String>,
}

impl PreparedText {
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

impl TitleTextInputs {
    /// Give the `<text>` whose id is `element` the box `text_box` (or none) and set the title's text as
    /// outlines again: a box is drawn only by outlines, so a box for a text that would stay text is an error
    /// saying why (taking one away is not). Slow (the sanitiser, the text engine): not for under the project
    /// lock.
    pub fn text_box(&self, element: &str, text_box: &TextBox) -> Result<PreparedText> {
        // A box moved into the `style` would leave a control on the attribute with nothing to set.
        let control = self.controls.iter().flatten().find(|c| {
            c.targets
                .iter()
                .any(|t| t.element == element && t.attribute.as_deref() == Some("inline-size"))
        });
        if let Some(c) = control {
            return Err(Error::InvalidArgument(format!(
                "the template's control \u{201c}{}\u{201d} sets the box width of \u{201c}{element}\u{201d}: change it there",
                c.label
            )));
        }
        let boxed = set_text_box(&restore_text(&self.svg)?, element, text_box)?;
        let (doc, _, kept) = self.read(&boxed)?;
        if text_box.width.is_some() {
            if let Some(why) = kept
                .iter()
                .find(|k| k.starts_with(&format!("the text \u{201c}{element}\u{201d} ")))
            {
                return Err(Error::InvalidArgument(format!(
                    "{why} cannot be drawn as outlines, so a box would not be drawn"
                )));
            }
        }
        Ok(self.prepared(doc, Vec::new()))
    }

    /// Give the `<text>` whose id is `element` the animation `animation` (or none): its text is set as outlines
    /// again, which is where the animation is compiled into per-glyph SMIL, so a text that would stay text cannot
    /// play one and is an error saying why (taking one away is not). A title whose text was not outlines is
    /// outlined for an animation; taking the animation away leaves it as it was. Slow (the sanitiser, the text
    /// engine): not for under the project lock.
    pub fn text_animation(&self, element: &str, animation: Option<&TextAnimation>) -> Result<PreparedText> {
        // An attribute a control sets is that control's to change.
        let control = self.controls.iter().flatten().find(|c| {
            c.targets
                .iter()
                .any(|t| t.element == element && t.attribute.as_deref() == Some(crate::svg::ANIMATE_ATTRIBUTE))
        });
        if let Some(c) = control {
            return Err(Error::InvalidArgument(format!(
                "the template's control \u{201c}{}\u{201d} sets the animation of \u{201c}{element}\u{201d}: change it there",
                c.label
            )));
        }
        let animated = set_text_animation(&restore_text(&self.svg)?, element, animation)?;
        if animation.is_none() && !has_outlines(&self.svg) {
            return Ok(self.prepared(SvgDoc::parse(&animated)?, Vec::new()));
        }
        let (doc, _, kept) = self.read(&animated)?;
        if animation.is_some() {
            if let Some(why) = kept
                .iter()
                .find(|k| k.starts_with(&format!("the text \u{201c}{element}\u{201d} ")))
            {
                return Err(Error::InvalidArgument(format!(
                    "{why} cannot be drawn as outlines, so its animation would not play"
                )));
            }
        }
        Ok(self.prepared(doc, Vec::new()))
    }

    /// The new artwork as stored: the title's values read back from it, its warnings and `more`.
    fn prepared(&self, doc: SvgDoc, more: Vec<String>) -> PreparedText {
        let mut warnings = doc.warnings().to_vec();
        warnings.extend(more);
        PreparedText {
            base_svg: self.svg.clone(),
            values: self.controls.as_ref().map(|c| read_values(doc.sanitized(), c)),
            doc,
            warnings,
        }
    }

    /// Draw the title's text in `form`. As outlines, each text that cannot be one says why in a warning,
    /// and a title none of whose text can is an error; as text, the outlines are taken away (a template
    /// title's are made again whenever its values change, so it is refused). Slow: not under the lock.
    pub fn convert(&self, form: TitleTextForm) -> Result<PreparedText> {
        let text = restore_text(&self.svg)?;
        match form {
            TitleTextForm::Text => {
                if self.controls.is_some() {
                    return Err(Error::InvalidArgument(
                        "a template title's text is set as outlines whenever its values change; save it as custom artwork to draw it with its fonts"
                            .to_string(),
                    ));
                }
                if text == self.svg {
                    return Err(Error::InvalidArgument(
                        "the title's text is drawn with its fonts already".to_string(),
                    ));
                }
                Ok(self.prepared(SvgDoc::parse(&text)?, Vec::new()))
            }
            TitleTextForm::Outlines => {
                let (doc, count, kept) = self.read(&text)?;
                if count == 0 {
                    return Err(Error::InvalidArgument(if kept.is_empty() {
                        "the title has no text to set as outlines".to_string()
                    } else {
                        format!("no text in the title can be set as outlines: {}", kept.join("; "))
                    }));
                }
                Ok(self.prepared(doc, kept.into_iter().map(|k| format!("{k} stays text")).collect()))
            }
        }
    }

    /// `svg` sanitised, read and outlined with the fonts the project embeds that it names.
    fn read(&self, svg: &str) -> Result<(SvgDoc, usize, Vec<String>)> {
        let doc = SvgDoc::parse(svg)?;
        let db = fonts::snapshot_for([doc.sanitized()]).db();
        doc.outlined_counted(&db)
    }
}

fn preset_refused(name: &str) -> Error {
    Error::InvalidArgument(format!(
        "\u{201c}{name}\u{201d} is drawn from a preset's parameters: convert it to custom artwork first"
    ))
}

impl Project {
    /// What changing the text of the title `title_id` (floating or a title clip) needs, read from the working
    /// timeline. A preset title is refused: it is drawn again from its parameters. Cheap, for under the lock.
    pub fn title_text_inputs(&self, title_id: Uuid) -> Result<TitleTextInputs> {
        let timeline = self.working_timeline()?;
        let (svg, template) = match timeline.title_clip(title_id) {
            Some((_, title)) => (&title.svg, &title.template),
            None => {
                let o = timeline.svg_overlay(title_id).ok_or(Error::SvgOverlayNotFound(title_id))?;
                if o.preset.is_some() {
                    return Err(preset_refused(&o.display_name()));
                }
                (&o.svg, &o.template)
            }
        };
        let controls = template.as_ref().map(|t| t.controls.clone());
        if let Some(controls) = &controls {
            // Stored with the title, not written by this build: checked before they are read with.
            validate_controls_shape(controls)?;
        }
        Ok(TitleTextInputs {
            title_id,
            svg: svg.clone(),
            controls,
        })
    }

    /// Store `prepared` ([`TitleTextInputs::text_box`] or [`TitleTextInputs::convert`]) as the artwork of the
    /// title `title_id`, if it is still the one it was made from (else an `InvalidArgument`: prepare again).
    /// One edit called `label`, staged for an agent; the title keeps its id, timing, placement and template
    /// (its values are read back from the new artwork).
    pub fn set_title_text_parsed(&self, title_id: Uuid, prepared: PreparedText, label: &str) -> Result<TitleEdited> {
        let changed = || Error::InvalidArgument("the title changed while its text was being set; try again".to_string());
        self.edit_timeline(label, move |timeline| {
            let svg = prepared.doc.sanitized().to_string();
            if let Some(o) = timeline.svg_overlays.iter_mut().find(|o| o.id == title_id) {
                if o.preset.is_some() {
                    return Err(preset_refused(&o.display_name()));
                }
                if o.svg != prepared.base_svg {
                    return Err(changed());
                }
                if let (Some(instance), Some(values)) = (o.template.as_mut(), prepared.values) {
                    instance.values = values;
                }
                o.svg = svg;
                return Ok(TitleEdited::Overlay(Box::new(SvgOverlayResult {
                    overlay: o.clone(),
                    warnings: prepared.warnings,
                })));
            }
            let (ti, ci) = timeline.locate(title_id).ok_or(Error::SvgOverlayNotFound(title_id))?;
            let track_id = timeline.tracks[ti].id;
            let clip = &mut timeline.tracks[ti].clips[ci];
            let title = clip
                .title_clip_mut()
                .ok_or_else(|| Error::InvalidArgument(format!("the clip {title_id} is not a title")))?;
            if title.svg != prepared.base_svg {
                return Err(changed());
            }
            if let (Some(instance), Some(values)) = (title.template.as_mut(), prepared.values) {
                instance.values = values;
            }
            title.svg = svg;
            Ok(TitleEdited::Clip(Box::new(TitleClipResult {
                clip: clip.clone(),
                track_id,
                warnings: prepared.warnings,
            })))
        })
    }

    /// [`Project::title_text_inputs`], [`TitleTextInputs::text_box`] and [`Project::set_title_text_parsed`].
    /// **Tests only**: preparing runs the sanitiser, which a surface must not do under the project lock.
    #[cfg(test)]
    pub fn set_title_text_box(&self, title_id: Uuid, element: &str, text_box: TextBox) -> Result<TitleEdited> {
        let prepared = self.title_text_inputs(title_id)?.text_box(element, &text_box)?;
        self.set_title_text_parsed(title_id, prepared, "Set text box")
    }

    /// The same for [`TitleTextInputs::text_animation`]. **Tests only.**
    #[cfg(test)]
    pub fn set_title_text_animation(
        &self,
        title_id: Uuid,
        element: &str,
        animation: Option<TextAnimation>,
    ) -> Result<TitleEdited> {
        let prepared = self
            .title_text_inputs(title_id)?
            .text_animation(element, animation.as_ref())?;
        self.set_title_text_parsed(title_id, prepared, "Set text animation")
    }

    /// The same for [`TitleTextInputs::convert`]. **Tests only.**
    #[cfg(test)]
    pub fn convert_title_text(&self, title_id: Uuid, form: TitleTextForm) -> Result<TitleEdited> {
        let prepared = self.title_text_inputs(title_id)?.convert(form)?;
        self.set_title_text_parsed(title_id, prepared, "Convert title text")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SvgPreset, SvgPresetKind, SvgPresetParams};
    use crate::project::{SvgLayout, SvgSource, TitleClipPlacement};
    use crate::svg::{text_animation_of, text_animation_preset, text_box_of, TextFit};

    const ART: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 200"><rect width="400" height="200" fill="#123"/><text id="name" x="20" y="100" font-family="Geist" font-size="48" fill="#fff">Somebody with a long name</text><text id="moved" x="20" y="160" dx="2" font-family="Geist">kept</text></svg>"##;

    fn custom(project: &Project) -> Uuid {
        project
            .add_svg_overlay(SvgSource::Custom(ART.to_string()), 0.0, Some(4.0), SvgLayout::default())
            .unwrap()
            .overlay
            .id
    }

    fn svg_of(edited: &TitleEdited) -> &str {
        match edited {
            TitleEdited::Overlay(o) => &o.overlay.svg,
            TitleEdited::Clip(c) => &c.clip.title_clip().unwrap().svg,
        }
    }

    fn shrink(width: f64) -> TextBox {
        TextBox {
            width: Some(width),
            fit: TextFit::Shrink,
        }
    }

    #[test]
    fn a_box_outlines_the_title_and_is_read_back_from_its_text_form() {
        let project = Project::open_in_memory().unwrap();
        let id = custom(&project);
        let edited = project.set_title_text_box(id, "name", shrink(300.0)).unwrap();
        let svg = svg_of(&edited);
        assert!(svg.contains("data-kerf-text"), "{svg}");
        let text = restore_text(svg).unwrap();
        assert_eq!(text_box_of(&text, "name"), Some(shrink(300.0)));
        assert!(edited.warnings().is_empty(), "{:?}", edited.warnings());
        assert!(project.history().unwrap().iter().any(|r| r.label == "Set text box"));
        // Taken away again, the text form is the artwork as it was.
        let none = project.set_title_text_box(id, "name", TextBox::default()).unwrap();
        assert_eq!(restore_text(svg_of(&none)).unwrap(), ART);
        // Not a text: refused with nothing written.
        let revisions = project.history().unwrap().len();
        assert!(project.set_title_text_box(id, "nope", shrink(10.0)).is_err());
        // A text that would stay text cannot draw a box: refused, saying why; taking none away is fine.
        let err = project
            .set_title_text_box(id, "moved", shrink(100.0))
            .unwrap_err()
            .to_string();
        assert!(err.contains("“moved”") && err.contains("dx") && err.contains("box"), "{err}");
        assert_eq!(project.history().unwrap().len(), revisions);
        project.set_title_text_box(id, "moved", TextBox::default()).unwrap();
    }

    #[test]
    fn custom_artwork_with_a_box_says_it_is_drawn_with_its_fonts_until_converted() {
        let project = Project::open_in_memory().unwrap();
        let boxed = ART.replace(r#"<text id="name""#, r#"<text id="name" inline-size="200""#);
        let clip = project
            .add_title_clip(ART, 0.0, Some(4.0), TitleClipPlacement::default())
            .unwrap()
            .clip;
        let patch = crate::project::TitleClipPatch {
            svg: Some(boxed.clone()),
            ..Default::default()
        };
        let updated = project.update_title_clip_parsed(clip.id, patch, None).unwrap();
        assert!(
            updated.warnings.iter().any(|w| w == crate::svg::BOX_NOTE),
            "{:?}",
            updated.warnings
        );
        let added = project
            .add_svg_overlay(SvgSource::Custom(boxed), 0.0, Some(4.0), SvgLayout::default())
            .unwrap();
        assert!(added.warnings.iter().any(|w| w == crate::svg::BOX_NOTE));
        let out = project.convert_title_text(clip.id, TitleTextForm::Outlines).unwrap();
        assert!(
            !out.warnings().iter().any(|w| w == crate::svg::BOX_NOTE),
            "{:?}",
            out.warnings()
        );
        // A box of `auto` is none, however it is written.
        for auto in [r#" inline-size="auto""#, r#" style="inline-size: AUTO !important""#] {
            let svg = ART.replace(r#"<text id="name""#, &format!(r#"<text id="name"{auto}"#));
            let doc = crate::svg::SvgDoc::parse(&svg).unwrap();
            assert!(!doc.warnings().iter().any(|w| w == crate::svg::BOX_NOTE), "{auto}");
        }
        // Plain artwork says nothing of boxes.
        let plain = project
            .add_svg_overlay(SvgSource::Custom(ART.to_string()), 0.0, Some(4.0), SvgLayout::default())
            .unwrap();
        assert!(!plain.warnings.iter().any(|w| w == crate::svg::BOX_NOTE));
    }

    #[test]
    fn a_custom_title_converts_to_outlines_and_back_and_says_what_stays_text() {
        let project = Project::open_in_memory().unwrap();
        let id = custom(&project);
        let out = project.convert_title_text(id, TitleTextForm::Outlines).unwrap();
        assert!(svg_of(&out).contains("data-kerf-text"));
        assert!(
            out.warnings()
                .iter()
                .any(|w| w.contains("“moved”") && w.contains("stays text")),
            "{:?}",
            out.warnings()
        );
        let back = project.convert_title_text(id, TitleTextForm::Text).unwrap();
        assert_eq!(svg_of(&back), ART);
        let err = project.convert_title_text(id, TitleTextForm::Text).unwrap_err().to_string();
        assert!(err.contains("already"), "{err}");
        // Nothing that can be an outline: an error naming why.
        let only_kept = project
            .add_svg_overlay(
                SvgSource::Custom(ART.replace(r#"<text id="name""#, r#"<text id="name" dy="1""#)),
                0.0,
                Some(4.0),
                SvgLayout::default(),
            )
            .unwrap()
            .overlay
            .id;
        let err = project
            .convert_title_text(only_kept, TitleTextForm::Outlines)
            .unwrap_err()
            .to_string();
        assert!(err.contains("dy") && err.contains("dx"), "{err}");
    }

    #[test]
    fn a_box_a_template_control_sets_is_changed_there() {
        let project = Project::open_in_memory().unwrap();
        let mut t = crate::template::fixtures::template();
        t.svg = t.svg.replace(r#"<text id="tag""#, r#"<text id="tag" inline-size="100""#);
        t.manifest.controls.push(crate::template::TemplateControl {
            id: "width".into(),
            label: "Line width".into(),
            kind: crate::template::TemplateKind::Number,
            targets: vec![crate::template::TemplateTarget {
                element: "tag".into(),
                attribute: Some("inline-size".into()),
            }],
            min: None,
            max: None,
        });
        let o = project
            .apply_template(&t, &Default::default(), 0.0, None, SvgLayout::default())
            .unwrap()
            .overlay;
        let err = project.set_title_text_box(o.id, "tag", shrink(60.0)).unwrap_err().to_string();
        assert!(err.contains("“Line width”") && err.contains("change it there"), "{err}");
        // The control still sets it.
        let set = project
            .set_title_values(o.id, &[("width".to_string(), "70".to_string())].into())
            .unwrap();
        assert!(restore_text(svg_of(&set)).unwrap().contains(r#"inline-size="70""#));
    }

    #[test]
    fn a_template_title_keeps_its_template_and_values_and_cannot_go_back_to_text() {
        let project = Project::open_in_memory().unwrap();
        let t = crate::template::fixtures::template();
        let o = project
            .apply_template(
                &t,
                &[("tagline".to_string(), "Kite fun".to_string())].into(),
                0.0,
                None,
                SvgLayout::default(),
            )
            .unwrap()
            .overlay;
        let edited = project.set_title_text_box(o.id, "tag", shrink(60.0)).unwrap();
        let TitleEdited::Overlay(after) = &edited else {
            panic!("a floating title")
        };
        let instance = after.overlay.template.as_ref().unwrap();
        assert_eq!(instance.values["tagline"], "Kite fun");
        assert!(after.overlay.svg.contains("data-kerf-text"));
        let err = project.convert_title_text(o.id, TitleTextForm::Text).unwrap_err().to_string();
        assert!(err.contains("template"), "{err}");
        // Its values set again keep the box, and the text is still shrunk.
        let again = project
            .set_title_values(o.id, &[("tagline".to_string(), "Kite fun forever".to_string())].into())
            .unwrap();
        assert_eq!(text_box_of(&restore_text(svg_of(&again)).unwrap(), "tag"), Some(shrink(60.0)));
    }

    #[test]
    fn a_title_clip_on_a_locked_track_takes_a_box_as_it_takes_new_artwork() {
        let project = Project::open_in_memory().unwrap();
        let placed = project
            .add_title_clip(ART, 0.0, Some(4.0), TitleClipPlacement::default())
            .unwrap();
        project.set_track_locked(placed.track_id, true).unwrap();
        let edited = project.set_title_text_box(placed.clip.id, "name", shrink(250.0)).unwrap();
        assert!(svg_of(&edited).contains("data-kerf-text"));
        let converted = project.convert_title_text(placed.clip.id, TitleTextForm::Text).unwrap();
        assert_eq!(restore_text(svg_of(&converted)).unwrap(), svg_of(&converted));
        let patch = crate::project::TitleClipPatch {
            name: Some("Renamed".into()),
            ..Default::default()
        };
        project.update_title_clip_parsed(placed.clip.id, patch, None).unwrap();
        let tl = project.timeline().unwrap();
        let (clip, title) = tl.title_clips().next().unwrap();
        assert_eq!((clip.timeline_start, clip.source_in, clip.source_out), (0.0, 0.0, 4.0));
        assert_eq!(title.name, "Renamed");
    }

    #[test]
    fn a_title_clip_takes_a_box_and_a_preset_or_a_changed_title_is_refused() {
        let project = Project::open_in_memory().unwrap();
        let clip = project
            .add_title_clip(ART, 0.0, Some(4.0), TitleClipPlacement::default())
            .unwrap()
            .clip;
        let edited = project.set_title_text_box(clip.id, "name", shrink(250.0)).unwrap();
        assert!(matches!(edited, TitleEdited::Clip(_)));
        assert!(svg_of(&edited).contains("data-kerf-text"));

        let preset = project
            .add_svg_overlay(
                SvgSource::Preset(SvgPreset {
                    kind: SvgPresetKind::FadeUp,
                    params: SvgPresetParams::new("Hi"),
                }),
                0.0,
                Some(4.0),
                SvgLayout::default(),
            )
            .unwrap()
            .overlay;
        let err = project.title_text_inputs(preset.id).unwrap_err().to_string();
        assert!(err.contains("preset"), "{err}");

        let id = custom(&project);
        let stale = project
            .title_text_inputs(id)
            .unwrap()
            .text_box("name", &shrink(100.0))
            .unwrap();
        project.convert_title_text(id, TitleTextForm::Outlines).unwrap();
        let err = project
            .set_title_text_parsed(id, stale, "Set text box")
            .unwrap_err()
            .to_string();
        assert!(err.contains("try again"), "{err}");
    }

    fn rise() -> TextAnimation {
        text_animation_preset("rise").unwrap().animation
    }

    #[test]
    fn an_animation_outlines_the_title_plays_per_glyph_and_is_read_back_from_its_text_form() {
        let project = Project::open_in_memory().unwrap();
        let id = custom(&project);
        let edited = project.set_title_text_animation(id, "name", Some(rise())).unwrap();
        let svg = svg_of(&edited);
        assert!(svg.contains("data-kerf-text") && svg.contains("<animateTransform"), "{svg}");
        assert!(edited.warnings().is_empty(), "{:?}", edited.warnings());
        assert_eq!(text_animation_of(&restore_text(svg).unwrap(), "name"), Some(rise()));
        let doc = crate::svg::SvgDoc::parse(svg).unwrap();
        assert_eq!(doc.natural_duration(), Some(rise().end()));
        assert!(project.history().unwrap().iter().any(|r| r.label == "Set text animation"));
        // Taken away: the title is the text, outlined, as it was.
        let none = project.set_title_text_animation(id, "name", None).unwrap();
        assert!(!svg_of(&none).contains("animateTransform") && !svg_of(&none).contains("data-kerf-animate"));
        assert_eq!(restore_text(svg_of(&none)).unwrap(), ART);
        // A text that would stay text cannot play one: refused, saying why, with nothing written.
        let revisions = project.history().unwrap().len();
        let err = project
            .set_title_text_animation(id, "moved", Some(rise()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("“moved”") && err.contains("dx") && err.contains("would not play"),
            "{err}"
        );
        assert!(project.set_title_text_animation(id, "nope", Some(rise())).is_err());
        assert!(project
            .set_title_text_animation(id, "name", Some(TextAnimation::default()))
            .is_err());
        assert_eq!(project.history().unwrap().len(), revisions);
    }

    #[test]
    fn taking_an_animation_off_custom_artwork_that_was_never_outlined_leaves_its_form_alone() {
        let project = Project::open_in_memory().unwrap();
        let withit = ART.replace(
            r#"<text id="name""#,
            &format!(r#"<text id="name" data-kerf-animate='{}'"#, rise().to_attribute()),
        );
        let id = project
            .add_svg_overlay(SvgSource::Custom(withit), 0.0, Some(4.0), SvgLayout::default())
            .unwrap();
        assert!(
            id.warnings.iter().any(|w| w == crate::svg::ANIMATE_NOTE),
            "drawn with fonts, it says its animation does not play: {:?}",
            id.warnings
        );
        let none = project.set_title_text_animation(id.overlay.id, "name", None).unwrap();
        assert_eq!(svg_of(&none), ART, "no outlines were made to take it off");
        // Outlined for an animation, with the note gone.
        let played = project.set_title_text_animation(id.overlay.id, "name", Some(rise())).unwrap();
        assert!(svg_of(&played).contains("data-kerf-text"));
        assert!(
            !played.warnings().iter().any(|w| w == crate::svg::ANIMATE_NOTE),
            "{:?}",
            played.warnings()
        );
    }

    #[test]
    fn a_title_clip_plays_an_animation_even_on_a_locked_track_and_a_preset_title_is_refused() {
        let project = Project::open_in_memory().unwrap();
        let placed = project
            .add_title_clip(ART, 0.0, Some(4.0), TitleClipPlacement::default())
            .unwrap();
        project.set_track_locked(placed.track_id, true).unwrap();
        let edited = project
            .set_title_text_animation(placed.clip.id, "name", Some(rise()))
            .unwrap();
        assert!(matches!(edited, TitleEdited::Clip(_)));
        assert!(svg_of(&edited).contains("animateTransform"));
        let tl = project.timeline().unwrap();
        let (clip, _) = tl.title_clips().next().unwrap();
        assert_eq!((clip.timeline_start, clip.source_in, clip.source_out), (0.0, 0.0, 4.0));
        let preset = project
            .add_svg_overlay(
                SvgSource::Preset(SvgPreset {
                    kind: SvgPresetKind::FadeUp,
                    params: SvgPresetParams::new("Hi"),
                }),
                0.0,
                Some(4.0),
                SvgLayout::default(),
            )
            .unwrap()
            .overlay
            .id;
        let err = project
            .set_title_text_animation(preset, "name", Some(rise()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("preset"), "{err}");
    }

    #[test]
    fn a_template_title_keeps_its_animation_through_its_values_and_a_saved_template_carries_it() {
        let project = Project::open_in_memory().unwrap();
        let t = crate::template::fixtures::template();
        let o = project
            .apply_template(
                &t,
                &[("tagline".to_string(), "Kite fun".to_string())].into(),
                0.0,
                None,
                SvgLayout::default(),
            )
            .unwrap()
            .overlay;
        project.set_title_text_animation(o.id, "tag", Some(rise())).unwrap();
        let again = project
            .set_title_values(o.id, &[("tagline".to_string(), "Kite fun forever".to_string())].into())
            .unwrap();
        let svg = svg_of(&again);
        assert!(svg.contains("animateTransform"), "{svg}");
        assert_eq!(text_animation_of(&restore_text(svg).unwrap(), "tag"), Some(rise()));
        assert_eq!(
            svg.matches("attributeName=\"opacity\"").count() - t_opacity_animations(&t),
            "Kite fun forever".chars().filter(|c| !c.is_whitespace()).count(),
            "one opacity animation a letter of the new text"
        );
        // Saved as a template, the animation is on the text: applying it plays it.
        let source = project.template_source(o.id).unwrap();
        let built = crate::template::build_template(
            &source,
            crate::template::TemplateSaveRequest {
                name: "Animated".into(),
                description: String::new(),
                controls: o.template.as_ref().unwrap().controls.clone(),
            },
            Uuid::new_v4(),
        )
        .unwrap();
        assert!(built.template.svg.contains("<text") && built.template.svg.contains("data-kerf-animate"));
        assert!(
            !built.template.svg.contains("animateTransform"),
            "the generated animation is not saved"
        );
        let prepared = built.template.prepare(&Default::default()).unwrap();
        assert!(prepared.doc.sanitized().contains("animateTransform"));
        assert!(prepared.doc.is_animated());
    }

    fn t_opacity_animations(t: &crate::template::Template) -> usize {
        t.svg.matches("attributeName=\"opacity\"").count()
    }

    #[test]
    fn a_control_that_sets_the_animation_attribute_owns_it() {
        let project = Project::open_in_memory().unwrap();
        let mut t = crate::template::fixtures::template();
        t.svg = t.svg.replace(r#"<text id="tag""#, r#"<text id="tag" data-kerf-animate="1""#);
        t.manifest.controls.push(crate::template::TemplateControl {
            id: "anim".into(),
            label: "Animation".into(),
            kind: crate::template::TemplateKind::Number,
            targets: vec![crate::template::TemplateTarget {
                element: "tag".into(),
                attribute: Some("data-kerf-animate".into()),
            }],
            min: None,
            max: None,
        });
        let o = project
            .apply_template(&t, &Default::default(), 0.0, None, SvgLayout::default())
            .unwrap()
            .overlay;
        let err = project
            .set_title_text_animation(o.id, "tag", Some(rise()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("“Animation”") && err.contains("change it there"), "{err}");
    }
}
