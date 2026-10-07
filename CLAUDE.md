# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Kerf is

A cross-platform desktop app for AI-assisted, **non-destructive** video/audio editing.
A Cargo workspace of three Rust crates + a Tauri-embedded SvelteKit frontend. The
distinguishing feature: a **stdio MCP server** lets an LLM analyze media and assemble
edits through the same engine the GUI uses. Nothing is re-encoded until export.

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
  lead and the container. The clone is a frame the original has no counterpart
  for, so a clip read from the proxy's start with no `-ss` (`clip_seek` 0 — a cut
  from the very head of the source) would hold it for `lead` while the export
  starts on the real first frame: `transition_fx` sets `ClipFx.head_pad` from the
  input's name and `video_clip_chain` opens that chain with `trim=start_frame=1`.
  Any seeked read skips the clone already. Ordinary assets get no such filter and
  an unchanged argv. A padded proxy never takes a hardware encoder (its output
  has no frame rate for the encoder to be told, and one refusal disables HW
  encode process-wide). **MPEG-TS is not fixed and is left alone**
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
  **GPU acceleration**: `hw_encoders()` probes once per process which hardware
  encoders (NVENC / QSV / VideoToolbox / AMF) this ffmpeg can *actually* use —
  each compiled-in candidate is verified with a one-frame test encode, because
  `-encoders` listing alone is not proof (`KERF_HW_ENCODE=none` disables). The
  list is surfaced as the `hw_encoders` Tauri command and the
  `export_capabilities` MCP tool, and the export dialog merges the verified ones
  into its codec choices. Proxy generation uses the first verified h264 HW
  encoder and stitching the first hevc one (both fall back to libx264 on any
  failure, and one such failure disables HW encode for the process). Background
  decodes (proxy, stitch, scene detection, the composited still) use the same
  `-hwaccel` (default `auto`, `KERF_HWACCEL=none` to disable) with a learned
  software fallback shared with the preview path; the GUI defaults export
  `hwaccel` to `auto` too, and `render_with_progress` retries a failed
  hardware-decode export once in software so the default can never lose a render.
  **How much of the machine any of this may take** is `engine/cpu.rs`. FFmpeg is
  written to finish as fast as it can — every run grabs every core and nothing
  coordinates one run with the next — so an agent analyzing eight sources over
  MCP used to spawn eight all-cores, whole-file decodes at once (each buffering
  its PCM, so gigabytes too) and leave the desktop unusable for no wall-clock
  gain. Two moving parts: **one heavy job at a time** (`cpu::lease`, a reentrant
  gate — an export's second pass and a stitch inside an import must not queue
  behind themselves) and **a share of the cores for that job** (`cpu_percent`,
  seeded from `KERF_CPU_PERCENT`, set at runtime by the app's settings). **Gated** =
  a whole-file job whose *result* is wanted later and that nobody is looking at:
  silence / scene / loudness detection, the PCM decode behind rhythm and in-process
  whisper, transcription, proxy, stitch, export. **Ungated** = whatever the UI is
  *drawing from* or an agent is *looking at*, which must not wait out a render:
  the moment reads (a scrubbed frame, the composited still, a clip's audio, the
  preview stream, a contact sheet) and the timeline's per-asset drawings — the
  waveform pyramid and the filmstrip — which do read the whole file, once, and cache
  it, but are what an opened project's clips are painted from. Ungated is not free:
  those are still thread-capped and niced and run a couple at a time, and the
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
  dropped entirely on a mono delivery. Tracks flagged `Track.duck` are mixed
  into their own bus and
  `sidechaincompress`'d against the rest before the final sum (music dips under
  dialogue); `ExportOptions.loudnorm` appends a single-pass `loudnorm` to -14 LUFS
  on the final mix, and `ExportOptions.range` renders only a span by building the
  graph from `Timeline::slice(start, end)` (a shifted sub-timeline copy — boundary
  clips retrimmed honoring speed/reverse, keyframes resampled, overlays clipped).
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
  The per-clip chains (`video_clip_chain` / `audio_clip_chain`) also realize
  each clip's **video effects** (`gblur`/`unsharp`/`hue`/`negate`/`vignette`, and
  `chromakey` which keeps alpha so a lower track shows through), **audio effects**
  (`highpass`/`lowpass`/`equalizer`/`acompressor`/`agate`) and **transform keyframes**
  — animated zoom via `scale=eval=frame`, animated position via the `overlay` x/y
  expr, rotation via `rotate`, opacity via `geq` (all driven by piecewise-linear
  `keyframe_expr` over clip-local time). **Any such expression must be quoted in
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
  seeking. `render_with_progress` streams ffmpeg's `-progress` to report
  `{fraction, elapsed_secs, eta_secs}` and polls a cancel callback (killing ffmpeg →
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
  drop one the audio clock has already passed.
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
  frames. **The proxy is where preview footage is converted**: `generate_proxy`
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
- `peaks.rs` (always compiled, CLI only) is what the timeline draws a clip's **waveform**
  from, and it answers a different question than `waveform` (N peaks for a whole
  file, 8 kHz, kept as it was): a clip shows a *window* at a zoom that never stops
  changing, so a file is decoded **once** into a `WaveformPyramid` — min/max peak
  pairs at 500 / 100 / 25 / 10 buckets per second, per channel (stereo when the
  source has two or more channels, mono otherwise) — and any window is then a slice
  read, `waveform_range(pyramid, start, end, buckets)` (pure + unit-tested; `start` /
  `end` in **source** seconds). It picks the *coarsest* level that still has a source
  bucket per requested one, partitions source buckets among the requested ones by
  where each begins (a peak lands in one column, never smeared across two; when the
  request is finer than the source it reads the bucket under each column's midpoint),
  leaves the part of a window outside the media as 0/0 buckets so the caller's
  time-to-column mapping stays linear, and caps `buckets` at 4096. The decode is
  **48 kHz f32** on purpose: 96 samples is exactly one 2 ms bucket, every coarser
  level is a whole multiple, and 48 kHz is what video audio already is, so no
  resampler runs and the peaks are the real samples' — at 8 kHz the low-pass smears
  transients and rings around a clipped plateau, so a flat-topped 1.0 would not read
  as full scale. Peaks are stored as `i16` (±32767 = full scale, so a clipped sample
  is recognizable; ~18 MB per hour of stereo) and the PCM is folded into buckets as
  it streams off the pipe, so memory is the pyramid, never the file. Like every read
  the timeline draws from it is **ungated** (no `cpu::lease`) but thread-capped and
  niced, at most two decodes at once (a freshly opened project asks for every clip
  at the same instant), concurrent requests for one file share one decode
  (`shared_pyramid`), and the pipe is read on a side thread so a decode silent for
  60 s is killed. The pyramid is cached at `<cache>/kerf/waveforms/<hash>.bin`, keyed
  by path + size + mtime + a format version, written to a temp file and renamed; a
  file that is short, long, the wrong version, inconsistent with its own frame count
  or has a bucket whose min exceeds its max is recomputed, never trusted — and the
  size is checked before anything is allocated. Loaded pyramids sit in a
  byte-bounded in-process LRU (64 MB) so scrolling does not re-read the cache file.
  `Project::waveform_range` / the lock-free `Project::decode_waveform_range` are the
  op (an asset with no audio stream is `InvalidArgument`), exposed as the
  `get_waveform_range` Tauri command and MCP tool; `get_waveform` / `get_energy`
  are unchanged.
- `filmstrip.rs` (always compiled, CLI only) is the video twin of `peaks.rs`: what the
  timeline draws a clip's **thumbnails** from. An asset is sampled once into a
  `Filmstrip` — 96 px-high frames (width from the *displayed* aspect, so rotation is
  autorotate's; a 360 asset is its raw equirect frame), `tile`d into one-row JPEG
  sheets of at most 8192 px, balanced so none is mostly padding (`sheet_layout`) — and
  a window is a few thumbnails picked with `Filmstrip::frame_at(t)` / `locate(k)`.
  **Thumbnail `k` is the frame on screen at source time `k * interval`** (the last
  frame at or before it; 0 is the first frame): that is `fps=…:start_time=0:round=up`
  — `fps`'s default `round=near` picks the frame half an interval *later*, and
  without `start_time=0` a picture that starts after the sound numbers its samples
  from its first frame's slot. The interval is the finest rung of 0.5 / 1 / 2 / 5 /
  10 / 15 / 30 / 60 … s that keeps the strip within 300 thumbnails (`pick_interval`;
  ~3-6 KB a frame); a still is one thumbnail. It decodes the **proxy when
  `ready_proxy` has one, else the original** — resolved *inside* the lock-free
  `Project::decode_filmstrip(&Asset)` and only on a cache miss (the proxy's path can
  take an ffprobe for the HDR check, so nothing under the project lock may touch
  the media: the surface holds the lock for `require_asset` alone) — and a failing
  proxy falls back to the original, hardware decode to software. The cache key is
  the **original's** identity (path + size + mtime + geometry + interval + version),
  never the proxy's: the frames look the same and a proxy keeps frame times, so a
  strip made before the proxy landed is the same entry after. HDR is tone-mapped
  after the downscale from `source_hdr` of the file actually decoded (a proxy
  converts nothing twice). **Keyframes for coarse originals:** from a 5 s interval
  up (assets of ten minutes and more) an *original* is decoded with
  `-skip_frame nokey`, so a thumbnail is the last *keyframe* at or before `k *
  interval` — up to a GOP earlier, never later — instead of the frame itself; 90 s
  of 1080p H.264 took 12.2 s every-frame and 0.8 s by keyframes. Never on the
  all-intra proxy (moot there: every frame is a keyframe, so its strip is exact),
  and never hardware-decoded (one frame in 60 is cheap in software); a keyframe pass
  that comes up short is retried with every frame. **Two ffmpegs, one decode:** the
  first streams *raw* thumbnails over a pipe — so the no-hang rule is per thumbnail
  (silent for `max(60 s, 6 × interval)` and it is killed) and the count is exact
  rather than inferred from tile padding — the second tiles and JPEG-encodes them
  from memory. **Ungated** (no `cpu::lease`) although it reads the whole file: the
  timeline draws from it, and gated it would sit behind the proxy encode and the
  import's analysis while clips stay empty. But it is a video decode beside whatever
  holds the lease, so it is capped at a quarter of the cores (one or two threads,
  `decode_threads`) and niced at **every** budget, 100% included, two at a time;
  concurrent asks for one asset share one decode. **A short decode is not cached:**
  ffmpeg exits 0 on a file that goes bad halfway, so when fewer thumbnails arrive
  than planned the video stream's own duration (ffprobe, only on a shortfall) says
  how many to expect, and more than 2 short is returned and memoized for the session
  but never written to disk (a video simply shorter than its container is complete
  at its own length). Cached at
  `<cache>/kerf/filmstrips/<hash>/` (`sheet-000.jpg …` + `manifest.json`) via a
  `.part` directory and a rename; a manifest that is not the canonical layout for its
  frame count, or a sheet that is missing / resized / not a JPEG of the promised size,
  is rebuilt, never trusted. The thumbnail width is the *coded* aspect — `StreamInfo`
  carries no sample aspect ratio, so an anamorphic source is not corrected (nor is it
  anywhere else: the project frame, fit, preview and export all work in coded pixels).
  `Filmstrip` serializes its geometry *without* the JPEG bytes (`#[serde(skip)]`) — a
  surface adds its own transport. No video stream is `InvalidArgument`; the
  `get_filmstrip` Tauri command is `require_asset` under the lock, then
  `Project::decode_filmstrip` with it released.
- `ffmpeg.rs` is the in-process **libav** backend (the `ffmpeg` feature): it supplies
  `probe` (reading the display matrix and colour tags the same way the ffprobe path does) and, behind the extra `libav-render` feature, an **experimental** in-process
  export pipeline. It can only compile with the dev libraries present (written against
  the ffmpeg-next 8.1 API). The default export path is the CLI one even in full builds.

**Transcription works in every build** (`engine/whisper.rs`, always compiled). Two
backends, picked by `analysis::default_transcriber`: the `whisper` feature's
in-process `whisper-rs` when it is compiled in, otherwise **FFmpeg 8.0's native
`whisper` audio filter** driven through the binary — `filter_available()` probes
`ffmpeg -h filter=whisper` once per process, `transcribe` runs
`aresample=16000,aformat=…,whisper=model=…:destination=…:format=srt` and parses the
SRT back (its `format=json` writes unescaped text, so SRT is the safe wire format).
`queue=30` overrides the filter's 3 s default, which would otherwise transcribe
three-second windows with no context. The model path is **never** put in the filter
graph (`:` and `\` are graph syntax, and a Windows path is both): ffmpeg runs with its
working directory set to the model's folder and both `model=` and `destination=` are
bare file names. Either backend gets its ggml model from `ensure_model`, which
**downloads it on first use** into `<cache>/kerf/models/ggml-<name>.bin` (streamed with
progress, `.part` + atomic rename, resumed via a range request, magic-byte checked so an
error page is never mistaken for a model). Which model: `set_speech_model` (the GUI
picker, persisted in project meta under `speech_model`) → `KERF_WHISPER_MODEL` (still
accepts a *path*, for existing setups) → `base`. `KERF_WHISPER_LANGUAGE` sets the
language hint, `KERF_WHISPER_MODEL_URL` an offline model mirror. `transcription_status()`
reports which backend is live, the model, and whether it still has to be fetched — the
transcript tab and an agent both read it to explain an empty transcript.
`analyze_asset_media_with_progress` streams a per-step `AnalysisProgress`
(`silence`/`scenes`/`loudness`/`rhythm`/`download_model`/`transcribe`/`done`), and
transcription runs **last** so the markers land before minutes of inference.
**A pass is abandonable** (`analyze_asset_media_cancellable` / `analyze_cancellable`,
a `CancelFn` alongside the `ProgressFn`): the check lands between steps *and* inside
transcription — the ffmpeg `whisper` run polls it about once a second off
`-stats_period 1` and kills the child, and the model download polls it per chunk,
keeping the `.part` file so the next attempt resumes rather than re-fetching 148 MB.
A cancelled pass returns `Error::Cancelled` and caches **nothing**: a half-analyzed
asset would read as analyzed, and its missing transcript as "no speech".

**Voiceover works in every build too** (`engine/tts.rs`, always compiled, no cargo
feature): Kokoro-82M text-to-speech, **in-process on ONNX Runtime**. Nothing of it
ships — `ort` is built `load-dynamic`, and the runtime library (Microsoft's release
archive for the platform, **pinned by SHA-256** in `runtime_spec`; Intel Macs stay
on 1.20, the last build Microsoft made for them; `ORT_DYLIB_PATH` supplies one
instead), the model (`onnx-community/Kokoro-82M-v1.0-ONNX`'s `model_quantized.onnx`,
~88 MB; `KERF_KOKORO_MODEL_URL` a mirror) and each voice pack (~0.5 MB) are fetched
on first use through **`engine/download.rs`** — the resumable, cancellable `.part` +
verify + rename fetch lifted out of whisper, which both now share. Static linking
was ruled out: pyke's prebuilt runtime needs glibc 2.38 and the Linux bundles build
on Ubuntu 22.04. Phonemes come from **`misaki-rs` without its espeak-ng fallback**
(espeak-ng is GPL-3), so unknown words are spelled out and only the English (`a*` /
`b*`) voices are offered; misaki-rs writes espeak-flavoured IPA (ZWJ-joined
diphthongs, length marks), which `to_kokoro_phonemes` (pure + unit-tested) maps onto
Kokoro's own single-symbol set. **Each sentence is synthesized on its own**
(`split_sentences`, abbreviation- and decimal-aware; blank line = paragraph pause),
which keeps every call inside the 510-token window (`chunk_tokens` splits a longer
one at a clause) and makes the timings exact — sample counts, not a speech model's
estimate. So a generated asset carries **`Asset.voiceover`** (`Voiceover`: script,
voice, speed, per-sentence `segments`; a `voiceover` JSON column migrated like
`source_paths`), `place_voiceover` writes those segments as its transcript, and
`generate_captions` subtitles it with no new caption code; `analyze_asset_media_*`
reads a voiceover's transcript from its script (`VoiceoverScript`) instead of
running whisper over audio whose words are known. The WAV lands in the **data**
dir (`<data>/kerf/voiceovers/voiceover-<hash>.wav` — the only copy, unlike a proxy)
with a `.json` timings sidecar, named by a hash of model/voice/speed/text so the
same script is a cache hit and the same asset. `Project::synthesize_voiceover`
(static, lock released, `cpu::lease` held, ORT intra-op threads = the budget, no
spinning) + `place_voiceover` (under the lock: the asset, its transcript, and one
`Add voiceover` edit onto the `VO` audio track, created on first use) is the
lock-free split again.

Two more optional features: `libav-render` (above) and `whisper` (in-process
`whisper-rs`; needs cmake, a C++ compiler and **libclang** at build time). Both are
off by default, so `--no-default-features` CI exercises neither — but **release
bundles are built `--features whisper`** (`release.yml`), because the bundled Windows
FFmpeg is a `--disable-whisper` build and a release with no in-process backend would
have no transcription at all there. `.github/actions/whisper-toolchain` installs and
*verifies* that toolchain (a missing libclang is not an error to whisper-rs-sys — it
silently falls back to its bundled Linux-generated bindings), and CI's `whisper` job
compiles the feature on every release runner (arm64 Linux included), plus the x86_64 macOS cross-compile the
release does, so it can't break for the first time during a release. On macOS it
also sets `MACOSX_DEPLOYMENT_TARGET` to `bundle.macOS.minimumSystemVersion`
(**10.15**, raised from Tauri's 10.13 default): ggml reaches for
`std::filesystem`, which libc++ marks unavailable below 10.15, so the floor is
what the feature costs — and a job compiling against the runner's own SDK would
never see it.

- **With FFmpeg dev libs** (full build): `cargo build` / `cargo run -p kerf-app`.
- **Without them** (CI, UI work): pass `--no-default-features`; everything but the
  in-process libav probe still works via the binaries.

## Common commands

```bash
# Rust — verify / test without FFmpeg dev libs (works everywhere)
cargo check --workspace --no-default-features
cargo test  -p kerf-core --no-default-features
cargo test  -p kerf-core --no-default-features split_and_remove_roundtrip   # single test

# The default run is pure (no binaries, no network). Tests that drive the real
# `ffmpeg` binary — playback streaming, the vertical/cover export — or download a
# real speech model are `#[ignore]`d, so run them explicitly when touching the
# engine or the export graph:
cargo test -p kerf-core --no-default-features -- --ignored

# The GPU compositor against FFmpeg's still (needs ffmpeg and an adapter — Mesa
# lavapipe is enough; `mesa-vulkan-drivers` on Debian / Ubuntu):
cargo test -p kerf-gpu --no-default-features -- --ignored

# Everything a commit / a push checks (prek, see below)
prek run --all-files
prek run --all-files --hook-stage pre-push

# MCP server — the desktop app hosts it (streamable HTTP on 127.0.0.1:7777/mcp).
# Run the app (below), then point an MCP client at the URL, e.g.:
#   claude mcp add --transport http kerf http://127.0.0.1:7777/mcp
# Override the bind address with KERF_MCP_ADDR. There is no standalone MCP binary.

# Frontend (Bun) — from frontend/
bun install
bun run dev      # http://localhost:1420, fixed port; uses sample data outside Tauri
bun run build    # static SPA -> frontend/build (consumed by Tauri)
bun run check    # svelte-check (type check)
bun run test     # bun's built-in runner over src/**/*.test.ts

# Desktop app — Tauri config is NOT at the default path, pass --config
bunx @tauri-apps/cli@2 dev   --config crates/kerf-app/tauri.conf.json
bunx @tauri-apps/cli@2 build --config crates/kerf-app/tauri.conf.json
cargo run -p kerf-app        # also works; runs the frontend dev command first
```

**Debug builds have their own identity.** `tauri.dev.conf.json` (next to
`tauri.conf.json`) sets `identifier` to `ch.orellbuehler.kerf.dev`, which names the
app's config and log directories and the single-instance lock — so a dev run
neither rewrites an installed Kerf's `settings.json` nor refuses to start while
that one is open. Tauri resolves its config at compile time (`tauri_build` and
`generate_context!` both merge the `TAURI_CONFIG` JSON env var over the file), and
`cargo run` has no CLI to set it, so `crates/kerf-app/build.rs` does: when cargo's
`PROFILE` is `debug` it merges `tauri.dev.conf.json` into `TAURI_CONFIG` (keys the
CLI already set win) and passes the result to rustc via `cargo:rustc-env`. That
covers `cargo run`, `cargo test` and `tauri dev` alike with no extra flag;
release builds never see it. A debug-profile build is therefore *not* the
shipping identifier — to test that, build `--release`.

### Local checks (prek) and the agent harness

`.pre-commit-config.yaml` is the single definition of "the checks", run by
[prek](https://prek.j178.dev) (`prek install --install-hooks` once per clone):
the **commit** stage is hygiene, `typos` (allowlist in `_typos.toml`),
`actionlint`, `zizmor`, `cargo fmt`, `svelte-check` (fails on warnings) and
`bun test`; the **push** stage adds clippy `-D warnings` and the kerf-core tests;
`commit-msg` enforces the lowercase-imperative subject and rejects AI
attribution trailers. `prek run --all-files [--hook-stage pre-push]` runs them by
hand. CI's `lint (prek)` job runs the commit stage (skipping the hooks that have
their own job), and `ci ok` is one status that is green only when every CI job is.
Rust lints are `[workspace.lints]` in the root `Cargo.toml` (no `dbg!`/`todo!`/
`println!`, justified `unsafe`, a few style lints) — every crate opts in with
`[lints] workspace = true`.

`.claude/` carries the shared agent setup: `settings.json` (an allowlist for the
check commands, and a PostToolUse hook that rustfmt's every `.rs` file an agent
writes, reporting parse errors back) and project subagents in `.claude/agents/`
— `engine` (kerf-core), `frontend`, `surface` (wire a core op into the Tauri
command + MCP tool + api.ts), `gpu` (kerf-gpu and its parity harness), and the
read-only `reviewer` and `verifier`.

## Architecture

`kerf-core` is the UI-agnostic engine. **`kerf-app` is the only binary; it is a thin
adapter over the `Project` API and exposes that same API twice — as Tauri commands to
the webview and as MCP tools to a connected LLM, both over one shared `Project`.** Add
capabilities to `kerf-core` first, then expose them in each surface. Keep that boundary:
no editing logic in the adapter.

### kerf-core (`crates/kerf-core/src/`)

- `model.rs` — the domain types and the only place timeline math lives: `Asset`,
  `StreamInfo`, `Timeline`→`Track`→`Clip` (the EDL), `AssetAnalysis`. A `Clip`
  references a source range (`source_in`/`source_out`) of an asset at a
  `timeline_start` — non-destructive. Besides the geometry (`Transform`) / color
  (`Color`) / `Transition` fields, a clip carries a `Vec<VideoEffect>` and
  `Vec<AudioEffect>` (per-clip filter chains) and a `Vec<Keyframe>` (transform
  **animation** — `Clip::transform_at` interpolates it, the engine renders the
  motion).
  **`TransitionKind` is three families, and the family decides the render**: a
  **dip** (`DipToBlack` / `DipToWhite`) takes both sides through a solid colour
  either side of the cut, a **dissolve** (`Crossfade`) mixes them, and a
  **motion** transition travels the incoming clip in over the outgoing one
  (`Slide*`) or carries the outgoing one out with it (`Push*`), four directions
  each — the direction naming the direction of *travel*. The enum answers for
  its own family (`dip_color` / `slide_from` / `pushes` / `overlaps`), so the
  engine never matches on eleven variants, and `wire_names` derives the
  expected-kind list both surfaces put in their errors. A dissolve or a motion
  transition plays both shots at once, so it borrows the outgoing clip's unused
  source handle: a clip trimmed to the very end of its footage has none to lend
  and the transition degrades to a hard cut (a dip needs none). Text titles /
  lower-thirds / captions live on the timeline itself as
  `Timeline.overlays: Vec<TextOverlay>` (each with its own `TextKeyframe`
  animation); `transcript_to_srt` serializes a transcript to SubRip.
  **Captions are timeline math, not a transcript dump**, and pure +
  unit-tested: a transcript is in *source* time and an overlay is in *timeline*
  time, so `Timeline::captions` projects each segment through the clips that
  actually show its footage (`Clip::source_span_to_timeline`, honoring trim /
  speed / reverse) — captions land on the words that survived the cut and words
  that were cut out get none. It reads through `for_render`, so a muted track is
  as uncaptioned as it is unheard; it chunks a sentence to `CaptionOptions`
  (a speech model emits whole sentences and a whole sentence does not fit a
  9:16 frame), timing lines by *character share*
  because neither speech backend reports word timings; lines too short to read
  merge back into a neighbour instead of flashing; and no two lines are ever on
  screen at once (captions are one lane of text, and the same footage reaching
  the cut twice would otherwise collide with itself). `TextOverlay.generated`
  marks what it wrote, so regenerating replaces its own set and leaves a typed
  title alone.
  **`CaptionStyle` is the look**, and one decision rather than four:
  `Lines` (4 words / 28 chars, 5% of frame height, low in the frame) is the
  subtitle shape a line is *read* in; `WordPunch` (one word, 11%, higher, bold)
  is the social shape a word is *watched* in, each landing on the beat of the
  speech. Word count, size, position and the flicker floors move together
  because they have to — held to `MIN_CAPTION` every short word would merge
  into a neighbour and word punch would collapse back into lines, so it gets
  its own `MIN_WORD_CAPTION` / `MIN_WORD_VISIBLE` and words merge far later.
  `CaptionOptions` is that style plus **overrides**: every number is optional
  and follows the style when omitted, `resolve()`ing to the `CaptionLayout`
  captioning works from — so asking for `word_punch` alone gets the whole look
  rather than one word left at subtitle size, and `CaptionOptions::default()`
  is unchanged, so every pre-existing call captions identically. `fit_size`
  then shrinks a caption to fit the frame: `drawtext` neither wraps nor scales
  and a 9:16 frame is barely half as wide as it is tall, so a long word — or a
  28-char subtitle line, already true before word punch — was drawn off both
  edges. `fontsize` cannot be an expression over `text_w` (the width is what
  depends on the size), so it is estimated from the character count against
  `Timeline.format`'s aspect; an unframed project assumes 16:9, wide enough
  that the fit never binds, so nothing that never picked a frame moved. A `Track`
  carries a `duck` flag (sidechain-ducked under the rest of the mix on export).
  `Fit` and `Delivery` live here (the domain owns the delivery shape; `engine::cli`
  re-exports `Fit`), and `Timeline.format` is the frame the project is cut for.
  **Smart crop** is here too and pure + unit-tested: `SalienceMap::crop_for` slides a
  window of the delivery aspect across the sampled map and returns the `CropFrame`
  (per-edge fractions, plus how far off centre it landed) that keeps the content —
  with a `CENTER_BIAS` so a flat map resolves to the plain centre crop rather than to
  whichever edge won by rounding, and `needs_crop` short-circuiting footage that is
  already the delivery shape.
  Inherent helpers (`Timeline::locate`, `Track::end`/`reflow`, `Clip::duration`,
  `Timeline::slice` — the shifted sub-timeline copy behind range export) back the
  operations. **Beat alignment** lives here too and is pure + unit-tested:
  `Timeline::beat_grid` maps the audio tracks' cached `Tempo` onto timeline time
  (confidence-gated by `BEAT_MIN_CONFIDENCE`, mirroring the ruler's ticks) and
  `Track::align_cuts_to_beats` ripples a track's cuts onto that grid — each clip
  retrimmed at its **outgoing** edge (`source_in` for a reversed clip, whose tail
  is the source's head), gaps preserved and their incoming cuts snapped too,
  stretching only as far as the asset has footage (a still loops, so it is
  unbounded). **What changed between two cuts** is here too and pure +
  unit-tested: `Timeline::diff` returns a `TimelineDiff` — a `DiffEntry` per
  change (`DiffKind` distinguishes an add from a cut from a *move* from a
  *retrim*, because those are different things to review), each already phrased
  for a human (`Trimmed clip on V1 at 0:04.0 — 4.0s → 2.5s (-1.5s)`) and
  carrying the clip/track/time so a UI can jump there. Everything is matched by
  **id**, so a reordered track reads as the handful of moves it is rather than as
  every clip having been replaced, and a removed track is one entry instead of one
  per orphaned clip. `StagedEdit` is a pending proposal (base seq, the edit
  labels, `stale`, and its diff). **Ripple** is here too, pure + unit-tested:
  `Timeline::ripple_from(before)` takes what an edit left behind and the cut it
  started from and, per track, matched by **id** like `diff`, shifts the clips the
  edit left *starting where they started* by the net change in length of what it
  did ahead of them — a clip's length change (right trim, speed) or removal, or an
  add that landed **on footage that was there** (an add that fits in free space,
  and every append, moves nothing). It carries the rules that were bugs waiting:
  a **left-edge trim keeps the clip's start** (the GUI commits it as `source_in`
  *plus* a later `timeline_start` to hold the right edge; ripple keeps the start
  and follows the length, so both forms give one result — only when the trim is
  the whole edit on the track); a **split shifts nothing** (the new half is an add
  over the footage the other half gave up, and they cancel); **moves never ripple**
  (a clip that merely changed its start or track is not "footage ahead");
  **clips the edit itself moved are not followers**, so an op that already closes
  the gap is not shifted twice; tracks are **independent** (no sync lock — a V1
  ripple leaves A1 where it is, which is why linked A/V is its own backlog item),
  a **locked track never moves**, and overlays / markers do not move. It **never
  produces an overlap**: if shifting would leave a touched clip overlapping
  another or before 0 (an add that lands *inside* a clip would need a split), that
  track is returned as the edit made it. `Timeline::move_clips` /
  `remove_clips` are the pure, all-or-nothing multi-clip edits behind the
  marquee: a `ClipMove` is a clip, an **absolute** start and an optional
  same-kind track; the group is checked as a group (moving clips pass through the
  places they are leaving, never onto each other or a clip that stays), and a
  locked track, a start before 0 or a clip named twice refuses the lot.
- `platform.rs` — **where the cut is going.** A static `TARGETS` table (Reels /
  Shorts / TikTok / Instagram feed / YouTube: delivery frame, accepted aspects,
  length limits) plus a pure, unit-tested `check` over a `CutSummary`. It keeps
  two limits apart that are usually conflated: a **hard** limit is what a
  platform rejects, a **reach** limit is what it accepts and then stops
  distributing — a four-minute Reel uploads fine and is shown only to existing
  followers, the worse outcome because nothing tells you. Findings carry a
  `Severity` (error / warning / tip) *and* an `IssueKind` (empty / length / shape
  / resolution / captions), because a landscape cut earns a near-identical shape
  complaint from every vertical feed and the UI has to collapse those into one
  line naming four platforms. Messages are phrased with the real numbers
  ("0:20 over", "cutting 1:00 would keep it in the feed"); aspect is compared as
  a **ratio**, so 720x1280 reads as the right shape and merely soft. The numbers
  are other companies' product decisions, verified 2026-08-25 and **advisory** —
  nothing here ever blocks an export. `Project::platform_check(frame)` resolves
  the summary from `working_timeline` (so an agent is judged on its own
  proposal) with an optional frame override, which the export dialog passes when
  a render resizes away from the project frame.
- `render_plan.rs` — **what one frame is made of**, shared by every renderer.
  `RenderPlan::at(timeline, assets, opts, t, color)` is the canvas (the same
  `export_format` the still uses, through `render_geometry`) plus the ordered video
  layers visible at `t`: asset path, resolved source time (speed / reverse /
  clamped), the `Transform` sampled at clip-local time, `Color`, the stream's
  displayed size / rotation / transfer / pixel format / matrix. It is built on
  `active_video_clips`, the very function `build_still_args` takes its inputs from,
  so the FFmpeg still and the GPU compositor cannot disagree about which clip is
  where in its source (a `cli.rs` test pins the argv against the plan); `still_size`
  is the preview-size rule both use.
  **`gpu_supported_at(size)` is the per-frame fallback switch** (`gpu_supported()` is
  its size-free half; `unsupported_reasons_at(size)` says *no, with reasons*) for
  anything the compositor does not render exactly: video effects, masks, 360 reframe,
  HDR, a live text overlay, a fade or transition in progress, a non-bicubic scaler, a
  layer with no known picture size, a shrink steeper than `MAX_SHRINK` (40:1, the
  steepest the scaler comparison measures), and the classes below. Pure + unit-tested
  like the rest of the timeline math.
  - **Pixel format is judged from a positive allow-list** (`pix_fmt_layout` in
    `model.rs` → `PixLayout { Yuv420, Gray, OtherYuv, Rgb }`): a format that is not
    known to be opaque is refused, so a name a deny-list never thought of (`ayuv`,
    `vuya`, the `rgb32` aliases) cannot reach a compositor that would flatten its
    alpha onto black. `pix_fmt: None` is the marker of an asset saved before it was
    recorded — **its colour matrix is unknown**, because "no `color_space`" then means
    *either* untagged *or* never probed, and a BT.709 clip of an old project must not
    be taken for BT.601: such a layer is refused when translucent, and under
    `BottomLayerTag` whenever it is in the stack.
  - **The composite's matrix is a property of the FFmpeg, probed**
    (`CompositeColorPolicy`, from `engine::composite_color_policy()`, a
    once-per-process behavioural probe in `cli.rs` like `graph_script_flag` /
    `zscale_available`). It builds a tiny raw-YUV clip tagged BT.709 and an untagged
    twin, **checks with `ffprobe` that the tag survived into each** (a clip that lost
    it would make the two measurements identical on any FFmpeg), runs both through the
    *real* `build_still_args` graph and compares the middle pixel: the same picture
    means the composite is untagged and read as BT.601 whatever the layers say
    (`FixedBt601` — FFmpeg 6.1), a different one means colourspace is negotiated along
    the overlay chain and the bottom layer's tag is the composite's (`BottomLayerTag` —
    FFmpeg 9.0). **A probe that cannot tell is `Unknown`, never a guess** — an earlier
    version fell back to `BottomLayerTag` as "the cautious one", which draws every
    BT.709 / BT.2020 clip wrongly on FFmpeg 6 (30.5 dB, 31 levels): the two policies
    differ on *exactly* that footage, so no answer for it is safe. `Unknown` draws only
    the stacks they agree on (BT.601 throughout) and refuses the rest. It is not
    remembered: a measured policy is kept for the process, a failed probe is retried
    after a backoff (5 s, doubling to 5 min, so a broken ffmpeg is not respawned per
    frame). The probe is bounded (`POLICY_PROBE_TIMEOUT` 15 s over six short runs, each
    served from side threads, killed at the deadline) because every concurrent first
    caller waits behind it; **the first call blocks 70–200 ms**, so call it from a
    blocking thread. The policy is an *input* to `RenderPlan::at`, so tests pin any of
    the three without an FFmpeg. Under `BottomLayerTag` a stack whose
    layers share one matrix class (BT.709, BT.2020, BT.601 — `smpte170m`, `bt470bg`
    and untagged are one class) is drawn with that matrix; **mixed matrices, an
    unknown one, and an RGB picture in a stack that is not BT.601 are refused**,
    because FFmpeg converts the other layers into the bottom layer's matrix with an
    arithmetic that was not reproduced (float and fixed-point models of it were off by
    up to 26 levels). `PlanCanvas.matrix` is the result; `PlanStream::matrix` is the
    one a translucent layer is taken out of YUV with.
  - **What depends on the render size** is decided in the plan, before any decode
    (`LayerGeometry` lives in kerf-core as `layer_geometry.rs` for this reason): a
    translucent layer of odd size, **resizing a picture that is not 8/10-bit
    4:2:0 or gray** (any scale stage that changes its size, in either direction) —
    FFmpeg scales in the format the picture has, the decode reduces it to 8-bit 4:2:0
    first, and the chroma is then interpolated from different samples: enlarging was
    4:4:4 x1.5 27 levels off, 4:2:2 x2 17, BGR0 x2 51, PNG RGB x2 69, and even a shrink
    as mild as 1.05-1.5x reads flat max 8-9 (4:2:2 at 0.9x is over the limit) with
    edges up to 32 levels. No band of ratios is measured strictly inside the limits on
    both FFmpegs for busy chroma, so none is claimed; a picture left at its size is
    drawn for every format — and **a crop of a picture whose chroma is finer than
    4:2:0's**. Where a position rounds
    depends on where in the graph it is (`RenderPlan::layer_geometry`): the first
    `crop` runs on the picture as decoded and rounds to its **native** chroma grid
    (`pix_fmt_subsampling`: even for 4:2:0, even columns for 4:2:2, nothing for 4:4:4,
    gray or RGB — a table by pixel format whose every entry an `#[ignore]`d test
    measures against the real `crop`). The chain converts to 4:2:0 in its **last**
    `scale`, so `pad`, and the Cover crop of a chain with one `scale`, are on the 4:2:0
    grid whatever the source was (the first version of this fix rounded those to the
    native grid too and was 20 to 30 dB off at every odd letterbox), while a Cover crop
    followed by the transform's own `scale` is still on the native grid (a gray layer
    with that was 35 levels off). The
    compositor's planes are 4:2:0, so luma lands exactly but the chroma of a 4:2:2 /
    4:4:4 / RGB picture positioned between two 4:2:0 samples is a pixel off (30 to 38 dB
    whole-frame): **refused**; gray, with no chroma to misplace, is drawn exactly, as
    is a 4:2:0 picture or any position that is on both grids. A format whose grid is not
    known (never recorded, or off the table) is refused wherever the grids disagree
    (`LayerGeometry::resolve_any_grid`).
  - **Range.** The decode converts a full-range picture (ffprobe names it `yuvj…`,
    whatever the container) to limited range up front; FFmpeg 9 keeps it full range
    through the graph (tagged `pc`) and runs `eq` and the overlay on it, converting
    last. That agrees with no colour correction and disagrees with it by 34 to 44 dB
    (every knob, measured on both an mjpeg file and full-range H.264), so **colour
    correction on a `yuvj` picture is refused** (under any policy — FFmpeg 6 happened
    to agree, but it is the decode order that is different), and so is **a graded layer
    in a stack that holds one** wherever the composite's colour is negotiated or
    unknown (the range, like the matrix, is the bottom layer's). An asset that never
    recorded its pixel format is not known not to be full range, and is treated so.
  What only the decode or the compositor can see is refused there instead
  (`GpuError::Unsupported`): a picture that decodes at another size than probed, an
  alpha channel found in the pixels of an asset that never recorded its format.
- `engine/cli/golden.rs` — **the golden argv oracle** (test-only, a child of `cli` so it
  reaches the private builders). 4000 seeded timelines (every transition kind with / without
  a source handle / across a gap, fades, speed, reverse, stills, keyframes, masks, effects,
  chroma, reframe, HDR, overlays, delivery format and fit, the audio mix, and **every
  `ExportOptions` field** with the one-, two- and no-pass encoder spellings) have their
  `build_export_args_phase`, `build_still_args` and `build_preview_args_with` argv reduced to
  FNV-1a digests, committed as 40 block digests each in
  `engine/cli/golden/{export,still,preview}.txt` (LF: `.gitattributes`, and the comparison
  ignores `\r`). A refactor of the graph builders must leave all three untouched; an
  intended argv change moves the files of the builders it touched. **Bless** with
  `KERF_GOLDEN_BLESS=1 cargo test -p kerf-core --no-default-features golden -- --nocapture`
  (exactly `1`): it rewrites **all three** files and says so, and `git diff` is the guard —
  only the files you meant to change should move. It is **machine-independent**: the
  builders read the machine in four places and each is pinned — the preview's
  `decode_hwaccel()` (through `build_preview_args_with`), `zscale_available()` (a `cfg(test)`
  thread-local override; every HDR case is built both ways), `drawtext`'s resolved font path
  (no overlay names a font) and **libm** (`db_to_linear` is `powf`, whose last digits differ
  between glibc with and without FMA, macOS, Windows and arm, so `round_libm` keeps the
  compressor / gate numbers to 10 significant digits; the generator seeds dB values known to
  differ, so a run under `GLIBC_TUNABLES=glibc.cpu.hwcaps=-FMA,-FMA4` fails without it). The
  digests are identical blessed with `KERF_HWACCEL` unset, `none` and `auto`. A coverage
  table (`family needle` lines, plus the branch `transition_fx` took and a few structural
  tags) fails the test if a family hits fewer than 20 cases — the generator stopped covering
  it, or the argv text changed; digest and coverage failures are reported together.
  `KERF_GOLDEN_CASES=<file>` writes a digest per case (diff base vs change to find the case
  in a failing block), `KERF_GOLDEN_DUMP=<n>` prints one case's argv,
  `KERF_GOLDEN_COVERAGE=1` the thinnest families.
- `project.rs` — `Project` wraps a `rusqlite::Connection`. **Persistence shape:**
  `assets` and `analysis` are real tables (streams/analysis stored as JSON columns);
  the **entire timeline is a single JSON blob** in a one-row `timeline` table. All
  edits go through `edit_timeline(|tl| ...)` which loads → mutates → saves the blob.
  **Ripple mode** is a project flag (`ripple_mode` / `set_ripple_mode`, in `meta`
  like `speech_model`: persisted with the file, default off, not an edit) that
  `edit_timeline` honors for every op — it snapshots the timeline, runs the op,
  and stores `after.ripple_from(&before)`, on the staged path too (so the review
  diff shows the clips that followed). It is applied there, not per op, so a new
  op ripples with no code of its own; with the flag off nothing is cloned and
  nothing changes. `Project::with_ripple(Option<bool>, |p| ..)` forces it on/off
  for the calls inside (`None` inherits) — how a tool takes an optional `ripple`
  argument; `ripple_active()` is the effective answer. The ops that decide their
  own layout go through `edit_timeline_exact` and never ripple: `ripple_delete`,
  `cut_clip_range`, the beat snap, `reorder`, `move_clip(s)`, `insert_clips`.
  `trim` re-reads its clip afterwards because a ripple can move it. A forced-on
  `remove_clips` is the multi-select ripple delete. `move_clips` / `remove_clips`
  are single revisions (`Move N clips` / `Remove N clips`), and — unlike the
  single-clip ops, which leave locks to the GUI — refuse clips on a locked track.
  `Project::sample()` seeds an in-memory demo (two assets + analysis + a starter
  timeline + a sample task queue); it backs the kerf-core tests, but the app now
  launches with an **empty** `Project::open_in_memory()` — the user imports media or
  opens a `.kerf` file to populate it.
  `analyze_asset`, `frame_at`, `waveform` and `waveform_range` delegate to the engine; editing ops are
  unchanged. `snap_to_beats(track_id, tolerance)` is "cut to the beat": it collects
  every asset's cached `Tempo`, builds the grid and aligns one track (or every
  unlocked video track) to it, defaulting the tolerance to half a beat so each cut
  moves to the beat it is already nearest; it errors when nothing rhythmic has been
  analyzed rather than silently doing nothing.
  `smart_crop(clip_id)` is "frame it for where it's going": reshaping a cut throws
  away most of one axis and both fits pick that axis blindly — `Cover` takes the
  middle, `Contain` letterboxes — so it samples where each shot's content actually
  sits and writes the crop that keeps it, **per clip**, as one `Smart crop` revision.
  Split three ways for the lock-free pattern (`smart_crop_inputs` under the lock →
  the static `sample_smart_crops` with it released → `apply_smart_crops` under it
  again); the result is an ordinary `Transform` crop, which the graph already applies
  *before* the fit scale, so the preview, the still and the export all follow and the
  inspector's sliders still have the last word. Clips already the delivery shape and
  360-reframed clips are left out (that camera *is* the framing decision), and a pass
  that changes nothing writes no revision.
  **One cut, every platform**: the same project can be delivered at several
  frames in one pass (a 9:16 Reel, a 1:1 post and a 16:9 upload), which is
  what exposed the tension in smart crop — its crop is baked into the
  transform for *one* shape, and framing for a second overwrote the first. So
  a clip carries **`Clip.framings`**, a crop per delivery shape (`Framing`,
  keyed by the reduced ratio `Delivery::ratio`, `(9, 16)`) beside the
  transform's, and **`Timeline::for_delivery(delivery)`** (pure +
  unit-tested) is the render of the cut at another frame: a copy whose format
  is that delivery and whose clips wear the crop they carry for its shape — the
  same change-the-timeline-not-the-graph pattern as `for_render`, so the graph
  builders never learned about it. A clip with no framing for the shape keeps
  the crop it has (never throw away a hand-made crop), which is why the framing
  pass writes an *identity* framing for a shot already that shape: a lookup
  miss would otherwise leave a 16:9 shot cut 9:16 delivering at 16:9 as the
  strip its 9:16 crop keeps. Generated captions are re-fit to the new aspect
  (`fit_size` again); typed titles are left alone. The framing pass is the
  smart-crop trio again for the *other* shapes — `framing_inputs(deliveries)`
  under the lock (the project frame's own ratio excluded, duplicates
  collapsed), the static `sample_framings` with it released (**one** salience
  decode per clip, a crop per shape from it — the map is a property of the
  shot, the crop of the frame), `apply_framings` under it as one `Frame for
  9:16, 1:1` revision that a re-run leaves alone — and `engine::render_variants`
  renders `ExportVariant`s (a `Delivery` + an output path; `ExportVariant::beside`
  names each file by shape, `cut-9x16.mp4`, an `x` because `:` is not a Windows
  filename character) **one after another**, each variant's `resolution` / `fit`
  taken from its delivery, reporting a `VariantProgress` (which file of how many
  plus the overall fraction). Sequential on purpose: an export takes every core
  it is given and `cpu::lease` would serialize them anyway, and a cancel is
  then clean — the file in flight is deleted, the finished ones kept.
  The **agent task queue** is a real `tasks` table (one row per `Task`,
  columns not JSON): `add_task` / `list_tasks` / `claim_next_task` / `complete_task`
  / `fail_task` / `resolve_task` / `remove_task` drive the `queued → working →
  ready → done` (or `failed`) lifecycle in `model.rs`.
  **Agent edits are staged, not applied** — the thing that makes an agent safe to
  leave running on someone's cut. A one-row `staged` table holds a proposal (the
  timeline being built, the one it branched from, the edit labels, the task it
  belongs to); `edit_timeline` routes an edit into it whenever the actor is
  `Agent` and a session is open, so the timeline the user is looking at never
  moves under them. `begin_staging` opens one, `staged()` reports it *with its
  diff* and whether it went `stale` (the user kept cutting, so applying would
  replace their newer work — refused unless `apply_staged(force)`),
  `apply_staged` lands it as **one** revision attributed to the agent (an empty
  proposal just closes, rather than putting a no-op edit in the user's history)
  and `discard_staged` throws it away. `working_timeline()` is the read side:
  the proposal while the agent has one, the live timeline otherwise — every read
  an edit depends on goes through it (including the preview, still and export
  paths), so the agent can *look at* the cut it is proposing, and the GUI, which
  never stages, always sees the live one. `restore` (undo/redo/revert) refuses
  for an agent holding staged edits rather than walking the ground out from under
  them. The queue ties in at both ends: `claim_next_task` opens a staging session
  for the task, `resolve_task` applies it (accepting the task *is* accepting its
  edits) and `remove_task` discards it. `diff_revisions` / `revision_diff` point
  the same diff at the stored history snapshots, so the edit log can say what an
  edit did rather than only which operation ran.
- `analysis.rs` — transcription / scene / silence / rhythm are **pluggable traits**
  (`Transcriber`, `SceneDetector`, `SilenceDetector`, `RhythmAnalyzer`). Real impls
  now exist:
  `FfmpegSilenceDetector` / `FfmpegSceneDetector` (CLI engine, always available —
  scene detection decodes hardware-accelerated and scores at 640px, the metric
  being resolution-normalized), `FfmpegRhythmAnalyzer` (onsets + tempo +
  speech/music class from **one** PCM decode — they used to be three traits, each
  re-decoding the whole file), `WhisperFilterTranscriber` (the ffmpeg `whisper`
  filter, always compiled) and
  `WhisperTranscriber` (in-process, `whisper` feature); `NullAnalyzer` is still the
  fallback. `Transcriber::transcribe` takes a `ProgressFn` *and* a `CancelFn` —
  alone among the providers, because it can download a model and then run for
  minutes, which is both the only step worth reporting on and the only one worth
  being able to give up on.
  `Project::analyze_asset` wires them and caches the `AssetAnalysis`.
- `error.rs` — `Error`/`Result`; the `Ffmpeg(#[from] ffmpeg_next::Error)` variant is
  itself `#[cfg(feature = "ffmpeg")]`.

### kerf-gpu (`crates/kerf-gpu/`)

The wgpu compositor — work package A0 of `.claude/plans/gpu-compositor-and-roadmap.md`,
a feasibility spike **not linked into kerf-app yet**. It draws a `RenderPlan` headless
(`Gpu::new(GpuOptions)`: instance / adapter / device, an `Err` rather than a panic
when there is no adapter, `force_fallback_adapter` for the software one) and reads
RGBA back. wgpu is built with the Vulkan / Metal / DX12 backends only — no GLES, no
WebGL — so the CI target is a software adapter (Mesa **lavapipe** on Linux, WARP on
Windows) and nothing may depend on an optional wgpu feature. **A GPU failure is an
error, not a panic**: every unit of wgpu work runs in out-of-memory / validation /
internal error scopes (`Gpu::guarded`), anything uncaptured is logged and remembered,
`Compositor::new` and `composite` return `Result`, and a lost device is tracked — every
later call is `GpuError::DeviceLost` and the **owner builds a new `Gpu` and
`Compositor`** (the device is not recreated behind the caller's back).

FFmpeg stays the decoder: `source::decode_layer` pipes one frame from the binary as
`yuv4mpegpipe` 4:2:0 (the same `-ss` as the still graph), spawned through kerf-core's
`ffmpeg_command()` (no console flash on Windows). **The decode states its own size**
(the y4m header) and it is compared with the probe's: a JPEG with an EXIF orientation
probes 480x270 and decodes 270x480, so a mismatch is `Unsupported` and that frame goes
through FFmpeg. A `pix_fmt` that is not on the allow-list of known-opaque ones is refused (the plan
does too); an asset with no recorded `pix_fmt` costs a second `yuva420p` decode that is
refused if anything is transparent. A child is killed after 30 s or when it writes more than the probed size,
and `-ss` past the last frame (zero frames) is `Ok(None)` — FFmpeg's own still draws
nothing for that layer, and so does the compositor. The layers of a frame decode in
parallel, each given `budget / layers` threads even at a full CPU budget
(`limit_ffmpeg_args(args, share)`). No cache, no proxy: that is A1.

What the parity harness forced, all recorded in `kerf-gpu`'s docs and shaders:

- **Composite in YUV, convert once.** FFmpeg's `overlay` blends the encoded planes and
  the still converts the *result* to RGB. The canvas texture holds Y, U, V, A and the
  blend acts per channel; an RGB-first design clamps each layer before blending and
  composites out-of-gamut-but-legal footage (saturated patterns, super-whites)
  differently. Chroma is replicated 2x2 at the final conversion, as swscale's unscaled
  path does.
- **The composite's matrix is the FFmpeg's, probed — not "always BT.601", and not the
  stream's** (`PlanCanvas.matrix`, `CompositeColorPolicy`). FFmpeg 6.1 composites onto an
  untagged black base, so the result is read as BT.601 whatever the layers were tagged
  (bt709, bt601 and untagged convert identically — measured). FFmpeg 9.0 negotiates
  colourspace along the overlay chain: the *bottom layer's* tag is the composite's, and a
  layer tagged otherwise is converted into it by a scaler stage whose arithmetic was not
  reproduced. An earlier version of this section said the matrix was fixed; that was true
  of 6.1 only, and the pinned 9.0.2 build was 29.5 dB / 35 levels off on a single
  BT.709 clip. So the policy is measured once per process from the real still graph
  (`composite_color_policy`), the plan takes it as input, **stacks of one matrix are drawn
  with it and stacks of mixed matrices are refused** (see `render_plan.rs` above), and the
  CI parity job runs both builds. The consequence to know about: an *export* is untagged
  `yuv420p` that a player shows as BT.709 for HD, so the FFmpeg preview already disagrees
  with the file by a few levels on saturated colour. Fixing that is a decision for when
  the GPU path replaces the FFmpeg preview. The stream's own matrix
  (`PlanStream::matrix`, from the probed `color_space`) is used for taking a translucent
  layer out of YUV in the round trip below and for choosing the composite's matrix.
- **`eq` is a YUV operation**, not an RGB one: `eq.rs` builds vf_eq's own per-plane
  tables (the integer `process_c` path when gamma is 1, the `pow` table otherwise, its
  `float` clamping, truncation) and the test suite checks them byte for byte against
  FFmpeg. Kerf's "temperature" is a power function on the chroma planes.
- **Scaler: swscale's bicubic, ported, not imitated** (`sws.rs`, `composite.wgsl`).
  swscale's filter *table* has quirks a formula misses — the window start truncated
  toward zero (so at the first pixels the tap at -1 is absent, not folded into pixel 0),
  right-edge folding, near-zero tap trimming, 14- / 12-bit weights normalized by error
  diffusion, the 16.16 step — and the shader does its integer arithmetic (15-bit
  horizontal intermediate clipped at the top, vertical pass rounded at bit 19). Planes
  are scaled independently at their own size and rounded to 8 bits between stages (the
  fit scale and the transform's scale are two scalers in cascade). The first version
  used a bicubic *formula* and was wrong by up to 20 levels along the borders of busy
  footage; the committed test
  (`the_scaler_matches_ffmpegs_scale_plane_by_plane`, `Compositor::composite_yuv`)
  compares planes with `ffmpeg -vf scale`: **within one level up to about 4:1** (noise and
  checkerboard included, up and down), within 2-3 levels (mean under 0.6) on the 8:1 to
  20:1 downscales of noise and test patterns and within 5 at 40:1 (x86 swscale's vertical
  scaler is not bit-exact with the C one this follows), identically on FFmpeg 6.1 and 9.0.
  **A shrink steeper than 40:1 (`MAX_SHRINK`) is refused by the plan** — a 58:1 shrink of
  a 4K test pattern read 16 levels off in RGB. Scaling happens on the decoded 8-bit 4:2:0,
  which is why resizing any other format — enlarging, or shrinking even by 1.05-1.5x (FFmpeg
  scales its chroma from the full-resolution plane, the compositor from the averaged one;
  flat max 8-9, edges up to 32 levels) — is refused by the plan too.
- **Opacity below 1 is FFmpeg's RGB round trip** (`roundtrip.rs` is the scalar
  reference, `roundtrip.wgsl` the passes): `colorchannelmixer` only takes RGB, so the
  layer goes `yuva420p -> argb -> yuva420p` before `overlay`. Out of YUV is swscale's
  C table converter with *the layer's own matrix* (a BT.709 stream converts as BT.709;
  exact on random pictures); back is **the composite's matrix** (`Rgb2Yuv`: the RGB
  frame in between carries no tag, so it takes the one the canvas is converted with —
  BT.601 on a fixed-policy FFmpeg, the stack's on a negotiating one; BT.601's table is
  swscale's explicit constants, the others its derivation; luma exact, chroma — pair
  sums then a stretched vertical bicubic — within a level); the alpha plane is
  `round(lrint(255 * op) * 256 / 255)` (50% is 129). Colour outside the RGB gamut comes
  back clipped, luma a level or two lower — a blend that skipped this was 34 dB / 22
  levels off on saturated bars. A translucent layer of **odd size is refused** (by the
  plan): the chroma pairing reads uninitialised padding past an odd picture, an odd
  height leaves swscale's unscaled path. So is a translucent layer whose matrix is
  unknown. The tightest figure on 9.0.2 is a translucent BT.709 test pattern alone
  (40.9 dB flat against the 40 dB floor, the GPU 1-3 levels bright from the final
  conversion's truncation bias).
- **A letterboxed layer is the whole frame.** `pad` emits a full-canvas frame, black
  bars included, so an identity clip that does not fill the frame covers what is below
  it, and `eq` grades the bars too (`LayerGeometry.matte`). `pad` also drops the last
  odd row / column of the picture.
- **An odd layer's last chroma block reaches one pixel past it** (`overlay` blends whole
  chroma samples): the compositor writes that pixel's U and V with a second, chroma-only
  pass.
