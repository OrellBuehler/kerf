# Kerf shell and design system: UI/UX polish review

Scope: title bar and menu bar, workspace switcher, dockview panels/tabs/sashes, status bar, notifications/toasts, focus, theme tokens, typography, spacing, icons, radii, elevation, contrast (Dark / Light / High contrast), hover/disabled states, keyboard labels.

Method: source read (`frontend/src`, `.claude/docs/frontend.md`) plus headless Chrome driving the dev harness at 1920x1080 and 1366x768 (also 1000 and 960 wide), dsf 1 and 2, all three presets, `?staged=1 ?music=1 ?update=1`. Contrast numbers come from `src/lib/contrast.ts` run over `PRESETS` (WCAG 2.x). Hover coverage was measured by hovering every visible interactive element and diffing computed style. Screenshots and probe scripts are in `.../scratchpad/review/shell/` (named below). No repo file was touched.

Caveat that matters for reading the screenshots: the brand fonts do not load (finding H1), so every screenshot shows the system fallback (DejaVu here, Segoe UI on a user's Windows machine). Metrics-based remarks are written to hold for either.

## Verdict

The structure is sound and unusually well engineered (token system, themes, ARIA menubar, contrast guard, workspace presets). What makes it feel less than professional is not layout, it is the missing "last 10%" of interaction polish: almost nothing reacts to the pointer (69 of 80 controls have no hover state), the brand typeface is never actually shipped, keyboard focus is invisible on the dock tabs, the primary action (Export) has no button outside the Deliver workspace, and the type/icon/radius scales exist as tokens but are bypassed by roughly 400 hard-coded values. Light and High-contrast are close but have specific gaps (shadows, hairline borders, tinted badges). Nearly everything below is a small, local change; the biggest structural fix (CSS classes for chrome buttons instead of inline `style=`) unlocks hover, active, disabled and focus in one pass.

---

## HIGH

**H1. The brand fonts never load; the app renders in whatever system font the OS has.**
- Evidence: `routes/layout.css:4` is `@import url('https://fonts.googleapis.com/...Geist...')`, placed after Tailwind's own `@import`s. It is dropped in dev and in the production build: `build/_app/immutable/assets/0.B9ir76SJ.css` has no `@font-face` and no `@import`; in the page `document.fonts.size === 0` and `document.styleSheets` has no font-face rules. `@fontsource-variable/inter` is in `package.json` and imported nowhere. All screenshots are DejaVu.
- Effect: Geist, Geist Mono and Space Grotesk (tokens `--font-sans/-mono/-display`) are fiction. Windows gets Segoe UI, mac gets SF, Linux gets DejaVu, and every 10-11px monospace readout (timecodes, status bar, accelerators, chips) changes width per OS. Even if it worked, a desktop editor should not need Google at first paint (CSP allows it: `tauri.conf.json:30-31, 41-42`).
- Fix: self-host. `bun add @fontsource-variable/geist @fontsource-variable/geist-mono`, import them at the top of `layout.css` (`@import '@fontsource-variable/geist';`), delete the googleapis `@import`, drop `fonts.googleapis.com` / `fonts.gstatic.com` from both CSP blocks, remove the unused Inter dependency. Space Grotesk is referenced by one `--type-title` use; drop it from the stack or self-host it too. Add `font-variant-numeric: tabular-nums` to the timecode/readout styles so digits do not jitter once real fonts load.

**H2. No hover (or pressed) feedback on ~86% of interactive controls.**
- Evidence: hover probe over 80 visible controls: 11 change computed style, 69 do not. No hover on: the five workspace tabs, gear, bell, version chip, all library rail icons, inactive dock tabs (Agent), the transport buttons (Go to start / Play / Go to end), every Inspector chip (+ Title, Lower third, Lines, ...), every Timeline toolbar and track-header button. The 11 that do are the six menu titles, the four dock close X buttons and the Inspector section summary. There is not a single `:active` rule.
- Cause: the shell is styled with inline `style="..."`, which cannot express `:hover`. `IconBtn.svelte` is used 3 times, `Btn.svelte` 60 times, but 115 raw `<button>`s are hand-styled; `Btn.svelte:42` and `IconBtn.svelte` even declare `transition: background` with no rule to transition to.
- Fix: put chrome buttons on classes in `layout.css` (or scoped `<style>` in `Btn`/`IconBtn`): `.k-btn`, `.k-icon-btn`, `.k-chip`, `.k-tab` with `:hover { background: var(--surface-hover); color: var(--text-primary) }`, `:active { background: var(--surface-active) }`, `[aria-pressed='true'], [aria-selected='true']` for the on state, `:disabled` for the off state. Inline `background:` beats any stylesheet, so the per-state colours must move out of the inline style (use `data-on`/`aria-pressed` selectors instead of the `{on ? ... : ...}` ternaries). Do the shell first (TitleBar, WorkspaceTabs, LibraryPanel rail, Btn, IconBtn), then the timeline toolbar. Dock tabs: `.dockview-theme-kerf .dv-tab.dv-inactive-tab:hover { color: var(--text-primary); background: var(--surface-hover) }` in `dockview-kerf.css`. Screenshots: `hover-sheet.png`.

**H3. Keyboard focus is invisible on dock tabs.**
- Evidence: each dock tab is `tabindex=0` (one tab stop per tab, 5 in Edit) but dockview's focus indicator is `outline: 1px solid var(--dv-tab-divider-color)` (`dockview.css:112-120`) and `dockview-kerf.css:16` sets that colour to `--border-subtle` (6% white, 1.15:1). Tabbing onto "Inspector"/"Timeline" shows nothing (`focus-sheet-dark.png`, tiles 03 and 14). The global ring at `layout.css:143` only lists `button, summary, input, select`, so `[role=tab]` and other `[tabindex]` hosts get none.
- Fix: in `dockview-kerf.css`: `.dockview-theme-kerf .dv-tab:focus-visible::after, .dockview-theme-kerf .dv-tab:has(:focus-visible)::after { outline: 2px solid var(--kerf-400) !important; outline-offset: -2px; }`. Broaden the global rule to `:where(button, summary, input, select, textarea, a, [tabindex], [role='tab'], [role='button']):focus-visible`.

**H4. Live information is drawn in the "disabled" colour at 10px (3.4:1).**
- Evidence: `--text-disabled` on `--surface-app` is 3.43:1 Dark / 3.55 Light. It is used for all status bar text (`StatusBar.svelte:57,59,131,142,152`: fps/resolution/codec, duration, asset/clip counts, project path, Logs), and for the idle gear, bell and version chip in the title bar (`TitleBar.svelte:63,80,100`). These are readouts and live controls, not disabled controls; `theme-guard.test.ts` holds `text-disabled` to only 3:1 and only on the assumption that it is a toggle's idle state, at normal sizes. The gear (Settings) being the dimmest thing in the window also hides the entry to Settings.
- Fix: status bar text to `--text-muted` (5.04:1) at 11px; gear/bell/version idle to `--text-secondary` (7.28:1) hover to `--text-primary`. Keep `--text-disabled` for genuinely disabled things only. If you want a quieter tier, raise `text-disabled` itself to `#718090` (4.61:1 on app, 3.63 on hover) and keep the guard.

**H5. Disabled context-menu items are nearly unreadable (1.8:1).**
- Evidence: `ContextMenu.svelte:130` applies `opacity: 0.55` on top of `color: var(--text-disabled)`. Effective contrast: 1.83:1 Dark, 1.90 Light, 2.59 High contrast (`dark-ctx.png`: "Link 2 clips", "Separate stems under this clip..."). Worse, the reason line under it is `--text-muted` and therefore brighter than its own label. The menubar's disabled items (`MenuBar.svelte:508`) use `text-disabled` with no opacity (3.1:1) so the two menu systems disagree.
- Fix: set `opacity: 1` on these rows (the blanket `button:disabled { opacity: .5 }` at `layout.css:147` would otherwise apply to the `disabled` buttons once the inline value is gone, see P1); disabled = `--text-disabled` label, `--text-muted` reason, same as the menubar.

**H6. The primary action has no button.**
- Evidence: after the toolbar was removed, Export lives only in File > Export..., Ctrl+E and the Deliver workspace. In Edit/Color/Audio/Motion nothing in the chrome says "you can export" (`TitleBar.svelte` has no Export; screenshots `base-*`). Resolve (Deliver page), Premiere (Export button), Final Cut (Share) all keep it top-right.
- Fix: add a small primary `Btn size="sm"` ("Export", `upload` icon, kerf-500 fill, `ui.openExport()`, tooltip with `settings.withShortcut`) to the right cluster of the title bar, left of the settings gear; show progress in-place while `editor.exportRun` is live (it already drives the status bar). 28px high.

**H7. Elevation does not follow the theme: Light gets heavy dark smudges.**
- Evidence: `kerf-tokens.css:239-244` shadows are hard-coded `rgba(0,0,0,.4-.66)` and never themed; `--shadow-pop`'s white 6% ring (line 244) is invisible on Light. In Light, menus/notification panel/preview frame cast a grey smear (`light-menu-edit.png`, `base-kerf-light-1920.png` under the preview frame). On Dark the same shadows are invisible against `#0f1318`, so overlays rely on a 1px 16% border for separation; `surface-raised` vs `surface-panel` is only 1.05:1.
- Fix: introduce `--shadow-color` (Dark `0 0 0 / .55`, Light `20 30 50 / .16`) and build every `--shadow-*` from it via `color-mix`/rgb with var; Light values roughly `0 8px 24px` at .14 for menus, `0 18px 48px` at .18 for dialogs. Add a 1px inner top highlight on Dark overlays (`inset 0 1px 0 color-mix(in srgb, var(--border-base) 5%, transparent)`) so raised surfaces read as raised.

**H8. High contrast does not raise borders; component edges stay at ~1.3:1.**
- Evidence: `kerf-tokens.css:79-82` derive borders as fixed 6/10/16% of `border-base`. In High contrast that gives 1.13 / 1.26 / 1.52:1 on the panel; the preset only thickens `line-width` to 1.5px. Buttons, chips, inputs and menus are outlined only by `--border-strong`, so their boundary fails the 3:1 non-text rule (WCAG 1.4.11) in the one theme meant to pass it. `base-high-contrast-1920.png`: panel edges and chips look identical to Dark.
- Fix: make the three percentages theme shape tokens (`--border-alpha-subtle/default/strong`, same `SHAPE_TOKENS` mechanism): Dark 6/10/16, Light 6/10/16, High contrast 30/45/70 (gives 2.6 / 4.5 / 9.8:1). Slightly lifting Dark's default to 8/12/18 also helps (1.33 -> 1.42:1) without changing character.

---

## MEDIUM

**M1. The type, spacing, icon and control scales exist but nothing uses them.**
- Evidence: `var(--space-*)`, `var(--icon-*)`, `var(--control-*)`, `font-size: var(--text-*)`: 0 uses each. Literal font sizes in `.svelte`: 12px x215, 11px x47, 10px x39, 13px x25, 9px x9, 10.5px x4, 12.5px x2, 11.5px x1, 14px, 16px. 9px: `MasterStrip.svelte:290`, `MediaChips.svelte:58`, `Timeline.svelte:2424,2658`, `TitleBar.svelte:85`, `AgentPanel.svelte:656`, `MixerStrip.svelte:274`, `LevelMeter.svelte:93`. Half-pixel sizes: `MenuBar.svelte:504,519`, `ContextMenu.svelte:100,114,126,145`, `NotificationCenter.svelte:126`. Dock tab size is the literal `13px` twice (`dockview-kerf.css:7,30`).
- Fix: snap 9 -> 10, 10.5 -> 11, 11.5 -> 11 or 12, 12.5 -> 12 (or 13). Add a `type-guard.test.ts` modelled on `theme-guard.test.ts` that fails on any `font-size:` literal outside `kerf-tokens.css`, with a short allow-list, and make `--text-2xs` (10px) the hard floor. Consider shipping `.t-ui`, `.t-label`, `.t-overline`, `.t-mono` classes so inline `font:var(--type-*)` is the norm.

**M2. Navigation text has four different sizes within the top 70px.**
- Evidence (measured): menu bar titles 11px/500 (`MenuBar.svelte:575`, `--type-label`), workspace tabs 11px, menu panel items 12.5px, dock tabs 13px/500, version chip 11px mono, status 10px. The dock tab strip directly under the 11px menu bar is the loudest text in the app, and larger than the panel body (12px).
- Fix: one rule: menubar titles, workspace tabs and dock tabs are 12px/500 (`--type-ui`); menu items 12px; accelerators and status 11px. Costs a few px of width at the 960 minimum; the compact "Menu" button already exists for that.

**M3. Accelerators: inconsistent between menu systems and illegible for punctuation/arrows.**
- Evidence: menubar uses mono 10.5px `--text-muted` (4.58:1; `MenuBar.svelte:519`), context menu mono 10px `--text-disabled` (3.1:1; `ContextMenu.svelte:151`). Entries whose accelerator is `,` `.` `+` `-` or an arrow render as specks (`menu-Playback.png`: Previous/Next marker; `menu-View.png`: Zoom in/out; arrows are ~5px glyphs in a 10.5px mono).
- Fix: one `.kbd` style: 11px, `--text-muted`, tabular, fixed `min-width` column right-aligned, 12px for arrow glyphs; spell `,` `.` `+` `-` as `,` in a tiny keycap (1px border, radius-xs, 2px 4px padding) or as `Comma`/`Period` in the menu string. Use the same component in both menus and in tooltips (see M10).

**M4. Menu icon column is ragged.**
- Evidence: Edit: 5 of 14 rows have an icon (Undo, Redo, Copy, Delete, Keyboard shortcuts); View: 3 of 8; Playback: 4 of 17 (Play, Go to start, Go to end, Add marker); Window: 1 of 10 (Reset); Help: 4 of 5. Reads as a rendering bug (`menu-Edit-sub.png`, `menu-Playback.png`). The same raggedness is in the context menu.
- Fix: in menubar menus keep only the check column (a menu bar is text-first on every platform); keep icons in context menus only where every row in the group has one. In `menus.ts` drop `icon` from menubar entries, keep it on the toolbar/context definitions.

**M5. Two menu components with different metrics.**
- Evidence: MenuBar panel row 26px, padding `0 6px 0 8px`, panel padding 4px, separator `4px 6px` in `--border-default` (`MenuBar.svelte:475,479,504`); ContextMenu row ~30px measured from the 2x screenshot (`padding:6px 8px` + 12.5px text), panel padding 5px, separator `5px 4px` in `--border-subtle` (`ContextMenu.svelte:85,89,124`); icon sizes 13 vs 14; submenu overlap logic only in the menubar.
- Fix: extract one `.kmenu` / `.kmenu-item` / `.kmenu-sep` stylesheet block (26px rows, 4px padding, 6px radius, 12px text) in `layout.css` and use it from both.

**M6. Context menus have no keyboard support.**
- Evidence: `ContextMenu.svelte:64` handles Escape only; the panel takes no focus, items have no roving tabindex, no arrows/typeahead. The menubar implements the full ARIA pattern.
- Fix: on open focus the first enabled item; reuse `stepFocus` / `typeahead` from `menus.ts`; Home/End/Arrows; restore focus on close; open from the Menu key / Shift+F10 on the timeline and bin.

**M7. Chip and radius language is inconsistent.**
- Evidence: two chip shapes in the same shell: pills (`radius-full`: notification filters and "Mark all read"/"Clear" `NotificationCenter.svelte:89,98,110`, version chip `TitleBar.svelte:96`, the 16:9/9:16 shape chips in the Deliver panel) versus 4px rectangles (+ Title, Lines/Word punch, color looks, effects chips). Literal radii bypass the tokens: 3px x18, 2px x13, 1px x7 against `--radius-xs/sm/md` = 2/4/6. Toast radius is 8px (Sonner) against menus and notification panel at 6px.
- Fix: rule: controls and chips = `--radius-sm`; panels/menus/popovers = `--radius-md`; dialogs/toasts = `--radius-lg`; pills only for count badges and status dots. Replace the literals; set Sonner's `--border-radius: var(--radius-md)`.

**M8. Icons: one stroke weight for every size, eight sizes, three close glyphs.**
- Evidence: `Icon.svelte:16` hard-codes `strokeWidth={2}` at all sizes. Sizes in use: 10, 11, 12, 13, 14, 15, 16, 18, 20+ (`--icon-*` unused). At 11-13px (gear 13, bell 13, menu icons 13, chevrons 12) a 2px stroke clots (`crop-titlebar-right-kerf-dark.png`). The dock close X is dockview's own 11px SVG (hairline), and six places draw a text `×` instead of the icon (`TitlesControls.svelte:263`, `Inspector.svelte:510,637,883,1148`, `AudioTab.svelte:71`, `EffectsTab.svelte:102`).
- Fix: `strokeWidth = s <= 13 ? 1.5 : 1.75` (or Lucide `absoluteStrokeWidth`); standardise on 12 (inline/chevrons), 14 (toolbar/menu/chips), 16 (rail/transport), 20 (empty states) and map them to `--icon-xs/sm/md/lg`; replace `×` text buttons with `<Icon n="x" s={12} />`; match the dock close X by overriding `.dv-default-tab-action svg` to 12px with the same weight, or hide it (see P3).

**M9. Toasts are off-system.**
- Evidence: `ui/sonner/sonner.svelte:14` `theme={mode.current}`, but `ModeWatcher` is never mounted, so the toast theme follows the OS, not Kerf's theme (probe: `data-sonner-theme="light"` while Kerf Dark is active and the OS prefers light); toast shadow `0 4px 12px rgba(0,0,0,.1)` (invisible on Dark), radius 8px, bottom edge at y=1056 vs status bar top at 1054 (the toast sits 2px into the status bar and has no gutter, `toast-Save_project.png`); the "Undo" action is a white inverse pill, the loudest element on screen and not the amber of every other primary (`toast-undo-sheet.png`); plain toasts have no icon but info ones do; toast icons (circle-check / octagon-x / triangle-alert / info, 16px) differ from the notification log's (check / triangle-alert / lightbulb / bell, 13px) for the same statuses.
- Fix: pass `theme={settings.theme.scheme}`; `offset={{ bottom: 'calc(var(--statusbar-h) + 12px)', right: 12 }}`; `toastOptions.classes` / CSS vars so the action button is a ghost with `--kerf-400` text (`[data-sonner-toast] [data-button]` override), shadow `var(--shadow-md)`, radius `var(--radius-md)`; one shared status-icon map used by both the Sonner snippets and `NotificationCenter` `look`.

**M10. Native `title` tooltips for everything.**
- Evidence: 174 `title=` attributes, zero uses of the shadcn tooltip primitive that is already in `ui/tooltip`. WebView2/WebKit draw OS-styled (light) tooltips on a dark UI after ~1s; they cannot show a keycap; several are paragraphs (workspace tabs ~90 characters, Ripple ~200, `Timeline.svelte` zoom "(Ctrl + wheel ...)").
- Fix: one `use:tip` action (or wrapper) rendering a single themed tooltip: 400ms delay, 12px, `--surface-raised`, `--radius-sm`, `--shadow-md`, label on line 1, optional `.kbd` from `settings.shortcut(id)`, optional muted second line. Migrate the shell and toolbars first; keep `title` only as the fallback. Shorten the long ones: title = noun + state, detail in line 2.

**M11. Notification centre: focus, hit targets and one low-contrast text.**
- Evidence: `role="dialog"` (line 70) but opening it does not move or trap focus (Tab from the bell goes to the version chip); row buttons are 20x20 (lines 158, 172); timestamps are 10px `--text-disabled` (line 151) on the unread row background `--surface-hover` = 2.70:1 Dark (3.18 Light); empty-state copy is `--text-disabled` 11.5px (line 123); the unread toggle is an unlabelled 7px dot.
- Fix: focus the panel on open, return focus to the bell on close, Esc already handled; row buttons 24x24; timestamps `--text-muted`; empty state through the shared component (P7); add a visible "Mark as read" label via tooltip (M10) and use a 8px dot with 1.5px ring.

**M12. Bell badge collides with the icon.**
- Evidence: `TitleBar.svelte:85`: 13px circle at `top:0; right:0` with 9px mono text, sitting on the bell glyph and covering about a third of it (`notif-bell-crop.png`); 9px is below the type floor (M1).
- Fix: min 14px, `top:-3px; right:-4px`, 10px text, `box-shadow: 0 0 0 1.5px var(--surface-app)` to cut it out of the bar; for the red "problem" state add the exclamation shape as well as colour (colour-only today).

**M13. Hit targets below 24px.**
- Evidence: gear and bell 26x22 (`TitleBar.svelte:59,76`), workspace tabs 22px high (`WorkspaceTabs.svelte:48`), version chip 22px, dock tab close 19x19, status Stop 17px (`StatusBar.svelte:109`), notification row buttons 20x20, sashes 4px wide (`dockview-kerf.css`). The title bar is 36px tall, so 28px controls cost nothing.
- Fix: title-bar controls 28px tall (`--control-sm`), close X hit area 22px around a 12px glyph, Stop as `Btn size="sm"`, sash hit area 8px (`.dv-sash::after { content:''; position:absolute; inset: 0 -2px; }` for vertical sashes and the mirror for horizontal) with the visible line kept thin.

**M14. Sash hover is a 4px solid amber bar.**
- Evidence: `--dv-active-sash-color: var(--kerf-500)` (`dockview-kerf.css:23`) paints the whole 4px sash at full saturation, after a 0.2s delay; at rest the only affordance is a 10% hairline (`hover-sheet.png`, last tile). It reads as "selected", the same signal as the selected clip and active tab.
- Fix: `color-mix(in srgb, var(--kerf-500) 65%, transparent)`, delay 0.1s, and a 2px visible line centred in a wider hit area (see M13).

**M15. Light theme: tinted badges and info colours fall under 4.5:1.**
- Evidence (Light): Unsaved badge `orange-400` text on its own 12% tint 3.06:1; Saved badge `green-400` 3.24:1; kerf badge `kerf-300` 3.82:1; `agent-500` as an info/icon colour on the app surface 2.87:1; `red-400` 4.06:1. They are 10px/600 text. The guard checks those hues only against the opaque panel, not against the tint they sit on.
- Fix (values verified): Light `orange-400` -> `#9c5400` (4.54:1 on its tint), `green-400` -> `#17744d` (4.59), `kerf-300` -> `#8a5a10` (4.52), `agent-500` stays for fills but use `agent-300` (`#126f80`, 5.27) wherever it is text or a thin icon. Add the tinted-fill pairs (`mixSrgb(hue-500, surface-app, .12)`) to `theme-guard.test.ts` PAIRS the way `GENERATED_TITLE_FILL` is.

**M16. A pending agent proposal is invisible in the shell.**
- Evidence: with `?staged=1` the review card is inside the Agent panel; in Audio and Deliver the agent is only a tab, and the tab (`staged.png`) and title bar carry no sign of it. `+page.svelte` raises it once, then it is silent.
- Fix: a cyan count dot on the Agent dock tab (`.dv-tab[aria-label='Agent'][data-pending]::after`, set from `editor.staged`/`agent` state through `panel.api.updateParameters` or a class on the tab, or a custom dockview tab renderer) and an `--agent-surface` chip "Review 4 changes" in the title bar that opens the Agent panel.

**M17. Status bar: mixed fonts, an unlabelled duration, three notations for the same time.**
- Evidence: `StatusBar.svelte:57-59` mono 10px for asset meta and duration, sans 10px for "3 assets - 3 clips" (line 131) and the Loading/Exporting/Analyzing states (lines 91-117), mono again for the path (142): the row zig-zags between families (`crop-statusbar-*.png`). The duration `00:20` (line 43 `tc`: seconds only, no frames) has no label and sits against the asset meta, while the Preview transport shows `00:20:15` and the Timeline toolbar `00:20` for the same cut.
- Fix: one family (sans 11px, `tabular-nums`; mono only for the file path); group as `[asset: 1920x1080 - 30 fps - h264]` | `[Cut 0:20.5]` using the shared `timecode.ts` formatter and the timeline fps; give the Stop buttons the shared small button.

**M18. Window title and chrome stack.**
- Evidence: the native caption (static `<title>Kerf</title>`, `+layout.svelte`) + 36px title bar + 34px tab strip = ~70px before content (plus a native caption above), and the OS title/taskbar never shows the project name or unsaved state. Documented as a deliberate stage; at 1366x768 it costs the timeline a track (`base-kerf-dark-1366.png`).
- Fix now (needs `core:window:allow-set-title` in the Tauri capabilities): `getCurrentWindow().setTitle(\`${name}${saved ? '' : ' •'} - Kerf\`)` from the `editor.projectName/saved` effect. Later: custom decorations to merge the caption and title bar (high effort).

**M19. `--type-caption` does not exist.**
- Evidence: `Inspector.svelte:805,974,1061` use `font: var(--type-caption)`; no such token in `kerf-tokens.css`, so the declaration is invalid at computed-value time and the text silently inherits the parent's size instead of a small caption.
- Fix: add `--type-caption: var(--fw-regular) var(--text-xs) / var(--leading-normal) var(--font-sans);` and extend `theme-guard`/type guard (M1) to fail on undefined `var(--type-*)`/`--text-*`.

**M20. Focus indicators are three different things and the slider one is too weak.**
- Evidence: buttons/summaries/inputs: 2px solid kerf-400, offset 2px (`layout.css:143-146`); menubar: 1.5px solid kerf-400, offset -1px (`MenuBar.svelte:596-600`); sliders: `--focus-ring` = 2px app-colour gap + 2px kerf-500 at 45% (`kerf-tokens.css:246`), measured 2.54:1 on Dark and 1.66:1 on Light against the surface, under the 3:1 needed for a focus indicator. The global `* { outline-ring/50 }` base rule sets a 50% colour on elements that never get an outline style.
- Fix: one token `--focus-outline: 2px solid var(--kerf-400)` plus `--focus-offset: 2px` (inset `-2px` inside dense strips); apply to every focusable class; slider thumb ring = solid `--kerf-400` over the same 2px `--surface-app` gap; delete `outline-ring/50` from the base layer.

**M21. Core panels can be closed with one mis-click.**
- Evidence: the active dock tab always shows a close X (Preview, Timeline, Library). Closing Timeline loses the main work surface; recovery is the Window menu.
- Fix: show the X only on tab hover/focus (`.dv-tab.dv-active-tab .dv-default-tab-action { visibility: hidden }` + `:hover`/`:focus-within` visible), and/or a custom tab renderer that omits the close action for Preview and Timeline (check dockview's API; there is no per-panel flag I could confirm).

---

## POLISH (little details)

**P1. Blanket disabled style.** `layout.css:147` `button:disabled { opacity: .5; cursor: not-allowed !important }`. 50% on a `text-secondary` button is 2.7:1 (2.5 Light) and a washed-out amber on primary; a red-circle cursor on greyed Undo/Redo reads as an error. Use explicit disabled colours (`--text-disabled`, `--border-subtle`, no fill change) and `cursor: default`.

**P2. `IconBtn` announces every button as a toggle.** `IconBtn.svelte:24` renders `aria-pressed={active}` always (`active=false` -> `aria-pressed="false"`), so the Collapse / Import / Voiceover buttons read as toggle buttons. Render it only when `active !== undefined`.

**P3. Dock tab strips are heavy for single-tab groups.** Library, Preview and Timeline each spend a 34px strip (`dockview-kerf.css:6`) on one tab. 30px and 12px labels reclaims 8px on a 768px screen with no loss; the active-tab underline could drop to 1.5px.

**P4. Version chip looks like inert text.** `TitleBar.svelte:96`: `dev` / `v0.22.0` 11px mono in `text-disabled`, pill-shaped with transparent border, but it is a button. Show it only when an update exists (amber, as now) and leave "check for updates" in Help; or render it as plain text with no pill and `cursor: default` until hovered.

**P5. Project name is the quietest text in the bar.** `TitleBar.svelte:46` 11px `text-secondary`, the Unsaved badge (19px tall beside 22-28px neighbours) is louder, and a brand-new untitled project shows a permanent amber "Unsaved". Name 12px `text-primary` 500; badge only a 6px amber dot + "Edited" in `text-muted` until the project actually has unsaved work (`editor.hasUnsavedWork`), and a rename affordance on double-click.

**P6. Control heights in the title bar: 22 / 24 / 26 / 28.** Gear/bell 22 (`TitleBar:59,76`), version chip 22, menu titles 24 (`MenuBar:575`), workspace buttons 22 inside a 28px pill (`WorkspaceTabs:48`). One height (28 from `--control-sm`) lines up the row on the pixel grid.

**P7. Empty states are four different things.** Inspector (20px icon + 12px muted, centered), NotificationCenter (`:123` 20px icon + 11.5px `text-disabled`, `padding:34px 16px`), the dock watermark `Workspace.svelte:74-75` (bare 12px `text-disabled` text, 3.3:1), the library dropzone. Build one `<EmptyState icon title hint action>` (icon 20 `--text-muted`, title 12px `--text-secondary`, hint 11px `--text-muted`, optional ghost button) and give the watermark real actions ("Open Library", "Reset workspace").

**P8. Workspace tab selected state double-signals and clips.** `WorkspaceTabs.svelte:48` selected = fill + 1px border + 2px amber underline; the underline follows the 4px radius so it curls at the pill corners and looks like a glitch at 2x (`crop-titlebar-center-kerf-dark.png`). The menu title open state uses fill + border only. Pick fill + border (matching the menu) or an inset underline drawn as a 2px `::after` with 6px side inset.

**P9. Title bar and tab strip are one flat band.** Both are `--surface-app` with no rule between them (by design); with panel vs app at 1.05:1 and hairlines at 1.3:1 the chrome reads flat. Optional: `--surface-app` slightly darker (`#0c0f13`) and `border-default` 12% (1.42:1) so strip, title bar and panel are distinguishable at a glance.

**P10. Scrollbar rules fight.** `layout.css:123-124` sets `scrollbar-width: thin; scrollbar-color` on `*`; Chromium 121+ (WebView2) then ignores the whole `::-webkit-scrollbar` block (10px, rounded, hover colour; lines 126-141), so the hover thumb colour is dead there and only WebKit uses it. Pick one system; if keeping the standard props, add `scrollbar-gutter: stable` to scrolling panels so content does not shift when a scrollbar appears (Inspector, Library). (Verify on WebView2; I could only confirm the rule precedence on Chromium here.)

**P11. Modifier names in prose bypass keymap.** `Timeline.svelte:2052` hard-codes `(⌘/Ctrl + wheel zooms at the cursor)` in a tooltip, which prints a Mac glyph on Windows; the Inspector multi-select note says "Del removes them all" although Delete is rebindable; `MasterStrip`/`FaderMeter`/`MixerStrip` tooltips hard-code "Shift" / "Alt" (correct on Windows, "Option" on mac). Add `settings.modLabel('primary' | 'alt')` and use it in prose; extend `keymap.test.ts`'s source scan to flag `⌘`/`Del` in strings.

**P12. z-index scale is ad hoc.** Values 24, 25, 26, 28, 29, 30, 35, 40, 45, 50, 60, 70, 950, 1000 and the Toaster's 999999999. Nothing documents the layering (panel 70, dockview overlay 45, menus 950, context menu 1000). Define `--z-dock/sticky/popover/menu/modal/toast` tokens and use them.

**P13. Light preview overlays vanish.** `base-kerf-light-1920.png`: `1920x1080 - 30.000` in `--text-on-video` (white) over the placeholder frame is unreadable; only the filename chip has a backing. Give every overlay label the same `color-mix(var(--scrim) 55%, transparent)` chip.

**P14. Library header stack repeats the tab.** Tab "Library" + header "MEDIA" + sub-header "3 ASSETS" in the first 130px (`crop-library-kerf-dark.png`). Keep one of the first two; (library reviewer's call, noted because it is a shell-wide pattern: panel tab title and in-panel header duplicate in Library, Inspector ("Titles lane" under "Inspector") and Deliver).

**P15. Kerf mark is small and unlabelled.** `TitleBar.svelte:36` `KerfMark size={15}` (12x15px, radius 1px) with no wordmark and no click target; fine as a brand glyph but 18px with `margin-right:4px` would line its optical centre with the 12px menu text, and `aria-hidden` should be set.

**P16. Status "Stop" buttons are text with a hairline.** `StatusBar.svelte:109,126`: 10px, `padding:1px 6px`, `background:none`: looks like a label until hovered, and there is no hover (H2). Use the small `Btn` ghost variant with a `square` icon.

**P17. `--text-disabled` on active surfaces (Dark and Light).** `text-disabled` on `surface-active` is 2.28:1 Dark / 2.76 Light; any idle toggle (DUCK, S/L, the gear when Settings is open and a neighbour is highlighted) drawn on `surface-active` falls below the 3:1 the guard intends. Add `text-disabled` x `STATES` to the guard's SECONDARY set; it will fail for Dark. `#8493a3` clears 3:1 on active (3.95) and 4.67 on hover, but at that point it is no longer dimmer than `text-muted` (`#7a8696`), so the real fix is to stop drawing idle toggles on `surface-active`, or to make idle toggles `text-muted`.

**P19. Notification panel is not anchored.** It is `top: titlebar-h + 4; right: 10px; width: 380px` (`NotificationCenter.svelte:72`), not aligned to the bell at x~1847 and with no caret; at 960px wide it overlaps the Inspector tabs. Anchor it to the bell's rect like `MenuBar.fit()` does.

---

## Verified OK (so nobody re-checks)

- `text-primary` / `text-secondary` / `text-muted` on every resting surface meet 4.5:1 in all three presets (guard + my run); `text-on-accent` on amber is 8.0:1 Dark, 5.0 Light.
- The menubar ARIA pattern, hover-to-switch, Alt/F10 focus, compact "Menu" at <320px left cell all work; compact mode at 960px is clean (`narrow-960.png`).
- Workspace switching works, stays centred on the bar, and keeps project state (`ws-*.png`).
- Update flow chip + dialog look right (`update-chip.png`, `update-dialog.png`); amber primary there is the model for the other buttons.
- The colour-literal guard is in place (per `theme-guard.test.ts`; I did not re-run it); `--line-width` shape tokens are honoured by menus and chips.

---

## Top 10 by impact per effort

1. **H2 + P2 + P1: hover / pressed / disabled via CSS classes on `Btn`, `IconBtn`, WorkspaceTabs, TitleBar buttons, rail, dock tabs.** Largest perceived-quality jump; one afternoon for the shell; unlocks everything else.
2. **H1: self-host Geist / Geist Mono** (two `@fontsource-variable` imports, delete the dead `@import` and CSP entries). 30 minutes; every pixel of text changes for the better and becomes deterministic across OS.
3. **H3 + M20: visible focus ring on dock tabs, one `--focus-outline` for all focusables** (4 lines of CSS plus a broader `:where()` selector).
4. **H4: stop using `--text-disabled` for status bar text and the gear/bell/version.** Swap two tokens (`text-muted`, `text-secondary`); fixes 3.4:1 on the Settings button and all readouts.
5. **H5: remove the stacked opacity on disabled context-menu rows.** One-line fix (`ContextMenu.svelte:130`), 1.8:1 -> 3.1:1 and the reason text stops outranking the label.
6. **H6: Export button in the title bar.** One `Btn` and a click handler; makes the app's goal reachable from every workspace.
7. **M1 + M19 + M2: snap off-scale font sizes (9 / 10.5 / 11.5 / 12.5) to the scale, define `--type-caption`, align menubar/tab/ws sizes to 12px, add the font-size guard test.** Mostly mechanical search-and-replace; makes the type system real and keeps it that way.
8. **M3 + M4: one accelerator style (11px, keycaps for punctuation) and no icons in menubar menus.** Small edits in `MenuBar.svelte`/`menus.ts`/`ContextMenu.svelte`; visibly cleaner menus.
9. **M9: Sonner brought into the theme** (`theme` from settings, offset above the status bar, ghost amber Undo, token shadow/radius, shared icon map). ~20 lines; fixes the most visible transient UI.
10. **H7 + H8: themable shadow colour and border alphas** (Light overlays stop smudging, High contrast gets real component edges). Token work in `kerf-tokens.css`/`theme.ts` with one guard test; closes the two theme-quality gaps.

Next tier if time allows: M10 (themed tooltip action), M13 (24px+ hit targets, wider sash hit area), M15 (Light badge colours with the verified hexes), M16 (agent proposal indicator), M6 (context-menu keyboard support).
