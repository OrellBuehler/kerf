# frontend — brief

Rules only. The full file `.claude/docs/frontend.md` is authoritative and holds the measurements and the story behind each rule: grep it for the phrase in parentheses.

`frontend/` is the SvelteKit / Svelte 5 runes SPA that Tauri embeds (Bun, Tailwind 4, dockview). Code lives in
`frontend/src/lib/` (`api.ts` bridge + harness, runes singletons `state.svelte.ts` / `editor-ui.svelte.ts` /
`settings.svelte.ts` / `workspace.svelte.ts` / `agent.svelte.ts` / `notifications.svelte.ts` / `updater.svelte.ts`, pure
bun-tested modules beside them), `src/lib/components/editor/`, `src/routes/+page.svelte`, `src/routes/layout.css`,
`styles/controls.css`, `styles/kerf-tokens.css`, `styles/dockview-kerf.css`.

## Map of the full file (grep these)

- Opening: build quirks, fonts, theme / tokens / shape, density and type, the theme library, the editor,
  files, the two guards, `upgradeStoredTheme`
- Dockview workspaces: `layout.ts`, `Settings.workspaces`, persistence, adopt, reset, resize
- `The **library**` rail, `One place per action`, folding
- `Detached panels` (popout), then title bar / menu bar / `Where the old toolbar's controls went`
- Deliver panel, Inspector (grade, blend, behaviours, stabilize, keyframes, dope sheet, easing, graph view)
- `Speed ramps`, `What the engine could always do`, `A control that is busy keeps focus`,
  `The Inspector's finish`
- Asset Inspector, `Asset tags`, `Polish presets`, controls.css / veil / focus / toasts, titles lane, Preview
  title boxes
- `Transform handles in the Preview`, `Motion presets`, `Live behaviours`, `The motion path`
- `The delivery frame has a frame rate`, delivery picker, export dialog, platform check, mirrors, caption
  import, thumbnails, markers
- Preview streaming: `Its frames are raw JPEG bytes`, double buffer, gate, ladder; `ExportDialog`, transcript,
  agent panel
- Review card / preview read-only / `holdForPreview`; `Editor context`; `PreviewEmpty`; `Z-order`
- `The Preview frame and its scrub bar`, `The GPU preview in the page`, `Drag frames (A3`,
  `GPU playback in the page`
- `Shared components` (`Modal`, `SliderRow`, chips, tips, menus), `Modals are modal`, notification log
- `Keyboard shortcuts are an action registry`, `Settings` dialog, media gone / partial accept / paging /
  mirrored refusals
- Update flow, harness `?flags`, `api.ts` mirrors, `types.ts` check, music / SFX bridges, compose dialog
- State singletons, bin rows, stems, file drop, `Proxy only in the preview`, SVG titles / templates / titles
  on tracks
- `## Scopes panel`, `## Sound FX panel`, `## Proxy queue panel`

## Build, fonts, theme, guards

- No `svelte.config.js`: adapter and compiler options sit inline in `vite.config.ts`; `+layout.ts` sets
  `ssr = false` and `prerender = true`; dev port is pinned to 1420 for Tauri
  (`adapter and compiler options live inline in`).
- Fonts are self-hosted via `@fontsource-variable/*` imported first in `layout.css` (Geist, Geist Mono, Space
  Grotesk, plus Inter and JetBrains Mono for the themes that pick them, `theme-fonts.ts`); nothing is fetched at
  first paint and the CSP has no font host; a `@import url(...)` after the Tailwind imports is ignored by CSS
  (`so nothing is fetched at first paint`).
- A new font licence notice file goes in three places: `bundle.resources` of `tauri.conf.json` and the `resources`
  override of both `pr-build.yml` and `release.yml`; `bundle-resources.test.ts` holds them equal; a new face in
  the existing `fonts-NOTICE.md` needs only its line there (`a new notice file must go in all three`).
- Every colour is themable: tokens live in `theme.ts`; `applyTheme` writes them as inline properties on
  `<html>`; every translucent token is a `color-mix` of an opaque base so a theme is a flat list of hex
  colours; editor components carry no colour literals (`of an opaque base so a`).
- A theme also carries a shape (`SHAPE_TOKENS`, border weights, shadow strength); `parseTheme` reads field by
  field (a colour exactly `#rrggbb` or the whole theme is refused, names cleaned, numbers clamped, choices
  known), and fills what a stored theme lacks from the preset whose colours it exactly is, else the scheme's
  (`numbers clamped and snapped to their range by`).
- Corner roundness (`radius-scale`, multiplies `--radius-*`), density (whole-pixel control and bar heights,
  never a text size, never under 24 px), the two typefaces and the review colours `diff-*` are part of a theme;
  `type-guard.test.ts` and `theme.test.ts` hold the type steps and the controls at every density
  (`Shape, density and type are part of a theme`).
- Every `input[type=range]` is styled once in `routes/layout.css` from the shape variables; `--slider-accent`
  is the one per-slider choice (`is styled once in`).
- `app.html` paints Kerf Dark before hydration and is pinned to Kerf Dark `surface-app`, as is the window
  `backgroundColor` (`paints Kerf Dark before hydration`).
- The contrast pair list lives in `theme-contrast.ts` (the guard and the editor read the same one); the pair
  list is never edited to pass (`and its reasoning live in`).
- Two guards in `theme-guard.test.ts`: no colour literal in `src/` outside `theme.ts`, `kerf-tokens.css`,
  `app.html` and the two harness image generators; and every preset must meet WCAG contrast for the pairs the
  UI draws (4.5:1 reading text and labels on fills, 3:1 muted-on-hover / strokes that carry meaning, High
  contrast 7:1 on reading text, tinted surfaces, hover / pressed shades, focus outline, High contrast border
  weights) (`Two bun guards keep this true`).
- A failing preset gets its values fixed, never the pair list (`a failing preset gets its`).
- `--text-disabled` is for what is off only (menu disabled rows, `controls.css`, named exceptions in
  `theme-guard.test.ts`); text people read is `text-muted` (`is for what is off`).
- `type-guard.test.ts`: a `font-size` is a `var(--text-*)` or one of the literal steps 10 / 11 / 12 / 13 / 14
  / 16 / 20 / 26 / 34 / 46 px; nothing under the 10 px floor; every `var(--type-*)` / `var(--text-*)` must be
  defined (`nothing under the 10 px floor`).
- A stored theme is a copy: `upgradeStoredTheme` (on what settings reads back, never on import) moves a theme
  whose colours are exactly a `SUPERSEDED_KERF_LIGHT` set to the current preset; when Light's colours change
  again, the outgoing set goes on that list (`A stored theme is a copy`).
- Themes are a library: presets read-only, the user's own (`UserTheme`, `base`) in a `ThemeBook` with a
  light / dark pairing, at most 40; stored in the one opaque `Settings.theme` as the theme in force (flat, as
  0.31 wrote it) plus `library`; a flat theme with no `library` is 0.31's and migrates on read, writing nothing
  until the first change; a `library` of a newer version is written back untouched; read once, written one at
  a time, newest wins (`Themes are a library`).
- Following the system is `prefers-color-scheme` (`matchMedia`, live) with one theme per scheme; while it is on
  a pick, an import and a saved copy assign their own scheme's slot and a toast says which slot, what it
  replaced and when the page changes; turning it off keeps the theme on screen; the list's arrows only move
  focus, Space / Enter pick; a slotted theme saved with the other scheme gives the slot back at save time
  (`Following the system`).
- The editor (`settings.themes`, `theme-session.ts`) is a 480 px strip with nothing dimmed: from 1280 px up the
  app shell is made narrower by it (`--shell-right`; the dock relayouts through the workspace's resize path,
  which writes nothing), below that it overlays; it is drawn in the theme the sitting began with, which needs
  the derived tokens and type styles declared on `[data-theme-scope]` too; it edits a draft that is applied
  live and written only by Save; Cancel and Escape put back (asking when dirty); undo steps are coalesced by
  kind; contrast findings warn and never block (`drawn in the theme the sitting began with`; `Docked, not floating`).
- A built-in is opened for editing as a copy that joins the library, and is used, only when saved and only if
  it changed; the row's text button is the one way into the editor (`opens a copy that joins`).
- A theme file is `themeFields` JSON `version: 1`, never an id; `readThemeFile` caps the text, parses with
  `parseTheme` and says what is wrong; an import is added under a free name, used and not upgraded
  (`the export is`).
- The desktop window stays hidden until the theme and its faces are applied, so no flash of Kerf Dark;
  `app.html` stays Kerf Dark for the harness; the OS title bar is not themed because `setTheme` would move
  `prefers-color-scheme` (`painting Kerf Dark for the browser harness`).

## Dock, workspaces, layout persistence

- Editor UI follows the Kerf design system under `components/editor/`; chrome is `TitleBar` + `StatusBar`
  around the dockview `Workspace.svelte`; eleven panels, each a Svelte component mounted into a dockview
  content element; `--dv-*` mapped to Kerf tokens in `dockview-kerf.css` (`into a dockview content element`).
- Workspaces Edit / Color / Audio / Motion / Deliver (and the user's own, below) are `aria-pressed` toggle buttons in the title bar (no
  tabpanel); a pointer click blurs the button, because a focused button swallows Space and the transport
  shortcut would stop (`a pointer click blurs the button`).
- `layout.ts` is the pure, bun-tested side: panel registry (titles, minimums), `PRESET_LAYOUTS` (a row of
  panels over a full-width timeline; the agent is a tab beside the inspector or deliver panel in every preset)
  and `sanitizeLayout` (known ids, each shown once, titles / minimums re-taken, floating groups dropped, else
  `null` so the preset is used) (`which turns a stored`). It migrates an old `media` / `bin` / `transcript`
  layout into `library` (`the first of`).
- Stored as `Settings.workspaces` `{active, layouts, offered, library, custom, order}` (opaque JSON on the Rust side),
  parsed field by field so one bad layout costs only that workspace; the old `layout` becomes Edit only when there is no `workspaces` value at
  all (`one bad layout costs that workspace its arrangement`).
- Floating groups stay off (`disableFloatingGroups`, `sanitizeLayout` drops them): a floating Preview over another panel would show
  that panel's opaque background in the GPU `window` technique's transparent hole (the surface is seen through the page), and a layout
  cannot hold them yet (`Floating groups stay off`).
- User workspaces: a workspace is a *key* (five ids, or `w-xxxxxxxx` of a saved one); `custom` (<= 5, names <= 18 chars with zero-width and bidi characters stripped, unique
  against every workspace on input and on read, where a clash is renamed `Name 2`, not dropped) and `order` (the buttons) are parsed field by field, a bad entry costs only itself, a saved one with
  an unreadable layout keeps its name and opens as Edit's preset, and no saved one has `offered` or adopts panels; save keeps
  `api.toJSON()` without rebuilding the dock; built-in ones can be reset, not deleted, saved ones deleted (asked), not reset;
  Reset all leaves the saved ones and the order; the buttons reorder by pointer drag (Escape / `pointercancel` / blur abandon),
  or Move left / right in the button's menu; the strip is the bar's `auto` track between side minimums (84 / 380 px), the five built-ins and the + fit at 960 px, more scroll inside the strip with the + pinned outside,
  edge fades, edge autoscroll in a drag and the marker in content coordinates, names ellipsised at 132 px (`Workspaces the user saves`; `The title bar at every width`).
- Settings writes go through `single-flight.ts`: one write in flight, newest wins, the follow-up reading state
  when it starts (`one write in flight, newest wins`).
- `workspace.switchTo` keeps the arrangement being left, swaps layout and library tab, and touches no project
  state (`touches no project state`).
- A layout is written only if it was rearranged: after a restore wait two frames, take that as the reference;
  `shouldPersistLayout` / `sameArrangement` (shares within 0.4 %) decide; what was written becomes the new
  reference (`only if it was rearranged`).
- Each stored layout carries the panels its preset offered (`offered`, `withLayout`; `readWorkspaces` /
  `adoptPanels` update on read); a panel the preset opens that the layout was never offered is inserted by
  pure `insertPanel` (tabbed with its preset group, else at the dock edge it had (the timeline), else beside the nearest panel, else tab beside
  the preview, or with it closed the inspector's / deliver / any group but the library's rail or the timeline);
  the layout is never reset to make room; a notice says what was added (`describeAdopted`); a panel offered
  and absent was closed by the user and stays closed (`is never reset to make room`;
  `closed by the user and stays closed`).
