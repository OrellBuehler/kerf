# mcp — brief

Rules only. The full file `.claude/docs/mcp.md` is authoritative and holds the measurements and the story behind each
rule: grep it for the phrase in parentheses.

Orientation: the app is the MCP server (rmcp streamable HTTP at `/mcp`, `127.0.0.1:7777`; also headless and stdio).
Source: `crates/kerf-app/src/mcp.rs` plus `mcp/` (`server.rs` lifecycle, `jobs.rs`, `reads.rs`, `project_tools.rs`,
`templates.rs`, `thumbnails.rs`, `batch.rs`, `hints.rs`, `toolsets.rs`, `prompts.rs`, `activity.rs`, `args.rs`,
`workspace.rs`, `files.rs`, `fetch.rs`), `shell.rs`.
Tools live in `mcp/<domain>.rs` (`library`, `analysis`, `editing`, `linked`, `clip_look`, `keyframes`, `mix`, `music`, `sound`, `titles`,
`captions`, `review`, `tasks`, `delivery`, `looking`), each with its own `tool_router_<module>` summed in `router()`; `mcp.rs` keeps the
server type, shared helpers, `router()` and the four orientation tools. A new tool goes in its domain's module.

**Tool index**: the last section of the full file (`## Tool index`) lists every tool by purpose. A test in kerf-app checks
it names every registered tool, so a new tool MUST be added there (and keep prose free of counts).

## Map of the full file

Top part (no heading, in order): router / instructions / auth / workspace / media in-out / stdio / Shell / rmcp patterns,
then per-tool-family paragraphs (visual, ripple, links, locks, grade, blend, keyframes, motion, behaviours, edit modes,
delivery format, stabilize, variants, captions, voiceover, SVG titles, templates, title clips, title text, import,
export, music, sound effects, master, thumbnails, review_cut, transcript reads, proxies, strict args, batches). Then
`## Prompts and tool sets`, `## Review, ownership and the live cut`, `## Background jobs`, `## Live activity stream`,
`## Small answers, paged reads`, `## Project and settings tools`, `## Tool index`.

## Server shape and rmcp patterns

- `INSTRUCTIONS` is a short routing note, at most 3500 chars (a test pins it), because clients cut at 4096 and defer
  tools; the whole workflow is `EDITING_GUIDE` / `get_editing_guide` (`Instructions and the guide.`).
- Keep tool descriptions short; argument reference goes in `schemars` docs (`Keep a tool's description short too`).
- `#[tool_router]` on the impl + `#[tool_handler]` on `impl ServerHandler`; no `tool_router` field on the struct, or the
  macro calls `Self::tool_router()` (`Patterns that matter if you edit`).
- The routers are summed once in a `router()` `OnceLock`, `#[tool_handler(router = router())]`, because the generated
  methods evaluate the expression per request (`evaluate* the router expression`).
- `ServerInfo` is `#[non_exhaustive]`: `get_info` uses `Default::default()` then sets fields incl. `server_info`
  (`server_identity`), else the server says "rmcp" (`introduces itself to every client`).
- Most tools return `Result<String, McpError>` with **compact** JSON. The visual tools (`get_frame`, `skim_asset`,
  `preview_timeline`, `skim_timeline`, `preview_svg_overlay`) return `CallToolResult` via `image_result` (`png_result` for
  the SVG sheet): text caption + `Content::image` with bare base64 + MIME, never a `data:` URL (`rmcp wants bare base64`).
- Every mutating tool goes through `edit()`: runs the op under the lock, **releases it**, only then emits
  `project-changed`, because the webview's re-fetch takes the same lock (`that order matters`). `lock()` sets
  `EditSource::Agent`. `act()` is `edit()` without agent staging, for non-edits (tasks, apply/discard/undo, history).
  Blocking-pool edits take `lock_agent_edit` (`The blocking-pool edits`).
- Non-cut-edit tools emit their own event, not `project-changed`: `speech-model-changed`, `ripple-mode-changed`,
  `templates-changed` (`which the webview listens for`).
- Under review, read tools (`get_timeline_state`, `timeline_summary`, `preview_timeline`, `export`) go through
  `working_timeline` (`so the agent sees the cut it is building`).
- `core_err` maps caller mistakes (stale id, out-of-range value, stale staged edit) to `invalid_params`, not
  `internal_error` (`splits the caller's mistakes`). Sizes an agent picks are clamped (`are clamped rather than trusted`).
- `Shell` (`shell.rs`): write `self.app.emit(…)` / `self.app.state::<crate::AppState>()`, never `tauri::Emitter::emit(&app,
  …)` or `use tauri::Manager` (`Shell` implements neither); helpers reached from `lib.rs` / `settings.rs` / `recovery.rs`
  take `&impl Host` (`The app around the server is a`).
