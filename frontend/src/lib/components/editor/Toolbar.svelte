<script lang="ts">
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import Btn from './Btn.svelte';
	import { ui, type Tool } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';

	let {
		onNew,
		onExport,
		onOpen,
		onSave
	}: { onNew: () => void; onExport: () => void; onOpen: () => void; onSave: () => void } =
		$props();

	const tools: [Tool, string, string][] = [
		['pointer', 'MousePointer2', 'Select (V)'],
		['razor', 'Scissors', 'Razor (C)'],
		['bookmark', 'Bookmark', 'Marker (M)']
	];

	function tc(s: number): string {
		const m = Math.floor(s / 60);
		const sec = Math.floor(s % 60);
		const frames = Math.floor((s % 1) * 24);
		return `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}:${frames.toString().padStart(2, '0')}`;
	}
</script>

{#snippet divider()}
	<span style="width:1px;height:22px;background:var(--border-strong);margin:0 4px;flex:none"></span>
{/snippet}

<div
	style="height:var(--toolbar-h);display:flex;align-items:center;gap:6px;padding:0 12px;background:var(--surface-panel);border-bottom:1px solid var(--border-default);flex:none"
>
	{#each tools as [id, ic, t] (id)}
		<IconBtn title={t} active={ui.tool === id} onclick={() => (ui.tool = id)}>
			<Icon n={ic} />
		</IconBtn>
	{/each}
	<IconBtn title="Snap to clips" active={ui.snap} onclick={() => (ui.snap = !ui.snap)}>
		<Icon n="magnet" />
	</IconBtn>

	{@render divider()}

	<IconBtn
		title="Undo (⌘Z)"
		disabled={!editor.canUndo}
		onclick={() => editor.undo()}
		style={editor.canUndo ? '' : 'opacity:.4;cursor:default'}
	>
		<Icon n="undo" />
	</IconBtn>
	<IconBtn
		title="Redo (⇧⌘Z)"
		disabled={!editor.canRedo}
		onclick={() => editor.redo()}
		style={editor.canRedo ? '' : 'opacity:.4;cursor:default'}
	>
		<Icon n="redo" />
	</IconBtn>

	{@render divider()}

	<IconBtn title="Skip to start" onclick={() => ui.seek(0)}><Icon n="skip-back" /></IconBtn>
	<IconBtn
		title={ui.playing ? 'Pause' : 'Play'}
		onclick={() => ui.togglePlay()}
		style="background:var(--surface-hover);color:var(--text-primary)"
	>
		<Icon n={ui.playing ? 'pause' : 'play'} />
	</IconBtn>
	<IconBtn title="Skip to end" onclick={() => ui.seek(editor.duration)}><Icon n="skip-forward" /></IconBtn>
	<span style="font-family:var(--font-mono);font-size:13px;color:var(--kerf-300);margin-left:6px;font-weight:500">
		{tc(ui.time)}
	</span>

	<div style="flex:1"></div>

	<Btn variant="ghost" size="sm" icon="file-plus" onclick={onNew} title="New empty project">New</Btn>
	<Btn variant="ghost" size="sm" icon="folder-open" onclick={onOpen} title="Open project…">Open</Btn>
	<Btn
		variant={editor.saved ? 'ghost' : 'secondary'}
		size="sm"
		icon="save"
		onclick={onSave}
		title="Save project as…">Save</Btn
	>
	{@render divider()}
	<Btn
		variant={ui.agentOpen ? 'agent' : 'ghost'}
		size="sm"
		icon="plug"
		onclick={() => (ui.agentOpen = !ui.agentOpen)}>Agent</Btn
	>
	{@render divider()}
	<Btn variant="primary" size="sm" icon="upload" onclick={onExport}>Export</Btn>
</div>
