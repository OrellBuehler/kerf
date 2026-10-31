/**
 * Live behaviours (B8b): the faithful TS mirror of kerf-core's `model/behavior.rs`. A `Behavior` is a
 * closed-form function of clip-local time added to (the zoom: multiplied into) one or more of a clip's transform
 * numbers on top of its static transform and its keys — a sway and a spin move the rotation, a breath the zoom,
 * a drift and a shake the position. Only those four numbers move, never opacity, a colour, a mask or the crop.
 *
 * Pure: this file knows the arithmetic (`term`, `behaved`), the limits (`checkBehavior`, `sanitized`,
 * `isNeutral`), a head move (`shifted`, `rebaseBehaviors`), the words for the Inspector and the defaults the UI adds. `channels.ts` builds the clip's resolver on it (`propertyAt` = the picture with
 * behaviours, `baseAt` = the pose without them); `behavior-ops.ts` has the clip-level edits (flags, bake, the
 * harness's set). `fixtures/behaviors-corpus.json`, written by kerf-core, pins every number
 * (`behaviors.test.ts`).
 *
 * `phase` is clip-local seconds added to the clock: a head cut shifts it by the cut, so the motion carries on from
 * where it was. A behaviour that cannot move anything (an amount or a speed of 0) is kept but is not live.
 */
import {
	MAX_BEHAVIOR_HZ,
	MAX_BEHAVIOR_PHASE,
	MAX_BEHAVIORS,
	MAX_BREATHE,
	MAX_DRIFT,
	MAX_SHAKE,
	MAX_SPIN_TURNS,
	MAX_SWAY_DEGREES
} from './types';
import type { Behavior, BehaviorKind, Property } from './types';

export { MAX_BEHAVIOR_HZ, MAX_BEHAVIOR_PHASE, MAX_BEHAVIORS, MAX_BREATHE, MAX_DRIFT, MAX_SHAKE, MAX_SPIN_TURNS, MAX_SWAY_DEGREES };

/** The most a drift's `angle` is read as (degrees either way). */
const MAX_ANGLE = 36_000;

export const BEHAVIOR_KINDS: readonly BehaviorKind[] = ['sway', 'breathe', 'spin', 'drift', 'shake'];

/** The frequency multiples and weights of a shake's three sines, per axis (`SHAKE_X` / `SHAKE_Y`). */
export const SHAKE_X: readonly (readonly [number, number])[] = [
	[1.0, 0.5],
	[2.3, 0.3],
	[3.7, 0.2]
];
export const SHAKE_Y: readonly (readonly [number, number])[] = [
	[1.3, 0.5],
	[2.9, 0.3],
	[4.1, 0.2]
];

const TAU = Math.PI * 2;

/** A behaviour with every member present, as the backend stores and serializes it. */
export type FullBehavior =
	| { kind: 'sway'; amount: number; speed: number; phase: number }
	| { kind: 'breathe'; amount: number; speed: number; phase: number }
	| { kind: 'spin'; speed: number; phase: number }
	| { kind: 'drift'; speed: number; angle: number; phase: number }
	| { kind: 'shake'; amount: number; speed: number; seed: number; phase: number };

/** The transform numbers a kind moves (`BehaviorKind::drives`). */
export function drivenBy(kind: BehaviorKind): readonly Property[] {
	switch (kind) {
		case 'sway':
		case 'spin':
			return ['rotation'];
		case 'breathe':
			return ['scale'];
		case 'drift':
		case 'shake':
			return ['pos_x', 'pos_y'];
	}
}

/** `b` with the members a short form leaves out filled in: `phase`, `seed` and `angle` default to 0. */
export function full(b: Behavior): FullBehavior {
	const phase = b.phase ?? 0;
	switch (b.kind) {
		case 'sway':
		case 'breathe':
			return { kind: b.kind, amount: b.amount, speed: b.speed, phase };
		case 'spin':
			return { kind: 'spin', speed: b.speed, phase };
		case 'drift':
			return { kind: 'drift', speed: b.speed, angle: b.angle ?? 0, phase };
		case 'shake':
			return { kind: 'shake', amount: b.amount, speed: b.speed, seed: b.seed ?? 0, phase };
	}
}

