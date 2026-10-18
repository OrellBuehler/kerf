//! Motion templates in a project: the fonts it embeds, applying a template as an animated title, and
//! setting the values of a title made from one.
//!
//! Both edits are the usual lock-free shape. The library, the zip, the sanitiser and the splice are
//! pure work done **before** the project lock (`Template::prepare`, `TemplateEditInputs::prepare`);
//! what runs under it ([`Project::apply_template_parsed`], [`Project::set_template_values_parsed`])
//! only checks, writes the fonts and pushes the title, through `edit_timeline` — so ripple, the history,
//! and an agent's staging (the title is proposed, the fonts are stored) work as for any title.
//! The plain `apply_template` / `set_template_values` are `#[cfg(test)]`, like `add_svg_overlay`: a
//! surface must not run the sanitiser under the lock.

use std::collections::HashSet;

use rusqlite::params;
use uuid::Uuid;

use super::{check_svg_rect, check_svg_span, Project, SvgLayout, SvgOverlayResult};
use crate::error::{Error, Result};
use crate::fonts::{self, EmbeddedFont};
use crate::model::{SvgOverlay, MAX_SVG_OVERLAYS};
use crate::template::{PreparedTemplate, PreparedValues, TemplateEditInputs, TemplateSource, MAX_PROJECT_FONT_BYTES};

impl Project {
    fn has_fonts_table(&self) -> bool {
        super::store::has_table(&self.conn, "fonts")
    }

