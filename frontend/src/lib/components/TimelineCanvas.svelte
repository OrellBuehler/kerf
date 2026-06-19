<script lang="ts">
	import { SvelteFlow, Background, Controls, Panel, type Node, type Edge } from '@xyflow/svelte';
	import '@xyflow/svelte/dist/style.css';
	import ClipNode from './clip-node.svelte';
	import { editor } from '$lib/state.svelte';
	import { clipDuration } from '$lib/types';

	// Custom node renderer for timeline clips.
	const nodeTypes = { clip: ClipNode } as never;

	const PX_PER_SEC = 12;
	const TRACK_H = 76;

	let nodes = $state.raw<Node[]>([]);
	let edges = $state.raw<Edge[]>([]);

	// Rebuild the graph whenever the timeline or assets change.
	$effect(() => {
		const out: Node[] = [];
		editor.timeline.tracks.forEach((track, ti) => {
			for (const clip of track.clips) {
				const dur = clipDuration(clip);
				const asset = editor.assets.find((a) => a.id === clip.asset_id);
				const hasAudio = !!asset?.streams.some((s) => s.kind === 'audio');
				out.push({
					id: clip.id,
					type: 'clip',
					position: { x: clip.timeline_start * PX_PER_SEC, y: ti * TRACK_H },
					width: Math.max(48, dur * PX_PER_SEC),
					draggable: true,
					connectable: false,
					data: {
						label: editor.assetName(clip.asset_id),
						kind: track.kind,
						duration: dur,
						volume: clip.volume,
						hasAudio
					}
				});
			}
		});
		nodes = out;
	});

	const totalDuration = $derived(
		Math.max(
			0,
			...editor.timeline.tracks.flatMap((t) => t.clips.map((c) => c.timeline_start + clipDuration(c)))
		)
	);
</script>

<div class="bg-muted/20 h-full w-full">
	<SvelteFlow bind:nodes bind:edges {nodeTypes} fitView minZoom={0.2} maxZoom={4}>
		<Background gap={24} />
		<Controls />
		<Panel position="top-left">
			<div class="bg-background/80 rounded-md border px-3 py-1.5 text-xs backdrop-blur">
				<span class="font-medium">Timeline</span>
				<span class="text-muted-foreground">
					· {totalDuration.toFixed(1)}s · {editor.timeline.tracks.length} tracks</span
				>
			</div>
		</Panel>
	</SvelteFlow>
</div>
