import { describe, expect, test } from 'bun:test';
import {
	DEGREES,
	NUMBER_COL,
	Pending,
	PERCENT,
	SECONDS,
	TIMES,
	editable,
	entryFor,
	fillOrigin,
	fromPos,
	isBipolar,
	neutralOf,
	parseEntry,
	readout,
	resetTo,
	snap,
	stepBy,
	tickPos,
	toPos,
	type RowSpec
} from './slider-row';

// The Inspector's rows, as they are declared there.
const POS_X: RowSpec = { min: -0.5, max: 0.5, step: 0.01, neutral: 0 };
const CONTRAST: RowSpec = { min: 0, max: 4, step: 0.05, neutral: 1 };
const SCALE: RowSpec = { min: 0.1, max: 2, step: 0.05, neutral: 1, scale: 100, digits: 0, unit: '%' };
const RATE: RowSpec = { min: 0.25, max: 4, step: 0.25, neutral: 1, unit: '×' };
const ROTATION: RowSpec = { min: -180, max: 180, step: 1, digits: 0, unit: '°' };
const CROP: RowSpec = { min: 0, max: 0.9, step: 0.01, neutral: 0 };
const OPACITY: RowSpec = { min: 0, max: 1, step: 0.05, neutral: 1, scale: 100, digits: 0, unit: '%' };
const CENTRE: RowSpec = { min: 0, max: 1, step: 0.01, neutral: 0.5, bipolar: true };
const FADE: RowSpec = { min: 0, max: 5, step: 0.1, neutral: 0, ...SECONDS, limit: { max: 12.5 } };

describe('toPos / fromPos', () => {
	test('a value is its place on the travel, held to it', () => {
		expect(toPos(0, POS_X)).toBe(0.5);
		expect(toPos(-0.5, POS_X)).toBe(0);
		expect(toPos(9, POS_X)).toBe(1);
		expect(toPos(-9, POS_X)).toBe(0);
		expect(toPos(1, CONTRAST)).toBe(0.25);
		expect(toPos(1, { min: 3, max: 3, step: 1 })).toBe(0);
	});

	test('a place is a value on the step grid that starts at the minimum', () => {
		expect(fromPos(0.5, POS_X)).toBe(0);
		expect(fromPos(0.2504, POS_X)).toBe(-0.25);
		expect(fromPos(0, RATE)).toBe(0.25);
		expect(fromPos(1, RATE)).toBe(4);
		// 0.25 + 0.3 × 3.75 = 1.375, which is between two steps; the nearer one wins.
		expect(fromPos(0.3, RATE)).toBe(1.5);
		expect(fromPos(2, ROTATION)).toBe(180);
		expect(fromPos(-1, ROTATION)).toBe(-180);
	});

	test('no float noise: 0.05 steps stay 0.05 steps', () => {
		expect(fromPos(0.0125, CONTRAST)).toBe(0.05);
		expect(snap(0.1 + 0.2, { min: 0, max: 1, step: 0.1 })).toBe(0.3);
	});
});

describe('bipolar fill and the rest tick', () => {
	test('a travel across zero is bipolar and rests at zero', () => {
		const brightness: RowSpec = { min: -1, max: 1, step: 0.05 };
		expect(isBipolar(brightness)).toBe(true);
		expect(neutralOf(brightness)).toBe(0);
		expect(fillOrigin(brightness)).toBe(0.5);
		expect(fillOrigin(POS_X)).toBe(0.5);
		expect(fillOrigin(ROTATION)).toBe(0.5);
	});

	test('a travel that does not cross zero fills from the left, and its tick is where it rests', () => {
		expect(isBipolar(CONTRAST)).toBe(false);
		expect(fillOrigin(CONTRAST)).toBe(0);
		expect(tickPos(CONTRAST)).toBe(0.25);
		expect(tickPos(SCALE)).toBeCloseTo(0.9 / 1.9, 10);
	});

	test('a centre that is half the travel is bipolar by being told so', () => {
		expect(isBipolar({ min: 0, max: 1, step: 0.01 })).toBe(false);
		expect(isBipolar(CENTRE)).toBe(true);
		expect(fillOrigin(CENTRE)).toBe(0.5);
		expect(tickPos(CENTRE)).toBe(0.5);
	});

	test('a rest at the end of the travel is not drawn: it is the end', () => {
		expect(tickPos(CROP)).toBeNull();
		expect(tickPos(OPACITY)).toBeNull();
		expect(tickPos(FADE)).toBeNull();
	});

	test('a row with nothing to rest at has no tick', () => {
		const overlay: RowSpec = { min: 0, max: 1, step: 0.01 };
		expect(neutralOf(overlay)).toBeNull();
		expect(tickPos(overlay)).toBeNull();
		expect(fillOrigin(overlay)).toBe(0);
	});
});

