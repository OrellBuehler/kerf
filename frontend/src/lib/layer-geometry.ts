/* The integer geometry of one layer, as FFmpeg places it: a faithful port of kerf-core's
 * `layer_geometry.rs` (`LayerGeometry::resolve_with`, `resolve_any_grid_with`,
 * `pix_fmt_subsampling`), which the export's graph, the scrubbed still and the GPU compositor all
 * place a clip by. The Preview's transform handles are drawn from it, so a handle sits on the
 * picture's edge where the file puts it, not where a fraction says it should be. kerf-core writes
 * its answers down (`layer_geometry/corpus.rs` → `fixtures/layer-geometry-corpus.json`) and
 * `layer-geometry.test.ts` replays every case through this port. */

import type { Fit, Transform } from './types';

export interface Rect {
	x: number;
	y: number;
	w: number;
	h: number;
}

export interface ScaleStage {
	src: Rect;
	scaled: [number, number];
	keep: Rect;
}

export interface Rotation {
	/** Clockwise radians. */
	angle: number;
	out: [number, number];
}

export interface LayerGeometry {
	stages: ScaleStage[];
	picture: [number, number];
	rotation: Rotation | null;
	layer: [number, number];
	picture_at: [number, number];
	picture_shows: [number, number];
	matte: boolean;
	origin: [number, number];
	opacity: number;
	translucent: boolean;
}

/** A chroma grid: log2 of the horizontal and vertical subsampling. */
export type Subsampling = [number, number];

export const YUV420: Subsampling = [1, 1];
export const NO_SUBSAMPLING: Subsampling = [0, 0];
export const ALL_GRIDS: Subsampling[] = [[0, 0], [1, 0], [0, 1], [1, 1], [2, 0], [2, 2]];

export interface Keyed {
	scale: boolean;
	rotation: boolean;
	rotates: boolean;
	opacity: boolean;
}

export interface Placement {
	keyframed: Keyed | null;
	offset: [number, number];
}

export const STILL: Placement = { keyframed: null, offset: [0, 0] };

export const TINY_SCALE = 0.01;

export type Resolved = { ok: LayerGeometry } | { err: string };

const roundW = (s: Subsampling, v: number) => v - (v & ((1 << s[0]) - 1));
const roundH = (s: Subsampling, v: number) => v - (v & ((1 << s[1]) - 1));

/** `f64::round_ties_even`. */
function lrint(d: number): number {
	const r = Math.round(d);
	return Math.abs(d % 1) === 0.5 && r % 2 !== 0 ? r - 1 : r;
}

/** `av_rescale` for positive operands. */
const avRescale = (a: number, b: number, c: number) => Math.floor((a * b + Math.floor(c / 2)) / c);

/** Round down to a multiple of two (two's complement, as `& !1` on an i64). */
const even = (v: number) => v - (((v % 2) + 2) % 2) + 0;

function overlayCoord(d: number): number {
	return even(Math.trunc(Math.min(Math.max(d, -1e9), 1e9)));
}

export function ffmpegAlpha(op: number): number {
	const mixed = Math.min(Math.max(lrint(Math.min(Math.max(op, 0), 1) * 255), 0), 255);
	return Math.min(Math.floor((mixed * 256 + 127) / 255), 255);
}

const isIdentity = (t: Transform) =>
	t.scale === 1 &&
	t.pos_x === 0 &&
	t.pos_y === 0 &&
	t.rotation === 0 &&
	t.opacity === 1 &&
	t.crop_left === 0 &&
	t.crop_right === 0 &&
	t.crop_top === 0 &&
	t.crop_bottom === 0;

const hasCrop = (t: Transform) => t.crop_left > 0 || t.crop_right > 0 || t.crop_top > 0 || t.crop_bottom > 0;

function cropWindow(iw: number, ih: number, tf: Transform, sub: Subsampling): Rect | string {
	if (!hasCrop(tf)) return { x: 0, y: 0, w: iw, h: ih };
	const cw = Math.max(1 - tf.crop_left - tf.crop_right, 0);
	const ch = Math.max(1 - tf.crop_top - tf.crop_bottom, 0);
	const w = roundW(sub, lrint(iw * cw));
	const h = roundH(sub, lrint(ih * ch));
	if (w <= 0 || h <= 0 || w > iw || h > ih) return `the crop leaves ${w}x${h} of ${iw}x${ih}`;
	let x = Math.max(lrint(iw * tf.crop_left), 0);
	let y = Math.max(lrint(ih * tf.crop_top), 0);
	if (x + w > iw) x = iw - w;
	if (y + h > ih) y = ih - h;
	return { x: roundW(sub, x), y: roundH(sub, y), w, h };
}

