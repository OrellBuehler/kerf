# engine-cli — brief

Rules only. The full file `.claude/docs/engine-cli.md` is authoritative and holds the measurements and the story
behind each rule: grep it for the phrase in parentheses.

Orientation: the FFmpeg backends in `crates/kerf-core/src/engine/`. `cli.rs` (always compiled) drives the `ffmpeg` /
`ffprobe` binaries (`KERF_FFMPEG` / `KERF_FFPROBE`) and holds probe, analysis reads, stills, proxies, export graph,
playback stream; submodules under `engine/cli/`, plus `cpu.rs`, `decode_policy.rs`, `source_probe.rs`, `clip_timing.rs`,
`thumbnail.rs`, `scopes.rs`. The full file is one long bullet list; topics start at a bold lead.
`cli.rs` itself is only the parent now: its code is split by domain into `engine/cli/<topic>.rs` (`spawn`, `probe`,
`proxy`, `export_args`, `filter_graph`, `stills`, ...), all re-exported, so `engine::cli::name` still resolves. The full
file's first paragraph says which function lives where.

## Map of the full file (topic order; grep the bold lead)

feature + backends, `cli.rs` contents, proxy width / `-ss` parity / head pad / MPEG-TS, export late-start sync, proxy
sidecar, analysis on the proxy, child supervision, partial-output removal, model download, proxy progress, GPU
acceleration (AMF, VAAPI), `decode_policy.rs`, `cpu.rs` (lanes, budget, wide-graph cap, machine read, OpenMP, GPU
slot), export graph (fit, `Blur`, zoom-in-fit, project format, fps), mixer / ducking / master bus / loudness, captions
and chapters, export progress, all-deliveries-in-one-run, long export in parts, levels, read-back of the export, masks,
fades, crossfade power, scrubbed still and scrub windows, `clip_timing.rs`, denoise, keyframe expressions, keyed zoom
order, placement, per-property channels, live behaviours, grade, keyed colour / mask / crop / volume, filter-graph
safety, text and fonts, animated SVG titles, titles on tracks, 360, Insta360, seeking and stills, playback stream,
phone footage (rotation, range, HDR), BT.709 label, scopes, blend modes, blend folding, speed ramps, stabilization,
thumbnails.

## Feature and backends

- `kerf-core` is `default-features = false`; the `ffmpeg` feature is only reachable through each crate's forward, so
  `--no-default-features` disables it everywhere (`activated through these forwards`).
- Everything in `cli.rs` (probe, silencedetect, scenes, frames, contact sheet, salience, composited still, cover frame,
  waveforms, export) must work with only the binaries, never the dev libs (`never the dev libraries`).
- The salience map is deliberately not face detection: no model to ship, it only has to beat a centre crop
  (`deliberately *not* face detection`).
- The cover frame `export_still` reuses the still builder with a `StillOutput::File` sink and no preview width cap;
  the preview keeps its MJPEG pipe so existing still tests stay byte-identical (`cover frame`).
- The sheet of the cut tiles at most four stills at a time via `run_still_as`, because one export pass would decode
  every frame of the span (`sheet of the cut`).
- Preview decodes read a cached all-intra proxy (`generate_proxy` background, `ready_proxy` never blocks,
  `Project::preview_source`); export always reads the original (`cached all-intra proxy`).

## Proxies

- `proxy_width` is 1280 (3072 for a spherical asset: reframing crops ~100 deg out of it); the width is in the cache
  key, so marking an asset 360 rebuilds it. The size is a setting (`proxy_base_width`, 720 / 1080 / default 1280,
  `proxy::set_proxy_size`); each size is its own file; a spherical source keeps the 3072/1280 ratio capped at 3072
  (`That width`, `The size is a setting`).
- A proxy must answer `-ss T` with the frame the original does, also when video starts late (input `-ss` is relative
  to the container start; a proxy is video-only) (`A proxy must answer`).
- `build_proxy_args(head_pad)` fixes only sources with `head_lead` above 1 ms: one clone of the first frame at t=0,
  merged by `interleave`, with `-fps_mode vfr` spelled by `fps_mode_flag()` (`-vsync` before 5.1). Never
  `-fps_mode cfr`: it regrids every frame and makes VFR phone footage constant (`Not `-fps_mode cfr``).
- Key gains `|lead` only for those sources; the file is `<hash>.lead.mp4`; paddedness lives in the name
  (`is_head_padded_proxy`), not a flag on `Asset` (`travels in the name`).
- `transition_fx` sets `ClipFx.head_pad` from the input's name and `video_clip_chain` opens with `trim=start_frame=1`
  for a clip read from the proxy's start with no `-ss`; seeked reads skip the clone already (`ClipFx.head_pad`).
- MPEG-TS is not fixed: lead 0, plain proxy, unchanged key; a late-starting TS still previews seeks `lead` off
  (`MPEG-TS is not fixed`).
- `source_traits` = the one cached ffprobe per file (HDR, lead, container, range). An ffprobe exiting 0 with nothing
  describing the file is a failed probe (`probe_source_traits` -> `None`, remembered for the backoff), never cached
  as neutral traits (`failed probe`). Killed after 20 s, one probe however many threads ask, failure remembered 60 s
  (`ProbeCache`). `source_is_full_range_known` / `ProbeCache::peek` spawn and wait on nothing, for a render
  (`source_is_full_range_known`).
- `run_piped_until` is the bounded child for probes: no thread a child's descendant can keep alive is joined; output
  waited at most deadline + 250 ms (`no thread a child's descendant`).
- An encode reads `source_traits` once (`generate_proxy_with` passes them to `proxy_path_for`, tone-map and pad): a
  second read could differ and write a plain picture under the tone-mapped / padded name (`An encode reads it once`).
- A proxy describes itself: `generate_proxy` writes `<hash>.json` (`<hash>.lead.json`) before renaming the proxy into
  place; `read_proxy_sidecar` accepts only current version + matching size; `proxy_video_info` falls back to one cached
  ffprobe (off the project lock) and writes it back (`A proxy describes itself`).
- `finalize_proxy`: if `dst` exists drop the new encode and write no sidecar over the winner's; else sidecar first,
  rename second. Sidecar temp is `<hash>.json.<pid>.<n>.part`, never the proxy's `<hash>.<pid>.part` (a shared name
  renamed the half-finished proxy away); each encode's temp is `<hash>.<pid>.<n>.part` (`finalize_proxy`,
  `two threads writing one sidecar`).
- Tests that generate a proxy into the user's cache clean with `test_support::ProxyGuard` / `remove_proxy`
  (`ProxyGuard`). `remove_proxy_files` / `proxy_cache_files` name all eight files (four key spellings, each with a
  sidecar); cache dirs come from `paths::cache_dir()` (`proxy_cache_files`).
- `generate_proxy_with(src, width, ProxyRun {..})` runs via `run_ffmpeg_streamed`: `-progress pipe:1 -stats_period 0.5`
  at spawn (never in `build_proxy_args`), progress held under 1, cancel polled (kill, remove `.part`,
  `Error::Cancelled`), kill after `EXPORT_STALL` (300 s) silence; the first report (0.0) only when it holds the slot
  (`A proxy reports, can be abandoned`).
- A stalled hardware decode is a hardware failure: `retry_proxy_in_software` turns hardware decode off and reruns once
  in software; a cancel is never retried (`A stalled hardware decode`).
- Proxies never encode in hardware, for any vendor: `libx264 -g 1` (`proxy_encode_args`, pinned by a test) with
  hardware decode only; one hardware refusal would turn hardware encode off for every later proxy and stitch.
  `HW_ENCODE_OK` is the Insta360 stitch's alone (`Proxies never encode in hardware`).
- A proxy takes the cores' slot in the proxy lane, never the hardware slot; the queue has one worker
  (`KERF_PROXY_WORKERS`) (`A preview proxy never takes it`).

## Late-start sources in export (`The export keeps a late-start source's sync`)

- Lead is measured on the video stream (`StreamInfo::lead`, `video_lead`) from the earliest picture or sound stream
  start, not `format.start_time` (a subtitle / data track pulls it earlier); above 1 ms; at or past the duration it
  is 0 (`lead_within`); transport streams included; carried as `ClipFx::lead` (`not ffprobe's`).
- A window starting inside the lead is not seeked (`clip_seek(start, lead)`; the MOV demuxer would land every stream on
  the first picture and lose the sound); `setpts` adds `picture_delay` = (lead - window start) / speed, forward clips
  only (`picture_delay`).
- A fade-in starts on the first picture (`ClipTiming::fades` anchors clip fade and dip fade-up at start +
  `picture_delay`); a dissolve's ramp ends with the outgoing tail (`st` = start + delay, `d` = len - delay), none if
  no longer than the delay; a fade step of no length is never emitted (FFmpeg reads `d=0` as 25 frames)
  (`A fade-in starts on the first picture`, `A dissolve's ramp ends`).
- `Planner::open_windows` keeps a fade-in's window from the clip's start (`open_windows`); `frame_pick` takes
  `FpsPick::lead`; a `.lead` proxy keeps the original's lead, any other proxy has its own (`media::decoded_lead`,
  `Project::preview_assets_of`). Golden family `late-start-delay` pins it (`late-start-delay`).
- Known limits: a reversed clip's bare end still takes its fade-out; a text track starting before the sound,
  cut from before it; the scrubbed still shows the first picture in the gap, not made to agree; assets imported before
  the lead was recorded read 0 until relinked (`**Known limits:**`, `read 0 (the old`).

## Analysis on the proxy

- Scene detection (`detect_scenes_preferring`, `Project::scoring_proxy`) and contact sheets (`Project::sheet_proxy`)
  read the proxy when one is ready, never waited for; they swap the path only, so no golden argv moves
  (`reads the proxy when one is ready`).
- Gate `proxy_fidelity` from the original's `source_traits`: times (sheet) need MP4 / MOV / Matroska / WebM; scores
  (scenes) also need SDR and `luma_plane` (planar / semi-planar YCbCr or gray); RGB, packed YCbCr, palette, alpha,
  unknown `pix_fmt`, HLG / PQ read the original (`**The gate**`).
- A sheet from a `.lead` proxy prefixes `trim=start_frame=1` when it starts at 0.000, else every cell shows its
  neighbour's frame (`A sheet from a `.lead` proxy`).
- Both reads are bounded (`-progress pipe:2` at spawn, `output_unless_stalled`): a hung proxy read falls back to the
  original, a stalled original read is an error. ffmpeg prints nothing until a frame reaches the output, so the scene
  pass selects frame 0 too (`eq(n,0)`, dropped by `parse_scenes`), `READ_STALL` 60 s, and a sheet's `sheet_stall` is
  30 s per window second (60 s..2 h) (`Both reads are bounded`).
- `salience_map` (smart crop, framing) stays on the original (`stays on the original`).

## Child supervision

- Every ffmpeg / ffprobe child is supervised: nothing waits forever and a job's Stop reaches the child
  (`Every ffmpeg / ffprobe child is supervised`). Four shapes by read kind:
  - whole-file reads with `-progress` (`detect_silence_cancellable`, scene pass, `measure_loudness_cancellable`,
    `salience_map_cancellable`) use `output_supervised(cmd, stall, total, cancel)`: killed after `READ_STALL` of stderr
    silence (or `total`; pass `Duration::MAX` for no stall), cancel polled 50x/s, lease via `cpu::lease_waiting(..,
    cancel)` (plain `cpu::lease()` is test-only) and the cancel checked once after the lease
    (`Whole-file reads with a `-progress` stream`);
  - PCM decodes stream through `peaks::pump_pcm_cancellable` (stall kill, cancel every 100 ms) (`PCM decodes`);
  - moment reads / probes and once-per-process capability probes use `output_bounded` with a wall-clock limit (import
    probe 300 s because ffprobe prints nothing until done; 60 s capability, 120 s frame, 60 s audio window); a timed-out
    capability probe reads "cannot run at all" (`Moment reads and probes`);
  - streamed jobs (export, proxy, stitch, whisper filter) use `run_ffmpeg_progress_as` / `run_ffmpeg_streamed_in`:
    stall bound + cancel, then `reap` (`Streamed jobs`).
- Reads a job drives take a cancel (`probe_cancellable`, `frame_jpeg_cancellable`, `audio_pcm_cancellable`) via
  `output_bounded_cancellable`; a stop is `Error::Cancelled`, never read as a hardware failure (no software retry,
  `HWACCEL_OK` untouched). With `None` it is the plain path: a scrubbed frame is superseded, not cancelled
  (`The reads a job drives take a cancel`).
- `reap(child)` is the only way to `wait()` on a child whose pipes closed: 30 s grace, then kill (`is the only way to
  `wait()``). The in-process libav probe is abandoned after 300 s or on cancel (`probe_within`).
- After `paths::confine_demuxers` every probe is `ffprobe` with `-format_whitelist paths::MEDIA_DEMUXERS` so a playlist
  or manifest is refused before FFmpeg opens what it names (`confine_demuxers`).
- A failed decode is an error, never an empty answer: a non-zero `silencedetect` / scene / `loudnorm` run fails with
  ffmpeg's last words; the only non-zero exit that is an answer is "Output file does not contain any stream"
  (`A failed decode is an error`).
- Analysis providers all take a `CancelFn` so Stop ends the step in flight (`threads the pass's cancel`). Plain-named
  `fn`s are no-cancel wrappers only where something outside the engine uses them; `_cancellable` / `_with` is the hooked
  one; smart-crop samplers report `progress(f32)` and a stop is `Cancelled`, not a skipped clip (`` `fn` pairs``).
