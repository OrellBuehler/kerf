# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Kerf is

A cross-platform desktop app for AI-assisted, **non-destructive** video/audio editing.
A Cargo workspace of three Rust crates + a Tauri-embedded SvelteKit frontend. The
distinguishing feature: a **stdio MCP server** lets an LLM analyze media and assemble
edits through the same engine the GUI uses. Nothing is re-encoded until export.

## The `ffmpeg` feature — read this first

`ffmpeg-next` links the system FFmpeg dev libraries, which are **not always installed**.
Every crate has a default-on `ffmpeg` feature that forwards to `kerf-core/ffmpeg`.
In the workspace `Cargo.toml`, `kerf-core` is declared with `default-features = false`,
so the feature is **only** activated through these forwards — which is what makes
`--no-default-features` actually disable it everywhere.

**The engine has two backends** (`crates/kerf-core/src/engine/`):

- `cli.rs` is **always compiled** and drives the `ffmpeg` / `ffprobe` **binaries**
  (override with `KERF_FFMPEG` / `KERF_FFPROBE`). Probe, `silencedetect`, scene
  detection, preview frames (`frame_at`; `frame_jpeg` for a low-res JPEG), the
  per-asset **contact sheet** (`contact_sheet` — a `tile`d grid of frames sampled
  across a range, for skimming footage) and the **composited timeline still**
  (`timeline_frame` / `build_timeline_frame_args`, pure + unit-tested — overlays
  every clip visible at a timeline time onto a black canvas, mirroring the export
  geometry, so an agent can *see the cut*), waveforms, and export all live here, so
  they work in the `--no-default-features` build — only the binaries are needed,
  never the dev libraries. Export is a **positional, multi-track** `filter_complex`
  (`build_export_args` / `build_filter_complex`, both pure + unit-tested): a black
  canvas with every video clip `overlay`'d at its `timeline_start` (later tracks on
  top, gaps fall through to black) and every audio-bearing clip `adelay`'d to its
  position and summed with `amix` — so clip positions, gaps and track layering all
  render. Each input gets a **per-input `-ss` fast-seek** to its clip's source-window
  start (shared `clip_source_window`/`clip_seek`, frame-accurate against the
  seek-relative `trim`), so a cut from deep in a long source decodes only the kept
  region, not everything from `t=0`. `render_with_progress` streams ffmpeg's
  `-progress` to report `{fraction, elapsed_secs, eta_secs}` and polls a cancel
  callback (killing ffmpeg → `RenderStatus::Cancelled`); `render_with` is the
  no-op-callback wrapper.
- `ffmpeg.rs` is the in-process **libav** backend (the `ffmpeg` feature): it supplies
  `probe` and, behind the extra `libav-render` feature, an **experimental** in-process
  export pipeline. It can only compile with the dev libraries present (written against
  the ffmpeg-next 8.1 API). The default export path is the CLI one even in full builds.

Two more optional features: `libav-render` (above) and `whisper` (local `whisper-rs`
transcription; needs a ggml model via `KERF_WHISPER_MODEL` and the whisper.cpp build
toolchain). Both are off by default and **not** exercised by `--no-default-features` CI.

- **With FFmpeg dev libs** (full build): `cargo build` / `cargo run -p kerf-app`.
- **Without them** (CI, UI work): pass `--no-default-features`; everything but the
  in-process libav probe still works via the binaries.

## Common commands

