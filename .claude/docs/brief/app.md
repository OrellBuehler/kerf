# app — brief

Rules only. The full file `.claude/docs/app.md` is authoritative and holds the measurements and the story behind each rule: grep it for the phrase in parentheses.

kerf-app is the Tauri v2 shell: `crates/kerf-app/src/lib.rs` (`run()`, commands), `main.rs`, `headless.rs`, `shell.rs`,
`settings.rs`, `recovery.rs`, `popout.rs`, `editor_context.rs`, `thumbnail_commands.rs`, `mcp_commands.rs`,
`gpu_preview.rs` + `gpu_preview/`. It owns the `Arc<Mutex<Project>>` shared with the MCP server; no editing logic here.
Commands live in `commands/<module>.rs` (`library`, `project`, `analysis`, `voiceover`, `music`, `sound_effects`, `editing`, `linked_av`,
`motion_templates`, `review`, `media`, `tasks`, `export`, `app_settings`): `pub(crate)`, listed in `generate_handler!` as
`commands::<module>::name`. `lib.rs` keeps the shell, shared helpers, logging / launch, `run()` and `get_render_bar` (a kerf-gpu test reads it).

## Map of the full file (grep these)

- Opening block: CSP, panics, single instance, launch argv, window reveal, generation guard, cancel flags
- `Playback ids.` / `Headless mode` / `Process start.` / `Library and project commands.`
- `is the entry`: the long command prose (analysis, proxies, voiceover, music, SFX, editing ops, titles,
  templates, captions, media, delivery, settings, MCP settings, export, output paths, playback, logging,
  blocking, config)
- `Command index` (every `generate_handler!` name) and `Events`
- `Editor context` / `Crash recovery` / `Scrubbed frames.`
- `GPU preview (A2` / `Scrubbing and hardening (WP-5)` / `Drag frames and forward scrubs (A3` / `GPU playback
  (A4`
- `Two surface techniques` / `macOS presenter (WP-8)` / `Detached panels`

## Start-up, CSP, launch

- CSP is on; anything new loaded from the network or a `blob:` must be added deliberately (`has to be added
  there deliberately`); a test pins it (`nothing_more`). No font host: fonts are bundled (`never reaches a
  font host`). `connect-src` needs `'self'` for SvelteKit's `_app/version.json` (`regains focus`). The limiter
  AudioWorklet stays a bundled same-origin script (`?worker&url`).
- Panics log a backtrace; release strips only `debuginfo` so frames keep names (`install_panic_hook`).
- Single-instance plugin is the first plugin in `run()`; a second launch focuses the window and emits
  `open-project-file` for a `.kerf` argv (`project_arg`) (`One instance per identity`).
- The first launch's argv goes to `AppState.launch`; the page pulls it once via `take_launch_project` — a
  command, not an event, because an event before the listener is lost; returns once so a reload does not
  reopen over edits (`first launch's own argv is honoured too`).
- Reopen-last-project is decided on a thread of its own, never main, because the file may be on an offline
  share; a project named on the command line or by a second launch always wins (`offer_reopen`).
- `last_open_project` is the project open when the last session ended, set by `note_open` (MCP opens too),
  cleared by `new_project` (`not the newest recent entry`).
- A second launch during webview boot is held in the slot (newest wins, one lock) (`still booting`).
- A `.kerf` argument naming nothing is never opened, because `Project::open` would create it: `{missing:
  path}` / `launch-project-missing` (`would *create* it`).
- Main window starts hidden; its `backgroundColor` equals Kerf Dark `surface-app` (bun test pins it vs
  `app.html`); the page calls `show_main_window` after settings, theme, dock and a painted frame (`The main
  window starts hidden`).
- A 3 s `REVEAL_FAILSAFE` thread shows it anyway; `reveal_once`: first asker wins, only a `show` that worked
  keeps the claim (no window gives it back, failsafe retries each 500 ms); a second launch marks the reveal
  done only if it worked (`reveal_once`).
- `show_main_window` is a command so no `core:window:allow-show` is needed (Rust test pins absence);
  `tauri.dev.conf.json` merges only `identifier` (`allow-show`).
- `use_bundled_ffmpeg()` is the first statement of `run()`, before runtime/webview/threads, because `set_var`
  races env reads; it returns what it set and `setup` logs it (`Process start.`). Setup warms fonts on a
  thread (`Setup warms the fonts`).

## Jobs, generation, cancel flags

- Any job that resolves inputs under the lock, releases it and applies later notes `Project::generation()` and
  checks it on return (`require_generation`); covered jobs listed at (`Jobs land in the project they started
  for`).
- `save_project_as` swaps its reopen in only if `Project::change_count()` is unchanged, else saves again under
  one held lock; `replace_with_saved_copy` keeps the generation (in-flight jobs still land), `replace_with`
  (open/new) moves it; the target goes through `check_output` (`replace_with_saved_copy`).
- Cancel flags are `StopEpoch`s on `AppState`: a run `.begin()`s and polls; `.stop()` ends runs begun before
  it only, so no flag resets under a starting run and a GUI Stop reaches the agent's run (`StopEpoch`). MCP
  takes the `begin_*` of the same job; one `export_cancel` for export and variants; a test pins each `begin_*`
  to its own button (`own button`).
