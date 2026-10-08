# GPU compositor + editor roadmap — progress log

Plan: `.claude/plans/gpu-compositor-and-roadmap.md`. One row per work package.

| WP | branch | PR | status | notes |
|---|---|---|---|---|
| A0 GPU feasibility spike | `feat/gpu-a0` | — | merged (local) | **Gate: PASS** on lavapipe (FFmpeg 6.1.1 and 9.0.2): 77 renders after review fixes (letterbox matte, opacity RGB round trip emulated, swscale scaler port, transposed decodes/alpha refused, wgpu error scopes); flat max ≤ 8/255, PSNR ≥ 40 dB, busy-source cases ≥ 45.8 dB. Composite in YUV like `overlay`, swscale-bicubic scaler, vf_eq tables, BT.601 output (what the FFmpeg still does). Bench (lavapipe, 1080p/1/3/6 layers): ffmpeg 119/242/414 ms vs gpu 132/249/509 ms — decode-bound; real GPU unmeasured. +5 MB binary (Linux). |
| B1 Workspaces + library rail | `feat/workspaces` | — | merged (local) | Two review rounds; awaits push. |
| A1 Frame source + render plan | `feat/gpu-a1a-oracle`, `-timing`, `-planner`, `-picks`, `feat/gpu-a1b-pieces`, `feat/gpu-a1b-source`, `feat/gpu-a1b-cursor` | A1a: OrellBuehler/kerf#105, OrellBuehler/kerf#107; A1b-1: OrellBuehler/kerf#111; A1b-2: OrellBuehler/kerf#112; A1b-3: OrellBuehler/kerf#113 (all merged) | merged | Design `.claude/plans/a1-design.md` (critiqued, revised). Seven slices: A1a-0 golden argv oracle, A1a-1 `{:.6}` + `clip_timing.rs`, A1a-2 Planner, A1a-3 picks + SourceMedia + span, A1b-1..3 FrameSource. A1b-2 (`FrameSource`: runs, self-test, reaper, `render_plan_with`, parity through it on both FFmpegs, fake-ffmpeg and stress tests, bench) done locally, stacked on A1b-1; A1b-3 (`FrameCursor`: exclusive run per clip, forward-only, reversed window capped, 4290 output frames checked against `select` on 9.0.2 here; CI's parity job runs the same ignored suite on the distro FFmpeg too) done locally, stacked on A1b-2. A1b-1 second review (2026-10-08): pts must strictly ascend in a run (`OutOfOrder`), unreadable pts is an error, a failed y4m reader stays failed, a lost thrash ticket ages out. A1 complete. |
| A2 Native preview surface | `feat/gpu-a2-surface` | — | review | **Gate: PASS where testable, JPEG fallback intact everywhere.** Opt-in *Settings › Preview › GPU preview (experimental)* (default off; off = today's behaviour, no extra command). `kerf-gpu` gains `Gpu::new_for_surface`, `Presenter` (+ `Surround`: matte in the frame, backdrop beyond) and `Compositor::render_plan_texture_with` (frame left on the GPU; `RenderedFrame::read_back` / `from_rgba`); `kerf-app` links it (`gpu_preview.rs`: lazy `Backend`, per-frame plan decision with the FFmpeg JPEG returned *in the same call*, `Backoff`, `react` = rebuild on device/surface loss) with commands `get_preview_frame` / `set_preview_bounds` / `gpu_preview_status` (GUI-only, no MCP tool, no capability, no CSP change, `tauri.conf.json` unchanged). Techniques: **Linux/X11 = child window (confirmed under WSLg + lavapipe)**, **Windows = the window's own surface under a transparent webview (unconfirmed, code only)**, **macOS = none in this build**, Wayland = JPEG. Measured on WSLg/lavapipe (debug build): 416x234 frame cached-decode 0 ms + composite 15-20 ms + present 2-3 ms; first frame decode 140-160 ms + composite 47 ms + present 17 ms; 672x378 composite 20 ms + present 4 ms. Frontend: `preview-bounds.ts` (device-pixel/DPR rounding, route policy, covered detection, hole polygon; 23 bun tests), `gpu-preview.svelte.ts`, Preview/Settings/StatusBar wiring, harness `?gpusurface=1`. Rust: 30 `gpu_preview` unit tests (technique table, bounds math, fit/placement, exact render width, backoff + its forgiveness, fallback decision, no-adapter child process, a hide landing mid-present, stale reports, a stalled / panicking build, a panic in the GPU path, the crash marker), `gpu::tests::a_window_no_backend_can_draw_to…`, 4 presenter tests, parity's `rendering_through_the_frame_source…` extended to the texture path, an ignored X11 end-to-end test (`x11.rs`: present to a real child window, read the pixels back from the X server, destroy the device, rebuild) and two more beside it (one process-wide display; an abstract-socket-only `Xvfb` reached at once), and `frame-pump.test.ts` (the frame effect's reactivity, run on Svelte's own runtime). The independent review of the branch found nine issues, all fixed (decisions below). Remaining: A3 (scrub / live drags), A4 (playback), real-machine confirmation (below). |
| A3 Scrub + live drags on GPU | — | — | todo | |
| B2 Waveforms + clip overlays + frame snapping | `feat/waveforms` | — | merged (local) | Waveform pyramid (48 kHz, 4 levels, cached) + `get_waveform_range`; tile-cached canvases, volume/fade overlays, frame quantization. |
| B3a Ripple + multi-select + zoom | `feat/timeline-editing` | — | merged (local) | `Timeline::ripple_from` (per-track, no sync lock), `move_clips`/`remove_clips`, marquee, group moves, zoom 0.05–2000 px/s. |
| B3b Filmstrips + track heights + minimap | `feat/filmstrips` | — | merged (local) | Per-asset filmstrip (proxy preferred, keyframe sampling for long originals, capped + niced even at 100%), `get_filmstrip` (no MCP tool: `skim_asset` covers agents), height presets (UI-only), minimap. |
| B6 On-canvas transform handles | — | — | todo | |
| A4 Playback | — | — | todo | |
| B4 Mixer | `feat/mixer` | OrellBuehler/kerf#110 | merged | Engine + surface done: `Timeline.master {volume, limiter, ceiling_db}` before `loudnorm` (omitted at neutral), `set_master_volume` / `set_master_limiter`, `get_levels` (one metered ffmpeg pass: per-track + master LUFS / sample + true peak / short-term max), golden family appended as cases 4000..4799. Mixer panel: one strip per audible track + master (shared dB taper with the header slider, one edit per gesture, keyboard nudges), measured Web Audio meters through per-track buses and a master limiter approximation, Measure → `get_levels` (range-aware), harness sample audio. |
| B5 Keyframes v2 | `feat/keyframe-easing` (B5a), `feat/keyframe-channels` (B5b-1) | — | in-progress | B5a done and merged (per-key `Easing`, an eased segment is a 12-piece polyline shared by `transform_at` and the export, exact head trims / slices, `set_keyframe_easing`, TS mirror pinned bit for bit). **B5b-1 done locally** (stacked on main): per-property channels — `Clip.channels: Vec<PropertyTrack>` for scale / position / rotation / opacity, the five colour numbers and the clip volume, one resolver (`Clip::property_keys`) over the legacy bundle, old projects byte-identical (golden 0..4800 untouched, 800 channel cases appended); export: keyed colour = `eq eval=frame`, keyed volume = `asetnsamples=n=128:p=0,volume eval=frame` after `atempo`, a transform keyed in part builds the rest from the static one; plan: `color_at`, `Animated.keys` / `color`, `GpuCaps::keyed_color` / `Unsupported::KeyedColor`; `set_property_keyframes` / `copy_keyframes` / `set_keyframe_easing prop` (core, Tauri, MCP, harness), TS mirror `channels.ts`, Inspector ◇ keys for colour and volume, preview ramps a keyed volume; a split now rebases the right half's animation. **Next (B5b-2)**: dope sheet panel (rows per property, marquee, drag-retime, copy/paste, Alt-duplicate), easing popover, mask params and crop (reformulated as zoom + pan) as animatable, GPU pass + parity case for animated colour, graph editor with bezier handles. |
| A5 Effect parity | — | — | todo | |
| B7 Colour grade + scopes | — | — | todo | |
| A6 Headless agent rendering | — | — | todo | |
| B8 Motion | — | — | todo | |
| A7 Export through the compositor | — | — | todo | |
| fix: keyframed zoom + graph bugs | `fix/keyed-zoom` | — | merged (local) | Moving zoom runs last at the output frame (export, preview stream and still alike); keyed rotation fills transparent; tiny-scale clamp; even HDR fit sizes; alpha sources keep their cut-out. Deliberate golden re-blesses, each proven equal to its family. |
| B9 Backlog | — | — | in-progress | Done: hardening (`feat/hardening-2`: hidden-until-themed window + failsafe, synchronous log writer, colour-literal + WCAG guards with Kerf Light fixes and a stored-theme upgrade, first-launch `.kerf`). Done: SRT/ASS caption import (`feat/caption-import`: tolerant parsers run outside the project lock, cut- or source-timed placement through the transcript caption path, offset, keep-lines, caps). Done: customizable keybindings (`feat/keybindings`: action registry, strict modifiers, override-only storage, Settings › Keyboard). Done: edit modes (`feat/edit-modes`: roll/slip/slide tools with a trim monitor, group split-and-remove on Q/W, cut welding). Done: linked A/V + detach audio (`feat/linked-av`: `link_id` / `source_audio`, group edits, range-based sync lock for J/L-cuts, orphan dissolve, fader-folding detach (a fresh lane for dynamics), pictures never cut to make room and trimmed sound reported, `extract_audio` doubling verified +6.02 dB and fixed, Rust-to-TS differential corpus). Done: menu bar + chrome rework (`feat/menu-bar`: VS Code-style File / Edit / View / Playback / Window / Help in the title bar over the keymap registry, the toolbar row removed and its controls moved into the Preview (transport) and Timeline (tools, ripple, snap, undo / redo, delivery frame); stored workspace layouts recorded against the panels their preset offered and brought up to date, so the mixer reaches an Audio saved before it; Reset workspace says what it did, Reset all added). Done: detachable panels (`feat/detach-panels`: dockview popout groups opened through Tauri's `on_new_window` — a panel's DOM moves into a second OS window while its script keeps running in the editor window's realm, so nothing is synchronised; Window › Panel windows and the tab menu; per-workspace windows stored in the layout and restored with their place; panels ask which window they are in (`windows` / `realm` / `onWindow`); dockview 8.4.1). Open: blurred background,  marker notes, split-and-remove, preview limiter, virtualisation, graph editor, lock checks in single-clip core ops, CSP in packaged builds, **export A/V offset for late-start sources** (a head clip's video is rebased to the clip start, `lead` early against its audio). |
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

- **2026-10-08 — A1b-3 `FrameCursor`, review fixes.** (1) *Repeated timestamps split the parser, not the
  contract.* `ShowinfoParser` is strict (`new()`: pts strictly ascending) because the frame cache keys a
  frame by pts, and a repeat would be one key for two pictures. The cursor has no cache and `Pick::select`
  takes a file's repeated timestamps as they come, so it reads with `ShowinfoParser::allowing_repeats()`:
  an equal pts passes, an earlier one is still `OutOfOrder`. The router's runs stay strict. Until then the
  `repeated-pts` clip of `tests/cursor.rs` failed on main's strict parser; it now passes and is asserted to
  repeat (4290 output frames on 9.0.2; the distro FFmpeg is CI's parity job). (2) *The run's `-ss` is the
  export's spelling for an `Fps` pick*: `kerf_core::export_seek_arg` (`format!("{seek}")`, truncated by
  FFmpeg to whole microseconds), shared by `push_inputs` and the pick's own model of the seek, and none at
  the head of the file; the still's `{:.6}` rounds, and a window start between two microseconds on a
  one-microsecond time base started the run a frame late (an ignored µs-mp4 test; mutating only the
  cursor's spelling fails it). (3) *Skipped output frames are read through*: a forward cursor keeps only
  the newest frame while a pick is undecided (a reversed one its whole window), so skipping no longer
  trips the reversed-window cap. (4) `picks_through` hands each frame to a callback. (5) A run that
  closes stdout and never exits is killed after `frame_timeout` (bounded `try_wait`, the child lock never
  held across a wait; the router's `run_reader` too, `EXIT_GRACE`); a wrong-sized picture is
  `Unsupported`; `for_layer` refuses `Before` without spawning; a failed thread start kills the child;
  `run_args` and the media-making tests spell `passthrough` with `fps_mode_flag()` (now public). (6) The
  cursor's "answer is `select`'s" holds where `-ss` lands on the frame the timestamps say; a long-GOP
  transport stream lands on a later keyframe as the export does, the same known limit as the still's.
  `tests/cursor_fake.rs` (a shell script as `KERF_FFMPEG`) holds the parts that need no FFmpeg.

- **2026-10-08 — A1b-2 `FrameSource`, post-merge review fixes (PR #112's contract: "byte-identical to the
  one-shot decode"; when in doubt, one-shot).** (1) *Cache history decided the answer.* What `-ss t` returns is
  not "the frame at `t`": a seek into the frames an open GOP's keyframe leads returns the keyframe, a
  long-GOP transport stream lands on the next one, and a run that read through from an earlier keyframe (or
  from 0) has the frame the one-shot does not. Reproduced on 9.0.2 with an x264 `open-gop=1` mp4 (frame 49
  against 50 at 1.96 s), an x265 mp4 (11 of 400 times) and a long-GOP `.ts` played from 0 (394 of 400 differ).
  A run therefore answers only for what it can be proved equal on, and every other file is decoded one-shot
  from the first thing that shows it (`State::distrusted`): the container is MP4 / MOV or Matroska / WebM
  (`kerf_core::source_is_indexed_container`, a positive allow-list on `ffprobe`'s `format_name`; the cursor
  refuses the rest too), no B frame (`showinfo`'s `type:`, which the self-test now requires: a closed-GOP file
  with B frames pays too, because `showinfo` does not give the decode order that would tell it from an open
  one), a first frame a whole interval or more after its seek (was two; the coverage claim from `pts - ft`
  went, and a VFR gap now reads as a late seek: speed only), and a file that contradicts itself (time base,
  unreadable or unpaired timestamps, another size, another picture at a cached pts). That last is also the
  negative cache: a file whose runs always died was spawned and killed once a request. Fixtures that would
  have caught it: open-GOP x264, x265 and long-GOP TS walked forward, backward and random through one source
  each against the one-shot (`tests/frame_source.rs`; the old VFR and TS legs are one-shot by design and say
  so), and a jittered-pts mp4 and the CFR mp4 for the files runs *do* answer. (2) *The reaper killed a parked
  run the moment it was reused* (silence counted from the frame it parked after): `Run::want_to` starts the
  clock when a run is wanted again. (3) *Nothing under the state lock waits on a process*: `run_reader`
  held the child's lock across `wait()` (a run that closes stdout and never exits blocked `kill`, and with
  it every request through `stop`); it is a bounded `try_wait` (`EXIT_GRACE`), `stop` kills on a short
  thread, and `spawn` registers the run, releases the lock for `Command::spawn` and takes it again. Tested
  with a wrapper that closes its output and sleeps, and mutation-checked. (4) *`FrameCache::put` panicked
  in a debug build* when two runs of an AVI-like file put two pictures at one pts (release silently kept
  the first): the cache refuses and counts it, the run fails and the file goes one-shot. (5) *A repeated
  timestamp made `Non-monotonic DTS` warnings that mangled `showinfo` lines* (the muxer logs from another
  thread between the calls that make one line: 5 runs in 200 lost a frame line, and the repeated-pts leg of the
  cursor test failed once in five suites): the run's filter chain ends in `setpts=N/TB`, after `showinfo`, which silences the muxer
  (0 in 200). (6) `render_plan_with` has an end-to-end test (five cases, both orders and hints, rendered
  RGBA equal to `render_plan`'s, after checking the compositor is deterministic). (7) *A latent bug the
  container probe's timing exposed*: a request made before a file's time base is known joins the run
  another request has starting (it cannot tell where it is), and when that run ended with no frame —
  because *its* seek (1.99 s) was past the end — the joiner was answered "no frame" for 0.5 s. The
  parity suite hit it in one run of three once its 28 threads reached the frame source together (the
  `ffprobe` they all wait for); a joiner now routes afresh (a fake `ffmpeg` that is slow and empty at
  1.99 s reproduces it; mutation-checked). With the process started off the lock a fresh run is also
  entered with its starter as a waiter (it is otherwise idle to the router and the first to be taken
  while the lock is released), and the parity harness asks again on `Busy` (28 threads for one file is
  more than its three runs: an app renders that frame with FFmpeg). Not done: the 6.1 leg (CI's parity
  job runs it).

- **2026-10-08 — A1b-2 / A1b-3, second review of the frame source and the cursor ("when in doubt,
  one-shot", again).** (1) *Intra refresh.* x264 `intra-refresh=1:bframes=0` (25 fps mp4) marks a sync
  sample every 2 s but only frame 0 is a keyframe; `-ss 2.0` decodes from the sync sample, the decoder
  outputs nothing until the refresh wave ends and the one-shot returns the frame at 2.72 s, where a run that
  read through from an earlier keyframe cached the true 2.0 to 2.68 (forward play 18 of 75 differed, with
  `distrusted == 0`: a run's own first frame is only checked at its own seek, and a run reading through
  never seeks there). Decided per file before any run: `kerf_core::source_seek_points_are_keyframes` lists
  the video packets (rungs of 16 / 64 / 256 / 1024 / 4096, until four sync samples are in view: an
  all-intra proxy costs sixteen packets) and decodes just those with `-skip_frame nokey`; every `K` packet
  must have an `I` picture with `key_frame` at its timestamp. The limit is stated, not hidden: it looks at
  the file's head (two encodes joined are not caught); a probe that cannot tell is "no" (one-shot), retried
  after a minute. Chosen over a lazy check inside the run (packet flags are not in `showinfo`, and listing
  every sync sample of a long original reads the whole file) and over seeking at each sync sample
  (`-read_intervals START%+#1`: faithful, but one seek per sample and it depends on how each build flushes
  its decoder). Finding while wiring it: on 9.0.2 an x264 open GOP's sync samples fail the probe (the
  decoder drops the reordered tail under `nokey`), so the open-GOP mp4 is now refused up front and the
  cursor's known limit shrank to "an open GOP the decoder does not show"; the run's own B-frame check stays
  and has a deterministic fake-`ffmpeg` test (`a_run_that_shows_a_b_frame_marks_the_file`) so it does not
  depend on a build's decoder. (2) *Hardware decode.* Runs passed `decode_hwaccel()` and the one-shot never
  did; a ProRes 4:2:2 10-bit `.mov` through 9.0.2's Vulkan decoder differed on 79 of 80 frames by up to 24
  levels. Runs are software, the retry-in-software code is gone, and `CursorConfig::hwaccel` defaults to
  `None` (an export names its decoder). (3) `FrameSource::cursor` refused every still image (a PNG probes
  as `png_pipe`): exempt like `frame`, and the cursor's gates (transport stream, intra refresh, a file a
  run marked, unrecorded / alpha pixel format) have a test. (4) *The late-seek threshold was the rounded
  interval*: on a 1 ms time base 30 fps is 33.33 ticks, gaps are 33 and 34, and a seek just after a 34 ms
  gap's start reads its first frame 33 ticks on, which `>= 33` called late and marked healthy 30 / 29.97 /
  120 fps mkv and webm files one-shot for good. `late_ticks` is the interval rounded up; an integral
  interval (every 25 fps file here) is unchanged, so the open-GOP and x265 landings are caught as before.
  The residual is stated in CLAUDE.md. (5) `source_traits` ran an unbounded `ffprobe` on the request path
  and respawned it on every frame after a failure: it is killed after 20 s, one probe however many threads
  ask, a failure remembered for 60 s (`ProbeCache`, shared with the seek-point probe). (6) A run the reaper
  failed while it was still starting (a slow spawn is silence) left its record and its starter's claim in
  the table, and a spawn failure woke nobody, so a request that had joined the run slept out its deadline:
  every failed start leaves through one `abandon` (`stop` + `notify_all`), mutation-checked. Not verified
  here: FFmpeg 6.1 (CI's parity job), a proxy made by a hardware encoder through the probe (a `-g 1` x264
  proxy passes).

- **2026-10-08 — B5a: an eased segment is a polyline.** The plan said to realize easing
  in `keyframe_expr` by sampling 8–12 linear pieces. Doing that only in the graph would
  leave the still (`transform_at`) on the exact curve and the export on the
  approximation — a few thousandths apart, and every exactness test (sweep, parity, the
  plan's motion samples) would need a tolerance. So the curve is *defined* as the 12
  pieces, in one function both read; the true bezier only places their points. Overshoot
  is refused (control points in the unit square) so value ranges stay those of the keys;
  overshoot belongs with the graph editor (B5b).

- **2026-10-08 — B5a review fixes.** (1) A chain of nested `if(lt(..))` is a level per
  polyline point and libavutil refuses expressions nested past ~100, so ten eased keys (12
  points each) failed the export and the playback stream on 4.4.2 and 9.0.2 alike. Rather than
  cut `EASE_STEPS` (the curve is *defined* by those pieces; fewer is a visibly coarser ease) or
  limit the key count, `keyframe_expr` writes a balanced tree above 24 points and the old
  chain below it, so no existing graph changed (golden oracle untouched). (2) A head cut exactly
  on a key read as "before the segment" and dropped the key's easing (`<` is `<=`, Rust and TS).
  (3) A key added inside a segment is a split, not a Linear insert: a hold stays held, a curve
  is cut with de Casteljau; presets and rising beziers are exact, an S that turns back is
  clamped into the unit square (re-fitted, 0.04 off at worst measured) — chosen over keeping
  the neighbour's easing and the new key Linear, which changed the motion of a cut that only
  pinned the present pose. (4) `set_keyframes` validates bezier control points like
  `set_keyframe_easing`. (5) A head trim still bakes the rest of a curve into linear keys:
  lossy for the picker, exact for the picture, and now documented rather than changed (`Easing::split`
  could keep one eased key there; the picture is already right).

- **2026-10-08 — A2: the technique is per platform, and only one is confirmed.** Spiked both on
  WSLg (Xwayland, `GDK_BACKEND=x11`, Mesa lavapipe). *Technique 1* (wgpu on the **main window's
  surface** under a transparent webview, `KERF_GPU_SURFACE=window`) **fails on WebKitGTK**: the
  surface is created, configured (Bgra8Unorm, Opaque) and presents without an error ("GPU 430x240 ·
  21 ms" in the status bar) but the screen shows black where the picture should be and the
  Preview's transport bar is left blank, because GTK composes the page into the toplevel's own X
  window and repaints over the swapchain (the toplevel has no native child window: `xwininfo`
  tree is one 1x1 window). *Technique 2* (a **borderless child X11 window**, made with `x11rb`
  with an empty Shape input region, placed over the panel) **works**: the GPU frame is the picture,
  the window follows dock sash drags (431x242 → 198x111 → back), is unmapped for playback, for
  frames the plan refuses (a title, a dissolve) and for dialogs, and an ignored test reads the
  presented pixels back **from the X server** exactly. Evidence: `target/a2-evidence/*.png` in the
  worktree (`window-technique-on-webkitgtk.png`, `x11-child-gpu-frame.png`, …). So **Linux/X11 =
  child window**, a Wayland session = JPEG with the reason in the status (no way to put a window of
  ours inside GTK's). **Windows = technique 1**, as the plan prefers, *without confirmation*: wry's
  `wgpu` example is the evidence (a swapchain on the parent HWND shows through a transparent
  WebView2), tao sets no `WS_CLIPCHILDREN`, and Tauri's own `transparent: true` is avoided on
  purpose — the runtime then paints the window with `softbuffer` (GDI) on every redraw, which would
  fight a DXGI swapchain; the webview is made transparent at runtime instead
  (`set_background_color` alpha 0) and opaque again when the backend goes. **macOS = none**: a
  surface under the webview needs a transparent window, i.e. Tauri's `macos-private-api` (a
  private WKWebView key and a feature that changes every macOS bundle), which is not enabled blind;
  `KERF_GPU_SURFACE=window` forces the attempt for whoever can run it.
- **2026-10-08 — A2: the window stays as it is.** No `transparent` in `tauri.conf.json`, so the
  hidden-until-themed reveal (`backgroundColor`, `reveal.ts`, the 3 s failsafe) cannot change; the
  only transparency (Windows, technique `window`) is applied at runtime when the setting is on and a
  frame asked for a backend, and undone when it goes. What that does to the first visible frame on
  Windows is a real-machine question (below).
- **2026-10-08 — A2: the plan decides, in the backend, and the JPEG comes back in the same call.**
  `get_preview_frame` plans off the project lock, asks `RenderPlan::reasons(caps, size)` at the size
  it would render, and returns the GPU result or FFmpeg's JPEG of that frame with the reasons — one
  round trip, one decision point, no second command for the fallback; a refusal is a value, not an
  error. The page only adds what the *surface* knows and the plan cannot: playback is streaming,
  there is nothing to show, the page has to draw over the picture (title box, trim monitor, guides)
  or has a dialog / menu / drag ghost over it, on a surface that sits above the page (technique
  `child`). That last one was found live: the Settings dialog opened *underneath* the child window.
  `covered` is a 3x3 `elementFromPoint` grid, every 150 ms and after each click / key; under a
  transparent webview it never matters, because the page draws over the surface.
- **2026-10-08 — A2: the render size keeps the canvas's shape.** `still_size` at 430 px is 240 rows
  for an ideal 241.9, so the compositor letterboxed 16:9 footage into a canvas a hair taller than 16:9
  and a 2 px pillar showed at the right edge of the surface (measured on the screenshot). The width
  is now the nearest even one (within 48 px, ≤ 1920) whose size has the canvas's aspect to the row
  (416x234 for a 431x242 panel); the presenter scales the ≤ 3 % difference bilinearly. A canvas with
  no exact width nearby (1998x1080) takes the least wrong.
- **2026-10-08 — A2: two colours, not one.** Under a transparent webview the surface is seen wherever
  no page element paints, not only in the frame: the pane's surround is a page layer with a hole,
  but the dock's gaps and the timeline's empty area paint through the (now transparent) ancestors.
  The presenter therefore paints a *matte* (`--frame-matte`) inside the frame where the picture is
  smaller and a *backdrop* (`--surface-app`) beyond it. A headless-Chrome screenshot of the harness
  (`?gpusurface=1`, transparent default background) showed the first version leaving the area below
  the timeline transparent.
- **2026-10-08 — A2: Hint::Scrub for every A2 frame.** The settled frame and a scrubbed one go
  through the same command and the frame source's `Scrub` intent (runs, a cache; `Exact` is an
  agent's "one frame, take nothing from anyone"). Nothing is done for scrubbing at GPU speed — that is
  A3 — but nothing is in its way: the backend is one `frame()` call per playhead position.
- **2026-10-08 — A2: a stable route, and a pump that reads nothing.** `routePreview` returns a new
  object whenever any input is recomputed (the titles under the playhead are a new array each tick),
  and an effect that read `route.via` re-ran on every seek, hiding and re-showing the child window
  each time (seen in the backend's own log). The first fix made the effect depend on `routeVia` /
  `routeOverlays` but left `pump()` reading `route` synchronously from the effect, so the effect still
  re-ran on every change of the trim monitor, an overlay selection or the GPU status object being
  replaced — one redundant FFmpeg composite each, **also with the setting off**. The fetching is now
  `frame-pump.svelte.ts`: `run()` (the effect body) reads the route as primitives, and only reads
  `routeOverlays` on the GPU route, where the backend takes it; `pump()` reads nothing reactive before
  its first await. `frame-pump.test.ts` compiles the module with Svelte's own compiler, runs it on
  Svelte's runtime under bun and counts what the pump asked for (setting off: trim monitor, titles,
  covered, status churn = no fetch; on: one fetch per answer however often the status is replaced;
  both mutations — reading `routeOverlays` always, reading it in `pump` — fail it).
- **2026-10-08 — A2 review round (independent review of the branch).** Nine findings, all fixed.
  (1) *A hide that landed during a frame was undone by it*: `attempt` checked `bounds.visible` once at
  the top and `Child::place` re-mapped the window after a long decode, and the page's `sameReport`
  never re-sent. The bounds lock is now held across re-check + apply + present and by `set_bounds`
  around store + hide; a hide wins (the frame is dropped to the JPEG); a *moved* frame is shown at its
  new place instead (re-laid-out under the lock, the picture scaled into it) rather than bailing to a
  JPEG per frame of a drag. (2) the pump above. (3) *`RustConnection::connect` hung 132 s on an
  abstract-only X server inside the render lock*: the abstract socket is tried first, the backend is
  built off the render lock with a 10 s deadline on its own thread. (4) the enable flag flips
  synchronously, a frame in flight re-checks it before it builds and before it shows. (5) panics are
  caught (the GPU path, the build thread) and the page treats a rejected command as a JPEG frame; an
  attempt marker file turns the setting off on the next launch if the process died during the first
  GPU frame. (6) the Windows transparency is the last build step and restored by `Drop`. (7) one
  process-wide Xlib display (tao's `display_handle()` opens a new one per call and never closes it),
  and `Backoff` forgives only a run of 20 shown frames. (8) every X request on the child is checked.
  (9) turning the setting on is a retry; the page flips to the GPU only after the settings write
  resolves and then asks for the frame again; a shown child window follows the frame's move at once
  and is re-raised per present; bounds reports carry a sequence number; the scroll listener is
  throttled to a frame. *Not done, by choice*: the name of the display is `XDisplayString` of our own
  `XOpenDisplay(NULL)` (`$DISPLAY`, what tao opens too) rather than GDK's, which would put a GTK call
  on a worker thread; `--display` on the command line is not honoured by either.

- **2026-10-08 — B5b-1: per-property channels, and why the bundle is not migrated.** The plan said
  `Vec<PropertyTrack{prop, keys}>` "with migration from the whole-transform `Keyframe`". Converting
  the bundle into five tracks on load would have been lossless, but it makes every legacy clip's graph
  depend on the conversion (the golden generator assigns `clip.keyframes` directly and ~50 tests do),
  leaves two representations alive anyway for the UI (the Inspector's Animation list, the timeline's
  diamonds read `clip.keyframes`), and rewrites projects a user only opened. So **the bundle stays as
  it is and is one source for a number that has no track of its own**: `Clip::property_keys(prop)` is
  the only reader (track → bundle → static), `transform_at` / `color_at` / `volume_at` / the export / the
  plan are views of it, and the first per-property write *detaches* just that number (copies its bundle
  keys into a track; an empty track means "static, whatever the bundle says" and is pruned when the bundle
  goes). Chosen over migrate-on-first-write (which makes the Inspector show an animated clip as
  unanimated until the UI reads tracks) and over "a track and the bundle both apply" (no answer to which
  wins). Cost: edits have two code paths (`rebase_animation` runs the bundle and `rebase_channels`) —
  both pinned, one TS mirror each.
- **2026-10-08 — B5b-1: what FFmpeg does with the expressions (4.4.2 and 9.0.2 identical).**
  *Colour*: `eq` takes an expression for every number (`brightness`, `contrast`, `saturation`, `gamma`,
  `gamma_r`, `gamma_b`) under `eval=frame`; `t` is the frame timestamp, so after `setpts` it is timeline
  time and the existing `(t-start)` form works. 18 rendered cases (each number, all five, late, speed 2 /
  0.5 / reversed, beside a keyed position + opacity, under a moving zoom, beside a static grade, range
  export, five rates) read 0 levels from the static-`eq` still at every fifth frame (3 with a keyed
  opacity: the still's RGB round trip vs the file's `geq`). Contrast pivots on luma 128: a mid-grey
  picture shows nothing. *Volume*: `volume=eval=frame` holds one gain per frame and the decoder's frames
  are 1024 samples (21 ms), so the chain cuts them to 128 samples first (`asetnsamples=n=128:p=0`; the
  default `p=1` pads the last frame with silence) — the render is then the unkeyed render scaled by
  the curve at each frame's start to 1.5e-8, and the lag against the curve is at most 2.7 ms. It sits
  after `atempo` (so `t` is the clip's playing time: checked at 0.5 / 2 / reversed / late / range
  export). A hold's step landing exactly on a frame start flips on float rounding of `t` — the tests keep
  steps off the 1/375 s grid. Mutation checks: `eval=init` and a missing `asetnsamples` both fail the
  rendered tests; the sweep fails a changed temperature coefficient and a static opacity / turn
  dropped from a clip keyed in part. *Not animatable yet*: crop edges (`crop`'s output size is fixed
  when the graph is configured) and mask parameters (a `geq` expression could carry them; waits for an
  editor that can set them).
- **2026-10-08 — B5b-1: a split did not carry the animation (fixed).** `Timeline::split_clip` copied the
  clip, so the right half's keys stayed clip-local to the *old* start and its animation replayed from
  the first key — for the bundle and the reframe camera as much as for channels. It now rebases the
  right half (`rebase_animation(at - start)`, the pose it opens on pinned, later keys shifted), with a
  Rust and a TS test; the linked-A/V corpus did not move (no keyed clips in it).
- **2026-10-08 — B5b-1: the plan.** A transform keyed in part is not built like one keyed in full, so
  `Placement::keyframed` became `Option<Keyed {scale, rotation, rotates, opacity}>` (`Keyed::all` is the
  bundle) and `Animated.keys` carries it; `Animated` is `Some` for colour-only clips too, with `keys:
  None`. A Motion plan refuses a keyed colour (`Unsupported::KeyedColor`) until a pass and a parity
  case exist; a still plan draws the sampled `color_at`. The preview's Web Audio gain ramps through a
  keyed volume's points (`gainBreakpoints`); the timeline's volume line and waveform scaling still read
  the static gain (the dope-sheet slice owns those).

- **2026-10-08 — B5b-1 review fixes.** (1) The preview collapsed a hold's two points at one time
  into one and ramped across the whole segment where the export steps (`gainAutomation` now ramps
  to the value a step leaves and `setValueAtTime`s the next). (2) `TranslucentMatrix` was skipped for any
  Motion clip with a keyed number; it is skipped for a keyed *opacity* only. (3) `channel_changes`
  compared tracks, so holding a bundle-driven number static diffed empty and `apply_staged` discarded the
  proposal; it compares the effective keys (a detach with the bundle's own keys is rightly no change).
  (4) `set_keyframes` now prunes empty tracks like the harness did, and the MCP descriptions of
  `set_keyframes` / `clear_keyframes` / `set_volume` / `set_color` / `set_transform` say what an empty list
  clears and that a keyed number ignores its static value. (5) A keyed volume is clamped to 4 on read and
  a static one is not, so `detach_audio` sends a clip whose keys the fader ratio would push past 4 to an
  equal-fader lane instead of folding the ratio in (chosen over clamping both, which would have changed
  the loudness of every existing static clip above 4, and over splitting the ratio between the keys and a
  lane gain, which has no home for it). (6) Two older bugs found by the review: `cut_range_pieces`'
  tail replayed its animation from the first key, and `split_clip` left both fades on both halves (a
  dip to black at the cut); each fade now stays on the half holding its edge, clamped to it (not left
  longer than the half, though the render clamps it anyway, so the stored value is the rendered one).
  The links corpus and the golden argv oracle did not move.

- **2026-10-08 — B9: menu bar, no toolbar row; stored layouts carry what they were offered.**
  Reported from the Windows build: *Reset Color workspace* "does nothing" and Audio looked
  like Motion with no Mixer. The reset path was run end to end in the browser harness (a
  rearranged layout, a tab dragged to another group, a closed panel, a stale entry from the
  older build that wrote every visited workspace, five window sizes and device pixel ratios,
  a folded library) and **always put the preset on the dock and cleared the entry** — so no
  race with the single-flight write, the reference logic or the legacy `layout` was found
  (the legacy layout only ever becomes Edit). What the report matches is state: the build
  that wrote an entry for every workspace the user merely visited left a *copy of that day's
  preset* behind for each, so Reset on Color put back a layout the same as the one it
  replaced (nothing visible changed, and nothing said so), while Audio's copy predated the
  Mixer and never would learn about it — a stored layout is a snapshot, and nothing recorded
  what it was a snapshot of. Fixed there: each stored layout records the panels its preset
  offered (`offered`); on read a copy of a preset is dropped, a panel the preset gained is put
  where the preset has it (`insertPanel`: tab mates, else beside the nearest neighbour with
  the preset's share, else a tab by the preview — never a reset to make room), a panel the
  layout was offered and lacks stays closed; Reset forgets the library tab too, says what it
  did (including "already default"), and a dock that takes neither layout nor preset is
  reported. **Not found, so not claimed:** a case where the live dock ignores a reset. If the
  Windows build still shows one, the new error toast and the log (`webview` target) will say
  why. Chrome: the toolbar row and the rule under the title bar are gone, the dock starts under
  the title bar; the menus are one widget over one data file, every entry a registry action
  (so the key shown is the user's) or a typed command. **Decisions:** the version chip, the
  gear and the bell stay at the right (the chip is the only thing that says an update is
  waiting; Help also has *Check for updates…*); the project name and badge sit at the right
  of the title bar and the path moved to the status bar (the left cell is the menus'); Save
  is one entry that reads "as…" once there is a file, because `save_project_as` is the only
  save; Quit reuses the close guard's `destroy` (no new capability); new actions are unbound
  except `S` for snapping. The window keeps native decorations — custom ones (menus in the
  caption bar) are a follow-up, not this change.

- **2026-10-08 — B9 menu bar review fixes.** An independent review reproduced in headless
  Chrome: (1) an Alt pressed and released *during a drag* (the timeline's links-off override)
  armed the bar and stole focus on release, so the next Space opened File instead of playing —
  `alt-tap.ts` now never arms while a pointer button is held and cancels on a window blur
  (Alt+Tab); (2) a hover timer from Edit's Tool submenu reopened it after the pointer moved to
  View — cleared on every title change; (3) a chord inside an open menu dropped focus to the
  body — it goes back to where it was; (4) a title takes focus on click (WKWebView); (5) *Reset
  all workspaces* throws away every arrangement and now asks first (only when one is stored);
  (6) the transport had no menu home once the Preview panel was closed — a **Playback** menu
  carries all of it, with the marks and markers, as registry actions. The compact-bar test is
  the *left cell's* width (`MENU_FULL_PX`), not the window's.

- **2026-10-08 — B9: detachable panels (Option A, dockview popouts through `on_new_window`).**
  Question: separate Tauri `WebviewWindow`s per panel (B) or dockview popouts that share the
  editor's JavaScript realm (A). B needs ~75 `$state` fields across six singletons synchronised,
  a leader for transport and audio, a high-rate playhead event, a `settings-changed` event and a
  copy of every cache per window, and cannot drag an asset between windows; A needs none of it.
  **Measured by hand under WSLg (WebKitGTK 2.50.4, Tauri 2.12, wry 0.57, packaged `tauri://`) in one
  session before GUI runs were ruled out here:** a window
  built from `on_new_window` with `window_features` is scriptable (`opener` is the editor, the
  panel's Svelte handlers and `$state` drive it, a real click in it increments the editor's
  state); a drag, a context menu, a shortcut key, playback with the playhead and meters, a theme
  switch and an HTML5 asset drag from a detached Library into the main Timeline all work across
  windows; closing from the window manager re-docks the panel; quitting or reloading the editor
  takes the windows with it; a relaunch restores the windows where they were. **Found and fixed:**
  dockview 8.3.1 refuses `tauri://` (bumped to 8.4.1, which also polls `closed`); a window
  declared in the config has no `window.open` handler (the main window is now built in code,
  `create: false`); WebKitGTK blocks a `window.open` no gesture asked for
  (`javascript-can-open-windows-automatically`, off by default — a restore at launch has none);
  WebKitGTK ignores `window.open` features (the rectangle is announced to the backend first);
  `window.close()` leaves a blank window (the widget's `destroy` now destroys the window); the
  position the platform reads differs from the one it sets (32 px under WSLg: moved by the error
  once after a restore); a popup shares the opener's content manager so an init-script label
  does not reach it (the label is chosen by the page); a missing popout page falls back to
  `index.html` and boots a second editor (real `popout.html` + a boot guard); a main-realm
  `IntersectionObserver` reports a detached element not intersecting for good; `<svelte:window>`
  listeners never hear a detached window (a drag stuck for want of `pointerup`); Svelte's delegated
  handlers live on a mount root, so a menu drawn in a detached window needs its own. dockview
  reports `PopoutGroup.window` as null once a window closed, so a closed window is found by
  diffing against `getPopouts()`. **Decisions:** the editor window keeps at least one panel;
  a hand-detached panel goes to another monitor when there is one; windows are per workspace and
  stored in its layout (`popoutGroups`, at most seven, the page always the popout page); a
  detached window has no menu bar (shortcuts are forwarded); toasts and the notification bell stay
  in the editor window; file drops onto a detached window do nothing in v1; the GPU preview
  (merged since) is routed to the JPEG for a Preview in a window (`routePreview`'s `detached`) —
  its surface is bound to `main` — and the Preview measures its transport bar in its own window. Disturbing nothing: with no panel detached every path is the
  one it was (the only change on the way is the main window being built from its config in code).
  WSLg also kills the web process when a second popup opens with GPU compositing
  (`SkiaGPUWorker` fault in swrast_dri.so; `WEBKIT_SKIA_ENABLE_CPU_RENDERING=1` avoids it) — an
  environment fault, not a Kerf one.
  **Repeatable evidence, no desktop:** bun tests for the layout reader (`popoutGroups`, hidden
  reference groups, limits, `sameArrangement`), the window registry and realm helpers, the label
  book and placement correction, the store against a stand-in dock, the workspace restore order
  (announce, build, wait, baseline) and the menus; Rust tests for the announcement queue and the
  placement function (`place`); and a headless-Chromium run against the browser harness
  (`bun run dev`, Playwright, `window.open` popups that share the realm like the desktop's) that
  opens a Timeline window and drags a clip in it, opens its context menu there and runs an item,
  plays from a key pressed in it, switches theme, sees the sliders filled, goes inert behind a
  modal, stores and restores the window across a reload, closes it and gets the panel back, and
  uses the Window menu — 29 checks, all passing. (The script lives in the author's scratch space;
  it needs only Playwright and the dev server.) Chromium also showed the observers differ by
  engine — an editor-realm `IntersectionObserver` says *intersecting* for a detached element
  there and *not* on WebKitGTK — which is why they are made in the element's own window.

- **2026-10-09 — B9 detached panels review fixes.** An independent review found no blockers and
  these: (1) a frame pending in a window that then hid or closed never ran, so the transport clock
  and the meters froze while the picture played on the other screen — `windows.requestFrame` now
  hands out a handle the registry moves (`visibilitychange`, window added / removed, a 250 ms
  watchdog), tested against stand-in windows that run frames only while they show; (2) `window.open`
  was checked for the popout path only — it also has to be the editor webview's scheme, host and
  port (`is_popout_url(url, main)`); (3) a refused open during a restore left its spent label at the
  front of the book — `#failed` now claims it unless dockview refused the URL; (4) a bun test pins
  `POPOUT_URL` to `POPOUT_PATH`; (5) the last editor-window panel is greyed out in *Panel windows*
  with the rule's reason instead of refusing with a toast.

## Needs a real machine

- B1–B3: every UI change was verified in the browser harness only; the Tauri desktop window (WebKitGTK / WebView2 / WKWebView canvas, events like `ripple-mode-changed`, real `get_waveform_range` / `get_filmstrip` against footage) needs a desktop run.
- B9 menu bar: on Windows (WebView2) the title-bar menus under the native caption bar, Alt / F10 focus, an Alt-drag in the timeline not stealing focus, and the new Reset workspace feedback against the stored layouts of the build that reported it.
- B9 detached panels (WebKitGTK/WSLg only so far): **GPU preview on** — a Preview dragged into a window while the surface shows (the child X11 window must go at once and the picture come back as the JPEG, and return when it is docked); **WebView2** — `NewWindowRequested` + `SetNewWindow` giving a scriptable popup, building a window inside the handler without a deadlock, `window.close()` then `Destroyed`, whether features are honoured, `screenX` and the saved rectangle on mixed-DPI monitors, a main-realm `ResizeObserver` observing a popout, frames when the editor window is minimized (the follow-the-visible-window logic is unit-tested against stand-ins only), the editor webview's `url()` read from inside the `window.open` handler and matching the popout URL's origin (`http://tauri.localhost` here), HTML5 tab / asset drags between windows against Tauri's drag-drop handler (kept on for file drops); **WKWebView** — `window.close()` is a no-op so `close_popout` has to be the way (untested), `window_features` placement (flipped y), window-level key events, frames in an occluded window (and `visibilitychange` firing for a minimized or covered window), the origin check on `tauri://localhost`, `javaScriptCanOpenWindowsAutomatically` (true by default per the headers); **everywhere** — real X11 / Wayland (positions cannot be set on Wayland; the 32 px offset is WSLg's), the single-instance second launch with windows open, a native menu bar not being inherited by a detached window, the packaged CSP applying to the popout page.
- B9 hardening: first visible frame / no flash per OS, the 3 s failsafe, focus, second launch mid-boot, a mistyped `.kerf`, the CSP in packaged Windows/macOS builds.
- B4: `get_levels` on a real long multi-track cut (here: synthetic tones, ~100x real time per true-peak meter) and the Mixer panel's meters against real playback.
- A0: kerf-gpu on a real GPU (Vulkan/Metal/DX12) and WARP; macOS has no software adapter (`KERF_GPU_ADAPTER=hardware`). Real-GPU still timings.
- A2 (Windows): the technique is code only. To confirm: turn *GPU preview* on in Settings with a
  real GPU and with WARP (`KERF_GPU_ADAPTER=software`) and look for the picture in the frame (the
  status bar says "GPU …" either way — **the page cannot tell a surface nobody sees from one it
  does**, which is exactly what the Linux window technique does, so it is the first thing to look
  at), titles and guides drawn *over* it, the transport bar and every other panel intact, a dialog
  over the frame, a window resize and maximize, a move between monitors of different scale
  (125 / 150 / 200 %: the bounds are rounded by edges and mapped through the webview's size, never
  tested against a real DPI), minimize / restore, and the first visible frame at launch with the
  setting on (the runtime `set_background_color` alpha 0 / back to `#0f1318`). If the surface is
  black or the page corrupted: `KERF_GPU_SURFACE=off`, and the child-window route is the fallback
  (a native child HWND is not written).
- A2 (macOS): no technique; the path is Tauri's `macos-private-api` (feature + `app.macOSPrivateApi`,
  then `transparent: true` at window creation, only when the setting is on at launch) and the same
  `window` technique; Metal's `CAMetalLayer` replaces the content view's layer and the WKWebView is a
  subview, as in wry's `wgpu` example. Needs a Mac, and a decision about shipping the private API.
- A2 (Linux): a Wayland session (the default on most distributions) has no technique and says so in
  the status; X11 on a **real GPU** (Mesa radeonsi/iris, NVIDIA) was not run — Vulkan on an Xlib
  child window, `GDK_SCALE=2`, a compositing manager that stacks windows differently from Xwayland's.
  Real-GPU frame times (A3's latency target of < 33 ms at 1080p) are unmeasured: the numbers above
  are lavapipe's.
- A2 (Windows, more): `tao` registers the window class with its own background brush and the runtime
  paints the window on `WM_ERASEBKGND`; whether that opaque erase of the parent window, which sits
  *under* the swapchain, is covered by the swapchain's present (DXGI flip model: it should be, the
  swapchain replaces the redirection surface) or flashes at a resize is unverifiable here and is the
  first thing to look at when the picture is black on Windows. `set_background_color` alpha 0 reaches
  the webview layer only (the window layer ignores alpha).
- A2 (binary size, for Windows and macOS bundles): linking `wgpu` (Vulkan / Metal / DX12 backends) into
  `kerf-app` grows the Linux release binary by **7.4 MB (7.1 MiB, +9.8 %)**: 76,145,104 → 83,575,232 bytes, `cargo build --release -p kerf-app --no-default-features --locked` of `main` and of this branch, same toolchain, `strip = "debuginfo"`; the Windows installer and the macOS
  bundle were not built here and will grow by a comparable amount (DX12 and Metal pull in different
  crates), which is worth a look at the PR-build artifacts.
