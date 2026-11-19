<script lang="ts">
	/* What a lane draws over its clips while a gesture is held: ghosts, trim, razor and roll marks, the drop ghost. */
	import Icon from '../Icon.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import type { LinkedTrimGhost, LinkedTrimPreview } from '#lib/linked-trim';
	import type { Ghost } from '#lib/multi-move';
	import type { EditPreview } from '#lib/trim-tools';
	import type { Track } from '#lib/types';
	import type { TrimDrag, TrimGesture } from './gesture';

	let {
		t,
		pxPerSec,
		ghostsByTrack,
		dropRefused,
		trimView,
		trimGhosts,
		trimDrag,
		razorLanes,
		razor,
		rollHover,
		tg,
		tgPreview,
		dropGhost
	}: {
		t: Track;
		pxPerSec: number;
		ghostsByTrack: Map<string, Ghost[]>;
		dropRefused: boolean;
		trimView: LinkedTrimPreview | null;
		trimGhosts: Map<string, LinkedTrimGhost[]>;
		trimDrag: TrimDrag | null;
		razorLanes: Map<string, string>;
		razor: { at: number | null } | null;
		rollHover: { trackId: string; time: number } | null;
		tg: TrimGesture | null;
		tgPreview: EditPreview | null;
		dropGhost: { trackId: string; start: number; dur: number; ok: boolean; hit?: number; label?: string } | null;
	} = $props();
</script>

