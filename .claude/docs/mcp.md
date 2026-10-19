# Embedded MCP server (`crates/kerf-app/src/mcp.rs`)

The app **is** the MCP server — there is no separate binary. `mcp::start` (called from `lib.rs`'s Tauri `.setup`
hook) hosts the tools over `rmcp`'s **streamable-HTTP** transport (`StreamableHttpService` + `LocalSessionManager`, nested
into an `axum` router) at `/mcp`, on `127.0.0.1:7777` by default. The server's whole lifecycle is `mcp/server.rs`; the
tools are in `mcp.rs` and four submodules (`jobs.rs`, `reads.rs`, `project_tools.rs`, and `server.rs` itself), each
with its own `#[tool_router(router = …)]` impl — `router()` adds them together once.

**Connecting and auth** (`mcp/server.rs`, settings `mcp_enabled` / `mcp_addr` / `mcp_require_token` / `mcp_token`,
`mcp_commands.rs` for the GUI). Every request needs `Authorization: Bearer <token>` (`mcp_require_token`, on by default;
the token is 32 random bytes in hex, made on the first start and kept in the settings file, compared in constant time by an
axum middleware that reads the *live* token). **A different token (or `mcp_require_token` switched) restarts the
listener** (`needs_restart`): every session opened under the old credentials ends with it, so a session id from before
no longer answers (404, and the old token 401s) — revoking a token is immediate, not "the next request"; the clients
that hold the new token initialize again. A refused request is logged at most once every ten seconds with how many were
left out. The token never reaches a log line (`McpConfig`, `Auth` and `Settings` print `<redacted>`), `generate_token` retries the OS
and returns an error rather than falling back to something guessable (or panicking), a token that could not be saved is kept
for the run and reported as `token_problem` in `mcp_connection_info` (and one that could not be made keeps the server off
while a token is required, saying so as its `error`). rmcp's
`.enforce_origin_validation()` with an empty allow-list refuses any request that carries an `Origin` (native clients send
none; a web page must not drive the editor through the loopback port), and rmcp validates the inbound **`Host`** against
`allowed_hosts` (loopback plus whatever the bind address names; a wildcard bind yields an empty list = allow any). A bind
beyond loopback is refused unless the token is required (`bind_problem`, also checked when settings are patched).
`KERF_MCP_ADDR` still overrides the stored address. A supervisor task restarts the listener when the address or the enabled
switch changes (`settings::update` → `settings_changed`; sessions on the old listener end), and reports a bind failure through
`server_error()` instead of exiting. `mcp_connection_info` returns the URL, the token, `running` / `error` and the ready-made
`claude mcp add --transport http kerf <url> --header "Authorization: Bearer <token>"` plus a generic `mcpServers` JSON snippet;
`regenerate_mcp_token` and `set_mcp_settings` (only `mcp_*` and `agent_review`) are the other two commands.
It shares the **same** `Arc<Mutex<Project>>` the Tauri commands hold, so the agent edits the project the user has open.
Patterns that matter if you edit
it: `#[tool_router]` on the impl + `#[tool_handler]` on `impl ServerHandler` — **no
`tool_router` field on the struct** (the macro would call `Self::tool_router()`).
That default is also the reason for the `router()` `OnceLock`: the generated
`call_tool` / `list_tools` / `get_tool` each *evaluate* the router expression, so
the sum of the routers rebuilds all ~140 routes — a schema lookup, a boxed handler
and a map insert apiece, ~250 µs of release-build work — on **every request**.
The routes are fixed at compile time, so it is built once and
`#[tool_handler(router = router())]` hands out a borrow.
`ServerInfo` is `#[non_exhaustive]`, so `get_info` builds it via `Default::default()`
then mutates fields — including `server_info` (`server_identity`), because that
default is filled from **rmcp's own** crate identity and left alone the server
introduces itself to every client as "rmcp". Most tools return `Result<String, McpError>` (**compact** JSON: `json()` no longer pretty-prints — indentation is a third of a timeline's weight), but the
four **visual** tools — `get_frame` (a single drill-in frame), `skim_asset` (a
contact-sheet montage of an asset + a text index of cell→timestamp, for finding good
parts), `preview_timeline` (the composited cut at a timeline time) and
`preview_svg_overlay` (below) — return
`Result<CallToolResult, McpError>` built by the `image_result` helper (`png_result` for the
SVG sheet, a drawing JPEG would smear): a caption
`Content::text` plus a `Content::image(bare_base64, "image/jpeg")` block the LLM can
actually *see* (rmcp wants bare base64 + MIME, **not** a `data:` URL). `preview_timeline` stays the one-shot
still (`composite_timeline_frame`, the working cut, the exact time asked): the scrub cache and its windows
(`app.md`) are the GUI's, where frames come one after another from a dragged playhead.
**`get_scopes`** measures instead of showing (`kerf_core::scopes`, `engine-cli.md`): the summary of the frame the
export writes at a time — per channel min / max / mean, pixels clipped at 0 and 1 counted over every pixel, the mean
chroma's hue and saturation, the densest vectorscope spots — so an agent checks exposure, clipping or a cast with no
image tokens; `grids` (waveform / parade / vectorscope / histogram, default 64 x 64 / 48, each `grid_size` field
optional) only when asked, `max_width` clamped to 3840. There is no
tool for the render bar (`get_render_bar`): how the GUI's preview plays a span is nothing an agent acts on.
**Look, then look closer**: `get_frame` and `preview_timeline` take an optional
`region` (a `Region` — fractions of the frame, normalized into it) that is
cropped out *before* the scale to `max_width`, and `skim_asset` takes a `cell`
that opens one sheet cell as a full frame (`contact_sheet_times` recomputes the
cell's moment, so the sheet is never rebuilt). A vision model spends the same
image tokens on whatever it is handed, so a quarter of the frame at 640 px
shows four times the detail of the whole frame at 640 px — and beats a larger
`max_width`, which costs more and still loses small text. A zoom reads the
**original** source rather than the 1280 proxy (`decode_preview_region` — the
proxy threw away the pixels being asked for), at `ZOOM_QUALITY` 2 instead of
the preview's 4, and never upscales: the composite (`timeline_frame_region`)
renders a canvas wide enough for the region alone to be `max_width`, capped at
the delivery frame, then crops. A full region is the byte-identical plain
decode. The caption echoes the region back after normalization so the model's
next crop is in the coordinates that were actually used. There is deliberately
no general image-ops tool — a crop for inspection is how a frame is presented,
not an edit. The `lock()`
helper sets `EditSource::Agent` per-op under the shared lock (the GUI's `project()`
helper sets `User` the same way); every **mutating** tool goes through the `edit()`
helper, which runs the op under the lock, **releases it**, and only then emits a
`project-changed` Tauri event so the webview re-fetches and the edit shows up
live in the GUI — that order matters, because the re-fetch the event triggers
takes the same lock. `set_speech_model` emits `speech-model-changed` instead,
which the webview listens for to re-read the transcription status: it reads that
once at launch, and `project-changed` would re-fetch the timeline, history and
task queue, none of which moved. `set_ripple_mode` is the same shape (it emits
`ripple-mode-changed`, not `project-changed`: a flag, not an edit). **Ripple over
MCP**: `get_ripple_mode` / `set_ripple_mode` read and write the project flag
(the latter flips the *user's* toolbar setting, so the core refuses it for an agent while
review is on — a call that wants one different answer passes `ripple` instead), `move_clips`
(`moves: [{clip_id, timeline_start, track_id?}]`, ids parsed by `clip_moves`) and
`remove_clips` (`clip_ids`, answering `{removed, ripple_active, rippled,
clips_shifted}` — `rippled` is *measured* (`Timeline::clips_moved_since`, the clips
standing elsewhere afterwards, matched by id), not the mode echoed back: ripple is an
attempt, skipped on a locked track and declined for a lane the shift would leave
overlapping) are the one-revision group edits, and the edits that follow the mode — `trim`, `set_speed`, `remove`,
`remove_clips`, `add_clip_to_timeline`, `split_at`, `split_remove`, `generate_voiceover`'s and
`compose_music`'s placement — take an optional `ripple` that is `project.with_ripple(p.ripple, …)`
around the core call (omitted follows the project; `false` is the escape hatch); their
descriptions say plainly that the push can be skipped, and an add *inside* a clip leaves
the overlap.
The ops that decide their own layout (`ripple_delete`, `cut_clip_range`,
`snap_to_beats`, `move_clip`, `move_clips`, `roll_edit`, `slip_clip`, `slide_clip`,
`reorder`, `duplicate_clips`) take none, which `ripple_is_an_optional_argument_on_exactly_the_edits_that_follow_the_mode`
pins against the generated schemas. **Linked A/V over MCP**: `detach_audio` / `reattach_audio` (`clip_id`; detach answers
`{clip, track_id, created_track}`), `detach_audio_clips` (`clip_ids`; one revision, answers
`{detached, skipped}` with a reason per skipped clip), `reattach_audio_clips` (`clip_ids`; one revision, all or
nothing, answers the pictures), and `set_volume` / `set_fade` / `set_clip_enabled` say a picture whose sound was
detached (`source_audio: false`) carries none — its linked audio clip is the one to edit, `extract_audio` (answers the same;
**no longer appends** an asset that is not on a video track — that is `add_asset_audio`),
`link_clips` / `unlink_clips` (`clip_ids`), and an optional
`link` on exactly the edits that carry linked clips (`link_is_an_optional_argument_on_exactly_the_edits_that_carry_linked_clips`
pins it against the generated schemas, the way `ripple` is pinned); `timeline_summary` gives each
track `linked_clips` / `detached_sound_clips`, and the server `instructions` carry one paragraph.
**Grading over MCP**: `set_grade` (`clip_id`, `curves?` `{master, red, green, blue: [{x, y}]}`, `wheels?`
`{shadows, midtones, highlights: {x, y, lightness}}`, `lut_path?` (`""` removes), `lut_intensity?`): omitted parts are
kept, the same `lut_path` without an intensity keeps the current one; a new LUT file is read and checked before the
lock is taken; the clip comes back. The nested curves / wheels are MCP-only mirrors (`CurvesArg`, `WheelsArg`, …) with
`deny_unknown_fields`, so `mastre` is refused; the persisted model stays lenient.
**Per-property animation over MCP**: `set_property_keyframes` (`clip_id`, `prop`, `keys:
[{time, value, easing?}]`; `[]` makes the number static again; the clip comes back) animates one number —
a transform number, a colour number or the clip's volume — independently of the rest,
`set_property_tracks` (`clip_id`, `tracks: [{prop, keys}]`) re-keys several numbers in one edit (the dope
sheet's gestures; a number twice or none at all is refused; each track is `PropertyTrackArg` with `keys` required and
`deny_unknown_fields`, since the model's `PropertyTrack` reads a misspelt `keys` as none and would make the number static), `copy_keyframes` (`from_clip_id`, `to_clip_id`,
`props?`, `offset?`) gives another clip the same
animation, and `set_keyframe_easing` takes an optional `prop`; the server `instructions` mention them.
`set_transform_at` (`clip_id`, `time`, any transform field) poses a clip at a moment of it in one edit without the
agent first working out what is animated: a keyed number gets a key at `time` (bundle or own track, whichever drives
it), the rest and the crop are static (`Clip::pose_transform`); it is what the Preview's handles write.
**Edit modes over MCP**: `roll_edit` (`clip_a`
the earlier clip, `clip_b`, `delta` seconds), `slip_clip` (`delta` in *source*
seconds, positive = later in its own footage) and `slide_clip` answer the
`EditOutcome` JSON (`applied` / `clamped` say how far a clamp let it go; a clamp to
nothing is `invalid_params` naming the limit), and `split_remove` (`side` is the
`SplitSide` enum in the schema, so a typo is rejected at the schema) answers the
surviving clip, `split_remove_clips { cuts: [{clip_id, at}], side, ripple? }` does it
to several clips as one revision (a picture and its sound; ids parsed by `clip_cuts`);
the server `instructions` mention them all. The server `instructions` carry the ripple
paragraph (check `get_ripple_mode` before trimming or removing). Because agent edits **stage**, "live in the GUI" now means the
proposal appears for review, not that the cut changes: the read tools
(`get_timeline_state`, `timeline_summary`, `preview_timeline`, `export`) go through
`working_timeline`, so the agent sees the cut it is building, and
`timeline_summary` carries `staged_changes` so it cannot mistake one for the other,
and a per-track `gaps` list — a hole between clips (or before the first one) is
black picture, which is the kind of defect an agent has to be *told* about since
it never watches the cut. `core_err` splits the caller's mistakes (a stale id, an
out-of-range value, a stale staged edit) out as `invalid_params`: reported as
`internal_error`, a mistyped uuid reads to a model as a broken server rather than
as something it can fix and retry. Sizes an agent picks out of a schema
description — `get_waveform`/`get_energy`/`get_waveform_range` buckets,
`get_frame`/`preview_timeline` widths — are clamped rather than trusted, the way `skim_asset` already clamps its
grid. `get_waveform_range` reads an asset's audio as signed min/max peaks per
channel over a **source-seconds** window (the cached peak pyramid, so the first call
per file decodes and every later window is a slice); it answers in *compact* JSON
(pretty-printing puts each of up to 16k numbers on its own line) and rejects a window
with `end <= start` as `invalid_params`, since the engine reads one as a row of
zeros and a model would take that for silence. `set_speech_model` is the write side of `transcription_status`
(`download_speech_model` only fills the cache; transcription uses whichever model
is *selected*, so downloading without selecting was a silent no-op) — it makes
both writes the GUI picker makes, though the picker itself only re-reads at
launch, so a model an agent selects shows there on the next start.
`sync_clips_by_audio { reference_clip_id, clip_id, force? }` moves a clip into sync with another by
their sound (inputs under `lock_agent`, decode + correlate without it, `apply_audio_sync` under it
again; `context.ct` cancels it, a slot wait goes out as a progress notification) and answers
`AudioSync` (offset, confidence, distinct — `null` for no rival peak —, reliable, inverted, applied);
`project-changed` only when it moved. Its description says only each clip's used source window is
compared. `export_variants` progress says `measuring the mix's loudness` while a static normalization meters. `export`'s `options.loudness_static` is in its description.
`set_delivery_format`'s `fit` takes `cover` (the default), `contain` or `blur` (the whole
picture over a blurred, enlarged copy of itself; a clip placed by its transform gets plain bars).
`smart_crop` frames each shot for the delivery frame (the server `instructions`
pair it with `set_delivery_format`, since reshaping to 9:16 otherwise keeps
whatever was in the middle). `export_variants` is the one-call multi-format
delivery: `formats` are shape names (`9:16` / `1:1` / `4:5` / `16:9`, or
`WxH` — `Delivery::parse`), it runs the framing pass first unless
`smart_crop` is false (the one write it makes, `project-changed` only when a
clip actually changed), renders through `render_variants` with progress on
the client's token naming the file in flight (`rendering 3 together (9:16, 1:1, 16:9)` for a
single pass), and reports each file with the
platforms it is `ready_for` and its non-tip issues — judged at *that* file's
frame and over the span of the export's `range` (`variant_output` →
`cut_summary_in(Some(frame), range)`), so the agent does not run
`platform_check` per variant afterwards. `single_pass` (default true) is `render_variants`'
`together`; the result says whether the run was one (`single_pass`, from `VariantsRun.together`), and a cancelled one removed
every file (`export cancelled; the N files rendering together were removed`). `generate_captions` / `clear_captions` caption the
cut; its `style` picks `lines` or `word_punch` and the `instructions` say to
prefer the latter for a vertical cut, since nothing in a tool list tells an
agent that the subtitle shape is not what social captions look like. They also
say to caption **last** and to re-run after any further
edit, because captions are placed in timeline time and a later trim moves the
words out from under them — which an agent has no way to infer from the tool
list.
`import_captions` is the same step for a `.srt` / `.vtt` / `.ass` / `.ssa` file: an absolute
`path` or inline `text` (exactly one), a `base` (`timeline`, or `source` with an
`asset_id` — `CaptionTimeBase::resolve`, shared with the GUI, refuses a contradiction
rather than preferring one half), the `generate_captions` look and an `offset`; it
reads and parses on the blocking pool *before* taking the lock, and returns the
`ImportSummary` rather than every overlay, and the `instructions` call it the caption
step (last, and it replaces the generated set).
`export_captions` writes the cut's captions (`.srt` / `.vtt`), `export_interchange` the cut as
OTIO or a CMX 3600 EDL and `export_markers` the markers as CSV or YouTube chapters — each
refuses to overwrite without `overwrite`, builds the file with the lock released and the last
two return `{path, report}`. `export`'s options take `embed_captions` / `caption_sidecars`.
`add_marker` / `update_marker` take `duration`, `comment` and `kind` (`comment` / `chapter`).
`generate_voiceover` narrates a script onto the `VO` track (optionally captioning the
cut in the same call), forwarding synthesis progress to the client's token and
re-emitting `voiceover-progress` so the GUI shows an agent's voiceover too; like an
import, the generated asset lands for the user at once, while its placement and
captions stage. `voiceover_status` is its read side.
**Animated SVG titles over MCP**: `add_svg_overlay { svg | preset + params, start, end?, x?, y?, width?, height?,
opacity?, name? }` (exactly one source: `svg_source` refuses both, neither, a preset without `params` and `params`
without a preset as `invalid_params`), `update_svg_overlay { overlay_id, name?, svg?, preset_params?, preset_kind?, start?,
end?, x?, y?, width?, height?, opacity? }` (`preset_kind` switches a preset title to another kind in **one edit** that
keeps its id, name, timing and draw order; a custom title refuses it, and `svg` is exclusive with it and `preset_params`),
`remove_svg_overlay { overlay_id }` and `list_svg_presets` (the catalogue:
`kind`, `label`, `params: [{name, required, default?}]`). The mutating three go through `edit()`, so they stage like
any agent edit; `Error::SvgOverlayNotFound` is `invalid_params` in `core_err`. **The document is read before the lock**:
the add and update tools are `async`, read the source on `blocking` (`SvgSource::check` / `SvgOverlayPatch::check`, up to
~1.7 s for a heavy document), and only then `edit()` runs `add_svg_overlay_parsed` / `update_svg_overlay_parsed` under the
lock. They answer `{overlay, warnings}` with
the overlay as an `SvgOverlaySummary` — **no source** (it can be 128 KiB, and a preset's is generated; `svg_bytes`
says how big), which `timeline_summary` lists the same way under `svg_overlays` (absent when none) — and
`warnings` are what the sanitiser or the SMIL reader dropped, so an agent that wrote CSS animations or
`animateMotion` is told rather than left with a title that does not move. **Writing one is a loop**: `validate_svg { svg }`
(read-only, no lock, on `blocking`) answers `{svg_bytes, warnings, natural_duration}` — the size of the sanitised
document, what would be dropped, and when the animation settles (null for a still or an endless loop) — and never echoes the
source; `preview_svg_overlay { overlay_id | svg, times? | count?, max_width? }` answers a **PNG contact sheet** from
`kerf_core::svg::raster::contact_sheet` plus a text index (cell → seconds after the title appears). `overlay_id` is
resolved under the lock from `working_timeline()` (so the proposal, staged edits included), together with the timeline's
`preset_frame()`, the lock is released, and only then is the document read and drawn (`svg_preview`, pure and unit-tested);
`svg` is a draft (its sanitiser warnings come back in the caption; its length is `SvgDoc::default_span`). `times` are seconds
after the title appears (an overlay's slice `offset` is added for the document clock; the labels in the image are that
clock, the caption says so), `count` (default 6, clamped to 1..=36) spreads cells from the first frame to the end; both
together, neither source or both sources are `invalid_params`, and a `times` list the sheet cannot hold is refused rather than
cut short. `max_width` is the width of **one cell** (default 480, 64..=960); a sheet over 16 Mpx in all
(`raster::MAX_SHEET_PIXELS`: 36 cells at 1920 px would be 100) is `invalid_params` telling to ask for smaller cells or fewer
moments. It shows the title alone over dark grey, not over
the footage — `preview_timeline` is for that. `get_timeline_state` returns the source, but one over 4 KiB
(`SVG_INLINE_LIMIT`; a title can be 128 KiB and a timeline holds 50) is replaced by a note with its size unless
`full_svg: true` (`elide_large_svg` edits the owned working copy, so nothing is lost and field order is unchanged).
The tool descriptions and the server `instructions` carry the
subset: SMIL only (`animate` / `set` / `animateTransform` / `animateColor`), times **relative to the overlay's start**,
a `viewBox` (or width and height) required, the viewBox = the delivery frame for full-frame artwork, what is
dropped (CSS animations, scripts, links and external images or fonts, `animateMotion`; the only image kept is an `<image>`
whose href is a `data:image/png|jpeg|gif|webp` URI) and the loop validate_svg → preview_svg_overlay → add_svg_overlay →
preview_timeline; tests pin the texts (including the order of that loop) and the schemas.
**Motion templates over MCP** (`mcp/templates.rs`, its own `tool_router_templates`; the model is `core-project.md` "Motion
templates"). A template is an animated SVG title kept in the user's library with named **controls** (`text` / `color` /
`number` / `font` / `image`, each aimed at element ids of the artwork). `list_templates` (read-only, no lock: `TemplateLibrary::list`
on `blocking`, `{templates, problems}`; the poster is not returned, an agent looks with `preview_svg_overlay`) and the library
writes `save_template { overlay_id, name, description?, controls? }`, `import_template { path }`, `export_template { template_id,
output_path, overwrite? }` and `delete_template { template_id }` are **not edits of the cut**: nothing stages or undoes, and the
tool emits `templates-changed` (not `project-changed`) so an open library grid can refresh. `save_template` reads the title
under `lock_agent_as` from `working_timeline()` (a staged title can be kept), releases it, and only then builds the file (fonts,
sanitiser, poster); `controls` are `TemplateControlArg`s (`deny_unknown_fields`, so a misspelt key is refused, not dropped).
**`apply_template { template_id, values?, image_files?, start, end?, x?, y?, width?, height?, opacity?, name? }`** and
**`set_template_values { overlay_id, values?, image_files? }`** are edits: the file is read and `prepare`d (splice + sanitiser, up to
~1.7 s) on `blocking` **before** the lock, and `edit()` then runs `apply_template_parsed` / `set_template_values_parsed`, so they
stage like any agent edit (`set_template_values` reads the title it splices from `working_timeline()` and the core refuses a
title that changed in between: "try again"). Both answer `SvgOverlayOutcome` — the title as an `SvgOverlaySummary`, whose new
`template` (`template_id`, `name`, `controls` without their targets, `values`) is how an agent learns the control ids to set,
and `warnings`; `timeline_summary` lists it the same way. `values` are strings by control id (partial; an unknown id is
`invalid_params`); an `image` control takes a picture **file** through `image_files` (control id → absolute path, at most 8,
`require_local_input_path` then `crate::template_image_data_url`: the file is opened, looked at through the handle and read at most
one byte past `template::MAX_IMAGE_BYTES` — 94 KiB, what fits in a title, the limit the engine judges by — and then
`template::image_data_url`), and a control in both maps is refused. `set_template_values` with no values (and no files) is the engine's
`InvalidArgument` ("no values to set"), not an adapter check.
`import_template` takes the import-path rules (`require_local_input_path`, then the `.kerftemplate` name check, and only then
`validate_import_path` on `blocking`); `export_template` is `export_srt`'s shape (`require_local_output_path`, the `.kerftemplate`
extension so no directory or other-extension guessing) and **everything that looks at the disk runs in one `blocking` step**
(`export_now`: `refuse_overwrite`, the refusal of a path inside the library directory — it canonicalises — and `check_outputs_now`,
then the write), the async worker doing none of it. **`delete_template`
is refused while `agent_review` is on** (a template is the user's own work and the delete cannot be undone), annotated destructive
like `export_template`; `save_template` and `apply_template` are annotated as edits, `set_template_values` / `import_template` as
idempotent sets. The instructions place the loop after the SVG one: author with `id`s → `validate_svg` → `add_svg_overlay` →
preview → `save_template` with `controls` → `apply_template`; tests pin that order and the schemas.
**Titles on tracks over MCP.** `add_title_clip { svg, start, duration?, track_id?, name? }` (Edit) and `update_title_clip {
clip_id, name?, svg? }` (Set) read the SVG on `blocking` before the lock and `edit()` runs `add_title_clip_parsed` /
`update_title_clip_parsed`; `apply_template` takes **`on_track: true`** (and `track_id?`) to put the template on a video track
instead (`apply_template_clip_parsed`; `end` becomes the duration; x / y / width / height / opacity are refused with it — the clip's
transform places it). These answer `TitleClipOutcome {title_clip: TitleClipSummary {id, track_id, name, start, end, source_in,
template?, svg_bytes}, warnings}`; `set_template_values` dispatches on the id (`set_title_values_parsed`) and answers the overlay or
the title clip; `save_template` and `preview_svg_overlay` take a title clip's id too. `timeline_summary` lists `title_clips`
(the clips are also counted in their tracks); `get_timeline_state` elides a large title clip's source like a floating one's. The
instructions say a title can be a clip on a track that takes a transition (a dip to white into an end card).
`import_asset` refuses a UNC / device-namespace `path` before any filesystem call (`require_local_input_path`), then a URL,
protocol, relative path or missing file (`validate_import_path`). It is the one write that does **not** stage — a file on disk is not
an edit to the user's cut, so imported media (and its background proxy) lands for
them immediately, reporting on the same `import-progress` event a lens-pair
stitch drives for the GUI. `export` takes rmcp's `RequestContext` beside its
`Parameters`: a render runs for minutes, so it forwards ffmpeg's progress to the
client's `progressToken` and passes `context.ct` as the cancel callback (both through `jobs::Run`; `background: true`
makes it a job instead). A failed or cancelled render removes only what *this run* wrote
(`kerf_core::discard_partial` against the `file_mtime` taken before — core does it, the tool repeats it so a cancel can
never leave debris), and the output is refused when it is a file the project still uses (`Project::check_output_path`:
a source, a proxy, the project itself). `export_capabilities` lists the
verified hardware encoders (it runs the probe); a `video_codec` from outside that list is the
caller's risk, and a `*_vaapi` one on a machine nothing has probed yet waits up to 10 s for the
probe to name the render node before the export starts (`vaapi_device_within`). Progress goes
through an unbounded channel to a spawned forwarder (`jobs::foreground`) because the render itself is
on the blocking pool and `notify_progress` is async; the forwarder drains the
channel even with no token, so a client that asked for no progress doesn't leave
ticks piling up.
**Music**: `get_music_structure { asset_id }` (compact: bpm, grid, bar starts, intro end,
phrases — no chroma; errors until analyzed, `music: null` without a pulse),
`plan_music_fit { clip_id, target? }` (read-only `MusicFit`) and `fit_music { clip_id,
target?, fade_out? }` (default `fade_out` true; answers `{fit: MusicFitReport, timeline}`;
decides its own layout, so it takes no `ripple`), `set_master_duck { depth_db? }` (the
speech gate; omitted = compressor), `stems_status` and `separate_stems { asset_id, clip_id? }`
(async, progress through the token like `generate_voiceover`, cancel via `context.ct`, the
GUI sees `stems-progress`; answers `StemsPlaced`), and `export`'s `options.loudness` preset. The server
`instructions` route music under a cut through analyze → fit_music → duck.
**Composed music**: `compose_music { score, track_id?, at?, align?, replace_clip_id?, ripple?, background? }` is
`generate_voiceover`'s shape: `score` is the core `MusicScore` itself (its schema, with the doc comments as
descriptions; a misspelled field in it, in a section, hit or `align` is an error — `deny_unknown_fields` there and on
the local `ComposeAlignParams` — and `seed` is bounded to 2^53 − 1 so it survives JSON), `track_id` / `replace_clip_id`
/ `align.track_id` are strings parsed with `parse_arg` (so a bad id names its argument), `align` is `{to:
cuts|markers, track_id?, max_tempo_change?}`. The request is checked, and the score fitted to the picture, **before
anything renders** — `compose_music_inputs` under `lock_agent_edit` and `with_ripple(p.ripple, …)` (a proposal
another session owns refuses here, not after a minute of rendering; an invalid score, a bad or locked track, a
replaced clip that is not composed music or is linked, or a placement that would land on another clip of the track
under the ripple in force is `invalid_params` naming the section / chord / lane) and it reads the **working timeline**
(staged cuts included). It then renders with the lock released (`synthesize_music` on `plan.score`, progress stages
`download_soundfont` / `waiting` / `render` to the client's token **and** re-emitted as the GUI's `music-progress`;
the GUI's `cancel_music` ends it too via `begin_music`, and so does a replaced project, `stop_or_replaced`) and
lands it with `place_music(&asset, plan.track_id, plan.at, plan.replace_clip_id)` — the request's own placement,
**as given** (a missing `at` means after what the track holds *when it is placed*) — under `lock_agent_edit` +
`with_ripple(ripple)`: the asset and its score-written analysis land at once, the placement stages, and a placement
that is refused (the cut changed while it rendered) stores neither. `track_id` is not `M`-only: any audio track.
`replace_clip_id` replaces the **whole bed** (every clip of its asset on the track becomes one clip). The answer
is `{asset, clip, score, align, replaced}` — `score` is the score **as rendered** (the aligned tempo and added hits),
`replaced` the ids of the clips that were replaced (`plan.replaces`; empty when a clip was added),
and the asset drops its own `music_score` copy (`list_assets` still shows it, which is how an agent finds the score
to edit before a `replace_clip_id` call). The ALIGN paragraph of the description is the rule of `plan_score_to_picture`
as it is: the dropout before a drop is always added, the riser only when the drop does not follow a build and starts at
least half a bar in (a test pins the words). `music_status` is its read side (SoundFont ready, bytes to fetch,
compositions in flight, the styles). `get_band_balance { asset_id? | clip_id? | range?, sections? }` is
`get_levels`' shape (read-only, resolve under `lock_agent_as`, decode with the lock released, `context.ct` and a
replaced project stop it): exactly one target is checked by the core (`invalid_params` otherwise), and the answer is the
core `BandBalance` (windows, `overall`, `windows_from`, `notes`). The descriptions carry everything a model needs to
write a score — styles, section kinds and what each does to the arrangement, chord names and roman numerals, the hit
kinds in **music** seconds, align's rules, recomposing — and a test pins that they do. The `instructions` route a music
request compose_music (align to the cuts) → fit_music → duck → get_levels / get_band_balance.
`set_master_volume` / `set_master_limiter` / `set_master_true_peak` are staged edits like any other;
`get_levels` (`range?`, `loudnorm?`) measures the working timeline (the proposal) and takes
`context.ct` as its cancel, and the server `instructions` — now a `const INSTRUCTIONS`, so a
test can pin them — send a social cut through it (-14 LUFS, true peak under -1 dBTP, fix with
the master tools or `loudnorm`). `set_master_limiter` names the ceiling the engine really
defaults to (a test ties its text to `MASTER_DEFAULT_CEILING_DB`) and says that a true peak still
over -1 dBTP with the limiter on is fixed by `set_master_true_peak` or a lower ceiling, not by
switching it on again.
`platform_check` (optional `range`, `{start, end}` timeline seconds: judge only the span an export of
that range would write, on the sliced cut) tells it whether the cut is publishable where it is going
(and the server `instructions` tell it to run that before reporting a cut
finished — an agent that assembles a four-minute Reel has done the work and lost
the audience), and `export_cover` writes the thumbnail.
`stage_edits` / `staged_diff` (the entries plus a rendered text summary) /
`apply_staged_edits` / `discard_staged_edits` drive it explicitly, and
`revision_diff` explains a past revision. The server `instructions` spell the flow
out, since an agent that does not know its edits are held back would report a cut
the user has not got.

