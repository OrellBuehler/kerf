# Motion templates and professional titles

Direction agreed 2026-10-18, from a read of EffectCraft (`storytold/effectcraft` 0.12.0, MIT OR
Apache-2.0) against Kerf 0.25. Goal: build an animated title such as a brand end card **inside**
Kerf and reuse it across projects as a Kerf object, without a browser renderer.

## Decisions

- **One rasteriser stays.** Titles remain SMIL SVG drawn by resvg for the still, the playback
  stream and the export. No HTML / JS `render(t)`: the webview is WebKitGTK on Linux, the CSP
  forbids it, and export must run headless.
- **EffectCraft is a source, not a dependency.** Its wgpu default features (gles, webgpu) would
  merge into ours, it is git-only, alpha, and works in f32 premultiplied RGBA. Code is ported
  module by module with attribution in `NOTICE`; its brand marks are never copied.
- **Its GPU shaders are not ported.** FFmpeg has no matching filters, so the GPU parity rule cannot
  hold. Effects that run *inside title rendering* (CPU RGBA already) are fair game later.
- **A template instance is self-contained.** Applying a template copies its SVG and fonts into the
  project; a project never depends on the user's library.

## Phases

| Phase | Scope | EffectCraft source |
| --- | --- | --- |
| **1** | `.kerftemplate` format, user library, save / apply / import / export / delete, controls bound to element ids, embedded fonts stored in the project, Templates section in Titles, Inspector controls, MCP + Tauri surfaces | `.ectemplate` manifest + controls model (`crates/project/src/essential.rs`, `engine/templates.rs`) |
| 1b | Generator (title) clip on a video track so a template takes transitions (dip to white) and extends the cut; animated overlay placement | — |
| 2 | Text engine: shaping, measurement, wrapping, per-glyph outlines into the SVG (fonts no longer needed at render) | `crates/text` (skrifa, harfrust, unicode-bidi, unicode-linebreak) |
| 3 | Text animators + range selectors and their 8 presets; AE-style speed/influence easing with overshoot, springs; trim paths; `animateMotion`; artwork entering from outside its viewBox | `text/selectors.rs`, `render/text.rs`, `engine/presets/text_animators.json`, `crates/keyframe`, `crates/path` |
| 4 | Logo to layers: connected-component split + auto-trace into an editable layered SVG title | `path/trace.rs` |
| 5 | Layer styles (shadow, glow, stroke, bevel), curated CPU effects inside titles, Lottie import, alpha export (ProRes 4444 / PNG sequence) | `render/styles.rs`, `crates/lottie`, `crates/effects` |

## Phase 1 contract

### File format: `.kerftemplate`

A zip, read into memory by known names only (never extracted by entry name). Limits: ≤ 64 entries,
≤ 32 MiB uncompressed in total, each entry's declared and actual size checked.

- `manifest.json`
  ```json
  { "format": "kerf-template", "version": 1, "id": "<uuid>", "name": "Kitefun end card",
    "description": "", "duration": 4.0, "width": 1080, "height": 1920,
    "controls": [ { "id": "tagline", "label": "Tagline", "kind": "text",
                    "targets": [ { "element": "tagline" } ] },
                  { "id": "brand", "label": "Brand colour", "kind": "color",
                    "targets": [ { "element": "bg-stop", "attribute": "stop-color" } ] } ],
    "fonts": [ { "file": "fonts/BarlowCondensed-Bold.ttf", "family": "Barlow Condensed" } ] }
  ```
- `title.svg` — the sanitised SMIL SVG. It renders as is: a control's default is whatever its
  targets hold in this file.
- `fonts/*` — TTF / OTF the SVG names, each ≤ 8 MiB.
- `poster.png` — the title drawn at half its duration, for the library grid.

### Controls

