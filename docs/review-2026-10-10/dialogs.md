# Kerf UI/UX review: modal dialogs, task flows and microcopy

Scope: Export, Settings (all six sections, shortcut recording), Update, Voiceover, Fit music, Separate stems, Import captions, confirm prompts, progress/cancel states, toasts and the notification log, empty/first-run state, and copy across the app.
Method: read every dialog source plus `modal.ts`, `notifications.svelte.ts`, `updater.svelte.ts`, `export-presets.ts`, `menus.ts`, `keymap.ts`. Then drove the harness (`localhost:1420`, with `?update=1` and `?music=1`) with headless Playwright at 1920x1080 and 1366x768, dark and light themes, DPR 2 crops, keyboard-only tab traces, and in-page injection of the in-progress states the harness doesn't simulate (export run, update error, empty project). Screenshots are in `.../review/dialogs/` (names cited as `[shot]`). Nothing in the repo was touched.

## Verdict

The dialogs are structurally good. The modal contract (trapFocus, inert page behind, Escape rules, running-state lock-outs), the shortcut recorder with its Swap/Unbind/Cancel conflict flow, the live Fit preview, and the "say what the first run downloads" pattern in Voiceover/Stems are better than most desktop editors. Layout holds at 1366x768, light theme is clean, nothing overflows horizontally.

What keeps it from feeling professional is a layer of **wrong or noisy information** (the Export readiness panel judging platforms for MP3s and GIFs, "Kerf is up to date" over a network error, a blank frame-rate select), **a few ways to lose work or focus** (scrim click discards a typed script and all export options; focus drops to `<body>` when a run starts), and **copy drift** (same action named 4-5 ways, `Open project….`, raw backend errors, `Could not` vs `Couldn't`, Yes/No on a data-loss prompt). Almost all of it is a one-to-ten-line fix. The root cause of most dialog inconsistencies is that the six dialogs each hand-roll their own scrim/header/close/focus code (z-index 50 vs 60, `1px` vs `var(--line-width)`, trapFocus in five but not Voiceover, aria-label in four but not Update). A shared `Modal.svelte` (scrim, header + named close button, trapFocus with initial-focus hook, scrim-click policy, "busy" lock) would remove about a third of the findings below at once.

Counts: 10 High, 31 Medium, 16 Polish (57 findings; the Top 10 at the end ranks them).

---

## High

**H1. Export "Where it is going" judges platforms it has no business judging, and puts an amber warning on the happiest path**
- Wrong: audio-only presets show "Ready for YouTube" plus "This frame is letterboxed on Instagram Reels, YouTube Shorts, TikTok and Instagram feed" [20-export-preset-audio_mp3]. The GIF preset shows "YouTube - 854x480 is below YouTube's 1920x1080; the platform will upscale it" [20-export-preset-gif]. An empty timeline says "Ready for YouTube" [122-empty-export]. A project already cut 1:1 is told "A 1:1 cut is letterboxed on Instagram Reels, TikTok and YouTube. Pick a delivery frame in the toolbar" [160-export-projectframe]. And the default 16:9 export always carries an amber triangle about four platforms the user never picked [10-export-1920].
- Where: `ExportDialog.svelte:468-470` (renders `<Readiness>` unconditionally when no variants); `Readiness.svelte:63-97`.
- Fix: render it only when `showVideo && opts.video_codec !== 'gif' && hasVideo`. Judge only platforms the user ticked under Deliver to; with none ticked show one neutral line: "1920x1080, 16:9, 0:20. Fine for YouTube and the web. Tick a shape under Deliver to for Reels, Shorts or TikTok." Reserve amber/red for real limits (length, size). Replace the shape sentence with: "Instagram Reels, YouTube Shorts and TikTok are vertical (9:16). This 16:9 cut would be letterboxed. Tick 9:16 under Deliver to to export a vertical version." Drop "in the toolbar" (the toolbar is behind the modal). Rename the heading "Where it is going" to "Platform check".

**H2. GIF preset: Frame rate select renders blank**
- Where: preset sets `fps: 15` (`export-presets.ts:298`), `FRAME_RATES` has no 15 (`:226-236`), so the `<select>` has no matching option [20-export-preset-gif].
- Fix: add `{label:'15 fps',value:15}` (and 12, 10) to `FRAME_RATES`, and as a safety net in `ExportDialog.svelte:423-428` append the current value as an option when it isn't in the list.

**H3. Update dialog tells the wrong story when the check fails**
- Wrong: with a failed check (no `u.update`) the title is still "Kerf is up to date" while a red box shows a raw reqwest error that is clipped at the right edge (no wrapping) [63-update-check-error]. The install-failure note "In-place updates need the AppImage build on Linux" is shown for every install error on every OS (`UpdateDialog.svelte:119-121`).
- Where: `UpdateDialog.svelte:29-39` (title), `:114-124` (error box), `:119`.
- Fix: title `Couldn't check for updates` when `phase==='error' && !u.update`. Body: "Kerf couldn't reach the update server. Check your connection and try again." Put the raw text under a `<details><summary>Details</summary>` with `overflow-wrap:anywhere`. Gate the AppImage note on `settings.platform === 'linux'` and reword: "In-place updates need the AppImage build. For a .deb or .rpm install, download the new version from the release page." Capitalise the first letter of whatever message is shown ("Failed to install: permission denied").

