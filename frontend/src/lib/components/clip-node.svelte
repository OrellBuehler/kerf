<script lang="ts">
	import type { NodeProps } from '@xyflow/svelte';

	type ClipData = {
		label: string;
		kind: 'video' | 'audio' | 'subtitle' | 'data';
		duration: number;
		volume: number;
		hasAudio: boolean;
	};

	const props: NodeProps = $props();
	const data = $derived(props.data as unknown as ClipData);

	const fmt = (s: number) => `${s.toFixed(1)}s`;
</script>

<div
	class="flex h-14 select-none flex-col justify-between overflow-hidden rounded-md border px-2 py-1 text-xs shadow-sm"
	class:bg-sky-500={data.kind === 'video'}
	class:border-sky-300={data.kind === 'video'}
	class:text-sky-50={data.kind === 'video'}
	class:bg-emerald-600={data.kind === 'audio'}
	class:border-emerald-400={data.kind === 'audio'}
	class:text-emerald-50={data.kind === 'audio'}
	style="width: 100%"
>
	<div class="truncate font-medium">{data.label}</div>
	<div class="flex items-center justify-between opacity-80">
		<span>{fmt(data.duration)}</span>
		{#if data.kind === 'audio' || data.hasAudio}
			<span>vol {Math.round(data.volume * 100)}%</span>
		{/if}
	</div>
</div>
