// The speed ramp editor's pure half: the graph's axes (time across the clip, speed on a log scale), the curve
// the keys draw, and the edits the editor makes to a list of keys — add one at the playhead, drag one, set its
// easing, delete one, apply a preset. The map the keys mean is `speed-ramp.ts`'s (kerf-core's); every list
// that leaves here is one `ramp_is_valid` accepts, sorted, with times a `MIN_KEY_GAP` apart.

import {
	clipDuration,
	clipRamp,
	MAX_SPEED,
	MAX_SPEED_KEYS,
	MIN_SPEED,
	rampSpeedAt,
	speedAt,
	speedMag,
	type Windowed
} from './speed-ramp';
import type { SpeedKey } from './types';

/** Closest two keys may sit, seconds: a key dragged against its neighbour stops here. */
export const MIN_KEY_GAP = 0.02;

/** The speeds the graph shows at the least, as a decade each way from 1x. */
export const AXIS_LO = 0.1;
export const AXIS_HI = 10;

/** A speed within this share of 1x is 1x (a drag lands on normal speed rather than 0.98x). */
const SNAP_UNITY = 0.04;

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);
const byTime = (a: SpeedKey, b: SpeedKey) => a.time - b.time;

/** A speed as the editor writes it: held to the engine's range, two significant digits, 1x when it is near. */
export function roundSpeed(v: number): number {
	const s = clamp(Number.isFinite(v) ? v : 1, MIN_SPEED, MAX_SPEED);
	if (Math.abs(s - 1) < SNAP_UNITY) return 1;
	return clamp(Number(s.toPrecision(2)), MIN_SPEED, MAX_SPEED);
}

// ---- axes ---------------------------------------------------------------------

/** The speed axis: a log scale from `lo` to `hi` over `height` px with `pad` px above and below. */
export interface SpeedAxis {
	lo: number;
	hi: number;
	height: number;
	pad: number;
}

/** The decades the graph spans: 0.1x-10x, wider when a key lies outside them. */
export function speedRange(keys: readonly SpeedKey[]): [number, number] {
	const speeds = keys.map((k) => k.speed).filter(Number.isFinite);
	const lo = Math.min(AXIS_LO, ...speeds);
	const hi = Math.max(AXIS_HI, ...speeds);
	return [10 ** Math.floor(Math.log10(lo) + 1e-9), 10 ** Math.ceil(Math.log10(hi) - 1e-9)];
}

/** The pixel row of `speed` (up is faster). */
export function yOfSpeed(a: SpeedAxis, speed: number): number {
	const t = (Math.log(clamp(speed, a.lo, a.hi)) - Math.log(a.lo)) / (Math.log(a.hi) - Math.log(a.lo));
	return a.pad + (1 - t) * Math.max(a.height - 2 * a.pad, 1);
}

/** The speed at pixel row `y`, held to the axis. */
export function speedAtY(a: SpeedAxis, y: number): number {
	const t = 1 - (y - a.pad) / Math.max(a.height - 2 * a.pad, 1);
	return clamp(Math.exp(Math.log(a.lo) + clamp(t, 0, 1) * (Math.log(a.hi) - Math.log(a.lo))), a.lo, a.hi);
}

/** The ticks of the speed axis: every power of ten it spans, with 1x among them. */
export function speedTicks(a: Pick<SpeedAxis, 'lo' | 'hi'>): number[] {
	const out: number[] = [];
	for (let v = a.lo; v <= a.hi * (1 + 1e-9); v *= 10) out.push(Number(v.toPrecision(3)));
	return out;
}

/** `0.1×`, `1×`, `10×`: a tick's label. */
export const speedLabel = (v: number): string => `${Number(v.toPrecision(3))}×`;

/** The time axis: `span` seconds across `width` px with `pad` px each side. */
export interface TimeAxis {
	span: number;
	width: number;
	pad: number;
}

/** What the time axis covers: the clip's length, or past it to the last key (shown shaded). */
export function timeSpan(clip: Windowed, keys: readonly SpeedKey[]): number {
	const last = keys.length ? keys[keys.length - 1].time : 0;
	return Math.max(clipDuration(clip), last, MIN_KEY_GAP);
}

export const xOfTime = (a: TimeAxis, t: number): number => a.pad + (clamp(t, 0, a.span) / a.span) * Math.max(a.width - 2 * a.pad, 1);

export const timeAtX = (a: TimeAxis, x: number): number => clamp(((x - a.pad) / Math.max(a.width - 2 * a.pad, 1)) * a.span, 0, a.span);

