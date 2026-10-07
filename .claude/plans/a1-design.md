# A1 design: FrameSource and a complete RenderPlan

Phase 1 of A1 (`feat/gpu-a1`, off `f14e06d`), revised after critique. Design only. Names are in `crates/kerf-core/src/engine/cli.rs` unless stated.

**Measured** (FFmpeg 6.1.1 and pinned 9.0.2 unless noted; lossless 10 fps frame-number clips, luma = 16 + 4n):
1. A y4m pipe **without `-fps_mode passthrough` duplicates VFR frames to constant rate** (120 packets became 179).
2. `-vf showinfo` before `scale` prints one `n: pts:` line per frame, 1:1 with stdout, plus `config in time_base: a/b`. **Plain `showinfo` checksums every frame: +65% decode time on 6.1, +40% on 9.0.2; `showinfo=checksum=0` is free.** `pts_time` keeps 6 digits, so identity is the **integer pts in ticks**. `-copyts -start_at_zero -ss T` gives absolute pts, relative to the file's `start_time` (as `-ss` is). **`-ss` rounds to the nearest tick** (1/24: `-ss 1.02` returns the 1.000 s frame).
3. **`-ss {:.3}` skips a frame**: with a frame at 1.0006, `-ss 1.0006` returns it, the spelling `1.001` returns the next.
4. The frame the export shows is **not** "first frame >= T". Source frames 2..7: **reverse `[7,6,5,4,3,2]`** (the still's `-ss 0.8` picks 8); speed 0.5 `[2,2,3,3,4,4,5,5]`; speed 2 `[2,4,6,8]`; speed 1.5 `[2,4,5,7,8,10]`; a clip placed 0.7 of a frame late `[2,2,3,4,5]`, 0.3 late `[2,3,4,5]`; reverse at speed 2 `[9,7,5,3]`. All equal **the last frame with `pts < ws + s*((k+1/2)/fps - start)`** (`ws` window start, `s` speed, `start` timeline start, `k` output frame; the first frame if none), reverse being that rule over the window, then mirrored (`P0 + Plast - pts`).
5. mpegts seeks sloppily (`-ss 1.0` delivered the 2.002 s keyframe first); 6.1 only.
6. Proxies are build-dependent. VFR: 6.1 holds frames to constant rate, 9.0.2 keeps them. **Video starting 5 s after the container: 9.0.2's proxy keeps that start, so `-ss T` on the proxy is `-ss T+5` on the original** (6.1's agreed). Pre-existing in FFmpeg's own preview.
7. A proxy is a smaller picture than the probed stream: `Project::preview_assets` swaps `path` but keeps the original's `StreamInfo` (right for the graph; wrong for A0's y4m size check and `LayerGeometry`).
8. `fps=29.97` is the grid 2997/100, `fps=29.97002997002997` is 30000/1001; `nominal_fps` snaps jittery VFR to the former.
9. Costs (6.1): one-shot 90 ms (720p all-intra proxy) / 260 ms (1080p GOP 120); streaming 3.6 / 2.5 ms a frame.

## 1. FrameSource (kerf-gpu)