- Window > a panel reopens beside the preview for the ones that anchor there while the preview is open; otherwise
  (and with the preview closed) at `openingSpot` of the workspace's preset (Edit's for a saved one), the Preview and
  the timeline included; no panel is special (`where the workspace's preset puts it`).
- Adding a panel to a preset means: `UNSTAMPED_OFFERED` must leave it out, and the earlier arrangement goes in
  `EARLIER_PRESETS` (a layout that is only a copy of today's or an earlier preset is dropped)
  (`offered it like the mixer in Audio`).
- Reset workspace (`workspace.reset()`, Window menu) forgets arrangement, its record and library tab, drops
  the debounced save, rebuilds from the preset and says what it did even when "already in its default
  arrangement"; reports `could not arrange` (`already in its default arrangement`). Reset all = `resetAll()`.
- A window resize can move shares: a `ResizeObserver` on the dock host writes what was pending, ignores the
  layout events the resize causes, retakes the reference after 150 ms of stillness
  (`otherwise the next unrelated event`).
- After every `fromJSON` force `api.layout()` at the host's real size, because a constraint changed in that
  gap makes dockview re-split the whole grid evenly (`makes dockview re-split the whole`).
- `+page.svelte` mounts the dock only once settings are loaded; panel roots are `flex:1;min-height:0` and own
  no width (`mounts the dock only once the settings are loaded`; `Panels own no width`).
- A panel is dragged by pointer events (`dndStrategy: 'pointer'`), never the browser's HTML5 drag: Tauri's
  drag-drop handler (on for file drops) leaves WebView2 no in-page `dragover` / `drop`, so the default drag moved
  no panel on Windows; Escape and a window blur abandon a drag through `dock-drag.ts`
  (`abandonDragOnEscape`), and `abandon()` before every rebuild of the dock and when a window opens or closes, or a drop would
  land on a panel of the new arrangement; only the primary button drags (`onlyPrimaryDrags`) (`Dragging a panel is a pointer drag`).

## Library panel and where actions live

- Library rail: roving-tabindex tablist; a pointer click does not leave focus on the rail; decoded thumbnails
  live in `thumbnails.ts` (the library remounts `MediaBin` on every switch); failed decodes are retried on
  next mount (`rather than the component`).
- The Media header is the one place for count and ways in; a bin row is compact; a kind the file cannot give
  is not drawn (`applicable` / `chipsFor`) unless it ran, is running or failed
  (`unless it ran, is running or failed`).
- Effects / Transitions / Audio tabs act on the selected clip, say why they are off, own no value; presets in
  `effect-presets.ts`; `Reset` is a ghost `Btn` on the heading line, never a chip
  (`say why they are off when none is`).
- One place per action: the Inspector tunes and creates nothing; the Library adds; the timeline places and
  trims; the menu bar keeps the common adds reachable; the styled titles, caption look and templates are the
  Titles tab's alone; `panels-polish.test.ts` holds it
  (`The Inspector is about the selection and creates nothing`; `are the Titles tab's alone`).
- Clicking the active rail icon folds the library (`library.collapsed`, persisted, shared by all workspaces);
  folded pins min = max = 40 px through `group.api.setConstraints` (panel-level `setConstraints` does
  nothing), hides the tab strip and hands freed width to the neighbouring group; a library sharing a group
  cannot fold (`has no listener on a dockview panel and does nothing`;
  `The library cannot fold while it has a window to itself`).

## Detached panels (popout.svelte.ts)

- Editor window keeps at least one panel (`detachBlocked`); a window that failed to open gives its label up
  (`#failed`); every open goes through `PopoutState.#open`, which announces (`popoutExpect`); `LabelBook` is a
  FIFO because `window.open` says nothing about which announcement it is for (`so opens are serialised`).
- Rule for panel code: `window` / `document` are the editor window's; an event in a detached window never
  reaches a listener here. Use `windows.svelte.ts`, `realm.ts` (`windowOf`, `documentOf`, `resizeObserverFor`
  / `intersectionObserverFor`: an observer from the editor window reports a detached element not intersecting
  for good), `window-events.ts` `onWindow`, `windows.listen` for app-level handlers
  (`The rule for panel code`; `not intersecting for good`).
- Svelte registers delegated handlers on its own mount root: anything drawn outside a panel's root in a
  detached window needs a root of its own there (one `ContextMenu` per window, `contextMenu.win`)
  (`registers delegated handlers`).
- The transport clock draws from `windows.requestFrame` (a hidden window pauses frames) and reads
  `performance.now()` itself; the registry re-asks on `visibilitychange`, add / remove, and a lazy watchdog
  (`The choice is not final`).
- Layouts keep their windows: `sanitizeLayout` reads `popoutGroups`; `sameArrangement` counts windows (12 px
  slack); restoring announces windows first, then builds the dock; saving waits `popoutRestorationPromise`;
  `settle` forgets unclaimed announcements; each workspace has its own windows (`announces them first`).
- Preview in a popout routes to the JPEG (`routePreview` `detached`), bounds observers stand down, surface
  hidden; its width is measured with the panel's window (`the panel is in a window of its own`).

## Title bar and menus

- Chrome is a title bar over the dock with the menu bar in it; no toolbar row; native OS decorations stay;
  `-webkit-app-region` drag with every control `no-drag`; three cells keep the workspace tabs centred
  (`The chrome is a title bar over the dock`).
- Export button: off with its reason as tooltip while the cut has no clips (`exportButtonState`,
  `aria-disabled` so it stays focusable); File > Export is greyed the same; Ctrl+E answers in a toast; shows
  percent while rendering (`stays focusable and the keyboard reaches the reason`).
- Pending proposal: cyan review chip (`proposalIndicator`, `reviewChipText`) and a count on the Agent dock tab
  (`agentTabLabel`, `data-pending`), because in Audio / Deliver the agent is only a tab
  (`and a proposal there was invisible`).
- A red bell badge carries a warning triangle so it is not colour alone (`so it is not colour alone`).
- OS window title via `window-title.ts` / `setWindowTitle` needs the `core:window:allow-set-title` capability
  (`core:window:allow-set-title`).
- Menus: `MenuBar.svelte` over pure `menus.ts`; Save reads "as…" once the project has a file because
  `save_project_as` is the only save (`is the only save there is`).
- An ellipsis promises a dialog or picker and nothing else; use `plainLabel()` for an action's name in a
  sentence; `copy-guard.test.ts` holds both (`An ellipsis promises a dialog or a picker and nothing else`).
- A menu bar is text-first: no entry carries an icon, only the tick / radio column; the key beside an entry is
  `Kbd` (`no entry carries`).
- Every entry names a keymap action and runs the page's own `run` handler, so menu and shortcut are one piece
  of code; new actions are unbound by default; a family with no key is a `MenuCommand` in `menu-commands.ts`
  (`so a menu and its shortcut are one piece of`). Window lists every panel as a `panel` command, except one with
  a key to bind, which is an action entry (`PANEL_ACTIONS`: `window.proxyQueue`)
  (`a panel with a key to bind is an action entry`).
- Quit asks about unsaved work like the window close and ends in the same `destroy` (`quitApp`, no new
  capability) (`Quit asks about unsaved work like the window's close button`).
- Menu keyboard: ARIA menubar, roving tab stop; Alt on its own or F10 focuses the bar (F10 not when
  user-bound, nor while typing); `alt-tap.ts` counts Alt only as a tap, never armed while a pointer button is
  held, cancelled by another key, press, wheel or blur; a menu title takes focus on click (WKWebView); a chord
  that closes a menu gives focus back (`Alt counts only as a tap`).
- Disabled menu entries still take focus (`aria-disabled`, reason as title); panels are `fixed`, placed from
  the opener's rect; nested submenus are not positioned inside a scrolling panel
  (`not positioned inside a scrolling panel`).
- Too narrow, the bar collapses to one "Menu" button; the test is the width of the title bar's left cell
  (`MENU_FULL_PX`), not the window (`test is the width of the title bar's`).
- A modal closes menus and makes the bar `inert`; a letter or arrow typed in a menu never reaches page
  shortcuts (the page returns early for events inside `[role=menubar]` / `[role=menu]`); a chord closes menus
  and goes on (`A modal closes the menus`).
- Transport is the Preview's own bar and sheds duration / rate under ~380 px of measured width
  (`transport-bar.ts`: unknown or 0 width is the full bar; buttons, timecode and scrub bar never go); tools /
  ripple / snapping / undo / delivery picker are the Timeline toolbar; `menus.test.ts` holds that no old
  toolbar entry lost its place (`an unknown or 0 width is the`).

## Deliver panel

- `DeliverPanel` reuses `Readiness` / `DeliverTo` / `SectionHead` with the export dialog, no fork; shape
  choice and smart-crop live on `ui` (`deliverShapes`, `deliverSmartCrop`, session-global); `Readiness`
  re-judges on every `editor.timeline` change, keeping the newest answer; shapes / platform check hidden and
  "Export N files" never said for a cut with no video clip (`hasPicture`); the render still goes through
  `ui.openExport()` (`no fork`; `re-judges on every`).

## Inspector

- A picture and its linked sound are one selection: the "N clips selected" notice counts link groups
  (`selectionUnits`, `link-ui.ts`) and a linked clip carries a "Linked to A1" chip
  (`A picture and its linked sound are`).
- The Inspector is mounted whether or not a clip is selected; subject = title, animated title, clip, else the
  bin asset, else an empty state; sections are native `<details>` `InspectorSection` blocks with a one-line summary;
  Timing and Volume open by default in Edit (`mounted whether or not a clip is selected`).
- The Inspector follows the task (`inspector-state.ts`): each workspace names the blocks it puts first (Color: Color,
  Curves / Wheels / LUT, Mask, Video effects; Audio: Volume, Fades, Audio effects, Music; Motion: Transform, Animation,
  Speed, Behaviours, Framing; Deliver: Framing, 360, Transform; Edit: the old order) and the sections it opens or folds;
  an id that is none of the five reads as Edit; `clipBlocks` is the one rule for which blocks a clip has, the markup
  holds no `{#if picture}`; a section is kept under `sectionId(title)`, namespaced by kind of subject (clip bare,
  `title-`, `animated-title-`, `asset-`, `template-card-`; only `TemplateSection` / `TextSection` are shared), and a
  section opened from code (`revealSection`) is not the user's choice (`Ids are namespaced by the kind of subject`).
- Open / closed is the section's own default, overridden by the workspace, overridden by the user: `InspectorMemory`
  keeps only what differs from the default, per workspace, for the session; the scroll is kept per workspace and per
  kind of subject and restored on mount; the panel is rebuilt on every workspace switch, so the `<details>` cannot hold
  it (`records only a choice that differs`; `restored on mount`).
- The clip header is one line: kind tile, name (never under 120 px), track, link, and one analysis pill (`Analyze`
  when nothing has run) that opens the Analyze menu; the facts wrap under the name in a narrow panel; the five chips
  are the bin's and the asset Inspector's; the one-line notes keep their detail behind a More button, not a tooltip; the
  form is held to 480 px and centred; an effect just added opens and scrolls to its section
  (`kind tile, name, track, what it is linked to`).
- One way to write a time in the Inspector: a place (In, Out, a title's Start / End, an animated title's) is timecode in
  a field, a length (a fade, a transition's Duration) is seconds on a slider row; a title's Color and Box are a swatch,
  `#rrggbb` and an opacity %, Box has an x for none; Reset goes back to a default, Clear takes keys or a ramp out, both a
  small secondary `Btn`, never ghost (grade included); a row label is `--text-secondary`; Transform rows carry the
  keyframe ◇, which and the pose's bundle keys are one thing to the summary, the Animation section and its Clear (rows
  are the union of both stores; removing a moment held by both is two undo steps, no single call exists); every key
  button, a number whose edit is a key, and every key writer refuse while the playhead is off the clip; while a speed
  ramp is set Rate is off (`One way to write a time`).
- `title-color.ts` is the port of the engine's `valid_color`: ffmpeg's name table (no `grey`), alpha read as the
  engine reads it, nothing written that it refuses; a colour change is built from the title the edit began on
  (`colorEdit`), never from the selected one; swatches note their pick on `input` and a field with nothing to write goes
  back to the row it is drawn from now (`commitTitleField`); the engine does not trim, so neither does this;
  `title-color.test.ts` reads `engine/colors.rs` (`reads `engine/colors.rs` to hold`).
- A typed number of a slider row ends on what its edit began on, and reads the row as it is by then only when none began
  (`leaveEntry`): a click that selects a clip without that section left the row's props unreadable (`leaveEntry`).
- Grade (`GradeSections.svelte`): one `set_grade` on release; Escape abandons via `beginDrag`; a click that
  only adds a point waits 300 ms for a double-click; a gesture that changes nothing writes nothing; curves use
  `grade.ts`, the TS mirror of the engine's cubic; the GPU preview refuses a graded clip
  (`the TS mirror of the engine's cubic`).
- Blend select (`blend-modes.ts`, ids = core `BlendMode`): one `set_blend_mode` on change, nothing for the
  mode the clip has; an absent field reads as `normal`; harness writes normal as no field and refuses an
  audio-track clip (`reads an absent field as`).
- Behaviours (`BehaviorsSection.svelte`): every write replaces the clip's whole list with one `set_behaviors`
  edit, read from the timeline when the gesture ends with only that row's number changed; a clip carries at
  most 8, Add off at the limit; Bake refusals show in the section, not as a toast; slider = one edit on
  release (`read from the timeline as it is when the gesture`).
- Stabilize (`StabilizeSection.svelte`) only for moving footage on a video track (`canStabilize`); the panel
  starts `analyze_clip_stabilization` itself for status `missing`, once per clip / setting / window, never one
  the user stopped (`noteStopped`); the export dialog counts unstabilized clips without refusing
  (`never one the user stopped`).
- Transition picker is grouped over `transitions.ts`; its bun test pins ids against `TransitionKind::ALL`
  (`pins the ids against`).
- Keyframes: Transform auto-keyframes at the playhead; colour and volume animate from the same panel (`setCol`
  / `setGain` / `keyAtPlayhead` over `set_property_keyframes` with the mirror's `upsertKey`, one edit per
  drag); mask numbers likewise (a live drag of a keyed one is not drafted); Crop motion rows have no static
  value, an edit is always a key at the playhead (`setCropNumber`) and Clear takes every crop key in one edit
  (`so a drag is one edit`; `an edit there is always a key at the playhead`).
- Dope sheet (`DopeSheet.svelte`, panel id `keyframes`, Motion preset): one row per keyed number (`dopeRows`);
  click selects and seeks exactly; key drag retimes across rows, magnets within 6 px (`dragMagnets` /
  `snapShift`) else the cut's frame grid, nothing before 0; Alt at release drops copies; a key landed on gives
  way; Copy / Cut / Paste / Delete / Select all go through `settings.actionFor` (`dopeKey`); ripple delete /
  duplicate / split are swallowed (`a key landed on gives way`).
- `EasingPopover.svelte` is drawn in the document body (a dock group is a containing block) and so listens for
  keys / clicks on its own box, since Svelte delegates a detached panel's events from the panel mount root; it
  closes without writing (`drawn in the document's body because`).
- Graph view (`DopeGraph.svelte`, `graph-editor.ts`): a handle drag is held to `0..=1` (no overshoot) and
  writes one `easeKeys` edit on the key that starts the segment; arrows nudge a twentieth, one edit per press,
  none for a held key; a handle ending on its starting curve writes nothing `sameEasing`
  (`on both axes, so no overshoot`).
- Every dope / graph gesture is one `set_property_tracks` edit of only the rows touched (`retimeKeys`,
  `deleteKeys`, `pasteKeys`, `easeKeys` in `dope-sheet.ts`); keys are named by number and time, not index; the
  preview of a proposal is read-only via `holdForPreview` (`Every gesture is one`).
- Web Audio gain follows a keyed volume (`gainAutomation`: linear ramps, steps where the curve does, read 1e-9
  s either side of a step); the timeline volume line and waveform scaling still draw the static gain
  (`steps where the curve does`).
- Mask / 360 / Text sections: every picture section is for `picture` assets on a picture track (sound half of
  a linked pair has none); picking a mask shape starts from a visible default; `lerpAngle` takes the shortest
  arc; footage Kerf did not detect as 360 has no 360 section, its override is the collapsed Advanced section;
  the caption style chip is deliberately not derived from existing overlays (`picking a shape starts from`;
  `takes the shortest arc`; `a caption's style is not recoverable from its text`).
- Contradiction on where titles are created: the Inspector "creates nothing" and its Titles section is gone,
  yet the Inspector still lists an always-visible Text overlays section with add / caption controls
  (contradiction in the full file: see `The Inspector is about the selection and creates nothing` vs
  `an always-visible`).
- Contradiction on easing presets: the Inspector picker is described with three bezier presets (eight
  choices), later as the same twelve as the popover (contradiction in the full file: see `own bezier presets`
  vs `eight before, now also Glide`).

## Speed ramps (speed-ramp.ts, speed-ramp-edit.ts, SpeedRamp.svelte; model speed_ramp.rs)

- Read `speed_ramp.rs` before changing either side. `Clip.speed_ramp` = `{time, speed, easing?}` keys in
  clip-local timeline seconds, at most 16, speed in `MIN_SPEED..=MAX_SPEED`, easing linear or hold; while
  `rampIsValid` the keys decide the rate and `speed` only the direction, else the ramp is ignored
  (`read it before changing either`).
- `speed-ramp.ts` is the one clock map: anything that converts between timeline and source clocks through
  `speed` (waveform / filmstrip columns, captions, transcript, beat grid, cut-to-beat, preview source time,
  trim bounds `headTime` / `tailTime`, `trimEdit`, Web Audio `rateAutomation`) must go through it; an unramped
  clip keeps the old `* speedMag` branch to the last bit
  (`Anything that converts between the timeline and source clocks through`).
- The harness replays the engine's rules: `setSpeedRamp` sorts and checks keys (empty clears; clearing a clip
  with no ramp is an error), keeps the source window, gives linked partners the same ramp on the timeline
  (`setSpeedRampLinked`), `setSpeed` clears the ramp, head moves rebase it (`rebaseRamp`), tail moves read the
  area under the curve (`tailSource`), slip keeps keys and refuses a ramp against another pace (`slipSpeeds`),
  a detached sound runs the picture's ramp (`The harness replays the engine's rules`).
- The sync lock and guard judge a ramped group by footage, not offset (`contentShift` / `stepAnchor` in
  `link-groups.ts`; `samePace` / `stepGap`); `trim` rebases on a head change (`trimRamp`, `Clip::follow_head`)
  (`judge a ramped group by footage`).
- Speed section: one `set_speed_ramp` on release, Escape / pointercancel / blur abandon via `beginDrag`; while
  a ramp is set Rate is unused, setting it clears the ramp and Reverse is off (reverse first, then ramp)
  (`ONE`).
- Every write goes through `planRampWrite`: unchanged keys are not sent; a clip starting after a linked
  partner is routed through the earliest-starting partner because `set_speed_ramp_linked` makes the named clip
  the authority; `moveKey` keeps order between neighbours closer than `2 * MIN_KEY_GAP`; music fit / audio
  sync refuse a ramped clip (`makes the named clip the authority`).

## Other Inspector rules

- Engine features reached in `ops.ts`: Export transcript .srt, Append all N files (`concatenate`, one
  revision), Copy / Paste keyframes (`editor.keyframeSource` remembers the clip, `keyframeOffer` says why off,
  `copy_keyframes` without offset keeps distance from the clip start)
  (`What the engine could always do and the interface did not reach`).
- A control that is busy keeps focus: while `editor.locked` (= `busy || previewingStaged`) do not use native
  `disabled` (it drops focus); `Btn`, `IconBtn`, `Chip`, `Tile`, `CheckRow` take `pending` (`aria-disabled`,
  swallowed press); fields are `readonly` + `aria-disabled`; selects write nothing while locked and restore
  the cut's value; a real reason (nothing selected) still uses `disabled`; `review-readonly.test.ts` and
  `pending-controls.test.ts` scan; an export no longer raises `busy` (`exportRun`), only a multi-format
  framing pass blocks, and New / Open decline while one runs (`A control that is busy keeps focus`).
- Inspector fields are one height (`--control-sm`, 28 px), numbers mono right-aligned; section summaries come
  from `inspector-summary.ts` (`The Inspector's finish`).
