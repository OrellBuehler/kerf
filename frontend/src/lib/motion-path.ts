/* The Preview's motion path — pure. A clip whose position is keyed draws the trajectory of its
 * centre over its whole duration: the eased curves of `pos_x` / `pos_y` (`channels.ts`, whichever of
 * a track and the legacy bundle drives each) sampled in time, mapped onto the render canvas the way
 * `TransformHandles` maps the clip's box — the position is a fraction of the frame from its centre,
 * and the integer geometry's rounding at the playhead (`PathFrame.shift`) is added, so the path
 * meets the box exactly where the clip is and stays smooth everywhere else.
 *
 * A *position key* is the union of the two numbers' key times. Dragging its dot writes the
 * numbers that moved as one `set_property_tracks` edit (`moveKeyTracks`); a number with no keys of
 * its own that the drag moves is keyed at every position key, holding its value everywhere but
 * the dragged one, so the rest of the path stays put. Double-clicking the path keys the numbers
 * that are keyed at the nearest frame (`addKeyTracks`). The component is `MotionPath.svelte`. */

import { interpolate, isKeyed, ownEasing, propertyCurve, propertyKeys, staticValue, upsertKey, type Animatable } from './channels';
import { snapToFrame } from './frames';
import { changed, movePose, type Box, type Canvas, type Guide, type Snapping } from './transform-handles';
import { DEFAULT_TRANSFORM, type PropertyKey, type PropertyTrack } from './types';

export type PosProp = 'pos_x' | 'pos_y';
export const POSITION: readonly PosProp[] = ['pos_x', 'pos_y'];
export type Pos = Record<PosProp, number>;

export interface Pt {
	x: number;
	y: number;
}

/** Two keys of one number this close in time are one moment (`upsert`'s microsecond). */
export const SAME_TIME = 1e-6;
/** The most points a path is drawn through, and the most tick marks along it. */
export const MAX_SAMPLES = 480;
export const MAX_TICKS = 90;

/** Whether the clip's position moves: either number has keys. */
export const positionKeyed = (clip: Animatable): boolean => POSITION.some((p) => isKeyed(clip, p));

// ---- the canvas -------------------------------------------------------------

/** Where a position lands on the canvas: the frame's centre, the position as fractions of the
 *  frame, and `shift` — what the engine's integer geometry adds to the box's centre at the playhead
 *  (overlay's even rounding, a letterbox's odd pixel). */
export interface PathFrame {
	canvas: Canvas;
	shift: Pt;
}

export function centreOf(f: PathFrame, posX: number, posY: number): Pt {
	return { x: f.canvas.width * (0.5 + posX) + f.shift.x, y: f.canvas.height * (0.5 + posY) + f.shift.y };
}

/** The position that puts the clip's centre at `p`: `centreOf`, undone. */
export function positionOf(f: PathFrame, p: Pt): Pos {
	return { pos_x: (p.x - f.shift.x) / f.canvas.width - 0.5, pos_y: (p.y - f.shift.y) / f.canvas.height - 0.5 };
}

/** The frame for a clip drawn as `box` at `pose`: the shift that makes `centreOf` meet the box. */
export function pathFrame(canvas: Canvas, box: Pick<Box, 'cx' | 'cy'>, pose: Pos): PathFrame {
	const bare = centreOf({ canvas, shift: { x: 0, y: 0 } }, pose.pos_x, pose.pos_y);
	return { canvas, shift: { x: box.cx - bare.x, y: box.cy - bare.y } };
}

// ---- the curve ---------------------------------------------------------------

/** `clip` with `tracks` in force: each replaces the number's keys (the legacy bundle too), as
 *  `set_property_tracks` does — the path of an edit before it is written. */
export function withTracks<T extends Animatable>(clip: T, tracks: readonly PropertyTrack[]): T {
	const props = new Set(tracks.map((t) => t.prop));
	return { ...clip, channels: [...(clip.channels ?? []).filter((c) => !props.has(c.prop)), ...tracks.map((t) => ({ prop: t.prop, keys: t.keys }))] };
}

/** The eased polyline of a number every renderer draws, read at clip-local seconds. */
function channel(clip: Animatable, prop: PosProp): (t: number) => number {
	const curve = propertyCurve(clip, prop);
	const still = staticValue(clip, prop);
	return (t) => interpolate(curve, t) ?? still;
}

