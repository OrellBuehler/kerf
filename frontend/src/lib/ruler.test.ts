import { describe, expect, test } from 'bun:test';
import {
	MIN_LABEL_PX,
	MIN_MINOR_PX,
	RULER_SECONDS,
	frameTicksIn,
	minorTicksIn,
	nominalRate,
	rulerGrid,
	rulerStep,
	showsFrameTicks,
	tickLabel,
	ticksIn
} from './ruler';
import { formatFrames, formatTimecode, timecodeRate } from './timecode';
import { ZOOM_MAX, ZOOM_MIN } from './zoom';

const RATES = [24, 25, 30, 60, 30000 / 1001, 24000 / 1001];
const zooms = (from = ZOOM_MIN, to = ZOOM_MAX, by = 1.07) => {
	const out: number[] = [];
	for (let z = from; z <= to; z *= by) out.push(z);
	return out;
};

describe('rulerStep', () => {
	test('keeps labels at the spacing the old ladder gave at the common zooms', () => {
		expect(rulerStep(36)).toBe(5);
		expect(rulerStep(96)).toBe(2);
		expect(rulerStep(8)).toBe(15);
	});

	test('always leaves a label its room, across the whole zoom range and every rate', () => {
		for (const fps of RATES) {
			for (const z of zooms()) {
				const step = rulerStep(z, fps);
				const coarsest = (RULER_SECONDS[RULER_SECONDS.length - 1] * nominalRate(fps)) / fps;
				// The last rung is the coarsest there is; every other zoom must fit.
				if (step < coarsest) expect(step * z).toBeGreaterThanOrEqual(MIN_LABEL_PX - 1e-6);
			}
		}
	});

	test('picks the finest step that does, never a coarser one than needed', () => {
		for (const fps of RATES) {
			let last = 0;
			const seen: number[] = [];
			for (const z of zooms(ZOOM_MIN, ZOOM_MAX, 1.013)) {
				const step = rulerStep(z, fps);
				if (step !== last) seen.push(step);
				last = step;
			}
			// A step is dropped for a finer one only when the finer fits: so the one left
			// behind, at the zoom it was left, did not.
			for (let i = 1; i < seen.length; i++) expect(seen[i]).toBeLessThan(seen[i - 1]);
		}
	});

	test('gets finer as the zoom goes in', () => {
		for (const fps of RATES) {
			let last = Infinity;
			for (const z of zooms(ZOOM_MIN, ZOOM_MAX, 1.2)) {
				const s = rulerStep(z, fps);
				expect(s).toBeLessThanOrEqual(last);
				last = s;
			}
		}
	});

	test('reaches single frames at frame-level zoom and hour-scale steps when fully out', () => {
		expect(rulerStep(ZOOM_MAX, 30)).toBeLessThan(0.1);
		expect(rulerStep(ZOOM_MIN)).toBeGreaterThanOrEqual(900);
	});

	test('a zoom that is not a number gets the coarsest step rather than a loop', () => {
		expect(rulerStep(NaN)).toBe(RULER_SECONDS[RULER_SECONDS.length - 1]);
		expect(rulerStep(0)).toBe(RULER_SECONDS[RULER_SECONDS.length - 1]);
	});

	test('the ladder is ascending', () => {
		for (let i = 1; i < RULER_SECONDS.length; i++) expect(RULER_SECONDS[i]).toBeGreaterThan(RULER_SECONDS[i - 1]);
	});

	test('a step under a second is a whole number of frames, and a second step is whole timecode seconds', () => {
		for (const fps of RATES) {
			const nominal = nominalRate(fps);
			for (const z of zooms(10, ZOOM_MAX, 1.05)) {
				const g = rulerGrid(z, fps);
				const frames = g.step * fps;
				expect(Math.abs(frames - Math.round(frames))).toBeLessThan(1e-6);
				expect(g.frames).toBe(Math.round(frames) < nominal);
				if (!g.frames) expect(Math.round(frames) % nominal).toBe(0);
			}
		}
	});

	test('at a fractional rate a labelled second is on a frame, not a hair off it', () => {
		const fps = 24000 / 1001;
		const g = rulerGrid(36, fps);
		expect(g.step).toBeCloseTo((5 * 24) / fps, 12);
		expect(g.step).toBeGreaterThan(5);
		for (let k = 0; k < 500; k++) expect(tickLabel(k, g, fps)).toBe(hms(k * 5));
	});

	test('at a drop-frame rate a label is the drop-frame timecode of its frame, as the playhead reads it', () => {
		for (const fps of [30000 / 1001, 60000 / 1001]) {
			const r = timecodeRate(fps);
			for (const z of [36, 2, ZOOM_MAX]) {
				const g = rulerGrid(z, fps);
				const f = Math.round(g.step * fps);
				for (let k = 0; k < 2000; k += 7) {
					expect(tickLabel(k, g, fps)).toBe(formatFrames(r, k * f));
					expect(tickLabel(k, g, fps)).toBe(formatTimecode(k * g.step, fps));
				}
			}
		}
		// Past the first minute the labels are drop-frame: 00;01;00;00 does not exist.
		expect(tickLabel(12, rulerGrid(36, 30000 / 1001), 30000 / 1001)).toBe('00;01;00;02');
	});
});

