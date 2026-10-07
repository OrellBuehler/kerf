<script lang="ts">
	/* The whole cut on one thin strip: every clip as a coloured block on its
	 * track's row, the playhead, the in / out marks, and the part the timeline is
	 * showing as a rectangle you can drag (scroll), drag by an edge (zoom), or jump
	 * by pressing on the bare strip. `minimap.ts` is all of the geometry — this
	 * measures the strip, draws what it says, and turns a pointer into one of its
	 * gestures.
	 *
	 * A gesture is absolute: each move is worked out from the pointer's position and
	 * the view as it was at the press (`start`), never from the last move, so a drag
	 * that the timeline answers a frame late cannot drift. The timeline owns the
	 * scroller, so a gesture ends in `onview` with the scroll and zoom it wants and
	 * the timeline applies them (a zoom change needs the lane re-widened first).
	 * Escape / pointercancel / blur put the view back as the press found it. */
	import { beginDrag } from '$lib/drag';
	import { clipDuration, type Track } from '$lib/types';
	import {
		EDGE_PX,
		centerOn,
		hitTest,
		mapSpan,
		moveTo,
		resizeLeft,
		resizeRight,
		rowLayout,
		timeToX,
		trackBlocks,
		windowRect,
		xToTime,
		type Hit,
		type MapGeo,
		type Target,
		type View
	} from '$lib/minimap';

	let {
		tracks,
		duration,
		scrollLeft,
		viewW,
		pxPerSec,
		time,
		markIn,
		markOut,
		onview,
		onseek
	}: {
		tracks: Track[];
		/** The cut's length, seconds. */
		duration: number;
		/** The timeline's view: how far it is scrolled, how wide it shows, its zoom. */
		scrollLeft: number;
		viewW: number;
		pxPerSec: number;
		time: number;
		markIn: number | null;
		markOut: number | null;
		/** Ask the timeline to scroll / zoom to this. */
		onview: (target: Target) => void;
		/** Move the playhead (a double-click on the strip). */
		onseek: (time: number) => void;
	} = $props();

	/** The strip's height, px. */
	const HEIGHT = 36;

	let strip = $state<HTMLElement | null>(null);
	let width = $state(0);
	let gesture = $state<Hit | null>(null);

	const geo = $derived<MapGeo>({ span: mapSpan(duration), width: Math.max(width, 1) });
	const view = $derived<View>({ scrollLeft, viewW, pxPerSec });
	const rect = $derived(windowRect(view, geo));
	const rows = $derived(rowLayout(tracks.length, HEIGHT));
	// Blocks depend on the cut and the strip's width only: scrolling and the
	// playhead never recompute them.
	const blocks = $derived(
		tracks.map((t) =>
			trackBlocks(
				t.clips.map((c) => ({ start: c.timeline_start, end: c.timeline_start + clipDuration(c) })),
				geo
			)
		)
	);
	const ranged = $derived(markIn !== null && markOut !== null && markOut > markIn);

	/** Where the pointer is on the strip, px. */
	const xOf = (e: PointerEvent) => (e.clientX - (strip?.getBoundingClientRect().left ?? 0));

	function onDown(e: PointerEvent) {
		if (e.button !== 0 || !strip) return;
		const g = geo;
		const start = view;
		const x = xOf(e);
		const r = windowRect(start, g);
		let hit = hitTest(x, r);
		// The view as the press found it: what Escape gives back.
		const home: Target = { scrollLeft: start.scrollLeft, zoom: start.pxPerSec };
		let grab = x - r.x;
		if (hit === 'outside') {
			// A press on the bare strip jumps the view there, and carries on as a drag
			// of the rectangle, held by its middle.
			onview(centerOn(start, g, x));
			hit = 'body';
			grab = r.w / 2;
		}
		gesture = hit;
		e.preventDefault();
		beginDrag(e, {
			move(ev) {
				const px = xOf(ev);
				if (hit === 'body') onview(moveTo(start, g, px - grab));
				else if (hit === 'left') onview(resizeLeft(start, g, px, duration));
				else if (hit === 'right') onview(resizeRight(start, g, px, duration));
			},
			commit() {
				gesture = null;
			},
			abandon() {
				gesture = null;
				onview(home);
			}
		});
	}

	function onDouble(e: MouseEvent) {
		const x = e.clientX - (strip?.getBoundingClientRect().left ?? 0);
		onseek(Math.min(Math.max(0, xToTime(x, geo)), duration));
	}

	/** The cursor over a position on the strip. */
	let cursor = $state('pointer');
	function onHover(e: PointerEvent) {
		if (gesture) return;
		const h = hitTest(xOf(e), rect);
		cursor = h === 'left' || h === 'right' ? 'ew-resize' : h === 'body' ? 'grab' : 'pointer';
	}
