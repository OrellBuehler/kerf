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
│   ├── kerf-mcp/              # stdio MCP server (rmcp) operating on kerf-core
│   └── kerf-app/              # Tauri v2 shell + commands bridging the frontend
└── frontend/                  # SvelteKit 2 / Svelte 5 / Tailwind 4 / shadcn-svelte
    └── src/lib/components/     #   MediaBin, PreviewPlayer, TimelineCanvas, AgentPanel
```

- **`kerf-core`** — the editor brain, UI-agnostic. A `.kerf` project is a SQLite
  database holding imported **assets** (probed metadata), cached **analysis**
  (silence, scene changes, transcript), and a non-destructive **timeline / EDL**
  (tracks of clips referencing source ranges). FFmpeg access is in-process via
  `ffmpeg-next`, gated behind a default-on `ffmpeg` cargo feature.
- **`kerf-mcp`** — an stdio [Model Context Protocol](https://modelcontextprotocol.io)
  server (official Rust SDK, `rmcp`) exposing the timeline + media engine as tools.
- **`kerf-app`** — the Tauri v2 desktop shell. Tauri commands bridge the SvelteKit
  frontend to `kerf-core`.
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
| MCP          | `rmcp` **1.7** (stdio transport)                  |

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

```bash
cargo run -p kerf-mcp                 # serves a seeded sample project on stdio
cargo run -p kerf-mcp -- path/to.kerf # serve an existing project
```

Logs go to **stderr**; **stdout** is reserved for the MCP JSON-RPC transport.

### Building without FFmpeg

Every crate exposes an `ffmpeg` feature (on by default) that forwards to
`kerf-core/ffmpeg`. Disable it to build the domain model, persistence, and MCP read
tools **without the FFmpeg dev libraries installed**:

```bash
cargo check  --workspace        --no-default-features
cargo test   -p kerf-core       --no-default-features
cargo run    -p kerf-mcp        --no-default-features
```

In this mode `import_asset` and `export` return `FfmpegDisabled`; everything else
(timeline editing, analysis storage, the seeded sample) works.

---

## MCP server

`kerf-mcp` speaks MCP over stdio and exposes these tools, all operating on the
non-destructive timeline:

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

### Connect it to Claude Code / Claude Desktop

```json
{
  "mcpServers": {
    "kerf": {
      "command": "/absolute/path/to/target/debug/kerf-mcp",
      "args": []
    }
  }
}
```

### Smoke test (no MCP client needed)

```bash
cargo build -p kerf-mcp --no-default-features
printf '%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"x","version":"0"}}}' \
 '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
 '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_assets","arguments":{}}}' \
 | ./target/debug/kerf-mcp
```

---

## Tauri commands

The desktop shell exposes these commands to the frontend (`@tauri-apps/api`):

- `list_assets() -> Asset[]`
- `get_timeline() -> Timeline`
- `get_asset_metadata(assetId) -> { asset, analysis }`
- `import_asset(path) -> Asset` _(requires the `ffmpeg` feature)_

## Analysis is pluggable

Transcription, scene detection, and silence detection are abstracted behind the
`Transcriber`, `SceneDetector`, and `SilenceDetector` traits in
`kerf-core::analysis`. A `NullAnalyzer` is provided; plug in `whisper-rs`, an
external service, or an FFmpeg `silencedetect` pass without touching the rest of
the core.

## Current status

This is a scaffold that boots end-to-end:

- ✅ Cargo workspace + Tauri v2 app with a SvelteKit/Svelte 5 frontend (Tailwind 4,
  shadcn-svelte, Svelte Flow rendering the timeline).
- ✅ `kerf-core` domain model, SQLite `.kerf` persistence, and timeline operations
  (unit-tested).
- ✅ `ffmpeg-next` integrated for in-process probing + a filtergraph export path
  (gated behind the `ffmpeg` feature).
- ✅ Working stdio MCP server (14 tools) verified against a sample project.
- ✅ Tauri commands wiring the frontend to `kerf-core`.

Stubbed / next up: real frame decoding into the preview player, waveform rendering,
an in-process (rather than CLI-driven) export pipeline, and wiring the agent panel
to an LLM.

## License

[PolyForm Noncommercial License 1.0.0](./LICENSE.md) — free for noncommercial use.
