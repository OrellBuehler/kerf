import { describe, expect, test } from 'bun:test';
import { MAX_CURVE_POINTS, evalCurve } from './grade';
import {
	POINT_STEP,
	addAfter,
	afterRemoval,
	curveKey,
	curveSide,
	hitRadius,
	nudgePoint,
	pickPoint,
	pointWords,
	pxOf,
	removePoint,
	selectPoint,
	setPoint,
	unitAt
} from './curve-edit';
import type { CurvePoint } from './types';

const pts = (p: [number, number][]): CurvePoint[] => p.map(([x, y]) => ({ x, y }));
const three = pts([[0, 0], [0.5, 0.7], [1, 1]]);

describe('the curve editor’s keys', () => {
	test('arrows move the point, Shift coarser and Alt finer', () => {
		expect(curveKey('ArrowRight')).toEqual({ kind: 'move', dx: 1, dy: 0, size: 'normal' });
		expect(curveKey('ArrowDown')).toEqual({ kind: 'move', dx: 0, dy: -1, size: 'normal' });
		expect(curveKey('ArrowUp', { shift: true })).toMatchObject({ size: 'coarse' });
		expect(curveKey('ArrowUp', { alt: true, shift: true })).toMatchObject({ size: 'fine' });
	});

	test('Page and Home / End pick the point; Delete and Backspace take it out; Enter and Insert add one', () => {
		expect(curveKey('PageUp')).toEqual({ kind: 'select', to: 'previous' });
		expect(curveKey('PageDown')).toEqual({ kind: 'select', to: 'next' });
		expect(curveKey('Home')).toEqual({ kind: 'select', to: 'first' });
		expect(curveKey('End')).toEqual({ kind: 'select', to: 'last' });
		expect(curveKey('Delete')).toEqual({ kind: 'remove' });
		expect(curveKey('Backspace')).toEqual({ kind: 'remove' });
		expect(curveKey('Enter')).toEqual({ kind: 'add' });
		expect(curveKey('Insert')).toEqual({ kind: 'add' });
		for (const k of ['a', 'Tab', 'Escape', ' ']) expect(curveKey(k)).toBeNull();
	});
});

describe('a point moved by a key', () => {
	test('a step is a hundredth, with no float noise after a run of them', () => {
		let p = three;
		for (let i = 0; i < 3; i++) p = nudgePoint(p, 1, 1, 0, 'normal');
		expect(p[1]).toEqual({ x: 0.53, y: 0.7 });
		expect(nudgePoint(three, 1, 0, 1, 'coarse')[1]).toEqual({ x: 0.5, y: 0.75 });
		expect(nudgePoint(three, 1, 0, -1, 'fine')[1]).toEqual({ x: 0.5, y: 0.699 });
		expect(POINT_STEP.normal).toBe(0.01);
	});

	test('it stays between its neighbours and inside the square', () => {
		expect(nudgePoint(three, 1, 1, 0, 'coarse').map((p) => p.x)).toEqual([0, 0.55, 1]);
		const edge = pts([[0, 0], [0.995, 0.5], [1, 1]]);
		expect(nudgePoint(edge, 1, 1, 0, 'normal')[1].x).toBe(0.99);
		expect(nudgePoint(three, 2, 0, 1, 'normal')[2].y).toBe(1);
		expect(nudgePoint(three, 0, -1, -1, 'normal')[0]).toEqual({ x: 0, y: 0 });
	});

	test('a key with no point to move changes nothing', () => {
		expect(nudgePoint(three, 5, 1, 0, 'normal')).toEqual(three);
	});

	test('the input is typed in, held between the neighbours, and the output to the square', () => {
		expect(setPoint(three, 1, { x: 0.3 })[1]).toEqual({ x: 0.3, y: 0.7 });
		expect(setPoint(three, 1, { y: 0.2 })[1]).toEqual({ x: 0.5, y: 0.2 });
		expect(setPoint(three, 1, { x: 2 })[1].x).toBe(0.99);
		expect(setPoint(three, 1, { y: -4 })[1].y).toBe(0);
	});
});

