<script lang="ts">
	/* One clip's waveform: a single <canvas> covering only the part of the clip
	 * that is on screen (plus overscan), drawn from the peak tiles
	 * `waveforms` fetches. The geometry is `waveform-view.ts`, the painting
	 * `waveform-draw.ts`; this is the glue.
	 *
	 * Two effects, one job each. The first says which tiles are needed — debounced,
	 * since scroll and zoom change it every tick. The second draws as soon as every
	 * tile it needs is in the cache; until then it leaves the previous bitmap where
	 * it was, positioned by the clip-local *time* it was drawn for rather than by
	 * pixels, so a zoom or a scroll in the meantime shows the old picture stretched
	 * or shifted into the right place instead of a blank. */
	import { untrack } from 'svelte';
	import type { Clip } from '$lib/types';
	import { effectiveGain } from '$lib/mixer';
	import { waveforms } from '$lib/waveforms';
	import { drawWaveform, type WavePalette } from '$lib/waveform-draw';
	import {
		bucketCells,
		canvasScale,
		capDpr,
		clipCanvasRect,
		columnPeaks,
		laneCount,
		secondsPerDevicePx,
		tilesForColumns,
		type TileData
	} from '$lib/waveform-view';

	let {
		clip,
		width,
		pxPerSec,
		dpr,
		viewLo,
		viewHi,
		trackVolume,
		liveVolume = null,
		duration,
		palette
	}: {
		clip: Clip;
		/** The clip's width on the timeline, css px. */
		width: number;
		pxPerSec: number;
		dpr: number;
		/** The lane-space range kept drawn (`visibleLaneRange`), css px. */
		viewLo: number;
		viewHi: number;
		/** The track fader the clip rides through. */
		trackVolume: number;
		/** The clip volume while its line is being dragged, before it is committed. */
		liveVolume?: number | null;
		/** The asset's audio length, seconds. */
		duration: number;
		palette: WavePalette;
	} = $props();

	/** Wait this long after the last scroll / zoom tick before asking for tiles, ms. */
	const WANT_DEBOUNCE = 60;

	let canvas = $state<HTMLCanvasElement | null>(null);
	let height = $state(0);
	/** Bumped when a tile arrives (or its asset fails): the draw re-checks. */
	let arrivals = $state(0);
	/** Bumped when a failed asset's cooldown ends: the tiles are asked for again. */
	let retries = $state(0);
	/** The clip-local seconds the canvas's bitmap was last drawn for. */
	let drawn = $state<{ t0: number; t1: number } | null>(null);
	let status = $state<'loading' | 'ready' | 'failed'>('loading');
	let failure = $state('');

	const ratio = $derived(capDpr(dpr));
	const rect = $derived(clipCanvasRect(clip.timeline_start * pxPerSec, width, viewLo, viewHi));
	const cells = $derived(bucketCells(secondsPerDevicePx(pxPerSec, ratio, clip.speed)));
	const scale = $derived(rect ? canvasScale(rect.x1 - rect.x0, ratio) : ratio);
	const columns = $derived(rect ? Math.max(1, Math.round((rect.x1 - rect.x0) * scale)) : 0);
	const tiles = $derived(
		rect ? tilesForColumns({ clip, pxPerSec, dpr: scale, x0: rect.x0, columns, cells, duration }) : []
	);
	const gain = $derived(effectiveGain(liveVolume ?? clip.volume, trackVolume));

	// ---- ask for what is needed ------------------------------------------------

	$effect(() => {
		void retries;
		const wanted = tiles;
		const asset = clip.asset_id;
		const owner = clip.id;
		if (wanted.length === 0) {
			// Off screen: no interest in any tile, so nothing queued for it ever runs.
			waveforms.release(owner);
			return;
		}
		if (wanted.every((t) => waveforms.get(asset, t))) return;
		const timer = setTimeout(() => waveforms.want(owner, asset, wanted, () => arrivals++), WANT_DEBOUNCE);
		return () => clearTimeout(timer);
	});

	// A clip that goes away (or is replaced) stops wanting its tiles.
	$effect(() => {
		const owner = clip.id;
		return () => waveforms.release(owner);
	});

	// ---- draw what is there ----------------------------------------------------

	$effect(() => {
		void arrivals;
		const el = canvas;
		const r = rect;
		const h = height;
		if (!el) return;
		if (!r) {
			// Scrolled out of range: a canvas keeps its full backing store (width x
			// height x 4 bytes, a few MB) for as long as it is alive, and a long pan
			// leaves one behind per clip. Hand it back.
			if (el.width > 1 || el.height > 1) {
				el.width = 1;
				el.height = 1;
			}
			if (untrack(() => drawn)) {
				drawn = null;
				status = 'loading';
			}
			return;
		}
		if (h <= 0) return;
		const asset = clip.asset_id;

		const have = new Map<number, TileData>();
		for (const t of tiles) {
			const data = waveforms.get(asset, t);
			if (data) have.set(t.index, data);
		}
		if (have.size < tiles.length) {
			const why = waveforms.failure(asset);
			if (why !== undefined) {
				status = 'failed';
				failure = why;
				// Try again when the cache stops holding the file off — which backs off the
				// longer it keeps failing, so a broken file is not asked for forever.
				const timer = setTimeout(() => retries++, (waveforms.retryIn(asset) ?? 0) + 50);
				return () => clearTimeout(timer);
			}
			// Still on its way: keep whatever bitmap there is.
			if (!untrack(() => drawn)) status = 'loading';
			return;
		}

		const channels = have.values().next().value?.channels ?? 1;
		const lanes = laneCount(channels, h);
		const cols = columnPeaks({
			clip,
			pxPerSec,
			dpr: scale,
			x0: r.x0,
			columns,
			cells,
			lanes,
			duration,
			tile: (index) => have.get(index)
		});
		const ctx = el.getContext('2d');
		if (!ctx) return;
		// Assigning a canvas's size reallocates its backing store even when it is the
		// same size — a volume drag redraws on every pointer move, so only on change.
		const pixelH = Math.max(1, Math.round(h * scale));
		if (el.width !== columns) el.width = columns;
		if (el.height !== pixelH) el.height = pixelH;
		drawWaveform(ctx, cols, { width: columns, height: pixelH, gain, dpr: scale, palette });
		drawn = { t0: r.x0 / pxPerSec, t1: (r.x0 + columns / scale) / pxPerSec };
		status = 'ready';
	});