- **Run = one ffmpeg process decoding forward from a start point**, the unit of everything below: `-hide_banner -nostats -nostdin -loglevel info [-hwaccel h] -copyts -start_at_zero -ss <T:.6> -i <path> -an -sn -dn -map 0:v:0 -vf showinfo=checksum=0,scale=out_range=tv -fps_mode passthrough -f yuv4mpegpipe -pix_fmt yuv420p pipe:1`. A0's `decode_args` stays as reference and fallback; it and the still move to `{:.6}` together. The reader keeps **A0's size check** (mismatch: source `Unsupported` for 60 s) and reads straight into plane `Vec`s. A stderr thread parses only `] n:\s*(\d+) pts:\s*(-?\d+|NOPTS)`, pairs frame to line **by `n`** and re-reads every `config in time_base`; `NOPTS`, a changed time base or a count mismatch is fatal (kill, FFmpeg fallback). A **self-test on a real fine-time-base file** (production flags, exact pts expected) and the first spawn dying on an unknown flag (`-fps_mode`, `showinfo=checksum`: ffmpeg < 5.1) set a **process-wide "path disabled" flag**, in both roles.
- **Runs, not decoders.** Per file up to 3 runs, globally <= 6. A request goes to a run **behind it within `skip` frames** (reads forward, every frame cached); else it restarts the file's LRU run (spawning one under the caps, evicting the global LRU idle run). `skip` is **24 on a proxy, 96 on an original** (restart / frame cost, finding 9). Interactive backward restarts seek 15 frames early. **Prefetch** (`Forward`) starts a spare run at the next clip's first frame. **`Exact` (agent frames) never evicts a run**: one-shot or temporary run. **Thrash guard**: > 4 restarts/s on `Forward` is `Err(Busy)`, the caller falls back to `stream_preview`. `FrameCursor` (A7) is an exclusive run handle. Read-ahead is bounded **in bytes** (48 MiB a run): the reader blocks, the pipe fills, ffmpeg idles.
- **Identity and pick.** `FrameKey { source, pts }`: `source` = `fnv1a(source_key(path))` (path, size, mtime: a replaced file or new proxy never serves stale frames; proxy and original distinct) + `DecodeFormat` (`Yuv420p8`); `pts` in ticks. Two queries: `AtOrAfter(T)` = first frame with `pts >= round(T x tb)` (the still, `-ss`) and `Before(T)` = the frame preceding it (its `prev_pts`). Each cached frame stores `prev_pts` (a run's first: its rounded seek tick, clamped to one frame interval per finding 5), so both are one `BTreeMap` range query, exact on VFR. Motion's pick is finding 4's `fps` rule, **not** "the frame containing T" (wrong for reverse, speed != 1, phase >= 1/2). Past the last frame: `eof_after`, `Ok(None)` (draw nothing, as FFmpeg). Stills: one `-i` decode without `-ss`, cached.
- **Cache.** `BTreeMap<(source, pts), { Arc<YuvFrame>, prev_pts, tick }>`, byte-capped (256 MiB, `KERF_FRAME_CACHE_MB`), LRU by O(n) scan like `cli.rs`'s `FrameCache`; `Arc` frames outlive eviction while composited.
- **Proxy, original, 10-bit.** `FrameSource` decodes the `SourceMedia` it is handed (§2). **Stay on 8-bit y4m**: a 10-bit source is reduced where the graph reduces it (A0 `source/10-bit` 48.9 dB); HDR stays refused (A5.6); P010 only for A5.6 and A7 *10-bit delivery* (refused until then). No pre-scaling in ffmpeg (double scaling against the swscale port). **Proxy timing is bug-compatible**: the same `-ss` on the same proxy as FFmpeg's preview, so parity holds; fixture `proxy/late-video-start` pins finding 6 on both builds. The fix (offset by original minus proxy `format.start_time`) must also reach FFmpeg's preview: a separate WP, flagged.
- **hwaccel, CPU.** `decode_hwaccel()` at spawn; a run dying before its first frame is retried once in software, success calls a new `kerf_core::disable_decode_hwaccel()` (`HWACCEL_OK` is private today). Interactive role: **no `cpu::lease`**, normal priority, `limit_ffmpeg_args(args, live_runs + 1)` computed **at spawn**. Export role (A7): niced under the driver's reentrant lease.
- **Threads, lifetime, failure.** `Arc<FrameSource>` (Send + Sync), one per process, owned by the render service beside the project, never behind its lock; blocking calls (blocking-pool threads only) on a per-file mutex + condvar; `frames()` fans layers out with `thread::scope`; the same frame twice is one decode. The reaper holds a `Weak` and kills runs idle > 20 s; `release(source)`, `release_all()` (app exit) and `Drop` kill and wait every child. Deadlines: first frame 30 s, between frames 15 s (`PREVIEW_TIMEOUTS`), interactive request 5 s. `Decode` / `Unsupported` / timeout / `Busy`: the caller renders **that frame** (or span) through FFmpeg; `Ok(None)` is "no frame".
- **Deferred to A2/A3**: pooled GPU textures, readback buffers, an upload cache.

## 2. RenderPlan, complete (kerf-core)

**Two modes.** `PlanMode::Still` is A0's contract (`build_still_args`); every A0 call and test unchanged. `PlanMode::Motion` is "the export graph at output frame `k`": `Planner::at_frame(k)`, with `PlanCanvas.fps` a **rational** parsed the way ffmpeg parses the `fps=` string the graph carries (finding 8) and `k*den/num` computed exactly (NTSC slot-boundary tests). Its deltas over the still, all in the code today: (1) outgoing clips play on `fx.tail`; (2) fades, dips, dissolve are timed from `timeline_start` with `dur` including the tail; (3) slide/push offsets join the position (`motion_expr`); (4) a keyframed clip never pads, always runs `scale eval=frame` (moving `second_scale`, hence `LayerGeometry`'s 4:2:0 point) and turns in a `hypot(iw,ih)` box when its keys rotate: `PlanLayer.animated`, and `LayerGeometry::resolve` gains `identity` and `motion` arguments whose defaults reproduce A0; (5) **keyframed opacity is a `geq` alpha, not the RGB round trip** (other arithmetic: refused until measured); (6) overlays are `between(t,start,end)`, end inclusive; (7) source frames follow the `fps` rule (finding 4), not `-ss`; (8) reframe is a `sendcmd` schedule (0.05 degree gate, held values, `cubic`; the still samples `line`), computed **once per clip in `Planner::new`**; (9) tone mapping sits after the fit scale, the still prefixes it; (10) `PlanCanvas` carries the output `pix_fmt` and gif: the GPU draws 8-bit 4:2:0 delivery only. The preview stream's `bilinear`/`yuvj420p` is an MJPEG-pipe artefact, **not** reproduced. Whether scrub adopts Motion is A3/A5.1's call (an unsupported frame falls back to a still that draws no fade).

