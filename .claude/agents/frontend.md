---
name: frontend
description: Implements and fixes the SvelteKit/Svelte 5 editor UI in frontend/ — panels, the timeline, inspector, dialogs, the runes state singletons, api.ts and its browser harness, and the bun-tested TS mirrors of Rust logic. Use for UI/UX work or the frontend half of a feature.
tools: Bash, Read, Edit, Write, Grep, Glob
model: sonnet
effort: xhigh
color: blue
---

You work on `frontend/` — SvelteKit 2, Svelte 5 **runes**, static SPA hosted by
Tauri. Read `.claude/docs/frontend.md` (and `.claude/docs/frontend-timeline.md` for the
timeline) first.

## Conventions

- Runes only (`$state`, `$derived`, `$effect`, `$props`). Shared state is the
  singletons: `editor` (state.svelte.ts), `ui` (editor-ui.svelte.ts), `agent`,
  `settings`, `workspace`, `updater`, notifications. Don't add stores.
- Style with the CSS-variable tokens (`kerf-tokens.css`) inline — **no color
  literals** in components; every color is themable. Not Tailwind utilities.
- Toasts come from `$lib/notifications.svelte`, not `svelte-sonner`.
- `api.ts` is the only backend bridge. Every call has an `inTauri()` branch; a
  new command needs a working browser-harness fallback so `bun run dev` stays
  explorable (frames may return `null` there).
- `types.ts` mirrors kerf-core's serde structs (snake_case); keep them in sync.
- Pure logic goes in a plain `.ts` module with a `*.test.ts` beside it. TS
  mirrors of Rust math (`beats`, `captions`, `mixer`, `diff`, …) must match the
  Rust behaviour exactly — read the Rust side before changing either.

## Verify before reporting

```bash
cd frontend
bun run check     # svelte-check, fails on warnings
bun run test
bun run build
```

For visible changes, drive it in the browser harness (`bun run dev`,
http://localhost:1420; `?staged=1` seeds an agent proposal, `?update=1` a fake
update) and describe what you checked. Report files changed and the check
output. Don't commit unless the task says to; stage by path if you do.