```bash
# Rust — verify / test without FFmpeg dev libs (works everywhere)
cargo check --workspace --no-default-features
cargo test  -p kerf-core --no-default-features
cargo test  -p kerf-core --no-default-features split_and_remove_roundtrip   # single test

# MCP server — the desktop app hosts it (streamable HTTP on 127.0.0.1:7777/mcp).
# Run the app (below), then point an MCP client at the URL, e.g.:
#   claude mcp add --transport http kerf http://127.0.0.1:7777/mcp
# Override the bind address with KERF_MCP_ADDR. There is no standalone MCP binary.

# Frontend (Bun) — from frontend/
bun install
bun run dev      # http://localhost:1420, fixed port; uses sample data outside Tauri
bun run build    # static SPA -> frontend/build (consumed by Tauri)
bun run check    # svelte-check (type check)

# Desktop app — Tauri config is NOT at the default path, pass --config
bunx @tauri-apps/cli@2 dev   --config crates/kerf-app/tauri.conf.json
bunx @tauri-apps/cli@2 build --config crates/kerf-app/tauri.conf.json
cargo run -p kerf-app        # also works; runs the frontend dev command first
```

There is no Rust lint config beyond defaults; `cargo clippy --workspace --no-default-features` is fine.

## Architecture

`kerf-core` is the UI-agnostic engine. **`kerf-app` is the only binary; it is a thin
adapter over the `Project` API and exposes that same API twice — as Tauri commands to
the webview and as MCP tools to a connected LLM, both over one shared `Project`.** Add
capabilities to `kerf-core` first, then expose them in each surface. Keep that boundary:
no editing logic in the adapter.

### kerf-core (`crates/kerf-core/src/`)

- `model.rs` — the domain types and the only place timeline math lives: `Asset`,
  `StreamInfo`, `Timeline`→`Track`→`Clip` (the EDL), `AssetAnalysis`. A `Clip`
  references a source range (`source_in`/`source_out`) of an asset at a
  `timeline_start` — non-destructive. Inherent helpers (`Timeline::locate`,
  `Track::end`/`reflow`, `Clip::duration`) back the operations.
- `project.rs` — `Project` wraps a `rusqlite::Connection`. **Persistence shape:**
  `assets` and `analysis` are real tables (streams/analysis stored as JSON columns);
  the **entire timeline is a single JSON blob** in a one-row `timeline` table. All
  edits go through `edit_timeline(|tl| ...)` which loads → mutates → saves the blob.
  `Project::sample()` seeds an in-memory demo (two assets + analysis + a starter
  timeline + a sample task queue); it backs the kerf-core tests, but the app now
  launches with an **empty** `Project::open_in_memory()` — the user imports media or
  opens a `.kerf` file to populate it.
  `analyze_asset`, `frame_at` and `waveform` delegate to the engine; editing ops are
  unchanged. The **agent task queue** is a real `tasks` table (one row per `Task`,
  columns not JSON): `add_task` / `list_tasks` / `claim_next_task` / `complete_task`
  / `fail_task` / `resolve_task` / `remove_task` drive the `queued → working →
  ready → done` (or `failed`) lifecycle in `model.rs`.
- `analysis.rs` — transcription / scene / silence are **pluggable traits**
  (`Transcriber`, `SceneDetector`, `SilenceDetector`). Real impls now exist:
  `FfmpegSilenceDetector` / `FfmpegSceneDetector` (CLI engine, always available) and
  `WhisperTranscriber` (`whisper` feature); `NullAnalyzer` is still the fallback.
  `Project::analyze_asset` wires them and caches the `AssetAnalysis`.
- `error.rs` — `Error`/`Result`; the `Ffmpeg(#[from] ffmpeg_next::Error)` variant is
  itself `#[cfg(feature = "ffmpeg")]`.

### embedded MCP server (`crates/kerf-app/src/mcp.rs`)

