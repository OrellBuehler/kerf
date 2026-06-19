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

- **With FFmpeg dev libs** (full build): `cargo build` / `cargo run -p kerf-app`.
- **Without them** (CI, UI work, MCP read tools): always pass `--no-default-features`.
  In this mode `Project::import_asset` and `Project::export` return `Error::FfmpegDisabled`;
  everything else (timeline editing, persistence, the seeded sample) works.

When disabled, the `ffmpeg` engine module is `#[cfg]`-compiled out and replaced by
stubs in `crates/kerf-core/src/engine/mod.rs`. The real libav code lives in
`engine/ffmpeg.rs` (written against the ffmpeg-next 8.1 API); it can only be
compiled with the dev libraries present.

## Common commands

```bash
# Rust — verify / test without FFmpeg dev libs (works everywhere)
cargo check --workspace --no-default-features
cargo test  -p kerf-core --no-default-features
cargo test  -p kerf-core --no-default-features split_and_remove_roundtrip   # single test

# MCP server (logs -> stderr, JSON-RPC -> stdout)
cargo run -p kerf-mcp --no-default-features            # seeded in-memory sample
cargo run -p kerf-mcp -- path/to.kerf                  # full build, open a project

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

`kerf-core` is the UI-agnostic engine. **Both `kerf-mcp` and `kerf-app` are thin
adapters over the same `Project` API** — add capabilities to `kerf-core` first, then
expose them in each adapter. Keep that boundary: no editing logic in the adapters.

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
  timeline) and is what both adapters launch with by default.
- `analysis.rs` — transcription / scene / silence are **pluggable traits**
  (`Transcriber`, `SceneDetector`, `SilenceDetector`) with a `NullAnalyzer`. Wire real
  engines (whisper-rs, FFmpeg `silencedetect`) here without touching core logic.
- `error.rs` — `Error`/`Result`; the `Ffmpeg(#[from] ffmpeg_next::Error)` variant is
  itself `#[cfg(feature = "ffmpeg")]`.

### kerf-mcp (`crates/kerf-mcp/src/main.rs`)

stdio MCP server using `rmcp` 1.7. Pattern that matters if you edit it:
`#[tool_router]` on the impl + `#[tool_handler]` on `impl ServerHandler` — **no
`tool_router` field on the struct** (the macro calls `Self::tool_router()`).
`ServerInfo` is `#[non_exhaustive]`, so `get_info` builds it via `Default::default()`
then mutates fields. Tools return `Result<String, McpError>` (pretty JSON). Each tool
locks `Arc<Mutex<Project>>` and calls a `kerf-core` method. **Never print to stdout** —
it's the transport; use `tracing` (stderr).

### kerf-app (`crates/kerf-app/src/lib.rs`, `main.rs`)

Tauri v2 shell. `lib.rs::run()` is the entry (`main.rs` just calls it); it manages a
`Mutex<Project>` and registers commands (`list_assets`, `get_timeline`,
`get_asset_metadata`, `import_asset`). Tauri auto-converts JS camelCase args to Rust
snake_case (`{ assetId }` → `asset_id`). Config: `tauri.conf.json` points
`frontendDist` at `../../frontend/build` and runs the frontend via `cd ../../frontend && bun run dev`.
`capabilities/default.json` grants `core:default` + `dialog:default`.

### frontend (`frontend/`)

SvelteKit 2 / Svelte 5 **runes** (forced on in `vite.config.ts`). Two layout quirks:
- **No `svelte.config.js`** — adapter and compiler options live inline in
  `vite.config.ts` via the `sveltekit()` plugin (new-style config). Static SPA via
  `adapter-static` (fallback `index.html`); `+layout.ts` sets `ssr = false` +
  `prerender = true`. Dev port is pinned to **1420** for Tauri.
- **Tailwind 4 = CSS config**, no `tailwind.config.js`. Theme tokens (shadcn vega
  preset) live in `src/routes/layout.css`; that's also the `tailwind.css` in
  `components.json`. Run `bunx shadcn-svelte add <name>` to add primitives.

`src/lib/api.ts` is the backend bridge: `inTauri()` decides between `invoke(...)` and
**seeded sample data**, so `bun run dev` is fully explorable in a plain browser.
State is one runes singleton (`src/lib/state.svelte.ts`, `export const editor`).
The timeline is `@xyflow/svelte` (Svelte Flow): `TimelineCanvas.svelte` rebuilds clip
nodes from `editor.timeline` in an `$effect`; `clip-node.svelte` is the custom node.

## Conventions

- Keep types in sync across the boundary: `kerf-core` serde structs ↔ `frontend/src/lib/types.ts`.
  Field names are snake_case in the JSON on both Tauri and MCP.
- License is **PolyForm Noncommercial 1.0.0** (public repo). New files inherit it via
  `license.workspace = true`; don't add other license headers.
- Versions were pinned against the crates.io sparse index / npm; check there (not the
  blocked crates.io JSON API) before bumping.
