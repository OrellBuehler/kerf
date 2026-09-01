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
  seeded from `KERF_CPU_PERCENT`, set at runtime by the app's settings). Gated =
  anything that reads a *whole file*: silence / scene / loudness detection, the
  PCM decode behind rhythm and in-process whisper, transcription, proxy, stitch,
  export. **Ungated** = anything that reads a *moment*: a scrubbed frame, the
  composited still, a clip's audio, the preview stream, a waveform, a contact
  sheet — the UI (and an agent *looking* at footage) must not wait out a render.
  The share becomes `-threads` / `-filter_threads` / `-filter_complex_threads`,
  written in at **spawn** time (`cpu::limit_args` / `limit_cmd`) rather than in
  the pure argument builders, so those keep describing exactly what ffmpeg is
  handed; `-threads` goes in twice because ffmpeg assigns it to whichever *file
  group* it sits in — at the front for the decoder, immediately before the last
  argument (the output sink) for the encoder. Plus below-normal scheduling
  priority (`cpu::background`, a creation flag on Windows / `nice` on unix),
  which is the half that actually keeps the desktop responsive. At **100%** none
  of the second half applies: no flags, no priority change, byte-identical
  invocations to the ones Kerf always issued.
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
  unit test asserted on the string. **Text overlays** (`Timeline.overlays`) are
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
  out. Frames are **paced to the requested fps against the wall clock**, which
  throttles ffmpeg through pipe backpressure instead of letting it race ahead and
  buffer the whole timeline, and each carries its timeline time so the webview can
  drop one the audio clock has already passed.
- `ffmpeg.rs` is the in-process **libav** backend (the `ffmpeg` feature): it supplies
  `probe` and, behind the extra `libav-render` feature, an **experimental** in-process
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

Two more optional features: `libav-render` (above) and `whisper` (in-process
`whisper-rs`; needs cmake, a C++ compiler and **libclang** at build time). Both are
off by default, so `--no-default-features` CI exercises neither — but **release
bundles are built `--features whisper`** (`release.yml`), because the bundled Windows
FFmpeg is a `--disable-whisper` build and a release with no in-process backend would
have no transcription at all there. `.github/actions/whisper-toolchain` installs and
*verifies* that toolchain (a missing libclang is not an error to whisper-rs-sys — it
silently falls back to its bundled Linux-generated bindings), and CI's `whisper` job
compiles the feature on all three runners, plus the x86_64 macOS cross-compile the
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

