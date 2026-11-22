# core-render-plan — brief

Rules only. The full file `.claude/docs/core-render-plan.md` is authoritative and holds the measurements and the story behind each rule: grep it for the phrase in parentheses.

## Orientation

kerf-core, three areas: `platform.rs` (platform checks), `review.rs` (cut review), and the render plan
(`render_plan.rs` + `planner.rs` + `plan_caps.rs`, with `frame_pick.rs`, `media.rs`, `render_bar.rs`, `play_plan.rs`,
`layer_geometry.rs`, `plan_svg.rs`); `plan_epoch.rs` is the change counter plans are cached by. The plan is "what one
frame is made of", shared by FFmpeg's still, the export graph and the kerf-gpu compositor. It is data: whether a
compositor draws a frame is `RenderPlan::reasons(&caps, size).is_empty()`; whatever is not drawn exactly is refused and
falls back to FFmpeg. Many rules mirror the export graph (`engine-cli.md`) and must move with it.

## Map of the full file (grep the bold lead)

- `platform.rs` bullet, then `review.rs` bullet, then the long `render_plan.rs` bullet, then `plan_epoch.rs`.
- Inside the render-plan bullet, in order: plan is complete / Planner; 10-bit delivery; title clip; two modes (Still,
  Motion), fades, candidates; which frame of which file (picks, tick arithmetic, fps pick, still images, late start,
  PickRate); blurred fill; FrameSource traps; plan describes the decoded file (proxy); spans and hand-over; render_bar;
  PlayCut; sweep.rs; A7 matrix concern; mask alpha; grade; colour and partly-keyed transforms; keyed mask / crop /
  placement; Animated.keys; live behaviours; stabilized; below full opacity; odd-side translucency; full range in a
  still; blend mode; rotation; speed ramp; folded fit; moving still images; GpuCaps / reasons; keyed alpha; ZoomBehind;
  fades and dip to white; FullRangeNegotiated / FullRangeCut / maybe_full_range; `gpu_supported_at` (effects, masks,
  matte, scalers, shrink); then the sub-bullets Text, Animated SVG titles, Pixel format, composite matrix policy, mixed
  matrices, BT.709 label, render-size dependencies (FormatResize, windows, odd pad, chroma grids), Range.

## platform.rs — where the cut is going

- Keep the two limits apart: hard (platform rejects) and reach (accepted, then not distributed); a reach miss is the worse
  outcome because nothing tells you. (`two limits apart`)
- Findings carry both `Severity` and `IssueKind` because the UI collapses same-kind findings across platforms.
  (`because a landscape cut earns`)
- Aspect is compared as a ratio (720x1280 reads as right shape, soft). Numbers are advisory; nothing here ever blocks an
  export. (`aspect is compared as`)
- `platform_report(frame, range)` resolves from `working_timeline` (an agent is judged on its own proposal); the frame
  override is for a render that resizes away from the project frame. (`so an agent is judged on its own`)
- `cut_summary_in` summarises the *sliced* timeline: `Timeline::export_span` is the one clamp the export's
  `effective_range` also reads (empty or inverted span = whole cut), then `Timeline::slice`, so length, frame, sound and
  title all describe the file. MCP `platform_check` takes the same `range`; `export_variants` judges each file over the
  range it was exported with. (`the one clamp the export's`)
- `PlatformReport` carries every target's check and the `CutSummary` judged; messages use `possessive`; `platforms.ts`
  mirrors it. (`mirrors it`)
- A file-size cap (`PlatformTarget::size_limits`, `SizeLimit`) is recorded only with a source and a date in its comment, else
  left unset with a `TODO(size)`; it is held to an **estimate** (`check_sized` with `SizeBounds`), never a guess: an **error only when `floor_bytes` is over it** (the
  model's floor, or a whole encode; never a sampled measured low end, which is a warning), a warning when the low or the high end is (`IssueKind::Size`). `check` is `check_sized(.., None)`, so no
  estimate, no cap or an empty cut answer exactly as before; `up_to_secs` is inclusive (the stricter tier at the boundary).
  Only TikTok has caps (500 MB / 2 GB, carried from the 2026-08-25 check, not re-checked). (`**File-size caps**`)

## review.rs — whether the editing is clean

- Pure `review(inputs, range, checks)` over `ReviewInputs` (`Project::review_inputs`: working timeline, assets, cached
  analysis, resolved under the lock; the review runs without it), on `for_render` at the delivery rate. (`resolved under the lock; the review runs without it`)
- Issues reuse `platform::Severity`; each carries `kind`, `time`/`end`, `clip_ids`, `message`, `fix` naming the tools. Each
  check is tuned to stay quiet on ordinary edits; thresholds are constants. (`tuned to stay quiet on ordinary edits`)
- `flash_frame`: visible clip (footage, still, title) on a video track under `FLASH_FRAMES` (3).
  (`a visible clip (footage, still or title)`)
- `mid_word_cut`: only a *sounding* clip's edge (`source_audio` and an audio stream; a detached picture leaves it to its
  sound); source time through speed/reverse within `WORD_TOLERANCE` (40 ms; 120 ms and a tip for estimated words) inside a
  word of at most `MAX_WORD_SECS`; not in silence, not at the file's own ends, not a split nothing was taken out of
  (`continues`: same asset and speed, contiguous source). (`a *sounding* clip's edge`)
- `hard_audio_cut`: sounding clips meeting on a lane (half a frame), no fade/transition/zero gain, sound both sides by
  silence analysis (`EDGE_SILENCE_MARGIN`); pairs without analysis are not guessed (all unjudged = `skipped`, some = one
  tip counting them). (`sounding clips meeting on a lane`)
- `jump_cut` (tip): one asset resuming on a video lane more than a frame and at most `JUMP_MAX_SOURCE_GAP` (8 s) later,
  no transition, no scene change between, no reframing (`PUNCH_IN_SCALE`, move, crop, keyed transform, other 360 view);
  a live behaviour is a look over the same framing and does not count; `reframed` reads the clips' keys, not
  `is_animated`. (`one asset's footage resuming`)
- `stale_captions`: the model keeps no caption options/revision stamp, so it compares words; only generated captions whose
  words appear as a run in the cut's transcripts (else imported/foreign, none = `skipped`); stale when fewer than
  `CAPTION_MATCH` words spoken within `CAPTION_SLACK` (0.6 s); uncaptioned speech runs of `UNCAPTIONED_MIN_WORDS` mean
  footage added after captioning; one issue each. (`the model keeps no`)
- `black_gap`: no visible clip on any video track (more than half a frame, not under a typed or SVG title); warning
  inside, tip after the last picture while sound runs on. (`no visible clip`)
- `unstabilized`: stabilized clip that plays/exports as shot (status from `ReviewInputs.stabilization`: missing, queued,
  analyzing, failed, unsupported); warning naming `stabilize_clip`. (`a stabilized clip that plays and exports as shot`)
- Same kind at the same moment in the same footage (picture + its sound, linked pair) merges into one issue. Tests in
  `review.rs`. (`merges into one issue naming every clip`)

## Plan: completeness and the Planner

- `RenderPlan::at(timeline, assets, opts, t, color)`: canvas via `render_geometry` (same `export_format` as the still;
  `fps` is the delivery rate when stated). It is a one-shot `Planner` in `PlanMode::Still` and shares `clip_source_time`
  with `active_video_clips` (what `build_still_args` uses) so the FFmpeg still and the GPU cannot disagree about which
  clip is where; `still_size` is the preview-size rule both use. (`cannot disagree about which clip is where`)
- A `Planner` prepares a cut once (`for_render`, `transition_fx`, geometry, asset facts, per-clip window/fades/motion keys,
  fonts, per-track sorted clip index with a running maximum of window ends); `at`/`at_frame` binary-search then walk back.
  `O(n)` for one very long clip under many short ones. (`The plan is complete, and a`)
- `at(t)` is the output frame *on screen* at `t` (`Rational::frame_containing`: slot `[k/fps,(k+1)/fps)`), not the
  nearest. (`is the output frame **on screen** at`)
- A clip with no asset is an error only when a frame asks for it; an `fps` FFmpeg would not parse is an error in either
  mode. (`A clip with no asset is an error`)
- Transitions are per layer, as in the graph: clip `FadeStep`s via `LayerFx::strength(tint, frame, fps)` (counts frames
  like `fade`), slide/push travel (`MotionKeys::at`), `tail`; a dissolve is two ordinary layers. (`are per-layer, as in the graph`)
- The canvas carries delivery `fps` (canvas `color=r=` parse, overlay clock) and `pick_fps` (the `fps=` filter's parse,
  `av_d2q`); equal for rates with small terms, different for `29.970029`. (`the same for every rate with small terms`)
  The plan holds live `PlanText`s (colour/box made safe, font file resolved once, bold known real or synthetic).

## 10-bit delivery

- Motion plans of `yuv420p10le`/`yuv422p10le` only (a still is always 8-bit 4:2:0). `GpuCaps::ten_bit` (`TenBitKinds`
  `P420`, `P422`, `PER_SLICE`) says which layouts are drawn, from `kerf-gpu`'s `delivery10` probe; other depths/layouts
  are `Unsupported::Delivery` whatever the caps. (`A 10-bit delivery`)
- The export builds each clip in the delivery format ahead of the 8-bit `overlay`, so `ten_bit_layer_refusal` is a positive
  list per layer (`DeliveryWith`): plain `yuv420p` of the composite's matrix filling the frame by its one fit `scale`
  (even sides), fades to black, nothing else. `ten_bit_frame_reasons` (`DeliveryFrame`): text over no clip, an SVG title,
  mixed matrices, a named scaler, a dither the thread range does not decide. (`is a positive list per layer`)
