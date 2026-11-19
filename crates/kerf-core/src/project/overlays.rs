//! Markers, text overlays and animated SVG titles.

use super::*;

/// Where an animated title's artwork comes from.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SvgSource {
    /// SVG source; it is sanitised before it is stored.
    Custom(String),
    /// One of the generated presets.
    Preset(SvgPreset),
}

impl SvgSource {
    /// Read the artwork: sanitise and parse custom SVG (up to a second and a half for a heavy
    /// document) or check a preset's parameters. Pure, so a surface does it **before it takes the
    /// project lock** and hands the result to [`Project::add_svg_overlay_parsed`].
    pub fn check(self) -> Result<SvgChecked> {
        match self {
            SvgSource::Custom(src) => SvgDoc::parse(&src).map(SvgChecked::Custom),
            SvgSource::Preset(p) => {
                check_params(&p.params)?;
                Ok(SvgChecked::Preset(p))
            }
        }
    }
}

/// An [`SvgSource`] that has been read: custom SVG already parsed, preset parameters already checked.
#[derive(Debug, Clone)]
pub enum SvgChecked {
    Custom(SvgDoc),
    Preset(SvgPreset),
}

/// Name and placement of a new animated title; each `None` takes the default (full frame, opaque).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SvgLayout {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
}

/// An animated title as written, with what the sanitiser or the SMIL reader could not honour.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SvgOverlayResult {
    pub overlay: SvgOverlay,
    pub warnings: Vec<String>,
}

/// When an animated title may be on screen: it starts before it ends, and neither is further out
/// than [`MAX_SVG_SECONDS`].
pub(super) fn check_svg_span(start: f64, end: f64) -> Result<()> {
    if !(start.is_finite() && end.is_finite()) {
        return Err(Error::InvalidArgument("start and end must be numbers of seconds".to_string()));
    }
    if end <= start {
        return Err(Error::InvalidArgument("overlay end must be after start".to_string()));
    }
    if end > MAX_SVG_SECONDS {
        return Err(Error::InvalidArgument(format!(
            "an animated title ends within the first {} hours",
            MAX_SVG_SECONDS / 3600.0
        )));
    }
    Ok(())
}

/// A placement the compositor can draw: finite numbers, a real size, an opacity.
pub(super) fn check_svg_rect(x: f64, y: f64, width: f64, height: f64, opacity: f64) -> Result<()> {
    if ![x, y, width, height, opacity].iter().all(|v| v.is_finite()) {
        return Err(Error::InvalidArgument(
            "x, y, width, height and opacity must be numbers".to_string(),
        ));
    }
    if !(-5.0..=5.0).contains(&x) || !(-5.0..=5.0).contains(&y) {
        return Err(Error::InvalidArgument(
            "x and y are fractions of the frame, within -5..=5".to_string(),
        ));
    }
    if width <= 0.0 || height <= 0.0 || width > 10.0 || height > 10.0 {
        return Err(Error::InvalidArgument(
            "width and height are fractions of the frame, > 0 and <= 10".to_string(),
        ));
    }
    if !(0.0..=1.0).contains(&opacity) {
        return Err(Error::InvalidArgument("opacity must be within 0.0..=1.0".to_string()));
    }
    Ok(())
}

impl Project {
    // ---- text overlays (titles / lower-thirds / captions) -----------------

    /// Add a text overlay drawn over the composited picture, returning it.
    /// Drop a named marker at `time`. Markers are kept sorted by time, so the
    /// UI and `next`/`previous` navigation never have to re-sort.
    pub fn add_marker(&self, time: f64, name: String, color: Option<String>) -> Result<Marker> {
        if !time.is_finite() || time < 0.0 {
            return Err(Error::InvalidArgument("marker time must be >= 0".to_string()));
        }
        // The colour reaches an inline style in the ruler: a colour spec, nothing else.
        let color = color.filter(|c| !c.is_empty());
        if let Some(c) = &color {
            check_color("marker color", c)?;
        }
        let marker = Marker {
            id: Uuid::new_v4(),
            time,
            name,
            color,
            ..Default::default()
        };
        self.edit_timeline("Add marker", move |timeline| {
            timeline.markers.push(marker.clone());
            timeline.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
            Ok(marker)
        })
    }

