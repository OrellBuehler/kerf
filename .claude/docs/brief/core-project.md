# core-project — brief

Rules only. The full file `.claude/docs/core-project.md` is authoritative and holds the measurements and the story behind each rule: grep it for the phrase in parentheses.

Subsystem: kerf-core `Project` persistence (`project.rs`, `project/*.rs`: `store.rs`, `batch.rs`, `exports.rs`, `music.rs`,
`sfx.rs`, `templates.rs`, `title_clips.rs`, `title_text.rs`, `tags.rs`, `stabilize.rs`, `thumbnails.rs`), `recovery.rs`,
`template/`, `interchange/`, `analysis.rs`, `proxy.rs`, `cache_cap.rs`, `error.rs`.

## Map of the full file (grep the bold lead)

- `project.rs` bullet: Persistence shape, Ripple mode, track locks, Several edits as one, generation /
  change_count, save_as,
  insert_asset, the media library (missing / relink / remove), Paths, setter validation, The file (schema
  steps), Save As atomic,
  history bounds, Crash recovery, sample, snap_to_beats, smart_crop, One cut every platform, Side-files,
  Interchange (OTIO / EDL),
  Marker lists, What changed without the lock, task queue, staged agent edits (review, owner, review off,
  flipping, asset-level,
  undo, partial accept, discard), music fit, Composed music, Band balance, Sound effects, Suggestions, stems,
  Animated SVG titles.
- **Motion templates**: Format, Controls, Text as glyph outlines, A text's box, Text animations, Curves,
  Lock-free shape,
  Library, Embedded fonts.
- **Titles on tracks**, **Asset tags**, **Stabilization**, `analysis.rs`, `proxy.rs`, `cache_cap.rs`,
  `error.rs`, **Thumbnails**.

## Persistence and edits

- The timeline is one JSON blob in a one-row table; every edit goes through `edit_timeline(|tl| ...)` (load,
  mutate, save)
  (`entire timeline is a single JSON blob`).
- Ripple is a project flag in `meta`, not an edit; `edit_timeline` applies it to every op (snapshot, run,
  `ripple_from`), on
  the staged path too, so a new op ripples with no code of its own (`It is applied there, not per op`).
- Ops that decide their own layout use `edit_timeline_exact` and never ripple: `ripple_delete`,
  `cut_clip_range`, beat snap,
  `reorder`, `move_clip(s)`, `insert_clips`, roll / slip / slide (`go through `edit_timeline_exact``).
- `trim` and `split_remove` re-read their clip afterwards because a ripple can move it (`re-reads its clip
  afterwards`).
- `move_clips` / `remove_clips` are single revisions and refuse a locked track (`refuse clips on a locked track`).
- Track locks are the core's to enforce, inside the edit on the timeline the edit lands on: split, trim, cut
  range, move
  (source and destination), reorder, ripple delete, remove, speed, enable, add, paste, titles / voiceover / beat
  snap on a
  named track all refuse a locked track; look and sound (volume, fades, effects, grade, transform, keys) stay
  editable
  (`A lock is the core's to enforce`).
- A clip added with no track goes to the first unlocked track of its kind (`default_track`); all locked refuses
  (`first **unlocked** track of its kind`).
- The lock is the user's: an agent may lock but never unlock a track (refused with review on or off) nor remove
  a locked one
  (`a lock is never only advisory`).
- A track is locked for the agent when `live || (proposal && !base)`, so a lock the proposal only inherited
  follows the user
  (`A lock set`). `working_timeline()` overlays it for agent reads; `run_edit`'s staged path overlays then
  removes it so the
  stored proposal never records the user's lock; a forced accept lands with the user's current locks
  (`effective_lock`).
- A paste and an add to a named track refuse a clip on a track it cannot play on (`check_track_kind`): no
  picture on audio,
  no sound on video; footage with both may go on either; a duplicate keeps its track and does not ask.
- `link-ops.ts` mirrors the lock rules for the harness (`link-ops.test.ts`, `api-locks.test.ts`).
- Batches (`project/batch.rs`): `Project::batch(label, dry_run, |p| ..)` runs ordinary edit calls in one
  transaction, each
  through `run_edit`, recorded as one step (live: `run_edit` skips its revision while `batch` is set; staged:
  first edit pushes
  the step, the rest `continue_staged_step`). Failure rolls everything back, a proposal the batch opened
  included; batches do
  not nest; every write is `Project::tx` (`BEGIN` top level, `SAVEPOINT` inside a batch) (`Several edits as one`).
- `generation()` moves on `replace_with`; a job notes it under the lock and drops its result if it changed. Only
  `replace_with_saved_copy` (Save As) may keep it, so in-flight jobs still land; both swaps return the replaced
  project
  (`#[must_use]`) and the caller `close()`s it after releasing the lock (`Two counters tell the app`).
- `change_count()` is SQLite `total_changes`, monotonic across `replace_with`: "did anything write while
  unlocked" is two
  numbers, not a fingerprint (`It is **monotonic across`).
- `merge_analysis` / `set_analysis` / `place_stems` answer `AssetNotFound` for a gone asset instead of writing
  nothing.
- `save_as` is atomic and a no-op onto its own file; the destination passes `check_output_path` first (`save_as`
  is atomic).
- `insert_asset` is an upsert (`replacing deletes the row first`), never `INSERT OR REPLACE`, because replace
  deletes the row and
  `analysis` cascades from it (`never `INSERT OR REPLACE``).

## Media library

- Media-library ops follow `*_inputs` (lock, database only) -> static fn touching files (lock released) ->
  `apply_*` (lock)
  (`The media library** is filesystem work`). Missing: `missing_assets_inputs` -> `find_missing_assets`.
- Relink: `relink_inputs` -> `probe_relink(&old, path, cancel)` -> `apply_relink_probed(id, &RelinkProbe)`,
  refused when the
  entry's path is no longer the one probed against; `check_relink` is the dry run using the same
  `keeps_analysis` rule
  (`apply_relink_probed(id, &RelinkProbe)`).
- Relink needs the same kinds of stream and length within `RELINK_DURATION_TOLERANCE`; analysis is kept only for
  the same
  media moved (same length to the millisecond, same streams, same size and ends when the old file is there),
  else dropped
  (`analysis is kept only for the same media moved`).
- `remove_asset(id, force)`: `AssetInUse` while a live or proposal clip uses it; force removes them as one
  never-rippling
  `Remove media` revision. Relink and remove are the user's; an agent is refused (`AgentReviewRequired` review on,
  `InvalidArgument` off) because a forced removal would edit the proposal while the asset left the live cut
  (`Both are the user's`).
