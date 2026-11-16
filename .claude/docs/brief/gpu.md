# gpu — brief

Rules only. The full file `.claude/docs/gpu.md` is authoritative and holds the measurements and the story behind
each rule: grep it for the phrase in parentheses.

## Orientation

`crates/kerf-gpu/`: the wgpu compositor. It draws a kerf-core `RenderPlan` headless (`Gpu`, `Compositor`,
`Presenter`) and reads RGBA back, or keeps the frame on the GPU for the Preview panel's native surface (A2,
`kerf-app` `gpu_preview.rs`, see `app.md`). FFmpeg stays the decoder, the exporter and the reference: every
visual claim is "equal to FFmpeg's picture, sample for sample", and what is not provably equal is refused
(`Unsupported`) so FFmpeg draws it. Source: `src/` (`compositor.rs`, `frame_source.rs`, `cursor.rs`,
`router.rs`, `frame_cache.rs`, `y4m.rs`, `showinfo.rs`, `scaler.rs`, `sws.rs`, `text.rs`, `svg.rs`, `blend.rs`,
`grade.rs`, `convert.rs`, `delivery10.rs`, `placement.rs`, `roundtrip*.rs`, the `*_probe.rs` files,
`selftest.rs`, `measure.rs`, shaders in `shaders/*.wgsl`); kerf-core supplies the pure model (`render_plan.rs`,
`layer_geometry.rs`, `eq.rs`, `gblur.rs`, `simd.rs`, `mask_plane.rs`). Rule 0: the model is checked against the
real filter on every build, not assumed from a version number.

## Map of the full file (grep these leads)

- Intro / caps / decode / error scopes / blend summary (lines 1-33); `Presenting a frame (A2)`; `Which adapter`.
- `A1's frame source, the pure pieces`: `frame_cache`, `y4m`, `showinfo`, `router` / `ThrashGuard`.
- `FrameSource`: trust conditions (container, keyframe seeks, I/P only, late landing, self-contradiction),
  `Hint::Forward`, `Hint::Prefetch`, "The rules it holds to" (timeouts, release, no process under a lock),
  `self_test`, software decode.
- `FrameCursor` (export's half); `Playing` (A4); `CPU-side work and SIMD`.
- `What the parity harness forced`: composite in YUV, composite matrix, BT.709 label, `eq`, scaler (tables,
  vertical arithmetic, names, probes, limits), plain `yuv444p` resize route, opacity round trip, letterbox /
  `pad`, odd chroma tail, limited-range decode, integer geometry, folded fit.
- `Text: FFmpeg rasterises`; `Animated SVG titles`; `The runtime self-test`.
- `Fades, dips, dissolves, slides and pushes` (+ full-range rules, export frames).
- `A moving zoom last, masks and effects`; `Moving stills (Ken Burns)`; live behaviours.
- `The placement`, `PlacedFormat`, probes of a placed layer, `Which frame is drawn`; `Masks`; `Rotation`.
- `The display conversion`; `The translucent round trip`; `Odd sizes of a translucent layer`.
- `Keyed opacity`; `Keyed colour`; `Effects` (grayscale / invert / sharpen, blur, vignette, chroma key).
- `A letterbox's padded frame`; `The blurred fill`; `Blend modes` (+ other-matrix blend, long runs); `The clip's
  grade`.
- `Layers of another matrix than the bottom's` (+ scaled cascade, fill cascade, probes).
- Test harnesses: `tests/motion_parity.rs`, `checker_z`, `checker_a_static_jpeg`, `tests/parity.rs`, the ledger,
  `fate_corpus`, `checker_mx_*`; `10-bit deliveries`; the independent parity review, per-build media, `What CI runs`.

## Overview, errors, caps (lines 1-33)

- Only Vulkan / Metal / DX12 wgpu backends; nothing may depend on an optional wgpu feature, because CI is a
  software adapter (lavapipe, WARP). (`nothing may depend on an optional wgpu feature`)
- A GPU failure is an error, never a panic: all wgpu work runs in error scopes (`Gpu::guarded`);
  `Compositor::new` / `composite` return `Result`; no adapter is an `Err`. (`A GPU failure is an`)
- A lost device makes every later call `GpuError::DeviceLost`; the OWNER builds a new `Gpu` + `Compositor`,
  never recreated behind the caller. (`behind the caller's back`)
- `Compositor::caps()` is what the plan is judged by. Each FFmpeg-dependent cap is set only where its probe
  measured FFmpeg equal to the model (scaler, `ScalerKinds`, matrix conversion, scaled conversion, placement +
  behaviors, svg, still motion, grade, blend, 10-bit, blur); else the plan refuses and FFmpeg draws.
  (`ScalerKinds`)
- `translucent_approximate` is set unless the round trip probe is `Match`; `translucent_odd` is positive (round
  trip matched AND an odd-size model found), else an odd-sided translucent layer is `Unsupported::Geometry`.
  (`Unsupported::Geometry`)
- A blend mode is drawn only upright / quarter-turned, own size or fit-resized, opaque or translucent, not
  letterboxed, over an even frame, where `blend::kernels()` matches; else `Unsupported::BlendMode`.
  (`A blend mode is drawn`)
- `source::decode_layer` pipes one frame as `yuv4mpegpipe` 4:2:0 through kerf-core's `ffmpeg_command()` (no
  Windows console flash) with the still graph's `-ss`. (`FFmpeg stays the decoder`)
- The decode states its own size (y4m header) and it is compared with the probe's; a mismatch is `Unsupported`
  (EXIF-rotated JPEG probes 480x270, decodes 270x480). (`The decode states its own size`)
- A `pix_fmt` not on the known-opaque allow-list is refused (the plan too); no recorded `pix_fmt` costs a second
  `yuva420p` decode, refused if anything is transparent. (`known-opaque`)
- A child is killed after 30 s or when it writes more than the probed size; reader threads are detached and a
  killed child is NEVER waited for through its pipes (a grandchild may hold them); an exited one gets 2 s for
  stdout. (`source::run_child_logged`)
- `-ss` past the last frame (zero frames) is `Ok(None)`: FFmpeg draws nothing there and so does the compositor.
  (`zero frames`)
- Layers decode in parallel with `budget / layers` threads each (one for a proxy layer) via
  `limit_frame_args(args, share)`. (`limit_frame_args(args, share)`)

## Presenting (A2) and adapter

- `render_plan_texture_with` keeps the composite on the GPU (`RenderedFrame`); read back it must equal
  `render_plan` bit for bit. (`**Presenting a frame (A2).**`)
- `Gpu::new_for_surface` makes the surface first and asks the adapter to be compatible (hybrid GPUs); an
  uncreatable surface is `GpuError::Surface`, never a panic. (`the surface is made first`)
- `Presenter`: non-sRGB format if one exists (encoded values go out as written; an sRGB target decodes in the
  shader), `Opaque` alpha (the webview is transparent, never the surface). (`a **non-sRGB** format`)
- Acquire `Outdated` / `Lost` is reconfigured once and retried; `Timeout` / `Occluded` are `GpuError::Surface`
  for the caller to skip. (`reconfigured once`)
- `Presenter::skip_vsync(on)`: reconfigures only on change and not at all without `Immediate` / `Mailbox`;
  surface contents are undefined after, so change it just before the frame presented. (`not guaranteed`)
- The app's request mailbox remembers ONE pump's newest number; alternating pumps are ordered by arrival.
  (`remembering **one** pump's newest number`)
- `GpuOptions::refuse_cpu` turns a CPU rasteriser into `GpuError::NoAdapter` (names `KERF_GPU_ADAPTER`);
  `for_preview` refuses CPU unless `software` / `auto`; `Default` / `for_tests()` stay lavapipe so parity and CI
  run. A software adapter is no win for live preview; nothing run on WARP. (`**Which adapter.**`)

## Frame cache, y4m, showinfo, router (lines 70-153)

- `FrameCache`: byte-capped LRU of `Arc<YuvFrame>` keyed `(SourceId, pts ticks)`; a PINNED frame is never
  evicted (cap is soft); the frame just inserted is never its own victim; `insert_cold` never pushes out a
  requested frame. (`**pinning**`)
- Each frame states `covers_from` ("no frame of this file has a pts in covers_from..pts"): lookups hit only when
  the frame PROVES it is first at or after `t`; a hole left by eviction is a miss. `mark_end` makes later times
  `PastEnd`. A run's first frame covers from its seek tick, which the caller clamps to one frame interval before
  it. (`which **the caller clamps to one frame interval before it**`)
- One picture per pts: inserting under a held key keeps the first; `conflicts_with` says beforehand whether the
  picture differs (run fails, file goes one-shot); never an assertion. (`says beforehand`)
- `Y4mReader`: header size is checked against the probe's BEFORE any plane is allocated; only `C420` / `420jpeg`
  / `420mpeg2` / `420paldv` / no tag (`420p10` also starts with "420"); after any error it stays failed.
  (`before any plane is allocated`)
- `ShowinfoParser`: the spawn must use `showinfo=checksum=0` (plain checksums every frame) and `-fps_mode
  passthrough` via `fps_mode_flag()` (`-vsync` before FFmpeg 5.1).
  (`which the spawn must spell with the engine's`)
- A frame is only a line with `[Parsed_showinfo_N @ 0x...] n:<n> pts:<pts>`; the prefix is required (also for
  `config in time_base`) so a title like `] n:1 pts:0` cannot poison it; strip ANSI and call `plain_log_env`.
  (`cannot poison it`)
- `n` must count 0, 1, 2...; pts must be strictly ascending (`OutOfOrder` -> file falls back to FFmpeg) because
  the cache keys by pts and a repeat would be served for another picture. `FrameCursor` uses
  `allowing_repeats()` (equal ok, earlier rejected). (`pts must be strictly ascending`)
- `time_base` is re-read on every occurrence (a change is an error); `pts:NOPTS` / non-plain-integer is an
  error; `line_lossy` takes bytes (non-UTF-8 names). (`is re-read`)
- Router: reuse an idle run behind the target by at most `effective_window` frames (24 proxy / 96 original, held
  to half the cache's capacity); at most 3 runs a file, 6 in all; a request behind every run starts
  `BACKWARD_LEAD` (15) frames early. (`REUSE_WINDOW_PROXY`)
- `Exact` NEVER evicts (reuse or one-shot); `Prefetch` only takes a spare slot; a busy run is neither reused nor
  evicted. (`**`Exact` never evicts**`)
- `ThrashGuard` counts only routes that destroy a run somebody may want back, only for `Forward`; time-weighted
  (`THRASH_BUSY_SECS` 0.75 s of the last second -> `Err(Busy)`); failed / empty runs still count
  (`FAILED_RUN_COST`); the `Ticket` is `#[must_use]` and must reach `finished`. A refusal renders that frame
  through FFmpeg's stream. Time is passed in (tests need no clock). (`#[must_use]`)

## FrameSource (lines 155-332)

- One per process; every call blocks (blocking pool, never under the project lock); layers asking for the same
  frame of one file share one decode. (`every call blocking`)
- `run_args` must end the chain in `setpts=N/TB` AFTER `showinfo` printed the file's own pts (a repeated
  timestamp otherwise makes the y4m muxer log `Non-monotonic DTS` mid-line, losing the prefix).
  (`the chain ends in`)
- Only `Pick::AtOrAfter` is served (keyed by `kerf_core::seek_ticks(t, tb)`); `Before` / `Fps` are a cursor's
  and `Unsupported`; a still is one cached one-shot. (`Only `Pick::AtOrAfter` is served`)
- Guarantee: a returned frame equals the one-shot decode (`decode_layer`, `-ss t`) byte for byte; `-ss t` is NOT
  "the frame at t" for every file, so runs answer only where provably equal and everything else goes ONE-SHOT
  PER FILE from the first thing that shows it (`State::distrusted`): the cost is speed, never a frame.
  (`a frame it returns is the one-shot decode's`)
- Trust: container must be MP4 / MOV / Matroska / WebM (positive allow-list, `source_is_indexed_container`; TS
  and AVI (guessed pts) go one-shot; a still passes); `FrameSource::cursor` refuses the same.
  (`**The container** is MP4 / MOV`)
- Trust: seeks must be decodes from a keyframe (`source_seek_points_are_keyframes`, `engine/source_probe.rs`,
  `-skip_frame nokey`, every sync sample must decode as an `I` picture; x264 `intra-refresh` fails it). It looks
  only at the file's HEAD (four sync samples): two joined encodes are not caught.
  (`**A seek into the file must be a decode from a keyframe**`)