    /// Rename, recolor or move a marker; each `None` leaves that field alone.
    /// Pass an empty `color` to clear it back to the UI default.
    pub fn update_marker(
        &self,
        marker_id: Uuid,
        time: Option<f64>,
        name: Option<String>,
        color: Option<String>,
    ) -> Result<Marker> {
        self.edit_marker(
            marker_id,
            MarkerPatch {
                time,
                name,
                color,
                ..MarkerPatch::default()
            },
        )
    }

    /// Remove a marker.
    pub fn remove_marker(&self, marker_id: Uuid) -> Result<()> {
        self.edit_timeline("Remove marker", |timeline| {
            let before = timeline.markers.len();
            timeline.markers.retain(|m| m.id != marker_id);
            if timeline.markers.len() == before {
                return Err(Error::InvalidArgument(format!("no marker {marker_id}")));
            }
            Ok(())
        })
    }

    pub fn add_overlay(&self, text: String, start: f64, end: f64) -> Result<TextOverlay> {
        // Finite first (`NaN.max(0.0)` is 0), then judged after the clamp: `(-2, 0)` would
        // otherwise become an overlay of no length.
        if !start.is_finite() || !end.is_finite() {
            return Err(Error::InvalidArgument("overlay end must be after start".to_string()));
        }
        let start = start.max(0.0);
        if end <= start {
            return Err(Error::InvalidArgument("overlay end must be after start".to_string()));
        }
        let overlay = TextOverlay::new(text, start, end);
        self.edit_timeline("Add text overlay", move |timeline| {
            timeline.overlays.push(overlay.clone());
            Ok(overlay)
        })
    }

    /// Update mutable fields of a text overlay; each `None` leaves a field
    /// unchanged. Pass an empty `bg` to clear the box background, or an empty
    /// `font` to revert to the default font.
    #[allow(clippy::too_many_arguments)]
    pub fn update_overlay(
        &self,
        overlay_id: Uuid,
        text: Option<String>,
        start: Option<f64>,
        end: Option<f64>,
        pos_x: Option<f64>,
        pos_y: Option<f64>,
        size: Option<f64>,
        color: Option<String>,
        bg: Option<String>,
        font: Option<String>,
        bold: Option<bool>,
    ) -> Result<TextOverlay> {
        TextOverlay::check_values(start, end, pos_x, pos_y, size)?;
        // Both colours are spliced into a filter graph and an inline style: a colour spec only.
        if let Some(c) = &color {
            check_color("text color", c)?;
        }
        if let Some(c) = bg.as_deref().filter(|c| !c.is_empty()) {
            check_color("box color", c)?;
        }
        self.edit_timeline("Update text overlay", move |timeline| {
            let o = timeline
                .overlays
                .iter_mut()
                .find(|o| o.id == overlay_id)
                .ok_or(Error::OverlayNotFound(overlay_id))?;
            if let Some(v) = text {
                o.text = v;
            }
            if let Some(v) = start {
                o.start = v.max(0.0);
            }
            if let Some(v) = end {
                o.end = v;
            }
            if let Some(v) = pos_x {
                o.pos_x = v;
            }
            if let Some(v) = pos_y {
                o.pos_y = v;
            }
            if let Some(v) = size {
                o.size = v;
            }
            if let Some(v) = color {
                o.color = v;
            }
            if let Some(v) = bg {
                o.bg = if v.is_empty() { None } else { Some(v) };
            }
            if let Some(v) = font {
                o.font = if v.is_empty() { None } else { Some(v) };
            }
            if let Some(v) = bold {
                o.bold = v;
            }
            o.check_span()?;
            Ok(o.clone())
        })
    }

