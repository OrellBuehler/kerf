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

    /// Store `fonts` in the project's table; one already there (by SHA-256) is skipped without being
    /// read again. Runs inside whatever transaction the caller has open.
    pub(super) fn store_fonts(&self, fonts: &[EmbeddedFont]) -> Result<()> {
        let mut have = self.conn.prepare("SELECT 1 FROM fonts WHERE sha256 = ?1")?;
        let mut insert = self
            .conn
            .prepare("INSERT OR IGNORE INTO fonts (sha256, family, data) VALUES (?1, ?2, ?3)")?;
        for font in fonts {
            if have.exists(params![font.sha256])? {
                continue;
            }
            insert.execute(params![font.sha256, font.family, font.data.as_slice()])?;
        }
        Ok(())
    }

    /// Read the fonts the file embeds into memory (`Project.embedded`) **without** making them the ones
    /// titles are drawn with: a project that is only being opened is not the open one yet, and may never
    /// be (the user cancels, the swap is refused), so it must not change how the open project draws.
    /// [`Project::activate_fonts`] does that, and the swap into the shared project calls it.
    pub(super) fn load_fonts(&self) -> Result<()> {
        *self.embedded.borrow_mut() = self.embedded_fonts()?;
        Ok(())
    }

    /// Make the fonts this project holds the ones titles are drawn with (what it read when it was
    /// opened and what templates added since): no table is read, so this is cheap under the lock.
    /// **The registry is the process's**, so this is for the project that is the open one: the swap
    /// ([`Project::replace_with`]) does it for the shared project, and a program that opens a single
    /// project and draws its titles without ever swapping calls it itself. Moves the plan epoch when the
    /// set changed, since every title's pictures are keyed by it.
    pub fn activate_fonts(&self) {
        if fonts::replace_embedded(self.embedded.borrow().clone()) {
            crate::plan_epoch::bump_plan_epoch();
        }
    }

    /// Delete the fonts no title names: a template's typeface stays in the project while a title uses it,
    /// not for good. A font is **kept** when the source of an animated title on the live timeline, or on
    /// the open proposal's, names its family ([`fonts::names_family`]); everything else goes, which is
    /// what keeps a template an agent applied and the user turned down from using up the project's
    /// [`MAX_PROJECT_FONT_BYTES`] for ever. Run when a proposal is discarded, on Save As and when a project
    /// is opened — never between, so undo can still bring a deleted title's font back until then; once
    /// collected it is not stored any more, and the title draws in Geist until the template is applied
    /// again (the same for a discarded proposal that is restored). Returns how many fonts went.
    ///
    /// Touches the file and what this project holds in memory, **not** the process's registry
    /// ([`Project::activate_fonts`] is the caller's to follow with, for the project that is open).
    pub(super) fn collect_unused_fonts(&self) -> Result<usize> {
        if !self.has_fonts_table() {
            return Ok(0);
        }
        let stored: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare("SELECT sha256, family FROM fonts")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        if stored.is_empty() {
            return Ok(0);
        }
        let mut titles: Vec<String> = self.timeline()?.title_sources().map(str::to_lowercase).collect();
        if let Some(row) = self.staged_row()? {
            let proposed: crate::model::Timeline = serde_json::from_str(&row.timeline)?;
            titles.extend(proposed.title_sources().map(str::to_lowercase));
        }
        let mut gone = 0;
        let mut delete = self.conn.prepare("DELETE FROM fonts WHERE sha256 = ?1")?;
        for (sha256, family) in &stored {
            if titles.iter().any(|t| fonts::names_family(t, family)) {
                continue;
            }
            gone += delete.execute(params![sha256])?;
            self.embedded.borrow_mut().retain(|f| f.sha256 != *sha256);
        }
        Ok(gone)
    }

    /// [`Project::collect_unused_fonts`] for the project that is the open one: what went is no longer
    /// registered for drawing either.
    pub(super) fn collect_unused_fonts_now(&self) {
        match self.collect_unused_fonts() {
            Ok(0) => {}
            Ok(gone) => {
                tracing::info!(gone, "dropped the embedded fonts no title names");
                self.activate_fonts();
            }
            Err(e) => tracing::warn!(error = %e, "could not tell which embedded fonts are unused"),
        }
    }

    /// The fonts of `incoming` the project would add, refused if they would take it past
    /// [`MAX_PROJECT_FONT_BYTES`].
    pub(super) fn check_font_room(&self, incoming: &[EmbeddedFont]) -> Result<()> {
        let have: HashSet<String> = if self.has_fonts_table() {
            let mut stmt = self.conn.prepare("SELECT sha256 FROM fonts")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        } else {
            HashSet::new()
        };
        let mut seen: HashSet<&str> = HashSet::new();
        let added: u64 = incoming
            .iter()
            .filter(|f| !have.contains(&f.sha256) && seen.insert(f.sha256.as_str()))
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
    /// agent's proposal while it has one): the artwork, its name and how long it runs. The id may name a
    /// floating title or a title clip (whose length is its document's, `source_out`). Cheap, for under
    /// the lock; `Library::save` does the rest without it.
    pub fn template_source(&self, overlay_id: Uuid) -> Result<TemplateSource> {
        let timeline = self.working_timeline()?;
        if let Some((clip, title)) = timeline.title_clip(overlay_id) {
            return Ok(TemplateSource {
                svg: title.svg.clone(),
                name: title.display_name(),
                duration: clip.source_out,
            });
        }
        let o = timeline
            .svg_overlay(overlay_id)
            .ok_or(Error::SvgOverlayNotFound(overlay_id))?;
        Ok(TemplateSource {
            svg: o.svg.clone(),
            name: o.display_name(),
            duration: o.doc_duration(),
        })
    }

    /// What changing the values of the template title `overlay_id` (floating or a title clip) needs: the
    /// artwork as it is now and its controls (checked: a project file can hold anything). `InvalidArgument`
    /// for a title that did not come from a template.
    pub fn template_edit_inputs(&self, overlay_id: Uuid) -> Result<TemplateEditInputs> {
        let timeline = self.working_timeline()?;
        let (name, svg, instance) = match timeline.title_clip(overlay_id) {
            Some((_, title)) => (title.display_name(), &title.svg, &title.template),
            None => {
                let o = timeline
                    .svg_overlay(overlay_id)
                    .ok_or(Error::SvgOverlayNotFound(overlay_id))?;
                (o.display_name(), &o.svg, &o.template)
            }
        };
        let instance = instance.clone().ok_or_else(|| not_a_template_named(&name))?;
        // Stored with the title, not written by this build: checked before they are trusted.
        crate::template::validate_controls_shape(&instance.controls)?;
        Ok(TemplateEditInputs::new(overlay_id, svg.clone(), instance))
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
        self.hold_fonts(&carried);
        Ok(result)
    }

    /// Keep the fonts a template carried, once its edit is stored: in what the project holds and in the
    /// set titles are drawn with (the pictures drawn from here on are keyed by the new set).
    pub(super) fn hold_fonts(&self, carried: &[EmbeddedFont]) {
        {
            let mut held = self.embedded.borrow_mut();
            for font in carried {
                if !held.iter().any(|f| f.sha256 == font.sha256) {
                    held.push(font.clone());
                }
            }
        }
        if fonts::register_embedded(carried) {
            crate::plan_epoch::bump_plan_epoch();
        }
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
    not_a_template_named(&o.display_name())
}

fn not_a_template_named(name: &str) -> Error {
    Error::InvalidArgument(format!("“{name}” is not a template title, so it has no values to set"))
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

    /// The same template with another font file, named by its title.
    fn with_font_two() -> Template {
        let mut t = with_font();
        t.svg = t.svg.replace("Zeist", "Zeyst");
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
    fn the_controls_a_title_stored_are_checked_before_its_values_are_set() {
        let project = Project::open_in_memory().unwrap();
        let added = project
            .apply_template(&template(), &values(&[]), 0.0, None, SvgLayout::default())
            .unwrap()
            .overlay;
        // A file from elsewhere (or a bug) left a control that would write an event handler.
        project
            .edit_timeline("corrupt", |t| {
                let instance = t.svg_overlays[0].template.as_mut().unwrap();
                instance.controls[1].kind = TemplateKind::Number;
                instance.controls[1].targets[0].attribute = Some("onclick".into());
                Ok(())
            })
            .unwrap();
        let head = project.history().unwrap().len();
        let err = project.template_edit_inputs(added.id).unwrap_err();
        assert!(
            matches!(err, Error::InvalidArgument(_)) && err.to_string().contains("onclick"),
            "{err}"
        );
        let err = project
            .set_template_values(added.id, &values(&[("tagline", "x")]))
            .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "{err}");
        assert_eq!(project.history().unwrap().len(), head, "no revision for a refusal");
    }

    #[test]
    fn setting_no_values_records_no_revision() {
        let project = Project::open_in_memory().unwrap();
        let added = project
            .apply_template(&template(), &values(&[]), 0.0, None, SvgLayout::default())
            .unwrap()
            .overlay;
        let head = project.history().unwrap().len();
        let err = project.set_template_values(added.id, &values(&[])).unwrap_err();
        assert!(
            matches!(err, Error::InvalidArgument(_)) && err.to_string().contains("no values to set"),
            "{err}"
        );
        assert_eq!(project.history().unwrap().len(), head);
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
    fn opening_a_project_registers_nothing_and_becoming_the_open_one_replaces_the_last_ones_fonts() {
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
        let epoch = crate::plan_epoch::plan_epoch();
        let first = Project::open(&path).unwrap();
        assert!(
            !fonts::has_family("Zeist") && fonts::embedded_digest().is_none(),
            "an opened project is not the open one: nothing it embeds is drawn with yet"
        );
        assert_eq!(first.embedded.borrow().len(), 1, "but it has read them");
        first.activate_fonts();
        assert!(fonts::has_family("Zeist"), "activating registers the project's fonts");
        assert!(crate::plan_epoch::plan_epoch() > epoch);
        let digest = fonts::embedded_digest();
        assert!(digest.is_some());
        // Another project without fonts, swapped in: the first one's do not leak into it.
        let epoch = crate::plan_epoch::plan_epoch();
        let mut shared = Project::open_in_memory().unwrap();
        let old = shared.replace_with(first);
        assert!(fonts::has_family("Zeist"));
        let old_first = shared.replace_with(old);
        assert!(!fonts::has_family("Zeist") && fonts::embedded_digest().is_none());
        assert!(crate::plan_epoch::plan_epoch() > epoch);
        drop((shared, old_first));
        // Reopening and swapping in gives the same set, so the same keys.
        let mut again = Project::open_in_memory().unwrap();
        again.replace_with(Project::open(&path).unwrap()).close();
        assert_eq!(fonts::embedded_digest(), digest);
        // Save As carries the fonts.
        let copy = dir.join("b.kerf");
        again.save_as(&copy).unwrap();
        fonts::replace_embedded(Vec::new());
        let mut copy_open = Project::open_in_memory().unwrap();
        copy_open.replace_with(Project::open(&copy).unwrap()).close();
        assert!(fonts::has_family("Zeist"));
        fonts::replace_embedded(Vec::new());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_open_that_is_refused_or_cancelled_leaves_the_open_projects_fonts_alone() {
        let dir = std::env::temp_dir().join(format!("kerf-tpl-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("other.kerf");
        {
            let other = Project::create(&path).unwrap();
            other
                .apply_template(&with_font_two(), &values(&[]), 0.0, None, SvgLayout::default())
                .unwrap();
        }
        fonts::replace_embedded(Vec::new());
        let open = Project::open_in_memory().unwrap();
        open.apply_template(&with_font(), &values(&[]), 0.0, None, SvgLayout::default())
            .unwrap();
        let digest = fonts::embedded_digest();
        assert!(fonts::has_family("Zeist") && !fonts::has_family("Zeyst"));
        // The file is opened (the MCP open_project does this before it asks whether the open project may
        // be let go; Save As does it for the copy) and then nothing is swapped in.
        let opened = Project::open(&path).unwrap();
        assert!(
            fonts::has_family("Zeist") && !fonts::has_family("Zeyst"),
            "still the open project's fonts"
        );
        assert_eq!(fonts::embedded_digest(), digest);
        assert_eq!(opened.embedded_fonts().unwrap().len(), 1);
        drop(opened);
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
        fonts::replace_embedded(Vec::new());
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
        fonts::replace_embedded(Vec::new());
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

    /// A title (custom art) that names the family `family`.
    fn naming(family: &str) -> crate::SvgSource {
        crate::SvgSource::Custom(format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"><text font-family=\"{family}\">a</text></svg>"
        ))
    }

    fn stored_families(p: &Project) -> Vec<String> {
        p.embedded_fonts().unwrap().into_iter().map(|f| f.family).collect()
    }

    #[test]
    fn the_fonts_of_a_discarded_proposal_are_dropped_and_no_longer_drawn_with() {
        let mut project = Project::open_in_memory().unwrap();
        project.set_actor(EditSource::Agent);
        project.begin_staging(None, None, None).unwrap();
        project
            .apply_template(&with_font(), &values(&[]), 0.0, None, SvgLayout::default())
            .unwrap();
        assert_eq!(stored_families(&project), ["Zeist"], "stored with the proposal");
        assert!(fonts::has_family("Zeist"));
        // Collecting while the proposal is open keeps what it names.
        assert_eq!(project.collect_unused_fonts().unwrap(), 0);
        project.set_actor(EditSource::User);
        project.discard_staged().unwrap();
        assert!(project.embedded_fonts().unwrap().is_empty(), "nothing names it any more");
        assert_eq!(project.embedded_font_bytes().unwrap(), 0, "the room it took is back");
        assert!(!fonts::has_family("Zeist") && fonts::embedded_digest().is_none());
        assert!(project.embedded.borrow().is_empty());
    }

    #[test]
    fn a_discard_keeps_the_fonts_the_live_cut_names() {
        let mut project = Project::open_in_memory().unwrap();
        project
            .apply_template(&with_font(), &values(&[]), 0.0, None, SvgLayout::default())
            .unwrap();
        project.set_actor(EditSource::Agent);
        project.begin_staging(None, None, None).unwrap();
        project
            .apply_template(&with_font_two(), &values(&[]), 5.0, None, SvgLayout::default())
            .unwrap();
        let mut both = stored_families(&project);
        both.sort();
        assert_eq!(both, ["Zeist", "Zeyst"]);
        project.set_actor(EditSource::User);
        project.discard_staged().unwrap();
        assert_eq!(
            stored_families(&project),
            ["Zeist"],
            "the live title's stays, the proposal's goes"
        );
        assert!(fonts::has_family("Zeist") && !fonts::has_family("Zeyst"));
        fonts::replace_embedded(Vec::new());
    }

    #[test]
    fn a_proposal_whose_template_was_undone_leaves_no_font_when_it_closes() {
        let mut project = Project::open_in_memory().unwrap();
        project.set_actor(EditSource::Agent);
        project.begin_staging(None, None, None).unwrap();
        project
            .apply_template(&with_font(), &values(&[]), 0.0, None, SvgLayout::default())
            .unwrap();
        project.undo_staged().unwrap();
        assert_eq!(stored_families(&project), ["Zeist"], "kept until the proposal ends");
        project.set_actor(EditSource::User);
        project.apply_staged(false).unwrap();
        assert!(project.embedded_fonts().unwrap().is_empty());
        assert!(!fonts::has_family("Zeist"));
    }

    #[test]
    fn accepting_a_proposal_keeps_its_fonts() {
        let mut project = Project::open_in_memory().unwrap();
        project.set_actor(EditSource::Agent);
        project.begin_staging(None, None, None).unwrap();
        project
            .apply_template(&with_font(), &values(&[]), 0.0, None, SvgLayout::default())
            .unwrap();
        project.set_actor(EditSource::User);
        project.apply_staged(false).unwrap();
        assert_eq!(stored_families(&project), ["Zeist"]);
        assert_eq!(project.collect_unused_fonts().unwrap(), 0, "the live cut names it now");
        fonts::replace_embedded(Vec::new());
    }

    #[test]
    fn saving_as_and_opening_drop_the_fonts_nothing_names() {
        let dir = std::env::temp_dir().join(format!("kerf-tpl-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.kerf");
        let used = EmbeddedFont::new("Zeist", fonts::shipped_font_renamed(true, "Zeist"));
        let unused = EmbeddedFont::new("Zeyst", fonts::shipped_font_renamed(false, "Zeyst"));
        {
            let project = Project::create(&path).unwrap();
            project
                .add_svg_overlay(naming("Zeist"), 0.0, Some(2.0), SvgLayout::default())
                .unwrap();
            project.store_fonts(&[used, unused.clone()]).unwrap();
            assert_eq!(project.embedded_fonts().unwrap().len(), 2);
        }
        // Opening drops the one nothing names, in the file and in what the project holds.
        let project = Project::open(&path).unwrap();
        assert_eq!(stored_families(&project), ["Zeist"]);
        assert_eq!(project.embedded.borrow().len(), 1);
        assert!(!fonts::has_family("Zeyst"), "and opening registered nothing");
        // Save As is the same for the project it saves, and for its copy.
        project.store_fonts(std::slice::from_ref(&unused)).unwrap();
        project.load_fonts().unwrap();
        assert_eq!(project.embedded.borrow().len(), 2);
        let copy = dir.join("b.kerf");
        project.save_as(&copy).unwrap();
        assert_eq!(stored_families(&project), ["Zeist"], "the project that was saved");
        assert_eq!(project.embedded.borrow().len(), 1);
        assert_eq!(stored_families(&Project::open(&copy).unwrap()), ["Zeist"], "the copy");
        drop(project);
        fonts::replace_embedded(Vec::new());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_font_already_stored_is_not_written_again() {
        let project = Project::open_in_memory().unwrap();
        let font = EmbeddedFont::new("Zeist", fonts::shipped_font_renamed(true, "Zeist"));
        project.store_fonts(std::slice::from_ref(&font)).unwrap();
        let changes = project.conn.total_changes();
        project.store_fonts(&[font.clone(), font]).unwrap();
        assert_eq!(project.conn.total_changes(), changes, "no row was written for a stored font");
        assert_eq!(project.embedded_fonts().unwrap().len(), 1);
        // And one font offered twice is stored, and counted for the limit, once.
        let other = EmbeddedFont::new("Zeyst", fonts::shipped_font_renamed(false, "Zeyst"));
        project.store_fonts(&[other.clone(), other]).unwrap();
        assert_eq!(project.embedded_fonts().unwrap().len(), 2);
        let twice = EmbeddedFont::new("Big", vec![7u8; (MAX_PROJECT_FONT_BYTES / 2 + 1) as usize]);
        assert!(project.check_font_room(&[twice.clone(), twice]).is_ok());
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
        project.load_fonts().unwrap();
        project.activate_fonts();
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
