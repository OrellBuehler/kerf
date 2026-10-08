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
  nothing here ever blocks an export. `Project::platform_check(frame)` resolves
  the summary from `working_timeline` (so an agent is judged on its own
  proposal) with an optional frame override, which the export dialog passes when
  a render resizes away from the project frame.
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
  one sampled transform, fades and transitions left out of the picture, half-open spans,
  text drawn statically). `Motion` is the export graph at output frame `k`: every
  expression the graph evaluates (`enable`, keyframes, the overlay position, `drawtext`) is
  read at `ffmpeg_frame_time(k)`, the slot boundary is `exact_time(k)`; an outgoing clip
  plays on its tail; a keyframed clip is never padded and always scales again
  (`LayerGeometry::resolve_with` and its `Placement`, which `resolve` defaults to
  `Placement::STILL`; a slide's travel joins the position in every branch); text is on
  `between(t,start,end)`. **A still plan is never drawn inside a transition**: it has no
  layer for the outgoing clip playing on its tail — the export keeps drawing it until its
  window closes, which is *after* the alpha ramp (the fade counts frames and rounds) and
  around an incoming clip that does not cover it (6.9 dB, max 255 at 24 fps, 0.6 s dissolve,
  frame 62) — so `RenderPlan.tails` records every clip whose tail window is open at the
  frame (read at `t` and at `ffmpeg_frame_time(frame)`) and `reasons` refuses it
  (`Unsupported::StillTail`), as it refuses a still inside a fade step or a travel,
  **whatever the caps**: `fades`, `transitions`, `keyed_*` and `motion` are about Motion
  plans (the FFmpeg still draws no fades), `mask` / `text` / `effects` / `reframe` / `hdr`
  hold for both. `frames_inside_a_transition_are_refused_and_the_ones_around_it_are_drawn`
  (kerf-gpu parity: dissolve / slide / push / dip, covering and partial incoming clip,
  frames before / inside / after, both FFmpegs) holds every listed frame to "refused, or
  the still it draws like is the export's frame and the GPU matches it", so the second half
  of a dip is a drawn frame by measurement. A Motion plan holds **candidates** at a clip's closing edge
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
  **Colour and partly-keyed transforms in the plan.** `PlanLayer.color` is `Clip::color_at(local)`, sampled
  per frame (a still plan draws it like a static grade; the ZoomBehind check reads it), and a **Motion**
  plan with a keyed colour is refused (`Unsupported::KeyedColor`, until `GpuCaps::keyed_color`: the
  file's `eq` is written per frame, and no pass or parity case draws one yet). `Animated` is `Some` for a
  keyed transform *or* colour and `Animated.keys` (`layer_geometry::Keyed {scale, rotation, rotates,
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
  **What the compositor may draw is data**: `GpuCaps` (`Compositor::caps()`, today
  `GpuCaps::A0`: `motion`, `fades`, `transitions`, `keyed_opacity`, `keyed_zoom`, `keyed_color`, `mask`,
  `text`, `reframe`, `hdr` and an `EffectKinds` bitset), and **`RenderPlan::reasons(&caps,
  size)` is a pure function of the plan's fields** returning `Unsupported` values whose
  `Display` is the message the plan has always given — nothing is decided while planning,
  so one plan answers for any caps and an A5 pass is a flip beside the pass and its parity
  cases. A Motion plan is refused as a whole until `caps.motion`, and in Motion a moving
  keyframed zoom (the export draws it at the output frame's time now, but no compositor has been
  held to it yet) and
  keyed opacity (a `geq` alpha, not the RGB round trip), a non-`yuv420p` delivery and a
  gif are refused. The old API stays: `gpu_supported()` / `unsupported_reasons()` (an owned
  `Vec<String>`) / `unsupported_reasons_at(size)` / `gpu_supported_at(size)` are
  `reasons(&GpuCaps::A0, ..)`. A still is refused while a fade step is live, a layer
  travels or a tail window is open — exactly, where A0 refused the whole
  `[start - d/2, start + d)` of a transition and missed the tail after the ramp.
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