- One formatter for a time (`time-format.ts`; `timeDisplay` singleton in `time-display.svelte.ts` is what
  every readout calls): mode `timecode` (default; drop-frame at 29.97 / 59.94) or `seconds`; only Settings
  sets the mode; a sentence the backend wrote is not re-spelled (`diff.ts` mirrors the backend phrasing)
  (`One formatter for a time`).
- A field shows what can be typed back: In / Out and title Start / End show timecode (or decimal seconds to
  the frame) and accept either spelling via `parseTime(text, fps, mode)`; a bare colon time is read the way
  the mode prints one; the refusal toast says what to type (`timeHint`); In / Out count frames at the source's
  rate (`so it shows what can be typed back`).
- Title Color / Box are a swatch plus the free-text field (`title-color.ts`, a pick keeps the alpha); Reverse
  / Bold / Invert are `CheckRow` components; destructive pair is `Remove clip` / `Ripple delete` (`data-quiet`).
- With no clip or title selected the bin asset is the Inspector's subject (`AssetInspector`,
  `asset-facts.ts`): Media / Analysis / Proxy sections, `Relink…` / `Remove…` last (Relink is the missing
  callout's alone while the file is gone) (`is the Inspector's subject`).
- A click in the bin, a right-click there or a click on a template card drops the timeline's selection
  (`editor.pickAsset` clears it; the timeline's own `editor.select(asset)` does not): the Inspector is on what
  was clicked last (`drops the timeline's`).
- `AssetPreview` / `source-player.svelte.ts`: hover stills are one `getFrame` at a time, newest wins; Play is
  `startAssetPlayback` plus `getAudio` windows on its own context, the picture leading; it and the transport
  pause each other; its inputs are primitives so a tag edit does not stop it; a scrub-bar drag restarts the
  stream once, on release (`a still wherever the pointer is over it`).
- The asset name and every fact value are `data-selectable`; a right-click on a fact copies it
  (`its right-click copies the name or the path`).

## Asset tags (tags.ts = model/tags.rs port)

- Bin selects several assets: `editor.selectedAssetIds` with `selectedAssetId` the primary; plain click
  replaces, Ctrl/Cmd toggles, Shift extends from the primary over the rows shown (`bin-selection.ts` over
  `selection.ts`); `editor.select` always resets to one; Escape is the existing `edit.clearSelection` (no new
  shortcut); a bin drag takes only the asset it began on (`Escape is the existing`).
- `ui.binAssets` (`bin-view.ts`: tag filter, then file filter, then sort) is the one list the bin draws,
  Shift-click runs across, the header counts and the selection is kept inside; within a property group any
  may hold, every group must; an asset with no size or date sorts last either way; file size and modified
  time come from `mediaStatus.files` (`asset_files`), never from `Asset`; a project load clears the filters
  and keeps the sort (`sorts and filters like a file manager`).
- Tag filter is `ui.tagFilter`, AND-combined through `tagsMatch`; a filter that hides part of the selection
  removes it (`restrictToVisible`); a project load clears the filter; the vocabulary is read again whenever
  `tagsSignature(editor.assets)` moves; every write of `editor.assets` goes through `#setAssets`, which stales
  a read in flight (`Generation`) (`which stales a read`).
- `TagDialog` (`ui.tagDialog`, counted in `modalOpen`) is the after-import prompt (before analysis starts;
  `Don't ask again` writes the setting on any way out) and the bulk dialog; a second prompt waits
  (`ui.openTagDialog` queues); Apply is one `editor.tagAssets` = one `edit_asset_tags` for all assets; a tag
  edit is no revision and has no undo (`a tag edit is no revision and has no undo`).
- `TagInput` is a combobox: Tab fills the lit suggestion, only Enter adds, Escape closes the list before a
  dialog; the list is drawn in the flow (a dialog clips floated lists); Inspector tag changes are queued in
  order so a second × is never dropped (`queued in order, so a second`).

## Polish presets, styling rules, toasts

- Looks are pure data in `style-presets.ts`; `Color.temperature` renders as opposing `eq` per-channel gammas
  and is omitted at 0 so old graphs stay byte-identical; the caption style matches what `generate_captions`
  writes in its `lines` style (`omitted at 0 so old graphs stay byte-identical`).
- Style with CSS-variable tokens inline, not Tailwind utilities, except what must react to the pointer,
  because an inline style cannot say `:hover` and an inline background beats any stylesheet rule
  (`an inline background beats any stylesheet rule`).
- `controls.css` holds `.k-btn`, `.k-ibtn`, `.k-tab`; `IconBtn` writes `aria-pressed` only when given
  `active`; both default to `type="button"` (a bare button in a form submits it) and a form's own Save says
  `type="submit"` with `form=<id>` (`Enter in the Save-as-template dialog's Name field`).
- Other plain buttons get one veil (`--hover-fill` / `--press-fill`, an inset `box-shadow`), except timeline
  clips, menu rows and dockview controls (matched by class-token start); disabled is `opacity:.5` with a
  `default` cursor; a control that must stay legible when off sets `opacity:1` itself
  (`because inline styles never set`).
- Dock tab close X shows on hover or `:focus-visible`, dimmed not hidden, so it stays in the accessibility
  tree (`dimmed to nothing, not hidden`).
- One keyboard focus indicator: `--focus-outline` from a zero-specificity `:where(...)` rule in `layout.css`;
  inset in dense strips; `.k-field` wrapper carries it via `:has(input:focus-visible)`
  (`One keyboard focus indicator`).
- Toasts are themed (`ui/sonner`) on Kerf's scheme; one status list shared with the notification log
  (`status-look.ts`); text on the washes is held to 4.5:1 (`one list shared with the notification log`).

## Titles lane, Preview title boxes, transform handles, motion

- Titles are their own items, not part of a clip: titles lane `T` above V1 (`data-title-lane`); click selects
  (`editor.selectOverlay`, exclusive with clip selection) and seeks; one `update_overlay` per gesture; a
  selected title replaces the clip sections in the Inspector
  (`Titles are their own items, not part of a clip`).
- Preview title boxes hide while playing; move / resize with pointer capture; Escape / pointercancel / blur
  abandon; one backend edit on release; laid out in engine units (`cqh`, `size` of the height, the title's
  font else shipped Geist); the box is as tall as the string's ink (as `drawtext`'s `text_h`), measured by
  `inkBox` with `FALLBACK_INK` 0.9 (`The box is as tall as the`).
