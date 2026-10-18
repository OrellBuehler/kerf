// Animated SVG titles, the page's side of them — pure. The engine draws the artwork (resvg, into
// every preview and export frame); the page never inlines it. What it keeps here is the vocabulary
// (the four presets, a title's display name), the placement box the preview moves and resizes, the
// sample document a new custom title starts from, and the data URL a thumbnail is shown through.

import type { Delivery, SvgOverlay, SvgPresetInfo, SvgPresetKind, SvgPresetParamsInput, SvgOverlayPatch } from './types';

/** A ready-made animated title as the controls offer it: kerf-core's `SvgPresetKind` with the words
 *  the page shows and what a new one is filled with. */
export interface SvgPresetChoice {
	kind: SvgPresetKind;
	label: string;
	hint: string;
	/** What a new title of this kind says; also what its thumbnail is drawn with. */
	sample: SvgPresetParamsInput;
}

/** In the order they are offered. `fixtures/svg-presets.json` (blessed from kerf-core) holds the same
 *  kinds and labels, and a test holds this list to it. */
export const SVG_PRESETS: readonly SvgPresetChoice[] = [
	{ kind: 'fade_up', label: 'Fade up', hint: 'Text that rises into place and fades in', sample: { text: 'Your title' } },
	{ kind: 'typewriter', label: 'Typewriter', hint: 'Types the text out one letter at a time', sample: { text: 'Your title' } },
	{
		kind: 'lower_third',
		label: 'Lower third',
		hint: 'A name bar that wipes in over the bottom of the frame',
		sample: { text: 'Your name', subtitle: 'Your role' }
	},
	{ kind: 'word_pop', label: 'Word pop', hint: 'One word at a time, popping in', sample: { text: 'Make every word count' } }
];

export const svgPresetChoice = (kind: SvgPresetKind): SvgPresetChoice | undefined => SVG_PRESETS.find((p) => p.kind === kind);

export const svgPresetLabel = (kind: SvgPresetKind): string => svgPresetChoice(kind)?.label ?? kind;

/** What the lane, the lists and the Inspector call an animated title: its own name, else what a preset
 *  says, else a placeholder. */
export function svgDisplayName(o: Pick<SvgOverlay, 'name' | 'preset'>): string {
	return o.name.trim() || o.preset?.params.text || 'SVG';
}

/** The lane tooltip: what the item is, then how it was made. */
export function svgTooltip(o: Pick<SvgOverlay, 'name' | 'preset'>): string {
	return `Animated title: ${svgDisplayName(o)} · ${o.preset ? svgPresetLabel(o.preset.kind) : 'custom SVG'}`;
}

/** Whether the preset (by its listing) takes a parameter — the Inspector shows only the fields a preset reads. A preset
 *  that is not in the list (it could not be loaded) shows every field. */
export function takesParam(info: SvgPresetInfo | undefined, name: string): boolean {
	return info ? info.params.some((p) => p.name === name) : true;
}

/** An SVG document as an `<img>` source. The page shows artwork only this way: an image runs no script and
 *  loads nothing, which inlining the document would not promise. */
export const svgDataUrl = (svg: string): string => `data:image/svg+xml,${encodeURIComponent(svg)}`;

/** The frame the artwork is drawn for: the delivery frame, else the 1080p kerf-core's presets assume. */
export function svgFrame(format: Pick<Delivery, 'width' | 'height'> | null | undefined): { width: number; height: number } {
	return format && format.width > 0 && format.height > 0 ? { width: format.width, height: format.height } : { width: 1920, height: 1080 };
}

const px = (v: number) => String(Math.round(v * 100) / 100);

/** The document a new custom title starts from: a gold plate that slides up and fades in, its word
 *  fading in after it, sized to the frame so the viewBox is the frame. Named colours only. */
