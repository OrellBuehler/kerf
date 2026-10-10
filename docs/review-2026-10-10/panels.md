# Kerf side panels: UI/UX polish review

Scope: Library (rail, Media bin, Titles, Effects, Transitions, Audio, Transcript), Inspector, Mixer + master strip, Agent panel.
Method: read all the source under `frontend/src/lib/components/editor/` plus `routes/layout.css`; drove the harness (`?music=1&staged=1`) with headless Chrome at 1920x1080, 1366x768, 1100x700 (260 px inspector), dark and light, DSF 1 and 2; measured computed styles (hover, focus, contrast, hit sizes, font sizes) in the page. Nothing in the repo was touched.
Screenshots: `/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/panels/` (referred to as `panels/`). Source paths are relative to `frontend/src/lib/components/editor/` unless they start with `routes/` or `lib/`.

## Verdict

The structure is good. The library rail, the collapsible Inspector with a one-line summary per section, the review card and the mixer's gesture model are all considered. The finish is where it falls short of "professional", for four systemic reasons:

1. **The design system's fonts never load.** Everything is drawn in whatever the OS falls back to.
2. **Nothing reacts to the pointer.** There is no hover or pressed state on any button, chip, tile or row in the side panels, because all styling is inline.
3. **Several controls are tiny or clip.** Sliders are 4 px tall to hit, the keyframe ◇ is 13x12 px, and slider values are cut off at the Inspector's own minimum width.
4. **There is no shared control vocabulary.** The Inspector alone has four button looks, four field widths, two slider alignments and five time formats. Library and Agent each add their own variants.

Most fixes are one global CSS rule or one shared component each, so the top-10 list at the end is cheap relative to the visible gain.

---

## High

**H1. Webfonts (Geist / Geist Mono / Space Grotesk) are never loaded.**
- Where: `routes/layout.css:4` is an `@import url(https://fonts.googleapis.com/...)` placed *after* `@import 'tailwindcss'`. CSS only honours `@import` at the top of a sheet.
- Evidence:
  - `dev.log` has the warning "@import statements must precede all other statements".
  - The production CSS (`frontend/build/_app/immutable/assets/0.*.css`, dated Oct 9) contains zero `googleapis` strings.
  - `document.fonts` is empty in the harness, so every screenshot is DejaVu Sans.
  - On a user's machine it will be Segoe UI or SF. Every px-tuned width (72 px label column, 46 px value column, 56 px thumbs) was tuned against a font nobody sees.
- Fix: self-host. `bun add @fontsource-variable/geist @fontsource-variable/geist-mono @fontsource-variable/space-grotesk`, import them first in `layout.css`, and drop the Google `@import`. That also removes a network dependency from an offline-first desktop app and lets the CSP `font-src` / `style-src` entries for Google go. `@fontsource-variable/inter` in `package.json` is unused; remove it.

**H2. No hover, pressed or active feedback on any inline-styled control.**
- Evidence: measured computed background, colour and border before and after `:hover` on the rail tabs, bin rows, `IconBtn`, chips, effect tiles, `Btn` (ghost and secondary), Agent Apply / Preview / quick-edit pills / Connect header. All were identical. Only the Inspector `<summary>` reacts, and only because it uses a `<style>` block. Script: `panels/hover.mjs`.
- Where: `Btn.svelte:114`, `IconBtn.svelte:143`, `lib/chip.ts`, `LibraryPanel.svelte:205`, `MediaBin.svelte:398`, `EffectsTab.svelte:31`, `AgentPanel.svelte:403-620`. The context menu does hover in JS (`ContextMenu.svelte:131`), so the need is known.
- Fix: one global rule in `routes/layout.css`. Inline styles never set `box-shadow`, so a stylesheet rule wins without `!important`:
  - `button:not(:disabled):not([aria-pressed='true']):hover, [role='button']:hover { box-shadow: inset 0 0 0 100vmax color-mix(in srgb, var(--text-primary) 7%, transparent); }`
  - `button:not(:disabled):active { box-shadow: inset 0 0 0 100vmax color-mix(in srgb, var(--text-primary) 12%, transparent); }`
  - Add `transition: box-shadow var(--dur-fast)`.
  - This keeps the colour-literal guard happy (tokens only).

