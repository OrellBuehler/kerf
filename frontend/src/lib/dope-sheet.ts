// The dope sheet's editing layer — pure. One row per keyed number of the selected clip
// (`channels.ts` decides what drives it), diamonds on one time axis in clip-local seconds, and
// every gesture turned into the tracks it rewrites: a retime, an Alt-duplicate, a delete, a paste,
// an easing. A gesture writes one `set_property_tracks` edit with only the rows it touched.
//
// A key is named by its number and its time (`KeyRef`), not by its index: an edit re-sorts the
// rows, and the selection that comes back is the keys where the gesture put them.

import { PROPERTIES, clampProperty, isKeyed, ownEasing, propertyKeys, propertyLabel } from './channels';
import { easedPoints } from './easing';
import { snapToFrame } from './frames';
import type { Animatable } from './channels';
import type { Easing, Property, PropertyKey, PropertyTrack } from './types';

/** Two keys of one number this close in time are one moment (`upsert`'s microsecond). */
export const SAME_TIME = 1e-6;

export interface KeyRef {
	prop: Property;
	time: number;
}

export interface DopeRow {
	prop: Property;
	label: string;
	keys: PropertyKey[];
}

/** What a copy holds: each key's number, its time after the earliest copied key, its value and
 *  how it leaves. A paste puts the earliest at the playhead. */
export interface KeyClipboard {
	keys: { prop: Property; offset: number; value: number; easing?: Easing }[];
}

/** The outcome of a gesture: the rows to write (only those it touched) and what to select. */
export interface DopeEdit {
	tracks: PropertyTrack[];
	selected: KeyRef[];
}

const byTime = (a: { time: number }, b: { time: number }) => a.time - b.time;
const isSame = (a: number, b: number) => Math.abs(a - b) <= SAME_TIME;
const plain = (k: PropertyKey): PropertyKey => {
	const out: PropertyKey = { time: k.time, value: k.value };
	if (k.easing && k.easing !== 'linear') out.easing = ownEasing(k.easing);
	return out;
};

/** `Scale`, `Position x`, … for a row's label. */
export function rowLabel(p: Property): string {
	const l = propertyLabel(p);
	return l.charAt(0).toUpperCase() + l.slice(1);
}

/** The rows of a clip: every keyed number, in the order the Inspector lists them. */
export function dopeRows(clip: Animatable): DopeRow[] {
	return PROPERTIES.filter((p) => isKeyed(clip, p)).map((prop) => ({ prop, label: rowLabel(prop), keys: propertyKeys(clip, prop) }));
}

export const isSelected = (sel: readonly KeyRef[], prop: Property, time: number) =>
	sel.some((r) => r.prop === prop && isSame(r.time, time));

/** `sel` with the key toggled in or out. */
export function toggleKey(sel: readonly KeyRef[], key: KeyRef): KeyRef[] {
	return isSelected(sel, key.prop, key.time)
		? sel.filter((r) => !(r.prop === key.prop && isSame(r.time, key.time)))
		: [...sel, key];
}

/** The selected keys that are still there (an edit elsewhere, an undo, a new clip drop the rest). */
export function liveSelection(rows: readonly DopeRow[], sel: readonly KeyRef[]): KeyRef[] {
	return sel.filter((r) => rows.some((row) => row.prop === r.prop && row.keys.some((k) => isSame(k.time, r.time))));
}

/** Every key of every row. */
export const allKeys = (rows: readonly DopeRow[]): KeyRef[] => rows.flatMap((r) => r.keys.map((k) => ({ prop: r.prop, time: k.time })));

/** The keys inside a marquee: rows `r0..=r1` (indices into `rows`) and times `t0..=t1`. */
export function marqueeKeys(rows: readonly DopeRow[], r0: number, r1: number, t0: number, t1: number): KeyRef[] {
	const [ra, rb] = r0 <= r1 ? [r0, r1] : [r1, r0];
	const [ta, tb] = t0 <= t1 ? [t0, t1] : [t1, t0];
	return rows
		.slice(Math.max(0, Math.ceil(ra)), Math.floor(rb) + 1)
		.flatMap((r) => r.keys.filter((k) => k.time >= ta && k.time <= tb).map((k) => ({ prop: r.prop, time: k.time })));
}

/** A marquee's result: the keys it covers, added to `base` when the gesture extends a selection. */
export function mergeSelection(base: readonly KeyRef[], add: readonly KeyRef[]): KeyRef[] {
	const out = [...base];
	for (const k of add) if (!isSelected(out, k.prop, k.time)) out.push(k);
	return out;
}

/** How far a retime may go: no selected key before the clip's start. */
export function clampShift(sel: readonly KeyRef[], dt: number): number {
	if (sel.length === 0) return 0;
	const first = Math.min(...sel.map((r) => r.time));
	return Math.max(dt, -first);
}

