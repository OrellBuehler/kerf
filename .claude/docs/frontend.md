# Frontend (`frontend/`)

SvelteKit 3 / Svelte 5 **runes** (forced on in `vite.config.ts`). Two layout quirks:
- **No `svelte.config.js`** — adapter and compiler options live inline in
  `vite.config.ts` via the `sveltekit()` plugin (new-style config). Static SPA via
  `adapter-static` (fallback `index.html`); `+layout.ts` sets `ssr = false` +
  `prerender = true`. Dev port is pinned to **1420** for Tauri.
- **Tailwind 4 = CSS config**, no `tailwind.config.js`. **Fonts are self-hosted**: Geist, Geist Mono
  and Space Grotesk come from `@fontsource-variable/*` imported first in `layout.css` (family names
  `'Geist Variable'`, `'Geist Mono Variable'`, `'Space Grotesk Variable'`, which is what `--font-sans` /
  `-mono` / `-display` name), bundled by Vite, so nothing is fetched at first paint and the CSP has no font
  host (a `@import url(https://fonts.googleapis.com…)` sat after the Tailwind imports for a long time,
  which CSS ignores — the brand fonts never loaded and every screenshot was the system fallback).
  The fonts are SIL OFL 1.1, so their licence text ships with the installer: `crates/kerf-app/licenses/fonts-NOTICE.md`, listed in `bundle.resources` of `tauri.conf.json` *and* in the `resources` override of `pr-build.yml` / `release.yml` (a new notice must go in all three; `bundle-resources.test.ts` holds the overrides to the committed list).
  `src/routes/layout.css` imports
  the **Kerf design tokens** (`src/lib/styles/kerf-tokens.css`) and maps the shadcn
  semantic vars onto them. **Every color is themable**: `src/lib/theme.ts` lists
  the opaque color tokens (grouped for the editor), three presets (Kerf Dark —
  bun-tested to equal the stylesheet's defaults — Kerf Light, High contrast),
  `parseTheme` (a stored or imported JSON; unknown tokens dropped, missing ones
  filled from the preset the colors it does carry are exactly — a High contrast saved before a token existed stays
  High contrast — else the scheme's) and `applyTheme`, which writes the tokens as
  inline properties on `<html>` (beating the stylesheet), toggles the `dark`
  class and `color-scheme`. Every translucent token in `kerf-tokens.css`
  (borders, fills, glows, the scrim) is a `color-mix` of an opaque base so a
  theme is a flat list of hex colors an `<input type=color>` can edit, and the
  editor components carry no color literals (`--scrim`, `--text-on-video`,
  `--drag-ghost`, `--frame-matte` exist for the few places that used to).
  A theme also carries a **shape** (`SHAPE_TOKENS`: `line-width`
  hairline borders / dividers / clip edges / ruler ticks, `line-emphasis` the
  selected-clip outline, `playhead-width`, and the slider's `slider-track`,
  `slider-track-radius`, `slider-thumb` plus a `round` / `bar` thumb style) —
  numbers clamped and snapped to their range by `parseTheme`, which fills a
  missing shape (an older theme file) from the scheme's preset; `shapeProps`
  turns it into the custom properties `applyTheme` writes, and the Dark preset's
  shape is bun-tested equal to the stylesheet like its colors. Every
  `input[type=range]` is styled once in `routes/layout.css` (webkit and moz
  pseudo-elements) from those variables, with `--slider-accent` as the one
  per-slider choice; changing a shape value makes the theme `Custom`, and
  High contrast ships thicker lines. The shape also carries the **three border weights**
  (`border-alpha-subtle` / `-default` / `-strong`, the % of `border-base` that `--border-subtle` /
  `-default` / `-strong` are; Dark 6 / 10 / 16, Light 8 / 14 / 22, High contrast 30 / 45 / 70 so its
  component edges pass 3:1) and the **shadow strength** (`shadow-strength`, % of the Dark alphas; Light
  30), with `shadow-color` a colour token beside `scrim` (Light's is a cool `#141e32`, so its menus stop
  casting a grey smear); `SHAPE_TOKENS` carry a `unit` (`px` or `%`) that `shapeProps` appends, and every
  `--shadow-*` is a `color-mix` of `--shadow-color` scaled by `--shadow-strength`. `parseTheme` fills a
  value a stored theme lacks from the preset whose colours it is exactly (a High contrast saved before
  these existed stays High contrast), else the scheme's. `app.html` paints Kerf Dark before hydration
  (and the desktop window stays hidden until the theme is applied — see kerf-app).
  **Two bun guards keep this true** (`theme-guard.test.ts`): a scan fails on any
  color literal (`#hex`, `rgb()`, `hsl()`, …) in `src/` outside `theme.ts`,
  `kerf-tokens.css`, `app.html` (pinned to Kerf Dark's `surface-app`, as is the
  window's `backgroundColor`) and the two browser-harness image generators
  (`sample-frame.ts`, `sample-filmstrip.ts`, whose colors are picture, not
  interface); and every preset must meet WCAG contrast (`contrast.ts`) for the
  pairs the UI draws — 4.5:1 for reading text and the labels on solid fills
  (`text-primary` / `-secondary` on every surface, `text-muted` on the resting
  ones, `text-on-accent`, `agent-fg`, `text-on-video` on the clip bodies), 3:1 for
  muted text on hover / active, accent and status hues as text on the panels and
  the strokes that carry meaning (clip edges, the playhead / selection amber, the
  waveform, the drag ghost, and `clip-accent` — the volume line, fade handles and keyframes on every clip
  body), plus the `color-mix` fill of a generated caption's
  block (`GENERATED_TITLE_FILL`, resolved per preset by `mixSrgb` and held to 4.5
  under its label). `text-disabled` is exempt on a *disabled* control; it used to be
  the idle colour of live toggles too (DUCK / S / L, the bell), which is no longer so (see below),
  but it is still held to 3:1 on the resting surfaces so a greyed label can be found; the
  translucent hairlines are exempt.
  High contrast is also held to 7:1 on its reading text. **Tinted surfaces** are checked too: a badge, an
  analysis chip or an error block is its hue at 10–16 % over whatever it sits on (read from
  `kerf-tokens.css` by `tintOf`), with the hue's light shade as 10 px text on it, held to 4.5 on the three
  panels and the hover surface; the filled buttons' hover / pressed shades (`kerf-600` / `-700`,
  `agent-600` / `-700`) under their labels; the focus outline's `kerf-400` on every resting surface; and
  High contrast's `border-alpha-default` / `-strong` against its panels at 3 / 4.5. Writing it moved Kerf
  Light's `green-400`, `orange-400`, `red-400`, `agent-300` and `kerf-300` darker (the v0.24.0 set and the
  one before `kerf-300` moved joined `SUPERSEDED_KERF_LIGHT`; `kerf-300` could only go as far as the S / L
  flags' 3:1 under their dark label allows, 3.1) and the amber `Badge` onto `kerf-200`. **`--text-disabled` is for what is off**:
  a scan allows it only in the menus' disabled rows, `controls.css`, and a few named exceptions (each with
  its reason in `theme-guard.test.ts`), because the status bar, every explanatory paragraph and the idle
  S / L / DUCK toggles used it at 3.3:1 — text people read is `text-muted`. **`type-guard.test.ts` keeps the
  scale a scale**: a `font-size` is a `var(--text-*)` or one of the literal steps (10 / 11 / 12 / 13 / 14 /
  16 / 20 / 26 / 34 / 46 px, read from the stylesheet), nothing under the 10 px floor, and a `var(--type-*)`
  / `var(--text-*)` must be defined (`--type-caption` was used and never existed, so its text took its
  parent's size). The pair list
  and its reasoning live at the top of that test; a failing preset gets its
  *values* fixed, not the list. Writing it found real defects — Kerf Light's clip
  bodies were pale under white labels (1.5:1), the logo mark was a hard-coded
  near-white that vanished on Light, and five dialogs carried a dead `rgba`
  shadow fallback — so Light's clip fills, waveform, amber, three status hues,
  muted text, disabled text and `agent-fg` were adjusted (Dark and High contrast
  passed as they were). **A stored theme is a copy**, so users who had picked Kerf
  Light kept the old colors: `upgradeStoredTheme` (on what `settings` reads back,
  never on an import) moves a theme whose colors are *exactly* a
  `SUPERSEDED_KERF_LIGHT` set to the current preset — name and shape kept — and
  leaves anything else alone; whenever Light's colors change again, the outgoing
  set goes on that list. `layout.css` is also the `tailwind.css` in `components.json`. Run
  `bunx shadcn-svelte add <name>` to add primitives.

The editor UI is implemented from the **Kerf design system** (claude.ai/design): an
editor-grade workspace under `src/lib/components/editor/` — bespoke atoms (`Btn`,
`IconBtn`, `Badge`, `Icon`, `KerfMark`) plus `TitleBar` (which holds the **menu bar**)
and `StatusBar` as fixed chrome around a **dockable workspace** (`Workspace.svelte`, composed by
`routes/+page.svelte`). The workspace is `dockview` (the vanilla package; its
`--dv-*` variables are mapped onto Kerf tokens in `styles/dockview-kerf.css` so
it follows the theme) hosting nine panels — `LibraryPanel`, `Preview`, `Timeline`,
`Inspector`, `AgentPanel`, `DeliverPanel`, `Mixer`, `Scopes`, `DopeSheet` (*Keyframes*) — each a Svelte component
`mount`ed into a dockview content element, so every panel is resizable by its
sash, movable by its tab (drop zones on any group edge, or tabbed into a group)
and closable; the **Window** menu reopens one (the library left of the
preview, the deliver panel and the mixer right of it, the rest beside the active
group) or resets the workspace. **Workspaces** — Edit / Color / Audio / Motion / Deliver,
toggle buttons in the **title bar** (`WorkspaceTabs`, the centre of a three-column
grid so they stay on the centre line; `aria-pressed`, since there is no tabpanel
to point a tablist at; a pointer click blurs the button, because a focused button
swallows Space and the transport shortcut would stop working) — are full dockview
presets. `src/lib/layout.ts` is
the pure, bun-tested side: the panel registry (titles, minimum sizes — the
Inspector's px-tuned controls need ~250), `PRESET_LAYOUTS` (each a row of panels
over a **full-width timeline** — the cut is what an editor looks at most, and a
timeline squeezed between two side panels showed thirty seconds of it; the agent
is a tab beside the inspector, or the deliver panel, in every one, so a proposal
that lands has somewhere to appear) and `sanitizeLayout`, which turns a stored
layout into one that can be trusted (known panel ids, each shown once,
titles/minimums re-taken from the registry, floating groups dropped) or `null` so
the preset is used. It also **migrates** a layout saved when the media bin and
transcript were panels of their own: the first of `media` / `bin` / `transcript`
found becomes `library`, the others drop, and an emptied group or branch is
pruned (a branch left with one child collapses into it). What is stored is
**`Settings.workspaces`** (`workspaces.ts`, bun-tested): `{active, layouts:
{<workspace>: <layout>}, offered: {<workspace>: [<panel>…]}, library: {tabs:
{<workspace>: <tab>}, collapsed}}`, parsed field by field — one bad layout costs that workspace its arrangement, not the
other four — with the old `layout` becoming Edit only when there is *no*
`workspaces` value at all (a reset Edit must not be brought back by a layout from
the old build), and the old single `library.tab` becoming the active workspace's
own. `settings.svelte.ts` holds it as the live copy and writes it through
`single-flight.ts` — **one write in flight, newest wins**, the follow-up reading
the state when it starts (a dock save, a rail click and a switch overlap, and two
racing writes could leave the older on disk; bun-tested). `workspace.svelte.ts`
is the runes singleton: `switchTo` keeps the arrangement being left, swaps the
layout and shows the library tab that workspace last had (each remembers its own;
until one is picked it is the workspace's tool: Color → Effects, Audio → Audio,
Motion → Titles — titles are added there, not in the Inspector —, Edit and Deliver → Media; a tab the user picked
in a workspace is stored and kept, only the unpicked default moved), and **touches no project state**
— cut, selection, playhead and playback are `editor` / `ui` and survive (dockview
rebuilds the panels, so a panel's own scroll starts over). **A layout is written
only if it was rearranged**: dockview reports layout changes for a great deal
that is not one (a restore, the library folding, a click that moves the active
group, a window resize), and writing each marked every workspace merely visited
as customised and brought a just-reset one straight back. So after a restore the
singleton waits two frames for the layout to settle, takes that as the reference,
and `shouldPersistLayout` (pure, bun-tested) writes only a layout that
`sameArrangement` finds different from it — same groups, panels and order, same
*shares* of each branch within 0.4 % (a dozen-pixel nudge of a sash counts; pixel
sizes, the active group and the active tab do not) — and, with no entry yet, from
the preset. What was written becomes the new reference; Reset clears the entry
and leaves none. **A stored layout is a snapshot, so each one is stored with the
panels its preset offered when it was saved** (`offered`, written by
`withLayout`; `readWorkspaces` / `adoptPanels` bring each one up to the preset of
the running build as it is read). A panel the preset opens now that the layout was
never offered and does not hold is new to it — the mixer in Audio, which a layout
saved before it would otherwise never get — and `insertPanel` (`layout.ts`, pure)
puts it where the preset does, reliably and in order: tabbed with the panels it
shares a group with in the preset; else in a group of its own beside the nearest
panel it sits next to there, taking the share the preset gives it out of that row
(only when the row runs the same way); else as a tab beside the preview. A layout
is never reset to make room, a notice says what was added (`describeAdopted`), and
the result is written back once. A panel the layout *was* offered and lacks was
closed by the user and stays closed. A layout stored before the record is taken to
have been offered `UNSTAMPED_OFFERED` (the presets of that time: all but the
mixer); if it is only a copy of today's preset — the build that wrote every
workspace the user merely visited left one for each — or of the earlier Audio
preset (`EARLIER_PRESETS`, checked with `sameArrangement`), it is dropped and the
workspace is its preset, today's. **Reset workspace** (`workspace.reset()`,
Window menu) forgets the arrangement, its record and the library tab picked in that
workspace (not the rail's fold, which is the rail's), drops a save still on the
debounce, rebuilds the dock from the preset and **says what it did** — including
"already in its default arrangement", because a workspace that never moved looks
the same afterwards and a reset that shows nothing reads as a broken one (that was
the report: Reset on a workspace whose stored layout merely equalled its preset).
It also reports a dock that takes neither the stored layout nor the preset
(`could not arrange`), which used to be silence. **Reset all workspaces**
(`resetAll()`) does that for the five. A window resize can move shares too (where a group's minimum
binds), so a `ResizeObserver` on the dock host writes what was pending, ignores
the layout events the resize causes, and retakes the reference once the window has
held still for 150 ms — otherwise the next unrelated event, a click on a tab,
would write a layout nobody arranged. After
every `fromJSON` it forces `api.layout()` at the host's real size: a layout
is built at the size it was saved at and the dock learns its real one a frame
later, and a constraint changed in that gap makes dockview re-split the whole
grid evenly. `+page.svelte` mounts the dock only once the settings are loaded so
it restores rather than rebuilds. Panels own no width: their roots are
`flex:1;min-height:0`, and a panel's minimum comes from the registry.
The **library** (`LibraryPanel.svelte`) replaces the old Media | Transcript tab
group with an icon **rail** (36 px icons in a 40 px column, tooltips and
`aria-label`s, a roving-tabindex tablist: arrows move focus, Enter / Space / click
choose; a pointer click does not leave focus on the rail): Media (`MediaBin`, whose
decoded thumbnails live in `thumbnails.ts` rather than the component — the library
remounts it on every tab switch, unfold and workspace switch, and each remount
used to decode every asset again; it keeps answers, including "no frame", but not
a failed decode, which is retried on the next mount), Titles (`TitlesControls`), Effects (color looks +
video effects), Transitions (the grouped picker), Audio (audio effects + the
voiceover and music entry points), Transcript (`TranscriptPanel`). **The Media tab's header is the one place for the count and the
ways in**: `MEDIA · 3` with the music, voiceover and import buttons (`importMedia`, `import-media.ts`; the import tooltip carries
`file.import`'s key from the keymap) — the bin used to stack the dock tab, `MEDIA` and `3 ASSETS`. **A bin row is
compact**: name and duration on one line, the spec in 11 px under it, then the proxy badge, one **analysis pill**
(`3 of 5 analyzed`, the kinds one per line in its tooltip: `analysisMeter`, `analysis-steps.ts`) and the `×2` (its tooltip
says "Used in 2 clips"). A kind the file cannot give — scenes of a song, speech of silent footage, anything of a still — is not
drawn (`applicable` / `chipsFor`) unless it ran, is running or failed; the Inspector's header keeps the five tags, minus those.
The row is a `role=button` with `aria-pressed`, takes Enter *and* Space, and its facts moved to the Inspector (below), so the
right-click menu is the actions. The **tile grid** of the Effects and Audio tabs is `minmax(96px, 1fr)` `Tile`s (one line, an ellipsis
and the tooltip when a name is longer — "Compressor" ran into its border at 76 px); the **Titles** controls are a 2-column grid of
equal `Btn sm` (`+ Title` / `+ Lower third` / `+ Caption` / `+ Text`; `Generate captions` or `Regenerate captions` with a trash icon for Clear (the two caption buttons share a row that stacks when the panel is narrow),
`Import`, `Write a voiceover…`) under `ADD TEXT` and `CAPTIONS` overlines, the two caption looks being the only chips.
`Reset` is a ghost `Btn` on the heading line of Color looks, never a chip among the looks. The Transcript marks the playhead's line with a
2 px accent bar and lights a line under the pointer, and its empty states are `EmptyState`s with `ProgressBar`s. Effects, Transitions and
Audio act on the selected clip and say why they are off when none is; they own no
value (a look is a `Color`, an effect an entry in the clip's chain), so the
Inspector stays where you tune — the presets are `effect-presets.ts`, shared with
its pickers. Titles (`TitlesControls`, over `title-actions.ts`) is the one panel of
title controls; the caption look it shares with the lane's menu is `ui.captionStyle`.
**One place per action.** The Inspector is about the selection and creates nothing:
the Library adds (titles, looks, effects, transitions, audio), the timeline places and
trims, the Inspector tunes what is selected, and the menu bar (entries from the keymap
registry, so each carries the user's key) keeps the common adds reachable when the library is
folded or closed — *Edit › Add title at playhead* (`title.add`, ⌘T / Ctrl+T, also the
titles lane's +), *Generate captions* (`title.generateCaptions`, also the lane's
right-click, with *Clear captions*) and *File › Import captions…*; animated titles are also the
lane's right-click. The styled titles (lower third, caption, text), the caption look and the
templates are the Titles tab's alone (the rail and *Window › Library* bring it back). The Inspector used to
carry the whole Titles controls as a *Titles lane* section under every selection, the
look chips in its Color section and *Add text overlay / Generate captions* in its
right-click; each duplicated the Titles tab, the Effects tab or the lane and is gone
(`panels-polish.test.ts` holds it). Context menus stay: they are the selection's own
actions where the pointer is. Clicking the
active icon **folds** the content to the rail (`library.collapsed`, persisted and
shared by every workspace — the rail is a tool, not part of an arrangement) and
the panel gives its width back: the registry minimum is the rail's 40 px, an open
library raises its group's to 240 and a folded one pins min = max = 40 through
`group.api.setConstraints` — a group's explicit constraints win over its active
panel's minimum, which is set when the panel is created, and the *panel*-level
`setConstraints` has no listener on a dockview panel and does nothing — hides the
group's tab strip, and hands the width it frees or takes to the group beside it
(dockview would give it to the last group in the row). Folding from the header's
chevron by keyboard moves focus to the rail's active tab, since the chevron
unmounts with the content. A library sharing a group with another panel cannot
fold — it has no width of its own to give back.
**Detached panels** (`popout.svelte.ts` over dockview's popout groups; the backend half is
`popout.rs`, see kerf-app). Any panel can go into a window of its own for a second screen:
**Window › Panel windows** (a tick per panel, in a window while ticked; *Return all panels to the
editor window*, `window.dockAll`) or the tab's right-click menu (*Move to new window* / *Return to the
editor window*, dockview's `getTabContextMenuItems`). The editor window keeps at least one panel
(`detachBlocked`, pure; the menu entry is greyed out with its reason). A window that failed to open
gives its label up (`#failed`), unless dockview refused the URL before the backend saw it. Every open goes through `PopoutState.#open`, which **announces** the window
(`popoutExpect`) and hands the label it gets to the window that opens (`LabelBook`, a FIFO: `window.open`
carries nothing to say which announcement it is for, so opens are serialised and the backend hands them
out in order); closing a window is dockview's own (it re-docks the panels where they came from) and
the *Return* entries just close the window. What it gives a window beyond dockview's: the live theme
(`mirrorRoot` copies `<html>`'s class and inline custom properties from a MutationObserver, otherwise a
window shows the stylesheet's defaults whatever the theme), a title (`Library — Kerf`), a slider-fill
observer of its own, **`inert` while a modal is open**, and the shortcut handler (`windows.listen`).
**The rule for panel code: `window` and `document` are the editor window's, and an event that happens in a
detached window is dispatched there and never reaches a listener on this one.** So `windows.svelte.ts`
(the registry; `version` moves when a window opens or closes or a panel moves) and `realm.ts` (pure:
`windowOf(el)`, `documentOf(el)`, `resizeObserverFor` / `intersectionObserverFor` — an observer made by the
editor window's constructor reports a detached window's element **not intersecting for good**) are how a
panel asks which window it is in; `window-events.ts`'s `onWindow({ pointermove, … })` is
`<svelte:window on…>` as an attachment that follows the element (a `capture` suffix is the capture phase);
`windows.listen` is for what is the app's (shortcuts, the click that dismisses a menu), heard in every
window. `beginDrag` listens on the window of its element. **Svelte registers delegated handlers
(`click`, …) on its own mount root**, so anything drawn outside a panel's root in a detached window needs
a root of its own there: the context menu is one `ContextMenu` per window (`contextMenu.win` is where
it was opened; mounted by `Workspace.svelte`'s `window` hook). The transport clock draws frames from
`windows.requestFrame` — the editor window's when it is showing, else a detached window that is (a
hidden window pauses its frames) — and reads `performance.now()` itself because a frame's timestamp
counts from its own window's start. **The choice is not final**: the registry owns the handle and a
frame waiting in a window that then hides or closes is asked for again where one shows — on each
window's `visibilitychange`, when a window is added or removed, and by one lazy 250 ms watchdog for a
platform that says nothing (a frame that outlives two ticks) — so the clock and the meters do not
freeze while the picture plays on the other screen. The library cannot fold while it has a window to itself. **Layouts
keep their windows**: `sanitizeLayout` reads dockview's `popoutGroups` (a window of one group or a
nested layout) through the same walk as the grid, so a panel is still shown once; the page is always
the popout page; a place that is not numbers is the platform's choice; the group a popped-out group
leaves behind in the grid — empty and hidden, holding the place its panels return to — is kept only while
a window points at it; `sameArrangement` counts each window's panels and place (12 px of slack for the
title bars a platform adds), so a window the user moved is written and one the platform nudged is not.
Restoring a workspace with windows **announces them first** (`announce`, in order — dockview restores
each from a timer) and then builds the dock; saving waits until `popoutRestorationPromise` (8 s at most)
so the windows opening are not taken for a rearrangement, and `settle` forgets any announcement nobody
took. Each workspace has its own windows: a switch closes one set and opens the other.
**The Preview panel in a window of its own** measures from there: the transport bar's width
(`transport-bar.ts`) is read with the panel's window (`windowOf`, its `requestAnimationFrame` and
`resize`, an observer of its own realm, read again on `windows.version`). The GPU preview's surface and
bounds belong to the editor window, so a Preview in a popout is routed to the JPEG (`routePreview`'s
`detached` input, `describeWhy`: "the panel is in a window of its own"), the bounds observers stand down
and the surface is hidden by the cleanup that reports the panel gone; docking it back resumes them.
**The chrome is a title bar over the dock, and the menu bar is in it.** There is no
toolbar row and no rule between the title bar and the dock: the dock starts where
the bar ends. The window keeps its native decorations (`tauri.conf.json` sets none
of its own, so the OS caption buttons are above Kerf's `TitleBar`, which is a row of
content — `-webkit-app-region` marks it draggable and every control `no-drag`);
custom window decorations, which would put the menus in the caption bar like VS Code,
are a possible follow-up and not this change. `TitleBar` is three cells (so the
workspace tabs stay on the centre line): the logo and the **menu bar** on the left;
the workspaces in the middle; the project's name with its Saved / Unsaved badge, an
**Export** button (a primary `Btn`, `file.export`'s key in its tooltip: the app's goal had no button outside the Deliver
workspace; off, with the reason as its tooltip, while the cut has no clips — `exportButtonState`, and `aria-disabled` rather than `disabled` so it stays focusable and the keyboard reaches the reason; File › Export… is greyed with the same reason and Ctrl+E answers it in a toast; while a render runs the button shows `42%` with a spinner, reopens the dialog and is on), the settings gear, the
notification bell and the version chip on the right. A pending agent proposal adds a cyan **Review 4 changes** chip
left of Export (`proposalIndicator`, `agent-status.ts`; it is its icon and count in a right cell narrower than 560 px — `reviewChipText`, 1366×768 has 513 — so the project's name keeps its room) that raises the Agent panel, and a count on the Agent dock tab
(`Workspace.svelte` sets `data-pending` on the tab's label, `dockview-kerf.css` draws it; the tab's `aria-label` becomes `Agent, 4 changes to review`: `agentTabLabel`) — in
Audio and Deliver the agent is only a tab, and a proposal there was invisible. The bell's badge sits off the corner with a
ring in the bar's colour, and a red one carries a warning triangle before the count so it is not colour alone (a mono `!` read as a `1`). The version chip
still turns amber when an update is waiting, which is why it stayed out of Help; Help also
has *Check for updates…*. The OS window's title is `• interview.kerf — Kerf` (`window-title.ts`; the dot leads while there is
unsaved work), set through `setWindowTitle` and the `core:window:allow-set-title` capability. The project's path moved to the status bar. **The menus**
(`MenuBar.svelte` over the pure, bun-tested `menus.ts`): *File* (New, Open…, Save…,
Import media…, Import captions…, Compose music…, Export…, Save cover frame…, an **Export as** submenu — Export captions…,
Export OTIO…, Export EDL…, Export markers as CSV…, Export YouTube chapters…, the last two off until
there is a marker (`MenuState.hasMarkers`) —, Settings, Quit — Save is
"as…" once the project has a file, since a saved project is its own SQLite file and
`save_project_as` is the only save there is), *Edit* (Undo / Redo, Cut / Copy / Paste /
Duplicate, **Split at playhead**, Delete / Ripple delete / Select all, **Tool**, **Clip** and
**Roll, slip, slide** submenus — the five tools as radios, trim / detach / link, and the
one-frame roll / slip / slide keys — **Add title at playhead** and **Generate captions** (off until the cut has a clip),
Ripple mode and Snapping as ticks, Customize keyboard shortcuts…), *View* (zoom, **Track height**, Overview strip, Safe-area guides,
**Delivery frame**, **Workspace**), *Window* (every panel as a tick, Reset <workspace>
workspace, Reset all workspaces — which asks first when any arrangement is stored),
*Playback* (Play / pause, Go to start / end, Go to timecode, back and forward a frame and a second,
Shuttle J / K / L, Set / Clear in and out, markers — the transport lives in the Preview
panel and a panel can be closed, so the whole of it is here as registry actions with
their bindings), *Help* (Keyboard shortcuts… — the cheat sheet —, Customize keyboard shortcuts…, Check for updates…, Release notes, Show log folder,
About). **An ellipsis promises a dialog or a picker and nothing else** — `Settings…`, `Keyboard
shortcuts…`, `Reset all workspaces…` (it asks), `Import media…`; an action's name in a sentence is
`plainLabel()` (`keymap.ts`), so the shortcut recorder says "already used by Open project", not
"Open project…." (`copy-guard.test.ts` holds both rules). A menu bar is text-first: **no entry carries
an icon** (a ragged icon column read as a rendering bug), only the check column of ticks and radios; the
key beside an entry is `Kbd` (`.k-kbd`, `accel.ts`: 11 px mono, muted, a punctuation key in a small
keycap, an arrow a size larger) — the same look as the context menus and the tooltips. **Every entry names a keymap action**
(`file.importCaptions`, `file.saveCover`, `app.quit`, `tool.snap` — default `S` —,
`view.minimap`, `view.safeAreas`, `workspace.<id>`, `window.resetWorkspace` /
`resetAllWorkspaces`, `app.keyboard` / `checkUpdate` / `releases` / `logs` / `about`
were added, the new ones unbound by default so a build never takes a key) and runs
the page's own `run` handler for it, so a menu and its shortcut are one piece of
code and the key printed beside the entry is `settings.shortcut(id)`, the user's. What
is one of a family and has no key (a delivery shape, a track height, a panel) is a
`MenuCommand`, run by `menu-commands.ts` (`setDeliveryPreset` is shared with the
timeline's own picker). Quit asks about unsaved work like the window's close button and
ends in the same `destroy` that guard ends with (`quitApp` in `api.ts`) — no new
capability. **Keyboard**: the ARIA menubar pattern with a roving tab stop; a click
opens a menu and, once one is open, hovering another title switches to it (the click
that finishes that hover does not close it again); ← → move along the bar and between
open menus, ↓ ↑ Home End move in a menu, → opens a submenu and ← closes it, Enter /
Space run, Esc closes one level and then leaves the bar, a letter jumps to the next
entry that starts with it (`stepFocus` / `typeahead`, pure); **Alt on its own, or F10,
focuses the bar** — F10 not when the user has bound it, neither while a field is being
typed in. Alt counts only as a tap (`alt-tap.ts`, a pure state machine, bun-tested): it
is never armed while a pointer button is held (the timeline reads Alt live as its
"leave the links alone" override on a drag, a trim or a razor cut, and an Alt pressed or
released mid-drag must not take focus — a stuck press whose release was missed is cleared by
the next move with no button down), another key, a pointer press or a wheel turn between
its down and up cancels it, and so does the window losing focus (Alt+Tab). A menu title
takes focus on a click (WKWebView does not focus a button a click lands on, and Esc
needs focus inside the bar), a pending hover timer is cleared when the pointer moves to
another title, and a chord that closes a menu gives focus back to where it was before the
bar took it. Menus are
`menu` panels of `menuitem` / `menuitemcheckbox` / `menuitemradio` entries
(`aria-checked`, `aria-haspopup`, `aria-expanded`, `aria-disabled` with the reason as the
title); a disabled entry still takes focus. Panels are `fixed`, placed from the rect of what
opened them and kept inside the window (a submenu flips to its parent's other side and,
failing that, slides in over it; a menu taller than the room scrolls — nested submenus are
not positioned inside a scrolling panel, which would clip them). Too narrow for its six
titles the bar becomes one "Menu" button whose entries are the menus as submenus: the
test is the width of the title bar's **left cell** (`MENU_FULL_PX`, 320 — the bar and the
logo measure ~331 at their widest, and the workspace tabs and the right cluster take their
share of the window first), not the window's width, so a ~1000 px window has them in full and
the 960 px minimum collapses them. A modal closes the menus and the bar is `inert` behind it like the rest
of the page; a letter or arrow typed in a menu never reaches the page's shortcuts (the page
returns early for events from inside `[role=menubar]` / `[role=menu]`, and the bar
`preventDefault`s what it takes); a chord closes the menus and goes on to the page. The
tokens are the guarded pairs (`text-secondary` on `surface-app`, `text-primary` and
`text-muted` on `surface-raised` / `surface-hover`, `kerf-400` for the tick), so the
contrast guard covers it. **Where the old toolbar's controls went**: the transport (go to
start, play / pause, go to end, the timecode with the timeline's fps; J / K / L stay on
the keyboard) is the **Preview**'s own bar, which sheds the duration and the rate below
~380 px of its *measured* width (`transport-bar.ts`, bun-tested: an unknown or 0 width is the
full bar, and the buttons, timecode and scrub bar never go) — see **the Preview frame and its
scrub bar** below; the tools, Ripple, Snapping,
Undo / Redo and the delivery-frame picker are the **Timeline**'s toolbar (every button a `ToolbarToggle`), which wraps to a
second row rather than clip
(its "Timeline" caption is gone — the dock tab says it); New / Open / Save / Export and
Panels are the File and Window menus. `menus.test.ts` holds each of those paths, and
that no entry of the old toolbar lost its place.
The **Deliver panel** (`DeliverPanel.svelte`) docks the export dialog's platform
check and *Deliver to* shapes, extracted into `Readiness` / `DeliverTo` /
`SectionHead` which the dialog uses too — no fork. The shape choice and the
smart-crop toggle live on `ui` (`deliverShapes`, `deliverSmartCrop`) because both
places edit them (session-global, so a shape ticked in the panel is ticked when the
dialog opens), and `Readiness` re-judges on every `editor.timeline` change
(docked beside a timeline being edited, a verdict cached at tick time goes
stale), keeping the newest answer. Shapes and the platform check are for a picture, so the panel hides
them, and the button never says "Export N files", for a cut with no video clip
(`hasPicture`, the same gate the dialog uses). The render still goes through the full dialog
(`ui.openExport()`); the panel shows its progress and Stop while one runs.
A picture and its linked sound are **one selection**: the Inspector's "N clips selected" notice counts
link groups (`selectionUnits`, `link-ui.ts`) and a linked clip carries a quiet "Linked to A1" chip beside
its track badge instead. The `Inspector` is **mounted whether or not a clip is selected** and shows the
selection: a title, an animated title, a clip, else the library asset selected in the bin,
else an empty state that says where titles are added. Its sections are `InspectorSection`s — native `<details>`
disclosures, so they are keyboard-operable for free — each with a one-line
**summary** on the right (`1.00×`, `Cropped`, `2 keyframes`) so a collapsed
section still says what it holds; Timing and Volume open by default,
everything else is folded. It edits the selected clip —
trim, volume, fades, speed, transform, color, **curves / color wheels / LUT** (`GradeSections.svelte`: a canvas
curve editor per channel — click adds a point, drag moves it, double-click or right-click removes it, one `set_grade` on
release, Escape abandons via `beginDrag`; a click that only adds a point or puts a wheel's puck down waits 300 ms for
a double-click, which undoes it, and a gesture that changes nothing writes nothing; three hue discs painted from `grade.ts`'s `wheelHue` with a lightness
`SliderRow` each; a `.cube` picker, a Clear button and an intensity slider, with `inspect_lut` saying the size or why the
file is not applied. Curves are drawn with `grade.ts`, the TS mirror of the engine's cubic; the preview shows the
grade through FFmpeg because the GPU preview refuses a graded clip), **transition** (a grouped picker
over `src/lib/transitions.ts` — fade / slide / push, then a direction, because
that is the order the choice is actually made and a flat list of eleven names
hides it; its bun test pins the ids against `TransitionKind::ALL`), plus **video / audio
effect chains** (add / tune / remove), **keyframe animation** (the Transform panel
auto-keyframes at the playhead and shows the sampled pose; each key but the last has an
**easing** picker in the Animation section — linear, ease in-out / out / in, hold and three
own bezier presets, `EASING_CHOICES` — which writes `set_keyframe_easing`; **colour and volume
animate from the same panel**: a ◇ beside each colour number and the volume keys it at the playhead at
the value it has there (◆ when a key sits there, and clicking it takes the key out; the last one makes
the number static), and once a number is keyed its slider shows the curve at the playhead and an edit
writes a key there — `setCol` / `setGain` / `keyAtPlayhead` over `set_property_keyframes` with the
mirror's `upsertKey`, so a drag is one edit (every number row is a `SliderRow`, below). The Transform sliders do the same for numbers keyed on
their own, and so do the **mask's** five numbers (`maskAt`, `setMaskNumber`: a key at the playhead while keyed, else the
static mask; a live drag of a keyed one is not drafted, `maskDraft` returns null as `colorDraft` does). The Framing section's
**Crop motion** rows (Zoom, Pan X, Pan Y — the animated crop, a window of the picture inside the static edges above) have no
static value: an edit there is always a key at the playhead (`setCropNumber`), and the group's *Clear* takes every crop key out
in one edit (*Reset crop* is the static edges'); the section's summary names both (`Cropped · Crop motion`); the Animation list is still the legacy bundle's. **The dope sheet** (`DopeSheet.svelte`, panel id
`keyframes`, titled *Keyframes*; in the Motion preset between the preview and the inspector, `EARLIER_PRESETS.motion`
is the arrangement before it) shows the selected clip: one row per keyed number (`dopeRows`, the resolver's keys, so a
bundle-driven transform number has a row too), its keys as diamonds on one axis over the clip's span (and past it, shaded,
when a key lies beyond the end), labelled in the cut's time with the timeline ruler's steps (`rulerGrid` / `tickLabel`);
a diamond is round when its segment is eased and square when it holds. Click selects a key and seeks to it (exactly),
Shift / Ctrl add or toggle, a drag on empty lane space is a marquee, the ruler seeks and scrubs (Escape puts the playhead
back). Dragging a key retimes the selection across rows: the grabbed key lands on a magnet within 6 px (the playhead, the
clip's ends, a key that is not moving, `dragMagnets` / `snapShift`) or else on a frame of the cut's grid (not the clip's,
which is off it when the clip starts between frames), and nothing goes before 0; **Alt at
the release** (the ghost follows Alt live) leaves the originals and drops copies; a key landed on gives way. While the
panel has focus the editor's own Copy / Cut / Paste / Delete / Select all bindings (`settings.actionFor`, so a rebound
key follows; `dopeKey` routes them, a held key repeating only where the registry allows) copy the selected keys (spacing
kept, the clipboard outlives the panel and works on another clip), cut them, paste them with the earliest at the
playhead, delete them (a row left empty is static) and select every key, and ripple delete / duplicate / split are
swallowed rather than reaching the clip behind the panel; Escape clears the selection. **The easing popover** (`EasingPopover.svelte`, from the bar's *Easing* button — which toggles it — or a diamond's
*Easing…*; drawn in the document's body because a dock group is a containing block, and so listening for its keys and
clicks on the box itself: Svelte delegates a detached panel's events from the panel's mount root, which the body is not) shows
the twelve presets of `EASING_CHOICES` (eight before, now also Glide / Settle / Lift / Whip — the Inspector's picker
lists the same) as curves drawn from the renderers' polyline (`easingPath`), lights the one the selected segments share,
and shapes the segment each selected key starts (`easingTargets`: a row's last key starts none); arrows move between
presets, Escape / a press outside / blur / scroll close it without writing. **Every gesture is one
`set_property_tracks` edit** of only the rows it touched (`retimeKeys`, `deleteKeys`, `pasteKeys`, `easeKeys` in the pure,
bun-tested `dope-sheet.ts`; a gesture that changes nothing writes nothing), shown as its ghost until the edit settles,
and Escape / pointercancel / blur abandon a drag (`beginDrag`). Keys are named by number and time, not index, so the
selection that comes back is where the gesture put them. The preview of a proposal is read-only here too
(`holdForPreview`). The preview's Web Audio gain follows a keyed volume — `gainAutomation`
ramps linearly through the curve's points and the fade edges, and **steps where the curve does** (a hold
or two keys at one time: the ramp arrives at the value the step leaves and a `setValueAtTime` lands the
next, read `1e-9 s` either side of the step because `(start + t) - start` can fall an ulp short of `t`),
as the export's `if(lt(t,..))` does — but the timeline's volume line and
waveform scaling still draw the static gain), a **Framing** section
(a `Smart crop` button that frames *this* shot for the delivery frame, plus
`Reset crop`, above the crop sliders it writes — greyed out with a reason when the
shot already matches the frame or is 360), a **Mask** section (pictures only — as is every picture section, Transform to Animation: `picture` is a
picture asset *on a picture track*, so the sound half of a linked pair has none; None / Rectangle /
Ellipse chips, then centre / size / feather / invert; picking a shape starts from
a visible default rather than a collapsed one, and the caption carries the recipe
the shape alone does not suggest — a lower track shows through, so a blurred face
is a duplicated, blurred copy above, masked), a **360 reframe**
section (yaw / pitch / roll / FOV, auto-keyframing
at the playhead like Transform — note its `lerpAngle` takes the shortest arc, which
plain `lerp` would read as a 340° swing across the seam; a source Kerf did not
detect as 360 has no such section — its projection picker, which marks the whole asset via
`set_asset_projection`, is the collapsed **Advanced** section, below), and an always-visible
**Text overlays** section (add titles / lower-thirds, caption the whole cut in
a **Lines / Word punch** style chosen by two chips above the button — the
selection is deliberately *not* derived from the overlays already there, since
a caption's style is not recoverable from its text and guessing it from the
word count would flip the chip whenever a sentence happened to be short —
the button relabels to `Regenerate captions` once there are generated captions, since a
later trim moves the words out from under them, with `Clear` beside it taking
only the generated ones (imported ones included) — and edit text / timing / position / size / color /
box / bold).
**What the engine could always do and the interface did not reach** (`ops.ts`): *Export transcript as .srt…* (the library's menu, the asset's Inspector panel and the transcript's header; off with a reason until the asset has a transcript; the desktop app asks where through `pickExportPath('srt', name)` and `export_srt` writes it, the harness builds the same text with `toSrt` — the faithful mirror of `transcript_to_srt` — and hands it to the download manager), *Append all N files to the timeline* (the library's menu: `concatenate`, one revision, a picture on the first picture track and a sound on the first sound track) and **Copy / Paste keyframes** (the clip menu and the Inspector's Animation section: `editor.keyframeSource` remembers the clip, so a paste reads the animation as it is then; `keyframeOffer` says why each is off; `copy_keyframes` with no offset keeps the keys' distance from the clip's start).
**A control that is busy keeps focus.** An edit in flight (or the proposal on screen: `editor.locked` = `busy || previewingStaged`) used to switch the Inspector's controls to native `disabled`, which drops keyboard focus — the key after the edit went nowhere. `Btn`, `IconBtn`, `Chip`, `Tile` and `CheckRow` take **`pending`**: `aria-disabled` (drawn as off by the same rules as `:disabled` in `controls.css`, not hovered) and a swallowed press; text and number fields are `readonly` + `aria-disabled` (`off` in the Inspector), selects and the colour box swallow the press that would open them, and `SliderRow`'s number box is read-only. A real reason (nothing selected, no picture) still uses `disabled`. `Btn` and `IconBtn` set `aria-disabled` *after* the caller's attributes, so a caller's own `aria-disabled` cannot undo `pending` (the Inspector's Copy / Paste keyframes used to: `pending={editor.locked || offer.copy.disabled}` now). A select that is only pending still takes the keys that change it, so its handler writes nothing while `editor.locked` and puts the control back to what the cut has (`patchTitle` / `patchSvg` / `setEasing`, the colour boxes through the same two; `review-readonly.test.ts` scans). `pending-controls.test.ts` keeps `disabled={editor.busy}` out of the Inspector, effects, transitions and titles. An export no longer raises `busy` (`exportRun` is the status bar's and the dialog's; only a multi-format export's framing pass, which writes to the clips, blocks) — an export that takes minutes must not freeze the editor — and New / Open decline while one runs.
**The Inspector's finish.** Every field is one height (`--control-sm`, 28 px): numbers in mono and right-aligned, words in the sans
(a title's text was typed in the number face), and a field of seconds says so with an `s` in the keyframe slot beside it. A section
header is 32 px (it was 49) with a flex summary that ends in an ellipsis beside the title, and its summary says what it holds: Timing
`Start 0:00.0 · Dur 0:12.5`, Color `None` for an ungraded clip (it read `Custom`), the look it matches, `Custom`, or `Animated · 2 keys`
(`inspector-summary.ts`). **One formatter for a time** (`time-format.ts`, pure and bun-tested; `timeDisplay` in `time-display.svelte.ts` is the reactive singleton every readout calls): a *mode* — `timecode` (the default: `HH:MM:SS:FF`, frames counted at the rate it is asked in — drop-frame `HH;MM;SS;FF` at 29.97 / 59.94, `formatTimecode` / `timecodeFrames`; the ruler's labels and `TimecodeField` follow `timeDisplay`) or `seconds` (`m:ss.d`, what the review card prints) — over the timeline's playhead and readouts, the transport, the scrub bar, the Inspector, the titles list, the status bar's `Cut`, the export range, the Deliver panel, the ruler's labels (`tickLabel(k, grid, fps, mode)`: `m:ss` on a whole-second step, the fraction to the millisecond below one), the Mixer's measured range (`scopeLabel(range, duration, fmt)`) and the Stems / Fit-music dialogs (`describeFit(report, fmt)`). Settings wires the mode (`timeDisplay.mode = …`); nothing else sets it. A bin row keeps its `m:ss` duration, the platform check its `m:ss` (it states the same length as the limit messages the backend writes), and the review card's `m:ss.d` is a mirror of the backend's phrasing (`diff.ts`) — a sentence the backend wrote is not re-spelled here. **A field is edited, so it shows what can be typed back**: the Inspector's In / Out and a title's Start / End are text fields (`timeRow`) showing timecode (or decimal seconds to the frame, never `4.633333`) and taking either spelling — `12.5`, `375f`, `0:12.5`, `00:00:12:15` (`parseTime(text, fps, mode)`; **a bare colon time is read the way the mode prints one**: timecode mode `1:05` is a second and five frames (the last field is frames, a field past the rate is refused), seconds mode it is a minute and five seconds (`M:SS` / `H:MM:SS`, `75:10` is minutes, `1:75` is refused); four fields are timecode and a decimal on the last is a clock, in both; the refusal's toast says what to type in the mode, `timeHint`) — put on a frame (`parseFrameTime`). In / Out count frames at the *source's* rate, not the cut's. The Source range read-only row went: it said what In and Out already show. A ◇ is amber and outlined when the
number is keyed somewhere, filled when a key sits at the playhead, and its tooltip names the row (`warmth`, not `temperature`). Crop is
`Left / Right / Top / Bottom`, the mask `Center X / Y`, the title's `Position X / Y`, the section `Transition` with an `Into clip` select.
**Footage Kerf did not detect as 360 has no 360 section**: the override (a source select and `This is 360 footage`) is a collapsed
`Advanced` section at the end of a picture clip's list, summarised `Not 360`; detected or reframed footage keeps its `360 reframe`
section. A title's Color and Box are a **swatch (native colour input) plus the free-text field** (`title-color.ts`: the engine's own
spelling — a name, `#rrggbb[aa]`, `@alpha` — read for the swatch, and a pick written back keeping the alpha; a box with none starts at
`@0.55`). Reverse, Bold and Invert are `CheckRow`s, box first. The destructive pair is one row of two small buttons (`Remove clip`,
`Ripple delete`; Remove is red ink on the neutral edge — `data-quiet` on a destructive `Btn` — not an outline in alarm colour). **With no clip or title selected, a library asset selected in the bin is the Inspector's subject**
(`AssetInspector`, `asset-facts.ts`): name and kind, *Add at playhead* / *Append* as one row of two, then one section per
subject with its own actions — **Media** (the facts; the bin row keeps the spec line), **Analysis** (summary `2 of 5` /
`running…`, the analysis chips without the proxy one, failures and findings, *Analyze* and *Export .srt*), **Proxy** (its
facts, *Rebuild* / *Delete*) — and *Relink…* / *Remove…* last, the row a clip keeps its Remove in (Relink is the missing
callout's alone while the file is gone). The rows are what the bin's right-click menu used to list as twenty (it was 960 px
tall; it is the actions now, with Analyze as one entry that opens its own menu).
**Polish presets** (`src/lib/style-presets.ts`, pure data over the existing
surfaces): one-click **looks** — the library Effects tab's
Punchy / Warm / Cool / Faded / B&W chips (the active one highlights; the Inspector's Color
sliders show exactly what a chip applied, its summary names the look, and *Reset color* under them goes back to none) built on `Color.temperature`, a warm-cool
channel in -1..1 rendered as opposing `eq` per-channel gammas (`eq_filter` —
omitted at 0 so old graphs stay byte-identical; plain saturation/gamma can't
tint) — and the Titles tab leads with **Title / Lower third /
Caption** style buttons that create a styled overlay at the playhead with
fade-in/out opacity keyframes; the caption style matches what
`generate_captions` writes in its `lines` style, so manual and generated
captions look alike (`CAPTION_LOOKS` in the same file is only the two
generate-time labels; their numbers live in `captions.ts`).
Everything is styled with the CSS-variable tokens directly (inline `style`), not Tailwind
utilities — **except what has to react to the pointer, because an inline style cannot say `:hover` and
an inline background beats any stylesheet rule**. `styles/controls.css` (imported by `layout.css`) holds
the shared controls as classes with attribute hooks: `.k-btn` (`Btn`: `data-variant` primary / agent /
secondary / ghost / destructive, `data-size`; a filled button steps to its 600 shade hovered and 700
pressed, ghost / secondary step up the surfaces, disabled is a plain outline in `text-disabled` rather than
a dimmed copy of the live look), `.k-ibtn` (`IconBtn`, the title bar's gear / bell / version chip and the
library rail: `aria-pressed` / `aria-selected` / `data-on` is the on state, `data-tone="quiet"` the rail's
muted rest, `data-accent` the version chip when an update waits; `IconBtn` writes `aria-pressed` only when
it is given `active`, since a plain icon button announced as "not pressed" reads as a switch; both default to **`type="button"`** — a bare `<button>` inside a `<form>` submits it, and Enter in the Save-as-template dialog's Name field pressed "Add control" — so a form's own Save says `type="submit"`, here with `form="<id>"` from the modal's footer) and `.k-tab`
(the workspace tabs). Everything else that is a plain `<button>` or `role=button` and keeps its colours
inline gets one **veil** (`--hover-fill` / `--press-fill`, an inset `box-shadow` of `text-primary` that
paints over any fill, because inline styles never set `box-shadow`), except the timeline clips, menu
rows and dockview's own controls (its close X has a hover of its own; matched by class-token start, `[class^='dv-']` / `[class*=' dv-']`, not a substring), and a disabled one falls back to `opacity:.5` with a `default` cursor (not
`not-allowed`: a red circle on a greyed Undo reads as an error) — a control that must stay legible when off
sets `opacity:1` itself (the context menu's rows, the transcript's cut lines, which used to be dimmed twice).
Dock tabs hover and press in `dockview-kerf.css`; a tab strip is 30 px, a tab's close X shows on hover or keyboard focus (`:focus-visible`) rather than on
the active tab all the time — dimmed to nothing, not hidden, so it stays in the accessibility tree, and without a pointer until it shows (Preview, Timeline and Library were one mis-click from gone; right-click on a tab still offers Close), and
a sash lights at 65 % amber after 0.1 s over its 8 px grip. A workspace tab's amber marker is a bar inset from the pill's corners, not an inset
shadow that curled up its sides. **One keyboard focus indicator**: `--focus-outline`
(2 px `kerf-400`, offset `--focus-offset`) from a zero-specificity `:where(button, summary, input, select,
textarea, a[href], [role=button], [role=tab])` rule in `layout.css`, inset in dense strips (the menu bar, the library rail, which clips an outset ring, dock
tabs — dockview's own was a 1 px outline in the divider colour, invisible), and a bordered wrapper around a bare
`<input>` (`.k-field`: the agent composer, the keyboard search) carries it while the *input* has keyboard focus (`:has(input:focus-visible)`, so the Queue button beside it does not ring the field too), and the slider thumb draws the
same colour and width as a ring (`--focus-ring`); `outline-ring/50` is gone from the base layer. **Toasts
are in the theme** (`ui/sonner`): Kerf's scheme (not the OS's, since `ModeWatcher` is not mounted), an
offset above the status bar, the menu's surface / shadow / radius, a ghost accent action instead of
sonner's inverse pill, and — one list shared with the notification log, `status-look.ts`: a circle-check, circle-x, triangle, info — a glyph and a 3 px edge in the kind's `-400` hue, with a 12 % wash of it over the toast for success / error / warning (`layout.css`; `status-look.test.ts` holds the stylesheet and both components to the list, and the contrast guard holds the text on those washes to 4.5:1). **Titles are their own items, not part of a clip.** `Timeline.overlays` has always
been timeline-level, and the UI now says so: the Timeline has a **titles lane**
(`T`, above V1; `data-title-lane`) where every title / lower-third / caption is a
block from `start` to `end` (generated captions dashed and dimmer; overlapping
items stack into rows via `packRows`). Click selects (`editor.selectOverlay`,
exclusive with the clip selection; click also seeks into the title), the body
drags in time, the 6px edges trim, with the clip drag's snapping (0 / playhead /
beats / every clip edge / other titles), Delete removes it, and one
`update_overlay` is written per gesture. A selected title makes the Inspector show
**that title's editor** (text, timing, position, size, colour, box, font, bold,
keyframes) *instead of* the clip sections; adding and captioning are the Titles tab's,
the lane's + and right-click menu's and the Edit menu's. The **Preview** draws an interactive box over each
title visible at the playhead (hidden while playing): drag to move, corner
handles to resize (scales `size` by the pointer's distance from the box centre),
Escape / pointercancel / blur abandon it, pointer capture holds it, local state
updates live and **one** backend edit lands on release. The box is laid out in
the engine's units: `cqh` against a size container covering the drawn picture,
centred on `(pos_x, pos_y)`, font `size` of the height, browser text metrics
standing in for drawtext's (the family the engine draws: the title's `font`, else — also when that one is
not installed — the shipped Geist, `'Geist Variable'` here), `boxborderw` as padding. **The box is as tall as the
string's ink**, as `drawtext`'s `text_h` is (`H` 0.71 of the font size in Geist, `x` 0.53, `Hxg` 0.87), not the
font's line height (1.30): the single line's `line-height` is the ink, with no explicit `height` (preflight's
`border-box` would swallow the box padding), and `inkBox` measures it with a canvas (`actualBoundingBoxAscent + Descent` at 100 px, cached,
not remembered while the face is still loading), `FALLBACK_INK` 0.9 where there is no canvas, and shifts the
ghost's glyphs from the line box's centre to the ink's, as the engine centres the ink at `(pos_x, pos_y)`; the headless
harness reads 0.71 / 0.54 / 0.88 for `H` / `x` / `Hxg`. It is aligned to within font-metric differences.
A **keyframed** title follows the Transform convention:
moving it keyframes the position at the playhead (`editor.moveOverlay`, updating
an existing keyframe within 20 ms, else inserting one carrying the opacity in
force), because the static `pos_x/pos_y` is not what an animated render reads;
resizing always writes the static `size`. Pure logic (box math, keyframe upsert,
row packing, snapping, span trim) is `src/lib/titles.ts`, bun-tested.