`kind`: `text` (an element's text content), `color` (an attribute, through the colour validator),
`number` (an attribute, finite, optional `min` / `max`), `font` (`font-family`, a family the
project or system has), `image` (an `<image>`'s `href`, a `data:` URL under the existing image
rules). A target is `{ element: <id>, attribute?: <name> }`; `attribute` is required except for
`text`. Values are spliced into the source by byte range (roxmltree), escaped, then the result goes
through the sanitiser like any custom SVG. A target id missing from the SVG is a save-time error.

### Model

`SvgOverlay.template: Option<TemplateInstance>` (skipped when `None`):
`{ template_id, name, controls: Vec<TemplateControl>, values: BTreeMap<String, String> }`.
Changing a value re-splices the overlay's current `svg`; the base SVG is not stored twice.

Embedded fonts: a project table keyed by sha256 (`family`, `data`), registered with the SVG font
database when the project becomes the open one or a template is applied. The SVG frame cache must not serve frames
drawn with a different font set.

### Library

`dirs::data_dir()/kerf/templates/<id>.kerftemplate` (`KERF_TEMPLATES_DIR` overrides). Library
writes are app-level, not timeline edits; applying a template is a timeline edit (staged for the
agent).

### Surfaces

| Core op | Tauri command | MCP tool |
| --- | --- | --- |
| list library | `list_templates` | `list_templates` |
| save from an SVG title | `save_template` | `save_template` |
| apply (adds an SVG title) | `apply_template` | `apply_template` |
| set control values | `set_template_values` | `set_template_values` |
| import a file into the library | `import_template` | `import_template` |
| export a library entry to a file | `export_template` | `export_template` |
| delete a library entry | `delete_template` | `delete_template` |

Beyond the table, the Tauri side has `template_poster` (a grid card's PNG as a `data:` URL), `svg_element_ids` (the ids a
control can aim at) and `template_image_value` (an image file as the `data:` URL an `image` control takes); over MCP an
`image` control takes a file through `image_files`, `delete_template` is refused while the user reviews agent edits, and the
library tools emit `templates-changed`. See `.claude/docs/app.md` and `.claude/docs/mcp.md`.

## Phase 1 as built in kerf-core

The contract above stands; this records what the engine does where the contract left a choice, so the
surface and frontend work follows it. Details and limits: `.claude/docs/core-project.md` ("Motion templates").

### Shapes (snake_case JSON, `schemars::JsonSchema` on all but the instance)

- `TemplateControl { id, label, kind: text|color|number|font|image, targets: [{element, attribute?}], min?, max? }`
- `TemplateInstance { template_id, name, controls: [TemplateControl], values: {control_id: string} }`, on
  `SvgOverlay.template` (absent when none). **`values` is read back from the title's `svg` after every
  change and has an entry for every control whose element is still there, except `image` controls**
  (their `data:` URL is in the `svg`; read the `href` of the target element there for a thumbnail).
- `TemplateInfo { id, name, description, duration, width, height, controls, fonts: [family], has_poster, size_bytes }`;
  `TemplateList { templates: [TemplateInfo], problems: [{file, error}] }` (a damaged file is a problem, not a failure);
  `TemplateSaveRequest { name, description?, controls }`; `TemplateSaved { info, warnings }`;
  `ImportOutcome { info, status: added|already_present|added_as_copy, warnings }`.
- Units: `number` values are unitless numbers; a `color` is `#rgb[a]`, `#rrggbb[aa]`, an SVG colour name,
  `none` or `currentColor` (no `rgb()`); a `font` is one family name.

### Calls (what each surface runs, and where the lock is)

| Surface | Core | Lock |
| --- | --- | --- |
| `list_templates` | `TemplateLibrary::default_location()?.list()` | none |
| (poster for a grid card) | `TemplateLibrary::poster(id) -> Option<Vec<u8>>` (PNG, ≤ 512 px) | none |
| `save_template` | `project.template_source(overlay_id)` (cheap) → `TemplateLibrary::save(&source, request)` (fonts, poster: slow) | source only |
| `apply_template` | `lib.get(id)?.prepare(&values)?` (slow) → `project.apply_template_parsed(prepared, start, end?, SvgLayout)` | apply only |
| `set_template_values` | `project.template_edit_inputs(overlay_id)?` (cheap) → `.prepare(&values)?` (slow) → `project.set_template_values_parsed(overlay_id, prepared)` | first and last |
| `import_template` | `TemplateLibrary::import(path)` | none |
| `export_template` | `TemplateLibrary::export(id, dest) -> PathBuf` | none |
| `delete_template` | `TemplateLibrary::delete(id)` | none |

`apply_template_parsed` and `set_template_values_parsed` return `SvgOverlayResult` (the title and the warnings,
including "font not installed"). `set_template_values_parsed` refuses a title that changed after
`prepare` read it (`InvalidArgument`, "changed while ... try again"): prepare again. `template::image_data_url(bytes)`
turns a PNG / JPEG / GIF / WebP file's bytes into the `data:` URL an `image` value takes (a title is held to
128 KiB, so about 90 KB of image).

