/* What the curve editor does with a pointer, a key and a typed number — pure, so the canvas component only draws and
 * wires events, and the tests pin the hit-testing and the keys. The points are `grade.ts`'s (`0..1` both ways,
 * `y` up); the editor's box is square, so a distance in the unit square is the same in px on both axes. */

import { MAX_CURVE_POINTS, addPoint, evalCurve, movePoint, nearestPoint } from './grade';
import type { CurvePoint } from './types';

/** How far one arrow press moves the selected point: the hundredth the In and Out fields show, five of them with
 *  Shift (the slider rows' coarse step), a tenth of that with Alt. */
export const POINT_STEP = { fine: 0.001, normal: 0.01, coarse: 0.05 } as const;

export type PointSelect = 'previous' | 'next' | 'first' | 'last';

/** What a key does in the curve editor. */
export type CurveKey =
	| { kind: 'move'; dx: -1 | 0 | 1; dy: -1 | 0 | 1; size: keyof typeof POINT_STEP }
	| { kind: 'select'; to: PointSelect }
	| { kind: 'remove' }
	| { kind: 'add' };

/** The editor's action for a key, or `null` for one that is not its: arrows move the selected point, Page Up and Down
 *  and Home and End pick the point, Delete or Backspace takes it out, Enter or Insert adds one. */
export function curveKey(key: string, mods: { shift?: boolean; alt?: boolean } = {}): CurveKey | null {
	const size = mods.alt ? 'fine' : mods.shift ? 'coarse' : 'normal';
	switch (key) {
		case 'ArrowRight':
			return { kind: 'move', dx: 1, dy: 0, size };
		case 'ArrowLeft':
			return { kind: 'move', dx: -1, dy: 0, size };
		case 'ArrowUp':
			return { kind: 'move', dx: 0, dy: 1, size };
		case 'ArrowDown':
			return { kind: 'move', dx: 0, dy: -1, size };
		case 'PageUp':
			return { kind: 'select', to: 'previous' };
		case 'PageDown':
			return { kind: 'select', to: 'next' };
		case 'Home':
			return { kind: 'select', to: 'first' };
		case 'End':
			return { kind: 'select', to: 'last' };
		case 'Delete':
		case 'Backspace':
			return { kind: 'remove' };
		case 'Enter':
		case 'Insert':
			return { kind: 'add' };
		default:
			return null;
	}
}

/** Float noise off a computed coordinate: 0.30000000000000004 is 0.3. */
const tidy = (v: number) => Math.round(v * 1e4) / 1e4;

/** The points with point `i` moved by a key, held between its neighbours and inside the square. */
export function nudgePoint(points: readonly CurvePoint[], i: number, dx: number, dy: number, size: keyof typeof POINT_STEP): CurvePoint[] {
	const p = points[i];
	if (!p) return points.map((q) => ({ ...q }));
	const step = POINT_STEP[size];
	const moved = movePoint(points, i, tidy(p.x + dx * step), tidy(p.y + dy * step));
	return moved.map((q) => ({ x: tidy(q.x), y: tidy(q.y) }));
}

/** The points with point `i`'s input and / or output set to a typed number, held where a drag would hold it. */
export function setPoint(points: readonly CurvePoint[], i: number, to: { x?: number; y?: number }): CurvePoint[] {
	const p = points[i];
	if (!p) return points.map((q) => ({ ...q }));
	return movePoint(points, i, to.x ?? p.x, to.y ?? p.y);
}

/** The points without point `i`. */
export const removePoint = (points: readonly CurvePoint[], i: number): CurvePoint[] => points.filter((_, j) => j !== i);

/** The index selected after a key picks a point: `current` is the one selected now (`null` for none). Clamped at the ends. */
export function selectPoint(current: number | null, count: number, to: PointSelect): number | null {
	if (count <= 0) return null;
	switch (to) {
		case 'first':
			return 0;
		case 'last':
			return count - 1;
		case 'previous':
			return current === null ? count - 1 : Math.max(0, current - 1);
		case 'next':
			return current === null ? 0 : Math.min(count - 1, current + 1);
	}
}

/** The point to select once point `i` is gone from a curve that had `count` points: the one that took its place, or the last. */
export const afterRemoval = (i: number, count: number): number | null => (count <= 1 ? null : Math.min(i, count - 2));

/** The points with one added half-way from point `i` to the next one (the last point: to the one before it), on the
 *  curve as it runs there, so the shape does not change; where it landed. `null` when there is no room. */