</script>

<div
	aria-hidden="true"
	bind:clientHeight={height}
	style="position:absolute;left:calc(-1 * var(--bw, 0px));top:calc(-1 * var(--bw, 0px));bottom:calc(-1 * var(--bw, 0px));width:{width}px;pointer-events:none;overflow:hidden"
>
	{#if status !== 'ready'}
		<!-- Until the peaks are in (or when they cannot be read): a hatch, flat
		     for a file that failed, so a missing waveform is told apart from a
		     loading one. -->
		<div
			title={status === 'failed' ? `Waveform unavailable — ${failure}` : undefined}
			style="position:absolute;inset:0;background:repeating-linear-gradient(90deg, var(--waveform) 0 1px, transparent 1px 3px);opacity:{status ===
			'failed'
				? 0.12
				: 0.3};mask-image:linear-gradient(transparent 36%, var(--scrim) 36%, var(--scrim) 64%, transparent 64%)"
		></div>
	{/if}
	<canvas
		bind:this={canvas}
		style="position:absolute;top:0;height:100%;display:{drawn
			? 'block'
			: 'none'};left:{drawn ? drawn.t0 * pxPerSec : 0}px;width:{drawn ? (drawn.t1 - drawn.t0) * pxPerSec : 0}px;opacity:.9"
	></canvas>
</div>
