import { describe, expect, test } from 'bun:test';
import {
	CURVE_HUES,
	RAMP_REACH,
	allKeys,
	changes,
	clampShift,
	copyKeys,
	deleteKeys,
	dopeKey,
	dopeRows,
	dragMagnets,
	easeKeys,
	easingName,
	easingPath,
	easingReason,
	easingTargets,
	extendRun,
	fitBoard,
	fitRun,
	fitTickLabels,
	keyIds,
	landsOn,
	nearestKey,
	partitionRamp,
	rampLevels,
	rampLine,
	stopText,
	liveSelection,
	marqueeKeys,
	mergeSelection,
	neighbourKey,
	nudgeOf,
	pasteKeys,
	rampMarks,
	retimeKeys,
	rowLabel,
	runShift,
	sharedEasing,
	sheetStatus,
	snapShift,
	curveHue,
	timeAt,
	toggleKey,
	xOf
} from './dope-sheet';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
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
		expect(rowLabel('pos_x')).toBe('Position X');
		expect(rows.map((r) => r.label)).toEqual(['Scale', 'Position X', 'Position Y', 'Rotation', 'Opacity', 'Volume']);
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
		expect(snapShift(sel, 2, 0.98, { magnets, reach: 0.05, fps: 30, clipStart: 0 })).toBeCloseTo(1, 12);
		// 2 + 0.31 = 2.31: no magnet, frame 69 at 30 fps.
		expect(snapShift(sel, 2, 0.31, { magnets, reach: 0.05, fps: 30, clipStart: 0 })).toBe(69 / 30 - 2);
		// Held at the clip's start.
		expect(snapShift(sel, 2, -9, { magnets, reach: 0.05, fps: 30, clipStart: 0 })).toBe(-2);
	});

	test('the frame is the cut’s, so a clip that starts between two frames keys on one', () => {
		const sel = [{ prop: 'opacity' as const, time: 2 }];
		const clipStart = 1.01;
		const dt = snapShift(sel, 2, 0.31, { magnets: [], reach: 0.05, fps: 30, clipStart });
		const onTimeline = clipStart + 2 + dt;
		expect(Math.abs(onTimeline * 30 - Math.round(onTimeline * 30))).toBeLessThan(1e-9);
		expect(Math.round(onTimeline * 30)).toBe(100);
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
		const edit = easeKeys(c, [...sel], 'hold');
		expect(edit.selected).toEqual([...sel]);
		for (const t of edit.tracks) setPropertyKeys(c, t.prop, t.keys);
		expect(propertyKeys(c, 'opacity').map((x) => x.easing ?? 'linear')).toEqual(['ease_in_out', 'hold', 'linear']);
		expect(sharedEasing(c, targets)).toBe('hold');
		const back = easeKeys(c, [...sel], 'linear');
		expect(back.tracks[0].keys[1]).toEqual(k(2, 0.5));
	});

	test('of a step’s two keys, the one that starts the segment is read and shaped', () => {
		const c = clip();
		setPropertyKeys(c, 'opacity', [k(0, 0, 'hold'), k(2, 0.2, 'hold'), k(2, 0.8), k(4, 1)]);
		const sel = [{ prop: 'opacity' as const, time: 2 }];
		expect(easingTargets(c, sel)).toEqual(sel);
		expect(sharedEasing(c, sel)).toBe('linear');
		const edit = easeKeys(c, sel, 'ease_out');
		expect(edit.tracks[0].keys[2].easing).toBe('ease_out');
	});

	test('a curve is copied, not shared with the clip it was read from', () => {
		const c = clip();
		const curve = { bezier: { x1: 0.1, y1: 0.2, x2: 0.3, y2: 0.4 } };
		setPropertyKeys(c, 'opacity', [k(0, 0, curve), k(2, 1)]);
		const read = propertyKeys(c, 'opacity')[0].easing;
		const moved = retimeKeys(c, [{ prop: 'opacity', time: 2 }], 1).tracks[0].keys[0].easing;
		expect(moved).toEqual(curve);
		expect(moved).not.toBe(read);
		expect(structuredClone(moved)).toEqual(curve);
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

describe('keys while the panel has focus', () => {
	const repeats = (id: string) => !['edit.copy', 'edit.cut', 'edit.paste', 'edit.delete', 'edit.duplicate', 'edit.split', 'edit.rippleDelete'].includes(id);
	test('the keys’ own copy, cut, paste, delete and select all', () => {
		expect(dopeKey('edit.copy', false, repeats)).toBe('copy');
		expect(dopeKey('edit.cut', false, repeats)).toBe('cut');
		expect(dopeKey('edit.paste', false, repeats)).toBe('paste');
		expect(dopeKey('edit.delete', false, repeats)).toBe('delete');
		expect(dopeKey('edit.selectAll', false, repeats)).toBe('selectAll');
	});
	test('an edit that would reach the clip behind the panel does nothing', () => {
		for (const id of ['edit.rippleDelete', 'edit.duplicate', 'edit.split']) expect(dopeKey(id, false, repeats)).toBe('swallow');
	});
	test('a held key repeats only where its action allows it', () => {
		expect(dopeKey('edit.paste', true, repeats)).toBe('swallow');
		expect(dopeKey('edit.selectAll', true, repeats)).toBe('selectAll');
	});
	test('everything else is the editor’s', () => {
		for (const id of [null, 'edit.undo', 'transport.play', 'tool.razor']) expect(dopeKey(id, false, repeats)).toBeNull();
	});
});

describe('moving keys with the keyboard', () => {
	const repeats = () => true;
	test('the step and jump keys are the keys’ while any is selected', () => {
		expect(nudgeOf(dopeKey('playback.stepBack', false, repeats))).toBe('stepBack');
		expect(nudgeOf(dopeKey('playback.stepForward', false, repeats))).toBe('stepForward');
		expect(nudgeOf(dopeKey('playback.jumpBack', false, repeats))).toBe('jumpBack');
		expect(nudgeOf(dopeKey('playback.jumpForward', false, repeats))).toBe('jumpForward');
		expect(nudgeOf(dopeKey('edit.delete', false, repeats))).toBeNull();
		expect(nudgeOf(null)).toBeNull();
	});

	test('a run counts its presses, a frame at the cut’s rate and a jump as a second', () => {
		let run = extendRun(null, 'stepForward');
		run = extendRun(run, 'stepForward');
		run = extendRun(run, 'stepForward');
		run = extendRun(run, 'stepBack');
		expect(run).toEqual({ frames: 2, seconds: 0 });
		expect(runShift(run, 25)).toBeCloseTo(0.08, 12);
		run = extendRun(run, 'jumpForward');
		expect(runShift(run, 25)).toBeCloseTo(1.08, 12);
		expect(runShift(extendRun(null, 'jumpBack'), 30)).toBe(-1);
		// a cut with no usable rate still moves
		expect(runShift({ frames: 3, seconds: 0 }, 0)).toBeCloseTo(0.1, 12);
	});

	test('a run is one retime of the selection, held at the clip’s start', () => {
		const c = clip();
		const sel = [
			{ prop: 'opacity' as const, time: 0 },
			{ prop: 'opacity' as const, time: 2 }
		];
		const edit = retimeKeys(c, sel, runShift({ frames: 3, seconds: 0 }, 30));
		expect(edit.tracks).toHaveLength(1);
		expect(edit.selected.map((r) => r.time)).toEqual([0.1, 2.1]);
		// before 0 is no moment of the clip: both keys stop together
		const back = retimeKeys(c, sel, runShift({ frames: -3, seconds: 0 }, 30));
		expect(back.selected.map((r) => r.time)).toEqual([0, 2]);
		expect(changes(c, back)).toBe(false);
	});

	test('up and down go to the nearest key of the next row that has one', () => {
		const rows = [
			{ prop: 'scale' as const, label: 'Scale', keys: [k(0, 1), k(4, 2)] },
			{ prop: 'pos_x' as const, label: 'Position X', keys: [] },
			{ prop: 'opacity' as const, label: 'Opacity', keys: [k(1, 0), k(3.4, 1), k(9, 0)] }
		];
		expect(neighbourKey(rows, { prop: 'scale', time: 4 }, 1)).toEqual({ prop: 'opacity', time: 3.4 });
		expect(neighbourKey(rows, { prop: 'scale', time: 0 }, 1)).toEqual({ prop: 'opacity', time: 1 });
		expect(neighbourKey(rows, { prop: 'opacity', time: 9 }, -1)).toEqual({ prop: 'scale', time: 4 });
		expect(neighbourKey(rows, { prop: 'scale', time: 0 }, -1)).toBeNull();
		expect(neighbourKey(rows, { prop: 'opacity', time: 1 }, 1)).toBeNull();
		expect(neighbourKey(rows, { prop: 'volume', time: 1 }, 1)).toBeNull();
	});
});

describe('the corner, the easing reason and the ruler', () => {
	test('the corner says what is selected, else how many numbers are listed', () => {
		expect(sheetStatus(6, 0)).toBe('6 numbers');
		expect(sheetStatus(1, 0)).toBe('1 number');
		expect(sheetStatus(6, 3)).toBe('3 selected');
		expect(sheetStatus(0, 0)).toBe('');
	});

	test('the easing button says why there is nothing to shape', () => {
		expect(easingReason(0, 0)).toContain('Select a key');
		expect(easingReason(2, 0)).toContain('last key');
		expect(easingReason(2, 1)).toBeNull();
	});

	test('a label that runs past the lane’s edge goes left of its tick, one that fits nowhere is left out', () => {
		const labels = fitTickLabels(
			[
				{ x: 8, text: '00:00:00' },
				{ x: 88, text: '00:00:05' },
				{ x: 228, text: '00:00:10' }
			],
			240
		);
		expect(labels.map((l) => l.text)).toEqual(['00:00:00', '00:00:05', '00:00:10']);
		expect(labels[0].left).toBe(11);
		expect(labels[2].left).toBeLessThan(228);
		for (const l of labels) {
			expect(l.left).toBeGreaterThanOrEqual(0);
			expect(l.left + l.text.length * 6.2).toBeLessThanOrEqual(240);
		}
		// no room beside the previous label either: only that one goes
		const tight = fitTickLabels(
			[
				{ x: 8, text: '00:00:00' },
				{ x: 40, text: '00:00:01' }
			],
			240
		);
		expect(tight.map((l) => l.text)).toEqual(['00:00:00']);
		expect(fitTickLabels([{ x: 8, text: '00:00:00:00' }], 40)).toEqual([]);
	});
});

describe('the speed ramp as marks', () => {
	const ramped = (keys: { time: number; speed: number; easing?: 'hold' }[]) =>
		({ ...clip(), speed: 1, speed_ramp: keys }) as unknown as Clip;

	test('a ramp lists its keys where they are, and what they say', () => {
		const marks = rampMarks(ramped([{ time: 0, speed: 1 }, { time: 4, speed: 2, easing: 'hold' }, { time: 8, speed: 0.5 }]), (t) => `${t}s`);
		expect(marks.map((m) => m.time)).toEqual([0, 4, 8]);
		expect(marks[1]).toEqual({ time: 4, speed: 2, hold: true, text: '2× at 4s, holds' });
		expect(marks[2].text).toBe('0.5× at 8s');
	});

	test('a clip without a ramp has no marks, and neither has one the engine would ignore', () => {
		expect(rampMarks(clip(), (t) => `${t}`)).toEqual([]);
		expect(rampMarks(ramped([{ time: 4, speed: 2 }, { time: 2, speed: 1 }]), (t) => `${t}`)).toEqual([]);
	});
});

describe('curve hues', () => {
	test('are theme tokens the stylesheet defines, and wrap', () => {
		const tokens = readFileSync(join(import.meta.dir, 'styles/kerf-tokens.css'), 'utf8');
		for (const h of CURVE_HUES) {
			const name = /^var\((--[a-z0-9-]+)\)$/.exec(h)?.[1];
			expect(name, h).toBeDefined();
			expect(tokens.includes(`${name}:`), `${name} is defined`).toBe(true);
		}
		expect(curveHue(0)).toBe(CURVE_HUES[0]);
		expect(curveHue(CURVE_HUES.length)).toBe(CURVE_HUES[0]);
		expect(curveHue(-1)).toBe(CURVE_HUES[CURVE_HUES.length - 1]);
	});
});

describe('a key is named by its time, not its place in the row', () => {
	test('two keys at one time (a step) keep two names, and a name moves with its key', () => {
		expect(keyIds([k(0, 1), k(2, 0.2, 'hold'), k(2, 0.8), k(4, 1)])).toEqual(['0#0', '2#0', '2#1', '4#0']);
		// the key before one that is deleted keeps its name; the one after keeps its own
		expect(keyIds([k(0, 1), k(4, 1)])).toEqual(['0#0', '4#0']);
	});
});

describe('where focus goes when its key is gone', () => {
	const rows = [
		{ prop: 'scale' as const, label: 'Scale', keys: [k(0, 1), k(4, 2)] },
		{ prop: 'opacity' as const, label: 'Opacity', keys: [k(1, 0), k(3.4, 1), k(9, 0)] },
		{ prop: 'volume' as const, label: 'Volume', keys: [k(2, 1)] }
	];
	test('the nearest key of its own row, the earlier on a tie', () => {
		expect(nearestKey(rows, { prop: 'opacity', time: 3.1 })).toEqual({ prop: 'opacity', time: 3.4 });
		expect(nearestKey(rows, { prop: 'opacity', time: 2.1 })).toEqual({ prop: 'opacity', time: 1 });
		expect(nearestKey([{ prop: 'opacity', label: 'Opacity', keys: [k(1, 0), k(3, 1)] }], { prop: 'opacity', time: 2 })).toEqual({ prop: 'opacity', time: 1 });
		expect(nearestKey(rows, { prop: 'scale', time: 2 })).toEqual({ prop: 'scale', time: 0 });
	});
	test('else the nearest row in the sheet’s order, then its nearest key', () => {
		expect(nearestKey(rows, { prop: 'pos_x', time: 3.5 })).toEqual({ prop: 'scale', time: 4 });
		expect(nearestKey([rows[2]], { prop: 'scale', time: 3.5 })).toEqual({ prop: 'volume', time: 2 });
	});
	test('else nothing: the panel itself takes it', () => {
		expect(nearestKey([], { prop: 'scale', time: 1 })).toBeNull();
		expect(nearestKey([{ prop: 'scale', label: 'Scale', keys: [] }], { prop: 'scale', time: 1 })).toBeNull();
	});
});

describe('a run is held inside the clip and lands on nothing', () => {
	const rows = [
		{ prop: 'opacity' as const, label: 'Opacity', keys: [k(0, 0), k(1, 1), k(1.5, 0.5), k(4, 0)] },
		{ prop: 'scale' as const, label: 'Scale', keys: [k(2, 1)] }
	];
	const fps = 30;
	const dur = 5;
	const one = [{ prop: 'opacity' as const, time: 1 }];

	test('a press moves the keys a frame', () => {
		const r = fitRun(rows, one, null, 'stepForward', fps, dur);
		expect(r.stop).toBeNull();
		expect(r.shift).toBeCloseTo(1 / 30, 12);
		expect(r.run).toEqual({ frames: 1, seconds: 0 });
	});

	test('a key goes up to the end of the clip and no further', () => {
		const last = [{ prop: 'opacity' as const, time: 4 }];
		const r = fitRun(rows, last, null, 'jumpForward', fps, dur);
		// a second on lands exactly on the end: that is still the clip
		expect(r.stop).toBeNull();
		expect(r.shift).toBe(1);
		// at the end a press moves nothing, and the run is as it was
		const again = fitRun(rows, last, r.run, 'stepForward', fps, dur);
		expect(again.stop).toBe('end');
		expect(again.run).toEqual(r.run);
		expect(again.shift).toBe(1);
		// a long jump from near the end lands on the end, not past it
		const near = fitRun(rows, last, { frames: 0, seconds: 0.5 }, 'jumpForward', fps, dur);
		expect(near.stop).toBe('end');
		expect(near.shift).toBe(1);
		expect(near.run).toEqual({ frames: 0, seconds: 1 });
		// and a press back is a frame from the end, not from where the presses would have gone
		const back = fitRun(rows, last, near.run, 'stepBack', fps, dur);
		expect(back.shift).toBeCloseTo(1 - 1 / 30, 12);
		expect(back.stop).toBeNull();
	});

	test('a key goes up to the start of the clip and no further', () => {
		const first = [{ prop: 'scale' as const, time: 2 }];
		const r = fitRun(rows, first, null, 'jumpBack', fps, dur);
		expect(r.shift).toBe(-1);
		expect(r.stop).toBeNull();
		const two = fitRun(rows, first, r.run, 'jumpBack', fps, dur);
		expect(two.shift).toBe(-2);
		expect(two.stop).toBeNull();
		const three = fitRun(rows, first, two.run, 'stepBack', fps, dur);
		expect(three.stop).toBe('start');
		expect(three.shift).toBe(-2);
		expect(three.run).toEqual(two.run);
	});

	test('a selection stops when its last key does', () => {
		const sel = [
			{ prop: 'opacity' as const, time: 1.5 },
			{ prop: 'opacity' as const, time: 4 }
		];
		const r = fitRun(rows, sel, null, 'jumpForward', fps, dur);
		expect(r.shift).toBe(1);
		expect(r.stop).toBeNull();
		expect(fitRun(rows, sel, r.run, 'stepForward', fps, dur).stop).toBe('end');
		const r2 = fitRun(rows, sel, null, 'jumpBack', fps, dur);
		expect(r2.shift).toBe(-1);
		expect(r2.stop).toBeNull();
	});

	test('a key already past the end may come back and not go on', () => {
		const out = [{ rows: [{ prop: 'opacity' as const, label: 'Opacity', keys: [k(6, 0)] }], sel: [{ prop: 'opacity' as const, time: 6 }] }][0];
		expect(fitRun(out.rows, out.sel, null, 'stepForward', fps, dur).stop).toBe('end');
		const back = fitRun(out.rows, out.sel, null, 'stepBack', fps, dur);
		expect(back.stop).toBeNull();
		expect(back.shift).toBeCloseTo(-1 / 30, 12);
	});

	test('a press that would land on a key that is not moving is refused, one step short of it', () => {
		// the key at 1 is a second from the one at 2 (scale does not matter: another row); on its own row 1.5 is half a second off
		const near = [
			{ prop: 'opacity' as const, label: 'Opacity', keys: [k(1, 1), k(1 + 2 / 30, 0.5)] }
		];
		const sel = [{ prop: 'opacity' as const, time: 1 }];
		const one = fitRun(near, sel, null, 'stepForward', fps, dur);
		expect(one.stop).toBeNull();
		const two = fitRun(near, sel, one.run, 'stepForward', fps, dur);
		expect(two.stop).toBe('key');
		expect(two.run).toEqual(one.run);
		expect(two.shift).toBeCloseTo(1 / 30, 12);
		expect(landsOn(near, sel, 2 / 30)).toBe(true);
		expect(landsOn(near, sel, 1 / 30)).toBe(false);
	});

	test('the keys of the selection do not block each other', () => {
		const both = [
			{ prop: 'opacity' as const, time: 1 },
			{ prop: 'opacity' as const, time: 1.5 }
		];
		expect(landsOn(rows, both, 0.5)).toBe(false);
		expect(landsOn(rows, one, 0.5)).toBe(true);
		expect(landsOn(rows, [{ prop: 'scale', time: 2 }], 0.5)).toBe(false);
	});

	test('what stopped it is said', () => {
		expect(stopText('start')).toContain('start');
		expect(stopText('end')).toContain('end');
		expect(stopText('key')).toContain('key');
		expect(stopText(null)).toBe('');
	});
});

describe('a clipboard is cut to what the clip has', () => {
	const board = {
		keys: [
			{ prop: 'pos_x' as const, offset: 0, value: 0.1 },
			{ prop: 'pos_x' as const, offset: 1, value: 0.2, easing: 'hold' as const },
			{ prop: 'volume' as const, offset: 0.5, value: 0.8 },
			{ prop: 'scale' as const, offset: 2, value: 1.2 }
		]
	};
	test('keeps what fits, says what it left, and starts the rest at its earliest key', () => {
		const sound = fitBoard(board, ['volume']);
		expect(sound.dropped).toEqual(['scale', 'pos_x']);
		expect(sound.board).toEqual({ keys: [{ prop: 'volume', offset: 0, value: 0.8 }] });
		const pic = fitBoard(board, ['pos_x', 'scale']);
		expect(pic.dropped).toEqual(['volume']);
		expect(pic.board?.keys.map((x) => x.offset)).toEqual([0, 1, 2]);
	});
	test('is nothing when nothing fits', () => {
		expect(fitBoard(board, ['rotation']).board).toBeNull();
		expect(fitBoard(board, []).dropped).toEqual(['scale', 'pos_x', 'volume']);
	});
	test('a paste puts nothing of a number the clip has no use for', () => {
		const c = clip();
		const edit = pasteKeys(c, board, 1, ['volume']);
		expect(edit.tracks.map((t) => t.prop)).toEqual(['volume']);
		expect(edit.tracks[0].keys.find((x) => x.time === 1)?.value).toBe(0.8);
		expect(pasteKeys(c, board, 1, ['rotation']).tracks).toEqual([]);
		// without a filter, as before
		expect(pasteKeys(c, board, 1).tracks.map((t) => t.prop)).toEqual(['scale', 'pos_x', 'volume']);
	});
});

describe('a key is named by its easing', () => {
	test('nothing for linear, the preset’s name, holds, a curve of its own', () => {
		expect(easingName(undefined)).toBe('');
		expect(easingName('linear')).toBe('');
		expect(easingName('ease_out')).toBe('ease out');
		expect(easingName('hold')).toBe('holds');
		expect(easingName({ bezier: { x1: 0.2, y1: 0.9, x2: 0.3, y2: 1 } })).toBe('snappy');
		expect(easingName({ bezier: { x1: 0.11, y1: 0.2, x2: 0.3, y2: 0.4 } })).toBe('custom curve');
	});
});

describe('the speed ramp’s row', () => {
	test('a key far past the clip is counted, not drawn, so it does not shrink the lane', () => {
		const marks = [{ time: 0 }, { time: 4 }, { time: 12 }, { time: 40 }, { time: 1e6 }];
		const { inLane, beyond } = partitionRamp(marks, 10);
		expect(inLane.map((m) => m.time)).toEqual([0, 4, 12]);
		expect(beyond.map((m) => m.time)).toEqual([40, 1e6]);
		expect(partitionRamp(marks, 10).inLane.every((m) => m.time <= 10 * RAMP_REACH)).toBe(true);
		expect(partitionRamp([], 10)).toEqual({ inLane: [], beyond: [] });
	});

	test('a slowdown falls and a speed-up rises, 1× between', () => {
		const slow = rampLevels([1, 0.5, 1], 36);
		expect(slow.y[1]).toBeGreaterThan(slow.y[0]);
		expect(slow.baseline).toBe(slow.y[0]);
		const fast = rampLevels([1, 2, 1], 36);
		expect(fast.y[1]).toBeLessThan(fast.y[0]);
		// a ramp that both speeds up and slows down puts 1× between its extremes
		const both = rampLevels([0.5, 1, 2], 36);
		expect(both.y[2]).toBeLessThan(both.y[1]);
		expect(both.y[1]).toBeLessThan(both.y[0]);
		expect(both.baseline).toBe(both.y[1]);
		// every mark is inside the row
		for (const y of [...slow.y, ...fast.y, ...both.y]) {
			expect(y).toBeGreaterThanOrEqual(4);
			expect(y).toBeLessThanOrEqual(32);
		}
	});

	test('a ramp that stays at 1× is a line through the middle', () => {
		expect(rampLevels([1, 1], 36)).toEqual({ y: [18, 18], baseline: null });
		expect(rampLevels([], 36)).toEqual({ y: [], baseline: null });
	});

	test('draws as a line that steps where a hold ends', () => {
		const marks = [
			{ time: 0, speed: 1, hold: false, text: '' },
			{ time: 2, speed: 2, hold: true, text: '' },
			{ time: 4, speed: 0.5, hold: false, text: '' }
		];
		const d = rampLine(marks, 6, (t) => t * 10, 36);
		expect(d.startsWith('M0.00')).toBe(true);
		// a hold keeps its level to the next key, then steps (a vertical segment at x = 40)
		const pts = d.split(/(?=[ML])/).map((p) => p.slice(1).split(' ').map(Number));
		expect(pts.some((p, i) => i > 0 && p[0] === pts[i - 1][0] && p[1] !== pts[i - 1][1])).toBe(true);
		expect(rampLine([], 6, (t) => t, 36)).toBe('');
	});
});
