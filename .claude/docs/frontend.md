# Frontend (`frontend/`)

SvelteKit 2 / Svelte 5 **runes** (forced on in `vite.config.ts`). Two layout quirks:
- **No `svelte.config.js`** — adapter and compiler options live inline in
  `vite.config.ts` via the `sveltekit()` plugin (new-style config). Static SPA via
  `adapter-static` (fallback `index.html`); `+layout.ts` sets `ssr = false` +
  `prerender = true`. Dev port is pinned to **1420** for Tauri.
- **Tailwind 4 = CSS config**, no `tailwind.config.js`. `src/routes/layout.css` imports
  the **Kerf design tokens** (`src/lib/styles/kerf-tokens.css`) and maps the shadcn
  semantic vars onto them. **Every color is themable**: `src/lib/theme.ts` lists
  the opaque color tokens (grouped for the editor), three presets (Kerf Dark —
  bun-tested to equal the stylesheet's defaults — Kerf Light, High contrast),
  `parseTheme` (a stored or imported JSON; unknown tokens dropped, missing ones
  filled from the scheme's preset) and `applyTheme`, which writes the tokens as
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
  High contrast ships thicker lines. `app.html` paints Kerf Dark before hydration
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
  waveform, the drag ghost), plus the `color-mix` fill of a generated caption's
  block (`GENERATED_TITLE_FILL`, resolved per preset by `mixSrgb` and held to 4.5
  under its label). `text-disabled` is exempt on a *disabled* control but the UI
  also draws the idle state of live toggles in it (DUCK / S / L, the bell), so it
  is held to 3:1 on the resting surfaces; the translucent hairlines are exempt.
  High contrast is also held to 7:1 on its reading text. The pair list
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
it follows the theme) hosting seven panels — `LibraryPanel`, `Preview`, `Timeline`,
`Inspector`, `AgentPanel`, `DeliverPanel`, `Mixer` — each a Svelte component
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
Motion → Transitions, Edit and Deliver → Media), and **touches no project state**
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
voiceover entry point), Transcript (`TranscriptPanel`). Effects, Transitions and
Audio act on the selected clip and say why they are off when none is; they own no
value (a look is a `Color`, an effect an entry in the clip's chain), so the
Inspector stays where you tune — the presets are `effect-presets.ts`, shared with
its pickers. Titles is the same component as the Inspector's *Titles lane*
section, which stays (a folded library must not make titles unreachable), over
`title-actions.ts`; the caption look they share is `ui.captionStyle`. Clicking the
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
the workspaces in the middle; the project's name with its Saved / Unsaved badge, the
settings gear, the notification bell and the version chip on the right (the chip still
turns amber when an update is waiting, which is why it stayed out of Help; Help also
has *Check for updates…*). The project's path moved to the status bar. **The menus**
(`MenuBar.svelte` over the pure, bun-tested `menus.ts`): *File* (New, Open…, Save…,
Import media…, Import captions…, Export…, Save cover frame…, Settings, Quit — Save is
"as…" once the project has a file, since a saved project is its own SQLite file and
`save_project_as` is the only save there is), *Edit* (Undo / Redo, Cut / Copy / Paste /
Duplicate, Delete / Ripple delete / Select all, **Tool** and **Clip** submenus —
the five tools as radios, and trim / detach / link — Ripple mode and Snapping as ticks,
Keyboard shortcuts…), *View* (zoom, **Track height**, Overview strip, Safe-area guides,
**Delivery frame**, **Workspace**), *Window* (every panel as a tick, Reset <workspace>
workspace, Reset all workspaces — which asks first when any arrangement is stored),
*Playback* (Play / pause, Go to start / end, back and forward a frame and a second,
Shuttle J / K / L, Set / Clear in and out, markers — the transport lives in the Preview
panel and a panel can be closed, so the whole of it is here as registry actions with
their bindings), *Help* (Keyboard shortcuts, Check for updates…, Release page, Open the
log folder, About). **Every entry names a keymap action**
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
full bar, and the buttons, timecode and scrub bar never go); the tools, Ripple, Snapping,
Undo / Redo and the delivery-frame picker are the **Timeline**'s toolbar, which wraps to a
second row rather than clip
(its "Timeline" caption is gone — the dock tab says it); New / Open / Save / Export and
Panels are the File and Window menus. `menus.test.ts` holds each of those paths, and
that no entry of the old toolbar lost its place.
The **Deliver panel** (`DeliverPanel.svelte`) docks the export dialog's readiness
verdict and *Deliver to* shapes, extracted into `Readiness` / `DeliverTo` /
`SectionHead` which the dialog uses too — no fork. The shape choice and the
smart-crop toggle live on `ui` (`deliverShapes`, `deliverSmartCrop`) because both
places edit them (session-global, so a shape ticked in the panel is ticked when the
dialog opens), and both components re-judge on every `editor.timeline` change
(docked beside a timeline being edited, a verdict cached at tick time goes
stale), keeping the newest answer. Shapes are for a picture, so the panel hides
them, and the button never says "Export N files", for a cut with no video clip
(`hasPicture`, the same gate the dialog uses). The render still goes through the full dialog
(`ui.openExport()`); the panel shows its progress and Stop while one runs.
The `Inspector` is **mounted whether or not a clip is selected**:
its Text overlays section belongs to the timeline rather than to any one clip, so
gating the panel on a selection made titles and captions unreachable until you
clicked a clip. Its sections are `InspectorSection`s — native `<details>`
disclosures, so they are keyboard-operable for free — each with a one-line
**summary** on the right (`1.00×`, `Cropped`, `2 keyframes`) so a collapsed
section still says what it holds; Timing, Volume and Titles open by default,
everything else is folded. It edits the selected clip —
trim, volume, fades, speed, transform, color, **transition** (a grouped picker
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
mirror's `upsertKey`, so a drag is one edit. The Transform sliders do the same for numbers keyed on
their own; the Animation list is still the legacy bundle's, and the per-number rows, dope sheet and easing
popover are the next slice. The preview's Web Audio gain follows a keyed volume — `gainAutomation`
ramps linearly through the curve's points and the fade edges, and **steps where the curve does** (a hold
or two keys at one time: the ramp arrives at the value the step leaves and a `setValueAtTime` lands the
next, read `1e-9 s` either side of the step because `(start + t) - start` can fall an ulp short of `t`),
as the export's `if(lt(t,..))` does — but the timeline's volume line and
waveform scaling still draw the static gain), a **Framing** section
(a `Smart crop` button that frames *this* shot for the delivery frame, plus
`Reset crop`, above the crop sliders it writes — greyed out with a reason when the
shot already matches the frame or is 360), a **Mask** section (None / Rectangle /
Ellipse chips, then centre / size / feather / invert; picking a shape starts from
a visible default rather than a collapsed one, and the caption carries the recipe
the shape alone does not suggest — a lower track shows through, so a blurred face
is a duplicated, blurred copy above, masked), a **360 reframe**
section (yaw / pitch / roll / FOV, auto-keyframing
at the playhead like Transform — note its `lerpAngle` takes the shortest arc, which
plain `lerp` would read as a 340° swing across the seam; for a source Kerf did not
detect as 360 it instead offers a projection picker that marks the whole asset via
`set_asset_projection`), and an always-visible
**Text overlays** section (add titles / lower-thirds, caption the whole cut in
a **Lines / Word punch** style chosen by two chips above the button — the
selection is deliberately *not* derived from the overlays already there, since
a caption's style is not recoverable from its text and guessing it from the
word count would flip the chip whenever a sentence happened to be short —
the button relabels to `Recaption` once there are generated captions, since a
later trim moves the words out from under them, with `Clear` beside it taking
only the generated ones (imported ones included) — and edit text / timing / position / size / color /
box / bold).
**Polish presets** (`src/lib/style-presets.ts`, pure data over the existing
surfaces): the Color section leads with one-click **looks** —
Punchy / Warm / Cool / Faded / B&W chips (the active one highlights; the sliders
show exactly what a chip applied) built on `Color.temperature`, a warm-cool
channel in -1..1 rendered as opposing `eq` per-channel gammas (`eq_filter` —
omitted at 0 so old graphs stay byte-identical; plain saturation/gamma can't
tint) — and the Text overlays section leads with **Title / Lower third /
Caption** style chips that create a styled overlay at the playhead with
fade-in/out opacity keyframes; the caption style matches what
`generate_captions` writes in its `lines` style, so manual and generated
captions look alike (`CAPTION_LOOKS` in the same file is only the two
generate-time labels; their numbers live in `captions.ts`).
Everything is styled with the CSS-variable tokens directly (inline `style`), not Tailwind
utilities. **Titles are their own items, not part of a clip.** `Timeline.overlays` has always
been timeline-level, and the UI now says so: the Timeline has a **titles lane**
(`T`, above V1; `data-title-lane`) where every title / lower-third / caption is a
block from `start` to `end` (generated captions dashed and dimmer; overlapping
items stack into rows via `packRows`). Click selects (`editor.selectOverlay`,
exclusive with the clip selection; click also seeks into the title), the body
drags in time, the 6px edges trim, with the clip drag's snapping (0 / playhead /
beats / every clip edge / other titles), Delete removes it, and one
`update_overlay` is written per gesture. A selected title makes the Inspector show
**that title's editor** (text, timing, position, size, colour, box, font, bold,
keyframes) *instead of* the clip sections; the add / caption controls live in a
"Titles lane" section below. The **Preview** draws an interactive box over each
title visible at the playhead (hidden while playing): drag to move, corner
handles to resize (scales `size` by the pointer's distance from the box centre),
Escape / pointercancel / blur abandon it, pointer capture holds it, local state
updates live and **one** backend edit lands on release. The box is laid out in
the engine's units: `cqh` against a size container covering the drawn picture,
centred on `(pos_x, pos_y)`, font `size` of the height, browser text metrics
standing in for drawtext's, `boxborderw` as padding, so it is aligned to within
font-metric differences. A **keyframed** title follows the Transform convention:
moving it keyframes the position at the playhead (`editor.moveOverlay`, updating
an existing keyframe within 20 ms, else inserting one carrying the opacity in
force), because the static `pos_x/pos_y` is not what an animated render reads;
resizing always writes the static `size`. Pure logic (box math, keyframe upsert,
row packing, snapping, span trim) is `src/lib/titles.ts`, bun-tested.


The timeline toolbar carries a **delivery frame picker** (also View › Delivery frame; Source / 16:9 / 9:16 / 1:1 / 4:5,
from `src/lib/delivery-formats.ts`, bun-tested) that sets `Timeline.format` — the
preview pane then *is* that frame (sized with `100cqh` container units so a 1:1
frame is height-bound in a wide pane, not squashed), and for a vertical or square
delivery it draws **safe-area guides** (the platform's top strip / caption rail /
action column, plus a title-safe box; `settings.safeAreas`, **off by default**, toggled from
Settings › Preview or the preview context menu, which both write the same
persisted `Settings.safe_areas` — it is held process-wide in `settings.rs`
rather than in the engine, since nothing in kerf-core cares about it). The export dialog's "Source" resolution relabels to
**Project frame (WxH)** so the two surfaces cannot silently disagree. The dialog
is **preset → destination → picture → sound → where it is going**, with a
`Quality` select of three named CRF points (Smaller file / Balanced / Higher
quality, derived from the codec's CRF range) standing in for the encoder; the
codec, rate control, tune/profile/pixel format, audio codec and container knobs
all live behind one **Advanced encoding** disclosure, and a CRF typed there
reads back as `Custom` in the select. Loudness normalization sits beside
`Include audio` rather than in Advanced, because for the social-video user it is
a polish switch, not an encoder setting. The **readiness panel** stays visible
(only its tips fold): "Ready for Instagram Reels · YouTube Shorts ·
TikTok", then any length errors / reach warnings one line each, then a *single*
collapsed line for shape ("A 16:9 cut is letterboxed on … Pick a delivery frame
in the toolbar") — grouped by `IssueKind`, because otherwise four vertical feeds
each say the same thing. It re-checks against `opts.resolution`, so a 9:16
project exported at 1920×1080 is judged as the landscape file it will be.
`kerf_core::platform` decides all of it; `src/lib/platforms.ts` is a bun-tested
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
**Import captions…** is a button in `TitlesControls` (so in the Inspector's Titles
lane and the library's Titles tab) and an entry in the titles-lane context menu
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
the cut?" before **Captions / Recaption** (and the Inspector's menu entry, which calls
the same `makeCaptions`) and the **AgentPanel's "Caption the cut" chip** (asked before
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
`Preview` shows the composited frame under the playhead, and during
**forward 1× playback it switches to the streamed frame source** (`start_playback`)
— per-frame `get_timeline_frame` decodes stay for scrubbing, shuttle and the
settled frame, where you want *one* frame rather than all of them. Its effect keys
off `ui.seekEpoch` (bumped only by a deliberate seek or a fresh play) and never off
`ui.time`, which ticks every animation frame and would respawn ffmpeg 60×/sec.
Which frames survive is `playback-sync.ts`'s `createFrameGate` (`show`/`skip`/
`resync`, unit-tested — the one piece of frontend logic with tests): every frame
arrives late by a *constant* transport cost (ffmpeg's spawn, then base64 + JSON +
IPC) that on its own exceeds the two-frame `STALE_AFTER` budget, so lag is judged
against the smallest this stream has managed rather than against zero — measuring
from zero dropped every frame forever and froze the pane. Only growth past that
floor is drift: `STALE_AFTER` skips the frame, `RESYNC_AFTER` restarts the stream
from the playhead rather than playing it out in slow motion against the sound.
`start_playback` logs `frames` / `first_frame_ms` per run, which is what separates
"never started" from "sent but dropped"; in the browser harness `startPlayback`
**synthesizes** frames at the requested fps behind a deliberate 90 ms lag, so
playback moves under `bun run dev` and that failure mode is reproducible without a
desktop build. `ExportDialog` (⌘E) drives
the full `ExportOptions` surface — presets, containers/codecs, rate control, resolution,
loudness normalize, and a **Range: In → out** choice when marks are set. It **opens on
the frame the project is cut for** (`initialExport`): the preset whose resolution is
that frame when one matches, else the default preset with its resolution cleared so
"Project frame" renders — otherwise a 9:16 project opened its export already
landscape and the readiness panel warned about the shape the user had just chosen.
Its **Deliver to** section is the multi-format export: shape chips (the
`DELIVERY_PRESETS` minus Source) that each add a file beside the chosen path
named by shape (`variantPath`, the bun-tested mirror of
`ExportVariant::beside`), a *Smart crop each shot for every shape* toggle, and a
per-file readiness line judged at that file's frame (`platformCheck([w, h])`),
in place of the single panel — with shapes picked, the Scaling rows hide (each
delivery brings its own resolution and fit) and the button reads `Export N
files`; `export-progress` then carries `variant` / `total`. The
**Transcript tab** of the library (`TranscriptPanel.svelte`, over the pure, bun-tested
`src/lib/transcript.ts`) **is an editing surface**: lines resolve to the clip carrying them,
click seeks, the playhead line highlights, and `×` cuts the sentence from the timeline
(`cut_clip_range`); cut lines render struck through. When it is *empty* it says which
of the five reasons applies (nothing selected / no backend / model not downloaded /
not analyzed / no speech) and offers the matching action — a model picker + download,
or Analyze — instead of a dead end. The **agent panel is a real MCP task
queue** (status · queue · history · add-task) — Kerf has no in-app chat; a connected
LLM claims tasks over MCP. The queue is `agent` state (`src/lib/agent.svelte.ts`, a third
runes singleton) backed by the `tasks` table over Tauri/MCP: the add-task box and preset chips
`agent.add(...)` real tasks, and `ready` tasks show Apply/Dismiss (`resolve_task`/`remove_task`).
The panel orders **review card → queue (ready first) → quick edits → connect →
history**, and since it shares a tab group with the Inspector by default,
`+page.svelte` brings it forward once when a proposal lands (`editor.staged`
going null → set) — a review nobody can see is not a review. Under **Quick
edits**, five preset chips (`Remove silences` / `Assemble rough cut` / `Frame for the delivery`
/ `Caption the cut` (analyzes whatever is in the cut but not yet transcribed,
then captions it) / `Cut to the beat` — which
analyzes whatever is on the audio tracks first, then calls `snap_to_beats`, and says
"No cuts were near a beat" instead of claiming an alignment when the grid never reached
them) also run the matching local op and
resolve their task; the rest just enqueue for the agent. In the browser there is no agent, so
queued tasks correctly just wait. Above the queue sits the **review card** — the
panel's whole point, since an agent's task edits never touch the open cut. It renders
`editor.staged`: the agent's note, a headline (`4 changes · 2:00.0 → 1:40.0 (-20.0s)`),
the changes grouped by what they touch and tinted by the design system's own
`--diff-add`/`--diff-remove`/`--diff-shift`, each row clicking through to the moment
it describes. **Preview** swaps the editor onto the proposed timeline behind a
banner (`editor.previewingStaged`; any real edit or a fresh `load()` drops back to
the live cut, and `refreshTimeline` parks an incoming live update rather than
yanking the view) — so the proposal can be *watched*, not only read. Apply lands it
as one revision (confirming first when it went `stale`), Discard drops it. The
headline arithmetic is `src/lib/diff.ts`, the bun-tested TS mirror of
`TimelineDiff::headline`; the entries themselves are phrased by kerf-core, which is
why `revisionDiff` returns `null` in the browser instead of a second, divergent diff
engine. Below the queue, the **History** section renders
`editor.history` (the `Revision[]` edit log, attributed to user/agent/system) with one-click
`editor.revertTo(seq)`, and each row expands to *what* that revision changed
(`revision_diff`).

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

**Modals are modal.** `ExportDialog` / `SettingsDialog` / `UpdateDialog` use the
`trapFocus` action (`src/lib/modal.ts`: takes focus, wraps Tab, restores focus on
close), and `+page.svelte` makes the app behind them (and behind `VoiceoverDialog`,
which focuses itself) `inert` and returns early from its global key handler while
any is open — Space / Delete / J-K-L / ⌘Z would
otherwise edit the live project under a dialog; a file drop is ignored then too.
Every shortcut — bare keys and ⌘ chords alike — stands down inside any text input /
textarea / select / contenteditable. **Nothing unsaved is dropped silently**: once saved a project is a
SQLite file and every edit is committed as it happens, so only a never-saved,
non-empty project (`editor.hasUnsavedWork`) can be lost — New, Open, the window's
close request (`onWindowCloseRequested`) and the updater's *Restart now* all
confirm first through `confirmAction` (the dialog plugin's `ask`; the plugin
replaces `window.confirm` with an async one, so it cannot be used as a guard). A
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
`NotificationCenter.svelte` (All / Unread / Problems, per-row read toggle, mark all
read, clear) and badges the unread count, red when anything unread actually failed.
Errors and warnings also linger longer on screen than sonner's default. The log is
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
(`ADDED_SHIFT` ⇧D detach, `ADDED_MOD` ⌘L link, `ADDED_MOD_SHIFT` ⇧⌘D reattach / ⇧⌘L unlink).
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

**Settings** are their own runes singleton (`src/lib/settings.svelte.ts`) behind
the title bar's gear (⌘,): `SettingsDialog.svelte` is a section rail plus a
panel, so the next preference is a row in a list rather than new chrome. Six
sections: **Performance** — the CPU limit as three named budgets (Background /
Balanced / Full speed) over a slider, reading back "9 of 12 cores for Kerf · 3
left for everything else", because the complaint this answers arrives in those
terms and not in percentages — **Analysis**, a master "analyze new media when it is imported" and one
toggle per kind (silence, scenes, loudness, rhythm, transcript; `Settings.auto_analysis`, which replaced
the single `transcribe` checkbox and migrates it): a clip can always be analysed by hand whatever is off,
and the same set decides what an agent's `analyze_asset` runs when it names no steps. **Speech** keeps
the voiceover model and points at Analysis; `TranscriptionStatus.enabled` is the transcript switch, so
the transcript tab says "Speech-to-text is off for new imports" and offers Transcribe. And **Preview**:
the **proxy size** (720 / 1080 / 1280 default / Off, `settings.proxySize`) and the **preview source**
(Auto / Always original / Proxy only, `settings.previewSource`; disabled with size Off, which forces the
original) — both written through, followed by a status refresh and a preview nudge — then one checkbox
for the safe-area guides over a vertical or square cut, and the experimental GPU preview
toggle with this machine's status line (technique, adapter, last failure). And **Appearance**: the
three theme presets as chips, a name and a dark/light scheme, **Import… /
Export…** (a `.json` file through the dialog plugin and `read_text_file` /
`write_text_file`; the harness uses an `<input type=file>` and a download), then
every color token as a picker grouped by `COLOR_GROUPS`. A color edit applies
at once (`applyTheme`) and is written 300 ms later — a picker fires per pixel
of a drag — and while one is pending a view coming back from another write
leaves the theme alone, so the newer colors never flicker back. Changing any
color makes the theme `Custom` (`presetIdFor` compares colors, not the name).
And **Keyboard**, described above. The percentage is clamped by the engine, so the
view that comes *back* from `set_settings` is what renders, not the value asked
for; in the browser harness `api.ts` answers from localStorage
(`kerf.settings.*`, the layout, theme, workspaces and keybindings as JSON strings) over
`navigator.hardwareConcurrency` so the dialog is drivable under `bun run dev`.

The **update flow** is its own runes singleton (`src/lib/updater.svelte.ts`,
alongside `editor`/`ui`/`agent`): it runs a *silent* check at startup and every
6 h through `api.ts`'s `checkUpdate` / `installUpdate` / `relaunchApp`, and drives
`idle → checking → { current | available → downloading → ready } | error`. The
title bar's version chip turns into an amber "⬇ 0.18.0" button when something is
available and opens `UpdateDialog` (release notes, download progress, then
**Restart now** — which warns first when the project has unsaved changes); the
dialog auto-opens the first time a given version is seen (remembered under
`kerf.update.seen` in localStorage) so declining doesn't nag every launch. The
`Update` handle the plugin returns stays module-local in `api.ts`, which hands the
UI plain data — so the browser harness can fake the whole flow: `bun run dev`
with **`?update=1`** offers a synthetic 0.99.0 and simulates the download, making
the dialog explorable without a signed desktop build.
`bun run dev` with **`?staged=1`** seeds a synthetic agent proposal (a tightened
intro), which is how the whole review flow — card, preview swap, apply, discard — is
driven end-to-end without a desktop build.
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
`ensureAnalysis(id, needed)` is what the Agent panel's quick edits call — Remove silences runs only
`silence`, Cut to the beat only `rhythm`, Caption the cut only `transcript` (`QUICK_EDIT_STEPS`).
`runAnalysis` re-reads the statuses afterwards and toasts each requested step that failed, once). There is **no scripted demo phase machine**: the
editor chrome derives from real state — `MediaBin` shows a dropzone until `editor.assets` is
non-empty, `StatusBar` shows the selected asset's real fps/resolution/codec and timeline
duration (plus the analysis step, what is still queued behind it and a **Stop**), and
`Preview` shows the decoded frame or a "No media loaded" placeholder.
Each **bin row is the asset's specs**, not just its name: a real decoded frame
(`get_frame` 10% in, cached per asset across re-docks; the icon stays in the
browser harness, which has no decoder), the spec line
(`1920×1080 · 29.97 fps · h264 · stereo`), and badges for 360 / still / how many
clips already use it, and **`MediaChips`** — the preview proxy's badge (`queued` / `proxy 42%` / `proxy` /
`proxy failed`, tooltip carrying the reason; nothing for a still or audio file) and one chip per analysis
kind (`SIL` `SCN` `LUFS` `BPM` `TXT`: done ✓ green, not run outlined, running spinning, failed `!` red,
off dashed `–`; state is a mark and a border as well as a colour, the tooltip says why). The Inspector's
clip header draws the same chips from the same store plus an **Analyze** button, and the timeline clip
menu lists how many are done and an `Analyze…` entry. The store is `media-status.svelte.ts`
(`mediaStatus`): `proxy-progress` / `analysis-status` events note into it, and it is re-read when the set of
assets changes and after a setting that moves what a status means. The **status bar** says which file
the frame under the playhead is decoded from (`preview: proxy 1280 px`, `preview: original · proxy 42%`,
`preview: waiting for proxy · 42%` under Proxy only, which also puts a badge in the Preview pane —
`previewSourceNote`, judged from the statuses already held, nothing asked per frame). Its **context menu leads with the facts** — the
frame, rate, codec, audio, projection, the stitched lens pair, the use count, the
import date, then how much of the analysis has run, what it found (loudness, tempo, silence, shots,
transcript) and the proxy's state — above the actions that need them: add at the
playhead / append, the audio action — labelled for what the backend will do (`extract_audio` or, for an asset not
playing its own sound, `add_asset_audio`)
(`media-info.ts` `audioExtraction`: `Detach audio from N clips` when clips of the asset still play
their own sound, else `Add audio to A1`, with `again` when its sound is already on an audio
track), remove silences (greyed out until silence has been detected), **Analyze** — one entry per kind
("Detect silence", "Find scene changes", "Measure loudness", "Find the beat and tempo", "Transcribe
speech", "Analyze everything"), each saying `done · run again` / `failed · retry`, disabled with the reason
for a voiceover, a running pass, a file with no audio or no speech backend — and Stop analysis, **Rebuild /
Build / Restart proxy** and **Delete / Cancel proxy** (`proxyActions`), mark the asset 360 or flat, copy the
path, show in folder. The phrasing is `src/lib/media-info.ts`, pure and bun-tested, so the
row and the menu cannot drift apart; `MenuItem` grew `header` / `info` rows for
it, which the shared `ContextMenu` renders non-interactively.
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