**Transform handles in the Preview** (`TransformHandles.svelte`, pure math in `transform-handles.ts`). The selected clip's
picture at the playhead (paused, not streaming, no proposal previewed, no trim monitor; not on a locked track, not a 360
reframe) gets a box drawn **where the export puts it**: the canvas is `renderCanvas` (`export_format` without options — the
delivery frame, else the first picture of the rendered cut, even sides, the delivery's fit), the box is
`layer-geometry.ts`, a faithful port of kerf-core's `layer_geometry.rs` (crop / fit / second scale / rotate / overlay
rounding, chroma grids, `pix_fmt_subsampling`) replayed against a Rust-written corpus (`core-render-plan.md`), at the
clip's pose at the playhead (`transformAt`). A translucent picture of odd size, which the engine's geometry refuses for its
pixels (FFmpeg's RGB round trip), is boxed as if opaque: opacity moves nothing; only a picture nothing draws (an empty
crop) has no box. A click on a picture selects the topmost clip under it (later video tracks
on top) and can carry straight into a move. Body = move, corners = scale about the centre, the knob = rotate, edges =
crop — only under a Contain fit with position / scale / rotation unkeyed, and it moves the scale and position so the rest
of the picture stays put (the export re-fits and re-centres a cropped window). Snapping (on with the timeline's snap
toggle, **Alt** inverts it, 6 px) puts an edge or the centre of the bounds on the frame's edges and centre lines, the
title-safe box and the platform chrome when the safe-area guides are drawn, and the other pictures' edges and centres;
scale also stops at 100 %; rotation lands on a right angle within 5°, **Shift** turns in 15° steps and keeps a move to one
axis — the tooltip names both with `settings.modLabel`. While held, the box is an exact ghost of the pose (D9: the picture
under it settles on release) with a readout (`· key` when a keyed number is moving). **One `setTransformAt` on release**
(`set_transform_at`: a keyed number keyed at the playhead, the rest static — one revision even when position x is keyed
and y is not) at `keyTime` — the unrounded clip-local playhead, the moment the pose is sampled at (a 1 ms grid can land
after it, where a hold still reads the old value); the Inspector's keyed writes use it too, so both hit one key. Only a
number whose value, rounded as the gesture writes it, differs from the start is sent, so the other axis of a Shift move
gets no key and a drag back to the start writes nothing; Escape / pointercancel / lost capture / blur abandon it.
The handles count as page overlays for `routePreview` (`handles`), so on an X11 child-window surface the frame falls back
to the JPEG while they show (a click on the surface itself never reaches the page there: select the clip in the timeline);
under a transparent webview they draw over the surface. Not done: separate X / Y scale (the model has one `scale`; it
needs a model, export-graph and GPU change), a live GPU picture during the drag (A3).


