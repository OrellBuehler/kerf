# Engine: the FFmpeg backends (`crates/kerf-core/src/engine/`)

## The `ffmpeg` feature — read this first

`ffmpeg-next` links the system FFmpeg dev libraries, which are **not always installed**.
Every crate has a default-on `ffmpeg` feature that forwards to `kerf-core/ffmpeg`.
In the workspace `Cargo.toml`, `kerf-core` is declared with `default-features = false`,
so the feature is **only** activated through these forwards — which is what makes
`--no-default-features` actually disable it everywhere.

**The engine has two backends** (`crates/kerf-core/src/engine/`):

- `cli.rs` is **always compiled** and drives the `ffmpeg` / `ffprobe` **binaries**
  (override with `KERF_FFMPEG` / `KERF_FFPROBE`). Probe, `silencedetect`, scene
  detection, preview frames (`frame_at`; `frame_jpeg` for a low-res JPEG), the
  per-asset **contact sheet** (`contact_sheet` — a `tile`d grid of frames sampled
  across a range, for skimming footage), the **salience map** behind smart crop
  (`salience_map` / `build_salience_args` / `score_salience`, the last two pure +
  unit-tested — one pass decodes ~48 tiny gray frames of a source window and scores
  each cell by edge energy plus frame-to-frame motion, so a locked-off talking head
  scores on detail and a follow shot on both; deliberately *not* face detection —
  no model to ship, and the answer only has to beat a centre crop) and the
  **composited timeline still**
  (`timeline_frame` / `build_still_args`, pure + unit-tested — overlays
  every clip visible at a timeline time onto a black canvas, mirroring the export
  geometry, so an agent can *see the cut*), the **cover frame** (`export_still` —
  the same graph and the same builder, but a `StillOutput::File` sink and no
  preview width cap, so the thumbnail is a real frame of the finished video at
  the delivery shape rather than a screenshot to crop back into agreement; the
  preview keeps its MJPEG pipe, which is why every pre-existing still test is
  byte-identical), waveforms, and export all live here, so
  they work in the `--no-default-features` build — only the binaries are needed,
  never the dev libraries. Preview decodes go through a **cached all-intra proxy**
  (`generate_proxy` in the background, `ready_proxy` never blocks, resolved by
  `Project::preview_source`; export always reads the original): `proxy_width`
  gives 1280 normally but 3072 for a spherical asset, because reframing crops
  ~100° out of the sphere and would otherwise leave ~355 real pixels. That width
  is part of the cache key, so marking an asset 360 rebuilds its proxy.
  **A proxy must answer `-ss T` with the frame the original does, including when
  the video starts late**: ffmpeg's input `-ss` is relative to the *container's*
  start (its earliest stream), and a proxy is video-only, so for a source whose
  video starts after its audio (`head_lead` = video start − container start,
  above 1 ms; audio at 0, video at 0.08 s) a plain proxy starts at the video and
  every seek lands `lead` seconds deeper into the footage than the export's.
  FFmpeg ≤ 6 hid this by accident (its default cfr mp4 sync pads the head, but
  regrids every frame to a grid anchored at zero — half a frame off for most
  leads, so still a frame out at every boundary); FFmpeg 7+ defaults an mp4 to
  vfr and keeps the late start, so the bundled 9.0 proxies were two frames out at
  half a second (a five-second lead: five seconds). `build_proxy_args(head_pad)`
  fixes only those sources: one clone of the first frame at t=0, merged in front
  of the stream by `interleave` (orders by timestamp, µs time base, so every
  other frame keeps its exact pts), with `-fps_mode vfr` spelled by
  `fps_mode_flag()` (`-vsync` before 5.1; 9.0 removed it). Not `-fps_mode cfr`:
  that regrids every frame and turns variable-frame-rate phone footage constant.
  The key gains `|lead` for these sources only (every ordinary file's key is
  unchanged, so nothing else is rebuilt) and the file is named `<hash>.lead.mp4`:
  that it is padded is a fact about the *file*, so it travels in the name
  (`is_head_padded_proxy`, pure) instead of a flag on `Asset` beside the swapped
  path. `source_traits` is the one cached ffprobe per file that answers HDR, the
  lead and the container — killed after 20 s, one probe however many threads ask, a failure
  remembered 60 s like `proxy_video_info`'s (`engine/source_probe.rs`'s `ProbeCache`, which the
  GPU frame source's seek-point probe sits behind too). The clone is a frame the original has no counterpart
  for, so a clip read from the proxy's start with no `-ss` (`clip_seek` 0 — a cut
  from the very head of the source) would hold it for `lead` while the export
  starts on the real first frame: `transition_fx` sets `ClipFx.head_pad` from the
  input's name and `video_clip_chain` opens that chain with `trim=start_frame=1`.
  Any seeked read skips the clone already. Ordinary assets get no such filter and
  an unchanged argv. **MPEG-TS is not fixed and is left alone**
  (`.ts`/`.mts`/`.m2ts`, `format_name` `mpegts`: lead reported 0, plain proxy,
  unchanged key): the TS demuxer measures the container start over the streams it
  reads, so the audio-less read rebases the video to zero and no pad can restore
  the original's offset — a late-starting transport stream still previews its
  seeks `lead` off. **Known export issue, not fixed here**: the export itself
  rebases a head clip's first video frame to the clip start (`trim=start=0` then
  `setpts=PTS-STARTPTS`), so for a late-start source a clip cut from the head has
  its video `lead` early against its own audio (the preview now matches the
  export, not the source's true sync); only clips that start past the lead are in
  sync.
  **A proxy describes itself.** `generate_proxy` writes `<hash>.json` beside the proxy
  (`<hash>.lead.json` for a padded one) *before* it renames the proxy into place, so a
  reader that finds the file finds its description: a `ProxySidecar` (version, the proxy's
  byte size, and the `StreamInfo` of **the proxy's** video stream — its size, always-`yuv420p`
  format, untagged-or-bt709 colour, upright rotation, probed with the same `probe` the import
  uses). `read_proxy_sidecar` accepts it only if the version is current and the size is the
  file's, so a replaced or half-written one reads as none. `proxy_video_info` is the reader:
  the sidecar, else **one** cached `ffprobe` (per file, size and mtime; a failure is
  remembered 60 s) written back as the sidecar — a proxy made before sidecars existed costs one
  probe, ever, and it spawns a process, so it runs off the project lock. `finalize_proxy`
  puts the encode in place: **if `dst` is already there** (a concurrent generator got there
  first) the new encode is dropped and **no sidecar is written** over the winner's; otherwise
  the sidecar goes first and the rename second. Each sidecar writer has its own temp name
  (`<hash>.json.<pid>.<n>.part`, a process-wide counter: two threads writing one sidecar must
  not share one), distinct from the proxy's `<hash>.<pid>.part` — a sidecar written through
  the proxy's name once renamed the half-finished proxy away. A test that generates a proxy
  into the user's cache cleans up with `test_support::ProxyGuard` / `remove_proxy`, which
  remove the sidecar as well (they used to leave `<hash>.json` behind). Only a
  probe is added: `generate_proxy`'s own argv, and every argv in the golden oracle, is
  unchanged. `proxy_path` still costs the one cached `source_traits` probe of the *original*
  (it keys the file on HDR and the lead), as it did before.
  **Whole-file analysis reads the proxy when one is ready** (never waited for: no file yet, "Always
  original" or size Off, or a proxy that fails to read, and the original is read). Scene detection
  (`detect_scenes_preferring(original, proxy, threshold)`, fed by `Project::scoring_proxy`) and a
  contact sheet (`Project::sheet_proxy`) swap the path and nothing else, so no golden argv moves.
  **The gate** is `proxy_fidelity`, from the cached `source_traits` of the original. *Times* (the
  sheet): an MP4 / MOV / Matroska / WebM source (a transport stream's pad is not fixed and every
  other container is unproven). *Scores* (scene detection): also SDR (no HLG / PQ transfer) and
  `luma_plane` — a planar or semi-planar YCbCr or gray picture, whose plane 0 is the luma that
  `select='scene'` scores and that a yuv420p proxy has; RGB (`gbrp`: the G plane; `rgb24`, `bgr0`:
  every interleaved byte), packed YCbCr, a palette, alpha, an unknown or unprobed `pix_fmt` read the
  original. Measured on the pinned 9.0.2 (`cli/scenes_on_proxy.rs`, `#[ignore]`d), through the software
  (libx264) proxy — a hardware-encoded one is unmeasured: cut lists frame-exact for H.264, 10-bit
  HEVC, 4:2:2 HEVC, a late video in MP4 and Matroska (the `.lead` pad's clone scores 0, and so does
  the real first frame it duplicates), a display-matrix portrait clip and variable-rate Matroska;
  the unpadded proxy of a late video is `lead` early. What a proxy does move is a frame's *score*,
  by at most ~0.02 on SDR YCbCr (grain, compression), so a cut scored within that of the threshold
  can flip; each fixture asserts its scores keep clear of it. HLG moves it by 0.20 to 0.21 (the
  original is scored in its code values, the proxy is tone-mapped) and RGB by 0.53 (cuts lost), so
  those read the original for scenes; a sheet only looks, so it takes the proxy (tone-mapped once).
  FFmpeg 6 and older write a constant-rate proxy, so a variable-rate source's cuts are within half
  a frame there. A sheet from a `.lead` proxy prefixes `trim=start_frame=1` when it starts at 0.000
  (a later seek has skipped the clone), or the clone takes a slot and every cell shows its
  neighbour's frame. Both reads are bounded: `-progress pipe:2` is added at spawn time and
  `output_unless_stalled` kills ffmpeg after a stretch of stderr silence, so a hung proxy read
  falls back to the original (a stalled original read is an error, not a short list of cuts).
  ffmpeg reports **nothing until a frame has reached the output**, so the bound has to fit the
  read: the scene pass selects frame 0 too (`eq(n,0)`, dropped again by `parse_scenes`, which
  skips showinfo's `n:0`; never a cut) and reports every second from the start, 60 s
  (`READ_STALL`) of silence being a wedge; a contact sheet's `tile` outputs only when the window
  is decoded, so its bound is `sheet_stall` = 30 s per second of window, between 60 s and 2 h
  (a 4:2:2 4K sheet is silent for the whole decode: 29 to 39 s measured).
  `salience_map` (smart crop, framing) stays on the original: its crop moved by up to 4.6 % of the
  frame width between original and proxy.
  **A proxy reports, can be abandoned, and cannot hang.** `generate_proxy_with(src, width,
  ProxyRun { reservation, duration, progress, cancel })` is `generate_proxy` with hooks (the
  latter is it with none): the encode runs through `run_ffmpeg_streamed`, which writes
  `-progress pipe:1 -stats_period 0.5` **at spawn time** (like the thread caps — `build_proxy_args`
  and the golden oracle do not move), turns `out_time_us / duration` into a fraction held under 1
  (the file is not in place until it is renamed), polls `cancel` between reports (kill, remove the
  `.part`, `Error::Cancelled`) and kills ffmpeg after `EXPORT_STALL` (300 s) without a word — it
  used to be a bare `.output()` that waited forever *while holding the machine's one heavy slot*.
  The first report (0.0) comes when the encode holds the slot, so a proxy queued behind an export
  is not shown building. **A stalled hardware decode is a hardware failure like a refusal**
  (`retry_proxy_in_software`, pure): it turns hardware decode off for the process and the
  proxy is run again once in software, instead of hanging 300 s per proxy; a cancel is never retried. Each
  encode's temp file is `<hash>.<pid>.<n>.part` (a counter), so a rebuild that cancels the encode
  before it does not share one with its replacement. **The size is a setting**: `proxy_width(projection)`
  = `proxy_width_for(projection, proxy_base_width())`, the base width being 1280 (the default, whose
  cache stays valid) or the 720 / 1080 Settings pick (`set_proxy_base_width`, driven by
  `proxy::set_proxy_size`); a spherical source keeps the 3072 / 1280 ratio capped at 3072 (set when
  proxies were hardware-encoded, which stops at 4096 across; the cap stays). The width is in the
  key, so each size is its own file. `remove_proxy_files(src, width)` deletes a proxy and its sidecar.
  **GPU acceleration**: `hw_encoders()` probes once per process which hardware encoders this
  ffmpeg can *actually* use: each compiled-in candidate gets a one-frame test encode, because
  `-encoders` alone is not proof (`KERF_HW_ENCODE=none` disables). Candidates are
  `HW_ENCODER_CANDIDATES` (NVENC, QSV, VideoToolbox, AMF incl. `av1_amf`: system-memory frames),
  then `VAAPI_ENCODER_CANDIDATES`. One thread per vendor, its encoders one at a time (consumer
  NVENC counts sessions); a test encode still running at 20 s is killed and ends its family or
  render node, and the `-encoders` listing gets 60 s (a cold first launch under a virus scan must
  not cache "none"). Surfaced as the `hw_encoders` command, the `export_capabilities` tool and
  the dialog's codec list plus a **Fast (GPU)** preset (frontend.md).
  **Proxies never encode in hardware** — deliberate, for NVENC / QSV / VideoToolbox users too:
  `generate_proxy_with` runs `libx264 -g 1` (`proxy_encode_args`, pinned by a test) with hardware
  *decode* only. Measured on a 7900 XT, 4K HEVC10 to 720p all-intra, 8 s: `d3d11va` + x264 1.90 s /
  1.0 CPU-s against `d3d11va` + AMF 1.85 s / 0.56 CPU-s, the same wall time, with AMF proxies a third
  larger (7.35 vs 5.54 MB) and one refusal turning hardware encode off for every later proxy and
  stitch. Only AMD was measured; the CPU saving elsewhere is unconfirmed. `HW_ENCODE_OK` is now
  the Insta360 stitch's alone (first verified HEVC hardware encoder, libx264 on any failure).
  **Export on AMF**: constant QP is `hw_qp(vc, crf)` (CRF + 6, at most 51; `av1_amf` CRF x 4, at
  most 255) and `-quality quality`. Source, the media review's 4K HEVC10 to 1080p30 8 s SDR render
  on an RX 7900 XT scored against a lossless render (PSNR of noisy synthetic content): x264 medium
  CRF 20 5.59 MB / 45.1 dB; `h264_amf` QP 20 (the old 1:1) 10.7 MB / 48.9 dB; QP 26 5.31 MB /
  45.5 dB (5.21 MB with `-quality quality`); `hevc_amf` QP 26 5.56 MB / 46.1 dB; `av1_amf` QP 90
  6.41 MB / 46.4 dB. The AV1 slope (the SVT-AV1 0..63 scale on 0..255) and the offset on real
  footage (VMAF) are **unconfirmed**, so x264 stays every default and "Fast (GPU)" is opt-in.
  NVENC, QSV and VideoToolbox are unchanged and unmeasured; the stitch's `quality_args` keeps AMF
  1:1 (a source decoded again). `-profile:v baseline` is no AMF name: `constrained_baseline`.
  **VAAPI export** (Linux AMD / Intel): the encoders take only frames on the device, so the builder
  names `-vaapi_device` (stand-in `VAAPI_DEFAULT_DEVICE`, builder stays pure) and the graph ends
  `[vhw]format=nv12,hwupload[outv]` after the titles (no `-pix_fmt`; validation allows only 8-bit
  4:2:0). At spawn `tune_export_args` swaps in the probed render node (`/dev/dri/renderD*`, four at
  most, or `KERF_VAAPI_DEVICE`; first node where anything encodes). An export that is the first to
  ask starts the probe on a thread and waits 10 s (`vaapi_device_within`), then keeps the stand-in
  node. Rate control `-rc_mode CQP -global_quality hw_qp(crf)` (lossless 1); the offset is the AMD
  one. Stitches and proxies never see VAAPI. Unconfirmed without a GPU: that an upload plus encode
  runs (the graph is rendered with the upload removed, `#[ignore]`d), `/dev/dri` in AppImage /
  Flatpak, the distro's `libva` (a missing one aborts the probe, read as no VAAPI). Background
  decodes (proxy, stitch, scene detection of an original, the composited still) use the same
  `-hwaccel` (default `auto`, `KERF_HWACCEL=none` to disable) with a learned
  software fallback shared with the preview path (`disable_decode_hwaccel()` is how a
  decoder outside the engine, `kerf-gpu`'s runs, teaches it one); the GUI defaults export
  `hwaccel` to `auto` too, and `render_with_progress` retries a failed
  hardware-decode export once in software so the default can never lose a render.
  **What each input is actually asked for** is `engine/decode_policy.rs`, applied to the
  built argv at **spawn** (`tune_inputs`; the builders still emit `-hwaccel` as the options
  say, which is what the golden oracle pins) from what is known of the file behind each
  `-i` — an asset's recorded codec / `pix_fmt`, or for a bare path the cached
  `source_traits` probe (taken only where the answer can depend on it, `facts_matter`: not on
  Linux / macOS for a long read). Rules, all pure and unit-tested (`resolve_hwaccel`): **a preview
  proxy (`is_cache_proxy`, by its `kerf/proxies/<hash>.mp4` name) is never decoded in
  hardware** — the session costs 70-80 ms a process against a 3 ms intra frame, and a
  sustained stream from it is faster in software too; **a one-frame read of proxies runs on
  one thread** (`-threads 1` ahead of each such `-i`, `cpu::single_frame_args` for the graph
  and the encoder; a still that also reads an original keeps the budget's caps, a 4K
  long-GOP seek needs them: one thread was 77 % slower); on **Windows `auto` becomes
  `d3d11va`** for H.264 / HEVC (`auto` picks DXVA2: slower, 4x the CPU), any other codec stays
  `auto` so one with no D3D11VA decoder is not handed a named one; a one-frame read of a
  **non-4:2:0 original (4:2:2, 4:4:4, 12 bit) asks for none** on Windows and Linux (AMD and
  most NVIDIA decoders lack it, the attempt is all cost; macOS is exempt, Intel iGPUs lose the
  attempt — name `KERF_HWACCEL=qsv`/`vaapi` to force it, a named accelerator always goes
  through verbatim); a long read (stream, export, proxy, scenes, filmstrip) still tries.
  There is **no size threshold** (the review's "hardware only above 1080p" is not
  implemented): a small original still asks. **A named accelerator exits non-zero with no
  device where `auto` fell back quietly** (ffmpeg 9.0.2 ends with `No device available for
  decoder` and `Hardware device setup failed`), so what a rerun in software teaches the process
  differs by path. **Unclassified, as before** — any rescue clears `disable_decode_hwaccel`'s
  flag, which the previews read: stills, frame grabs, the stream (before its first frame), scenes,
  salience, filmstrips. **Classified** — only a failure whose text names an unobtainable decoder
  (`looks_like_hardware_decode_failure`: those two phrases, `Unrecognized hwaccel`, a bad
  `hwaccel` option, `requires hwaccel`, and the pinned build's `implib-gen: ... failed to load library` abort (SIGABRT) for a hardware library — `libva-drm.so.2` missing makes every `auto` and `vaapi` run die on a Linux box without libva; *not* `Device creation failed`, `Using auto hwaccel` or
  `Failed setup for format`, which a successful `auto` run prints too, so a missing output
  directory or a full disk is not the hardware's) convicts, through `convict_hardware_decode`
  (which also sets the separate `HWACCEL_PROVEN_BAD`): `render_with_progress`
  (`retry_export_in_software`) and `generate_proxy_with` (the decoder is all the hardware a proxy
  uses; a *stall* convicts it before the rerun, via the unclassified flag only). **An export drops its symbolic `auto` before the argv is built
  (`export_hwaccel_request`) under `KERF_HWACCEL=none` or once `HWACCEL_PROVEN_BAD` is set** — not
  on the previews' flag, so one spurious preview rescue does not send every export to software;
  the GUI's "GPU decode" toggle still shows on meanwhile, and the first export after only a
  preview learned the device is missing still launches and reruns once. A named accelerator is
  never dropped — it fails and reruns once per export on a machine without the device. Stream and
  export decide whether a failed run may blame hardware at all from the rewritten argv (an input
  really handed `-hwaccel`), not from the request: **an export whose tuned argv has no
  `-hwaccel` (a proxy or image-only timeline) gets no software rerun**, because the rerun would
  be byte-identical (pinned by a test). Call sites: `tune_still_args` (`run_still`),
  `stream_preview_once`, `tune_export_args`, `decode_frame` / `decode_hwaccel_for`;
  `limit_frame_args` is the same for `kerf-gpu`'s per-layer decodes. **The Insta360 stitch is
  not tuned**: it keeps the plain request (`auto`, never `d3d11va`).
  **How much of the machine any of this may take** is `engine/cpu.rs`. FFmpeg is
  written to finish as fast as it can — every run grabs every core and nothing
  coordinates one run with the next — so an agent analyzing eight sources over
  MCP used to spawn eight all-cores, whole-file decodes at once (each buffering
  its PCM, so gigabytes too) and leave the desktop unusable for no wall-clock
  gain. Two moving parts: **one heavy job at a time** (`cpu::lease`, a reentrant
  gate — an export's second pass and a stitch inside an import must not queue
  behind themselves; **in three lanes** (`cpu::Priority`): **foreground** — an export (each
  variant), a levels measurement, a voiceover, an Insta360 stitch at import, a smart crop: whatever
  a user is actively waiting on, and the default lane of a plain `lease()` — goes before **proxy**,
  which goes before **background** — analysis steps and transcription, which run inside
  `cpu::in_background` so every whole-file decode they reach into queues there without knowing.
  Within a lane the slot goes in arrival order (tickets, not whichever thread the scheduler
  wakes). Foreground first because an export started on a freshly opened 80-clip project must not
  sit at 0% behind every proxy build; proxy before background because the preview is decoding the
  original until a proxy lands while analysis is only wanted eventually. **A proxy reserves its place
  when it is queued** (`cpu::reserve()` → `Reservation::lease_waiting`), not when its worker thread
  gets as far as asking: `spawn_proxy` runs in the import before the webview can start the analysis
  of the very file, and without the reservation the analysis took the slot while the worker was
  still in its ffprobe, then held it for a whole scene-detection pass over 5K HEVC. A reservation
  blocks background work, never the foreground. A job already running is never interrupted — a
  waiter waits for the step in flight, not for the queue. **A job that waits says why and can be
  stopped while it does**: `lease_waiting(on_wait, cancel)` calls `on_wait(Wait::Proxy | Wait::Job)`
  when it first has to wait (and again if the reason changes — never under the gate's lock) and
  polls `cancel` four times a second, returning `Error::Cancelled` and leaving its queue. The export
  says it through `ExportProgress.waiting` ("waiting for the preview proxy"; the dialog and status
  bar show it instead of 0%), analysis through the `waiting` stage, a voiceover through its `waiting`
  stage, a levels measurement only honours the Stop; a proxy waiting for its slot is cancelled by a
  delete / rebuild / settings change without ever starting. **Still one job on the cores at a time at
  any budget**: two at once would be two shares of the machine, which is what `cpu_percent` says one
  job may take) and **a share of the cores for that job** (`cpu_percent`,
  seeded from `KERF_CPU_PERCENT`, set at runtime by the app's settings). **Gated** =
  a whole-file job whose *result* is wanted later and that nobody is looking at:
  silence / scene / loudness detection, the PCM decode behind rhythm and in-process
  whisper, transcription, proxy, stitch, export. **Ungated** = whatever the UI is
  *drawing from* or an agent is *looking at*, which must not wait out a render:
  the moment reads (a scrubbed frame, the composited still, a clip's audio, the
  preview stream, a contact sheet) and the timeline's per-asset drawings — the
  waveform pyramid and the filmstrip — which do read the whole file, once, and cache
  it, but are what an opened project's clips are painted from. Ungated is not free:
  those are still thread-capped and run a couple at a time (the waveform and filmstrip decodes are niced
  too; a scrubbed still, a scrub window and the preview stream keep normal priority, being what the user waits on), and the
  filmstrip, a *video* decode that runs beside the proxy encode of the very file it
  samples, holds its own tighter cap (below).
  The share becomes `-threads` / `-filter_threads` / `-filter_complex_threads`,
  written in at **spawn** time (`cpu::limit_args` / `limit_cmd`) rather than in
  the pure argument builders, so those keep describing exactly what ffmpeg is
  handed; `-threads` goes in twice because ffmpeg assigns it to whichever *file
  group* it sits in — at the front for the decoder, immediately before the last
  argument (the output sink) for the encoder. Plus below-normal scheduling
  priority (`cpu::background`, a creation flag on Windows / `nice` on unix),
  which is the half that actually keeps the desktop responsive. At **100%** none
  of the second half applies: no flags, no priority change, byte-identical
  invocations to the ones Kerf always issued — for the jobs that were always
  issued. The one exception is by design: the filmstrip decode uses `cpu::cap_args`
  / `cpu::background_always`, which hold at every budget, because "at 100% leave
  ffmpeg alone" is for the job that owns the machine, and the filmstrip is the side
  job that must stay out of that job's way.
  **The hardware's slot** (`cpu::Resource::Gpu`, `MAX_GPU_JOBS` 2) is the one exception to one job at a time, and
  only an **export** can take it. An export that is hardware from end to end feeds the decoder and the encoder with
  0.3 to 0.7 of a core, so it neither waits for the cores' job nor holds it up, and two together finish about a third
  sooner than one after the other (a third gains nothing). Same three lanes (foreground, proxy, background; arrival
  order within one), though an export is foreground. **A preview proxy never takes it**: its encode is x264 on the
  cores whatever decodes it (`proxy_encode_args`: a hardware H.264 intra pass costs the same wall time, writes proxies
  a third larger, and one refusal used to turn hardware encoding off for the process), and hardware decode alone is
  the light part; so `generate_proxy_with` always leases the cores' slot, in the proxy lane, and the queue has one
  worker (`KERF_PROXY_WORKERS` overrides). The caller decides what qualifies and every doubt is the cores.
  `export_resource` (judged on the tuned argv): the lane on, a hardware `-c:v` (`enc_family`: NVENC, QSV, AMF,
  VideoToolbox, VAAPI), every input an image or a video with a **named** `-hwaccel` that decodes it (`auto` is never
  named: it falls back to software without a word, so on Linux and macOS only an explicitly named accelerator
  qualifies, and on Windows `decode_policy` makes `auto` `d3d11va`) and a stream the decoder has
  (`InputFacts::hardware_decodes`: H.264 4:2:0 at **8 bit**, since High10 has no D3D11VA, DXVA2 or VAAPI decoder and
  ffmpeg decodes it in software without a word, or HEVC 4:2:0 at 8 or 10 bit; an H.264 stream whose depth was not
  recorded is not assumed to be 8), and a graph whose filters are all in `HARDWARE_GRAPH_FILTERS` (scale, pad,
  overlay, fps, the VAAPI tail `format=nv12,hwupload`, ... and the sound ones; none of `zscale` / `geq` / `rotate` /
  `v360` / `drawtext` / a blur / a grade). `-vaapi_device` is a global option written ahead of the inputs and is not looked at (the
  encoder name and the upload carry the VAAPI export), and `tune_export_args` swaps in the render node the probe
  verified before the argv is judged. The rerun after a hardware decode failure asks no input for hardware, so it is
  the cores' and is a fresh attempt that asks for the cores' slot. `KERF_GPU_LANE=off` / `cpu::set_gpu_lane(false)`
  keeps every job on the cores' slot. Not measured on a GPU (none here): the gate and the classification are
  unit-tested, and the 0.3 to 0.7 core and +32 % figures are the review's, from an RX 7900 XT.
  Export is a **positional, multi-track** `filter_complex`
  (`build_export_args` / `build_filter_complex`, both pure + unit-tested): a black
  canvas with every video clip `overlay`'d at its `timeline_start` (later tracks on
  top, gaps fall through to black) and every audio-bearing clip `adelay`'d to its
  position and summed with `amix` — so clip positions, gaps and track layering all
  render. `ExportOptions.fit` decides what happens when the delivery aspect differs
  from the footage: `Contain` (the default, and the historical behaviour) scales to
  fit and pads, `Cover` scales with `force_original_aspect_ratio=increase` and crops
  — which is what makes the vertical / square presets produce a usable shot rather
  than a strip of picture in a black field. It sets the *base* fit only; a clip with
  its own transform still composes on top.
  **The shape itself is a property of the project**, not of one render:
  `Timeline.format: Option<Delivery>` (`{width, height, fit}`) is the frame the cut
  is being made *for*. `export_format` applies it between the footage-derived
  default and an explicit `opts.resolution` — so the shape follows the footage when
  unset (every pre-existing timeline, byte-identical graphs), the project frame when
  set, and a size typed into the export dialog always wins. Because
  `preview_resolution` and `build_timeline_frame_args` already derive their canvas
  from `export_format`, the streamed playback, the scrubbed still and the export all
  render the same frame from that one change — `Timeline::slice`/`for_render` carry
  `format` through so range export and playback keep it. `still_clip_chain` honors
  `fit` too (it used to letterbox unconditionally, so the one frame you looked at
  while cutting was the one shape you were never going to ship).
  Every track carries a **mixer strip**: `Track.volume` (the fader) and
  `Track.pan`. The fader rides each clip *after* its own gain and effect chain —
  a channel strip, so pulling a music bed down does not change what its
  compressor was reacting to — and the pan is a **balance** (`Track::pan_gains`,
  pure + unit-tested), not a constant-power law: the side you turn towards stays
  at unity and the other is attenuated away, because leaning a finished stereo
  track should not make it louder. Both are omitted from the graph at their
  neutral values, so every pre-existing mix is byte-identical, and the pan is
  dropped entirely on a mono delivery. A clip's `adelay` is whole milliseconds when its start is one, else an exact sample count (`adelay=NS`, `audio_delay`) — ms rounding put a bar-aligned splice up to 0.5 ms off its neighbour. Tracks flagged `Track.duck` are mixed
  into their own bus and
  `sidechaincompress`'d against the rest before the final sum (music dips under
  dialogue); With `MasterBus.duck_depth_db` set the duck is a **speech gate** instead
  (`duck_gate_chains`): a constant 1.0 through a `sidechaingate` keyed by the rest makes a
  0/1 "speech" curve (300 ms release = hangover), smoothed by a fast (7 Hz) and a slow
  (1.2 Hz) one-pole and maxed (rises in ~50 ms, falls over ~300 ms), and the ducked bus is
  `amultiply`'d by `1 - (1 - depth) * curve` — exactly `depth` dB under speech, verified
  rendered (±0.5 dB). Traps: a sidechain filter **ends with the shorter input** (the gate key
  is padded with `apad`; the compressor path still cuts a bed off when the dialogue ends first — a
  pre-existing bug, left alone because fixing it moves every ducking graph), a digitally
  silent key holds `sidechaingate` **open** (1e-6 floor), and `amix` silently drops a
  negative weight. `ExportOptions.loudness` (`LoudnessPreset`: youtube / spotify -14, apple
  -16, broadcast -23 LUFS, all -1 dBTP, `loudnorm` TP half a dB under) wins over the bool;
  youtube is byte-identical to `loudnorm: true`.
  `ExportOptions.loudnorm` appends a single-pass `loudnorm` to -14 LUFS
  on the final mix, and `ExportOptions.range` renders only a span by building the
  graph from `Timeline::slice(start, end)` (a shifted sub-timeline copy — boundary
  clips retrimmed honoring speed/reverse, keyframes resampled, overlays clipped).
  **The master bus** (`Timeline.master: MasterBus {volume, limiter, ceiling_db}`,
  defaulted and not written while `is_default`) is the last stage of the mix:
  after the final `amix` / duck sum and **before** `loudnorm`, `master_filters`
  (pure + unit-tested) appends `volume=` and `alimiter=limit=…:attack=5:release=100:
  level=0:latency=1`, each omitted while `is_neutral` (unity and no limiter — a
  stored ceiling alone changes nothing), so every earlier graph is byte-identical.
  Two `alimiter` options are load-bearing: its default **auto-levels** (scales the
  output back up so the peak sits at full scale — the ceiling would become makeup
  gain), and without `latency=1` its lookahead delays the whole mix by the attack
  and drops its tail (sound out of step with the picture); an ignored test fails
  without either. **`latency` is probed, not assumed**: FFmpeg 4.4 (Ubuntu 22.04's
  system ffmpeg) has no such option and refuses the whole graph with `Option
  'latency' not found` — every limiter-on export and every `get_levels` —, so
  `alimiter_latency_available()` (once per process, like `zscale_available` /
  `graph_script_flag`: `ffmpeg -h filter=alimiter` lists a `latency` line, pure
  `help_lists_latency`; a binary that will not run reads as having it) decides
  whether `master_filters` spells `:latency=1`. Without it the limiter is emitted
  as `…:level=0` and the mix trails the picture by the 5 ms attack and loses its
  last 5 ms (under a frame at any rate; nothing compensates it). A `cfg(test)`
  thread-local (`with_alimiter_latency`) pins the answer for the unit tests and the
  golden oracle; an ignored test checks the probe against a real `alimiter=…:latency=1`
  run, and the levels tests run on 4.4 as well as 6.1 and 9.0 (their delay check is
  skipped where the option is absent). The ceiling is a **sample**-peak ceiling (no
  oversampling), rounded to six decimals so the text never rides on a libm `pow`'s
  last digit; `get_levels` reports the true peak, which runs *above* the sample
  ceiling (measured on 9.0.2 at a -1 dBFS ceiling: an 11 kHz tone read -0.2 dBTP,
  15 kHz +0.1) — which is why the default ceiling (`MASTER_DEFAULT_CEILING_DB`) is
  **-1.5 dBFS**, `loudnorm`'s own TP target, and not the -1 dBTP that platforms ask
  for. `for_render` / `for_delivery` / `slice` carry
  it (range export and variants keep the mix; the playback stream has no sound),
  `safe_volume` / `safe_ceiling_db` re-clamp in the builder (a `.kerf` never passes
  the clamping ops), and `DiffKind::MasterChanged` stops a master-only agent
  proposal diffing as empty and being discarded by `apply_staged`.
  **Levels** (`Project::levels_inputs` → static `measure_levels` → `engine::mix_levels`,
  `engine/cli/levels.rs`; lock-free split again): one ffmpeg pass over the export's own
  audio graph. `build_filter_complex_metered` is `build_filter_complex` plus taps
  (byte-identical with metering off): each track's clips are summed into a submix,
  `asplit` into an `ebur128@t<i>` meter, and the finished mix — after the master and
  the optional `loudnorm` — is tapped last, so the master reading is what the file
  contains (an ignored test renders the file and reads it back). A track's reading is
  its strip output, before the duck bus and the master; every tap reads sample + **true**
  peak (`peak=sample+true`: the oversampling roughly doubles a meter's cost — measured
  about 5 ms of work per second of audio per tap — and which track is hot is the
  question). `MeterParser` reads stderr line by
  line — frame lines carry the short-term maximum (`-120.7` until the first 3 s
  window closes is `None`), the `Summary:` block is one log call so only its first
  line is prefixed — and `-inf` / the -70 LUFS gate floor read as `None`. Whole-file,
  so `cpu::lease` + thread caps; a stall watchdog (120 s with no stderr line; a meter
  logs ten a second) and `cancel` kill the child. `Levels.notes` is the advice
  against -14 LUFS / -1 dBTP, and it takes the **master bus** the mix went through
  (`Levels::new(.., &timeline.master)`, `level_notes`): over -1 dBTP with the limiter
  off says turn it on, but with it **already on** says lower its ceiling by the
  overshoot plus 0.5 dB (`LEVELS_CEILING_MARGIN_DB`, clamped to the filter's floor; at
  the floor, lower the master instead) — the old advice looped an agent that followed
  it, because the limiter holds the sample peak and the overshoot is between samples.
  A track over 0 dBFS before the master is told so without claiming a clip: the graph
  is float, so nothing clips until the mix is written and the master can still bring
  it under. `levels.ts` `levelNotes` mirrors the words exactly (a string in both
  languages' tests pins that).
  **`Clip.mask`** cuts a clip to a rectangle or ellipse (centre / size in
  fractions of the rendered frame, feathered, optionally inverted): outside it
  the clip goes transparent and a lower track shows through. Deliberately *one*
  primitive that composes with the track stack rather than a masking mode per
  use — a blurred face is a duplicated shot on the track above, blurred and
  masked; a region grade is the same with a colour. That is also what keeps it a
  single filter in the linear per-clip chain (`mask_filter`, a `geq` rewriting
  only the alpha plane — no branch in the graph): one expression covers both
  shapes, each axis scaled so the edge is at distance 1, `max` for a rectangle
  and `hypot` for an ellipse. `geq` is per-pixel and slow, the cost keyframed
  opacity already pays.
  **Video fades are timed on the timeline**: `video_clip_chain`'s `setpts` has
  already moved the frames to `timeline_start`, so every `fade` (in / out, dip,
  dissolve) starts at `timeline_start + …` — timed from 0 they blacked out any
  later clip with a fade-out and dropped its fade-in and transitions. Audio is
  re-based to the clip before `adelay`, so its `afade`s stay clip-local.
  **The scrubbed still draws fades, dips, dissolves, slides and pushes as the file does**
  (`engine/cli/still_fx.rs`; it used to show each clip's own span unfaded — the incoming clip alone
  inside a dissolve, 176 levels off the export's frame — and the agent's `timeline_frame`, the cover
  and the preview fallback all went through that). `still_layers` takes the clips from
  `active_video_clips_with_tails` (an outgoing clip stays a layer for its `ClipFx::tail`, read from
  the source it borrows) and, per layer, the `ClipTiming::fades` still changing the picture at output
  frame `k = Rational::frame_containing(t)` and the slide / push travel at `ffmpeg_frame_time(k)`
  (added to the overlay position; `overlay` truncates it to pixels as it does in the file). A fade
  is the export's own `fade` filter, run where the export runs it (after the rotation, ahead of a
  moving zoom) on the frame restamped as frame `k` of a `1/fps` stream
  (`settb=expr=den/num,setpts=k,fade=…,setpts=PTS-STARTPTS`: a `fade` counts frames of its input
  time base, and `overlay` shows a secondary frame only from its timestamp on). A chain with no alpha
  plane ends `format=yuv420p` after the fades (`yuva420p` for an alpha source), as the export's does:
  left to negotiation `fade` runs on the `yuva420p` the overlay asks for, where black is 0 and not
  the limited-range 16, up to 17 levels off on the way to black. A clip that entered through a
  dissolve is on an alpha plane for good in the export (`xfade_in > 0`), so a later fade of its own
  or a dip out runs on `yuva420p` too: the alpha decision is made from the clip's whole `fades()`
  list whenever a fade is in progress (`StillFades::alpha_chain`), not from the ramp alone. A tail
  is open for the frames whose slot starts inside its window (`active_video_clips_with_tails`'s
  `tail_at`: the exact slot start plus a nanosecond), not for the still's raw `t`: a window of
  18.3 frames closes inside one, and that frame still draws the tail (on the grid the frame that
  starts at the window's end has none). A fade that is not changing the
  picture is left out, so every still outside a transition keeps its argv (golden: 1581 of 5600
  still cases moved, exactly the new `still-fade` / `still-tail` / `still-travel` families; export
  and preview 0). **Everything that renders a still now returns the export's frame inside a
  transition**: `timeline_frame` / `preview_timeline`, the preview fallback and `export_cover`, so a
  cover at `t = 0` of a clip with a fade-in is black (or mid-fade at a later `t`), as the video's
  frame is. Measured (`still_rendered.rs`, `#[ignore]`d, 6.1 and 9.0, 24 to 60 fps): every
  pixel of every frame of a dissolve (also from black, from a still image, with a reversed or 2x
  outgoing clip, a transformed, graded, translucent or zooming incoming one, over another track,
  with the clips' own fades, one that fades or dips out after its dissolve, and a 0.61 s transition
  at 30 fps asked for at the start, middle and end of every frame), a dip to black, a fade-in / out
  and the eight slides and pushes is equal to the export's; a dip to white is up to 2 levels off outside its fade, where the file's
  `fade c=white` at full strength still rounds. The still's frame *pick* is unchanged (`-ss`, not
  the `fps` filter's), so moving footage can differ from the file by a source frame as ever.
  **Scrubbed frames (`engine/cli/scrub.rs`, `still_cache.rs`).** A still is one process (55 to 150 ms, of which 3 to
  10 is the frame), so `ScrubFrames` (one per process, in `AppState`) pays the process once per **window**. `frame()`
  quantises `t` to the cut's frame grid (`Rational::frame_containing`, drawn at the slot's start as the export draws
  it; the raw `-ss` landed a source frame late) and serves it from a bounded LRU (`KERF_SCRUB_CACHE_MB` 64, 2048
  entries). **The key is the still's own argv** (`StillKey`: a hash of `build_still_args_rendered` at that frame
  plus the size and mtime of every `-i`), so nothing is invalidated: an edit that changes the frame, a landed proxy
  or a replaced file is another key, an edit elsewhere leaves it, an undo is a hit.
  - **A miss starts one window**: the export graph over `Timeline::slice`, `-frames:v n` (24, `KERF_SCRUB_WINDOW`),
    its reader filing each JPEG under the key of the still it stands for; the request returns when *its* frame is
    out. A request the running window will draw (its own cut says so, asked off the lock) waits, any other kills it
    ("latest wins"). A scrub within 8 frames of the end of a finished window starts the next (`read_ahead_frames`).
  - **Lead.** A window starts 1 frame before the one asked for (2 after it going back), so the step the other way is
    a hit. That holds over an original too: reaching `k` the process decodes every frame from the keyframe, so `k - 1`
    costs a filter and a JPEG, and without it a click then a step back is a miss that starts a window of its own (870 ms
    against 0.1 on a 1080p long-GOP original, 370 for the one-shot). A drag back is one window behind the frame:
    over originals a short window at the frame for the first step back saved 160 ms on that step and cost 1.2 s on the
    drag, and a short first window doubled a forward drag from a cold start, so neither is used.
  - **Lifecycle.** Spawned off the lock (`Reading::Window`, one thread when every input is cheap: 192 to 109 ms for
    three proxy layers). Killed by `stop()` (playback), `clear()` (project opened), an edit or jump, a reaper (10 s
    with nothing drawn, 5 s between frames; parked on the condvar when idle) and `Drop`. `stop` / `clear` bump an
    epoch, so a request already waiting or a read-ahead already planned starts nothing after. A window that died or
    stalled by itself is not restarted for the same request (the one-shot answers); one another request replaced is.
  - **Window frame = the still, or the export where the still is off** (`scrub_rendered.rs`, `#[ignore]`d, 6.1 and
    9.0, 20 cases): byte for byte on 9.0 for plain cuts (30, 29.97 and 23.976 cuts, 60 or 2x sources, offsets, titles
    incl. an animated one over a box, captions), the export's frame on a reversed clip, the still's picture to the
    encoder's rounding over another track, an image, Cover, a grade. **On 6.1 the still is the odd one out on every
    cut**: its bare `color` base (the window's has `format=yuv420p`) lets `overlay` settle on another pixel format,
    14 levels off on one-pixel chroma lines; the window is the export. `Snapshot::plain` refuses (the one-shot draws
    it) a fade, dissolve, slide, push or tail, a source that is not a whole multiple of the cut's rate (24 or 25 in 30,
    1.5x, 0.5x: the `fps` pick follows where the slice starts), the last frame of a title (`drawtext`'s `between`
    includes `end`), 360 and HDR.
  - **Measured** (`tests/scrub_bench.rs`, release, 1280x720 proxies at 960 px, 4 shared cores, 3 runs;
    `docs/review-2026-10-10/after/perf-4/`), 1 layer / 3, FFmpeg 9.0.2: one-shot 50-71 / 94-119 ms; first frame of a
    window 71-89 / 104-132 (10 to 20 ms *more* than a still); next frames of a 60 Hz drag 0.0-0.1 / 4.8-9.0 ms; a held
    frame 0.01 ms; twenty steps 1.0-1.4 s / 1.9-2.4 s against the drag's own 0.32 s. Originals, backward drags and
    jumps: the README there.
  **What a clip does in time is `clip_timing.rs`** (pure, `pub(crate)`, no dependency
  on the engine): `ClipFx` (what its transitions gave it: tail, dissolve, dips, travel,
  HDR, head pad), `transition_fx` (indexed by flat clip *storage* index over the
  `for_render()`ed timeline and the graph's own asset list; `clips_with_fx` pairs each
  clip with its entry), `clip_source_window` / `clip_seek`, `is_head_padded_proxy` and
  `ClipTiming`, whose `window` / `fades` / `motion_keys` the builders *format* into
  `enable=`, `fade=` and the `overlay` x/y expressions, and whose `enabled(t)` /
  `motion_at(t)` / `FadeStep::progress_at_frame` a frame renderer *evaluates* — one
  decision, two readers. `fades` is video-only (`audio_clip_chain` composes the same
  `ClipFx` its own way and shares only the duration, window and seek). **The number is
  not what FFmpeg does with it**: `enabled` is the overlay's `enable` predicate (closed
  at `end`), necessary and not sufficient — an equal-rate source is drawn on
  `start <= t < end` and nothing at `end`; graph expressions are evaluated at
  `ffmpeg_frame_time(k) = k * (den / num)`, an ulp off `k / fps`, which drops the first
  frame of a third of the frame-aligned clips at 24 fps and half at 29.97; and a `fade`
  counts frames (`S = round(st * fps)`, `N = round(d * fps)`, `i / N`), it does not
  interpolate time. Exact rationals are for the frame pick alone: `Rational` is the delivery
  rate as FFmpeg parses the text the graph prints (`av_d2q(x, 1001000)`, ported and checked
  against `showinfo` on both builds: `29.97` runs on 2997/100, `29.97002997002997` on
  30000/1001), with `frame_time(k)` for what the graph evaluates and `exact_time(k)` for the
  pick. Each is pinned against
  rendered pixels by `#[ignore]`d tests in `engine/cli/rendered.rs` (both FFmpegs). Moving
  the extraction changed no argv byte: the golden oracle below is the proof, and the next
  change to these numbers has to go through it. Shared test fixtures (assets, streams,
  clips, tracks) live in `engine/test_support.rs`.
  The per-clip chains (`video_clip_chain` / `audio_clip_chain`) also realize
  each clip's **video effects** (`gblur`/`unsharp`/`hue`/`negate`/`vignette`, and
  `chromakey` which keeps alpha so a lower track shows through), **audio effects**
  (`highpass`/`lowpass`/`equalizer`/`acompressor`/`agate`) and **transform keyframes**
  — animated zoom via `scale=eval=frame`, animated position via the `overlay` x/y
  expr, rotation via `rotate`, opacity via `geq` (all driven by piecewise-linear
  `keyframe_expr` over clip-local time). **An expression may nest about 100 levels** (libavutil's
  `av_expr_parse` stack: the whole text, each bracket and each function argument is one level; the
  next is `EMFILE`, which the filter reports as `Invalid argument` — the 101st on 4.4.2, a level
  sooner on 9.0.2) **and a chain of `if(lt(..))` is a level per polyline point**, twelve to an
  eased segment: ten eased keys failed the export *and* the playback stream. So above
  `KEYFRAME_TREE_POINTS` (24) points `keyframe_expr` writes a **balanced binary tree** over the
  segments (`if(lt(t,t_mid),left,right)`, the head and tail holds wrapped around it): `log2(n)`
  levels, and `log2(n)` comparisons an evaluation where the chain walked all of them (a `geq`
  pays that per pixel). Up to the threshold the text is the chain every earlier graph carried, so
  the golden oracle did not move. Chain and tree pick the one segment whose span holds the time
  (a step's empty span is never reached), so they evaluate alike: the sweep's evaluator enforces
  the limit (`NESTING_LIMIT`) and holds the tree to `interpolate`, a unit test bounds the nesting
  of 40 eased keys, and `keyed_zoom.rs`'s `a_clip_with_many_eased_keys_exports_and_plays_back`
  renders thirteen eased keys on every animated channel through the export and `stream_preview`
  against `transform_at` (it failed on both FFmpegs before the tree). Anything else that writes
  an expression per input item must be a tree too. **The keyframed zoom is the one stage that changes a
  picture's size from frame to frame, and almost nothing after it can follow**:
  `format` negotiation inserts a fixed-size converter (`overlay` takes `yuva420p`
  only, so a chain ending `format=yuv420p` got one), and `geq` / `rotate` / `eq` /
  `gblur` / `zscale` read the frame size once, when the graph is configured — a filter
  after a `scale eval=frame` was pinned to the *first* frame's size for the whole clip, so
  a scale-only zoom never showed, a keyed opacity ramp and a keyed rotation ran at the
  first frame's geometry, and the zoom itself sat before `fps` and was read at the
  *source* frame's time (a 10 fps clip in a 30 fps export zoomed in three-frame steps).
  So when `Clip::zoom_animated()` (the keyed scale actually moves; position / rotation /
  opacity-only keys keep their chain, bar the rotation's fill below) `video_clip_chain` puts the zoom **last**: crop,
  fit, `setsar`, **`fps`**, tone-map, `eq`, effects, `format=yuva420p`, chroma key,
  mask / opacity `geq`, `rotate`, fades — all at the constant fit size — then the
  `scale ... eval=frame`, then `format=yuva420p`, then `overlay`, which reads every
  picture's own `w`/`h` per frame (`(W-w)/2` centres it as it grows) and takes the
  yuva format natively, so nothing sits between. The zoom is therefore evaluated at the
  **output** frame's time, on both FFmpegs. Two costs of that order, both bounded: an
  effect, mask or `rotate` now acts on the picture at fit size and the zoom magnifies the
  result (a blur grows with the zoom, as in an NLE's effect-then-motion order; a hard mask
  edge is as soft as the zoom is large), and those filters run on the fit-size picture even
  when the zoom shrinks it (`geq` is ~150 ms a frame at 1080p, so a keyed-opacity clip zoomed
  out pays what the fit-size clip would — `KERF_ZOOM_COST=1` times it, `keyed_zoom_cost`).
  **The scrubbed still follows the same order** (`still_clip_chain(.., zoom_last)`, fed
  `Clip::zoom_animated()`): it used to zoom first, so its blur was the same softness at every
  zoom while the export's grew with it (a sigma-6 edge was 26 px wide in the still and 18 in
  the file at a zoom of 0.5; the two now agree: 18 and 18 there, 56 and 56 at 1.6). Every other clip's still, and every other
  clip's export, zooms first as ever, so **there is a step where the keys stop being equal**
  (1.6 to 1.6000001): a blur or a hard mask edge is a zoom's factor softer on one side of it
  than the other, because "unkeyed or held" and "moving" are two orders and an effect's size is
  defined by where in the order it sits. It is inherent to leaving every held and unkeyed clip
  byte-identical; moving the held keyed clips across too would only move the step to the first
  keyframe. **`rotate=…:fillcolor=none` is not
  transparent**: `none` means "do not fill", the corners keep whatever the buffer `rotate`
  reuses held, and a rotation that *moves* leaves every earlier pose behind (the "erratic"
  rotation; the opaque footprint of a rigidly turning rectangle grew 44k → 64k pixels on
  6.1 and 9.0 alike), so the keyed rotation fills `black@0`; the static rotation, whose
  footprint never changes, keeps `none` and its argv. The rendered tests
  (`engine/cli/keyed_zoom.rs`, `#[ignore]`d, both FFmpegs) measure the picture and the
  blue box inside it on **every output frame** against `Clip::transform_at` and every
  eighth against the scrubbed still, for the zoom alone and with position, opacity,
  rotation, crop, mask, effects, grade, fades, HLG, Cover, a still, speed, reverse, every
  transition, a range export, every frame rate, a slow source, a shared input and the
  playback stream (`KERF_ZOOM_KEEP` keeps what they rendered, `KERF_ZOOM_VERBOSE` prints
  the frames worth a look); `rendered.rs`'s `a_keyframed_zoom_is_read_at_the_output_frame_…`
  is the mechanism, straight off the filter. The plan (`render_plan`) mirrors the order by
  refusing: a Motion plan refuses a moving zoom (`Unsupported::KeyedZoom`, until
  `GpuCaps::keyed_zoom`), and a Still plan refuses one with a grade, rotation, fade of opacity,
  mask or effect in front of it (`Unsupported::ZoomBehind`) — a pure zoom, whose two orders are
  one chain, is still drawn; `Animated.zooms` is `Clip::zoom_animated()` itself, which holds every
  key against the first one *in time*. Three more graph bugs of the same shape are fixed beside
  it. **A tiny scale snapped to full size**: `scale` reads a width or height that evaluates to 0
  as "unset" and keeps the input's, so 0.0004 of a frame drew at 100%; below `TINY_SCALE` (0.01,
  on the static zoom, any key of a keyed one, and the still) the sizes are `max(1, ...)`
  (`zoom_scale`), and above it no graph has changed. **HDR footage aborted on an odd size**:
  `zscale` (the tone-map, which follows the geometry) refuses a size not divisible by its
  subsampling, and 4:3 footage Contain-fitted into 9:16 is 405 rows high, so for an HDR clip the
  Contain fit says `force_divisible_by=2` and a constant zoom is `max(2,2*trunc(x/2))` (a moving
  zoom runs after the tone-map and needs neither; SDR is byte-identical). **Alpha sources were
  flattened**: a chain with no alpha plane of its own ended in `format=<pix_fmt>`, which has none,
  so a transparent PNG sticker or an FFV1 clip cut-out drew as a whole rectangle where the still
  (no terminal format) kept the cut-out, and a moving zoom (terminal `yuva420p`) flipped it back;
  `ClipFx.alpha` (`Asset::has_alpha`: the probed pixel format of the first video stream is an
  alpha one; an unrecorded format is not) ends such a chain in `format=yuva420p`.
  **Per-property channels in the graph.** A clip's transform is built number by number from
  `Clip::is_keyed`, not from "is it animated": `video_clip_chain` writes a keyed scale as the
  `scale eval=frame` (last, when it moves), a keyed rotation as the `black@0`-filled `rotate`, a keyed
  opacity as the `geq` alpha, and builds every number it does **not** key from the static transform as
  an unkeyed clip does (a constant `scale`, a constant `rotate=…:fillcolor=none`, `colorchannelmixer`);
  the overlay's `x` / `y` are each the curve or the static offset (`curve_or_static`). Any keyed number
  makes the clip "animated" for the pad / centre decision exactly as the bundle did, so a bundle clip
  (all five keyed) is byte-identical. **A keyed colour is an `eq` with `eval=frame`** (`eq_filter_keyed`):
  each keyed number a quoted expression of the frame's time `t` — `t` is the frame timestamp, which
  `setpts` has put on the timeline, so clip-local time is `(t-start)` as everywhere else — the unkeyed
  numbers stay the plain numbers, the temperature is `gamma_r='1+0.3*(…)'` / `gamma_b='1-0.3*(…)'`.
  **Measured, on FFmpeg 4.4.2 and 9.0.2 alike**: every `eq` number (`brightness`, `contrast`,
  `saturation`, `gamma`, `gamma_r`, `gamma_b`) accepts an expression under `eval=frame`, the centre
  pixel of every fifth output frame is **0 levels** from the static-`eq` still of the same moment
  (3 with a keyed opacity beside it: the still takes a constant opacity through the RGB round trip, the
  file a `geq`) at 24 / 25 / 29.97 / 30 / 60 fps, late on the timeline, at speed 2 / 0.5 / reversed,
  under a moving zoom, beside a static grade and in a range export
  (`engine/cli/keyed_channels.rs`, `#[ignore]`d; without `eval=frame` it fails at the second frame). A test
  picture's luma must sit away from 128 or a `contrast` ramp does nothing visible (it pivots there).
  **A keyed volume** is `asetnsamples=n=128:p=0,volume='…':eval=frame` placed *after* `atempo`, so its `t`
  is the clip's own playing time (checked at speed 0.5 / 2, reversed, late on the timeline, range
  export). `volume` holds one gain for a whole frame and decoders hand over 1024 samples (21 ms) — a fade
  would step audibly — so the frames are cut to 128 samples first (2.7 ms at 48 kHz); **`p=0` is
  required** (the default pads the last frame with silence). With that, on both builds, the render is
  the unkeyed render scaled **sample for sample** by the curve at the start of each 128-sample frame (worst
  difference 1.5e-8 in float, five easings), and the frame size is a lag of at most 2.7 ms against the
  curve. One knife edge: a hold's step that lands exactly on a frame's start flips on the float rounding
  of `t` (2.2 s is frame 825 and read as "before the step"), so the tests keep steps off that grid.
  `sweep.rs` evaluates the export's `eq` numbers and `volume` expression at every output frame / time and
  holds them to `layer.color` / `Clip::volume_at` (and that a plan's turn or opacity is *realised* in the
  graph — it caught the partial-keying branches in mutation checks).
  **Any such expression must be quoted in
  the filter value** — it contains commas, and an unquoted comma is where the
  graph parser thinks the filter ended; an unquoted `overlay=x=` and `drawtext`
  x/y made every animated clip and every animated overlay abort the render with
  `No such filter`, invisibly, because the graph *string* looked right and every
  unit test asserted on the string. Free-form strings get the same care: overlay /
  box / chroma-key colours pass `valid_color` (else a safe default — a `.kerf` file
  never goes through `validate_export`), `pix_fmt` / `scaler` / `gif_dither` are
  allow-listed, and overlay text escapes `%` as `\\%` (a bare `%` makes
  `drawtext` render nothing while ffmpeg exits 0). **Text overlays** (`Timeline.overlays`) are
  `drawtext`'d onto the final composite (animated x/y/alpha exprs when keyframed); the
  still / preview path samples `Clip::transform_at` and draws overlays statically.
  **A `drawtext` value is read by three parsers, and `escape_drawtext` writes for all three**: the text *expansion*
  (a backslash escapes, `%` starts `%{..}`), the filter's *option string* (`av_get_token` ends a value at `:`, a backslash
  escapes, an apostrophe quotes, blanks at the ends are dropped) and the *filtergraph* (`'...'` protects `[],;`, an
  apostrophe is `'\''`). So `\` -> `\\` and `%` -> `\%` for the expansion, then `\` -> `\\`, `'` -> `\'`, `:` -> `\:` and an end
  blank -> `\ ` for the option string, then `'` -> `'\''` for the graph. A line break (LF, CR, CRLF as one, U+2028/2029) is one
  space (`drawtext` starts a new line at a CR too) and other control characters but a tab are dropped. It used to write for the last alone, and **an
  apostrophe swallowed every option after it** (the title came out as small black garbage at the top left: `It's here` drew
  `Its here:fontsize=36:fontcolor=...`), **a colon failed the whole render on FFmpeg 9** (`No option name near ' b'`) and cut the
  text on 6.1, and a backslash vanished. `drawtext_text_is_what_the_user_typed` (`#[ignore]`d) renders each against the same
  text read literally from a file (`textfile=` + `expansion=none`) on whichever FFmpeg runs it. `fontfile=` is escaped the same
  way minus the expansion level, which is also what a Windows path (`C:\Fonts\a.ttf` -> `C\:\\Fonts\\a.ttf`) needs (not run on
  Windows). **Every overlay names a font file**: `fonts::resolve_text_font` — the installed family it names, else the shipped
  Geist (`crates/kerf-core/assets/fonts`, SIL OFL 1.1, Regular and Bold, written to `<cache>/kerf/fonts/<name>-<digest>.ttf`;
  else to a per-user `0700` directory in the temp one, `kerf-fonts-<uid>`, refused if it is someone else's or open to others) —
  **checked on every use** (a `stat`, the bytes compared only when it changed, never trusting a length), rewritten when the cache
  was cleared or `/tmp` swept while the app ran, and a failed write is not remembered — because a bare `drawtext` asks its own build for "Sans" (nothing at all in a build without fontconfig, which
  is what the Windows and macOS bundles ship) and a render plan can only name a file. Geist covers Latin, Latin Extended and
  Cyrillic; Greek (a few letters) and CJK draw as `drawtext`'s missing-glyph boxes, as they did in a system Sans without
  them. The golden pins the default at a fixed path (`fonts::with_pinned_default_font`). `text_raster_graph` is the same
  `drawtext` for the GPU compositor's rasteriser (`gpu.md`).
  **360 footage** is reprojected by `v360`: `StreamInfo.projection` is detected at
  probe time (`detect_projection` — a `Spherical Mapping` side-data entry, or an
  Insta360 `.insv` whose frame is two squares side by side; deliberately *no*
  bare-aspect guess) or set by hand with `set_asset_projection` (a persisted
  per-**asset** override for footage neither signal catches — a stitched equirect
  that lost its metadata; it sticks so every later cut reframes), and clips cut
  from such an asset get a default
  `Clip.reframe` (`Clip::for_asset`) aiming a virtual camera at the sphere. In
  `video_clip_chain` a reframed clip runs `setpts → fps → [sendcmd] → v360@c{n} →
  [crop]` *before* the fit `scale`: `fps` is hoisted so an 8K source reprojects at
  the output rate, `v360`'s `w`/`h` render straight to the export frame, and
  `crop` moves after reprojection (edge fractions of a raw fisheye frame are
  meaningless). Animation goes through `sendcmd` because `v360`'s yaw/pitch/roll/
  `d_fov` are command-settable — but each command rebuilds its remap LUT (~32 ms
  at 1080p), so `reframe_commands` emits only channels that actually move, gates
  on `REFRAME_CMD_TOLERANCE`, and leads each command by half a frame. Values are
  wrapped/clamped first: `v360` **silently discards** an out-of-range command.
  `fov` maps to `d_fov` (aspect-correct on its own; `h_fov` would stretch). The
  resulting graph outgrows argv — Linux caps one argument at 128 KiB, Windows the
  whole command line at 32767 — so `externalize_filter_complex` spills anything
  over `GRAPH_ARG_MAX` to a temp file, passed via `-filter_complex_script` or,
  where FFmpeg 8 removed that option, the `-/filter_complex <file>` form that
  replaced it (`graph_script_flag` probes `-h full` once per process). The
  still path samples `Clip::reframe_at` to a constant `v360` instead, and
  `export_format` ignores a reframed clip's source dimensions so a 5760x2880
  capture does not become the deliverable size.
  A real **Insta360 capture is a *pair* of files** (`VID_…_00_….mp4` /
  `…_10_….mp4`), one circular fisheye per lens — neither is 360 on its own, so
  `Project::probe_import` stitches them at import: `insta360_pair` recognizes a
  square frame whose positional `_00_`/`_10_` lens token has a sibling on disk,
  `stitch_insta360` runs `hstack → v360=dfisheye:e:…:roll=180` (the lenses record
  upside down) into a 5760x2880 file cached at
  `<cache>/kerf/stitched/<hash>.mp4` keyed by *both* lens files (HEVC via a
  verified GPU encoder when one exists — the frame is too wide for h264 NVENC —
  else libx264 CRF 15), and the asset
  that lands describes **that** file (projection forced to `Equirect` — the CLI
  can't write an `sv3d` box) with the originals kept in `Asset.source_paths`.
  It is a full re-encode (~2x realtime in software, far faster on a GPU), so it
  streams progress (`import-progress`
  in the app), is serialized per pair, and dedupes via `insert_or_get_asset` —
  importing the other lens afterwards is a cache hit resolving to the same asset.
  Each input gets a **per-input `-ss` fast-seek** to its clip's source-window
  start (shared `clip_source_window`/`clip_seek`, frame-accurate against the
  seek-relative `trim`), so a cut from deep in a long source decodes only the kept
  region, not everything from `t=0`. **Still images** (PNG/JPEG/… — detected at
  probe time via `is_still_codec` + no audio + sub-second duration, flagged on the
  stream as `StreamInfo.image` and given `DEFAULT_IMAGE_DURATION` on import) are the
  exception: a still has no source timeline, so its input is `-loop 1 -framerate
  fps -t <window>` instead of `-ss`'d, and its in-graph `trim` stays absolute (seek
  forced to 0); `frame_*`/`timeline_frame` likewise decode the single frame without
  seeking. The **composited still's `-ss` is spelled to the microsecond**
  (`kerf_core::seek_arg`, `{:.6}`, which `kerf-gpu`'s `decode_args` calls too): `-ss` is
  exact on a fine time base, and with a
  frame at 1.0006 s `-ss 1.0006` returns it where the old `{:.3}` spelling, `1.001`,
  skipped to the next frame (`a_still_picks_the_frame_at_a_fine_time_base_second`,
  `#[ignore]`d, both FFmpegs). `render_with_progress` streams ffmpeg's `-progress` to
  report `{fraction, elapsed_secs, eta_secs}` and polls a cancel callback (killing ffmpeg →
  `RenderStatus::Cancelled`); `render_with` is the no-op-callback wrapper.
  `audio_pcm` decodes a source window to raw mono s16le PCM (input-side `-ss`) —
  the GUI's Web Audio preview playback fetches clip audio through it.
  **Playback is a real video stream**, not a slideshow: `stream_preview` hands a
  whole span to **one long-lived ffmpeg** (a spawn-seek-decode-exit cycle per frame
  caps well below frame rate however fast the machine is) and reads composited
  JPEGs off its stdout, split on the `FFD8`/`FFD9` markers. It composites through
  **the same graph the export builds** — `push_inputs` + `build_filter_complex`
  over a `Timeline::slice` from the playhead, both now shared with
  `build_export_args` — so what plays is what renders: every track, effect,
  keyframe and overlay, and the same `Timeline::for_render` gate, so a muted or
  solo-shadowed track is as absent from playback as from the file. Only the ends
  differ: proxy paths in (the caller passes
  `timeline_frame_inputs`' proxy-swapped assets), `-c:v mjpeg -f image2pipe pipe:1`
  out. **Nothing waits on ffmpeg forever**: the stream reads stdout on a side
  thread and gives up (killing the child, so the hwaccel retry falls back to
  software) after 30 s with no first frame or 15 s between frames; an export
  polls cancel every 250 ms rather than per progress line and is killed after
  300 s with no `-progress` at all; each `hw_encoders` probe is capped at 20 s;
  and a spilled graph file is unique per call, since two renders sharing a tag
  used to delete each other's script. Frames are **paced to the requested fps against the wall clock**, which
  throttles ffmpeg through pipe backpressure instead of letting it race ahead and
  buffer the whole timeline, and each carries its timeline time so the webview can
  drop one the audio clock has already passed, and `late` (seconds behind its slot on
  that schedule; 0 when early) so a caller can tell a hiccup from a stream that renders
  slower than it plays — late grows frame by frame in the second case. **Rate and
  width are `playback_params`** (pure, unit-tested; `stream_preview_with` takes a
  `PlaybackRequest { fps, max_width }`, `stream_preview` is a stated rate at the default
  width): an unstated rate is **the cut's own** — `export_format` of `for_render()`, so a
  muted track's footage does not set it — clamped to `MIN_STREAM_FPS..=MAX_STREAM_FPS`
  (1..60: a display paints no more), and a stated one is clamped the same, a NaN /
  zero / negative one counting as unstated; an unstated width is 960
  (`PREVIEW_STREAM_WIDTH`), a stated one is clamped to 64..=1920 (the frame tools' own
  cap), and `preview_resolution` still never renders wider than the footage. The preview
  asks for its pane's width (`preview-size.ts`) so a playing frame is as sharp as the paused
  one. Neither moves a golden: the builders take them as arguments. **A stream that cannot
  keep up is restarted cheaper by the page, not caught up by the engine** — see
  `frontend.md`: ffmpeg has no wall clock, so frames dropped after it rendered them save
  nothing, and a stream at 50 of 60 fps is a sixth of a second further behind per second.
  Measured here (4 shared cores, no GPU, ffmpeg 6.1, a 1280x720 all-intra proxy, JPEG q6, 3
  threads): unpaced, one clip renders ~200 fps at 960 px (~12 ms CPU a frame), 160 at 1440, 80 at
  1920; three layers 130 / 85 / 45. Paced at the cut's rate it delivers 24.1 / 30.1 / 60.1 fps with a
  p99 lateness under 6 ms, for 0.40 / 0.43 / 0.68 cores (one layer, 1280 px) and 0.56 / 0.65 / 1.07
  (three layers) — 60 fps costs about 1.7x the CPU of the 24 it used to be for 2.5x the frames. On
  one pinned core at a 25% budget, 3 layers at 60 fps hold at 960 px (0.89 of the core) and not at
  1280 (~50 fps) or 1920 (25), which is the overload the page steps down from.
  **Phone and camera footage** needs two things the container says rather than
  the pixels. *Rotation*: an iPhone portrait clip is a landscape frame plus a
  `Display Matrix`, and every FFmpeg decode autorotates it (nothing here passes
  `-noautorotate`; a proxy is written upright with no matrix of its own). The probe
  (`display_rotation`, the `Display Matrix` side data, or the clockwise `rotate`
  tag on an FFmpeg old enough to lack it) therefore reports the **displayed**
  size in `StreamInfo.width/height` — what `export_format`, fit/crop, smart crop
  and the bin's spec line all do their geometry against — and keeps the angle in
  `StreamInfo.rotation`. `fps` goes through `nominal_fps`: `r_frame_rate` on a
  jittery variable-frame-rate clip can be a multiple of the real rate (a ~30 fps
  clip probing as 120), so past 1.5x the average the average, snapped to a
  standard rate, is used instead. *HDR*: `StreamInfo.color_transfer` /
  `color_primaries` are recorded at probe and `StreamInfo::hdr()` names HLG
  (`arib-std-b67`; a Dolby Vision profile 8.4 iPhone file's base layer is HLG) or
  PQ (`smpte2084`). Squeezing those into `yuv420p` untouched is washed-out,
  mis-tagged picture, so every decode tone-maps to 8-bit SDR BT.709 **exactly
  once** (`tonemap_chain`, pure + unit-tested): `zscale` to light-linear float,
  BT.709 primaries, `tonemap=mobius` (linear to 70%, then a shoulder, so midtones
  are not re-graded), `zscale` back to BT.709 with error diffusion — or, when
  `zscale_available()` (probed once per process, like `graph_script_flag`) finds
  no libzimg, a `colorspace` primaries/matrix move, near-right for HLG. The
  *graph* paths get it from `ClipFx.hdr` (set in `transition_fx` from the asset),
  placed in `video_clip_chain` after the fit scale and `fps` — so the float stage
  runs on delivery-sized, kept frames — and before any colour work; the composited
  still prefixes it per clip. The *single-input* decodes (`decode_frame`,
  `contact_sheet`, `generate_proxy`) have only a path, so they ask `source_hdr`
  (a view of `source_traits`, one cached ffprobe per file) and append the chain
  after their own downscale.
  Salience and scene detection are analysis, not picture, and read the raw
  frames of the original; scene detection reads the proxy's instead only where
  `proxy_fidelity` says its scores are the same (SDR YCbCr in MP4 / MOV / Matroska, see
  above). **The proxy is where preview footage is converted**: `generate_proxy`
  tone-maps while it downsizes, `Project::preview_assets` hands the graph the
  proxy-swapped asset as `Asset::as_sdr_proxy` (same geometry, no HDR tags), and
  a file's proxy key gains `|sdr` when it is HDR, so a proxy cached before this
  existed (an untouched HDR picture) is rebuilt rather than trusted. Until the
  proxy lands the preview tone-maps the original in the graph, which is the same
  conversion; export always reads the original. Both go through plain software
  filters after the decode, so `-hwaccel` (frames come back to system memory, and
  a 4:2:2 or otherwise unsupported stream falls back to software inside
  `auto`, or via the one-shot retry for a named accelerator) is unaffected.
  SDR sources produce byte-identical argv and graphs. The input's transfer,
  primaries and matrix are stated to `zscale` rather than read from the frame, so
  a stripped-tag phone file does not abort the graph with "no path between
  colorspaces". Assets saved before these fields existed deserialize as SDR with
  the coded size; re-importing the file re-probes it.
