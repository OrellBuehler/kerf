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
draws, `GpuCaps::A0` plus `text`, `exact_scaler` (once `scaler::kernels()` measured this FFmpeg's scaler as the model), `blur_fill`, `matrix_convert` (once `convert::kernels()` measured this FFmpeg's conversion between matrices as the model, and the grade's kernel too), `motion`, `fades`, `transitions`, `dip_white`, `keyed_zoom`, `placement` (with `behaviors`: where `placement_probe::measured()` says this FFmpeg's `pad`, `perspective` and `crop` place a picture as the model does; `Compositor::caps()` drops both otherwise), `keyed_opacity`, `keyed_color`, `mask`, `grade` (every kind, where `grade::measured()` says this FFmpeg's conversions are the model's: `Compositor::caps()` drops it otherwise), `blend` (the modes `blend::measured()` says this FFmpeg draws as the model does, both matrices) and the blur / sharpen / grayscale / invert / vignette / chroma key `effects` today, which `composite` / `render_plan` judge the plan by), and a lost device is tracked — every
later call is `GpuError::DeviceLost` and the **owner builds a new `Gpu` and
`Compositor`** (the device is not recreated behind the caller's back).
**A blend mode is drawn** (`GpuCaps::blend`, below: "Blend modes") for a layer at its own size, upright or turned by a quarter, opaque and not letterboxed, over an even frame, and
where `blend::kernels()` says this FFmpeg's chain is the model's; every other blended layer is refused by the plan (`Unsupported::BlendMode(layer, mode, cause)`,
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
export within the scaler's level (placed: 1) and the scaler's plus the translucent round trip's (scaled and translucent: 2 in every plane, the `2` row of the ladder
above: `SCALED` plus `TRANSLUCENT` would say luma 1 and chroma 2, but the scaler's level crosses into luma through the round trip's conversion). It also holds the feed (a cursor opened ahead is the one the cut uses and the first
clip's goes with it; a transport stream is refused when opened ahead; with more clips in the window than it holds it keeps the ones wanted first), and the texture a session presents to be the composite's picture.

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
    C build's (`sws::table_aligned`; the old zero padding was a level off on 7.9:1 and steeper shrinks).
  * **The vertical arithmetic.** C (`yuv2planeX_8_c`: `(64 << 12) + sum(c * s) >> 19`) or, on x86 with
    SSE2 and default flags, `yuv2yuvX.asm`: `(c * s) >> 16` per tap (`pmulhw`: every tap truncates),
    started from `(64 + 8 * (N - 1)) >> 4` with `N` the aligned tap count, `>> 3` — **except the last two
    rows of luma and the last row of 4:2:0 chroma**, which `swscale.c` hands back to the C kernel
    (`dstY >= dstH - 2`; slice threads do not matter, `dst_h` is the picture's). A single-tap axis is
    `yuv2plane1`, `(s + 64) >> 7`, the same in both.
  * **The algorithm an unflagged `scale` runs.** The product's graphs are complex ones, where the filter's
    own default applies: bicubic since FFmpeg 5.0, **bilinear before it** (4.4's `scale` defaults to
    `flags=bilinear`; only a simple `-vf` graph is handed the command line's bicubic). The probe tells
    (`Model { kernel, default }`), so the 4.4.2 here draws every fit in bilinear, as it is rendered. Explicit
    flags win (`blur_fill`'s thumbnail scales say `flags=bilinear`; a delivery's `scaler` names its own: a delivery that
    names `bicubic` is drawn as bicubic on a build whose unflagged `scale` is bilinear too, `compositor.rs` `named`,
    and any other name is refused, `Unsupported::Scaler`).
  Held by `tests/scaler.rs` (`#[ignore]`d, ffmpeg only, no adapter): the scalar model
  (`scaler::scale_plane`) equals FFmpeg's `scale` **sample for sample** on both builds over noise and a
  smoother picture, both algorithms, 2:1 to 61:1 shrinks, odd outputs, enlargements and the blurred
  fill's thumbnail and fill scales, at 1, 4 and 16 filter threads; and by the shader against the same
  FFmpeg in `parity.rs`' `the_scaler_matches_ffmpegs_scale_plane_by_plane` (max 0 in every plane, 75 plane
  results). The model is `Model::ASSUMED` (C, bicubic) while the probe is `Unknown`/`Differs`, which is
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
  conversion's truncation bias). **A quarter turn of a translucent layer is refused**
  (`Unsupported::Rotation`, from the layer's geometry: `translucent` and a 90 or 270 turn; the render guards it too): the graph runs
  `colorchannelmixer`, `rotate` and then the conversion to 4:2:0 on the RGBA picture, so the chroma pairs two pixels of a row and filters
  over the rows of the *turned* picture, while the round trip here comes before the turn (the pairs are the same samples upright and
  at 180, other ones at 90 and 270): Y exact, U and V up to 17 and 19 levels off in the first frames of `checker_b_m`'s 36 cases at 90
  and 270 (0 and 180 stay on the ladder). It cannot be drawn in the graph's order to the sample either, because the
  round trip's chroma is only within a level of swscale's even upright. A keyed opacity (a `geq` alpha on `yuva420p`, pointwise) and a
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

**Animated SVG titles (not drawn; what drawing one takes, measured).** ffmpeg never sees an SVG: `svg_layer.rs` draws each title with resvg into a straight-alpha RGBA PNG (`encode_frame_png`)
and the graph `overlay`s it (`[cur][N:v]overlay=x:y`; an export adds `settb`/`setpts`, `eof_action=pass` and `enable='between(t,start,end)'`). The same resvg draws the same pixels in the compositor's
process, so nothing of the artwork is a parity question. The rest is `overlay`'s: on a `yuv420p` composite it takes the picture as `yuva420p`, converted by an auto-inserted `scale`, and blends
with `vf_overlay.c`'s `blend_plane` (identical in 4.4.2 and 9.0.2: `FAST_DIV255(d*(255-a) + s*a)` per sample, `a` the picture's alpha for luma and, for chroma, the 2x2 average
`(a+a+a+a)>>2` — at the last chroma column or row `(alpha_v + alpha_h)>>1` of what there is). `tests/svg_overlay.rs` holds this to the real `overlay`: **the planes ffmpeg converts the PNG to inside a
`-filter_complex` graph of the same build, blended with that arithmetic, are the overlay's output with 0 of 9216 samples differing, on 4.4.2 and 9.0.2** (four random pictures with every alpha, at (0,0) and two even offsets) — so a compositor
needs no model of swscale (RGB to YUV coefficients, the 2:1 vertical chroma filter, the x86 kernels): as text asks ffmpeg for the glyph bitmaps, it asks for the planes of each picture (cached by its digest) and blends. The trap
the test records: a *simple* graph (`-vf format=yuva420p`) converts differently on 4.4.2 (87 to 1472 of 9216 samples away; the complex graph's auto-inserted scaler is not the simple graph's there), so the conversion must be asked of a `-filter_complex` graph.
What stands between this and a drawn title is plumbing, not exactness: `RenderPlan.svg` is only ids — the plan has to carry each picture's rectangle, opacity, the document's clock and which picture of the sequence the frame shows (`svg_layer::seq_of`'s
`Layout`: Held or Frames, `first_picture`, `doc_t0`, Still against Motion windows); the layer path's `compose_exact` has to take an unscaled picture with an external alpha plane (`ComposeParams` is built from a layer's geometry, and an odd
rectangle is the translucent-odd-layer case it already refuses); and a **title clip** is not an overlay but a clip chain (fit, fades and transitions on an alpha picture, then `overlay` over the lower tracks), which is the `alpha_picture` capability the ledger counts apart. T19's floating presets are
full-frame, even rectangles (480 of its 690 refused frames); its title clips are the other 210.

**The runtime self-test** (`selftest.rs`, `parity_self_test(&Arc<Gpu>)`). Parity is proven in CI on two FFmpeg
builds and one software adapter; a user has a third FFmpeg and a real GPU, so the first use of the GPU path
(the app: `app.md`) draws up to five small frames on the compositor and through that machine's own FFmpeg and holds
them to the harness's **strict limits** (`PSNR_FLAT_MIN` 40 dB, `MAX_FLAT` 8, `PSNR_ALL_MIN` 40, `FLAT_SHARE_MIN`
0.5, the same edge band: constants and `compare` are copies of `tests/parity.rs`', and
`the_runtime_self_test_judges_by_the_limits_and_the_comparison_of_this_harness` holds them equal). The frames:
`fit` (one untagged layer enlarged 4:3: swscale's tables, the final conversion), `pip` (a gradient with a shrunk,
cropped, 80%-opaque, graded picture-in-picture: the scaler down, `eq`, the opacity round trip, `overlay`) and
`bt709` (a BT.709-tagged stack: the composite colour policy the plan was given, which is how 6 and 9 differ) and
`grade` (a 640x360 untagged clip through a 5^3 LUT, curves and wheels: the x86 conversion kernels of this machine's FFmpeg, which is what `grade::kernels()` also asks) and
`place` (a clip whose zoom and position are keyed, at a moment inside the move: `pad`, `perspective` and `crop`). **The placed frame is drawn only where `placement_probe::placement()` is `Match`**
(the plan refuses a moving zoom otherwise, so there is no frame; `placement_for_selftest()` holds the placement back on an `Unknown` that survives a second try after the backoff, through the shared `measure::checked` (the grade is held back the same way, without the second try); `run` takes the answer as a parameter, and
`a_machine_whose_conversions_differ_or_are_unknown_is_checked_on_the_other_three_frames` rehearses `Differs` and `Unknown` for both). `a_compositor_that_did_not_move_the_picture_would_fail_the_placed_frame`
holds that the frame has teeth: the same clip drawn without its keys fails the check's limits.
**The scaler is pinned and held back like the grade** (`scaler::kernels_for_selftest()`, `Compositor::with_scaler_kernels`): the frames above all resize, and the plan refuses a resized layer on a scaler that is not measured to be swscale's (`ScalerApproximate`, the GPU draws none there), so a self-test that drew them on an arm64 FFmpeg (a scaler that `Differs`) or after a probe that timed out would stop on its own refusals and close the whole GPU path. Where `exact()` is false the clips are made at the canvas' size and the layers are only moved, faded and graded (`a_machine_whose_scaler_differs_or_is_unknown_is_checked_on_frames_it_does_not_resize`); an `Unknown` holds the scaler back for the process (`held_back`) until a self-test that can tell has run. **A render never runs a probe**: `plane_filters`, the grade, the matrix conversion, the resize gate and a mask's or a keyed opacity's `geq` (`kerf_core::geq_edge_measured()`) read what was measured, the same answer `caps()` was made from (no `mask` or `keyed_opacity` in it while the `geq` is `Unknown`) (`prepare` runs them, off the render lock, for a plan that needs them: the grade's conversions for a grade, a dip to white or a conversion between matrices; the playback stream and the render bar call `prepare_timeline` once, when a session is made ready or begins, which measures what the whole cut can need — the grade's conversions, the scaler, the matrix conversion, `geq`, `drawtext`'s layout, the blend chain for a blended clip, the placement and planar-conversion probes for a clip whose zoom or position moves — and a session adds `prepare_placed_files(&cut, &timeline)`, which asks the file of every placed clip for its range flag (the part that depends on what the cut resolves its clips to, a proxy included; a file already asked costs a `stat`), so that no frame of the loop waits on a probe).
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
FFmpegs: fit 48.8 dB / max 4, pip 46.2 dB / max 6, bt709 49.4 / max 5 on 6.1.1 and 48.6 / 5 on 9.0.2, grade 45.6 dB / max 3 on both, place 48.1 dB / max 4 on both (the same clip drawn without its keys: 6.6 dB / max 255); the same GPU
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
different amount on every GPU); an opaque layer keeps the hardware's, which is exactly the source. The translucent layers of `parity.rs` take the
integer blend too and stayed inside their bounds (44 to 50 dB flat, 3 to 6 levels). A slide or a push is a position (the geometry already
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
every export frame of the cut 16 off (the flagged-export reason, as for `yuvj`). A plain layer, a cut and a shrunk layer over bars are exact on 6.1.1 and, in a still, within the strict bounds on 9.0.2. So `maybe_full_range()` is true for it (the blur / sharpen / invert / vignette / key / `eq` refusals, `FullRangeResized`, and
`FullRangeCut` where the policy is not `FixedBt601`), and `FullRangeNegotiated` fires **under every policy** for it, in a still and in the export frames a fade runs in (under FFmpeg 9 the cut is the export frames' reason). `Unknown` and `tv` leave the picture as it was. **The policy is a proxy for the behaviour, measured on
FFmpeg 6.1.1 and 9.0.2 only: 5.x, 7.x, 8.x and distro builds are unmeasured** (`composite_color_policy()` probes the composite's colour, not the range handling; a build that negotiates the colour but
not the range would be refused needlessly, one that does the reverse would be drawn wrong). FFmpeg 6 converts
at the decode, as the compositor does, and draws all of it to the level — **unless the scaler resizes the picture** (`FullRangeResized`, both FFmpegs): swscale converts the range inside
the resize, and an export's chroma reads up to 15 levels off (a fade on a 1.5x enlargement: flat 14), so a resized full-range picture is refused in an export frame and inside a fade (plain, in a still, it is
drawn inside the strict bounds, 5 flat). **The dip-to-white refusals follow the Motion plane bound (2 levels) and are conservative in stills on purpose**: with
them bypassed, RGB, 10-bit, 4:4:4, 4:2:2, gray, BT.2020, full-range and half-size layers all pass the still's STRICT bounds, but `reasons()` is one function for both plan modes and a refusal
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
  compositor's **scaler** is also in play ahead of the placement (a Cover fit that resizes, a held zoom that is a `scale`) the bound is the scaler's own
  (`SCALED`, 1 level); the placement adds nothing to it. On this CPU the system 4.4.2's x86 swscale is further off the port than that at sharp edges
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
  Y 65 / UV 128 off at 90 and 180 on 6.1.1, a plain layer's chroma included, which the plan let through), and a mask or a keyed opacity is `Unsupported` while the build is `Unknown`. `kerf_gpu::prepare(&plan)` (or `prepare_timeline`, for every clip of a cut) measures it ahead of the render lock; the render reads `kerf_core::geq_edge_measured()` and never runs the probe, and `Compositor::caps()` has no `mask` or `keyed_opacity` while it is `Unknown` (the plan refuses up front); `Compositor::with_geq_edge` pins it.
* Refused (`MaskWith`): a mask on a **rotated** layer, or one below full opacity (the sampled one: its alpha goes through `colorchannelmixer`).

**Rotation: a quarter turn, or FFmpeg's.** `rotate` has two paths (`vf_rotate.c`, the same in 6.1 and 9.0): a `simple_rotate` copy when the angle is within `FLT_EPSILON` of 0, π/2, π or 3π/2, and a
general one that walks the output with `c = int_sin(angle_int + INT_PI/2)` and `s = int_sin(angle_int)`, `angle_int = (int)(angle * 65536 * 16)`, a fifth-term Taylor series in integers. The samples are whole only where `c` and `s` come
out exactly 0 and ±65536. They do for the quarters -47, -46, -43 to 40, 43 and 44, and for no other whole number of turns up to the 1000 radians `kerf_core::quarter_turn` looks at: the series and `INT_PI` are a unit off further out, growing
with the angle (41, 42 and 45 to 120 and -44, -45 and -48 to -120 quarters: the picture lands between samples; 1e7 degrees is an `int` overflow, undefined in C and done differently by the builds). Measured, not guessed: `rotate`
(the export's text, `yuva420p`, pictures of odd and even sides with a different value in every sample) against the exact quarter turn of its input, for **every quarter from -1310 to 1310, FFmpeg 6.1.1 and 9.0.2: the set above, no
other, which `quarter_turn` computes** (`checker_e_quarter_turn_is_the_set_rotate_turns_exactly_for_every_quarter`, `#[ignore]`d, `cargo test -p kerf-gpu --no-default-features --test checker_e_turn -- --ignored`) (its port of `int_sin` and the fast path, within `FLT_EPSILON` radians of a quarter, the filter's own fast-path window: 90.00001 degrees is a rotation like 91 is, and the unit tests list the set; `tests/checker_e_turn.rs` holds the sweep and the hairs to `rotate` itself). **Every other rotation is refused on every layer** (`Unsupported::Rotation`,
plain, blended, keyed opacity or not, and a rotation a key or a behaviour moves, which is a `hypot` box turned per frame): the float edge of the old pass was Y 117 / UV 169 levels off the fixed-point one at the rotated edge and no ladder is a
drawn frame. The compositor guards the same (`look_of`: `GpuError::Unsupported`) and snaps a quarter's cosine and sine to exact 0 and ±1. **A turned odd box carries the overlay's chroma tail like any odd layer** (the last chroma
block of an odd-sided layer reaches one pixel past it, `tail`): a quarter turn is a picture like another, and the tail was skipped for turned layers (a blended 639x360 gray turned 180: U 720 samples, 48 levels off; 640x359 at 90 and 270: U 2520, 13 levels).

**Keyed opacity** (export frames). FFmpeg's chain has a `geq` whose alpha is `(int)(opacity(T) * 255)` (`T` the frame's time on the
clip's `fps` grid) where a constant opacity takes `colorchannelmixer` and the RGB round trip. The plan evaluates it
(`PlanLayer::keyed_alpha`, `core-render-plan.md`) and the compositor blends with that one alpha in integers (`LayerFades::alpha`),
no round trip; the clip's fades and a dissolve's ramp act on it as on any alpha plane. The `geq` also copies Y, U and V through
`lum` / `cb` / `cr`, so on a build whose `geq` reads a plane's last column and row one in (6.1) the blend reads them so too
(`mask_edge`, as for a mask; ahead of a turn, see above: `tests/motion_parity.rs` `a_keyed_opacity_on_a_turned_layer_is_shifted_before_the_turn`, 0 differing samples at every quarter on a picture with detail at its edges, 6.1.1 and 9.0.2). Byte for byte at the picture's size (linear, eased, held, bezier off the frame grid, a tree of many
keys, 29.97, with fades, in a dissolve); the scaler's level on a pip or an odd size; turned by a quarter (any other turn is refused, "Rotation"). A mask with it is drawn
while the curve holds 1 (the `geq` writes the mask, byte for byte) and refused on the frames below it (`MaskWith`: one `geq`
multiplies both); **the alpha judged is the `geq`'s own (`keyed_alpha < 255`), not the sampled opacity**, which can sit on the
other side of 1 by the last bit. **Before a moving zoom** it is drawn on the frames where the curve holds 1 (the `geq` is the identity on
the alpha plane and copies Y, U, V: the zoom-last path of a mask, `edge_planes` ahead of the scale; `SCALED`, measured 1 / 1 on 6.1.1 and
9.0.2) and refused once it falls (`ZoomBehind`: its alpha plane would be scaled with the zoom). A **chroma key under it is refused for the clip** (`Effects`, on the frames at 255 too, so a clip is
not drawn by two renderers in turn): that `geq` reads the key's plane (`alpha(X,Y)`), and on a build that reads a plane's last
column and row one in (6.1) it reads the key's alpha there one in, which the compositor's key does not (measured on the frames at
255: luma 21 off at the edge, 2636 samples over 14 frames; 9.0.2 is exact, so shifting the key's alpha under `mask_edge` is the
follow-up, with its zoom-last variant). The compositor refuses the combination itself too.

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
4:2:2, 4:4:4, gray, RGB or BT.709 source (the `eq` runs on the picture the chain has made 4:2:0 by then, as for a static grade). A blur after it is `EFFECT`'s level, a moving
zoom after it `SCALED` (the grade runs at the fit size). **On a layer the compositor scales, the tables used to multiply the scaler's level** (the C kernel was a level from an x86 swscale's, and a table maps two values a level apart to values `eq::table_steps` apart: contrast 1.8 and gamma 2 step from 0 to 12 at Y 57, a brightness of 0.3 added from 0 to 19 at Y 15); a frame was drawn while that step was at most `MAX_AMPLIFIED_ERROR` (2) and FFmpeg's past it (`Unsupported::GradeSteep`, judged per frame and for static grades too, with the sharpen rule's geometry). **Where the scaler is measured exact (`caps.exact_scaler`: both FFmpegs of CI on x86-64) there is no level to multiply and the refusal is gone**: `eq_tables_of_any_step_are_the_export_on_a_layer_the_scaler_resizes` holds noise, a luma ramp and hard 16 / 235 edges through 15 grades, the ones that read 3 to 19 levels off before included, to 0 in every plane; the rule and its table steps stay for a machine whose scaler is not exact (the plan's unit test `a_grade_whose_tables_amplify_the_scalers_level_past_two_is_refused_on_a_layer_it_resizes`). Cost: about 20 microseconds when gamma or the temperature takes the `pow` path, under one otherwise,
kept per thread for the last four colours, so a clip's frames (the same colour) cost a lookup; built only for a graded layer with a scale upstream.
The sharpen rule below starts its worst case from the grade's luma step. **The refusals about grading the picture itself are for the clip, not the frame**
(`RenderPlan::graded`: a keyed `eq` is in the chain on every frame of the clip, neutral numbers or not): a full-range picture (`GradedFullRange`: FFmpeg 9 grades
the raw values before it converts the range, the compositor converts first) on every frame, the ones where the curve passes through neutral
included, and a graded layer in a stack that holds one where the composite's colour is negotiated (`GradedInFullRangeStack`).

**Effects.**

* `Grayscale` (`hue=s=0`: U, V to 128) and `Invert` (`negate`: `255 - v` on Y, U, V) are tables, composed with `eq`'s and applied in the scaler's vertical
  pass. **Sharpen** is `unsharp` 5x5 on luma in integers (the binomial `[1 4 6 4 1]` twice, `s + (((s - ((b + 128) >> 8)) * amount) >> 16)`). Byte for byte.
* **Blur** is `gblur` (`blur.wgsl`): the first-order recursive Gaussian in `float` (right, left, down, up; one compute invocation per row and column),
  then `postscale`, clip and `lrintf`, each plane at its own size. It is **not bit-exact and cannot be**: FFmpeg's x86 build runs the horizontal pass
  in AVX2 / AVX-512 with another association of the same sums and an FMA, so its answer depends on the CPU by the last bits of a `float`, which `lrintf`
  turns into a level where a value sits on a half. Measured on 9.0.2: 0 to 59 samples in 3.2 million a level off, none more. **The blur is the compositor's one compute pass, and on a
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
  else `kerf_core::cpu_graph_threads()` (the budget's when it caps the graph, else FFmpeg's default of cores + 1, at most 16), which is the
  playback stream's and an export's; `Compositor::with_graph_threads` pins it. An FFmpeg run with another thread count differs at those
  samples, as FFmpeg's own runs do. Byte for byte against the filter at the same thread count (`Compositor::chroma_key_alpha` reads the
  plane back for the plane-level test; the parity stills and export frames use the budget's). **Full range:** a `yuv420p` picture whose full range is only a `color_range=pc` tag (VP9, HEVC, AV1) is refused (schema v10 records the range): FFmpeg keys its raw chroma, the compositor the converted one (19 levels off on a gradient and 73 on the test pattern under 9.0.2; 6.1.1 happened to agree on the gradient). The same **gap** as the vignette for an asset stored before v10.
* Refused (`Effects`): a vignette on a picture that is not `yuv420p` or that is full range (by its format or its `pc` flag: `maybe_full_range_in`; FFmpeg vignettes it in its own
  format: other planes, another dither course), a vignette map over the device's storage-buffer limit; a chroma key after a scale or a
  blur (its alpha moves by up to 1/blend levels for a level of chroma, and a blend of 0 flips it 0 to 255: drawn only on chroma that is
  FFmpeg's to the level), on a picture that is not `yuv420p` or that is full range, on a rotated layer or one below full
  opacity, on one narrower or shorter than 2, or with a similarity or a blend `chromakey` refuses (0.00001 to 1, 0 to 1), or under a keyed opacity (see above); `MaskWith`: a mask over a chroma key (`geq` multiplies the key's alpha). A sigma outside 0 to 1024
  or an amount outside -2 to 5 (FFmpeg's options refuse them too), a blur over the device's storage-buffer limit, and a **sharpen that amplifies a difference upstream of it** (judged with the geometry, so by render size): FFmpeg's scale or blur is a level from ours, and `unsharp` carries it through at worst `1 + 2|amount|` at an edge sample and rounds up a level more with each further sharpen (its `>> 16` floors). From the first source (a scale before the effects: every stage, or the fit scale when a zoom comes last; or a blur with sigma > 0) the worst case starts at 1 and each later sharpen that is not zero at `unsharp`'s 16 bits adds `ceil(2 * err * |amount|)`; the frame is refused when it passes 2. So one sharpen of at most 0.5 is drawn (0.6 is not), and any second one is refused, however small. With no source upstream a sharpen is exact at any amount, chained or not, and a blur after it counts for nothing. `Matte`: a blur, a sharpen, a vignette or a chroma key on a
  letterbox (they would see the bars as picture; tables and a grade carry the bars through). By the picture (`effect_on_picture_refusal`): an invert,
  blur or sharpen on a **full-range** picture or one whose format was never recorded (`maybe_full_range_in(policy)`: where FFmpeg 9 negotiates the range this
  holds a deeper-than-8-bit one too, which may carry a `pc` tag; FFmpeg runs the filter on the raw values and converts the range afterwards: an invert is 8
  levels off, a sharpen's overshoot 26, on 9.0.2; grayscale is the same on any range), and an invert of a **gray** picture (FFmpeg leaves
  its chroma at 128, the compositor's planes would go to 127).
* 4:2:2, 4:4:4, RGB and gray pictures (and 10-bit under `FixedBt601`) take every other effect and a mask at the plain source's own figures (max 3: the decode's rounding).
  Compute passes are core wgpu on every backend and the blur writes no storage texture: no optional feature.

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
— and, by the letterbox rules that still hold, a blur, sharpen, vignette or chroma key on the padded frame (they would see the fill as picture), a grade,
a dip to white. `tests/motion_parity.rs` (`a_blurred_fill_*`, 7 tests, 117 cases) holds the export's planes to `EXACT` on both FFmpegs: portrait / landscape /
square, noise, hard edges, odd and 20:1 sources, frame sizes from 96x64 to 3840x2160 (radii 1 to 16), fades, dissolves, dips,
the eight slides and pushes, over and under other tracks, `eq`, grayscale, a keyed colour, speed, reverse, a late source, every matrix (untagged, BT.709,
`smpte170m`, `bt470bg`, BT.2020) and a `tv` flag. `tests/parity.rs` (`a_blurred_fill_is_drawn_as_the_still_draws_it`) holds the still exactly too, through
`still_planes_differ`: the still's own `yuv444p` planes (`timeline_frame_yuv_cancellable`) against the compositor's composite put through the same
conversion in a graph of the same kind — **an exact comparator for still plans** that the RGB one (edge band, PSNR) cannot be; it holds on both FFmpegs.

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
The same probe sets `caps.blend_extras` (`blend::extras()`): `BT2020` when all nine modes and the placement matched in BT.2020, and `RGB_PICTURE` when **the export's own chain** (`blend_probe_graph`, an `rgb24` clip into its `format=yuva420p`, placed by the real `overlay`) comes back as the model's blend of the planes the decode makes of the same noise (`scale=out_range=tv`, then 4:2:0: one more child), BT.601 (an RGB picture is refused in any other stack). Not the two conversions side by side: 4.4's are equal on noise and its blended RGB picture is not the decode's.
**The probe is bounded and keeps only a complete answer** (`judge`, `run_child`): one deadline (`PROBE_TIMEOUT`) for all ~40 children, run `WORKERS` (6) at a time, none under a lock, **one measurement at a time whoever asks** (`Probe::found`: a second caller waits on a condition variable for the first, no longer than the measurement's own bound plus a grace -- then it is given "not yet", nothing drawn -- and takes its answer, a kept one or the backoff's nothing, rather than launching the children again; the state is locked to read and to store, never while a child runs); a measurement that panics (or a worker: its job counts as a child that did not answer) is contained -- not measured, the backoff set, the place given up (`found` catches it and finishes with nothing; `Turn`'s drop is the backstop for a finish that itself unwinds) -- so later callers do not probe again at once; a late child is killed and its pipes are never joined on (the readers are detached and answer over a channel, so a wrapper script's grandchild holding a pipe cannot hold the probe). A child that did not answer is logged with the job it probes and the tail of its stderr (`Failed`, 512 bytes). **A child that did not answer** (late, failed, no frame) **takes what it probes out** (its mode, its matrix's placement, the RGB lift, BT.2020) whatever the others found, and the probe is then not kept (`Verdict::kept`): `found()` draws nothing and retries after the backoff, so a timeout on a loaded machine neither draws a mode unmeasured nor switches one off for the process's life.
**FFmpeg 4.4.2's probe fails, and that is a real bug of that build**: its `overlay` slices the alpha plane of a clip placed above the frame's top edge wrongly when it runs on several threads (the alpha loses rows: the blend does nothing; Y 212 off), so on 4.4.2 every blend is refused,
and the parity cases there check the refusal and then draw with the caps forced (`forced_blends`, `blend_check`) to hold the arithmetic to the export's planes for the clips not above the edge.
*Refused* (`BlendMode` with its cause; every cause is listed): a layer the compositor's scaler resizes (the scaler's level is not swscale's and the blend carries it into the composite), a rotated one **except a quarter turn, half or three quarters (`kerf_core::quarter_turn`: the `rotate` filter lands on whole samples there; any other angle and a keyed rotation are refused, "Rotation")**, one below full
opacity (the RGB round trip's chroma is a level off), a letterboxed one (its bars are transparent in the blend), **a graded one (a blended clip's chain runs `format=yuva420p` and the transparent `pad` before the grade, so FFmpeg's RGB filters run `yuva420p -> argb -> yuva420p` where the grade's model is `yuv420p -> rgb24`: Y 11 / UV 9 off, every case)**, an RGB picture where the probe says the graph's conversion is not the decode's (FFmpeg 4.4's is Y 3 / UV 11 off), a matrix with no tables (an unlisted tag included; BT.2020 where it was not measured), **a layer a moving zoom or position places** (the placement ends the clip's chain in `pad`, `perspective` and `crop`, and no case holds the blend's two placements of that picture to FFmpeg's: `RenderPlan::pose`, and the compositor's own guard on `look.placed`; `a_blended_clip_whose_zoom_or_position_moves_is_ffmpegs`), a mode the probe refused. (An odd frame is refused by `composite_texts` for every plan; it is not a blend cause.)
**The ledger's T9 stays refused** (1920 frames, 9.0.2: `blend_mode/on a layer below full opacity` and `blend_mode/on a layer the compositor's scaler resizes` hit every frame, the mode itself none): its overlays are translucent and, planned at the preview's 540 px, resized. Lifting them is an exact scaler (a blended clip at the render size's own picture is drawn today) and an exact RGB round trip. Parity: `motion_parity` (`blend/...`: every mode over the bars and the test pattern both ways, over noise both ways, fades, keyed opacity, masks, a key, a dissolve, moved / off the frame / odd positions, over nothing,
two blended clips, a late start under a plain clip, BT.709 / `smpte170m` / `bt470bg` footage; **all 0 differing samples in every plane**) and `parity` (the stills at `STRICT`: PSNR >= 46 dB, max 3, the PNG conversion's own rounding).

**The clip's grade** (wheels = `colorbalance`, curves = `lutrgb`, a 3D LUT = `lut3d`; `grade.rs`, `shaders/grade.wgsl`, `compositor/grade_passes.rs`,
`GpuCaps::grade`; not the `eq` colour above, which is a YUV operation). The grade's filters only take RGB, so FFmpeg converts the picture `yuv420p -> rgb24`, runs
`lut3d`, `lutrgb`, `colorbalance` (that order) and converts it back. The compositor draws **the conversions as well as the filters, to the level**: a graded layer at its
own size is the export's, sample for sample (0 differing samples in `tests/grade.rs` and in 106 of the 108 `grade/` rows of `motion_parity` on 6.1.1 (104 of 106 on 9.0.2, which does not run the two cuts that are 6.1's alone, a graded clip before a BT.709 one), the frames the plan draws; the other two are a rotated layer and a translucent one at their own ladder's level; the Still-plan `grade/` rows are the ungraded family's own 43-51 dB, max 2-5 (the rotated layer is the low end, 43.1 dB / 5), `STRICT`). Every stage was written from the FFmpeg 6.1.1 and 9.0.2 sources, first as a numpy model, then as the scalar
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
  the self-test, which every GPU frame waits for. `Compositor::caps_for(kernels, scaler, convert)` is the same without a device (the render bar's; `caps_with` adds the text layout, the blend modes and their extras, `caps_pinned` the whole set but the placement). `grade_look` keeps its own check (`Unsupported` for a caller that skipped the plan), and
  `Compositor::CAPS` stays the build's "assuming they match" (coverage, benches). The probe picture is held to tell the x86 vertical kernel from the C one (`the_probe_picture_tells_the_x86_vertical_kernel_from_the_c_one`); the 14 chains are held to be 14 different answers.
  **The grade is measured on x86-64 only**; elsewhere the probe decides.
* **Refused** (each `Unsupported::GradeWith` / `Matte` / `ZoomBehind` in the plan, a held case in `parity.rs` and `motion_parity.rs` with FFmpeg rendering the fallback; `core-render-plan.md`
  has the list): a layer the scaler resizes **with no `eq` between the fit `scale` and the grade** (the filters between pass any format along and the grade's RGB-only filters ask for RGB, so the negotiation has the fit `scale` output RGB itself, scaling and converting in one pass from chroma the compositor has already scaled; with an `eq`, which takes planar YCbCr only, the conversion is its own `scale` and the exact scaler's picture is what it reads: drawn, and the same on a machine whose scaler is not measured exact is refused as the scaler's level times the conversions, 3 levels of luma and 2 of chroma), a letterbox (the bars are
  graded and their chroma meets the picture's in the conversion back), an odd side, any picture but 8-bit limited-range `yuv420p` (FFmpeg grades the others in the format it
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
  samples there); a letterbox needs the bars as padded planes before the grade; an effect or a fade after a grade needs the RGB / 4:4:4 picture the filter negotiates; a dip to
  white and a translucent layer are exact in the plan's other places and would gain from the same kernels. The 10-bit, 4:2:2 and 4:4:4 pictures need their own conversion kernels.

**Layers of another matrix than the bottom's** (`convert.rs`, `GpuCaps::matrix_convert`, `RenderPlan::convert_refusal`; FFmpeg 9, `BottomLayerTag` only). **A compositor whose conversion is not `Match` draws no frame of a mixed-matrix cut** (`Unsupported::MixedMatrices` on every frame, the deciding clip's own included: its matrix is the composite's so nothing converts it, but *which* clip decides is the planner's reading of the graph, and the probe checks that reading in the same pass as the conversion, so a machine that fails either has checked neither). **Which clip decides the composite's matrix**, for the whole cut (`PlanCanvas::cut_matrix`), is read from FFmpeg 9's format negotiation (`avfiltergraph.c`) and held by real files: the colour-space lists of an `overlay`'s three pads are one list, a `scale` has two (in and out), and each list is reduced to the single value its filter's input holds, filters in the order the graph was written, with the converters it inserts after every one of them. A clip whose chain already ends in `yuva420p` (what `overlay`'s second input takes), with nothing in it forcing a converter, needs none before the overlay: its post-`scale` pads are tied to the composite's list and its fit `scale` makes the composite's matrix its own tag. Every other clip gets a converter `scale` (the pixel-format conversion to `yuva420p`, which also carries the matrix) whose input is its tag and whose output is the composite's, inserted when the graph's links are visited. So the composite takes the tag of **the first such clip on the alpha plane**, else of the first clip (a converter each), the order being the graph's chains: **a track's clips in start-time order** (`cli.rs`'s own sort, not storage order), tracks in list order; and every clip tagged otherwise is converted into it once, wherever it is on the timeline: a lone clip of the other matrix on screen is converted all the same, which the per-frame `composite_matrix` did not know. What puts a chain on the plane, with nothing in it that may carry a converter (`Ends::Clean`, the clips that decide), is a dissolve, a keyed zoom or position (`placement`) and a source with an alpha channel: held by `tests/checker_b_filters.rs` (`deciding_clip_on_an_alpha_plane`, a keyed zoom held at one value being a plain chain). A clip on the plane that holds an opacity below 1 (an RGB round trip), a rotation, a mask or a chroma key (not measured), a colour correction (`eq` takes no alpha), a grade, an effect, a tone map, a reprojection, a dip to white on either side of it (`fade c=white` runs on RGB: an alpha-source or dissolving clip that dips, as the outgoing clip of a `DipToWhite` or as the incoming one after two plain clips, was not the decider: every sample moved), or (placed) a black fade, may carry a converter in its chain and is not read (`Ends::Unread`): nor is a blend mode (`blend`, in RGB): a cut of mixed matrices holding one is `MixedMatrices` in every frame (`PlanCanvas::unread_decider_in_cut`). An RGB picture is taken into YCbCr with the **cut's** matrix, so a frame of it alone in a cut that holds a BT.709 or BT.2020 clip is refused, not judged by the stack on screen (`PlanCanvas::cut_non_601`, `RgbInStack`). Read from a build that behaves so (three clips in every combination of plain and dissolving over BT.601 / BT.709, and `tests/checker_b_filters.rs`'s deciding-clip, storage-order and fuzz cases: 0 differing samples in every drawn frame).

The conversion itself: swscale has no YCbCr-to-YCbCr matrix change and goes **through RGB** in two contexts: `yuv420p -> bgr24` with the source's coefficients (the x86 SSSE3 kernel of the grade's way in, `X86Kernel`) and `bgr24 -> yuv420p` with the destination's by **`ff_rgb24toyv12`**, the converter an unscaled even-width `bgr24 -> yuv420p` has of its own (`bgr24ToYv12Wrapper`; not the generic scaler the grade's way out is): `Y = ((ry*r + gy*g + by*b) >> 15) + 16`, chroma from the 2x2 block's channel means (`>> 2`) the same way plus 128, the C function on every row on the pinned 9.0.2 and **an x86 build with the inline MMX one would run other arithmetic between the first two and last two rows** (`Variant::Mmx`, modelled, not drawn: the probe says `Differs` and the plan refuses). Measured: 0 differing samples on noise and on the probe's polynomial in all three planes, every pair of BT.601, BT.709, BT.2020, both orders; the passes equal the scalar reference for every pair at seven sizes (`tests/convert.rs`). **Where in the chain**: in an export a plain chain ends in `format=yuv420p` and the conversion is a converter `scale` after it: **scaled, then converted** (0 of 10240 luma samples differ in that order, 9931 the other way). **A still's graph has no `format`**: the chain's last `scale` converts the matrix itself, in one pass with its resize (47398 of 230400 luma samples differ on a half-size picture-in-picture), so in a still a layer whose last scale resizes it (a picture-in-picture, a shrink, an enlargement, a Cover crop, a letterbox) is refused; one the chain does not resize is exact (the identity `scale` converts unscaled), a dissolving one included. **A clip on the plane that is to be converted** (it is not the decider) has its fit `scale` convert the matrix itself, in one pass with its resize, as in a still: the phone clips (540x960 into 960x540) converted after their resize are 22 % of the luma off, up to 17 levels, and match neither convert-before-scale nor convert-after-chain; so such a layer is refused where the chain resizes it (`MixedMatricesWith`) and drawn where it does not. The decider is not converted at all; the plain clips of the cut are converted exactly (the first clip of a vlog among them).

What is converted: an opaque 8-bit limited-range `yuv420p` video picture (an export's 4:2:2 and 4:4:4 too: they reach the conversion as the 4:2:0 the decode makes, as a `format=yuv420p` ahead of the converter does; a still's `scale` converts them and the matrix in one pass: refused there) of even size (and bars that meet it on the 4:2:0 grid: they are black in every matrix; over a blurred fill the whole filled frame is converted, which a plain layer takes: a dissolving one does not), a crop, a scale, a pad, a position, speed, invert and grayscale (tables the export's scale applies ahead of the converter; refused in a still, whose composite is not the conversion of the effect's picture: 470555 samples differ; not on bars), a dissolve, a slide or a push, and a fade to black that is not running; refused (`Unsupported::MixedMatricesWith`, a detail each, each a held case in `parity.rs` / `motion_parity.rs`): a grade, any other effect, a mask, a rotation or an opacity below 1, a colour correction, a keyed number, an HDR / reframed / 360 picture, a dip to white, a fade to black running (FFmpeg fades the picture ahead of the conversion), 10-bit, gray or yuvj or a still image, a grade anywhere in a mixed stack, an RGB picture in a stack or cut that is not BT.601 (`RgbInStack`), and a cut whose deciding clip is no YCbCr picture of a known matrix or is not read (`MixedMatrices`, every frame). The probe (`convert::kernels`, like `grade::kernels`: three rounds, of 3, 2 and 1 children, once per process, cached, backoff) runs the real overlay graph for BT.709 under BT.601 and BT.2020 under BT.709 (the cheap round, three children at once; a machine that converts another way is `Differs` there, for good, without the files below) and, only where those match, **the three-clip graph the planner's decider reads** (two tagged files made first, so a failure past this point is `Unknown`, asked again after the backoff) (a plain BT.709 clip, a clip whose chain ends in `yuva420p` with no tag, a plain BT.709 clip, side by side: the composite takes the alpha-plane clip's tag, so the plain pair is converted into BT.601 and the middle one is untouched; a build that negotiates otherwise is `Differs`, and every mixed-matrix cut is FFmpeg's), and the caps carry `matrix_convert` only where it matches **and** the grade's kernel does (the way in). `Compositor::convert_yuv` is the passes for the plane-level test.

**`tests/motion_parity.rs`** (`#[ignore]`d; both FFmpegs in CI's parity job) holds the compositor to the **export**, frame by frame: each case builds a short
cut, renders it through `render_with` (the real graph) losslessly, reads **every frame back as `yuv420p` planes** (no colour conversion between: the planes `overlay`
blended) and draws the same frames from a Motion plan through `MotionFrames`, comparing plane for plane with a stated bound per case, **every frame of the range, the last
included** (`target/parity/motion-report.txt`; montages `FFmpeg | GPU | diff x8` of frames worth a look and of any over the bound in `target/parity/motion/`). The sources are lossless
(testsrc2 with a frame counter, colour bars, a 25 fps clip, a still, a portrait one for a letterbox, noise in all three planes, a gradient for the rotated cases) and, for the
format and range cases, `variants()` (10-bit, 4:4:4, 4:2:2, gray, an RGB PNG, MJPEG and a JPEG still, BT.709-tagged). Families: plain cuts and speeds at five rates, clip fades,
dips, dissolves, the eight slides and pushes, keyed position, quarter-turned and translucent layers on an alpha plane (a keyed rotation and any other turn are refused cases), and the coverage archetypes' montage and talking-head
shapes. **Every sample equals the export's wherever a picture is drawn at its own size on 9.0.2 and 6.1.1 (max error 0)**; one level where the compositor scales a layer on a machine whose scaler is not measured exact (`scaler.rs`; 0 where it is), two where it scales **and** grades, and a dip to
white at two (`WHITE`; three of Y on BT.709, `WHITE_709`). **A grade at the picture's size is exact on both** (0 differing samples in the frames drawn of all but two of the 108 `grade/` rows: each part alone and the three together over six pictures, untagged, BT.709 and `smpte170m` / `bt470bg`, a crop, speed 2, an `eq` ahead of it (static and keyed), over another track, dissolving in and out, under a mask and a chroma key, a keyed opacity and a keyed position over another track, beside clips of its own matrix in either order, a labelled HD clip; rotated at one level and translucent at one level of chroma, which is those layers' own ladder). **A refusal case** (`run_refusing`) skips the frames the plan refuses for its reason, requires at least one, and
holds **every other frame to the export** (a refusal wider than it has to be shows): the letterboxed / odd / resized / not-`yuv420p` dips to white, the fades on a translucent layer,
the full-range pictures (`run_full_range`: drawn exactly under `FixedBt601`, refused under the negotiating policies), and the `eq` tables of any step on a layer the scaler resizes (`run_grade_steep`: a keyed ramp frame by frame, drawn or refused by the plan's caps; `eq_tables_of_any_step_are_the_export_on_a_layer_the_scaler_resizes` holds the static ones to the level on a luma ramp, hard edges and noise). A mutation of the chroma constant by nine in 65536 fails the dips
at once. The Still-plan side is `parity.rs`: `frames_inside_a_transition_are_drawn_as_ffmpegs_still_draws_them` (the plan holds the frame and the GPU is held strictly to the still, the still to
the export), `every_kind_of_transition_and_the_clips_own_fades_are_drawn_in_a_still`, `a_dip_to_white_is_drawn_where_its_round_trip_was_measured_and_refused_elsewhere` and
`a_full_range_picture_fades_as_the_ffmpeg_in_use_converts_its_range` (the `check_refused` cases, with FFmpeg rendering each fallback).
**The range flag's cases** (`a_yuv420p_flagged_full_range_is_drawn_or_refused_as_the_export_reads_its_raw_samples`; the Still side `a_yuv420p_flagged_full_range_is_refused_where_ffmpeg_reads_its_raw_samples`):
a lossless FFV1 `yuv420p` file flagged `-color_range pc` (a codec whose decoder keeps the format `yuv420p`, as VP9 / HEVC / AV1 do; H.264 would call it `yuvj420p`), the same pattern flagged `tv` and unflagged. Motion: plain, black-then-flagged
and flagged-then-limited cuts exact under FFmpeg 6 and refused as the cut (`FullRangeCut`) under 9; the fade refused for the frames it runs in under 6 (`FullRangeNegotiated`; the one frame at full factor is held exact) and as the cut under 9; a shrink and an enlargement refused
(`FullRangeResized`) under both; the vignette, invert, blur, sharpen, `eq` and chroma key refused on every frame under both; the `tv` and unflagged controls exact in all of them. Still: the same refusals with FFmpeg rendering each fallback, the plain, shrunk, enlarged and
after-limited frames strictly drawn, and **the refusal's necessity** (`drawn_anyway`): the very frames planned as an asset stored before v10 would be (flag `None`) and drawn are outside the strict bounds (vignette 12 / 13, fade 9 / 11, and on FFmpeg 9 the key and `eq`), so a compositor that learns to
read raw samples fails the test and the refusal can go.
**The BT.709 label's case** (`untagged_hd_is_labelled_for_the_whole_graph_that_holds_a_matrix`): a 1280x720 untagged HD clip made translucent (2 levels of Y / UV, the scaler's and the round trip's:
`HD_TRANSLUCENT`) alone in the first second of a cut whose second second holds a BT.709 clip, and over a BT.709 clip, plus the same frames with no BT.709 clip in the graph; the Motion plan labels by the cut's graph where a
Still plan labels by the layers on screen. Reading the labelled layer as BT.601 again fails it on both FFmpegs (9.0.2 refuses the frames as mixed matrices, 6.1.1 draws them 8 levels off).
**The label's dip case** (`a_labelled_hd_clip_dips_through_white_with_the_bt709_round_trip`, `WHITE_709`; the Still side `a_labelled_hd_layer_in_a_dip_to_white_is_drawn_as_the_layer_tagged_in_the_file`):
a labelled HD clip dipping through white is drawn as the file tagged BT.709 is, which is what the export renders because the label is the first filter of the chain. Behind the fit `scale` (the filter FFmpeg has convert the picture
to the `rgb24` the white `fade` needs) it was BT.601 out of YUV and BT.709 back, and the compositor was Y 17 / UV 9 off the export on 9.0.2, Y 30 / UV 16 on 6.1.1, on every frame of the clip.

**GPU-6's cases** (zoom in, out, from small, with a position, eased, three keys, late, speed 2, reversed, Cover into 960x540 and 9:16; masks
shapes x feather x invert, off-centre, larger than the frame, thin, the shape's edge on the picture's border, over noise; the effects and their chains;
a dissolve, dip, slide or push into a zooming clip and out of one) hold the bounds of the ladder below, **on both FFmpegs**:

| Bound (Y / UV) | Cases | Why |
| --- | --- | --- |
| 0 | a mask at the picture's size (20 of 27 mask rows), grayscale, invert, sharpen, a vignette (every frame of the run), a chroma key, a keyed opacity and a keyed colour (the fades and dissolve of each included) at the picture's size | integer arithmetic, reproduced |
| 1 (`SCALED`; **0 where `scaler::kernels()` is a `Match`**, which `check_motion` holds them to) | a layer the compositor scales, a mask, a vignette or a keyed opacity on it, a one-stage zoom, a keyed colour before a moving zoom | on a machine whose scaler is not measured the compositor draws the C kernel, a level from an x86 swscale's; both FFmpegs of CI are measured and these are exact |
| 1 (`EFFECT`) | a blur at the picture's size, and a blur then a vignette (measured 0: the vignette's factor is at most 1) | `gblur`'s SIMD summation order (above) |
| 2 | a layer the compositor scales *and* makes translucent, no effect (`tests/playback.rs`, `play/pip/translucent and scaled`, measured Y 2 / UV 2 on both FFmpegs); `EFFECT_SCALED`: a blur, grayscale or invert on a scaled or translucent layer; a blur then a sharpen of at most 0.5 even unscaled and opaque (the blur's level times the gain); a sharpen of at most 0.5 after a scale. `CASCADED`: a zoom (the fit scale, then the zoom's). `ZOOMED_MASK`: a soft mask edge, or a chroma key's, on a zoomed picture (measured 2 / 2). Rotated, graded and scaled (`GRADED_SCALED`: on a machine whose scaler is not measured exact, a keyed or static grade whose tables step by at most 2, the rest refused (`GradeSteep`), measured 2 / 2; where it is exact, 0), dip to white (Y three on BT.709, `WHITE_709`) | two of the causes above on one sample; the RGB kernel for white |

A **sharpen after a scale or a blur has no row beyond that: it is refused past a worst-case error of 2.** `unsharp` is exact, but it carries the one-level difference upstream (the scaler's, or `gblur`'s SIMD order) through at worst `1 + 2|amount|` levels of luma at an edge sample (measured after a scale: 3 at 1.5, 7 at 3, 11 at 5; after a blur on 6.1.1: 2 at 1, 3 at 1.5, 5-6 at 5), and its `>> 16` floors, so each further sharpen adds at least a level (measured on 6.1.1: two of 0.2 reach 3, three of 0.1 reach 4), and the GPU refuses rather than approximates. One sharpen of 0.5 is the two levels of a scaled blur. (A grade between a scale and a sharpen amplifies too: the worst case starts at the grade's steepest luma step, `GradeSteep` above.)

**`tests/parity.rs`** (`#[ignore]`d; CI job `parity`, a **matrix of the distro FFmpeg and
the pinned one** — the two compose colour differently, so a green run on one proves
nothing about the other) renders each case at several times through `export_still` (a
PNG at the full canvas) and through the GPU and compares them:
**PSNR >= 40 dB and max per-channel error <= 8/255 outside the edge band**, where the
band is every pixel within 2 px of a >24-level step in the *reference*,
**at least half the frame must be outside it** (a case that is edge everywhere — noise, a
1-px checkerboard — takes `BUSY` limits: the whole image against the same error bounds),
plus a whole-image PSNR floor (40 dB) that catches
a layer a row off. Thresholds live as named constants at the top of the file with the
reason for each and the measured numbers at the bottom; a case never relaxes them
silently, and a new visual feature gets a case. Assets are made by the same probe an
import uses (`Project::probe_asset`), so a stream says what the app would know — and the
harness reads the policy the same way the product does (`composite_color_policy`), so
each case is judged against what *this* FFmpeg does: a stack of one matrix is compared
strictly, a stack the policy cannot draw (`check_mixed`: a mixed-tag stack the conversion does not cover on 9.0.2, where the
same case is compared strictly on 6.1) is **asserted refused by the plan**; a mixed-tag stack it does (`check_converted`: plain layers the chain's last scale does not resize) is held exact (`check_exact`). Cases:
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
the **clip's grade** (`grade/`: wheels, curves, a LUT at 100 % and 40 % and all three, over bars, testsrc2 and a gradient, untagged, BT.709 and `smpte170m` / `bt470bg` (BT.601's coefficients through the tagged way out), after an `eq`, cropped and moved over another track, dissolving in, masked, rotated, translucent, after a fade-in, a keyed opacity and a keyed position over another track; beside clips of its own matrix in either order (drawn on both FFmpegs), beside a BT.709 one (`a_graded_clip_beside_a_clip_of_another_matrix_is_refused_where_ffmpeg_converts_it_across_the_graph`: drawn on 6.1, refused in every frame of the cut on 9.0 with FFmpeg rendering it; a still of the stack too, `check_mixed`); its refusals in `what_the_compositor_does_not_draw_of_a_grade_is_refused`: a pip, an enlargement, a letterbox, 10-bit / 4:4:4 / 4:2:2 / gray / BGR0, full range, BT.2020, an unrecorded format, each effect, a running fade (a clip's own `fade_in` / `fade_out` over another track too: `fade` to black, not a ramp), a moving zoom, and the grade's kernels pinned to `Differs` / `Unknown`: the plan refuses before the render does; the render's own guards, each through `look_of` (`look_of_refuses_them_itself_whatever_the_plan_said`), and `composite_yuv` refusing the same plans, in `compositor/grade_passes.rs`),
and **refusals** (an EXIF-oriented JPEG, FFV1 `yuva420p`, the same with the pixel
format unrecorded, a translucent odd layer, a moving zoom behind a rotation, an opacity below 1 or a dip to white, a vignette on a 4:4:4 picture or a letterbox, a chroma key on a pip, after a blur, on a 4:4:4 picture, under a mask or a keyed opacity or on a rotated layer, a mask on a rotated or a translucent layer, a blur on a letterbox, an invert / blur / sharpen on a full-range picture, an invert on a gray one) —
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
frames alone) gone from (b). **The readback is bounded**: the
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
`grade_with/…`, `dip_to_white/Resized`, `format_resize/<pix_fmt>`), with the cause's full text as the key; (d) the greedy order to lift
them in with the drawn share after each (never `unplannable`); (e) the variants that still refuse a frame together after (d); (f) the
variants no archetype hits, each with why when it is known (the compositor draws it under `Compositor::CAPS`; the user cannot build
it; this FFmpeg's policy or grade probe takes it out of play). It is
exact only because `RenderPlan::reasons` lists **every** cause of a layer (a refused geometry is judged on the picture's own chroma
grid so the layer's other reasons still show; a grade on a letterboxed layer the scaler resizes is a `Matte` and a `GradeWith`). The
composite colour policy decides some of it (`mixed_matrices`, `full_range_*`), so run it on both FFmpegs (`KERF_FFMPEG` /
`KERF_FFPROBE`). It plans `OriginalMedia` at 540 px wide, where the app's preview plans `ProxyMedia` at 960, and it ranks what the
archetypes reach: a variant none of them hits is absent from (b) and listed in (f), so a zero there is a fact about the archetypes,
not about the plan (`every_variant()` / `ordinal()` fail to compile when `Unsupported` gains a variant). `coverage.rs` holds the
review's T1 to T3 and five real-world shapes, T4 to T8 (generated sources, cached under `target/tmp/coverage-media` by a hash of the
FFmpeg and its arguments): a 16:9 talking head with jump cuts, a title, lower thirds and a music bed; a phone vlog (1080x1920 clips in a
16:9 canvas at 60 fps, BT.709 and BT.601 mixed, dissolved; T5b the same over `Fit::Blur`); a JPEG / PNG photo slideshow with Ken Burns
keys and crossfades; a three-angle multicam cut with captions; 4K footage graded (eq, curves, wheels) into a 1080p canvas. **T9 to T24
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
`ExportOptions` they name), T24 SMPTE 240M / untagged / BT.2020 archive footage. Variants it cannot reach, and why: `motion`, `fade`,
`travel`, `tail`, `keyed_opacity`, `keyed_color`, `keyed_zoom`, `mask`, `text` (drawn under `CAPS`), `no_picture` (the project keeps
an audio-only asset off a video track), `text_without_font` (the shipped font cannot be written), `unmeasured_policy` (the probe
cannot tell), and per build `grade` (kernels match) or `grade_with` (kernels differ), and under
`FixedBt601` `mixed_matrices`, `mixed_matrices_with`, `matrix_unknown`, `rgb_in_stack`, `full_range_cut`, `graded_in_full_range_stack`, and `text_sequence` (a tab, over 160 characters or a canvas too wide to measure: no
archetype has one; text itself is drawn once `text::build()` has probed this FFmpeg's `drawtext`, the header prints the layout, `Compositor::caps_pinned(kernels, scaler, convert, text, blend, extras)`).

**`tests/fate_corpus.rs`** (`#[ignore]`d, and `KERF_FATE=1`: CI's parity job runs every ignored test and this one needs a network, so
without the variable it skips with a message; `ffmpeg`, `ffprobe`, `curl`, no adapter) is hostile input from FFmpeg's FATE sample
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

**Every GPU change is judged by the `parity` agent** (`.claude/agents/parity.md`, after the implementer and the verifier, before
the reviewer): it reads the C source of the filters for both FFmpeg builds, writes its own adversarial cases (tests only, never
`src/`, a shader or the implementer's cases), runs them on both builds, mutation-checks the new arithmetic, draws each refusal anyway
to see it is necessary, and reports the ledger and `coverage.rs` delta.

```bash
cargo test -p kerf-gpu --no-default-features -- --ignored        # needs ffmpeg + an adapter
KERF_GPU_ADAPTER=hardware cargo test -p kerf-gpu --no-default-features -- --ignored   # the machine's GPU
KERF_BENCH=1 cargo test -p kerf-gpu --no-default-features --release -- --ignored --nocapture bench
cargo test -p kerf-gpu --no-default-features --test coverage -- --ignored --nocapture   # frames drawn vs refused, per archetype
cargo test -p kerf-gpu --no-default-features --test coverage -- --ignored --nocapture ledger   # target/parity/ledger.txt: what refuses them
KERF_FATE=1 cargo test -p kerf-gpu --no-default-features --test fate_corpus -- --ignored --nocapture   # FATE samples (downloads into target/fate-suite/)
cargo test -p kerf-gpu --no-default-features --test motion_parity -- --ignored --nocapture   # the export's frames vs the compositor's, plane for plane
```