The app **is** the MCP server — there is no separate binary. `mcp::serve` hosts the
tools over `rmcp` 1.7's **streamable-HTTP** transport (`StreamableHttpService` +
`LocalSessionManager`, nested into an `axum` router) on `127.0.0.1:7777/mcp`
(`KERF_MCP_ADDR` overrides). It is spawned from `lib.rs`'s Tauri `.setup` hook on
`tauri::async_runtime` and shares the **same** `Arc<Mutex<Project>>` the Tauri commands
hold, so the agent edits the project the user has open. Patterns that matter if you edit
it: `#[tool_router]` on the impl + `#[tool_handler]` on `impl ServerHandler` — **no
`tool_router` field on the struct** (the macro calls `Self::tool_router()`).
`ServerInfo` is `#[non_exhaustive]`, so `get_info` builds it via `Default::default()`
then mutates fields. Most tools return `Result<String, McpError>` (pretty JSON), but the
three **visual** tools — `get_frame` (a single drill-in frame), `skim_asset` (a
contact-sheet montage of an asset + a text index of cell→timestamp, for finding good
parts) and `preview_timeline` (the composited cut at a timeline time) — return
`Result<CallToolResult, McpError>` built by the `image_result` helper: a caption
`Content::text` plus a `Content::image(bare_base64, "image/jpeg")` block the LLM can
actually *see* (rmcp wants bare base64 + MIME, **not** a `data:` URL). The `lock()`
helper sets `EditSource::Agent` per-op under the shared lock (the GUI's `project()`
helper sets `User` the same way); every **mutating** tool calls `self.changed()`, which
emits a `project-changed` Tauri event so the webview re-fetches and the edit shows up
live in the GUI.

### kerf-app (`crates/kerf-app/src/lib.rs`, `main.rs`)

Tauri v2 shell. `lib.rs::run()` is the entry (`main.rs` just calls it); it owns the
`Arc<Mutex<Project>>` (cloned into both the Tauri managed state and `mcp::serve`) and
registers a command per `Project` op — reads (`list_assets`,
`get_timeline`, `get_asset_metadata`), `import_asset` / `analyze_asset`, every editing
op (`cut_clip`, `add_clip`, `split_clip`, `trim_clip`, `reorder_clip`, `move_clip`,
`ripple_delete`, `add_track`, `remove_track`, `remove_clip`, `set_volume`, `set_fade`,
`remove_silence`, `extract_audio`, `concatenate` — each returns the
refreshed `Timeline`), media (`get_frame` → base64 PNG data URL, `get_waveform`), the
agent task queue (`list_tasks`, `add_task` → the new `Task`; `resolve_task` /
`remove_task` → the refreshed `Task[]`), and `export_timeline` (emits `export-progress`
events) / `cancel_export`. Tauri auto-converts JS camelCase args to Rust
snake_case (`{ assetId }` → `asset_id`). Config: `tauri.conf.json` points
`frontendDist` at `../../frontend/build` (resolved relative to the config file). The
`beforeDevCommand`/`beforeBuildCommand` hooks, however, run from Tauri's *app dir* —
which for this `crates/kerf-app` layout resolves to `crates/`, not the config dir or repo
root — so they anchor to the repo via `cd "$(git rev-parse --show-toplevel)/frontend" && bun run dev`
instead of a fragile relative path.
`capabilities/default.json` grants `core:default` + `dialog:default`.

### frontend (`frontend/`)

SvelteKit 2 / Svelte 5 **runes** (forced on in `vite.config.ts`). Two layout quirks:
- **No `svelte.config.js`** — adapter and compiler options live inline in
  `vite.config.ts` via the `sveltekit()` plugin (new-style config). Static SPA via
  `adapter-static` (fallback `index.html`); `+layout.ts` sets `ssr = false` +
  `prerender = true`. Dev port is pinned to **1420** for Tauri.
- **Tailwind 4 = CSS config**, no `tailwind.config.js`. `src/routes/layout.css` imports
  the **Kerf design tokens** (`src/lib/styles/kerf-tokens.css`) and maps the shadcn
  semantic vars onto them; the app is **dark-only** (`<html class="dark">` in `app.html`).
  That file is also the `tailwind.css` in `components.json`. Run
  `bunx shadcn-svelte add <name>` to add primitives.

