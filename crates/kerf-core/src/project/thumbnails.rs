//! Thumbnails in a project (the model is `model/thumbnail.rs`, the drawing `crate::thumbnail`).
//!
//! An edit is the lock-free shape the templates use: [`Project::thumbnail_base`] reads the thumbnail
//! (or a new one's defaults) under the lock, [`prepare_thumbnail`] builds the whole new value with it
//! released (the sanitiser, a template's zip and fonts, the headline's artwork), and
//! [`Project::apply_thumbnail_parsed`] only stores it — through `edit_timeline`, so it stages for an
//! agent and undoes — refusing when the thumbnail changed in between.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Project;
use crate::error::{Error, Result};
use crate::fonts::EmbeddedFont;
use crate::model::thumbnail::{
    check_layer_rect, check_moment, default_placement, parse_thumbnail_size, MAX_THUMBNAILS, MAX_THUMBNAIL_LAYERS,
};
use crate::model::{Asset, SvgPresetParams, Thumbnail, ThumbnailLayer, Timeline, VideoGraphics};
use crate::svg::SvgDoc;
use crate::template::{Library, TemplateEditInputs};

/// What a thumbnail should become. Each `None` keeps what it has (a new one: its default).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ThumbnailSpec {
    /// A label for it.
    #[serde(default)]
    pub name: Option<String>,
    /// The background frame, in timeline seconds.
    #[serde(default)]
    pub time: Option<f64>,
    /// `youtube` (1280x720), `vertical` (1080x1920), `square` (1080x1080), `4:5` (1080x1350), `16:9`
    /// (1920x1080) or `WxH` (16 to 4096 a side). A new one defaults to the delivery frame's shape
    /// (youtube for a landscape or unset one).
    #[serde(default)]
    pub size: Option<String>,
    /// Where the crop is centred when the frame has another shape: fractions of the frame, 0..1 (0.5
    /// is the middle).
    #[serde(default)]
    pub focus_x: Option<f64>,
    #[serde(default)]
    pub focus_y: Option<f64>,
    /// Which of the video's own graphics the background keeps: `none` (the footage alone), `titles`
    /// (titles and text, no captions; the default) or `all`.
    #[serde(default)]
    pub graphics: Option<VideoGraphics>,
    /// The whole stack of layers, bottom first, replacing the one it has.
    #[serde(default)]
    pub layers: Option<Vec<ThumbnailLayerSpec>>,
}

/// One layer of a thumbnail's stack: an existing one (`layer_id`), changed or not, or a new one made
/// from exactly one of `text`, `template_id`, `svg` and `image`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ThumbnailLayerSpec {
    /// Keep this layer (its id from the thumbnail's `layers`): its artwork and placement stay unless
    /// given here.
    #[serde(default)]
    pub layer_id: Option<String>,
    /// A headline: `text` large and bold with a dark outline (a line per `\n`, at most three), an
    /// optional `subtitle` under it in `accent`, `color` and `font`.
    #[serde(default)]
    pub text: Option<SvgPresetParams>,
    /// A motion template from the library, drawn still.
    #[serde(default)]
    pub template_id: Option<String>,
    /// Control values for the template (new, or the layer's own when it came from one), by control id;
    /// an image control takes a `data:` URL.
    #[serde(default)]
    pub values: Option<BTreeMap<String, String>>,
    /// Artwork of one's own: an SVG document with a `viewBox`.
    #[serde(default)]
    pub svg: Option<String>,
    /// A picture: a `data:` URL of a PNG, JPEG, GIF or WebP (a little under 96 KB).
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// The moment of an animated artwork that is drawn, in seconds; a negative one goes back to the
    /// default, the moment it shows most (its settled state).
    #[serde(default)]
    pub at: Option<f64>,
    /// The rectangle the artwork is fitted into ("contain", centred), as fractions of the thumbnail. A
    /// new headline defaults to a band across the lower part, anything else to the whole picture.
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub height: Option<f64>,
    /// 0..1, default 1.
    #[serde(default)]
    pub opacity: Option<f64>,
}