/**
 * The shift a drag lands on: the grabbed key on a magnet within `reach` seconds (the playhead,
 * the clip's ends, another key that is not moving), else the grabbed key on a frame of the cut —
 * the timeline's grid, so a clip that starts between two frames still keys on one. The result is
 * held so that no selected key goes before 0.
 */
export function snapShift(
	sel: readonly KeyRef[],
	grabbed: number,
	raw: number,
	opts: { magnets: readonly number[]; reach: number; fps: number; clipStart: number }
): number {
	const target = grabbed + raw;
	let best: number | null = null;
	for (const m of opts.magnets) {
		if (Math.abs(m - target) <= opts.reach && (best === null || Math.abs(m - target) < Math.abs(best - target))) best = m;
	}
	const landed = best ?? snapToFrame(opts.clipStart + target, opts.fps) - opts.clipStart;
	return clampShift(sel, landed - grabbed);
}

/** The magnets of a drag: 0, the clip's end, the playhead (clip-local) and every key that is not
 *  being moved. */
export function dragMagnets(rows: readonly DopeRow[], sel: readonly KeyRef[], duration: number, playhead: number | null): number[] {
	const out = [0, duration];
	if (playhead !== null && playhead >= 0) out.push(playhead);
	for (const r of rows) for (const k of r.keys) if (!isSelected(sel, r.prop, k.time)) out.push(k.time);
	return out;
}

function touched(sel: readonly KeyRef[]): Property[] {
	return PROPERTIES.filter((p) => sel.some((r) => r.prop === p));
}

/** Put `placed` into `keys`: a key of `keys` at the same moment as one of them gives way. */
function landInto(keys: PropertyKey[], placed: PropertyKey[]): PropertyKey[] {
	const kept = keys.filter((k) => !placed.some((p) => isSame(p.time, k.time)));
	return [...kept, ...placed].map(plain).sort(byTime);
}

/**
 * The selected keys moved `dt` seconds (or, with `duplicate`, copies of them put there and the
 * originals left): each keeps its value and how it leaves, and a key it lands on gives way.
 */
export function retimeKeys(clip: Animatable, sel: readonly KeyRef[], dt: number, duplicate = false): DopeEdit {
	const shift = clampShift(sel, dt);
	const tracks: PropertyTrack[] = [];
	const selected: KeyRef[] = [];
	for (const prop of touched(sel)) {
		const keys = propertyKeys(clip, prop);
		const moving = keys.filter((k) => isSelected(sel, prop, k.time));
		const staying = duplicate ? keys : keys.filter((k) => !isSelected(sel, prop, k.time));
		const placed = moving.map((k) => ({ ...plain(k), time: k.time + shift }));
		tracks.push({ prop, keys: landInto(staying, placed) });
		selected.push(...placed.map((k) => ({ prop, time: k.time })));
	}
	return { tracks, selected };
}

/** The selected keys taken out (a number left with none is static again). */
export function deleteKeys(clip: Animatable, sel: readonly KeyRef[]): DopeEdit {
	return {
		tracks: touched(sel).map((prop) => ({
			prop,
			keys: propertyKeys(clip, prop)
				.filter((k) => !isSelected(sel, prop, k.time))
				.map(plain)
		})),
		selected: []
	};
}

/** The selected keys, for a paste. Undefined when nothing is selected. */
export function copyKeys(clip: Animatable, sel: readonly KeyRef[]): KeyClipboard | undefined {
	const keys = touched(sel).flatMap((prop) =>
		propertyKeys(clip, prop)
			.filter((k) => isSelected(sel, prop, k.time))
			.map((k) => ({ prop, k }))
	);
	if (keys.length === 0) return undefined;
	const first = Math.min(...keys.map(({ k }) => k.time));
	return {
		keys: keys.map(({ prop, k }) => {
			const out: KeyClipboard['keys'][number] = { prop, offset: k.time - first, value: k.value };
			if (k.easing && k.easing !== 'linear') out.easing = ownEasing(k.easing);
			return out;
		})
	};
}

/** The copied keys put on `clip` with the earliest at `at` (clip-local seconds, at least 0); a
 *  key there gives way. Selects what was pasted. */
export function pasteKeys(clip: Animatable, board: KeyClipboard, at: number): DopeEdit {
	const start = Math.max(0, Number.isFinite(at) ? at : 0);
	const tracks: PropertyTrack[] = [];
	const selected: KeyRef[] = [];
	for (const prop of PROPERTIES) {
		const mine = board.keys.filter((k) => k.prop === prop);
		if (mine.length === 0) continue;
		const placed = mine.map((k) => {
			const out: PropertyKey = { time: start + k.offset, value: clampProperty(prop, k.value) };
			if (k.easing) out.easing = ownEasing(k.easing);
			return out;
		});
		tracks.push({ prop, keys: landInto(propertyKeys(clip, prop), placed) });
		selected.push(...placed.map((k) => ({ prop, time: k.time })));
	}
	return { tracks, selected };
}