- A keyframed title follows the Transform convention: moving keys the position at the playhead
  (`editor.moveOverlay`, an existing key within 20 ms is updated), because static `pos_x/pos_y` is not what an
  animated render reads; resizing always writes static `size` (`because the static`).
- Transform handles (`TransformHandles.svelte`, `transform-handles.ts`): only for the selected picture at the
  playhead, paused, not streaming, no proposal, no trim monitor, unlocked, not 360 reframe; the box is where
  the export puts it (`layer-geometry.ts` = port of `layer_geometry.rs`, replayed against a Rust-written
  corpus); body moves, corners scale, knob rotates, edges crop (only under Contain with position / scale /
  rotation unkeyed) (`where the export puts it`).
- Handles: snapping 6 px, Alt inverts; scale stops at 100 %; rotation lands on right angles within 5 deg;
  Shift = 15 deg steps / one axis; one `setTransformAt` (`set_transform_at`) on release at `keyTime`, the
  unrounded clip-local playhead, which the Inspector's keyed writes use too, so both hit one key; only numbers
  that differ from the start are sent; Escape / pointercancel / lost capture / blur abandon; they count as
  page overlays for `routePreview` (`the unrounded clip-local playhead`).
- Motion presets: the library Motion tab (`MotionTab.svelte`); click = `applyMotion(preset)` in `ops.ts`: one
  `apply_motion_preset` for every selected picture clip (`motionTargets` leaves audio-track clips out since
  the core would refuse the whole call), through `holdForPreview` and `report`; options `ui.motionMode` /
  `motionLength` / `motionIntensity`; `motion-presets.ts` is replayed bit for bit over
  `fixtures/motion-presets-corpus.json` (`since the core would refuse the whole call`).
- Behaviour tiles: `addBehavior(kind)` appends the kind to each selected picture clip; clips with the same
  list are one `set_behaviors` edit, differing lists one edit per distinct list because the command replaces
  one list on all its clips; a clip at 8 refuses the lot
  (`since the command replaces one list on all its clips`).
- Live behaviours mirror (`behaviors.ts`, `behavior-ops.ts`, replayed over `fixtures/behaviors-corpus.json`; a
  pose is read to a billionth because Rust and V8 `sin` differ by an ulp). The resolver has two views:
  `propertyAt` / `transformAt` are the picture (behaviours on top), `baseAt` / `baseTransformAt` the pose
  without; `isDriven` / `transformAnimated` vs `transformKeyed` (keys alone: what routes an Inspector edit).
  Every edit that writes keys samples the base (`poseTransform`, `addKeyframe`, `motion-presets.ts`,
  `rebaseAnimation`, `motion-path.ts`); Inspector Transform sliders show and write the base
  (`Every edit that writes keys samples the base`).
- In the Preview the box follows the picture (`Layer.pose`) and writes through `toBase(layer, patch)` so a
  drag moves the pose and the behaviour goes on; a driven clip is not cropped in place (`croppable`); the
  motion path draws the base keyed path; a clip with only behaviours has no path
  (`a drag moves the pose and the behaviour`).
- Motion path (`MotionPath.svelte`, `motion-path.ts`; View > Motion path, `view.motionPath`, unbound, on by
  default in `localStorage`): shown for one unlocked picture with keyed position under the playhead; a
  position key is the union of both numbers' key times (`pathKeys`); a dot click seeks exactly (`seekTarget`);
  a dot drag reuses handle snapping and writes one `set_property_tracks` on release (`moveKeyTracks`), nothing
  if it moves nothing; only numbers that moved are written, a legacy-bundle number is detached alone, an
  unkeyed number moved by a free drag is keyed at every position key (Shift keeps it unkeyed); double-click
  keys every keyed position number at the nearest frame (`addKeyTracks`); counts as a page overlay for
  `routePreview` (`A position key is the union of the two`).

## Delivery frame, export dialog, platform check

- `Delivery.fps` (1..240) is the cut's rate for exports, playback and stills; set where the frame is chosen
  via `setDeliveryFps` in `menu-commands.ts` (keeps the frame, one edit); a new shape keeps the rate
  (`keepRate` in `setDeliveryPreset`, since `set_delivery_format` clears it when called without one); picking
  Source shape clears frame and rate; `timelineFps` / `renderedFps` return it ahead of any clip's;
  `checkDeliveryFps` mirrors the backend (`A new shape keeps the rate`).
- Export dialog "Source" resolution relabels to Project frame (WxH) so surfaces cannot silently disagree;
  `initialExport` starts a project with a rate on that rate (social presets say 30 and would override it); the
  remembered draft is dropped when the rate changed (`frameKey` carries `@fps` only when set)
  (`so the two surfaces cannot silently disagree`; `which would override the rate the user set`).
- The delivery picker sets `Timeline.format`; the preview pane then is that frame (`100cqh` units); safe-area
  guides are off by default and held process-wide in `settings.rs`; `DELIVERY_FITS` through `setDeliveryFit`
  keeps the frame (`so the two surfaces cannot silently disagree`).
- Loudness is a select of targets (`LOUDNESS_TARGETS`, all -1 dBTP) writing `ExportOptions.loudness`; the old
  `loudnorm` flag is the YouTube target and `loudness` wins as in the engine; `withLoudness` clears the flag
  so Off cannot become YouTube; Keep the dynamics writes `loudness_static`; the Mixer True peak toggle is
  export-only (`so Off cannot be turned back into YouTube`).
- Sync by audio: `syncOffer` greys the clip-menu entry with the backend's refusals; `Move anyway` (with
  `force`) applies the measurement just made; a queued sync shows a Cancel toast and the cancelled rejection
  stays quiet (`for a match that was not reliable`).
- Export options are remembered for the session (`export-draft.ts`), restored unless the project was re-cut
  for another frame, dropped by `ui.projectReplaced()`; a scrim click does not close the dialog
  (`a remembered 9:16 preset must not open on a 16:9 cut`).
- Platform check (`Readiness`, `platform-verdict.ts`) is for video exports of a cut with a picture only
  (`judgeable`; `null` verdict for no length); with no shape ticked it is one neutral line, amber / red only
  for a length limit of a platform the frame is made for; resolution and captions tips are folded, never
  amber; with shapes ticked each file is a row judged for the platforms that take that shape; the shape
  sentence appears only when the project is cut for a shape and the file is wider; judged at the frame and
  length the render will produce (`platform_check` answers a `PlatformReport` with the `CutSummary`; Rust
  summarises the sliced timeline `Project::cut_summary_in`) (`export of a cut with a picture`).
- `kerf_core::platform` decides what each platform accepts, `platform-verdict.ts` only which answers to show;
  there are no size rules (a size warning needs an engine estimate first); `platforms.ts` is used only by the
  harness (`There are no size rules`).
- Harness mirrors: `smart-crop.ts` mirrors only shape arithmetic (centre window); `captions.ts` is faithful
  (the mirror caught a Rust collision) and `caption-import.ts` / `placeCues` the same; `importCaptionsText` is
  the harness / file-input variant, `importCaptions(path)` needs the desktop app
  (`so the harness produces exactly the captions the`).
- Caption import (`importCaptionFile` in `title-actions.ts`): pick (desktop dialog answers a path the backend
  reads with its guards; harness reads an `<input type=file>` opened before any `await` or the click stops
  being a user gesture) -> confirm when generated captions exist (asked after the pick so it can name the
  file; counted on `editor.liveTimeline`) -> `editor.importCaptions` / `importCaptionsText` ->
  `describeImport` toast, warning when cues were dropped (`importTone`); selection untouched; "to a source
  clip" offers only assets a rendering clip shows (`importableAssets`) (`confirmReplaceCaptions`).
- `confirmReplaceCaptions` asks before Captions / Recaption, the lane menu entry, Edit > Generate captions and
  the Agent panel's "Caption the cut" chip (asked before the task is queued so declining leaves nothing);
  imported and generated sets are indistinguishable, so tooltips say replace / remove either
  (`so declining leaves nothing behind`).
- `Keep the file's lines` (`KEEP_LINES`) defaults by delivery frame (`keepLinesDefault`: on landscape /
  unframed, off square / tall, until touched) because `fit_size` shrinks a kept ~80-character line to the
  frame width; never sent in Word punch; `offset` may be negative (`normalizeOffset`) (`keepLinesDefault`).
  Tests stub `./api` and `./notifications.svelte` at module load because svelte-sonner cannot load under bun
  (`cannot load under bun`).
- Thumbnails (`ThumbnailSection.svelte`): `thumbnail_preview` PNG as object URL asked 300 ms after the last
  change, old URL revoked; every change is one `editor.setThumbnail` (`set_thumbnail`, stack rebuilt from
  `keptLayers`); `thumbnail-design.ts` `putThumbnail` is the harness side (`the old URL revoked`).
- Side-file exports (`file-actions.ts`) ask a path (`pickSavePath`) and toast the backend's `Report`
  (`export-report.ts`, warnings first); the export dialog's Captions & chapters: embed only for containers
  that carry a subtitle track (`carriesSubtitles`, mirrored in `validateExport`), `.srt` / `.vtt` sidecars, an
  existing sidecar comes back as a refusal that becomes a replace confirmation and a rerun with
  `overwrite_sidecars`; with no captions all reset to off
  (`A sidecar that already exists comes back as a refusal naming it`). Markers edit through
  `MarkerDialog.svelte` and `marker-edit.ts` (sends only what changed).
- `ExportDialog` opens on the frame the project is cut for (`initialExport`: preset whose resolution is that
  frame, else default with resolution cleared), or a 9:16 project would open landscape and warn about the
  shape just chosen (`otherwise a 9:16 project opened its export already`).
- Fast (GPU) preset (`gpuPreset(hw)`) is listed only when `hw_encoders` verified an H.264 hardware encoder
  (`api.hwEncoders()`), never opened on; x264 presets stay defaults until hardware quality is confirmed;
  `hwQp` mirrors the engine's CRF-to-QP offset (`is listed only when`).
- Deliver to chips add files beside the chosen path (`variantPath` mirrors `ExportVariant::beside`), each
  judged at its own frame (`platformCheck([w, h])`); shapes hide the Scaling rows; a stopped single pass
  rejects with "export cancelled; all N files were removed" (`in place of the single-file line`).