**H4. Stray scrim click (or Esc) discards typed work**
- Verified: type a script in Voiceover, click the dark area, reopen: script is empty. Configure Export (preset, Deliver to, Advanced), click the scrim, reopen: back to Web 1080p. `docs/frontend.md` says export options are deliberately not persisted.
- Where: `VoiceoverDialog.svelte:136-140`, `ExportDialog.svelte:306-309`.
- Fix: Voiceover: ignore scrim clicks while `text.trim()` is non-empty, and save the draft to localStorage (`kerf.voiceover.draft`) on input, restore on open, clear on success. Export: scrim click does nothing (Esc/Cancel/X still close) and keep the last `opts` + preset id on `ui` for the session. Settings/Update/Fit/Stems can keep scrim-to-close (nothing to lose).

**H5. Focus is dropped to `<body>` at the worst moments, and Voiceover isn't trapped at all**
- Verified: Stems "Separate" unmounts the button that had focus, so `document.activeElement` is `BODY` for the whole run and after Cancel [t13]; Escape and Tab then act outside the dialog. Same pattern in Voiceover (Generate to Cancel) and Export (Export to Stop, `ExportDialog.svelte:679-707`). `VoiceoverDialog.svelte:130-141` has no `use:trapFocus`: Tab walked out to `BODY` after Cancel [t25]. After any dialog opened from a menu closes (File > Export...), focus is `BODY` because the menu item that was `previous` is gone (`modal.ts:41`, `previous?.isConnected`).
- Fix: `use:trapFocus` on Voiceover. On run start move focus to the Cancel/Stop button (`bind:this` + `tick()`); on failure/cancel return it to the primary button. In `trapFocus`, when `previous` is no longer connected, fall back to a `[data-focus-return]` element (the menubar's roving tab stop). Give `trapFocus` an optional "initial focus" selector so dialogs can focus the first field or primary action instead of the root (see P8).

**H6. Close buttons have no accessible name; Update dialog has no label**
- `<Btn>✕</Btn>` in Export (`:323`), Settings (`:145`), Update (`:66`), Voiceover (`:153`) reads as "multiplication x"; Fit and Stems already pass `aria-label="Close"`. `UpdateDialog.svelte:44-55` has `role=dialog` with no `aria-label`. Export's label says "Export video" though it also exports audio.
- Fix: `aria-label="Close"` and `<Icon n="x">` instead of the glyph everywhere; `aria-labelledby` the title span on Update; Export `aria-label="Export"`.

**H7. The notification log drops the reason, and dark-theme toast descriptions are unreadable**
- `notifications.record(kind, message)` ignores `opts.description` (`notifications.svelte.ts:80`), so "Could not save the theme" shows without "permission denied (os error 13)" in the bell panel [101-notifications-dark]. In the dark theme the description line of a toast is ~2:1 grey on grey [103-toasts-sheet-dark, row 2 left]. "About Kerf" puts its entire content in that description (see M17).
- Fix: `record(kind, text, description?)`, render it under the line in `NotificationCenter` (text-secondary, 12px). In `layout.css`: `[data-sonner-toast] [data-description]{color:var(--text-secondary)}`.

**H8. Data-loss confirms are native Yes/No with four different wordings and no Save**
- `confirmAction` calls `ask(message,{title,kind:'warning'})` with no labels (`api.ts:530-534`), so users get Yes/No under "Close Kerf and lose it?". Four phrasings for one fact: `+page.svelte:115` "never been saved. Close Kerf and lose it?", `:235` "never been saved and will be lost. Continue?" (title "Unsaved project"), `:296` "Quit Kerf and lose it?", `UpdateDialog.svelte:24` "...lost on restart. Restart anyway?". The update dialog also shows its own "unsaved changes" banner ("changes" is wrong: a never-saved project has no *changes*, the whole project is unsaved).
- Fix: use `message()` (plugin-dialog 2.4+) with `buttons:{yes:'Save...',no:"Don't save",cancel:'Cancel'}`, or at minimum pass `okLabel`/`cancelLabel`. Copy deck below.

**H9. A silent 6-hourly check can pop a blocking modal mid-edit, and the download blocks the app**
- `updater.svelte.ts:56-59` re-checks every 6 h silently, and `:82-90` opens the dialog for any unseen version, so a release published during a session interrupts whatever the user is doing (the page goes inert). During download there is no X, Esc is disabled and the footer says "Kerf will keep running until you restart" (`UpdateDialog.svelte:135`), which can't be true of a modal that blocks the whole app.
- Fix: auto-open only on the startup check; later checks light the title-bar chip and raise a toast "Kerf 0.19.0 is available" with a "View" action. While downloading, offer "Hide" (continue in the status bar) instead of locking; footer copy "Downloading in the background."

**H10. Keyboard recorder: action names with trailing ellipses leak into sentences**
- "Ctrl+O is already used by Open project...." and a button "Unbind Open project..." [51-kbd-conflict] (the "..." on a button promises another dialog). Same for the screen-reader announcements.
- Where: `KeyboardSettings.svelte:52` (`nameOf`), `:356`, `:373-375`, `:387-388`, announcements at `:71, 93, 110, 127, 140`.
- Fix: `const plain = (s: string) => s.replace(/…$/, '')` applied to every label used in prose. Result: "Ctrl+O is already used by Open project." / "Unbind Open project". Also `:232` tooltip says "Put all 1 changed shortcuts back..." (the inline confirm handles singular, the tooltip doesn't).

---

## Medium

### Export dialog

**M1. Quality reads "Custom - see Advanced" the moment you pick Reels or Square** [20-export-preset-vertical_reel]. Those presets use CRF 23, which is not one of the three named points (20 +/- 6/4). The user hasn't customised anything. Where: `ExportDialog.svelte:118-124`, `export-presets.ts` `vertical_reel`/`square_social`. Fix: set the two presets to CRF 20 or 26, or snap to the nearest named point, or label it `Custom (CRF 23)`.

**M2. Validation is far from the field and unhelpful** [31-export-invalid]. The red strip sits above the footer ("A target video bitrate is required for bitrate / two-pass."), the offending input is scrolled out of view and not marked, the glyph is a text "⚠" (`ExportDialog.svelte:674`). On an empty project the only message is "These settings would export nothing." Where: `export-presets.ts:371-389`. Fix: `aria-invalid` plus a red border on the field, an Icon instead of the glyph, and copy:
  - bitrate missing: "Enter a target bitrate for Bitrate or Two-pass, for example 8M."
  - nothing to export (empty): "Nothing to export yet. Add a clip to the timeline." (and disable File > Export... with reason "The cut has no clips", like Save cover frame...)
  - nothing to export (otherwise): "Turn on video or audio to export."
  - `libx264 can't go in a .webm file.` -> "H.264 can't be stored in a .webm file. Pick another codec or container." (use the codec's label, not its id)
  - `Two-pass encoding isn't supported for hardware encoder h264_nvenc; use crf or bitrate.` -> "Two-pass isn't available with GPU encoders. Choose Quality or Bitrate."
  - `Invalid video bitrate "abc".` -> "'abc' isn't a bitrate. Use a number with k or M, for example 8M or 6500k."
  - `tune "film" is not valid for libx265.` -> "The 'film' tune isn't available for H.265. Set Tune to None."
  - `GIF export needs video...` -> "A GIF needs a video clip on the timeline."
  - `MP3 is audio-only, but the timeline has no audio.` -> "MP3 needs sound, and the timeline has none."

**M3. While rendering the form stays editable and nothing says closing is safe** [32-export-rendering]. The progress text has `min-width:104px` so the bar's right end shifts as the readout grows from "42%" to "90% · 3s left". Where: `ExportDialog.svelte:326-667` (no `fieldset disabled`), `:679-700`. Fix: wrap the body in `<fieldset disabled={rendering}>`; fixed `min-width:150px` with `font-variant-numeric:tabular-nums`; add the line "You can close this window. The export keeps running; stop it here or from the status bar." and a label "Exporting file 2 of 3 - cut-1x1.mp4".

**M4. Audio-only exports show an "Include audio" checkbox** (unchecking it yields the "export nothing" error) [20-export-preset-audio_mp3]. Where: `ExportDialog.svelte:450-451`. Fix: hide the toggle when `info.audioOnly`.

**M5. Alignment: hints and select widths** [10-export-1920]. Hints under selects are right-aligned in two places (`:386`, `:418`) and left-aligned in others (`:441`, `:458`, `:538`). Select widths follow content (`selectCss` `min-width:180px`), so "Normalize loudness" is 256 px wide and starts 76 px left of the others. Fix: `width:220px` on every select and left-align all hints under the label column, or right-align all under the control.

**M6. The one line that says what you will get is hidden in Advanced, and its units disagree with the form.** The summary "MP4 · H.264 · CRF 20 · 1920x1080 · AAC 192k · faststart" lives inside the collapsed disclosure (`:472-480`). It prints `source` where the select says "Source"/"Project frame", `30fps` where the app elsewhere writes `30 fps`, and `-14 LUFS` with a hyphen where the select uses a true minus `−14` (`export-presets.ts:416-419, 432`). Fix: move it above the footer (always visible, mono, 12px) and align the strings (`Project frame`, `30 fps`, `−14 LUFS`).

**M7. Range row is in the wrong place and ignores the marks.** "Range" appears between Destination and Picture [30-export-range] and defaults to "Full timeline" even when in/out marks were set. Fix: own "Range" row below Preset; label the option `In to out (0:02.5-0:12.3)`; default to the marked range when marks exist (with the select to flip back).

**M8. "Browse" has no ellipsis, and the primary button silently opens a file picker when no destination is chosen** (`:350`, `doExport` `:211-213`). Fix: `Browse...`; label the primary `Export...` while `outputPath` is empty; placeholder `No file chosen yet` in the sans font (see P1).

**M9. Preset names and descriptions are inconsistent and some are jargon** (`export-presets.ts:249-313`): "Half-size UHD via H.265.", "Next-gen efficiency via SVT-AV1.", "Audio · MP3 320k". Group the select with `<optgroup>` (Video / Social / Master / Audio) and use the copy deck below.

**M10. Advanced naming:** three things called "Quality" (the Picture select, the segmented "Quality (CRF)", the slider "Quality (CRF)"), two "Container" (the select and the section holding Fast start/Title), a bare "Title" (it is metadata), "GPU decode (hwaccel)", option values in raw ffmpeg case (`medium`, `high`, `yuv420p` next to `None`, `Auto`), and a hint in the 3.27:1 disabled colour: "lower = higher quality & larger file" (`:534`). Fix: segmented -> "Constant quality (CRF)"; slider label "CRF" with hint "Lower CRF means higher quality and a larger file."; section "Container" -> "File"; "Title" -> "Metadata title"; "GPU decode (hwaccel)" -> "Use GPU decoding"; "Fast start (web streaming)" -> "Optimize for web playback (fast start)"; capitalise presets/profiles (`Medium`, `High`).

**M11. The dialog changes height by up to ~330 px when you switch presets** (902 px for Web 1080p, 760 for GIF, 574 for MP3; all vertically centred, so the header and Preset select move under the cursor). Fix: top-anchor the dialog (`align-items:flex-start;padding-top:8vh`) or give the body a `min-height`.

**M12. At 1366x768 the readiness verdict and Deliver to are below the fold** [10-export-1366]: body is 797 px of content in a 615 px viewport. Fix: put Frame rate and Quality on one row (two-column grid for the short selects), tighten row padding to 2px, or move the readiness verdict into the footer.

### Settings, Voiceover, Fit, Stems

**M13. Indeterminate and multi-stage progress is misleading.** With no known size the bar is a static 30% (`pct ?? 30` in `UpdateDialog.svelte:101`, `VoiceoverDialog.svelte:275`, `StemsDialog.svelte:180`, `SettingsDialog.svelte:338`); with stages the bar jumps to 100% and restarts at 0% ("Downloading voice runtime... 100%" then "Reading the script... 25%") [72/73-vo-running]. Only Stems' bar has `role=progressbar`. Fix: an `indeterminate` CSS animation (translating 30% segment) when `pct === null`; prefix the stage label with "Step 2 of 4"; add `role="progressbar"` + `aria-valuenow` to the Voiceover, Update, Export and Settings bars.

**M14. Cancel/Stop verbs disagree.** Export: "Stop" then "Stopping...". Stems/Voiceover/Settings: "Cancel" then "Stopping...". Both are red-outlined "destructive" buttons, although cancelling a synth is not destructive (stopping an export deletes a partial file, and only the status-bar tooltip says so). Fix: in dialogs "Cancel" -> "Cancelling...", keep "Stop"/"Stopping..." for Export and the status bar; Export's Stop gets the tooltip "Stop the export and delete the partial file"; use the secondary variant for the non-destructive cancels.

**M15. One action, many names.** Voiceover: "Voiceover..." (`TitlesControls.svelte:140`), "Write a voiceover..." (`AudioTab.svelte:84`), "Or write a voiceover" (`MediaBin.svelte:354`, no ellipsis), tooltip "Voiceover - speak a script onto the timeline" (`:378`), dialog title "Voiceover". Fit: menu "Fit to video...", dialog "Fit music to video", button "Fit music". Stems: "Separate stems..." / "Separate stems under this clip..." / title "Separate stems" / button "Separate". Fix: Voiceover everywhere "Write a voiceover..." with the dialog titled "Write a voiceover" and the button "Generate voiceover"; Fit: title and menu "Fit to video", button "Fit"; Stems: title "Separate stems", button "Separate stems".

**M16. Menu ellipsis and label audit** (rule: "..." only when a dialog or picker opens). Missing: File > `Settings` (`keymap.ts:350`), Help > `Keyboard shortcuts` (`menus.ts:306`; Edit > has the ellipsis for the same action), Window > `Reset all workspaces` (asks first, `menus.ts:298`), `Import files` (empty bin, picker) and `Browse`. Verb drift: "Release page" vs "Open the log folder" (and the status-bar says "Show in folder"). Fix: `Settings...`, `Keyboard shortcuts...`, `Reset all workspaces...`, `Import media...`, `Release notes`, `Show log folder`.

**M17. "About Kerf" is a four-second info toast** whose body is the invisible description (version "Kerf dev", license, URL not clickable). Where: `+page.svelte:311-315`. Fix: a small About dialog (name, version, build, license, link, "Copy version info").

**M18. Toasts don't carry meaning in colour.** Success/error/warning/info toasts are identical grey cards with a mono-white icon [103-toasts-sheet-dark]. The notification centre uses the same triangle glyph for warning and error (the toast uses circle-x). Fix: tint the icon and a 3 px left border with `--success` / `--danger` / `--warning`; use `x-circle` for errors in `NotificationCenter`.

**M19. Error toasts are raw and context-free (~35 call sites):** `toast.error(e instanceof Error ? e.message : String(e))` surfaces `invalid argument: this asset has no sound to separate`, `media engine error: ...`, `failed to install: permission denied`, UUID-bearing `clip not found: 7f3c...`, lower-case sentences. Mixed contractions: "Couldn't" (13 sites) vs "Could not" (6: `MediaBin.svelte:296`, `popout.svelte.ts:243`, `settings.svelte.ts:216,506`, `workspace.svelte.ts:194`, `AgentPanel.svelte:80`). Mixed separators: `Couldn't import X: msg` vs `Couldn't read the waveform of X - msg`. Fix: one `describeError(context, e)` in `ops.ts` (`errorMessage` is already the funnel for part of them): strip `invalid argument: `, `media engine error: `, `io error: `, capitalise, map `cancelled`; callers supply a lead-in ("Export failed", "Voiceover failed", "Stems failed"). Standardise on "Couldn't ..." and the em dash.

**M20. Update dialog content.** Release notes render as raw Markdown (`white-space:pre-wrap`, `:111`; the harness shows backticks), "Asking GitHub for the latest release..." duplicates the title, "Installed dev -> 0.99.0" reads as if 0.99.0 were installed, the ready-state warning says "unsaved changes". Fix: render notes with a tiny Markdown subset (headings, lists, links) or strip syntax; copy: row label `Current 0.18.0 -> 0.19.0 · 1 Oct 2026`; checking: "Checking for updates..."; current: "You're on the latest version."

**M21. First run / empty project.** There is no onboarding beyond the bin's dropzone, and the product's distinguishing feature (the agent) isn't mentioned until the user finds the collapsed "Connect an agent" at the bottom of the Agent tab [180-agent]. Specific copy problems [120-empty-1920-dark]: Timeline "Timeline empty - import media and queue a cut" ("queue a cut" is agent jargon, `Timeline.svelte:2483`); bin "Kerf analyzes it locally on import - you choose what in Settings" (what is the default? link?); button "Import files" vs the menu's "Import media..."; the title bar shows an amber "Unsaved" badge on an empty untitled project that has nothing to lose (`hasUnsavedWork` is false); the Preview says only "No media loaded". Fix: empty Preview becomes a 3-step card (1 Import media, 2 Cut on the timeline, 3 Export; plus "Or connect an agent over MCP"); copy below; show the Unsaved badge only when `editor.hasUnsavedWork`.

**M22. Explanatory copy in the 3.27:1 colour.** `--text-disabled` on `--surface-panel` is 3.27:1 (dark), and the guard only holds it to 3:1; but 12 px paragraphs that carry real information use it: Settings (`:229,235,282,300,343,367,392,419,466,502`), Export hints (`:534,538`), Keyboard hints (`:255,290,411`), Fit/Stems. Fix: swap those to `--text-muted` (4.8:1); keep `--text-disabled` for genuinely disabled controls.

**M23. Settings a11y.** Chip groups have no pressed state: CPU presets (`:184-189`), theme presets (`:476-483`), Dark/Light (`:497-500`), Thumb style (`:533-537`); the Voiceover chips (`:232-256`) as well (Fit and Deliver-to already use `aria-pressed`). The section rail has no `aria-current`/tab roles (`:153-166`). The Preview radio chips are `role=radio` but each is its own tab stop (no arrow-key roving). Fix: `aria-pressed={active}` in the `chip()` consumers, `aria-current="page"` on the rail, roving tabindex for the radiogroups.

**M24. Settings jargon.** "an all-intra copy ... a long GOP", "what an agent's analyze_asset runs" (a tool name in user copy, `:283`), "FFmpeg's JPEG", "Pixels across; 360 footage keeps its larger ratio." (a fragment pasted onto a hint). Rewrites in the copy deck.

**M25. Speech section.** The Speech-to-text half has no controls (it points at Analysis) while the transcription model picker lives in the Transcript tab; after the voice model downloads, "Without this it downloads the first time you generate one." stays [201-settings-speech-after], and during the download the status line still says "Not downloaded yet (~100 MB)" [200]. Fix: move the speech model picker/download here; hide the trailing sentence once `ready`; status line "Downloading..." while preparing; a check icon on "Voice model ready".

**M26. Appearance.** The toolbar wraps so "Export..." sits alone on a second row [42-settings-appearance-0]; the Dark/Light pair next to the theme name has no label; dozens of colour rows use opaque names (Void, Inset, Raised) with no preview. Fix: move Import.../Export... onto the theme-name row; label the pair "Base"; add a one-line description to each group.

**M27. VoiceoverDialog diverges from the theme and from its siblings.** Hard-coded `1px` borders instead of `var(--line-width)` (`:118, 124, 145, 148, 188, 259`; it breaks the "line thickness is a theme token" rule in `docs/frontend.md`); mixed time formats in one dialog ("At playhead (0.0s)" next to "about 0:13"); `Ctrl+Enter` to generate exists (`:182-187`) but is never shown; the textarea gets the browser's `auto` focus ring (see P2). Fix: tokens, `formatTime` for both, a hint under the script: "{shortcut} to generate" via `settings.shortcut`.

**M28. FitMusicDialog edge cases.** "0" or "abc" in Custom shows "Enter a length in seconds" (wrong for 0) in muted grey and the field isn't marked invalid [84, 88]; the checked-but-disabled "Fade out at the end - nothing runs over" looks half-broken [82]; "Target 0:20.5" next to "20.5 s" in the field. Fix: `parseTarget` returns a reason ("Enter a length greater than 0 seconds."), `aria-invalid` + danger border, accept `m:ss`, hide the fade row unless `overruns` (or show it unchecked with a one-line explanation).

**M29. Stems dialog copy is long and low-contrast.** Intro paragraph is `--text-muted` 12 px, followed by two grey boxes [91-stems-asset]. Fix: collapse into one sentence + one box: "Splits the sound into drums, bass, other and vocals on this computer using the Demucs model. Takes about as long as the audio, or longer. Speech goes to vocals, so it also separates dialogue from background sound." Then the box: "The four stems are added to the library as 'music.mp3 · drums' and so on. The timeline doesn't change." / "The first run downloads the separation model (~174 MB) and the audio runtime."

**M30. Success toasts for results that land out of sight have no action.** Voiceover's "Voiceover added and captioned" (`VoiceoverDialog.svelte:99`) has no Undo although Fit music (`FitMusicDialog.svelte:90`) and Stems-under-a-clip (`StemsDialog.svelte:62-65`) do; stems added to the library only (`Split music.mp3 into drums, bass, other and vocals - in the library`) have no way to get there. Fix: Voiceover toast gets `Undo`; the library-only stems toast gets `Show in library` (select the first stem and scroll the bin to it).

**M31. Shared `Modal.svelte`.** Dialog z-index (Update 60, others 50), widths (460/480/560/600/620), border tokens, close button, scrim policy and focus handling are copy-pasted six times (see Verdict). Extract one component with props `{title, icon, width, busy, dismissOnScrim, initialFocus}`.

---

## Polish (the little details)

**P1. Destination placeholder** is `Choose a file...` in the mono face with wide tracking [190-export-2x-top]. Use the sans face and `No file chosen yet`; show the path with `text-overflow:ellipsis` + `direction:rtl` trick or a middle-ellipsis so the file name stays visible.

**P2. Textarea focus ring is the browser default.** `layout.css:143` styles `button/summary/input/select:focus-visible` but not `textarea`, so the Voiceover script shows `outline: auto 1px` (near-white in dark, tan in light) instead of the 2 px kerf-400 ring everywhere else. Add `textarea:focus-visible` to the rule.

**P3. Units.** Frame-rate options are bare numbers (`60`, `59.94`): "60 fps". Bitrates are `192k` while the sample rate is `48 kHz`: "192 kbps". Target bitrate placeholder `e.g. 8M`: "e.g. 8M (8 Mbps)". Custom width/height inputs have no labels or units: `aria-label="Width"`/`"Height"` and a trailing "px".

**P4. Disclosure styles differ in one dialog:** native `<details>` marker for "Advanced encoding" and "1 tip", a hand-drawn `▸ Show ffmpeg command` button, and a mono "⚠" glyph for issues. Use one chevron Icon; write "FFmpeg" in the label.

**P5. Aspect fit row.** Label "Aspect fit" with options "Fit (letterbox)" / "Fill (crop)" reads "Fit: Fit". Rename the row "If the shape differs" with "Letterbox (keep the whole frame)" and "Crop to fill".

**P6. Dialogs appear instantly.** Add a 120 ms fade + 4 px rise on the panel and a fade on the scrim, disabled under `prefers-reduced-motion`.

**P7. Close "x" is a 12 px text glyph in a 32x30 ghost button.** Use the `x` Icon at 14 px; same hit area.

**P8. Initial focus and tab order.** `trapFocus` focuses the dialog root, so the first Tab lands on the close button, not the first field. Opt-in `data-autofocus` for: Export (Preset), Update (Install update), Fit (Custom or Fit button), Stems (Separate), Settings (active section).

**P9. Keyboard settings.** The `+` and reset icon buttons are 22 px (below the 24 px minimum); the recording chip doesn't echo held modifiers ("Ctrl+..."); the hint line is static while recording ("Press the keys you want. Esc cancels, Backspace removes it." would be live).

**P10. Update dialog.** The title icon is always "download", also on "up to date" and on errors (use check/alert-circle); the "Release page" button is on the left of a footer whose right side changes per phase, which moves the primary button; the date appears as `2026-10-01` (use `1 Oct 2026`).

**P11. Toast copy.** `Exported -> /home/orell/Videos/.../my-project-final-cut-v3-FINAL.mp4` wraps mid-path; `Saved -> path`, `Cover saved -> path` likewise. Say `Exported my-project-final-cut-v3.mp4` with the "Show in folder" action (multi: `Exported 3 files`). Caption import: `Imported 2 captions from 2 of 3 cues (1 outside the cut)` -> `Imported 2 of 3 cues. 1 fell outside the cut.`

**P12. Deliver-to hint wraps in the middle of a file name** (`cut-` / `1x1.mp4`) [22-export-deliver-two]. Wrap each name in `white-space:nowrap`. Copy: "Writes one file per shape next to the file above: cut-9x16.mp4, cut-1x1.mp4. Each shot is framed for its shape." Without a destination the examples say `cut-...`; use the project name.

**P13. Status bar run controls are 10 px** (`StatusBar.svelte:98-111`): the Stop button is ~18 px tall and the export line shows only "Exporting 42%", not "2/3" or the ETA the dialog has.

**P14. Case and register.** Sentence case is consistent app-wide (a strength). Exceptions to tidy: "Release page", "Fast start (web streaming)", "Deliver to" (ok), "Where it is going" (-> Platform check). Dialog titles mix noun ("Voiceover", "Export") and verb phrase ("Separate stems", "Fit music to video"); pick verb phrases: "Export", "Write a voiceover", "Separate stems", "Fit to video".

**P15. Time formats vary across the app:** `0:20.5` (Fit, Export range), `00:20:15` (Deliver length), `00:20` (status bar), `0:13` and `0.0s` (Voiceover), `2:00` (bin), `01:00.50` (Inspector). Route all through one `formatTime` and use `m:ss.f` for dialogs.

**P16. "Voice model ready" and similar success states** are plain primary text; add a green check. Update's "ready" state "The new version is installed. Restart Kerf to use it." is good copy and should be kept.

---

## Copy deck (exact replacements)

**Confirm prompts** (title / message / buttons)
| Where | Title | Message | Buttons |
|---|---|---|---|
| Close window (`+page.svelte:115`) | Close Kerf? | This project hasn't been saved. If you close now it will be lost. | Save... · Don't save · Cancel |
| Quit (`:296`) | Quit Kerf? | This project hasn't been saved. If you quit now it will be lost. | Save... · Don't save · Cancel |
| New / Open (`:235`) | Replace this project? | This project hasn't been saved. Starting another one will lose it. | Save... · Discard · Cancel |
| Restart (`UpdateDialog.svelte:24`) | Restart Kerf? | This project hasn't been saved. If you restart now it will be lost. | Restart anyway · Cancel |
| Reset all workspaces (`:305`) | Reset all workspaces? | Every workspace goes back to its default layout. Arrangements you made are lost. | Reset all · Cancel |
| Replace captions (`caption-import-ui.ts:251`) | Replace captions? | Replace the 2 captions on the cut with those in sample.srt? Titles you added by hand are kept. | Replace · Cancel |
| Stale apply (`AgentPanel.svelte:138`) | Apply stale changes? | You edited the timeline after these changes were staged. Applying them replaces your newer cut. | Apply and replace · Cancel |
Update's in-dialog banner: "This project hasn't been saved. Save it before restarting or it will be lost."

**Update dialog:** check failed -> title "Couldn't check for updates", body "Kerf couldn't reach the update server. Check your connection and try again." (+ Details). Checking -> "Checking for updates..." Current -> "You're on the latest version." Downloading footer -> "Downloading in the background." with a Hide button. Install error on Linux -> "In-place updates need the AppImage build. For a .deb or .rpm install, download the new version from the release page."

**Export presets** (label / description)
- Web 1080p / "H.264 MP4 at 1080p. Plays everywhere; the safe default."
- YouTube 4K (HEVC) / "H.265 at 4K. About half the size of H.264 at the same quality; needs a recent player."
- AV1 (smallest) / "AV1 in MKV at 1080p. Smallest files and slowest to encode; some devices can't play it."
- ProRes 422 HQ / "Edit-grade master in a .mov file. Very large."
- Small share (720p) / "720p H.264 for chat apps and email."
- Reels / Shorts (9:16) / "1080x1920 vertical H.264 at 30 fps. Fills the frame and crops the sides."
- Square (1:1) / "1080x1080 H.264 at 30 fps for feed posts. Fills the frame and crops."
- Target size (2-pass) / "H.264 at a fixed average bitrate (8 Mbps to start). Takes about twice as long."
- Animated GIF / "Looping GIF at 480p, 15 fps, no sound."
- MP3 audio (320 kbps) / "Just the mixed sound as an MP3."
- WAV audio (24-bit) / "Uncompressed 24-bit master of the mix."
Group with `<optgroup>`: Video · Social · Master · Audio.

**Settings**
- Preview proxies intro: "Kerf makes a lightweight copy of each video so scrubbing stays smooth, even with 4K or 5K footage. Exports always use your original files."
- Proxy size hint (replacing the "Pixels across;" fragment): "Width of the copy in pixels. 1280 is the default: sharp enough to judge framing, light enough to scrub. 360 footage keeps its wider ratio. Copies made at another size stay on disk until you delete them from the bin."
- Analysis footnote: "With the switch above off, nothing runs on import. These choices still decide what a connected agent analyzes when it doesn't ask for specific steps. You can always analyze a clip by hand."
- GPU preview: "Draws frames the GPU compositor can render exactly in a native surface instead of FFmpeg's JPEG. Anything it can't draw, such as a transition in progress or an unsupported effect, is still drawn by FFmpeg. Playback still uses FFmpeg. The status bar shows which renderer made the frame you're looking at."

**Menus:** `Settings...`, `Keyboard shortcuts...` (both places), `Reset all workspaces...`, `Import media...` (also the empty-bin button), `Release notes`, `Show log folder`, About Kerf opens a dialog.

**Empty state:** bin: "Drop media to start" / "Kerf analyzes new media on this computer. Choose what in Settings > Analysis." / [Import media...] / "or write a voiceover". Timeline: "Drop media here to start cutting." Preview card: "1 Import media · 2 Cut it on the timeline · 3 Export. Or connect an agent and let it assemble a first cut." Title-bar badge: hide "Unsaved" until `hasUnsavedWork`.

**Toasts:** `Exported {fileName}` + "Show in folder"; `Saved {fileName}`; `Imported 2 of 3 cues. 1 fell outside the cut.`; every `Could not ...` -> `Couldn't ...`; lead-ins `Export failed — {reason}`, `Voiceover failed — {reason}`, `Stem separation failed — {reason}`.

---

## Top 10 by impact per effort

1. **H3 Update dialog failure state** (title "up to date" over a network error, clipped raw text, AppImage note on every OS): ~15 lines, removes a flat-out wrong message.
2. **H2 GIF frame-rate select is blank**: add 15/12/10 to `FRAME_RATES` and fall back to showing the current value. One line of data.
3. **H1 Readiness panel gating and shape copy**: hide for audio-only, GIF and empty cuts; judge only ticked platforms; neutral line by default. Removes the amber warning from every default export.
4. **H4 Stop the scrim click discarding the script and all export options** (plus keep the last export options and a voiceover draft): the only real data-loss in the dialogs.
5. **H6 + P2 + M23 accessibility sweep**: `aria-label="Close"` and an X icon in four dialogs, `aria-labelledby` on Update, `textarea:focus-visible`, `aria-pressed` on chips, `aria-current` on the settings rail. A few lines each, together a big a11y jump.
6. **H7 + M18 toast and notification-log fixes**: record the description, fix the dark-theme description colour, tint success/error/warning. Makes the log actually useful for the failures it exists for.
7. **H8 confirm prompts**: explicit button labels (or three-button Save/Don't save/Cancel) and one wording for "never saved". Four call sites plus the copy table.
8. **H5 focus management**: `trapFocus` on Voiceover, focus the Cancel/Stop button when a run starts, restore focus to the menu bar after menu-launched dialogs. Small change in `modal.ts` plus three dialogs.
9. **M15 + M16 + H10 naming and ellipsis pass**: "Write a voiceover...", "Fit to video", `Settings...`, `Keyboard shortcuts...`, `Reset all workspaces...`, `Import media...`, "Open project." in the conflict prompt, `Couldn't`. Pure string edits across ~20 sites; makes the app read as one product.
10. **M22 + M13 legibility and progress**: replace `--text-disabled` with `--text-muted` on 20-odd explanatory paragraphs (3.27:1 -> 4.8:1), and animate indeterminate progress bars instead of a frozen 30%. Search-and-replace plus one CSS keyframe.

(Structural follow-up after these: M31, one shared `Modal.svelte`.)

Screenshots and the Playwright scripts used are in `/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/dialogs/` (e.g. `10-export-1920.png`, `20-export-preset-gif.png`, `51-kbd-conflict.png`, `63-update-check-error.png`, `103-toasts-sheet-dark.png`, `120-empty-1920-dark.png`).
