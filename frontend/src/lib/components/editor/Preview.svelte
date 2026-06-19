<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import { ui } from '$lib/editor-ui.svelte';

	const empty = $derived(ui.phase === 'empty');
</script>

<div style="flex:1;min-height:0;display:flex;flex-direction:column;background:var(--surface-void)">
	<div style="flex:1;min-height:0;display:grid;place-items:center;padding:20px;position:relative">
		{#if empty}
			<div
				style="display:flex;flex-direction:column;align-items:center;gap:12px;color:var(--text-disabled)"
			>
				<Icon n="clapperboard" s={30} /><span style="font-size:13px">No media loaded</span>
			</div>
		{:else}
			<div
				style="position:relative;aspect-ratio:16/9;max-height:100%;max-width:100%;width:min(100%, 720px);border-radius:4px;overflow:hidden;background:radial-gradient(120% 120% at 30% 20%, #2b3a49 0%, #161d24 55%, #0d1116 100%);border:1px solid var(--border-default);box-shadow:var(--shadow-md)"
			>
				<div
					style="position:absolute;inset:0;background:linear-gradient(115deg, transparent 40%, rgba(226,157,46,.06) 60%)"
				></div>
				<div style="position:absolute;left:14px;top:12px;display:flex;gap:6px">
					<Badge tone="kerf">A-roll 04</Badge>
					{#if ui.phase === 'analyzing'}<Badge tone="agent" dot>agent working</Badge>{/if}
				</div>
				<div
					style="position:absolute;right:14px;top:12px;font-family:var(--font-mono);font-size:11px;color:rgba(255,255,255,.55)"
				>
					3840×2160 · 23.976
				</div>
				<div
					style="position:absolute;inset:0;display:grid;place-items:center;color:rgba(255,255,255,.22)"
				>
					<Icon n={ui.playing ? 'pause' : 'play'} s={44} />
				</div>
				<div
					style="position:absolute;left:14px;bottom:12px;font-family:var(--font-mono);font-size:12px;color:var(--kerf-200)"
				>
					00:00:32:08
				</div>
			</div>
		{/if}
	</div>
	<div
		style="height:40px;flex:none;display:flex;align-items:center;gap:12px;padding:0 16px;border-top:1px solid var(--border-default);background:var(--surface-app)"
	>
		<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-secondary)">00:00:32:08</span>
		<div style="flex:1;height:4px;border-radius:999px;background:var(--surface-inset);position:relative">
			<div
				style="position:absolute;inset:0 auto 0 0;width:{empty
					? '0%'
					: '38%'};background:var(--kerf-500);border-radius:999px"
			></div>
			<div
				style="position:absolute;left:{empty
					? '0%'
					: '38%'};top:50%;width:11px;height:11px;border-radius:50%;background:var(--kerf-400);transform:translate(-50%,-50%);box-shadow:0 0 0 3px var(--surface-app)"
			></div>
		</div>
		<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)">01:24:00</span>
	</div>
</div>
