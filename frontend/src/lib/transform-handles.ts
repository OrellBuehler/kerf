/* The Preview's transform handles — pure. A clip's picture is drawn where the export puts it:
 * the canvas is `export_format`'s (the delivery frame, else the first picture's size), the picture
 * box is `layer-geometry.ts`'s port of the engine's own integer geometry, so a handle sits on the
 * edge the file has. Gestures work in canvas pixels and answer the transform numbers they change;
 * snapping, the 90° detent and the Shift constraints are here, the pointer plumbing is
 * `TransformHandles.svelte`'s. One edit is written per gesture, on release (`set_transform_at`). */

import { isKeyed, transformAt } from './channels';
import { pixFmtSubsampling, resolveLayer, resolveLayerAnyGrid, YUV420, type LayerGeometry, type Resolved } from './layer-geometry';
import { trackRenders } from './levels';
import { clipDuration, DEFAULT_TRANSFORM, type Asset, type Clip, type Fit, type Timeline, type Transform } from './types';

export interface Canvas {
	width: number;
	height: number;
	fit: Fit;
}

/** The frame the cut renders at, as `export_format` decides it with no export options: the
 *  delivery frame, else the first picture of the rendered cut (a 360 clip reframed to flat keeps
 *  1920×1080), clamped to even sides. */
export function renderCanvas(timeline: Timeline, assets: readonly Asset[]): Canvas {
	let width = 1920;
	let height = 1080;
	outer: for (const track of timeline.tracks) {
		if (!trackRenders(timeline, track)) continue;
		for (const clip of track.clips) {
			if (clip.enabled === false) continue;
			const v = assets.find((a) => a.id === clip.asset_id)?.streams.find((s) => s.kind === 'video');
			if (!v) continue;
			if (clip.reframe?.output !== 'flat' && v.width && v.height) {
				width = v.width;
				height = v.height;
			}
			break outer;
		}
	}
	if (timeline.format) {
		width = timeline.format.width;
		height = timeline.format.height;
	}
	return { width: Math.max(width & ~1, 2), height: Math.max(height & ~1, 2), fit: timeline.format?.fit ?? 'contain' };
}

/** A clip's picture on the canvas: its centre and size in canvas pixels and its clockwise turn. */
export interface Box {
	cx: number;
	cy: number;
	w: number;
	h: number;
	/** Clockwise radians. */
	angle: number;
}

export interface Layer {
	clip: Clip;
	/** Seconds into the clip at the time asked (what a key is written at). */
	local: number;
	src: [number, number];
	grid: [number, number] | null;
	pose: Transform;
	geometry: LayerGeometry;
	box: Box;
}

/** Where the geometry puts the picture: the letterboxed picture of an identity layer (its
 *  matte is the canvas), else the picture centred in the overlaid (rotated) layer. */
export function pictureBox(g: LayerGeometry): Box {
	if (g.matte) {
		return { cx: g.picture_at[0] + g.picture_shows[0] / 2, cy: g.picture_at[1] + g.picture_shows[1] / 2, w: g.picture_shows[0], h: g.picture_shows[1], angle: 0 };
	}
	return { cx: g.origin[0] + g.layer[0] / 2, cy: g.origin[1] + g.layer[1] / 2, w: g.picture[0], h: g.picture[1], angle: g.rotation?.angle ?? 0 };
}

/** The geometry of `src` posed `pose`, as the still places it: on the picture's own chroma grid,
 *  else the one every grid agrees on, else (a crop that rounds differently per grid) 4:2:0's. */
export function geometryOf(src: [number, number], grid: [number, number] | null, canvas: Canvas, pose: Transform): LayerGeometry | null {
	const size: [number, number] = [canvas.width, canvas.height];
	let got: Resolved = grid ? resolveLayer(src, size, canvas.fit, pose, grid) : resolveLayerAnyGrid(src, size, canvas.fit, pose);
	if ('err' in got && !grid) got = resolveLayer(src, size, canvas.fit, pose, YUV420);
	return 'ok' in got ? got.ok : null;
}