/// What a thumbnail edit read before the lock was released: the thumbnail as it was (`None` for a new
/// one) and the value to start from.
#[derive(Debug, Clone)]
pub struct ThumbnailBase {
    pub(crate) before: Option<Thumbnail>,
    pub(crate) start: Thumbnail,
}

/// A thumbnail built and checked by [`prepare_thumbnail`], to be stored.
#[derive(Debug, Clone)]
pub struct PreparedThumbnail {
    pub(crate) before: Option<Thumbnail>,
    pub(crate) thumbnail: Thumbnail,
    pub(crate) fonts: Vec<EmbeddedFont>,
    pub(crate) warnings: Vec<String>,
}

/// A thumbnail as stored, with what the sanitiser or a template could not honour.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThumbnailResult {
    pub thumbnail: Thumbnail,
    pub warnings: Vec<String>,
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::InvalidArgument(msg.into())
}

fn not_found(id: Uuid) -> Error {
    invalid(format!("no thumbnail {id}"))
}

/// The size a new thumbnail takes from the delivery frame: its shape when it is vertical, square or
/// 4:5, else `youtube`.
fn default_size(timeline: &Timeline) -> (u32, u32) {
    let name = match timeline.format.map(|f| f.ratio()) {
        Some((w, h)) if w < h && (w, h) == (4, 5) => "4:5",
        Some((w, h)) if w < h => "vertical",
        Some((1, 1)) => "square",
        _ => "youtube",
    };
    parse_thumbnail_size(name).unwrap_or((1280, 720))
}

/// The artwork of a picture: the image fitted to its own size.
fn image_svg(data_url: &str) -> Result<String> {
    let url = data_url::DataUrl::process(data_url).map_err(|_| invalid("`image` is not a data: URL"))?;
    let (bytes, _) = url.decode_to_vec().map_err(|_| invalid("`image` is not a data: URL"))?;
    let size = imagesize::blob_size(&bytes).map_err(|_| invalid("`image` is not a PNG, JPEG, GIF or WebP picture"))?;
    // Checked as a template's image is: the kind, the pixels and the size a title can hold.
    let url = crate::template::image_data_url(&bytes)?;
    let (w, h) = (size.width.max(1), size.height.max(1));
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\"><image href=\"{url}\" width=\"{w}\" height=\"{h}\"/></svg>"
    ))
}

/// The artwork a layer spec names, if it names one: `(svg doc, text, template, name, fonts, warnings)`.
type Artwork = (
    SvgDoc,
    Option<SvgPresetParams>,
    Option<crate::template::TemplateInstance>,
    String,
    Vec<EmbeddedFont>,
    Vec<String>,
);

fn artwork(spec: &ThumbnailLayerSpec, library: Option<&Library>) -> Result<Option<Artwork>> {
    let given = [
        spec.text.is_some(),
        spec.template_id.is_some(),
        spec.svg.is_some(),
        spec.image.is_some(),
    ];
    if given.iter().filter(|g| **g).count() > 1 {
        return Err(invalid(
            "a layer is one of `text`, `template_id`, `svg` or `image`, not several",
        ));
    }
    if let Some(text) = &spec.text {
        crate::svg::check_params(text)?;
        let doc = SvgDoc::parse(&crate::svg::render_headline(text))?;
        let name: String = text.text.lines().next().unwrap_or("").trim().chars().take(40).collect();
        let warnings = doc.warnings().to_vec();
        return Ok(Some((doc, Some(text.clone()), None, name, Vec::new(), warnings)));
    }
    if let Some(id) = &spec.template_id {
        let library = library.ok_or_else(|| invalid("the template library is not available"))?;
        let prepared = library.get(id)?.prepare(&spec.values.clone().unwrap_or_default())?;
        return Ok(Some((
            prepared.doc,
            None,
            Some(prepared.instance),
            prepared.name,
            prepared.fonts,
            prepared.warnings,
        )));
    }
    let (svg, name) = match (&spec.svg, &spec.image) {
        (Some(svg), _) => (svg.clone(), "Artwork"),
        (_, Some(image)) => (image_svg(image)?, "Picture"),
        _ => return Ok(None),
    };
    let doc = SvgDoc::parse(&svg)?;
    let warnings = doc.warnings().to_vec();
    Ok(Some((doc, None, None, name.to_string(), Vec::new(), warnings)))
}