**H3. Slider rows overflow and clip their value at the panel's own minimum width.**
- Where: `Inspector.svelte:429-468` (`rangeRow`): fixed 72 px label + `flex:1` range + 46 px value + ◇. A native range input has an intrinsic min width, so the row does not shrink.
- Evidence: `panels/insp260-1.png` (260 px inspector, the registry minimum). "100%", "0.00" and "1.00×" are cut off at the right edge; the Timing summary reads "00:00.00 · 00:12...".
- Fix: add `input[type=range]{min-width:0}` globally, make the label `width:64px; flex:none` (or `minmax`), and let the value column be `min-width:40px`.

**H4. Hit targets are far below usable size.**
- Sliders: `input[type=range]` computes to height **4 px**; the layout.css rules size only the track, not the box. The thumb overflows, but grabbing the track means hitting 4 px. Fix in `routes/layout.css:152`: `input[type=range]{height:18px}` with the track pseudo-element unchanged.
- Keyframe ◇ (`Inspector.svelte:424`) is a text glyph in a 13.2x12 px button. Make it a 24x24 `IconBtn`-style button with an `Icon`. Today the glyph also depends on font fallback to draw a diamond.
- Transcript cut `×` (`TranscriptPanel.svelte:426-432`) is an 11 px icon with 1 px padding. Give it 24 px.
- Mixer M/S/D are 24 px, which is fine. The bin's "×N" badge is not a target, but see M10.

**H5. Agent: the queue card's Apply bypasses the stale check, the toast and the error path, and duplicates the review card's Apply.**
- Where: `AgentPanel.svelte:403-412` calls `agent.resolve(t.id)` (`agent.svelte.ts:61`). In kerf-core `resolve_task` applies the staged edit with `apply_staged(false)`.
- Consequences:
  - If the cut changed since staging, `StagedEditStale` is thrown into an un-caught click handler. The user sees nothing, whereas the review card (`applyProposal`, line 134) confirms first and toasts.
  - On success there is no "Applied" toast.
  - Two amber primary buttons for one proposal, about 200 px apart, with different words for the same negative action (Discard vs Dismiss).
- Fix: when `staged?.task_id === t.id`, hide Apply / Dismiss on the task card and show only "Reviewing above" (the line at 398 already says it). Otherwise route through `applyProposal` / `discardProposal`.

**H6. At 768 px height (or 260 px width) the review card's Apply is below the fold.**
- Where: `AgentPanel.svelte:706-737`. The composer is permanently about 100 px (input row plus a two-line hint) and the title bar is 40 px.
- Evidence: `panels/s768-edit.png` (322x400 panel leaves about 230 px of scroll area; Apply / Preview / Discard are not visible) and `panels/s960-edit.png`.
- Fix:
  - Make the review card's header and action row `position:sticky;top:0`, or move Apply / Preview / Discard into the card header next to the "review" badge.
  - Collapse the composer hint into the input's `title` or `aria-describedby`, and show it only when the panel is taller than about 520 px.

**H7. The master fader is the shortest fader in the mixer.**
- Where: `MasterStrip.svelte:174-282` stacks Limiter, Ceiling slider, Duck mode select, an unlabelled depth slider and a hint above the fader.
- Evidence: `panels/s768-audio.png`. The master travel is about 70 px against about 190 px for track faders; the dB scale drops labels (+6 / +3 vanish). At 1920 it is about 230 px vs 340 px (`panels/mix-dark-mixer.png`).
- Fix: move the Limiter / Duck controls into a collapsible "Master FX" block (collapsed by default, remembered), or lay Limiter and Ceiling in one row and put Duck mode behind a gear. The fader should get at least the height the track faders get.

**H8. Selecting any linked clip (the normal case) shows a warning-style "2 clips selected" banner.**
- Where: `Inspector.svelte:686-696`. A click on a linked picture selects its sound partner too, so the first thing the Inspector shows after clicking V1 is "2 clips selected: the settings below edit interview.mp4 only. Drag any selected clip to move them all; Del removes them all." in a bordered box (`panels/insp-video-collapsed.png`).
- Fix: show the banner only when the selection contains clips that are not link partners of each other. For a linked pair show a quiet "Linked to A1" chip beside the track badge in the header instead.

---

## Medium