- `ten_bit_dither_rows(threads, rows)` is swscale's slice cut; the graph-level number is `None` when the possible thread
  counts disagree **or the graph is not known**. Counts come from `ten_bit_thread_range` (budget lowered to
  `wide_graph_cap`, the arithmetic `cap_wide_graph` writes with at spawn, for the run of the frame in
  `PlanCanvas::export_runs`); a compositor's pin wins. (`The counts are`)
- The graph is known only for a request that says its export renders the delivery alone (`PlanRequest::alone`); a
  multi-format export written in one run has every variant's graph in one. (`The graph is known only for a request`)
- Size is not a refusal: the ledger's small resolution judges rows as the export at that resolution; a frame with no clip is
  judged like any other. (`The size is not a refusal`)

## Title clip

- A title clip is no layer: a frame it is on names it in `RenderPlan::svg` (`PlanSvg` with `title: None`) after floating
  titles; the compositor refuses (`Unsupported::Svg`), FFmpeg draws from resvg. (`A title clip`)

## Two modes, fades, candidates

- `Still` is the contract the GPU path started with (`build_still_args`: one sampled transform, half-open spans, static
  text) **and** the FFmpeg still's own frame inside a transition (`engine/cli/still_fx.rs`): fades counted at output frame
  `k = frame_containing(t)`, travel read at `ffmpeg_frame_time(k)`, outgoing clip on tail judged at the slot start
  (`exact_time(k)` plus a nanosecond: a window closing inside a frame is open for it, one ending on a boundary is closed for
  the frame that starts there; `candidates_between` can look before `t`). (`Two modes.`)
- `Motion` is the export graph at output frame `k`: every expression (`enable`, keyframes, overlay position, `drawtext`) is
  read at `ffmpeg_frame_time(k)`; slot boundary is `exact_time(k)`; outgoing clip plays on its tail; a keyframed clip is
  never padded and always scales again (`LayerGeometry::resolve_with`/`Placement`); text on `between(t,start,end)`.
  (`is the export graph at output frame`)
- Fades, tails and travel are in both plans; a compositor says whether it draws them (`caps.fades`, `transitions`,
  `dip_white`), otherwise `reasons` refuses (`Fade`/`Tail`/`Travel`/`DipToWhite`). (`Fades, tails and travel are in both plans`)
- A fade is its integer factor (`FadeStep::factor_at_frame`: `65535 * i / N` truncated, complement for fade-out;
  `FADE_FULL` = filter does not run). `fade_ops(layer)`, `on_alpha_plane(layer)` (alpha chain: rotation, opacity < 1, mask,
  key, dissolve entered through, keyed rotation/opacity in an export frame: `fade` black is 0 there, 16 on `yuv420p`),
  `through_rgba(layer)`. Where the still's `fade` runs on the `fps=` timebase and the canvas parses the rate differently,
  a still inside a fade is `PickRate`, as an export frame always is. (`A fade is its **integer factor**`)
- A Motion plan holds **candidates** at a clip's closing edge (window contains the frame time, end included); `fps_pick`
  resolves them: drawn exactly when its pick names a frame. (`A Motion plan holds`)

## Which frame of which file (`frame_pick.rs`, `media.rs`)

- `PlanLayer.pick` is `AtOrAfter(t)` (still: first frame `-ss {:.6}` returns; `0.0` for a still image in a still plan),
  `Before(t)`, or `Fps(FpsPick)` (Motion: speed, direction, source window *with its tail*, clip start, output frame, `fps=`
  rational, padded-proxy clone dropped); `Pick::select(&SourceFrames { pts, time_base, start_us })` is the reference.
  (`Which frame of which file`)
- No tick arithmetic can wrap or panic: every timestamp difference goes through `diff`, every sum through `sum`
  (saturating; seek and container start summed exactly in `i128`, clamped); a sum that fits is the plain operator's.
  (`No tick arithmetic can wrap or panic`)
- The `fps` pick is arithmetic, not the first-order formula. Facts it encodes: `-ss` shifts by the seek rounded to a
  *tick*; `trim` keeps `[lo, hi)` after both ends are cut to whole microseconds; `reverse` emits last frame first with
  forward-order timestamps; `setpts` truncates (`D2TS` is an `(int64_t)` cast; rounding put a frame a slot late); `fps`
  rounds half away from zero; the stream ends where the frame `trim` dropped would have landed (for a window to the file's
  end, one frame interval past the last frame); nothing is drawn from that slot on (`overlay=eof_action=pass`).
  (`arithmetic, not the formula the A1 design measured`)
- A forward clip's picture reads on past its window (`picture_window`: one source frame, `ClipFx::frame`, 0.1 s when the
  probe has no rate, plus one output frame at its speed; a still's `-t` with it) and is cut at `end_frame`
  (`trim=end_pts=` after `fps`, a millionth of a frame of slack). `FpsPick.window` / `FpsPick.end`; the run ends at the
  earlier. (`reads on past its`)
- `tpad=stop_mode=clone` is not used: FFmpeg 4.4 crashes on a clone of a stream that had no frame. (`FFmpeg 4.4 crashes`)
- `FpsPick.pinned`: chain whose `fps` is pinned to the clip's start (`fps=fps=R:start_time=S`: moving zoom/position, keyed
  crop, moving still); not plain `round(pts * rate)` on a tie; the plan sets it from `Animated` (`places || crop ||
  still_motion`). (`pinned to the clip's start`)
- A clip is drawn on every frame from its first frame's slot up to the one before its end's; an equal-rate end is not
  drawn; a mid-frame end shows the nearest source frame, never more than half an output frame past the clip's length. A
  reversed clip keeps its window; its closing edge is the stream's. (`A clip is drawn on every frame`)
- The time base is an input (`start/TB` truncated to a tick) and so is the container start (`start_us`; `-ss` is relative
  to it). The export spells `-ss` and `trim` with `{}` of the `f64`; only the still uses `{:.6}`. (`The time base is therefore an input`)
- A window to the end of the file ends the stream the last frame's own **duration** (`SourceFrames.last_duration`) past
  it, not the last gap. (`A window that runs to the end of the file`)
- A still image is a stream: export reads it `-loop 1 -framerate <fps> -t <end>`; `Pick::Fps` carries `image: Some(fps)`,
  `SourceFrames::NONE`; not drawn on the frame its window closes on; `source_in` 0.25 starts at the first frame at or
  after a quarter second. (`A still image is a stream too`)
- Late-starting source (`FpsPick::lead` from `ClipFx::lead`): a window inside the lead is not seeked (`clip_seek(start,
  lead)`); `setpts` start is `start + picture_delay(...)` added in the graph's order; frames before the first picture are
  `Early` (nothing drawn). (`A late-starting source`)
- A rate whose canvas and `fps=` parses differ (`29.970029`) is refused in Motion (`Unsupported::PickRate`): the pick
  assumes one grid. (`is refused in Motion`)
- Entry points: `engine/cli/picked.rs` (`#[ignore]`d, both FFmpegs) renders numbered frames through the real export graph
  and compares every output frame (incl. `proxy/late-video-start`); run it on any pick change. (`engine/cli/picked.rs`)

## Blurred fill (`Fit::Blur`)