- Headless: events reach only in-process listeners, `get_editor_context` says `reported: false`, `note_open` skips the
  recent list, agent edits land live (review off for that process; `--review` keeps it) (`Headless, events reach only`).

## Connecting and auth

- Every request needs `Authorization: Bearer <token>` (`mcp_require_token`, default on); constant-time compare in an axum
  middleware reading the *live* token (`Connecting and auth`).
- A different token (or `mcp_require_token` switched) restarts the listener (`needs_restart`) and ends all sessions at
  once (`restarts the listener`).
- The token never reaches a log line (`<redacted>` in `McpConfig`, `Auth`, `Settings`); `generate_token` errors rather than
  fall back to something guessable; an unsaved token is reported as `token_problem` (`never reaches a log line`).
- `.enforce_origin_validation()` with an empty allow-list refuses any `Origin` (a web page must not drive the editor via
  loopback); rmcp validates `Host` against `allowed_hosts` (`refuses any request that carries an`).
- A bind beyond loopback is refused unless the token is required (`bind_problem`) (`is refused unless the token is required`).
- Headless `server::Overrides` (applied in `McpConfig::from_settings`, so settings cannot undo them): on, token required;
  address `--mcp-addr` > `KERF_MCP_ADDR` > stored; token `KERF_MCP_TOKEN` (this run only, never written) > stored >
  generated (`A headless run holds the server to`).
- `set_mcp_settings` writes only `mcp_*` and `agent_review` (`are the other two commands`).

## Workspace (headless `--workspace DIR` / `KERF_WORKSPACE`)

- Every client-named path goes through `KerfMcp::path_arg` (`workspace::confine`) **before** the tool's own checks:
  `import_asset`, `import_captions`, `set_grade` `lut_path`, `open_project`, `save_project_as`, template import/export,
  `image_files` / `image_file`, every `output_path`, and files beside an output (`check_beside`) (`identity without a workspace`).
- Order: URL refusal (`looks_like_url`) and a pure lexical fold-and-compare (`inside`) before any filesystem call; then the
  deepest existing ancestor is canonicalised and must lie in canonical DIR (`a pure lexical fold-and-compare`).
- DIR may not be a filesystem root and may not contain Kerf's settings, data, token file, cache or template library
  (`DIR may not be a filesystem root`).
- A file in DIR must not reach outside it: `set_speech_model` takes only a model name (`check_speech_model`); an opened
  project may name only files in DIR or Kerf's generated-media dirs, spelled with no `.` / `..` part
  (`Project::referenced_files`, `check_project`) (`What a file in DIR could reach outside it`).
- No playlist or manifest: `Workspace::open` turns on `confine_demuxers` so every probe passes `-format_whitelist
  MEDIA_DEMUXERS` (a content sniff is not enough); a media path with `%` is refused (`refuse_pattern`);
  `Workspace::check_media` runs in `import_asset` and for each media file `check_project` sees (`No playlist or manifest`).
- The whitelist guards only the import / open probe: keep one tenant per container; confinement is API-level only
  (`keep one tenant per container`).

## Media in and out (`mcp/files.rs`, `mcp/fetch.rs`; workspace only)

- `files::router` is merged beside `/mcp` only with a workspace, inside the same `require_bearer` layer, repeating the
  `Origin` (403) and `Host` checks (`no router without a workspace`).
- Request path must be **plain** (`check_plain`), then `locate`s through `Workspace::resolve` and must equal DIR joined with
  it, so any symbolic link is a 403; GET opens with `O_NOFOLLOW` (`must be **plain**`).
- PUT streams to `<name>.<8 hex>.part`, `sync_all`, then `place`s: `?overwrite=1` renames, otherwise a hard link that fails
  on an existing name (409); under 1 GiB free (`check_room`) is 507 with the `.part` gone (`streams the body to`).
- **`refuse_in_use`** runs on every PUT (before and after the body), DELETE and `import_url` destination: refuses any
  `*.kerf-journal` / `-wal` / `-shm` name (SQLite replays a hot journal into the project that opens it), a name whose
  `.part` passes 255 bytes, and any file the open project uses (`Project::output_path_inputs`, `paths::same_file`)
  (`runs on every PUT`).
- File-system calls run on the blocking pool (`on_disk`) (`The file system calls run on the blocking pool`).
- An uploaded file is bytes, not trusted media: trusted only after `Workspace::check_media` (via `import_run` for
  `import_asset` / `import_url`, `check_project` on open) (`An uploaded file is bytes, not trusted media`).
- `WORKSPACE_TOOLS` are absent from `tools/list` without a workspace, so the desktop lists exactly what it did;
  `import_url` only with `--allow-url-import` (`files::offered`).