/** `rotw` / `roth`: FFmpeg takes the sine and cosine as `float`. */
function rotatedBox(iw: number, ih: number, angle: number): [number, number] {
	const s = Math.fround(Math.sin(angle));
	const c = Math.fround(Math.cos(angle));
	const w = Math.abs(ih * s) + Math.abs(iw * c);
	const h = Math.abs(iw * s) + Math.abs(ih * c);
	return [Math.max(Math.trunc(w + 0.5), 1), Math.max(Math.trunc(h + 0.5), 1)];
}

const sameGeometry = (a: LayerGeometry, b: LayerGeometry) => JSON.stringify(a) === JSON.stringify(b);

/** `LayerGeometry::resolve_with`: a `src`-sized picture on a `canvas`-sized frame. */
export function resolveLayer(
	src: [number, number],
	canvas: [number, number],
	fit: Fit,
	tf: Transform,
	sub: Subsampling = YUV420,
	placement: Placement = STILL
): Resolved {
	const [iw, ih] = src;
	const keyed = placement.keyframed;
	const [mx, my] = placement.offset;
	const [ow, oh] = canvas;
	if (iw === 0 || ih === 0 || ow === 0 || oh === 0) return { err: 'an empty picture or canvas' };
	if (!Number.isFinite(tf.scale) || tf.scale <= 0 || ![tf.pos_x, tf.pos_y, tf.rotation, tf.opacity, mx, my].every(Number.isFinite)) {
		return { err: 'a non-finite transform' };
	}
	const win = cropWindow(iw, ih, tf, sub);
	if (typeof win === 'string') return { err: win };
	const tmpW = avRescale(oh, win.w, win.h);
	const tmpH = avRescale(ow, win.h, win.w);
	// A blurred fill places the picture as Contain does; only what is behind it differs.
	const contain = fit !== 'cover';
	const fw = contain ? Math.min(tmpW, ow) : Math.max(tmpW, ow);
	const fh = contain ? Math.min(tmpH, oh) : Math.max(tmpH, oh);
	if (fw <= 0 || fh <= 0) return { err: `the fit scale leaves ${fw}x${fh}` };
	const zoomKeyed = !!keyed?.scale;
	const secondScale = zoomKeyed || (!isIdentity(tf) && Math.abs(tf.scale - 1) > 1e-9);
	const coverGrid = secondScale ? sub : YUV420;
	const keep: Rect = contain
		? { x: 0, y: 0, w: fw, h: fh }
		: {
				x: roundW(coverGrid, lrint((fw - ow) / 2)),
				y: roundH(coverGrid, lrint((fh - oh) / 2)),
				w: roundW(coverGrid, ow),
				h: roundH(coverGrid, oh)
			};
	const stages: ScaleStage[] = [{ src: win, scaled: [fw, fh], keep }];
	let picture: [number, number] = [keep.w, keep.h];

	if (!keyed && isIdentity(tf)) {
		let origin: [number, number] = contain ? [even(Math.trunc((ow - fw) / 2)), even(Math.trunc((oh - fh) / 2))] : [0, 0];
		let layer: [number, number];
		let pictureAt: [number, number];
		let shows: [number, number];
		let matte: boolean;
		if (contain) {
			shows = [picture[0] & ~1, picture[1] & ~1];
			layer = [ow, oh];
			pictureAt = [Math.max(origin[0], 0), Math.max(origin[1], 0)];
			matte = shows[0] !== ow || shows[1] !== oh;
		} else {
			layer = picture;
			pictureAt = [0, 0];
			shows = picture;
			matte = false;
		}
		if (matte) origin = [0, 0];
		if (mx !== 0 || my !== 0) {
			origin = [overlayCoord((ow - layer[0]) / 2 + mx * ow), overlayCoord((oh - layer[1]) / 2 + my * oh)];
		}
		return {
			ok: {
				stages,
				picture,
				rotation: null,
				layer,
				picture_at: pictureAt,
				picture_shows: shows,
				matte,
				origin,
				opacity: 1,
				translucent: false
			}
		};
	}

	if (zoomKeyed || Math.abs(tf.scale - 1) > 1e-9) {
		const tiny = tf.scale < TINY_SCALE;
		const side = (v: number, whole: number) => (v === 0 ? (tiny ? 1 : whole) : Math.min(v, 65535 * 4));
		const w = side(Math.trunc(picture[0] * tf.scale), picture[0]);
		const h = side(Math.trunc(picture[1] * tf.scale), picture[1]);
		stages.push({ src: { x: 0, y: 0, w: picture[0], h: picture[1] }, scaled: [w, h], keep: { x: 0, y: 0, w, h } });
		picture = [w, h];
	}

	const opacityKeyed = !!keyed?.opacity;
	if (!opacityKeyed && tf.opacity < 1 && (picture[0] % 2 === 1 || picture[1] % 2 === 1)) {
		return {
			err: `a translucent layer of odd size ${picture[0]}x${picture[1]} (FFmpeg's RGB round trip reads past the picture)`
		};
	}

	let rotation: Rotation | null = null;
	const angle = tf.rotation * (Math.PI / 180);
	if (keyed?.rotation) {
		if (keyed.rotates) {
			const side = Math.max(Math.trunc(Math.hypot(picture[0], picture[1]) + 0.5), 1);
			rotation = { angle, out: [side, side] };
		}
	} else if (tf.rotation !== 0) {
		rotation = { angle, out: rotatedBox(picture[0], picture[1], angle) };
	}
	const layer = rotation ? rotation.out : picture;
	const origin: [number, number] = [
		overlayCoord((ow - layer[0]) / 2 + (tf.pos_x + mx) * ow),
		overlayCoord((oh - layer[1]) / 2 + (tf.pos_y + my) * oh)
	];
	return {
		ok: {
			stages,
			picture,
			rotation,
			layer,
			picture_at: [0, 0],
			picture_shows: layer,
			matte: false,
			origin,
			opacity: opacityKeyed
				? Math.fround(Math.min(Math.max(tf.opacity, 0), 1))
				: tf.opacity < 1
					? Math.fround(ffmpegAlpha(tf.opacity) / 255)
					: 1,
			translucent: !opacityKeyed && tf.opacity < 1
		}
	};
}

