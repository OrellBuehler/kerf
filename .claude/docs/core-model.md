# kerf-core: `model.rs` — domain types and timeline math

- `model.rs` — the domain types and the only place timeline math lives: `Asset`,
  `StreamInfo`, `Timeline`→`Track`→`Clip` (the EDL), `AssetAnalysis`. A video `StreamInfo`
  carries **`lead`**: seconds its first picture starts after the earliest picture or sound of the
  file (the probe's stream `start_time`s, not the container's, which a text track can pull earlier),
  above 1 ms and below the duration (a start at or past the end is broken and reads 0); omitted at 0,
  so old JSON reads an ordinary start. The export seeks and places the clip's picture by it
  (`ClipFx::lead`, `engine-cli.md`); a proxy-decoded asset keeps the original's only for a `.lead`
  padded proxy (`media::decoded_lead`, `Project::preview_assets_of`). It also carries
  **`color_range`** (`pc` / `tv`, schema v10; `None` when the file says none and for an asset
  probed before it was recorded), the flag a `yuv420p` VP9 / HEVC / AV1 file marks full range
  with (`engine-cli.md`, `core-render-plan.md`). A `Clip`
  references a source range (`source_in`/`source_out`) of an asset at a
  `timeline_start` — non-destructive. Besides the geometry (`Transform`) / color
  (`Color`) / `Transition` fields, a clip carries a `Vec<VideoEffect>` and
  `Vec<AudioEffect>` (per-clip filter chains), a `Grade` (`model/grade.rs`: optional curves, colour wheels and a
  `.cube` LUT reference, normalized by `Grade::normalized`, omitted from the JSON when empty, not keyable; diffed as
  "curves set" / "LUT … at N%"; `grade.ts` is its TS mirror) and a `Vec<Keyframe>` (transform
  **animation** — `Clip::transform_at` interpolates it, the engine renders the
  motion). **Each key carries an `Easing`** for the segment that *leaves* it (`Linear` —
  the default, omitted from the JSON so every existing project and graph is
  byte-identical — `Hold`, `EaseIn` / `EaseOut` / `EaseInOut` (CSS's curves) or a
  `Bezier {x1, y1, x2, y2}` held to the unit square: no overshoot, so a value never leaves
  its two keys' range and the tiny-scale / opacity guards on the keys stay true). **An
  eased segment *is* a polyline** of `EASE_STEPS` (12) straight pieces through the true
  curve (`eased_points`; a hold is an equal-time step), and `Clip::keyframe_channel` is the
  one place a channel's points come from: `transform_at` interpolates it and the export's
  `keyframe_expr` (straight lines only) is written from it, so the still, the export and a
  GPU pass agree exactly rather than approximately — the sweep checks an eased cut at every
  output frame time at five rates, and fails if the export ignores easing. A head trim or a
  slice inside an eased segment — or **exactly on a key** (`rebase_animation` finds the segment
  with `a.time <= by`; a strict `<` read a cut on a key as "before the segment" and dropped the
  key's outgoing easing, so a hold became a ramp in the playback hand-over's `Timeline::slice`,
  a range export, `split_remove` left and a roll / slide head move) — is exact for the same
  reason: `rebase_animation` turns the rest of the curve into plain keys (a hold just keeps
  holding). **That bake is lossy for the picker and exact for the picture**: after a head trim
  the remainder of a curve is up to eleven linear keys, not one eased key, so the Inspector
  reads them as Linear and the curve cannot be re-edited as one; what renders is identical.
  Re-keying a moment (`add_keyframe` at an existing time) keeps its easing, and **a key added
  inside a segment splits it** (`Clip::insert_keyframe`, `Easing::split`): a hold stays held
  through the new key (a Linear one turned the rest of the hold into a ramp) and a curve is cut
  where its x is the key's fraction (de Casteljau, each half normalized to its own unit square,
  so a preset becomes two beziers). That is exact for every preset and any bezier whose control
  points rise (`x1 <= x2`, `y1 <= y2`; the tests hold it to 1e-9 / 1e-4); a half of an S that
  turns back needs a control point outside the square, is clamped into it and so **re-fitted**
  (0.04 off for `(0.2, 0.9, 0.3, 0.1)`), and a half whose value does not change is Linear. The
  new key sits on the sampled pose, as ever. `validate_keyframe` (what `set_keyframes` runs)
  applies the same bezier range check as `set_keyframe_easing` (`validate_easing`), and the diff
  says "easing changed on N keyframes" for a change of nothing else, not "keyframes retimed".
  `frontend/src/lib/easing.ts` is the faithful mirror (both suites pin the same curve and split
  values bit for bit), used by the Inspector's sampled pose, the harness's edits
  (`insertKeyframe`, `easingProblem`) and `edit-modes.ts`'s `rebaseAnimation`.
  **Any one number of a clip can carry keys of its own** (B5b, `model/channels.rs`): `Clip.channels:
  Vec<PropertyTrack {prop, keys: Vec<PropertyKey {time, value, easing}>}>` for a `Property` —
  `scale`, `pos_x`, `pos_y`, `rotation`, `opacity`, the five colour numbers or the clip's `volume`
  (linear gain, 0..=4) — beside the legacy `keyframes` bundle, which animates the five transform numbers
  *together*. **There is one resolver, `Clip::property_keys(prop)`, and everything reads it**: a number
  with a track is driven by it (values held to the static setters' range on read — `Property::clamp` — so
  a hand-edited file never hands `eq` or `volume` a value they refuse, and the export's `1 + 0.3 t` gamma
  is `temperature_gammas`' exactly); a transform number *without* one reads the bundle's keys as stored;
  anything else is its static value. `property_curve` (the eased polyline), `property_at`, `transform_at`,
  `color_at`, `volume_at`, `is_animated` / `color_animated` / `volume_animated` / `zoom_animated` are all
  views of it (and `held_scale`, `pan_extent` and `fit_order`, which decides whether the chain fits the picture to the frame and
  then zooms it, or takes the zoom in the fit's own `scale`: `engine-cli.md`), so the still, the preview, the plan and the export
  cannot disagree. **Nothing is converted
  behind anyone's back, and an old project renders the graph it always did** (the golden digests of the
  first 4800 cases are untouched): the bundle is not migrated on load and not on write — the first
  per-property write *detaches* just that number (`channel_mut` copies its bundle keys into a track, the
  other four keep the bundle), and **a track with no keys is kept while a bundle exists to say "static,
  whatever the bundle says"** (`prune_channels` drops it once the bundle is gone — it runs after every
  op that can empty the bundle or a track: `set_property_keys`, `insert_property_key` and
  `Project::set_keyframes`, which the harness's `setKeyframes` mirrors; a number can never fall back to
  the bundle by accident). The legacy ops keep their meaning: `set_keyframes` replaces **the bundle
  only** (an empty list clears the bundle, not the numbers that have keys of their own — the clip can
  still be animated afterwards, and the MCP description says so; `clear_keyframes` is the one that makes
  the whole transform static), `add_keyframe` writes the bundle (and puts an asked-for number into the
  track that drives it, since the bundle's key for it is ignored), `set_keyframe_easing` without a
  property shapes the transform key at that time in the bundle *and* in every transform track that has
  one there, `clear_keyframes` takes the bundle and the transform tracks and leaves colour and volume.
  **While a number is keyed its static value (`set_volume` / `set_color` / `set_transform`) is not used**,
  which those tools' descriptions now state (an agent that "sets the colour" of a clip whose brightness is
  keyed would otherwise change nothing and not know why).
  `Project::set_property_keyframes(clip, prop, keys)` replaces one number's keys (each checked against
  `Property::check`, at most `MAX_CHANNEL_KEYS` = 1000, bezier range; no keys = static),
  `set_transform_at(clip, time, TransformPatch)` is one revision that poses a clip at a moment (`Clip::pose_transform`:
  a keyed number gets a key at `time` — in its own track if it has one, else in the bundle with the other bundle numbers
  at their sampled values, as `add_keyframe` puts one in — and every other number, a track held empty against the
  bundle included, and the crop edges always, its static value; `channels.ts` `poseTransform` mirrors it, tests by
  name). `set_property_easing(clip, prop, time, easing)` and `copy_keyframes(from, to, props, offset)` (every
  keyed number when `props` is empty; a negative offset cuts the head off with `rebase_head`, so the
  destination opens on the pose; replaces the destination's tracks for those numbers; refuses a number
  the source has no keys for and the same clip twice) are the surface ops, with
  `set_property_tracks(clip, tracks)` — several numbers' keys in one edit ("Edit keyframes"; each track checked as
  `set_property_keyframes` checks it, a number twice or none at all refused), what one dope-sheet gesture writes. **Edits keep channels where
  they were** with the B5a rules: `rebase_animation` (every head move — `Timeline::slice`, split-and-remove
  left, roll / slide of the next clip, a linked partner's trim) runs the bundle *and* `rebase_channels`
  (`rebase_head`: pose pinned at 0, a cut inside an eased segment bakes the rest into plain keys, a hold
  keeps holding, a cut exactly on a key keeps its outgoing segment), `upsert` splits a segment a new key
  lands in (`Easing::split`), `detach_audio` carries a keyed volume over through the fader ratio, and the
  diff names the number that moved (`volume keyframes 0 → 2`, `easing changed on 1 opacity keyframe`) —
  **judged on the keys that drive the number, not on whether it has a track**: a bundle-animated number
  held static by an empty track diffs `opacity keyframes 2 → 0` (it used to diff as empty, and
  `apply_staged` discarded an agent's proposal whose render had changed), and one taken over with the keys
  the bundle already gave it diffs as nothing. **A keyed volume is read held to 0..=`MAX_CHANNEL_VOLUME`
  (4); a static one is not**, so `detach_audio` does not fold the fader ratio into keys when that would
  push one past the cap and silently lower the sound: it takes the equal-fader route a compressor takes
  (a lane whose fader equals the picture track's, else a new track at it) and carries the keys as they
  are.
  **`Timeline::split_clip` now rebases the right half's animation** (`rebase_animation(at - start)`):
  it did not, so every split of a keyed clip replayed the animation from its first key in the right
  half (bundle and reframe alike; `a_split_also_keeps_the_legacy_bundle_playing_through` fails without
  it, as does the TS mirror's); **`cut_range_pieces`** (`cut_clip_range`, `remove_silence`, the linked
  cut) re-times its **tail** piece the same way — by the head and the removed middle — and so does a
  sole-surviving tail, which is a head trim. **A split also puts each fade on the half that holds its
  edge** (the left keeps `fade_in`, the right `fade_out`, each clamped to its half as for any clip that
  shrank; `transition_in` stays on the left): both halves used to keep both, so a clip with a fade-out
  dipped to black at every split. The links corpus did not move (no case splits a faded clip), and the
  unit tests are mirrored by name in `links.test.ts`. **The mask's numbers and the crop are animatable too**
  (B5b-2): `mask_x`, `mask_y`, `mask_width`, `mask_height`, `mask_feather` (held to `Mask::normalized`'s ranges,
  `MIN_MASK_SIZE` / `MAX_MASK_SIZE`; the static value is the clip's mask's, `Clip::mask_at`) and **the crop as a
  zoom and a pan, not four edges** — `crop_zoom` (1..=`MAX_CROP_ZOOM` = 10: 1 is the whole picture, 2 half of each
  side), `crop_x` / `crop_y` (-1..=1: the window against the left / top edge, centred, against the right / bottom).
  An edge that moved per frame would need a picture whose size changes per frame, which almost nothing downstream
  can follow (`crop`'s `w` / `h` are evaluated once); the keyed crop is a window of the *statically cropped*
  picture with its own aspect, `1 / zoom` of each side, placed by the pan in the room left over
  (`Clip::crop_window_at`), so it never shows anything outside the picture and its size never changes. It has no
  static value (neutral 1, 0, 0) and sits on top of the four static edges. **`crop_animated` is "some zoom key
  zooms in"**: a zoom held at 1 shows the whole picture whatever the pans say, so such keys draw nothing, cost
  nothing and are not refused by the GPU plan (as a keyed rotation at 0 or opacity at 1).
  `mask_animated` is "a mask number is keyed **and** the clip has a mask": keying one on a clip without a mask is
  refused (`check_keyable` in `set_property_keyframes` / `set_property_tracks` / `copy_keyframes`; no keys is
  always fine; `copy_keyframes` with no `props` leaves a mask's numbers behind for a destination with no mask
  instead of refusing the copy), and `set_mask(None)` drops the mask's keys. `frontend/src/lib/channels.ts`
  is the faithful TS mirror (resolver, `upsertKey`, `rebaseHead`, `propertyKeysShifted`, ranges, `maskAt`,
  `cropWindowAt`, `cropAnimated`, `keyableProblem`; its key copies own their bezier easing (`ownEasing`), since a
  curve read off the editor's reactive timeline is a proxy the harness's `structuredClone` refuses; both
  suites pin the same samples, head-cut keys and split beziers bit for bit), used by the harness, the
  Inspector's sampled pose and the preview's gain (`gainAutomation`).
  **`TransitionKind` is three families, and the family decides the render**: a
  **dip** (`DipToBlack` / `DipToWhite`) takes both sides through a solid colour
  either side of the cut, a **dissolve** (`Crossfade`) mixes them, and a
  **motion** transition travels the incoming clip in over the outgoing one
  (`Slide*`) or carries the outgoing one out with it (`Push*`), four directions
  each — the direction naming the direction of *travel*. The enum answers for
  its own family (`dip_color` / `slide_from` / `pushes` / `overlaps`), so the
  engine never matches on eleven variants, and `wire_names` derives the
  expected-kind list both surfaces put in their errors. A dissolve or a motion
  transition plays both shots at once, so it borrows the outgoing clip's unused
  source handle: a clip trimmed to the very end of its footage has none to lend
  and the transition degrades to a hard cut (a dip needs none). Text titles /
  lower-thirds / captions live on the timeline itself as
  `Timeline.overlays: Vec<TextOverlay>` (each with its own `TextKeyframe`
  animation); `transcript_to_srt` serializes a transcript to SubRip.
  **Captions are timeline math, not a transcript dump**, and pure +
  unit-tested: a transcript is in *source* time and an overlay is in *timeline*
  time, so `Timeline::captions` projects each segment through the clips that
  actually show its footage (`Clip::source_span_to_timeline`, honoring trim /
  speed / reverse) — captions land on the words that survived the cut and words
  that were cut out get none. It reads through `for_render`, so a muted track is
  as uncaptioned as it is unheard; it chunks a sentence to `CaptionOptions`
  (a speech model emits whole sentences and a whole sentence does not fit a
  9:16 frame). A segment **with `words`** is chunked over its words (`word_chunks`, the same
  `max_words` / `max_chars` rule): a line runs from its first word's start to the next line's, or
  across a pause to its last word's end + `CAPTION_HOLD` (0.5 s; the last line no further than the
  segment end), and is kept only if the span its words are *spoken* in meets the clip (a hold
  reaching past an in-point captions nothing). One **without** words (an imported cue, an older
  transcript) is timed by *character share* as before, so those caption byte-identically; lines too short to read
  merge back into a neighbour instead of flashing; and no two lines are ever on
  screen at once (captions are one lane of text, and the same footage reaching
  the cut twice would otherwise collide with itself). `TextOverlay.generated`
  marks what it wrote, so regenerating replaces its own set and leaves a typed
  title alone.
  **The cut's transcript** (`model/cut_transcript.rs`, pure + unit-tested): `Timeline::cut_transcript`
  maps the same way through the same walk (`segments_in_cut`, shared with `project_through_clips`, so
  captions did not move) into `CutLine`s — the words of one segment one clip keeps, in timeline seconds,
  with `clip_id` / `track_id` / `asset_id`, the source span and each `CutWord`'s timeline and source
  bounds (`words_of`, so an older line is cut by estimated words). It reads `for_render` (a muted or
  solo-shadowed track and a disabled clip say nothing) and only clips with `source_audio`, as the
  export's `clip_sounds` decides: a detached picture says nothing, its audio clip does. **A word belongs to
  the clip that plays its midpoint** (half-open source window), clipped to it — a word split by a cut
  is read once, a trimmed sliver not at all; the line's text is the kept words (its own text when all
  are kept). Lines sort by start (stable, so a tie keeps track order); one asset's line at the same
  source and timeline start twice (a picture still sounding beside its sound) is kept once, first in
  track order. A reversed clip's words keep their order with times running down.
  `Project::cut_transcript_inputs` / `CutTranscriptInputs::read` split it around the lock and name the
  `untranscribed` audible assets (an audio stream, no done transcript).
  **`CaptionStyle` is the look**, and one decision rather than four:
  `Lines` (4 words / 28 chars, 5% of frame height, low in the frame) is the
  subtitle shape a line is *read* in; `WordPunch` (one word, 11%, higher, bold)
  is the social shape a word is *watched* in, each landing on the beat of the
  speech. Word count, size, position and the flicker floors move together
  because they have to — held to `MIN_CAPTION` every short word would merge
  into a neighbour and word punch would collapse back into lines, so it gets
  its own `MIN_WORD_CAPTION` / `MIN_WORD_VISIBLE` and words merge far later.
  `CaptionOptions` is that style plus **overrides**: every number is optional
  and follows the style when omitted, `resolve()`ing to the `CaptionLayout`
  captioning works from — so asking for `word_punch` alone gets the whole look
  rather than one word left at subtitle size, and `CaptionOptions::default()`
  is unchanged, so every pre-existing call captions identically. `fit_size`
  then shrinks a caption to fit the frame: `drawtext` neither wraps nor scales
  and a 9:16 frame is barely half as wide as it is tall, so a long word — or a
  28-char subtitle line, already true before word punch — was drawn off both
  edges. `fontsize` cannot be an expression over `text_w` (the width is what
  depends on the size), so it is estimated from the character count against
  `Timeline.format`'s aspect; an unframed project assumes 16:9, wide enough
  that the fit never binds, so nothing that never picked a frame moved. A `Track`
  carries a `duck` flag (sidechain-ducked under the rest of the mix on export).
  `Fit` and `Delivery` live here (the domain owns the delivery shape; `engine::cli`
  re-exports `Fit`: `Contain`, `Cover`, and `Blur` — Contain over a blurred copy of the picture,
  `engine-cli.md`), and `Timeline.format` is the frame the project is cut for. A `Delivery` also
  carries an optional **frame rate** (`fps`, schema v12): `Delivery::with_fps` / `check_delivery_fps` hold it to
  `MIN_DELIVERY_FPS..=MAX_DELIVERY_FPS` (1..240, an `InvalidArgument` outside) and make an NTSC rate within 0.005
  (`23.976`, `29.97`, `59.94`, also 47.952 / 119.88 / 239.76) the exact `n * 1000 / 1001`; `Delivery::rate()` is the
  value the engine reads (a stored one out of range is ignored); `Delivery::new` has none, an absent one is not
  written (`skip_serializing_if`), so a delivery that never had one stores byte-for-byte what it did (`Eq` went with
  the `f64`); `for_delivery` keeps the project's rate for a variant that names none; the diff reads
  `1080x1920 (cover, 29.97 fps)` (`fmt_delivery`).
  **Smart crop** is here too and pure + unit-tested: `SalienceMap::crop_for` slides a
  window of the delivery aspect across the sampled map and returns the `CropFrame`
  (per-edge fractions, plus how far off centre it landed) that keeps the content —
  with a `CENTER_BIAS` so a flat map resolves to the plain centre crop rather than to
  whichever edge won by rounding, and `needs_crop` short-circuiting footage that is
  already the delivery shape.
  **Caption import** (`captions_import.rs` parsers, `Timeline::place_cues`,
  `Project::import_captions`) puts a subtitle file on the cut through the
  *transcript* code, not beside it: `Timeline::captions` is now
  `project_through_clips` + `settle_caption_lines` and `place_cues` calls the same two,
  so chunking, the flicker floors, the one-lane rule and `fit_size` cannot drift.
  `CaptionTimeBase::Source(asset)` *is* `captions` over a one-asset map (trim / speed /
  reverse, `for_render`); `Timeline` (the default) takes the times as they stand,
  clipped to `for_render().duration()` — a film-length SRT outruns a short cut — and,
  unlike source time, a muted track does not silence it (the file captions the finished
  cut, not one clip's sound). An *empty* timeline has no end to run past, so its window
  is `EMPTY_CUT_WINDOW` (a day) rather than whatever a hand-edited file says. A
  `CaptionImportRequest` also carries an **`offset`** (seconds, either sign, ±100 h)
  added to every cue before placement — a broadcast SRT that starts at `01:00:00`
  wants `-3600`. The
  parsers are pure and tolerant — SubRip (BOM, CRLF / CR, missing or absurd indices, a
  `,` or `.` fraction read as a *decimal* fraction, `<i>` / `<font>` and `{\an8}`
  stripped, no blank line between cues with the index handed back to its cue), **WebVTT**
  (`parse_vtt`: blank-line blocks; the `WEBVTT` header, `NOTE` / `STYLE` / `REGION` skipped
  uncounted, an optional cue id, `.` milliseconds and optional hours, cue settings ignored,
  `<v Speaker>` / `<c.x>` / `<b>` and inline `<00:01.000>` timestamps removed, `&amp;`-style
  references decoded *after* markup so `&lt;i&gt;` stays text; captions have no speaker
  field, so a voice's name is dropped; `detect` says `vtt` for a `WEBVTT` first line) and ASS /
  SSA (the `[Events]` `Format:` line picks the columns, `Dialogue:` only, `{…}` blocks
  and `\p` drawings dropped, `\N` / `\h`) — and what they cannot read is **counted,
  never fatal** (`skipped_lines`: empty or zero / negative-length cues, a stray line, a
  bad `Dialogue:`); styles and positions in the file are not imported, since where a
  caption sits is the `CaptionStyle`'s call. Control characters (NUL, ESC…) are
  stripped from a cue — a NUL in an ffmpeg argv fails *every* spawn — and
  `escape_drawtext` drops them too, which closes the same hole for a typed title (only
  controls other than tab / CR; every real title is byte-identical).
  **The caps are guards, not tidiness, and each one is an incident**: 5 MiB a file,
  10,000 cues (parsing stops once exceeded), 100,000 words, 2,000 chars a cue,
  **16,384 chars a line refused *before* it is cleaned**, 20,000 captions written. The
  cleanup scans are bounded so they are linear — `strip_markup` looks for a tag's `>`
  only within 256 bytes and only up to the next `<`, `strip_braces` copies the rest
  when no `}` remains (an unbounded `find` per `<` / `{` was quadratic: 400 KB of `<`
  took 6 s, a full file ~17 min) — and `time_chunks` keeps its weights as it merges
  and stops a scan at the first short line instead of rebuilding and cloning every
  chunk per merge (the output is bit-identical; a test sweeps it against the old
  implementation). The merge is still **quadratic within a cue** (4x the words
  costs ~16x), so the per-cue and per-import caps are what bound it: the worst
  import they allow (100,000 one-letter words in 2,000-char cues) places in about
  half a second unoptimized. Performance tests assert generous wall-clock limits
  (seconds) sized to catch the algorithm or a lost cap, never a busy machine.
  Encodings: UTF-8 / BOM'd UTF-16 / Latin-1 read as Windows-1252.
  **Parsing is outside the project lock.** `parse_captions` (after `read_caption_file`)
  is a pure static step that yields a `CaptionFile`; `Project::import_captions(&file,
  req)` only *places* it, inside the `edit_timeline` closure — so the Tauri commands
  and the MCP tool read, decode and parse on the blocking pool with the lock released,
  as `analyze_asset` and `smart_crop` do.
  Lines carry an `origin`, so every cue is accounted for exactly once
  (`cues == placed + dropped_outside + dropped_short + dropped_overlap`;
  `captions >= placed`, a long cue being several lines): *outside* never met the cut
  (past its end or before its start, footage no clip shows), *short* met it for a moment
  below the readable floor (its own length, or the sliver left at an edge), *overlap*
  lost its slot. Simultaneous cues keep **file order** on the import path
  (`SimultaneousLines::ByOrigin`) — a transcript, which has none, still sorts by text.
  **Imported overlays are `generated`, on purpose**: captions
  are one lane of text, so the imported set *is* the caption set — importing replaces
  the earlier generated / imported captions (`replaced`), Clear / Recaption / the
  `for_delivery` re-fit treat it like any other, and a later `generate_captions`
  replaces an imported set (two sets would put two lines on screen at once; typed
  titles are never touched). A cue's own line breaks are re-flowed by the style — a
  larger `max_words` / `max_chars` keeps cues whole. It is one `Import captions`
  revision computed inside the `edit_timeline` closure (an agent's lands in its
  proposal), and a refused import (no cues, nothing reaching the cut, an asset not on
  the timeline, more than 20,000 captions) writes nothing. Core takes *text*;
  `read_caption_file` (`.srt` / `.vtt` / `.ass` / `.ssa`, regular file, size) runs before the lock
  in the Tauri command and the MCP tool.
  **Caption export** (`captions_export.rs`, pure) is the way back out: `cut_captions` is the
  timeline's *generated* overlays (a typed title is not a caption) — already in timeline
  time, projected through trims / reorders / speed when they were written, so a cue is
  exactly the line the burned-in overlay draws — clipped to `[0, duration]`, control
  characters dropped; `write_srt` / `write_vtt` round to whole ms (end ≥ start + 1 ms),
  turn `-->` in SRT text into `->` and escape `& < >` in VTT, and read back through the
  importers unchanged (a test).
  **Markers** carry `duration` (0 = a point), `comment` and `kind` (`Comment` / `Chapter`),
  all `#[serde(default)]` and omitted at their defaults, so a stored timeline is
  byte-identical. `MarkerPatch` + `Marker::apply` is the one validated edit (time and
  duration ≥ 0, comment ≤ `MAX_MARKER_COMMENT`, control characters but `\n` / `\t` dropped);
  `slice` clips a marker's span to the window and the diff names length / kind / comment
  changes. A `Chapter` marker becomes a chapter of the export (see `engine-cli.md`).
  Inherent helpers (`Timeline::locate`, `Track::end`/`reflow`, `Clip::duration`,
  `Timeline::slice` — the shifted sub-timeline copy behind range export) back the
  operations. **Beat alignment** lives here too and is pure + unit-tested:
  `Timeline::bar_grid` is the same over `Tempo.downbeats` (bar starts; the rhythm pass
  replaces an asset's autocorrelated `Tempo` with `Tempo::from_grid` when a bar grid is
  fitted, so the ruler, snapping and cut-to-the-beat all use the fitted grid).
  `Timeline::beat_grid` maps the audio tracks' cached `Tempo` onto timeline time
  (confidence-gated by `BEAT_MIN_CONFIDENCE`, mirroring the ruler's ticks) and
  `Track::align_cuts_to_beats` ripples a track's cuts onto that grid — each clip
  retrimmed at its **outgoing** edge (`source_in` for a reversed clip, whose tail
  is the source's head), gaps preserved and their incoming cuts snapped too,
  stretching only as far as the asset has footage (a still loops, so it is
  unbounded). **What changed between two cuts** is here too and pure +
  unit-tested: `Timeline::diff` returns a `TimelineDiff` — a `DiffEntry` per
  change (`DiffKind` distinguishes an add from a cut from a *move* from a
  *retrim*, because those are different things to review), each already phrased
  for a human (`Trimmed clip on V1 at 0:04.0 — 4.0s → 2.5s (-1.5s)`) and
  carrying the clip/track/time (and, for an `Overlay*` entry, the `overlay_id`)
  so a UI can jump there or mark it on the timeline. Everything is matched by
  **id**, so a reordered track reads as the handful of moves it is rather than as
  every clip having been replaced, and a removed track is one entry instead of one
  per orphaned clip. **Every entry has a stable `id`** (`<kind>:<clip|text|marker|track
  id>`, e.g. `clip_retrimmed:<uuid>`; unique per diff, unchanged as a proposal grows; the
  `Marker*` kinds carry a `marker_id`) so a reviewer can send back the ones it accepts.
  `StagedEdit` is a pending proposal (base seq, the edit
  labels, `stale`, its diff, `owned_by_agent` — the session id itself (`owner`) is Rust-side only,
  `#[serde(skip)]`, so it never reaches the webview or another agent — how many steps `undo_staged` can take
  back, and the asset changes held back with it — `StagedAssetChange`, id
  `asset_projection:<asset id>` (`projection`) or `asset_tags:<asset id>` (`tag_edit`: the agent's edits to the asset's tags
  composed into one `TagEdit`, run over the tags as they are when it is read; absent on a projection change, so a stored one reads
  as before), usable wherever an entry id is).
  **Partial accept** is `model/subset.rs` (pure + unit-tested): `Timeline::merge_subset(base,
  proposed, picked_ids)` writes each picked change onto the **live** cut by identity (this
  clip / text or animated title / marker / track — never by position), so what the user did since stands. A
  clip's start, its footage window and everything else are separate picks (`ClipMoved` /
  `ClipRetrimmed` / `ClipChanged` combine per clip). What cannot be taken apart is not
  guessed: a **linked partner's** entries, the **new track** a picked clip lands on and the
  moves that take clips off a track the proposal removes come with the pick
  (`SubsetMerge::auto_included`, each with its reason); a pick that leaves two clips on top
  of each other (`check_subset_lanes`, against what already overlapped in the live cut) or
  pulls linked clips out of step (`first_sync_break`) is refused naming the track and time;
  a pick whose target the user removed since is `skipped`, not fatal. What is left of a proposal after a
  partial accept is `Timeline::rebase_rest(base, proposed, rest_ids)`: the same by-identity merge of the
  **unapplied** changes onto the cut as it now is, *without* the closure and the two refusals (it is
  the cut to show, not a change to write; a change that no longer fits is dropped and counted) — so a
  forced accept leaves the user's newer work in the remainder instead of a diff that undoes it.
  `Timeline::check_own_lanes(before, ids)`
  is the same overlap rule for the clip an edit itself placed (`add_clip_to_timeline`,
  `trim`, a voiceover): judged **after** the ripple, which may have made the room, and not
  for an overlap the clip already had. **Ripple** is here too, pure + unit-tested:
  `Timeline::ripple_from(before)` takes what an edit left behind and the cut it
  started from and, per track, matched by **id** like `diff`, shifts the clips the
  edit left *starting where they started* by the net change in length of what it
  did ahead of them — a clip's length change (right trim, speed) or removal, or an
  add that landed **on footage that was there** (an add that fits in free space,
  and every append, moves nothing). It carries the rules that were bugs waiting:
  a **left-edge trim keeps the clip's start** (the GUI commits it as `source_in`
  *plus* a later `timeline_start` to hold the right edge; ripple keeps the start
  and follows the length, so both forms give one result — only when the trim is
  the whole edit on the track); a **split shifts nothing** (the new half is an add
  over the footage the other half gave up, and they cancel); **moves never ripple**
  (a clip that merely changed its start or track is not "footage ahead");
  **clips the edit itself moved are not followers**, so an op that already closes
  the gap is not shifted twice; tracks are **independent — except for linked
  clips**: a clip the ripple moved takes its *linked partners* along by the same
  amount (the sync lock, `conform_links`; only clips follow, never the rest of the
  partner's lane — see Linked A/V below),
  a **locked track never moves**, and overlays / markers do not move. It **never
  produces an overlap**: if shifting would leave a touched clip overlapping
  another or before 0 (an add that lands *inside* a clip would need a split), that
  track is returned as the edit made it. `Timeline::move_clips` /
  `remove_clips` are the pure, all-or-nothing multi-clip edits behind the
  marquee: a `ClipMove` is a clip, an **absolute** start and an optional
  same-kind track; the group is checked as a group (moving clips pass through the
  places they are leaving, never onto each other or a clip that stays), and a
  locked track, a start before 0 or a clip named twice refuses the lot.
  **Edit modes** are here too, pure + unit-tested, and each *clamps and reports*
  rather than refusing (`EditOutcome {requested, applied, clamped, clips}`; it
  errors only when the clamp leaves nothing to move, so a no-op records no
  revision) — to the footage (`SourceLimits`: `Asset::source_limit`, infinite for a
  still) and to a 0.05 s floor (`MIN_EDIT_CLIP`); each has a `*_range` returning
  the `DeltaRange` it clamps to, which is what a drag reads. **Roll**
  (`roll_edit(a, b, delta)`): `a`'s end and `b`'s start move together, so the
  pair's span and everything after it are unchanged; the clips must touch within
  `ADJACENT_EPS` (1 ms — the engine's own transition-partner test) with `a`
  first. **Slip** (`slip_clip`): the source window shifts, position and length do
  not; `delta` is **source** seconds, and *positive means the clip starts later in
  its own footage*, so a reversed clip's window moves the mirrored way and the
  sign always means the same on screen; a still is an error. **Slide**
  (`slide_clip`): the clip moves, the neighbours that *touch* it give way (the
  previous one's end and the next one's start move by `delta`); a neighbour across
  a gap is never trimmed — the clip stops where it would meet it — and the last
  clip, with no next to give way, extends the track. **Split and remove**
  (`split_remove(clip, at, Left|Right)`): the surviving half keeps the clip's id,
  so `ripple_from` reads it as the ordinary trim it is (a left removal holds the
  clip's start and closes the track under ripple, leaves the gap otherwise); it
  drops what belonged to the removed half (`fade_in` + `transition_in` on the
  left, `fade_out` on the right). Shared rules: a *head* move re-times the clip's
  keyframes and reframe keyframes with its content (`Clip::rebase_animation` — the
  same pose-pinning `Timeline::slice` does, which now calls it), a tail move or a
  slip/slide of the clip itself leaves them clip-local; fades are clamped into a
  clip that shrank; a still's window is only ever written on its out-point (no
  negative `source_in`); locked tracks refuse; every op validates before it
  mutates. **Float residue is welded**: a roll or slide computes one side of a cut
  from a window and the other from `start + delta`, which disagree by a few ulps
  (±6e-14 s, in a large share of cases) — invisible to a render, but read as an
  overlap by a strict test like `Project::move_clip`'s. So the cut the edit made is
  closed *exactly* (`weld`: the follower starts at `leader.timeline_end()`, the
  expression the overlap checks use) and the far edge, where the edit runs into a
  clip it did not move, is pulled back off that clip by shortening its window point
  by the overshoot (`fit_end`) — both only for float noise (`DIFF_EPS`), never a real
  sub-millisecond gap, which is data. A fuzz test asserts no junction of the lane
  overlaps afterwards. **`split_remove_clips(&[ClipCut{clip_id, at}], side)`** is
  `split_remove` on a selection as one edit — all or nothing, **at most one clip per
  track** (a lane trimmed at two places has no single edit point for `ripple_from` to
  hold still), each track rippling on its own. A same-length window shift diffs as
  `Slipped clip … footage +0.03s (in-point 10.00s → 10.03s)` — two decimals (three
  if two would show a real shift as zero), signed as `slip_clip` is (`+` = later in
  its footage, so a reversed clip's window moving down prints `+`), not a `+0.0s` trim.
- **Invariants** (`model/check.rs`, pure + unit-tested): `Timeline::check()` refuses a timeline no
  edit should leave — a non-finite time, gain or speed; a clip whose `source_out <= source_in`; a
  negative fade or transition; a text / SVG overlay that does not end after it starts; a non-finite
  keyframe or marker time; a track, clip or overlay id used twice. `Project::run_edit` takes
  `known_violations()` before the edit and calls `check_new(&known)` after it, on every edit (live and
  staged) before the transaction commits: only a violation (rule + item id) the edit **introduced** is
  refused, so a file an older build left broken can still be edited, and a failing edit persists
  nothing. The message names the item ("the clip at 0:04.0 on V1 has a negative fade", "the text
  “hi” at 0:01.0 must end after it starts").
  **Overlaps are not checked**: Kerf allows them (an explicit `add_clip`, a trim into the next clip, old
  files), and the ripple already treats an overlap the edit did not make as old news. Nothing is indexed
  by position in the stored timeline (everything is by id), so there is no index range to check.
- **Edit values** (`model/values.rs`, pure + a table test pinning ranges and messages): the ranges a
  setter holds what it is handed to, before it changes anything — `Transform::check_values` /
  `check_crop` (`set_transform`), `Color::check_values` (`set_color`), `Clip::check_fades` (`set_fade`),
  `Clip::check_trim` + `Asset::check_source_out` (`trim`; the latter `add_clip_to_timeline` too), `TextOverlay::check_values` /
  `check_span` (`update_overlay`), `TextKeyframe::check` (`set_overlay_keyframes`). They are shared with
  the GPU preview's drag frames (`PreviewDraft::apply`, `app.md`), so a preview never shows a value the
  edit would refuse: change a range here, not in a setter.
- **Fit music to length** (`model/music_fit.rs`, pure + unit-tested): `plan_music_fit(music,
  target, rate)` keeps the intro (before the first downbeat) and the ending (after the last
  whole bar) and walks the bars between, **jumping only between repeating phrases**
  (`PhraseMatch`, 8 bars cost 2, 4 bars cost 3, never two jumps in a row): a DP over (bar,
  bars played, just jumped) picks the bar count nearest the target — a **tie goes to the
  longer** walk (a fade can shorten it; the shorter leaves picture without music) — then the
  fewest jumps. The search runs to `max(wanted, bars) + bars` (a jump moves at most a song's
  length; capping it at `wanted + 1` missed the arrangement just above). Consecutive bars
  merge into one segment, so a continuous join is never a splice. Boundaries land on the
  source's sample grid and each `output_start` is the exact running sum rounded once (no
  drift). `music_fit_clips` turns a plan into clips copied from the music clip (gain and
  audio effects kept, keyframes and link dropped) with each splice crossfaded over **one
  10 ms window centred on it**: the outgoing clip's `source_out` and the incoming clip's
  `source_in` / `timeline_start` move half a window early, and the incoming `Crossfade` of
  a full window makes the export's tail and fade-in cover the same samples (offset windows
  played both copies at full gain — +65 % peak in the Python prototype). An overrun with
  `fade_out` is cut at the target and faded over `FIT_FADE_S`.
- **Music scores** (`model/score.rs` + `score/{chord,arrange,align}.rs`, pure + unit-tested): a `MusicScore`
  (`bpm` 40–240, `key` `G` / `F#m` / `Bb` / `Ebm`, a `MusicStyle` — `tropical_house`, `pop`, `lofi`,
  `cinematic`, `ambient`, `rock` —, `sections: [{kind: intro|verse|build|drop|breakdown|outro, bars, chords,
  energy?}]`, `hits: [{at_s, kind: impact|riser|dropout|whoosh}]`, `seed` (0 to `MAX_SEED` = 2^53 − 1, the largest integer a JS number keeps through JSON; a bigger one is refused), `instruments?` GM-program overrides)
  is 4/4 only, rides on `Asset.music_score` and renders through `engine::compose` (`engine-media.md`). **Chords**
  are names (`C`, `Em`, `F#m7`, `Gsus4`, `D/F#`, `Bbmaj7`, `E5`, `Cadd9`, `Am7b5`) or **roman numerals in the key's own
  scale** (in A minor `VI` is F, `VII` is G, `i` is Am; case is quality, a leading `b` / `#` moves the root), one per
  bar, a shorter list cycling; `resolve()` checks everything and the error names `sections[i] (kind)` and the
  chord (`InvalidArgument`) — and its `chords` are **as played**: the last bar of an outro that ends the piece is
  already the tonic chord, so the notes, the bar before's approach note and the analysis chroma agree; the score
  types refuse an unknown field (`deny_unknown_fields`); limits keep a render in memory (100 sections, 128 bars each, 400 bars, 600 s, 256
  hits). `duration_s` = bars + a **release tail** (`RELEASE_TAIL_S` 1.5 s, at most 0.9 of a bar so the tail is never
  a whole extra bar); `beat_grid()` is exact (beat 0 at 0 s, bar every 4), `music_analysis(duration)` writes the
  asset's **bar structure from the chords** (per-bar chroma from the chord tones, then the same `phrase_matches` the
  audio analysis uses, so `fit_music` finds the repeating phrases) and `rhythm(duration)` the whole rhythm result
  (full-confidence `Tempo::from_grid`, kicks, hits and downbeats as onsets, class music) — `analysis::ScoreRhythm`
  serves it when a composed asset is analysed, so a re-analysis never replaces it with a detector's guess.
  **`expand(score) -> Arrangement`** is deterministic: per role (`Drums`, `Top` = hats / shakers / cymbals, `Bass`,
  `Pad`, `Keys`, `Lead`, `Fx`) `NoteEvent {channel, program, key, velocity, start_s, dur_s}`, the kick times the
  sidechain follows, the dropout windows, the per-section make-up gain and the preset's mix. Each style is a
  `Preset` of data (GM programs, 16-step drum lanes written as digit strings — `7` is velocity 100 — bass /
  comping / hook patterns, swing, humanization, role gains and shares, sidechain, impact strength). **Section
  logic** (`drum_tier`, `bass_mode`, `keys_mode`, `lead_mode`, from the kind and the energy: < 0.4 light, < 0.75
  mid, else full): an intro is a pad, a sparse chord and a hook teaser late; a verse the groove; a **build** pulses
  the bass in eighths, rolls the snare a hit a beat → two → four → eight with the velocity climbing, brings the kick
  in a fifth of the way and drops it from the last beat, cuts the harmony a sixteenth-and-a-half short of a drop, and
  ends in a riser into whatever follows; a **drop** is every layer plus a crash and an impact (not when one drop
  follows another); a **breakdown** has no kick and no bass; an **outro** thins and its **last bar resolves on the
  tonic chord** held into the tail. A fill closes a section (and every eighth bar of a long one). **Harmony is
  voice-led** (`chord::voice`: every chord tone once inside a range and a spread, the voicing nearest the last by
  total nearest-tone movement; a ninth chord drops its fifth); the bass plays the root (or a slash chord's bass) in
  G1–F#2 and approaches the next root by **a step of the key's scale from the side it comes from**; the hook is
  a motif over the **chord-tone ladder** (index 0 = the chord's root in the lead's range, so one motif fits every
  chord), `a a' b b'` over four bars with the last bar's final note resolved to the root, the same two motifs for
  every section of a kind. Style colour extends plain triads (`lofi` to sevenths, `ambient` to add9). All
  variation comes from a small seeded generator (xorshift64\*, pinned by a test) keyed by seed, role, **section and
  bar** — editing a section never re-rolls the ones before it. **Score to picture**
  (`plan_score_to_picture(score, cut_times_s, music_start_s, &AlignOptions) -> (MusicScore, AlignReport)`): the cuts
  are moved into music time; the tempo is searched in 0.05 % steps within ±`max_tempo_change` (default 6 %) for the
  most cuts within 40 ms of a beat, a downbeat counting double, and **changes only when that beats the score's own
  tempo**, taking from the nearest run of equally good tempos the one that centres the cuts (so a nudge lands the
  cuts within a few ms, not at the edge of the tolerance); then an `impact` on every cut on a downbeat (on the
  grid, not on the cut) and, before a `drop` a cut lands within one beat of, a `dropout` (one beat) and a `riser`
  (not after a build, which has its own). Hits already there are not added twice. The score's **own hits stay at their
  absolute seconds**, so a tempo whose piece would end before the last of them is never a candidate (a fit to 127 BPM
  that would cut the piece short of an impact at 80 s settles on the slower tempo that keeps it); the hits added stop at
  `MAX_HITS` (a note in the report says how many were left out) and the score returned is `validate()`d. The report lists every cut's
  `before` / `after` reading (on beat, on downbeat, offset) and the counts.
- **Limits that keep a project loadable.** A clip's speed is `MIN_SPEED..=MAX_SPEED`
  (0.01×..100×, either sign; so is every key of a speed ramp) — `set_speed` and the ratio `set_speed_linked` hands each partner
  both hold it (the audio tempo is one `atempo` step per doubling). `Mask::normalized` maps a
  non-finite size to a default instead of clamping `NaN` through. `Project`'s writes also run a
  serde walk (`finite.rs`, `first_non_finite`) over every timeline before it is stored: serde_json
  writes `NaN` as `null`, which a plain number field cannot read back, so one poisoned number would
  stop the project opening. The setters refuse non-finite input themselves with a message; the walk
  is the net under every op that computes a number.
- **Blend modes** (`BlendMode` in `model.rs`). `Clip.blend_mode` (absent at `normal`, the default; schema v17) is how the
  clip composites onto the tracks below: `normal`, `multiply`, `screen`, `overlay`, `hard_light`, `add` (linear dodge),
  `darken`, `lighten`, `difference`, `exclusion` — the separable modes FFmpeg's `blend` computes as the textbook defines them
  (its soft light, dodge, burn and subtract are other formulas and are not offered; an unknown name does not read).
  `BlendMode::ffmpeg` is `blend`'s name with the clip as the top input, which swaps `overlay` and `hard_light` (FFmpeg's
  `overlay` conditions on the top input); `apply` is the formula per channel, which the render tests hold FFmpeg to;
  `parse` takes common spellings (`hard-light`, `linear dodge`). Split, duplicate, paste and a link carry it as they carry
  every field (a detached sound is a fresh clip at `normal`); the diff says `blend normal → screen`; OTIO and EDL have no
  blend mode, the report says `clip blend modes …` and OTIO keeps it in `metadata.kerf`.
- **Speed ramps** (`model/speed_ramp.rs`: the type and the time map; `model/ramp_edit.rs`: the edits and the diff text; pure +
  unit-tested in `ramp_tests.rs` / `ramp_link_tests.rs`). `Clip.speed_ramp: Vec<SpeedKey>` (absent when empty; schema v14) makes a
  clip's rate change over its length. A `SpeedKey` is a clip-local **timeline** time, a speed magnitude (`MIN_SPEED..=MAX_SPEED`)
  and the `RampEasing` of the segment leaving it (`linear`, the default and omitted, or `hold`). Before the first key the clip
  runs at the first key's speed, after the last at the last's, between two keys the speed is a straight line in timeline time
  (a trapezoid of source distance, a quadratic-root inverse) or held. The direction is still `speed`'s sign; its magnitude is
  **not read** while a ramp is set. A ramp the map cannot read (`ramp_is_valid`: 1..=`MAX_SPEED_KEYS` (16) keys, strictly
  increasing times within ±`MAX_RAMP_TIME` (1e6 s: the squared segment lengths stay finite, so the map never returns a NaN),
  speeds in range) is ignored — the clip plays at its constant speed — and flagged by the invariants (`Rule::ClipSpeedRamp`), so
  an old or hand-edited file still opens and no edit introduces one. **One map**: `Clip::along(local)` /
  `local_at_along(along)` (plus `speed_at`, `mean_speed`, `is_ramped`); `duration`, `source_to_timeline`, `timeline_to_source`
  read it, and anything that converts between the two clocks must too, never `speed_mag()` — an unramped clip keeps its old
  `* speed` / `/ speed` arithmetic to the last bit, which is why no graph, golden or corpus case moved.
  **Edits keep every frame on its footage.** A *head* move rebases the ramp (`Clip::rebase_ramp`, called from
  `rebase_animation`, so from `move_head`, split's right half, `slice`, `cut_range_pieces`' tail and a linked partner's trim; and
  from `Project::trim` through `follow_head`, which reads the head off the source in-points through the old ramp): a key at 0
  with the speed at the cut — the easing of the segment the cut fell in, so a hold keeps holding — keys at or before the cut
  dropped, the rest shifted back; a cut before the first key pins nothing (same curve, the cap is never exceeded); pulled
  earlier every key shifts later and the new head runs at the first key's speed; keys that round onto one another are
  dropped, never the ramp. A *tail* move (`move_tail`, `Clip::tail_shift`) leaves the keys and takes the source the ramp plays
  there (`along(d + by) − S`); `handles` (what roll, slide and carried trims clamp to) are what the footage either side plays
  for through the ramp; a slip keeps keys and length (the area under the curve is the source span); `slice`, the beat snap,
  `fit_end` and the span `cut_range_pieces` removes go through the map. `Project::set_speed_ramp(clip, keys)`
  (`Timeline::set_speed_ramp` / `set_speed_ramp_linked`, label "Set speed ramp") keeps the window and changes the length, so it
  ripples like `set_speed`; `check_speed_ramp` takes ≤16 keys at times `0..=MAX_RAMP_TIME` with finite speeds in range, no two
  at one time (sorted for you); no keys clears, and clearing a ramp that is not there is an error. `set_speed` **replaces** a
  ramp (on every clip of a link group); linked partners get the same ramp on the timeline (`core-links.md`). A title must have
  none (`generated_violations`). Diff: `speed ramp 3 keys, 0.30×–2.00×`, `speed ramp cleared`, `speed ramp 3 → 4 keys, …`,
  `speed ramp retimed` (a head trim) / `changed`. OTIO and EDL cannot carry a ramp: they write the average speed
  (`export_speed`: `mean_speed` in the clip's direction) with a warning, and OTIO keeps the ramp in `metadata.kerf`. Audio sync
  and music fit refuse a ramped clip; `review_cut` compares neighbours' speeds at the cut (`pace_gap`).
- **Stabilization** (`model/stabilize.rs`). `Clip.stabilize: Option<Stabilize>` (absent when `None`; schema v18):
  `strength` `0..=1` (default 0.5; the smoothing radius, up to a second of frames) and `zoom` percent `0..=MAX_STABILIZE_ZOOM`
  (30) or `None` for automatic. `check` refuses a value out of range, `normalized` holds a stored one to them for the readers;
  `Clip::is_stabilized` is set and not a title. Only the setting is in the timeline: the measurement is a cache
  (`crate::stabilize`, `engine-cli.md`), so a split or a trim keeps the setting (a clone) and the cache, a paste or a duplicate
  copies it, and it is not carried to linked partners (a picture's look, like the grade). Diff: `stabilized (50%)`,
  `stabilization 80%`, `stabilization off`.
- **Animated SVG overlays** (`Timeline.svg_overlays: Vec<SvgOverlay>`, `svg/`, pure +
  unit-tested; the engine side is `engine-cli.md`). An overlay is SMIL-animated vector art
  drawn between `start` and `end`, fit *contain* and centred into the rect `x, y, width, height`
  (frame fractions), above the clips and below text overlays (list order; captions stay on
  top). **`svg` stores the sanitised source, so stored == rendered**: `svg::sanitize` is an
  allow-list run once on the way in — scripts, `foreignObject` / `iframe` / `audio` / `video`,
  elements outside resvg's vocabulary, `on*` and foreign-namespace attributes, anything but
  `#fragment` links (an `<image>` may carry `data:image/png|jpeg|gif|webp`, **whose header says at most 16 Mpx** — the
  decoder allocates what the header claims — else its `href` is cut with a warning; `svg/images.rs`: PNG / GIF / WebP by
  `imagesize`, **JPEG by our own marker walk** (`jpeg_size`: it skips the `0xFF` fill bytes the format allows before a marker, which
  `imagesize` takes for a marker and so errors on or lands on a decoy frame header; the largest frame header before the end of the image
  is the size)), external `url()`,
  `@import`, `@keyframes` and `animation*` / `transition*` (warning "CSS animations are not
  rendered — use SMIL"); a plain DOCTYPE is cut, one with an internal subset is refused; caps
  128 KiB, 20,000 elements, depth 64, 2,000 animations, 1,000 values each, `MAX_SVG_OVERLAYS` =
  50 a timeline. **Depth is read off the bytes before roxmltree sees the text** (`nesting_exceeds`: it
  recurses per open element with no limit of its own, and ~3,500 nested `<g>` — 25 KB — abort a 2 MiB
  thread's stack; comments / CDATA / PIs and quoted values are skipped); an animation's `attributeName`
  must be an XML name with at most one prefix (`is_attribute_name`) because `at` writes it into the
  document as markup (`x="1" href="/etc/passwd" onload="a" y` was an injection), and only `xlink:href` of
  the XLink attributes survives, **and only when no unprefixed `href` is beside it** — resvg reads
  `xlink:width` as `width` and the unprefixed `href` over the XLink one, and **roxmltree's bare-name
  `node.attribute("href")` matches `xlink:href` too** (use `sanitize::plain_attribute` for "no namespace"). **It is a splice, not a re-serialisation** (byte ranges of the original, so the
  second pass over its own output changes nothing — a test sweeps nasty inputs for exactly
  that). **`SvgDoc::parse` + `SvgDoc::at(doc_t)`** are the evaluator: the document is read once
  (roxmltree, then a usvg parse to refuse what resvg cannot size — a `viewBox` or pixel
  `width`/`height` is required, since resvg sizes a bare document to its content and an
  animation would change that per frame; `<use>` / `feImage` expansion is counted from the XML
  *first* — more than 100,000 drawn nodes, 128 levels or a reference cycle is refused (a ring of
  **gradients** by `href` is not: each only lends its stops, and usvg breaks the loop), since usvg
  expands a cycle to its million-node limit before it notices; the target of a `use` is its unprefixed
  `href`, else `xlink:href`, as resvg's, and a `url(#id)` in `fill` / `stroke` / `mask` / `clip-path` /
  `filter` / `marker-*` (attribute, **every declaration of `style`**, or a `<style>` rule — see below) counts the target again for every element that draws it; a `pattern` /
  gradient / `filter` `href` is followed too, and `<image>` pixels are summed the same way — more than 64 Mpx decoded in a frame,
  each `<use>` of an image decoding it again, is refused — and `SvgDoc::widest_image_pixels` is what a frame's decode must hold),
  and `at` returns a *static* SVG for one moment —
  animation elements cut out, animated attributes spliced in — which is what every renderer is
  handed, so a still, a stream and an export of the same moment are one picture. A CSS property
  (`opacity`, `fill`, `visibility`…) is written by appending `;prop:value` to `style` (a rule in
  `<style>` beats a presentation attribute, an inline declaration beats the rule; no leading `;` after an
  empty or `;`-ended style, where resvg's CSS reader would drop it); geometry is a replaced / inserted
  attribute. The value an animation starts from is the element's own, else for an **inherited** property
  (`values::is_inherited`: fill, stroke*, font-*, letter-spacing, visibility, …) the nearest ancestor's
  `style` declaration or attribute, else the initial one (`opacity`, `display`, `stop-color` are not inherited) **The subset**: `animate`, `animateColor`, `set`,
  `animateTransform`; `begin` / `end` clock values, `id.begin|end ± offset` syncbases (resolved
  once, memoised, 32 deep, a cycle is unresolved), `indefinite`; an `end` list with an **unresolved** item (an event, a syncbase on nothing or on an animation that
  never begins / ends) leaves the interval open — its natural length — where one whose times all precede the begin
  makes no interval; `dur`, `repeatCount` /
  `repeatDur`, `min` / `max`, `fill`, `restart`, `values` / `from` / `to` / `by`, `keyTimes`,
  `calcMode` linear / discrete / paced / spline (Newton + bisection), `additive`, `accumulate`
  (ignored for a `to`-only animation, as in SMIL; the repeat index is an `f64`, not a saturating `u32`)
  and the sandwich (begin time, then document order; a transform sum post-multiplies onto the
  base transform). A frozen value is the last one when the active duration is an exact multiple
  of `dur`, else the value at `AD mod dur`. Colours blend per sRGB channel, paths blend only on
  one command sequence, mixed units and keywords step. **Anything else is a warning, never a
  failure** (`SvgDoc::warnings` / `check_svg`): events, `repeat()`, `accessKey`, wallclock never
  fire; `animateMotion`, `animate` on `transform`, animating `href` / `style` / `on*` and a base value that comes
  from a `<style>` rule is not seen (the default is used) — all dropped or ignored with a warning. **Dropped too is anything that
  would make the guard (which reads the document as stored) wrong afterwards**: an animation of `mask` / `filter` / `clip-path` / `clip` / `marker*` / `id` / `class`, one whose `values` / `from` / `to` / `by` hold a
  `url(` **unless it animates `fill` or `stroke` between gradients** (a paint server is a handful of stops however often it
  is used; "gradient" = an element that is the only one with its id, so there is no doubt which element `url(#g)` reaches), and one of `radius` / `order` / `numOctaves`
  (they set the cost of a filter primitive). **A `<style>` rule keeps its `url(#id)`** (Illustrator's "Internal CSS" writes
  `.stN{clip-path:url(#SVGID_n_)}`): `svg/rules.rs` reads every rule's *subject* (its last compound selector) and counts the
  reference on each element that subject could match — the name, classes and id it spells out, nothing else (an ancestor, a
  pseudo-class or an attribute test only narrows, so ignoring them over-counts), and **every** element when it cannot read the
  selector (`*`, `svg|rect`, an escape). A rule that reaches the elements inside what it points at (`rect { mask: url(#m) }` over a
  mask holding a rect) makes that mask refer to itself and is refused like any loop. **A later element repeating an `id` loses it**
  (warning): usvg reaches the *last* element of an id through `url(#id)` and an href chain and the first through `<use>`, the guard
  one, so the sanitiser leaves one — the first, which is also the browser's. A document whose filters, layers or overdraw
  would take seconds a frame is refused when it is read
  (`svg/cost.rs`: `feMorphology` radius, `feConvolveMatrix` order and `feTurbulence` octaves times the region, plus a pixmap per
  isolated group and the pixels each path covers, all **at the scale resvg really draws with — every group's `transform`
  multiplied on the way down, the `viewBox` one usvg puts on the root and an animated one included — and a region held to
  resvg's own 5x canvas**; at a nominal 1080p when read, and again by the rasteriser on each moment's tree at the real size).
  `svg::may_animate(src)` is false when the sanitised text has no animation element (a text test; `true` is only "maybe"), which is
  what lets the engine keep such a title as one picture. Start and end are held to 24 h (`MAX_SVG_SECONDS`).
  `natural_duration` (all animations settled; `None` for a still or an endless loop) is what
  `add_svg_overlay` defaults the end to, held to 2–30 s (else 4 s; `SvgDoc::default_span`).
  `SvgOverlay.template: Option<TemplateInstance>` is set when a title came from a motion template
  (`core-project.md`, "Motion templates"): the controls and their values, skipped in the JSON when `None`.
  Replacing the artwork through `SvgOverlayPatch.svg` clears it; the template values are changed through
  `Project::set_template_values_parsed`, not the patch. `svg/splice.rs` is the id-addressed byte-range
  splice (`Elements`, `Part::{Text, Attribute}`, `apply`) the controls ride on. Its helpers read an
  attribute by the name the source spells (`xlink:href` is the XLink one, a bare `href` is not).
  `SvgOverlayPatch` has `svg` (custom artwork), `preset_params` (re-fill) and `preset_kind` (another
  preset, same id and draw order; `svg` excludes both, custom art refuses both). **Presets** (`SvgPresetKind`
  `fade_up` / `typewriter` / `lower_third` / `word_pop`, `svg::render_preset`) are only SVG the
  subset understands, drawn for a viewBox equal to the delivery frame (`Timeline::preset_frame`:
  `format`, else 1920x1080), blank and static at their duration. `SvgOverlay.preset` remembers
  how they were made, and **the artwork is re-made** when the span changes length, the params
  change, the delivery format changes (`set_delivery_format`) and in `for_delivery` for a new
  shape (custom art is only scaled). `slice` clips and shifts like text but adds the cut-off
  front to `offset` (`SvgOverlay::doc_time(t)` = `t - start + offset`), so a range export or the
  playback hand-over joins the animation part-way instead of replaying it. Ripple never moves
  overlays; the diff reuses `OverlayAdded` / `Changed` / `Removed` with `overlay_id`
  ("Added animated title “X” at 0:01.0–0:04.0"); `CutSummary.has_text` stays text-only. The catalogue and one
  drawing of each preset are written to `frontend/src/lib/fixtures/svg-presets.json` for the browser harness
  (`svg_preset_fixture_is_what_this_engine_does`; after an intended preset change run
  `KERF_BLESS_SVG_PRESETS=1 cargo test -p kerf-core --no-default-features -- svg_preset_fixture`).
- **Generated clips** (`model/generator.rs`, pure + unit-tested): `Clip.generator: Option<Generator>` (absent from the
  JSON when `None`, so a clip of footage writes the bytes it always did) makes a clip whose picture is *made*: today one kind,
  `Generator::Title(TitleClip {name, svg, template?})`, tagged `"kind": "title"` — an animated SVG title on a **video track**,
  so it takes `transition_in` (a dip to white from the shot before it), fades, transform / channel keys (an **animated
  placement**), colour, grade and effects, and is trimmed, split, rolled, slipped, slid, rippled and moved like any clip. Its
  `asset_id` is `GENERATED_ASSET` (the nil UUID, which no asset has: whatever looks a generated clip up in the library by
  mistake finds nothing rather than the wrong file), **its source is the document's own clock** (`source_in` is what a head
  trim or a slice cut off the front, as `SvgOverlay::offset` is; `Clip::generator_time(local)`), it has no sound, and past the
  end of its animations the document holds its last state, so extending the clip holds the end of the title. `svg` is
  sanitised and `template` behaves exactly as on a floating title (`core-project.md`). **Invariants** (`check.rs`,
  `Rule::ClipGenerated`): a generated clip is on a video track, names no asset, plays at speed 1 (no reverse), is not linked,
  and its window lies within `0..=MAX_SVG_SECONDS`; and (`Rule::TitleClipCount`) a timeline holds at most `MAX_TITLE_CLIPS`
  (50) of them, each title past the cap a violation of its own, so any op that adds one (a split copies the document) is
  refused — so `set_speed`, a reverse or a link is refused by the invariant net for
  every op at once. `Timeline::title_clip(id)` / `title_clips()` / `title_sources()` (every title's source, floating and on
  tracks: what the font collection reads). The diff says `Added title “Card” to V1 at …`, `Removed title “Card” …` and, for
  the content, `name “A” → “B”`, `Tagline “A” → “B”` or `artwork changed` (`generator_changes`). A generated clip's footage
  reaches `MAX_SVG_SECONDS` in `Project::source_limits` (one entry keyed `GENERATED_ASSET`), and `transition_fx` gives it an
  unbounded handle (a dissolve out of a title draws its document on) and `alpha` (its pictures are transparent). **Presets
  are not title clips yet**: a preset is laid out for its length and would have to be re-made by every op that changes a
  clip's length; a template or custom artwork holds its end instead. (`assemble_by_tag` renders a preset once for its span and
  stores it as artwork, so a later trim does not retime it.) `frontend/src/lib/title-clips.ts` mirrors placement,
  naming and lookup.
- **Asset tags** (`model/tags.rs`, pure + unit-tested; storing, staging and `assemble_by_tag` are `core-project.md`):
  `Asset.tags: Vec<Tag>` (absent from the JSON when empty). `Tag {key?, value}` is `location: Cumbuco` or the plain `jump`
  (`Tag::parse` splits on the first `:` when what precedes it is a key — one word of at most 32 characters, so `intro 10:30` is
  a plain tag, while `: x` is an empty key and refused; `Display` writes it back). **Spelling**: whitespace (tabs and line breaks included)
  collapses to one space, other control characters go, the key is lowercased, a value keeps its case; an empty key or value is
  refused, a key is at most `MAX_TAG_KEY` (32) characters, a value `MAX_TAG_VALUE` (64), an asset `MAX_ASSET_TAGS` (64) tags.
  Tags compare ignoring case, `dedupe_tags` keeps the first spelling, and several values of one key (`rider: A`, `rider: B`)
  are fine. `TagEdit {add, remove, remove_keys}` (`deny_unknown_fields`) applies in that order — `remove_keys` (every tag of a
  key), `remove` (exactly that key and value; a plain tag takes off only a plain one), `add` — normalizing everything it
  names and refusing the whole edit on one invalid tag; replacing a location is `remove_keys: ["location"]` with
  `add: [location: X]`. `a.then(&b)` is the one edit that does `a` and then `b` (keys and removals union; `a`'s adds that `b`
  takes off drop out; a property test pins it over random edits), which is how an agent's staged edits stack. `apply_capped`
  is `apply` for an edit run later over tags that changed since: it never refuses, leaves the adds past `MAX_ASSET_TAGS` off and
  skips an entry that does not normalize. `TagFilter {key?, value?}` is one term: `{key, value}` that tag, `{key}` any tag with the key, `{value}`
  a plain tag (`TagFilter::parse`: `location: X`, `location:*`, `jump`; the same key rule as `Tag::parse`); `tags_match(&[TagFilter], &[Tag])` needs every term.
  `TagSummary::of` is the vocabulary (`keys[] {key, assets, values[] {value, assets}}` and `plain[] {value, assets}`, A to Z,
  an asset counted once per value). `group_by_key(assets, key, order, include_untagged)` is the grouping `assemble_by_tag`
  uses: groups follow `order` (ignoring case), the rest A to Z, the assets lacking the key last as a group of no value when
  asked; an asset with several values of the key joins the first in that order; `order` entries no asset carries come back as
  `unmatched`. `StagedAssetChange.tag_edit` is the held-back form.
- **Thumbnails** (`model/thumbnail.rs`, pure + unit-tested): `Timeline.thumbnails: Vec<Thumbnail>` (absent when empty, so a cut
  without one stores what it always did; nothing that renders the cut reads it — `slice` / `for_render` copies drop it). A
  `Thumbnail` is `time` (timeline seconds), `width` x `height` (`parse_thumbnail_size`: `youtube`, `vertical`, `square`, `4:5`,
  `16:9` or `WxH`, 16..4096), `focus_x` / `focus_y` (where `crop_in` centres the largest rect of its shape in the frame, kept
  inside), `graphics` (`VideoGraphics::{None, Titles, All}`, `background_of`: no titles / text / captions / title clips, no
  captions — the default — or everything) and `layers: Vec<ThumbnailLayer>` (sanitised `svg` fitted "contain" into a rect of
  fractions, `rect_px` rounding like a title's; `text: Option<SvgPresetParams>` for a headline, `template: Option<TemplateInstance>`,
  `at`: the moment drawn, `None` = `settled_moment`, the latest of 17 samples within 1 % of the most coverage, so a title that
  animates out is drawn settled). `background_scale` renders the frame larger than the delivery (≤ 7680 px long side) so a crop
  of another shape is not upscaled. The diff has one `ThumbnailsChanged` entry ("Changed the thumbnails", detail `added “A”,
  changed “B”`), and partial accept lays the proposal's whole list (`subset.rs`, `staged-subset.ts`).
- `timecode.rs` — SMPTE timecode at the timeline's `f64` rate: `TimecodeRate` (nominal base, frames
  dropped per minute — drop-frame at 29.97 / 59.94, told by closeness to `30000/1001` / `60000/1001`),
  `fields` / `frames` / `format` and `parse` (Premiere-style entry, relative `+` / `-`, a dropped label
  read as its minute's first frame), all held to `MAX_TIMECODE_FRAME` so the TS mirror (`timecode.ts`) does
  the same integer arithmetic in doubles. Pure; the frontend's tests replay its cases.
- `hostile.rs` (only under `cfg(test)` or the `test-support` feature, which kerf-gpu, kerf-app and kerf-core's own integration tests turn on as a dev-dependency; never in a release build) — seeded mutation for robustness tests: every prefix, bounded
  rounds of bit flips / inserted / deleted / duplicated bytes, and each number pushed to the edges of
  `u32` / `i64` / `f64` (`number_variants`), each case under `catch_unwind`. Used by
  `tests/hostile_input.rs` (caption files and their import, the stored timeline), `engine/cli/hostile_tests.rs`
  (ffprobe JSON, source traits, silence / scene logs), `source_probe.rs`, kerf-gpu's y4m / `showinfo` and
  kerf-app's settings; `frontend/src/lib/hostile-input.test.ts` is the page's twin. A panic found keeps its
  input as a regression test.
