<script lang="ts">
	import { onMount, tick, untrack } from 'svelte';
	import { toast, notifications } from '$lib/notifications.svelte';
	import TitleBar from '$lib/components/editor/TitleBar.svelte';
	import Workspace from '$lib/components/editor/Workspace.svelte';
	import StatusBar from '$lib/components/editor/StatusBar.svelte';
	import ExportDialog from '$lib/components/editor/ExportDialog.svelte';
	import SettingsDialog from '$lib/components/editor/SettingsDialog.svelte';
	import UpdateDialog from '$lib/components/editor/UpdateDialog.svelte';
	import AboutDialog from '$lib/components/editor/AboutDialog.svelte';
	import VoiceoverDialog from '$lib/components/editor/VoiceoverDialog.svelte';
	import FitMusicDialog from '$lib/components/editor/FitMusicDialog.svelte';
	import StemsDialog from '$lib/components/editor/StemsDialog.svelte';
	import ShortcutSheet from '$lib/components/editor/ShortcutSheet.svelte';
	import RelinkDialog from '$lib/components/editor/RelinkDialog.svelte';
	import ContextMenu from '$lib/components/editor/ContextMenu.svelte';
	import NotificationCenter from '$lib/components/editor/NotificationCenter.svelte';
	import Icon from '$lib/components/editor/Icon.svelte';
	import Btn from '$lib/components/editor/Btn.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor, EditRefused, PreviewReadOnly } from '$lib/state.svelte';
	import { agent } from '$lib/agent.svelte';
	import { applyProposal, discardProposal } from '$lib/proposal-actions';
	import { changeWord, reviewStepTip } from '$lib/proposal-ui';
	import { updater } from '$lib/updater.svelte';
	import { settings } from '$lib/settings.svelte';
	import { mediaStatus } from '$lib/media-status.svelte';
	import { workspace } from '$lib/workspace.svelte';
	import { resetAllWorkspacesAsked } from '$lib/workspace-actions';
	import { popout } from '$lib/popout.svelte';
	import { windows } from '$lib/windows.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import {
		cutSelection,
		deleteSelection,
		addMarker,
		detachSelection,
		duplicateClips,
		holdForPreview,
		linkSelection,
		keyEdit,
		pasteClips,
		reattachSelection,
		report,
		splitSelection,
		trimSelection,
		unlinkSelection
	} from '$lib/ops';
	import { allowsRepeat, type ActionId } from '$lib/keymap';
	import { runMenuCommand } from '$lib/menu-commands';
	import type { MenuCommand } from '$lib/menus';
	import { saveCoverFrame, saveProject } from '$lib/file-actions';
	import { importCaptionFile } from '$lib/title-actions';
	import { checkMissingOnOpen } from '$lib/library-actions';
	import {
		askUnsaved,
		inTauri,
		isMediaPath,
		onAnalysisProgress,
		onAnalysisStatus,
		onImportProgress,
		onModelProgress,
		onProxyProgress,
		onSmartCropProgress,
		onWindowCloseRequested,
		openReleases,
		quitApp,
		revealLogs,
		setWindowTitle,
		showMainWindow,
		takeLaunchProject
	} from '$lib/api';
	import { windowTitle } from '$lib/window-title';
	import { finishImport, importMedia } from '$lib/import-media';
	import { clipsIn, exportButtonState } from '$lib/export-button';
	import { afterPaint, fontsReady, revealWindow } from '$lib/reveal';
	import { missingProjectMessage } from '$lib/launch';
	import { tip } from '$lib/tip';
	import { UNSAVED, proceedAfter, type UnsavedCopy } from '$lib/confirm-copy';
	import { changeIndex, changePoints, stepChange } from '$lib/staged-diff';
	import { clampPlayhead, secondFrames, stepFrames } from '$lib/frames';
	import type { SettingsView } from '$lib/types';
	import { isPagingKey, onEditorSurface } from '$lib/focus-area';

	/** Any modal on screen. The app behind it is `inert` and no editor shortcut
	 *  may fire: Space / Delete / J-K-L would edit the live project under it. */
	const modalOpen = $derived(
		ui.exportDialog ||
			settings.open ||
			updater.dialogOpen ||
			ui.voiceoverDialog !== null ||
			ui.aboutDialog ||
			ui.fitMusicDialog !== null ||
			ui.relinkDialog ||
			ui.stemsDialog !== null ||
			ui.shortcutSheet
	);
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

	// The OS hears the project's name, and that it has work that is not saved.
	const title = $derived(windowTitle(editor.projectName, editor.hasUnsavedWork));
	$effect(() => {
		void setWindowTitle(title).catch(() => {});
	});

	// Any timeline edit mid-playback re-anchors the audio so what's heard
	// matches the new cut (volume/fade tweaks land live too).
	$effect(() => {
		void editor.timeline;
		untrack(() => ui.resync());
	});

	// The playhead cannot stand past the end of the cut. An edit that shortens it (a delete, an undo, an
	// agent's trim, a proposal on screen) takes it back to the new end rather than leaving it parked in
	// nothing, reading a time the cut does not have.
	$effect(() => {
		const end = editor.duration;
		untrack(() => {
			const back = clampPlayhead(ui.time, end);
			if (back !== null) ui.seek(back);
		});
	});

	// The desktop window is created hidden, so the unthemed first frame is never
	// seen. Once the settings are in — which is when the theme is applied and the
	// dock is built — wait for the interface's fonts and the frame that draws them, then show the
	// window, so its first frame is not the system fallback re-flowing. (If
	// this never runs, the backend shows it itself after a few seconds.)
	let revealed = false;
	$effect(() => {
		if (!settings.loaded || revealed) return;
		revealed = true;
		void revealWindow({ settle: tick, fonts: () => fontsReady(), paint: afterPaint, show: showMainWindow });
	});

	onMount(() => {
		const firstLoad = editor.load();
		// A project that comes up with media whose file is gone offers to relink it.
		void firstLoad.then(() => checkMissingOnOpen());
		void agent.load();
		void ui.loadFonts();
		void ui.loadTranscriptionStatus();
		void settings.load();
		// Ask GitHub whether a newer signed release exists (silently — offline is
		// not worth an interruption) and offer it in the title bar / dialog.
		const unlisteners: Array<() => void> = [];
		// An edit the backend (or the preview) refused is reported by whoever asked for it — a toast. One that
		// nobody caught (`void editor.undo()` from a toast's button) is said here, so a refusal is never silent
		// and never said twice.
		const onRefusal = (ev: PromiseRejectionEvent) => {
			if (!(ev.reason instanceof EditRefused)) return;
			ev.preventDefault();
			// Previewing a proposal is a state, not a failure.
			if (ev.reason instanceof PreviewReadOnly) toast.info(ev.reason.message);
			else toast.error(ev.reason.message);
		};
		window.addEventListener('unhandledrejection', onRefusal);
		unlisteners.push(() => window.removeEventListener('unhandledrejection', onRefusal));
		// The shortcuts and the menu-suppressing handler listen to every window the editor
		// is in: a key pressed in a detached panel's window is dispatched there and never
		// reaches a listener on this one.
		unlisteners.push(windows.listen('keydown', onKey), windows.listen('contextmenu', onContextMenu));
		const stopUpdater = updater.init();
		// A project that was never saved lives only in memory: closing the window
		// would drop it without a word, so ask first.
		void onWindowCloseRequested(
			() => editor.hasUnsavedWork,
			() => proceedUnsaved(UNSAVED.close)
		).then((un) => unlisteners.push(un));

		// Each asset's preview proxy and per-kind analysis, kept current by the backend's events
		// (an agent's runs included) — the bin's badges and chips read them.
		void onProxyProgress((s) => mediaStatus.noteProxy(s)).then((un) => unlisteners.push(un));
		void onAnalysisStatus((s) => mediaStatus.noteAnalysis(s)).then((un) => unlisteners.push(un));
		// Only a 360 lens pair reports an import's progress — its stitch is a full re-encode, so the import
		// overlay shows how far along it is (and Stop). A speech model's download reports its bytes; a smart
		// crop reports the shots sampled so far.
		void onImportProgress((p) => (editor.importProgress = p.fraction)).then((un) => unlisteners.push(un));
		void onModelProgress((p) => ui.noteModelProgress(p)).then((un) => unlisteners.push(un));
		void onSmartCropProgress((p) => (editor.smartCropProgress = p.fraction)).then((un) => unlisteners.push(un));
		// Analysis names its step as it goes — transcription in particular downloads a model and then runs for
		// minutes. (The browser harness simulates the ticks.)
		void onAnalysisProgress((p) => ui.noteAnalysisProgress(p)).then((un) => unlisteners.push(un));

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
		/** Another project is open now (an agent swapped it): forget the selection and what the session
		 *  remembered about the last one, and load the new project as an open does. */
		async function onProjectReplaced() {
			editor.selectedAssetId = null;
			editor.selectedClipId = null;
			editor.selectedClipIds = [];
			editor.selectedOverlayId = null;
			editor.selectedSvgOverlayId = null;
			await editor.load();
			ui.projectReplaced();
			await agent.load();
			toast.info(`The agent switched to ${editor.projectName}`);
			void checkMissingOnOpen();
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
					// The GPU preview's first-use self-test has its verdict: the frame that waited for it
					// went through FFmpeg, so ask for it again.
					await listen('gpu-preview-checked', () => ui.refreshPreview()),
					// A second launch with a `.kerf` argument: the running app opens it.
					await listen<string>('open-project-file', (e) => void openProjectAt(e.payload)),
					// …or one naming a file that is not there, which is never created.
					await listen<string>('launch-project-missing', (e) => toast.error(missingProjectMessage(e.payload))),
					// An agent can pick the speech model over MCP; the status is
					// otherwise only read at launch, so the picker would keep
					// showing the previous model until the next start.
					await listen('speech-model-changed', () => void ui.loadTranscriptionStatus()),
					// An agent opened, started or saved-as a project through MCP: the same swap the
					// menu does, so reload everything it showed (the backend already refused to drop
					// unsaved work unless the agent was told to).
					await listen('project-replaced', () => void onProjectReplaced()),
					// An agent changed a setting (MCP `set_settings`): the dialog and everything backed by one follows.
					await listen<SettingsView | null>('settings-changed', (e) => settings.noteChanged(e.payload)),
					// Likewise the ripple flag: an agent that flips it changes what
					// the user's next trim or delete does, so the toolbar has to say.
					await listen('ripple-mode-changed', () => void onRippleChanged())
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

	/** The export no longer freezes the editor, but it renders *this* project: replacing it under the render would
	 *  have the export read a project that is not the one it was started on. Said, not silently ignored. */
	function renderRunning(): boolean {
		if (editor.exportRun === null) return false;
		toast.info('An export is running — stop it from the status bar before opening or starting another project.');
		return true;
	}

	/** New / Open replace the project; one that was never saved would be gone. */
	async function okToReplace(): Promise<boolean> {
		return !editor.hasUnsavedWork || proceedUnsaved(UNSAVED.replace);
	}

	/** The unsaved-project prompt and what its answer means: whether to go ahead. "Save…" opens
	 *  the save dialog and goes ahead only once the project is on disk. */
	async function proceedUnsaved(copy: UnsavedCopy): Promise<boolean> {
		return proceedAfter(await askUnsaved(copy), saveProject);
	}

	async function onNew() {
		if (!inTauri()) {
			toast.info('Creating a project is available in the desktop app.');
			return;
		}
		if (renderRunning()) return;
		if (!(await okToReplace())) return;
		try {
			if (await editor.newProject()) {
				ui.projectReplaced();
				await agent.load();
				toast.success('New project');
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	/** Open a project file — `path` when a launch handed one over, else the picker; `recent` when it is one of
	 *  File › Open Recent's (a file that has gone is then dropped from the list and said so). */
	async function openProjectAt(path?: string, recent = false) {
		if (!inTauri()) {
			toast.info('Opening a project file is available in the desktop app.');
			return;
		}
		if (renderRunning()) return;
		if (!(await okToReplace())) return;
		try {
			if (await (recent && path ? editor.openRecent(path) : editor.openProject(path))) {
				ui.projectReplaced();
				await agent.load();
				toast.success(`Opened ${editor.projectName}`);
				void checkMissingOnOpen();
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		} finally {
			// Opening puts the project at the top of the recent list; a file that has gone drops off it.
			void settings.refreshRecent();
		}
	}

	/** File › Open Recent's entries, and the one that clears the list; everything else is `runMenuCommand`'s. */
	function onMenuCommand(c: MenuCommand) {
		if (c.type === 'recent') void openProjectAt(c.path, true);
		else if (c.type === 'recent-clear') void settings.clearRecentAsked();
		else runMenuCommand(c);
	}

	async function onSave() {
		if (!inTauri()) {
			toast.info('Saving a project file is available in the desktop app.');
			return;
		}
		await saveProject();
	}

	/** File › Export… and its key: the title bar's button's rule (`exportButtonState`) — an empty cut says why
	 *  instead of opening a dialog with nothing to render, and a render under way reopens its dialog. */
	function onExport() {
		const state = exportButtonState({ clips: clipsIn(editor.liveTimeline), running: editor.exportRun !== null });
		if (state.disabled) {
			toast.info(state.reason ?? 'Nothing to export yet');
			return;
		}
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
			() => proceedUnsaved(UNSAVED.quit)
		);
	}

	/** Window › Reset all workspaces throws away every arrangement made, so it asks first
	 *  — unless there is none to lose, when it only rebuilds the one on screen. */
	function resetAllWorkspaces() {
		return resetAllWorkspacesAsked();
	}

	function onAbout() {
		ui.openAbout();
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

	/** The times the proposal on screen changes the cut at, for stepping through them. */
	const reviewPoints = $derived(editor.previewingStaged && editor.staged ? changePoints(editor.staged.diff.entries) : []);

	/** Apply or Discard from the banner: held while one is under way, so a double click is one answer. */
	let reviewBusy = $state(false);
	async function reviewAct(answer: () => Promise<boolean>) {
		reviewBusy = true;
		try {
			await answer();
		} finally {
			reviewBusy = false;
		}
	}

	/** `[` / `]`: the playhead to the change before / after it, while a proposal is on the timeline. */
	function stepReview(dir: 1 | -1): void | false {
		if (!editor.previewingStaged) return false;
		const at = stepChange(reviewPoints, ui.time, dir, editor.fps, editor.duration);
		if (at === null) toast.info(dir > 0 ? 'That is the last change in the proposal' : 'That is the first change in the proposal');
		else ui.seek(at);
	}

	/** Step by frames of the cut from the frame being shown (a paused playhead can be mid-frame). */
	function step(frames: number) {
		ui.seek(stepFrames(ui.time, frames, editor.fps, editor.duration));
	}

	// Suppress the native browser context menu app-wide so views can supply their
	// own (Timeline, MediaBin, Preview each open one). Editable / selectable text
	// keeps the native menu so copy / paste / spell-check still work there.
	function onContextMenu(e: MouseEvent) {
		const t = e.target as Element | null;
		if (t?.closest('input, textarea, [contenteditable="true"], [data-selectable]')) return;
		e.preventDefault();
	}

	/** Delete / Shift+Delete: a selected title or animated title goes first (whichever of the two it
	 *  was — a title has no gap to close); else the selected clips as one edit,
	 *  with `ripple` closing the gaps behind them (plain Delete leaves them,
	 *  unless ripple mode is on). Nothing selected: not ours, so the key is left alone. */
	function deleteKey(ripple: boolean): void | false {
		if (editor.selectedOverlayId) {
			if (holdForPreview()) return;
			void editor
				.removeOverlay(editor.selectedOverlayId)
				.then(() => toast('Title removed', { action: { label: 'Undo', onClick: () => void editor.undo() } }))
				.catch(report);
		} else if (editor.selectedSvgOverlayId) {
			if (holdForPreview()) return;
			void editor
				.removeSvgOverlay(editor.selectedSvgOverlayId)
				.then(() => toast('Animated title removed', { action: { label: 'Undo', onClick: () => void editor.undo() } }))
				.catch(report);
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
		'file.import': () => void importMedia(),
		'file.export': () => onExport(),
		'file.importCaptions': () => void importCaptionFile(),
		'file.saveCover': () => void saveCoverFrame(),
		'app.settings': () => settings.toggle(),
		'app.quit': () => void onQuit(),

		'edit.undo': () => {
			if (holdForPreview()) return;
			if (editor.canUndo) void editor.undo();
		},
		'edit.redo': () => {
			if (holdForPreview()) return;
			if (editor.canRedo) void editor.redo();
		},
		'edit.selectAll': () => editor.selectAll(),
		'edit.copy': () => {
			const n = editor.copySelection();
			if (n) toast(n === 1 ? 'Clip copied' : `${n} clips copied`);
		},
		'edit.cut': () => void cutSelection(),
		'edit.paste': () => void pasteClips(),
		'edit.duplicate': () => void duplicateClips(),
		'edit.split': () => void splitSelection(),
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
		// One frame a press (the registry marks them `repeat: false`): each is one edit, and a held key would
		// write a revision per auto-repeat.
		'edit.rollBack': () => void keyEdit('roll', -1),
		'edit.rollForward': () => void keyEdit('roll', 1),
		'edit.slipBack': () => void keyEdit('slip', -1),
		'edit.slipForward': () => void keyEdit('slip', 1),
		'edit.slideBack': () => void keyEdit('slide', -1),
		'edit.slideForward': () => void keyEdit('slide', 1),
		'tool.snap': () => {
			ui.snap = !ui.snap;
		},
		// A project setting: the registry marks it `repeat: false`, so a held key
		// does not flip it back and forth.
		'tool.rippleMode': () => void editor.setRippleMode(!editor.rippleMode).catch(report),

		'playback.toggle': () => ui.togglePlay(),
		'playback.shuttleBack': () => ui.shuttle(-1),
		'playback.pause': () => ui.pause(),
		'playback.shuttleForward': () => ui.shuttle(1),
		'playback.stepBack': () => step(-1),
		'playback.stepForward': () => step(1),
		'playback.jumpBack': () => step(-secondFrames(editor.fps)),
		'playback.jumpForward': () => step(secondFrames(editor.fps)),
		'playback.toStart': () => ui.seek(0),
		'playback.toEnd': () => ui.seek(editor.duration),

		'marker.add': () => void addMarker(),
		'marker.prev': () => ui.gotoMarker(-1),
		'marker.next': () => ui.gotoMarker(1),
		'review.prevChange': () => stepReview(-1),
		'review.nextChange': () => stepReview(1),
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

		'app.shortcuts': () => ui.openShortcuts(),
		'app.keyboard': () => settings.openSection('keyboard'),
		'app.checkUpdate': () => updater.open(),
		'app.releases': () =>
			void openReleases().catch((e) => toast.error(`Couldn't open the release page — ${e instanceof Error ? e.message : String(e)}`)),
		'app.logs': () => {
			if (!inTauri()) {
				toast.info('The log folder is available in the desktop app.');
				return;
			}
			void revealLogs().catch(report);
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
		// Page Up / Page Down scroll whichever panel has the keyboard; a proposal's review takes them only
		// from the editor surface (the timeline, the preview, nothing in particular).
		if (id.startsWith('review.') && isPagingKey(e.key) && !onEditorSurface(target)) return;
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
	<TitleBar onAction={runAction} onCommand={onMenuCommand} />
	<!-- While a proposal is on screen the editor is showing a cut that is not
	     the project's yet; say so where it cannot be missed, and keep its three answers — apply, discard, go
	     back — in reach, so the review does not need the Agent panel open. The timeline is read-only here. -->
	{#if editor.previewingStaged}
		<div
			role="status"
			style="flex:none;display:flex;align-items:center;gap:9px;min-height:32px;padding:3px 12px;background:var(--agent-surface);border-bottom:var(--line-width) solid var(--agent-border);color:var(--agent-300);font-size:12px;overflow:hidden"
		>
			<span style="flex:none;width:7px;height:7px;border-radius:50%;background:var(--agent-400);box-shadow:0 0 8px var(--agent-400)"
			></span>
			<span style="flex:none;white-space:nowrap" use:tip={'The agent’s proposal is on the timeline and the cut is read-only. Nothing is changed until you apply it.'}
				>Previewing the agent's proposed cut — your timeline is unchanged.</span
			>
			{#if editor.staged}
				<!-- The proposal as marks on the timeline: stepping from change to change, and what the
				     marks mean (colour and outline style both say it). -->
				<span
					data-review-nav
					style="display:inline-flex;align-items:center;gap:4px;margin-left:6px;color:var(--text-secondary);font-size:11px;white-space:nowrap"
				>
					<button
						use:tip={settings.tip('Previous change', 'review.prevChange')}
						aria-label="Previous change"
						disabled={reviewPoints.length === 0}
						onclick={() => void stepReview(-1)}
						style="display:grid;place-items:center;width:22px;height:22px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--agent-border);background:var(--surface-raised);color:var(--text-secondary);cursor:pointer"
						><Icon n="chevron-left" s={12} /></button
					>
					<span
						style="font-family:var(--font-mono);min-width:12ch;text-align:center"
						aria-live="polite"
						use:tip={reviewStepTip(reviewPoints.length, editor.staged.diff.entries.length, settings.keysFor('review.prevChange'), settings.keysFor('review.nextChange'))}
						>{reviewPoints.length > 0 ? `place ${changeIndex(reviewPoints, ui.time, editor.fps, editor.duration)} of ${reviewPoints.length}` : 'no places'}</span
					>
					<button
						use:tip={settings.tip('Next change', 'review.nextChange')}
						aria-label="Next change"
						disabled={reviewPoints.length === 0}
						onclick={() => void stepReview(1)}
						style="display:grid;place-items:center;width:22px;height:22px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--agent-border);background:var(--surface-raised);color:var(--text-secondary);cursor:pointer"
						><Icon n="chevron-right" s={12} /></button
					>
					<span style="margin-left:4px;font-family:var(--font-mono)">· {changeWord(editor.staged.diff.entries.length)} in all</span>
				</span>
				<span data-review-legend style="display:inline-flex;align-items:center;gap:10px;margin-left:8px;font-size:11px;color:var(--text-secondary);white-space:nowrap">
					<span style="display:inline-flex;align-items:center;gap:4px"
						><span style="width:12px;height:9px;border:2px solid var(--diff-add);border-radius:2px"></span>added</span
					>
					<span style="display:inline-flex;align-items:center;gap:4px"
						><span style="width:12px;height:9px;border:2px dashed var(--diff-shift);border-radius:2px"></span>changed</span
					>
					<span style="display:inline-flex;align-items:center;gap:4px"
						><span
							style="width:12px;height:9px;border:1.5px dashed var(--diff-remove);border-radius:2px;background:repeating-linear-gradient(135deg,color-mix(in srgb,var(--diff-remove) 55%,transparent) 0 1px,transparent 1px 4px)"
						></span>removed</span
					>
				</span>
			{/if}
			<div style="flex:1"></div>
			{#if editor.staged}
				<Btn variant="primary" size="sm" disabled={reviewBusy} onclick={() => void reviewAct(applyProposal)}>Apply</Btn>
				<Btn size="sm" disabled={reviewBusy} onclick={() => void reviewAct(discardProposal)}>Discard</Btn>
			{/if}
			<Btn size="sm" onclick={() => void editor.exitStagedPreview()}>Exit preview</Btn>
		</div>
	{/if}
	<!-- Something the project itself could not do — a `.kerf` that would not
	     open, an edit the backend refused. It used to be recorded in
	     `editor.error` and never shown, so a corrupt file opened as silence. -->
	{#if editor.error}
		<div
			role="alert"
			style="flex:none;display:flex;align-items:center;gap:9px;min-height:30px;padding:5px 12px;background:color-mix(in srgb,var(--danger) 14%,transparent);border-bottom:var(--line-width) solid color-mix(in srgb,var(--danger) 40%,transparent);color:var(--text-primary);font-size:12px"
		>
			<Icon n="alert-triangle" s={13} color="var(--danger)" />
			<span style="flex:1;min-width:0">{editor.error}</span>
			<button
				onclick={() => (editor.error = null)}
				style="height:24px;padding:0 9px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--border-strong);background:var(--surface-raised);color:var(--text-secondary);font-size:11px;cursor:pointer"
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

{#if ui.fitMusicDialog}
	<FitMusicDialog clipId={ui.fitMusicDialog.clipId} onClose={() => ui.closeFitMusic()} />
{/if}

{#if ui.stemsDialog}
	<StemsDialog assetId={ui.stemsDialog.assetId} clipId={ui.stemsDialog.clipId} onClose={() => ui.closeStems()} />
{/if}

{#if updater.dialogOpen}
	<UpdateDialog />
{/if}

{#if ui.aboutDialog}
	<AboutDialog onClose={() => ui.closeAbout()} />
{/if}

{#if ui.shortcutSheet}
	<ShortcutSheet onClose={() => ui.closeShortcuts()} />
{/if}

{#if ui.relinkDialog}
	<RelinkDialog onClose={() => ui.closeRelink()} />
{/if}

<ContextMenu />
<NotificationCenter />