### Where it differs from, or adds to, the text above

- **Setting values is its own edit, not a field of `SvgOverlayPatch`**: the splice must read the title as it is
  (and the sanitiser takes up to a second and a half), which cannot happen in `check()` before the lock.
  `SvgOverlayPatch.svg` replaces the artwork and **ends the template** (`template` becomes `None`).
- Embedded fonts are the `fonts` table (`sha256`, `family`, `data`), schema **v3** (v2 is the proposal's `owner` / `assets` columns). A project becoming the open
  one makes its fonts *the* embedded set (it replaces the previous project's; opening alone registers nothing); applying a template adds to it. A title is planned
  with the part of the set it names (`fonts::FontSet`), whose digest is in the SVG frame cache key when it is not empty and whose database draws the picture.
  A font no title names is deleted when a proposal is discarded, on Save As and on open.
- A font is carried only when it is a plain `.ttf` / `.otf`, ≤ 8 MiB, ≤ 16 per template, and its `OS/2` licence
  flags allow embedding; otherwise `save` leaves it out and warns. Embedded faces win over installed ones.
- `import` never overwrites: the same id with other content is kept under a new id (`added_as_copy`).
- A library file is listed only when its name is `<manifest id>.kerftemplate`.
- Attribution: `THIRD-PARTY-NOTICES.md` (there is no `NOTICE` file).

## Phase 1b as built

A template (or custom SVG) can be a **title clip**: a generated clip on a video track, so it takes a `transition_in` (a dip to
white from the shot before it), fades, transform / channel keys (the **animated placement**: a title on a track is placed and
keyed by its clip's transform, not by a rectangle), colour and effects, and extends the cut like any clip. Details:
`core-model.md` ("Generated clips"), `core-project.md` ("Titles on tracks"), `engine-cli.md` ("Titles on tracks").

### Shapes

- `Clip.generator: Option<Generator>` (absent when `None`); `Generator::Title(TitleClip {name, svg, template?})`, JSON
  `{"kind": "title", "name", "svg", "template"?}`. `asset_id` is `GENERATED_ASSET` (nil UUID). The clip's source is the
  document's clock (`source_in` = the head cut off, `Clip::generator_time`); speed 1, not reversed, not linked, on a video
  track, within `MAX_SVG_SECONDS` (`check.rs` invariants, so every op is covered). Schema **v5** (a marker step, no table change).
- `TitleClipPlacement {track_id?, name?}`, `TitleClipResult {clip, track_id, warnings}`, `TitleClipPatch {name?, svg?}`,
  `TitleEdited::{Overlay(SvgOverlayResult), Clip(TitleClipResult)}`.

### Calls

| Surface | Core | Lock |
| --- | --- | --- |
| `add_title_clip` (Tauri, MCP) | `SvgDoc::parse` → `project.add_title_clip_parsed(doc, start, duration?, placement)` | place only |
| `apply_template_clip` (Tauri); `apply_template` `on_track: true` (MCP) | `lib.get(id)?.prepare(&values)?` → `project.apply_template_clip_parsed(prepared, start, duration?, placement)` | place only |
| `update_title_clip` (Tauri, MCP) | `SvgDoc::parse` (if `svg`) → `project.update_title_clip_parsed(id, patch, doc)` | edit only |
| `set_template_values` (either id) | `template_edit_inputs(id)` → `prepare` → `set_title_values_parsed(id, prepared)` | first and last |
| `save_template` (either id) | `template_source(id)` (a clip's length is `source_out`) | source only |

### Left for later, deliberately

- **Presets are not title clips**: a preset is laid out for its length, and a clip's length changes under many ops (trim, roll,
  split, ripple); regenerating it on each would re-time the animation behind the user's back. A template or custom artwork holds its
  end instead. A preset could become a clip once it has a "made for length" rule (or by converting it to custom art first).
- No conversion between a floating title and a title clip yet (the overlay's rectangle maps onto a clip transform exactly only
  given the document's aspect; straightforward to add in `title_clips.rs` when the UI wants it).
- The stream names a title clip's pictures for the slice it plays (a stream from another playhead redraws the rest of the title);
  the floating titles' grid-named reuse (`stream_sequences`) could be extended to clips.
- A dip or fade acts on the title's own pixels (its chain is the clip's): a transparent title over nothing dips only its artwork.

### Notes for phase 2 (text engine) and later

- **Everything a title draws goes through one string, `svg`**, on a floating title (`SvgOverlay.svg`) or a title clip
  (`TitleClip.svg`); read both through `Timeline::title_sources()` (fonts) or `Generator::svg()`. If phase 2 bakes glyph outlines into
  the SVG at apply / set-values time, do it in `Template::prepare` / `TemplateEditInputs::prepare` and both kinds follow; if it
  changes how a document is *drawn*, `svg/raster.rs` and `svg_layer.rs` serve both (title clips reuse `seq_of` / `still_of` through
  `Art`). Bump `RASTER_VERSION` if the pictures change.
- The embedded-font rules are unchanged for clips: a title clip's source names its families like a floating title's
  (`collect_unused_fonts` reads `title_sources`, the drawing takes `fonts::snapshot().named_by([svg])`). Once outlines replace text,
  `fonts::names_family` stops seeing a family in the source, so revisit `collect_unused_fonts` (keep the font while the template's
  controls can still re-shape text).
- A new kind of generated picture (Lottie in phase 5) is a new `Generator` variant: `Generator::svg()` would become a "how to draw"
  dispatch, and `svg_layer::clip_sequences` the place that asks for pictures. The graph only needs a sequence of PNGs on the output
  grid (`ClipStamp`), so any CPU renderer fits.
- The golden family is `title_clip_for` (cases 6400..6800); append another family rather than changing its dice.

## Phase 2 as built (part 1: the engine and outlines)

The text engine is `kerf-core/src/text/` (ported from EffectCraft `crates/text`, trimmed to SVG text: `face.rs`, `layout.rs`,
`path.rs`), on the harfrust / skrifa / unicode-bidi / simplecss versions usvg already links, plus `unicode-linebreak`. The SVG
binding is `svg/outline.rs`. Details: `core-project.md` ("Text as glyph outlines").

- **Where**: `Template::prepare` and `TemplateEditInputs::prepare` (so floating titles and title clips both), as the 1b notes
  suggested. Values always go into the **text form** (`svg::restore_text`), then `SvgDoc::outlined(db)` sets it again.
- **Stored form**: the `<text>` renamed `<g data-kerf-text="">`, everything it had still inside, plus a generated
  `<g data-kerf-glyphs="">` (bounding-box path, one path per glyph). Reversible byte for byte; `read_values`, `apply_values`, the
  controls and the save dialog work unchanged. **Schema step v8**, a JSON step with no table change: the shape is the same
  string, but an older build draws the title and cannot set its text again (it splices into the hidden runs).
- **Fonts**: the `font-family` stays in the source, so `collect_unused_fonts` keeps a template's fonts while its controls can
  set the text again, and the `FontSet` / cache key are unchanged. A missing font no longer changes the picture.
- **Fidelity**: glyph origins and the bounding box match usvg's to 0.02 units, the picture to anti-aliasing. Whatever the engine
  cannot draw exactly stays text (the list is in `outline.rs` and `core-project.md`).
- **Wrapping**: SVG 2 `inline-size` and `white-space: pre-line` (with `line-height`) are honoured by outlines only.
- **Golden / raster**: no graph change, `RASTER_VERSION` unchanged, no new golden family.

### Left for part 2 and later

- A way to set the box in the product: `inline-size` from the save dialog or inspector, and **fit** (shrink a text to its box,
  the engine's `Layout::width` is the measure), so a long tagline wraps or fits without editing the SVG.
- Outlining custom (non-template) titles on request ("convert text to outlines"), a Tauri command and an MCP tool.
- More of SVG text as outlines: positioned `<tspan>` lines (`x` + `dy`), a gradient or pattern paint (one bounding box for the
  text), decorations, right-to-left paragraphs, small caps and font features, `font-variation-settings`.
- Phase 3 builds on the per-glyph paths: `Glyph.cluster` and `Glyph.line` give the character, word and line indices the
  selectors need (the paths are in visual order).