- Trust: only I and P pictures; the first B frame a run reads ends it and marks the file (every open-GOP file
  and, deliberately, closed-GOP B files: decode order is unavailable). (`**Only I and P pictures**`)
- Trust: a seek landing one frame interval or more late marks the file; the interval is `late_ticks` ROUNDED UP,
  not `frame_ticks`' (ms time base: 30 fps interval 33.33 ticks, frames 33 / 34 apart); a late frame claims
  nothing before itself; the stated price is a landing one short frame late from a seek on a frame's own tick.
  (`**A seek that lands late**`)
- Trust: a file that contradicts itself (time base change, unreadable / unpairable timestamps, other size /
  format than probed, differing picture at a cached pts) ends the run and marks the file (no respawn per
  request). (`**A file that contradicts itself**`)
- `Hint::Forward` is what the app's drag asks with; NOT for a plan with a reversed layer (backward frames would
  restart every frame and trip the thrash guard). (`**`Hint::Forward`** is also`)
- `Hint::Prefetch` returns `Ok(None)`, waits for nothing, draws nothing, parks a run at the frame; takes only a
  SPARE slot; leaves unproven files alone (no one-shot for a frame nobody asked for); refused by
  `render_plan_with` / `_texture_with`. (`**`Hint::Prefetch`** is a request`)