**M1. Control columns do not line up inside the Inspector.**
- Evidence (measured slider x-ranges): Transform / Speed / Crop sliders span x=1561..1852, Color sliders 1561..1829 (23 px shorter, because only Color rows carry a ◇). The label-less Volume slider starts at x=12 while every other slider starts at x=94.
- Evidence (title editor): Text / Color / Font inputs start at x=66, sliders at x=94, Start / End are right-aligned (`panels/insp-title-0.png`).
- Where: `Inspector.svelte:429-468`, `:771`, `:557-614`.
- Fix:
  - Reserve the ◇ slot (a 24 px placeholder) in every `rangeRow`.
  - Use one `grid-template-columns: 72px 1fr 52px 24px` for all rows.
  - Give Volume a visible "Level" label.
  - Make Text / Color / Font rows use the same grid.

**M2. Five time formats.**
- Where: bin `2:00`; Inspector `00:12.50` (`tc`, `Inspector.svelte:274`); Titles list `00:00.00` (`TitlesControls.svelte:89`); Agent diff `0:12.5 → 0:09.0` and `12.5s`; transport `00:00:00` / `01:00:15`; In / Out / Start / End / Fades as bare decimals (`12.5`, `0`).
- Evidence: the Timing section shows `00:12.50` read-only rows directly above an `In` field showing `0` and an `Out` field showing `12.5` (`panels/c-timing.png`). The summary "00:00.00 · 00:12.50" has no labels.
- Fix: one `formatTime` (hh:mm:ss.cc, tabular figures) used by all; edit fields accept and show it; add "s" unit or timecode to the In / Out / Fade fields.

**M3. Slider-vs-field choices are inconsistent, and sliders lack reset and neutral marks.**
- Fades are bare number fields but Speed is a slider with 0.25 steps (no way to type 1.1x or 0.8x). Position, Rotation and Crop are slider-only with no numeric entry.
- Bipolar parameters (Position X/Y, Rotation, Brightness, Warmth, Yaw, Pitch, Roll, Centre) fill from the left, so "0.00" looks 50% full. The mixer pan already does it right with `fillFrom` (`MixSlider.svelte:34`).
- Defaults sit at odd places: Contrast 1.00 is at 25% of its 0..4 track, Saturation 1.00 at 33%, with no tick or detent (`panels/c-color.png`).
- No double-click-to-reset on Inspector sliders, though mixer sliders have it.
- Fix:
  - Add a shared `<SliderRow>` with a number field instead of a bare readout (click-to-type), a neutral tick, bipolar fill and double-click / Alt-click reset.
  - Replace the Fades and Speed number fields with it too.

**M4. Colour section: summary is wrong, and keyframe state is hard to read.**
- Where: `Inspector.svelte:994`: `summary = activeLook(col)?.label ?? 'Custom'`. A clip with the default colour reads "Custom". Transform / Volume / 360 say "Animated · N keys", but Color never does, even with a keyed brightness (verified, `panels/k-color.png`).
- Keyframe ◇: "keyed elsewhere" vs "not keyed" differ only by `text-secondary` vs `text-muted` (`Inspector.svelte:424`). There is no prev / next key navigation. A keyed slider looks identical to a static one.
- Fix:
  - Summary: "None" when equal to `DEFAULT_COLOR`, "Animated · N" when keyed, the look label when it matches, otherwise "Custom".
  - Render keyed-elsewhere as an amber outline diamond and keyed-here as a filled amber one (use `Icon`, not a text glyph).
  - Add ◀ ▶ jump-to-key when a property is keyed.

**M5. Too many button looks, and uneven control heights.**
- The Inspector alone mixes four button looks: `chip()` (27.4 px, `Inspector.svelte:369`, duplicating `lib/chip.ts`), `Btn secondary sm` (30 px: Analyze), `Btn ghost` (borderless: "+ Keyframe @ playhead", "Clear keyframes", "Ripple delete") and raw destructive outlines. Agent adds raw 30 px buttons and full-radius pills; Library uses 4 px-radius chips.
- Heights of neighbouring controls: chip 27.4, select 28, number input 29.4, Btn sm 30.
- "Fit to video…" (`Inspector.svelte:798`), "Smart crop" and "Reset crop" are actions drawn as toggle chips.
- Borderless ghost buttons read as plain text (`panels/k-anim.png`: "+ Keyframe @ playhead" and "Clear" look like captions).
- Fix: one `Btn` size scale (28 px for all dense panels), `Chip` for toggles only, ghost buttons get a hairline `--border-default` or at minimum the H2 hover wash, delete the duplicate `chip` in `Inspector.svelte`. Pick one chip radius (4 px) and use it in Agent too.