## Preview streaming (forward 1x playback)

- Preview shows the composited frame; during forward 1x playback it switches to the streamed `start_playback`
  source; per-frame `get_timeline_frame` stays for scrubbing, shuttle, the settled frame; the effect keys off
  `ui.seekEpoch`, never off `ui.time` (that would respawn ffmpeg every animation frame); it restarts for
  `boxWidth`, not for the source cap (`which ticks every animation frame and would respawn ffmpeg`).
- The stream plays at the cut's own rate (`renderedFps` = first video clip in the cut as it renders: muted /
  solo-shadowed tracks and disabled clips do not set it; `timelineFps` scans all clips as edited) and at
  `frameWidth`, always sent; the page leaves `fps` out at the top of the ladder and the engine reads the same
  number (`playback_params`) (`The stream plays at the cut's own frame rate`).
- Frames are raw JPEG bytes: `parsePlaybackMessage` (f64 time then JPEG, a view into the message, handler must
  not throw into the channel); only a frame the gate lets through becomes a `jpegDataUrl`, handed to
  `frames.showStreamed` into `frameUrl`, the one slot the settled stills share
  (`Its frames are raw JPEG bytes`).
- The picture is double-buffered (`picture-buffer.ts`): two stacked `<img decoding="async">` stay mounted
  across every route (hidden with `visibility`, never `{#if}`); a new `src` is set on the hidden one,
  awaited via `decode()`, then shown (`pictureSlot`); one decode at a time, newest waiting; shown in `frameSeq` order
  (identical URL counts); a superseded decode is dropped at most `MAX_DROPS` in a row; a rejected decode keeps
  the picture; `frameCovers` hides the old picture; `showStreamed` marks earlier requests stale
  (`shown = asked + 1`) (`The picture is double-buffered`).
- `CANVAS_PAINTER` is off; turn it on per engine only after measuring there
  (`Turn it on per engine only after measuring there`).
- Frame gate (`createFrameGate`, `playback-sync.ts`): lag is judged against the stream's own floor, never
  zero; only growth is drift (`staleAfter` at least 75 ms; `RESYNC_AFTER`); `OVERLOAD_AFTER` (0.4 s) of
  growing skips -> `degrade`; `HEAL_WATCH` 0.2 s / `RESTART_COST` 0.5 s / `HEAL_MARGIN` decide a heal
  `resync`; a quick catch-up is waited out (`measuring from zero dropped every frame forever`).
- `degrade`, or a resync within 10 s of another, restarts one rung cheaper (`stepDown`): frames are dropped
  from the source, not the stream; order: halve a rate above 30, narrow width 1920 -> 1440 -> 960, halve
  again, narrowest; the cheapest rung has no `degrade`
  (`frames are dropped from the source, not from the stream`).
- A stream starts the cut's usual start-up ahead of the playhead (`startAt`); the ladder remembers the least
  start-up per key (cut, proxy epoch, pane width), capped by `MAX_LEAD` (0.25 s) and the rendered end
  (`renderedDuration`); minimum, not latest, because a quicker start would put picture ahead of sound
  (`The minimum, not the latest`).
- `createLadder` also withholds a heal restart for `REPEAT_RESYNC_WINDOW_MS` (10 s) after a lag restart (gate
  built with `healWatch: Infinity`), else a start-up stall would restart into itself; steps are remembered for
  a minute (`stepsToStart`); an explicit `fps` goes to the backend only from the first step down
  (`because a stall that comes with`).
- The page keeps `<img>` over a canvas painter after measurement; WebKit and WebView2 are unmeasured
  (`WebKit and WebView2`).

## Transcript, agent panel, review

- `TranscriptPanel` (`transcript.ts`): a line resolves to the clip showing most of its cut range; `×` cuts via
  `cut_clip_range` over the words' bounds (pauses stay); an empty transcript says which of five reasons
  applies with the matching action (`a line whose words are gone reads as cut`).
- The agent panel is a real MCP task queue, no in-app chat; `agent.svelte.ts` is a runes singleton over the
  `tasks` table; with review off a finished task is `done` straight away (`complete_task`); a `ready` task's
  card offers Apply only when no proposal of its own is in the review card (`readyCard`); `queueEmptyHint` /
  `queuedChipLegend` follow the switch (`With review off a finished task is`).
- One status line (`agentLine`) + Connect, then review card, queue, quick edits, history; Apply / Preview /
  Discard row is pinned (`data-review-actions`); composer hint only at 520 px height (`showComposerHint`);
  `+page.svelte` raises the Agent panel once when a proposal lands (`editor.staged` null -> set)
  (`a review nobody can see is not a review`).
- Quick edits (`quick-edits.ts`) run an edit now and the task is recorded only after the edit has run (add,
  then resolve), so an agent never claims a task already done; a failed edit leaves nothing; `add_task` has no
  already-done form; the rest enqueue; Remove silences acts on `quickTarget`; local chips are off while a
  proposal is previewed (`the task is recorded only after the edit has run`).
- Review card: with review on, an agent's task edits never touch the open cut; with it off the card exists
  only for a `stage_edits` proposal or one left from before the switch; Preview swaps the editor onto the
  proposed timeline (`editor.previewingStaged`; `refreshTimeline` parks an incoming live update)
  (`whole point while review is on`).
- The preview is read-only: an edit is refused before the backend is asked (`#apply` takes a thunk;
  `PreviewReadOnly` shown as a notice); apply and discard are the two edits that end it; the preview is
  dropped only after an edit went through (`The preview is read-only`).
- Every way into an edit says so first as a notice: start with `holdForPreview()` in `ops.ts` before anything
  is planned, copied or remembered; show every rejection through `report(e)`; no component writes its own
  `toast.error(e.message)` for an edit; `review-readonly.test.ts` scans
  (`Every way into an edit says so first and says it as a notice`).
- Menus are held by data: `MenuState.previewing` turns editing entries off with the reason (`hold` in
  `menus.ts`), context menus use `heldForPreview` (`menu-items.ts`), quick edits use `heldWhilePreviewing`;
  toolbar, faders and lanes as drop target are off (`Menus are held by data`).
- Reads of the proposal are ordered (`ReadOrder`, `generation.ts`): of reads that come back the one that
  started last writes; re-check the preview after every await; project load / proposal settle call `invalidate()`
  (`the one that started last writes`).
- One set of answers in `proposal-actions.ts` (`applyProposal`, `discardProposal`, `inspectChange`): Apply
  confirms when `stale`, Discard of more than three changes asks (`discardNeedsConfirm`), a row selects
  through `reviewTarget` (a clip only in the proposal opens the preview) (`One set of answers`).
- Review chip text has three tiers by right-cell width (`Review N changes`, `N changes`, bare count)
  (contradiction in the full file: see `right cell narrower than 560 px` vs `from 440`).
- The queue card of the task whose edits are staged has no buttons of its own; `diff.ts` mirrors
  `TimelineDiff::headline` only; entries are phrased by kerf-core so `revisionDiff` returns `null` in the
  browser (`instead of a second, divergent diff`).

## Editor context, empty preview, z-order

- Report playhead, selection, marks, `ui.timelineView`, `ui.focusedPanel`, selected asset through
  `reportUiContext` (`editor-context.ts`: throttled, unchanged state not re-sent, 30 s heartbeat, no-op in
  harness) (`So that an agent knows what`).
- `PreviewEmpty`: three steps from `firstRunSteps`; action is `firstRunAction` (Add selected to timeline when
  the library is on screen, Open Library only when closed); Export is off with a reason until a clip exists
  (`exportButtonState`); gives way by height, not spilling over the transport
  (`it used to sit over an open library and do nothing`).
- Z-order layers: timeline 24-29, dockview overlay 45, dialogs 50, file-drop 60, notification log 70, menu bar
  panels 950, context menu 1000, toaster above; a new overlay takes the layer of what it is most like
  (`A new overlay takes the layer of what it is most like`).

## Preview frame, scrub bar, GPU preview in the page

- Preview frame = delivery shape at `min(100%, 100cqh x aspect)`; `requestWidth` rounds `ceil(css x dpr)` up
  to 480 / 960 / 1440 / 1920, settled 200 ms; a box with no width is not a measurement; the width is held to
  the widest source read at the playhead (`previewSourceWidth`, `capToSource`) except under a delivery frame;
  the pump asks nothing until measured (`is not a measurement`).
- On the GPU route the pump is not single-flight: one request per wanted time, `superseded` answers ignored,
  an answer older than the one on show dropped (`asked` / `shown`); each request carries `PreviewOrder`
  because IPC calls can reach the backend reversed; `gpu-preview-checked` refreshes the preview like
  `proxy-ready` (`On the GPU route the pump is not single-flight`).
