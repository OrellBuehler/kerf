<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { toast } from '$lib/notifications.svelte';
	import TitleBar from '$lib/components/editor/TitleBar.svelte';
	import Toolbar from '$lib/components/editor/Toolbar.svelte';
	import MediaBin from '$lib/components/editor/MediaBin.svelte';
	import Preview from '$lib/components/editor/Preview.svelte';
	import Timeline from '$lib/components/editor/Timeline.svelte';
	import Inspector from '$lib/components/editor/Inspector.svelte';
	import AgentPanel from '$lib/components/editor/AgentPanel.svelte';
	import StatusBar from '$lib/components/editor/StatusBar.svelte';
	import ExportDialog from '$lib/components/editor/ExportDialog.svelte';
	import SettingsDialog from '$lib/components/editor/SettingsDialog.svelte';
	import UpdateDialog from '$lib/components/editor/UpdateDialog.svelte';
	import ContextMenu from '$lib/components/editor/ContextMenu.svelte';
	import NotificationCenter from '$lib/components/editor/NotificationCenter.svelte';
	import Icon from '$lib/components/editor/Icon.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { agent } from '$lib/agent.svelte';
	import { updater } from '$lib/updater.svelte';
	import { settings } from '$lib/settings.svelte';
	import { inTauri, isMediaPath } from '$lib/api';
	import type { AnalysisProgress, ModelProgress } from '$lib/types';

	let exportOpen = $state(false);
	/** True while files are hovering over the window, for the drop overlay. */
	let dropHover = $state(false);

	// Any timeline edit mid-playback re-anchors the audio so what's heard
	// matches the new cut (volume/fade tweaks land live too).
	$effect(() => {
		void editor.timeline;
		untrack(() => ui.resync());
	});

	onMount(() => {
		void editor.load();
		void agent.load();
		void ui.loadFonts();
		void ui.loadTranscriptionStatus();
		void settings.load();
		// Ask GitHub whether a newer signed release exists (silently — offline is
		// not worth an interruption) and offer it in the title bar / dialog.
		const stopUpdater = updater.init();

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
					await Promise.all([editor.refreshTimeline(), editor.refreshHistory(), agent.load()]).catch(() => {});
				} while (dirty);
			} finally {
				refreshing = false;
			}
		}
		const unlisteners: Array<() => void> = [];
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
						if (ui.dndAsset) return;
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
					// An agent can pick the speech model over MCP; the status is
					// otherwise only read at launch, so the picker would keep
					// showing the previous model until the next start.
					await listen('speech-model-changed', () => void ui.loadTranscriptionStatus()),
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
			});
		}
		return () => {
			for (const un of unlisteners) un();
			stopUpdater();
		};
	});

	async function onNew() {
		if (!inTauri()) {
			toast.info('Creating a project is available in the desktop app.');
			return;
		}
		try {
			if (await editor.newProject()) {
				await agent.load();
				toast.success('New project');
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	async function onOpen() {
		if (!inTauri()) {
			toast.info('Opening a project file is available in the desktop app.');
			return;
		}
		try {
			if (await editor.openProject()) {
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
		exportOpen = true;
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
			await ui.analyzeQueue(imported.map((a) => a.id));
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	/** One step for arrow-key seeking: a single source frame (derived from the
	 *  selected asset's fps, default 30), or a whole second when Shift is held. */
	function frameStep(coarse: boolean): number {
		if (coarse) return 1;
		const v = editor.selectedAsset?.streams.find((s) => s.kind === 'video');
		const fps = v?.fps && v.fps > 0 ? v.fps : 30;
		return 1 / fps;
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

	function onKey(e: KeyboardEvent) {
		if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
		const k = e.key.toLowerCase();

		// File operations (⌘/Ctrl). Handled first so they win over the bare-key
		// tool shortcuts, and any other modified combo returns without falling
		// through (so e.g. ⌘C doesn't get read as the razor 'c').
		if (e.metaKey || e.ctrlKey) {
			if (k === 'z') {
				e.preventDefault();
				if (e.shiftKey) {
					if (editor.canRedo) void editor.redo();
				} else if (editor.canUndo) void editor.undo();
			} else if (k === 'y') {
				e.preventDefault();
				if (editor.canRedo) void editor.redo();
			} else if (k === 's') {
				e.preventDefault();
				void onSave();
			} else if (k === 'o') {
				e.preventDefault();
				void onOpen();
			} else if (k === 'n') {
				e.preventDefault();
				void onNew();
			} else if (k === 'e') {
				e.preventDefault();
				onExport();
			} else if (k === 'i') {
				e.preventDefault();
				void onImport();
			} else if (e.key === ',') {
				e.preventDefault();
				settings.toggle();
			} else if (k === 'a') {
				e.preventDefault();
				editor.selectAll();
			} else if (k === 'c') {
				e.preventDefault();
				const n = editor.copySelection();
				if (n) toast(n === 1 ? 'Clip copied' : `${n} clips copied`);
			} else if (k === 'x') {
				e.preventDefault();
				const n = editor.copySelection();
				if (n)
					void editor
						.removeSelected(false)
						.then(() => toast(n === 1 ? 'Clip cut' : `${n} clips cut`))
						.catch(clipErr);
			} else if (k === 'v') {
				e.preventDefault();
				void editor
					.paste(ui.time)
					.then((n) => n && toast(n === 1 ? 'Clip pasted' : `${n} clips pasted`))
					.catch(clipErr);
			} else if (k === 'd') {
				e.preventDefault();
				void editor
					.duplicateSelection()
					.then((n) => n && toast(n === 1 ? 'Clip duplicated' : `${n} clips duplicated`))
					.catch(clipErr);
			}
			return;
		}

		// Tools / transport (bare keys).
		if (k === 'v') ui.tool = 'pointer';
		else if (k === 'c') ui.tool = 'razor';
		else if (k === 'm') {
			void editor
				.addMarkerAtPlayhead(ui.time)
				.then(() => toast('Marker added', { action: { label: 'Undo', onClick: () => void editor.undo() } }))
				.catch((err) => toast.error(err instanceof Error ? err.message : String(err)));
		} else if (e.key === ',') ui.gotoMarker(-1);
		else if (e.key === '.') ui.gotoMarker(1);
		else if (k === 'j') ui.shuttle(-1);
		else if (k === 'k') ui.pause();
		else if (k === 'l') ui.shuttle(1);
		else if (k === 'i') {
			// I/O mark the working range at the playhead; Shift clears a mark.
			// The pair stays ordered so a mark can't cross its partner.
			if (e.shiftKey) ui.markIn = null;
			else ui.markIn = Math.min(ui.time, ui.markOut ?? Infinity);
		} else if (k === 'o') {
			if (e.shiftKey) ui.markOut = null;
			else ui.markOut = Math.max(ui.time, ui.markIn ?? 0);
		} else if (e.key === ' ') {
			e.preventDefault();
			ui.togglePlay();
		} else if (e.key === 'ArrowLeft') {
			e.preventDefault();
			ui.seek(ui.time - frameStep(e.shiftKey));
		} else if (e.key === 'ArrowRight') {
			e.preventDefault();
			ui.seek(ui.time + frameStep(e.shiftKey));
		} else if (e.key === 'Home') {
			e.preventDefault();
			ui.seek(0);
		} else if (e.key === 'End') {
			e.preventDefault();
			ui.seek(editor.duration);
		} else if (e.key === '+' || e.key === '=') {
			e.preventDefault();
			ui.zoom = Math.min(96, ui.zoom + 8);
		} else if (e.key === '-') {
			e.preventDefault();
			ui.zoom = Math.max(8, ui.zoom - 8);
		} else if ((e.key === 'Delete' || e.key === 'Backspace') && editor.selectedClipIds.length > 0) {
			e.preventDefault();
			// Shift+Delete ripples (closes the gap); plain Delete leaves a gap.
			void editor
				.removeSelected(e.shiftKey)
				.then((n) =>
					toast(n === 1 ? 'Clip removed' : `${n} clips removed`, {
						action: { label: 'Undo', onClick: () => void editor.undo() }
					})
				)
				.catch((err) => toast.error(err instanceof Error ? err.message : String(err)));
		}
	}
</script>

<svelte:window onkeydown={onKey} oncontextmenu={onContextMenu} />

<div style="position:fixed;inset:0;display:flex;flex-direction:column;background:var(--surface-void)">
	<TitleBar />
	<Toolbar {onNew} {onExport} {onOpen} {onSave} />
	<!-- While a proposal is on screen the editor is showing a cut that is not
	     the project's yet; say so where it cannot be missed. -->
	{#if editor.previewingStaged}
		<div
			style="flex:none;display:flex;align-items:center;gap:9px;height:30px;padding:0 12px;background:var(--agent-surface);border-bottom:1px solid var(--agent-border);color:var(--agent-300);font-size:12px"
		>
			<span style="width:7px;height:7px;border-radius:50%;background:var(--agent-400);box-shadow:0 0 8px var(--agent-400)"
			></span>
			<span>Previewing the agent's proposed cut — your timeline is unchanged.</span>
			<div style="flex:1"></div>
			<button
				onclick={() => editor.exitStagedPreview()}
				style="height:22px;padding:0 9px;border-radius:var(--radius-full);border:1px solid var(--agent-border);background:var(--surface-raised);color:var(--text-secondary);font-size:11px;cursor:pointer"
				>Exit preview</button
			>
		</div>
	{/if}
	<!-- Something the project itself could not do — a `.kerf` that would not
	     open, an edit the backend refused. It used to be recorded in
	     `editor.error` and never shown, so a corrupt file opened as silence. -->
	{#if editor.error}
		<div
			style="flex:none;display:flex;align-items:center;gap:9px;min-height:30px;padding:5px 12px;background:color-mix(in srgb,var(--danger) 14%,transparent);border-bottom:1px solid color-mix(in srgb,var(--danger) 40%,transparent);color:var(--text-primary);font-size:12px"
		>
			<Icon n="alert-triangle" s={13} color="var(--danger)" />
			<span style="flex:1;min-width:0">{editor.error}</span>
			<button
				onclick={() => (editor.error = null)}
				style="height:22px;padding:0 9px;border-radius:var(--radius-full);border:1px solid var(--border-strong);background:var(--surface-raised);color:var(--text-secondary);font-size:11px;cursor:pointer"
				>Dismiss</button
			>
		</div>
	{/if}
	<div style="flex:1;display:flex;min-height:0">
		<MediaBin />
		<div style="flex:1;display:flex;flex-direction:column;min-width:0">
			<Preview />
			<Timeline />
		</div>
		{#if ui.inspectorOpen}
			<Inspector />
		{/if}
		{#if ui.agentOpen}
			<AgentPanel />
		{/if}
	</div>
	<StatusBar />
</div>

<!-- Files are over the window and about to be dropped. -->
{#if dropHover}
	<div
		style="position:fixed;inset:0;z-index:60;display:grid;place-items:center;background:color-mix(in srgb,var(--surface-void) 72%,transparent);pointer-events:none"
	>
		<div
			style="display:flex;flex-direction:column;align-items:center;gap:10px;padding:26px 40px;border-radius:var(--radius-md);border:1.5px dashed var(--kerf-400);background:var(--surface-panel);color:var(--text-primary)"
		>
			<Icon n="film" s={24} color="var(--kerf-400)" />
			<span style="font:var(--type-ui)">Drop to import</span>
		</div>
	</div>
{/if}

{#if exportOpen}
	<ExportDialog onClose={() => (exportOpen = false)} />
{/if}

{#if settings.open}
	<SettingsDialog onClose={() => settings.close()} />
{/if}

{#if updater.dialogOpen}
	<UpdateDialog />
{/if}

<ContextMenu />
<NotificationCenter />
