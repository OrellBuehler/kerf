import { describe, expect, test } from 'bun:test';
import {
	allKeys,
	changes,
	clampShift,
	copyKeys,
	deleteKeys,
	dopeRows,
	dragMagnets,
	easeKeys,
	easingPath,
	easingTargets,
	liveSelection,
	marqueeKeys,
	mergeSelection,
	pasteKeys,
	retimeKeys,
	rowLabel,
	sharedEasing,
	snapShift,
	timeAt,
	toggleKey,
	xOf
} from './dope-sheet';
import { setPropertyKeys, propertyKeys } from './channels';
import { EASING_CHOICES } from './easing';
import type { Clip, Keyframe, PropertyKey } from './types';

const k = (time: number, value: number, easing?: PropertyKey['easing']): PropertyKey => ({ time, value, ...(easing ? { easing } : {}) });
const kf = (time: number, scale: number, opacity: number): Keyframe => ({ time, scale, pos_x: 0, pos_y: 0, rotation: 0, opacity });

function clip(): Clip {
	return {
		id: 'c',
		asset_id: 'a',
		source_in: 0,
		source_out: 10,
		timeline_start: 5,
		volume: 1,
		channels: [
			{ prop: 'opacity', keys: [k(0, 1, 'ease_in_out'), k(2, 0.5), k(4, 0)] },
			{ prop: 'volume', keys: [k(1, 1), k(3, 0.25, 'hold'), k(6, 1)] }
		]
	} as unknown as Clip;
}

describe('rows', () => {
	test('one row per keyed number, in the Inspector’s order, with its keys', () => {
		const c = clip();
		c.keyframes = [kf(0, 1, 1), kf(4, 2, 1)];
		const rows = dopeRows(c);
		expect(rows.map((r) => r.prop)).toEqual(['scale', 'pos_x', 'pos_y', 'rotation', 'opacity', 'volume']);
		expect(rows[0].keys.map((x) => x.value)).toEqual([1, 2]);
		expect(rows[4].keys.map((x) => x.time)).toEqual([0, 2, 4]);
		expect(rowLabel('pos_x')).toBe('Position x');
	});

	test('a selection keeps only the keys that are still there', () => {
		const rows = dopeRows(clip());
		expect(
			liveSelection(rows, [
				{ prop: 'opacity', time: 2 },
				{ prop: 'opacity', time: 3 },
				{ prop: 'scale', time: 0 }
			])
		).toEqual([{ prop: 'opacity', time: 2 }]);
		expect(allKeys(rows)).toHaveLength(6);
	});

	test('toggle and marquee', () => {
		const rows = dopeRows(clip());
		expect(toggleKey([{ prop: 'opacity', time: 2 }], { prop: 'opacity', time: 2 + 1e-9 })).toEqual([]);
		expect(toggleKey([], { prop: 'volume', time: 1 })).toEqual([{ prop: 'volume', time: 1 }]);
		// Both rows, 1.5..3.5 s: opacity's 2 and volume's 3.
		expect(marqueeKeys(rows, 1, 0, 3.5, 1.5)).toEqual([
			{ prop: 'opacity', time: 2 },
			{ prop: 'volume', time: 3 }
		]);
		expect(marqueeKeys(rows, 1, 1, 0, 10).map((r) => r.time)).toEqual([1, 3, 6]);
		expect(mergeSelection([{ prop: 'volume', time: 1 }], [{ prop: 'volume', time: 1 }, { prop: 'volume', time: 3 }])).toHaveLength(2);
	});
});

describe('retime', () => {
	test('moves the selected keys across rows, each keeping its value and easing, in one set of tracks', () => {
		const c = clip();
		const sel = [
			{ prop: 'opacity', time: 0 },
			{ prop: 'volume', time: 3 }
		] as const;
		const edit = retimeKeys(c, [...sel], 0.5);
		expect(edit.tracks.map((t) => t.prop)).toEqual(['opacity', 'volume']);
		expect(edit.tracks[0].keys).toEqual([k(0.5, 1, 'ease_in_out'), k(2, 0.5), k(4, 0)]);
		expect(edit.tracks[1].keys).toEqual([k(1, 1), k(3.5, 0.25, 'hold'), k(6, 1)]);
		expect(edit.selected).toEqual([
			{ prop: 'opacity', time: 0.5 },
			{ prop: 'volume', time: 3.5 }
		]);
		expect(changes(c, edit)).toBe(true);
	});

	test('a key it lands on gives way, and nothing goes before the clip’s start', () => {
		const c = clip();
		const onto = retimeKeys(c, [{ prop: 'opacity', time: 4 }], -2);
		expect(onto.tracks[0].keys).toEqual([k(0, 1, 'ease_in_out'), k(2, 0)]);
		const early = retimeKeys(c, [{ prop: 'volume', time: 1 }, { prop: 'volume', time: 3 }], -5);
		expect(early.tracks[0].keys.map((x) => x.time)).toEqual([0, 2, 6]);
		expect(clampShift([{ prop: 'volume', time: 1 }], -5)).toBe(-1);
	});

	test('Alt-duplicate leaves the originals and selects the copies', () => {
		const c = clip();
		const edit = retimeKeys(c, [{ prop: 'opacity', time: 2 }], 1, true);
		expect(edit.tracks[0].keys.map((x) => [x.time, x.value])).toEqual([
			[0, 1],
			[2, 0.5],
			[3, 0.5],
			[4, 0]
		]);
		expect(edit.selected).toEqual([{ prop: 'opacity', time: 3 }]);
	});

	test('a drag that goes nowhere changes nothing', () => {
		const c = clip();
		expect(changes(c, retimeKeys(c, [{ prop: 'opacity', time: 2 }], 0))).toBe(false);
		expect(changes(c, retimeKeys(c, [{ prop: 'opacity', time: 2 }], 0, true))).toBe(false);
	});

	test('the grabbed key lands on a magnet in reach, else on a frame', () => {
		const rows = dopeRows(clip());
		const sel = [{ prop: 'opacity' as const, time: 2 }];
		const magnets = dragMagnets(rows, sel, 10, 2.7);
		expect(magnets).toContain(2.7);
		expect(magnets).not.toContain(2);
		// 2 + 0.98 = 2.98 is within 0.05 of volume's 3.
		expect(snapShift(sel, 2, 0.98, { magnets, reach: 0.05, fps: 30 })).toBeCloseTo(1, 12);
		// 2 + 0.31 = 2.31: no magnet, frame 69 at 30 fps.
		expect(snapShift(sel, 2, 0.31, { magnets, reach: 0.05, fps: 30 })).toBe(69 / 30 - 2);
		// Held at the clip's start.
		expect(snapShift(sel, 2, -9, { magnets, reach: 0.05, fps: 30 })).toBe(-2);
	});
});