</script>

<div
	bind:this={strip}
	bind:clientWidth={width}
	role="presentation"
	title="Overview of the whole cut — drag the box to scroll, drag its edges to zoom, click to jump, double-click to move the playhead"
	onpointerdown={onDown}
	onpointermove={onHover}
	ondblclick={onDouble}
	style="position:relative;flex:1;min-width:0;height:{HEIGHT}px;overflow:hidden;background:var(--surface-inset);touch-action:none;user-select:none;cursor:{gesture ===
	'body'
		? 'grabbing'
		: gesture
			? 'ew-resize'
			: cursor}"
>
	{#each tracks as t, i (t.id)}
		{@const row = rows[i]}
		{#if row}
			{#each blocks[i] as b, k (k)}
				<span
					style="position:absolute;left:{b.x}px;top:{row.y}px;width:{b.w}px;height:{row.h}px;background:{t.kind ===
					'audio'
						? 'var(--track-audio-edge)'
						: 'var(--track-video-edge)'};opacity:{t.muted ? 0.35 : 0.9};{b.w >= 4
						? 'box-shadow:inset -1px 0 0 var(--surface-inset)'
						: ''}"
				></span>
			{/each}
		{/if}
	{/each}

	{#if ranged}
		<div
			style="position:absolute;left:{timeToX(markIn ?? 0, geo)}px;width:{timeToX((markOut ?? 0) - (markIn ?? 0), geo)}px;top:0;bottom:0;background:var(--selection-fill);pointer-events:none"
		></div>
	{/if}
	{#if markIn !== null}
		<span
			style="position:absolute;left:{timeToX(markIn, geo)}px;top:0;bottom:0;width:var(--line-emphasis);background:var(--kerf-400);pointer-events:none"
		></span>
	{/if}
	{#if markOut !== null}
		<span
			style="position:absolute;left:{timeToX(markOut, geo) - 1}px;top:0;bottom:0;width:var(--line-emphasis);background:var(--kerf-400);pointer-events:none"
		></span>
	{/if}

	<span
		style="position:absolute;left:{timeToX(time, geo)}px;top:0;bottom:0;width:var(--playhead-width);background:var(--playhead);pointer-events:none"
	></span>

	<!-- the view: the box is the part of the cut the timeline is showing -->
	<div
		data-minimap-window
		style="position:absolute;left:{rect.x}px;width:{rect.w}px;top:0;bottom:0;box-sizing:border-box;border:var(--line-emphasis) solid var(--kerf-400);border-radius:2px;background:color-mix(in srgb,var(--kerf-400) 14%,transparent);pointer-events:none"
	>
		{#if rect.w > EDGE_PX * 3}
			<span
				style="position:absolute;left:1px;top:50%;width:2px;height:12px;margin-top:-6px;border-radius:1px;background:var(--kerf-300)"
			></span>
			<span
				style="position:absolute;right:1px;top:50%;width:2px;height:12px;margin-top:-6px;border-radius:1px;background:var(--kerf-300)"
			></span>
		{/if}
	</div>
</div>