**Transcript reads.** Words are most of a transcript (~1.4 MB for two hours), so `analyze_asset` and
`get_asset_metadata` answer a summary (`reads::AnalysisSummary`: counts, the head of the text and the speaker
labels; never the words or the lines). `get_transcript { asset_id, words?, speaker?, from?, to?, offset?,
limit? }` (`reads.rs`) pages the lines overlapping the source window, each with its `speaker`, at most `limit`
(default 100, ≤ 500) from `offset` with `total`, `speakers` and `next_offset` to page on; `words: true` (default
false) adds the words, estimated on the fly for a transcript cached before words were recorded.
`speaker_ranges { asset_id, speaker?, join_gap?, offset?, limit? }` (gap clamped 0..10 s, default 0.75) is "cut to speaker": each
speaker's source-time ranges (by each word's label, not the line's majority; a window of `limit` ≤ 2000 per speaker, `total_ranges`
and `more` say what is left) and speaking time.
`find_filler_words { asset_id, fillers?, offset?, limit? }` (≤ 100 fillers) returns fillers by word bounds for
`cut_clip_range`, paged (`total`, `next_offset`; `limit` ≤ 1000). `refine_transcript { asset_id, background? }` re-times words and labels speakers on a cached
transcript through `crate::run_refine`, the Tauri command's body: background lane, `analysis-progress`,
stopped by the request's cancellation or the GUI's analysis Stop (`begin_analysis`), refused if a newer
transcript landed meanwhile or the project was replaced (`require_generation`), `analysis-status` after
(not `project-changed`: only analysis moved). `sync_clips_by_audio` is a long tool the same way (`background`,
progress for a wait on the heavy-job slot, `stop_or_replaced`), and its move stages like any agent edit
(`lock_agent_edit`). `get_scopes` takes the request's cancellation and reads the working timeline (the
proposal); `set_grade` checks a new LUT path like an import (absolute, local, an existing regular file, no URL or
network share) before it reads it.

