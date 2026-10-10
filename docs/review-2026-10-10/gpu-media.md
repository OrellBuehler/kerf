# Kerf media pipeline: where the GPU can help (and where it cannot)

Analysis only. Nothing in `/home/orell/github/kerf` was edited, built or committed. Date 2026-10-10, HEAD `06f729d`.

Legend used throughout: **[M]** measured by me for this report, **[D]** taken from the repo docs and not re-measured, **[E]** estimate or general knowledge that I could not verify here.

---

## 0. Headlines (ranked)

Most of what the interactive path costs today is **fixed per-process overhead, not decoding**, and the biggest GPU-related loss is that `-hwaccel auto` is requested for work where it makes things slower. Ranked by (real-world win) / (effort and risk), for the stated user (iPhone HEVC/HDR + Canon R5 II 10-bit 4:2:2, Windows, AMD GPU):

| # | Work package | Measured win (Windows, Ryzen 5800X3D + RX 7900 XT) | Effort | Risk |
|---|---|---|---|---|
| 1 | **WP1** Do not pass `-hwaccel` (and use 1 thread) for single-frame / proxy-fed decodes | scrub of a proxy frame **149 ms -> 55 ms** [M]; playback start-up about -75 ms [M]; Linux 55 -> 31 ms [M] | S | very low |
| 2 | **WP2** Choose the decoder deliberately: `d3d11va` on Windows (today `auto` = **dxva2**), probe-verified, skip unsupported pixel formats | export 4K->1080p -8..-10 % wall (HDR also -25 % CPU); 4K original stream 1.89 s -> 1.47 s; decode-only CPU 1.53 -> 0.39 s; **4:2:2 originals: `auto` is 43 % slower than software per scrub** [M] | S | low |
| 3 | **WP3** Scene detection (and agent contact sheets) read the **proxy** when it is ready | 4K HEVC10 clip 2.34 s -> 0.27 s (8.7x); 4:2:2 clip 4.66 s / 35 CPU-s -> 0.27 s / 1.0 CPU-s (17x / 34x) [M] | S-M | medium (scene scores move) |
| 4 | **WP4** Hardware *export* encoders: recalibrate the CRF->QP mapping, add `av1_amf`, `-quality quality`, offer a "GPU" preset | 4K->1080p 8 s: x264-medium 3.45 s / 25 CPU-s -> `h264_amf` 2.0 s / 1.9 CPU-s [M]; **today's AMF mapping makes files 1.9x larger** [M] | S-M | medium (quality perception unverified on real footage) |
| 5 | **WP5** Do not hardware-*encode* proxies (keep hardware decode) | same wall, +0.4 CPU-s per 8 s clip, proxies 25 % smaller [M]; removes "one refusal disables HW encode for the process" | S | very low |
| 6 | **WP6** Scrub amortisation: cache composited stills + prefetch a window from one spawn | marginal frame costs about 1 ms vs 55-150 ms per spawn [M] | S-M | low |
| 7 | **WP7** Play at the timeline fps instead of a fixed 24 | stream sustains >1000 fps from a proxy [M] | S | low |
| 8 | **WP8** One decode, many consumers on import (proxy + filmstrip [+ scenes]) for sources the GPU cannot decode (4:2:2) | R5-II-like clip: removes 1-2 of 3 full 4K 4:2:2 decodes (about 25-35 CPU-s each per 8 s) [M] | M-L | medium |
| 9 | **WP9** A GPU lane in `cpu.rs` | a hardware-only proxy job uses 0.3-0.7 core; two concurrent give +32 % aggregate, more gives nothing [M] | M | medium |
| 10 | WP10-WP13 | stills tone-map order, Linux VAAPI encode / NVIDIA zero-copy, persistent decoders, transport | see §5 | |

**Do not build on FFmpeg GPU filters yet** (`scale_d3d11`, `scale_d3d12`, `vpp_amf`, Vulkan upload, `libplacebo`, `tonemap_opencl`): on the bundled FFmpeg 9.0.2 with an RX 7900 XT each either fails to initialise or gives a different picture (§3.7) [M].

**Transport is not the bottleneck.** base64 + JSON costs tens of microseconds per frame against a 41.7 ms frame period (§3.6) [M/E]. Do not spend effort there before the other items.

**What the GPU cannot do for this user:** Canon R5 II 4:2:2 10-bit HEVC has **no AMD hardware decode** (VCN: 4:2:0 only). It falls back to software silently: 61 fps for 4K on this 16-thread CPU, 100-140 ms of CPU per frame [M]. For that footage the wins are fewer redundant decodes (WP3, WP8), not the GPU.

---

## 1. Method and environment

* Windows side measured by launching the **exact bundled FFmpeg** (BtbN `n9.0.2-25-g67b60c310b-20261010`, `win64-gpl-9.0.zip`; SHA-256 checked equal to the pin in `scripts/fetch-ffmpeg.mjs`) as `ffmpeg.exe` from WSL via interop on the Windows host: **Ryzen 7 5800X3D (8c/16t), Radeon RX 7900 XT, driver 32.0.31041.1004** (read with `Win32_VideoController`). So these are real-GPU numbers on the stated hardware, but see caveats.
* Linux side: WSL2, same CPU, no usable GPU (nvenc/qsv/amf fail their probes, VAAPI has no driver, Vulkan is llvmpipe). Used the repo's local `crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu` (9.0.2, GPL) and `/usr/bin/ffmpeg` 4.4.2.
* Test media (synthetic: `testsrc2` + temporal noise; generated with the 9.0.2 binary, deleted afterwards): 4K30 HEVC Main10 4:2:0 HLG (8 s, 46 Mbps, "iPhone-like"); 4K30 HEVC 4:2:2 10-bit PQ (8 s, 135 Mbps, "Canon-like"); 1080p30 H.264 SDR. Synthetic noise makes encoders look worse than real footage and PSNR is not perceptual quality.
* Command lines are the production ones, reconstructed from `build_proxy_args`, `build_still_args`, `build_preview_args_with`, `build_export_args_phase`, `decode_frame`, `tonemap_chain` (graph strings copied from the code and its unit tests). I did not run the app (no GUI by instruction).
* Timing: wall = median of 2-15 runs; CPU on Windows = `Process.TotalProcessorTime` (15 ms timer granularity). **The box was shared with other agent sessions** (host CPU 30-50 % busy at times, WSL load average 9-24), so expect +/-20-30 % noise and treat deltas under 15 % as noise. Linux CPU-seconds were measured under heavier contention than the Windows ones and are inflated.
* Interop adds process-creation overhead compared with a Rust `Command` spawn; absolute spawn-floor numbers inside the app may be 10-30 ms lower.
* Scripts and raw logs: `scratchpad/review/gpu-media/win-scripts/` (`g1..g16.sh`, `bg.ps1`, `par.ps1`) and `.../bin/*.py` + `.../logs/`.