- A replaced project stops its jobs via `stop_or_replaced` / `Throttled` (generation read at most twice a
  second) so the ffmpeg is killed (`A replaced project stops its jobs`).
- Playback ids are `session * 2^32 + n` (`playback-id.ts`); `Playbacks` keeps highest begun (`fetch_max`) and
  highest stopped, so reordered start/stop and a reloaded page resolve by comparison (`Playback ids.`).

## Headless mode (`headless.rs`, `shell.rs`)

- Code the MCP server reaches takes a `Shell` or `&impl Host`, never an `AppHandle`, because headless builds
  no Tauri app (tao panics with no display); `main.rs` calls `headless_main()` before `run()` (`code the MCP
  server reaches takes a`).
- `--project` must exist, `--new-project` must not (exit 2) (`must exist and`).
- `--workspace DIR` / `KERF_WORKSPACE` confines every client-named path (`mcp.md` "Workspace"): existing
  non-root dir; settings, data, cache dirs, template library, `--token-file` may not lie in it (exit 2,
  checked before `--data-dir` / `--cache-dir` are made); holds over `--stdio` (`confines every path an MCP
  client names`).
- `--public-url`, `--max-upload-gb`, `--allow-url-import` are usage errors without a workspace; `--public-url`
  is read only over HTTP and errors with `--stdio` (`each a usage error without a workspace`).
- No single-instance plugin: taken port exits 3; two runs sharing a settings file can race the first token, so
  use `--data-dir` or `KERF_MCP_TOKEN` (`race to make the first token`). In-memory only: review, proxies,
  `--cpu-percent`; no GPU preview, crash marker, reveal failsafe or launch slot (`No GPU preview, crash
  marker`).
- `--cache-dir` is set before logging or any thread reads it (`paths::set_cache_dir`) and moves the downloads
  too (`that moves the`). `--cache-max-gb N` (> 0, flag wins, else exit 2) starts `cache_cap::start` after the
  project opens; a cap that cannot be kept is exit 2, not a run without it (`not a run without it`).
- Only this process's project is protected: never share a cache dir with a desktop app or a second server;
  `stitched/` is not covered (`Only this process's project is protected`).
- stdout is one JSON ready line and `--help`; logs go to stderr and `<data>/logs`; token never printed unless
  `--show-token`; `--token-file` atomic, mode 0600 (`one JSON ready line`).
- Signals: close listener, `AppState::stop_all`, cancel and wait up to 10 s, `recovery::on_exit`; second
  signal exits 130; codes 0 signal, 1 failure, 2 usage, 3 not listening (`Exit codes: 0`).
- `--stdio`: no listener/token/ready line; `--mcp-addr`, `--token-file`, `--show-token` are usage errors,
  `KERF_MCP_TOKEN` ignored (`serves one MCP session`). stdout carries JSON-RPC only: `rpc_out` moves real
  stdout to a private CLOEXEC fd and points fd 1 at stderr; a stderr that is the same file as non-terminal
  stdout (`same_file`) is refused, exit 2 (`stdout carries JSON-RPC only`).
- `--prefetch-models` starts no server, uses the same checked fetch as first use, needs `--cache-dir`; a
  `--workspace` with it is only checked (`--prefetch-models whisper`). `tests/headless.rs` drives the real
  binary without `DISPLAY`.

## Commands: shape and locking

- Tauri maps JS camelCase args to snake_case; unparsable ids name their argument (`invalid clip_id`).
- No command runs on the main thread: quick ones `#[tauri::command(async)]`, heavy ones `async fn` over
  `blocking()`, resolving inputs under the lock and releasing it before the slow part (`lock_user`,
  `Project::decode_*`); MCP heavy tools do the same with `lock_agent` (`No command runs on the main thread`).
- `run_analysis` is the one path for command and MCP tool (`lock` closure: `lock_user` / `lock_agent`); merges
  each finished step as it lands, keeps a cancelled/failed step out, "nothing to run" errors before anything
  starts (`merges each`).
- Call `warm_transcription_backend` before taking a project lock, never inside one: statuses read the backend
  and the first answer runs `ffmpeg -h filter=whisper` (`warm_transcription_backend`).
- `spawn_proxy` takes its place before analysis before it returns and may run ffprobe: call it from the
  blocking pool; `open_project` uses `respawn_proxies` (own thread) (`takes its place in front of analysis`).
- `get_timeline_frame`, `get_preview_frame`, `start_playback` refuse under Proxy only while a clip's proxy
  builds; `get_frame` does not; `set_settings` re-queues proxies after a `proxy_size` / `preview_source`
  change (`refuse under Proxy only`). `rebuild_proxy` clears a deleted proxy's mark only once accepted.
- `get_ripple_mode` / `set_ripple_mode` answer the bool: a setting, no revision (`records no revision`).
- `compose_music` checks and aligns under the lock (rehearsing placement so a refused start fails before the
  render), renders lock-free, lands via `place_music` with the request's own `track_id` / `at` /
  `replace_clip_id`; a refused placement stores nothing (`rehearsing the placement too`).