- History never brings removed media back: `restore` refuses a snapshot with a clip of a missing asset, naming
  it from the
  `removed_assets` meta list (`History does not bring removed media back`).
- Caches keyed by path / size / mtime are cleaned by the static `forget_media_caches(path)`, which probes, so it
  runs with the
  lock released.
- Paths: `probe_asset*` want an absolute path to an existing regular file; refuse `http:` / `concat:` / `pipe:`
  and relative
  paths. `check_output_path` refuses an export onto an asset source, a stitched capture, the project database /
  journal,
  or the cache, canonicalised; `export`, `export_with`, `export_still`, `save_as` call it. An adapter rendering
  unlocked takes
  `output_path_inputs()` under the lock then the static `check_output_path_with` (`check_output_path_with(out,
  &protected)`).
- `referenced_files()` lists media and LUT files for a host that confines files; `history_page` /
  `list_tasks_page` window
  from the newest end.
- Setters validate where set: non-finite numbers refused (and by the serde walk of every stored timeline,
  `core-model.md`);
  `set_volume` clamps to `MAX_CHANNEL_VOLUME`; colours go through `valid_color`; audio effect params held to the
  FFmpeg
  filter ranges (`acompressor` threshold -60..0 dB, ratio 1..20, attack 0.01..2000 ms, release 0.01..9000 ms,
  makeup 0..36 dB;
  `agate` <= 0 dB; `afftdn` reduction 0.01..97 dB, floor -80..-20 dB; EQ gain +-900 dB; frequencies >0 up to
  999999 Hz);
  video effects (`gblur` 0..1024, `unsharp` -2..5, `chromakey` similarity >= 1e-5) (`What a setter accepts is
  checked`).
- Add / trim keeps a clip inside its footage (`source_in >= 0`, `source_out <=` asset length, a still has no
  end) and off other
  clips of its lane, checked after the ripple; `add_clip_to_timeline` keeps the track sorted; `set_speed` and every
  `set_speed_ramp` key are held to `MIN_SPEED..=MAX_SPEED`.

## The file (`project/store.rs`)

- `PRAGMA user_version` is the schema version; `MIGRATIONS` are one step each in its own transaction; never edit
  a shipped
  step, append one (`brought forward by`). Current `SCHEMA_VERSION` is 21.
- The version covers the JSON in the tables too: any change an older build would drop (unknown field, at next
  write) or
  misread (unknown variant) is a step of its own: `run` a no-op or JSON rewrite, `needed` always (so the file is
  copied
  aside), `needed_to_read` false (`The version covers the`).
- Steps so far, each "older build drops / misreads X": v1 `staged.owner`/`assets`; v3 `fonts` table; v4
  `assets.music_score`;
  v5 generated title clip; v6 `Fit::Blur` + `StreamInfo.lead`; v7 keyed mask / crop properties; v8 outlined
  title text; v9
  `data-kerf-animate`; v10 `StreamInfo.color_range`; v11 animation `curves`; v12 `Delivery.fps`; v13
  `Timeline.thumbnails`;
  v14 `Clip.speed_ramp`; v15 `assets.tags` + `tag_edit`; v16 `AudioEffect::Denoise`; v17 `Clip.blend_mode`; v18
  `Clip.stabilize`; v19 `assets.sound_effect`; v20 `Clip.behaviors`; v21 `StreamInfo.time_base`.
- `needed_to_read` is true where reads name the column (v1, v2, v4, v15, v19), false for `fonts` (reads tolerate
  its
  absence) so a write-protected v2 project opens as it is (`needed_to_read` says whether).
- A new file is stamped `SCHEMA_VERSION`; a newer file is refused before any write (`Error::NewerProject`); an
  older file is
  first copied beside itself as `<stem> (schema vN backup).kerf` (kept if present). No backup when no pending
  step changes
  the file; a failed backup is only a warning unless a pending step is destructive (`Each step has a`).
- A read-only file opens as it is; one needing a real upgrade is refused with "copy it somewhere you can write".
  `SCHEMA`
  runs first, so a step must tolerate tables it creates.
- `persist_tests` keeps `persisted/timelines-v<N>.json` for each version that moved the stored shape; all must
  still read,
  the newest must round-trip; moving the shape fails until a step is appended and `KERF_BLESS_PERSISTED=1`
  writes the file
  (add it to `PERSISTED`); a tables-only step needs no file (`KERF_BLESS_PERSISTED=1`). It also pins
  `store::pending_steps`.
- v21 `time_base` backfill: an export that could fold blended clips probes the tick (`engine::time_base_inputs`
  -> bounded
  `probe_time_bases` unlocked -> `apply_time_bases`, once, no revision); an unknown tick keeps clips out of a fold
  (`a_tick_found_for_an_asset_without_one_is_kept_once_and_is_no_edit`).
- Save As is atomic (`vacuum_into_atomic`): `VACUUM INTO` a hidden temp file in the target's dir, fsync, rename,
  fsync dir
  (unix); any failure removes the temp and leaves the old target; stale own `.tmp` files over an hour are
  deleted on save.
- History is bounded: `record_revision` keeps the newest `history_limit` (`DEFAULT_HISTORY_LIMIT` 500,
  `KERF_HISTORY_LIMIT`, minimum 1) and never prunes a proposal's `base_seq` or later; the oldest left is the
  baseline;
  `revert_to` / `diff_revisions` only reach listed revisions (`The history is bounded`).
- Crash recovery (`recovery.rs`): an untitled project lives in `<root>/<session>/untitled.kerf` under an OS lock
  on a separate
  `<session>/lock` file (Windows byte-range locks would block SQLite), unlocked explicitly on drop because a
  forked child
  shares the open file (`unlocked explicitly`). `scan` offers only directories whose lock it can take and that
  hold work;
  a directory is never deleted unless this process holds its lock (`WouldBlock` and any other lock error leave
  it alone)
  (`is **never** deleted unless this process holds its lock`). `close()` deletes the recovery dir;
  `close_at_exit` only when
  it holds no work. Headless `kerf-app` without `--data-dir` uses the desktop recovery root.
- `Project::sample()` is test seed data; the app launches with an empty untitled project (`seeds an in-memory
  demo`).
- `snap_to_beats` defaults tolerance to half a beat and errors when nothing rhythmic was analysed rather than
  doing nothing.

