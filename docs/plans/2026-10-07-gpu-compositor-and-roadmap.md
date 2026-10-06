# GPU compositor + editor roadmap — orchestrator plan

Status: **approved direction, not started** (2026-10-07).
Owner: one long-running **orchestrator** session that manages subagents, reviews and merges, step by step, until every phase below is done.
Progress log: `docs/plans/2026-10-07-progress.md` (the orchestrator creates and maintains it — see §8).

This plan has two parts that run as one programme:

- **Part A — GPU compositor (wgpu).** Composite frames on the GPU in Rust, show them in a native surface in the Preview panel, and finally render export through the same compositor. The SvelteKit UI stays.
- **Part B — editor feature roadmap.** Workspaces + left library rail, waveforms, timeline editing, mixer, keyframes v2, on-canvas handles, colour grading + scopes, motion, and a backlog — distilled from analyses of freecut, OpenCut and LeGit (2026-10-06).

Read `CLAUDE.md` before anything else. It documents why the engine is the way it is; most of its rules exist because a bug already happened.

---

## 0. Why this, and what was decided

Kerf renders everything — playback, the scrubbed still, the agent's `preview_timeline`, the cover frame and the export — through **one FFmpeg filter graph** (`engine/cli.rs`: `build_filter_complex`, `push_inputs`, `stream_preview`, `timeline_frame`). That gives exact preview/export parity and a huge effect catalogue for free, but:

- a still costs ~50–150 ms; seeking re-spawns/re-seeks ffmpeg; drags and colour wheels can't get 16 ms feedback;
- much of FFmpeg's filtering is CPU, so multi-layer 4K / `v360` / `geq` masks can't play in real time;
- frames reach the webview as JPEG → base64 → JSON → IPC.

Options considered: (A) FFmpeg only, (B) WebGPU/WebGL in the webview, (C) a Rust wgpu compositor. Also considered and **rejected**: a full native UI rewrite (egui/iced/Slint/GPUI). The UI is not the bottleneck; a native rewrite would throw away the timeline, inspector, dockview layouts, theme system, transcript editor, agent review UI and the browser harness, and would still need everything in (C). OpenCut's switch to GPUI + wgpu is still "a window that opens" five months in.

**Decisions (binding unless the user changes them):**

| # | Decision |
|---|---|
| D1 | Build a Rust **wgpu** compositor (`crates/kerf-gpu`). Keep the SvelteKit/Tauri UI. |
| D2 | **FFmpeg stays the source of truth until parity is proven.** Every GPU path has an FFmpeg fallback; an effect not yet ported renders through FFmpeg. Export moves to the GPU only at A7, behind a gate. |
| D3 | Preview/export parity is enforced by an automated **parity harness** (same timeline, same time, both renderers, image metric with a threshold), run in CI on a software adapter. |
| D4 | FFmpeg remains the **decoder and encoder** (hardware decode/encode already exist). wgpu only composites. Audio stays FFmpeg (export) + Web Audio (preview). |
| D5 | Keyframes stay in **seconds**; gestures snap to frames (round once per gesture, derive every field from the snapped delta). |
| D6 | The left **library rail replaces** the Media/Transcript tab group (default; ask the user once at B1 kick-off, proceed with the default if no answer). |
| D7 | **Nested compositions / parenting / expressions are out of scope.** |
| D8 | Per-track audio **buses**: decided inside B4 (needed for track-level EQ/automation). Default: per-clip automation first, buses only if B4's design review says it's cheap. |
| D9 | During a drag the UI may show an **approximate** frame (GPU or CSS ghost) and must settle to the exact render on release. Once the GPU path covers the effect, the drag frame *is* exact. |
| D10 | Reuse the project agents in `.claude/agents/` (`engine`, `frontend`, `surface`, `reviewer`, `verifier`); add a `gpu` agent (A0). Every subagent: `model: "sonnet"`, told to work at **xhigh** effort. Never launch Fable subagents; never `subagent_type: "fork"`. |

---

## 1. Ground rules for the orchestrator