The timeline toolbar carries a **delivery frame picker** (also View › Delivery frame; Source / 16:9 / 9:16 / 1:1 / 4:5,
from `src/lib/delivery-formats.ts`, bun-tested; with a frame set the toolbar's menu adds *Footage: fill & crop / fit &
letterbox / fit & blurred fill*, `DELIVERY_FITS` through `setDeliveryFit` in `menu-commands.ts`, which keeps the frame and
changes `fit`; the export dialog's fit select offers the same three, `FITS`) that sets `Timeline.format` — the
preview pane then *is* that frame (sized with `100cqh` container units so a 1:1
frame is height-bound in a wide pane, not squashed), and for a vertical or square
delivery it draws **safe-area guides** (the platform's top strip / caption rail /
action column, plus a title-safe box; `settings.safeAreas`, **off by default**, toggled from
Settings › Preview or the preview context menu, which both write the same
persisted `Settings.safe_areas` — it is held process-wide in `settings.rs`
rather than in the engine, since nothing in kerf-core cares about it). The export dialog's "Source" resolution relabels to
**Project frame (WxH)** so the two surfaces cannot silently disagree. The dialog
is **preset → destination → picture → sound → deliver to → platform check** (the preset select is grouped Video / Social / Master / Audio, each described in a plain sentence of what you get and what it costs — `presetsByGroup`), with a
`Quality` select of three named CRF points (Smaller file / Balanced / Higher
quality, derived from the codec's CRF range) standing in for the encoder; the
codec, rate control, tune/profile/pixel format, audio codec and container knobs
all live behind one **Advanced encoding** disclosure, and a CRF typed there
reads back as `Custom` in the select. Loudness normalization sits beside
`Include audio` rather than in Advanced, because for the social-video user it is
a polish switch, not an encoder setting. It is a **select of targets** — Off /
YouTube −14 / Spotify −14 / Apple −16 / Broadcast −23 LUFS (`LOUDNESS_TARGETS`, all
−1 dBTP) — writing `ExportOptions.loudness`; the old `loudnorm` boolean is the YouTube
target and `loudness` wins over it exactly as in the engine (`loudnessChoice`), and
`withLoudness` clears the flag in the same move so Off cannot be turned back into YouTube
by a flag left set. The chosen target also reads in the summary line (`… · AAC 192k · -16 LUFS`).
With a target, **Keep the dynamics** writes `ExportOptions.loudness_static` (one measured gain and a
true-peak limiter instead of `loudnorm`; the hint under it says which). The Mixer's master FX block has a
**True peak** toggle beside the limiter (`setMasterTruePeak`, dimmed while the limiter is off; export-only —
the preview's limiter is the same either way, and `fxSummary` does not mention it).
The clip menu offers **Sync by audio** when exactly two clips are selected and one is right-clicked:
`audio-sync.ts` `syncOffer` greys it with the backend's refusals (no sound, speed ≠ 1, linked to each other,
both on one track, the clicked clip's or a partner's track locked); `editor.syncByAudio` moves the clicked
clip to the other (`describeSync` toast, and a **Move anyway** action — `force`, which applies the
measurement just made — for a match that was not reliable). A sync queued behind another job shows a
toast with Cancel (`onAudioSyncProgress` / `cancelAudioSync`; the cancelled rejection stays quiet). The harness answers as if both
recordings started together.
The options are remembered for the session (`export-draft.ts`: `ui.exportDraft`, written when the
dialog closes, restored by `restoreDraft` unless the project has been re-cut for another frame since —
a remembered 9:16 preset must not open on a 16:9 cut — and dropped by `ui.projectReplaced()` when a
project is opened or created), and a click on the scrim does not close the
dialog (Esc, Cancel and ✕ do). The frame-rate select is `frameRateOptions(fps)`: `FRAME_RATES`
(15 / 12 / 10 are the GIF's) plus the current value when the list lacks it, never a blank select.
The **Platform check** (`Readiness`, over the pure `platform-verdict.ts`) is for a *video
export of a cut with a picture* and says nothing otherwise: `judgeable` is false for audio
containers, a GIF and a cut with no video clip, and the verdict is `null` for a cut with
no length (the dialog and the Deliver panel both gate on it). With **no shape ticked** it is
one neutral line stating the file and where it already fits — "1920×1080, 16:9, 0:20. Fine for
YouTube and the web. Tick a shape under Deliver to for Reels, Shorts or TikTok." (the length rounded
exactly as the platform messages round it, so "3:01" never sits beside "3:01 is over …" for a 180.6 s cut;
the platforms are the ones whose check had no `shape` issue at that
frame, so a 1:1 cut is "fine for YouTube Shorts and Instagram feed" and never told it is
letterboxed) — and amber / red appear only for a *length* limit of a platform that frame is made for
(the reach warning, the hard cap, the minimum). A low resolution (one tip for the frame, naming every
platform that would upscale it, and the size they are made for) and the muted-feed captions tip are folded tips, never amber (the
captions tip is not given for YouTube). With **shapes ticked**
each file is a row ("9:16 · Ready for Instagram Reels · YouTube Shorts · TikTok", then its length
problems), judged only for the platforms that take that shape, and the single-file line is gone.
The one shape sentence — "Instagram Reels, YouTube Shorts and TikTok are vertical (9:16). This 16:9
cut would be letterboxed. Tick 9:16 under Deliver to to export a vertical version." — appears only
when the *project* is cut for a shape and the file the options write is wider (a 9:16 project sent
through "Web 1080p"), naming the platforms that frame is for; a landscape project exported 9:16 is
a reframe the user asked for and says nothing. The check is judged at the frame the render
will produce (`opts.resolution`, else the project's) and the length it will write (the marked In → out
when chosen): `platform_check(width, height, range)` answers a `PlatformReport` — the targets *and*
the `CutSummary` they judged, so the line states the file's real frame and length, not a second reading.
Rust summarises the range on the *sliced* timeline the export builds (`Project::cut_summary_in`), so the
frame, sound and titles are those of the span; the harness mirror (`exportSpan`, `api.ts`) clamps the
span and re-reads the titles over it, and keeps the sample's frame and sound.
There are no size rules: `kerf_core::platform` has length and shape limits only (TikTok's 500 MB / 2 GB
caps are prose in its `notes`), so a size warning would need an engine estimate first.
`kerf_core::platform` decides what each platform accepts; `platform-verdict.ts` only decides which of
its answers to show. `src/lib/platforms.ts` is a bun-tested
mirror used **only** by the browser harness, so the panel is drivable under
`bun run dev`. `src/lib/smart-crop.ts` is the same arrangement for smart crop: only
the *shape* arithmetic is mirrored (bun-tested), because the harness has no decoder
to sample with and so lands on the centre window — which part of the shot survives
is the half that only exists with media behind it. `src/lib/captions.ts` is the
same arrangement again, but *faithful* rather than approximate — captioning is
arithmetic all the way down, so the harness produces exactly the captions the
backend would (the mirror caught the two-captions-at-once collision the Rust
tests had not) — and `caption-import.ts` carries the subtitle parsers and
`captions.ts` `placeCues` the same way, which is how `importCaptionsText` (the
variant the harness and a file input use; `importCaptions(path)` needs the desktop
app) imports for real under `bun run dev`, with `describeImport` the toast line.
**Import captions…** is a button in `TitlesControls` (the library's Titles tab),
*File › Import captions…* and an entry in the titles-lane context menu
(plus one per clip the cut shows, up to three). The button opens an inline options
row — not a dialog, the controls live in a narrow column — and *Choose file…* runs
`importCaptionFile` (`title-actions.ts`): `pickCaptionFile` (desktop: the dialog
plugin, answering a *path* the backend reads with its guards; harness: an `<input
type=file>` read as text, opened before any `await` or the click is no longer a user
gesture) → `confirmAction` when generated captions exist ("Replace the 12 captions
already on the cut with those in x.srt?" — asked after the pick so it can name the
file, counted on `editor.liveTimeline`, the cut the edit lands on rather than a
proposal being previewed) → `editor.importCaptions` /
`importCaptionsText` → a `describeImport` toast, a *warning* when any cue was
dropped or unreadable (`importTone`). The selection is left alone. Timing is
**Timed to the cut** (default) or **to a source clip**, offered only the assets a
*rendering* clip shows (`importableAssets` — a muted track or disabled clip has no
footage to caption), defaulting to the selected clip's; the choice lives on `ui`
beside `captionStyle` (so both copies of the controls agree) and `resolveChoice`
falls back to the cut when nothing is offered. An imported set and a generated one
are both `generated` and cannot be told apart, so Recaption's and Clear's tooltips
say they replace / remove *either* instead of guessing — and so does the confirm:
`confirmReplaceCaptions` (`title-actions.ts`) asks "Replace the 12 captions already on
the cut?" before **Captions / Recaption** (and the lane's menu entry and *Edit › Generate captions*,
which call the same `makeCaptions`) and the **AgentPanel's "Caption the cut" chip** (asked before
the task is queued, so declining leaves nothing behind) write over a set, counted on
`editor.liveTimeline` like the import's — the same count the options row and the
button label show (`#liveTimeline` is `$state.raw` so they follow a parked update).
The options row also has **Keep the file's lines** and **Shift times**: the first sends
`KEEP_LINES` (`max_words` / `max_chars` of a whole cue) so a professionally timed
cue is one caption instead of being re-split; **its default follows the delivery
frame** (`keepLinesDefault`: on for landscape and unframed, off for square and tall,
until the box is touched, `ui.captionImportKeepLines` being `null` until then)
because `fit_size` shrinks a kept ~80-character line to the frame's *width* — a
legible 60% at 16:9, a 19-pixel smear at 9:16 — and it is never sent in Word punch,
whose one-word-at-a-time look it would defeat. The second is the `offset` (negative
allowed; `normalizeOffset` holds it to what the engine takes). The words and the
request are `caption-import-ui.ts` (pure); the flow is bun-tested over the harness in
`title-actions.test.ts`, which stubs `./api` and `./notifications.svelte` at module
load (svelte-sonner cannot load under bun), and the desktop half — the dialog's
filters, the `import_captions` arguments — in `api-caption-import.test.ts`.
The **cover frame** is saved from the preview's context menu
(`Save cover frame…` → `export_cover` at the playhead), and both a finished
export and a saved cover offer **Show in folder** in their toast.
The side-file exports (`file-actions.ts`: `exportCaptionFile`, `exportInterchangeFile`,
`exportMarkerList`) ask for a path (`pickSavePath`) and toast the backend's `Report` as the
description (`export-report.ts`: warnings first, `(×n)` counts; a warning toast when anything was
dropped), so the notification log keeps what an OTIO / EDL could not carry; the harness writes no
file and says so. The **export dialog** has a *Captions & chapters* section when the cut has
captions or chapter markers: embed as a subtitle track (only for a container that carries one,
`carriesSubtitles`, mirrored in `validateExport`; switching to one that cannot turns it off),
*Keep captions out of the picture* (`skip_burned_captions`, with a line on why: the embedded
track is off by default and the captions are otherwise burned in), `.srt` / `.vtt` sidecars, and
a line saying how many chapter markers become chapters; with no captions in the cut all of these
are reset to off. A sidecar that already exists comes back as a refusal naming it, which the
dialog turns into a *Replace caption files?* confirmation and a rerun with `overwrite_sidecars`.
**Markers** are edited in `MarkerDialog.svelte` (the marker's context menu, *Edit marker…*:
name, Comment / Chapter, length, comment, one `update_marker` through `marker-edit.ts`, which
sends only what changed); *Make it a chapter / a comment* flips the kind; a marker with a length
draws its span under the ruler chip, and the tooltip names the kind, span and comment.
`Preview` shows the composited frame under the playhead, and during
**forward 1× playback it switches to the streamed frame source** (`start_playback`)
— per-frame `get_timeline_frame` decodes stay for scrubbing, shuttle and the
settled frame, where you want *one* frame rather than all of them. Its effect keys
off `ui.seekEpoch` (bumped only by a deliberate seek or a fresh play) and never off
`ui.time`, which ticks every animation frame and would respawn ffmpeg 60×/sec; it
also restarts for the pane's request width (`boxWidth`, which moves a few times in a
session) but not for the source cap, which changes as the playhead crosses clips.
**The stream plays at the cut's own frame rate and at the width the pane asks for.** The rate is
`renderedFps(timeline, assets)` (`timecode.ts`, held to 1..60 by `playbackFps`): the first clip with
video **in the cut as it renders**, the way the engine reads it (`export_format` of `for_render()`:
muted and solo-shadowed tracks, `trackRenders`, and disabled clips do not set it), where
`timelineFps` — what timecode counts frames in — scans every track and clip as edited. The page
leaves `fps` out at the top of the ladder and the engine reads the same number (`playback_params`),
so the ladder and the gate start from the rate the stream really runs at; the twin of the engine's
`the_rate_is_the_cut_as_it_renders_not_a_muted_tracks_footage` is in `timecode.test.ts`. The width
is `frameWidth` (the pane's, held to the source at the playhead) and is always sent, so a playing
frame is as sharp as the paused one.
**Its frames are raw JPEG bytes**, not data URLs: `api.ts` `startPlayback` parses each channel
message (`frame-transport.ts` `parsePlaybackMessage`: an `f64` time, then the JPEG, a view into the
message; the handler cannot throw into the channel's own ordering) into `{ time, jpeg }`, and only a
frame the gate lets through is turned into anything: `jpegDataUrl` (`Uint8Array.toBase64` where the
webview has it, a chunked `btoa` otherwise) goes to `frames.showStreamed`, into the pane's **one
persistent `<img decoding="async">`** — which the settled still then replaces the `src` of, so
pausing swaps no element. `CANVAS_PAINTER` (`frame-transport.ts`, **off**) is the alternative: a
stream painter (`createStreamPainter`: two decodes in flight at most, the newest waiting frame wins,
a frame that finishes after a newer one is dropped, nothing is drawn after `stop`) decodes with
`createImageBitmap` (`frame-canvas.ts`, with a data-URL `Image` fallback it only commits to after
five bad frames in a row) onto a `<canvas>` over the still, and `streamShown` says it holds the
picture until a fetched frame clears it. Turn it on per engine only after measuring there. In the
browser harness `startPlayback` **synthesizes** JPEG bytes (`sampleFrameJpeg`) at the rate and width
asked for (the cut's own rate when none is) behind a deliberate 90 ms lag, through the same route,
so playback moves under `bun run dev` and the failure modes are reproducible without a desktop build.
Which frames survive is `playback-sync.ts`'s `createFrameGate` (`show`/`skip`/
`resync`/`degrade`, unit-tested): every frame
arrives late by a *constant* transport cost (ffmpeg's spawn, then IPC and the decode)
that on its own exceeds the two-frame `staleAfter` budget (not less than 75 ms: `ui.time` is written
once per animation frame, 17 ms of jitter in every lag, so 50 ms at 60 fps left 33 ms for the rest), so
lag is judged against the smallest this stream has managed (its `floor()`) rather
than against zero — measuring from zero dropped every frame forever and froze the pane.
Only growth past that floor is drift: `staleAfter` skips the frame, `RESYNC_AFTER` restarts
the stream from the playhead rather than playing it out in slow motion against the sound.
**A stream that renders slower than it plays** (60 fps asked of a machine that does 50: the
frames are a sixth of a second further behind every second) would be skipped to a frozen
pane for the six seconds it takes to drift a second behind, so the gate counts a run of skips
whose drift is still growing and after `OVERLOAD_AFTER` (0.4 s of frames) says `degrade` (a
burst after a hiccup is a run of skips too, but its drift shrinks). **A stream that catches up
slowly** is the same freeze the other way round (a 600 ms stall caught up at 1.1x real time heals at
a ninth of a second per second: six seconds of skips under the one-second resync): once a run of skips
has been watched for `HEAL_WATCH` (0.2 s, timed by the `at` the page passes), the gate projects how long
the pane stays frozen until the drift is back inside the budget and says `resync` when that is more
than a restart costs (`RESTART_COST` 0.5 s, or the stream's own floor when longer; only when the drift
is `HEAL_MARGIN` past the budget, since it is only as good as `ui.time`'s 17 ms). A quick catch-up (the
burst after a hiccup; four times real time) is waited out. `degrade` — and a `resync`
that follows another within 10 s — restarts the stream from the playhead one rung cheaper
(`stepDown`): **frames are dropped from the source, not from the stream** — ffmpeg has no
wall clock, so dropping the ones it already rendered saves nothing — halving a rate above 30
first (60 → 30), then narrowing the width a step at a time to 960 (1920 → 1440 → 960), then
halving again (30 → 15), then the narrowest step; smooth motion before a sharp picture that is
moving. **A stream starts the cut's usual start-up ahead of the playhead** (`startAt`): the sound goes on
while ffmpeg spawns, and a stream started at the playhead would show its first frame that long behind it
for good, since the gate judges lag against its own floor. The ladder remembers, per key (cut, proxy epoch,
pane width), the **least** start-up any of its streams has taken — a stream is in step by its floor, so its
start-up is the floor plus the head start it was given — and starts every later stream, a Play, a seek or a
lag restart, that far ahead, capped at `MAX_LEAD` (0.25 s: a longer one is a stall that landed in a start-up,
not one to make up for) and at the end of the cut **as it renders** (`renderedDuration`: the engine refuses a
start at or past it). The first Play of a cut has nothing to lead by. The minimum, not the latest, because a
start that comes in quicker than the last one would put the picture ahead of the sound, which is worse than
behind it. (Replayed on the recorded traces, the restarted stream's first frame was 294 ms behind the sound
without a head start and 54 ms with it, and never ahead.) The ladder is `createLadder` (`playback-sync.ts`,
tested): the steps down this cut is at (remembered for a minute after the last restart, `stepsToStart`: a stream
that met a busy moment finds out; one that cannot is stepped down again within a second), that start-up, and
**whether the next stream may be restarted to heal a slow catch-up**: not for `REPEAT_RESYNC_WINDOW_MS` (10 s)
after a lag restart (`heal`; the gate is built with `healWatch: Infinity`), because a stall that comes with
every start (a decoder's first second) would be restarted into itself over and over (a simulation of a 0.6 s
stall a second into every stream, caught up at 1.1x: 20 restarts and 21 s of frozen picture in 40 s with the
ladder stepping to the floor; with the breaker, one restart). The gate adds the head start a stream was given to
its floor for what a restart costs. It is a counter read when the stream starts, not state; an explicit `fps`
goes to the backend only from the first step down. At the cheapest rung the gate is built without `degrade` (a
restart is an ffmpeg spawn on a machine that is already short): it skips to the resync, as it did before. A
stream that dropped anything leaves a line in the log when it ends (rate, width, frames shown and skipped, why it
restarted).
`start_playback` logs `frames` / `first_frame_ms` / `late_frames` per run, which is what separates
"never started" from "sent but dropped" from "could not keep up".
**What the transport measured** (headless Chromium, software rendering, 4 shared cores, 300
unique JPEG frames per size, medians): raw bytes are 25% fewer through the channel (76 → 57 KB at
1080p) and no base64 / JSON at the backend's end, but **not** the 2-4x decode win the review expected.
The old path's `JSON.parse` of the message is 0.1 ms and `img.src =` 0.1-0.3 ms — Chromium decodes
a data-URL image lazily and off the main thread — and its `decode()` took 3.5 / 5.9 / 9.3 ms at 960 /
1280 / 1920 px, where header + `Blob` + `createImageBitmap` + canvas draw took 5.2 / 7.2 / 12.2 ms; the
browser's CPU per painted frame at 30 and 60 fps was within 10% either way, and the canvas path
keeps the main thread busier (a software `drawImage`; `bitmaprenderer` was no better). So the page
keeps the `<img>`: building the data URL in JS from the raw bytes costs the same per frame as the old
`JSON.parse` + `img.src =` (0.1 / 0.2 / 0.2 ms synchronous with `toBase64`, 0.3 / 0.5 / 0.7 ms with the
chunked `btoa` fallback, at 960 / 1280 / 1920 px; load time and paced CPU unchanged). WebKit and WebView2
are unmeasured.
`ExportDialog` (⌘E) drives
the full `ExportOptions` surface — presets, containers/codecs, rate control, resolution,
a loudness target, and a **Range: In → out** choice when marks are set. It **opens on
the frame the project is cut for** (`initialExport`): the preset whose resolution is
that frame when one matches, else the default preset with its resolution cleared so
"Project frame" renders — otherwise a 9:16 project opened its export already
landscape and the platform check warned about the shape the user had just chosen.
**Fast (GPU)** (`gpuPreset(hw)`, after Web 1080p) is listed only when `hw_encoders` verified an
H.264 hardware encoder (`api.hwEncoders()`, whose answer `knownHwEncoders()` keeps so a reopened
dialog lists it at once) and is never what the dialog opens on: it is Web 1080p with that encoder
(first of NVENC, QuickSync, VideoToolbox, AMF, VAAPI), Web 1080p's CRF 20 (the engine maps it to the
equal-quality QP on AMF and VAAPI; the other families take the number as it is) and GPU decode. The x264
presets stay the defaults until the hardware encoders' quality is confirmed on real footage.
`hwQp` mirrors the engine's CRF→QP offset for the command preview. Under `bun run dev`,
**`?hw=h264_amf,hevc_amf`** answers `hw_encoders` as a machine with those encoders (none by default).
Its **Deliver to** section is the multi-format export: shape chips (the
`DELIVERY_PRESETS` minus Source) that each add a file beside the chosen path
named by shape (`variantPath`, the bun-tested mirror of
`ExportVariant::beside`), a *Smart crop each shot for every shape* toggle, and a
per-file row in the platform check judged at that file's frame (`platformCheck([w, h])`),
in place of the single-file line — with shapes picked, the Scaling rows hide (each
delivery brings its own resolution and fit) and the button reads `Export N
files`; `export-progress` then carries `variant` / `total`, and `together` when the files render in one
pass (the default; `exportVariants`' `singlePass`), which the progress text shows as `3 at once ·`
instead of `1/3 ·`; a stopped single pass rejects with `export cancelled; all N files were removed`, which
the dialog's toast repeats. The
**Transcript tab** of the library (`TranscriptPanel.svelte`, over the pure, bun-tested
`src/lib/transcript.ts`) **is an editing surface**: lines resolve to the clip carrying them,
click seeks, the playhead line highlights, and `×` cuts the sentence from the timeline
(`cut_clip_range`, over its words' bounds when it has words, so the pauses either side stay); cut
lines render struck through. A line resolves to the clip showing most of its cut range (not its
midpoint), so a line whose words are gone reads as cut. With two or more speakers a label heads each
change of speaker and a select filters the lines to one; a transcript with no speakers offers **Label
speakers** (`ui.refineTranscript` → `refine_transcript`, shown and stoppable like an analysis pass).
When it is *empty* it says which
of the five reasons applies (nothing selected / no backend / model not downloaded /
not analyzed / no speech) and offers the matching action — a model picker + download,
or Analyze — instead of a dead end. The **agent panel is a real MCP task
queue** (one status line · review card · queue · quick edits · history · composer) — Kerf has no in-app chat; a connected
LLM claims tasks over MCP. The queue is `agent` state (`src/lib/agent.svelte.ts`, a third
runes singleton) backed by the `tasks` table over Tauri/MCP: the add-task box and preset chips
`agent.add(...)` real tasks, and `ready` tasks show Apply/Dismiss (`resolve_task`/`remove_task`).
The panel is **one status line** (`agentLine`: `Agent connected` / `No agent connected` / `Last seen 4m ago` /
`Agent working` / the endpoint's error — it had an `idle` badge over a "No agent connected" row, two statuses that could
disagree) with a **Connect** button that opens the setup card *under it* (the code rows wrap), then **review card → queue
(ready first) → quick edits → history** (the setup card folds itself away when a proposal arrives — at 1280×720 it pushed the proposal off the screen), the **Apply / Preview / Discard row pinned under the scrolling list** (`data-review-actions`, the panel's own row between the scroll area and the composer, so it is on screen whatever the list holds), and a one-row **composer** (32 px; its hint line shows only when the panel is at least
520 px tall — `showComposerHint` — and is the field's description otherwise). The sections share the overline-and-rule heading; Quick
edits folds from a chevron in it, and its one legend line says what the two icons mean (`play` runs now, `list-plus` is queued). A task's badge is
`STATUS_MAP`: queued neutral with a clock, working and ready the agent's cyan, **applied green, failed red with an alert icon**. Since it shares a tab group with the Inspector by default,
`+page.svelte` brings it forward once when a proposal lands (`editor.staged`
going null → set) — a review nobody can see is not a review. Under **Quick
edits**, four preset chips run an edit now (`quick-edits.ts`: `Remove silences` / `Frame for the delivery`
/ `Caption the cut` (analyzes whatever is in the cut but not yet transcribed — `cutAssets`, which
leaves out titles on tracks: they have no asset — then captions it) / `Cut to the beat` — which
analyzes whatever is on the audio tracks first, then calls `snap_to_beats`, and says
"No cuts were near a beat" instead of claiming an alignment when the grid never reached
them); **the task is recorded only after the edit has run** (`record`: add, then resolve), so a connected agent never sees a task it could claim and redo (short of a window of two IPC round trips: `add_task` has no already-done form, so the task is `queued` until the `resolve_task` that follows — closing it needs a backend command that creates it done), and a failed edit leaves nothing in the queue. The rest enqueue for the agent — `Assemble rough cut` is one of them now: it used to run the same silence removal as `Remove silences` under a bigger name. Remove silences acts on `quickTarget`: the footage of the selected clip, else the selected library asset, else the first picture on the cut, and says which (a line under the chips, the toast naming it, "Nothing to remove" when the cut did not change); the local chips are off while a proposal is previewed. In the browser there is no agent, so
queued tasks correctly just wait. Above the queue sits the **review card** — the
panel's whole point, since an agent's task edits never touch the open cut. It renders
`editor.staged`: the agent's note, a headline (`4 changes · 2:00.0 → 1:40.0 (-20.0s)`),
the changes grouped by what they touch and tinted by the design system's own
`--diff-add`/`--diff-remove`/`--diff-shift`, each row clicking through to the moment
it describes. **Preview** swaps the editor onto the proposed timeline behind a
banner (`editor.previewingStaged`; a fresh `load()` drops back to
the live cut, and `refreshTimeline` parks an incoming live update rather than
yanking the view) — so the proposal can be *watched*, not only read, and the timeline marks what it
changes (outlines on added / moved clips and titles, hatched ghosts for what it removes, Page Up / Page Down to step through the
changes, a `place 2 of 3 · 5 changes in all` chip in the banner: frontend-timeline.md, *A proposal on the timeline is marked*). **The preview is read-only**: the cut on screen is not the project's, so an edit is refused *before the backend is asked* (`#apply` takes a thunk for that reason, and a refusal is a `PreviewReadOnly` — said as a notice, "Exit the preview to edit") — apply and discard are the two edits that end it — and the Inspector is `inert` with a line saying why; the preview is dropped only after an edit has gone through, so a refused apply leaves it exactly as it was. **Every way into an edit says so first and says it as a notice.** An action that reads the cut on screen before it asks the backend — delete, cut, split, trim, roll / slip / slide, detach / reattach / link / unlink, paste, duplicate, a marker, append all, paste keyframes, caption import, a delivery frame, a quick edit (which would analyze media and read the proposal's cut) — starts with `holdForPreview()` (`ops.ts`: the notice, and it returns before anything is planned, copied to the clipboard or remembered; `cutSelection` used to copy first), and every rejection is shown through `report(e)` (a `PreviewReadOnly` is `toast.info`, anything else `toast.error`) — no component writes its own `toast.error(e.message)` for an edit (`review-readonly.test.ts` scans). Menus are held by data: `MenuState.previewing` turns every editing entry of the menu bar off with the reason (`hold` in `menus.ts`; copy, select all, the tools, ripple and snapping stay), and the context menus pass their editing rows through `heldForPreview` (`menu-items.ts`; go to, copy, analyze stay). The timeline's toolbar (undo / redo, `+ V` / `+ A`, fades, the delivery frame), the track level and pan faders and the lanes as a drop target are off, the preview draws no title placement boxes, and the Agent panel's quick edits are off in the chips *and* the right-click menu (`heldWhilePreviewing`). Reads of the proposal are ordered (`ReadOrder`, `generation.ts`: of the reads that *come back* the one that started last writes, and it re-checks the preview after every await; a project loaded or a proposal settled `invalidate()`s every read in flight), so a newer read that failed does not cost an older, successful one its answer. An edit while one waits re-reads it so its `stale` flag is the backend's at once, and a failed read says nothing about the proposal. **One set of answers** (`proposal-actions.ts`: `applyProposal`, `discardProposal`, `inspectChange`) serves the card, the preview banner (Apply / Discard / Exit preview) and the strip over the timeline: Apply confirms first when it went `stale`, a Discard of **more than three changes asks** (`discardNeedsConfirm`; the same question when a task's ✕ would discard its proposal), and a row of the proposal selects through `reviewTarget` — a clip or title that only exists in the proposal opens the preview and is selected there, instead of selecting a clip the live cut does not have. The title-bar chip is `Review 4 changes` in a right cell of 560 px or more, `4 changes` from 440 (1280×720 has ~470, 1366×768 513; the project's name is what gives way) and only below that the bare count, with `Review 4 changes` as its accessible name and `badgeTip` as its tooltip (`reviewChipText`). The banner's counter tooltip names the keys the user has bound (`reviewStepTip`, `settings.keysFor`), not Page Up / Page Down. The queue card of the
task whose edits are staged has no buttons of its own (`readyCard`: one Apply, in the review card);
any other ready task's Apply / Dismiss and the remove ✕ report a refusal as a toast (`agent-tasks.ts`). The
headline arithmetic is `src/lib/diff.ts`, the bun-tested TS mirror of
`TimelineDiff::headline`; the entries themselves are phrased by kerf-core, which is
why `revisionDiff` returns `null` in the browser instead of a second, divergent diff
engine. Below the queue, the **History** section renders
`editor.history` (the `Revision[]` edit log, attributed to user/agent/system) with one-click
`editor.revertTo(seq)`, and each row expands to *what* that revision changed
(`revision_diff`).

**Editor context.** So that an agent knows what "this clip" is, the page reports the playhead,
selection, in/out marks, `ui.timelineView` (the timeline's visible seconds), `ui.focusedPanel`
(dockview's active panel) and the selected asset through `reportUiContext` (`editor-context.ts`:
throttled, unchanged state not re-sent, a 30 s heartbeat; a no-op in the harness) — `app.md`.

**The Preview with nothing on the timeline** is `PreviewEmpty` (it was a clapperboard and "No media loaded"): a card of three steps — *Import
media*, *Cut it on the timeline*, *Export* — each marked done / current / to do from the project (`firstRunSteps`, `first-run.ts`: an
asset in the library finishes the first, a clip the second), with the keymap's own key for import and export in the line, an
*Import media…* — or, with media in the library and the library on screen, *Add selected to timeline* (`firstRunAction`; *Open Library* only when the library is closed: it used to sit over an open library and do nothing) — and an *Export…* button (off, with the reason in its tooltip, until a clip is on the timeline — as the title
bar's is: `exportButtonState`, which stays on while a render runs, since the button reopens its dialog), and "Or connect an agent over MCP and let it assemble a first cut" with a button
that raises the Agent panel, whose status line carries *Connect*. The card fills the pane, which is a size container, and gives way by its
height rather than spilling over the transport: spacing tightens under 345 px of content (1366×768 has 326), the lines under each step go
under 310 (1280×720 has 296), the agent row under 260 (960×640 has 247), and what still does not fit scrolls inside the card.

**Z-order** (no tokens yet; the layers in use, low to high): the timeline's sticky and ghost layers 24–29, dockview's own overlay 45, dialogs 50
(`Modal`, later siblings stack over earlier), the file-drop overlay 60, the notification log 70, the menu bar's panels 950, a context menu 1000,
and the toaster (Sonner's own, very large) above them all. A new overlay takes the layer of what it is most like.

**The Preview frame and its scrub bar.** The frame is the delivery shape at `min(100%, 100cqh × aspect)` of its
pane — the 720 px cap it had left it 62 % of the pane at 1920×1080 and a postage stamp at 2560×1440. What is asked of the
backend follows the box: `preview-size.ts` `requestWidth(css width, dpr)` rounds `ceil(css × dpr)` up to 480 / 960 /
1440 / 1920 (the top is the MCP frame tools' own cap; the backend takes any width and never scales a picture past its
own, so the bound is only on cost), measured from the frame element by a `ResizeObserver`, the window's resize and a
`(resolution: Xdppx)` media query (a window dragged to another monitor fires neither of the first two),
**settled 200 ms** so a dragged panel edge is a few requests, not one per pixel; a box with no width (a hidden dock
tab) is not a measurement — the last one stands, or nothing is asked. The width is then **held to the widest source the
preview reads at the playhead** (`proxy-info.ts` `previewSourceWidth`: a ready proxy's width when the preview decodes it,
else the original's; `capToSource`), since the default proxy is 1280 px wide and a 1440 / 1920 px composite of it is the
same picture upscaled at several times the decode cost on every scrub frame — not under a delivery frame, whose canvas is
not the source's shape (`data-request-width` on the frame says what it asked). The pump (`frame-pump.svelte.ts`) takes it as `width()`, reads it in `run()` — a different
step is a new frame — and asks nothing until the box has been measured, so a first frame is not decoded at a guess
and again at the real size. **On the GPU route the pump is not single-flight** (the JPEG route is: one fetch, the latest wanted frame
waiting): the backend's `get_preview_frame` is latest-wins (`app.md`), so a request goes out for every wanted time, an
answer `superseded` is ignored, and an answer older than the one on show (`asked` / `shown` counters) is dropped, which keeps a
slow JPEG from landing over a newer picture across a route change. Each GPU request also says which it is
(`PreviewOrder`: the pump's own name and its `asked` number, passed to `get_preview_frame`), because separate IPC calls can reach the
backend reversed and the backend then drops the older one instead of drawing it last. `gpu-preview-checked` (the backend's first-use self-test has a
verdict) refreshes the preview like `proxy-ready`. (Playback's streamed frames ask for the same width, read when the stream starts.) The picture is clipped to the
frame's radius and the frame's border is `border-strong`, since a black matte is 1.05:1 on the pane. **The HUD** over the
picture is scrim chips with `text-on-video` (the name, ellipsised at 62 % of the width so a long file name never reaches the
format, then `1920×1080 · 30 fps` in the transport's own rate format, and the proxy / analysis status chips with a dot) — bare
tinted text was unreadable on a snow shot and on the Light theme's pale placeholder — and it no longer prints the timecode, which
is the transport's. **`ScrubBar.svelte`** is a 16 px hit area around a 4 px rail (`--scrub-rail`, an alias of `text-disabled`, so
the 3:1 it is held to covers it and it shows in High contrast, where the old `surface-inset` rail on the black bar was 1.05:1) with
the in / out range behind the fill and a thumb that grows while held. A press moves the playhead there and a drag moves it
*live* (`scrub.ts`: `scrubTime`, and `frameLimited` — at most one `ui.seek` per animation frame, with the newest position, since
each seek is an FFmpeg composite); it is `beginDrag` (pointer capture, so the pointer may leave the bar; Escape /
pointercancel / lost capture / blur put the playhead back where the press found it), playback is paused while the bar is held
and resumes at either end of the gesture — except when it was let go at the very end of the cut, where `play()` would start
the cut over (a seek mid-play restarts the audio each time). It is a `role=slider` with the cut's
timecode as its value text; a press does not focus it (a focused slider owns Space and the arrows — the page stands down for
the keys a slider operates — so one left focused after a click would silence the transport shortcuts), and once Tab has
put focus there its own keys are ← → a frame, ⇧← ⇧→ a second, Home / End the ends, Space play.

**The GPU preview in the page** (`Preview.svelte`, `preview-bounds.ts`,
`gpu-preview.svelte.ts`). With the setting on, a frame whose route is `gpu`
(`routePreview`: pure and bun-tested — setting, platform support, playback streaming, an
empty timeline, overlays, covered) is asked of `getPreviewFrame` instead of
`getTimelineFrame`, and when the backend says `renderer: 'gpu'` the frame draws *nothing*
(`gpuShown`: no image, no backdrop — the surface is what is seen); a frame it hands back is
the JPEG as ever. The frame's content box is reported to `set_preview_bounds` by a
`ResizeObserver`, the window events and a 150 ms poll, one report at a time (`singleFlight`),
`deviceRect` rounding both edges so neighbours never gap at 125 / 150 %, and a hidden report
goes out when the panel unmounts (a workspace switch) or the setting goes off. Under technique
`window` the frame's ancestors get `data-surface-hole` (transparent, in `layout.css`) and the
pane's surround is a layer with an even-odd `clip-path` hole at the frame (`holePolygon`) that
covers the **pane only** — it is positioned, so it paints above the unpositioned transport bar,
and one spanning the whole panel hid every control in the bar but the scrub dot — so nothing else
on the page changes; `?gpusurface=1` makes the browser harness answer as such a
surface (there is no GPU there), and a headless-Chrome screenshot with a transparent default
background shows the hole. The status bar names the renderer of the frame on screen (`GPU
430×240 · 23 ms · llvmpipe`, or `FFmpeg · <why>`), only while the setting is on, and the
Settings dialog's Preview section carries the toggle and the machine's status line.

**Drag frames (A3, `preview-draft.ts`).** A gesture under way puts what it would write in the one draft slot (`ui.setDraft(owner,
draft)`, read as `ui.previewDraft`; `PreviewDraft`: the clips and titles it changes, whole) and clears it once its edit has landed or
it was abandoned, so the picture does not step back to the cut before it in between. The slot has an **owner** (`putDraft`): each
writer (a `SliderRow`, the timeline, the Preview's title box) passes its own symbol, its `null` clears only its own draft, and a draft
equal by value to the one held leaves the slot as it was (`sameDraft`), so pointer events that change nothing ask for no frame. A
`SliderRow` that unmounts mid-gesture clears its draft, and the Preview drops any draft naming a clip, track or title the cut no
longer holds (`draftHolds`: an undo mid-gesture), which the backend would refuse. The Preview sends the draft only while it changes
the frame at the playhead (`draftTouches`: a clip on a video track at the playhead where it is or where it goes, give or take a frame
and the transitions of its tracks, or a title on screen there); otherwise the frame is the cut's. The frame pump reads it **on the GPU
route only** and sends it with the frame (`getPreviewFrame(.., draft)`), so the surface shows the frame the release will write; on the
JPEG route (the setting off, a title box or the trim monitor over a surface above the page) it is not read and asks for nothing — the
edit's frame once written, as before. While a proposal is previewed none is sent. The pump's `drawnDraft` is the draft of the frame on
show (`null` for any other picture). The builders mirror the edits and say `null` where the outcome is not known
exactly: `transformDraft` / `colorDraft` (`setTf` / `setCol`; a keyed number is written as a key the backend places, so it is not
drafted), `maskDraft`, `fadeDraft` (held to the clip's length), `titleMoveDraft` (`moveOverlay`: a key at the playhead for an
animated title, `withKeyframeAt`) and `titleSizeDraft`, `moveDraft` (every ghost of `planMove`'s plan, partners included; nothing
while the drop is refused or a no-op) and `trimDraft` (`trimEdit`'s fields). Sources: `SliderRow`'s `onLive` (the value of a drag or
a run of keys, then `null`; `Pending.scope` is the clip it began on) for the Inspector's transform, crop, colour, mask, fade and
title rows; the Preview's title box drags (the ghost text is hidden once the GPU has presented a frame of this drag's draft,
`drawnDraft`, not before); the timeline's moves and nudges and a trim of a clip alone (`gestureDraft`; a move or trim is held through
its write by `landWith`, a nudge by `nudge.writing`, which keeps its plan until the edit lands). A trim that ripples or carries partners, roll /
slip / slide, animated SVG titles, speed and the grade sections draw the cut as it is until released. Tests: `preview-draft.test.ts`,
the draft cases of `frame-pump.test.ts` (`drawnDraft` included), `Pending.scope` in `slider-row.test.ts`.

**GPU playback in the page** (`gpu-playback.ts`, `output-latency.ts`, `frame-pump.svelte.ts`, `Preview.svelte`; the backend's half is in
`app.md`). With the GPU preview on and the route `gpu` at the moment Play starts (the setting, a surface, nothing over the frame that a surface above
the page cannot show, not a detached panel), the **playback effect starts a `GpuPlayback` session instead of the stream** and the frame pump
takes over (`playback` / `playClock` deps): the webview stays the clock owner, and the audio stays the clock. Once per animation frame (the
effect on `ui.time`), the pump asks `getPreviewFrame(time, …, { session })` (`hint: 'forward'`) for **`pictureTime(clock, outputLatency, roundTrip)`**:
the audio clock less the sound's **output latency** (`audio.outputLatency()`; `outputLatency(ctx)`: `getOutputTimestamp()`'s context time against the
render head, else the browser's `outputLatency + baseLatency`, else 0; held to 0.5 s and smoothed by a median of 9) plus the **median round trip** of
the requests that drew a frame (held to 0.25 s), so a picture lands on the screen when its sound lands in the room and a slow frame is asked for
ahead by what it will cost. A frame of the cut is asked for **once** (`frameIndex`; never twice, never an earlier one when an estimate moves), and the
first request of a session is the only one in flight until it is answered, because it **decides the session** (`renderer: 'gpu'` or `'stream'`);
requests the backend replaced answer `superseded` and change nothing (**skip, never slow down** is the latest-wins mailbox's, not a gate's: a GPU
that overruns a slot loses the frames it overran, and `createFrameGate` stays the stream's). A `stream` answer, a rejected request (the GPU is marked
failed and the surface hidden), or a GPU whose median round trip is over **three frame periods for a second** (`OVERLOAD_PERIODS`, `OVERLOAD_AFTER`)
**hands the session to the stream at once** (`handOverToStream(now)`), as does a request the backend has not answered at all for 1.5 s (5 s before the first
answer; `STALL_AFTER`, `GpuPlayback.stalled`: only a request *waiting* counts, a clock that stands still asks for nothing) — a decode that hangs blocks the one worker that draws; an answer's **`play.ahead`** (the first frame the GPU will not draw and the
time a stream must start at) hands over **when the time that is heard is within the stream's start-up of it** (`handoverDue`; the ladder's
measured start-up, else 0.15 s). **The takeover stream is in the GPU's time** (`ladder.start({latency, ahead})`): it reads the clock less the same median output latency the GPU's pictures
do (`GpuPlayback.offsetAt`: zero until a frame was drawn unless the session was begun from the audible picture, a running play rebuilt; never before the
playhead the play started from), so the picture does not jump forward by the
latency at the hand-over (`ui.time - offset - f.time` in the gate), and the same latency carries over the restarts of that stream in the same play
(`takeoverLatency`, reset wherever a stream that is not its continuation begins). **It starts at the engine's hand-over only when a window is open and the
page was on time** (`takeoverStart`): `ahead.handover` is before `ahead.at` only when the refused frame sits in a fade, a dissolve or a slide
(`SpanPlan::handover` is the frame itself when none is open, and an answer to a request is a round trip old, so starting there would be early for
nothing), and a slice that began inside the window would drop it and pop the clip in. The paced stream keeps the offset it starts with and the gate judges
drift against its own floor, so a stream started `d` before where the start-up puts it trails the sound by `d` for the rest of the play, unseen:
**synchronisation wins over the transition when the hand-over is late**. The hand-over is honoured within `MAX_BEHIND` (50 ms, a frame or two of the
trigger's lateness) of the plain start, and past that the stream starts at the playhead and the window is cut. An **at-once** hand-over (the GPU gave up:
overload, silence) is told nothing about a refusal that is still ahead (`takeoverAhead`; one that is due is kept): otherwise the pane would hold the GPU's last
picture until the stream reached it, up to five seconds. **With no start-up measured** (the ladder's key resets on every edit, so a GPU play's takeover is usually the first stream it sees) the takeover is started
`DEFAULT_STARTUP` (150 ms) ahead, as `handoverDue`'s trigger assumes, not at the playhead; **the limit that remains on a first Play** is a stream that takes
longer than that to start, which trails the sound by the difference (100 ms for a 250 ms start-up) until a stream has been seen to start and the next one is
started that far ahead: the gate judges drift against its own floor and cannot see a constant lag. `playback-sync.test.ts` runs a takeover through the real
ladder and gate for no window, a window on time, a window late and an unmeasured ladder, and holds the steady lag to a frame (three for the slow first start). **A play holds the picture at the playhead it
started from** (`new GpuPlayback(fps, playStart(ui.time, outputLatency, midPlay))`) until the audible time reaches it: the first requests are not for the clock
less the latency, which is before the paused frame (up to nine frames replayed with a Bluetooth headset). Rebuilt in a running play (an edit, a resize: the
effect ran again without a seek, `playSession`) the session starts at the audible picture instead, `ui.time - latency`, which is what is on the screen — and no earlier
than the playhead the play began at, so a rebuild within the latency of Play or a seek does not rewind the picture. A hand-over starts the stream through the same ladder (`takeover`): the stream is *not* `streaming` (so the route stays `gpu` and the
surface stays up) until its first frame **at or past where the GPU stops** is shown, and only then does the GPU's session end and the surface go. A
session that was given up on is **barred** for the rest of that play (`gpuBarred`: until a deliberate seek or an edit, not for a stream restart for
lag), so a GPU that failed or cannot keep up never flips back and forth with the stream. **A play the stream took over at a refusal is taken back**
(`TakeBack`, `gpu-playback.ts`): when the session stopped where the engine said (`GpuPlayback.resumable`: a planned hand-over, or a decline at Play
whose answer names the refused frame; not `failed`: a frame that failed, `session`, an overload, or a request that was rejected or never answered,
`giveUp`), the stream, once it has been on show for `READY_SETTLE` (1 s), asks `frames.ready(at, ..)` (`hint: 'ready'`, with the pump's `client` and
next `seq`, so a ready request that reaches the backend after a newer request of the page, the paused frame once the play stops, is superseded and
primes nothing over it) for `lead` past what is heard and no earlier than the refused frame: `READY_LEAD` (0.6 s), twice what the last answer
took, or room for the GPU's session to begin its first frame early after the route is back (first frame + `SHOW_AHEAD` + 0.1 s), whichever is
most, capped at `READY_MAX_LEAD` (2.5 s) so what is made ready is played well inside the backend's 10 s. Only `stream` with no `play` ends the
take-back for the play (`never`: the setting, no surface, no plan from there); `stream` with `play.ahead` (nothing within the backend's 4 s yet), a
superseded answer and a request that failed (logged) are `later`, asked again `READY_EVERY` (1 s) after the answer came, doubling with each `later`
in a row up to `READY_MAX_WAIT` (8 s), so a cut the GPU draws none of costs the one worker that draws a scan every 8 s at most. A `ready` answer's
`play.resume` is where the play from there is made ready. The GPU's session begins a first frame's cost before it (`firstFrameLead`: what the last
take-back's first frame cost, `learnedLead`, kept apart from the steady round trip of the session the stream took over, `handoverTrip`, which is the
estimate until a take-back has been measured; never less than a frame), and `SHOW_AHEAD` (0.3 s) before the session begins the route is the
surface's again (`returning`: `streaming && !returning` in the route, so the bounds go out visible; the surface is not shown before a frame is drawn on
it, and the pump, `streaming && gpuPlay === null`, asks nothing). The `GpuPlayback(fps, resume, true)` holds its picture at `resume`, and once its
first frame is on the screen (`gpuShown`) the stream stops, the bar is lifted and the play is the GPU's again (and may hand over again later). A
session that declines before it drew (a refused frame, the bounds not delivered yet, something over the frame on a surface above the page) leaves
the stream playing; one that failed ends the take-back (`abandonTakeBack`). A declined session and a time made ready that went by (`MISSED_AFTER`)
are asked about again after a wait of their own count (`TakeBack.retry`: 1, 2, 4, 8 s, which a `ready` answer does not reset and only a play taken
back does, the next hand-over's `TakeBack` starting from nothing), so a take-back that keeps failing at the switch costs a primed play every 8 s at
most, and pausing, a seek or an edit ends it. **What is left at the switch:** the first frame is begun by an estimate of what it
costs, so it can land a frame early (the picture holds a frame ahead of the sound) or late (the stream already showed a frame or two past `resume`,
and they are shown again); and the take-back is in the audible time, so from a stream that never was the GPU's (declined at Play, read from the
plain clock) the picture steps back by the output latency. Under Proxy only the first request is rejected as the stream is (`playWaiting` →
`playbackWaiting`). The status bar names the renderer of each drawn frame (`GPU 960×540 · 14 ms · adapter`; a repeat is not a new frame and
changes nothing). Pausing ends the session (the effect's cleanup), and the settled frame comes back through the ordinary GPU route. In the
browser harness `getPreviewFrame` answers a forward request `stream` (no GPU), so playback is the synthesised stream as ever; `?gpusurface=1` pretends
a surface and has the GPU play, `&gpuplay=stream` has it decline every session and `&gpuplay=ahead` has it draw for two seconds and announce a
refusal (the hand-over), and take the play back at once when asked (`ready`), so a play cycles GPU, stream, GPU. Tests: `gpu-playback.test.ts`
(the request arithmetic, the decision, the hand-over, the overload, `resumable` and `TakeBack`), the Preview's wiring pinned in `playback-sync.test.ts`,
`output-latency.test.ts`, the `GPU playback` cases of `frame-pump.test.ts` (the pump over the real Svelte runtime), and the ladder's `startup`.

**Shared components** (`components/editor/`; `shared-components.test.ts` keeps the dialogs, the Inspector's rows and the toolbar on them).
**`Modal`** is the shell of every dialog: the scrim (`--scrim` at 55 %, one `z-index` of 50 — a later sibling, the
update dialog, stacks over an earlier), the panel (`--line-width` border, `--shadow-lg`), a header with an icon, the title (the
dialog's accessible name, `aria-labelledby`) and a named close button (an `IconBtn`, `title="Close"`, an `x` icon), `trapFocus`,
Escape, and an optional `footer` snippet drawn under a rule; the body is the dialog's own. What differs is props:
`width` / `height` (px, never wider than the window), `busy` (a run that cannot be walked away from: Escape, the scrim *and* the
close button are off, and the button is gone), `dismissOnScrim` (off for a dialog holding typed input or chosen options),
`initial` (a selector for the field that takes focus on open) and `rescue` (see below). The close rules are the pure
`mayClose(via, {busy, dismissOnScrim})` and `scrimClick` in `modal.ts`: a click closes only if the *press* began on the scrim
too, so selecting text in a field and letting go over the dark no longer closes the dialog. Policy per dialog: Export
`dismissOnScrim={false}` and never `busy` (closing mid-render leaves it running); Voiceover `busy={generating}`,
`dismissOnScrim={scrimCloses(text, generating)}`, `initial="textarea"`; Compose music `busy={generating}`, `dismissOnScrim={false}`, `initial="[data-initial]"`; Fit / Stems / Update `busy` while they run; Settings none.
**`SliderRow`** is one numeric property: label, slider, number field, and a 24 px slot for the keyframe button, on one grid
(`72px 1fr 60px 24px` — `NUMBER_COL`, room for `100.00s` in the 12 px mono type — the slot kept when empty so every slider in the panel is the same length; `reserve={false}` in
a dialog; `minmax(0, 1fr)` so the row shrinks to the Inspector's 260 px minimum without clipping a value). The slider is
`MixSlider` (`solid`: drawn like the plain range) so a drag writes **one edit on release**, Escape / pointercancel / blur
abandon it, a run of arrow keys is one edit and a double-click puts the property back to its rest value. **Only the thumb
takes hold, so a click elsewhere on the track no longer jumps the value** — a deliberate change from the native range it
replaced, as in the mixer: a jump on the first click would make a double-click three edits (jump, jump, reset). The rest value (`neutral`) is a
tick on the travel, and a property that goes both ways (`bipolar`, by default a travel across zero) fills from it instead of
from the left. The number is a field (click, type, Enter or Tab to write, Escape to put it back; `1.1` on a 0.25 step is fine):
what is shown is `value × scale` with `digits` decimals and a `unit` (`PERCENT`, `DEGREES`, `TIMES`, `SECONDS`), and the geometry,
snapping, readout, entry parsing and key steps are the pure, bun-tested `slider-row.ts` (a typed entry is NFKC-normalised, so full-width digits parse, and held to `limit`, a fade
as long as its clip on a 5 s slider, and to the digits shown: a fade is 0.01 s, a title's size 0.1 %). A value is *at rest* only when it
is exactly there — a 0.4° rotation reads 0° and can still be reset or typed to 0. **A write goes to what the edit began on**: a typed
entry is written when the field blurs, and the click that selects another clip blurs it after the selection has moved, so the row
reads its `scope` (clip, title, playhead time; `Pending` in `slider-row.ts`) when a drag, a key run or a typed entry *starts* and hands it
back with the value (`at`); the entry is parsed with the **spec it began with** too (a fade typed on a 20 s clip and committed by the click on a 3 s one is not
clamped to 3 s), and focusing the number field starts a fresh `Pending`, so one left behind by a row that was disabled mid-focus is not inherited; the Inspector's helpers (`setTf`, `setCol`, `setCam`, `patchMask`, `setFade`, `moveTitle`, and a keyed
number's `keyAtPlayhead`) read that clip. **A plain field** (`numRow`, a title's Text / Color / Box / Font / Bold, an effect's parameters, the
Transitions tab's length) is no different: its `change` comes with the blur, by when the selection has moved *and the field has been
re-rendered with the new selection's value*, so reading either writes the other clip's number onto this one. `FieldEdits`
(`field-edit.ts`) notes the scope on `focus` and the typed text on `input` and `compositionend`, and the commit takes both from there (`noteFocus` /
`noteTyped` / `endEdit` in the Inspector); a select or a checkbox changes at once and only needs the scope. A bun test holds every
text and number input of the Inspector to this. Rest values are the model's
`DEFAULT_TRANSFORM` / `DEFAULT_COLOR` / `DEFAULT_REFRAME` / `DEFAULT_MASK`. The Inspector's other rows (read-only values, plain number
fields) share the label column and end where the sliders' numbers do, a field's unit (`s`) sitting in the keyframe slot. The ◇ is a 24 px `IconBtn` with a `diamond` icon (filled while a key sits at the playhead).
A plain `<input type=range>` (Settings, the timeline zoom) gets an 18 px hit box and `min-width:0` from `layout.css`.
**`ToolbarToggle`** is a button of the timeline toolbar: 24 px high and at least 24 px wide, one radius, and one "on" for every kind
of button — the amber outline and wash (`kerf-400` border, `kerf-300` on `selection-fill`, a pair the contrast guard holds to 4.5:1)
whether it is a tool, a mode (snapping is a pressed magnet, not a word that changes width), a view or a menu with something chosen.
`pressed` is a toggle (announced as one), `on` lights without announcing, neither is an action; `bare` is no outline while idle (tools,
zoom), `square` a picture-only button. Its states are `.k-tt` in `controls.css`, which the veil leaves alone.
**One control scale for the panels**: `Btn sm` is `--control-sm` (28 px; it was 30) beside every field, chip and tile in a panel.
**`Chip`** (`.k-chip`) is for what is *on or off* among choices — `pressed` is required, so a chip never reads as an action; an action is
a `Btn`, and a ghost `Btn` is a quiet one (the hover wash is the veil's). The dialogs that still draw a chip by hand use `chip()`
(`chip.ts`), which has the same height, weight and radius. **`Tile`** (`.k-tile`) is a preset in a grid: one line, an ellipsis, the
tooltip. **`Hint`** is the box that says why a tab's controls are off, the same in Effects, Transitions and Audio. **`CheckRow`**
(`.k-check`) is a checkbox with its words, box first, the whole row the target. **`EmptyState`** is a panel with nothing to show: one icon,
one line, one reason, optionally a button (the Inspector, the notification log, the Transcript and the dock's watermark, which offers
*Open Library* and *Reset workspace*). Three small classes in `controls.css`: `.k-select` (a native select drawn as a field, its
chevron two gradients of the muted ink, so it follows the theme; use `background-color`, never the `background` shorthand, beside it),
`.k-swatch` (a colour input as a field-high swatch) and `.k-overline`. `tip-guard.test.ts` lists `Chip`, `Tile` and `EmptyState` among the
components whose `title` is theirs. Pills (`radius-full`) are for count badges and dots; chips, buttons and the shell's small controls are `--radius-sm`.

**Tooltips are `use:tip`, not `title=`** (`tip.ts`; `tip-guard.test.ts` fails on a native `title` on any plain element, on a
component that would pass one on to a plain element, and on one set from script — `Btn`, `IconBtn` and `ToolbarToggle` take a `title`
prop and route it to the action). `use:tip={'Razor'}`, or `{ text, keys, detail }` — `settings.tip(label, actionId, detail?)` makes
one with the user's key (`keys`, drawn as keycaps by `accel.ts`) and a muted second line, so a long hint is a noun plus a sentence
instead of a paragraph in one line (a string's lines after the first are its detail). One tip is up at a time: it shows after 400 ms
(60 ms right after another has gone, so a row of buttons reads as one sweep, and at once when the keyboard focuses a control —
`:focus-visible` only, a click that focuses is not asking, and never a field you type in — `raisesFocusTip`: it would cover the next one),
and goes on a press, a scroll, leaving or Escape. **Escape only dismisses; it is never consumed**, so a slider's Escape-to-abandon and a dialog's Close
work with a tip showing. It is dismissible and
persistent but **not hoverable by design** — a tip that took the pointer would sit over the next toolbar row and eat the click meant for
it. **A context menu holds every tip back while it is open** (`holdTips`; the menu's own info rows used to raise one over the label beside them — the info row's detail is a line under it now). It is the menu's surface (`--surface-raised`, `--shadow-md`, `--radius-sm`), `role=tooltip`, and `aria-describedby` points at it
while it shows — the whole tip when it says more than the element's name, only its detail when the name *is* the tip's text
(`describeTarget`: a screen reader would say the label twice); an element with no name of its own (an icon button) takes the tip's text as its
`aria-label`, since the `title` that used to name it is gone. One element per *document* (`realm.ts`), so a detached panel window draws its own; no fade under
`prefers-reduced-motion`. The timing (`createTipController`), placement (`placeTip`) and the rest are pure and bun-tested; the tip
does not appear on a disabled control in engines that drop its pointer events (Chromium delivers them; WebKit untested).
**Context menus are keyboard menus** (`ContextMenu.svelte` over `menu-nav.ts`, shared with the menu bar's `stepFocus` / `typeahead`): it
takes focus when it opens — the panel for a pointer, the first row when it was opened from the keyboard (the Menu key, Shift+F10, Enter
on a button; judged from the invoker's `:focus-visible`) — ↓ ↑ wrap, Home / End jump, Enter / Space run, a letter jumps to the next row
that starts with it, Escape closes and gives focus back to the invoker, Tab closes and goes on. Rows are roving-tabindex `menuitem`s; a disabled
row is `aria-disabled`, not `disabled`, so it takes focus and says why; the lit row is one at a time (`:hover` and the keys' `:focus-visible`, the
pointer moving focus to the row it is over). The capture-phase key handler keeps a menu's keys from reaching the page's shortcuts. An action that
opens a dialog finds focus on the invoker, so the dialog's own focus return works. At 1366×768 an ArrowDown used to scroll the page and
close the menu. A menu opened from the keyboard starts on its first *runnable* row (`startRow`). A group of radio chips (Settings' proxy size and preview
source) is one tab stop (`tabStop`: the chosen chip if it can take focus, else the first that can — Preview source disables all but "Always original"
while proxies are off) the arrows move through (`roving.ts`; selection follows focus, except Proxy size, where a choice rebuilds every proxy, so its
arrows only move focus and Space / Enter choose); a chip that is on or off carries `aria-pressed`, the Settings rail `aria-current="page"`, and a button that is a picture or a glyph has a name
(`a11y-guard.test.ts` scans the markup for each, and that the one focus rule lists `textarea`).
**Progress bars say what they know** (`ProgressBar.svelte`, `.k-progress` in `controls.css`): a `progressbar` with an accessible name that fills to its
value, or — with none (a download of unknown size, a stage that reports nothing, an export waiting on its proxy) — sweeps a segment along the track
rather than parking it at 30 %; under `prefers-reduced-motion` the whole track is tinted instead (`progress-guard.test.ts`). Its `flex` is `1 1 auto`: with `flex: 1` (basis 0) a bar under a label in a column — every run dialog — was drawn 0 px tall.
**About Kerf** is a dialog (`AboutDialog`, `ui.aboutDialog`; `about.ts`): version, platform, build, licence, and *Copy version info* — the text a bug
report needs, the same lines that are on screen.

**Modals are modal.** Every dialog is a `Modal` (above), which uses the
`trapFocus` action (`src/lib/modal.ts`: takes focus, wraps Tab, restores focus on
close; Voiceover starts on its textarea via `initial`), and `+page.svelte` makes the app behind them `inert` and returns early from its global key handler while
any is open — Space / Delete / J-K-L / ⌘Z would
otherwise edit the live project under a dialog; a file drop is ignored then too.
Every shortcut — bare keys and ⌘ chords alike — stands down inside any text input /
textarea / select / contenteditable. **Focus survives a run and a menu.** A dialog whose footer
swaps for a run (Export, Voiceover, Stems, Fit, Update) passes `rescue` and marks its footer actions
`data-rescue`: when the button that had focus is replaced or disabled a `MutationObserver` moves it to
the live action (Generate → Cancel, and back to Generate when the run ends) or to the dialog root,
not `<body>` (`focusLost`, pure). On close focus goes back to the control that had it; if that is
gone (the menu entry that opened the dialog) to the menu bar's tab stop (`data-focus-return`) **when
a menu entry that opened a dialog was run from the keyboard** — anyone else is left on `body`, since
a focused menu title would take Space from play / pause. The bar's `byKeys` ends with a pointer press,
Tab, or focus given back (Escape, a chord), so it cannot leak into a later dialog a shortcut opens. **A scrim click never loses typed input**: Export ignores it (its options
come back too); Voiceover ignores it while the script is non-empty (`scrimCloses`) and keeps the
script as a draft (`kerf.voiceover.draft`, cleared by a successful generate, not kept for a script
regenerated from an existing voiceover); Settings / Update / Fit / Stems still close on it.
**Recovering a crashed untitled project**: after the first load (and before a launch file is
opened) `offerRecoveries` in `+page.svelte` asks `listRecoveries` and, per session, newest first,
`askRecovery(recoveryCopy(info))` — a native three-button dialog, **Recover · Discard · Not now**
(`recovery.ts` holds the copy and `recoveryChoice`; a dismissed or unrecognised answer is Not now, and
the two-button fallback offers Recover / Not now, never Discard). Recover asks about this project's
unsaved work first (`okToReplace`), then `editor.recoverProject(id)`; after one recovery the rest wait
for the next launch. The browser harness has none.
**Nothing unsaved is dropped silently**: once saved a project is a
SQLite file and every edit is committed as it happens, so only a never-saved,
non-empty project (`editor.hasUnsavedWork`) can be lost — New, Open, the window's
close request (`onWindowCloseRequested`) and the updater's *Restart now* all
confirm first (the plugin replaces `window.confirm` with an async one, so it cannot be used as a
guard). **A confirm names its buttons as verbs** (`confirm-copy.ts` is the copy deck as data): the
unsaved-project prompts are `askUnsaved` — a native three-button dialog, **Save… · Don't save ·
Cancel** (Discard on New / Open), "Save…" opening the save dialog and going ahead only once a file was
written (`proceedAfter`), and a dismissed or unrecognised answer always Cancel so nothing is lost; the
rest are `confirmAction(message, title, { ok, cancel })` — *Restart anyway*, *Reset all*, *Apply and
replace*, *Replace* — where `labels` is a required argument, so a Yes / No prompt cannot be asked. A platform that cannot show
named buttons falls back to the plugin's own two-button `ask` with the same labels (without Save); the browser harness has `window.confirm`. A
render is `editor.exportRun` (progress + cancelling), not dialog state: closing
the export dialog mid-render leaves it running, the status bar shows it with a
**Stop**, and reopening the dialog shows the same bar. The `invoke` wrapper
rejects any argument holding NaN / Infinity (an emptied `<input type=number>`),
which JSON would turn into `null` and the backend into an opaque deserialize
error; the Inspector's number fields also snap back to the clip's value when
the entry is empty, negative or clamped, and keep a title's End after its Start.

Every toast is also a **notification log** entry (`src/lib/notifications.svelte.ts`,
a fourth runes singleton). Components import `toast` from *there* rather than from
`svelte-sonner` — a drop-in wrapper, so no call site changed — because a toast is
gone in four seconds, which is fine for "Clip copied" and useless for the model
download that failed with a reason worth reading. The title bar's bell opens
`NotificationCenter.svelte` (All / Unread / Problems as `Chip`s, per-row read toggle, `Mark all read` and `Clear`
as `Btn sm` — the panels' one 28 px scale, no longer 22 px pills) and badges the unread count, red when anything unread actually failed.
Errors and warnings also linger longer on screen than sonner's default. **The log keeps a notice's
`description`** (the reason under a failure — "permission denied (os error 13)") and shows it under the
line; the panel takes focus when it opens and Escape gives it back to the bell. A failure toast leads with what was being done
and then says why — `Export failed — Permission denied (os error 13)` (`failure-copy.ts`: the backend's
`invalid argument:` / `media engine error:` layer off, the first letter up; `Couldn't …`, never `Could
not`) — and a finished file is named by its file, `Exported cut-1x1.mp4` with *Show in folder*. The log is
deliberately *not* replayable — a toast's "Undo" action is dropped rather than kept,
since an hour later it would undo whatever the newest revision is, not the edit the
notice was about. It is also why the failure paths that used to reject into nothing
(`fetchSpeechModel`, `analyzeQueue`'s per-asset catch, the media bin's `runAnalysis`
calls) now report: a notice that is never raised cannot be recovered from a log.

**Keyboard shortcuts are an action registry, not key checks.** `src/lib/keymap.ts`
(pure, bun-tested) names every shortcut as an action — id, label, group, default
chord(s) — and `+page.svelte`'s one window handler asks `settings.actionFor(e)`
which action an event is and runs that id's entry in a `Record<ActionId, handler>`
(an action without a handler is a type error; a handler returns `false` when it
did not take the key). Nothing else spells a key: menus and tooltips read
`settings.shortcut(id)` / `withShortcut(label, id)`, and `keymap.test.ts` scans the
sources so a hand-written `(⌘Z)` or `shortcut: 'Del'` fails. Chords match what the
key *types* (`KeyboardEvent.key`, so AZERTY / Dvorak get their Z; a non-ASCII
character falls back to the physical key's US letter; Shift is dropped from
punctuation, since `+` is Shift+= on one layout and bare on another; a key that
would not read back from its stored spelling — `ß`, macOS's no-break space for ⌥Space
(which is `Space`) — is never recorded as something that silently vanishes). `Mod` in the
stored spelling is ⌘ on macOS and Ctrl elsewhere, and a chord means *exactly* its
modifiers — the old handler ignored extra Shift/Alt and took ⌘ or Ctrl everywhere;
`keymap.test.ts` holds the defaults against a copy of it (the differences: ⌘⇧S
stays Save as a second default, ⇧J-style accidents and Ctrl-on-Mac are gone), plus the
bare keys added since (`ADDED`: N / Y / U / Q / W / S) and the modified chords added for linked A/V
(`ADDED_SHIFT` ⇧D detach, `ADDED_MOD` ⌘L link, ⌘K split, ⌘G go to timecode and ⌘T add title, `ADDED_MOD_SHIFT` ⇧⌘D reattach / ⇧⌘L unlink). **Split at playhead** is `edit.split` (⌘K; Edit menu and the clip menu, which says why when it is off): the selected clips the playhead is inside, or — nothing selected — every clip under it, cut on the frame it is on, a link group as one cut (`planPlayheadSplit`, `key-edits.ts`; one Undo per cut made). **Roll, slip and slide have keys**, a frame a press (`repeat: false`, since each press is a revision): `<` / `>` roll the cut nearest the playhead (the playhead follows it), `Alt+,` / `Alt+.` slip the selected clip's footage, `Mod+Alt+,` / `Mod+Alt+.` slide it — each tried first on a scratch copy through `previewEdit` (the drags' own port), so a limit or a locked track is a sentence, not a refusal from the backend. **`?` opens the shortcut cheat sheet** (`ShortcutSheet`, `shortcut-sheet.ts`: every key in force, grouped as the settings list is, read-only; Help › Keyboard shortcuts…) and *Customize keyboard shortcuts…* is the rebinding editor (`app.keyboard`) — the two used to be one dialog. The registry's *Save project…* is one label for one command; the File menu says *Save project as…* once the project has a file.
**A long timeline lane culls its clips** (`frontend-timeline.md`): a clip outside the visible window, or a narrow one drawn on the lane's canvas, has no button, so Tab and the clip keys reach only the clips in view; scroll or zoom to one first.
**Only what the user changed is stored** (`Settings.keybindings`, opaque to Rust
like `theme`: `{ version, bindings: { id: [chord…] } }`, patch-written, `null` when
nothing is customised), so an untouched action follows the running build's defaults
and changing a default needs no migration; `KEYMAP_VERSION` / `MIGRATIONS` carry a
customisation across a rename or split, and `parseKeyOverrides` drops unknown ids
and unreadable chords and forgets anything equal to the defaults (a stored `[]` is
a deliberate unbind). `resolveBindings` keeps what fires unambiguous: a customised
chord beats another action's *default* (a later build's new default never steals a
key already in use), and any other collision goes to the earlier registry entry.
An action marked `repeat: false` (paste, duplicate, marker, ripple toggle, play /
pause, the file commands, …) acts once per press — the page swallows a held key's
auto-repeat — while stepping, zooming and undo keep repeating.
**Settings › Keyboard** (`KeyboardSettings.svelte`) is search, click-to-record
(Esc cancels, Backspace removes, Tab leaves), a conflict prompt naming the other
action with Swap / Unbind / Cancel (`applyRebind` never guesses; Cancel has the
focus, so a held Enter cannot answer it), per-row Reset — on offer whenever the
chords in force differ from the defaults, and asking the same question when another
action has since taken one (`applyReset`) — and Reset all, and a read-only list of the keys that are *not* rebindable (Esc
abandoning drags and closing menus and dialogs, Tab, Enter / Space on a focused
control, a widget's arrows, wheel and click modifiers). Focus goes back to a row
after every change: focus left on the page behind a modal stops Escape closing it.
**Presets** (`KEY_PRESETS` in `keymap.ts`): Kerf (the defaults), Premiere Pro, DaVinci Resolve and
Final Cut Pro, each an override table over existing action ids (a `mac` table where the Mac's keys
differ), applied as ordinary overrides (`presetOverrides`), so resolving, conflicts and per-row Reset are
the usual ones. The Keyboard section's **Preset** select shows the one in force (`presetOf`, *Custom* when
none) and replaces stored changes only after a Replace / Keep question. `keymap-presets.test.ts` holds
every preset to known action ids, readable chords and no collision on either platform. Left out, and why:

| Preset | Not mapped | Why |
| --- | --- | --- |
| Premiere Pro | Ripple Edit / Rate Stretch / Pen / Hand / Zoom / Type tools, Add Edit (⌘K), Insert / Overwrite (, .), Lift / Extract (; '), next / previous edit (↑ ↓), 5-frame steps, workspaces (⌥⇧1–9), Duplicate (⌘⇧/), apply transitions (⌘D, ⇧D) | No such action in Kerf; ⌥⇧ / ⇧ + digit types a symbol, which a layout-following chord reads as the symbol; ⇧/ is not a chord. Ripple mode takes B. |
| DaVinci Resolve | Pages (⇧2–8), Trim mode's roll / slip / slide split, Blade at playhead (⌘B), Insert / Overwrite (F9 F10), next / previous edit, Loop (⌘/) | No such action, or ⇧ + digit (as above); the Trim tool (T) is Roll. Trim start / end are `{` / `}` (what ⇧[ / ⇧] type on a US layout), which a layout that needs AltGr for them cannot press. |
| Final Cut Pro | Position / Range / Hand / Zoom tools, Blade at playhead (⌘B), Connect / Insert / Append (Q W E), 10-frame steps, Share destinations | No such action; ⇧← / ⇧→ stay a second. |

**Settings** are their own runes singleton (`src/lib/settings.svelte.ts`) behind the title bar's gear (⌘,). `SettingsDialog.svelte` is a
rail of **nine sections** — General, Appearance, Editing, Preview & playback, Performance, Analysis, Agent, Keyboard, Advanced; it opens on
General — and a **search box** on top of the rail. A section is a component of its own (`Settings<Name>.svelte`) made of `SettingRow`s
(`id`, label, hint, control right or stacked), and `settings-index.ts` lists every row's id with the words that should find it: a query
hides every row it does not match (`useFilter()` context, `settings-filter.ts`), the panel then shows each section with a match under its
heading (counts on the rail; a rail click scrolls to the section), Escape clears the box before it closes the dialog.
`settings-index.test.ts` scans the section files and holds rows and index to each other, so a row that cannot be found, or an entry
nothing answers to, fails a test. Where each setting lives (**every control is wired end to end**):
**General** — *Time display* (`time_display`: timecode / seconds; `settings.setTimeMode` sets `timeDisplay.mode`, which every readout
follows, and is read back on launch), *Reopen the last project* (`reopen_last_project`) and *Recent projects* (the list `recentProjects`
prunes, with *Clear* behind a confirm; the same list is File › Open Recent). **Appearance** — theme presets, name and scheme, Import… /
Export… (their own row, so they no longer wrap), then the shape sliders and every colour token (a colour edit applies at once and is written
300 ms later; changing any makes the theme `Custom`). **Editing** — the defaults a fresh session starts with, one opaque JSON object
`Settings.editing` (`{snap, still_seconds, title_seconds}`, `editing-prefs.ts`, read field by field and clamped, never trusted):
*Snapping on at launch* (sets `ui.snap` once on load and again when changed; the toolbar toggle stays the session's), *Length of a still
image* (`placementOut(asset, still_seconds)` is the source end of every single placement — the bin's *Add at playhead* / *Append*, the
Inspector's, the empty Preview's and a bin drag — *Append all* is the backend's `concatenate` and keeps its default) and *Length of a new
title* (`addTitleHere` — the titles lane's +, *Edit › Add title at playhead* — and *Add text*). **Preview & playback** — proxy size and preview source (`ChoiceChips`, the one radio-chip
component: one tab stop, arrows move; proxy size only moves focus because choosing one rebuilds every proxy), safe areas, the GPU preview
and its status line. **Performance** — the CPU limit as three named budgets over a slider, read back as "9 of 12 cores for Kerf · 3 left".
**Analysis** — a master "analyze on import" and one switch per kind (`auto_analysis`; the same set decides what an agent's `analyze_asset`
runs when it names no steps), the **speech model** (select, status, *Download now* with a progress bar showing bytes and a **Stop**:
`ui.fetchSpeechModel` / `ui.stopModelDownload` over `model-progress`, a stopped download is a notice, not an error) and the voiceover
model. **Agent** — *Review the agent's edits* (`agent_review`; turning it **off** asks), *Let agents connect* with the listener's state
(`mcpStatus`: off / listening on URL / why it is not / starting) and when an agent last came, the *address* (validated before it is sent,
`addressProblem`; the server restarts by itself; `KERF_MCP_ADDR` in the environment wins and the row says so — a restart-required note
rather than a silent no-op; a network address needs the token), the *access token* (require switch; masked until *Show*; *Copy*;
*New token…* asks; **a `token_problem` — the token could not be saved or made — is said under the switch and in `McpConnect`**, `?tokenproblem=1` seeds it in the harness), and **Connect an agent** (`McpConnect.svelte`, also the Agent panel's Connect card: the endpoint plus per-client setup
— Claude Code's one-line command with the `Authorization: Bearer` header, a `mcpServers` JSON for Cursor / Windsurf / Cline / Zed, VS Code's
`.vscode/mcp.json`, Claude Desktop through `mcp-remote`; `mcp-clients.ts`; the token is dots in the snippet until *Show token*, a copy
always takes the real text). All of it goes through `setMcpSettings` / `mcpConnectionInfo` / `regenerateMcpToken`; **turning the server off or the token requirement off asks** (`CONFIRM.serverOff` / `tokenOff`), and a switch whose write is refused puts its box back (`flip`: the box moved by itself, the state it shows did not). A change that restarts the listener (address, switch, token requirement) is followed with `awaitListener` — read again until `listenerSettled` (up / off / failed), at most five seconds — and the address says what happened (`addressNotice`: listening on it, why it did not, `KERF_MCP_ADDR` wins, or *Address saved* while it is still starting), never what was hoped for. A `settings-changed`
event (an agent's `set_settings`) carries the `SettingsView` and is `settings.noteChanged(view)`; one without it (`null`) reads the settings again. Every on/off setting is a **`Switch`** (a 16 px native checkbox in a 32 × 28 px label, so the press need not land on the box) and its `SettingRow` takes `for` so the row's name toggles it. **Keyboard** — described above.
**Advanced** — *Open logs*, *Show the settings file* (`reveal_settings_file`), *Reset layout* (every workspace to its preset, the question
Window › Reset all workspaces asks: `resetAllWorkspacesAsked`) and *Reset all settings* (confirm; `reset_settings` keeps the old file beside
the new one and the agent door — address, token — as it was; `settings.resetAll()` reads everything that is read once again and the dock
is rebuilt, `workspace.rebuild()`). The percentage is clamped by the engine, so the view that comes *back* from `set_settings` is what
renders, not the value asked for; in the browser harness `api.ts` answers from localStorage (`kerf.settings.*`, the layout, theme,
workspaces, keybindings and editing defaults as JSON strings) over `navigator.hardwareConcurrency` so the dialog is drivable under
`bun run dev`.

**Media that has gone, media taken out** (`library-actions.ts`, words and counts in `library-copy.ts`). `mediaStatus.missing` is what `missing_assets` says
(read with every status refresh and once more when a project comes up: `checkMissingOnOpen` opens **`RelinkDialog`** by itself when anything is
gone). A bin card carries a `missing` badge, the bin a banner with *Relink…*, the asset's Inspector a callout, and the bin's menu *Relink to another
file…* / *Remove from project…*. **Relink** (`pickRelinkPath` → `check_relink` → `relink_asset`) keeps the asset's id, place and every clip; the dry run refuses a file that cannot stand in before
anything is asked, and for a different recording of an asset that *has* analysis asks first (`relinkDropsAnalysisCopy`); it says afterwards that the analysis was dropped. The dialog
speaks of one file or several (`relinkIntro`), and each row's buttons carry the asset's name (`Locate broll.mp4…`). `?relinkto=other.mp4` makes the harness picker choose a different file. **Remove** confirms: unused media is a plain question; media a clip (or the open proposal —
`getStagedTimeline` is counted) uses says what goes with it and passes `force`, one `Remove media` step that cannot be undone (the media is gone);
both are held while a proposal is previewed.
**Partial accept** (`review-picks.ts`, the Agent panel's card): every change has a checkbox (and each group header a tri-state one, library changes
a `Library` group); nothing ticked is the whole proposal. Ticked changes go to `apply_staged_subset` (`editor.applyStagedSubset` → `#apply` with
`review`, the proposal closes only when the backend says `closed`; a stale cut asks first, as for Apply). **A linked picture and sound go together**: the card says
*Comes along — linked to a clip you picked* beforehand (`comingAlong` runs `closeOver` — the one closure `staged-subset.ts` also takes before it merges, the port of `merge_subset`: links, the new track, and the moves that leave a track picked for removal — over the live cut and the proposal; the base is not on the page, so the live cut stands in for it) and the toast
is the backend's own sentence (what came along, what no longer applied, how many are left). The Apply button says `Apply 2 of 4`. *Undo step*
(`undo_staged_edit`, a small icon beside the badge) takes back the agent's latest step inside the proposal. **A discard is restorable**: the toast's
Undo and the panel's *Restore* card (`editor.discarded`, read whenever no proposal is open — a read that fails or was overtaken by a project load / a new proposal leaves no card; an edit re-reads it, so the card says *The timeline has changed since* and *Restore* asks first, `CONFIRM.restoreStale`) call `restore_discarded_edits`. Core drops the kept copy when a newer proposal is applied or the project is replaced (`core-project.md`).
**Export asks before it replaces a file** (`export-overwrite.ts`): the files the render will write (the path, or one `-9x16` sibling per delivery shape)
go through `pathsExist`; the file the native save dialog returned is skipped (it asked), every other one that exists is asked about once.
**Music structure** (`MusicStructure.svelte`, `music-structure.ts`) is the Inspector's view of `get_music_structure`: the energy curve (`getEnergy`, asked
once per asset), a line per bar, the intro and a coloured band wherever a stretch repeats (the longest repeating phrase wins over the halves inside it);
under the clip's *Music* section and the asset's *Music structure*.
**File › Open Recent** is `settings.recent` (`recentProjects`, pruned; refreshed on open and save) in a submenu with *Clear recent projects…* (asks); the page runs
it (`onMenuCommand` → `openProjectAt(path, true)`, the unsaved-work question included). **Jobs you may stop** (`job-line.ts`): the status bar shows
the one in front — framing the shots (`smart-crop-progress`, `cancel_smart_crop`), a cover frame, a speech model download, an import (also a ✕ in the
library header and a Stop on the empty bin) — with its percentage and **Stop**; a stopped run is a notice (`isCancelled` in `report`), not an error.
**Long lists are read a page at a time** (`paging.ts`): `getHistory(limit)` / `listTasks(limit)` return the newest 100 edits / 30 tasks, the Agent panel
says `100+ edits` and *Show older edits* / *Show older tasks* ask for more; undo / redo cope with a head older than the page (`undoable`, `redoable`).
**The refusals the engine gained are mirrored** (`clip-limits.test.ts` replays `review_tests.rs`): `link-ops.ts` `addClip` / `trim` / `setSpeed` refuse
footage past the end (`SOURCE_EPS`), a negative start, a clip left on another of its lane *after* the ripple (`checkOwnLanes`, `runEdit`'s `lanes`) and a
speed outside `MIN_SPEED..MAX_SPEED` (a linked partner too); the harness runs them, and the Inspector's typed In / Out asks `trimRefusal` first so the
sentence is the backend's, said at once.

The **update flow** is its own runes singleton (`src/lib/updater.svelte.ts`,
alongside `editor`/`ui`/`agent`): it runs a *silent* check at startup and every
6 h through `api.ts`'s `checkUpdate` / `installUpdate` / `relaunchApp`, and drives
`idle → checking → { current | available → downloading → ready } | error`. The
title bar's version chip turns into an amber "⬇ 0.18.0" button when something is
available and opens `UpdateDialog` (release notes, download progress, then
**Restart now** — which warns first when the project has unsaved changes (the same fact in the dialog's
banner). The download **holds the dialog still** (`busy`, no Hide): on Windows the plugin's `downloadAndInstall` launches the
installer and exits the app when it is done, past every close guard, so a hidden download could take unsaved work and a running
export with it, silently. **Install asks first, on Windows only** (`installPrompt(risk, isWindows())`, `confirm-copy.ts`; macOS and Linux do not exit after the
download — the update waits for *Restart now*, which asks about unsaved work itself — so they keep the plain flow and the neutral footer
`downloadingNote`): with an unsaved project the three-button *Save… · Install anyway · Cancel*, with only an export, an analysis or an import
(or 360 stitch) running a two-button one saying it will be stopped, and nothing at stake no question; `install()` itself ignores a second press while one runs. The line over the body reads `Current 0.24.0 → 0.25.0 · 1 Oct 2026`
(`versionLine`, not "Installed … →"); a failed *check* is titled
"Couldn't check for updates", never "up to date", says so in a sentence with the raw error under a
*Details* disclosure, and the AppImage fallback note shows only for a failed install on Linux
(`isLinux`, `keymap.ts`) — `update-copy.ts`); the
dialog auto-opens the first time a given version is seen (remembered under
`kerf.update.seen` in localStorage) so declining doesn't nag every launch. The
`Update` handle the plugin returns stays module-local in `api.ts`, which hands the
UI plain data — so the browser harness can fake the whole flow: `bun run dev`
with **`?update=1`** offers a synthetic 0.99.0 and simulates the download, making
the dialog explorable without a signed desktop build.
`bun run dev` with **`?staged=1`** seeds a synthetic agent proposal (a tightened
intro) *and* the ready task it belongs to (without the flag there is no ready task: a card offering to apply a change that does not exist), which is how the whole review flow — card, preview swap, apply, discard — is
driven end-to-end without a desktop build; its `stale` flag is computed as the backend's is (the history head is not where it branched), so editing first makes Apply ask. **`?empty=1`** is a first run (an empty library, a cut with its two empty tracks, no queue); **`?slow=1`** makes the simulated analysis steps last 2.4 s each, with progress ticks, so the step counter and Stop can be looked at (with `?music=1` as well it also removes the song, so there is a removal
to draw). **`?music=1`** lays the harness's song (`music.mp3`,
always in the library, with a bar grid) on its own track A2 — bar ticks on the ruler and
*Fit to video* (see frontend-timeline.md) are then one right-click away.
The library and review commands the desktop app answers have harness twins with the backend's wording:
**`?missing=1`** leaves the b-roll and the song without a file (`missingAssets`, `relinkAsset` — kinds judged by
extension, analysis kept only for the same file name — and `removeAsset`, which refuses a used asset without `force`
in the backend's words), **`?recent=1`** seeds `recentProjects`, **`?exists=1`** makes `pathsExist` answer yes (the
overwrite prompt), and the staged proposal carries core's entry ids so `applyStagedSubset` (the port in
`staged-subset.ts`, tested case for case against `model/subset.rs`), `undoStagedEdit`, `discardedStagedEdit` and
`restoreDiscardedEdits` run on it. Playback ids are `session * 2^32 + n` (`playback-id.ts`, session from
`playback_session`; see app.md).
`data.ts` keeps only the `STATUS_MAP`/`PRESETS` presentation bits —
all project data renders from the real backend.

`src/lib/api.ts` is the backend bridge: `inTauri()` decides between `invoke(...)` and a
**seeded in-memory sample with working local timeline ops**, so every edit/analysis/waveform
is explorable in a plain browser via `bun run dev` (frames return `null` there → Preview
keeps its placeholder; `getWaveformRange` answers from `src/lib/sample-waveform.ts`, a
deterministic stand-in shaped like the engine's pyramid read — stereo or mono per the
asset, zeros outside the media, the analysis's silences as a noise floor, and a clipped
stretch so the clipping colour is visible; `getFilmstrip` answers from
`src/lib/sample-filmstrip.ts` with a strip of the engine's *shape* — generated SVG
sheets labelled with each thumbnail's source time and index, black padding after the
last one — whose geometry comes from `src/lib/filmstrip-geometry.ts`, the faithful,
bun-tested mirror of the engine's interval ladder, thumbnail width, sheet layout, plan
and `Filmstrip::frame_at` / `locate` (the lookups a consumer makes against a real strip
too)). **Ripple in the harness is a port, not a
lookalike**: `src/lib/ripple.ts` is the *faithful*, bun-tested mirror of
`Timeline::ripple_from` (its test replays the Rust tests case for case, same clips and
numbers, so a rule changed in kerf-core has to change there or a test names it) and
`src/lib/multi-edit.ts` the same for `Timeline::move_clips` / `remove_clips` (same
checks, same messages), and `src/lib/edit-modes.ts` for the edit modes
(`rollEdit` / `slipClip` / `slideClip` / `splitRemove` plus their `*Range`
functions — the clamp a drag holds the pointer to — replaying the Rust tests, messages
included; `api.ts` runs them in the harness, `editor.roll` / `slip` / `slide` /
`splitRemove` / `splitRemoveClips` are the thin actions over them). **Linked A/V is the same
arrangement**: `link-groups.ts` + `links.ts` mirror `model/links.rs`, the `*Linked` modes live
in `edit-modes.ts`, `conformLinks` (the sync lock) in `ripple.ts`, and `links.test.ts` replays
the Rust cases name for name; **`link-ops.ts` is the pure mirror of `Project`'s link-aware ops
and of `run_edit`** (`runEdit`: scratch copy, per-lane ripple, sync lock with the named clips as
anchors, guard, orphan dissolve), which `api.ts` composes through `devRun` (a scratch copy, so a
locked partner leaves the harness untouched, like the backend), every edit taking an optional
trailing `link` (`false` = the named clip alone), plus `detachAudio` / `detachAudioClips` /
`extractAudio` (answers `AudioDetached {timeline, detached, skipped}`) / `addAssetAudio` /
`reattachAudio` / `reattachAudioClips` / `linkClips` / `unlinkClips`; `links-corpus.test.ts` replays kerf-core's own answers
(see Linked A/V above) through it. `audio.ts` schedules no clip with
`source_audio === false` — scheduling both a picture and its detached sound *is* the
doubling; the editor chrome for it is the Linked A/V paragraph above. **The harness cut starts
detached-and-linked** (V1 `c1` silent with `link_id`, A1 `c3` its sound for the same span — heard once;
`Project::sample` detaches the same way but then unlinks, because the kerf-core tests built on it
edit one clip at a time), so `bun run dev` shows the feature; tests that want the old shape start
from `reattachAudio('c1')` or `unlinkClips`. `api.ts` keeps the project's ripple flag in the harness state
(`getRippleMode` / `setRippleMode`; not an edit, no revision) and runs every local edit
that can change how much footage sits ahead of a clip — add, split, trim, speed, remove,
voiceover placement — through `devEdit`, `edit_timeline` in miniature (snapshot, edit,
`rippleFrom`), while the layout-deciding ones (move, reorder, ripple delete, cut range,
beat snap, paste) skip it as in the core; `moveClips` / `removeClips(ids, ripple?)`
reject as the backend does and leave nothing behind. `api-ripple.test.ts` drives it all.
`music-fit.ts` is the same arrangement for fit-to-length (`plan_music_fit`, `music_fit_clips` and
`Project::fit_music`'s checks, messages included; `music-fit.test.ts` replays the Rust cases), behind
`planMusicFit` / `fitMusic`.
**Composed music and band balance.** `api.ts`: `musicStatus` / `prepareMusic` (the SoundFont, ~32 MB) / `composeMusic(ComposeRequest)` →
`ComposeResult` (`Composed` + the refreshed `timeline`) / `cancelMusic` / `onMusicProgress` (`music-progress`:
`download_soundfont`, `waiting`, `render`, an agent's runs included), `getBandBalance(BandBalanceRequest)` →
`BandBalance` / `cancelBandBalance`; a stopped run rejects `music cancelled` (`MUSIC_CANCELLED`) /
`band balance cancelled` (`BAND_BALANCE_CANCELLED`), which a dialog stays quiet on. The ids are snake_case
inside the request (`track_id`, `replace_clip_id`), unlike the camelCase arguments of the older commands.
`music-score.ts` is the faithful mirror of the score's pure half — `scoreDuration`, `sectionSpans`,
`validateScore` (the engine's refusals, word for word, naming the section or chord; a seed over 2^53 - 1 is refused
too, `MAX_SEED`), `chordError` / `keyError` (the grammar only), `resolvedChords` (the chords **as played**: the last bar
of an outro that ends the piece is the tonic, as `resolve()` has it) and `planScoreToPicture` (validates the score
going in and the one coming out; never picks a tempo that ends the piece before a hit it already has; stops adding
hits at `MAX_HITS` and says how many it left out) — replayed from the Rust cases in `music-score.test.ts`, for the
dialog's live length and errors and for the harness. `music-place.ts` mirrors the placement in `project/music.rs`
(`musicBed`, `musicStart`, `putMusic`, `rehearseMusic`; `music-place.test.ts`) and `music-wire.ts` the structs' `deny_unknown_fields` (serde's "unknown field `x`, expected one of …" in Tauri's "invalid
args for command" frame, nested structs included). Under `bun run dev` `composeMusic` fakes the SoundFont download and render ticks and
lands an audio-only asset as long as the score on the `M` track (the same score is the same asset by path; its tempo
and bar analysis written from the score, from the chords as played), aligns to the first video track's cuts or the
markers, and refuses what the engine refuses. A placement is rehearsed on a copy of the cut with the render's length
**before** the render (a piece over another clip of its track — `{at: 0}` and then `{at: 2}` on `M` — is refused with
`the clip would overlap another clip on M at … — move or trim that one first`, unless ripple is on and only pushes it)
and checked again when it lands, as one edit that ripples like any other (`runEdit` with the clip as a lane); the asset
and its analysis are stored only after that, so a refusal leaves neither. `replace_clip_id` replaces the **whole music
bed** — every clip of that asset on the track — with one clip at the earliest start, the named clip's id, the first
clip's level / fade-in / effects / keys and the last one's fade-out (`Composed.replaced` is the ids, in time order,
`[]` for an added clip); a locked track, a bed linked to a picture and a clip that is not composed music are refused in
the core's words. `getBandBalance` picks its windows as `project/music.rs` does and words its
`notes` as `balance_notes` (`band-balance.ts`) but the measurement itself is made up (in a browser it takes
~0.6 s and honours Stop; under bun it is at once). `api-compose.test.ts` drives the bridge.
**The Compose music dialog** (`ComposeMusicDialog`, `ui.composeDialog`, `ui.openCompose(prefill?)`, inert-behind like
the voiceover's) is the voiceover's arrangement: `musicStatus` first, saying what the first piece downloads
(`soundfontNote`), then `music-progress` stages (`musicStageLabel`) under a **Cancel** (`cancelMusic`; `music cancelled`
stays quiet and returns to the form); failures are `Composing failed — …` toasts, `PreviewReadOnly` a notice. It cannot
be dismissed mid-run (`busy`) and holds what was typed (`dismissOnScrim={false}`, like Export). The form is
`compose-form.ts` (pure, bun-tested): style chips (a style moves the tempo only while it is still the old style's
default: `setStyle`), tempo, key, **Variation** (the score's `seed`, with a reroll button), and a **section list** —
kind, bars, a **chords line** (`I V vi IV`, `C | G | Am | F`; an arrow or dash between chords is not a chord:
`parseChords`), optional energy (the placeholder is the kind's default), up / down / remove and a start time. Faults are
shown per field as typed (`formIssues`: chord and key messages are the engine's, from `chordError` / `keyError`;
`general` is what only the whole score can say — no section, over 400 bars / 600 s, a kept hit past the end — and
`validateScore` is the oracle: a bun test holds `formIssues().any` to `validateScore() !== null` over a table), with the
live total (`24 bars · 54 s`); Compose's tooltip names the first (`firstIssue`). **Fit to the picture** lands on a video
track's *cuts* (a picker when several have any) or the *markers*, with a *Tempo change* slider (0–25 %, 6 %); it is off,
with the reason, until there is a cut or a marker (`alignChoices` / `alignProblem`, the cut count of `cut_times`). Hits an
earlier alignment wrote are kept (*Clear hits*): they sit at fixed times in the score. *Place*: after the music on `M`
(default) or at the playhead. With an alignment the dialog stays on a **result** (cuts on a beat / downbeat before →
after, the tempo change, hits added, the engine's notes: `alignRows`) and Done; without one it closes on a toast with
Undo. Either way the new clip is selected (`editor.composeMusic`, which also refreshes the assets and loads the analysis
the core wrote) and the playhead goes to its start unless it is already inside (`revealAt`). `Composed.replaced` is read
through `wasReplaced` (the ids of the clips replaced). Re-composing replaces the **whole music bed** — every
clip of that asset on the track, the segments Fit to video made of one piece (`musicBed`, `replaceNote`) — and is off,
with the core's reason, on a locked track or a bed linked to a picture (`composeOffer`). Reached from **File › Compose
music…** (`file.composeMusic`, unbound, held while a proposal is previewed), the Media tab's header (a music icon beside
the microphone) and empty bin, the Audio tab's *Music* block, the bin's **Edit & compose again…** (an asset's score, no
clip to replace) and a clip's right-click and Inspector **Edit & re-compose…** (`replaceClipId`; the button reads
*Re-compose*). The Titles tab does not offer it. An asset with a `music_score` — in the Inspector for a clip, and for a
library asset — has a **Composed music** section (`ComposedMusic`): the score's facts, its sections as a contour (width
by bars, height by energy: `sectionContour`), the edit button and **Band balance**: *Measure* calls
`getBandBalance({ clip_id })` (`{ asset_id }` without a clip), *Stop* `cancelBandBalance`; the answer is a row per window,
the whole stretch first, with a bar per band (sub, bass, L-M, mid, H-M, air: its share of the window's energy, the
dominant one lit, one scale for every row; `band-view.ts`) and the loudness, then the engine's notes. It is dropped when
the clip, its asset, trim or level changes, and cancelled when the section goes away.
This browser sample is a **dev harness only** — the desktop app always
uses the real backend and starts empty. State is two runes singletons: `src/lib/state.svelte.ts`
(`export const editor` — assets, timeline, analyses, selection, and the editing actions that
call the backend and apply the returned `Timeline`) and `src/lib/editor-ui.svelte.ts`
(`export const ui` — chrome state, playhead/zoom/playback, and `analyzeQueue` /
`runAnalysis(id, steps?)` / `stopAnalysis`: a batch analyzes **one asset at a time** — each pass is
ffmpeg-bound, so running them together only makes each slower — and stopping drops
the whole rest of the queue. `analyzeImported(ids, auto)` is what an import calls: nothing when the
master is off, else per asset only the kinds that are on and **not already done** (`autoSteps`; a
re-imported file redoes nothing, a failed kind is retried, transcript needs a backend), and
`ensureAnalysis(id, needed)` is what the Agent panel's quick edits call (`autoSteps` also skips what a file
cannot give: silent b-roll gets scenes only, a still nothing) — — Remove silences runs only
`silence`, Cut to the beat only `rhythm`, Caption the cut only `transcript` (`QUICK_EDIT_STEPS`).
`runAnalysis` re-reads the statuses afterwards and toasts each requested step that failed, once). There is **no scripted demo phase machine**: the
editor chrome derives from real state — `MediaBin` shows a dropzone until `editor.assets` is
non-empty, `StatusBar` names two things apart (`status-facts.ts`) — the **project** (`30 fps · 1920×1080`: the delivery frame, else the first picture on the cut) with the cut's length, and, after a rule, `Selected broll.mp4 · 24 fps · 3840×2160 · h264`, the library asset's own facts, which it used to print unlabelled beside the cut's timecode — (plus the analysis step, what is still queued behind it and a **Stop**), and
`Preview` shows the decoded frame or, with nothing on the timeline, the first-run card (`PreviewEmpty`, below).
Each **bin row is the asset's specs**, not just its name: a real decoded frame
(`get_frame` 10% in, cached per asset across re-docks; the icon stays in the
browser harness, which has no decoder), the name with its `m:ss` duration beside it, the spec line
(`1920×1080 · 29.97 fps · h264 · stereo`), then one line of status — badges for 360 / still / how many
clips already use it, and **`MediaChips`** in its `compact` form: the preview proxy's badge (`queued` / `proxy 42%` / `proxy` /
`proxy failed`, tooltip carrying the reason; nothing for a still or audio file) and **one analysis pill** — `analyzed`,
`3 of 5 analyzed`, `not analyzed`, `analyzing…`, `1 failed` or `analysis off`, its tooltip listing the five kinds with their states (`analysisMeter`; a
kind the file cannot give is not counted: `applicable` / `chipsFor`). State is a mark and a tint as well as a colour. The Inspector's
clip header draws the five kinds *as words* (`Silence` `Scenes` `Loudness` `Rhythm` `Transcript` — the two-to-four letter tags needed a tooltip to be read: done ✓ green, not run outlined, running spinning, failed `!` red,
off dashed `–`; minus the kinds the file cannot give) from the same store plus an **Analyze** button — which, while a run is on that asset, is replaced by **`AnalysisRun`**: `step 2 of 5 · detecting scenes 35%` and a **Stop** (the same on the bin card, and the status bar's label carries the counter; `stepCounter` in `analysis-steps.ts`, the plan being the `steps` the run was asked for — unknown for a run left to Settings' switches, which counts nothing; the speech model's download is the transcript's step), and the timeline clip
menu lists how many are done and an `Analyze…` entry. The store is `media-status.svelte.ts`
(`mediaStatus`): `proxy-progress` / `analysis-status` events note into it, and it is re-read when the set of
assets changes and after a setting that moves what a status means. The **status bar** says which file
the frame under the playhead is decoded from (`preview: proxy 1280 px`, `preview: original · proxy 42%`,
`preview: waiting for proxy · 42%` under Proxy only, which also puts a badge in the Preview pane —
`previewSourceNote`, judged from the statuses already held, nothing asked per frame). Its **context menu is the actions only** — the facts moved to the Inspector (`AssetInspector`, `asset-facts.ts`: the
frame, rate, codec, audio, projection, the stitched lens pair, the use count, the import date, how much of the analysis has run
and what it found, the proxy's state), which shows the selected asset when no clip or title is selected. The menu: add at the
playhead / append, the audio action — labelled for what the backend will do (`extract_audio` or, for an asset not
playing its own sound, `add_asset_audio`)
(`media-info.ts` `audioExtraction`: `Detach audio from N clips` when clips of the asset still play
their own sound, else `Add audio to A1`, with `(a second copy)` when its sound is already on an audio
track), remove silences (greyed out until silence has been detected), **Separate stems…** (assets with sound; see below), **Analyze…**
(one entry that opens the Analyze menu where the pointer was: one item per kind — "Detect silence", "Find scene changes",
"Measure loudness", "Find the beat and tempo", "Transcribe speech", "Analyze everything" — each saying `done · run again` /
`failed · retry`, disabled with the reason for a voiceover, a running pass, a file with no audio or no speech backend — and
Stop analysis), **Rebuild / Build / Restart proxy** and **Delete / Cancel proxy** (`proxyActions`), mark the asset 360 (a globe)
or flat, copy the path, show in folder. The phrasing is `src/lib/media-info.ts` and `asset-facts.ts`, pure and bun-tested, so the
row, the Inspector and the menu cannot drift apart; `MenuItem` has `header` / `info` rows (the timeline's menus use `info`), which the
shared `ContextMenu` renders non-interactively.
**Stem separation** (Demucs: drums / bass / other / vocals). One modal, `StemsDialog` (`ui.stemsDialog`,
`ui.openStems(assetId, clipId?)`, inert-behind like the voiceover's), reached from the bin's **Separate
stems…** (the stems join the library as `<name> · drums` …, the cut is untouched) and the clip menu's
**Separate stems under this clip…** (also four new audio tracks Drums / Bass / Other / Vocals at the
clip's position and span, and the clip's own sound switched off — an audio clip disabled, a picture's
`source_audio` off — as one `Separate stems` revision with a toast Undo; the four new clips are
selected). `stems.ts` `stemsOffer` is the clip menu's gate: shown when the asset has an audio stream,
greyed with the backend's refusals (locked track; a picture whose sound is on a linked audio clip —
separate that clip). The dialog follows the voiceover's arrangement: it asks `stemsStatus` first and
says what the first run downloads (`downloadNote`: the ~174 MB model, the audio runtime), then streams
`stems-progress` (`download_runtime` / `download_model` / `separate` / `encode`, a bar per stage) with a
**Cancel** (`cancelStems`; the run rejects with `stems cancelled`, which stays quiet and returns to the
form) and a success toast (`describeStems`); other failures are error toasts. It cannot be dismissed
mid-run, and `editor.separateStems` refreshes the asset list on a failure, because the core stores the
stems *before* it places them, so a refused placement still leaves them in the library. Under `bun run
dev` `separateStems` fakes the progress (a first-use "download", then separate / encode), makes four
stem assets and, with a clip, runs `stems.ts` `placeStems` — the faithful mirror of
`Project::place_stems` (same checks and messages, same lanes, sound-off rule), replayed by
`stems.test.ts`; a second run of the same asset finds the stems again by path and reports no
progress, like the app's per-file cache. `api-stems.test.ts` drives the bridge.
**Dropping files onto the window imports them** (`+page.svelte` listens for Tauri's
`onDragDropEvent`, filters by `isMediaPath` — the same extension list the picker
filters by, so a dropped folder of mixed files doesn't answer with one error per
README — and runs the same `editor.importPaths` the picker resolves to), which is
what the bin's "Drop media to start" had been promising. `editor.error` renders as a
dismissible banner under the title bar: it was recorded and never shown, so a `.kerf`
that would not open opened as silence.

The browser harness fakes all of it so the bin, menus and settings can be looked at under `bun run dev`
(`api.ts` `devProxy` / `devAnalyze`): the interview's proxy is building at 42 %, the b-roll's ready;
`?proxy=failed` / `?proxy=queued` make the b-roll's a failed / queued one; Rebuild runs a timed build,
Analyze steps run in ~450 ms each and announce themselves through the same `analysis-status` path.
The pure phrasing is `analysis-steps.ts` and `proxy-info.ts` (bun-tested).

**Proxy only in the preview.** The backend refuses a frame or playback whose clip has no proxy yet with a
message starting `waiting for the preview proxy` (`proxy-wait.ts`, import-free because the frame pump is
compiled alone in its test). The pump treats that as the preview waiting — the picture and the GPU surface
stay, nothing falls back to the JPEG — and `Preview.svelte` shows `playback waits for the proxy` instead of
a `Playback preview failed:` toast, restarting the stream when a proxy lands. A queued export reads
`Export waiting for the preview proxy…` (`ExportProgress.waiting`) instead of a stuck 0%.

**Animated SVG titles in `api.ts`.** `addSvgOverlay(source, start, end?, layout?)` / `updateSvgOverlay(id, patch)` /
`removeSvgOverlay(id)` (each answers the refreshed `Timeline`), `validateSvg(svg)` → `SvgCheck`, `listSvgPresets()` and
`svgPresetPreview(kind, params, duration?)` → SVG text, which the page may only show through an `<img>` data URL.
The harness stand-in is `svg-dev.ts`: kerf-core's refusals (placement ranges, `end > start`, fifty titles, empty
preset text, `svg` xor `preset_params` / `preset_kind`, a refused update changes nothing) over the dev timeline, with
`svg_overlays` absent again when it empties. Preset artwork is **kerf-core's own**, written down in
`fixtures/svg-presets.json` (each preset drawn once for 1920×1080 and four seconds with placeholder text; the
harness swaps in text, colours and font and does not re-time it, and custom SVG is not sanitised —
`validateSvg` only drops and reports scripts, foreign objects and `animateMotion`, but refuses at a glance what the engine
does: over 128 KiB, not XML (the browser's `DOMParser`, guarded because bun has none), a root without
`xmlns="http://www.w3.org/2000/svg"`, no `viewBox` / pixel `width` + `height`; preset colours go through `isSvgColour`, which
reads names, `#rgb[a]` / `#rrggbb[aa]` and `rgb[a]()` / `hsl[a]()` as `svgtypes` does). A preset change in
`kerf-core/src/svg/presets.rs` fails `svg_preset_fixture_is_what_this_engine_does` until
`KERF_BLESS_SVG_PRESETS=1 cargo test -p kerf-core --no-default-features -- svg_preset_fixture` rewrites it;
`api-svg.test.ts` drives the harness. A patch with `svg` also drops `template` (the controls pointed into the old artwork).

**Motion templates in `api.ts`** (surfaces only; the library UI is separate). `listTemplates()` → `TemplateList`,
`templatePoster(id)` → a `data:` URL or `null`, `svgElementIds(svg)` → `[{id, tag}]`, `templateImageValue(path)` → the
`data:` URL an `image` control takes, `saveTemplate(overlayId, name, controls, description?)` → `TemplateSaved`,
`applyTemplate(templateId, values, start, end?, layout?)` and `setTemplateValues(overlayId, values)` → `TemplateEdit`
(`{timeline, overlay_id, warnings}`: the refreshed cut, the title to select, what could not be honoured),
`importTemplate(path)` → `ImportOutcome`, `exportTemplate(id, path)` → where it went, `deleteTemplate(id)`. A title from a
template has `SvgOverlay.template` (`TemplateInstance`: `template_id`, `name`, `controls`, `values`); **`values` is read
back from the title's own `svg` and has no `image` controls**, and changing one is `setTemplateValues`, never a patch of
`svg` (that ends the template). The harness stand-in is `template-dev.ts`: an in-memory library seeded with one sample
template (`SAMPLE_TEMPLATE_ID`; `resetDevTemplates()` puts it back), kerf-core's refusals for controls and values (ids, kinds
and their attributes, a missing or repeated element id, colours without `rgb()`, unitless numbers within `min` / `max`,
partial values, unknown control ids), values spliced into the artwork by element id with the same escaping and read back,
and `import_template` never overwriting; there is no file system, so an exported "file" is kept under its path for
`importTemplate` to read back, and any image path answers one fixed picture. `api-templates.test.ts` drives it.

**Motion templates in the editor.** The library, the saving, the splicing and the sanitiser are the engine's; the page
asks well and shows the result through `<img>` data URLs only. A template title's `svg` holds its text as glyph outlines
(`<g data-kerf-text="">` keeps the element's id, attributes and text): the save dialog's tags come from `svg_element_ids`, which
reports such an element as the `text` it was, and its attribute reads (`elementAttributes`, `holds`) see the same attributes.
**A title's text** (`title-text.ts`, pure, bun-tested): `textForm` is a port of kerf-core's `restore_text` (a token walk:
the marked `<g …>` back to `<text`, its direct `data-kerf-glyphs` child cut, its `</g>` back to `</text>`), replayed against
kerf-core's own outlines by `outline-corpus.test.ts` (`fixtures/outline-corpus.json`, blessed with `KERF_BLESS_CORPUS=1 …
-- outline_corpus`). The Inspector's **SVG source** box shows and compares the text form (`shownSource`), noting that applying
a change to an outlined custom title draws it with its fonts until it is outlined again. **`TextSection.svelte`** (after the
Template section, for every title but a preset, floating or on a track) lists each `<text>` with an id (`titleTexts`: the
box read off its start tag, which an outlined text keeps) under its template control's label or `#id`: a width (empty is no
box; `boxWidthRefusal` before the round trip) and *Wrap lines* / *Shrink to fit*, each commit one `setTitleTextBox` edit
(nothing while locked, busy or previewing a proposal; the engine's notes said as a toast), and for a custom title *Draw as
outlines* / *Draw with fonts* (`convertTitleText`). The harness writes the box (`withTextBox`, `set_text_box`'s splice) and
draws text with its fonts, saying so (`HARNESS_NOTE`). `template-ui.ts` is the pure side (bun-tested): `slugId`
(a control id from its label: lower case, accents dropped, `-2` past a taken one), `defaultAttribute` (a color's `stop-color`
on a `stop`, else `fill`; an image's `href`; a font's `font-family`; a number's usual size), `elementsFor` (text at
`text` / `tspan`, an image at `<image>`; an id that two or more elements carry is **not targetable** — the engine refuses it
— and `ambiguousFor` lists it apart, as a disabled "used by N elements" option, so it is not simply missing), the draft model
(`DraftControl`, `relabel` / `changeKind` / `pickElement` / `editId`: the id follows the label only until it has been edited,
and an id emptied by hand stays empty and says so instead of snapping back to the slug),
`controlProblem` / `checkDraft` (kerf-core's `validate_controls` said before the round trip — the engine still has the last
word: ids, targets and attributes, **text that holds another control's target** — `holds`, a walk of the source's tags with a
stack —, control characters in a name, description or label, lengths in characters of the trimmed text; a **blank name is
allowed**, `build_template` takes the title's own), `valueRefusal` / `validSvgColor` / `normalizeValue` / `plainDecimal` (a
value as the engine takes it: numbers unitless, plain decimal; a color is `valid_svg_color` exactly — four hex lengths, one of
the 148 `svgtypes` names in any case (`svg-color-names.ts`, shared with the harness), `none`, `currentColor` spelled so,
nothing else),
`pickColor` / `swatchOf`, and the words (`savedLine`, `importLine`, `appliedLine`). It reads a title's *source* only for
names and values (`elementAttributes`, `imageHref`: a regular-expression walk of the start tags, so an element is offered
the attribute it has; a picture is shown only if it is a `data:image/png|jpeg|gif|webp` URL). `template-actions.ts` runs
them (`addTemplateHere`, `importTemplateFile`, `exportTemplateFile`, `deleteTemplateFile` — asks first —,
`saveTitleAsTemplate`, `setTemplateValue`, `setTemplateImage`; `posterOf` caches a poster per id) and says outcomes through
`notifications` (the engine's warnings are the toast's description); `editor.applyTemplate` / `setTemplateValues` take
the `TemplateEdit`'s timeline like any edit and the first selects the new title. **Library**: `TemplatesSection` is part
of `TitlesControls`, the Titles tab: an `Import…` ghost button on the
`TEMPLATES` heading, one `TemplateCard` each (poster in the delivery frame's shape, the duration as a chip over it, name,
`N controls`, font families; a click adds it at the playhead; the `⋯` button (named "More actions for <name>") and a right-click offer *Add at the
playhead*, *Export…* — a save dialog for `.kerftemplate`, the harness answers `/harness/<name>` and keeps the "file" for
`importTemplate` — and *Delete…* — confirmed, titles made from it keep working). The grid reads the library when it opens
and on `templates-changed` (`onTemplatesChanged`), which fires for the backend's event (an agent's change: the Tauri
commands emit none) **and** for the page's own save, import and delete — `api.ts` announces them once the call has
resolved, in the app and in the harness alike — so the Titles tab refreshes wherever it is mounted; `problems` are a collapsed one-line `<details>`; an empty library explains how to make one. There is no drag to
the timeline: the preset tiles have none either. **Save as template…** (`SaveTemplateDialog` over `Modal`; `ui.saveTemplateDialog`,
mounted by the page and counted in `modalOpen`): name (blank = the title's own name, as the engine has it; it is the field's placeholder), description, and
a list of controls (`TemplateControlEditor`; every field of a row is named by its control's label, or `control N` before it has
one): label, kind, an id that follows the label until edited, the element(s)
from `svgElementIds` (`id · tag`, only those the kind can aim at), the attribute (a select for color / image, fixed for
font, none for text, free with the element's own numeric attributes as suggestions for a number), min / max for a
number; each control's problem is under it and Save waits on all — Save is the form's submit button, so Enter in a field saves
once nothing is wrong; the engine's refusal (through `tidyReason`) is shown in the dialog and its
warnings (a font that could not be embedded) in a toast. A title that is itself from a template starts with its
controls. **Inspector**: an animated title gets a **Template** section (`TemplateSection`) — first, above Timing, when it
came from a template (a field per control: text, a swatch + hex field, a number with its `min` / `max`, a font select
over `ui.availableFonts`, an image thumbnail + *Choose image…* → `pickImageFile` → `templateImageValue`; one
`set_template_values` edit per committed change — text, color and font on `change`, a **number on blur or Enter** (its spinner
and arrow keys fire `change` at every step), never per keystroke, to the title the field took focus on (`FieldEdits`), unchanged
values write nothing, a refused one toasts and puts the field back; a change that arrives while another edit is in flight
(`editor.busy`) is **held with what was typed** and made when that edit ends, one at a time, not put back (the proposal on
screen says so, `holdForPreview`, and puts the field back); a control whose
element left the artwork says so) and last, for custom artwork, as *Save as template…*; preset titles have no ids to
aim at and no section. Editing the SVG source ends the link (the section says so). A swatch for a color *name* asks the
browser's canvas (`css-color.ts`). Harness: `?empty=1` has no templates, `?templateproblems=1` lists an unreadable file;
`template-dev.ts` now refuses what core does (nested text controls, `attribute_name_ok`, control characters in name /
description / label, a blank name falling back to the title's, Rust's `f64` grammar for numbers, 128 KiB after the
splice). Tests: `template-ui`, `template-actions`, `template-wiring`, `css-color`, `api-templates`, `api-templates-changed` (the desktop half of `templates-changed`).

**Titles on tracks in the editor.** A title clip is a `Clip` with `generator: {kind: 'title', name, svg, template?}` and
`asset_id` `GENERATED_ASSET` (`types.ts`); `title-clips.ts` (bun-tested) is the pure side — `isTitleClip` / `titleOf` /
`titleName`, `findTitle(timeline, id)` (a `TitleRef` for a floating title or a title clip: what `TemplateSection`, the save
dialog and `setTemplateValue` read through `editor.titleById`), `titleClip` and `placeTitleClip` (kerf-core's placement: the named
video track, else the lowest unlocked one free over the span, else a new `V{n}` on top). `api.ts`: `applyTemplateClip(templateId,
values, start, duration?, trackId?, name?)`, `addTitleClip(svg, start, duration?, trackId?, name?)`, `updateTitleClip(clipId,
patch)` → `TemplateEdit` with `clip_id`; `setTemplateValues` takes either id (the harness's `setTemplateValuesIn` answers
`overlay` or `clip`, `applyTemplateClipTo` places one, and `setSpeed` refuses a title as the engine's invariant does).
`editor.applyTemplateClip` / `addTitleClip` / `updateTitleClip` select the clip they made; `editor.select` of the generated asset
looks nothing up. **Template card**: *Add to a track at the playhead* beside *Add at the playhead*. **Inspector**: a selected
title clip shows its name with the template mark, a **Title** section (the name field, through `FieldEdits` like every plain
field: `renameTitle` writes to the clip it took focus on) and the **Template** section of a floating title (`TemplateSection`
takes `{id, svg, template}`), and has no Speed section; Timing, Fades, Transform (keyframes: an animated placement), Color, Mask
and Transition are the clip's. Tests: `title-clips.test.ts`.

**Animated SVG titles in the editor.** The engine draws the artwork (resvg, into every preview and export frame), so the
page **never inlines the SVG** — `svg-titles.test.ts` fails on any `{@html}` / `innerHTML` in `src/` — and shows it only as an
`<img>` data URL (`svgDataUrl`; the CSP's `img-src` has `data:`) for a preset's thumbnail. `svg-titles.ts` is the pure side
(bun-tested): the four presets' labels / hints / sample text (held to `fixtures/svg-presets.json`), `svgDisplayName`, the sample
document a new custom title starts from (`sampleSvg`: a gold plate sliding in, viewBox = the delivery frame, named colours only),
and the placement box maths (`dragRect` / `roundRect` / `rectPatch`). **State**: `editor.svgOverlays` (a getter over
`timeline.svg_overlays`, so it is live under the bun runes stand-in), `selectedSvgOverlayId` — exclusive with the clip and text-title
selections (`selectSvgOverlay` / `selectOverlay` / `selectClip` … each clear the others), pruned in `#setTimeline` when an undo or
agent edit removes it — and `addSvgOverlay` / `addSvgTitle` (selects what it made) / `updateSvgOverlay` / `removeSvgOverlay`. **Add**:
the Titles controls' `ANIMATED` section is a `+ Animated` button that opens tiles of the four
presets with thumbnails (`presetThumbnail`: `svg_preset_preview` drawn for 600 s so the browser's own SMIL player plays the entrance
and holds it, replayed on hover; cached per frame shape; the thumbnail span is `pointer-events:none` — replacing the `<img>` under a
pressed pointer swallowed the click) and `Custom SVG…` (`addCustomSvgTitle`: the sample, selected, and `ui.svgSourceFocus` makes the
Inspector open and focus its source box). Every new one starts at the playhead; the lane's right-click menu offers the same. **Inspector**
(a selected animated title replaces the clip sections, like a text title): Timing (name, start, end), Placement (x / y / width /
height / opacity — fractions of the frame, shown as percentages; the artwork is fitted "contain" and centred inside), then a **Preset**
section — kind chips, text, subtitle / colour / accent / font only for the parameters `list_svg_presets` says that kind takes, and
*Convert to custom* (`update_svg_overlay` with the title's own `svg`, which drops the preset) — or, for custom artwork, a monospace **SVG
source** textarea with Apply / Revert: Apply asks `validate_svg` first (a refusal is shown under the box and nothing is written), writes
one `update_svg_overlay`, and lists the warnings the engine could not honour (mutating commands do not return them). **A preset patch
always carries every parameter** (the backend fills a missing one with its default, so `{text}` alone would reset the colours), and
a refused field edit puts the field back. The kind chips are `changePreset`: one `update_svg_overlay` with `preset_kind` (one
revision; the title keeps its id, name, params, timing, placement and place in the draw order, and stays selected).
**Preview**: the selected animated title at the playhead (paused) gets a placement box over its rectangle in the same layer as the
title boxes (drag to move, corner handles to resize with the opposite corner held, one `update_svg_overlay` on release, Escape /
pointercancel / lost capture / blur abandon; `data-svg-box`, never the artwork), and counts toward `titlesShown`, so it routes the
preview like a title. Delete removes it with an Undo toast. **Review**: `staged-diff.ts` marks an animated title by its
`overlay_id` like a text title, and resolves an `overlay_removed` against `svg_overlays` too (the ghost's `svg` flag, named by
`svgDisplayName`). Wiring is held by `svg-wiring.test.ts`, the actions by `title-actions-svg.test.ts`, the state by `state-svg.test.ts`.

## Scopes panel

`Scopes.svelte`, docked in the Color preset between the preview and the inspector (a layout stored before it is
offered it like the mixer in Audio: `UNSTAMPED_OFFERED` leaves it out, `EARLIER_PRESETS.color` is the old Color). A
selector picks waveform / RGB parade / vectorscope / histogram (remembered in `localStorage`), drawn on a canvas from
`get_scopes`' grids by `scopes-view.ts` (pure, bun-tested: log density, row 0 at the bottom, 75 % targets and the
skin-tone line on the vectorscope), and the readout under it is the summary (`summaryLines`: a clipped channel is
counted in pixels and warns). The frame is the export's still from the originals at 960 wide, niced, so it is a real
decode: one request in flight, the newest playhead and cut asked for once it lands, 150 ms after the last change while
paused and every 2 s while playing, nothing while the panel is hidden. Colours are the `--scope-*` tokens (derived from
the theme's bases in `kerf-tokens.css`; the parade's blue is the theme's own `blue-500`), resolved again on a theme
change. The harness answers colour bars (`harnessScopes`).
