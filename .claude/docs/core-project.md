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
  when it resolves its inputs under the lock and drops its result if it changed. `change_count()`
  is SQLite's `total_changes` — every row written through the connection (analysis merges,
  ripple mode, meta, a declined proxy …), so "did anything write while the lock was released?"
  is two numbers, not a content fingerprint. It is **monotonic across `replace_with`** (the swapped-in
  project's connection restarts at zero; the count carries on from the one it replaced), so a number
  noted under the old project can never equal one read under the new — `generation()` still says
  *which* project. `merge_analysis` / `set_analysis` (and
  `place_stems`) answer `AssetNotFound` for an asset that is gone instead of writing for nothing.
  **`save_as` is atomic**: `VACUUM INTO` a temporary file beside the destination, then rename; a
  failure leaves the destination as it was (it used to be deleted first), and saving onto the
  file the project is open on is a no-op; the destination is held to `check_output_path` first (never
  over an asset's source, a stitched capture, a proxy or the cache). **`insert_asset` is an upsert** (`ON CONFLICT … DO
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
  compare) stays as the pre-split shape. Same kinds of stream, length within
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
  `Project::sample()` seeds an in-memory demo (two assets + analysis + a starter
  timeline + a sample task queue); it backs the kerf-core tests, but the app now
  launches with an **empty** `Project::open_in_memory()` — the user imports media or
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
  filename character) **one after another**, each variant's `resolution` / `fit`
  taken from its delivery, reporting a `VariantProgress` (which file of how many
  plus the overall fraction). Sequential on purpose: an export takes every core
  it is given and `cpu::lease` would serialize them anyway, and a cancel is
  then clean — the file in flight is deleted, the finished ones kept.
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
  edits) and `remove_task` discards it. `diff_revisions` / `revision_diff` point
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
  is never refused.
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
  dismissed. An empty proposal is not kept and does not replace one that is. The `staged` table
  gained `owner` and `assets` (added by `init`'s probe-and-`ALTER`, like `voiceover`; an older
  file's open proposal loads with no owner and no asset changes).
  `plan_music_fit(clip, target)` / `fit_music(clip, target, fade_out)` fit a music clip on
  an audio track (normal speed, unlinked, with a cached `AssetAnalysis.music`) to `target`
  or, by default, the picture's end minus the clip's start; `fit_music` replaces the clip
  with `music_fit_clips` in one `edit_timeline_exact` revision ("Fit music to length") and
  refuses a result that would run into the next clip on the track.
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
  transcript. **What a request runs** is `resolve_steps(asset, requested)`: a file with no audio is
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