/** A number as Rust's `Display` writes it (what the refusals quote). */
function display(n: number): string {
	if (Number.isNaN(n)) return 'NaN';
	if (!Number.isFinite(n)) return n < 0 ? '-inf' : 'inf';
	return String(n);
}

/** A number held to `lo..=hi`, a non-number read as 0 (`held`). */
function held(v: number, lo: number, hi: number): number {
	return Number.isFinite(v) ? Math.min(Math.max(v, lo), hi) : 0;
}

/** The integer hash of a seed and a wave's index (`mix`): all `u32` wrapping arithmetic, so it has the same bits
 *  in any language. */
export function mix(seed: number, k: number): number {
	let x = (seed + Math.imul(k, 0x9e3779b9) + 0x7f4a7c15) >>> 0;
	x = (x ^ (x >>> 16)) >>> 0;
	x = Math.imul(x, 0x85ebca6b) >>> 0;
	x = (x ^ (x >>> 13)) >>> 0;
	x = Math.imul(x, 0xc2b2ae35) >>> 0;
	return (x ^ (x >>> 16)) >>> 0;
}

/** The phase (radians, `0..2 pi`) a seed gives wave `k` (`shake_phase`). */
export function shakePhase(seed: number, k: number): number {
	return (mix(seed, k) / 4_294_967_296) * TAU;
}

function within(what: string, v: number, lo: number, hi: number, unit: string): string | undefined {
	if (Number.isFinite(v) && v >= lo && v <= hi) return undefined;
	return `${what} must be a number within ${display(lo)}..=${display(hi)}${unit} (got ${display(v)})`;
}

/** What `Behavior::check` refuses a behaviour for (a number outside its range, or not a number), or `undefined`
 *  when it is fine. The text is the core's, after `invalid argument: `. */
export function behaviorProblem(b: Behavior): string | undefined {
	const phase = (what: string) => within(`${what} phase`, b.phase ?? 0, -MAX_BEHAVIOR_PHASE, MAX_BEHAVIOR_PHASE, ' seconds');
	switch (b.kind) {
		case 'sway':
			return (
				within('sway amount', b.amount, 0, MAX_SWAY_DEGREES, ' degrees') ??
				within('sway speed', b.speed, 0, MAX_BEHAVIOR_HZ, ' Hz') ??
				phase('sway')
			);
		case 'breathe':
			return (
				within('breathe amount', b.amount, 0, MAX_BREATHE, '') ??
				within('breathe speed', b.speed, 0, MAX_BEHAVIOR_HZ, ' Hz') ??
				phase('breathe')
			);
		case 'spin':
			return within('spin speed', b.speed, -MAX_SPIN_TURNS, MAX_SPIN_TURNS, ' turns a second') ?? phase('spin');
		case 'drift':
			return (
				within('drift speed', b.speed, -MAX_DRIFT, MAX_DRIFT, ' frames a second') ??
				within('drift angle', b.angle ?? 0, -MAX_ANGLE, MAX_ANGLE, ' degrees') ??
				phase('drift')
			);
		case 'shake':
			return (
				within('shake amount', b.amount, 0, MAX_SHAKE, ' of the frame') ??
				within('shake speed', b.speed, 0, MAX_BEHAVIOR_HZ, ' Hz') ??
				phase('shake')
			);
	}
}

/** What `check_behaviors` refuses a list for: more than `MAX_BEHAVIORS`, or a number out of range. */
export function behaviorsProblem(list: readonly Behavior[]): string | undefined {
	if (list.length > MAX_BEHAVIORS) return `at most ${MAX_BEHAVIORS} behaviours per clip (got ${list.length})`;
	for (const b of list) {
		const problem = behaviorProblem(b);
		if (problem) return problem;
	}
	return undefined;
}

/** The behaviour held to its ranges (`Behavior::sanitized`): what every reader evaluates, so a hand-edited file
 *  cannot hand a filter a value it refuses. */