---

## 2. Current state (code map)

* **Decode acceleration.** `hwaccel()` (`engine/cli.rs:124`) defaults to `auto`; `decode_hwaccel()` (:147) returns it unless a process-wide learned flag (`HWACCEL_OK`, :137) was cleared by any one failure. It is requested by: the composited still (`Project::composite_timeline_frame`, `project.rs:917-935`; `build_still_args`, `cli.rs:6787-6794`, per input, **proxies included**), the playback stream (`stream_preview_once`, `cli.rs:4349`, via `push_inputs`, `cli.rs:3802-3809`), `decode_frame` (`cli.rs:1545-1590`), proxy generation (`cli.rs:2438`), scene detection (`cli.rs:1215`), salience (`:1304`), filmstrips (`filmstrip.rs:974`, **but not for the proxy attempt**, `filmstrip.rs:943-962`) and the GUI export default (`ExportDialog.svelte:587`). Frames are always downloaded to system memory (no `-hwaccel_output_format`), so the whole filter graph is software.
* **Encode acceleration.** `hw_encoders()` (:207) probes `HW_ENCODER_CANDIDATES` (:188: nvenc/qsv/videotoolbox/amf, h264/hevc[/av1]; VAAPI deliberately absent) with a one-frame encode each. Proxies take the first verified h264 encoder (`proxy_hw_encoder`, :2207); exports let the user pick; CRF is mapped to a vendor knob in `quality_args` (:2177) and `push_video_opts` (:3969).
* **Proxies.** All-intra (`-g 1`), 1280 wide (720/1080 selectable), video-only, written to `<cache>/kerf/proxies/<hash>.mp4`; HDR is tone-mapped in the proxy; `Project::preview_assets` swaps the path (`project.rs:1114`). Gated by `cpu::lease` in the *proxy* lane.
* **Scrub.** `frame-pump.svelte.ts` (single-flight) -> `get_timeline_frame` (`kerf-app/src/lib.rs:1997`) -> `composite_timeline_frame` -> `timeline_frame` -> `run_still` (`cli.rs:6566`): **one `ffmpeg` process per frame**, a `filter_complex` with a black base + overlay, MJPEG on a pipe, base64 data URL back. No result cache for stills (the 96-entry LRU at `cli.rs:75` serves only `decode_frame`).
* **Playback.** `start_playback` (`lib.rs:2099`) -> `stream_preview` (`cli.rs:4304`): one long-lived ffmpeg running the export graph at <=960 px, MJPEG q6, JPEGs split on `FFD8/FFD9`, paced to a fixed `PLAYBACK_FPS = 24` (`playback-sync.ts:11`), `PlaybackFrame{time, jpeg: "data:image/jpeg;base64,..."}` over a Tauri `Channel`, shown in an `<img>` (`Preview.svelte:719`). Any seek, edit or proxy landing restarts the process.
* **Budget.** `cpu.rs`: one heavy job at a time in three lanes (foreground > proxy > background, `cpu.rs:129-458`), `cpu_percent` default 75 -> `-threads/-filter_threads/-filter_complex_threads` written at spawn (`limit_args`, :497) plus below-normal priority. Interactive reads are ungated but get `limit_args(budget_threads())` (12 threads on 16 logical cores).
* **Already in tree, opt-in:** kerf-gpu (`FrameSource` long-lived software decodes + wgpu compositor + native surface), per `.claude/docs/gpu.md` and `app.md` [D]. Its decodes are software on purpose.
* **Shipping reality.** Only Windows bundles FFmpeg (`release.yml` `bundleFfmpeg`, `fetch-ffmpeg.mjs` BtbN GPL 9.0.2). macOS and Linux use whatever `ffmpeg` is on PATH, so their hardware paths are only as good as the user's build.

---

## 3. Where the time goes (measured)

All Windows rows: RX 7900 XT + 5800X3D, FFmpeg 9.0.2 (bundled build). "4K" = 3840x2160 30 fps, 240 frames.

### 3.1 Scrub: one composited still (960x540 MJPEG, production graph)