describe('reset', () => {
	test('goes back to the rest value', () => {
		expect(resetTo(0.3, POS_X)).toBe(0);
		expect(resetTo(2.5, CONTRAST)).toBe(1);
		expect(resetTo(0.4, CENTRE)).toBe(0.5);
		expect(resetTo(1.5, OPACITY)).toBe(1);
	});

	test('is nothing when the row is already there, so a double-click writes no edit', () => {
		expect(resetTo(0, POS_X)).toBeNull();
		expect(resetTo(1, CONTRAST)).toBeNull();
		expect(resetTo(0.9, SCALE)).toBe(1);
		// Float noise from a drag is not a difference.
		expect(resetTo(0.1 + 0.2 - 0.3, POS_X)).toBeNull();
	});

	test('puts back a value that only reads like the rest: 0.4° shows as 0° and is not 0', () => {
		expect(resetTo(0.4, ROTATION)).toBe(0);
		expect(resetTo(0.996, SCALE)).toBe(1);
		expect(resetTo(1.004, CONTRAST)).toBe(1);
	});

	test('is nothing when the row has no rest to go back to', () => {
		expect(resetTo(0.3, { min: 0, max: 1, step: 0.01 })).toBeNull();
	});
});

describe('readout', () => {
	test('is the value in the units it is shown in', () => {
		expect(readout(0, POS_X)).toBe('0.00');
		expect(readout(-0.256, POS_X)).toBe('-0.26');
		expect(readout(0.55, SCALE)).toBe('55%');
		expect(readout(1, RATE)).toBe('1.00×');
		expect(readout(45.4, ROTATION)).toBe('45°');
		expect(readout(1.25, FADE)).toBe('1.25s');
	});

	test('never shows a negative zero', () => {
		expect(readout(-0.4, ROTATION)).toBe('0°');
		expect(readout(-0.001, POS_X)).toBe('0.00');
	});

	test('the editable text drops the unit', () => {
		expect(editable(0.55, SCALE)).toBe('55');
		expect(editable(1, RATE)).toBe('1.00');
	});
});