fn layer(
    spec: &ThumbnailLayerSpec,
    existing: &[ThumbnailLayer],
    library: Option<&Library>,
    fonts: &mut Vec<EmbeddedFont>,
    warnings: &mut Vec<String>,
) -> Result<ThumbnailLayer> {
    let kept = match &spec.layer_id {
        Some(id) => {
            let id: Uuid = id.parse().map_err(|_| invalid(format!("layer_id {id} is not a UUID")))?;
            Some(
                existing
                    .iter()
                    .find(|l| l.id == id)
                    .cloned()
                    .ok_or_else(|| invalid(format!("the thumbnail has no layer {id}")))?,
            )
        }
        None => None,
    };
    let made = artwork(spec, library)?;
    let mut out = match (kept, made) {
        (kept, Some((doc, text, template, name, carried, said))) => {
            fonts.extend(carried);
            warnings.extend(said);
            let headline = text.is_some();
            let (x, y, width, height) = default_placement(headline);
            let base = kept.unwrap_or(ThumbnailLayer {
                id: Uuid::new_v4(),
                name,
                svg: String::new(),
                text: None,
                template: None,
                at: None,
                x,
                y,
                width,
                height,
                opacity: 1.0,
            });
            ThumbnailLayer {
                svg: doc.sanitized().to_string(),
                text,
                template,
                ..base
            }
        }
        (Some(mut kept), None) => {
            if let Some(values) = &spec.values {
                let instance = kept
                    .template
                    .clone()
                    .ok_or_else(|| invalid(format!("layer “{}” is not a template, so it has no values to set", kept.name)))?;
                crate::template::validate_controls_shape(&instance.controls)?;
                let prepared = TemplateEditInputs::new(kept.id, kept.svg.clone(), instance).prepare(values)?;
                kept.svg = prepared.doc.sanitized().to_string();
                if let Some(instance) = kept.template.as_mut() {
                    instance.values = prepared.values;
                }
                warnings.extend(prepared.warnings);
            }
            kept
        }
        (None, None) => return Err(invalid("a new layer needs `text`, `template_id`, `svg` or `image`")),
    };
    if let Some(name) = spec.name.as_ref().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        out.name = name.chars().take(80).collect();
    }
    if let Some(at) = spec.at {
        out.at = if at < 0.0 { None } else { Some(at) };
    }
    out.x = spec.x.unwrap_or(out.x);
    out.y = spec.y.unwrap_or(out.y);
    out.width = spec.width.unwrap_or(out.width);
    out.height = spec.height.unwrap_or(out.height);
    out.opacity = spec.opacity.unwrap_or(out.opacity);
    check_layer_rect(out.x, out.y, out.width, out.height, out.opacity)?;
    if let Some(at) = out.at {
        check_moment(at)?;
    }
    Ok(out)
}

/// The thumbnail `base` with `spec` laid over it, every layer made and checked. Runs the sanitiser,
/// reads templates from `library` and draws headlines: for **outside** the project lock.
pub fn prepare_thumbnail(base: ThumbnailBase, spec: &ThumbnailSpec, library: Option<&Library>) -> Result<PreparedThumbnail> {
    let ThumbnailBase { before, start } = base;
    let mut t = start;
    if let Some(name) = spec.name.as_ref().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        t.name = name.chars().take(80).collect();
    }
    t.time = spec.time.unwrap_or(t.time);
    if let Some(size) = &spec.size {
        (t.width, t.height) = parse_thumbnail_size(size)?;
    }
    t.focus_x = spec.focus_x.unwrap_or(t.focus_x);
    t.focus_y = spec.focus_y.unwrap_or(t.focus_y);
    t.graphics = spec.graphics.unwrap_or(t.graphics);
    let (mut fonts, mut warnings) = (Vec::new(), Vec::new());
    if let Some(specs) = &spec.layers {
        if specs.len() > MAX_THUMBNAIL_LAYERS {
            return Err(invalid(format!("a thumbnail stacks at most {MAX_THUMBNAIL_LAYERS} layers")));
        }
        let layers = specs
            .iter()
            .map(|s| layer(s, &t.layers, library, &mut fonts, &mut warnings))
            .collect::<Result<Vec<_>>>()?;
        let mut ids: Vec<Uuid> = layers.iter().map(|l| l.id).collect();
        ids.sort();
        ids.dedup();
        if ids.len() != layers.len() {
            return Err(invalid("a layer is in the stack once: a `layer_id` is named twice"));
        }
        t.layers = layers;
    }
    t.check()?;
    Ok(PreparedThumbnail {
        before,
        thumbnail: t,
        fonts,
        warnings,
    })
}

