<script lang="ts">
	/* A long lane's narrow clips (under `BLOCK_MAX_PX` in view: no label, thumbnail or waveform to read
	 * on any) as blocks on one canvas instead of a button each (`timeline-cull.ts`). The canvas covers
	 * the lane's visible window only, so its backing store stays a screen wide however long the cut,
	 * and draws what a button would show of its clip: faded while disabled or moved by a gesture,
	 * grey with the clip off, its edge in the mark's colour (`blockMark`: the selection ring, a staged
	 * proposal's add or shift, a clip the proposal touches, a partner of the clip under the pointer).
	 * It is not a place to edit: the block under the pointer is promoted to a real clip (`onhover`),
	 * which then takes the press as any clip does; a press or a right-click that comes before any
	 * hover (a pen, a touch) is handed to the lane's own clip handlers (`onpress`, `oncontext`), and
	 * a press where no block is is the lane's own (`pressOnEmpty`): a marquee or a seek. */
	import type { Clip } from '#lib/types';
	import { blockAt, clipBox, type BlockMark } from '#lib/timeline-cull';
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
		marks,
		mark,
		faded,
		off,
		filter,
		cursor,
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
		/** The edge colour of each mark, resolved the same way. */
		marks: Record<Exclude<BlockMark, null>, string>;
		mark: (c: Clip) => BlockMark;
		/** A clip drawn faded, as its button would be (disabled, or moved by a gesture). */
		faded: (c: Clip) => boolean;
		/** A clip drawn grey (disabled). */
		off: (c: Clip) => boolean;
		/** The lane's own filter (a locked lane's desaturation, a silent track's grey). */
		filter: string;
		/** The cursor over a block: the lane's clip cursor. */
		cursor: string;
		onhover: (e: PointerEvent, id: string | null) => void;
		onpress: (e: PointerEvent, c: Clip) => void;
		oncontext: (e: MouseEvent, c: Clip) => void;
	} = $props();

	let canvas = $state<HTMLCanvasElement | null>(null);
	let height = $state(0);
	/** Whether the pointer is over a block (the cursor says so). */
	let over = $state(false);
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
			const m = mark(c);
			ctx.globalAlpha = faded(c) ? 0.4 : 1;
			ctx.filter = off(c) ? 'grayscale(1)' : 'none';
			ctx.fillStyle = m ? marks[m] : edge;
			ctx.fillRect(x0, 0, x1 - x0, h);
			const ring = m ? 2 * line : line;
			if (x1 - x0 > 2 * ring) {
				ctx.fillStyle = fill;
				ctx.fillRect(x0 + ring, ring, x1 - x0 - 2 * ring, h - 2 * ring);
			}
		}
		ctx.globalAlpha = 1;
		ctx.filter = 'none';
	});

	const at = (e: MouseEvent): Clip | null => blockAt(blocks, duration, pxPerSec, lo + e.offsetX);
</script>

<canvas
	bind:this={canvas}
	bind:clientHeight={height}
	aria-hidden="true"
	data-lane-blocks
	style="position:absolute;left:{lo}px;top:5px;height:calc(100% - 10px);width:{widthCss}px;touch-action:none;cursor:{over ? cursor : 'default'};filter:{filter}"
	onpointermove={(e) => {
		const c = at(e);
		over = !!c;
		onhover(e, c?.id ?? null);
	}}
	onpointerleave={(e) => {
		over = false;
		const to = e.relatedTarget as Element | null;
		if (!to?.closest?.('.kclip')) onhover(e, null);
	}}
	onpointerdown={(e) => {
		const c = at(e);
		if (c) onpress(e, c);
	}}
	onclick={(e) => {
		// A press on a block was a clip's; the lane under it must not take the click as a seek.
		if (at(e)) e.stopPropagation();
	}}
	oncontextmenu={(e) => {
		const c = at(e);
		if (c) oncontext(e, c);
	}}
></canvas>