## Smart crop, delivery, side-files

- `smart_crop` is `smart_crop_inputs` -> static `sample_smart_crops` -> `apply_smart_crops`; writes an ordinary
  `Transform`
  crop per clip as one `Smart crop` revision; skips clips already the delivery shape and 360-reframed clips;
  writes no
  revision when nothing changes (`Split three ways for the lock-free pattern`).
- Multi-format: `Clip.framings` keeps a crop per delivery shape (keyed by reduced `Delivery::ratio`);
  `Timeline::for_delivery` renders the cut at another frame by changing the timeline, not the graph. A clip with
  no framing
  for the shape keeps its crop (never throw away a hand-made crop), which is why the framing pass writes an
  identity framing
  for a shot already that shape; captions are re-fit, typed titles left alone (`One cut, every platform`).
- Framing pass: `framing_inputs` -> static `sample_framings` (one salience decode per clip) -> `apply_framings`
  as one
  revision a re-run leaves alone. `engine::render_variants` renders `ExportVariant`s (file names by shape,
  `cut-9x16.mp4`,
  `x` not `:` for Windows); `together` renders one ffmpeg run, else variants run one after another (cancel
  deletes the file
  in flight, keeps finished ones); single pass is not used for two-pass, a hardware encoder, an audio-only
  container or
  two variants on one file name (`the app's default, `single_pass``).
- Side-files (`project/exports.rs`): `delivery_inputs` snapshots working timeline, original assets, render frame
  rate and
  name under the lock; `captions_document`, `interchange_document`, `markers_document` build text with it released;
  `write_document` writes. Captions need captions, interchange needs clips, markers need markers; captions and
  markers read
  `for_render`. `edit_marker` / `add_marker_with` take a `MarkerPatch` as one revision (`Side-files of a
  delivery`).
- Interchange (`interchange/`) is pure, no file I/O, returns text plus a `Report` (identical messages merged
  with a count).
  OTIO writes every track, gaps, transitions, `LinearTimeWarp`, `file://` references, markers and
  `metadata.kerf` (whole clip
  serde state, mixer flags, overlays, SVG titles, format, master). EDL: one video track, record from
  `01:00:00:00`, NTSC
  exact / drop-frame, 8-char unique reels, dips as cuts and slides / pushes as dissolves (reported), `M2` speed,
  disabled
  clips left out, A/V sound reported not written. Golden text in `interchange/golden/*.{otio,edl}`, blessed with
  `KERF_INTERCHANGE_BLESS=1` (`Golden text in`).
- Marker lists: `markers_csv` is RFC 4180, every cell quoted, cells starting `= + - @`/tab/CR are `'`-prefixed
  (formula
  injection); `youtube_chapters` reports (does not enforce) fewer than three or shorter than 10 s;
  `ffmetadata_chapters`
  only chapter markers, escaping `= ; # \` and newlines (`Marker lists`).
- Activity (`project_activity.rs`): the update hook marks areas (`TIMELINE`, `PROPOSAL`, `TASKS`, `ASSETS`,
  `PROJECT`);
  the `set_listener` callback runs under the lock on the writing thread, so it must only wake something (the MCP
  activity
  stream is the one listener); same blind spots as the epoch; a rolled-back write still marks (`it must only
  wake something`).

## Tasks and staged agent edits

- The task queue is a real `tasks` table (not JSON); lifecycle `queued -> working -> ready -> done` / `failed`
  (`real `tasks` table`).
- Agent edits are staged: `edit_timeline` routes an `Agent` edit into the proposal whenever a session is open,
  so the user's
  timeline never moves under them. `staged()` reports with diff and `stale`; applying a stale proposal is
  refused unless
  `apply_staged(force)`; apply lands as one revision attributed to the agent (an empty one just closes). Every
  read an edit
  depends on goes through `working_timeline()`, including preview, still and export paths; the GUI never stages
  (`Agent edits are staged, not applied`).
- `restore` (undo / redo / revert) refuses for an agent holding staged edits (`walking the ground out from under`).
- With review on `claim_next_task` opens a staging session; `resolve_task` applies it; `remove_task` discards
  the proposal
  first in one transaction with the delete, and a non-owner is refused `StagedEditOwnedByOther` with the task
  still queued
  (`the proposal first, in one transaction with the delete`).
- Review is the default (`set_agent_review`, set from Settings, not in the file, survives `replace_with`). On:
  an Agent edit
  with no session opens one in the same transaction as the edit (a failing edit leaves no empty proposal); an
  agent cannot
  `apply_staged` (even `force`), `apply_staged_subset`, `resolve_task` with staged edits, or
  `undo`/`redo`/`revert_to`
  (`Review is the default`).
- A proposal has an owner (MCP session id, `staged.owner`; `StagedEdit` serialises only `owned_by_agent`). The
  adapter
  calls `ensure_agent_staging(owner)` in the same lock as an agent edit; it declares and checks and writes
  nothing, because
  opening the session up front left an empty owned proposal when the edit then failed. Apply, discard, undo by a
  non-owner
  agent are refused; the tool that only ends a proposal calls `declare_agent(owner)`; the user is never refused
  (`A proposal has an owner`).
- `release_orphaned_staging(gone)` clears the owner when the adapter says the MCP session is gone (`A proposal
  whose session is gone is released`).
- `set_ripple_mode` as an agent is refused while review is on; the edit's `ripple` argument is how an agent
  ripples once.
- Review off: task work lands live (`Review off: task work lands live`). `claim_next_task` only marks `working`;
  edits are
  ordinary agent-attributed revisions, undoable by the agent. `complete_task` is `done` when review is off and
  nothing is
  held back, else `ready` (`task_work_waits`); `fail_task` does not roll back. `stage_edits` / `begin_staging`
  still open a
  proposal with review off; a claim is refused (`StagedEditPending`) while any non-terminal proposal is open.
- Flipping the switch with a proposal open is safe because `run_edit` routes on whether a proposal exists, not
  the flag
  (`The switch flipping with a proposal open`): off, an open proposal stays open and keeps taking agent edits;
  on mid-task,
  landed edits stay and the next edit opens a proposal.
- Asset-level agent writes (`set_asset_projection`, `edit_asset_tags`) stage a `StagedAssetChange`
  (`staged.assets`); the
  agent's own reads see them (the user's do not); accept writes them with no revision; asking for what the
  library already has
  stages nothing; `stage_asset_changes` writes any mix as one undoable step (`Asset-level agent writes are
  staged too`).
- Undo inside a proposal: `staged_undo` table, bounded at `STAGED_UNDO_DEPTH` (20), emptied with the proposal;
  `undo_staged()`
  covers timeline edits and asset changes (`Undo inside a proposal`).
- Partial accept `apply_staged_subset(entry_ids, force)`: one `Agent edit (k of n changes)` revision; the rest
  stays open
  re-based (`base`, `base_seq` move; proposal timeline rebuilt as live plus exactly the unapplied changes via
  `Timeline::rebase_rest`, because keeping it as it was would diff as undoing the user's later work); a change
  that no longer
  fits is dropped and counted (`Partial accept`).
- Discard is restorable: `discard_staged` keeps a non-empty proposal in `meta` (`discarded_staged`);
  `restore_discarded()`
  only with no proposal open, once, possibly stale, no undo history, unowned when the user restores; an agent
  may restore only
  what an agent discarded (`discarded_by_user`); the kept copy is dropped when a newer proposal is really
  applied and on
  `replace_with` (Save As keeps it) (`Discard is restorable`).

## Music, sound effects, stems

- `plan_music_fit` / `fit_music`: music clip on an audio track (normal speed, unlinked, with cached
  `AssetAnalysis.music`);
  one `edit_timeline_exact` revision "Fit music to length"; refuses a result running into the next clip
  (`plan_music_fit(clip, target)`).
- Composed music (`project/music.rs`): `Asset.music_score` (v4). Request types use `#[serde(deny_unknown_fields)]`
  (`MusicScore`, `ScoreSection`, `ScoreHit`, `InstrumentOverrides`, `ComposeRequest`, `ScoreAlign`,
  `BandBalanceRequest`,
  `BandWindow`) so a misspelled field is an error (`The request types refuse a misspelled`).