- The Preview may be closed or behind another tab: closed it is unmounted (stream and GPU session end, the bounds
  cleanup hides the surface, the status bar's renderer clears); behind a tab (`panelApi.isVisible`, `shown`) the
  pump's `width` is `null`, the playback effect starts nothing and the renderer label stays clear, mounting hidden
  included (`preview-shown.ts`); the clock and the sound are `ui`'s and go on
  (`is a panel like the others and may be closed`).
- `ScrubBar.svelte`: rail uses `--scrub-rail` (alias of `text-disabled`, so the 3:1 guard covers it); drag
  moves the playhead live, at most one `ui.seek` per animation frame (`frameLimited`); `beginDrag` with
  pointer capture, Escape / pointercancel / blur restore; playback pauses while held and resumes at either end
  except at the very end of the cut; a press does not focus it (a focused slider owns Space and arrows and
  would silence transport shortcuts) (`a press does not focus it`).
- `routePreview` (pure, bun-tested) decides GPU vs JPEG from setting, platform, streaming, empty timeline,
  overlays, covered; on `renderer: 'gpu'` the frame draws nothing (`gpuShown`); bounds go to
  `set_preview_bounds` by `ResizeObserver`, window events and a 150 ms poll, one at a time (`singleFlight`),
  `deviceRect` rounding both edges; a hidden report goes out on unmount or setting off; under technique
  `window` the pane surround has an even-odd `clip-path` hole (`holePolygon`) covering the pane only, because
  one spanning the whole panel hid the bar controls (`one spanning the whole panel hid every control`).
- Drag frames (A3, `preview-draft.ts`): a gesture puts what it would write in one draft slot via
  `ui.setDraft(owner, draft)`, cleared once its edit lands or it is abandoned; each writer passes its own
  symbol and its `null` clears only its own draft (`putDraft`); equal drafts leave the slot (`sameDraft`); the
  Preview drops a draft naming a clip, track or title the cut no longer holds (`draftHolds`) and sends it only
  while it changes the frame at the playhead (`draftTouches`); read on the GPU route only; none while a
  proposal is previewed; builders return `null` where the outcome is not exactly known (`transformDraft`,
  `colorDraft`, `maskDraft`, `fadeDraft`, `titleMoveDraft`, `titleSizeDraft`, `moveDraft`, `trimDraft`; keyed
  numbers are not drafted); a trim that ripples or carries partners, roll / slip / slide, animated SVG titles,
  speed and grade sections draw the cut as it is until released
  (`so pointer events that change nothing ask for no frame`).

## GPU playback in the page (gpu-playback.ts, output-latency.ts, frame-pump.svelte.ts)

- With the GPU preview on and route `gpu` at Play, the playback effect starts a `GpuPlayback` session instead
  of the stream and the pump takes over; the webview and the audio stay the clock; each animation frame asks
  for `pictureTime(clock, outputLatency, roundTrip)` (latency held to 0.5 s, smoothed; round trip held to 0.25
  s), so a picture lands when its sound lands
  (`so a picture lands on the screen when its sound lands in the room`).
- A frame is asked for once (`frameIndex`); the first request is the only one in flight because it decides the
  session (`renderer: 'gpu'` or `'stream'`); `superseded` changes nothing (skip, never slow down: that is the
  backend mailbox's, not a gate) (`never twice, never an earlier one when an estimate moves`).
- Hand the session to the stream at once (`handOverToStream`) on a `stream` answer, a rejected request (GPU
  marked failed, surface hidden), a median round trip over three frame periods for a second, or a request
  unanswered 1.5 s (5 s before the first answer; only a waiting request counts); `play.ahead` hands over when
  the heard time is within the stream's start-up of it (`handoverDue`)
  (`hands the session to the stream at once`).
- The takeover stream runs in the GPU's time (`ladder.start({latency, ahead})`, `GpuPlayback.offsetAt`,
  `takeoverLatency`) so the picture does not jump at hand-over; it starts at the engine's hand-over only when
  a window is open and the page is on time (`takeoverStart`); synchronisation wins over the transition when
  the hand-over is late: honoured within `MAX_BEHIND` (50 ms) of the plain start, past that the stream starts
  at the playhead and the window is cut; an at-once hand-over is told nothing about a refusal still ahead
  (`takeoverAhead`); unmeasured ladder uses `DEFAULT_STARTUP`
  (`synchronisation wins over the transition when the hand-over is late`).
- A play holds the picture at the playhead it started from (`playStart`) until the audible time reaches it; a
  rebuild in a running play starts at the audible picture, never earlier than the play's start (`playSession`)
  (`A play holds the picture at the playhead it`).
- During takeover the stream is not `streaming` (route stays `gpu`, surface up) until its first frame at or
  past where the GPU stops is shown; a session given up on is barred for the rest of that play (`gpuBarred`:
  until a deliberate seek or an edit) (`so a GPU that failed or cannot keep up never flips back and forth`).
- A play the stream took over at a refusal is taken back by `TakeBack`: only when the session stopped where
  the engine said (`resumable`, not `failed`); after `READY_SETTLE` (1 s) ask `frames.ready(at, ..)`
  (`hint: 'ready'`, pump `client` + next `seq`, so a late ready is superseded) for `lead` past what is heard
  (`READY_LEAD` 0.6 s, capped `READY_MAX_LEAD` 2.5 s so it is played inside the backend's 10 s); `never` ends
  the take-back, `later` retries `READY_EVERY` doubling to `READY_MAX_WAIT`; the session begins a first
  frame's cost early (`firstFrameLead`, `learnedLead`, `handoverTrip`); route returns `SHOW_AHEAD` before;
  `GpuPlayback(fps, resume, true)` holds at `resume`; a declined session leaves the stream playing, a failed
  one ends the take-back (`abandonTakeBack`); `TakeBack.retry` 1, 2, 4, 8 s; pause, seek or edit ends it
  (`A play the stream took over at a refusal is taken back`).
- Under Proxy only the first request is rejected as the stream is (`playWaiting` -> `playbackWaiting`);
  pausing ends the session; the harness answers forward requests `stream`, `?gpusurface=1` pretends a surface,
  `&gpuplay=stream` / `&gpuplay=ahead` (`Under Proxy only the first request is rejected as the stream is`).

## Shared components

- `Modal` is the shell of every dialog (`shared-components.test.ts`): scrim click closes only if the press
  began on the scrim (`mayClose`, `scrimClick` in `modal.ts`); props `busy` (Escape, scrim and close button
  all off), `dismissOnScrim` (off for typed input), `initial`, `rescue`; Export `dismissOnScrim={false}` and
  never `busy`; Voiceover `busy={generating}`; Compose music `busy` + `dismissOnScrim={false}`; Fit / Stems /
  Update `busy` while they run (`is the shell of every dialog`).
- `SliderRow` (grid `72px 1fr 60px 24px`, `NUMBER_COL`; `reserve={false}` in a dialog): `MixSlider` writes one
  edit on release; Escape / pointercancel / blur abandon; an arrow-key run is one edit; double-click resets;
  only the thumb takes hold (a jump on the first click would make a double-click three edits); bipolar fills
  from the rest value; entry is NFKC-normalised and held to `limit` and the shown digits; a value is "at rest"
  only when exactly there (`Only the thumb`).
- A write goes to what the edit began on: `SliderRow` reads its `scope` (`Pending` in `slider-row.ts`) when a
  drag, key run or typed entry starts, parses entry with the spec it began with, and a focused number field
  starts a fresh `Pending`; Inspector helpers (`setTf`, `setCol`, `setCam`, `patchMask`, `setFade`,
  `moveTitle`, `keyAtPlayhead`) read that clip (`A write goes to what the edit began on`).
- Plain fields use `FieldEdits` (`field-edit.ts`: `noteFocus` / `noteTyped` / `endEdit`): note scope on focus
  and text on `input` / `compositionend`, because `change` arrives after the selection moved and the field
  re-rendered; a select or checkbox needs only scope; a bun test holds every text / number input of the
  Inspector to this (`so reading either writes the other clip's number onto this one`).
- `ToolbarToggle`: `pressed` is a toggle (announced), `on` lights without announcing, `bare` / `square`
  variants; states are `.k-tt` in `controls.css` (`one radius, and one "on" for every kind`). `Btn sm` is
  `--control-sm` (28 px).
- `Chip` is for on / off among choices (`pressed` required; an action is a `Btn`); dialogs drawing a chip by
  hand use `chip()`; `Tile`, `Hint`, `CheckRow`, `EmptyState`; `.k-select` uses `background-color`, never the
  `background` shorthand; `tip-guard.test.ts` treats `Chip`, `Tile`, `EmptyState` as owning their `title`;
  pills (`radius-full`) only for count badges and dots (`so a chip never reads as an action`).
- Tooltips are `use:tip`, not `title=` (`tip-guard.test.ts` fails on a native `title` on a plain element, a
  component passing one on, or one set from script; `Btn` / `IconBtn` / `ToolbarToggle` route their `title`
  prop); `settings.tip(label, actionId, detail?)` shows the user's key; one tip at a time; keyboard focus
  shows it at once only for `:focus-visible` and never on a typing field (`raisesFocusTip`); Escape only
  dismisses and is never consumed; not hoverable by design; a context menu holds tips back (`holdTips`);
  `aria-describedby` / `aria-label` rules in `describeTarget`; one element per document (`realm.ts`)
  (`Escape only dismisses; it is never consumed`).
- Context menus are keyboard menus (`ContextMenu.svelte` over `menu-nav.ts`): focus on open (panel for
  pointer, first runnable row `startRow` for keyboard), roving-tabindex, disabled rows `aria-disabled` not
  `disabled`, capture-phase handler keeps keys from page shortcuts, an action that opens a dialog finds focus
  on the invoker; radio-chip groups are one tab stop (`tabStop`, `roving.ts`); Proxy size arrows move focus
  only because a choice rebuilds every proxy; `a11y-guard.test.ts` scans (`Context menus are keyboard menus`).
- `ProgressBar.svelte`: named `progressbar`; without a value it sweeps a segment instead of parking at 30 %;
  reduced motion tints the track; `flex: 1 1 auto` because basis 0 in a column drew 0 px tall
  (`progress-guard.test.ts`) (`Progress bars say what they know`).

## Modals, recovery, unsaved work, notifications

- Every dialog is a `Modal` with `trapFocus`; `+page.svelte` makes the app behind `inert` and returns early
  from its global key handler while any is open (Space / Delete / J-K-L / undo would edit the live project);
  file drops are ignored then; shortcuts stand down inside text inputs / selects / contenteditable
  (`Modals are modal`).
- Focus survives a run and a menu: a dialog whose footer swaps for a run passes `rescue` and marks footer
  actions `data-rescue` (`focusLost`, pure); on close focus returns to the invoker, else to the menu bar tab
  stop (`data-focus-return`) only when the menu entry was run from the keyboard (`byKeys` ends on pointer
  press, Tab or focus given back) (`Focus survives a run and a menu`).
- A scrim click never loses typed input: Export ignores it; Voiceover ignores it while the script is non-empty
  (`scrimCloses`) and keeps a draft (`kerf.voiceover.draft`, cleared by a successful generate)
  (`A scrim click never loses typed input`).
- Crash recovery: `offerRecoveries` after first load and before a launch file; native Recover / Discard / Not
  now (`recovery.ts`; dismissed = Not now; the two-button fallback never offers Discard); Recover asks
  `okToReplace` first; after one recovery the rest wait for next launch
  (`Recovering a crashed untitled project`).
- Nothing unsaved is dropped silently: only a never-saved non-empty project can be lost
  (`editor.hasUnsavedWork`); New, Open, window close and the updater's Restart now confirm first (the plugin's
  async `confirm` cannot be used as a guard); confirms name verbs (`confirm-copy.ts`;
  `confirmAction(message, title, { ok, cancel })` requires labels); `askUnsaved` is Save... / Don't save /
  Cancel and a dismissed answer is always Cancel (`Nothing unsaved is dropped silently`).
- A render is `editor.exportRun`, not dialog state; closing the dialog leaves it running
  (`not dialog state: closing`). The `invoke` wrapper rejects any argument holding NaN / Infinity; Inspector
  number fields snap back on empty / negative / clamped entry (`rejects any argument holding NaN`).
- Every toast is also a notification-log entry (`notifications.svelte.ts`): components import `toast` from
  there, not `svelte-sonner`; the log keeps `description`; failure toasts lead with what was being done then
  why (`failure-copy.ts`, never "Could not"); a toast's Undo action is dropped from the log (it would undo
  whatever is newest later); failure paths that rejected into nothing now report
  (`action is dropped rather than kept`).

## Keyboard registry

- Shortcuts are an action registry in `keymap.ts`: one window handler in `+page.svelte` asks
  `settings.actionFor(e)` and runs the entry in a `Record<ActionId, handler>` (return `false` if the key was
  not taken); nothing else spells a key: menus / tooltips read `settings.shortcut(id)` / `withShortcut`;
  `keymap.test.ts` fails on hand-written `(⌘Z)` labels
  (`Keyboard shortcuts are an action registry, not key checks`).
- Chords match what the key types (`KeyboardEvent.key`), `Mod` is Cmd on macOS / Ctrl elsewhere, a chord means
  exactly its modifiers; a key that would not read back is never recorded; `keymap.test.ts` holds defaults
  against a copy of the old handler (`and Ctrl elsewhere, and a chord means`).
- Split at playhead is `edit.split` (`planPlayheadSplit`, `key-edits.ts`; one Undo per cut); roll / slip /
  slide keys are one frame per press with `repeat: false` and tried first on a scratch copy via `previewEdit`
  so limits read as sentences (`Roll, slip and slide have keys`).
- A long timeline lane culls clips: a clip outside the window or narrow has no button, so Tab and clip keys
  reach only clips in view (`A long timeline lane culls its clips`).
- Only what the user changed is stored (`Settings.keybindings` opaque `{version, bindings}`, `null` when
  nothing customised); `KEYMAP_VERSION` / `MIGRATIONS` carry renames; `parseKeyOverrides` drops unknown ids
  and unreadable chords and forgets defaults (stored `[]` = deliberate unbind); `resolveBindings`: a
  customised chord beats another action's default, other collisions go to the earlier registry entry;
  `repeat: false` actions act once per press (`Only what the user changed is stored`).
- Settings > Keyboard (`KeyboardSettings.svelte`): click-to-record; conflicts prompt Swap / Unbind / Cancel
  (`applyRebind` never guesses, Cancel has focus); per-row Reset (`applyReset`); focus returns to a row after
  every change because focus left behind a modal stops Escape closing it (`never guesses; Cancel has the`).
- `KEY_PRESETS` (Kerf, Premiere Pro, DaVinci Resolve, Final Cut Pro) are override tables over existing action
  ids, applied as ordinary overrides; `keymap-presets.test.ts` holds known ids, readable chords, no collision
  on either platform; actions Kerf lacks, and shift-plus-digit chords, are left out (`Left out, and why`).

## Settings dialog

- `SettingsDialog.svelte`: rail of nine sections, a section per `Settings<Name>.svelte` made of `SettingRow` components;
  `settings-index.ts` lists every row id with search words; `settings-index.test.ts` fails on an unfindable
  row or unanswered entry; every control is wired end to end (`every control is wired end to end`).
- Editing defaults are one opaque `Settings.editing` object (`editing-prefs.ts`, read field by field and
  clamped); `placementOut(asset, still_seconds)` is the source end of every single placement; Append all is
  the backend's `concatenate` and keeps its default (`never trusted`). Analysis switches (`auto_analysis`)
  also decide what an agent's `analyze_asset` runs with no steps.