- A clean end calls `mark_end`; a clean run with no frame is a seek past the end; a non-zero exit with no frame
  is a FAILED START, never an end (error carries ffmpeg's stderr tail). (`a **failed start**`)
- A run reading past its target unproven is given up on and re-routed; after two starts it decodes one-shot. A
  request joining a starting run before the time base is known (`joined_blind`) re-routes if that run ends
  empty. (`joined_blind`)
- Nothing waits forever, nothing under the lock waits on a process: reaper kills runs silent for
  `first_frame_timeout` (30 s) / `frame_timeout` (15 s) or idle `idle_kill` (20 s); silence is measured FROM
  WHEN THE RUN IS WANTED (`Run::want_to`); `Scrub` / `Forward` give up after 5 s, `Exact` after 30 s.
  (`**Nothing waits forever, and nothing under the lock waits on a process**`)
- `release` / `release_all` / `Drop` kill every child; a request waiting on a released file FAILS
  (`GpuError::Decode`) instead of re-starting a run; `Shared::releases` is read BEFORE anything else in
  `frame()`, probes included. (`reads it **before anything else, the probes included**`)
- A run that closes stdout and does not exit is killed after `EXIT_GRACE` (5 s) by a `try_wait` loop; never
  `wait()` under the child's lock. `spawn` enters the run with no child, unlocks for `Command::spawn`, re-locks;
  `stop` hands the kill to a thread; one-shots capped at 2. (`started or waited for with the state lock held`)
- `self_test` on first use (a 30000/1001 `mpeg4` clip from ffmpeg's own encoder): if it fails, or
  `KERF_FRAME_SOURCE=oneshot`, or an asset never recorded its pixel format, the process decodes with
  `decode_layer`. (`**The path can be off**`)
- Runs decode in SOFTWARE as the one-shot does (`hwaccel: None`; `decode_args` never carries `-hwaccel`): a
  hardware decoder differed up to 24 levels. `CursorConfig::hwaccel` defaults `None`.
  (`**Runs decode in software, as the one-shot does**`)
- `tests/frame_source_fake.rs` (unix, own binary, `KERF_FFMPEG` wrapper) holds hang / die / never-exits /
  differing-picture behaviour; `tests/frame_source.rs` holds each trust gate against real files.
  (`holds a run that hangs`)

## FrameCursor (the export's half, lines 334-383)

- One clip's frames in output order from an EXCLUSIVE run (no router, no cache). The seek is spelled as the
  export does: `FpsPick::seek_arg()` / `kerf_core::export_seek_arg` (shortest `f64` text, FFmpeg truncates to
  microseconds, no `-ss` at the head), NOT the still's rounding `{:.6}` (starts a frame late).
  (`kerf_core::export_seek_arg`)
- `FpsPick::lead` (late-starting source): a window inside it opens with no seek and its first slot is clip start
  + `picture_delay`; earlier output frames pick nothing (layer not drawn, as in the export).
  (`**`FpsPick::lead`**`)
- `cursor.pick` answers `Pick::select` over the whole file only where `-ss` lands on the frame the timestamps
  say; otherwise (late keyframe landing) it answers over the frames it was given, not proven against the export;
  `FrameSource::cursor` refuses files a router run marked. (`where `-ss` lands on the frame`)
- Keeps every timestamp but only pixels a later pick can show; reversed clips hold their whole window, capped by
  `window_cap_bytes` (512 MiB; past it `Unsupported`). FORWARD ONLY (a dropped frame's pick is an error).
  (`**Forward only**`)
- Watchdog kills reads past `first_frame_timeout` / `frame_timeout`; unrecognised size/layout is `Unsupported`
  not a failed decode; `Drop` kills the run. `FrameSource::cursor` opens only where runs are trusted (self-test
  ok, recorded opaque pixel format, MP4/Matroska or still, keyframe seek points, not marked).
  (`opens one where runs are trusted`)

## Playing (A4, lines 385-408)

- Playing is a loop over a `PlayCut` and CURSORS, not the router: a playing frame must be the export's `fps`
  pick, which only a cursor reads. (`**Playing** (A4) is a loop over`)
- `MotionFrames::frames` reads layers side by side; `render_motion_texture` is `render_plan_texture_with` over
  it; `warm_text` rasterises titles ahead (a caption's ffmpeg costs a prefetch thread, not the frame).
  (`Compositor::warm_text(plan, size)`)
- `MotionFrames::feed()` is a `CursorFeed`: `open(layer, needed_at)` applies `FrameSource::check_cursor`'s
  gates, starts ffmpeg ahead (at most 6 waiting, 10 s each; the clip wanted LAST is dropped for a nearer one,
  `Opened::Full` otherwise); call it from a thread that is not drawing. A cursor opens at the clip's own seek,
  so a session slices from the playhead (`PlayCut`). (`**`MotionFrames::feed()` is a `CursorFeed`**`)
- `tests/playback.rs` holds played frames to the export plane for plane (0 levels except scaled layers); slides
  / a dissolve out of a still are held to the SLICE's export (pixel-truncated travel, 25 fps PNG stream).
  (`tests/playback.rs`)

## CPU-side work and SIMD (lines 410-458)

- Per-pixel work is the GPU's; CPU per-sample work is vectorised or not done twice (`memcpy` copies stay).
  Kernels live in kerf-core (`simd.rs`, `mask_plane.rs`, `mask_plane/x86.rs`). (`**CPU-side work and SIMD.**`)
- Every kernel keeps its scalar function (`position_ne_scalar`, `plane_bounds_scalar`,
  `mask_alpha_plane_scalar`): fallback, `KERF_SIMD=scalar`, oracle; a vector path must return EXACTLY what
  scalar returns (0 differing samples), since these feed exact-parity text and mask paths. All CPU samples are
  8-bit. (`A vector path returns exactly what the scalar one`)
- `KERF_SIMD` caps the path (`scalar` / `off` / `none` / `0`, `sse2` / `neon`, `avx2`; other values log one
  warning); `simd::with_path` is the tests' hook. (`**Detection, cap, fallback**`)
- Ellipse mask rows: `f64` lanes in the scalar's operation order, NO fused multiply-add, same truncating
  `cvttpd`; the band's `hypot` and trailing columns go through scalar `Params::px`.
  (`**The mask's alpha plane**`)
- `sws::table_aligned` memo key must be ALL of `(scaler, align, src, dst, one)` (a smaller key hands the C
  build's table to a SIMD build); lock only to look up / keep, never while building or cloning; tables over the
  entry cap are not cloned. (`**Not vector code**`)
- Codegen pitfall: in a `#[target_feature]` kernel `copy_from_slice` / iterator adapters are CALLS in release;
  use raw pointers after one bounds check. (`**Pitfall (codegen)**`)
- Open end: NEON is written but has never run (byte scans only; mask rows take the scalar path there).
  (`**NEON is written but has never run**`)

## What the parity harness forced: colour, scaler, geometry (lines 460-737)

- Composite in YUV, convert once: FFmpeg's `overlay` blends encoded planes and the still converts the RESULT to
  RGB (an RGB-first design mis-draws out-of-gamut-but-legal footage); chroma replicated 2x2 at the final
  conversion. (`**Composite in YUV, convert once.**`)
- The composite's matrix is FFmpeg's, PROBED (`composite_color_policy`, once per process; the plan takes it as
  input): 6.1 reads the result as BT.601 whatever the tags, 9.0 takes the BOTTOM layer's tag and converts other
  layers by a scaler stage. Stacks of one matrix are drawn, mixed ones only where the conversion is reproduced
  (`matrix_convert`). (`**The composite's matrix is the FFmpeg's, probed`)
- Known consequence: an export is untagged `yuv420p` that players show as BT.709, so the FFmpeg preview already
  disagrees with the file on saturated colour; fixing that is a decision for when the GPU path replaces the
  FFmpeg preview. (`Fixing that is a decision for when`)
- Settled: untagged HD beside footage naming a matrix is labelled BT.709 by the export graph (`engine-cli.md`
  "The BT.709 label"; plan mirrors it), so it is one matrix; untagged HD beside `smpte170m` is refused on 9.0.2;
  a lone untagged HD clip is read as BT.601; decoder-default tags (PNG `gbr`, JPEG `bt470bg`, WebP, `mjpeg`,
  `amv`, `speedhq`, `jpegls`) are not matrices and label nothing. (`**One graph is settled**`)
- Accepted difference: the playback stream (`SpanPlan::handover`) labels over its own slice, the Motion plan
  over the cut's graph; when the slice dropped the matrix-naming clip translucent HD layers, coloured text and a
  dip to white differ by levels. (`**Accepted difference**`)
- `PlanStream::matrix` (probed `color_space`) is used for taking a translucent layer out of YUV and for choosing
  the composite's matrix. (`PlanStream::matrix`)
- `eq` is a YUV operation: kerf-core's `eq.rs` (`kerf_gpu::eq`) builds vf_eq's per-plane tables (integer
  `process_c` at gamma 1, `pow` table otherwise, `float` clamp, truncation), byte for byte; "temperature" is a
  power function on the chroma planes. (`is a YUV operation`)
- Scaler: swscale's own filter TABLE is ported, never a bicubic formula (quirks: window start truncated toward
  zero, edge folding, near-zero tap trimming, error-diffused weights, 15-bit horizontal intermediate); planes
  scale independently, rounded to 8 bits between stages (fit and transform scales are two scalers in cascade).
  (`**Scaler: swscale's own, ported, and measured**`)
- What the scaler is depends on the BUILD, not the graph: the compositor draws the one this FFmpeg runs
  (`scaler::kernels()`, measured once, real `scale`; `exact_scaler`). (`depends on the build, not the graph`)
- SIMD builds round filter sizes up (`filterAlign`) and KEEP THE ORIGINAL TAPS in the padding, so weights differ
  from the C build's (`sws::table_aligned`; zero padding was a level off). (`**The tables.**`)
- Vertical arithmetic: C `yuv2planeX_8_c` vs x86 `yuv2yuvX` (every tap truncates, different start constant)
  EXCEPT the last two luma rows and last 4:2:0 chroma row, which `swscale.c` hands to the C kernel (`dstY >=
  dstH - 2`); single-tap axis is `yuv2plane1`. (`**The vertical arithmetic.**`)
- An unflagged `scale` is bicubic since FFmpeg 5.0 but BILINEAR before it (4.4 defaults `flags=bilinear` in
  complex graphs); the probe's `Model { kernel, default, named }` tells. Explicit flags win (`blur_fill`
  thumbnail `flags=bilinear`; a delivery's `scaler` names its own).
  (`**The algorithm an unflagged `scale` runs.**`)
- Delivery scaler names (`bicubic`, `bilinear`, `lanczos`, `neighbor`, `spline`; engine `SCALERS`,
  `:flags=<name>` on every fit / zoom `scale`) are ported (`sws::Scaler`); Lanczos / spline coefficients are
  `double` arithmetic, the MACHINE's, so the probe proves each name separately (`Model::named`,
  `scaler::NAMED`); a name whose chains differ is left out and only frames naming it are FFmpeg's.
  (`**The names a delivery can give its scaler**`)
- The base scaler and each name are separate questions with separate backoff: a failed / hung name child leaves
  only THAT name out and `kernels()` stays `Match`; a failed base child makes everything `Unknown`.
  (`**The base scaler`)
- Only what a caller asks for is measured (`kernels_for(names)`; `prepare` = base + the delivery's name;
  `prepare_timeline` and the self-test = base only). Single flight (`scaler::Probe`, `FLIGHT_BOUND` 25 s); base
  stored as soon as its ten children are in (`on_base`), a hung name never delays it; names failed once retry on
  a detached thread (backoff 5 s to 5 min); differing names are out for good. (`**Single flight**`)
- Probe cost: a first `kernels()` is 20 ffmpeg children at once, each `PROBE_TIMEOUT` 20 s, each given
  `limit_frame_args(share = 20)`. (`**The probe's cost**`)
- Lanczos needs a second proof, the function itself (`scaler::sin_graph`: a `geq` writes `sin` digits compared
  bit for bit with `f64::sin`); a different libm is `Differs` for Lanczos alone.
  (`**Lanczos needs a second proof, the function itself**`)
- Shrink limits per name: swscale cascades a second scaler past 256 taps, so `max_shrink_for` is 42 (Lanczos),
  18 (spline), 63 otherwise; past it `Unsupported::Shrink`. A shrink over 63:1 (`MAX_SHRINK`) is refused.
  (`**A shrink has a limit per name**`)
- A still whose fit picture FFmpeg makes (`FitCopy`) is refused when the delivery names any of the four
  non-default scalers; a moving still's route is drawn under a name only where `still_probe::verdict_route`
  measured it. (`scaler_named_other`)
- `Model::ASSUMED` (C, bicubic) applies while the probe is `Unknown` / `Differs`; then `caps.scaler_approximate`
  is set and the plan refuses every layer a scale resizes (`Unsupported::ScalerApproximate`); things that would
  amplify the level (grade on a resized layer, blurred fill) are refused (`caps.exact_scaler`). The arm64
  `Aligned` model is built from source and NOT measured. (`**not measured**`)
- Scaling happens on the decoded 8-bit 4:2:0, so resizing any other format (even 1.05-1.5x shrinks: FFmpeg
  scales chroma from the full-resolution plane) is refused by the plan.
  (`Scaling happens on the decoded 8-bit 4:2:0`)
- Plain `yuv444p` resize route (`resize_444`, `resize444.rs`, `RenderPlan::resize_444_refusal`,
  `PlanLayer::wide_chroma`): ONE swscale call takes `yuv444p W x H` to `yuv420p`, each chroma plane from the
  full-resolution picture. The decode must hand over `yuv444p` planes (no `scale=out_range=tv`);
  `DecodeFormat::Yuv444p8` is part of `SourceId`; the plan's `RenderPlan::for_decode(caps, size)` stamps
  `wide_chroma` and a harness that decodes for itself MUST call it. (`**A plain `yuv444p` clip the fit resizes`)
- 444 route refused: any cap but `resize_444`, odd window / result / crop / picture, a second scale, colour
  correction, grade, effect, mask, rotation, opacity, blend, fade / dissolve / slide, a keyed number besides
  zoom and position, blurred fill, HDR / 360 / full-range / still, a delivery naming a scaler besides `bicubic`,
  chroma shrink past 31.5:1, another matrix than the composite's where negotiated; a pan alone is FFmpeg's. A
  moving zoom / position is the placement's and drawn. (`**Refused** (`FormatResize` as before`)
- A static `crop` window the fit does not resize: FFmpeg crops the 4:4:4 picture then converts the WINDOW (edge
  chroma clamped); `yuv444p` windows are drawn from planes, 8-bit 4:2:0 / gray / nv12 stay on the decode, every
  other format is refused (`RenderPlan::format_window`). (`**A window the fit does not resize**`)
- The 444 probe (`resize444::resize_444()`, 45 s deadline, 30 s backoff, `Unknown` never cached) needs
  `scaler::kernels()` `Match` and runs the export's own graph; pin with `with_resize_444`. It deliberately has
  NO frame in the runtime self-test (the probe measures the graph itself). (`**The probe**
  (`resize444::resize_444()`)
- Opacity below 1 is FFmpeg's RGB round trip (`yuva420p -> argb -> yuva420p`): out of YUV with the LAYER's
  matrix, back with the COMPOSITE's (`Rgb2Yuv`); alpha plane `round(lrint(255 * op) * 256 / 255)`; odd-sized
  layers go through `roundtrip_probe::odd_translucency`; a translucent layer whose matrix is unknown is refused.
  (`**Opacity below 1 is FFmpeg's RGB round trip**`)
- A quarter turn of a translucent layer is refused (`Unsupported::Rotation`; the graph converts to 4:2:0 AFTER
  the turn, the round trip here comes before): keyed opacity and a black fade on an OPAQUE layer commute with
  the turn and are drawn. (`**A quarter turn of a translucent layer is refused**`)
- A letterboxed layer is the whole frame: `pad` emits a full canvas, so an identity clip not filling the frame
  covers what is below and `eq` grades the bars (`LayerGeometry.matte`).
  (`**A letterboxed layer is the whole frame.**`)
- `pad` always drops the last odd COLUMN and the last odd ROW only when the `scale` before it resampled; AT THE
  ORIGIN whether it drops the row depends on the decoder and frame writability, so an odd-height picture at the
  origin not resampled by `scale` is refused (`Unsupported::Geometry`, every build).
  (`Not at the frame's origin, though`)
- Off the origin it depends on the graph (`RenderPlan::odd_pad`): the first filter in `pad`'s format-list group
  that restricts the format (`format=yuva420p`, the overlay's input) decides what `scale` converts to. A Still
  plan (`scale,pad,setsar` into an overlay taking only `yuva420p`) drops the odd row at any offset; colour /
  grade / effect / fade / mask / keyed / HDR in the group is refused (the plan does not follow the negotiation);
  a Motion plan's chain ends in `format=yuv420p`: drawn for `yuv420p`, refused for other formats and alpha-plane
  chains. (`**Off the origin it depends on the graph, not the decoder**`)
- An odd layer's last chroma block reaches one pixel past it: a second chroma-only pass writes that pixel's U
  and V. (`**An odd layer's last chroma block reaches one pixel past it**`)
- The decode asks for limited range explicitly (`scale=out_range=tv`): FFmpeg 6.1 converts a full-range JPEG, 9
  hands raw bytes back. (`**The decode asks for limited range explicitly**`)
- Geometry is FFmpeg's integer geometry (`layer_geometry.rs`, `kerf_gpu::geometry`): first `crop` `lrint` to the
  NATIVE chroma grid, fit `av_rescale`, `pad` / `overlay` round down to even, `rotate` half-up; only quarter /
  half / three-quarter turns land on whole samples and are drawn. (`**Geometry is FFmpeg's integer geometry**`)
- A zoom in under Contain, and a Cover clip that zooms or pans over a hanging cover, FOLD into one `scale` (fit
  then zoom is no longer FFmpeg's graph): refused (`Unsupported::FoldedFit`); zooms out and scale 1 unchanged.
  (`**That geometry is fit, then zoom, and FFmpeg's graph no longer is for a zoom in**`)

## Text (lines 739-815)

- FFmpeg rasterises, the compositor only blends: `drawtext` layout (shaping, kerning, hinting, quarter-pixel
  phase) is never redone. `drawtext` blends each glyph bitmap IN TURN (border first, then fills), so one mask of
  their sum is wrong where bitmaps meet; `text::raster` recovers the bitmap SEQUENCE from FFmpeg (`text/seq.rs`)
  and the compositor blends it in order. (`**`drawtext` blends each glyph's bitmap in turn**`)
- Raster steps: `measure` (debug log of a multi-line `drawtext`; lines told apart by what they say, groups cut
  at each `Line: 0`; Legacy 4.4.2 reads box widths in ONE run, spilling long graphs), `dictionary` (each unit
  alone at four quarter-pixel phases, drawn in an INVISIBLE box with a wide margin because a shaped `drawtext`
  clips bitmaps to the box), placement (`x64`, phase `((x64+pen64) >> 4) & 3`, baseline phases tried likeliest
  first), and PROOF: the sequence blended onto black must equal FFmpeg's picture sample for sample, else
  `GpuError::Unsupported`. (`(4) **proof**`)
- `text::model()` probes layout (Shaped via `width64`, Legacy via `text_w`, else `None`) and colour conversion
  (`Build { layout, colour }`); `prepare(plan)` asks it ahead; `caps.text` only for a known build, else every
  text frame is FFmpeg's. 4.4.2 converts colour in fixed point (`RGB_TO_Y_CCIR`, BT.601 whatever the tag; a
  level off): the probe compares both models. (`**and how it converts the text's colour**`)
- The blend is `drawutils.c`'s u32 integer arithmetic `((K - a) * d + a * s) >> 24`; colours are parsed as
  `av_parse_color` does (`parse_color`), never asked of FFmpeg; `s` is in the composite's matrix.
  (`The blend is `drawutils.c`'s integer arithmetic`)
- Rasterising: `RASTER_TIMEOUT` 5 s; a run is waited 5 s + 25 ms per instance (20 s max), else refusal.
  `TextAtlas` caches by (text, font file length + mtime, ffmpeg path), 256 states / 48 MiB; `text::raster`
  remembers a REFUSAL for 30 s under the atlas key, asked BEFORE the first ffmpeg (scrubbing Arial Narrow /
  script / RTL spawns nothing). Drag frames' unseen title states go to a second atlas (4 states / 16 MiB,
  `get_drafted`). (`RASTER_TIMEOUT`)
- `prepare_text` blocks on processes for a first-seen state, so the caller must hold no lock the UI needs
  (neither project lock nor render lock, `app.md`). A frame with no layer is drawn in RGB (FFmpeg's black base
  is not YUV); `composite_yuv` refuses such a plan. (`**A frame with no layer is drawn in RGB**`)
- Nothing is drawn that is not proven (`**Nothing is drawn that is not proven**`): `sum_fallbacks()` must stay
  0; the plan refuses what it can tell (`Unsupported::TextSequence`: tab, over 160 characters
  `MAX_SEQUENCE_CHARS`, too wide `MAX_MEASURE_SIDE`) and every text without `caps.text` (`Unsupported::Text`);
  the render refuses failed proofs, timeouts, and faces touching the dictionary cell's edge (script faces, Arial
  Narrow Latin on shaped builds, RTL on all three builds). Border and print under 12 px are drawn now.
  (`**Nothing is drawn that is not proven**`)
- Colour fonts are not drawable (`drawtext` has no BGRA path; CBDT is a hard error on 6.1.1 and 9.0.2; COLR
  draws as monochrome outlines). (`**Colour fonts**`)
- Known costs not fixed: keyframed slide-in text rasterises per frame (quantising risks exactness); a non-ASCII
  Windows profile path reaches `fontfile` through FreeType's narrow open (not run on Windows).
  (`Known costs, not fixed`)
- Builds differ: 4.4.2 lays text in whole pixels with fixed-point colour; 6.1.1 and 9.0.2 use 26.6 HarfBuzz and
  float colour; `colours_parse_as_ffmpeg_reads_them` reads colours through the `color` source because 4.4.2's
  `drawbox color=red` writes 254. (`**Builds differ**`)

## Animated SVG titles (lines 817-834)

- ffmpeg never sees an SVG: resvg draws a straight-alpha PNG (`svg_layer.rs`) in both the graph and the
  compositor, so artwork is no parity question; the compositor reproduces `overlay`'s `blend_plane`, the
  position ROUNDED DOWN TO EVEN, and chroma alpha averages. (`and **the position rounded down to even**`)
- The compositor asks ffmpeg for the converted planes (`svg::convert`) through a `-filter_complex` graph (a
  simple `-vf format=yuva420p` converts differently on some builds). (`The trap`)
- Drawn only when the plan says so (`caps.svg`: a floating title over at least one clip on a BT.601 composite
  where `svg::svg()` measured `Match`); `draw_svg` runs AFTER the layers and BEFORE the text. Refused: a title
  CLIP, a frame with no clip, a non-BT.601 composite, a picture over 2 x 4K pixels / the texture limit.
  (`**Drawn**: the plan`)
- `PictureCache`: 64 pictures / 160 MB; ONE FLIGHT per picture with no lock held while made, waiters 30 s max;
  failures remembered 20 s; a resvg panic is a failure (`svg::make` catches it); `clear` leaves the books right.
  (`**one flight per picture with no lock held while it is made**`)
- `warm_svg` makes a frame's pictures ahead; `warm_svg_ahead` (playback prefetch, 1.5 s window) only where it
  fits without pushing out an unshown prefetched picture; during a play an animated-title frame is the stream's
  (`PlayRefusal::AnimatedTitle`). (`warm_svg_ahead`)

## The runtime self-test (lines 836-865)

- `parity_self_test`: on first use of the GPU path it draws up to six small frames (`fit`, `pip`, `bt709`,
  `grade`, `place`, `pip-odd`; `pip-odd` LAST so `starts_with(["fit","pip","bt709"])` holds) through the
  compositor and that machine's FFmpeg and holds them to a COARSE screen (`PSNR_FLAT_MIN` 40 dB etc.). It is NOT
  the parity claim: parity is zero differing samples in `parity.rs` / `motion_parity.rs`; a false alarm closes
  the GPU path for someone it works for. (`**This is not the parity claim and is not held to it**`)
- Frames that need a measured model are drawn only where it matched: grade where `grade::kernels()` is `Match`
  (else the plan refuses its own frame -> `Inconclusive` -> the whole path closed), placed where `placement()`
  is `Match`, `pip-odd` where the odd model was found, and all frames that resize only where the scaler is
  measured exact (else clips are made at canvas size). Probes held back as `Unknown` stay held (`held_back`)
  until a self-test that can tell has run; `Check::retry()` re-runs a failed check only. `Differs` is permanent.
  (`**The scaler is pinned and held back like the grade**`)
- No rotation frame (float sampling differs a level between GPUs, not drift). References come from
  `export_still`, GPU frames through a private `FrameSource` (`Hint::Scrub`), thread-capped, no budget slot, no
  project lock. (`No rotation: its float sampling can differ by a level`)
- A render NEVER runs a probe: `plane_filters`, grade, matrix conversion, resize gate and `geq` read what was
  measured (`geq_edge_measured()`); `prepare` runs them off the render lock per plan, `prepare_timeline` once
  per session (+ `prepare_placed_files`, four files at a time, none started after 20 s) so no frame of the loop
  waits on a probe. (`**A render never runs a probe**`)
- Result `Ok` / `SelfTestError::Mismatch` / `Inconclusive` (FFmpeg won't make clips, colour policy unmeasurable,
  plan refuses its own frame, GPU failure, 90 s deadline); the deadline is looked at BETWEEN steps, so the worst
  case is about four minutes of everything going through FFmpeg. What the app does with it: `app.md`.
  (`**The deadline is looked at between`)

## Fades, dips, dissolves, slides, pushes (lines 867-927)

- Fades use `vf_fade`'s own integer arithmetic (`fade_ops` factors, `kerf_core::fade_luma` / `fade_chroma` /
  `fade_alpha` and the shader copies; black level 16 on `yuv420p`, 0 on `yuva420p`; chroma constant 8421367, NOT
  plain `+ 32768`); they run in `fs_compose` AFTER rotation and letterbox matte, on the 8-bit picture.
  (`**`vf_fade`'s own`)
- A dissolve is two ordinary layers (outgoing tail from borrowed handle + incoming on an alpha ramp blended
  `FAST_DIV255`); a layer with alpha below 255 is blended IN INTEGERS (`fs_compose_exact`), never by hardware
  blend (device-dependent rounding); an opaque layer keeps the hardware's.
  (`A **dissolve is two ordinary layers**`)
- The alpha ramp of a translucent incoming clip comes AHEAD of the 256/255 scale: `((a * f + 32768) >> 16) * 256
  / 255`. (`**The alpha ramp of a translucent incoming clip comes ahead of the 256/255 scale**`)
- Dip to white is an RGB filter: the compositor runs the grade's conversions (swscale's x86
  `ff_yuv_420_rgb24_ssse3` in, generic scaler out) around it and is EXACT where `grade::kernels() == Match`
  (`caps.dip_white`; the render re-checks); an arm64 kernel makes it FFmpeg's. A dip "drawn within a bound" is
  not drawn. (`dip drawn within a bound is not drawn`)
- Dip / fade refusals (`FadeOnTranslucent` / `DipToWhite` / `FullRangeNegotiated`): fade on a layer below full
  opacity (RGBA incl. alpha plane); dip on an alpha plane, beside a black fade of the same clip, on any
  non-`yuv420p` picture or non-BT.601/709 matrix, on a `yuv420p` flagged full range (`DipWhy::FullRange`), under
  `invert` / `vignette` (`DipWhy::RgbEffect`), on a letterboxed, odd-sided or scaler-resized layer.
  (`**Where it is refused**`)
- Full range: a `yuvj` picture is refused whenever the composite policy is not `FixedBt601` (9.0), together with
  EVERY deeper-than-8-bit YCbCr/gray format, because the name is only a heuristic and the range TAG
  (`StreamInfo.color_range`, schema v10) is the truth; a YCbCr picture flagged `pc` is refused in any format;
  RGB / gray never count (ffprobe prints `pc` for them: counting refused every PNG). (`**A full-range
  (`yuvj`) picture is refused`)
- A `yuv420p` flagged `pc` (VP9 / HEVC / AV1 / FFV1-in-Matroska; `PlanStream::raw_full_range`) is NOT a `yuvj`
  picture on 6: its RAW samples reach fade, eq, vignette, chromakey, unsharp, gblur on both FFmpegs while the
  decode converted them, so `maybe_full_range()` is true and `FullRangeNegotiated` fires under EVERY policy for
  it; `FullRangeCut` refuses EVERY frame of a cut holding one under 9 (limited frames convert to full range
  everywhere). (`is not a `yuvj` picture on FFmpeg 6.`)
- `FullRangeResized` (both FFmpegs): swscale converts range inside the resize (chroma up to 15 off), so a
  resized full-range picture is refused in an export frame, inside a fade and in a still.
  (`unless the scaler resizes the picture`)
- Open end: the full-range policy is a proxy measured on FFmpeg 6.1.1 and 9.0.2 only; 5.x, 7.x, 8.x and distro
  builds are UNMEASURED. (`distro builds are unmeasured`)
- The dip-to-white refusals are the Motion plane's and conservative in stills on purpose (`reasons()` is one
  function for both plan modes; a refusal is not drawn approximately). (`conservative in stills on purpose`)