/** `HH:MM:SS` of whole seconds. */
const hms = (sec: number) =>
	[Math.floor(sec / 3600), Math.floor(sec / 60) % 60, sec % 60].map((n) => String(n).padStart(2, '0')).join(':');

describe('rulerGrid minors', () => {
	test('divide a step into round parts, never closer than the minimum', () => {
		for (const fps of RATES) {
			for (const z of zooms()) {
				const g = rulerGrid(z, fps);
				if (g.divisions > 1) expect((g.step * z) / g.divisions).toBeGreaterThanOrEqual(MIN_MINOR_PX - 1e-6);
				if (g.frames) expect(g.divisions).toBe(1);
			}
		}
	});

	test('a five-second step has ten minors, one every half second', () => {
		const g = rulerGrid(36);
		expect([g.step, g.divisions]).toEqual([5, 10]);
		const minors = minorTicksIn(0, 10, g);
		expect(minors.map((m) => m.t)).toEqual([0.5, 1, 1.5, 2, 2.5, 3, 3.5, 4, 4.5, 5.5, 6, 6.5, 7, 7.5, 8, 8.5, 9, 9.5]);
	});

	test('skips the ticks a label stands on and marks the middle one', () => {
		const g = rulerGrid(36);
		const minors = minorTicksIn(0, 15, g);
		for (const m of minors) expect(m.k % g.divisions).not.toBe(0);
		expect(minors.filter((m) => m.mid).map((m) => m.t)).toEqual([2.5, 7.5, 12.5]);
	});

	test('an odd division has no middle', () => {
		const g = rulerGrid(8);
		expect([g.step, g.divisions]).toEqual([15, 3]);
		expect(minorTicksIn(0, 45, g).some((m) => m.mid)).toBe(false);
		expect(minorTicksIn(0, 45, g).map((m) => m.t)).toEqual([5, 10, 20, 25, 35, 40]);
	});

	test('are none when the step has no room for them, or is a frame count', () => {
		expect(minorTicksIn(0, 100, { step: 1, frames: false, divisions: 1 })).toEqual([]);
		expect(minorTicksIn(0, 10, rulerGrid(ZOOM_MAX, 30))).toEqual([]);
	});

	test('a window the width of a screen holds a bounded number of them at every zoom', () => {
		for (const z of zooms(ZOOM_MIN, ZOOM_MAX, 1.3)) {
			const g = rulerGrid(z, 30);
			expect(minorTicksIn(5000 / z, 7000 / z, g).length).toBeLessThanOrEqual(2000 / MIN_MINOR_PX + 12);
		}
	});

	test('never start before zero and are bounded', () => {
		const g = rulerGrid(36);
		expect(minorTicksIn(-30, 10, g)[0].t).toBe(0.5);
		expect(minorTicksIn(0, 1e9, g, 300).length).toBe(300);
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
	test('a whole-second step reads HH:MM:SS, the project timecode without its frames', () => {
		const g = rulerGrid(36);
		expect(tickLabel(0, g, 30)).toBe('00:00:00');
		expect(tickLabel(3, g, 30)).toBe('00:00:15');
		expect(tickLabel(13, g, 30)).toBe('00:01:05');
	});

	test('hours carry, so an hour is not 60:00', () => {
		const g = rulerGrid(0.5);
		expect(g.step).toBe(300);
		expect(tickLabel(12, g, 30)).toBe('01:00:00');
		expect(tickLabel(13, g, 30)).toBe('01:05:00');
	});

	test('a sub-second step reads HH:MM:SS:FF and lands on the frame it names', () => {
		for (const fps of RATES) {
			const g = rulerGrid(ZOOM_MAX, fps);
			expect(g.frames).toBe(true);
			const nominal = nominalRate(fps);
			const f = Math.round(g.step * fps);
			for (let k = 0; k < 400; k++) {
				const [, , s, ff] = tickLabel(k, g, fps).split(/[:;]/).map(Number);
				const frame = (Math.floor((k * f) / nominal) % 60) * nominal + ((k * f) % nominal);
				expect(s * nominal + ff).toBe(frame);
			}
		}
	});

	test('the label of a tick is the transport timecode of its time', () => {
		const g = rulerGrid(500, 24);
		expect(g.step).toBeCloseTo(5 / 24, 12);
		expect(tickLabel(7, g, 24)).toBe('00:00:01:11');
		expect(tickLabel(7, g, 24)).toBe(formatTimecode(7 * g.step, 24));
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

describe('tickLabel in seconds mode', () => {
	test('a whole-second step is m:ss, h:mm:ss from an hour, and never a tenth it does not have', () => {
		const g = rulerGrid(8, 30);
		expect(g.frames).toBe(false);
		expect(tickLabel(0, g, 30, 'seconds')).toBe('0:00');
		const five = { step: 5, frames: false, divisions: 1 };
		expect(tickLabel(13, five, 30, 'seconds')).toBe('1:05');
		expect(tickLabel(720, five, 30, 'seconds')).toBe('1:00:00');
		expect(tickLabel(721, five, 30, 'seconds')).toBe('1:00:05');
	});

	test('a step of a timecode second at 29.97 is labelled by the second it is nearest', () => {
		const g = { step: 30 / 29.97, frames: false, divisions: 1 };
		expect(tickLabel(59, g, 29.97, 'seconds')).toBe('0:59');
		expect(tickLabel(60, g, 29.97, 'seconds')).toBe('1:00');
	});

	test('below a second the fraction is there, to the millisecond, so two frame ticks never read alike', () => {
		const frame = { step: 1 / 30, frames: true, divisions: 1 };
		const labels = Array.from({ length: 31 }, (_, k) => tickLabel(k, frame, 30, 'seconds'));
		expect(new Set(labels).size).toBe(31);
		expect(labels[1]).toBe('0:00.033');
		expect(labels[15]).toBe('0:00.5');
		expect(labels[30]).toBe('0:01.0');
		expect(tickLabel(3, { step: 0.5, frames: true, divisions: 1 }, 30, 'seconds')).toBe('0:01.5');
	});

	test('timecode is still the default, and says the same as before', () => {
		const g = rulerGrid(8, 30);
		expect(tickLabel(13, { step: 5, frames: false, divisions: 1 }, 30)).toBe('00:01:05');
		expect(tickLabel(13, { step: 5, frames: false, divisions: 1 }, 30, 'timecode')).toBe('00:01:05');
		expect(tickLabel(0, g, 30)).toBe(tickLabel(0, g, 30, 'timecode'));
	});
});
