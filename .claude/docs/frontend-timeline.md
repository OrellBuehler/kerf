# Frontend: the timeline

The **timeline is a bespoke NLE timeline** that renders **real `editor.timeline`
state** (ruler + tracks + clips positioned by `timeline_start`/duration at `ui.zoom`
px/sec + playhead), with scene markers / silence regions / **beat ticks** (the tempo grid
of audio-track clips, confidence-gated, hidden when beats land closer than 4px — from
`src/lib/beats.ts`, the TS mirror of the Rust beat math that the ruler, the drag
snapping and the browser harness's alignment all share, unit-tested with `bun test`)
mapped from `AssetAnalysis` and
real audio waveforms (below); the razor tool splits, Delete removes, Shift+Delete
ripple-deletes, clicks select/seek, and (pointer tool) **clips drag to reposition** — free
positioning with gaps, snapping to clip edges / playhead / 0 / beats, and **dropping onto another
same-kind track** (`move_clip`, via pointer events + `data-lane` hit-testing) — and
**edge-drag to trim** (6px `ew-resize` handles; clamped to source handles, neighbors and
a 0.05s minimum; left edges commit `trim_clip` with `timeline_start` so the right edge
stays put; stills extend freely since they loop).
**Gestures are frame-quantized** (`src/lib/frames.ts`, bun-tested; keyframes stay in
seconds): a trim, move, drop, razor cut and fade length land on a frame of the cut's
rate — `editor.fps`, i.e. `timelineFps`, the first video clip's rate else 30, which is
`export_format`'s rule. Each rounds **once, from the raw pointer position** (a frame is
`k / fps` from an integer `k`, so equal frames are equal doubles and nothing drifts over
a long run of edits), the ghost and the commit use that one value, and a trim derives
every field from it (`trimEdit`). A magnet within reach (`ui.snap`: 0 / playhead / beats
/ clip edges) still wins, unrounded; frames are *not* a magnet and apply with snapping
off too. A landing within 1 µs of a neighbour's edge *is* that edge (`welds`):
`move_clip`'s overlap test is a strict float compare, and an edge computed as
`start + length / speed` can sit an ULP past its frame — so a tail butted against a
neighbour is placed by `startBefore` (the latest start whose `start + duration` does not
pass it, in the backend's own arithmetic; `tail - dur` is an ULP too high about as often
as not). Whether a press became a drag is judged on pointer travel (3 px), never on the
quantized position (one pixel is under a frame at high zoom), and an edge keeps the
offset it was grabbed at. A razor cut keeps half a frame
either side (`splitPoint`; a clip with no interior frame says so), the context menu's
split quantizes the playhead, and Escape / pointercancel / blur abandon a clip, edge or
title drag.
**Ripple, the selection set, group moves and zoom** are the timeline's editing layer, each
a pure bun-tested module under the component. *Ripple*: `editor.rippleMode` mirrors the
project flag — read in `load()` (so launch, New and Open) and again on the
`ripple-mode-changed` event an agent's `set_ripple_mode` emits (with a toast, since it is
the user's own toolbar setting that moved); the toolbar's **Ripple** toggle (`R`,
`aria-pressed`) is lit while on, with a second cue in the ruler corner and an accented
ruler underline, and its tooltip says each track ripples on its own but a moved clip takes its linked partners along.
All the rippling is the backend's, but the GUI shows it: with ripple on, an edge drag is
no longer stopped by its neighbours (it pushes them — only the source's footage, the
0.05 s minimum and 0 stop it; `ripple-trim.ts`'s `trimBounds`), and its ghost is the
*outcome* (`linked-trim.ts`'s `linkedTrimPreview`, the generalisation of
`ripple-trim.ts`'s single-lane `rippleTrimPreview` — a bun test holds the two equal with
links off: the trim applied to a scratch copy of the lanes, then `ripple.ts`'s `rippleFrom`,
sync lock included — see Linked A/V below), because a left-edge trim keeps the clip's start
rather than holding the right edge — one ghost per clip it moves, on whichever track, the
moved clips dimmed, red and inert if the backend would decline the ripple. The bounds are asked again at every move
and at the release, since the mode can flip mid-drag. `load()` reads the flag on every
load (so Open and New refresh it; `state-ripple.test.ts` pins that). *Selection* is a set: `selection.ts` holds every way of
changing `selectedClipIds` + the primary (`selectedClipId`, the clip the Inspector edits —
with several selected it shows an "N clips selected" note, since its sections act on that
one): a click replaces, Ctrl/Cmd toggles, Shift extends along the primary's track (adds
the clip when the primary is elsewhere), and a **marquee** (pointer tool; a drag from empty
lane, the titles lane or the space under the tracks) selects every clip its rectangle
touches — Shift adds, Ctrl/Cmd toggles — recomputed from the selection as the press found
it so the rectangle can shrink (`marqueeSelect`; `marquee.ts` tests the rectangle, in lane
space, against lane boxes measured from the DOM since the heights are CSS). Clips on a
locked track are not swept up (locking guards edits, and the selection is what every edit
acts on) but stay clickable. Escape mid-drag restores the selection; the click that ends (or follows an abandoned)
marquee is swallowed — held until it arrives or the next press, not on a timer — so it
does not seek and deselect; Escape otherwise clears (the page's
handler — the timeline's and the preview's abandon-a-gesture handlers run in the capture
phase and stop the event, so abandoning a drag never also clears). `#setTimeline` prunes
ids another edit removed. *Group move*: pressing a selected clip of several keeps them
(a click that never drags narrows to it), and dragging moves them all by the grabbed
clip's Δt — its start is the one snapped and frame-quantized, its group's own edges being
no magnet — plus one **lane offset applied within each kind's lanes** (`multi-move.ts`,
`planMove`). Its checks are `Timeline::move_clips`' (group as a group, before 0 refused not
clamped, locked or missing lane refused) and a property test replays random drags against
`multi-edit.ts`'s mirror, so the verdict drawn while dragging is the backend's: one ghost
per clip, red with the reason beside the pointer when refused (letting go then does
nothing), and a valid drop is ONE `editor.moveClips` — one revision, one undo. Delete is
`removeClips(ids)` (one revision; ripple follows the project's mode) and Shift+Delete
forces ripple; a clip on a locked track is left alone and stays selected, and Cut
(⌘/Ctrl+X) copies only what it can remove and says so when that is nothing (`ops.ts`
`deleteSelection` / `cutSelection`). *Zoom*
(`zoom.ts`): 0.05–2000 px/s, `ui.zoom` still px/s but stepped by ratio (+/-, buttons ×1.25)
and a logarithmic slider; ⌘/Ctrl + wheel is exponential in the delta (a pinch is smooth) and
holds the time under the pointer (`zoomAround`; the scroll is applied after the lane has
been rewidened); **⇧Z / the fit button** fits the cut (`ui.zoomToFit()` bumps `fitEpoch`,
since only the timeline knows its width). The ceiling comes down for a very long cut so the
lane stays under 8 M px — the one thing a browser cannot lay out. Nothing else assumed a
range: the waveform rung choice scales to any px/s (bottoming out at the engine's 2 ms
bucket, 4 px at the ceiling) and frame snapping works in seconds. `ruler.ts` makes the
label step follow the zoom and renders only the ticks in the visible window (hundreds,
not an hour's worth), with sub-second labels and, once a frame is 8 px wide, a mark per
frame.
**Roll, slip and slide** are three more tools beside Select and Razor (toolbar buttons;
`N` / `Y` / `U`; `Tool` is `'pointer' | 'razor' | TrimTool`) over `edit-modes.ts`.
`src/lib/trim-tools.ts` is everything a drag needs of them, pure and bun-tested;
`Timeline.svelte` is only pointer plumbing (`beginTrimTool` → `beginDrag`: capture,
Escape / cancel / blur abandon, **one** `editor.roll` / `slip` / `slide` on release).
*Roll* grabs the nearest cut within 8 px (`cutsOf` / `nearestCut` — two touching clips;
a press away from any cut is a click), lit on hover; the cut follows the pointer by the
offset it was grabbed at, snapped (playhead / beats / edges other than the pair's own) and
frame-rounded once. *Slip* is `slipDelta`: the content follows the pointer (drag right =
earlier footage = a negative backend `delta`, reversed clips included), rounded in
timeline frames then × speed, and the clip's filmstrip / waveform redraw from the slipped
window as you drag. *Slide* snaps the clip's start as a move does, minus the clips that
travel with it (`slideMembers`). The pointer is **held to the range** (`holdToRange` over
`rollRange` / `slipRange` / `slideRange`), not refused, and the ghost is the **outcome**:
`previewEdit` runs the mirror on a plain copy of the lane (`structuredClone` throws on
the editor's `$state` proxies) and returns the clips as they would stand. Ghost and
readout go amber when held at a limit (the reason beside the pointer) and red when the
backend would refuse (locked track, no longer a cut); release re-previews — the project
can move mid-drag — and toasts "Roll stopped at … — the incoming clip has no footage
left". `clamped` is judged client-side from that range, since the Tauri commands answer
with the timeline, not the `EditOutcome`. Meanwhile the Preview shows the **trim monitor**
(`ui.trimMonitor`, `TrimMonitor` / `TrimFrame`): `monitorFor` picks the frames either side
(a roll's outgoing last + incoming first, a slip's new in + out, a slide's two changed
neighbour edges; none on an audio track) and `getFrame` decodes them from the *source*,
single-flight, newest wins, and a request for the `{assetId, time}` already asked for is
skipped (a drag re-derives the cell on every pointer move; within one frame it is the same
picture) — the harness draws its stamped stand-in frames. A clip removed under a live
gesture (an agent's edit, an undo, Delete) abandons it (`subjectsPresent`, an effect in
`Timeline.svelte`): nothing is written and the ghost goes.
`ClipOverlays`' hit areas (`tooled`) are inert under any tool but Select; none of the
three ripples. **Trim start / end to playhead** (`Q` / `W`, the clip menu; `ops.ts` `trimSelection`) is `split_remove` on
every *selected* clip the playhead is inside (`planPlayheadTrim`: the razor's frame rule,
the 0.05 s floor as a sentence, locked tracks reported, one clip per track) as ONE
`split_remove_clips` — one revision, so a V1 clip and its A1 partner undo together —
following ripple mode (each track on its own), with a toast saying why when there is
nothing to cut; the clip menu's labels go plural (`Trim starts to playhead`) when several
clips are selected. The TS mirrors print numbers as the backend does: `format-fixed.ts`'s
`toFixedEven` rounds an exact binary tie to the even digit like Rust's `{:.N}` (JS's
`toFixed` takes the larger: 4.25 → `4.3` vs `4.2`), used by `formatTime` and the
refusals' `0.12s`.
**Linked A/V in the timeline** (`link-ui.ts`, `linked-trim.ts`, `multi-move.ts`, bun-tested; the chrome
is `Timeline.svelte`): a linked clip wears a **badge** — a chain, plus a muted speaker on a picture whose
sound was detached (`linkBadges`; the tooltip names the partners and says what Alt does) — and hovering
a clip outlines its partners. A **click selects the clip and its partners** (`clickSelectLinked`; Ctrl
toggles the pair, Shift brings in partners, a marquee sweeps them with the primary staying a clip it
touched); **Alt-click selects just the one**. **Alt is the escape hatch from links**, read live from the
pointer and the key (a "links off" chip lights in the toolbar): `link: false` on a drag, an edge trim, the
razor, roll / slip / slide, and the menu's *Remove only this clip*. A drag's plan is a `$derived` of the
pointer, the cut and Alt (`planMove` with `{links}`): dragged clips take the lane offset, their partners
are **carried by the same Δt on their own track** (`withLinkedMoves`' rule; the grabbed clip's own
partners are never lane-shifted even when selected) and checked with the group — a locked partner or
one that would land on a clip / before 0 turns the drop red with that said — and drawn as `carried`
ghosts; `moves` names only the clips dragged (the backend adds the rest; a property test replays random
linked drags against the mirror). An edge trim's bounds are the clip's narrowed by each sharing partner's
neighbours (`linkedTrimBounds`; a partner's footage is no limit — it is trimmed less); its ghost
(`linkedTrimPreview`) is `trim_clip` + `carry_extent_edit` + the per-lane ripple + `conformLinks` (the trimmed
clip its anchor, the trim itself the timeline "moved apart" is judged on) + the carried-lane check + the sync guard on a scratch copy, so a
clip ripple pushes shows the partner it drags along on its own lane (a J/L-cut offset kept), and a refusal — a
locked partner, a clip outside its group or a picture in the way, a linked clip it would cover, a clip left under 0.05 s —
is red with its reason; a sound it cuts back to make room is drawn too and named (`trimmed`, an amber hint — the
revision's label says it afterwards) (`api-links.test.ts` holds it equal to the harness commit).
Roll / slip / slide run `previewEdit(…, links)` over the `*Linked` edits and `*RangeLinked` clamps:
partners are `partner`-role ghosts with a `trackId`, the readout says `· with A1`. `gestureReason` adds
"or hold Alt to edit this clip on its own" to a refusal about linked clips. The clip menu and keymap share
`linkPlans` (`ops.ts`): **Detach audio** (⇧D; **one revision for the whole selection** via `detach_audio_clips`,
the toast's Undo takes it back and names a skipped clip), **Reattach audio** (⇧⌘D; shown only where something is
detached, greyed with the reason when unmuting would double the sound; **one revision for the whole
selection** via `reattach_audio_clips`, so one Undo), **Link** (⌘L) / **Unlink** (⇧⌘L), each
disabled with the backend's reason under its label (`MenuItem.reason`; `planLink` / `planUnlink` are the
validation halves of `linkClips` / `unlinkClips`). A detached picture plays none of its sound: no volume
line, no mixer strip when a video track's clips are all detached, and the Inspector's Volume / Audio
effects give way to a note saying where it plays (its fades stay — they are the picture's). Picture clips
never drew a waveform, so there is none to hide.
**Waveforms** are one `<canvas>` per audio clip covering only the on-screen part of it
plus overscan (`ClipWaveform.svelte`; a one-hour clip at 96 px/s is 345 600 px, which no
canvas holds). `waveform-view.ts` is the pure geometry: `sourceAt` maps clip pixels to
source seconds through `source_in`/`source_out`, speed and reverse (a reversed clip is
read through the mapping, mirrored, never flipped), the bucket width is the widest rung
(the backend's 2 / 10 / 40 / 100 ms levels, then doubling) within 1.5 device pixels
(DPR capped at 2), and what is fetched is fixed **tiles** of 2048 buckets aligned to the
*source* clock — so a scroll, a trim, or a split's two halves land on cached tiles.
`waveform-cache.ts` (injectable fetcher; the app's instance is `waveforms.ts`) caches by
asset + window + bucket count, joins in-flight requests, runs three at a time newest
interest first and drops queued tiles nobody wants any more, remembers a failed asset
(one notification, no re-request per scroll; held off 30 s, doubling per failure in a row
up to 10 min, cleared by a tile arriving), and is LRU-bounded. `want` tells its owner
(microtask) when everything it asked for was already cached or the asset is held off,
because nothing else would — a clip that looked a moment before another clip's request
landed the same tiles would otherwise stay unpainted. The draw waits until every tile it
needs is cached and until then leaves the old bitmap where it was, placed by clip-local
*time* so a zoom or scroll shows it stretched, not blank; a clip scrolled out of range
releases its tiles and shrinks its canvas to 1x1 (a canvas keeps its whole backing store
otherwise), and a redraw only assigns the canvas size when it changed.
`waveform-draw.ts` fills one polygon per lane (not a line per sample), scaled by
`effectiveGain` — clip volume through the track fader, as the export multiplies them —
and repaints columns at full scale (|peak| ≥ 0.999, or pushed there by gain) in `--danger`;
a canvas cannot read `var()`, so `readPalette` resolves `--waveform` / `--danger` once per
`settings.theme` change. A stereo clip gets two lanes when its clip is at least
`STEREO_MIN_HEIGHT` (48) px tall (`laneCount`, a function of pixels so the track-height
presets drive it) and folds to one below that; the default 64 px track is stereo.
`get_waveform` is no longer used by the timeline (the MCP tool keeps it).
**Filmstrips** are the video twin: one `<canvas>` per video clip over its on-screen part
(`ClipFilmstrip.svelte`; the waveform's windowing, DPR cap and size discipline), blitted from
the asset's `get_filmstrip` sheets. `filmstrip-view.ts` is the pure layout: a clip is a row of
**slots**, each the thumbnail's aspect at the clip's height in whole px, the grid anchored at
the clip's left edge (a scroll moves nothing on it); slot `i` shows the frame at the source
time under the middle of its *visible* part, through `waveform-view.ts`'s `sourceAt` (trim /
speed / reverse — a reversed clip's footage runs backwards, never flipped) and
`filmstrip-geometry.ts`'s `frameAt` / `locate`; slot edges are rounded as *edges*, so
neighbours never seam; a still is its one thumbnail repeated. `filmstrip-draw.ts` paints;
`filmstrip-cache.ts` (injectable fetcher + decoder; the app's instance is `filmstrips.ts`,
which decodes a sheet's `data:` URL through an `Image` then `createImageBitmap` — never
`fetch`, `connect-src` refuses `data:`) keeps one decoded strip per **asset** (a split's two
clips share it): one in-flight load per asset, two at a time newest interest first, queued
loads nobody wants dropped, a failed asset held off with `backoff.ts`'s doubling cooldown (the
waveform cache's rule; one warning toast), memory bounded **in bytes** (192 MB): LRU applies only to
assets no visible clip holds (`hold`/`want` register a clip as owner, `release` drops it), so
a held asset is never evicted and a working set over budget overshoots until it scrolls away
rather than thrashing; `prune` and `clear` notify the owners of what they drop. A clip box under 28 px shows none and never fetches;
until drawn the clip keeps its plain look, and once drawn its label moves to the foot on a
scrim backing so a bright frame cannot swallow it.
**Track heights** are three named presets (`track-heights.ts`): compact 32 / medium 64 (what
it always was) / large 112 px of lane. Everything else is a function of the clip box the lane
leaves, so nothing else knows about presets: compact (21 px) folds a stereo waveform to one
lane, shows no thumbnails or clip grab handles, and drops the header's mixer strip; large
(101 px) gives two readable lanes and near-native thumbnails. It is a viewer's choice, not
part of the cut, so it is **UI-only** (`ui.heights`, per track id in `localStorage`
`kerf.timeline.heights`; a `Track.height` field would be engine work for a per-viewer
convenience): `all` is the global choice (what "all tracks" last set, what a track with no
choice of its own is, and what the **titles lane** follows), a track set to it drops its
override, "all tracks" clears every exception, and the table is capped at 256. The toolbar's
three glyph buttons set all tracks (lit when every track agrees); a track header's name is its
menu (so is the header's right-click). Marquee hit-testing and lane `offsetTop` read the DOM,
so they follow the heights. The compact titles lane is 27 px (`MIN_TITLE_LANE_PX`: the 26 px
add button plus the lane's border).
The **minimap** (`Minimap.svelte` over the pure `minimap.ts`; toolbar toggle, remembered) is
the whole cut on a 36 px strip: a block per clip per track row (runs too fine to tell apart
merge, so blocks are bounded by the strip's width), the playhead, the in / out marks, and the
visible window as a box. The box and the timeline's `scrollLeft` / zoom are one thing seen two
ways (`windowRect` view -> box, `targetForRect` box -> view). Drag the body to scroll (the zoom
is kept *exactly*, not re-derived from a box widened to its 8 px minimum), drag an edge to
zoom (the other edge stays put, even after the zoom was clamped), press the bare strip to jump
there (and keep dragging), double-click to move the playhead. Gestures are absolute from the
press, Escape restores the view, and the timeline applies the result like a wheel zoom
(`pendingScroll`). `rowLayout` always fits the strip (the gap gives first, then 1 px rows,
then fractional rows), and the strip is `aria-hidden` on purpose: the timeline's own keys
(scroll, zoom, ⇧Z, J/K/L) are the accessible path.
**`ClipOverlays.svelte`** is everything on a clip beside its body: a **volume line**
(dB scale −36 dB…`MAX_GAIN` (+6 dB, `mixer.ts`) — one ceiling shared with the Inspector's
slider and the track fader, a clip set above it by an agent keeps its value and is drawn at
the top; the bottom edge is silence; a drag is relative to the clip's *real* level, so a
small drag moves a 6x clip from 6x instead of collapsing it, with a detent at exactly 0 dB,
since the export omits unity from the graph; double-click resets), **fade handles**
at the top corners (picture and sound both fade, so every clip has them; the volume line
is only for clips whose asset has audio; clamped to the clip and to each other,
double-click clears), the fade ramps, **keyframe diamonds** (clip-local seconds; click
seeks), and the **trim edges** with their halos. The top 14 px of a clip is the handles'
alone and the line's travel stays below it; the edge strips and the line's grab band do
not overlap; nothing is hit-testable until the clip is hovered or selected, nor under the
razor, nor on a locked track (keyframes still seek). Every gesture is `drag.ts`'s
`beginDrag` (pointer capture; Escape / cancel / lost capture / blur abandon), shows its
value live (the waveform follows the volume line) and writes **one** edit on release,
holding the live value until that edit settles. The ruler renders **in/out marks**
(`I`/`O` set at the playhead, `⇧I`/`⇧O` clear) that drive range export. Transport is
**J/K/L shuttle** (repeat taps double to ±8×) plus Space; playback is **audible**:
`src/lib/audio.ts` is a Web Audio engine that fetches clip PCM windows over `get_audio`
and schedules them with volume / fades / speed / reverse applied. **Per-clip effect
chains are auralized**: passing `clipId` to `get_audio` decodes the window through that
clip's own ffmpeg chain (`audio_effects_filter`, the same string the export renders), so
the chain is part of the buffer cache key and retuning an EQ re-fetches. It runs before
this engine's gain envelope where the export runs it after the clip gain — audible only
to a level-dependent effect, and keeping volume in Web Audio is what lets the fader stay
live instead of re-fetching PCM on every drag. Reverse shuttle is still silent. The
playhead follows the audio clock — edits mid-playback re-anchor via
`ui.resync()` from a `+page.svelte` effect. The timeline
toolbar's `+ V` / `+ A` add tracks and each track header has a `×` to remove one
(`add_track` / `remove_track`) and, on audio tracks, a **DUCK toggle**
(`set_track_duck`); the timeline is genuinely **multi-track**. Any track that can
actually be heard — an audio track, or a video track whose clips carry sound —
also gets a **mixer strip** (level fader + pan, double-click to return either to
neutral, tooltips in dB and L/R); a silent track gets none. `src/lib/mixer.ts` is
the *faithful* mirror of `Track::pan_gains`, because preview playback renders the
pan as the same balance the export does — a `StereoPannerNode`'s constant-power
law would quietly disagree with the file, and `get_audio` hands back mono, so the
two gain legs into a merger *are* the stereo pair. `src/lib/levels.ts` holds the master
bus's limits (the Rust constants), `levelNotes` (the *faithful* mirror of the advice
`Levels::new` writes) and `estimateLevels`, the browser harness's stand-in for `get_levels`
(an *approximation* from the sample analysis through faders, pan, master and limiter,
flagged `estimated`). The **Mixer panel** (`Mixer.svelte`, in the panel registry and the
Audio workspace preset, reachable from the Panels menu) is one vertical `MixerStrip` per
audible track plus a `MasterStrip`. Which tracks are audible is `mixer-strips.ts`'s
`trackHasSound`, which the track header uses too. It mirrors the export graph's
`clip_sounds`: `Clip.source_audio`, written only when false, marks a picture whose sound
was detached, and a video track made only of those has no strip. Each strip has a dB-tapered fader,
pan, M / S / Duck and a meter. The taper (`gainToFader` / `faderToGain` in `mixer.ts`:
unity at 0.75, floor −60 dB) is shared with the header's level slider, so a level sits
at the same place on both. `MixSlider` gives every fader and pan the same gesture, from
`slider-gesture.ts`: one edit per drag, written on release, and a run of arrow-key
nudges written once it goes quiet (or on Enter / blur). Escape abandons a gesture,
double-click resets, and Ctrl+Z during an unwritten run takes the run back rather than
undoing the edit before it.

The meters are **measured**. `audio.ts` routes each track through a bus gain and its
pan legs, then a stereo pair of `AnalyserNode`s, into a master gain. That feeds the
limiter, which is a `DynamicsCompressorNode` approximation (`limiterParams` in
`audio-mix.ts` trims its automatic makeup gain; the tooltip says it is an
approximation of the export's `alimiter`), then a master analyser. The fader moved
from the clip envelope (`clipGainAt`) onto the bus, which is the same product, so what
plays is unchanged. `meter.ts` holds the ballistics: peak with fall-off, smoothed RMS
and a held peak. The meters animate only while playing. Ducking is **export-only**:
Web Audio has no sidechain without an AudioWorklet, and the Duck toggle's tooltip says
the preview plays the track at its fader. **Measure** on the master strip calls
`get_levels` over the whole cut, or over in → out when both marks are set, and
`levels-view.ts` phrases the result. It is a whole-mix decode under the heavy-job lease
(minutes on a long cut), so while it runs the button is a **Stop** (`ui.stopMeasure()` →
`cancel_levels`, then `Stopping…` until the backend gives up): the pass rejects with
`levels cancelled` (`isLevelsCancelled`), which is quiet — no toast, the last result
stays. In the browser harness, `sample-audio.ts`
synthesizes a voice-like signal per asset at its analysed loudness, so playback,
meters and faders are drivable under `bun run dev`. The old
`@xyflow/svelte` `TimelineCanvas`/`clip-node` scaffold was removed (the
dep is still in `package.json`, now unused).
