# The GPU preview on by default: readiness audit, what is built, and what settles the flip

State: **built behind one constant, left off.** `GPU_PREVIEW_DEFAULT_ON` in `crates/kerf-app/src/settings.rs` is
`false`. Flipping it is that one line (plus the label, below). The reasons it is not flipped are in "Why it is not
flipped"; the test that settles it is in "The ten-minute script".

## What decides whether the GPU draws a frame

Page (`routePreview`, `Preview.svelte`): the setting **in force** (`Settings::gpu_preview_in_force`: the user's own
word, else `GPU_PREVIEW_DEFAULT_ON` where the platform has a surface technique, and not a `crash` block) -> the platform
has a technique (`status.supported`) -> the panel is in the editor window -> not empty, not streaming -> nothing the
surface (above the page on X11) would paint over (`overlays`, `covered`). Anything else is the JPEG route, unchanged.

Backend (`GpuPreview::attempt_in`), in order, each step a JPEG of the same frame in the same call when it says no:
enabled -> technique -> bounds reported and visible -> overlays -> plan (`Planner`, the probed colour policy) ->
`kerf_gpu::prepare` probes -> `ensure_backend` (backoff 5 s to 5 min; the build off the render lock, `BUILD_DEADLINE`
10 s; the adapter is hardware only: a CPU rasteriser such as lavapipe or WARP is `NoAdapter`) -> surface size and place
-> `RenderPlan::reasons(caps, size)` empty (the compositor draws it exactly) -> the self-test gate (this adapter, driver
and FFmpeg passed `parity_self_test`) -> `FrameSource` trust gates and decode -> render -> present. A play session
(`hint: forward`) is decided by its first request and by what the next 5 s hold (`PlayCut::ahead`).

## The audit

"Way back" = what ends the user on a correct FFmpeg picture with nothing done by them.