- Export frames (`PlanMode::Motion`) pick source frames with the `fps` filter (`Pick::Fps`) served by a
  `FrameCursor`; `MotionFrames` opens a cursor on first appearance, hands picks in order, drops it when the clip
  leaves, reopens for an earlier frame. A still plan holds the same fades / travel / tails with still `-ss`
  picks. (`**Export frames**`)

## Moving zoom last, masks and effects, moving stills (lines 929-1051)

- Chain for a moving-zoom clip is `crop, fit, setsar, fps, eq, effects, format=yuva420p, mask, rotate, fades,
  scale eval=frame`: everything before the zoom acts at the FIT size and the zoom magnifies it. A moving zoom or
  position is drawn by the placement pass; refused (`ZoomBehind`) when a rotation, opacity below 1 or dip to
  white is ahead, for odd height or an out-of-range pose (`Geometry`), for a layer not 4:2:0 / gray (unless
  `placed_chroma`) or possibly full range where range is negotiated (`PlacedFormat`). A grade ahead of the
  placement is drawn. (`**A moving zoom or position is drawn by the placement pass**`)
- Moving stills (Ken Burns, `still_motion`, `StillKinds`, `still_probe`): FFmpeg MAKES the picture (a scaled
  copy, `source::copy_of`, decoded once and cached) and the compositor MOVES it (`perspective` tables); a move
  that leaves the frame bare is the general chain and FFmpeg makes the FIT picture (`FitCopy`).
  (`**Moving stills (Ken Burns)**`)
- The export draws the frame at the DELIVERY's size (a render at another size is refused); a still plan's copy
  depends on the render size, so `RenderPlan::for_render(size)` must be called (every decoding entry point does;
  `composite_*` from a plan not worked out is refused).
  (`**The export draws the frame at the delivery's size**`)
- `still_probe` admits a CHAIN of a kind into the caps by a toy cut that takes it and no other (move inside the
  frame, engine fit of Cover, behind a static crop, `Staged` after a guard `crop ... exact=1` with no other
  crop; each with the delivery's scaler named or not; plus per-name routes `StillKinds::named_route`). Base
  chains (`jpeg()` / `rgb()` default scaler) are measured first; a layer is drawn only with its chain AND its
  kind's base chains measured. EVERY TEXT THAT IS DRAWN IS A TEXT A TOY CUT RAN (`FitShape::Cropped` is no
  chain: refused). (`**Every text that is drawn is a text a toy cut ran**`)
- Toy-cut alpha edges where `perspective`'s alpha ramps are skipped on purpose (held in whole frames by
  `parity.rs` / `motion_parity.rs`). A copy FFmpeg makes at another size than the plan says is `Differs`.
  (`**wherever the layer's alpha is settled**`)
- Moving-still refusals: kind not measured (4:2:2 / 4:4:4 JPEG, RGBA / gray / palette PNG, `yuv420p` file),
  full-range picture where range is negotiated, colour / grade / effect / mask / translucent / dip, keyed crop,
  keyed colour (`eq_in_chain`), blend mode, a chain not measured, wrong render size, copy of an odd side or over
  8192 (`MAX_COPY_SIDE`), a `perspective` with no table (`corners_have_a_map`) or a table over 128 MiB
  (`MAX_TABLE_BYTES`); mask / blur / sharpen / vignette / rotation make the move the general chain's.
  (`**Refused** (each a specific `StillMotion` detail`)
- `copies.rs`: one process-wide LRU of 96 MiB, single flight, failures 30 s. An export frame's copy is made
  ahead (`CursorFeed::open` / `open_planned`, which answers `Opened::Skipped` for a refused plan); a moving
  still has NO cursor; a still plan's copy is made for the instant: `Hint::Exact` waits, every other hint gets
  `Busy` and renders through FFmpeg. Source bounded 1..8192 a side.
  (`**The copies are kept apart from the frames**`)
- A photograph the probe saw turned or a file replaced since: Cover's one-stage fit is the engine's own text
  (names no picture size); any other fit is written at the probed sizes behind a guard `crop=...exact=1` that
  FFmpeg refuses on a different size, smaller or larger.
  (`**A photograph the probe saw turned, or a file replaced since**`)
- The fit-made family (`RenderPlan::fits_by_ffmpeg`): scaler / decode refusals (`Shrink`, `FullRangeResized`,
  `FormatResize`, chroma-grid `Geometry`) do not apply because FFmpeg makes the picture; `ScalerApproximate`,
  `odd_pad`, `MixedMatricesWith`, the full-range policy, blend, `eq_in_chain` and the chain's own gate still do.
  (`**The fit-made family**`)
- Planner `Ends` mirrors the engine's `placed = placement_animated() && still_motion.is_none()`: a moving still
  on its own route is a plain chain and never decides the cut's matrix. An RGB picture beside BT.709 / BT.2020
  footage is refused as a static PNG is. (`**The planner's `Ends`**`)
- The ledger plans export frames at the delivery's frame but judges at 540x302: refusal of other sizes is right
  (no plan stands for an export at another size: `cover_overflows` / `FoldedFit`). Held by
  `tests/still_route_size.rs`. (`so `StillLayer::geometry` / `fit_copy` refuse the frame at any other size`)
- On 9.0.2 a cut of JPEGs alone is NOT claimed (no case holds the base / bars / mixed cut) and stays refused.
  (`a cut of JPEGs alone is not claimed`)
- Live behaviours (breathe, drift, shake): the plan's `transform` is the exact pose at the frame
  (`Clip::transform_at`, `sin` / `cos` included) and the placement draws it; sway / spin turn the picture so
  `Behavior` stays. (`**Live behaviours**`)
- Order: stage 0 fit scale with `eq` tables in its vertical pass; effects, mask plane and fades run on fit-size
  planes; the placement then reads the four planes. Rotation, opacity below 1 and a dip to white ahead of the
  placement are refused whatever the caps (`ZoomBehind`). (`Refused whatever the caps`)

## The placement (lines 1053-1114)

