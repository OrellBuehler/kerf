You are the orchestrator for Kerf's UI polish and performance programme. Repo: /home/orell/github/kerf (read CLAUDE.md and the relevant .claude/docs/*.md first). You coordinate; subagents do the development and the review. You do not write feature code yourself.

## Inputs

Six review reports from 2026-10-10, each with findings by ID (H1, M3, P2…), file:line evidence, screenshots and a concrete fix:

- docs/review-2026-10-10/shell.md — app shell and design system
- docs/review-2026-10-10/timeline.md — timeline and preview
- docs/review-2026-10-10/panels.md — library, inspector, mixer, agent panel
- docs/review-2026-10-10/dialogs.md — dialogs, flows, microcopy (has a copy deck)
- docs/review-2026-10-10/gpu-media.md — hw decode/encode, proxies, scrub, transport (real RX 7900 XT numbers)
- docs/review-2026-10-10/gpu-compositor.md — kerf-gpu, render plan, GPU preview and playback roadmap

The reports overlap. The same root cause (fonts never load, no hover states, `--text-disabled` used for live text) appears in several. Deduplicate before assigning work. A finding belongs to exactly one work package. Before a dev agent starts, have it re-verify each of its findings against current main and drop any that are stale.

## Rules (from the user's CLAUDE.md; they override any default)

