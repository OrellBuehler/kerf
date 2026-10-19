# kerf-app (`crates/kerf-app/src/lib.rs`, `main.rs`)

Tauri v2 shell. **CSP is on** (`app.security.csp` in `tauri.conf.json`, an object so Tauri can add its hashes): `default-src 'self'`, scripts `'self'` only (Tauri hashes SvelteKit's inline bootstrap in the fallback `index.html`), styles allow `'unsafe-inline'` because the UI is styled with inline `style` attributes, fonts are `'self' data:` (Geist, Geist Mono and Space Grotesk are self-hosted through `@fontsource-variable/*` and bundled by Vite, so the app never reaches a font host — there is no `fonts.googleapis.com` / `fonts.gstatic.com` entry to restore), images `data:` (frames are data URLs), `connect-src ipc: http://ipc.localhost`, no objects or `<base>`. Anything new that loads from the network or a `blob:` has to be added there deliberately. **Panics log a backtrace** (`install_panic_hook` forces capture; the release profile strips only `debuginfo`, keeping the symbol table so frames carry function names at a modest size cost). **One instance per identity**: `tauri-plugin-single-instance` is the first plugin in `run()`. A second launch focuses the running window (unminimizing it) and, when its argv carries a `.kerf` path (resolved against the second launch's cwd by `project_arg`), emits `open-project-file` to the webview, which asks about unsaved work like any other open and calls `open_project`. **The first launch's own argv is honoured too**: `run()` resolves it through the same `project_arg` (`launch_project`, pure — the `exists` check is a parameter) against the process cwd into `AppState.launch` (a `LaunchSlot`), and the webview **pulls** it once with `take_launch_project` after its listeners and first `editor.load()` are in place — a command, not an event, because an event emitted before the page has a listener is lost. It returns `{open: path}` once (a reloaded webview must not reopen it over the user's edits) and goes through the same `openProjectAt` as `open-project-file`, unsaved-work question included. With nothing named on the command line and `reopen_last_project` on, a thread of its own — never the main thread: the file may sit on a share that is offline — asks the disk whether `last_open_project` still exists (`settings::reopen_target`) and offers it to the same slot (`LaunchSlot::offer_reopen`: held for the page's pull, or an `open-project-file` event when the page is already listening), so the page opens it exactly like a named one; a project named on the command line or by a second launch always wins, early or late. `last_open_project` is the project that was open when the last session ended, not the newest recent entry: `note_open` (every way a project becomes the open one, the MCP server's included) sets it on open / save-as and clears it for `new_project` (an unsaved project reopens nothing). **A second launch that arrives while the webview is still booting** would hit the same lost event, so until the webview has asked the slot *holds* its request instead (newest wins; one lock covers both halves, so a request is delivered exactly one way) and only afterwards is it emitted. A `.kerf` argument naming nothing on disk is never opened — `Project::open` would *create* it — and comes back as `{missing: path}` (second launch: the `launch-project-missing` event), which the page toasts as `File not found: …`. (A macOS Finder open arrives as `RunEvent::Opened`, not argv, and no `fileAssociations` are configured; neither is handled.) **The main window starts hidden** (`visible: false`, with `backgroundColor` = Kerf Dark's `surface-app`, which a bun test pins equal to `app.html`'s paint): the webview calls `show_main_window` once the settings are in (theme applied, dock built) and a frame has painted (`reveal.ts`: first the bundled interface fonts, `fontsReady` over `document.fonts.load`, *or* 400 ms, so the first frame is not the system fallback re-flowing; then two animation frames *or* a 150 ms timer, since a hidden page may never get a frame), and a 3 s `REVEAL_FAILSAFE` thread shows it anyway so a crashed bundle cannot leave an invisible app. `reveal_once` over `AppState.main_window_shown` makes the first asker win and the rest no-ops (a timer firing after the user minimized the window must not pop it back up) — but only a `show` that *worked* (a window existed and `show()` succeeded) keeps the claim, so a request that finds no window gives it back and the failsafe retries every 500 ms. A second launch always brings the window forward (`unminimize` + `show` + `set_focus`) and marks the reveal done only if that worked — it can run before the config windows exist, and then the failsafe is still armed. It is a command rather than the window API so the capability needs no `core:window:allow-show` (a Rust test pins its absence), and the debug identity cannot diverge: `tauri.dev.conf.json` merges only `identifier` (also pinned). **Jobs land in the project they started for.** Every job that resolves its inputs under the lock, releases it and
applies a result later notes `Project::generation()` with the inputs and checks it when it comes back
(`require_generation`): import (and the stitch), analysis (each finished step and the final merge — `run_analysis`
is shared with the MCP tool), voiceover, music, stems, levels, band balance, smart crop and the export's framing pass, captions import,
relink and the proxy rebuild's un-decline, and `respawn_proxies` (stops queueing a replaced project's assets).
Where the webview has a quiet "cancelled" string (analysis, voiceover, music, stems, levels, band balance) a replaced project returns
that; elsewhere the error says `the project was replaced while …; the result was dropped`.
`save_project_as` asks the same of its reopen and swaps it in only if `Project::change_count()` (every row the
connection wrote: analysis merged by a background job, the ripple flag, meta, a declined proxy — not only edits) is
what it was; otherwise it saves again under one held lock. The swap is `Project::replace_with_saved_copy`, which
**keeps the generation**: the copy is the same project in another file, so an analysis, import, voiceover or proxy in
flight still lands (`replace_with` — open / new — is the one that moves it). Onto the file already open it does nothing,
and the target is checked like an export's output (`check_output`: sources, proxies, Kerf's settings and logs). **Cancel flags** are all `StopEpoch`s on `AppState` (readable from `mcp.rs` as `app.state::<AppState>()`):
`export_cancel`, `analysis_cancel`, `levels_cancel`, `voiceover_cancel`, `music_cancel` (`cancel_music` → `music cancelled`),
`bands_cancel` (`cancel_band_balance` → `band balance cancelled`), `stems_cancel`, `model_cancel`
(`cancel_model_download`), `cover_cancel` (`cancel_cover_export` → `cover export cancelled`), `smart_crop_cancel`
(`cancel_smart_crop` → `smart crop cancelled`, progress as `smart-crop-progress { fraction }`) and `import_stop`
(`cancel_import` → `import cancelled`). A run calls `.begin()` and polls the closure it gets; `.stop()` ends every run
begun before it and none begun after, so there is no flag to reset under a run about to start (a Stop pressed just
before another run began used to be wiped by it) and a GUI Stop reaches the agent's run of the same job:
`begin_analysis`, `begin_model_download`, `begin_import`, `begin_cover_export`, `begin_voiceover`, `begin_music`, `begin_stems`,
`begin_smart_crop` and `begin_export` are what `mcp.rs` takes for `analyze_asset`, `download_speech_model`,
`import_asset`, `export_cover`, `generate_voiceover`, `compose_music`, `separate_stems`, `smart_crop` and `export` / `export_variants`
(one `export_cancel` for both; a test pins that each `begin_*` listens to its own button and no other). **A replaced project stops its jobs**:
`run_analysis`, voiceover, music, band balance, stems, smart crop and the export's framing pass fold "the generation moved" into their
cancel (`stop_or_replaced` / `Throttled`: the generation is read under the lock at most twice a second), so the ffmpeg
working towards a result that would be dropped is killed instead of finishing.

**Playback ids.** The page builds a stream's id as `session * 2^32 + n` (`playback-id.ts`), the session being what
`playback_session` hands out per page load; `Playbacks` keeps the highest id begun as the one that should run
(`fetch_max`) and the highest stopped id (`stopped >= id` ends a stream), so a start that arrives after a newer one, a
stop that arrives before its start, and a reloaded page whose counter restarted at 1 all resolve by comparison.

**Process start.** `use_bundled_ffmpeg()` sets `KERF_FFMPEG` / `KERF_FFPROBE` and is the **first statement of `run()`**,
before the runtime, the webview or any thread exists (`set_var` races every concurrent read of the environment);
logging is not up yet, so it returns what it set and `setup` logs it.