- `preview_sound_effect` returns WAV bytes via `tauri::ipc::Response`, not a path (CSP has no `media-src`, no
  asset protocol); touches no project; GUI-only on purpose (`bytes of the rendered WAV`). `add_sound_effects`
  renders lock-free (stopped only by a replaced project) then places as **one** revision (`Add sound
  effects`).
- Edits that carry linked clips take optional `link` (`false` = named clips alone) via `with_links`;
  `remove_clips` `ripple: true` via `with_ripple`, omitted follows the project mode; roll/slip/slide clamp and
  never ripple; group ops (`move_clips`, `split_remove_clips`, `apply_motion_preset`, `set_behaviors`) are one
  revision, all or nothing (`edits the named clips alone`).
- `set_delivery_format` sets the rate with the frame: a call without `fps` clears it (page passes `keepRate`);
  refused outside 1..240 (`so a call without`). `set_grade` checks a new LUT path off the lock so a curve edit
  on a clip whose LUT went missing still applies (`checked off the lock`).
- `assemble_by_tag` is `async` + `blocking`: inputs under `lock_user` → `AssemblePlan::prepare` free → apply
  under the lock; `list_assets` takes no filter (`assemble_by_tag_inputs`).
- SVG/title work that parses or rasterises runs before `lock_user` on the blocking pool; the lock runs only
  `*_parsed` (`add_svg_overlay_parsed`, `apply_template_parsed`, `set_title_text_parsed`, `title_text_edit`) —
  covers `add_svg_overlay`, `update_svg_overlay`, `apply_template(_clip)`, `add_title_clip`, `validate_svg`
  (needs no project) (`Reading an SVG takes`). `set_template_values` / title-text edits take inputs under the
  lock, prepare released, and refuse a title that changed meanwhile ("try again") (`template_edit_inputs`).
- `svg_preset_preview` is shown through an `<img>` data URL, never inline, because the sanitiser, not the
  webview, decides what an SVG may do (`never inline`).
- Template library calls need no project and never lock; `svg_element_ids` is pure and the core refuses text
  over a title's 128 KiB; `template_image_value` reads at most one byte past `template::MAX_IMAGE_BYTES`;
  `import_template` never overwrites (`never take the lock`). `export_template` uses
  `require_user_output_path` and `check_output` including the library dir (`and the library directory`);
  `save_template` reads under `lock_user`, builds released. The library is app-level (not undone, not staged)
  (`not undone, not staged`).
- `cut_transcript`, export/interchange/markers/captions commands snapshot under the lock and build/write on
  the blocking pool (`cut_transcript_inputs`). `refine_transcript` uses `run_refine` shared with MCP, lock
  released, background lane, stopped by `cancel_analysis` (`run_refine`).
- `set_clip_stabilize` refuses a still, a title, a clip of sound and 360 (`a still, a`).
- `sync_clips_by_audio` is lock-free inputs → measure → apply; a forced call for the plan last measured
  applies `AppState.last_sync` instead of decoding again (`AppState.last_sync`).
- `get_filmstrip` ships sheets as base64 `data:` URLs (CSP admits `data:`, not `blob:`) and has no MCP tool
  (`FilmstripPayload`).
- `start_playback` streams **raw bytes** over a `Channel` (f64 LE time + JPEG, `playback_message`;
  `frame-transport.ts` and a test share a vector), not base64 JSON; cancelled by caller-supplied id, because
  start/stop can arrive out of order and a late stop must not kill its replacement (`caller-supplied`).
- `start_playback` resolves `Ok` for stop/supersede, rejects with the ffmpeg error when a watched stream dies;
  it passes "superseded or stopped" as cancel to `stream_preview_until` so a seek also ends title drawing
  (`stream_preview_until`).
- `get_audio` returns raw mono s16le PCM via `tauri::ipc::Response`, "the only non-JSON command" — yet
  `preview_sound_effect` and `thumbnail_preview` also return bytes (contradiction in the full file: see
  `only non-JSON command` vs `the PNG`).
- `thumbnail_commands.rs`: templates and the sanitiser run before the lock; `export_thumbnail` reads originals
  (`thumbnail_commands.rs`). `reveal_path` opens the containing folder, not the file (`containing folder`).
- `get_staged_edit` returns the `StagedEdit` with its diff in one round trip; it carries `owned_by_agent`,
  never the agent's session id; `apply_staged_*` / `resolve_task` re-make proxies when the proposal marked
  assets, a tag change does not (`staged_asset_changes`).
- `cancel_levels`: flag reset when `get_levels` starts, polled as its cancel; the pass holds `cpu::lease` so
  it must stay stoppable (`cannot be left`).
- `get_render_bar` passes no caps unless `display_probe::offered()`, so bar and preview agree (no GPU span on
  a machine whose colour conversion differs); caps are `Compositor::session_caps_for` after
  `kerf_gpu::prepare_timeline` on the blocking thread (`the render bar has no GPU span`).
- GUI-only (no MCP twin): `get_filmstrip`, `preview_sound_effect`, `set_preview_bounds`, `gpu_preview_status`,
  popout commands (`GUI-only on purpose`).
- New commands go into the Command index; events from the MCP server: `project-replaced`, `settings-changed`
  (payload the full `SettingsView`), `ripple-mode-changed`, `speech-model-changed`, `templates-changed` (`from
  the MCP server also`). `platform_targets` is mirrored for the harness in `platforms.ts` (`is wired: the`).

