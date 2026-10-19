<script lang="ts">
	/* A long lane's clips, zoomed out past reading (more than `BLOCK_MIN_CLIPS` in view, each a few
	 * pixels wide), as blocks on one canvas instead of a button each (`timeline-cull.ts`). The canvas
	 * covers the lane's visible window only, so its backing store stays a screen wide however long
	 * the cut. It is not a place to edit: the block under the pointer is promoted to a real clip
	 * (`onhover`), which then takes the press as any clip does; a press or a right-click that comes
	 * before any hover (a pen, a touch) is handed to the lane's own clip handlers (`onpress`,
	 * `oncontext`). */
	import type { Clip } from '#lib/types';
	import { blockAt, clipBox } from '#lib/timeline-cull';
	import { canvasScale } from '#lib/waveform-view';

	let {
		blocks,
		duration,
		pxPerSec,
		lo,
		hi,
		dpr,
		fill,
		edge,
		dim,
		onhover,
		onpress,
		oncontext
	}: {
		/** The clips drawn here, in lane order (a later one over an earlier one where they overlap). */
		blocks: readonly Clip[];
		duration: (c: Clip) => number;
		pxPerSec: number;
		/** The lane-space window drawn (`visibleLaneRange`), css px. */
		lo: number;
		hi: number;
		dpr: number;
		/** The track's clip colours, resolved from the theme (a canvas cannot read `var(--…)`). */
		fill: string;
		edge: string;
		/** A clip drawn faded (disabled), as its button would be. */
		dim: (c: Clip) => boolean;
		onhover: (id: string | null) => void;
		onpress: (e: PointerEvent, c: Clip) => void;
		oncontext: (e: MouseEvent, c: Clip) => void;
	} = $props();

	let canvas = $state<HTMLCanvasElement | null>(null);
	let height = $state(0);
	const widthCss = $derived(Math.max(1, hi - lo));

	$effect(() => {
		const el = canvas;
		if (!el || height <= 0) return;
		const scale = canvasScale(widthCss, dpr);
		const w = Math.max(1, Math.round(widthCss * scale));
		const h = Math.max(1, Math.round(height * scale));
		if (el.width !== w) el.width = w;
		if (el.height !== h) el.height = h;
		const ctx = el.getContext('2d');
		if (!ctx) return;
		ctx.clearRect(0, 0, w, h);
		const line = Math.max(1, Math.round(scale));
		for (const c of blocks) {
			const box = clipBox(c, duration(c), pxPerSec);
			const x0 = Math.round((Math.max(box.left, lo) - lo) * scale);
			const x1 = Math.round((Math.min(box.right, hi) - lo) * scale);
			if (x1 <= x0) continue;
			ctx.globalAlpha = dim(c) ? 0.4 : 1;
			ctx.fillStyle = edge;
			ctx.fillRect(x0, 0, x1 - x0, h);
			if (x1 - x0 > 2 * line) {
				ctx.fillStyle = fill;
				ctx.fillRect(x0 + line, line, x1 - x0 - 2 * line, h - 2 * line);
			}
		}
		ctx.globalAlpha = 1;
	});

	const at = (e: MouseEvent): Clip | null => blockAt(blocks, duration, pxPerSec, lo + e.offsetX);
</script>

<canvas
	bind:this={canvas}
	bind:clientHeight={height}
	aria-hidden="true"
	data-lane-blocks
	style="position:absolute;left:{lo}px;top:5px;height:calc(100% - 10px);width:{widthCss}px;touch-action:none"
	onpointermove={(e) => onhover(at(e)?.id ?? null)}
	onpointerdown={(e) => {
		const c = at(e);
		if (c) onpress(e, c);
	}}
	oncontextmenu={(e) => {
		const c = at(e);
		if (c) oncontext(e, c);
	}}
></canvas>
