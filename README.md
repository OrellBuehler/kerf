<div align="center">

<img src="docs/img/kerf-mark.svg" alt="Kerf" height="72" />

# Kerf

### The non-destructive video editor your AI can drive.

Kerf is a cross-platform desktop editor with an **embedded MCP server**, so an LLM
(Claude, or any MCP client) can analyze your footage and assemble the cut through the
**same engine the GUI uses, on the same live project** — and it can actually *see* the
frames it's editing. Nothing is re-encoded until you export.

[![CI](https://github.com/OrellBuehler/kerf/actions/workflows/ci.yml/badge.svg)](https://github.com/OrellBuehler/kerf/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/OrellBuehler/kerf?color=e29d2e&label=release)](https://github.com/OrellBuehler/kerf/releases)
[![License](https://img.shields.io/badge/license-PolyForm%20Noncommercial-22b4c4.svg)](./LICENSE.md)
[![Rust](https://img.shields.io/badge/Rust-1.82+-dea584.svg?logo=rust&logoColor=white)](https://rustup.rs)
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB.svg?logo=tauri&logoColor=white)](https://tauri.app)
[![Stars](https://img.shields.io/github/stars/OrellBuehler/kerf?style=flat&color=e29d2e)](https://github.com/OrellBuehler/kerf/stargazers)

</div>

<div align="center">

<!-- Drop a screenshot of the full editor here. See docs/img/README.md for the shot list. -->
<img src="docs/img/screenshot-editor.png" alt="Kerf editor — multi-track timeline, preview, and agent panel" width="880" />

</div>

> A _kerf_ is the slit a saw leaves behind — the cut itself. Kerf edits
> non-destructively: your timeline is an edit list over the original media, and
> nothing is re-encoded until you hit export.

---

## Why Kerf is different

Plenty of tools bolt a chatbot onto a video editor. Kerf inverts that: the editor **is**
an API, exposed twice over one shared project — as the GUI you click, and as
[Model Context Protocol](https://modelcontextprotocol.io) tools an agent calls. So the
agent isn't scripting a black box; it drives the identical engine, and every edit it
makes shows up **live** in your timeline for you to review.

- 🎬 **The agent has eyes.** `get_frame`, `skim_asset` (a contact-sheet montage for
  finding the good parts), and `preview_timeline` (the composited cut at a given time)
  return real images the model *sees* — so it can find the right moment and confirm the
  cut, not guess from metadata.
- 🧠 **Same engine, same project.** The MCP server and the webview hold the *same*
  `Project`. When the agent trims a clip, your GUI re-renders it instantly.
- 🤝 **You stay in control.** Work is handed back and forth through a **task queue**: you
  enqueue plain-language tasks, the agent claims one, does the edits, and marks it *ready*
  for you to apply or dismiss. Kerf never edits on its own, and every change is a
  revertible entry in the edit history.
- ✂️ **Truly non-destructive.** Everything is an EDL over source ranges. Cuts, effects,
  and keyframes are just data until you export a single `filter_complex`.
- 📦 **Runs without FFmpeg dev libraries.** The engine drives the `ffmpeg`/`ffprobe`
  binaries, so probe, analysis, frames, waveforms, and export all work on a plain
  install — the in-process libav path is optional.

---

## Features

| | |
| --- | --- |
| **Multi-track NLE** | Bespoke timeline: video/audio/text tracks, free clip positioning with gaps, drag-to-move across tracks, edge-drag trim, razor split, ripple delete, snapping to edges / playhead / **beats**. |
| **Audible playback** | Real Web-Audio playback with J/K/L shuttle + scrub; volume, fades, speed and reverse are auralized and the playhead follows the audio clock. |
| **Analysis** | FFmpeg `silencedetect`, scene detection, audio energy/**beat** grid, and waveforms — all CLI-driven. Optional local **Whisper** transcription. |
| **Effects & color** | Per-clip video (`blur`/`sharpen`/`hue`/`negate`/`vignette`/`chromakey`) and audio (`highpass`/`lowpass`/`EQ`/`compressor`/`gate`) chains, plus transform + color grade. |
| **Keyframe animation** | Animated zoom, position, rotation and opacity via piecewise-linear keyframes — the Transform panel auto-keyframes at the playhead. |
| **Titles & captions** | Text overlays / lower-thirds with their own keyframes, and one-click **captions from a transcript** (SRT export too). |
| **Smart mixing** | Per-track **ducking** (music dips under dialogue via sidechain) and single-pass **loudnorm** to −14 LUFS on export. |
| **Export** | Positional, multi-track `filter_complex` with progress + cancel; **range export** renders just the region between your in/out marks. |
| **Agent workflow** | 55 MCP tools, a persisted task queue, and a fully revertible edit history attributed to user / agent / system. |

<div align="center">

<!-- Agent panel + inspector screenshots. See docs/img/README.md. -->
<img src="docs/img/screenshot-agent.png" alt="Kerf agent panel — task queue and edit history" width="430" />
<img src="docs/img/screenshot-inspector.png" alt="Kerf inspector — effects, keyframes, and overlays" width="430" />

</div>

---

## Quickstart

### Download a build

Grab the installer for your platform from the
[**latest release**](https://github.com/OrellBuehler/kerf/releases/latest) — `.dmg`/`.app`
(macOS), `.AppImage`/`.deb` (Linux), or `.msi`/`.exe` (Windows). macOS and Linux builds
expect a system FFmpeg on `PATH`; the Windows build bundles it.

### Or build from source

```bash
git clone https://github.com/OrellBuehler/kerf
cd kerf/frontend && bun install && cd ..
cargo run -p kerf-app            # launches the app (runs the frontend dev server first)
```

You need **Rust ≥ 1.82**, **Bun ≥ 1.2**, the platform WebView/GTK libraries for Tauri,
and (for the default `ffmpeg` feature) the FFmpeg development libraries + `clang`. See
[Building](#building) for per-platform setup and the no-FFmpeg path.

---

## Driving Kerf from an LLM

The desktop app hosts the MCP server over streamable HTTP at `127.0.0.1:7777/mcp`
(override with `KERF_MCP_ADDR`). Start the app, then point an MCP client at it — e.g.
with Claude Code:

```bash
claude mcp add --transport http kerf http://127.0.0.1:7777/mcp
```

…or in any MCP client config that supports HTTP servers:

```json
{
  "mcpServers": {
    "kerf": { "type": "http", "url": "http://127.0.0.1:7777/mcp" }
  }
}
```

Now ask the agent to work on the project you have open — _"skim the interview clip, cut
the dead air, and drop in captions."_ It will claim a task, use the tools below, and hand
back a reviewable result.

### The tools (55)

<details>
<summary><b>See / analyze</b></summary>

`list_assets` · `get_asset_metadata` · `analyze_asset` · `get_timeline_state` ·
`timeline_summary` · `get_waveform` · `get_energy` · **`get_frame`** (drill-in frame) ·
**`skim_asset`** (contact-sheet montage) · **`preview_timeline`** (the composited cut) —
the last three return images the model can see.
</details>

<details>
<summary><b>Cut & arrange</b></summary>

`cut_clip` · `add_clip_to_timeline` · `split_at` · `trim` · `reorder` · `move_clip` ·
`remove` · `ripple_delete` · `cut_clip_range` · `add_track` · `remove_track` ·
`set_track_duck` · `remove_silence` · `extract_audio` · `concatenate`
</details>

<details>
<summary><b>Style & animate</b></summary>

`set_volume` · `set_fade` · `set_speed` · `set_transform` · `set_color` ·
`set_transition` · `set_video_effects` · `set_audio_effects` · `set_keyframes` ·
`add_keyframe` · `clear_keyframes` · `add_overlay` · `update_overlay` · `remove_overlay` ·
`set_overlay_keyframes` · `captions_from_transcript` · `export_srt` · `list_fonts`
</details>

<details>
<summary><b>Render & hand-off</b></summary>

`export` · `list_tasks` · `add_task` · `claim_next_task` · `complete_task` · `fail_task` ·
`history` · `undo` · `redo` · `revert_to`
</details>

Every mutating tool emits a `project-changed` event, so edits appear in the GUI as the
agent makes them.

---

## Building

<details>
<summary><b>Linux (Debian / Ubuntu)</b></summary>

```bash
# Tauri system dependencies
sudo apt update
sudo apt install -y build-essential curl wget file pkg-config clang \
  libwebkit2gtk-4.1-dev librsvg2-dev libxdo-dev libssl-dev \
  libayatana-appindicator3-dev

# FFmpeg development libraries (for the default `ffmpeg` feature)
sudo apt install -y libavutil-dev libavcodec-dev libavformat-dev \
  libavdevice-dev libavfilter-dev libswscale-dev libswresample-dev
```
</details>

<details>
<summary><b>macOS</b></summary>

```bash
xcode-select --install            # Command Line Tools (provides clang)
brew install ffmpeg pkg-config    # FFmpeg dev libraries + pkg-config
```
</details>

<details>
<summary><b>Windows</b></summary>

Install the [WebView2 runtime](https://developer.microsoft.com/microsoft-edge/webview2/)
(preinstalled on Windows 11) and the MSVC build tools, then provide FFmpeg via
[vcpkg](https://vcpkg.io):

```powershell
vcpkg install ffmpeg:x64-windows
$env:VCPKG_ROOT = "C:\path\to\vcpkg"
# or point ffmpeg-sys-next at a shared build:
$env:FFMPEG_DIR = "C:\ffmpeg"      # contains include/ lib/ bin/
```

You also need LLVM/clang on `PATH` for bindgen (`winget install LLVM.LLVM`).
</details>

### Without the FFmpeg dev libraries

Every crate exposes an `ffmpeg` feature (on by default) that forwards to
`kerf-core/ffmpeg`. Disable it to build the model, persistence, and MCP tools with only
the `ffmpeg`/`ffprobe` **binaries** installed — probe, analysis, frames, waveforms and
export all still work:

```bash
cargo check  --workspace  --no-default-features
cargo test   -p kerf-core --no-default-features
cargo run    -p kerf-app  --no-default-features
```

Two further optional features need a fuller toolchain and are off by default:
`libav-render` (experimental in-process libav export) and `whisper` (local `whisper-rs`
transcription; set `KERF_WHISPER_MODEL`).

---

## Architecture

Kerf is a Cargo workspace of **two Rust crates** plus a Tauri-embedded SvelteKit frontend.
`kerf-core` is the UI-agnostic engine; `kerf-app` is a thin adapter that exposes that one
`Project` API **twice** — as Tauri commands to the webview and as MCP tools to a connected
LLM — over one shared, locked project.

```
kerf/
├── crates/
│   ├── kerf-core/            # engine: domain model, .kerf persistence, FFmpeg backends
│   │   ├── model.rs          #   Asset / Timeline (EDL) → Track → Clip, effects, keyframes
│   │   ├── project.rs        #   SQLite .kerf project + all timeline operations + task queue
│   │   ├── analysis.rs       #   pluggable Transcriber / SceneDetector / SilenceDetector
│   │   └── engine/           #   cli.rs (binaries, always on) + ffmpeg.rs (in-process libav)
│   └── kerf-app/             # Tauri v2 shell
│       ├── lib.rs            #   one Arc<Mutex<Project>> shared by both surfaces
│       └── mcp.rs            #   embedded rmcp streamable-HTTP MCP server
├── frontend/                 # SvelteKit 2 / Svelte 5 (runes) / Tailwind 4 / shadcn-svelte
└── site/                     # Hugo landing site (deployed to GitHub Pages)
```

| Layer       | Choice                                          |
| ----------- | ----------------------------------------------- |
| Shell       | Rust + Tauri **2**                              |
| Frontend    | SvelteKit **2** / Svelte **5** (runes), Bun     |
| Styling     | Tailwind CSS **4** (CSS config) + design tokens |
| Media       | FFmpeg binaries (always) · `ffmpeg-next` **8.1** (optional libav) |
| Persistence | `rusqlite` (bundled SQLite) — one `.kerf` file  |
| MCP         | `rmcp` **1.7** (streamable-HTTP transport)      |

The engine's export and timeline-still paths are **pure and unit-tested** — clip
positions, gaps, track layering, effects, keyframes, overlays, ducking and loudnorm all
render from data. See [`CLAUDE.md`](./CLAUDE.md) for a deep tour of the internals.

---

## Status

Kerf is a working editor under active development — not a scaffold. The timeline,
preview, transcript, analysis, effects, keyframes, captions, playback and export are all
wired to real backend state, and the MCP surface is exercised end-to-end.

**Roadmap:** a live activity stream pushed from the MCP server (the queue is polled
today), richer staged-edit diffs in the review step, and auralized effect chains in
preview playback.

Contributions welcome — see [`CONTRIBUTING.md`](./CONTRIBUTING.md).

## License

[PolyForm Noncommercial License 1.0.0](./LICENSE.md) — free for noncommercial use.
</content>
</invoke>