## Settings (`settings.rs`)

- Preferences live in the platform config dir, not in a `.kerf`; `KERF_CPU_PERCENT` wins at launch, a moved
  slider after (`a moved slider wins after`).
- `workspaces`, theme, keybindings, `editing`, `layout` are opaque `serde_json::Value`s: the frontend owns the
  shape and validates them on the way back (`opaque`).
- `set_settings` takes a patch merged under a mutex; only changed fields are pushed into the engine, so a
  layout write never re-applies the stored CPU share over `KERF_CPU_PERCENT` (`takes a patch`).
- The write is atomic (temp, fsync, rename) and happens **before** the patch is put into force; a failed write
  changes nothing; tests that put settings into force take turns (`ONE_AT_A_TIME`) (`happens before the patch
  is put`).
- An unreadable / non-UTF-8 / non-object file is moved aside to `settings.corrupt-<unix-ms>.json` before
  defaults load, so a save cannot destroy an imported theme; a parseable file is read field by field via
  `parse`: a bad value defaults only its field, and the raw value plus unknown keys are kept in
  `Settings::kept` and written back unchanged (`moved aside to`).
- A new `Settings` field must be listed in `STORED_KEYS`, or it lands in `kept` and is lost on restart
  (`STORED_KEYS`).
- At most five `settings.corrupt-*` and five `settings.reset-*` stay (`prune_backups`). The recent list is
  statted with the settings lock released (`recent_in`) so an offline share never blocks a write.
- `recent_projects` (newest first, max 10) and `last_open_project` are app-kept, not writable by a patch; a
  vanished file is dropped, never created (`not writable by a patch`). Old `transcribe` migrates into
  `auto_analysis.transcript`, never written back (`never written back`).
- `reset_settings` keeps the old file as `settings.reset-<unix-ms>.json` plus recent list and last open
  project; `settings::reset_keep` is the one place to add what a reset must not lose (`settings::reset_keep`).
- `read_text_file` / `write_text_file` are the only commands reading a caller-chosen path: `.json` only,
  regular files, 1 MiB cap (`the only commands that`).
- `set_mcp_settings` refuses any other key; a network address needs the token; the listener restarts when
  address or switch moves (`refuses any other key`). `mcp_connection_info` `running` / `error` are for the
  address now configured so the page can wait for the restart (`listener_state`).
- `agent_review` off: agent edits (task work included) land live as ordinary revisions; an open proposal stays
  open and is applied for no one; the app does nothing on the flip (`off means an agent's edits`).
  `agent_status` judges by age since `mcp::LAST_AGENT_ACTIVITY` (stamped in `lock_agent` and `get_info`), not
  a connection (`LAST_AGENT_ACTIVITY`).

## Export, output paths, logging

- A failed or cancelled render deletes what it wrote and the engine does it (`kerf_core::discard_partial`),
  only if this run touched the file (mtime), so an earlier export at that path survives; commands neither
  remove nor compare; MCP `export` repeats it so a cancel lands before the future drops (`deletes what it
  wrote`).
- `keep_time_bases` runs after a completed export with no lock for probing, then `Project::apply_time_bases`
  under the surface's lock (database only: no revision, no `project-changed`); a failed write is logged and
  the export succeeds; a cancelled or failed render keeps nothing (`keeps the tick its render backfilled`).
- `export_variants` is one `render_variants` call, one ffmpeg run unless `single_pass` is false: a stopped
  single pass removes every file, sequential removes the one in flight and keeps finished ones (`all N files
  were removed`). `smart_crop` frames shots first and streams `waiting: "framing the shots"` (`framing the
  shots`).
- Every output path is checked against the project's own files (`Project::output_path_inputs` under lock,
  `check_output_path_with` after release, before the lock-free render) in `export_timeline`,
  `export_variants`, `export_cover`, `export_srt`; `check_output` adds Kerf's config dir (holds the agent
  token) and logs (`check_output_path_with`).
- Agent-named output goes through `require_local_output_path`: refuses UNC/network paths (`//host/share` on
  Windows only, as `//tmp/x` is `/tmp/x` elsewhere), URLs, relative paths, the device namespace; a user-picked
  save-dialog path uses `require_user_output_path` (shares allowed on Windows) (`which lets a share they chose
  through`).
- A file an agent asks Kerf to **read** (`import_asset`, caption `path`) goes through
  `require_local_input_path` before any `stat`, because opening a share makes the machine authenticate to its
  host (`require_local_input_path`).
- Logging: the file layer is synchronous — the `RollingFileAppender` is the writer, not
  `tracing_appender::non_blocking` — because a queue's worker loses the last lines on a hard crash and neither
  a panic-hook flush nor dropping the guard would cover it (`The file layer is synchronous`). `log_dir_path`
  is the one place the dir is decided (`log_dir_path`).
- A log ending without `kerf exiting` may have crashed; one ending in an update line did not (Windows updater
  `process::exit`s) (`may* have crashed`). Startup line never carries env dumps or args (`never env dumps`).
