# Contributing to Kerf

Thanks for taking a look. Kerf is early and moving fast — issues, ideas and PRs are all
welcome.

## Ground rules

- **Add capabilities to `kerf-core` first**, then expose them in each surface (Tauri
  command *and* MCP tool). Keep editing logic out of the `kerf-app` adapter — it's a thin
  bridge over the `Project` API.
- **Keep the types in sync across the boundary:** `kerf-core` serde structs ↔
  `frontend/src/lib/types.ts`. Field names are `snake_case` in JSON on both Tauri and MCP.
- Match the surrounding style; `cargo fmt` and `cargo clippy` must be clean.
- License is **PolyForm Noncommercial 1.0.0**. New files inherit it via
  `license.workspace = true` — don't add other license headers.

## Dev setup

You don't need the FFmpeg **dev libraries** to work on most of the codebase — the engine
drives the `ffmpeg`/`ffprobe` **binaries** by default:

```bash
# Rust — verify / test without FFmpeg dev libs (works everywhere)
cargo check --workspace --no-default-features
cargo test  -p kerf-core --no-default-features

# Frontend (from frontend/)
bun install
bun run dev       # http://localhost:1420 — seeded sample data outside Tauri
bun run check     # svelte-check

# Full desktop app (needs FFmpeg dev libs for the default feature)
cargo run -p kerf-app
```

See [`README.md`](./README.md#building) for per-platform FFmpeg setup and
[`CLAUDE.md`](./CLAUDE.md) for a deep tour of the architecture.

## The landing site

The marketing site lives in [`site/`](./site) (Hugo). Screenshots and brand assets are in
[`docs/img/`](./docs/img) — a single source of truth shared by the README and the site.
To preview locally:

```bash
cd site && hugo server
```

The site auto-deploys to GitHub Pages on any push to `main` that touches `site/**` or
`docs/img/**`.

## Pull requests

- Branch off `main`, keep PRs focused, and describe the user-visible change.
- If you touch the export graph or timeline math, add or update the unit tests in
  `kerf-core` — those paths are pure and tested on purpose.
- Good first areas: new video/audio effects, MCP tool ergonomics, timeline UX, docs.