There is no Rust lint config beyond defaults; `cargo clippy --workspace --no-default-features` is fine.

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
  labels, `stale`, and its diff).
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
- `project.rs` — `Project` wraps a `rusqlite::Connection`. **Persistence shape:**
  `assets` and `analysis` are real tables (streams/analysis stored as JSON columns);
  the **entire timeline is a single JSON blob** in a one-row `timeline` table. All
  edits go through `edit_timeline(|tl| ...)` which loads → mutates → saves the blob.
  `Project::sample()` seeds an in-memory demo (two assets + analysis + a starter
  timeline + a sample task queue); it backs the kerf-core tests, but the app now
  launches with an **empty** `Project::open_in_memory()` — the user imports media or
  opens a `.kerf` file to populate it.
  `analyze_asset`, `frame_at` and `waveform` delegate to the engine; editing ops are
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
  that changes nothing writes no revision. The **agent task queue** is a real `tasks` table (one row per `Task`,
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
task queue, none of which moved. Because agent edits **stage**, "live in the GUI" now means the
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
description — `get_waveform`/`get_energy` buckets, `get_frame`/`preview_timeline`
widths — are clamped rather than trusted, the way `skim_asset` already clamps its
grid. `set_speech_model` is the write side of `transcription_status`
(`download_speech_model` only fills the cache; transcription uses whichever model
is *selected*, so downloading without selecting was a silent no-op) — it makes
both writes the GUI picker makes, though the picker itself only re-reads at
launch, so a model an agent selects shows there on the next start.
`smart_crop` frames each shot for the delivery frame (the server `instructions`
pair it with `set_delivery_format`, since reshaping to 9:16 otherwise keeps
whatever was in the middle). `generate_captions` / `clear_captions` caption the
cut; its `style` picks `lines` or `word_punch` and the `instructions` say to
prefer the latter for a vertical cut, since nothing in a tool list tells an
agent that the subtitle shape is not what social captions look like. They also
say to caption **last** and to re-run after any further
edit, because captions are placed in timeline time and a later trim moves the
words out from under them — which an agent has no way to infer from the tool
list.
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

Tauri v2 shell. `lib.rs::run()` is the entry (`main.rs` just calls it); it owns the
`Arc<Mutex<Project>>` (cloned into both the Tauri managed state and `mcp::serve`) and
registers a command per `Project` op — reads (`list_assets`,
`get_timeline`, `get_asset_metadata`), `import_asset` / `analyze_asset` (emits
`analysis-progress` per step), speech-to-text (`transcription_status`,
`set_speech_model`, `download_speech_model` → emits `model-progress`), every editing
op (`cut_clip`, `add_clip`, `split_clip`, `trim_clip` (optional `timeline_start` so a
left-edge trim keeps the right edge put, atomically), `reorder_clip`, `move_clip`,
`ripple_delete`, `cut_clip_range` (remove a **source-time** span from a clip and
ripple closed — the transcript-editing primitive), `add_track`, `remove_track`,
`set_track_duck`, `set_track_volume` / `set_track_pan`, `set_delivery_format` (the project's delivery frame; omit
width/height to clear it), `remove_clip`, `set_volume`, `set_fade`,
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
`.kerf` file; `KERF_CPU_PERCENT` wins at launch, a moved slider wins after)
and `agent_status` (the MCP endpoint plus how
many seconds ago an agent last spoke to it, or `null` if none ever has —
`mcp::LAST_AGENT_ACTIVITY`, stamped in `lock_agent` and in `get_info`, since
`initialize` is the one moment an agent is known to be there; a
streamable-HTTP client holds no connection between calls, so there is no socket
to report and the panel judges from the age instead of the green dot it used to
show unconditionally). **No command runs on the main thread** (a plain sync
command would freeze the window in Tauri v2): quick ops are
`#[tauri::command(async)]`, and every heavy one (ffmpeg decode / analysis /
export, disk-bound open/save) is an `async fn` that pushes its work onto the
blocking pool via the `blocking()` helper — resolving inputs under the shared
project lock and **releasing it before the slow part** (see `lock_user`; the
lock-free `Project::decode_*` statics exist for exactly this). The MCP server's
heavy tools (`analyze_asset`, `get_frame`, `skim_asset`, `preview_timeline`,
`get_waveform`/`get_energy`, `export`) follow the same shape with `lock_agent`.
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
`updater:default` + `process:allow-restart` + `opener:allow-open-url`. That last
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
  semantic vars onto them; the app is **dark-only** (`<html class="dark">` in `app.html`).
  That file is also the `tailwind.css` in `components.json`. Run
  `bunx shadcn-svelte add <name>` to add primitives.

The editor UI is implemented from the **Kerf design system** (claude.ai/design): a dark,
editor-grade workspace under `src/lib/components/editor/` — bespoke atoms (`Btn`,
`IconBtn`, `Badge`, `Icon`, `KerfMark`) plus `TitleBar`, `Toolbar`, `MediaBin`,
`Preview`, `Timeline`, `Inspector`, `AgentPanel`, `StatusBar`, composed by
`routes/+page.svelte`. The `Inspector` (right panel) is **mounted whether or not a
clip is selected** and toggled from the toolbar (`ui.inspectorOpen`): its Text
overlays section belongs to the timeline rather than to any one clip, so gating the
whole panel on a selection made titles and captions unreachable until you clicked a
clip. It edits the selected clip —
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
utilities. The **timeline is a bespoke NLE timeline** that renders **real `editor.timeline`
state** (ruler + tracks + clips positioned by `timeline_start`/duration at `ui.zoom`
px/sec + playhead), with scene markers / silence regions / **beat ticks** (the tempo grid
of audio-track clips, confidence-gated, hidden when beats land closer than 4px — from
`src/lib/beats.ts`, the TS mirror of the Rust beat math that the ruler, the drag
snapping and the browser harness's alignment all share, unit-tested with `bun test`)
mapped from `AssetAnalysis` and
real audio waveforms (`get_waveform`); the razor tool splits, Delete removes, Shift+Delete
ripple-deletes, clicks select/seek, and (pointer tool) **clips drag to reposition** — free
positioning with gaps, snapping to clip edges / playhead / 0 / beats, and **dropping onto another
same-kind track** (`move_clip`, via pointer events + `data-lane` hit-testing) — and
**edge-drag to trim** (6px `ew-resize` handles; clamped to source handles, neighbors and
a 0.05s minimum; left edges commit `trim_clip` with `timeline_start` so the right edge
stays put; stills extend freely since they loop). The ruler renders **in/out marks**
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
action column, plus a title-safe box; `ui.safeAreas`, toggled from the preview
context menu). The export dialog's "Source" resolution relabels to
**Project frame (WxH)** so the two surfaces cannot silently disagree. It also
leads with a **readiness panel**: "Ready for Instagram Reels · YouTube Shorts ·
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
landscape and the readiness panel warned about the shape the user had just chosen. `MediaBin`'s
**Transcript tab is an editing surface**: lines resolve to the clip carrying them,
click seeks, the playhead line highlights, and `×` cuts the sentence from the timeline
(`cut_clip_range`); cut lines render struck through. When it is *empty* it says which
of the five reasons applies (nothing selected / no backend / model not downloaded /
not analyzed / no speech) and offers the matching action — a model picker + download,
or Analyze — instead of a dead end. The **agent panel is a real MCP task
queue** (status · queue · history · add-task) — Kerf has no in-app chat; a connected
LLM claims tasks over MCP. The queue is `agent` state (`src/lib/agent.svelte.ts`, a third
runes singleton) backed by the `tasks` table over Tauri/MCP: the add-task box and preset chips
`agent.add(...)` real tasks, and `ready` tasks show Apply/Dismiss (`resolve_task`/`remove_task`).
Five preset chips (`Remove silences` / `Assemble rough cut` / `Frame for the delivery`
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

**Settings** are their own runes singleton (`src/lib/settings.svelte.ts`) behind
the title bar's gear (⌘,): `SettingsDialog.svelte` is a section rail plus a
panel, so the next preference is a row in a list rather than new chrome. Its one
section is **Performance** — the CPU limit as three named budgets (Background /
Balanced / Full speed) over a slider, reading back "9 of 12 cores for Kerf · 3
left for everything else", because the complaint this answers arrives in those
terms and not in percentages. The percentage is clamped by the engine, so the
view that comes *back* from `set_settings` is what renders, not the value asked
for; in the browser harness `api.ts` answers from localStorage over
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
keeps its placeholder). This browser sample is a **dev harness only** — the desktop app always
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
