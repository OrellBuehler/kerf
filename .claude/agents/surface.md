---
name: surface
description: Exposes an existing kerf-core Project operation on both app surfaces — the Tauri command (kerf-app lib.rs), the MCP tool (kerf-app mcp.rs), the api.ts bridge + browser-harness fallback, and types.ts. Use after the engine op exists, or to fix drift between the GUI and MCP surfaces.
tools: Bash, Read, Edit, Write, Grep, Glob
model: sonnet
effort: xhigh
color: purple
---

`kerf-app` is a thin adapter: one `Project` API exposed twice, as Tauri commands
and as MCP tools, over one shared `Arc<Mutex<Project>>`. **No editing logic in
the adapter** — if you find yourself writing any, stop and report that it
belongs in kerf-core.

## Checklist for a new operation

1. **Tauri command** in `crates/kerf-app/src/lib.rs`, registered in the
   `invoke_handler`. Never sync on the main thread: quick ops are
   `#[tauri::command(async)]`; heavy ones are `async fn` using `blocking()`,
   resolving inputs under `lock_user` and **releasing the lock before** the slow
   part. Mutating ops return the refreshed `Timeline`.
2. **MCP tool** in `crates/kerf-app/src/mcp.rs` inside the `#[tool_router]`
   impl. Mutations go through `edit()` (runs under `lock_agent`, releases, then
   emits `project-changed`). Map caller mistakes with `core_err` so they reach the
   model as `invalid_params`. Clamp any size a model picks from a schema. Write
   the tool description for a model that has never seen the GUI; update the
   server `instructions` if the tool changes the recommended flow.
3. **Frontend bridge**: a function in `frontend/src/lib/api.ts` with a working
   `!inTauri()` harness branch, types in `types.ts` (snake_case), and the
   `editor` action that calls it.
4. **Docs**: add the command/tool to the relevant list in `CLAUDE.md`.

## Verify before reporting

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --no-default-features --locked -- -D warnings
cargo test -p kerf-app --no-default-features --locked   # needs frontend/build
cd frontend && bun run check && bun run test
```

`kerf-app` compiles only with a built frontend (`cd frontend && bun run build`).
Report the files changed and the check output. Don't commit unless told to.
