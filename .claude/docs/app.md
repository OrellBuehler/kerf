# kerf-app (`crates/kerf-app/src/lib.rs`, `main.rs`)

Tauri v2 shell. **CSP is on** (`app.security.csp` in `tauri.conf.json`, an object so Tauri can add its hashes): `default-src 'self'`, scripts `'self'` only (Tauri hashes SvelteKit's inline bootstrap in the fallback `index.html`), styles allow `'unsafe-inline'` because the UI is styled with inline `style` attributes plus the Google Fonts stylesheet host, fonts add `fonts.gstatic.com`, images `data:` (frames are data URLs), `connect-src ipc: http://ipc.localhost`, no objects or `<base>`. Anything new that loads from the network or a `blob:` has to be added there deliberately. **Panics log a backtrace** (`install_panic_hook` forces capture; the release profile strips only `debuginfo`, keeping the symbol table so frames carry function names at a modest size cost). **One instance per identity**: `tauri-plugin-single-instance` is the first plugin in `run()`. A second launch focuses the running window (unminimizing it) and, when its argv carries a `.kerf` path (resolved against the second launch's cwd by `project_arg`), emits `open-project-file` to the webview, which asks about unsaved work like any other open and calls `open_project`. **The first launch's own argv is honoured too**: `run()` resolves it through the same `project_arg` (`launch_project`, pure — the `exists` check is a parameter) against the process cwd into `AppState.launch` (a `LaunchSlot`), and the webview **pulls** it once with `take_launch_project` after its listeners and first `editor.load()` are in place — a command, not an event, because an event emitted before the page has a listener is lost. It returns `{open: path}` once (a reloaded webview must not reopen it over the user's edits) and goes through the same `openProjectAt` as `open-project-file`, unsaved-work question included. **A second launch that arrives while the webview is still booting** would hit the same lost event, so until the webview has asked the slot *holds* its request instead (newest wins; one lock covers both halves, so a request is delivered exactly one way) and only afterwards is it emitted. A `.kerf` argument naming nothing on disk is never opened — `Project::open` would *create* it — and comes back as `{missing: path}` (second launch: the `launch-project-missing` event), which the page toasts as `File not found: …`. (A macOS Finder open arrives as `RunEvent::Opened`, not argv, and no `fileAssociations` are configured; neither is handled.) **The main window starts hidden** (`visible: false`, with `backgroundColor` = Kerf Dark's `surface-app`, which a bun test pins equal to `app.html`'s paint): the webview calls `show_main_window` once the settings are in (theme applied, dock built) and a frame has painted (`reveal.ts`: two animation frames *or* a 150 ms timer, since a hidden page may never get a frame), and a 3 s `REVEAL_FAILSAFE` thread shows it anyway so a crashed bundle cannot leave an invisible app. `reveal_once` over `AppState.main_window_shown` makes the first asker win and the rest no-ops (a timer firing after the user minimized the window must not pop it back up) — but only a `show` that *worked* (a window existed and `show()` succeeded) keeps the claim, so a request that finds no window gives it back and the failsafe retries every 500 ms. A second launch always brings the window forward (`unminimize` + `show` + `set_focus`) and marks the reveal done only if that worked — it can run before the config windows exist, and then the failsafe is still armed. It is a command rather than the window API so the capability needs no `core:window:allow-show` (a Rust test pins its absence), and the debug identity cannot diverge: `tauri.dev.conf.json` merges only `identifier` (also pinned). **Not verifiable without a display**: whether the *first visible frame* is already painted when `show()` lands (a hidden window may not render until shown, which is why `backgroundColor` exists) and how long the timer-vs-frame race takes per OS — check both on a real desktop. `lib.rs::run()` is the entry (`main.rs` just calls it); it owns the
`Arc<Mutex<Project>>` (cloned into both the Tauri managed state and `mcp::serve`) and
registers a command per `Project` op — reads (`list_assets`,
`get_timeline`, `get_asset_metadata`), `import_asset` / `analyze_asset` (emits
`analysis-progress` per step), speech-to-text (`transcription_status`,
`set_speech_model`, `download_speech_model` → emits `model-progress`), ripple mode
(`get_ripple_mode` / `set_ripple_mode { on }` — both answer the bool, a setting
that records no revision and returns no timeline), voiceover
(`voiceover_status`, `prepare_voiceover` / `generate_voiceover` → emit
`voiceover-progress`, `cancel_voiceover`), every editing
op (`cut_clip`, `add_clip`, `split_clip`, `trim_clip` (optional `timeline_start` so a
left-edge trim keeps the right edge put, atomically), `reorder_clip`, `move_clip`,
`move_clips { moves }` (a group, one revision, all or nothing), `roll_edit { clipA, clipB,
delta }` / `slip_clip { clipId, delta }` / `slide_clip { clipId, delta }` (clamping edit
modes, never ripple), `split_remove { clipId, at, side }` (`"left"` | `"right"`; follows
ripple mode) and `split_remove_clips { cuts, side }` (`cuts: [{clip_id, at}]`, one revision),
`ripple_delete`, `cut_clip_range` (remove a **source-time** span from a clip and
ripple closed — the transcript-editing primitive), `add_track`, `remove_track`,
`set_track_duck`, `set_track_volume` / `set_track_pan`, `set_master_volume` /
`set_master_limiter` (the master bus; each returns the `Timeline`), `get_levels` (`range?`,
`loudnorm?` → `Levels`; whole-file, so lock-free; `cancel_levels` stops it, rejecting with
`levels cancelled`), `set_delivery_format` (the project's delivery frame; omit
width/height to clear it), `remove_clip`, `remove_clips { clipIds, ripple? }`
(one revision; `ripple: true` is the multi-select ripple delete, via
`with_ripple`; omitted follows the project's mode), `set_volume`, `set_fade`,
`set_speed`, `set_transform`, `set_color`, `set_transition`, `set_mask`,
`set_video_effects`,
`set_audio_effects`, `set_keyframes` / `add_keyframe` / `clear_keyframes`,
`set_keyframe_easing { clipId, time, easing }` (the key within a millisecond of `time`),
`set_reframe` / `clear_reframe` / `set_reframe_keyframes` / `add_reframe_keyframe`,
`set_asset_projection` (asset-level 360 mark; returns the `Asset`),
`add_overlay` / `update_overlay` / `remove_overlay` / `set_overlay_keyframes`,
`generate_captions` / `clear_captions` (caption the whole cut, in timeline
time), `import_captions` / `import_captions_text` (a subtitle file by path / by text —
the text variant is what an `<input type=file>` or a paste uses; both return
`{timeline, summary}` rather than a bare `Timeline`), `export_srt`, `remove_silence`, `snap_to_beats`,
`smart_crop` (frame each shot for the delivery frame),
`extract_audio` (detaches an asset's cut clips and reports what it skipped, see Linked A/V) / `add_asset_audio`
(an asset's whole audio as a clip) / `detach_audio_clips`,
`detach_audio` / `reattach_audio` / `reattach_audio_clips` / `link_clips` / `unlink_clips`, `concatenate` — each
returns the refreshed `Timeline`; every edit that carries linked clips takes an optional
`link` (`false` edits the named clips alone: `trim_clip`, `move_clip(s)`, `split_clip`,
`remove_clip(s)`, `ripple_delete`, `cut_clip_range`, `set_speed`, `roll_edit`, `slip_clip`,
`slide_clip`, `split_remove(_clips)`, via `with_links`)), media (`get_frame` → base64 PNG data URL, `get_waveform`,
`get_waveform_range` → a source-seconds window as min/max peaks per channel,
`get_filmstrip` → an asset's thumbnail strip, the `Filmstrip` JSON with each sheet's
JPEG added as a base64 `data:` URL (`FilmstripPayload` — the CSP admits `data:` images
and no `blob:`; core serializes the geometry without pixels) and **no MCP tool**, since
`skim_asset` is how an agent looks at footage,
`start_playback` / `stop_playback` — streamed composited frames over a
`tauri::ipc::Channel`, cancelled **by caller-supplied id** rather than a generation
counter, because start and stop are separate async calls that can arrive out of
order and a late stop must not kill the stream that replaced it —
`get_audio` → a clip window as **raw mono s16le PCM via `tauri::ipc::Response`**, the
only non-JSON command — the preview's Web Audio playback decodes it),
delivery (`export_cover` → a cover image at the full delivery frame,
`platform_targets` / `platform_check` → the readiness verdict, `reveal_path` →
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
`set_settings` → a `SettingsView`: the *effective* CPU budget read back out of
the engine, the cores it works out to, and the machine it is a share of —
`settings.rs` persists them as JSON in the platform config dir, since how much
of *this* computer Kerf may use is not something that should travel inside a
`.kerf` file; `KERF_CPU_PERCENT` wins at launch, a moved slider wins after.
The file also carries the **workspaces** (which one is active, each one's
dock arrangement, the library rail's tab and folded state), the **color theme**,
the **keybindings** the user changed and `layout` — the single arrangement from
before there were workspaces, now only migrated from — as opaque
`serde_json::Value`s: the frontend owns their shape and
validates them on the way back in, so `get_settings` re-reads the file for those
where the engine-held values are read live). **`set_settings` takes a patch**,
not the whole object — only the fields that changed (`{workspaces}`, `{theme}`,
`{keybindings}`, `{cpu_percent}`), merged into the file under a mutex, and only those fields are
pushed into the engine (so a layout write never re-applies the stored CPU share
over a `KERF_CPU_PERCENT` override). The write is atomic (temp file in the same
directory, fsync, rename over), and a file that does not parse is moved aside to
`settings.corrupt-<unix-ms>.json` before defaults load, so the next save cannot
destroy an imported theme), `read_text_file` / `write_text_file`
(a theme file the user picked, imported or exported — the only commands that
read a caller-chosen path, so both take only `.json` paths, refuse a non-regular file, and cap read and write at 1 MiB)
and `agent_status` (the MCP endpoint, an `error` when the server could not bind — the agent panel then says the port is taken instead of showing a dead endpoint — plus how
many seconds ago an agent last spoke to it, or `null` if none ever has —
`mcp::LAST_AGENT_ACTIVITY`, stamped in `lock_agent` and in `get_info`, since
`initialize` is the one moment an agent is known to be there; a
streamable-HTTP client holds no connection between calls, so there is no socket
to report and the panel judges from the age instead of the green dot it used to
show unconditionally). A **failed render deletes what it wrote** (`discard_partial`, in both
`export_timeline` and the MCP `export`): only if this run touched the file
(mtime differs from before), so a failure before ffmpeg opened the output cannot
delete the earlier export sitting at that path. `export_variants` renders its
files one `render_variants` call at a time so a failure names the file in flight —
that one is removed, the finished ones stay. The error carries ffmpeg's stderr
tail. `start_playback` resolves `Ok` for a stop or supersede but rejects with the
ffmpeg error when a stream someone is still watching dies, so the preview can say
why it went black. **Logging** (`init_logging`): stdout plus a daily-rolling `kerf.<date>.log` (14 kept) in
`<app data dir>/logs` — `log_dir_path` is the one place that is decided, shared by
`init_logging`, `log_dir` and `reveal_logs`; if it is not writable the app logs to stdout
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
confirms) + `opener:allow-open-url`. That last
one enables the command **with no scope of its own** (`allow-default-urls` is a
separate permission), so it is listed in object form with an `allow` entry for
`https://github.com/OrellBuehler/kerf/*` — without a scope every `openUrl` call
comes back `ForbiddenUrl` and the "Release page" button silently does nothing.