/** The clip's centre on the canvas at clip-local `t`. */
export function pathAt(clip: Animatable, f: PathFrame): (t: number) => Pt {
	const fx = channel(clip, 'pos_x');
	const fy = channel(clip, 'pos_y');
	return (t) => centreOf(f, fx(t), fy(t));
}

/** Every moment a position key sits at within the clip, earliest first (`pos_x`'s and `pos_y`'s
 *  together; keys past the clip's end are not on its path). */
export function keyTimes(clip: Animatable, duration: number): number[] {
	const all = POSITION.flatMap((p) => propertyKeys(clip, p).map((k) => k.time))
		.filter((t) => t >= 0 && t <= duration + SAME_TIME)
		.sort((a, b) => a - b);
	const out: number[] = [];
	for (const t of all) if (out.length === 0 || t - out[out.length - 1] > SAME_TIME) out.push(t);
	return out;
}

const usable = (fps: number) => Number.isFinite(fps) && fps > 0;

/** The moments a path is sampled at: every frame of the clip (every few when that is more than
 *  `max`), its end, and every key. */
export function sampleTimes(duration: number, fps: number, keys: readonly number[], max = MAX_SAMPLES): number[] {
	if (!(duration > 0)) return [0];
	const times = [0, duration, ...keys.filter((t) => t > 0 && t < duration)];
	if (usable(fps)) {
		const frames = Math.floor(duration * fps + 1e-9);
		const stride = Math.max(1, Math.ceil(frames / Math.max(max - keys.length - 2, 1)));
		for (let k = stride; k <= frames; k += stride) times.push(k / fps);
	}
	times.sort((a, b) => a - b);
	return times.filter((t, i) => i === 0 || t - times[i - 1] > 1e-9 && t <= duration);
}

export interface PathSample extends Pt {
	t: number;
}

/** The path: the clip's centre at each sampled moment of its duration. A hold is the straight
 *  jump between the frame before it and the frame it lands on. */
export function samplePath(clip: Animatable, f: PathFrame, duration: number, fps: number): PathSample[] {
	const at = pathAt(clip, f);
	return sampleTimes(duration, fps, keyTimes(clip, duration)).map((t) => ({ t, ...at(t) }));
}

/** An SVG path through the points (canvas px). */
export function pathData(points: readonly Pt[]): string {
	return points.map((p, i) => `${i === 0 ? 'M' : 'L'}${p.x.toFixed(2)} ${p.y.toFixed(2)}`).join(' ');
}

// ---- keys ----------------------------------------------------------------------

/** A position key: the clip's centre at a moment either number has a key at. */
export interface PathKey extends Pt, Pos {
	/** Clip-local seconds. */
	time: number;
	/** Whether each number has a key of its own here (the other is read off its curve). */
	keyed: Record<PosProp, boolean>;
}

export function pathKeys(clip: Animatable, f: PathFrame, duration: number): PathKey[] {
	const at = POSITION.map((p) => ({ fn: channel(clip, p), keys: propertyKeys(clip, p) }));
	return keyTimes(clip, duration).map((time) => {
		const pos_x = at[0].fn(time);
		const pos_y = at[1].fn(time);
		const keyed = { pos_x: at[0].keys.some((k) => Math.abs(k.time - time) <= SAME_TIME), pos_y: at[1].keys.some((k) => Math.abs(k.time - time) <= SAME_TIME) };
		return { time, pos_x, pos_y, ...centreOf(f, pos_x, pos_y), keyed };
	});
}

/** The key at the playhead, or the nearest within a frame of it: its index, else -1. */
export function hereKey(keys: readonly PathKey[], local: number, fps: number): number {
	const reach = (usable(fps) ? 1 / fps : 0.04) + 1e-9;
	let best = -1;
	keys.forEach((k, i) => {
		const d = Math.abs(k.time - local);
		if (d <= reach && (best < 0 || d < Math.abs(keys[best].time - local))) best = i;
	});
	return best;
}

/** Where the playhead goes to be at a key: the key's moment, or — a key at the very end of the
 *  clip, where the clip is no longer under the playhead — its last frame. Timeline seconds. */
