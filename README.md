# Kerf

**Kerf** is a cross-platform desktop application for AI-assisted video/audio editing.
It pairs a non-destructive, FFmpeg-backed editing engine with a **stdio MCP server**,
so an LLM can analyze loaded media and assemble edits through well-defined tools.

> A _kerf_ is the slit a saw leaves behind — the cut. Kerf edits non-destructively:
> nothing is re-encoded until you export.

---

## Architecture

Kerf is a Cargo workspace of three Rust crates plus a Tauri-embedded SvelteKit frontend:

```
kerf/
├── Cargo.toml                 # workspace (pinned deps)
├── crates/
│   ├── kerf-core/             # domain model, .kerf persistence, FFmpeg engine
│   │   ├── model.rs           #   Asset / Timeline (EDL) / Clip / analysis types
│   │   ├── project.rs         #   .kerf project (SQLite) + timeline operations
│   │   ├── analysis.rs        #   pluggable transcription / scene / silence traits
│   │   └── engine/ffmpeg.rs   #   in-process libav probe + filtergraph render
│   └── kerf-app/              # Tauri v2 shell: webview commands + embedded MCP server
│       └── src/mcp.rs         #   rmcp HTTP MCP server, shares the app's Project
└── frontend/                  # SvelteKit 2 / Svelte 5 / Tailwind 4 / shadcn-svelte
    └── src/lib/components/     #   MediaBin, PreviewPlayer, TimelineCanvas, AgentPanel
```

- **`kerf-core`** — the editor brain, UI-agnostic. A `.kerf` project is a SQLite
  database holding imported **assets** (probed metadata), cached **analysis**
  (silence, scene changes, transcript), and a non-destructive **timeline / EDL**
  (tracks of clips referencing source ranges). FFmpeg access is in-process via
  `ffmpeg-next`, gated behind a default-on `ffmpeg` cargo feature.
- **`kerf-app`** — the Tauri v2 desktop shell. Tauri commands bridge the SvelteKit
  frontend to `kerf-core`, and the app **also hosts an embedded
  [Model Context Protocol](https://modelcontextprotocol.io) server** (official Rust
  SDK, `rmcp`, streamable HTTP) that exposes the timeline + media engine as tools over
  the *same* live project — so a connected LLM edits what the user has open.
- **`frontend/`** — media bin, a Svelte Flow (`@xyflow/svelte`) timeline canvas,
  preview player, and an AI agent panel, built with shadcn-svelte primitives.

### Tech stack (verified versions)

| Layer        | Choice                                            |
| ------------ | ------------------------------------------------- |
| Shell        | Rust + Tauri **2.11**                             |
| Frontend     | SvelteKit **2** / Svelte **5** (runes), Bun       |
| Styling      | Tailwind CSS **4** (CSS config, no JS config)     |
| Components   | shadcn-svelte **1.3** (Tailwind 4 registry)       |
| Timeline UI  | `@xyflow/svelte` **1.6** (Svelte Flow)            |
| Media        | `ffmpeg-next` **8.1** (libav, FFmpeg ≥ 4.4)       |
| Persistence  | `rusqlite` **0.40** (bundled SQLite)              |
| MCP          | `rmcp` **1.7** (streamable-HTTP transport)        |

---

## Prerequisites

- **Rust** ≥ 1.82 (stable) — <https://rustup.rs>
- **Bun** ≥ 1.2 — <https://bun.sh> (frontend package manager / dev server)
- **FFmpeg development libraries** — only required for the default `ffmpeg` feature
  (probing and export). See per-platform setup below.
- **clang / libclang** — `ffmpeg-sys-next` uses bindgen.
- Platform **WebView/GTK** libraries for Tauri.

### Linux (Debian / Ubuntu)

```bash
# Tauri system dependencies
sudo apt update
sudo apt install -y build-essential curl wget file pkg-config clang \
  libwebkit2gtk-4.1-dev librsvg2-dev libxdo-dev libssl-dev \
  libayatana-appindicator3-dev

# FFmpeg development libraries (for the `ffmpeg` feature)
sudo apt install -y libavutil-dev libavcodec-dev libavformat-dev \
  libavdevice-dev libavfilter-dev libswscale-dev libswresample-dev
```

### macOS

