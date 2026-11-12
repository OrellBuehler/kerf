# kerf-gpu (`crates/kerf-gpu/`)

The wgpu compositor — work package A0 of `.claude/plans/gpu-compositor-and-roadmap.md`,
a feasibility spike that `kerf-app` now links (A2: the Preview panel's opt-in native
surface, `crates/kerf-app/src/gpu_preview.rs`, in `app.md`). It draws a `RenderPlan` headless
(`Gpu::new(GpuOptions)`: instance / adapter / device, an `Err` rather than a panic
when there is no adapter, `force_fallback_adapter` for the software one) and reads
RGBA back. wgpu is built with the Vulkan / Metal / DX12 backends only — no GLES, no
WebGL — so the CI target is a software adapter (Mesa **lavapipe** on Linux, WARP on
Windows) and nothing may depend on an optional wgpu feature. **A GPU failure is an
error, not a panic**: every unit of wgpu work runs in out-of-memory / validation /
internal error scopes (`Gpu::guarded`), anything uncaptured is logged and remembered,
`Compositor::new` and `composite` return `Result` (and `Compositor::caps()` says what it
draws, `GpuCaps::A0` plus `text`, `exact_scaler` (once `scaler::kernels()` measured this FFmpeg's scaler as the model), `scalers` (the names a delivery gives its scaler besides `bicubic` whose tables the same probe measured to be this FFmpeg's: `ScalerKinds`, `bilinear` / `lanczos` / `neighbor` / `spline`, one by one), `blur_fill`, `matrix_convert` (once `convert::kernels()` measured this FFmpeg's conversion between matrices as the model, and the grade's kernel too), `matrix_convert_scaled` (a `scale` that resizes a picture as it converts its matrix, once `convert::scaled()` measured this FFmpeg's cascade as the model), `motion`, `fades`, `transitions`, `dip_white`, `keyed_zoom`, `placement` (with `behaviors`: where `placement_probe::measured()` says this FFmpeg's `pad`, `perspective` and `crop` place a picture as the model does; `Compositor::caps()` drops both otherwise), `keyed_opacity`, `keyed_color`, `mask`, `svg` (floating animated SVG titles, where `svg::measured()` says this FFmpeg's `overlay` of a picture is the model: `Compositor::caps()` sets it from the probe, `Unsupported::SvgOverlay` otherwise), `still_motion` (the kinds of picture, a JPEG and an RGB one, whose Ken Burns chains `still_probe::measured()` says this FFmpeg draws as the model does; `Compositor::caps()` sets it from the probe), `grade` (every kind, where `grade::measured()` says this FFmpeg's conversions are the model's: `Compositor::caps()` drops it otherwise), `blend` (the modes `blend::measured()` says this FFmpeg draws as the model does, both matrices), `translucent_approximate` (set unless `roundtrip_probe::measured()` is `Match`: a layer below full opacity is then FFmpeg's, `Unsupported::TranslucentApproximate`), `translucent_odd` (positive: set where the round trip matched and `roundtrip_probe::measured_odd()` found a model of this FFmpeg's odd sizes; a layer below full opacity with an odd side is otherwise `Unsupported::Geometry`, "a translucent layer of odd size") and the blur / sharpen / grayscale / invert / vignette / chroma key `effects` today (the blur only where `blur_probe::measured()` is `Match`), which `composite` / `render_plan` judge the plan by), and a lost device is tracked — every
later call is `GpuError::DeviceLost` and the **owner builds a new `Gpu` and
`Compositor`** (the device is not recreated behind the caller's back).
**A blend mode is drawn** (`GpuCaps::blend`, below: "Blend modes") for a layer upright or turned by a quarter, at its own size or resized by the fit pass, opaque or below full opacity, not letterboxed, over an even frame, and
where `blend::kernels()` says this FFmpeg's chain is the model's (a layer below full opacity: where `blend::extras()` has `TRANSLUCENT`); every other blended layer is refused by the plan (`Unsupported::BlendMode(layer, mode, cause)`,
`core-render-plan.md`) and FFmpeg draws it. `PlanLayer.blend` is `Normal` in the fakes and fixtures.

FFmpeg stays the decoder: `source::decode_layer` pipes one frame from the binary as
`yuv4mpegpipe` 4:2:0 (the same `-ss` as the still graph), spawned through kerf-core's
`ffmpeg_command()` (no console flash on Windows). **The decode states its own size**
(the y4m header) and it is compared with the probe's: a JPEG with an EXIF orientation
probes 480x270 and decodes 270x480, so a mismatch is `Unsupported` and that frame goes
through FFmpeg. A `pix_fmt` that is not on the allow-list of known-opaque ones is refused (the plan
does too); an asset with no recorded `pix_fmt` costs a second `yuva420p` decode that is
refused if anything is transparent. A child is killed after 30 s or when it writes more than the probed size (`source::run_child_logged`: the readers of its pipes are detached threads that hand their bytes over a channel, and a child that is killed is **never** waited for through them — a wrapper script's grandchild may hold the pipes open as long as it likes; one that exits is waited for 2 s for its stdout, then it is an error), and `-ss` past the last frame (zero frames) is `Ok(None)` — FFmpeg's own still draws
nothing for that layer, and so does the compositor. The layers of a frame decode in
parallel, each given `budget / layers` threads even at a full CPU budget, or one thread when the
layer reads a preview proxy (`limit_frame_args(args, share)`, `engine/decode_policy.rs`). That is the one-shot path; `FrameSource` (below) is the
long-lived one.

**Presenting a frame (A2).** `Compositor::render_plan_texture_with` is `render_plan_with`
without the readback: the finished composite stays on the GPU as a `RenderedFrame` (an
`Rgba8Unorm` texture; `read_back(&Gpu)` copies it out for a test — `tests/parity.rs`'
end-to-end case asserts the texture, read back, is the picture `render_plan` draws, bit for
bit — and `from_rgba` uploads a picture the compositor did not draw).
`Gpu::new_for_surface(options, target)` opens the device on an adapter that can present to a
window (the surface is made first and the adapter asked to be compatible with it, so a
hybrid-GPU laptop uses the one that drives the screen; a surface that cannot be made — a
window system no backend takes — is `GpuError::Surface`, never a panic), and `Presenter`
configures it (a **non-sRGB** format if the adapter has one, so the encoded values the
composite holds go out as written, and an sRGB target decodes them in the shader first;
`Opaque` alpha — the webview is what is transparent, never the surface) and draws a
`RenderedFrame` into a rectangle (`present.wgsl`: a texel centre per pixel, so exact at 1:1
and bilinear otherwise) with a `Surround`: a matte inside the frame where the picture is
smaller, a backdrop beyond it. An acquire that says `Outdated` / `Lost` is reconfigured once
and retried; `Timeout` / `Occluded` are a `GpuError::Surface` for the caller to skip.
`present.rs`' tests (`#[ignore]`d, an adapter) hold the blit pixel-exact against an offscreen
target in RGBA, BGRA and sRGB formats. **`Presenter::skip_vsync(on)`** switches the surface between
`AutoVsync` and `AutoNoVsync` (wgpu's own fallback chain: `Immediate`, `Mailbox`, `Fifo`), reconfiguring
only on a change and not at all where the surface has neither `Immediate` nor `Mailbox`
(`present_mode_for`, pure, tested over the mode lists); the contents of a reconfigured surface are
not guaranteed, so the caller changes the mode just before the frame it presents. The app asks for
it while frames of a drag follow each other (`app.md`). The app's request mailbox drops a frame request that
arrives after a newer one of the same pump, remembering **one** pump's newest number: two pumps alternating
requests (two windows scrubbing at once) are ordered by arrival, as one without a number is.

**Which adapter.** `GpuOptions::refuse_cpu` makes a device request that wgpu answers with a CPU
rasteriser (lavapipe, WARP: wgpu ranks `Cpu` last but still returns it when it is the only adapter) a
`GpuError::NoAdapter` that names the adapter and `KERF_GPU_ADAPTER`; `force_fallback_adapter` asks for it by
name and is never refused. `GpuOptions::for_preview(KERF_GPU_ADAPTER)` is the app's choice: unset or
`hardware` takes the machine's GPU and **refuses a CPU rasteriser**, `software` forces the CPU adapter, `auto` /
`any` takes whatever wgpu prefers. `Default` and `for_tests()` (lavapipe, unless `KERF_GPU_ADAPTER=hardware|auto`)
are unchanged, so the parity harness and CI still run on it. Measured (the review, lavapipe, 960x540): a
software adapter draws one layer in about the time of an FFmpeg still on three or four cores, so it is not a
win for a live preview; nothing here has been run on WARP.

**A1's frame source, the pure pieces** (A1b-1: design `.claude/plans/a1-design.md` §1; none
of these spawns a process, `FrameSource` itself is A1b-2). All unit-tested, all pure:

- `frame_cache::FrameCache` — `Arc<YuvFrame>`s keyed `(SourceId { file: source_identity,
  format }, pts in ticks)`, byte-capped (`DEFAULT_CAP_BYTES` 256 MiB), least recently used out
  first by an O(n) scan, **pinning** (`pin` hands out an RAII `PinGuard` and dropping it
  unpins; a pinned frame is never evicted, the cap is soft for them and `CacheStats` says so; a
  purge or `clear` detaches live guards — `PinGuard::is_live` goes false; the frame just
  inserted is never its own victim), `Arc`s outlive eviction. `insert_cold` is for frames a
  run only *passes* on the way to its target: it takes free space or a cold slot, and never
  pushes out a frame that was asked for. `covers_from - 1` is a `checked_sub`. Each frame states **`covers_from`**: "no frame of this file has a
  pts in `covers_from..pts`", so `at_or_after(source, t)` is one `BTreeMap` range query that
  hits only when the frame *proves* it is the first at or after `t` (exact on VFR; a hole left
  by an eviction is a miss, never the next frame held), `before(source, t)` is the frame at
  `covers_from - 1`, and `mark_end` makes every later time `Lookup::PastEnd` (FFmpeg draws
  nothing there). A frame decoded after another covers from the tick after it; a run's first
  covers from its seek tick, which **the caller clamps to one frame interval before it**
  (`FrameSource` marks the file instead when the first frame is a whole interval late, and then
  claims nothing before that frame). A file has one picture per pts, so inserting under a held key
  keeps the first frame; `conflicts_with` says beforehand whether the picture offered is a
  different one (the run fails and the file goes one-shot) and `CacheStats::conflicts` counts a
  refused one — no assertion, because a container that guesses its pts does that.
- `y4m::Y4mReader` — streams frames straight into the plane `Vec`s (`take().read_to_end()`
  into spare capacity: no whole-stream buffer, no copy, no zero-fill; the header and `FRAME`
  marker are read unbuffered, a few bytes, so nothing swallows the planes). The header's size
  is compared with the probe's **before any plane is allocated** (`Y4mError::Size` →
  `GpuError::Unsupported`), only `C420` / `420jpeg` / `420mpeg2` / `420paldv` / no tag is
  accepted (`420p10` also starts with "420"), no bytes or a header alone is "no frame", a
  stream ending between frames is finished and one ending inside a header or plane is
  `Truncated`; after any error the reader stays failed (every later call repeats it, never a
  clean `None`). FFmpeg 6.1 writes `C420mpeg2`, 9.0 `C420jpeg`, same pictures (fixtures).
- `showinfo::ShowinfoParser` — the timestamps of a run, read off stderr as it is written
  (flags: `-hide_banner -nostats -nostdin -loglevel info ... -vf showinfo=checksum=0,... `
  and `-fps_mode passthrough`, which the spawn must spell with the engine's `fps_mode_flag()`
  (`-vsync` before FFmpeg 5.1); plain `showinfo` checksums every frame: +65 % decode on 6.1,
  +40 % on 9.0). A frame is a line
  holding `[Parsed_showinfo_N @ 0x…] n:<n> pts:<pts>` and nothing else is read. The
  prefix is required, including for `config in time_base`, so a file name or a container
  title containing `] n:1 pts:0` cannot poison it. ANSI colour is stripped first, and the
  spawn calls `plain_log_env`, which removes `AV_LOG_FORCE_COLOR` and sets
  `AV_LOG_FORCE_NOCOLOR=1`. Coloured and poisoned-title fixtures from both builds are in
  `tests/fixtures/showinfo/`. Beyond that, `n` must count 0, 1, 2, ... (a rebuilt
  graph renumbers: an error), a run's **pts must be strictly ascending** (an out-of-order or
  repeated pts is `ShowinfoError::OutOfOrder`, and that file falls back to FFmpeg: the cache
  keys a frame by its pts and covers ticks up to it, so a repeat would be served for another
  picture and a step back, a decoder still learning its reorder depth after a mid-GOP seek,
  would let a frame claim ticks that are not its own). That is `ShowinfoParser::new()`, the
  `FrameSource` router's; a `FrameCursor` has no cache and reads with
  `ShowinfoParser::allowing_repeats()`, which accepts an **equal** pts (a file whose time base
  rounds two frames onto one tick, which `Pick::select` takes as it comes) and still rejects
  an earlier one. `config in time_base: a/b` is re-read
  on every occurrence (a frame before the first, a bad ratio or a *change* is an error),
  `pts:NOPTS` or a pts that is not a plain integer (`0x21`) is an error, and
  `duration:` is kept (the last frame's is `SourceFrames::last_duration`). `line_lossy` takes
  bytes, since stderr carries non-UTF-8 file names. **The real stderr of both FFmpegs is in
  `tests/fixtures/showinfo/`** (mp4 1/12288, matroska 1/1000, mpegts 1/90000 with a container
  start, with and without `-ss`; the SEI `User Data=` hex lines, the `Output #0` block printed
  *between* the first frames and the closing statistics included) and the two builds report
  identical frames; `tests/fixtures/y4m/` holds a real y4m of each (`.gitattributes` pins
  them binary / LF).
- `router::route(&[RunState], &Request) -> Route` and `ThrashGuard` — which run serves a
  request. **Reuse** an idle run of the file behind the target by at most `effective_window`
  frames (`REUSE_WINDOW_PROXY` 24, `REUSE_WINDOW_ORIGINAL` 96, held to half of what the cache
  holds at the frame's size — a 96-frame read-forward at 1080p would otherwise flush the
  other layers; nearest wins), else **start** (free slot: at most
  3 runs a file, 6 in all) or **replace** the least recently used *idle* run (the file's when
  it is at its cap, the process's otherwise); a request behind every run of its file starts
  `BACKWARD_LEAD` (15) frames early; **`Exact` never evicts** (reuse or `OneShot`, a decode of
  its own that registers nothing), **`Prefetch` only takes a spare slot**, a busy run is
  neither reused nor evicted (`Route::Busy`). The guard counts only routes that **destroy a
  run somebody may want back** (`Route::is_thrash`: a restart, or a start that evicts a run
  that is not stale — not filling a free slot, a six-layer frame starts six runs at once) and
  only for `Forward`. `check(intent, route, now)` hands back a `#[must_use]` `Ticket` that goes
  to `finished(ticket, now, outcome)` once the run delivers its first frame or fails. It is
  **time-weighted**: once restarts have cost ≥ `THRASH_BUSY_SECS` (0.75 s) of the last second,
  the next is `Err(Busy)`. A run that fails or dies empty still counts (a failed start into a
  free slot is charged `FAILED_RUN_COST` too), so a crashing decoder cannot slip past by freeing
  its slot. Evicting a stale idle run of another file does not count, so a montage across more
  than six files is not thrash, and a start still open after a full window (its ticket never
  came back) is closed there and ages out instead of refusing every later start. A refusal (`GpuError::Busy` via `From`) renders that frame through FFmpeg's stream instead.
  Refusals are not counted so it recovers when the caller stops. Time is passed in, so the
  tests need no clock.
- kerf-core: `Pick::progress` / `FpsPick::seek` (above), `source_identity`,
  `disable_decode_hwaccel`.

**`FrameSource`** (A1b-2, `frame_source.rs`) puts those pieces to work: frames for a plan's
layers from **long-lived `ffmpeg` runs** and the frame cache instead of a spawn per frame.
`FrameSource::new(FrameSourceConfig)` → `Arc`, one per process, every call blocking (blocking
pool, never under the project lock); `frames(&layers, Hint)` decodes the layers side by side and
**layers asking for the same frame of one file share one decode**; `Compositor::render_plan_with`
is `render_plan` over it (`composite_shared` takes the `Arc` frames). A run is
`run_args`: `-hide_banner -nostats -nostdin -loglevel info [-hwaccel h] -copyts -start_at_zero -ss
T -i path -an -sn -dn -map 0:v:0 -vf showinfo=checksum=0,scale=out_range=tv,setpts=N/TB
-fps_mode passthrough -f yuv4mpegpipe -pix_fmt yuv420p pipe:1` (`-fps_mode` spelled by
`kerf_core::fps_mode_flag()`, which is `-vsync` before FFmpeg 5.1; the chain ends in `setpts=N/TB`,
after `showinfo` has printed the file's own pts, because a repeated timestamp makes the y4m muxer log
`Non-monotonic DTS` from another thread between the calls that make one `showinfo` line, which loses
its prefix: 5 runs in 200 on a repeated-pts mkv, 0 with it), spawned under `plain_log_env` with the
CPU cap divided among the runs alive; a reader thread pairs each y4m frame with its `showinfo` line **by number**
and files it under `(identity captured at spawn, pts)`. Only `Pick::AtOrAfter` is served (a still's
pick, keyed by **`kerf_core::seek_ticks(t, tb)`**, the tick the still's `-ss {:.6}` becomes);
`Before` / `Fps` are a cursor's (A1b-3) and come back `Unsupported`; a still image is one one-shot
decode, cached. What it guarantees: **a frame it returns is the one-shot decode's, byte for byte —
for the files it answers from runs, and those are the files it could not catch a run out on.** The
one-shot (`decode_layer`, `-ss t`, the FFmpeg still's own seek) is the contract, and what `-ss t`
returns is *not* "the frame at `t`" for every file: a seek into the frames a keyframe leads (B
frames of an open GOP, 1.96 s before a keyframe at 2.00 s) returns the keyframe, where a run that
read through from an earlier keyframe has the frame itself (49, against the one-shot's 50); a
transport stream lands on the next keyframe whatever is asked, and a run from 0 did not. So runs
answer only where they can be proved equal, and everything else is **one-shot, per file, from the
first thing that shows it** (`State::distrusted`, `SourceStats::distrusted`; the cost is speed, never
a frame — **within the limits stated below**: the seek-point probe looks at a file's head, and a
timestamp on a coarse time base can hide a late landing of exactly one rounded tick):
- **The container** is MP4 / MOV or Matroska / WebM (`kerf_core::source_is_indexed_container`, one
  cached `ffprobe` a file, off the lock): a positive allow-list, so a transport stream, AVI (whose
  pts are guessed, and guessed differently run to run: the same pts held two pictures and the
  cache's `debug_assert!` panicked a run's thread under the lock), program and elementary streams
  and a file the probe could not name go one-shot before any run. `FrameSource::cursor` refuses
  them too (a still image is let through by both: it has no container to be indexed, it probes as
  `png_pipe`).
- **A seek into the file must be a decode from a keyframe**
  (`kerf_core::source_seek_points_are_keyframes`, `engine/source_probe.rs`, one cached, bounded
  probe a file, off the lock; `FrameSource::cursor` refuses a file that fails it too).
  `-ss T` seeks to the last *sync sample* (the packets flagged `K`) at or before `T` and returns the
  first picture the decoder outputs at or after `T`; that is the frame at `T` only if the decoder
  outputs the sync sample's own picture first. x264's `intra-refresh` marks the start of every
  refresh wave a sync sample but codes it as a P picture, and the decoder outputs nothing until the
  wave is over: `-ss 2.0` returns 2.72 s, where a run that read through from an earlier keyframe has
  the true frames 2.0 to 2.68 (forward play: 18 of 75 answers differed with nothing distrusted: a
  run's own first frame is only checked at *its* seek, and a run reading through never seeks there). The probe lists the first packets of the video stream (16, 64, 256, 1024,
  4096 — as many rungs as it takes to see four sync samples, so an all-intra stream costs sixteen
  packets and a long GOP the rung that holds its first few) and decodes just those with
  `-skip_frame nokey`, requiring a picture of type `I` with `key_frame` set at **every sync
  sample's timestamp**; a file where one is missing, one with no sync sample in view and one the
  probe could not read (a failure, 20 s, a timestamp `ffprobe` does not print) is "no" and goes
  one-shot. **Limit: it looks at the file's head** (the first four sync samples), so two encodes
  joined, the first with keyframes and the second with intra refresh, is not caught; intra refresh
  is a property of a whole encode. B-frame streams are a different case with the same outcome: a
  closed GOP's keyframes pass, an open GOP's often do not (the decoder drops its reordered tail),
  and `State::distrusted` catches the rest at their first B-frame.
- **Only I and P pictures** (`ShowFrame::pict`, the `type:` of the `showinfo` line; the self-test
  fails a build that does not print it): the first B frame a run reads ends the run and marks the
  file. That is every open-GOP file (x264 `open-gop=1`, x265's default) and, deliberately, closed-GOP
  files with B frames too — telling them apart needs the keyframes' decode order, which `showinfo`
  does not give; all-intra proxies, the files the preview reads, are unaffected.
- **A seek that lands late**: the first frame of a seeked run is the first at or after the seek tick,
  less than one frame interval away; **one interval or more** marks the file (it was two intervals,
  which let a landing one or two frames late through, and the frame's coverage was claimed from
  `pts - ft`, so cache history decided the answer). A frame that landed late claims nothing before
  itself. "One interval" is `late_ticks`, the interval **rounded up** (`ceil` of the exact ticks, with a
  float-noise guard), not `frame_ticks`' rounded one: on a millisecond time base a 30 fps interval is
  33.33 ticks and the frames are 33 and 34 apart, so a seek just after a frame that a 34 ms gap follows
  reads its first frame 33 ticks on without anything being skipped, and `>= 33` marked 30, 29.97 and
  120 fps mkv / webm files (a seek in about one in a hundred) for good. The price, stated exactly: when
  the interval is not a whole number of ticks, a landing one *short* frame late from a seek that was
  on a frame's own tick (a distance of `ceil - 1`) reads as not late; that frame is still the
  one-shot's for that seek, and only a frame another run cached could answer a later request
  differently. An interval of a whole number of ticks (25 fps on 1/1000 or 1/12800, 30000/1001 on
  1/30000) keeps the old test: an open GOP's landing a whole interval late is caught as before. This also marks a variable-frame-rate file the first time a seek falls in a gap of a frame
  or more (the `vfr` leg of `tests/frame_source.rs` is that case: 79 of 80 answers are one-shot, equal
  to the one-shot decode and no faster).
- **A file that contradicts itself**: a time base that changes between runs, timestamps that cannot
  be read or paired with frames (`ShowinfoError`), a picture of another size or format than probed,
  or a picture that differs from the one an earlier run cached at the same pts
  (`FrameCache::conflicts_with`; the cache refuses and counts it, never a panic) ends the run and
  marks the file, so a file whose runs always die is not spawned and killed once a request.

**`Hint::Forward`** is also what the app's preview asks with for a drag that walks forward through the cut (A3, `app.md`): the run
reads ahead and the next frame of the drag is a cache hit; the frames are the router's either way. Not for a plan with a reversed
layer: its frames walk backward through the file, so a forward run would restart every frame and trip the thrash guard.

**`Hint::Prefetch`** is a request for a frame that will be wanted soon (the first of the clip a cut is about
to bring): it returns `Ok(None)` for every layer, **waits for nothing and draws nothing**, and has a run
waiting at the frame when it can. It opens the file's two probes (container, seek points: 50 ms between
them, once a file), does nothing when the cache or a run already covers the frame, takes only a **spare**
slot (the router's `Intent::Prefetch`: never stops a run anyone may be reading; at the file's cap of 3 or
the process's 6 it is a `Skip`), and leaves a file the runs are not proved equal for alone (no one-shot
decode for a frame nobody asked for). A still image is decoded once and cached. The run reads to the frame and
parks (`want` = the target), is idle to the router from then on and ages out like any run. Counted in
`SourceStats::prefetched`. `render_plan_with` / `_texture_with` refuse it (`Unsupported`): it returns no
frames to draw. `kerf-app` drives it around the playhead (`app.md`, "GPU preview"): the first frame of the
clips that start within 2 s ahead and the last frame of the ones that ended within 2 s behind, since a drag goes
either way.
`tests/prefetch.rs` (`#[ignore]`d, real ffmpeg): the run is waiting and the request that follows starts
nothing, a covered or heading-for frame starts nothing, the file's cap is respected, an unproven container
is left alone, dropping the source kills the prefetched run; with `KERF_BENCH=1` the first frame past a cut
with and without a prefetch (1280x720 all-intra, a loaded 4-core box, debug build: a clip never read before
145 to 183 ms on 9.0.2 and 321 to 361 on 6.1.1, 33 to 38 and 73 to 93 once its probes are done, 0.2 ms with a
prefetch; `docs/review-2026-10-10/after/perf-4`).

The checks: the parity harness decodes every compared case both ways and asserts equal planes
(FFmpeg 6.1.1 and 9.0.2) and `rendering_through_the_frame_source_is_the_picture_the_one_shot_decodes_give`
asserts the **rendered** RGBA of `render_plan_with` equals `render_plan`'s, bit for bit, on five
cases in two orders and both hints; `tests/frame_source.rs` walks a 29.97 mp4 and a jittered-pts mp4
(trusted: runs answer, in forward, backward and random order, every answer equal) and an open-GOP
x264, an x265 and a long-GOP transport stream (the files above: forward, backward and random through
one source each, every answer equal to the one-shot's, and the stats say which went one-shot), an
x264 `intra-refresh` mp4 (forward play, near each sync sample, backward, random: no run is started;
without the seek-point probe 18 of 75 forward answers differed with nothing distrusted), a ProRes
4:2:2 10-bit `.mov` and an all-intra x264 (the proxy's shape) answered from software runs, a 30 fps
mkv on a millisecond time base (a fresh source for each time that falls just after a 34 ms gap), a
cursor gating leg (`FrameSource::cursor` over each refusal and a still image) and, in
`tests/frame_source_fake.rs`, a fake `ffmpeg` whose run shows a B-frame (the run's own check, whatever
a real file's probe makes of it), plus six threads at once. The rules it holds to:
- A run's first frame covers from its seek tick (within a frame interval of it, see above); a run
  **from 0** covers everything before its first frame (a late-starting picture). A clean end calls
  `mark_end` (a time past it is `Ok(None)`, no decode); a clean run with **no** frame is a seek
  past the end (`mark_end(seek_tick - 1)`); a non-zero exit with no frame is a **failed start**,
  never an end, and its error carries ffmpeg's stderr tail.
- A run that reads past its target without the cache proving the frame is given up on
  and the request routed again; after two starts it decodes one-shot (`SourceStats::fallbacks`).
  A request made before the file's time base is known joins a run another request has starting
  (`joined_blind`) and cannot tell where that run is, so that run ending (or failing) with no frame,
  because *its* seek was past the end, is not an answer for the joiner: it routes afresh (it used to
  return "no frame" for a time inside the file; `tests/frame_source_fake.rs`).
- **Nothing waits forever, and nothing under the lock waits on a process**: a reaper thread (it
  holds a `Weak`) kills a run that is wanted and silent for `first_frame_timeout` (30 s) /
  `frame_timeout` (15 s) and one idle for `idle_kill` (20 s), and drops a finished run's record
  after 5 s. **The silence it measures starts when the run is wanted** (`Run::want_to`): a run
  parked at what it was asked for has made no progress for as long as it sat there, and was killed
  within a tick of being asked again. A `Scrub` / `Forward` request gives up after
  `request_timeout` (5 s), `Exact` after the one-shot's 30 s; a run blocks once it is past what
  was asked (plus `Forward`'s read-ahead, 48 MiB of frames, at most 24), so the pipe fills and
  ffmpeg idles; `release(source)`, `release_all()` and `Drop` kill every child. **A request waiting
  on a file that is released fails** (`GpuError::Decode`): a run the *router* stops sends its
  request to route afresh, which for a released file started a new run of it right after `release`
  returned (`spawned: 2`; a test reading `stats().runs` failed whenever that request won the lock).
  `Shared::releases` counts the releases (an atomic, bumped under the state lock) and `frame()`
  reads it **before anything else, the probes included** (an ffprobe before the first lock takes
  seconds on a loaded machine, and a release then is one the request began before);
  `State::released` (`Releases`) holds each file's count and time, and a request fails on a file whose
  count is above the one it read; the reaper forgets a release after twice the longest deadline. A
  request that read the count after the release is an ordinary one and starts a run (a closed source
  notes nothing). A run that closes
  its stdout and does not exit is killed after `EXIT_GRACE` (5 s; `wait_until`, a `try_wait` loop
  that holds the child's lock only for a poll — the reader used to `wait()` under it, so every
  `kill` and, through `stop` under the state lock, every request waited too). **No process is
  started or waited for with the state lock held**: `spawn` enters the run in the table with no
  child, releases the lock for `Command::spawn` and takes it again to give the run its child (a
  run stopped meanwhile has its fresh child killed), and `stop` hands the kill to a short thread
  (`reap_later`). One-shots are capped (`max_oneshots`, 2).
- **The path can be off**: the first use runs **`self_test`** (an `mpeg4` clip at 30000/1001
  made by ffmpeg's own encoder, decoded from frame 5 with the production flags, every pts
  expected exactly, all inside 10 s), and if it fails (a build whose `showinfo`, `-ss` or time
  base behave otherwise than the two measured, 6.1.1 and 9.0.2) the process decodes with
  `decode_layer`, as does
  `KERF_FRAME_SOURCE=oneshot` and any asset that never recorded its pixel format.
- **Runs decode in software, as the one-shot does** (`run_args(.., hwaccel: None)`; `decode_args` has
  never carried `-hwaccel`). They used to pass `kerf_core::decode_hwaccel()` (`auto` by default), and a
  hardware decoder is not always the software one: a ProRes 4:2:2 10-bit `.mov` through FFmpeg 9.0.2's
  Vulkan decoder (lavapipe is enough) differed from the one-shot on 79 of 80 frames, by up to 24
  levels. The retry-in-software / `disable_decode_hwaccel` dance went with it. `CursorConfig::hwaccel`
  stays a knob and now defaults to `None`: the export that opens a cursor says which decoder its
  render uses.
- `tests/frame_source_fake.rs` (unix, its own binary: it points `KERF_FFMPEG` at a wrapper)
  holds a run that hangs and one that dies to a prompt `GpuError::Decode`, a run that closes its
  output and never exits (`release` must not wait on it, and the request that waited on it fails
  rather than starts another run; the test polls for the wrapper's `closed.done` marker, as a loaded
  machine takes seconds to get a run that far) and a second run whose pictures differ from the
  first's at the same pts (the file goes one-shot, no panic).
  `bench_frame_source_decode_vs_one_shot` (`KERF_BENCH=1`) times both paths: here, release, software
  decode, 1x playback is 4.9 / 4.4 / 27 ms a frame at 720p / 1080p / 4K against 102 / 133 / 314
  one-shot, and a scrub (jumps of about a second) 23 / 65 / 217 against 101 / 139 / 314.

**`FrameCursor`** (A1b-3, `cursor.rs`) is the export's half: one clip's frames in output order
from an **exclusive run** of its own (opened at the clip's `FpsPick::seek`, with `run_args`; read
on the caller's thread, registered with no router, cached nowhere). **The seek is spelled as the
export spells it** — `FpsPick::seek_arg()`, the shortest text of the `f64` (`kerf_core::export_seek_arg`,
shared with `push_inputs`, so the two cannot drift), which FFmpeg truncates to whole microseconds,
and no `-ss` at the head of the file — not the still's `{:.6}`, which rounds: for a window start
like 2/30 s on a one-microsecond time base (frame at 66666) the rounded text (66667) starts the run
a frame after the one the export keeps and every pick is a frame late (20 of the 59 window starts
in `tests/cursor.rs`'s microsecond mp4 differ between the two spellings; it holds the export's).
**`FpsPick::lead`** (`ClipFx::lead`, the source's `StreamInfo::lead`) is a late-starting source's:
a window that starts inside it is opened with no seek (`FpsPick::seek` is `0`), and its first frame's
slot is the clip's start plus `picture_delay`, so the output frames before it pick nothing — the
layer is not drawn, as in the export (`tests/motion_parity.rs`, `late/*`: head, inside the lead,
past it, speed 2 / 0.5, reversed, a fade-in at 30 and 25 fps — where the 0.3 s lead is half a
frame —, entering through a dissolve, one inside the lead and a dip, over another track, exact). `cursor.pick(&Pick)` reads until `Pick::progress`
decides and returns the shown frame, so its answer is `Pick::select`'s over the whole file — which
`picked.rs` holds to the export's rendered frames — **where `-ss` lands on the frame the timestamps
say**: a seek that lands on a later keyframe (a seek into the frames an open GOP's keyframe leads;
a long-GOP transport stream, which `FrameSource::cursor` refuses up front along with the other
containers that are not MP4 / Matroska) starts the run late, as the export's own `-ss` does, and the
cursor answers over the frames it was given — the export's, not `select`'s over the whole file, and
not proven against the export. `FrameSource::cursor` refuses a file a router run has already marked
(it cannot know of an open-GOP mp4 whose sync samples the decoder shows as keyframes before a run
has read its B-frames): the same known limit as the still's. It
keeps every timestamp read but only the pixels a later pick can show: `keep_from` once decided and,
while a forward pick is undecided, the newest frame (output frames the caller skips are read through,
not held); a forward clip holds a frame or two, a **reversed** one its whole window, capped by
`CursorConfig::window_cap_bytes` (512 MiB; past it `Unsupported`, FFmpeg renders the clip).
**Forward only**: a pick whose frame was dropped is an error, the same frame again is fine;
`Before` of a run's first frame is `None` from the file's start and `Unsupported` otherwise (and
`for_layer` refuses a `Before` pick up front, spawning nothing). A repeated timestamp is accepted
(`ShowinfoParser::allowing_repeats`, above), one going back is an error. **Nothing waits forever**:
a watchdog kills a read waiting past `first_frame_timeout` / `frame_timeout`, a run that closed its
stdout and does not exit within `frame_timeout` is killed (a bounded `try_wait`, never `wait()` under
the child's lock; the router's `run_reader` the same, `EXIT_GRACE`), a size or layout the compositor
does not draw is `Unsupported` (not a failed decode), a thread that cannot be started kills the child,
and `Drop` kills the run. `FrameSource::cursor(layer, config)` opens one where runs are trusted
(refused: the self-test failed, an unrecorded or alpha pixel format, a container that is not
MP4 / Matroska — a still image excepted — , a file whose sync samples are not keyframes, a file a
router run has marked one-shot; `tests/frame_source.rs` holds each gate);
`cursor::picks_through(cursor, pick, frames, |frame, shown| ..)` is the per-clip loop an export
makes, handing each frame to a callback as it is decided (a long clip is never held whole).
`tests/cursor.rs` (`#[ignore]`d, both FFmpegs in CI's parity job; 9.0.2 run here) decodes lossless
**self-numbering** sources (16 bits in luma blocks: a 30 fps CFR, a VFR on the 1/30 grid, and one
with repeated timestamps, asserted to have them) and checks the number on every output frame of
90 clips (speeds 0.5 to 4, forward and reverse, windows off the grid / from the start / to the end
of the file, 24 / 29.97 / 30 / 60 fps) against `select`: 4290 frames on 9.0.2. `tests/cursor_fake.rs`
(not `#[ignore]`d, unix: `KERF_FFMPEG` is a shell script writing a numbered y4m and `showinfo`
lines) holds what needs no FFmpeg — skipped output frames are not held, a run that closes its output
and never exits is killed, a wrong-sized picture is `Unsupported`, `Before` spawns nothing.

**Playing** (A4) is a loop over a `kerf_core::PlayCut` (`core-render-plan.md`: the Motion plan of the slice `stream_preview` plays) and
the cursors above, not over the router's runs: the router serves `Pick::AtOrAfter` from a shared cache, and a playing frame has to be the
export's `fps` pick, which only a cursor reads. `MotionFrames::frames(plan)` reads a frame's layers **side by side** (scoped threads, one per
cursor: three layers cost the slowest decode, not the sum), `Compositor::render_motion_texture(plan, size, &mut motion)` is
`render_plan_texture_with` over it (the texture stays on the GPU for the presenter), and `Compositor::warm_text(plan, size)` rasterises a
plan's titles into the atlas without drawing, so a caption that comes on screen costs its `ffmpeg` on a prefetch thread and not on the frame (and `render_motion_texture` warms the frame's own titles *while* its cursors read, the two waits being independent).
**`MotionFrames::feed()` is a `CursorFeed`**: `open(layer, needed_at)` checks the clip as `FrameSource::cursor` does (`FrameSource::check_cursor`, the same
gates split out: the self-test, a recorded opaque pixel format, an indexed container, seek points on keyframes, no run has marked the file) and
starts its `ffmpeg`, which fills the pipe and waits (at most 6 waiting, 10 s each; `needed_at` is when the clip comes on screen, and with 6 waiting the
one wanted **last** is dropped for a nearer clip, while a clip wanted after all of them is not opened: `Opened::Full`, the caller asks again; `close()` ends the feed
and a cursor that was being opened is dropped when it is ready); the frame that first needs the clip takes the waiting
cursor instead of paying the file's two probes (50 ms) and the process start. Called from a thread that is not drawing, and a clip it refuses is
known *before* the cut, when the caller can still choose what to play instead. A cursor opens at the clip's own seek, so what it reads from there
to the frame is decode work: a play session starts its slice at the playhead (`PlayCut`), which puts that seek at the playhead and not at the
clip's in point. `tests/playback.rs` (`#[ignore]`d; both FFmpegs) holds the played frames to the export, **plane for plane, 0 levels in every
case but the layers the compositor scales**: from the start (a hard cut, a clip's fades, a dissolve, a still image, a title), from the middle of a clip on the grid and between two frames, from deep in a
long clip, from inside a dissolve (the session starts before it and the frames from the requested time on are the whole cut's, dissolve
included), a dip, slides and a push entered inside the transition, a 25 fps clip in a 30 fps cut and a 2x clip (the slice's export, the graph
the stream plays), and every n-th frame asked for (n = 2, 3, 7, 30: a frame a newer request replaced changes no other). A slide's travel is
truncated to whole pixels, and the slice's clock is not the whole cut's by a float's width, so a travel that lands exactly on a pixel can fall
either side of it: slides are held to the slice's export, and so is a dissolve out of a still image (a PNG is a 25 fps stream, so the cut is a 25 fps one
with a 30 fps clip in it, and which source frame an output frame picks depends on where the slice starts). A layer over another track is held to the
export to the sample, scaled and translucent included (`play/pip/transform`, `play/pip/translucent and scaled`: they were held to 1 and 2 levels while the scaler's C kernel and the round trip's C vertical pass were a level from swscale's x86 ones). It also holds the feed (a cursor opened ahead is the one the cut uses and the first
clip's goes with it; a transport stream is refused when opened ahead; with more clips in the window than it holds it keeps the ones wanted first), and the texture a session presents to be the composite's picture.

**CPU-side work and SIMD.** The per-pixel work is the GPU's; what the CPU still does one sample at a time is vectorised or not done
twice, and nothing else is (`tests/bench_cpu.rs`, `#[ignore]`d, `KERF_BENCH=1`, times the CPU side; the `memcpy`-bound copies stay as
they are). The kernels live in `kerf-core` (`simd.rs`, `mask_plane.rs`, `mask_plane/x86.rs`) and `kerf-gpu` calls them:

- **Byte scans** (`simd::position_ne` / `rposition_ne`, the first / last byte that is not a value; `plane_bounds` on top): the bounding
  box of a title's glyph and box masks (`text.rs` `Raster::from_parts`, `text/seq.rs` `read_box` / `shape_of`), the "is the box solid"
  check, and the opaque-alpha scan of an asset that never recorded its pixel format (`source.rs`). 16 / 32 bytes a step with `cmpeq` +
  `movemask`, blocks of 64 bytes tested with the masks and-ed together, the tail one register overlapping bytes already known equal.
  Measured on x86_64 (1080p plane, 1.7 ms scalar): 0.04 ms on SSE2, 0.03 on AVX2.
- **The mask's alpha plane** (`kerf_core::mask_alpha_plane`; made for every new `(mask, size)`, so on each step of a mask-handle drag).
  A **rectangle**'s alpha is a monotone function of its offset (`1 - d`, `/ feather`, `clamp`, `* 255` and the truncating cast each keep the
  order, `inverted` turns it round), and a sample's offset is the larger of its column's and its row's, so its alpha is the smaller of
  the two alphas (the larger when inverted): one ramp per column and one value per row, and a byte `min` / `max` per row (`min_row_*`,
  `max_row_*`; SSE2 16 bytes, AVX2 32). An **ellipse** is `f64` lanes (2 on SSE2, 4 on AVX2) running the scalar's operations in the
  scalar's order, no fused multiply-add, the same truncating `cvttpd`; its band around the edge needs libm's `hypot` and, with the
  columns past the last whole group, goes through the scalar `Params::px`. Measured on x86_64 at 1080p, scalar to AVX2: 1.2 to 0.13 ms
  (hard rect), 3.2 to 0.11 (feathered rect), 1.6 to 0.7 (hard ellipse), 2.4 to 2.0 (feathered ellipse, whose ring is all `hypot`).
- **Not vector code**: `sws::table_aligned` is **memoised** per `(scaler, align, src, dst, one)` (a table is a pure function of exactly
  these five, so the key is all of them: a key that left one out would hand the C build's table to a SIMD build or the bicubic table to a
  bilinear scale; 64 entries, 32 MiB, no table over 8 MiB, newest first). Every table of the compositor comes through it (`bicubic`, `table`
  and `plane_filters`' calls are this), `table_aligned_uncached` is the reference (`bicubic_uncached` is it for the C bicubic). The lock is
  taken to look up and to keep, never while a table is built or cloned, and a table over the entry cap is not cloned at all. A scaled layer's
  six tables cost 0.25 to 0.64 ms every frame and now 0.002 to 0.006 (a clone of the cached table). `effects::vignette_map` works each distinct `(|xx|, |yy|)` once
  (`hypot` takes magnitudes, so the map depends only on the offsets' magnitudes however the size is odd or even) and a row with a magnitude met
  before is copied: 40 to 9 ms at 1080p, 175 to 54 at 4K, once per size; held to the every-sample function bit for bit at odd and even
  sizes.
- **Pitfall (codegen)**: inside a `#[target_feature]` kernel, `copy_from_slice` or an iterator adapter is a *call* in a release build with
  several codegen units (the mask rows ran 2.3 times slower than the scalar's). The kernels read and write through raw pointers after one
  bounds check (`n = min(len)`, `i + N <= n`).
- **Detection, cap, fallback**: `kerf_core::simd::path()` is the widest of `Scalar` / `Sse2` / `Avx2` (x86_64, `is_x86_feature_detected!`)
  or `Neon` (aarch64, `is_aarch64_feature_detected!`) the CPU has, probed once. `KERF_SIMD` caps it (case-insensitive: `scalar` / `off` /
  `none` / `0`, `sse2` / `neon`, `avx2`; any other value is no cap and logs one `tracing` warning). `simd::with_path(path, f)` forces a path
  on a thread (`#[doc(hidden)]`, the tests' hook). Every kernel keeps its scalar function (`position_ne_scalar`, `plane_bounds_scalar`,
  `mask_alpha_plane_scalar`): the fallback, what `KERF_SIMD=scalar` runs, and the oracle. A vector path returns exactly what the scalar one
  returns (0 differing samples): these feed the exact-parity text and mask paths. Every CPU-side sample of the compositor is 8-bit (the y4m
  is `C420`, the readback `Rgba8`), so there are no 10 / 16-bit kernels.
- **Tests**, always run, each on every path `available()` lists: the scans over widths 1 to 67 and larger, from several start offsets
  (loads are unaligned), with a differing byte at every position and 0 to 3 random ones; `plane_bounds` against the old row-by-row
  function on noise, rectangle, sparse, full and empty planes; the mask plane for random masks (every shape, feathers either side of the
  1e-6 threshold and past 1, centres and sizes out of range, inverted) at widths 1 to 67 and 128 / 255 to 257 with heights 1, 2, 3, 18 and 40,
  and at picture sizes, against `reference_plane`, the function as it was before the vector paths, verbatim; `text.rs` cuts a title's
  masks identically on every path. `kerf-core/tests/checker_simd*.rs` (naive loops, misaligned sub-slices, an electric-fence allocator)
  and `kerf-gpu/tests/checker_simd_cut.rs` (a masked, titled cut per `KERF_SIMD` value; `#[ignore]`d) are the independent checks;
  `kerf-gpu/tests/checker_simd2_{memo,memo_bytes,vignette}.rs` hold the memo to `bicubic_uncached` / `table_aligned_uncached` over
  both scalers and the alignments 1, 2 and 4 (and its caps, under a counting allocator) and the vignette map to a re-derivation from `vignette.c`; `kerf-core/tests/checker_simd2.rs` checks the rect rows.
- **NEON is written but has never run**: the byte scans have a NEON path (`vld1q_u8`, `vdupq_n_u8`, `vceqq_u8`, `vminvq_u8` per 16 bytes;
  the block or chunk that differs is searched by the scalar scan). It type-checks and lints clean for `aarch64-unknown-linux-gnu`, and its
  control flow (`first_ne_by_chunks`: blocks, chunks, the overlapping end) is arch-independent and tested with a portable "all equal"; an
  arm64 runner runs the tests above on it unchanged. The mask rows have no NEON kernel and take the scalar path there.

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
  layer tagged otherwise is converted into it by a scaler stage (**reproduced since**: the
  `convert.rs` paragraph below). An earlier version of this section said the matrix was fixed; that was true
  of 6.1 only, and the pinned 9.0.2 build was 29.5 dB / 35 levels off on a single
  BT.709 clip. So the policy is measured once per process from the real still graph
  (`composite_color_policy`), the plan takes it as input, **stacks of one matrix are drawn
  with it, stacks of mixed matrices are drawn where the conversion is reproduced
  (`GpuCaps::matrix_convert`, below) and refused elsewhere** (see `render_plan.rs` above), and the
  CI parity job runs both builds. The consequence to know about: an *export* is untagged
  `yuv420p` that a player shows as BT.709 for HD, so the FFmpeg preview already disagrees
  with the file by a few levels on saturated colour. Fixing that is a decision for when
  the GPU path replaces the FFmpeg preview. **One graph is settled**: untagged HD beside footage that names a matrix is
  labelled BT.709 by the export graph itself (`engine-cli.md`, *The BT.709 label*; the plan mirrors it,
  `core-render-plan.md`), so the stack is one matrix and is drawn on 9.0.2 (it was a mixed stack there, refused); untagged HD beside `smpte170m` footage became BT.709 against BT.601 and is refused there. A lone
  untagged HD clip is still read as BT.601 by FFmpeg and drawn that way (27.3 dB against a player on 9.0.2). A logo or a photo beside HD
  (a PNG's `gbr`, the `bt470bg` of a JPEG, a WebP and the `mjpeg`, `amv`, `speedhq`, `jpegls` (and their decode-only relatives) decoders are decoders' defaults, not matrices) is not a stack that is labelled and is
  drawn or refused as before. **Accepted difference**: the playback stream that takes over from the compositor (`SpanPlan::handover`) is a
  slice of the cut and labels over its own clips, where the Motion plan labels over the cut's graph; when the slice has dropped the
  matrix-naming clip a translucent HD layer differs by 1 level in the export on 9.0.2 and 8 to 14 on 6.1.1 (export 8, still 14, stream 11, scrub window 14), coloured text by 16 levels on its pixels on 9.0.2 and not at all on 6.1.1, and a dip to white by 2 levels on 9.0.2 and 15 on 6.1.1 in the export (the RGB round trip reads the label; labelled against not, the clip alone; in a mixed stack on 6.1.1 the label moves an HD clip's dip by 33.0 dB / 15 levels into it and 32.5 / 15 out of it over all frames, method in `engine-cli.md`), opaque pictures not at all. The stream's own matrix
  (`PlanStream::matrix`, from the probed `color_space`) is used for taking a translucent
  layer out of YUV in the round trip below and for choosing the composite's matrix.
- **`eq` is a YUV operation**, not an RGB one: `kerf-core`'s `eq.rs` (re-exported as `kerf_gpu::eq`: the plan also reads the tables, for how steep they are) builds vf_eq's own per-plane
  tables (the integer `process_c` path when gamma is 1, the `pow` table otherwise, its
  `float` clamping, truncation) and the test suite checks them byte for byte against
  FFmpeg. Kerf's "temperature" is a power function on the chroma planes.
- **Scaler: swscale's own, ported, and measured** (`sws.rs`, `scaler.rs`, `composite.wgsl`).
  swscale's filter *table* has quirks a formula misses — the window start truncated
  toward zero (so at the first pixels the tap at -1 is absent, not folded into pixel 0),
  right-edge folding, near-zero tap trimming, 14- / 12-bit weights normalized by error
  diffusion, the 16.16 step — and the shader does its integer arithmetic on them (15-bit
  horizontal intermediate clipped at the top, vertical pass in one of two arithmetics, below).
  Planes are scaled independently at their own size and rounded to 8 bits between stages (the
  fit scale and the transform's scale are two scalers in cascade). The first version
  used a bicubic *formula* and was wrong by up to 20 levels along the borders of busy
  footage. **What a build's scaler is depends on the build, not the graph, and the compositor draws
  the one this FFmpeg runs** (`scaler::kernels()`, measured once per process on the real `scale`,
  like `grade::kernels()`; `Compositor::caps()` has `exact_scaler` once it matched):
  * **The tables.** A SIMD build (x86 with MMX, NEON) rounds each filter's size up (4 horizontally,
    2 vertically: `filterAlign`) and **keeps the original taps in the padding** — swscale only
    *counts* the near-zero taps on the right as droppable and rounds that count up, copying the originals
    unless `SWS_BITEXACT` — so the row is normalised with them and its weights are another's than the
    C build's (`sws::table_aligned`; the old zero padding was a level off on 7.9:1 and steeper shrinks). Built tables are memoised
    on the whole input, scaler and alignment included (the SIMD notes).
  * **The vertical arithmetic.** C (`yuv2planeX_8_c`: `(64 << 12) + sum(c * s) >> 19`) or, on x86 with
    SSE2 and default flags, `yuv2yuvX.asm`: `(c * s) >> 16` per tap (`pmulhw`: every tap truncates),
    started from `(64 + 8 * (N - 1)) >> 4` with `N` the aligned tap count, `>> 3` — **except the last two
    rows of luma and the last row of 4:2:0 chroma**, which `swscale.c` hands back to the C kernel
    (`dstY >= dstH - 2`; slice threads do not matter, `dst_h` is the picture's). A single-tap axis is
    `yuv2plane1`, `(s + 64) >> 7`, the same in both.
  * **The algorithm an unflagged `scale` runs.** The product's graphs are complex ones, where the filter's
    own default applies: bicubic since FFmpeg 5.0, **bilinear before it** (4.4's `scale` defaults to
    `flags=bilinear`; only a simple `-vf` graph is handed the command line's bicubic). The probe tells
    (`Model { kernel, default, named }`: the kernel, the algorithm of an unflagged `scale`, and the named scalers proved besides), so the 4.4.2 here draws every fit in bilinear, as it is rendered. Explicit
    flags win (`blur_fill`'s thumbnail scales say `flags=bilinear`; a delivery's `scaler` names its own: a delivery that
    names `bicubic` is drawn as bicubic on a build whose unflagged `scale` is bilinear too, `compositor.rs` `named`).
  * **The names a delivery can give its scaler** (`bicubic`, `bilinear`, `lanczos`, `neighbor`, `spline`: the engine's `SCALERS`; every fit and
    zoom `scale` of the graph carries `:flags=<name>`). `sws::Scaler` has all five: `initFilter`'s `SWS_LANCZOS` (`p` = 3, window 6 samples per
    output sample, `sin(πx) sin(πx/p) / (π²x²/p)` in `double`, zero past `p`), `SWS_SPLINE` (window 20, `getSplineCoeff`, the natural cubic
    spline, adds and multiplies in `double`) and `SWS_POINT` (the filter's `neighbor`: its own branch, one tap at `(x + 2^15) >> 16` with the
    centred positions, padded to the alignment with zeros like every one-tap row), then the same trim, border folding and error-diffused
    normalisation as bicubic's. Lanczos and spline coefficients are `double` arithmetic truncated to the 54-bit integers (`libm`'s `sin`; a
    compiler that fuses multiply-adds rounds the spline's polynomial in the last place), so unlike the integer tables **they are the
    machine's, and the probe proves them name by name** (`Model::named`, `scaler::NAMED`: three chains of the delivery's own `scale=W:H:flags=<name>`
    text for each of `lanczos`, `spline`, `neighbor`: two shrinks, one with odd sides, and an enlargement, compared with FFmpeg's pictures
    through the model of the kernel the base chains found; `bilinear` is proven by the base chains' bilinear scales). A name whose chains
    come back as another picture is left out of the set and only the frames that name it are FFmpeg's (a `warn!` says so). **The base scaler
    and each name are separate questions with a backoff of their own** (`scaler::Round`, `State`): the base (the unflagged scale, the bicubic
    and bilinear chains: 10 children) is `Unknown` when one of its own children did not come back, as it always was, and then nothing is
    compared (the names have no model to be compared through, so none is asked or counted as failed); but a child of a name (its three
    chains, and Lanczos's `sin`) that fails or hangs leaves **that name** out of `Model::named` and the base's answer as its own ten children
    measured it: `kernels()` is a `Match` without the name, `exact()` stays true, and bicubic and bilinear and the other names are drawn. **Only what
    a caller asks for is measured and waited for** (`scaler::kernels_for(names)`; `kernels()` asks for every name): `prepare` asks for the base
    and the one name its delivery gives its scaler (`ScalerKinds::of_name`; none for the default and for `bicubic`), `prepare_timeline` and the
    self-test for the base alone, so a project that names no scaler never waits for a name (the first call is 10 children, not 20) and a Lanczos one
    waits for Lanczos's four. **Single flight** (`scaler::Probe`, like `measure::Probe`): a question is measured by one caller at a time, whoever
    asks (the preview's `prepare`, Play's `prepare_timeline`, the self-test); the others wait on a condvar for that measurement, no longer than its
    bound (25 s, `FLIGHT_BOUND`; past it the flight holds nothing and the next caller measures) and are told what is known; a measurement that
    panics lands its flights (`Flying`'s drop). **Two phases**: the base is stored (and the callers that asked for it alone woken) the moment its
    own ten children are in, while a name's hung children still run, so a hung name never delays the base answer on first use (`measure_with`'s
    `on_base`). **Two passes in a call** (`Probe::settle`): a caller that came while another was still measuring the base could not ask for its name (a name is compared
    with the model the base gives), so after it has waited for the base it plans again, for the names it wants only (never the base, and not a name whose
    flight was left marked past its bound), and measures its name itself: it is never told "not measured" for a name nobody is measuring, and it never waits
    for a name it did not ask for. **A name that failed once is retried on a detached thread** (`spawn_detached`, one retry in flight per name): the
    call that finds it due (backoff 5 s, doubling to 5 min) answers at once with what is known, and the name joins the set for the calls after it
    when the retry answers (its 3 or 4 children, compared with the model and the probe picture the base was found with), so a name whose
    `sin` hangs again costs no preview frame and no Play press its 20 s, in a bicubic project or a Lanczos one; only the first measurement of a name is
    waited for, by the caller that asked for it. A name that differs is out for good and is not asked again. The self-test's `held_back` is the base's alone. Held by `scaler::tests` (a
    failing or hung child of each name, the hang killed at its deadline by `run_child` with the other pictures the model's, each question's
    backoff, a base that fails leaving the names unasked, only what is asked for being measured, **a due name whose retry hangs delaying no
    caller, asked for or not, and being retried once**, a caller that asks for the base alone not held by a name that hangs on first use, five
    callers one measurement, a follower bounded by the flight's bound, a panicking measurement, a retry that gets no thread, a caller that came during the base's flight having its name measured once the base lands, a failure two overlapping flights both report counted once (`Slot::failed`'s guard), the self-test) and
    `tests/scaler_probe_isolation.rs` (`#[ignore]`d: real FFmpeg children
    under a shell wrapper in a child process: each name's chain or `sin` child failing and then repaired, a base child failing and
    repaired, and a hung child of each name and of the base killed at its 20 s deadline, on 6.1.1 and 9.0.2). **The probe's cost**: a first `kernels()` (every name) is 20 `ffmpeg` children started at once, one picture each (the source, 9 base
    chains, 9 named chains, the `sin` geq), each bounded by `PROBE_TIMEOUT` (20 s, so a hung child costs the caller that asked for its question
    20 s, never more); `measure_with` hands every one `limit_frame_args(share = 20)`, the number that run side by side (`a_round_asks_for_the_children_...`
    holds it), which is one thread each: the 18 `-filter_complex` children have no `-i`, so the cheap-input rule gives them `-threads 1
    -filter_threads 1 -filter_complex_threads 1`, and the two `-f lavfi -i` ones are the budget's threads over 20, at least one. A retry of a
    name is 3 or 4 children (`shared(3..4)`). Measured (`checker_scalers_probe`'s plain scenario, a 4-core box at a load average near 30): the first
    `kernels()` takes 1.6 to 1.9 s wall and about 1.5 s of CPU in all (user 1.2 s, sys 0.35 s, the debug-built test's model comparison included);
    every later call is a lock and a read. **The chains cannot see a last-place
    difference in `sin`** (it moves a 14-bit coefficient with a probability of about 2^-38; measured on mutants, a `sin` with a random relative error of 1e-8
    or less comes back as the same pictures in 40 of 40 draws, 1e-7 is caught in 7 of 40, and a float-precision `sin` is not caught at all,
    over these three chains as over seven one-axis ones), so **Lanczos needs a second proof, the function itself** (`scaler::sin_graph`): FFmpeg's expression evaluator calls the
    same libm `sin` in the same process as swscale (`d->a.func0 = sin`, `eval.c`), and a `geq` that computes `sin(n * 0.0023)` for 4096
    values of `n` (arguments to 9.4: Lanczos's `πx` and `πx/3`, `x` to 3) and writes the eight base-256 fractional digits of the result as
    pixels (`mod(floor(t * pow(256, k + 1)), 256)`: scalings by powers of two, `floor` and integer subtractions, exact) hands back every
    call's result, which is compared **bit for bit** with `f64::sin`'s. A different libm is `Differs` for Lanczos alone (a `warn!` says
    so); a single float-precision `sin`, and one that is a unit in the last place off at one argument in seven, both leave exactly
    Lanczos out of the set on both builds (mutation-checked against the real FFmpegs). The spline has no libm, only adds and
    multiplies in `double`: a compiler that fuses them (an arm64 build) rounds in the last place, which no picture shows (a mutant with
    `mul_add` comes back as the same pictures), and which moves a coefficient as rarely as the `sin` does; what the chains do catch is a
    wrong constant or branch (the spline's constant 3e-9 off, the neighbor's window start half a sample off: both leave that name out).
    **A shrink has a limit per name**: the filter outgrows swscale's 256 taps (where it cascades a second scaler, which the model does not
    follow) from 43.4:1 for Lanczos and 19.6:1 for the spline (`sws::table_aligned` has no table there; the sweep over `dst` 1 to 96 and
    the usual sizes has a table at every ratio under, for every alignment), so `kerf_core::max_shrink_for` is 42 and 18 for those two
    (`MAX_SHRINK_LANCZOS`, `MAX_SHRINK_SPLINE`; 63 for the rest, a one-tap or two-tap table has no limit but the comparison's) and a stage
    past it is `Unsupported::Shrink`. **A still whose fit picture FFmpeg makes (`FitCopy`) is refused when the delivery
    names any of the four** (`scaler_named_other`: `:flags=<name>` is part of every `scale` of its text, in chains `still_probe` measures
    with the default scaler and with `bicubic` named only; `still_chain` / `fit_copy` give none, `StillMotion` says why). **A moving still on the
    route (the move stays inside the frame: the scaled copy FFmpeg makes and one `perspective`) is drawn under a name** where the move inside the frame
    of that kind of picture was measured under that name (`still_probe::verdict_route`, `StillKinds::named_route`, 4 names x the kinds, `RenderPlan::still_route_scaler`;
    below, "Moving stills").
  Held by `tests/scaler.rs` (`#[ignore]`d, ffmpeg only, no adapter): the scalar model
  (`scaler::scale_plane`) equals FFmpeg's `scale` **sample for sample** on both builds over noise and a
  smoother picture, both algorithms, 2:1 to 61:1 shrinks, odd outputs, enlargements and the blurred
  fill's thumbnail and fill scales, at 1, 4 and 16 filter threads, **and for bilinear, lanczos, spline and neighbor named** over the same
  sizes up to each name's `max_shrink_for` and at the limit itself with odd sides (333 scales, 219,381,979 samples of Y, U and V compared in
  the run, 0 differ, on 6.1.1 and on 9.0.2); and by the shader against the same
  FFmpeg in `parity.rs`' `the_scaler_matches_ffmpegs_scale_plane_by_plane` (max 0 in every plane, 75 plane
  results) and `a_delivery_that_names_its_scaler_is_drawn_with_that_scalers_tables` / `the_named_scalers_are_drawn_to_their_steepest_shrink_and_refused_past_it`
  (every name through Contain and Cover fits both ways, an odd source, noise and a 1-pixel checkerboard, a zoom out in cascade, a keyed zoom,
  the blurred fill, a T23c teaser with a fade and a caption, steep shrinks; 129 frames, each held to the still's planes, its PNG and the
  display, 0 samples differ on either build, and the refusals past a name's limit asserted with FFmpeg rendering them). The model is `Model::ASSUMED` (C, bicubic) while the probe is `Unknown`/`Differs`, which is
  the compositor as it was: a level from an x86 FFmpeg's on a resized layer, and what would amplify that
  level (a grade on a resized layer, the blurred fill) is refused (`caps.exact_scaler`). The arm64
  `Aligned` model (SIMD tables, C arithmetic) is built from the source and **not measured**: the probe
  decides there. **A shrink steeper than 63:1 (`MAX_SHRINK`) is refused**: the model equals swscale's at every
  ratio from 40:1 to 63:1 (the checker's `scaler_model_beyond_the_shrink_limit`: 40, 40.5, 41, 42, 45, 50, 55, 60, 62, 63:1,
  both algorithms, both axes, 0 differing samples; `tests/scaler.rs` to 61:1 on both builds) and has no table at 64:1
  (the filter outgrows swscale's 256 taps, where it cascades two scalers). **A compositor whose scaler was not measured as swscale's (`Kernels::Differs` /
  `Unknown`, or a probe not yet run) carries `caps.scaler_approximate` and the plan refuses every layer that a scale
  resizes** (`Unsupported::ScalerApproximate`, kind `scaler_approximate`): its scale is a level off, and a level is not
  exact; `A0` leaves the cap off, so what A0 draws it draws as it always did. Scaling happens on the decoded 8-bit 4:2:0,
  which is why resizing any other format — enlarging, or shrinking even by 1.05-1.5x (FFmpeg
  scales its chroma from the full-resolution plane, the compositor from the averaged one;
  flat max 8-9, edges up to 32 levels) — is refused by the plan too.
- **A plain `yuv444p` clip the fit resizes is the exception: drawn from its full-resolution chroma** (`GpuCaps::resize_444`, `resize444.rs`,
  `RenderPlan::resize_444_refusal`, `PlanLayer::wide_chroma`; T22's screen recording, `format_resize/yuv444p`). The export's chain ends in
  `format=yuv420p` (the placement chain's head and a still's last `scale` too) and every filter between the fit `scale` and it takes any format along
  (`crop`, `setsar`, `fps`, `trim`, `pad`, `eq`, `fade`), so the format negotiation hands the fit `scale` the composite's format and **one swscale call**
  takes `yuv444p W x H` to `yuv420p w x h` (`-v debug`: `fmt:yuv444p -> fmt:yuva420p` in the one `Parsed_scale`, both FFmpegs). swscale scales each plane
  from its own source (`initFilter` over `chrSrcW` -> `chrDstW`, `chrSrcH` -> `chrDstH`, the positions centred on both sides whatever the subsampling:
  `get_local_pos`): the luma `W x H -> w x h` as always, and **each chroma plane from the full-resolution picture, `W x H -> w/2 x h/2`**, twice the luma's ratio
  and by the tables the luma's size gives (the x86 horizontal and vertical arithmetic, the last chroma row in C). That is `scaler::scale_plane` over the
  4:4:4 plane with `c_rows` 2 for luma and 1 for chroma: the model needs no new arithmetic, only a chroma plane that is as large as the luma. The old route
  (decode to 4:2:0, which averages the chroma away, then scale) was up to 27 levels off. **Evidence**: `tests/checker_444_scaler.rs` (`#[ignore]`d, ffmpeg, no
  adapter) holds the model to FFmpeg's bare `scale` plane for plane: 203 scales, 181,744,908 samples, 0 differing on 6.1.1 and on 9.0.2 -- 16 sizes of
  `W x H` from 1280x720 (shrinks to chroma 63:1, 2:1 horizontally with the luma unscaled, enlargements to 2x where the chroma axes are 1:1, sizes whose halves are
  odd), noise and a smooth picture with one-pixel chroma stripes, the default scaler, `bicubic`, `bilinear` and the named `lanczos` / `spline` / `neighbor`,
  1, 4 and 16 filter threads, windows (the engine's `crop` ahead of the scale), a `yuva420p` destination (alpha 255), the steepest shrinks and the model's refusal
  of the table that does not exist past 63:1 of chroma. **The decode** hands the planes over as the decoder made them (`source::decode_args`, `frame_source::run_args_in`:
  `-pix_fmt yuv444p`, **no `scale=out_range=tv`** -- the export's chain gets the decoder's frame, and the plan lets no full-range picture through) and
  `Y4mReader::of_format` reads `C444`; `DecodeFormat::Yuv444p8` is part of `SourceId`, so the cache, the runs and the distrust of a file never alias the 4:2:0 decode of
  the same file, and a `FrameCursor` knows its format (`MotionFrames::prepare` reopens a cursor whose format is not the layer's now, a feed's waiting one included:
  `CursorFeed::open_planned` opens in the format the plan marks). **Which layers** is the plan's (`RenderPlan::for_decode(caps, size)` stamps `PlanLayer::wide_chroma`;
  the compositor's entry points, `render_plan*`, `render_motion_texture` and the cursors' feed, do it, and a harness that decodes for itself must); the compositor mirrors the
  plan at the render (`composite_texts`: a layer the plan draws from 4:4:4 that was decoded as 4:2:0, or the reverse, is `Unsupported`; a frame whose planes are not
  those of its format is a `Decode` error). The scaler pass is the 4:2:0 layer's (`composite.wgsl` `fs_h` / `fs_v`, `scale_stage`): `plane_stage_in(stage, chroma, wide)`
  reads the same window of a full-size chroma plane and scales it to half the result, and everything after the stage is the layer a 4:2:0 picture would be.
  **Refused** (`FormatResize` as before, from `resize_444_refusal`): any cap but `resize_444` (a compositor sets it only on a `Match` of the probe and an exact scaler),
  an odd window, result, Cover crop or picture, a second scale in the chain (the transform's constant zoom: FFmpeg resizes the 4:4:4 picture in the first), colour correction, a
  grade, an effect, a mask, a rotation, an opacity, a blend mode, a fade, dissolve or slide, a keyed number besides the zoom and the position, a blurred fill, an HDR / reframed / 360 / full-range / still
  picture, a delivery that names a scaler besides `bicubic`, a chroma shrink past the table limit (`max_shrink_for` of the chroma ratio, 31.5:1 of luma), another matrix
  than the composite's where the build negotiates it (swscale converts it in this very call), and every other format (4:2:2, 4:4:0, 10-bit, RGB). **The probe**
  (`resize444::resize_444()`, `measure::Probe`, 45 s deadline, 30 s backoff, `Unknown` never cached; `prepare(plan)` runs it for a plan with a `yuv444p` layer, the harnesses'
  compositors up front) needs `scaler::kernels()` a `Match` and runs the **export's own graph** (`kerf_core::export_graph`) and the **still's** (`still_graph`) over eight toy
  cuts of a lossless noise `yuv444p` file (a Cover shrink, a Cover that crops, the engine's static `crop` ahead of the fit, a Contain pillarbox, an enlargement, 2:1, a
  10:1 shrink whose chroma is 20:1, `bicubic` named): three export frames and a still each, every plane of the picture held to the model with no tolerance, after
  asking the plan that it takes the toy from its 4:4:4 planes. A build whose `scale` is another arithmetic, or whose graph puts a conversion between the fit and the
  overlay, is `Differs` and nothing is drawn from 4:4:4; one that could not tell is `Unknown` and asked again after the backoff. Pin it with `Compositor::with_resize_444`.
  **A zoom or position that moves is the placement's, and is drawn** (T22's keyed `screen` clip): the placement chain's head is `format=yuva420p`, so the fit `scale`
  is still the one call, and `pad` / `perspective` / `crop` work on the 4:2:0 planes it leaves, the layer a 4:2:0 clip's from there on (the probe's two moving toys
  compare the first frame, whose pose is the identity, so the fit's picture is what the placement hands on). **Held**: `parity.rs` `a_yuv444_clip_the_fit_resizes_is_drawn_from_its_full_resolution_chroma` (7 cuts, 14 stills: planes, PNG and display 0 differing),
  `a_moving_zoom_on_a_yuv444_clip_the_fit_resizes_is_placed_from_the_fits_planes` (a zoom in and out, a pan, a zoom and pan; Cover and Contain),
  `what_a_yuv444_resize_leaves_to_ffmpeg_is_refused` (the refusals above drawn by FFmpeg, 4:2:2, the unmeasured and differing caps, the render's mirror),
  `motion_parity.rs` `a_yuv444_clip_the_fit_resizes_is_drawn_from_its_full_resolution_chroma_in_an_export` (six short exports, a moving zoom and a 4:4:4 clip cut to a 4:2:0 one among them:
  0 differing in every plane) and the local sweeps (`checker_444_stills_a_grid_of_canvases_fits_and_crops`: four pictures, 17 canvases, both fits, with and without a crop: 171 cuts drawn, two stills each, and 101 refused
  for the format, an odd result or the table limit, FFmpeg rendering each; `checker_444_stills_placed_moves`: 20 moves, 4 instants; `motion_parity.rs` `checker_444_motion_*`: 46 exports of 12 frames,
  canvases of every shape, crops, speeds 2, 0.5 and reversed, a 4:4:4 clip over and under a 4:2:0 one, moving zooms), all 0 differing on 6.1.1 and 9.0.2. **A pan alone** (a keyed position, its scale
  channel keyed at a constant) is FFmpeg's in an export: the chain keeps a per-frame `scale` at 1, a second scale that resizes the 4:4:4 picture in the first (a still samples the pose and has none, and is
  drawn). **The ledger**: T22's `format_resize/yuv444p` was the single cause of its 450 still and 446 motion frames (the two clips that are not full range, plain or keyed): **6.1.1 53,418 -> 54,314
  of 71,586 (74.6% -> 75.9%, +450 still, +446 motion), 9.0.2 50,332 -> 50,782 (70.3% -> 70.9%, +450 still; its export frames are refused as a cut FFmpeg 9 flags full range)**;
  `KERF_LEDGER_444_OFF=1` asks the ledger as if the probe had not matched.
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
  levels off on saturated bars. A translucent layer of **odd size** takes other routes
  through swscale and is drawn where `roundtrip_probe::odd_translucency` measured this build's ("The translucent round trip", below).
  A translucent layer whose matrix is unknown is refused. The tightest figure on 9.0.2 is a translucent BT.709 test pattern alone
  (40.9 dB flat against the 40 dB floor, the GPU 1-3 levels bright from the final
  conversion's truncation bias). **A quarter turn of a translucent layer is refused**
  (`Unsupported::Rotation`, from the layer's geometry: `translucent` and a 90 or 270 turn; the render guards it too): the graph runs
  `colorchannelmixer`, `rotate` and then the conversion to 4:2:0 on the RGBA picture, so the chroma pairs two pixels of a row and filters
  over the rows of the *turned* picture, while the round trip here comes before the turn (the pairs are the same samples upright and
  at 180, other ones at 90 and 270): Y exact, U and V up to 17 and 19 levels off in the first frames of `checker_b_m`'s 36 cases at 90
  and 270 (0 and 180 were the upright layer's own figures then). It cannot be drawn in the graph's order to the sample either, because the
  round trip pairs the turned rows' samples, which comes after the turn (a half turn too, below). A keyed opacity (a `geq` alpha on `yuva420p`, pointwise) and a
  black fade on an opaque layer commute with the turn and are exact turned a quarter (`a_translucent_layer_turned_a_quarter_is_refused_and_the_other_turns_are_drawn`);
  a fade on a translucent layer and a dip to white on a rotated one (an alpha plane) are refused already.
- **A letterboxed layer is the whole frame.** `pad` emits a full-canvas frame, black
  bars included, so an identity clip that does not fill the frame covers what is below
  it, and `eq` grades the bars too (`LayerGeometry.matte`). `pad` also drops the last
  odd **column** of the picture, always, and the last odd **row** only when the `scale` ahead of it resampled the
  picture (it wrote into the buffer `pad` was handed, and `pad` fills the bottom bar in place from the chroma
  block's first row): a picture that is already the frame's width and 4:2:0 passes through `scale` untouched, and `pad`
  copies every row (`layer_geometry.rs` `resampled`; measured on 9.0.2: a 3-row picture shows 3, a 360x203 one
  `scale` made shows 202). Not at the frame's origin, though: with x = y = 0 `pad` pads in place whenever the decoder's frame is
  writable and has the room (`frame_needs_copy`), dropping the odd row of a picture one row short of the frame (64x35 in 64x36), and
  copies otherwise, which no plan can tell (asked of FFmpeg's own export per decoder: 6.1.1 keeps the row for FFV1, rawvideo,
  VP9 and MPEG-4 and drops it for ffvhuff and MagicYUV; 9.0.2 drops it for FFV1, ffvhuff and MagicYUV, keeps it for rawvideo and
  changes from frame to frame for VP9 and MPEG-4) — an odd-height picture at the origin that `scale` did not resample is refused
  (`Unsupported::Geometry`, every build). **Off the origin it depends on the graph, not the decoder** (`RenderPlan::odd_pad`). The
  decoder's frame has no room before it, so `pad` copies it (the row stays) unless a `scale` converted the picture into the
  buffer `pad` handed it, which is the graph's format negotiation: the filters that share `pad`'s format list (`setsar`) pass the constraint on, and the first that restricts it (`format=yuva420p`, the overlay's `yuva420p` input) decides what the
  `scale` before the `pad` converts to. **A Still plan** is `scale,pad,setsar` into an overlay that takes only `yuva420p`, so a
  plain `yuv420p` picture is converted and `pad` fills in place: the odd last luma row (16) and the chroma row under it (128) go,
  at any offset (64x5 to 64x33 in 64x36, 64x31 / 64x33 in 64x40; 6.1.1 and 9.0.2, FFV1 and ffvhuff), and `layer_geometry`
  shows one row less (`picture_shows`). The origin stays refused in a still too, as in an export. A colour correction, grade,
  effect, fade, mask, keyed number, HDR picture or other format puts other filters in the group, and the plan does not follow the
  negotiation through them: refused ("in a still whose chain is not a plain ... `scale,pad,setsar`"). FFmpeg's own stills, asked
  per kind (`checker_b_r4_still_other_chains_are_refused_and_ffmpegs_odd_row_in_each`, both builds): the row **goes** for an `eq`
  (contrast or brightness), a blur, sharpen, grayscale, invert, a chroma key, a keyed colour or a held keyed opacity or zoom, and a
  blend; it **stays** for a black fade in or out and for a grade (the plan's geometry would draw those exactly; they are refused
  until a case holds them, the black fade's 32 of 32 stills having been drawn exact under the scratch patch); an HDR picture is
  unread. At the origin FFmpeg's still drops the row for every decoder on both builds (`ORIGIN ROW` lines), and a still at the origin
  is refused all the same: the origin's refusal is the export's, which the same plan code states. **A Motion plan**'s chain ends in
  `format=yuv420p`, no conversion for `yuv420p`: a frame off the origin is drawn, and refused for any other format (a 10-bit
  picture's odd row goes there too on both builds) and for a chain on the alpha plane (a crossfade's incoming clip, a mask, a
  chroma key: its `format=yuva420p` makes `scale` convert, and 11 of 24 frames of a crossfade were inexact before it was refused).
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
  and round down to even (the 4:2:0 grid, whatever the source was), `rotate` rounds its box half-up; **only a quarter, half or three quarters of a turn is drawn** ("Rotation" below), where
  it lands on whole samples.
  **That geometry is fit, then zoom, and FFmpeg's graph no longer is for a zoom in**: a static zoom over 1 under Contain, and a Cover
  clip that zooms or pans over a cover that hangs over the frame, are one `scale` from the source to the frame times the zoom (and
  Cover no longer crops to the frame first), so the plan refuses them (`Unsupported::FoldedFit`, `core-render-plan.md`) and FFmpeg
  draws the frame. Zooms out under Contain (the PiP cascade: fit, then shrink) and everything with scale 1 and no pan are unchanged,
  and so are the coverage archetypes (the tutorial's keyed push-ins were refused as `Unsupported::KeyedPlacement` until the placement pass, see the ledger below). The parity cases follow: `geometry/zoom-off-canvas` and `pip/cover-then-scale` are held
  as refusals (with `geometry/pan-off-canvas` and `pip/contain-then-scale` keeping what they drew), and the grid cases
  `cover-odd-*-then-scale` are refused the same way. **Not run where this was written (no Vulkan adapter, no lavapipe)**: the refusals
  are plan-level and unit-tested in kerf-core (`planner.rs`, `render_plan.rs`) and the coverage test ran; the adapter suites
  (`parity.rs`, `motion_parity.rs`) were edited and compile but not run. Drawing a folded layer needs a one-stage
  `LayerGeometry` (`scaled` = the folded box, `keep` = the centred window a Cover clip keeps), the TS port and corpus
  following, and a parity case per shape on both FFmpegs.

**Text: FFmpeg rasterises, the compositor blends** (`text.rs`, `text/seq.rs`, `shaders/text.wgsl`, `GpuCaps::text`).

*Design.* `drawtext` is layout (shaping, kerning, hinting, the quarter-pixel phase of each glyph bitmap) and a blend; only
the blend is arithmetic a shader can copy, so the layout is never redone. **`drawtext` blends each glyph's bitmap in turn**
(a synthetic-bold border's first, then the fills), and a sample, or a 2x2 chroma block (each bitmap's own `sum >> 2` is blended), that
two bitmaps touch is two blends there: one mask of their sum is the same only while no two meet, which is never along the
letter gaps of a border (33 levels of chroma) nor in 3 to 11 px text (10), and a level or two at a few samples at 12 px and up
(measured with the sum on 9.0.2: 1 to 5 samples at 12 to 19 px, 1 to 2 in a missing glyph's box). So `text::raster(t, size)` recovers the
**sequence of bitmaps** from FFmpeg itself (`text/seq.rs`), and the compositor blends them in order:
(1) `measure` (cached per build, font file, size, text; a run that failed is not tried again for 30 s): the pen of every glyph unit and the box — *Shaped* (HarfBuzz: 6.1.1 and 9.0.2 measured): the debug log of
one multi-line `drawtext` with a prefix of the text per line (`Line: k -- glyphs count: g - width64: w - offset_right64: r`: a unit starts where
`g` grows, a ligature (Geist shapes `fi`, `ff`, `->`) is a unit of several characters, and its pen is `w - r`, 26.6 exactly). The lines are told apart by what they say and not by whose they are
(6.1.1 prints `[drawtext @ …]` whatever the instance is called, 9.0.2 `[drawtext@name @ …]`): the log is cut at each `Line: 0` and the group of exactly `n` lines numbered `0..n` is the instance's (the box probe logs
a group of its own), none or two that differ is a refusal. *Legacy* (4.4.2, whole pixels): the width of the one-row box of every prefix less that of the character alone, **one run** (a script file when the graph is long, `kerf_core::spill_long_graph`);
both read the text's box drawn at `y=ascent` for `text_w`, `text_h`, `ascent`.
(2) `dictionary` (cached per glyph unit): each unit drawn alone, fill and border, at the four quarter-pixel phases (4 frames of `x=X0+mod(n,4)/4`,
`y_align=baseline`, `y` = the phase) and where its bitmap lands against the pen and the baseline. **A shaped `drawtext` clips every bitmap to the text's box grown
by `bb`** (a pixel more than the border, the box padding with a box), and a unit's bitmap overhangs its advance (the hook of an `f`): the dictionary is drawn
in an invisible box with a wide margin or the hook would be cut in it and not in the middle of a text (found with DejaVu Serif 11 px, 1 sample).
(3) placement: `x64 = (int)((w*px - text_w/2) * 64)` (the graph's own expression, evaluated in doubles), a glyph's quarter-pixel phase is `((x64+pen64) >> 4) & 3`, the baseline
is `(int)(y*64 + offset_top64)` where `offset_top64` is the tallest glyph's top in 26.6, which only its pixel (`ascent`) is told: the (at most five) quarter-pixel
phases it can be are tried, likeliest first, each against (4).
(4) **proof**: the sequence blended in turn onto black must equal, sample for sample, the white-on-black picture FFmpeg drew for the same overlay at the same place
(`text_raster_graph`'s `Glyphs` part, border visible), else the text is refused (`GpuError::Unsupported`). Nothing of the layout is trusted — a ligature the shaper forms
differently in context, a tab, a glyph that touches the edge of its dictionary cell, a font whose bitmaps do not reproduce, all end here.
`Layers::of` flattens the sequence for the shader: per luma sample the coverage of each bitmap that touches it, in order (RGBA, four layers a pass), per chroma block each
bitmap's own `sum >> 2`, in order; `text.wgsl` blends the layers one after the other (a bitmap's zero is `((K - 0) * d + 0) >> 24 = d`, no change), so a sample costs what it
has bitmaps on it. The first sight of a state costs a few `ffmpeg` runs (measure, dictionary, the picture): 130 to 330 ms at 960 px in a debug build on lavapipe for a caption of a few dozen glyphs, and 1 to 2 s on 4.4.2 for 120 to 150 *distinct*
glyphs (the measure is an instance of `drawtext` per row, each opens the font: 0.5 to 1.2 s of it); the measure and the
dictionary are shared by every position of the same text, so a title sliding in costs one run a frame as before. A run is waited for 5 s and 25 ms an instance of `drawtext` it holds (20 s at most): one that does not finish is a refusal. `text::model()` probes which layout the build has (the debug log of
a two-letter `drawtext`: `width64` is Shaped, `text_w` Legacy; neither is `None`, retried after 30 s) **and how it converts the text's colour** (`text::build()`: `Build { layout, colour }`);
`prepare(plan)` asks it ahead of a frame with text, `Compositor::caps` only reads the answer (`text::measured_build`) and has `caps.text` only for a build it knows, so on any other every text frame is FFmpeg's.
*Colour:* 6.1.1 and 9.0.2 convert in floating point (`draw_color`, the matrix' coefficients); **4.4.2 in libavutil's fixed-point `RGB_TO_Y_CCIR` macros** (10 bits, BT.601 whatever the tag; `draw_color_ccir`), a level off in Y for some colours
(Aqua, Cyan, Ivory, LemonChiffon, Lime, MediumPurple, SkyBlue of the 140 names; 12 of the 83 the checker draws; lime at 0.5 opacity). The probe draws boxes in 14 colours the two give different planes for and compares each plane with what
each model predicts for the blend of the box over the canvas; a build that matches neither has no text drawn on it. (`Raster::colour` carries the model to `blends`, which refuses the fixed-point one for a matrix other than BT.601.)
The blend is `drawutils.c`'s integer arithmetic (the same in 4.4.2, 6.1.1 and 9.0.2), in u32:
`((K - a) * d + a * s) >> 24` (`K = 0x1010101`), `a` = coverage x `(0x10307 * alpha + 3) >> 8` for luma and the 2x2 coverage sum
`>> 2` for 4:2:0 chroma (a block sticking out of the bitmap counts only what is in it), `0x10203 * alpha + 2` for the box (half per
missing row or column at an odd edge); `s` is `ff_draw_color` in the composite's matrix (`PlanCanvas.matrix`: 9.0's `drawtext` reads it
off its link, 6.1's uses BT.601, which `FixedBt601` already says; limited range, `+ 0.5`, truncated) and the colour's alpha is
`(a * 256·opacity) / 255`. Colours are parsed as `av_parse_color` does (`parse_color`: 140 names, `#` / `0x` hex with alpha, `@alpha`
truncated), not asked of FFmpeg. Passes ping-pong between two canvases that agree outside the region the overlays can touch
(box, then glyph layers, one scissored pass each). The mask (`Raster::boxed`) is still one rectangle FFmpeg draws once per state
(`rasterize`, killed at `RASTER_TIMEOUT` 5 s, `limit_frame_args`); a mask is cached in `TextAtlas` (key: graph text, font file length + mtime, `ffmpeg` path; 256 states /
48 MiB, LRU, each state charged its bitmaps, its dense layers and, again, the textures made from them, the charge kept per slot and given back as it was; a state wanted by several frames runs once; the atlas
drops a failure, so `text::raster` keeps a *refusal* (anything `Unsupported`: a proof that did not match, a face FFmpeg could not open, a timeout) for 30 s under the atlas key, asked before the first `ffmpeg` of the rasterisation, so a scrub over Arial Narrow, a script face or RTL text spawns nothing per frame (`tests/text_refusal_memory.rs`: a wrapper darkens the first run's picture so the proof refuses, then counts the runs of three more asks: none) — an animated position is a new key per frame, and the measure and dictionary behind it are remembered by text and size alone; textures are made by the first frame that
draws one). A drag's frame (`render_draft_texture_with`, the app's draft frames) is drawn exactly as any other, but a title state the
atlas does not hold is a pose of the drag (a new one per pointer move, never drawn again): it is made into a second atlas of 4 states /
16 MiB (`TextAtlas::get_drafted`, `peek`), so a drag never pushes the cut's own titles out. `prepare_text` blocks on those processes for a first-seen state, so the caller must hold no lock the UI needs while it renders
(the app's frame holds neither the project lock nor its render lock then, `app.md`). **A frame with no layer is drawn in RGB** (FFmpeg never makes the black base YUV), so
the same shader blends on the RGB picture and `composite_yuv` refuses such a plan rather than return planes without its text.

*Verification.* `tests/text.rs` holds the sequence blended by the scalar reference (`blend_coverage` / `blend_rect`) to `drawtext`'s own `yuv420p` planes
**sample for sample, 0 differing and no allowance, on all three FFmpegs** (4.4.2 Legacy, 6.1.1 and 9.0.2 Shaped): plain, boxed, odd offsets, edges, corners, wholly outside, opacity 0 to 0.99, hex / named /
translucent colours, BT.709 and BT.2020, escapes, Cyrillic, missing glyphs (their outline boxes were 1 to 2 samples off as a sum), every size 3 to 24 px, a synthetic-bold border at 10 to 100 px (sentences, tight pairs, ligatures, translucent, boxed,
at the edges, at eight quarter-pixel phases) and four installed faces (DejaVu Sans / Serif, Ubuntu, Ubuntu Mono; plain and bold at 11 to 40 px) — each asserting the
sequence was recovered (`Raster::exact`). `text_checker.rs` and `motion_text_checker.rs` are an independent check of the same (20 and 4 tests, plus `text_timeout.rs`: tabs, long texts, shapers that do not give a glyph a character, 22 named faces and a sweep of every installed font file, colours, sizes, canvases,
positions, first sight): every case exact or refused, none drawn as a sum. `the_text_pass_is_the_reference_blend_plane_by_plane` holds the shader to the reference on the compositor's own planes
(0 differing samples, 9 frames, 6 overlays, among them a border and 9 px print); `motion_parity` holds the export's own planes to the compositor's for small print and a
thickened bold over a fade, sliding and fading (0 differing samples in 8 cases of 60 frames); the `text/` parity cases hold the whole picture to the still, each against
the same picture without the text, as before.

*Refusals.* **Nothing is drawn that is not proven**: the one mask of the sum, which `raster` once blended where the sequence could not be recovered for a large plain text, was up to 30 levels off exactly there
(a tab, over 160 characters, a canvas too wide to measure, a glyph that does not shape one to a unit, Arabic and Hebrew, faces whose bitmaps overhang) and is gone; `text::rasterize` still makes it, for a test to measure, and `sum_fallbacks()` is 0.
The plan refuses what it can tell (`Unsupported::TextSequence`, kind `text_sequence`: `kerf_core::text_sequence_limit` — a tab, over `MAX_SEQUENCE_CHARS` (160) characters, a text too wide for `MAX_MEASURE_SIDE`) and, without `caps.text`
(a build whose layout or colour conversion the probe does not know, or has not run on), refuses every text (`Unsupported::Text`); a border (`TextBorder`) and print under 12 px (`TextTooSmall`) are no longer refusals of any kind.
The render refuses the rest (`GpuError::Unsupported`: FFmpeg draws the frame): `seq::sequence` must reproduce the picture exactly; a run that fails, times out or writes too much; a face whose bitmaps touch the edge of the dictionary's cell
(a script's swashes reach an em left of the pen and half an em above the ascent: the cell is 1 em left, 1.8 above, 1 below). Measured with the checker's 22 faces: Windows script and display faces are refused at many sizes
(Segoe Script, Forte, Brush Script, Gabriola, Lucida Handwriting), and **Arial Narrow's Latin text on the shaped builds**, whose proof fails from the first glyph that follows a kerned space (A, T, f): suspected to be a GPOS placement (`x_offset`) that the debug log does not carry
(pens are `width64 - advance`, the offset moves the bitmap and not the pen). Right-to-left text is refused on all three builds (4.4.2 reorders the whole string with fribidi; the shaped builds join and reorder). **Colour fonts**: `drawtext` loads glyphs without `FT_LOAD_COLOR` and has no BGRA path — a CBDT font (Noto Color Emoji) is a hard
error on 6.1.1 (a segfault) and 9.0.2 at every size, so there is no colour glyph to draw as a silhouette; a COLR font is drawn by
`drawtext` itself as monochrome outlines and the bitmaps are exactly that (sbix and COLR were not available to test). Known costs, not fixed:
the atlas key holds the sampled position at full precision, so a **keyframed slide-in rasterises per frame** (quantising it risks
exactness), though its measure and dictionary are shared; a Windows profile path with non-ASCII characters reaches `fontfile` through FreeType's narrow open (not run on Windows). Over a full-range picture on 9.0 (text drawn in full range, converted last) the picture is within 2 levels of the control: drawn.
**Builds differ**: 4.4.2 lays text out in whole pixels (`s->x`, `s->y` are ints, bitmaps rendered once at the origin, no clip window, `FT_Get_Kerning` of the `kern` table only) and converts colours in fixed point; 6.1.1 and 9.0.2 in 26.6 with HarfBuzz
(`s->x` a double, a bitmap per quarter pixel, `ascent` and `text_h` ceilings of 26.6 values, the clip window above) and in floating point; 6.1.1 already has the HarfBuzz layout (it was taken for the legacy one until a build was run) and 4.4.2's `drawbox color=red` writes 254 on `rgb24`
(`colours_parse_as_ffmpeg_reads_them` reads colours through the `color` source for that reason).

**Animated SVG titles (floating ones drawn: `svg.rs`, `shaders/svg.wgsl`, `compositor/svg_pass.rs`).** ffmpeg never sees an SVG: `svg_layer.rs` draws each title with resvg into a straight-alpha RGBA PNG (`picture_png`)
and the graph `overlay`s it (`[cur][N:v]overlay=x:y`; an export adds `settb`/`setpts`, `eof_action=pass` and `enable='between(t,start,end)'`). The same resvg draws the same pixels in the compositor's
process (the plan carries the artwork, fonts, placement, opacity and the document's clock: `core-render-plan.md`), so nothing of the artwork is a parity question. The rest is `overlay`'s: on a `yuv420p` composite it takes the picture as `yuva420p`, converted by an auto-inserted `scale`, and blends
with `vf_overlay.c`'s `blend_plane` (identical in 6.1.1 and 9.0.2: `FAST_DIV255(d*(255-a) + s*a)` per sample, `a` the picture's alpha for luma and, for chroma, the 2x2 average
`(a+a+a+a)>>2` — at the last chroma column or row `(alpha_v + alpha_h)>>1` of what there is, which ignores a last column's own second luma column — and **the position rounded down to even**, `s->x &= ~((1 << hsub) - 1)`: an odd offset lands where the even one below it does, negative ones too;
a picture off the frame is clipped on every side, an odd one ends in a half chroma block whose chroma, not its luma, reaches one pixel past it). `tests/svg_overlay.rs` holds this to the real `overlay` (**the planes ffmpeg converts the PNG to inside a
`-filter_complex` graph of the same build, blended with that arithmetic, are the overlay's output with 0 of 9216 samples differing**, 6.1.1 and 9.0.2), so the compositor
needs no model of swscale (RGB to YCbCr coefficients, the 2:1 vertical chroma filter, the x86 kernels): as text asks ffmpeg for the glyph bitmaps, it **asks ffmpeg for the planes of each picture**
(`svg::convert`: `-f image2pipe -c:v png -i pipe:0 -filter_complex [0:v]format=yuva420p[o]`, `Budget::ffmpeg` so it is capped to the CPU budget's threads, bounded by a timeout and killed past it) and blends them. The trap
the test records: a *simple* graph (`-vf format=yuva420p`) converts differently on some builds (87 to 1472 of 9216 samples away), so the conversion is asked of a `-filter_complex` graph.
**Drawn**: the plan (`caps.svg`, `Unsupported::SvgOverlay`) says yes for a floating title over at least one clip on a BT.601 composite where `svg::svg()` measured a `Match`; the compositor
(`prepare_svg`, then `draw_svg` after the layers and before the text, the export's draw order) takes each title's picture from the `PictureCache` (keyed by the FFmpeg that converted it and by what decides the pixels — the document's text at that moment, size, opacity, fonts, rasteriser version —, so a title that holds still between its moves
and a document that cannot move are one picture; 64 pictures / 160 MB, least recently used out, **one flight per picture with no lock held while it is made** — a flight is a marker in its slot, the askers that arrive wait on the slot's condition for at most 30 s each and take the flight's result, a failure included, and a maker that panics ends its flight so a waiter makes the picture —, a failure remembered for 20 s so a failed child is not respawned by every frame, and a resvg panic is such a failure (`svg::make` catches it); a `clear` leaves the books right: a flight that began before it keeps and remembers nothing), makes it on a miss (one resvg draw, one PNG, one `ffmpeg` child) and blends it with one pass per title over a copy of the
canvas (`svg.wgsl`: the pass reads the copy, writes the region the picture can touch, the luma where the picture is and the chroma of every block it reaches). `Compositor::warm_svg` makes a frame's pictures ahead (`render_motion_texture` does it beside the decode), and `warm_svg_ahead` is the playback prefetch's: it makes a picture only where it fits without pushing out one a prefetch made that no frame has shown yet (`PictureCache::warm`, `Warmed::NoRoom`; a picture is due for 10 s), because the window it looks ahead (1.5 s) can be more pictures than the cache holds; **during a play** a frame whose picture is not the one before it (`PlayRefusal::AnimatedTitle`) is the stream's, a title that holds still is drawn (`core-render-plan.md`); `overlay_picture` runs the pass on planes
you give it, to hold the shader to `svg::blend_onto` (the reference) sample for sample. **The probe** (`svg::svg()`, `measure::cached`, about twenty short children, run by `prepare` / `prepare_timeline`): the export's own chain text (`kerf_core::svg_overlay_chains`) over a test composite with a PNG of every alpha, even and odd sizes, offsets odd, negative and past the edge,
every sample against the converted planes blended by the model; `Differs` / `Unknown` leave `caps.svg` off (`Compositor::with_svg` pins it; a test of `a_blend_one_level_off_is_a_difference_and_not_a_match` shows it has teeth). **Refused** (parity cases `the_apps_svg_presets_...`, `a_title_at_any_size_and_offset_...`, `a_translucent_title_...`, `a_title_that_moves_...`, `several_titles_...`, `a_title_over_fades_...`, `a_title_the_compositor_does_not_draw_...` in `parity.rs`, the `svg/export/` cases in `motion_parity.rs`):
a **title clip** (a generated clip on a track), which is not an overlay but a clip chain — fit, fades and transitions on an alpha picture, then `overlay` over the lower tracks, through the clip chain's own `scale`/`format` conversions and its alpha-plane fades, none of which the cases here hold (the `alpha_picture` capability the ledger counts apart); a frame with **no clip** (the black base is no YUV composite); a **composite that is not BT.601** (FFmpeg 9 ties the overlay's second input to the first's colour space, so a title over a BT.709 or BT.2020 bottom layer is converted with that matrix: measured, 1215 to 1290 of 9216 samples off the untagged conversion; BT.601 tags `smpte170m` / `bt470bg` convert as untagged); a picture over 2 x 4K pixels or the device's texture limit (render-time `Unsupported`).
`svg::convert` takes any number of pictures in one child (held equal to one child each by a test); the compositor asks for one at a time, so every new picture of a sequence is a child that `warm_svg` makes ahead of the frame.

**The runtime self-test** (`selftest.rs`, `parity_self_test(&Arc<Gpu>)`). Parity is proven in CI on two FFmpeg
builds and one software adapter; a user has a third FFmpeg and a real GPU, so the first use of the GPU path
(the app: `app.md`) draws up to six small frames on the compositor and through that machine's own FFmpeg and holds
them to **its own coarse screen** (`PSNR_FLAT_MIN` 40 dB, `MAX_FLAT` 8, `PSNR_ALL_MIN` 40, `FLAT_SHARE_MIN`
0.5, an edge band: `selftest::compare`). **This is not the parity claim and is not held to it**: `tests/parity.rs` and `motion_parity.rs` hold every drawn sample to zero; the self-test only has to catch a GPU or a build that draws grossly something else on a machine CI never saw, and a false alarm closes the GPU path for someone it works for (`tests/parity.rs`' copy of these limits and the test that held them equal are gone with the tolerance they served). The frames:
`fit` (one untagged layer enlarged 4:3: swscale's tables, the final conversion), `pip` (a gradient with a shrunk,
cropped, 80%-opaque, graded picture-in-picture: the scaler down, `eq`, the opacity round trip, `overlay`) and
`bt709` (a BT.709-tagged stack: the composite colour policy the plan was given, which is how 6 and 9 differ) and
`grade` (a 640x360 untagged clip through a 5^3 LUT, curves and wheels: the x86 conversion kernels of this machine's FFmpeg, which is what `grade::kernels()` also asks) and
`place` (a clip whose zoom and position are keyed, at a moment inside the move: `pad`, `perspective` and `crop`) and
`pip-odd` (the `pip` at 213x133, both sides odd: the odd round trip below; **drawn only where the round trip matched, the scaler is the model and `roundtrip_probe::odd_translucency_for_selftest()` found a model**, which the compositor is pinned to like the others (`Compositor::with_odd_translucency`), and last in the list so that `starts_with(["fit", "pip", "bt709"])` still holds). **The placed frame is drawn only where `placement_probe::placement()` is `Match`**
(the plan refuses a moving zoom otherwise, so there is no frame; `placement_for_selftest()` holds the placement back on an `Unknown` that survives a second try after the backoff, through the shared `measure::checked` (the grade is held back the same way, without the second try); `run` takes the answer as a parameter, and
`a_machine_whose_conversions_differ_or_are_unknown_is_checked_on_the_other_three_frames` rehearses `Differs` and `Unknown` for both). `a_compositor_that_did_not_move_the_picture_would_fail_the_placed_frame`
holds that the frame has teeth: the same clip drawn without its keys fails the check's limits.
**The scaler is pinned and held back like the grade** (`scaler::kernels_for_selftest()`, `Compositor::with_scaler_kernels`): the frames above all resize, and the plan refuses a resized layer on a scaler that is not measured to be swscale's (`ScalerApproximate`, the GPU draws none there), so a self-test that drew them on an arm64 FFmpeg (a scaler that `Differs`) or after a probe that timed out would stop on its own refusals and close the whole GPU path. Where `exact()` is false the clips are made at the canvas' size and the layers are only moved, faded and graded (`a_machine_whose_scaler_differs_or_is_unknown_is_checked_on_frames_it_does_not_resize`); an `Unknown` holds the scaler back for the process (`held_back`) until a self-test that can tell has run. **A render never runs a probe**: `plane_filters`, the grade, the matrix conversion, the resize gate and a mask's or a keyed opacity's `geq` (`kerf_core::geq_edge_measured()`) read what was measured, the same answer `caps()` was made from (no `mask` or `keyed_opacity` in it while the `geq` is `Unknown`) (`prepare` runs them, off the render lock, for a plan that needs them: the grade's conversions for a grade, a dip to white or a conversion between matrices, the scaler for any layer, `roundtrip_probe::translucency()` for a layer below full opacity (anything but `Match` is `GpuCaps::translucent_approximate`), `blur_probe::blur()` for a blur (the blur is in the caps only on `Match`) and `display_probe::display()` for any plan with a layer (no RGB is handed out otherwise); the playback stream and the render bar call `prepare_timeline` once, when a session is made ready or begins, which measures what the whole cut can need — the grade's conversions, the scaler, the matrix conversion, `geq`, `drawtext`'s layout, the blend chain for a blended clip, the translucent round trip for a clip below full opacity or with a keyed one, `gblur` for a clip with a blur, the display conversion, the placement and planar-conversion probes for a clip whose zoom or position moves — and a session adds `prepare_placed_files(&cut, &timeline)`, which asks the file of every placed clip for its range flag (the part that depends on what the cut resolves its clips to, a proxy included; a file already asked costs a `stat`; four at a time, no new one after 20 s, in track order; a placed file of the first horizon it left unasked is then asked by `prepare_ahead`, one at a time), so that no frame of the loop waits on a probe).
**The grade's frame is drawn only where `grade::kernels()` is `Match`**: the plan refuses a graded layer otherwise (`Compositor::caps()`, below), so there is no frame to draw, and a self-test that tried to would stop on `Inconclusive` ("the plan refuses its own check frame") and close the whole GPU path on a machine whose FFmpeg merely differs (an arm64 build, or a probe that timed out) even for ungraded frames. `parity_self_test` asks `kernels_for_selftest()` first, makes no graded clip, and the other three decide the check; `run` takes the answer as a parameter (`a_machine_whose_conversions_differ_or_are_unknown_is_checked_on_the_other_three_frames`: `Differs` and `Unknown` pass on `fit`, `pip`, `bt709` alone; the wrong colour policy still fails `bt709`). **A probe that could not tell holds the grade back**: `kernels_for_selftest()` returning `Unknown` sets `held_back` in the probe's state, and while it is set `kernels()` / `measured()` answer `Unknown` without probing, so a probe that succeeds later cannot open a grade the check never looked at; only a later self-test lifts it, and `Check::retry()` re-runs a failed check only, so after a self-test that passed with `Unknown` the grade stays on FFmpeg until the app restarts or the adapter changes. A `Differs` is permanent and holds nothing back.
No rotation: its float sampling can differ by a level between GPUs for reasons that are not drift. The clips are
made by that FFmpeg itself (FFV1 in Matroska, three of them in parallel, 480x270, 0.4 s, probed as an import
probes), the references are `export_still`, the GPU frames come through a private `FrameSource` (`Hint::Scrub`,
the preview's own), and everything runs the way interactive reads do: thread-capped children (`limit_frame_args`),
no machine-budget slot, nothing under a project lock; it took 1.9 to 2.7 s on FFmpeg 6.1.1 and 1.0 to 1.2 s on 9.0.2
on lavapipe with three frames (release build, a loaded 4-core box; `docs/review-2026-10-10/after/gpu-5`), and 3.6 s and 1.7 s with the fourth, the grade (the same box, loaded); with the fifth, the placed frame, and the placement probe it waits for, 4.9 s and 2.8 s in a debug build on a box at a load average near 9 (the placement probe alone 1.7 s and 1.1 s; 5.8 s and 4.0 s at a load average above 20). The answer is `Ok(report)`, **`SelfTestError::Mismatch`** (the GPU draws something else than this
FFmpeg) or **`Inconclusive`** (an FFmpeg that would not make the clips, a colour policy that could not be
measured, the plan refusing its own frame, the GPU failing on it, the 90 s deadline). **The deadline is looked at between
steps** (before the clips and before each case), not inside one: a case begun just before it still runs its reference
(`export_still`, a 120 s timeout of its own) and a conversion (30 s), so the worst case is about four minutes, during which
every frame goes through FFmpeg as before. What the app does with the answer is in `app.md`. Measured on lavapipe, both
FFmpegs: fit, pip, bt709, grade, place and pip-odd all 99.0 dB / max 0 (drawn exact now; the figures of the screen were once 45 to 49 dB / max 3 to 6), and the same place clip drawn without its keys 6.6 dB / max 255; the same GPU
drawn under the *other* colour policy fails the bt709 frame at 24 dB / 40 to 42 levels (that is the 6-versus-9
drift the test is for; `a_gpu_drawn_under_the_wrong_colour_policy_fails_it`).

**Fades, dips, dissolves, slides and pushes** (`composite.wgsl`, `roundtrip.wgsl`, `motion.rs`; `GpuCaps::{motion, fades,
transitions, dip_white}`). The plan hands each layer's `fade` steps as integers (`RenderPlan::fade_ops`: tint, edge and the
factor `65535 * i / N` the filter multiplies by; `65535` is "does not run", not even a rounding) and the compositor applies **`vf_fade`'s own
arithmetic** (`kerf_core::fade_luma` / `fade_chroma` / `fade_alpha`, the shader's copies): luma `((Y - bl) * f + (bl << 16) + 32768) >> 16`,
with the black level `bl` **16 on `yuv420p` and 0 on `yuva420p`** (`RenderPlan::on_alpha_plane`: a rotation, an opacity, a mask, the dissolve the
clip entered through), chroma `((C - 128) * f + 8421367) >> 16` (the constant is nine short of the obvious one, and a plain `+ 32768`
is a level off at a few factors), alpha `(A * f + 32768) >> 16`. `fade_arithmetic_is_ffmpegs` checks it against the filter on
every sample value of every factor of eight rates / starts / lengths, in and out, `yuv420p`, `yuva420p` and `alpha=1` (1566 frames): the *filter* is identical on 6.1.1 and 9.0.2 (what 9.0.2 does to a full-range picture around it is not, below). The fades run in
`fs_compose`, **after** the rotation and the letterbox matte (they are part of the picture the filter sees) and on the 8-bit picture,
the clip's own fade-in and fade-out in turn (a dip to black's halves are those). A **dissolve is two ordinary layers**: the outgoing clip on its tail
(a plain layer, from the handle it borrows) and the incoming one on an alpha ramp, which `overlay` blends as `FAST_DIV255(d * (255 - a) + s * a)`
(`(x + 128) * 257 >> 16`, measured exact on all three planes for a uniform alpha, both builds). A layer whose alpha is below 255 is
therefore blended **in integers** (`fs_compose_exact`, over a copy of the region it touches, since a pass cannot read what it
writes) instead of by the hardware's blend, whose rounding is the device's own (so the same plan would differ from FFmpeg's by a
different amount on every GPU); an opaque layer keeps the hardware's, which is exactly the source. The translucent layers take the
integer blend too and are the export's to the sample (see "The translucent round trip" below). A slide or a push is a position (the geometry already
adds the travel to the overlay's, truncated and rounded to even as `overlay` does) and the outgoing side of a push is a tail
that travels. **The alpha ramp of a translucent incoming clip comes ahead of the 256/255 scale** (an opacity below 1 leaves
RGBA, `fade alpha=1` runs on it there): `((a * f + 32768) >> 16) * 256 / 255`, not the ramp of the scaled alpha. **A dip to white** is a filter
that takes RGB only, so FFmpeg converts `yuv420p -> rgb24 -> yuv420p` around it (in every frame of the clip in an export, in a still while the fade
runs): the compositor runs **the grade's conversions** around the RGB fade (`((c - 255) * f + (255 << 16) + 32768) >> 16`, exact against `rgb24`):
swscale's x86 `ff_yuv_420_rgb24_ssse3` kernel in (`grade_passes.rs` `white_planes`, `X86Kernel`) and the generic scaler out
(`untagged_back`: `rgb24ToY_c`, the half-summed chroma and `yuv2yuvX` with the last chroma row in C), the arithmetic `tests/grade.rs` holds to both
FFmpegs on random pictures. A dip to white is therefore **exact** (0 samples, every plane, every frame: `motion_parity.rs` `WHITE` / `WHITE_709` are
`EXACT`, and `checker_b_r4_export_dip_to_white_frames_the_plan_draws_are_exact` / `..._still_dip_to_white_stills_...` hold it to nothing), where the probe
says this FFmpeg converts as the grade's model does (`grade::kernels() == Match`: `caps.dip_white` is that, and the render checks it again). It used to
follow the C tables of the translucent round trip (`yuva420p -> argb`, no SIMD version), which are up to two levels from the kernel's (three of Y on
BT.709) — a dip drawn within a bound is not drawn. **Measured on x86-64**: the arm64 kernel the macOS bundle runs makes the probe `Differs`, and the dip is
FFmpeg's there. **Where it is refused** (specific reasons, `Unsupported::FadeOnTranslucent` / `DipToWhite` / `FullRangeNegotiated`): a black or white fade
running on a layer below full opacity (`colorchannelmixer` leaves RGBA and `rotate` and `fade` run on that, **alpha plane included**: Y 153 levels off the
first time it was drawn like a plain fade — the harness found it); a dip to white on an alpha plane (FFmpeg picks another RGB format) or beside a running black
fade of the same clip; on any picture that is not `yuv420p` (RGB, 10-bit, 4:4:4, 4:2:2, gray, `yuvj`: 9 to 21 levels off **in an export's planes on 9.0.2**) or whose matrix is not BT.601 / BT.709;
on a `yuv420p` **flagged full range** (`DipWhy::FullRange`; the export's round trip is in the picture's range on **every frame of the clip**, not only while the fade runs, and the compositor's tables are
limited-range's: Y 2, UV 1 off on 6.1.1, on untagged, BT.601 and BT.709 alike; under FFmpeg 9 `FullRangeCut` refuses the whole export anyway); under an **`invert` or a `vignette`** (`DipWhy::RgbEffect`; of
the clip chain's effects only `negate` and `vignette` list an RGB format, so the fit `scale` ahead of them is the filter that converts to RGB and FFmpeg runs the effect there, in an export and in a
still, on both builds: invert Y 3, vignette Y 47 off. Asked of FFmpeg's own graphs: `gblur`, `unsharp`, `hue` and `eq` list none, and the conversion is the fade's own `scale`);
on a **letterboxed** layer (FFmpeg pads before the round trip, so its bars whiten: Y 126 against 16 at half way, where the compositor's matte stays black); on a picture
with an **odd** side (the round trip reads past the line); and on a layer the **scaler resizes** (a layer that is only cropped is exact): the `scale` filter is handed
`rgb24` by the fade's negotiation and scales and converts in one pass, chroma interpolated from other samples than the compositor's scale-then-convert (a half-size layer
Y 19, UV 28 off at sharp edges, an enlargement 11 / 10) — found by the harness comparing the frames beside a refusal. **A full-range (`yuvj`) picture is refused
whenever the composite policy is not `FixedBt601`** (FFmpeg 9; `PlanStream::maybe_full_range_in(policy)`, which also counts **every deeper-than-8-bit YCbCr or gray format** there, because the
name is only a heuristic and the range *tag* is the truth, which `StreamInfo.color_range` records since schema v10 (`pc` / `tv`, `None` for a file that says none and for an asset stored before): a 10-bit file tagged `pc` probes as `yuv420p10le` like the limited one, and 9.0.2 handles it as it does `yuvj` —
Y 16 off in an export's planes, flat 16 / 15 / 11 under a still's fade — so a **YCbCr** picture **flagged `pc` is refused in any format** there (an RGB or gray one is never a flag: ffprobe prints `pc` for them on both FFmpegs, and counting it refused every PNG under FFmpeg 9 — the suite found it), and with no flag recorded the **limited 10-bit picture is refused with it** (the depth heuristic stays for `None` and `tv`: a file may say nothing); FFmpeg 6.1.1 draws both exactly: Y 0 / UV 0 in Motion, 3 in a still's fade, so the name alone decides there, as before — **except a `yuv420p` flagged `pc`** (below): in a still, while a fade, dip or dissolve runs on it (`fade` darkens the *raw* values toward the
limited-range black 16 and converts the range afterwards: frame 0 of a fade-in reads Y 30 where black is 16, 15 to 19 levels off over the fade), and **every export frame of a cut that holds one**
(`Unsupported::FullRangeCut`, from `PlanCanvas::full_range_cut`, which the `Planner` sets from the cut's clips): FFmpeg 9 flags the whole export full range, keeps the full-range
clip's raw values (Y 17 / UV 16 off a compositor that converts first) and **converts every limited-range frame of the cut to full range** (a black frame reads Y 0 where the compositor draws 16, a test pattern
76 against 81, a gap too), before the clip, after it and on another track alike, so no frame of such a cut is drawn, not only the full-range layer's. **A `yuv420p` flagged `pc` (`color_range=pc`; a VP9, HEVC or AV1 file, FFV1 in Matroska; `PlanStream::raw_full_range`) is not a `yuvj` picture on FFmpeg 6.** Its format is already the overlay's, so
nothing converts its range ahead of the graph's filters and its **raw samples reach `fade`, `eq`, `vignette`, `chromakey`, `unsharp`, `gblur` and the rest on both FFmpegs**, where the compositor's decode (`scale=out_range=tv`) has converted it. Measured with the range flag cleared
(what a pre-v10 asset's plan sees), 6.1.1 / 9.0.2: a still's vignette 13 / 12 levels off, a fade 11 / 9, `eq` within the bounds / 11, the chroma key within them / 19 on a gradient (73 on the test pattern); an export's planes on 6.1.1 a fade Y 14, vignette 10, invert 4, `eq` 6, sharpen 20, blur 1 and a resize UV 8; on 9.0.2
every export frame of the cut 16 off (the flagged-export reason, as for `yuvj`). A plain layer, a cut and a shrunk layer over bars are exact on 6.1.1 (and, in a still, on 9.0.2 where the picture is limited range; a full-range one in a still is refused there, `FullRangeStill`). So `maybe_full_range()` is true for it (the blur / sharpen / invert / vignette / key / `eq` refusals, `FullRangeResized`, and
`FullRangeCut` where the policy is not `FixedBt601`), and `FullRangeNegotiated` fires **under every policy** for it, in a still and in the export frames a fade runs in (under FFmpeg 9 the cut is the export frames' reason). `Unknown` and `tv` leave the picture as it was. **The policy is a proxy for the behaviour, measured on
FFmpeg 6.1.1 and 9.0.2 only: 5.x, 7.x, 8.x and distro builds are unmeasured** (`composite_color_policy()` probes the composite's colour, not the range handling; a build that negotiates the colour but
not the range would be refused needlessly, one that does the reverse would be drawn wrong). FFmpeg 6 converts
at the decode, as the compositor does, and draws all of it to the level — **unless the scaler resizes the picture** (`FullRangeResized`, both FFmpegs): swscale converts the range inside
the resize, and an export's chroma reads up to 15 levels off (a fade on a 1.5x enlargement: flat 14), so a resized full-range picture is refused in an export frame, inside a fade and in a still (`FullRangeResized`: a still of one was drawn "inside the strict bounds, 5 flat" once, which was a tolerance, and read thousands of samples off). **The dip-to-white refusals are the Motion plane's and are conservative in stills on purpose**: with
them bypassed, RGB, 10-bit, 4:4:4, 4:2:2, gray, BT.2020, full-range and half-size layers were within a few levels of the still (measured against a bound that is gone, not to the sample), and `reasons()` is one function for both plan modes: a refusal
is not drawn approximately. **Export frames** (`PlanMode::Motion`, `caps.motion`) pick source frames with the `fps` filter
(`Pick::Fps`), which a `FrameCursor` serves; `MotionFrames` (`motion.rs`) is the per-clip bookkeeping: a cursor opened the first
time a clip is on screen, handed the pick of each later frame in order, dropped when the clip leaves the plan, reopened for a frame earlier than
the last it answered. A still plan (what the app's preview and `FrameSource` serve) holds the same fades, travel and tails with the still's `-ss` picks, so
a paused frame inside a transition is drawn like the file shows it — where the FFmpeg *still* used to be the only thing that did.

**A moving zoom last, masks and effects** (`composite.wgsl`, `shaders/planeops.wgsl`, `shaders/blur*.wgsl`, `effects.rs`;
`GpuCaps::{keyed_zoom, mask, effects}`). FFmpeg's chain for a clip with a **moving zoom** is `crop, fit, setsar, fps, eq, effects,
format=yuva420p, mask, rotate, fades, scale eval=frame` (`video_clip_chain`; `still_clip_chain(.., zoom_last)` for the still): everything
before the zoom acts at the **fit size** and the zoom magnifies it (a blur grows, a hard mask edge softens).

* **A moving zoom or position is drawn by the placement pass** (`GpuCaps::placement`; see "The placement" below): the export and the
  FFmpeg still end such a clip's chain in `pad`, `perspective` and `crop`, and the compositor reproduces those after everything else
  at the fit size. A layer is still refused (`ZoomBehind`) when a rotation, an opacity below 1 or a dip to white is ahead of
  it, for a moving position as for a moving zoom, for an odd-height picture or a pose out of the table's range (`Geometry`), for a layer that is not
  4:2:0 or gray (unless the probe says the graph converts 4:4:4 and 4:2:2 as the decode does: `GpuCaps::placed_chroma`), or may be full range where the
  composite's range is negotiated (`PlacedFormat`, below), and for a letterboxed picture at the identity pose in a still (`Geometry`: the still pads it to
  the whole frame first, with opaque bars). **A grade ahead of the placement is drawn** (where the compositor draws the grade at all: `GpuCaps::grade`, which
  the conversion probe gates): all twelve grades of the harness (wheels, curves, LUTs of 17, 33 and 2 points, all three) are exact behind a moving zoom and a
  moving position on 6.1.1 and 9.0.2, in an export frame and in a still.
* **Moving stills (Ken Burns)** (`GpuCaps::still_motion`: `StillKinds`, `still_probe`; `core-render-plan.md`, `engine-cli.md` "A moving still"): a still image
  whose zoom or position moves is drawn by **FFmpeg making the picture and the compositor moving it**. The moving still's route (the move stays inside the
  frame) is the export's scaled copy of the file (`scale` to `q` with the delivery's scaler, Lanczos by default, `crop` to the window any frame reads:
  `kerf_core::StillMotion::once`) and one `perspective` per frame; the compositor asks FFmpeg for the copy (`source::copy_of`: the engine's own `once` text
  then `format=yuva420p`, which is the format the graph's negotiation gives the chain, since `perspective` and `overlay` both take `yuva420p`, so the
  scaler converts the range of a JPEG or the RGB of a PNG in the same pass that resizes it; decoded once and cached beside the file, keyed by the filters)
  and draws the `perspective` from its own table (`PlacementMap::from_corners` over `PlacementFrame::window`: the same shader and tables as the placement,
  the filter's `lrint` and 8-bit fixed point, from corners the plan evaluates in the expression's operation order). The result is a frame-sized layer; the
  fades, the dissolve and the travel of a slide or push act on it after, as the graph has them (`composite_texts`' `moved_of`, ahead of the layer's stages).
  A move that leaves the frame bare is the general chain's (a moving zoom with a placement): the compositor draws that placement as before, and **FFmpeg makes the
  fit picture** it is laid from (`kerf_core::FitCopy`: the geometry's `crop` / `scale` / `crop` stages as text, then `format=yuva420p`), which lifts, for an
  `yuvj420p` or `rgb24` still, the resize of a full-range or RGB picture, the scaler's one level and the chroma grid of the crop (all FFmpeg's own).
  **The export draws the frame at the delivery's size**: an export frame's copy is made for it (a render at another size is refused); a still plan's copy depends
  on the size it renders at (`StillMotion::build` over the instant), so `RenderPlan::for_render(size)` works it out (every entry point that decodes does, `for_size`, a fit picture included; `composite_*` from a plan that is not worked out is refused).
  `still_probe` (the `measure` slot and backoff of the other probes) is what lets a **chain** of a kind into the caps (`StillKinds`: one bit per kind and
  `kerf_core::StillChain`). A chain is the shape of the text FFmpeg is given for the copy, and each is measured by a toy cut that takes it and no other (the plan of
  each toy is asked which chain it takes): the move inside the frame, the engine's own fit of a Cover frame (`FitShape::Engine`), that behind the clip's static `crop`
  (`EngineCrop`), and the geometry's `scale` stages after a guard `crop ... exact=1` and with **no other `crop`** (`Staged`: a Contain frame, a held zoom out of a picture of the frame's
  shape), each with the delivery's scaler named (`:flags=bicubic`, spelled as the engine's own list has it: `Bicubic` names none) or not: eight chains a kind (`StillChain::ALL`).
  **The move inside the frame under a named scaler** (`bilinear`, `lanczos`, `neighbor`, `spline`: `:flags=<name>` in every `scale` of the text) is a ninth to twelfth thing a kind
  has, a bit of its own (`StillKinds::named_route(kind, name)`: 4 names x 3 kinds, after the chains' bits; `StillKinds` is a `u64`) measured by `still_probe::verdict_route`
  / `ensure_routes`: the same toy cuts as `StillChain::inside` (a landscape and a portrait Cover one) exported with `scaler: <name>`, the plan of each asked that its layer takes the route under that name
  (`RenderPlan::still_route_scaler`) and each frame of the export and the still held to the model with no tolerance. The compositor scales nothing for it (FFmpeg makes the copy, the
  name in its text); what is measured is the graph's chain against the standalone decode and the `perspective` table. Probes are per kind and name, on demand (`prepare_stills`), after
  the kind's base chains; a layer is drawn with its kind's base chains **and** its route measured. The fit pictures of a placement are not measured under a name and stay FFmpeg's
  (`StillMotion`, naming the scaler): a picture that does not cover the frame (a Contain frame moved over its bars), a move that hangs off the frame's edge. Parity:
  `parity.rs`' `a_moving_still_in_a_delivery_that_names_its_scaler_is_drawn_as_ffmpegs_still_draws_it` (the shapes of the move, PNG / JPEG / odd JPEG / portrait, the four names;
  the fit pictures refused with FFmpeg rendering them): 160 frames exact on 6.1.1 (planes, PNG and display, 0 samples differ) and 80 on 9.0.2, where the JPEG cases are
  refused for their full range as for any JPEG move; the checker's cases (`checker_scalers_moving_stills_and_fit_copies`) draw 144 frames exact on 6.1.1 and 48 on 9.0.2. The ledger
  does not move (no archetype is a Ken Burns slideshow under a named scaler): 46,516 / 41,688 frames.
  **Every text that is drawn is a text a toy cut ran**: the geometry's stages with a `crop` of their own besides the guard (a Contain frame behind the clip's static crop, the Cover
  overhang cut after a first scale) are `FitShape::Cropped`, which no toy takes, so it is no chain (`StillChain::measurable` is false, `StillKinds::chain` is empty, the plan
  refuses the layer and `prepare` measures and makes nothing for it): FFmpeg draws it. A kind's two **base** chains (the move inside the frame and the engine's fit, default
  scaler: a plain slideshow, `jpeg()` / `rgb()`) are measured together first, whatever a plan needs, and every other chain on demand (`ensure`, from `prepare` /
  `prepare_ahead`, which measure the kinds and chains a plan takes and nothing else, and only for a layer the plan does not refuse whatever the probe says (nor for a frame
  FFmpeg draws whole: a scaler the compositor does not implement, an animated title, a GIF, a delivery format it does not draw, a pick rate, a cut FFmpeg 9 flags full range: `Unsupported::frame_wide`, the one list; a layer is drawn only
  with its chain **and** its kind's base chains measured, and only then is its copy made); a probe
  that cannot tell is retried after the backoff). Toy cuts are noise pictures of the kind (a landscape, a portrait, a small one and one of the canvas' shape), 64x36 at 30 fps; the export's frames
  and the still are held to the model with no tolerance **wherever the layer's alpha is settled** (0 or 255, for the chroma a block covered entirely or not at all). The
  samples at the edge of a picture panned off the frame, where `perspective`'s alpha ramps, are skipped on purpose: they are `overlay`'s blend of the placement's alpha over the base, which is the placement's and
  the blend's and not the copy's chain, and which `parity.rs` / `motion_parity.rs` (`a_move_that_pans_off_the_picture_*`, the slideshow archetype) and `checker_a_placement` hold sample for sample in whole frames. A copy
  FFmpeg makes at another size than the plan says is a `Differs` (the geometry is not FFmpeg's), not a run that failed. 6.1.1: every chain of both kinds `Match`; 9.0.2: `rgb24` every chain `Match`,
  `yuvj420p` `Differs` on the placement route (3 to 5 levels, the `PlacedFormat` full-range finding) and refused by policy anyway. (Its inside chain with the bicubic scaler named is measured as a `Match` on 9.0.2
  but never drawn: a chain is drawn beside its kind's base chains, which differ there, and the plan and `prepare` both require them.)
  **Refused** (each a specific `StillMotion` detail, a held case in `parity.rs` /
  `motion_parity.rs` with FFmpeg rendering the fallback): a kind not measured (4:2:2 / 4:4:4 JPEG, RGBA / gray / palette PNG, a `yuv420p` file), a full-range
  picture where the composite's range is negotiated (9.0.2: `FullRangeCut` in every export frame of a cut that holds a JPEG, `StillMotion` in a still, and `FullRangeStill` for a plain full-range picture in one), colour, a
  grade, an effect, a mask, a translucent layer or a dip to white on the still, a keyed crop (`KeyedCrop`), **a keyed colour (the engine's `eq` is in the chain of every frame of the clip, so a frame where the numbers pass through neutral is FFmpeg's too: `RenderPlan::eq_in_chain`)**, **a blend mode** (`BlendMode`: the route has no `pose`, so the blend's "moving zoom or position" refusal names it by `takes_still_route`), a chain not measured (a crop, a staged fit or a named scaler on a build whose toy for it did not `Match`), a render size the copy was not made for, a copy of an odd side or over 8192 a side, and a move whose `perspective` has no table (`PlacementMap::corners_have_a_map`, constant time: the corners in the filter's fixed-point range and the map not degenerate; a zoom and a pan make a rectangle, whose tables then cannot fail) or a frame whose per-pixel placement table would be over the 128 MiB storage buffer every device has (`cw * ch * 4` bytes, `MAX_TABLE_BYTES`: `from_corners` keeps `cw + ch` axis entries while every rounding is the same along a column, but falls back to the per-pixel table for about one move in three thousand, which the plan cannot tell apart in constant time, so it bounds the frame instead, as the draw's limit does; a device's smaller buffer is the draw's) (all in the plan since they are the plan's to know: `MAX_COPY_SIDE`; the draw-time guards stay). A mask, a blur, a
  sharpen, a vignette or a rotation make the move the general chain's (the route is not taken at all).
  Held to FFmpeg (0 differing samples): `parity.rs` `a_moving_still_is_drawn_as_ffmpegs_still_draws_it` (JPEG 3:2, portrait, 4:3, odd-sized, PNG 16:9),
  `the_shapes_of_a_ken_burns_move_*` (held zoom, three keys eased, cropped first, Contain, a vertical frame, renders at 960 and 540 wide as planes against
  `timeline_frame_yuv`), `a_moving_still_inside_a_transition_*` (fades, crossfade, dip, slide, push, both orders of JPEG and PNG),
  `a_move_that_pans_off_the_picture_is_placed_from_the_fit_ffmpeg_makes`; `motion_parity.rs` the same in export frames at 24 / 25 / 30 / 60 fps, late, speed 2,
  reversed, over and under video, and the slideshow archetype (both routes, 144 frames).
  **The copies are kept apart from the frames** (`copies.rs`: one process-wide LRU of 96 MiB, single flight, failures remembered for 30 s), so a copy is made
  once however many ask (the feeder, the primer, `prepare`, the frame) and nothing else is pushed out for it. An export frame's copy is the clip's own and is
  made ahead: `CursorFeed::open` makes it on the feeder's thread (a host that has the plan asks `CursorFeed::open_planned`, which makes it only for a plan the compositor draws, as `prepare` does, and answers `Opened::Skipped` for one it refuses) (a moving still has **no cursor**: the pick of a still image is decided without the
  file's frames, `Pick::select`, so the file is decoded once, for the copy) and `prepare` / `prepare_ahead` make it for the plans they are given. A still
  plan's copy is made for the instant (the FFmpeg still sizes it by the zoom it shows then, so it is not the clip's), which a drag changes at every position:
  `Hint::Exact` waits for it; every other hint is not made to wait, the copy is started on a thread of its own (one at a time) and the caller is told `Busy`
  (the caller renders that frame through FFmpeg, and the next ask for it finds the copy). The source is bounded (1 to 8192 a side) before any copy path.
  **A photograph the probe saw turned, or a file replaced since** (an EXIF orientation: the stored size, a turned decode; a picture of another size): the fit of
  a Cover frame (one stage; the commonest) is the **engine's own text**, `[crop=w=iw*..:h=ih*..:x=iw*..:y=ih*..,]scale=W:H:force_original_aspect_ratio=increase,crop=W:H`,
  which names no size of the picture's, so such a file is fitted as the graph fits it, to the sample (`a_photograph_the_probe_saw_turned_is_fitted_as_the_graph_fits_it`,
  `checker_t6_*file_replaced*`, 6.1.1 and 9.0.2). Any other fit (a held zoom's second stage) is the geometry's stages written out at the probed sizes, behind a guard
  `crop=w='if(eq(iw,W)*eq(ih,H),W,0)':h=H:x=0:y=0:exact=1` that FFmpeg refuses on a picture of another size, smaller **or larger** (`Unsupported`, "probed as");
  `exact=1` because a 4:2:0 picture's `crop` is otherwise cut to the chroma grid (1501x1003 to 1500x1002) and the graph's own `scale` keeps the whole odd picture.
  The moving still's own route needs no such text: its copy is whatever the graph's filters make of the decoded picture.
  **The fit-made family** (`RenderPlan::fits_by_ffmpeg`: an `yuvj420p` / `rgb24` still with a moving zoom or position that is not the route, nothing between its fit and the
  placement): the compositor does not resize or decode that picture, so the refusals about its own scaler and decode do not apply: `Shrink`, `FullRangeResized`, `FormatResize` (a resized PNG),
  `PlacedFormat` for other layouts, and the chroma-grid `Geometry` check of `layer_geometry` (a crop window between two 4:2:0 chroma samples, lane B's odd-last-row comparison included).
  **Still applying to it**: `ScalerApproximate`, the odd-pad `Geometry` (`odd_pad`), `MixedMatricesWith`, the full-range policy (`FullRangeNegotiated`, `PlacedFormat` full-range, `FullRangeCut`),
  the blend, the keyed colour (`eq_in_chain`, per clip), and the chain's own gate (the shape of its text measured for the kind, above).
  **The planner's `Ends`** (which clip's chain is on FFmpeg 9's alpha plane, so decides the cut's matrix) mirrors the engine's `placed = placement_animated() && still_motion.is_none()`:
  a moving still on the export's own route is a plain chain (`perspective` draws it into the frame and `format=<delivery>` ends it) and never decides; one placed by the general chain does.
  **Colour matrix (9.0.2)**: a moving still goes through the same `RgbInStack` / `MixedMatrices` judgement as every layer (`composite_matrix` over the layers on screen,
  and the planner's cut-level flag over every clip's stream, whichever route its picture takes), so an RGB picture beside BT.709 / BT.2020 footage is refused as a static
  PNG is, once that refusal covers the cut (lane B); a PNG after such footage in a cut, not on screen with it, is the pre-existing gap `checker_t6_*static_png*` controls.
  **Ledger (T6, the photo slideshow of three shapes, Ken Burns, crossfades)**: still plans 0% to 100% drawn on 6.1.1 (26.7% on 9.0.2: the PNG-only frames), and
  export frames at the delivery's frame 100% on 6.1.1 (the ledger's 540 px motion rows stay refused: an export frame's copy is made for the delivery's frame);
  T20's still plans 22.5% to 46.9% (6.1.1). **The ledger's `still_motion` gain in its motion rows (T6:m, T20:m: 1,548 frames on 6.1.1, the top of (b)) is no gap.** The ledger
  plans an export frame of the delivery (1920x1080) and judges it at its own width, 540x302 (`still_size`); the export's copy (`q`, the window, the fit order) and a placed still's
  fit picture are made for the delivery's frame, so `StillLayer::geometry` / `fit_copy` refuse the frame at any other size (`still_motion/at a render of 540x302 ...` in (c),
  the copy's cause and the fit picture's, 68 frames both). Judged at the delivery's frame ((g)) T6 is 1350 of 1350 drawn and T20 183 -> 381 of 825: the 198 frames the render size
  alone refused, and the other 444 stay refused for reasons that do not move (alpha pictures, rotations). The copy could be rebuilt for a render size, but "the export at that
  width" is not the delivery's plan drawn smaller: it is the plan of an export at that resolution (`ExportOptions::resolution`), judged on its own frame. Planning every motion row
  that way (a spike, not kept) draws T6 1350 of 1350 and T20 315, and loses T3 2250 -> 243, T10 227 -> 77 and T21 357 -> 239 (motion 24,657 -> 23,864 of 35,793): 540x302 is not 16:9, so a
  960x540 picture on a PiP hangs its cover 2 px over it (`cover_overflows`) and takes the `Uncropped` fit order (`FoldedFit`) that the 1080p export does not. No plan stands for an export
  at another size, so the refusal is right for the ledger's width and for any caller, who plans the export they want. Held without FFmpeg by kerf-core's `tests/still_route_size.rs`
  (drawn at the delivery's frame and at no other, `for_render` leaves an export frame's copy alone, an export at another resolution has its own, a still plan is worked out for any size,
  the fit order is judged against the frame) and with it by `motion_parity`'s `checker_t6_export_frames_at_another_size_are_refused` and `checker_a_stills`'
  `checker_a_t6_a_copy_is_drawn_only_at_the_size_it_was_made_for`. **On 9.0.2 `still_motion` is never the only reason** (gain 0; (d) credits +885 only after `full_range_cut` and the rest are
  lifted): a JPEG is also `full_range_cut` (export frames), `full_range_still` / `full_range_negotiated` (stills) and `placed_format`, and (c) names its causes (the full-range gate, the
  `yuvj420p` placement chain, the two render-size ones). **9.0.2 and a JPEG, measured**: the moving still's own chain and a JPEG that covers the frame, faded in or not, are
  drawn to 0 differing samples when the refusals are bypassed (the copy stays full range and the fade darkens toward 16 on the raw values, which is FFmpeg 9's), so
  what keeps a JPEG cut off the GPU there is the cut-level conversion of everything limited-range in it (the black base, a PNG, a gap, the bars of a pan off the
  picture: `FullRangeCut`, `FullRangeNegotiated`), not the JPEG; a cut of JPEGs alone is not claimed (no case holds the base, the bars and a mixed cut), so it stays
  refused.
* **Live behaviours** (`GpuCaps::behaviors`; `core-render-plan.md`): breathe, drift and shake are a formula of the clip's clock added to its pose, the plan's
  `transform` is the exact pose they give at the frame (`Clip::transform_at`, the arithmetic of the graph's expressions, `sin` / `cos` included), and the
  placement draws that pose: held exactly in export frames and stills (`behavior/*`: each alone, all three, over a keyed zoom). A sway or a spin turns the
  picture (a keyed rotation, which no pass draws to the sample: the rotated edge is a stair in FFmpeg and a float sample here; the plan names it `Unsupported::Rotation` too, "Rotation" below), so `Behavior` stays for them
  whatever the caps.
* The compositor draws that order (`LayerLook::placed`: `animated.places`, a geometry that ends at the fit-size picture). Stage 0 is the fit
  scale with the `eq` tables in its vertical pass; the effects, the mask plane and the fades (`fs_fade`: Y, U, V and the alpha) run on the
  fit-size planes; the placement then reads the four planes. A zoom that holds still keeps the old order (a `scale` ahead of the rest).
* Refused whatever the caps (`ZoomBehind`): a rotation and an opacity below 1 (the placement would act on the rotated alpha or the RGB round
  trip, neither of which a case holds to FFmpeg's), and a dip to white (`fade c=white` converts the picture to RGB and back
  around the fade; no case holds the placement behind it).

**The placement** (`kerf_core::perspective`, `src/placement.rs`, `shaders/placement.wgsl`, `Compositor::place_planes`; `tests/placement.rs`).
FFmpeg ends the chain of a clip whose zoom or position moves in `format=yuva420p, pad, perspective=interpolation=linear[:eval=frame], crop`
(`engine::cli::placement_chain`, the export, the stream and the still alike), and every sample of that is integer or `f64` arithmetic that is
reproduced exactly:

* **`pad`** adds a margin of `2*ceil(max(0, cw-iw)/4)+2` samples on each side (so the canvas fits and two samples more), in `black@0`: Y 16, U = V = 128, alpha
  0. Its output width and the picture it copies are rounded down to even (`ff_draw_round_to_sub`), the right border is drawn from the even width
  (**an odd width loses its last column, on every build**), and `crop` takes the canvas from `2*floor((W-cw)/4)`. **An odd height is refused**
  (`Geometry`): `pad` draws its bottom bar over the last row only when it pads in place, which depends on how the graph allocated the frame, and
  the builds differ (`color=size=6x5` padded in place: 4 rows on 4.4.2, 5 on 9.0.2; copied, 3 of 3 on both).
* **`perspective`** is `pv[x + y*w] = lrint(256 * (x0*x + x1*y + x2) / (x6*x + x7*y + x8))` for every pixel, in `f64`, from nine numbers built from the
  four corners the expressions evaluate to (`PlacementFrame::corners`: `(edge + 0.5 - crop - centre) / s + dim/2 - 0.5` in the order the text writes it,
  `centre = canvas/2 + pos*canvas`, `pos` the plan's pose plus a slide's or push's travel: `RenderPlan::pose`). The corners are an axis-aligned rectangle,
  so `x6` and `x3` are exactly zero and `v` depends on the row alone; `x1` and `x7` are zero or **rounding noise** (`(x0 - x1) - x0 + x1` is not 0 in
  `f64`), under which `u` is the column's unless the noise carries it across a rounding boundary. `PlacementMap::new` takes one table per axis
  (`canvas.0 + canvas.1` entries) when the value at the window's first and last row, widened by 1e-6 + 1e-12 of itself, rounds to one integer, and
  the filter's whole table, pixel by pixel (`PlacementMap::exact`), otherwise; unit tests hold the first to the second over 4000 random poses and the
  structured ones whose positions can sit on a tie. A pose whose positions pass 2^21 samples (`lrint` leaves 32 bits) is refused in the plan.
* **`resample_linear`** reads the entry of a sample's top-left luma pixel (chroma `>> 1`, no half-sample centring), splits it into a whole part and 8 bits of
  fraction, and mixes four neighbours with integer weights, `(sub_v_inv * (sub_u_inv * a + sub_u * b) + sub_v * (...) + 2^15) >> 16`. The filter's two
  edge branches each ignore the fraction of the axis they clamp; clamping each tap to the frame gives the same number (a clamped pair is one sample).
  The alpha plane is resampled like luma, so the fringe where the picture meets the pad is a ramp of alpha *and* of Y towards 16.
* **`PlacedFormat`** (kind `placed_format`): the chain converts the picture to `yuva420p` itself, at its head, where the compositor's decode converted it to 8-bit
  limited-range 4:2:0 ahead of the graph. The two are the same arithmetic where the picture is already 4:2:0 (8 and 10 bit, any matrix tag, gray: held exactly on
  both builds), and are not elsewhere: a **4:4:4 picture's chroma is 24 levels off on 4.4.2 and a 4:2:2 one's 16**, exact on 6.1.1 and 9.0.2. The cause is the `scale`
  filter's own default: **4.4.2's `flags` default is `bilinear`** (`vf_scale.c`; 6.1.1's is empty, which is swscale's bicubic), and the graph's conversion is that
  filter's where the decode's is the CLI's simple graph, bicubic (the standalone conversion and the standalone placement are both exact on 4.4.2). So **the
  4:4:4 / 4:2:2 refusal is lifted by a measurement, not a version** (`kerf_gpu::conversion`, `GpuCaps::placed_chroma`, set by `Compositor::caps()` once the probe
  has said `Match`): one tiny `testsrc2` picture of each format through the decode's conversion and through the graph's (`scale ... , format=yuva420p`, the
  chain's own head), compared plane by plane; `Differs` (4.4.2: 774 samples, max 20) or `Unknown` (FFmpeg would not run: retried after a backoff, never cached)
  leaves them refused. `prepare(plan)` runs it ahead of a plan with such a layer. Only 8-bit planar `yuv444p` and `yuv422p` are lifted: **RGB** and the rest of the
  family (4:4:0, 4:1:1, 12 bit, packed) were not measured and stay refused. A **full-range** picture (`yuvj420p`, 10-bit flagged `pc`, or one whose range is not
  recorded) is **3 to 5 levels off in a still on 9.0.2** (4:4:4 and 4:2:2 flagged `pc`: 5 to 8; exact on 4.4.2) and stays refused where the composite's range is negotiated (`policy != FixedBt601`), **the lifted 4:4:4 and 4:2:2 included**. An asset probed before the range flag was recorded (`color_range` `None`) may be full range without saying so, which the plan cannot know and the file can: where the range is negotiated, `placement_of` reads the file's answer **from the cache alone** (`kerf_core::source_is_full_range_known`, `ProbeCache::peek`: it spawns nothing and does not wait for a probe under way) and refuses a `pc` one at the render (`GpuError::Unsupported`), and a file with no answer yet (not asked ahead, being asked, the probe failed or said nothing usable: `None`, never a guess); a layer whose asset recorded its range costs nothing. **The session-level decisions see that refusal before the frame does** (`GpuCaps::file_range`, which `Compositor::session_caps()` and `session_caps_for` set: the caps of a span, which a play session's `begin_play` / `ahead` rescans / `ready_play` and the primer take from the compositor and the render bar from `session_caps_for(grade::measured(), scaler::measured(), convert::measured())`; `caps()`, `caps_for`, `Compositor::CAPS` and so the compositor's own entry points do not): with it `RenderPlan::reasons` reads the same cache for a placed layer with no recorded range under a negotiating policy and refuses (`PlacedFormat`, "full-range ... its asset did not record it" or "of a range its asset did not record and its file has not said") a file that is full range or that has no answer, so a play session's `cut.ahead`, `resume`, the primer and the render bar hand such a span to the stream up front instead of a session starting on the GPU and handing over a frame into it. The entry points that ask do so first: `prepare_ahead` (session start, resume, primer), `prepare`, and `prepare_ranges(plan)`, the range part alone, which the render bar calls on its plans through `BarRequest::ask` (once for each set of layers on screen, by file and by whether it is placed, not once a frame: a clip split in two with a moving zoom on its second half only is one file asked about from there; `get_render_bar` passes `kerf_gpu::prepare_ranges`). The compositor's own entry points judge by `caps()` (`file_range` off), because the decode is what asks the file for the frame it draws and `placement_of`'s check follows it with its own reasons (the parity harness gates its frames by `plan.reasons(&compositor.caps(), ..)` before any `prepare`, and a cold file must still be drawn); `file_range` is `false` in `GpuCaps::A0`, so every other host and test judges a plan that is a pure function of its inputs (`render_plan.rs`: `a_placed_layer_with_no_recorded_range_is_judged_by_the_files_answer_only_where_the_host_asks_it`). A running session's rescan (`PlaySession::ahead`, every 0.5 s over 5 s) asks the files of the part of the horizon that has come into view with `kerf_gpu::prepare_ranges_ahead(&cut, from, horizon)` (each set of layers once; the session's own thread, off the play lock; a stat for a file already asked, one `ffprobe` for a new one) before it judges the window with `session_caps()`, so a placed clip that enters the horizon later is not refused for a range nobody has asked yet. **The render never probes; `prepare(plan)` does, ahead of it** (and `prepare_ahead(&cut, from, horizon)` for every frame of a play's horizon, at a session's start and before the horizon scan: `begin_play`, `ready_play`, `make_primed`, which also call `prepare_timeline(&timeline)` for the probes of the whole cut and `prepare_placed_files(&cut, &timeline)` for the range of every placed clip's file (four files at a time, none started after 20 s); `FrameSource::check` asks it too), as it does the placement and conversion probes. The one-shot `Compositor::render_plan` and `composite_yuv` (tests, diagnostics) prepare for themselves; `composite`, the texture renders and the shared / counted composites do not (`tests/checker_a_range.rs` holds the one-shot's answers, `tests/range_warmup.rs` that `composite` refuses a cold file by name without probing it, and draws or refuses it as the file says after `prepare`). **The decode makes the answer exist** (`frame_source::warm_range`, for a placed layer whose asset did not record its range: in `FrameSource::frame`, `check_cursor` and every frame of `MotionFrames::frames`; a `stat` and a cache hit, one `ffprobe` for a file first seen **or replaced since**, the cache being keyed by size and modified time), which is how `render_plan_with` and the play path find it. A file rewritten under a running session is therefore re-asked by its next frame instead of refused until the next session: `tests/motion_parity.rs`'s `checker_a_a_placed_clip_in_an_export_in_parts` re-runs the test binary, whose `media()` rewrites its fixtures in place under the sibling tests (the cause of a `checker_a_a_grade_ahead_of_a_placement_is_the_exports` failure on 9.0.2 that only a full, parallel run showed).

**The probes of a placed layer** share `measure.rs`: a `Verdict` (`Match`, `Differs(where)`, `Unknown`), one `Probe` each holding the answer (kept for good), the retry time and the single flight, `cached(.., timing, probe)` (an `Unknown` is asked again after 30 s and never believed meanwhile), `checked` (the self-test's) and `Budget::ffmpeg(args, input)` (a bounded child through `kerf_core::run_piped_until`: killed at its limit, **no thread joined that a child's descendant can keep alive**).
  * **One probe at a time.** The render bar, a frame's `prepare`, a play session's primer and the self-test ask for the same answer from different threads, and only the first runs the probe: the others wait on a condition variable for no longer than the probe is bounded to (its deadline and a 0.5 s grace) and take its answer; one that is not answered by then gets `Unknown`, "not yet", and refuses. No lock is held while a child runs (the state is locked to read it and to store the answer), and a probe that panics gives its place up and backs off (`Lead`'s drop) instead of leaving every waiter to time out.
  * **One deadline for the whole probe** (`Timing { backoff: 30 s, overall, per_run: 20 s }`; `overall` 45 s for the placement probe, 30 s for the conversion probe), shared by all its children through a `Budget`: each child gets the shorter of its own limit and what is left, and a probe that has run out is `Unknown` (it used to be ten sequential runs of 20 s, and four).
  * **Argv.** A child's argv is the quiet flags, the probe's args and the thread caps of a frame read (`limit_frame_args`, as the grade's and the text's probes take them). The spill is asked of a graph that needs it only (it asks this FFmpeg which spelling it takes, a run of `-h full` of its own) and the child's limit is worked out after it, so the probe's deadline holds. A `-filter_complex` graph over the engine's argv rule (`kerf_core::GRAPH_ARG_MAX`, 8192) goes through a script file (`spill_long_graph`, the engine's own spill), and the placement probe stacks a shape's poses in as many runs as keep each graph under that rule (a run is a stack of two at least; one pose alone is placed without `split` and `hstack`, which take two) (the poses were one 31 KB command line per shape, a kilobyte short of Windows' 32,767); a run's `perspective` is therefore still in its argv, which is what a wrapper that perturbs or cuts off the probe's graph reads (`tests/checker_a_gates.rs`).
  * **The self-test's hold.** `checked` does not take an earlier failure's backoff (a `prepare` whose probe timed out a moment before) for its verdict: it waits the backoff out and runs the probe, and one that cannot tell is run once more after the backoff, so a transient failure no longer holds the placement back for the session. A probe that cannot tell twice sets `held_back`: `known()` and `cached()` answer `Unknown` without probing until a later self-test that can tell has run (`checker_a_gates`: a probe that succeeds later must not open a placed frame the check never drew). `Check::retry()` (the app) re-runs a failed check only, so after a self-test that passed with the placement held back it stays FFmpeg's until the app restarts or the adapter changes: two failures a backoff apart means FFmpeg could not run a tiny graph twice, and everything else on that machine is failing too. The grade's own hold (`grade::kernels_for_selftest`) is unchanged.
  Unit-tested without FFmpeg (`measure::tests`: the backoff, six callers one run, a bounded waiter, a panicking probe, the self-test's retry and its wait for a running probe, the spill; `conversion::an_unknown_is_asked_again_after_thirty_seconds_and_a_match_or_a_difference_is_kept`).
* `placement_probe` (`GpuCaps::placement` and `behaviors`): the **export's own chain text** (`kerf_core::placement_probe_chain`, `placement_chain` joined, both forms: `eval=init`, the still's, and `eval=frame`, the export's) on tiny noise pictures with an alpha plane, every plane held to `placement::reference` with no tolerance: seven shapes (the canvas's size, smaller, larger, an odd width, a thin picture), poses on a rounding tie (the canvas a power of two, so every number is dyadic), non-dyadic ones and ones off the frame, stacked with `hstack` in runs of about ten (each graph within `GRAPH_ARG_MAX`); then a keyed zoom and position with a breath, a drift and a shake over `perspective`'s frame count (`kerf_core::placement_probe_motion`, the clip's `driven_expr` against `Clip::transform_at`: the count's origin, `eval=frame` re-evaluating, the machine's `sin` / `cos` against the app's). About twenty-five short `ffmpeg` runs, each bounded by 20 s and all of them by 45 s; any difference is `Differs` (it names the first case), a run that fails, writes the wrong amount or runs out of the deadline is `Unknown`. 6.1.1 and 9.0.2: `Match`.
* `conversion` (`placed_chroma`): above; the same `measure` slot and backoff, run on the play path through `prepare_ahead` and, for any cut with a moving zoom or position (the picture formats are the files', which a timeline does not know), by `prepare_timeline`.
  (The same default is the likeliest cause of the compositor's scaler being up to 29 levels off swscale on 4.4.2: it follows bicubic.)
* **Which frame is drawn** (`FpsPick::pinned`, `frame_pick.rs`): a placed clip's chain pins its `fps` (`fps=fps=R:start_time=S`, `keyed_crop_fps`), and with
  `start_time` the filter rounds each input time **relative to the start**: `slot = round_us(S, fps) + round((pts - round_us(S, tb)) * rate)`, both offsets rounded
  half away from zero from the microseconds `S * 1e6` truncates to (`vf_fps.c` `config_props`, `read_frame`), shows its first frame from the offset on whatever that
  frame's own slot (`next_pts` starts there), and ends the stream at the status's own rounding (`update_eof_pts`, no offsets) or at the last frame's offset slot,
  whichever is later (the status is acknowledged only with one frame left buffered, so the frame before the last is put out until the last one's slot has come). The
  plain pick (`round(pts * rate)`) differs on every tie: 30 fps footage at 24, 25, 48, 50 or 12 fps, or a start off the stream's ticks (at 24 fps, `start_time=0.37`
  against `fps=24` after `setpts=PTS-STARTPTS+0.37/TB`: output ticks 10, 14, 18, 22, 26, 30). The unit test runs `vf_fps.c`'s state machine step by step over 4 time
  bases, 9 rates, 6 starts and 4 speeds, and the rendered one (`picked.rs`, `the_fps_pick_is_the_frame_the_export_renders_for_a_clip_whose_fps_is_pinned_to_its_start`:
  245 clips, 5 sources x 7 rates x 7 starts / speeds / seeks, through the real export graph) holds it to 6.1.1 and 9.0.2.
* The compositor draws the four canvas-sized planes (Y, U, V, alpha) and hands them to `compose_exact` as a layer at the origin with a per-sample alpha,
  `overlay`'s own integer blend. The fades (a clip's own, a dip to black, a dissolve's ramp on the alpha plane or on the opaque one `format=yuva420p` gives
  a picture) run on the fit-size planes first, as in the chain; the mask is cut at the fit size and placed with the picture.
* **Held to FFmpeg on both builds**: `tests/placement.rs` (`pad,perspective,crop` over noise with an alpha plane, 144 shapes and poses, every sample), the
  shader against the model, `motion_parity.rs` (`zoom/*`, `position/*`, `shape/*`, `zoom+*`, the dissolves, dips, slides and pushes into and out of a
  placed clip, 0 differing samples in every plane of every frame), and `parity.rs` (`a_moving_zoom_is_placed_as_the_still_places_it`: the compositor's planes
  taken to RGB by FFmpeg itself against the still's PNG, `check_exact`, with a `control/plain` case that holds the harness to an unplaced layer). Where the
  compositor's **scaler** is also in play ahead of the placement (a Cover fit that resizes, a held zoom that is a `scale`) the case is exact too (the scaler is swscale's to the sample; it was held to 1 level while the compositor drew the C kernel), the placement adds nothing to it. On this CPU the system 4.4.2's x86 swscale is further off the port than that at sharp edges
  (22 levels on the test pattern at a 2:1 shrink, with or without a placement; the pinned 9.0.2 is within the level): a layer the compositor resizes is
  not exact against that build, placed or not.

**Masks.**

* The alpha plane is `geq`'s: `kerf_core::mask_alpha_plane` evaluates the expression in `f64` and truncates as `geq` stores it (`255 * 0.999` is 254),
  equal to the real filter on 21 million samples on both FFmpegs (`rendered.rs`). It is made once per `(mask, size)` (`MaskCache`: 8 planes or 96 MiB,
  the GPU copy counted), where `geq` costs 93 ms of CPU a 960x540 frame, every frame.
* The blend takes the alpha **per sample** (`fs_compose_exact`): luma with its own, each chroma sample with `overlay`'s average over its 2x2 block (`>> 2`;
  the last chroma column and row average what they have, the last column not reading its second luma column: `blend_plane`, the same on both FFmpegs).
  A dissolve's ramp acts on that plane sample by sample. On a letterbox the mask is the padded frame's (`pad` runs ahead of `geq`): the canvas's size.
* **FFmpeg 6.1's `geq` reads a plane's last column and row one in** (its bilinear read clamps to `w - 2` / `h - 2`): the masked picture's right-most
  column and bottom row, on all three planes, are their neighbours'; 9.0 copies them. `kerf_core::geq_edge()` measures which (the filter over a 16x16
  picture of distinct samples, once per process, outside any lock; `Unknown`, not a guess, retried after a backoff). The blend then reads the layer one in
  (`ComposeParams::edge`, a letterbox included), **the planes of a late zoom, and of a turned layer, are shifted before the scale / the turn** (`edge_planes`, `fs_edge`: `geq` runs on the
  picture ahead of both, so the row and column it replaces land on whatever edge of the turned box the turn gives them (a quarter turn puts the bottom row on the left column, a half turn the
  last column on the first); shifting the turned box's own last column and row, as the unturned layer's are, was exact only at -90 and 270 where the shifted edges fall off the canvas, and
  Y 65 / UV 128 off at 90 and 180 on 6.1.1, a plain layer's chroma included, which the plan let through), and a mask or a keyed opacity is `Unsupported` while the build is `Unknown`. `kerf_gpu::prepare(&plan)` (or `prepare_timeline`, for every clip of a cut) measures it ahead of the render lock; the render reads `kerf_core::geq_edge_measured()` and never runs the probe, and `Compositor::caps()`, like `caps_for` / `caps_with` / `caps_pinned` / `session_caps_for` (the render bar, which read `geq_edge_measured()`), has no `mask` or `keyed_opacity` while it is `Unknown` (the plan refuses up front); `Compositor::with_geq_edge` pins it.
* **A mask on a layer below full opacity is drawn** (a constant opacity; a still's sampled keyed one is that too). The graph's order is `format=yuva420p`, the mask's `geq` (alpha only), then `colorchannelmixer=aa=<opacity>` inside the
  RGB round trip (`mask_filter`, `opacity_filter`), so the mask's plane is the alpha that goes in: `colorchannelmixer` writes `av_clip_uint8(lrint(level * opacity))` for **each level** (its table for the alpha channel, the other three
  terms 0; `lrint` goes to even on a tie, 0.5 makes every odd level one), a dissolve's `fade alpha=1` rounds on that, and the conversion back to `yuva420p` scales it by 256/255 (`kerf_core::layer_geometry::ffmpeg_alpha_of(level, opacity, ramps)`,
  the one rule: `ffmpeg_alpha_ramps` is its level 255). The colour planes never see the alpha (swscale's way in copies it, the way back reads RGB), so they are the unmasked layer's round trip. The compositor maps the mask's plane through
  that table (`LayerFades::mask_alpha`, `Compositor::alpha_levels`: `planeops.wgsl`'s `fs_lut`, 256 entries) and `overlay` blends luma per sample and chroma over each block's four alphas as for any mask. **On 6.1 the `geq`'s copy of the last column
  and row comes before the round trip, so the planes are shifted before it** (`shifted_here` includes a translucent layer; shifting after, in the blend, is a level of luma and chroma on a few samples: found by mutating it). Whether the table
  is FFmpeg's is **measured with the round trip** (`roundtrip_probe`: five masked cases hand the chain a picture whose alpha plane holds every level in four orders, at 50 % (every odd level a tie), 25 %, 85 %, 4 % and 99.9 %, and hold the
  alpha it ends with to `ffmpeg_alpha_of` level by level; the unit test `every_level_of_a_masks_alpha_ends_as_the_model_says_at_every_tie` runs 3,688 opacities (every tie of the 255 grid, ±1 ulp, the ties of `level * opacity` for 19 divisors) over
  all 256 levels, 944,128 samples, on both builds; `a_map_of_the_levels_that_is_not_ffmpegs_is_differs_in_a_masked_case` shows the probe tells truncation, round-half-up and an unscaled map from FFmpeg's): there is one verdict (`Translucency`), so a
  build whose masked alpha differs is `Differs` for every translucent layer, and `translucent_approximate` refuses them all. Cases: `parity.rs` `masks_on_a_layer_below_full_opacity_are_drawn_as_the_still_draws_them` (shapes, feathers, inverted, resized,
  odd offsets, off the frame, 4:3 contain, 9:16 Cover crop, noise, BT.709, two stacked, 10-bit / 4:4:4 / full-range / gray / RGB sources, a keyed opacity a still samples, a dissolve), `checker_mo_stills_every_alpha_level_and_rounding_tie_of_a_masked_opacity`
  (45 opacities at the ties, a wide feather so a picture holds every level, resized in half of them), `motion_parity.rs` `a_mask_on_a_layer_below_full_opacity_is_drawn_as_the_export_draws_it`; all 0 differing samples on 6.1.1 and 9.0.2.
  **The ledger**: T12's masked ghost (360 + 362 frames; `mask_with/on a layer below full opacity` was the only cause in 722 of its 733 frames) **is drawn, still and motion: 6.1.1 49,400 -> 50,122 (70.9% -> 71.9%), 9.0.2 45,830 -> 46,552 (65.8% -> 66.8%)** of
  69,666; the variant leaves (b) and (c); T11's other 11 frames of it (a callout's pop-in, a keyed opacity under a mask) pass this variant now and are still refused for their other causes (the keyed mask, the keyed crop), so the total does not show the keyed part.
* **A chroma key on a layer below full opacity is drawn the same way** (a constant opacity; a still's sampled keyed one is that too). The chain is `format=yuva420p`, `chromakey` (the alpha plane), then `colorchannelmixer=aa=<opacity>` inside the round
  trip, and the round trip cannot tell whose plane it maps: `LayerFades::mask_alpha` is also made for a keyed layer (`has_chroma_key`), and the key's plane goes through `Compositor::alpha_levels` before the blend, so a dissolve's ramp, the 256/255 scale and the ties
  of `lrint` are the mask's (`ffmpeg_alpha_of`). Nothing else changes: there is no `geq`, so no `mask_edge` and no shifted planes, and the colour planes are the unkeyed translucent layer's round trip. The probe holds the chain to it
  (`roundtrip_probe` `KEYED_CASES`: four cases that run the engine's own `chromakey` text, `kerf_core::chroma_key_filter`, over a picture whose chroma is a ramp, split the keyed picture so the key alone and the key through `colorchannelmixer` come from one graph (the key's
  alpha depends on the slice threads), and hold the second's alpha to `ffmpeg_alpha_of` of the first's level by level, 148 to 252 distinct levels in three of them, and the colour planes to the model; `a_map_of_the_levels_that_is_not_ffmpegs_is_differs_in_a_keyed_case`
  shows a truncating, a half-up and an unscaled map are `Differs` there): one verdict (`Translucency`) still, so a build that maps a key's levels another way refuses every translucent layer. Cases: `parity.rs` `chroma_keys_on_a_layer_below_full_opacity_are_drawn_as_the_still_draws_them`
  (keys of six colours / similarities / blends, the 255 grid's ties and the doubles either side, a ramp and noise, resized, moved, cropped, a Contain fit that fills the frame and one with bars, a 9:16 Cover crop, BT.709, a sampled keyed opacity, a dissolve),
  `motion_parity.rs` `a_chroma_key_on_a_layer_below_full_opacity_is_drawn_as_the_export_draws_it`; the checker's `checker_ko_stills_*` (opacities at the 255 grid's ties and the doubles either side, keys of five colours and five similarity/blend pairs over four pictures, dissolves, sizes and fits)
  and `checker_ko_motion_*` (every `j / 255` held keyed, ties held constant, widths, 12 frame rates, every transition, things beside a key and an opacity, curves and starts), and the independent checker's
  `checker_ko2_*` (`checker_ko2_plane`: the `geq` and the round trip over a real key's alpha level by level; `checker_ko2_probe`: shims that edit the keyed graphs and must make the probe `Differs`; stills and export families, drawn-anyway sweeps of the kept refusals):
  local runs against both FFmpegs, with their counts and costs in `build-and-ci.md`. 0 differing samples on 6.1.1 and 9.0.2 in every one.
  **The ledger** (new archetype T25, a green-screen ghost with the numbers a user types, T13's presenter at 0.9 and a 0.37 pip, not chosen to keep a layer's sides even: a key under 0.7 / 0.5 / 0.85 / 0.6 opacity, a pip, a keyed opacity that rises and falls, a dissolve between ghosts, a colour correction ahead of the key, a tilt):
  6.1.1 50,392 -> 50,510 of 71,586 (70.4% -> 70.6%), 9.0.2 46,822 -> 46,940 (65.4% -> 65.6%). The gain is T25's export frames, 119 -> 237 of 960 (the keyed opacity has no round trip, so odd sizes are drawn); its stills stay 151 of 960, and so do T13's:
  **a constant opacity on a layer of odd size** (483x271 and 198x111 at the ledger's 540-px render, where the app's preview plans at 960) was the `geometry` refusal, then the single largest gain left in the ledger (1,528 frames; lifted below). T1 to T24 are unchanged
  (50,122 and 46,552 of 69,666); the variant `effects/a chroma key on a rotated layer or one below full opacity` (481 frames, no gain) is `a chroma key on a rotated layer` (483 with T25's tilt, no gain). Refused: a key on a **rotated** layer and on a **blended** layer below full opacity
  (`Effects`, from `RenderPlan::alpha_plane_refusal`; the compositor says the same), over a mask (`MaskWith`), before a moving zoom (`ZoomBehind`), and every refusal of a translucent layer (a fade to black, a turn, the matrix; an odd size only beyond what `translucent_odd` was held to: under 16 px, a blend, a grade, an effect but the key).
  **Only the rotation is shown necessary** (drawn anyway, a turned key is up to 166 levels off); the key over a mask, on a blended layer below full opacity and before a moving zoom came out exact when the independent checker drew them anyway
  (`checker_ko2_stills_refusals_beside_a_key_and_an_opacity_drawn_anyway`, `checker_ko2_motion_refusals_*`), so those three are **unproven rather than wrong** and stay refused until a parity case holds them.
  **The odd translucent layer** (this package, "The translucent round trip" below): the ledger's constant-opacity layers of odd size (483x271, 198x111) are drawn. **6.1.1 50,510 -> 52,038 of 71,586 (70.6% -> 72.7%), 9.0.2 46,940 -> 48,468 (65.6% -> 67.7%): +1,528 on each build**, the whole of the `geometry` refusal's
  gain and nothing else moved (`KERF_LEDGER_ODD_OFF=1` asks the ledger as if the odd-size probe had found no model, and reproduces the before to the frame). T25 151 -> 840 stills and 237 -> 838 export frames of 960, T13 600 -> 720 and 599 -> 717. What still refuses T25 (120 and 122 frames) is the tilted ghost: the rotation
  (`Rotation` for a translucent turn, `Effects` for a key on a turned layer), whose `geometry` variant (`a translucent layer of odd size 483x271 (turned)`, 242 frames) is the same frames, no gain.
* **A mask under a keyed opacity** (an export frame: one `geq`, no round trip) is drawn as the product `((keep * curve) * 255.0) as u8` ("Keyed opacity" below).
* Refused (`MaskWith`): a mask on a **rotated** layer, over a chroma key, and on a **blended** layer below full opacity, constant or keyed (the blend chain's probe carries the opaque plane of a constant opacity alone);
  and, whatever the caps, a turned or fading translucent layer (the round trip's own refusals; an odd-sized one is drawn since `translucent_odd`).

**Rotation: a quarter turn, or FFmpeg's.** `rotate` has two paths (`vf_rotate.c`, the same in 6.1 and 9.0): a `simple_rotate` copy when the angle is within `FLT_EPSILON` of 0, π/2, π or 3π/2, and a
general one that walks the output with `c = int_sin(angle_int + INT_PI/2)` and `s = int_sin(angle_int)`, `angle_int = (int)(angle * 65536 * 16)`, a fifth-term Taylor series in integers. The samples are whole only where `c` and `s` come
out exactly 0 and ±65536. They do for the quarters -47, -46, -43 to 40, 43 and 44, and for no other whole number of turns up to the 1000 radians `kerf_core::quarter_turn` looks at: the series and `INT_PI` are a unit off further out, growing
with the angle (41, 42 and 45 to 120 and -44, -45 and -48 to -120 quarters: the picture lands between samples; 1e7 degrees is an `int` overflow, undefined in C and done differently by the builds). Measured, not guessed: `rotate`
(the export's text, `yuva420p`, pictures of odd and even sides with a different value in every sample) against the exact quarter turn of its input, for **every quarter from -1310 to 1310, FFmpeg 6.1.1 and 9.0.2: the set above, no
other, which `quarter_turn` computes** (`checker_e_quarter_turn_is_the_set_rotate_turns_exactly_for_every_quarter`, `#[ignore]`d, `cargo test -p kerf-gpu --no-default-features --test checker_e_turn -- --ignored`) (its port of `int_sin` and the fast path, within `FLT_EPSILON` radians of a quarter, the filter's own fast-path window: 90.00001 degrees is a rotation like 91 is, and the unit tests list the set; `tests/checker_e_turn.rs` holds the sweep and the hairs to `rotate` itself). **Every other rotation is refused on every layer** (`Unsupported::Rotation`,
plain, blended, keyed opacity or not, and a rotation a key or a behaviour moves, which is a `hypot` box turned per frame): the float edge of the old pass was Y 117 / UV 169 levels off the fixed-point one at the rotated edge and no ladder is a
drawn frame. The compositor guards the same (`look_of`: `GpuError::Unsupported`) and snaps a quarter's cosine and sine to exact 0 and ±1. **A turned odd box carries the overlay's chroma tail like any odd layer** (the last chroma
block of an odd-sided layer reaches one pixel past it, `tail`): a quarter turn is a picture like another, and the tail was skipped for turned layers (a blended 639x360 gray turned 180: U 720 samples, 48 levels off; 640x359 at 90 and 270: U 2520, 13 levels).

**The display conversion** (`display_probe.rs`, `Compositor::with_display`, `composite.wgsl` `fs_convert`). The preview shows RGB, and the compositor converts its finished composite once, at the end, in the integers
of swscale's x86 `yuv420p -> rgb24` kernel (`ff_yuv_420_rgb24_ssse3`: `pmulhw` on 13-bit coefficients in 16-bit lanes, `grade::X86Kernel`), which the still's PNG and an export are read through on an x86 FFmpeg with SSSE3. Another CPU's FFmpeg runs another kernel (NEON, or the C
one with its tables): with `-cpuflags 0` the preview RGBA read **up to 2 levels off in about 45% of samples**, shown as FFmpeg's, which the pass used to do ungated. **It is gated now, by a probe, not by a version**: `display_probe::display()` (once per process; `prepare` /
`prepare_timeline` / the one-shot `render_plan` ask) runs FFmpeg's own conversion: the engine's still writes **no conversion text** (its graph ends in `yuv420p` and the output's `-pix_fmt rgb24` makes FFmpeg insert its scaler, the unscaled `yuv420p -> rgb24` in question; the matrix is the composite's tag), so the probe runs a bare `-pix_fmt rgb24` for BT.601 (an untagged composite) and, for the matrix a tag names, `scale=in_color_matrix=<m>:in_range=tv` (not text the engine writes: the blend chain's `blend.rs` writes `scale=flags=neighbor:in_color_matrix=<m>:in_range=tv` for its own RGB round trip; the bare form is the probe's and the parity harness's, which names a plan's matrix to a still with it) for BT.601, BT.709 and BT.2020; `the_display_probes_text_is_the_conversion_a_stills_png_runs` (`parity.rs`) holds both to the real still's PNG on both builds
over noise in all three planes of six even sizes (2x2 to 202x100: a width of one chroma sample, a vector's width and a tail past it) and holds every sample to `grade::yuv420_to_rgb`, with no tolerance; `-cpuflags 0` reads `Differs` (`a_build_with_no_simd_kernels_is_differs`).
**Anything but `Match` (an unmeasured probe and `Unknown` included) and the compositor hands out no RGB**: a composite asked for as RGB or as a texture (`Output::Rgb` / `Output::Texture`: `composite`, `render_plan*`, the presenter's frames) is `GpuError::Unsupported` and the preview is
FFmpeg's, the narrowest the preview path allows: the YUV planes of an export or a playback (`Output::Yuv`: `composite_yuv*`), which no conversion touches, are unaffected, and a frame with no layer (the black base, which every kernel converts to 0, or text in RGB over it) never reaches the conversion and is
handed out. It is not a plan reason (the plan does not know the preview from the export). The runtime self-test checks RGB frames only where the probe says `Match`, and is `Inconclusive` otherwise (`display_for_selftest`: the preview stays off, as the compositor would keep it). A harness that renders RGB reads
`display_probe::display()` in its priming (`render_plan` asks it itself), or pins `with_display(Match)`.

**The translucent round trip** (a constant opacity below 1; `roundtrip.rs`, `shaders/roundtrip.wgsl`, `roundtrip_probe.rs`, `GpuCaps::translucent_approximate`). `colorchannelmixer` only takes RGB, so FFmpeg takes the layer `yuva420p -> argb ->
yuva420p` around it: out with swscale's **C** `yuva2argb` (`ARGB` has no x86 SIMD version; the layer's own matrix), the alpha `lrint(255 * op)`, and back with the generic scaler
(luma per pixel, chroma from horizontal pixel pairs summed in 15 bits, then the vertical bicubic over the rows). **That vertical pass is the x86 `yuv2yuvX` kernel for every
chroma row but the last, which `swscale.c` gives to the C one** (the grade's way back, `fs_chroma_v_x86`, `grade::vertical_start`); the compositor used to run the C arithmetic here, and
that was the "translucent ladder": one level of U and V on about one chroma sample in fourteen, in every frame of every translucent layer in an export (`TRANSLUCENT`, 552,495 / 149,704 /
570,178 samples of `a_translucent_layer_dissolves_...`'s three rows on 6.1.1), up to two in a still's RGB. With the kernel it is **exact, every plane of every frame**, on 6.1.1 and 9.0.2 (the
`opacity` rows of `parity.rs`, `motion_parity.rs`'s `opacity/`, `dissolve/translucent-incoming`, `fade/translucent`, `grade/translucent`, `label/translucent-*`). **Which kernels this
machine's swscale runs is measured** (`roundtrip_probe::translucency()`: the export's own `colorchannelmixer` text, `kerf_core::opacity_filter`, between the two `format=yuva420p` conversions the
graph puts around it, over noise in all three planes at 60 %, 99.9 % and 4 %, untagged and BT.709-tagged, five cases whose alpha plane is a mask's (every level), and four whose alpha plane is a chroma key's (the next bullet but one), every plane of the layer `overlay` is handed held to `roundtrip::model` with no tolerance):
an arm64 build (NEON `yuv420_to_argb`, other vertical kernels), an SSE-only one or any other swscale is `Differs` or `Unknown`, `GpuCaps::translucent_approximate` is set, every layer that
takes the round trip (`RenderPlan::through_rgba`) is refused (`Unsupported::TranslucentApproximate`) and `translucent_planes` refuses it too. The runtime self-test draws its picture-in-picture
opaque on such a machine. **Every turn of a translucent layer is refused** (`Rotation`, "a turn of a layer below full opacity"; the upright layer is exact): FFmpeg turns the RGBA picture and converts it to 4:2:0 afterwards. A quarter turn
pairs other samples than the compositor's round trip, which comes before the turn (chroma tens of levels off); **a half turn pairs the same samples but filters the rows the other way round** (the vertical bicubic's
window is not symmetric): one level of U and V on 4,631 samples of 30 frames of `opacity/turned 180` on 6.1.1, found by the zero-tolerance harness (the base called it the same ladder as the upright layer's).
What an exact route needs: turn the RGB texture the first pass makes (`fs_rgb`) by the quarter or the half ahead of the way back (the pass `rotate` makes on the RGBA picture: `simple_rotate`'s copy), run the pairs and the
vertical filter over the turned picture (its height is the layer's width for a quarter), and compose the planes it makes without turning them again (`rotate` 0 and the turned box as the picture); the alpha is uniform, so it turns with nothing.

**Odd sizes of a translucent layer** (a side of the picture odd: `roundtrip::yuv_to_rgb`, `rgb_to_yuv`, `OddWidth`, `roundtrip_probe::odd_translucency`, `shaders/roundtrip.wgsl` `fs_chroma_up` / `fs_wide` / `fs_tall` / `fs_rgb_full` / `fs_chroma_h_scaled`,
`compositor/odd_passes.rs`, `GpuCaps::translucent_odd`; `ComposeParams.pad2[0]` / `composite.wgsl` `zero_col`). It was the single largest refusal in the ledger (1,528 frames, T13 and T25) on the stated ground that the chroma pairing "reads uninitialised
padding" and an odd height "takes a generic path". Both are true and **neither is garbage**: FFmpeg's output for an odd picture is a function of the picture alone, bit for bit, on 6.1.1 and 9.0.2 (read in libswscale `yuv2rgb.c`, `swscale_unscaled.c`, `output.c`, `input.c`, `utils.c`
and measured: frame 0 identical over `-threads 1` / `3`, `-filter_threads 1` and one or eight frames at five odd shapes; three pictures run in nine different orders, each equal to the picture run alone; 2,912 frames of every odd and mixed shape from 1x1 to 201x201 and 27 shapes up to 3841x2161, equal to the
model on both builds but at a height of 7). `-cpuflags 0` is not (the C vertical scaler: `Differs`, as for even sizes). What FFmpeg does:
* `yuva420p -> argb` of an **even height** is the unscaled converter (`yuva2argb_c`, the only gate is `!(dstH & 1)`), whose tail loop handles `dstW & 4` and `dstW & 2`: **an odd width's last pixel is never written**, and the frame pool (`av_buffer_allocz`) made it 0, 0, 0, 0 --
  a transparent black, so the picture's whole last column, luma **and chroma** (`overlay`'s alpha for the last chroma column is the last luma column's alone), is transparent (`zero_col`; `fs_rgb` zeroes it before a dip's fade, which the plan refuses for odd sides anyway).
* An **odd height** takes the generic scaler (`!(dstH & 1)` is the unscaled converter's gate): the chroma goes from `ceil(h/2)` rows to `h` by the vertical bicubic (12-bit weights, `(2^18 + sum) >> 19`) and through the same tables (`yuv2rgb_X_c_template`). With an **odd width too**
  `sws_init_context` forces `SWS_FULL_CHR_H_INT` ("Forcing full internal H chroma due to odd output size"): the chroma is also scaled to the width (14-bit weights, `>> 7`, 15 bits clipped at the top) and the colour is `yuv2rgb_write_full`'s
  13-bit `yuv2rgb_*_coeff` arithmetic in **32-bit integers that wrap** (a saturated picture's `Y + V * coeff` passes 2^31: 161 pixels of a 483x271 noise picture were 255 off in a model with 64-bit sums), clipped at 30 bits (`FullChroma`, `fs_rgb_full`).
  Every pixel is written, so there is no unwritten column.
* `argb -> yuva420p` of an odd width, **6.1**: the chroma of an RGB source is halved (`chrSrcHSubSample = 1`), so the last chroma sample is the pair (last pixel, the pixel after it), which is **in the row's padding** and still 0, 0, 0 (nothing writes it: the unscaled
  converter stops short and the full-chroma writer stops at `dstW`) -- the sum of one pixel and black (`OddWidth::Pairs`, `fs_chroma_h`). **9.0** added `!(srcW & 1)` to that rule: an odd width's chroma is made for every pixel (`rgb24ToUV_c`, 14 bits) and the horizontal bicubic
  (14-bit weights, `hScale16To15`'s shift of 13) brings the row to the chroma plane's width with its edge taps clamped to the last pixel: nothing is read past the line (`OddWidth::Scaled`, `fs_chroma_h_scaled`). An odd height then filters `h` rows to `ceil(h/2)`
  as the even one does (the x86 kernel for every chroma row but the last, which is the C one: `dstY >= dstH - 2`), the table now `sws::bicubic(h, ch)`.
* **The probe** (`roundtrip_probe::odd_translucency()`, its own two single-flight probes `ODD_PAIRS` / `ODD_SCALED`, `OddTranslucency::{Pairs, Scaled, Differs, Unknown}`; the scaled one runs only after the pairs one found a difference; `prepare` / `prepare_timeline` run it with the round trip): the export's own
  `colorchannelmixer` text over the nine cases of the even probe (noise in all three planes at 60 %, 99.9 %, 4 %, BT.709-tagged, and a mask's alpha plane holding every level in four orders at 50 %, 25 %, 85 %, 4 % and 99.9 % BT.709) on **47x31, 47x32 and 48x31** (both odd, the width, the height), all four
  planes held to the model with no tolerance. 6.1.1 is `Pairs` (the scaled model `Differs`: 367 U and 373 V samples), 9.0.2 is `Scaled` (the pairs model `Differs`); `-cpuflags 0` is neither. The probe has teeth: a model that keeps the unwritten column opaque (32 alpha samples), one a level off in the odd row's luma (47) and the other build's
  route are each `Differs` (`a_model_of_the_odd_sizes_that_is_not_ffmpegs_is_differs`). `Unknown` is not kept (asked again after the backoff) and draws nothing odd; so does `Differs`.
* **What the plan draws**: `GpuCaps::translucent_odd` is `translucent_approximate` off and a model found; the plan (`RenderPlan::translucent_odd_refusal`, `core-render-plan.md`) then draws a plain, colour-corrected, masked, chroma-keyed or dissolving odd layer and keeps FFmpeg's a side under 16 (the 7-row filter is two taps, which swscale's output functions take another way, and a model of the
  4:7 enlargement was 138 luma samples off on 6.1.1; every other height from 2 to 80, and 27 larger shapes, are exact), a turn, a blend mode, a grade, an effect other than the key. A fade to black on a translucent layer and a dip to white stay refused, as for even sizes.
* **Held to FFmpeg**: `parity.rs` `odd_translucent_layers_are_drawn_as_the_still_draws_them` (the fast set, CI: odd width, odd height, both, keyed, masked, a dissolve: seven stills) and `checker_odd_stills_every_shape_of_picture_position_and_alpha_plane` (53 more, local; one stack of two matrices is the conversion's and 9.0.2 refuses it: busy pictures, two 1080p frames,
  saturated bars, noise with every Y, U, V triple at both-odd sizes, opacities 0.04 to 0.999, BT.709 and BT.2020 stacks, a 9:16 and a 4:3 frame, crops, odd offsets across every edge, wholly outside, a colour correction, masks, keys, stacks over an odd opaque layer and over another translucent one,
  two dissolves at three moments); `motion_parity.rs` `odd_translucent_layers_are_drawn_as_the_export_draws_them` (the fast set, in CI's motion subset: four short exports, an odd width, an odd height, both with a key, and a dissolve) and `checker_odd_motion_every_shape_of_picture_position_and_alpha_plane` (29 exports, local: 29.97 and 25 fps, positions, a crop, masks, keys, dissolves,
  a slide and a push, and the fade to black that stays FFmpeg's). **0 differing samples in every plane of every frame, 6.1.1 and 9.0.2** (the stills: planes, PNG and display; 34 exports, 731 frames). The refusals have cases (`refused/translucent-odd-layer under 16 px`, `blended`, `with an effect`; `key+opacity/odd 19x10`, still and export;
  `what_a_translucent_or_resized_blended_clip_is_still_refused_for_is_refused` keeps its odd, blended case), and the plan's unit test holds each guard (`a_translucent_layer_of_odd_size_is_drawn_with_a_model_of_it_and_refused_beyond_what_it_was_held_to`: dropping the model, the minimum side, the turn, the blend, the grade or the effect guard, or counting an opaque or keyed layer, fails it).

**Keyed opacity** (export frames). FFmpeg's chain has a `geq` whose alpha is `(int)(opacity(T) * 255)` (`T` the frame's time on the
clip's `fps` grid) where a constant opacity takes `colorchannelmixer` and the RGB round trip. The plan evaluates it
(`PlanLayer::keyed_alpha`, `core-render-plan.md`) and the compositor blends with that one alpha in integers (`LayerFades::alpha`),
no round trip; the clip's fades and a dissolve's ramp act on it as on any alpha plane. The `geq` also copies Y, U and V through
`lum` / `cb` / `cr`, so on a build whose `geq` reads a plane's last column and row one in (6.1) the blend reads them so too
(`mask_edge`, as for a mask; ahead of a turn, see above: `tests/motion_parity.rs` `a_keyed_opacity_on_a_turned_layer_is_shifted_before_the_turn`, 0 differing samples at every quarter on a picture with detail at its edges, 6.1.1 and 9.0.2). Byte for byte at the picture's size (linear, eased, held, bezier off the frame grid, a tree of many
keys, 29.97, with fades, in a dissolve); the scaler's level on a pip or an odd size; turned by a quarter (any other turn is refused, "Rotation"). A mask with it is drawn
while the curve holds 1 (the `geq` writes the mask, byte for byte) **and below it**: one `geq` writes `a='(keep)*(curve)*alpha(X,Y)'`, which multiplies **left to right in `f64`** and
truncates, `((keep * curve) * 255.0) as u8` (`kerf_core::mask_alpha_plane_keyed`, the plan's `PlanLayer::keyed_opacity` being the curve's value that `keyed_alpha` truncates), so the compositor makes the
mask's plane for the frame on the CPU (every sample's keep in `f64`, no cache: the curve moves; a hard or well-inside sample is the shortcut `mask_alpha_plane` takes) and uploads it as the alpha plane
(`look_of`), with no round trip. The mask's byte and the curve's byte cannot give the product back, so this is exact only with the same order and the same last bit of the curve:
`motion_parity.rs` `a_keyed_opacity_under_a_mask_is_the_one_geq_product_of_keep_and_curve` holds both on **the knife's edge** (a rectangle's feather at the 255 grid's points where `keep * curve * 255` is an integer
on paper, and linear ramps whose lerp lands the same on thousands of samples of some frames: found by search; the other two orders of the product, `keep * (curve * 255)` and `(keep * 255) * curve`, differ from
FFmpeg's in 11,457 and 4,986 luma samples of six frames, so the case has teeth), and `engine::cli::rendered`'s `a_keyed_mask_plane_is_the_geq_planes_sample_for_sample` holds the plane to `geq`'s own over 17,006,856 samples
on both builds. A blend mode on such a layer and a keyed mask (`KeyedMask`) are still refused (a chroma key is drawn, below); **the alpha judged is the `geq`'s own (`keyed_alpha < 255`), not the sampled opacity**, which can sit on the
other side of 1 by the last bit. **Before a moving zoom** it is drawn on the frames where the curve holds 1 (the `geq` is the identity on
the alpha plane and copies Y, U, V: the zoom-last path of a mask, `edge_planes` ahead of the scale; exact on 6.1.1 and
9.0.2) and refused once it falls (`ZoomBehind`: its alpha plane would be scaled with the zoom). A **chroma key under it is drawn**: the one `geq` is `a='(curve)*alpha(X,Y)'` with `alpha(X,Y)` the key's plane, which on a build that
reads a plane's last column and row one in (6.1) is the key's alpha one in there (measured with the key unshifted: luma 21 off at the edge,
2636 samples over 14 frames; 9.0.2 is exact), and the product is the curve's `f64` times the level, stored truncated. The compositor makes the key's plane from the planes as
they were (`chromakey` comes first: before they are shifted), shifts it under `mask_edge` (`edge_plane`, any shape of picture, odd included: a keyed opacity has no round trip) and
maps it through `LayerFades::key_alpha`, 256 entries of `(curve * level) as u8` for this frame's `PlanLayer::keyed_opacity` (skipped when it is the identity), then blends as for any
alpha plane. The plan always carries the curve (`PlanLayer::keyed_opacity`) and the compositor refuses a keyed layer that has none. Before a moving zoom it is drawn on the frames where the curve holds 1 and refused once it falls (`ZoomBehind`), as a plain layer is.
`motion_parity.rs` `a_keyed_opacity_over_a_chroma_key_is_the_geq_product_of_the_curve_and_the_keys_alpha` (linear, rising, held, hard-edged, eased, many keys, 29.97, odd and resized layers, Cover and Contain fits, fades, a dissolve,
the zoom) and `..._on_a_knife_edge` (the curve held at 14/25, 7/25, 6/11, 3/11, 15/26, 9/14, 21/38, 15/22, 13/23 and 31/60, the fractions found by search whose `f64` product with a level is a hair off an integer, `knife_levels`; over `ramp`, whose key holds 115 levels,
and the case asserts that the key's plane, read back from the compositor, has at least 100 samples at levels where each of the three wrong models of the product -- the curve truncated to a byte first, the product rounded, single precision -- part from the right one)
hold it, 0 differing samples on 6.1.1 and 9.0.2.

**Keyed colour** (export frames). The clip's `eq` is `eval=frame`: each keyed number a quoted expression of `t` (the frame's timestamp on the
clip's `fps` grid, `pts * av_q2d(tb)`), which `eq` evaluates per frame, clamps through a `float` (`av_clipf`) and builds its Y / U / V tables from again.
The plan carries the numbers the expressions evaluate to (`PlanLayer::color`, `engine::keyed_color_value`: each keyed number through
`keyframe_value`, the others the clip's own; the temperature's `gamma_r` / `gamma_b` are `1 + 0.3 t` and `1 - 0.3 t`, which
`Color::temperature_gammas` computes in the same order; `sweep.rs` holds all of them to the evaluated text **to the bit**, at all five rates) and
`look_of` builds the tables from them every frame (`eq::luts`, as for a static grade: 768 bytes, a few microseconds, uploaded with the layer), so there is no
shader change. They compose with a following grayscale or invert and sit ahead of a blur, as a static grade's do. **Byte for byte (0 differing
samples, over noise in all three planes so that every table entry is looked at, identically on 6.1.1 and 9.0.2)** at the picture's size: each of the
five numbers alone and all five, linear / eased / held / bezier off the frame grid / many eased keys (a tree), the temperature crossing 0 (on a frame
and between two: `gamma_r = gamma_b = 1`, the chroma tables the identity) and going to 0, at 24 / 25 / 29.97 (`29.97` and `30000/1001`) / 30 / 60 fps, late on the
timeline (also at 29.97 and `30000/1001`, the start off the frame grid), at speed 2 / 0.5 / reversed, beside a static grade (the numbers not keyed are the clip's own), with the clip's fades, in a dissolve, after a dip,
beside a keyed opacity (an `eq` and a `geq` written per frame), under a mask, before an invert / grayscale / sharpen / vignette, translucent (`TRANSLUCENT`: the RGB round trip
reads the graded picture; Y 0 / UV 1), turned by a quarter (exact; any other turn is refused whole), and on a 10-bit,
4:2:2, 4:4:4, gray, RGB or BT.709 source (the `eq` runs on the picture the chain has made 4:2:0 by then, as for a static grade). A blur after it is exact (`gblur`'s fused recursion), a moving
zoom after it exact too (the grade runs at the fit size). **On a layer the compositor scales, the tables used to multiply the scaler's level** (the C kernel was a level from an x86 swscale's, and a table maps two values a level apart to values `eq::table_steps` apart: contrast 1.8 and gamma 2 step from 0 to 12 at Y 57, a brightness of 0.3 added from 0 to 19 at Y 15); a frame was drawn while that step was at most `MAX_AMPLIFIED_ERROR` (2) and FFmpeg's past it (`Unsupported::GradeSteep`, judged per frame and for static grades too, with the sharpen rule's geometry). **Where the scaler is measured exact (`caps.exact_scaler`: both FFmpegs of CI on x86-64) there is no level to multiply and the refusal is gone**: `eq_tables_of_any_step_are_the_export_on_a_layer_the_scaler_resizes` holds noise, a luma ramp and hard 16 / 235 edges through 15 grades, the ones that read 3 to 19 levels off before included, to 0 in every plane; the rule and its table steps stay for a machine whose scaler is not exact (the plan's unit test `a_grade_whose_tables_amplify_the_scalers_level_past_two_is_refused_on_a_layer_it_resizes`). Cost: about 20 microseconds when gamma or the temperature takes the `pow` path, under one otherwise,
kept per thread for the last four colours, so a clip's frames (the same colour) cost a lookup; built only for a graded layer with a scale upstream.
The sharpen rule below starts its worst case from the grade's luma step. **The refusals about grading the picture itself are for the clip, not the frame**
(`RenderPlan::graded`: a keyed `eq` is in the chain on every frame of the clip, neutral numbers or not): a full-range picture (`GradedFullRange`: FFmpeg 9 grades
the raw values before it converts the range, the compositor converts first) on every frame, the ones where the curve passes through neutral
included, and a graded layer in a stack that holds one where the composite's colour is negotiated (`GradedInFullRangeStack`).

**Effects.**

* `Grayscale` (`hue=s=0`: U, V to 128) and `Invert` (`negate`: `255 - v` on Y, U, V) are tables, composed with `eq`'s and applied in the scaler's vertical
  pass. **Sharpen** is `unsharp` 5x5 on luma in integers (the binomial `[1 4 6 4 1]` twice, `s + (((s - ((b + 128) >> 8)) * amount) >> 16)`). Byte for byte.
* **Blur** is `gblur` (`blur.wgsl`, `blur_out.wgsl`, `fma.wgsl`): the first-order recursive Gaussian in `float` (right, left, down, up; one compute invocation per row and column),
  then `postscale`, clip and `lrintf`, each plane at its own size. **It is exact where this FFmpeg's `gblur` is the fused-multiply-add recursion**, which is what its x86 AVX2 and
  AVX-512 kernels run (`x86/vf_gblur.asm`: `vfmadd231ps` for every step of the horizontal and the vertical pass, plain `mulps` for the boundary scale and `postscale`; the
  two kernels differ in lanes, not in arithmetic). The unfused order the shader used to follow (`x += nu * prev`, the product rounded first) read 597 samples of 112 pictures a level
  off; the fused one reads none, on 6.1.1 and 9.0.2 (`tests/blur_model.rs`: noise and a gradient with sparse noise at four sizes, sigmas 0.5 to 1024). WGSL does not promise an
  `fma` is fused and a compiler may fuse what is not, so **the fusion is done in integers** (`fma.wgsl`: the exact 48-bit product, aligned to the addend in a 64-bit window with a
  sticky bit, rounded to nearest even; the boundary scale and `postscale` are the same function with a zero addend), **for the non-negative finite floats the filter works on, and
  as IEEE has it where the result is subnormal or underflows** (a bright dot, bar or pixel on a field of Y = 0, or of U / V = 0, makes the recursion's running value decay through 2^-126 after about 40 samples at sigma 0.5 and 135 at 2: FFmpeg's fused multiply-add
  gives the subnormal there and then zero; the first version of `fma.wgsl` wrapped the exponent once the result was below 2^-126 and drew garbage, max difference 255, found by `checker_z`'s `z_blur_on_a_field_of_zeros_...`: the rounding position is now
  `max(top - 23, -149 - window exponent)`, a subnormal operand reads its exponent as 1, and an overflow is infinity), **on the bit patterns**: the working buffer and the uniform hold `u32`s and nothing but `fma_pos` touches a value,
  so no backend's flush of denormals (on a load, a store or a multiply) can change one. Held to `f32::mul_add` bit for bit on 1,019,463 triples (ties, a zero addend, an addend far above and far below the product,
  **subnormal operands, addends and results, the ties and carries at the quantum 2^-149, overflow, and the decay of the recursion's own running value to zero at sigmas 0.3 to 40**: `blur_probe.rs`). **The coefficients
  are in the domain or the blur is refused**: `kerf_core::gblur::gaussian(sigma)` is `gblur`'s `set_params` (in its own precision) when `nu` is in `[0, 1)`, `boundaryscale` is positive and finite and the gain is non-negative and finite, else `None`; the plan
  refuses a blur whose `gaussian` is `None` (`effect_refusal`), the compositor takes its coefficients from the same function, and the sigmas it excludes are those where `set_params`' `double` cancellation (`1 + 2 lambda - sqrt(1 + 4 lambda)`, `lambda` near the spacing of the doubles
  around 1) makes `nu` up to 1.99 and `boundaryscale` down to -3: about 1.05e-8 to 1.29e-8, computed from the derived numbers, not from a sigma range (the sweep in `gblur.rs` finds the band; `z_gblurs_parameters_are_non_negative_for_every_sigma_the_plan_draws` holds it from the
  other side). **Which kernel this FFmpeg runs is measured, not
  assumed** (`blur_probe::blur()`, once per process, the export's own `gblur` text over noise in all three planes of ten pictures: three 160x96 ones **chosen to tell the fused recursion from the fully unfused one** (the two differ on a few samples in a hundred thousand, so six small pictures of noise let an unfused build through: seeds found by running both models until a picture held five or more disagreeing samples, 6, 8 and 5; `the_probes_chosen_pictures_tell_...` holds it), **four more, one for each of the recursion's four sweeps** (rightwards and leftwards along the rows, downwards and upwards along the columns: the later sweeps re-round what the earlier ones left, so a build unfused in the first sweep alone differs from the model on about one sample in a hundred million, and a picture where it does was searched for over tens of thousands, 4, 34, 23 and 22 samples; before them the probe read `Match` on a build unfused in the horizontal forward sweep alone, which `checker_z` found; `every_sweep_unfused_alone_is_told_by_its_own_picture` holds each at two or more, `no_combination_of_unfused_sweeps_is_invisible_to_the_probes_pictures` every combination of sweeps, and `a_model_unfused_in_one_sweep_alone_reads_differs` runs the probe against each single-sweep mutation of the model with the real FFmpeg: all four `Differs`) and three small odd-sided ones for the kernels' tails; sigmas 2.5 to 1024; `a_build_without_the_fused_kernels_is_differs` runs the probe with `-cpuflags 0` (the C loop) and `-cpuflags -avx2-avx512` (SSE4's horizontal pass) and both are `Differs`, on 6.1.1 and 9.0.2; the translucency probe's `-cpuflags 0` is `Differs` too, 41 U and 32 V samples): a build with
  SSE4 only (four samples at a time through the powers of `nu`) or no kernel (a C loop, fused or not by its compiler) is a level off on the few samples that sit on a half, and has no
  blur in `Compositor::caps()` (`EffectKinds::BLUR` is removed on anything but `Match`, an `Unknown` included; the render refuses one too). A blur of sigma 0 is the identity on every build (`set_params`
  makes `nu` 0 and both scales 1) and stays drawn **where the kernel's hazards below cannot touch a sample of it** (only (2) is harmless at sigma 0; `plane_hazard_for(.., identity)`). **A blur is no source of a difference any more**: a sharpen after it, at any amount, and a chroma key after it are drawn (the amplification rule below counts only a scale that is not exact).
  **FFmpeg's own `gblur` is not the recursion on every plane shape at every slice-thread count, and a blur on such a plane is FFmpeg's**
  (`kerf_core::gblur::plane_hazard` / `picture_hazard`; found by `checker_z`'s `z2_a_blur_on_a_tiny_unscaled_picture` and `z2_a_blur_on_a_small_picture_in_an_export`). Read off
  `vf_gblur.c`'s slicing and `x86/vf_gblur.asm` (identical in 6.1 and 9.0) and held by `tests/blur_slices.rs` against both FFmpegs on both kernels (AVX-512's 16 lanes; AVX2's 8, with
  `-cpuflags -avx512`): 12 widths x 17 heights x 11 slice-thread counts (2,244 pictures in a simple graph, a 660-picture subset in a complex one, the edges of the rule; trimmed to fit the CI
  parity job, one to three minutes a build). No plane the rule clears differs from the model, and no plane it refuses is exact but for (2)'s race, which can hide a bad plane in a run. The hazards:
  (1) **a plane one sample wide or tall** (the horizontal kernel's loop over `width - 1` samples and the vertical one's over the rows below the first run once too many; width 1 corrupts the heap);
  (2) **more slice threads than rows** (`filter_postscale` has `min(width * height, threads)` slices of `height * job / jobs` rows; a slice of no rows still scales one whole vector from its start,
  so the first `lanes` samples are scaled twice, 0.65 times as bright at sigma 0.5, and racily); (3) **a slice of `lanes` rows or more over a plane `lanes` wide or less** (the blocked horizontal
  kernel runs its block of columns once before it asks whether the row is longer than that). The thresholds grow with the threads (chroma height 7 against 8 threads, 45 against 64), which is why the
  blur probe's smallest picture could not see it on a many-core machine.
  **Sigma exactly 0 lifts (2), and nothing else, with a proof**: `set_params(0)` is `0 / 0`, a NaN that `gblur` replaces by `nu` 0 and both scales 1, on every build. The empty slice then scales a vector
  of 8-bit values by `1 * 1` and clips it to 0..255 again; the real slice that writes the same samples does the same, `x * 1` is `x` and the clip is idempotent, so every interleaving leaves the samples
  as they were, and the vector's part past the plane is the buffer's never-written tail, which nothing reads as a sample (valgrind sees no access outside the allocation). `plane_hazard_for(.., identity)`;
  `a_blur_of_sigma_0_is_the_picture_on_every_plane_the_lifted_rule_clears` holds it on both builds and kernels (552 + 230 pictures, two runs each: none differs). A tiny sigma whose derived coefficients are the
  same numbers (`Gaussian::is_identity`: 1e-5, 1e-9) is **not** lifted: that is a cancellation in the filter's `double` arithmetic, build-dependent in principle and not measured by the blur probe (1e-6 is a
  gain of 2e17). (3) **stays refused at sigma 0**: the loop reads and writes past the row, what it reads reaches the row as `x + 0 * past`, which is `x` only while `past` (the next row's first samples, or the
  never-written tail) is a finite float, and nothing promises that; it was the picture in every run, which is the heap's luck. (1) corrupts the heap whatever the coefficients.
  **The rule is computed, not a margin**: the planes are the picture's luma and half-size chroma (rounded up), the threads the graph this frame stands in for runs on (`RenderPlan::graph_thread_range`: the
  compositor's pin; one for a still whose every input is a proxy or an image, `single_frame_args`; the budget's where `limit_args` caps the graph (the app's default is 75 %, so four cores run
  `-filter_complex_threads 3`); else FFmpeg's default `min(av_cpu_count() + 1, 16)`, one on one CPU), and the vector width the one the blur probe found (`GpuCaps::blur_lanes`; `0` judges both). **`av_cpu_count` is
  reproduced exactly on Linux and read at each judgement**: `kerf_core::ffmpeg_cpu_count()` is `CPU_COUNT(sched_getaffinity(0))` (the same code in 6.1 and 9.0), one system call, not cached (and the cores the budget is a share of come out of the same mask, `engine-cli.md`: no pair of two reads to tear), because a running
  process's affinity changes (a cgroup cpuset resize by Docker or the Kubernetes CPU manager moves every thread, `taskset -p` without `-a` one) and an FFmpeg started afterwards counts the new one
  (`checker_p2_threads_the_plan_follows_an_affinity_that_changes`); `cores()` (`available_parallelism`'s rule, mirrored) also applies a cgroup's CPU quota, which FFmpeg does not, so a container limited to two of sixty-four CPUs
  runs FFmpeg's graph on sixteen. The range is that one number. **Where the count is `None`** (Windows and macOS, not reproduced; a failed `sched_getaffinity`, where FFmpeg's `av_cpu_count` keeps its initial **one**:
  measured under a seccomp filter on 6.1.1 and 9.0.2, `checker_p3_count_a_failing_call_is_no_count_and_its_range_holds_the_one_cpu_ffmpeg_then_counts`) **the range is `1..=16`**, a blur is judged over all of it
  (a 12x64 blur cleared at 5 threads was NarrowBlocks at FFmpeg's 1, Y 707 off), and **a chroma key is not drawn**: its window starts over at each slice, `Compositor::graph_threads_for` is `Some` only for one
  number, and the plan refuses a keyed layer otherwise (a key over bars at 64x64 differed by 4 samples a plane when the low end was taken for the count). **FFmpeg's own process must count what Kerf does**: the
  distro build links `libgomp`, which binds its initial thread under `OMP_PROC_BIND`, `OMP_PLACES` or `GOMP_CPU_AFFINITY`; `cli::command` removes the binding variables from every child's environment (the
  measurements and the cost are in `engine-cli.md`; `checker_p2_threads_the_plan_follows_the_environment_the_openmp_runtime_reads`). `the_default_slice_threads_of_a_graph_are_inside_the_range_the_plan_judges`
  and `an_exports_own_argv_at_every_budget_is_...` hold FFmpeg's default and a real export at 25/50/75/100 % to the plan, and `the_cores_ffmpeg_counts_are_the_ones_reproduced` (ignored) compares the count with
  FFmpeg's own `-v debug` "detected N logical cores"; `checker_p2_count` and `checker_p2_threads` re-run the tests under `taskset -c` masks. The blur probe captures the range once per process
  (`blur_probe::probe_found`) and the plan follows the live one: the probe's shapes sit at the ends of the range of its moment, which is enough to find the vector width and to test the rule.
  **The probe finds the width and checks the rule** (`blur_probe::shape_cases`: 12x16, 8x8, 16x32, 24x40, 40x40 at one thread, 12x34 and 8x16 at two, 40x4 and 40x6 at three, and at both ends of this machine's
  range planes one row short of the threads, at the threads and with room, each plane predicted by both widths' rules; a case with a plane under two samples across is left out: `(40, 2t - 2, t)` and
  `(40, 2t, t)` are 40x2 at two threads and at one, a chroma plane of 20x1, which FFmpeg can crash on; `the_shapes_tell_the_8_lane_kernel_...` holds every range of 1 to 16 threads). A width whose rule clears a plane
  that is not the model's is unsound and dropped; none left is `Differs`, one is the width, both is "not known which". A child that would not run does not hide a `Differs` found in the others (it makes a
  clean probe `Unknown`). The picture pairs that decide fused from unfused run on one explicit slice thread, so a 16-core machine's default does not decide them. **The probe runs at most 29 children** (ten pictures,
  four of them again on this machine's thread counts, fifteen shapes), four at a time on one deadline of 45 s (`TIMING.overall`, `MAX_CHILDREN`: eight rounds of about 5 s), so a host that is slow to start a process
  does not time out into `Unknown` on the shapes alone.
  **The blur is the compositor's one compute pass, and on a
  software adapter it waits for the render passes before it** (`Compositor::settle`: submit, wait): Mesa 23.2's lavapipe starts a dispatch while its rasteriser threads
  are still drawing the texture it reads, whatever barrier wgpu records, so a blur straight after a sharpen (the slowest pass) blurred half-drawn rows
  (Y up to 210 off, a different frame on every run: the `effect/sharpen 0.5 then blur on a pip` case, on every lane's baseline; exact with `LP_NUM_THREADS=0`, which is how it was found).
  A hardware adapter is never stalled.
* **Vignette** is `vignette` with its defaults (`vignette.wgsl`, `effects.rs`): a factor per luma sample, `cos(angle * d / dmax)^4` in `double` stored
  as `float` (each axis offset truncated to an integer; the angle is `PI/5` **as a `float`**: `av_clipf` clips it, and the `double` angle puts the
  factor a `float` step high on some samples, a level where the sum sits a few millionths over an integer: 7 samples in 3.5 million, found by the
  Motion harness); luma `(int)(f * s + dither)`, chroma `(int)(f * (s - 127) + 127 + dither)` with its block's top-left factor. The `float` products and
  sum are done in integers (no backend's FMA contraction can move a level), the map is a storage buffer made once per size. The **dither is an LCG that
  runs on across frames** (one step per sample, Y then U then V): sample `i` of the chain's `n`th frame is state `n * (w*h + 2*cw*ch) + i`, reached
  through tables (`vignette_steps`: `A^x` and `G_x` per column, each row's first state, so a sample is one multiply-add). A still plan's `n` is 0 (the still graph runs one frame through the chain); an export frame's is its clip's
  (`Pick::output_index`, counted by its `FrameCursor`: `FrameCursor::count`, `MotionFrames::counts`, passed by `render_motion_texture` and
  `composite_counted` / `composite_yuv_counted`; without it the frame is `Unsupported`). Byte for byte, still and export, every frame (the late start,
  speed, reverse and dissolve cases included), on 6.1.1 and 9.0.2. The count is the **full cut's**: a range export (`rendered.slice`) and the
  preview's hand-over stream start their graph at the slice, so their dither restarts there, a phase the GPU does not follow (a level on a
  dithered sample, never more). **Full range:** a full-range picture is refused when its format says so (`yuvj*`) **or its range flag does** (`StreamInfo.color_range == "pc"`, schema v10; `PlanStream::maybe_full_range`): a VP9
  (HEVC / AV1) file probes as `yuv420p` with `color_range=pc`, FFmpeg vignettes its raw values and the GPU would vignette the converted ones (measured 12 to 13 levels off, still and export). The invert, blur and sharpen are refused with it. **Gap, narrower:** an asset stored before v10 has no flag
  (`None`, "not known", judged by its name as ever) until the file is probed again (a relink replaces its streams; the library does not re-probe on open), so a pre-v10 `yuv420p` file that was `pc` is still drawn wrong.
* **Chroma key** is `chromakey` after `format=yuva420p` (`chromakey.wgsl`, `effects.rs`): the layer's alpha plane, at the picture's size, from
  its chroma (the effects change none of it, so only the last key counts). Each luma sample's alpha is the mean of nine `sqrt((du² + dv²) /
  (255² * 2))` over the 3x3 window of chroma samples around it, from the key's U, V (`RGB_TO_U` / `RGB_TO_V` in 10-bit fixed point of the
  colour's RGB, `kerf_core::color_rgb`: FFmpeg's colour table), summed in `double`, then `(mean - similarity) / blend` clipped and scaled to 255
  (a hard 0 / 255 at a blend of 0.0001 or less). WGSL has no `f64`: the terms are a table of `double` bits, the shader adds them in integers
  (`chroma_key_add`, round to nearest even), and the rest of the filter's `double` arithmetic is monotone in the sum, so it is 255 thresholds
  the CPU finds by bisection (`chroma_key_thresholds`) and the shader counts. **Outside the picture** a window entry is not reloaded: it keeps
  what the filter last wrote there in the same slice, the key colour where a slice starts, so the first sample of each slice depends on the
  graph's **slice threads** (`min(h, threads)`), and the compositor uses those of the graph the frame stands in for, read at each draw (once, by the first keyed layer)
  (`Compositor::graph_threads_for`): one for a paused frame or scrub window whose every input is a proxy or an image (`single_frame_args`),
  else `kerf_core::cpu_exact_graph_threads()` (the budget's when it caps the graph, else FFmpeg's default of `av_cpu_count() + 1`, at most 16, where `ffmpeg_cpu_count` reproduces the count: Linux; **`None` where the range is not one number, and the plan refuses the keyed layer**), which is the
  playback stream's and an export's; `Compositor::with_graph_threads` pins it. An FFmpeg run with another thread count differs at those
  samples, as FFmpeg's own runs do. Byte for byte against the filter at the same thread count (`Compositor::chroma_key_alpha` reads the
  plane back for the plane-level test; the parity stills and export frames use the budget's). **Full range:** a `yuv420p` picture whose full range is only a `color_range=pc` tag (VP9, HEVC, AV1) is refused (schema v10 records the range): FFmpeg keys its raw chroma, the compositor the converted one (19 levels off on a gradient and 73 on the test pattern under 9.0.2; 6.1.1 happened to agree on the gradient). The same **gap** as the vignette for an asset stored before v10.
* Refused (`Effects`): a vignette on a picture that is not `yuv420p` or that is full range (by its format or its `pc` flag: `maybe_full_range_in`; FFmpeg vignettes it in its own
  format: other planes, another dither course), a vignette map over the device's storage-buffer limit; a chroma key after a scale
  **where the scaler is not measured to be swscale's** (`!caps.exact_scaler`; its alpha moves by up to 1/blend levels for a level of chroma, and a blend of 0 flips it 0 to 255: drawn only on chroma that is
  FFmpeg's to the sample, which a measured scaler leaves — the lift is held by `a_chroma_key_after_a_scale_is_drawn_as_chromakey_draws_it` in `parity.rs` and `motion_parity.rs`, on both builds: 3 shrinks x 5 keys x noise and a test card x 2 key colours, the key at every position among the other effects, with an `eq` ahead, the green-screen shape, and (motion) in fades and moved, **and enlargements** (a 640x360 clip into a 960x540 Cover frame, 1.5x, and a 1.25 zoom on top) at every key position; the still version asserts that the keyed layer is on screen at the frame it looks at, the noise being 0.2 s long, which it once was not, so that every noise case was a frame of bars), on a picture that is not `yuv420p` or that is full range, on a rotated layer or on a blended layer below full
  opacity (a constant or keyed opacity is otherwise drawn, see above), on one narrower or shorter than 2, or with a similarity or a blend `chromakey` refuses (0.00001 to 1, 0 to 1); `MaskWith`: a mask over a chroma key (`geq` multiplies the key's alpha). A sigma outside 0 to 1024
  or an amount outside -2 to 5 (FFmpeg's options refuse them too), a blur over the device's storage-buffer limit, and, **where the scaler is not measured to be swscale's** (`!caps.exact_scaler`), a **sharpen that amplifies a difference upstream of it** (judged with the geometry, so by render size): that compositor's scale is a level from FFmpeg's, and `unsharp` carries it through at worst `1 + 2|amount|` at an edge sample and rounds up a level more with each further sharpen (its `>> 16` floors). From the scale (a scale before the effects: every stage, or the fit scale when a zoom comes last; a blur is no source: it is `gblur`'s to the sample or not drawn) the worst case starts at 1 and each later sharpen that is not zero at `unsharp`'s 16 bits adds `ceil(2 * err * |amount|)`; the frame is refused when it passes 2. So one sharpen of at most 0.5 is drawn (0.6 is not), and any second one is refused, however small. With no source upstream, or a measured scaler, a sharpen is exact at any amount, chained or not, after a blur too. `Matte`: a chroma key on a
  letterbox (an alpha plane cut from the padded frame), and a blur, a sharpen, a vignette or a grade on one for a compositor without `caps.matte_effects` (below: they
  see the bars as picture; tables carry the bars through). By the picture (`effect_on_picture_refusal`): an invert,
  blur or sharpen on a **full-range** picture or one whose format was never recorded (`maybe_full_range_in(policy)`: where FFmpeg 9 negotiates the range this
  holds a deeper-than-8-bit one too, which may carry a `pc` tag; FFmpeg runs the filter on the raw values and converts the range afterwards: an invert is 8
  levels off, a sharpen's overshoot 26, on 9.0.2; grayscale is the same on any range), and an invert of a **gray** picture (FFmpeg leaves
  its chroma at 128, the compositor's planes would go to 127).
* 4:2:2, 4:4:4, RGB and gray pictures (and 10-bit under `FixedBt601`) take every other effect and a mask at the plain source's own figures (max 3: the decode's rounding).
  Compute passes are core wgpu on every backend and the blur writes no storage texture: no optional feature.

**A letterbox's padded frame** (`GpuCaps::matte_effects`, `compositor.rs` `pad_planes`, `planeops.wgsl` `fs_pad`). FFmpeg's chain is `scale, pad, setsar, fps, eq, grade, effects, format`:
`pad` hands everything after it the whole canvas-sized frame, bars included, so a blur runs the bars into the edge of the picture, a vignette's centre and size are the
frame's (its dither runs on across the clip's frames as ever), a sharpen overshoots along the picture's edge and a grade's RGB round trip pairs the bars' chroma with the
picture's. The compositor used to draw the picture and the bars apart (the compose pass puts the matte around the picture at the end), so it refused them (`Matte`). Now a layer
with `geom.matte` that has a blur, a sharpen, a vignette, a grade or a table after one of those lays the **planes of the padded frame out whole** before the grade and the steps:
`fs_pad` writes, per plane, the picture's sample where the luma sample lies inside `picture_shows` at `picture_at` (a chroma sample is the picture's chroma block of its top-left
luma sample: the offset is even) and the bars' value everywhere else (Y 16, U = V = 128 through the `eq` tables and the pointwise effects ahead of the first blur, i.e. the same
`matte` the compose pass uses), and from there the layer is that frame (`picture = layer`, `matte = false`, as under a blurred fill, where the frame is already whole): every pass
(`blur_planes`, `sharpen_planes`, `vignette_planes`, `grade_planes`, the tables) reads the planes' own sizes, so each is the filter on a frame of the canvas' size and nothing
else changed. The plan judges the same frame: the blur's kernel hazards (`gblur::picture_hazard_for`) on the layer's planes, `effect_picture` (the storage limits, the mask's size)
as the layer. **Held** (`RenderPlan::padded_refusal`, `Unsupported::Effects` naming what is not): an 8-bit 4:2:0 `yuv420p` picture (any other format is refused because it is **unproven**, not because it is
filtered in its own format: FFmpeg's negotiation converts it to 4:2:0 at the fit `scale`, ahead of the pad, and blur and sharpen of a 10-bit, 4:2:2, 4:4:4, gray and RGB picture at its own size were
exact on the examples tried, on 6.1.1 only; a vignette is refused for them anyway, and nothing in the archetypes needs the lift), no mask (cut from the same frame, with the `geq` edge rule on the padded planes) and no
keyed number. A letterboxed layer is the identity pose, so it is opaque and upright. A grade on it additionally keeps its own rules (`GradeWith`): the picture's **own** sides even (without an `eq` ahead of
it the negotiation has the fit `scale` output RGB and `pad` run in RGB, where an odd picture's odd row and column are dropped differently: 85 to 146 levels of luma off on 203-wide and 203-high pictures with no
`eq` on 6.1.1; a resized layer needs the `eq`), and **no `eq` ahead of it at an odd offset**: in RGB `ff_draw_round_to_sub` rounds nothing, so `pad` keeps the picture at the unrounded `(ow - iw) / 2`
where the compositor pads at the even offset (the YUV `pad`'s, which an `eq` ahead keeps in the chain). `(canvas - picture) / 2` odd on either axis, no `eq`, no blurred fill (composed in YUV, exact at any
offset) is refused (`GradeWith`, "at an odd offset"; also in `grade_geometry_refusal`): 640x360 in 640x722 (y 181) or 642x360 (x 1), 1080x1350 in 1080x1920 (y 285). **A sharpen is refused on a frame under
8 rows** (`UNSHARP_MIN_ROWS`, `Effects`, "a sharpen on a frame under 8 rows high"; in the compositor's `look_of` too): `unsharp` is not the model on a frame of 2, 4 or 6 rows, exact from 8 and whatever
the width, with or without bars. It is an old bug of main (no bars needed) that the letterbox made reachable; the frame is the padded one, so a 4-row picture in a tall frame is drawn and a 400x4 frame is not. A chroma key is still `Matte`: its alpha plane is
cut from that frame, and no case holds it. A converted layer (FFmpeg 9, another matrix than the composite's) with any effect or grade is `MixedMatricesWith` as before. The cases: `parity.rs` `effects_on_a_letterbox_are_drawn_as_the_still_draws_them` (34 stills: 320x640, 384x682 with uneven bars, 360x640 with
the odd row gone, 640x720 at the picture's own size, noise, a pillarbox, bars a table has been through, a dissolve, dip, slide, push and fades, a blurred fill), `a_grade_on_a_letterbox_is_drawn_as_the_still_draws_it` (24, BT.709-tagged pictures among them),
`what_a_letterbox_leaves_to_ffmpeg_is_refused` (9 refusals: a chroma key, a mask, a 10-bit picture, an odd picture's grade, a resized grade with no `eq`, two odd offsets, a 4-row sharpen), all in CI's fast set (about 40 CPU-seconds net on 6.1.1, 30 on 9.0.2: the probes dominate a single run), and the heavy sweeps
`checker_mt_stills_*` (123 stills of 1080x1920, 960x1708, 540x960, uneven bars from 600 to 706 rows, tiny frames; 212 around transitions and fades; 60 grades; 24 at the preview's width; skipped in CI,
`--skip checker_mt_stills`) and `motion_parity.rs` `effects_on_a_letterbox_are_drawn_as_the_export_draws_them` and its siblings (203 export cases, 2,694 frames, 42 of the cases at delivery sizes from 1080x1920 down to 64x96 and 10 from 1080p shots), 0 differing samples
in every plane on 6.1.1 and 9.0.2. The independent checker's `checker_mt2_stills_*` and `checker_mt2_motion_*` (skipped in CI with `--skip checker_mt2`) draw 1,185 stills (6.1.1; 1,179 on 9.0.2) and 561 export
cases of 9,480 frames (555 and 9,408), 0 samples differing, and hold the refusals above (the odd offsets, the sharpen under 8 rows, the odd-row cases `odd_pad` always refused). The ledger: T14 (6.1.1) 12 to 702 stills and 10 to 700 export frames drawn of 942, 9.0.2 12 to 702 stills (its export frames are the cut's `full_range_cut`).

**The blurred fill** (`Fit::Blur`; `kerf_core::blur_fill`, `GpuCaps::blur_fill`, `compositor.rs` `blur_fill_planes`, `fill.rs`,
`planeops.wgsl` `fs_box`). A clip placed by the plain fit whose picture is not the frame's shape sits over a copy of itself
(`engine::cli::blur_fill`): `scale` to *cover* a thumbnail of `2*floor(w/16)` x `2*floor(h/16)` (at least 4x4) with `flags=bilinear`,
`crop` about its centre (`lrint((in - out) / 2)` rounded down to even), `boxblur=r:2` (`r` a sixteenth of the smaller side, at least 1),
`scale` back up to the frame (bilinear), and the picture's own fit `overlay`ed on it. The compositor draws it plane for plane, **0 differing
samples**: the picture's planes through two bilinear scales on swscale's own tables and kernels (`scaler.rs`; the thumbnail is
a 4.5:1 shrink and the fill an 8:1 enlargement on a 1080p frame, both exact in `tests/scaler.rs`), the blur as `blur8` writes it
(a running sum with the line mirrored about its ends *including* the edge sample; the shader sums the whole window,
`(sum * inv + 32768) >> 16`, `inv = (65536 + length/2) / length`, four passes a plane, `fill::box_blur` is the C port the
tests hold to FFmpeg's), `eq`'s tables applied in the vertical pass that scales the fill up (the filter runs on the whole padded
frame), and `overlay`'s copy of the fitted picture at its even position (a chroma sample lands whole, so the half block of an odd picture's
last column and row is the picture's while the luma beside it is the fill's: a texture copy of the picture's planes into the fill's). The layer is then the whole frame
with no matte (`geom.picture = geom.layer`), so fades, dissolves, slides and pushes act on it as on any padded frame.
**Which layers**: `takes_blur_fill` is the clip's (identity transform, not keyed, no reframe, no mask, no alpha, no alpha-producing effect:
`PlanLayer::blur_fill` carries the picture the graph judges the shape by, the original's) and the render size's (a picture of the frame's
shape has nothing to fill: `RenderPlan::blur_fill(layer, size)`); every other clip is placed by Contain's chain, byte for byte, and is not
refused for the delivery's fit. **Refused** (`Unsupported::BlurFill` without the pass or the exact scaler under it; `BlurFillWith` per layer):
a picture that is not 8-bit `yuv420p` (FFmpeg scales and blurs the thumbnail in the picture's own format: drawn anyway, 10-bit Y 2 / UV 2, 4:4:4
33 / 55, 4:2:2 0 / 27, gray 33 / 0), a full-range picture (raw samples: 14 to 18), an HDR one (it composes in 10 bits ahead of the tone-map), a thumbnail
shrunk past 63:1, a frame whose chroma lines are shorter than the box (`2r + 1`: `boxblur` reads past them; FFmpeg 9 refuses to configure it at all)
— and a chroma key on the padded frame (an alpha plane cut from it) and a dip to white. A blur, a sharpen, a vignette and a grade over a fill are drawn
(the planes are already whole: `matte_effects` above; `what_runs_on_the_padded_frame_is_drawn_over_a_fill_and_a_chroma_key_stays_ffmpegs`, a grade with the picture's own sides even). `tests/motion_parity.rs` (`a_blurred_fill_*`, 7 tests, 117 cases) holds the export's planes to `EXACT` on both FFmpegs: portrait / landscape /
square, noise, hard edges, odd and 20:1 sources, frame sizes from 96x64 to 3840x2160 (radii 1 to 16), fades, dissolves, dips,
the eight slides and pushes, over and under other tracks, `eq`, grayscale, a keyed colour, speed, reverse, a late source, every matrix (untagged, BT.709,
`smpte170m`, `bt470bg`, BT.2020) and a `tv` flag. `tests/parity.rs` (`a_blurred_fill_is_drawn_as_the_still_draws_it`) holds the still exactly too, through
`still_planes_differ`: the still's own `yuv444p` planes (`timeline_frame_yuv_cancellable`) against the compositor's composite put through the same
conversion in a graph of the same kind — **an exact comparator for still plans** that the RGB one cannot be before the conversion is exact; it holds on both FFmpegs.

**Blend modes** (`blend.rs`, `shaders/blend.wgsl`, `compositor/blend_passes.rs`, `GpuCaps::blend`, `tests/blend_kernels.rs`; the export graph is `engine-cli.md`, "Blend modes").
FFmpeg lays the clip opaque on a copy of the composite (`cc`), its alpha plane alone (`aa`), `alphamerge`s, converts both frames to `gbrap` and back to `yuva420p` with
`scale=flags=neighbor` and mixes them with `blend` in RGB, then `overlay`s the result onto the untouched composite. **Everything after the placement is reproduced in
integers, to the level**, and each stage was found by measuring and held to the filter over every value, **identically on 4.4.2, 6.1.1 and 9.0.2**:
* **Into RGB** is `yuv2gbrp_full_X_c`, the C path (no SIMD exists for planar RGB out; **`gbrp` is another path with other arithmetic, `gbrap` is the chain's**): chroma repeated 2x2,
  `(Y*512 - 8192) * 9539 + 2^21` and the chroma terms in **32-bit integers that wrap** (a saturated blue overflows and is clipped to 0 by the wrap, not to 255), clipped to 30 bits, `>> 22`;
  swscale's own coefficients (`Tables::new`: BT.601 `9539 13075 -6660 -3209 16525`). All 16.7 million (Y, U, V), both matrices, the frame with and without an alpha plane.
* **The mode** is `vf_blend.c`'s `int` expression, clip on top (kerf's `Overlay` is FFmpeg's `hardlight`, `HardLight` its `overlay`); the x86 SIMD ones (SSE2 / AVX2, taken at opacity 1)
  are those expressions (`multiply`'s `(x + 1 + ((x + 1) >> 8)) >> 8` is `x / 255`): every pair of values, all nine modes.
* **Out of RGB** is `planar_rgb_to_y` / `_uv` and swscale's `fill_rgb2yuv_table` (BT.601's literals): luma `(((ry r + gy g + by b + 0x80100) >> 9) + 32) >> 6`, and **chroma from the bottom row of each
  2x2 block, the sum of its two pixels** (`((ru R + gu G + bu B + 0x800200) >> 10) + 32) >> 6`): `neighbor` is a point sample, but RGB sources are read halved horizontally and the point lands on the odd row.
* **`lutyuv=a=255`, which lays the clip opaque, is `clipval`**: the clip's Y is held to 16..235 and U, V to 16..240 (noise showed it: 17 % of the samples a few levels off).
  The compositor does the same to the clip's samples inside its rectangle and to the chroma blocks it touches (`clip_colours`); the composite outside the clip is not clamped.
* **The last `overlay`** weighs the mix by the clip's alpha per luma sample and per chroma block (`blend255`; the block's alpha is the average of its four, a last column ignoring its own second luma column:
  `chroma_alpha`), over the canvas's chroma size. The alpha is the layer's own: opacity (static is refused, below), a keyed one, a mask or a key's plane, and **its fades, which are all alpha fades**
  (a multiplied clip must not go through black): the clip's fade-in and fade-out, a dip's halves and a dissolve's ramp are each a `fade alpha=1` with its own rounding, and **the chain runs them in `ClipTiming::fades`'s order, not in-then-out**
  (`LayerFades::ramps`, up to eight, in `fade_ops`'s order: a dissolve that overlaps the fade-out ramps after it; a fade-in beside a crossfade on one edge is two steps, drawn). Outside the clip's rectangle it is 0.
* **The matrix is the one the graph names**, not the composite's (`PlanCanvas::blend_tag`: the bottom layer's tag over the whole cut for an export, over the layers on screen for a still; BT.709 for a labelled one; none = BT.601;
  `blend_matrix()`): BT.601, BT.709 and BT.2020 have tables (BT.2020 where the probe matched, `BlendExtras::BT2020`); `fcc` and `smpte240m` are refused, **and so is every tag that is not on the allow-list** (`kerf_core::model::matrix_tag_is_unlisted`: `bt2020c`, `ycgco`, `ictcp`, `chroma-derived-nc` / `-c`, `smpte2085`, `gbr` on YCbCr), on the bottom layer or on the blended clip, under every policy: FFmpeg 9.0.2 fails to export such a cut at all, and FFmpeg 6.1.1 reads it as untagged.
The passes: copy the canvas region (whole 2x2 blocks) to `before` and `laid`, draw the clip opaque onto `laid` with the plain layer's own pass (so its place, odd edges and chroma blocks are the plain layer's), then `fs_blend_rgb`
(both frames to RGB, the mode, RGB out) and `fs_blend_over` (back to 4:2:0 and the integer blend onto the canvas by the alpha). `Compositor::blend_planes` runs the passes alone on planes the caller made.
**Does this FFmpeg do that?** `blend::kernels()` (once per process, off every lock, `prepare` asks) runs the export's own chain text (`kerf_core::blend_colour_graph`, `blend_probe_graph`) on noise, per mode and per matrix (BT.601, BT.709, BT.2020),
and a clip placed by the export's `overlay` at the frame's edges, and keeps the modes that came back as `blend::reference` computes them; `caps.blend` is those the two matrices agree on, and nothing until it has run.
The same probe sets `caps.blend_extras` (`blend::extras()`): `BT2020` when all nine modes and the placement matched in BT.2020, and `RGB_PICTURE` when **the export's own chain** (`blend_probe_graph`, an `rgb24` clip into its `format=yuva420p`, placed by the real `overlay`) comes back as the model's blend of the planes the decode makes of the same noise (`scale=out_range=tv`, then 4:2:0: one more child), BT.601 (an RGB picture is refused in any other stack). Not the two conversions side by side: 4.4's are equal on noise and its blended RGB picture is not the decode's. And `TRANSLUCENT` (below), the same chain on a clip below full opacity.
**The probe is bounded and keeps only a complete answer** (`judge`, `run_child`): one deadline (`PROBE_TIMEOUT`) for all ~44 children, run `WORKERS` (6) at a time, none under a lock, **one measurement at a time whoever asks** (`Probe::found`: a second caller waits on a condition variable for the first, no longer than the measurement's own bound plus a grace -- then it is given "not yet", nothing drawn -- and takes its answer, a kept one or the backoff's nothing, rather than launching the children again; the state is locked to read and to store, never while a child runs); a measurement that panics (or a worker: its job counts as a child that did not answer) is contained -- not measured, the backoff set, the place given up (`found` catches it and finishes with nothing; `Turn`'s drop is the backstop for a finish that itself unwinds) -- so later callers do not probe again at once; a late child is killed and its pipes are never joined on (the readers are detached and answer over a channel, so a wrapper script's grandchild holding a pipe cannot hold the probe). A child that did not answer is logged with the job it probes and the tail of its stderr (`Failed`, 512 bytes). **A child that did not answer** (late, failed, no frame) **takes what it probes out** (its mode, its matrix's placement, the RGB lift, the translucent chain, BT.2020) whatever the others found, and the probe is then not kept (`Verdict::kept`): `found()` draws nothing and retries after the backoff, so a timeout on a loaded machine neither draws a mode unmeasured nor switches one off for the process's life.
**FFmpeg 4.4.2's probe fails, and that is a real bug of that build**: its `overlay` slices the alpha plane of a clip placed above the frame's top edge wrongly when it runs on several threads (the alpha loses rows: the blend does nothing; Y 212 off), so on 4.4.2 every blend is refused,
and the parity cases there check the refusal and then draw with the caps forced (`forced_blends`, `blend_check`) to hold the arithmetic to the export's planes for the clips not above the edge.
*Refused* (`BlendMode` with its cause; every cause is listed): a rotated one **except a quarter turn, half or three quarters (`kerf_core::quarter_turn`: the `rotate` filter lands on whole samples there; any other angle and a keyed rotation are refused, "Rotation")**, a letterboxed one (its bars are transparent in the blend), **a graded one (a blended clip's chain runs `format=yuva420p` and the transparent `pad` before the grade, so FFmpeg's RGB filters run `yuva420p -> argb -> yuva420p` where the grade's model is `yuv420p -> rgb24`: Y 11 / UV 9 off, every case)**, an RGB picture where the probe says the graph's conversion is not the decode's (FFmpeg 4.4's is Y 3 / UV 11 off), a matrix with no tables (an unlisted tag included; BT.2020 where it was not measured), **a layer a moving zoom or position places** (the placement ends the clip's chain in `pad`, `perspective` and `crop`, and no case holds the blend's two placements of that picture to FFmpeg's: `RenderPlan::pose`, and the compositor's own guard on `look.placed`; `a_blended_clip_whose_zoom_or_position_moves_is_ffmpegs`), a mode the probe refused, and a layer below full opacity on a build whose probe did not find `TRANSLUCENT` (next). (An odd frame is refused by `composite_texts` for every plan; it is not a blend cause. A translucent layer of odd size, a turn of one and a key under an opacity below 1 keep the reasons they have for every layer, blended or not; a mask under an opacity below 1 is drawn unblended and refused blended, `MaskWith` "on a blended layer below full opacity", the blend chain's probe carrying the opaque plane of a constant opacity alone.)
**A layer below full opacity or resized is drawn, and its two old causes are gone** (`BlendExtras::TRANSLUCENT`; both were "no case holds it", never a measured difference, and measured they are exact):
* *Below full opacity.* The clip's chain is `format=yuva420p,colorchannelmixer=aa=…` (the RGB round trip, `roundtrip.rs`) and only then the blend's `[layer]split`, so FFmpeg converts the one `argb` picture to `yuva420p` twice, once per branch (`lutyuv=a=255` for the clip's colour, the placement `overlay` and `alphaextract` for its alpha plane). The compositor draws the round trip's planes (`translucent_planes`) like any clip's (`clipval`, the plain layer's pass) and lays the mix by the alpha `ffmpeg_alpha` (`lrint(255 * op)` scaled by 256/255). **The clip's fades are `fade alpha=1` steps on that RGBA alpha, each rounding on its own, before the conversion's scale**: the blend pass applied them after it (`ramps`), a level off on every frame of a fade (254,489 Y and 139,569 UV samples of `opacity 0.6 faded` on 6.1.1), so `LayerFades::of` folds the ramps into the alpha with `kerf_core::layer_geometry::ffmpeg_alpha_ramps` for a layer that takes the round trip (the plain dissolve's single `ramp` was already). **Does this FFmpeg convert the pictures that way?** `blend::measure` runs the export's own text (`translucent_graph`: `[1:v]…format=yuva420p,colorchannelmixer=aa=0.6[clip]` into `blend_probe_graph`, the real `overlay` placing it off the frame's right edge) on noise in **all three matrices** (untagged, BT.709 and BT.2020), **plain and with the clip's fades** (`fade=t=in:st=0:d=0.12:alpha=1,fade=t=out:st=0.04:d=0.12:alpha=1` from `kerf_core::fade_filter`, between the mixer and the `split` as the export has them: three frames written, the third judged, partway through both fades so the alpha takes two roundings of its own, `faded_alpha()` = the plan's `ffmpeg_alpha_ramps`), and holds the planes to `roundtrip_probe::model_with_alpha` laid like any clip and blended by `reference`. The way back to 4:2:0 is the one the compositor draws with (`way_back`: `PlanCanvas::matrix`, so BT.601 under `FixedBt601`, the stack's matrix under `BottomLayerTag`, either only where the policy could not be told and only BT.601 stacks are drawn). `TRANSLUCENT` is in `caps.blend_extras` when every one of the six matched, the BT.2020 pair counting only where `BT2020` is (a build whose BT.2020 translucent chain differs takes the one extra out for every stack: one flag, the refusal on the safe side), and every mode did. Six children more than before (about 44 of them now, still six at a time under the one deadline): the whole probe takes 0.2 s longer on 9.0.2 and 0.6 s on 6.1.1 (1.8 -> 2.4 s on a loaded four-core machine, with the composite policy's own probe included on the first call). A build whose auto-inserted conversion takes another route to the branches, or another kernel, is FFmpeg's (the probe has teeth, `checker_bl_probe.rs`: with `-cpuflags 0` behind `KERF_FFMPEG`, no SIMD kernels, all nine modes and the placement still match and `TRANSLUCENT` alone is out; so it is with another alpha rounding, with a fade a step slower than the model's (the plain chains match) and with another rounding in BT.2020 alone (BT.2020 stays drawn)) (`BlendMode`, "on a layer below full opacity (this FFmpeg's blend chain is not measured…)"), and the compositor's guard says the same (pinned caps carry every extra); the round trip's own probe (`TranslucentApproximate`) still decides whether any translucent layer is drawn.
* *Resized.* The layer is the fit pass's picture (the scaler is swscale's to the sample where `scaler::kernels()` measured it, `ScalerApproximate` for every layer otherwise) and the blend chain begins after it: nothing in the blend reads the scale. The chain's `lutyuv=a=255` and the alpha are the plain layer's.
**The ledger's T9** (1920 frames; `blend_mode/on a layer below full opacity` and `blend_mode/on a layer the compositor's scaler resizes` were the only causes in every one, the mode itself in none) **is drawn, still and motion: 960 + 960 of 960 + 960 on both builds** (6.1.1 45,556 -> 47,476 and 9.0.2 41,986 -> 43,906 of 69,666 frames, 65.4% -> 68.1% and 60.3% -> 63.0%, the 9.0.2 figures being the ones with the scaled cascade of the matrix conversion already in; nothing else moved, `blend_mode` leaves (b); with the floating SVG titles' lift (#278: T19, +964 on each build) and the named scalers' (#282: T23c, +960) on the same tree the ledger is **49,400 (70.9%) on 6.1.1 and 45,830 (65.8%) on 9.0.2**, the three lifts adding to the frame (45,556 + 964 + 960 + 1,920 and 41,986 + 964 + 960 + 1,920): T9's rows are 960 + 960 either way, only T19's and T23c's moved, and no archetype carries both a blend mode and a floating title, so the ledger cannot show an interaction -- `checker_bl_stills_a_floating_svg_title_over_a_translucent_or_resized_blended_clip` and its export twin hold the pair, a title laid over the composite a translucent or resized blend made). Parity: `motion_parity` (`blend/...`: every mode over the bars and the test pattern both ways, over noise both ways, fades, keyed opacity, masks, a key, a dissolve, moved / off the frame / odd positions, over nothing,
two blended clips, a late start under a plain clip, BT.709 / `smpte170m` / `bt470bg` footage; **all 0 differing samples in every plane**) and `parity` (the stills: 0 differing samples in the planes, the PNG's RGB and the display's). **Translucent and resized** (`blend-cov/...`, `bc_run`: every frame drawn by the plan and the compositor, no tolerance; **in motion 427 cases, 6,734 frames, 2.34 billion samples on 6.1.1 (420, 6,629 and 2.31 billion on 9.0.2, where a full-range picture's whole cut is FFmpeg's), and 196 stills (414 million samples: planes, PNG and display) plus 30 stills at the preview's widths (planes), 0 differing on both**): `every_blend_mode_on_a_translucent_or_resized_layer_is_drawn_as_the_chain_draws_it` (9 modes x opacity / scaled / both / keyed opacity / keyed and scaled, over the bars, over noise, under noise), `the_alpha_of_a_translucent_blended_clip_is_the_chains_...` (opacities 0 to 0.999, the `lrint` tie, the clip's fades, every transition into and out of a translucent and a translucent resized clip), `a_translucent_or_resized_blended_clip_among_the_other_layers_...`, `..._with_what_precedes_the_blend` (`eq`, grayscale / invert / blur / sharpen, BT.709 and `smpte170m` footage, odd positions and odd sizes, quarter turns, a Cover fit that crops, a mask, a key), `..._on_canvases_of_every_shape` (160x90 to 1280x720, 322x182, 642x362, portrait, square), `a_blended_clip_of_another_picture_or_rate_...` (10-bit, 4:4:4, 4:2:2, gray, RGB, a 25 fps source, double speed, 29.97; the same resized are the scaler's format refusal), `..._over_bt2020_footage_...` and `what_a_translucent_or_resized_blended_clip_is_still_refused_for_is_refused`; in `parity.rs` the same in stills (`blend_modes_on_a_translucent_or_resized_layer_...`, `..._with_what_precedes_the_blend` with a BT.2020 stack, `..._on_canvases_of_every_shape`, and `..._at_the_previews_width`: 1920x1080 footage shrunk to 960, 540 and 320 px across, the still's `yuv444p` planes against the compositor's, 30 stills).

**A blended clip whose matrix is not the composite's** (FFmpeg 9; the translucent / resized blends meeting `matrix_convert_scaled`; `checker_bl_stills_a_blended_clip_of_another_matrix_...` in `parity.rs`, `checker_bl_a_blended_clip_of_another_matrix_...` in `motion_parity.rs`: eight pairs of 601, 709 and 2020, a 960x540 picture the fit shrinks by itself, noise, and per pair a plain layer, a zoom of 0.5 and 0.9, an opacity of 0.6, both, a keyed opacity and a dissolve in, with the mode rotating over the nine). Exact through one path or refused by the other, never drawn off: **in a still the plan draws a plain, a shrunk and a dissolving blended clip of another matrix through the cascade** (the dissolving one is on the alpha plane, so its fit `scale` converts; the blend then works on the cascade's picture; 40 of 72 instants on 9.0.2, 0 differing samples in the planes, the PNG and the display) **and refuses the translucent ones** (`convert_refusal`: "a rotation or an opacity below 1", the alpha plane the conversion also reads; the compositor's own guard says the same, so no instant passes the plan and fails the render). **In an export on 9.0.2 every such frame is FFmpeg's** (the cut is mixed and holds a blend, whose deciding clip the plan does not read: `PlanCanvas::unread_decider_in_cut` makes it `MixedMatrices`; 0 of 3,120 frames drawn, every refusal names the matrices), so the cascade and the translucent round trip never meet in a frame. On 6.1.1 the matrices are ignored (`FixedBt601`) and all 72 stills and 3,120 frames are drawn and exact. Lifting the export's refusal needs the deciding clip read for a blend first; these cases then hold what it draws.

**A long run of blended clips in an export (fixed by the blend fold, `engine-cli.md`).** `checker_bl_how_many_consecutive_blended_clips_an_export_holds` (`motion_parity.rs`) renders a cut of consecutive blended clips through the export and draws nothing. On FFmpeg 9.0.2 an export of eleven or more of them used to deliver no frame (nine rendered; the alpha sweep's fourteen gave none in 300 s, with one filter thread as with eight, opaque as translucent; 6.1.1 rendered in seconds): the nested forks of the graph. Runs of seven or more are folded into one fork now, and the case, run with `KERF_BL_N=14` on the pinned n9.0.2-48, renders the fourteen (28 frames) in 11 s translucent and 5 s opaque. The comments in the case and in the alpha sweep's test still describe the old limit; the sweep's clip count is no longer bounded by it.

**The clip's grade** (wheels = `colorbalance`, curves = `lutrgb`, a 3D LUT = `lut3d`; `grade.rs`, `shaders/grade.wgsl`, `compositor/grade_passes.rs`,
`GpuCaps::grade`; not the `eq` colour above, which is a YUV operation). The grade's filters only take RGB, so FFmpeg converts the picture `yuv420p -> rgb24`, runs
`lut3d`, `lutrgb`, `colorbalance` (that order) and converts it back. The compositor draws **the conversions as well as the filters, to the level**: a graded layer at its
own size is the export's, sample for sample (0 differing samples in `tests/grade.rs` and in 106 of the 108 `grade/` rows of `motion_parity` on 6.1.1 (104 of 106 on 9.0.2, which does not run the two cuts that are 6.1's alone, a graded clip before a BT.709 one), the frames the plan draws; the other two, a rotated layer and a translucent one, were at their own ladder's level then and are exact now; the Still-plan `grade/` rows are exact too). Every stage was written from the FFmpeg 6.1.1 and 9.0.2 sources, first as a numpy model, then as the scalar
`grade::reference`, and held to both builds on noise in all three planes (every Y, U, V triple, clipped RGB included; `tests/grade.rs`: the reference vs FFmpeg's filter
text vs the shaders, 21 grades (every part alone, in pairs, all three, untagged and BT.709, a 2^3 and a 33^3 LUT, a zigzag curve, wheels at the rim of the disc) on six pictures with odd chroma sizes and a 2x2, and three of them on a 1080p frame):
* **In is swscale's x86 kernel, not the C tables of the translucent round trip** (`roundtrip.rs`'s `Yuv2Rgb` is `yuva420p -> argb`, which has no SIMD version). `yuv420p ->
  rgb24` is `ff_yuv_420_rgb24_ssse3`: 16-bit lanes, `pmulhw` by 13-bit coefficients (`ff_yuv2rgb_c_init_tables`' `roundToInt16(c * 8192)`: BT.601 y 9539, vr 13075, ub 16525,
  vg -6660, ug -3209, offset 128), chroma replicated 2x2 (`X86Kernel`, `fs_rgb`). The same on both FFmpegs. Odd heights leave it (swscale's generic path) and odd widths read past
  the line, so the plan refuses odd sides.
* **Wheels** are `color_balance8`'s `float` arithmetic op for op (`Balance`, `fs_balance`); **curves** are the `lutrgb` table (`curve_tables`, the model's value rounded: the
  engine's expression equals it for every value); the **LUT** is `interp_8_tetrahedral` (`lut3d_apply`, `fs_lut3d`): the per-byte axes (`prev`, `next`, fraction) are made in
  `f32` on the CPU (`LutAxes`), the table is `kerf_core::baked_lut` (the copy's rows with the intensity applied) uploaded as an `Rgba32Float` 3D texture (four cached), the
  four products are summed left to right and the result **truncated** (`av_clip_uint8(sum * 255.0f)`: `(int)`, not `lrintf`). A GPU may fuse `a * b + c` into one rounding, a CPU's
  FFmpeg does two, so every product a sum follows goes through `pin` (an OR with a uniform that is zero, which no compiler sees through) and every `/ 255.0f` is a table made on the CPU.
* **Out, untagged**: `rgb24 -> yuv420p` by the generic scaler. Luma is `rgb24ToY_c`; chroma is `rgb24ToUV_half_c` (sums of horizontal pairs) and then the vertical filter, which is
  **swscale's x86 `yuv2yuvX`** (`pmulhw` of each tap, `(coef * src) >> 16`, start `(64 + 8 * (N - 1)) >> 4` with N aligned to 2, then `>> 3`) **on every chroma row but the last**,
  which `swscale.c` hands to the C `yuv2planeX` (`dstY >= dstH - 2`, "we can't use MMX here without overwriting this array's tail"). The shader's scaler (`sws.rs`) is the C
  arithmetic; the two differ in one sample in fourteen by a level (`ResampleParams.x86`, `fs_v`'s branch, `fs_chroma_v_x86`).
* **Out, tagged** (the stream names a matrix, or the graph labels it BT.709): the engine ends the grade with `scale=out_color_matrix=<m>,format=yuv444p,setparams=colorspace=<m>`
  (6.1 converts back as BT.601 otherwise), so the picture goes `rgb24 -> yuv444p` (`fs_plane444` x3) and the 4:2:0 conversion that follows is a 2:1 horizontal bicubic plus the same
  vertical rule (`PlaneFilters.x86_start`). The matrix is `PlanLayer::grade_route()`: BT.601 / untagged and BT.709 are drawn, any other tag is refused.
* **Does this FFmpeg convert like that?** The kernels are SIMD, and another CPU's FFmpeg (NEON, plain C) agrees to a level or two, not to the level; the RGB format
  FFmpeg negotiates is a property of the build **and of which filters the chain holds** (the intersection of what they take). `grade::kernels()` runs a 48x32 picture of
  integer noise in all three planes (made by `geq` from the coordinates) and then **every non-empty set of the grade's parts (the LUT, the curves, the wheels: seven) through both
  routes (untagged, BT.709-tagged), each as its own graph with the filters `kerf_core::grade_filter_chain` writes for it** (the very text the export uses: a LUT's copy, `lutrgb`
  expressions, `colorbalance` options, the `scale` / `format` / `setparams` of the tagged way out) and compares each result with `grade::reference` of the same grade: `Match`,
  `Differs(the chain)` (named: "a LUT and wheels on a BT.709-tagged picture") or `Unknown`. That is fifteen short children at once, thread-capped
  (`limit_frame_args`: the budget's threads shared among them, at any budget; **not niced**: a frame waits on the probe, and a probe that loses its slice to a busy machine times out into `Unknown`), a
  `-loglevel error` each, bounded at 20 s, about 0.3 s on 9.0.2 and 0.6 s on 6.1.1 on a loaded 4-core box (the first probe, an identity `lutrgb` in three graphs, was 0.1 s and tested the conversions only: a build that
  picks planar `gbrp` for `colorbalance` beside `lut3d` would not have shown). **One graph per chain, and not `random()`**: the first probe stacked the routes in one graph and was wrong on 9.0.2 only (FFmpeg 9 negotiates the matrix across a
  whole graph, so the BT.709-tagged branch made the untagged one convert as BT.709, and the GPU refused every graded layer there until the parity run showed it), and `random()` restarts its sequence for every slice thread.
  The cache and backoff are `cached(state, now, measure)` (like `geq_edge`'s; unit-tested: an `Unknown` is never remembered, a measured answer is kept for good, the wait doubles from 5 s to 5 min, two failures at once are one, the first answer stored wins and the probe runs with no lock held).
  **Anything but `Match` takes the grade out of the caps**: `Compositor::caps()` is `CAPS` less `grade` unless the pin (`with_grade_kernels`) or `grade::measured()` (what is known without
  running anything: `Unknown` before the first probe, during a backoff and while held back) says `Match`, so the plan refuses a graded layer (`Unsupported::Grade`) before anything is decoded for it, and playback / priming
  (`cut.ahead`) hand a cut with one to the stream. Never-measured counts as not known: `prepare()` (the still path, off the render lock) and the render bar (a cut with a grade asks, on its blocking thread) measure ahead of it, and so does
  the self-test, which every GPU frame waits for. `Compositor::caps_for(kernels, scaler, convert)` is the same without a device (the render bar's; `caps_with` adds the text layout, the blend modes and their extras, `caps_pinned` the whole set but the placement, with the measured `geq` edge). `grade_look` keeps its own check (`Unsupported` for a caller that skipped the plan), and
  `Compositor::CAPS` stays the build's "assuming they match" (coverage, benches). The probe picture is held to tell the x86 vertical kernel from the C one (`the_probe_picture_tells_the_x86_vertical_kernel_from_the_c_one`); the 14 chains are held to be 14 different answers.
  **The grade is measured on x86-64 only**; elsewhere the probe decides.
* **Refused** (each `Unsupported::GradeWith` / `Matte` / `ZoomBehind` in the plan, a held case in `parity.rs` and `motion_parity.rs` with FFmpeg rendering the fallback; `core-render-plan.md`
  has the list): a layer the scaler resizes **with no `eq` between the fit `scale` and the grade** (the filters between pass any format along and the grade's RGB-only filters ask for RGB, so the negotiation has the fit `scale` output RGB itself, scaling and converting in one pass from chroma the compositor has already scaled; with an `eq`, which takes planar YCbCr only, the conversion is its own `scale` and the exact scaler's picture is what it reads: drawn, and the same on a machine whose scaler is not measured exact is refused as the scaler's level times the conversions, 3 levels of luma and 2 of chroma), an odd picture side (a letterbox is drawn: "A letterbox's padded frame"), any picture but 8-bit limited-range `yuv420p` (FFmpeg grades the others in the format it
  negotiates), a BT.2020 / `fcc` / `smpte240m` tag, a video effect other than a chroma key after the grade, a fade to black or white or a dip to white running on it,
  **a cut that mixes colour matrices under a negotiating policy** (see below), a part the caps do not name. Drawn **with** a grade: a moving zoom or position (the placement reads the 4:2:0 planes the grade hands on; all twelve grades are exact behind it on 6.1.1 and 9.0.2, in an export frame and in a still), a crop that is the canvas, a position on a canvas the picture fits, an opacity (the alpha plane), a mask, a rotation,
  a dissolve (its alpha ramp; **a clip's own `fade_in` / `fade_out` is not one**: it is `fade` to black of the picture on whatever track, so it is refused where it runs, on a layer over another track as on the bottom one), a chroma key, an `eq` ahead of it (static and keyed), speed,
  a keyed opacity, a keyed position, and the layers under it.
* **A cut that mixes colour matrices** (`PlanCanvas::mixed_matrices_cut`, Motion plans only: the cut's video inputs are not all one `PlanStream::matrix()`, the graph's BT.709 label included; an RGB picture is its own business): under `BottomLayerTag` FFmpeg 9 converts the picture the grade
  leaves into the bottom layer's matrix **inside the graph, on every frame of the clip**, not only where the two are on screen together (an untagged 640x360 clip graded after a BT.709 one: up to 24 levels of Y and 22 of UV off the model on every frame of the clip, which the per-frame stack check
  (`MixedMatrices`) saw none of), so `GradeWith("in a cut that mixes colour matrices")` refuses the graded clip in every frame of such a cut and FFmpeg draws it. Under `FixedBt601` each layer is taken as it is and the cut is drawn to the level in either order. One matrix is not a mix:
  untagged beside untagged, beside a `smpte170m` tag (BT.601's coefficients), BT.709 beside BT.709 and untagged HD beside BT.709 (labelled) are exact on both builds in either order. A Still plan's graph is the layers on screen, which `MixedMatrices` already judges.
  **Not the grade's alone, and fixed for the ungraded layer**: the same cut *without* a grade was wrong in the same frames on 9.0.2 (an untagged 640x360 clip after a BT.709 one: up to 31 levels of Y and 18 of UV off on every frame of the clip, because a Motion plan's `composite_matrix` was per frame); the composite's matrix is the cut's now (`PlanCanvas::cut_matrix`) and the ungraded layer is converted into it (the paragraph above). A *graded* layer stays refused in a mixed cut: its way back is a different conversion into the bottom layer's matrix inside the graph (the grade's generic scaler path), not reproduced.
* **Cost** (`bench_a_grade`, 1080p, lavapipe, a loaded 4-core box; `target/parity/bench-grade.txt`): a still composite (ms, min of runs) is 49 for a plain cut, 189 with wheels, 152 with
  curves, 165 with a 33-point LUT and 234 with all three; the FFmpeg still is 202 / 229 / 243 / 265 / 316. An export frame (decode + composite) is 53 / 160 / 155 / 191 / 244
  against FFmpeg's 45 / 75 / 58 / 72 / 96 ms: on a software adapter the grade is a handful of full-size render passes (to RGB, the LUT, the curves, the wheels, luma, chroma), and
  FFmpeg's graded export is faster there. A real GPU is unmeasured.
* **Not drawn yet, and why** (the next steps): a scaled graded layer with no `eq` needs the fit scale and the conversion to RGB as one pass (swscale scales chroma from its own
  samples there); an effect or a fade after a grade needs the RGB / 4:4:4 picture the filter negotiates; a dip to
  white and a translucent layer are exact in the plan's other places and would gain from the same kernels. The 10-bit, 4:2:2 and 4:4:4 pictures need their own conversion kernels.

**Layers of another matrix than the bottom's** (`convert.rs`, `GpuCaps::matrix_convert`, `RenderPlan::convert_refusal`; FFmpeg 9, `BottomLayerTag` only). **A compositor whose conversion is not `Match` draws no frame of a mixed-matrix cut** (`Unsupported::MixedMatrices` on every frame, the deciding clip's own included: its matrix is the composite's so nothing converts it, but *which* clip decides is the planner's reading of the graph, and the probe checks that reading in the same pass as the conversion, so a machine that fails either has checked neither). **Which clip decides the composite's matrix**, for the whole cut (`PlanCanvas::cut_matrix`), is read from FFmpeg 9's format negotiation (`avfiltergraph.c`) and held by real files: the colour-space lists of an `overlay`'s three pads are one list, a `scale` has two (in and out), and each list is reduced to the single value its filter's input holds, filters in the order the graph was written, with the converters it inserts after every one of them. A clip whose chain already ends in `yuva420p` (what `overlay`'s second input takes), with nothing in it forcing a converter, needs none before the overlay: its post-`scale` pads are tied to the composite's list and its fit `scale` makes the composite's matrix its own tag. Every other clip gets a converter `scale` (the pixel-format conversion to `yuva420p`, which also carries the matrix) whose input is its tag and whose output is the composite's, inserted when the graph's links are visited. So the composite takes the tag of **the first such clip on the alpha plane**, else of the first clip (a converter each), the order being the graph's chains: **a track's clips in start-time order** (`cli.rs`'s own sort, not storage order), tracks in list order; and every clip tagged otherwise is converted into it once, wherever it is on the timeline: a lone clip of the other matrix on screen is converted all the same, which the per-frame `composite_matrix` did not know. What puts a chain on the plane, with nothing in it that may carry a converter (`Ends::Clean`, the clips that decide), is a dissolve, a keyed zoom or position (`placement`) and a source with an alpha channel: held by `tests/checker_b_filters.rs` (`deciding_clip_on_an_alpha_plane`, a keyed zoom held at one value being a plain chain). A clip on the plane that holds an opacity below 1 (an RGB round trip), a rotation, a mask or a chroma key (not measured), a colour correction (`eq` takes no alpha), a grade, an effect, a tone map, a reprojection, a dip to white on either side of it (`fade c=white` runs on RGB: an alpha-source or dissolving clip that dips, as the outgoing clip of a `DipToWhite` or as the incoming one after two plain clips, was not the decider: every sample moved), or (placed) a black fade, may carry a converter in its chain and is not read (`Ends::Unread`): nor is a blend mode (`blend`, in RGB): a cut of mixed matrices holding one is `MixedMatrices` in every frame (`PlanCanvas::unread_decider_in_cut`). An RGB picture is taken into YCbCr with the **cut's** matrix, so a frame of it alone in a cut that holds a BT.709 or BT.2020 clip is refused, not judged by the stack on screen (`PlanCanvas::cut_non_601`, `RgbInStack`). Read from a build that behaves so (three clips in every combination of plain and dissolving over BT.601 / BT.709, and `tests/checker_b_filters.rs`'s deciding-clip, storage-order and fuzz cases: 0 differing samples in every drawn frame).

The conversion itself: swscale has no YCbCr-to-YCbCr matrix change and goes **through RGB** in two contexts: `yuv420p -> bgr24` with the source's coefficients (the x86 SSSE3 kernel of the grade's way in, `X86Kernel`) and `bgr24 -> yuv420p` with the destination's by **`ff_rgb24toyv12`**, the converter an unscaled even-width `bgr24 -> yuv420p` has of its own (`bgr24ToYv12Wrapper`; not the generic scaler the grade's way out is): `Y = ((ry*r + gy*g + by*b) >> 15) + 16`, chroma from the 2x2 block's channel means (`>> 2`) the same way plus 128, the C function on every row on the pinned 9.0.2 and **an x86 build with the inline MMX one would run other arithmetic between the first two and last two rows** (`Variant::Mmx`, modelled, not drawn: the probe says `Differs` and the plan refuses). Measured: 0 differing samples on noise and on the probe's polynomial in all three planes, every pair of BT.601, BT.709, BT.2020, both orders; the passes equal the scalar reference for every pair at seven sizes (`tests/convert.rs`). **Where in the chain**: in an export a plain chain ends in `format=yuv420p` and the conversion is a converter `scale` after it: **scaled, then converted** (0 of 10240 luma samples differ in that order, 9931 the other way). **A still's graph has no `format`**: the chain's last `scale` converts the matrix itself, in one pass with its resize (47398 of 230400 luma samples differ on a half-size picture-in-picture when it is converted after the resize), so in a still a layer whose last scale resizes it is swscale's *cascade* (below: drawn for a picture that shrinks, refused for the rest); one the chain does not resize is exact (the identity `scale` converts unscaled), a dissolving one included. **A clip on the plane that is to be converted** (it is not the decider) has its fit `scale` convert the matrix itself, in one pass with its resize, as in a still: the phone clips (540x960 into 960x540) converted after their resize are 22 % of the luma off, up to 17 levels, and match neither convert-before-scale nor convert-after-chain; it is the cascade too. The decider is not converted at all; the plain clips of the cut are converted exactly (the first clip of a vlog among them).

What is converted: an opaque 8-bit limited-range `yuv420p` video picture (an export's 4:2:2 and 4:4:4 too: they reach the conversion as the 4:2:0 the decode makes, as a `format=yuv420p` ahead of the converter does; a still's `scale` converts them and the matrix in one pass: refused there) of even size (and bars that meet it on the 4:2:0 grid: they are black in every matrix; over a blurred fill a plain layer's whole filled frame is converted after the overlay; a dissolving or still layer's fill converts inside its own up-scale, below), a crop, a scale, a pad, a position, speed, invert and grayscale (tables the export's scale applies ahead of the converter; refused in a still, whose composite is not the conversion of the effect's picture: 470555 samples differ; not on bars), a dissolve, a slide or a push, and a fade to black on a plain chain of an export (FFmpeg fades the picture ahead of its converter `scale`: the compositor fades the planes, then converts them, `motion_parity.rs` `convert/fade-in and out on the converted clip` and on a letterboxed phone clip: 0 differing samples); refused (`Unsupported::MixedMatricesWith`, a detail each, each a held case in `parity.rs` / `motion_parity.rs`): a grade, any other effect, a mask, a rotation or an opacity below 1, a colour correction, a keyed number, an HDR / reframed / 360 picture, a dip to white, a fade to black running in a still or on the alpha plane (a dissolving clip's own fade sits behind its ramp: not measured), 10-bit, gray or yuvj or a still image, a grade anywhere in a mixed stack, an RGB picture in a stack or cut that is not BT.601 (`RgbInStack`), and a cut whose deciding clip is no YCbCr picture of a known matrix or is not read (`MixedMatrices`, every frame). **The cascade of a scale that resizes as it converts** (`convert::scaled_to_rgb24` / `scale_and_change_matrix`, `Compositor::scale_convert_planes` and `scaled_convert.wgsl`, `GpuCaps::matrix_convert_scaled`, `convert::scaled()`; `ScaleStage::cascade_refusal` and `RenderPlan::scaled_convert_refusal` judge it in the plan). FFmpeg 9's `scale` hands swscale different source and destination matrices (`graph.c`, `add_legacy_sws_pass`: `sws_setColorspaceDetails(src->csp, dst->csp)`), and `sws_setColorspaceDetails` (`utils.c`: "YUV color matrix differs for YUV->YUV, using intermediate RGB") builds two contexts around a `bgr24` picture of **the smaller of the two sizes** (`srcW*srcH > dstW*dstH` ? destination : source). For a picture that **shrinks** that is the destination's: context 0 is `yuv420p W x H -> bgr24 w x h`, *scaled*, with the source's coefficients, and context 1 is the unscaled `bgr24 -> yuv(a)420p` of the paragraph above (`ff_rgb24toyv12`, the destination's coefficients; an `yuva420p` destination has its alpha plane filled with 255, nothing else differs). Context 0 is swscale's scaler with an RGB destination and no `SWS_FULL_CHR_H_INT`: the luma `W -> w` and the chroma `W/2 -> w/2` (one chroma sample serves two pixels) horizontally into the 15-bit intermediate (bicubic, the x86 tables with filter sizes aligned to 4, as the plane scaler's), the luma `H -> h` and **the chroma `H/2 -> h`** vertically (an RGB destination has a chroma row for every row; sizes aligned to 2); the positions are centred on both sides (`get_local_pos` gives 128 whatever the subsampling). The output function is `yuv2packedX` -- the general one, whatever the chroma filter's size, as long as the luma filter has more than two taps (one or two go to `yuv2packed1` / `yuv2packed2`: refused) -- and on x86 it is the inline MMX `yuv2bgr24_X` (`YSCALEYUV2RGBX`: `pmulhw` of each 12-bit weight and 15-bit sample, summed from `vRounder` 4 on 16-bit lanes, then `(Y8 - yOffset) * yCoeff >> 16` and the chroma terms by the grade's `X86Kernel` constants) for every row but the last two, and the C `yuv2rgb_X_c_template` (19-bit sums, `Yuv2Rgb`'s tables) for those, which `swscale.c` re-initialises to C at `dstY >= dstH - 2` (`Packed::Mmx`; `Packed::C` is the model of a build without the inline MMX, not drawn). Measured on 9.0.2: 0 differing samples of 6 452 568 compared against a bare `scale` naming both matrices (`this_ffmpeg_scales_and_converts_a_picture_as_the_model_does`: 14 sizes from 128x96 to 1080x1920, three pairs of matrices, `yuv420p` and `yuva420p` destinations, noise, at 1, 4 and 16 filter threads: swscale's own slices change nothing) and the passes equal the reference at every size and pair (`tests/convert.rs`); end to end in `parity.rs` (a still: picture-in-picture, shrink, letterbox, HD pairs) and `motion_parity.rs` (the vlog chains of dissolving clips). **Refused** (`MixedMatricesWith`, a detail each): a picture that does not shrink (the same area or more: the intermediate is the *source's* size and the scaler is in context 1, from `bgr24`, not modelled), a Cover fit (the result is cropped), an odd side, luma rows that are not scaled, a picture that is not 4:2:0 (the format is converted in the same call), and any caps without `matrix_convert_scaled`. **The blurred fill behind such a layer** (T5b; `convert::enlarge_and_change_matrix`, `Compositor::enlarge_convert_planes`, `fill_convert.wgsl`, `GpuCaps::matrix_convert_fill`, `RenderPlan::fill_cascade_refusal`, `BlurFill::enlarge_refusal`): FFmpeg 9 ties the fill's scales to the composite's colour space as the fit's. `-v debug` on the export's graph: the thumbnail `scale` converts nothing (the crop and the blur follow in the picture's own matrix); the up-scale `scale=W:H:flags=bilinear` converts as it enlarges 8:1 (`yuv420p bt709 120x66 -> yuva420p smpte170m 960x540`); the fill's `overlay` is `yuva420p` over `yuva420p` at alpha 255, a copy of the picture. A plain export chain's converter sits after the overlay, so its whole filled frame is converted, as it was; a still's layer is converted whether it dissolves or not. For an enlargement the `bgr24` intermediate is the **source's** (the thumbnail's) size: `yuv420p -> bgr24` unscaled (the x86 kernel of the way in), then `bgr24 -> yuva420p` scaled to the frame by the generic scaler: 14-bit luma and **one chroma sample per pixel** (`bgr24ToY_c`, `bgr24ToUV_c`; an enlargement never pairs them), `hScale16To15` (`>> 13`, bilinear tables aligned to 4), the x86 planar vertical pass with chroma rows `h -> H/2`. Scaling up and then converting is not it (a noise thumbnail 120x66 to 960x540: 492,905 of 518,400 luma samples differ, by up to 65 levels), so the fill has passes of its own. A fit that does not resize the picture converts it unscaled (`convert_matrix`) and lays it on the same fill; an odd picture side there is refused. Drawn for an even frame the thumbnail is smaller than on both axes, where the probe found a match. **Held**, 0 differing: the model against FFmpeg's bare `scale` on 6.1.1 and 9.0.2 (14 frames from 32x18 to 3840x2160, 3 pairs of matrices, 1 / 4 / 16 threads, 184,353,408 samples); the passes against the model (42,943,872); the kept stills and export of `parity.rs` and `motion_parity.rs` (an unresized fit among them); the local sweeps `checker_bf_*` (stills: 72 drawn on 9.0.2 and 18 refused for an odd picture, 90 of 90 on 6.1.1; exports: every trio of matrices, frames to 1080p, speeds, own fades refused while they run) and `checker_bf2_*`. A fill up-scaled and not converted is 405,132 samples off (6 levels) at the dissolve's middle. A mutation of a constant of `fill_convert.wgsl` or of the model's tables fails at the first picture (the C rows of a bilinear enlargement are unobservable). **The ledger**: T5b's 150 stills and 1,024 export frames were the single cause `mixed_matrices_with/a blurred fill behind a layer that dissolves`: **9.0.2 49,158 -> 50,332 of 71,586 (68.7% -> 70.3%)**, T5b 100% in both modes; 6.1.1 unchanged (FFmpeg 6 converts nothing). No archetype has a fit that converts without resizing under a fill, so lifting that refusal moves nothing.
**The probe** (`convert::scaled()`, run by `convert::kernels()` once the conversion matched, so every `prepare` / `prepare_timeline` that asks for the conversion measures it, off the render lock, on a blocking thread; cached, backed off) runs the export's own chains over a decider and a clip whose fit `scale` resizes and converts it: `decider_graph`'s `fit,fps=1,format=yuva420p` (a dissolving clip's) and the same without `fps` and `format` (a still's), a square and a tall picture into a 64x48 canvas, BT.601 into BT.709, BT.709 into BT.601 and BT.2020 into BT.709, from tagged files, **and the fill's**: `kerf_core::blur_fill_chain` (the export's own text) with `setsar=1` and, dissolving, `fps=1,format=yuva420p`, a 90x180 picture over a 128x96 canvas (a 16x12 thumbnail, an 8:1 up-scale), both directions of 601 and 709, dissolving and still, held to the model of the whole fill (`filled_frame`). A fill that does not match leaves the verdict `Scaled::Shrinks` (layers that shrink are still drawn; the fill behind a converted layer is FFmpeg's). **Its cost** is 25 `ffmpeg` children on top of the conversion's own: ten tagged files made at once, five reference pictures made at once, then the ten graphs at once (each bounded by 20 s, so **three sequential phases, 60 s at the worst**, after the conversion probe's own three; about 0.2 s on a quiet machine), each given a `limit_frame_args` share equal to the children running beside it, and a reference picture that comes back short (a child that exits 0 with less than a frame) is `Unknown`, retried after the backoff, never a `Differs`. A result that is not the model's is `Differs` (naming whether the C output functions on every row would have matched: a build without the inline MMX), a probe that cannot run is `Unknown` and retried after the backoff; neither draws the layer, and the conversion not matching makes it `Differs` without a run.

The probe (`convert::kernels`, like `grade::kernels`: three rounds, of 3, 2 and 1 children, once per process, cached, backoff) runs the real overlay graph for BT.709 under BT.601 and BT.2020 under BT.709 (the cheap round, three children at once; a machine that converts another way is `Differs` there, for good, without the files below) and, only where those match, **the three-clip graph the planner's decider reads** (two tagged files made first, so a failure past this point is `Unknown`, asked again after the backoff) (a plain BT.709 clip, a clip whose chain ends in `yuva420p` with no tag, a plain BT.709 clip, side by side: the composite takes the alpha-plane clip's tag, so the plain pair is converted into BT.601 and the middle one is untouched; a build that negotiates otherwise is `Differs`, and every mixed-matrix cut is FFmpeg's), and the caps carry `matrix_convert` only where it matches **and** the grade's kernel does (the way in). `Compositor::convert_yuv` is the passes for the plane-level test (`Compositor::scale_convert_yuv` is the scaled cascade's: a picture, a size and two matrices in, the planes out, held to `convert::scale_and_change_matrix` in `tests/convert.rs` and to FFmpeg's own `scale` in `tests/checker_mx_cascade.rs`).

**`tests/motion_parity.rs`** (`#[ignore]`d; CI's parity job, on both FFmpegs, runs five named cases of it, not the file: see "What CI runs" below) holds the compositor to the **export**, frame by frame: each case builds a short
cut, renders it through `render_with` (the real graph) losslessly, reads **every frame back as `yuv420p` planes** (no colour conversion between: the planes `overlay`
blended) and draws the same frames from a Motion plan through `MotionFrames`, comparing plane for plane with a stated bound per case, **every frame of the range, the last
included** (`target/parity/motion-report.txt`; montages `FFmpeg | GPU | diff x8` of frames worth a look and of any over the bound in `target/parity/motion/`). The sources are lossless
(testsrc2 with a frame counter, colour bars, a 25 fps clip, a still, a portrait one for a letterbox, noise in all three planes, a gradient for the rotated cases) and, for the
format and range cases, `variants()` (10-bit, 4:4:4, 4:2:2, gray, an RGB PNG, MJPEG and a JPEG still, BT.709-tagged). Families: plain cuts and speeds at five rates, clip fades,
dips, dissolves, the eight slides and pushes, keyed position, quarter-turned and translucent layers on an alpha plane (a keyed rotation and any other turn are refused cases), and the coverage archetypes' montage and talking-head
shapes. **Every sample of every frame the plan lets the compositor draw equals the export's, on 9.0.2 and 6.1.1 (max error 0, no case names a bound: `Limits`, `SCALED`, `TRANSLUCENT`, `HD_TRANSLUCENT`, `GRADED_SCALED`, `EFFECT`, `EFFECT_SCALED` and `WHITE` are gone)**: a layer the scaler resizes (on a machine whose scaler is not measured exact the plan refuses it, `ScalerApproximate`), a translucent one (`roundtrip_probe`), a blur (`blur_probe`), a dip to white, a graded layer. **A grade at the picture's size is exact on both** (0 differing samples in the frames drawn of all but two of the 108 `grade/` rows: each part alone and the three together over six pictures, untagged, BT.709 and `smpte170m` / `bt470bg`, a crop, speed 2, an `eq` ahead of it (static and keyed), over another track, dissolving in and out, under a mask and a chroma key, a keyed opacity and a keyed position over another track, beside clips of its own matrix in either order, a labelled HD clip; rotated and translucent, exact too). **A refusal case** (`run_refusing`) skips the frames the plan refuses for its reason, requires at least one, and
holds **every other frame to the export** (a refusal wider than it has to be shows): the letterboxed / odd / resized / not-`yuv420p` dips to white, the fades on a translucent layer,
the full-range pictures (`run_full_range`: drawn exactly under `FixedBt601`, refused under the negotiating policies), and the `eq` tables of any step on a layer the scaler resizes (`run_grade_steep`: a keyed ramp frame by frame, drawn or refused by the plan's caps; `eq_tables_of_any_step_are_the_export_on_a_layer_the_scaler_resizes` holds the static ones to the level on a luma ramp, hard edges and noise). A mutation of the chroma constant by nine in 65536 fails the dips
at once. The Still-plan side is `parity.rs`: `frames_inside_a_transition_are_drawn_as_ffmpegs_still_draws_them` (the plan holds the frame and the GPU is held strictly to the still, the still to
the export), `every_kind_of_transition_and_the_clips_own_fades_are_drawn_in_a_still`, `a_dip_to_white_is_drawn_where_its_round_trip_was_measured_and_refused_elsewhere` and
`a_full_range_picture_fades_as_the_ffmpeg_in_use_converts_its_range` (the `check_refused` cases, with FFmpeg rendering each fallback).
**The range flag's cases** (`a_yuv420p_flagged_full_range_is_drawn_or_refused_as_the_export_reads_its_raw_samples`; the Still side `a_yuv420p_flagged_full_range_is_refused_where_ffmpeg_reads_its_raw_samples`):
a lossless FFV1 `yuv420p` file flagged `-color_range pc` (a codec whose decoder keeps the format `yuv420p`, as VP9 / HEVC / AV1 do; H.264 would call it `yuvj420p`), the same pattern flagged `tv` and unflagged. Motion: plain, black-then-flagged
and flagged-then-limited cuts exact under FFmpeg 6 and refused as the cut (`FullRangeCut`) under 9; the fade refused for the frames it runs in under 6 (`FullRangeNegotiated`; the one frame at full factor is held exact) and as the cut under 9; a shrink and an enlargement refused
(`FullRangeResized`) under both; the vignette, invert, blur, sharpen, `eq` and chroma key refused on every frame under both; the `tv` and unflagged controls exact in all of them. Still: the same refusals with FFmpeg rendering each fallback, the plain, shrunk, enlarged and
after-limited frames strictly drawn, and **the refusal's necessity** (`drawn_anyway`): the very frames planned as an asset stored before v10 would be (flag `None`) and drawn differ from FFmpeg's (vignette 12 / 13, fade 9 / 11, and on FFmpeg 9 the key and `eq`), so a compositor that learns to
read raw samples fails the test and the refusal can go.
**The BT.709 label's case** (`untagged_hd_is_labelled_for_the_whole_graph_that_holds_a_matrix`): a 1280x720 untagged HD clip made translucent (exact: it was held to 2 levels of Y / UV, the scaler's and the round trip's, while both ran the C
kernel) alone in the first second of a cut whose second second holds a BT.709 clip, and over a BT.709 clip, plus the same frames with no BT.709 clip in the graph; the Motion plan labels by the cut's graph where a
Still plan labels by the layers on screen. Reading the labelled layer as BT.601 again fails it on both FFmpegs (9.0.2 refuses the frames as mixed matrices, 6.1.1 draws them 8 levels off).
**The label's dip case** (`a_labelled_hd_clip_dips_through_white_with_the_bt709_round_trip`, exact; the Still side `a_labelled_hd_layer_in_a_dip_to_white_is_drawn_as_the_layer_tagged_in_the_file`):
a labelled HD clip dipping through white is drawn as the file tagged BT.709 is, which is what the export renders because the label is the first filter of the chain. Behind the fit `scale` (the filter FFmpeg has convert the picture
to the `rgb24` the white `fade` needs) it was BT.601 out of YUV and BT.709 back, and the compositor was Y 17 / UV 9 off the export on 9.0.2, Y 30 / UV 16 on 6.1.1, on every frame of the clip.

**GPU-6's cases** (zoom in, out, from small, with a position, eased, three keys, late, speed 2, reversed, Cover into 960x540 and 9:16; masks
shapes x feather x invert, off-centre, larger than the frame, thin, the shape's edge on the picture's border, over noise; the effects and their chains;
a dissolve, dip, slide or push into a zooming clip and out of one) are exact, **on both FFmpegs**: there is no ladder of bounds any more (it had rows at 0, 1 for `SCALED` and `EFFECT`, 2 for a scaled translucent layer, `EFFECT_SCALED`, `CASCADED`, `ZOOMED_MASK`, `GRADED_SCALED` and the dip to white's `WHITE`; every one is 0: the scaler is measured swscale's, the blur is `gblur`'s fused recursion, the translucent round trip runs the x86 vertical kernel, the dip runs the grade's kernels). A plan the compositor cannot draw to the sample is refused, and the case says so (`run_refusing`).

A **sharpen after a scale is refused past a worst-case error of 2 only where the scaler is not swscale's to the sample** (`!caps.exact_scaler`; then `ScalerApproximate` refuses a resized layer anyway on any compositor that measured it, so the rule is A0's): `unsharp` is exact, but it carries a one-level difference upstream through at worst `1 + 2|amount|` levels of luma at an edge sample (measured after a scale on a C-kernel scaler: 3 at 1.5, 7 at 3, 11 at 5), and its `>> 16` floors, so each further sharpen adds at least a level. **Where the scaler is exact, and after a blur (the shader's is `gblur`'s to the sample), a sharpen of any amount, chained, is drawn** (`effect/blur then sharpen 5`, `effect/sharpen 5 on a pip`, `sharpen 0.001 twice`, ... in `motion_parity.rs`; `effect/sharpen-5-after-a-scale`, `effect/blur-then-sharpen-5` in `parity.rs`; both exact). (A grade between a scale and a sharpen amplifies too on a scaler that is not exact: `GradeSteep` above.)

**`tests/checker_z.rs` and `checker_z_text.rs`** (the second independent checker of the zero-tolerance audit; `#[ignore]`d, `cargo test -p kerf-gpu --no-default-features --test checker_z -- --ignored --nocapture`, once per FFmpeg): the translucent round trip, `gblur`, the display conversion and
full-range pictures, each held to FFmpeg with **no bound** (`Expect::Drawn` / `Refused` / `Any`: a frame the plan lets through is the export's to the sample, and every refused frame is also drawn anyway to say whether the refusal is *needed*: `NEEDED` lines differ, `WIDE` lines would have been exact). 24 tests, all passing on 6.1.1 and 9.0.2.
Its findings on the first pass, all fixed: the shader's fused multiply-add wrapped on a result below 2^-126 (a bright shape on a field of zeros, max difference 255: `z_blur_on_a_field_of_zeros_where_the_recursion_goes_subnormal`), `set_params`' cancellation at a sigma of about 1e-8 (`z_blur_at_the_sigmas_where_gblurs_parameters_cancel`), a blur probe that read `Match`
for a build unfused in the first sweep alone, a display conversion with no gate, and a translucent layer in the benches that panicked on an unprimed probe. `z_a_machine_with_other_kernels_is_protected_by_the_probes` and `z_a_probe_that_says_match_is_not_blind_to_a_bigger_picture` run under a `KERF_FFMPEG` wrapper that adds `-cpuflags 0`:
with it the translucency, blur and display probes read `Differs` (41 U / 32 V, 6 and 4 samples) and the refusals are needed. `KERF_Z_FORCE_DISPLAY` asks for the display RGB where the grade probe would skip it: on a machine whose display probe is not a `Match` the compositor refuses the RGB (`GpuError::Unsupported`), which is the point.

**`tests/checker_a_static_jpeg.rs`** (from the independent checker of the zero-tolerance audit; `#[ignore]`d): a JPEG and a PNG at nine sizes into two canvases, both fits, two instants; footage alone in every range and tag (tv and full, untagged and BT.709, a
`yuv420p` flagged `pc`); and a PNG at the canvas's size, moved, over the same footage. Every frame is drawn and exact against the still's `yuv444p` planes, or the plan refuses it. On the base it found a resized JPEG in a still drawn Y 5,182 / U 13,104 / V 13,855
samples off on 6.1.1, a JPEG at the canvas's size and full-range footage Y 54,152 / 57,449 samples off (max 14 to 17) on 9.0.2: `FullRangeResized` now covers a still and `FullRangeStill` refuses a full-range picture in a still under a negotiated range (`core-render-plan.md`).
Measured now: 6.1.1 footage 10 of 10 drawn exact, statics 8 drawn exact and 136 refused, PNG over footage 20 of 20 exact; 9.0.2 footage 4 drawn exact and 6 refused, statics 4 exact and 140 refused, PNG over footage 4 exact and 16 refused (12 for the range, 4 for `rgb_in_stack`).

**`tests/parity.rs`** (`#[ignore]`d; CI job `parity`, a **matrix of the distro FFmpeg and
the pinned one** — the two compose colour differently, so a green run on one proves
nothing about the other) renders each case at several times through `export_still` (a
PNG at the full canvas) and through the GPU and compares them:
**0 differing samples** ([`check`] in the file: no PSNR, no edge band, no named bound): the still's own `yuv444p` planes
(`timeline_frame_yuv_cancellable`) against the compositor's composite put through the same conversion, the compositor's composite taken
to `rgb24` by FFmpeg against the PNG `export_still` wrote, and the RGBA `render_plan` reads back (the preview) against that same conversion
(it is swscale's own x86 integers now, `composite.wgsl` `fs_convert`: it used to round in `float`, up to 3 levels from the PNG), wherever the frame is even and the grade probe says the
kernel is the model's (the grade probe is the harness's proxy; the compositor itself reads `display_probe`, above). A frame with no layer under text is RGB end to end and is held to the PNG directly. A case that must be refused is `check_refused` (the plan says no, FFmpeg
still renders the frame) and one that must differ (`drawn_anyway`) differs by at least one sample, which is what makes a refusal needed. A new visual feature gets a case. Assets are made by the same probe an
import uses (`Project::probe_asset`), so a stream says what the app would know — and the
harness reads the policy the same way the product does (`composite_color_policy`), so
each case is judged against what *this* FFmpeg does: a stack of one matrix is compared
strictly, a stack the policy cannot draw (`check_mixed`: a mixed-tag stack the conversion does not cover on 9.0.2, where the
same case is compared strictly on 6.1) is **asserted refused by the plan**; a mixed-tag stack it does (`check_converted`: plain layers the chain's last scale does not resize; `check_resized`: a layer it shrinks, swscale's cascade) is held exact (`check_exact`), and one it does not (`check_resized_refused`: a layer the scale grows, a Cover crop) is asserted refused. Cases:
single clip (three sources), a gap, contain / cover into 9:16 / 16:9 and up / down
scaling, picture-in-picture (including a 361x203 layer in a 722x640 frame), scale + rotate
+ crop (refused: a 17 degree turn), odd-sized and off-canvas layers, **letterboxed layers over and under others** (bars
cover, graded bars), **opacity** (several sources and roles, a graded fade),
**matrices** (BT.709 and BT.2020 single and layered, mixed tags, an RGB PNG under and over
tagged clips, translucent layers over tagged ones in both orders), **the BT.709 label** (`label/` and `proxy/`: untagged HD alone, among
untagged footage and beside a BT.709 clip, plain and translucent in both orders, strictly on both FFmpegs; beside an HLG proxy, which the
tone-map tags BT.709; untagged SD and PNG logos, which are not labelled; `smpte170m` and `bt2020nc` footage beside it, refused on 9.0.2; the previews'
own proxies from `generate_proxy`; a real JPEG, WebP and MJPEG clip, whose `bt470bg` tags are decoders' defaults and label nothing; and `tests/motion_parity.rs`, a translucent HD clip alone in its second of a cut whose
other second holds a BT.709 clip, where the Motion plan labels by the cut's graph; a labelled HD clip's dip to white in a still and in motion), colour (all four knobs,
contrast + saturation only, warm / cool), PNG and JPEG stills, speed / reverse / keyframes (a zoom
and a position moving; a rotation and an opacity moving over a held zoom, the rotation refused),
10-bit / 4:2:2 / 4:4:4 / BGR0 / gray / full-range / odd-sized / metadata-rotated sources
(shrunk, fitted and **enlarged** — every resize of a non-4:2:0, non-gray format is
asserted refused, the mild shrinks included, and the same formats at their own size drawn), **busy sources** scaled by non-integer ratios, a clip past the end of its
footage (nothing drawn, like FFmpeg), the scaler plane by plane (including 8:1 to 20:1
shrinks), a quarter-turned grey with no colour fringe and every other turn refused by the plan and by the compositor, an asset that never recorded its pixel
format, **chroma-grid cases** (crops, Cover offsets and letterbox gaps chosen so the
4:2:0 and the native rounding differ, on 4:2:0 / 4:2:2 / 4:4:4 / gray / BGR0 / RGB PNG —
the crops of finer-chroma pictures that cannot be placed exactly are asserted refused),
the **unmeasured-policy** case (`Unknown`: BT.601 stacks drawn, BT.709 / BT.2020 refused),
the **full-range grading** cases (a graded `yuvj` clip, and a graded layer in a stack with
one, refused; ungraded and limited-range controls drawn; a keyed colour on a `yuvj` clip refused on every frame, the neutral ones included), the **range-flag** cases (a `yuv420p` file flagged `pc`: effects, `eq`, a running fade refused, plain / shrunk / enlarged drawn; `tv` / unflagged controls drawn),
the **steep grades** (`a_grade_whose_tables_amplify_the_scalers_level_past_two_is_refused_on_a_layer_it_resizes`: contrast and saturation past 2, a gamma past 1.2, a black point parked above 0, the far end of every range, on a pip, a letterbox and a Cover fit, each refused with FFmpeg rendering it; the same grade at the picture's size, and a gentle one on the pip, drawn),
the **clip's grade** (`grade/`: wheels, curves, a LUT at 100 % and 40 % and all three, over bars, testsrc2 and a gradient, untagged, BT.709 and `smpte170m` / `bt470bg` (BT.601's coefficients through the tagged way out), after an `eq`, cropped and moved over another track, dissolving in, masked, rotated, translucent, after a fade-in, a keyed opacity and a keyed position over another track; beside clips of its own matrix in either order (drawn on both FFmpegs), beside a BT.709 one (`a_graded_clip_beside_a_clip_of_another_matrix_is_refused_where_ffmpeg_converts_it_across_the_graph`: drawn on 6.1, refused in every frame of the cut on 9.0 with FFmpeg rendering it; a still of the stack too, `check_mixed`); its refusals in `what_the_compositor_does_not_draw_of_a_grade_is_refused`: a pip, an enlargement, a letterbox of an odd picture, 10-bit / 4:4:4 / 4:2:2 / gray / BGR0, full range, BT.2020, an unrecorded format, each effect, a running fade (a clip's own `fade_in` / `fade_out` over another track too: `fade` to black, not a ramp), a moving zoom, and the grade's kernels pinned to `Differs` / `Unknown`: the plan refuses before the render does; the render's own guards, each through `look_of` (`look_of_refuses_them_itself_whatever_the_plan_said`), and `composite_yuv` refusing the same plans, in `compositor/grade_passes.rs`),
and **refusals** (an EXIF-oriented JPEG, FFV1 `yuva420p`, the same with the pixel
format unrecorded, a translucent layer under 16 px, a moving zoom behind a rotation, an opacity below 1 or a dip to white, a vignette on a 4:4:4 picture, a chroma key on a letterbox or on a 4:4:4 picture, under a mask or a keyed opacity or on a rotated layer, a mask on a rotated layer or a blended one below full opacity, an invert / blur / sharpen on a full-range picture, an invert on a gray one) —
each of which FFmpeg still renders (and a fade to black or white on a translucent layer, a dip to white on a rotated, letterboxed, odd, resized or non-`yuv420p` layer or beside a black fade, a fade on a full-range picture under FFmpeg 9), **transitions** (`transition/`, `transition-kind/`, `fade/`: every kind and a clip's own fades in a still, see
above), **a moving zoom last, masks and effects** (`zoom/`, `zoom+mask/`, `mask/`, `effect/`, and `effect-planes/`: the blur, sharpen, tables and vignette plane by plane against the real filters, all sizes), and **text** (the `text/` family: the app's title and
caption styles, positions and edges, boxes, colours, sizes, keyframed and overlapping overlays, escapes, installed fonts,
over every kind of picture, with no layer, small print and a thickened bold, and the text it must refuse, each held to the same picture without the text; see
"Text" above). The table
held 822 rows on FFmpeg 6.1.1 before the small-print and synthetic-bold cases (775 on the pinned 9.0.2 the Windows and macOS bundles ship, where the mixed-tag stacks are asserted refused instead of compared; CI runs the harness without `--nocapture`, so it keeps no report); on 6.1.1 152 rows are `text/`, its
controls included, 191 the transitions, fades and dips and 184 the zoom, mask, effect and chroma key cases, plus the plane-level scaler (75), effect (108) and chroma key alpha (12) runs, all passing strictly. A failing case writes the reference, GPU and diff images to `target/parity/`
(`KERF_PARITY_KEEP=1` keeps them for passing ones; `KERF_PARITY_EXPLORE=1` prints
everything and fails nothing); every run writes `target/parity/report.txt`. The tests of a binary run in parallel on one compositor, so a fixture they share is written once and renamed into place (`tests/common`, `write_fixture`: the grades' `.cube` files): `std::fs::write` truncates first, and a plan or an FFmpeg that read the file in that window found it empty ("no LUT_3D_SIZE"), left the LUT out and drew the frame without it (one frame of `grade/keyed position` at Y 11 / UV 8, intermittent in CI, while FFmpeg's export and the GPU were each deterministic).
`tests/bench.rs` times a still on the GPU against FFmpeg at 1080p / 4K and 1 / 3 / 6
layers, decode apart from composite (`KERF_BENCH=1`), and a captioned frame cold (FFmpeg rasterises the caption: one
process, +30 to +95 ms on lavapipe by FFmpeg and size) and warm (the atlas: a few ms, inside the noise), and a mask, a blur, a sharpen and a
moving zoom frame against FFmpeg's still and export (`bench_a_mask_a_blur_and_a_moving_zoom`: the mask plane's one-off cost apart), a graded frame at 1080p (`bench_a_grade`: wheels, curves, a 33-point LUT and all three against FFmpeg's still and export; `bench-grade.txt`), and a chroma key at 1080p (`bench_a_chroma_key`: on lavapipe the key pass is about 200 ms a frame, nine emulated `double` adds a sample; to FFmpeg the key adds about 80 ms to a still and 15 ms to an export frame on 3 slice threads; a real GPU unmeasured).
`tests/coverage.rs` plans the review's archetypes (no decode, no adapter), still plans and export frames, and tells how many
frames a compositor draws before and after fades and transitions (still plans: the captioned 9:16 talking head 96.0% to 100%, the
same without captions 96.0% to 100%, the 16:9 montage with dissolves, dips and a slide 78.8% to 100%; export frames: 0% to 100% for those, since a Motion plan was
refused whole). The tutorial with a webcam picture-in-picture was 16.7% in both modes while its keyed push-ins were
`Unsupported::KeyedPlacement`; with the placement pass it is **100% in both modes** (the clip with a mask and a blur included: the push-in
is the placement, the mask and the blur are drawn at the fit size ahead of it), and the ledger's total (T1 to T8, 34200 sampled frames) went from 64.6% to 75.6% on 4.4.2 (63.7% to 74.7% on
the pinned 9.0.2). With T9 to T24 in it (69666 frames, still and motion) the pinned 9.0.2 goes from 43.7% to 51.8% drawn, `keyed_placement` (7.6% of the
frames alone) gone from (b). The floating animated SVG titles (`svg.rs`) took T19 (explainer with SVG titles, title clips and a template) from 570 / 563 of 1260 frames drawn (still / motion, both
FFmpegs) to 1050 / 1047: +964 of the 69666 on both builds, 6.1.1 from 65.4% to 66.8% (46520) and the pinned 9.0.2 from 58.5% to 59.8%, 61.7% (42950) with the matrix package's scaled conversion beside it; the 423 frames left under `svg` (kind `svg`) are the title clips. The delivery's named scalers lift T23c (`lanczos`, `scaler` in (b), 960 frames) from 0% to 100% in both modes on both
builds and move nothing else (6.1.1: 45556 to 46516 of 69666 drawn, 65.4% to 66.8%; 9.0.2: 40728 to 41688, 58.5% to 59.8%). The two are different rows (T19 is the default scaler with SVG titles, T23c a teaser with a caption and no SVG), so the gains add: 6.1.1 47480 of 69666 drawn (68.2%; 46516 with the scaler alone, 46520 with the SVG alone), 9.0.2 43910 (63.0%; 42946 and 42950), measured by the coverage test on both builds, not summed. **The readback is bounded**: the
wait for the GPU to finish is `READBACK_TIMEOUT` (30 s), a `GpuError::Readback` rather
than a caller blocked forever on a wedged device.

**The ledger** (`tests/coverage.rs`, `the_ledger_says_which_refusals_hold_the_compositor_back`) measures which refusals hold the
compositor back. Plan-only (no decode, no adapter, nothing rendered), it asks `RenderPlan::reasons` for every output frame of every
archetype, as a still plan and as an export frame, under the caps the app asks with (`Compositor::caps_pinned(grade::kernels(), scaler::kernels(), convert::kernels(), text, blend, extras)`, after the placement, planar-conversion and `geq` probes: the
header prints the answers, and the grade is out of the caps where this FFmpeg's conversions are not the model's, a moving zoom where its placement is not, a mask and a keyed opacity until its `geq` edge is known), and writes
`target/parity/ledger.txt` and `ledger-<ffmpeg version>.txt`: (a) frames sampled / drawn / refused per archetype; (b) per
`Unsupported` variant (`Unsupported::kind()`; a frame the planner cannot plan is `unplannable`) the frames it refuses, the frames drawn
**if that variant alone were lifted** (the frames it is the only reason of), split into still and motion, and the archetypes it hits
(`T3:sm` = still and motion), sorted by that gain; (c) the same by cause for the variants that bundle several (`effects/…`,
`grade_with/…`, `dip_to_white/Resized`, `format_resize/<pix_fmt>`, `still_motion/…`: its render-size causes are not gaps, see "Moving stills"), with the cause's full text as the key; (d) the greedy order to lift
them in with the drawn share after each (never `unplannable`); (e) the variants that still refuse a frame together after (d); (f) the
variants no archetype hits, each with why when it is known (the compositor draws it under `Compositor::CAPS`; the user cannot build
it; this FFmpeg's policy or grade probe takes it out of play). (g) T6 and T20, whose export frames carry a picture FFmpeg makes for the delivery's frame, by the set of reasons that refuses a frame, at the ledger's width and at the delivery's own
(an export frame is drawn at no other). It is
exact only because `RenderPlan::reasons` lists **every** cause of a layer (a refused geometry is judged on the picture's own chroma
grid so the layer's other reasons still show; a grade on a letterboxed layer the scaler resizes with no `eq` is a `GradeWith`, and a `Matte` for a compositor without `matte_effects`). The
composite colour policy decides some of it (`mixed_matrices`, `full_range_*`), so run it on both FFmpegs (`KERF_FFMPEG` /
`KERF_FFPROBE`). It plans `OriginalMedia` at 540 px wide, where the app's preview plans `ProxyMedia` at 960, and it ranks what the
archetypes reach: a variant none of them hits is absent from (b) and listed in (f), so a zero there is a fact about the archetypes,
not about the plan (`every_variant()` / `ordinal()` fail to compile when `Unsupported` gains a variant). `coverage.rs` holds the
review's T1 to T3 and five real-world shapes, T4 to T8 (generated sources, cached under `target/tmp/coverage-media` by a hash of the
FFmpeg and its arguments): a 16:9 talking head with jump cuts, a title, lower thirds and a music bed; a phone vlog (1080x1920 clips in a
16:9 canvas at 60 fps, BT.709 and BT.601 mixed, dissolved; T5b the same over `Fit::Blur`); a JPEG / PNG photo slideshow with Ken Burns
keys and crossfades; a three-angle multicam cut with captions; 4K footage graded (eq, curves, wheels) into a 1080p canvas. **T9 to T25
use every feature the editor offers that still refuses**, each a cut a user can build (`Extra` adds the sources a plan reads the
format of: HLG / PQ / BT.2020 / 10-bit / ProRes 4:2:2 / ProRes 4444 / 4:4:4 / RGB / MJPEG `yuvj422p` / full-range H.264 and VP9 /
GIF / indexed and RGBA PNG / EXR / equirect / SMPTE 240M / 29.97 / a green screen, tagged with `setparams` because FFmpeg 7 takes the
encoder's tags from the frames): T9 blend modes (leaks, grain, flashes), T10 stabilize + speed ramps + reverse + shake, T11 keyed
crop / mask / colour + motion presets, T12 face blur, spotlight, tilted and translucent masked PiPs, T13 green screen with a blur,
vignette, grade, sharpen, mask, tilt, T14 a vertical `Contain` edit with effects on letterboxed, 4K and 4:2:2 shots, T15 a wedding
from five camera formats with dips to white, LUT, curves and a punch-in, T16 HDR / BT.2020 / 10-bit travel, T17 alpha overlays
(ProRes 4444, RGBA PNG, GIF, a palette PNG, an EXR), T18 bold captions with no bold face and a missing font, T19 SVG titles, title
clips, a template and small text, T20 behaviours, T21 a 360 reframe and punch-ins, T22 a screen tutorial (4:4:4, RGB, full range,
webcam bubble, a 4K thumbnail), T23a-e one teaser delivered as ProRes, GIF, lanczos, 10-bit and 29.970029 fps (planned with the
`ExportOptions` they name), T24 SMPTE 240M / untagged / BT.2020 archive footage, T25 a green-screen presenter as a ghost (a key under a constant and a keyed opacity). Variants it cannot reach, and why: `motion`, `fade`,
`travel`, `tail`, `keyed_opacity`, `keyed_color`, `keyed_zoom`, `mask`, `text` (drawn under `CAPS`), `no_picture` (the project keeps
an audio-only asset off a video track), `text_without_font` (the shipped font cannot be written), `unmeasured_policy` (the probe
cannot tell), and per build `grade` (kernels match) or `grade_with` (kernels differ), and under
`FixedBt601` `mixed_matrices`, `mixed_matrices_with`, `matrix_unknown`, `rgb_in_stack`, `full_range_cut`, `graded_in_full_range_stack`, and `text_sequence` (a tab, over 160 characters or a canvas too wide to measure: no
archetype has one; text itself is drawn once `text::build()` has probed this FFmpeg's `drawtext`, the header prints the layout, `Compositor::caps_pinned(kernels, scaler, convert, text, blend, extras)`).

**`tests/fate_corpus.rs`** (`#[ignore]`d, and `KERF_FATE=1`: it needs a network and an `--ignored` run that is not asking for one (a full local run) must not fetch, so
without the variable it skips with a message; CI's parity job does not run it; `ffmpeg`, `ffprobe`, `curl`, no adapter) is hostile input from FFmpeg's FATE sample
suite: 26 files (8 MB) downloaded from `https://fate-suite.ffmpeg.org/` into `target/fate-suite/` (kept between runs, no checksums,
**never committed**: mixed licences) plus four cut short, covering odd and tiny sizes, 4:2:2 / 4:4:4 / 4:4:0 / 10 bit, a 16-bit gray
JPEG and a gray PNG, alpha (ProRes 4444, PNG, GIF; the VP8 webm probes as plain `yuv420p`), `yuvj`, interlaced, a display matrix, a
stream that changes size part-way, a negative first timestamp and AVI / FLV / IVF / OBU / elementary-stream containers. Each file goes
through the probe, a plan, `decode_layer`, `FrameSource::frame` at three times (start, middle, past the end) and a `FrameCursor` over
the first output frames, on a thread bounded to 900 s (more than its stages' own timeouts add up to) with its panic caught. The
contract: **a frame at the probed size with consistent planes (and the frame source's equal to the one-shot's), or an error**; never a
panic or a hang. What each stage did is printed and written to `target/parity/fate-corpus-<ffmpeg version>.txt` (a codec one build lacks
is a finding, not a failure). It found no panic or hang; it shows that the plan cannot see what only the decode can (a JPEG with an EXIF
orientation probes 200x300 and decodes 300x200, FFmpeg 9 applies a MOV clean aperture the probe's size does not: both are refused by the
size check, so FFmpeg draws them), that a bare H.264 stream answers `-ss 0.000000` with no frame on FFmpeg itself, and that a 16-bit gray
JPEG is a picture the plan draws (`gray16le` is on the allow-list) with no parity case.

**The scaled cascade's checker cases** (`checker_mx_*`, written against `convert::scaled_to_rgb24` and the plan's guards): `tests/checker_mx_cascade.rs` (`#[ignore]`d, ffmpeg and an adapter, **not in CI's list**) holds the model and the passes to FFmpeg's own `scale` plane for plane on hard edges, single-sample stripes, saturated chroma, flat pictures of every luma, ramps and a bright bottom edge, at the sizes where the plan's guards switch, in every ordered pair of matrices, both destinations and 1 to 16 filter threads, through the export's own chains (a decider, a pad, `fps`, `format=yuva420p`), the C rows at the foot of every height, and the refusals drawn anyway to see each is necessary (a Cover crop is the cascade cut, a 4:2:2 picture is not the cascade of its 4:2:0); `tests/checker_mx_probe.rs` (unix, wrapper scripts in place of ffmpeg, one child process a scenario) shows `convert::scaled()` is not `Match` where its premises break (another CPU's flags, another scaler, the accurate-rounding and bit-exact flags, a stalled or failing child, which is `Unknown` within its deadline) and that a build that never cascades is `Differs` in both probes (skipped where the build under test negotiates the matrix); `parity.rs` and `motion_parity.rs` carry the `checker_mx_*` stills and exports (a fixed-matrix FFmpeg returns at once from each: it converts nothing; the fast set runs the trimmed stills, the sweeps of pairs, scales and canvases are the cascade file's).

**Every GPU change is judged by the `parity` agent** (`.claude/agents/parity.md`, after the implementer and the verifier, before
the reviewer): it reads the C source of the filters for both FFmpeg builds, writes its own adversarial cases (tests only, never
`src/`, a shader or the implementer's cases), runs them on both builds, mutation-checks the new arithmetic, draws each refusal anyway
to see it is necessary, and reports the ledger and `coverage.rs` delta.

**The synthesized media is kept per FFmpeg build** (`tests/common/mod.rs`): `media_dir(name)` is `target/tmp/<name>-<key>`, the key a hash of the FFmpeg's path, its `-version` text (release and configuration) and the binary's size and time, so the pinned build, the system one and a wrapper that adds flags (`-cpuflags 0`) each have their own files and rendered stills (a tag or a muxer default differs between releases, and two runs in one directory rewrote each other's files: false failures, found when the builds ran side by side). `common::ffmpeg` writes an output file beside its name (`<stem>.<pid>.<n>.part.<ext>`) and renames it over it, so a process that reads the file while another one makes it again finds the old one or the new, never half of one (the file made on the way is removed when FFmpeg fails or is killed: `output_within` ends a child after a limit, 30 s for `-version` and 900 s for a run). `write_atomic` and `write_script` (a wrapper script is written under a name of its own, made executable and renamed, so it is never open for writing when it is run) are the same for a file and a script. `media_path(name)` is the directory without making it, for a case that asks whether the code under test made it (`checker_a_a_placed_clip_in_an_export_in_parts` asks whether the export made its segment directory: a `media_dir` in the child as well would have been there whatever the export did). `parity.rs`, `motion_parity.rs`, `checker_bl_probe.rs` and `checker_bl_threads.rs` use them for every directory they make, and the builds may run those at the same time; the other suites (`coverage`, `checker_z`, `checker_t6_*`, `convert`, ... with directories of their own; `checker_b_*` keys its by the FFmpeg's path alone) do not yet: run those one build at a time.

**What CI runs** (job `parity`, both FFmpegs, lavapipe): a **fast set**, an explicit allowlist of test targets, not every ignored test.
The full set took hours on a runner and the job hit its 30-minute timeout. One `--no-fail-fast` invocation of `--lib --test parity --test compositor
--test convert --test coverage --test cursor --test frame_source_fake --test prefetch --test playback --test grade --test text --test blend_kernels
-- --ignored` (12.5 minutes on 6.1.1, the slower build, 9.5 on 9.0.2; `parity` is two thirds of it; the scaled cascade's cases add, measured as CPU time on a loaded four-core machine, about 70 s on 9.0.2 for the four `checker_mx_*` stills of `parity.rs` and 17 s for `convert`'s two ffmpeg-and-pass cases, and on 6.1.1 8 s for `convert` and nothing for the stills, which return at once where nothing is converted), and the blend coverage's eight new stills cases (`blend_modes_on_a_translucent_or_resized_layer_...` and its three siblings, and the checker's four `checker_bl_stills_*`, one of which, the fades and dissolve sweep, is a third of it) cost, as CPU time with each case's start-up in it, about 13 minutes on 9.0.2 and 20 on 6.1.1 (a matrix×blend case of them: 66 s and 129 s); **the job skips seven of the nine** (the eight and the SVG title over a blend, `--skip`s in `ci.yml`, `build-and-ci.md`) and keeps `blend_modes_on_a_translucent_or_resized_layer_...` and the matrix×blend case, about 2.5 and 5.1 CPU-minutes, then `--test motion_parity -- --ignored --exact` with
`fade_arithmetic_is_ffmpegs`, `export_frames_are_drawn_without_a_transition`, `clip_fades_are_drawn_as_the_export_draws_them`,
`keyframed_position_and_rotation_in_an_export_frame`, `a_keyed_opacity_under_a_mask_is_the_geq_product_on_a_knife_edge` and `a_keyed_opacity_over_a_chroma_key_is_the_geq_product_on_a_knife_edge`. Not in it: the `checker_*` suites, the rest of `motion_parity`, `blur_slices`, `blur_model`, `bench*`,
`frame_source` (one decode case is 60-100 s), `fate_corpus`, and the single-purpose files; `cursor_fake` and `hostile_input` have no ignored test and run
in the `build` job's `cargo test --workspace`. A new test file runs in CI only once it is added to the list in `.github/workflows/ci.yml`. The letterbox's cases (above) add to the fast set three tests of `parity.rs`
(`effects_on_a_letterbox_are_drawn_as_the_still_draws_them`, `a_grade_on_a_letterbox_is_drawn_as_the_still_draws_it`, `what_a_letterbox_leaves_to_ffmpeg_is_refused`: about 40 CPU-seconds on 6.1.1 and 30 on 9.0.2 beyond the
probes a run pays once), and their heavy sweeps are named `checker_mt_stills_*` so that one `--skip checker_mt_stills` leaves them out, like the mask checker's `checker_mo_stills`: measured
CPU time (user + system, a loaded four-core machine, one process's probes included) 6.1.1 about 8 minutes for the four, 9.0.2 about 6; their export-frame siblings in `motion_parity.rs` are not in CI's list.
**Every visual change still gets its parity case in the fast set** (`parity.rs` for a still, `compositor.rs` for a pass): a case that lives only in
`motion_parity.rs` or a `checker_*` file is not run by CI. **A green CI proves the fast set**; a change to the compositor, the plan, a shader or the
scaler / conversion / grade models is run in full, locally, on both builds, a suite at a time (it takes hours):

```bash
cargo test -p kerf-gpu --no-default-features --locked -- --ignored     # the full set, the system FFmpeg
KERF_FFMPEG=crates/kerf-app/binaries/ffmpeg-<triple> KERF_FFPROBE=crates/kerf-app/binaries/ffprobe-<triple> \
  cargo test -p kerf-gpu --no-default-features --locked -- --ignored   # the full set, the pinned one
```

```bash
cargo test -p kerf-gpu --no-default-features -- --ignored        # needs ffmpeg + an adapter
KERF_GPU_ADAPTER=hardware cargo test -p kerf-gpu --no-default-features -- --ignored   # the machine's GPU
KERF_BENCH=1 cargo test -p kerf-gpu --no-default-features --release -- --ignored --nocapture bench
cargo test -p kerf-gpu --no-default-features --test coverage -- --ignored --nocapture   # frames drawn vs refused, per archetype
cargo test -p kerf-gpu --no-default-features --test coverage -- --ignored --nocapture ledger   # target/parity/ledger.txt: what refuses them
KERF_FATE=1 cargo test -p kerf-gpu --no-default-features --test fate_corpus -- --ignored --nocapture   # FATE samples (downloads into target/fate-suite/)
cargo test -p kerf-gpu --no-default-features --test motion_parity -- --ignored --nocapture   # the export's frames vs the compositor's, plane for plane
```