/** The selected keys that start a segment — the ones an easing shapes. */
export function easingTargets(clip: Animatable, sel: readonly KeyRef[]): KeyRef[] {
	return touched(sel).flatMap((prop) => {
		const keys = propertyKeys(clip, prop);
		return keys
			.filter((k, i) => i + 1 < keys.length && keys[i + 1].time - k.time > SAME_TIME && isSelected(sel, prop, k.time))
			.map((k) => ({ prop, time: k.time }));
	});
}

/** `easing` on the segment each selected key starts (`easingTargets`). The selection is kept. */
export function easeKeys(clip: Animatable, sel: readonly KeyRef[], easing: Easing): DopeEdit {
	const targets = easingTargets(clip, sel);
	return {
		tracks: touched(targets).map((prop) => ({
			prop,
			keys: propertyKeys(clip, prop).map((k) => {
				const out = plain(k);
				if (!isSelected(targets, prop, k.time)) return out;
				if (easing === 'linear') delete out.easing;
				else out.easing = ownEasing(easing);
				return out;
			})
		})),
		selected: [...sel]
	};
}

/** The easing every target shares, or undefined when they differ (or there are none). */
export function sharedEasing(clip: Animatable, targets: readonly KeyRef[]): Easing | undefined {
	let found: Easing | undefined;
	let first = true;
	for (const r of targets) {
		// The last key at that moment: of a step's two keys, the one that starts a segment.
		const k = propertyKeys(clip, r.prop).findLast((k) => isSame(k.time, r.time));
		const e = k?.easing ?? 'linear';
		if (first) found = e;
		else if (JSON.stringify(e) !== JSON.stringify(found)) return undefined;
		first = false;
	}
	return found;
}

/** Whether a gesture's tracks change anything (a drag that went nowhere writes nothing). */
export function changes(clip: Animatable, edit: DopeEdit): boolean {
	return edit.tracks.some((t) => JSON.stringify(propertyKeys(clip, t.prop).map(plain)) !== JSON.stringify(t.keys.map(plain)));
}

// ---- the axis -----------------------------------------------------------------

/** The time axis: clip-local seconds `0..=duration` over `width` px, with `pad` px at each end so
 *  a key on either edge is a whole diamond. */
export interface Axis {
	duration: number;
	width: number;
	pad: number;
}

export function xOf(axis: Axis, t: number): number {
	const span = Math.max(1, axis.width - 2 * axis.pad);
	return axis.pad + (axis.duration > 0 ? (t / axis.duration) * span : 0);
}

export function timeAt(axis: Axis, x: number): number {
	const span = Math.max(1, axis.width - 2 * axis.pad);
	return axis.duration > 0 ? ((x - axis.pad) / span) * axis.duration : 0;
}

/** An easing's curve as an SVG path in a `w` × `h` box (value up), drawn from the polyline the
 *  renderers use, so a preset looks the way it plays. */
export function easingPath(e: Easing, w: number, h: number, inset = 2): string {
	const pts = easedPoints([
		[0, 0, e],
		[1, 1, 'linear']
	]);
	const x = (u: number) => inset + u * (w - 2 * inset);
	const y = (v: number) => h - inset - v * (h - 2 * inset);
	return pts.map(([u, v], i) => `${i === 0 ? 'M' : 'L'}${+x(u).toFixed(2)} ${+y(v).toFixed(2)}`).join(' ');
}

/** What a key the editor binds does while the dope sheet has focus: the keys' own copy, cut,
 *  paste, delete and select all, or nothing (`swallow`) for an editing shortcut that would
 *  otherwise reach the clip behind the panel — a ripple delete, a duplicate, a split. A held key
 *  repeats only where its action allows it. `null` leaves the key to the editor. */
export type DopeKey = 'copy' | 'cut' | 'paste' | 'delete' | 'selectAll' | 'swallow';

export function dopeKey(action: string | null, repeat: boolean, allowsRepeat: (id: string) => boolean): DopeKey | null {
	const own: Record<string, DopeKey> = {
		'edit.copy': 'copy',
		'edit.cut': 'cut',
		'edit.paste': 'paste',
		'edit.delete': 'delete',
		'edit.selectAll': 'selectAll',
		'edit.rippleDelete': 'swallow',
		'edit.duplicate': 'swallow',
		'edit.split': 'swallow'
	};
	const what = action ? own[action] : undefined;
	if (!what) return null;
	return repeat && !allowsRepeat(action!) ? 'swallow' : what;
}
