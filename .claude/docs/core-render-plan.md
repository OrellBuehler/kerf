# kerf-core: platform checks and the render plan

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
  nothing here ever blocks an export. `Project::platform_report(frame, range)` resolves
  the summary from `working_timeline` (so an agent is judged on its own
  proposal) with an optional frame override, which the export dialog passes when
  a render resizes away from the project frame, and an optional `range`: the span an
  export of that range writes. `Project::cut_summary_in` summarises it on the *sliced*
  timeline (`Timeline::export_span` — the one clamp the export's `effective_range` also
  reads, so an empty or inverted span is the whole cut — then `Timeline::slice`), so the
  length, the footage-derived frame, whether the span has sound and whether a title is on
  screen in it all describe the file, and a Reel cut from the middle of a long timeline is
  judged as the Reel. The MCP `platform_check` takes the same optional `range`, and
  `export_variants` judges each file over the range it was exported with. It answers a
  `PlatformReport`: every
  target's check *and* the `CutSummary` they judged, so a UI can state the file's
  frame and length without reading the project a second time. Messages say
  `Instagram Reels'` / `TikTok's` (`possessive`); `platforms.ts` mirrors it.
- `render_plan.rs` + `planner.rs` + `plan_caps.rs` — **what one frame is made of**,
  shared by every renderer. `RenderPlan::at(timeline, assets, opts, t, color)` is the
  canvas (the same `export_format` the still uses, through `render_geometry`) plus the
  ordered video layers visible at `t`: asset path, resolved source time (speed / reverse /
  clamped), the `Transform` sampled at clip-local time, `Color`, the stream's
  displayed size / rotation / transfer / pixel format / matrix. It is a one-shot
  `Planner` in `PlanMode::Still` and shares `clip_source_time` with `active_video_clips`,
  the function `build_still_args` takes its inputs from, so the FFmpeg still and the GPU
  compositor cannot disagree about which clip is where in its source (a `cli.rs` test
  pins the argv against the plan, and `planner.rs` the whole plan against
  `active_video_clips` over a busy cut); `still_size` is the preview-size rule both use.
  **The plan is complete, and a `Planner` prepares a cut once.** `Planner::new` does the
  per-cut work (`for_render`, `transition_fx`, geometry, the asset facts, each clip's
  window / fades / motion keys, fonts) and a **per-track clip index** (sorted by start,
  with a running maximum of window ends): `at(t)` / `at_frame(k)` binary-search to the
  clips that have started and walk back only while an earlier window could still reach
  the frame — `O(log n)` plus the clips on screen for a cut whose clips follow one
  another, but `O(n)` for a track with one very long clip under many short ones (the
  running maximum never lets the walk stop). `at(t)` is the output frame **on screen** at
  `t` (`Rational::frame_containing`: the slot `[k/fps, (k+1)/fps)` holds it, a hair of
  floating point included), not the nearest. A clip with no asset is an error only when a
  frame asks for it; an `fps` FFmpeg would not parse is an error in either mode. A layer carries its sampled transform and colour, `mask` (normalized), `effects`
  (a chroma key's colour made safe), `reframe` (`PlanReframe { pose, interp }`: sampled
  — the `sendcmd` schedule's held pose is for the pass that draws a reframe), `hdr`,
  `projection`, `animated` (`Animated`: which transform numbers are keyed — `keys: Option<Keyed>`, what
  `Placement` and the geometry need — which move, and whether the colour is keyed) and `fx: LayerFx` — **transitions
  are per-layer, as in the graph**: the clip's `FadeStep`s (evaluated by
  `LayerFx::strength(tint, frame, fps)`, which counts frames like `fade` does), the slide /
  push travel at this frame (`MotionKeys::at`) and whether the layer is on its `tail`; a
  dissolve is two ordinary layers. The plan holds the live `PlanText`s (colour and box
  made safe, the font file resolved once, bold known to be real or synthetic), and the
  canvas the delivery's `fps` (a `Rational`: the `color=r=` canvas's parse, the overlay's
  clock), `pick_fps` (the `fps=` filter's parse, `av_d2q` with `INT_MAX` — the grid clips
  are placed on; the same for every rate with small terms, different for `29.970029`),
  `pix_fmt`, gif and composite colour policy.
  **Two modes.** `Still` is the contract the GPU path started with (`build_still_args`:
  one sampled transform, half-open spans, text drawn statically) **and the FFmpeg still's own frame
  inside a transition** (`engine/cli/still_fx.rs`): the fades still changing the picture, counted at the
  output frame `k = frame_containing(t)`, the slide / push travel read at `ffmpeg_frame_time(k)` and an
  outgoing clip on its tail, judged at the slot's start (`exact_time(k)` plus a nanosecond: a window that
  closes inside a frame is open for it, one that ends on a boundary is closed for the frame that starts
  there; `candidates_between` looks from there, which can be before the `t` asked for).
  `the_still_plan_holds_the_layers_the_still_draws_at_every_time_of_every_transition` holds the plan's
  layers, source times, travel and running fades to `still_layers` over every kind, four rates and
  windows that close inside a frame. `Motion` is the export graph at output frame `k`: every
  expression the graph evaluates (`enable`, keyframes, the overlay position, `drawtext`) is
  read at `ffmpeg_frame_time(k)`, the slot boundary is `exact_time(k)`; an outgoing clip
  plays on its tail; a keyframed clip is never padded and always scales again
  (`LayerGeometry::resolve_with` and its `Placement`, which `resolve` defaults to
  `Placement::STILL`; a slide's travel joins the position in every branch); text is on
  `between(t,start,end)`. **Fades, tails and travel are in both plans** and a compositor says whether it
  draws them (`caps.fades`, `caps.transitions`, `caps.dip_white`); `reasons` refuses a frame inside one
  otherwise (`Unsupported::Fade` / `Tail` / `Travel` / `DipToWhite`). A fade is its **integer factor**
  (`FadeStep::factor_at_frame`: `65535 * i / N` truncated, `65535 -` that for a fade-out; `FADE_FULL` is
  "the filter does not run"), `RenderPlan::fade_ops(layer)` lists a layer's steps in chain order with
  their factors, `on_alpha_plane(layer)` says whether the chain is on `yuva420p` (a rotation, an opacity
  below 1, a mask, a chroma key, the dissolve the clip entered through, a keyed rotation or opacity in an
  export frame: `fade` takes black 0 there and 16 on `yuv420p`) and `through_rgba(layer)` whether an
  opacity below 1 left it in RGBA (`colorchannelmixer`, then `rotate` and `fade` on RGBA, alpha plane
  included). Where the still's `fade` runs on the `fps=` timebase and the canvas parses the rate
  differently, a still inside a fade is `PickRate`, as an export frame always is.
  `frames_inside_a_transition_are_drawn_as_ffmpegs_still_draws_them` (kerf-gpu parity: dissolve / slide /
  push / dip, covering and partial incoming clip, frames before / inside / after, both FFmpegs) holds every
  listed frame's FFmpeg still to the export's frame (>= 35 dB, the tail-only frame included) and the GPU to
  the still; `tests/motion_parity.rs` holds the export's own frames to a Motion plan, plane for plane
  (`gpu.md`). A Motion plan holds **candidates** at a clip's closing edge
  (the layers whose `enable` window contains the frame time, end included) and **`fps_pick`
  resolves them**: a candidate is drawn exactly when its pick names a frame
  (`rendered.rs` holds that to the pixels at five rates, and every drawn clip is planned).
  **Which frame of which file** (`frame_pick.rs`, `media.rs`):
  `PlanLayer.pick` is a `Pick` — `AtOrAfter(t)` (the still: the first frame `-ss {:.6}`
  returns; `0.0` for a still image in a still plan, a Motion plan gives it `Fps`), `Before(t)` (the frame preceding it) or
  `Fps(FpsPick)` (a Motion plan: speed, direction, the source window *with its tail*, the
  clip's start, the output frame, the `fps=` rational, and whether a padded proxy's clone is
  dropped) — and `Pick::select(&SourceFrames { pts, time_base, start_us })` is the reference
  that names an index among a file's frames (ticks, as `ffprobe` / `showinfo` state them).
  **The `fps` pick is arithmetic, not the formula the A1 design measured** (last frame with
  `pts < ws + s*((k+1/2)/fps - start)`, which is right to first order): `-ss` shifts the
  timestamps by the seek rounded to a *tick*, `trim` keeps `[lo, hi)` after both ends are cut
  to whole microseconds, `reverse` emits the frames last first **with the timestamps in
  forward order**, `setpts` evaluates in doubles and **truncates** (`D2TS` is an
  `(int64_t)` cast — a rounding put a frame a slot *late* wherever its time lands a fraction of a
  tick under a half-slot boundary), `fps`
  rounds to its slot half away from zero, and **the stream ends where the frame `trim`
  dropped would have landed** (the first frame past the window, retimed; for a window to the
  end of the file one frame interval — the file's last — past the last frame): nothing is
  drawn from that slot on (`overlay=eof_action=pass`), so a slowed clip's last frame is held
  for its whole share, the last frame of a sped-up or reversed one whose slot is past the end
  is dropped, and the equal-rate end is not drawn. That end, not the overlay's `enable`,
  decides a clip's closing edge. The time base is therefore an input (`start/TB` is
  truncated to a tick: a frame-aligned speed-2 clip's exact tie falls on the low side), and
  so is the container's start (`start_us`: `-ss` is relative to it). The export spells its
  `-ss` and `trim` with `{}` of the `f64` (FFmpeg reads whole microseconds, truncated); only
  the still uses `{:.6}`. A window that runs to the end of the file ends the stream the last
  frame's own **duration** (`SourceFrames.last_duration`: ffprobe's frame `duration`,
  `showinfo`'s `duration:`) past the last frame — not the last gap: matroska's alternate 33 and
  34 ms and its last frame lasts 33. **A still image is a stream too**: the export reads it
  as `-loop 1 -framerate <fps> -t <end>`, so `Pick::Fps` carries `image: Some(<fps>)` and
  `fps_pick` makes the run of frames up (no `SourceFrames` needed, `SourceFrames::NONE`); the
  frame `-t` cuts at ends the stream, so a still is *not* drawn on the frame its window closes
  on, and `source_in` 0.25 starts it on the first frame at or after a quarter second. `engine/cli/picked.rs` (`#[ignore]`d, both FFmpegs) renders clips
  whose frames number themselves through the real export graph and compares every output
  frame (and stills, 192 of them, by whether they are drawn): speed 0.5 to 4, forward and reverse, every phase of the grid at 24 / 25 / 29.97 / 30 /
  60 / 23.976 fps (2997/100 and 2997/125 slot boundaries), a seek off the grid, matroska
  and transport-stream time bases (the latter with a container start), a variable frame rate
  (forward and reversed), windows to the end of the file and of one frame, a dissolve's
  tail, and **`proxy/late-video-start`** (the pick over a head-padded proxy's own frames is
  what the preview graph renders from it, at the head — the clone dropped — and deeper in).
  `drop_first`'s removal makes the head case fail; so does rounding in `setpts`.
  **A late-starting source** (`FpsPick::lead`, from `ClipFx::lead`): a window inside the lead
  is not seeked (`clip_seek(start, lead)`) and the run's `setpts` start is
  `start + picture_delay(...)`, added in the graph's order (`+(start+delay)/TB`), so the frames
  before the source's first picture are `Early` (nothing drawn), as in the export.
  A rate whose canvas and `fps=` parses differ (`29.970029`) is refused in Motion
  (`Unsupported::PickRate`): the pick assumes one grid.
  **Traps for the `FrameSource` that will feed it** (A1b): `showinfo` pts under `-copyts
  -start_at_zero -ss` are already start-relative, so pass `start_us = 0`; use the decoder's
  best-effort timestamps (AVI has no frame pts); a streaming cursor needs `STARTPTS`, one
  frame of lookahead and the first frame past the window's end (or the last frame's duration at
  the end of the file) — **`Pick::progress(&read, eof)`** (A1b-1, pure, property-tested
  against `fps_pick` over the whole file) is that cursor's rule: fed the frames a run has
  produced so far it says `NeedMore`, `NeedEarlier` (a `Before` whose answer is ahead of
  the first frame read: nothing there if the run began at the file's start, else restart
  earlier — only the cursor knows which) or `Ready { shown, keep_from }`, answers exactly as the
  whole file would, and answers as early as it can (one frame past the shown one, or at the
  window's end); `FpsPick::seek()` is where the run must begin; reverse needs every pts of
  the window; a non-all-intra transport stream
  may not deliver the picked frame after an `-ss` (**both** builds — the design note's "6.1
  only" was wrong; an all-intra one is exact). **A fade on a layer with an alpha plane goes to
  luma 0, not 16** (`fade` on `yuva420p`; white is 235 either way): `FadeStep` /
  `LayerFx::strength` do not know which black, and must before A5.1 draws one (design note
  finding 19).
  **A plan describes the file that is decoded.** `PlanRequest { mode, color, media }`
  takes a `MediaResolver` (`PlanRequest::still(c).with_media(&ProxyMedia)`; the default is
  `OriginalMedia`): `SourceMedia { path, proxy, video }` is what decoding `path` yields,
  `ProxyMedia` resolves an asset like `Project::preview_source` and takes the proxy's
  stream from its sidecar (above), falling back to the original. The Planner builds the
  decoded assets once (path swapped, video stream replaced — so `ClipFx.hdr` / `head_pad`
  follow the file that is opened and `PlanLayer.stream` is the proxy's size and format, which
  is what `LayerGeometry` and the refusals judge) while **the delivery canvas still derives
  from the originals** (`render_geometry`). `PlanLayer.source: PlanSource { proxy }` says
  which. Resolve off the project lock, once per `Planner`. `SourceMedia::decoded` carries the
  original's spherical projection onto the proxy's stream (the proxy file has none of its own;
  it is the same picture, smaller). `SourceMedia` carries no `identity`: the frame cache's
  file key is `kerf_core::source_identity(path)` (`fnv1a` of `source_key`: path, size and
  modified time — one `stat`), taken on the *decoded* path when a frame is asked for, so a
  replaced file or a new proxy is a new identity at the moment it matters (A1b-1).
  **Spans and the hand-over.** `Planner::span(a, b, size, caps)` evaluates every grid frame
  of `[a, b)` and run-length-encodes `reasons(..).is_empty()` (`SpanPlan::runs`); the lazy
  `Planner::first_unsupported(a, limit, size, caps)` stops at the first frame a compositor
  with those caps does not draw (a playback loop asks it every few frames — A4 scans 5 s
  ahead), `SpanPlan::first_unsupported` answers from the runs. **`SpanPlan::handover(a)`**
  is where a stream that takes over from the compositor starts: `stream_preview` plays
  `Timeline::slice(start, ..)`, and a slice cuts the front off the clips it starts inside —
  fade-in zeroed, `transition_in` dropped — **and** shortens or drops the clip *before* a
  transition (the transition clamps to what is left of it: a dissolve shortens, a dip's
  fade-out restarts, and with the outgoing clip gone the incoming one fades up from black
  instead of crossing it). So a clip is in the way from its start to the end of its fade-in /
  slide, and one that transitions out from `lead` before its end to its end *inclusive*;
  `stream_start` is the latest time at or before `a` that no window is open at (windows chain)
  and the caller drops frames before `first_shown = a`. **`Handover.frame` is the delivery
  frame of the whole cut and the caller must pin it** (`ExportOptions::resolution` for the
  stream): in a project with no delivery frame the canvas derives from the clips on the
  timeline, so a slice that drops the clip that defined it is cut for another frame (tested). A planner test slices at the
  hand-over and holds every layer's fade, dissolve and tail state to the full cut's, and
  shows a slice started inside a dissolve has neither. `KERF_BENCH=1 cargo test -p kerf-core
  --no-default-features --release -- --ignored --nocapture bench_planning` prints
  `Planner::new` / `at_frame` / `span` / `first_unsupported` over a 500-clip, five-track cut
  (print-only; here 0.8 ms to plan the cut, 3 µs a frame with five layers on screen and 5 µs
  a frame for `span` / `first_unsupported` with `reasons` at 1080p).
  **`render_bar.rs`** (pure) is the timeline's render bar: `PlayCut::new(.., 0.0, ..)` over the whole cut, every
  grid frame judged (`at_frame`, `PlayCut::playing_refusal`, `reasons(caps, still_size(frame, width))`) and run-length
  encoded by state and kind into `BarSpan { start, end, state: gpu | stream | stall, kind, reason }`; `caps: None` is
  the GPU preview off (only stalls), `waits` the Proxy-only proxy waits; a frame with no layer and no title is no span,
  a cut with no video none at all, a rate outside 1..60 fps the stream's (`frontend-timeline.md` has the semantics).
  **`PlayCut`** (`play_plan.rs`, pure) is the cut a compositor that **plays** draws, which is what
  `stream_preview` plays: a `PlanMode::Motion` planner over `Timeline::slice(start, ..)`, so the clips'
  `fps` picks, fades and dissolves are the export's of that slice. `start` is a frame of the project's
  grid (slice frame `j` is frame `k0 + j` of the cut, so a page counting frames agrees), or the latest grid
  frame before a transition window `SpanPlan::handover` says a slice there would cut off (the caller
  draws nothing before the time it was asked for; the clips are read through). The slice is taken a
  nanosecond early (`SLICE_BIAS`): a clip placed after others sums their lengths and can start a float's
  width under the frame, and a slice that "cuts" it by that width drops its fade-in and its transition.
  The frame and the rate are the **whole** cut's, pinned in the slice's options (a slice that drops the
  first clip would derive another canvas), and a cut outside 1 to 60 fps is refused as the stream's rate
  is. `PlayCut::ahead(from, horizon, size, caps)` is the lazy scan: the first frame the compositor does
  not draw — `PlayRefusal::Plan(Unsupported)` — or draws exactly but **not in time** (`Reversed`: the cursor
  reads the clip's whole window before its first frame; `MovingText`: a title whose position is keyed
  is a new `drawtext` rasterisation per frame), with `handover`, the time a stream that carries on must
  start at (`SpanPlan::handover` of the slice's windows, in the cut's time).
  `engine/cli/sweep.rs` holds
  the Motion plan against the **evaluated** graph (a ~50-line evaluator for
  `keyframe_expr`'s grammar: zoom / rotate / opacity, the overlay's x/y against the
  layer's `origin`, travel and all, a title's position and `between`, five frame rates)
  — parsing `enable=` back out of the graph would only restate what the builder printed.
  It checks the *grammar* at the output frame's time, not the graph's clock (that the zoom is
  read at the output frame's time and shown is `keyed_zoom.rs`'s, above). **A7 concern**: a Motion
  plan decides the composite's matrix per frame from the layers on it
  (`composite_matrix`), while FFmpeg 9 negotiates colourspace across the whole graph, so
  over a cut whose bottom layer changes the matrix the export converts with may not be the
  one a frame's own layers suggest — to be measured before the GPU encodes an export.
  **The mask's alpha is shared too**: `kerf_core::mask_alpha_plane(&Mask, w, h)` is `mask_filter`'s `geq` evaluated in `f64` and
  truncated as `geq` stores it (`engine::cli::rendered`, `a_mask_plane_is_the_geq_planes_sample_for_sample`: 21 million samples equal on both
  FFmpegs), at the size of the picture the `geq` sees — `LayerGeometry::picture`, or the fit-size `stages[0].keep` when a moving zoom
  comes last — so a compositor draws a mask with no FFmpeg and no per-frame cost.
  **A grade is refused**: `PlanLayer.grade` is the clip's `Grade`, and curves, wheels or a LUT that changes the picture
  are `Unsupported::Grade(layer, what)` (kind `grade`) whatever the caps — no compositor pass or parity case draws
  `lutrgb` / `colorbalance` / `lut3d` or their RGB round trip yet.
  **Colour and partly-keyed transforms in the plan.** `PlanLayer.color` is `Clip::color_at(local)`, sampled
  per frame (a still plan draws it like a static grade; the ZoomBehind check reads it), and a **Motion**
  plan with a keyed colour is refused (`Unsupported::KeyedColor`, until `GpuCaps::keyed_color`: the
  file's `eq` is written per frame, and no pass or parity case draws one yet). A **keyed mask or crop** is the
  same: `PlanLayer.mask` is `Clip::mask_at(local)` and `PlanLayer.transform` is `Clip::framing_at(local)` (the
  keyed crop folded into the crop edges; `ActiveClip::transform` too, so the FFmpeg still draws the same), a
  still plan draws them as a static mask and crop, and a Motion plan is refused (`Unsupported::KeyedMask` /
  `KeyedCrop`, no cap: the file's `geq` reads them per frame, the crop's bilinearly, and no pass draws either).
  `Animated.mask` / `Animated.crop` say so. `Animated` is `Some` for a
  keyed transform, colour, mask or crop and `Animated.keys` (`layer_geometry::Keyed {scale, rotation, rotates,
  opacity}`) says which transform numbers are keyed, because a clip keyed in part is not built like one
  keyed in full: a keyed zoom is a second `scale` even at 1, an unkeyed one is the static scale (a second
  scale only when it is not 1); a keyed turn is the `hypot` box, an unkeyed one the tight `rotw` box; a keyed
  opacity is the `geq` alpha with no odd-size restriction, an unkeyed one the RGB round trip — so the
  matrix a translucent layer needs (`Unsupported::TranslucentMatrix`) is skipped only for a **keyed
  opacity** (`Keyed::opacity`), not for any keyed number: a clip with only its position keyed and a static
  opacity below 1 still takes the round trip.
  `Placement::keyframed` carries that (`Keyed::all(rotates)` is the legacy bundle), a colour-only clip is
  placed as a static one, and the sweep holds origin, zoom, turn, opacity and grade of the new cuts to the
  evaluated graph.
  **What the compositor may draw is data**: `GpuCaps` (`Compositor::caps()`: the bits `motion`, `fades`,
  `transitions`, `dip_white`, `keyed_opacity`, `keyed_zoom`, `keyed_color`, `mask`, `text`, `reframe`, `hdr` and an
  `EffectKinds` bitset; today `GpuCaps::A0` plus every one of them but `keyed_opacity`, `keyed_color`, `reframe`, `hdr`
  and the vignette / chroma-key kinds), and **`RenderPlan::reasons(&caps,
  size)` is a pure function of the plan's fields** returning `Unsupported` values whose
  `Display` is the message the plan has always given — nothing is decided while planning,
  so one plan answers for any caps and an A5 pass is a flip beside the pass and its parity
  cases. A Motion plan is refused as a whole until `caps.motion`, and in Motion a moving
  keyframed zoom is refused until `caps.keyed_zoom` (`KeyedZoom`: the export draws it last, at the output
  frame's time; a compositor that sets the cap draws the grade, the effects, the mask and the fades at the fit size and zooms after
  them), and
  keyed opacity (a `geq` alpha, not the RGB round trip), a non-`yuv420p` delivery and a
  gif are refused. **With `keyed_zoom` the plan still refuses a rotation, an opacity below 1 and a dip to white in front of a moving
  zoom** (`ZoomBehind(layer, what)`, Still and Motion alike: the rotated alpha plane and the RGB round trip would have to be scaled
  with the zoom), and without it a Still refuses a grade, rotation, opacity, mask or effect in front of one. The old API stays: `gpu_supported()` / `unsupported_reasons()` (an owned
  `Vec<String>`) / `unsupported_reasons_at(size)` / `gpu_supported_at(size)` are
  `reasons(&GpuCaps::A0, ..)`: a plan is refused while a fade step is live, a layer
  travels or a tail window is open — exactly, where A0 refused the whole
  `[start - d/2, start + d)` of a transition and missed the tail after the ramp. **What a compositor that
  draws fades still refuses** (specific reasons, whatever the caps): `FadeOnTranslucent` (a black or white fade running on a
  layer whose opacity is below 1: FFmpeg fades the RGBA picture `colorchannelmixer` leaves, alpha plane
  included, ahead of the conversion back to `yuva420p`; a dissolve's ramp touches the alpha alone and is
  drawn) and, with `caps.dip_white`, `DipToWhite(layer, why)`: a dip to white is `yuv420p -> rgb24 -> yuv420p` around
  the fade (a filter of every frame of the clip in an export, of the frames the fade runs in a still), measured within 2
  levels (3 on BT.709) of the compositor's tables on x86-64 (arm64 NEON untested), and only on **8-bit limited-range
  untagged / BT.601 / BT.709 `yuv420p`**. Refused, each with its own `why`: an alpha plane (FFmpeg picks another RGB
  format), a black fade of the clip running ahead of it, any other pixel format (RGB 11 / 12 levels of Y / UV off,
  10-bit 9 / 16, 4:4:4 16 / 17, 4:2:2 9 / 13, full range up to 21 — in an export's planes on 9.0.2 — or an unrecorded format) or matrix, and, from `reasons(size)`
  (the geometry): a letterboxed layer (`pad` runs before the round trip, so FFmpeg's bars whiten; Y 126 against 16 at half
  way), a picture with an odd side (the round trip reads past it), and any layer the scaler resizes (the `scale` filter hands
  the fade its `rgb24` itself, scaling and converting in one pass: Y up to 19, UV 28 off, a crop that leaves the size is exact).
  The dip-to-white refusals are the **Motion** plane bound's and are conservative in stills on purpose: bypassed, RGB, 10-bit, 4:4:4,
  4:2:2, gray, BT.2020, full-range and half-size layers all pass a still's strict bounds, but `reasons()` is one function for both modes.
  `FullRangeNegotiated(layer)` (a still): a `yuvj` (or unrecorded-format) picture inside a running fade, dip or dissolve where FFmpeg 9 works on its raw
  values and converts the range afterwards (`fade` darkens toward the limited-range 16 first: frame 0 of a fade-in reads Y 30 where black is 16).
  **`FullRangeCut`** (an export frame; a plan-level reason, from `PlanCanvas::full_range_cut`, which the `Planner` sets when any clip of the cut
  `maybe_full_range()`): FFmpeg 9 flags the whole export full range and converts every limited-range frame of it (a black frame reads Y 0, not 16), so under any policy
  but `FixedBt601` **no frame of a cut that holds a full-range clip** is drawn — before it, after it, on another track or a gap. FFmpeg 6 converts at the decode, as the
  compositor does, and draws it all to the level, except that **`FullRangeResized`** (under either FFmpeg, from `reasons(size)`) refuses a full-range
  picture the scaler resizes in an export frame and inside a fade (swscale converts the range inside the resize: chroma up to 15 levels off).
  `maybe_full_range()` is a name heuristic (`yuvj…`, or no recorded format): the range tag (`color_range`) is the truth and `StreamInfo` does not record it. Under a policy that is not
  `FixedBt601`, `maybe_full_range_in(policy)` therefore also counts any deeper-than-8-bit YCbCr / gray format (`pix_fmt_is_high_bit`): a `pc`-tagged 10-bit file probes as `yuv420p10le`, 9.0.2 handles it as `yuvj`, and the
  limited 10-bit picture is refused with it (it is what the cut flag, `FullRangeNegotiated` and the grading refusals use; 6.1.1 draws both exactly, so the name alone decides there). Only a clip the export holds sets the cut flag (not a muted track, a
  disabled clip, a track a solo shadows, or an audio track's use of the asset).
  The policy condition is a **proxy measured only on FFmpeg 6.1.1 and 9.0.2** (5.x, 7.x, 8.x and distro builds are unmeasured).
  **`gpu_supported_at(size)` is the per-frame fallback switch** (`gpu_supported()` is
  its size-free half; `unsupported_reasons_at(size)` says *no, with reasons*) for
  anything the compositor does not render exactly: video effects (`Effects(layer, what)`: names the effect and
  why — a vignette's dither is a counter that runs on across frames, a chroma key's alpha is a float colour distance — or the value
  `gblur` / `unsharp` refuse itself: a sigma outside 0 to 1024, an amount outside -2 to 5; or what the picture changes: an invert, blur or sharpen on a full-range or never-recorded picture, an invert on a gray one; and in `reasons`, by render size, a sharpen after a scale or a blur: from the first scale or blur with sigma > 0, the worst case starts at 1 and each later non-zero sharpen adds `ceil(2 * err * |amount|)`, so the frame is refused past 2: one sharpen above 0.5, or a second one), masks (`Mask` without `caps.mask`;
  `MaskWith(layer, what)` with it, on a rotated layer or one below full opacity; `Matte(layer, what)` for a blur or a sharpen on a
  letterboxed picture, judged with the geometry in `reasons`: FFmpeg's `pad` hands them the padded frame, bars included — a mask is cut
  from that frame too and is drawn at the layer's size), 360 reframe,
  HDR, a text overlay it cannot reproduce (below), a fade or transition in progress, a non-bicubic scaler, a
  layer with no known picture size, a shrink steeper than `MAX_SHRINK` (40:1, the
  steepest the scaler comparison measures), and the classes below. Pure + unit-tested
  like the rest of the timeline math. **`Unsupported::kind()`** names a refusal without its layer
  names and numbers (`mask`, `mixed_matrices`, `text_too_small`, …; an exhaustive `match`, so a new
  variant must choose one): the key the GPU preview counts refusals by (`app.md`).
  - **Text** (`caps.text`, which `kerf-gpu`'s compositor sets: FFmpeg rasterises `drawtext`, the compositor blends it —
    `gpu.md`). The font is **always a file the plan can name**: `fonts::resolve_text_font` (the installed family the overlay
    names, else the shipped Geist, `Regular` or `Bold`, written to `<cache>/kerf/fonts/`, checked on each call) is called by the graph
    (`drawtext_common`) and the planner alike, so `PlanText.font_file` is exactly what the still / export pass as
    `fontfile=` — `TextWithoutFont` is only the case where the default could not be written. `kerf_core::text_still_filter(&PlanText, h)`
    is the `drawtext` the still draws for a plan's overlay (a test holds it byte-equal to `drawtext_still` over the
    timeline's overlay at the same time) and `text_raster_graph` the graph the compositor rasterises it with (same options and
    position, white on black in `rgb24`, parts `Glyphs` and `Box`). Refused: `Text` (no `caps.text`), `TextWithoutFont`,
    **`TextBorder`** (`synthetic_bold`: a bold request in a font with no bold face — only a *named* family; the default has one) and
    **`TextTooSmall(px)`** (the smallest overlay's `fontsize`, `round(height * size)`, under `MIN_TEXT_PX` = 12, judged at the
    render size by `reasons`, not the size-free half).
  - **Animated SVG titles** (`Timeline.svg_overlays`): resvg draws them and FFmpeg overlays the pictures, so no compositor pass draws one and
    none ever will without a parity case. The `Planner` keeps their windows and `RenderPlan.svg` (`Vec<Uuid>`) names the ones on screen at a frame,
    live like a text overlay (`start <= t < end` in a Still, `<= end` in Motion: the graph's `between`); `reasons` pushes **`Unsupported::Svg`**
    (`kind` `svg`, "an animated SVG title is on screen") for any plan that names one, whatever the caps, so the frame — and a `PlayCut` scan
    that reaches it — falls back to FFmpeg. The plan carries no geometry or artwork: nothing draws from it.
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
    **The graph's BT.709 label is mirrored** (`PlanStream::label_bt709`, `engine-cli.md`): an untagged HD YCbCr layer that
    shares the export graph with an input naming a matrix is BT.709 to the plan exactly when the graph labels it, under
    every policy (it is also the matrix a translucent layer leaves YUV with on FFmpeg 6.1). A **Motion** plan takes it from
    the cut's table (`graph_bt709_labels` over every video clip of the rendered timeline: a BT.709 clip that is not on
    screen at this frame still labels the HD clip, as the export's graph does), a **Still** plan from the layers on screen
    (`model::bt709_labels` in `Planner::plan`, as `build_still_args` does over `still_layers`). Both decide from the
    **original's** video stream (`AssetFacts::original`), never the proxy's the layer describes: the proxy of a 1080p
    clip at a small Settings size is 640x360 and still labelled. So stacks of untagged HD with BT.709 or HDR footage are
    one matrix and are drawn on 9.0.2 (they were refused), and untagged HD beside `smpte170m` / `bt470bg` / `bt2020nc`
    footage is BT.709 against BT.601 / BT.2020 and refused (it was drawn as BT.601 throughout). `Unknown` still
    refuses any stack that is not BT.601. `reserved` reads as no tag (`PlanStream::matrix`, `known_color`). A PNG's `gbr` and the
    `bt470bg` of a JPEG, a WebP and the `mjpeg`, `amv`, `speedhq`, `jpegls` (and their decode-only relatives) decoders are decoders' defaults and name no matrix, so a logo,
    a photo or a webcam clip beside HD labels nothing and the plan reads those stacks as it did (a full-range H.264 clip's
    stated `bt709` is the file's word and does label).
    **An accepted difference**: the plan's decision is the cut's (a Motion plan) or the layers' on screen (a Still), while the
    FFmpeg stream a handover starts (`SpanPlan::handover`, a `Timeline::slice` from `stream_start`) decides over its own
    clips; where the slice drops the matrix-naming clip, a translucent HD layer or a dip to white in the stream is read as
    BT.601 where the plan drew it as BT.709 (the label's own effect on such a layer, the clip alone labelled against not: a translucent
    one 1 level in the export on 9.0.2 and 8 to 14 on 6.1.1 (export 8, still 14, stream 11, scrub window 14), coloured text 16 levels on its
    pixels on 9.0.2 and none on 6.1.1, a dip to white 2 levels on 9.0.2 and 15 on 6.1.1 in the export: the dip is
    `yuv420p -> rgb24 -> yuv420p`, which reads the matrix the label states). The same label is what moves an HD clip's dip in a mixed
    stack on FFmpeg 6.1.1 (33.0 dB / 15 levels into the HD clip, 32.5 / 15 out of it, over all frames of the export; method in
    `engine-cli.md`); it equals the file tagged BT.709, which the plan draws (`a_labelled_hd_clip_dips_through_white_with_the_bt709_round_trip`). Opaque pictures are identical.
  - **What depends on the render size** is decided in the plan, before any decode
    (`LayerGeometry` lives in kerf-core as `layer_geometry.rs` for this reason; the frontend's port,
    `layer-geometry.ts`, places the Preview's transform handles and replays `layer_geometry/corpus.rs`'s
    `fixtures/layer-geometry-corpus.json` — regenerate with `KERF_BLESS_CORPUS=1 cargo test -p kerf-core
    --no-default-features -- layer_geometry_corpus` after an intended rounding change): a
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
- `plan_epoch.rs` — **has anything a plan is made from changed?** A process-wide counter
  (`plan_epoch()`) that moves for every row a `Project` connection inserts, updates or deletes one by one (an
  `update_hook`, installed in `Project::init`; the `analysis`, `history` and `tasks` tables are
  skipped, they are written all the time by background work and no plan reads them). **The hook does not
  see a `DELETE` without a `WHERE`** (SQLite empties the table instead of visiting its rows): `clear_staged`
  says `WHERE 1`, and any new statement that clears a table must too. Nor does it see the row an
  `INSERT OR REPLACE` replaces, only the insert. The counter also moves whenever a
  project is opened or created, and **after** one replaces another (`Project::replace_with`, which every
  place that swaps the shared project uses, under its lock: a frame that took its inputs from the old
  project after the new one was opened would otherwise stay fresh), when a preview proxy lands (`finalize_proxy`, after the rename) or is
  deleted (`remove_proxy_files`) and when the proxy size or the preview source changes. A holder of a
  frame's inputs (the GPU preview's snapshot, `app.md`) reads it **under the project lock, before it
  reads the project**, and keeps the inputs while the counter is the same: a bump that changed
  nothing costs one rebuild, a change that does not bump is the thing it must not allow (so the rule is
  "everything but what is known to be irrelevant"). It cannot see a proxy file deleted by hand, which is
  why the snapshot is also dropped after 2 s. `Project::preview_assets_of(assets)` resolves the
  proxy-swapped assets from assets already read (it touches the disk, so off the lock), and
  `Project::proxy_waits_in(timeline, source, from, to, input)` is `proxy_waits` over a timeline and
  proxy inputs in hand, so Proxy only can refuse a frame without the lock (`proxy_waits` is the same
  function over the live project; a test holds the two equal).
