<script lang="ts">
	import * as Card from '$lib/components/ui/card';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { ScrollArea } from '$lib/components/ui/scroll-area';
	import { Film, Music, Upload } from '@lucide/svelte';
	import { toast } from 'svelte-sonner';
	import { editor } from '$lib/state.svelte';
	import { inTauri } from '$lib/api';
	import type { Asset } from '$lib/types';

	function fmtDuration(s: number): string {
		const m = Math.floor(s / 60);
		const sec = Math.floor(s % 60);
		return `${m}:${sec.toString().padStart(2, '0')}`;
	}

	function resolution(asset: Asset): string | null {
		const v = asset.streams.find((s) => s.kind === 'video');
		return v?.width && v?.height ? `${v.width}×${v.height}` : null;
	}

	async function onImport() {
		if (!inTauri()) {
			toast.info('Media import is available in the desktop app.');
			return;
		}
		try {
			const asset = await editor.importMedia();
			if (asset) toast.success(`Imported ${asset.name}`);
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}
</script>

<div class="flex h-full flex-col">
	<div class="flex items-center justify-between border-b px-3 py-2">
		<h2 class="text-sm font-semibold">Media bin</h2>
		<Button size="sm" variant="secondary" onclick={onImport}>
			<Upload class="size-4" />
			Import
		</Button>
	</div>

	<ScrollArea class="flex-1">
		<div class="flex flex-col gap-2 p-3">
			{#each editor.assets as asset (asset.id)}
				<button class="text-left" onclick={() => editor.select(asset.id)}>
					<Card.Root
						class="hover:bg-accent/50 transition-colors {editor.selectedAssetId === asset.id
							? 'border-primary ring-primary/30 ring-1'
							: ''}"
					>
						<Card.Content class="flex items-center gap-3 p-3">
							<div class="bg-muted text-muted-foreground flex size-9 items-center justify-center rounded">
								{#if asset.streams.some((s) => s.kind === 'video')}
									<Film class="size-4" />
								{:else}
									<Music class="size-4" />
								{/if}
							</div>
							<div class="min-w-0 flex-1">
								<div class="truncate text-sm font-medium">{asset.name}</div>
								<div class="text-muted-foreground flex gap-1.5 text-xs">
									<span>{fmtDuration(asset.duration)}</span>
									{#if resolution(asset)}<span>· {resolution(asset)}</span>{/if}
								</div>
							</div>
							<div class="flex flex-col items-end gap-1">
								{#each asset.streams.slice(0, 2) as stream (stream.index)}
									<Badge variant="outline" class="text-[10px]">{stream.codec}</Badge>
								{/each}
							</div>
						</Card.Content>
					</Card.Root>
				</button>
			{/each}

			{#if editor.assets.length === 0}
				<p class="text-muted-foreground p-4 text-center text-sm">
					No media yet. Import a file to get started.
				</p>
			{/if}
		</div>
	</ScrollArea>
</div>
