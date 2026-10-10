# Kerf timeline and preview: UI/UX polish review

Scope: `frontend/src/lib/components/editor/` Timeline, ClipOverlays, ClipWaveform, ClipFilmstrip, Minimap, ContextMenu, Preview, TrimMonitor/TrimFrame, StatusBar; `zoom.ts`, `ruler.ts`, `timecode.ts`, `waveform-draw.ts`, `kerf-tokens.css`.
Method: Playwright against the running harness (dark and light; 1920x1080, 1366x768, 1100x700, 2560x1440; DSF 1 and 2; `?music=1`, `?staged=1`, `?gpusurface=1`), plus the source. Screenshots are in `/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/timeline/` (cited by file name below). Line numbers are for `Timeline.svelte` unless another file is named. Nothing in the repo was touched.
Caveat: the harness paints stand-in filmstrip frames (the amber squares and "0:04.0 #8" stamps), so I judged clip/filmstrip layout, not picture content.

## Verdict

The engineering underneath is far ahead of the finish on top. The gestures (linked A/V, ripple-aware ghosts, roll/slip/slide with a trim monitor, frame-quantized edits) are deeper than most first-generation NLEs. The surface does not yet read as a professional tool, for five reasons:

1. **The timeline looks unfinished.** The lane, the ruler and the row borders stop 48 px after the last clip, so 40-70% of a wide timeline is blank (H1). The ruler has labels but no tick marks, and the only visible ticks are beat ticks that collide with the labels (H10).
2. **Feedback gaps on the most-used gestures.** Plain edge trims, clip moves and razor cuts show no number, no snap indicator and no cut-line preview, although roll/slip/slide and fades do (H3-H5). Locked tracks look identical to unlocked ones (H2).
3. **The Preview is under-built.** The frame is capped at 720 px (H7). The scrub bar is invisible, 4 px tall and not draggable (H6). HUD text is burned onto the picture and is unreadable on light frames or in the light theme (M20).
4. **Inconsistent control language.** There are four different "on" styles, three control heights, 14 px zoom buttons, idle toggles drawn in the disabled colour at 3.4:1, and five different timecode formats (H8, H14, M9).
5. **A few outright mistakes.** The clip menu advertises `C` for "Split at playhead" when `C` selects the Razor tool and no split key exists (H11). A double separator in the canvas menu. The empty-cut length reads 00:08. Video tracks are stacked upside down relative to compositing order (H12).

Most of the fixes are small and local. Many come from one cause: `Timeline.svelte` is 2857 lines with essentially all styling inline, and each button re-implements its own look. Extracting a `ToolbarToggle`, a `TrackFlag` and a `ClipLabel` would remove roughly a third of the findings below by construction.

Counts: 15 High, 28 Medium, 14 Polish (57). Top-10 by impact per effort is at the end.

---

## HIGH

### H1. The lane, ruler and track rows end at the end of the cut; 40-70% of the timeline is dead space
- **What:** `laneWidth()` = `max(760, ceil(max(duration, 8) * 36) + 48)` (`zoom.ts:30-38`, default `ZOOM_DEFAULT = 36`, `zoom.ts:20`). At 1920 px the lane ends at x=1010 of 1920 (measured: lanes 786 px wide, `dom1.mjs`); the ruler background, grid lines and every track row border stop there, and the rest is flat panel colour. At 1366 the gap is 356 px, at 2560 about 1550 px. Zoomed far out the ruler visibly ends mid-panel (`z-out18.png`, ruler ends at x=985). The empty state suffers the same (`e-empty-full.png`). First impression at launch: `s1-dark-1920.png`.
- **Where:** lanes box `2354-2360` (`width:{contentW}px;flex:none`), ruler `2362-2368`, grid `2471-2477`, rows `2559`.
- **Fix:** give the lanes box `min-width:100%` of the scroller's visible lane area, so the ruler, row borders and grid continue to the viewport edge. Draw a subtle "past the end of the cut" tint plus a 1 px end-of-cut line. On the first load of a project with clips, fit the cut to the width (call `fitToWindow`) instead of a fixed 36 px/s, and re-fit while the user has not zoomed. Premiere, Resolve and FCP all fill the full width.

### H2. A locked track is invisible in the lane
- **What:** locking (`L`) only lights the header button and changes the cursor to `not-allowed` (`1075`). The clip style (`2591-2597`) has no locked branch. With V1 locked the clips look exactly as before (`st-headers.png`). A locked track that does not look locked is how people lose edits.
- **Fix:** hatch the lane (a `repeating-linear-gradient` of `--border-default`), dim clips to about 0.75, and show a padlock glyph in the header and at the lane's left edge. Resolve and Premiere both hatch or grey locked tracks.