describe('parseEntry', () => {
	test('full-width digits, point and units, as an IME writes them, parse', () => {
		expect(parseEntry('５５', SCALE)).toBe(0.55);
		expect(parseEntry('１．２５', RATE)).toBe(1.25);
		expect(parseEntry('５０％', SCALE)).toBe(0.5);
		expect(parseEntry('－４５', ROTATION)).toBe(-45);
		expect(parseEntry('１２ｓ', FADE)).toBe(12);
	});

	test('reads a number with or without its unit, in shown units', () => {
		expect(parseEntry('55', SCALE)).toBe(0.55);
		expect(parseEntry('55%', SCALE)).toBe(0.55);
		expect(parseEntry(' 55 % ', SCALE)).toBe(0.55);
		expect(parseEntry('1.1', RATE)).toBe(1.1);
		expect(parseEntry('0.8x', RATE)).toBe(0.8);
		expect(parseEntry('0,8×', RATE)).toBe(0.8);
		expect(parseEntry('-30°', ROTATION)).toBe(-30);
		expect(parseEntry('+0.25', POS_X)).toBe(0.25);
		expect(parseEntry('.5', CENTRE)).toBe(0.5);
	});

	test('a fade can be a quarter second or a few frames: two digits, not one', () => {
		expect(parseEntry('0.25', FADE)).toBe(0.25);
		expect(parseEntry('0.04', FADE)).toBe(0.04);
	});

	test('is not held to the slider step, only to the digits shown', () => {
		expect(parseEntry('1.1', RATE)).toBe(1.1);
		expect(parseEntry('1.234', RATE)).toBe(1.23);
		expect(parseEntry('12.4', ROTATION)).toBe(12);
	});

	test('is held to the travel, or to the typed limit when the row has one', () => {
		expect(parseEntry('900', ROTATION)).toBe(180);
		expect(parseEntry('-900', ROTATION)).toBe(-180);
		expect(parseEntry('10', RATE)).toBe(4);
		expect(parseEntry('0', RATE)).toBe(0.25);
		expect(parseEntry('8', FADE)).toBe(8);
		expect(parseEntry('30', FADE)).toBe(12.5);
	});

	test('refuses what is not a number', () => {
		for (const bad of ['', '  ', 'abc', '1..2', '1e3', '--1', '5 5', '%', '5%%', 'Infinity', 'NaN']) {
			expect(parseEntry(bad, SCALE), JSON.stringify(bad)).toBeNull();
		}
	});
});

describe('entryFor', () => {
	test('a changed number is one edit', () => {
		expect(entryFor('1.1', 1, RATE)).toEqual({ kind: 'commit', value: 1.1 });
		expect(entryFor('30', 0.5, SCALE)).toEqual({ kind: 'commit', value: 0.3 });
	});

	test('the number it already has puts the field back and writes nothing', () => {
		expect(entryFor('1.00', 1, RATE)).toEqual({ kind: 'revert' });
		expect(entryFor('100%', 1, SCALE)).toEqual({ kind: 'revert' });
	});

	test('a number that only reads like the one it has is still a change', () => {
		expect(entryFor('100', 0.996, SCALE)).toEqual({ kind: 'commit', value: 1 });
		expect(entryFor('0', 0.4, ROTATION)).toEqual({ kind: 'commit', value: 0 });
	});

	test('so does text that is not a number', () => {
		expect(entryFor('fast', 1, RATE)).toEqual({ kind: 'revert' });
		expect(entryFor('', 1, RATE)).toEqual({ kind: 'revert' });
	});

	test('an entry past the travel lands on its end, which may be where it already is', () => {
		expect(entryFor('999', 180, ROTATION)).toEqual({ kind: 'revert' });
		expect(entryFor('999', 90, ROTATION)).toEqual({ kind: 'commit', value: 180 });
	});
});

describe('stepBy', () => {
	const step = (dir: 1 | -1, size: 'fine' | 'normal' | 'coarse' | 'page' = 'normal') => ({ kind: 'step' as const, dir, size });

	test('an arrow is one step, snapped', () => {
		expect(stepBy(0, step(1), POS_X)).toBe(0.01);
		expect(stepBy(1, step(-1), RATE)).toBe(0.75);
		expect(stepBy(0.1 + 0.2, step(1), { min: 0, max: 1, step: 0.1 })).toBe(0.4);
	});

	test('Shift is five steps, Alt a digit, a page a tenth of the travel', () => {
		expect(stepBy(0, step(1, 'coarse'), POS_X)).toBe(0.05);
		expect(stepBy(1, step(1, 'fine'), RATE)).toBe(1.01);
		expect(stepBy(0, step(1, 'page'), ROTATION)).toBe(36);
		expect(stepBy(0, step(1, 'page'), POS_X)).toBe(0.1);
	});

	test('Alt never steps further than a plain arrow', () => {
		expect(stepBy(0, step(1, 'fine'), ROTATION)).toBe(1);
	});

	test('the ends stop the travel; Home and End are the ends', () => {
		expect(stepBy(0.5, step(1), POS_X)).toBe(0.5);
		expect(stepBy(-0.5, step(-1), POS_X)).toBe(-0.5);
		expect(stepBy(3, { kind: 'edge', to: 0 }, RATE)).toBe(0.25);
		expect(stepBy(3, { kind: 'edge', to: 1 }, RATE)).toBe(4);
	});
});

