// The dope sheet's editing layer — pure. One row per keyed number of the selected clip
// (`channels.ts` decides what drives it), diamonds on one time axis in clip-local seconds, and
// every gesture turned into the tracks it rewrites: a retime, an Alt-duplicate, a delete, a paste,
// an easing. A gesture writes one `set_property_tracks` edit with only the rows it touched.
//
// A key is named by its number and its time (`KeyRef`), not by its index: an edit re-sorts the
// rows, and the selection that comes back is the keys where the gesture put them.

import { PROPERTIES, clampProperty, isKeyed, ownEasing, propertyKeys } from './channels';
import { EASING_CHOICES, easedPoints, easingId } from './easing';
import { snapToFrame } from './frames';
import { rowName } from './property-labels';
import { fmtSpeed, keysOf, speedPath } from './speed-ramp-edit';
import type { Animatable } from './channels';
import type { Windowed } from './speed-ramp';
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

/** `Scale`, `Position X`, … for a row's label: the Inspector's own words (`property-labels.ts`). */
export const rowLabel = (p: Property): string => rowName(p);

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

/**
 * The part of a clipboard a clip can take: the keys of numbers it has (`allowed`, what its Inspector has rows for:
 * `clipProperties`), the earliest of those again at offset 0 so a paste still puts it at the playhead. `dropped` is
 * the numbers left out, in order; `board` is null when nothing fits.
 */
export function fitBoard(board: KeyClipboard, allowed: readonly Property[]): { board: KeyClipboard | null; dropped: Property[] } {
	const keep = board.keys.filter((k) => allowed.includes(k.prop));
	const dropped = PROPERTIES.filter((p) => board.keys.some((k) => k.prop === p) && !allowed.includes(p));
	if (keep.length === 0) return { board: null, dropped };
	const first = Math.min(...keep.map((k) => k.offset));
	return { board: { keys: keep.map((k) => ({ ...k, offset: k.offset - first })) }, dropped };
}

/** The copied keys put on `clip` with the earliest at `at` (clip-local seconds, at least 0); a
 *  key there gives way. Selects what was pasted. Only numbers in `allowed` are put (all, when it is not given). */
