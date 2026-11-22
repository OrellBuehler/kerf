# core-model — brief

Rules only. The full file `.claude/docs/core-model.md` is authoritative and holds the measurements and the story
behind each rule: grep it for the phrase in parentheses.

## Orientation

- `crates/kerf-core/src/model.rs` + `model/*.rs` (`grade`, `channels`, `motion`, `behavior`, `cut_transcript`, `subset`,
  `check`, `values`, `music_fit`, `score`, `sfx`, `sfx_suggest`, `speed_ramp`, `ramp_edit`, `stabilize`, `generator`,
  `tags`, `thumbnail`, `links`), `svg/`, `captions_import.rs`, `captions_export.rs`, `timecode.rs`, `hostile.rs`,
  `finite.rs`. The domain types (`Asset`, `StreamInfo`, `Timeline` > `Track` > `Clip`, `AssetAnalysis`) and the only
  place timeline math lives; pure and unit-tested, no I/O, no lock.
- Recurring pattern: a new clip/timeline field is omitted from the JSON at its neutral value so old projects and every
  existing graph stay byte-identical (golden oracle, links corpus); TS mirrors are named per topic.

## Map of the full file

The first huge bullet, in order (grep the bold lead): `StreamInfo` `lead`/`color_range`; Clip fields; keyframe easing;
per-property channels (B5a/B5b); mask/crop channels; motion presets (B8a); live behaviours (B8b); `TransitionKind`
families; captions (`CaptionStyle`, `fit_size`); `Delivery` fps; smart crop; caption import; caption export; markers;
beat alignment; diff / `StagedEdit`; partial accept; ripple; `move_clips`; edit modes (roll, slip, slide,
split-and-remove, weld); `split_remove_clips`. Then one bullet each: Invariants; Edit values; Fit music to length;
Music scores; Sound effects; Sound-effect suggestions; Limits that keep a project loadable; Blend modes; Speed ramps;
Stabilization; Animated SVG overlays; Generated clips; Asset tags; Thumbnails; `timecode.rs`; `hostile.rs`.

## Stream and clip basics

- `StreamInfo.lead` comes from the streams' `start_time`s, not the container's; kept only if 1 ms < lead < duration, else 0;
  omitted at 0. Export seeks/places the picture by it (`ClipFx::lead`); a proxy keeps it only if `.lead` padded
  (`media::decoded_lead`) (`a proxy-decoded asset keeps the original's`).
- `color_range` (`pc`/`tv`, schema v10) is `None` when unknown (`a `yuv420p` VP9 / HEVC`).
- `Grade` is normalized by `Grade::normalized`, omitted when empty, not keyable; `grade.ts` mirrors it
  (`normalized by `Grade::normalized``).

## Keyframes and easing

- An `Easing` belongs to the segment *leaving* a key; `Linear` is default and omitted; a `Bezier` is held to the unit square
  (no overshoot) so values never leave the two keys' range and tiny-scale / opacity guards hold (`held to the unit square`).
