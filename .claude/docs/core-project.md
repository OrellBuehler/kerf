# kerf-core: `project.rs`, `analysis.rs`, `error.rs`

- `project.rs` — `Project` wraps a `rusqlite::Connection`. **Persistence shape:**
  `assets` and `analysis` are real tables (streams/analysis stored as JSON columns);
  the **entire timeline is a single JSON blob** in a one-row `timeline` table. All
  edits go through `edit_timeline(|tl| ...)` which loads → mutates → saves the blob.
  **Ripple mode** is a project flag (`ripple_mode` / `set_ripple_mode`, in `meta`
  like `speech_model`: persisted with the file, default off, not an edit) that
  `edit_timeline` honors for every op — it snapshots the timeline, runs the op,
  and stores `after.ripple_from(&before)`, on the staged path too (so the review
  diff shows the clips that followed). It is applied there, not per op, so a new
  op ripples with no code of its own; with the flag off nothing is cloned and
  nothing changes. `Project::with_ripple(Option<bool>, |p| ..)` forces it on/off
  for the calls inside (`None` inherits) — how a tool takes an optional `ripple`
  argument; `ripple_active()` is the effective answer. The ops that decide their
  own layout go through `edit_timeline_exact` and never ripple: `ripple_delete`,
  `cut_clip_range`, the beat snap, `reorder`, `move_clip(s)`, `insert_clips`.
  `trim` re-reads its clip afterwards because a ripple can move it. A forced-on
  `remove_clips` is the multi-select ripple delete. `move_clips` / `remove_clips`
  are single revisions (`Move N clips` / `Remove N clips`), and — unlike the
  single-clip ops, which leave locks to the GUI — refuse clips on a locked track.
  `roll_edit` / `slip_clip` / `slide_clip` (`Roll edit` / `Slip clip` / `Slide
  clip`, one revision each, `source_limits()` read before the edit) are
  `edit_timeline_exact` — they move no length, only where footage changes hands;
  `split_remove` (`Split and remove left|right`) goes through `edit_timeline` and
  follows the mode, re-reading its clip like `trim`; `split_remove_clips` is that
  for a group — one revision (`Split and remove left (2 clips)`), the single-clip
  `split_remove` being a group of one. All of them refuse a locked track.
  **Two counters tell the app that the project under a job is not the one it left.**
  `generation()` moves when `replace_with` swaps a project in (open / new): a job notes it
  when it resolves its inputs under the lock and drops its result if it changed. **`replace_with_saved_copy`
  is the one swap that does not move it** — Save As reopens the copy it wrote, which is the same project in
  another file, so jobs in flight (analysis, import, voiceover, proxies) still land; the change count carries on
  across it too. Only that swap may use it. Both swaps **return the project replaced** (`#[must_use]`;
  `agent_review` carries over): the caller `close()`s it after releasing the lock (see crash recovery). `change_count()`
  is SQLite's `total_changes` — every row written through the connection (analysis merges,
  ripple mode, meta, a declined proxy …), so "did anything write while the lock was released?"
  is two numbers, not a content fingerprint. It is **monotonic across `replace_with`** (the swapped-in
  project's connection restarts at zero; the count carries on from the one it replaced), so a number
  noted under the old project can never equal one read under the new — `generation()` still says
  *which* project. `merge_analysis` / `set_analysis` (and
  `place_stems`) answer `AssetNotFound` for an asset that is gone instead of writing for nothing.
  **`save_as` is atomic** (`store::vacuum_into_atomic`, below): a failure leaves the destination as
  it was (it used to be deleted first), and saving onto the file the project is open on is a no-op; the
  destination is held to `check_output_path` first (never over an asset's source, a stitched capture, a
  proxy or the cache). **`insert_asset` is an upsert** (`ON CONFLICT … DO
  UPDATE`), never `INSERT OR REPLACE`: replacing deletes the row first and `analysis` hangs off it
  with `ON DELETE CASCADE`, so marking an asset 360 used to drop its transcript.
  **The media library** is filesystem work (stat, ffprobe, reading file ends), so each op is split to
  keep it out from under the project lock: an `_inputs` read under the lock (database only), a
  static fn that touches files with the lock released, an `apply_*` under it again — the
  `*_inputs → static → apply_*` shape of every media job. `missing_assets_inputs()` →
  `Project::find_missing_assets(files)` (a stat per asset; `missing_assets()` is the two behind
  `&self`, for a caller with no lock). `relink_inputs(id)` (the stored asset) →
  `Project::probe_relink(&old, path, cancel)` (probe, compatibility check, then **the old file's
  size and a SHA-256 of both files' first and last MiB** — `paths::same_content`, `None` when the old
  file is gone) → `apply_relink_probed(id, &RelinkProbe)` (no file touched; refused when the entry's
  path is no longer the one probed against, "try again"); `relink_asset(id, path)` is all three plus
  `forget_media_caches`, and `apply_relink(id, &Asset)` (an already-probed asset, stat-only size
  compare) stays as the pre-split shape. **`Project::check_relink(&old, has_analysis, path, cancel)`** is the dry run
  (`probe_relink` + `RelinkProbe::keeps_analysis(&old)`, the rule the real relink applies) → `RelinkCheck
  {analysis_kept, has_analysis}`; `has_analysis(id)` is the locked read. Same kinds of stream, length within
  `RELINK_DURATION_TOLERANCE` s or 1 %; **analysis is kept only for the same media moved** — same
  length to the millisecond, same streams, and, when the old file was there to compare, the same size
  *and* ends (an old file that is gone leaves length + streams to decide); otherwise it is dropped. A
  hand-set projection, the id, name and clips carry over. `remove_asset(id, force)` (`AssetInUse` while
  a clip of the live cut or the proposal uses it; `force` removes those clips as one never-rippling
  `Remove media` revision and strips the proposal and a kept discarded one). **Both are the user's,
  and an agent's call is refused outright** — `AgentReviewRequired` with review on, an
  `InvalidArgument` with it off (a session could be open, and a forced removal would then edit the
  proposal while the asset row left the live cut). **History does not bring removed media back**:
  `restore` (undo / redo / revert) refuses a snapshot with a clip of an asset that is no longer in
  the library — naming it, from the `removed_assets` meta list (`[id, name]`, newest 200) — rather than
  put clips of nothing on the timeline; steps from before the media was used still restore. Caches keyed
  by the file's path / size / mtime are cleaned by the static `forget_media_caches(path)`, which
  probes and so runs with the lock released.
  **Paths**: `probe_asset` / `probe_asset_with(path, cancel)` (so every import) want an absolute path to
  an existing regular file and refuse `http:` / `concat:` / `pipe:` strings and relative paths
  (`paths.rs`); `check_output_path(out)` refuses an export onto an asset's source, a stitched
  capture file, the project database and its journal, or anything inside Kerf's cache, canonicalised so
  a symlink, hard link or `..` detour cannot hide it — `export`, `export_with`, `export_still` and
  `save_as` call it. It canonicalises and stats, so an adapter that renders with the lock released
  takes `output_path_inputs()` (a `paths::Protected` list, database + environment only) under the lock
  and runs the static `Project::check_output_path_with(out, &protected)` after releasing it.
  `history_page(Page)` / `list_tasks_page(Page)` window a long list from the newest end
  (`Page { limit, offset }`) and return the total.
  **What a setter accepts is checked where it is set**: a non-finite number is refused by every
  numeric setter and, as the net under all of them, by the serde walk every stored timeline
  goes through (`core-model.md`); `set_volume` clamps to `MAX_CHANNEL_VOLUME` like the track and
  master faders; overlay / marker / chroma-key colours go through `valid_color` when set;
  audio effect parameters are held to the ranges of the FFmpeg filters they become (`acompressor`:
  threshold −60..0 dB, ratio 1..20, attack 0.01..2000 ms, release 0.01..9000 ms, makeup 0..36 dB;
  `agate` ≤ 0 dB; EQ gain ±900 dB; frequencies above 0 up to 999999 Hz — an `#[ignore]`d test runs
  every boundary through the real ffmpeg), video effects likewise (`gblur` 0..1024, `unsharp`
  −2..5, `chromakey` similarity ≥ 1e-5); a clip added or trimmed stays inside its footage
  (`source_in ≥ 0`, `source_out ≤` the asset's length, a still has no end), the new/trimmed clip
  may not land on another clip of its lane (checked after the ripple, so ripple mode still
  pushes the later clips along), and `add_clip_to_timeline` keeps the track sorted;
  `set_speed` is held to `MIN_SPEED..=MAX_SPEED`.
  **The file** (`project/store.rs`): `PRAGMA user_version` is the schema version, brought forward by
  `MIGRATIONS` (one step each, each in its own transaction stamping the version it reached; never edit
  a shipped step, append one). Version 0 is every file written before versions existed (the legacy
  baseline; v0→v1 is the old probe-then-`ALTER` columns); v1→v2 adds `staged.owner` / `staged.assets`
  (below); v2→v3 adds the `fonts` table (below); v3→v4 adds `assets.music_score`; v4→v5 is a JSON step for
  `Fit::Blur` (`"fit":"blur"` in a delivery frame, a variant an older build fails on: `run` a no-op, `needed` always, so every
  older file is copied aside once — the no-op probes of the earlier steps still decide nothing on their own — and
  `needed_to_read` false; `persisted/timelines-v5.json` holds a blurred-fill cut). A step's `needed_to_read` says whether a build that
  cannot write must still refuse a file lacking what it adds: true for the v1, v2 and v4 columns (the reads
  name them), false for the `fonts` table (reads tolerate its absence), so a write-protected v2 project
  opens as it is. A new file is stamped `SCHEMA_VERSION`; a
  file newer than that is refused before anything is written (`Error::NewerProject`, "made by a newer
  Kerf", which the GUI toasts); an older file is first copied beside itself as
  `<stem> (schema vN backup).kerf` (kept if one already exists), then upgraded. Each step has a
  `needed` probe and a `destructive` flag: no backup when no pending step would change the file (only
  the version is stamped), and a backup that fails (full disk, read-only folder) is a warning unless a
  pending step is destructive. A **read-only** file (SQLite fell back to read-only) opens as it is,
  nothing written; one that needs a real upgrade is refused with a "copy it somewhere you can write"
  message. `SCHEMA` still runs first, so a step must tolerate tables it creates. **The version covers the
  JSON in the tables too** (timeline, history, proposal, analysis, asset streams): a change an older build
  would drop (an unknown field, at its next write) or misread (an unknown variant) is a step of its own —
  `run` a no-op (or a JSON rewrite), `needed` always (so the file is copied aside first), `needed_to_read`
  false. `persist_tests` keeps `persisted/timelines-v<N>.json` (the sample's timeline and five of the golden
  oracle's) for each version that moved the stored shape: every one must still read, and the newest must
  round-trip unchanged, so moving the shape fails until a step is appended and `KERF_BLESS_PERSISTED=1`
  writes the new version's file (add it to `PERSISTED`); a tables-only step needs no file. `meta.kerf_version` is the build that made the file, `meta.last_kerf_version`
  the last one that opened it to write. **Save As is atomic** (`vacuum_into_atomic`):
  `VACUUM INTO` a hidden temp file in the target's directory, fsync it, rename it over the target,
  fsync the directory on unix; any failure removes the temp file and leaves the old target as it was.
  Each save first deletes this target's own `.<name>.<uuid>.tmp` files over an hour old (a crashed
  save's). Saving a project onto its own file is a no-op. **The history is bounded**: `record_revision` keeps
  the newest `history_limit` revisions (`DEFAULT_HISTORY_LIMIT` 500, `KERF_HISTORY_LIMIT` overrides,
  `set_history_limit` per project, minimum 1 — the head is always the newest after a record), and
  never prunes a staged proposal's `base_seq` or anything after it. The oldest revision left is the new
  baseline: undo stops there and `revision_diff` of it is empty, like revision 0. `revert_to` /
  `diff_revisions` only reach revisions `history()` still lists. Stored JSON is not compressed.
  **Crash recovery** (`recovery.rs`): an untitled project can live in `<root>/<session>/untitled.kerf`
  (`Project::create_recovery`), its directory held under an OS lock (`File::try_lock` on
  `<session>/lock`, a separate file because Windows byte-range locks would block SQLite) for as long
  as it is open, and **unlocked explicitly** on drop (`SessionLock`): the lock belongs to the open
  file, which a child forked by another thread (`pre_exec` spawns fork) shares until it execs, so
  closing alone could leave a let-go session locked; `path()` stays `None`, so it still reads as unsaved. `recovery::scan(root, skip)`
  offers the directories whose lock it can take and that hold work (assets, a revision past 0, a task
  or a proposal — the `summarize` read opens read-write so SQLite can roll back a crash's hot
  journal; a table the file lacks counts as empty, so a 0-byte or half-created file is cleaned up),
  holding them locked as `Recoverable`; one it locked that holds nothing is deleted. A
  directory is **never** deleted unless this process holds its lock: `WouldBlock` (a running
  instance) and any other lock error (unsupported, permissions) or an unreadable project leave it
  alone. `Project::open_recovered` reopens one. `replace_with` / `replace_with_saved_copy` swap under the lock and **return** the
  project replaced (`#[must_use]`); the caller `close()`s it after releasing the lock, which deletes
  its recovery directory (it was saved, recovered into another, or let go). `close_at_exit` deletes it
  only when it holds no work (the same `summarize` rule) — anything else waits for the next launch.
  `Project::sample()` seeds an in-memory demo (two assets + analysis + a starter
  timeline + a sample task queue); it backs the kerf-core tests, but the app now
  launches with an **empty** untitled project (`Project::create_recovery`, see above) — the user imports media or
  opens a `.kerf` file to populate it.
  `analyze_asset`, `frame_at`, `waveform` and `waveform_range` delegate to the engine; editing ops are
  unchanged. `snap_to_beats(track_id, tolerance)` is "cut to the beat": it collects
  every asset's cached `Tempo`, builds the grid and aligns one track (or every
  unlocked video track) to it, defaulting the tolerance to half a beat so each cut
  moves to the beat it is already nearest; it errors when nothing rhythmic has been
  analyzed rather than silently doing nothing.
  `smart_crop(clip_id)` is "frame it for where it's going": reshaping a cut throws
  away most of one axis and both fits pick that axis blindly — `Cover` takes the
  middle, `Contain` letterboxes — so it samples where each shot's content actually
  sits and writes the crop that keeps it, **per clip**, as one `Smart crop` revision.
  Split three ways for the lock-free pattern (`smart_crop_inputs` under the lock →
  the static `sample_smart_crops` with it released → `apply_smart_crops` under it
  again); the result is an ordinary `Transform` crop, which the graph already applies
  *before* the fit scale, so the preview, the still and the export all follow and the
  inspector's sliders still have the last word. Clips already the delivery shape and
  360-reframed clips are left out (that camera *is* the framing decision), and a pass
  that changes nothing writes no revision.
  **One cut, every platform**: the same project can be delivered at several
  frames in one pass (a 9:16 Reel, a 1:1 post and a 16:9 upload), which is
  what exposed the tension in smart crop — its crop is baked into the
  transform for *one* shape, and framing for a second overwrote the first. So
  a clip carries **`Clip.framings`**, a crop per delivery shape (`Framing`,
  keyed by the reduced ratio `Delivery::ratio`, `(9, 16)`) beside the
  transform's, and **`Timeline::for_delivery(delivery)`** (pure +
  unit-tested) is the render of the cut at another frame: a copy whose format
  is that delivery and whose clips wear the crop they carry for its shape — the
  same change-the-timeline-not-the-graph pattern as `for_render`, so the graph
  builders never learned about it. A clip with no framing for the shape keeps
  the crop it has (never throw away a hand-made crop), which is why the framing
  pass writes an *identity* framing for a shot already that shape: a lookup
  miss would otherwise leave a 16:9 shot cut 9:16 delivering at 16:9 as the
  strip its 9:16 crop keeps. Generated captions are re-fit to the new aspect
  (`fit_size` again); typed titles are left alone. The framing pass is the
  smart-crop trio again for the *other* shapes — `framing_inputs(deliveries)`
  under the lock (the project frame's own ratio excluded, duplicates
  collapsed), the static `sample_framings` with it released (**one** salience
  decode per clip, a crop per shape from it — the map is a property of the
  shot, the crop of the frame), `apply_framings` under it as one `Frame for
  9:16, 1:1` revision that a re-run leaves alone — and `engine::render_variants`
  renders `ExportVariant`s (a `Delivery` + an output path; `ExportVariant::beside`
  names each file by shape, `cut-9x16.mp4`, an `x` because `:` is not a Windows
  filename character), each variant's `resolution` / `fit` taken from its
  delivery, reporting a `VariantProgress` (which file of how many plus the
  overall fraction). With `together` (the app's default, `single_pass`) every
  file comes out of **one ffmpeg run** (`engine/cli/together.rs`, see
  engine-cli.md): the sources decoded once and `split` into each frame's graph,
  the mix built once and `asplit` into each file, a stop removing every file it
  touched (`VariantProgress.together`, `variant` 0). Without it, or where one run
  cannot write them (two-pass, a hardware encoder, an audio-only container, two
  variants on one file name), they render **one after another**, and a cancel
  deletes the file in flight and keeps the finished ones.
  **Side-files of a delivery** (`project/exports.rs`): `delivery_inputs` snapshots the
  working timeline, the *original* assets, the render frame rate (`render_geometry`) and the
  project's name under the lock; the static `captions_document` (refused with no captions;
  it and `markers_document` read `for_render`, as the export does),
  `interchange_document` (OTIO / EDL, refused with no clips) and `markers_document` (CSV /
  YouTube, refused with no markers) build the text with it released, and `write_document`
  writes it. `edit_marker` / `add_marker_with` take a `MarkerPatch` as one revision.
  **Interchange** (`interchange/`, pure, no file I/O) returns text plus a `Report` (`Level`
  info / warning, identical messages merged with a count — FilmCraft's design). **OTIO**
  (`otio.rs`): every track (`enabled` = not muted), gaps, `Transition.1` from `transition_fx`
  (a dissolve or motion transition is `in_offset` 0 / `out_offset` the achieved overlap, a
  dip `d/2` either side, none written for a hard cut), `LinearTimeWarp` for speed, an
  `ExternalReference` `file://` URL to `Asset::path`, the timeline's markers on the stack, and
  `metadata.kerf` holding each clip's whole serde state, each track's mixer flags, the
  overlays, SVG titles, format and master. **EDL** (`edl.rs`): one video track
  (`video_track`, 0 = V1), record from `01:00:00:00`, source from the file's zero, frames at
  the rate (`Timebase`: NTSC rates exact, drop-frame at 29.97 / 59.94 with `;` before the
  frames), 8-character reels from the file stem made unique per file, `* FROM CLIP NAME` /
  `* TO CLIP NAME` / `* SOURCE FILE`, a dissolve as the outgoing `C` line plus the incoming
  `D nnn` (`BL` with no predecessor), dips written as cuts and slides / pushes as dissolves
  (reported), `M2` at speed × timebase (negative reversed) with the source out at
  `src_in ± record frames × |speed|` (counting down when reversed), disabled clips left out,
  and the sound of A/V clips reported as not written (events are channel `V`).
  Golden text in `interchange/golden/*.{otio,edl}` (`golden.rs`; bless with
  `KERF_INTERCHANGE_BLESS=1 cargo test -p kerf-core --no-default-features interchange`).
  **Marker lists** (`interchange/markers.rs`): `markers_csv` (RFC 4180, every cell quoted,
  free-text cells starting with `= + - @` / tab / CR — or one of the four after leading
  whitespace — prefixed with `'`), `youtube_chapters` (chapter markers, else all; `0:00 Intro`
  added when the first is later, fewer than three or shorter than 10 s reported, not
  enforced) and `ffmetadata_chapters` (chapter markers only, each to the next or its own
  duration, `= ; # \` and newlines escaped; `None` with none).
  The **agent task queue** is a real `tasks` table (one row per `Task`,
  columns not JSON): `add_task` / `list_tasks` / `claim_next_task` / `complete_task`
  / `fail_task` / `resolve_task` / `remove_task` drive the `queued → working →
  ready → done` (or `failed`) lifecycle in `model.rs`.
  **Agent edits are staged, not applied** — the thing that makes an agent safe to
  leave running on someone's cut. A one-row `staged` table holds a proposal (the
  timeline being built, the one it branched from, the edit labels, the task it
  belongs to); `edit_timeline` routes an edit into it whenever the actor is
  `Agent` and a session is open, so the timeline the user is looking at never
  moves under them. `begin_staging` opens one, `staged()` reports it *with its
  diff* and whether it went `stale` (the user kept cutting, so applying would
  replace their newer work — refused unless `apply_staged(force)`),
  `apply_staged` lands it as **one** revision attributed to the agent (an empty
  proposal just closes, rather than putting a no-op edit in the user's history)
  and `discard_staged` throws it away. `working_timeline()` is the read side:
  the proposal while the agent has one, the live timeline otherwise — every read
  an edit depends on goes through it (including the preview, still and export
  paths), so the agent can *look at* the cut it is proposing, and the GUI, which
  never stages, always sees the live one. `restore` (undo/redo/revert) refuses
  for an agent holding staged edits rather than walking the ground out from under
  them. The queue ties in at both ends: `claim_next_task` opens a staging session
  for the task, `resolve_task` applies it (accepting the task *is* accepting its
  edits) and `remove_task` discards it — **the proposal first, in one transaction with the delete**: an agent
  that does not own the proposal is refused `StagedEditOwnedByOther` with the task still in the queue (it used
  to delete the row and then fail). `diff_revisions` / `revision_diff` point
  the same diff at the stored history snapshots, so the edit log can say what an
  edit did rather than only which operation ran.
  **Review is the default (`set_agent_review`, on; the app sets it from Settings, it is not in the
  file and survives `replace_with`).** With it on, an `Agent`-actor edit with no session open
  *opens one* (in the same transaction as the edit, so a failing edit leaves no empty proposal),
  and an agent can neither land a proposal nor rewind the user's history: `apply_staged` (`force`
  included), `apply_staged_subset`, `resolve_task` of a task with staged edits, and
  `undo`/`redo`/`revert_to` as the agent are refused (`AgentReviewRequired` / an
  `InvalidArgument` for the history) — the actor on the lock *is* the "initiated by" argument.
  Off, an agent edits live unless a session is open. **A proposal has an owner** (the MCP
  session id, `staged.owner`; `StagedEdit` serialises only `owned_by_agent`, never the id): the
  adapter calls `ensure_agent_staging(owner)` inside the same lock as an agent edit — it **declares
  and checks and writes nothing**: it records which session is acting (`set_actor` forgets it at
  every lock) and answers `StagedEditOwnedByOther` to a different session (a caller that names nobody
  is "different" from an owner). The *edit* then opens the session (review on, none open) or lets a
  session with no owner (a claimed task's) be adopted by the first named editor, inside its own
  transaction — a call that opened it up front left an empty owned proposal behind whenever the
  edit then failed. Apply, discard and undo
  by a non-owner agent are refused the same way (the tool that only ends a proposal calls
  `declare_agent(owner)`, which records the session without opening anything); the user (GUI)
  is never refused. **A proposal whose session is gone is released**: `release_orphaned_staging(gone)` clears the
  owner when the adapter says the owning MCP session no longer exists (closed, expired, or the server restarted
  with a new token), so the agent that reconnected — or the next named session to edit — adopts it instead of
  being told another agent owns it; with no owner left, the ordinary adoption rule applies and the steps are kept.
  `set_ripple_mode` as an agent is refused while review is on (the flag is the user's toolbar setting and applies
  at once, to a proposal in progress too): the edit's `ripple` argument is how an agent ripples for one call.
  `claim_next_task(owner)` / `begin_staging(task, note, owner)` open a session for a given owner.
  **Asset-level agent writes are staged too**: `set_asset_projection` as the agent records a
  `StagedAssetChange` in the proposal (the `staged.assets` column) instead of rewriting the
  library, the agent's own `get_asset` / `list_assets` read through the pending changes (the
  user's do not), and accepting writes them (no revision — the library is not in the edit
  history); asking for what the library already has stages nothing.
  **Undo inside a proposal**: every step pushes the previous state onto `staged_undo` (a table, so
  an edit writes one timeline copy, not twenty), bounded at `STAGED_UNDO_DEPTH` (20) and emptied
  with the proposal; `undo_staged()` pops one — returning its label and the proposal —
  timeline edits and held-back asset changes alike. **Partial accept**:
  `apply_staged_subset(entry_ids, force)` takes ids from `staged.diff.entries[].id` (or an
  `asset_changes[].id`), writes them onto the live cut with `Timeline::merge_subset`
  (`core-model.md`) as one `Agent edit (k of n changes)` revision and reports what it did in a
  `SubsetApplied` (`auto_included` with reasons, `skipped`, `remaining`, `closed`, a
  `message`). The rest of the proposal **stays open, re-based onto the cut as it now is**:
  `base` and `base_seq` move, and the proposal's timeline is rebuilt as the live cut plus
  *exactly the changes not applied* (`Timeline::rebase_rest`, `core-model.md`) — not kept as it was,
  which after a forced accept lacks what the user did since and would diff as undoing it. A change
  that no longer fits the cut is dropped from the rest (counted in the `message`); it closes when
  nothing is left. **Discard is restorable**:
  `discard_staged` keeps a non-empty proposal in `meta` (`discarded_staged`, in the file, so it
  survives a restart); `discarded_staged()` previews it and `restore_discarded()` brings it
  back — only with no proposal open (`StagedEditPending`), once, possibly `stale`, with no undo
  history, owned by no session when the user restores it, with its task dropped if that was
  dismissed. **Who discarded it is kept** (`discarded_by_user` in the stored row): an agent may bring back
  only a proposal an agent discarded — one the user threw away stays thrown away unless the user restores it. An empty proposal is not kept and does not replace one that is. **The kept copy is dropped when it can only mislead**: when a newer proposal is
  really applied (`apply_staged`, `apply_staged_subset`; a session that changed nothing closing is not one), and when the project
  is replaced (`replace_with` — an open or new project; `replace_with_saved_copy`, Save As, keeps it). The `staged` table
  gained `owner` and `assets` (schema step v1 → v2 in `store.rs`, a probe-and-`ALTER` like `voiceover`'s
  v0 → v1, so a file at v1 is copied aside once as `(schema v1 backup)` before it is upgraded; an older
  file's open proposal loads with no owner and no asset changes).
  `plan_music_fit(clip, target)` / `fit_music(clip, target, fade_out)` fit a music clip on
  an audio track (normal speed, unlinked, with a cached `AssetAnalysis.music`) to `target`
  or, by default, the picture's end minus the clip's start; `fit_music` replaces the clip
  with `music_fit_clips` in one `edit_timeline_exact` revision ("Fit music to length") and
  refuses a result that would run into the next clip on the track.
  **Composed music** (`project/music.rs`, tests in `music_tests.rs`; the voiceover's shape again): an asset carries
  **`Asset.music_score`** (`MusicScore`, a `music_score` JSON column — schema step v3 → v4 in `store.rs`, a
  probe-and-`ALTER` like the others, so a v2 file is copied aside once as `(schema v2 backup)`), which is what lets a
  piece be edited and composed again and what its analysis is written from. **The request types refuse a misspelled
  field** (`#[serde(deny_unknown_fields)]` on `MusicScore`, `ScoreSection`, `ScoreHit`, `InstrumentOverrides`,
  `ComposeRequest`, `ScoreAlign`, `BandBalanceRequest`, `BandWindow`): `enrgy` is an error naming it, not a section
  with the default energy. The split: `compose_music_inputs(&ComposeRequest {score, track_id?, at?, align?,
  replace_clip_id?}) -> ComposePlan` under the lock — **everything that can be said wrong is said here, before the
  render**: the score resolves, the target track exists, is audio and is **not locked**, a replaced clip is on an
  audio track, of a **composed** asset, with no clip of the bed linked to a picture — then the music's start (the
  **earliest clip of the replaced bed**, else `at`, else the end of the track) and, with `align`, the **working
  timeline's** cuts (the boundaries *between* the clips of a video track, the picture's first start and last end being
  its edges) or markers go to `plan_score_to_picture`. **Last, the placement is rehearsed** (`check_music_fits`): a
  stand-in asset of the fitted score's length goes through the same `put_music` and then `ripple_lanes` (when
  `ripple_active()` — the project's flag or a `with_ripple` override around the call) and `check_own_lanes`, exactly
  what `edit_timeline` does, so a start (`at`) that lands on another clip of the track, or a longer recomposed piece
  that would run into the next clip, is refused **before the render** with edit_timeline's own words ("the clip would
  overlap another clip on M at …"); with ripple on a piece that only *pushes* what is there passes (a start *inside* a
  clip still fails: ripple never splits one). The render is never shorter than the score (`ceil` frames), so the
  rehearsal can pass what placing refuses by a frame, never the reverse. The plan holds the fitted score, the `AlignReport`, the
  request's own `track_id` / `at` / `replace_clip_id` **as given** (a `None` start is resolved when the music is
  placed, after what the track holds *then*), `replaces` (the clip ids of the bed, in time order; empty when a clip is
  added) and `music_start` (what the alignment used; informational) → static `synthesize_music(score, progress,
  cancel) -> Asset` (renders with the lock released, probes, names it `Music: <title>`) → `place_music(asset,
  track_id?, timeline_start?, replace_clip_id?)` under the lock: the score is resolved first (`rhythm`), then **the
  asset, its analysis and the edit are one transaction** — the store and the analysis run *inside* the `edit_timeline`
  closure, so a refused placement (a locked or bad target, a clip in the way, a bed linked to a picture) rolls back
  and stores no asset and no analysis, a staged agent edit included (an asset that was already in the library stays);
  the asset is stored (the same score is the same file, so the same asset), **its analysis is written from the score**
  (`Rhythm` kind: the exact grid as a full-confidence `Tempo`, the bars and phrases as `MusicAnalysis`, onsets, class
  music — nothing is analysed, so `get_music_structure`, `fit_music` and the beat snap work at once; other kinds an
  earlier pass cached stay), and **one `Add music` edit** puts the clip on the audio track named **`M`** (`MUSIC_TRACK`,
  made on first use like `VO`) after what it holds. An agent's edit stages through `edit_timeline` (the asset and its
  analysis land at once, the placement waits). **`replace_clip_id` replaces the whole bed**: every clip of the named
  clip's asset on its track — the segments `fit_music` made of one piece — is removed and **one** clip of the new
  render takes the place of the first, as one `Recompose music` revision: the named clip's id, the earliest start,
  the first clip's gain / fade-in / effects / keys and the last one's fade-out, `source_in` 0, `source_out` the new
  length, speed 1. It is registered with `edit_lanes` and goes through `edit_timeline`, so **ripple applies as to any
  edit** (the clips after the bed move by the difference in length) and without it a longer render that would run into
  the next clip is refused with nothing changed. `Composed.replaced` is the list of the clip ids that were replaced.
  **Band balance**
  (`band_balance_inputs(&BandBalanceRequest {asset_id? | clip_id? | range?, sections?}) -> BandPlan` under the lock →
  static `measure_band_balance(&plan, cancel) -> BandBalance` → `band_balance(&req)` for a caller with `&self`): exactly one
  target; times are the **target's own clock** (an asset's seconds, a clip's *source* seconds with its `volume` applied
  — its speed is not, a timeline's seconds for a `range` of the working timeline's mix); windows are the explicit
  `sections` (cut to the sound, each with ≥ 0.25 s), else a composed asset's **score sections** (named, numbered when a
  kind repeats), else the analysed bar grid in four-bar phrases (eight, sixteen … while there would be more than 16),
  else eight equal windows; `windows_from` says which. `BandBalance` carries the windows, the whole stretch as
  `overall` and `notes` (a heavy low end, a thin mid range, a dull top, a loudness swing, a window that is bassier than the
  whole). `analysis.rs` serves a composed asset's rhythm from `ScoreRhythm` (below the providers) instead of
  `FfmpegRhythmAnalyzer`.
  `separate_stems_media(asset, progress, cancel)` (static, lock released) separates and
  probes the four stems; `place_stems(source, stems, clip?)` stores them (reusing
  imported ones) and, with a clip, lays each on a new audio track named after it at the
  clip's span/speed/gain and switches the clip's own sound off (audio clip disabled,
  picture `source_audio` off; a picture whose sound is on a linked clip is refused).
  **Animated SVG titles** (`add_svg_overlay(SvgSource, start, end?, SvgLayout)`,
  `update_svg_overlay(id, SvgOverlayPatch)`, `remove_svg_overlay(id)` — each one
  `edit_timeline` revision, so an agent's stage; see `core-model.md` for the model).
  **Parsing costs up to ~1.7 s, so a surface does it before it takes the project lock**:
  `SvgSource::check() -> Result<SvgChecked>` (`Custom(SvgDoc)` / `Preset`, params checked) then
  `Project::add_svg_overlay_parsed(SvgChecked, start, end?, SvgLayout)`, and `SvgOverlayPatch::check() ->
  Result<Option<SvgDoc>>` (exclusivity, params, finite times, the artwork parsed) then
  `Project::update_svg_overlay_parsed(id, patch, doc)` (a `doc` replaces the artwork and `patch.svg` is not
  parsed again; with `None` a `patch.svg` is still parsed, which is all `update_svg_overlay` is). The plain
  `add_svg_overlay` / `update_svg_overlay` are the check plus the parsed one and are `#[cfg(test)]`: no surface may
  parse under the lock, and none does (`lib.rs` and `mcp.rs` call the `_parsed` pair). `start` and `end` are held
  to `MAX_SVG_SECONDS` (24 h) on add and update (`check_svg_span`; `SvgOverlayPatch::check_cheap` refuses it before the lock).
  `SvgSource` is `Custom(String)` (parsed through `SvgDoc::parse` *before* the edit closure, so a
  bad document, a root that is not `<svg>`, no size or a cap exceeded is an `InvalidArgument` and
  writes no revision) or `Preset(SvgPreset)` (`svg::check_params`, then generated inside the
  closure for the *working* timeline's `preset_frame()`). Both return `SvgOverlayResult {overlay,
  warnings}` (what the sanitiser / SMIL reader could not honour). The end defaults to the
  document's `natural_duration` held to 2–30 s, else four seconds; placement is checked finite
  and sane (`check_svg_rect`; opacity 0..=1); a 51st title is refused; `Error::SvgOverlayNotFound`.
  An update's `svg` converts a preset to custom, its `preset_params` re-fills a preset and its
  `preset_kind` makes it another kind (both refused on custom art; `svg` is exclusive with either;
  both together are one edit; the id, name, timing, placement and draw order stay), and **a preset
  is re-made whenever its length changes** — a move that keeps the length keeps the artwork.
  `SvgDoc::default_span` (natural duration held to 2–30 s, else 4 s) is the end an add defaults to,
  also what `preview_svg_overlay` takes as a draft's length. `set_delivery_format` re-makes every preset for the new frame.
- **Motion templates** (`template/`, `project/templates.rs`; plan: `.claude/plans/motion-templates.md`). A
  template is an animated SVG title packaged with the fonts it needs and **controls** (a tagline, a brand
  colour, a logo) that change it without opening the artwork; it lives in a per-user library and is
  applied to any project. `SvgOverlay.template: Option<TemplateInstance>` (`template_id`, `name`,
  `controls`, `values`; absent from the JSON when `None`, so a project without templates stores the bytes it
  always did) is all a title remembers; **its artwork is its own `svg`** (a value is spliced into it, never
  kept beside it), and **`values` is read back from that `svg`** after every change (so it cannot drift;
  `image` controls are left out, their `data:` URL being in the `svg` already). A project never reaches
  back into the library: applying copies the SVG into the title and the fonts into the project.
  * **Format** (`archive.rs`): `.kerftemplate` is a zip of `manifest.json` (`format: "kerf-template"`,
    `version: 1`, `id` a lower-case UUID, `name`, `description`, `duration`, `width`, `height`,
    `controls`, `fonts`), `title.svg`, `fonts/*.ttf|otf` (≤ 8 MiB each, ≤ 16) and `poster.png`. Read into
    memory **by known names only** (never extracted: no entry name becomes a path); first the central
    directory is checked — ≤ 64 entries, every name plain (no `..`, `/` start, `\`, drive, NUL) and
    unique, no encryption, **the declared sizes summed ≤ 32 MiB** (a zip bomb deflates to kilobytes, so
    the declared size is what counts), no entries sharing bytes (`has_overlapping_entries`) — then each
    entry is capped (manifest 256 KiB, title 128 KiB, poster 4 MiB, font 8 MiB) and must be exactly as
    long as its header says. Unknown entries are ignored. The manifest's `format` and `version` are
    looked at before the rest is parsed: a newer version is refused as "made by a newer Kerf", an older one
    is brought forward by `model::UPGRADES` (`UPGRADES[i]` rewrites the JSON of version `i + 1` into `i + 2`;
    `VERSION` is `1 + UPGRADES.len()`; append one per format change) as it is read — a library file keeps its
    bytes. **A manifest field this build does not know is refused** (`unknown_field`, by its path, a `null`
    counting as absent) rather than dropped, so a format change must bump the version. A font must be a plain sfnt (`fonts::check_font`: not a `.ttc`, `fontdb` can parse it)
    and its manifest family becomes the family the file really is; **one whose `OS/2` `fsType` does not
    allow embedding (`fonts::embedding_allowed`: restricted licence 0x0002, preview & print 0x0004, bitmap
    only 0x0200) refuses the whole file** — when a file is read, so `import` and `get` both — as a font that
    cannot be read does: `save` leaves such a font out with a warning, and a file that carries one anyway
    was made elsewhere, so dropping it would hand back a template that draws other than the one sent. A poster that is not a PNG under
    2048 px is no poster. `to_bytes` is deterministic (fixed timestamps and order). The library is
    `dirs::data_dir()/kerf/templates/<id>.kerftemplate` (`KERF_TEMPLATES_DIR` overrides, read on every
    call).
  * **Controls** (`model.rs`, `validate.rs`, `values.rs`; JSON snake_case, mirrored in `types.ts`): `kind`
    `text` / `color` / `number` / `font` / `image`; `targets: [{element: <id>, attribute?}]` (`attribute`
    required except for `text`; a colour writes one of `fill stroke stop-color flood-color
    lighting-color color`, a font `font-family`, an image `href` / `xlink:href`; never `id`, `class`,
    `style`, a link or `on*` / `xmlns*`); `min` / `max` on a number only. Control ids are
    `[A-Za-z][A-Za-z0-9_-]{0,63}` and unique; at most 64 controls, 16 targets each. **Save-time
    validation** (`validate_controls`, run by `Template::validate` on the sanitised title): every target
    element exists exactly once, has the attribute (a `text` control aims at a `text` / `tspan` /
    `textPath`, an `image` one at an `<image>`), no two controls write the same place, and a text
    control's element holds no other control's target. **Values** (`apply_values`, pure, partial — only the
    named controls change; an unknown id is refused): text ≤ 2,000 characters, no control characters (`\r\n` and a lone `\r` become `\n` first; **any** value over the
    title's 128 KiB is refused before it is decoded or copied to a target);
    colour `#rgb[a]` / `#rrggbb[aa]` / a name `svgtypes` knows (bare letters, not
    `engine::valid_color`: that is FFmpeg's table, which has no `transparent`) / `none` / `currentColor` (no `@`, no `0x`, no function); number finite, within the bounds, written in plain
    decimal (**unitless**: `5px` is refused); font ≤ 64 characters of letters, digits, space, `- _ .`
    (an unknown family is a *warning*, the title draws in Geist); image a `data:` URL that passes
    `images::check_href` (the 16 Mpx header rule). Values are **spliced by byte range** (`svg::splice`,
    roxmltree ranges; text escaped `& < >`, an attribute escaped for its own quote; two edits on the same
    bytes are an error). **A `text` control sets the element's own text, not its contents**: the value goes
    into the first run of direct text (text and CDATA between the element's children), the other runs are
    emptied, and every child element, comment and processing instruction stays — a SMIL `<animate>` /
    `<set>` or a styled `<tspan>` inside the element survives (an element with no text gets it just before
    its end tag, `<x/>` is opened). `read_values` reads those same direct runs, so the value shown is the value
    a set writes (a word inside a `<tspan>` is its own control's). **The result then goes through
    `SvgDoc::parse`** (the sanitiser has the last word). An animation that animates the same attribute keeps its own values: a control sets the base.
  * **Lock-free shape.** The zip, the fonts, the sanitiser and the poster are all outside the project lock.
    Apply: `TemplateLibrary::get(id)` (or any `Template`) → `Template::prepare(&values)` →
    `PreparedTemplate` → under the lock `Project::apply_template_parsed(prepared, start, end?, SvgLayout)`.
    The end defaults to `start + manifest.duration`; `SvgLayout` places it (the whole frame) and may
    rename it (the template's name); one `Add template title “Name”` edit through `edit_timeline` (an
    agent's is staged). Set values: `Project::template_edit_inputs(overlay)` (reads
    `working_timeline()`) → `TemplateEditInputs::prepare(&values)` → `Project::set_template_values_parsed`,
    one `Set template values` edit that **refuses a title that changed since it was prepared**. The
    controls a title stored are checked again (`validate_controls_shape`, in `template_edit_inputs` and in
    `prepare`) before they are trusted to say where a value goes, and **no values is an `InvalidArgument`**
    (`prepare` refuses it) rather than a revision that changed nothing — every surface gets that one answer. The
    plain `apply_template` / `set_template_values` are `#[cfg(test)]`, like `add_svg_overlay`. An
    `update_svg_overlay` with `svg` replaces the artwork and so **ends the template** (the controls point
    into the old one); a rename or a move keeps it. The diff says `Tagline “A” → “B”` for a values change.
  * **Library** (`library.rs`, app-level — not a timeline edit, not undone, not staged): `list()` reads only
    each manifest, sorts by name, and returns the files that did not read in `problems` instead of
    failing; a file is only listed if its name is its manifest's id **and its extension is exactly
    `.kerftemplate`** (`path_of` writes no other, so what is listed is what `get` / `delete` / `export` reach);
    it also removes a `.kerf-tmp-*.tmp` file older than ten minutes (the remains of a write that never
    finished). `save(&TemplateSource, request)`
    (`Project::template_source(overlay)` reads the source under the lock; the rest needs no project) builds
    a template (`save.rs`): the controls validated against the title, the size from its `viewBox`, the fonts
    its `font-family` values resolve to (attributes, `style`, `<style>` rules and SMIL `set` / `animate`;
    each family at the weights and slants the document names plus regular, through the **same font
    database the renderer uses** so a project's own embedded fonts are found; Geist and the CSS generic
    families are never embedded; a face over 8 MiB, in a `.ttc`, not installed or flagged **restricted
    embedding** in its `OS/2` `fsType` is left out with a warning) and a poster drawn at half the
    duration with exactly those fonts (`fonts::svg_fontdb_with`). `import(path)` reads and **validates the
    whole file** (the title re-sanitised, the controls checked against it, a missing poster drawn), then
    writes a fresh canonical file — the library never keeps the bytes of the original; an id the
    library has is `already_present` when the content is the same, else the import is kept under a new
    id (`added_as_copy`), never overwriting; a damaged entry of that id is replaced. **The copy's id is made
    from its content** (a version-8 UUID of the template's digest), so importing the same file again finds
    its copy and says `already_present` instead of adding a third. `export(id, dest)`
    copies the library file atomically (a directory gets `<name>.kerftemplate`, a name Windows reserves for
    a device — `con`, `nul`, `com1` ... — gets an underscore in front). Every write is a temp file beside
    the target under a random name, opened `create_new` (a file or link planted there is never written
    through), fsync, rename. `delete(id)`; ids are checked as UUIDs before they become a file name. **The
    library is bounded** — `MAX_TEMPLATES` (500) files and `MAX_BYTES` (2 GiB) in all, checked by `add`
    (so by `save` and `import` too; replacing an entry never grows the count) with an error that says to
    delete some.
  * **Embedded fonts** (`fonts.rs`, `project/templates.rs`): the `fonts` table (`sha256` primary key,
    `family`, `data`; schema v3) holds what the project embeds; `apply_template_parsed` stores a template's
    fonts in the edit's transaction (a font already stored is skipped without being written; a row whose
    bytes do not hash to its key is skipped on read) up to 128 MiB. **A font stays while a title names its
    family and not longer** (`collect_unused_fonts`): a font is kept when the source of a title on the live
    timeline *or on the open proposal* names its family (`fonts::names_family`, a case-blind substring: a name
    in a caption costs one font too many, never one too few), and the rest are deleted **when a proposal is
    discarded (or closed with nothing left of it), on Save As (the project saved and its copy) and when a project is opened** — nowhere else, so
    undo can still bring a deleted title's font back until one of those. Once collected a font is gone: undoing
    to a revision whose title used it, or restoring a discarded proposal, draws that title in Geist until the
    template is applied again. That is what stops a template an agent applied and the user turned down from
    keeping the 128 MiB for ever.
    **Opening a project registers nothing**: `Project::open` only reads its fonts into memory
    (`Project.embedded`), because an opened project may never become the open one (the user cancels,
    `open_project` is refused for unsaved work, Save As's copy fails to swap). **The process's set is replaced
    at the swap** (`replace_with` / `replace_with_saved_copy` → `activate_fonts`, from what the project holds,
    so no table is read under the lock; `activate_fonts` is public for a program that opens one project and
    never swaps), and applying a template adds to it (`register_embedded`); each moves the plan epoch when the
    set changed. The registry is **process-wide** (per thread under `cfg(test)` in kerf-core), so tests in another
    crate that open projects on parallel threads and look at fonts must serialise.
    **A title is drawn with the fonts it names**, not with everything registered: planning a title takes
    `fonts::snapshot_for` (a `FontSet`: the registered fonts whose family the title's source names, with their
    digest and, built on first use, their database), and that one value keys the pictures *and* draws them
    (`SvgSeq::fonts`, `StillSvg::fonts`), so the key cannot name another set than the pictures were drawn with
    however the registry moves between planning and drawing. So a template font held for a proposal's review
    is in no key and no picture of a title that does not name it. **Residual:** a *live* title that names the
    same family (an installed Arial, say) is drawn with the proposal's embedded Arial while the proposal is
    open, since embedded faces win over installed ones and nothing marks a font as the proposal's; it is the
    same pixels as accepting it, and the font goes if the proposal is discarded. The registry's lock is held
    only to swap an `Arc`, never across a build of the font database (a system scan and every face parsed:
    seconds), so `replace_embedded` / `embedded_digest` and the swap under the project lock never wait on
    one. See `engine-cli.md` for how the SVG font database and the picture cache keys follow.
- `analysis.rs` — transcription / scene / silence / rhythm are **pluggable traits**
  (`Transcriber`, `SceneDetector`, `SilenceDetector`, `RhythmAnalyzer`). Real impls
  now exist:
  `FfmpegSilenceDetector` / `FfmpegSceneDetector` (CLI engine, always available —
  scene detection decodes hardware-accelerated and scores at 640px, the metric
  being resolution-normalized; **from the asset's ready proxy** instead when
  `Project::scoring_proxy` finds one, see below), `FfmpegRhythmAnalyzer` (onsets + tempo +
  speech/music class from **one** PCM decode — they used to be three traits, each
  re-decoding the whole file), `WhisperFilterTranscriber` (the ffmpeg `whisper`
  filter, always compiled) and
  `WhisperTranscriber` (in-process, `whisper` feature); `NullAnalyzer` is still the
  fallback. `Transcriber::transcribe` takes a `ProgressFn` *and* a `CancelFn` —
  alone among the providers, because it can download a model and then run for
  minutes, which is both the only step worth reporting on and the only one worth
  being able to give up on.
  **Analysis runs in steps, one kind at a time.** `AnalysisKind` (`silence`, `scenes`,
  `loudness`, `rhythm`, `transcript`; `parse_list` takes the names an agent tries and `all`) is
  what an analysis is made of: `analyze_asset_steps(asset, kinds, progress, cancel)` runs only
  those, in pass order, and returns a `StepsRun` — a **patch** holding what the finished steps found
  (with those kinds in `patch.ran`), the steps that failed with why, and whether it was cancelled.
  `Project::merge_analysis(&patch)` folds it into the cached `AssetAnalysis` and leaves every other
  kind as it was (`AssetAnalysis::merge`); a step that is cancelled or fails caches **nothing**, a
  failed step no longer discards the ones that finished, and a run where every step failed is an
  error. `AssetAnalysis.ran` records which kinds completed, the only way to tell "ran and found no
  silence" from "never ran"; an analysis cached before it existed (no `ran`) counts a kind as done
  when it has data (`AssetAnalysis::done`, mirrored in `analysis-steps.ts`). `analysis_status`
  reads one `AnalysisState` per kind — `done`, `running`, `failed` (the session remembers the last
  failure per asset+kind, cleared by a later success), `off` (switched off in Settings, the master
  off, or no speech backend) or `not_run` — and `Project::analysis_statuses` all assets.
  `AutoAnalysis` (master `enabled` + one flag per kind) is the process-wide set behind
  Settings › Analysis; it replaces the old transcribe switch, which swapped in the null transcriber for
  *every* pass. Now the step set decides: `analyze_asset_steps(None)` runs `default_kinds()` (the
  per-kind toggles, minus transcript without a backend), a named list runs exactly that — so an
  agent cannot fetch a speech model for someone who turned transcription off, and a user can still
  transcribe one clip by hand. Transcribing with no backend is a failed step, not an empty
  transcript. **Word timings and speaker labels are part of the `transcript` step**, not a sixth
  kind: no model, one 16 kHz decode and a few seconds of DSP per hour next to minutes of inference,
  so they always run with it (reported as `transcribe` with the detail "timing words and labelling
  speakers") and per-kind status stays five kinds — `transcript` done means words and speakers were
  attempted. A transcript cached before (no `words`, no `speaker`) still reads as done; readers
  estimate its words on the fly (`transcript::words_of`), and `refine_transcript` adds them without
  re-transcribing: `Project::transcript(asset)` under the lock → static `refine_transcript_media` →
  `Project::store_transcript(asset, read, refined)` (a merge with `ran = [transcript]`, refused when the
  cache no longer holds `read`, so a refinement never overwrites a newer transcript). `speaker_ranges(asset, speaker?,
  join_gap)` (errors naming the known speakers for an unknown one, or when nothing is labelled) and
  `filler_words(asset, fillers)` (default um / uh / … / you know, phrases matched across words of one
  line) read it by word bounds. **What a request runs** is `resolve_steps(asset, requested)`: a file with no audio is
  not asked for silence, loudness, rhythm or speech (silent b-roll used to get a failed transcript
  chip and an error toast on every import), a still has nothing to analyze, and an empty answer is
  an `InvalidArgument` that says why (everything switched off; a silent file asked for its speech)
  rather than an empty run. `analyze_asset_steps` runs in the background lane of the heavy-job queue
  (`engine-cli.md`), announces `waiting`, can be stopped while queued, and hands the patch so far to
  `on_step` after each finished step so the adapter merges it then. `Project::analyze_asset` is the
  convenience over all of it.
- `proxy.rs` — preview proxies as something the user can see and steer. Settings the engine reads
  (`ProxySize`: 0 / 720 / 1080 / 1280, lenient on read; **a change of size cancels every queued or
  building proxy** — they are at the old width — and the adapter queues what is missing at the new one; `PreviewSource`: `auto` / `original` /
  `proxy_only`, unknown reads as `auto`; size 0 forces the original), `ProxyStatus` per asset
  (`not_needed` for a still or audio-only, `off` with a reason, `missing`, `queued`, `building`
  with a fraction and ETA, `ready` with bytes, `failed` with the reason), and the queue: `queue_auto`
  (an import or project open — skipped for stills, when proxies are not wanted, for a deleted
  proxy and for one already on disk), `rebuild`, `delete`, `cancel_all`. **The registry holds only
  what is in flight** (queued / building / failed this session); *ready* is the file on disk —
  what `ready_proxy` and the preview themselves ask — so a status cannot claim a proxy the preview
  would not use. Each submission has a generation; a worker only touches its own entry, never the one
  a rebuild put in its place. The adapters pass a `Notify` that turns each change into the
  `proxy-progress` event. A job stays `Queued` until it holds the machine's slot (its first report is
  0.0), and a *manual* Rebuild's progress shows under Always original, which builds none by itself.
  The queue has one worker (`KERF_PROXY_WORKERS` overrides): the encode is x264 on the cores' one slot, so more
  workers would only be encodes already past their ffprobe. A worker's per-job run is `catch_unwind`-guarded (`run_guarded`): a panic marks that job `Failed`
  and the worker takes the next one. A deleted proxy is remembered **in the project** (`Project::proxy_declined`,
  meta key `proxy_declined`, a list of asset ids; not an edit) so the next open does not build it
  again; a rebuild forgives it. Under **Proxy only** `Project::proxy_waits(from, to)` names the clips
  in range with no ready proxy; the GUI preview commands refuse to decode the original for them, an
  agent's reads do not ask. Export never reads a proxy whatever any of this says.
  **Analysis takes a ready proxy too** (`Project::scoring_proxy` for `FfmpegSceneDetector`,
  `Project::sheet_proxy` for `decode_contact_sheet` / the `skim_asset` sheet; pure
  `resolve_analysis_proxy`): the same `preview_source` rule (Always original, or size Off, reads
  the original; a proxy still building is not waited for, analysis already queues behind the proxy
  lane) plus `engine::proxy_fidelity` — an MP4 / MOV / Matroska source for times, also SDR
  YCbCr (not RGB) for scene scores (`engine-cli.md` has the exact gate and the measured limits). A proxy that fails to read falls
  back to the original. Smart crop's `salience_map` is not swapped.
- `error.rs` — `Error`/`Result`; the `Ffmpeg(#[from] ffmpeg_next::Error)` variant is
  itself `#[cfg(feature = "ffmpeg")]`.
