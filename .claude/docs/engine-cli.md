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
  **A proxy reports, can be abandoned, and cannot hang.** `generate_proxy_with(src, width,
  ProxyRun { reservation, duration, progress, cancel })` is `generate_proxy` with hooks (the
  latter is it with none): the encode runs through `run_ffmpeg_streamed`, which writes
  `-progress pipe:1 -stats_period 0.5` **at spawn time** (like the thread caps — `build_proxy_args`
  and the golden oracle do not move), turns `out_time_us / duration` into a fraction held under 1
  (the file is not in place until it is renamed), polls `cancel` between reports (kill, remove the
  `.part`, `Error::Cancelled`) and kills ffmpeg after `EXPORT_STALL` (300 s) without a word — it
  used to be a bare `.output()` that waited forever *while holding the machine's one heavy slot*.
  The first report (0.0) comes when the encode holds the slot, so a proxy queued behind an export
  is not shown building. **A stalled hardware encode or decode is a hardware failure like a refusal**
  (`retry_proxy_in_software`, pure): it turns hardware encode (and decode) off for the process and the
  proxy is run again once in software, instead of hanging 300 s per proxy; a cancel is never retried. Each
  encode's temp file is `<hash>.<pid>.<n>.part` (a counter), so a rebuild that cancels the encode
  before it does not share one with its replacement. **The size is a setting**: `proxy_width(projection)`
  = `proxy_width_for(projection, proxy_base_width())`, the base width being 1280 (the default, whose
  cache stays valid) or the 720 / 1080 Settings pick (`set_proxy_base_width`, driven by
  `proxy::set_proxy_size`); a spherical source keeps the 3072 / 1280 ratio capped at 3072 (hardware
  H.264 stops at 4096 across and one refusal turns every hardware encode off). The width is in the
  key, so each size is its own file. `remove_proxy_files(src, width)` deletes a proxy and its sidecar.
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
  software fallback shared with the preview path (`disable_decode_hwaccel()` is how a
  decoder outside the engine, `kerf-gpu`'s runs, teaches it one); the GUI defaults export
  `hwaccel` to `auto` too, and `render_with_progress` retries a failed
  hardware-decode export once in software so the default can never lose a render.
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
  dropped entirely on a mono delivery. A clip's `adelay` is whole milliseconds when its start is one, else an exact sample count (`adelay=NS`, `audio_delay`) — ms rounding put a bar-aligned splice up to 0.5 ms off its neighbour. Tracks flagged `Track.duck` are mixed
  into their own bus and
  `sidechaincompress`'d against the rest before the final sum (music dips under
  dialogue); `ExportOptions.loudnorm` appends a single-pass `loudnorm` to -14 LUFS
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