- An eased segment is a polyline of `EASE_STEPS` (12); `Clip::keyframe_channel` is the one place channel points come from;
  `transform_at` and the export's `keyframe_expr` both read it so still, export and GPU agree exactly
  (`Clip::keyframe_channel` is the`).
- A head trim / slice inside an eased segment or exactly ON a key: `rebase_animation` finds the segment with `a.time <= by`
  (strict `<` dropped the outgoing easing; a hold became a ramp) and bakes the rest into plain keys. The bake is exact for
  the picture, lossy for the picker (Inspector reads Linear) (`That bake is lossy for the picker`).
- Re-keying an existing time keeps its easing; a key added inside a segment SPLITS it (`Clip::insert_keyframe`,
  `Easing::split`): a hold stays held, a curve is cut by de Casteljau (a preset becomes two beziers). Exact when `x1 <= x2`,
  `y1 <= y2`; otherwise clamped and re-fitted; a half with no value change is Linear (`re-fitted`).
- `validate_keyframe` (run by `set_keyframes`) applies the same bezier range check as `set_keyframe_easing`; the diff says
  "easing changed on N keyframes" for an easing-only change (`easing changed on N keyframes`).
- `frontend/src/lib/easing.ts` is the faithful mirror (both suites pin curve and split values bit for bit); change both
  together (`is the faithful mirror (both suites`).

## Per-property channels (`model/channels.rs`)

- `Clip.channels: Vec<PropertyTrack>` keys any one number (`scale`, `pos_x`, `pos_y`, `rotation`, `opacity`, five colour
  numbers, `volume` 0..=4) beside the legacy `keyframes` bundle (`Any one number of a clip can carry keys`).
- ONE resolver, `Clip::property_keys(prop)`; everything (`property_curve`, `property_at`, `transform_at`, `color_at`,
  `volume_at`, `is_animated`, `held_scale`, `pan_extent`, `fit_order`) is a view of it so still, preview, plan and export
  cannot disagree. A tracked number is driven by its track, held to the static setters' range on read (`Property::clamp`);
  an untracked transform number reads the bundle's keys as stored (`There is one resolver`).
- Nothing is converted behind anyone's back: no migration on load or write; the first per-property write detaches just that
  number (`channel_mut`); an empty track is kept while a bundle exists ("static, whatever the bundle says"); `prune_channels`
  drops it once the bundle is gone and must run after every op that can empty the bundle or a track (`set_property_keys`,
  `insert_property_key`, `Project::set_keyframes`; harness `setKeyframes` mirrors) (`Nothing is converted`).
- Legacy ops: `set_keyframes` replaces the bundle ONLY (empty clears the bundle, not tracked numbers); `clear_keyframes`
  clears bundle and transform tracks, leaves colour and volume; `add_keyframe` writes the bundle (and the driving track for
  an asked-for number); `set_keyframe_easing` without a property shapes the key in the bundle and every transform track having
  one there (`The legacy ops keep their meaning`).
- While a number is keyed its static value (`set_volume` / `set_color` / `set_transform`) is NOT used; tool descriptions say so
  (`While a number is keyed its static value`).
- Surface ops: `Project::set_property_keyframes` (checked by `Property::check`, at most `MAX_CHANNEL_KEYS` = 1000, bezier
  range; no keys = static), `set_transform_at` / `Clip::pose_transform` (one revision; `channels.ts` `poseTransform` mirrors),
  `set_property_easing`, `copy_keyframes` (negative offset cuts the head with `rebase_head`; replaces destination tracks;
  refuses a number the source lacks and the same clip twice), `set_property_tracks` (a number twice or none refused; one
  dope-sheet gesture) (`set_property_tracks(clip, tracks)`).
- Edits keep channels where they were: every head move (`Timeline::slice`, split-and-remove left, roll/slide next clip,
  linked partner's trim) runs `rebase_animation` over bundle AND `rebase_channels`; `upsert` splits a segment;
  `detach_audio` carries a keyed volume via the fader ratio (`Edits keep channels where`).
- The diff judges on the keys that DRIVE a number, not on whether a track exists (a bundle-animated number held static by an
  empty track diffs `opacity keyframes 2 → 0`), else `apply_staged` discarded a proposal whose render changed
  (`judged on the keys that drive the number`).
- A keyed volume is held to 0..=`MAX_CHANNEL_VOLUME` (4), a static one is not: `detach_audio` must not fold the fader ratio
  into keys past the cap (it would silently lower the sound); it takes the equal-fader route (lane whose fader equals the
  picture track's, else a new track) and carries keys unchanged (`A keyed volume is read held`).
- `Timeline::split_clip` rebases the right half's animation (`rebase_animation(at - start)`); `cut_range_pieces`
  (`cut_clip_range`, `remove_silence`, linked cut) re-times its tail piece the same way. A split puts each fade on the half
  holding its edge (left `fade_in`, right `fade_out`, each clamped; `transition_in` stays left) or a faded clip dips to black
  at every split (`A split also puts each fade`). Unit tests are mirrored by name in `links.test.ts`.
- Mask numbers (`mask_x|y|width|height|feather`, held to `Mask::normalized` ranges, `Clip::mask_at`) and crop are keyable;
  the crop is a zoom + pan, NOT four edges (`crop_zoom` 1..=`MAX_CROP_ZOOM` 10, `crop_x` / `crop_y` -1..=1) because a
  per-frame crop size is something almost nothing downstream follows; it is a window of the statically cropped picture
  (`Clip::crop_window_at`), no static value, on top of the four static edges (`The mask's numbers and the crop are animatable`).
- `crop_animated` = "some zoom key zooms in": a zoom held at 1 draws nothing and is not refused by the GPU plan
  (`crop_animated` is "some zoom key`).
- `mask_animated` = a mask number keyed AND the clip has a mask; keying one on a clip without a mask is refused
  (`check_keyable` in `set_property_keyframes` / `set_property_tracks` / `copy_keyframes`; no keys always fine;
  `copy_keyframes` with no `props` leaves mask numbers behind); `set_mask(None)` drops the mask's keys
  (`keying one on a clip without a mask`).
- `frontend/src/lib/channels.ts` is the faithful TS mirror (`upsertKey`, `rebaseHead`, `maskAt`, `cropWindowAt`,
  `keyableProblem`; its copies own their bezier easing via `ownEasing`, a reactive proxy breaks `structuredClone`); both
  suites pin the same samples (`is the faithful TS mirror`).

## Motion presets (`model/motion.rs`)

- Pure data expanding into channels: no render path, stored field, migration or golden change (`Motion presets (B8a`).
  Entry points: `MotionPreset`, the `SPECS` table, `Clip::apply_motion`, `Project::apply_motion_preset`
  (`project/motion.rs`), `motion_presets()`; `motion_presets_rendered.rs` is `#[ignore]`d.
- A direction names where the picture comes FROM / goes TO (unlike `TransitionKind`'s travel); every track runs `at` 0..1
  and a bezier cannot overshoot, so an overshoot is its own key (`a direction names where the picture comes`).
- The reference is the pose (`Clip::property_at` sampled before writing): entrance ends on the pose at the window's end
  (`Base::End`), exit starts from it (`Start`), pulse/wiggle/bounce ride the line between edge poses (`Trend`), making a
  double application a no-op. `Replace` is relative to the STATIC value so an entrance does not park the clip where a
  half-finished slide ended (`The reference is the pose`).
- `apply_motion` is all or nothing; `duration` is clamped to the clip, under `MIN_MOTION_SECONDS` (0.01) refused; `intensity`
  0.25..=2 (scale never under `TINY_SCALE`, opacity in 0..=1). `Add` replaces keys inside the window and keeps those outside,
  splitting the segment an edge falls in; the last-applied preset wins where windows meet; `Replace` writes exactly the
  preset's keys; a bundle-driven number detaches alone (`apply_motion(preset, mode`).
- `Project::apply_motion_preset(clip_ids, ..)` is one `edit_timeline` revision, all or nothing, a clip named twice once,
  audio-track clip refused, a LOCKED TRACK IS NOT refused (a lock guards layout, keyframes are look:
  `Timeline::ensure_editable`), linked partners untouched, an agent's staged (`a locked track not`).
- `fixtures/motion-presets-corpus.json` feeds the TS mirror (`KERF_BLESS_CORPUS=1 … -- motion_corpus`).

## Live behaviours (`model/behavior.rs`)

- `Clip.behaviors` (absent when empty, schema v20), at most `MAX_BEHAVIORS` = 8 (`Rule::ClipBehavior`); `breathe`
  `amount <= MAX_BREATHE` (0.9) so scale never reaches 0; shake phases come from `seed` by an integer hash with no libm so
  the export text prints no transcendental constant; shake moves position only (`Live behaviours (B8b`).
- `u = local + phase`, local is the clip-local timeline clock the keys read, so speed and reverse change nothing (`local is the`).
- Ranges (`MAX_SWAY_DEGREES` 180, `MAX_BEHAVIOR_HZ` 30, `MAX_SPIN_TURNS` 10, `MAX_DRIFT` 5, `MAX_SHAKE` 0.25, `|phase| <=
  1e6`) are refused on write (`Behavior::check`, `check_behaviors`) AND held on read (`Behavior::sanitized`) so a hand-edited
  file or NaN cannot reach a filter; a behaviour that moves nothing is not live (`is_neutral`): flags and graph unchanged
  (`refused on write`).
- One resolver, two views: `property_at` / `transform_at` = picture (`base_at` + `behaved`; zoom multiplies, the rest add, in
  list order = export text order); `base_at` / `base_transform_at` = pose WITHOUT behaviours, which every edit that writes keys
  must sample (`rebase_animation`, `pose_transform`, `add_keyframe`, presets, bake) or the wobble is written into keys and
  added again (`One resolver, two views`).
- Flags include behaviours: `is_driven(prop)` (what the chain is built from), `is_animated`, `zoom_animated`,
  `placement_animated`, `picture_moves`, `rotation_turns`; `pan_extent_over(horizon)` adds their reach so a behaviour-only
  clip takes the same moving-zoom / sub-pixel / keyed-rotation chains as a keyed one (`The flags are`).
- A head move shifts `phase` by the cut (`rebase_behaviors`, run by `rebase_animation`); a plain `Project::trim` leaves
  behaviours clip-local like keys; a split keeps them on both halves (`A head move shifts`).
- `Clip::bake_behaviors(fps, max_keys)` is exact at the fps frame grid and REFUSES past `max_keys` (`MAX_CHANNEL_KEYS`)
  rather than thin (`bake_behaviors(fps, max_keys)`).
- `copy_keyframes` / `clear_keyframes` do not touch behaviours; `Project::set_behaviors(ids, list)` (one revision, empty
  clears, an audio-track clip refuses the lot, locks and linked partners do not matter) and `Project::bake_behaviors(ids, fps)`
  are in `project/behavior.rs`; OTIO keeps the clip whole in `metadata.kerf` (`do not touch behaviours`).
- `fixtures/behaviors-corpus.json` (`KERF_BLESS_CORPUS=1 … -- behaviors_corpus`): its freshness test compares NUMERICALLY
  (the numbers are the blessing machine's libm), not byte for byte; `with_libm_ulps(n, f)` proves corpus and golden oracle do
  not read that digit (`compares them **numerically**`).

## Transitions

- `TransitionKind` has three families that decide the render: dip, dissolve (`Crossfade`), motion (`Slide*` / `Push*`,
  direction = travel). The enum answers for its family (`dip_color` / `slide_from` / `pushes` / `overlaps`) so the engine
  never matches on eleven variants; `wire_names` derives the expected-kind list both surfaces put in errors
  (`three families, and the family decides`).
- Dissolve/motion transitions borrow the outgoing clip's unused source handle; a clip trimmed to the end of its footage has
  none and degrades to a hard cut (a dip needs none) (`borrows the outgoing clip's unused`).

## Captions

- Captions are timeline math: a transcript is in SOURCE time, an overlay in TIMELINE time; `Timeline::captions` projects
  segments through the clips that show them (`Clip::source_span_to_timeline`: trim/speed/reverse), so cut-out words get none;
  it reads `for_render` (a muted track is uncaptioned) (`Captions are timeline math`).
- A segment with `words` chunks over words (`word_chunks`): a line ends at the next line's start or, across a pause, at its last
  word's end + `CAPTION_HOLD` (0.5 s), and is kept only if the span its words are *spoken* in meets the clip. A segment
  WITHOUT words is timed by character share so old transcripts caption byte-identically; too-short lines merge; no two lines
  are ever on screen at once (`A segment **with `words`**`).
- `TextOverlay.generated` marks what captioning wrote so regeneration replaces its own set and leaves a typed title alone
  (`marks what it wrote`).
- `CaptionStyle` (`Lines`: 4 words / 28 chars, 5%; `WordPunch`: one word, 11%) moves count, size, position and flicker floors
  together; word punch has its own `MIN_WORD_CAPTION` / `MIN_WORD_VISIBLE` or words would merge back into lines
  (`is the look**, and one decision`).
- `CaptionOptions` = style + optional overrides, `resolve()`d to `CaptionLayout`; `CaptionOptions::default()` must stay
  unchanged so existing calls caption identically (`is that style plus **overrides**`).
- `fit_size` shrinks a caption to the frame (`drawtext` neither wraps nor scales); `fontsize` cannot be an expression over
  `text_w`, so it is estimated from char count against `Timeline.format`'s aspect; an unframed project assumes 16:9 so
  nothing that never picked a frame moved (`fontsize` cannot be an expression`).
- `Track.duck` = sidechain ducking on export. `Fit` (`Contain`, `Cover`, `Blur`) and `Delivery` live here; `engine::cli`
  re-exports `Fit` (`Fit` and `Delivery` live here`).
- `Delivery.fps` (schema v12): `Delivery::with_fps` / `check_delivery_fps` hold it to `MIN_DELIVERY_FPS..=MAX_DELIVERY_FPS`
  (1..240, `InvalidArgument`) and snap an NTSC rate within 0.005 to the exact `n * 1000 / 1001`; the engine reads
  `Delivery::rate()` (out-of-range stored value ignored); an absent fps is not written (`skip_serializing_if`) so old
  deliveries store byte-for-byte what they did; `for_delivery` keeps the project's rate for a variant naming none
  (`carries an optional **frame rate**`).
- Smart crop: `SalienceMap::crop_for` with `CENTER_BIAS` (a flat map gives the plain centre crop); `needs_crop`
  short-circuits footage already the delivery shape (`**Smart crop** is here too`).

## Caption import (`captions_import.rs`) and export (`captions_export.rs`)

- Import goes through the transcript code: `Timeline::captions` = `project_through_clips` + `settle_caption_lines` and
  `place_cues` calls the same two, so chunking, flicker floors, one-lane rule and `fit_size` cannot drift
  (`settle_caption_lines`).
- `CaptionTimeBase::Source(asset)` is `captions` over a one-asset map (`for_render`); `Timeline` (default) takes times as they
  stand, clipped to `for_render().duration()`, and a muted track does NOT silence it; an empty timeline uses
  `EMPTY_CUT_WINDOW` (a day); `CaptionImportRequest.offset` (seconds, ±100 h) is added to every cue before placement
  (`CaptionTimeBase::Source(asset)`).
- Parsers (SubRip, WebVTT `parse_vtt`, ASS/SSA) are pure and tolerant; what they cannot read is COUNTED, never fatal
  (`skipped_lines`); file styles/positions are not imported (`CaptionStyle` decides); entities decode AFTER markup so
  `&lt;i&gt;` stays text; no speaker field (`skipped_lines`).
- Control characters are stripped from a cue (a NUL in an ffmpeg argv fails every spawn) and `escape_drawtext` drops them
  too, for typed titles as well (`a NUL in an ffmpeg argv fails`).
- The caps are guards, each an incident: 5 MiB a file, 10,000 cues (parsing stops), 100,000 words, 2,000 chars a cue, 16,384
  chars a line refused BEFORE it is cleaned, 20,000 captions written. Cleanup scans must stay linear (`strip_markup` looks for
  `>` only within 256 bytes and up to the next `<`; `strip_braces` copies the rest when no `}` remains); the merge is still
  quadratic within a cue so the per-cue and per-import caps bound it; perf tests use generous wall-clock limits that catch an
  algorithm or a lost cap, never a busy machine (`The caps are guards, not tidiness`).
- Parsing is OUTSIDE the project lock: `parse_captions` (after `read_caption_file`: `.srt`/`.vtt`/`.ass`/`.ssa`, regular file,
  size) is a pure static step yielding a `CaptionFile`; `Project::import_captions(&file, req)` only places it inside the
  `edit_timeline` closure; Tauri commands and MCP tool read/decode/parse on the blocking pool (`**Parsing is outside the project lock.**`).
- Every cue is accounted for once: `cues == placed + dropped_outside + dropped_short + dropped_overlap`; simultaneous cues
  keep file order (`SimultaneousLines::ByOrigin`) (`accounted for exactly once`).
- Imported overlays are `generated` ON PURPOSE: importing replaces earlier generated/imported captions, a later
  `generate_captions` replaces an imported set (two sets would put two lines on screen), typed titles are never touched
  (`Imported overlays are `generated`, on purpose`).
- Import is one `Import captions` revision computed inside the `edit_timeline` closure (an agent's lands in its proposal); a
  refused import (no cues, nothing reaching the cut, asset not on the timeline, more than 20,000 captions) writes nothing
  (`one `Import captions``).
- Export: `cut_captions` = the timeline's GENERATED overlays only (a typed title is not a caption), clipped to
  `[0, duration]`; `write_srt` / `write_vtt` round to whole ms (end >= start + 1 ms), turn `-->` into `->` in SRT text,
  escape `& < >` in VTT, and read back through the importers unchanged (`**Caption export**`).

## Cut transcript (`model/cut_transcript.rs`)

- `Timeline::cut_transcript` shares the caption walk (`segments_in_cut`) so captions did not move; it reads `for_render` and
  only clips with `source_audio` as the export's `clip_sounds` decides (a detached picture says nothing, its audio clip does)
  (`**The cut's transcript**`).
- A word belongs to the clip that plays its midpoint (half-open source window), clipped to it: a word split by a cut is read
  once, a sliver not at all. Lines sort by start (stable); one asset's line at the same source and timeline start twice is
  kept once, first in track order (`A word belongs to`).
- `Project::cut_transcript_inputs` / `CutTranscriptInputs::read` split it around the lock; `untranscribed` never lists a
  synthesized effect (`cut_transcript_inputs`).

## Markers, beats

- Markers: `duration`, `comment`, `kind` are `#[serde(default)]` and omitted at defaults (byte-identical timelines);
  `MarkerPatch` + `Marker::apply` is the one validated edit (time/duration >= 0, comment <= `MAX_MARKER_COMMENT`, controls but
  `\n` / `\t` dropped); `slice` clips a marker's span (`**Markers** carry`).
- Beat alignment: the rhythm pass replaces an autocorrelated `Tempo` with `Tempo::from_grid` when a bar grid is fitted so
  ruler, snapping and cut-to-the-beat share one grid; `beat_grid` is gated by `BEAT_MIN_CONFIDENCE` (mirrors the ruler);
  `Track::align_cuts_to_beats` retrims at the OUTGOING edge (`source_in` for a reversed clip), preserves gaps, stretches only as
  far as the footage goes (`**Beat alignment** lives here too`).

## Diff, staging, partial accept

- `Timeline::diff` -> `TimelineDiff`: everything matched by id (a reordered track reads as a few moves; a removed track is one
  entry); `DiffKind` keeps add / cut / move / retrim apart. Every entry has a stable `id` (`<kind>:<clip|text|marker|track
  id>`), unique per diff and unchanged as a proposal grows, so a reviewer can send back accepted ones
  (`**What changed between two cuts**`).
- `StagedEdit.owner` (session id) is Rust-side only (`#[serde(skip)]`): never to the webview or another agent;
  `StagedAssetChange` ids are `asset_projection:<id>` / `asset_tags:<id>`; `tag_edit` composes the agent's tag edits into one
  `TagEdit` run over the tags as they are when read (`is a pending proposal`).
- Partial accept (`Timeline::merge_subset(base, proposed, picked_ids)`) writes each pick onto the LIVE cut by identity, never
  by position. Start, footage window and the rest are separate picks. What cannot be taken apart comes with the pick
  (`SubsetMerge::auto_included`: a linked partner's entries, the new track a picked clip lands on, moves off a removed track);
  a pick leaving two clips overlapping (`check_subset_lanes`, against what already overlapped) or pulling linked clips out of
  step (`first_sync_break`) is refused naming track and time; a pick whose target the user removed is `skipped`, not fatal
  (`**Partial accept** is`).
- The remainder is `Timeline::rebase_rest(base, proposed, rest_ids)`: the same merge of unapplied changes without the closure
  and the two refusals (it is the cut to show, not a change to write); what no longer fits is dropped and counted
  (`rebase_rest`).
- `Timeline::check_own_lanes(before, ids)` is the overlap rule for a clip an edit itself placed (`add_clip_to_timeline`,
  `trim`, voiceover): judged AFTER the ripple, and not for an overlap the clip already had (`check_own_lanes`).

## Ripple and multi-clip edits

- `Timeline::ripple_from(before)`: per track, by id, shifts clips left *starting where they started* by the net length change
  of what the edit did ahead of them (right trim, speed, removal, or an add landing on footage that was there; an add in free
  space and every append move nothing) (`**Ripple** is here too`).
- Rules that were bugs: a left-edge trim keeps the clip's start (GUI commits `source_in` plus a later `timeline_start`; both
  forms give one result, only when the trim is the whole edit on the track); a split shifts nothing; moves never ripple;
  clips the edit itself moved are not followers; tracks are independent EXCEPT linked clips (partners follow by the same
  amount: sync lock `conform_links`; only clips follow, never the partner's lane; `core-links.md`); a locked track never moves;
  overlays and markers do not move (`a **left-edge trim keeps the clip's start**`).
- Ripple NEVER produces an overlap: if shifting would leave a touched clip overlapping another or before 0, that track is
  returned as the edit made it (`produces an overlap**`).
- `Timeline::move_clips` / `remove_clips` are pure, all-or-nothing: a `ClipMove` is a clip, an ABSOLUTE start and an optional
  same-kind track; checked as a group (moving clips may pass through places they leave, never onto each other or a clip that
  stays); a locked track, a start before 0 or a clip named twice refuses the lot (`are the pure, all-or-nothing`).

## Edit modes (roll, slip, slide, split-and-remove)

- Each mode CLAMPS AND REPORTS (`EditOutcome {requested, applied, clamped, clips}`), erroring only when the clamp leaves
  nothing to move (a no-op records no revision); clamps: the footage (`SourceLimits`, `Asset::source_limit`, infinite for a
  still) and `MIN_EDIT_CLIP` (0.05 s); each has a `*_range` returning the `DeltaRange` a drag reads (`**Edit modes** are here too`).
- Roll: `a`'s end and `b`'s start move together; clips must touch within `ADJACENT_EPS` (1 ms), `a` first. Slip: window
  shifts, position and length do not; `delta` is SOURCE seconds, positive = clip starts later in its footage (a reversed
  clip's window moves the mirrored way so the sign always means the same on screen); a still is an error. Slide: touching
  neighbours give way; a neighbour across a gap is never trimmed (the clip stops at it); the last clip extends the track
  (`**Roll**`).
- `split_remove(clip, at, Left|Right)`: the survivor keeps the clip's id so `ripple_from` reads an ordinary trim (a left removal
  holds the start and closes the track under ripple, else leaves the gap); it drops what belonged to the removed half
  (`fade_in` + `transition_in` left, `fade_out` right) (`**Split and remove**`).
- Shared: a head move re-times keyframes and reframe keyframes (`Clip::rebase_animation`, also used by `Timeline::slice`); a tail
  move or slip/slide leaves them clip-local; fades clamp into a clip that shrank; a still's window is only written on its
  out-point; locked tracks refuse; every op validates before it mutates (`Shared rules`).
- Float residue is welded: roll/slide compute one side from a window and the other from `start + delta` (ulps apart), so the
  cut is closed exactly (`weld`: follower starts at `leader.timeline_end()`) and the far edge pulled back by shortening its
  window point (`fit_end`), only for float noise (`DIFF_EPS`), never a real sub-millisecond gap (that is data)
  (`**Float residue is welded**`).
- `split_remove_clips(&[ClipCut{clip_id, at}], side)`: all or nothing, AT MOST ONE CLIP PER TRACK (a lane trimmed in two places
  has no single edit point for `ripple_from`) (`split_remove_clips`).
- A same-length window shift diffs as `Slipped clip … footage +0.03s`, signed as `slip_clip` is, not a `+0.0s` trim
  (`A same-length window shift diffs as`).

## Invariants (`model/check.rs`), edit values (`model/values.rs`), limits

- `Timeline::check()` refuses what no edit should leave: non-finite time/gain/speed; `source_out <= source_in`; negative fade
  or transition; a text/SVG overlay not ending after it starts; non-finite keyframe or marker time; a duplicated track, clip or
  overlay id (`**Invariants** (`model/check.rs``).
- `Project::run_edit` takes `known_violations()` before the edit and `check_new(&known)` after, on EVERY edit (live and staged)
  before the transaction commits: only a violation the edit INTRODUCED is refused, so a file an older build left broken is
  still editable; a failing edit persists nothing (`known_violations()` before the edit`).
- Overlaps are NOT checked: Kerf allows them (explicit `add_clip`, a trim into the next clip, old files); ripple treats an
  overlap the edit did not make as old news (`**Overlaps are not checked**`).
- Setters hold values to the shared ranges (`Transform::check_values` / `check_crop`, `Color::check_values`,
  `Clip::check_fades`, `Clip::check_trim` + `Asset::check_source_out`, `TextOverlay::check_values` / `check_span`,
  `TextKeyframe::check`) before changing anything; the GPU preview's drag frames share them (`PreviewDraft::apply`) so a
  preview never shows a value the edit would refuse: change a range THERE, not in a setter (`change a range here, not in a setter`).
- Speed is `MIN_SPEED..=MAX_SPEED` (0.01x-100x, either sign, also every ramp key); `set_speed` and the ratio `set_speed_linked`
  hands each partner hold it (audio tempo is one `atempo` step per doubling) (`**Limits that keep a project loadable.**`).
- `Mask::normalized` maps a non-finite size to a default instead of clamping NaN (`non-finite size to a default`).
- `Project` writes run a serde walk (`finite.rs`, `first_non_finite`) over every timeline before storing, because serde_json
  writes NaN as `null` which a plain number field cannot read back and the project would stop opening; setters still refuse
  non-finite input themselves; the walk is the net under every op that computes a number (`first_non_finite`).

## Fit music to length (`model/music_fit.rs`)

- `plan_music_fit(music, target, rate)` keeps the intro and ending and jumps only between repeating phrases (`PhraseMatch`: 8
  bars cost 2, 4 bars cost 3, never two jumps in a row); the DP picks the bar count nearest the target, a TIE GOES TO THE LONGER
  walk (a fade can shorten it; the shorter leaves picture without music), then fewest jumps (`plan_music_fit(music,`).
- The search runs to `max(wanted, bars) + bars` (capping at `wanted + 1` missed the arrangement just above); consecutive bars
  merge so a continuous join is never a splice; each `output_start` is the exact running sum rounded once (no drift)
  (`The search runs to`).
- `music_fit_clips` copies clips from the music clip (gain, audio effects kept; keyframes and link dropped); each splice is
  crossfaded over ONE 10 ms window centred on it: outgoing `source_out` and incoming `source_in` / `timeline_start` move half a
  window early and the incoming `Crossfade` is a full window (offset windows played both copies at full gain); an overrun
  with `fade_out` is cut at the target and faded over `FIT_FADE_S` (`window centred on it`).

## Music scores (`model/score.rs`, `score/{chord,arrange,align}.rs`)

- `MusicScore` is 4/4 only; `bpm` 40-240; `seed` 0..=`MAX_SEED` (2^53 - 1, the largest integer a JS number keeps through JSON;
  bigger refused); rides on `Asset.music_score`, renders through `engine::compose` (`engine-media.md`); types refuse unknown
  fields (`deny_unknown_fields`); limits keep a render in memory: 100 sections, 128 bars each, 400 bars, 600 s, 256 hits
  (`limits keep a render in memory`).
- Chords are names or roman numerals in the key's own scale, one per bar, a shorter list cycling; `resolve()` checks everything
  and errors name `sections[i] (kind)` and the chord; `chords` are AS PLAYED (the last bar of an outro that ends the piece is
  already the tonic) so notes, approach note and analysis chroma agree (`roman numerals in the key's own`).
- `duration_s` = bars + a release tail (`RELEASE_TAIL_S` 1.5 s, at most 0.9 of a bar); `beat_grid()` exact;
  `music_analysis(duration)` writes bar structure from the chords through the same `phrase_matches` as audio analysis so
  `fit_music` works; `rhythm(duration)` is full-confidence; `analysis::ScoreRhythm` serves it for a composed asset so
  re-analysis never replaces it with a detector's guess (`RELEASE_TAIL_S`).
- `expand(score) -> Arrangement` is DETERMINISTIC; all variation comes from a seeded xorshift64* (pinned by a test) keyed by
  seed, role, SECTION AND BAR, so editing a section never re-rolls the ones before it (`xorshift64`).
- Section logic (`drum_tier`, `bass_mode`, `keys_mode`, `lead_mode`; energy < 0.4 light, < 0.75 mid, else full): a drop is
  every layer plus crash and impact (not when one drop follows another); a breakdown has no kick and no bass; an outro's last
  bar resolves on the tonic held into the tail (`drum_tier`). Harmony is voice-led (`chord::voice`) (`**Harmony is`).
- Score to picture `plan_score_to_picture(...)`: tempo searched in 0.05 % steps within ±`max_tempo_change` (default 6 %),
  downbeat counts double, and it changes ONLY when that beats the score's own tempo; impacts go on cuts on a downbeat; a
  `dropout` + `riser` before a `drop` a cut lands within a beat of (no riser after a build); hits already there are not
  added twice (`**Score to picture**`).
- The score's own hits stay at their ABSOLUTE seconds, so a tempo whose piece would end before the last of them is never a
  candidate; added hits stop at `MAX_HITS` (a report note counts what was left out); the returned score is `validate()`d
  (`absolute seconds`).

## Sound effects (`model/sfx.rs`) and suggestions (`model/sfx_suggest.rs`)

- `SfxSpec` rides on `Asset.sound_effect`, `{"kind":"whoosh"}` alone is valid, `deny_unknown_fields`; `validate()` names the bad
  field (`duration_s` > 0 finite; `pitch` ±12; `intensity`, `tail` 0..=1; `seed` <= `MAX_SEED`) (`**Sound effects**`).
- `catalogue()` is the static table, one `SfxPreset` per kind in `SfxKind::ALL` order (21 kinds in five categories)
  (`21 kinds in five`).
- `resolved_duration_s()` is held to whole ms and CLAMPED to the kind's range (a 10 s whoosh is 3 s), not refused; `resolved()`
  spells it out and that spec is what is rendered, hashed and stored (`**`resolved_duration_s()`**`).
- The anchor is the hit point (`SfxAnchor`: `hit`, `peak`, `end`); `reverse` mirrors it (`len - anchor`); synthesis builds on
  the same anchors and a test pins the loudest part there (`The anchor is the hit point`).
- `level_db` (<= 0) is the file's peak below the -1 dBFS every render is scaled to, set from loudness so kinds sit together
  in a mix and not by their peaks (`level_db` (≤ 0)`).
- Suggestions: `suggest_sound_effects(&SfxSuggestInputs, &SfxSuggestRequest)`, inputs from
  `Project::suggest_sound_effects_inputs`; pure. A moment's `SfxSource` and score: `marker` 1.0, `music_drop` 0.90-0.95,
  `opening` 0.80, `title` 0.75, `transition` 0.70, `punch_in` 0.65, `cut` <= 0.60 (`**Sound-effect suggestions**`).
- A marker `sfx:<kind>` (or bare `sfx` = impact) is PINNED: never thinned away, it thins what is near it (`**pinned**`).
- Music drops: analysed music counts only audio classified `Music` >= 0.5 at a 4-bar boundary where the four-bar mean chroma
  either side is >= 0.25 apart; riser of two bars (1.5-4 s, never longer than the time before) AND an impact; no riser when
  under 1 s of music precedes, when the drop follows a `Build`, or the score has a `Riser`/`Whoosh` hit there; no impact where it
  has an `Impact`/`Dropout` hit (`music_drop` 0.90`).
- A cut is a join of two adjacent pictures on a video track that is not a continuation of the same shot, not a title, not
  hidden by an opaque full-frame clip above; a seamless split is no cut, but one whose second half is a punch-in gets the
  punch, never a plain cut (`A cut is a join of two adjacent`).
- Density: spacing subtle 4 s / balanced 2 s / punchy 1 s; cuts and punch-ins need both shots >= 1.5 s at balanced and do not
  count at subtle (`**Density**`).
- What cannot be suggested is dropped BEFORE thinning so it holds nothing back: a moment within 0.25 s of an existing effect
  clip, a part whose kinds the request's `categories` / `kinds` disallow, a part `add_sound_effects` would refuse (`lands`
  asks `model::sfx::span`, the rule `span_for` places by) (`dropped **before**`).
- Each source has preferred kinds and fallbacks, so `kinds: ["swipe"]` turns cuts into swipes and `categories: ["impact"]`
  keeps a drop's impact and loses its riser (`each source has preferred kinds`).
- Thinning keeps markers, then highest scores (ties: earlier, then source order), at most `MAX_SFX_PER_CALL`; survivors
  alternate kind within (source, part) and take seed = running index so no two are one render; gain -3 dB, -8 dB under a
  transcribed word of a clip that plays its own sound (`Thinning keeps markers`).
- `range` (half-open) filters the answer of the WHOLE cut so a range holds exactly what the whole call puts there
  (`filters the answer of the **whole** cut`).

## Blend modes (`BlendMode`)

- `Clip.blend_mode` (absent at `normal`; schema v17): only the separable modes FFmpeg's `blend` computes as the textbook
  defines; its soft light, dodge, burn and subtract are other formulas and NOT offered; an unknown name does not read
  (`**Blend modes**`).
- `BlendMode::ffmpeg` SWAPS `overlay` and `hard_light` (FFmpeg's `overlay` conditions on the top input); `apply` is the formula
  the render tests hold FFmpeg to (`which swaps `overlay``).
- Split, duplicate, paste and link carry it (a detached sound is a fresh clip at `normal`); OTIO and EDL have no blend mode:
  report says `clip blend modes …`, OTIO keeps it in `metadata.kerf` (`Split, duplicate, paste and a link carry it`).

## Speed ramps (`model/speed_ramp.rs`, `model/ramp_edit.rs`)

- `Clip.speed_ramp: Vec<SpeedKey>` (absent when empty; schema v14): key times are clip-local TIMELINE time; the direction is
  still `speed`'s sign and its magnitude is NOT read while a ramp is set (`**Speed ramps**`).
- A ramp the map cannot read (`ramp_is_valid`: 1..=`MAX_SPEED_KEYS` (16) keys, strictly increasing times within ±`MAX_RAMP_TIME`
  (1e6 s, so the map never returns NaN), speeds in range) is ignored (constant speed plays) and flagged by `Rule::ClipSpeedRamp`:
  old files open and no edit introduces one (`A ramp the map cannot read`).
- ONE map: `Clip::along(local)` / `local_at_along(along)` (+ `speed_at`, `mean_speed`, `is_ramped`); `duration`,
  `source_to_timeline`, `timeline_to_source` read it, and anything converting between the two clocks must too, NEVER
  `speed_mag()`; an unramped clip keeps its old arithmetic to the last bit so no graph, golden or corpus case moved (`**One map**`).
- Edits keep every frame on its footage: a head move rebases the ramp (`Clip::rebase_ramp`, via `rebase_animation`: `move_head`,
  split right half, `slice`, `cut_range_pieces` tail, linked trim; and `Project::trim` through `follow_head`): key at 0 with the
  speed at the cut (a hold keeps holding), earlier keys dropped; keys that round onto one another are dropped, never the ramp
  (`**Edits keep every frame on its footage.**`).
- A tail move (`move_tail`, `Clip::tail_shift`) leaves the keys; `handles` (what roll, slide and carried trims clamp to) come
  through the ramp; a slip keeps keys and length; `slice`, the beat snap, `fit_end` and `cut_range_pieces`' removed span go
  through the map (`A *tail* move`).
- `Project::set_speed_ramp(clip, keys)` keeps the window and changes the length, so it ripples like `set_speed`;
  `check_speed_ramp` takes <= 16 keys at `0..=MAX_RAMP_TIME`, finite in-range speeds, no two at one time; no keys clears;
  clearing an absent ramp is an error. `set_speed` REPLACES a ramp (on every clip of a link group); linked partners get the same
  ramp (`core-links.md`); a title must have none (`keeps the window and changes the length`).
- OTIO and EDL cannot carry a ramp: they write the average speed (`export_speed`) with a warning; audio sync and music fit
  refuse a ramped clip (`OTIO and EDL cannot carry a ramp`).

## Stabilization (`model/stabilize.rs`)

- `Clip.stabilize` (absent when `None`; schema v18): `strength` 0..=1, `zoom` 0..=`MAX_STABILIZE_ZOOM` (30) or `None` =
  automatic; `check` refuses out of range, `normalized` holds a stored one for readers (`**Stabilization**`).
- Only the SETTING is in the timeline; the measurement is a cache (`crate::stabilize`): split/trim keep both, paste/duplicate
  copy the setting, and it is NOT carried to linked partners (`Only the setting is in the timeline`).

## Animated SVG overlays (`Timeline.svg_overlays`, `svg/`)

- Drawn between `start` and `end`, fit contain in `x, y, width, height` (frame fractions), above clips and below text overlays
  (captions stay on top). `svg` stores the SANITISED source, so stored == rendered; `svg::sanitize` is an allow-list run once on
  the way in (`**`svg` stores the sanitised source`).
- Stripped: scripts, `foreignObject`/`iframe`/`audio`/`video`, elements outside resvg's vocabulary, `on*` and foreign-namespace
  attributes, anything but `#fragment` links, external `url()`, `@import`, `@keyframes`, `animation*`/`transition*` (warning
  "use SMIL"); a plain DOCTYPE is cut, one with an internal subset refused; caps 128 KiB, 20,000 elements, depth 64, 2,000
  animations, 1,000 values each, `MAX_SVG_OVERLAYS` = 50.
- An `<image>` may carry `data:image/png|jpeg|gif|webp` whose header says at most 16 Mpx (the decoder allocates what the header
  claims), else its `href` is cut with a warning; JPEG size comes from our own marker walk (`jpeg_size`; `imagesize` mis-reads
  `0xFF` fill bytes) (`whose header says at most 16 Mpx`).
- Depth is read off the BYTES before roxmltree sees the text (`nesting_exceeds`): roxmltree recurses per open element with no
  limit and ~3,500 nested `<g>` abort a 2 MiB thread stack (`Depth is read off the bytes`).
- An animation's `attributeName` must be an XML name with at most one prefix (`is_attribute_name`) because `at` writes it into
  the document as markup (injection); only `xlink:href` of the XLink attributes survives, only when no unprefixed `href` is
  beside it; roxmltree's bare-name `node.attribute("href")` matches `xlink:href` too: use `sanitize::plain_attribute` for "no
  namespace" (`must be an XML name with at most one prefix`).
- Sanitising is a SPLICE of byte ranges, not a re-serialisation, so a second pass changes nothing (`It is a splice, not a re-serialisation`).
- `SvgDoc::parse` refuses what resvg cannot size (a `viewBox` or pixel `width`/`height` is REQUIRED: an animation would change
  a bare document's size per frame); `<use>`/`feImage` expansion is counted from the XML FIRST (more than 100,000 drawn nodes,
  128 levels or a cycle is refused; usvg would expand a cycle to its million-node limit before noticing; a ring of gradients
  is fine); a `url(#id)` in `fill`/`stroke`/`mask`/`clip-path`/`filter`/`marker-*` (attribute, every `style` declaration, or a
  `<style>` rule) counts the target again per element that draws it; `<image>` pixels are summed the same way (more than 64
  Mpx decoded in a frame is refused; `SvgDoc::widest_image_pixels`) (`are the evaluator`).
- `SvgDoc::at(doc_t)` returns a STATIC SVG for one moment, which every renderer is handed, so still, stream and export are one
  picture; a CSS property is written by appending `;prop:value` to `style` (no leading `;` after an empty or `;`-ended style:
  resvg's CSS reader drops it); an animation's start value is the element's own, else for an inherited property
  (`values::is_inherited`) the nearest ancestor's, else the initial one (`returns a *static* SVG for one moment`).
- Subset: `animate`, `animateColor`, `set`, `animateTransform`; syncbases resolved once, memoised, 32 deep, a cycle unresolved;
  an `end` list with an UNRESOLVED item leaves the interval open, one whose times all precede the begin makes no interval;
  `accumulate` ignored for a `to`-only animation; sandwich order is begin time then document order (`**The subset**`).
- Anything else is a WARNING, never a failure (`SvgDoc::warnings` / `check_svg`): events, `repeat()`, `accessKey`, wallclock,
  `animateMotion`, `animate` on `transform`, animating `href`/`style`/`on*`, and a base value from a `<style>` rule are
  dropped/ignored (`SvgDoc::warnings`).
- Also DROPPED because it would make the guard (which reads the document as stored) wrong afterwards: an animation of `mask` /
  `filter` / `clip-path` / `clip` / `marker*` / `id` / `class`; one whose `values`/`from`/`to`/`by` hold a `url(` UNLESS it
  animates `fill`/`stroke` between gradients; and `radius` / `order` / `numOctaves` (they set a filter primitive's cost)
  (`would make the guard`).
- A `<style>` rule keeps its `url(#id)` (`svg/rules.rs`): counted on every element the rule's subject (last compound selector)
  could match by the name/classes/id it spells (ancestors and pseudo-classes only narrow, so ignoring them over-counts), and on
  EVERY element when the selector cannot be read; a rule reaching inside what it points at makes that mask refer to itself and
  is refused like any loop (`keeps its `url(#id)``).
- A later element repeating an `id` loses it (warning): usvg reaches the LAST via `url(#id)` / href chains and the FIRST via
  `<use>`; the sanitiser leaves the first, the browser's too (`repeating an `id` loses it`).
- A document whose filters, layers or overdraw would take seconds a frame is refused when read (`svg/cost.rs`), measured at the
  scale resvg really draws with (every group's `transform` multiplied down, the root `viewBox` one, animated included, region
  held to resvg's own 5x canvas), at nominal 1080p when read and again by the rasteriser at the real size
  (`would take seconds a frame`).
- `svg::may_animate(src)` is false only when the sanitised text has no animation element (`true` = "maybe"), which lets the
  engine keep such a title as one picture; start and end held to 24 h (`MAX_SVG_SECONDS`); `add_svg_overlay` defaults the end to
  `natural_duration` held to 2-30 s, else 4 s (`may_animate(src)`).
- `SvgOverlay.template` is set for a motion-template title (`core-project.md`); replacing artwork through `SvgOverlayPatch.svg`
  clears it; template values change through `Project::set_template_values_parsed`, NOT the patch; `svg/splice.rs` reads an
  attribute by the name the source spells (`xlink:href` is the XLink one, a bare `href` is not) (`SvgOverlay.template`).
- `SvgOverlayPatch`: `svg`, `preset_params` and `preset_kind`; `svg` excludes both, custom art refuses both
  (`has `svg` (custom artwork)`).
- Presets (`SvgPresetKind`, `svg::render_preset`) are only SVG the subset understands, drawn for a viewBox equal to the delivery
  frame (`Timeline::preset_frame`); the artwork is RE-MADE when the span changes length, params change, the delivery format
  changes (`set_delivery_format`) and in `for_delivery` for a new shape (custom art is only scaled) (`**Presets**`).
- `slice` adds the cut-off front to `offset` (`SvgOverlay::doc_time(t)` = `t - start + offset`) so a range export or playback
  hand-over joins the animation part-way; ripple never moves overlays; `CutSummary.has_text` stays text-only
  (`adds the cut-off`).
- The preset catalogue is `frontend/src/lib/fixtures/svg-presets.json`; after an intended preset change run
  `KERF_BLESS_SVG_PRESETS=1 cargo test -p kerf-core --no-default-features -- svg_preset_fixture` (`KERF_BLESS_SVG_PRESETS=1`).

## Generated clips (`model/generator.rs`)

- `Clip.generator` (absent when `None`, so footage clips write the bytes they always did): only `Generator::Title(TitleClip)`
  today, an animated SVG title on a VIDEO track that takes `transition_in`, fades, keys, colour, grade, effects and is edited
  like any clip (`**Generated clips**`).
- `asset_id` is `GENERATED_ASSET` (the nil UUID, no asset has it: a mistaken library lookup finds nothing, not the wrong file);
  its source is the document's own clock (`source_in` = what a head trim or slice cut off; `Clip::generator_time(local)`); no
  sound; past its animations it holds its last state (`its source is the document's own clock`).
- Invariants (`Rule::ClipGenerated`): video track, no asset, speed 1 (no reverse), not linked, window within
  `0..=MAX_SVG_SECONDS`; `Rule::TitleClipCount`: at most `MAX_TITLE_CLIPS` (50), each title past the cap a violation of its
  own, so any op adding one (a split copies the document) is refused; `set_speed`, reverse and link are refused by the net for
  every op at once (`Rule::ClipGenerated`).
- `title_sources()` = every title's source, floating and on tracks (what the font collection reads); footage reaches
  `MAX_SVG_SECONDS` in `Project::source_limits` (entry keyed `GENERATED_ASSET`); `transition_fx` gives it an unbounded handle
  and `alpha` (`title_sources()`).
- Presets are NOT title clips yet: a preset is laid out for its length and would be re-made by every length-changing op;
  `assemble_by_tag` stores a preset as artwork for its span so a later trim does not retime it; `title-clips.ts` mirrors
  placement/naming/lookup (`title clips yet`).

## Asset tags (`model/tags.rs`)

- `Tag::parse` splits on the first `:` only when what precedes is a key (one word, <= 32 chars: `intro 10:30` is plain; `: x`
  empty key, refused) (`**Asset tags**`).
- Spelling: whitespace collapses to one space, other control chars go, key lowercased, value keeps its case; empty key/value
  refused; `MAX_TAG_KEY` 32, `MAX_TAG_VALUE` 64, `MAX_ASSET_TAGS` 64; compare ignoring case, `dedupe_tags` keeps the first
  spelling; several values of one key are fine (`**Spelling**`).
- `TagEdit {add, remove, remove_keys}` (`deny_unknown_fields`) applies in that order (`remove_keys`, `remove`, `add`),
  refusing the whole edit on one invalid tag; `a.then(&b)` is how an agent's staged edits stack (a property test pins it);
  `apply_capped` (for an edit run later over changed tags) never refuses, leaves adds past `MAX_ASSET_TAGS` off, skips an entry
  that does not normalize (`applies in that order`).
- `tags_match` needs every `TagFilter` term; `group_by_key` is what `assemble_by_tag` uses (groups follow `order`, the rest A to
  Z, an asset with several values joins the first in `order`); `StagedAssetChange.tag_edit` is the held-back form
  (`TagFilter {key?, value?}`).

## Thumbnails (`model/thumbnail.rs`) and utilities

- `Timeline.thumbnails` (absent when empty); nothing that renders the cut reads it (`slice` / `for_render` copies drop it)
  (`**Thumbnails**`).
- `Thumbnail`: size via `parse_thumbnail_size` (16..4096), `graphics` default = no captions, `at: None` = `settled_moment` so a
  title that animates out is drawn settled; `background_scale` renders larger than the delivery (<= 7680 px long side) so a
  crop of another shape is not upscaled (`is `time` (timeline seconds)`).
- The diff has one `ThumbnailsChanged` entry; partial accept lays the proposal's WHOLE list (`subset.rs`, `staged-subset.ts`)
  (`ThumbnailsChanged`).
- `timecode.rs`: SMPTE at the timeline's `f64` rate, drop-frame at 29.97 / 59.94; all held to `MAX_TIMECODE_FRAME` so the TS
  mirror `timecode.ts` does the same integer arithmetic in doubles; the frontend's tests replay its cases
  (`SMPTE timecode at the timeline's`).
- `hostile.rs` exists only under `cfg(test)` or the `test-support` feature, NEVER in a release build; seeded mutation under
  `catch_unwind`; users: `tests/hostile_input.rs`, `engine/cli/hostile_tests.rs`, `source_probe.rs`, kerf-gpu's y4m/`showinfo`,
  kerf-app settings; `frontend/src/lib/hostile-input.test.ts` is the twin; a panic found keeps its input as a regression test
  (`seeded mutation`).

## Known open ends

- Presets are not title clips yet: a preset is laid out for its length and would need re-making by every length-changing op
  (`title clips yet`).
- An eased curve after a head trim is baked to plain linear keys: exact picture, but the Inspector cannot re-edit it as one
  curve (`That bake is lossy for the picker`).
- A half of a split S-bezier whose control point leaves the unit square is re-fitted, not exact (`re-fitted`).
