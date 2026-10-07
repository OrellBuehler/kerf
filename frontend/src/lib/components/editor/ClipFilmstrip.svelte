<script lang="ts">
	/* One video clip's thumbnails: a single <canvas> covering only the part of the
	 * clip that is on screen (plus overscan), blitted from the decoded sheets
	 * `filmstrips` holds. The geometry is `filmstrip-view.ts` (which thumbnail in
	 * which slot, through trim / speed / reverse), the painting `filmstrip-draw.ts`;
	 * this is the glue, and it follows `ClipWaveform.svelte`'s shape — the same
	 * windowing (`clipCanvasRect`), the same size discipline.
	 *
	 * One effect does it all. It asks for the asset's strip when the clip is on
	 * screen and tall enough to show thumbnails (a compact track never fetches
	 * one), and draws as soon as the strip is held. Drawing is synchronous — the
	 * sheets are already decoded — so unlike the waveform there is no stale bitmap
	 * to place by time: until the strip arrives the canvas is simply hidden and the
	 * clip keeps its plain look. A clip scrolled out of range, or a track shrunk to
	 * compact, releases its interest and shrinks the canvas to 1x1 (a canvas keeps
	 * its whole backing store otherwise); a redraw only assigns the canvas size when
	 * it changed, since that reallocates the backing store even for the same size. */
	import { untrack } from 'svelte';
	import type { Clip } from '$lib/types';
	import { drawFilmstrip } from '$lib/filmstrip-draw';
	import { filmVisible, planSlots } from '$lib/filmstrip-view';
	import { filmstrips } from '$lib/filmstrips';
	import { canvasScale, capDpr, clipCanvasRect } from '$lib/waveform-view';

	let {
		clip,
		width,
		pxPerSec,
		dpr,
		viewLo,
		viewHi,
		onready
	}: {
		clip: Clip;
		/** The clip's width on the timeline, css px. */
		width: number;
		pxPerSec: number;
		dpr: number;
		/** The lane-space range kept drawn (`visibleLaneRange`), css px. */
		viewLo: number;
		viewHi: number;
		/** Told when the thumbnails are on the clip (and when they are not), so the
		 *  clip can make its label legible over a picture only while there is one. */
		onready?: (ready: boolean) => void;
	} = $props();

	let canvas = $state<HTMLCanvasElement | null>(null);
	let height = $state(0);
	/** Bumped when the strip arrives (or its asset fails): the draw re-checks. */
	let arrivals = $state(0);
	/** Bumped when a failed asset's cooldown ends: the strip is asked for again. */
	let retries = $state(0);
	/** Clip-local css px the canvas's bitmap covers; null while nothing is drawn. */
	let placed = $state<{ x0: number; w: number } | null>(null);
	let announced = false;

	const ratio = $derived(capDpr(dpr));
	const rect = $derived(clipCanvasRect(clip.timeline_start * pxPerSec, width, viewLo, viewHi));
	const scale = $derived(rect ? canvasScale(rect.x1 - rect.x0, ratio) : ratio);
	const columns = $derived(rect ? Math.max(1, Math.round((rect.x1 - rect.x0) * scale)) : 0);

	/** The cache calls this when the strip arrives, fails, or is dropped from under us. */
	const notify = () => arrivals++;

	function announce(ready: boolean) {
		if (ready === announced) return;
		announced = ready;
		onready?.(ready);
	}

	$effect(() => {
		void arrivals;
		void retries;
		const el = canvas;
		const r = rect;
		const h = height;
		const asset = clip.asset_id;
		const owner = clip.id;
		if (!el) return;
		if (!r || !filmVisible(h)) {
			// Off screen, or too short for a picture: no interest in the strip (a queued
			// load nobody wants never runs), and hand the backing store back.
			filmstrips.release(owner);
			if (el.width > 1 || el.height > 1) {
				el.width = 1;
				el.height = 1;
			}
			if (untrack(() => placed)) placed = null;
			announce(false);
			return;
		}
		const loaded = filmstrips.get(asset);
		if (!loaded) {
			announce(false);
			if (untrack(() => placed)) placed = null;
			const why = filmstrips.failure(asset);
			if (why !== undefined) {
				// The clip keeps its plain look. Try again when the cache stops holding
				// the file off — which backs off the longer it keeps failing.
				const timer = setTimeout(() => retries++, (filmstrips.retryIn(asset) ?? 0) + 50);
				return () => clearTimeout(timer);
			}
			// Still on its way (or evicted since): ask again — cheap, and it replaces
			// whatever this clip asked for before.
			filmstrips.want(owner, asset, notify);
			return;
		}
		// Drawing from it: the cache must not evict it from under a visible clip (and
		// tells us if it is dropped anyway, so this canvas redraws instead of going stale).
		filmstrips.hold(owner, asset, notify);
		const ctx = el.getContext('2d');
		if (!ctx) return;
		// Assigning a canvas's size reallocates its backing store even when it is the
		// same size, so only on change.
		const pixelH = Math.max(1, Math.round(h * scale));
		if (el.width !== columns) el.width = columns;
		if (el.height !== pixelH) el.height = pixelH;
		const slots = planSlots({
			strip: loaded.strip,
			clip,
			pxPerSec,
			clipWidth: width,
			heightPx: h,
			x0: r.x0,
			columns,
			scale
		});
		drawFilmstrip(ctx, slots, {
			width: columns,
			height: pixelH,
			strip: loaded.strip,
			sheet: (i) => loaded.images[i]
		});
		const next = { x0: r.x0, w: columns / scale };
		const prev = untrack(() => placed);
		if (!prev || prev.x0 !== next.x0 || prev.w !== next.w) placed = next;
		announce(true);
	});

	// A clip that goes away (or is replaced) stops wanting its strip, and stops
	// claiming a picture.
	$effect(() => {
		const owner = clip.id;
		return () => {
			filmstrips.release(owner);
			announce(false);
		};
	});
</script>

<!-- Inside the clip's border (unlike the waveform, which a thin trace can cross
     the border with): a picture over the border would hide the selection outline. -->
<div
	aria-hidden="true"
	bind:clientHeight={height}
	style="position:absolute;inset:0;pointer-events:none;overflow:hidden"
>
	<canvas
		bind:this={canvas}
		style="position:absolute;top:0;height:100%;display:{placed ? 'block' : 'none'};left:calc({placed?.x0 ?? 0}px - var(--bw, 0px));width:{placed?.w ?? 0}px"
	></canvas>
</div>