    /// The fonts this project embeds (a table a file from before schema v3 may not have yet: none).
    /// A row whose bytes are not what its key says is skipped, with a warning.
    pub fn embedded_fonts(&self) -> Result<Vec<EmbeddedFont>> {
        if !self.has_fonts_table() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare("SELECT sha256, family, data FROM fonts ORDER BY sha256")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (sha256, family, data) = row?;
            if fonts::sha256_hex(&data) != sha256 {
                tracing::warn!(
                    family,
                    "an embedded font in the project does not match its checksum; leaving it out"
                );
                continue;
            }
            out.push(EmbeddedFont {
                sha256,
                family,
                data: std::sync::Arc::new(data),
            });
        }
        Ok(out)
    }

    /// The size of the fonts the project embeds, in bytes.
    pub fn embedded_font_bytes(&self) -> Result<u64> {
        if !self.has_fonts_table() {
            return Ok(0);
        }
        let n: i64 = self
            .conn
            .query_row("SELECT COALESCE(SUM(LENGTH(data)), 0) FROM fonts", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Store `fonts` in the project's table; one already there (by SHA-256) is left alone. Runs inside
    /// whatever transaction the caller has open.
    pub(super) fn store_fonts(&self, fonts: &[EmbeddedFont]) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare("INSERT OR IGNORE INTO fonts (sha256, family, data) VALUES (?1, ?2, ?3)")?;
        for font in fonts {
            stmt.execute(params![font.sha256, font.family, font.data.as_slice()])?;
        }
        Ok(())
    }

    /// Make this project's fonts the ones titles are drawn with: [`fonts::replace_embedded`] with the
    /// table, so what an earlier project embedded does not leak into this one and a title draws the
    /// same however many projects were opened first. Called when a project is opened; moves the plan
    /// epoch when the set changed, since every title's pictures are keyed by it.
    pub(super) fn register_fonts(&self) -> Result<()> {
        *self.embedded.borrow_mut() = self.embedded_fonts()?;
        self.activate_fonts();
        Ok(())
    }

    /// Make the fonts this project holds the ones titles are drawn with (what it read when it was
    /// opened and what templates added since): no table is read, so this is cheap under the lock.
    pub(super) fn activate_fonts(&self) {
        if fonts::replace_embedded(self.embedded.borrow().clone()) {
            crate::plan_epoch::bump_plan_epoch();
        }
    }

    /// The fonts of `incoming` the project would add, refused if they would take it past
    /// [`MAX_PROJECT_FONT_BYTES`].
    fn check_font_room(&self, incoming: &[EmbeddedFont]) -> Result<()> {
        let have: HashSet<String> = if self.has_fonts_table() {
            let mut stmt = self.conn.prepare("SELECT sha256 FROM fonts")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        } else {
            HashSet::new()
        };
        let added: u64 = incoming
            .iter()
            .filter(|f| !have.contains(&f.sha256))
            .map(|f| f.data.len() as u64)
            .sum();
        if self.embedded_font_bytes()? + added > MAX_PROJECT_FONT_BYTES {
            return Err(Error::InvalidArgument(format!(
                "the project already embeds {} MiB of fonts; at most {} MiB are kept",
                self.embedded_font_bytes()? >> 20,
                MAX_PROJECT_FONT_BYTES >> 20
            )));
        }
        Ok(())
    }

    /// What making a template from the title `overlay_id` needs, read from the working timeline (an
    /// agent's proposal while it has one): the artwork, its name and how long it runs. Cheap, for under
    /// the lock; `Library::save` does the rest without it.
    pub fn template_source(&self, overlay_id: Uuid) -> Result<TemplateSource> {
        let timeline = self.working_timeline()?;
        let o = timeline
            .svg_overlay(overlay_id)
            .ok_or(Error::SvgOverlayNotFound(overlay_id))?;
        Ok(TemplateSource {
            svg: o.svg.clone(),
            name: o.display_name(),
            duration: o.doc_duration(),
        })
    }

    /// What changing the values of the template title `overlay_id` needs: the artwork as it is now and
    /// its controls. `InvalidArgument` for a title that did not come from a template.
    pub fn template_edit_inputs(&self, overlay_id: Uuid) -> Result<TemplateEditInputs> {
        let timeline = self.working_timeline()?;
        let o = timeline
            .svg_overlay(overlay_id)
            .ok_or(Error::SvgOverlayNotFound(overlay_id))?;
        let instance = o.template.clone().ok_or_else(|| not_a_template(o))?;
        Ok(TemplateEditInputs::new(overlay_id, o.svg.clone(), instance))
    }

    /// Add a template as an animated title: `prepared` is [`crate::Template::prepare`]'s answer, already
    /// sanitised and read, so nothing here parses and it is cheap under the project lock. The title
    /// starts at `start` and ends at `end`, by default `start` plus the template's duration; `layout`
    /// places it (the whole frame by default) and may rename it (the template's name by default). The
    /// template's fonts are stored in the project and registered for drawing. One `Add template title`
    /// edit, staged for an agent. A 51st title is refused, as for any.
    pub fn apply_template_parsed(
        &self,
        prepared: PreparedTemplate,
        start: f64,
        end: Option<f64>,
        layout: SvgLayout,
    ) -> Result<SvgOverlayResult> {
        if !start.is_finite() {
            return Err(Error::InvalidArgument("start must be a number of seconds".to_string()));
        }
        let start = start.max(0.0);
        if end.is_some_and(|e| !e.is_finite() || e <= start) {
            return Err(Error::InvalidArgument("overlay end must be after start".to_string()));
        }
        let end = end.unwrap_or(start + prepared.duration);
        check_svg_span(start, end)?;
        let (x, y, width, height, opacity) = (
            layout.x.unwrap_or(0.0),
            layout.y.unwrap_or(0.0),
            layout.width.unwrap_or(1.0),
            layout.height.unwrap_or(1.0),
            layout.opacity.unwrap_or(1.0),
        );
        check_svg_rect(x, y, width, height, opacity)?;
        self.check_font_room(&prepared.fonts)?;
        let name = layout
            .name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| prepared.name.clone());
        let overlay = SvgOverlay {
            id: Uuid::new_v4(),
            name,
            svg: prepared.doc.sanitized().to_string(),
            start,
            end,
            x,
            y,
            width,
            height,
            opacity,
            offset: 0.0,
            preset: None,
            template: Some(prepared.instance),
        };
        let warnings = prepared.warnings;
        let carried = prepared.fonts;
        let label = format!("Add template title “{}”", overlay.name);
        let result = self.edit_timeline(&label, |timeline| {
            if timeline.svg_overlays.len() >= MAX_SVG_OVERLAYS {
                return Err(Error::InvalidArgument(format!(
                    "a timeline holds at most {MAX_SVG_OVERLAYS} animated titles"
                )));
            }
            self.store_fonts(&carried)?;
            timeline.svg_overlays.push(overlay.clone());
            Ok(SvgOverlayResult { overlay, warnings })
        })?;
        // After the edit is stored: the pictures drawn from here on are keyed by the new set.
        {
            let mut held = self.embedded.borrow_mut();
            for font in &carried {
                if !held.iter().any(|f| f.sha256 == font.sha256) {
                    held.push(font.clone());
                }
            }
        }
        if fonts::register_embedded(&carried) {
            crate::plan_epoch::bump_plan_epoch();
        }
        Ok(result)
    }

    /// Change the values of a template title: `prepared` is [`TemplateEditInputs::prepare`]'s answer
    /// (the title as it was, spliced with the new values, sanitised and read). Stored only if the
    /// title is still the one it was made from — a title that changed meanwhile is an `InvalidArgument`
    /// and the caller prepares again. One `Set template values` edit, staged for an agent; the title's
    /// id, timing and placement stay.
    pub fn set_template_values_parsed(&self, overlay_id: Uuid, prepared: PreparedValues) -> Result<SvgOverlayResult> {
        self.edit_timeline("Set template values", move |timeline| {
            let o = timeline
                .svg_overlays
                .iter_mut()
                .find(|o| o.id == overlay_id)
                .ok_or(Error::SvgOverlayNotFound(overlay_id))?;
            if o.template.is_none() {
                return Err(not_a_template(o));
            }
            if o.svg != prepared.base_svg {
                return Err(Error::InvalidArgument(
                    "the title changed while its values were being set; try again".to_string(),
                ));
            }
            o.svg = prepared.doc.sanitized().to_string();
            if let Some(instance) = o.template.as_mut() {
                instance.values = prepared.values;
            }
            Ok(SvgOverlayResult {
                overlay: o.clone(),
                warnings: prepared.warnings,
            })
        })
    }

    /// [`Project::apply_template_parsed`] for a template and values not yet prepared. **Tests only**:
    /// preparing runs the sanitiser, which a surface must not do under the project lock.
    #[cfg(test)]
    pub fn apply_template(
        &self,
        template: &crate::template::Template,
        values: &std::collections::BTreeMap<String, String>,
        start: f64,
        end: Option<f64>,
        layout: SvgLayout,
    ) -> Result<SvgOverlayResult> {
        self.apply_template_parsed(template.prepare(values)?, start, end, layout)
    }

    /// [`Project::set_template_values_parsed`] with the preparing done here. **Tests only**, like
    /// `apply_template`.
    #[cfg(test)]
    pub fn set_template_values(
        &self,
        overlay_id: Uuid,
        values: &std::collections::BTreeMap<String, String>,
    ) -> Result<SvgOverlayResult> {
        let prepared = self.template_edit_inputs(overlay_id)?.prepare(values)?;
        self.set_template_values_parsed(overlay_id, prepared)
    }
}