export function pasteKeys(clip: Animatable, copied: KeyClipboard, at: number, allowed?: readonly Property[]): DopeEdit {
	const board = allowed ? (fitBoard(copied, allowed).board ?? { keys: [] }) : copied;
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
 *  otherwise reach the clip behind the panel — a ripple delete, a duplicate, a split. The step
 *  and jump keys (a frame, a second) move the selected keys instead of the playhead while any
 *  is selected (`nudgeOf`); with none they are the editor's. A held key repeats only where its
 *  action allows it. `null` leaves the key to the editor. */
export type DopeKey =
	| 'copy'
	| 'cut'
	| 'paste'
	| 'delete'
	| 'selectAll'
	| 'swallow'
	| 'stepBack'
	| 'stepForward'
	| 'jumpBack'
	| 'jumpForward';

export function dopeKey(action: string | null, repeat: boolean, allowsRepeat: (id: string) => boolean): DopeKey | null {
	const own: Record<string, DopeKey> = {
		'edit.copy': 'copy',
		'edit.cut': 'cut',
		'edit.paste': 'paste',
		'edit.delete': 'delete',
		'edit.selectAll': 'selectAll',
		'edit.rippleDelete': 'swallow',
		'edit.duplicate': 'swallow',
		'edit.split': 'swallow',
		'playback.stepBack': 'stepBack',
		'playback.stepForward': 'stepForward',
		'playback.jumpBack': 'jumpBack',
		'playback.jumpForward': 'jumpForward'
	};
	const what = action ? own[action] : undefined;
	if (!what) return null;
	return repeat && !allowsRepeat(action!) ? 'swallow' : what;
}

// ---- moving keys with the keyboard ----------------------------------------------

/** A run of step / jump presses on the selected keys: how many frames and how many seconds so far. It is one edit,
 *  written when the run has been quiet for a moment (`retimeKeys` over `runShift`). */
export interface NudgeRun {
	frames: number;
	seconds: number;
}

export type Nudge = Extract<DopeKey, 'stepBack' | 'stepForward' | 'jumpBack' | 'jumpForward'>;

/** The nudge a dope key is, or null. */
export function nudgeOf(what: DopeKey | null): Nudge | null {
	return what === 'stepBack' || what === 'stepForward' || what === 'jumpBack' || what === 'jumpForward' ? what : null;
}

/** `run` with one more press of `n`. */
export function extendRun(run: NudgeRun | null, n: Nudge): NudgeRun {
	const at = run ?? { frames: 0, seconds: 0 };
	if (n === 'stepBack') return { ...at, frames: at.frames - 1 };
	if (n === 'stepForward') return { ...at, frames: at.frames + 1 };
	return { ...at, seconds: at.seconds + (n === 'jumpForward' ? 1 : -1) };
}

/** How far a run moves the keys, seconds (a frame is the cut's, so keys keep their distance from each other). */
export function runShift(run: NudgeRun, fps: number): number {
	const rate = Number.isFinite(fps) && fps > 0 ? fps : 30;
	return run.frames / rate + run.seconds;
}

/** The key one row up (`-1`) or down (`1`) nearest in time to `from`, skipping rows with no key; null at the edge. */
export function neighbourKey(rows: readonly DopeRow[], from: KeyRef, dir: -1 | 1): KeyRef | null {
	const at = rows.findIndex((r) => r.prop === from.prop);
	if (at < 0) return null;
	for (let i = at + dir; i >= 0 && i < rows.length; i += dir) {
		const row = rows[i];
		let best: PropertyKey | null = null;
		for (const k of row.keys) if (best === null || Math.abs(k.time - from.time) < Math.abs(best.time - from.time)) best = k;
		if (best) return { prop: row.prop, time: best.time };
	}
	return null;
}

// ---- words and marks --------------------------------------------------------------

/** The corner above the row labels: what is selected, else how many numbers the sheet lists. */
export function sheetStatus(rows: number, selected: number): string {
	if (selected > 0) return `${selected} selected`;
	return rows === 0 ? '' : `${rows} ${rows === 1 ? 'number' : 'numbers'}`;
}

/** Why the easing button has nothing to shape, in a sentence; null when it has. */
export function easingReason(selected: number, targets: number): string | null {
	if (targets > 0) return null;
	return selected === 0
		? 'Select a key to shape the segment it starts.'
		: 'The last key of a row starts no segment, so there is nothing to ease. Select one with a key after it.';
}

/** One ruler label as drawn: its text and its left edge in px. */
export interface TickLabel {
	text: string;
	left: number;
}

/**
 * Where the ruler's labels go. A label sits just right of its tick; one that would run past the edge of the lane
 * sits just left of it, and one that would overlap the label before it, or fit nowhere, is left out (its tick stays).
 * `x` is each tick's place in px, in order; `width` the lane's width; `charPx` the width of one character.
 */
export function fitTickLabels<T extends { x: number; text: string }>(
	ticks: readonly T[],
	width: number,
	charPx = 6.2,
	gap = 3,
	edge = 2
): (T & TickLabel)[] {
	const out: (T & TickLabel)[] = [];
	let end = -Infinity;
	for (const t of ticks) {
		const w = t.text.length * charPx;
		const right = t.x + gap;
		const left = t.x - gap - w;
		const fitsRight = right + w <= width - edge && right >= end + 4;
		const fitsLeft = left >= edge && left >= end + 4;
		const at = fitsRight ? right : fitsLeft ? left : null;
		if (at === null) continue;
		out.push({ ...t, left: at });
		end = at + w;
	}
	return out;
}

/** The hues the graph tells its curves apart by, as theme tokens, handed out by a row's place in the sheet. */
export const CURVE_HUES: readonly string[] = [
	'var(--scope-red)',
	'var(--scope-green)',
	'var(--scope-blue)',
	'var(--agent-400)',
	'var(--orange-500)',
	'var(--text-secondary)'
];

export const curveHue = (index: number): string => CURVE_HUES[((index % CURVE_HUES.length) + CURVE_HUES.length) % CURVE_HUES.length];

/** A key of the clip's speed ramp as the sheet marks it: where it is (clip-local seconds), how fast the clip runs
 *  there, and what it says about it. The ramp is not a property (its own store, its own write): the sheet only shows it. */
export interface RampMark {
	time: number;
	speed: number;
	hold: boolean;
	text: string;
}

export function rampMarks(clip: Windowed, fmtTime: (t: number) => string): RampMark[] {
	return keysOf(clip).map((k) => ({
		time: k.time,
		speed: k.speed,
		hold: k.easing === 'hold',
		text: `${fmtSpeed(k.speed)} at ${fmtTime(k.time)}${k.easing === 'hold' ? ', holds' : ''}`
	}));
}

// ---- stable names, and where focus goes -------------------------------------------------

/** A key's identity within its row: its time, and which of the keys at that time it is (a step has two). Elements are
 *  keyed by it, so a key that is deleted takes its own button with it, not its neighbour's. */
export function keyIds(keys: readonly PropertyKey[]): string[] {
	const seen = new Map<number, number>();
	return keys.map((k) => {
		const n = seen.get(k.time) ?? 0;
		seen.set(k.time, n + 1);
		return `${k.time}#${n}`;
	});
}

/**
 * Where focus goes when the key it was on is gone: the nearest key of the same number in time, else the nearest key of the
 * number nearest in the sheet's order (up before down on a tie, earlier before later), else null (the panel itself).
 */
export function nearestKey(rows: readonly DopeRow[], from: KeyRef): KeyRef | null {
	const at = (p: Property) => PROPERTIES.indexOf(p);
	const own = rows.find((r) => r.prop === from.prop && r.keys.length > 0);
	const pool = own
		? [own]
		: [...rows]
				.filter((r) => r.keys.length > 0)
				.sort((a, b) => Math.abs(at(a.prop) - at(from.prop)) - Math.abs(at(b.prop) - at(from.prop)) || at(a.prop) - at(b.prop))
				.slice(0, 1);
	const row = pool[0];
	if (!row) return null;
	let best = row.keys[0];
	for (const k of row.keys) if (Math.abs(k.time - from.time) < Math.abs(best.time - from.time)) best = k;
	return { prop: row.prop, time: best.time };
}

// ---- a run of key presses, held inside the clip ------------------------------------------

export interface FittedRun {
	/** The run after the press (what it was, when the press moved nothing). */
	run: NudgeRun;
	/** How far it moves the keys, seconds. */
	shift: number;
	/** What stopped the press: the clip's start or end (the keys go up to it), or a key it would land on (it goes no
	 *  further: a run stops one step short of a key, where a drag lets the key it lands on give way). */
	stop: 'start' | 'end' | 'key' | null;
}

const EPS = 1e-9;

/** Whether moving `base` by `shift` would put one of its keys on a key that is not moving. */
export function landsOn(rows: readonly DopeRow[], base: readonly KeyRef[], shift: number): boolean {
	return base.some((b) =>
		(rows.find((r) => r.prop === b.prop)?.keys ?? []).some(
			(k) => !isSelected(base, b.prop, k.time) && Math.abs(k.time - (b.time + shift)) <= SAME_TIME
		)
	);
}

/**
 * One more press of `n` on a run over `base`: the run and shift it comes to. The keys stay on the clip, `0..=duration`
 * (the span the playhead can key: a key already past the end may only come back); at either end the run lands exactly on
 * it and a press that would go further moves nothing; and a press that would land a key on one that is not moving is
 * refused (`stop: 'key'`). So the keys of a run are never lost to it: nothing gives way.
 */
export function fitRun(
	rows: readonly DopeRow[],
	base: readonly KeyRef[],
	prev: NudgeRun | null,
	n: Nudge,
	fps: number,
	duration: number
): FittedRun {
	const was = prev ?? { frames: 0, seconds: 0 };
	const wasShift = runShift(was, fps);
	const cand = extendRun(was, n);
	const want = runShift(cand, fps);
	const times = base.map((k) => k.time);
	const lo = -Math.min(...times);
	const hi = Math.max(0, duration - Math.max(...times));
	const shift = Math.min(Math.max(want, lo), hi);
	const stop = want < lo - EPS ? 'start' : want > hi + EPS ? 'end' : null;
	if (stop && Math.abs(shift - wasShift) < EPS) return { run: was, shift: wasShift, stop };
	if (landsOn(rows, base, shift)) return { run: was, shift: wasShift, stop: 'key' };
	return { run: stop ? { frames: 0, seconds: shift } : cand, shift, stop };
}

/** What a stopped run says, for the corner of the sheet. */
export function stopText(stop: FittedRun['stop']): string {
	return stop === 'start' ? 'at the clip’s start' : stop === 'end' ? 'at the clip’s end' : stop === 'key' ? 'next key in the way' : '';
}

/** The name of a key's easing for a screen reader and a tooltip: nothing for linear, `holds` for a hold. */
export function easingName(e: Easing | undefined): string {
	const id = easingId(e);
	if (id === 'linear') return '';
	if (id === 'hold') return 'holds';
	if (id === 'custom') return 'custom curve';
	return (EASING_CHOICES.find((c) => c.id === id)?.label ?? id).toLowerCase();
}

// ---- the speed ramp's row ----------------------------------------------------------------

/** How far past the clip's end a speed key still gets a place on the axis, as a multiple of the clip's length: a key
 *  beyond it (a ramp may hold keys eleven days out) is counted at the lane's end, not drawn, so it does not shrink everything. */
export const RAMP_REACH = 3;

/** The marks the axis holds, and how many are further out than it goes. */
export function partitionRamp<T extends { time: number }>(marks: readonly T[], duration: number): { inLane: T[]; beyond: T[] } {
	const reach = Math.max(duration, 1e-3) * RAMP_REACH;
	return { inLane: marks.filter((m) => m.time <= reach + EPS), beyond: marks.filter((m) => m.time > reach + EPS) };
}

/**
 * Where a ramp's keys sit in a row `height` px tall: faster up, slower down, on a log scale between the slowest and the
 * fastest of its keys and 1× (so the line says rising or falling even for a ramp of 2× and 4×); a ramp that never leaves 1×
 * is a line through the middle. `baseline` is where 1× is, when it is not the whole row.
 */
export function rampLevels(speeds: readonly number[], height: number, pad = 4): { y: number[]; baseline: number | null } {
	const lo = Math.min(1, ...speeds);
	const hi = Math.max(1, ...speeds);
	const span = Math.max(height - 2 * pad, 0);
	if (!(hi > lo * (1 + EPS))) return { y: speeds.map(() => height / 2), baseline: null };
	const at = (v: number) => pad + (1 - Math.log(v / lo) / Math.log(hi / lo)) * span;
	return { y: speeds.map(at), baseline: at(1) };
}

/** The ramp as a line across the row: flat to its first key, straight along a linear segment, a step where a hold ends. */
export function rampLine(marks: readonly RampMark[], span: number, x: (t: number) => number, height: number, pad = 4): string {
	if (marks.length === 0) return '';
	const { y } = rampLevels(
		marks.map((m) => m.speed),
		height,
		pad
	);
	const at = new Map(marks.map((m, i) => [m.speed, y[i]]));
	return speedPath(
		marks.map((m) => (m.hold ? { time: m.time, speed: m.speed, easing: 'hold' as const } : { time: m.time, speed: m.speed })),
		span,
		x,
		(v) => at.get(v) ?? height / 2
	);
}
