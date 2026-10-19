//! A title's text as the user shapes it: the box a text wraps or shrinks to, and whether its text is drawn
//! as glyph outlines or with its fonts. For any title but a preset (drawn from its parameters), floating or
//! on a track, custom artwork or a template.
//!
//! The same lock-free shape as a template's values: [`Project::title_text_inputs`] reads the title under
//! the lock, [`TitleTextInputs`] makes the new artwork without it (the text form, the change, the
//! sanitiser, the text engine), and [`Project::set_title_text_parsed`] stores it if the title is still the
//! one it was made from.

use uuid::Uuid;

use super::title_clips::TitleClipResult;
use super::{Project, SvgOverlayResult, TitleEdited};
use crate::error::{Error, Result};
use crate::fonts;
use crate::svg::{restore_text, set_text_box, SvgDoc, TextBox};
use crate::template::{read_values, TemplateControl};

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
    warnings: Vec<String>,
}

impl PreparedText {
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

impl TitleTextInputs {
    /// Give the `<text>` whose id is `element` the box `text_box` (or none) and set the title's text as
    /// outlines again: a box is drawn only by outlines. A text that stays text says so in a warning. Slow
    /// (the sanitiser, the text engine): not for under the project lock.
    pub fn text_box(&self, element: &str, text_box: &TextBox) -> Result<PreparedText> {
        let boxed = set_text_box(&restore_text(&self.svg)?, element, text_box)?;
        let (doc, _, _) = self.read(&boxed)?;
        let warnings = doc.warnings().to_vec();
        Ok(PreparedText {
            base_svg: self.svg.clone(),
            doc,
            warnings,
        })
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
                let doc = SvgDoc::parse(&text)?;
                let warnings = doc.warnings().to_vec();
                Ok(PreparedText {
                    base_svg: self.svg.clone(),
                    doc,
                    warnings,
                })
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
                let mut warnings = doc.warnings().to_vec();
                warnings.extend(kept.into_iter().map(|k| format!("{k} stays text")));
                Ok(PreparedText {
                    base_svg: self.svg.clone(),
                    doc,
                    warnings,
                })
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
        Ok(TitleTextInputs {
            title_id,
            svg: svg.clone(),
            controls: template.as_ref().map(|t| t.controls.clone()),
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
                if let Some(instance) = o.template.as_mut() {
                    instance.values = read_values(&svg, &instance.controls);
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
            if let Some(instance) = title.template.as_mut() {
                instance.values = read_values(&svg, &instance.controls);
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
    use crate::svg::{text_box_of, TextFit};

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
        // The text that cannot be an outline says so.
        assert!(edited.warnings().is_empty(), "{:?}", edited.warnings());
        assert!(project.history().unwrap().iter().any(|r| r.label == "Set text box"));
        // Taken away again, the text form is the artwork as it was, outlined.
        let none = project.set_title_text_box(id, "name", TextBox::default()).unwrap();
        assert_eq!(
            restore_text(svg_of(&none)).unwrap(),
            restore_text(svg)
                .unwrap()
                .replace(r#" inline-size="300" data-kerf-fit="shrink""#, "")
        );
        // Not a text: refused with nothing written.
        assert!(project.set_title_text_box(id, "nope", shrink(10.0)).is_err());
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
}