- All failures reach the file: the `invoke` wrapper in `api.ts` forwards every rejection to `log_frontend`
  (also toasts, `window.onerror`, CSP violations); cap 8 KiB per message, 30 lines/s; MCP tool errors log once
  in the `call_tool` override (`log_frontend`). No-op in the browser harness.

## Build config

- `beforeDevCommand` / `beforeBuildCommand` run from Tauri's app dir (crates/), so they anchor with `cd "$(git
  rev-parse --show-toplevel)/frontend"`; `frontendDist` is relative to the config file (`fragile relative
  path`).
- `build.rs` takes the Windows manifest from Tauri (`new_without_app_manifest`) and embeds
  `windows-app-manifest.xml` via the linker, or the lib's test binary binds comctl32 v5 and dies with
  `STATUS_ENTRYPOINT_NOT_FOUND` (`new_without_app_manifest`).
- `opener:allow-open-url` in `capabilities/default.json` must be object form with an `allow` scope for
  `https://github.com/OrellBuehler/kerf/*`, else every `openUrl` is `ForbiddenUrl` (`with no scope of its
  own`).

## Editor context, recovery, scrub

- `report_ui_context` goes into `EditorContextState`, its own `Mutex`, never the `Project` (a view of the
  window, not the cut); page throttles to 300 ms plus a 30 s heartbeat; `get_editor_context` reports
  `age_secs`, `stale` after 90 s (`EditorContextState`).
- The untitled project is never only in memory: created in `setup`, not the top of `run()`, because a second
  launch is handed over before `setup` and must not make a session (`must not make a session`); `new_project`
  does the same.
- Replacing the project via `replace_with` deletes the old session; each command `close()`s the returned
  project after dropping the lock (`deletes the old session`). Clean exit: `close_at_exit` keeps a session
  holding work; the close guard can be bypassed (`close_at_exit`). "Don't save" calls
  `discard_recovery_at_exit` (matched by session id); a failed or unasked prompt never sets it
  (`close_on_exit`).
- `list_recoveries` scans once per run, skips our own session, keeps found ones **locked** in `RecoveryState`
  until recovered/discarded; no MCP tools; `project_path` stays `null` for a recovery-backed project
  (`RecoveryState`).
- `get_timeline_frame` and the GPU JPEG route use `AppState.scrub` (one `ScrubFrames`); `start_playback` calls
  `scrub.stop()`, `new_project` / `open_project` `scrub.clear()`; MCP `preview_timeline` stays a one-shot
  still of the staged cut on purpose (`stays the one-shot still on purpose`).
- Env knobs: `KERF_SCRUB_CACHE_MB`, `KERF_SCRUB_WINDOW`, `KERF_GPU_LANE=off`, `KERF_PLAYBACK_PARTS=off`,
  `KERF_EXPORT_PARTS=off`, `KERF_SEGMENT_DIR`, `KERF_KEEP_PARTS` (`KERF_SCRUB_CACHE_MB`).

## GPU preview (A2, `gpu_preview.rs`, opt-in)

- `Settings.gpu_preview` defaults off; off, none of it runs and the webview sends no extra command (`default
  off, persisted beside`).
- `get_preview_frame` plans from a snapshot of plan inputs (`gpu_preview/snapshot.rs`) at the start of the
  output frame holding the time (`plan_at_grid`, same grid as `ScrubFrames`); answers `renderer: 'gpu' |
  'ffmpeg' | 'superseded'`: GPU only when `RenderPlan::reasons(caps, size)` is empty, else FFmpeg's JPEG of
  that frame in the same call; a refusal is not an error (`snapshot of the plan's inputs`).
- GPU refuses rather than approximates; the refusal lists for text, fades/dips/transitions, keyed placement,
  masks and effects are at (`a frame is refused for text only where`), (`A paused frame inside a fade`),
  (`masks and the blur`). Keyed zoom/position draws only once the placement probe says this FFmpeg's
  `pad`/`perspective`/`crop` place as the model does, else the plan names `keyed_placement`
  (`keyed_placement`).
- Device, compositor, `FrameSource`, presenter are built lazily and dropped together (`Backend`); a build
  failure is remembered with backoff (5 s doubling to 5 min, `Backoff`, forgiven only by 20 presented frames
  in a row); `DeviceLost` or any device/surface-gone error (`react`) drops and rebuilds; a refused plan, busy
  decode or occluded window is only that frame's fallback.
- Nothing here may freeze the preview: build the backend **off the render lock** with `BUILD_DEADLINE` (10 s,
  own thread; a frame that finds a build under way is the JPEG at once); measurements run via
  `kerf_gpu::prepare` on the frame's blocking thread before `ensure_backend`, off the render lock and outside
  the build deadline, each probe bounded; a failed probe is the JPEG and retried after backoff (`here may
  freeze the preview`).
