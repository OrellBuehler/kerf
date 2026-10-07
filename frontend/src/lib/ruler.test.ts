import { describe, expect, test } from 'bun:test';
import {
	MIN_LABEL_PX,
	RULER_STEPS,
	frameTicksIn,
	labelDecimals,
	rulerStep,
	showsFrameTicks,
	tickLabel,
	ticksIn
} from './ruler';
import { ZOOM_MAX, ZOOM_MIN } from './zoom';

describe('rulerStep', () => {
	test('keeps labels at the spacing the old ladder gave at the common zooms', () => {
		expect(rulerStep(36)).toBe(5);
		expect(rulerStep(96)).toBe(2);
		expect(rulerStep(8)).toBe(15);
	});

	test('always leaves a label its room, across the whole zoom range', () => {
		for (let z = ZOOM_MIN; z <= ZOOM_MAX; z *= 1.07) {
			const step = rulerStep(z);
			// The last rung is the coarsest there is; every other zoom must fit.
			if (step < RULER_STEPS[RULER_STEPS.length - 1]) expect(step * z).toBeGreaterThanOrEqual(MIN_LABEL_PX - 1e-6);
		}
	});

	test('picks the finest step that does, never a coarser one than needed', () => {
		for (let z = ZOOM_MIN; z <= ZOOM_MAX; z *= 1.13) {
			const step = rulerStep(z);
			const i = RULER_STEPS.indexOf(step);
			if (i > 0) expect(RULER_STEPS[i - 1] * z).toBeLessThan(MIN_LABEL_PX);
		}
	});

	test('gets finer as the zoom goes in', () => {
		let last = Infinity;
		for (let z = ZOOM_MIN; z <= ZOOM_MAX; z *= 1.2) {
			const s = rulerStep(z);
			expect(s).toBeLessThanOrEqual(last);
			last = s;
		}
	});

	test('reaches sub-second steps at frame-level zoom and hour-scale steps when fully out', () => {
		expect(rulerStep(ZOOM_MAX)).toBeLessThan(0.1);
		expect(rulerStep(ZOOM_MIN)).toBeGreaterThanOrEqual(900);
	});

	test('a zoom that is not a number gets the coarsest step rather than a loop', () => {
		expect(rulerStep(NaN)).toBe(RULER_STEPS[RULER_STEPS.length - 1]);
		expect(rulerStep(0)).toBe(RULER_STEPS[RULER_STEPS.length - 1]);
	});

	test('the ladder is ascending', () => {
		for (let i = 1; i < RULER_STEPS.length; i++) expect(RULER_STEPS[i]).toBeGreaterThan(RULER_STEPS[i - 1]);
	});
});

describe('ticksIn', () => {
	test('lists the multiples of the step in the range, ends included', () => {
		expect(ticksIn(0, 20, 5).map((t) => t.t)).toEqual([0, 5, 10, 15, 20]);
		expect(ticksIn(7, 21, 5).map((t) => t.t)).toEqual([10, 15, 20]);
	});

	test('the index is the position on the grid, so a window can start anywhere', () => {
		const ticks = ticksIn(11, 31, 5);
		expect(ticks.map((t) => t.k)).toEqual([3, 4, 5, 6]);
	});

	test('a fractional step does not drift', () => {
		const ticks = ticksIn(0, 100, 0.1);
		const last = ticks[ticks.length - 1];
		expect(last.k).toBe(1000);
		expect(Math.abs(last.t - 100)).toBeLessThan(1e-9);
		expect(ticksIn(0, 1, 0.1).map((t) => t.k)).toEqual([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
	});

	test('never starts before zero', () => {
		expect(ticksIn(-30, 10, 5)[0].t).toBe(0);
	});

	test('is bounded however the range was asked for', () => {
		expect(ticksIn(0, 1e9, 0.05, 500).length).toBe(500);
	});

	test('is empty for a step or a range that makes no sense', () => {
		expect(ticksIn(0, 10, 0)).toEqual([]);
		expect(ticksIn(0, 10, -1)).toEqual([]);
		expect(ticksIn(10, 0, 5)).toEqual([]);
		expect(ticksIn(NaN, 10, 5)).toEqual([]);
	});

	test('a window the width of a screen holds a bounded number of ticks at every zoom', () => {
		for (let z = ZOOM_MIN; z <= ZOOM_MAX; z *= 1.3) {
			const step = rulerStep(z);
			const n = ticksIn(5000 / z, 7000 / z, step).length;
			expect(n).toBeLessThanOrEqual(2000 / MIN_LABEL_PX + 2);
		}
	});
});

describe('tickLabel', () => {
	test('whole-second steps read mm:ss', () => {
		expect(tickLabel(0, 5)).toBe('00:00');
		expect(tickLabel(3, 5)).toBe('00:15');
		expect(tickLabel(13, 5)).toBe('01:05');
	});

	test('minutes carry past 59, as the old ruler did', () => {
		expect(tickLabel(75, 60)).toBe('75:00');
	});

	test('sub-second steps carry the fraction they need', () => {
		expect(labelDecimals(1)).toBe(0);
		expect(labelDecimals(0.5)).toBe(1);
		expect(labelDecimals(0.1)).toBe(1);
		expect(labelDecimals(0.05)).toBe(2);
		expect(tickLabel(3, 0.5)).toBe('00:01.5');
		expect(tickLabel(13, 0.05)).toBe('00:00.65');
	});

	test('a tenth reads as a tenth, not one off through float noise', () => {
		for (let k = 0; k < 700; k++) {
			const label = tickLabel(k, 0.1);
			const [mmss, f] = label.split('.');
			const [m, s] = mmss.split(':').map(Number);
			expect(m * 600 + s * 10 + Number(f)).toBe(k);
		}
	});

	test('hundredths too', () => {
		for (let k = 0; k < 2500; k += 7) {
			const label = tickLabel(k, 0.05);
			const [mmss, f] = label.split('.');
			const [m, s] = mmss.split(':').map(Number);
			expect(m * 6000 + s * 100 + Number(f)).toBe(k * 5);
		}
	});
});

describe('frame ticks', () => {
	test('appear once a frame is wide enough to draw', () => {
		expect(showsFrameTicks(36, 30)).toBe(false);
		expect(showsFrameTicks(240, 30)).toBe(true);
		expect(showsFrameTicks(2000, 60)).toBe(true);
		expect(showsFrameTicks(100, 0)).toBe(false);
	});

	test('sit on whole frames', () => {
		const ticks = frameTicksIn(1, 1.2, 30, 600);
		expect(ticks.map((t) => t.k)).toEqual([30, 31, 32, 33, 34, 35, 36]);
		for (const t of ticks) expect(t.t).toBe(t.k / 30);
	});

	test('are empty when the zoom is too shallow, so asking is always safe', () => {
		expect(frameTicksIn(0, 100, 30, 36)).toEqual([]);
	});

	test('are bounded', () => {
		expect(frameTicksIn(0, 1e6, 30, 2000, 300).length).toBe(300);
	});
});