- `import_url` is a long tool sharing `import_run`; a given path must be free; a file the import refuses is deleted
  (`a long tool: download on the blocking pool`).
- **SSRF** (`fetch.rs`): only `http` / `https`, no URL credentials; each hop's host resolved here and refused unless
  **every** address is public (`is_public`); connect through `Pinned` (vetted addresses only, no re-lookup), never a proxy;
  redirects by hand, each rechecked, at most 5; size cap on announced length and bytes (`is resolved here (10 s)`).
- Also refused: this machine's own addresses (`own_addresses`) and two cloud metadata IPs by name; unresolvable and
  non-public hosts give the same message, which never names the address (`which never names the address`).
- Body must be `identity` (any other `Content-Encoding` refused); at most four stalled reader threads (`READERS`)
  (`The body must be`).

## stdio

- `server::start_stdio` makes no listener or token (the pipe is the trust boundary); one `KerfMcp::over_stdio` carries
  session id `stdio-<uuid>` and the `--tools` selection; `Listener::lasting()` never drops a quiet client's subscriptions
  (`of a listener and a token`).

## Visual and measuring tools

- `preview_timeline` stays the one-shot still (`composite_timeline_frame`, working cut, exact time); the scrub cache is the
  GUI's (`stays the one-shot`).
- `skim_timeline`: cells drawn up to four at a time via `engine::timeline_sheet_cancellable` (niced, first failure or cancel
  stops the rest), tiled by one more ffmpeg; caption indexes cell -> time -> clips from `still_layers`; `cell` never
  rebuilds the sheet; no Tauri command (`A gap is the still's black canvas`).
- `get_scopes` measures instead of showing (`kerf_core::scopes`): reads the working timeline, takes the request's
  cancellation; `max_width` clamped to 3840; there is no tool for the render bar (`measures instead of showing`).
- `region` on `get_frame` / `preview_timeline` is cropped *before* the scale to `max_width`; a zoom reads the **original**
  source (`decode_preview_region`), never upscales; a full region is the byte-identical plain decode; no general image-ops
  tool, deliberately (`Look, then look closer`).
- Reads that decode take the request's cancellation token (`so a dropped request kills its ffmpeg`).
- `get_waveform_range` rejects `end <= start` as `invalid_params`, since the engine reads it as zeros and a model would take
  silence (`rejects a window`).

## Ripple, layout, links and locks over MCP

- `set_ripple_mode` flips the user's toolbar flag, so the core refuses it for an agent while review is on; use the `ripple`
  argument (`the core refuses it for an agent while`).
- `rippled` in `remove_clips`' answer is *measured* (`Timeline::clips_moved_since`), not the mode echoed (`not the mode echoed back`).
- Edits that follow the mode (`trim`, `set_speed`, `set_speed_ramp`, `remove`, `remove_clips`, `add_clip_to_timeline`,
  `split_at`, `split_remove`, voiceover / music placement) take optional `ripple` = `project.with_ripple(p.ripple, …)`
  around the core call; descriptions say the push can be skipped (`the push can be skipped`).
- Ops that decide their own layout (`ripple_delete`, `cut_clip_range`, `snap_to_beats`, `move_clip(s)`, `roll_edit`,
  `slip_clip`, `slide_clip`, `reorder`, `duplicate_clips`, `split_all_tracks`, `lift_range`, `extract_range`,
  `ripple_delete_gap`) take no `ripple`; pinned by
  `ripple_is_an_optional_argument_on_exactly_the_edits_that_follow_the_mode` (`decide their own layout`).
- Range edits `split_all_tracks { at, track_ids?, link? }`, `lift_range` / `extract_range { from, to, track_ids?, link? }`,
  `ripple_delete_gap { track_id, at, link? }` are `edit()` around `with_links` and no `with_ripple`; `track_ids` omitted is
  **every unlocked track** (locked tracks holding a clip the edit would have cut, or moved, are named under `skipped`); the descriptions say lift
  leaves the gap, extract closes it, text overlays, floating titles and markers do not move on an extract, a remainder under one
  frame refuses the range with the nearest one that works (never changed; `not_cut` for a cut), and extract carries a lead-only
  sound like the gap delete (the lock trims the linked sound of the neighbouring shot, `lock_trimmed`; an unlinked clip refuses);
  all batch
  (`Range edits over MCP`).
- `link` is optional on exactly the edits that carry linked clips, pinned by
  `link_is_an_optional_argument_on_exactly_the_edits_that_carry_linked_clips` (`pins it against the generated schemas, the way`).