/** The pictures on screen at `time`, bottom to top (a later video track composites over an
 *  earlier one): rendered tracks, enabled clips, flat footage. */
export function layersAt(timeline: Timeline, assets: readonly Asset[], canvas: Canvas, time: number): Layer[] {
	const out: Layer[] = [];
	for (const track of timeline.tracks) {
		if (track.kind !== 'video' || !trackRenders(timeline, track)) continue;
		const clip = track.clips.find((c) => time >= c.timeline_start && time < c.timeline_start + clipDuration(c));
		if (!clip || clip.enabled === false || clip.reframe) continue;
		const v = assets.find((a) => a.id === clip.asset_id)?.streams.find((s) => s.kind === 'video');
		if (!v?.width || !v.height) continue;
		const src: [number, number] = [v.width, v.height];
		const grid = v.pix_fmt ? pixFmtSubsampling(v.pix_fmt) : null;
		const local = Math.max(0, time - clip.timeline_start);
		const pose = { ...DEFAULT_TRANSFORM, ...transformAt(clip, local) };
		const geometry = geometryOf(src, grid, canvas, pose);
		if (!geometry) continue;
		out.push({ clip, local, src, grid, pose, geometry, box: pictureBox(geometry) });
	}
	return out;
}

/** `p` in the box's own frame: from its centre, unturned. */
export function toLocal(box: Box, x: number, y: number): [number, number] {
	const dx = x - box.cx;
	const dy = y - box.cy;
	const c = Math.cos(box.angle);
	const s = Math.sin(box.angle);
	return [dx * c + dy * s, -dx * s + dy * c];
}

export function contains(box: Box, x: number, y: number): boolean {
	const [lx, ly] = toLocal(box, x, y);
	return Math.abs(lx) <= box.w / 2 && Math.abs(ly) <= box.h / 2;
}

/** The topmost picture under a point, skipping the clips `skip` says no to (a locked track). */
export function topLayerAt(layers: readonly Layer[], x: number, y: number, skip: (c: Clip) => boolean = () => false): Layer | null {
	for (let i = layers.length - 1; i >= 0; i--) {
		if (!skip(layers[i].clip) && contains(layers[i].box, x, y)) return layers[i];
	}
	return null;
}

/** Half the width and height of the box's axis-aligned bounds. */
export function halfExtents(box: Box): [number, number] {
	const c = Math.abs(Math.cos(box.angle));
	const s = Math.abs(Math.sin(box.angle));
	return [(box.w * c + box.h * s) / 2, (box.w * s + box.h * c) / 2];
}

// ---- snapping ------------------------------------------------------------------

/** A line a box can snap to (canvas px): a vertical one at `x`, or a horizontal one at `y`. */
export interface Guide {
	axis: 'x' | 'y';
	at: number;
}

export interface Targets {
	xs: number[];
	ys: number[];
}

/** The platform chrome the safe-area guides shade, as fractions of the frame. */
export interface Chrome {
	top: number;
	bottom: number;
	right: number;
}

/** What a box snaps to: the frame's edges and centre lines, the title-safe box and the shaded
 *  platform chrome when the guides are drawn, and the edges and centres of the other pictures. */
export function snapTargets(canvas: Canvas, others: readonly Box[], chrome: Chrome | null): Targets {
	const { width: W, height: H } = canvas;
	const xs = [0, W / 2, W];
	const ys = [0, H / 2, H];
	if (chrome) {
		xs.push(W * 0.05, W * 0.95, W * (1 - chrome.right));
		ys.push(H * 0.05, H * 0.95, H * chrome.top, H * (1 - chrome.bottom));
	}
	for (const b of others) {
		const [hx, hy] = halfExtents(b);
		xs.push(b.cx - hx, b.cx, b.cx + hx);
		ys.push(b.cy - hy, b.cy, b.cy + hy);
	}
	return { xs, ys };
}