// ---- the curve ----------------------------------------------------------------

/** The speed curve over `[0, span]` as an SVG path: flat to the first key and from the last, a straight line
 *  along a linear segment, a step where a hold ends. `x` / `y` place a time and a speed. */
export function speedPath(keys: readonly SpeedKey[], span: number, x: (t: number) => number, y: (v: number) => number): string {
	if (keys.length === 0) return '';
	const pts: string[] = [];
	const at = (t: number, v: number) => pts.push(`${pts.length ? 'L' : 'M'}${x(t).toFixed(2)} ${y(v).toFixed(2)}`);
	at(0, keys[0].speed);
	at(keys[0].time, keys[0].speed);
	for (let i = 0; i + 1 < keys.length; i++) {
		const [k, n] = [keys[i], keys[i + 1]];
		if (k.easing === 'hold') at(n.time, k.speed);
		at(n.time, n.speed);
	}
	const last = keys[keys.length - 1];
	at(Math.max(span, last.time), last.speed);
	return pts.join('');
}

/** The ramp as a small picture for a clip's badge: its curve in a `w` x `h` box on the same log axis (empty for a
 *  clip without a ramp). */
export function miniCurve(clip: Windowed, w: number, h: number): string {
	const keys = clipRamp(clip);
	if (!keys) return '';
	const span = timeSpan(clip, keys);
	const [lo, hi] = speedRange(keys);
	const speed: SpeedAxis = { lo, hi, height: h, pad: 1 };
	const time: TimeAxis = { span, width: w, pad: 1 };
	return speedPath(keys, span, (t) => xOfTime(time, t), (v) => yOfSpeed(speed, v));
}

// ---- edits --------------------------------------------------------------------

/** The keys of a clip's ramp, as a fresh list (none when it has no ramp the map reads). */
export const keysOf = (clip: Windowed): SpeedKey[] => (clipRamp(clip) ?? []).map((k) => ({ ...k }));

const keyOf = (time: number, speed: number, easing?: 'linear' | 'hold'): SpeedKey =>
	easing === 'hold' ? { time, speed, easing } : { time, speed };

/** Whether another key fits: under the cap and not on one already there. */
export function canAddKey(keys: readonly SpeedKey[], t: number): boolean {
	return keys.length < MAX_SPEED_KEYS && !keys.some((k) => Math.abs(k.time - t) < MIN_KEY_GAP);
}

/**
 * `keys` with one more at clip-local time `t`, at the speed the clip runs there now — so the curve does not move
 * (a key inside a hold holds too). A clip with no ramp gets a key at its start as well, both at its constant speed.
 * `null` when it would not fit (`canAddKey`).
 */
export function addKeyAt(clip: Windowed, t: number): SpeedKey[] | null {
	const keys = keysOf(clip);
	if (!canAddKey(keys, t)) return null;
	const speed = speedAt(clip, t);
	const before = keys.filter((k) => k.time <= t).at(-1);
	const key = keyOf(t, speed, before?.easing);
	if (keys.length === 0 && t >= MIN_KEY_GAP) keys.push(keyOf(0, speed));
	return [...keys, key].sort(byTime);
}

/** Drop key `i`; the last one going leaves no ramp at all (an empty list). */
export function removeKey(keys: readonly SpeedKey[], i: number): SpeedKey[] {
	return keys.length <= 1 ? [] : keys.filter((_, j) => j !== i).map((k) => ({ ...k }));
}

/** Key `i` set to `easing` (the segment that leaves it); a list that already says so comes back as it was. */
export function setKeyEasing(keys: readonly SpeedKey[], i: number, easing: 'linear' | 'hold'): SpeedKey[] {
	return keys.map((k, j) => (j === i ? keyOf(k.time, k.speed, easing) : { ...k }));
}

/** Key `i` moved to `time` and `speed`: its time held between its neighbours' (never past them: a drag does not
 *  reorder) and inside `[0, span]`, its speed rounded as `roundSpeed` does. */
export function moveKey(keys: readonly SpeedKey[], i: number, time: number, speed: number, span: number): SpeedKey[] {
	const lo = i > 0 ? keys[i - 1].time + MIN_KEY_GAP : 0;
	const hi = i + 1 < keys.length ? keys[i + 1].time - MIN_KEY_GAP : Math.max(span, lo);
	const t = clamp(time, lo, Math.max(hi, lo));
	return keys.map((k, j) => (j === i ? keyOf(t, roundSpeed(speed), k.easing) : { ...k }));
}