describe('the unit presets', () => {
	test('read as the Inspector always has', () => {
		expect(readout(0.55, { min: 0, max: 1, step: 0.05, ...PERCENT })).toBe('55%');
		expect(readout(12.6, { min: -180, max: 180, step: 1, ...DEGREES })).toBe('13°');
		expect(readout(0.5, { min: 0.25, max: 4, step: 0.25, ...TIMES })).toBe('0.50×');
		expect(readout(0.5, { min: 0, max: 5, step: 0.1, ...SECONDS })).toBe('0.50s');
	});
});

describe('NUMBER_COL', () => {
	test('the number column fits what a row reads, in 12 px mono (0.6 em a character) with its 3 px padding and 1 px border', () => {
		const room = (NUMBER_COL - 2 * (3 + 1)) / 7.2;
		const longest = [readout(10, FADE), readout(100, FADE), readout(-10, FADE), readout(-1, RATE), readout(-180, ROTATION), readout(4, SCALE)];
		expect(longest).toContain('100.00s');
		for (const text of longest) expect(text.length, text).toBeLessThanOrEqual(room);
	});
});

describe('Pending', () => {
	const A = { scope: 'clip A', value: 1, spec: FADE };
	const B = { scope: 'clip B', value: 2, spec: { ...FADE, limit: { max: 3 } } };

	test('an edit ends on what it began on, not on what is selected by then', () => {
		const edit = new Pending<string>();
		edit.begin(A.scope, A.value, A.spec);
		expect(edit.end(B)).toEqual(A);
	});

	test('with no beginning it ends where it is: a double-click is its own whole gesture', () => {
		expect(new Pending<string>().end(B)).toEqual(B);
	});

	test('the first beginning stands: a drag does not move the target it started on', () => {
		const edit = new Pending<string>();
		edit.begin(A.scope, A.value, A.spec);
		edit.begin(B.scope, B.value, B.spec);
		expect(edit.end(B)).toEqual(A);
	});

	test('it ends once: the next edit begins afresh', () => {
		const edit = new Pending<string>();
		edit.begin(A.scope, A.value, A.spec);
		edit.end(A);
		expect(edit.end(B)).toEqual(B);
	});

	test('a given-up edit leaves nothing behind', () => {
		const edit = new Pending<string>();
		edit.begin(A.scope, A.value, A.spec);
		edit.drop();
		expect(edit.end(B)).toEqual(B);
	});

	test('a clip and a playhead time travel together', () => {
		const edit = new Pending<{ clip: string; time: number }>();
		edit.begin({ clip: 'c1', time: 1.5 }, 0.4, FADE);
		expect(edit.end({ scope: { clip: 'c2', time: 9 }, value: 1, spec: FADE })).toEqual({ scope: { clip: 'c1', time: 1.5 }, value: 0.4, spec: FADE });
	});

	test('a typed entry is held to the limit it began with, not the new selection’s', () => {
		// A fade typed on a 20 s clip and committed by the click that selects a 3 s clip.
		const onA: RowSpec = { ...FADE, limit: { max: 20 } };
		const onB: RowSpec = { ...FADE, limit: { max: 3 } };
		const edit = new Pending<string>();
		edit.begin('clip A', 0, onA);
		const from = edit.end({ scope: 'clip B', value: 0, spec: onB });
		expect(parseEntry('12', from.spec)).toBe(12);
		expect(parseEntry('12', onB)).toBe(3);
		expect(entryFor('12', from.value, from.spec)).toEqual({ kind: 'commit', value: 12 });
	});
});