### H3. Snapping has no indicator
- **What:** `snapPoint` and `snapStart` (`542-552`, `683-695`) compute the magnet, but nothing in the template shows which magnet won or that one is active. The ghost just jumps. Pointer-to-playhead, clip-edge, beat and bar snaps all feel like random stickiness (`i1-clip-drag.png`).
- **Fix:** have `quantizeTime` / `gridMagnets` return the winning magnet, and draw a 1 px full-height line in `--kerf-300` (white at 60%) at that time while the gesture is held, with a 10 px label ("Playhead", "Bar 12", "Edge V1"). Premiere draws a white magnet line, FCP a yellow one, Resolve highlights the edge.

### H4. A plain edge trim gives no numeric feedback and no frame in the Preview
- **What:** roll/slip/slide get a readout and a trim monitor (`2805-2818`, `tl-slip-drag-full.png`), fades get a readout (`ClipOverlays.svelte:490-503`). The most common gesture, the edge trim, shows only a dashed amber ghost (`2736-2744`, `i1-edge-drag.png`). No delta, no new duration, no timecode, and the trimmed-away part of the clip is not marked.
- **Fix:** reuse the readout component at the pointer: `-12 f · 00:07:18` for the delta and new length, and amber when held at a limit, as roll does. Dim the part being removed (or extend the clip's own edge live). Feed `ui.trimMonitor` with a one-cell monitor of the edge frame. Premiere shows the edge frame in the Program Monitor during trims; Resolve uses a dual-up viewer.

### H5. The Razor tool has no cut-line preview
- **What:** with `C` active the only feedback is the `crosshair` cursor (`1076-1078`, `tl-razor-hover.png`). There is no line at the pointer, no snap, no hint that linked partners will be cut too (`609-622`).
- **Fix:** draw a 1 px vertical line through the hovered clip (and its linked partners unless Alt) at `splitPoint(snapPoint(...))`. Shift cuts all tracks (add this). Premiere draws a vertical edit line across tracks, Resolve a red blade line.

### H6. The transport scrub bar is invisible, 4 px tall and not draggable
- **What:** `Preview.svelte:850-861`. The track is `--surface-inset` on `--surface-app` = 1.05:1 contrast, so only the dot reads (`c2-transport.png`). The click target is the 4 px strip (measured 766 x 4 at y=642.5). It uses `onclick`, not pointer events. Measured: dragging from x=700 to x=1000 leaves the timecode at `00:00:00` mid-drag and jumps to `00:33:05` on release. In/out marks and markers are not shown on it.
- **Fix:** a 16 px hit area wrapping the 4 px visual track; `--border-strong` or `--surface-hover` track colour; pointer capture with `beginDrag` for a live scrub; draw in/out range and marker ticks; show a hover time chip. Every NLE viewer scrubs live.

### H7. The Preview frame is capped at 720 px wide
- **What:** `frameBox = width:min(100%, 720px, calc(100cqh * aspect))` (`Preview.svelte:261`). At 1920x1080 the pane allows about 913x514 and the frame is 720x405 (62% of the available area); at 2560x1440 it is still 720x405 (about 30%, a postage stamp in a black pane, `p-2560.png`). Frames are also requested at 960 px (`Preview.svelte:120-121`), so removing the cap alone would upscale at DSF 2.
- **Fix:** drop the 720 px cap (keep `min(100%, 100cqh * aspect)`), and request `ceil(cssWidth * dpr)` bucketed to 480/960/1440/1920. Add a Fit / 50% / 100% select to the viewer. Premiere and Resolve fill the monitor.

### H8. Five timecode formats for one quantity, and the main one is ambiguous
- **What:** ruler `mm:ss` / `mm:ss.f` (`ruler.ts:57`); toolbar `mm:ss`, floored, no label (`120-124`, `1994`); transport and HUD `mm:ss:ff` (`timecode.ts:formatTimecode`, "non-drop, minutes:seconds:frames", which reads as HH:MM:SS); status bar `[h:]mm:ss` (`StatusBar.svelte:43-50`); Inspector `mm:ss.cc`. A 20.5 s cut is "00:20", "00:20", "00:20:15" and "00:20.50" at once; a 61-minute cut is `61:00` in the toolbar and `1:01:00` in the status bar.
- **Fix:** one project timecode, `HH:MM:SS:FF` (hours hidden only when 0 and the width is tight), from a single formatter used by the transport, HUD, toolbar and status bar. Keep `mm:ss.cc` only for editable seconds fields. Premiere, Resolve and FCP show HH:MM:SS:FF everywhere.

### H9. No current-time readout on the timeline, and the one number there is wrong for an empty cut
- **What:** the toolbar's `01:00` after Redo is the cut length, unlabeled, and is `max(editor.duration, 8)` (`99`, `1994`), so an empty cut shows `00:08` (`e-empty-full.png`) while the transport shows `00:00:00`. While scrubbing or dragging nothing says what time you are at unless the Preview panel is on screen.
- **Fix:** replace it with `TC current / TC total` (mono, 12 px, `--kerf-300` for current). Show a small TC chip on the playhead head while scrubbing. Use `editor.duration`, not the layout minimum, for the readout.

### H10. The ruler has no tick marks; labels float, and beat ticks collide with them
- **What:** labels sit at `left: t*pxPerSec + 4px; top: 7px` (`2370-2375`) with no tick of their own, so a label's time is by proximity only. With music loaded, the only ticks are cyan beat/bar ticks (`2387-2398`), and a bar tick (11 px tall from the bottom) overlaps the 10 px label text (7-17 px): visible at `00:00`, `00:10`, `00:20` in `c2-overview-ruler.png` and at `00:00.5`, `00:02.5` in `z-in8.png`. A reader takes beats for seconds. Frame ticks are 4 px at 45% opacity (`2376-2380`), too faint to see.
- **Fix:** draw major ticks (8-10 px) at every label and minor ticks (4 px) at 5 or 10 subdivisions; left-align the label just right of its tick. Move beat/bar ticks into their own 6 px strip under the labels, or draw them as faint lines in the lane. Hide any label that a marker chip or bar tick would overlap. Premiere and Resolve both use major and minor ticks.

### H11. The clip menu advertises the wrong shortcut for Split, and there is no split key at all
- **What:** "Split at playhead" shows `C` (`1388-1391`, `settings.shortcut('tool.razor')`). `C` selects the Razor tool (`+page.svelte:465-467`); no key splits at the playhead (`keymap.ts` has none). The item is also disabled unless the playhead is inside the clip, even though a right-click at a point is the obvious split position (`cm-audio-clip.png`).
- **Fix:** add `edit.split` (Premiere Ctrl+K, Resolve Ctrl+B) that cuts every selected clip (or all tracks) under the playhead, and show that key. Make the menu item "Split here" at the right-click time (quantized), falling back to the playhead.

### H12. Video tracks stack upside down relative to compositing
- **What:** later video tracks composite on top (kerf's own MCP documentation says so), yet V2 is drawn below V1 and `+ V` inserts it directly above the audio tracks (`2240`, `2545`; `tr-compact-crop.png`: V1, V2, A1, A2). Every mainstream NLE stacks video upward (V3, V2, V1, then A1, A2) with a heavy divider between video and audio. Putting B-roll "over the interview" currently means dragging it down.
- **Fix:** render video tracks in reverse order above the audio group, with a 2 px divider, and make `+ V` add at the top. This is display-only if `data-lane` hit-testing and `moveTracks` still use track ids. Keep a "Track order" option if it is felt to be risky.

### H13. Agent proposals have no diff on the timeline
- **What:** `--diff-add`, `--diff-remove` and `--diff-shift` exist and are used only by the Agent panel and the trim monitor labels. While previewing a staged proposal, the timeline swaps to the proposed cut under a banner but trimmed, moved and added clips look identical to untouched ones (`m-staged-preview-full.png`). The one thing that makes Kerf different, reviewing what an AI changed, is not drawn where you review it.
- **Fix:** while `editor.previewingStaged`, outline moved/trimmed clips in `--diff-shift`, added clips and titles in `--diff-add`, and draw removed ranges as hatched `--diff-remove` ghosts. Add `[` / `]` to jump between changes and a "4 changes" chip in the toolbar. Resolve's compare view and Premiere's Review mode do the same for revisions.

### H14. Idle toggles look disabled, and "on" looks the same for solo, lock, duck, ripple and snap
- **What:** S, L, DUCK, chevrons, the toolbar duration, the ruler labels and every status-bar string use `--text-disabled` (#5e6b7a on #0f1318 = 3.43:1, below 4.5:1 for 10-12 px text). A track's idle S/L reads as a disabled control. When on, S uses `--kerf-400`, L `--kerf-300`, DUCK `--kerf-500` (`2267-2272`, `144-146`), the same amber as selection, playhead and Ripple. Mute is the only distinguishable state (red).
- **Fix:** idle = `--text-muted` (5.04:1) or `--text-secondary`. Give each state its own cue: solo amber, lock a padlock icon with a neutral fill (not amber), mute red, duck agent-cyan or an outline-only dot. Replace the cryptic letter L with a padlock icon (Resolve and Premiere use a lock glyph).

### H15. No autoscroll while dragging near the viewport edge
- **What:** `grep` finds no autoscroll in `Timeline.svelte` or `drag.ts`. A clip drag, edge trim, marquee, title drag or roll cannot go past the visible window; only a ruler scrub scrolls, via the "keep playhead visible" effect, which jumps by 75% of the view (`1573-1581`). With the default 36 px/s a 5-minute cut is 10 800 px wide, so moving a clip across it means zoom out, drop, zoom in.
- **Fix:** an rAF loop in `beginDrag` that scrolls at a speed proportional to how far the pointer is into a 48 px edge band, applied to drag, trim, marquee, roll/slip/slide and scrub. Replace the playhead page-flip with smooth follow. All of Premiere, Resolve, FCP and CapCut autoscroll.

---

## MEDIUM

### M1. Clicking a clip moves the playhead
- **What:** an unmoved clip press seeks to the click point (`860-864`, locked path `606`), as does a click on a title (`1760`) and on empty lane (`1199-1208`, which also deselects). Premiere, Resolve, FCP and CapCut select without moving the playhead, so you can park the playhead, then pick a clip and press Q/W (trim to playhead). Here, selecting destroys that setup.
- **Fix:** make a plain clip click select only; seek on click only in the ruler or empty lane (and make empty-lane click not seek while deselecting). If the "click to seek" is a deliberate choice, make it a setting.

### M2. The playhead is not frame-quantized
- **What:** `ui.seek` accepts any float (`editor-ui.svelte.ts:463-467`). A ruler scrub lands mid-frame, and `frameStep` adds 1/fps to that fractional position (`+page.svelte:372-374`), so stepping preserves the sub-frame offset, while every gesture and the split quantize. The TC readout floors, so the displayed frame and the decoded picture can disagree on mixed-rate footage (24 fps clip in a 30 fps cut).
- **Fix:** round to `k / fps` in `seek` for scrubs and clicks; step from the rounded frame.

### M3. The playhead cap scrolls away on vertical scroll
- **What:** the playhead (`2848-2854`) is in the lanes box, not the sticky ruler. Once the tracks scroll vertically the line stays over the ruler but the triangle head is gone (`vs-scrolled.png`, line starts at the ruler with no head).
- **Fix:** render the head and a TC flag inside the sticky ruler (z above the markers).

### M4. A clip's name is not pinned
- **What:** the label is a flex child at the clip's left padding (`2645-2653`). Scroll so the start of a long clip is off-screen and the clip has no name (`sc2-scrolled.png`: music clip, no label, only the header says A2).
- **Fix:** offset the label by `max(0, scrollX - clipLeft)` (the component already has `scrollX` / `viewLo`). Resolve, Premiere and FCP pin the name.

### M5. Audio clip names sit on the waveform
- **What:** the audio label is at the foot with only a text-shadow (`2645-2653`), over the second stereo lane. White on the waveform mint (#6fcfa8) is 1.88:1 (`c2-clips-left.png`, A1 "interview.mp4"; `vs-scrolled.png`).
- **Fix:** use the same scrim chip as video labels, or reserve a 14 px name strip on top of any clip 48 px or taller and draw the waveform below it (Resolve and Premiere put the name in a header strip).

### M6. Clipping paints whole columns solid red
- **What:** `waveform-draw.ts:99-110` fills the entire polygon of a clipped column in `--danger`. On the fixture's loud bridge this is a 56 px x lane-height flat red rectangle that hides the waveform and reads as a rendering error (`c2-clips-left.png`, `vs-scrolled.png`). In a track of loud music it would be a red band across the clip.
- **Fix:** paint only the top and bottom 3-4 px of the lane (or a 2 px cap) in red and keep the waveform body mint. Resolve and Reaper show only peak tips.

### M7. The track header is heavy, wide and duplicates the Mixer
- **What:** 224 px fixed (`--track-header-w`), holding name+chevron, DUCK, mute, S, L, X, a fader and a pan, all in 26 px bordered buttons (`2241-2349`). The Remove-track X sits 6 px from L, one miss away from deleting a track (the toast has Undo). The same controls exist in the Mixer panel. Premiere uses about 140 px, Resolve about 180.
- **Fix:** default 168 px, drag-resizable. Mute, solo and lock as icon-only toggles in a joined group; Remove only in the context menu; fader and pan appear on hover or at Large height (or open from a popover).

### M8. The track name is a hidden height menu, and tracks can't be renamed
- **What:** clicking `V1 v` opens the height menu (`2248-2255`), which the toolbar and context menu already offer. There is no rename and no colour. The 10 px chevron is `--text-disabled`.
- **Fix:** double-click renames; a drag handle on the header's bottom border resizes the height (Premiere and Resolve both do); drop the chevron.

### M9. The toolbar has four "on" styles, three control heights and 14 px targets
- **What:**
  - "On" looks: snap = `--surface-hover` + kerf-300 text (`2076-2078`); Ripple, heights, minimap and delivery = kerf-400 border + `--selection-fill` (`2090`, `2107`, `2122`); tool buttons = `--surface-active` + kerf-400 icon (`1931-1935`); Fade = `--surface-hover` (`2020`).
  - Heights, measured: tool buttons 26x24, text pills 21 px, zoom buttons 14x14 (`measure1.mjs`), slider 24, with 4 px and 3 px radii mixed.
  - `snap on` / `snap off` swaps text and so changes width.
  - Zoom icon buttons have no chrome or hover and only a 14 px target.
- **Fix:** one `ToolbarToggle` (24 px, `--radius-sm`, one on/off look), snap as a magnet icon with an aria-pressed state, zoom buttons 24 px hit areas, separators between Tools | Edit modes | View | Add.

### M10. Contextual controls are inserted into the middle of the toolbar
- **What:** with a selection, `N selected`, `FADE [in] [out]` and the `links off` chip appear beside the cryptic length (`1995-2034`, `i1-select-c2-full.png`). Fades already live on the clip handles, the Inspector and the clip menu: four places.
- **Fix:** keep only the `links off` chip; move "N selected" to the status bar; drop the toolbar Fade buttons.

### M11. The overview strip costs 37 px even when it shows nothing
- **What:** at the default zoom the window box spans the whole strip (`n-1366-full.png`), so it adds nothing. At 1100x700 the timeline shows V1 and half of A1 (`n-1100-full.png`).
- **Fix:** auto-hide the strip when the view covers at least 98% of the cut, or default it off and show it when zoomed. Premiere has none; Resolve's is optional.

### M12. The timeline's vertical budget is small by default
- **What:** fixed chrome inside the timeline: dock tab 32 + toolbar 34 + overview 37 + ruler 26 + titles lane 30 = 159 px. At 1920x1080 the track scroller is 285 px (three tracks exactly, `measure1.mjs`); at 1366x768 only V1 and half of A1 are visible (`s1-dark-1366.png`) while the Preview gets 387 px.
- **Fix:** give the Edit preset about 45% to the timeline; collapse the empty Titles lane to a 12 px strip that expands on hover or add; merge the toolbar and the dock tab row where possible.

### M13. Markers: AI colour, and the chip lands on the label row
- **What:** the default marker colour is `--agent-400` cyan (`2402`), the colour reserved for AI/analysis. The chip is vertically centred in the 26 px ruler (`2409-2432`) at z 29, over the tick labels (`st-marks-titles-full.png`: "Marker 1" crowds `00:05`). Marker names are 9 px.
- **Fix:** default to a neutral/blue marker colour (`--green-400` or a new `--marker`); put the chip in its own row below the labels; 11 px names.

### M14. The in/out range tints only the ruler
- **What:** `2434-2439` fills the 26 px ruler. The lanes below are untouched, so the range is easy to lose, especially after zoom or scroll (`st-marks-titles.png`).
- **Fix:** also scrim the lanes outside the range (or tint inside it), and show a `range 00:00:06:00` chip. Premiere shades the work area; Resolve highlights it across tracks.

### M15. Clip drag shows a ghost but no number and no target-lane cue
- **What:** moving a clip gives a dashed outline only (`2703-2719`, `i1-clip-drag.png`), with no delta or time, no highlight of valid target lanes and no dimming of wrong-kind lanes. The ghost fill is `--drag-ghost` #788cff, an off-palette periwinkle that is also the transition ramp colour.
- **Fix:** a pointer readout `+1:12 · V1` (amber/red as roll's readout is), a `--selection-fill` tint on the lane under the pointer, and a ghost colour from the kerf amber ramp.

### M16. The context menu has no keyboard model
- **What:** `ContextMenu.svelte:64-69` handles only Escape. Opening on a clip leaves focus on the clip, `ArrowDown` does nothing, and `Tab` enters the menu at the first enabled item (`menukb.mjs`). Hover is set by inline JS, there are no submenus (Analyze opens a second menu at the pointer), no typeahead.
- **Fix:** focus the first enabled item on open, roving `ArrowUp/Down/Home/End`, `Enter` runs, `ArrowRight` opens a submenu, `Escape` returns focus to the invoker.

### M17. The clip menu is 21 items tall; the canvas and track menus have inconsistencies
- **What:**
  - The audio-clip menu is 604 px, and the multi-select video menu 710 px (`i1-ctx-clip-full.png`): 65% of a 1080 screen, scrolling at 768, covering the clip you clicked. It stacks four red trash rows (Remove, Ripple delete, Remove only this clip, Ripple delete only this clip) all with the same icon.
  - The canvas menu has two consecutive separators (`1879-1880`; `cm-canvas.png`, a tall double rule between Add audio track and Paste).
  - The track menu says "Mute track" with an eye-off icon on an audio track (`1332-1334`) where the header shows a speaker; Solo and the three height rows have no icon, so labels are not aligned in a column (`cm-track.png`).
- **Fix:** group the Link/Analyze/Fit/Stems entries into submenus; fold "only this clip" into a hint on Delete (Alt) or a submenu; delete the duplicate separator; pick icons by track kind; give every row an icon or none.

### M18. Clips show no keyboard focus ring and cannot be operated by keyboard
- **What:** the clip is a `<button>`, but the inline `outline:{lit ? ... : 'none'}` (`2597`) beats the global `button:focus-visible` rule (`layout.css:143-145`). Measured: `:focus-visible` matches and the outline is `none`. Enter or the arrow keys do nothing on a focused clip.
- **Fix:** move the outline to a class (`.kclip:focus-visible`), and give Enter/Space "select" and arrow keys "nudge/seek to edge".

### M19. Preview controls are minimal
- **What:** the transport has only skip-back, play, skip-forward (`Preview.svelte:816-871`). No frame step (ArrowLeft/Right exist as keys only), no loop, no mark in/out, no volume, no zoom/fit, no overlays toggle; J/K/L are invisible. The comment even says "J / K / L stay on the keyboard".
- **Fix:** `⏮ ◀| ▶ |▶ ⏭` plus loop, a `Fit ▾` select, an overlays (`⊞`) menu, and volume/mute; in Premiere and Resolve all of these are on the viewer.

### M20. HUD text is burned onto the picture and fails on light frames
- **What:** the clip-name badge, `res · fps` and the timecode are always painted over the frame (`789-808`). The 55% white resolution text has no scrim; in the light theme the placeholder frame makes it invisible (`l-full.png`) and a real bright shot (sky, snow) does the same. The timecode is a duplicate of the transport's (`807` vs `846-848`) in two different tokens (`--kerf-200` vs `--kerf-300`). The fps shows `30.000` here and `30 fps` in the bar.
- **Fix:** remove the on-picture timecode; move name/res to an optional header strip above the frame or hover-only; if kept on the picture, put them on a `--scrim` chip (as the Badge does) and use one token and one fps format.

### M21. Safe-area guides are narrow and unlabeled
- **What:** shown only for vertical/square deliveries (`268`, `aspect < 1.2`), off by default, no on-canvas toggle (Settings, View menu, or the Preview context menu); nothing for 16:9 (no action/title safe, no centre cross, no thirds). The regions carry no labels, so the 8/20/14% areas read as arbitrary dashed boxes (`m-916-full.png`).
- **Fix:** an Overlays menu in the viewer (Platform UI, Action safe 93%, Title safe 90%, Thirds, Centre). Label each region at 9-10 px, 60% opacity. Resolve and Premiere both label safe areas.

### M22. Title drag has no snapping or guides, and the centre can leave the frame
- **What:** `dragPosition` clamps the centre to 0..1 (`titles.ts:57-68`) and nothing snaps to the centre axes or safe margins; no guides are drawn while dragging (`Preview.svelte:583-597`). A title can sit half outside the frame.
- **Fix:** snap within 6 px to centre lines and safe margins, draw cyan guide lines, and clamp the box edges rather than the centre (Premiere, Resolve and Canva all show smart guides).

### M23. Every visible title keeps a permanent dashed outline
- **What:** `768` outlines each title at the playhead with a dashed line even when unselected. A caption-heavy cut (word-punch) will be a field of boxes over the picture.
- **Fix:** show the outline on hover or when selected; keep a faint dot at the title origin otherwise.

### M24. The status bar mixes meanings and carries a dev line
- **What:** the left text is the metadata of the selected asset followed, with no label, by the cut duration (`30.000 fps · 1920×1080 · h264  00:20`, `StatusBar.svelte:56-61`), so it reads as the asset's length and changes when you select broll. The renderer line (`GPU 1920×1080 · 14 ms · llvmpipe`, `79-89`) is a developer diagnostic in amber (`--kerf-300`) at 10 px. Everything is `--text-disabled` (3.43:1).
- **Fix:** label the duration (`Cut 00:20:15`); fold renderer and ms into a tooltip on a small `GPU`/`Proxy` chip (neutral, amber only when degraded); text 11 px `--text-muted`.

### M25. Empty states are unhelpful
- **What:** the Timeline message "Timeline empty — import media and queue a cut" (`2479-2485`) is centred in the lanes box, so it lands on A1's border (`e-empty-full.png`), uses agent jargon ("queue a cut") and offers no action. The Preview says "No media loaded" with no button or drop hint (`701-704`). Both toolbars still act as if there were media (the length reads 00:08).
- **Fix:** a dashed drop zone across the video lane: "Drop media here, or press ⌘I", with an Import button; the same in the Preview.

### M26. Toasts cover the timeline's lower-right corner
- **What:** "Marker added", "3 clips removed" and "Cutting for 9:16" sit over the A2/A3 lanes and the scroll area for several seconds (`st-marks-titles-full.png`, `e-empty-full.png`, `m-916-full.png`), hiding clips and the horizontal scrollbar.
- **Fix:** dock toasts above the status bar at the left of the preview, or top-centre; informational toasts 2 s, Undo toasts 5 s.

### M27. The "Track background" theme token is dead, and rows have no body fill
- **What:** `--track-bg` is exposed in the theme editor (`theme.ts:78`, `kerf-tokens.css:121`) and used nowhere in components (grep). The lane rows have no background, so with a short clip the row is indistinguishable from the empty panel, and rows only show as 1 px borders at 6% alpha (`--border-subtle`). In light theme the same.
- **Fix:** paint each lane with `--track-bg` (alternating +2% for every second row, as Resolve does), or remove the token.

### M28. DUCK looks live in the preview but is export-only
- **What:** the DUCK toggle lights the same as S/L but ducking is not applied in the preview (the tooltip says so, `2262-2264`), so what you hear is not what is rendered.
- **Fix:** a small "export" superscript or a different state glyph when on, and (when possible) draw the ducking envelope on the clip.

---

## POLISH (little details)

### P1. Type is too small in too many places
Clip labels, ruler labels, toolbar pills and tabs 10 px; speed badge and marker names 9 px (`2658`, `2428`); status bar 10 px. At DSF 1 on a 1080p laptop this is below comfortable reading. Resolve uses 11-12 px for timeline text; set a 11 px floor and add a UI scale setting.

### P2. Narrow clips show label fragments
At 20-30 px wide the label is clipped to a single letter ("t" on V1, "m" on A2; `z-out18.png`). Hide the label (and its scrim) below about 40 px and show the name in a tooltip.

### P3. The playhead glow tints the footage
`box-shadow: 0 0 10px 1px var(--playhead-glow)` (`2849`) is drawn over the clips and washes the left edge of the filmstrip (`c2-headers.png`, orange smear to the right of the line). Use a 1 px darker halo on the left and right, not a blur, or draw the glow only in the ruler.

### P4. Trim-tool glyphs are indistinguishable
Roll (`separator-vertical`), Slip (`gallery-horizontal`) and Slide (`arrow-left-right`) are the same weight and read as the same idea (`c2-toolbar-left.png`). Use the Premiere-style metaphors (roll: ◀▶ with a centre bar; slip: a filmstrip with ←→; slide: a clip with ←→), and show the tool name after the icon when the toolbar has room.

### P5. Native `title` tooltips everywhere
Every control relies on the browser tooltip, so there is a 1+ s delay, no key-cap styling and no rich content (e.g. the Ripple tooltip is a paragraph). `components/ui/tooltip` exists. Use a 300 ms tooltip with a `<kbd>` shortcut.

### P6. The delivery button changes width and shifts the right cluster
Picking 9:16 changes `Source` (67 px) to `9:16` (about 55 px) and moves the zoom slider and snap button 12 px (`m-916-full.png`). Fix the width, or put the frame in a labelled field `Frame: Source`; "Source" alone doesn't tell that it is the delivery aspect.

### P7. The Preview frame doesn't clip its picture to its radius, and black bars merge with the pane
The frame has `border-radius:4px` but no `overflow:hidden` (`Preview.svelte:709`), so image corners poke through the rounded border. `--frame-matte` (#000) on `--surface-void` (#0a0c0f) is 1.05:1, so letterbox and pillarbox bars are indistinguishable from the pane; only the 10% border marks the frame. Use `--surface-app` for the pane or a 20% border, and `overflow:hidden`.

### P8. The name badge can collide with the resolution text
Badge has no `max-width`, so in a narrow 9:16 frame a long filename runs into `1080×1920 · 30.000` (`m-916-preview.png`, 40 px apart already with a short name). Add `max-width: 55%` with ellipsis.

### P9. Playhead pointer and ruler cursor
The ruler uses `cursor:ew-resize` (`2368`) for scrubbing; a pointer (or the playhead's own cursor) is the convention. Over a clip body the default cursor is `grab` (`1086`), which promises a drag that is not always available (locked, razor).

### P10. Duplicate text in the slip monitor
The trim monitor title (`Slip +33 f · +1.10 s · in 00:01:03 → out 00:09:03`) and the pointer readout repeat the same sentence (`tl-slip-drag-full.png`). Keep it in the monitor and keep only the delta at the pointer.

### P11. Fader and pan sliders carry no reference marks
No unity tick on the level slider, no centre tick on the pan, no dB readout while dragging, and both thumbs are 14 px circles that dominate the 64 px header (`c2-headers.png`). Add a 0 dB tick and centre notch, and show the value in the thumb tooltip while dragging.

### P12. The cut end is not marked, and the selection ranks are subtle
There is no end-of-cut line or flag (see H1). The primary selected clip is `kerf-500` and the rest `kerf-600` in a 1 px shadow (`2593-2596`), a difference that is hard to see; make the primary 2 px with a corner pip.

### P13. The Overview header column wastes 224 px
`OVERVIEW` (`2169-2173`) sits over the track header width with nothing else in it. Put the zoom/fit controls there when the strip is shown.

### P14. Idle cursor over a locked clip's edge, and frame ticks
`.edge.locked` is `not-allowed` but the halo is disabled with no other hint. The frame ticks (4 px at 45%, `2376-2380`) at 8+ px per frame are invisible; raise to 6 px at 70%.

---

## Screenshots referenced

`s1-dark-1920`, `s1-dark-1366`, `s1-music-1920`, `c2-toolbar-left/right`, `c2-overview-ruler`, `c2-headers`, `c2-clips-left`, `c2-transport`, `c2-preview-frame`, `c2-statusbar`, `i1-select-c2(-full)`, `i1-hover-c2`, `i1-hover-edge`, `i1-edge-drag`, `i1-clip-drag`, `i1-ctx-clip-full`, `z-in8`, `z-in20`, `z-out18`, `z-fit`, `st-marks-titles(-full)`, `st-headers`, `l-full`, `l-select-crop`, `n-1100-full`, `n-1366-full`, `tr-5tracks`, `tr-compact-crop`, `tr-large`, `tl-razor-hover`, `tl-roll-drag-full`, `tl-slip-drag-full`, `m-staged-full`, `m-staged-preview-full`, `m-916-full`, `m-916-preview`, `p-2560`, `e-empty-full`, `cm-canvas`, `cm-track`, `cm-audio-clip`, `vs-scrolled`, `sc2-scrolled`, `f-ripple`.

---

## Top 10 by impact per effort

1. **H1: fill the viewport.** `min-width:100%` on the lanes box (and fit-on-open). One line of CSS plus a call to the existing `fitToWindow`. Biggest single change to how finished the timeline looks.
2. **H6: the scrub bar.** Visible track colour, a 16 px hit area and pointer-captured drag via `beginDrag`. Small change that fixes an actually broken control.
3. **H7: Preview size.** Delete the 720 px cap and request the frame at the CSS width x DPR. A one-line cap plus a width calculation; it more than doubles the picture area on a large monitor.
4. **H11 + M17: menu correctness.** Fix the false `C` shortcut and add a real split key; remove the double separator; mute icon by kind; trim the delete variants. Mostly string and item edits.
5. **H2: show locked tracks.** A hatch background and padlock in lane and header. A few lines in the clip style and header.
6. **H10: ruler ticks.** Major and minor ticks; move beat/bar ticks off the label row. Pure markup in the ruler block (`2362-2398`).
7. **H8 + H9: one timecode.** One formatter with hours, a `current / total` readout in the toolbar, and the empty-cut `00:08` fix. Removes five formats and gives the timeline its missing clock.
8. **H14 + M9: control language.** Idle toggles at `--text-muted`, distinct on-states per toggle, padlock icon, a single `ToolbarToggle` (24 px, 24 px zoom targets). One small component, applied about 25 times.
9. **H3 + H5 + H4: gesture feedback.** A snap line (return the magnet from `quantizeTime`), a razor cut line, and a trim readout reusing the existing roll readout component. Medium effort, and these make editing feel precise.
10. **M20 + M11/M12: reclaim and clean up space.** Move the on-picture timecode out and scrim the HUD, auto-hide the overview strip when the whole cut is visible, and collapse the empty Titles lane. Small changes that give back about 70 px of timeline height and fix the light-theme HUD.

Strategic items that need design decisions rather than quick fixes: H12 (video track order), H13 (diff overlays for agent proposals), H15 (autoscroll), M7 (header density and rename).