- `set_speed_ramp`: `with_links` over `with_ripple`, batchable; keys are clip-local *timeline* seconds; core refuses more
  than 16 keys and two at one time; empty `keys` clears (`set_speed_ramp { clip_id, keys:`).
- Linked A/V: `set_volume` / `set_fade` / `set_clip_enabled` on a picture with detached sound (`source_audio: false`)
  say its linked audio clip is the one to edit; `extract_audio` no longer appends an asset not on a video track (that is
  `add_asset_audio`) (`Linked A/V over MCP`).
- Locked tracks hold over MCP: every layout edit on a locked track is refused, staged ones included, even for a lock set
  while the proposal was open; an agent may lock but not unlock, nor remove a locked track; `cut_clip`, `remove_silence`,
  `add_asset_audio`, `concatenate` use the first **unlocked** track (`Locked tracks hold over MCP`).
- `roll_edit` / `slip_clip` / `slide_clip` answer `EditOutcome`; a clamp to nothing is `invalid_params` naming the limit;
  `slip_clip` delta is in *source* seconds; `split_remove`'s `side` is an enum so a typo fails at the schema (`Edit modes over MCP`).
- `set_transform_at` gives a keyed number a key at `time`; the rest and the crop stay static (`Clip::pose_transform`)
  (`poses a clip at a moment`).

## Look and animation tools

- `set_grade`: omitted parts kept; a new LUT is read and checked (absolute, local, regular file, no URL) before the lock;
  nested args are MCP-only mirrors with `deny_unknown_fields` while the persisted model stays lenient (`Grading over MCP`).
- `set_blend_mode`: audio-track clip refused; unknown mode is `invalid_params` listing the modes; description must keep the
  opacity / mask / key / fades, letterbox and cost notes (`Blend modes over MCP`).
- `set_property_keyframes` mask numbers are refused on a clip with no mask; `PropertyTrackArg` requires `keys` and denies
  unknown fields, because the model's `PropertyTrack` reads a misspelt `keys` as none and makes the number static; a number
  twice or none in `set_property_tracks` is refused (`Per-property animation over MCP`).