impl Project {
    /// What an edit of thumbnail `id` starts from (`None`: a new one, with its defaults: the delivery
    /// frame's shape and the frame at 0), read from the working timeline. Cheap, for under the lock.
    pub fn thumbnail_base(&self, id: Option<Uuid>) -> Result<ThumbnailBase> {
        let timeline = self.working_timeline()?;
        if let Some(id) = id {
            let t = timeline.thumbnail(id).cloned().ok_or_else(|| not_found(id))?;
            return Ok(ThumbnailBase {
                before: Some(t.clone()),
                start: t,
            });
        }
        let (width, height) = default_size(&timeline);
        Ok(ThumbnailBase {
            before: None,
            start: Thumbnail {
                id: Uuid::new_v4(),
                name: format!("Thumbnail {}", timeline.thumbnails.len() + 1),
                time: 0.0,
                width,
                height,
                focus_x: 0.5,
                focus_y: 0.5,
                graphics: VideoGraphics::default(),
                layers: Vec::new(),
            },
        })
    }

    /// Store a thumbnail [`prepare_thumbnail`] built: a new one is added (one `Add thumbnail` edit), an
    /// existing one replaced (`Change thumbnail`) only if it is still what the edit started from — one
    /// that changed meanwhile is an `InvalidArgument` and the caller prepares again. A template's fonts
    /// are stored and registered for drawing. Staged for an agent.
    pub fn apply_thumbnail_parsed(&self, prepared: PreparedThumbnail) -> Result<ThumbnailResult> {
        let PreparedThumbnail {
            before,
            thumbnail,
            fonts,
            warnings,
        } = prepared;
        self.check_font_room(&fonts)?;
        let label = match before {
            None => format!("Add thumbnail “{}”", thumbnail.name),
            Some(_) => format!("Change thumbnail “{}”", thumbnail.name),
        };
        let stored = thumbnail.clone();
        self.edit_timeline(&label, |timeline| {
            match &before {
                None => {
                    if timeline.thumbnails.len() >= MAX_THUMBNAILS {
                        return Err(invalid(format!("a project keeps at most {MAX_THUMBNAILS} thumbnails")));
                    }
                    timeline.thumbnails.push(stored);
                }
                Some(before) => {
                    let slot = timeline
                        .thumbnails
                        .iter_mut()
                        .find(|t| t.id == before.id)
                        .ok_or_else(|| not_found(before.id))?;
                    if slot != before {
                        return Err(invalid("the thumbnail changed while this edit was made; try again"));
                    }
                    *slot = stored;
                }
            }
            self.store_fonts(&fonts)
        })?;
        self.hold_fonts(&fonts);
        Ok(ThumbnailResult { thumbnail, warnings })
    }

    /// Remove a thumbnail (one `Remove thumbnail` edit).
    pub fn remove_thumbnail(&self, id: Uuid) -> Result<()> {
        self.edit_timeline("Remove thumbnail", |timeline| {
            let before = timeline.thumbnails.len();
            timeline.thumbnails.retain(|t| t.id != id);
            if timeline.thumbnails.len() == before {
                return Err(not_found(id));
            }
            Ok(())
        })
    }

    /// What drawing thumbnail `id` needs: the thumbnail, the working timeline and the **original**
    /// assets (a preview swaps in proxies with `Project::preview_assets_of`, outside the lock).
    pub fn thumbnail_render_inputs(&self, id: Uuid) -> Result<(Thumbnail, Timeline, Vec<Asset>)> {
        let timeline = self.working_timeline()?;
        let t = timeline.thumbnail(id).cloned().ok_or_else(|| not_found(id))?;
        Ok((t, timeline, self.list_assets()?))
    }