- A render that fails or is cancelled removes what it wrote, but only if this run touched it: `discarding_partial`
  (`file_mtime` before, `discard_partial(path, before)` after anything but `Completed`). Never an unconditional
  `remove_file` after a cancel: a cancel while queued or a failure before ffmpeg opened the output leaves last week's
  export alone. Same for `export_still_cancellable` and `stitch_insta360_with` (`.part` goes, never renamed into the
  cache); two-pass stats files are a drop guard (`removes what it wrote`).
- The speech-model download (`download.rs`) reads on a thread: no byte for 60 s is a stall (error, `.part` kept for
  resume), Cancel polled every 200 ms, with resolve / send / response timeouts (`The speech-model download`).

## Hardware encode / decode

- `hw_encoders()` probes once per process with a one-frame test encode per candidate (`-encoders` alone is not proof;
  `KERF_HW_ENCODE=none`); one thread per vendor, encoders one at a time (consumer NVENC counts sessions); a test
  encode at 20 s is killed, the `-encoders` listing gets 60 s (a cold launch under a virus scan must not cache
  "none") (`**GPU acceleration**`).
- AMF export: constant QP `hw_qp(vc, crf)` = CRF + 6, at most 51 (`av1_amf` CRF x 4, at most 255), `-quality quality`;
  `-profile:v baseline` is no AMF name (`constrained_baseline`); the stitch's `quality_args` keeps AMF 1:1; x264 stays
  every default, "Fast (GPU)" is opt-in (`Export on AMF`).
- VAAPI export: encoders take only device frames, so the builder names `-vaapi_device` (`VAAPI_DEFAULT_DEVICE`,
  builder stays pure) and the graph ends `[vhw]format=nv12,hwupload[outv]`; only 8-bit 4:2:0 validates; `tune_export_args`
  swaps in the probed render node at spawn (`KERF_VAAPI_DEVICE`); the first export waits 10 s on the probe
  (`vaapi_device_within`); `-rc_mode CQP -global_quality hw_qp(crf)`; stitches and proxies never see VAAPI
  (`**VAAPI export**`).
- Background decodes use `-hwaccel` (default `auto`, `KERF_HWACCEL=none`) with a learned software fallback shared with
  previews (`disable_decode_hwaccel()`, also called by `kerf-gpu`); `render_with_progress` retries a failed
  hardware-decode export once in software so the default can never lose a render (`software fallback shared with the preview path`).
- `engine/decode_policy.rs` decides per input at spawn on the built argv (`tune_inputs`); builders still emit `-hwaccel`
  as the options say (what the golden pins). Rules (`resolve_hwaccel`, pure): a cache proxy (`is_cache_proxy`) is never
  hardware-decoded; a one-frame read of proxies runs on one thread (`cpu::single_frame_args`; a still that also reads an
  original keeps the caps); Windows `auto` becomes `d3d11va` for H.264 / HEVC only; a one-frame read of a non-4:2:0
  original asks for none on Windows / Linux (macOS exempt; a named accelerator always passes verbatim); there is no
  size threshold (`**What each input is actually asked for**`, `non-4:2:0`, `no size threshold`).