The editor UI is implemented from the **Kerf design system** (claude.ai/design): a dark,
editor-grade workspace under `src/lib/components/editor/` — bespoke atoms (`Btn`,
`IconBtn`, `Badge`, `Icon`, `KerfMark`) plus `TitleBar`, `Toolbar`, `MediaBin`,
`Preview`, `Timeline`, `AgentPanel`, `StatusBar`, composed by `routes/+page.svelte`.
Everything is styled with the CSS-variable tokens directly (inline `style`), not Tailwind
utilities. The **timeline is a bespoke NLE timeline** that renders **real `editor.timeline`
state** (ruler + tracks + clips positioned by `timeline_start`/duration at `ui.zoom`
px/sec + playhead), with scene markers / silence regions mapped from `AssetAnalysis` and
real audio waveforms (`get_waveform`); the razor tool splits, Delete removes, Shift+Delete
ripple-deletes, clicks select/seek, and (pointer tool) **clips drag to reposition** — free
positioning with gaps, snapping to clip edges / playhead / 0, and **dropping onto another
same-kind track** (`move_clip`, via pointer events + `data-lane` hit-testing). The timeline
toolbar's `+ V` / `+ A` add tracks and each track header has a `×` to remove one
(`add_track` / `remove_track`); the timeline is genuinely **multi-track**. The old
`@xyflow/svelte` `TimelineCanvas`/`clip-node` scaffold was removed (the
dep is still in `package.json`, now unused). `Preview` shows the **decoded frame**
(`get_frame`) under the playhead with real playback/scrub. The **agent panel is a real MCP task
queue** (status · queue · history · add-task) — Kerf has no in-app chat; a connected
LLM claims tasks over MCP. The queue is `agent` state (`src/lib/agent.svelte.ts`, a third
runes singleton) backed by the `tasks` table over Tauri/MCP: the add-task box and preset chips
`agent.add(...)` real tasks, and `ready` tasks show Apply/Dismiss (`resolve_task`/`remove_task`).
Two preset chips (`Remove silences` / `Assemble rough cut`) also run the matching local op and
resolve their task; the rest just enqueue for the agent. In the browser there is no agent, so
queued tasks correctly just wait. Below the queue, the **History** section renders
`editor.history` (the `Revision[]` edit log, attributed to user/agent/system) with one-click
`editor.revertTo(seq)`. `data.ts` keeps only the `STATUS_MAP`/`PRESETS` presentation bits —
all project data renders from the real backend.

`src/lib/api.ts` is the backend bridge: `inTauri()` decides between `invoke(...)` and a
**seeded in-memory sample with working local timeline ops**, so every edit/analysis/waveform
is explorable in a plain browser via `bun run dev` (frames return `null` there → Preview
keeps its placeholder). This browser sample is a **dev harness only** — the desktop app always
uses the real backend and starts empty. State is two runes singletons: `src/lib/state.svelte.ts`
(`export const editor` — assets, timeline, analyses, selection, and the editing actions that
call the backend and apply the returned `Timeline`) and `src/lib/editor-ui.svelte.ts`
(`export const ui` — chrome state, playhead/zoom/playback, and `runAnalysis` which runs real
analysis and toggles the `analyzing` flag). There is **no scripted demo phase machine**: the
editor chrome derives from real state — `MediaBin` shows a dropzone until `editor.assets` is
non-empty, `StatusBar` shows the selected asset's real fps/resolution/codec and timeline
duration, and `Preview` shows the decoded frame or a "No media loaded" placeholder.

## Conventions

- Keep types in sync across the boundary: `kerf-core` serde structs ↔ `frontend/src/lib/types.ts`.
  Field names are snake_case in the JSON on both Tauri and MCP.
- License is **PolyForm Noncommercial 1.0.0** (public repo). New files inherit it via
  `license.workspace = true`; don't add other license headers.
- Versions were pinned against the crates.io sparse index / npm; check there (not the
  blocked crates.io JSON API) before bumping.
