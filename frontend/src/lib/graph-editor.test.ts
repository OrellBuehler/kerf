import { describe, expect, test } from 'bun:test';
import {
	activeRow,
	curvePath,
	dragHandle,
	handleEdit,
	handlePoint,
	handlesOf,
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
	});

	test('maps a value to a height and back, value up', () => {
		const va = { lo: 0, hi: 2, height: 120, pad: 10 };
		expect(yOf(va, 2)).toBe(10);
		expect(yOf(va, 0)).toBe(110);
		expect(valueAt(va, yOf(va, 0.7))).toBeCloseTo(0.7, 12);
	});

	test('gridlines fall on round steps with as many decimals as the step needs', () => {
		expect(valueTicks(0, 1, 4)).toEqual([0, 0.5, 1]);
		const t = valueTicks(0, 1, 5);
		expect(t).toEqual([0, 0.2, 0.4, 0.6, 0.8, 1]);
		expect(tickText(t[3], t)).toBe('0.6');
		expect(valueTicks(-30, 90, 6)).toEqual([-20, 0, 20, 40, 60, 80]);
		expect(tickText(40, [0, 20])).toBe('40');
		expect(valueTicks(1, 1)).toEqual([]);
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

	test('a flat segment’s handles move in time only', () => {
		const [flat] = segments('volume', propertyKeys(clip(), 'volume'));
		expect(dragHandle(flat, 1, 1.5, 9)).toEqual({ bezier: { x1: 0.25, y1: 1 / 3, x2: 2 / 3, y2: 2 / 3 } });
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
		const edit = handleEdit(c, eased, e);
		expect(edit.tracks).toEqual([{ prop: 'opacity', keys: [k(0, 1, e), k(2, 0.5), k(4, 0, 'hold'), k(6, 1)] }]);
		expect(edit.selected).toEqual([{ prop: 'opacity', time: 0 }]);
		expect(changes(c, edit)).toBe(true);
		expect(changes(c, handleEdit(c, eased, 'ease_in_out'))).toBe(false);
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
		expect(activeRow(rows, [], null)).toBe('opacity');
		expect(activeRow(rows, [{ prop: 'opacity', time: 0 }, { prop: 'volume', time: 1 }], null)).toBe('volume');
		expect(activeRow(rows, [{ prop: 'volume', time: 1 }], 'opacity')).toBe('opacity');
		expect(activeRow(rows, [], 'scale')).toBe('opacity');
		expect(activeRow([], [], null)).toBeNull();
	});
});
