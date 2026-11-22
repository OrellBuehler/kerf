# frontend-timeline — brief

Rules only. The full file `.claude/docs/frontend-timeline.md` is authoritative and holds the measurements and the
story behind each rule: grep it for the phrase in parentheses.

The timeline is a bespoke NLE timeline rendering `editor.timeline` (ruler, tracks, clips at `ui.zoom` px/s,
playhead), with waveforms, filmstrips, minimap, clip overlays, mixer and Web Audio playback. It lives in
`frontend/src/lib/`: `Timeline.svelte` is only pointer plumbing; the logic is pure bun-tested modules (`frames.ts`,
`selection.ts`, `multi-move.ts`, `trim-tools.ts`, `linked-trim.ts`, `zoom.ts`, `waveform-view.ts`,
`filmstrip-view.ts`, `mixer.ts`, `levels.ts`, `audio.ts`, ...).

## Map of the full file (grep the bold lead)

- Opening paragraph: gestures, frame quantizing, ripple, selection, group move, zoom, lane width.
- `**Long lanes are virtualised**` · `**The ruler is timecode.**` · `**Roll, slip and slide**` · trim to playhead.
- `**Linked A/V in the timeline**` · `**Locked tracks, clip names, the clock.**` · `**Gestures give feedback**` ·
  `**Autoscroll**` · `**A click selects and nothing more**` (playhead on a frame, seeking).
- `**A library drop is a pointer drag**` · `**The render bar**` · `**A clip with the keyboard focus nudges**` ·
  `**Track order is drawn, not stored**` · `**A proposal on the timeline is marked**` · `**One timecode**`.
- scrub bar · `**Waveforms**` · `**A title clip**` · `**Filmstrips**` · `**Track heights**` ·
  `**Animated SVG titles share the titles lane.**` · `The **toolbar**` · `The **minimap**` · `ClipOverlays.svelte`
  (then transport, `audio.ts`, mixer, meters, limiter, master FX, Duck, Measure run on in the same paragraph).
- `**Fit to video**` (music) · `**A ramped clip**`.

## Gestures and quantizing

- Frame-quantize every gesture (`frames.ts`); keyframes stay in seconds. Rate = `editor.fps` / `timelineFps`:
  `Delivery.fps`, else the first video clip's, else 30 (`export_format`'s rule) (`**Gestures are frame-quantized**`).
- Round once, from the raw pointer position, as `k / fps`, so equal frames are equal doubles; ghost and commit share
  that one value; a trim derives every field from it (`trimEdit`) (`from the raw pointer position`).
- A magnet in reach (`ui.snap`) wins unrounded; a bar beats a nearer beat (`gridMagnets`) but not a nearer clip edge;
  frames are not a magnet and apply with snapping off (`bar wins over a nearer beat`).
- A landing within 1 µs of a neighbour's edge is that edge (`welds`): `move_clip`'s overlap test is a strict float
  compare; a butted tail is placed by `startBefore` in the backend's arithmetic (`A landing within 1 µs`).
- Drag vs click is judged on pointer travel (3 px), never on the quantized position; an edge keeps the offset it was
  grabbed at (`Whether a press became a drag`).
- A razor cut keeps half a frame either side (`splitPoint`); the menu's split quantizes the playhead and shows no key
  (`C` is the Razor tool; nothing splits at the playhead from the keyboard) (`A razor cut keeps half a frame`).
- Escape / pointercancel / blur abandon a clip, edge or title drag; the click that follows an abandoned press is
  swallowed (`resetDragState`, roll / slip / slide abandon) or it seeks / clears selection (`The click that still follows`).
- Trims: 6 px handles, clamped to source handles, neighbours, 0.05 s minimum; a left-edge trim commits `trim_clip` with
  `timeline_start` so the right edge stays; stills extend freely (`**edge-drag to trim**`).
- Beat ticks hide below 4 px spacing; bar ticks (`barGrid`, mirror of `Timeline::bar_grid`) survive zoom-out; the
  `beats.ts` mirror is shared by ruler, snapping, harness alignment (`hidden when beats land closer than 4px`).

## Ripple, selection, group move, zoom

- `editor.rippleMode` mirrors the project flag: read in every `load()` and on `ripple-mode-changed` (toast: an agent
  moved the user's setting); `state-ripple.test.ts` pins it (`ripple-mode-changed`).
- Rippling is the backend's; the GUI only shows it. With ripple on, an edge drag is stopped only by source footage,
  0.05 s and 0 (`trimBounds`); the ghost is the outcome, one per moved clip, red and inert if the backend would
  decline: `linkedTrimPreview` generalises `rippleTrimPreview` (a bun test holds them equal with links off) (`one ghost per clip it moves`).
- Re-ask bounds at every move and at release, since the mode can flip mid-drag (`since the mode can flip mid-drag`).
- A trim may not leave the clip on another clip of its lane (after the ripple: `checkOwnLanes`) nor ask for footage
  past the asset's end or before 0; same rule refuses an added clip; `set_speed` is held to `MIN_SPEED..MAX_SPEED`
  (`**A trim may not leave the trimmed clip on another clip of its lane**`).
- Selection is a set; only `selection.ts` changes `selectedClipIds` + the primary (the Inspector's clip). Click
  replaces, Ctrl/Cmd toggles, Shift extends along the primary's track; the marquee recomputes from the selection as
  the press found it so it can shrink (`marqueeSelect`) (`*Selection* is a set`).
- Marquee skips locked-track clips (selection is what every edit acts on) but they stay clickable (`are not swept up`).
- The click that ends or follows an abandoned marquee is swallowed, held until it arrives or the next press, not on a
  timer (`held until it arrives or the next press`).
- Escape mid-drag restores the selection; timeline and preview abandon handlers run in the capture phase and stop the
  event so abandoning never also clears; `#setTimeline` prunes removed ids (`in the capture`).
- Group move: Δt from the grabbed clip's snapped, quantized start (group's own edges are no magnet) plus one lane
  offset within each kind's lanes (`planMove`); pressing a selected clip keeps the group (`*Group move*`).
- The drawn verdict must equal the backend's `Timeline::move_clips` (group as a group, before 0 refused not clamped,
  locked / missing lane refused; a property test replays drags against `multi-edit.ts`); refused = red ghosts with the
  reason, release does nothing; a valid drop is ONE `editor.moveClips` (`is the backend's: one ghost`).
