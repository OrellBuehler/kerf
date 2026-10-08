/**
 * Keyframe easing: the faithful TS mirror of kerf-core's `Easing`, `eased_points` and
 * `Clip::keyframe_channel` (model.rs). A key's easing shapes the segment that *leaves* it,
 * and an eased segment *is* a polyline of EASE_STEPS straight pieces through the true curve —
 * so the editor's sampled pose, the harness's edits and the export all read one curve.
 */
import type { Easing, Keyframe } from './types';

/** Straight pieces an eased segment is drawn with (`EASE_STEPS` in model.rs). */
export const EASE_STEPS = 12;

const unit = (v: number) => (Number.isFinite(v) ? Math.min(Math.max(v, 0), 1) : 0);

/** The control points of a curved easing (`undefined` for linear and hold). */
function bezier(e: Easing): [number, number, number, number] | undefined {
	if (e === 'linear' || e === 'hold') return undefined;
	if (e === 'ease_in') return [0.42, 0, 1, 1];
	if (e === 'ease_out') return [0, 0, 0.58, 1];
	if (e === 'ease_in_out') return [0.42, 0, 0.58, 1];
	const b = e.bezier;
	return [unit(b.x1), unit(b.y1), unit(b.x2), unit(b.y2)];
}

/** One axis of the cubic bezier from 0 to 1 with inner control values `p1` and `p2`, at `s`. */
function bezierAxis(s: number, p1: number, p2: number): number {
	const r = 1 - s;
	return 3 * r * r * s * p1 + 3 * r * s * s * p2 + s * s * s;
}

/** The bezier parameter at which its x is `u` (x is monotonic on the unit square: bisection). */
function bezierParam(x1: number, x2: number, u: number): number {
	let lo = 0;
	let hi = 1;
	for (let i = 0; i < 40; i++) {
		const mid = 0.5 * (lo + hi);
		if (bezierAxis(mid, x1, x2) < u) lo = mid;
		else hi = mid;
	}
	return 0.5 * (lo + hi);
}

/** The true curve's progress at `u` in [0, 1] (a hold is 0 until the end). */
export function curve(e: Easing, u: number): number {
	const x = Math.min(Math.max(u, 0), 1);
	const b = bezier(e);
	if (!b) return e === 'hold' && x < 1 ? 0 : x;
	const [x1, y1, x2, y2] = b;
	return bezierAxis(bezierParam(x1, x2, x), y1, y2);
}

type Point = [number, number];

/** The two easings a segment becomes when a key is put `u` of the way along it (0 < u < 1):
 *  the same curve drawn in two pieces, each normalized to its own unit square (de Casteljau).
 *  Holds and lines stay as they are. Exact for the presets and any bezier whose control points
 *  rise; one whose half would need a control point outside the square has it clamped there
 *  (`Easing::split` in model.rs). */
export function splitEasing(e: Easing, u: number): [Easing, Easing] {
	const b = bezier(e);
	if (!b) return [e, e];
	const [x1, y1, x2, y2] = b;
	const lerp = (a: Point, c: Point, s: number): Point => [a[0] + (c[0] - a[0]) * s, a[1] + (c[1] - a[1]) * s];
	const [p0, p1, p2, p3]: Point[] = [[0, 0], [x1, y1], [x2, y2], [1, 1]];
	const s = bezierParam(x1, x2, Math.min(Math.max(u, 0), 1));
	const [a, c, d] = [lerp(p0, p1, s), lerp(p1, p2, s), lerp(p2, p3, s)];
	const [m, n] = [lerp(a, c, s), lerp(c, d, s)];
	const at = lerp(m, n, s);
	const unitSquare = (v: number) => Math.min(Math.max(v, 0), 1);
	const half = (from: Point, h1: Point, h2: Point, to: Point): Easing => {
		const [w, h] = [to[0] - from[0], to[1] - from[1]];
		if (w < 1e-9 || h < 1e-9) return 'linear';
		const norm = (p: Point): Point => [unitSquare((p[0] - from[0]) / w), unitSquare((p[1] - from[1]) / h)];
		const [n1, n2] = [norm(h1), norm(h2)];
		return { bezier: { x1: n1[0], y1: n1[1], x2: n2[0], y2: n2[1] } };
	};
	return [half(p0, a, m, at), half(at, n, d, p3)];
}