- A named accelerator exits non-zero with no device where `auto` falls back quietly. Unclassified paths (stills, frame
  grabs, stream before its first frame, scenes, salience, filmstrips): any rescue clears `disable_decode_hwaccel`'s
  flag. Classified (`looks_like_hardware_decode_failure`: "No device available for decoder", "Hardware device setup
  failed", `Unrecognized hwaccel`, bad `hwaccel` option, `requires hwaccel`, the `implib-gen` library-load abort) convicts
  via `convict_hardware_decode` (sets `HWACCEL_PROVEN_BAD`); not `Device creation failed`, `Using auto hwaccel`,
  `Failed setup for format` (a successful `auto` run prints them) (`**Unclassified, as before**`, `**Classified**`).
- An export drops symbolic `auto` before the argv is built (`export_hwaccel_request`) under `KERF_HWACCEL=none` or once
  `HWACCEL_PROVEN_BAD`, not on the previews' flag; a named accelerator is never dropped. An export whose tuned argv has
  no `-hwaccel` gets no software rerun (it would be byte-identical) (`drops its symbolic `auto``, `gets no software
  rerun`).
- Call sites: `tune_still_args`, `stream_preview_once`, `tune_export_args`, `decode_frame` / `decode_hwaccel_for`;
  `limit_frame_args` for `kerf-gpu`'s decodes. The Insta360 stitch is not tuned (plain `auto`, never `d3d11va`)
  (`The Insta360 stitch is`).

## Machine budget (`engine/cpu.rs`)

- Heavy work is gated: one heavy job on the cores at a time at any budget (`cpu::lease`, reentrant: an export's second
  pass and a stitch inside an import must not queue behind themselves) with a share of the cores (`cpu_percent`, from
  `KERF_CPU_PERCENT`, set by settings). Two at once would be two shares (`one heavy job at a time`).
- Three lanes (`cpu::Priority`), arrival order within a lane: foreground (each export variant, levels, voiceover,
  Insta360 stitch at import, smart crop; default of plain `lease()`) before proxy before background (analysis,
  transcription, run inside `cpu::in_background` so every whole-file decode they reach queues there). A proxy must
  not make a fresh export wait; preview needs a proxy before analysis is wanted (`in three lanes`).
- A proxy reserves its place when queued (`cpu::reserve()` -> `Reservation::lease_waiting`), not when its worker asks:
  `spawn_proxy` runs in the import before the webview starts analysis of the same file. A reservation blocks background
  work, never foreground; a running job is never interrupted (`A proxy reserves its place`).
- A job that waits says why and can be stopped: `lease_waiting(on_wait, cancel)` calls `on_wait(Wait::Proxy | Wait::Job)`
  when it first waits (never under the gate's lock), polls `cancel` 4x/s, returns `Error::Cancelled`. Surfaces:
  `ExportProgress.waiting`, the analysis / voiceover `waiting` stage; levels only honours Stop; a queued proxy is
  cancelled by delete / rebuild / settings change without starting (`A job that waits says why`).
- Gated = whole-file jobs whose result is wanted later (silence / scene / loudness, PCM behind rhythm and in-process
  whisper, transcription, proxy, stitch, export). Ungated = what the UI draws from or an agent looks at (scrubbed
  frame, composited still, a clip's audio, preview stream, contact sheet, waveform pyramid, filmstrip), which must not
  wait out a render; still thread-capped, a couple at a time, waveform / filmstrip niced (scrub / stream keep normal
  priority); the filmstrip holds a tighter cap (`**Gated** =`).
- The share becomes `-threads` / `-filter_threads` / `-filter_complex_threads` at spawn (`cpu::limit_args` /
  `limit_cmd`), never in the pure builders; `-threads` is written twice (front for the decoder, before the last
  argument for the encoder) because ffmpeg assigns it per file group; plus below-normal priority
  (`cpu::background`) (`goes in twice`).
- At 100% none of it applies (byte-identical invocations), except the filmstrip decode (`cpu::cap_args` /
  `cpu::background_always`), which holds at every budget (`background_always`).
- A wide graph is capped (`cap_wide_graph` via `spawn_caps` in `cli.rs`): each `scale` holds a swscale pool of
  `-filter_complex_threads`, so threads = contexts x pool (hundreds of clips reach thousands; macOS
  `kern.num_taskthreads` 2,560 is unverified on a Mac). `graph_contexts` x threads is kept to 1,536 by lowering
  `-filter_complex_threads` to `1536 / contexts`, min 1; a graph that fits is untouched (golden). A graph with a
  `chromakey` or `gblur` is not lowered (`SLICE_DEPENDENT_FILTERS`: results depend on slice count which plan and
  compositor take to be the budget's) (`A wide graph is capped further`).
- One function `wide_graph_cap(GraphShape { contexts, slice_dependent }, granted)` is shared by the spawn and the GPU
  plan; `export_graph_runs` gives the plan each run's graph (`export_parts`, the decision `render_attempt` takes)
  (`wide_graph_cap(GraphShape`).
- Slice threads a graph runs on: `cpu::graph_thread_range` (GPU plan judges `gblur` over it) and
  `cpu::exact_graph_threads` (a `chromakey` is drawn at it) = the budget's where `limit_args` caps, else FFmpeg's
  `min(av_cpu_count() + 1, 16)` (`The slice threads a graph runs on`).
- One read of the machine (`cpu::machine`): one `sched_getaffinity` gives FFmpeg's CPU count (`cpu::ffmpeg_cpu_count()`)
  and, limited by the cgroup quota (`engine/cpu/cgroup.rs`, mirrors Rust 1.99.1 `available_parallelism`,
  `cores_are_available_parallelism` holds it equal), the cores the budget is a share of (`cpu::cores()`). Plan and spawn
  derive both alike; a quota changing mid-read is not covered; at 100% neither is read (`One read of the machine`).
- Caps written at spawn are one read too: `cpu::budget()` is a `Budget`; `limit_args` / `cap_args` / `limit_cmd` /
  `limit_outputs_args` take it and omit the cap when it is all the cores; a `Lease` carries its budget; a part is
  `budget.with_threads(n)`; a bare count is not a budget (compile error) (`is a `Budget``).
- `ffmpeg_cpu_count()` is `None` on Windows / macOS and when the call fails: `av_cpu_count` then keeps its initial one,
  so the range is `1..=16`, never `cores()` up. Consumers treat it as a range: `gblur` judged over all of it; a
  `chromakey` (drawn at one count, `Compositor::graph_threads_for`) is refused by the plan ("a chroma key over a graph
  whose slice-thread count is not known") unless the count is one number; `cpu::graph_threads()` is only the low end
  (`**`ffmpeg_cpu_count()` is `None`**`).
- `cli::command` (via `ffmpeg_command`, `bg_command`) is the one place a child of the engine or compositor is built and
  removes the OpenMP binding variables `OPENMP_BINDING_VARS` (`OMP_PROC_BIND`, `OMP_PLACES`, `GOMP_CPU_AFFINITY`,
  `KMP_AFFINITY`) via `env_remove` (argv untouched): distro FFmpeg 6.1.1's `libgomp` would pin the process so
  `av_cpu_count` reads 1. Only binding is removed; whisper's OpenMP loops keep the user's thread count and wait policy
  (`FFmpeg's own process must count what Kerf counts`).
- Hardware slot (`cpu::Resource::Gpu`, `MAX_GPU_JOBS` 2) is the one exception to one job at a time and only an export
  can take it; a proxy never does. Every doubt is the cores' slot. `export_resource` qualifies an export only if: lane
  on; hardware `-c:v` (`enc_family`); every input an image or a video with a named `-hwaccel` that decodes it (`auto`
  is never named; on Windows `decode_policy` makes it `d3d11va`); `InputFacts::hardware_decodes` (H.264 4:2:0 at 8 bit
  only, HEVC 4:2:0 at 8 / 10 bit; unrecorded H.264 depth not assumed 8); every filter in `HARDWARE_GRAPH_FILTERS`
  (none of `zscale` / `geq` / `rotate` / `v360` / `drawtext` / blur / grade). The rerun after a hardware decode failure
  is the cores'. `KERF_GPU_LANE=off` / `cpu::set_gpu_lane(false)` keeps all on the cores (`**The hardware's slot**`,
  `export_resource` (judged`).

## Export graph

- `build_export_args` / `build_filter_complex` (pure): a black canvas, every video clip `overlay`'d at its
  `timeline_start` (later tracks on top), every audio clip `adelay`'d and `amix`ed (`positional, multi-track`).
- Clip edges: chain trims its `picture_window`, puts it on the grid with `fps=`, cuts with `trim=end_pts=<end_frame>`;
  the overlay's `enable` opens a frame early (`ClipTiming::enable_window`) so back-to-back clips leave no frame between
  them (`**Clip edges:**`).
- `ExportOptions.fit`: `Contain` (default) scales and pads, `Cover` uses `force_original_aspect_ratio=increase` and crops;
  it is the base fit only, a clip's own transform composes on top (`decides what happens when the delivery aspect`).
- `Blur` = Contain over a blurred cover copy (`blur_fill`, pure; replaces `pad`; inner pads named `bf<instance>` export,
  `bf<layer>` still; thumbnail an eighth of the frame, `boxblur` radius under half the chroma plane). `takes_blur_fill`
  (pure, same answer for export and still) refuses: clips moved / scaled / rotated / cropped / keyframed by transform,
  360 reframes, see-through ones (`Asset::has_alpha`, chroma key, mask), frame-shaped or unprobed pictures; those get a
  Contain format from the caller, argv byte-identical to Contain. The `overlay` composes in 8-bit 4:2:0 except an HDR
  clip (`format=yuv420p10`). The GPU draws it only for 8-bit `yuv420p` with an exact scaler (`blur_fill_clip`,
  `kerf_core::blur_fill_chain`, `gpu.md`) (`**`Blur`** is Contain`, `Which clips take it`, `composes in 8-bit 4:2:0`).
- A zoom is part of the fit where it can be (`Clip::fit_order(fit, cover_overflows)` -> `FitOrder`, pure; one answer for
  export, stream, still, plan): `Folded` (Contain / Blur with a static or held zoom over 1: one `fit_scale`; ahead of a
  tone-map box `max(2,2*trunc((W*S)/2))` + `force_divisible_by=2`; below `TINY_SCALE` `max(1,..)`); `Uncropped` (Cover:
  `increase`, no crop to the frame so a position pans the real picture; `bound_crop` keeps it bounded with a centred
  `crop=w='iw-4*floor(max(0,iw-N)/4)'` so the overlay's `(W-w)/2` and even sizes still hold). Uncropped needs the cover to
  overhang (`layer_geometry::cover_overflows`, cautious yes when unknown) and a zoom or pan; a frame-shaped picture keeps
  its chain. Unchanged byte for byte: identity, Cover at scale 1 with no pan, zoom out under Contain, a moving zoom
  (last stage at fit size), Cover with a mask / vignette where the cover overhangs, reframed clips, titles. Folding the
  moving zoom was left (every stage between would run at source size, 4x pixels). The still follows
  (`still_clip_chain_graded(.., order)`, judged at the delivery frame). Golden `zoom-folded`, `cover-uncropped` and
  `still-` / `preview-` forms (`A zoom is part of the fit`, `Folding the moving zoom`).
- The frame shape is a project property: `Timeline.format: Option<Delivery>` (`{width, height, fit, fps?}`);
  `export_format` applies it between the footage default and an explicit `opts.resolution` (the dialog's size wins);
  unset, graphs are byte-identical (`The shape itself is a property of the project`).
- Frame rate: `Delivery.fps` (schema v12; `Delivery::rate()` 1..240, out-of-range ignored) replaces the first clip's
  rate; `opts.fps` wins over both; `export_format_sourced` names the source (`FpsSource`); `exported_frame` /
  `variant_frame` (`ExportedFrame`) come from `export_timeline`. `check_delivery_fps` makes `23.976` / `29.97` /
  `59.94` (within 0.005 of `n*1000/1001`) the exact NTSC rate (FFmpeg reads short decimals as `2997/100`). A variant
  keeps the project's rate (`Timeline::for_delivery`). Pinned by
  `the_delivery_rate_is_the_default_of_every_render_path_and_an_explicit_one_wins`; with no delivery rate the argv is the
  old one (`**The frame rate is the same`, `check_delivery_fps`).
- Stream, scrubbed still and export all derive the canvas from `export_format` (`preview_resolution`,
  `build_timeline_frame_args`); `Timeline::slice` / `for_render` carry `format`; `still_clip_chain` honours `fit`
  (`Timeline::slice`/`for_render` carry`).

## Mixer, ducking, master bus, loudness

- `Track.volume` rides each clip after its own gain / effect chain; `Track.pan` is a balance (`Track::pan_gains`), not
  constant-power: the side you turn towards stays at unity. Both are omitted at neutral values (byte-identical); pan
  is dropped on a mono delivery (`**mixer strip**`).
- `adelay` is whole ms when the start is one, else an exact sample count (`adelay=NS`, `audio_delay`): ms rounding put
  a bar-aligned splice up to 0.5 ms off (`adelay=NS`).
- `Track.duck` tracks go to their own bus and `sidechaincompress` against the rest. With `MasterBus.duck_depth_db` the
  duck is a speech gate (`duck_gate_chains`: `sidechaingate` 0/1 curve, 300 ms release, 7 Hz + 1.2 Hz one-poles maxed,
  `amultiply` by `1 - (1 - depth) * curve`) (`speech gate`).
- Traps: a sidechain filter ends with the shorter input (the gate key is padded with `apad`; the compressor path still cuts a bed
  off when dialogue ends first, left alone because fixing it moves every ducking graph); a digitally silent key holds
  `sidechaingate` open (1e-6 floor); `amix` silently drops a negative weight (`ends with the shorter input`).
- `ExportOptions.loudness` (`LoudnessPreset`: youtube / spotify -14, apple -16, broadcast -23 LUFS, -1 dBTP) wins over
  the bool `loudnorm`; youtube is byte-identical to `loudnorm: true` (`wins over the bool`).
- Master bus (`Timeline.master: MasterBus {volume, limiter, ceiling_db}`) is the last mix stage, after the final
  `amix` / duck sum and before `loudnorm` (`master_filters`, pure): `volume=` then `alimiter=...:attack=5:release=100:
  level=0:latency=1`, each omitted while `is_neutral` (a stored ceiling alone changes nothing) (`**The master bus**`).
- Two `alimiter` options are load-bearing: default auto-level would turn the ceiling into makeup gain; without
  `latency=1` its lookahead delays the mix by the attack and drops the tail (`load-bearing`).
- `latency` is probed: FFmpeg 4.4 has no such option and refuses the whole graph. `alimiter_latency_available()` (once
  per process, `help_lists_latency`; a binary that will not run reads as having it) decides `:latency=1`; without it
  the mix trails the picture by 5 ms. `with_alimiter_latency` pins it in unit tests / golden (`is probed, not assumed`).
- Ceiling is a sample-peak ceiling rounded to six decimals; `get_levels` reports true peak above it, so the default
  `MASTER_DEFAULT_CEILING_DB` is -1.5 dBFS (`loudnorm`'s TP target), not -1 (`**-1.5 dBFS**`).
- `for_render` / `for_delivery` / `slice` carry the master; `safe_volume` / `safe_ceiling_db` re-clamp in the builder
  (a `.kerf` never passes the clamping ops); `DiffKind::MasterChanged` keeps a master-only agent proposal from
  diffing empty and being discarded by `apply_staged` (`safe_volume`).
- The preview plays the same filter: `frontend/src/lib/alimiter.ts` ports `alimiter` sample for sample; the ignored
  `alimiter_corpus` test writes `fixtures/alimiter-corpus.json` through real ffmpeg with `limiter_filter`'s text and
  `alimiter.test.ts` replays it; bless with `KERF_BLESS_CORPUS=1`; changing the limiter text moves it
  (`The preview plays the same filter`).
- `MasterBus.true_peak` (opt-in, only with the limiter) oversamples: `aresample=<4x>,alimiter,aresample=<rate>`
  (`oversampled`). AAC under a ceiling gets `-aac_pns 0` (`holds_peak_ceiling`; `push_output_opts` writes it right after
  `-c:a aac`; `together.rs` passes the first variant's answer to every file): PNS adds +3..+8 dBTP spikes. Not probed;
  omitted without a ceiling and for other codecs; `audio_codec: None` not covered (`AAC under a ceiling gets`).
- Static loudness (`loudness_static`): `render_with_progress` runs one `levels::static_loudness_gain` pass first
  (after the preflight; skipped when nothing writes sound; `ExportStage::Metering`); `prepare_variants` preflights all
  then measures once; the pass meters the export's own graph with normalization off and fills the `serde(skip)`
  `loudness_gain_db` = target - I (+-`LOUDNESS_MAX_GAIN_DB` 30); the builder emits `volume=<g>dB` (omitted under 0.005)
  and the oversampled limiter at the target's -1 dBTP less `STATIC_TP_MARGIN_DB` 0.3. Off = byte-identical; a cancel
  during the measure is `Cancelled` (`**Static loudness**`).
- Crossfade is constant-power: an `afade` wholly one half of a real pair (`ClipFx::afade_in_power`, `tail_power`, set by
  `transition_fx` only for a dissolve / motion transition from an adjacent partner with no clip fade or dip folded in)
  gets `:curve=qsin`. Everything else stays linear: fade from black, mixed fade, and pairs playing the same samples on
  both sides (`clip_timing::same_sound`) or a de-click splice (`SPLICE_CROSSFADE_S`): qsin would swell +3 dB there
  (music-fit render tests are sample-exact on it). `audio-mix.ts` mirrors it (`crossfadeIn`, `fadeInTimes`,
  `crossfadePair`) (`constant-power**: an`).
- Audio is re-based to the clip before `adelay` so its `afade`s stay clip-local; video fades are timed on the timeline
  (`start` = `timeline_start + ...`, since `setpts` already moved the frames), else a later clip's fade-out blacks it
  out (`Video fades are timed on the timeline`).

## Captions, chapters, range, progress, deliveries

- `embed_captions` muxes a soft subtitle stream (`mov_text` mp4 / mov, `webvtt` webm, `srt` mkv; `validate_export`
  refuses other containers), not the default track (`-disposition:s:0 0`), since captions are also burned in unless
  `skip_burned_captions` (`**Captions and chapters ride along**`).
- `caption_sidecars` writes `.srt` / `.vtt` only once the render completed (failed / cancelled exports never touch
  them; a write failure is reported after the render is safe: written outside `discarding_partial`); an existing one
  is refused up front unless `overwrite_sidecars` (`check_caption_sidecars` checks every variant first); captions
  requested with none in the cut are refused (`only once the render has completed`).
- Builders stay pure: `side_files::prepare` writes SubRip / FFMETADATA temp files named in render-only `#[serde(skip)]`
  `subtitle_input` / `chapters_input`; unset adds nothing; `export_resource` skips those inputs (`names them in the
  render-only`). `ExportOptions.range` builds from `Timeline::slice(start, end)` (`renders only a span`).
- Export progress (`cli/progress.rs`): `fraction` spans the whole export; stages own shares (`metering_share` 0.08 with
  a picture, 0.4 sound alone); `waiting` wins over `stage`. ffmpeg prints no `-progress` block until the first packet,
  so `run_progress_with` says `starting` at spawn and each second; `out_time_us`, else `out_time_ms` (microseconds
  despite the name), else `out_time`, `N/A` is no position; held at 99% until ffmpeg exits; eta from the first output
  (`**Export progress**`, `prints **no** `-progress` block`).
- `EXPORT_STALL` is judged on movement, not on lines (`run_progress_stalling`): 300 s without one more frame or later
  output position, because FFmpeg 7+ prints blocks regardless; a slow run that moves is never stopped; errors say where
  it stuck and a failing part names what it draws (`part_drawing`) (`judged on movement, not on lines`).
- Open end: FFmpeg 4.4.x single-pass `loudnorm,aresample` fails at the encoder (`aformat=channel_layouts=` after it
  works); a fix moves every loudnorm golden graph (`Cannot select channel`).
- All delivery frames in one run (`cli/together.rs`, `render_variants(.., together: true, ..)`): `build_together_args`
  (pure) builds each variant with `build_filter_complex_with` over `for_delivery` + `export_timeline` then `relabel`s
  into `d<k>_...`. Filter instance names are not labels, so name them apart (`instances`): frame `k > 0` names a keyed
  reframe `v360@d<k>c3` and its `sendcmd` targets that name, because `sendcmd` delivers to the first instance of a
  name only (`sendcmd` delivers to the first instance).
- Clips' inputs must be the same argv for every variant (`for_delivery` moves crops and captions only); shared pictures
  `split`; the mix is the first variant's alone (`[d0_outa]asplit`); `sinks_from_end` locates outputs after
  `tune_inputs` and the spill (`must be
  the same argv`). `render_together` mirrors `render_with_progress` once for the lot (`discard_partial` of every output
  on anything but `Completed`), `cpu::limit_outputs_args` gives each encoder `shared_threads(threads, K)`
  (`render_together`).
- Rendered one after another instead (`renders_together`): single variant, two-pass, hardware encoder (session cap /
  second VAAPI upload unproven), audio-only container, two variants on one file name; `build_together_args` is `None`
  for a cut with no picture. A cut that needs parts refuses `together`; the single-delivery argv is untouched
  (`Refused (`renders_together`)`).

## Long exports in parts (`cli/segments.rs`)

- One run opens every clip of the span and each decoder holds frames in flight (~95 MB per megapixel), so big reels
  ran out of memory with no frame out; `plan_export` splits only when the whole span is over `SEGMENT_LIMITS` (32
  megapixels of open decode area, 16 inputs, 6 blended clips; a still counts a quarter). Below it the argv is the
  single run's byte for byte. `KERF_EXPORT_PARTS=off` never splits, a number replaces the 32; gif, audio-only, VAAPI
  and uncuttable cuts stay whole (`only when the whole span is over`).
- The blend limit is the backstop for what cannot fold and for nested folds: `PlanClip::fork` is `Own` or a folded `Run`
  (up to `RUN_FORKS`); a part nests at most 6 forks (`PLAYBACK_LIMITS` too); the golden parts oracle passes `blends:
  usize::MAX`. A cut for the forks is made only if it lowers the heaviest part (`only if it lowers`).
- `plan_parts` (pure): greedy from the front, cutting where a clip's last drawable frame has gone or the next one's
  first comes (`touch`), preferring a cut no clip straddles. A straddling clip is an input of both parts, read from its
  own start in both with its own seek / trim / fades / keys. A footage clip is not cut through if a part would decode
  more than `preroll_s` (20 s) (`-ss` rebases away the first frame's timestamp). A still image is restarted by each part
  (`Restart`: `-f image2 -pattern_type none -framerate`, `loop`, `setpts=N+k`, `trim=end_frame=n`) unless something keeps
  a count across frames (keyed crop / moving still `perspective`, `vignette` dither, speed, reverse) (`**Where to cut**`,
  `A **still image** costs`).
- Time is the export's: a part's canvas is frames `first..first+n` of the whole export's (`color=...:d=(n-0.5)/fps`,
  `setpts=N+first`: frame-counted, half a frame of slack), chains come from `build_filter_complex_with` given the whole
  cut's `ClipFx` table and a plan over the part's clips (`Scope` / `InputScope`), the part shifts to zero only as it
  leaves the graph (`setpts=PTS-STARTPTS`), nothing is rebased. Titles read the whole export's directory from the first
  picture the part needs (`-start_number`). `canvas_frames` reproduces `color=d=`'s count. A clip's chain is the whole
  run's: a restarted still keeps its asset size for `cover_overflows`; tests
  `a_part_fits_and_zooms_a_clip_in_the_order_the_whole_export_does` + the `golden.rs` corpus (`**Time is the export's.**`).
- The join is lossless and the encode is one: each part is FFV1 (`-coder 1`, Matroska, `-pix_fmt yuv420p`, every part the
  same format: a mid-stream pixel format change made the concat reader lose frames) of exactly its frame count (checked
  from `frame=`); the last pass reads them with the concat demuxer (exact `duration`s), `settb=expr=1/fps,setpts=N`, and
  encodes once with the user's codec / rate control / two-pass / faststart / hardware / container options via
  `push_output_opts`. Not per-part final codec + `concat -c copy` (identical SPS/GOP/rate control needed; useless for
  two-pass, gif, hardware). Intermediates in `<cache>/kerf/segments/<pid>-<n>` (`KERF_SEGMENT_DIR`), removed on success /
  failure / cancel / panic (`KERF_KEEP_PARTS` keeps; dead processes' swept at start) (`The join is lossless`).
- Sound is one pass: the last pass mixes the whole cut from the clips that have any, each opened `-vn`
  (`InputScope::sound_only`; `InputPlan::base` shifts `[N:a]` past the concat input) through the export's own audio graph,
  so ducking, master, `loudnorm`, static gain and encoder see the whole program (`**Sound is one pass.**`).
- Parts and last pass run under one foreground `cpu::lease` taken first, each as `run_ffmpeg_progress_counted` with the
  stall watchdog and cancel; each part's decoders are `-threads 4` (`cpu::cap_input_threads`: `limit_args`' head flags
  reach only the first input's group). `ExportProgress.part` (`ExportPart`) rides beside `stage`; parts take 55% of the
  bar; a part's `Finishing` is told as `Rendering` (`cap_input_threads`).
- Open: cutting a long footage clip mid-way; `together` in parts; pipelining parts into the encode; free-space check
  before parts start. Golden `golden/parts.txt` (`Not done: cutting a long footage clip`).

## Levels and export read-back

- `Project::levels_inputs` -> static `measure_levels` -> `engine::mix_levels` (lock-free split): one ffmpeg pass over the
  export's own audio graph; `build_filter_complex_metered` = `build_filter_complex` plus taps (byte-identical with
  metering off): per-track `ebur128@t<i>` on the strip output (before duck bus and master), the finished mix tapped last
  after master and `loudnorm`; every tap reads sample + true peak (`**Levels**`).
- `MeterParser` reads stderr line by line (short-term max `-120.7` until the first 3 s window is `None`; the `Summary:`
  block is one log call so only its first line is prefixed; `-inf` / the -70 LUFS gate floor are `None`). Whole-file:
  `cpu::lease` + caps; 120 s stall watchdog and `cancel` kill the child (`reads stderr line by`).
- `Levels.notes` takes the master bus (`Levels::new(.., &timeline.master)`, `level_notes`): over -1 dBTP with limiter off
  says enable it with true-peak; already on says turn on `set_master_true_peak` or lower the ceiling by overshoot + 0.5 dB
  (`LEVELS_CEILING_MARGIN_DB`, clamped to the floor; at the floor lower the master); with `true_peak` on only the lower
  ceiling (the old advice looped an agent). A track over 0 dBFS pre-master is told so without claiming a clip (float
  graph). `levels.ts` `levelNotes` mirrors the words exactly (`takes the **master bus**`).
- An export reads back what it wrote (`cli/written.rs`, `render_measured` / `render_variants_measured`, `measure_export`):
  after a completed export (`render_with_progress`'s own contract unchanged) the file is probed (`ffprobe -show_format
  -show_streams`, 60 s) into an `ExportReport` and its first audio stream metered (`ebur128@file=peak=sample+true`)
  into `loudness` beside `target` and `true_peak_over_db` (compared to a tenth, only when over). It never fails the
  export: a probe / meter failure or a stop is `problem`; Stop keeps the finished file. It is `ExportStage::Measuring`
  in the last `VERIFY_SHARE` 3% of the bar and ungated (`cap_args`, niced, no heavy-job lease) so a proxy cannot hold
  a finished export's answer; a multi-format run that did not complete returns no reports (`An export reads back what it
  wrote`, `It never fails the export`).
- `stream_mix_pcm` (`build_mix_pcm_args` = `build_filter_complex` without meters, `-f f32le pipe:1` via
  `pump_pcm_cancellable`) is the other reader of the graph (band balance, `engine-media.md`); ungated, `cap_args`,
  niced (`**`stream_mix_pcm`**`).

## Masks, fades, scrubbed still, scrub windows

- `Clip.mask` is deliberately one primitive that composes with the track stack (a blurred face is a duplicated shot above,
  blurred and masked), a single linear filter (`mask_filter`: a `geq` rewriting only the alpha plane; no graph branch)
  (`**`Clip.mask`**`). A constant opacity below 1 comes after it and maps the mask's alpha level by level
  (`lrint(level * opacity)`, `kerf_core::layer_geometry::ffmpeg_alpha_of`); a keyed opacity shares the mask's `geq`
  (`kerf_core::mask_alpha_plane_keyed`); the alpha plane is also computed without FFmpeg (`mask_alpha_plane`), drawn by
  the GPU once per `(mask, size)` (`lrint(level * opacity)`).
- `geq` is not the identity on colour planes on every build: 6.1's bilinear read clamps to `w - 2` / `h - 2` (last
  column / row are the ones before); `geq_edge()` measures it (`is not the identity on the colour planes`).
- The scrubbed still draws fades, dips, dissolves, slides and pushes as the file does (`engine/cli/still_fx.rs`, used by
  `timeline_frame`, the cover, the preview fallback): `still_layers` takes `active_video_clips_with_tails`; per layer the
  `ClipTiming::fades` still changing the picture at output frame `k = Rational::frame_containing(t)`, slide / push travel
  at `ffmpeg_frame_time(k)`. A fade runs where the export runs it (after rotation, ahead of a moving zoom) on a frame
  restamped as frame `k` of a `1/fps` stream (`settb=expr=den/num,setpts=k,fade=...,setpts=PTS-STARTPTS`: `fade` counts
  frames and `overlay` shows a secondary frame only from its timestamp on) (`draws fades, dips, dissolves, slides and
  pushes as the file does`).
- A chain with no alpha plane ends `format=yuv420p` after the fades (`yuva420p` for an alpha source); a clip that entered
  through a dissolve is on an alpha plane for good (`xfade_in > 0`) so later fades / dips out run on `yuva420p` too:
  decided from the clip's whole `fades()` list while a fade is in progress (`StillFades::alpha_chain`). A tail is open
  for frames whose slot starts inside its window (`tail_at`: slot start + 1 ns), not the raw `t`. A fade not changing the
  picture is left out so stills outside transitions keep their argv (`ends `format=yuv420p` after the fades`, `tail_at`).
- The still's frame pick is unchanged (`-ss`, not the `fps` filter's): moving footage can differ by a source frame; a dip
  to white is up to 2 levels off outside its fade. Golden `still-fade` / `still-tail` / `still-travel` (`Everything that
  renders a still now returns`).
- Scrubbed frames (`engine/cli/scrub.rs`, `still_cache.rs`): `ScrubFrames` (one per process, `AppState`) pays the process
  once per window. `frame()` quantises `t` to the cut's frame grid (`Rational::frame_containing`, the slot's start as the
  export draws it) and serves a bounded LRU (`KERF_SCRUB_CACHE_MB`). The key is the still's own argv (`StillKey`: hash of
  `build_still_args_rendered` + size and mtime of every `-i`), so nothing needs invalidating (`**The key is the still's own
  argv**`).
- A miss starts one window (export graph over `Timeline::slice`, `-frames:v` 24 / `KERF_SCRUB_WINDOW`, JPEGs filed under
  the key of the still each stands for); a request the running window will draw (asked off the lock) waits, any other
  kills it ("latest wins"); within 8 frames of a finished window's end the next starts (`read_ahead_frames`). A window
  starts 1 frame before the asked one (2 after going back); short first windows were measured worse. Spawned off the lock
  (`Reading::Window`); killed by `stop()`, `clear()`, an edit or jump, a reaper (10 s / 5 s) and `Drop`; `stop` / `clear`
  bump an epoch so waiting requests start nothing; a window that died by itself is not restarted for the same request
  (`**A miss starts one window**`, `A window starts 1 frame before`, `epoch, so a request already waiting`).
- Window frame = the still, or the export where the still is off (`scrub_rendered.rs`); on 6.1 the still is the odd one
  out. `Snapshot::plain` refuses (one-shot draws it): fade, dissolve, slide, push, tail; a source not a whole multiple of
  the cut's rate; the last frame of a title (`drawtext` `between` includes `end`); an animated SVG title on screen; 360
  and HDR (`Window frame = the still`).

## `clip_timing.rs`, denoise

- `clip_timing.rs` (pure, `pub(crate)`, no engine dependency): `ClipFx`, `transition_fx` (indexed by flat clip storage
  index over the `for_render()`ed timeline and the graph's own asset list; `clips_with_fx`), `clip_source_window` /
  `clip_seek`, `is_head_padded_proxy`, `ClipTiming` (`window` / `fades` / `motion_keys` formatted by builders;
  `enabled(t)` / `motion_at(t)` / `FadeStep::progress_at_frame` evaluated by frame renderers: one decision, two
  readers). `fades` is video-only (`audio_clip_chain` shares only duration, window, seek) (`What a clip does in time is
  `clip_timing.rs``).
- The number is not what FFmpeg does with it: `enabled(t, fps)` is the overlay `enable` predicate (opened a frame early,
  closed at `end`; necessary, not sufficient: drawn from the first frame's slot to `end_frame`, nothing at `end`);
  graph expressions evaluate at `ffmpeg_frame_time(k) = k * (den / num)`, an ulp off `k / fps`, which dropped the first
  frame of a third of aligned clips at 24 fps (hence the slack); a `fade` counts frames (`S = round(st * fps)`,
  `N = round(d * fps)`). Exact rationals are for the frame pick alone: `Rational` is the rate as FFmpeg parses the text
  (`av_d2q(x, 1001000)`; `29.97` is 2997/100), `frame_time(k)` for the graph, `exact_time(k)` for the pick; pinned by
  `engine/cli/rendered.rs`; any change must go through the golden oracle (`not what FFmpeg does with it**`).
- Denoise (`AudioEffect::Denoise`, `engine/cli/denoise.rs`) = `afftdn=nr=..:nf=..` (`:tn=1` only when floor tracked).
  Since FFmpeg 5.1 `afftdn` outputs the window two hops back, so the sound comes out `2 * (sr / 80)` samples late and loses
  its last two hops; 4.4 / 5.0 keep time but not length. `afftdn_delays()` (once per process; unrunnable binary reads as
  delaying; `with_afftdn_delay` pins tests) picks the wrap: delaying = `asetpts=PTS-STARTPTS,apad=pad_dur=0.024995,
  afftdn=..,atrim=start=0.024995,asetpts=PTS-STARTPTS` (rebased because `atrim` reads timestamps and the preview's `-ss`
  decode does not start at 0); not delaying = `adelay=delays=40:all=1,apad=pad_dur=0.04,afftdn=..,atrim=start=0.04:
  end=<len+0.04>` (40 ms = whole samples at every rate 8..192 kHz), cut back by `audio_effect_filter`'s `len` /
  `ClipTiming::duration` (the preview's `audio_effects_filter` has none; `audio_pcm`'s `-t` cuts). Only the delaying text
  is in the golden. Runs at the source rate after the tempo; level-dependent (`is_dynamic`) (`**since FFmpeg 5.1 its
  output`).

## Keyframe expressions, keyed zoom, placement, behaviours

- `video_clip_chain` / `audio_clip_chain` realize effects, audio effects and transform keyframes (moving zoom / position
  via placement `perspective`, held via `scale` + overlay x/y, rotation via `rotate`, opacity via `geq`) from
  piecewise-linear `keyframe_expr` over clip-local time (`also realize`).
- An expression may nest about 100 levels (libavutil `av_expr_parse`; the next is `EMFILE`, reported `Invalid argument`;
  the 101st on 4.4.2, a level sooner on 9.0.2) and an `if(lt(..))` chain is a level per point (twelve per eased
  segment). Above `KEYFRAME_TREE_POINTS` (24) `keyframe_expr` writes a balanced binary tree (`if(lt(t,t_mid),left,
  right)`); up to it the chain is kept (golden). The sweep evaluator enforces `NESTING_LIMIT`. Anything that writes an
  expression per input item must be a balanced tree (`An expression may nest about 100 levels`, `must be a tree too`).
- The keyframed zoom is the one stage that changes size frame to frame and almost nothing after it can follow: format
  negotiation inserts a fixed-size converter, `geq` / `rotate` / `eq` / `gblur` / `zscale` read the size once at
  configure. So when `Clip::zoom_animated()` (position / rotation / opacity-only keys keep their chain),
  `video_clip_chain` puts the zoom last: crop, fit, `setsar`, `fps`, tone-map, `eq`, effects, `format=yuva420p`, chroma
  key, mask / opacity `geq`, `rotate`, fades, then the zoom (now the placement), evaluated at the output frame's time.
  Costs: effects act at fit size and the zoom magnifies the result; those filters run at fit size even when the zoom
  shrinks it (`KERF_ZOOM_COST=1`) (`**The keyframed zoom is the one stage`, `puts the zoom **last**`).
- The scrubbed still follows (`still_clip_chain(.., zoom_last)`); every other clip zooms first, so there is a step where
  keys stop being equal (1.6 vs 1.6000001), inherent to keeping held / unkeyed clips byte-identical
  (`a step where the keys stop being equal`).
- `rotate=...:fillcolor=none` is not transparent (corners keep the reused buffer; a moving rotation leaves earlier poses):
  the keyed rotation fills `black@0`; the static rotation keeps `none` (`**`rotate=…:fillcolor=none` is not`).
- Rendered oracle `keyed_zoom.rs` (`KERF_ZOOM_KEEP`, `KERF_ZOOM_VERBOSE`) takes `Clip::fit_order` into account (a static
  zoom / position over an overhanging Cover picture is the whole cover zoomed and cut by the canvas) (`KERF_ZOOM_KEEP`).
- The plan mirrors the order by refusing: Motion plan a moving zoom (`Unsupported::KeyedZoom`) until `GpuCaps::keyed_zoom`
  (`kerf-gpu` sets it); Still plan without the cap a zoom with grade / rotation / opacity fade / mask / effect in front
  (`Unsupported::ZoomBehind`); a pure zoom is always drawn; with the cap rotation, opacity below 1 and a dip to white
  in front are still refused; `Animated.zooms` is `Clip::zoom_animated()` itself (`Unsupported::KeyedZoom`).
- Tiny scale: `scale` reads width / height evaluating to 0 as "unset"; below `TINY_SCALE` (0.01: static zoom, any key,
  the still) sizes are `max(1, ...)` (`zoom_scale`) (`**A tiny scale snapped to full size**`). HDR: `zscale` refuses sizes
  not divisible by subsampling, so an HDR clip's Contain fit says `force_divisible_by=2` and a constant zoom is
  `max(2,2*trunc(x/2))` (`**HDR footage aborted on an odd size**`). Alpha sources: a chain with no alpha plane of its own
  must not end in `format=<pix_fmt>`; `ClipFx.alpha` (`Asset::has_alpha`, unrecorded = not) ends it in `format=yuva420p`
  (`**Alpha sources were`).
- A moving zoom or position is placed at sub-pixel positions (`placement_filters`, when `Clip::placement_animated()`):
  ends `format=yuva420p` (unless alpha), `pad` (transparent, centred, at least canvas and 2 px past the picture), `perspective=
  ..:interpolation=linear:eval=frame` (corners: canvas `X` reads the picture at `(X + 0.5 - cx)/s + w/2 - 0.5`), `crop`
  to the canvas round the same centre; pad / crop offsets are even whole numbers (`2*ceil`, `2*floor`) so `perspective`
  computes the same window; time is `perspective_time` off a pinned `fps` (`keyed_crop_fps`). A fade on a chain with no
  alpha plane gets `format=<pix_fmt>` ahead of the placement. The scrubbed still runs the same `placement_chain`
  (`still_clip_chain_graded(.., placement)`); a held zoom / position keeps its graph (`**A moving zoom or position is
  placed at sub-pixel positions**`).
- `kerf-gpu` draws the placement (`gpu.md`, "The placement"); odd-height pictures, RGB (and 4:4:4 / 4:2:2 where the
  conversion differs), rotation, translucency, dip to white ahead of it stay FFmpeg's (`**`kerf-gpu` draws this chain**`).
- Per-property channels: a clip's transform is built number by number from `Clip::is_keyed`, not "animated": keyed scale
  = `scale eval=frame` (last, when it moves), keyed rotation = `black@0` `rotate`, keyed opacity = `geq` alpha; every
  unkeyed number comes from the static transform; overlay `x` / `y` are each the curve or static offset
  (`curve_or_static`); any keyed number makes the clip "animated" for the pad / centre decision, so a bundle clip is
  byte-identical (`**Per-property channels in the graph.**`).
- Live behaviours (`engine/cli/behavior.rs`, `core-model.md`): every transform number that becomes a time expression goes
  through `driven_expr(clip, prop, tvar)`: curve (or static) then each behaviour, `(curve)+(term)` for position /
  rotation, `(curve)*(1+term)` for zoom (`u` = `((tvar-<timeline_start>)+<phase>)`). Call sites: overlay `x`/`y` of an
  animated clip, `placement_filters`, held-keyed `zoom_scale`, keyed `rotate`, `still_motion`'s pose. A behaviour that moves
  nothing is left out; the text is the model's arithmetic operation for operation (`Behavior::term`) with nothing
  transcendental printed as a number (so export / stream text is machine-independent; `sweep`'s evaluator holds it to
  `property_at`) (`**`driven_expr(clip, prop, tvar)`**`).
- The chain is chosen by `Clip::is_driven` / `rotation_turns`, not `is_keyed`: sway / spin take the `black@0` `hypot`
  `rotate`; breathe / drift / shake the moving-zoom / sub-pixel placement; a still image the moving-still route when it
  covers the frame at every frame. `still_motion_for` judges every frame of a clip with behaviours at the times the
  export evaluates the pose (output frame `slot + k`, `slot` from `keyed_crop_fps`, less the clip's start; not `length /
  count`) and leaves clips over 20000 frames to the general chain. Terms add a few levels (13-key eased tree + all five
  kinds stays under 100: tested with `eval`); Windows caps a command line at 32 K (`The chain is chosen by`).

## Grade (`Clip.grade`, `model/grade.rs`, `engine/cli/grade.rs`)

- Order LUT -> curves -> wheels (same in `video_clip_chain` and `still_clip_chain_graded`), after tone-map and `eq`, before
  effects and any alpha plane; each part omitted at neutral (no golden moves) (`**LUT → curves → wheels**`).
- Curves are `lutrgb` expressions, not a file: a balanced `if(lt(..))` tree down to one segment's Horner cubic (the
  model's Fritsch-Carlson constants), `round(maxval*clip(ld(0),0,1))`, so the stored integer is the model's at any depth;
  not `curves` (own overshooting spline), not `lut1d` (truncates, needs a file) (`**Curves are `lutrgb` expressions**`).
- LUT = `lut3d=file='..':interp=tetrahedral` over Kerf's copy (`color_lut.rs`): `.cube` parsed strictly (3D only, size
  2-65, unit domain, `size^3` finite rows, 32 MB cap; FFmpeg ignores `DOMAIN_MIN` before the size and misreads CR-only
  files), rewritten to `<cache>/kerf/luts/<fnv>.cube` with intensity baked in, path escaped like `fontfile=`. A missing
  or broken file is left out of a preview / still with a warning and refused by an export (`check_export_luts` in
  `export_preflight`); builders read the disk so they run off the project lock; copies pruned on the run's first lookup
  (30 days, then LRU past 256 MB); parse errors give the line and a fixed message, never the file's text
  (`**The LUT is `lut3d=`, `and **refused by an`).
- Wheels are `colorbalance`: puck `(x, y)` -> `(x, -x/2 + sqrt3/2 y, -x/2 - sqrt3/2 y)` + lightness, held to +-1, rounded to
  1e-4, zeros omitted; the filter weighs ranges by pixel max + min (`**Wheels are`).
- All three are RGB filters: FFmpeg 6.1 converts back as BT.601 and leaves the tag `unknown` (9.0.2 keeps both). For a
  tagged picture (`ClipFx::frame_matrix` / `model::frame_matrix`) the grade ends in `scale=out_color_matrix=<m>,
  format=yuv444p|..10le|..12le|..16le,setparams=colorspace=<m>`; untagged = BT.601 both ways, nothing added
  (`converts back as BT.601`, `the_grade_text_is_pinned`).
- The GPU draws this chain (`gpu.md` "The clip's grade"): filter order, `frame_matrix` rule (`model::frame_matrix_tag`,
  the plan's `grade_tag`) and the closing `scale=out_color_matrix` are what `GradeRoute` and `kerf-gpu/tests/grade.rs`
  are held to; `kerf_core::grade_filter_chain(grade, matrix)` is the shared text; change one and the parity cases move
  (`**The GPU draws this chain**`).

## Keyed colour, mask, crop, volume, moving still

- Keyed colour = `eq` with `eval=frame` (`eq_filter_keyed`): each keyed number a quoted expression of `t` (clip-local =
  `(t-start)`); temperature `gamma_r='1+0.3*(..)'` / `gamma_b='1-0.3*(..)'`. Without `eval=frame` it fails at the second
  frame. `keyed_color_value(clip, T)` is the same in Rust; the Motion plan carries it and the GPU builds `eq`'s tables
  from it (`**A keyed colour is an `eq``, `keyed_color_value`).
- Keyed mask = the mask's alpha `geq` (`keyed_mask_alpha`) with each keyed curve evaluated once a row (`st(slot,..)`
  at `X` 0, slots 0-4 in `Property::MASK` order, keyed opacity 5; every other pixel `ld(slot)`); a keyed feather is
  guarded `if(lte(F,0.000001),lte(d,1),clip((1-d)/F,0,1))`; a static mask's argv is untouched (`**A keyed mask**`).
- Keyed crop = `perspective` (`crop_remap_filter`, `crop_corners`, `sense=source`, `eval=frame`) reading the window of
  `Clip::crop_window_at`, placed after the static `crop` and `setpts`, before the fit `scale`. `perspective` has only a
  frame count, so the `fps` ahead of it is hoisted there with `start_time` pinned (`keyed_crop_fps`; `fps` rounds half
  away from zero, `frame_pick::rescale_near`). A crop whose zoom never leaves 1 is not animated (`crop_animated`). A
  per-pixel `geq` crop was ~100x slower; each curve is a balanced `if` tree written 8x (zoom) / 4x (pan) per corner, so
  graphs grow with keys and spill to a file (`**A keyed crop**`).
- Parity of keyed mask / crop was measured on 6.1 only (4.4 and 9.0 unavailable) (`4.4 and 9.0 were not`).
- Moving still (`cli/still_motion.rs`, `StillMotion`, pure): a still image whose zoom / position / animated crop moves
  (`Clip::picture_moves`) is scaled once ahead of the `loop` to the smallest size covering the greatest magnification
  (`HEADROOM` 2%), cut once, then one `perspective` per frame + `crop=W:H:0:0`; the scrubbed still uses
  `StillMotion::at`; the input gets `-framerate` alone; a part reads it from its start (`restartable`); the GPU draws it
  from FFmpeg's copy (`gpu.md` "Moving stills"; `kerf_core::still_motion`, `cli::StillRoutes`). Eligibility
  (`still_motion_for`, every frame): an image, no rotation / mask / reframe / HDR / blur / sharpen / vignette, known size,
  crop never wider than the picture, the picture covering the whole frame at every frame under the clip's fit order;
  else the general chain. Golden `ken-burns`, `still-ken-burns`, `preview-ken-burns`, `ken-burns-scaled-once`
  (`**A moving still**`, `**Eligibility**`).
- Keyed volume = `asetnsamples=n=128:p=0,volume='..':eval=frame` after `atempo` (so `t` is the clip's playing time).
  `volume` holds one gain per frame and decoders hand over 1024 samples (a fade would step audibly); `p=0` is required
  (default pads the last frame with silence). Tests keep hold steps off the 128-sample frame grid (float rounding of
  `t`). `sweep.rs` holds the export's `eq` numbers and `volume` expression to `layer.color` / `Clip::volume_at`
  (`**A keyed volume**`).

## Filter-graph safety and text

- Any expression containing commas must be quoted in the filter value; an unquoted `overlay=x=` / `drawtext` x/y once
  made every animated clip abort with `No such filter` while every unit test (which asserted on the string) passed, so
  render the graph when its shape changes (`**Any such expression must be quoted in`).
- Colours pass `valid_color` (`engine/colors.rs`; else a safe default; a `.kerf` never goes through `validate_export`):
  ffmpeg's own grammar: a name from `av_parse_color`'s table (`grey`, `darkgrey`, `lightgray`, `rebeccapurple`,
  `transparent` are not in it), `#RRGGBB[AA]` / `0xRRGGBB[AA]` (lower-case `0x`; `#RGB` refused), optional `@alpha`
  `0..=1` or `0x0..0xff`; never `random`. `pix_fmt` / `scaler` / `gif_dither` are allow-listed; overlay text escapes `%`
  as `\\%` (a bare `%` makes `drawtext` render nothing with exit 0) (`ffmpeg's own
  grammar`).
- Text overlays are `drawtext`'d on the final composite; the still / preview path samples `Clip::transform_at` and draws
  them statically (`**Text overlays**`).
- A `drawtext` value is read by three parsers and `escape_drawtext` writes for all three: text expansion (`\` -> `\\`,
  `%` -> `\%`), the option string (`\` -> `\\`, `'` -> `\'`, `:` -> `\:`, an end blank -> `\ `), then the filtergraph
  (`'` -> `'\''`). Line breaks (LF, CR, CRLF as one, U+2028/2029) become one space, other control characters but tab are
  dropped. `fontfile=` is escaped the same minus expansion (Windows path `C:\Fonts\a.ttf` -> `C\:\\Fonts\\a.ttf`, not run on
  Windows). Test `drawtext_text_is_what_the_user_typed` (`is read by three parsers, and `escape_drawtext``).
- Every overlay names a font file (`fonts::resolve_text_font`): the installed family, else the shipped Geist
  (`crates/kerf-core/assets/fonts`, written to `<cache>/kerf/fonts/<name>-<digest>.ttf`, else a per-user `0700`
  `kerf-fonts-<uid>` dir, refused if someone else's or open to others), checked on every use (a `stat`, bytes compared only
  on change, never trusting a length), rewritten if the cache was cleared / `/tmp` swept, a failed write not remembered:
  a bare `drawtext` asks for "Sans" (nothing without fontconfig, as the Windows / macOS bundles ship). Greek and CJK draw
  as missing-glyph boxes. Golden pins the default path (`fonts::with_pinned_default_font`); `text_raster_graph` is the
  same for the GPU rasteriser (`**Every overlay names a font file**`).

## Animated SVG titles (`Timeline.svg_overlays`, `engine/cli/svg_layer.rs`, `svg/raster.rs`; SMIL subset in `core-model.md`)

- ffmpeg never sees an SVG: resvg draws `SvgDoc::at(doc_t)` into a PNG and ffmpeg `overlay`s it, so export, still, stream
  and contact sheet draw one title from one rasteriser; `raster::render(svg, pw, ph, opacity)` fits "contain", clipped to
  its viewport, alpha pre-multiplied by opacity (`ffmpeg never sees an SVG`).
- Fonts via `fonts::FontSet` (`raster::render_with`; `render` uses `fonts::svg_fontdb()`): the project's embedded fonts the
  title names (loaded first, so they beat an installed face of the same family), then system, then shipped Geist (stands
  for serif / sans-serif and any missing family); `fonts::replace_embedded` / `register_embedded` change the set; a
  `FontSet` is immutable, builds once outside the registry lock. Template titles are already glyph paths
  (`svg/outline.rs`) so they draw the same everywhere (`the open project's **embedded** fonts`).
- Safety: an `<image>` `href` that is not `data:` is nothing (no file, no network); a `data:` image is decoded only after
  its header says within 16 Mpx (`svg/images.rs`, `imagesize`; decoders allocate what a header claims); `render` refuses a
  picture over `cost::MAX_FRAME_OPS` (2e9 per-pixel ops). The estimate walks usvg's tree as resvg draws it (root
  transform x every group's `transform`; mask / pattern / `feImage` restart from the transform resvg gives them; layers
  held to `max_filter_bbox` 5W x 5H), runs on the static document of that moment (`at(t)`) so a title growing into a heavy
  filter is refused at that frame; a too-large pattern tile is refused. resvg cannot be stopped mid-picture, so one
  picture is bounded to seconds and cancel / deadline is checked between pictures (`60,000 x 60,000`, `Bounded, not
  interruptible`).
- Draw order: clips, SVG overlays in list order, text, captions on top; titles end on `vtext` / `vcomp` (gif) / the final
  pad; a graph without titles is byte-identical (`**Draw order**`).
- Export / stream read a numbered sequence: `-f image2 -framerate {fps} -i {dir}/%06d.png` after the clips' inputs
  (`svg_layer::push_inputs`, `%` in the dir escaped `%%`; the levels graph adds none). Frame `j` = document at `doc_t0 +
  j/fps` for exactly the frames the overlay's `between(t,start,end)` is true on (`Rational::frame_time`, not past the
  cut's last frame). Chain `settb=expr=den/num,setpts=N+k0` then `overlay=x:y:eof_action=pass:enable='between(..)'`:
  counted in frames, not seconds (`PTS-STARTPTS+k0*den/num/TB` truncates in doubles, a frame off). `x` / `y` =
  `round(x*W)`; a rectangle off the frame or over 50 Mpx is left out. No `-hwaccel` on the image2 input;
  `export_resource` keeps such an export on the cores' slot. A title at / past the cut's end or at a non-time position is
  dropped before its frame search (`window`, clamp 1e9 s); `start` / `end` held to 24 h (`MAX_SVG_SECONDS`)
  (`**An export or a stream reads a numbered sequence**`).
- A document that cannot move is one picture (`Kind::Held`; `svg::may_animate(src)` false when the sanitised text has no
  animation element, a text test so builders stay pure): `-f image2 -pattern_type none -loop 1 -framerate {fps} -i
  {dir}/000000.png`, keyed by artwork, size, opacity only; the loop ends with the main input. Text that mentions a
  dropped animation is still a sequence (`**A document that cannot move is one picture**`).
- Stream pictures are named for the title, not the playhead (`stream_sequences(titles, total, sliced, start, fmt)`;
  `PreviewCut` keeps unsliced titles): drawn once on the output's frame grid, keyed as an export of that cut / size / rate
  would be; a stream from a playhead on a frame of its grid reads from picture `j0` (`-start_number j0`); between frames
  keeps the exact per-playhead sequence. `prepare` draws only `SvgSeq::needed()` and writes `complete` only when every
  picture is there (`**The stream's pictures are named for the title`).
- Pictures are on disk before ffmpeg spawns (no stdin; two-pass and the software retry re-run it): `render_with_progress`
  calls `svg_layer::prepare_export` before `retry_export_in_software`, once for the lot, holding pins until the render
  ends; otherwise it takes its own foreground `cpu::lease_waiting` (`ExportProgress.waiting`, "rendering animated
  titles"), draws on `lease.threads()` and releases before ffmpeg asks; cancel polled 40x/s -> `RenderStatus::Cancelled`.
  `export_timeline` is what the builder and `prepare_export` both start from; variants prepare their own size. A request
  that would be refused is refused first (`export_preflight`) (`**The pictures are on disk before ffmpeg is spawned**`).
- The stream (`stream_preview_once`: `preview_cut` -> `prepare_preview` -> `preview_args`) draws titles on the budget's
  threads, ungated, at most 10 s from the call (`animated title too heavy to preview`), one preparation at a time
  (`PreviewSlot` on a `PreviewGate`; process gate vs a test's own; `prepare_preview_in`) and with a cancel
  (`stream_preview_until(.., cancel, on_frame)`, 40x/s, before each document and every 16 compare frames; a picture in
  flight finishes) (`PreviewSlot` on a `PreviewGate``).
- `prepare` runs no more threads than `RENDER_MEMORY` (1 GiB) allows (12 bytes/pixel + 8/pixel of the widest embedded
  image, per thread). An export's titles (`Priority::Behind`) draw with `cpu::background_thread()` (below normal when
  budget < 100%: `nice(10)` Linux, utility QoS on macOS, not `PRIO_DARWIN_BG`; Windows left alone); the stream's
  (`Priority::Normal`) do not, since Play is waiting (`**Memory and priority**`).
- Fonts are read before the drawing, outside the deadline (`fonts::warm()` in `prepare_as`; the deadline moves by the
  time it took; Setup warms them at launch). Cache pictures use `raster::encode_frame_png` (`Fast` + `Sub`; a test pins
  decoded pixels equal to `encode_png`'s); the workspace `Cargo.toml` optimises `png`, `fdeflate`, `crc32fast`,
  `simd-adler32` in dev so a debug build meets the 10 s; `encode_png` stays for the agent's contact sheet; parsed
  documents kept in `DocCache` (last 6) (`The fonts are read before the drawing`, `raster::encode_frame_png`).
- Cache: `<cache>/kerf/svg/<pid>/<key>/NNNNNN.png` + `complete` marker (else private `kerf-svg-<uid>` in temp, else unique
  `0700` `kerf-svg-<pid>-<nanos>-<n>`). `key` = SHA-256 of `RASTER_VERSION`, sanitised source, document clock at the first
  picture, rate, picture count, pixel size, opacity; not the position; not installed fonts (the dir is emptied at first
  use, `claim_dir`: a pid is reused and an old marker is not trusted). The embedded fonts the title names are in the key
  when there are any (`svg_layer::digest` mixes the `FontSet` digest; none adds nothing so old keys / golden unchanged).
  The `FontSet` is taken once at planning (`fonts::snapshot()`, `named_by`) and rides in `SvgSeq::fonts` /
  `StillSvg::fonts` so a project swapped in mid-prepare cannot draw under the neighbour's key; under `cfg(test)` the
  registry is per thread (`**The cache**`, `**The embedded fonts the title names are in the key when there are any**`).
- Frames are renamed into place whole (an interrupted run resumes); the marker is last and `mark_complete` refuses a gone
  directory; a sequence is ready when the marker and the first and last picture exist; consecutive missing frames with
  the same `at()` text are one render + hard links (`renamed into place whole`).
- Eviction: dead pids' dirs removed at first use (Windows: untouched for a day); `raster::trim` holds the total to
  `KERF_SVG_CACHE_MB` (1024) dropping LRU sequences / `still-*.png`, never a pinned one (`raster::Pin`, taken before the
  marker is looked at). Check and removal are one step: `evict` takes the pins' lock and renames the entry to `.gone-*`
  before deleting; an entry that will not rename (Windows open handle) is left whole until the next trim, never emptied
  (deleting after the lock lets a preparer re-mark a dying directory; under the lock it stalls every pin). Trim runs only
  after something was written (`**The check and the removal are one step**`).
- An argv path is the cache directory's text + `/` + names (`svg_layer::argv_root`; `SvgSeq.input`, `StillSvg.input`),
  never `Path::join` (Windows puts `\`, so argv / golden would differ by OS); `raster::with_pinned_dir` pins it in tests
  (`An argv path is the cache directory's text`).
- A still reads one PNG (`still_svgs`: overlays with `start <= t < end`; `still-{key}.png`; `-f image2 -pattern_type none
  -i path` so `%d`, `[`, `*` in the path are literal): `run_still` calls `still_titles` (calling thread, ungated, pinned)
  before spawning (`timeline_frame`, `timeline_frame_region`, `export_still`, MCP `preview_timeline`). The scrub cache
  needs no `stat` of it (`file_stamp`); `Snapshot::plain` is false from a title's first to last frame; `window_args` cuts
  titles out of its slice (`**A still reads one PNG**`).
- The compositor reads the same arithmetic (`gpu.md` "Animated SVG titles", `core-render-plan.md`): `svg_layer::sequence_of`,
  `rect_of`, `clamped_opacity`, `picture_png`, `picture_key`, `overlay_chains` (none changes a graph)
  (`**The compositor reads the same arithmetic**`). Not drawn: `animateMotion`, CSS animation, `<image>` whose `href` is not
  a `data:` PNG / JPEG / GIF / WebP (`**Not drawn**`).
- Titles on tracks (`svg_layer::clip_sequences`, `engine/cli/title_clips.rs`): the same pictures drawn over the whole frame
  (`fmt` size, opaque), one per output frame from the first at or after start to the last before end (tail included), picture
  `j` at `doc_t0 = source_in + exact_time(first) - timeline_start`; a clip on no frame still has one picture. `push_inputs`
  records `InputPlan::stamps`; `video_clip_chain_stamped` starts with `settb=expr=den/num,setpts=N+k` (`,trim=end_frame=n`
  held) in place of `trim` / `reverse` / `setpts`; the rest is the clip's own chain (a dip to white fades the title's pixels;
  a transparent title over nothing dips only its artwork). Drawn before spawn in `prepare_export` and the stream
  (`PreviewCut::pictures`); the stream's are named for the slice (grid-named reuse not done for clips). A still reads
  one picture (`still_clip_picture`); `active_video_clips` does not clamp a title's source time to an asset. A graph that
  draws no picture names no title (`drop_titles` for audio-only, levels, mix, scrub windows). OTIO writes
  `GeneratorReference.1` (`generator_kind` `KerfTitle`); EDL leaves titles out with a warning. Golden `title-clip*`
  (`**Titles on tracks**`, `A graph that draws no picture names no title`).

## 360, Insta360, seeking, stills

- `StreamInfo.projection` is detected at probe (`detect_projection`: `Spherical Mapping` side data, or an Insta360 `.insv`
  whose frame is two squares side by side; deliberately no bare-aspect guess) or set by hand (`set_asset_projection`, a
  persisted per-asset override); clips from such an asset get a default `Clip.reframe` (`Clip::for_asset`)
  (`**360 footage** is reprojected`).
- A reframed clip runs `setpts -> fps -> [sendcmd] -> v360@c{n} -> [crop]` before the fit `scale` (`fps` hoisted so 8K
  reprojects at the output rate; `crop` after reprojection since raw fisheye edge fractions are meaningless). Animation via
  `sendcmd` (each command rebuilds the remap LUT, ~32 ms at 1080p): `reframe_commands` emits only channels that move, gates
  on `REFRAME_CMD_TOLERANCE`, leads each by half a frame; values are wrapped / clamped first because `v360` silently
  discards out-of-range commands; `fov` maps to `d_fov` (`h_fov` would stretch) (`sendcmd` because`).
- A graph can outgrow argv (Linux 128 KiB per argument, Windows 32767 total): `externalize_filter_complex` spills over
  `GRAPH_ARG_MAX` to a temp file via `-filter_complex_script` or, where FFmpeg 8 removed it, `-/filter_complex <file>`
  (`graph_script_flag` probes `-h full` once). `GRAPH_ARG_MAX` / `spill_long_graph` are `pub` for the compositor's
  children; a spilled file is unique per call (two renders sharing a tag deleted each other's script)
  (`externalize_filter_complex`).
- The still samples `Clip::reframe_at` to a constant `v360`; `export_format` ignores a reframed clip's source dimensions so
  a 5760x2880 capture does not become the deliverable size (`reframe_at`).
- An Insta360 capture is a pair of files (`_00_` / `_10_`), neither 360 alone, so `Project::probe_import` stitches at import:
  `insta360_pair`, `stitch_insta360` (`hstack -> v360=dfisheye:e:..:roll=180`, lenses record upside down) into a 5760x2880
  file `<cache>/kerf/stitched/<hash>.mp4` keyed by both lens files (HEVC via a verified GPU encoder, the frame is too wide
  for h264 NVENC, else libx264 CRF 15); the asset describes that file (projection forced `Equirect`, the CLI cannot write
  an `sv3d` box), originals in `Asset.source_paths`. Progress via `import-progress`; `stitch_insta360_with` /
  `Project::probe_import_with` take a cancel (polled waiting for the other lens's stitch, the slot, and while encoding);
  serialized per pair (a claim on the cache file in a set + condvar, never a mutex across the encode; waiters poll cancel
  every 100 ms and take the other's result); dedupes via `insert_or_get_asset` (`Insta360 capture is a *pair* of files`,
  `is serialized`).
- Each input gets a per-input `-ss` to its clip's source-window start (`clip_source_window` / `clip_seek`, frame-accurate
  against seek-relative `trim`) (`per-input
  `-ss` fast-seek`).
- Still images (`is_still_codec` + no audio + sub-second duration -> `StreamInfo.image`, `DEFAULT_IMAGE_DURATION`): no source
  timeline, input is its one frame (`-framerate fps`, no `-ss`), decoded once and held by `loop=loop=N-1:size=1`
  (`still_pictures`), in-graph `trim` stays absolute (seek forced 0); `-loop 1` re-decoded the file per output frame
  (`**Still images**`). A frame grab or contact sheet at the start must not seek at all (`seek_unless_start`): on a JPEG
  still any `-ss`, even 0, makes ffmpeg exit 0 having decoded nothing (`seek_unless_start`).
- The composited still's `-ss` is spelled to the microsecond (`kerf_core::seek_arg`, `{:.6}`, also used by `kerf-gpu`'s
  `decode_args`): `{:.3}` skipped a frame at 1.0006 s on a fine time base (`spelled to the microsecond`).
- `render_with_progress` streams `-progress` into `{fraction, elapsed_secs, eta_secs}` and polls cancel; `render_with` is
  the no-op-callback wrapper; `audio_pcm` decodes a window to mono s16le (input-side `-ss`) for the GUI's Web Audio
  preview (`render_with` is the no-op-callback`).

## Playback stream

- `stream_preview` hands a whole span to one long-lived ffmpeg (per-frame spawn caps below frame rate) and reads composited
  JPEGs off stdout split on `FFD8` / `FFD9` (`**Playback is a real video stream**`). It composites through the same graph
  the export builds (`push_inputs` + `build_filter_complex` over a `Timeline::slice` from the playhead, shared with
  `build_export_args`) and the same `Timeline::for_render` gate; only the ends differ: proxy paths in
  (`timeline_frame_inputs`), `-c:v mjpeg -f image2pipe pipe:1` out (`composites through`).
- A stream plays in parts (`preview_parts`, `segments::PLAYBACK_LIMITS`: two inputs a part, 10 s pre-roll): the export
  planner cuts the slice, each part is the export's part graph (`push_part_picture`, shared with `build_part_args`) with
  `-frames:v` and the JPEG sink (`preview_part_args`), `pump_runs` starts the next part's ffmpeg once this one showed its
  first frame, on one clock; a cut within the limits is one run with unchanged argv; a part that dies before its first
  frame having asked for hardware decode restarts in software; `KERF_PLAYBACK_PARTS=off` (`**A stream plays in parts**`).
- Nothing waits on ffmpeg forever: the stream reads stdout on a side thread and gives up (kill, so the hwaccel retry falls
  back to software) after 30 s with no first frame or 15 s between frames; an export polls cancel every 250 ms and is killed
  after 300 s with no `-progress`; each `hw_encoders` probe is capped at 20 s (`**Nothing waits on ffmpeg forever**`).
- Frames are paced to the requested fps against the wall clock (backpressure, not racing ahead), carry their timeline time
  and `late` (seconds behind schedule; grows when the stream renders slower than it plays) (`paced to the requested fps`).
- `playback_params` (pure; `PlaybackRequest { fps, max_width }`): an unstated rate is the cut's own (`export_format` of
  `for_render()`: delivery rate else first clip as it renders; a muted track's footage does not set it), clamped
  `MIN_STREAM_FPS..=MAX_STREAM_FPS` (1..60), stated the same, NaN / zero / negative = unstated; width default 960
  (`PREVIEW_STREAM_WIDTH`), stated clamped 64..=1920, never wider than the footage (`**Rate and`).
- A stream that cannot keep up is restarted cheaper by the page, not caught up by the engine (`frontend.md`): ffmpeg has no
  wall clock, frames dropped after rendering save nothing (`restarted cheaper by the page`).
- With the GPU preview on the GPU may play a session instead (`app.md` *GPU playback*): same Motion plan of the same slice
  (`PlayCut`) from a forward-only `FrameCursor` per clip; `stream_preview` is what it hands over to
  (`SpanPlan::handover`); the stream stays default and fallback (`the GPU may play a
  session instead`).

## Phone and camera footage, HDR

- Rotation: every FFmpeg decode autorotates (nothing passes `-noautorotate`; a proxy is written upright). The probe
  (`display_rotation`: `Display Matrix`, or clockwise `rotate` tag on old FFmpeg) reports the displayed size in
  `StreamInfo.width/height` (what `export_format`, fit / crop, smart crop and the bin geometry use) and the angle in
  `StreamInfo.rotation` (`display_rotation`).
- Range: `StreamInfo.color_range` records ffprobe's `color_range` (`range_tag`; `unknown` / nothing / else = `None`; libav
  `range_name` agrees). A full-range VP9 / HEVC / AV1 file is plain `yuv420p` to `pix_fmt`, hence the flag
  (`render_plan.rs` refuses the picture on it). Stored as ffprobe says (RGB / gray read `pc` too), the plan reads it on
  YCbCr only; schema v10, older assets have `None` = not known, judged by name (`StreamInfo.color_range`). `fps` goes
  through `nominal_fps` (jittery VFR `r_frame_rate` can be a multiple; past 1.5x the average, snapped) (`nominal_fps`).
- HDR: `StreamInfo.color_transfer` / `color_primaries`, `StreamInfo::hdr()` (HLG `arib-std-b67`; PQ `smpte2084`). Every
  decode tone-maps to 8-bit SDR BT.709 exactly once (`tonemap_chain`, pure): `zscale` linear float, BT.709 primaries,
  `tonemap=mobius`, `zscale` back with error diffusion; without libzimg (`zscale_available()`, probed once) a `colorspace`
  move. Graph paths use `ClipFx.hdr` (from `transition_fx`), after the fit scale and `fps`, before any colour work; the
  composited still prefixes it per clip; single-input decodes (`decode_frame`, `contact_sheet`, `generate_proxy`) ask
  `source_hdr` (view of `source_traits`) and append it after their own downscale (`tonemap_chain`, pure + unit-tested`, `ClipFx.hdr`).
- Salience / scene detection read raw frames of the original (scenes use the proxy only where `proxy_fidelity` allows)
  (`are analysis, not picture`). The proxy is where preview footage is converted: `generate_proxy` tone-maps while
  downsizing, `Project::preview_assets` hands the graph `Asset::as_sdr_proxy` (no HDR tags), an HDR file's proxy key gains
  `|sdr` (older proxies rebuilt); until it lands the preview tone-maps the original in the graph; export always reads the
  original. The input's transfer / primaries / matrix are stated to `zscale`, not read from the frame (stripped-tag phone
  files abort with "no path between colorspaces"). SDR argv byte-identical (`**The proxy is where preview footage is
  converted**`).

## The BT.709 label (`model::bt709_labels`, `BT709_LABEL` = `setparams=colorspace=bt709`)

- Untagged HD YCbCr is labelled BT.709 only when another input of the same graph names a matrix, because FFmpeg reads
  untagged as BT.601, FFmpeg 9 negotiates the matrix along the graph (converting the untagged HD) and the GPU refuses the
  stack (`**The BT.709 label**`).
- "Names a matrix" = HDR (tone-mapped to tagged BT.709) or a tag that is the file's word: not `unknown` / `unspecified` /
  `reserved` and not a decoder's default (PNG is `gbr`; JPEG / WebP `bt470bg`; `mjpeg`, `amv`, `speedhq`, `jpegls` tag
  everything `bt470bg`, as do decode-only `mjpegb`, `sp5x`, `thp`, `smvjpeg`; `mxpeg` assumed) (`StreamInfo::names_a_matrix`,
  `DEFAULT_TAG_CODECS`). Principle: a tag a decoder writes by default is not the file's word; grey, alpha, SD and `yuvj`
  footage from other codecs do name a matrix when tagged, an untagged `yuvj` clip is never labelled itself; held over
  the corpus by `a_decoders_default_tag_names_no_matrix_in_any_graph_of_the_corpus`
  (`not a decoder's default`, `The principle is "a tag a decoder writes by default`).
- HD = mpv's rule `width >= 1280 || height > 576` on the original's displayed size (a small proxy size is still labelled as
  its original is: `PlanStream::label_bt709`); YCbCr on the `pix_fmt_layout` allow-list; never `yuvj...`, RGB, grey, alpha
  or a still image (`mpv's rule`).
- A single clip, or any graph with no matrix-naming input, is byte-identical; it is not a proxy label (proxy keys do not
  move). One table decides it, `ClipFx.label_bt709` (`transition_fx` -> `graph_bt709_labels`), read by
  `build_filter_complex` (export, levels meter, stream, scrub window) and the planner; the still decides over its own layers
  (`StillLayer.label_bt709`). The decision is the graph's, not the frame's (a stream / window / ranged export starting past
  the BT.709 clip is its own graph and does not hold it) (`One table decides
  it`, `**The decision is the graph's, not the frame's**`).
- The filter is the first of the clip's chain, ahead of `trim` and every scaler, never the `-colorspace` output option
  (FFmpeg 9 treats it as an order to convert): it is a tag, not a conversion (the fit `scale` would convert a picture for
  a dip to white's RGB-only `fade`, and a label behind it states the matrix of a picture already read as BT.601).
  Untagged HD beside footage tagged `smpte170m` / `bt470bg` becomes a mixed stack: the export converts, the GPU refuses on
  9.0.2. Tests `a_labelled_input_renders_as_a_file_tagged_bt709_is_in_all_four_builders`,
  `a_labelled_clip_dips_through_white_as_a_file_tagged_bt709_does` (`**Why first, and not`).
- FFmpeg 6.1 (`FixedBt601`): the label negotiates nothing and only changes an HD layer taking the RGB round trip in a mixed
  stack (translucent, dip through white) (`**FFmpeg 6.1 (`FixedBt601`)**`). It is applied under every policy because
  `composite_color_policy()` runs `build_still_args` under its own mutex (a builder that asked would deadlock); the plan
  mirrors it (`PlanStream::matrix()` BT.709 for a labelled layer) (`would deadlock`).

## Scopes (`scopes.rs`)

- The frame is `timeline_frame_yuv_cancellable` (`timeline_scopes_cancellable`): the still's graph with a
  `StillOutput::Yuv444Pipe` sink (`-f rawvideo -pix_fmt yuv444p`, new sink, no golden moves) from the originals
  (`export_still_inputs`) at delivery width: the Y'CbCr the file holds, not an `rgb24` pipe (composite matrix, BT.601 on
  FFmpeg 6). `yuv444_to_rgba` decodes limited-range with `delivery_matrix` (BT.709 at 1280 wide or over 576 tall, else
  BT.601; `bt601` / `bt709` / `bt2020` nameable), clipping what the cube cannot hold. `analyze` reads every pixel for
  summary / histogram (a clipped pixel counts) and a decimated frame (at most 480x270) for waveform, parade, vectorscope
  (64^2, cells >= 1%). Ungated by `cpu::lease` but niced (`bg_command`), thread-capped, supervised
  (`output_supervised_sized`: `STILL_TIMEOUT`, cancel), at most `MAX_SCOPE_WIDTH` (3840) wide (`**Scopes**`,
  `A measurement, not a frame anyone waits`).

## Blend modes (`Clip::blend_mode`, `engine/cli/blend.rs`)

- A `normal` clip keeps its `[cur][v{flat}]overlay=..` byte for byte; any other mode replaces it with `blend_chains`: the
  composite is split (`format=yuv420p` first), one copy `select`ed to the clip's window (the `enable`'s `between(t,..)`) so
  only the window's frames are converted; the clip's pad is laid out twice by the very `overlay` the plain composite would
  use, opaque (`lutyuv=a=255`) for colour and transparent (`lutyuv=a=0`) then `alphaextract` for alpha (one `overlay` onto
  a transparent canvas would mix the canvas's chroma into a translucent layer); `alphamerge`, `gbrap`, `blend=c0_mode=M:
  c1_mode=M:c2_mode=M` (`c3` left `normal`), back to `yuva420p`, last `overlay` onto the untouched composite =
  `composite*(1-a) + mode(clip, composite)*a` (`**Blend modes**`).
- Fades and dips of a blended clip are alpha fades (`ClipTiming::fades` makes every step `FadeTint::Alpha`); letterbox bars
  are transparent (`letterbox_pad` with `keeps_clear_bars`) unless a later filter drops alpha (tone-map, grade matrix
  `format=yuv444p..`, `vignette`); static rotation fills `black@0` (`static_rotate`), not `none`; the still uses the same
  chains (`still_overlay`, `StillBlend`) (`**A blended clip's fades and dips are`).
- Colour: blend modes are RGB; both conversions name the composite's matrix = the bottom layer's tag (`composite_matrix`:
  BT.709 for a labelled layer, else `frame_matrix`) with `tv` range (naming another makes FFmpeg 9 convert the whole
  composite at the last `overlay`; FFmpeg 6 carries it untagged); untagged bottom layer names nothing; the scaler is
  `neighbor` (`composite_matrix`). A clip below full opacity has `format=yuva420p,colorchannelmixer=aa=..` ahead of the
  `split`, so its fades are `fade alpha=1` steps on RGBA alpha before the 256/255 scale (`A clip below full opacity`).
- The GPU draws it where exact (`gpu.md` "Blend modes"; `colour_chains` / `blend_colour_graph` / `blend_probe_graph`;
  `composite_matrix` mirrored by `PlanCanvas::blend_tag`): `lutyuv=a=255` is `clipval` (Y 16..235, U/V 16..240) and `gbrp` is
  another swscale path than `gbrap` (`**`lutyuv=a=255` is `clipval`**`). Golden `blend`.
- Limits: 4:2:0 pictures whose chroma makes no RGB colour (sharp saturated edges) clip into the gamut; along edges that cut a
  2x2 chroma block the blend takes the block's chroma from one pixel (`**Limits**: a 4:2:0 picture`).

## Blend folding (`blend::fold_runs` / `blend_fold`; `blend_folds` in `cli.rs` decides)

- Nested blend forks cost ~3.3x each on FFmpeg 9.0.2, so a run of 7+ (`FOLD_MIN`) consecutive blended footage clips of one
  track whose drawn frames share none is one fork (`[cur]format=yuv420p,split=N+1`, then `overlay=eof_action=pass` in
  turn); shorter runs keep the nested graph byte for byte (`with_fold_min` test-only); hundreds are quadratic
  (`**A run of blended clips is folded into one fork**`, `**Run**: 7 or more`).
- Guard (`ClipTiming::drawn_slots`): a still or title is not footage; `Rational::from_fps` must equal `from_fps_filter`; slots
  `[first, end_frame(window().1))` (dissolve tail included) pairwise disjoint. `first` is the lower of the start's nearest slot
  and the slot of the start truncated to the source's tick (`setpts=PTS-STARTPTS+S/TB` is an `(int64_t)` cast;
  `StreamInfo.time_base`, schema v21, `ClipFx::time_base`). `settb` is not added to a chain (it would move the picture with
  the fold) (`**Guard**`).
- No tick, no fold: a stream with none recorded is drawn anywhere (`(0, i64::MAX)`). Pre-v21 assets are backfilled at export:
  `time_base_inputs` -> `probe_time_bases` (bounded, cached, lock released; `render_with_progress` / `render_variants` do
  these) -> `Project::apply_time_bases` (no revision; the project holder calls it after); a failed probe keeps `None`
  (nested, planner cuts into parts) (`**No tick, no fold**`).
- A fold holds ~10 swscale contexts a clip (`cap_wide_graph` bounds the threads); a shared frame, overlap, tail, plain clip
  or still ends a run; folds nest with each other and clips, so the planner prices a run at `RUN_FORKS` (3) forks
  (`**A run, a break**`, `**Folds nest with each other and with clips**`).

## Speed ramps (`Clip::speed_ramp`; `model/speed_ramp.rs` map, `Clip::along` / `local_at_along`; `clip_timing/ramp.rs` prints it)

- Nothing reads `speed_mag()` for a ramped clip: window, tail, `picture_window`, handle, scrub, plan, cover and thumbnail
  times all go through the map (``speed_mag()` for a ramped clip``).
- Picture: `setpts='st(0,(PTS-STARTPTS)*TB);(E+start)/TB'`, `E` the closed-form inverse per region (hold / equal speeds
  `T+(r-R)/v`; linear `T+2x/(v+sqrt(v*v+2k*x))`, `x = r-R`, no cancellation near `k` = 0), `r` stored once (`st(0,..)` /
  `ld(0)`), regions a balanced `if(lte(ld(0),R_j),..)` tree, the whole value quoted (commas and a `;`). `Ex` is text and
  arithmetic (every operation parenthesised, no exponent in a number) so `eval` computes what the graph does; `reverse`
  stamps frames with forward timestamps so `r` is source seconds in the playing direction (`*Picture*`).
- A tail (`ClipFx::tail`) is `along(dur+tail)-along(dur)` source seconds past the window; a clip ending mid-ramp plays on through
  the rest of the ramp as far as its handle (`transition_fx`: `local_at_along(span + handle) - duration`); `same_sound` is
  false for a ramped side; a window starting inside a late file's lead starts the clock late (`ramp_offset` = `lead - window
  start`); everything keyed is on the clock after `setpts` and unchanged by a ramp (`ramp_offset`).
- Sound: `atempo` has one tempo per stream and its `tempo` command is no way round (each command re-origins the stream,
  loses 1024 source samples; one instance per step loses up to 20 ms of length), so `audio_steps` cuts the ramp: a hold and
  the stretches before / after the keys one step each; a moving speed in steps whose chord stays within 8 ms of source of the
  curve, 0.12..1 s, at most 128 (longer ramps lengthen steps, counted before allocating); `tempo = source span / timeline
  span`, so sound meets picture at every edge (`*Sound*`).
- Each step: `atrim` (60 ms roll-in, 100 ms flush), `atempo` stages (fewest within 0.5..2 sharing the tempo; none at 1x;
  per-stage tempo by `sqrt` / bisection, stage count by doubling, never `powf` / `log2`: a libm `pow`'s last digit differs per
  platform and is printed in the graph; `tempo_advance` sums by plain multiplication), `adelay` for a first step before the
  source, `apad=whole_len`, `atrim=start_sample:end_sample` to exact samples plus half a 2 ms crossfade each side
  (`audio_pieces`, pure), `acrossfade` chains (`never `powf` / `log2``). `atempo` plays its content 13..19 ms early, counted
  per stage (`tempo_advance` = `0.0145 * (1 + s + s^2 ..)`, held at its 0.25x / 8x values), so each step is read that much
  later. Steps are independent WSOLA runs (a steady tone changes phase at a seam; 2 ms crossfade = click-free, no more)
  (`plays its content early`).
- `adelay` stamps the silence it emits first with no time when its input arrives late and the muxer takes `INT64_MAX`
  ("Non-monotonic DTS", then "Conversion failed"): the ramped chain ends `asetpts=N/SR/TB`; raw-sample tests do not see it;
  verify with a full AAC / mp4 export of a long file (`**`adelay` stamps the silence it puts out`).
- The scrubbed still is `-ss` at the map's source time (export's frame or a neighbour; a reversed still a frame past, as at
  constant speed). Golden `speed-ramp` (`ramp_for`), `ramp_rendered.rs` (`-- --ignored ramp_rendered`); the GPU refuses a
  ramped layer (`core-render-plan.md`) (`speed-ramp`).

## Stabilization (`crate::stabilize`, `engine/cli/stabilize.rs`; model `core-model.md`, project ops `core-project.md`)

- vid.stab when the binary has it (`vidstab_available`: `-filters` lists `vidstabdetect` and `vidstabtransform`, probed once,
  `with_vidstab` in tests; bundled GPL builds have it, minimal / LGPL ones do not), `deshake` otherwise
  (`**vid.stab when the binary has it**`).
- Measuring (`measure_motion`, background lane, no progress while waiting so the job reads `queued`; `READ_STALL`, cancel; fresh
  emptied work dir so files have plain names): the clip's window of the original through `scale=w='min(1920,iw)':h=-2,
  vidstabdetect=..:shakiness=5:accuracy=15,showinfo`, then one frame through `vidstabtransform=input=motions.trf:debug=1`
  which solves the transforms as it opens and writes `global_motions.trf` (fresh process, same `rand()` sequence as vid.stab's
  own second pass). `MotionAnalysis` kept at `<cache>/kerf/stabilize/<asset id>/<digest>.json` (`*Measuring*`).
- `analysis_for` picks, among analyses of the same file covering the window, the one starting earliest, then ending latest, then
  newest (so a head-trimmed slice picks what the whole clip does); a proxy's lookup holds the stamp to the file it names;
  `stabilize::forget` on remove / relink (`Project::forget_removed_caches` / `forget_relinked_caches`, lock released); the first
  slice written in a run prunes on its own thread (slices unused 1 week, work dirs older than a day, analyses unreadable / of a
  replaced file / of a file missing 90 days) (`analysis_for`).
- Correcting: `stabilize::corrections` is vid.stab's `cameraPathGaussian` operation for operation (radius `round(strength x
  fps)`, path minus average, own zoom term zeroed) over the whole analysis; every graph hands `vidstabtransform` only the absolute
  corrections of the frames it reads (`relative=0:smoothing=0:optzoom=0:zoom=Z:crop=black:interpol=bicubic`, old transforms
  format, `<cache>/kerf/stabilize/slices/<digest>.trf`), scaled to the frame the input decodes (original's displayed size; a
  proxy's from `proxy_video_info`), so previews, stills, proxies and export move a frame alike. Zoom = the setting's, else
  `auto_zoom` over the whole analysis (1st/99th percentile, rotation left out, <= 30%) so slices get the whole clip's zoom
  (`*Correcting*`).
- Frame identity: vid.stab numbers frames from the first it gets, so a stabilized clip's input opens `SEEK_PAD` (3) source frames
  early (`clip_input_seek`: `-ss`, input dedup key, picture and sound trims, parts' input count) and the chain trims at
  `Slice::start` (midpoint before the first corrected frame, never on a frame); the filter goes right after that `trim`, before
  `reverse` and the retime (speed, reverse, ramps keep it) (`*Which frame is which*`).
- An HDR clip is tone-mapped before it is steadied in every graph (export moves its tone-map to straight after the `trim` for a
  stabilized clip; the still always tone-maps first; the SDR proxy was tone-mapped at encode): a 10-bit HDR picture reaches
  vid.stab's 8 bits only as SDR. A still seeks to the same midpoint and runs the slice after its tone-map. A clip whose analysis
  does not cover it gets no filter and plays as shot; images, titles and 360 never get it. `deshake`: `rx` / `ry` 16..64
  (multiples of 16) ~ `0.5% + 2.5% x strength` of the width, `edge=mirror`, centred `crop=w=iw/k:h=ih/k` by the zoom; temporal, so
  a still / stream from the playhead starts afresh (`tone-mapped before it is steadied`).
- A clip that plays as shot (`unsteady_clips`: `missing`, `queued`, `analyzing`, `failed`, `unsupported`) is never refused an
  export: `review_cut`'s `unstabilized` check, the MCP `export` `warnings` and the dialog say so (`**A clip that will play as
  shot**`).
- Known limits: vid.stab / deshake are 8-bit (a 10-bit SDR source is converted ahead); FFmpeg 6 regrids a VFR source's proxy so
  frames can drift from the original's (bundled 9.0 keeps them); the detect `showinfo` log is held in memory (~40 MB for the
  longest window); corrections are pinned against libvidstab 1.1, another version may move frames (`**Known limits**: vid.stab
  and deshake`).

## Thumbnails (`thumbnail.rs`)

- A designed thumbnail has no graph of its own: the background is `export_still` of `VideoGraphics::background_of` the cut
  to a temp PNG at the delivery frame x `Thumbnail::background_scale` (preview: x preview width over thumbnail's); at scale 1
  the argv is the cover's (golden unmoved). Cropped / scaled with tiny-skia (bicubic), layers drawn by
  `svg::raster::render_with` with the fonts its sources name (`fonts::snapshot_for`), composited source-over; PNG written
  atomically, `.jpg` / `.webp` via `transcode_image` (`-c:v mjpeg -q:v 2` / `libwebp -quality 90`, killed past `STILL_TIMEOUT`,
  cancellable, failed output removed) (`A designed thumbnail is drawn without a graph`).
- `suggest_frames`: `candidate_times` (pure) splits each video clip's clean window (span less fades, `transition_in`, the next
  clip's) at its asset's scene changes, middle of each piece, topped up with even times, at most 24; each is
  `timeline_frame_yuv_cancellable` at 320 px with no graphics, three at a time; `frame_stats` drops black / faded / flat frames
  (`is_blank`); `score_frames` weights sharpness 0.5, exposure 0.25, contrast 0.25; `pick_best` keeps the top N at least `span /
  4N` apart (`**Frame suggestions**`).

## Known open ends the full file states

- Unconfirmed without a GPU / on a Mac: VAAPI upload + encode runs, `/dev/dri` in AppImage / Flatpak, the distro's `libva`;
  AV1 AMF slope / offset on real footage; NVENC / QSV / VideoToolbox export QPs unmeasured; hardware-slot gate and
  classification only unit-tested; macOS `kern.num_taskthreads` limit; hardware-encoded proxy score fidelity (`Unconfirmed
  without a GPU`, `unverified on a Mac`).
- Late start: MPEG-TS previews seeks `lead` off; reversed clip's bare end fade-out; a text track leading the sound; Matroska seek
  lands sound up to a block late; `atempo` spreads an onset early (`**Known limits:**`, `there, not fixed: a seek into Matroska`).
- Not fixed: FFmpeg 4.4.x `loudnorm,aresample` channel-layout failure; compressor ducking path cutting a bed when dialogue ends
  first (`Cannot select channel`, `pre-existing bug, left alone`).
- Parts: no mid-clip cuts of long footage, no `together` in parts, no pipelining, no free-space check (`Not done: cutting a long
  footage clip`).
- Keyed mask / crop parity only on 6.1; moving still not measured on 9.0; keyed-zoom key-equality step is inherent; moving-zoom
  folding left (`Not measured on 9.0`, `Folding the moving zoom`).
- SVG: no `animateMotion` / CSS animation / non-`data:` images; titles-on-tracks stream reuse by grid name not done
  (`**Not drawn**`).
