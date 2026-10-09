<script lang="ts">
	import { onMount, tick, untrack } from 'svelte';
	import { toast, notifications } from '$lib/notifications.svelte';
	import TitleBar from '$lib/components/editor/TitleBar.svelte';
	import Workspace from '$lib/components/editor/Workspace.svelte';
	import StatusBar from '$lib/components/editor/StatusBar.svelte';
	import ExportDialog from '$lib/components/editor/ExportDialog.svelte';
	import SettingsDialog from '$lib/components/editor/SettingsDialog.svelte';
	import UpdateDialog from '$lib/components/editor/UpdateDialog.svelte';
	import VoiceoverDialog from '$lib/components/editor/VoiceoverDialog.svelte';
	import ContextMenu from '$lib/components/editor/ContextMenu.svelte';
	import NotificationCenter from '$lib/components/editor/NotificationCenter.svelte';
	import Icon from '$lib/components/editor/Icon.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { agent } from '$lib/agent.svelte';
	import { updater } from '$lib/updater.svelte';
	import { settings } from '$lib/settings.svelte';
	import { mediaStatus } from '$lib/media-status.svelte';
	import { workspace } from '$lib/workspace.svelte';
	import { popout } from '$lib/popout.svelte';
	import { windows } from '$lib/windows.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import {
		cutSelection,
		deleteSelection,
		detachSelection,
		linkSelection,
		reattachSelection,
		trimSelection,
		unlinkSelection
	} from '$lib/ops';
	import { allowsRepeat, type ActionId } from '$lib/keymap';
	import { runMenuCommand } from '$lib/menu-commands';
	import { saveCoverFrame } from '$lib/file-actions';
	import { importCaptionFile } from '$lib/title-actions';
	import {
		inTauri,
		isMediaPath,
		confirmAction,
		onAnalysisStatus,
		onProxyProgress,
		onWindowCloseRequested,
		openReleases,
		quitApp,
		revealLogs,
		showMainWindow,
		takeLaunchProject
	} from '$lib/api';
	import { afterPaint, revealWindow } from '$lib/reveal';
	import { missingProjectMessage } from '$lib/launch';
	import type { AnalysisProgress, ModelProgress } from '$lib/types';

	/** Any modal on screen. The app behind it is `inert` and no editor shortcut
	 *  may fire: Space / Delete / J-K-L would edit the live project under it. */
	const modalOpen = $derived(ui.exportDialog || settings.open || updater.dialogOpen || ui.voiceoverDialog !== null);
	/** True while files are hovering over the window, for the drop overlay. */
	let dropHover = $state(false);

	// Re-read the proxy and analysis statuses whenever the set of assets changes (an import, an
	// agent's import or voiceover, a project opened): a new asset has no status until asked.
	$effect(() => {
		void editor.assets.map((a) => a.id).join(',');
		untrack(() => void mediaStatus.refresh());
	});

	// A modal over the editor window puts what is in the detached windows out of reach too.
	$effect(() => popout.setInert(modalOpen));

	// Any timeline edit mid-playback re-anchors the audio so what's heard
	// matches the new cut (volume/fade tweaks land live too).
	$effect(() => {
		void editor.timeline;
		untrack(() => ui.resync());
	});

	// The desktop window is created hidden, so the unthemed first frame is never
	// seen. Once the settings are in — which is when the theme is applied and the
	// dock is built — wait for the frame that draws them and show the window. (If
	// this never runs, the backend shows it itself after a few seconds.)
	let revealed = false;
	$effect(() => {
		if (!settings.loaded || revealed) return;
		revealed = true;
		void revealWindow({ settle: tick, paint: afterPaint, show: showMainWindow });
	});

	onMount(() => {
		const firstLoad = editor.load();
		void agent.load();
		void ui.loadFonts();
		void ui.loadTranscriptionStatus();
		void settings.load();
		// Ask GitHub whether a newer signed release exists (silently — offline is
		// not worth an interruption) and offer it in the title bar / dialog.
		const unlisteners: Array<() => void> = [];
		// The shortcuts and the menu-suppressing handler listen to every window the editor
		// is in: a key pressed in a detached panel's window is dispatched there and never
		// reaches a listener on this one.
		unlisteners.push(windows.listen('keydown', onKey), windows.listen('contextmenu', onContextMenu));
		const stopUpdater = updater.init();
		// A project that was never saved lives only in memory: closing the window
		// would drop it without a word, so ask first.
		void onWindowCloseRequested(
			() => editor.hasUnsavedWork,
			() => confirmAction('This project has never been saved. Close Kerf and lose it?', 'Close Kerf')
		).then((un) => unlisteners.push(un));

		// Each asset's preview proxy and per-kind analysis, kept current by the backend's events
		// (an agent's runs included) — the bin's badges and chips read them.
		void onProxyProgress((s) => mediaStatus.noteProxy(s)).then((un) => unlisteners.push(un));
		void onAnalysisStatus((s) => mediaStatus.noteAnalysis(s)).then((un) => unlisteners.push(un));

		// The desktop app hosts the MCP server, so an agent can edit the same
		// project live. It emits `project-changed` after each mutation; re-fetch
		// the timeline, history, and task queue so the GUI reflects agent edits.
		// Agent edits arrive in bursts (one event per mutation), so coalesce:
		// at most one refresh in flight plus one queued re-run, instead of piling
		// up a redundant full re-fetch per event.
		// It also emits `proxy-ready` once a background preview proxy finishes, so
		// the preview re-decodes the current frame from the faster proxy.
		let refreshing = false;
		let dirty = false;
		async function onProjectChanged() {
			if (refreshing) {
				dirty = true;
				return;
			}
			refreshing = true;
			try {
				do {
					dirty = false;
					await Promise.all([
						editor.refreshTimeline(),
						// An agent's voiceover is an asset the bin has not heard of.
						editor.refreshAssets(),
						editor.refreshHistory(),
						agent.load()
					]).catch(() => {});
				} while (dirty);
			} finally {
				refreshing = false;
			}
		}
		/** The flag changed behind our back (an agent set it): re-read it, and say so
		 *  — it is the user's own toolbar setting that moved. */
		async function onRippleChanged() {
			if (await editor.loadRippleMode()) {
				toast.info(`Ripple mode turned ${editor.rippleMode ? 'on' : 'off'} by the agent`);
			}
		}
		if (inTauri()) {
			// Files dropped onto the window import the same way the picker does —
			// which is what the media bin's "Drop media to start" has been
			// promising.
			void import('@tauri-apps/api/webview').then(async ({ getCurrentWebview }) => {
				unlisteners.push(
					await getCurrentWebview().onDragDropEvent((e) => {
						// A clip being dragged out of the media bin is an HTML5 drag
						// inside the webview, not files arriving from the OS; it must
						// not raise the import overlay over the lane it is aiming at.
						if (ui.dndAsset || modalOpen) return;
						if (e.payload.type === 'enter' || e.payload.type === 'over') dropHover = true;
						else if (e.payload.type === 'leave') dropHover = false;
						else if (e.payload.type === 'drop') {
							dropHover = false;
							void onDropPaths(e.payload.paths);
						}
					})
				);
			});
			void import('@tauri-apps/api/event').then(async ({ listen }) => {
				unlisteners.push(
					await listen('project-changed', () => void onProjectChanged()),
					await listen('proxy-ready', () => ui.refreshPreview()),
					// A second launch with a `.kerf` argument: the running app opens it.
					await listen<string>('open-project-file', (e) => void openProjectAt(e.payload)),
					// …or one naming a file that is not there, which is never created.
					await listen<string>('launch-project-missing', (e) => toast.error(missingProjectMessage(e.payload))),
					// An agent can pick the speech model over MCP; the status is
					// otherwise only read at launch, so the picker would keep
					// showing the previous model until the next start.
					await listen('speech-model-changed', () => void ui.loadTranscriptionStatus()),
					// Likewise the ripple flag: an agent that flips it changes what
					// the user's next trim or delete does, so the toolbar has to say.
					await listen('ripple-mode-changed', () => void onRippleChanged()),
					// Only a 360 lens pair reports here — its stitch is a full
					// re-encode, so the import overlay shows how far along it is.
					await listen<{ fraction: number }>(
						'import-progress',
						(e) => (editor.importProgress = e.payload.fraction)
					),
					// Analysis names its step as it goes — transcription in
					// particular downloads a model and then runs for minutes.
					await listen<AnalysisProgress>('analysis-progress', (e) =>
						ui.noteAnalysisProgress(e.payload)
					),
					await listen<ModelProgress>(
						'model-progress',
						(e) => (ui.modelFraction = e.payload.fraction ?? 0)
					)
				);
				// A `.kerf` on the command line of this very launch (a second launch's
				// arrives as the events above, unless it came while this page was still
				// starting, when it is waiting here instead). Asked for only now — with
				// the listeners up and the first load done, so the open is neither raced
				// by that load nor lost to a listener that did not exist yet — and it
				// goes through the same path as the event, unsaved-work question
				// included. A path that is not there is reported, never created.
				await firstLoad;
				const launched = await takeLaunchProject().catch(() => null);
				if (launched && 'open' in launched) await openProjectAt(launched.open);
				else if (launched) toast.error(missingProjectMessage(launched.missing));
			});
		}
		return () => {
			for (const un of unlisteners) un();
			stopUpdater();
		};
	});

	/** New / Open replace the project; one that was never saved would be gone. */
	async function okToReplace(): Promise<boolean> {
		return (
			!editor.hasUnsavedWork ||
			confirmAction('This project has never been saved and will be lost. Continue?', 'Unsaved project')
		);
	}

	async function onNew() {
		if (!inTauri()) {
			toast.info('Creating a project is available in the desktop app.');
			return;
		}
		if (!(await okToReplace())) return;
		try {
			if (await editor.newProject()) {
				await agent.load();
				toast.success('New project');
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	/** Open a project file — `path` when a launch handed one over, else the picker. */
	async function openProjectAt(path?: string) {
		if (!inTauri()) {
			toast.info('Opening a project file is available in the desktop app.');
			return;
		}
		if (!(await okToReplace())) return;
		try {
			if (await editor.openProject(path)) {
				await agent.load();
				toast.success(`Opened ${editor.projectName}`);
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	async function onSave() {
		if (!inTauri()) {
			toast.info('Saving a project file is available in the desktop app.');
			return;
		}
		try {
			if (await editor.saveProjectAs()) toast.success(`Saved → ${editor.currentPath}`);
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	function onExport() {
		ui.openExport();
	}

	/** File › Quit. The window's close button asks about unsaved work; so does this. */
	async function onQuit() {
		if (!inTauri()) {
			toast.info('Quitting is available in the desktop app.');
			return;
		}
		await quitApp(
			() => editor.hasUnsavedWork,
			() => confirmAction('This project has never been saved. Quit Kerf and lose it?', 'Quit Kerf')
		);
	}

	/** Window › Reset all workspaces throws away every arrangement made, so it asks first
	 *  — unless there is none to lose, when it only rebuilds the one on screen. */
	async function resetAllWorkspaces() {
		const w = settings.workspaces;
		const stored = Object.keys(w.layouts).length > 0 || Object.keys(w.library.tabs).length > 0;
		if (stored && !(await confirmAction('Reset every workspace to its default arrangement? The arrangements you made are lost.', 'Reset all workspaces'))) {
			return;
		}
		workspace.resetAll();
	}

	function onAbout() {
		toast.info(`Kerf ${updater.version || ''}`.trim(), {
			description: 'Non-destructive, AI-assisted video editing. PolyForm Noncommercial 1.0.0 — github.com/OrellBuehler/kerf'
		});
	}

	async function onImport() {
		if (!inTauri()) {
			toast.info('Importing media is available in the desktop app.');
			return;
		}
		await finishImport(editor.importMedia());
	}

	/** Files dropped onto the window. Non-media is filtered out here rather than
	 *  handed to ffprobe, so dropping a folder's worth of mixed files doesn't
	 *  answer with one error toast per README. */
	async function onDropPaths(paths: string[]) {
		if (!inTauri() || paths.length === 0) return;
		const media = paths.filter(isMediaPath);
		const rejected = paths.length - media.length;
		if (media.length === 0) {
			toast.error(paths.length === 1 ? "That isn't a media file Kerf can import." : 'No importable media in that drop.');
			return;
		}
		if (rejected > 0) toast.info(`Skipped ${rejected} non-media file${rejected === 1 ? '' : 's'}`);
		await finishImport(editor.importPaths(media));
	}

	/** Report what landed, then analyze it — one asset at a time, because each
	 *  pass is ffmpeg-bound, and stoppable, because the transcription at the end
	 *  of each one runs for minutes. */
	async function finishImport(job: ReturnType<typeof editor.importMedia>) {
		try {
			const { imported, failed } = await job;
			for (const f of failed) toast.error(`Couldn't import ${f.name}: ${f.message}`);
			if (imported.length === 0) return;
			toast.success(
				imported.length === 1 ? `Imported ${imported[0].name}` : `Imported ${imported.length} files`
			);
			await ui.analyzeImported(
				imported.map((a) => a.id),
				settings.autoAnalysis
			);
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	// A proposal the agent stages is only visible in the Agent panel, which
	// shares a tab group with the Inspector by default — so when one lands,
	// bring that panel forward once. Switching away again is left alone.
	let hadStaged = false;
	$effect(() => {
		const has = editor.staged !== null;
		if (workspace.open.length === 0) return;
		if (has && !hadStaged) workspace.show('agent');
		hadStaged = has;
	});

	/** Step by one timeline frame; Shift seeks a whole second. */
	function frameStep(coarse: boolean): number {
		return coarse ? 1 : 1 / editor.fps;
	}

	// Suppress the native browser context menu app-wide so views can supply their
	// own (Timeline, MediaBin, Preview each open one). Editable / selectable text
	// keeps the native menu so copy / paste / spell-check still work there.
	function onContextMenu(e: MouseEvent) {
		const t = e.target as Element | null;
		if (t?.closest('input, textarea, [contenteditable="true"], [data-selectable]')) return;
		e.preventDefault();
	}

	const clipErr = (err: unknown) => toast.error(err instanceof Error ? err.message : String(err));

	/** Delete / Shift+Delete: a selected title goes first (whichever of the two it
	 *  was — a title has no gap to close); else the selected clips as one edit,
	 *  with `ripple` closing the gaps behind them (plain Delete leaves them,
	 *  unless ripple mode is on). Nothing selected: not ours, so the key is left alone. */
	function deleteKey(ripple: boolean): void | false {
		if (editor.selectedOverlayId) {
			void editor
				.removeOverlay(editor.selectedOverlayId)
				.then(() => toast('Title removed', { action: { label: 'Undo', onClick: () => void editor.undo() } }))
				.catch(clipErr);
		} else if (editor.selectedClipIds.length > 0) {
			void deleteSelection(ripple);
		} else {
			return false;
		}
	}

	/** What each action does. Which key runs it is `settings.actionFor`'s business
	 *  (the registry in `keymap.ts`, with the user's changes on top); the type makes
	 *  an action without a handler a compile error. A handler returns `false` when
	 *  it did not take the key, so the browser still gets it. */
	const run: Record<ActionId, () => void | false> = {
		'file.new': () => void onNew(),
		'file.open': () => void openProjectAt(),
		'file.save': () => void onSave(),
		'file.import': () => void onImport(),
		'file.export': () => onExport(),
		'file.importCaptions': () => void importCaptionFile(),
		'file.saveCover': () => void saveCoverFrame(),
		'app.settings': () => settings.toggle(),
		'app.quit': () => void onQuit(),

		'edit.undo': () => {
			if (editor.canUndo) void editor.undo();
		},
		'edit.redo': () => {
			if (editor.canRedo) void editor.redo();
		},
		'edit.selectAll': () => editor.selectAll(),
		'edit.copy': () => {
			const n = editor.copySelection();
			if (n) toast(n === 1 ? 'Clip copied' : `${n} clips copied`);
		},
		'edit.cut': () => void cutSelection(),
		'edit.paste': () =>
			void editor
				.paste(ui.time)
				.then((n) => n && toast(n === 1 ? 'Clip pasted' : `${n} clips pasted`))
				.catch(clipErr),
		'edit.duplicate': () =>
			void editor
				.duplicateSelection()
				.then((n) => n && toast(n === 1 ? 'Clip duplicated' : `${n} clips duplicated`))
				.catch(clipErr),
		'edit.delete': () => deleteKey(false),
		'edit.rippleDelete': () => deleteKey(true),
		// Split-and-remove at the playhead; the backend follows ripple mode. Both say
		// why when there is nothing to cut, so neither returns `false`.
		'edit.trimStart': () => void trimSelection('left'),
		'edit.trimEnd': () => void trimSelection('right'),
		// Linked A/V: each says why when it cannot (nothing selected, nothing to detach), so
		// none returns `false`.
		'edit.detachAudio': () => void detachSelection(),
		'edit.reattachAudio': () => void reattachSelection(),
		'edit.link': () => void linkSelection(),
		'edit.unlink': () => void unlinkSelection(),
		'edit.clearSelection': () => {
			// Whatever else Escape is for gets it first: a menu or the notification
			// panel closing, a drag being abandoned (those stop the event; a dialog
			// never gets here, the page is inert behind it).
			if (!contextMenu.visible && !notifications.open) editor.clearSelection();
			// And it is never swallowed: it is how the browser backs out of things too.
			return false;
		},

		'tool.pointer': () => {
			ui.tool = 'pointer';
		},
		'tool.razor': () => {
			ui.tool = 'razor';
		},
		'tool.roll': () => {
			ui.tool = 'roll';
		},
		'tool.slip': () => {
			ui.tool = 'slip';
		},
		'tool.slide': () => {
			ui.tool = 'slide';
		},
		'tool.snap': () => {
			ui.snap = !ui.snap;
		},
		// A project setting: the registry marks it `repeat: false`, so a held key
		// does not flip it back and forth.
		'tool.rippleMode': () => void editor.setRippleMode(!editor.rippleMode).catch(clipErr),

		'playback.toggle': () => ui.togglePlay(),
		'playback.shuttleBack': () => ui.shuttle(-1),
		'playback.pause': () => ui.pause(),
		'playback.shuttleForward': () => ui.shuttle(1),
		'playback.stepBack': () => ui.seek(ui.time - frameStep(false)),
		'playback.stepForward': () => ui.seek(ui.time + frameStep(false)),
		'playback.jumpBack': () => ui.seek(ui.time - frameStep(true)),
		'playback.jumpForward': () => ui.seek(ui.time + frameStep(true)),
		'playback.toStart': () => ui.seek(0),
		'playback.toEnd': () => ui.seek(editor.duration),

		'marker.add': () =>
			void editor
				.addMarkerAtPlayhead(ui.time)
				.then(() => toast('Marker added', { action: { label: 'Undo', onClick: () => void editor.undo() } }))
				.catch(clipErr),
		'marker.prev': () => ui.gotoMarker(-1),
		'marker.next': () => ui.gotoMarker(1),
		// The in / out pair stays ordered so a mark can't cross its partner.
		'range.markIn': () => {
			ui.markIn = Math.min(ui.time, ui.markOut ?? Infinity);
		},
		'range.markOut': () => {
			ui.markOut = Math.max(ui.time, ui.markIn ?? 0);
		},
		'range.clearIn': () => {
			ui.markIn = null;
		},
		'range.clearOut': () => {
			ui.markOut = null;
		},

		'view.zoomIn': () => ui.zoomBy(1),
		'view.zoomOut': () => ui.zoomBy(-1),
		'view.zoomFit': () => ui.zoomToFit(),
		'view.minimap': () => ui.toggleMinimap(),
		'view.safeAreas': () => void settings.setSafeAreas(!settings.safeAreas),

		'workspace.edit': () => workspace.switchTo('edit'),
		'workspace.color': () => workspace.switchTo('color'),
		'workspace.audio': () => workspace.switchTo('audio'),
		'workspace.motion': () => workspace.switchTo('motion'),
		'workspace.deliver': () => workspace.switchTo('deliver'),
		'window.resetWorkspace': () => workspace.reset(),
		'window.resetAllWorkspaces': () => void resetAllWorkspaces(),
		'window.dockAll': () => popout.dockAll(),

		'app.keyboard': () => settings.openSection('keyboard'),
		'app.checkUpdate': () => updater.open(),
		'app.releases': () =>
			void openReleases().catch((e) => toast.error(`Couldn't open the release page — ${e instanceof Error ? e.message : String(e)}`)),
		'app.logs': () => {
			if (!inTauri()) {
				toast.info('The log folder is available in the desktop app.');
				return;
			}
			void revealLogs().catch(clipErr);
		},
		'app.about': () => onAbout()
	};

	/** A menu entry names an action; it runs what its key runs. Nothing is selected
	 *  by it that a modal covers: the bar is inert behind one. */
	function runAction(id: ActionId) {
		void run[id]();
	}

	function onKey(e: KeyboardEvent) {
		if (e.defaultPrevented || modalOpen) return;
		const target = e.target instanceof Element ? e.target : null;
		// Typing goes to the field. A range / checkbox only needs the keys that
		// operate it, so J/K/L still shuttle after a fader was clicked.
		if (target?.closest('input:not([type="range"]):not([type="checkbox"]):not([type="radio"]), textarea, select, [contenteditable="true"]')) return;
		const k = e.key.toLowerCase();
		const operates = k === ' ' || k === 'enter' || k.startsWith('arrow');
		if (operates && target?.closest('input, button, summary, [role="slider"], [role="tab"], [role="menuitem"]')) return;
		// The menu bar has its own keys; a letter typed in an open menu is a jump to an
		// entry, not the razor tool (it stops what it takes; this is for what it leaves).
		if (target?.closest('[role="menubar"], [role="menu"]') && !(e.ctrlKey || e.metaKey || e.altKey)) return;

		// A key that is bound to nothing — or to a combination that is not exactly
		// this one — does nothing, and in particular never falls through to a
		// shorter chord (⌘C is not the razor's bare C).
		const id = settings.actionFor(e);
		if (!id) return;
		// One press, one action: a held ⌘V must not paste a dozen copies. The repeat
		// is consumed, so the browser does not scroll or click on it either.
		if (e.repeat && !allowsRepeat(id)) {
			e.preventDefault();
			return;
		}
		if (run[id]() !== false) e.preventDefault();
	}
</script>

<div
	inert={modalOpen}
	style="position:fixed;inset:0;display:flex;flex-direction:column;background:var(--surface-void)"
>
	<TitleBar onAction={runAction} onCommand={runMenuCommand} />
	<!-- While a proposal is on screen the editor is showing a cut that is not
	     the project's yet; say so where it cannot be missed. -->
	{#if editor.previewingStaged}
		<div
			style="flex:none;display:flex;align-items:center;gap:9px;height:30px;padding:0 12px;background:var(--agent-surface);border-bottom:var(--line-width) solid var(--agent-border);color:var(--agent-300);font-size:12px"
		>
			<span style="width:7px;height:7px;border-radius:50%;background:var(--agent-400);box-shadow:0 0 8px var(--agent-400)"
			></span>
			<span>Previewing the agent's proposed cut — your timeline is unchanged.</span>
			<div style="flex:1"></div>
			<button
				onclick={() => editor.exitStagedPreview()}
				style="height:22px;padding:0 9px;border-radius:var(--radius-full);border:var(--line-width) solid var(--agent-border);background:var(--surface-raised);color:var(--text-secondary);font-size:11px;cursor:pointer"
				>Exit preview</button
			>
		</div>
	{/if}
	<!-- Something the project itself could not do — a `.kerf` that would not
	     open, an edit the backend refused. It used to be recorded in
	     `editor.error` and never shown, so a corrupt file opened as silence. -->
	{#if editor.error}
		<div
			style="flex:none;display:flex;align-items:center;gap:9px;min-height:30px;padding:5px 12px;background:color-mix(in srgb,var(--danger) 14%,transparent);border-bottom:var(--line-width) solid color-mix(in srgb,var(--danger) 40%,transparent);color:var(--text-primary);font-size:12px"
		>
			<Icon n="alert-triangle" s={13} color="var(--danger)" />
			<span style="flex:1;min-width:0">{editor.error}</span>
			<button
				onclick={() => (editor.error = null)}
				style="height:22px;padding:0 9px;border-radius:var(--radius-full);border:var(--line-width) solid var(--border-strong);background:var(--surface-raised);color:var(--text-secondary);font-size:11px;cursor:pointer"
				>Dismiss</button
			>
		</div>
	{/if}
	<!-- The dock waits for the settings so it can restore the saved arrangement
	     rather than build the default and then rebuild. -->
	{#if settings.loaded}
		<Workspace />
	{:else}
		<div style="flex:1"></div>
	{/if}
	<StatusBar />
</div>

<!-- Files are over the window and about to be dropped. -->
{#if dropHover}
	<div
		style="position:fixed;inset:0;z-index:60;display:grid;place-items:center;background:var(--surface-overlay);pointer-events:none"
	>
		<div
			style="display:flex;flex-direction:column;align-items:center;gap:10px;padding:26px 40px;border-radius:var(--radius-md);border:1.5px dashed var(--kerf-400);background:var(--surface-panel);color:var(--text-primary)"
		>
			<Icon n="film" s={24} color="var(--kerf-400)" />
			<span style="font:var(--type-ui)">Drop to import</span>
		</div>
	</div>
{/if}

{#if ui.exportDialog}
	<ExportDialog onClose={() => ui.closeExport()} />
{/if}

{#if settings.open}
	<SettingsDialog onClose={() => settings.close()} />
{/if}

{#if ui.voiceoverDialog}
	<VoiceoverDialog prefill={ui.voiceoverDialog.prefill} onClose={() => ui.closeVoiceover()} />
{/if}

{#if updater.dialogOpen}
	<UpdateDialog />
{/if}

<ContextMenu />
<NotificationCenter />
