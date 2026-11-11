# kerf-core: platform checks, the cut review and the render plan

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
- `review.rs` — **whether the editing is clean** (the MCP `review_cut`, Tauri `review_cut`). Pure `review(inputs,
  range, checks)` over `ReviewInputs` (`Project::review_inputs`: the working timeline, the assets and the cached
  analysis of the assets it uses, resolved under the lock; the review runs without it), on `for_render` at the
  export rate (`delivery_rate`). Issues reuse `platform::Severity` and carry `kind` (the `Check`), `time` / `end`,
  `clip_ids`, `message` and a `fix` naming the tools. Each check is tuned to stay quiet on ordinary edits, thresholds
  as constants: **`flash_frame`** a visible clip (footage, still or title) on a video track under `FLASH_FRAMES` (3);
  **`mid_word_cut`** a *sounding* clip's edge (`source_audio` and an audio stream, so a detached picture leaves it to its
  sound) whose source time (through speed / reverse) is `WORD_TOLERANCE` (40 ms, `ESTIMATED_WORD_TOLERANCE` 120 ms
  and a tip for estimated words) inside a word of at most `MAX_WORD_SECS`, not in a silence, not at the file's own
  ends and not a split nothing was taken out of (`continues`: same asset and speed, contiguous source);
  **`hard_audio_cut`** sounding clips meeting on a lane (half a frame) with no fade, transition or zero gain and
  sound on both sides by silence analysis (`EDGE_SILENCE_MARGIN`); pairs without it are not guessed — all unjudged is
  `skipped`, some is one tip counting them; **`jump_cut`** (tip) one asset's footage resuming on a video lane more than a
  frame and at most `JUMP_MAX_SOURCE_GAP` (8 s) later with no transition, no scene change between and no reframing
  (`PUNCH_IN_SCALE`, a move, crop, keyed transform or other 360 view; a live behaviour, a shake or a breath, is a look laid
  over the same framing and does not count, `reframed` reads the clips' keys, not `is_animated`); **`stale_captions`** — the model keeps no
  options or revision stamp for captions, so it compares words: a generated caption whose words appear as a run in
  the cut's transcripts (else it is imported or foreign and not compared; none are → `skipped`) is stale when fewer
  than `CAPTION_MATCH` of its words are spoken within `CAPTION_SLACK` (0.6 s) of it — reaching further past a clip
  edge the caption was cut short at — and speech the default captions would cover now with no caption near it, in
  runs of `UNCAPTIONED_MIN_WORDS`, is footage added after captioning; one issue each; **`black_gap`** no visible clip
  on any video track (more than half a frame, not under a typed title or SVG title): a warning inside, a tip
  after the last picture while sound runs on; **`unstabilized`** a stabilized clip that plays and exports as shot (its
  status from `ReviewInputs.stabilization`, read from the analysis cache when the review runs: `missing`, `queued`,
  `analyzing`, `failed` or `unsupported`), a warning over the clip naming `stabilize_clip`. The same kind at the same moment in the same footage (a picture and
  its sound doubled, a linked pair) merges into one issue naming every clip. Tests in `review.rs`.
- `render_plan.rs` + `planner.rs` + `plan_caps.rs` — **what one frame is made of**,
  shared by every renderer. `RenderPlan::at(timeline, assets, opts, t, color)` is the
  canvas (the same `export_format` the still uses, through `render_geometry` — so its `fps` is the delivery
  frame's rate when the project states one) plus the
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
  **A title clip** (a generated clip, `core-model.md`) is no layer: a frame it is on names it in `RenderPlan::svg` after the
  floating titles (a `PlanSvg` with `title: None`), by clip id, so the compositor refuses the frame (`Unsupported::Svg`) and FFmpeg
  draws it from resvg's pictures.
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
  **No tick arithmetic can wrap or panic**, whatever a file or a clip brings (pts and the container's start at the ends of the
  `i64`, a last duration of `i64::MAX`, an in-point of 1e300 seconds that saturates the seek): every difference of timestamps
  goes through `diff`, every sum through `sum` (saturating; the seek and the container's start are summed exactly in an `i128`
  and clamped), and a sum that fits is the plain operator's, so a pick over timestamps that overflow nothing is what it always
  was (`checker_a_gates`: 300,000 random picks at the ends of the integers and of the floats, no panic, and `fps_pick`,
  `Pick::select`, `progress` over the whole file, `output_index` and `fps_span` agree).
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
  drawn from that slot on (`overlay=eof_action=pass`). That end, rounded, came up to a source frame
  short of the clip's end, and a clip lost its last frame wherever its window's first frame fell
  late enough: a black frame at a cut halfway between two frames (12.75 s at 30 fps), and at a
  slowed or low-rate clip's end on the grid. So a forward clip's picture **reads on past its
  window** (`picture_window`: one source frame, `ClipFx::frame`, a tenth of a second when the
  probe has no rate, plus one output frame at its speed; a still's `-t` with it) and **is cut at
  `end_frame`** (`trim=end_pts=` after `fps`: the first output frame at or after its end, tail
  included, with a millionth of a frame of slack). `FpsPick.window` is the picture window and
  `FpsPick.end` that end; the run ends at the earlier of the two. **`FpsPick.pinned`** is a chain whose `fps` is pinned to the clip's start
  (`fps=fps=R:start_time=S`: a moving zoom or position, a keyed crop, a moving still): `start_time` makes the filter round each input time relative to the
  start, put the first frame out from it and end the stream at the later of the status's rounding and the last frame's offset slot (`gpu.md`, "Which frame is drawn"),
  which is not the plain `round(pts * rate)` on a tie — the plan sets it from `Animated` (`places || crop || still_motion`). A clip is drawn on every frame
  from its first frame's slot up to the one before its end's, an equal-rate end is not drawn,
  and the frame on a mid-frame end shows the nearest source frame, never one more than half an
  output frame past the clip's own length. A reversed clip keeps its window (its read past the end
  would be below its start, under the seek): its closing edge is the stream's, as before.
  (`tpad=stop_mode=clone` would hold the last frame instead, but FFmpeg 4.4 crashes on a clone of a
  stream that had no frame.) The time base is therefore an input (`start/TB` is
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
  A blurred-fill delivery (`Fit::Blur`) is judged **per layer and by the render size**: `PlanLayer::blur_fill` is `Some(picture)` for a clip
  the fill suits (`engine::cli::blur_fill_clip`: identity transform, not keyed, no reframe, no mask, no alpha; the picture is the original's,
  which the graph judges the shape by), `RenderPlan::blur_fill(layer, size)` resolves the thumbnail chain (`blur_fill::BlurFill`: the cover, the
  crop, the radius) when that picture is not the frame's shape, and `reasons` asks a compositor with `caps.blur_fill` **and** `caps.exact_scaler`
  for the layer that leaves bars (`geom.matte`): `Unsupported::BlurFill` without them (once a plan), `BlurFillWith(layer, why)` for a picture that is not
  8-bit `yuv420p` (or never recorded), a full-range one, an HDR one, a thumbnail shrunk past 63:1 (`MAX_SHRINK`) or a frame whose chroma lines are
  shorter than `2r + 1` (`BlurFill::resolve`), or **a picture of a degenerate aspect**: a thumbnail cover side past `MAX_COVER` (16384; a 1x100000 picture
  covers a 240-wide thumbnail with 24 million rows, and its scalers' tables would be as many) is `resolve`'s `Err`, a `BlurFillWith` of its own. (`MAX_SHRINK` is
  judged in 64 bits: a hostile size times 63 overflowed 32.) A clip the fill does not suit is placed by Contain's chain, byte for byte, and is not refused for the
  delivery's fit. `LayerGeometry` still places the picture as under `Contain` (and so does its port, `layer-geometry.ts`, `croppable`: the corpus
  replays blurred cases); the compositor makes the whole padded frame of it.
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
  window's end); **`Pick::output_index(&read)`** is how many frames the clip's chain has passed on
  before the shown one (a still's pick 0: the still graph runs one frame through it; an `Fps` pick its
  slot counted from the window's first frame's, any speed or direction), which a filter that counts
  frames needs (`vignette`'s dither, `gpu.md`); `FpsPick::seek()` is where the run must begin; reverse needs every pts of
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
  `BarRequest::ask` is the host's hook, called with a frame's plan before it is judged when the layers on screen are not the
  previous frame's (and only with `caps`): what it measures or asks of a file for the plan to read from a cache, which for the GPU
  preview is `kerf_gpu::prepare_ranges` (a placed layer's range, below).
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
  is a new `drawtext` rasterisation per frame; `AnimatedTitle`: a floating SVG title on a frame whose picture is not the one the frame before it showed — `SvgTitle::changes_within`, the document's text at the two moments, only for a title whose source `may_animate` — is a resvg draw and an `ffmpeg` conversion per frame; a title that cannot move, or holds still between its moves, is drawn), with `handover`, the time a stream that carries on must
  start at (`SpanPlan::handover` of the slice's windows, in the cut's time). `PlayCut::resume(from, reach,
  horizon, size, caps)` is its converse: the first frame within `reach` from which every frame for `horizon` is
  drawn (frames planned in order, each once), where a play the stream took over can be handed back to the compositor.
  `engine/cli/sweep.rs` holds
  the Motion plan against the **evaluated** graph (a ~50-line evaluator for
  `keyframe_expr`'s grammar: zoom / rotate / opacity, the overlay's x/y against the
  layer's `origin`, travel and all, a title's position and `between`, five frame rates)
  — parsing `enable=` back out of the graph would only restate what the builder printed.
  It checks the *grammar* at the output frame's time, not the graph's clock (that the zoom is
  read at the output frame's time and shown is `keyed_zoom.rs`'s, above). **A7 concern, fixed**: a Motion
  plan used to decide the composite's matrix per frame from the layers on it (`composite_matrix`), while FFmpeg 9 negotiates colourspace across the
  whole graph — the first clip's tag is the composite's for the cut and every clip of another matrix is converted into it wherever it is on the
  timeline, so a lone clip of another matrix was drawn as a plain copy up to 31 levels of Y and 18 of UV off (9.0.2, measured). `PlanCanvas::cut_matrix`
  is now the cut's matrix (under `BottomLayerTag`, in a cut of mixed matrices) and the layers of another matrix are converted (`MixedMatricesWith`, `matrix_reasons`);
  `PlanCanvas::mixed_matrices_cut` is still the cut-level fact the graded layer is refused by (`GradeWith`).
  **The mask's alpha is shared too**: `kerf_core::mask_alpha_plane(&Mask, w, h)` is `mask_filter`'s `geq` evaluated in `f64` and
  truncated as `geq` stores it (`engine::cli::rendered`, `a_mask_plane_is_the_geq_planes_sample_for_sample`: 21 million samples equal on both
  FFmpegs), at the size of the picture the `geq` sees — `LayerGeometry::picture` (for a placed layer the fit-size picture, its geometry ends there) —
  so a compositor draws a mask with no FFmpeg and no per-frame cost. Its rows run on SSE2 / AVX2 where the CPU has them (`kerf_core::simd`, `gpu.md` *CPU-side work and SIMD*), the scalar function is `mask_alpha_plane_scalar`, and every path is held to it byte for byte.
  **The clip's grade (`PlanLayer.grade`: LUT, curves, wheels) is drawn by what the caps say, and judged for the chain**:
  `GpuCaps::grade` is a `GradeKinds` bitset (`WHEELS`, `CURVES`, `LUT`, `ALL`; `A0` has none), and a part the compositor
  does not draw is `Unsupported::Grade(layer, what)` (kind `grade`) — which is also what a compositor whose FFmpeg's conversions are not (or not yet known to be) the ones it follows says:
  `Compositor::caps()` is `CAPS` less `grade` until `kerf_gpu::grade::measured()` is `Match`, so the plan refuses a graded layer before anything is decoded. FFmpeg's grade is a round trip through RGB (`yuv420p` to
  `rgb24` by swscale's x86 kernel, `lut3d`, `lutrgb`, `colorbalance`, and back by the scaler whose vertical filter has an x86
  kernel of its own), so what a compositor reproduces is bounded by where that was measured, and the rest is
  `Unsupported::GradeWith(layer, what)` (kind `grade_with`): a picture that is not 8-bit limited-range `yuv420p` (`grade_chain_refusal`),
  a colour matrix with no measured round trip (BT.601, BT.709 and untagged are drawn: `PlanLayer::grade_route()` is the
  matrix and whether the grade ends in `scale=out_color_matrix` to 4:4:4, which is when the stream names a matrix or the graph
  labels it BT.709, `PlanStream::grade_tag()`), a video effect other than a chroma key after the grade (they run on the RGB or 4:4:4
  picture FFmpeg negotiates), a fade to black or white running on it (a clip's own `fade_in` / `fade_out` is one on any track; a dissolve's alpha ramp is not), a dip to white,
  **a cut that mixes colour matrices** under a negotiating policy (`PlanCanvas::mixed_matrices_cut`: a Motion plan's cut holds video inputs of more than one `PlanStream::matrix()`, the BT.709 label included; FFmpeg 9 converts the grade's way back into the bottom layer's matrix
  on every frame of the clip, whether or not the two are on screen together), a layer the compositor's scaler resizes
  unless `caps.exact_scaler` and an `eq` precedes the grade (`RenderPlan::eq_in_chain`: without one the fit `scale` converts to RGB in the same pass;
  without the exact scaler the scaler's one-level difference becomes 3 or 4 through the conversions, past the bound of 2), an odd side (swscale reads
  past the line), and in `reasons` the geometry ones `Matte(layer, "a grade")` (a letterbox: the bars are part of the frame the
  grade sees). (A moving zoom or position behind the grade is drawn: the placement reads the 4:2:0 planes the grade hands on.) `RenderPlan::graded()`
  includes a grade, so the full-range refusals (`GradedFullRange`, `GradedInFullRangeStack`) judge it too. `kerf_core::baked_lut`
  gives a compositor the LUT's table with its intensity applied (`Lut3d::at_intensity`, the same numbers `to_cube` writes,
  four cached by path / length / mtime), `grade_filter_chain` the filters the engine writes for it, and `frame_matrix_tag` the
  matrix word `frame_matrix` reads. A grade has no keyable number, so a Motion plan needs nothing more of it.
  **Colour and partly-keyed transforms in the plan.** `PlanLayer.color` is `Clip::color_at(local)` in a still plan
  (a static grade's sample; the ZoomBehind check reads it) and, in a **Motion** plan with a keyed colour, **the numbers
  the export's `eq` evaluates to at that frame**: `engine::keyed_color_value(clip, T)`, each keyed number through
  `keyframe_value` (the quoted expression's own arithmetic, as `keyed_alpha` is) at `T` = `ffmpeg_frame_time(k)` on the
  clip's `fps` grid — `eq` reads `pts * av_q2d(tb)` behind `fps` — and the numbers not keyed the clip's own, the temperature
  as `Color::temperature_gammas` makes `1 + 0.3 t` / `1 - 0.3 t` of it (`sweep.rs` holds every number, `gamma_r` and `gamma_b`
  included, to the evaluated graph text to the bit, at all five rates). `eq` rebuilds its tables from them every frame, so a
  compositor that sets `GpuCaps::keyed_color` (a Motion plan is refused `Unsupported::KeyedColor` without it) builds its
  tables from the layer's colour each frame, as for a static grade. **A keyed `eq` is in the chain on every frame of the
  clip**, neutral numbers or not, so the refusals about grading a picture (`GradedFullRange`, `GradedInFullRangeStack`) judge
  `RenderPlan::graded(layer)` — a colour that is not the identity, or in a Motion plan any keyed colour — and a full-range clip
  is refused on the frames where the curve passes through neutral too. **On a compositor without `caps.exact_scaler`, a grade on a layer the scaler resizes is judged by its tables**
  (`Unsupported::GradeSteep`, from `reasons(size)`, static grades and each frame of a keyed one alike; with it, never): the compositor's scaler is a
  level from swscale's and `eq` maps two values a level apart to `eq::table_steps(&layer.color)` apart (`kerf_core::eq`: the port of `eq`'s
  tables, shared with the compositor; the steepest step between neighbouring entries of the Y, U and V tables of that colour, the whole table, a
  black point parked above 0 included), so a frame whose steepest step is past `MAX_AMPLIFIED_ERROR` (2) is FFmpeg's. The geometry is the
  sharpen rule's below (a scale ahead of the `eq`: every stage, or the fit scale when a moving zoom comes last); an unscaled layer is exact
  at any step. Nothing is built for an ungraded or unscaled layer, and the last four colours' answers are kept per thread (the build is about
  20 microseconds on the `pow` path). A **keyed mask** is the same:
  `PlanLayer.mask` is `Clip::mask_at(local)`, a still plan draws it as a static mask (the FFmpeg still draws
  `mask_at` through the static `geq`, the compositor `mask_alpha_plane` of it), and a Motion plan is refused
  (`Unsupported::KeyedMask`, no cap). A **keyed crop** is refused in both modes (`Unsupported::KeyedCrop`): the
  export *and* the FFmpeg still read the picture through its window with `perspective` (fixed-point, bilinear),
  which no compositor pass draws, and the layer's transform keeps only the static crop. `Animated.mask` /
  `Animated.crop` say so. A **moving zoom or position** (`Animated.places`, `Clip::placement_animated`) is refused in
  both modes (`Unsupported::KeyedPlacement`) unless `GpuCaps::placement`, which `kerf-gpu` sets **only once a probe of the FFmpeg in
  use has found that its `pad`, `perspective` and `crop` place a picture as `kerf_core::perspective` does** (`kerf_gpu::placement_probe`, which runs
  `engine::placement_probe_chain`, the export's own filter text, and `placement_probe_motion`, a keyed zoom and position with a breath,
  a drift and a shake over `perspective`'s frame count, against `Clip::transform_at`; `behaviors` goes with it; a probe that cannot tell is `Unknown`
  and refuses): the export and the
  FFmpeg still place the picture with `pad`, `perspective` and `crop` at sub-pixel positions (`engine-cli.md`), which
  `kerf_core::perspective` reproduces (the frames `pad` and `crop` make, the corners the expressions evaluate to, the filter's
  matrix and table of sample positions: `RenderPlan::pose` is the zoom, the position and a slide's travel it is drawn from;
  `LayerGeometry` of such a layer ends at the picture the chain makes before it, `Placement::placed`, with no zoom stage when
  the zoom is the one that moves). With the cap the rest of the layer is judged as ever, and a placed layer is refused when
  a rotation, an opacity below 1 or a dip to white is ahead of it (`ZoomBehind`, the moving position's as much as the
  moving zoom's; a grade is drawn behind it), for an odd-height picture (`Geometry`: `pad` keeps or loses its last row by how the graph allocated the
  frame, and the builds differ), for a letterboxed picture at the identity pose in a still (the still pads it to the whole
  frame first, with opaque bars), for a picture that is not 4:2:0 or gray (`PlacedFormat`: the chain converts it to `yuva420p` itself, which is the decode's conversion only where the `scale` filter defaults to the same scaler: 24 levels of chroma off on 4:4:4 and 16 on 4:2:2 on 4.4.2, exact on 6.1.1 and 9.0.2, so planar 4:4:4 / 4:2:2 are drawn where `GpuCaps::placed_chroma` says the probe measured a match), for a moving still image (`StillMotion`'s own graph: no placement checks apply), and for a pose whose positions leave the table's range (`PlacementFrame::reachable`).
  **Where the composite's range is negotiated, a placed layer whose asset did not record its range** (`color_range` `None`: probed before the flag was kept) **is judged by its file** for a host that asks the files ahead (`GpuCaps::file_range`, which the compositor's `session_caps()` sets and `GpuCaps::A0` and `Compositor::caps()` do not, so a plan judged without it stays a pure function of its inputs): `reasons` reads `source_is_full_range_known` (the cache alone: it spawns and waits on nothing) and refuses (`PlacedFormat`) a file that is full range or that has not said (never asked, being asked, failed, nothing usable), which is what the compositor's draw-time check refuses, seen by `PlayCut::ahead` / `resume`, a primer and the render bar before the frame instead of a frame into a session (`gpu.md`).
  `Animated` is `Some` for a
  keyed transform, colour, mask or crop and `Animated.keys` (`layer_geometry::Keyed {scale, rotation, rotates,
  opacity}`) says which transform numbers are keyed, because a clip keyed in part is not built like one
  keyed in full: a keyed zoom is a second `scale` even at 1, an unkeyed one is the static scale (a second
  scale only when it is not 1); a keyed turn is the `hypot` box, an unkeyed one the tight `rotw` box; a keyed
  opacity is the `geq` alpha (no round trip), an unkeyed one the RGB round trip, of which an odd side is the render's to judge (`GpuCaps::translucent_odd`) — so the
  matrix a translucent layer needs (`Unsupported::TranslucentMatrix`) is skipped only for a **keyed
  opacity** (`Keyed::opacity`), not for any keyed number: a clip with only its position keyed and a static
  opacity below 1 still takes the round trip.
  `Placement::keyframed` carries that (`Keyed::all(rotates)` is the legacy bundle), a colour-only clip is
  placed as a static one, and the sweep holds origin, zoom, turn, opacity and grade of the new cuts to the
  evaluated graph.
  **Live behaviours** (`Clip.behaviors`, `core-model.md`) are in the plan as the pose they give: `PlanLayer.transform` is
  `Clip::transform_at` (the formulas included, the exact numbers the graph's expressions evaluate at that frame) and `Animated` is built
  from `is_driven` / `rotation_turns` / `zoom_animated` / `placement_animated`, so a sway or spin is a keyed rotation (`Keyed.rotation` /
  `rotates`), a breath a moving zoom (`zooms`) and a drift, shake or breath `places` (the placement, once it is drawn). `Animated.behaves`
  (`Clip::has_behaviors`; a behaviour that moves nothing is not live) makes **every frame refused** in a Still and a Motion plan,
  `Unsupported::Behavior(layer)` (kind `behavior`), until `GpuCaps::behaviors`, which `kerf-gpu` sets: the pose is exact, and the placement draws it
  (breathe, drift and shake: `behavior/*` in both parity suites). One that turns the picture (`Animated.rotates`: a sway or a spin) is refused
  whatever the caps. With the bit set the pose is judged as any other, and
  `live_behaviours_are_the_pose_the_plan_carries_and_are_refused_until_a_compositor_says_it_draws_them` pins that for a sway
  and a spin nothing else refuses and for the rest nothing does but `Behavior` (the placement is drawn).
  **A stabilized clip is refused** (`PlanTiming.stabilized` = `Clip::is_stabilized`, `Unsupported::Stabilized(layer)`, kind
  `stabilized`, Still and Motion alike): its frames are moved by vid.stab's transform or `deshake` in the chain, which no pass
  draws, so FFmpeg draws them (`a_stabilized_clip_is_refused_in_a_still_and_an_export_frame`).
  **A layer below full opacity** (`RenderPlan::through_rgba`: a constant opacity below 1, in a still a sampled keyed one too; an export frame's keyed alpha is a `geq`, not this; a mask or a chroma key on it is drawn: the plane's alpha is what `colorchannelmixer` maps) takes
  FFmpeg's `yuva420p -> argb -> yuva420p` around `colorchannelmixer`, whose conversions run on this machine's swscale kernels. The compositor reproduces them
  to the sample on x86 (`kerf-gpu`'s `roundtrip_probe`, `gpu.md`, "The translucent round trip"), and `GpuCaps::translucent_approximate` (set by the compositor unless the probe says `Match`;
  `GpuCaps::A0` leaves it off) makes `reasons` refuse every such layer as **`TranslucentApproximate(layer)`** (kind `translucent_approximate`, the last variant but one of the enum: `tests/coverage.rs`' `ordinal` 58).
  A turn of such a layer (a quarter, three quarters or a half: `Rotation`, "a turn of a layer below full opacity"), its matrix unknown (`TranslucentMatrix`) and a fade to black or white running on it stay refused whatever the caps.
  **A picture with an odd side** (`LayerGeometry::translucent_odd`: `resolve` no longer refuses it, the render's caps do) is the compositor's where `GpuCaps::translucent_odd` is set (`kerf-gpu`'s `roundtrip_probe::odd_translucency` found a model of this FFmpeg's swscale for odd widths and heights; `GpuCaps::A0` leaves it off, `Compositor::caps()` sets it only with `translucent_approximate` off) and otherwise refused as before, `Unsupported::Geometry` "a translucent layer of odd size WxH (FFmpeg's RGB round trip reads past the picture)". With the cap, `RenderPlan::translucent_odd_refusal` keeps FFmpeg's what the model was not held to, each with its own text after the same head: a side under `TRANSLUCENT_ODD_MIN_SIDE` (16: the vertical chroma filter of 7 rows is two taps, which swscale's output functions take another way), a turn, a blend mode, a grade, an effect other than a chroma key. A plain, colour-corrected, masked, keyed or dissolving layer is drawn (`a_translucent_layer_of_odd_size_is_drawn_with_a_model_of_it_and_refused_beyond_what_it_was_held_to`; the dip to white and the fade to black keep their own refusals). The frontend port of `resolve` (`layer-geometry.ts`) and the corpus lost the same refusal.
  **A full-range picture in a still is judged by the range, not by a band** (`Unsupported::FullRangeResized(layer, "in a still")` and `Unsupported::FullRangeStill(layer)`, kinds `full_range_resized` and
  `full_range_still`, the last variant of the enum, `tests/coverage.rs` ordinal 59). *Resized*, on either FFmpeg: swscale converts the range inside the resize, so a JPEG 1200x800 into 320x180 reads
  Y 5,182 / U 13,104 / V 13,855 samples off on 6.1.1 (max 1 / 2 / 2: the old comment "a still is held to that" was a tolerance, and is gone). *At its own size, where the composite's colour is negotiated*
  (`PlanCanvas::policy` is not `FixedBt601`: FFmpeg 9): the still's graph keeps the picture's range through the overlay and converts it last, so its planes are full-range ones (a plain `yuvj420p` clip, a JPEG, a
  `yuv420p` flagged `pc`: Y 53,914 / 54,152 samples off, max 14 to 17; the PNG's RGB 3 to 4 levels), and **any layer on screen** that may be full range refuses the frame: a PNG moved over full-range footage
  (`rgb_in_stack` names matrices, not ranges) and text over it included. A picture flagged `pc` in any YCbCr format counts (`flagged_full_range`), the depth heuristic of `maybe_full_range_in` does not (a limited 10-bit
  picture is drawn exactly in a still). FFmpeg 6 converts a `yuvj` picture at the decode as the compositor does, so footage and a JPEG at the canvas's size stay drawn there (`checker_a_static_jpeg.rs`: 10 of 10 footage frames and
  every unresized static still exact, the rest refused; on 9.0.2 4 of 10 and 4 of 144). An export refuses the whole cut (`FullRangeCut`).
  **A blend mode** (`PlanLayer.blend`, `Unsupported::BlendMode(layer, mode, cause)`, kind `blend_mode`, Still and Motion alike): FFmpeg mixes the layer with the composite by `blend` in RGB after
  a conversion each way and two placements (`engine-cli.md`, "Blend modes"). The compositor draws the modes in `GpuCaps::blend` (`BlendKinds`; `kerf-gpu`'s probe says which this FFmpeg matches, `gpu.md`, "Blend modes"),
  and every cause that keeps a layer from being drawn is its own reason with the cause as the string, so the ledger ranks them: the mode not in the caps (`size_free_reasons`), a composite matrix with no tables
  (`RenderPlan::blend_matrix`, from `PlanCanvas::blend_tag`: the tag the graph's blend chains name, the bottom layer's over the whole cut for an export, over the layers on screen for a still, decided in the planner
  and checked against the graph's text; BT.2020 needs `caps.blend_extras` `BT2020`; **the tags are an allow-list**: a bottom layer (`PlanCanvas::blend_tag_unlisted`) or a blended clip stating a matrix
  `model::matrix_tag_is_unlisted` calls unlisted -- `bt2020c`, `ycgco`, `ictcp`, `chroma-derived-*`, `smpte2085`, `gbr` on YCbCr, which FFmpeg 9 cannot export at all and FFmpeg 6 reads as untagged -- is refused whatever the policy, as `fcc` and `smpte240m` are), an RGB picture (unless `RGB_PICTURE`), a graded clip (its RGB round trip is on `yuva420p` under a blend), a moving zoom or position (`RenderPlan::pose` is `Some`: the placement is not a picture the blend chain is held to), a layer below full opacity (`through_rgba`) where the caps lack `BlendExtras::TRANSLUCENT` (`size_free_reasons`:
  the probe did not find this FFmpeg's chain, `colorchannelmixer` ahead of the `split`, carrying the round trip's layer as the model does; it is drawn otherwise, a fade included: the fades' `alpha=1` steps run on the RGBA alpha before the conversion's 256/255 scale, `ffmpeg_alpha_ramps`), and from `reasons(size)`
  a rotated one that is not a quarter turn (`is_quarter_turn`) or has a keyed rotation and a letterboxed one (`geom.matte`). **A resized layer is no blend cause**: the layer is the fit pass's picture, the blend chain begins after it, and a scaler not measured to be swscale's is `ScalerApproximate`'s for every layer.
  **A rotation is drawn only as a quarter turn `rotate` lands on whole samples for** (`Unsupported::Rotation(layer, what)`, kind `rotation`, `size_free_reasons`, on every layer, plain or blended): `quarter_turn(angle)` is a port of the filter's
  own integer sine (`vf_rotate.c`: `angle_int = res * 65536 * 16`, `int_sin`) and its fast path (an angle within `FLT_EPSILON` of 0, π/2, π or 3π/2), and answers only for the angles where `c` and `s` come out exactly 0 and ±65536 -- the quarters
  -47, -46, -43 to 40, 43 and 44 and no others up to the 1000 radians it looks at (41, 42, 45 to 120 and -44, -45, -48 to -120 are a unit off and the picture lands between samples; measured, FFmpeg 6.1.1 and 9.0.2, every quarter from -1310 to 1310,
  `tests/checker_e_turn.rs`, against the export's own `rotate` text, and the many-turns cases of `tests/motion_parity.rs`), within `FLT_EPSILON` radians (the filter's own fast-path window) of a quarter (90.00001 degrees, 1.7e-7 radians off, is a rotation like 91). **A quarter or three quarters of a turn of a layer below full opacity** (a constant opacity: `LayerGeometry::translucent`) is `Unsupported::Rotation` too, from `reasons` (it needs the geometry): the export converts the turned RGBA picture to 4:2:0 and the compositor's round trip comes before its turn, so the chroma pairs other samples (U and V up to 19 levels off, Y exact); half turns and upright layers keep the translucent ladder, a keyed opacity and a fade of an opaque layer are not affected. Any other angle (the compositor's float edge against the filter's fixed-point one was Y 117 / UV 169 off), a rotation a key or a behaviour moves (a `hypot` box turned per frame) and a huge or non-finite angle
  is refused; a clip whose keyed rotation never turns has no `rotate`. The compositor's own guard says the same (`GpuError::Unsupported`).
  `normal` refuses nothing (`a_blend_is_drawn_where_the_chain_is_exact_and_each_cause_that_it_is_not_is_listed`, `the_matrix_a_blend_names_is_the_plans_blend_tag`).
  **A speed ramp is refused** (`PlanTiming.ramped`, `Unsupported::SpeedRamp(layer)`, kind `speed_ramp`, from `size_free_reasons` in a
  Still and a Motion plan alike, for every cap): the export retimes a ramped clip's frames with a `setpts` expression and lets `fps`
  pick among them (`engine-cli.md`, "Speed ramps"), a pick over a rate that moves which `frame_pick.rs` does not model, and a
  still's `-ss` names the frame the map says where the file shows the one whose slot has come (a neighbour in half the frames).
  What the plan still holds is exact: `clip_source_time` is `Clip::along` of the clip-local time (`(t - start) * speed` bit for bit
  for a clip without a ramp), `PlanTiming.speed` is the clip's mean speed (`Clip::mean_speed`), and a Motion plan's `FpsPick.speed` is
  **infinite** (`run` refuses a speed that is not a positive number, so the pick names *no* frame rather than the wrong one a constant
  speed would; infinite and not NaN, which would make the plan unequal to itself). The same keeps the other readers honest: the
  scrub window's `Snapshot::plain` leaves a ramped clip to the one-shot still (the window's `fps` pick is not the still's), an export in
  parts neither restarts a ramped still nor plans its cost at `speed_mag` (`source_per_frame` is the mean speed), and the
  thumbnail suggestions place a scene change with `source_to_timeline`.
  **A zoom that is part of the fit is refused** (`PlanLayer.fit_order`, `Clip::fit_order(canvas.fit, cover_overflows)`;
  `engine-cli.md`, "A zoom is part of the fit"): FFmpeg takes such a picture from the source in **one** `scale` to the frame times
  the zoom (`Folded`: a zoom in under Contain; `Uncropped`: a Cover clip that zooms or pans and whose cover hangs over the frame,
  or any Cover zoom in) where `LayerGeometry` and the compositor fit to the frame and scale that (two stages, Cover's crop between
  them). So `size_free_reasons` pushes `Unsupported::FoldedFit(layer, which)` (kind `folded_fit`) in a Still and a Motion plan, for every cap,
  until the compositor draws the one-stage picture with parity cases (**not done: no adapter where it was written**). A still
  that samples a keyed pose to the identity draws the plain fit (`Fitted`), as the still builder does; a moving zoom, a zoom out under Contain,
  a Cover clip of the frame's own shape that moves or zooms out (`cover_overflows`, judged from the decoded picture and the delivery
  canvas) and a masked or vignetted one stay `Fitted`, so a webcam PiP over a 16:9 cut keeps drawing (the coverage archetypes are 100%).
  **A still image whose picture moves has two routes in the graph, and the plan says which** (`Animated.still_motion` is the export's own
  eligibility, `engine::cli::StillRoutes` = `still_motion_for` + `StillStatic::of_clip`, asked once per clip by the planner; a move that
  leaves the frame bare at any frame is not eligible and is drawn as footage with a moving zoom, the placement). **The moving still's route**
  (`PlanLayer::still: Some(StillLayer)`, `RenderPlan::takes_still_route`; `kerf_core::still_motion`): the export's scaled copy of the
  still (`once`: `scale` to `q`, `crop` to the window any frame reads) and one `perspective` per frame, whose four corners the plan evaluates
  (`StillMotion::corners`, the expression's operation order, from `StillLayer::pose` = `property_at`). An export frame carries the clip's own
  plan (`clip_wide`, `export`); a still plan's depends on the render size and is **worked out by `RenderPlan::for_render(size)`**: every
  compositor entry point that decodes (`render_plan`, `render_plan_with`, `render_plan_texture_with`, `render_draft_texture_with`) does it
  (`for_size`: for a moving still's copy *and* for a fit picture that is missing or was made for another size), and a caller that decodes and
  composites by hand must too: `composite_*` from a plan that is not worked out is `Unsupported` ("not worked out for this render"), never a
  draw from a picture the compositor scaled itself (`reasons` judges an unworked plan as it would be made, which is what the ledger and the
  render bar need; it refuses a fit picture made for another size, or that cannot be made). `StillMotion` (kind `still_motion`, with the detail) is refused for: a picture kind the compositor did not measure
  (`GpuCaps::still_motion: StillKinds` = `yuvj420p` / `rgb24`; `Compositor::caps()` sets it from `kerf_gpu::still_probe`), a full-range
  picture where the composite's range is negotiated, colour / grade / effect / mask / opacity below 1 / a dip to white on the layer, a render
  size other than the one the move was made for (an export frame's is the delivery's). `layer_geometry` of such a layer is the frame-sized
  picture the `perspective` makes (no stage, no fit order, no placement), so `FormatResize`, `FullRangeResized`, `Geometry`'s chroma grid
  and `FoldedFit` are not its reasons; and it has no `pose`, so what the plan keys on a pose for it names by `takes_still_route` (a blend mode: `BlendMode`, "a moving zoom or position"). It is refused too for a keyed colour (`eq_in_chain`: the engine's `eq` is in the chain of every frame of the clip, so a frame where the numbers pass through neutral is not the plain move), a chain not measured (`StillChain`), a copy of an odd side or over `MAX_COPY_SIDE` a `perspective` that has no table (`PlacementMap::corners_have_a_map`: constant time) and a frame whose per-pixel placement table (`cw * ch * 4` bytes; `from_corners` falls back to it for about one move in three thousand) would be over `MAX_TABLE_BYTES`, 128 MiB (all in `reasons`, as the draw would refuse them). The blend gate is `takes_still_route`, not a pose or `places`, so a moving still whose only move is a crop stays covered if `KeyedCrop` is lifted. **A still whose placement is drawn** (`PlanLayer::fit: Some(FitCopy)`, `RenderPlan::fits_by_ffmpeg`:
  an `yuvj420p` or `rgb24` image with a moving zoom or position that is not the route above, nothing between its fit and the placement) has
  **FFmpeg make the fit picture** (`FitCopy::filters`: for a Cover frame fitted by one stage the engine's own text, `[crop=iw*..,]scale=W:H:force_original_aspect_ratio=increase,crop=W:H`, which names
  no size of the picture's; else the geometry's `crop` / `scale` / `crop` stages written out at the sizes `layer_geometry` gives behind a guard `crop` that refuses a picture of another size; the delivery's scaler flags; stamped by `for_render(size)`, and by the planner for an export frame, at the delivery
  frame): the compositor's scaler and decode are not in play, so the resize of a full-range JPEG or an RGB picture, the scaler's one level and
  the chroma grid of the crop are FFmpeg's, and `PlacedFormat` (other layouts), `FullRangeResized`, `FormatResize`, `Shrink` and `Geometry`
  (the chroma-grid check, lane B's odd-last-row comparison included) are not its reasons (the `fits_by_ffmpeg` family); `ScalerApproximate`, the odd-pad `Geometry`, `MixedMatricesWith` and the full-range policy refusals of a placed layer (`FullRangeNegotiated`, `FullRangeStill`: a still's own planes under FFmpeg 9) remain, and its colour is judged by the clip (`eq_in_chain`). Its fit picture has a **shape** (`FitCopy::shape`: the engine's own text, behind a static crop or not, the geometry's `scale` stages after a guard, or those stages with a `crop` of their own, `Cropped`, which no toy cut takes), and `StillChain` (shape, scaler named exactly as the engine's list spells it) is what the caps hold per kind: an unmeasured chain, and `Cropped` always, is `StillMotion`. **A moving still that is neither** (an image with
  a moving zoom or position and colour, a grade, an effect, a mask, a rotation, an opacity below 1, a dip to white, or a picture kind no chain
  was measured for) **is refused as `StillMotion`**: the footage placement would draw it from the compositor's decode and scaler, which no case
  holds a still to (only the two routes above are held, exact on 6.1.1 and 9.0.2).
  `LayerGeometry` is **unchanged** (still fit-then-zoom: the compositor's order, and `layer-geometry.ts`, the Preview handles' port): a Contain
  zoom's picture is within a pixel or two of the file's, and for Cover the handles show the zoomed *frame* where the file now has the
  larger cover, cut by the canvas. Drawing these layers means a one-stage `LayerGeometry` (`scaled` = the folded box, `keep` = the centred
  window), the port and the corpus following it, and parity cases on both FFmpegs.
  **What the compositor may draw is data**: `GpuCaps` (`Compositor::caps()`: the bits `motion`, `fades`,
  `transitions`, `dip_white`, `keyed_opacity`, `keyed_zoom`, `keyed_color`, `mask`, `text`, `reframe`, `hdr`, a `GradeKinds` bitset, an
  `EffectKinds` bitset, a `BlendKinds` bitset (`blend`: the modes this FFmpeg's blend probe matched, per matrix) and `BlendExtras` (`blend_extras`:
  `BT2020`, `RGB_PICTURE`); today `GpuCaps::A0` plus every one of them but `reframe`, `hdr`
  with every effect kind and every grade kind, the grade and blend bits where `grade::measured()` / `blend::measured()` say this FFmpeg draws as the model), and **`RenderPlan::reasons(&caps,
  size)` is a pure function of the plan's fields** returning `Unsupported` values whose
  `Display` is the message the plan has always given and which lists **every** cause of a refusal, not the first (a layer whose
  geometry is refused still gets its other reasons, judged on the picture's own chroma grid; a grade on a letterboxed layer
  the scaler resizes is a `Matte` and a `GradeWith`; a dip to white lists each of `Letterboxed`, `OddSize` and `Resized` that
  applies): the ledger in `gpu.md` ranks reasons by the frames lifting one alone would draw, which is only true if none hides
  behind another; whether a frame is drawn is `reasons().is_empty()` as before — nothing is decided while planning,
  so one plan answers for any caps and an A5 pass is a flip beside the pass and its parity
  cases. A Motion plan is refused as a whole until `caps.motion`, and in Motion a moving
  keyframed zoom is refused until `caps.keyed_zoom` (`KeyedZoom`: the export draws it last, at the output
  frame's time; a compositor that sets the cap draws the grade, the effects, the mask and the fades at the fit size and zooms after
  them), and
  keyed opacity until `caps.keyed_opacity` (`KeyedOpacity`: a `geq` alpha, not the RGB round trip), a non-`yuv420p` delivery and a
  gif are refused. **`PlanLayer::keyed_alpha`** is that `geq`'s alpha in an export frame, `(int)(opacity(T) * 255)`: the opacity
  curve evaluated by `engine::keyframe_value`, which is `keyframe_expr`'s arithmetic in its own order (the same segment by the same
  comparisons, the constants the `f64`s `av_expr` parses back; `sweep.rs` holds it to the evaluated text to the bit), at `T` =
  `ffmpeg_frame_time(k)` on the clip's `fps` grid, as `geq` reads `pts * av_q2d(tb)`; `None` in a still (which samples the opacity
  and draws it like a static one) and for a curve that never leaves 1 (no `geq`). That `geq` is the mask's too (`PlanLayer::keyed_opacity` is the curve's `f64` value `keyed_alpha` truncates: under a mask the `geq`
  multiplies it with the mask's keep before it truncates), so the opacity a moving zoom (or a blend mode on a key or a mask) is judged by is the layer's `keyed_alpha < 255` as well as the sampled
  opacity (`alpha_plane_refusal`, `zoom_order_refusal`; a mask is drawn under it), and **a chroma key under a keyed opacity is drawn**: the one `geq` multiplies the curve into the key's alpha
  plane (`alpha(X,Y)`, one in at the edge on 6.1, as the colour planes are), the compositor keeps a table of this frame's `curve * level` truncated (`gpu.md`, "Keyed opacity"). **A keyed opacity on a turned layer is not refused**: that `geq` runs ahead of the `rotate`, so on 6.1 the
  row and column it replaces land on the turned box's other edges, and the compositor shifts the picture's planes before the turn (`gpu.md`), exact at every quarter on both builds
  (a plain layer's chroma was up to 128 off on 6.1.1 before; the plan had let it through). **With `keyed_zoom` the plan still refuses a rotation, an opacity below 1 and a dip to white in front of a moving
  zoom** (`ZoomBehind(layer, what)`, Still and Motion alike: the placement would act on the rotated alpha plane, on the RGB round trip's
  or on the RGB frame a dip to white leaves, and no case holds any of them to FFmpeg's; a grade is not here, it is drawn ahead of the placement),
  and without it a Still refuses a grade, rotation, opacity, mask or effect in front of one. The old API stays: `gpu_supported()` / `unsupported_reasons()` (an owned
  `Vec<String>`) / `unsupported_reasons_at(size)` / `gpu_supported_at(size)` are
  `reasons(&GpuCaps::A0, ..)`: a plan is refused while a fade step is live, a layer
  travels or a tail window is open — exactly, where A0 refused the whole
  `[start - d/2, start + d)` of a transition and missed the tail after the ramp. **What a compositor that
  draws fades still refuses** (specific reasons, whatever the caps): `FadeOnTranslucent` (a black or white fade running on a
  layer whose opacity is below 1: FFmpeg fades the RGBA picture `colorchannelmixer` leaves, alpha plane
  included, ahead of the conversion back to `yuva420p`; a dissolve's ramp touches the alpha alone and is
  drawn) and, with `caps.dip_white`, `DipToWhite(layer, why)`: a dip to white is `yuv420p -> rgb24 -> yuv420p` around
  the fade (a filter of every frame of the clip in an export, of the frames the fade runs in a still), drawn **exactly** with
  the grade's conversions (swscale's x86 kernel in, the generic scaler out) where the probe says this FFmpeg converts as the grade's
  model does (`caps.dip_white` is `grade::kernels() == Match`: x86-64; the arm64 kernel makes it FFmpeg's), and only on **8-bit limited-range
  untagged / BT.601 / BT.709 `yuv420p`**. Refused, each with its own `why`: an alpha plane (FFmpeg picks another RGB
  format), a black fade of the clip running ahead of it, a `yuv420p` flagged full range (`DipWhy::FullRange`: the export's round trip converts in that range on
  every frame of the clip), an `invert` or a `vignette` on the clip (`DipWhy::RgbEffect`: of the effects only `negate` and `vignette` list an RGB format, so the fit `scale`
  converts the picture ahead of them and FFmpeg runs them in RGB — `gblur`, `unsharp`, `hue` and `eq` list none and stay in the YUV; invert Y 3, vignette Y 47 off), any other pixel format (RGB 11 / 12 levels of Y / UV off,
  10-bit 9 / 16, 4:4:4 16 / 17, 4:2:2 9 / 13, full range up to 21 — in an export's planes on 9.0.2 — or an unrecorded format) or matrix, and, from `reasons(size)`
  (the geometry): a letterboxed layer (`pad` runs before the round trip, so FFmpeg's bars whiten; Y 126 against 16 at half
  way), a picture with an odd side (the round trip reads past it), and any layer the scaler resizes (the `scale` filter hands
  the fade its `rgb24` itself, scaling and converting in one pass: Y up to 19, UV 28 off, a crop that leaves the size is exact).
  The dip-to-white refusals are the **Motion** plane bound's and are conservative in stills on purpose: bypassed, RGB, 10-bit, 4:4:4,
  4:2:2, gray, BT.2020, full-range and half-size layers all pass a still's strict bounds, but `reasons()` is one function for both modes.
  `FullRangeNegotiated(layer)` (a still): a `yuvj` (or unrecorded-format) picture inside a running fade, dip or dissolve where FFmpeg 9 works on its raw
  values and converts the range afterwards (`fade` darkens toward the limited-range 16 first: frame 0 of a fade-in reads Y 30 where black is 16). A **`yuv420p`
  flagged `pc`** (`PlanStream::raw_full_range`) gets it under **every** policy and in an export frame too (under `FixedBt601` only; the cut is the reason where FFmpeg 9 flags it): its format is already the overlay's, so FFmpeg 6 has no conversion ahead of the fade either.
  **`FullRangeCut`** (an export frame; a plan-level reason, from `PlanCanvas::full_range_cut`, which the `Planner` sets when any clip of the cut
  `maybe_full_range()`): FFmpeg 9 flags the whole export full range and converts every limited-range frame of it (a black frame reads Y 0, not 16), so under any policy
  but `FixedBt601` **no frame of a cut that holds a full-range clip** is drawn — before it, after it, on another track or a gap. FFmpeg 6 converts at the decode, as the
  compositor does, and draws it all to the level, except (a) **a `yuv420p` flagged `pc` that is letterboxed in an export** (`Unsupported::Geometry`, "flagged full range", any
  policy but `BottomLayerTag`): its format is the overlay's, so nothing converts its range ahead of the graph, `pad` draws the bars in limited black (16) and the converter
  `scale` ahead of the overlay turns the range afterwards (bars Y 30 where the compositor's matte is 16; the picture itself is exact; a still's fit `scale` converts the range
  ahead of its `pad`, and is exact), (b) a dip to white on such a picture, **on every frame of the clip** (`DipWhy::FullRange`: the export's round trip converts in the
  picture's range, the compositor's tables are limited-range's: Y 2, UV 1 off), and (c) **`FullRangeResized`** (under either FFmpeg, from `reasons(size)`) refuses a full-range
  picture the scaler resizes in an export frame, inside a fade **and in a still** (swscale converts the range inside the resize, between the horizontal and the vertical pass: chroma up to 15 levels off in an export, a JPEG half-size over video Y 12 / UV 16 over a fifth of a still, an enlargement 14 / 21 — a still used to be drawn inside a band and is not any more; what an exact route needs: the raw full-range planes from the decode and swscale's `lumConvertRange` / `chrConvertRange` on the 15-bit lines between the passes of the compositor's scaler).
  `maybe_full_range()` is true for a `yuvj…` name, no recorded format, or **a `yuv420p` flagged full range** (`StreamInfo.color_range == "pc"`, recorded since schema v10; a VP9 / HEVC / AV1 file probes as plain `yuv420p`): the range flag is the truth, but an
  asset stored before v10 has none (`None`, "not known": judged by its name as ever; so does an unflagged file and a `tv` one). `flagged_full_range()` is the flag in any YCbCr format (RGB and gray pictures read `pc` from ffprobe on every FFmpeg, so for them it says nothing). Under a policy that is not
  `FixedBt601`, `maybe_full_range_in(policy)` also counts a YCbCr picture flagged `pc` in any format and any deeper-than-8-bit YCbCr / gray format (`pix_fmt_is_high_bit`) whatever it says: a `pc`-tagged 10-bit file probes as `yuv420p10le`, 9.0.2 handles it as `yuvj`, and a
  limited 10-bit picture is refused with it, because a file may say nothing of its range (it is what the cut flag, `FullRangeNegotiated` and the grading refusals use; 6.1.1 draws a 10-bit one exactly, so there only the name and a `yuv420p` flag decide).
  A flagged `yuv420p` is the exception on 6.1.1: nothing converts it ahead of the filters, so its raw samples meet the vignette, key, `eq`, fade and the other effects on every FFmpeg (refused under any policy), while a plain layer and a cut are exact there. Only a clip the export holds sets the cut flag (not a muted track, a
  disabled clip, a track a solo shadows, or an audio track's use of the asset).
  The policy condition is a **proxy measured only on FFmpeg 6.1.1 and 9.0.2** (5.x, 7.x, 8.x and distro builds are unmeasured).
  **`gpu_supported_at(size)` is the per-frame fallback switch** (`gpu_supported()` is
  its size-free half; `unsupported_reasons_at(size)` says *no, with reasons*) for
  anything the compositor does not render exactly: video effects (`Effects(layer, what)`: names the effect without its kind
  in the caps, or the value
  `gblur`'s own kernel is not the recursion on every plane shape at every slice-thread count, so a blur is refused (`Effects`, "a blur on a plane ...", sigma 0 included) when a plane of the picture (luma, and chroma at half size rounded up) is one sample wide or tall, has fewer rows than the graph has slice threads (**not at sigma exactly 0**, the only blur lifted: `set_params(0)` is `0 / 0`, the same NaN on every build, so `nu` is 0 and both scales 1 and the empty postscale slice scales by 1 and clips again, which is the sample for every 8-bit value in any interleaving; `gblur::plane_hazard_for`. A tiny sigma whose coefficients `Gaussian::is_identity` calls the same, 1e-5 or 1e-9, is a cancellation in the filter's `double` arithmetic that the blur probe does not measure, and is not lifted), or a vector of rows a slice over a plane no wider than a vector (still refused at sigma 0: the kernel reads past the row, which is `x + 0 * past` and `x` only for a finite `past`) (`kerf_core::gblur::picture_hazard_for`, judged over `RenderPlan::graph_thread_range(caps)`, which is exactly FFmpeg's default thread count where `kerf_core::ffmpeg_cpu_count` reproduces its core count (Linux: `sched_getaffinity` of the calling thread, read at each judgement; `None` where it is not reproduced or the call fails, where FFmpeg's `av_cpu_count` keeps one), else the range `1..=16` (a blur is judged over all of it, and a chroma key, drawn at one count, is refused: "a chroma key over a graph whose slice-thread count is not known"), with `GpuCaps::blur_lanes` and `GpuCaps::graph_threads` for the width and the pin); `gblur` / `unsharp` refuse itself: a sigma outside 0 to 1024, an amount outside -2 to 5, **and a sigma whose coefficients `gblur`'s own double-precision cancellation leaves outside what the shader computes** (`kerf_core::gblur`: `nu = (1 + 2 lambda - sqrt(1 + 4 lambda)) / (2 lambda)` cancels for a sigma of about 1.05e-8 to 1.29e-8 to a `nu` up to 1.99 and a `boundaryscale` down to -3; `gaussian(sigma)` is `None` exactly where `Gaussian::in_shader_domain` is false, from the derived numbers, and the compositor takes its coefficients from the same function); or what the picture changes: an invert, blur or sharpen on a full-range or never-recorded picture, an invert on a gray one, a vignette on anything but a limited-range `yuv420p` one (`vignette` works in the picture's own format), a chroma key on anything but a `yuv420p` one, or on a rotated layer or a blended one below full opacity (a key under a constant or a keyed opacity is drawn); in `reasons` a chroma key after a scale ahead of the effects **where the scaler is not measured to be swscale's** (`!caps.exact_scaler`, as for the sharpen and the grade: its alpha moves by up to 1/blend levels for a level of chroma; where the scaler is exact the key after a scale is drawn, alone and after a blur, a sharpen, an invert, a grayscale or a vignette: `a_chroma_key_after_a_scale_is_drawn_as_chromakey_draws_it` in `parity.rs` and `motion_parity.rs`; **a blur is no longer a reason**: the compositor's blur is `gblur`'s to the sample, `GpuCaps::effects` holding it only where `blur_probe` measured the kernel) or on a picture under two samples a side; a similarity or blend `chromakey` refuses itself (0.00001 to 1, 0 to 1); and in `reasons`, by render size, a sharpen after a scale **that is not exact** (`!caps.exact_scaler`: the worst case starts at 1, at the grade's steepest luma step when a scaled layer is graded, and each later non-zero sharpen adds `ceil(2 * err * |amount|)`, so the frame is refused past 2: one sharpen above 0.5, or a second one; where the scaler is swscale's to the sample, and after a blur, a sharpen is exact at any amount, chained, and drawn); and a grade whose tables step past 2, above), masks (`Mask` without `caps.mask`;
  `MaskWith(layer, what)` with it, on a rotated layer, over a chroma key, or on a blended layer below full opacity (a mask under a constant opacity is drawn, level by level:
  `layer_geometry::ffmpeg_alpha_of`, and under a keyed one as the `geq`'s own `f64` product, `PlanLayer::keyed_opacity` and `mask_alpha_plane_keyed`, `gpu.md` "Masks" and "Keyed opacity"; a chroma key's alpha follows `alpha_plane_refusal` too: `Effects`, "a chroma key on a rotated layer" or "on a blended layer below full opacity", and a key under either opacity is otherwise drawn); `Matte(layer, what)` for a blur, a sharpen, a vignette or a chroma key on a
  letterboxed picture, judged with the geometry in `reasons`: FFmpeg's `pad` hands them the padded frame, bars included — a mask is cut
  from that frame too and is drawn at the layer's size), 360 reframe,
  HDR, a text overlay it cannot reproduce (below), a fade or transition in progress, a delivery scaler the compositor has not measured
  (`bicubic` and none are drawn wherever the scaler is exact; `bilinear`, `lanczos`, `neighbor` and `spline` only where `GpuCaps::scalers`
  holds the name, i.e. where `kerf_gpu::scaler::kernels()` proved that name's tables on this FFmpeg: `Unsupported::Scaler`, settled
  (`frame_wide`) only for a spelling the engine's list does not have, `ScalerKinds::of_name`), a still whose fit picture
  FFmpeg makes when the delivery names any of those four (`scaler_named_other`: the chains `still_probe` measures are the default scaler's
  and `bicubic` named; `StillMotion`), and a moving still on the route (`takes_still_route`) of a delivery that names one, unless the caps hold the move
  inside the frame of that kind of picture under that name (`StillKinds::named_route`, `RenderPlan::still_route_scaler`; `still_chain` gives none for it), a
  layer with no known picture size, a shrink steeper than `max_shrink_for(canvas.scaler)` (`MAX_SHRINK`, 63:1, the
  steepest the scaler comparison measures; `MAX_SHRINK_LANCZOS` 42 and `MAX_SHRINK_SPLINE` 18 for a delivery that names those, whose tables
  reach swscale's 256 taps, where it cascades a second scaler, from 43.4:1 and 19.6:1), and the classes below. Pure + unit-tested
  like the rest of the timeline math. **`Unsupported::kind()`** names a refusal without its layer
  names and numbers (`mask`, `mixed_matrices`, `text_sequence`, …; an exhaustive `match`, so a new
  variant must choose one): the key the GPU preview counts refusals by (`app.md`).
  - **Text** (`caps.text`, which `kerf-gpu`'s compositor sets: FFmpeg rasterises `drawtext`, the compositor blends it —
    `gpu.md`). The font is **always a file the plan can name**: `fonts::resolve_text_font` (the installed family the overlay
    names, else the shipped Geist, `Regular` or `Bold`, written to `<cache>/kerf/fonts/`, checked on each call) is called by the graph
    (`drawtext_common`) and the planner alike, so `PlanText.font_file` is exactly what the still / export pass as
    `fontfile=` — `TextWithoutFont` is only the case where the default could not be written. `kerf_core::text_still_filter(&PlanText, h)`
    is the `drawtext` the still draws for a plan's overlay (a test holds it byte-equal to `drawtext_still` over the
    timeline's overlay at the same time) and `text_raster_graph` the graph the compositor rasterises it with (same options and
    position, white on black in `rgb24`, parts `Glyphs` — a synthetic border drawn white in it — and `Box`); `text_piece_filter` /
    `TextPiece` is one piece of the same look (a prefix of the text, one glyph unit, a box) with the caller's colours and position, and
    `text_as_drawn` the characters `drawtext` is given: what the compositor measures and takes the bitmaps apart with. Refused: `Text` (no `caps.text`: a compositor sets it only for a build whose `drawtext` it can take apart
    glyph bitmap by glyph bitmap, as `drawtext` blends them, and whose colour conversion it knows, `gpu.md`), `TextWithoutFont`, and **`TextSequence(why)`** (kind `text_sequence`: `text_sequence_limit` — a tab, over
    `MAX_SEQUENCE_CHARS` = 160 characters, a text too wide for `MAX_MEASURE_SIDE` = 16 384 px of measuring canvas at the render size; judged by `reasons`, the width at the size). A synthetic-bold border (its bitmaps overlap along every letter gap) and print
    under 12 px (its bitmaps share samples all along the line) are not refusals: one mask of their sum is not their blends in turn, so they are blended in turn, and what the compositor cannot then reproduce exactly (a glyph that does not shape one to a unit,
    a face whose bitmaps overhang) is refused by the render (`GpuError::Unsupported`), not the plan.
  - **Animated SVG titles** (`Timeline.svg_overlays`, `plan_svg.rs`): resvg draws them and FFmpeg overlays the pictures; a compositor draws the same PNG's
    planes (FFmpeg converts it) with `overlay`'s own arithmetic (`gpu.md` "Animated SVG titles"). `RenderPlan.svg` is a `Vec<PlanSvg>`: a **floating** title carries an
    `SvgTitle` (the artwork `Arc<str>`, the `FontSet` it is drawn with — `fonts::snapshot().named_by` of the artwork, taken once by the `Planner` —, the placement as the overlay
    stores it, the clamped opacity and the document's clock `doc_t` at this frame); a **title clip** is `title: None`. Nothing re-derives the export's arithmetic: the
    `Planner` reads `svg_layer::sequence_of` (the output frames the title is on, `window()` over the cut's own length, and `doc_t0 = max(0, offset + exact_time(first) - start)`) once,
    a Motion frame `k` is on iff `first <= k <= last` and its clock is `doc_t0 + exact_time(k - first)` (the export's picture `j`), a Still is `start <= t < end` at
    `doc_time(t)` (`still_svgs`); the rectangle at a render size is `svg_layer::rect_of` (`PlanSvg::rect(size)`, `None` when the export leaves the title out: wholly off
    the frame or over `MAX_PIXELS`), the picture `svg_layer::picture_png` (the one function an export's sequence, a still's picture and the compositor's draw with) and its
    key `picture_key`. `reasons` pushes **`Unsupported::Svg`** (kind `svg`, a title clip: a clip chain of fit, fades and transitions on an alpha picture, not an
    overlay; frame-wide, nothing lifts it) and, for a floating title, **`Unsupported::SvgOverlay(SvgWhy)`** (kind `svg_overlay`; first cause only): `Unmeasured` (no `caps.svg`:
    a compositor sets it only where it measured this FFmpeg's `overlay` of a picture, `kerf_gpu::svg`), `NoLayer` (no clip under it: the black base is no YUV composite) and
    `Matrix(m)` (a composite that is not BT.601: FFmpeg 9 converts the picture with the composite's matrix, which is not held to the conversion the compositor asks for). `svg_overlay_chains`
    is the export's own `settb`/`setpts`/`overlay` text for a probe to run.
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
    and untagged are one class) is drawn with that matrix; **an unknown one and an RGB picture in a stack that is not BT.601 are refused**.
    **Mixed matrices** are FFmpeg's conversion of the other layers into the composite's (swscale through RGB; float and
    fixed-point models of it were off by up to 26 levels), reproduced (`gpu.md`, `convert.rs`) for a plain layer: `matrix_reasons`
    keeps `MixedMatricesWith(layer, why)` (kind `mixed_matrices_with`; the cut-level `MixedMatrices` is `mixed_matrices`) for a layer whose matrix is not `PlanCanvas.matrix` and that
    `RenderPlan::convert_refusal` does not clear (a grade, an effect other than invert / grayscale (and those in a still), a mask, a rotation or opacity,
    `eq`, a keyed number, a format other than 8-bit limited-range `yuv420p` / `yuv422p` / `yuv444p` video, a fade to black running in a still or on the alpha plane (a plain export chain's fades ahead of its converter: drawn) or
    a dip to white, an odd size or bars off the 4:2:0 grid, a blurred fill behind a layer on the alpha plane, and in a **still**
    a 4:2:2 / 4:4:4 picture, and a layer whose last `scale` resizes it unless it is the cascade `RenderPlan::scaled_convert_refusal` clears: a still has no plain `format` ahead of the overlay, so that
    scale converts in the same pass; the same for a layer on the alpha plane that is not the decider: its fit `scale` converts. swscale cascades `yuv420p -> bgr24` at the
    smaller size and back (`gpu.md`, `GpuCaps::matrix_convert_scaled`), drawn for a 4:2:0 picture that **shrinks** with the whole result kept, even sides and scaled luma rows
    (`ScaleStage::cascade_refusal`), refused for a Cover crop, a picture that does not shrink, a blurred fill, **a delivery that names `bilinear`, `lanczos`, `neighbor` or `spline`** (its flags are in that very `scale`, and the cascade is reproduced with the default scaler's tables: 42,858 to 79,994 samples off on 9.0.2 without the refusal, `a_shrunk_layer_of_another_matrix_in_a_delivery_that_names_a_scaler_...`) and a caps without the cap), and
    for a caps without `matrix_convert`. **In a Motion plan the composite's matrix is the cut's**: `PlanCanvas::cut_matrix` is
    the tag of the first clip, in the graph's order (a track's clips by start time, tracks in list order), that ends on the
    alpha plane (`yuva420p`) with nothing forcing a converter in its chain, else the first clip's: FFmpeg 9's negotiation,
    read from `avfiltergraph.c` and held by real files (`gpu.md`). It holds for every frame, so a lone clip of another matrix
    is converted wherever it is on the timeline. A cut whose deciding clip is RGB, of no known matrix, **or not read** (a
    clip on the plane that holds an `eq`, a grade, an effect, a tone map, a reprojection, a stabilisation or a placed black
    fade, or a blend mode anywhere in the cut: `PlanCanvas::unread_decider_in_cut`) is `MixedMatrices` in every frame, and so is a mixed cut for **caps without `matrix_convert`** (the conversion, and with it the decider's reading, is not measured on this FFmpeg: the deciding clip's own frames are refused too), and a
    mixed cut with a grade in the stack is refused. An RGB picture is taken into YCbCr with the cut's matrix, so one alone
    in a cut with a BT.709 or BT.2020 clip is `RgbInStack` (`PlanCanvas::cut_non_601`). `PlanCanvas.matrix` is the result for
    the frame's stack; `PlanStream::matrix` is the one a translucent layer is taken out of YUV with.
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
    translucent layer of odd size (with the cap `translucent_odd` it is drawn but for the cases above), **resizing a picture that is not planar 8-bit
    4:2:0 (`yuv420p` / `yuvj420p`)** (any scale stage that changes its size, in either direction; 10-bit 4:2:0 and gray included: FFmpeg scales 10 bits and
    dithers once at the end, off by one in Y, U and V on 6.1.1, and a gray luma in `gray`, up to 10 levels off over half the plane on 9.0.2 — `FormatResize`; not a layer whose fit picture FFmpeg makes, which the compositor does not resize: `fits_by_ffmpeg`, `yuvj420p` / `rgb24` only, and the scaler-approximate and odd-pad refusals still apply to it) —
    FFmpeg scales in the format the picture has, the decode reduces it to 8-bit 4:2:0
    first, and the chroma is then interpolated from different samples: enlarging was
    4:4:4 x1.5 27 levels off, 4:2:2 x2 17, BGR0 x2 51, PNG RGB x2 69, and even a shrink
    as mild as 1.05-1.5x reads flat max 8-9 (4:2:2 at 0.9x is over the limit) with
    edges up to 32 levels. No band of ratios is measured strictly inside the limits on
    both FFmpegs for busy chroma, so none is claimed; a picture left at its size is
    drawn for every format — **a `pad` of a picture with an odd height that `scale` did not resample, placed at the frame's origin** (one row short of the
    frame: `Geometry`; `vf_pad.c` pads in place when the frame it is handed is writable and has room, and the odd last row goes, and copies it
    otherwise. An export hands it the decoder's frame, so the outcome is the decoder's and the frame's and the plan cannot say — asked of FFmpeg's own export, per
    decoder (`checker_b_r4_origin_which_decoders_drop_the_odd_row`): 6.1.1 keeps the row for FFV1, rawvideo, VP9 and MPEG-4 and drops it for ffvhuff and MagicYUV; 9.0.2
    drops it for FFV1, ffvhuff and MagicYUV, keeps it for rawvideo and changes from frame to frame for VP9 and MPEG-4. A still hands it the buffer of its own `scale`, which
    drops the row for every decoder on both builds; it is refused at the origin all the same, as the same plan code states it. The refusal is on every build.
    **Off the origin it is the graph's** (`RenderPlan::odd_pad`): a Still plan's `scale` converts a plain `yuv420p` picture into the `yuva420p` its overlay takes, inside the buffer `pad` hands it, so `pad` fills in place and the odd row and the chroma row under it go at any gap (`layer_geometry` shows one row less; refused when the chain carries a colour correction, grade, effect, fade, mask, keyed number or another format, which decide differently: FFmpeg's still keeps the row for a black fade and a grade and drops it for an `eq`, an effect, a chroma key and a keyed number, and none of these is drawn yet). A Motion plan's trailing `format=yuv420p` makes it a copy for `yuv420p` (drawn) and a conversion for another format, a chain on the alpha plane (a dissolve, a mask, a chroma key) or a blend mode (refused). A native format that is not 4:2:0 (4:2:2, 4:4:4, RGB) at the frame's width is converted by `scale` into that buffer in every chain, and its geometry (the native grid's) drops the row: drawn, exact on both builds, with no crop between two chroma samples. **A `yuv420p` picture flagged full range, letterboxed, in an export** whose FFmpeg does not negotiate the colour (policy other than `BottomLayerTag`) is refused: `pad` draws the bars in limited black and the graph converts the range afterwards (they read Y 30; the compositor's are 16) — and **a crop of a picture whose chroma is finer than
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
    whatever the container, or it is a `yuv420p` flagged `color_range=pc`) to limited range up front; FFmpeg 9 keeps it full range
    through the graph (tagged `pc`) and runs `eq` and the overlay on it, converting
    last. That agrees with no colour correction and disagrees with it by 34 to 44 dB
    (every knob, measured on both an mjpeg file and full-range H.264), so **colour
    correction on a `yuvj` picture, or a `yuv420p` flagged `pc`, is refused** (under any policy — FFmpeg 6 happened
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