    /// Remove a text overlay.
    pub fn remove_overlay(&self, overlay_id: Uuid) -> Result<()> {
        self.edit_timeline("Remove text overlay", move |timeline| {
            let before = timeline.overlays.len();
            timeline.overlays.retain(|o| o.id != overlay_id);
            if timeline.overlays.len() == before {
                return Err(Error::OverlayNotFound(overlay_id));
            }
            Ok(())
        })
    }

    // ---- animated SVG titles ----------------------------------------------

    /// Add an animated SVG title, returning it with what could not be honoured.
    ///
    /// `source` is custom SVG (sanitised and read first: bad XML, a root that is not `<svg>`, no size
    /// or a cap exceeded is an `InvalidArgument`) or a preset (its parameters checked, its artwork
    /// generated for the project's delivery frame and the title's length). `end` defaults to when the
    /// animation has finished — [`SvgDoc::natural_duration`] held to 2–30 s — and to four seconds for
    /// a still, a loop and a preset. One `Add animated title` edit, staged for an agent.
    ///
    /// **Tests only.** This reads the artwork (up to a second and a half) wherever the caller is, which
    /// for a surface is under the project lock; surfaces read first ([`SvgSource::check`]) and take
    /// the lock for [`Project::add_svg_overlay_parsed`].
    #[cfg(test)]
    pub fn add_svg_overlay(
        &self,
        source: SvgSource,
        start: f64,
        end: Option<f64>,
        layout: SvgLayout,
    ) -> Result<SvgOverlayResult> {
        self.add_svg_overlay_parsed(source.check()?, start, end, layout)
    }

    /// `add_svg_overlay` for artwork that has been read already ([`SvgSource::check`]):
    /// nothing in it parses, so it is cheap to run under the project lock. A surface checks the
    /// source first and takes the lock after.
    pub fn add_svg_overlay_parsed(
        &self,
        source: SvgChecked,
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
        let (svg, preset, warnings, span) = match source {
            SvgChecked::Custom(doc) => (doc.sanitized().to_string(), None, doc.warnings().to_vec(), doc.default_span()),
            SvgChecked::Preset(p) => (String::new(), Some(p), Vec::new(), 4.0),
        };
        let end = end.unwrap_or(start + span);
        check_svg_span(start, end)?;
        let (x, y, width, height, opacity) = (
            layout.x.unwrap_or(0.0),
            layout.y.unwrap_or(0.0),
            layout.width.unwrap_or(1.0),
            layout.height.unwrap_or(1.0),
            layout.opacity.unwrap_or(1.0),
        );
        check_svg_rect(x, y, width, height, opacity)?;
        let mut overlay = SvgOverlay {
            id: Uuid::new_v4(),
            name: layout.name.map(|n| n.trim().to_string()).unwrap_or_default(),
            svg,
            start,
            end,
            x,
            y,
            width,
            height,
            opacity,
            offset: 0.0,
            preset,
            template: None,
        };
        self.edit_timeline("Add animated title", move |timeline| {
            if timeline.svg_overlays.len() >= MAX_SVG_OVERLAYS {
                return Err(Error::InvalidArgument(format!(
                    "a timeline holds at most {MAX_SVG_OVERLAYS} animated titles"
                )));
            }
            overlay.regenerate(timeline.preset_frame());
            timeline.svg_overlays.push(overlay.clone());
            Ok(SvgOverlayResult { overlay, warnings })
        })
    }

    /// Change an animated title; each `None` in `patch` leaves a field alone.
    ///
    /// `svg` replaces the artwork with custom source (sanitised as on add; the title stops being a
    /// preset); `preset_params` re-fills a preset title and `preset_kind` switches it to another kind,
    /// keeping its id, name, timing and place in the draw order (both refused on a custom one), and
    /// `svg` is exclusive with either. **A preset's artwork is re-made whenever its length changes**
    /// (its animations are scheduled against the span), so trimming or stretching one in the timeline
    /// retimes the effect instead of cutting it off.
    ///
    /// **Tests only**, like `add_svg_overlay`: a surface reads first
    /// ([`SvgOverlayPatch::check`]) and takes the lock for [`Project::update_svg_overlay_parsed`].
    #[cfg(test)]
    pub fn update_svg_overlay(&self, overlay_id: Uuid, patch: SvgOverlayPatch) -> Result<SvgOverlayResult> {
        self.update_svg_overlay_parsed(overlay_id, patch, None)
    }

