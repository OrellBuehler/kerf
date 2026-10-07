// A stand-in for `get_filmstrip` in the browser harness, which has no decoder.
// The desktop app answers with JPEG sheets of the real footage
// (`kerf_core::filmstrip_for`); this answers with a strip of the same *shape* —
// the same interval ladder, thumbnail width, sheet split and padding, built by
// `filmstrip-geometry.ts` — whose sheets are small generated SVGs, so the
// timeline's thumbnails, sheet seams and cache can all be developed under
// `bun run dev`.
//
// Each thumbnail is drawn to be told apart from its neighbours and from another
// asset's: a gradient whose hue is the asset's, drifting along the clip; a square
// that travels across the frame with source time; and the source time and
// thumbnail index as text. A caller that picks the wrong thumbnail for a time, or
// the wrong x on a sheet, shows it at once. The tail of the last sheet is solid
// black, as the real tile's padding is, so drawing the padding is visible too.
//
// Deterministic: a pure function of the asset's id, duration and streams, so the
// same asset always reads the same.

import type { Asset, Filmstrip, FilmstripSheet } from './types';
import { planFilmstrip, stripGeometry, timeOf } from './filmstrip-geometry';
import type { StripGeometry } from './filmstrip-geometry';

/** FNV-1a over the string, so the hue is stable across runs and engines. */
function seedOf(text: string): number {
	let h = 0x811c9dc5;
	for (let i = 0; i < text.length; i++) {
		h ^= text.charCodeAt(i);
		h = Math.imul(h, 0x01000193);
	}
	return h >>> 0;
}

/** `1:05.5` / `0:12.0` — a source time in the label. */
function stamp(seconds: number): string {
	const whole = Math.max(0, seconds);
	const m = Math.floor(whole / 60);
	return `${m}:${(whole - m * 60).toFixed(1).padStart(4, '0')}`;
}

/** The drawing of thumbnail `frame` at `x`, as SVG markup. */
function cell(strip: StripGeometry, frame: number, x: number, hue: number, still: boolean): string {
	const w = strip.frame_width;
	const h = strip.frame_height;
	// The hue drifts across the clip, so a thumbnail's place in the strip is
	// visible even with the label hidden by a narrow clip.
	const progress = strip.frames > 1 ? frame / (strip.frames - 1) : 0;
	const top = `hsl(${Math.round(hue + progress * 70) % 360} 45% 34%)`;
	const bottom = `hsl(${Math.round(hue + progress * 70 + 24) % 360} 50% 16%)`;
	const id = `g${frame}`;
	// A square that travels the width of the thumbnail with source time: 4 s a lap.
	const lap = still ? 0 : (timeOf(strip, frame) % 4) / 4;
	const side = Math.max(8, Math.round(h * 0.2));
	const sx = (x + lap * (w - side)).toFixed(1);
	const label = still ? 'still' : stamp(timeOf(strip, frame));
	return (
		`<linearGradient id="${id}" x1="0" y1="0" x2="0" y2="1">` +
		`<stop offset="0" stop-color="${top}"/><stop offset="1" stop-color="${bottom}"/></linearGradient>` +
		`<rect x="${x}" y="0" width="${w}" height="${h}" fill="url(#${id})"/>` +
		`<rect x="${sx}" y="${Math.round(h * 0.34)}" width="${side}" height="${side}" rx="3" fill="#e29d2e"/>` +
		`<rect x="${x + w - 1}" y="0" width="1" height="${h}" fill="#000" fill-opacity="0.45"/>` +
		`<text x="${x + 4}" y="${h - 22}" fill="#fff" fill-opacity="0.85" font-family="monospace" font-size="11">${label}</text>` +
		`<text x="${x + 4}" y="${h - 8}" fill="#fff" fill-opacity="0.55" font-family="monospace" font-size="10">#${frame}</text>`
	);
}

/** One sheet as a `data:` URL: its thumbnails side by side, then black padding. */
function sheetUrl(strip: StripGeometry, sheet: StripGeometry['sheets'][number], hue: number, still: boolean): string {
	let body = `<rect width="${sheet.width}" height="${sheet.height}" fill="#000"/>`;
	for (let i = 0; i < sheet.count; i++) {
		body += cell(strip, sheet.first_frame + i, i * strip.frame_width, hue, still);
	}
	const svg =
		`<svg xmlns="http://www.w3.org/2000/svg" width="${sheet.width}" height="${sheet.height}" ` +
		`viewBox="0 0 ${sheet.width} ${sheet.height}">${body}</svg>`;
	return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
}

/**
 * The filmstrip of `asset` as the harness sees it: `get_filmstrip`'s answer
 * shape, with generated sheets. Throws what the backend would for an asset with
 * no video stream.
 */
export function sampleFilmstrip(asset: Pick<Asset, 'id' | 'duration' | 'streams'>): Filmstrip {
	const plan = planFilmstrip(asset);
	const strip = stripGeometry(plan);
	const hue = seedOf(asset.id) % 360;
	const sheets: FilmstripSheet[] = strip.sheets.map((sheet) => ({
		...sheet,
		data_url: sheetUrl(strip, sheet, hue, plan.still)
	}));
	return { ...strip, sheets };
}