export function sampleSvg(width: number, height: number): string {
	const w = Math.max(16, Math.round(width));
	const h = Math.max(16, Math.round(height));
	const bw = w * 0.4;
	const bh = h * 0.16;
	const x = (w - bw) / 2;
	const y = (h - bh) / 2;
	const rise = h * 0.04;
	return [
		`<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">`,
		`  <rect x="${px(x)}" y="${px(y)}" width="${px(bw)}" height="${px(bh)}" rx="${px(bh * 0.15)}" fill="gold" opacity="0">`,
		`    <animate attributeName="opacity" from="0" to="1" begin="0s" dur="0.6s" fill="freeze"/>`,
		`    <animateTransform attributeName="transform" type="translate" from="0 ${px(rise)}" to="0 0" begin="0s" dur="0.6s" fill="freeze"/>`,
		`  </rect>`,
		`  <text x="${px(w / 2)}" y="${px(y + bh * 0.66)}" text-anchor="middle" font-family="Geist" font-size="${px(bh * 0.5)}" font-weight="700" fill="black" opacity="0">`,
		`    Hello`,
		`    <animate attributeName="opacity" from="0" to="1" begin="0.3s" dur="0.5s" fill="freeze"/>`,
		`  </text>`,
		`</svg>`
	].join('\n');
}

// ---- the placement box -----------------------------------------------------------------------

/** A title's placement rectangle, fractions of the frame, top-left origin. */
export interface Rect {
	x: number;
	y: number;
	width: number;
	height: number;
}

/** Smallest a box may be dragged to, a fraction of the frame. */
export const RECT_MIN = 0.02;

export type RectHandle = 'move' | 'nw' | 'ne' | 'sw' | 'se';

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

/** Where a box's edge may be dragged: the frame, widened to take in where it started (a box already past the
 *  frame can be pulled back without being thrown at its edge). */
const reach = (start: number): [number, number] => [Math.min(0, start), Math.max(1, start)];

/**
 * The box after the pointer travelled `dx`, `dy` pixels across a `w` x `h` px frame. `move` keeps the size and
 * holds the box inside the frame (a box larger than the frame can slide across it); a corner moves with the
 * opposite corner held, never past `RECT_MIN` of it and never off the frame.
 */
export function dragRect(from: Rect, handle: RectHandle, dx: number, dy: number, w: number, h: number): Rect {
	const fx = w > 0 ? dx / w : 0;
	const fy = h > 0 ? dy / h : 0;
	if (handle === 'move') {
		const [x0, x1] = [Math.min(0, 1 - from.width), Math.max(0, 1 - from.width)];
		const [y0, y1] = [Math.min(0, 1 - from.height), Math.max(0, 1 - from.height)];
		return { ...from, x: clamp(from.x + fx, x0, x1), y: clamp(from.y + fy, y0, y1) };
	}
	const right = from.x + from.width;
	const bottom = from.y + from.height;
	let [left, top] = [from.x, from.y];
	let [r, b] = [right, bottom];
	if (handle === 'nw' || handle === 'sw') {
		const [lo] = reach(from.x);
		left = clamp(from.x + fx, lo, Math.max(lo, right - RECT_MIN));
	} else {
		const [lo, hi] = reach(right);
		r = clamp(right + fx, Math.min(hi, from.x + RECT_MIN), hi);
	}
	if (handle === 'nw' || handle === 'ne') {
		const [lo] = reach(from.y);
		top = clamp(from.y + fy, lo, Math.max(lo, bottom - RECT_MIN));
	} else {
		const [, hi] = reach(bottom);
		b = clamp(bottom + fy, Math.min(hi, from.y + RECT_MIN), hi);
	}
	return { x: left, y: top, width: r - left, height: b - top };
}

/** A rect to the four decimals the backend is written, so a drag does not store `0.30000000000000004`. */
export function roundRect(r: Rect): Rect {
	const q = (v: number) => Math.round(v * 10000) / 10000;
	return { x: q(r.x), y: q(r.y), width: q(r.width), height: q(r.height) };
}

/** The part of a rect that changed, as an update; empty when nothing did. */
export function rectPatch(from: Rect, to: Rect): SvgOverlayPatch {
	const patch: SvgOverlayPatch = {};
	for (const k of ['x', 'y', 'width', 'height'] as const) if (Math.abs(to[k] - from[k]) > 1e-9) patch[k] = to[k];
	return patch;
}

export const rectOf = (o: Rect): Rect => ({ x: o.x, y: o.y, width: o.width, height: o.height });

/** Whether a title fills the frame, the placement a new one starts with. */
export const isFullFrame = (o: Rect): boolean => o.x === 0 && o.y === 0 && o.width === 1 && o.height === 1;