- Agent section: `agent_review` off asks (task work lands too; a waiting proposal stays; `reviewOffNote` never
  claims edits land while a proposal is open); the address is validated before send (`addressProblem`),
  `KERF_MCP_ADDR` wins and the row says so, a network address needs the token; token masked until Show, a copy
  always takes real text; a `token_problem` is shown under the switch and in `McpConnect`; tool sets put
  `?tools=...` on the endpoint and snippets (`withToolSets`, `mcp-connect.ts`) and are page state, not a
  setting; server off / token off ask (`CONFIRM.serverOff` / `tokenOff`); a refused switch puts its box back
  via `flip`; a listener-restarting change is followed by `awaitListener` (up to five seconds) and
  `addressNotice` says what happened, never what was hoped for (`a `token_problem``).
- `settings-changed` carries the `SettingsView` (`settings.noteChanged(view)`); with `null` re-read; every on
  / off setting is a `Switch` and its `SettingRow` takes `for` (`settings.noteChanged(view)`).
- Advanced: `reset_settings` keeps the old file and the agent door (address, token); `settings.resetAll()`
  re-reads what is read once and `workspace.rebuild()`; the engine clamps the CPU percentage so the returned
  view is what renders; the harness answers from `localStorage` (keys `kerf.settings.*`)
  (`The percentage is clamped by the engine`).

## Media, review picks, lists, mirrored refusals

- Media gone: `mediaStatus.missing` from `missing_assets`, read with every status refresh and when a project
  comes up (`checkMissingOnOpen` opens `RelinkDialog`); Relink (`pickRelinkPath` -> `check_relink` ->
  `relink_asset`) keeps the asset id, place and every clip; the dry run refuses a stand-in before asking; a
  different recording of an analysed asset asks first (`relinkDropsAnalysisCopy`); Remove confirms and a used
  asset passes `force` as one non-undoable step; both held while a proposal is previewed
  (`keeps the asset's id, place and every clip`).
- Partial accept (`review-picks.ts`): nothing ticked is the whole proposal; ticked changes go to
  `apply_staged_subset` (`editor.applyStagedSubset`; closes only when the backend says `closed`; stale asks
  first); a linked picture and sound go together (`comingAlong` -> `closeOver`, the closure `staged-subset.ts`
  takes, port of `merge_subset`); the toast is the backend's sentence
  (`A linked picture and sound go together`).
- A discard is restorable (`editor.discarded`, read whenever no proposal is open; `restore_discarded_edits`;
  Restore asks first when the cut changed, `CONFIRM.restoreStale`); core drops the kept copy when a newer
  proposal applies or the project is replaced (`A discard is restorable`).
- Export asks before it replaces a file (`export-overwrite.ts`): `pathsExist` over every file the render will
  write except the one the native save dialog returned (`Export asks before it replaces a file`).
- Open Recent runs `openProjectAt(path, true)` including the unsaved-work question; status-bar jobs you may
  stop (`job-line.ts`) show the one in front with Stop; a stopped run is a notice (`isCancelled` in `report`)
  not an error (`Jobs you may stop`).
- Long lists are paged (`paging.ts`): history / tasks return the newest page; undo / redo cope with a head
  older than it (`undoable`, `redoable`) (`Long lists are read a page at a time`).
- Engine refusals mirrored: `clip-limits.test.ts` replays `review_tests.rs`; `link-ops.ts` `addClip` / `trim`
  / `setSpeed` refuse footage past the end (`SOURCE_EPS`), negative start, a clip left on another of its lane
  after ripple (`checkOwnLanes`), speed outside `MIN_SPEED..MAX_SPEED` (also a linked partner); typed In / Out
  asks `trimRefusal` first (`The refusals the engine gained are mirrored`).

## Update flow (updater.svelte.ts)

- Silent check at startup and every 6 h through `checkUpdate` / `installUpdate` / `relaunchApp`; states idle
  -> checking -> current | available -> downloading -> ready | error; Restart now warns about unsaved work
  (`is its own runes singleton`).
- The download holds the dialog still (`busy`, no Hide) because on Windows `downloadAndInstall` launches the
  installer and exits past every close guard (`so a hidden download could take unsaved work`).
- Install asks first on Windows only (`installPrompt(risk, isWindows())`, `confirm-copy.ts`): unsaved project
  -> Save / Install anyway / Cancel; export, analysis or import running -> a two-button question; nothing at
  stake -> none; `install()` ignores a second press; macOS / Linux keep the plain flow
  (`Install asks first, on Windows only`).
- A failed check is titled "Couldn't check for updates", never "up to date"; the AppImage note only for a
  failed Linux install; the dialog auto-opens once per version (`kerf.update.seen`); the plugin's `Update`
  handle stays module-local in `api.ts` (`never "up to date"`).

## api.ts bridge, harness mirrors, bridges

- Outside Tauri `api.ts` answers from a seeded in-memory sample with working local ops; frames are `null`;
  `data.ts` keeps only `STATUS_MAP` / `PRESETS` presentation bits and all project data comes from the real
  backend; harness flags: `?staged=1`, `?empty=1`, `?slow=1`, `?music=1`, `?missing=1`, `?tags=1`,
  `?recent=1`, `?exists=1`, `?proxy=failed|queued|waiting`, `?update=1`, `?hw=`, `?gpusurface=1`, `&gpuplay=`,
  `?relinkto=`, `?tokenproblem=1`, `?templateproblems=1`
  (`seeded in-memory sample with working local timeline ops`).
- `?proxy=queued` is a live proxy queue in the harness (one build at a time; `getProxyQueue`, `prioritizeProxy`, `cancelProxy`,
  `cancelProxyQueue`, `pauseProxyQueue` / `resumeProxyQueue`, `onProxyQueueChanged`); `?proxy=waiting` holds the machine for an export;
  with no queue flag every build starts at once as before (`is a **live queue**`).
- `?staged=1` seeds a proposal and the ready task it belongs to; its `stale` flag is computed as the backend's
  is (`a card offering to apply a change that does not exist`).
- Ripple in the harness is a port: `ripple.ts` mirrors `Timeline::ripple_from` (replays the Rust tests case
  for case; change a rule in kerf-core and its mirror together); `multi-edit.ts` mirrors `move_clips` /
  `remove_clips`; `edit-modes.ts` mirrors roll / slip / slide / split-remove and their `*Range` clamps
  (`Ripple in the harness is a port, not a`).
- Linked A/V mirror: `link-groups.ts` + `links.ts` mirror `model/links.rs`, `conformLinks` in `ripple.ts`,
  `link-ops.ts` mirrors `Project`'s link-aware ops and `run_edit` (`runEdit`: scratch copy, per-lane ripple,
  sync lock with named clips as anchors, guard, orphan dissolve) composed by `api.ts` through `devRun`; every
  edit takes optional trailing `link`; `links.test.ts` and `links-corpus.test.ts` replay Rust cases;
  `audio.ts` schedules no clip with `source_audio === false` (scheduling both a picture and its detached sound
  is the doubling); the harness cut starts detached-and-linked
  (`scheduling both a picture and its detached sound`).
- Range edits mirror: `range-edit.ts` ports `model/range_edit.rs` (`splitAllTracks`, `liftRange`, `extractRange`,
  `rippleDeleteGap`) and `link-ops.ts` wraps each in `runEdit` with ripple **off** (sync lock and guard on; labels
  `Split all tracks (N clips)`, `Lift range (N clips)`, `Extract range (N clips)`, `Ripple delete gap`); `cutEndAt` /
  `cutStartAt` in `edit-modes.ts` are the cuts `splitRemove` shares with them; `range-edit.test.ts` replays the Rust unit
  tests and `links-corpus.test.ts` the engine's answers (the harness does not mirror `check_new`). `api.ts` answers
  `EditDone<R> {timeline, result}`, takes `trackIds?` and `link?` and no `ripple`; one frame is `1 / renderedFps` via
  `EditEnv.frameRate` (a range that would leave a sliver is refused with the range that works; `not_cut`; `lock_trimmed`); `splitRemoveClips` takes `ripple?`
  (`the port of `model/range_edit.rs``).
- Harness keeps the ripple flag in state (not an edit, no revision) and runs every local edit that can change
  footage ahead of a clip through `devEdit`; layout-deciding ones (move, reorder, ripple delete, cut range,
  beat snap, paste) skip it as in the core (`api-ripple.test.ts` drives it) (`skip it as in the core`).
  `music-fit.ts` mirrors fit-to-length.
- `applyEdits(edits, label?, {dryRun?})` is several edits as ONE revision, all or nothing (`editor.applyEdits` is its `#apply` action): `edits`
  are `{tool, args}` of the MCP quick edit tools with their own snake_case args, answer `{timeline, results, applied, label?}`. The harness runs
  each op through its single-op function, then folds the revisions into one (restoring timeline, history and redo branch on a failing op or a
  dry run); `batch-edits.ts` holds the checks, `"$K.path"`, `compactAnswer` and the table of the clip-level ops (the rest are refused by name),
  and its test pins the tool list and the caps (300 ops, 1 MiB of references, 200 characters of name) to `mcp/batch.rs`
  (`applyEdits(edits, label?, {dryRun?})`). The args are the MCP tool's, which differ from the same-named `api.ts` function in places
  (`set_delivery_format` without `fps` keeps the rate). Text a user typed that goes into `args` goes through `literal()` (a leading
  `$` is a reference; `$100` is sent as `$$100`).
- `types.ts` is checked against the serde types (`types-mirror.test.ts`): names only (not field types or
  optionality); types whose shape the source does not decide are unjudged; on failure fix `types.ts` (missing
  Rust field: add it, `Option` / skip is `?`; extra property: remove unless something reads it) or add an
  `EXCEPTIONS` entry (`type`, `side`, `member`, `why`; each checked to still be needed); a differently-named
  mirror goes in `MIRRORS`; CI runs it, the local prek `bun-test` hook fires only on a frontend `.ts` change,
  so run `bun run test` after editing a serde type (`is checked against the serde types`).