/** Whether two lists are the same ramp (a gesture that ends where it began writes nothing). */
export function sameKeys(a: readonly SpeedKey[], b: readonly SpeedKey[]): boolean {
	return (
		a.length === b.length &&
		a.every((k, i) => k.time === b[i].time && k.speed === b[i].speed && (k.easing ?? 'linear') === (b[i].easing ?? 'linear'))
	);
}

// ---- presets ------------------------------------------------------------------

/** What a preset is built from: the clip's length and the playhead on it (clip-local seconds), and the speed it
 *  is built around (the clip's constant speed). */
export interface PresetContext {
	duration: number;
	at: number;
	base: number;
}

export interface RampPreset {
	id: string;
	label: string;
	hint: string;
	make(c: PresetContext): SpeedKey[];
}

/** A list of keys from `[time, factor]` pairs of `base`, in order and inside the clip; a pair that would sit
 *  within `MIN_KEY_GAP` of the one before it is left out. */
function around(base: number, duration: number, pairs: readonly (readonly [number, number])[]): SpeedKey[] {
	const out: SpeedKey[] = [];
	for (const [t, factor] of pairs) {
		const time = clamp(t, 0, Math.max(duration, 0));
		const prev = out.at(-1);
		if (prev && time < prev.time + MIN_KEY_GAP) continue;
		out.push(keyOf(time, clamp(base * factor, MIN_SPEED, MAX_SPEED)));
	}
	return out;
}

/** Seconds either side of the playhead the slow-mo preset eases over. */
const SLOW_MO_SPAN = 0.5;

export const RAMP_PRESETS: readonly RampPreset[] = [
	{
		id: 'ramp-up',
		label: 'Ramp up',
		hint: 'Starts at the clip’s speed and speeds up to four times that by the end.',
		make: ({ duration, base }) => around(base, duration, [[0, 1], [duration, 4]])
	},
	{
		id: 'ramp-down',
		label: 'Ramp down',
		hint: 'Starts at the clip’s speed and slows to a quarter of it by the end.',
		make: ({ duration, base }) => around(base, duration, [[0, 1], [duration, 0.25]])
	},
	{
		id: 'slow-mo',
		label: 'Slow-mo at playhead',
		hint: 'Normal speed, easing down to 0.3× at the playhead and back up half a second either side.',
		make: ({ duration, at, base }) => {
			const [lo, hi] = [Math.max(0, at - SLOW_MO_SPAN), Math.min(duration, at + SLOW_MO_SPAN)];
			const pairs: [number, number][] = [[at, 0.3]];
			if (at - lo >= MIN_KEY_GAP) pairs.unshift([lo, 1]);
			if (hi - at >= MIN_KEY_GAP) pairs.push([hi, 1]);
			return around(base, duration, pairs);
		}
	},
	{
		id: 'speed-up-middle',
		label: 'Speed up middle',
		hint: 'Normal speed at either end with a four-times stretch through the middle of the clip.',
		make: ({ duration, base }) =>
			around(base, duration, [[0, 1], [duration * 0.3, 1], [duration * 0.42, 4], [duration * 0.58, 4], [duration * 0.7, 1]])
	}
];

/** The preset's keys for `clip` with the playhead at clip-local `at`, built around the clip's constant speed. */
export function presetKeys(preset: RampPreset, clip: Windowed, at: number): SpeedKey[] {
	return preset.make({ duration: clipDuration(clip), at: clamp(at, 0, clipDuration(clip)), base: speedMag(clip) });
}

// ---- words --------------------------------------------------------------------

/** `0.3×`, `1×`, `4.5×`: a speed as the editor says it (three significant digits; a badge says two). */
export function fmtSpeed(v: number, digits = 3): string {
	return `${Number(v.toPrecision(digits))}×`;
}

/** A ramp in a few words: `Ramp · 0.3×–4×` (its slowest and fastest key); nothing for a clip without one. */
export function rampSummary(clip: Windowed): string {
	const keys = clipRamp(clip);
	if (!keys) return '';
	const speeds = keys.map((k) => k.speed);
	const [lo, hi] = [Math.min(...speeds), Math.max(...speeds)];
	return lo === hi ? `Ramp · ${fmtSpeed(lo)}` : `Ramp · ${fmtSpeed(lo)}–${fmtSpeed(hi)}`;
}

/** The speed at clip-local `t`, as the graph's readout says it. */
export const speedNow = (keys: readonly SpeedKey[], t: number): number => (keys.length ? rampSpeedAt(keys, t) : 1);