**No transition node; transitions are per-layer, as in the graph.** `transition_fx` only decorates clips, so each side's layer carries its half: a dissolve is two ordinary layers (outgoing on its tail at alpha 1, incoming on an alpha ramp, or from black across a gap), a dip two fades (`d/2` a side), slide/push two offsets, no source handle a hard cut. `ClipFx` / `transition_fx` / `clip_source_window` / `clip_seek` move to a pure `clip_timing.rs` with `ClipTiming::{visible, fades, motion_at}`: the builders *format* from it, the plan *evaluates* it. `fades` is **video-only**: `audio_clip_chain` composes differently (`fi` adds `afade_in`, `fo` adds the tail) and stays put.

```rust
RenderPlan { time, mode, canvas: PlanCanvas /*+ fps: Rational, pix_fmt, policy*/, layers: Vec<PlanLayer>, overlays: Vec<PlanText> }
PlanLayer  { ..A0.., source: PlanSource { identity, proxy }, /* `stream` describes the DECODED file */ animated: bool,
             pick: Pick, fx: LayerFx, mask: Option<Mask> /*normalized*/, effects: Vec<VideoEffect> /*safe_color*/,
             reframe: Option<PlanReframe { pose, interp }>, hdr: Option<Hdr> }
Pick       { AtOrAfter(T) /*Still*/, Before(T), Fps { speed, reverse, window: (f64, f64), start, k } /*Motion*/ }
LayerFx    { fades: Vec<FadeStep { colour, st, d, progress }>, dissolve: Option<f32>, motion: (f64, f64), tail: bool }
PlanText   { id, text, size /*frame fraction*/, pos, alpha /*TextOverlay::sample*/, color, bg /*valid_color*/,
             font_file: Option<PathBuf> /*resolve_font_file; None is never drawn*/, synthetic_bold }
```
`fps_pick` (pure, kerf-core) resolves `Fps` to a query: forward = `Before(bound)` (finding 4); reverse = that rule over the window, mirrored (exact on constant rate; reversed VFR is refused). The export cursor of a reversed clip buffers its window as the `reverse` filter does, up to a byte cap, and serves it mirrored; a longer window is refused (FFmpeg renders it). `progress` is a pure function of `t`, `st`, `d`; A5.1 ports `fade`'s integer arithmetic. Masks are fractions of the *layer* frame.

