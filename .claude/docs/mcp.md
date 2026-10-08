# Embedded MCP server (`crates/kerf-app/src/mcp.rs`)

The app **is** the MCP server — there is no separate binary. `mcp::serve` hosts the
tools over `rmcp` 3.1's **streamable-HTTP** transport (`StreamableHttpService` +
`LocalSessionManager`, nested into an `axum` router) on `127.0.0.1:7777/mcp`
(`KERF_MCP_ADDR` overrides). rmcp validates the inbound **`Host`** header against
an allow-list that defaults to loopback (a DNS-rebinding guard), which would make
every `KERF_MCP_ADDR` override reject its own clients — so `allowed_hosts` (pure +
unit-tested) derives the list from the bind address: a concrete address is added to
the loopback defaults, and a wildcard bind (`0.0.0.0` / `[::]`) can't be enumerated
at all, so it yields an empty list, rmcp's "allow any".
It is spawned from `lib.rs`'s Tauri `.setup` hook on
`tauri::async_runtime` and shares the **same** `Arc<Mutex<Project>>` the Tauri commands
hold, so the agent edits the project the user has open. Patterns that matter if you edit
it: `#[tool_router]` on the impl + `#[tool_handler]` on `impl ServerHandler` — **no
`tool_router` field on the struct** (the macro would call `Self::tool_router()`).
That default is also the reason for the `router()` `OnceLock`: the generated
`call_tool` / `list_tools` / `get_tool` each *evaluate* the router expression, so
`Self::tool_router()` rebuilds all ~85 routes — a schema lookup, a boxed handler
and a map insert apiece, ~250 µs of release-build work — on **every request**.
The routes are fixed at compile time, so it is built once and
`#[tool_handler(router = router())]` hands out a borrow.
`ServerInfo` is `#[non_exhaustive]`, so `get_info` builds it via `Default::default()`
then mutates fields — including `server_info` (`server_identity`), because that
default is filled from **rmcp's own** crate identity and left alone the server
introduces itself to every client as "rmcp". Most tools return `Result<String, McpError>` (pretty JSON), but the
three **visual** tools — `get_frame` (a single drill-in frame), `skim_asset` (a
contact-sheet montage of an asset + a text index of cell→timestamp, for finding good
parts) and `preview_timeline` (the composited cut at a timeline time) — return
`Result<CallToolResult, McpError>` built by the `image_result` helper: a caption
`Content::text` plus a `Content::image(bare_base64, "image/jpeg")` block the LLM can
actually *see* (rmcp wants bare base64 + MIME, **not** a `data:` URL).
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
(the tool description warns that the latter flips the *user's* toolbar setting —
a call that wants one different answer passes `ripple` instead), `move_clips`
(`moves: [{clip_id, timeline_start, track_id?}]`, ids parsed by `clip_moves`) and
`remove_clips` (`clip_ids`, answering `{removed, ripple_active, rippled,
clips_shifted}` — `rippled` is *measured* (`Timeline::clips_moved_since`, the clips
standing elsewhere afterwards, matched by id), not the mode echoed back: ripple is an
attempt, skipped on a locked track and declined for a lane the shift would leave
overlapping) are the one-revision group edits, and the edits that follow the mode — `trim`, `set_speed`, `remove`,
`remove_clips`, `add_clip_to_timeline`, `split_at`, `split_remove`, `generate_voiceover`'s
placement — take an optional `ripple` that is `project.with_ripple(p.ripple, …)`
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
**Per-property animation over MCP**: `set_property_keyframes` (`clip_id`, `prop`, `keys:
[{time, value, easing?}]`; `[]` makes the number static again; the clip comes back) animates one number —
a transform number, a colour number or the clip's volume — independently of the rest,
`copy_keyframes` (`from_clip_id`, `to_clip_id`, `props?`, `offset?`) gives another clip the same
animation, and `set_keyframe_easing` takes an optional `prop`; the server `instructions` mention them.
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
`smart_crop` frames each shot for the delivery frame (the server `instructions`
pair it with `set_delivery_format`, since reshaping to 9:16 otherwise keeps
whatever was in the middle). `export_variants` is the one-call multi-format
delivery: `formats` are shape names (`9:16` / `1:1` / `4:5` / `16:9`, or
`WxH` — `Delivery::parse`), it runs the framing pass first unless
`smart_crop` is false (the one write it makes, `project-changed` only when a
clip actually changed), renders through `render_variants` with progress on
the client's token naming the file in flight, and reports each file with the
platforms it is `ready_for` and its non-tip issues — judged at *that* file's
frame via `cut_summary(Some(frame))`, so the agent does not run
`platform_check` per variant afterwards. `generate_captions` / `clear_captions` caption the
cut; its `style` picks `lines` or `word_punch` and the `instructions` say to
prefer the latter for a vertical cut, since nothing in a tool list tells an
agent that the subtitle shape is not what social captions look like. They also
say to caption **last** and to re-run after any further
edit, because captions are placed in timeline time and a later trim moves the
words out from under them — which an agent has no way to infer from the tool
list.
`import_captions` is the same step for a `.srt` / `.ass` / `.ssa` file: an absolute
`path` or inline `text` (exactly one), a `base` (`timeline`, or `source` with an
`asset_id` — `CaptionTimeBase::resolve`, shared with the GUI, refuses a contradiction
rather than preferring one half), the `generate_captions` look and an `offset`; it
reads and parses on the blocking pool *before* taking the lock, and returns the
`ImportSummary` rather than every overlay, and the `instructions` call it the caption
step (last, and it replaces the generated set).
`generate_voiceover` narrates a script onto the `VO` track (optionally captioning the
cut in the same call), forwarding synthesis progress to the client's token and
re-emitting `voiceover-progress` so the GUI shows an agent's voiceover too; like an
import, the generated asset lands for the user at once, while its placement and
captions stage. `voiceover_status` is its read side.
`import_asset` is the one write that does **not** stage — a file on disk is not
an edit to the user's cut, so imported media (and its background proxy) lands for
them immediately, reporting on the same `import-progress` event a lens-pair
stitch drives for the GUI. `export` takes rmcp's `RequestContext` beside its
`Parameters`: a render runs for minutes, so it forwards ffmpeg's progress to the
client's `progressToken` and passes `context.ct` as the cancel callback, deleting
the half-written file on cancel the way the GUI's export does. Progress goes
through an unbounded channel to a spawned forwarder because the render itself is
on the blocking pool and `notify_progress` is async; the forwarder drains the
channel even with no token, so a client that asked for no progress doesn't leave
ticks piling up.
**Music**: `get_music_structure { asset_id }` (compact: bpm, grid, bar starts, intro end,
phrases — no chroma; errors until analyzed, `music: null` without a pulse),
`plan_music_fit { clip_id, target? }` (read-only `MusicFit`) and `fit_music { clip_id,
target?, fade_out? }` (default `fade_out` true; answers `{fit: MusicFitReport, timeline}`;
decides its own layout, so it takes no `ripple`), `set_master_duck { depth_db? }` (the
speech gate; omitted = compressor), and `export`'s `options.loudness` preset. The server
`instructions` route music under a cut through analyze → fit_music → duck.
`set_master_volume` / `set_master_limiter` are staged edits like any other;
`get_levels` (`range?`, `loudnorm?`) measures the working timeline (the proposal) and takes
`context.ct` as its cancel, and the server `instructions` — now a `const INSTRUCTIONS`, so a
test can pin them — send a social cut through it (-14 LUFS, true peak under -1 dBTP, fix with
the master tools or `loudnorm`). `set_master_limiter` names the ceiling the engine really
defaults to (a test ties its text to `MASTER_DEFAULT_CEILING_DB`) and says that a true peak still
over -1 dBTP with the limiter on is fixed by lowering the ceiling, not by switching it on again.
`platform_check` tells it whether the cut is publishable where it is going
(and the server `instructions` tell it to run that before reporting a cut
finished — an agent that assembles a four-minute Reel has done the work and lost
the audience), and `export_cover` writes the thumbnail.
`stage_edits` / `staged_diff` (the entries plus a rendered text summary) /
`apply_staged_edits` / `discard_staged_edits` drive it explicitly, and
`revision_diff` explains a past revision. The server `instructions` spell the flow
out, since an agent that does not know its edits are held back would report a cut
the user has not got.

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
the machine.
`analysis_status { asset_id? }` reads the per-kind state of one asset or all of them. The proxy and analysis
tools take the project as the agent (`lock_agent`: activity stamped, edits attributed). Asking
`analyze_asset` for nothing it can run — everything switched off and no `steps`, or only audio steps on
a file with no audio — is an invalid-params error, not an empty result.