**M6. Titles controls row is ragged.**
- Where: `TitlesControls.svelte:126-153`. "+ Text" has `flex:1` and borderless, so its label floats mid-row; "Captions" / "Voiceover…" / "Import captions…" wrap unpredictably.
- Evidence: `panels/c-titles.png` (400 px) and the Inspector's Titles lane (453 px) wrap differently.
- Fix: a 2-column grid of equal bordered `Btn sm`: `+ Text | Captions` / `Voiceover… | Import captions…`. Put the Clear action as an icon button next to Recaption.

**M7. Effect / audio tile grid clips and wraps.**
- Where: `EffectsTab.svelte:75`, `AudioTab.svelte:255`, `grid-template-columns:repeat(auto-fill,minmax(76px,1fr))`.
- Evidence: "Compressor" is 81 px of text in a 78 px content box and runs into the right border (scrollWidth 81 > clientWidth 78, `panels/c-audio-tiles.png`); at the Audio workspace's 320 px library it is cramped; "Chroma key" wraps to two lines (`panels/lib-effects-clip.png`).
- Fix: `minmax(96px,1fr)`, `white-space:nowrap`, `overflow:hidden; text-overflow:ellipsis` with `title`. Consider a 2-column list with a short hint under each name.

**M8. Selecting an asset in the bin does nothing in the Inspector, while the facts live in a 960 px context menu.**
- Evidence: clicking a bin row leaves the Inspector on "Select a clip or a title to inspect it" (verified). The right-click menu is 320x960 px at 1080 high and scrolls at 768: ten info rows, ten analysis rows, eight actions (`panels/ctx-bin-dark.png`, `MediaBin.svelte:111-306`).
- Fix: when an asset (and no clip) is selected, show an "Asset" Inspector (name, spec, used-in, imported, analysis facts, proxy facts, Analyze / Proxy buttons). Cut the context menu to actions only.

**M9. Bin context menu details.**
- No arrow-key navigation or initial focus (only Esc, `ContextMenu.svelte:64`); `role=menu` without the menu keyboard model.
- Fractional font sizes (10.5 px at `:100` and `:114`, 12.5 px at `:126`) off the 10/11/12/13 scale.
- "Mark as 360 (equirect)" uses the `rotate-ccw` (undo) icon (`MediaBin.svelte:248`); "Add audio to A1 again" is ambiguous.
- Fix: roving focus with ↑ ↓ Home End Enter; snap sizes to 11 / 12 / 13; use a globe or sphere icon; label "Add audio to A1 (again)".

**M10. Bin row noise.**
- Five analysis chips are always drawn at 9 px mono with cryptic abbreviations (SIL SCN LUFS BPM TXT), including kinds that can never apply: an mp3 shows SCN as a permanent outlined "not run". Docs say `autoSteps` skips kinds a file cannot give, but the chip still shows (`MediaChips.svelte:177`, `panels/l-bin.png`).
- "×2" (used in N clips) has no title (`MediaBin.svelte:449`).
- The harness shows "no proxy" for an mp3 (a harness artefact; the real backend returns `not_needed`, but confirm).
- At 233 px content width a row is about 140 px tall (spec line wraps, proxy badge and chips on separate lines), so 3 assets do not fit a 1366x768 screen (`panels/s768-edit.png`).
- Fix:
  - Render non-applicable kinds as `off` (dashed "–") or omit them.
  - Collapse the five chips into a compact "3/5" meter with a tooltip, or show them only on hover / selection.
  - Add `title="Used in 2 clips"`.
  - Offer a compact list mode (name + duration + one status dot).

**M11. Text-overlay editor details.**
- Title text is typed into a **monospace** field (`inputCss` = mono, used for `Text` at `Inspector.svelte:564`); copy should be sans.
- Colour and Box are free-text fields ("white", "none", `Inspector.svelte:582-600`) with no swatch or picker. Add `<input type=color>` plus a hex field.
- Start / End are bare seconds (see M2).
- Fix: sans font for text inputs; swatch-plus-hex colour field; align labels to the shared grid (M1).

**M12. Contrast gaps the theme guard does not cover.** (Measured with composited backgrounds, `panels/contrast*.mjs`.)