export function seekTarget(clipStart: number, duration: number, time: number, fps: number): number {
	const last = usable(fps) ? Math.max(duration - 1 / fps, 0) : Math.max(duration - 1e-3, 0);
	return clipStart + (time >= duration - SAME_TIME ? Math.min(time, last) : time);
}

// ---- speed ticks ---------------------------------------------------------------

export interface PathTick extends Pt {
	t: number;
	/** The unit normal of the path there. */
	nx: number;
	ny: number;
}

/** Tick marks along the path at equal steps of time (every frame, or every few when that is
 *  more than `max`), across it: close together where the clip is slow, wide apart where it is fast.
 *  Not where a key's dot is (`avoid`) and not where the clip stands still. */
export function pathTicks(clip: Animatable, f: PathFrame, duration: number, fps: number, avoid: readonly number[], max = MAX_TICKS): PathTick[] {
	if (!usable(fps) || !(duration > 0)) return [];
	const frames = Math.floor(duration * fps + 1e-9);
	const step = Math.max(1, Math.ceil(frames / max));
	const at = pathAt(clip, f);
	const half = 0.5 / fps;
	const out: PathTick[] = [];
	for (let k = 0; k < frames; k += step) {
		const t = k / fps;
		if (avoid.some((a) => Math.abs(a - t) < half)) continue;
		const a = at(Math.max(t - half, 0));
		const b = at(Math.min(t + half, duration));
		const len = Math.hypot(b.x - a.x, b.y - a.y);
		if (len < 1e-3) continue;
		out.push({ t, ...at(t), nx: -(b.y - a.y) / len, ny: (b.x - a.x) / len });
	}
	return out;
}

/** An SVG path of the ticks, each `2 * half` long (canvas px). */
export function tickPath(ticks: readonly PathTick[], half: number): string {
	return ticks
		.map((k) => `M${(k.x - k.nx * half).toFixed(2)} ${(k.y - k.ny * half).toFixed(2)} L${(k.x + k.nx * half).toFixed(2)} ${(k.y + k.ny * half).toFixed(2)}`)
		.join(' ');
}

// ---- hit testing ---------------------------------------------------------------

/** The point of the path nearest `p`, and the moment it is at (interpolated along the segment). */
export function nearestOnPath(samples: readonly PathSample[], p: Pt): { t: number; x: number; y: number; distance: number } | null {
	if (samples.length === 0) return null;
	let best: { t: number; x: number; y: number; distance: number } | null = null;
	const consider = (t: number, x: number, y: number) => {
		const distance = Math.hypot(p.x - x, p.y - y);
		if (!best || distance < best.distance) best = { t, x, y, distance };
	};
	if (samples.length === 1) consider(samples[0].t, samples[0].x, samples[0].y);
	for (let i = 0; i + 1 < samples.length; i++) {
		const a = samples[i];
		const b = samples[i + 1];
		const dx = b.x - a.x;
		const dy = b.y - a.y;
		const len2 = dx * dx + dy * dy;
		const u = len2 > 0 ? Math.min(Math.max(((p.x - a.x) * dx + (p.y - a.y) * dy) / len2, 0), 1) : 0;
		consider(a.t + u * (b.t - a.t), a.x + u * dx, a.y + u * dy);
	}
	return best;
}

/** The key under `p` — the nearest dot within `radius` canvas px — as an index into `keys`, else -1. */
export function keyAt(keys: readonly PathKey[], p: Pt, radius: number): number {
	let best = -1;
	let bestD = radius;
	keys.forEach((k, i) => {
		const d = Math.hypot(k.x - p.x, k.y - p.y);
		if (d <= bestD) {
			best = i;
			bestD = d;
		}
	});
	return best;
}

/** A clip-local moment brought to a frame of the cut (its grid, not the clip's, as the dope sheet
 *  lands a key) and held to the clip. */
export function snapKeyTime(clipStart: number, t: number, duration: number, fps: number): number {
	return Math.min(Math.max(snapToFrame(clipStart + t, fps) - clipStart, 0), duration);
}

// ---- edits ---------------------------------------------------------------------

/**
 * The tracks that move the position key at `time` to `to` (only the numbers that moved): the
 * number's keys with that one's value replaced — how it leaves kept, a key it has none at yet
 * splitting the segment it lands in — or, for a number with no keys of its own, keys at every
 * position key (`times`) that hold its value everywhere but `time` and follow the other number's
 * easing. A number written whose keys came from the legacy bundle leaves the bundle alone: the
 * track detaches just that number (`set_property_keys`). Empty when nothing moved.
 */