- The render itself never spawns a process; a layer whose probe has no answer yet is refused (FFmpeg's)
  (`never spawns a process`).
- A play session primes the whole timeline once (`ready_play`, `begin_play` after the self-test gate,
  `make_primed`): `prepare_timeline`, `prepare_placed_files`, `prepare_ahead`; a moving still's probe
  (`still_probe`) is asked per plan, because a timeline does not know which assets are pictures (`primes the
  **whole timeline**`). The render bar asks as it judges layers (`BarRequest::ask`) so bar and stream agree on
  file range.
- A panic in the GPU path (`frame()`'s `catch_unwind`, the build thread's) drops the backend behind the
  backoff, hides the surface and answers FFmpeg's JPEG; the page treats a *rejected* `get_preview_frame` the
  same (`catch_unwind`).
- The setting flips synchronously (`set_enabled`; only `teardown_if_off` runs on a thread); an in-flight frame
  re-checks before building and before showing; turning on forgets the backoff (`set_enabled`).
- A presented frame prefetches cuts around the playhead (`prefetch_ahead`, `cut_moments`): clips starting
  within 2 s or ended within 2 s, nearest first, at most 3, via `FrameSource::frames(.., Hint::Prefetch)` on
  its own thread after the render lock is released; a failure is nothing.
- A hard driver crash must not repeat each launch: marker `gpu-preview-attempt` in the config dir is written
  before a backend is built and removed once a frame reached the screen; a launch finding it turns the setting
  off (`take_crash_marker_in`). On Windows the webview is made transparent as the **last** build step and
  restored by a guard's `Drop` (`restored by a guard`).
- `set_preview_bounds`: device pixels relative to the webview plus its size, `visible`, two colours, and a
  `seq` across every Preview the page has had; an older report is ignored (`is how the page says where the
  Preview frame is`). `GpuPreview::set_bounds` and an in-flight frame's present (`under_bounds`) take the same
  lock and the frame re-reads bounds under it, so a hide that lands mid-draw sticks; the child window follows
  move/resize (`Child::follow`) and is re-raised each present (`Child::place`).
- No MCP tool (agent has `preview_timeline`); no capability permission or CSP change; `tauri.conf.json`
  unchanged (no `transparent`) (`No capability permission and no CSP change`).

## Snapshot, latest-wins, present mode, self-test (WP-5)

- Frame inputs are an `Arc<PlanInputs>` (`PlanSnapshots`), fresh while `kerf_core::plan_epoch()` is unchanged
  and under 2 s; stale ones are retaken from `lock_user` (`Taken::from` reads the epoch first under the lock;
  `Taken::resolve` stats proxies after); it is `working_timeline()` as the GUI sees it; the Proxy-only refusal
  answers from it (`PlanInputs::proxy_wait`) (`Arc<PlanInputs>`).
- Latest wins: requests post to a `Mailbox`; the waiting one is replaced and answers `superseded`; a dead
  worker or dropped job answers an error, not a skipped frame (`Mailbox`). "Newest" is the page's order
  (`client` + `seq`), not arrival, since Tauri runs each call as its own task; no `client`/`seq` goes by
  arrival (`not the order of arrival`).
- The render lock is not held across decode: a frame takes `Drawing` (Arcs) under the lock, decodes and
  composites without it, relocks to show only if the backend is still the one that drew it (`Drawing`).
- Present mode is decided by a request, not a timer by `Pace`: drag = requests under 120 ms apart, end = two
  gaps of 600 ms+ in a row; `Presenter::skip_vsync(true)` only in a drag where the surface has
  `Immediate`/`Mailbox`; the mode changes just before the frame presented into the reconfigured surface (`not
  by a timer`).
- Self-test (`check.rs`): the first GPU-able frame starts `kerf_gpu::parity_self_test` on its own thread;
  every frame is FFmpeg's until the verdict (`gpu-preview-checked`, then the page asks again); a mismatch and
  a check that could not be made both close the GPU path; toggling the setting re-runs it; a verdict belongs
  to an `AdapterKey`; the gate is no looser than the parity contract (`parity_self_test`).
- Diagnostics (`diagnostics.rs`) are read by tests, no command serves them; `drive_tests.rs` /
  `bench_tests.rs` (`KERF_BENCH=1`) are `#[ignore]`d, need a device + FFmpeg (`drive_tests.rs`).

## Drag frames (A3, `gpu_preview/draft.rs`, `pace.rs`)

- `draft` (`PreviewDraft`) is what the gesture would write on release, applied to the snapshot's timeline
  (`PreviewDraft::apply`); each change goes through its edit's own checks (`Transform::check_values`,
  `Color::check_values`, `Clip::check_fades`, `Mask::normalized`, `Clip::check_trim`, `Timeline::move_clips`,
  `TextOverlay::check_values`, `TextKeyframe::check`) so the preview never shows a state the edit would
  refuse; anything else, an unknown clip/title, or a cut breaking an invariant (`Timeline::check_new`) is
  refused (`never shows a state`).
- Nothing is written, the epoch does not move: the gesture still writes **one** edit on release (`epoch does
  not move`).
- A refused or undrawable draft, or clips waiting on a proxy, is FFmpeg's JPEG **of the cut as it is**; with
  the GPU preview off no draft is ever sent; drafts prime and prefetch nothing; a `forward` request takes no
  draft (`prime nothing and prefetch nothing`).
- `Trend`: a drag whose last three requests each stepped forward at most 0.5 s decodes with `Hint::Forward {
  fps }`, not `Hint::Scrub` (`decode_hint`) — unless a layer plays backward (reading ahead would restart its
  run each frame and trip the `Busy` thrash guard) (`unless a layer of the plan plays backward`).