**`SourceMedia` fixes finding 7.** `SourceMedia { path, identity, proxy, video: StreamInfo }` is what decoding `path` yields; `trait MediaResolver` has `OriginalMedia` (A0) and `ProxyMedia`, which reads a **`StreamInfo` sidecar (`<hash>.json`) that `generate_proxy` writes after the encode** (on its background thread). A proxy without one resolves to the original while a background probe backfills it: **no ffprobe runs on the interactive path**. The canvas still derives from the original's streams.

**`gpu_supported` as data.** `GpuCaps` (all false = A0; `effects` a per-kind bitset: blur, sharpen, grayscale, invert, vignette, chroma key) comes from `Compositor::caps()`. Reasons are a **pure function of plan fields**, `RenderPlan::reasons(&GpuCaps, size) -> Vec<Unsupported>` (an enum whose `Display` keeps today's messages), not strings stored while planning; `gpu_supported_at` is `reasons(..).is_empty()`. An A5 pass is a caps flip beside the pass and its parity cases:

| item | plan carries | GPU today | until |
|---|---|---|---|
| layers, transform, crop, fit, `eq`, opacity, stills, sampled keyframes | yes | yes (A0) | |
| fades, dips, dissolve, slide/push, tails, Motion picks | `LayerFx`, `Pick` | refused | A5.1 |
| mask / text / effects+chroma / reframe / HDR | `mask` / `PlanText` / `effects` / `reframe` / `hdr` | refused | A5.2 to A5.6 |
| non-bicubic scaler, shrink > 40:1, non-4:2:0 resize, unknown matrix, non-8-bit-4:2:0 delivery | A0 reasons | refused | unchanged |

**`Planner` and `span`.** `Planner::new` computes `for_render`, the asset map, `transition_fx`, geometry and reframe schedules once, plus a **per-track sorted clip index**, so `at` is O(log n + active). `span(a, b, size, caps)` evaluates every grid frame and run-length-encodes `reasons(..).is_empty()`; **`first_unsupported(a, limit)` short-circuits** at the first bad frame within `limit` seconds (A4 scans 5 s ahead of the playhead; a bench line pins `at` on a 500-clip timeline). Per frame, not breakpoints: support flips *inside* a keyframed span (opacity through 1.0, scale through 40:1).

**Hand-over to the stream.** `SpanPlan::handover(a) -> (stream_start, first_shown)`: `stream_preview` plays `Timeline::slice(start, end)`, which drops outgoing tails and clears `transition_in` of a front-cut clip, so a stream started inside a transition would cut it. `stream_start` is the latest time <= `a` with no transition or fade window open; the callback drops frames before `first_shown = a`. A4 plans the *unsliced* timeline. `for_render` / `slice` / `for_delivery` otherwise stay caller-side transforms, as `build_export_args_phase` and `render_variants` apply them before the graph.

## 3. Agreement tests, and proof export argv does not move

1. **Machine-independent golden oracle: its own first slice and PR, before any extraction** (landed as `engine/cli/golden.rs`, a child of `cli` so it reaches the private builders). The builders read the machine: `build_preview_args` calls `decode_hwaccel()` (`KERF_HWACCEL`, `HWACCEL_OK`), HDR chains call `zscale_available()` (probes the binary), `drawtext_common` embeds a `resolve_font_file` path. Seams (the only production edits, argv-neutral): `build_preview_args_with(.., hwaccel)` (the old name wraps it), a `cfg(test)` thread-local zscale override, a generator that names no font; HDR cases run **both ways**. A seeded xorshift makes 4000 cases (non-dyadic reals: 0.1, 1/3, speed 0.7) over everything touching `video_clip_chain`, `audio_clip_chain`, `transition_fx`, `build_filter_complex`; a **coverage table** (`family needle`: text that must appear in a case's argv, plus the transition branch `transition_fx` took) requires every family in >= 20 cases, so it cannot quietly stop covering. FNV-1a digests live in **three files** (export, still, preview), 40 blocks of 100 cases each, so the `{:.6}` re-bless touches only the still file; `KERF_GOLDEN_CASES=<file>` writes one digest line per case (diff base against change to find the case in a failing block) and `KERF_GOLDEN_DUMP=<n>` prints one case's argv. **Blessed at `f14e06d`'s builders three times, `KERF_HWACCEL` unset, `none` and `auto`, digests identical**: the independence proof.
2. **S1 re-bless, deliberate**: the first commit of the next slice moves the still and `decode_args` to `-ss {:.6}`; only the still digests change, and the progress log says so. The extraction commit after it changes no digest.
3. **Grid sweep (pure) on shared fixtures** (the `cli.rs` helpers and cases for positions, layers, speed, reverse, slice, mute/solo, transitions, delivery format and variants move to `engine/test_support`). Per fixture, every `t` on a 50 ms grid plus each window edge +/- 1e-6: parse `enable='between(t,s,e)'`, `trim`, `-ss`, `setpts`, `fade=` out of `build_filter_complex` and assert the Motion plan holds exactly those clips in order with the same `FadeStep`s; a ~60-line evaluator for the `keyframe_expr` grammar checks `transform_at`, overlay x/y/scale/rotate/alpha, `motion_expr` and drawtext against `TextOverlay::sample`.
4. **The pick is tested on rendered frames, not graph strings** (`#[ignore]`d, both FFmpegs): frame-number clips through the real export graph over speed 0.5 / 1 / 1.5 / 2 / 4, reverse, clip phase 0 to 0.9 of a frame, a VFR clip and 29.97 slot boundaries, comparing the rendered frame numbers with `fps_pick`.
5. Every A0 plan test and `the_render_plan_and_the_still_args_agree_on_layers_timing_and_canvas` pass untouched; `slice(a, b)` planned at `t'` equals the full plan at `a + t'` except the documented edges; `for_delivery(9:16)` gives the framing crop and canvas; `reasons` per caps value; `span` / `handover` on fade, dissolve, overlay and keyed-opacity fixtures.
6. **FrameSource (A1b).** Pure: y4m reader, showinfo parser (fixtures from the final flag set on both builds, with 9.0's `User Data=` noise), cache (bytes, LRU, coverage, eof), router, thrash guard. `#[ignore]`d on real ffmpeg: frame-number clips at 29.97 CFR, VFR and mpegts (`AtOrAfter` equals `decode_layer` byte for byte for 50 random `T`); sequential playback restarts once; two clips of one file use two runs; `Exact` evicts nothing; a fake `KERF_FFMPEG` that hangs, dies or rejects a flag. Parity: the A0 table through `FrameSource` is **byte-identical** to the one-shot path, plus a `proxy/` family (reference `export_still` over proxy-swapped assets; `generate_proxy` with `XDG_CACHE_HOME` in the tmpdir; includes `late-video-start`).

## 4. API sketch

```rust
// kerf-core
pub enum PlanMode { Still, Motion }      pub struct GpuCaps { fades, transitions, mask, text, reframe, hdr: bool, effects: EffectKinds } // ::A0
pub struct PlanRequest<'a> { mode, color: CompositeColorPolicy, media: &'a dyn MediaResolver }
impl Planner { pub fn new(&Timeline, &[Asset], &ExportOptions, PlanRequest) -> Result<Self>;
               pub fn at(&self, t: f64) -> Result<RenderPlan>;  pub fn at_frame(&self, k: u64) -> Result<RenderPlan>;
               pub fn span(&self, a: f64, b: f64, size: (u32, u32), caps: &GpuCaps) -> SpanPlan; }
impl SpanPlan { pub fn first_unsupported(&self, a: f64, limit: f64) -> Option<f64>; pub fn handover(&self, a: f64) -> Handover; }
impl RenderPlan { pub fn reasons(&self, &GpuCaps, size: (u32, u32)) -> Vec<Unsupported>; }
RenderPlan::at(tl, assets, opts, t, color)   // A0 signature, = Planner(Still, OriginalMedia)
// kerf-gpu
pub enum Hint { Exact, Scrub, Forward { fps: f64 } }
impl FrameSource { pub fn new(FrameSourceConfig) -> Arc<Self>;
    pub fn frames(&self, &[PlanLayer], Hint) -> Result<Vec<Option<Arc<YuvFrame>>>, GpuError>;   // Err(Busy) = fall back
    pub fn prefetch(&self, &[PlanLayer]);  pub fn cursor(&self, &SourceMedia, from: f64) -> Result<FrameCursor, GpuError>;  // A1b-3
    pub fn release(&self, SourceId);  pub fn release_all(&self);  pub fn stats(&self) -> SourceStats; }
impl Compositor { pub fn caps(&self) -> GpuCaps;
    pub fn composite(&self, &RenderPlan, &[Option<Arc<YuvFrame>>], (u32, u32)) -> Result<RgbaFrame, GpuError>;
    pub fn render_plan_with(&self, &RenderPlan, (u32, u32), &FrameSource, Hint) -> Result<(RgbaFrame, RenderTimings), GpuError>; }
```
**Callers.** A2/A3: `at(t)`, `frames(.., Scrub)`, `composite` (A2 adds a texture sink beside the readback). A4: `span` chunks, `prefetch`, `Forward`, `first_unsupported` / `handover` (and `Busy`) to `stream_preview`. A5.x: caps flip + pass + parity case. A6: `at` + `Exact`. A7: `Motion` plans over `for_delivery`/`slice`d timelines, a `FrameCursor` per source, `composite`, readback, encoder pipe.

## 5. Risks, and out of A1

- **showinfo is a log channel**, verified on 6.1.1 and 9.0.2 only: self-test, `n:` pairing, fatal `NOPTS` and the disabled flag bound it; a build without it disables the path, never the app.
- **Which source frame** is a contract (`-ss` pick for A0/A3, the `fps` rule for A7); VFR proxies and late video starts are FFmpeg's own preview mismatch (finding 6), pinned by fixtures.
- **Memory and bandwidth**: a 4K frame is 12 MB (~370 MB/s at 30 fps through the pipe); byte caps, <= 6 runs and proxies contain it.
- **Out**: zero-copy hardware frames; surfaces (A2); the audio-clock loop (A4); GPU passes for fades, masks, text, effects, reframe, HDR (A5; A1 carries the data and refuses); P010/16-bit; GPU pooling and upload cache (A2/A3); chunked backward GOP decode for reverse; FFmpeg's still drawing fades; replacing `stream_preview`; disk frame caches; any change to export argv.

## 6. Work split (each slice <= ~1,500 changed lines, mergeable alone)

| slice | content | ~lines |
|---|---|---|
| **A1a-0** | golden oracle alone: seams, generator, coverage assert, three digest files, blessed under three `KERF_HWACCEL` settings | 800 |
| **A1a-1** | `{:.6}` re-bless commit, then `clip_timing.rs` extraction (builders format from it) + shared fixtures; no digest change after the re-bless | 800 |
| **A1a-2** | `Planner` (per-track index, `at_frame`, rational fps), plan types, `GpuCaps`, `reasons`, `LayerGeometry` `identity`/`motion`, grid-sweep tests | 1,200 |
| **A1a-3** | `Pick` + `fps_pick`, frame-number render test, `SourceMedia` + proxy sidecar + `proxy/late-video-start`, `span` / `handover`, bench line; `CLAUDE.md` plan section | 1,100 |
| **A1b-1** | kerf-gpu pure pieces: byte-capped cache, y4m plane reader, showinfo parser, router, thrash guard (unit-tested); kerf-core `disable_decode_hwaccel`, `source_identity` | 950 |
| **A1b-2** | `FrameSource` (runs, spawn, self-test, reaper), `render_plan_with`, parity through `FrameSource` + `proxy/` family on both FFmpegs, bench lines; `CLAUDE.md` kerf-gpu section | 1,300 |
| **A1b-3** | `FrameCursor` + reverse window cursor, real-clip pick tests | 700 |