fn not_a_template(o: &SvgOverlay) -> Error {
    Error::InvalidArgument(format!(
        "“{}” is not a template title, so it has no values to set",
        o.display_name()
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{EditSource, MAX_SVG_SECONDS};
    use crate::template::fixtures::template;
    use crate::template::{ManifestFont, Template, TemplateControl, TemplateKind, TemplateTarget};

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// The fixture template with a font it carries (Geist Bold under another name) and a text in it.
    fn with_font() -> Template {
        let mut t = template();
        t.svg = t.svg.replace("font-family=\"Geist\"", "font-family=\"Zeist\"");
        let font = EmbeddedFont::new("Zeist", fonts::shipped_font_renamed(true, "Zeist"));
        t.manifest.fonts = vec![ManifestFont {
            file: "fonts/Zeist.ttf".into(),
            family: "Zeist".into(),
        }];
        t.fonts = vec![font];
        t.manifest.controls.push(TemplateControl {
            id: "face".into(),
            label: "Typeface".into(),
            kind: TemplateKind::Font,
            targets: vec![TemplateTarget {
                element: "tag".into(),
                attribute: Some("font-family".into()),
            }],
            min: None,
            max: None,
        });
        t
    }

    fn overlays(p: &Project) -> Vec<SvgOverlay> {
        p.timeline().unwrap().svg_overlays
    }

    #[test]
    fn applying_a_template_adds_a_title_that_remembers_its_template_and_stores_its_fonts() {
        let project = Project::open_in_memory().unwrap();
        let t = with_font();
        let before = project.history().unwrap().len();
        let r = project
            .apply_template(
                &t,
                &values(&[("tagline", "Fly <high>"), ("brand", "#ff0000")]),
                2.0,
                None,
                SvgLayout::default(),
            )
            .unwrap();
        let o = &r.overlay;
        assert_eq!((o.start, o.end), (2.0, 6.0), "the manifest's four seconds");
        assert_eq!(
            (o.x, o.y, o.width, o.height, o.opacity),
            (0.0, 0.0, 1.0, 1.0, 1.0),
            "the whole frame"
        );
        assert_eq!(o.name, "Kitefun end card");
        assert!(o.preset.is_none());
        assert!(o.svg.contains("Fly &lt;high&gt;") && o.svg.contains("#ff0000"));
        let instance = o.template.as_ref().unwrap();
        assert_eq!(instance.template_id, t.manifest.id);
        assert_eq!(instance.controls, t.manifest.controls);
        assert_eq!(instance.values["tagline"], "Fly <high>");
        assert_eq!(instance.values["brand"], "#ff0000");
        assert_eq!(instance.values["face"], "Zeist", "the defaults are read back too");
        assert_eq!(overlays(&project), vec![o.clone()]);
        let history = project.history().unwrap();
        assert_eq!(history.len(), before + 1);
        assert_eq!(history.last().unwrap().label, "Add template title “Kitefun end card”");

        // The fonts are in the project and in the registry; applying again stores nothing twice.
        let stored = project.embedded_fonts().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].family, "Zeist");
        assert_eq!(stored[0].sha256, t.fonts[0].sha256);
        assert!(fonts::has_family("Zeist"));
        assert!(fonts::embedded_digest().is_some());
        project
            .apply_template(
                &t,
                &values(&[]),
                10.0,
                Some(12.5),
                SvgLayout {
                    name: Some(" Second ".into()),
                    x: Some(0.1),
                    ..SvgLayout::default()
                },
            )
            .unwrap();
        assert_eq!(project.embedded_fonts().unwrap().len(), 1);
        let second = &overlays(&project)[1];
        assert_eq!(
            (second.start, second.end, second.x, second.name.as_str()),
            (10.0, 12.5, 0.1, "Second")
        );
        fonts::replace_embedded(Vec::new());
    }

    #[test]
    fn a_bad_application_writes_nothing() {
        let project = Project::open_in_memory().unwrap();
        let t = with_font();
        let head = project.history().unwrap().len();
        let none = |r: Result<SvgOverlayResult>| {
            assert!(matches!(r, Err(Error::InvalidArgument(_))), "{r:?}");
            assert!(overlays(&project).is_empty());
            assert!(
                project.embedded_fonts().unwrap().is_empty(),
                "no fonts stored for a refused title"
            );
            assert_eq!(project.history().unwrap().len(), head);
        };
        none(project.apply_template(&t, &values(&[("brand", "javascript:1")]), 0.0, None, SvgLayout::default()));
        none(project.apply_template(&t, &values(&[("nope", "x")]), 0.0, None, SvgLayout::default()));
        none(project.apply_template(&t, &values(&[]), f64::NAN, None, SvgLayout::default()));
        none(project.apply_template(&t, &values(&[]), 3.0, Some(3.0), SvgLayout::default()));
        none(project.apply_template(&t, &values(&[]), 0.0, Some(MAX_SVG_SECONDS + 1.0), SvgLayout::default()));
        none(project.apply_template(
            &t,
            &values(&[]),
            0.0,
            None,
            SvgLayout {
                width: Some(0.0),
                ..SvgLayout::default()
            },
        ));
        none(project.apply_template(
            &t,
            &values(&[]),
            0.0,
            None,
            SvgLayout {
                opacity: Some(2.0),
                ..SvgLayout::default()
            },
        ));
        fonts::replace_embedded(Vec::new());
    }

    #[test]
    fn a_title_past_the_limit_is_refused_and_its_fonts_are_not_kept() {
        let project = Project::open_in_memory().unwrap();
        let t = with_font();
        for _ in 0..MAX_SVG_OVERLAYS {
            project
                .apply_template(&t, &values(&[]), 0.0, None, SvgLayout::default())
                .unwrap();
        }
        let err = project
            .apply_template(&with_font_two(), &values(&[]), 0.0, None, SvgLayout::default())
            .unwrap_err();
        assert!(err.to_string().contains("at most"), "{err}");
        assert_eq!(
            project.embedded_fonts().unwrap().len(),
            1,
            "the second template's font was rolled back"
        );
        fonts::replace_embedded(Vec::new());
    }

    /// The same template with another font file.
    fn with_font_two() -> Template {
        let mut t = with_font();
        t.manifest.id = Uuid::new_v4().to_string();
        t.fonts = vec![EmbeddedFont::new("Zeyst", fonts::shipped_font_renamed(false, "Zeyst"))];
        t.manifest.fonts[0].family = "Zeyst".into();
        t
    }

    #[test]
    fn setting_values_re_splices_the_title_and_keeps_everything_else() {
        let project = Project::open_in_memory().unwrap();
        let t = template();
        let added = project
            .apply_template(&t, &values(&[("tagline", "Hi")]), 1.0, None, SvgLayout::default())
            .unwrap()
            .overlay;
        project
            .update_svg_overlay(
                added.id,
                crate::model::SvgOverlayPatch {
                    x: Some(0.2),
                    opacity: Some(0.5),
                    ..Default::default()
                },
            )
            .unwrap();
        let r = project
            .set_template_values(added.id, &values(&[("tagline", "Bye & <bye>"), ("brand", "teal")]))
            .unwrap();
        let o = &r.overlay;
        assert_eq!((o.id, o.start, o.end, o.x, o.opacity), (added.id, 1.0, 5.0, 0.2, 0.5));
        assert!(o.svg.contains("Bye &amp; &lt;bye&gt;") && o.svg.contains("fill=\"teal\""));
        assert!(!o.svg.contains(">Hi<"));
        let v = &o.template.as_ref().unwrap().values;
        assert_eq!((v["tagline"].as_str(), v["brand"].as_str()), ("Bye & <bye>", "teal"));
        assert_eq!(overlays(&project), vec![o.clone()]);
        // A partial set leaves the other controls alone.
        let r = project
            .set_template_values(added.id, &values(&[("brand", "#010203")]))
            .unwrap();
        assert_eq!(r.overlay.template.as_ref().unwrap().values["tagline"], "Bye & <bye>");
        // The diff says what changed, by the control's label.
        let diff = project.revision_diff(project.history().unwrap().last().unwrap().seq).unwrap();
        let text = format!("{diff:?}");
        assert!(text.contains("Brand colour"), "{text}");
        project
            .set_template_values(added.id, &values(&[("tagline", "Third")]))
            .unwrap();
        let diff = format!(
            "{:?}",
            project.revision_diff(project.history().unwrap().last().unwrap().seq).unwrap()
        );
        assert!(diff.contains("Tagline “Bye & <bye>” → “Third”"), "{diff}");
    }

    #[test]
    fn a_bad_value_a_plain_title_or_a_stale_prepare_is_refused_with_no_revision() {
        let project = Project::open_in_memory().unwrap();
        let t = template();
        let added = project
            .apply_template(&t, &values(&[]), 0.0, None, SvgLayout::default())
            .unwrap()
            .overlay;
        let head = project.history().unwrap().len();
        for bad in [
            values(&[("brand", "url(#x)")]),
            values(&[("nope", "x")]),
            values(&[("tagline", "\u{1}")]),
        ] {
            assert!(
                matches!(project.set_template_values(added.id, &bad), Err(Error::InvalidArgument(_))),
                "{bad:?}"
            );
        }
        assert!(matches!(
            project.set_template_values(Uuid::new_v4(), &values(&[])),
            Err(Error::SvgOverlayNotFound(_))
        ));
        let plain = project
            .add_svg_overlay(crate::SvgSource::Custom(template().svg), 0.0, Some(2.0), SvgLayout::default())
            .unwrap()
            .overlay;
        let head = head + 1;
        let err = project
            .set_template_values(plain.id, &values(&[("tagline", "x")]))
            .unwrap_err();
        assert!(err.to_string().contains("not a template title"), "{err}");
        assert_eq!(project.history().unwrap().len(), head);

        // Prepared against one artwork, applied after the title changed: refused, not blended.
        let inputs = project.template_edit_inputs(added.id).unwrap();
        let prepared = inputs.prepare(&values(&[("tagline", "late")])).unwrap();
        project
            .set_template_values(added.id, &values(&[("tagline", "first")]))
            .unwrap();
        let err = project.set_template_values_parsed(added.id, prepared).unwrap_err();
        assert!(err.to_string().contains("changed while"), "{err}");
        assert_eq!(overlays(&project)[0].template.as_ref().unwrap().values["tagline"], "first");
    }

    #[test]
    fn replacing_the_artwork_of_a_template_title_ends_its_template() {
        let project = Project::open_in_memory().unwrap();
        let added = project
            .apply_template(&template(), &values(&[]), 0.0, None, SvgLayout::default())
            .unwrap()
            .overlay;
        project
            .update_svg_overlay(
                added.id,
                crate::model::SvgOverlayPatch {
                    name: Some("renamed".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(overlays(&project)[0].template.is_some(), "a rename keeps it");
        project
            .update_svg_overlay(
                added.id,
                crate::model::SvgOverlayPatch {
                    svg: Some(crate::template::fixtures::SVG.to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            overlays(&project)[0].template.is_none(),
            "the controls pointed into the old artwork"
        );
        let diff = format!(
            "{:?}",
            project.revision_diff(project.history().unwrap().last().unwrap().seq).unwrap()
        );
        assert!(diff.contains("no longer a template title"), "{diff}");
    }

    #[test]
    fn an_agents_template_is_staged_and_its_values_are_set_on_the_proposal() {
        let mut project = Project::open_in_memory().unwrap();
        project.set_actor(EditSource::Agent);
        project.begin_staging(None, None, None).unwrap();
        let t = template();
        let added = project
            .apply_template(&t, &values(&[("tagline", "staged")]), 0.0, None, SvgLayout::default())
            .unwrap()
            .overlay;
        assert!(overlays(&project).is_empty(), "the cut the user sees did not move");
        assert_eq!(project.working_timeline().unwrap().svg_overlays.len(), 1);
        // The values are read from the proposal (working_timeline), so the agent can set them again.
        let r = project
            .set_template_values(added.id, &values(&[("tagline", "staged again")]))
            .unwrap();
        assert!(r.overlay.svg.contains("staged again"));
        assert!(overlays(&project).is_empty());
        let staged = project.staged().unwrap().unwrap();
        assert_eq!(staged.edits.len(), 2);
        assert!(staged.edits[0].contains("Add template title"));
        project.set_actor(EditSource::User);
        project.apply_staged(false).unwrap();
        let o = &overlays(&project)[0];
        assert!(o.svg.contains("staged again"));
        assert_eq!(o.template.as_ref().unwrap().values["tagline"], "staged again");
    }

    #[test]
    fn the_source_of_a_template_is_read_from_the_working_timeline() {
        let mut project = Project::open_in_memory().unwrap();
        let live = project
            .add_svg_overlay(
                crate::SvgSource::Custom(template().svg),
                1.0,
                Some(4.5),
                SvgLayout {
                    name: Some("Live".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .overlay;
        let source = project.template_source(live.id).unwrap();
        assert_eq!((source.name.as_str(), source.duration), ("Live", 3.5));
        assert_eq!(source.svg, live.svg);
        assert!(matches!(
            project.template_source(Uuid::new_v4()),
            Err(Error::SvgOverlayNotFound(_))
        ));
        project.set_actor(EditSource::Agent);
        project.begin_staging(None, None, None).unwrap();
        project
            .update_svg_overlay(
                live.id,
                crate::model::SvgOverlayPatch {
                    end: Some(6.0),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            project.template_source(live.id).unwrap().duration,
            5.0,
            "the proposal's, not the live one's"
        );
    }

    #[test]
    fn a_project_opens_with_its_fonts_registered_and_replaces_the_last_projects() {
        let dir = std::env::temp_dir().join(format!("kerf-tpl-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.kerf");
        {
            let project = Project::create(&path).unwrap();
            project
                .apply_template(&with_font(), &values(&[]), 0.0, None, SvgLayout::default())
                .unwrap();
        }
        fonts::replace_embedded(Vec::new());
        assert!(!fonts::has_family("Zeist"));
        let first = Project::open(&path).unwrap();
        assert!(fonts::has_family("Zeist"), "opening registers the project's fonts");
        let digest = fonts::embedded_digest();
        assert!(digest.is_some());
        // Another project without fonts: the first one's do not leak into it.
        let epoch = crate::plan_epoch::plan_epoch();
        let other = Project::open_in_memory().unwrap();
        assert!(!fonts::has_family("Zeist") && fonts::embedded_digest().is_none());
        assert!(crate::plan_epoch::plan_epoch() > epoch);
        drop(other);
        drop(first);
        // Reopening gives the same set, so the same keys.
        let _again = Project::open(&path).unwrap();
        assert_eq!(fonts::embedded_digest(), digest);
        // Save As carries the fonts.
        let copy = dir.join("b.kerf");
        _again.save_as(&copy).unwrap();
        fonts::replace_embedded(Vec::new());
        let _copy = Project::open(&copy).unwrap();
        assert!(fonts::has_family("Zeist"));
        fonts::replace_embedded(Vec::new());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn becoming_the_shared_project_makes_its_fonts_the_drawing_fonts() {
        let dir = std::env::temp_dir().join(format!("kerf-tpl-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("b.kerf");
        {
            let b = Project::create(&path).unwrap();
            b.apply_template(&with_font(), &values(&[]), 0.0, None, SvgLayout::default())
                .unwrap();
        }
        let mut shared = Project::open_in_memory().unwrap();
        let b = Project::open(&path).unwrap();
        // Between the open and the swap something else registered another font and dropped b's.
        fonts::replace_embedded(vec![EmbeddedFont::new("Zeyst", fonts::shipped_font_renamed(false, "Zeyst"))]);
        assert!(!fonts::has_family("Zeist"));
        let epoch = crate::plan_epoch::plan_epoch();
        let old = shared.replace_with(b);
        assert!(fonts::has_family("Zeist") && !fonts::has_family("Zeyst"));
        assert!(crate::plan_epoch::plan_epoch() > epoch);
        // Applying a template to the active project keeps the project's own record in step.
        shared
            .apply_template(&with_font_two(), &values(&[]), 0.0, None, SvgLayout::default())
            .unwrap();
        let mut again = Project::open_in_memory().unwrap();
        assert!(!fonts::has_family("Zeist"));
        let old_shared = again.replace_with(shared);
        assert!(
            fonts::has_family("Zeist") && fonts::has_family("Zeyst"),
            "both templates' fonts came along"
        );
        old.close();
        old_shared.close();
        fonts::replace_embedded(Vec::new());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_font_row_that_is_not_what_its_key_says_is_left_out() {
        let project = Project::open_in_memory().unwrap();
        project
            .conn
            .execute(
                "INSERT INTO fonts (sha256, family, data) VALUES ('deadbeef', 'Bad', x'00010203')",
                [],
            )
            .unwrap();
        assert!(project.embedded_fonts().unwrap().is_empty());
        project.register_fonts().unwrap();
        assert_eq!(fonts::embedded_digest(), None);
    }

    #[test]
    fn a_project_holds_a_bounded_amount_of_fonts() {
        let project = Project::open_in_memory().unwrap();
        let big = EmbeddedFont::new("Big", vec![0u8; (MAX_PROJECT_FONT_BYTES / 2 + 1) as usize]);
        let other = EmbeddedFont::new("Bigger", vec![1u8; (MAX_PROJECT_FONT_BYTES / 2 + 1) as usize]);
        project.store_fonts(std::slice::from_ref(&big)).unwrap();
        assert!(
            project.check_font_room(std::slice::from_ref(&big)).is_ok(),
            "already there: nothing added"
        );
        let err = project.check_font_room(std::slice::from_ref(&other)).unwrap_err();
        assert!(err.to_string().contains("MiB of fonts"), "{err}");
        assert_eq!(project.embedded_font_bytes().unwrap(), big.data.len() as u64);
    }
}