| Element | Dark | Light |
|---|---|---|
| Analysis chip "done" (9 px green on mint, `MediaChips.svelte:186`) | 7.1 | **2.93** |
| "proxy 42%" badge, 10 px | 7.9 | 4.3 |
| Transcript struck-through (cut) lines | **1.74** | **1.82** |
| Transcript timestamps (10 px, `text-disabled`) | 3.6 | 3.33 |
| Review card group labels CLIPS / TEXT (`AgentPanel.svelte:483`) | 3.11 | 3.68 |
| Task remove `×` | 3.11 | 3.68 |
| Mixer idle M / S / D (12 px) | 3.11 | 3.68 |
| Muted spec line on the *selected* bin row | 3.97 | |

- The cut-line number is double dimming: `text-disabled` colour plus the global `button:disabled{opacity:.5}` (`routes/layout.css:150`), because the seek button is disabled for cut lines (`TranscriptPanel.svelte:409`). Someone restoring a cut must be able to read what was cut.
- Fix: the guard checks status hues on panels but not on their own tinted surfaces, so add the `success-surface` / `danger-surface` / `agent-surface` pairs to `theme-guard.test.ts`. Cut lines should use `text-muted` + `line-through` with no opacity. Review labels and timestamps should use `text-muted`. Raise chips to 10 px.

**M13. Agent panel idiom clashes.**
- Three section idioms in one panel: overline + rule (Queue, History, `AgentPanel.svelte:318`), a 13 px bold native `<details>` (Quick edits, `:609`), a bordered card with chevron (Connect, `:567`).
- Status styling: `failed` is `neutral` tone with the `history` icon (`data.ts:13`), indistinguishable from "Applied". Use `danger` and an alert icon.
- Remove-from-queue is a rotated `plus` icon in `text-disabled` (`:372`); use the `x` icon at `text-muted`.
- Task results and `meta` prose are set in monospace (`:380`); History source label too (`:650`).
- Tiny type: History "now" 9 px (`:656`), Revert 10 px (`:664`).
- The `idle` badge in the header plus "No agent connected" below are two status indicators.
- Fix: use the overline idiom for all sections (Quick edits collapsible via a chevron in the same header), sans for prose, one status line.

**M14. "Connect an agent" is far from "No agent connected", and the command row is silently clipped.**
- When no agent is connected, the setup card sits below the queue and the review card.
- The command row is `overflow-x:auto; white-space:nowrap` (`AgentPanel.svelte:335`): `claude mcp add --transport http kerf http://127.0` is cut mid-URL with no ellipsis or scrollbar (`panels/agent-connect.png`).
- Fix: make "No agent connected" a button ("Connect…") that opens the card; wrap the code (`white-space:pre-wrap; word-break:break-all`).

**M15. Focus and accessibility inconsistencies.**
- Bin rows are `div[role=button]`, so the global `button:focus-visible` ring (2 px amber) misses them. They get Chrome's thin default (`panels/focus-bin.png`).
- Only Enter selects a row; Space does nothing (`MediaBin.svelte:396`).
- Selection is exposed to AT by border colour alone (no `aria-pressed` / `aria-selected`).
- `IconBtn` always renders `aria-pressed` (`IconBtn.svelte:146`), so plain actions like Import, Voiceover and collapse announce as "toggle button, not pressed" (verified `aria-pressed="false"` on `button[title=Import]`).
- Fix: add `[role=button]:focus-visible` to the global focus rule, handle Space, `aria-pressed` on the row only for the toggled state, drop `aria-pressed` unless `active` is passed.

**M16. Inspector shows irrelevant sections.**
- **Mask** (`Inspector.svelte:1019`, outside the `kind==='video'` guard) and **Transition (in)** show on a music clip (`panels/insp-audio-all-1.png`). Mask is meaningless on audio.
- **360 reframe** (`:837`) shows a paragraph, a source select and a "Mark as 360 & reframe" button on every flat video clip, in fifth position.
- Fix: gate Mask (and the transition if audio crossfades are not supported) on video; hide the manual 360 override behind an "Advanced" disclosure, or show "Not 360" as a one-line summary with a small "This is 360 footage…" link.

**M17. Section density.**
- Collapsed sections are 49 px each (`InspectorSection.svelte:17-18`: 10 px margin, 38 px min-height, 10 px padding); a video clip has 13 sections, about 640 px of headers.
- The Timing summary "00:00.00 · 00:12.50" has no labels.
- "Trim source" is a 12 px muted subhead (`Inspector.svelte:738`) in a different style from section titles and the overline subheads used elsewhere.
- Fix: 32-34 px section headers, summary "Start 0:00 · Dur 0:12.5", subheads in the overline style.