- Delete = `removeClips(ids)` (ripple follows project mode), Shift+Delete forces ripple; locked clips are left alone
  and stay selected; Cut copies only what it can remove and says so (`copies only what it can remove`).
- Paste: own track unless locked, else first unlocked of its kind, else one of the clip's own kind (`clipKind`);
  footage with both kinds, or not in the bin, refuses rather than guesses (`Paste puts each clip back`).
- Duplicate stays on each clip's own track, off with the lock named; Inspector Rate / Reverse / Remove / Ripple delete
  are off on a locked track's clip (`track-locks.ts`; the core refuses all) (`Duplicate (which stays on`).
- Zoom (`zoom.ts`): 0.05-2000 px/s, `ui.zoom` stays px/s, stepped by ratio, log slider; Ctrl/Cmd+wheel exponential
  and holds the time under the pointer (`zoomAround`, scroll after the lane is rewidened); Shift+Z / Fit bumps
  `fitEpoch` via `ui.zoomToFit()`, since only the timeline knows its width (`holds the time under the pointer`).
- Zoom follows the cut until a person sets it (`ui.zoomFollowsCut`; the `ui.zoom` setter means "a person took it";
  only `followZoom`'s fit uses `ui.fitZoomTo`); an explicit Fit ends the following too, else a resize swaps the
  uncapped fit for the capped one (`**Until anyone sets the zoom it follows the cut**`).
- `followZoom` fits on open (`editor.loadEpoch`), first clip, width change (settled 150 ms), cut outgrowing the window;
  never past `AUTO_FIT_MAX` (100 px/s); never when the cut shrinks or a clip is trimmed (it would rescale the lane
  under every edit); a window under 160 px is nothing to fit to (`which would rescale the lane under every edit`).
- Lane box is at least the window wide (`min-width: calc(100% - var(--track-header-w))`, `laneW`); past the cut
  (`cutEndX`) is dimmed (`**The lane box is at least the window wide**`).
- No scrollbar may change the lane width: scroller `scrollbar-gutter: stable`, lane `overflow-x: clip` (a gesture or
  library-drop ghost widens it instead: `gestureEnd`), and `laneWidth` must not round float dust up, or classic
  scrollbars flicker forever (`scrollbar may change that width`).
- The zoom ceiling drops for a very long cut so the lane stays under 8 M px, the one thing a browser cannot lay out
  (`the one thing a browser cannot lay out`). `ruler.ts` renders only visible ticks (`not an hour's worth`).

## Long lanes (virtualised)

- A lane of 48+ clips draws only clips meeting the quantized, overscanned window the waveforms draw from
  (`visibleLaneRange`) (`**Long lanes are virtualised**`).
- A clip is a block iff narrower than 12 px of footage: one canvas per lane, window-wide, clamped to the lane so it
  adds no scroll range; a wider clip is a button however many are in view (`Whether a clip is`).
- Only what the pointer or keyboard holds is a button (`pinnedClips`: pressed, both sides of a rolled cut, focused,
  nudged); what a gesture only moves is drawn from the model (`ghostsInView`) (`Only what the pointer or the keyboard holds`).
- Hover is not an input of the drawing: the promoted button is inserted at its lane-order place (`laneButtons`),
  because a keyed `{#each}` moving a node (`.before()`) drops the pointer capture and a roll / slide never gets a
  move; a hover change must leave every lane's `blocks` and canvas as they were (`**Hover is not an input of the drawing**`).
- `hoverPartners` is one shared empty set for clips without partners (the draw effect reruns only when partners
  light); the promoted clip clears hover on leaving, the canvas only on leaving for a non-clip, or the two swap
  forever (`is one shared empty set for every clip without partners`).
- Promote on pointer move only with no button down and no gesture (`mayPromote`); a narrow clip's button has no
  padding (`clipPadding`) (`not while a button is down`).
- A pen / touch press on a block without a hover goes to the lane's clip handlers with the block's clip; a press with
  no block is the lane's (`pressOnEmpty`: marquee or seek) (`A press or right-click on a block`).
- The canvas draws what the button would show (`blockMark`) (`The canvas draws`).
- Caveats: blocks are not in the tab order; hover outline, focus ring and tooltip appear only once promoted; a block's
  proposal mark is its edge colour, not the dashed / dotted frame (`**Caveats**`). Harness: `?long=N` (`seeds a long cut`).

## Ruler, roll / slip / slide, trim to playhead

- Ruler steps are timecode steps (`rulerGrid(px/s, fps)`; a second is `nominal` frames, 30 at 29.97), labels `HH:MM:SS`
  or `HH:MM:SS:FF` of a tick on that frame. Analysis marks hang from the top, labels beside their own major tick, ticks
  on the bottom edge; beats and bars must not land on a label (`**The ruler is timecode.**`).
- Roll / slip / slide are tools `N` / `Y` / `U` (`Tool` is `'pointer' | 'razor' | TrimTool`) over `edit-modes.ts`;
  `trim-tools.ts` has everything a drag needs; `Timeline.svelte` stays plumbing (`beginTrimTool` -> `beginDrag`), ONE
  `editor.roll` / `slip` / `slide` on release (`**Roll, slip and slide**`).
- Roll grabs the nearest cut within 8 px (`nearestCut`; away from a cut is a click); follows by the grab offset,
  snapped (not to the pair's own edges), frame-rounded once (`*Roll* grabs the nearest cut`).
- Slip (`slipDelta`): drag right = earlier footage = negative backend `delta`, reversed clips included; rounded in
  timeline frames then x speed (a ramped clip's mean speed; keys untouched) (`*Slip* is`). Slide snaps the start as a
  move does, minus the clips that travel with it (`slideMembers`) (`*Slide* snaps`).
- The pointer is held to the range, not refused (`holdToRange`); the ghost is the outcome: `previewEdit` runs the
  mirror on a plain copy of the lane (`structuredClone` throws on `$state` proxies). Amber = held at a limit, red =
  backend would refuse; release re-previews, the project can move mid-drag (`**held to the range**`).
- `clamped` is judged client-side from the range: the Tauri commands answer with the timeline, not `EditOutcome`
  (`since the Tauri commands answer`).
- Trim monitor (`ui.trimMonitor`, `monitorFor`): source frames, single-flight, newest wins, repeated `{assetId, time}`
  skipped; none on an audio track (`**trim monitor**`).
- A clip removed under a live gesture (agent edit, undo, Delete) abandons it (`subjectsPresent`), nothing written;
  `ClipOverlays` hit areas (`tooled`) are inert under any tool but Select; none of the three ripples (`A clip removed under a live`).
- Trim start / end to playhead (`Q` / `W`, `ops.ts` `trimSelection`): ONE `split_remove_clips` over every selected
  clip under the playhead (`planPlayheadTrim`), so V1 + A1 undo together; follows ripple mode; toast when nothing
  to cut (`**Trim start / end to playhead**`).
- TS number printing matches Rust: `toFixedEven` rounds an exact tie to even like `{:.N}` (JS `toFixed` takes the
  larger); used by `formatTime` and refusals (`rounds an exact binary tie to the even digit`).

## Linked A/V in the timeline

- A click selects the clip and its partners (`clickSelectLinked`); Alt-click selects one. Alt is the escape hatch from
  links, read live from pointer and key: `link: false` on drag, edge trim, razor, roll / slip / slide, *Remove only
  this clip* (`**Alt is the escape hatch from links**`).
- Context menus are built by hand per right-click; keep them honest via `menu-items.ts` (`tidyMenu`, `muteItem`,
  `removeRows`) (`keeps them honest`).
- A drag's plan is a `$derived` of pointer, cut and Alt (`planMove` with `{links}`); partners are carried by the same
  Δt on their own track (`withLinkedMoves`' rule; the grabbed clip's own partners are never lane-shifted) and checked
  with the group; `moves` names only the clips dragged (the backend adds the rest) (`by the same Δt on their own track`).
- Edge-trim bounds are the clip's narrowed by each sharing partner's neighbours (`linkedTrimBounds`; a partner's
  footage is no limit). `linkedTrimPreview` = `trim_clip` + `carry_extent_edit` + per-lane ripple + `conformLinks` +
  carried-lane check + sync guard on a scratch copy; `api-links.test.ts` holds it equal to the harness commit
  (`holds it equal to the harness commit`).
- Refusals draw red with the reason (locked partner, clip outside its group or a picture in the way, linked clip it
  would cover, clip left under 0.05 s); a sound it cuts back is drawn and named (`trimmed`) (`a sound it cuts back to make room`).
- Roll / slip / slide run `previewEdit(..., links)` over `*Linked` edits and `*RangeLinked` clamps; partners are
  `partner`-role ghosts with a `trackId`; `gestureReason` adds "or hold Alt to edit this clip on its own" (`hold Alt to edit this clip on its own`).
- Clip menu and keymap share `linkPlans` (`ops.ts`): Detach audio / Reattach audio are one revision for the whole
  selection (`detach_audio_clips`, `reattach_audio_clips`); Link / Unlink disabled with the backend's reason
  (`planLink` / `planUnlink` are the validation halves) (`**one revision for the whole selection**`).
- A detached picture plays none of its sound: no volume line, no mixer strip if all its track's clips are detached,
  Inspector Volume / Audio effects replaced by a note (fades stay) (`A detached picture plays none of its sound`).

## Locked tracks, clip names, gesture feedback

- A locked track looks locked: hatched lane (`[data-lane][data-locked]`), `saturate(.45)` clips (not dimmed: label
  contrast), padlock chip kept at the window's left edge; the header lock is a padlock glyph, amber belongs to
  selection and solo (`locked track looks locked`).
- A clip name (`.kname`, `clip-label.ts`) is `position: sticky`; the clip must be `overflow: clip`, not `hidden` (a
  hidden one is a scroll container, the label would stick to the clip); under 40 px no name; chip held to 4.5:1
  (`NAME_CHIP_PCT`) (`not dimmed, which would drop`). Focus ring is `--focus-outline`; Enter / Space selects a focused
  clip (`A clip has a **keyboard focus ring**`).
- Snap lines (`snap.ts`) are read off the result (`markAt` / `snapLines`), never a second snap, so they cannot
  disagree with where the clip went; a merely frame-rounded edge is on nothing; none with snapping off (`*Snap lines*`).
- Readouts (`gesture-readout.ts`, one `readoutChip` snippet; times in the interface's time mode): trim amber when the
  pointer asked for more than allowed (`trimLimitWhy`), red with the engine's reason when refused; trim feeds a
  one-cell `ui.trimMonitor` (`trimEdgeMonitor`) (`*Readouts*`).
- The razor draws its cut before the press, through linked partners (none with Alt) at `splitPoint(snapPoint(...))`
  (`The **razor** draws the cut`).
- Autoscroll (`autoscroll.ts`): 48 px band, speed proportional to depth, px/s over real frame time with the sub-pixel
  remainder carried; the gesture then re-reads the pointer (`rereadPointer`, handlers take a `PointerLike`) (`**Autoscroll**`).
- A press that began in the band waits until the pointer was in the interior or moved 16 px toward the edge; a slip
  counts scroll as travel; the lane widens with the gesture (`liveEnd`) (`A press that began inside the band`).
- Escape / pointercancel / blur / lost capture stop autoscroll with the gesture; a ruler scrub restores the playhead
  exactly (`ui.restoreTime`, not re-rounded), an in / out drag restores the marks (`scrubFrom` / `markFrom`), a marker
  drag writes nothing (`not re-rounded to a frame`).
- Playhead follow (`followScroll`): pinned inside the edge it runs toward (no 75 % page flip); off-screen jump scrolls
  smoothly (instant under `prefers-reduced-motion`); never while a gesture is held (`pinned inside the edge it runs toward`).

## Playhead and seeking

- A click selects and nothing more: on a clip (locked too), a title, or a roll / slip / slide press that never drags,
  the playhead stays parked. Ruler, scrub bar, minimap seek; empty lane clears a selection if any, else seeks
  (`**A click selects and nothing more**`).
- `ui.seek` rounds to the nearest frame (`frames.ts` `seekTime`; the cut's end stays exactly the end) (`**The playhead is on a frame**`).
- Programmatic "go to the start of X" (title, review card's changed clip, transcript line, `[` / `]` / Page Up / Down)
  passes `{ snap: 'up' }`: nearest could land half a frame before a title, not yet on screen to select (`rounding to nearest could stand up to half a`).
- A step (arrows, Shift a second, scrub keys) is `stepFrames` from the frame shown (`shownFrame`, the timecode's
  floor), else a mid-frame playhead rounds back or skips one (`measured from the frame being`).
- A keyframe diamond seeks `{ snap: 'exact' }`: the Inspector's keyframe-at-playhead check assumes the playhead is
  at the key (`keyframes are in seconds, and the Inspector's`).
- The playhead cannot stand past the cut's end: an effect in `+page.svelte` pulls it back after a shortening edit
  (`cannot stand past the end of the cut`). Its head lives in the sticky ruler (`lives in the sticky ruler`).
- "Drop media here to start cutting." sits in the first lane (sticky), not centred over the stack (`Drop media here to start cutting.`).

## Drops

- A library drop is a pointer drag, not HTML5 drag-and-drop (never reaches the page on Windows, `app.md`): `MediaBin`
  starts it past a 4 px slop, writes `ui.dndAsset` / `ui.dndPoint`; the timeline hit-tests `laneAt` (`[data-lane]`);
  release calls `ui.assetDropTarget` (`onLaneDrop`, one `editor.add`); Escape / pointercancel / blur abandon. One
  window only: a bin in a popout does not drop onto the main lanes (`**A library drop is a pointer drag**`).
- A sound effect drops the same way (`ui.dndSfx`, `ui.sfxDropTarget`, `onSfxDrop`) but only on an audio lane, landing
  by its hit (`dropGhostFor`); red where it cannot land or would overlap (with ripple on, running into a clip's front
  pushes it, only starting inside one is red; red is refused on release in words); one `addSoundEffects` with the
  lane's `track_id`; a popped-out panel does not drag (`but only on an audio lane`).

## Render bar, nudge, track order, proposals

- Render bar (`RenderBar.svelte`, `render-bar.ts`, `get_render_bar` = `kerf_core::render_bar`): asked again 400 ms
  after the cut, media, GPU setting or preview width changes, one request in flight and one after (`**The render bar**`).
- Red `stall` where playback stalls whoever plays (reversed clip, proxy wait under Proxy only, frame with no plan);
  with GPU preview on, green `gpu` / yellow `stream`; off (default) only red; no span for audio-only, muted video, gaps
  (`**red** (`).
- It does not know what only a running session knows (self-test, untrusted seeks, backend down); a cut over 25 000
  frames is judged every n-th frame; a proxy wait names the clip, not build progress (`What only a running session knows`).
- Nudge (`nudge.ts`): arrows move a focused clip (and selection) a frame, Shift a second, Alt leaves partners; a run
  is ONE edit written after 450 ms quiet, on Enter or on focus leaving; drawn via `planMove`; Escape abandons
  (`**A clip with the keyboard focus nudges**`).
- The undo key (`settings.actionFor(e) === 'edit.undo'`) during an unwritten nudge run takes the run back, not the
  edit before; redo drops the run; a refused move is a toast (`during an unwritten run takes the`).
- Track order is drawn, not stored: `displayTracks` shows video highest layer first, then audio; `Timeline.tracks`,
  every edit, export and the agent's tools keep V1 ... A1 order; lane lookup is by id (`data-track-id`)
  (`**Track order is drawn, not stored**`).
- Group move gets the drawn order: `planMove` takes `drawnMoveTracks` (`multi-move.ts`), so lane offsets and "no video
  track above V2" count as drawn and a picture dragged up takes its sound up (`drawnMoveTracks`).
- Proposal marks (`staged-diff.ts` `diffView`, staged `TimelineDiff` entries matched by id; `DiffEntry.overlay_id` is
  the title an `overlay_*` entry is about) apply while `editor.previewingStaged`: added solid `--diff-add`, moved /
  trimmed dashed `--diff-shift` (line style carries it without hue), removed hatched `--diff-remove` ghost
  (`**A proposal on the timeline is marked**`).
- Banner counts: places = distinct times (`changePoints`), changes = entries; keep the two apart (`so the two counts are not mixed`).
- The cut is read-only while previewed: presses that would start a drag / trim / roll-slip-slide / title or marker
  drag do not start (`holdForReview` remembers the press; first move past slop, a razor click or arrow nudge says
  "Exit the preview to edit"); selecting works; editing controls are off with the reason (`heldForPreview`); `editor`
  refuses any edit that gets through (`**The cut is read-only while it is previewed**`).
- Before preview: a strip over the toolbar and a dotted agent-hued frame on touched live clips (`touchedOnLive`; what
  it only adds has nothing to mark) (`**Before it is previewed**`). Previewed, the same bar (`ProposalStrip`) carries the
  stepper (`Place 2 of 3 (5 changes in 3 places)`), the legend and Exit preview / Apply / Discard at the same height, so
  nothing moves (`there is no banner above the dock`).
- `review.prevChange` / `review.nextChange` (Page Up / Down, `[` / `]`; rebindable; only while a proposal is on the
  timeline) use `stepChange` with `snap: 'up'`. Page keys scroll whichever panel has the keyboard, so review takes
  them only from the editor surface (`focus-area.ts`, `data-editor-surface`); `[` / `]` work anywhere
  (`Page Up / Page Down scroll whichever panel has the keyboard`).
- The mark is an `::after` frame inside the border, under `ClipOverlays`, `pointer-events: none`, not an outline, so
  partner highlight and focus ring still show; ruler labels follow the time mode (`tickLabel`'s `mode`) (`not an outline`).

## Timecode

- One timecode (`timecode.ts`, mirror of kerf-core `timecode.rs`; `timecode-entry.test.ts` replays its cases):
  `HH:MM:SS:FF`, drop-frame `HH;MM;SS;FF` at 29.97 / 59.94 (by closeness to `30000/1001` / `60000/1001`: the rate is
  an `f64`), non-drop elsewhere, hours always present (`**One timecode**`).
- `mm:ss.cc` survives only in the Inspector's editable seconds fields and prose mirroring Rust (`diff.ts` `formatTime`)
  (`survives only in the Inspector's editable seconds fields`).
- At a drop-frame rate ruler labels are drop-frame too (`tickLabel`), ticks stay on whole frames of nominal seconds
  (`so a tick reads as the playhead would there`).
- Clock = the ruler's corner (`data-timeline-time`); an empty cut is `00:00:00:00`, not the lane's 8 s minimum (`**The timeline's clock**`).
- Timecode entry (`TimecodeField.svelte`; `playback.goToTimecode`, Cmd/Ctrl+G; focuses the timeline's field, else the
  transport's): `parseTimecode` takes any of `:;.,`, fewer fields fill from the right, bare digits as right-aligned
  pairs, `+15` / `-1.00` relative to the frame shown, overflow carried, all-empty refused. Enter seeks (`frameTime`);
  Escape / Tab / click away leave the playhead; unreadable turns red and stays (`**The playhead's timecode is typed into**`).
- Scrub bar (`ScrubBar.svelte`, `scrub.ts`): hover chip kept inside the bar (`chipLeft`); ticks via `markerTicks`
  (`The transport's **scrub bar**`).

## Waveforms, titles, filmstrips

- Waveform: one `<canvas>` per audio clip covering only the on-screen part plus overscan (a long clip at high zoom is
  far beyond any canvas) (`**Waveforms** are one`).
- `waveform-view.ts` `sourceAt` maps clip pixels to source seconds through `source_in` / `source_out`, speed (ramp via
  `along` / `localAtAlong`) and reverse (read through the mapping, mirrored, never flipped) (`is the pure geometry`).
- Bucket width = widest rung (backend 2 / 10 / 40 / 100 ms, then doubling) within 1.5 device px, DPR capped at 2, a
  ramped clip at its slowest key's (`slowestSpeed`); fetch fixed 2048-bucket tiles aligned to the source clock so
  scroll, trim and a split's halves hit cached tiles (`tiles** of 2048 buckets`).
- `waveform-cache.ts` (injectable fetcher; app instance `waveforms.ts`): keyed asset + window + bucket count, joins
  in-flight, three at a time newest first, drops unwanted queued tiles, remembers a failed asset (one notification, no
  re-request per scroll, doubling hold-off to a cap, cleared by a tile arriving), LRU-bounded (`joins in-flight requests`).
- `want` must tell its owner when everything asked for was already cached or the asset is held off, because nothing
  else would (`tells its owner`).
- The draw waits for every tile and meanwhile keeps the old bitmap placed by clip-local time (stretched, not blank); a
  scrolled-out clip releases tiles and shrinks its canvas to 1x1 (a canvas keeps its whole backing store); assign
  canvas size only when it changed (`leaves the old bitmap where it was`).
- `waveform-draw.ts`: one polygon per lane, scaled by `effectiveGain` (clip volume through the track fader, as the
  export multiplies); full-scale columns (|peak| >= 0.999 or pushed there by gain) are marked only at their tips (3 px
  `--danger` top and bottom), the body keeps the wave colour (`at their **tips**`).
- A canvas cannot read `var()`: `readPalette` resolves `--waveform` / `--danger` once per `settings.theme` change (`readPalette`).
- Stereo: two lanes at clip height >= `STEREO_MIN_HEIGHT` (48 px; `laneCount`, in pixels so presets drive it), else
  one; `get_waveform` is no longer used by the timeline (MCP keeps it) (`laneCount`).
- A title clip (`title-clips.ts`) has no filmstrip; label `titleName` (`data-title-clip`); every gesture is the
  footage clip's; its source limit is the engine's 24 h, so `trimBounds` lets its right edge extend and stops its left
  at the document's start (`**A title clip**`).
- Filmstrips: one `<canvas>` per video clip over its on-screen part (`ClipFilmstrip.svelte`). `filmstrip-view.ts`:
  slots at the thumbnail's aspect in whole px, grid anchored at the clip's left edge; slot `i` shows the frame at the
  source time under the middle of its visible part via `sourceAt` and `frameAt` / `locate`; slot edges rounded as
  edges (no seams); a still repeats its thumbnail (`**Filmstrips** are the video twin`).
- `filmstrips.ts` decodes a sheet's `data:` URL via `Image` then `createImageBitmap`, never `fetch`: `connect-src`
  refuses `data:` (`which decodes a sheet's`).
- `filmstrip-cache.ts`: one decoded strip per asset (a split's clips share it); one load per asset, two at a time,
  newest first; failures use `backoff.ts` with one warning toast; bounded in bytes; LRU applies only to assets no
  visible clip holds (`hold` / `want` / `release`), so a held asset is never evicted and over-budget overshoots rather
  than thrashes (`keeps one decoded strip per **asset**`). A clip box under 28 px shows none and never fetches (`A clip box under 28 px`).

## Track heights, titles lane, toolbar, minimap

- Track heights are three presets (`track-heights.ts`: compact / medium / large); everything else is a function of
  the clip box the lane leaves, so nothing else knows about presets (`**Track heights** are three named presets`).
- Heights are UI-only (`ui.heights`, per track id in `localStorage` `kerf.timeline.heights`); a `Track.height` field
  would be engine work for a per-viewer convenience. `all` is the global choice and what the titles lane follows; a
  track set to it drops its override; the table is capped at 256 (`so it is **UI-only**`).
- Marquee hit-testing and lane `offsetTop` read the DOM, so they follow heights (`read the DOM`).
- Titles lane: compact 27 px (`MIN_TITLE_LANE_PX`); with no title or caption a 16 px strip (`COLLAPSED_TITLE_PX`,
  `titleLaneHeightFor`) with a header button adding a title at the playhead, still taking right-click and marquee. It
  must not open on hover: a lane growing under the pointer moves the clip it was aiming at (`**With no title or caption on the cut`).
- Animated SVG titles share the titles lane: `packRows([...overlays, ...svgOverlays])` (ties keep the order given),
  `.ktitle[data-kind="svg"]`, `SVG_TITLE_FILL` held to 4.5:1 under `--text-on-video` by the theme guard (`**Animated SVG titles share the titles lane.**`).
- SVG title gesture = the text title's `titleDrag`, other animated titles' edges added as magnets (`otherTitleEdges`,
  `Title edge`); ONE `update_svg_overlay({start, end})` on release; `retimeOverlay` and its keyframe re-timing are
  text only; proposals mark and ghost them (`otherTitleEdges`).
- Toolbar = `ToolbarToggle`s (`frontend.md`, Shared components): 24 px high, 24 px target at least, lit one way
  (amber outline and wash); the track header's S / L / DUCK toggles are not part of it (`Every one is 24 px high`).
- Minimap (`Minimap.svelte`, `minimap.ts`; hidden while the view takes in 98 % of the cut, `viewCoversCut`): box and
  timeline `scrollLeft` / zoom are one thing seen two ways (`windowRect`, `targetForRect`); body drag keeps zoom
  exactly, edge drag zooms (other edge stays put even after clamping), bare press jumps and keeps dragging,
  double-click moves the playhead; gestures are absolute from the press, Escape restores the view, applied like a
  wheel zoom (`pendingScroll`) (`are one thing seen two`).
- The minimap strip is `aria-hidden` on purpose: the timeline's own keys (scroll, zoom, Shift+Z, J/K/L) are the
  accessible path (`on purpose: the timeline's own keys`).

## ClipOverlays

- `ClipOverlays.svelte` owns everything on a clip beside its body: volume line, fade handles and ramps, keyframe
  diamonds (clip-local seconds; click seeks), trim edges with halos (`is everything on a clip beside its body`).
- Volume line: -36 dB ... `MAX_GAIN` (+6 dB, `mixer.ts`; one ceiling shared with the Inspector slider and track
  fader); a clip above it keeps its value, drawn at the top; a drag is relative to the clip's real level; detent at
  exactly 0 dB since export omits unity; double-click resets; only for clips whose asset has audio (`a detent at exactly 0 dB`).
- Fade handles: top corners, every clip, clamped to the clip and each other, double-click clears (`at the top corners`).
- Overlays use `--clip-accent`, not `kerf-300` (Kerf Light's amber scale is dark, invisible on dark clip bodies);
  `clip-accent` must hold 3:1 on every clip body in every preset (`theme-guard.test.ts`); volume line has a 70 % scrim
  halo (`Kerf Light's amber scale is dark`).
- Top 14 px of a clip is the handles' alone, the line's travel stays below; edge strips and the line's grab band must
  not overlap; nothing hit-testable until hovered or selected, nor under the razor, nor on a locked track (keyframes
  still seek) (`The top 14 px of a clip is the handles'`).
- Every gesture is `drag.ts` `beginDrag` (pointer capture; Escape / cancel / lost capture / blur abandon), shows its
  value live, writes ONE edit on release and holds the live value until that edit settles (`holding the live value until that edit settles`).
- In / out marks (`I` / `O` at the playhead, Shift+I / Shift+O clear) drive range export (`in/out marks`).

## Transport, audio, mixer

- Transport: J/K/L shuttle (repeat taps double to +-8x) plus Space; `audio.ts` fetches clip PCM windows over
  `get_audio` and schedules them with volume / fades / speed / reverse (`**J/K/L shuttle**`).
- Per-clip effect chains are auralized: pass `clipId` to `get_audio` (decoded through the clip's own
  `audio_effects_filter`), so the chain is part of the buffer cache key and retuning an EQ re-fetches (`**Per-clip effect`).
- That chain runs before the engine's gain envelope where export runs it after clip gain (audible only to
  level-dependent effects); volume stays in Web Audio so the fader stays live (`audible only`).
- Reverse shuttle: each clip behind the playhead plays backwards from where the playhead meets it, from a buffer stored
  the other way round (`#buffer(clip, backwards)`) first padded to the window's own length because `audio_pcm` does
  not pad; gain through `gainAutomationBack` (`audio-mix.ts`); nearest clips first (`**Reverse shuttle is audible**`).
- `MAX_WINDOW` (600 s of source) is held at the end the playhead enters by, so reverse is silent while the playhead is
  more than 600 s of source from a clip's end (`is held at the end the playhead enters by`).
- A window is cached one way round at a time; asked for the other, the copy is turned in memory and replaces it (no
  second fetch, no second copy against `CACHE_CAP`) (`one way round at a time`).
- Decode and turn write straight into the new buffer with an indexed loop, never a per-sample callback
  (`Float32Array.from` with a map was ~17x slower); `audio.test.ts` is the offset oracle (`with an indexed loop`).
- The playhead follows the audio clock; mid-playback edits re-anchor via `ui.resync()` from a `+page.svelte` effect (`follows the audio clock`).
- Tracks: `+ V` / `+ A` (`add_track` / `remove_track`), header `×`, audio DUCK toggle (`set_track_duck`). Any
  track that can be heard gets a mixer strip (fader + pan, double-click returns to neutral); a silent track none (`**DUCK toggle**`).
- `mixer.ts` is the faithful mirror of `Track::pan_gains`: preview pan must be the export's balance; a
  `StereoPannerNode`'s constant-power law would disagree with the file; `get_audio` is mono, so two gain legs into a
  merger are the stereo pair (`would quietly disagree with the file`).
- `levels.ts` holds the master bus's limits (the Rust constants), `levelNotes` (faithful mirror of `Levels::new`'s
  advice) and `estimateLevels` (harness approximation of `get_levels`, flagged `estimated`) (`holds the master`).
- Mixer panel (`Mixer.svelte`; panel registry, Audio workspace preset, Window menu): a `MixerStrip` per audible track
  plus `MasterStrip`. Audible = `mixer-strips.ts` `trackHasSound`, shared with the track header, mirroring the export
  graph's `clip_sounds`: `Clip.source_audio` (written only when false) marks a detached picture; a video track of
  only those has no strip (`**Mixer panel**`).
- M and S are 24 px wide at least, which is why a strip is 108 px (`panels-polish.test.ts` does the sum); the taper
  (`gainToFader` / `faderToGain`: unity at 0.75, floor -60 dB) is shared with the header slider so a level sits in the
  same place on both (`unity at 0.75`).
- `MixSlider` / `slider-gesture.ts`: one edit per drag on release; arrow nudges written once quiet (or Enter / blur);
  Escape abandons; double-click resets; Ctrl+Z during an unwritten run takes the run back (`gives every fader and pan the same gesture`).

## Meters and limiter

- Meters are measured: per-track bus gain and pan legs, stereo `AnalyserNode` pair, master gain, limiter, master
  analyser; they animate only while playing; the fader sits on the bus (same product as `clipGainAt`, what plays is
  unchanged) (`are **measured**`).
- The limiter is the export's own where the webview can run it: `alimiter.ts` ports FFmpeg `alimiter` sample for
  sample as `limiter_filter` spells it (replayed against an ffmpeg corpus, `engine-cli.md`) in an AudioWorklet
  (`limiter-worklet.ts`, `kerf-alimiter`) (`the limiter is the **export's own**`).
- It limits the preview's mix (mono 32 kHz decode, no ducking), so it can differ a little; tooltips say which plays
  (`limiterNote` of `audio.limiterKind()`: `pending` / `exact` / `approximate`, via `onLimiterKind`) (`which is not the export's`).
- Post the limit only when it changes, and `{reset: true}` on every `start()` and whenever the worklet re-enters the
  path, so no lookahead or gain reduction carries over (`{reset: true}`).
- Vite bundles the worklet as its own file (`limiter-worklet-url.ts`, `?worker&url`, imported on demand because bun
  cannot resolve the suffix), served from the app's origin so `script-src 'self'` admits it with no `blob:` (`bun cannot resolve the suffix`).
- A stream cannot trim `latency=1`'s lookahead: the sound is 5 ms late (`alimiterDelay`) and
  `AudioEngine.outputLatency()` adds it while the worklet is in the path so the GPU picture waits (`A stream cannot trim`).
- Fallback is the `DynamicsCompressorNode` approximation (`limiterParams`, trims its makeup gain) until the module
  loads, for good where it cannot (`AudioEnv.createLimiter`, reason via `AudioEnv.warn`), or after `onprocessorerror`
  (the compressor takes the master back, never silence). True-peak oversampling is export-only (`rather than leaving it silent`).
- `meter.ts` holds the ballistics. Ducking is export-only (no Web Audio sidechain without an AudioWorklet); the Duck
  tooltip says the preview plays the track at its fader (`Ducking is **export-only**`).
- Master FX block (Limiter, Ceiling, Duck mode): folded by default, remembered (`master-fx.ts`, `localStorage`), with a
  `fxSummary` line so a folded block does not hide an active limiter (`FX block`).
- A muted or empty strip dims name, pan, fader, meter but keeps M / S / D at full strength (`A muted or empty strip`).
- Duck mode (`MasterBus.duck_depth_db`, `set_master_duck`): **Compressor** (no depth) or **Speech gate** (ducked bus
  drops by exactly the depth); depth slider (`levels.ts` `duckToPos` / `posToDuck` / `nudgeDuck`: -40 to -1, whole dB,
  double-click -12) shown only under Speech gate; Speech gate restores the last depth held (-12 until set); one edit
  per gesture; nothing previews (`pair chooses *how*`).
- The harness keeps the key absent for the compressor so the master goes back to absent like the saved file; "No track
  set to Duck" shows while no track is flagged (`keeps the key absent`).
- Measure (master strip) calls `get_levels` over the whole cut or in -> out when both marks are set; a whole-mix decode
  under the heavy-job lease, so while it runs the button is Stop (`ui.stopMeasure()` -> `cancel_levels`, then
  `Stopping…`); the pass rejects with `levels cancelled` (`isLevelsCancelled`), quiet: no toast, last result stays
  (`**Measure** on the master strip`).
- Harness `sample-audio.ts` synthesizes a voice-like signal per asset so playback, meters and faders work under `bun
  run dev` (`synthesizes a voice-like signal`).

## Fit to video (music)

- `AssetAnalysis.music` makes an audio-track clip music; `music-ui.ts` `fitOffer` is the one gate for the clip menu,
  the Inspector's Music section and the dialog: shown only with the analysis, greyed with the backend's refusal
  (locked track, still linked to a picture, not at normal speed) (`is the one gate the clip menu`).
- Analyses load only for assets something selected or analyzed, so Timeline calls `editor.ensureAnalyses` for every
  audio-track asset (once each; `load()` forgets), else a reopened project has no beat / bar ticks or menu entry
  (`Analyses are only loaded for assets`).
- `FitMusicDialog` (`ui.fitMusicDialog`, modal) calls `planMusicFit` on open and whenever its question changes
  (newest answer wins); a refusal is shown in the card and noticed once; Fade out is enabled only when the plan runs
  over (`whenever its question changes`).
- Apply is `editor.fitMusic`: one `Fit music to length` revision, the replacing clips selected, a toast with Undo
  (`the clips that replace the music selected`).
- Harness: `music-fit.ts` is the faithful mirror of `model/music_fit.rs` (plan walk, `music_fit_clips`, `Project::fit_music`
  checks / messages), replayed by `music-fit.test.ts`; `?music=1` lays `sample-music.ts` on track A2 (`the faithful mirror of`).

## Ramped clips

- `Clip.speed_ramp` (`speed-ramp.ts`; editor is the Inspector's): badge shows the log-axis curve (`miniCurve`) and
  mean speed (`**A ramped clip**`).
- Everything that draws footage reads source time through the ramp: filmstrip slots and waveform columns (`sourceAt`
  over `along`), scene / silence / beat marks (`sourceToTimeline`), captions and transcript lines, clip width
  (`clipDuration`), edge-trim bounds (`headTime` / `tailTime`) (`reads source time through`).
- An edge trim on a ramped clip commits one `trim`; `Project::trim` rebases the ramp when the head moves
  (`Clip::follow_head`; harness `trimRamp`) so the far edge stays at the pointer (`rebases the ramp when the head moves`).
- `rippleTrimPreview` and `linkedTrimPreview` apply the same rebase to their scratch copies, `linkedTrimPreview`
  before `carryExtentEdit` (which reads the trimmed clip), so a partner is carried off the rebased ramp and a
  neighbour past the far edge is not a false overlap (`apply the same rebase to their scratch copies`).

## Open ends the full file states

- Shift = cut every track at the playhead is not built: it needs a one-revision batch split in the engine
  (`split_remove_clips` is the only batch cut) (`needs a one-revision batch split`).
- Nothing splits at the playhead from the keyboard yet (`from the keyboard yet`).
- Long-lane caveats (tab order, promoted-only hover / focus / tooltip, edge-colour proposal mark) (`**Caveats**`).
- The preview limiter can differ slightly from the export's; ducking and true-peak oversampling are export-only
  (`True peak's oversampling is export-only`).
- A bin or panel popped out into its own window cannot drop / drag onto the main window's lanes (`A panel popped out into a window does not drag`).
- The `@xyflow/svelte` dependency is unused but still in `package.json` (`dep is still in `).