### 1.1 Work packages and branches
- Every row in §4 / §5 is a **work package (WP)** = one branch = one PR (split further if a PR would exceed ~1,500 changed lines; never merge a half-feature that breaks the app — gate unfinished UI behind a setting instead).
- One **git worktree per WP** off fresh `origin/main`: `git worktree add -b <branch> ../kerf-<wp> origin/main`. Never work in the user's checkout (`/home/orell/github/kerf`); other sessions may be using it.
- Branch names: `feat/gpu-<step>`, `feat/<feature>`, `fix/<thing>`.
- At most **three WPs in flight** at once, and never two that edit the same hot file (`Timeline.svelte`, `Preview.svelte`, `Inspector.svelte`, `engine/cli.rs` `build_filter_complex`, `model.rs` `Clip`) at the same time. Sequence those.
- After merging, remove the worktree and delete the local branch; `git fetch --prune`.

### 1.2 The loop per WP
1. **Design note** (≤1 page, in the PR description draft): scope, files, data-model changes (serde defaults!), FFmpeg mapping, MCP exposure, preview/export parity story, risks. For L-sized WPs, have a `reviewer` subagent critique the design before implementation.
2. **Implement** with the right agents, in this order when a feature spans layers: `engine` (kerf-core + tests) → `surface` (Tauri command + MCP tool + `api.ts` + browser harness + `types.ts`) → `frontend` (UI) → `gpu` (shader/pass + parity case). Launch independent halves in parallel only when they don't touch the same files.
3. **Verify**: `verifier` agent (prek commit + push stages; `--ignored` ffmpeg tests when the engine or graph changed; parity harness when anything visual changed).
4. **Review**: `reviewer` agent on `git diff origin/main...HEAD`. Fix every confirmed finding (send it back to the implementing agent), re-verify, re-review if the fix was non-trivial.
5. **Browser-harness check** for UI work (`bun run dev`, Playwright/headless Chrome screenshots), and for anything that touches the desktop window, a WSLg run (see the `kerf-gui-verification` memory: mouse only, XComposite capture). Report honestly what could not be verified.
6. **Docs**: update `CLAUDE.md` (the relevant section, concise) in the same PR.
7. **PR**: push, `gh pr create` with what/why/verification/known limits. Wait for CI (`gh pr checks --watch`). Red CI → fix on the branch, don't merge red. Merge with `gh pr merge --merge` (merge commits are the repo convention).
8. Log the outcome in the progress file (§8).

### 1.3 Hard rules (from the user and the repo)
- **Never mention Claude, Anthropic or AI** in commits, PRs, tags or merge messages; no `Co-Authored-By`, no session links. A PreToolUse hook enforces it and greps the *whole command* for "claude" — stage `CLAUDE.md` via a glob (`git add CLAUD*.md`). Commit signing (1Password) can fail transiently and leave the index staged — never chain two commits in one command; check `git status` after a failed commit.
- Commit style: lowercase imperative, short first line.
- Stage specific files, never `git add -A` / `git add .`.
- Never run `sudo`; ask the user to run it.
- `--no-default-features` everywhere in CI and local checks (no FFmpeg dev libs needed). Workspace lints forbid `println!`/`eprintln!` (use `tracing`; `#[allow(clippy::print_stderr)]` only in tests that need it). CI's clippy can be newer than local — read CI's clippy output, not just local.
- Keep `kerf-core` UI-agnostic; **no editing logic in `kerf-app`**. Every capability is exposed twice: Tauri command and MCP tool.
- Keep `kerf-core` serde structs ↔ `frontend/src/lib/types.ts` in sync; JSON is snake_case.
- Every new model field is `#[serde(default)]` and **omitted from the graph at its neutral value**, so old projects render byte-identical graphs (this is how every previous feature kept the existing tests green).
- Filter-graph expressions containing commas **must be quoted**; free-form strings are validated/escaped (see CLAUDE.md "Any such expression must be quoted").
- Versions: check the crates.io sparse index (`curl -s https://index.crates.io/<2>/<2>/<name> | tail -1`) / npm before adding or bumping.
- Licences: freecut and OpenCut are MIT — ideas freely, code only with the MIT notice kept (add a `THIRD-PARTY-NOTICES` entry). **Do not copy** freecut's SoundTouch time-stretch (LGPL-family), its halftone shader, or the easings.dev preset catalogue. Kerf is PolyForm Noncommercial; don't add other licence headers.

