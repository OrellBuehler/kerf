/* What the Scopes panel draws, as numbers: how a count grid becomes pixels, how the readout words
 * the summary, and the browser harness's stand-in (colour bars). Pure, so it is tested without a
 * canvas; the panel only resolves the theme's colours and puts the pixels on screen.
 *
 * Grids are `kerf_core::scopes`: `counts[row * columns + column]`, row 0 the lowest level. */

import { trackRenders } from './levels';
import { clipDuration } from './speed-ramp';
import type { ScopeGrid, ScopeGridSize, ScopeHistogram, ScopeKind, ScopeMatrix, ScopeSummary, Scopes, Timeline } from './types';

export interface ScopeKindInfo {
	id: ScopeKind;
	label: string;
	/** A shorter name that is still a word, for a panel too narrow for the full one. */
	short: string;
	/** The icon of the chip where there is no room for a word at all (`kindLook`): the tooltip carries the name. */
	icon: string;
	/** What it shows, for the tooltip (which also carries the full name wherever the chip is short or an icon). */
	hint: string;
}

export const SCOPE_KINDS: readonly ScopeKindInfo[] = [
	{ id: 'waveform', label: 'Waveform', short: 'Wave', icon: 'audio-waveform', hint: 'Waveform: brightness at each level, left to right across the frame' },
	{ id: 'parade', label: 'RGB parade', short: 'Parade', icon: 'columns-3', hint: 'RGB parade: red, green and blue side by side, each as a waveform' },
	{ id: 'vectorscope', label: 'Vectorscope', short: 'Vector', icon: 'orbit', hint: 'Vectorscope: hue around the circle, saturation outward. The boxes are 75% colour bars, the line is skin tone' },
	{ id: 'histogram', label: 'Histogram', short: 'Histogram', icon: 'chart-no-axes-column', hint: 'Histogram: how many pixels sit at each level' }
];

/** The panel width from which the four chips fit one row with their full names, px; from `WORDS_FROM` with the short
 *  ones (still words); below it the picker is four icon buttons and the name of the one that is on. */
export const FULL_LABELS_FROM = 376;
export const WORDS_FROM = 288;

export type KindLook = 'full' | 'words' | 'icons';

/** How the kind picker is drawn for a panel `width` px wide (the full names while it is not measured yet). */
export function kindLook(width: number): KindLook {
	if (!(width > 0) || width >= FULL_LABELS_FROM) return 'full';
	return width >= WORDS_FROM ? 'words' : 'icons';
}

/** The chips of the kind picker for a panel `width` px wide, when it is drawn as chips: full names, or short ones. */
export function kindOptions(width: number): { value: ScopeKind; label: string; hint: string }[] {
	const full = kindLook(width) !== 'words';
	return SCOPE_KINDS.map((k) => ({ value: k.id, label: full ? k.label : k.short, hint: k.hint }));
}

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

export interface ReadoutLine {
	label: string;
	text: string;
	warn: boolean;
}

type Channel = ScopeSummary['luma'];

/** `5–75%, mean 43%`: where a channel's levels lie. */
const spread = (c: Channel) => `${pct(c.min)}–${pct(c.max)}%, mean ${pct(c.mean)}%`;

/** What is clipped in a channel, only the sides that are: `clipped 2.1%`, `crushed 300 px · clipped 1 px`; empty when
 *  nothing is. */
export function clipping(c: Channel): string {
	const parts: string[] = [];
	if (c.clipped_low > 0) parts.push(`crushed ${clipText(c.clipped_low, c.clipped_low_pct)}`);
	if (c.clipped_high > 0) parts.push(`clipped ${clipText(c.clipped_high, c.clipped_high_pct)}`);
	return parts.join(' · ');
}

/** The colour line: neutral, or the cast's hue and strength, and the mean saturation. */
export function colourText(s: ScopeSummary): string {
	const cast = s.chroma.saturation;
	return cast < 0.02
		? `neutral · saturation ${pct(s.chroma.mean_saturation)}%`
		: `cast ${Math.round(s.chroma.hue_deg)}° at ${pct(cast)}% · saturation ${pct(s.chroma.mean_saturation)}%`;
}

/**
 * What the readout under a scope says, for the scope that is showing: the brightness scopes say the luma and whether
 * anything is clipped, the parade the three channels (a clipped one says so on its own line, nothing repeats "none"),
 * the vectorscope the colour. One short line each, so the panel's narrowest still holds them.
 */
export function readoutLines(kind: ScopeKind, s: ScopeSummary): ReadoutLine[] {
	if (kind === 'vectorscope') return [{ label: 'Colour', text: colourText(s), warn: false }];
	if (kind === 'parade') {
		return (
			[
				['Red', s.red],
				['Green', s.green],
				['Blue', s.blue]
			] as const
		).map(([label, c]) => {
			const lost = clipping(c);
			return { label, text: lost ? `${spread(c)} · ${lost}` : spread(c), warn: c.clipped_high > 0 };
		});
	}
	const lost = clipping(s.luma);
	return [
		{ label: 'Luma', text: spread(s.luma), warn: false },
		{ label: 'Clipping', text: lost || 'none', warn: s.luma.clipped_high > 0 }
	];
}

