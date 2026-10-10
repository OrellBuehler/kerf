# Kerf GPU compositor: where the time goes and what to build next

Scope: `crates/kerf-gpu`, the render plan in kerf-core (`render_plan.rs`, `planner.rs`, `plan_caps.rs`),
`crates/kerf-app/src/gpu_preview.rs`, and the frontend's GPU preview routing. Analysis only; nothing in
the repo was edited. Tags used throughout: **[M]** measured here, **[R]** read from code or docs,
**[E]** estimated (reasoning, not measurement), **[D]** a number from the repo's own docs.

## 0. Headlines

Ranked roadmap (details in section 8):

1. **WP-0 Quick wins that need no GPU (S, days).** (a) Stop sending the playback stream as base64 data
   URLs: raw JPEG bytes through a Tauri raw `Channel` and `createImageBitmap` cost 2.5 to 3 ms per 960x540
   frame in the webview against 5 to 13 ms for `<img src="data:...">` (25 to 45 ms at 1080p) [M, headless
   Chromium]. (b) Cap decoder and filter threads on single-frame stills: the FFmpeg fallback still goes from
   59 to 33 ms (1 layer) and 143 to 76 ms (3 layers) [M]. (c) Play at the project fps, not a hard-coded 24.
2. **WP-1 Text on the GPU (L).** The biggest coverage unlock. In a constructed captioned-talking-head cut the
   GPU can draw 12.7% of frames today; with text it is 96 to 100%. Two blockers, and the second is hidden:
   the text pass itself, and `font: None` (the default for every title and caption) which the plan refuses
   as `TextWithoutFont` even after `caps.text` is switched on [M].
3. **WP-2 Readback to canvas as the universal presenter (M).** Composite on the GPU, read back, ship raw
   RGBA to a `<canvas>`. Works on macOS, Wayland, Windows without confirming a native-surface trick, in
   detached Preview windows, and under HTML title/handle overlays. Expected ~5 to 8 ms of overhead per
   960x540 frame [E].
4. **WP-3 Proxy colour normalisation (S to M).** Under the shipped FFmpeg 9.0.2 policy, any frame that stacks
   an untagged proxy and a BT.709-tagged proxy is refused (`MixedMatrices`) [M with real proxies]. Making
   proxies colour-consistent removes that refusal from preview plans for dissolves and PiP of mixed sources.
5. **WP-4 Motion mode on the GPU: fades, dips, dissolves, slide/push (L).** Prerequisite for playback and
   export, and a fidelity fix: today a paused frame inside a fade or dissolve falls back to an FFmpeg *still*
   that draws no fade at all [R].
6. **WP-5 Scrub at GPU speed (A3) plus hardening (M).**
7. **WP-6 Keyed zoom, masks, blur (M each).** Masks matter most for speed: FFmpeg's `geq` mask costs 93 ms CPU
   per 960x540 preview frame and 402 ms per 1080p export frame [M].
8. **WP-7 GPU playback (A4) (L).** After 1, 4 and a prefetch for cuts (an asset-to-asset cut stalls 63 ms
   today [M]). Frame pacing can stay in the webview (pull model), so no audio-clock port to Rust is needed.
9. **WP-8 macOS native surface (M, needs a Mac).** Try the existing `KERF_GPU_SURFACE=window` first; the
   "needs `macos-private-api`" premise in the docs is probably wrong (section 6).
10. **WP-9 Export through the compositor (A7) (XL).** Saves only the filter-graph share of export CPU:
    25% (1 layer, x264 medium) to ~90% (mask) of per-frame CPU [M]. Do last.

What I could not measure: any real GPU, Windows, macOS, WebKit/WKWebView, Tauri IPC throughput, or real
user timelines. Every "on a real GPU" number is an estimate.

---

## 1. Method and caveats

* Host: WSL2 Ubuntu 22.04 on a shared 16-thread box. **Load average was 10 to 42 for the whole session**
  (other sessions were compiling). Wall times are inflated and noisy; I report minimums, min of 3, or CPU time
  (user+sys) where I could. Treat absolute numbers as "this machine under load", relative ones as sturdier.
* FFmpeg: headline numbers use the **pinned static FFmpeg 9.0.2** in `crates/kerf-app/binaries/` (what the
  bundles ship). The distro FFmpeg is 4.4.2; I use it only where noted. **The parity suite cannot run on
  4.4.2** (its fixture generator needs `-display_rotation`, FFmpeg 6+); with the pinned build all 28 parity
  tests pass in 19 s [M, lavapipe, release build].
* GPU: only Mesa lavapipe (llvmpipe, a CPU rasteriser). `/dev/dxg` exists but there is no Vulkan ICD for it.
  So **`composite record+submit` (CPU-side cost of building a frame) is meaningful; GPU execution time on
  lavapipe is not**.
* Media: four real proxies from `~/.cache/kerf/proxies` (1280x720 @25, 1280x960 @30, 1280x720 @59.94,
  1280x1280 @24; all-intra H.264, `-g 1`), read-only.
* Browser-side costs come from **headless Chromium** (Playwright 1.63 shell build), not WebKitGTK/WKWebView.
* Environment fault worth knowing: the WSLg Wayland socket was wedged, and `wgpu`/Vulkan init blocked forever in
  `connect()` to `/run/user/1000/wayland-0`. Headless runs need `unset DISPLAY WAYLAND_DISPLAY`, a scratch
  `XDG_RUNTIME_DIR`, and `VK_ICD_FILENAMES=.../lvp_icd.x86_64.json`. No window was opened.
* Scratch material lives in `scratchpad/review/gpu-compositor/`: `gpubench/` (a Rust harness against the
  repo's crates, built into the repo's existing `target/` and removed afterwards), `ffall.sh`,
  `ffexport2.sh`, `stillbench.sh`, `browser-bench.cjs`, `coverage.log`, `*.log`.

## 2. Current state

**Shipped (A0 to A2):** a wgpu compositor that draws a `RenderPlan` exactly like FFmpeg's *still* graph
(YUV blend, ported swscale bicubic, vf_eq tables, the opacity RGB round trip), `FrameSource` (long-lived
`ffmpeg` runs, y4m + showinfo, frame cache, run router, thrash guard), `FrameCursor` (export-order reads),
and the opt-in A2 preview surface (`Settings.gpu_preview`, off by default). Parity is held by `tests/parity.rs`
(160 renders, PSNR >= 40 dB, max error <= 8 outside edge bands; typical results 45 to 55 dB, max 2 to 6 levels
[M, `target/parity/report.txt`]).