**Proxies and analysis steps.** `analyze_asset { asset_id, steps? }` runs only the named kinds
(`silence`, `scenes`, `loudness`, `rhythm`, `transcript`, `all`; aliases such as `tempo` / `transcription`
parse) and merges them into the cache; omitted, it runs what the user left on in Settings ›
Analysis, so an agent cannot fetch a speech model for someone who turned transcription off — naming
`transcript` overrides that. It shares `run_analysis` with the GUI command, so the bin's chips update
live for an agent's run too; the result is the cached analysis plus `analysis_status` and, for a partial
failure, `failed: [{step, reason}]` (a run where every step failed is an error; a step that fails does
not stop the others). `get_asset_metadata` adds `analysis_status` and `proxy`. `proxy_status
{ asset_id? }` (one asset or all), `rebuild_proxy` and `delete_proxy` steer the preview cache — a proxy is
a cache, not an edit, so none of them stage. A queued proxy goes before any analysis that is waiting for
the machine, and a `scenes` step and the `skim_asset` sheet read a ready proxy instead of the original (an
SDR YCbCr MP4 / MOV / Matroska source for scenes, any such container for the sheet; `core-project.md`); the zoomed `cell` is still
a full-detail frame of the original.
`analysis_status { asset_id? }` reads the per-kind state of one asset or all of them. The proxy and analysis
tools take the project as the agent (`lock_agent`: activity stamped, edits attributed). Asking
`analyze_asset` for nothing it can run — everything switched off and no `steps`, or only audio steps on
a file with no audio — is an invalid-params error, not an empty result.

