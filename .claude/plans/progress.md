# GPU compositor + editor roadmap — progress log

Plan: `.claude/plans/gpu-compositor-and-roadmap.md`. One row per work package.

| WP | branch | PR | status | notes |
|---|---|---|---|---|
| A0 GPU feasibility spike | `feat/gpu-a0` | — | merged (local) | **Gate: PASS** on lavapipe (FFmpeg 6.1.1 and 9.0.2): 77 renders after review fixes (letterbox matte, opacity RGB round trip emulated, swscale scaler port, transposed decodes/alpha refused, wgpu error scopes); flat max ≤ 8/255, PSNR ≥ 40 dB, busy-source cases ≥ 45.8 dB. Composite in YUV like `overlay`, swscale-bicubic scaler, vf_eq tables, BT.601 output (what the FFmpeg still does). Bench (lavapipe, 1080p/1/3/6 layers): ffmpeg 119/242/414 ms vs gpu 132/249/509 ms — decode-bound; real GPU unmeasured. +5 MB binary (Linux). |
| B1 Workspaces + library rail | `feat/workspaces` | — | merged (local) | Two review rounds; awaits push. |
| A1 Frame source + render plan | `feat/gpu-a1a-oracle`, `-timing`, `-planner`, `-picks` (A1a-0..3 merged locally; A1b next) … | — | in-progress | Design `.claude/plans/a1-design.md` (critiqued, revised). Seven slices: A1a-0 golden argv oracle, A1a-1 `{:.6}` + `clip_timing.rs`, A1a-2 Planner, A1a-3 picks + SourceMedia + span, A1b-1..3 FrameSource. |
| A2 Native preview surface | — | — | todo | |
| A3 Scrub + live drags on GPU | — | — | todo | |
| B2 Waveforms + clip overlays + frame snapping | `feat/waveforms` | — | merged (local) | Waveform pyramid (48 kHz, 4 levels, cached) + `get_waveform_range`; tile-cached canvases, volume/fade overlays, frame quantization. |
| B3a Ripple + multi-select + zoom | `feat/timeline-editing` | — | merged (local) | `Timeline::ripple_from` (per-track, no sync lock), `move_clips`/`remove_clips`, marquee, group moves, zoom 0.05–2000 px/s. |
| B3b Filmstrips + track heights + minimap | `feat/filmstrips` | — | merged (local) | Per-asset filmstrip (proxy preferred, keyframe sampling for long originals, capped + niced even at 100%), `get_filmstrip` (no MCP tool: `skim_asset` covers agents), height presets (UI-only), minimap. |
| B6 On-canvas transform handles | — | — | todo | |
| A4 Playback | — | — | todo | |
| B4 Mixer | `feat/mixer` | — | in-progress | Engine + surface done: `Timeline.master {volume, limiter, ceiling_db}` before `loudnorm` (omitted at neutral), `set_master_volume` / `set_master_limiter`, `get_levels` (one metered ffmpeg pass: per-track + master LUFS / sample + true peak / short-term max), golden family appended as cases 4000..4799. Open: Mixer panel UI, Web Audio master + meters, preview ducking label. |
| B5 Keyframes v2 | — | — | todo | |
| A5 Effect parity | — | — | todo | |
| B7 Colour grade + scopes | — | — | todo | |
| A6 Headless agent rendering | — | — | todo | |
| B8 Motion | — | — | todo | |
| A7 Export through the compositor | — | — | todo | |
| fix: keyframed zoom + graph bugs | `fix/keyed-zoom` | — | merged (local) | Moving zoom runs last at the output frame (export, preview stream and still alike); keyed rotation fills transparent; tiny-scale clamp; even HDR fit sizes; alpha sources keep their cut-out. Deliberate golden re-blesses, each proven equal to its family. |
| B9 Backlog | — | — | in-progress | Done: hardening (`feat/hardening-2`: hidden-until-themed window + failsafe, synchronous log writer, colour-literal + WCAG guards with Kerf Light fixes and a stored-theme upgrade, first-launch `.kerf`). Done: SRT/ASS caption import (`feat/caption-import`: tolerant parsers run outside the project lock, cut- or source-timed placement through the transcript caption path, offset, keep-lines, caps). Done: customizable keybindings (`feat/keybindings`: action registry, strict modifiers, override-only storage, Settings › Keyboard). Done: edit modes (`feat/edit-modes`: roll/slip/slide tools with a trim monitor, group split-and-remove on Q/W, cut welding). Open: linked A/V + detach audio (verify `extract_audio` doubling), blurred background,  marker notes, split-and-remove, preview limiter, virtualisation, graph editor, lock checks in single-clip core ops, CSP in packaged builds, **export A/V offset for late-start sources** (a head clip's video is rebased to the clip start, `lead` early against its audio). |
| fix: proxy late video start | `fix/proxy-late-video-start` | — | merged (local) | Padded proxies (`<hash>.lead.mp4`: one clone of frame 0 at t=0, timestamps kept, software encode); head clips drop the clone; TS left as a documented limit. |

## Decisions

- **2026-10-06 — branches.** The session was started on a single designated
  branch; the plan asks for one branch per work package. Followed the plan:
  `feat/…` branches, one PR each, merged with merge commits.
- **2026-10-06 — no attribution lines.** Commits and PR bodies carry no session
  links or trailers (plan §1.3 and the repo's `commit-msg` hook).
- **2026-10-06 — A0 ∥ B1.** Started B1 alongside A0 as the plan allows (§5:
  "Run B1 in parallel with A0–A2"); they share no files beyond `CLAUDE.md` and
  this log.
- **2026-10-06 — `gpu` agent.** Defined in `.claude/agents/gpu.md` (A0). A
  session only loads agent types at start, so until a fresh session the GPU
  work runs on the `engine` agent type with the `gpu` agent's instructions.
- **2026-10-06 — GitHub writes blocked.** Every push and GitHub API write
  returns 403 (the GitHub App is not installed / linked for this repository).
  Work continues on local branches; a local integration branch `local/main`
  stands in for `main` (each finished WP merged into it with `--no-ff`), so
  later WPs build on earlier ones. When access returns, the branches are pushed
  and PRs opened in merge order.
- **2026-10-06 — main did not type-check.** The hand-off merge (#89) left
  duplicated imports/script blocks (21 svelte-check errors, dev server 500).
  Fixed on its own branch `fix/hand-off-duplicates` (to merge first); B1 merges
  it.

- **2026-10-06 — A0 edge band.** The agent lowered the edge-detection step
  from 24 to 12 levels (a 30 %-opacity layer's edges fell under 24 and FFmpeg's
  RGB round trip blurs chroma around them) and added a whole-image PSNR floor
  (40 dB, 30 dB for rotated cases). Accepted: the flat-region limits (40 dB,
  8 levels) did not move, and the whole-image floor catches what a wider band
  would excuse.
- **2026-10-06 — A0 colour policy.** FFmpeg's still converts the composite to
  RGB as BT.601 whatever the source tags; the GPU matches it
  (`PlanCanvas.matrix`). Exports are untagged yuv420p shown as BT.709 by
  players, so the preview and the file already differ slightly on saturated
  colour. Revisit when the GPU replaces the FFmpeg preview (A3/A7).
  **Superseded 2026-10-07:** that holds for FFmpeg 6.1 only. FFmpeg 9.0.2 (the
  pinned build the bundles ship) negotiates colourspace across the overlay
  chain, so the bottom layer's tag decides the composite matrix. The engine
  now *measures* which policy the ffmpeg in use follows
  (`composite_color_policy()`, a once-per-process probe through the real still
  graph) and the plan takes it as input; single-matrix stacks are mirrored,
  mixed-matrix stacks under negotiation are refused. The parity job runs both
  the distro and the pinned FFmpeg.
- **2026-10-07 — A0 refusals.** Enlarging a non-4:2:0 layer, unknown pixel
  formats (positive allow-list), translucent layers of assets saved before
  `pix_fmt` was recorded, and shrinks steeper than 40:1 are refused by the
  plan (FFmpeg fallback). Kept the 40:1 cap the agent added: nothing measures
  beyond it.
- **2026-10-06 — vf_eq arithmetic.** `kerf-gpu/src/eq.rs` reimplements the
  maths of FFmpeg's `eq` filter from its documented behaviour and source
  reading, without copying code; no third-party notice added (an algorithm is
  not covered by the LGPL).

- **2026-10-07 — B3 split.** B3 landed as B3a (ripple, multi-select, group
  moves, zoom) and B3b (filmstrips, heights, minimap): both rewrite
  `Timeline.svelte`, so they ran in sequence.
- **2026-10-07 — ripple scope.** Ripple is per track with no sync lock (until
  B9's linked A/V); titles/markers don't move. The new multi-clip ops refuse
  locked tracks while the older single-clip core ops still don't check locks
  (only the GUI does) — a follow-up for B9.
- **2026-10-07 — filmstrip is a side job.** Ungated (the timeline must not
  wait behind a render) but thread-capped and niced even at a 100 % budget,
  and keyframe-sampled on long originals.
- **2026-10-07 — merge conflicts resolved on the WP branch.** Each WP merges
  `local/main` into itself before landing (merge commits, no rebase), so the
  eventual PRs merge cleanly in order.

- **2026-10-07 — A1 frame identity is pts.** Frames are keyed by (file
  identity, pts in stream ticks), not an index, and Motion picks follow the
  `fps` filter's rule (verified on rendered frame numbers incl. reverse/speed).
  The FFmpeg still's `-ss` moves from `{:.3}` to `{:.6}` (it skipped a frame on
  fine time bases) — a deliberate still-argv change, re-blessed in the oracle.
- **2026-10-07 — B9 hardening scope.** Logging is synchronous (one write per
  event; no hot-path logging), missing `.kerf` launch paths are reported and
  never created, and stored Kerf Light themes that exactly match an old preset
  upgrade on load.

- **2026-10-07 — keybindings are strict.** A chord means exactly its
  modifiers (⇧J no longer shuttles, Ctrl chords don't fire on macOS) so
  conflicts are unambiguous; one-press actions ignore auto-repeat.

- **2026-10-07 — per-worktree build dirs.** A shared `CARGO_TARGET_DIR`
  across worktrees let cargo reuse another branch's test binary (workspace
  crate hashes ignore the worktree path), so some runs were vacuous. Every
  worktree now builds into its own `/home/user/.ct/<name>`, deleted when the
  worktree goes; the integration branch was re-verified clean that way.
- **2026-10-07 — golden argv oracle.** 4000 seeded cases digest the export,
  still and preview builders (A1a-0); libm-derived numbers are rounded to 10
  significant digits before hashing (FMA vs generic `pow` differ in the last
  ulp), digest files are pinned LF.

- **2026-10-07 — what the export draws at clip edges.** `ClipTiming::enabled`
  is only the overlay's enable window; FFmpeg evaluates it at
  `k*(1/fps)` (an ulp below `k/fps` for a third of 24 fps frames), so frame-
  aligned clip starts can lose their first frame and fades count whole frames.
  Both are pinned by rendered tests; the GPU pick (A1a-3) must reproduce them.
- **2026-10-07 — wall-clock tests.** Performance guards assert generous limits
  (8–10 s, 5 s in bun) sized to catch the quadratic algorithm they guard, not a
  busy machine running the suite in parallel.

- **2026-10-07 — keyframed zoom is a graph bug.** The export (and the preview
  stream) never animated a scale-only keyframed zoom: a format converter in
  front of `overlay` pins the first frame's size. Fixed in the graph
  (`fix/keyed-zoom`, a deliberate argv change) rather than reproduced on the
  GPU.
- **2026-10-07 — edit modes act per track.** Roll/slip/slide don't move a
  linked partner (no sync lock until linked A/V lands).

- **2026-10-07 — Still refusals are exact.** A Still frame is refused only
  while a fade step is live, a layer travels, or an outgoing clip's tail window
  is open (the export can draw the tail around a non-covering incoming clip);
  the `fades`/`transitions` caps apply to Motion plans only. Parity-tested
  around dissolves, slides, pushes and dips.

- **2026-10-07 — D8: per-track buses later, and only for tracks that need one.**
  The fader and pan ride each clip after its own chain (a linear op, so the same
  signal as a bus fader); a *bus* only buys something for a **nonlinear or
  per-track insert** (track EQ / compressor / automation acting on the summed
  track). Moving every export to submixes would re-shape the audio graph of every
  project: all of the golden digests with sound re-blessed, the byte-identical
  guarantee gone, for no change in what anyone hears. Per-clip effects already
  exist and B5's per-property channels give clip-level volume automation, which
  covers the use cases that exist today. So: **no buses now.** When track inserts
  arrive, build the bus topology *only for tracks that carry one* (neutral
  omitted, every other track stays flat and byte-identical). The cost of that is
  already small and proven: the metered levels build (`build_filter_complex_metered`)
  sums each track into a submix before the final sum, and equals the flat graph in
  what it measures (an ignored test renders the export and reads it back).
- **2026-10-07 — master bus placement and limiter.** After the final sum / duck
  bus and before `loudnorm`. The limiter is `alimiter` with `level=0` (its
  default auto-level scales the output back up to full scale, turning a ceiling
  into makeup gain) and `latency=1` (otherwise the lookahead delays the mix by
  its attack and drops its tail, out of step with the picture); both verified by
  ignored tests that fail without them, on FFmpeg 6.1.1 and 9.0.2. Ceiling is a
  *sample*-peak ceiling (no oversampling); `get_levels` reports the true peak.
- **2026-10-07 — levels are one pass.** The alternative to taps is one render per
  track. A metered build of the export's own audio graph taps each track's strip
  and the finished mix, each with sample + true peak (measured ~0.01x real time
  per meter; true peak is about half of that, sample-only would have saved
  half the cost on a long cut but left a track's true peak unanswered), so the
  master reading is the file's (tested against a rendered file). A track reads *before* the duck bus and the master. Short-term
  max comes from the frame log (None under 3 s). Gated by `cpu::lease`, a
  120 s stall watchdog and the MCP cancel token.
- **2026-10-07 — golden family appended, not interleaved.** The 800 master-bus
  cases are cases 4000..4799, so the first 4000 per-case digests are identical
  before and after (`KERF_GOLDEN_CASES` diff, checked on the whole file) and the
  three digest files only gained block lines (`git diff`: 24 insertions, 0
  deletions). Interleaving them would have moved every block.
- **2026-10-07 — toolchain noise.** rustc/clippy 1.99.0 flags four
  `redundant_clone` sites in code this WP does not touch (`planner.rs:997`,
  `keyed_zoom.rs:1385`, two in `cli.rs` tests); present on the base commit too, so
  left alone here. FFmpeg 9.0.2 also mis-probes some float-PCM `.wav` fixtures as
  MPEG-TS (the byte pattern of a pure tone), so the level tests use FLAC.
- **2026-10-07 — the frame pick is FFmpeg's arithmetic.** `fps_pick`
  replays `setpts` tick truncation, the "stream ends where the next frame
  would land" EOF rule (using the last frame's own duration), reverse
  re-stamping and still-image loops; verified on rendered barcode clips over
  mp4/mkv/ts/avi/nut time bases, VFR, speeds and reverse on both FFmpegs.
  Proxies carry a `StreamInfo` sidecar so the interactive path never
  ffprobes; `Handover` pins the canvas for A4's stream restarts.

## Needs a real machine

- B1–B3: every UI change was verified in the browser harness only; the Tauri desktop window (WebKitGTK / WebView2 / WKWebView canvas, events like `ripple-mode-changed`, real `get_waveform_range` / `get_filmstrip` against footage) needs a desktop run.
- B9 hardening: first visible frame / no flash per OS, the 3 s failsafe, focus, second launch mid-boot, a mistyped `.kerf`, the CSP in packaged Windows/macOS builds.
- B4: `get_levels` on a real long multi-track cut (here: synthetic tones, ~100x real time per true-peak meter) and the Mixer panel's meters against real playback.
- A0: kerf-gpu on a real GPU (Vulkan/Metal/DX12) and WARP; macOS has no software adapter (`KERF_GPU_ADAPTER=hardware`). Real-GPU still timings.