- **The decode asks for limited range explicitly** (`scale=out_range=tv`): FFmpeg 6.1
  converts a full-range JPEG for `-pix_fmt yuva420p` alone, FFmpeg 9 hands the raw
  full-range bytes back untouched — the first thing the pinned build found.
- **Geometry is FFmpeg's integer geometry** (`layer_geometry.rs` in kerf-core, pure;
  re-exported as `kerf_gpu::geometry`): the first `crop` rounds with `lrint` and then to
  the picture's *native* chroma grid (`Subsampling`: even for 4:2:0, nothing for 4:4:4 —
  see `render_plan.rs` above for which positions), the fit uses `av_rescale`, `pad`,
  `overlay` and (unless the transform's own `scale` follows it) the Cover crop truncate
  and round down to even (the 4:2:0 grid, whatever the source was), `rotate` rounds its box half-up and samples bilinear about the pixel-index
  centres, chroma outside a rotated picture is clamped to its edge (FFmpeg leaves a few
  green pixels there; the GPU does not copy them).

**`tests/parity.rs`** (`#[ignore]`d; CI job `parity`, a **matrix of the distro FFmpeg and
the pinned one** — the two compose colour differently, so a green run on one proves
nothing about the other) renders each case at several times through `export_still` (a
PNG at the full canvas) and through the GPU and compares them:
**PSNR >= 40 dB and max per-channel error <= 8/255 outside the edge band**, where the
band is every pixel within 2 px of a >24-level step in the *reference* (>12 for a case
with a rotated layer, whose stair-stepped edge at a reduced opacity shows less contrast),
**at least half the frame must be outside it** (a case that is edge everywhere — noise, a
1-px checkerboard — takes `BUSY` limits: the whole image against the same error bounds),
plus a whole-image PSNR floor (40 dB, 30 dB for a case with a rotated layer) that catches
a layer a row off. Thresholds live as named constants at the top of the file with the
reason for each and the measured numbers at the bottom; a case never relaxes them
silently, and a new visual feature gets a case. Assets are made by the same probe an
import uses (`Project::probe_asset`), so a stream says what the app would know — and the
harness reads the policy the same way the product does (`composite_color_policy`), so
each case is judged against what *this* FFmpeg does: a stack of one matrix is compared
strictly, a stack the policy cannot draw (`check_mixed`: mixed tags on 9.0.2, where the
same case is compared strictly on 6.1) is **asserted refused by the plan**. Cases:
single clip (three sources), a gap, contain / cover into 9:16 / 16:9 and up / down
scaling, picture-in-picture (including a 361x203 layer in a 722x640 frame), scale + rotate
+ crop, odd-sized and off-canvas layers, **letterboxed layers over and under others** (bars
cover, graded bars), **opacity** (several sources and roles, a graded fade),
**matrices** (BT.709 and BT.2020 single and layered, mixed tags, an RGB PNG under and over
tagged clips, translucent layers over tagged ones in both orders), colour (all four knobs,
contrast + saturation only, warm / cool), PNG and JPEG stills, speed / reverse / keyframes,
10-bit / 4:2:2 / 4:4:4 / BGR0 / gray / full-range / odd-sized / metadata-rotated sources
(shrunk, fitted and **enlarged** — every resize of a non-4:2:0, non-gray format is
asserted refused, the mild shrinks included, and the same formats at their own size drawn), **busy sources** scaled by non-integer ratios, a clip past the end of its
footage (nothing drawn, like FFmpeg), the scaler plane by plane (including 8:1 to 20:1
shrinks), a rotated grey with no colour fringe, an asset that never recorded its pixel
format, **chroma-grid cases** (crops, Cover offsets and letterbox gaps chosen so the
4:2:0 and the native rounding differ, on 4:2:0 / 4:2:2 / 4:4:4 / gray / BGR0 / RGB PNG —
the crops of finer-chroma pictures that cannot be placed exactly are asserted refused),
the **unmeasured-policy** case (`Unknown`: BT.601 stacks drawn, BT.709 / BT.2020 refused),
the **full-range grading** cases (a graded `yuvj` clip, and a graded layer in a stack with
one, refused; ungraded and limited-range controls drawn),
and **refusals** (an EXIF-oriented JPEG, FFV1 `yuva420p`, the same with the pixel
format unrecorded, a translucent odd layer) — each of which FFmpeg still renders. 160
renders in the table on FFmpeg 6.1.1 (150 compared and 10 asserted refused on the pinned
9.0.2 the Windows and macOS bundles ship) plus the plane-level scaler runs, all passing
strictly. A failing case writes the reference, GPU and diff images to `target/parity/`
(`KERF_PARITY_KEEP=1` keeps them for passing ones; `KERF_PARITY_EXPLORE=1` prints
everything and fails nothing); every run writes `target/parity/report.txt`.
`tests/bench.rs` times a still on the GPU against FFmpeg at 1080p / 4K and 1 / 3 / 6
layers, decode apart from composite (`KERF_BENCH=1`). **The readback is bounded**: the
wait for the GPU to finish is `READBACK_TIMEOUT` (30 s), a `GpuError::Readback` rather
than a caller blocked forever on a wedged device.

```bash
cargo test -p kerf-gpu --no-default-features -- --ignored        # needs ffmpeg + an adapter
KERF_GPU_ADAPTER=hardware cargo test -p kerf-gpu --no-default-features -- --ignored   # the machine's GPU
KERF_BENCH=1 cargo test -p kerf-gpu --no-default-features --release -- --ignored --nocapture bench
```

### embedded MCP server (`crates/kerf-app/src/mcp.rs`)

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
`remove_clips`, `add_clip_to_timeline`, `split_at`, `generate_voiceover`'s
placement — take an optional `ripple` that is `project.with_ripple(p.ripple, …)`
around the core call (omitted follows the project; `false` is the escape hatch); their
descriptions say plainly that the push can be skipped, and an add *inside* a clip leaves
the overlap.
The ops that decide their own layout (`ripple_delete`, `cut_clip_range`,
`snap_to_beats`, `move_clip`, `move_clips`, `reorder`, `duplicate_clips`) take
none, which `ripple_is_an_optional_argument_on_exactly_the_edits_that_follow_the_mode`
pins against the generated schemas. The server `instructions` carry the ripple
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
`platform_check` tells it whether the cut is publishable where it is going
(and the server `instructions` tell it to run that before reporting a cut
finished — an agent that assembles a four-minute Reel has done the work and lost
the audience), and `export_cover` writes the thumbnail.
`stage_edits` / `staged_diff` (the entries plus a rendered text summary) /
`apply_staged_edits` / `discard_staged_edits` drive it explicitly, and
`revision_diff` explains a past revision. The server `instructions` spell the flow
out, since an agent that does not know its edits are held back would report a cut
the user has not got.

### kerf-app (`crates/kerf-app/src/lib.rs`, `main.rs`)

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
`move_clips { moves }` (a group, one revision, all or nothing), `ripple_delete`, `cut_clip_range` (remove a **source-time** span from a clip and
ripple closed — the transcript-editing primitive), `add_track`, `remove_track`,
`set_track_duck`, `set_track_volume` / `set_track_pan`, `set_delivery_format` (the project's delivery frame; omit
width/height to clear it), `remove_clip`, `remove_clips { clipIds, ripple? }`
(one revision; `ripple: true` is the multi-select ripple delete, via
`with_ripple`; omitted follows the project's mode), `set_volume`, `set_fade`,
`set_speed`, `set_transform`, `set_color`, `set_transition`, `set_mask`,
`set_video_effects`,
`set_audio_effects`, `set_keyframes` / `add_keyframe` / `clear_keyframes`,
`set_reframe` / `clear_reframe` / `set_reframe_keyframes` / `add_reframe_keyframe`,
`set_asset_projection` (asset-level 360 mark; returns the `Asset`),
`add_overlay` / `update_overlay` / `remove_overlay` / `set_overlay_keyframes`,
`generate_captions` / `clear_captions` (caption the whole cut, in timeline
time), `export_srt`, `remove_silence`, `snap_to_beats`,
`smart_crop` (frame each shot for the delivery frame),
`extract_audio`, `concatenate` — each returns the
refreshed `Timeline`), media (`get_frame` → base64 PNG data URL, `get_waveform`,
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
commitment to ten transcriptions), app preferences (`get_settings` /
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

**Auto-update.** The app updates itself from its own GitHub releases via
`tauri-plugin-updater` (+ `tauri-plugin-process` for the relaunch), both
registered in `run()`. `plugins.updater` in `tauri.conf.json` points at
`https://github.com/OrellBuehler/kerf/releases/latest/download/latest.json`
and embeds the **minisign public key**: a bundle only installs if its signature
verifies against that key, so the update path is not just "trust whatever the
URL serves". `bundle.createUpdaterArtifacts` makes `tauri build` emit the
updatable bundles (`.app.tar.gz` / `.AppImage` / NSIS `-setup.exe`) plus a
`.sig` per bundle — which means **a bundle build now needs the private key**
(`TAURI_SIGNING_PRIVATE_KEY`, or `TAURI_SIGNING_PRIVATE_KEY_PATH`, plus
`…_PASSWORD`) in the environment; plain `cargo build` / CI is unaffected.
`release.yml` passes those from repo secrets, and a **separate
`updater-manifest` job** assembles `latest.json` from the uploaded `.sig` files
*after* all bundles land (`includeUpdaterJson: false` on the build step): the
per-platform jobs run concurrently and each writing the manifest would leave
only whichever finished last. It runs under `!cancelled()`, not on plain
success — the matrix is `fail-fast: false`, and one platform failing must not
leave the release with no manifest at all, which would 404 the feed for
*everyone*. Prereleases are skipped, so they never become the update everyone is
offered.
A published manifest is then **read back** from `releases/latest/download/`
and the run fails if any of the five platform keys is missing — a partial
manifest still ships (better than none), but no longer silently. Nothing is
built until `ci-green` has seen a successful CI run on the tagged commit (it
polls, because a release is usually published while CI on the merge commit is
still running), and `attest` adds a build-provenance attestation to every
installer. **`prepare-release.yml`** (`workflow_dispatch`, input `version`) does
the release PR's edits: the three version fields, `cargo update --workspace`,
and `fetch-ffmpeg.mjs --repin`, which moves the FFmpeg pins to the newest
upstream builds and rewrites the script's digests. CI's `engine` job runs the
`#[ignore]`d binary tests against both the distro FFmpeg and the **pinned**
one on Linux, Windows and macOS, and runs weekly, so a pruned BtbN pin shows
up before a release needs it; the `parity` job is the same pair on Linux (the
GPU compositor against each FFmpeg, on lavapipe); a `libav` job compiles the `ffmpeg` /
`libav-render` features against the Ubuntu dev libraries.

**Publishing a release would open a gap in the feed**, so the workflow closes it:
the new tag becomes `releases/latest` the moment it is published, but its
`latest.json` is only attached ~25 min later when the slowest bundle (Windows)
finishes — `releases/latest/download/latest.json` would 404 until then and every
running install's check fail with the plugin's `Could not fetch a valid release
JSON`. A **`hold-release` job** (first in `release.yml`, no `needs:`, so it lands
seconds after the publish event) marks the release a *prerelease*, which parks
`releases/latest` on the previous version whose manifest is intact — a check
during the build says "up to date" instead of erroring — and `updater-manifest`
**promotes it back** (`gh release edit --prerelease=false --latest`) in the same
step that uploads `latest.json`, so `releases/latest` only ever points at a
release that already has its manifest. Both jobs gate on
`!github.event.release.prerelease`, the *event payload*, which the hold's own
edit cannot change — a release cut as a genuine prerelease is skipped by both and
never becomes `releases/latest`. A release that fails outright just stays the
prerelease it was parked as, which is the safe state; `api.ts` still rewrites the
plugin's error into an explanation (`describeFeedFailure`) as a backstop.
**PR builds** (`pr-build.yml`) bundle every non-draft, non-Dependabot PR for
Windows x64 / macOS arm64 / Linux x64 like the release does (bundled FFmpeg,
`--features whisper`) but unsigned, with no updater artifacts and the cargo
cache on, and upload them as 14-day artifacts. `pr-build-comment.yml` keeps one
comment on the PR linking them: it runs on `workflow_run` because a fork's run
only holds a read-only token, so it never checks out PR code, and it accepts the
`pr-number` artifact only when that PR's head is the commit the run built.
`workflow_run` fires only for workflows on the default branch, so a PR that adds
or changes the commenter is not commented on by its own version.
In-place update is per-platform: macOS and Windows
(NSIS, `installMode: passive`) always; on Linux **only the AppImage** — a
`.deb`/`.rpm` install fails the install step, which the dialog reports with a
link to the release page.

### frontend (`frontend/`)

SvelteKit 2 / Svelte 5 **runes** (forced on in `vite.config.ts`). Two layout quirks:
- **No `svelte.config.js`** — adapter and compiler options live inline in
  `vite.config.ts` via the `sveltekit()` plugin (new-style config). Static SPA via
  `adapter-static` (fallback `index.html`); `+layout.ts` sets `ssr = false` +
  `prerender = true`. Dev port is pinned to **1420** for Tauri.
- **Tailwind 4 = CSS config**, no `tailwind.config.js`. `src/routes/layout.css` imports
  the **Kerf design tokens** (`src/lib/styles/kerf-tokens.css`) and maps the shadcn
  semantic vars onto them. **Every color is themable**: `src/lib/theme.ts` lists
  the opaque color tokens (grouped for the editor), three presets (Kerf Dark —
  bun-tested to equal the stylesheet's defaults — Kerf Light, High contrast),
  `parseTheme` (a stored or imported JSON; unknown tokens dropped, missing ones
  filled from the scheme's preset) and `applyTheme`, which writes the tokens as
  inline properties on `<html>` (beating the stylesheet), toggles the `dark`
  class and `color-scheme`. Every translucent token in `kerf-tokens.css`
  (borders, fills, glows, the scrim) is a `color-mix` of an opaque base so a
  theme is a flat list of hex colors an `<input type=color>` can edit, and the
  editor components carry no color literals (`--scrim`, `--text-on-video`,
  `--drag-ghost`, `--frame-matte` exist for the few places that used to).
  A theme also carries a **shape** (`SHAPE_TOKENS`: `line-width`
  hairline borders / dividers / clip edges / ruler ticks, `line-emphasis` the
  selected-clip outline, `playhead-width`, and the slider's `slider-track`,
  `slider-track-radius`, `slider-thumb` plus a `round` / `bar` thumb style) —
  numbers clamped and snapped to their range by `parseTheme`, which fills a
  missing shape (an older theme file) from the scheme's preset; `shapeProps`
  turns it into the custom properties `applyTheme` writes, and the Dark preset's
  shape is bun-tested equal to the stylesheet like its colors. Every
  `input[type=range]` is styled once in `routes/layout.css` (webkit and moz
  pseudo-elements) from those variables, with `--slider-accent` as the one
  per-slider choice; changing a shape value makes the theme `Custom`, and
  High contrast ships thicker lines. `app.html` paints Kerf Dark before hydration
  (and the desktop window stays hidden until the theme is applied — see kerf-app).
  **Two bun guards keep this true** (`theme-guard.test.ts`): a scan fails on any
  color literal (`#hex`, `rgb()`, `hsl()`, …) in `src/` outside `theme.ts`,
  `kerf-tokens.css`, `app.html` (pinned to Kerf Dark's `surface-app`, as is the
  window's `backgroundColor`) and the two browser-harness image generators
  (`sample-frame.ts`, `sample-filmstrip.ts`, whose colors are picture, not
  interface); and every preset must meet WCAG contrast (`contrast.ts`) for the
  pairs the UI draws — 4.5:1 for reading text and the labels on solid fills
  (`text-primary` / `-secondary` on every surface, `text-muted` on the resting
  ones, `text-on-accent`, `agent-fg`, `text-on-video` on the clip bodies), 3:1 for
  muted text on hover / active, accent and status hues as text on the panels and
  the strokes that carry meaning (clip edges, the playhead / selection amber, the
  waveform, the drag ghost), plus the `color-mix` fill of a generated caption's
  block (`GENERATED_TITLE_FILL`, resolved per preset by `mixSrgb` and held to 4.5
  under its label). `text-disabled` is exempt on a *disabled* control but the UI
  also draws the idle state of live toggles in it (DUCK / S / L, the bell), so it
  is held to 3:1 on the resting surfaces; the translucent hairlines are exempt.
  High contrast is also held to 7:1 on its reading text. The pair list
  and its reasoning live at the top of that test; a failing preset gets its
  *values* fixed, not the list. Writing it found real defects — Kerf Light's clip
  bodies were pale under white labels (1.5:1), the logo mark was a hard-coded
  near-white that vanished on Light, and five dialogs carried a dead `rgba`
  shadow fallback — so Light's clip fills, waveform, amber, three status hues,
  muted text, disabled text and `agent-fg` were adjusted (Dark and High contrast
  passed as they were). **A stored theme is a copy**, so users who had picked Kerf
  Light kept the old colors: `upgradeStoredTheme` (on what `settings` reads back,
  never on an import) moves a theme whose colors are *exactly* a
  `SUPERSEDED_KERF_LIGHT` set to the current preset — name and shape kept — and
  leaves anything else alone; whenever Light's colors change again, the outgoing
  set goes on that list. `layout.css` is also the `tailwind.css` in `components.json`. Run
  `bunx shadcn-svelte add <name>` to add primitives.

The editor UI is implemented from the **Kerf design system** (claude.ai/design): an
editor-grade workspace under `src/lib/components/editor/` — bespoke atoms (`Btn`,
`IconBtn`, `Badge`, `Icon`, `KerfMark`) plus `TitleBar`, `Toolbar`, `StatusBar` as
fixed chrome around a **dockable workspace** (`Workspace.svelte`, composed by
`routes/+page.svelte`). The workspace is `dockview` (the vanilla package; its
`--dv-*` variables are mapped onto Kerf tokens in `styles/dockview-kerf.css` so
it follows the theme) hosting six panels — `LibraryPanel`, `Preview`, `Timeline`,
`Inspector`, `AgentPanel`, `DeliverPanel` — each a Svelte component
`mount`ed into a dockview content element, so every panel is resizable by its
sash, movable by its tab (drop zones on any group edge, or tabbed into a group)
and closable; the toolbar's **Panels** menu reopens one (the library left of the
preview, the deliver panel right of it, the rest beside the active group) or
resets the workspace. **Workspaces** — Edit / Color / Audio / Motion / Deliver,
toggle buttons in the **title bar** (`WorkspaceTabs`, the centre of a three-column
grid so they stay on the centre line; `aria-pressed`, since there is no tabpanel
to point a tablist at; a pointer click blurs the button, because a focused button
swallows Space and the transport shortcut would stop working) — are full dockview
presets. `src/lib/layout.ts` is
the pure, bun-tested side: the panel registry (titles, minimum sizes — the
Inspector's px-tuned controls need ~250), `PRESET_LAYOUTS` (each a row of panels
over a **full-width timeline** — the cut is what an editor looks at most, and a
timeline squeezed between two side panels showed thirty seconds of it; the agent
is a tab beside the inspector, or the deliver panel, in every one, so a proposal
that lands has somewhere to appear) and `sanitizeLayout`, which turns a stored
layout into one that can be trusted (known panel ids, each shown once,
titles/minimums re-taken from the registry, floating groups dropped) or `null` so
the preset is used. It also **migrates** a layout saved when the media bin and
transcript were panels of their own: the first of `media` / `bin` / `transcript`
found becomes `library`, the others drop, and an emptied group or branch is
pruned (a branch left with one child collapses into it). What is stored is
**`Settings.workspaces`** (`workspaces.ts`, bun-tested): `{active, layouts:
{<workspace>: <layout>}, library: {tabs: {<workspace>: <tab>}, collapsed}}`, parsed
field by field — one bad layout costs that workspace its arrangement, not the
other four — with the old `layout` becoming Edit only when there is *no*
`workspaces` value at all (a reset Edit must not be brought back by a layout from
the old build), and the old single `library.tab` becoming the active workspace's
own. `settings.svelte.ts` holds it as the live copy and writes it through
`single-flight.ts` — **one write in flight, newest wins**, the follow-up reading
the state when it starts (a dock save, a rail click and a switch overlap, and two
racing writes could leave the older on disk; bun-tested). `workspace.svelte.ts`
is the runes singleton: `switchTo` keeps the arrangement being left, swaps the
layout and shows the library tab that workspace last had (each remembers its own;
until one is picked it is the workspace's tool: Color → Effects, Audio → Audio,
Motion → Transitions, Edit and Deliver → Media), and **touches no project state**
— cut, selection, playhead and playback are `editor` / `ui` and survive (dockview
rebuilds the panels, so a panel's own scroll starts over). **A layout is written
only if it was rearranged**: dockview reports layout changes for a great deal
that is not one (a restore, the library folding, a click that moves the active
group, a window resize), and writing each marked every workspace merely visited
as customised and brought a just-reset one straight back. So after a restore the
singleton waits two frames for the layout to settle, takes that as the reference,
and `shouldPersistLayout` (pure, bun-tested) writes only a layout that
`sameArrangement` finds different from it — same groups, panels and order, same
*shares* of each branch within 0.4 % (a dozen-pixel nudge of a sash counts; pixel
sizes, the active group and the active tab do not) — and, with no entry yet, from
the preset. What was written becomes the new reference; Reset clears the entry
and leaves none. A window resize can move shares too (where a group's minimum
binds), so a `ResizeObserver` on the dock host writes what was pending, ignores
the layout events the resize causes, and retakes the reference once the window has
held still for 150 ms — otherwise the next unrelated event, a click on a tab,
would write a layout nobody arranged. After
every `fromJSON` it forces `api.layout()` at the host's real size: a layout
is built at the size it was saved at and the dock learns its real one a frame
later, and a constraint changed in that gap makes dockview re-split the whole
grid evenly. `+page.svelte` mounts the dock only once the settings are loaded so
it restores rather than rebuilds. Panels own no width: their roots are
`flex:1;min-height:0`, and a panel's minimum comes from the registry.
The **library** (`LibraryPanel.svelte`) replaces the old Media | Transcript tab
group with an icon **rail** (36 px icons in a 40 px column, tooltips and
`aria-label`s, a roving-tabindex tablist: arrows move focus, Enter / Space / click
choose; a pointer click does not leave focus on the rail): Media (`MediaBin`, whose
decoded thumbnails live in `thumbnails.ts` rather than the component — the library
remounts it on every tab switch, unfold and workspace switch, and each remount
used to decode every asset again; it keeps answers, including "no frame", but not
a failed decode, which is retried on the next mount), Titles (`TitlesControls`), Effects (color looks +
video effects), Transitions (the grouped picker), Audio (audio effects + the
voiceover entry point), Transcript (`TranscriptPanel`). Effects, Transitions and
Audio act on the selected clip and say why they are off when none is; they own no
value (a look is a `Color`, an effect an entry in the clip's chain), so the
Inspector stays where you tune — the presets are `effect-presets.ts`, shared with
its pickers. Titles is the same component as the Inspector's *Titles lane*
section, which stays (a folded library must not make titles unreachable), over
`title-actions.ts`; the caption look they share is `ui.captionStyle`. Clicking the
active icon **folds** the content to the rail (`library.collapsed`, persisted and
shared by every workspace — the rail is a tool, not part of an arrangement) and
the panel gives its width back: the registry minimum is the rail's 40 px, an open
library raises its group's to 240 and a folded one pins min = max = 40 through
`group.api.setConstraints` — a group's explicit constraints win over its active
panel's minimum, which is set when the panel is created, and the *panel*-level
`setConstraints` has no listener on a dockview panel and does nothing — hides the
group's tab strip, and hands the width it frees or takes to the group beside it
(dockview would give it to the last group in the row). Folding from the header's
chevron by keyboard moves focus to the rail's active tab, since the chevron
unmounts with the content. A library sharing a group with another panel cannot
fold — it has no width of its own to give back.
The **Deliver panel** (`DeliverPanel.svelte`) docks the export dialog's readiness
verdict and *Deliver to* shapes, extracted into `Readiness` / `DeliverTo` /
`SectionHead` which the dialog uses too — no fork. The shape choice and the
smart-crop toggle live on `ui` (`deliverShapes`, `deliverSmartCrop`) because both
places edit them (session-global, so a shape ticked in the panel is ticked when the
dialog opens), and both components re-judge on every `editor.timeline` change
(docked beside a timeline being edited, a verdict cached at tick time goes
stale), keeping the newest answer. Shapes are for a picture, so the panel hides
them, and the button never says "Export N files", for a cut with no video clip
(`hasPicture`, the same gate the dialog uses). The render still goes through the full dialog
(`ui.openExport()`); the panel shows its progress and Stop while one runs.
The `Inspector` is **mounted whether or not a clip is selected**:
its Text overlays section belongs to the timeline rather than to any one clip, so
gating the panel on a selection made titles and captions unreachable until you
clicked a clip. Its sections are `InspectorSection`s — native `<details>`
disclosures, so they are keyboard-operable for free — each with a one-line
**summary** on the right (`1.00×`, `Cropped`, `2 keyframes`) so a collapsed
section still says what it holds; Timing, Volume and Titles open by default,
everything else is folded. It edits the selected clip —
trim, volume, fades, speed, transform, color, **transition** (a grouped picker
over `src/lib/transitions.ts` — fade / slide / push, then a direction, because
that is the order the choice is actually made and a flat list of eleven names
hides it; its bun test pins the ids against `TransitionKind::ALL`), plus **video / audio
effect chains** (add / tune / remove), **keyframe animation** (the Transform panel
auto-keyframes at the playhead and shows the sampled pose), a **Framing** section
(a `Smart crop` button that frames *this* shot for the delivery frame, plus
`Reset crop`, above the crop sliders it writes — greyed out with a reason when the
shot already matches the frame or is 360), a **Mask** section (None / Rectangle /
Ellipse chips, then centre / size / feather / invert; picking a shape starts from
a visible default rather than a collapsed one, and the caption carries the recipe
the shape alone does not suggest — a lower track shows through, so a blurred face
is a duplicated, blurred copy above, masked), a **360 reframe**
section (yaw / pitch / roll / FOV, auto-keyframing
at the playhead like Transform — note its `lerpAngle` takes the shortest arc, which
plain `lerp` would read as a 340° swing across the seam; for a source Kerf did not
detect as 360 it instead offers a projection picker that marks the whole asset via
`set_asset_projection`), and an always-visible
**Text overlays** section (add titles / lower-thirds, caption the whole cut in
a **Lines / Word punch** style chosen by two chips above the button — the
selection is deliberately *not* derived from the overlays already there, since
a caption's style is not recoverable from its text and guessing it from the
word count would flip the chip whenever a sentence happened to be short —
the button relabels to `Recaption` once there are generated captions, since a
later trim moves the words out from under them, with `Clear` beside it taking
only the generated ones — and edit text / timing / position / size / color /
box / bold).
**Polish presets** (`src/lib/style-presets.ts`, pure data over the existing
surfaces): the Color section leads with one-click **looks** —
Punchy / Warm / Cool / Faded / B&W chips (the active one highlights; the sliders
show exactly what a chip applied) built on `Color.temperature`, a warm-cool
channel in -1..1 rendered as opposing `eq` per-channel gammas (`eq_filter` —
omitted at 0 so old graphs stay byte-identical; plain saturation/gamma can't
tint) — and the Text overlays section leads with **Title / Lower third /
Caption** style chips that create a styled overlay at the playhead with
fade-in/out opacity keyframes; the caption style matches what
`generate_captions` writes in its `lines` style, so manual and generated
captions look alike (`CAPTION_LOOKS` in the same file is only the two
generate-time labels; their numbers live in `captions.ts`).
Everything is styled with the CSS-variable tokens directly (inline `style`), not Tailwind
utilities. **Titles are their own items, not part of a clip.** `Timeline.overlays` has always
been timeline-level, and the UI now says so: the Timeline has a **titles lane**
(`T`, above V1; `data-title-lane`) where every title / lower-third / caption is a
block from `start` to `end` (generated captions dashed and dimmer; overlapping
items stack into rows via `packRows`). Click selects (`editor.selectOverlay`,
exclusive with the clip selection; click also seeks into the title), the body
drags in time, the 6px edges trim, with the clip drag's snapping (0 / playhead /
beats / every clip edge / other titles), Delete removes it, and one
`update_overlay` is written per gesture. A selected title makes the Inspector show
**that title's editor** (text, timing, position, size, colour, box, font, bold,
keyframes) *instead of* the clip sections; the add / caption controls live in a
"Titles lane" section below. The **Preview** draws an interactive box over each
title visible at the playhead (hidden while playing): drag to move, corner
handles to resize (scales `size` by the pointer's distance from the box centre),
Escape / pointercancel / blur abandon it, pointer capture holds it, local state
updates live and **one** backend edit lands on release. The box is laid out in
the engine's units: `cqh` against a size container covering the drawn picture,
centred on `(pos_x, pos_y)`, font `size` of the height, browser text metrics
standing in for drawtext's, `boxborderw` as padding, so it is aligned to within
font-metric differences. A **keyframed** title follows the Transform convention:
moving it keyframes the position at the playhead (`editor.moveOverlay`, updating
an existing keyframe within 20 ms, else inserting one carrying the opacity in
force), because the static `pos_x/pos_y` is not what an animated render reads;
resizing always writes the static `size`. Pure logic (box math, keyframe upsert,
row packing, snapping, span trim) is `src/lib/titles.ts`, bun-tested.

The **timeline is a bespoke NLE timeline** that renders **real `editor.timeline`
state** (ruler + tracks + clips positioned by `timeline_start`/duration at `ui.zoom`
px/sec + playhead), with scene markers / silence regions / **beat ticks** (the tempo grid
of audio-track clips, confidence-gated, hidden when beats land closer than 4px — from
`src/lib/beats.ts`, the TS mirror of the Rust beat math that the ruler, the drag
snapping and the browser harness's alignment all share, unit-tested with `bun test`)
mapped from `AssetAnalysis` and
real audio waveforms (below); the razor tool splits, Delete removes, Shift+Delete
ripple-deletes, clicks select/seek, and (pointer tool) **clips drag to reposition** — free
positioning with gaps, snapping to clip edges / playhead / 0 / beats, and **dropping onto another
same-kind track** (`move_clip`, via pointer events + `data-lane` hit-testing) — and
**edge-drag to trim** (6px `ew-resize` handles; clamped to source handles, neighbors and
a 0.05s minimum; left edges commit `trim_clip` with `timeline_start` so the right edge
stays put; stills extend freely since they loop).
**Gestures are frame-quantized** (`src/lib/frames.ts`, bun-tested; keyframes stay in
seconds): a trim, move, drop, razor cut and fade length land on a frame of the cut's
rate — `editor.fps`, i.e. `timelineFps`, the first video clip's rate else 30, which is
`export_format`'s rule. Each rounds **once, from the raw pointer position** (a frame is
`k / fps` from an integer `k`, so equal frames are equal doubles and nothing drifts over
a long run of edits), the ghost and the commit use that one value, and a trim derives
every field from it (`trimEdit`). A magnet within reach (`ui.snap`: 0 / playhead / beats
/ clip edges) still wins, unrounded; frames are *not* a magnet and apply with snapping
off too. A landing within 1 µs of a neighbour's edge *is* that edge (`welds`):
`move_clip`'s overlap test is a strict float compare, and an edge computed as
`start + length / speed` can sit an ULP past its frame — so a tail butted against a
neighbour is placed by `startBefore` (the latest start whose `start + duration` does not
pass it, in the backend's own arithmetic; `tail - dur` is an ULP too high about as often
as not). Whether a press became a drag is judged on pointer travel (3 px), never on the
quantized position (one pixel is under a frame at high zoom), and an edge keeps the
offset it was grabbed at. A razor cut keeps half a frame
either side (`splitPoint`; a clip with no interior frame says so), the context menu's
split quantizes the playhead, and Escape / pointercancel / blur abandon a clip, edge or
title drag.
**Ripple, the selection set, group moves and zoom** are the timeline's editing layer, each
a pure bun-tested module under the component. *Ripple*: `editor.rippleMode` mirrors the
project flag — read in `load()` (so launch, New and Open) and again on the
`ripple-mode-changed` event an agent's `set_ripple_mode` emits (with a toast, since it is
the user's own toolbar setting that moved); the toolbar's **Ripple** toggle (`R`,
`aria-pressed`) is lit while on, with a second cue in the ruler corner and an accented
ruler underline, and its tooltip says each track ripples on its own (no sync lock yet).
All the rippling is the backend's, but the GUI shows it: with ripple on, an edge drag is
no longer stopped by its neighbours (it pushes them — only the source's footage, the
0.05 s minimum and 0 stop it; `ripple-trim.ts`'s `trimBounds`), and its ghost is the
*outcome* (`rippleTrimPreview`: the trim applied to a scratch copy of the track, then
`ripple.ts`'s `rippleFrom`), because a left-edge trim keeps the clip's start rather than
holding the right edge — one ghost per clip it moves, the moved clips dimmed, red and
inert if the backend would decline the ripple. The bounds are asked again at every move
and at the release, since the mode can flip mid-drag. `load()` reads the flag on every
load (so Open and New refresh it; `state-ripple.test.ts` pins that). *Selection* is a set: `selection.ts` holds every way of
changing `selectedClipIds` + the primary (`selectedClipId`, the clip the Inspector edits —
with several selected it shows an "N clips selected" note, since its sections act on that
one): a click replaces, Ctrl/Cmd toggles, Shift extends along the primary's track (adds
the clip when the primary is elsewhere), and a **marquee** (pointer tool; a drag from empty
lane, the titles lane or the space under the tracks) selects every clip its rectangle
touches — Shift adds, Ctrl/Cmd toggles — recomputed from the selection as the press found
it so the rectangle can shrink (`marqueeSelect`; `marquee.ts` tests the rectangle, in lane
space, against lane boxes measured from the DOM since the heights are CSS). Clips on a
locked track are not swept up (locking guards edits, and the selection is what every edit
acts on) but stay clickable. Escape mid-drag restores the selection; the click that ends (or follows an abandoned)
marquee is swallowed — held until it arrives or the next press, not on a timer — so it
does not seek and deselect; Escape otherwise clears (the page's
handler — the timeline's and the preview's abandon-a-gesture handlers run in the capture
phase and stop the event, so abandoning a drag never also clears). `#setTimeline` prunes
ids another edit removed. *Group move*: pressing a selected clip of several keeps them
(a click that never drags narrows to it), and dragging moves them all by the grabbed
clip's Δt — its start is the one snapped and frame-quantized, its group's own edges being
no magnet — plus one **lane offset applied within each kind's lanes** (`multi-move.ts`,
`planMove`). Its checks are `Timeline::move_clips`' (group as a group, before 0 refused not
clamped, locked or missing lane refused) and a property test replays random drags against
`multi-edit.ts`'s mirror, so the verdict drawn while dragging is the backend's: one ghost
per clip, red with the reason beside the pointer when refused (letting go then does
nothing), and a valid drop is ONE `editor.moveClips` — one revision, one undo. Delete is
`removeClips(ids)` (one revision; ripple follows the project's mode) and Shift+Delete
forces ripple; a clip on a locked track is left alone and stays selected, and Cut
(⌘/Ctrl+X) copies only what it can remove and says so when that is nothing (`ops.ts`
`deleteSelection` / `cutSelection`). *Zoom*
(`zoom.ts`): 0.05–2000 px/s, `ui.zoom` still px/s but stepped by ratio (+/-, buttons ×1.25)
and a logarithmic slider; ⌘/Ctrl + wheel is exponential in the delta (a pinch is smooth) and
holds the time under the pointer (`zoomAround`; the scroll is applied after the lane has
been rewidened); **⇧Z / the fit button** fits the cut (`ui.zoomToFit()` bumps `fitEpoch`,
since only the timeline knows its width). The ceiling comes down for a very long cut so the
lane stays under 8 M px — the one thing a browser cannot lay out. Nothing else assumed a
range: the waveform rung choice scales to any px/s (bottoming out at the engine's 2 ms
bucket, 4 px at the ceiling) and frame snapping works in seconds. `ruler.ts` makes the
label step follow the zoom and renders only the ticks in the visible window (hundreds,
not an hour's worth), with sub-second labels and, once a frame is 8 px wide, a mark per
frame.
**Waveforms** are one `<canvas>` per audio clip covering only the on-screen part of it
plus overscan (`ClipWaveform.svelte`; a one-hour clip at 96 px/s is 345 600 px, which no
canvas holds). `waveform-view.ts` is the pure geometry: `sourceAt` maps clip pixels to
source seconds through `source_in`/`source_out`, speed and reverse (a reversed clip is
read through the mapping, mirrored, never flipped), the bucket width is the widest rung
(the backend's 2 / 10 / 40 / 100 ms levels, then doubling) within 1.5 device pixels
(DPR capped at 2), and what is fetched is fixed **tiles** of 2048 buckets aligned to the
*source* clock — so a scroll, a trim, or a split's two halves land on cached tiles.
`waveform-cache.ts` (injectable fetcher; the app's instance is `waveforms.ts`) caches by
asset + window + bucket count, joins in-flight requests, runs three at a time newest
interest first and drops queued tiles nobody wants any more, remembers a failed asset
(one notification, no re-request per scroll; held off 30 s, doubling per failure in a row
up to 10 min, cleared by a tile arriving), and is LRU-bounded. `want` tells its owner
(microtask) when everything it asked for was already cached or the asset is held off,
because nothing else would — a clip that looked a moment before another clip's request
landed the same tiles would otherwise stay unpainted. The draw waits until every tile it
needs is cached and until then leaves the old bitmap where it was, placed by clip-local
*time* so a zoom or scroll shows it stretched, not blank; a clip scrolled out of range
releases its tiles and shrinks its canvas to 1x1 (a canvas keeps its whole backing store
otherwise), and a redraw only assigns the canvas size when it changed.
`waveform-draw.ts` fills one polygon per lane (not a line per sample), scaled by
`effectiveGain` — clip volume through the track fader, as the export multiplies them —
and repaints columns at full scale (|peak| ≥ 0.999, or pushed there by gain) in `--danger`;
a canvas cannot read `var()`, so `readPalette` resolves `--waveform` / `--danger` once per
`settings.theme` change. A stereo clip gets two lanes when its clip is at least
`STEREO_MIN_HEIGHT` (48) px tall (`laneCount`, a function of pixels so the track-height
presets drive it) and folds to one below that; the default 64 px track is stereo.
`get_waveform` is no longer used by the timeline (the MCP tool keeps it).
**Filmstrips** are the video twin: one `<canvas>` per video clip over its on-screen part
(`ClipFilmstrip.svelte`; the waveform's windowing, DPR cap and size discipline), blitted from
the asset's `get_filmstrip` sheets. `filmstrip-view.ts` is the pure layout: a clip is a row of
**slots**, each the thumbnail's aspect at the clip's height in whole px, the grid anchored at
the clip's left edge (a scroll moves nothing on it); slot `i` shows the frame at the source
time under the middle of its *visible* part, through `waveform-view.ts`'s `sourceAt` (trim /
speed / reverse — a reversed clip's footage runs backwards, never flipped) and
`filmstrip-geometry.ts`'s `frameAt` / `locate`; slot edges are rounded as *edges*, so
neighbours never seam; a still is its one thumbnail repeated. `filmstrip-draw.ts` paints;
`filmstrip-cache.ts` (injectable fetcher + decoder; the app's instance is `filmstrips.ts`,
which decodes a sheet's `data:` URL through an `Image` then `createImageBitmap` — never
`fetch`, `connect-src` refuses `data:`) keeps one decoded strip per **asset** (a split's two
clips share it): one in-flight load per asset, two at a time newest interest first, queued
loads nobody wants dropped, a failed asset held off with `backoff.ts`'s doubling cooldown (the
waveform cache's rule; one warning toast), memory bounded **in bytes** (192 MB): LRU applies only to
assets no visible clip holds (`hold`/`want` register a clip as owner, `release` drops it), so
a held asset is never evicted and a working set over budget overshoots until it scrolls away
rather than thrashing; `prune` and `clear` notify the owners of what they drop. A clip box under 28 px shows none and never fetches;
until drawn the clip keeps its plain look, and once drawn its label moves to the foot on a
scrim backing so a bright frame cannot swallow it.
**Track heights** are three named presets (`track-heights.ts`): compact 32 / medium 64 (what
it always was) / large 112 px of lane. Everything else is a function of the clip box the lane
leaves, so nothing else knows about presets: compact (21 px) folds a stereo waveform to one
lane, shows no thumbnails or clip grab handles, and drops the header's mixer strip; large
(101 px) gives two readable lanes and near-native thumbnails. It is a viewer's choice, not
part of the cut, so it is **UI-only** (`ui.heights`, per track id in `localStorage`
`kerf.timeline.heights`; a `Track.height` field would be engine work for a per-viewer
convenience): `all` is the global choice (what "all tracks" last set, what a track with no
choice of its own is, and what the **titles lane** follows), a track set to it drops its
override, "all tracks" clears every exception, and the table is capped at 256. The toolbar's
three glyph buttons set all tracks (lit when every track agrees); a track header's name is its
menu (so is the header's right-click). Marquee hit-testing and lane `offsetTop` read the DOM,
so they follow the heights. The compact titles lane is 27 px (`MIN_TITLE_LANE_PX`: the 26 px
add button plus the lane's border).
The **minimap** (`Minimap.svelte` over the pure `minimap.ts`; toolbar toggle, remembered) is
the whole cut on a 36 px strip: a block per clip per track row (runs too fine to tell apart
merge, so blocks are bounded by the strip's width), the playhead, the in / out marks, and the
visible window as a box. The box and the timeline's `scrollLeft` / zoom are one thing seen two
ways (`windowRect` view -> box, `targetForRect` box -> view). Drag the body to scroll (the zoom
is kept *exactly*, not re-derived from a box widened to its 8 px minimum), drag an edge to
zoom (the other edge stays put, even after the zoom was clamped), press the bare strip to jump
there (and keep dragging), double-click to move the playhead. Gestures are absolute from the
press, Escape restores the view, and the timeline applies the result like a wheel zoom
(`pendingScroll`). `rowLayout` always fits the strip (the gap gives first, then 1 px rows,
then fractional rows), and the strip is `aria-hidden` on purpose: the timeline's own keys
(scroll, zoom, ⇧Z, J/K/L) are the accessible path.
**`ClipOverlays.svelte`** is everything on a clip beside its body: a **volume line**
(dB scale −36 dB…`MAX_GAIN` (+6 dB, `mixer.ts`) — one ceiling shared with the Inspector's
slider and the track fader, a clip set above it by an agent keeps its value and is drawn at
the top; the bottom edge is silence; a drag is relative to the clip's *real* level, so a
small drag moves a 6x clip from 6x instead of collapsing it, with a detent at exactly 0 dB,
since the export omits unity from the graph; double-click resets), **fade handles**
at the top corners (picture and sound both fade, so every clip has them; the volume line
is only for clips whose asset has audio; clamped to the clip and to each other,
double-click clears), the fade ramps, **keyframe diamonds** (clip-local seconds; click
seeks), and the **trim edges** with their halos. The top 14 px of a clip is the handles'
alone and the line's travel stays below it; the edge strips and the line's grab band do
not overlap; nothing is hit-testable until the clip is hovered or selected, nor under the
razor, nor on a locked track (keyframes still seek). Every gesture is `drag.ts`'s
`beginDrag` (pointer capture; Escape / cancel / lost capture / blur abandon), shows its
value live (the waveform follows the volume line) and writes **one** edit on release,
holding the live value until that edit settles. The ruler renders **in/out marks**
(`I`/`O` set at the playhead, `⇧I`/`⇧O` clear) that drive range export. Transport is
**J/K/L shuttle** (repeat taps double to ±8×) plus Space; playback is **audible**:
`src/lib/audio.ts` is a Web Audio engine that fetches clip PCM windows over `get_audio`
and schedules them with volume / fades / speed / reverse applied. **Per-clip effect
chains are auralized**: passing `clipId` to `get_audio` decodes the window through that
clip's own ffmpeg chain (`audio_effects_filter`, the same string the export renders), so
the chain is part of the buffer cache key and retuning an EQ re-fetches. It runs before
this engine's gain envelope where the export runs it after the clip gain — audible only
to a level-dependent effect, and keeping volume in Web Audio is what lets the fader stay
live instead of re-fetching PCM on every drag. Reverse shuttle is still silent. The
playhead follows the audio clock — edits mid-playback re-anchor via
`ui.resync()` from a `+page.svelte` effect. The timeline
toolbar's `+ V` / `+ A` add tracks and each track header has a `×` to remove one
(`add_track` / `remove_track`) and, on audio tracks, a **DUCK toggle**
(`set_track_duck`); the timeline is genuinely **multi-track**. Any track that can
actually be heard — an audio track, or a video track whose clips carry sound —
also gets a **mixer strip** (level fader + pan, double-click to return either to
neutral, tooltips in dB and L/R); a silent track gets none. `src/lib/mixer.ts` is
the *faithful* mirror of `Track::pan_gains`, because preview playback renders the
pan as the same balance the export does — a `StereoPannerNode`'s constant-power
law would quietly disagree with the file, and `get_audio` hands back mono, so the
two gain legs into a merger *are* the stereo pair. The old
`@xyflow/svelte` `TimelineCanvas`/`clip-node` scaffold was removed (the
dep is still in `package.json`, now unused). The toolbar carries a **delivery frame picker** (Source / 16:9 / 9:16 / 1:1 / 4:5,
from `src/lib/delivery-formats.ts`, bun-tested) that sets `Timeline.format` — the
preview pane then *is* that frame (sized with `100cqh` container units so a 1:1
frame is height-bound in a wide pane, not squashed), and for a vertical or square
delivery it draws **safe-area guides** (the platform's top strip / caption rail /
action column, plus a title-safe box; `settings.safeAreas`, **off by default**, toggled from
Settings › Preview or the preview context menu, which both write the same
persisted `Settings.safe_areas` — it is held process-wide in `settings.rs`
rather than in the engine, since nothing in kerf-core cares about it). The export dialog's "Source" resolution relabels to
**Project frame (WxH)** so the two surfaces cannot silently disagree. The dialog
is **preset → destination → picture → sound → where it is going**, with a
`Quality` select of three named CRF points (Smaller file / Balanced / Higher
quality, derived from the codec's CRF range) standing in for the encoder; the
codec, rate control, tune/profile/pixel format, audio codec and container knobs
all live behind one **Advanced encoding** disclosure, and a CRF typed there
reads back as `Custom` in the select. Loudness normalization sits beside
`Include audio` rather than in Advanced, because for the social-video user it is
a polish switch, not an encoder setting. The **readiness panel** stays visible
(only its tips fold): "Ready for Instagram Reels · YouTube Shorts ·
TikTok", then any length errors / reach warnings one line each, then a *single*
collapsed line for shape ("A 16:9 cut is letterboxed on … Pick a delivery frame
in the toolbar") — grouped by `IssueKind`, because otherwise four vertical feeds
each say the same thing. It re-checks against `opts.resolution`, so a 9:16
project exported at 1920×1080 is judged as the landscape file it will be.
`kerf_core::platform` decides all of it; `src/lib/platforms.ts` is a bun-tested
mirror used **only** by the browser harness, so the panel is drivable under
`bun run dev`. `src/lib/smart-crop.ts` is the same arrangement for smart crop: only
the *shape* arithmetic is mirrored (bun-tested), because the harness has no decoder
to sample with and so lands on the centre window — which part of the shot survives
is the half that only exists with media behind it. `src/lib/captions.ts` is the
same arrangement again, but *faithful* rather than approximate — captioning is
arithmetic all the way down, so the harness produces exactly the captions the
backend would (the mirror caught the two-captions-at-once collision the Rust
tests had not). The **cover frame** is saved from the preview's context menu
(`Save cover frame…` → `export_cover` at the playhead), and both a finished
export and a saved cover offer **Show in folder** in their toast.
`Preview` shows the composited frame under the playhead, and during
**forward 1× playback it switches to the streamed frame source** (`start_playback`)
— per-frame `get_timeline_frame` decodes stay for scrubbing, shuttle and the
settled frame, where you want *one* frame rather than all of them. Its effect keys
off `ui.seekEpoch` (bumped only by a deliberate seek or a fresh play) and never off
`ui.time`, which ticks every animation frame and would respawn ffmpeg 60×/sec.
Which frames survive is `playback-sync.ts`'s `createFrameGate` (`show`/`skip`/
`resync`, unit-tested — the one piece of frontend logic with tests): every frame
arrives late by a *constant* transport cost (ffmpeg's spawn, then base64 + JSON +
IPC) that on its own exceeds the two-frame `STALE_AFTER` budget, so lag is judged
against the smallest this stream has managed rather than against zero — measuring
from zero dropped every frame forever and froze the pane. Only growth past that
floor is drift: `STALE_AFTER` skips the frame, `RESYNC_AFTER` restarts the stream
from the playhead rather than playing it out in slow motion against the sound.
`start_playback` logs `frames` / `first_frame_ms` per run, which is what separates
"never started" from "sent but dropped"; in the browser harness `startPlayback`
**synthesizes** frames at the requested fps behind a deliberate 90 ms lag, so
playback moves under `bun run dev` and that failure mode is reproducible without a
desktop build. `ExportDialog` (⌘E) drives
the full `ExportOptions` surface — presets, containers/codecs, rate control, resolution,
loudness normalize, and a **Range: In → out** choice when marks are set. It **opens on
the frame the project is cut for** (`initialExport`): the preset whose resolution is
that frame when one matches, else the default preset with its resolution cleared so
"Project frame" renders — otherwise a 9:16 project opened its export already
landscape and the readiness panel warned about the shape the user had just chosen.
Its **Deliver to** section is the multi-format export: shape chips (the
`DELIVERY_PRESETS` minus Source) that each add a file beside the chosen path
named by shape (`variantPath`, the bun-tested mirror of
`ExportVariant::beside`), a *Smart crop each shot for every shape* toggle, and a
per-file readiness line judged at that file's frame (`platformCheck([w, h])`),
in place of the single panel — with shapes picked, the Scaling rows hide (each
delivery brings its own resolution and fit) and the button reads `Export N
files`; `export-progress` then carries `variant` / `total`. The
**Transcript tab** of the library (`TranscriptPanel.svelte`, over the pure, bun-tested
`src/lib/transcript.ts`) **is an editing surface**: lines resolve to the clip carrying them,
click seeks, the playhead line highlights, and `×` cuts the sentence from the timeline
(`cut_clip_range`); cut lines render struck through. When it is *empty* it says which
of the five reasons applies (nothing selected / no backend / model not downloaded /
not analyzed / no speech) and offers the matching action — a model picker + download,
or Analyze — instead of a dead end. The **agent panel is a real MCP task
queue** (status · queue · history · add-task) — Kerf has no in-app chat; a connected
LLM claims tasks over MCP. The queue is `agent` state (`src/lib/agent.svelte.ts`, a third
runes singleton) backed by the `tasks` table over Tauri/MCP: the add-task box and preset chips
`agent.add(...)` real tasks, and `ready` tasks show Apply/Dismiss (`resolve_task`/`remove_task`).
The panel orders **review card → queue (ready first) → quick edits → connect →
history**, and since it shares a tab group with the Inspector by default,
`+page.svelte` brings it forward once when a proposal lands (`editor.staged`
going null → set) — a review nobody can see is not a review. Under **Quick
edits**, five preset chips (`Remove silences` / `Assemble rough cut` / `Frame for the delivery`
/ `Caption the cut` (analyzes whatever is in the cut but not yet transcribed,
then captions it) / `Cut to the beat` — which
analyzes whatever is on the audio tracks first, then calls `snap_to_beats`, and says
"No cuts were near a beat" instead of claiming an alignment when the grid never reached
them) also run the matching local op and
resolve their task; the rest just enqueue for the agent. In the browser there is no agent, so
queued tasks correctly just wait. Above the queue sits the **review card** — the
panel's whole point, since an agent's task edits never touch the open cut. It renders
`editor.staged`: the agent's note, a headline (`4 changes · 2:00.0 → 1:40.0 (-20.0s)`),
the changes grouped by what they touch and tinted by the design system's own
`--diff-add`/`--diff-remove`/`--diff-shift`, each row clicking through to the moment
it describes. **Preview** swaps the editor onto the proposed timeline behind a
banner (`editor.previewingStaged`; any real edit or a fresh `load()` drops back to
the live cut, and `refreshTimeline` parks an incoming live update rather than
yanking the view) — so the proposal can be *watched*, not only read. Apply lands it
as one revision (confirming first when it went `stale`), Discard drops it. The
headline arithmetic is `src/lib/diff.ts`, the bun-tested TS mirror of
`TimelineDiff::headline`; the entries themselves are phrased by kerf-core, which is
why `revisionDiff` returns `null` in the browser instead of a second, divergent diff
engine. Below the queue, the **History** section renders
`editor.history` (the `Revision[]` edit log, attributed to user/agent/system) with one-click
`editor.revertTo(seq)`, and each row expands to *what* that revision changed
(`revision_diff`).

**Modals are modal.** `ExportDialog` / `SettingsDialog` / `UpdateDialog` use the
`trapFocus` action (`src/lib/modal.ts`: takes focus, wraps Tab, restores focus on
close), and `+page.svelte` makes the app behind them (and behind `VoiceoverDialog`,
which focuses itself) `inert` and returns early from its global key handler while
any is open — Space / Delete / J-K-L / ⌘Z would
otherwise edit the live project under a dialog; a file drop is ignored then too.
Every shortcut — bare keys and ⌘ chords alike — stands down inside any text input /
textarea / select / contenteditable. **Nothing unsaved is dropped silently**: once saved a project is a
SQLite file and every edit is committed as it happens, so only a never-saved,
non-empty project (`editor.hasUnsavedWork`) can be lost — New, Open, the window's
close request (`onWindowCloseRequested`) and the updater's *Restart now* all
confirm first through `confirmAction` (the dialog plugin's `ask`; the plugin
replaces `window.confirm` with an async one, so it cannot be used as a guard). A
render is `editor.exportRun` (progress + cancelling), not dialog state: closing
the export dialog mid-render leaves it running, the status bar shows it with a
**Stop**, and reopening the dialog shows the same bar. The `invoke` wrapper
rejects any argument holding NaN / Infinity (an emptied `<input type=number>`),
which JSON would turn into `null` and the backend into an opaque deserialize
error; the Inspector's number fields also snap back to the clip's value when
the entry is empty, negative or clamped, and keep a title's End after its Start.

Every toast is also a **notification log** entry (`src/lib/notifications.svelte.ts`,
a fourth runes singleton). Components import `toast` from *there* rather than from
`svelte-sonner` — a drop-in wrapper, so no call site changed — because a toast is
gone in four seconds, which is fine for "Clip copied" and useless for the model
download that failed with a reason worth reading. The title bar's bell opens
`NotificationCenter.svelte` (All / Unread / Problems, per-row read toggle, mark all
read, clear) and badges the unread count, red when anything unread actually failed.
Errors and warnings also linger longer on screen than sonner's default. The log is
deliberately *not* replayable — a toast's "Undo" action is dropped rather than kept,
since an hour later it would undo whatever the newest revision is, not the edit the
notice was about. It is also why the failure paths that used to reject into nothing
(`fetchSpeechModel`, `analyzeQueue`'s per-asset catch, the media bin's `runAnalysis`
calls) now report: a notice that is never raised cannot be recovered from a log.

**Keyboard shortcuts are an action registry, not key checks.** `src/lib/keymap.ts`
(pure, bun-tested) names every shortcut as an action — id, label, group, default
chord(s) — and `+page.svelte`'s one window handler asks `settings.actionFor(e)`
which action an event is and runs that id's entry in a `Record<ActionId, handler>`
(an action without a handler is a type error; a handler returns `false` when it
did not take the key). Nothing else spells a key: menus and tooltips read
`settings.shortcut(id)` / `withShortcut(label, id)`, and `keymap.test.ts` scans the
sources so a hand-written `(⌘Z)` or `shortcut: 'Del'` fails. Chords match what the
key *types* (`KeyboardEvent.key`, so AZERTY / Dvorak get their Z; a non-ASCII
character falls back to the physical key's US letter; Shift is dropped from
punctuation, since `+` is Shift+= on one layout and bare on another; a key that
would not read back from its stored spelling — `ß`, macOS's no-break space for ⌥Space
(which is `Space`) — is never recorded as something that silently vanishes). `Mod` in the
stored spelling is ⌘ on macOS and Ctrl elsewhere, and a chord means *exactly* its
modifiers — the old handler ignored extra Shift/Alt and took ⌘ or Ctrl everywhere;
`keymap.test.ts` holds the defaults against a copy of it (the differences: ⌘⇧S
stays Save as a second default, ⇧J-style accidents and Ctrl-on-Mac are gone).
**Only what the user changed is stored** (`Settings.keybindings`, opaque to Rust
like `theme`: `{ version, bindings: { id: [chord…] } }`, patch-written, `null` when
nothing is customised), so an untouched action follows the running build's defaults
and changing a default needs no migration; `KEYMAP_VERSION` / `MIGRATIONS` carry a
customisation across a rename or split, and `parseKeyOverrides` drops unknown ids
and unreadable chords and forgets anything equal to the defaults (a stored `[]` is
a deliberate unbind). `resolveBindings` keeps what fires unambiguous: a customised
chord beats another action's *default* (a later build's new default never steals a
key already in use), and any other collision goes to the earlier registry entry.
An action marked `repeat: false` (paste, duplicate, marker, ripple toggle, play /
pause, the file commands, …) acts once per press — the page swallows a held key's
auto-repeat — while stepping, zooming and undo keep repeating.
**Settings › Keyboard** (`KeyboardSettings.svelte`) is search, click-to-record
(Esc cancels, Backspace removes, Tab leaves), a conflict prompt naming the other
action with Swap / Unbind / Cancel (`applyRebind` never guesses; Cancel has the
focus, so a held Enter cannot answer it), per-row Reset — on offer whenever the
chords in force differ from the defaults, and asking the same question when another
action has since taken one (`applyReset`) — and Reset all, and a read-only list of the keys that are *not* rebindable (Esc
abandoning drags and closing menus and dialogs, Tab, Enter / Space on a focused
control, a widget's arrows, wheel and click modifiers). Focus goes back to a row
after every change: focus left on the page behind a modal stops Escape closing it.

**Settings** are their own runes singleton (`src/lib/settings.svelte.ts`) behind
the title bar's gear (⌘,): `SettingsDialog.svelte` is a section rail plus a
panel, so the next preference is a row in a list rather than new chrome. Five
sections: **Performance** — the CPU limit as three named budgets (Background /
Balanced / Full speed) over a slider, reading back "9 of 12 cores for Kerf · 3
left for everything else", because the complaint this answers arrives in those
terms and not in percentages — and **Speech**, one checkbox for whether the
analysis pass transcribes at all (`Settings.transcribe` →
`kerf_core::set_transcription_enabled`, a process-wide flag `analyze_asset_media_*`
reads to swap in the null transcriber): every import analyzes, and someone who
never wants a model fetched or minutes of inference spent needs the whole pass
to survive that, not to fail on the download. `TranscriptionStatus.enabled`
reports it, so the transcript tab's empty state can say "Speech-to-text is
off" and open Settings rather than offer a download. And **Preview**, one checkbox
for the safe-area guides over a vertical or square cut. And **Appearance**: the
three theme presets as chips, a name and a dark/light scheme, **Import… /
Export…** (a `.json` file through the dialog plugin and `read_text_file` /
`write_text_file`; the harness uses an `<input type=file>` and a download), then
every color token as a picker grouped by `COLOR_GROUPS`. A color edit applies
at once (`applyTheme`) and is written 300 ms later — a picker fires per pixel
of a drag — and while one is pending a view coming back from another write
leaves the theme alone, so the newer colors never flicker back. Changing any
color makes the theme `Custom` (`presetIdFor` compares colors, not the name).
And **Keyboard**, described above. The percentage is clamped by the engine, so the
view that comes *back* from `set_settings` is what renders, not the value asked
for; in the browser harness `api.ts` answers from localStorage
(`kerf.settings.*`, the layout, theme, workspaces and keybindings as JSON strings) over
`navigator.hardwareConcurrency` so the dialog is drivable under `bun run dev`.

The **update flow** is its own runes singleton (`src/lib/updater.svelte.ts`,
alongside `editor`/`ui`/`agent`): it runs a *silent* check at startup and every
6 h through `api.ts`'s `checkUpdate` / `installUpdate` / `relaunchApp`, and drives
`idle → checking → { current | available → downloading → ready } | error`. The
title bar's version chip turns into an amber "⬇ 0.18.0" button when something is
available and opens `UpdateDialog` (release notes, download progress, then
**Restart now** — which warns first when the project has unsaved changes); the
dialog auto-opens the first time a given version is seen (remembered under
`kerf.update.seen` in localStorage) so declining doesn't nag every launch. The
`Update` handle the plugin returns stays module-local in `api.ts`, which hands the
UI plain data — so the browser harness can fake the whole flow: `bun run dev`
with **`?update=1`** offers a synthetic 0.99.0 and simulates the download, making
the dialog explorable without a signed desktop build.
`bun run dev` with **`?staged=1`** seeds a synthetic agent proposal (a tightened
intro), which is how the whole review flow — card, preview swap, apply, discard — is
driven end-to-end without a desktop build.
`data.ts` keeps only the `STATUS_MAP`/`PRESETS` presentation bits —
all project data renders from the real backend.

`src/lib/api.ts` is the backend bridge: `inTauri()` decides between `invoke(...)` and a
**seeded in-memory sample with working local timeline ops**, so every edit/analysis/waveform
is explorable in a plain browser via `bun run dev` (frames return `null` there → Preview
keeps its placeholder; `getWaveformRange` answers from `src/lib/sample-waveform.ts`, a
deterministic stand-in shaped like the engine's pyramid read — stereo or mono per the
asset, zeros outside the media, the analysis's silences as a noise floor, and a clipped
stretch so the clipping colour is visible; `getFilmstrip` answers from
`src/lib/sample-filmstrip.ts` with a strip of the engine's *shape* — generated SVG
sheets labelled with each thumbnail's source time and index, black padding after the
last one — whose geometry comes from `src/lib/filmstrip-geometry.ts`, the faithful,
bun-tested mirror of the engine's interval ladder, thumbnail width, sheet layout, plan
and `Filmstrip::frame_at` / `locate` (the lookups a consumer makes against a real strip
too)). **Ripple in the harness is a port, not a
lookalike**: `src/lib/ripple.ts` is the *faithful*, bun-tested mirror of
`Timeline::ripple_from` (its test replays the Rust tests case for case, same clips and
numbers, so a rule changed in kerf-core has to change there or a test names it) and
`src/lib/multi-edit.ts` the same for `Timeline::move_clips` / `remove_clips` (same
checks, same messages). `api.ts` keeps the project's ripple flag in the harness state
(`getRippleMode` / `setRippleMode`; not an edit, no revision) and runs every local edit
that can change how much footage sits ahead of a clip — add, split, trim, speed, remove,
voiceover placement — through `devEdit`, `edit_timeline` in miniature (snapshot, edit,
`rippleFrom`), while the layout-deciding ones (move, reorder, ripple delete, cut range,
beat snap, paste) skip it as in the core; `moveClips` / `removeClips(ids, ripple?)`
reject as the backend does and leave nothing behind. `api-ripple.test.ts` drives it all.
This browser sample is a **dev harness only** — the desktop app always
uses the real backend and starts empty. State is two runes singletons: `src/lib/state.svelte.ts`
(`export const editor` — assets, timeline, analyses, selection, and the editing actions that
call the backend and apply the returned `Timeline`) and `src/lib/editor-ui.svelte.ts`
(`export const ui` — chrome state, playhead/zoom/playback, and `analyzeQueue` /
`runAnalysis` / `stopAnalysis`: a batch analyzes **one asset at a time** — each pass is
ffmpeg-bound, so running them together only makes each slower — and stopping drops
the whole rest of the queue). There is **no scripted demo phase machine**: the
editor chrome derives from real state — `MediaBin` shows a dropzone until `editor.assets` is
non-empty, `StatusBar` shows the selected asset's real fps/resolution/codec and timeline
duration (plus the analysis step, what is still queued behind it and a **Stop**), and
`Preview` shows the decoded frame or a "No media loaded" placeholder.
Each **bin row is the asset's specs**, not just its name: a real decoded frame
(`get_frame` 10% in, cached per asset across re-docks; the icon stays in the
browser harness, which has no decoder), the spec line
(`1920×1080 · 29.97 fps · h264 · stereo`), and badges for 360 / still / how many
clips already use it / analyzed. Its **context menu leads with the facts** — the
frame, rate, codec, audio, projection, the stitched lens pair, the use count, the
import date, then what analysis found (loudness, tempo, silence, shots,
transcript, or "not analyzed") — above the actions that need them: add at the
playhead / append, extract audio, remove silences (greyed out until silence has
been detected), analyze or stop, mark the asset 360 or flat, copy the path, show
in folder. The phrasing is `src/lib/media-info.ts`, pure and bun-tested, so the
row and the menu cannot drift apart; `MenuItem` grew `header` / `info` rows for
it, which the shared `ContextMenu` renders non-interactively.
**Dropping files onto the window imports them** (`+page.svelte` listens for Tauri's
`onDragDropEvent`, filters by `isMediaPath` — the same extension list the picker
filters by, so a dropped folder of mixed files doesn't answer with one error per
README — and runs the same `editor.importPaths` the picker resolves to), which is
what the bin's "Drop media to start" had been promising. `editor.error` renders as a
dismissible banner under the toolbar: it was recorded and never shown, so a `.kerf`
that would not open opened as silence.

## Conventions

- Keep types in sync across the boundary: `kerf-core` serde structs ↔ `frontend/src/lib/types.ts`.
  Field names are snake_case in the JSON on both Tauri and MCP.
- License is **PolyForm Noncommercial 1.0.0** (public repo). New files inherit it via
  `license.workspace = true`; don't add other license headers.
- Versions were pinned against the crates.io sparse index / npm; check there (not the
  blocked crates.io JSON API) before bumping.
- **Push a PR branch only when it is ready to test.** Every push to a non-draft PR
  rebuilds three installers (`pr-build.yml`, ~7–13 min each) on top of CI, and they
  share the account's few runners — macOS above all. Commit locally as often as you
  like; batch the work and push once, when there is something worth installing.
