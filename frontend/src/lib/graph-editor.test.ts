import { describe, expect, test } from 'bun:test';
import {
	activeProp,
	curvePath,
	dragHandle,
	handleEdit,
	handlePoint,
	handlesOf,
	isFlat,
	nudgeHandle,
	sameEasing,
	segments,
	tickText,
	valueAt,
	valueRange,
	valueTicks,
	yOf
} from './graph-editor';
import { changes, dopeRows } from './dope-sheet';
import { propertyKeys } from './channels';
import { curve, easingProblem } from './easing';
import type { Clip, PropertyKey } from './types';

const k = (time: number, value: number, easing?: PropertyKey['easing']): PropertyKey => ({ time, value, ...(easing ? { easing } : {}) });

function clip(): Clip {
	return {
		id: 'c',
		asset_id: 'a',
		source_in: 0,
		source_out: 10,
		timeline_start: 5,
		volume: 1,
		channels: [
			{ prop: 'opacity', keys: [k(0, 1, 'ease_in_out'), k(2, 0.5), k(4, 0, 'hold'), k(6, 1)] },
			{ prop: 'volume', keys: [k(1, 1), k(3, 1)] }
		]
	} as unknown as Clip;
}

describe('the value axis', () => {
	test('spans the keys, and a flat row sits in the middle of a range around it', () => {
		expect(valueRange(propertyKeys(clip(), 'opacity'))).toEqual([0, 1]);
		expect(valueRange(propertyKeys(clip(), 'volume'))).toEqual([0.5, 1.5]);
		expect(valueRange([k(0, 20), k(1, 20)])).toEqual([18, 22]);
		expect(valueRange([])).toEqual([0, 1]);
		expect(valueRange([k(0, Number.NaN), k(1, 2), k(2, Number.POSITIVE_INFINITY), k(3, 3)])).toEqual([2, 3]);
		expect(valueRange([k(0, Number.NaN)])).toEqual([0, 1]);
	});

	test('maps a value to a height and back, value up', () => {
		const va = { lo: 0, hi: 2, height: 120, pad: 10 };
		expect(yOf(va, 2)).toBe(10);
		expect(yOf(va, 0)).toBe(110);
		expect(valueAt(va, yOf(va, 0.7))).toBeCloseTo(0.7, 12);
	});

	test('gridlines fall on round steps with as many decimals as the step needs', () => {
		expect(valueTicks(0, 1, 4)).toEqual({ step: 0.5, values: [0, 0.5, 1] });
		const t = valueTicks(0, 1, 5);
		expect(t.values).toEqual([0, 0.2, 0.4, 0.6, 0.8, 1]);
		expect(tickText(t.values[3], t.step)).toBe('0.6');
		expect(valueTicks(-30, 90, 6).values).toEqual([-20, 0, 20, 40, 60, 80]);
		expect(tickText(40, 20)).toBe('40');
		expect(valueTicks(1, 1)).toEqual({ step: 0, values: [] });
		// One tick in sight still reads with its step's decimals, not as a whole number.
		const one = valueTicks(0.58, 0.63, 1);
		expect(one.values).toEqual([0.6]);
		expect(tickText(one.values[0], one.step)).toBe('0.6');
		expect(tickText(0.125, 0.005)).toBe('0.125');
	});
});