    /// `update_svg_overlay` with the artwork read already: `doc` is [`SvgOverlayPatch::check`]'s
    /// answer for `patch.svg` and replaces the artwork (`patch.svg` itself is not looked at again), so
    /// nothing here parses and it is cheap to run under the project lock. With `doc` `None` a
    /// `patch.svg` is still parsed here, as `update_svg_overlay` always has.
    pub fn update_svg_overlay_parsed(
        &self,
        overlay_id: Uuid,
        patch: SvgOverlayPatch,
        doc: Option<SvgDoc>,
    ) -> Result<SvgOverlayResult> {
        let doc = match doc {
            Some(doc) => {
                patch.check_cheap()?;
                Some(doc)
            }
            None => patch.check()?,
        };
        self.edit_timeline("Update animated title", move |timeline| {
            let frame = timeline.preset_frame();
            let o = timeline
                .svg_overlays
                .iter_mut()
                .find(|o| o.id == overlay_id)
                .ok_or(Error::SvgOverlayNotFound(overlay_id))?;
            let length = o.doc_duration();
            if let Some(v) = patch.name {
                o.name = v.trim().to_string();
            }
            if let Some(v) = patch.start {
                o.start = v.max(0.0);
            }
            if let Some(v) = patch.end {
                o.end = v;
            }
            o.x = patch.x.unwrap_or(o.x);
            o.y = patch.y.unwrap_or(o.y);
            o.width = patch.width.unwrap_or(o.width);
            o.height = patch.height.unwrap_or(o.height);
            o.opacity = patch.opacity.unwrap_or(o.opacity);
            check_svg_span(o.start, o.end)?;
            check_svg_rect(o.x, o.y, o.width, o.height, o.opacity)?;
            let mut warnings = Vec::new();
            let mut regenerate = o.preset.is_some() && (o.doc_duration() - length).abs() > 1e-6;
            if let Some(doc) = &doc {
                o.svg = doc.sanitized().to_string();
                o.preset = None;
                // New artwork: the controls point into the old one.
                o.template = None;
                warnings = doc.warnings().to_vec();
                regenerate = false;
            } else if patch.preset_params.is_some() || patch.preset_kind.is_some() {
                let preset = o.preset.as_mut().ok_or_else(|| {
                    Error::InvalidArgument("this title is custom SVG, not a preset; pass svg to change it".to_string())
                })?;
                if let Some(kind) = patch.preset_kind {
                    preset.kind = kind;
                }
                if let Some(params) = patch.preset_params {
                    preset.params = params;
                }
                regenerate = true;
            }
            if regenerate {
                o.regenerate(frame);
            }
            Ok(SvgOverlayResult {
                overlay: o.clone(),
                warnings,
            })
        })
    }

    /// Remove an animated title.
    pub fn remove_svg_overlay(&self, overlay_id: Uuid) -> Result<()> {
        self.edit_timeline("Remove animated title", move |timeline| {
            let before = timeline.svg_overlays.len();
            timeline.svg_overlays.retain(|o| o.id != overlay_id);
            if timeline.svg_overlays.len() == before {
                return Err(Error::SvgOverlayNotFound(overlay_id));
            }
            Ok(())
        })
    }

    /// Set (or clear, with an empty list) an overlay's position/opacity keyframes.
    pub fn set_overlay_keyframes(&self, overlay_id: Uuid, mut keyframes: Vec<TextKeyframe>) -> Result<TextOverlay> {
        for k in &keyframes {
            k.check()?;
        }
        keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
        self.edit_timeline("Set overlay keyframes", move |timeline| {
            let o = timeline
                .overlays
                .iter_mut()
                .find(|o| o.id == overlay_id)
                .ok_or(Error::OverlayNotFound(overlay_id))?;
            o.keyframes = keyframes;
            Ok(o.clone())
        })
    }
}
