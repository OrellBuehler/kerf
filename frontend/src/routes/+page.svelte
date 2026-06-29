<script lang="ts">
	import { onMount } from 'svelte';
	import { toast } from 'svelte-sonner';
	import TitleBar from '$lib/components/editor/TitleBar.svelte';
	import Toolbar from '$lib/components/editor/Toolbar.svelte';
	import MediaBin from '$lib/components/editor/MediaBin.svelte';
	import Preview from '$lib/components/editor/Preview.svelte';
	import Timeline from '$lib/components/editor/Timeline.svelte';
	import Inspector from '$lib/components/editor/Inspector.svelte';
	import AgentPanel from '$lib/components/editor/AgentPanel.svelte';
	import StatusBar from '$lib/components/editor/StatusBar.svelte';
	import ExportDialog from '$lib/components/editor/ExportDialog.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { agent } from '$lib/agent.svelte';
	import { inTauri } from '$lib/api';

	let exportOpen = $state(false);

	onMount(() => {
		void editor.load();
		void agent.load();

		// The desktop app hosts the MCP server, so an agent can edit the same
		// project live. It emits `project-changed` after each mutation; re-fetch
		// the timeline, history, and task queue so the GUI reflects agent edits.
		let unlisten: (() => void) | undefined;
		if (inTauri()) {
			void import('@tauri-apps/api/event').then(({ listen }) =>
				listen('project-changed', () => {
					void editor.refreshTimeline();
					void editor.refreshHistory();
					void agent.load();
				}).then((un) => {
					unlisten = un;
				})
			);
		}
		return () => unlisten?.();
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
		try {
			const { imported, failed } = await editor.importMedia();
			for (const f of failed) toast.error(`Couldn't import ${f.name}: ${f.message}`);
			if (imported.length > 0) {
				toast.success(
					imported.length === 1 ? `Imported ${imported[0].name}` : `Imported ${imported.length} files`
				);
				for (const a of imported) await ui.runAnalysis(a.id);
			}
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
			}
			return;
		}

		// Tools / transport (bare keys).
		if (k === 'v') ui.tool = 'pointer';
		else if (k === 'c') ui.tool = 'razor';
		else if (e.key === ' ') {
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
		} else if ((e.key === 'Delete' || e.key === 'Backspace') && editor.selectedClipId) {
			e.preventDefault();
			// Shift+Delete ripples (closes the gap); plain Delete leaves a gap.
			const id = editor.selectedClipId;
			void (e.shiftKey ? editor.rippleDelete(id) : editor.remove(id))
				.then(() => toast('Clip removed', { action: { label: 'Undo', onClick: () => void editor.undo() } }))
				.catch((err) => toast.error(err instanceof Error ? err.message : String(err)));
		}
	}
</script>

<svelte:window onkeydown={onKey} />

<div style="position:fixed;inset:0;display:flex;flex-direction:column;background:var(--surface-void)">
	<TitleBar />
	<Toolbar {onNew} {onExport} {onOpen} {onSave} />
	<div style="flex:1;display:flex;min-height:0">
		<MediaBin />
		<div style="flex:1;display:flex;flex-direction:column;min-width:0">
			<Preview />
			<Timeline />
		</div>
		{#if editor.selectedClip}
			<Inspector />
		{/if}
		{#if ui.agentOpen}
			<AgentPanel />
		{/if}
	</div>
	<StatusBar />
</div>

{#if exportOpen}
	<ExportDialog onClose={() => (exportOpen = false)} />
{/if}