| Case | What happens | Way back | Status |
| --- | --- | --- | --- |
| Per-frame refusal (`Unsupported`, plan reasons, busy decode, occluded window) | that frame is FFmpeg's, in the same call | automatic | fine, by design |
| Device lost / out of memory (driver reset, TDR) | `react` -> `Rebuild`: backend dropped, surface hidden, the frame is the JPEG; the first loss is rebuilt at once, then 5 s doubling to 5 min, forgiven only by 20 presented frames; a play session hands to the stream | automatic | fine (tested: `a_device_that_dies_after_every_frame_is_not_forgiven...`) |
| Surface lost / outdated (monitor change, sleep, RDP reconnect) | the presenter reconfigures once and retries; else `GpuError::Surface` -> `Rebuild`; `Timeout` / `Occluded` skip the frame | automatic | fine, **never run on a real window** |
| Driver crash while the backend is made or on the first frame (process dies), or a build that never answers | attempt marker -> the next launch blocks the preview for this machine (**now** a `GpuBlock`, not a silent `gpu_preview: false`), says so once. A build that never answered keeps the marker across a clean quit (`build_stalled`) | automatic | fine |
| Driver crash **after** the first frame (mid-session) | nothing, before this work: the next launch tried again, crashed again, for ever | **was a blocker**; now the session marker: two unclean runs in a row block it. A run that ends on purpose is not one: `RunEvent::Exit`, and the Windows updater (which ends the process with `std::process::exit(0)`) through `gpu_preview_leaving` before the install | fixed, tested; one crash still happens |
| Adapter passes the self-test, then **stalls** (a decode or driver call that never returns) | play session: given up after 1.5 s (5 s for the first answer) and handed to the stream. Paused / scrubbed frame: **was a blocker** (the page awaited the one worker for ever, the picture froze) | now a request unanswered for 8 s is the JPEG from the command that does not use the worker, and so is every frame until the late answer comes | fixed in the page, tested. The worker itself is not recoverable in the process (a thread in a driver call, possibly under the render lock), so switching the setting off and on does **not** retry it: Settings says to restart Kerf, and a force-quit counts a strike. Until the backend has drawn a frame the limit is 90 s (build 10 s, probes 45 s, first decode 30 s) with the JPEG standing in provisionally at 8 s and nothing latched, afterwards 8 s; a legitimately slow frame in the middle of a session can pass 8 s and then costs a JPEG for it, and the late answer brings the GPU back |
| Self-test mismatch | gate closed for the session; **now** kept in the settings for that adapter + driver + FFmpeg binary (path, size, mtime: not its version) + app version, closed from the first frame at the next launch; a driver update, a replaced FFmpeg or a new Kerf is checked again | automatic | fixed |
| Self-test could not be made (no FFmpeg fixtures, policy unknown, deadline) | closed for the session, retried by the next launch | automatic | fine (deliberately not kept) |
| No adapter / CPU-only (VM, RDP session with the basic render driver, lavapipe, WARP) | `NoAdapter` with the reason in Settings; JPEG; backoff 5 s to 5 min, so a failed attempt costs one adapter enumeration every 5 min at most | automatic | fine; deliberately not kept (the adapter set differs per session: RDP vs console) |
| Virtual GPU that reports a real device type (virtio, VMware SVGA) | taken as hardware: it would pass the self-test and present | none: no evidence the surface shows | **unverified** |
| Hybrid-GPU laptop | surface made first, adapter asked to be compatible with it, high-performance preferred; on Linux the iGPU is picked when the dGPU cannot present to the X screen | automatic where wgpu says no | **unverified** (Windows cross-adapter present, the dGPU woken for a preview: battery cost unmeasured) |
| Linux, X11 | borderless child X11 window over the panel (`x11rb`, empty input shape) | n/a | confirmed **only** under WSLg with lavapipe, which the default adapter policy refuses; **never on a real X server with a real GPU** |
| Linux, Wayland | `Child::create` refuses a non-Xlib handle: status reason, JPEG; retried every 5 min (cheap, one log line). **The default is not applied** to a Wayland session (`linux_windows_are_x11`: `GDK_BACKEND`, else `WAYLAND_DISPLAY` / `DISPLAY`), so nothing tries and the switch does not show on | automatic | fine |
| macOS | `resolve_technique("macos")` is none: `supported = false`, and the default is not applied there (`gpu_preview_default_for`) unless a developer sets `KERF_GPU_SURFACE=window`, which forces a technique | n/a | harmless; the presenter has never run on a Mac |
| Windows, WebView2 hosting (`window` technique) | the swapchain is the main window's, the webview is made transparent (`set_background_color` alpha 0) as the last build step and restored by a guard | **none: nothing can tell that the surface is visible through the webview**; the self-test checks what the compositor draws, not that the page shows it. A frame that is `gpu` and black has no detector | **blocker for the flip, cannot be fixed without a Windows machine** |
| WebKitGTK hosting | page composes into the toplevel; the child window sits above it, so frames that need a title box, guides, the trim monitor, or have a dialog over them are the JPEG | automatic | fine where the child window shows (see above) |
| Colour management | the JPEG goes through the webview's colour management (WebView2 reads the display profile); the native surface does not | none | **unverified**: a wide-gamut display may show the GPU picture more saturated than the JPEG |
| Cost of first use | the first GPU-able frame starts the self-test (FFmpeg children, thread-capped; **5.2 s** here on lavapipe with the pinned FFmpeg 9.0.2, on a box busy with builds, six frames) and `prepare` probes (each bounded by 20 s); every frame until then is the JPEG | automatic | fine, but with the default on **every user pays it** on their first project; unmeasured off lavapipe |

Verdict: the machinery around the GPU path (failure, crash, stall, mismatch) now ends in FFmpeg's picture without the
user doing anything on every path that can be tested without hardware. The two things it cannot detect are
**whether the surface is visible through the webview on Windows** and **whether its colours match the page's**. Both
produce a confident-looking `renderer: gpu` frame that is black or off, with no automatic way back.

## What is built

The mechanism is described where the code is, in `.claude/docs/app.md` ("GPU preview (A2)": the setting and its
default, `GpuBlock`, the markers, the self-test's kept mismatch) and `.claude/docs/frontend.md` ("GPU playback in the
page": the stuck-worker guard, the open ends at the switch). Two things belong to the flip and are said here:

* `legacy_unchosen_false_follows_default()` in `settings.rs` is the owner's call: version 0.31.0 wrote
  `gpu_preview: false` for everyone who ever saved a setting, so a stored `false` with `gpu_preview_chosen` false is a
  default written down for most and a choice for a few. It stays distinguishable from a choice made since, and the
  function returns `false` (keep it off), so flipping the constant reaches fresh installs and files with no value,
  not an existing install that merely saved a setting. Returning `true` re-enables the few who turned it off on purpose.
* A block lapses with the app's version, so a crash or a mismatch found by 0.32.0 is forgotten by 0.33.0 (a fix may
  be in it); it also lapses with any write of the setting by the user.

## Why it is not flipped

