# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Kerf is

A cross-platform desktop app for AI-assisted, **non-destructive** video/audio editing.
A Cargo workspace of three Rust crates + a Tauri-embedded SvelteKit frontend. The
distinguishing feature: an **MCP server embedded in the app** (streamable HTTP,
`127.0.0.1:7777/mcp`) lets an LLM analyze media and assemble edits through the same
engine the GUI uses. Nothing is re-encoded until export.

- `kerf-core` — the UI-agnostic engine: domain model + timeline math, `Project`
  (SQLite), analysis, and the FFmpeg engine.
- `kerf-app` — the only binary. A **thin adapter** exposing the `Project` API twice:
  Tauri commands to the webview and MCP tools to an agent, over one shared
  `Arc<Mutex<Project>>`. No editing logic here.
- `kerf-gpu` — wgpu compositor; `kerf-app` links it for the opt-in GPU preview surface
  (A2, off by default). Export and forward playback stay FFmpeg's.
- `frontend/` — SvelteKit 2 / Svelte 5 runes, Tailwind 4, Bun.

## Detailed docs — read the relevant one before touching a subsystem

Most rules in these files exist because a bug already happened. They record *why*.

| File | Covers |
| --- | --- |
| `.claude/docs/engine-cli.md` | `engine/cli.rs`: probe, proxies (head pad, sidecars), HW encode/decode, `cpu.rs` budget, export graph, master bus, levels, masks, keyframe expressions, keyed zoom order, 360/v360, Insta360 stitch, stills, playback stream, rotation, HDR tone-map, per-property channel graph (keyed colour / volume) |
| `.claude/docs/engine-media.md` | `peaks.rs` waveforms, `filmstrip.rs`, libav backend, whisper transcription, Kokoro TTS voiceover, optional cargo features |
| `.claude/docs/core-model.md` | `model.rs`: clips, easing, transitions, captions, caption import, beats, diff, ripple, edit modes, per-property keyframe channels |
| `.claude/docs/core-links.md` | Linked A/V: detach/reattach, carried edits, sync lock, `run_edit`, the links corpus |
| `.claude/docs/core-render-plan.md` | `platform.rs`, `render_plan.rs` / `planner.rs` / `plan_caps.rs`, frame picks, GPU refusal rules, colour / partly-keyed transforms in the plan |
| `.claude/docs/core-golden.md` | The golden argv oracle and how to bless it |
| `.claude/docs/core-project.md` | `project.rs` persistence, ripple flag, smart crop, multi-format delivery, task queue, staged agent edits, `analysis.rs` (analysis steps, per-kind status), `proxy.rs` (proxy status / queue / settings) |
| `.claude/docs/gpu.md` | kerf-gpu: frame cache, y4m/showinfo parsing, router, `FrameSource`, `FrameCursor`, presenting a frame (A2), parity harness findings |
| `.claude/docs/mcp.md` | The embedded MCP server and its tools |
| `.claude/docs/app.md` | kerf-app shell: CSP, single instance, launch args, window reveal, commands list, settings, logging, the GPU preview surface (`gpu_preview.rs`) |
| `.claude/docs/build-and-ci.md` | Debug identity, prek checks, CI, auto-update, release workflow, PR builds |
| `.claude/docs/frontend.md` | Theming, design system, title-bar menu bar, dockable workspaces (stored layouts, offered panels, reset), library, inspector, titles, mixer, export dialog, agent panel, notifications, keymap, settings, updater, GPU preview in the page, browser harness |
| `.claude/docs/frontend-timeline.md` | The timeline: gestures, frame quantizing, ripple/selection/group moves/zoom, roll/slip/slide, linked A/V UI, waveforms, filmstrips, track heights, minimap, clip overlays |

Project subagents in `.claude/agents/`: `engine`, `frontend`, `surface` (wire a core op
into Tauri command + MCP tool + api.ts), `gpu`, and read-only `reviewer` / `verifier`.

## The `ffmpeg` feature — read this first

`ffmpeg-next` links the system FFmpeg dev libraries, which are **not always installed**.
Every crate has a default-on `ffmpeg` feature that forwards to `kerf-core/ffmpeg`;
`kerf-core` is declared `default-features = false` in the workspace, so
`--no-default-features` really disables it everywhere. The **CLI engine
(`engine/cli.rs`) is always compiled** and drives the `ffmpeg` / `ffprobe` binaries
(override with `KERF_FFMPEG` / `KERF_FFPROBE`) — probe, analysis, previews, waveforms
and export all work without the dev libs. Only the in-process libav probe (and the
experimental `libav-render`) needs them. Transcription (FFmpeg's `whisper` filter or the
`whisper` feature) and voiceover (ONNX Runtime, loaded dynamically) work in every build.

- **With FFmpeg dev libs**: `cargo build` / `cargo run -p kerf-app`.
- **Without them** (CI, UI work): pass `--no-default-features`.

## Common commands