/** The shift that brings one of `features` onto one of `targets` within `threshold`, nearest first. */
export function nearestSnap(features: readonly number[], targets: readonly number[], threshold: number): { shift: number; at: number } | null {
	let best: { shift: number; at: number } | null = null;
	for (const f of features) {
		for (const t of targets) {
			const d = t - f;
			if (Math.abs(d) <= threshold && (!best || Math.abs(d) < Math.abs(best.shift))) best = { shift: d, at: t };
		}
	}
	return best;
}

// ---- gestures ----------------------------------------------------------------------

export interface Snapping {
	on: boolean;
	targets: Targets;
	/** In canvas px. */
	threshold: number;
}

export interface Moved {
	patch: Partial<Transform>;
	guides: Guide[];
}

const round = (v: number, places: number) => {
	const k = 10 ** places;
	return Math.round(v * k) / k + 0;
};

/** Drag the picture by `(dx, dy)` canvas px. Shift (`axis`) keeps it on the axis it moved most
 *  along; snapping brings an edge or the centre of its bounds onto a target. The position is a
 *  fraction of the frame, so a delta in pixels is a delta in `pos` exactly. */
export function movePose(start: Transform, box: Box, dx: number, dy: number, canvas: Canvas, snap: Snapping, axis = false): Moved {
	if (axis) {
		if (Math.abs(dx) >= Math.abs(dy)) dy = 0;
		else dx = 0;
	}
	const guides: Guide[] = [];
	if (snap.on) {
		const [hx, hy] = halfExtents(box);
		const cx = box.cx + dx;
		const cy = box.cy + dy;
		const sx = axis && dx === 0 ? null : nearestSnap([cx - hx, cx, cx + hx], snap.targets.xs, snap.threshold);
		const sy = axis && dy === 0 ? null : nearestSnap([cy - hy, cy, cy + hy], snap.targets.ys, snap.threshold);
		if (sx) {
			dx += sx.shift;
			guides.push({ axis: 'x', at: sx.at });
		}
		if (sy) {
			dy += sy.shift;
			guides.push({ axis: 'y', at: sy.at });
		}
	}
	return { patch: { pos_x: round(start.pos_x + dx / canvas.width, 4), pos_y: round(start.pos_y + dy / canvas.height, 4) }, guides };
}

export const MIN_SCALE = 0.01;
export const MAX_SCALE = 20;

/** Scale about the centre: the pointer's distance from it against where the press started. Snapping
 *  stops an edge of the bounds on a target, or the picture at its fitted size (100%). */
export function scalePose(start: Transform, box: Box, from: [number, number], to: [number, number], snap: Snapping): Moved {
	const d0 = Math.hypot(from[0] - box.cx, from[1] - box.cy);
	const d1 = Math.hypot(to[0] - box.cx, to[1] - box.cy);
	let scale = d0 > 0 ? start.scale * (d1 / d0) : start.scale;
	const guides: Guide[] = [];
	if (snap.on && start.scale > 0) {
		const [hx, hy] = halfExtents(box);
		// Each candidate as a scale, and how far it moves the bounds' edge (px) from where the pointer has it.
		const now = scale / start.scale;
		let best: { scale: number; px: number; guide: Guide | null } | null = null;
		const consider = (s: number, px: number, guide: Guide | null) => {
			if (px <= snap.threshold && (!best || px < best.px)) best = { scale: s, px, guide };
		};
		consider(1, Math.abs(1 / start.scale - now) * Math.max(hx, hy), null);
		for (const x of snap.targets.xs) {
			const r = hx > 0 ? Math.abs(x - box.cx) / hx : 0;
			if (r > 0) consider(start.scale * r, Math.abs(r - now) * hx, { axis: 'x', at: x });
		}
		for (const y of snap.targets.ys) {
			const r = hy > 0 ? Math.abs(y - box.cy) / hy : 0;
			if (r > 0) consider(start.scale * r, Math.abs(r - now) * hy, { axis: 'y', at: y });
		}
		const b = best as { scale: number; px: number; guide: Guide | null } | null;
		if (b) {
			scale = b.scale;
			if (b.guide) guides.push(b.guide);
		}
	}
	return { patch: { scale: round(Math.min(Math.max(scale, MIN_SCALE), MAX_SCALE), 4) }, guides };
}