- Judged per layer and by render size: `PlanLayer::blur_fill` is `Some(picture)` only for a clip the fill suits
  (`engine::cli::blur_fill_clip`: identity transform, not keyed, no reframe, no mask, no alpha; the picture is the
  original's). `RenderPlan::blur_fill(layer, size)` resolves the thumbnail chain; `reasons` asks a compositor with
  `caps.blur_fill` **and** `caps.exact_scaler` for the layer that leaves bars. (`is judged **per layer and by the render size**`)
- Refusals: `BlurFill` without the caps (once a plan); `BlurFillWith(layer, why)` for a picture not 8-bit `yuv420p` (or never
  recorded), full-range, HDR, thumbnail shrunk past 63:1 (`MAX_SHRINK`, judged in 64 bits), chroma lines shorter than
  `2r + 1`, or a degenerate aspect (thumbnail cover side past `MAX_COVER` 16384, `BlurFill::resolve` `Err`). (`or **a picture of a degenerate aspect**`)
- A clip the fill does not suit is placed by Contain's chain byte for byte and is not refused for the delivery's fit.
  `LayerGeometry` (and `layer-geometry.ts`, `croppable`) still places as under `Contain`; the compositor makes the whole
  padded frame. (`is placed by Contain's chain, byte for byte`)

## FrameSource traps (A1b) and Pick::progress

- `showinfo` pts under `-copyts -start_at_zero -ss` are already start-relative: pass `start_us = 0`; use the decoder's
  best-effort timestamps (AVI has no frame pts); a streaming cursor needs `STARTPTS`, one frame of lookahead and the first
  frame past the window's end (or the last duration at EOF). (`that will feed it`)
- `Pick::progress(&read, eof)` is the cursor's rule (`NeedMore`, `NeedEarlier`, `Ready { shown, keep_from }`): answers
  exactly as the whole file would, as early as it can. `Pick::output_index(&read)` is how many frames the chain passed
  before the shown one (a still's pick 0; `Fps`: slot counted from the window's first frame), which frame-counting
  filters need (`vignette` dither). `FpsPick::seek()` is where the run begins; reverse needs every pts of the window; a
  non-all-intra transport stream may not deliver the picked frame after `-ss` (both builds). (`is that cursor's rule`)
- A fade on a layer with an alpha plane goes to luma 0, not 16 (`fade` on `yuva420p`; white 235 either way); `FadeStep` /
  `LayerFx::strength` do not know which black and must before A5.1 draws one. (`A fade on a layer with an alpha plane goes to`)

## A plan describes the file that is decoded (proxies)

- `PlanRequest { mode, color, media }` takes a `MediaResolver` (default `OriginalMedia`; `ProxyMedia` resolves like
  `Project::preview_source`). The Planner builds decoded assets once (path swapped, video stream replaced) so
  `ClipFx.hdr`/`head_pad` follow the opened file and `PlanLayer.stream` is the proxy's size/format, **while the delivery
  canvas still derives from the originals**. Resolve off the project lock, once per `Planner`. (`A plan describes the file that is decoded.`)
- `SourceMedia::decoded` carries the original's spherical projection onto the proxy stream. `SourceMedia` carries no
  identity: the frame-cache key is `kerf_core::source_identity(path)` (path, size, mtime) on the *decoded* path when a frame
  is asked, so a replaced file or new proxy is a new identity. (`carries no`)

## Spans and the hand-over

- `Planner::span(a, b, size, caps)` run-length-encodes `reasons(..).is_empty()`; `first_unsupported(a, limit, size, caps)`
  is lazy (playback asks every few frames); `SpanPlan::first_unsupported` answers from the runs. (`Spans and the hand-over.`)
- `SpanPlan::handover(a)` is where a stream taking over starts: `stream_preview` plays `Timeline::slice(start, ..)`, which
  zeroes the fade-in and drops `transition_in` of clips it starts inside, and shortens/drops the clip *before* a transition
  (dissolve shortens, dip's fade-out restarts, with the outgoing clip gone the incoming fades up from black). A clip is in
  the way from its start to the end of its fade-in/slide; one transitioning out, from `lead` before its end to its end
  *inclusive*; `stream_start` is the latest time at or before `a` no window is open at (windows chain); the caller drops
  frames before `first_shown = a`. (`is where a stream that takes over from the compositor starts`)
- `Handover.frame` is the whole cut's delivery frame and the caller must pin it (`ExportOptions::resolution` for the
  stream): with no project delivery frame the canvas derives from the clips, so a slice dropping the defining clip is cut
  for another frame. (`the caller must pin it`)

## render_bar.rs (pure) and PlayCut (`play_plan.rs`, pure)

- Render bar: `PlayCut::new(.., 0.0, ..)` over the whole cut; every grid frame judged and run-length encoded by state and
  kind into `BarSpan { start, end, state: gpu | stream | stall, kind, reason }`; `caps: None` = GPU preview off (only
  stalls); `waits` = Proxy-only proxy waits; a frame with no layer and no title is no span; a cut with no video none; a rate
  outside 1..60 fps is the stream's. Semantics in `frontend-timeline.md`. (`is the timeline's render bar`)
- `BarRequest::ask` is the host's hook, called with a frame's plan before it is judged when the layers on screen differ
  from the previous frame's (only with `caps`); for the GPU preview it is `kerf_gpu::prepare_ranges`. (`is the host's hook`)
- `PlayCut` is a `PlanMode::Motion` planner over `Timeline::slice(start, ..)`, so picks, fades, dissolves are the export's
  of that slice. `start` is a frame of the project grid (slice frame `j` is cut frame `k0 + j`) or the latest grid frame
  before a transition window `SpanPlan::handover` says the slice would cut off. (`is the cut a compositor that **plays** draws`)
- The slice is taken a nanosecond early (`SLICE_BIAS`): a clip placed after others sums their lengths and can start a
  float's width under the frame, and a slice that "cuts" it by that width drops its fade-in and transition. (`SLICE_BIAS`)
- Frame and rate are the **whole** cut's, pinned in the slice's options (a slice dropping the first clip would derive
  another canvas); a cut outside 1 to 60 fps is refused as the stream's rate is. (`pinned in the slice's options`)
- `PlayCut::ahead(from, horizon, size, caps)` finds the first frame not drawn (`PlayRefusal::Plan`) or drawn exactly but
  not in time: `Reversed` (cursor reads the whole window first), `MovingText` (keyed title position = a `drawtext`
  rasterisation per frame), `AnimatedTitle` (floating SVG title on a frame whose picture differs from the previous one,
  `SvgTitle::changes_within`, only when `may_animate`); returns `handover`. `PlayCut::resume(from, reach, horizon, size,
  caps)` is the first frame within `reach` from which every frame for `horizon` is drawn (planned in order, each once).
  (`is the lazy scan`)

## sweep.rs, A7 matrix concern, mask alpha

- `engine/cli/sweep.rs` holds the Motion plan against the *evaluated* graph (a small evaluator for `keyframe_expr`'s
  grammar: zoom/rotate/opacity, overlay x/y against `origin`, travel, title position and `between`, five frame rates);
  parsing `enable=` back out would only restate the builder. It checks the grammar at the output frame's time, not the
  graph's clock (that is `keyed_zoom.rs`'s). (`the Motion plan against the **evaluated** graph`)
- A Motion plan must not decide the composite's matrix per frame: FFmpeg 9 negotiates across the whole graph (first clip's
  tag is the composite's for the cut; every other-matrix clip is converted wherever it is). `PlanCanvas::cut_matrix` is the
  cut's matrix (under `BottomLayerTag`, in a mixed cut), other-matrix layers are converted (`MixedMatricesWith`,
  `matrix_reasons`); `mixed_matrices_cut` is the cut-level fact the graded layer is refused by GradeWith.
  (`**A7 concern, fixed**`)
- `kerf_core::mask_alpha_plane(&Mask, w, h)` is `mask_filter`'s `geq` evaluated in `f64` and truncated as `geq` stores it,
  at the size of the picture the `geq` sees (`LayerGeometry::picture`; for a placed layer the fit-size picture). SIMD rows
  (`kerf_core::simd`) are held byte for byte to `mask_alpha_plane_scalar`. (`**The mask's alpha is shared too**`)

## Grade

- The clip's grade is drawn by what the caps say: `GpuCaps::grade` is a `GradeKinds` bitset (`WHEELS`, `CURVES`, `LUT`,
  `ALL`; `A0` none); an undrawn part is `Unsupported::Grade(layer, what)`. `Compositor::caps()` is `CAPS` less `grade`
  until `kerf_gpu::grade::measured()` is `Match`, so the plan refuses a graded layer before anything is decoded.
  (`is drawn by what the caps say, and judged for the chain`)
- The rest is `GradeWith(layer, what)` (FFmpeg's grade is a YCbCr -> RGB -> YCbCr round trip, reproduced only where
  measured): picture not 8-bit limited-range `yuv420p` (`grade_chain_refusal`); a matrix with no measured round trip (BT.601,
  BT.709, untagged are drawn; `grade_route()`, `PlanStream::grade_tag()`); a video effect other than a chroma key after
  the grade; a fade to black/white running on it (a clip's own `fade_in`/`fade_out` is one; a dissolve's alpha ramp is
  not); a dip to white; a cut mixing matrices under a negotiating policy (`mixed_matrices_cut`, the BT.709 label
  included); a layer the compositor's scaler resizes unless `caps.exact_scaler` **and** an `eq` precedes the grade
  (`eq_in_chain`; otherwise the one-level scaler difference becomes 3-4, past the bound of 2); an odd side; for a
  compositor without `GpuCaps::matte_effects`, `Matte(layer, "a grade")` on a letterbox (with the cap: even sides, 8-bit
  `yuv420p`, no mask, no keyed number: `padded_refusal`). (`a colour matrix with no measured round trip`)
- `RenderPlan::graded()` includes a grade, so `GradedFullRange` / `GradedInFullRangeStack` judge it too. Helpers:
  `kerf_core::baked_lut`, `grade_filter_chain`, `frame_matrix_tag`. (`includes a grade, so the full-range refusals`)
- Without `caps.exact_scaler`, a grade on a scaler-resized layer is judged by its tables (`GradeSteep`, from
  `reasons(size)`, each frame of a keyed one too; with the cap never): steepest neighbouring step of the Y, U, V tables
  (`eq::table_steps(&layer.color)`, black point included) past `MAX_AMPLIFIED_ERROR` (2) is FFmpeg's; geometry as the
  sharpen rule; unscaled layer exact at any step. (`is judged by its tables`)

## Colour, keyed mask/crop/placement, Animated.keys

- `PlanLayer.color` is `Clip::color_at(local)` in a still; in a Motion plan with keyed colour it is the numbers the
  export's `eq` evaluates at that frame: `engine::keyed_color_value(clip, T)`, `T = ffmpeg_frame_time(k)` on the clip's
  `fps` grid, each keyed number through `keyframe_value`, unkeyed ones the clip's own, temperature as
  `Color::temperature_gammas`; `sweep.rs` holds them to the evaluated text to the bit at five rates. (`**Colour and partly-keyed transforms in the plan.**`)
- `eq` rebuilds its tables every frame: Motion needs `GpuCaps::keyed_color` or is refused `KeyedColor`. A keyed `eq` is in the
  chain on every frame, neutral numbers or not, so `GradedFullRange` etc. judge `RenderPlan::graded(layer)` (colour not
  identity, or in Motion any keyed colour): a full-range clip is refused on neutral frames too. (`neutral numbers or not`)
- Keyed mask: `PlanLayer.mask` is `Clip::mask_at(local)`; a still draws it as a static mask; a Motion plan is refused
  (`Unsupported::KeyedMask`, no cap). Keyed crop refused in both modes (`KeyedCrop`): export and still read the window
  through `perspective` (fixed-point, bilinear), no compositor pass draws it; the transform keeps only the static crop.
  (`the layer's transform keeps only the static crop`)