- Start from: `draft.rs`, `pace.rs`, `a_forward_drag_reads_ahead_unless_a_layer_plays_backward`,
  `drive_tests.rs`.

## GPU playback (A4, `gpu_preview/play.rs`)

- `hint: "forward"` is a play session: the webview stays the clock; requests use the same latest-wins mailbox
  and `client`/`seq` order, do not feed `Pace`, show with vsync; skip, never slow down (`the webview stays the
  clock`). No hint besides `forward` / `ready` is accepted; `forward` without `client` and `session` is an
  error (`is accepted, and`).
- The first request of a session number decides for the whole session: `renderer: "gpu"` or `"stream"` with
  reasons; stream when the preview is off, no surface/bounds, something drawn over a surface above it, no
  plan, backend not up, self-test not passed, or anything in the next 5 s (`HORIZON`) is not drawn or not in
  time (`PlayCut::ahead`) (`which decides for the whole session`). Under Proxy only it is rejected as the
  stream is (`proxy_wait_from`).
- Each `gpu` answer carries `play: {session, start, frame, repeat, ahead?}`, `ahead` = `{at, handover, kind,
  why}` (`SpanPlan::handover`), so the page hands over before the first undrawable frame; a frame at or after
  `play.ahead.at` is never attempted, whatever the page asks, so a late hand-over cannot run a reversed clip
  or a moving title (`is never attempted`).
- A session ends on: project change (plan epoch), another session, a request not part of it, a replaced
  backend, the setting off, `IDLE` (5 s), or a frame that cannot be drawn (`renderer: "stream"` +
  `play.ahead`); a request over 1 s behind starts over; a request in the frame on screen is `repeat` (`the
  session also`).
- A failed play frame leaves the surface up (`hide_on_failure: false`) so the last GPU picture stays until the
  stream's first frame; a scrub's failed frame still hides it. The session is out of its lock while a frame is
  drawn; a panic in a step is caught and is the stream's (`out of its lock while`).
- Before the first frame the clips at the playhead pass `CursorFeed::check` (untrusted seeks, unknown layout),
  so an unrunnable clip is the stream's with the surface untouched (`CursorFeed::check`).
- A session nobody asks for is ended: `kerf-gpu-prime` reaps it after `IDLE`; `keep_play` stores with the slot
  held and re-reads setting and backend, so a teardown during a frame is not undone; `prime` refuses while a
  forward request is served (`serving`) or a session is held (`keep_play`).
- Prefetch thread `kerf-gpu-play-prefetch` looks 2 s ahead: opens cursors, rasterises titles
  (`Compositor::warm_text`), makes floating SVG pictures (`warm_svg_ahead`); only for frames the compositor
  draws and only as many as the cache holds in order (`Warmed::NoRoom`); with over six clips the feed keeps
  those wanted **first** (`CursorFeed::open_planned`, `Opened::Skipped` / `Opened::Full`).
- A session's end waits for that thread (`FEEDER_JOIN`) so the device is released before the next frame, not
  by a detached thread at process exit (lavapipe shader-compiler crash); tests' `Turn` (`busy.rs`) waits too
  (`FEEDER_JOIN`).
- Priming (`prime.rs`, `kerf-gpu-prime`): after 300 ms (`PRIME_AFTER`) of a paused GPU frame as last request a
  `PlayCut` is made and the frame's cursors opened; Play adopts it (`take_primed`) only from `[at - 0.3, at +
  0.15]`, same plan epoch and backend; scan, surface, self-test and plan are asked again at adoption, so
  priming decides nothing; a stream-bound cut is not primed; dropped by anything not a play from that
  playhead, project change, backend/setting going, or `PRIME_TTL` (10 s) (`PRIME_AFTER`).
- Taking a play back (`ready_play`, `hint: "ready"`): ends any session and primed one, asks what a session
  asks, makes `PlayCut::resume(t, REACH, HORIZON + 0.5)`, primes it, answers `renderer: "ready"` with
  `play.resume`, draws nothing, surface untouched; out-of-order requests are superseded; the page backs off
  while answers are `stream` (1, 2, 4, then 8 s) with a separate count for take-backs (`Taking a play back
  from the stream`).
- Cursor timeouts are `play_config()` (10 s first frame, 3 s next), shorter than export's so a hung decode
  ends in the stream's hands (`play_config()`). `play_tests.rs` (`#[ignore]`d) drives sessions without a
  window.

## Surface techniques and platform notes

- `resolve_technique`; `KERF_GPU_SURFACE=off|window|child` overrides (`Two surface techniques`). `window`:
  wgpu draws on the main window's surface under a transparent webview (`set_background_color` alpha 0 at
  runtime, restored when off). Tauri's `transparent: true` is deliberately not used on Windows (softbuffer GDI
  path fights a swapchain).
- Linux/X11 uses `child`: a borderless child X11 window (`x11rb`, empty Shape input region) over the panel,
  because WebKitGTK composes into the toplevel and `window` loses to GTK's repaints; Wayland has none
  (`Child::create` refuses non-Xlib) so the JPEG with that reason (`Child::create`).
- A child window sits above the page, so `routePreview` (frontend, pure) sends a frame needing a title box,
  trim monitor or safe-area guides to the JPEG, as does a dialog/menu/drag ghost over the frame (`covered`:
  3x3 `elementFromPoint` every 150 ms and after each click or key) (`*above* the page`).