export function moveKeyTracks(clip: Animatable, time: number, to: Partial<Pos>, times: readonly number[]): PropertyTrack[] {
	const tracks: PropertyTrack[] = [];
	for (const prop of POSITION) {
		const value = to[prop];
		if (value === undefined || !Number.isFinite(value)) continue;
		const keys = propertyKeys(clip, prop);
		if (isKeyed(clip, prop)) {
			const own = keys.find((k) => Math.abs(k.time - time) <= SAME_TIME);
			tracks.push({ prop, keys: upsertKey(keys, { time: own?.time ?? time, value }) });
			continue;
		}
		const other = propertyKeys(clip, prop === 'pos_x' ? 'pos_y' : 'pos_x');
		const still = staticValue(clip, prop);
		tracks.push({
			prop,
			keys: times.map((t) => {
				const key: PropertyKey = { time: t, value: Math.abs(t - time) <= SAME_TIME ? value : still };
				const easing = other.find((k) => Math.abs(k.time - t) <= SAME_TIME)?.easing;
				if (easing && easing !== 'linear') key.easing = ownEasing(easing);
				return key;
			})
		});
	}
	return tracks;
}

/** A drag of a key's dot by `(dx, dy)` canvas px from where it was pressed: snapping brings it onto
 *  the frame's edges and centre lines and the other pictures (`snap`, as the transform handles
 *  snap a box's centre), `axis` (Shift) keeps it to the axis it moved most along. Answers the
 *  position it lands on, the guides it snapped to and the tracks that write it (only the numbers
 *  that moved, rounded as the handles write them). */
export function dragKey(
	clip: Animatable,
	key: PathKey,
	f: PathFrame,
	dx: number,
	dy: number,
	snap: Snapping,
	axis: boolean,
	times: readonly number[]
): { pos: Pos; guides: Guide[]; tracks: PropertyTrack[] } {
	// Snapped and written in the frame's own units, where the centre is `0.5 + position` of it: the shift is
	// the playhead pose's pixel rounding, which a position meant to land on a centre line must not inherit.
	const start = { ...DEFAULT_TRANSFORM, pos_x: key.pos_x, pos_y: key.pos_y };
	const at = { cx: f.canvas.width * (0.5 + key.pos_x), cy: f.canvas.height * (0.5 + key.pos_y), w: 0, h: 0, angle: 0 };
	const m = movePose(start, at, dx, dy, f.canvas, snap, axis);
	const moved = changed(start, m.patch);
	return {
		pos: { pos_x: m.patch.pos_x ?? key.pos_x, pos_y: m.patch.pos_y ?? key.pos_y },
		guides: m.guides,
		tracks: moveKeyTracks(clip, key.time, { pos_x: moved.pos_x, pos_y: moved.pos_y }, times)
	};
}

/** The tracks that put a position key at clip-local `time`: every number that is keyed and has
 *  none there gets one at the value its curve has, so the path does not change shape (the key
 *  splits the segment it lands in). Empty when there is nothing to add. */
export function addKeyTracks(clip: Animatable, time: number): PropertyTrack[] {
	const tracks: PropertyTrack[] = [];
	for (const prop of POSITION) {
		if (!isKeyed(clip, prop)) continue;
		const keys = propertyKeys(clip, prop);
		if (keys.some((k) => Math.abs(k.time - time) <= SAME_TIME)) continue;
		tracks.push({ prop, keys: upsertKey(keys, { time, value: channel(clip, prop)(time) }) });
	}
	return tracks;
}

// ---- the view toggle -------------------------------------------------------------

/** Whether the motion path is drawn (on by default): a viewer's choice, remembered like the
 *  timeline's minimap, not part of the cut. */
export const MOTION_PATH_KEY = 'kerf.preview.motionPath';

export function loadMotionPath(): boolean {
	try {
		return localStorage.getItem(MOTION_PATH_KEY) !== '0';
	} catch {
		return true;
	}
}

export function saveMotionPath(on: boolean): void {
	try {
		localStorage.setItem(MOTION_PATH_KEY, on ? '1' : '0');
	} catch {
		/* ignore a blocked store */
	}
}