```bash
xcode-select --install            # Command Line Tools (provides clang)
brew install ffmpeg pkg-config    # FFmpeg dev libraries + pkg-config
```

### Windows

Install the [WebView2 runtime](https://developer.microsoft.com/microsoft-edge/webview2/)
(preinstalled on Windows 11) and the MSVC build tools, then provide FFmpeg. The
simplest route is [vcpkg](https://vcpkg.io):

```powershell
vcpkg install ffmpeg:x64-windows
$env:VCPKG_ROOT = "C:\path\to\vcpkg"
# or point ffmpeg-sys-next at a shared FFmpeg build:
$env:FFMPEG_DIR = "C:\ffmpeg"      # contains include/ lib/ bin/
```

You also need LLVM/clang on `PATH` for bindgen (`winget install LLVM.LLVM`).

> **No system FFmpeg?** You can build FFmpeg from source as part of the crate with
> `cargo build --features kerf-core/build` (slow), or skip media features entirely
> with `--no-default-features` (see below).

---

## Build & run

### Frontend only (browser, for UI work)

```bash
cd frontend
bun install
bun run dev       # http://localhost:1420 — runs with seeded sample data
bun run build     # static SPA into frontend/build (consumed by Tauri)
```

Outside Tauri the UI falls back to a seeded sample project so it is fully explorable
in a browser.

### Desktop app (Tauri)

```bash
cd frontend && bun install && cd ..
cargo run -p kerf-app            # debug; runs beforeDevCommand to start Vite
# or, with the Tauri CLI for HMR + bundling:
bunx @tauri-apps/cli@2 dev   --config crates/kerf-app/tauri.conf.json
bunx @tauri-apps/cli@2 build --config crates/kerf-app/tauri.conf.json
```

### MCP server

The MCP server is **embedded in the desktop app** — running `kerf-app` (above) starts
it on `127.0.0.1:7777/mcp` (override with `KERF_MCP_ADDR`). There is no separate
binary; the agent edits the same project the GUI has open. See
[MCP server](#mcp-server-1) below to connect a client.

### Building without FFmpeg

Every crate exposes an `ffmpeg` feature (on by default) that forwards to
`kerf-core/ffmpeg`. Disable it to build the domain model, persistence, and MCP read
tools **without the FFmpeg dev libraries installed**:

```bash
cargo check  --workspace        --no-default-features
cargo test   -p kerf-core       --no-default-features
cargo run    -p kerf-app        --no-default-features
```

In this mode the in-process libav **probe** is unavailable, but import, analysis
(`silencedetect` / scene detection), preview frames, waveforms and export all
still work by driving the system `ffmpeg` / `ffprobe` **binaries** — only the
optional `libav-render` (in-process export) and `whisper` (transcription)
features need a fuller toolchain.

---

## MCP server

The desktop app hosts the MCP server over streamable HTTP and exposes these tools, all
operating on the same live, non-destructive timeline the GUI shows:

| Tool                    | Purpose                                                    |
| ----------------------- | --------------------------------------------------------- |
| `list_assets`           | List imported media assets                                |
| `get_asset_metadata`    | Probed metadata + cached analysis for an asset            |
| `get_timeline_state`    | The full timeline / EDL                                   |
| `cut_clip`              | Append a `[start, end)` cut of an asset                   |
| `add_clip_to_timeline`  | Add a clip referencing a source range to a track          |
| `split_at`              | Split a timeline clip at a time                           |
| `trim`                  | Adjust a clip's source in/out                             |
| `reorder`               | Move a clip within its track (re-flows gaplessly)         |
| `remove`                | Remove a clip                                             |
| `set_volume`            | Set a clip's linear gain                                  |
| `remove_silence`        | Append the non-silent spans of an asset (uses analysis)   |
| `extract_audio`         | Append an asset's audio to the audio track                |
| `concatenate`           | Stitch several assets end-to-end                          |
| `export`                | Render the timeline (requires the `ffmpeg` feature)       |
| `history`               | List timeline revisions (the edit history)                |
| `undo` / `redo`         | Step back / forward through the edit history              |
| `revert_to`             | Restore the timeline to a specific revision               |
| `list_tasks`            | List the agent task queue with each task's status         |
| `add_task`              | Enqueue a task (status: queued)                           |
| `claim_next_task`       | Claim the oldest queued task (marks it working)           |
| `complete_task`         | Mark a claimed task ready for review, with a summary      |
| `fail_task`             | Mark a task failed with an error message                  |

The **task queue** is how the user and a connected LLM hand work back and forth:
the desktop app enqueues plain-language tasks, the agent calls `claim_next_task`,
performs edits with the timeline tools above, then `complete_task`s with a summary
the user reviews and applies. Kerf never edits on its own.

### Connect it to Claude Code / Claude Desktop

Start the desktop app first (it hosts the server), then register the HTTP endpoint.
With Claude Code:

```bash
claude mcp add --transport http kerf http://127.0.0.1:7777/mcp
```

Or directly in an MCP client config that supports HTTP servers:

```json
{
  "mcpServers": {
    "kerf": {
      "type": "http",
      "url": "http://127.0.0.1:7777/mcp"
    }
  }
}
```

### Smoke test (no MCP client needed)

With the app running, hit the endpoint over HTTP (the streamable-HTTP transport
replies via SSE, so ask for an event stream):

```bash
curl -sN http://127.0.0.1:7777/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"x","version":"0"}}}'
```

---

## Tauri commands

The desktop shell exposes these commands to the frontend (`@tauri-apps/api`):

- `list_assets() -> Asset[]`
- `get_timeline() -> Timeline`
- `get_asset_metadata(assetId) -> { asset, analysis }`
- `import_asset(path) -> Asset` _(requires the `ffmpeg` feature)_
- `list_tasks() -> Task[]`, `add_task(prompt) -> Task`, `resolve_task(taskId) -> Task[]`,
  `remove_task(taskId) -> Task[]` — the agent task queue
- `get_history() -> Revision[]`, `undo() / redo() -> Timeline`, `revert_to(seq) -> Timeline`
  — the timeline edit history

## Analysis is pluggable

Transcription, scene detection, and silence detection are abstracted behind the
`Transcriber`, `SceneDetector`, and `SilenceDetector` traits in
`kerf-core::analysis`. A `NullAnalyzer` is provided; plug in `whisper-rs`, an
external service, or an FFmpeg `silencedetect` pass without touching the rest of
the core.

## Current status

This is a scaffold that boots end-to-end:

- ✅ Cargo workspace + Tauri v2 app with a SvelteKit/Svelte 5 frontend (Tailwind 4,
  shadcn-svelte, bespoke NLE timeline).
- ✅ `kerf-core` domain model, SQLite `.kerf` persistence, and timeline operations
  (unit-tested).
- ✅ FFmpeg engine: CLI-driven probe/analysis/frames/waveforms/export everywhere,
  plus `ffmpeg-next` in-process probing under the `ffmpeg` feature.
- ✅ Working stdio MCP server (24 tools, incl. `analyze_asset`, the task-queue
  tools, and the edit-history tools) verified against a sample project.
- ✅ Tauri commands wiring the frontend to every `kerf-core` operation (editing,
  analysis, preview frames, waveforms, export).
- ✅ Real local analysis: FFmpeg `silencedetect` + scene detection, decoded preview
  frames, and audio waveforms — all CLI-driven, so no dev libraries required.
- ✅ Timeline, preview and transcript render real backend state (not mock data).
- ✅ Agent task queue persisted in `kerf-core` (`tasks` table), exposed over MCP
  (`list_tasks` / `claim_next_task` / `complete_task` / `fail_task`) and as Tauri
  commands; the agent panel renders the live queue and an add-task box, so a
  connected LLM and the user hand work back and forth over MCP.
- ✅ Revertible timeline edit history (`history` table, attributed to user/agent/
  system) with `undo` / `redo` / `revert_to` over MCP and Tauri; the agent panel
  shows the revision list with one-click revert.

Behind feature flags (need a fuller toolchain, not exercised in the default CI
build): `libav-render` — an experimental in-process libav export pipeline; and
`whisper` — local `whisper-rs` transcription (set `KERF_WHISPER_MODEL`).

Next up: a live activity stream pushed from the MCP server (the queue is polled
on load today), and richer staged-edit diffs in the review step.

## License

[PolyForm Noncommercial License 1.0.0](./LICENSE.md) — free for noncommercial use.
