# kerf-core: `model.rs` — domain types and timeline math

- `model.rs` — the domain types and the only place timeline math lives: `Asset`,
  `StreamInfo`, `Timeline`→`Track`→`Clip` (the EDL), `AssetAnalysis`. A `Clip`
  references a source range (`source_in`/`source_out`) of an asset at a
  `timeline_start` — non-destructive. Besides the geometry (`Transform`) / color
  (`Color`) / `Transition` fields, a clip carries a `Vec<VideoEffect>` and
  `Vec<AudioEffect>` (per-clip filter chains) and a `Vec<Keyframe>` (transform
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
  views of it, so the still, the preview, the plan and the export cannot disagree. **Nothing is converted
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
  `set_property_easing(clip, prop, time, easing)` and `copy_keyframes(from, to, props, offset)` (every
  keyed number when `props` is empty; a negative offset cuts the head off with `rebase_head`, so the
  destination opens on the pose; replaces the destination's tracks for those numbers; refuses a number
  the source has no keys for and the same clip twice) are the surface ops. **Edits keep channels where
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
  unit tests are mirrored by name in `links.test.ts`. **Not animatable yet**: the crop edges (`crop`'s output size is fixed
  when the graph is configured — `w` / `h` are evaluated once — so an animated crop needs the
  zoom-and-pan reformulation, not a number per frame) and the mask's centre / size / feather (a `geq`
  expression could carry them; left for when the dope sheet can edit them). `frontend/src/lib/channels.ts`
  is the faithful TS mirror (resolver, `upsertKey`, `rebaseHead`, `propertyKeysShifted`, ranges; both
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
  9:16 frame), timing lines by *character share*
  because neither speech backend reports word timings; lines too short to read
  merge back into a neighbour instead of flashing; and no two lines are ever on
  screen at once (captions are one lane of text, and the same footage reaching
  the cut twice would otherwise collide with itself). `TextOverlay.generated`
  marks what it wrote, so regenerating replaces its own set and leaves a typed
  title alone.
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
  re-exports `Fit`), and `Timeline.format` is the frame the project is cut for.
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
  stripped, no blank line between cues with the index handed back to its cue) and ASS /
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
  `read_caption_file` (`.srt` / `.ass` / `.ssa`, regular file, size) runs before the lock
  in the Tauri command and the MCP tool.
  Inherent helpers (`Timeline::locate`, `Track::end`/`reflow`, `Clip::duration`,
  `Timeline::slice` — the shifted sub-timeline copy behind range export) back the
  operations. **Beat alignment** lives here too and is pure + unit-tested:
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
  carrying the clip/track/time so a UI can jump there. Everything is matched by
  **id**, so a reordered track reads as the handful of moves it is rather than as
  every clip having been replaced, and a removed track is one entry instead of one
  per orphaned clip. `StagedEdit` is a pending proposal (base seq, the edit
  labels, `stale`, and its diff). **Ripple** is here too, pure + unit-tested:
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
