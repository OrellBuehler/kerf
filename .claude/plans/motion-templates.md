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
database when the project opens or a template is applied. The SVG frame cache must not serve frames
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
