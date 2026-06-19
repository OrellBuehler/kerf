<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import { Separator } from '$lib/components/ui/separator';
	import { Play, SkipBack, SkipForward, Film } from '@lucide/svelte';
	import { editor } from '$lib/state.svelte';
	import { clipDuration } from '$lib/types';

	const asset = $derived(editor.selectedAsset);
	const timelineDuration = $derived(
		Math.max(
			0,
			...editor.timeline.tracks.flatMap((t) => t.clips.map((c) => c.timeline_start + clipDuration(c)))
		)
	);

	function fmt(s: number): string {
		const m = Math.floor(s / 60);
		const sec = Math.floor(s % 60);
		return `${m}:${sec.toString().padStart(2, '0')}`;
	}
</script>

<div class="flex h-full flex-col bg-black">
	<div class="flex flex-1 items-center justify-center p-4">
		<div
			class="relative flex aspect-video w-full max-w-3xl items-center justify-center rounded-md border border-white/10 bg-zinc-900"
		>
			<div class="flex flex-col items-center gap-2 text-zinc-500">
				<Film class="size-10" />
				<p class="text-sm">{asset ? asset.name : 'No clip selected'}</p>
				{#if asset}
					{@const v = asset.streams.find((s) => s.kind === 'video')}
					{#if v}
						<p class="text-xs text-zinc-600">{v.width}×{v.height} · {v.fps?.toFixed(0)} fps</p>
					{/if}
				{/if}
			</div>
		</div>
	</div>

	<Separator class="bg-white/10" />
	<div class="flex items-center gap-3 px-4 py-2 text-white">
		<Button size="icon" variant="ghost" class="text-white hover:bg-white/10" aria-label="Previous">
			<SkipBack class="size-4" />
		</Button>
		<Button size="icon" variant="ghost" class="text-white hover:bg-white/10" aria-label="Play">
			<Play class="size-4" />
		</Button>
		<Button size="icon" variant="ghost" class="text-white hover:bg-white/10" aria-label="Next">
			<SkipForward class="size-4" />
		</Button>
		<div class="bg-white/15 mx-2 h-1 flex-1 overflow-hidden rounded-full">
			<div class="bg-primary h-full w-0"></div>
		</div>
		<span class="font-mono text-xs tabular-nums text-zinc-400">0:00 / {fmt(timelineDuration)}</span>
	</div>
</div>