Nothing about the GPU preview's **surface** has run on a real GPU: every parity case and every drive test is offscreen,
on lavapipe, and the one surface run (WSLg, child window) used lavapipe, which the hardware-only default refuses.
`< 33 ms` for a scrub is "for a real GPU and is unmeasured". A default is a promise to every user that the first frame
looks right; the checks that guard it cannot see the two failures above, and they are the ones that would show on the
owner's own machine. The flip is one line once the script below passes.

## The flip

1. `GPU_PREVIEW_DEFAULT_ON = true` in `crates/kerf-app/src/settings.rs`.
2. The Settings row's label loses "(experimental)" (`SettingsPreview.svelte`), the hint says "on by default".
3. Release note: the preview is on for anyone who never chose; it turns itself off on a crash or a failed check, and
   Settings > Preview turns it off or on. Say that **a block lapses with each app version**: a machine that crashed
   or failed its check on one version tries the preview again on the next, once.
4. Optionally the legacy `false` reading above.
5. Recommended with the flip: a one-time hint in the **status bar** (outside the surface: a child window covers
   anything the page draws over the picture) for a user who never chose, "GPU preview is on. Not right? Switch it off",
   because the two failures nothing detects (a surface that is not visible, colours that differ) are exactly the ones
   with no automatic way back. UI work, so screenshots at 1920x1080 and 1366x768 in both themes.

## The ten-minute script (Windows + AMD RX 7900 XT first; then Linux X11 on a real GPU; macOS only to confirm nothing changes)

Use a PR installer, GPU preview switched **on** in Settings > Preview (the constant is off, so this is the explicit
choice). Project: 1080p or 4K footage, two clips with a cut, a title, a 0.5 s fade. Write each answer down.

1. **Does the surface show (2 min).** Open the project, put the playhead on a clip. The status bar must read
   `GPU WxH · n ms · <your adapter>` (not `FFmpeg · ...`) and the pane must show the picture. A black or matte-coloured
   pane with `GPU` in the status bar is the Windows blocker: stop, note the OS build, WebView2 runtime version, display
   scale, and take a screenshot. Resize the window, drag it to a second monitor if there is one, minimise and restore:
   the picture must come back.
2. **Colour (1 min).** On a saturated frame (a red or a skin tone), switch the GPU preview off and on in Settings
   several times. The picture must not change brightness or saturation. On a wide-gamut or HDR display say so.
3. **Scrub (2 min).** Drag the playhead across 30 s of 1080p and across 4K footage. The status bar's ms (decode +
   composite + present) should be under 33 on the 7900 XT; note the typical and worst value, and whether the picture ever
   freezes or flashes black.
4. **Playback and the switch (3 min).** Play from before the title through the fade. The GPU should play, hand to the
   stream at what it cannot draw (the status bar changes), and take the play back after. Note any jump, black flash or
   repeated frames at each switch; with a click track or a clap, whether picture and sound stay together (try Bluetooth
   output if you have it: that is the largest output latency).
5. **The way back (2 min).** Quit Kerf. In the folder that holds `settings.json` (Settings > Advanced > Show file; `%APPDATA%\ch.orellbuehler.kerf` on
   Windows, `~/.config/ch.orellbuehler.kerf` on Linux) create an empty file `gpu-preview-attempt` and start Kerf: expect a
   toast "GPU preview was turned off", the switch off, Settings > Preview naming the reason, and an ordinary preview.
   Switch it on: it works again. Then the real thing: with a GPU frame showing, kill Kerf from Task Manager (not Quit),
   start it, get a GPU frame, kill it again: the **third** start must come up with it blocked and the toast.
6. **Report** the table below, the log lines `GPU preview is up` (adapter, backend, technique) and the first
   `self-test` verdict from the log file (Settings > Advanced > Open logs; the same folder holds the log files to attach).

| Machine | Surface visible | Colour same | Scrub ms typical / worst | Playback switches clean | Way back works |
| --- | --- | --- | --- | --- | --- |
| Windows 11, RX 7900 XT | | | | | |
| Linux X11, real GPU | | | | | |
| Linux Wayland | (JPEG with a reason expected) | | | | |
| macOS | (nothing changes expected) | | | | |

The flip is settled when the Windows and Linux X11 rows are all yes, scrub is under 33 ms at 1080p, and the way back
works on both. A single "surface not visible" or "colour differs" on a machine is a no for that platform: the
default would then have to be set per platform (`gpu_preview_default_for` takes the OS already) instead of for all.