/** How near a right angle a turn snaps onto it, and the step Shift turns in. */
export const RIGHT_ANGLE_DETENT = 5;
export const ROTATE_STEP = 15;

/** The degrees (clockwise) the pointer turned round the box's centre going from `from` to `to`:
 *  the short way, so a gesture adds these up move by move and can go round more than once. */
export function sweep(box: Box, from: [number, number], to: [number, number]): number {
	const a0 = Math.atan2(from[1] - box.cy, from[0] - box.cx);
	const a1 = Math.atan2(to[1] - box.cy, to[0] - box.cx);
	const d = ((a1 - a0) * 180) / Math.PI;
	return ((((d + 180) % 360) + 360) % 360) - 180;
}

/** Turn by `swept` degrees from where the gesture started. Shift turns in 15° steps; otherwise a
 *  turn within 5° of a right angle lands on it (unless snapping is off). */
export function rotatePose(start: Transform, swept: number, snap: boolean, steps = false): Moved {
	let rotation = start.rotation + swept;
	if (steps) rotation = Math.round(rotation / ROTATE_STEP) * ROTATE_STEP;
	else if (snap) {
		const right = Math.round(rotation / 90) * 90;
		if (Math.abs(rotation - right) <= RIGHT_ANGLE_DETENT) rotation = right;
	}
	return { patch: { rotation: round(rotation, 2) }, guides: [] };
}

export type Edge = 'left' | 'right' | 'top' | 'bottom';

/** The smallest share of the source a crop leaves on an axis. */
export const MIN_CROP_LEFT = 0.02;

/** Whether the edges of this layer can be cropped where it is drawn: a Contain fit (a Cover fit
 *  shows a frame-shaped window of the picture whatever the crop, so its edges are not the crop's),
 *  and a position, scale and rotation that are not keyed (the crop is one number for the whole
 *  clip, and keeping the picture still while it changes moves those). */
export function croppable(clip: Clip, canvas: Canvas): boolean {
	return canvas.fit === 'contain' && !(['scale', 'pos_x', 'pos_y', 'rotation'] as const).some((p) => isKeyed(clip, p));
}

/** The contain fit's ideal factor for a `w`×`h` window: canvas px per source px. */
const fitFactor = (canvas: Canvas, w: number, h: number) => Math.min(canvas.width / w, canvas.height / h);

/**
 * Crop an edge to where the pointer is, keeping the rest of the picture where it was: the export
 * re-fits a cropped window and centres it, so the scale and position are moved to undo that —
 * the content stays the same size in the same place and only the dragged edge moves. Snapping
 * stops the edge on a target while the picture is upright.
 */
