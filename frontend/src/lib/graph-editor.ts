// The graph editor's pure half: a keyed number's curve over the dope sheet's time axis, with the
// bezier handles of each segment it eases. A handle is a control point of `Easing::Bezier` drawn in the
// segment's own box (its first key at one corner, its second at the other), so dragging one writes a
// bezier easing on the key that starts the segment — through `easeKeys`, one `set_property_tracks`
// edit. Control points stay inside the box (`easingProblem`: no overshoot), so the curve never leaves
// the values of its two keys.

import { SAME_TIME, easeKeys, type DopeEdit, type DopeRow, type KeyRef } from './dope-sheet';
import { controlPoints, easedPoints } from './easing';
import type { Animatable } from './channels';
import type { Easing, Property, PropertyKey } from './types';

/** The value axis: `lo..=hi` over `height` px, `pad` px inside either end, value up. */
export interface ValueAxis {
	lo: number;
	hi: number;
	height: number;
	pad: number;
}

/** The values a row's curve spans (an eased segment stays between its keys), widened when they are
 *  all one value so the line sits in the middle. */
export function valueRange(keys: readonly PropertyKey[]): [number, number] {
	if (keys.length === 0) return [0, 1];
	const vs = keys.map((k) => k.value);
	const lo = Math.min(...vs);
	const hi = Math.max(...vs);
	if (hi - lo > 1e-9) return [lo, hi];
	const half = Math.max(Math.abs(lo) * 0.1, 0.5);
	return [lo - half, hi + half];
}

export function yOf(va: ValueAxis, v: number): number {
	const span = Math.max(1, va.height - 2 * va.pad);
	return va.pad + (1 - (v - va.lo) / (va.hi - va.lo)) * span;
}

export function valueAt(va: ValueAxis, y: number): number {
	const span = Math.max(1, va.height - 2 * va.pad);
	return va.lo + (1 - (y - va.pad) / span) * (va.hi - va.lo);
}

/** Round steps (1, 2 or 5 times a power of ten) for gridlines in `lo..=hi`, about `want` of them. */
export function valueTicks(lo: number, hi: number, want = 4): number[] {
	const span = hi - lo;
	if (!(span > 0) || !Number.isFinite(span)) return [];
	const raw = span / Math.max(1, want);
	const mag = 10 ** Math.floor(Math.log10(raw));
	const step = [1, 2, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? 10 * mag;
	const out: number[] = [];
	for (let i = Math.ceil(lo / step - 1e-9); i * step <= hi + step * 1e-9; i++) out.push(i === 0 ? 0 : Number((i * step).toPrecision(12)));
	return out;
}

/** A tick's label: as many decimals as its step needs. */
export function tickText(v: number, ticks: readonly number[]): string {
	const step = ticks.length > 1 ? Math.abs(ticks[1] - ticks[0]) : 1;
	const places = Math.max(0, Math.min(4, -Math.floor(Math.log10(step) + 1e-9)));
	return v.toFixed(places);
}

/** One segment of a row: the key that starts it (whose easing shapes it) and the one it reaches. */
export interface Segment {
	prop: Property;
	t0: number;
	v0: number;
	t1: number;
	v1: number;
	easing: Easing;
}

/** Every segment of a row with a length (two keys at one moment are a step, not a segment). */
export function segments(prop: Property, keys: readonly PropertyKey[]): Segment[] {
	const out: Segment[] = [];
	keys.forEach((k, i) => {
		const next = keys[i + 1];
		if (!next || next.time - k.time <= SAME_TIME) return;
		out.push({ prop, t0: k.time, v0: k.value, t1: next.time, v1: next.value, easing: k.easing ?? 'linear' });
	});
	return out;
}

/** The control points a segment's handles stand for: the easing's own, a straight line's thirds (a
 *  bezier with them is the same line), and none for a hold, which has no curve to shape. */
export function handlesOf(e: Easing): [number, number, number, number] | undefined {
	if (e === 'hold') return undefined;
	return controlPoints(e) ?? [1 / 3, 1 / 3, 2 / 3, 2 / 3];
}

export type Handle = 1 | 2;

/** Where handle `which` of `seg` sits, as (time, value). */
export function handlePoint(seg: Segment, which: Handle, e: Easing = seg.easing): [number, number] | undefined {
	const cp = handlesOf(e);
	if (!cp) return undefined;
	const [x, y] = which === 1 ? [cp[0], cp[1]] : [cp[2], cp[3]];
	return [seg.t0 + x * (seg.t1 - seg.t0), seg.v0 + y * (seg.v1 - seg.v0)];
}

const unit = (v: number) => (Number.isFinite(v) ? Math.min(Math.max(v, 0), 1) : 0);

/**
 * The easing handle `which` of `seg` makes when it is put at (`t`, `v`): that point in the segment's
 * box, held inside it. A flat segment (both keys one value) has no height to measure `v` in, so the
 * handle keeps its height there and moves only in time. A hold has no handles and stays as it is.
 */
export function dragHandle(seg: Segment, which: Handle, t: number, v: number): Easing {
	const cp = handlesOf(seg.easing);
	if (!cp) return seg.easing;
	const dv = seg.v1 - seg.v0;
	const x = unit((t - seg.t0) / (seg.t1 - seg.t0));
	const i = which === 1 ? 0 : 2;
	const y = Math.abs(dv) > 1e-12 ? unit((v - seg.v0) / dv) : cp[i + 1];
	const next = [...cp];
	next[i] = x;
	next[i + 1] = y;
	return { bezier: { x1: next[0], y1: next[1], x2: next[2], y2: next[3] } };
}

/** The one edit a handle drag writes: `easing` on the key that starts the segment. */
export function handleEdit(clip: Animatable, seg: Segment, easing: Easing): DopeEdit {
	return easeKeys(clip, [{ prop: seg.prop, time: seg.t0 }], easing);
}

/** The row the graph draws with handles: the one picked by its label while it is still keyed, else the
 *  row of the last selected key, else the first. */
export function activeRow(rows: readonly DopeRow[], selection: readonly KeyRef[], picked: Property | null): Property | null {
	if (picked && rows.some((r) => r.prop === picked)) return picked;
	const last = selection.findLast((s) => rows.some((r) => r.prop === s.prop));
	return last?.prop ?? rows[0]?.prop ?? null;
}

/**
 * A row's curve as an SVG path: the polyline the renderers draw (`easedPoints`), with the value
 * before the first key and after the last held out to `from` / `to` (clip-local seconds).
 */
export function curvePath(keys: readonly PropertyKey[], x: (t: number) => number, y: (v: number) => number, from: number, to: number): string {
	if (keys.length === 0) return '';
	const pts = easedPoints(keys.map((k) => [k.time, k.value, k.easing ?? 'linear']));
	const first = keys[0];
	const last = keys[keys.length - 1];
	if (from < first.time) pts.unshift([from, first.value]);
	if (to > last.time) pts.push([to, last.value]);
	return pts.map(([t, v], i) => `${i === 0 ? 'M' : 'L'}${+x(t).toFixed(2)} ${+y(v).toFixed(2)}`).join(' ');
}