- macOS has no technique by default; `KERF_GPU_SURFACE=window` forces it for a developer (may cover the
  window); the user-visible note does not name the variable (`macOS has`).
- `KERF_GPU_ADAPTER`: unset/`hardware` refuses a CPU rasteriser (lavapipe, WARP) and says so in status;
  `software` forces CPU; `auto`/`any` takes whatever; `KERF_GPU_SELFTEST=off` skips the first-use check
  (`picks the adapter`).
- Forward 1x playback is the FFmpeg stream unless the GPU plays the session; while streaming the surface is
  hidden and shown again on the settled frame (`is a JPEG route`).
- The Xlib `Display` is the process's own, opened once: `tao`'s `display_handle()` calls `XOpenDisplay` each
  time and never closes it, so it is not used (only the window id comes from the handle); every child-window
  request is `check()`ed; `RustConnection::connect` misses the abstract socket (WSL), Kerf's `connect` tries
  it first (`is the process's own, opened once`).
- macOS presenter (WP-8, from source reading only): wgpu's Metal surface asserts the main thread while
  `build_backend` runs on `kerf-gpu-build`, so `surface_on_main_thread("macos")` opens the device off-thread
  and makes only the surface through `on_main_thread` (5 s deadline); wgpu adds a `CAMetalLayer` sublayer
  after WKWebView's layer, so it should draw **above** the page and is never removed (drop leaves the last
  drawable, a rebuild stacks another); `data-surface-hole` in `Preview.svelte` arranges see-through (`macOS
  presenter (WP-8)`). Plan: `.claude/plans/macos-presenter.md`.

## Detached panels (`popout.rs`)

- Panels run in the editor window's JS realm (dockview popout via `window.open`): `editor` / `ui`, transport
  clock, Web Audio engine and every `Channel` are the same objects in every window, nothing is synchronised
  (`nothing is synchronised`).
- The main window is built in code (`create: false` in `tauri.conf.json`, `popout::create_main_window` from
  that entry via `from_config`; a Rust test pins it), because only a builder can carry `on_new_window`
  (`create_main_window`).
- The handler answers only a window the page announced (`popout_expect`, `PopoutQueue`: in order, 15 s
  time-out, max 32 waiting) and only for `/popout.html` on the editor webview's own scheme, host and port
  (`is_popout_url`); anything else is denied (`window the page announced`). The window is built with
  `window_features` so it is scriptable; `place` is pure and moves an off-screen rect onto a screen; WebKitGTK
  ignores `window.open` features so the rect comes from the announcement (`window_features`).
- Closing is per platform: WebKitGTK `close` destroys the widget only (hook connects `destroy` to the window),
  WebView2 destroys the window, WKWebView has no `webViewDidClose:` so the page calls `close_popout`; the
  editor window going away or reloading destroys every popout (`on_window_event`, `on_page_load`);
  `popout-closed` is emitted (`Closing**, per platform`).
- Linux: `javascript-can-open-windows-automatically` defaults false, so `with_webview` turns it on for the
  editor window (`javascript-can-open-windows-automatically`).
- No capability or CSP change: a popout is served by the same `tauri://` protocol under the same CSP (`so the
  same CSP applies`).
- `frontend/static/popout.html` must be a real file (the static fallback answers with `index.html` and starts
  a second editor); `+layout.svelte` refuses to boot when a window has an opener (`must be a real file`).
- dockview >= 8.4.1 is required: 8.3.1 refuses non-http(s) popout URLs (every packaged Linux/macOS build) and
  only 8.4 polls the popout's `closed` flag (`dockview ≥ 8.4.1`).
- `popout.svelte.ts` moves a window that opened off by the read-vs-set error once (`positionCorrection`, pure)
  so a restored window does not creep; positions are logical pixels clamped against `available_monitors`.
- Tauri's drag-drop handler (`dragDropEnabled`, on for file drops) makes WebView2 never deliver in-page HTML5
  `dragover` / `drop`, so in-page drags must be pointer drags (`DROPEFFECT_NONE`).
- `gpu_preview.rs` hard-codes the `main` window, so a Preview in a popout takes the JPEG path (`detached`
  reason) and hides the surface (`hard-codes the`).

## Open ends the full file states

- Packaged CSP checked only on WebKitGTK under Xvfb; WebView2 / WKWebView packaged builds not run (`are still
  to run`).
- macOS Finder open (`RunEvent::Opened`) and `fileAssociations` are not handled (`neither is handled`).
- Not verifiable without a display: whether the first visible frame is painted when `show()` lands, and the
  timer-vs-frame race per OS (`Not verifiable without a display`).
- Windows `window` GPU technique unconfirmed; macOS presenter never run on a Mac (`unconfirmed`).
- Popouts not verified outside Linux/WSLg: WebView2 `NewWindowRequested`, WKWebView `close_popout` /
  placement, mixed-DPI `screenX`, `requestAnimationFrame` in a minimized window, drags between windows (`Not
  verified outside Linux/WSLg`).
- The < 33 ms GPU scrub target is for a real GPU and is unmeasured (`unmeasured`).
