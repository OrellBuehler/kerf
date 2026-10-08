# GPU compositor + editor roadmap — progress log

Plan: `.claude/plans/gpu-compositor-and-roadmap.md`. One row per work package.

| WP | branch | PR | status | notes |
|---|---|---|---|---|
| A0 GPU feasibility spike | `feat/gpu-a0` | — | merged (local) | **Gate: PASS** on lavapipe (FFmpeg 6.1.1 and 9.0.2): 77 renders after review fixes (letterbox matte, opacity RGB round trip emulated, swscale scaler port, transposed decodes/alpha refused, wgpu error scopes); flat max ≤ 8/255, PSNR ≥ 40 dB, busy-source cases ≥ 45.8 dB. Composite in YUV like `overlay`, swscale-bicubic scaler, vf_eq tables, BT.601 output (what the FFmpeg still does). Bench (lavapipe, 1080p/1/3/6 layers): ffmpeg 119/242/414 ms vs gpu 132/249/509 ms — decode-bound; real GPU unmeasured. +5 MB binary (Linux). |
| B1 Workspaces + library rail | `feat/workspaces` | — | merged (local) | Two review rounds; awaits push. |
| A1 Frame source + render plan | `feat/gpu-a1a-oracle`, `-timing`, `-planner`, `-picks`, `feat/gpu-a1b-pieces`, `feat/gpu-a1b-source` | A1a: OrellBuehler/kerf#105, OrellBuehler/kerf#107 (merged); A1b-1: OrellBuehler/kerf#111 | in-progress | Design `.claude/plans/a1-design.md` (critiqued, revised). Seven slices: A1a-0 golden argv oracle, A1a-1 `{:.6}` + `clip_timing.rs`, A1a-2 Planner, A1a-3 picks + SourceMedia + span, A1b-1..3 FrameSource. A1b-2 (`FrameSource`: runs, self-test, reaper, `render_plan_with`, parity through it on both FFmpegs, fake-ffmpeg and stress tests, bench) done locally, stacked on A1b-1; A1b-3 (cursor) next. |
| A2 Native preview surface | — | — | todo | |
| A3 Scrub + live drags on GPU | — | — | todo | |
| B2 Waveforms + clip overlays + frame snapping | `feat/waveforms` | — | merged (local) | Waveform pyramid (48 kHz, 4 levels, cached) + `get_waveform_range`; tile-cached canvases, volume/fade overlays, frame quantization. |
| B3a Ripple + multi-select + zoom | `feat/timeline-editing` | — | merged (local) | `Timeline::ripple_from` (per-track, no sync lock), `move_clips`/`remove_clips`, marquee, group moves, zoom 0.05–2000 px/s. |
| B3b Filmstrips + track heights + minimap | `feat/filmstrips` | — | merged (local) | Per-asset filmstrip (proxy preferred, keyframe sampling for long originals, capped + niced even at 100%), `get_filmstrip` (no MCP tool: `skim_asset` covers agents), height presets (UI-only), minimap. |
| B6 On-canvas transform handles | — | — | todo | |
| A4 Playback | — | — | todo | |
| B4 Mixer | `feat/mixer` | — | done (awaiting PR) | Engine + surface done: `Timeline.master {volume, limiter, ceiling_db}` before `loudnorm` (omitted at neutral), `set_master_volume` / `set_master_limiter`, `get_levels` (one metered ffmpeg pass: per-track + master LUFS / sample + true peak / short-term max), golden family appended as cases 4000..4799. Mixer panel: one strip per audible track + master (shared dB taper with the header slider, one edit per gesture, keyboard nudges), measured Web Audio meters through per-track buses and a master limiter approximation, Measure → `get_levels` (range-aware), harness sample audio. |
| B5 Keyframes v2 | — | — | todo | |
| A5 Effect parity | — | — | todo | |
| B7 Colour grade + scopes | — | — | todo | |
| A6 Headless agent rendering | — | — | todo | |
| B8 Motion | — | — | todo | |
| A7 Export through the compositor | — | — | todo | |
| fix: keyframed zoom + graph bugs | `fix/keyed-zoom` | — | merged (local) | Moving zoom runs last at the output frame (export, preview stream and still alike); keyed rotation fills transparent; tiny-scale clamp; even HDR fit sizes; alpha sources keep their cut-out. Deliberate golden re-blesses, each proven equal to its family. |
| B9 Backlog | — | — | in-progress | Done: hardening (`feat/hardening-2`: hidden-until-themed window + failsafe, synchronous log writer, colour-literal + WCAG guards with Kerf Light fixes and a stored-theme upgrade, first-launch `.kerf`). Done: SRT/ASS caption import (`feat/caption-import`: tolerant parsers run outside the project lock, cut- or source-timed placement through the transcript caption path, offset, keep-lines, caps). Done: customizable keybindings (`feat/keybindings`: action registry, strict modifiers, override-only storage, Settings › Keyboard). Done: edit modes (`feat/edit-modes`: roll/slip/slide tools with a trim monitor, group split-and-remove on Q/W, cut welding). Done: linked A/V + detach audio (`feat/linked-av`: `link_id` / `source_audio`, group edits, range-based sync lock for J/L-cuts, orphan dissolve, fader-folding detach (a fresh lane for dynamics), pictures never cut to make room and trimmed sound reported, `extract_audio` doubling verified +6.02 dB and fixed, Rust-to-TS differential corpus). Open: blurred background,  marker notes, split-and-remove, preview limiter, virtualisation, graph editor, lock checks in single-clip core ops, CSP in packaged builds, **export A/V offset for late-start sources** (a head clip's video is rebased to the clip start, `lead` early against its audio). |
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
- **2026-10-07 — push access restored; stacked PRs.** The finished branches were
  pushed in merge order. At most three PRs are open at once, each based on the
  branch before it, so every diff shows one work package and CI runs on the
  combined tree. When the lowest one merges, the next is retargeted to `main` and
  `main` is merged up the stack. `local/main` is retired.
- **2026-10-07 — FFmpeg repin.** BtbN pruned `autobuild-2026-09-22-13-18`, so every
  pinned engine job 404'd, on `main` too. The pin moved to `autobuild-2026-10-07-13-07`
  (same 9.0 branch, n9.0.2-22), on the first PR of the stack. `--repin` needs the
  GitHub API, which the sandbox blocks for BtbN, so the tag was read via `git
  ls-remote`, the build name via `git describe` of FFmpeg's `release/9.0`, and the
  digests from the downloads. Parity and the pick tests pass on it.
- **2026-10-07 — subagent limit.** The weekly subagent quota ran out mid-session
  (it resets 2026-10-12). The three stopped packages (linked A/V, mixer, A1b-1 fixes)
  were finished directly, with the same verification and no second independent
  review. They get one before merge, once agents are available.
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
  **Superseded 2026-10-07 (`feat/linked-av`):** with links in force they act on the
  group — roll rolls each partner pair sharing the cut, slip and slide move every
  partner, and the whole edit clamps to the tightest member.
- **2026-10-07 — linked A/V (core + surfaces).** `Clip.link_id` (identity, at most one
  clip per track) and `Clip.source_audio` (omitted at `true`; only `false` reaches the
  graph, as a drop from the audio mix, so the golden oracle did not move). Links are on
  by default and per call `link: false` (`Project::with_links`) is the escape hatch —
  no project-wide switch. A linked edit is a group edit: all or nothing, a locked
  partner refuses it. Ripple scope changed: tracks stay independent **except** that a
  clip the ripple pushed drags its linked partners by the same amount (clips only,
  never the lane). `reorder`, property edits and captions are deliberately not
  link-aware. Imports / `cut_clip` / `add_clip` still do not auto-link an A/V asset's
  sound (follow-up option).
- **2026-10-07 — range-based sync lock (review of the first linked-A/V cut).** The
  per-track ripple plus "partners follow a pushed clip" only handled mirrored pairs; a
  J- or L-cut pair (sound leading or trailing its picture) was refused 10-65% of the
  time for ripple delete / speed / ripple remove / ripple trims / cut range, with an
  error steering to `link: false`, which desyncs. Replaced by `Timeline::conform_links`:
  in step = equal content offsets; after every edit each group is re-aligned to its
  *authority* (the named clip, else its track, else the first member that moved) by
  shifting the others, linked followers win against linked material (trimmed back) and
  refuse only for a locked track, an unlinked clip in the way, or a linked clip they
  would cover entirely. Unlinked clips on a partner's track never move for it (a
  behaviour change: ripple delete used to close the partner's whole lane). The guard
  stays as the net and no longer advises `link: false`. Split / cut re-link **by side**
  (an unsplit partner after the cut belongs to the right half), orphaned groups dissolve
  in the same edit, a muted picture pasted without its sound is unmuted, `reattach` is
  never rippled and refuses to double the sound, `detach` folds the picture track's fader
  into the new clip (pan / duck / mute are the destination's, documented), `extract_audio`
  no longer falls through to appending (that is `add_asset_audio`) and reports skipped
  clips, `detach_audio_clips` is the one-revision batch. The browser harness is replayed
  against a corpus kerf-core writes (`links-corpus.json`).
- **2026-10-07 — linked A/V, second review.** (1) *Both partners named.* Trim to the
  playhead names a picture and its sound; the per-lane ripple then pulled each track by its
  own length and the lock read that as "moved apart" (refused 90/101, "unlink them first").
  "Moved apart" is now judged on the timeline **as the edit left it** (before
  `ripple_lanes`; `left` in `run_edit`); if the named members agree there the first in track
  order is the authority and the other named ones are shifted too. `move_clips` with
  partners at different deltas stays refused. (2) *Cut range:* a partner's leftover is a
  linked clip when making room (`settle_linked`), and what a partner keeps after the cut is
  moved to the cut explicitly (`closing`), so a lone leftover still resumes there. (3)
  *Detach under a compressor or gate* no longer folds the fader (the gain would sit ahead of
  a level-dependent effect): the clip goes to a lane at the picture track's fader, else a
  new track at it (no skip path — a lane can always be made); the doc claim is narrowed to
  linear chains. (4) The corpus fixture is `eol=lf` in `.gitattributes` and the freshness
  test normalizes `\r\n`. (5) `first_sync_break` names the lowest track pair (TS too).
  *Victims* — decided: a picture is **never** cut to make room or pushed before 0 (refused,
  naming the lane, Alt offered); a linked **sound** may be trimmed back or lose its head and
  the revision label says so (`… (trimmed sound on A2)`, live and staged); the floor is
  `MIN_EDIT_CLIP` (0.05 s) — a victim that would fall under it is refused, not stubbed. A
  J-cut lead lost at 0 is trimmed and reported rather than refused: the lead is a sound's,
  the picture is untouched, and refusing would block deleting the first shot of any J-cut
  edit. Cost, measured on the J/L fuzz: 13% of moving edits are blocked (was ~5%) — 2.8% of
  those naming a picture, 23% of those naming a sound under ripple, which pulls the next
  shot's picture onto the one before; before, that silently cut the previous shot. The UI
  preview (`linkedTrimPreview`) runs the same rules, refusals included, and names a trimmed
  sound. Corpus 80 → 93 cases.
- **2026-10-07 — `extract_audio` doubled the sound, measured.** The export mixes every
  clip whose asset has audio, video tracks included, so appending the asset's audio
  with its picture still on V1 was +6.02 dB over the clip alone (real render, ffmpeg
  6.1). `extract_audio(asset)` now detaches the asset's cut picture clips and only
  appends when none is on the timeline; the per-clip form is `detach_audio`. The
  sample project seeded the doubled shape; it now seeds the detached-then-unlinked one.

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
- **2026-10-07 — B4 preview: ducking export-only, limiter approximated.** Web
  Audio has no sidechain compressor without an AudioWorklet, and a worklet would be
  a second DSP implementation to keep in step with `sidechaincompress`. So the
  preview plays a ducked track at its fader, and the Duck toggle's tooltip says so.
  The master limiter previews as a hard-knee `DynamicsCompressorNode` at the ceiling,
  with its makeup gain trimmed out. It is labelled an approximation, and Measure
  reads the real export graph.
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
- **2026-10-07 — A1b-2: parity with the still beats the true frame.** On a long-GOP
  transport stream `-ss t` lands on the next keyframe and the decode does not recover
  (finding 5), in the one-shot decode and the FFmpeg still alike. A run restarted
  earlier would return the frame really at `t` — right by the timestamps, wrong by the
  contract (the GPU frame must be the frame FFmpeg would draw). So a file whose first
  run lands more than two frame intervals late is marked `late_seek` and decoded
  one-shot from then on; speed is lost only on such files. Also: parity asserts equal
  decoded planes, not equal composites — a one-off RGBA mismatch on 9.0.2
  (`pip/odd-361x203-in-722x640 @ 0.5`) never recurred in nine suites and a
  six-thread stress test, and rendering one plan twice never differed.
- **2026-10-08 — B4 review fixes (PR #110).** (1) `alimiter` has no `latency` on
  FFmpeg 4.4 (Ubuntu 22.04's system ffmpeg), which refused every limiter-on export
  and `get_levels`; the option is now probed once per process
  (`alimiter_latency_available`, `ffmpeg -h filter=alimiter`) with a `cfg(test)` pin,
  and without it the limiter is emitted without `latency` (the mix then trails the
  picture by the 5 ms attack; not compensated). The golden oracle builds a
  limiter case's export both ways: the first 40 blocks and `still.txt` /
  `preview.txt` did not move, only `export.txt` blocks 40..47. The levels tests pass
  on 4.4.2 and 9.0.2. (2) The limiter is a sample-peak limiter: on 9.0.2 at a
  -1 dBFS ceiling an 11 kHz tone read -0.2 dBTP and 15 kHz +0.1, so "turn on the
  limiter" looped an agent that had already done it. `Levels::new` now takes the
  master bus; with the limiter on and the true peak over -1 dBTP the note says to
  lower the ceiling by the overshoot + 0.5 dB, and the default ceiling is -1.5 dBFS
  (`loudnorm`'s TP). A track over 0 dBFS is no longer said to "clip the sum" (the
  graph is float). (3) A GUI Measure held the process-wide `cpu::lease` with no way
  out; `cancel_levels` (the `cancel_analysis` shape) and a Stop on the Measure
  button, still gated since it is a whole-mix decode. (4) The mixer's strip rule
  anticipates linked A/V's `Clip.source_audio` (PR #109) rather than
  mirroring a `clip_sounds` this base does not have.

- **2026-10-08 — linked A/V, PR review fixes.** (1) *A carried partner was never lane-checked.* `trim`
  (and the beat snap, through `carry_links_since`) wrote a partner's new span with no overlap check, so a
  move by trim or a tail extension put a detached sound on an unlinked voice-over where `move_clip` refuses.
  The check cannot sit inside `carry_extent_edit`: ripple legitimately makes room (a tail extension pushes
  the clip behind the sound), so it runs in `run_edit` **after** the per-lane ripple and the sync lock
  (`Timeline::check_carried_lanes` on the partners recorded in `Project::edit_carried`), refusing with the
  lane named when a carried partner now overlaps a clip outside its group that it did not overlap before
  (the `settle_followers` wording, shared). The named clip's own lane is still unchecked, as a trim always
  was. Under ripple a move by trim is still refused: no length changed, so nothing ripples. TS mirror
  (`checkCarriedLanes`, `runEdit`'s third argument, the trim preview) and five corpus cases; corpus 93 →
  102. (2) *Multi-select Reattach was N revisions and partial on error.* `reattach_audio_clips` (core,
  Tauri, MCP, harness) is one `Reattach audio (N clips)` revision, **all or nothing** (unlike the detach
  batch, which skips and reports: half a reattach is a half-undone selection), a pair named by both its
  clips counted once; `reattachSelection` has one Undo. (3) `set_volume` / `set_fade` / `set_clip_enabled`
  say a detached picture carries no sound. (4) A clippy `nonminimal_bool` in `with_linked_cuts` rewritten
  with its short-circuit kept; a duplicated phrase in `CLAUDE.md` fixed.

## Needs a real machine

- B1–B3: every UI change was verified in the browser harness only; the Tauri desktop window (WebKitGTK / WebView2 / WKWebView canvas, events like `ripple-mode-changed`, real `get_waveform_range` / `get_filmstrip` against footage) needs a desktop run.
- B9 hardening: first visible frame / no flash per OS, the 3 s failsafe, focus, second launch mid-boot, a mistyped `.kerf`, the CSP in packaged Windows/macOS builds.
- B4: `get_levels` on a real long multi-track cut (here: synthetic tones, ~100x real time per true-peak meter) and the Mixer panel's meters against real playback.
- A0: kerf-gpu on a real GPU (Vulkan/Metal/DX12) and WARP; macOS has no software adapter (`KERF_GPU_ADAPTER=hardware`). Real-GPU still timings.