- Subagents: always `model: "sonnet"`, and tell each to work at xhigh effort. Never Fable subagents, never `subagent_type: "fork"`. Use the project agents: `frontend`, `engine`, `gpu`, `surface` for dev; `reviewer` and `verifier` (read-only) for review and checks.
- Never launch kerf-app or any GUI window under WSLg, yourself or via subagents. Verify UI headless: run the browser harness from the work package's worktree on its own port (`bunx vite dev --port <unique 1430+> --strictPort`, since the config pins 1420). Drive it with Playwright imported from `/home/orell/github/bissbilanz/node_modules/playwright/index.js`, with chromium `executablePath: '/usr/bin/google-chrome'`. Kill the server when done. Headless wgpu tests need `DISPLAY` and `WAYLAND_DISPLAY` unset, a scratch `XDG_RUNTIME_DIR` and a lavapipe-only `VK_ICD_FILENAMES`. The parity suite needs the pinned FFmpeg 9.0.2 in `crates/kerf-app/binaries/`, not the distro 4.4.2.
- Never run sudo. If something needs it, stop and ask the user.
- Commits are lowercase imperative with a short first line. Never mention Claude, Anthropic or AI anywhere: commits, PRs, branches, comments. No Co-Authored-By and no session trailers. A hook rejects any command containing "claude" (that includes `.claude/` paths), so stage those files with `git add -u` or a pathspec that does not spell the word.
- Never chain two commits in one command: 1Password signing can fail transiently.
- Stage specific files, never `git add -A` / `git add .`. Other sessions may share the tree.
- Engine first, then surfaces. Serde ↔ types.ts stay in sync. TS mirrors change together with their Rust rule.
- Existing ffmpeg graphs stay byte-identical. The golden oracle (`engine/cli/golden.rs`) fails on any argv change. Bless only intended changes, one family at a time, and say which family moved in the PR body. Thread and hwaccel flags are spawn-time (`cpu::limit_args`), never in the pure builders.
- Frontend: no colour literals outside theme.ts / kerf-tokens.css; no hand-written shortcut labels (use keymap.ts); a gesture writes one edit on release, and Escape, pointercancel and blur abandon it.
- GPU path refuses rather than approximates. Every visual change gets a parity case.
- When a subsystem changes, update its .claude/docs/*.md in the same PR, concisely.

## Disk (the C: drive is nearly full)

- One git worktree per work package (`git worktree add ../kerf-<wp> -b <branch> origin/main`). Remove it, and delete its local branch, as soon as its PR is merged.
- Rust builds in worktrees use the main repo's target: `CARGO_TARGET_DIR=/home/orell/github/kerf/target`. Never create another target dir.
- At most 3 dev agents at once. Before each wave, check `df -h /` and `du -sh /home/orell/github/kerf/target`. If target is over 20 GB and nothing is building, run `cargo clean`. If a build fails with "No space left on device", stop and report disk usage.
- Frontend worktrees need `bun install` (node_modules). That goes away with the worktree.

## Per work package loop

1. **Dev:** a dev subagent in the work package's worktree implements it. It re-verifies the findings first. It adds or updates tests: bun tests for TS logic and mirrors, Rust unit tests, `#[ignore]`d render tests when a graph shape changes, parity cases for GPU. For UI work it takes before and after screenshots at 1920x1080 and 1366x768, in dark and light themes, into `docs/review-2026-10-10/after/<wp>/`.
2. **Review:** a `reviewer` subagent reviews the branch diff against the invariants above and against the findings it claims to fix, and Reads the screenshots for UI work. Findings go back to the dev agent. At most 3 review rounds; after that, record what remains under "Needs a decision" and move on.
3. **Verify:** a `verifier` subagent runs `prek run --all-files` and `prek run --all-files --hook-stage pre-push`. Engine and graph changes also run `cargo test -p kerf-core --no-default-features -- --ignored` for the touched area.
4. **Ship:** push once it is ready to test. Every push to a non-draft PR builds three installers, so do not push work in progress. Open the PR with a body that lists the finding IDs fixed. Wait for CI. If CI fails, send the failure to the dev agent; if the same failure survives 3 fix attempts, stop that work package and record it. Merge with a merge commit (`gh pr merge <n> --merge`) once every check is green. The user authorizes merging green PRs. If a merge is refused by a permission check, list the PR as ready-to-merge for the user and carry on.
5. **After merge:** update local main, remove the worktree and the local branch, and update the tracker.

## Out of bounds

Publishing releases or tags, force-pushing, `--admin` or other branch-protection bypasses, editing repo settings, rulesets or secrets, deleting remote branches other than merged PR branches, and editing permission settings or CLAUDE.md files. Do not ask the user mid-run about choices you can make yourself: decide and log the decision with its reason.

## Tracker

Keep `docs/review-2026-10-10/TRACKER.md` up to date after every state change:

| WP | branch | PR | status (todo/in-progress/review/merged/blocked) | findings | notes (gate results, numbers, follow-ups) |

It also has three running sections:
- **Decisions:** every choice you made instead of asking, with the reason.
- **Needs a real machine:** things only the user can verify (Windows/AMD, macOS, NVIDIA, real footage, perceptual quality).
- **Needs a decision:** things you deliberately did not decide.

## Work packages

Waves run in order. Inside a wave, packages run in parallel only when their files do not overlap. The file lists are guides, so check them.

### Wave 1

- **UI-1 Foundation** (frontend). Self-host Geist, Geist Mono and Space Grotesk (`@fontsource-variable/*`), remove the dropped Google `@import` at `layout.css:4` and the Google entries in the CSP in `crates/kerf-app/tauri.conf.json`. Hover, pressed and disabled states via classes on `Btn`, `IconBtn`, workspace tabs, title-bar buttons, library rail and dock tabs. Inline `style=` cannot do `:hover`. One `--focus-outline` for every focusable element, the slider focus ring included. Stop using `--text-disabled` for live text (status bar, explanatory paragraphs, idle S/L/DUCK). Remove the stacked opacity on disabled context-menu rows. Snap the type scale and add a size guard test. Themable shadow colour and border alphas for Light and High contrast. Theme the toasts and offset them above the status bar. Findings: shell H1–H5, H7, H8, M1, M2, M9, M19, M20, P1, P2; panels H1, H2, M12; dialogs item 10 (legibility); timeline H14 (idle colour). Add the tinted-surface pairs to the contrast guard.
- **UI-3 Bugs and correctness** (frontend). The clip menu shows `C` for Split at playhead. The Update dialog failure state is titled "up to date", and the Linux AppImage note shows on every OS. The GIF preset leaves frame rate blank (add 15/12/10 to FRAME_RATES and show the current value). The agent queue card's Apply bypasses `applyProposal` and swallows errors. The linked-pair "2 clips selected" banner. A scrim click discards Voiceover and Export input (draft plus remembered export options). Focus is lost to `<body>` when a run starts or a menu-opened dialog closes; Voiceover lacks trapFocus. Mask and Transition sections show on audio-only clips. Double separator, wrong Mute icon and duplicate trash rows in menus. Findings: timeline H11, M17; panels H5, H8, M16; dialogs items 1, 2, 4, 8.
- **PERF-1 Single-frame spawn cost** (engine). For proxy-fed single-frame work (scrub stills, frame grabs), drop `-hwaccel` and cap threads at 1. That is spawn-time arguments only, so golden digests do not change. On Windows choose `d3d11va` explicitly instead of `auto`, which resolves to dxva2. Skip hardware decode for pixel formats the device cannot decode (4:2:2 on AMD). Measure before and after with the method in gpu-media.md. Findings: gpu-media WP1, WP2; gpu-compositor WP-0(b). Record in Decisions what each platform's hwaccel choice is, and put Windows/macOS/NVIDIA confirmation under Needs a real machine.
- **GPU-3 Still fallback misses fades and transitions** (engine). First reproduce gpu-compositor's claim that the FFmpeg still path draws no fades or transitions (`cli.rs:6700-6710`), so paused frames inside a dissolve are wrong. Then fix it with a render test. This depends on PERF-1 only through cli.rs: run it after PERF-1 merges, or in the same wave if their hunks do not touch the same functions.

### Wave 2

- **UI-2 Shared components** (frontend). A `Modal.svelte` adopted by all six dialogs (scrim, header, named close button, trapFocus, z-index, Escape). A `SliderRow` (reserved ◇ slot, number field, neutral tick, bipolar fill, double-click reset, 18 px hit box, `min-width:0` so values don't clip, ◇ as a 24 px icon button). A `ToolbarToggle`, giving the timeline toolbar one "on" style, one height and 24 px targets. Findings: dialogs M31 and the structural note; panels H3, H4, M1, M3; timeline H14, M9.
- **UI-4 Timeline and preview layout** (frontend). Lanes, ruler and row borders fill the viewport; fit on open. Remove the 720 px preview cap and size the request to CSS width × DPR. A transport scrub bar with a 16 px hit area, a visible track and a pointer-captured drag that updates live. Ruler major and minor ticks, with beat and bar ticks moved off the label row. One timecode format everywhere, plus a timeline current-time readout; an empty cut is 00:00. Locked tracks get a hatch and a padlock. HUD legible in light and on bright frames; don't duplicate the transport timecode. Collapse the overview strip and the empty Titles lane when they carry nothing. Clip focus ring. Pin the name when the clip starts off-screen. Audio clip name contrast. Clipping drawn as a marker, not whole red columns. Findings: timeline H1, H2, H6–H10, M11, M12, M20 and the related Polish items.
- **UI-6 Copy and accessibility sweep** (frontend). Apply the dialogs.md copy deck: ellipsis on items that open a dialog, "Couldn't" style, verbs on buttons, explicit confirm labels (Save / Don't save / Cancel where the platform allows). aria-labels on ✕ buttons, `aria-current` on the settings rail, pressed state on chips, a `textarea:focus-visible` ring. A themed tooltip action replacing native `title=`. Notification log keeps `description`; toasts tinted by kind. Keyboard navigation in context menus. Findings: dialogs items 5–7, 9 and the copy deck; shell M3, M4 and the tooltip and hit-target items; timeline context-menu keyboard.
- **PERF-2 Scene detection on the proxy** (engine). When a proxy is ready, analysis reads it instead of the original (gpu-media WP3). Make sure proxy timing equals original timing ("a proxy answers `-ss T` with the frame the original does").
- **PERF-3 Playback** (engine + frontend). Play at the timeline frame rate instead of the fixed `PLAYBACK_FPS = 24`. Send playback JPEGs as raw bytes over the channel into `createImageBitmap` and a canvas instead of base64 data URLs; measure webview decode before and after. Findings: gpu-compositor WP-0(a)(c); gpu-media WP7. The transport cost is the webview's data-URL decode, not the base64 encode, and both reports agree once read that way.
- **GPU-1 Proxy colour normalisation** (engine + gpu). Tag proxies consistently so stacking an untagged and a BT.709 proxy is not refused as `MixedMatrices` (gpu-compositor WP-3), with parity cases. Decide the preview colour target (BT.709 vs BT.601) and log it.

### Wave 3

- **UI-5 Timeline gestures** (frontend). Snap indicator. Readout during a plain edge trim. Razor cut-line preview. Autoscroll while dragging, trimming and marquee-selecting. Diff markers on the timeline for a staged agent proposal (the `--diff-*` tokens). Draw higher video tracks above lower ones (V2 over V1) to match compositing; this is a convention change, so log it in Decisions. Clicking a clip should not move the playhead. Frame-quantized playhead. Findings: timeline H3–H5, H12, H13, H15 and the related Medium items.
- **UI-7 Panels polish** (frontend). Library tile overflow ("Compressor", `minmax(76px)`), one button scale, ragged Titles row. Agent panel review-card actions sticky at 1366x768, smaller composer, a proposal indicator on the Agent tab and title bar. Master fader height (don't stack Limiter, Ceiling and Duck above it). 360 override behind Advanced. An Export button in the title bar (shell H6). Remaining panels and shell Medium/Polish items.
- **UI-8 Export dialog logic** (frontend). Gate "Where it is going": judge only ticked platforms; no platform verdicts for MP3/WAV/GIF or an empty timeline; no letterbox warning on 1:1; no amber on a default export (dialogs item 3).
- **PERF-4 Still cache and prefetch** (engine). Cache composited stills and prefetch a window from one long-lived spawn (gpu-media WP6), plus a GPU resource class/lane in `cpu.rs` (WP9). Expose `Prefetch` through `Hint` so cuts don't stall (gpu-compositor).
- **GPU-2 Text on the GPU** (engine + gpu, L). Resolve a default font so titles and captions are no longer `TextWithoutFont`. Decide between bundling and resolving the platform font, and log it. FFmpeg rasterises `drawtext` coverage into a cached atlas and the GPU blends it. Parity cases for every text style. Report the coverage gain on the captioned talking-head archetype from gpu-compositor.md.

### Wave 4 (large; start only when waves 1–3 are merged)

- **GPU-4** Fades, dips, dissolves, slide and push on the GPU, with a Motion pixel-parity harness (gpu-compositor WP-4).
- **GPU-5** Scrub at GPU speed plus hardening: snapshot plan inputs, latest-wins frames, refuse CPU adapters by default, a runtime parity self-test (WP-5).
- **GPU-6** Keyed zoom, masks and blur on the GPU (WP-6).
- **GPU-7** GPU forward playback with a pull model where the webview keeps the clock, and prefetch at cuts (WP-7).
- **PERF-5** Optional AMF / VAAPI export encode (gpu-media WP4, WP11) with a recalibrated QP mapping. AMF families only in the golden file. Ship behind detection and leave defaults unchanged until VMAF on real footage is under Needs a real machine and confirmed. Do not hardware-encode proxies (WP5).
- **macOS presenter** (gpu-compositor WP-8): implement only what can be verified without a Mac; otherwise write the plan into Needs a real machine.

Skip for now: export through the compositor (WP-9), in-process libav decode (GPL licence issue with the BtbN build), and FFmpeg GPU filters (they failed or did not match on this driver).

## Finish

When every package is merged, blocked or deferred: make sure main is pulled, all worktrees are removed, and target is under 20 GB. Do not cut a release. Report to the user:
- merged PRs with their finding IDs;
- blocked items and why;
- the Decisions list;
- Needs a real machine;
- Needs a decision;
- before and after numbers for every PERF and GPU package;
- a pointer to the after/ screenshots.