| Case | wall |
|---|---|
| `ffmpeg.exe -version` / trivial lavfi frame (process floor) | 27-72 ms / 81-90 ms (varies run to run) |
| **Proxy (720p all-intra), production flags** (`-hwaccel auto`, default threads) | **149 ms** (n=15) |
| same, software decode | 69 ms |
| same, software + `-threads 1 -filter_threads 1 -filter_complex_threads 1` | **55 ms** |
| same, `-hwaccel d3d11va` | 121 ms |
| FHD H.264 original: software / auto / d3d11va | 151 / 232 / 183 ms |
| 4K HEVC10 original (no tone-map): software / auto(dxva2) / d3d11va | 464 / 370 / 311 ms |
| 4K HLG original, tone-map before scale (the still's order) auto | 627-765 ms |
| same, tone-map after the fit scale (the export's order), auto / d3d11va | 463 / 354 ms |
| 4K 4:2:2 PQ original, software | 1.27-1.49 s (after-scale order 1.18 s) |
| 4K 4:2:2 single frame, software / `auto` / `d3d11va` | **1053 / 1508 / 1525 ms** |

Linux (WSL, 9.0.2, busy box) [M]: process floor 9 ms; proxy still 55-70 ms, of which **about 24 ms is thread-pool creation** (55 -> 31 ms with the three `1`s; CPU 99 -> 36 ms); FHD original 163 ms; 4K HEVC10 original 756 ms; HLG tone-map-first 1.08 s; 4:2:2 1.37 s. System FFmpeg 4.4.2: floor 31 ms, proxy still 79 ms.

Reading: a proxy scrub frame does about 3-10 ms of real work (a 720p intra frame decodes in 2.7 ms on one thread [M]); the rest is process start (27-72 ms Windows), **hardware decoder session creation (+70-80 ms per spawn, DXVA2 and D3D11VA alike)** and **thread pools for one frame (+14-25 ms)**. The engine's own filmstrip code already skips `-hwaccel` for a proxy (`filmstrip.rs:943-962`), for this reason; the scrub and stream paths do not. Before a proxy exists the cost is real decoding (0.3-1.5 s) and, for HDR, a full-resolution float tone-map ahead of the scale.

### 3.2 Playback stream (long-lived process)

| | wall for 240 frames |
|---|---|
| from the proxy, 960x540 MJPEG q6, software | 270-280 ms (about 85 ms start-up, then **about 0.8 ms/frame = 1200 fps**) |
| same with `-hwaccel auto` | 353 ms (+75 ms start-up) |
| same with MJPEG q2 | 286 ms (CPU 188 -> 313 ms) |
| from the 4K HEVC10 *original* (no tone-map): software / d3d11va / auto | 1.65 s / 1.47 s / **1.89 s** (145 / 164 / 127 fps) |
| from the 1080p H.264 *original*, decode + scale to 960: software / d3d11va / auto | **298 ms (281 ms CPU)** / 363 ms (47 ms CPU) / 407 ms (47 ms CPU) |

JPEG size at q6 (synthetic content): 21 KB at 960x540, 37 KB at 1280x720, 58 KB at 1080p (q2: 37 KB at 960). The fixed 24 fps pacing leaves >98 % of the pipeline's capacity unused for proxy-fed timelines.

### 3.3 Proxy generation (4K HEVC10 HLG -> 1280x720 all-intra, 240 frames)

Windows, production command with the proxy's tone-map chain (CPU `zscale`+`tonemap`), 12 threads:

| Pipeline | wall | CPU-s |
|---|---|---|
| A production default: `-hwaccel auto` (dxva2) + `h264_amf` | 3.96 s | 13.3 |
| B `auto` + x264 intra | 3.98 s | 16.1 |
| C `d3d11va` + `h264_amf` | 3.29 s | 14.6 |
| D all software + x264 intra | 4.10 s | 26.1 |
| quiet re-run: `d3d11va` + CPU tone-map + `h264_amf` | 2.45 s | 4.9 |

Same without tone-map (SDR-like): software + x264 **2.29 s / 17.5 CPU-s**; `auto` + AMF 2.39 s / 1.25; **`d3d11va` + AMF 1.85 s / 0.56**; `d3d11va` + x264 1.90 s / 1.02. A pure GPU decode with no download runs at 1.12 s (215 fps); the 4K P010 readback (25 MB/frame) adds 0.67 s, so the path is **readback-bound at about 130-180 fps** [M]. Two concurrent hardware proxy jobs: 237 fps aggregate vs 179 for one (+32 %); 3 and 4 jobs give no more (237, 221). Hardware decode is **byte-identical** to software across `auto`/dxva2, `d3d11va`, `d3d12va` for H.264 8-bit, HEVC Main10 and the AMF proxy (`-f md5` after forcing the pixel format; five `-ss` seek points on the HEVC GOP picked the same frames), and a full HDR export through sw/dxva2/d3d11va decode produced identical files (`cmp`) [M].

Stage split, software on Linux (busy box, so shares are indicative): decode 50 %, tone-map 30 %, scale 10 %, x264 intra 10 % (decode-only 12.7 CPU-s of 25.1). For the **4:2:2 PQ** source: decode alone 33.8 of 46.8 CPU-s (72 %); Windows decode-only is 3.94 s (23.5 CPU-s) in software, 4.12 s with `auto` and 3.64 s with explicit `d3d11va` (silent software fallback in both) [M]. Extrapolated [E from M]: decode alone is 61 fps (2x real time) on all 16 threads, so one hour of such footage costs about 30 min of wall time and about 2.9 CPU-hours before any scale, tone-map or encode.

AMF all-intra proxy: all 240 frames are I; 7.35 MB vs 5.54 MB for x264 veryfast crf 24 (+33 %) [M]. Scrub from either is the same (86 vs 101 ms, both software decode).

Other stage facts: `hw_encoders()` costs about 1 s once per process on this machine (nvenc 3 x 65 ms, qsv 3 x 85 ms, amf 2 x 300 ms, sequential); all-intra x264 at 720p is about 2.5-3 ms/frame/thread.

### 3.4 Export (8 s of 4K HEVC10 -> 1080p30 H.264, production graph incl. black base + overlay + `pad`)

| Decode / encode | HDR (CPU tone-map) wall / CPU-s | SDR-like wall / CPU-s |
|---|---|---|
| software + x264 medium crf 20 | 6.26 s / 58.3 | 4.22 s / 36.7 |
| `auto` (dxva2) + x264 | 5.42 s / 37.7 | 3.82 s / 24.1 |
| **`d3d11va` + x264** | 5.01 s / 28.4 | 3.45 s / 25.4 |
| `d3d11va` + `h264_amf` cqp 20 | **3.80 s / 2.7** | **2.01 s / 1.9** |
| `d3d11va` + `hevc_amf` cqp 23 | 4.34 s / 5.0 | 2.04 s / 1.8 |

Stage split of the `d3d11va` + AMF SDR export: bare decode 1.1 s without readback and 1.3 s with it, whole production graph + AMF 1.8 s; the base/pad/overlay/setpts/fps stages add about 5 % [M]. The wall time is **decode + download bound**, not graph bound. 4:2:2 PQ -> 1080p SDR (R5-II-like): `auto` + x264 medium **10.7 s / 93 CPU-s** for 8 s of footage (0.75x real time), `auto` + `h264_amf` 7.8 s / 60 CPU-s [M].

Encoder quality against a lossless render of the same graph (SDR-like, PSNR is not perceptual):

| Encoder / setting | size | PSNR |
|---|---|---|
| x264 medium crf 20 | 5.59 MB | 45.06 dB |
| x264 medium crf 26 | 1.49 MB | 41.50 dB |
| `h264_amf` cqp 20 (**Kerf maps CRF 20 to this**) | 10.70 MB | 48.90 dB |
| `h264_amf` cqp 26 / cqp 26 `-quality quality` / cqp 30 | 5.31 / 5.21 / 2.88 MB | 45.48 / 45.54 / 43.26 dB |
| `h264_amf` vbr_peak 5M quality | 5.07 MB | 45.41 dB |
| `hevc_amf` cqp 26 / cqp 29 quality | 5.56 / 3.31 MB | 46.07 / 44.36 dB |
| `av1_amf` cqp 90 quality (works on this GPU, not in Kerf's candidate list) | 6.41 MB | 46.36 dB |

The AMF H.264 stream had no B-frames (`has_b_frames=0`); `hevc_amf` writes bt709/bt709/bt709 colour tags while `h264_amf` and x264 leave transfer/primaries unspecified [M].

### 3.5 Analysis and timeline drawings (original vs proxy)

* **Scene detection** (`analysis.rs:151` reads `asset.path`, the original): 4K HEVC10, auto 2.34 s / 3.9 CPU-s, d3d11va 1.84 s, software 1.88 s / 12.2 CPU-s, **proxy 0.27 s / 1.0 CPU-s**; 4:2:2 original 4.66 s / 35.4 CPU-s vs the same 0.27 s [M].
* **Filmstrip** on the original (capped at 4 threads, `filmstrip.rs`): 4K HEVC10 software 3.64 s / 12.4 CPU-s, auto 1.63 s / 0.2, d3d11va 1.35 s; 4:2:2 software **7.5 s for an 8 s clip**; from a proxy 0.28 s [M].
* So an imported R5-II-like clip is decoded in full by the proxy, by the filmstrip (until the proxy lands) and by scene detection, each about 25-35 CPU-s per 8 s of footage, none hardware-capable on AMD.

### 3.6 Frame transport (Rust -> webview)

* Tauri 2.12 `Channel::send` sends JSON bodies under 8192 bytes by `eval` and everything larger by **storing the body and having the page `fetch` it** (`ipc/channel.rs:34-38, 290-322`); raw bodies already take the fetch path above 1024 bytes. A 28 KB `PlaybackFrame` therefore costs one `eval` plus one fetch round trip per frame regardless of encoding [D, read from source].
* Per-frame CPU: JSON.parse of the 28-77 KB payload 9-25 us, base64 decode 7-9 us (V8, measured in Node as a stand-in for WebView2), base64 encode in Rust about 10 us [E from library throughput]. Total **<0.1 % of a 41.7 ms frame**. At 150 KB (1080p, q2) JSON.parse is 150 us. Bandwidth: 21 KB x 1.33 x 24 fps = 0.67 MB/s.
* What I could not measure headless: the eval + fetch latency inside WebView2/WKWebView and the `<img>` JPEG decode on the main thread (about 1-3 ms at 960x540 [E]).

### 3.7 GPU filters on the bundled FFmpeg + RX 7900 XT (negative results) [M]

| Attempt | Result |
|---|---|
| `scale_d3d11` (`-hwaccel_output_format d3d11`), HEVC10 and H.264 8-bit | `Could not create the texture (80070057)` -> graph fails |
| `scale_d3d12` | `Video process configuration not supported by hardware` |
| `vpp_amf` from d3d11 or via `hwmap` | `AMFConverter-Init() failed with error 10` |
| `-hwaccel vulkan` decode | `Decode error rate 1 exceeds maximum` |
| FFmpeg Vulkan `hwupload` (any content, incl. synthetic 1080p) | `VK_ERROR_UNKNOWN` in `vulkan_pool_alloc` (device creation itself works) |
| `libplacebo` on system-memory frames (own Vulkan device) | works, but **4.6 s vs 2.45 s** for the proxy (4K upload/download per frame) and **17.5 dB PSNR vs the CPU tone-map** (different HLG handling) |
| `tonemap_opencl` (mobius, d3d11va decode, CPU scale first) | works: **2.15 s / 0.53 CPU-s vs 2.45 s / 4.9** for the CPU chain, but **29.8 dB PSNR / SSIM 0.90 vs the CPU chain** (colours differ) |
| `colorspace=all=bt709:iall=bt2020` (the no-zimg fallback) | 19.3 dB vs the zscale chain |

So no GPU filter pipeline is both working and colour-identical here; and with system-memory frames they do not even win on speed.

---

## 4. Answers

### Q1. Where does time go today?

* **Scrub** (user waits per frame): fixed overhead dominates. With a proxy: 149 ms = 27-72 ms process + 70-80 ms hardware decode session + 14-25 ms thread pools + 3-10 ms of work (Windows). Without one: full decode of the original (0.3 s at 4K 4:2:0, 1.3-1.5 s at 4K 4:2:2), plus for HDR a float tone-map at *full* resolution because the still applies it before the fit scale (`cli.rs:6819-6821`).
* **Playback**: start-up (spawn + graph + hw session, about 85-160 ms) on every seek / edit / proxy landing; steady state about 1 ms/frame from a proxy, 145-165 fps from a 4K original; transport negligible. The fixed 24 fps is a choice, not a limit.
* **Proxy generation**: 4:2:0 10-bit: decode 50 % -> readback-bound on a GPU; HDR tone-map (CPU `zscale` + `tonemap`, about 2 cores on average here) becomes the largest CPU cost once decode is hardware; scale 10 %, intra x264 only 10 %. 4:2:2: decode 70-80 % and nothing accelerates it on AMD.
* **Export**: for 4K->1080p, decode + x264 are about 50/50 of the CPU; with `d3d11va` + AMF the run is decode/readback bound at about 120 fps and CPU falls 10-13x. The composite graph itself (black base, pad, overlay) is about 5 %.
* **Import**: the same 4K stream is decoded by the proxy, the filmstrip and scene detection.

### Q2. Which hardware integrations pay off, per platform; which are risky?

Ranked within each platform. "Done" = already in the tree.

**Windows + AMD (the user; measured)**
1. `d3d11va` decode instead of `auto` (=dxva2) and no hardware session for proxy-fed work: WP1 + WP2. Verified bit-exact.
2. AMF H.264 / HEVC (and AV1) for **export** with a recalibrated quality map: WP4. At equal PSNR/size on this clip: 1.3-1.7x faster than `d3d11va` + x264 medium (1.4-1.9x faster than today's `auto` + x264) and 10-13x less CPU.
3. AMF for proxies: not worth it (WP5).
4. Not usable now: every FFmpeg GPU filter path (§3.7). 4:2:2: no hardware decode exists.

**Windows + NVIDIA / Intel [E]**: `auto` should pick dxva2 here too (its hw-config order puts DXVA2 first for h264/hevc), so WP2 applies; NVDEC via `cuda`, QSV via `qsv`. The largest extra win is a CUDA end-to-end proxy (`-hwaccel cuda -hwaccel_output_format cuda`, `scale_cuda`, `h264_nvenc -g 1`), SDR only; CUDA context creation per spawn (about 100+ ms, unverified) makes WP1 matter even more. Intel Quick Sync can decode 4:2:2 HEVC on recent iGPUs. Risks: NVENC consumer session limits, driver quirks. Everything here needs a machine.

**macOS (Apple silicon) [E]**: VideoToolbox decode/encode are already wired (`h264/hevc_videotoolbox`, `auto` -> videotoolbox). WP1/WP2 apply. Likely extra wins: M-series hardware decode of 4:2:2 10-bit HEVC (the only platform where R5 II footage would be accelerated) and `prores_videotoolbox` for ProRes exports (Kerf uses software `prores_ks`). No bundled FFmpeg on macOS, so results depend on the user's Homebrew build. Risks: HLG/PQ tagging with VT encode, odd-size and 4:2:2 handling.

**Linux**
* NVIDIA: `cuda` decode and nvenc already work through `auto` + the probe.
* AMD/Intel [E]: VAAPI decode works through `auto`; **no hardware encode at all**, because VAAPI encoders are excluded (`cli.rs:183-187`: they need `-vaapi_device` and a `format=nv12,hwupload` graph tail). WP11. Risks: distro FFmpeg builds, `/dev/dri` access in AppImage/Flatpak, `mesa-va-drivers` presence.

**Risk register**
* *Colour/HDR:* hardware **decode** is exact where tested (H.264, HEVC Main10, D3D11VA/D3D12VA/DXVA2) [M]; kerf-gpu already found a Vulkan ProRes decoder 24 levels off [D], so allow-list codec/pixel-format/API combinations rather than trusting `auto`. Hardware **encode** changes VUI tags, B-frame use and rate-quality behaviour per vendor. GPU **tone-mapping** changes the picture (§3.7) and the export would still use zscale, breaking "tone-map once, identically" (`engine-cli.md`).
* *Golden oracle:* it pins `decode_hwaccel()` through `build_preview_args_with` and builds export/still argv from `ExportOptions`; digests are identical under any `KERF_HWACCEL` [D]. Anything done at spawn time (`limit_args`, caller-chosen `opts.hwaccel`) cannot move a digest. Changing the AMF/NVENC spellings in `push_video_opts` moves `export.txt` (intended; bless once, tie to the encoder-family needles).
* *Driver flakiness:* the learned fallback `HWACCEL_OK` / `HW_ENCODE_OK` is process-wide, so one bad file disables hardware for the session; make it per (codec, pix_fmt) class.

### Q3. Can scrub stop spawning one ffmpeg per frame?

Yes, and the fixed costs show what is available (Windows, proxy frame):

| Approach | Per-frame latency | Evidence |
|---|---|---|
| Today | 149 ms | [M] |
| **WP1: same architecture, no hw session, 1 thread** | **55 ms** (Linux 31 ms) | [M] |
| Frame cache + one spawn decoding a window (WP6) | 55-70 ms for the first, about 1 ms each for the next ones | marginal cost [M] |
| Long-lived decode (kerf-gpu `FrameSource`), sequential | 4.4-4.9 ms at 720p/1080p, 27 ms at 4K | [D] |
| Same, jumpy scrub (restart per jump) | 23 / 65 / 217 ms at 720p / 1080p / 4K | [D] |
| Steady state inside one long-lived ffmpeg, decode + scale + composite + MJPEG | 0.8 ms/frame | [M] |
| In-process random access to an all-intra proxy (decode 2.7 ms one thread, plus scale + JPEG) | 3-5 ms | [M] pieces, [E] sum |

Options and verdicts:
1. **Cheap, now:** WP1 + WP6. No new architecture; removes about two thirds of the latency and all repeat costs.
2. **Structural, already built:** make kerf-gpu `FrameSource` + native surface the default scrub path once the Windows technique is confirmed on a real machine (not my package; its decodes are software, which this analysis supports). Jumps still pay a restart.
3. **In-process libav** (the `ffmpeg` feature exists for probe/experimental export): needs shared FFmpeg libraries in every bundle, and linking the **GPL** BtbN build into a PolyForm-Noncommercial app is a licence problem; only an LGPL decode-only build is plausible (decoders, swscale, mjpeg need no x264/x265). Large (L) and the licence question must be settled first. Not recommended before options 1-2.
4. **Packet-fed persistent ffmpeg** for all-intra proxies (parse the mp4 sample table in Rust, feed Annex-B + an AUD per frame so the raw demuxer flushes): about 5 ms random access, but fragile and it can only serve single-layer frames; superseded by option 2.
5. **MJPEG-in-MOV proxies** whose samples can be served as JPEG bytes without any decode: 2x the size of x264 intra (10.7 vs 5.5 MB per 8 s) [M], parity risk because the pass-through frame is full-range yuvj420p; interesting only for single-layer frames. Low priority.

### Q4. Cheapest big transport win?

None is needed for CPU reasons (§3.6): the per-frame cost is under 0.1 % of the budget. Ordered by value anyway:
1. Do not move frames at all: the native surface (kerf-gpu A2/A4) removes JPEG encode + transport + decode; that is the only "big" win and it is in flight.
2. Cut the round trip: send `{time, id}` (a few bytes, under the 8 KB direct-`eval` threshold) over the Channel and let the page load the JPEG with `<img src="frame://.../id">` from a custom URI-scheme handler (or `createImageBitmap(blob)` into a canvas). Removes base64 (-25 % bytes), JSON, the second IPC hop, and moves decode off the main thread. Needs a deliberate CSP `img-src` addition (`app.md`). Effort S-M; saving is milliseconds of latency and a little main-thread time, **only relevant above 1080p / 60 fps or when the main thread is busy** [E].
3. Raw `InvokeResponseBody::Raw` instead of JSON: saves the string build/parse and 25 % bytes but keeps the fetch hop; not worth a change alone.
4. WebCodecs from the all-intra proxy: possible on WebView2/WKWebView, unknown on WebKitGTK, and cannot show composites; it duplicates what kerf-gpu is for. No.

### Q5. How should the CPU budget and lanes account for GPU work?

Evidence: a hardware-end-to-end SDR proxy job used 0.56-1.0 CPU-s per 8 s of 4K (0.3-0.7 core) vs 11-17.5 CPU-s (about 6-8 cores) in software, yet occupies the one heavy slot; two such jobs run +32 % faster in aggregate, a third adds nothing [M]. With CPU tone-map the "GPU" job still uses about 2 cores. A background job slowed a concurrent scrub in my test, but the shared-box noise (baseline 77-126 ms) is too large to rank software against hardware jobs [M, inconclusive].

Proposal:
* Give the gate a **resource class**: `Cpu` (today) and `Gpu`. A job is `Gpu` only when its plan is hardware end to end (verified decoder for this codec/pix_fmt **and** verified encoder, no `zscale`/`geq`/`rotate`/`v360`/`drawtext`, no HDR chain). One `Cpu` job and up to two `Gpu` jobs may run together; lanes keep their meaning inside each class. A hardware attempt that falls back to software must acquire the `Cpu` slot before re-running (it already has a retry; make the retry take the lease and report "waiting").
* Keep `limit_args` for software stages but do not add encoder `-threads` to hardware encoders; cap `-filter_threads` instead. Keep `background()` priority: GPU jobs still use CPU for readback and swscale.
* Surface it: progress text "d3d11va -> h264_amf", a Settings switch (Auto / Off) next to the CPU budget instead of env-only `KERF_HWACCEL` / `KERF_HW_ENCODE`, and the verified decoders next to `hw_encoders`.
* NVIDIA consumer NVENC has a session cap [E]: keep a process-wide limit on concurrent hardware *encodes* (exports + proxies).
* Interactive reads stay ungated, but stop giving them the proxy-hostile defaults (WP1).

---

## 5. Work packages (ranked)

Every package that touches only spawn-time arguments or caller-chosen `ExportOptions` leaves the golden digests byte-identical; the check is `cargo test -p kerf-core --no-default-features golden` (no `.txt` diff) plus the pure-function unit tests and, for anything that runs ffmpeg, `-- --ignored` with `KERF_FFMPEG` set to both a 4.4.x/6.x and the pinned 9.0.2 binary. A/B on a Windows machine with `win-scripts/g9.sh`, `g10.sh`.

### WP1 - No hardware session and 1 thread for proxy-fed single-frame work (S)
* **What:** (a) a pure `wants_hwaccel(asset)`: hardware only for inputs that are not cache proxies and are larger than about 1080p (or whose long-GOP decode cost justifies it); the callers that spawn per frame or per play (`composite_timeline_frame` / `_region` at `project.rs:928, 946`, `export_still` at :1093, `stream_preview_once`, `decode_frame` at `cli.rs:1567`) pass `hwaccel: None` when every active input is a proxy; (b) single-frame stills use `cpu::cap_args(args, 1)` (add `-filter_threads 1 -filter_complex_threads 1`) instead of `limit_args(budget_threads())` at `cli.rs:6578`; stream start-up the same for proxy inputs.
* **Why:** [M] hardware session +70-80 ms per spawn; thread pools +14-25 ms; proxy decode is 2.7 ms.
* **Win:** Windows scrub 149 -> 55 ms (-63 %; hardware session removal alone 149 -> 69 ms), stream start -75 ms; Linux 55 -> 31 ms; NVIDIA likely larger [E].
* **Risk:** very low. The retry/learned-fallback paths are unchanged. Threshold evidence [M]: for 1080p H.264 even a sustained 240-frame decode is faster in software (298 ms vs 363-407 ms; hardware only saves CPU, 47 vs 281 ms), so hardware belongs on > 1080p originals and on background whole-file jobs, not on latency-sensitive reads. Mixed timelines keep `-hwaccel` for the original inputs (it is per input).
* **Platforms:** all. **Files:** `engine/cli.rs` (`run_still`, `stream_preview_once`, `decode_frame`), `project.rs`, `engine/cpu.rs` (a single-frame cap helper), `.claude/docs/engine-cli.md`.
* **Verify:** unit test for `wants_hwaccel`; golden unchanged because `build_still_args` / `push_inputs` still read `opts.hwaccel` as given; Windows A/B script.

### WP2 - Deliberate decoder selection (S-M)
* **What:** replace the `auto` default with a verified per-platform choice. A `hw_decoders()` probe in the style of `hw_encoders()` (decode a 2-frame clip that ffmpeg generates once, per candidate) with preference lists: Windows `d3d11va` (then `dxva2`, `cuda`, `qsv`), macOS `videotoolbox`, Linux `cuda` then `vaapi`; keep `KERF_HWACCEL` as override. Gate by the asset's recorded codec and `pix_fmt` (h264/hevc/av1/vp9 with `yuv420p`/`yuv420p10le`; **no hardware request for 4:2:2/4:4:4/12-bit**). Make the learned fallback per (codec, pix_fmt) class instead of process-wide.
* **Why/Win [M]:** `auto` is dxva2 on this machine (verbose log "Using auto hwaccel type dxva2"). d3d11va vs auto: decode-only 1.79 vs 2.86 s (CPU 0.39 vs 1.53 s), 4K original stream 1.47 vs 1.89 s, filmstrip 1.35 vs 1.63 s, scene detect 1.84 vs 2.34 s, export wall 3.45 vs 3.82 s (SDR) and 5.01 vs 5.42 s (HDR); CPU equal for SDR (25.4 vs 24.1 CPU-s) but 28.4 vs 37.7 CPU-s for HDR. For 4:2:2, `auto` adds +450 ms per single-frame decode (+43 %).
* **Risk:** low-medium: older Intel/NVIDIA drivers differ; the probe plus retry-in-software already exist. **Platforms:** all (preference list per OS). **Files:** `engine/cli.rs` (:120-172, new probe next to :207), `engine/source_probe.rs` (cache), `kerf-app` capabilities command, ExportDialog hwaccel toggle.
* **Verify:** unit tests for the pure preference/allow-list function; `export_capabilities` shows the verified decoders; golden unchanged (hwaccel is an input of the builders).

### WP3 - Whole-file video analysis reads the proxy when ready (S-M)
* **What:** `FfmpegScenes::detect_scenes` (`analysis.rs:151`), `Project::decode_contact_sheet` (`project.rs`) and optionally `salience_map` take `preview_source(asset)` (proxy if ready, else original). Keep the original as fallback and as a setting. Analysis already waits behind the proxy lane.
* **Win [M]:** 8.7x wall (4:2:0) and 17x wall / 34x CPU (4:2:2) for scene detection; removes the third full-resolution decode on import.
* **Risk:** medium: scores on a tone-mapped, crf-24 intra 720p proxy differ from scores on the original (the detector scales to 640 px anyway). Validate cut lists on real footage (HDR, 4:2:2, handheld) and re-tune the 0.3 threshold if needed. Not a GPU item but the single biggest saving for footage the GPU cannot decode.
* **Platforms:** all. **Files:** `analysis.rs`, `project.rs`, `engine/cli.rs:1215`, docs. **Verify:** golden unaffected (not a graph builder); compare scene lists original vs proxy on the corpus.

### WP4 - Hardware export encoders: calibration, AV1, quality, preset (S-M)
* **What:** (a) AMF: map CRF to QP with an offset (measured equal-size/quality: x264 crf 20 ~ `h264_amf` qp 26, `hevc_amf` qp 26) instead of 1:1; verify NVENC `-cq`, QSV `-global_quality` and VT `-q:v` the same way on their hardware; (b) add `-quality quality` for AMF (and `-preset` for NVENC/QSV is already there); (c) add `av1_amf` to `HW_ENCODER_CANDIDATES` and the Mp4/Mkv/Webm lists (works on RX 7000/9000 [M]); (d) an export preset "Fast (GPU)" shown only when `hw_encoders()` is non-empty; keep x264 the default; (e) run the `hw_encoders()` probes in parallel (about 1 s sequential here).
* **Win [M]:** 4K -> 1080p, 8 s: 3.45 s -> 2.0 s wall and 25 -> 1.9 CPU-s (SDR); 5.0 -> 3.8 s and 28 -> 2.7 CPU-s (HDR). Today's 1:1 mapping gives 1.9x larger files at +3.8 dB.
* **Risk:** medium. PSNR on noisy synthetic content is not what viewers see; x264 usually beats hardware encoders per bit on real footage (AMF H.264 here has no B-frames). Validate with VMAF on real iPhone/R5 clips before changing any default. VUI tags differ per encoder (`hevc_amf` tags bt709; others leave them unspecified).
* **Platforms:** Windows AMD verified; NVIDIA/Intel/macOS unverified. **Files:** `cli.rs` (`push_video_opts` Amf/Nvenc branches ~4023-4102, `quality_args` :2177, :188, container lists ~3020-3070), `frontend/src/lib/export-presets.ts`, `ExportDialog.svelte`, docs.
* **Verify:** this moves the AMF families in `golden/export.txt`: bless once with `KERF_GOLDEN_BLESS=1`, show with `KERF_GOLDEN_FAMILIES` that only AMF/AV1 cases moved. Add a real-ffmpeg ignored test per encoder that renders a clip and checks PSNR within a band of the x264 reference.

### WP5 - Proxies: hardware decode yes, hardware encode no (S)
* **What:** `proxy_hw_encoder()` (`cli.rs:2207`) returns `None` when hardware decode is active (and by default); keep `libx264 -g 1 veryfast`.
* **Why [M]:** d3d11va+x264 intra 1.90 s / 1.0 CPU-s vs d3d11va+AMF 1.85 s / 0.56 CPU-s (same wall); AMF proxies are 33 % larger (7.35 vs 5.54 MB per 8 s); one refusal turns hardware encode off for the whole process (`HW_ENCODE_OK`).
* **Optional:** a shorter GOP (x264 `-g 12`, no B) shrinks proxies about 3x (5.5 -> 1.9 MB / 8 s) at 1.7 ms/frame average decode [M], with a seek costing up to 11 extra decodes (about 20 ms single-threaded); only worth it for disk pressure and requires the persistent-decoder paths to tolerate GOPs (kerf-gpu's `FrameSource` already accepts a reuse window of 24 frames [D]). The proxy key would change (every proxy rebuilds).
* **Risk:** very low (software path is the reference). **Files:** `cli.rs:2207-2215, 2437-2439`, tests at `cli.rs:7446`. **Verify:** proxy argv is outside the golden oracle; existing proxy unit tests.

### WP6 - Amortise the spawn: composited-still cache and window prefetch (S-M)
* **What:** an LRU of composited JPEGs keyed by (working-timeline revision hash, frame index, width, proxy epoch); on a miss, produce a small window around the time from one process using the existing stream graph (`build_preview_args` unpaced, `-frames:v N`), and cache every frame. Neighbouring scrub steps then cost nothing.
* **Why [M]:** a spawn costs 55-150 ms, each further frame about 1 ms.
* **Caveat:** the stream graph is the export graph; the still deliberately does not reproduce static mid-transition blends (`engine-cli.md`). A prefetched frame can differ from the settled still on transition frames; either exclude transition frames from the window or accept the (closer-to-export) difference. **Risk:** low-medium (cache invalidation on edits is the hard part; key on a revision hash and the proxy-ready epoch). **Files:** `project.rs`, `kerf-app/src/lib.rs:1997`, `frontend/src/lib/frame-pump.svelte.ts`. **Verify:** golden untouched (reuses builders unchanged); tests that the cache never serves across a revision change.

### WP7 - Playback at the timeline's rate (S)
* **What:** drive `PLAYBACK_FPS` (`playback-sync.ts:11`) from the timeline fps, capped at 60 (`stream_preview_once` already clamps 1-60), falling back to 24 after a resync.
* **Why [M]:** the pipeline sustains >1000 fps from proxies; 24 fps playback of 30/60 fps footage is visibly choppy for no CPU reason. **Risk:** low (the resync gate exists). **Files:** `playback-sync.ts`, `Preview.svelte:188`. **Verify:** `playback-sync` bun tests; golden uses explicit fps already.

### WP8 - One decode, several consumers on import (M-L)
* **What:** for sources without hardware decode (4:2:2, HEVC 4:4:4, ProRes), run one ffmpeg that decodes once and writes the proxy and the filmstrip thumbnails (and optionally a scene-score stream) with `split`; keep the existing separate paths as the fallback.
* **Why [M]:** 3 full 4K 4:2:2 decodes (proxy, filmstrip, scenes) at about 25-35 CPU-s per 8 s each; fusing the first two halves the import cost; WP3 removes the third.
* **Risk:** medium: cancellation, cache sidecars, failure isolation, filmstrip is deliberately ungated and capped (`filmstrip.rs`) while the proxy is gated. **Files:** `cli.rs` (`generate_proxy_with`), `filmstrip.rs`, `proxy.rs`. **Verify:** proxy argv is outside the oracle; ignored tests that both outputs equal the separate runs.

### WP9 - A GPU lane in `cpu.rs` (M)
As §4 Q5. **Win [M]:** 0.3-0.7 core per hardware proxy job; +32 % aggregate with two. **Risk:** medium: classification must be right (a job that falls back becomes CPU-bound while holding only a GPU slot) and tests for the gate are timing-sensitive. **Files:** `engine/cpu.rs`, `proxy.rs`, `cli.rs` (`generate_proxy_with`, `render_attempt`), `kerf-app` settings. **Verify:** extend the existing gate tests; argv unchanged.

### WP10 - Tone-map after the fit scale in the composited still (S-M)
* **What:** in `build_still_args` (`cli.rs:6819-6821`) prefix the tone-map after the clip's fit scale with `force_divisible_by=2`, as `video_clip_chain` does.
* **Win [M]:** 4K HLG original scrub 627 -> 463 ms (auto), 354 ms with d3d11va; 4:2:2 -7 %. Matters only until the proxy lands or under "Original" preview, and for the agent's `preview_timeline`.
* **Risk:** low-medium: odd sizes (the earlier "HDR aborted on an odd size" bug). **Golden:** moves `still.txt` for HDR cases only; bless once. **Verify:** `KERF_GOLDEN_FAMILIES`, an ignored render test with an odd-height Contain fit.

### WP11 - Platform encoders and zero-copy pipelines that need hardware to build (M each)
* Linux VAAPI encode (`h264_vaapi`/`hevc_vaapi`/`av1_vaapi`): `-vaapi_device`, a `format=nv12,hwupload` graph tail appended to the complex graph (`-vf` cannot be combined with `-filter_complex` on one stream), probe like the others. Benefits Linux AMD/Intel, which has no hardware encode today.
* NVIDIA CUDA proxy: `-hwaccel cuda -hwaccel_output_format cuda`, `scale_cuda`, `h264_nvenc -g 1` for SDR; HDR stays on the CPU chain.
* macOS: `prores_videotoolbox` for ProRes exports; confirm 4:2:2 HEVC decode.
* **Not verifiable here.** Verify each in the real-machine list (§6); proxies are outside the golden oracle, export spellings move their family.

### WP12 - Persistent decoders (L, see Q3)
Promote kerf-gpu `FrameSource` after Windows confirmation, or evaluate an LGPL in-process decode. Out of scope for the engine fixes above.

### WP13 - Transport (S-M, low value)
Id-only Channel messages + custom URI scheme for JPEG bytes (CSP change) as in Q4. Do only after the above.

### Hygiene items (S)
* Run the `hw_encoders()` probes in parallel or at startup (about 1 s on the first proxy or export dialog here).
* `status_within` probes and the learned flags: log which decoder `auto` actually chose (`-v verbose` line "Using auto hwaccel type ...") when a hardware decode fails, for diagnostics.

### Explicitly not recommended now
FFmpeg GPU filter pipelines (§3.7): `scale_d3d11`, `scale_d3d12`, `vpp_amf`, Vulkan upload, `libplacebo`, `tonemap_opencl`. They fail or change the picture, and with system-memory frames do not win on speed. Revisit only with a newer FFmpeg/driver and a parity check (the kerf-gpu harness idea applied to the FFmpeg export).

---

## 6. What needs a real-GPU machine (or the real app)

Measured here on the target AMD/Windows hardware, but **launched through WSL interop and not inside the app**: all Windows numbers. Confirm in the packaged app: WP1/WP2 scrub and start-up (Rust `Command` spawn with `CREATE_NO_WINDOW` and the real thread caps), and the user-visible effect on playback restarts.

Not measurable here at all:
* NVIDIA (nvdec/`cuda`, `scale_cuda`, NVENC session limits, CUDA init time per spawn), Intel (QSV, 4:2:2 HEVC decode), Apple silicon (VideoToolbox decode/encode, 4:2:2 HEVC, `prores_videotoolbox`), Linux VAAPI (AMD radeonsi and Intel iHD), Windows on other GPUs.
* Whether `-hwaccel auto` really picks dxva2 on non-AMD Windows GPUs (I only saw it on AMD).
* WebView2 / WKWebView / WebKitGTK costs: Channel eval + fetch latency, `<img>` JPEG decode, a custom URI-scheme handler.
* Perceptual quality (VMAF) of AMF/NVENC/VT versus x264 on real iPhone and Canon footage, 8K R5 II files (decode throughput), Canon Log 3 handling (not flagged HDR by `source_traits`), Dolby Vision profile 8.4 streams.
* The foreground-vs-background interference test (§3.4 note): the shared box made the baseline vary 77-126 ms.
* Whether the failures in §3.7 are build- or driver-specific (BtbN 9.0.2 + Adrenalin 32.0.31041.1004): retest with another FFmpeg and driver.

---

## 7. Appendix: reproduction

* Windows (from WSL; `ffmpeg.exe` + media in `C:\Users\<you>\AppData\Local\Temp\kerf-gpu-bench`, deleted afterwards): `win-scripts/tw.sh N "<args>"` runs the exe through `t.ps1` and prints median wall and CPU. `g1/g2` proxy variants, `g3/g9/g10/g15` stills, `g4` stream, `g5-g7` export + quality, `g8/g12` OpenCL / libplacebo, `g11` filmstrip, `g13` scene detect, `g14` 4:2:2 export, `g16` export stage split, `par.ps1` concurrent jobs, `bg.ps1` interference.
* Linux: `bin/e_proxy.py`, `e_scrub.py`, `e_scrub2.py`, `e_gop.py`, `bench.py` (median wall + child CPU).
* Key production argv shapes used: proxy `-hwaccel auto -i SRC -an -vf scale='min(1280,iw)':-2:flags=bilinear,<tonemap_chain> -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -pix_fmt nv12 -f mp4 DST`; still `-hwaccel auto -ss T -i P -filter_complex "color=c=black:s=960x540:d=0.1[base];[0:v]trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]" -map [outv] -frames:v 1 -q:v 4 -f image2pipe -vcodec mjpeg pipe:1`; export graph with `color=...:d=8,format=yuv420p[vbase]` + per-clip chain + `overlay=eof_action=pass:enable='between(t,0,8)'`.
* Disk hygiene: all media, proxies, the Windows FFmpeg zip and the Windows temp directory were removed after the runs; only small scripts and logs remain under `scratchpad/review/gpu-media/`. Repo `target/` is 17 GB (untouched).