export function sanitized(b: Behavior): FullBehavior {
	const f = full(b);
	const phase = held(f.phase, -MAX_BEHAVIOR_PHASE, MAX_BEHAVIOR_PHASE);
	switch (f.kind) {
		case 'sway':
			return { ...f, amount: held(f.amount, 0, MAX_SWAY_DEGREES), speed: held(f.speed, 0, MAX_BEHAVIOR_HZ), phase };
		case 'breathe':
			return { ...f, amount: held(f.amount, 0, MAX_BREATHE), speed: held(f.speed, 0, MAX_BEHAVIOR_HZ), phase };
		case 'spin':
			return { ...f, speed: held(f.speed, -MAX_SPIN_TURNS, MAX_SPIN_TURNS), phase };
		case 'drift':
			return { ...f, speed: held(f.speed, -MAX_DRIFT, MAX_DRIFT), angle: held(f.angle, -MAX_ANGLE, MAX_ANGLE), phase };
		case 'shake':
			return { ...f, amount: held(f.amount, 0, MAX_SHAKE), speed: held(f.speed, 0, MAX_BEHAVIOR_HZ), phase };
	}
}

/** Whether the (sanitized) behaviour can move nothing (`Behavior::is_neutral`): no amount, or no speed. */
export function isNeutral(b: FullBehavior): boolean {
	switch (b.kind) {
		case 'sway':
		case 'breathe':
		case 'shake':
			return b.amount === 0 || b.speed === 0;
		case 'spin':
		case 'drift':
			return b.speed === 0;
	}
}

/** The behaviours that move something, as they are read (held to their ranges), in order (`live_behaviors`). */
export function liveBehaviors(list: readonly Behavior[] | undefined): FullBehavior[] {
	return (list ?? []).map(sanitized).filter((b) => !isNeutral(b));
}

/** Whether a live behaviour moves `prop` (`Clip::behaves`; only a transform number can be). */
export function behaves(list: readonly Behavior[] | undefined, prop: Property): boolean {
	return liveBehaviors(list).some((b) => drivenBy(b.kind).includes(prop));
}

/** The behaviour continuing after the clip's start moved `by` seconds later (negative: earlier): the clock a cut
 *  piece reads is the old one (`Behavior::shifted`). */
export function shifted(b: Behavior, by: number): FullBehavior {
	const f = full(b);
	if (!Number.isFinite(by)) return f;
	return { ...f, phase: Math.min(Math.max(f.phase + by, -MAX_BEHAVIOR_PHASE), MAX_BEHAVIOR_PHASE) };
}

/** The behaviours' share of `rebase_animation`: the start moved `by` seconds later (the head cut) or earlier, so
 *  each one's clock moves with the footage (`rebase_behaviors`). Mutates `clip`. */
export function rebaseBehaviors(clip: { behaviors?: Behavior[] }, by: number) {
	if (by === 0 || !clip.behaviors?.length) return;
	clip.behaviors = clip.behaviors.map((b) => shifted(b, by));
}

interface Wave {
	freq: number;
	weight: number;
	phase: number;
}

/** The three sines a shake throws position x or y by (`Behavior::waves`). */
export function waves(b: FullBehavior, prop: Property): [Wave, Wave, Wave] | null {
	if (b.kind !== 'shake') return null;
	const [table, base] = prop === 'pos_x' ? [SHAKE_X, 0] : prop === 'pos_y' ? [SHAKE_Y, 3] : [null, 0];
	if (!table) return null;
	const wave = (k: number): Wave => ({ freq: b.speed * table[k][0], weight: table[k][1], phase: shakePhase(b.seed, base + k) });
	return [wave(0), wave(1), wave(2)];
}

/** The term this (sanitized) behaviour adds to `prop` at `local` seconds from the clip's start: an offset
 *  (degrees, frame fractions), or for `breathe` the amount of the factor `1 + term`; 0 for a number it does not
 *  drive (`Behavior::term`). The export writes this exact arithmetic as text. */
export function term(b: FullBehavior, prop: Property, local: number): number {
	const u = local + b.phase;
	const drives = drivenBy(b.kind).includes(prop);
	switch (b.kind) {
		case 'sway':
		case 'breathe':
			return drives ? b.amount * Math.sin(TAU * b.speed * u) : 0;
		case 'spin':
			return drives ? b.speed * 360 * u : 0;
		case 'drift':
			if (prop === 'pos_x') return b.speed * Math.cos((b.angle * Math.PI) / 180) * u;
			if (prop === 'pos_y') return b.speed * Math.sin((b.angle * Math.PI) / 180) * u;
			return 0;
		case 'shake': {
			const w = waves(b, prop);
			if (!w) return 0;
			const sine = (x: Wave) => x.weight * Math.sin(TAU * x.freq * u + x.phase);
			return b.amount * (sine(w[0]) + sine(w[1]) + sine(w[2]));
		}
	}
}