describe('delete, copy and paste', () => {
	test('delete takes the keys out; a row left empty is static', () => {
		const c = clip();
		const edit = deleteKeys(c, [
			{ prop: 'opacity', time: 0 },
			{ prop: 'opacity', time: 2 },
			{ prop: 'opacity', time: 4 },
			{ prop: 'volume', time: 6 }
		]);
		expect(edit.tracks).toEqual([
			{ prop: 'opacity', keys: [] },
			{ prop: 'volume', keys: [k(1, 1), k(3, 0.25, 'hold')] }
		]);
		setPropertyKeys(c, 'opacity', edit.tracks[0].keys);
		expect(dopeRows(c).map((r) => r.prop)).toEqual(['volume']);
	});

	test('a paste puts the earliest copied key at the playhead, keeps the spacing and easing, and replaces what is there', () => {
		const c = clip();
		const board = copyKeys(c, [
			{ prop: 'opacity', time: 2 },
			{ prop: 'volume', time: 3 }
		])!;
		expect(board.keys).toEqual([
			{ prop: 'opacity', offset: 0, value: 0.5 },
			{ prop: 'volume', offset: 1, value: 0.25, easing: 'hold' }
		]);
		const edit = pasteKeys(c, board, 5);
		expect(edit.tracks[0].keys.map((x) => x.time)).toEqual([0, 2, 4, 5]);
		expect(edit.tracks[1].keys).toEqual([k(1, 1), k(3, 0.25, 'hold'), k(6, 0.25, 'hold')]);
		expect(edit.selected).toEqual([
			{ prop: 'opacity', time: 5 },
			{ prop: 'volume', time: 6 }
		]);
		expect(copyKeys(c, [])).toBeUndefined();
	});

	test('a paste onto a clip without that number gives it a row', () => {
		const from = clip();
		const to = { ...clip(), id: 'd', channels: undefined } as Clip;
		const board = copyKeys(from, [{ prop: 'opacity', time: 0 }, { prop: 'opacity', time: 4 }])!;
		const edit = pasteKeys(to, board, -3);
		expect(edit.tracks).toEqual([{ prop: 'opacity', keys: [k(0, 1, 'ease_in_out'), k(4, 0)] }]);
	});
});

describe('easing', () => {
	test('only a key that starts a segment is shaped, and the shared easing is read back', () => {
		const c = clip();
		const sel = [
			{ prop: 'opacity', time: 2 },
			{ prop: 'opacity', time: 4 },
			{ prop: 'volume', time: 1 }
		] as const;
		const targets = easingTargets(c, [...sel]);
		expect(targets).toEqual([
			{ prop: 'opacity', time: 2 },
			{ prop: 'volume', time: 1 }
		]);
		expect(sharedEasing(c, targets)).toBe('linear');
		expect(sharedEasing(c, [{ prop: 'opacity', time: 0 }, { prop: 'volume', time: 3 }])).toBeUndefined();
		const edit = easeKeys(c, targets, 'hold');
		for (const t of edit.tracks) setPropertyKeys(c, t.prop, t.keys);
		expect(propertyKeys(c, 'opacity').map((x) => x.easing ?? 'linear')).toEqual(['ease_in_out', 'hold', 'linear']);
		expect(sharedEasing(c, targets)).toBe('hold');
		const back = easeKeys(c, targets, 'linear');
		expect(back.tracks[0].keys[1]).toEqual(k(2, 0.5));
	});

	test('twelve presets, each drawn as the curve it plays', () => {
		expect(EASING_CHOICES).toHaveLength(12);
		expect(new Set(EASING_CHOICES.map((c) => c.id)).size).toBe(12);
		expect(easingPath('linear', 40, 26, 3)).toBe('M3 23 L37 3');
		expect(easingPath('hold', 40, 26, 3)).toBe('M3 23 L37 23 L37 3');
		expect(easingPath('ease_in_out', 40, 26, 3).split('L')).toHaveLength(13);
	});
});

describe('axis', () => {
	test('time and x are inverse, with the pad at both ends', () => {
		const axis = { duration: 4, width: 216, pad: 8 };
		expect(xOf(axis, 0)).toBe(8);
		expect(xOf(axis, 4)).toBe(208);
		expect(timeAt(axis, xOf(axis, 1.25))).toBeCloseTo(1.25, 12);
	});
});