**Not built (progress log: A3, A4, A5, A6, A7 all `todo`):** scrub/drag at GPU speed, playback, every effect
pass, agent rendering, export. `Compositor::caps()` is `GpuCaps::A0` (`compositor.rs:326`): no motion, fades,
transitions, keyed channels, mask, text, effects, reframe or HDR.

**How a GPU preview frame is made** (`gpu_preview.rs` `frame()` 912, `attempt()` 976): `get_preview_frame`
takes `PlanInputs` under the project lock (`lib.rs:2015`), builds a fresh `Planner` per call (~0 ms [M]),
asks `plan.reasons(caps, size)` (1047), and on an empty list renders through `FrameSource` with
`Hint::Scrub` (1058), then presents. Otherwise it returns FFmpeg's JPEG for the same frame in the same call.

**Presenting:** per platform, one technique (`resolve_technique`, 102). Linux/X11: borderless child window,
confirmed under WSLg. Windows: the window's own surface under a runtime-transparent WebView2, **unconfirmed**.
macOS and Wayland: none, JPEG. A surface above the page cannot show HTML overlays, so a frame with a title box,
trim monitor or safe-area guide goes to the JPEG route (`routePreview`, `preview-bounds.ts:175`), as does a
Preview in a detached window.

**Playback:** `start_playback` (`lib.rs:2099`) runs `stream_preview` (`cli.rs:4304`): one ffmpeg process
running the *export* filter graph over `Timeline::slice(start, ..)` with `scale=bilinear`, `yuvj420p`, 960 px
wide, `mjpeg -q:v 6`; frames are cut out of the pipe (`next_jpeg` 4284), copied, base64-encoded (`lib.rs:2132`),
and sent over a Tauri `Channel` as JSON; the webview drops them into an `<img>`. `PLAYBACK_FPS` is **24**
(`playback-sync.ts:11`) whatever the project fps. The audio clock is Web Audio (`audio.ts:141`); the picture
chases it (`createFrameGate`).

---

## 3. Where the time goes (Q1)

### 3a. A scrubbed or paused frame