export function addAfter(points: readonly CurvePoint[], i: number): { points: CurvePoint[]; index: number } | null {
	if (points.length < 2 || points.length >= MAX_CURVE_POINTS) return null;
	const a = points[Math.min(Math.max(i, 0), points.length - 1)];
	const b = i >= points.length - 1 ? points[points.length - 2] : points[i + 1];
	const x = tidy((a.x + b.x) / 2);
	return addPoint(points, x, tidy(evalCurve(points, x)));
}

/** How near a pointer must be to a point to take hold of it, in the unit square: 10 px, held to a share of a very small
 *  box and to a share of a large one, so points close together can still be told apart. */
export function hitRadius(sizePx: number): number {
	if (!(sizePx > 0)) return 0.05;
	return Math.min(0.08, Math.max(0.03, 10 / sizePx));
}

/** The point a pointer at `(x, y)` (the unit square) takes hold of, or `-1`. */
export const pickPoint = (points: readonly CurvePoint[], x: number, y: number, sizePx: number): number =>
	nearestPoint(points, x, y, hitRadius(sizePx));

/** A pointer's place in the unit square (y up) over a box `rect`. */
export function unitAt(rect: { left: number; top: number; width: number; height: number }, clientX: number, clientY: number): { x: number; y: number } {
	if (!(rect.width > 0) || !(rect.height > 0)) return { x: 0, y: 0 };
	return { x: (clientX - rect.left) / rect.width, y: 1 - (clientY - rect.top) / rect.height };
}

/** A point of the unit square in px of a `size` square, `y` down, the square drawn `pad` px in from each edge so a point on
 *  the edge is still a whole circle. */
export const pxOf = (p: CurvePoint, size: number, pad = 0): { x: number; y: number } => {
	const span = size - 2 * pad;
	return { x: pad + p.x * span, y: pad + (1 - p.y) * span };
};

/** The box the unit square is drawn in, for a canvas box `rect` with `pad` px of margin: what a pointer is measured against. */
export const plotRect = (
	rect: { left: number; top: number; width: number; height: number },
	pad: number
): { left: number; top: number; width: number; height: number } => ({
	left: rect.left + pad,
	top: rect.top + pad,
	width: rect.width - 2 * pad,
	height: rect.height - 2 * pad
});

/** The canvas's side for a column `available` px wide: it fills the column, held between a floor and `max`. */
export function curveSide(available: number, max: number, min = 96): number {
	if (!(available > 0)) return min;
	return Math.round(Math.min(max, Math.max(min, available)));
}

/** A point in words, for a screen reader and a tooltip. */
export function pointWords(points: readonly CurvePoint[], i: number): string {
	const p = points[i];
	if (!p) return 'No point selected';
	return `Point ${i + 1} of ${points.length}: in ${p.x.toFixed(2)}, out ${p.y.toFixed(2)}`;
}

// ---- a point is named by where it is, not by its place in a list ------------------------
//
// A curve's points are drawn from one list (the cut's, or a gesture's draft of it) and written against another (the cut's, as it
// is when the write's turn comes). An index into the first is not an index into the second: a point a click had added only to the
// draft is not in the cut yet, and removing "point 2" of the cut removed the *next* point, or the whole curve. No two points of
// a curve are closer than `MIN_POINT_GAP` in their input, so the input names a point, and a write that cannot find it writes nothing.

/** How near two inputs are to be the same point's: the model's own dedup distance. */
const SAME_X = 1e-6;

/** The index of the point whose input is `x`, or `-1`. */
export const indexAtX = (points: readonly CurvePoint[], x: number): number => points.findIndex((p) => Math.abs(p.x - x) < SAME_X);

/** The points without the one at input `x`, or `null` when there is none. */
export function removeAtX(points: readonly CurvePoint[], x: number): CurvePoint[] | null {
	const i = indexAtX(points, x);
	return i < 0 ? null : removePoint(points, i);
}

/** The points with the one at input `x` moved to `to` (held between its neighbours), or `null` when there is none. */
export function moveAtX(points: readonly CurvePoint[], x: number, to: CurvePoint): CurvePoint[] | null {
	const i = indexAtX(points, x);
	return i < 0 ? null : movePoint(points, i, to.x, to.y);
}

/** The points with the one at input `x` given a typed input and / or output, or `null` when there is none. */
export function setAtX(points: readonly CurvePoint[], x: number, to: { x?: number; y?: number }): CurvePoint[] | null {
	const i = indexAtX(points, x);
	return i < 0 ? null : setPoint(points, i, to);
}

/** The points with one added at `at`, or `null` when there is no room for it (too near another, or the curve is full). */
export const addAt = (points: readonly CurvePoint[], at: CurvePoint): CurvePoint[] | null => addPoint(points, at.x, at.y)?.points ?? null;