    /// What suggesting frames needs: the working timeline, the assets and the scene changes analysis
    /// found in each asset of the cut (source seconds; none for one not analyzed).
    pub fn frame_suggestion_inputs(&self) -> Result<(Timeline, Vec<Asset>, HashMap<Uuid, Vec<f64>>)> {
        let timeline = self.working_timeline()?;
        let mut scenes = HashMap::new();
        for clip in timeline.tracks.iter().flat_map(|t| t.clips.iter()) {
            if scenes.contains_key(&clip.asset_id) || clip.generator.is_some() {
                continue;
            }
            let found = self.get_analysis(clip.asset_id)?.map(|a| a.scene_changes).unwrap_or_default();
            scenes.insert(clip.asset_id, found);
        }
        Ok((timeline, self.list_assets()?, scenes))
    }

    /// [`prepare_thumbnail`] and [`Project::apply_thumbnail_parsed`] in one. **Tests only**: preparing
    /// runs the sanitiser, which a surface must not do under the project lock.
    #[cfg(test)]
    pub fn put_thumbnail(&self, id: Option<Uuid>, spec: &ThumbnailSpec, library: Option<&Library>) -> Result<ThumbnailResult> {
        self.apply_thumbnail_parsed(prepare_thumbnail(self.thumbnail_base(id)?, spec, library)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Delivery, DiffKind, EditSource, Fit};

    fn headline(text: &str) -> ThumbnailLayerSpec {
        ThumbnailLayerSpec {
            text: Some(SvgPresetParams::new(text)),
            ..ThumbnailLayerSpec::default()
        }
    }

    #[test]
    fn a_thumbnail_is_added_changed_undone_and_removed_as_edits_of_the_cut() {
        let p = Project::sample().unwrap();
        let spec = ThumbnailSpec {
            time: Some(2.0),
            layers: Some(vec![headline("Big news")]),
            ..ThumbnailSpec::default()
        };
        let made = p.put_thumbnail(None, &spec, None).unwrap().thumbnail;
        assert_eq!((made.width, made.height), (1280, 720));
        assert_eq!(made.layers[0].kind(), "text");
        assert_eq!(made.layers[0].name, "Big news");
        assert_eq!(
            (
                made.layers[0].x,
                made.layers[0].y,
                made.layers[0].width,
                made.layers[0].height
            ),
            default_placement(true)
        );
        assert_eq!(p.timeline().unwrap().thumbnails, vec![made.clone()]);

        let layer_id = made.layers[0].id.to_string();
        let changed = p
            .put_thumbnail(
                Some(made.id),
                &ThumbnailSpec {
                    size: Some("vertical".into()),
                    layers: Some(vec![
                        ThumbnailLayerSpec {
                            svg: Some("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"><rect width=\"10\" height=\"10\"/></svg>".into()),
                            ..ThumbnailLayerSpec::default()
                        },
                        ThumbnailLayerSpec {
                            layer_id: Some(layer_id),
                            y: Some(0.1),
                            ..ThumbnailLayerSpec::default()
                        },
                    ]),
                    ..ThumbnailSpec::default()
                },
                None,
            )
            .unwrap()
            .thumbnail;
        assert_eq!((changed.width, changed.height), (1080, 1920));
        assert_eq!(changed.layers.len(), 2);
        assert_eq!(changed.layers[1].id, made.layers[0].id, "the kept layer keeps its id");
        assert_eq!(changed.layers[1].svg, made.layers[0].svg, "and its artwork");
        assert_eq!(changed.layers[1].y, 0.1);
        assert_eq!(changed.layers[0].kind(), "svg");
        assert_eq!(changed.time, 2.0, "what the spec leaves out stays");

        p.undo().unwrap();
        assert_eq!(p.timeline().unwrap().thumbnails, vec![made.clone()]);
        p.remove_thumbnail(made.id).unwrap();
        assert!(p.timeline().unwrap().thumbnails.is_empty());
        assert!(p.remove_thumbnail(made.id).is_err());
    }

    #[test]
    fn a_new_thumbnail_takes_the_delivery_shape_and_bad_specs_are_refused() {
        let p = Project::sample().unwrap();
        p.set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover))).unwrap();
        let headed = ThumbnailSpec {
            layers: Some(vec![headline("x")]),
            ..ThumbnailSpec::default()
        };
        let t = p.put_thumbnail(None, &headed, None).unwrap().thumbnail;
        let t2 = t.clone();
        assert_eq!((t.width, t.height), (1080, 1920));
        let refused = |spec: ThumbnailSpec| p.put_thumbnail(Some(t.id), &spec, None).unwrap_err().to_string();
        assert!(refused(ThumbnailSpec {
            size: Some("huge".into()),
            ..ThumbnailSpec::default()
        })
        .contains("not a thumbnail size"));
        assert!(refused(ThumbnailSpec {
            time: Some(-1.0),
            ..ThumbnailSpec::default()
        })
        .contains("time"));
        assert!(refused(ThumbnailSpec {
            layers: Some(vec![ThumbnailLayerSpec::default()]),
            ..ThumbnailSpec::default()
        })
        .contains("needs"));
        let both = ThumbnailLayerSpec {
            svg: Some("<svg/>".into()),
            ..headline("x")
        };
        assert!(refused(ThumbnailSpec {
            layers: Some(vec![both]),
            ..ThumbnailSpec::default()
        })
        .contains("not several"));
        assert!(refused(ThumbnailSpec {
            layers: Some(vec![ThumbnailLayerSpec {
                layer_id: Some(Uuid::new_v4().to_string()),
                ..ThumbnailLayerSpec::default()
            }]),
            ..ThumbnailSpec::default()
        })
        .contains("no layer"));
        assert!(refused(ThumbnailSpec {
            layers: Some(vec![ThumbnailLayerSpec {
                layer_id: Some(t2.layers[0].id.to_string()),
                values: Some(BTreeMap::from([("a".to_string(), "b".to_string())])),
                ..ThumbnailLayerSpec::default()
            }]),
            ..ThumbnailSpec::default()
        })
        .contains("not a template"));
    }

    #[test]
    fn a_stale_edit_is_refused_and_an_agent_edit_stages_as_one_thumbnails_change() {
        let mut p = Project::sample().unwrap();
        let t = p.put_thumbnail(None, &ThumbnailSpec::default(), None).unwrap().thumbnail;
        let base = p.thumbnail_base(Some(t.id)).unwrap();
        p.put_thumbnail(
            Some(t.id),
            &ThumbnailSpec {
                time: Some(1.0),
                ..ThumbnailSpec::default()
            },
            None,
        )
        .unwrap();
        let stale = prepare_thumbnail(
            base,
            &ThumbnailSpec {
                time: Some(3.0),
                ..ThumbnailSpec::default()
            },
            None,
        )
        .unwrap();
        assert!(p.apply_thumbnail_parsed(stale).unwrap_err().to_string().contains("try again"));

        p.set_actor(EditSource::Agent);
        p.put_thumbnail(
            Some(t.id),
            &ThumbnailSpec {
                name: Some("Agent's".into()),
                ..ThumbnailSpec::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(p.timeline().unwrap().thumbnails[0].name, t.name, "the live cut is untouched");
        let staged = p.staged().unwrap().unwrap();
        assert_eq!(staged.diff.entries.len(), 1);
        assert_eq!(staged.diff.entries[0].kind, DiffKind::ThumbnailsChanged);
        assert_eq!(staged.diff.entries[0].detail.as_deref(), Some("changed “Agent's”"));
        p.set_actor(EditSource::User);
        p.apply_staged(false).unwrap();
        assert_eq!(p.timeline().unwrap().thumbnails[0].name, "Agent's");
    }

    /// `cargo test -p kerf-core --no-default-features -- --ignored a_thumbnail_renders`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_thumbnail_renders_at_its_size_with_its_layers_and_leaves_the_cover_alone() {
        use crate::engine::test_support::StatusBounded;
        let ffmpeg = std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
        let dir = std::env::temp_dir().join(format!("kerf-thumbnail-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("bars.mp4");
        let made = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=s=320x180:r=25:d=4")
            .args(["-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
        let project = Project::open_in_memory().unwrap();
        let asset = project.import_asset(&clip).unwrap();
        project.add_clip_to_timeline(asset.id, None, 0.0, 4.0, Some(0.0)).unwrap();
        let cover = |name: &str| {
            let (timeline, assets) = project.export_still_inputs().unwrap();
            let path = dir.join(name);
            Project::render_still(&timeline, &assets, 1.0, &path, Some(crate::engine::ImageFormat::Png)).unwrap();
            std::fs::read(path).unwrap()
        };
        let before = cover("before.png");

        let plain = project
            .put_thumbnail(
                None,
                &ThumbnailSpec {
                    time: Some(1.0),
                    size: Some("640x360".into()),
                    ..ThumbnailSpec::default()
                },
                None,
            )
            .unwrap()
            .thumbnail;
        let draw = |id: Uuid| {
            let (t, timeline, assets) = project.thumbnail_render_inputs(id).unwrap();
            crate::thumbnail::render(&t, &timeline, &assets, t.width, &|| false).unwrap()
        };
        let bare = draw(plain.id);
        assert_eq!((bare.width(), bare.height()), (640, 360));
        let titled = project
            .put_thumbnail(
                Some(plain.id),
                &ThumbnailSpec {
                    layers: Some(vec![headline("Big news")]),
                    ..ThumbnailSpec::default()
                },
                None,
            )
            .unwrap()
            .thumbnail;
        let with_title = draw(titled.id);
        let (x, y, w, h) = titled.layers[0].rect_px(640, 360).unwrap();
        let differs = |px: u32, py: u32| bare.pixel(px, py) != with_title.pixel(px, py);
        let inside = (y as u32..y as u32 + h).flat_map(|py| (x as u32..x as u32 + w).map(move |px| (px, py)));
        assert!(inside.filter(|&(px, py)| differs(px, py)).count() > 500, "the headline is drawn");
        assert!((0..640).all(|px| !differs(px, 10)), "nothing above the headline's band moves");

        for (name, format) in [("t.png", "png"), ("t.jpg", "jpeg"), ("t.webp", "webp")] {
            let path = dir.join(name);
            let (t, timeline, assets) = project.thumbnail_render_inputs(titled.id).unwrap();
            crate::thumbnail::write(&t, &timeline, &assets, &path, &|| false).unwrap();
            let size = imagesize::size(&path).unwrap();
            assert_eq!((size.width, size.height), (640, 360), "{format}");
        }
        let vertical = project
            .put_thumbnail(
                None,
                &ThumbnailSpec {
                    time: Some(2.0),
                    size: Some("vertical".into()),
                    focus_x: Some(0.2),
                    ..ThumbnailSpec::default()
                },
                None,
            )
            .unwrap()
            .thumbnail;
        let tall = draw(vertical.id);
        assert_eq!((tall.width(), tall.height()), (1080, 1920));

        assert_eq!(cover("after.png"), before, "the video's own frame does not change");

        let (timeline, assets, scenes) = project.frame_suggestion_inputs().unwrap();
        let found = crate::thumbnail::suggest_frames(&timeline, &assets, &scenes, 3, None, &|| false).unwrap();
        assert!(!found.frames.is_empty() && found.frames.len() <= 3, "{:?}", found.frames);
        assert!(found.frames.windows(2).all(|w| w[0].score >= w[1].score));
        assert!(found.frames.iter().all(|f| (0.0..=4.0).contains(&f.time)));
        assert!(tiny_skia::Pixmap::decode_png(&found.sheet_png).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_layer_is_the_image_fitted_to_its_own_size() {
        let png = {
            let mut pixmap = tiny_skia::Pixmap::new(40, 20).unwrap();
            pixmap.fill(tiny_skia::Color::from_rgba8(200, 30, 30, 255));
            pixmap.encode_png().unwrap()
        };
        let url = crate::template::image_data_url(&png).unwrap();
        let svg = image_svg(&url).unwrap();
        assert!(svg.contains("viewBox=\"0 0 40 20\""), "{svg}");
        let doc = SvgDoc::parse(&svg).unwrap();
        assert!(doc.sanitized().contains("data:image/png;base64,"));
        assert!(image_svg("data:text/plain,hello").is_err());
    }
}
