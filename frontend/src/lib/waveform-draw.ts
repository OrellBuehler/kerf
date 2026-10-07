/* Painting a clip's waveform onto a canvas, from the columns `waveform-view.ts`
 * works out. The shape is one filled polygon per lane — the upper edge through
 * every column's peak, back along the lower — not a line per sample, so there are
 * no hairline seams between columns and the cost is one fill however wide the clip
 * is. Columns at full scale are then painted over in the clip colour, one polygon
 * per run of them.
 *
 * Colours come in as resolved strings: a canvas cannot read `var(--waveform)`, so
 * `readPalette` asks the browser what the theme's tokens are *now*, once per theme
 * change, and nothing here has a colour literal. */

import { isClipped, type Columns } from './waveform-view';

/** The subset of `CanvasRenderingContext2D` the painting uses — so a test can hand
 *  in a recorder. */
export interface Ctx2D {
	fillStyle: string | CanvasGradient | CanvasPattern;
	clearRect(x: number, y: number, w: number, h: number): void;
	beginPath(): void;
	moveTo(x: number, y: number): void;
	lineTo(x: number, y: number): void;
	closePath(): void;
	fill(): void;
}

export interface WavePalette {
	/** The waveform. */
	wave: string;
	/** A clipped peak. */
	clip: string;
}

export interface DrawOptions {
	/** Canvas size, device pixels. */
	width: number;
	height: number;
	/** Linear gain applied to what is drawn: the clip's volume through the track fader. */
	gain: number;
	/** The ratio the canvas is drawn at, for the one-pixel minimums. */
	dpr: number;
	palette: WavePalette;
}

const clamp1 = (v: number) => Math.max(-1, Math.min(1, v));

/** What the browser resolves `var(--token)` to right now, as a colour string a
 *  canvas accepts. Reads through a throwaway element so a token that is itself an
 *  alias (`--danger: var(--red-500)`) or a `color-mix` still resolves. */
export function resolveColor(token: string, root: HTMLElement = document.documentElement): string {
	const probe = document.createElement('span');
	probe.style.display = 'none';
	probe.style.color = `var(${token})`;
	root.appendChild(probe);
	const color = getComputedStyle(probe).color;
	probe.remove();
	return color;
}

/** The two colours a waveform is drawn in, from the active theme. */
export function readPalette(root: HTMLElement = document.documentElement): WavePalette {
	return { wave: resolveColor('--waveform', root), clip: resolveColor('--danger', root) };
}

/** Where every column's polygon edges are, in canvas pixels, for one lane. */
function laneEdges(cols: Columns, lane: number, o: DrawOptions): { top: Float32Array; bottom: Float32Array } {
	const laneH = o.height / cols.lanes;
	const centre = lane * laneH + laneH / 2;
	const pad = Math.max(1, Math.round(o.dpr));
	const half = Math.max(1, laneH / 2 - pad);
	const top = new Float32Array(cols.count);
	const bottom = new Float32Array(cols.count);
	const floor = Math.max(1, o.dpr); // a silent stretch still draws a line
	for (let d = 0; d < cols.count; d++) {
		let t = centre - clamp1(cols.max[lane][d] * o.gain) * half;
		let b = centre - clamp1(cols.min[lane][d] * o.gain) * half;
		if (b - t < floor) {
			const mid = (t + b) / 2;
			t = mid - floor / 2;
			b = mid + floor / 2;
		}
		top[d] = t;
		bottom[d] = b;
	}
	return { top, bottom };
}

/** One polygon over columns `[from, to)`: along the tops, back along the bottoms. */
function fillRun(ctx: Ctx2D, top: Float32Array, bottom: Float32Array, from: number, to: number) {
	ctx.beginPath();
	ctx.moveTo(from, top[from]);
	for (let d = from; d < to; d++) ctx.lineTo(d + 0.5, top[d]);
	ctx.lineTo(to, top[to - 1]);
	ctx.lineTo(to, bottom[to - 1]);
	for (let d = to - 1; d >= from; d--) ctx.lineTo(d + 0.5, bottom[d]);
	ctx.lineTo(from, bottom[from]);
	ctx.closePath();
	ctx.fill();
}

/**
 * Paint `cols` onto `ctx`: each lane's waveform in the wave colour, scaled by
 * `gain` (and held inside the lane — a boosted clip peaks at the lane's edge, not
 * past it), and the columns whose peak is clipped over it in the clip colour.
 * Returns how many columns were clipped.
 */
export function drawWaveform(ctx: Ctx2D, cols: Columns, o: DrawOptions): number {
	ctx.clearRect(0, 0, o.width, o.height);
	if (cols.count === 0) return 0;
	let clippedColumns = 0;
	for (let lane = 0; lane < cols.lanes; lane++) {
		const { top, bottom } = laneEdges(cols, lane, o);
		ctx.fillStyle = o.palette.wave;
		fillRun(ctx, top, bottom, 0, cols.count);

		ctx.fillStyle = o.palette.clip;
		let start = -1;
		for (let d = 0; d <= cols.count; d++) {
			const clipped = d < cols.count && isClipped(cols.min[lane][d], cols.max[lane][d], o.gain);
			if (clipped && start < 0) start = d;
			if (!clipped && start >= 0) {
				fillRun(ctx, top, bottom, start, d);
				clippedColumns += d - start;
				start = -1;
			}
		}
	}
	return clippedColumns;
}