describe('segments and handles', () => {
	test('a segment per key that reaches a later one; a step is none', () => {
		const segs = segments('opacity', [k(0, 1, 'ease_in'), k(1, 0.5), k(1, 0.2), k(3, 0)]);
		expect(segs.map((s) => [s.t0, s.t1, s.easing])).toEqual([
			[0, 1, 'ease_in'],
			[1, 3, 'linear']
		]);
	});

	test('handles are the easing’s control points in the segment’s box; a line’s are its thirds; a hold has none', () => {
		const [eased, line, held] = segments('opacity', propertyKeys(clip(), 'opacity'));
		expect(handlePoint(eased, 1)).toEqual([0.84, 1]);
		expect(handlePoint(eased, 2)).toEqual([1.16, 0.5]);
		expect(handlesOf('linear')).toEqual([1 / 3, 1 / 3, 2 / 3, 2 / 3]);
		expect(handlePoint(line, 1)).toEqual([2 + 2 / 3, 0.5 - 0.5 / 3]);
		expect(handlePoint(held, 1)).toBeUndefined();
		expect(dragHandle(held, 1, 5, 0.5)).toBe('hold');
	});

	test('dragging a handle writes a bezier inside the box, whichever way the segment goes', () => {
		const [eased] = segments('opacity', propertyKeys(clip(), 'opacity'));
		// The segment falls from 1 to 0.5: a handle at 0.6 is 0.8 of the way down.
		expect(dragHandle(eased, 1, 0.5, 0.6)).toEqual({ bezier: { x1: 0.25, y1: 0.8, x2: 0.58, y2: 1 } });
		// Past the box: held at its edges, so the curve never overshoots its keys.
		const out = dragHandle(eased, 2, 9, -3);
		expect(out).toEqual({ bezier: { x1: 0.42, y1: 0, x2: 1, y2: 1 } });
		expect(easingProblem(out)).toBeUndefined();
		const back = dragHandle(eased, 2, -1, 7);
		expect(back).toEqual({ bezier: { x1: 0.42, y1: 0, x2: 0, y2: 0 } });
	});

	test('a flat segment has nothing to shape (and a handle on it would move in time only)', () => {
		const [flat] = segments('volume', propertyKeys(clip(), 'volume'));
		expect(isFlat(flat)).toBe(true);
		expect(isFlat(segments('opacity', propertyKeys(clip(), 'opacity'))[0])).toBe(false);
		expect(dragHandle(flat, 1, 1.5, 9)).toEqual({ bezier: { x1: 0.25, y1: 1 / 3, x2: 2 / 3, y2: 2 / 3 } });
	});

	test('an arrow nudges a handle a twentieth of its box, up on the screen whichever way the segment goes', () => {
		const [eased] = segments('opacity', propertyKeys(clip(), 'opacity'));
		// Falling from 1 to 0.5: up raises the value, so the handle's share of the drop shrinks.
		const up = nudgeHandle(eased, 2, 0, 1) as { bezier: Record<string, number> };
		expect(up.bezier.x2).toBeCloseTo(0.58, 12);
		expect(up.bezier.y2).toBeCloseTo(0.95, 12);
		const right = nudgeHandle(eased, 1, 1, 0) as { bezier: Record<string, number> };
		expect(right.bezier.x1).toBeCloseTo(0.47, 12);
		// Against its box it goes nowhere: the same curve as before.
		const ease_in = segments('opacity', [k(0, 0, 'ease_in'), k(1, 1)])[0];
		expect(sameEasing(nudgeHandle(ease_in, 2, 1, 0), 'ease_in')).toBe(true);
		expect(nudgeHandle(segments('opacity', [k(0, 0, 'hold'), k(1, 1)])[0], 1, 1, 0)).toBe('hold');
	});

	test('easings are the same when their control points are', () => {
		expect(sameEasing('ease_in', { bezier: { x1: 0.42, y1: 0, x2: 1, y2: 1 } })).toBe(true);
		expect(sameEasing('linear', { bezier: { x1: 1 / 3, y1: 1 / 3, x2: 2 / 3, y2: 2 / 3 } })).toBe(true);
		expect(sameEasing('hold', 'hold')).toBe(true);
		expect(sameEasing('hold', 'linear')).toBe(false);
		expect(sameEasing('ease_in', 'ease_out')).toBe(false);
	});

	test('a line given a handle that stays on it is still the same line', () => {
		const [, line] = segments('opacity', propertyKeys(clip(), 'opacity'));
		const e = dragHandle(line, 1, 2.5, 0.375);
		for (const u of [0.1, 0.5, 0.9]) expect(curve(e, u)).toBeCloseTo(u, 9);
	});
});

describe('the edit', () => {
	test('a handle drag is one edit of the row it shaped, on the key that starts the segment', () => {
		const c = clip();
		const [eased] = segments('opacity', propertyKeys(c, 'opacity'));
		const e = dragHandle(eased, 1, 0.5, 0.6);
		const keep = [
			{ prop: 'opacity' as const, time: 2 },
			{ prop: 'volume' as const, time: 1 }
		];
		const edit = handleEdit(c, eased, e, keep)!;
		expect(edit.tracks).toEqual([{ prop: 'opacity', keys: [k(0, 1, e), k(2, 0.5), k(4, 0, 'hold'), k(6, 1)] }]);
		expect(edit.selected).toEqual(keep);
		expect(changes(c, edit)).toBe(true);
	});

	test('a handle that ends where the curve already is writes nothing, nor does one whose key is gone', () => {
		const c = clip();
		const [eased] = segments('opacity', propertyKeys(c, 'opacity'));
		expect(handleEdit(c, eased, 'ease_in_out')).toBeNull();
		expect(handleEdit(c, eased, { bezier: { x1: 0.42, y1: 0, x2: 0.58, y2: 1 } })).toBeNull();
		const line = segments('opacity', propertyKeys(c, 'opacity'))[1];
		expect(handleEdit(c, line, { bezier: { x1: 1 / 3, y1: 1 / 3, x2: 2 / 3, y2: 2 / 3 } })).toBeNull();
		expect(handleEdit(c, { ...eased, t0: 0.5 }, 'ease_in')).toBeNull();
	});
});

describe('the curve', () => {
	test('the renderers’ polyline, held out before the first key and after the last', () => {
		const id = (v: number) => v;
		expect(curvePath([k(1, 0), k(2, 1, 'hold'), k(3, 0)], id, id, 0, 4)).toBe('M0 0 L1 0 L2 1 L3 1 L3 0 L4 0');
		expect(curvePath([], id, id, 0, 1)).toBe('');
		const eased = curvePath([k(0, 0, 'ease_in'), k(1, 1)], (t) => t * 100, (v) => v * 100, 0, 1);
		expect(eased.split(' L')).toHaveLength(13);
	});

	test('the active row: picked by its label, else the last selected key’s, else the first', () => {
		const rows = dopeRows(clip());
		expect(activeProp(rows, [], null)).toBe('opacity');
		expect(activeProp(rows, [{ prop: 'opacity', time: 0 }, { prop: 'volume', time: 1 }], null)).toBe('volume');
		expect(activeProp(rows, [{ prop: 'volume', time: 1 }], 'opacity')).toBe('opacity');
		expect(activeProp(rows, [], 'scale')).toBe('opacity');
		expect(activeProp([], [], null)).toBeNull();
	});
});