**Library and project commands.** `remove_asset { assetId, force? }` → `{asset, clips_removed, timeline}` (refused while
a clip of the cut or of a proposal uses the asset unless `force`; the proxies and waveform cache go with the lock
released), `relink_asset { assetId, newPath }` → `Relinked` (`relink_inputs` under the lock, `probe_relink` — the probe and a
comparison of both files' ends — with it released, `apply_relink_probed` under it, `forget_media_caches` released
again; `cancel_import` stops the probe), `check_relink { assetId, newPath }` → `RelinkCheck {analysis_kept, has_analysis}` (the same
lock-free probe and refusals, changing nothing: what a relink would cost, so the prompt can say a different recording drops the analysis first), `missing_assets` → `MissingAsset[]` (`missing_assets_inputs` under the lock,
`find_missing_assets` stat-ing the files with it released: a share that went away can take long to say so), `paths_exist { paths }` → `boolean[]` (the export dialog's
overwrite check), `recent_projects`, `open_recent { path }` (a vanished file is dropped from the list, never created —
`open_project` refuses a path that names nothing too) and `clear_recent_projects`. Staged proposals:
`apply_staged_subset { entryIds, force? }` → `SubsetApplied`, `undo_staged_edit` → `{label, staged}`,
`discarded_staged_edit`, `restore_discarded_edits` (a `StagedEdit` carries `owned_by_agent`, never the agent's session id);
`apply_staged_edit`, `apply_staged_subset` and `resolve_task` make the
proxies again when the proposal also marked assets (a 360 projection). `get_history` and `list_tasks` take optional
`limit` / `offset` (a window from the newest end, oldest first). Ids that fail to parse name their argument
(`invalid clip_id "…": not an id …`, `clip_ids[2]` for a list).

**Not verifiable without a display**: whether the *first visible frame* is already painted when `show()` lands (a hidden window may not render until shown, which is why `backgroundColor` exists) and how long the timer-vs-frame race takes per OS — check both on a real desktop. **Setup warms the fonts**: `std::thread::spawn(kerf_core::fonts::warm)` reads the system fonts (hundreds of faces on a Mac) off the first Play of an animated title. `lib.rs::run()` is the entry (`main.rs` just calls it); it owns the
`Arc<Mutex<Project>>` (cloned into both the Tauri managed state and `mcp::serve`) and
registers a command per `Project` op — reads (`list_assets`,
`get_timeline`, `get_asset_metadata`), `import_asset` / `analyze_asset(assetId, steps?)` (`steps`: any of `silence`,
`scenes`, `loudness`, `rhythm`, `transcript`, `all` — only those run, merged into the cached analysis;
omitted, the kinds Settings leaves on. `run_analysis` is the one path the command and the MCP tool
share, taking the project through a `lock` closure (`lock_user` for the GUI, `lock_agent` for MCP,
which stamps the agent's activity and actor): it streams `analysis-progress` per step and
`analysis-status` — the per-kind state — after each step starts and finishes, **merges each
finished step into the cache as it lands** (so its chip turns done and its markers appear while the
slower steps run), with the lock released for the work, and keeps a cancelled or failed step out of
the cache; nothing to run is an error before anything starts), `analysis_statuses` (every status reads the transcription backend, whose first answer runs `ffmpeg -h filter=whisper`: `warm_transcription_backend` asks it before a project lock is taken — setup warms it on a thread, and the GUI command and the MCP `list_assets` / `analysis_status` call it first — never inside one), preview proxies
(`proxy_statuses`, `rebuild_proxy` (clears a deleted proxy's mark only once the rebuild was accepted), `delete_proxy` → a `ProxyStatus`; the `proxy-progress` event
carries one on every change and `proxy-ready` still follows a landed proxy so the preview re-decodes.
`spawn_proxy` queues through `kerf_core::proxy` and takes its place in front of analysis **before it
returns** — call it from the blocking pool, it may run the cached ffprobe (`set_asset_projection` and
its MCP twin are `async` + `blocking` for that; `open_project` calls `respawn_proxies`, which does
every probe on a thread of its own, so the open never waits for N ffprobes); the MCP import does it
inside its `blocking` closure for that reason. `set_settings` re-queues the missing proxies after a
`proxy_size` / `preview_source` change; `get_timeline_frame`, `get_preview_frame` and `start_playback`
refuse under Proxy only while a clip's proxy is building, `get_frame` does not), speech-to-text (`transcription_status`,
`set_speech_model`, `download_speech_model` → emits `model-progress`; `cancel_model_download` stops it, rejecting `model download cancelled`), ripple mode
(`get_ripple_mode` / `set_ripple_mode { on }` — both answer the bool, a setting
that records no revision and returns no timeline), voiceover
(`voiceover_status`, `prepare_voiceover` / `generate_voiceover` → emit
`voiceover-progress`, `cancel_voiceover`), composed music (`music_status`; `prepare_music` downloads the SoundFont;
`compose_music { request }` takes a core `ComposeRequest` — `{score, track_id?, at?, align?, replace_clip_id?}` — checks
and aligns it under the lock (rehearsing the placement too, so a start on another clip of the track is refused before
the render, as the project's ripple flag decides), renders with the lock released and lands it through `place_music`
with the request's own `track_id` / `at` / `replace_clip_id` as given (a refused placement stores nothing), answering the
`Composed` fields flattened (`asset`, `clip`, `score` as rendered, `align`, `replaced`: the ids of the clips replaced —
the whole bed of `replace_clip_id`, empty when a clip was added) plus the refreshed `timeline`; both emit `music-progress` — the voiceover's payload, stages `download_soundfont`,
`waiting`, `render` — and `cancel_music` rejects `music cancelled`), band balance (`get_band_balance { request }`, a core
`BandBalanceRequest`, answering `BandBalance`; the decode runs with the lock released and `cancel_band_balance` rejects
`band balance cancelled`), every editing
op (`cut_clip`, `add_clip`, `split_clip`, `trim_clip` (optional `timeline_start` so a
left-edge trim keeps the right edge put, atomically), `reorder_clip`, `move_clip`,
`move_clips { moves }` (a group, one revision, all or nothing), `roll_edit { clipA, clipB,
delta }` / `slip_clip { clipId, delta }` / `slide_clip { clipId, delta }` (clamping edit
modes, never ripple), `split_remove { clipId, at, side }` (`"left"` | `"right"`; follows
ripple mode) and `split_remove_clips { cuts, side }` (`cuts: [{clip_id, at}]`, one revision),
`ripple_delete`, `cut_clip_range` (remove a **source-time** span from a clip and
ripple closed — the transcript-editing primitive), `add_track`, `remove_track`,
`set_track_duck`, `set_track_volume` / `set_track_pan`, `set_master_volume` /
`set_master_limiter` / `set_master_duck` (`depthDb?`) / `set_master_true_peak` (`enabled`; the master bus; each returns the
`Timeline`), `plan_music_fit` (`clipId`, `target?` → `MusicFit`), `fit_music` (`clipId`,
`target?`, `fadeOut` → `{timeline, report}`), `stems_status`, `separate_stems` (`assetId`, `clipId?` →
`{placed, timeline}`; streams `stems-progress`, rejects `stems cancelled`), `cancel_stems`, `get_levels` (`range?`,
`loudnorm?` → `Levels`; whole-file, so lock-free; `cancel_levels` stops it, rejecting with
`levels cancelled`), `set_delivery_format` (the project's delivery frame; omit
width/height to clear it), `remove_clip`, `remove_clips { clipIds, ripple? }`
(one revision; `ripple: true` is the multi-select ripple delete, via
`with_ripple`; omitted follows the project's mode), `set_volume`, `set_fade`,
`set_speed`, `set_transform`, `set_color`, `set_transition`, `set_mask`,
`set_grade { clipId, grade }` (curves / wheels / LUT; a LUT path the clip did not have is checked off the lock,
`blocking`, so a curve edit on a clip whose LUT went missing still applies),
`inspect_lut { path }` (→ `LutInfo {path, size, title}`, or why it is not a usable `.cube`),
`set_video_effects`,
`set_audio_effects`, `set_keyframes` / `add_keyframe` / `clear_keyframes`,
`set_keyframe_easing { clipId, time, easing, prop? }` (the key within a millisecond of `time`; with
`prop`, that one number's), `set_property_keyframes { clipId, prop, keys }` (one number's own keys; none =
static) / `set_property_tracks { clipId, tracks }` (several numbers' keys in one edit — the dope sheet) /
`copy_keyframes { fromClipId, toClipId, props?, offset? }`,
`set_reframe` / `clear_reframe` / `set_reframe_keyframes` / `add_reframe_keyframe`,
`set_asset_projection` (asset-level 360 mark; returns the `Asset`),
`add_overlay` / `update_overlay` / `remove_overlay` / `set_overlay_keyframes`,
`add_svg_overlay { source, start, end?, layout? }` (`source` is `{custom: svg}` or `{preset: {kind, params}}`) /
`update_svg_overlay { overlayId, patch }` (the patch's `preset_kind` switches a preset title's kind in one edit, keeping
its id and draw order) / `remove_svg_overlay { overlayId }` (the animated titles; each returns the
refreshed `Timeline`, and the new title is the one that was not there before), `validate_svg { svg }` → `SvgCheck`
(`check_svg`: the sanitised source, warnings, natural duration; needs no project, so no lock). **Reading an SVG takes
up to ~1.7 s, so these three are `async` + `blocking`**: the add and update parse (`SvgSource::check` /
`SvgOverlayPatch::check`) on the blocking pool *before* `lock_user`, and the lock only runs `add_svg_overlay_parsed` /
`update_svg_overlay_parsed`,
`list_svg_presets` → `SvgPresetInfo[]` and `svg_preset_preview { kind, params, duration? }` → the preset's SVG text
for the project's delivery frame (the page shows it through an `<img>` data URL, never inline — the sanitiser, not
the webview, decides what an SVG may do; `check_params` refuses empty text first),
**motion templates** (`kerf_core::template`; model in `core-project.md`, the MCP side in `mcp.md`): the library calls need no
project and never take the lock — `list_templates` → `TemplateList {templates, problems}` (manifests only),
`template_poster { templateId }` → a `data:image/png;base64,` URL or `null` (a grid card fetches its own),
`svg_element_ids { svg }` → `[{id, tag}]` (what a control can aim at; pure, and the text comes from the webview, so the core
refuses one over a title's 128 KiB before parsing it), `template_image_value { path }` → the `data:` URL
an `image` control takes (the file opened, checked through the handle, read at most one byte past `template::MAX_IMAGE_BYTES` —
94 KiB — then `template::image_data_url` judges the picture),
`import_template { path }` (a `.kerftemplate`; `validate_import_path`, then `TemplateLibrary::import` → `ImportOutcome`,
which never overwrites), `export_template { templateId, path }` (`require_user_output_path`, `check_output` against the
project's sources / proxies, Kerf's settings and logs **and the library directory**; resolves to where the file went),
`delete_template { templateId }`; all `async` on `blocking`. `save_template { overlayId, name, description?, controls }` reads
the title under `lock_user` (`Project::template_source`) and builds the file (fonts, sanitiser, poster: slow) with the lock
released → `TemplateSaved {info, warnings}`. **`apply_template { templateId, values, start, end?, layout? }`** (`layout` is the
`SvgLayout` of `add_svg_overlay`; `values` is control id → string, partial) reads the template and `prepare`s it (splice +
sanitiser) *before* `lock_user`, which then only runs `apply_template_parsed`; **`set_template_values { overlayId, values }`**
takes the title under the lock (`template_edit_inputs`), prepares with it released and stores with
`set_template_values_parsed`, which refuses a title that changed meanwhile ("try again"). Both resolve to
`TemplateEdit {timeline, overlay_id, warnings}` — the refreshed cut, the title made or changed, and what the sanitiser or the
machine could not honour. The library is app-level (not undone, not staged);
`generate_captions` / `clear_captions` (caption the whole cut, in timeline
time), `import_captions` / `import_captions_text` (a subtitle file by path / by text —
the text variant is what an `<input type=file>` or a paste uses; both return
`{timeline, summary}` rather than a bare `Timeline`), `export_srt`, `export_captions` (the cut's
captions as `.srt` / `.vtt`), `export_interchange` (OTIO / EDL, returns `{path, report}`),
`export_markers` (CSV / YouTube chapters, `{path, report}`) — each snapshots under the lock and
builds and writes the file on the blocking pool —, `refine_transcript` (re-time words + label speakers on the cached transcript via `run_refine`, shared
with MCP: lock released, background lane, `analysis-progress`, stopped by `cancel_analysis`; returns the
analysis), `remove_silence`, `snap_to_beats`,
`smart_crop` (frame each shot for the delivery frame),
`sync_clips_by_audio` (`referenceId`, `clipId`, `force?` → `{sync: AudioSync, timeline}`; heavy, the
lock-free inputs → measure → apply split; a slot wait emits `audio-sync-progress`; a forced call for the
plan last measured applies `AppState.last_sync` instead of decoding again — "Move anyway") /
`cancel_audio_sync` (rejects it with `audio sync cancelled`),
`extract_audio` (detaches an asset's cut clips and reports what it skipped, see Linked A/V) / `add_asset_audio`
(an asset's whole audio as a clip) / `detach_audio_clips`,
`detach_audio` / `reattach_audio` / `reattach_audio_clips` / `link_clips` / `unlink_clips`, `concatenate` — each
returns the refreshed `Timeline`; every edit that carries linked clips takes an optional
`link` (`false` edits the named clips alone: `trim_clip`, `move_clip(s)`, `split_clip`,
`remove_clip(s)`, `ripple_delete`, `cut_clip_range`, `set_speed`, `roll_edit`, `slip_clip`,
`slide_clip`, `split_remove(_clips)`, via `with_links`)), media (`get_frame` → base64 PNG data URL,
`get_waveform_range` → a source-seconds window as min/max peaks per channel,
`get_filmstrip` → an asset's thumbnail strip, the `Filmstrip` JSON with each sheet's
JPEG added as a base64 `data:` URL (`FilmstripPayload` — the CSP admits `data:` images
and no `blob:`; core serializes the geometry without pixels) and **no MCP tool**, since
`skim_asset` is how an agent looks at footage,
`get_preview_frame` (`hint: "forward"` is a frame of GPU playback, `draft` a gesture's frame) / `set_preview_bounds` / `gpu_preview_status` (the GPU preview, below; GUI-only; its tallies are kept for tests and logs, no command serves them),
`get_timeline_frame` (the composited still at a time; see Scrubbed frames below),
`get_scopes` (`kerf_core::scopes` of the export's still at a time, from the originals: the summary and the grids asked
for; the MCP tool of the same name) / `get_render_bar` (`kerf_core::render_bar` over the working cut, for the GPU
preview's `Compositor::CAPS` when the setting is on, the proxy waits and the measured colour policy; GUI-only, see
`frontend-timeline.md`),
detached panels (`popout_expect` / `popout_cancel` / `popout_focus` / `popout_move` / `close_popout`, see Detached panels below; GUI-only, no MCP tool),
`start_playback { playback_id, start, fps?, max_width?, on_frame }` / `stop_playback` — streamed
composited frames over a `tauri::ipc::Channel<InvokeResponseBody>`, **raw bytes** (`playback_message`:
the frame's timeline time as a little-endian `f64`, then the JPEG undecoded; the page parses it with
`frame-transport.ts`, and both ends hold the same fixed vector in a test) — not base64 in JSON: a quarter
fewer bytes through the channel and no base64 / JSON at this end. It is not a faster webview decode (headless
Chromium measured none, `frontend.md`): the page builds the data URL itself, for the frames it shows. Tauri 2.12
sends a raw payload over 1 KiB through its `fetch` channel command and a smaller one
inline; the page receives an `ArrayBuffer` either way, in order. `fps` / `max_width` left out mean the
cut's own rate and the default width (`kerf_core::playback_params`); the preview always states the width and
the rate only when restarting a stream that could not keep up. The "playback stream ended" log line carries `fps`,
`max_width`, `late_frames` (more than two frame intervals behind the schedule) and `worst_late_ms`,
the numbers to read from a user's log when playback is reported choppy. Cancelled **by caller-supplied
id** rather than a generation counter, because start and stop are separate async calls that can arrive
out of order and a late stop must not kill the stream that replaced it —
`get_audio` → a clip window as **raw mono s16le PCM via `tauri::ipc::Response`**, the
only non-JSON command — the preview's Web Audio playback decodes it),
delivery (`export_cover` → a cover image at the full delivery frame,
`platform_targets` / `platform_check` (frame, range → a `PlatformReport`: the checks and the cut they judged) → the platform check, `reveal_path` →
show a rendered file in the OS file manager, opening its *containing folder*
rather than the file, since "show me where it went" is not a request to launch a
player), the
agent task queue (`list_tasks`, `add_task` → the new `Task`; `resolve_task` /
`remove_task` → the refreshed `Task[]`), the agent's staged proposal
(`get_staged_edit` → the `StagedEdit` *with its diff*, so the review card renders
from one round-trip; `get_staged_timeline` for previewing it; `apply_staged_edit` /
`discard_staged_edit`) and `revision_diff`, `export_timeline` (emits
`export-progress` events) / `cancel_export`, `cancel_analysis` (the same shape,
for the analysis pass — importing ten clips must not be an unbreakable
commitment to ten transcriptions), `cancel_levels` (the same again, for the Mixer's
measurement: a flag on `AppState` reset when `get_levels` starts and polled as its cancel
callback — the pass holds the process-wide `cpu::lease`, so it cannot be left
unstoppable), app preferences (`get_settings` /
`set_settings` → a `SettingsView`: the *effective* CPU budget, the
automatic-analysis set (`auto_analysis`: a master and a flag per kind — the old `transcribe` flag in a
settings file migrates into its `transcript` flag on load, `parse` / `migrate`, and is never written back),
`proxy_size` (0 / 720 / 1080 / 1280) and `preview_source` (`auto` / `original` / `proxy_only`)
read back out of the engine, the cores it works out to, and the machine it is a share of —
`settings.rs` persists them as JSON in the platform config dir, since how much
of *this* computer Kerf may use is not something that should travel inside a
`.kerf` file; `KERF_CPU_PERCENT` wins at launch, a moved slider wins after.
The file also carries the **workspaces** (which one is active, each one's
dock arrangement, the library rail's tab and folded state), the **color theme**,
the **keybindings** the user changed, the **editing defaults** (`editing`: `{snap, still_seconds, title_seconds}`, read and clamped by
`editing-prefs.ts`; a reset puts them back) and `layout` — the single arrangement from
before there were workspaces, now only migrated from — as opaque
`serde_json::Value`s: the frontend owns their shape and
validates them on the way back in, so `get_settings` re-reads the file for those
where the engine-held values are read live). **`set_settings` takes a patch**,
not the whole object — only the fields that changed (`{workspaces}`, `{theme}`,
`{keybindings}`, `{cpu_percent}`), merged into the file under a mutex, and only those fields are
pushed into the engine (so a layout write never re-applies the stored CPU share
over a `KERF_CPU_PERCENT` override). The write is atomic (temp file in the same
directory, fsync, rename over) and **happens before the patch is put into force**
(a write that fails is an error and changes nothing). A file that cannot be read, is not UTF-8,
is not JSON or is not an object is moved aside to `settings.corrupt-<unix-ms>.json` before defaults
load, so the next save cannot destroy an imported theme; a file that parses is read **field by field**
(`parse`): a value that does not fit its field (an unknown enum name, a number out of range, the wrong
JSON type) is logged and leaves only that field at its default — **and the file keeps what it held**: that raw
value (a newer build's enum name, say) and any key this build has never heard of are carried in `Settings::kept` and
written back unchanged by every save until the user changes that setting, so opening the file in an older build and saving
something unrelated costs nothing. A key that is a field of `Settings` must be in `STORED_KEYS` or `parse` files it under
`kept` and the setting is lost on restart (this once happened to `editing`); a test checks every key of the serialized
defaults is listed, and another round-trips every field through `save_to` / `load_from`. At most five `settings.corrupt-*` and five `settings.reset-*` copies are kept (the newest;
`prune_backups`). The recent list is statted with the settings lock **released** (`recent_in`: read, stat, then lock and
drop the ones the stat said were gone), so an offline share never holds up another settings write. The file also carries `time_display`
(`timecode` | `seconds`), `reopen_last_project` and `recent_projects` (newest first, at most 10; kept by
the app — `open_project`, `open_recent` and `save_project_as` push the path, `recent_projects` drops the
files that are gone, `clear_recent_projects` empties it — and not writable by a patch) and `last_open_project` (see the
launch paragraph; also app-kept, not in `SettingsView`).
`reset_settings` puts every preference back to its default and into force, keeping the old file as
`settings.reset-<unix-ms>.json` and the recent list and last open project (`settings::reset_keep` is the one place to add
what a reset must not lose); `reveal_settings_file` shows the file in the OS file manager), `read_text_file` / `write_text_file`
(a theme file the user picked, imported or exported — the only commands that
read a caller-chosen path, so both take only `.json` paths, refuse a non-regular file, and cap read and write at 1 MiB)
and `agent_status` (the MCP endpoint, an `error` when the server could not bind — the agent panel then says the port is taken instead of showing a dead endpoint — plus how
many seconds ago an agent last spoke to it, or `null` if none ever has —
`mcp::LAST_AGENT_ACTIVITY`, stamped in `lock_agent` and in `get_info`, since
`initialize` is the one moment an agent is known to be there; a
streamable-HTTP client holds no connection between calls, so there is no socket
to report and the panel judges from the age instead of the green dot it used to
show unconditionally). `mcp_connection_info` (URL, token, `token_problem` — why the token is not what it should be, or `null` —, `running` / `error`
**for the address now configured** (`listener_state`: until the supervisor has restarted for a new address, the old one's
state says nothing about it, so `running` is false and `error` empty, which is what lets the page wait for the restart),
`claude_code_command` with the bearer header and a generic `mcpServers` JSON snippet; `mcp_commands.rs`), `regenerate_mcp_token` (the old token stops working at once)
and `set_mcp_settings { mcp_enabled?, mcp_addr?, mcp_require_token?, agent_review? }` (refuses any other key; a network
address needs the token; `settings::update` restarts the listener when the address or switch moves, see `mcp.md`) are the Agent
settings page's commands — the settings file holds `agent_review`, `mcp_enabled`, `mcp_addr`, `mcp_require_token` and
`mcp_token`, and `SettingsView` carries all but the token. A **failed or cancelled render deletes what it wrote**, and the engine does it
(`kerf_core::discard_partial`, inside `render_with_progress` and so inside `render_variants`; the MCP `export` repeats it
so a cancel lands before the handler's future can be dropped): only if this run touched the file
(mtime differs from before), so a failure or a cancel before ffmpeg opened the output cannot delete the
earlier export sitting at that path, and the commands neither remove nor compare anything themselves.
`export_variants` is one `render_variants` call over every file: the one in flight is removed, the finished
ones stay, and its `VariantProgress` is the `export-progress` event as it is. With `smart_crop` the shots are
framed first (`sample_framings_with`, `export_cancel` stops it), streaming the same event with `waiting:
"framing the shots"` so the bar does not sit at 0%. Every output path is checked against the project's own
files (`Project::output_path_inputs` read under the lock, `check_output_path_with` — which canonicalises and stats — run
after it is released and before the lock-free render: a source file, a capture, a proxy, the project database) in `export_timeline`, `export_variants`, `export_cover` and `export_srt`, and
`require_local_output_path` — what an **agent** may name — refuses UNC / network paths (`\\host\share`, the mixed
forms and `\\?\UNC\…`, and `//host/share` **on Windows only**: on macOS and Linux `//tmp/x` is `/tmp/x`; only the
verbatim local-drive spelling `\\?\C:\…` passes). A path the user picked in Kerf's own save dialog goes through
`require_user_output_path`, which lets a share they chose through on Windows (`PathSource::User`); URLs, protocol sinks,
relative paths and the device namespace (`\\.\pipe\…`) are refused for both. A file an agent asks Kerf to **read**
(`import_asset`, a caption file's `path`) goes through `require_local_input_path` first — the same UNC / device
refusal (`network_path`), before any `stat`, since opening a share makes the machine authenticate to its host. `check_output` adds Kerf's own config
dir (the settings, which hold the agent token, and their backups) and the logs to what the project protects. The error carries
ffmpeg's stderr tail. `start_playback` resolves `Ok` for a stop or supersede but rejects with the
ffmpeg error when a stream someone is still watching dies, so the preview can say
why it went black. It calls `kerf_core::stream_preview_until` with "superseded or stopped" as the
cancel, so a stop or a seek also ends the drawing of an animated title's pictures that comes before the
first frame (`engine-cli.md`), not only a stream that has started. **Logging** (`init_logging`): stdout plus a daily-rolling `kerf.<date>.log` (14 kept) in
`<app data dir>/logs` — `log_dir_path` is the one place that is decided, shared by
`init_logging` and `reveal_logs`; if it is not writable the app logs to stdout
only. **The file layer is synchronous**: the `RollingFileAppender` is the layer's writer directly (`file_layer`), one `write` per event, not behind `tracing_appender::non_blocking`. That queue's worker thread is what a hard crash takes the last lines with — an aborting panic, `process::exit`, or a segfault in FFmpeg / ONNX Runtime never reaches a `WorkerGuard`'s `Drop`, a flush in the panic hook would not cover them, and dropping the guard there would silence logging for the rest of a session after a panic on a thread that does not end the process. Logging is a few dozen lines a session, none in a hot loop, so a syscall each is free. Two Rust tests hold it: `a_line_is_on_disk_the_moment_logging_returns` logs and reads straight back 40 times (a queue-backed writer fails it every time), and a child process that logs a burst and `process::exit(1)`s (no destructors, so anything *buffered* is lost — `abort()` would be more literal but raises apport / WER / ReportCrash; `exit` is not enough to catch a queue, which drains during it). `log_panic` / `panic_summary` are what the panic hook writes. `RunEvent::Exit` logs `kerf exiting`, and `installUpdate` logs a line first, but neither makes the end of a log conclusive: the Windows updater install calls `process::exit` past `RunEvent::Exit`, and End Task / SIGTERM skip it — a log that ends without `kerf exiting` *may* have crashed, and one that ends in an update line did not. The startup line carries version, OS/arch, the ffmpeg/ffprobe in use and both
directories (never env dumps or args). Failures reach the file from three places:
Tauri commands return plain `String` errors that Tauri offers no hook to observe, so the
single `invoke` wrapper in `api.ts` forwards every rejection (command name + message,
`info` for a cancellation) to the **`log_frontend`** command, which also takes error /
warning toasts (`notifications.svelte.ts`) and `window.onerror` / `unhandledrejection`
(`log.ts`, installed in `+layout.svelte`); it writes with target `webview`, caps a message
at 8 KiB and admits 30 lines per second. MCP tool errors are logged once, in the
`call_tool` override beside `#[tool_handler]` (target `mcp`: `warn` for invalid_params,
`error` otherwise), whichever helper built the error. All of it is a no-op in the browser
harness.
**No command runs on the main thread** (a plain sync
command would freeze the window in Tauri v2): quick ops are
`#[tauri::command(async)]`, and every heavy one (ffmpeg decode / analysis /
export, disk-bound open/save) is an `async fn` that pushes its work onto the
blocking pool via the `blocking()` helper — resolving inputs under the shared
project lock and **releasing it before the slow part** (see `lock_user`; the
lock-free `Project::decode_*` statics exist for exactly this). The MCP server's
heavy tools (`analyze_asset`, `get_frame`, `skim_asset`, `preview_timeline`,
`get_waveform`/`get_energy`/`get_waveform_range`, `export`) follow the same shape
with `lock_agent`.
Tauri auto-converts JS camelCase args to Rust
snake_case (`{ assetId }` → `asset_id`). Config: `tauri.conf.json` points
`frontendDist` at `../../frontend/build` (resolved relative to the config file). The
`beforeDevCommand`/`beforeBuildCommand` hooks, however, run from Tauri's *app dir* —
which for this `crates/kerf-app` layout resolves to `crates/`, not the config dir or repo
root — so they anchor to the repo via `cd "$(git rev-parse --show-toplevel)/frontend" && bun run dev`
instead of a fragile relative path.
`build.rs` takes the **Windows app manifest** away from Tauri
(`new_without_app_manifest`) and embeds `windows-app-manifest.xml` through the
linker instead: Tauri's copy rides in the `.res`, which cargo links into *bins*
only, so the lib's test binary ran with no activation context, bound comctl32
**v5**, and died with `STATUS_ENTRYPOINT_NOT_FOUND` on the `TaskDialogIndirect`
import rfd (via `tauri-plugin-dialog`) contributes — before a single test ran.
Whether the linker pulls that object in at all shifts with unrelated dependency
bumps, which is how an rmcp upgrade broke `cargo test -p kerf-app` on Windows.
`capabilities/default.json` grants `core:default` + `dialog:default` +
`updater:default` + `process:allow-restart` + `core:window:allow-destroy` (the
unsaved-project close guard holds the window open, then destroys it once the user
confirms) + `core:window:allow-set-title` (the OS title follows the project's name and
whether it has unsaved work: `window-title.ts`, `setWindowTitle` in `api.ts`) + `opener:allow-open-url`. That last
one enables the command **with no scope of its own** (`allow-default-urls` is a
separate permission), so it is listed in object form with an `allow` entry for
`https://github.com/OrellBuehler/kerf/*` — without a scope every `openUrl` call
comes back `ForbiddenUrl` and the "Release notes" button silently does nothing.

**Command index** (every name in `generate_handler!`; the prose above says what they do):
*project* `project_path`, `new_project`, `open_project`, `save_project_as`, `recent_projects`, `open_recent`,
`clear_recent_projects`, `take_launch_project`, `show_main_window`, `list_recoveries`, `recover_project`, `discard_recovery`,
`discard_recovery_at_exit`; *library* `list_assets`, `get_asset_metadata`,
`list_fonts`, `import_asset`, `cancel_import`, `remove_asset`, `relink_asset`, `check_relink`, `missing_assets`, `paths_exist`,
`set_asset_projection`; *analysis, proxies, transcription* `analyze_asset`, `cancel_analysis`, `analysis_statuses`,
`proxy_statuses`, `rebuild_proxy`, `delete_proxy`, `transcription_status`, `set_speech_model`, `download_speech_model`,
`cancel_model_download`; *voiceover, music, stems, mixing* `voiceover_status`, `prepare_voiceover`, `generate_voiceover`,
`cancel_voiceover`, `music_status`, `prepare_music`, `compose_music`, `cancel_music`, `get_band_balance`,
`cancel_band_balance`, `stems_status`, `separate_stems`, `cancel_stems`, `get_levels`, `cancel_levels`, `plan_music_fit`,
`fit_music`, `set_track_volume`, `set_track_pan`, `set_master_volume`, `set_master_limiter`, `set_master_true_peak`,
`set_master_duck`, `sync_clips_by_audio`, `cancel_audio_sync`;
*timeline* `get_timeline`, `get_ripple_mode`, `set_ripple_mode`, `cut_clip`, `add_clip`, `split_clip`, `trim_clip`,
`reorder_clip`, `move_clip`, `move_clips`, `ripple_delete`, `cut_clip_range`, `roll_edit`, `slip_clip`, `slide_clip`,
`split_remove`, `split_remove_clips`, `insert_clips`, `duplicate_clips`, `remove_clip`, `remove_clips`, `add_track`,
`remove_track`, `set_track_duck`, `set_track_muted`, `set_track_solo`, `set_track_locked`, `set_clip_enabled`,
`set_delivery_format`, `concatenate`, `remove_silence`, `snap_to_beats`, `smart_crop`, `cancel_smart_crop`;
*linked A/V* `extract_audio`, `add_asset_audio`, `detach_audio`, `detach_audio_clips`, `reattach_audio`,
`reattach_audio_clips`, `link_clips`, `unlink_clips`; *clip properties* `set_volume`, `set_fade`, `set_speed`,
`set_transform`, `set_color`, `set_grade`, `inspect_lut`, `set_transition`, `set_mask`, `set_video_effects`, `set_audio_effects`, `set_keyframes`,
`add_keyframe`, `set_keyframe_easing`, `set_property_keyframes`, `set_property_tracks`, `copy_keyframes`, `clear_keyframes`, `set_reframe`,
`clear_reframe`, `set_reframe_keyframes`, `add_reframe_keyframe`; *markers, text, titles, captions* `add_marker`,
`update_marker`, `remove_marker`, `add_overlay`, `update_overlay`, `remove_overlay`, `set_overlay_keyframes`,
`add_svg_overlay`, `update_svg_overlay`, `remove_svg_overlay`, `validate_svg`, `list_svg_presets`,
`svg_preset_preview`, `list_templates`, `template_poster`, `svg_element_ids`, `template_image_value`, `save_template`,
`apply_template`, `set_template_values`, `import_template`, `export_template`, `delete_template`, `generate_captions`, `clear_captions`, `import_captions`, `import_captions_text`, `export_srt`, `export_captions`, `export_interchange`, `export_markers`,
`refine_transcript`;
*history and review* `get_history`, `revision_diff`, `undo`, `redo`, `revert_to`, `get_staged_edit`,
`get_staged_timeline`, `apply_staged_edit`, `apply_staged_subset`, `discard_staged_edit`, `undo_staged_edit`,
`discarded_staged_edit`, `restore_discarded_edits`, `list_tasks`, `add_task`, `resolve_task`, `remove_task`;
*preview and media* `get_frame`, `get_timeline_frame`, `get_preview_frame`, `set_preview_bounds`, `gpu_preview_status`,
`playback_session`, `start_playback`, `stop_playback`, `get_waveform_range`,
`get_filmstrip`, `get_audio`, `get_energy`, `get_scopes`, `get_render_bar`; *delivery* `hw_encoders`, `export_timeline`, `export_variants`,
`cancel_export`, `export_cover`, `cancel_cover_export`, `platform_targets`, `platform_check`, `reveal_path`;
*agent and app* `agent_status`, `report_ui_context`, `mcp_connection_info`, `regenerate_mcp_token`, `set_mcp_settings`, `get_settings`, `set_settings`, `reset_settings`, `reveal_settings_file`,
`read_text_file`, `write_text_file`, `reveal_logs`, `log_frontend`; *detached panels* `popout_expect`,
`popout_cancel`, `close_popout`, `popout_focus`, `popout_move`.
**Events**: `project-changed`, `analysis-progress`, `analysis-status`, `proxy-progress`, `proxy-ready`,
`export-progress`, `import-progress`, `audio-sync-progress`, `model-progress`, `smart-crop-progress`, `voiceover-progress`, `music-progress`, `stems-progress`,
`gpu-preview-checked`, `popout-closed`, `open-project-file`, `launch-project-missing`; from the MCP server also
`project-replaced` (another project is open: reload everything), `settings-changed` (**payload: the full
`SettingsView`**, as `get_settings` answers it, after an agent's `set_settings`), `ripple-mode-changed`,
`speech-model-changed` and `templates-changed` (an agent saved, imported or deleted a template: refetch `list_templates`). `platform_targets` is wired: the
Deliver panel's verdict (`platform-verdict.ts`) takes the targets from it (`platformTargets()`, asked once) for the size a
soft frame is made for; the browser harness answers with the mirror in `platforms.ts`.

**Editor context (`editor_context.rs`).** `report_ui_context` takes what the editor shows —
playhead, selected clip ids, in/out marks, the visible timeline range (`ui.timelineView`,
written by the timeline), the focused dock panel (`ui.focusedPanel`, dockview's active panel)
and the selected asset — into `EditorContextState`, its own `Mutex` in app state and never the
`Project` (a view of the window, not the cut). `editor-context.ts` throttles it to the latest
state once per 300 ms, only when it changed, and re-sends it every 30 s as a heartbeat; the MCP
`get_editor_context` reads it back with `age_secs`, and `stale` once a report is over 90 s old.

**Crash recovery** (`recovery.rs`): the untitled project is never only in memory. `setup` resolves
`<app data>/recovery` (the debug identity's own data dir, so a dev run never touches an installed Kerf's
sessions) and swaps the launch's empty project for `Project::create_recovery` there — in `setup`, not at
the top of `run()`, because a second launch is handed to the first by the single-instance plugin before
`setup` and must not make a session. `new_project` (now `async` + `blocking`) makes a fresh session the
same way (`recovery::untitled`; no data dir or no file locking falls back to in-memory). Replacing the
project (`replace_with`: Open, New, Save As, Recover) deletes the old session — each command
`close()`s the returned project after dropping the lock. A clean exit (`RunEvent::Exit`) uses
`close_at_exit`: an untouched session goes, one holding work stays to be offered next launch (the close
guard can be bypassed, and `hasUnsavedWork` is narrower than recovery's rule); a crash leaves it.
An explicit **Don't save** in the close / quit prompt calls `discard_recovery_at_exit` (`leaveAfter`
in `confirm-copy.ts`), which records the current session's id; at exit that session is deleted even
with work (`close_on_exit`, matched by id, so a project replaced since is not). A prompt that failed
or a close that asked nothing never sets it.
`list_recoveries` scans once per run (`kerf_core::recovery::scan`, skipping our own session) and keeps
what it found **locked** in `RecoveryState` until `recover_project { id }` (opens it as the untitled
project, like `open_project`: scrub cleared, speech model restored, proxies queued) or
`discard_recovery { id }` (deletes it); one left alone is offered again next launch. No MCP tools —
recovery is the user's call at launch. `project_path` stays `null` for a recovery-backed project, so
the GUI still treats it as unsaved.

**Scrubbed frames.** `get_timeline_frame` and the GPU preview's JPEG route go through `AppState.scrub`, one
`kerf_core::ScrubFrames` per process: the frame of the cut's grid that holds the time, from a bounded cache or from
the window of neighbours one process draws for it (`engine-cli.md`, "Scrubbed frames"). `start_playback` stops the
window (`scrub.stop()`), `new_project` / `open_project` clear it (`scrub.clear()`). The MCP `preview_timeline`
stays the one-shot still on purpose: an agent asks for exactly that time and the staged cut. Environment:
`KERF_SCRUB_CACHE_MB` (64), `KERF_SCRUB_WINDOW` (frames per window, 24; `off` / `0` = one cached still per miss);
`KERF_GPU_LANE=off` keeps every job on the cores' slot (`engine-cli.md`, `cpu.rs`).

**GPU preview (A2, `gpu_preview.rs`, opt-in).** *Settings › Preview › GPU preview (experimental)*
(`Settings.gpu_preview`, default off, persisted beside `safe_areas`) draws the Preview
panel's frame with `kerf-gpu`'s compositor in a native surface. Off, none of it runs and the
webview sends no extra command: the JPEG path is what it was. On, `get_preview_frame
{ time_secs, max_width, overlays, client?, seq? }` plans the frame from a **snapshot of the plan's inputs**
(`gpu_preview/snapshot.rs`: the working timeline, the assets as imported, the proxy-swapped ones the
FFmpeg graph reads and every asset's proxy input, in an `Arc`), plans it (`Planner` over `ProxyMedia`, the
measured colour policy, at the start of the output frame whose slot holds the time, `plan_at_grid`, as the JPEG route's
`ScrubFrames` counts it, so the two show one frame between two frames) and answers `{renderer: 'gpu' | 'ffmpeg' | 'superseded', frame?, reasons,
timings?}`: the GPU when `RenderPlan::reasons(caps, size)` is empty and everything works,
else FFmpeg's JPEG **of that frame, in the same call**, with the plan's reasons (a refusal
is not an error). Titles and captions are part of what the compositor draws (`caps.text`: FFmpeg rasterises each distinct
overlay once and the GPU blends it, `gpu.md`); a frame is refused for text only under 12 px or in a named font with no bold face. A paused frame inside a fade, dip, dissolve, slide or push is drawn too (a still plan holds the running fades, the travel and the outgoing clip's tail: `caps.fades` / `transitions` / `dip_white`, `gpu.md`; refused: a black or white fade on a layer below full opacity, a dip to white on an alpha plane, a letterboxed, odd or resized layer or a picture that is not 8-bit `yuv420p`, and a full-range picture inside a fade under FFmpeg 9). A moving zoom, masks and the blur / sharpen / grayscale / invert effects are drawn as well (`caps.keyed_zoom` / `mask` / `effects`, `gpu.md`; refused: a vignette, a chroma key, a rotation, an opacity below 1 or a dip to white in front of a moving zoom, a mask on a rotated or translucent layer, a blur or a sharpen on a letterbox, an invert / blur / sharpen on a full-range picture, an invert on a gray one, a sharpen after a scale or a blur that carries its difference past two levels: one above 0.5, or a second). The device, compositor, `FrameSource` and presenter are built lazily by
the first frame that wants them and dropped together (`Backend`); a build failure is
remembered with a backoff (5 s doubling to 5 min, `Backoff`), a `DeviceLost` or any error
that says the device or surface is gone (`react`) drops the backend and rebuilds it on the
next use — the first loss at once, as `kerf-gpu` documents: the owner builds a new `Gpu` —
and a refused plan, a busy decode or an occluded window is only that frame's fallback. **A frame that was
presented asks the frame source for the cuts around the playhead** (`prefetch_ahead`, `cut_moments`): the clips of video
tracks that render and **start** within 2 s ahead of it (`upcoming_clip_starts`, planned at their start) and the ones that
**ended** within 2 s behind it (`passed_clip_ends`, planned at their last frame, half a frame before the end), nearest first,
at most 3 in all, each moment once, so a drag in either direction finds a run waiting: their layers are
handed to `FrameSource::frames(.., Hint::Prefetch)` on a
thread of its own (one at a time, the same moments not again within a second, after the render lock is
released; a moment the GPU would refuse at this size is skipped, and a panic clears the in-flight flag), so a run is waiting and the file's two probes are done when the playhead crosses the cut
instead of the first frame there costing a process start. It never takes a run another layer is reading
and a failure is nothing: the frame is decoded when asked. **Nothing
here may freeze the preview**: a backend is built **off the render lock** and with a deadline
(`BUILD_DEADLINE`, 10 s, on a thread of its own; a frame that finds a build under way is the
JPEG at once, "the GPU preview is starting", and one that outlives the deadline is left to finish
and dropped), a masked plan has FFmpeg's `geq` measured once by `kerf_gpu::prepare`, on the frame's own blocking thread before `ensure_backend` (off the render lock and outside the build deadline, bounded by the probe's own `PROBE_TIMEOUT`, 10 s; a failed probe is FFmpeg's JPEG for that frame and retried after a backoff), a panic anywhere in the GPU path (`frame()`'s `catch_unwind`, and the build thread's)
drops the backend behind the backoff, hides the surface and answers with FFmpeg's JPEG — and the
page treats a *rejected* `get_preview_frame` the same way (the JPEG for that frame, the surface
hidden). The setting flips **synchronously** (`set_enabled` returns whether a teardown is due; only
`teardown_if_off`, which may wait for a frame in flight, runs on a thread), and a frame in flight
looks at it again before it builds and before it shows anything; turning it on is a retry (the
backoff is forgotten). `Backoff` is forgiven only by a run of 20 frames that reached the screen,
not by one, so a device that is lost after every present waits longer each time instead of being
rebuilt every other frame. **A hard driver crash does not repeat on every launch**: a marker file
(`gpu-preview-attempt`, in the config directory) is written before a backend is built and removed
once a frame has reached the screen; a launch that finds it turns the setting off before reading
it (`take_crash_marker_in`) and says so in the Settings status line. On Windows the webview is
made transparent as the **last** step of a build and restored by a guard's `Drop`, so a build that
fails after it cannot leave the page transparent over nothing.
`set_preview_bounds` (GUI-only) is how the page says where the Preview frame is: **device
pixels relative to the webview, plus the webview's size** (the backend maps the rectangle
onto the surface when the two differ by a rounding), a `visible` flag, and the two colours
painted around the picture (`--frame-matte` inside the frame, `--surface-app` beyond it), and
a `seq` that counts up across every Preview the page has had (a panel replaced by a workspace
switch can send its last report after the new one's first; the backend ignores a report older
than the newest). It never waits for a *render*, but it is **serialized with a picture being
shown**: `GpuPreview::set_bounds` and the present of an in-flight frame (`under_bounds`) take
the same lock, and the frame looks at the bounds again under it. A hide that lands while a
frame is being drawn therefore sticks (the frame is dropped to the JPEG; before, it re-mapped
the child window the page had just had hidden, over playback or a dialog), and a frame the page
moved meanwhile is shown at the frame's new place (`layout_for` again, the picture scaled into
it); a shown child window also **follows** a move or resize at once rather than waiting for the
next picture (`Child::follow`), and is re-raised with every present (`Child::place`: GTK makes
native windows of its own and one made later would otherwise end up above it). The
render is at about the size it is shown (`place_frame`: the width nearest the panel's, at
most 1920, whose size keeps the canvas's shape to the row — `still_size` at 430 px is 240
rows for a 241.9 ideal, which letterboxed the footage with a 2 px pillar), and the presenter
draws it letterboxed in the frame (`object-fit: contain`). `gpu_preview_status` says what
this machine does: the platform's technique, whether a device is up, the adapter and
whether it is software, the last failure. **No MCP tool**: the agent has `preview_timeline`,
and nothing here is an edit. **No capability permission and no CSP change**: these are the
app's own commands (the capability file gates plugins and core APIs; `build.rs` registers no
app manifest), and the GPU frame never reaches the webview, so no `data:` / `blob:` image
is added. `tauri.conf.json` is **unchanged** — no `transparent`: the window still starts
hidden with `backgroundColor`, the reveal (`reveal.ts`, the Rust failsafe) is exactly as
before, and the one transparency there is (technique `window`, below) is applied at runtime
after the user turned the setting on and a frame asked for it, and undone with the backend.

**Scrubbing and hardening (WP-5).** *Snapshot*: a frame's inputs used to be the project lock, the working timeline
parsed from its JSON, every asset read and the disk asked for each one's proxy — per frame of a drag. They are an
`Arc<PlanInputs>` held between frames (`PlanSnapshots`) and fresh while `kerf_core::plan_epoch()` is the number it was
taken at (`core-render-plan.md`: every project write, a project opened, a proxy landing or deleted, a proxy-size or preview-source
change) and for under 2 s; a stale one is taken again from `lock_user` (`Taken::from` reads the epoch first, under the
lock; `Taken::resolve` stats the proxies after it is gone). It is `working_timeline()` as the GUI sees it (the user's view:
a staged agent edit is in it once accepted, which writes the timeline). The Proxy-only refusal is answered from it
(`PlanInputs::proxy_wait`, the status read from the proxy queue, not the project). Measured (release, `docs/review-2026-10-10/after/gpu-5`):
the inputs of a frame were 45 / 215 / 735 µs for 8 / 120 / 480 clips and are 0.1 to 0.3 µs from the snapshot (a stale one is rebuilt in the
same time the old path took: 40 / 200 / 690 µs). *Latest wins*: `get_preview_frame` posts
a request into a `Mailbox`; the one waiting is replaced and answers `renderer: "superseded"` at once, the frame in hand
is finished and the newest waiting request drawn next, by a worker on the blocking pool that exists while there is work
(a worker that dies, or a job the executor drops without running it, tells what it never answered: an error and not a skipped
frame, and the next request starts a worker). "Newest" is the page's order, not the order of arrival: Tauri runs each
`get_preview_frame` as a task of its own, so two can arrive reversed. A request carries `client` (the pump's name for its
life) and `seq` (its running number, the pump's `asked`); one older than the newest seen from that client answers `superseded`
at once and starts nothing, a request with neither goes by arrival. The page's GPU route is therefore not
single-flight (`frame-pump.svelte.ts`: a request per wanted time, an answer older than the one on show is dropped, a
`superseded` one ignored); the JPEG route is as it was. *The render lock is not held across the decode*: a frame takes
what it draws with (`Drawing`: the compositor, frame source, device and host as `Arc`s) under the lock, lets go, decodes and
composites, and takes the lock again to show the result if the backend is still the one that drew it (the frame source is
its identity; a restarted one is a fallback frame, and a failure of the old one does not take the new one down), so
turning the setting off, a lost device or a build no longer waits up to a decode's timeout for a frame. *Present mode*:
`Pace` (`pace.rs`) calls requests under 120 ms apart a drag and two gaps of 600 ms or more in a row the end of one;
frames of a request that arrives in a drag are presented with `Presenter::skip_vsync(true)` (`AutoNoVsync`) where the surface
has `Immediate` or `Mailbox`. The mode is decided **by a request, not by a timer**: nothing runs when a drag stops, so
the last frame of a drag is presented as the drag's were (without vsync), and the surface goes back to vsync only with the
request that makes a second lull in a row (a request 600 ms or more after the one before, twice: the second click after a
drag). The mode changes just before the frame that is presented into the reconfigured surface. *Self-test* (`check.rs`): the first frame the GPU would draw starts `kerf_gpu::parity_self_test`
(`gpu.md`) on a thread of its own and is FFmpeg's, as is every frame until the verdict (`gpu-preview-checked` is
emitted then, and the page asks for the frame again). Passed opens the GPU path for the session. A **mismatch** and a check
that **could not be made** both close it, and **turning the setting off and on runs the check again** for either (a driver, an
FFmpeg or a machine under load may not be what it was; a mismatch that is real is found again for a couple of seconds of
work); every verdict is logged (`info` for a pass, `warn` for a failure). The reason is the Settings status line and every
frame's reason, and it stays there when a backend is rebuilt on the same adapter (`Check::closed`). A verdict belongs to an
adapter and an FFmpeg (`AdapterKey`, made once per backend): a backend rebuilt on the same device keeps it, another adapter is
checked again. The gate is deliberately no looser than the parity contract: the limits are the harness's own. *Counters*
(`diagnostics.rs`): every frame that went through FFmpeg counts its reasons by kind (`Unsupported::kind()` for the plan's;
`self_test_pending`, `overlays`, `hidden`, `backend`, `decode_busy`, … for the rest) with the first message as an example,
logged once per new kind (`info`) and as a one-line session summary at teardown; the diagnostics (read by tests; no command serves them any more) are
`{tally: {gpu_frames, ffmpeg_frames, superseded, refusals[]}, snapshots: {served, taken}, requests, self_test, adapter,
software}`. Local and in memory; no MCP tool (nothing here is an edit). `gpu_preview/drive_tests.rs` drives the whole frame path
without a window (a real device, compositor and frame source over real clips, a screen that records; `#[ignore]`d), and
`bench_tests.rs` times it (`KERF_BENCH=1`).

**Drag frames and forward scrubs (A3, `gpu_preview/draft.rs`, `pace.rs`).** `get_preview_frame { …, draft }` is the frame of a
gesture under way: `draft` (`PreviewDraft { clips: [{track_id, clip}], overlays }`) is what the gesture would write on release, each
clip and title whole, and the frame is planned from the snapshot's timeline with those in place of its own (`PreviewDraft::apply`).
A draft changes only what a drafted gesture writes, each change through its edit's own checks, so the preview never shows a state
the edit would refuse: a clip's transform / colour / fades (`Transform::check_values` + `check_crop`, `Color::check_values`,
`Clip::check_fades`, kerf-core's `model/values.rs`, which `set_transform` / `set_color` / `set_fade` call too), its mask
(`Mask::normalized`, as `set_mask` stores it), a trim alone (`Clip::check_trim`, `Asset::check_source_out`, `check_own_lanes`) and
a move (`Timeline::move_clips` itself: a track of its kind, none locked, never onto a clip that stays, never before 0); a title's
position and size (`TextOverlay::check_values` / `check_span`) or keys (`TextKeyframe::check`, sorted). Anything else (the footage
above all), a clip or title the cut does not hold, or a cut breaking an invariant it did not already break (`Timeline::check_new`) is
refused. Nothing is written and the epoch does not move, so the gesture still writes one edit on release. A draft
frame is planned, refused and drawn like any other (the plan's reasons, the self-test gate, the surface); one the GPU does not draw,
a draft that is refused, or one whose clips wait on a proxy under Proxy only, is **FFmpeg's JPEG of the cut as it is**, with the
reason and a last one saying the drag shows once it is let go (with the GPU preview off the preview is that too: no draft is ever
sent on the JPEG route). Draft frames prime nothing and prefetch nothing (the next frame of a drag is the same time in another
cut), and their titles' poses go to a small atlas of their own (`render_draft_texture_with`, `gpu.md`). A `forward` request takes no
draft. `Trend` (`pace.rs`, beside `Pace`) reads the requests' times as they arrive: a drag whose last three requests each stepped
forward by at most 0.5 s is decoded with `Hint::Forward { fps }` (the canvas's rate) instead of `Hint::Scrub` (`decode_hint`) —
unless a layer of the plan plays backward, whose frames go the other way through the file: reading ahead would restart its run every
frame and trip the thrash guard (`Busy`), so that drag stays a scrub. The run reads ahead and the next frame is in the cache when it is asked for; frames are the same either way
(the run is the router's, proved equal to the one-shot), and a refusal by the thrash guard is that frame's FFmpeg fallback. Tests:
`draft.rs` (apply and its refusals), `pace.rs` (trend), `a_forward_drag_reads_ahead_unless_a_layer_plays_backward`, and the ignored `drive_tests.rs` cases `a_drag_frame_is_the_picture_the_edit_draws_once_it_is_written`
(an Inspector drag and a timeline move drawn as a draft are, pixel for pixel, the frames drawn after the edit is written; a vignette
draft and a stranger's clip go through FFmpeg) and `a_drag_that_walks_forward_reads_ahead_and_draws_the_frames_a_scrub_draws`.
`bench_tests.rs`' `a_scrub_and_a_gesture_at_1080p` (`KERF_BENCH=1`) times a 1920x1080 canvas of 1080p all-intra clips: on lavapipe
(debug build, a loaded 4-core box) the decode wait of a forward scrub goes from 5 to 0.1 ms with the read-ahead, and the frame is
lavapipe's raster (50 to 150 ms for one layer, 600 to 750 ms for a scaled and graded one, whether its numbers change or not); the < 33 ms target is for a real GPU and is unmeasured.

**GPU playback (A4, `gpu_preview/play.rs`; the page's half is in `frontend.md`).** `get_preview_frame { …, hint: "forward", client,
session }` is a frame of a **play session**: the webview stays the clock (the audio clock, less the output latency, plus the request's own
round trip) and asks for the time the picture should show once per animation frame; the requests go through the same latest-wins mailbox
and `client`/`seq` order as a scrub's, so a frame that overruns its slot costs the frames it overran and nothing else (skip, never slow
down), and they do not feed `Pace` (frames of a session are shown with vsync, never as a drag). No other `hint` is accepted, and `forward`
without a `client` and a `session` is an error. A session is a `kerf_core::PlayCut` (the Motion plan of the slice `stream_preview` would
play from the playhead; `core-render-plan.md`) over a `MotionFrames` (one forward-only cursor per clip; `gpu.md`), begun by the page's first
request of a session number, **which decides for the whole session**: the answer is `renderer: "gpu"` or `renderer: "stream"` with the
reasons (the page then plays the stream as it always did). It is the stream's when the GPU preview is off, has no surface or bounds, the
page has something to draw over a surface that sits above it, the plan cannot be made, the backend is not up or the self-test has not
passed, or **anything in the next 5 s (`HORIZON`) is a frame the compositor does not draw or would not draw in time** (`PlayCut::ahead`:
the plan's refusals, a reversed clip, a title whose position moves). Under Proxy only it is rejected as the stream is
(`PlanInputs::proxy_wait_from`, the whole cut from the start). The scan goes on half a second at a time, and a clip the prefetch finds no
run trusted for counts as a refusal too, so each `gpu` answer carries `play: {session, start, frame, repeat, ahead?}` and `ahead` is
`{at, handover, kind, why}`: the first frame the GPU will not draw and the time a stream that carries on must start at (`SpanPlan::handover`,
in the cut's time), so the page hands over before it gets there. A request in the frame already on the screen is `repeat` (nothing drawn;
a step back of under a second too); a request more than a second behind the last starts the session over from there; the session also
ends with a project change (the plan epoch), another session of the page or of another page, a request that is not part of it (the paused
frame, a scrub), a backend that was replaced, the setting turned off and `IDLE` (5 s) without a request, and a frame that cannot be drawn
ends it with `renderer: "stream"` and `play.ahead` (a cursor that died, a surface hidden mid-frame). **A failed play frame leaves the surface up**
(`conclude(.., hide_on_failure: false)`: the stream takes over beside it and the GPU's last picture stays until the stream's first frame is shown; a scrub's
failed frame still hides it for the JPEG, and a lost device is rebuilt as ever). The session is out of its lock while
a frame is drawn (teardown never waits for one), a panic in a step is caught and is the stream's too. **Before the first frame** the clips on screen at the
playhead are checked with `CursorFeed::check` (a container whose seeks are not trusted, a format of unknown layout), so a clip that cannot be run is the
stream's from the first request with the surface untouched, not a first frame that fails. **A frame at or after the first one the GPU does not draw** (the
scan's or a clip the prefetch could not open: `play.ahead.at`) is never attempted, whatever the page asks: it is answered `stream` with that `ahead`, so a
late hand-over cannot run a reversed clip or a moving title. **A session nobody asks for is ended**: the page that handed over, gave the GPU up or was
refused stops asking, so the `kerf-gpu-prime` thread (which waits for its nearest deadline and for nothing when it has none, and ends with the preview)
reaps the session `idle()` (`IDLE`, 5 s) after its last request, killing its idle cursors and ending its prefetch thread; a request after that begins a new
session. `keep_play` stores a session with its slot held and the setting and backend re-read there, so a teardown that ran while a frame was drawn is not
undone by the frame, and priming (`prime`) stores under the lock `take_primed` reads, refusing while a forward request is being served (`serving`) or a
session is held, so a priming that was in flight when Play landed cannot leave a duplicate. The prefetch thread
(`kerf-gpu-play-prefetch`) looks 2 s ahead: it opens the cursor of every clip about to come on screen (the file's probes and the process
start are paid before the cut) and has the titles of the next 1.5 s rasterised (`Compositor::warm_text`), neither on the frame's path. It stops at a
cancel flag between two clips and two frames, and a feed closed with the session starts nothing and drops a cursor it was opening; with more than six clips
in the window the feed keeps the ones wanted **first** (`CursorFeed::open(layer, needed_at)` drops the one wanted last for a nearer one, and opens nothing
for a clip wanted after all of them: `Opened::Full`, asked again later). A session's end waits for that thread (at most `FEEDER_JOIN`, 150 ms, for the one
clip or title it is at) so that what it holds, the device included, is released before the next frame and not by a detached thread as a process exits (the
lavapipe shader compiler crashed such an exit about one drive-test run in ten; the tests' `Turn` also waits for these threads: `busy.rs`).
**Priming** (`gpu_preview/prime.rs`): the start of a session is mostly `ffmpeg` starting (50 to 70 ms for one clip, more for several), and the playhead
it will play from is known long before Play, so when a **paused** GPU frame has been the last request for 300 ms (`PRIME_AFTER`) the thread
`kerf-gpu-prime` makes the `PlayCut` from it and opens the cursors of the frame on screen (each `ffmpeg` decodes its first frames into the pipe and
waits). The Play that follows adopts it (`take_primed`) if it asks from within `[start - 0.3, start + 0.15]` of the same plan epoch and backend; the
scan, the surface, the self-test and the plan are asked at adoption as at any start, so priming decides nothing. A cut the stream will play
(anything refused in the 5 s, a check that has not passed, a clip no run is trusted for) is not primed, and a primed session is dropped (its
`ffmpeg`s killed) by anything that is not a play from that playhead, a project change, the backend or the setting going, and 10 s (`PRIME_TTL`)
unplayed. The cost is one `ffmpeg` per layer of the frame on screen, idle, while the playhead rests: it is opened once per rest, not per frame.
Cursor timeouts of a session are `play_config()` (first frame 10 s, next 3 s), shorter than the export's so a hung decode ends in the stream's hands.
The diagnostics' `play` counts `{sessions, declined, handed_over, frames, skipped, repeats}`; the log has a line per session
(`GPU playback session begun` / `ended`, with the mean decode, composite and present time of its frames and the worst).
`gpu_preview/play_tests.rs` (`#[ignore]`d, a device and an FFmpeg) drives it without a window: the check pending, a cut crossed with its
cursor opened ahead, skipped and repeated frames, a title that moves, a transport stream found ahead, a change / a seek / another session /
the setting off.

**Two surface techniques, picked per platform** (`resolve_technique`;
`KERF_GPU_SURFACE=off|window|child` overrides). *`window`*: wgpu draws to the **main
window's own surface** and the webview over it is transparent
(`WebviewWindow::set_background_color`, alpha 0, at runtime, restored when the preview goes
off); the page keeps drawing titles, safe-area guides and the trim monitor over the picture.
**Windows' technique, unconfirmed** (needs a real machine): tao does not set
`WS_CLIPCHILDREN`, so the swapchain on the parent HWND should show through a transparent
WebView2 — wry's own `wgpu` example does exactly this (with winit, which has to turn
clip-children off). Tauri's `transparent: true` is deliberately *not* used there: on Windows
it makes the runtime paint the window with a `softbuffer` surface on every redraw, a GDI path
that would fight a swapchain. *`child`*: a **borderless child X11 window** of the toplevel
(`x11rb`, an empty Shape input region so the pointer falls through to the webview) placed
over the panel. **Linux/X11's technique, confirmed under WSLg + lavapipe**: WebKitGTK
composes the page into the toplevel's own X window, so technique `window` there loses to
GTK's repaints — measured: the surface presents ("GPU 430x240 · 21 ms") and the screen shows
black where the picture should be, with the Preview's transport bar left blank — and a
Wayland session has no way to put a window of ours inside GTK's (`Child::create` refuses a
non-Xlib handle, which leaves the JPEG with that reason in the status). A child window sits
*above* the page, so the page cannot draw over it: `routePreview` (frontend, pure) sends a
frame that needs a title box, the trim monitor or safe-area guides to the JPEG, and so does
one with a dialog, a menu or a drag ghost over the frame (`covered`: a 3x3
`elementFromPoint` grid on the frame, every 150 ms and after each click or key — the
Settings dialog opened *under* the picture before this existed); the in-frame HUD
(the name and `resolution · fps` chips, on a scrim) is under it too; it no longer prints a timecode, which is the transport bar's. **macOS has
no technique by default** (nothing here has run on a Mac, and the status line says so);
`KERF_GPU_SURFACE=window` forces the attempt for a developer, and may cover the window (relaunch
without it). Tauri's `macos-private-api` is *not* what is missing: see *macOS presenter* below.
`KERF_GPU_ADAPTER` picks the adapter (`GpuOptions::for_preview`): unset or `hardware` is the machine's GPU
and **a CPU rasteriser (lavapipe, WARP) is refused** (the status says the only adapter is one, and why it is not used), `software`
forces the CPU adapter, `auto` / `any` takes whatever wgpu prefers, CPU included; `KERF_GPU_SELFTEST=off` skips the
first-use check below.
Playback (forward 1×) is the FFmpeg stream unless the GPU plays the session (above): `streaming` is a JPEG route, the surface
is hidden while it runs and shown again on the settled frame. `x11.rs`' ignored tests present to a real child window and read the
pixels back **from the X server** (also after `skip_vsync(true)` and back: lavapipe on an `Xvfb` has a
mode that skips the refresh wait), destroy the device and draw again on a new one, and reach a
server that listens on the **abstract socket only** (an `Xvfb` where `/tmp/.X11-unix` is not
writable, WSL's case) at once: `RustConnection::connect` tries the filesystem socket and then TCP
and never the abstract socket libxcb tries first, and waited out a TCP timeout of minutes
(`connect` tries it first). **The Xlib `Display` is the process's own, opened once**: `tao`'s
`display_handle()` calls `XOpenDisplay` anew on every call, never closes it and `new_unchecked`s a
null one, so its handle is not used — only the toplevel's window id comes from the window handle —
and one display is kept for the whole life of the process however often the backend is rebuilt.
Every request on the child window is `check()`ed, so a server that refuses it falls back promptly.

**macOS presenter (WP-8): what the source says.** Read from tauri 2.12, wry 0.57, tao 0.37,
wgpu-hal 30 and raw-window-metal 1.1; none of it has run on a Mac. (1) Tauri's `macos-private-api`
gates only the `transparent` option of the window and webview builders: the surface goes *under* the
webview and the window stays opaque. wry itself clears the WKWebView's `drawsBackground` (a private
KVC key) whenever a `backgroundColor` is configured (Kerf's is), so the page is see-through
wherever it paints nothing, which `Preview.svelte`'s `data-surface-hole` arranges. (2) tao hands
wgpu the window's *current* content view, after wry's build its `WryWebViewParent` holding the
WKWebView. wgpu's Metal backend does not replace that view's layer: it makes it layer-backed and
adds a `CAMetalLayer` **sublayer** that follows it by KVO. Hence: **(a)** creating the surface
asserts the main thread and `build_backend` runs on `kerf-gpu-build`, so `KERF_GPU_SURFACE=window`
on a Mac could only fail; now `surface_on_main_thread("macos")` opens the device off-thread and
`on_main_thread` (Tauri's `run_on_main_thread`, 5 s deadline, a panic is an error, a task that
arrives after the deadline does not run its work) makes only the surface there; other platforms
are unchanged. **(b)** The sublayer is added after the WKWebView's layer, so it should draw
**above** the page: backdrop and picture over an invisible, still clickable UI, with the Settings
toggle unreachable. That is why macOS stays "none" by default and why the user-visible note does not
name the variable. **(c)** Nothing removes it: the superlayer retains it (the layer's `Drop`, which
only deregisters its observers, never runs), so dropping the surface (setting off, `drop_backend`,
device loss) leaves the last drawable on screen and a rebuild stacks another. A developer who forced
it recovers by quitting and relaunching without `KERF_GPU_SURFACE`. The test of it on a Mac, the
design of an app-owned layer if it fails, the measurements and the acceptance list are in
`.claude/plans/macos-presenter.md`.

**Detached panels (`popout.rs`).** A panel moved into a window of its own is dockview's
popout: `window.open`, then the panel's DOM is moved into the new window's document while
its script keeps running in the editor window's JavaScript realm — so the `editor` / `ui`
singletons, the transport clock, the Web Audio engine and every Tauri `Channel` are the same
objects in every window and **nothing is synchronised**. The shell has three jobs.
(1) **The main window is built in code** — `tauri.conf.json` says `"create": false` for it and
`popout::create_main_window` builds it from that very entry (`WebviewWindowBuilder::from_config`,
so `visible: false`, the backdrop and the reveal are exactly as before; a Rust test pins
`create: false`), because only a builder can carry the `on_new_window` handler: a window declared
in the config has none and `window.open` returns null from it. (2) **The handler answers only a
window the page announced** (`popout_expect { rect?, size?, background? }` → `{label, position}`,
a `PopoutQueue` of announcements taken in order, 15 s time-out, at most 32 waiting; pure and
unit-tested) **and only for `/popout.html` on the editor webview's own scheme, host and port**
(`is_popout_url(url, main)`, `main` read from the webview): anything else — a stray `window.open`, a
`target="_blank"`, another origin's page of that name — is denied. The window is built `window_features(features)` (what makes it
*related* to the opener: same web process on WebKitGTK, same environment on WebView2, same
configuration on WKWebView — the thing that makes the returned `Window` scriptable), sized and
placed by `place` (pure): a rectangle off every screen is moved onto one, a panel detached by hand
goes to the centre of **another** monitor when there is one, WebKitGTK ignores the `window.open`
features (`NewWindowFeatures` arrive as `None`), so the rectangle comes from the announcement.
`popout_cancel` forgets one the page did not open, `popout_focus` raises one (`window.focus()`
does not raise a native window), `popout_move` applies the one-shot correction below, and
`close_popout` destroys one by label. (3) **Closing**, per platform, because wry differs: its
WebKitGTK `close` signal destroys the webview widget only (a blank window stays — the Linux hook
connects the widget's `destroy` to the window's), WebView2 destroys the window itself, and
WKWebView has no `webViewDidClose:` so `window.close()` is a no-op there and the page asks
`close_popout` from dockview's `onWillClosePopoutWindow`. The editor window going away destroys
every popout (`on_window_event`), as does its page reloading (`on_page_load`: the panels in them
are that page's); a popout going away emits `popout-closed`. Also Linux-only
(`gtk`, `webkit2gtk`, the versions Tauri resolves): WebKitGTK defaults
`javascript-can-open-windows-automatically` to **false** and blocks a `window.open` no gesture
asked for — a restored layout opens windows at launch with none — so `with_webview` turns it on
for the editor window (the handler still decides). No capability and no CSP change: the panels
run in the editor's realm and use its IPC, and a popout is served by the same `tauri://` protocol,
so the same CSP applies to it (measured: an inline script and a foreign image are blocked there).
The popout page `frontend/static/popout.html` must be a real file — the static fallback would
answer the path with `index.html` and start a second editor — and `+layout.svelte` refuses to
boot when a window has an opener. **dockview ≥ 8.4.1** is required: 8.3.1 refuses any
non-http(s) popout URL, which is every packaged Linux / macOS build (`tauri://localhost`), and
only 8.4 polls the popout's `closed` flag, the one signal a shell that destroys a webview gives.
Tauri's `window.screenX` is the outer position and `innerWidth` the inner size, but the position
the platform *reads* is not always the one it was *set* to (WSLg: 32 px), and a layout saves the
read one; `popout.svelte.ts` therefore moves a window that opened a little off by the error, once
(`positionCorrection`, pure), so a restored window does not creep at every launch.
Windows are positioned in logical pixels clamped against `available_monitors` (work areas).
**Not verified outside Linux/WSLg**: WebView2's `NewWindowRequested` + `SetNewWindow` (the popup
being scriptable, no deadlock building a window inside the handler), WKWebView's `close_popout`
path and `window_features` placement, `screenX` against mixed-DPI monitors, `requestAnimationFrame`
in a main window that is minimized, HTML5 tab drags between windows against Tauri's drag-drop
handler on Windows (`dragDropEnabled` — Kerf keeps it on for file drops). The GPU preview
(`gpu_preview.rs`) hard-codes the `main` window for its surface and its bounds, so a Preview in a
popout takes the JPEG path (`routePreview`'s `detached` reason) and hides the surface.