- `apply_motion_preset` / `set_behaviors`: one revision, all or nothing, a clip named twice once, an audio-track clip
  refuses the lot, a locked track does not; `duration` is **clamped**; the preset list in the description is held to
  `motion_presets()` by a test; `set_behaviors` replaces the whole list (max 8), a number past its limit is
  `invalid_params`, the `behaviors` schema description must carry every limit (a test holds it to the core's constants);
  behaviours ride on top of pose and keys, GPU preview falls back to FFmpeg; `bake_behaviors` refuses a clip it would put
  past 1000 keys a number (`Motion presets over MCP`).
- `set_delivery_format`: `fit` is `cover` (default), `contain` or `blur`; `fps` 1..240 (`29.97` etc. made exact
  `n * 1000 / 1001`); **omitting `fps` keeps the current rate**, `0` clears it, `fps` alone changes only the rate, none of
  the three clears the frame; without it an export takes the **first** clip's rate, so the tool, `export` and the
  instructions say to set it (`keeps the project's current rate`).
- `export` reports the frame it wrote from the same snapshot the render reads (`kerf_core::exported_frame`); each
  `export_variants` output has its own (`variant_frame`) (`reports the frame it wrote`).
- `stabilize_clip`: the edit first (unnamed fields keep the clip's value; an equal setting is no edit, opens no proposal),
  then the analysis on blocking with the lock released (`stabilize_inputs` under `lock_agent_as`) (`stabilize_clip { clip_id, enabled?`).

## Delivery, captions, voiceover

- `smart_crop` is paired with `set_delivery_format` by the instructions. `export_variants`: framing pass first unless
  `smart_crop: false` (its one write; `project-changed` only when a clip changed); each file judged at *that* file's frame
  and the `range` (`variant_output`); a cancelled single pass removes every file (`one-call multi-format`).
- Captions go **last** and are re-run after any further edit, because they are placed in timeline time and a trim moves
  the words (`caption **last**`).
- `import_captions`: exactly one of `path` / `text`; `base` contradictions refused by `CaptionTimeBase::resolve` (shared with
  the GUI); parse on blocking *before* the lock; replaces the generated set (`is the same step for a`).
- `export_captions` / `export_interchange` / `export_markers` refuse to overwrite without `overwrite` and build the file with
  the lock released (`refuses to overwrite without`).
- `generate_voiceover`: the asset lands at once, placement and captions stage; re-emits `voiceover-progress` (`narrates a script onto`).

## SVG titles, templates, title clips, title text

- `add_svg_overlay`: exactly one source (`svg_source` refuses both, neither, a preset without `params`, `params` without a
  preset); `preset_kind` on `update_svg_overlay` switches kind in one edit and is refused on a custom title
  (`Animated SVG titles over MCP`).
- **The document is read before the lock**: add / update read the source on `blocking` (`SvgSource::check`), then `edit()`
  runs `*_parsed`; answers carry an `SvgOverlaySummary` with **no source** (`The document is read before the lock`).
- `validate_svg` is read-only, no lock; `preview_svg_overlay` resolves `overlay_id` from `working_timeline()`, releases the
  lock, then draws (`svg_preview`); exactly one source; a sheet over 16 Mpx (`raster::MAX_SHEET_PIXELS`) or an unholdable
  `times` list is `invalid_params` (`Writing one is a loop`).
- `get_timeline_state` elides an SVG source over 4 KiB (`SVG_INLINE_LIMIT`) unless `full_svg: true` (`elide_large_svg`).
- Descriptions and instructions carry the SVG subset (SMIL only, times relative to the overlay's start, `viewBox` required)
  and the loop validate_svg -> preview_svg_overlay -> add_svg_overlay -> preview_timeline; tests pin it
  (`tests pin the texts (including the order`).
- Templates (`mcp/templates.rs`): library writes (`save_template`, `import_template`, `export_template`,
  `delete_template`) are **not edits of the cut**: nothing stages or undoes; they emit `templates-changed`. `save_template`
  reads the title under `lock_agent_as` from `working_timeline()`, releases, then builds. Control args deny unknown fields
  (`Motion templates over MCP`).
- `apply_template` / `set_template_values`: read and prepare on blocking **before** the lock, then `edit()` runs `*_parsed`;
  the core refuses a title that changed in between; an `image` control takes a **file** through `image_files` (at most 8,
  `require_local_input_path`, read at most one byte past `template::MAX_IMAGE_BYTES`); a control in both maps is refused;
  no values is the engine's `InvalidArgument` (`are edits: the file is read and`).
- `import_template`: `require_local_input_path`, the `.kerftemplate` name check, then `validate_import_path` on blocking.
  `export_template` is `export_srt`'s shape and everything that looks at the disk runs in one `blocking` step
  (`export_now`) (`everything that looks at the disk runs in one`).
- **`delete_template` is refused while `agent_review` is on** (a template is the user's work, the delete irreversible)
  (`annotated destructive`).
- The instructions order: author with `id`s -> `validate_svg` -> `add_svg_overlay` -> preview -> `save_template` ->
  `apply_template`; tests pin it (`The instructions place the loop after the SVG one`).
- Title clips: SVG read on blocking before the lock; `apply_template` `on_track: true` refuses x / y / width / height /
  opacity (the clip's transform places it) (`Titles on tracks over MCP.`).
- A template title's `svg` stores text as glyph outlines; `get_timeline_state` shows the text form (`svg::restore_text`,
  before the elision); agents set text through `set_template_values` (`data-kerf-text`).
- `set_title_text_box` / `convert_title_text` (a template title cannot go back to text): prepare on blocking before the
  lock, then `edit()` runs `set_title_text_parsed` (`A title's text over MCP`).
- `svg_bytes` in summaries is the **text form's** size (`text_form_bytes`), not the stored one (`text_form_bytes`).
- `set_title_text_animation`: `preset` xor a whole `animation`; `curve` needs a `preset` or an `animation` that moves,
  scales or turns something; same lock-free shape (`curve preset id`).

## Import, export, music, sound effects, thumbnails, review

- `import_asset` refuses UNC / device paths before any filesystem call (`require_local_input_path`), then URL / relative /
  missing; it is the one write that does **not** stage (`the one write that does **not** stage`).
- `export` forwards progress to the `progressToken` and passes `context.ct` as cancel; a failed or cancelled render removes
  only what *this run* wrote (`kerf_core::discard_partial`, repeated by the tool); the output is refused when the project
  still uses it (`Project::check_output_path`) (`a render runs for minutes`).
- A completed `export` / `export_variants` keeps the backfilled tick: `crate::keep_time_bases` re-asks with no lock held,
  then `Project::apply_time_bases` under `lock_agent_as` (database only: no revision, nothing staged); its failure is
  logged, not an export failure (`keeps the tick it backfilled`).
- A `video_codec` outside `export_capabilities` is the caller's risk (`verified hardware encoders`).
- Export progress goes through an unbounded channel to a forwarder (`jobs::foreground`) that drains even with no token
  (`unbounded channel`).
- Music: `fit_music` decides its own layout (no `ripple`); instructions route analyze -> fit_music -> duck
  (`route music under a cut through analyze`).
- `compose_music`: `score` and `ComposeAlignParams` deny unknown fields; validate and fit **before anything renders**
  (`compose_music_inputs` under `lock_agent_edit` + `with_ripple`; a proposal another session owns refuses here), on the
  **working timeline**; render with the lock released (stopped by `cancel_music`, `stop_or_replaced`); land with
  `place_music` **as given**; a refused placement stores neither asset nor clip; `replace_clip_id` replaces the **whole
  bed**; the ALIGN paragraph of the description is `plan_score_to_picture`'s rule (a test pins the words) (`Composed music`).
- `get_band_balance`: exactly one of asset / clip / range, checked by the core; decode with the lock released
  (`get_band_balance { asset_id?`).
- Sound effects: `at` is where the effect's **hit** lands, not its clip start (the doc, description, guide and
  `list_sound_effects` all say so); `SfxPlacement` and `UpdateSoundEffectParams` have hand-written `Deserialize`
  (`deny_unknown_fields` does not survive `flatten`); `update_sound_effect` takes the **whole** spec (`A placement is flat`).
- Sound-effect inputs run **before anything renders** under `lock_agent_edit` + `with_ripple`; render with the lock released;
  landing is `KerfMcp::edit` around `place_sound_effects` / `apply_sound_effect_update` + `require_generation`; one
  revision; no `background`, no progress (`run **before anything renders**`).
- Sound-effect answers come from core's `SfxPlaced` (`reads::sfx_answer`); counts in texts are pinned to `SfxKind::ALL`;
  **no MCP tool previews a sound**, on purpose; `suggest_sound_effects`' params struct is core's `SfxSuggestRequest` and its
  placements must feed back through `sound_effects_inputs` (`There is no MCP tool that previews a sound`).
- `set_master_limiter`'s text is tied to `MASTER_DEFAULT_CEILING_DB`; `get_levels` measures the working timeline
  (`names the ceiling the engine really`).
- The instructions tell an agent to run `platform_check` before reporting a cut finished (`before reporting a cut`).
- Thumbnails (`mcp/thumbnails.rs`): `add_thumbnail` / `update_thumbnail` (`layers` is the whole new stack) /
  `remove_thumbnail` are staged edits (`edit()`), files and templates read on blocking before the lock; `export_thumbnail`
  refuses to overwrite (`that never appear in it`).
- `review_cut`: read-only, pure review after the lock is released; an unknown check is `invalid_params`; a check whose
  analysis is missing is skipped with the reason, never guessed (`never guessed`).
- The instructions spell out the staging flow: an agent that does not know its edits are held back would report a cut the
  user has not got (`drive it explicitly`).

## Transcript reads, proxies and analysis

- `get_asset_metadata` also answers the file's `size_bytes` / `modified`, statted after the lock is released (`size_bytes` and `modified`).
- `analyze_asset` / `get_asset_metadata` answer an `AnalysisSummary`, never the words; `get_transcript` pages lines
  (<= 500) and adds words only on request (`Transcript reads.`).
- `get_cut_transcript`: inputs under the lock, pure `CutTranscriptInputs::read` after release (`Transcript reads.`).
- `search_transcripts` / `speaker_ranges` / `find_filler_words`: cached JSON read under `lock_agent_as`, parsed and matched
  on blocking with the lock released (`transcript_search_inputs`) (`cut to speaker`).
- `refine_transcript` runs `crate::run_refine`: background lane, stopped by cancellation or the GUI's analysis Stop,
  refused if a newer transcript landed or the project was replaced; emits `analysis-status`, not `project-changed`
  (`the Tauri command's body`).
- `sync_clips_by_audio`: decode + correlate without the lock, `apply_audio_sync` under it (`lock_agent_edit`);
  `project-changed` only when it moved (`sync_clips_by_audio { reference_clip_id`).
- `analyze_asset { steps? }`: omitted, it runs what Settings left on, so an agent cannot fetch a speech model for someone
  who turned transcription off; naming `transcript` overrides; asking for nothing runnable is `invalid_params`
  (`Proxies and analysis steps.`).
- `proxy_status`, `rebuild_proxy`, `delete_proxy` never stage; a queued proxy goes before waiting analysis; proxy and
  analysis tools take the project as the agent (`lock_agent`) (`a cache, not an edit`).
- `proxy_status` answers `{proxies, queue}` (not a bare array): `queue` is `reads::QueueBrief` — jobs in run order with
  `position` and `eta_secs`, `remaining`, total `eta_secs`, `paused`, `waiting_on`, `failed`; jobs and failures cut to 20 except
  the asked-about asset's; the core's rate / pipeline / size left out (`proxy_status` answers `{proxies, queue}`). `rebuild_proxy`
  takes `next: true` = `build_next` (first in line, nothing discarded); cancel / pause / resume of the queue are the user's and
  have no tool (`cancelling, pausing and resuming the queue are the user's`).
- `set_speech_model` is the write side of `transcription_status`: `download_speech_model` only fills the cache
  (`write side of`).

## Strict arguments, hints, editor context

- `call_tool` refuses a key the tool's `input_schema` does not list (`mcp/args.rs`) as `-32602`, because serde silently
  drops unknown keys (`trim { rippel: false }`); only **top-level** keys are checked (`Strict arguments, hints`).
- Parameter structs take no `#[serde(alias)]` (schemars does not list aliases) (`parameter structs take no alias`).
- Every tool needs a row in `mcp/hints.rs` (title, four hints, tool set); `every_tool_is_titled_and_annotated` fails on a
  missing tool or an entry naming none, so **a new tool needs a row there** (`a new tool needs a`).
- Destructive: applying / discarding a proposal, `resolve_task` / `remove_task`, undo / redo / `revert_to`, exports,
  `delete_proxy` (`Staged (or undoable) agent edits are not destructive`).
- `get_editor_context` headless is `{reported: false, stale: true, note}` (`before any report (headless)`).

## Batches (`mcp/batch.rs`)

- `apply_edits` is **one** revision via `Project::batch`, all or nothing; a tool must be in `BATCHABLE` (quick cut-only
  edits; `set_grade` without `lut_path`); at most 200 ops; each op checked before the lock; `EditOp` denies unknown fields
  (`at most 200 ops`).
- Every batchable tool is `edit(|p| Self::<tool>_op(p, params))`: a new one must follow that shape so `ripple` / `link` and
  refusals are the tool's own (`every batchable tool is`).
- A dry run keeps nothing and emits no `project-changed` (`is replaced by that value of op K's`).
- The GUI's `apply_edits` Tauri command calls the same `check` / `run_ops` as the user (live, never staged, up to `MAX_USER_OPS`
  and `USER_DEADLINE`); keep them free of agent-only logic (`runs the same ops through`).
- `run_ops` takes a deadline, checked between ops (the agent's `AGENT_DEADLINE` 10 s, the GUI's 4 s): past it the batch is refused
  whole and rolled back. The tool is `async` over `blocking()`. References copy at most `MAX_REFERENCE_BYTES` per batch, digits
  only after `$`; a `label` is cut at `MAX_LABEL_CHARS`; a user's text starting with `$` is sent as `$$…` (`MAX_REFERENCE_BYTES`).

## Prompts and tool sets

- Prompts name tools in **backticks** and only tools; `every_tool_a_prompt_names_is_registered` fails on a backticked
  non-tool, fewer than five tools, or a long text; field names stay unquoted (`Prompts.`).
- Each `hints::TOOLS` row ends with one `ToolSet`; `every_tool_is_in_one_tool_set` pins it (`Tool sets.`).
- `?tools=core,titles` is read per request from `http::request::Parts` (no session state); `initialize` refuses an unknown
  set; `list_tools` adds `ESSENTIALS` (`needs no session state`).
- **A call to a tool outside the selection still runs**: the selection trims client context, the token is the permission
  (`A call to a tool outside the selection still runs`).

## Review, ownership and the live cut

- Identity: `call_tool` stores `Mcp-Session-Id` (`note_session`, `owner()`); **`edit()`** calls
  `Project::ensure_agent_staging(owner)` first, refusing a proposal another session owns (`StagedEditOwnedByOther`,
  `invalid_params`); `staged_diff` exposes only `owned_by_agent`, never the session id (`Identity.`).
- Review is the user's switch (`agent_review`, default on): with it on, `apply_staged_edits` (`force` too),
  `apply_staged_subset`, `resolve_task` of a task with a proposal are refused by the core (`AgentReviewRequired`) and the
  adapter refuses `undo` / `redo` / `revert_to` (`refuse_live_history`); `set_settings` cannot change it
  (`Review is the user's switch`).
- Review off: `claim_next_task` opens no proposal and edits are ordinary attributed revisions, so tool text, `INSTRUCTIONS`
  and `EDITING_GUIDE` must say there is no proposal to hand over; `stage_edits` is the explicit opt-in; a proposal open
  when review is switched off stays open, is never applied for the agent, and blocks `claim_next_task`
  (`Review off: task work lands live.`).
- A proposal outlives its session: `lock_agent` asks `Project::release_orphaned_staging`; an id never seen is treated as
  alive (release only on evidence); the stdio session is always alive, any other `stdio-…` owner is gone
  (`A proposal outlives its session.`).
- What the user alone does: `discard_unsaved` refused while review is on; `save_project_as` never replaces an existing file
  while review is on; export-family tools and Save As are held off Kerf's own settings and logs (`crate::check_output`);
  `restore_discarded_edits` restores only what an agent discarded; `delete_template` refused (`What the user alone does.`).
- Results are summaries (`reads::timeline_brief`, `reads::track_brief`), not whole timelines (`Results are summaries.`).

## Background jobs (`mcp/jobs.rs`)

- Long tools (`analyze_asset`, `export`, `export_variants`, `generate_voiceover`, `compose_music`, `separate_stems`,
  `download_speech_model`, `import_asset`, `import_url`, `smart_crop`, `stabilize_clip`, `refine_transcript`,
  `sync_clips_by_audio`) take `background: true`; written once against `jobs::Run`; **argument validation stays in the
  tool body, before `long_tool`** (`argument validation stays in the tool body`).
- The GUI's Stop reaches the agent's run via `Run::also_cancelled_by(begin_…())` (a Stop pressed before a run began does
  not end it); voiceover, music, stems, smart-crop and framing also stop when the project is replaced
  (`a Stop pressed before a run began does not end`).
- Every job that resolves inputs under the lock notes `Project::generation()` and refuses to land in a project opened since
  (`require_generation`); a Save As does not count (`refuses to land its result in a project`).
- `export` / `export_variants` word every tick from `ExportProgress`; `progress` is over the whole export and only moves
  forward (`word every tick`).

## Live activity stream (`mcp/activity.rs`)

- Resource-updated notifications are the mechanism; logging notifications are deprecated (SEP-2577) and not used
  (`resource-updated notifications`).
- Producers never wait: one broadcast channel; the project listener runs under the project lock and only wakes
  `pump_project`, which publishes with no lock held; a listener set while marks are pending is woken at once
  (`Producers never wait`).
- A slow client holds no backlog: sends over 10 s are given up, a `Lagged` reader is told everything changed; the task
  holds only a `Weak` of the listener (`so a slow client holds no backlog`).
- Liveness follows the client, not our sends: every client request `touch`es the listener; 5 minutes of silence drops the
  subscriptions, because sends keep rmcp's session alive (and a crashed client's proposal) (`Liveness follows the client`).

## Small answers, paged reads (`mcp/reads.rs`)

- `list_assets` pages (<= 500) and is also cut at ~20,000 chars (`ASSET_PAGE_CHARS`); `fields: "compact"` defaults to `full`
  so no caller loses a field; `imported_after` with `newest_imported_at` taken before importing (`assets_page`).
- Tags are objects, not `"key: value"` strings; `TagArg` denies unknown fields while the model's `Tag` stays lenient;
  `edit_asset_tags` is `edit()` around `Project::edit_asset_tags`; `list_tags` is the agent's view (`Tags.`).
- `assemble_by_tag`: `assemble_by_tag_inputs` + `prepare` on blocking, then `edit(apply_assemble_by_tag)`; one staged step,
  not batchable (`apply_assemble_by_tag`).
- Windowed reads: transcripts / analysis detail, generated captions and clips past 200 in `get_timeline_state`
  (`full: true` for all), `entries` of `staged_diff` / `revision_diff`, `history`, `list_tasks`; `missing_assets` is
  read-only (relinking and removing media are the user's) (`moves the *generated* captions`).

## Project and settings tools (`mcp/project_tools.rs`)

- `save_project_as` reopen is guarded by `change_count` and `require_generation`; the swap keeps the generation so jobs in
  flight still land; each swap `close()`s the replaced project once the lock is released; replacing an in-memory project
  needs `discard_unsaved` (forbidden under review); a swap emits `project-replaced` (`reopen guarded by`).
- `get_settings` / `set_settings` are an allow-list (`READABLE` / `WRITABLE`) — never theme, layout, workspaces, shortcuts,
  token, address or the review switch; `agent_review` is read-only (`are an allow-list`).

## Entry points by name

- Tests that gate a new tool: `every_tool_is_titled_and_annotated`, `every_tool_is_in_one_tool_set`, the tool-index test,
  `every_tool_a_prompt_names_is_registered`, the `ripple` / `link` schema pins, `mcp_parameters_take_no_serde_alias`.
- Helpers: `edit()`, `act()`, `lock()` / `lock_agent_as` / `lock_agent_edit`, `long_tool`, `jobs::Run`, `image_result`,
  `path_arg`, `core_err`, `require_generation`, `stop_or_replaced`, `KerfMcp::over_stdio`.

## Open ends the full file states

- No MCP tool previews a sound, on purpose (`There is no MCP tool that previews a sound`).
- `own_addresses` is not listed on Windows and `check_room` is not checked on Windows (`not listed on Windows`).
- Confinement is API-level only; hosted deployments need OS isolation (`OS isolation`).
- Keys inside nested objects (`export`'s `options`, a `moves` entry) are still dropped by serde (`still dropped by serde`).