### 1.3a Running in a cloud environment
- The user's global `~/.claude/CLAUDE.md` and its commit hook are **not present** in a cloud session. The project's `.claude/settings.json` turns attribution off, but the rules in §1.3 still apply verbatim — check every commit message and PR body yourself.
- Commits are not SSH-signed there (the 1Password signer is local-only); that is expected.
- There is no GPU: wgpu runs on Mesa **lavapipe** (installed by the setup script); `KERF_HWACCEL=none` / `KERF_HW_ENCODE=none` keep ffmpeg in software.
- There is no display: GUI checks go through the browser harness (`bun run dev` + headless Chrome/Playwright). Desktop-window work (A2) needs the user to run a PR build on Windows — ask.
- Worktree paths in this plan are relative (`../kerf-<wp>`); use the cloud checkout's parent directory.

### 1.4 When to stop and ask the user
- A go/no-go gate in Part A (A0, A2, A7).
- A decision in §7 that the WP can't proceed without.
- Anything destructive or outward-facing beyond pushing branches / merging green PRs (releases, tags, force-pushes, deleting remote branches other than merged PR branches, changing repo settings or secrets).
- Three consecutive failed attempts at the same CI failure.

---

## 2. Current state at hand-off (2026-10-07)

Merged to `main` this week: voiceover (Kokoro TTS, #83), dependency bumps (#75–#82), phone/mirrorless footage fixes (#80), CI speedups (#81), bun-audit fix (#82). Being merged as part of the hand-off (check `gh pr list --state all` for the final state): theme line/slider tokens + fixed-size settings dialog (#84), error logging to `<app data dir>/logs` (#85), titles lane + preview move/resize handles (#86), and `feat/hardening` (LeGit learnings: atomic/patch settings, single instance + dev identity, CSP + file-access lockdown, command-name drift test, envPrefix guard, release backtraces, startup flash, crash-safe log tail, theme guard tests). Hardening landed six of nine items (atomic/patch settings with corrupt-aside, single instance + `ch.orellbuehler.kerf.dev` debug identity via `build.rs`/`tauri.dev.conf.json`, MCP port-taken error in `agent_status`, CSP + `.json`-only text-file commands, command-name drift test, envPrefix guard, `strip = "debuginfo"` + backtrace in the panic hook). **Not done, and part of B9:** (1) startup flash — start hidden, show after the theme is applied, Rust failsafe; (2) crash-safe log tail — the non-blocking appender can lose the last lines on a hard crash; (3) theme guard tests — colour-literal scan + WCAG contrast pairs per preset; (4) a `.kerf` path passed on the *first* launch is still ignored (only a second launch forwards it).

Unverified items that the first WPs should confirm (cheap, do them early):
- The titles lane / preview handles, theme tokens and logging were verified only in the browser harness — run them in the desktop app (WSLg) once.
- `extract_audio` may double the audio (it adds the asset's audio track without muting the clip) — verify with an export; fix inside B9 "detach audio".
- CSP in a **packaged** build (`tauri build`): check the console for violations.

---

## 3. Agents

| Agent | Use for | Notes |
|---|---|---|
| `engine` | kerf-core: model, timeline math, project ops, ffmpeg graph | Pure logic gets unit tests. |
| `surface` | Tauri command + MCP tool + `api.ts` (+ harness fallback) + `types.ts` | No logic in the adapter. MCP tools that mutate go through `edit()`. |
| `frontend` | Svelte UI, runes singletons, bun-tested TS mirrors | Tokens only, no colour literals; pointer capture + cancel on every drag. |
| `gpu` (**new, created in A0**) | `crates/kerf-gpu`: wgpu device/surfaces, WGSL passes, texture upload, the parity harness | Copy the frontmatter shape of `engine.md`; body: read §A of this plan + CLAUDE.md engine section; always add a parity case for every visual change. |
| `reviewer` | read-only diff review against Kerf's invariants | Run on every WP before PR. For GPU WPs also ask it to check: colour-space conversions, texture lifetimes, sync/backpressure, fallbacks. |
| `verifier` | runs prek stages, `--ignored` ffmpeg tests, parity harness | Read-only; diagnoses, never fixes. |

Prompt every subagent with: the WP's design note, the exact worktree path, "never touch other worktrees or the user's checkout", the hard rules from §1.3 that apply, and what to report (files, decisions, real verification output, what's unverified).

---

## 4. Part A — GPU compositor

### A-overview: target architecture

```
            Timeline (EDL, kerf-core)                     FFmpeg (decode, encode, audio)
                     │                                                │
          RenderPlan::at(t) / ::span(a,b)  ◄── shared "what is visible, where, how" ──►  build_filter_complex
                     │                                                                (fallback + export until A7)
         ┌───────────┴────────────┐
   FrameSource (per asset)        Compositor (kerf-gpu, wgpu)
   ffmpeg decode → NV12/RGBA      passes: yuv→rgb, transform/crop, opacity,
   frame cache, seek, prefetch    masks, effects, transitions, text, v360, grade
         └───────────┬────────────┘
                     ▼
      ┌──────────────┼───────────────────────────────┐
  native surface   headless texture → JPEG       headless texture → raw frames
  (Preview panel)  (MCP preview_timeline/get_frame)   → ffmpeg encoder (export, A7)
```

Key design rules:
- **`RenderPlan`** (kerf-core, pure, unit-tested) is extracted from what `build_filter_complex` already computes: for a time `t`, the ordered list of visible layers with resolved source time, transform/crop/opacity (keyframes sampled), mask, effects, transition state, overlays, and the canvas (`export_format`). The FFmpeg builder keeps working on the timeline; the GPU consumes the plan. A unit test asserts the plan and the FFmpeg graph agree on layer order / timing for the existing graph fixtures.
- **Colour pipeline is explicit**: decode to NV12/YUV420P (8-bit) or P010 (10-bit), convert in a shader with the stream's matrix (BT.601/709/2020) and range (limited/full) from `StreamInfo`; composite in **linear-light or gamma space — pick one in A0 and match what FFmpeg does** (FFmpeg's `overlay`/`scale` work in the encoded gamma space; matching that is what makes parity possible); output sRGB/BT.709. HDR (HLG/PQ) tone-mapping is A5 work; until then HDR clips fall back to FFmpeg.
- **Fallback is per frame**: if the plan for `t` contains anything the GPU can't render yet (`RenderPlan::gpu_supported()`), that frame goes through the FFmpeg path. The UI shows which renderer produced the frame in the status bar (dev aid; a setting can hide it later).
- **Locks**: never hold the `Project` mutex while decoding or rendering — resolve the plan under the lock, release, render (the existing lock-free `decode_*` pattern).
- **CPU budget**: decoding through ffmpeg still goes through `engine/cpu.rs`. Ungated (moment reads) vs gated (whole-file) rules apply unchanged.
- **CI has no GPU**: use a software adapter — Mesa **lavapipe/llvmpipe** on Linux (`mesa-vulkan-drivers`), **WARP** on Windows (DX12), Metal on macOS runners. `wgpu::Instance` with `force_fallback_adapter` for tests. Parity thresholds must hold on the software adapter.

### A0 — Feasibility spike (M) — **gate: user go/no-go**
- New crate `crates/kerf-gpu` (workspace member, `license.workspace = true`), deps: `wgpu` (currently 30.x; check MSRV against the workspace `rust-version` 1.95), `pollster`, `bytemuck`. Not yet linked into kerf-app.
- Headless device (no window). `FrameSource` v0: decode one frame of an asset at a source time via the existing ffmpeg binary path (rawvideo NV12 over a pipe, from the **proxy** when one is ready), upload, YUV→RGB shader.
- Compositor v0: black canvas at `export_format`, layers in track order with `Transform` (scale/position/rotation/crop) + opacity + `Fit::Contain/Cover`, plus `eq` colour (brightness/contrast/saturation/gamma/temperature — port `eq_filter`'s maths).
- `RenderPlan` v0 in kerf-core for exactly these features.
- **Parity harness** (`crates/kerf-gpu/tests/parity.rs`, `#[ignore]` + a CI job): generate synthetic media with ffmpeg (`testsrc2`, `smptehdbars`, a gradient, a still), build timelines (single clip, two overlapping tracks, scaled/rotated/cropped clip, opacity 0.5, colour adjustments, contain vs cover into 9:16), render each at several times through `timeline_frame` (FFmpeg) and the GPU, compare with **PSNR ≥ 40 dB and max per-channel error ≤ 8/255 outside a 2-px edge band** (tune in A0 and record the final thresholds in the test). Write diff images to `target/parity/` on failure.
- Benchmarks: time per still (GPU vs FFmpeg), 1080p and 4K canvas, 1/3/6 layers, software adapter and a real GPU if the machine has one.
- Deliverable: PR with the crate, the plan type, the harness, numbers in the PR description, and `.claude/agents/gpu.md`. **Stop and report numbers to the user** before A1.

### A1 — Frame source + render plan, production quality (M)
- `FrameSource`: per-asset decoder that streams frames forward (one long-lived ffmpeg per active asset, like `stream_preview`), seeks by restarting at the nearest keyframe (proxies are all-intra, so seeks are cheap), LRU frame cache keyed by (asset, source frame index) with a memory cap, prefetch around the playhead. Stills (`StreamInfo.image`) decode once. Hardware decode via the existing `-hwaccel` path; frames come back through system memory (zero-copy interop is out of scope; note it as future work).
- 10-bit sources: P010 upload path or 16-bit RGBA; HDR → mark unsupported for now.
- `RenderPlan` complete for everything the compositor will eventually need (layers, keyframes sampled, transitions with both sides and progress, overlays, masks, effects list, reframe), with `gpu_supported()` reporting what A0/A5 can do.
- Tests: plan fixtures mirroring the existing `build_filter_complex` tests (positions, gaps, track order, speed/reverse, slice/for_render, for_delivery framings).

### A2 — Native preview surface in the Preview panel (M–L) — **gate: works on Windows, macOS, Linux**
- Present a wgpu surface **in the Preview panel's rectangle** of the Tauri window. Two candidate techniques; spike both briefly and pick per platform:
  1. wgpu renders to the main window's surface *under* a transparent webview; the Preview panel's frame area is transparent (`background: transparent` + Tauri `transparent: true` window). Works on Windows (WebView2) and macOS (WKWebView); on Linux (WebKitGTK) transparency + GL/Vulkan surfaces are fragile.
  2. A borderless **child window** (Tauri v2 multiwebview/`WindowBuilder` with a parent, or a raw platform child via `raw-window-handle`) kept aligned to the panel's bounds (the frontend reports bounds on resize/scroll/dock moves via a command; DPI-aware).
- The frontend keeps owning everything else in the panel (overlays, safe areas, title handles, timecode); those are drawn by the webview over/around the surface, so technique 1 is preferred where it works.
- Fallback: if no surface can be created (or a setting is off, or the platform is unsupported), the Preview panel keeps today's JPEG path. Setting: *Settings › Preview › GPU preview (experimental)*, default **off** until A4.
- Verify on all three OSes (CI can't — the user runs a build from the PR artifacts; ask them). **Gate: user confirms on Windows (their main machine) before A3.**

### A3 — Scrub + live drags on the GPU (M)
- Scrubbing, the settled frame and shuttle use the GPU path when `gpu_supported()`; otherwise FFmpeg as today.
- Live drags render through the GPU per pointer-move (title handles from #86, transform handles from B6 when it lands): the drag frame becomes exact for supported features (D9).
- Commit on pointer-up still writes one revision.
- Measure input-to-photon latency on scrub (target < 33 ms at 1080p on a real GPU).

### A4 — Playback (L)
- A render loop paced to the timeline fps against the **audio clock** (`audio.ts` Web Audio is the master clock today; expose its current time to Rust via a command or keep the clock in Rust and drive Web Audio from it — decide in the design note).
- Frame drop policy (never let video run behind audio; skip, don't slow down), prefetch across cuts, proxy usage as today.
- Playback falls back to `stream_preview` for spans containing unsupported features (pre-scan the span with `RenderPlan`).
- Turn the GPU preview setting **on by default** when A4 passes on the user's machine.

### A5 — Effect parity, one effect at a time (L, many small PRs)
Each effect = its own PR: WGSL pass + `gpu_supported()` update + parity cases + benchmark line. Order by usage:
1. Transitions: dips, crossfade, slide/push (all four directions) — handle borrowing as in the FFmpeg graph.
2. Masks (rectangle/ellipse, feather, invert) — replaces the slow `geq`.
3. Text overlays: `glyphon`/`cosmic-text` (check compatibility with the chosen wgpu version), font set matching what `drawtext` uses (bundle the default font so both renderers use the same file), box, keyframed x/y/alpha, the `fit_size` logic shared.
4. Video effects: blur (separable Gaussian, matched to `gblur` sigma), sharpen (`unsharp`), hue, negate, vignette, chroma key (match `chromakey` similarity/blend).
5. `v360` reprojection (equirect / dual-fisheye → rectilinear, yaw/pitch/roll/d_fov, keyframed) — a single fragment shader; compare against `v360` with the same interpolation.
6. HDR: HLG/PQ → SDR tone mapping matching the proxy's `zscale/tonemap` chain.
7. Anything left in `VideoEffect`.

The parity harness is the acceptance test for every item; thresholds may be per-effect (record them) but never silently loosened.

### A6 — Headless rendering for the agent (S–M)
- `preview_timeline`, `get_frame` (region zoom), `export_cover`, `skim_asset` (optional) render through the GPU when an adapter exists and the plan is supported; FFmpeg otherwise. Same output contract (JPEG bytes, region semantics). The MCP server must keep working on a machine with no usable GPU.

### A7 — Export through the compositor (L) — **gate: user go/no-go**
- GPU renders each output frame → readback (double-buffered, async) → raw frames piped into an ffmpeg **encode-only** process (existing encoders incl. verified HW encoders, `-progress`, cancel). Audio keeps the existing FFmpeg audio graph, muxed in the same process (`-f rawvideo -i pipe:0` + the audio inputs) or a second pass.
- Range export, multi-format variants (`render_variants`, `for_delivery`), cover frames, `loudnorm` keep working.
- Parity: render a set of reference projects both ways; frame-sampled PSNR + duration/AV-sync checks.
- Keep the FFmpeg export selectable (*Export › Advanced › Renderer: GPU / FFmpeg*) for at least one release; GPU becomes default only after the user approves.
- After A7, **new visual features are implemented once, on the GPU**; until then they are implemented in both renderers with a parity case (this is why B6/B7 should, where possible, land after A5's relevant passes).

### Part A risks to watch
- WebKitGTK surface/transparency on Linux (A2) — acceptable outcome: Linux keeps the JPEG path for preview but still gets GPU headless/export.
- Colour mismatch (matrix/range/gamma) — the parity harness catches it; fix in the conversion pass, not with per-effect fudge.
- Driver variance — test on the software adapter (CI) and the user's GPU; don't depend on optional wgpu features without a fallback.
- Memory: 4K RGBA frames are 33 MB; cap caches, reuse textures, pool readback buffers.
- Binary size / MSRV from wgpu — check CI's `msrv` job and bundle sizes in A0.

---

## 5. Part B — feature roadmap

Run B1 in parallel with A0–A2 (UI-only, no conflict). From B2 on, sequence against Part A so hot files are not edited concurrently. Where a feature is visual, check §A7's rule: before A7 it needs both renderers (FFmpeg graph + GPU pass + parity case), after A7 only the GPU.

| WP | Content | Size | Depends on |
|---|---|---|---|
| **B1 Workspaces + left library rail** | (a) Workspace presets **Edit / Color / Audio / Motion / Deliver** as tabs (TitleBar or Toolbar): each a full dockview `SerializedDockview` in `layout.ts`; per-workspace saved layouts in `Settings.layouts` (patch-style write, sanitized like `sanitizeLayout`); switching never touches project state, selection or playhead; *Reset workspace*. (b) A `library` panel with an icon **rail** on its left (36 px icons, tooltips, click active icon to collapse, collapsed state persisted): Media, Titles, Effects (looks + video effects), Transitions, Audio (audio effects, voiceover), Transcript — replaces the Media/Transcript tab group (D6). (c) *Deliver* workspace docks the export dialog's readiness/variants content. New panel ids get registry entries + minimum sizes. | M | — |
| **B2 Waveforms + clip overlays + frame snapping** | Engine: `waveform_pyramid` (one decode → min/max peaks at ~500/100/25/10 per second, stereo when present, cached at `<cache>/kerf/waveforms/<hash>.bin`, ungated + niced, bounded memory) + `get_waveform_range(asset, start, end, buckets)` (Tauri + MCP), keeping `get_waveform`/`get_energy`. Frontend: one canvas per visible clip window, level chosen from zoom, DPR ≤ 2, min/max filled shape, stereo lanes on tall tracks, scaled by clip volume × track fader, clipped samples coloured, fades shaded. Overlays: draggable **volume line** and **fade handles** on clips, **keyframe diamonds**, trim halos — in a child component (`ClipOverlays.svelte`), not more lines in `Timeline.svelte`. Frame-quantized trim/move/split (D5). | M | #86 merged |
| **B3 Timeline editing** | **Ripple mode** toggle (global; pure `Timeline::ripple_from(before)` diff-based, matched by id like `Timeline::diff`, applied by `edit_timeline` when the flag is set; exposed to MCP as a parameter), **filmstrip thumbnails** (ffmpeg `fps=1/N,scale,tile` from the proxy, cached beside proxies, windowed canvas), per-track **height** presets (compact/medium/large; titles lane follows), zoom-to-fit + ctrl-wheel zoom + wider zoom range, **marquee multi-select** + multi-move, **minimap**. | M | B2 |
| **B4 Mixer** | Dockable **Mixer** panel (strips: dB fader with log mapping, pan, M/S/Duck, meter; **Master** strip). Model: `Timeline.master {volume, limiter}` (serde default, omitted at neutral → byte-identical graphs) appended before `loudnorm`. Preview: per-track bus `GainNode` → stereo split → two `AnalyserNode`s → master gain + analyser (real measured meters). MCP: `set_master_volume`, `get_levels(range)` via `astats`/`ebur128`. Decide D8 here. Preview ducking: approximate or label as export-only. | M | B1 (panel slot) |
| **B5 Keyframes v2** | `easing` per key (Linear, Hold, EaseIn/Out/InOut, Bezier{x1,y1,x2,y2}; serde default Linear → byte-identical), realised in `keyframe_expr` by sampling bezier segments into 8–12 linear sub-segments (hold = step); `Timeline::slice` resamples eased curves and inserts boundary keys. Then **per-property channels** (`Vec<PropertyTrack{prop, keys}>` with migration from the whole-transform `Keyframe`), making colour, volume (`volume=eval=frame` after `atempo`, `asetnsamples` to limit zipper noise), crop and mask params animatable. **Dope sheet** panel (rows per property, diamonds on the shared time axis, marquee, drag-retime with snapping, copy/paste, Alt-duplicate) + **segment easing popover** with ~12 own presets (not easings.dev's). Graph editor with bezier handles is a later WP (B5b). GPU: the compositor samples the same curves (shared pure Rust function — one implementation for both renderers). MCP: `set_keyframe_easing`, `set_property_keyframes`, `copy_keyframes`. | L | B1; ideally A1 (plan samples keyframes) |
| **B6 On-canvas transform handles** | Move/scale/rotate/crop clips in the Preview with snap lines (centre, edges, safe areas, other layers; 90° rotation snap within 5°), separate X/Y scale, one revision per gesture, pointer capture + cancel. Reuse the titles-handle machinery from #86. Live via A3's GPU drag path; before A3, CSS ghost + exact frame on release (D9). | M | A3 preferred |
| **B7 Colour grade + scopes + Color workspace** | Model: `Color.wheels {lift, gamma, gain, offset}` (FFmpeg: `colorbalance` or exact `lutrgb` CDL — note `lutrgb` is 8-bit; for 10-bit prefer GPU or a cached `lut3d`), `Color.curves {master,r,g,b}` (FFmpeg `curves` with pchip), `VideoEffect::Lut3d {path, intensity}` (copy `.cube` into the project cache, validate the path), eyedropper white balance + auto balance (`frame_jpeg` sample). UI: `ColorWheel.svelte` (puck drag, double-click reset, luma slider, keyboard nudge), SVG curve editor, LUT picker, grade presets. **Scopes** panel (waveform, RGB parade, vectorscope with skin-tone line, histogram): GUI computes from the GPU frame (A3+) or the preview JPEG before that; MCP `scope_frame` via ffmpeg `waveform`/`vectorscope`/`histogram`. Color workspace preset (big preview, scopes, wheels dock, short timeline). MCP: `set_color_grade`, `set_curves`, `apply_lut`, `scope_frame`. Qualifiers/power windows deferred (mask + duplicated clip covers region grades). | L | B1, B5 (animatable colour), A3+A5 for live wheels |
| **B8 Motion** | **Motion presets** (entrance/exit/emphasis as pure data expanding into eased per-property keys; Replace vs Add; MCP `apply_motion_preset`), **live behaviours** (`Clip.behaviors`: sway/breath/spin/drift/shake as closed-form expressions of `t` — exact in both renderers; "bake" samples to keys), **motion path** overlay in the Preview (sampled position channel with key handles; straight/eased paths first). Motion workspace preset. | M–L | B5, B6 |
| **B9 Backlog** (one PR each, any order, S–M) | • **Edit modes**: roll, slip, slide (kerf-core first, then UI with live preview, then MCP). • **Linked A/V + detach audio** (`Clip.link_id`, honoured by move/trim/razor; `Clip.source_audio` toggle that moves the clip's own span to an audio track and mutes the source — and fix/verify `extract_audio` doubling). • **Blurred / coloured background** for Contain fit (`Fit::Blur` or `Delivery.background`; FFmpeg `split → scale cover + boxblur (downscaled first) → overlay`; GPU pass). • **SRT/ASS caption import** into overlays via `Timeline::captions`' chunk/fit logic. • **Marker notes + durations**. • **Customizable keybindings** (versioned defaults with migration). • **Split-and-remove left/right**. • **Preview master limiter**. • **Timeline virtualisation** (cull off-screen clips; canvas per track above ~80 clips) when long timelines hurt. • **Graph editor** for keyframes (B5b). • Remaining **hardening** items (§2): hidden-until-themed window, crash-safe log tail, theme literal/contrast guard tests, open a `.kerf` passed on first launch; and verify the CSP in packaged Windows/macOS builds. | S–M each | — |

Every B-WP also: updates the MCP server `instructions` when agent workflow changes (e.g. "ripple mode exists", "use easing", "check scopes"), adds MCP tools via `surface`, and keeps the browser harness working (`api.ts` fallbacks) so the UI stays drivable under `bun run dev`.

---

## 6. Verification matrix

| Change touches | Must run |
|---|---|
| any Rust | `cargo fmt --all --check`, `cargo clippy --workspace --no-default-features --all-targets -- -D warnings`, `cargo test --workspace --no-default-features` |
| ffmpeg graph / engine | `cargo test -p kerf-core --no-default-features -- --ignored` (real ffmpeg) |
| kerf-gpu / anything visual after A0 | parity harness (software adapter) + benchmark line in the PR |
| frontend | `cd frontend && bun run check && bun run test && bun run build` |
| UI behaviour | browser harness (Playwright/headless Chrome screenshots), plus WSLg desktop run for window/surface work |
| everything | `prek run --all-files` (commit + push stages) via `verifier` |
| A2/A4/A7 gates | the user runs the PR's build on Windows (their main machine) |

CI changes to make along the way: a `parity` job (Linux lavapipe; add Windows WARP if stable), keep `msrv`, keep `--no-default-features`.

---

## 7. Open questions (ask the user when the WP needs them; otherwise use the default)

1. B1: rail **replaces** Media/Transcript tabs (default yes, D6).
2. B4: per-track audio buses now or later (default: later, D8).
3. B7: scopes in the GUI and MCP, or MCP only (default: both).
4. B2: stereo waveform lanes (default: yes on tall tracks, mono otherwise).
5. A2: acceptable for Linux to keep the JPEG preview if WebKitGTK surfaces are unreliable (default: yes, report it).
6. A7: when to make GPU export the default (user decides after A7 parity numbers).

---

## 8. Progress log and resuming

The orchestrator keeps `docs/plans/2026-10-07-progress.md` (committed with each merged WP, via the WP's own PR) with one row per WP:

```
| WP | branch | PR | status (todo/in-progress/review/merged/blocked) | notes (gate results, thresholds, numbers, follow-ups) |
```

On (re)start: read this plan, the progress log, `gh pr list --state open`, `git worktree list`, and the memory index; resume the first non-merged WP in this order:

**A0 → (B1 ∥ A1) → A2 → A3 → B2 → B3 → B6 → A4 → B4 → B5 → A5 (effects, interleaved) → B7 → A6 → B8 → A7 → B9 items**

Rationale: A0 de-risks the whole programme before anything depends on it; B1 is the most visible UI change and is independent; the timeline WPs (B2, B3) come after #86 and before the heavier model changes; B6 lands once drags can be GPU-live (A3); B5 precedes B7/B8 because colour/motion animation need per-property keyframes; B7 waits for the GPU colour pass so wheels are live; A7 waits until the effect catalogue is ported so export doesn't lose features.

A WP is **done** only when: merged to `main` with green CI, reviewer findings resolved, CLAUDE.md updated, progress log updated, worktree removed.