**M18. Mixer details.**
- Duck is "D" on strips but "DUCK" in the timeline headers.
- A disabled depth slider with no label or readout sits under the Duck mode select (`MasterStrip.svelte:227-247`), looking like a stray control.
- `.strip.quiet` fades the whole strip to 60% (`MixerStrip.svelte:492`), controls included.
- 9-10 px type is everywhere (30 nodes at 9 px in the Mixer: meter scale labels, "ducks on export" hint, `C`).
- Fix: "Duck" wording consistent (D with `title`, or a `DUCK` pill if width allows); hide the depth slider unless Speech gate is chosen, or label it; dim only the meter / name, not the toggles; raise the floor to 10 px.

**M19. Transcript.**
- The active line is a darker inset block with no accent (`TranscriptPanel.svelte:402`), which is easy to miss.
- Lines have no hover state; the rows are buttons without affordance.
- Empty-state body text is 11 px (`:349`), unlike the 12 px used by the other tabs' hints.
- Fix: left accent bar for the active line, hover wash, 12 px body.

**M20. Native number-input spinners.**
- Chrome's grey up/down block appears on hover and focus (`panels/spin-hover.png`), unstyled. The focus ring is a 2 px amber outline with 2 px offset, heavier than anything else around it (`routes/layout.css:143`).
- Fix: `input[type=number]::-webkit-inner-spin-button{opacity:.5}` or hide it and add click-drag scrub plus ↑/↓. For inputs use an inset 1 px accent border instead of an offset outline.

---

## Polish (little details)

1. **"1 assets"**: `MediaBin.svelte:363` prints `{assets.length} assets`; pluralise ("1 asset").
2. **Library header stacks three titles**: dock tab "Library", overline "MEDIA", overline "3 ASSETS" (`panels/lib-media-after.png`). Fold the count into the section header ("MEDIA · 3").
3. **Import `IconBtn` title is just "Import"** (`MediaBin.svelte:381`); use "Import media…" with the shortcut from the keymap (`withShortcut`).
4. **Bin rows have no double-click or Enter to add at the playhead**, and no search or sort. Selection by Enter only selects.
5. **Cryptic labels**: "Crop L / R / T / B" (`Inspector.svelte:980-989`) → "Left / Right / Top / Bottom". Mixed spellings "Centre X" (`:1041`) vs "Color". "Pos X" (title editor) vs "Position X" (Transform). "Transition (in)" vs the library's "Transitions". "Warmth" is the label for `temperature`.
6. **"Invert" checkbox is unstyled** (`Inspector.svelte:1054`) while Bold and Reverse use `accent-color` and 18 px. Share one `Checkbox`/`Switch`.
7. **Destructive pair at the bottom** (`Inspector.svelte:1159-1177`): a full-width red outline "Remove clip" plus a ghost "Ripple delete · close gap" with a vague label. Both are already on Delete / the context menu; make it a small "…" menu or a quieter text button, and name it "Ripple delete".
8. **Duplicate helpers**: `chip()` is defined in both `Inspector.svelte:369` and `lib/chip.ts`; the tile style string is copied in `EffectsTab.svelte:31` and `AudioTab.svelte:237`; the empty-state hint box is pasted in four tabs. Extract `Hint`, `Tile`, `Chip` components.
9. **Hard-coded `1px solid var(--danger)`** (`AgentPanel.svelte:439`) and `border:1px solid var(--track-text-edge)` (`Inspector.svelte:538`) and the 2 px left accents (`AgentPanel.svelte:357, 454`) bypass `--line-width`, so High-contrast and shape themes will not scale them.
10. **Read-only values vs inputs are not right-aligned to the same edge**: read rows end at x=440, input text ends at x=432 (`panels/c-timing.png`).
11. **Native selects** in Inspector (Transition type, effects) and the library (speech model) show the browser chevron; style `appearance:none` with a token-coloured chevron so it matches the field.
12. **Number fields hold "0" and "12.5" with no unit**; add an inline suffix ("s"). The Transitions tab's "Length ... s" (`TransitionsTab.svelte:154-171`) is the right pattern.
13. **Effects tab reason box** says "Select a clip on the timeline" in the Library with a lightbulb; same text repeated in Effects, Transitions and Audio. Fine, but extract it; Transitions puts it above the "None" chip while Effects puts it above the first heading, with different margins (12 vs 14 px).
14. **"Reset" chip sits among the looks** (`EffectsTab.svelte:66`) and wraps alone onto line two at 400 px. Put "Reset" as a text button on the heading line.
15. **Mixer pan label** is "C" for centre but "L12 / R30"-style otherwise (`MixerStrip.svelte:453`); make the label width fixed so the slider does not shift.
16. **Mixer footnote** (`Mixer.svelte:158`) is 11 px muted prose below the strips, cut off at 768 px height. Move the two caveats into the Duck / Limiter tooltips (already there) and drop the footnote.
17. **Agent task `Badge` colours**: `Queued` and `Applied` are the same neutral badge (`data.ts:9-12`). Give Applied the success tone and Queued a clock icon (the icon is already there; the badge is not).
18. **Review card**: "Your timeline is untouched until you apply these." is shown every time; reduce to a tooltip on the headline once the user has applied one proposal, or fold it into the header subline (`AgentPanel.svelte:466`).
19. **Quick-edit pills**: radius-full, `play` vs `list-plus` icons carry the "runs now vs queued" meaning that the two hint paragraphs also carry (`AgentPanel.svelte:610-626`). Pick one signal (the icon plus a single legend line).
20. **Title bar redundancy in Agent**: dock tab "Agent" and the in-panel header "Agent queue" with a 22 px icon tile (`AgentPanel.svelte:423-433`). The status badge could go on the tab row.
21. **History row `now` marker 9 px mono green** (`AgentPanel.svelte:656`); use the `Badge success` at 10 px for readability.
22. **Library rail**: clicking the *active* icon folds the library, which is discoverable only through the tooltip text ("click to collapse"). Fine for VS Code users; consider a subtle chevron on the active icon. Rail tooltips are native `title` (about 1 s delay); a custom tooltip would match the editor chrome.
23. **Inspector `Reverse` / `Bold` / `Invert` rows**: label left, checkbox hard right with a wide empty gap; make the whole row a switch (`role=switch`) or put the checkbox first.
24. **`Fit to video…` chip** under Music is a chip, but a dialog opener: add the ellipsis-style icon or use `Btn secondary sm`.
25. **Section summaries** use `float:right` inside `<summary>` (`InspectorSection.svelte:22`); on a narrow panel the title and a long summary can wrap under each other. Use `display:flex; justify-content:space-between` on a wrapper span.
26. **Bin context-menu header path** is truncated left-to-right (`ContextMenu.svelte:100`); for paths truncate in the middle (keep the file name end visible).

