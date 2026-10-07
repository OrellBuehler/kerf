/* The geometry of a clip's waveform: which stretch of the source is on screen,
 * which pieces of the backend's peak pyramid it takes to draw it, and what each
 * device pixel column of the canvas is made of. Pure — `waveform-cache.ts` owns
 * the fetching and `waveform-draw.ts` the painting, so this is what the tests pin.
 *
 * A clip's canvas covers only the on-screen part of the clip (plus some
 * overscan), not the clip: at 96 px/s a one-hour clip is 345 600 px wide, and no
 * canvas holds that. What the canvas draws is read from fixed **tiles** of the
 * source — `TILE_BUCKETS` peak pairs each, aligned to the source's own clock
 * rather than to the clip — so scrolling, a trim, or a split (two clips of one
 * asset) all land on tiles that are already cached.
 *
 * Source time and timeline pixels meet in `sourceAt`: forward, a clip's pixel
 * `x` shows source `source_in + x / pxPerSec * |speed|`; reversed, it counts down
 * from `source_out`, which is all "a reversed clip draws mirrored" is — the
 * columns are read through the mapping, the data is never flipped. */

import type { WaveformRange } from './types';

/** The finest stored level is 500 buckets a second: a "cell" is its 2 ms bucket. */
export const CELL_RATE = 500;

/** Bucket widths of the backend's stored levels (500 / 100 / 25 / 10 a second), in cells. */
export const LEVEL_CELLS = [1, 5, 20, 50] as const;

/** Peak pairs in one tile. Half the backend's 4096 cap; at about a bucket a pixel
 *  that is a tile per ~2000 device px, so a viewport needs a handful of requests
 *  and a scroll one every screen or so. */
export const TILE_BUCKETS = 2048;

/** Highest device pixel ratio a waveform is drawn at: a retina canvas costs 4x
 *  the memory of a plain one and the shape is no more informative. */
export const MAX_DPR = 2;

/** Widest a clip's canvas gets, device pixels. Past this the ratio is lowered. */
export const MAX_CANVAS_PX = 16384;

/** A lane shorter than this draws one folded waveform; a stereo clip needs at
 *  least this much height (px) to give each channel a lane worth reading. */
export const STEREO_MIN_HEIGHT = 48;

/** Peaks at or past this read as clipped. The engine stores a clipped sample as
 *  exactly +-1.0, and 16-bit rounding of a near-miss lands just under it. */
export const CLIP_PEAK = 0.999;

/** A bucket may be this much wider than a device pixel before the next finer
 *  rung is used — bigger steps mean fewer, larger tiles; at 1.5 the aggregation
 *  into pixel columns is still a straight min/max. */
export const RUNG_SLACK = 1.5;

/** Spare room drawn either side of the viewport, as a fraction of its width. */
export const OVERSCAN = 0.5;

/** The viewport edge snaps out to a multiple of this (css px), so scrolling
 *  re-lays the canvases out a screen-quarter at a time rather than per pixel. */
export const VIEW_QUANT = 256;

/** The subset of a clip the mapping reads. */
export interface ClipWindow {
	source_in: number;
	source_out: number;
	speed?: number;
}

/** Playback speed's magnitude, clamped away from zero like the engine's `speed_mag`. */
export const speedMag = (c: { speed?: number }): number => Math.max(Math.abs(c.speed ?? 1), 0.01);

export const isReversed = (c: { speed?: number }): boolean => (c.speed ?? 1) < 0;

/** A device pixel ratio as drawn: at least 1 (a zoomed-out page is not worth a
 *  sub-pixel canvas), at most `MAX_DPR`. */
export const capDpr = (dpr: number): number => (Number.isFinite(dpr) && dpr > 1 ? Math.min(dpr, MAX_DPR) : 1);

/** Source seconds under one device pixel of a clip at `pxPerSec`. */
export const secondsPerDevicePx = (pxPerSec: number, dpr: number, speed?: number): number =>
	speedMag({ speed }) / (Math.max(pxPerSec, 1e-6) * capDpr(dpr));

// ---- timeline pixels <-> source seconds --------------------------------------

/** The source second shown at `px` pixels from the clip's left edge. */
export function sourceAt(c: ClipWindow, px: number, pxPerSec: number): number {
	const t = (px / pxPerSec) * speedMag(c);
	return isReversed(c) ? c.source_out - t : c.source_in + t;
}

/** The pixel (from the clip's left edge) where source second `src` is shown. */
export function pxAtSource(c: ClipWindow, src: number, pxPerSec: number): number {
	const t = (isReversed(c) ? c.source_out - src : src - c.source_in) / speedMag(c);
	return t * pxPerSec;
}

/** The source span `[a, b]` (ascending, whichever way the clip plays) that the
 *  clip-local pixels `[x0, x1]` show. */