- Moving zoom/position (`Animated.places`, `Clip::placement_animated`) is refused in both modes (`KeyedPlacement`) unless
  `GpuCaps::placement`, which kerf-gpu sets only once a probe of the FFmpeg in use finds `pad`/`perspective`/`crop` place a
  picture as `kerf_core::perspective` does (`placement_probe` over `engine::placement_probe_chain`, the export's own filter
  text, and `placement_probe_motion`; a probe that cannot tell is `Unknown` and refuses). (`only once a probe of the FFmpeg in`)
- With the cap a placed layer is still refused for: rotation, opacity < 1 or dip to white ahead of it (`ZoomBehind`, moving
  position too; a grade is drawn behind it); odd-height picture (`Geometry`: `pad` keeps or loses the last row by how the
  graph allocated the frame); letterboxed picture at the identity pose in a still (the still pads to the whole frame first,
  opaque bars); picture not 4:2:0 or gray (`PlacedFormat`; planar 4:4:4/4:2:2 drawn where `GpuCaps::placed_chroma` says
  the probe matched); a moving still image (`StillMotion`'s own graph); a pose leaving the table's range
  (`PlacementFrame::reachable`). (`for an odd-height picture`)
- A placed layer whose asset did not record its range (`color_range` `None`), where the composite's range is negotiated, is
  judged by its file for a host that asks files ahead (`GpuCaps::file_range`, set by `session_caps()`, not by `A0` or
  `Compositor::caps()`, so a plan judged without it stays a pure function of its inputs): `reasons` reads
  `source_is_full_range_known` (cache only; spawns and waits on nothing) and refuses (`PlacedFormat`) a full-range or
  unsaid file. (`is judged by its file`)
- `Animated.keys` (`Keyed { scale, rotation, rotates, opacity }`) says which numbers are keyed because a clip keyed in part
  is not built like one keyed in full: keyed zoom = second `scale` even at 1 (unkeyed: static scale, second only if not 1);
  keyed turn = `hypot` box (unkeyed: tight `rotw`); keyed opacity = `geq` alpha, unkeyed = RGB round trip (odd side:
  `translucent_odd`). `TranslucentMatrix` is skipped only for a **keyed opacity**, not any keyed number. `Placement::keyframed`
  carries it (`Keyed::all(rotates)` legacy); a colour-only clip is placed as a static one. (`because a clip keyed in part is not built like one`)

## Live behaviours, stabilized, translucency

- Live behaviours (`Clip.behaviors`) are in the plan as the pose they give: `PlanLayer.transform` is `Clip::transform_at`;
  `Animated` is built from `is_driven` / `rotation_turns` / `zoom_animated` / `placement_animated` (sway/spin = keyed
  rotation, breath = moving zoom, drift/shake/breath = `places`). `Animated.behaves` (`Clip::has_behaviors`; one that
  moves nothing is not live) refuses every frame, Still and Motion, `Unsupported::Behavior(layer)`, until
  `GpuCaps::behaviors` (kerf-gpu sets it); one that turns the picture (`Animated.rotates`) is refused whatever the caps.
  (`Live behaviours`)
- A stabilized clip is refused in Still and Motion (`Unsupported::Stabilized`): vid.stab/`deshake` frames no pass draws.
  (`A stabilized clip is refused`)
- A layer below full opacity (`RenderPlan::through_rgba`: constant opacity < 1, in a still a sampled keyed one too; an export
  frame's keyed alpha is a `geq`, not this; a mask or key on it is drawn) takes FFmpeg's `yuva420p -> argb -> yuva420p`
  round trip around `colorchannelmixer`; the compositor reproduces it to the sample on x86 (`roundtrip_probe`).
  `GpuCaps::translucent_approximate` (set unless the probe says `Match`; `A0` leaves it off) makes `reasons` refuse every
  such layer, `TranslucentApproximate`. Its turn (see Rotation), unknown matrix (`TranslucentMatrix`) and a fade to
  black/white on it stay refused whatever the caps. (`A layer below full opacity`)
- Odd side on a translucent layer (`LayerGeometry::translucent_odd`; `resolve` no longer refuses, the render's caps do):
  drawn where `GpuCaps::translucent_odd` is set (only with `translucent_approximate` off), else `Unsupported::Geometry`.
  With the cap `translucent_odd_refusal` keeps FFmpeg's what the model was not held to: a side under
  `TRANSLUCENT_ODD_MIN_SIDE` (16), a turn, a blend mode, a grade, an effect other than a chroma key; dip to white and fade
  to black keep their own refusals. The frontend port (`layer-geometry.ts`) and corpus lost the same refusal.
  (`A picture with an odd side`)
- `tests/coverage.rs` pins the `Unsupported` variants' ordinals (`TranslucentApproximate` 58, `FullRangeStill` last, 59);
  a new variant must choose a `kind()` and shows up there. (`the last variant but one of the enum`)

## Full range in a still

- Judged by the range, not a band: `FullRangeResized(layer, "in a still")` (swscale converts the range inside any resize,
  either FFmpeg) and `FullRangeStill(layer)` (at its own size where the composite's colour is negotiated, i.e. policy not
  `FixedBt601`/FFmpeg 9: the still keeps the picture's range through the overlay and converts last, so **any layer on
  screen** that may be full range, a PNG over full-range footage and text over it included, refuses the frame). A picture
  flagged `pc` in any YCbCr format counts (`flagged_full_range`); the depth heuristic of `maybe_full_range_in` does not (a
  limited 10-bit picture is exact in a still). FFmpeg 6 converts `yuvj` at decode as the compositor does, so footage and a
  JPEG at the canvas size stay drawn there. An export refuses the whole cut (`FullRangeCut`).
  (`is judged by the range, not by a band`)

## Blend, rotation, speed ramp, folded fit

- A blend mode (`PlanLayer.blend`, `Unsupported::BlendMode(layer, mode, cause)`, Still and Motion): FFmpeg mixes in RGB after
  a conversion each way and two placements (`engine-cli.md`, "Blend modes"). The compositor draws the modes in
  `GpuCaps::blend` (`BlendKinds`, per matrix, from the probe). Every cause is its own reason, with the cause as the string,
  so the ledger ranks them: mode not in caps; composite matrix with no tables (`blend_matrix` from `PlanCanvas::blend_tag`:
  bottom layer's tag over the whole cut for an export, over the layers on screen for a still; BT.2020 needs `BlendExtras`
  `BT2020`); an RGB picture (unless `RGB_PICTURE`); a graded clip; a moving zoom/position (`pose` is `Some`); a layer below
  full opacity unless `BlendExtras::TRANSLUCENT`; from `reasons(size)` a rotation that is not a quarter turn or is keyed, and
  a letterboxed layer (`geom.matte`). (`**A blend mode**`)
- The blend matrix tags are an allow-list: a bottom layer (`blend_tag_unlisted`) or blended clip whose matrix
  `model::matrix_tag_is_unlisted` calls unlisted (`bt2020c`, `ycgco`, `ictcp`, `chroma-derived-*`, `smpte2085`, `gbr` on
  YCbCr; as `fcc`, `smpte240m`) is refused whatever the policy. A resized layer is no blend cause (the blend chain begins
  after the fit pass; an unmeasured scaler is `ScalerApproximate` for every layer). `normal` refuses nothing.
  (`the tags are an allow-list`)
- Rotation is drawn only as a quarter turn `rotate` lands on whole samples for (`Unsupported::Rotation`, from
  `size_free_reasons`, every layer): `quarter_turn(angle)` ports the filter's integer sine and fast path (`vf_rotate.c`,
  within `FLT_EPSILON` of 0, pi/2, pi, 3pi/2) and answers only where `c` and `s` are exactly 0 and +-65536 (the quarters
  -47, -46, -43 to 40, 43, 44 and no others up to 1000 radians; 90.00001 degrees is refused like 91). Any other angle, a
  rotation a key or a behaviour moves (`hypot` box turned per frame), and a huge or non-finite angle is refused; a clip
  whose keyed rotation never turns has no `rotate`. (`A rotation is drawn only as a quarter turn`)
- Quarter / three-quarter turn of a layer below full opacity (constant: `LayerGeometry::translucent`) is `Rotation` too,
  from `reasons`: the export converts the turned RGBA to 4:2:0 while the compositor's round trip comes before its turn.
  Half turns and upright layers keep the translucent ladder; keyed opacity and a fade of an opaque layer unaffected.
  (contradiction in the full file: the translucent paragraph says a turn "a quarter, three quarters or a half" of such a
  layer stays refused whatever the caps, this paragraph says half turns keep the ladder: see the pointer and
  `A layer below full opacity`.) (`A quarter or three quarters of a turn of a layer below full opacity`)
- A speed ramp is refused (`PlanTiming.ramped`, `Unsupported::SpeedRamp`, `size_free_reasons`, Still and Motion, every cap):
  the export retimes with a `setpts` expression and lets `fps` pick, a pick `frame_pick.rs` does not model; a still's `-ss`
  names another frame than the one shown. The plan stays exact elsewhere: `clip_source_time` is `Clip::along`, `speed` is
  `Clip::mean_speed`, and a Motion `FpsPick.speed` is **infinite** (`run` refuses a non-positive speed, so the pick names
  no frame rather than a wrong one; infinite not NaN, which would make the plan unequal to itself). Readers kept honest:
  scrub window `Snapshot::plain` leaves a ramped clip to the one-shot still; export in parts neither restarts a ramped still
  nor plans cost at `speed_mag`; thumbnail suggestions use `source_to_timeline`. (`A speed ramp is refused`)
- A zoom that is part of the fit is refused (`PlanLayer.fit_order`, `Clip::fit_order(canvas.fit, cover_overflows)`;
  `engine-cli.md` "A zoom is part of the fit"): FFmpeg takes it in **one** `scale` to frame times zoom (`Folded`: zoom in
  under Contain; `Uncropped`: Cover clip that zooms/pans with the cover hanging over, or any Cover zoom in) where
  `LayerGeometry` and the compositor fit then scale. `size_free_reasons` pushes `FoldedFit(layer, which)` in both modes for
  every cap until the compositor draws the one-stage picture with parity cases. A still sampling a keyed pose to identity,
  a moving zoom, zoom out under Contain, a same-shape Cover clip that moves/zooms out, masked or vignetted clips stay
  `Fitted` (so a webcam PiP over 16:9 keeps drawing). (`A zoom that is part of the fit is refused`)
- `LayerGeometry` is unchanged (still fit-then-zoom, also `layer-geometry.ts`); drawing folded fits needs a one-stage
  `LayerGeometry` (`scaled` = folded box, `keep` = centred window), the port and corpus following, parity on both FFmpegs.
  (`is **unchanged**`)

## Moving still images

- A still image whose picture moves has two routes and the plan says which (`Animated.still_motion` = the export's
  eligibility, `engine::cli::StillRoutes` = `still_motion_for` + `StillStatic::of_clip`, asked once per clip; a move that
  leaves the frame bare at any frame is not eligible and is drawn as footage with a moving zoom). (`has two routes in the graph, and the plan says which`)
- Moving-still route (`PlanLayer::still: Some(StillLayer)`, `takes_still_route`, `kerf_core::still_motion`): the export's
  scaled copy (`once`) and one `perspective` per frame whose four corners the plan evaluates (`StillMotion::corners`, the
  expression's operation order, pose from `property_at`). A still plan depends on the render size and is worked out by
  `RenderPlan::for_render(size)`: every decoding compositor entry point does it (`render_plan`, `render_plan_with`,
  `render_plan_texture_with`, `render_draft_texture_with`) and a caller that decodes and composites by hand must too;
  `composite_*` from a plan not worked out is `Unsupported`, never a draw from a picture the compositor scaled itself.
  (`**The moving still's route**`)
- `StillMotion` is refused for: a picture kind not measured (`GpuCaps::still_motion: StillKinds`, `yuvj420p`/`rgb24`, from
  `still_probe`); full-range where the range is negotiated; colour/grade/effect/mask/opacity < 1/dip to white; a render size
  other than the one the move was made for (an export at another size is the plan of that export, `ExportOptions::resolution`,
  not this plan drawn smaller; `tests/still_route_size.rs`); a keyed colour; a chain not measured (`StillChain`); a copy of
  odd side or over `MAX_COPY_SIDE`; a `perspective` with no table (`corners_have_a_map`); a per-pixel table over
  `MAX_TABLE_BYTES` (128 MiB); all in `reasons`. Its geometry is the frame-sized picture (no stage/fit order/placement, no
  `pose`), so `FormatResize`, `FullRangeResized`, `Geometry`'s chroma grid and `FoldedFit` are not its reasons; the blend
  gate is `takes_still_route`, not a pose or `places`. (`is refused for: a picture kind the compositor did not measure`)
- A still whose placement is drawn (`PlanLayer::fit: Some(FitCopy)`, `fits_by_ffmpeg`: `yuvj420p`/`rgb24` with a moving
  zoom/position, not the route above, nothing between fit and placement) has FFmpeg make the fit picture (`FitCopy::filters`,
  stamped by `for_render(size)` and, for an export frame, by the planner at the delivery frame), so `PlacedFormat`,
  `FullRangeResized`, `FormatResize`, `Shrink`, `Geometry` chroma-grid are not its reasons while `ScalerApproximate`, odd-pad
  `Geometry`, `MixedMatricesWith`, `FullRangeNegotiated`/`FullRangeStill` remain and colour is judged by the clip
  (`eq_in_chain`). An unmeasured chain, and `Cropped` always, is `StillMotion`. (`FFmpeg make the fit picture`)
- A moving still that is neither (colour, grade, effect, mask, rotation, opacity < 1, dip to white, or an unmeasured picture
  kind) is refused `StillMotion`: footage placement would use the compositor's decode and scaler, which no case holds a
  still to. (`would draw it from the compositor's decode`)

## GpuCaps, reasons, keyed alpha

- What a compositor may draw is data: `GpuCaps` (`Compositor::caps()`; bits `motion`, `fades`, `transitions`, `dip_white`,
  `keyed_opacity`, `keyed_zoom`, `keyed_color`, `mask`, `text`, `reframe`, `hdr`, `GradeKinds`, `EffectKinds`, `BlendKinds`,
  `BlendExtras`). `RenderPlan::reasons(&caps, size)` is a pure function of the plan's fields and lists **every** cause, not
  the first (the ledger in `gpu.md` ranks by frames lifting one alone would draw, true only if none hides behind another);
  frame drawn iff `reasons().is_empty()`; nothing is decided while planning, so one plan answers for any caps. (`**What the compositor may draw is data**`)
- A refused layer still gets its other reasons (geometry-refused layer judged on the picture's own chroma grid; a grade on a
  letterboxed scaler-resized layer is `GradeWith` and `Matte` without `matte_effects`; a dip to white lists each of
  `Letterboxed`, `OddSize`, `Resized`). (`lists **every** cause of a refusal, not the first`)
- Motion plan refused as a whole until `caps.motion`; moving keyed zoom until `caps.keyed_zoom` (`KeyedZoom`: export draws it
  last at the output frame's time; the compositor with the cap draws grade, effects, mask, fades at fit size and zooms
  after); keyed opacity until `caps.keyed_opacity` (`KeyedOpacity`: a `geq` alpha); a non-`yuv420p` delivery (10-bit layouts:
  see above) and a gif are refused. (`A Motion plan is refused as a whole until`)
- `PlanLayer::keyed_alpha` is the `geq`'s alpha in an export frame, `(int)(opacity(T) * 255)`: curve through
  `engine::keyframe_value` (`keyframe_expr`'s arithmetic in its own order, same segment by the same comparisons, constants
  the `f64`s `av_expr` parses back; `sweep.rs` holds it to the bit) at `T = ffmpeg_frame_time(k)` on the clip's `fps` grid;
  `None` in a still and for a curve that never leaves 1. (`which samples the opacity`)
- That `geq` is the mask's too (`PlanLayer::keyed_opacity` is the curve's `f64` that `keyed_alpha` truncates; under a mask the
  `geq` multiplies it with the mask's keep before truncating), so a moving zoom (or a blend on a key/mask) is judged by
  `keyed_alpha < 255` as well as sampled opacity (`alpha_plane_refusal`, `zoom_order_refusal`; a mask is drawn under it).
  A chroma key under a keyed opacity is drawn (one `geq` multiplies the curve into the key's alpha plane). (`is the mask's too`)
- A keyed opacity on a turned layer is not refused: that `geq` runs ahead of the `rotate`, so on 6.1 the replaced row/column
  land on the turned box's other edges and the compositor shifts the picture's planes before the turn (gpu.md).
  (`A keyed opacity on a turned layer is not refused`)
- With `keyed_zoom` the plan still refuses a rotation, an opacity below 1 and a dip to white in front of a moving zoom
  (`ZoomBehind(layer, what)`, Still and Motion: the placement would act on the rotated alpha plane, the RGB round trip's or
  the RGB frame; no case holds them to FFmpeg; a grade is drawn ahead of the placement); without it a Still refuses a grade,
  rotation, opacity, mask or effect in front of one. (`in front of a moving`)
- Old API stays: `gpu_supported()` / `unsupported_reasons()` / `unsupported_reasons_at(size)` / `gpu_supported_at(size)` are
  `reasons(&GpuCaps::A0, ..)`: refused while a fade step is live, a layer travels or a tail window is open, exactly. (`The old API stays`)

## Fades, dip to white, full-range policies

- Still refused whatever the caps: `FadeOnTranslucent` (a black/white fade on a layer with opacity < 1: FFmpeg fades the RGBA
  picture `colorchannelmixer` leaves, alpha included; a dissolve's ramp touches alpha alone and is drawn). (`(specific reasons, whatever the caps)`)
- `DipToWhite(layer, why)` (with `caps.dip_white` = `grade::kernels() == Match`, x86-64; arm64's kernel makes it FFmpeg's) is
  drawn only on 8-bit limited-range untagged/BT.601/BT.709 `yuv420p`, and refused for: alpha plane; a black fade of the clip
  ahead of it; `yuv420p` flagged full range (`DipWhy::FullRange`, every frame of the clip); `invert`/`vignette` on the clip
  (`DipWhy::RgbEffect`; `gblur`, `unsharp`, `hue`, `eq` stay in YUV); any other pixel format or matrix; from `reasons(size)`:
  letterboxed (`pad` runs before the round trip), odd side, any layer the scaler resizes. (`a dip to white is `)
- The dip refusals are the Motion plane bound's and are conservative in stills on purpose; `reasons()` is one function for
  both modes. (`are the **Motion** plane bound's and are conservative in stills on purpose`)
- `FullRangeNegotiated(layer)` (a still): a `yuvj`/unrecorded picture inside a running fade, dip or dissolve where FFmpeg 9
  works on raw values and converts range afterwards. A `yuv420p` flagged `pc` (`PlanStream::raw_full_range`) gets it under
  every policy and in an export frame. (`gets it under`)
- `FullRangeCut` (export frame; plan-level, `PlanCanvas::full_range_cut`, set by the `Planner` when any clip `maybe_full_range()`):
  FFmpeg 9 flags the whole export full range, so under any policy but `FixedBt601` no frame of a cut that holds a full-range
  clip is drawn (before, after, other track, gap). FFmpeg 6 draws it all except (a) `yuv420p` flagged `pc` letterboxed in an
  export (`Geometry`, "flagged full range", any policy but `BottomLayerTag`), (b) a dip to white on such a picture on every
  frame (`DipWhy::FullRange`), (c) `FullRangeResized` (either FFmpeg, from `reasons(size)`): a full-range picture the scaler
  resizes in an export frame, inside a fade, and in a still. (`no frame of a cut that holds a full-range clip`)
- `maybe_full_range()` is true for a `yuvj...` name, no recorded format, or `yuv420p` flagged full range (`StreamInfo.color_range
  == "pc"`, recorded since schema v10; VP9/HEVC/AV1 probe as plain `yuv420p`); an asset stored before v10 has `None` and is
  judged by name as ever. `flagged_full_range()` is the flag in any YCbCr format (RGB/gray read `pc` always, so it says
  nothing for them). Under a policy that is not `FixedBt601`, `maybe_full_range_in(policy)` also counts YCbCr flagged `pc` in
  any format and any deeper-than-8-bit YCbCr/gray format (`pix_fmt_is_high_bit`) whatever it says, because a file may say
  nothing of its range (used by the cut flag, `FullRangeNegotiated`, grading refusals; on 6.1.1 only the name and a
  `yuv420p` flag decide). (`is true for a`)
- A flagged `yuv420p` on 6.1.1: nothing converts it ahead of the filters, so raw samples meet vignette, key, `eq`, fade,
  effects on every FFmpeg (refused under any policy); a plain layer and a cut are exact there. Only a clip the export holds
  sets the cut flag (not a muted track, disabled clip, solo-shadowed track, or an audio track's use of the asset).
  (`Only a clip the export holds sets the cut flag`)

## gpu_supported_at: effects, masks, matte, scalers

- `gpu_supported_at(size)` is the per-frame fallback switch for anything the compositor does not render exactly (`gpu_supported()`
  its size-free half; `unsupported_reasons_at(size)` says why). (`is the per-frame fallback switch`)
- Effects (`Effects(layer, what)`): a blur is refused when a plane (luma; chroma at half size rounded up) is one sample wide
  or tall, has fewer rows than the graph has slice threads, or (vector of rows a slice) is over a plane no wider than a
  vector; **not at sigma exactly 0**, the only blur lifted (`gblur::plane_hazard_for`, `kerf_core::gblur::picture_hazard_for`;
  sigma 0 still refused for the width case: the kernel reads past the row). A tiny sigma `Gaussian::is_identity` calls the same
  (1e-5, 1e-9) is not lifted: cancellation the blur probe does not measure.
  (`is not the recursion on every plane shape at every slice-thread count`)
- Hazards are judged over `RenderPlan::graph_thread_range(caps)`: FFmpeg's default thread count where
  `kerf_core::ffmpeg_cpu_count` reproduces its core count (Linux `sched_getaffinity` of the calling thread, read at each
  judgement; `None` elsewhere or on failure), else `1..=16` (a blur is judged over all of it; a chroma key, drawn at one
  count, is refused: "a chroma key over a graph whose slice-thread count is not known"); `GpuCaps::blur_lanes` and
  `graph_threads` are the width and the pin. (`a chroma key over a graph whose slice-thread count is not known`)
- `gblur`/`unsharp` refuse themselves: sigma outside 0 to 1024, amount outside -2 to 5, and a sigma whose coefficients `gblur`'s
  double-precision cancellation leaves outside what the shader computes (`kerf_core::gblur`; `gaussian(sigma)` is `None`
  exactly where `Gaussian::in_shader_domain` is false; the compositor takes coefficients from the same function).
  (`sigma whose coefficients`)
- By what the picture changes: invert/blur/sharpen on a full-range or never-recorded picture; invert on gray; vignette on
  anything but limited-range `yuv420p` (`vignette` works in the picture's format); chroma key on anything but `yuv420p`, on a
  rotated layer, or a blended one below full opacity (a key under constant or keyed opacity is drawn); a chroma key after a
  scale ahead of the effects where `!caps.exact_scaler` (alpha moves up to 1/blend levels per chroma level; where exact it is
  drawn, alone or after blur/sharpen/invert/grayscale/vignette); a picture under two samples a side; similarity/blend
  `chromakey` values outside 0.00001 to 1 and 0 to 1. A blur is no longer a reason: `GpuCaps::effects` holds it only where
  `blur_probe` measured the kernel. (`**a blur is no longer a reason**`)
- A sharpen after a scale that is not exact (`!caps.exact_scaler`): worst case starts at 1 (the grade's steepest luma step if a
  scaled layer is graded) and each later non-zero sharpen adds `ceil(2 * err * |amount|)`; refused past 2 (one sharpen above
  0.5, or a second). Where the scaler is swscale's to the sample a sharpen is exact at any amount, chained, after a blur.
  (`a sharpen after a scale **that is not exact**`)
- Masks: `Mask` without `caps.mask`; `MaskWith(layer, what)` on a rotated layer, over a chroma key, or on a blended layer
  below full opacity. A mask under a constant opacity is drawn level by level (`layer_geometry::ffmpeg_alpha_of`), under a
  keyed one as the `geq`'s own `f64` product (`keyed_opacity`, `mask_alpha_plane_keyed`). A key's alpha follows
  `alpha_plane_refusal` too. (`under a constant opacity is drawn, level by level`)
- `Matte(layer, what)`: chroma key on a letterboxed picture, and blur/sharpen/vignette/grade for a compositor without
  `GpuCaps::matte_effects`: `pad` hands the effects the padded frame, bars included; a mask is cut from that frame and drawn
  at the layer's size. With the cap the compositor lays out the padded frame whole (blur kernel hazards judged on its planes);
  `Effects(layer, ..)` names what the lift is not held for (`padded_refusal`: not 8-bit `yuv420p`, a mask, a clip with keyed
  numbers). (`for a chroma key on a`)
- Two more: a sharpen on a frame under 8 rows (`UNSHARP_MIN_ROWS`; the padded frame of a letterbox) and a grade on a
  letterbox with no `eq` ahead at an odd offset (`GradeWith`: `(canvas - picture) / 2` odd on either axis,
  `letterbox_offset_is_odd_without_eq`; a blurred fill is exact). (`Two refusals beside it`)
- Also refused: 360 reframe, HDR, text it cannot reproduce, a fade/transition in progress, a delivery scaler not measured
  (`bicubic` and none drawn wherever the scaler is exact; `bilinear`, `lanczos`, `neighbor`, `spline` only where
  `GpuCaps::scalers` holds the name, i.e. `kerf_gpu::scaler::kernels()` proved its tables: `Unsupported::Scaler`, settled
  `frame_wide` only for a spelling the engine's list lacks, `ScalerKinds::of_name`); a still whose fit picture FFmpeg makes when
  the delivery names one of those four (`scaler_named_other`); a moving still on the route of a delivery naming one unless
  the caps hold it (`StillKinds::named_route`, `still_route_scaler`); a layer with no known picture size; a shrink steeper
  than `max_shrink_for(canvas.scaler)` (`MAX_SHRINK` 63:1; `MAX_SHRINK_LANCZOS` 42; `MAX_SHRINK_SPLINE` 18, whose tables reach
  swscale's 256 taps and cascade a second scaler). (`a delivery scaler the compositor has not measured`)
- `Unsupported::kind()` names a refusal without layer names/numbers (`mask`, `mixed_matrices`, `text_sequence`, ...); an
  exhaustive `match`, so a new variant must choose one; the GPU preview counts refusals by it (`app.md`). (`names a refusal without its layer`)

## Text

- `caps.text` is set by kerf-gpu only for a build whose `drawtext` it can take apart glyph bitmap by glyph bitmap and whose
  colour conversion it knows. The font is always a file the plan can name: `fonts::resolve_text_font` (installed family
  else shipped Geist, `Regular`/`Bold`, written to `<cache>/kerf/fonts/`, checked on each call) is called by the graph
  (`drawtext_common`) and the planner alike, so `PlanText.font_file` is exactly the `fontfile=` the still/export pass;
  `TextWithoutFont` only when the default could not be written. (`always a file the plan can name`)
- Helpers: `text_still_filter(&PlanText, h)` (a test holds it byte-equal to `drawtext_still`), `text_raster_graph`,
  `text_piece_filter` / `TextPiece`, `text_as_drawn`. (`the still draws for a plan's overlay`)
- Refused: `Text` (no `caps.text`), `TextWithoutFont`, `TextSequence(why)` (`text_sequence_limit`: a tab, over
  `MAX_SEQUENCE_CHARS` = 160 characters, or too wide for `MAX_MEASURE_SIDE` = 16 384 px at the render size; judged by
  `reasons`). Synthetic-bold borders and print under 12 px are not refusals (blended in turn); what the compositor then
  cannot reproduce exactly is refused by the render (`GpuError::Unsupported`), not the plan. (`TextSequence(why)`)

## Animated SVG titles (`Timeline.svg_overlays`, `plan_svg.rs`)

- resvg draws, FFmpeg overlays the pictures; the compositor draws the same PNG's planes with `overlay`'s arithmetic
  (`gpu.md`). `RenderPlan.svg` is `Vec<PlanSvg>`: a floating title carries `SvgTitle` (artwork `Arc<str>`, `FontSet` from
  `fonts::snapshot().named_by` taken once by the `Planner`, placement, clamped opacity, `doc_t`); a title clip is `title:
  None`. (`Animated SVG titles`)
- Nothing re-derives the export's arithmetic: the `Planner` reads `svg_layer::sequence_of` once (frames the title is on,
  `window()` over the cut's own length, `doc_t0 = max(0, offset + exact_time(first) - start)`); Motion frame `k` is on iff
  `first <= k <= last` with clock `doc_t0 + exact_time(k - first)`; a Still is `start <= t < end` at `doc_time(t)`
  (`still_svgs`); rectangle `svg_layer::rect_of` (`None` when the export leaves it out: wholly off frame or over
  `MAX_PIXELS`); picture `svg_layer::picture_png` (the one function export, still and compositor draw with), key
  `picture_key`. (`Nothing re-derives the export's arithmetic`)
- `reasons` pushes `Unsupported::Svg` (title clip; frame-wide, nothing lifts it) and, for a floating title,
  `SvgOverlay(SvgWhy)` (first cause only): `Unmeasured` (no `caps.svg`: set only where the compositor measured this FFmpeg's
  `overlay` of a picture, `kerf_gpu::svg`), `NoLayer` (no clip under it), `Matrix(m)` (composite not BT.601: FFmpeg 9
  converts the picture with the composite's matrix). `svg_overlay_chains` is the export's text for a probe. (`first cause only`)

## Pixel format, composite matrix policy

- Pixel format is judged from a positive allow-list (`pix_fmt_layout` in `model.rs` -> `PixLayout { Yuv420, Gray, OtherYuv,
  Rgb }`): a format not known to be opaque is refused, so names a deny-list never thought of (`ayuv`, `vuya`, `rgb32`
  aliases) cannot reach a compositor that would flatten alpha onto black. (`Pixel format is judged from a positive allow-list`)
- `pix_fmt: None` marks an asset saved before it was recorded: its colour matrix is unknown (no `color_space` = untagged or
  never probed; a BT.709 clip of an old project must not be taken for BT.601): refused when translucent, and under
  `BottomLayerTag` whenever in the stack. (`its colour matrix is unknown`)
- The composite's matrix is a property of the FFmpeg, probed (`CompositeColorPolicy` from `engine::composite_color_policy()`,
  once-per-process behavioural probe in `cli.rs` like `graph_script_flag` / `zscale_available`): a raw-YUV clip tagged BT.709 and
  an untagged twin, **checked with `ffprobe` that the tag survived**, through the real `build_still_args` graph, compare the
  middle pixel: same = `FixedBt601` (FFmpeg 6.1), different = `BottomLayerTag` (FFmpeg 9.0). (`The composite's matrix is a property of the FFmpeg, probed`)
- A probe that cannot tell is `Unknown`, never a guess (falling back to `BottomLayerTag` as "cautious" drew BT.709/2020 clips
  wrongly on FFmpeg 6; the policies differ on exactly that footage). `Unknown` draws only stacks both agree on (BT.601
  throughout) and refuses the rest. A measured policy is kept for the process; a failed probe is retried after a backoff
  (5 s doubling to 5 min). (`A probe that cannot tell is `)
- The probe is bounded (`POLICY_PROBE_TIMEOUT` 15 s, six short runs on side threads, killed at the deadline) because every
  concurrent first caller waits; the first call blocks 70-200 ms, so call it from a blocking thread. The policy is an input
  to `RenderPlan::at`, so tests pin any of the three without an FFmpeg. (`the first call blocks 70`)
- Under `BottomLayerTag` a stack whose layers share one matrix class (BT.709, BT.2020, BT.601; `smpte170m`, `bt470bg` and
  untagged are one class) is drawn with that matrix; an unknown one and an RGB picture in a stack that is not BT.601 are
  refused. (`and an RGB picture in a stack that is not BT.601 are refused`)

## Mixed matrices

- FFmpeg's conversion of other layers into the composite's matrix is reproduced for a plain layer (`gpu.md`, `convert.rs`);
  `matrix_reasons` keeps `MixedMatricesWith(layer, why)` (cut-level `MixedMatrices`) for a layer whose matrix is not
  `PlanCanvas.matrix` and that `RenderPlan::convert_refusal` does not clear: grade, effect other than invert/grayscale (and
  those in a still), mask, rotation or opacity, `eq`, keyed number, format other than 8-bit limited-range `yuv420p` /
  `yuv422p` / `yuv444p` video, a fade to black running in a still or on the alpha plane (a plain export chain's fades ahead
  of its converter are drawn) or a dip to white, odd size or bars off the 4:2:0 grid, in a **still** a 4:2:2/4:4:4 picture,
  and a layer whose last `scale` resizes it unless the cascade `scaled_convert_refusal` clears; and any layer for caps
  without `matrix_convert`. (`**Mixed matrices** are FFmpeg's conversion`)
- The cascade (swscale `yuv420p -> bgr24` at the smaller size and back, `GpuCaps::matrix_convert_scaled`) is drawn for a 4:2:0
  picture that **shrinks** with the whole result kept, even sides and scaled luma rows (`ScaleStage::cascade_refusal`); refused
  for a Cover crop, a picture that does not shrink, a delivery naming `bilinear`/`lanczos`/`neighbor`/`spline` (its flags are
  in that `scale`; the cascade is reproduced with the default scaler's tables), and caps without it. (`swscale cascades `)
- A blurred fill behind such a layer (FFmpeg 9 ties the fill's two scales to the composite's colour space) is drawn where
  `GpuCaps::matrix_convert_fill` says measured (`fill_cascade_refusal`, `BlurFill::enlarge_refusal`: an even frame the
  thumbnail is smaller than on both axes). A fit that converts without resizing converts unscaled and lays on the same fill;
  an odd picture side is refused there. (`A blurred fill behind such a layer`)
- In a Motion plan the composite's matrix is the cut's: `PlanCanvas::cut_matrix` is the tag of the first clip in graph order
  (a track's clips by start, tracks in list order) that ends on the alpha plane (`yuva420p`) with nothing forcing a converter,
  else the first clip's (FFmpeg 9 negotiation, `avfiltergraph.c`); it holds for every frame, so a lone other-matrix clip is
  converted wherever it is. (`In a Motion plan the composite's matrix is the cut's`)
- `MixedMatrices` in every frame: a cut whose deciding clip is RGB, of no known matrix, or not read (a clip on the plane that
  holds an `eq`, grade, effect, tone map, reprojection, stabilisation or placed black fade, or a blend mode anywhere in the
  cut: `unread_decider_in_cut`); a mixed cut for caps without `matrix_convert` (the deciding clip's frames refused too); a
  mixed cut with a grade in the stack. An RGB picture alone with a BT.709/BT.2020 clip is `RgbInStack` (`cut_non_601`).
  `PlanCanvas.matrix` is the result for the frame's stack; `PlanStream::matrix` is the one a translucent layer is taken
  out of YUV with. (`A cut whose deciding clip is RGB`)

## The graph's BT.709 label

- Mirrored (`PlanStream::label_bt709`, `engine-cli.md`): an untagged HD YCbCr layer sharing the export graph with an input
  naming a matrix is BT.709 to the plan exactly when the graph labels it, under every policy. Motion: from the cut's table
  (`graph_bt709_labels` over every video clip of the rendered timeline: an off-screen BT.709 clip still labels the HD clip);
  Still: from the layers on screen (`model::bt709_labels` in `Planner::plan`). (`**The graph's BT.709 label is mirrored**`)
- Both decide from the **original's** video stream (`AssetFacts::original`), never the proxy's (a 640x360 proxy of a 1080p clip
  is still labelled). `Unknown` still refuses any stack not BT.601. `reserved` reads as no tag (`PlanStream::matrix`,
  `known_color`). A PNG's `gbr` and the `bt470bg` of JPEG/WebP and the `mjpeg`, `amv`, `speedhq`, `jpegls` (and decode-only
  relatives) decoders are decoders' defaults and name no matrix; a full-range H.264 clip's stated `bt709` does label.
  (`never the proxy's the layer describes`)
- Accepted difference: the plan decides over the cut (Motion) or layers on screen (Still), while the stream a handover starts
  decides over its own slice; where the slice drops the matrix-naming clip, a translucent HD layer, coloured text or a dip to
  white in the stream is read as BT.601 where the plan drew BT.709. Opaque pictures are identical. (`**An accepted difference**`)

## What depends on the render size (`layer_geometry.rs`)

- Decided in the plan before any decode. `LayerGeometry` lives in kerf-core for this reason; `layer-geometry.ts` (Preview
  transform handles) replays `layer_geometry/corpus.rs`'s `fixtures/layer-geometry-corpus.json`; regenerate with
  `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- layer_geometry_corpus` after an intended rounding
  change. (`**What depends on the render size**`)
- Resizing a picture that is not planar 8-bit 4:2:0 (`yuv420p`/`yuvj420p`), any scale stage that changes its size either
  way, 10-bit 4:2:0 and gray included, is `FormatResize`, because FFmpeg scales in the picture's format while the decode
  reduces to 8-bit 4:2:0 first and chroma is interpolated from different samples; no ratio band is claimed safe. Not a
  layer whose fit picture FFmpeg makes (`fits_by_ffmpeg`, `yuvj420p`/`rgb24` only; scaler-approximate and odd-pad refusals
  still apply). (`**resizing a picture that is not planar 8-bit`)
- Exception, a plain `yuv444p` clip with `GpuCaps::resize_444` (`resize_444_refusal`; `takes_444(layer, caps, size)`;
  `for_decode(caps, size)` stamps `PlanLayer::wide_chroma`): lifted only for **one stage that is the fit `scale`** (a second
  scale, the zoom, resizes the 4:4:4 picture in the first), a window, result, Cover crop and picture of **even** sides
  (`ScaleStage::chroma_444_refusal`), chroma ratio within the table limit (`max_shrink_for` against `scaled / 2`), no colour
  correction, grade, effect, mask, rotation, opacity, blend, fade, dissolve, slide, keyed number (zoom and position are fine),
  blurred fill, HDR/reframed/360/full-range/still picture, no scaler named but `bicubic`, and where colour is negotiated the
  composite's own matrix; 4:2:2, 4:4:0, 10-bit 4:4:4, RGB stay `FormatResize`. (`except for a plain`)
- A window the fit does not resize is a resize for this purpose (`RenderPlan::format_window`, `FormatResize` with
  `from == to`): `crop` cuts in the picture's format and the last `scale` converts the *window* whose edge clamps taps, where
  the decode converted the whole picture. Only 8-bit `yuv420p`/`yuvj420p`, `gray` and `nv12` are exact; a plain `yuv444p`
  window is drawn from its 4:4:4 planes; every other format is refused; the render mirrors it (`format_window_refusal` in
  `composite_texts`). Windows of never-recorded `pix_fmt` and of `nv21` are refused now. (`**A window the fit does not resize is a resize for this purpose**`)
- Geometry's chroma-grid check (`layer_geometry_resolved`) judges only where a window *lands* and the format gate only a
  stage that *resizes*; the converter's input is not the whole picture when the first stage reads a window or a stage before
  the last is not the whole picture at its size (a Cover crop followed by a second scale is already `FoldedFit`, now refused
  for the format too). (`judges only where the window *lands*`)
- A picture left whole at its size is drawn for every format, except a `pad` of an odd-height picture `scale` did not
  resample, placed at the frame's origin (`Geometry`, one row short): `vf_pad.c` pads in place when the frame is writable
  with room (the odd last row goes), else copies; the export hands the decoder's frame, so the outcome depends on decoder
  and frame and the plan cannot say; a still always drops the row; it is refused at the origin on every build.
  (`of a picture with an odd height that`)
- Off the origin it is the graph's (`RenderPlan::odd_pad`): a Still plan's `scale` converts plain `yuv420p` into the
  `yuva420p` inside the buffer `pad` hands it, so the odd row and the chroma row under it go at any gap; refused when the
  chain carries a colour correction, grade, effect, fade, mask, keyed number or another format (they decide differently;
  none drawn yet). A Motion plan's trailing `format=yuv420p` makes it a copy for `yuv420p` (drawn) and a conversion for
  another format, a chain on the alpha plane (dissolve, mask, key) or a blend mode (refused). A native 4:2:2/4:4:4/RGB format
  at the frame's width is converted by `scale` into that buffer in every chain: drawn, exact. (`**Off the origin it is the graph's**`)
- A `yuv420p` picture flagged full range, letterboxed, in an export whose FFmpeg does not negotiate colour (policy other than
  `BottomLayerTag`) is refused: `pad` draws the bars in limited black and the graph converts the range afterwards. (`they read Y 30`)
- A crop of a picture whose chroma is finer than 4:2:0's: the first `crop` runs on the picture as decoded and rounds to its
  **native** chroma grid (`pix_fmt_subsampling`: even for 4:2:0, even columns for 4:2:2, nothing for 4:4:4/gray/RGB; a table
  by pixel format, every entry measured by an `#[ignore]`d test against the real `crop`); the chain converts to 4:2:0 in its
  **last** `scale`, so `pad` and the Cover crop of a chain with one `scale` are on the 4:2:0 grid whatever the source, while
  a Cover crop followed by the transform's own `scale` is on the native grid. The compositor's planes are 4:2:0, so a 4:2:2 /
  4:4:4 / RGB picture positioned between two 4:2:0 samples is a pixel off: refused; gray is exact, as is any position on both
  grids. An unknown grid (never recorded or off the table) is refused wherever grids disagree
  (`LayerGeometry::resolve_any_grid`). (`Where a position rounds`)

## Range

- Colour correction on a `yuvj` picture or a `yuv420p` flagged `pc` is refused under any policy (the decode converts to
  limited up front; FFmpeg 9 keeps full range through the graph and runs `eq` and the overlay on it, converting last; FFmpeg
  6 agreeing is accidental decode-order luck), and so is a graded layer in a stack that holds one wherever the composite's
  colour is negotiated or unknown (range, like the matrix, is the bottom layer's). An asset that never recorded its pixel
  format is not known not to be full range and is treated so. (`is refused** (under any policy`)
- What only the decode or the compositor can see (a picture that decodes at another size than probed; an alpha channel found
  in the pixels of an asset that never recorded its format) is refused there (`GpuError::Unsupported`). (`What only the decode or the compositor can see`)

## plan_epoch.rs — has anything a plan is made from changed?

- `plan_epoch()` is a process-wide counter that moves for every row a `Project` connection inserts, updates or deletes one by
  one (`update_hook`, installed in `Project::init`); `analysis`, `history`, `tasks` are skipped (written constantly by
  background work, no plan reads them). (`has anything a plan is made from changed?`)
- The hook does not see a `DELETE` without a `WHERE` (SQLite empties the table): `clear_staged` says `WHERE 1`, and any new
  statement that clears a table must too. Nor the row an `INSERT OR REPLACE` replaces, only the insert. (`does not
  see a `DELETE` without a `WHERE``)
- The counter also moves when a project is opened/created and **after** one replaces another (`Project::replace_with`, which
  every place swapping the shared project uses, under its lock: a frame that took inputs from the old project after the new
  one was opened would stay fresh), when a preview proxy lands (`finalize_proxy`, after the rename) or is deleted
  (`remove_proxy_files`), and when the proxy size or preview source changes. (`and **after** one replaces another`)
- A holder of a frame's inputs (the GPU preview's snapshot, `app.md`) reads the counter under the project lock, before it
  reads the project, and keeps the inputs while it is unchanged: a bump that changed nothing costs one rebuild, a change
  that does not bump is what it must not allow ("everything but what is known to be irrelevant"). It cannot see a proxy file
  deleted by hand, which is why the snapshot is also dropped after 2 s. (`reads it **under the project lock`)
- `Project::preview_assets_of(assets)` resolves proxy-swapped assets from assets already read (touches disk, so off the
  lock); `Project::proxy_waits_in(timeline, source, from, to, input)` is `proxy_waits` over a timeline and proxy inputs in
  hand, so Proxy only can refuse a frame without the lock (a test holds the two equal). (`preview_assets_of`)

## Entry points named by the full file

- Plan vs still: a `cli.rs` test pins the still argv against the plan; `planner.rs` holds the whole plan against
  `active_video_clips` over a busy cut; `the_still_plan_holds_the_layers_the_still_draws_at_every_time_of_every_transition`
  and kerf-gpu's `frames_inside_a_transition_are_drawn_as_ffmpegs_still_draws_them` hold transitions; `tests/motion_parity.rs`
  holds the export's own frames to a Motion plan; `rendered.rs` holds candidates/`fps_pick` to the pixels at five rates.
  (`pins the argv against the plan`)
- Refusals: `tests/coverage.rs` (variant ordinals), `tests/checker_e_turn.rs` (rotation), `tests/still_route_size.rs`,
  `parity.rs` / `motion_parity.rs` (chroma key after a scale), `keyed_zoom.rs` (zoom read at the output frame's time),
  `a_stabilized_clip_is_refused_in_a_still_and_an_export_frame`. (`tests/checker_e_turn.rs`)
- Planning cost: `KERF_BENCH=1 cargo test -p kerf-core --no-default-features --release -- --ignored --nocapture
  bench_planning` (print-only). (`bench_planning`)

## Open ends the full file states

- Folded fits (`FoldedFit`) are not drawn: no adapter where it was written; needs a one-stage `LayerGeometry`, port, corpus and
  parity cases. (`**not done: no adapter where it was written**`)
- `KeyedCrop` is refused in both modes (candidate to lift); the moving-still blend gate is `takes_still_route` so a crop-only
  moving still stays covered if it is lifted. (`stays covered if `)
- A keyed mask in Motion is refused with no cap (`KeyedMask`); a still with an odd-pad off the origin and colour/effect/fade
  chain is refused ("none of these is drawn yet"). (`none of these is drawn yet`)
- Fade-on-alpha-plane black (0 vs 16) is not known to `FadeStep` / `LayerFx::strength`: must be before A5.1 draws one.
  (`must before A5.1`)
- The full-range / negotiated-colour policy condition is a proxy measured only on FFmpeg 6.1.1 and 9.0.2 (5.x, 7.x, 8.x,
  distro builds unmeasured). (`The policy condition is a **proxy measured only on FFmpeg 6.1.1 and 9.0.2**`)
- An exact route for full-range resized pictures needs the raw full-range planes from the decode and swscale's
  `lumConvertRange`/`chrConvertRange` on the 15-bit lines between the compositor scaler's passes. (`what an exact route needs`)
- Accepted handover difference for the BT.709 label (see above); dip to white, translucent round trip and grade conversions are
  held only where x86 kernels were probed (arm64 is FFmpeg's). (`the arm64 kernel makes it FFmpeg's`)
