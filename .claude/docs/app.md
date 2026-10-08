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
`set_keyframe_easing { clipId, time, easing, prop? }` (the key within a millisecond of `time`; with
`prop`, that one number's), `set_property_keyframes { clipId, prop, keys }` (one number's own keys; none =
static) / `copy_keyframes { fromClipId, toClipId, props?, offset? }`,
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
`get_preview_frame` / `set_preview_bounds` / `gpu_preview_status` (the GPU preview, below; GUI-only),
detached panels (`popout_expect` / `popout_cancel` / `popout_focus` / `popout_move` / `close_popout`, see Detached panels below; GUI-only, no MCP tool),
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

**GPU preview (A2, `gpu_preview.rs`, opt-in).** *Settings › Preview › GPU preview (experimental)*
(`Settings.gpu_preview`, default off, persisted beside `safe_areas`) draws the Preview
panel's frame with `kerf-gpu`'s compositor in a native surface. Off, none of it runs and the
webview sends no extra command: the JPEG path is what it was. On, `get_preview_frame
{ time_secs, max_width, overlays }` takes the plan's inputs under the project lock
(`plan_inputs`: the working timeline, the assets as imported and the proxy-swapped ones the
FFmpeg graph reads), **releases it**, plans the frame (`Planner` over `ProxyMedia`, the
measured colour policy) and answers `{renderer: 'gpu' | 'ffmpeg', frame?, reasons,
timings?}`: the GPU when `RenderPlan::reasons(caps, size)` is empty and everything works,
else FFmpeg's JPEG **of that frame, in the same call**, with the plan's reasons (a refusal
is not an error). The device, compositor, `FrameSource` and presenter are built lazily by
the first frame that wants them and dropped together (`Backend`); a build failure is
remembered with a backoff (5 s doubling to 5 min, `Backoff`), a `DeviceLost` or any error
that says the device or surface is gone (`react`) drops the backend and rebuilds it on the
next use — the first loss at once, as `kerf-gpu` documents: the owner builds a new `Gpu` —
and a refused plan, a busy decode or an occluded window is only that frame's fallback. **Nothing
here may freeze the preview**: a backend is built **off the render lock** and with a deadline
(`BUILD_DEADLINE`, 10 s, on a thread of its own; a frame that finds a build under way is the
JPEG at once, "the GPU preview is starting", and one that outlives the deadline is left to finish
and dropped), a panic anywhere in the GPU path (`frame()`'s `catch_unwind`, and the build thread's)
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
Settings dialog opened *under* the picture before this existed); the in-frame badge,
resolution and timecode are under it too (the transport bar has the timecode). **macOS has
no technique in this build**: a surface under the webview needs a transparent window, which
Tauri gates behind `macos-private-api` (a private WKWebView key, and a feature that changes
every macOS bundle); it is not enabled blind (`KERF_GPU_SURFACE=window` forces the attempt).
`KERF_GPU_ADAPTER=software` takes the CPU adapter (lavapipe, WARP) instead of the machine's GPU.
Playback (forward 1×) is still the FFmpeg stream: `streaming` is a JPEG route, the surface
is hidden while it runs and shown again on the settled frame (A4; scrubbing at GPU speed and
live drags are A3's). `x11.rs`' ignored tests present to a real child window and read the
pixels back **from the X server**, destroy the device and draw again on a new one, and reach a
server that listens on the **abstract socket only** (an `Xvfb` where `/tmp/.X11-unix` is not
writable, WSL's case) at once: `RustConnection::connect` tries the filesystem socket and then TCP
and never the abstract socket libxcb tries first, and waited out a TCP timeout of minutes
(`connect` tries it first). **The Xlib `Display` is the process's own, opened once**: `tao`'s
`display_handle()` calls `XOpenDisplay` anew on every call, never closes it and `new_unchecked`s a
null one, so its handle is not used — only the toplevel's window id comes from the window handle —
and one display is kept for the whole life of the process however often the backend is rebuilt.
Every request on the child window is `check()`ed, so a server that refuses it falls back promptly.

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