export function sourceWindow(c: ClipWindow, x0: number, x1: number, pxPerSec: number): [number, number] {
	const a = sourceAt(c, x0, pxPerSec);
	const b = sourceAt(c, x1, pxPerSec);
	return a <= b ? [a, b] : [b, a];
}

// ---- which part of a clip is on screen ---------------------------------------

/**
 * The lane-space range `[lo, hi]` (css px) to keep drawn: the viewport, plus
 * `OVERSCAN` of its width each side, widened out to a multiple of `quant`. It only
 * changes when the viewport has moved far enough to matter.
 */
export function visibleLaneRange(
	scrollX: number,
	viewW: number,
	quant = VIEW_QUANT,
	overscan = OVERSCAN
): { lo: number; hi: number } {
	const w = Math.max(1, viewW);
	return {
		lo: Math.max(0, Math.floor((scrollX - w * overscan) / quant) * quant),
		hi: Math.max(quant, Math.ceil((scrollX + w * (1 + overscan)) / quant) * quant)
	};
}

/** The part of a clip `[clipLeft, clipLeft + clipWidth]` (lane px) that falls in
 *  `[lo, hi]`, as whole clip-local pixels — or null when none of it does. */
export function clipCanvasRect(
	clipLeft: number,
	clipWidth: number,
	lo: number,
	hi: number
): { x0: number; x1: number } | null {
	const x0 = Math.max(0, Math.floor(lo - clipLeft));
	const x1 = Math.min(Math.ceil(clipWidth), Math.ceil(hi - clipLeft));
	return x1 > x0 ? { x0, x1 } : null;
}

/** The ratio a canvas `widthCss` wide is drawn at: `dpr`, lowered only when it
 *  would pass `MAX_CANVAS_PX`. */
export function canvasScale(widthCss: number, dpr: number): number {
	const d = capDpr(dpr);
	return widthCss * d > MAX_CANVAS_PX ? Math.max(1, MAX_CANVAS_PX / Math.max(1, widthCss)) : d;
}

/** One lane for mono, or a stereo clip on a short track; two when the clip is
 *  stereo and `heightPx` (the lane's pixel height) leaves each channel room. */
export const laneCount = (channels: number, heightPx: number): 1 | 2 =>
	channels >= 2 && heightPx >= STEREO_MIN_HEIGHT ? 2 : 1;

// ---- tiles --------------------------------------------------------------------

/** Bucket widths in cells: the stored levels, then doubling — so a bucket is
 *  always a whole number of the backend's own buckets and never straddles two. */
export const RUNGS: readonly number[] = [...LEVEL_CELLS, ...Array.from({ length: 12 }, (_, i) => 100 * 2 ** i)];

/** The bucket width (cells) to draw a clip at: the widest rung that is at most
 *  `RUNG_SLACK` device pixels, so every pixel column holds at least one bucket. */
export function bucketCells(secPerDevicePx: number): number {
	const cap = secPerDevicePx * CELL_RATE * RUNG_SLACK;
	let best = RUNGS[0];
	for (const r of RUNGS) if (r <= cap + 1e-9) best = r;
	return best;
}

/** One cache-able request: `buckets` peak pairs over `[start, end)` source seconds. */
export interface TileSpec {
	cells: number;
	index: number;
	start: number;
	end: number;
	buckets: number;
}

/** Seconds of source one tile of `cells`-wide buckets covers. */
export const tileSeconds = (cells: number): number => (TILE_BUCKETS * cells) / CELL_RATE;

/** Tile `index` of the `cells` grid. Each edge is a single division of an
 *  integer, so the same tile is the same pair of doubles however it was asked for. */
export function tileSpec(cells: number, index: number): TileSpec {
	return {
		cells,
		index,
		start: (index * TILE_BUCKETS * cells) / CELL_RATE,
		end: ((index + 1) * TILE_BUCKETS * cells) / CELL_RATE,
		buckets: TILE_BUCKETS
	};
}

/** The tiles that cover source `[a, b]` of an asset `duration` long (nothing past its end). */
export function tilesFor(a: number, b: number, cells: number, duration: number): TileSpec[] {
	if (!(b > a) || !(duration > 0) || a >= duration) return [];
	const span = tileSeconds(cells);
	const first = Math.max(0, Math.floor(a / span));
	const last = Math.ceil(Math.min(b, duration) / span) - 1;
	const out: TileSpec[] = [];
	for (let i = first; i <= last; i++) out.push(tileSpec(cells, i));
	return out;
}

/** The tiles `columnPeaks` will read for a canvas `columns` device pixels wide
 *  starting `x0` css px into the clip — with a bucket of margin either side, since
 *  the last column can reach a hair past the pixel it was sized from. */
export function tilesForColumns(a: Omit<ColumnArgs, 'lanes' | 'tile'>): TileSpec[] {
	const bw = a.cells / CELL_RATE;
	const [lo, hi] = sourceWindow(a.clip, a.x0, a.x0 + a.columns / a.dpr, a.pxPerSec);
	return tilesFor(lo - bw, hi + bw, a.cells, a.duration);
}

