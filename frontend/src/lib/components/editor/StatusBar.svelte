<script lang="ts">
	import Icon from './Icon.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { inTauri, revealLogs } from '$lib/api';
	import { toast } from 'svelte-sonner';

	const showLogs = inTauri();

	/** Real metadata of the selected asset: fps · resolution · codec. */
	const meta = $derived.by(() => {
		const a = editor.selectedAsset;
		if (!a) return null;
		const v = a.streams.find((s) => s.kind === 'video');
		const parts: string[] = [];
		if (v?.fps) parts.push(`${v.fps.toFixed(3)} fps`);
		if (v?.width && v?.height) parts.push(`${v.width}×${v.height}`);
		const codec = (v ?? a.streams[0])?.codec;
		if (codec) parts.push(codec);
		return parts.join(' · ') || null;
	});

	const clipCount = $derived(
		editor.timeline.tracks.reduce((n, t) => n + t.clips.length, 0)
	);

	function tc(s: number): string {
		const total = Math.max(0, s);
		const h = Math.floor(total / 3600);
		const m = Math.floor((total % 3600) / 60);
		const sec = Math.floor(total % 60);
		const mm = `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}`;
		return h > 0 ? `${h}:${mm}` : mm;
	}
</script>

<div
	style="height:var(--statusbar-h);flex:none;display:flex;align-items:center;gap:12px;padding:0 12px;background:var(--surface-app);border-top:1px solid var(--border-default)"
>
	{#if meta}
		<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">{meta}</span>
	{/if}
	<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">
		{tc(editor.duration)}
	</span>
	<div style="flex:1"></div>
	{#if ui.analyzing}
		<span style="display:inline-flex;align-items:center;gap:6px;font-size:10px;color:var(--agent-300)">
			<span style="width:6px;height:6px;border-radius:50%;background:var(--agent-400)"></span>
			Analyzing… {Math.round(ui.progress)}%
		</span>
	{:else}
		<span style="font-size:10px;color:var(--text-disabled)">
			{editor.assets.length} asset{editor.assets.length === 1 ? '' : 's'} · {clipCount} clip{clipCount ===
			1
				? ''
				: 's'}
		</span>
	{/if}
	{#if showLogs}
		<span style="width:1px;height:12px;background:var(--border-default)"></span>
		<button
			type="button"
			title="Open the log folder — attach kerf.<date>.log when reporting an issue"
			onclick={() =>
				revealLogs().catch((e) => toast.error(e instanceof Error ? e.message : String(e)))}
			style="display:inline-flex;align-items:center;gap:5px;background:none;border:none;cursor:pointer;color:var(--text-disabled);font-size:10px;padding:0"
		>
			<Icon n="folder-open" s={11} />
			Logs
		</button>
	{/if}
</div>
