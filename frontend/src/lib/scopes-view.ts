/* What the Scopes panel draws, as numbers: how a count grid becomes pixels, how the readout words
 * the summary, and the browser harness's stand-in (colour bars). Pure, so it is tested without a
 * canvas; the panel only resolves the theme's colours and puts the pixels on screen.
 *
 * Grids are `kerf_core::scopes`: `counts[row * columns + column]`, row 0 the lowest level. */

import type { ScopeGrid, ScopeGridSize, ScopeHistogram, ScopeKind, ScopeMatrix, ScopeSummary, Scopes } from './types';

export const SCOPE_KINDS: { id: ScopeKind; label: string }[] = [
	{ id: 'waveform', label: 'Waveform' },
	{ id: 'parade', label: 'RGB parade' },
	{ id: 'vectorscope', label: 'Vectorscope' },
	{ id: 'histogram', label: 'Histogram' }
];

export function isScopeKind(v: unknown): v is ScopeKind {
	return SCOPE_KINDS.some((k) => k.id === v);
}

/** What the panel asks for: fine enough for a panel a few hundred pixels tall. */
export const PANEL_GRID: ScopeGridSize = { columns: 256, rows: 192, vector: 160 };

/** Half the vectorscope's side in Cb / Cr units (`VECTOR_EXTENT`). */
export const VECTOR_EXTENT = 0.6;

export type Rgb = [number, number, number];

/** A pixel buffer the grids are painted into (an `ImageData`'s shape). */
export interface Pixels {
	width: number;
	height: number;
	data: Uint8ClampedArray;
}

export function blankPixels(width: number, height: number): Pixels {
	return { width, height, data: new Uint8ClampedArray(Math.max(0, width * height * 4)) };
}

/** How bright a cell of `count` reads when the densest holds `max`: a log curve, so a few samples
 *  still show beside a flat field, and a hit is never invisible. */
export function density(count: number, max: number): number {
	if (count <= 0 || max <= 0) return 0;
	return Math.min(1, 0.25 + (0.75 * Math.log1p(count)) / Math.log1p(max));
}

/** Paint `grid` in `color` over the box `x0..x0+w`, `0..pixels.height` of `pixels`, adding to what
 *  is there (so a parade's channels, or anything drawn over, stack). Row 0 is the bottom. */
export function paintGrid(pixels: Pixels, grid: ScopeGrid, color: Rgb, x0 = 0, w = pixels.width): void {
	const { columns, rows, counts } = grid;
	if (columns <= 0 || rows <= 0 || w <= 0 || pixels.height <= 0) return;
	let max = 0;
	for (const c of counts) if (c > max) max = c;
	if (max === 0) return;
	const h = pixels.height;
	for (let py = 0; py < h; py++) {
		const row = Math.min(rows - 1, Math.floor(((h - 1 - py) * rows) / h));
		for (let px = 0; px < w; px++) {
			const col = Math.min(columns - 1, Math.floor((px * columns) / w));
			const a = density(counts[row * columns + col] ?? 0, max);
			if (a === 0) continue;
			const i = (py * pixels.width + x0 + px) * 4;
			pixels.data[i] = Math.min(255, pixels.data[i] + color[0] * a);
			pixels.data[i + 1] = Math.min(255, pixels.data[i + 1] + color[1] * a);
			pixels.data[i + 2] = Math.min(255, pixels.data[i + 2] + color[2] * a);
			pixels.data[i + 3] = 255;
		}
	}
}

/** A histogram's bins as bar heights 0..1 against the tallest bin of all the channels drawn. */
export function histogramHeights(h: ScopeHistogram, channels: (keyof ScopeHistogram)[]): Record<string, number[]> {
	let max = 0;
	for (const c of channels) for (const v of h[c]) if (v > max) max = v;
	const out: Record<string, number[]> = {};
	for (const c of channels) out[c] = h[c].map((v) => (max > 0 ? Math.sqrt(v / max) : 0));
	return out;
}

/** Where (Cb, Cr) lands in a square vectorscope of side `size`, pixels from its top-left. */
export function vectorPoint(cb: number, cr: number, size: number): { x: number; y: number } {
	const t = (v: number) => (v + VECTOR_EXTENT) / (2 * VECTOR_EXTENT);
	return { x: t(cb) * size, y: (1 - t(cr)) * size };
}

const WEIGHTS: Record<Exclude<ScopeMatrix, 'auto'>, [number, number]> = {
	bt601: [0.299, 0.114],
	bt709: [0.2126, 0.0722],
	bt2020: [0.2627, 0.0593]
};

/** Y', Cb, Cr of normalized R'G'B' (`kerf_core::scopes::ycbcr`). */
export function ycbcr(matrix: Exclude<ScopeMatrix, 'auto'>, r: number, g: number, b: number): [number, number, number] {
	const [kr, kb] = WEIGHTS[matrix];
	const y = kr * r + (1 - kr - kb) * g + kb * b;
	return [y, (b - y) / (2 * (1 - kb)), (r - y) / (2 * (1 - kr))];
}

/** The 75% primaries and secondaries a vectorscope marks its targets at, as R'G'B' 0..1. */
export const TARGETS: { name: string; rgb: Rgb }[] = [
	{ name: 'R', rgb: [0.75, 0, 0] },
	{ name: 'Mg', rgb: [0.75, 0, 0.75] },
	{ name: 'B', rgb: [0, 0, 0.75] },
	{ name: 'Cy', rgb: [0, 0.75, 0.75] },
	{ name: 'G', rgb: [0, 0.75, 0] },
	{ name: 'Yl', rgb: [0.75, 0.75, 0] }
];