<!-- One ghost per clip of the dragged group, where it would land: red
     all over when the group cannot (the drop then does nothing). A linked
     partner carried along is a ghost too, on its own track, marked with the chain. -->
{#each ghostsByTrack.get(t.id) ?? [] as g (g.clipId)}
	<div
		data-ghost={g.carried ? 'carried' : 'moved'}
		style="position:absolute;left:{g.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
			6,
			g.dur * pxPerSec
		)}px;border:1.5px dashed {dropRefused ? 'var(--red-500)' : g.carried ? 'var(--kerf-300)' : 'var(--kerf-400)'};border-radius:2px;background:{dropRefused
			? 'var(--danger-surface)'
			: `color-mix(in srgb,var(--drag-ghost) ${g.carried ? 10 : 16}%,transparent)`};pointer-events:none;z-index:25;overflow:hidden"
	>
		{#if g.carried}
			<span style="position:absolute;left:3px;top:2px;color:{dropRefused ? 'var(--red-500)' : 'var(--kerf-300)'}"
				><Icon n="link" s={10} /></span
			>
		{/if}
	</div>
{/each}
{#if trimView}
	<!-- Where the trimmed clip and everything the trim changes would be, on every
	     track: its linked partners following the edge, the clips ripple pushes and
	     — the sync lock — the partners of those. A left edge keeps the clip's start
	     under ripple, so this is not the edge dragged. -->
	{#each trimGhosts.get(t.id) ?? [] as g (g.id)}
		<div
			data-ghost={g.id === trimDrag?.clipId ? 'trimmed' : trimView.shifted.has(g.id) ? 'shifted' : 'partner'}
			style="position:absolute;left:{g.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
				2,
				g.dur * pxPerSec
			)}px;border:1.5px dashed {trimView.ok ? 'var(--kerf-400)' : 'var(--red-500)'};border-radius:2px;background:{trimView.ok
				? 'color-mix(in srgb,var(--drag-ghost) 16%,transparent)'
				: 'var(--danger-surface)'};pointer-events:none;z-index:25"
		></div>
	{/each}
{:else if trimDrag?.moved && trimDrag.trackId === t.id}
	{@const gl = trimDrag.edge === 'l' ? trimDrag.pos : trimDrag.origStart}
	{@const gr = trimDrag.edge === 'l' ? trimDrag.origEnd : trimDrag.pos}
	<div
		style="position:absolute;left:{gl * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
			2,
			(gr - gl) * pxPerSec
		)}px;border:1.5px dashed var(--kerf-400);border-radius:2px;background:color-mix(in srgb,var(--drag-ghost) 16%,transparent);pointer-events:none;z-index:25"
	></div>
{/if}
{#if trimDrag?.moved && trimDrag.trackId === t.id}
	<!-- The part of the clip the trim takes away is hatched and stood back, so a trim reads as a cut. -->
	{@const cutFrom = trimDrag.edge === 'l' ? trimDrag.origStart : Math.min(trimDrag.pos, trimDrag.origEnd)}
	{@const cutTo = trimDrag.edge === 'l' ? Math.max(trimDrag.pos, trimDrag.origStart) : trimDrag.origEnd}
	{#if cutTo - cutFrom > 1e-6}
		<div
			data-trim-away
			style="position:absolute;left:{cutFrom * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(1, (cutTo - cutFrom) * pxPerSec)}px;border-radius:2px;background:repeating-linear-gradient(135deg,color-mix(in srgb,var(--scrim) 80%,transparent) 0 2px,color-mix(in srgb,var(--scrim) 55%,transparent) 2px 6px);pointer-events:none;z-index:24"
		></div>
	{/if}
{/if}
{#if razorLanes.has(t.id) && razor && razor.at !== null && ui.tool === 'razor'}
	<!-- The cut the razor would make, through this clip and the partners cut with it. -->
	<div
		data-razor-line
		style="position:absolute;left:{razor.at * pxPerSec - 1}px;top:2px;bottom:2px;width:2px;border-radius:1px;background:var(--text-primary);box-shadow:0 0 0 1px color-mix(in srgb,var(--scrim) 70%,transparent);pointer-events:none;z-index:27"
	></div>
{/if}
{#if rollHover?.trackId === t.id && !tg}
	<!-- The cut the roll tool would take hold of. -->
	<div
		style="position:absolute;left:{rollHover.time * pxPerSec - 1}px;top:3px;bottom:3px;width:2px;border-radius:1px;background:var(--kerf-300);box-shadow:0 0 6px 1px var(--kerf-400);pointer-events:none;z-index:24"
	></div>
{/if}
{#if tg?.moved && tgPreview}
	<!-- A roll, slip or slide in progress: every clip it changes, where it would
	     stand — a linked clip's partners on their own tracks. Amber when the drag is
	     held at a limit, red when the backend would turn it down (letting go then
	     writes nothing). -->
	{@const pv = tgPreview}
	{@const edge = !pv.ok ? 'var(--red-500)' : pv.clamped ? 'var(--warning)' : 'var(--kerf-400)'}
	{#each pv.ghosts.filter((g) => g.trackId === t.id) as g (g.id)}
		<div
			data-ghost={g.role === 'partner' ? 'partner' : g.role}
			style="position:absolute;left:{g.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
				2,
				g.dur * pxPerSec
			)}px;border:1.5px dashed {edge};border-radius:2px;background:{!pv.ok
				? 'var(--danger-surface)'
				: pv.clamped
					? 'var(--warning-surface)'
					: 'color-mix(in srgb,var(--drag-ghost) 16%,transparent)'};pointer-events:none;z-index:25"
		></div>
	{/each}
	{#if tg.edit?.tool === 'roll' && tg.trackId === t.id}
		<!-- Where the cut was, and where it is: the distance is the roll. -->
		<div
			style="position:absolute;left:{tg.origin * pxPerSec}px;top:2px;bottom:2px;width:0;border-left:1px dashed var(--text-muted);pointer-events:none;z-index:25"
		></div>
		<div
			style="position:absolute;left:{(tg.origin + pv.applied) * pxPerSec - 1}px;top:2px;bottom:2px;width:2px;border-radius:1px;background:{edge};box-shadow:0 0 6px 1px {edge};pointer-events:none;z-index:26"
		></div>
	{/if}
{/if}
{#if dropGhost && dropGhost.trackId === t.id}
	<div
		style="position:absolute;left:{dropGhost.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
			6,
			dropGhost.dur * pxPerSec
		)}px;border:1.5px dashed {dropGhost.ok
			? 'var(--kerf-400)'
			: 'var(--red-500)'};border-radius:2px;background:{dropGhost.ok
			? 'var(--selection-fill)'
			: 'var(--danger-surface)'};pointer-events:none;z-index:25"
	>
		{#if dropGhost.hit !== undefined}
			<span
				style="position:absolute;left:{Math.max(0, (dropGhost.hit - dropGhost.start) * pxPerSec - 1)}px;top:0;bottom:0;width:2px;background:{dropGhost.ok
					? 'var(--kerf-400)'
					: 'var(--red-500)'}"
			></span>
			{#if dropGhost.label}
				<span
					style="position:absolute;left:5px;top:50%;transform:translateY(-50%);max-width:calc(100% - 10px);overflow:hidden;white-space:nowrap;text-overflow:ellipsis;font-size:var(--text-xs);font-weight:500;color:var(--text-primary)"
					>{dropGhost.label}</span
				>
			{/if}
		{/if}
	</div>
{/if}