- Split: `compose_music_inputs` under the lock says everything that can be said wrong before the render (score
  resolves,
  target is an unlocked audio track, replaced clip is on a composed asset with no picture link), then rehearses
  the placement
  (`check_music_fits`: stand-in asset through `put_music`, `ripple_lanes` when ripple is active,
  `check_own_lanes`) so overlap
  is refused before rendering with edit_timeline's words; ripple pushing passes, a start inside a clip fails
  (`Last, the placement is rehearsed`). Then static `synthesize_music` (lock released) -> `place_music` under
  the lock.
- `place_music`: asset, analysis and edit are one transaction inside the `edit_timeline` closure, so a refused
  placement
  stores nothing; analysis is written from the score (`Rhythm` kind, full-confidence `Tempo`, `MusicAnalysis`),
  not analysed;
  one `Add music` edit on the audio track `M` (`MUSIC_TRACK`) (`asset, its analysis and the edit are one
  transaction`).
- `replace_clip_id` replaces the whole bed (all clips of that asset on its track) with one clip, one `Recompose
  music`
  revision; ripple applies as to any edit and without it a longer render running into the next clip is refused
  (`replaces the whole bed`).
- Band balance: `band_balance_inputs` -> static `measure_band_balance` -> `band_balance`; exactly one target;
  times are the
  target's own clock; windows are explicit `sections` (each >= 0.25 s), else score sections, else bar-grid
  phrases, else
  eight equal windows (`Band balance`). `analysis.rs` serves a composed asset's rhythm from `ScoreRhythm`.
- Sound effects (`project/sfx.rs`): `Asset.sound_effect` (v19). `SfxPlacement` is flat on the wire;
  `deny_unknown_fields`
  cannot combine with `flatten`, so `Deserialize` is hand-split (`deny_unknown_fields` cannot be combined with
  `flatten`).
- `at` is the hit point (`model::sfx_anchor_s`); a start before 0 is cut off the head with a 4 ms fade-in unless
  under
  `SFX_HEAD_SNAP_S` (10 ms); a hit leaving under 20 ms of sound is refused naming the time (rounded up).
  `model::sfx::span`
  is the one rule (`is where the effect's hit point lands`).
- `sound_effects_inputs`: 1..=256 placements, valid specs, `at` finite >= 0, `volume_db` -60..=12, named track
  an existing
  unlocked audio track, all against the working timeline, then rehearsed (`check_sfx_fits`). Then static
  `synthesize_sound_effects` (lock released) -> `place_sound_effects` under the lock: each asset must be the
  render of its
  planned spec; assets are stored inside the `edit_timeline` closure (refused placement stores nothing); the
  batch is one
  `Add sound effects` revision (`check_sfx_fits`).
- Lanes: no `track_id` goes on the first unlocked audio track named `FX`, `FX 2`... whose `[start, end)` is free
  (touching
  is not overlap; earlier effects of the batch count), else a new audio track; a locked `FX` is skipped; placed
  in the order
  given, so suggestions are sorted by time (`Lanes`).
- Reshape `update_sound_effect_inputs` -> `synthesize_sound_effect` -> `apply_sound_effect_update`: unlocked
  audio track and
  an asset with `sound_effect`; one `Edit sound effect` revision keeps the hit point's timeline position, id,
  gain, fades,
  effects, keys; resets trim and speed; ripple applies (`is the reshape`).
- `preview_sound_effect` renders to the pruned cache dir and makes no edit; `add_sound_effects` /
  `update_sound_effect` are
  `#[cfg(test)]` (surfaces render with the lock released).
- Suggestions: `suggest_sound_effects_inputs` reads the working timeline and cached analysis only (analyses
  nothing) ->
  pure `model::suggest_sound_effects`; a read, no revision; each suggestion's placement has no `track_id`
  (`Suggestions`). Rules are in `core-model.md`.
- Stems: `separate_stems_media` (static, unlocked); `place_stems(source, stems, clip?)` lays each stem on a new
  audio track and
  switches the clip's own sound off; a picture whose sound is on a linked clip is refused
  (`separate_stems_media(asset, progress, cancel)`).

## Animated SVG titles

- `add_svg_overlay`, `update_svg_overlay`, `remove_svg_overlay` are each one `edit_timeline` revision (they stage).
- Parsing costs up to ~1.7 s, so a surface parses before taking the lock: `SvgSource::check()` then
  `add_svg_overlay_parsed`;
  `SvgOverlayPatch::check()` then `update_svg_overlay_parsed`. The plain versions are `#[cfg(test)]`; `lib.rs`
  and `mcp.rs`
  call the `_parsed` pair (`so a surface does it before it takes the project lock`).