- Chain: `format=yuva420p, pad, perspective=interpolation=linear[:eval=frame], crop`
  (`engine::cli::placement_chain`, export / stream / still alike); reproduced exactly. `pad` margin
  `2*ceil(max(0, cw-iw)/4)+2`, black@0, even rounding, an odd width loses its last column on every build; an ODD
  HEIGHT is refused (`Geometry`: pad-in-place depends on allocation and builds differ).
  (`**`pad`** adds a margin of`)
- `perspective` is `lrint(256 * ...)` in `f64` from corners evaluated in the expression's operation order
  (`PlacementFrame::corners`; `RenderPlan::pose` includes slide / push travel); `x1` / `x7` may be rounding
  noise, so `PlacementMap::new` takes per-axis tables only when first / last values round alike (widened by 1e-6
  + 1e-12), else the whole table (`PlacementMap::exact`). Poses over 2^21 samples are refused.
  (`**`perspective`** is `pv[x + y*w]`)
- Alpha is resampled like luma, so the fringe where the picture meets the pad is a ramp of alpha AND of Y
  towards 16. (`so the fringe where the picture meets the pad`)
- `PlacedFormat`: the chain converts to `yuva420p` itself where the decode converted first; same for 4:2:0 /
  gray, NOT for 4:4:4 / 4:2:2 (4.4.2's `scale` default is bilinear). The 4:4:4 / 4:2:2 refusal is lifted by a
  MEASUREMENT (`kerf_gpu::conversion`, `placed_chroma`), 8-bit planar only; RGB / 4:4:0 / 4:1:1 / 12-bit stay
  refused. (`kerf_gpu::conversion`)
- A full-range picture is refused where the composite's range is negotiated. An asset with `color_range` `None`:
  `placement_of` reads the file's answer FROM THE CACHE ALONE (`source_is_full_range_known`, `ProbeCache::peek`:
  spawns nothing, no wait) and refuses a `pc` one or no answer at the render. (`**from the cache alone**`)
- The session-level decisions must see that refusal before the frame does (`GpuCaps::file_range` set by
  `session_caps()` / `session_caps_for`; NOT by `caps()` / `caps_for` / `CAPS` / the compositor's entry points,
  so harnesses stay pure): the stream gets such a span up front. `prepare_ahead`, `prepare`,
  `prepare_ranges(plan)` (render bar via `BarRequest::ask`), `prepare_ranges_ahead` (session rescan every 0.5 s)
  ask first. (`**The session-level decisions see that refusal before the frame does**`)
- The render never probes; `prepare(plan)` does ahead of it (`prepare_ahead`, `begin_play`, `ready_play`,
  `make_primed`, `FrameSource::check`). The one-shot `render_plan` / `composite_yuv` prepare for themselves;
  `composite`, texture renders and counted composites do not.
  (`**The render never probes; `prepare(plan)` does, ahead of it**`)
- `frame_source::warm_range` makes the range answer exist at the decode (a `stat` + cache hit, one ffprobe for a
  file first seen OR REPLACED SINCE; cache keyed by size and mtime). (`**The decode makes the answer exist**`)
- Probes share `measure.rs` (`Verdict`, `Probe`, `cached` (an `Unknown` asked again after 30 s, never believed
  meanwhile), `checked`, `Budget::ffmpeg` via `run_piped_until`: killed at its limit, no thread joined that a
  descendant can keep alive). One probe at a time (condvar waiters bounded by the deadline + 0.5 s grace, then
  `Unknown`); no lock held while a child runs; a panicking probe gives its place up and backs off.
  (`**One probe at a time.**`)
- One deadline for the whole probe (`Timing { backoff 30 s, overall (45 s placement, 30 s conversion), per_run
  20 s }`), children get the shorter of their limit and what is left. (`**One deadline for the whole probe**`)
- Argv of a probe child: quiet flags, probe args, `limit_frame_args`; graphs over `GRAPH_ARG_MAX` (8192) go
  through a script file (`spill_long_graph`); the placement probe stacks poses in runs of at least two (one pose
  alone is placed without `split` / `hstack`) so no command line nears Windows' 32,767.
  (`**Argv.** A child's argv`)
- The self-test's `checked` waits out an earlier failure's backoff and retries once; twice-unknown sets
  `held_back` (a probe that succeeds later must not open a placed frame the check never drew).
  (`**The self-test's hold.**`)
- `placement_probe` uses the export's own chain text (`placement_probe_chain`, both `eval=init` and
  `eval=frame`) on tiny noise with every plane held to `placement::reference`, with ties, odd widths and keyed
  behaviours. (`* `placement_probe` (`GpuCaps::placement` and `behaviors`)`)
- `FpsPick::pinned` (`frame_pick.rs`): a placed clip pins `fps=fps=R:start_time=S` and the filter rounds each
  input time RELATIVE TO THE START (`vf_fps.c`); the plain `round(pts * rate)` differs on every tie. Held by
  `picked.rs`. (`**Which frame is drawn** (`FpsPick::pinned`, `frame_pick.rs`)`)
- Held: `tests/placement.rs`, `motion_parity.rs` (`zoom/*`, `position/*`, `shape/*`, `zoom+*`), `parity.rs`
  `a_moving_zoom_is_placed_as_the_still_places_it`. A layer the compositor resizes is NOT exact against the
  system 4.4.2 (its x86 swscale is further off the port). (`**Held to FFmpeg on both builds**`)

## Masks (lines 1116-1168)