FFmpeg path (what runs today, and the GPU path's fallback), 960x540 from a 720p proxy, pinned FFmpeg 9.0.2:

| Piece | Cost | Basis |
| --- | --- | --- |
| spawn the process (`-version`) | 9 to 10 ms | [M] |
| open + probe + seek + decode **one input** | ~28 ms; each further input adds 20 to 40 ms (2 inputs 57 ms, 3 inputs 93 ms, 6 inputs 230 ms) | [M] |
| decoder/filter thread-pool spin-up with `-threads auto` | ~14 ms per input (see WP-0b) | [M] |
| scale/overlay/pad + MJPEG encode | a few ms | [M] |
| **whole still, 1 layer** | **52 to 57 ms** (min 52, median 57); with `-threads 1` per input 33 ms | [M] |
| **whole still, 3 layers** | **143 to 163 ms**; with `-threads 1` per input 76 to 86 ms | [M] |
| base64 + JSON + IPC (24 to 60 KB) | ~1 to 3 ms | [E] |
| `<img src=data:...>` decode and paint, 960x540 | 5 to 13 ms median, p95 22 to 28 ms | [M, Chromium] |

So a default scrubbed frame is roughly 65 to 80 ms for one layer and 160 to 250 ms for three. **The dominant
cost is opening and probing inputs per frame per layer, not the filter graph and not the encode.**
(On the distro FFmpeg 4.4.2, with 207 shared libraries, process start alone is 50 to 90 ms.)

GPU path, same frame, lavapipe [M]:

| Piece | Cost |
| --- | --- |
| planning (`Planner::new` + `at`) | ~0.00 to 0.01 ms |
| decode wait on a cache hit / warm run | 0.01 to 0.03 ms |
| decode wait on a cold restart (p95 of random jumps) | 45 to 55 ms |
| **composite record + submit (CPU)**, 1 layer | 0.4 to 0.8 ms |
| composite record + submit, 3 layers with an 80% opacity PiP and a rotated PiP | 1.9 to 3.0 ms |
| GPU execution on lavapipe (CPU raster; not representative), median | 15 to 21 ms at 430 / 540 / 960 px wide, 65 to 120 ms for the 3-layer case |
| first frame ever in a process (self-test, probes, spawn) | ~200 ms once (decode wait 157 to 165 ms) |
| per new asset: `source_is_indexed_container` + seek-point probe | 13 to 17 ms + 29 to 41 ms, once per file |
| `Gpu::new` / `Compositor::new` / colour-policy probe | 26 ms / 28 ms / 50 ms (lavapipe; real-GPU shader compile [E] 50 to 400 ms) |

Random-jump scrub through the router, 1 layer, 960x540: total median 21 ms, p95 83 ms; with 3 layers the
median is 112 ms, p95 156 ms, almost all of it lavapipe. A repeated render of a cached frame is 20 ms total, of
which 0.6 ms is CPU recording. On a real GPU I expect GPU time of roughly 1 to 3 ms [E] (about 15 M texel
fetches per 960x540 layer for the swscale emulation), plus up to one vsync of present latency
(`AutoVsync`, `desired_maximum_frame_latency: 2`, `present.rs:329`). That puts a warm scrub at ~5 to 15 ms and a
cold restart at ~55 to 70 ms [E].

**Reading:** the GPU path's scrub win (5 to 10x on warm frames, more with several layers) comes from *keeping
decoders alive and not re-opening inputs*, not from GPU arithmetic. A software adapter is no slower than the FFmpeg
still for single frames at preview size (20 to 28 ms against 52 to 57 ms for one layer), but it burns 3 to 4 cores (process CPU 75 to 130 ms per frame, 372 ms for three layers), and
wgpu does return a CPU adapter if it is the only one (`wgpu-core` ranks `Cpu` last but still picks it;
`gpu.rs:163` asks `HighPerformance` without excluding `Cpu`).

### 3b. Forward playback

Today's stream, per frame at 960x540, real 720p proxy, pinned FFmpeg, **CPU ms/frame (min of 3)** [M]:

| Graph | CPU ms/frame | Cores at 24 fps |
| --- | --- | --- |
| decode only | 2.5 | 0.06 |
| graph, 1 layer, no encode | 8.0 | 0.19 |
| **graph + mjpeg q6 (today's stream)** | **10.8** | 0.26 |
| + eq / clip fade / drawtext caption | 10.3 to 10.8 | 0.25 (these are free) |
| + gblur sigma 8 | 16.2 | 0.39 |
| + ellipse mask (`geq`) | **92.6** | **2.2** |
| rotate 8 deg + opacity 0.8 | 30.6 | 0.73 |
| 3 layers (base + 2 PiP, one translucent, one rotated) | 29.6 | 0.71 |
| dissolve (two alpha-faded layers) | 21.6 | 0.52 |

Other fixed costs of the stream:

* **Start-up:** time to the first JPEG is 63 ms (1 layer) and 144 ms (3 inputs) [M]. Every play, seek or edit
  during playback restarts the stream (`Preview.svelte:168` effect).
* **JPEG size:** 9 to 23 KB per frame at q6 on these proxies [M] (the code comment says ~40 KB).
* **Webview side:** `<img src=data:>` 5 to 13 ms per 960x540 frame (p95 22 to 28), **25 to 45 ms at 1080p**
  (p95 90 to 117) [M, Chromium]. That is why the stream is capped at 960 px and q6. With the same JPEG as raw
  bytes through `createImageBitmap` it is 2.5 to 3.2 ms and 5.4 to 5.7 ms [M].
* **IPC:** a payload over 8 KiB leaves the direct-eval path and is fetched by the webview
  (`tauri/src/ipc/channel.rs:36`); raw bytes are supported (`InvokeResponseBody::Raw`). Throughput unmeasured.
* **fps:** fixed 24 [R], so 30/60 fps projects preview at 24.

GPU path with `FrameSource` + `Hint::Forward` [M]: the ffmpeg run that feeds it costs 4.3 ms CPU/frame at 720p
(y4m + showinfo); the bench in the repo reports 5.1 / 6.2 / 12.3 ms per frame at 720p / 1080p / 4K against
52.8 / 89.3 / 241 ms for one-shot decodes. Composite recording is 0.5 to 3 ms. On lavapipe, 960x540 free-run is
38.7 fps for one layer (22 ms/frame, of which 21 ms is the software rasteriser) and 7.5 fps for three layers.
**On a real GPU I expect ~6 to 8 ms CPU per frame for one layer and about 4 ms more per extra layer (its decode)**,
because the filter graph, JPEG encode, base64 and webview decode drop out [E]. The measured stalls are elsewhere:

* **A cut to a not-yet-started run costs 63 to 64 ms** on the first frame of the new clip (338 ms the first
  time in a process), against a 40 ms frame interval [M, `gpubench cut`]. `Prefetch` is routed in
  `router.rs` but not exposed by `Hint` and never called (A4 was to drive it).
* Frame cache: 3 layers over 150 frames inserted 504 frames and evicted 336 under the 256 MiB cap; fine, but
  sized for 1 layer at 1080p.

### 3c. Export

One ffmpeg process does decode + graph + encode. 1080p30, 300 frames, CPU ms/frame (min of 3, pinned FFmpeg,
host load 15 to 40) [M]:

| Step | CPU ms/frame | Graph share of total |
| --- | --- | --- |
| decode 1080p H.264 | 7.0 | |
| graph, 1 layer (scale, pad, overlay) | 19.3 | |
| + libx264 medium crf23 (FFmpeg default) | 75.8 total | **25%** |
| + libx264 veryfast crf23 | 52.4 total | **37%** |
| graph, 3 layers (2 alpha PiPs, one rotated) | 80.5 | |
| + libx264 medium | 142.1 total | **57%** |
| graph, ellipse mask via `geq` | **402** | |
| + libx264 medium | 435 total (noisy: load 40) | **>= 90%** |

(An earlier run on the distro FFmpeg 4.4.2: graph 28.6 of 122 ms for 1 layer + x264 medium, 573 ms with the mask; same shape.)
So a compositor in the export path pays off in proportion to the graph's weight: ~1.2x for a plain cut at
x264 medium, much more with a hardware encoder (which takes encode CPU to near zero) or with masks/alpha
stacks. Decode (7 ms) and encode stay FFmpeg's. `render_variants` runs variants sequentially, decoding
the sources once per variant (`cli.rs:4616`); a compositor with a shared decode could remove that, but
nothing in the plan says so yet.

---

## 4. What the GPU refuses, and what lifting each refusal is worth (Q2)

I built three constructed timelines with the real model and planner and counted drawable frames at each
capability level (`gpubench coverage`, `coverage.log`). **These are my archetypes, not measured user projects**;
parameters are in `gpubench/src/coverage.rs`. Frames the compositor draws (to the harness's tolerance), as a share of the cut:

| Cut (render 540 px) | Plan | A0 (today) | + text* | + motion | + fades | + transitions | + keyed zoom / mask / effects |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **T1** 9:16 talking head, 60 s, 15 clips, 0.1 s fades, captions over ~85% | Still | **12.7%** | 96.0% | | | | 96.0% |
| | Motion | 0% | | 12.2% | 12.8% | 12.8% | **100%** with text |
| **T1b** same, hard cuts | Still | 13.3% | **100%** | | | | |
| **T1c** same, fades, no captions | Still | **96.0%** | 96.0% | | | | |
| | Motion | 0% | | 96.0% | **100%** | 100% | |
| **T2** 16:9 montage, 45 s, 55% dissolves, 12% dips, 6% slides, graded, 2 titles | Still | 69.5% | 78.8% | | | | 78.8% |
| | Motion | 0% | | 69.6% | 74.2% | 86.7% | **100%** with text |
| **T3** 16:9 tutorial, 90 s, webcam PiP, 5 lower thirds, push-ins, one mask+blur clip | Still | 65.6% | 83.3% | | | | **100%** |
| | Motion | 0% | | 16.6% | 16.6% | 16.6% | **100%** after keyed zoom |

\* "+ text" assumes an explicit font. **With `font: None` (what `TextOverlay::new` sets and what captions
and `add_overlay` produce) the plan adds `TextWithoutFont` and the "+ text" column does not move**: T1 stays at
12.7%, T2 at 69.5%, T3 at 65.6% [M]. `render_plan.rs:788-794`, `model.rs:1928`, `planner.rs:323`.

Where the refusals actually fall at A0 (frames hit / frames where it is the only reason):

* T1: `Text` 1301/1250, `Fade` 60/9.
* T2: `Fade` 228/52, `StillTail` 154/0, `Text` 150/104, `Travel` 11/0.
* T3: `Text` 500/400, `Effects`/`Mask`/`ZoomBehind` 375/0 (one clip).

Ranking by frames unlocked per unit of effort:

1. **Default font + text pass.** Captions are the flagship output of an AI-assisted short-form editor and
   cover most of a talking-head cut. 12.7% to 96 to 100% on T1.
2. **Mixed colour matrices.** Not in my archetypes (all untagged proxies), but real: in the user's proxy cache
   two proxies are untagged and two are BT.709 (`tv/bt709`). Under the **pinned FFmpeg 9.0.2 policy
   (`BottomLayerTag`)** a stack of an untagged and a BT.709 proxy is refused: *"layers tagged with different
   YCbCr matrices ... not reproduced"* [M, `gpubench mixed`, both orders]. Dissolves, slides and PiP
   between clips from different cameras hit it. Float and fixed-point models of FFmpeg's conversion were
   off by up to 26 levels (`gpu.md`), so porting it is expensive; normalising proxies is cheap.
3. **Fades, dips, dissolve ramp, slide/push (Motion mode).** T1c 96 to 100%, T2 +4.6% (fades) then +12.5%
   (transitions). Also the way to make *paused* frames right (next section).
4. **Keyed zoom (and `ZoomBehind`)**: T3 +17 to 83% because a push-in is the standard Ken Burns move. Needs a
   "last-stage scale evaluated per frame" pass; the export graph itself was fixed in `fix/keyed-zoom`
   (progress log), so the GPU follows the fixed graph.
5. **Masks, blur and the other effects.** Few frames in typical cuts, but they are the ones FFmpeg is slowest
   at (`geq` mask 93 ms CPU/frame at preview size, 402 ms at 1080p), so the speed gain per refused frame is
   the largest.
6. Reframe/360 (`v360`), chroma key, HDR: large, rare in the preview. HDR is already avoided in preview because
   proxies are tone-mapped to SDR (`Asset::as_sdr_proxy`).

Refusal ladder, cheapest and widest first (shares are of the frames in my three archetypes, A0 caps):

| Refusal | Plan | T1 / T2 / T3 frames | Effort to lift | Note |
| --- | --- | --- | --- | --- |
| `Text` (+ `TextWithoutFont`) | both | 87% / 13% / 22% | L (+S for the font) | WP-1 |
| Mixed matrices (`MixedMatrices`, `MatrixUnknown`) | both | not in these cuts | S to M | normalise proxies, WP-3 |
| Keyed colour (`KeyedColor`) | Motion | 0 / 0 / 0 | S | tables are already sampled per frame (`eq::luts(&layer.color)`); needs only parity cases and the cap flip |
| Fade steps (`Fade`) | Still: never drawable; Motion | 4% / 20% / 0 | L | WP-4 |
| `Tail`, `StillTail`, `Travel` | Motion | 0 / 15% / 0 | M after fades | WP-4 |
| Keyed zoom (`KeyedZoom`, `ZoomBehind`) | both | 0 / 0 / 17% (Still), 83% (Motion) | M | WP-6 |
| Mask, effects | both | 0 / 0 / 17% | M each | WP-6; biggest speed gain per frame |
| Keyed opacity (`KeyedOpacity`) | Motion | 0 | M | `geq` alpha arithmetic differs from the RGB round trip |
| Reframe / 360, chroma key | both | rare | L | |
| HDR | both | n/a in preview | L | proxies are already tone-mapped SDR |

**A finding that changes the framing of "refusal":** a *still* plan is refused inside any fade, dissolve,
slide or tail (`Unsupported::Fade/Travel/StillTail`), and the fallback is FFmpeg's still, which has no fade
or transition code at all: `build_still_args`' own doc says "Static blends (mid-crossfade dissolve,
dip-to-black) are intentionally *not* reproduced; the still shows the frame each visible clip contributes"
(`cli.rs:6700-6710`; `still_clip_chain` is "minus the time-domain `trim`/`setpts`/`fps`/fade steps").
So today a paused or scrubbed frame inside a dissolve shows the unfaded picture(s), disagreeing with
playback and export (the repo documents a worst case of 6.9 dB, max error 255, at one frame of a 0.6 s
dissolve; `core-render-plan.md`).
GPU Motion plans would make those frames *more* faithful than the current fallback.

---

## 5. GPU playback: what it takes and what it wins (Q3)

**Decode path.** Keep `ffmpeg` software decode into y4m pipes (`FrameSource`, `Hint::Forward`). Proxies are
all-intra 1280 px, so a run costs ~4.3 ms CPU/frame [M] and 1.4 MB/frame over the pipe. Hardware decode and
zero-copy are not worth it: `wgpu` has no stable external-texture import, the `FrameSource` already abandoned
`-hwaccel` after a ProRes/Vulkan mismatch (`gpu.md`), and originals have proxies. Needed changes:

* **Prefetch at cuts.** Expose `Prefetch` in `Hint`, scan ahead with `Planner::first_unsupported` /
  `span`, and start the next clip's run (including the two per-file probes, ~50 ms) one to two seconds early.
  Measured stall without it: 63 ms per asset-to-asset cut; same-asset jump cuts restart a run (45 to 55 ms
  p95 [M]).
* **Motion plans for playback**, because transitions and fades exist only there. That needs `Pick::Fps` served
  from the frame cache (today only `AtOrAfter` is) or a `FrameCursor` per layer (forward only, which is fine
  for a play session), and the Motion parity harness (WP-4).
* **Compositor allocation hygiene** (optional): textures, uniform buffers and filter-table textures are made
  per frame (`compositor.rs:498, 513, 664, 717`); it is ~1 ms and not the bottleneck.

**Pacing.** Two designs:

* *Pull (recommended first):* the webview stays the clock owner. While `ui.playing`, the frame pump asks
  `get_preview_frame(ui.time + measured latency, hint=Forward)` each `requestAnimationFrame`, latest wins. The
  backend pre-scans with `first_unsupported`; if the next ~5 s are drawable it presents on the surface/canvas,
  otherwise it keeps today's stream for this play session (all-or-nothing, no mid-play flipping). No
  audio-clock plumbing to Rust, no drift correction, same behaviour as the existing paused-frame pump, one
  IPC (~1 to 3 ms [E]) per frame.
* *Push:* a Rust loop with an anchored clock (`audio_time`, host time) refreshed by the page every ~250 ms and
  slewed. Lower jitter, more code (anchor protocol, output-latency compensation, resync policy). Only if the
  pull model shows jitter in practice.

Frame-drop policy stays "skip, never slow down" (`createFrameGate` already encodes it).

**Expected win** [E unless noted]: start latency 63 to 144 ms [M] becomes ~5 ms with a warm run; light
timelines go from ~11 ms (ffmpeg) + 5 to 13 ms (webview) per frame to ~6 to 8 ms; heavy ones (masks, alpha
stacks, dissolves) from 22 to 93 ms of CPU per frame to roughly 10 to 20 ms (about 4 ms of decode per layer plus
1 to 3 ms of recording); preview can run at the panel size
(up to 1920, `MAX_RENDER_WIDTH`) and at the project fps instead of 960 px at 24. The prerequisite is coverage
(WP-1, WP-4): with captions on 85% of the cut, all-or-nothing gating would almost never choose the GPU.

---

## 6. Presenting on macOS and elsewhere (Q4)

Existing status: Linux/X11 child window (confirmed on WSLg+lavapipe), Windows window-under-transparent-WebView2
(unconfirmed), macOS none, Wayland none. All native surfaces share one limit: the page cannot be drawn over a
surface above it, and only some placements put the surface below.

**macOS options**

1. **Try the existing `Technique::Window` first** (`KERF_GPU_SURFACE=window`, zero new code). The docs and
   `resolve_technique` say macOS needs a transparent *window*, "Tauri's `macos-private-api` (a private
   WKWebView key)". That premise looks wrong for this design:
   * The surface under the webview is the content view's `CAMetalLayer` (wgpu/Metal); the *window* can stay
     opaque. What must be transparent is the **WKWebView background**, i.e. `drawsBackground = NO`.
   * **Kerf already does that on macOS today**: `tauri.conf.json` sets the window's `backgroundColor`,
     `tauri-runtime/src/webview.rs:501` copies it into the webview attributes, and wry then sets the private
     `drawsBackground` KVC key on the configuration whenever a background colour is set
     (`wry-0.57.0/src/wkwebview/mod.rs:371-375`). wry's runtime `set_background_color` uses the same key on the
     instance (`:988-994`).
   * `macos-private-api` in Tauri only gates `WindowBuilder::transparent` (`tauri-runtime-wry/src/lib.rs:1052`),
     which this design does not need.
   This is source reading [R], not a Mac run. Risks to check on a Mac: wgpu replaces the content view's layer, making it layer-hosting while WKWebView is a
   subview (wry's `wgpu` example likewise puts a transparent webview over a wgpu window surface); `AppKit` calls (`setWantsLayer`, `setLayer`) must run on the
   **main thread**, but `build_backend` runs on a `kerf-gpu-build` thread (`gpu_preview.rs:1218`), so surface
   creation has to be hopped with `AppHandle::run_on_main_thread`; Retina `contentsScale` tracking when the window
   moves between displays; resize flicker (`presentsWithTransaction`); `set_background_color` on a webview is
   documented as "not implemented for the webview layer" on macOS in Tauri but wry 0.57 implements it.
2. **A dedicated child `NSView` with a `CAMetalLayer`** (objc2), added as a sibling of the WKWebView, either
   *below* it (needs the transparent webview, same page-overlay behaviour as Windows) or *above* it with
   `hitTest:` returning nil (public API only; the X11 child's limits: HTML overlays force the JPEG route). Cleaner
   than replacing the content view's layer, easier to show/hide/resize, parallel to `x11.rs` (512 lines). M.
   Re-raise on each present like `Child::place`, because wry may reorder subviews.
3. **A child `NSWindow`**: avoid (spaces, full screen, shadows, move sync).
4. **Readback to canvas (WP-2):** no native surface at all.

**Windows:** keep the current trick but confirm it on a PR build. wry's own WebView2 host uses
`WS_CHILD | WS_CLIPCHILDREN` for the webview HWND (`webview2/mod.rs:230`), not the parent, and `tao` sets no
`WS_CLIPCHILDREN` on the toplevel, which is the premise. Fallbacks: a `WS_CHILD` HWND above the webview with
`WM_NCHITTEST -> HTTRANSPARENT` (the X11 child's analogue), or WP-2.

**Wayland (Linux default session):** a subsurface cannot be placed inside GTK's. Cheapest partial: run the app
with `GDK_BACKEND=x11` (XWayland) when the preview is enabled (S, but affects fractional scaling and must be set
before GTK init). WP-2 covers it fully.

**The alternative that removes the problem: readback to canvas (WP-2).** The compositor already renders into a
texture; reading it back and drawing it in the page makes every platform equal, allows HTML overlays and
title handles over the picture, works in detached Preview windows (today forced to JPEG), and removes the
`covered` hit-testing poll. Costs for 960x540 RGBA (2.07 MB): readback ~1 to 3 ms on a discrete GPU, much
less on unified memory [E]; IPC copy of 2 MB ~2 to 4 ms [E, unmeasured]; `putImageData` 0.8 to 1.1 ms
(**2.4 to 3.8 ms at 1080p**) [M, Chromium]; WebGL `texImage2D` 0.2 ms (960) / 2.4 ms (1080p) [M, software GL].
Total ~5 to 8 ms per frame at preview size, so 30 fps playback is comfortable and 60 fps is plausible;
1080p RGBA (8.3 MB) is heavier (~20 to 30 ms [E]) and should be a native-surface-only case. The same readback
ring is needed by A6 (agent frames) and A7 (export), so the work is shared. **Open risk:** Tauri raw-channel
throughput on WebKitGTK/WKWebView is unknown; WP-0a is the cheap way to find out.

---

## 7. Risks to "refuse rather than approximate; every visual change gets a parity case" (Q5)

1. **The harness only knows stills.** `tests/parity.rs` renders each case through `export_still` and compares.
   Every WP that draws a Motion-only thing (fades, tails, travel, Fps picks, keyed zoom/colour/opacity,
   playback, export) has no reference renderer in the harness; `picked.rs`, `rendered.rs` and `sweep.rs` in
   kerf-core pin the *plan*, not GPU pixels. A Motion pixel-parity harness (export-graph frames vs GPU,
   per frame, both FFmpegs) is a prerequisite, and a larger job than any one pass.
2. **"Exact" already means "within a measured tolerance."** The harness passes at PSNR >= 40 dB and 8
   levels outside an edge band, with the band excluded from strict checking (up to 50% of a busy frame);
   the scaler is within 1 level up to ~4:1, 2 to 3 levels up to 20:1, 5 at 40:1; rotated cases have a 30 dB
   floor and one case reports a 254-level edge error inside its band [M, report]. New passes with inherently
   inexact algorithms (Gaussian blur truncation, `v360` interpolation, text rasterisation) will push on this.
   Keep thresholds per effect, record them, and write down which regimes are refused.
3. **FFmpeg version drift.** Parity is proven on 6.1.1 and 9.0.2 only; the dev machine's 4.4.2 cannot run the
   suite, and Ubuntu 22.04 users ship 4.4.2. Only the frame source has a runtime self-test; the compositor's
   arithmetic (overlay, eq, scale, fade) does not. FFmpeg 7/8 changes to swscale/overlay would be silently
   "approximate". Mitigation: a runtime **parity self-test** like `composite_color_policy()` (50 ms [M]) that
   renders a few synthetic frames both ways at first use and disables the GPU path on drift; a nightly job
   against FFmpeg master; a version allow-list as the cheap interim.
4. **Colour target.** The composite is read as BT.601 under FFmpeg 6.1 and as the bottom layer's tag under 9.0,
   while an export is untagged and players show HD as BT.709 (`gpu.md`). Parity tests enshrine the preview
   colour. The day the GPU path replaces the FFmpeg preview, that either stays a known preview/file mismatch or
   needs a new reference ("what a player shows"), which breaks the "parity = FFmpeg still" definition.
5. **Real GPUs are untested.** CI is lavapipe (and WARP). f32 paths (rotation sampling, fused multiply-add
   contraction) can differ by a level across GPUs; the bit-exact end-to-end test only compares the same device
   with itself. Add Metal (macOS runners) and WARP/DX12 to the parity matrix; plan a periodic
   `KERF_GPU_ADAPTER=hardware` run on the maintainers' machines.
6. **Frame identity heuristics.** `FrameSource` equals the one-shot decode only for the files it can prove runs
   equal on (MP4/MKV, seek points that are keyframes checked at the file's head, no B-frames); the limits are
   documented (`gpu.md`). Proxies are the safe case; originals are the risk.
7. **Hidden refusals defeat the feature.** `TextWithoutFont` (section 4) and `MixedMatrices` are refusals that
   no caps flip fixes. Add local, opt-in counters of `Unsupported` kinds per frame so priorities come from real
   projects, not from archetypes like mine (the reasons are already returned per frame).
8. **Software adapters.** wgpu may hand back lavapipe/WARP. The status exposes `software`, but nothing declines
   it; for playback and multi-layer frames it is slower and louder than FFmpeg.
9. **Process-level safety is good, throughput safety is not.** Crash marker, backoff, device-loss rebuild, panic
   catch: all present. A slow decode holds the single `render` mutex for up to the 5 s request timeout, so
   toggling the setting or a second request can wait that long.

---

## 8. Ranked roadmap

Effort: S <= 2 days, M ~ 1 week, L ~ 2 to 3 weeks, XL > 1 month (one developer who knows the code, including
parity cases and docs). "Win" is measured where tagged, otherwise estimated.

### WP-0 Quick wins (S each, independent, do first)

**0a. Raw bytes for the playback stream.** Send each JPEG as `InvokeResponseBody::Raw` on a
`Channel<InvokeResponseBody>`; in the page build a `Blob`, `createImageBitmap`, draw to a `<canvas>` (avoids
a `blob:` entry in the CSP). Why: base64+data URL costs 5 to 13 ms per frame (25 to 45 ms at 1080p) against
2.5 to 3 ms (5.5 ms) [M]. Win: frees the webview main thread, lets the stream width rise above 960 for large
panels. It is also the cheapest test of raw-channel throughput on WebKitGTK/WKWebView, which WP-2 depends on.
Risk: low; `createImageBitmap` on WebKit is available (Safari 15+). Files: `crates/kerf-app/src/lib.rs`
(`PlaybackFrame`, `start_playback`), `frontend/src/lib/api.ts` (`startPlayback`, harness fallback),
`Preview.svelte`, `frame-pump.svelte.ts`, docs `app.md`/`frontend.md`. Parity: none (pixels unchanged); add a
bun test for the frame gate with bytes.

**0b. Thread caps on single-frame decodes.** `run_still` calls `cpu::limit_args(.., budget_threads())`, which caps
only the *first* input's decoder and the encoder at the budget (default 75% of cores: 12 of 16 threads here,
`cpu.rs:47`) and leaves every other input on `-threads auto`; a one-frame decode wants one thread, and the
frame-thread pool costs ~14 ms per input. Add `-threads 1` per input and `-filter_threads 1
-filter_complex_threads 1` for stills and one-shot decodes (I measured with auto threads, i.e. 16; the app's
default is close). Win [M]: 59 to 33 ms (1 layer),
143 to 76 ms (3 layers); host-dependent (16 threads here). Risk: low; output is identical, but verify with a
pixel-equality test and the golden oracle (flags are added at spawn time, not in the builders). Files:
`engine/cpu.rs`, `engine/cli.rs` (`run_still`, `decode_frame`), `kerf-gpu/src/source.rs` (`decode_args`).

**0c. Project fps for playback.** `PLAYBACK_FPS = 24` regardless of the project; use the project fps
(cap 30 or 60, fall back on lag). Win: smoothness; cost ~+25% ffmpeg CPU at 30 fps [E]. Files:
`playback-sync.ts`, `Preview.svelte`.

### WP-1 Text on the GPU (L; 1a is S to M)

**1a. A resolvable default font.** Today `font: None` -> `font_file: None` -> `TextWithoutFont`. Decide the
default (system "Sans" through the same lookup `drawtext` uses, or a bundled OFL font) and make the graph pass
`fontfile=` always so the plan and the file agree. This changes every titled export/still/preview argv
(golden re-bless, family `drawtext`), and arguably fixes Windows/static builds where bare `drawtext` has no
font. Files: `fonts.rs`, `engine/cli.rs` (`drawtext_common` 6007), `planner.rs` (323), golden digests
(`engine/cli/golden/*.txt`), `docs/engine-cli.md`.

**1b. The text pass.** Recommended design: **let FFmpeg rasterise, let the GPU blend.** Run `drawtext` once
per distinct text state in a background job (like filmstrips; ungated, niced) onto a full-range canvas to get
the 0 to 255 coverage mask and the box, trim to the bounding box, cache by (text, font file, size, bold,
canvas size). The pass applies `drawtext`'s own YUV mask blend (colour -> YUV via drawutils, per-plane
subsampled mask averaging, `boxborderw=12`, synthetic-bold `borderw=2`, `alpha` keyframes, integer x/y from
`TextKeyframe`). Layout (`text_w`, kerning, shaping) is then FFmpeg's by construction, on whatever build the
user has. The alternative (cosmic-text/glyphon as in the original plan) gives different hinting, kerning
and shaping, so it can only be held to the edge-band tolerance. Win: T1 12.7% -> 96 to 100% [M]; paused
caption frames ~60 -> ~10 ms [E]; prerequisite for playback. Effort L, risk medium (blend arithmetic, per-build
`drawtext` differences, atlas sizing: one 1080x1920 canvas RGBA is 8 MB so trim). Also revisit `routePreview`:
with GPU-drawn titles the page draws only the selection handles, so `titlesShown` should not force the JPEG
route on `Window` technique (and does on `Child` until WP-2). Files: new `kerf-gpu/src/text.rs` and
`shaders/text.wgsl`, `compositor.rs`, `plan_caps.rs`/`render_plan.rs` (`caps.text`), `engine/cli.rs` (job),
`preview-bounds.ts`, `Preview.svelte`. **Parity cases:** white text no box; boxed `black@0.5`; synthetic
bold; two fonts; sizes 1% to 20% of height; positions at the frame edges and off-canvas; keyframed position and
alpha; escapes (`%`, `:`, `'`, `\`, newlines); non-Latin and missing-glyph (refused); over a graded layer, over
a translucent layer, over a letterbox; 9:16 and odd canvas; both FFmpegs, still and Motion.

### WP-2 Readback-to-canvas presenter (M)

New `Technique::Canvas` that needs no host window: render to the existing texture, copy to a staging ring
(double-buffered; ideally pack YUV 4:2:0 on the GPU for A6/A7 too), send as raw bytes over a `Channel`, draw in
a `<canvas>`; keep native surfaces as the optional high-resolution path. Why: macOS, Wayland, Windows without
trusting an unconfirmed trick, detached Preview windows, HTML overlays, deletes the `covered` poll. Win: ~5 to
8 ms/frame overhead at 960x540 [E]; platform reach is the point. Risk: medium (IPC throughput on WebKit
engines, colour management of canvas, memory churn of 2 MB buffers at 30 fps). Do WP-0a first. Files:
`kerf-gpu/src/compositor.rs` (readback ring), `gpu_preview.rs` (`Technique::Canvas`, no `Host`),
`lib.rs`, `api.ts`, `Preview.svelte`, `preview-bounds.ts`, `frame-pump.svelte.ts`, docs. Parity: pixel
equality between canvas bytes and `read_back()` (already asserted for the texture path), plus a browser-harness
screenshot check; no new FFmpeg parity.

### WP-3 Proxy colour normalisation (S to M)

Convert proxies to one matrix (BT.709 for HD, as players assume) at proxy time with an explicit
`out_color_matrix` and tag them consistently, bump the proxy cache key/sidecar version. Why: removes
`MixedMatrices`/`MatrixUnknown`/`UnmeasuredPolicy` from preview plans, which otherwise refuse every
dissolve/PiP across cameras under FFmpeg 9 [M]. Win: coverage for multi-source projects (not in my
archetypes); no change to the compositor. Risk: proxies are regenerated; preview colour for untagged BT.601
sources shifts to what FFmpeg's own overlay conversion would produce, so check that preview(proxy) is close to
export(original) for a mixed pair. Files: `engine/cli.rs` (`build_proxy_args` 2254), `proxy.rs`, `media.rs`,
sidecar tests. Parity: a mixed-tag pair (untagged + bt709, bt601 + bt709, bt2020) drawn by the GPU against
FFmpeg's own render of the same normalised proxies.

### WP-4 Motion mode: fades, dips, dissolves, slide/push (L)

Switch on `caps.motion`, `caps.fades`, `caps.transitions`: `Pick::Fps` served from the frame cache (or a cursor
per layer), `fade`'s integer arithmetic as a per-frame LUT (black end depends on whether the layer carries an
alpha plane: Y 16 vs 0, a1-design finding 19), the alpha ramp for dissolves (simpler than the RGB round trip), tail
layers, travel offsets. Why: exact paused frames inside transitions (the FFmpeg still draws none), prerequisite
for playback and export. Win: T1c +4%, T2 +4.6% then +12.5% [M]; also removes the "StillTail" class. Risk:
high (needs the Motion harness, `PickRate`, per-frame composite matrix vs FFmpeg 9's graph-wide negotiation). Files:
`plan_caps.rs`, `render_plan.rs`, `planner.rs`, `clip_timing.rs` (`FadeStep`), `frame_pick.rs`,
`kerf-gpu/src/frame_source.rs` and `compositor.rs`, new `shaders/fade.wgsl`, new `tests/motion_parity.rs`.
**Parity cases:** fade in/out at 24/25/29.97/30/60 fps; durations 1 frame to longer than the clip; fade on
alpha layers, translucent layers, graded layers, speed != 1, reversed; dip to black/white; dissolve with
covering and partial incoming clip; the 11 transition kinds; frames before/inside/after (the existing
`frames_inside_a_transition_...` family becomes "drawn and equal").

### WP-5 Scrub at GPU speed (A3) and hardening (M)

Snapshot `PlanInputs` as an `Arc` invalidated on `project-changed` (no project lock and no timeline clone per
frame), latest-wins mailbox in Rust instead of one IPC round trip per frame, `PresentMode::AutoNoVsync` while
dragging, `Hint::Prefetch` around the playhead, `render` lock no longer held across the decode wait, refuse
`Cpu` adapters unless forced or proven faster, runtime parity self-test (section 7.3), parity matrix with
macOS/Metal and WARP, local refusal counters. Win: warm scrub ~5 to 15 ms [E], cold restarts 55 to 70 ms [E];
risk reduction before default-on. Files: `gpu_preview.rs`, `lib.rs`, `kerf-gpu/src/gpu.rs`,
`frame_source.rs`, `.github/workflows/ci.yml`. Parity: the self-test itself; no new visual cases.

### WP-6 Keyed zoom, masks, blur and friends (M each)

Keyed zoom (the Still `ZoomBehind` and Motion `KeyedZoom` classes), rect/ellipse mask with feather and invert,
`gblur`/`unsharp`/grayscale/invert/vignette, then chroma key. Why: T3 +17 to 83%; masks and blur are where
FFmpeg is slowest (mask 93 ms CPU/frame at preview size [M]). Risk: medium to high per effect (Gaussian kernel
truncation, `geq` float expressions, chroma-plane handling). Files: `shaders/*.wgsl`, `compositor.rs`,
`plan_caps.rs`. **Parity:** zoom easing/odd sizes/mid-stream resize; mask shape x feather x invert x rotated or
scaled layer x opacity; blur sigma sweep including chroma planes and borders; vignette/sharpen/negate on limited
and full range; each with a stated tolerance.

### WP-7 GPU playback (A4) (L)

Pull model (section 5), prefetch for cuts, all-or-nothing gating by `first_unsupported`, handover to
`stream_preview` through `SpanPlan::handover` when a span is refused. Win: start latency 63 to 144 ms -> ~5 ms
[M->E], 2 to 9x less CPU per frame on heavy cuts [E], panel-size preview at project fps. Risk: medium (A/V
offset, cut stalls, renderer switching). Depends on WP-1, WP-4 for real coverage. Files: `gpu_preview.rs`,
`lib.rs`, `kerf-gpu/src/frame_source.rs` (`Hint::Prefetch`), `Preview.svelte`, `playback-sync.ts`,
`audio.ts` (output latency). Tests: a barcode clip + click track A/V offset measurement; no visual parity
beyond WP-4.

### WP-8 macOS native surface (M, needs a Mac)

Step 1: run `KERF_GPU_SURFACE=window` on a Mac with the existing code (section 6). Step 2: if it fails, a child
`NSView`+`CAMetalLayer` module beside `x11.rs`, surface creation on the main thread. Skip if WP-2 ships and
performs. Files: new `gpu_preview/macos.rs`, `gpu_preview.rs` (`resolve_technique`), `Cargo.toml` (objc2 crates),
`app.md`.

### WP-9 Export through the compositor (A7) (XL)

YUV canvas readback (not RGB: the canvas is already FFmpeg's pre-encode YUV; 3.1 MB/frame at 1080p against
8.3 MB RGBA) piped into an encode-only ffmpeg with the audio graph. Win [M]: graph share of CPU is 25% (plain
cut, x264 medium), 37% (veryfast), 57% (3 layers), ~90% (mask); more with a hardware encoder. Risk: highest
(colour policy per frame vs FFmpeg 9's negotiation, VFR picks, range export, variants, cancel). Do after WP-4 and
WP-6; consider sharing decode across `render_variants` as part of it.

---

## 9. Open questions

1. **Real hardware.** All "real GPU" figures are estimates. One Windows machine with a discrete GPU, one with an
   integrated one, and one Apple Silicon Mac would settle GPU execution time, present latency, shader compile
   time, and whether the Windows/macOS surface tricks work. Which machines does the maintainer have?
2. **Default title font.** Bundle an OFL font (size, licence notice) or resolve the platform's? Either changes
   every titled export's argv and possibly its look.
3. **Colour target** when the GPU replaces the FFmpeg preview: keep matching FFmpeg's still, or match what
   a player shows? (Section 7.4.)
4. **How much approximation is acceptable for text edges and blur?** The invariant says refuse; the harness
   already tolerates an edge band. Decide per effect before WP-1b/WP-6.
5. **Is the readback presenter acceptable as the default?** It trades a copy per frame for platform
   uniformity. Measure raw-channel throughput on WebKitGTK/WKWebView/WebView2 first (WP-0a).
6. **What do real timelines look like?** My coverage numbers use constructed cuts. Local counters of refusal
   reasons would replace them in a week of use.
7. **Minimum FFmpeg for the GPU path.** Gate by version, by self-test, or both? Ubuntu 22.04 ships 4.4.2.
8. **Persistent decoder for random access.** The scrub floor on a cold jump is a process start + open (~45 to
   55 ms [M]); only an in-process decoder (libav, not linked in bundles) or a low-resolution frame sprite cache
   would go below it. Worth it?

---

## Appendix A: raw numbers

All CPU numbers are min of 3 unless stated; load average 10 to 42.

* FFmpeg still (960x540, 720p proxy, pinned): 1 layer min 52.3 / median 56.8 / p90 58.7 ms (n=40); 4:3 proxy
  min 57.7 / median 65.2 ms. Time to first stream frame: 1 layer min 63 / median 65 ms; 3 layers 144 / 149 ms.
  `ffmpeg -version` 9 to 10 ms (distro 4.4.2: 50 to 90 ms, 207 libraries).
* Per-input cost, no filter, 1 frame: 1 input 38 ms; 2 inputs 57; 3 inputs 93; 6 inputs 230.
* `-threads 1`: 1 input 24 to 26 ms vs 38 to 40; full still 1 layer 33 vs 59 ms; 3 layers 76 vs 143 ms
  (min), medians 36 vs 66 and 86 vs 163.
* Repo benches on lavapipe (release): 1080p canvas, ffmpeg still vs GPU total, 1/3/6 layers: 72/140/275 ms vs
  67/104/196 ms; 4K: 183/375/847 vs 196/348/736 [M, pinned]. Frame source (one-shot vs runs, ms/frame): 720p
  playback 52.8 vs 5.1, scrub 55.6 vs 10.9; 1080p 89.3 vs 6.2, 119.8 vs 36.1; 4K 241.1 vs 12.3, 217.1 vs 64.7.
* `gpubench` (lavapipe, 960x540, 1 layer): forward free-run 38.7 fps; record+submit median 0.76 ms; lavapipe
  completion median 21.5 ms; random scrub total median 21.1 / p95 83.2 ms; paused re-render 20.1 ms. 430 px:
  60.9 fps, 15 ms; 540x960 cover: 54 fps, 17 ms. 3 layers: 7.5 fps, record 3.0 ms, completion 118.7 ms.
* Cut stall (asset-to-asset, no prefetch): 63 to 64 ms; 338 ms in a cold process.
* Chromium (headless, CPU): data-URL JPEG decode+paint 960x540 median 5.5 to 12.8 ms (p95 22 to 28), 1080p 25 to
  54 ms (p95 89 to 118); Blob+`createImageBitmap` 2.5 to 3.2 ms / 5.4 to 5.7 ms; RGBA `putImageData` 0.8 to 1.1 ms
  / 2.4 to 3.8 ms; `createImageBitmap(ImageData)`+draw 2.1 to 2.2 / 6.3 to 10.8 ms; WebGL `texImage2D` 0.2 / 2.4 ms.
* Parity (pinned, lavapipe, release): 28 tests pass in 19 s; on distro 4.4.2 27 of 28 fail at fixture creation.

## Appendix B: how to reproduce

```bash
S=/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/gpu-compositor
source $S/env.sh        # unsets DISPLAY/WAYLAND_DISPLAY, scratch XDG_RUNTIME_DIR, lavapipe-only Vulkan ICD
export KERF_FFMPEG=$PWD/crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu
export KERF_FFPROBE=$PWD/crates/kerf-app/binaries/ffprobe-x86_64-unknown-linux-gnu
KERF_BENCH=1 cargo test -p kerf-gpu --no-default-features --release -- --ignored --nocapture bench
cargo test -p kerf-gpu --no-default-features --release -- --ignored        # parity
FF=$KERF_FFMPEG $S/ffall.sh ~/.cache/kerf/proxies/af5e3ad1363126f9.mp4 300 # stream costs
FF=$KERF_FFMPEG $S/ffexport2.sh $S/tmp/src1080.mp4                         # export costs (needs a 1080p source)
node $S/browser-bench.cjs                                                  # webview-side costs (Chromium)
# gpubench: coverage | mixed | cut | probes | "<case name>"  (source in $S/gpubench, builds against the repo)
```