/** The polyline a channel of `[time, value, easing]` keys (sorted by time) is drawn as. */
export function easedPoints(keys: [number, number, Easing][]): [number, number][] {
	const out: [number, number][] = [];
	keys.forEach(([t0, v0, easing], i) => {
		out.push([t0, v0]);
		const next = keys[i + 1];
		if (!next) return;
		const [t1, v1] = next;
		if (t1 - t0 < 1e-9) return;
		if (easing === 'linear') return;
		if (easing === 'hold') {
			out.push([t1, v0]);
			return;
		}
		for (let j = 1; j < EASE_STEPS; j++) {
			const u = j / EASE_STEPS;
			out.push([t0 + (t1 - t0) * u, v0 + (v1 - v0) * curve(easing, u)]);
		}
	});
	return out;
}

/** One channel of a clip's keyframes as the polyline every renderer draws. */
export function keyframeChannel(keyframes: Keyframe[], get: (k: Keyframe) => number): [number, number][] {
	const ks = [...keyframes].sort((a, b) => a.time - b.time);
	return easedPoints(ks.map((k) => [k.time, get(k), k.easing ?? 'linear']));
}

/** `key` put into a clip's keyframes (`Clip::insert_keyframe` in model.rs): a key within a
 *  microsecond of one replaces it and keeps how it leaves; one inside a segment splits it, so a
 *  hold stays held and a curve stays the same curve. Returns a new, sorted list. */
export function insertKeyframe(keyframes: Keyframe[], key: Keyframe): Keyframe[] {
	const SAME = 1e-6;
	const sorted = [...keyframes].sort((a, b) => a.time - b.time);
	const out: Keyframe[] = sorted.map((k) => ({ ...k }));
	const placed: Keyframe = { ...key };
	const old = sorted.find((k) => Math.abs(k.time - key.time) <= SAME);
	if (old) {
		if (old.easing) placed.easing = old.easing;
		else delete placed.easing;
	} else {
		const i = sorted.findIndex((k, j) => k.time < key.time && j + 1 < sorted.length && key.time < sorted[j + 1].time);
		if (i >= 0) {
			const [a, b] = [sorted[i], sorted[i + 1]];
			const [before, after] = splitEasing(a.easing ?? 'linear', (key.time - a.time) / (b.time - a.time));
			for (const [k, e] of [[out[i], before], [placed, after]] as [Keyframe, Easing][]) {
				if (e === 'linear') delete k.easing;
				else k.easing = e;
			}
		}
	}
	return [...out.filter((k) => Math.abs(k.time - key.time) > SAME), placed].sort((a, b) => a.time - b.time);
}

/** The refusal a bezier with a control point outside the unit square gets (`validate_easing`
 *  in project.rs), or `undefined` when the easing is fine. */
export function easingProblem(e: Easing | undefined): string | undefined {
	if (!e || typeof e === 'string') return undefined;
	return Object.values(e.bezier).some((v) => !Number.isFinite(v) || v < 0 || v > 1)
		? 'bezier control points must be within 0.0..=1.0 (no overshoot)'
		: undefined;
}

/** The easings offered in the editor, in menu order, with their labels. */
export const EASING_CHOICES: { id: string; label: string; easing: Easing }[] = [
	{ id: 'linear', label: 'Linear', easing: 'linear' },
	{ id: 'ease_in_out', label: 'Ease in-out', easing: 'ease_in_out' },
	{ id: 'ease_out', label: 'Ease out', easing: 'ease_out' },
	{ id: 'ease_in', label: 'Ease in', easing: 'ease_in' },
	{ id: 'hold', label: 'Hold', easing: 'hold' },
	{ id: 'snappy', label: 'Snappy', easing: { bezier: { x1: 0.2, y1: 0.9, x2: 0.3, y2: 1 } } },
	{ id: 'gentle', label: 'Gentle', easing: { bezier: { x1: 0.45, y1: 0.05, x2: 0.55, y2: 0.95 } } },
	{ id: 'drift', label: 'Drift', easing: { bezier: { x1: 0.1, y1: 0.6, x2: 0.4, y2: 1 } } }
];

/** The menu entry an easing is (custom beziers that match none read as `custom`). */
export function easingId(e: Easing | undefined): string {
	const easing = e ?? 'linear';
	if (typeof easing === 'string') return easing;
	const hit = EASING_CHOICES.find(
		(c) =>
			typeof c.easing !== 'string' &&
			(['x1', 'y1', 'x2', 'y2'] as const).every((k) => Math.abs((c.easing as { bezier: Record<string, number> }).bezier[k] - easing.bezier[k]) < 1e-9)
	);
	return hit?.id ?? 'custom';
}