- Composed music bridge in `api.ts`: stopped runs reject `music cancelled` / `band balance cancelled`, which
  dialogs stay quiet on; ids are snake_case inside the request unlike older camelCase arguments
  (`which a dialog stays quiet on`). `music-score.ts` mirrors the pure half of the score (`validateScore` with
  the engine's words, `MAX_SEED`, `resolvedChords` as played, `planScoreToPicture`); `music-place.ts` mirrors
  `project/music.rs`; `music-wire.ts` mirrors `deny_unknown_fields`.
- Harness `composeMusic` rehearses the placement on a copy with the render's length before rendering and
  checks again when it lands as one rippling edit; asset and analysis are stored only after, so a refusal
  leaves neither; `replace_clip_id` replaces the whole music bed (every clip of that asset on the track) with
  one clip; locked track, bed linked to a picture and non-composed clip are refused in the core's words;
  `getBandBalance` words its notes as `balance_notes` but the measurement is made up
  (`so a refusal leaves neither`).
- Sound effects bridge: `previewSoundEffect` returns WAV bytes (CSP has no `media-src`, no asset protocol;
  `null` in harness); `updateSoundEffect` takes the whole spec (what it leaves out returns to default); a
  placement's `at` is where the hit lands, not the clip start; `sfx.ts` `SFX_CATALOGUE` is a copy of the Rust
  table that `sfx.test.ts` fails on when a row, kind order or category label differs; placement rules
  (`planSoundEffects`, `rehearseSoundEffects`) rehearse before anything is stored; stand-in assets stored only
  after the placement went through; `suggestSoundEffects` is a lossy mirror
  (`fails when a row, the kind order or a category label differs`).
- Compose music dialog (`ComposeMusicDialog`, `ui.openCompose(prefill?)`): `musicStatus` first; progress under
  Cancel; cannot be dismissed mid-run, `dismissOnScrim={false}`; form is pure `compose-form.ts` (`setStyle`
  moves tempo only while it is the old style's default; `parseChords`); per-field issues (`formIssues`, engine
  messages from `chordError` / `keyError`), and `validateScore` is the oracle a bun test holds
  `formIssues().any` to; Fit to the picture is off with a reason until a cut or marker (`alignChoices` /
  `alignProblem`); re-composing replaces the whole bed and is off on a locked track or a bed linked to a
  picture (`composeOffer`) (`is the oracle`).

## State singletons, bin, stems, drop

- Analysis queue: a batch analyzes one asset at a time (ffmpeg-bound) and stopping drops the whole queue;
  `analyzeImported` runs nothing when the master is off, else only kinds that are on and not already done
  (`autoSteps`); `ensureAnalysis` serves the Agent quick edits; `QUICK_EDIT_STEPS`: Remove silences only
  `silence`, Cut to the beat only `rhythm`, Caption the cut only `transcript`
  (`so running them together only makes each slower`).
- No scripted demo phase machine: chrome derives from real state (`MediaBin` dropzone until `editor.assets` is
  non-empty; `StatusBar` names project and selected-asset facts apart, `status-facts.ts`)
  (`scripted demo phase machine`).
- Bin row, Inspector and menu phrasing live in `media-info.ts` / `asset-facts.ts` so they cannot drift;
  `mediaStatus` (`media-status.svelte.ts`) is updated by `proxy-progress` / `analysis-status` events and
  re-read when assets change; `stepCounter` counts only for a run asked for explicit steps; the status bar
  says which file the frame is decoded from (`previewSourceNote`, from statuses already held, nothing asked
  per frame), and is a button to the proxy queue while that proxy is building or waiting (`queue`)
  (`the Inspector and the menu cannot drift apart`; `the note is a`).
- Stem separation (`StemsDialog`, `ui.openStems(assetId, clipId?)`): `stemsOffer` gates the clip menu; the
  dialog asks `stemsStatus` first; cannot be dismissed mid-run; `editor.separateStems` refreshes the asset
  list on failure because the core stores stems before placing them; `placeStems` mirrors
  `Project::place_stems` (`because the core stores the`).
- Dropping files imports them (`onDragDropEvent`, filtered by `isMediaPath`, same list as the picker, so a
  folder of mixed files does not give one error per README); `editor.error` is a dismissible banner under the
  title bar (`doesn't answer with one error per`).

## Proxy only, export words

- The backend refuses a frame or playback whose clip has no proxy with a message starting
  `waiting for the preview proxy` (`proxy-wait.ts`, import-free because the frame pump is compiled alone in
  its test); the pump treats it as waiting (picture and GPU surface stay, no JPEG fallback); `Preview.svelte`
  shows "playback waits for the proxy" instead of a failure toast and restarts the stream when a proxy lands
  (`import-free because the frame pump is`). Both waiting chips are buttons that open the proxy queue and end in
  how many it holds and how long, when that is known (`hudLine`) (`Both of its chips`).
- Export words come from `export-progress.ts` (`progressLine`, `statusText`) over `ExportProgress.stage`; the
  fraction is over the whole export and never runs back; the last notice carries the file's measured loudness
  (`ExportReport`, `writtenNotice`); a true peak over the ceiling is a warning, a failed measurement adds "not
  measured" without alarm; `exportTimeline` answers one report, `exportVariants` an array (`never runs back`).

## SVG titles and templates

- Animated SVG titles: the page never inlines SVG (`svg-titles.test.ts` fails on `{@html}` / `innerHTML` in
  `src/`); only `<img>` data URLs; `svg-dev.ts` mirrors kerf-core refusals; custom SVG is not sanitised in the
  harness (`validateSvg` drops scripts / foreign objects / `animateMotion` and refuses what the engine
  refuses); preset artwork is `fixtures/svg-presets.json`, rewritten with
  `KERF_BLESS_SVG_PRESETS=1 cargo test -p kerf-core --no-default-features -- svg_preset_fixture` after a
  change in `kerf-core/src/svg/presets.rs`; a patch with `svg` also drops `template`
  (`never inlines the SVG`).
- A preset patch always carries every parameter (the backend defaults a missing one, so `{text}` alone would
  reset colours); `changePreset` is one `update_svg_overlay` with `preset_kind`; Apply of custom source asks
  `validate_svg` first; the preview placement box is `data-svg-box` (never the artwork) and counts toward
  `titlesShown`; `staged-diff.ts` marks animated titles by `overlay_id` and resolves `overlay_removed` against
  `svg_overlays` (`would reset the colours`).
- A click on a template card selects it (`ui.selectedTemplate`; `TemplateInspector` plays it from one
  `templateFrames` call of at most 64 pictures, `template-frames.ts`); a double-click adds it at the playhead;
  the selection is dropped by a timeline selection, a bin click and a project load (`a click selects it`).
- Templates: `values` is read back from the title's own `svg` and has no `image` controls; change one with
  `setTemplateValues`, never a patch of `svg` (that ends the template); `template-dev.ts` mirrors core
  refusals; `import_template` never overwrites (`never a patch of`).
- A template title's `svg` holds text as glyph outlines (`<g data-kerf-text="">`); `title-text.ts` `textForm`
  ports `restore_text` and is replayed against `fixtures/outline-corpus.json` (bless with
  `KERF_BLESS_CORPUS=1 ... -- outline_corpus`); `TextSection.svelte` writes one `setTitleTextBox` /
  `setTitleTextAnimation` edit per commit, nothing while locked, busy or previewing; box splice replayed by
  `text-box-corpus.test.ts`; text animation JSON is byte for byte what `to_attribute` writes, replayed by
  `text-animation-corpus.test.ts` (`kerf-core's own outlines by`).
- `template-ui.ts` (pure): an id that two or more elements carry is not targetable and is listed apart as
  disabled (`ambiguousFor`); the engine has the last word on `checkDraft`; colour values follow
  `valid_svg_color` exactly; a picture is shown only for `data:image/png|jpeg|gif|webp`; deleting a template
  asks first (`not targetable`).
- Template Inspector section: one `set_template_values` edit per committed change (text / colour / font on
  `change`, a number on blur or Enter, never per keystroke), to the title the field took focus on via
  `FieldEdits`; a change arriving while another edit is in flight is held with what was typed and made when
  that edit ends; a proposal on screen refuses and puts the field back (`never per keystroke`).
  `templates-changed` fires for the backend's event and for the page's own save / import / delete (`api.ts`
  announces after the call resolved) (`which fires for the backend's event`).
- Titles on tracks: a title clip is a `Clip` with `generator: {kind: 'title', ...}` and `asset_id`
  `GENERATED_ASSET`; `title-clips.ts` is pure (`findTitle`, `placeTitleClip`: named video track, else lowest
  unlocked one free over the span, else new `V{n}` on top); `setSpeed` refuses a title as the engine's
  invariant does; `editor.select` of the generated asset looks nothing up; a title clip has no Speed section
  (`the lowest unlocked one free over the span`).

## Scopes panel

- `Scopes.svelte` is docked in the Color preset (a stored layout is offered it like the mixer in Audio;
  `UNSTAMPED_OFFERED` leaves it out, `EARLIER_PRESETS.color` is the old Color); the frame is the export's
  still from originals at 960 wide, niced, so a real decode: one request in flight, newest asked once it
  lands, 150 ms debounce when paused, every 2 s playing, nothing while hidden; colours are the `--scope-*`
  tokens, resolved again on a theme change (`niced, so it is a real`).

## Sound FX panel (dock id sfx, SoundFx.svelte, sfx-panel.ts)

- State lives in the singleton `sfxPanel` because a workspace switch rebuilds the panel; the catalogue is read
  once from `onMount`, never an `$effect`, or a failing backend would be asked again for ever; a kind this
  build's table does not know is left out of the grid and shown as a plain suggestion row
  (`a failing backend would be asked again for ever`).
- Tile drag uses `beginDrag` (Escape / pointercancel / blur abandon; the click that ends a drag is not a pick)
  and the bin's pointer drag (`ui.dndSfx`, `ui.dndPoint`, `ui.sfxDropTarget`), not HTML5 drag-and-drop; the
  ghost shows over an audio lane only; red = a drop the core would refuse, refused on release in words
  (`dropRefusal`) before the engine is asked; with ripple on an add pushes later clips, so only an effect
  starting inside a clip is red; `sfx-panel.test.ts` holds the ghost to `planSoundEffects` in both modes; a
  panel in a window of its own does not drag (`popout.isDetached('sfx')`)
  (`so an effect that only runs into the front of a clip`).
- Panel changes go to a draft only; nothing is written until Add; the audition context is made and run
  synchronously from the press (`Auditioner.prime`), because a webview lets audio start only from a gesture; a
  `resume()` unanswered for 750 ms is given up on with a toast
  (`a context first made after the await can stay suspended`).
- `sfx.add` is unbound by default, `repeat: false`; it waits for or re-reads the catalogue
  (`addFromShortcut`); Add is off for `editor.locked`; adds go through `editor.addSoundEffects` (`#apply`,
  then the bin is re-read before the cut shows the clips, so nothing points at an asset the bin lacks)
  (`so nothing points at an asset the bin lacks`).
- Auto sound design: Find moments is a read; a newer find overtakes an older; off with nothing on the timeline
  and while a proposal is previewed (moments are read from the live cut); a list belongs to the cut it was
  found on (`stampOf`): after an edit it is out of date and cannot be added
  (`A list belongs to the cut it was found on`).
- Inspector `SoundEffectSection`: a slider commits one `editor.updateSoundEffect(clipId, wholeSpec)` on
  release; the asset is swapped in place, hit / fades / effects / level stay, one `Edit sound effect`
  revision; off while `editor.locked` (`pending`, keeps focus) (`Edit sound effect`).

## Proxy queue panel (dock id proxyQueue, ProxyQueue.svelte, proxy-queue.ts, proxy-queue.svelte.ts)

- In no preset: it is opened where the wait is felt (the Preview's waiting chips, the status bar's source note, a
  queued / building proxy's bin badge: `workspace.show('proxyQueue')`) or from Window, an action entry
  (`window.proxyQueue`, unbound); no layout is offered it, and a layout the user put it in keeps it
  (`it is opened where the wait is felt`).
- The state is the singleton `proxyQueue` over a backend and a clock it is given; it subscribes only while a view
  `watch()`es (the panel while on screen, the Preview while a waiting chip is drawn; the status bar and the bin
  only link); the first watcher makes the one pair of subscriptions and reads, the last drops both
  (`only while a view watches`).
- `proxy-queue-changed` is read at once; `proxy-progress` at most once a second; reads are single-flight and
  ordered, and Pause / Resume's answer takes its place in line like a read (`so it is read at most once`;
  `the answer of Pause / Resume is itself a queue`).
- A job's `eta_secs` counts from the read: the clock time is anchored at `readAt`; a job with no estimate makes none
  up; with `waiting_on` the time is "then about …" with no clock time (`does not drift while the answer ages`).
- "N× real time" is `rate / (width × height)`, said only for a build on the pipeline the rate was measured on
  (`pipeline that rate was measured on`).
- Build next is not offered to the first job waiting nor to a build; Cancel all asks first (`cancelProxyQueueCopy`,
  *Cancel all* / *Keep building*, destructive, `data-quiet`); a pointer click blurs the button; a busy row is
  `pending`, not `disabled`; a refusal goes through `report` (`canPrioritize`).
- A queue of hundreds draws `ROW_CAP` rows with "and N more" and a Show more; rows are keyed by asset, a moved row loses
  focus, so a keyboard Build next puts it back and a keyboard Cancel moves it to the next row (`a moved element loses focus`).

## Open ends the full file states

- Transform handles: separate X / Y scale is not done (needs model, export-graph and GPU change); a live GPU
  picture during the handle drag (A3) is not done (`Not done: separate X / Y scale`).
- No size warnings on the platform check until an engine estimate exists (`There are no size rules`).
- WebKit and WebView2 are unmeasured for the playback transport; `CANVAS_PAINTER` stays off
  (`WebKit and WebView2`).
- GPU takeover limits: a stream that takes longer than the start-up to start trails the sound by the
  difference on the first Play; the switch can land a frame early or late; a take-back from a stream that
  never was the GPU's steps the picture back by the output latency (`What is left at the switch`).
- `add_task` has no already-done form: a connected agent could claim a quick-edit task in a window of two IPC
  round trips (`closing it needs a backend command that creates it done`).
- Tooltips on a disabled control in engines that drop its pointer events: Chromium delivers them, WebKit
  untested (`WebKit untested`).
- Custom window decorations (menus in the caption bar) are a possible follow-up
  (`are a possible follow-up and not this change`).
- Fast (GPU) export preset is not the default until hardware encoder quality is confirmed on real footage
  (`until the hardware encoders' quality is confirmed on real footage`).
- Harness gaps by design: no thumbnail preview, no `revisionDiff`, `suggestSoundEffects` is lossy,
  band-balance measurement is made up, harness text animation plays nothing (`HARNESS_ANIMATION_NOTE`).
- Keymap presets leave out actions Kerf lacks (Premiere ripple edit / lift / extract, Resolve pages and blade,
  Final Cut connect / insert) (`No such action in Kerf`).