/** The cache key of a tile: the asset, the source window and the bucket count. */
export const tileKey = (assetId: string, t: Pick<TileSpec, 'start' | 'end' | 'buckets'>): string =>
	`${assetId}:${t.start.toFixed(6)}:${t.end.toFixed(6)}:${t.buckets}`;

/** A tile's peaks, held as `Float32Array`s — a fraction of the memory of the
 *  JSON arrays they came in, and exact for the engine's four-place values. */
export interface TileData {
	channels: number;
	buckets: number;
	min: Float32Array[];
	max: Float32Array[];
}

export function tileData(r: WaveformRange): TileData {
	return {
		channels: r.channels,
		buckets: r.buckets,
		min: r.min.map((lane) => Float32Array.from(lane)),
		max: r.max.map((lane) => Float32Array.from(lane))
	};
}

// ---- pixel columns ------------------------------------------------------------

/** What the canvas draws: for each device pixel column, each lane's extremes. */
export interface Columns {
	count: number;
	lanes: number;
	/** `[lane][column]`, -1..1 before gain. */
	min: Float32Array[];
	max: Float32Array[];
	/** Columns that needed a tile that is not loaded (the drawing waits for 0). */
	missing: number;
}

export interface ColumnArgs {
	clip: ClipWindow;
	pxPerSec: number;
	/** The ratio the canvas is drawn at (already capped). */
	dpr: number;
	/** Clip-local css px of the canvas's left edge. */
	x0: number;
	/** Device pixels across. */
	columns: number;
	cells: number;
	/** Lanes drawn; 1 folds a stereo asset's channels into one. */
	lanes: number;
	/** The asset's audio length, seconds: buckets past it are silence, not missing. */
	duration: number;
	tile: (index: number) => TileData | undefined;
}

/**
 * Each device pixel column's min/max per lane, read through the clip's mapping.
 *
 * Buckets are *partitioned* among columns by where each one begins — the rule the
 * backend uses when it folds a level into a window — so a peak lands in exactly
 * one column instead of smearing across two; a column finer than a bucket reads
 * the bucket under its midpoint. A column over a tile that is not loaded counts as
 * `missing`; one past the end of the media is silence.
 */
export function columnPeaks(a: ColumnArgs): Columns {
	const { clip, pxPerSec, dpr, x0, columns, cells, lanes, duration } = a;
	const bw = cells / CELL_RATE;
	const total = Math.ceil(duration / bw - 1e-9);
	const out: Columns = {
		count: columns,
		lanes,
		min: Array.from({ length: lanes }, () => new Float32Array(columns)),
		max: Array.from({ length: lanes }, () => new Float32Array(columns)),
		missing: 0
	};
	const lo = new Float64Array(lanes);
	const hi = new Float64Array(lanes);
	for (let d = 0; d < columns; d++) {
		const [sa, sb] = sourceWindow(clip, x0 + d / dpr, x0 + (d + 1) / dpr, pxPerSec);
		let k0 = Math.ceil(sa / bw - 1e-6);
		let k1 = Math.ceil(sb / bw - 1e-6);
		if (k1 <= k0) {
			k0 = Math.floor(((sa + sb) / 2) / bw);
			k1 = k0 + 1;
		}
		k0 = Math.max(0, k0);
		k1 = Math.min(total, k1);
		lo.fill(Infinity);
		hi.fill(-Infinity);
		let absent = false;
		for (let k = k0; k < k1 && !absent; k++) {
			const index = Math.floor(k / TILE_BUCKETS);
			const tile = a.tile(index);
			if (!tile) {
				absent = true;
				break;
			}
			const j = k - index * TILE_BUCKETS;
			for (let c = 0; c < tile.channels; c++) {
				const lane = lanes === 1 ? 0 : Math.min(c, lanes - 1);
				const mn = tile.min[c][j];
				const mx = tile.max[c][j];
				if (mn < lo[lane]) lo[lane] = mn;
				if (mx > hi[lane]) hi[lane] = mx;
			}
		}
		if (absent) {
			out.missing++;
			continue;
		}
		for (let l = 0; l < lanes; l++) {
			// nothing under the column (past the media): silence
			out.min[l][d] = lo[l] === Infinity ? 0 : lo[l];
			out.max[l][d] = hi[l] === -Infinity ? 0 : hi[l];
		}
	}
	return out;
}

/** Whether a column's peak is clipped: the source itself hits full scale (that
 *  distortion is baked in whatever the level), or the gain pushes it there. */
export function isClipped(minV: number, maxV: number, gain: number): boolean {
	const peak = Math.max(maxV, -minV);
	return peak >= CLIP_PEAK || peak * gain >= CLIP_PEAK;
}