describe('picking a point by key', () => {
	test('Page keys step and stop at the ends; none selected starts from the end that was asked for', () => {
		expect(selectPoint(1, 3, 'next')).toBe(2);
		expect(selectPoint(2, 3, 'next')).toBe(2);
		expect(selectPoint(1, 3, 'previous')).toBe(0);
		expect(selectPoint(0, 3, 'previous')).toBe(0);
		expect(selectPoint(null, 3, 'next')).toBe(0);
		expect(selectPoint(null, 3, 'previous')).toBe(2);
		expect(selectPoint(1, 3, 'first')).toBe(0);
		expect(selectPoint(1, 3, 'last')).toBe(2);
		expect(selectPoint(null, 0, 'next')).toBeNull();
	});

	test('after a point is taken out the one that took its place is selected, or none', () => {
		expect(afterRemoval(1, 3)).toBe(1);
		expect(afterRemoval(2, 3)).toBe(1);
		expect(afterRemoval(0, 3)).toBe(0);
		expect(afterRemoval(0, 1)).toBeNull();
		expect(removePoint(three, 1)).toEqual(pts([[0, 0], [1, 1]]));
	});
});

describe('a point added by key', () => {
	test('lands half-way to the next one, on the curve as it runs there, so the shape stays', () => {
		const added = addAfter(three, 0)!;
		expect(added.index).toBe(1);
		expect(added.points.length).toBe(4);
		expect(added.points[1].x).toBe(0.25);
		expect(Math.abs(added.points[1].y - evalCurve(three, 0.25))).toBeLessThan(1e-4);
	});

	test('after the last point it goes between that and the one before', () => {
		const added = addAfter(three, 2)!;
		expect(added.points[added.index].x).toBe(0.75);
	});

	test('on the identity’s two ends it puts a point in the middle', () => {
		const added = addAfter(pts([[0, 0], [1, 1]]), 0)!;
		expect(added.points[1]).toEqual({ x: 0.5, y: 0.5 });
	});

	test('there is no room past the cap or between two points a gap apart', () => {
		const full = Array.from({ length: MAX_CURVE_POINTS }, (_, i) => ({ x: i / (MAX_CURVE_POINTS - 1), y: i / (MAX_CURVE_POINTS - 1) }));
		expect(addAfter(full, 3)).toBeNull();
		expect(addAfter(pts([[0, 0], [0.012, 0.5], [1, 1]]), 0)).toBeNull();
	});
});

describe('the pointer on the canvas', () => {
	const box = { left: 10, top: 20, width: 200, height: 200 };

	test('the box’s bottom left is 0, 0 and its top right is 1, 1', () => {
		expect(unitAt(box, 10, 220)).toEqual({ x: 0, y: 0 });
		expect(unitAt(box, 210, 20)).toEqual({ x: 1, y: 1 });
		expect(unitAt(box, 110, 120)).toEqual({ x: 0.5, y: 0.5 });
		expect(unitAt({ left: 0, top: 0, width: 0, height: 0 }, 5, 5)).toEqual({ x: 0, y: 0 });
	});

	test('a point is drawn where it is hit', () => {
		const p = pxOf({ x: 0.5, y: 0.7 }, 200);
		expect(p.x).toBe(100);
		expect(p.y).toBeCloseTo(60, 9);
		expect(pxOf({ x: 0, y: 0 }, 200)).toEqual({ x: 0, y: 200 });
	});

	test('a point is taken hold of from ten pixels away, whatever the size of the box', () => {
		expect(hitRadius(200)).toBe(0.05);
		expect(hitRadius(100)).toBe(0.08);
		expect(hitRadius(400)).toBe(0.03);
		expect(hitRadius(0)).toBe(0.05);
		// 9 px from the middle point on a 200 px box is in reach; 12 px is not
		expect(pickPoint(three, 0.5 + 9 / 200, 0.7, 200)).toBe(1);
		expect(pickPoint(three, 0.5 + 12 / 200, 0.7, 200)).toBe(-1);
		// on a 280 px box the same 9 px is still in reach, and 12 px is out
		expect(pickPoint(three, 0.5 + 9 / 280, 0.7, 280)).toBe(1);
		expect(pickPoint(three, 0.5 + 12 / 280, 0.7, 280)).toBe(-1);
	});

	test('the nearer of two points in reach is the one taken', () => {
		const close = pts([[0.5, 0.5], [0.53, 0.5]]);
		expect(pickPoint(close, 0.52, 0.5, 200)).toBe(1);
		expect(pickPoint(close, 0.51, 0.5, 200)).toBe(0);
	});

	test('the canvas fills its column between a floor and a ceiling', () => {
		expect(curveSide(298, 280)).toBe(280);
		expect(curveSide(226, 280)).toBe(226);
		expect(curveSide(40, 280)).toBe(96);
		expect(curveSide(0, 280)).toBe(96);
	});
});

describe('a point in words', () => {
	test('names its place and its numbers', () => {
		expect(pointWords(three, 1)).toBe('Point 2 of 3: in 0.50, out 0.70');
		expect(pointWords(three, 7)).toBe('No point selected');
	});
});