- `SvgSource::Custom` parses before the edit closure (bad document is `InvalidArgument`, no revision); `Preset`
  is generated
  inside the closure for the working timeline's `preset_frame()`. Start / end held to `MAX_SVG_SECONDS` (24 h);
  a 51st title
  is refused (`check_svg_span`).
- Update: `svg` converts a preset to custom, `preset_params` re-fills, `preset_kind` changes kind (both refused
  on custom art;
  `svg` exclusive with either); a preset is re-made whenever its length changes; `set_delivery_format` re-makes
  every preset
  (`is re-made whenever its length changes`).

## Motion templates (`template/`, `project/templates.rs`)

- A title keeps only `SvgOverlay.template: Option<TemplateInstance>`; its artwork is its own `svg`; `values` is
  read back from
  that `svg` after every change so it cannot drift; a project never reaches back into the library: applying
  copies the SVG
  and the fonts in (`its artwork is its own`). Absent from JSON when `None` (byte-identical).
- Format (`archive.rs`): `.kerftemplate` zip with `manifest.json` (`format: "kerf-template"`, `version: 1`),
  `title.svg`,
  `fonts/*.ttf|otf` (<= 8 MiB each, <= 16), `poster.png`. Read into memory by known names only, never extracted.
  Central
  directory checked first: <= 64 entries, plain unique names (no `..`, leading `/`, `\`, drive, NUL), no
  encryption, declared
  sizes summed <= 32 MiB (a zip bomb deflates to kilobytes), no overlapping entries; then caps (manifest 256
  KiB, title 128 KiB,
  poster 4 MiB, font 8 MiB) and exact lengths (`a zip bomb deflates to kilobytes`). Unknown entries ignored.
- Manifest `format` and `version` are read first: newer is refused ("made by a newer Kerf"), older goes through
  `model::UPGRADES` (`UPGRADES[i]` rewrites version `i + 1` to `i + 2`; `VERSION` is `1 + UPGRADES.len()`;
  append one per
  format change). An unknown manifest field is refused (`A manifest field this build does not know is refused`),
  so a format change must bump the version.
- Fonts must be a plain sfnt (`fonts::check_font`, not `.ttc`); one whose `OS/2` `fsType` forbids embedding
  (`fonts::embedding_allowed`: 0x0002, 0x0004, 0x0200) refuses the whole file on read (`import` and `get`),
  while `save`
  leaves it out with a warning (`refuses the whole file`). A poster not PNG or over 2048 px is no poster;
  `to_bytes` is
  deterministic. Library dir `dirs::data_dir()/kerf/templates/<id>.kerftemplate`, `KERF_TEMPLATES_DIR` read on
  every call.
- Controls: kinds `text` / `color` / `number` / `font` / `image`; `targets` need `attribute` except for `text`;
  allowed
  attributes: colour `fill stroke stop-color flood-color lighting-color color`, font `font-family`, image `href` /
  `xlink:href`; never `id`, `class`, `style`, a link or `on*` / `xmlns*`; `min` / `max` on numbers only; ids
  `[A-Za-z][A-Za-z0-9_-]{0,63}` unique; <= 64 controls, 16 targets each (`Controls`).
- Save-time validation (`validate_controls`, via `Template::validate` on the sanitised title): every target
  exists exactly
  once with its attribute, no two controls write the same place, a text control's element holds no other
  control's target
  (`element holds no other control's target`).
- Values (`apply_values`, pure, partial; unknown id refused): text <= 2,000 chars, no control chars; any value
  over the title's
  128 KiB refused before decode; colours `#rgb[a]` / `#rrggbb[aa]` / `svgtypes` names / `none` / `currentColor`
  (not
  `engine::valid_color`, FFmpeg's table lacks `transparent`); numbers finite, in bounds, plain decimal, unitless
  (`5px`
  refused); font <= 64 chars (unknown family is a warning); image a `data:` URL passing `images::check_href` (16
  Mpx rule).
  Spliced by byte range (`svg::splice`); a `text` control sets the element's own first direct text run and keeps
  children,
  comments and SMIL; the result then goes through `SvgDoc::parse` (the sanitiser has the last word) (`Values are
  **spliced by byte range**`).
- Glyph outlines (`svg/outline.rs`): `Template::prepare` and `TitleTextInputs` splice values into the text form
  (`svg::restore_text`), sanitise, then `SvgDoc::outlined(db)`. The form is reversible: `<g data-kerf-text="">`
  keeps every
  attribute, run, `<tspan>` and animation, plus a generated last child `<g data-kerf-glyphs="">`; `restore_text`
  is byte for
  byte, so `read_values` / `apply_values` work unchanged and `build_template` / `element_ids` restore first
  (`The form is reversible`). It draws what resvg drew (`It draws what resvg drew`).
- Only outlines honour `inline-size` wrapping and `white-space: pre-line`. Anything else stays text (positioned
  / nested
  `<tspan>`, `textPath`, `dx`/`dy`/`rotate`/`textLength`, text in `<clipPath>`, RTL / vertical, decorations,
  small caps,
  `font-variation-settings`, bbox-measured paint, dashes, layout animations, a `<style>` rule that would match
  differently, an
  uninstalled character, colour glyph, `<use>` instances, non-default `shape-rendering`, markers, outlines over
  128 KiB);
  nothing is outlined past the cap; kept text with `inline-size` / `pre-line` is a warning. Text that cannot fit
  the cap (24
  bytes per visible character at least) is refused before layout (`Anything else stays text`).
- A values change outlines with the embedded fonts the new title names; `restore_text` refuses a source over the
  size /
  nesting cap before parsing. Wrap compares running f32 sums, summing directly within `1e-4` of the total so a
  break does not
  flip (`A values change`).
- A text's box (`svg/text_box.rs`, `project/title_text.rs`): `inline-size` plus `data-kerf-fit="shrink"`
  (`FIT_MARK`);
  shrink scales everything following the font size by `width / widest`, up to four passes, never up.
  `svg::set_text_box`
  needs a `<text>` with a unique id, width finite, positive, <= `MAX_BOX_WIDTH`; writes the width first in
  `style` with
  `!important` so it beats attribute and `<style>` rules and any declaration simplecss's tokenizer stops at;
  refused when a
  template control targets the text's `inline-size` (`FIT_MARK`).
- `title_text_inputs(id)` works on floating or title-clip titles, not a preset (redrawn from params); `text_box`
  / `convert`
  run outside the lock, then `set_title_text_parsed` with the "try again" rule; `convert` to `text` form is
  refused for a
  template title and for one with no outlines; a box on a text that stays text is an error;
  `fixtures/text-box-corpus.json`
  (`fixtures/text-box-corpus.json`) is what the frontend port replays (`title_text_inputs(id)`).
- Custom artwork with a box and no outlines draws with its fonts and gets `svg::BOX_NOTE`; `outlined_counted` and
  `Template::validate` drop it; a template poster is drawn from the outlined title.
- Text animations (`svg/text_animation.rs`, `glyph_smil.rs`, `curve.rs`): JSON in `data-kerf-animate`
  (`TextAnimation::to_attribute`; `from_attribute` refuses unknown fields, wrong types, out-of-bounds numbers,
  no-op
  animations). It is data, not SMIL: the outline pass compiles it; the attribute stays on the `<g
  data-kerf-text>` and
  `restore_text` drops the generated animation (`It is **data, not`). Bounds: `delay` 0..30 s, `duration`
  0.05..30 s, wiggly `min`..`max` in -1..1, `correlation` 0..1.
- Compile: sampled every 1/120 s, Ramer-Douglas-Peucker to fixed tolerances, at most 96 points in one pass
  (split the
  stretch furthest off its chord first). Never loosen the tolerance to fit (an earlier version did and a long
  wiggle compiled to
  no animation) (`Never loosen the tolerance to fit`). Work cap `MAX_WORK` (3 M samples; a curve on a property
  costs
  another pass: `glyph_smil::unit_work`) else the text stays text with a warning; > 600 units, > 2,000
  animations or the
  128 KiB cap also keep text (`MAX_WORK`).
- Rotation and scale turn about the unit's centre (0.35 em above baseline); a property that does not change is an
  attribute; blur filter ids `kf-blur-N` skip ids the document has; custom artwork with the attribute and no
  outlines says
  `svg::ANIMATE_NOTE`.
- `TitleTextInputs::text_animation` (`svg::set_text_animation`, then outline) is refused when a template control
  targets the
  attribute, when the text stays text, and when a title has nothing set as outlines; taking one off a
  never-outlined title
  leaves it so; then `set_title_text_parsed`. Eight presets (`text_animation_presets`) are copied, referred to
  by nothing.
  Fixture `fixtures/text-animation-corpus.json` (`fixtures/text-animation-corpus.json`) is what the frontend
  replays; `parseAnimation` reads the raw seed token (serde refuses `7.0` / `1e3`). No graph change, no golden
  family.
- Curves (`svg/curve.rs`): `{"kind":"speed",...}` or `{"kind":"spring","bounce"}`, read strictly; written for a
  unit settling
  (amount falls 1 -> 0): factor `1 - progress(1 - amount)` (`Curve::settle`); no curve means a straight
  animation compiles
  byte-identical. Speeds -5..5, influences 0.01..1, bounce 0.05..0.8. Bounded by construction: each sample clamped
  (`MAX_MOVE` 20 em, `MAX_TURN` 3600 deg, scale `MIN_SCALE`..`MAX_SCALE` 10, blur 0..1 em, opacity 0..1) and a
  non-finite
  progress falls back to the straight line (`Bounded by construction`). `with_motion_curve` puts a curve only on
  the x, y,
  scale, rotation the animation changes, refuses one for an animation changing none (opacity / blur keep none).
- Lock-free apply: `TemplateLibrary::get` -> `Template::prepare(&values)` -> `PreparedTemplate` -> under the lock
  `apply_template_parsed(prepared, start, end?, SvgLayout)` (one `Add template title` edit; staged for an
  agent). Set values:
  `template_edit_inputs` (reads `working_timeline()`) -> `TemplateEditInputs::prepare` ->
  `set_template_values_parsed`,
  which refuses a title that changed since it was prepared; stored controls are re-checked with
  `validate_controls_shape`;
  no values is an `InvalidArgument`; plain `apply_template` / `set_template_values` are `#[cfg(test)]`; an
  `update_svg_overlay` with `svg` ends the template, a rename or move keeps it (`Lock-free shape.`).
- Library (`library.rs`, app-level: not an edit, not undone, not staged): `list()` reads manifests only, returns
  unreadable
  files in `problems`; a file is listed only if its name is its manifest's id and the extension is exactly
  `.kerftemplate`;
  removes `.kerf-tmp-*.tmp` older than ten minutes. `save` embeds fonts through the same font database the
  renderer uses
  (never Geist or CSS generics; a face over 8 MiB, in a `.ttc`, not installed or restricted is left out with a
  warning).
  `import` validates the whole file and writes a fresh canonical file; same id with same content is
  `already_present`, else
  `added_as_copy` under a content-derived id (UUIDv8), never overwriting. `export` copies atomically (Windows
  device names
  get an underscore). Every write is a temp file beside the target, random name, `create_new`, fsync, rename.
  Ids are checked
  as UUIDs before becoming a file name. Bounded: `MAX_TEMPLATES` 500, `MAX_BYTES` 2 GiB, checked by `add`
  (`library is bounded`).
- Embedded fonts (`fonts.rs`): `fonts` table (`sha256` key; v3) up to 128 MiB, stored inside the edit's
  transaction. A font
  stays while a title names its family (`collect_unused_fonts`, `fonts::names_family` case-blind substring); the
  rest are
  deleted only when a proposal is discarded or closed empty, on Save As, and when a project is opened, so undo
  can still bring
  a font back until then (`A font stays while a title names its`).
- Opening a project registers nothing: `Project::open` only reads fonts into `Project.embedded`, because an
  opened project may
  never become the open one; the process set is replaced at the swap (`activate_fonts`), and applying a template
  adds
  (`register_embedded`); each moves the plan epoch when the set changed. The registry is process-wide (per
  thread under
  `cfg(test)`), so cross-crate tests opening projects on parallel threads must serialise (`Opening a project
  registers nothing`).
- A title is drawn with the fonts it names: planning takes `fonts::snapshot_for` (`FontSet`), one value that
  keys the
  pictures and draws them (`SvgSeq::fonts`, `StillSvg::fonts`). The registry lock is only held to swap an `Arc`,
  never across
  a font-database build (`A title is drawn with the fonts it names`). `engine-cli.md` covers the SVG font
  database keys.

## Titles on tracks, asset tags, stabilization

- Title clips (`project/title_clips.rs`): `add_title_clip_parsed(SvgDoc, start, duration?, TitleClipPlacement)`,
  `apply_template_clip_parsed`, `update_title_clip_parsed`, one `edit_timeline` revision each, artwork read
  before the lock.
  Placement: the named video track (a clip over the span is refused unless ripple makes room), else the lowest
  unlocked video
  track free over the span, else a new `V{n}` on top (`TitleClipResult {clip, track_id, warnings}`).
  `set_title_values_parsed` dispatches on overlay vs clip
  (`TitleEdited`). Ops that look an asset up per clip skip generated ones (`proxy_waits`,
  `refuse_removed_media`); sound ops say a
  title has none; `measure_levels` / mix band-balance ranges are checked against `Timeline::untitled_duration`
  (`range_past_the_sound`). At most `MAX_TITLE_CLIPS` (50) titles on tracks; `put_title` refuses the 51st and
  the invariant net
  refuses a split that would make one.
- Asset tags (`project/tags.rs`): `assets.tags` JSON column (v15); `edit_asset_tags(ids, &TagEdit)` resolves and
  applies all
  before writing any (unknown id, invalid tag or 65th tag refuses the lot), one transaction, no revision. As the
  agent
  with review on or a proposal open it stages one `StagedAssetChange` per asset with `tag_edit`. The held change
  is a delta,
  not a snapshot (`TagEdit::then`, `TagEdit::apply_capped`), applied to the tags as they are when read, so a tag
  the user added
  live is kept and adds past 64 are left off rather than failing the accept (`The held change is a delta, not`).
  An edit that leaves the agent's view unchanged stages nothing and pushes no undo step.
- `assemble_by_tag`: `assemble_by_tag_inputs` -> `AssemblePlan::prepare` (lock-free preset render) ->
  `apply_assemble_by_tag`
  (one `edit_named` revision, so ripple, sync lock and staging apply); no matching asset is an error; titles are
  title clips,
  the cap of 50 refuses the whole assembly; preset titles are not retimed by a later trim; assets with no stream
  or length are
  skipped (`is three steps`).
- Stabilization (`project/stabilize.rs`): `set_clip_stabilize` is one revision through `edit_timeline` (stages);
  refused for
  audio-track clips, titles, stills, no-picture clips and 360 footage (`why_not_stabilized`); a locked track
  does not refuse
  it (a look). Analysis: `stabilize_inputs` under the lock -> static `analyze_stabilization(job, progress,
  cancel)` (`Ok(false)`
  without vid.stab or when an analysis already covers the clip; `measure_motion` in the background lane) ->
  `stabilization_status_inputs` -> static `stabilization_statuses` (`off`, `unsupported`, `basic`, `queued`,
  `analyzing`,
  `ready`, `failed`, `missing`); `review`'s `unstabilized` check reads statuses with the lock released
  (`Stabilization`).

## analysis.rs

- Transcription / scene / silence / rhythm are pluggable traits (`Transcriber`, `SceneDetector`, `SilenceDetector`,
  `RhythmAnalyzer`); `NullAnalyzer` is the fallback. Only `Transcriber::transcribe` takes both `ProgressFn` and
  `CancelFn`
  (it can download a model and run for minutes). `FfmpegRhythmAnalyzer` does onsets, tempo and speech/music
  class from one PCM
  decode. Scene detection scores at 640 px and may read the asset's ready proxy (`Project::scoring_proxy`).
- Analysis runs in steps, one kind at a time: `AnalysisKind` (`silence`, `scenes`, `loudness`, `rhythm`,
  `transcript`);
  `analyze_asset_steps(asset, kinds, progress, cancel)` returns a `StepsRun` patch; `Project::merge_analysis`
  folds it in and
  leaves other kinds; a cancelled or failed step caches nothing, a failed step does not discard finished ones,
  and a run where
  every step failed is an error (`Analysis runs in steps, one kind at a time.`).
- `AssetAnalysis.ran` is the only way to tell "ran and found nothing" from "never ran"; a cache with no `ran`
  counts a kind done
  when it has data (`AssetAnalysis::done`, mirrored in `analysis-steps.ts`). `analysis_status` per kind: `done`,
  `running`,
  `failed` (last failure per asset+kind, cleared on success), `off`, `not_run`.
- `AutoAnalysis` (master `enabled` + a flag per kind) decides the step set: `analyze_asset_steps(None)` runs
  `default_kinds()`
  (toggles, minus transcript without a backend); a named list runs exactly that, so an agent cannot fetch a
  speech model for
  someone who turned transcription off and a user can still transcribe one clip by hand. Transcribing with no
  backend is a
  failed step, not an empty transcript (`cannot fetch a speech model`).
- Word timings and speaker labels are part of the `transcript` step, not a sixth kind; status stays five kinds.
  Old transcripts
  without `words` / `speaker` read as done; readers estimate words (`transcript::words_of`); `refine_transcript`
  adds them
  without re-transcribing: `Project::transcript` -> static `refine_transcript_media` -> `store_transcript(asset,
  read, refined)`,
  refused when the cache no longer holds `read`, so it never overwrites a newer transcript. `speaker_ranges`
  errors naming the
  known speakers; `filler_words` matches phrases across words; `transcript_search_inputs` returns each analysis
  row as unparsed
  stored JSON (`Word timings and speaker labels are part of the`).
- `resolve_steps(asset, requested)`: a file with no audio is not asked for silence, loudness, rhythm or speech;
  a synthesized
  sound effect keeps only silence and loudness (`AnalysisKind::fits_effect`); a still has nothing to analyze; an
  empty answer is
  an `InvalidArgument` saying why, not an empty run (`What a request runs`).
- `analyze_asset_steps` runs in the background lane of the heavy-job queue (`engine-cli.md`), announces
  `waiting`, can be stopped
  while queued, and hands the patch so far to `on_step` after each step so the adapter merges it then.

## proxy.rs

- Settings: `ProxySize` 0 / 720 / 1080 / 1280 (lenient on read; 0 forces the original); a size change cancels
  every queued or
  building proxy and the adapter queues what is missing at the new size. `PreviewSource` `auto` / `original` /
  `proxy_only`
  (unknown reads as `auto`). `ProxyStatus`: `not_needed`, `off`, `missing`, `queued`, `building`, `ready`,
  `failed`. Queue:
  `queue_auto` (skipped for stills, when proxies are not wanted, for a deleted proxy, one already on disk),
  `rebuild`, `delete`,
  `cancel_all` (`cancels every queued or`).
- The registry holds only what is in flight; `ready` is the file on disk (`ready_proxy`), so a status cannot
  claim a proxy the
  preview would not use. Each submission has a generation; a worker only touches its own entry (`The registry
  holds only`).
- A job stays `Queued` until it holds the machine's slot; one worker (`KERF_PROXY_WORKERS`), because the encode
  is x264 on the
  cores' one slot; the run is `catch_unwind`-guarded (`run_guarded`), a panic marks the job `Failed`.
- A deleted proxy is remembered in the project (`proxy_declined` meta key, not an edit) so the next open does
  not rebuild it;
  a rebuild forgives it. Under Proxy only `Project::proxy_waits(from, to)` names clips with no ready proxy; GUI
  preview commands
  refuse to decode the original for them, an agent's reads do not ask. Export never reads a proxy whatever any
  of this says.
- Analysis takes a ready proxy too (`scoring_proxy`, `sheet_proxy`, pure `resolve_analysis_proxy`): same
  `preview_source`
  rule, a proxy still building is not waited for, plus `engine::proxy_fidelity` (MP4 / MOV / Matroska for times,
  SDR YCbCr for
  scene scores; exact gate in `engine-cli.md`); a proxy that fails to read falls back to the original; smart crop's
  `salience_map` is not swapped (`Analysis takes a ready proxy too`).

## cache_cap.rs

- Opt-in ceiling on regenerable caches for a hosted headless server (`--cache-max-gb N` / `KERF_CACHE_MAX_GB`);
  nothing else
  starts it. Every cache is under `paths::cache_dir()/kerf`; never call `dirs::cache_dir()` directly (`Where
  caches live:`).
- Pruned by the cap: `proxies/` (hash `.mp4` / `.lead.mp4` + `.json` sidecar = one unit), `waveforms/`,
  `filmstrips/<hash>/`,
  `stems/<hash>/`, `music/`. Never pruned: `models/`, `soundfonts/`, `runtime/`, `stitched/` (the stitched file
  is the asset's
  source, unbounded and outside the cap), and self-bounded `luts/`, `sfx-preview/`, `stabilize/`, `fonts/`,
  `svg/<pid>/`,
  `segments/<pid>-n/`. The data dir's `sfx/`, `voiceovers/`, `music/` (the only copy), recovery files and logs
  are never touched
  (`*never pruned*`).
- The plan is pure (`prune_plan(entries, cap)`): only `Stale` (`.part` older than 24 h) and `Evictable` go,
  stale first then
  least recently used (`noatime` degrades it to write order), ties by path, only until `total <= cap`; a stale
  `.part` is
  swept only while over the cap; never a zero-byte unit; `InUse` and `InFlight` (`.part` names or touched in the
  last 10 min)
  are kept; if the kept part alone exceeds the cap it is warned about and nothing else removed (`The plan is
  pure`).
- In use: `InUse::of(&assets, root)` = `engine::asset_cache_files(asset)` (one stat, no ffprobe; proxy names in
  all four
  spellings) plus the asset's own file and its directories up to the cache root when it lies inside
  `<cache>/kerf`: separated
  stems are project media, and the whole stem unit stays while any one is an asset (`**In use** is`).
- Never blind: `run_once` skips the pass, removing nothing, when `list_assets` fails or an asset's original
  cannot be stat'ed
  (`InUse::unreadable`: names of what exists cannot be worked out, so an offline asset pauses the cap with a
  warning); skipped
  and over-the-cap states are told once on begin and once on end (`Never blind:`).
- Locks: `start(cap, assets)` takes a closure that reads assets under the project lock and returns; stats, plan
  and deletes
  run after on the `kerf-cache-cap` thread (nice 10), at once, every 10 min, and 20 s after a `nudge()`. `start` is
  `Err(reason)` when it cannot run (headless exits 2). Only this process's open project is protected, so do not
  share one
  cache dir with another app or server (`One process:`). A removed proxy reads `missing`, previews fall back,
  `remove` bumps
  `plan_epoch`, `queue_auto` rebuilds it; the open project's are never removed.

## error.rs, thumbnails

- `error.rs`: the `Ffmpeg(#[from] ffmpeg_next::Error)` variant is itself `#[cfg(feature = "ffmpeg")]`.
- Thumbnails (`project/thumbnails.rs`): `thumbnail_base(id?)` under the lock -> static `prepare_thumbnail(base,
  &ThumbnailSpec,
  library)` unlocked -> `apply_thumbnail_parsed` (`Add` / `Change thumbnail` through `edit_timeline`: stages,
  undoes, diffs);
  refused when the stored thumbnail is no longer the one prepared from; `layers` is the whole new stack; a layer
  is made from
  exactly one of `text`, `template_id`, `svg`, `image` (a `data:` URL via `template::image_data_url`);
  `MAX_THUMBNAILS` 20,
  `MAX_THUMBNAIL_LAYERS` 12; `collect_unused_fonts` keeps fonts a layer names (`Thumbnails`).
  `thumbnail_render_inputs` and
  `frame_suggestion_inputs` feed `crate::thumbnail` with the lock released.

## Entry points by topic

- Edits and ripple: `edit_timeline`, `edit_timeline_exact`, `run_edit`, `Project::batch`; locks:
  `ensure_editable`, `effective_lock`.
- File format: `store.rs` `MIGRATIONS`, `SCHEMA_VERSION`, `persist_tests`, `store::pending_steps`.
- Staging: `begin_staging`, `staged()`, `apply_staged`, `apply_staged_subset`, `working_timeline()`,
  `task_tests.rs`.
- Tests named for a topic: `link-ops.test.ts`, `api-locks.test.ts`, `project/blend_tests.rs`,
  `project/behavior_tests.rs`,
  `music_tests.rs`, `sfx_tests.rs`, `tags_tests.rs`, `tests/headless.rs`.

## Open ends the full file states

- Residual: a live title naming the same family as a proposal's embedded font is drawn with the proposal's
  embedded face while
  the proposal is open, because nothing marks a font as the proposal's (`Residual:`).
- A font collected on discard / Save As / open is gone for good: undoing to a revision whose title used it, or
  restoring a
  discarded proposal, draws that title in Geist until the template is applied again (`Once collected a font is
  gone`).
- A v21 asset with no stored `time_base` is "not known" and stays out of a fold until an export backfills it
  (`an asset stored before has`).
- An asset stored before v10 has no `color_range` and is judged by its format name until relinked or re-imported
  (`reads "not known"`).
- The cache cap sees only this process's open project: sharing a cache directory with another app or server is
  unsupported
  (`do not share one`).