const pct = (v: number) => `${Math.round(v * 100)}`;

/** A clipped count as the readout says it: a count of pixels, or a share once it is large. */
export function clipText(count: number, share: number): string {
	if (count === 0) return 'none';
	if (share < 0.1) return `${count.toLocaleString('en-US')} px`;
	return `${share.toFixed(1)}%`;
}

/** The readout under the scope: one line per channel and the colour, worded for an editor. */
export function summaryLines(s: ScopeSummary): { label: string; text: string; warn: boolean }[] {
	const channels = [
		['Luma', s.luma],
		['Red', s.red],
		['Green', s.green],
		['Blue', s.blue]
	] as const;
	const lines: { label: string; text: string; warn: boolean }[] = channels.map(([label, c]) => ({
		label,
		text: `${pct(c.min)}–${pct(c.max)}%, mean ${pct(c.mean)}% · crushed ${clipText(c.clipped_low, c.clipped_low_pct)} · clipped ${clipText(c.clipped_high, c.clipped_high_pct)}`,
		warn: c.clipped_high > 0
	}));
	const cast = s.chroma.saturation;
	lines.push({
		label: 'Colour',
		text:
			cast < 0.02
				? `neutral · saturation ${pct(s.chroma.mean_saturation)}%`
				: `cast ${Math.round(s.chroma.hue_deg)}° at ${pct(cast)}% · saturation ${pct(s.chroma.mean_saturation)}%`,
		warn: false
	});
	return lines;
}

// ---- the browser harness --------------------------------------------------------------

/** Seven 75% colour bars over the frame's width, as R'G'B' 0..1. */
const BARS: Rgb[] = [
	[0.75, 0.75, 0.75],
	[0.75, 0.75, 0],
	[0, 0.75, 0.75],
	[0, 0.75, 0],
	[0.75, 0, 0.75],
	[0.75, 0, 0],
	[0, 0, 0.75]
];

function channelOf(values: number[]): ScopeSummary['luma'] {
	const min = Math.min(...values);
	const max = Math.max(...values);
	const mean = values.reduce((a, b) => a + b, 0) / values.length;
	return { min, max, mean, clipped_low: 0, clipped_high: 0, clipped_low_pct: 0, clipped_high_pct: 0 };
}

/** What `get_scopes` answers in the browser harness: the scopes of colour bars (a 1920x1080
 *  frame, BT.709), so the panel has something real to draw. */
export function harnessScopes(grids: ScopeKind[], size: ScopeGridSize = PANEL_GRID): Scopes {
	const m = 'bt709' as const;
	const perBar = 1080 * Math.floor(1920 / BARS.length);
	const ys = BARS.map(([r, g, b]) => ycbcr(m, r, g, b));
	const summary: ScopeSummary = {
		width: 1920,
		height: 1080,
		matrix: m,
		red: channelOf(BARS.map((c) => c[0])),
		green: channelOf(BARS.map((c) => c[1])),
		blue: channelOf(BARS.map((c) => c[2])),
		luma: channelOf(ys.map((y) => y[0])),
		chroma: {
			cb: ys.reduce((a, y) => a + y[1], 0) / ys.length,
			cr: ys.reduce((a, y) => a + y[2], 0) / ys.length,
			hue_deg: 0,
			saturation: 0,
			mean_saturation: ys.reduce((a, y) => a + Math.hypot(y[1], y[2]) / 0.5, 0) / ys.length
		},
		peaks: []
	};
	const level = (v: number, rows: number) => Math.round(Math.min(1, Math.max(0, v)) * (rows - 1));
	const column = (values: (bar: number) => number): ScopeGrid => {
		const g: ScopeGrid = { columns: size.columns, rows: size.rows, counts: new Array(size.columns * size.rows).fill(0) };
		for (let c = 0; c < size.columns; c++) {
			const bar = Math.min(BARS.length - 1, Math.floor((c * BARS.length) / size.columns));
			g.counts[level(values(bar), size.rows) * size.columns + c] = 1080;
		}
		return g;
	};
	const out: Scopes = { summary };
	if (grids.includes('waveform')) out.waveform = column((b) => ys[b][0]);
	if (grids.includes('parade')) out.parade = [0, 1, 2].map((k) => column((b) => BARS[b][k]));
	if (grids.includes('vectorscope')) {
		const v = size.vector;
		const g: ScopeGrid = { columns: v, rows: v, counts: new Array(v * v).fill(0) };
		const cell = (c: number) => Math.min(v - 1, Math.max(0, Math.floor(((c + VECTOR_EXTENT) / (2 * VECTOR_EXTENT)) * v)));
		for (const [, cb, cr] of ys) g.counts[cell(cr) * v + cell(cb)] += perBar;
		out.vectorscope = g;
	}
	if (grids.includes('histogram')) {
		const bins = () => new Array(256).fill(0);
		const h: ScopeHistogram = { red: bins(), green: bins(), blue: bins(), luma: bins() };
		BARS.forEach((c, i) => {
			h.red[Math.round(c[0] * 255)] += perBar;
			h.green[Math.round(c[1] * 255)] += perBar;
			h.blue[Math.round(c[2] * 255)] += perBar;
			h.luma[Math.round(ys[i][0] * 255)] += perBar;
		});
		out.histogram = h;
	}
	return out;
}