- Mask alpha plane is `geq`'s: `kerf_core::mask_alpha_plane` evaluates in `f64` and TRUNCATES (`255 * 0.999` is
  254); cached per `(mask, size)` in `MaskCache` (8 planes / 96 MiB). The blend takes alpha PER SAMPLE
  (`fs_compose_exact`; chroma = `overlay`'s 2x2 average); on a letterbox the mask is the padded frame's.
  (`The alpha plane is `geq`'s`)
- FFmpeg 6.1's `geq` reads a plane's last column and row ONE IN (clamps to `w - 2` / `h - 2`); 9.0 copies them.
  `kerf_core::geq_edge()` measures which; the blend reads the layer one in (`ComposeParams::edge`) and planes of
  a late zoom and of a TURNED layer are shifted BEFORE the scale / turn (`edge_planes`, `fs_edge`). A mask or
  keyed opacity is `Unsupported` while the build is `Unknown`; `caps()` omits `mask` / `keyed_opacity` then;
  `prepare` / `prepare_timeline` measure it, the render reads `geq_edge_measured()`; `with_geq_edge` pins.
  (`**FFmpeg 6.1's `geq` reads a plane's last column and row one in**`)
- A mask on a layer below full opacity is drawn: `colorchannelmixer` writes `av_clip_uint8(lrint(level *
  opacity))` per level, then the conversion scales 256/255 (`layer_geometry::ffmpeg_alpha_of`, THE one rule);
  the compositor maps the mask plane through that table (`LayerFades::mask_alpha`, `alpha_levels`, `fs_lut`); on
  6.1 the `geq` last-column copy comes BEFORE the round trip, so planes shift before it. One verdict
  (`Translucency`) covers it: a build whose masked alpha differs refuses every translucent layer.
  (`**A mask on a layer below full opacity is drawn**`)
- A chroma key on a layer below full opacity is drawn the same way (`LayerFades::mask_alpha` for
  `has_chroma_key`; no `geq`, no shifted planes); the probe splits the keyed picture so key and
  key-through-mixer come from ONE graph (the key's alpha depends on slice threads).
  (`**A chroma key on a layer below full opacity is drawn the same way**`)
- Refused: key on a ROTATED layer (necessary: up to 166 levels off) and key / mask on a BLENDED layer below full
  opacity, over a mask (`MaskWith`), before a moving zoom (`ZoomBehind`); the key-over-mask, blended and
  zoom-behind ones are UNPROVEN rather than wrong and stay refused until a case holds them.
  (`**Only the rotation is shown necessary**`)
- A mask under a keyed opacity is drawn as `((keep * curve) * 255.0) as u8` (`Keyed opacity`); refused
  (`MaskWith`): mask on a rotated layer, over a chroma key, on a blended layer below full opacity.
  (`**A mask under a keyed opacity**`)

## Rotation (lines 1170-1178)

- Only quarters `quarter_turn` computes (the set measured for -1310..1310 quarters on both builds: -47, -46,
  -43..40, 43, 44) are whole samples; EVERY OTHER ROTATION is refused on every layer (`Unsupported::Rotation`,
  also keyed / behaviour rotations); the compositor guards the same (`look_of`) and snaps a quarter's cos / sin
  to exact 0 / +-1. (`**Every other rotation is refused on every layer**`)
- A turned odd box still carries the overlay's chroma tail (`tail`).
  (`**A turned odd box carries the overlay's chroma tail like any odd layer**`)

## Display conversion (lines 1180-1188)

- The preview's RGB conversion uses swscale's x86 `yuv420p -> rgb24` integers (`ff_yuv_420_rgb24_ssse3`,
  `fs_convert`) and is gated by a PROBE not a version (`display_probe::display()`, once per process). Anything
  but `Match` (unmeasured, `Unknown`) and the compositor hands out NO RGB (`Output::Rgb` / `Output::Texture` ->
  `GpuError::Unsupported`, preview is FFmpeg's); YUV outputs and layer-free frames are unaffected. It is not a
  plan reason. A harness that renders RGB reads `display()` in its priming or pins `with_display(Match)`; the
  self-test checks RGB only on `Match`. (`**Anything but `Match` (an unmeasured probe and `Unknown` included)`)

## Translucent round trip and odd sizes (lines 1190-1232)

- Out of YUV is swscale's C `yuva2argb` (no SIMD for ARGB); back is the generic scaler with the x86 `yuv2yuvX`
  vertical kernel for every chroma row but the last (C) (`fs_chroma_v_x86`, `grade::vertical_start`); running
  the C arithmetic there was the "translucent ladder".
  (`**That vertical pass is the x86 `yuv2yuvX` kernel for every`)
- Which kernels this swscale runs is MEASURED (`roundtrip_probe::translucency()`, the export's own
  `colorchannelmixer` text, mask and key alpha cases); arm64 / SSE-only / other builds are `Differs` /
  `Unknown`: `translucent_approximate` is set, every layer through the round trip (`RenderPlan::through_rgba`)
  is refused (`TranslucentApproximate`) and `translucent_planes` refuses too; the self-test draws its PiP
  opaque. (`RenderPlan::through_rgba`)
- Every turn of a translucent layer is refused (`Rotation`): a quarter turn pairs other samples; a half turn
  pairs the same but filters rows the other way (asymmetric bicubic window).
  (`**Every turn of a translucent layer is refused**`)
- Odd translucent sizes: FFmpeg's output for odd pictures is a function of the picture alone (not garbage): even
  height = unscaled converter where an odd width's last pixel is NEVER written (frame pool zero: transparent
  black column, `zero_col`); odd height = generic scaler (and with odd width `SWS_FULL_CHR_H_INT`, 32-bit
  WRAPPING `yuv2rgb_write_full` arithmetic, `FullChroma`); 6.1 `argb -> yuva420p` pairs read the zero padding
  (`OddWidth::Pairs`), 9.0 scales the row (`OddWidth::Scaled`). (`**neither is garbage**`)
- `odd_translucency()` (`ODD_PAIRS` / `ODD_SCALED`, nine cases on 47x31 / 47x32 / 48x31) decides; `Unknown` is
  not kept and draws nothing odd; the plan (`translucent_odd_refusal`) draws plain / colour-corrected / masked /
  keyed / dissolving odd layers and keeps FFmpeg's sides under 16 (7-row filter takes another path), turns,
  blend, grade, effects other than the key, fade to black and dip to white. (`**What the plan draws**`)

## Keyed opacity and keyed colour (lines 1234-1281)

- Keyed opacity (export frames): the chain's `geq` alpha is `(int)(opacity(T) * 255)`; the plan carries it
  (`PlanLayer::keyed_alpha`), the compositor blends that one alpha in integers (`LayerFades::alpha`), NO round
  trip; the `geq` copies Y/U/V so 6.1 shifts them one in (`mask_edge`; before a turn: `edge_planes`); turned by
  a quarter drawn. (`**Keyed opacity** (export frames)`)
- Mask with keyed opacity: one `geq` multiplies LEFT TO RIGHT in `f64` and truncates (`mask_alpha_plane_keyed`);
  the compositor builds the plane on the CPU per frame; exact only with the same order and last bit of the
  curve. The alpha judged is the `geq`'s (`keyed_alpha < 255`), not the sampled opacity. Blend mode and
  `KeyedMask` still refused. (`**and below it**`)
- Before a moving zoom: drawn while the curve holds 1, refused once it falls (`ZoomBehind`). A chroma key under
  it is drawn: the key's plane is made BEFORE the shift (`chromakey` first), shifted under `mask_edge`, mapped
  through `LayerFades::key_alpha`; the plan always carries the curve, the compositor refuses a keyed layer that
  has none. (`A **chroma key under it is drawn**`)
- Keyed colour: `eq` is `eval=frame`; the plan carries evaluated numbers (`PlanLayer::color`,
  `keyed_color_value`, `Color::temperature_gammas` in the same operation order) and `look_of` builds `eq::luts`
  every frame (no shader change). The refusals about grading the picture are for the CLIP, not the frame
  (`RenderPlan::graded`): a full-range picture (`GradedFullRange`) on every frame, a graded layer in a stack
  holding one where colour is negotiated (`GradedInFullRangeStack`). (`**Keyed colour** (export frames)`)
- On a layer the compositor scales the `eq` tables used to multiply the scaler's level (`GradeSteep`,
  `MAX_AMPLIFIED_ERROR` 2); where `caps.exact_scaler` the refusal is GONE and the rule stays for scalers not
  exact. (`**On a layer the compositor scales, the tables used to multiply the scaler's level**`)

## Effects (lines 1283-1392)

- Grayscale (`hue=s=0`) and Invert (`negate`) are tables composed with `eq`'s in the scaler's vertical pass;
  sharpen is `unsharp` 5x5 on luma in integers; all byte for byte. (`* `Grayscale` (`hue=s=0`: U, V to 128)`)
- Blur is `gblur` (first-order recursive Gaussian in `float`, then `postscale`, clip, `lrintf`); exact ONLY
  where this FFmpeg's `gblur` is the FUSED-multiply-add recursion (x86 AVX2 / AVX-512). WGSL does not promise
  fused `fma`, so the fusion is done IN INTEGERS on the bit patterns (`fma.wgsl`; non-negative finite floats;
  IEEE subnormal / underflow handling: a naive version wrapped the exponent and drew garbage); nothing but
  `fma_pos` touches a value so no backend's denormal flush can change one.
  (`so **the fusion is done in integers**`)
- The coefficients must be in the domain or the blur is refused: `kerf_core::gblur::gaussian(sigma)` is `None`
  outside it (sigmas ~1.05e-8..1.29e-8 are excluded); plan refuses via `effect_refusal`, compositor uses the
  same function. Sigma outside 0..1024 and amount outside -2..5 are refused.
  (`kerf_core::gblur::gaussian(sigma)`)
- The kernel is MEASURED (`blur_probe::blur()`, once per process, the export's own `gblur` text over ten
  pictures chosen to tell fused from unfused AND one picture per recursion sweep, plus odd-sided ones);
  `EffectKinds::BLUR` is removed on anything but `Match` (SSE4-only or C-loop builds). Sigma 0 is the identity
  on every build. A blur is no source of a difference any more (sharpen / chroma key after it are drawn).
  (`EffectKinds::BLUR`)
- FFmpeg's `gblur` is NOT the recursion on every plane shape at every slice-thread count: a blur on such a plane
  is FFmpeg's (`kerf_core::gblur::plane_hazard` / `picture_hazard`). Hazards: (1) a plane one sample wide or
  tall (heap corruption), (2) more slice threads than rows (`filter_postscale` scales the first `lanes` samples
  twice, racily), (3) a slice of `lanes` rows or more over a plane `lanes` wide or less. Sigma exactly 0 lifts
  (2) only (proof: `plane_hazard_for(.., identity)`); a tiny sigma with the same numbers
  (`Gaussian::is_identity`) is NOT lifted. (`**FFmpeg's own `gblur` is not the recursion on every plane shape`)
- The hazard rule is COMPUTED from the planes (luma + half chroma, rounded up) and the threads of the graph the
  frame stands in for (`RenderPlan::graph_thread_range`: compositor pin; one for a still of proxies / images
  (`single_frame_args`); the budget's where `limit_args` caps the graph (75% default -> `-filter_complex_threads
  3` on 4 cores); else `min(av_cpu_count() + 1, 16)`) and the vector width the probe found (`blur_lanes`).
  (`**The rule is computed, not a margin**`)
- `kerf_core::ffmpeg_cpu_count()` reproduces `av_cpu_count` on Linux via `sched_getaffinity` and is READ AT EACH
  JUDGEMENT, NOT cached (affinity / cgroup cpusets change under a running process); `cores()` applies a cgroup
  quota FFmpeg does not. (`kerf_core::ffmpeg_cpu_count()`)
- Where the count is `None` (Windows, macOS, a failed call -> FFmpeg counts ONE) the range is `1..=16`, a blur
  is judged over all of it, and a CHROMA KEY is NOT drawn (`graph_threads_for` is `Some` only for one number).
  (`**Where the count is `None`**`)
- FFmpeg's own process must count what Kerf does: `cli::command` removes OpenMP binding variables
  (`OMP_PROC_BIND`, `OMP_PLACES`, `GOMP_CPU_AFFINITY`) from every child's environment (`engine-cli.md`).
  (`**FFmpeg's own process must count what Kerf does**`)
- The blur probe finds the vector width and checks the rule over shape cases; a width whose rule clears a plane
  that is not the model's is dropped, none left is `Differs`; a child that would not run does not hide a
  `Differs` found elsewhere; at most 29 children, four at a time, one 45 s deadline.
  (`**The probe finds the width and checks the rule**`)
- On a SOFTWARE adapter the blur waits for the render passes before it (`Compositor::settle`: submit, wait):
  lavapipe starts a dispatch while its rasteriser threads still draw the texture it reads (Mesa 23.2). A
  hardware adapter is never stalled. (`software adapter it waits for the render passes before it`)
- Vignette: `cos(angle * d / dmax)^4` in `double` stored as `float` with the angle `PI/5` AS A `float`; `float`
  products and sums in integers (no FMA contraction); luma `(int)(f * s + dither)`. The dither is an LCG that
  RUNS ON ACROSS FRAMES: sample `i` of frame `n` is state `n * (w*h + 2*cw*ch) + i` (`vignette_steps`); a
  still's `n` is 0, an export frame's its clip's (`Pick::output_index`, `FrameCursor::count`,
  `MotionFrames::counts`; without a count the frame is `Unsupported`); the count is the FULL cut's, so a range
  export (`rendered.slice`) and the hand-over stream restart it at the slice (a level on a dithered sample).
  (`cos(angle * d / dmax)^4`)
- Vignette / invert / blur / sharpen / key refuse a full-range picture (format `yuvj*` OR `color_range ==
  "pc"`). Open end: an asset stored before schema v10 has no flag (`None`) and a pre-v10 `pc` `yuv420p` file is
  still drawn wrong until re-probed (a relink replaces its streams; the library does not re-probe on open).
  (`**Gap, narrower:**`)
- Chroma key: alpha is the mean of nine `sqrt((du^2 + dv^2) / (255^2 * 2))` over the 3x3 chroma window in
  `double` (table of `double` bits summed in integers, `chroma_key_add`, 255 thresholds by bisection); only the
  LAST key counts. OUTSIDE the picture a window entry keeps what the filter last wrote in the same slice, so the
  first sample of each slice depends on the graph's SLICE THREADS (`min(h, threads)`): the compositor uses the
  graph's (`Compositor::graph_threads_for`, `kerf_core::cpu_exact_graph_threads()`; `with_graph_threads` pins).
  A `yuv420p` flagged `pc` is refused. (`**Outside the picture** a window entry is not reloaded`)
- Effect refusals (`Effects`): vignette on a non-`yuv420p` or full-range picture or a map over the
  storage-buffer limit; chroma key after a scale where `!caps.exact_scaler`, on a non-`yuv420p` / full-range
  picture, on a rotated or blended-below-full-opacity layer, narrower / shorter than 2, similarity / blend
  outside 0.00001..1 / 0..1; blur over the storage limit; where `!caps.exact_scaler` a sharpen that amplifies a
  scale's difference (worst case `1 + 2|amount|`, each later sharpen adds `ceil(2 * err * |amount|)`, refused
  past 2); `Matte`; invert / blur / sharpen on full-range or unrecorded-format pictures; invert of a gray
  picture. (`**where the scaler is not measured to be swscale's**`)
- 4:2:2, 4:4:4, RGB and gray pictures (and 10-bit under `FixedBt601`) take every other effect and a mask at the
  plain source's own figures; compute passes are core wgpu, no optional feature. (`take every other effect`)

## A letterbox's padded frame (lines 1394-1420)

- FFmpeg's chain `scale, pad, setsar, fps, eq, grade, effects, format` hands everything after `pad` the WHOLE
  canvas-sized frame, bars included; a layer with `geom.matte` that has blur / sharpen / vignette / grade / a
  table after one lays out the planes of the padded frame whole (`fs_pad`, `pad_planes`) and from there is that
  frame (`matte = false`); the plan judges the same frame. (`**A letterbox's padded frame**`)
