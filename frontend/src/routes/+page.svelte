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
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { agent } from '$lib/agent.svelte';
	import { inTauri, pickAndExport } from '$lib/api';

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

	async function onExport() {
		if (!inTauri()) {
			toast.info('Export renders the timeline with FFmpeg in the desktop app.');
			return;
		}
		try {
			const out = await toast.promise(pickAndExport(), {
				loading: 'Rendering timeline…',
				success: (p) => (p ? `Exported → ${p}` : 'Export cancelled'),
				error: (e) => (e instanceof Error ? e.message : String(e))
			});
			void out;
		} catch {
			/* surfaced via toast */
		}
	}

	function onKey(e: KeyboardEvent) {
		if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
		const k = e.key.toLowerCase();
		if ((e.metaKey || e.ctrlKey) && k === 'z') {
			e.preventDefault();
			if (e.shiftKey) {
				if (editor.canRedo) void editor.redo();
			} else if (editor.canUndo) {
				void editor.undo();
			}
			return;
		}
		if ((e.metaKey || e.ctrlKey) && k === 'y') {
			e.preventDefault();
			if (editor.canRedo) void editor.redo();
			return;
		}
		if (k === 'v') ui.tool = 'pointer';
		else if (k === 'c') ui.tool = 'razor';
		else if (k === 'm') ui.tool = 'bookmark';
		else if (e.key === ' ') {
			e.preventDefault();
			ui.togglePlay();
		} else if (e.key === '+' || e.key === '=') {
			e.preventDefault();
			ui.zoom = Math.min(96, ui.zoom + 8);
		} else if (e.key === '-') {
			e.preventDefault();
			ui.zoom = Math.max(8, ui.zoom - 8);
		} else if ((e.key === 'Delete' || e.key === 'Backspace') && editor.selectedClipId) {
			e.preventDefault();
			// Shift+Delete ripples (closes the gap); plain Delete leaves a gap.
			if (e.shiftKey) void editor.rippleDelete(editor.selectedClipId);
			else void editor.remove(editor.selectedClipId);
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
