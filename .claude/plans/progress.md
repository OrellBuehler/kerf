# GPU compositor + editor roadmap — progress log

Plan: `.claude/plans/gpu-compositor-and-roadmap.md`. One row per work package.

| WP | branch | PR | status | notes |
|---|---|---|---|---|
| A0 GPU feasibility spike | `feat/gpu-a0` | — | review | **Gate: PASS** on lavapipe (FFmpeg 6.1.1 and 9.0.2): 77 renders after review fixes (letterbox matte, opacity RGB round trip emulated, swscale scaler port, transposed decodes/alpha refused, wgpu error scopes); flat max ≤ 8/255, PSNR ≥ 40 dB, busy-source cases ≥ 45.8 dB. Composite in YUV like `overlay`, swscale-bicubic scaler, vf_eq tables, BT.601 output (what the FFmpeg still does). Bench (lavapipe, 1080p/1/3/6 layers): ffmpeg 119/242/414 ms vs gpu 132/249/509 ms — decode-bound; real GPU unmeasured. +5 MB binary (Linux). |
| B1 Workspaces + library rail | `feat/workspaces` | — | merged (local) | Two review rounds; awaits push. |
| A1 Frame source + render plan | — | — | todo | |
| A2 Native preview surface | — | — | todo | |
| A3 Scrub + live drags on GPU | — | — | todo | |
| B2 Waveforms + clip overlays + frame snapping | `feat/waveforms` | — | merged (local) | Waveform pyramid (48 kHz, 4 levels, cached) + `get_waveform_range`; tile-cached canvases, volume/fade overlays, frame quantization. |
| B3a Ripple + multi-select + zoom | `feat/timeline-editing` | — | merged (local) | `Timeline::ripple_from` (per-track, no sync lock), `move_clips`/`remove_clips`, marquee, group moves, zoom 0.05–2000 px/s. |
| B3b Filmstrips + track heights + minimap | `feat/filmstrips` | — | in-progress | |
| B6 On-canvas transform handles | — | — | todo | |
| A4 Playback | — | — | todo | |
| B4 Mixer | — | — | todo | |
| B5 Keyframes v2 | — | — | todo | |
| A5 Effect parity | — | — | todo | |
| B7 Colour grade + scopes | — | — | todo | |
| A6 Headless agent rendering | — | — | todo | |
| B8 Motion | — | — | todo | |
| A7 Export through the compositor | — | — | todo | |
| B9 Backlog | — | — | todo | |

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

## Needs a real machine

- A0: kerf-gpu on a real GPU (Vulkan/Metal/DX12) and WARP; macOS has no software adapter (`KERF_GPU_ADAPTER=hardware`). Real-GPU still timings.