**Strict arguments, hints and editor context.** The `call_tool` override refuses a call
naming a key its tool's `input_schema` does not list (`mcp/args.rs`) as invalid params
(-32602), naming the key and the accepted ones (or "it takes no arguments") — serde drops
an unknown key, so `trim { rippel: false }` used to trim with the project's mode and report
success. The accepted set follows `allOf` / `anyOf` / `oneOf` and local `$ref`s (a
`#[serde(flatten)]` struct or enum), and a schema with `additionalProperties` (a flattened
map) accepts anything. Only **top-level** keys are checked; keys inside a nested object
(`export`'s `options`, a `moves` entry) are still dropped by serde. schemars does not list
`#[serde(alias)]` names, so the check would refuse them: parameter structs take no alias
(`mcp_parameters_take_no_serde_alias` scans `mcp.rs` and the `mcp/` modules). Every tool has a `title` and the four hints from the table in
`mcp/hints.rs`, applied to the routes once in `router()`; `every_tool_is_titled_and_annotated`
fails on a tool missing from it (and on an entry naming no tool), so **a new tool needs a
row there**. Staged (or undoable) agent edits are not destructive; applying / discarding a
proposal, `resolve_task` / `remove_task`, undo / redo / `revert_to` (the live cut), the
exports (a file outside the project) and `delete_proxy` are. Reads and previews are
`readOnlyHint`, and `openWorldHint` marks the tools that may download a model.
`get_editor_context` returns what the webview last reported (`app.md`): playhead,
`selected_clip_ids`, `mark_in` / `mark_out`, `visible_range`, `focused_panel`,
`selected_asset_id`, with `age_secs` and `stale`; before any report (headless) it is
`{reported: false, stale: true, note}`. The `instructions` point "this clip" / "here" at it.

## Review, ownership and the live cut

Agent edits **stage** (`core-project.md`). The adapter's part:

- **Identity.** rmcp makes one `KerfMcp` per session, and `call_tool` stores the request's `Mcp-Session-Id` in it
  (`note_session`, read back by `owner()`; none for a client that keeps no session). `lock()` / `lock_agent_as` hand the
  project that id (`Project::declare_agent`), and **`edit()`** — every timeline edit — also calls
  `Project::ensure_agent_staging(owner)` first, which declares the session and refuses a proposal another session owns
  (`StagedEditOwnedByOther`, `invalid_params`); the edit itself then opens the session's proposal when review is on
  (lazily, in its own transaction — a failed edit leaves none behind). `staged_diff` / `StagedEdit` say only
  `owned_by_agent`, never the session id. `act()` is `edit()` without that step, for what
  is not an edit of the cut (tasks, apply / discard / undo of the proposal, history). The blocking-pool edits (voiceover
  placement, stems, smart crop, framings, caption import) take `lock_agent_edit`.
- **Review is the user's switch** (`agent_review`, default on, read at every agent lock from `settings::agent_review()`).
  With it on `apply_staged_edits` (`force` included), `apply_staged_subset` and `resolve_task` of a task with a proposal
  are refused by the core (`AgentReviewRequired`: "the user applies agent edits in Kerf"), and the adapter also refuses
  `undo` / `redo` / `revert_to` (`refuse_live_history`) — they would rewrite the user's live cut with nothing to review.
  `undo_staged_edit` (the last step of the proposal, up to 20), `discard_staged_edits` and `restore_discarded_edits` (the
  last discarded proposal, once) work on the agent's own proposal at any time. `get_settings` shows `agent_review`;
  `set_settings` cannot change it.
- **A proposal outlives its session.** `note_session` records every session id that made a call (`server::note_session_seen`);
  `lock_agent` asks `Project::release_orphaned_staging` whether the proposal's owner is a session that was seen and is
  no longer on any listener (closed, expired, or gone with a restart — a new token ends them all), and clears the owner if
  so, so the agent that reconnected, or the next to edit, adopts its own old proposal with its steps instead of being told
  another agent owns it. An id never seen is treated as alive: ownership is let go of only on evidence.
- **What the user alone does.** `discard_unsaved` on `open_project` / `new_project` is refused while review is on (the
  user confirms throwing unsaved work away, in Kerf's window); `save_project_as` will not replace an existing file while
  review is on, whatever `overwrite` says, and every export-family tool and Save As is held off Kerf's own settings and logs
  (`crate::check_output`, beside the project's sources and proxies); `set_ripple_mode` is refused (the `ripple` argument
  on an edit is the per-call way); `restore_discarded_edits` brings back only what an agent discarded, never what the
  user did; `remove_task` by a session that does not own the task's proposal leaves the task in the queue;
  `delete_template` is refused, since a template is theirs and a delete cannot be undone (Motion templates over MCP, above).
- **Results are summaries.** `apply_staged_edits`, `discard_staged_edits`, `undo`, `redo`, `revert_to`, `smart_crop`,
  `fit_music` and `snap_to_beats` answer `reads::timeline_brief` (durations, clip counts per track) instead of the whole
  timeline; `staged_diff` has the entries (each with an `id`, the key `apply_staged_subset` takes). The tools that
  set a track's switches answer `reads::track_brief` (the track without its clips, with a `clip_count`),
  `set_delivery_format` the format and a `timeline_brief`, `remove_task` the id and how many tasks remain.

## Background jobs (`mcp/jobs.rs`)

`analyze_asset`, `export`, `export_variants`, `generate_voiceover`, `compose_music`, `separate_stems`, `download_speech_model`,
`import_asset`, `smart_crop`, `refine_transcript` and `sync_clips_by_audio` take `background: true` and answer `{job_id}` at once; `job_status`, `cancel_job` and
`list_jobs` read and stop them. Each is written once against `jobs::Run` (`cancelled()` + `progress(fraction, message)`):
a foreground call builds it from the request (`foreground`: the `ct` cancellation token and the `progressToken`, with a relay
task that keeps progress monotonic), a background one from its record in `Jobs` (bounded: 8 running, 48 records, a finished
job forgotten after an hour). `KerfMcp::long_tool` does the branching, so **argument validation stays in the tool body,
before it** — a bad path or id is an error on the call, not a job that fails later. The GUI's Stop reaches the agent's run
of the same job: `analyze_asset`, `refine_transcript` (the analysis Stop), `import_asset`, `download_speech_model`, `export_cover`, `generate_voiceover`,
`compose_music`, `separate_stems`, `smart_crop`, `export` and `export_variants` poll the `StopEpoch` the GUI's own button trips
(`Run::also_cancelled_by(begin_analysis() / begin_import() / begin_model_download() / begin_cover_export() /
begin_voiceover() / begin_music() / begin_stems() / begin_smart_crop() / begin_export())`; a Stop pressed before a run began does not end
it, and a run begun after is not wiped by one), and the voiceover, music, stems, smart-crop and framing jobs also stop when the
project is replaced (`stop_or_replaced`). `analyze_asset` also forwards the GUI's `analysis-progress` events for its asset
to the client (`AnalysisProgressRelay`); `download_speech_model` emits `model-progress` like the GUI's own download.
Every job that resolves inputs under the lock notes `Project::generation()` and refuses to land its result in a project
opened since (`require_generation`) — a Save As does not count as one (`replace_with_saved_copy` keeps it). The reads
that decode (`get_waveform`, `get_waveform_range`, `get_energy`, `skim_asset`, `preview_timeline`, `get_frame`) take the
request's cancellation token, so a dropped request kills its ffmpeg; `export_cover` reports `reading the cut` →
`rendering the cover frame` → `cover written` on the `progressToken`.

## Small answers, paged reads (`mcp/reads.rs`)

`list_assets` pages (`offset` / `limit`, 100 by default, at most 500; `{total, offset, returned, next_offset, assets}`, and
only the assets of the page are looked at on the disk) and lists each asset with `source_missing` (stat, outside the lock), `proxy` (`ProxyBrief`: state, fraction, ETA,
reason) and per-kind `analysis` states; `get_asset_metadata` and `analyze_asset` answer an `AnalysisSummary` (counts, totals,
the first ~600 characters of the transcript); `get_transcript` and `get_analysis_detail` page the lists (`offset` / `limit`,
`next_offset`, windows in source seconds; `get_transcript` also takes `speaker` and `words`: the lines carry their speaker label and the
timed words only on request, `speakers` lists the labels, and the summaries name the speakers but never carry words); `get_timeline_state` moves the *generated* captions into a `captions` window
(`captions_offset` / `captions_limit`; `full: true` for everything) and, past 200 clips, windows the clips too
(`clips_offset` / `clips_limit`, a `clips_window` object with `next_offset`); `staged_diff` and `revision_diff` answer the
diff's numbers and headline with `entries` paged (`offset` / `limit`, 100 by default); `history` and `list_tasks` take
`offset` / `limit` and answer `{total, next_offset, …}`; `missing_assets` lists the assets whose source file is gone (read-only:
relinking and removing media are the user's); `generate_captions` answers a count, the first few overlays and the first hundred ids.
`timeline_summary` carries `media_notes` (the media a cut uses whose proxy is not ready or whose file is gone) and
`clips_without_proxy` per track.

## Project and settings tools (`mcp/project_tools.rs`)

`project_info`, `save_project`, `save_project_as` (absolute `.kerf`, `overwrite`, reopen guarded by `change_count` and
`require_generation`; the swap keeps the generation, so jobs in flight still land), `open_project`, `new_project`
(all three record the open project for the recent list / "reopen last project", as the GUI does):
`new_project` starts an untitled project in a crash-recovery file like the window's New (`recovery::untitled`), and each swap `close()`s the project it replaced once the lock is released (an untitled one's recovery directory goes with it). Replacing a project that exists only in memory (or untitled) needs `discard_unsaved`, which review forbids (above); a swap emits
`project-replaced` (the page reloads like after its own open) and does what the GUI's open does (`restore_speech_model`,
`respawn_proxies`, scrub cache). `get_settings` / `set_settings` are an allow-list (`READABLE` / `WRITABLE`): auto_analysis,
cpu_percent, proxy_size, preview_source, time_display, plus `agent_review` read-only — never the theme, layout, workspaces,
shortcuts, token, address or review switch. `set_settings` emits `settings-changed` with the full `SettingsView` as its
payload, which the page applies.

## Tool index

Every tool, by purpose (a test pins that this list names each registered tool, and another that the README's tool lists and
`### The tools (N)` heading and the site's `toolCount` in `site/hugo.toml` match the registered count — keep prose free of numbers):

- **Project and library**: `project_info`, `save_project`, `save_project_as`, `open_project`, `new_project`, `list_assets`,
  `missing_assets`, `get_asset_metadata`, `import_asset`, `set_asset_projection`, `list_fonts`.
- **Analysis and proxies**: `analyze_asset`, `analysis_status`, `get_transcript`, `speaker_ranges`, `find_filler_words`,
  `refine_transcript`, `get_analysis_detail`,
  `transcription_status`, `download_speech_model`, `set_speech_model`, `proxy_status`, `rebuild_proxy`, `delete_proxy`,
  `get_music_structure`.
- **Looking and measuring**: `timeline_summary`, `get_timeline_state`, `get_editor_context`, `get_scopes`, `skim_asset`, `get_frame`, `preview_timeline`,
  `get_waveform`, `get_waveform_range`, `get_energy`, `get_levels`, `get_band_balance`, `platform_check`.
- **Editing the cut**: `cut_clip`, `add_clip_to_timeline`, `split_at`, `trim`, `reorder`, `move_clip`, `move_clips`,
  `ripple_delete`, `cut_clip_range`, `roll_edit`, `slip_clip`, `slide_clip`, `split_remove`, `split_remove_clips`, `remove`,
  `remove_clips`, `duplicate_clips`, `concatenate`, `remove_silence`, `snap_to_beats`, `sync_clips_by_audio`, `smart_crop`, `add_track`,
  `remove_track`, `get_ripple_mode`, `set_ripple_mode`.
- **Linked picture and sound**: `extract_audio`, `add_asset_audio`, `detach_audio`, `detach_audio_clips`, `reattach_audio`,
  `reattach_audio_clips`, `link_clips`, `unlink_clips`.
- **Clip look and sound**: `set_volume`, `set_fade`, `set_speed`, `set_transform`, `set_transform_at`, `set_color`, `set_transition`,
  `set_grade`, `set_video_effects`, `set_audio_effects`, `set_mask`, `set_clip_enabled`.
- **Keyframes and 360**: `set_keyframes`, `add_keyframe`, `set_keyframe_easing`, `set_property_keyframes`,
  `set_property_tracks`, `copy_keyframes`,
  `clear_keyframes`, `set_reframe`, `clear_reframe`, `set_reframe_keyframes`, `add_reframe_keyframe`.
- **Mix and music**: `set_track_volume`, `set_track_pan`, `set_track_duck`, `set_track_muted`, `set_track_solo`,
  `set_track_locked`, `set_master_volume`, `set_master_limiter`, `set_master_true_peak`, `set_master_duck`, `plan_music_fit`, `fit_music`,
  `compose_music`, `music_status`, `separate_stems`, `stems_status`.
- **Titles, captions, markers, frame**: `add_overlay`, `update_overlay`, `remove_overlay`, `set_overlay_keyframes`,
  `add_svg_overlay`, `update_svg_overlay`, `remove_svg_overlay`, `add_title_clip`, `update_title_clip`, `list_svg_presets`, `validate_svg`, `preview_svg_overlay`,
  `preview_svg_preset`, `list_templates`, `save_template`, `apply_template`, `set_template_values`, `import_template`,
  `export_template`, `delete_template`, `generate_captions`, `import_captions`, `clear_captions`, `export_srt`, `export_captions`, `add_marker`,
  `update_marker`, `remove_marker`, `set_delivery_format`.
- **Voiceover**: `generate_voiceover`, `voiceover_status`.
- **Delivery**: `export`, `export_variants`, `export_cover`, `export_capabilities`, `export_interchange`, `export_markers`.
- **Review and history**: `stage_edits`, `staged_diff`, `apply_staged_edits`, `apply_staged_subset`, `discard_staged_edits`,
  `undo_staged_edit`, `restore_discarded_edits`, `history`, `revision_diff`, `undo`, `redo`, `revert_to`.
- **Task queue**: `list_tasks`, `add_task`, `claim_next_task`, `complete_task`, `fail_task`, `resolve_task`, `remove_task`.
- **Background jobs**: `job_status`, `cancel_job`, `list_jobs`.
- **Settings**: `get_settings`, `set_settings`.
