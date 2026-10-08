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
draws, `GpuCaps::A0` today, which `composite` / `render_plan` judge the plan by), and a lost device is tracked — every
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
(`limit_ffmpeg_args(args, share)`). That is the one-shot path; `FrameSource` (below) is the
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
target in RGBA, BGRA and sRGB formats.

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
  ffmpeg idles; `release(source)`, `release_all()` and `Drop` kill every child. A run that closes
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
  output and never exits (`release` must not wait on it) and a second run whose pictures differ
  from the first's at the same pts (the file goes one-shot, no panic).
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
in `tests/cursor.rs`'s microsecond mp4 differ between the two spellings; it holds the export's). `cursor.pick(&Pick)` reads until `Pick::progress`
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
contrast + saturation only, warm / cool), PNG and JPEG stills, speed / reverse / keyframes (a zoom
and a position moving; a rotation and an opacity moving over a held zoom),
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
format unrecorded, a translucent odd layer, a moving zoom behind a rotation or a grade) —
each of which FFmpeg still renders. 160
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