const MATRIX_NAMES: Record<Exclude<ScopeMatrix, 'auto'>, string> = { bt601: 'BT.601', bt709: 'BT.709', bt2020: 'BT.2020' };

/** What was measured, in one line: the frame (at `at`, as the interface writes a time), where it comes from, its size and
 *  the matrix its colours were read with. The frame is the export's, from the original media, never a proxy. */
export function legendText(s: Pick<ScopeSummary, 'width' | 'height' | 'matrix'>, at: string): string {
	return legendParts(s, at).join(' · ');
}

/**
 * The legend's pieces that fit one line `width` px wide (`charPx` a character): from the front, so the frame and where it comes
 * from stay and the size and matrix go first. Never fewer than one. (A legend that wraps leaves a dot at the end of a line
 * or the size clipped; this says less instead.)
 */
export function fitLegend(parts: readonly string[], width: number, charPx = 5.4, sep = ' · '): string[] {
	let used = 0;
	const out: string[] = [];
	for (const part of parts) {
		const next = used + (out.length ? sep.length : 0) + part.length;
		if (out.length > 0 && next * charPx > width) break;
		out.push(part);
		used = next;
	}
	return out;
}

/** The legend's pieces, so a narrow panel wraps between them and never inside one. */
export function legendParts(s: Pick<ScopeSummary, 'width' | 'height' | 'matrix'>, at: string): string[] {
	return [`Frame at ${at}`, 'original media', `${s.width}×${s.height}`, MATRIX_NAMES[s.matrix] ?? s.matrix];
}

// ---- what there is to measure ------------------------------------------------------

export type ScopeSubject = 'empty' | 'gap' | 'picture';

/**
 * What the scopes have to measure at `time`, as the renderer sees the cut (`Timeline::for_render`): a muted picture track,
 * one shadowed by a soloed one, and a disabled clip draw nothing (`trackRenders`). `empty` when nothing of the cut draws a
 * picture at all (an empty cut, only sound, every picture track hidden), `gap` when something does but not at the playhead
 * (no clip covers it, and no title either), else `picture`. Only a `picture` is asked of the backend; the other two are said,
 * instead of measuring black as if it were a finding. A title (an overlay) is drawn whatever the tracks do.
 */
export function scopeSubject(timeline: Pick<Timeline, 'tracks' | 'overlays' | 'svg_overlays'>, time: number, eps = 1e-6): ScopeSubject {
	const overlays = [...(timeline.overlays ?? []), ...(timeline.svg_overlays ?? [])];
	const drawn = timeline.tracks.filter((t) => t.kind === 'video' && trackRenders(timeline, t)).flatMap((t) => t.clips.filter((c) => c.enabled !== false));
	if (drawn.length === 0 && overlays.length === 0) return 'empty';
	const covers = (start: number, end: number) => time >= start - eps && time < end - eps;
	if (overlays.some((o) => covers(o.start, o.end))) return 'picture';
	return drawn.some((c) => covers(c.timeline_start, c.timeline_start + clipDuration(c))) ? 'picture' : 'gap';
}

/** The media query that is true while the display's pixel ratio is `dpr`: it stops being true when the window moves to
 *  another screen or is zoomed, which is the moment a canvas drawn at the old ratio is soft. */
export const dprQuery = (dpr: number): string => `(resolution: ${Number.isFinite(dpr) && dpr > 0 ? dpr : 1}dppx)`;

// ---- the graticule's numbers -----------------------------------------------------------

/** The levels a waveform, parade and histogram are ruled at, in percent. */
export const LEVELS: readonly number[] = [0, 25, 50, 75, 100];

/** The row of `percent` in a plot `height` tall starting `top` down (100 at the top). */
export const levelY = (percent: number, top: number, height: number): number => top + (1 - percent / 100) * height;

/** A label of text `size` tall centred on `y`, kept inside a plot `height` tall (the 0 and 100 lines sit at its edges). */
export function labelY(y: number, size: number, height: number): number {
	const half = size / 2 + 1;
	return Math.min(Math.max(y, half), Math.max(half, height - half));
}

/** The vectorscope's rings, as a share of its full-saturation ring (`VECTOR_EXTENT` leaves room beyond it). */
export const RINGS: readonly { share: number; label: string }[] = [
	{ share: 0.75, label: '75' },
	{ share: 1, label: '100' }
];

/** Whether a scope is big enough to carry its numbers without them sitting on the trace. */
export const showsScale = (side: number): boolean => side >= 140;

/** Whether the plot's own kind of scope draws numbers down its left side (the brightness scopes) or along its foot. */
export const scaleSide = (kind: ScopeKind): 'left' | 'foot' | 'none' =>
	kind === 'waveform' || kind === 'parade' ? 'left' : kind === 'histogram' ? 'foot' : 'none';

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
