# macOS presenter (gpu-compositor WP-8): plan, measurements, acceptance

Nothing here has run on a Mac. The findings it rests on, and what is already built (the main-thread
surface, the platform note), are in `.claude/docs/app.md` (*macOS presenter*). Read from tauri 2.12,
wry 0.57, tao 0.37, wgpu-hal 30 and raw-window-metal 1.1. If WP-2 (the canvas presenter) ships and
performs, step 2 is not needed.

## Step 1: what the existing code does on a Mac (minutes)

`KERF_GPU_SURFACE=window`, *Settings › Preview › GPU preview* on, a clip under the playhead.

* **It may cover the window.** The source predicts the Metal layer over the page and left behind
  (app.md, (b) and (c)). With the page covered the Settings toggle cannot be reached. To get out:
  quit (the native menu bar is not covered, so Cmd+Q works) and relaunch **without**
  `KERF_GPU_SURFACE`; macOS then has no technique and builds no surface, so the toggle may stay on.
  To clear the setting itself, quit and set `"gpu_preview": false` in
  `~/Library/Application Support/ch.orellbuehler.kerf/settings.json` (`ch.orellbuehler.kerf.dev`
  for a debug build), then relaunch.
* **Record:** the status line (`window`, `ready`); whether the picture is in the frame with the page
  around it or over everything; the layer order (Xcode's View Debugger); the screen after the
  setting goes off; after a second on/off (stacking).
* **Expect console noise.** wgpu-hal's `acquire_texture` walks up from the layer to its delegate
  and reads `NSWindow.occlusionState`, and `configure` sets `CAMetalLayer` properties, both from the
  render thread; the Main Thread Checker (Xcode scheme) may flag them. That is wgpu's, and a reason
  step 2 should keep the layer out of the content view's tree.
* If, against the source, the layer is under the page and gone when the setting is off, make
  `window` macOS's technique (one arm of `resolve_technique`) and stop.

## Step 2: an app-owned layer (if step 1 fails as predicted)

* **Where:** `crates/kerf-app/src/gpu_preview/macos.rs` beside `x11.rs`, `#[cfg(target_os =
  "macos")]`, a `Host::Layer` variant. Dependencies under
  `[target.'cfg(target_os = "macos")'.dependencies]` at the locked versions (objc2 0.6,
  objc2-app-kit 0.3, objc2-quartz-core 0.3, objc2-foundation).
* **The layer is ours.** `CAMetalLayer::new()` handed to wgpu as
  `SurfaceTargetUnsafe::CoreAnimationLayer` (a `Gpu::create_surface_unsafe` wrapper is the only
  kerf-gpu change), so wgpu never touches the content view. It lives in a layer-hosting `NSView`
  (`setLayer`, then `setWantsLayer`) added to the content view `NSWindowBelow` the WKWebView,
  frame = the Preview frame (the `Child` model: surface = frame, no full-window fill, nothing to
  hit-test, the page draws over it through its hole). Fallback: the whole-window surface with
  `zPosition = -1` on the layer.
* **A third `Technique` variant** (`Layer`): `Window` is a whole-window surface and `Child` has
  `overlays() == false`, and this is a frame-sized surface under the page. It needs:
  `Technique::{name, overlays}` (`"layer"`, true); `layout_for` (the `Child` arm: `Layout.window`);
  `GpuPreview::follow` (today only `Technique::Child` follows a move); `Host::{apply, follow, hide,
  release, window_size, make_transparent}`; `build_backend`; `resolve_technique` (the `macos` arm
  and the `KERF_GPU_SURFACE=layer` spelling); `GpuPreviewStatus.technique`; `types.ts`
  (`technique: 'window' | 'child' | null`); `SettingsDialog.svelte` (the two label ternaries and
  the `=== 'child'` block); `Preview.svelte` (its three `technique === 'window'` / `!== 'window'`
  tests: the hole, the transparent root and the surround layer apply to `layer` too); and the
  tests that pin the technique table.
* **Main thread:** AppKit calls go through `on_main_thread` (create, apply) or a posted task
  (`follow`; `hide` = `setHidden`; `release` = `removeFromSuperview` and drop the layer).
* **Scale:** `contentsScale` = `backingScaleFactor` at creation and on
  `NSWindowDidChangeBackingPropertiesNotification`; `drawableSize` through `Presenter::resize`.
* **Occlusion:** wgpu's guard finds the window by walking `superlayer()` up to the first ancestor
  with a delegate. A layer in a layer-hosting view may have none up its chain, so the guard may be
  skipped and `nextDrawable` can stall (wgpu issue 8309). Own it: observe
  `NSWindowDidChangeOcclusionStateNotification` and hide / skip frames, and put a watchdog on the
  present (`attempt` holds the render lock while presenting).
* **Frame math (pure; write it first, with its table, beside `layout_for`).** The rectangle is
  webview device pixels, top-left origin. `r = scaled(rect, viewport, inner_size)` (as
  `Technique::Window` does), then `frame_pt = (r.x/s, H_pt - (r.y + r.h)/s, r.w/s, r.h/s)` with
  `s = backingScaleFactor` and `H_pt` the parent's `bounds.height`; the drawable is `(r.w, r.h)`
  whole pixels. Cases: s = 1, 2, 1.5; a page zoom (viewport != inner size); an overhanging
  rectangle; a 1 px frame.

## Measure (Apple Silicon, and an Intel Mac with a dGPU if there is one)

* Start-up to the first frame (`BUILD_DEADLINE` is 10 s; Metal shader compilation is the unknown).
* `present_ms` / `composite_ms` at 960 and 1920 px, against lavapipe's.
* Stalls in `nextDrawable` (it cannot time out: wgpu-hal sets `allowsNextDrawableTimeout(false)`),
  with the window occluded, the display asleep, a Space switch, full screen. A stall holds the
  render lock.
* A resize drag (`presentsWithTransaction` if the picture lags the frame).
* One test card through the JPEG route and the GPU route: an sRGB layer against a colour-managed
  image; if they differ, set the layer's colour space explicitly.
* `KERF_GPU_ADAPTER=software`: macOS has no software adapter, so it must end in the JPEG with a
  reason.

## Acceptance

* The picture sits in the frame with titles, safe-area guides and the trim monitor drawn over it.
* A dialog or a menu over the frame; the Settings toggle always reachable.
* A dock sash drag, window resize, full screen, a move between displays of different scale and a
  page zoom keep the picture on the frame to the pixel.
* Off and on leaves exactly one layer or view and no stale picture; a rebuild after a lost device
  does not stack another.
* An occluded or minimized window falls back to the JPEG and recovers; a presenter that stalls
  (occlusion, display sleep) neither freezes the preview nor holds the render lock past a bound
  (the occlusion check and the watchdog above).
* The ignored parity suite passes with `KERF_GPU_ADAPTER=hardware` on both FFmpegs; record PSNR and
  max error against lavapipe's.
* An ignored `macos.rs` test presents a known frame into an offscreen window and compares a window
  capture with `RenderedFrame::read_back`, as `x11.rs` does against the X server.