/** `LayerGeometry::resolve_any_grid_with`: for a picture whose chroma grid is not known — the
 *  answer only when every grid gives the same one. */
export function resolveLayerAnyGrid(
	src: [number, number],
	canvas: [number, number],
	fit: Fit,
	tf: Transform,
	placement: Placement = STILL
): Resolved {
	const first = resolveLayer(src, canvas, fit, tf, YUV420, placement);
	if ('err' in first) return first;
	for (const sub of ALL_GRIDS) {
		const other = resolveLayer(src, canvas, fit, tf, sub, placement);
		if ('err' in other) return other;
		if (!sameGeometry(other.ok, first.ok)) {
			return {
				err: 'its crop rounds differently for 4:2:0 and for other pixel formats, and this one is not known well enough to say which'
			};
		}
	}
	return first;
}

/** `pix_fmt_subsampling`: the native chroma grid of a pixel format, or null when it is not one
 *  whose `crop` only rounds to a grid. */
export function pixFmtSubsampling(name: string): Subsampling | null {
	const n = name.toLowerCase();
	const t = n.endsWith('le') || n.endsWith('be') ? n.slice(0, -2) : n;
	if (['p010', 'p012', 'p016'].includes(t)) return YUV420;
	if (['p210', 'p212', 'p216'].includes(t)) return [1, 0];
	if (['p410', 'p412', 'p416'].includes(t)) return NO_SUBSAMPLING;
	const digits = /\d*$/.exec(t)![0];
	const stem = t.slice(0, t.length - digits.length);
	let base = t;
	let bits = 8;
	if (digits.length > 0 && (stem.endsWith('p') || stem === 'gray')) {
		base = stem;
		bits = Number(digits);
	}
	const planar = [8, 9, 10, 12, 14, 16].includes(bits);
	switch (base) {
		case 'yuv420p':
		case 'yuvj420p':
			return planar ? YUV420 : null;
		case 'nv12':
		case 'nv21':
			return YUV420;
		case 'yuv422p':
		case 'yuvj422p':
			return planar ? [1, 0] : null;
		case 'nv16':
			return [1, 0];
		case 'yuv440p':
		case 'yuvj440p':
			return planar ? [0, 1] : null;
		case 'yuv411p':
		case 'yuvj411p':
			return [2, 0];
		case 'yuv410p':
			return [2, 2];
		case 'yuv444p':
		case 'yuvj444p':
		case 'gray':
		case 'gbrp':
			return planar ? NO_SUBSAMPLING : null;
		case 'nv24':
		case 'nv42':
		case 'rgb24':
		case 'bgr24':
		case 'rgb0':
		case 'bgr0':
		case '0rgb':
		case '0bgr':
		case 'rgb48':
		case 'bgr48':
			return NO_SUBSAMPLING;
		default:
			return null;
	}
}