- Held only for an 8-bit 4:2:0 `yuv420p` picture (other formats refused as UNPROVEN), no mask, no keyed number;
  a letterboxed layer is the identity pose (opaque, upright). A grade on it additionally: the picture's own
  sides even, and NO `eq` ahead of it at an odd offset (`(canvas - picture) / 2` odd, no `eq`, no blurred fill
  -> `GradeWith` "at an odd offset": RGB `pad` rounds nothing). (`**Held** (`RenderPlan::padded_refusal`)
- A sharpen is refused on a frame under 8 rows (`UNSHARP_MIN_ROWS`; also in `look_of`): `unsharp` is not the
  model on 2 / 4 / 6 rows. A chroma key on a letterbox is still `Matte`. A converted layer with any effect or
  grade is `MixedMatricesWith`. (`UNSHARP_MIN_ROWS`)

## The blurred fill (`Fit::Blur`, lines 1422-1448)

- A clip with the plain fit whose picture is not the frame's shape sits over a copy of itself
  (`engine::cli::blur_fill`): bilinear thumbnail `2*floor(w/16)` x `2*floor(h/16)` (min 4x4), crop about the
  centre, `boxblur=r:2`, bilinear scale back, the fit `overlay`ed. Drawn plane for plane (box blur mirrors the
  line INCLUDING the edge sample; `eq` tables in the vertical pass that scales the fill up; `overlay` copies the
  fitted picture at its even position, so an odd picture's last chroma column is the picture's).
  (`**The blurred fill**`)
- `takes_blur_fill`: identity transform, not keyed, no reframe / mask / alpha / alpha-producing effect, AND the
  render size's shape (`RenderPlan::blur_fill(layer, size)`); every other clip is placed by Contain's chain.
  Refused (`BlurFill`, `BlurFillWith`): not 8-bit `yuv420p`, full-range, HDR, thumbnail shrunk past 63:1, frames
  whose chroma lines are shorter than the box (`2r + 1`), a chroma key on the padded frame, dip to white. Blur /
  sharpen / vignette / grade over a fill are drawn. (`**Which layers**: `takes_blur_fill` is the clip's`)
- `still_planes_differ` is the exact comparator for still plans (the still's own `yuv444p` planes against the
  compositor's composite put through the same conversion). (`**an exact comparator for still plans**`)

## Blend modes (lines 1450-1486)

- FFmpeg lays the clip opaque on a composite copy, `alphamerge`s, converts both to `gbrap` and back with
  `scale=flags=neighbor`, mixes with `blend` in RGB, `overlay`s onto the untouched composite; everything after
  the placement is reproduced in integers (`blend.rs`, `shaders/blend.wgsl`, `compositor/blend_passes.rs`,
  `tests/blend_kernels.rs`). Into RGB is `yuv2gbrp_full_X_c` (32-bit WRAPPING integers; `gbrp` is another path,
  `gbrap` the chain's); out of RGB chroma comes from the BOTTOM row of each 2x2 block; the clip's Y is clamped
  16..235 / UV 16..240 (`clipval`, `clip_colours`) while the composite outside it is not. (`**Blend modes**`)
- Kerf's `Overlay` mode is FFmpeg's `hardlight` and `HardLight` its `overlay`.
  (`kerf's `Overlay` is FFmpeg's `hardlight``)
- The last `overlay` weighs by the clip's alpha per luma sample and chroma block (`blend255`, `chroma_alpha`);
  its fades are ALL ALPHA FADES (a multiplied clip must not go through black), run in `ClipTiming::fades`'s
  order, NOT in-then-out (`LayerFades::ramps`, up to eight). (`ClipTiming::fades`)
- The blend matrix is the one the graph NAMES (`PlanCanvas::blend_tag`: bottom layer's tag over the whole cut
  for an export, over the layers on screen for a still; BT.709 for a labelled one): BT.601 / 709 / 2020 have
  tables (2020 only where the probe matched, `BlendExtras::BT2020`); `fcc`, `smpte240m` and EVERY tag not on the
  allow-list (`matrix_tag_is_unlisted`) are refused under every policy.
  (`**The matrix is the one the graph names**`)
- `blend::kernels()` (once per process, off every lock, `prepare` asks) runs the export's own chain text per
  mode and matrix and keeps the modes that matched; `blend_extras`: `BT2020`, `RGB_PICTURE` (BT.601 only; the
  export's own chain, NOT two conversions side by side), `TRANSLUCENT`. (`**Does this FFmpeg do that?**`)
- The blend probe is bounded and keeps only a COMPLETE answer: one deadline `PROBE_TIMEOUT`, ~44 children
  `WORKERS` (6) at a time, none under a lock, one measurement at a time (`Probe::found`), a panic is contained,
  late children killed and their pipes never joined; a child that did not answer TAKES WHAT IT PROBES OUT and
  the probe is not kept (`Verdict::kept`), retried after backoff.
  (`**The probe is bounded and keeps only a complete answer**`)
- FFmpeg 4.4.2's blend probe fails (a real bug of that build: `overlay` mis-slices the alpha plane of a clip
  above the top edge on several threads), so every blend is refused there; parity cases check the refusal then
  draw with forced caps (`forced_blends`, `blend_check`).
  (`**FFmpeg 4.4.2's probe fails, and that is a real bug of that build**`)
- Blend refusals (`BlendMode` with its cause): non-quarter rotation (quarter / half / three-quarter turns
  drawn), letterboxed, GRADED (`yuva420p -> argb -> yuva420p` vs the grade's `yuv420p -> rgb24`), RGB picture
  where the probe says the graph's conversion differs, a matrix with no tables, a layer a moving zoom / position
  places (`a_blended_clip_whose_zoom_or_position_moves_is_ffmpegs`), a mode the probe refused, a translucent
  layer without `TRANSLUCENT`. (`*Refused* (`BlendMode` with its cause; every cause is listed)`)
- Translucent blended layers: the chain converts the one `argb` picture to `yuva420p` TWICE (once per branch);
  clip fades are `fade alpha=1` steps on the RGBA alpha, each rounding on its own, BEFORE the conversion's scale
  (`LayerFades::of` folds them via `ffmpeg_alpha_ramps` for a layer that takes the round trip); `TRANSLUCENT` is
  measured (six cases; BT.2020 extra only where `BT2020` is; one flag, refusal on the safe side). Resized
  blended layers: nothing in the blend reads the scale.
  (`**A layer below full opacity or resized is drawn, and its two old causes are gone**`)
- A blended clip whose matrix is not the composite's (FFmpeg 9): in a still, plain / shrunk / dissolving ones go
  through the cascade and translucent ones are refused (`convert_refusal`); in an export on 9.0.2 EVERY such
  frame is FFmpeg's (`unread_decider_in_cut` -> `MixedMatrices`); open end: lifting that needs the deciding clip
  read for a blend first. (`**A blended clip whose matrix is not the composite's**`)
- Long blend runs: 11+ consecutive blended clips used to deliver no frame on 9.0.2 (nested forks); runs of 7 or
  more are folded into one fork now (`engine-cli.md`); comments in the case and in the alpha sweep still
  describe the old limit. (`**A long run of blended clips in an export (fixed by the blend fold`)

## The clip's grade (lines 1488-1544)

- Grade = wheels (`colorbalance`), curves (`lutrgb`), 3D LUT (`lut3d`), run in THAT ORDER in RGB; the compositor
  draws the CONVERSIONS too, to the level (`grade.rs`, `grade.wgsl`, `compositor/grade_passes.rs`): in is
  swscale's x86 `ff_yuv_420_rgb24_ssse3` kernel (13-bit coefficients, NOT the C tables of the translucent round
  trip), odd sides refused; out untagged is `rgb24ToY_c` / `rgb24ToUV_half_c` + the x86 `yuv2yuvX` vertical on
  every chroma row but the last; out TAGGED ends with
  `scale=out_color_matrix=<m>,format=yuv444p,setparams=colorspace=<m>` (BT.601 / untagged and BT.709 drawn,
  other tags refused). (`**The clip's grade**`)
- Wheels are `color_balance8`'s float arithmetic op for op; LUT is `interp_8_tetrahedral` with axes in `f32` on
  the CPU, result TRUNCATED; every product a sum follows goes through `pin` (a GPU may fuse `a * b + c`) and
  every `/ 255.0f` is a CPU table. (`**Wheels** are `color_balance8`'s `float` arithmetic`)
- The grade is probed (`grade::kernels()`: every non-empty subset of the parts (seven) x untagged / BT.709, each
  as its OWN graph with `kerf_core::grade_filter_chain` text; fifteen children, thread-capped, NOT niced;
  stacking routes in one graph was wrong on 9.0.2 because FFmpeg 9 negotiates the matrix across a whole graph;
  `random()` restarts per slice thread). (`**Does this FFmpeg convert like that?**`)
- Anything but `Match` takes the grade out of the caps (`caps()` = `CAPS` less `grade` unless pinned
  (`with_grade_kernels`) or `grade::measured()` says `Match`): the plan refuses a graded layer
  (`Unsupported::Grade`) before anything is decoded and playback hands the cut to the stream; never-measured
  counts as not known (`prepare()`, render bar, self-test measure ahead). `caps_for` / `caps_with` /
  `caps_pinned` are the device-free forms; the grade is measured on x86-64 only.
  (`**Anything but `Match` takes the grade out of the caps**`)
- Grade refusals (`GradeWith` / `Matte` / `ZoomBehind`): a layer the scaler resizes with NO `eq` between the fit
  `scale` and the grade (the negotiation has `scale` output RGB itself); odd side; anything but 8-bit
  limited-range `yuv420p`; BT.2020 / `fcc` / `smpte240m`; a video effect other than a chroma key after the
  grade; a fade or dip running on it (a clip's own `fade_in` / `fade_out` is NOT a dissolve); a cut mixing
  matrices under a negotiating policy. Drawn with a grade: moving zoom / position, opacity, mask, rotation,
  dissolve, chroma key, `eq` ahead, speed, keyed opacity / position.
  (`* **Refused** (each `Unsupported::GradeWith` / `Matte` / `ZoomBehind` in the plan`)
- A cut that mixes matrices (`PlanCanvas::mixed_matrices_cut`, Motion plans only): under `BottomLayerTag` FFmpeg
  9 converts the grade's output into the bottom layer's matrix INSIDE THE GRAPH ON EVERY FRAME OF THE CLIP, so
  `GradeWith("in a cut that mixes colour matrices")` refuses the graded clip in every frame; a GRADED layer
  stays refused in a mixed cut (the ungraded layer is converted into the cut's matrix,
  `PlanCanvas::cut_matrix`). One matrix is not a mix. (`* **A cut that mixes colour matrices**`)
- Not drawn yet (next steps): scaled graded layer with no `eq` (needs fit scale + RGB conversion as one pass);
  effect or fade after a grade; 10-bit / 4:2:2 / 4:4:4 grade kernels. Cost: on a software adapter FFmpeg's
  graded export is faster; a real GPU is unmeasured. (`**Not drawn yet, and why** (the next steps)`)

## Layers of another matrix than the bottom's (lines 1546-1553)

- A compositor whose conversion is not `Match` draws NO frame of a mixed-matrix cut (`MixedMatrices` on every
  frame, the deciding clip's own included): which clip decides is the planner's reading of FFmpeg 9's format
  negotiation and the probe checks that reading too.
  (`**A compositor whose conversion is not `Match` draws no frame of a mixed-matrix cut**`)
- The composite takes the tag of the FIRST clip ON THE ALPHA PLANE (`Ends::Clean`: dissolve, keyed zoom /
  position, alpha source), else the first clip, in graph order (a track's clips in START-TIME order, tracks in
  list order); `Ends::Unread` clips (opacity, rotation, mask / key, `eq`, grade, effect, tone map, reprojection,
  dip to white, placed black fade) and blend modes make a mixed cut `MixedMatrices` (`unread_decider_in_cut`).
  An RGB picture converts with the CUT's matrix (`cut_non_601`, `RgbInStack`).
  (`**Which clip decides the composite's matrix**`)
- The conversion goes THROUGH RGB in two contexts (`yuv420p -> bgr24` source coefficients, then `ff_rgb24toyv12`
  with the destination's: C on the pinned 9.0.2; an inline-MMX build would differ, `Variant::Mmx`, probe
  `Differs`). In an export it is scaled THEN converted; in a still the last `scale` converts in one pass with
  its resize: swscale's CASCADE (drawn for a shrinking picture, refused otherwise). The decider is not
  converted. (`**Where in the chain**`)
- Converted pictures: opaque 8-bit limited `yuv420p` of even size (crop, scale, pad, position, speed, invert /
  grayscale in an export, dissolve, slide / push, fade to black on a plain export chain); refused
  (`MixedMatricesWith`): grade, other effects, mask, rotation, opacity below 1, colour correction, keyed
  numbers, HDR / reframed / 360, dip to white, fade in a still or on the alpha plane, 10-bit / gray / yuvj /
  still images, RGB in a non-BT.601 stack, and a cut whose decider is no known-matrix YCbCr picture.
  (`What is converted: an opaque 8-bit limited-range`)
- Cascade (`convert::scaled()`, `matrix_convert_scaled`): `bgr24` intermediate of the SMALLER size; for a
  shrinking picture context 0 is `yuv420p W x H -> bgr24 w x h` scaled with the source's coefficients; MMX
  output for all rows but the last two, C for those. Refused: a picture that does not shrink, Cover fit, odd
  side, unscaled luma rows, non-4:2:0, caps without `matrix_convert_scaled`.
  (`**The cascade of a scale that resizes as it converts**`)
- The blurred fill behind a converted layer has passes of its own (`enlarge_and_change_matrix`,
  `fill_convert.wgsl`, `matrix_convert_fill`; the `bgr24` intermediate is the thumbnail's size; scaling up then
  converting is NOT it). A fill that does not match leaves the verdict `Scaled::Shrinks`.
  (`**The blurred fill behind such a layer**`)
- Probes: `convert::kernels` (three rounds of 3, 2, 1 children; the three-clip decider graph only if the cheap
  round matched; caps carry `matrix_convert` only where it matches AND the grade's kernel does) and
  `convert::scaled()` (run by `kernels()`; 25 children, three sequential phases, 60 s worst case; a short
  reference picture is `Unknown` never `Differs`). (`The probe
  (`convert::kernels`, like `grade::kernels`: three rounds`)

## Test harnesses (lines 1555-1740)

- `tests/motion_parity.rs` (`#[ignore]`d) holds the compositor to the EXPORT frame by frame (every frame of the
  range, the last included; no named bound: every drawn sample equals the export's). A refusal case
  (`run_refusing`) skips the frames the plan refuses for its reason, requires at least one, and holds EVERY
  OTHER frame to the export (a refusal wider than it has to be shows). (`**A refusal case** (`run_refusing`)`)
- Refusal necessity: `drawn_anyway` cases draw a refused frame anyway and require a difference (`NEEDED` vs
  `WIDE` in `checker_z`); a compositor that learns to read raw samples fails and the refusal can go.
  (`**the refusal's necessity**`)
- `tests/checker_z.rs` / `checker_z_text.rs` (`--features local-suites`, once per FFmpeg) hold the round trip,
  `gblur`, display and full-range with NO bound; a `KERF_FFMPEG` wrapper adding `-cpuflags 0` must make those
  probes `Differs`. (`**`tests/checker_z.rs` and `checker_z_text.rs`**`)
- `tests/parity.rs` (CI job `parity` is a MATRIX of the distro and the pinned FFmpeg: they compose colour
  differently, a green run on one proves nothing about the other) renders each case through `export_still` and
  the GPU and holds 0 differing samples in: the still's `yuv444p` planes, the composite taken to `rgb24` by
  FFmpeg against the PNG, and the preview RGBA against the same conversion (where the grade probe says the
  kernel is the model's). A NEW VISUAL FEATURE GETS A CASE: `check_refused` (the plan says no, FFmpeg renders),
  `drawn_anyway` (must differ), `check_mixed` / `check_converted` / `check_resized` / `check_resized_refused`.
  (`**`tests/parity.rs`** (`#[ignore]`d; CI job `parity``)
- A failing case writes images to `target/parity/` (`KERF_PARITY_KEEP=1`, `KERF_PARITY_EXPLORE=1`). Tests of a
  binary run in PARALLEL on one compositor: a shared fixture is written once and renamed into place
  (`tests/common`, `write_fixture`), since `std::fs::write` truncates first and a reader found an empty `.cube`.
  (`KERF_PARITY_KEEP=1`)
- The readback is bounded: `READBACK_TIMEOUT` 30 s -> `GpuError::Readback`. (`**The readback is bounded**`)
- The ledger (`tests/coverage.rs`, `the_ledger_says_which_refusals_hold_the_compositor_back`) is plan-only and
  ranks which refusal holds the compositor back; it is exact only because `RenderPlan::reasons` lists EVERY
  cause of a layer; run it on BOTH FFmpegs (the colour policy decides some of it); it plans `OriginalMedia` at
  540 px where the app plans `ProxyMedia` at 960; `every_variant()` / `ordinal()` fail to compile when
  `Unsupported` gains a variant. T9..T25 are cuts using every feature that still refuses. (`**The ledger**
  (`tests/coverage.rs`)
- `tests/fate_corpus.rs` (`#[ignore]` AND `KERF_FATE=1`; downloads FATE samples to `target/fate-suite/`, NEVER
  commit them: mixed licences) feeds hostile input through probe, plan, `decode_layer`, `FrameSource`,
  `FrameCursor`; contract: a frame at the probed size with consistent planes (frame source equal to one-shot) OR
  AN ERROR; never a panic or hang. (`**`tests/fate_corpus.rs`**`)
- `checker_mx_*` (`checker_mx_cascade.rs`, `checker_mx_probe.rs`) hold the cascade model and probe;
  `checker_mx_cascade.rs` is NOT in CI's list. (`**The scaled cascade's checker cases**`)

## 10-bit deliveries (`yuv420p10le`, `yuv422p10le`; lines 1723-1740)

- The composite is 8 bits but the clips are NOT built in 8: the fit `scale` writes the delivery's format, `fade`
  runs at 10 bits, an auto `scale` takes the picture to `yuva420p` for the overlay (dithering down BEFORE the
  blend), `overlay` composes 8-bit, a last auto `scale` takes the composite up. Modelled in
  `delivery10::{scale_to_10, fade_sample, dither_plane, chroma_to_420, to_ten_bit, base_planes}`.
  (`**The composite is 8 bits, but the clips are not built in 8**`)
- 10 -> 8 dither of a 4:2:0 picture is swscale's ordered 2x2 dither whose row is the picture's on 9.0.2 and the
  ROW WITHIN THE SLICE on 6.1.1 (slice threads cut `FFALIGN(ceil(rows / threads), 2)` rows;
  `ten_bit_dither_rows`, `TenBitKinds::PER_SLICE`): the plan then needs the threads the export spawns the graph
  with. A 4:2:2 delivery has no slice dependence. (`planarCopyWrapper`)
- `color=c=black,format=<10-bit>` is luma 64 and chroma 514, NOT 512: an empty 10-bit canvas is
  `delivery10::base_planes`, not Y 16 / U = V = 128.
  (`**`color=c=black,format=<10-bit>` is luma 64 and chroma 514, not 512**`)
- `RenderPlan::ten_bit_layer_refusal` is a POSITIVE list: a plain `yuv420p` clip of a known matrix filling the
  frame by the one fit `scale` with fades to black only -> else `DeliveryWith`; titles over no clip, SVG, mixed
  matrices, a named scaler or an undecided dither -> `DeliveryFrame`; other depths / layouts `Delivery`.
  (`**Which layers**: `RenderPlan::ten_bit_layer_refusal` is a positive list`)
- `delivery10::ten_bit()` (once per process, 120 s bound, 72 children) holds every plane of eight toy cuts at 1
  / 2 / 4 threads to the model; `measured_kinds()` says which layouts matched; the 10-bit planes are the 8-bit
  composite taken up ON THE CPU (`to_ten_bit`), not a shader.
  (`**Does this FFmpeg do that?** `delivery10::ten_bit()``)
- Threads of a 4:2:0 10-bit frame: the engine lowers `-filter_complex_threads` AT SPAWN for wide graphs
  (`cap_wide_graph`: `max(1, 1536 / contexts)`, `GRAPH_POOL_THREADS` 1536, never raised, not for `chromakey` /
  `gblur` graphs) and a cut rendered in parts spawns each part with its own graph. ONE arithmetic:
  `kerf_core::wide_graph_cap(GraphShape, granted)` is what the engine writes and the plan applies; thread flags
  stay in `spawn_caps`, never the pure builders. (`**One arithmetic, shared.**`)
- ONE decision of whole or parts: `render_attempt` and `export_graph_runs` both ask `export_parts`
  (`KERF_EXPORT_PARTS`); `PlanCanvas::export_runs` is lazy (`PER_SLICE` builds only) and resolved ONCE per plan
  (`RenderPlan::ten_bit_thread_range`, a compositor pin `GpuCaps::graph_threads` wins).
  (`**One decision of whole or parts.**`)
- A graph the plan cannot know is refused (`DeliveryFrame` on a `PER_SLICE` build): asset tick unrecorded ahead
  of foldable blended clips (`time_base_inputs`), a failing build, a frame no run holds, a request that does not
  say its export renders the delivery alone (`PlanRequest::alone`); a multi-format export written in one run
  (`together`) is not the graph of one delivery. (`**A graph the plan cannot know is refused**`)
- A still / preview plan of a 10-bit delivery is the 8-bit still graph, intended (`build_still_args` has no
  10-bit chain; `ten_bit_layout()` and `export_runs` are Motion-only; A2 plans stills with
  `ExportOptions::default()`).
  (`**A still / preview plan of a 10-bit delivery is the 8-bit still graph, intended**`)

## Process (lines 1742-1778)

- Every GPU change gets an independent parity review (full file: the paragraph on the `parity` check, after the
  implementation and the verification, before review): tests only, never `src/` or a shader; both FFmpeg builds;
  mutation-checked arithmetic; each refusal drawn anyway to see it is necessary; reports the ledger and
  `coverage.rs` delta. (`**Every GPU change is judged by the `parity` agent**`)
- Test media is kept PER FFMPEG BUILD (`tests/common/mod.rs`: `media_dir(name)` = `target/tmp/<name>-<key>`, key
  = hash of path + `-version` + size/mtime; `common::ffmpeg` writes `<stem>.<pid>.<n>.part.<ext>` and renames;
  `write_atomic` / `write_script`; `media_path(name)` does not create). `parity.rs`, `motion_parity.rs`,
  `checker_bl_probe.rs`, `checker_bl_threads.rs` use them; the OTHER suites do not yet: run those ONE BUILD AT A
  TIME. (`**The synthesized media is kept per FFmpeg build**`)
- CI job `parity` runs a FAST SET: an explicit allowlist of test targets (`--lib --test parity --test compositor
  --test convert --test coverage --test cursor --test frame_source_fake --test prefetch --test playback --test
  grade --test text --test blend_kernels --test delivery10_parity -- --ignored`, then named `motion_parity`
  cases with `--exact`); the full set took hours and hit the 30-minute timeout. A NEW TEST FILE RUNS IN CI ONLY
  ONCE IT IS ADDED to that list in `.github/workflows/ci.yml`; heavy sweeps are `--skip`ped by name
  (`checker_mt_stills`, `checker_mt2`, `checker_mo_stills`, some `checker_bl_stills_*`; see `build-and-ci.md`).
  (`**What CI runs**`)
- EVERY VISUAL CHANGE STILL GETS ITS PARITY CASE IN THE FAST SET (`parity.rs` for a still, `compositor.rs` for a
  pass): a case only in `motion_parity.rs` or `checker_*` is not run by CI. A green CI proves the fast set; a
  change to the compositor, plan, a shader or the scaler / conversion / grade models is run in FULL locally on
  both builds, a suite at a time (hours). (`**Every visual change still gets its parity case in the fast set**`)
- Full local run: `cargo test -p kerf-gpu --no-default-features --features local-suites --locked -- --ignored`
  for the system FFmpeg, and again with `KERF_FFMPEG` / `KERF_FFPROBE` pointing at
  `crates/kerf-app/binaries/ffmpeg-<triple>` / `ffprobe-<triple>` for the pinned one.
  (`# the full set, the pinned one`)

## Entry points by topic

- Frame source: `FrameSource::{new, frames, cursor, check_cursor, release}`, `FrameCache`, `router::route`,
  `ThrashGuard`, `ShowinfoParser`, `Y4mReader`, `source::decode_layer`, `selftest::parity_self_test`.
- Playback: `MotionFrames` (`feed()` / `CursorFeed`), `FrameCursor`, `cursor::picks_through`,
  `Compositor::render_motion_texture`, `warm_text`, `warm_svg_ahead`, `prepare_timeline`,
  `prepare_placed_files`.
- Plan interface: `Compositor::{caps, caps_for, caps_with, caps_pinned, session_caps, session_caps_for}`,
  `RenderPlan::{reasons, for_render, for_decode}`, `prepare`, `prepare_ahead`, `prepare_ranges`.
- Measured models: `scaler::kernels`, `grade::kernels`, `convert::{kernels, scaled}`, `blend::{kernels,
  extras}`, `roundtrip_probe::{translucency, odd_translucency}`, `placement_probe::placement`,
  `blur_probe::blur`, `display_probe::display`, `svg::svg`, `delivery10::ten_bit`, `still_probe`,
  `resize444::resize_444`, `text::{model, build}`, `kerf_core::{geq_edge, composite_color_policy, quarter_turn,
  max_shrink_for}`.
- Tests: `tests/parity.rs` (stills), `tests/motion_parity.rs` (exports), `tests/coverage.rs` (ledger),
  `tests/frame_source*.rs`, `tests/cursor*.rs`, `tests/playback.rs`, `tests/scaler.rs`, `tests/grade.rs`,
  `tests/text.rs`.

## Known open ends the full file states

- NEON SIMD path has never run on arm64 (mask rows have no NEON kernel).
  (`**NEON is written but has never run**`)
- Nothing here has been run on WARP; the arm64 scaler `Aligned` model is not measured; the grade is measured on
  x86-64 only. (`nothing here has been run on WARP`)
- Full-range / range-negotiation policy measured on FFmpeg 6.1.1 and 9.0.2 only; 5.x, 7.x, 8.x, distro builds
  unmeasured. (`distro builds are unmeasured`)
- Assets stored before schema v10 have no range flag: a pre-v10 `pc` `yuv420p` file is still drawn wrong
  (vignette, chroma key). (`**Gap, narrower:**`)
- Folded fit (`FoldedFit`) layers need a one-stage `LayerGeometry` + TS port + corpus + parity cases to be
  drawn; the adapter suites for it were edited but not run where written.
  (`Drawing a folded layer needs a one-stage`)
- Present-mode / drag-time behaviour on a real GPU and a real GPU's grade / key cost are unmeasured.
  (`A real GPU is unmeasured`)
- Arial Narrow Latin text proof fails on shaped builds (suspected GPOS `x_offset`); RTL, colour fonts and script
  faces stay FFmpeg's; Windows non-ASCII font paths not run. (`suspected to be a GPOS placement`)
- Keyed-rotation, mask-over-key, blended-translucent key / mask and key-before-zoom refusals are unproven rather
  than wrong (candidates to lift with a parity case). (`**Only the rotation is shown necessary**`)
- Blended clips of another matrix in a 9.0.2 export are all FFmpeg's until the deciding clip is read for a
  blend. (`Lifting the export's refusal needs the deciding clip read for a blend first`)
- A cut of JPEGs alone under 9.0.2 is not claimed (`a cut of JPEGs alone is not claimed`); a Windows profile
  path with non-ASCII characters through FreeType's narrow open (`Windows profile path`).
  (`a cut of JPEGs alone is not claimed`)
- Text on a pre-held fit: keyframed slide-in text rasterises per frame (`so a **keyframed slide-in rasterises
  per frame**`). (`so a **keyframed slide-in rasterises per frame**`)