```bash
# Rust — verify / test without FFmpeg dev libs (works everywhere)
cargo check --workspace --no-default-features
cargo test  -p kerf-core --no-default-features
cargo test  -p kerf-core --no-default-features split_and_remove_roundtrip   # single test

# Tests that drive the real ffmpeg binary or download a model are #[ignore]d —
# run them when touching the engine or the export graph:
cargo test -p kerf-core --no-default-features -- --ignored
# GPU compositor vs FFmpeg (needs ffmpeg + an adapter; Mesa lavapipe is enough):
cargo test -p kerf-gpu --no-default-features -- --ignored

# Re-bless the golden argv oracle after an *intended* graph change (git diff is the guard)
KERF_GOLDEN_BLESS=1 cargo test -p kerf-core --no-default-features golden -- --nocapture
# Regenerate the linked-A/V corpus the frontend replays
KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- links_corpus

# Everything a commit / a push checks
prek run --all-files
prek run --all-files --hook-stage pre-push

# Frontend (Bun) — from frontend/
bun install
bun run dev      # http://localhost:1420; seeded in-memory harness outside Tauri
bun run build    # static SPA -> frontend/build (consumed by Tauri)
bun run check    # svelte-check (fails on warnings in prek)
bun run test     # bun test over src/**/*.test.ts

# Desktop app — Tauri config is NOT at the default path
bunx @tauri-apps/cli@2 dev   --config crates/kerf-app/tauri.conf.json
bunx @tauri-apps/cli@2 build --config crates/kerf-app/tauri.conf.json
cargo run -p kerf-app        # also works; runs the frontend dev command first

# MCP: run the app, then
#   claude mcp add --transport http kerf http://127.0.0.1:7777/mcp   (KERF_MCP_ADDR overrides)
```

Debug builds use their own identifier (`ch.orellbuehler.kerf.dev`), so a dev run does not
touch an installed Kerf's settings — see `.claude/docs/build-and-ci.md`.

## Invariants that apply everywhere

- **Engine first, then surfaces.** New capability lands in `kerf-core`, then as a Tauri
  command (`lib.rs`), an MCP tool (`mcp.rs`), the `api.ts` bridge + its browser-harness
  fallback, and `types.ts`. Serde structs ↔ `types.ts` stay in sync; JSON is snake_case.
- **Never hold the project lock across media work.** Resolve inputs under the lock,
  release it, run ffmpeg / decode / inference, re-lock to apply (`*_inputs` → static
  `decode_*` / `sample_*` → `apply_*`). No Tauri command runs on the main thread: quick
  ones are `#[tauri::command(async)]`, heavy ones go through `blocking()`.
- **Agent edits stage.** Edits with `EditSource::Agent` go into the staged proposal;
  every read an edit depends on goes through `working_timeline()`. Mutating MCP tools use
  the `edit()` helper, which emits `project-changed` only *after* releasing the lock.
- **All timeline edits go through `edit_timeline` / `run_edit`**, which apply ripple and
  the linked-A/V sync lock; layout-deciding ops use `edit_timeline_exact`. Timeline math
  lives in `model.rs` (and `model/links.rs`), pure and unit-tested.
- **Existing graphs stay byte-identical.** A new option is omitted from the argv at its
  neutral value. The golden oracle (`engine/cli/golden.rs`) fails on any argv change;
  bless only intended changes, one at a time, and tie the moved cases to a family.
- **Filter-graph safety.** Any expression containing commas must be quoted in the filter
  value; colours go through `valid_color`, enum-ish strings are allow-listed, `drawtext`
  escapes `%` and strips control chars. An expression may nest ~100 levels — anything
  generated per input item must be a balanced tree, not a chain. Unit tests on the graph
  *string* are not enough: render it (`#[ignore]`d tests) when the shape changes.
- **Machine budget.** Whole-file background jobs take `cpu::lease`; what the UI draws
  from or an agent looks at is ungated but thread-capped and niced. Thread flags are
  added at spawn time (`cpu::limit_args`), never in the pure builders. The queue has three lanes —
  **foreground** (export, levels, voiceover, stitch: what a user waits on) before **proxy** before
  **background** (analysis): a proxy goes before analysis and after foreground jobs, reserving its
  place when queued (`cpu::reserve`). A job that waits for the slot says so and polls its cancel.
- **Nothing waits on a child process forever** — every ffmpeg / ffprobe read has a
  timeout and a kill, and nothing is spawned or waited on under a lock.
- **Export reads originals; preview reads proxies.** A proxy must answer `-ss T` with the
  frame the original does.
- **TS mirrors are faithful.** `ripple.ts`, `multi-edit.ts`, `edit-modes.ts`,
  `link-ops.ts`, `captions.ts`, `easing.ts`, `mixer.ts`, `levels.ts`, … replay the Rust
  tests; change a rule in kerf-core and its mirror together.
- **GPU path refuses rather than approximates**: anything not drawn exactly is
  `Unsupported` and falls back to FFmpeg. Every visual change gets a parity case.
- **Frontend**: no colour literals outside `theme.ts` / `kerf-tokens.css` (a bun test
  scans); no hand-written shortcut labels (use the `keymap.ts` registry); gestures write
  **one** edit on release and Escape / pointercancel / blur abandon them.
- **Docs**: when you change a subsystem, update its `.claude/docs/*.md` file in the same PR,
  concisely. Add new commands/tools to the lists in `app.md` / `mcp.md`.

## Conventions

- License is **PolyForm Noncommercial 1.0.0** (public repo). New crates inherit it via
  `license.workspace = true`; don't add license headers.
- Versions were pinned against the crates.io sparse index / npm; check there (not the
  blocked crates.io JSON API) before bumping.
- Rust lints are `[workspace.lints]` (no `dbg!` / `todo!` / `println!`); every crate opts
  in with `[lints] workspace = true`.
- **Push a PR branch only when it is ready to test.** Every push to a non-draft PR
  rebuilds three installers (`pr-build.yml`, ~7–13 min each) on shared runners. Commit
  locally as often as you like; push once there is something worth installing.