export function cropPose(start: Transform, layer: Pick<Layer, 'src' | 'box' | 'geometry'>, edge: Edge, to: [number, number], canvas: Canvas, snap: Snapping): Moved {
	const [iw, ih] = layer.src;
	const box = layer.box;
	const win = layer.geometry.stages[0].src;
	const kx = box.w / win.w;
	const ky = box.h / win.h;
	let [lx, ly] = toLocal(box, to[0], to[1]);
	const guides: Guide[] = [];
	if (snap.on && Math.abs(Math.sin(box.angle)) < 1e-9 && Math.cos(box.angle) > 0) {
		if (edge === 'left' || edge === 'right') {
			const s = nearestSnap([box.cx + lx], snap.targets.xs, snap.threshold);
			if (s) {
				lx += s.shift;
				guides.push({ axis: 'x', at: s.at });
			}
		} else {
			const s = nearestSnap([box.cy + ly], snap.targets.ys, snap.threshold);
			if (s) {
				ly += s.shift;
				guides.push({ axis: 'y', at: s.at });
			}
		}
	}
	// The source pixel under the pointer, then the crop that puts the edge there.
	const sx = win.x + win.w / 2 + lx / kx;
	const sy = win.y + win.h / 2 + ly / ky;
	const crop = { left: start.crop_left, right: start.crop_right, top: start.crop_top, bottom: start.crop_bottom };
	const clamp = (v: number, hi: number) => Math.min(Math.max(v, 0), Math.max(hi, 0));
	if (edge === 'left') crop.left = clamp(sx / iw, 1 - crop.right - MIN_CROP_LEFT);
	if (edge === 'right') crop.right = clamp(1 - sx / iw, 1 - crop.left - MIN_CROP_LEFT);
	if (edge === 'top') crop.top = clamp(sy / ih, 1 - crop.bottom - MIN_CROP_LEFT);
	if (edge === 'bottom') crop.bottom = clamp(1 - sy / ih, 1 - crop.top - MIN_CROP_LEFT);
	const ww0 = iw * (1 - start.crop_left - start.crop_right);
	const wh0 = ih * (1 - start.crop_top - start.crop_bottom);
	const ww1 = iw * (1 - crop.left - crop.right);
	const wh1 = ih * (1 - crop.top - crop.bottom);
	const scale = start.scale * (fitFactor(canvas, ww0, wh0) / fitFactor(canvas, ww1, wh1));
	// The window's centre moved this far in the source; the picture's centre goes with it, turned.
	const dsx = (iw * (crop.left - crop.right) - iw * (start.crop_left - start.crop_right)) / 2;
	const dsy = (ih * (crop.top - crop.bottom) - ih * (start.crop_top - start.crop_bottom)) / 2;
	const k = start.scale * fitFactor(canvas, ww0, wh0);
	const c = Math.cos(box.angle);
	const s = Math.sin(box.angle);
	const dx = (dsx * c - dsy * s) * k;
	const dy = (dsx * s + dsy * c) * k;
	const patch: Partial<Transform> = {
		scale: round(Math.min(Math.max(scale, MIN_SCALE), MAX_SCALE), 4),
		pos_x: round(start.pos_x + dx / canvas.width, 4),
		pos_y: round(start.pos_y + dy / canvas.height, 4)
	};
	if (edge === 'left') patch.crop_left = round(crop.left, 4);
	if (edge === 'right') patch.crop_right = round(crop.right, 4);
	if (edge === 'top') patch.crop_top = round(crop.top, 4);
	if (edge === 'bottom') patch.crop_bottom = round(crop.bottom, 4);
	return { patch, guides };
}

/** Only what the gesture changed, against the pose it started from: an edit that changes
 *  nothing writes nothing. */
export function changed(start: Transform, patch: Partial<Transform>): Partial<Transform> {
	const out: Partial<Transform> = {};
	for (const k of Object.keys(patch) as (keyof Transform)[]) {
		const v = patch[k];
		if (v !== undefined && Math.abs(v - start[k]) > 1e-9) out[k] = v;
	}
	return out;
}

/** The resize cursor for a handle at `base` degrees (0 = east, clockwise) on a box turned `angle`. */
export function resizeCursor(base: number, angle: number): string {
	const deg = ((((base + (angle * 180) / Math.PI) % 180) + 180) % 180);
	const step = Math.round(deg / 45) % 4;
	return ['ew-resize', 'nwse-resize', 'ns-resize', 'nesw-resize'][step];
}

/** What the pointer says while a gesture is held. */
export function readout(kind: 'move' | 'scale' | 'rotate' | 'crop', pose: Transform, edge?: Edge): string {
	const pct = (v: number) => `${Math.round(v * 1000) / 10}%`;
	switch (kind) {
		case 'move':
			return `x ${pct(pose.pos_x)} · y ${pct(pose.pos_y)}`;
		case 'scale':
			return pct(pose.scale);
		case 'rotate':
			return `${Math.round(pose.rotation * 10) / 10}°`;
		case 'crop': {
			const key = `crop_${edge ?? 'left'}` as keyof Transform;
			return `crop ${edge} ${pct(pose[key])}`;
		}
	}
}