---

## Top 10 by impact per effort

1. **H1 Self-host the fonts and remove the dead `@import`** (about 20 min). Everything on screen changes for the better, and the offline desktop app stops depending on a CDN.
2. **H2 One global hover / active rule** (about 15 min). Fixes "feels dead" across the Library, Inspector and Agent.
3. **H4 + H3 Slider box height 4 px to 18 px, `min-width:0`, key toggle to a 24 px icon button** (about 30 min). Directly improves precision editing and ends the clipped values at narrow widths.
4. **H5 Agent task card: hide Apply / Dismiss when it is the staged task, or route through `applyProposal`** (about 20 min). Removes a silent failure and a double primary action.
5. **H8 Linked-pair selection banner** (about 15 min). The first thing a user sees after clicking a clip is currently a warning that is not one.
6. **M1 + M3 Shared `SliderRow` with a reserved ◇ slot, bipolar fill, neutral tick, number field, double-click reset** (about 2-3 h). One component fixes alignment, reset, and the Fades / Speed / Crop inconsistencies.
7. **M16 Gate Mask / Transition to video and tuck the 360 override behind Advanced** (about 20 min). Removes visible nonsense on every audio clip and shortens every video clip's inspector.
8. **H6 Sticky review actions + compact composer** (about 45 min). Makes the Agent panel's main action reachable on a laptop screen.
9. **M7 + M6 + M5 Fix the tile grid (`minmax(96px)`) and the Titles button grid, and settle on one Btn size scale** (about 1 h). Cleans up the most visible ragged edges in the Library.
10. **M12 Fix the contrast gaps (light chip 2.93:1, transcript cut lines 1.7:1, 10 px disabled-grey labels) and extend the guard to tinted surfaces** (about 1 h). Real readability gain, and the existing guard prevents regression.

Runner-up: H7 (master strip height at laptop sizes). It is the biggest layout problem in the Mixer and is moderate effort (about 1-2 h).