/** `base` with every live behaviour that drives `prop` applied at `local` (the zoom multiplies, the rest add), in
 *  list order — the order the export's expression has (`Clip::behaved`). */
export function behavedValue(list: readonly Behavior[] | undefined, prop: Property, base: number, local: number): number {
	if (!list?.length) return base;
	let v = base;
	for (const b of list.map(sanitized)) {
		if (isNeutral(b) || !drivenBy(b.kind).includes(prop)) continue;
		v = prop === 'scale' ? v * (1 + term(b, prop, local)) : v + term(b, prop, local);
	}
	return v;
}

// ---- the words ---------------------------------------------------------------------------------

export const BEHAVIOR_LABELS: Record<BehaviorKind, string> = {
	sway: 'Sway',
	breathe: 'Breathe',
	spin: 'Spin',
	drift: 'Drift',
	shake: 'Shake'
};

/** What each kind does, for a tile's tooltip and the section's rows. */
export const BEHAVIOR_HINTS: Record<BehaviorKind, string> = {
	sway: 'Rocks the picture side to side (rotation), a few degrees at a steady pace',
	breathe: 'Pulses the zoom in and out, as if the picture were breathing',
	spin: 'Turns the picture steadily, a fraction of a turn every second',
	drift: 'Slides the picture steadily in one direction, a slow push across the frame',
	shake: 'Throws the picture about like a handheld camera; the same seed is the same shake'
};

const num = (v: number, digits: number) => String(Number(v.toFixed(digits)));

/** A phrase for a row and a tooltip: "sway 5° at 0.5 Hz" (`Behavior::label`). */
export function behaviorLabel(b: Behavior): string {
	const f = full(b);
	switch (f.kind) {
		case 'sway':
			return `sway ${num(f.amount, 2)}° at ${num(f.speed, 2)} Hz`;
		case 'breathe':
			return `breathe ${num(f.amount * 100, 0)}% at ${num(f.speed, 2)} Hz`;
		case 'spin':
			return `spin ${num(f.speed, 3)} turns/s`;
		case 'drift':
			return `drift ${num(f.speed, 3)} frames/s at ${num(f.angle, 1)}°`;
		case 'shake':
			return `shake ${num(f.amount * 100, 1)}% at ${num(f.speed, 2)} Hz (seed ${f.seed})`;
	}
}

/** The section's summary: the kinds in order, "Sway · Shake" (a kind twice is "Sway ×2"), or "None". */
export function behaviorsSummary(list: readonly Behavior[] | undefined): string {
	if (!list?.length) return 'None';
	const names: BehaviorKind[] = [];
	const counts = new Map<BehaviorKind, number>();
	for (const b of list) {
		if (!counts.has(b.kind)) names.push(b.kind);
		counts.set(b.kind, (counts.get(b.kind) ?? 0) + 1);
	}
	return names.map((k) => (counts.get(k)! > 1 ? `${BEHAVIOR_LABELS[k]} ×${counts.get(k)}` : BEHAVIOR_LABELS[k])).join(' · ');
}

/** The behaviour a new row starts as: a gentle one of its kind (`seed` a random shake when given). */
export function defaultBehavior(kind: BehaviorKind, seed = 0): FullBehavior {
	switch (kind) {
		case 'sway':
			return { kind, amount: 5, speed: 0.5, phase: 0 };
		case 'breathe':
			return { kind, amount: 0.04, speed: 0.3, phase: 0 };
		case 'spin':
			return { kind, speed: 0.1, phase: 0 };
		case 'drift':
			return { kind, speed: 0.02, angle: 0, phase: 0 };
		case 'shake':
			return { kind, amount: 0.01, speed: 8, seed, phase: 0 };
	}
}

/** A seed for a new shake: any `u32`, so two shakes on a clip are not the same one. */
export function randomSeed(): number {
	return Math.floor(Math.random() * 4_294_967_296) >>> 0;
}
