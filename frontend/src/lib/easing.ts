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

/** The true curve's progress at `u` in [0, 1] (a hold is 0 until the end). */
export function curve(e: Easing, u: number): number {
	const x = Math.min(Math.max(u, 0), 1);
	const b = bezier(e);
	if (!b) return e === 'hold' && x < 1 ? 0 : x;
	const [x1, y1, x2, y2] = b;
	const axis = (s: number, p1: number, p2: number) => {
		const r = 1 - s;
		return 3 * r * r * s * p1 + 3 * r * s * s * p2 + s * s * s;
	};
	let lo = 0;
	let hi = 1;
	for (let i = 0; i < 40; i++) {
		const mid = 0.5 * (lo + hi);
		if (axis(mid, x1, x2) < x) lo = mid;
		else hi = mid;
	}
	return axis(0.5 * (lo + hi), y1, y2);
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
