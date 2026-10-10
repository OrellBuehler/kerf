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
  `plan_music_fit(clip, target)` / `fit_music(clip, target, fade_out)` fit a music clip on
  an audio track (normal speed, unlinked, with a cached `AssetAnalysis.music`) to `target`
  or, by default, the picture's end minus the clip's start; `fit_music` replaces the clip
  with `music_fit_clips` in one `edit_timeline_exact` revision ("Fit music to length") and
  refuses a result that would run into the next clip on the track.
- `analysis.rs` — transcription / scene / silence / rhythm are **pluggable traits**
  (`Transcriber`, `SceneDetector`, `SilenceDetector`, `RhythmAnalyzer`). Real impls
  now exist:
  `FfmpegSilenceDetector` / `FfmpegSceneDetector` (CLI engine, always available —
  scene detection decodes hardware-accelerated and scores at 640px, the metric
  being resolution-normalized), `FfmpegRhythmAnalyzer` (onsets + tempo +
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
  A worker's per-job run is `catch_unwind`-guarded (`run_guarded`): a panic marks that job `Failed`
  and the worker takes the next one. A deleted proxy is remembered **in the project** (`Project::proxy_declined`,
  meta key `proxy_declined`, a list of asset ids; not an edit) so the next open does not build it
  again; a rebuild forgives it. Under **Proxy only** `Project::proxy_waits(from, to)` names the clips
  in range with no ready proxy; the GUI preview commands refuse to decode the original for them, an
  agent's reads do not ask. Export never reads a proxy whatever any of this says.
- `error.rs` — `Error`/`Result`; the `Ffmpeg(#[from] ffmpeg_next::Error)` variant is
  itself `#[cfg(feature = "ffmpeg")]`.
