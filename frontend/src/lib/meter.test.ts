import { describe, expect, test } from 'bun:test';
import {
	clearMeter,
	clearStereo,
	clippedAny,
	HOLD_FALL_DB_PER_S,
	HOLD_MS,
	IDLE_METER,
	IDLE_STEREO,
	MAX_STEP_MS,
	METER_CLIP_DB,
	METER_FLOOR_DB,
	PEAK_FALL_DB_PER_S,
	peakLabel,
	readBlock,
	RMS_FALL_MS,
	RMS_RISE_MS,
	SILENCE,
	settleMeter,
	settleStereo,
	stepMeter,
	stepStereo,
	strongest,
	toDb,
	type ChannelMeter,
	type Reading
} from './meter';

const at = (peak: number, rms = peak - 6): Reading => ({ peak, rms });
/** Runs a meter through `frames` identical readings, `dt` ms apart. */
const run = (m: ChannelMeter, r: Reading, frames: number, dt = 16): ChannelMeter => {
	for (let i = 0; i < frames; i++) m = stepMeter(m, r, dt);
	return m;
};

describe('reading a block', () => {
	test('peak is the loudest sample, RMS the power', () => {
		const sine = Float32Array.from({ length: 4800 }, (_, i) => 0.5 * Math.sin((2 * Math.PI * 100 * i) / 48000));
		const r = readBlock(sine);
		expect(r.peak).toBeCloseTo(-6.02, 1);
		// A sine's RMS is its peak less 3 dB.
		expect(r.rms).toBeCloseTo(-9.03, 1);
		expect(readBlock([1, -1, 1, -1])).toEqual({ peak: 0, rms: 0 });
		expect(readBlock([0.1, -0.9, 0.2]).peak).toBeCloseTo(toDb(0.9), 12);
	});

	test('silence and an empty block read as the floor', () => {
		expect(readBlock([])).toEqual(SILENCE);
		expect(readBlock(new Float32Array(1024))).toEqual(SILENCE);
		expect(toDb(0)).toBe(METER_FLOOR_DB);
		expect(toDb(Number.NaN)).toBe(METER_FLOOR_DB);
		expect(toDb(1e-9)).toBe(METER_FLOOR_DB);
		expect(toDb(1)).toBe(0);
	});

	test('a signal over full scale reads over 0 dB — float audio does not clip until the end', () => {
		expect(readBlock([2, -2]).peak).toBeCloseTo(6.02, 2);
	});
});

describe('the peak bar', () => {
	test('rises at once', () => {
		const m = stepMeter(IDLE_METER, at(-12), 16);
		expect(m.peak).toBe(-12);
		expect(stepMeter(m, at(-3), 16).peak).toBe(-3);
	});

	test('falls at a fixed rate, whatever the frame time', () => {
		const m = stepMeter(IDLE_METER, at(-6), 16);
		const half = stepMeter(m, SILENCE, 100);
		expect(half.peak).toBeCloseTo(-6 - PEAK_FALL_DB_PER_S * 0.1, 9);
		// Two frames of 50 ms are one of 100.
		const two = stepMeter(stepMeter(m, SILENCE, 50), SILENCE, 50);
		expect(two.peak).toBeCloseTo(half.peak, 9);
	});

	test('never falls below the floor, and a long gap integrates only so far', () => {
		expect(run(IDLE_METER, SILENCE, 100).peak).toBe(METER_FLOOR_DB);
		const loud = stepMeter(IDLE_METER, at(-6), 16);
		const stalled = stepMeter(loud, SILENCE, 60_000);
		expect(stalled.peak).toBeCloseTo(-6 - (PEAK_FALL_DB_PER_S * MAX_STEP_MS) / 1000, 9);
		expect(stepMeter(loud, SILENCE, Number.NaN).peak).toBe(-6);
		expect(stepMeter(loud, SILENCE, -5).peak).toBe(-6);
	});
});

describe('the RMS bar', () => {
	test('eases toward the reading, quicker up than down', () => {
		const up = stepMeter(IDLE_METER, at(0, -20), RMS_RISE_MS);
		// One time constant covers 1 - 1/e of the way.
		expect(up.rms).toBeCloseTo(METER_FLOOR_DB + (-20 - METER_FLOOR_DB) * (1 - Math.exp(-1)), 6);
		const loud: ChannelMeter = { ...IDLE_METER, rms: -20 };
		const down = stepMeter(loud, SILENCE, MAX_STEP_MS);
		expect(down.rms).toBeCloseTo(-20 + (METER_FLOOR_DB + 20) * (1 - Math.exp(-MAX_STEP_MS / RMS_FALL_MS)), 6);
		// The same distance, the same time: rising covers more than falling.
		const rise = stepMeter({ ...IDLE_METER, rms: -50 }, at(0, -20), 50).rms - -50;
		const fall = -20 - stepMeter({ ...IDLE_METER, rms: -20 }, at(0, -50), 50).rms;
		expect(rise).toBeGreaterThan(fall);
	});

	test('settles on a steady level', () => {
		expect(run(IDLE_METER, at(-6, -18), 200).rms).toBeCloseTo(-18, 2);
	});

	test('RMS stays under the peak for a real signal', () => {
		const m = run(IDLE_METER, at(-6, -14), 100);
		expect(m.rms).toBeLessThan(m.peak);
	});
});

describe('the hold marker', () => {
	test('takes a higher peak at once and parks there', () => {
		let m = stepMeter(IDLE_METER, at(-3), 16);
		expect(m.hold).toBe(-3);
		m = run(m, SILENCE, 50, 16); // 800 ms: still holding
		expect(m.hold).toBe(-3);
		expect(m.holdAge).toBeCloseTo(800, 6);
	});

	test('lets go after the hold time, falling slower than the bar it sits over would', () => {
		let m = stepMeter(IDLE_METER, at(-3), 16);
		m = run(m, SILENCE, Math.ceil(HOLD_MS / 16) + 1, 16);
		const before = m.hold;
		m = stepMeter(m, SILENCE, 100);
		expect(m.hold).toBeCloseTo(before - HOLD_FALL_DB_PER_S * 0.1, 6);
		expect(m.hold).toBeLessThan(-3);
	});

	test('is never under the peak bar', () => {
		let m = IDLE_METER;
		for (let i = 0; i < 300; i++) {
			m = stepMeter(m, i % 40 === 0 ? at(-2) : SILENCE, 16);
			expect(m.hold).toBeGreaterThanOrEqual(m.peak);
		}
	});

	test('a new peak under the held one does not move it, one equal to it restarts the hold', () => {
		let m = stepMeter(IDLE_METER, at(-3), 16);
		m = run(m, SILENCE, 30, 16);
		const aged = m.holdAge;
		m = stepMeter(m, at(-10), 16);
		expect(m.hold).toBe(-3);
		expect(m.holdAge).toBeCloseTo(aged + 16, 6);
		m = stepMeter(m, at(-3), 16);
		expect(m.holdAge).toBe(0);
	});
});

describe('max and clip', () => {
	test('max is the highest peak seen, clipped latches at full scale', () => {
		let m = stepMeter(IDLE_METER, at(-8), 16);
		m = stepMeter(m, at(-2.5), 16);
		m = run(m, SILENCE, 100, 16);
		expect(m.max).toBe(-2.5);
		expect(m.clipped).toBe(false);
		m = stepMeter(m, at(METER_CLIP_DB), 16);
		expect(m.clipped).toBe(true);
		m = run(m, SILENCE, 100, 16);
		expect(m.clipped).toBe(true);
		expect(m.max).toBe(METER_CLIP_DB);
	});

	test('just under the threshold is not a clip', () => {
		expect(stepMeter(IDLE_METER, at(METER_CLIP_DB - 0.01), 16).clipped).toBe(false);
	});
});

describe('stopping', () => {
	test('the bars drop to nothing and what the run reached stays', () => {
		let m = run(IDLE_METER, at(-1.5, -12), 20);
		m = stepMeter(m, at(0), 16);
		const s = settleMeter(m);
		expect(s.peak).toBe(METER_FLOOR_DB);
		expect(s.rms).toBe(METER_FLOOR_DB);
		expect(s.hold).toBe(0);
		expect(s.max).toBe(0);
		expect(s.clipped).toBe(true);
		// Settling is idempotent, and a meter that read nothing settles to idle.
		expect(settleMeter(s)).toEqual(s);
		expect(settleMeter(IDLE_METER)).toEqual(IDLE_METER);
	});

	test('a stereo meter steps and settles channel by channel', () => {
		let m = stepStereo(IDLE_STEREO, at(-6), at(-12), 16);
		expect(m.l.peak).toBe(-6);
		expect(m.r.peak).toBe(-12);
		expect(strongest(m)).toBe(-6);
		expect(clippedAny(m)).toBe(false);
		m = stepStereo(m, at(0), SILENCE, 16);
		expect(clippedAny(m)).toBe(true);
		const s = settleStereo(m);
		expect(s.l.peak).toBe(METER_FLOOR_DB);
		expect(s.l.clipped).toBe(true);
		expect(s.r.clipped).toBe(false);
	});

	test('a reading of nothing reads as the dash', () => {
		expect(peakLabel(METER_FLOOR_DB)).toBe('−∞');
		expect(peakLabel(-6.24)).toBe('-6.2');
		expect(peakLabel(0.01)).toBe('0.0');
		expect(peakLabel(2.5)).toBe('2.5');
	});
});

describe('clearing', () => {
	test('drops the hold, the max and the lamp but not the bars', () => {
		let m = stepMeter(IDLE_METER, at(0, -10), 16);
		m = run(m, at(-20, -30), 3, 16);
		expect(m.clipped).toBe(true);
		const c = clearMeter(m);
		expect(c.clipped).toBe(false);
		expect(c.max).toBe(METER_FLOOR_DB);
		expect(c.hold).toBe(m.peak);
		expect(c.holdAge).toBe(0);
		expect(c.peak).toBe(m.peak);
		expect(c.rms).toBe(m.rms);
	});

	test('clears both channels of a strip', () => {
		const m = stepStereo(IDLE_STEREO, at(0), at(-3), 16);
		const c = clearStereo(m);
		expect(clippedAny(c)).toBe(false);
		expect(strongest(c)).toBe(METER_FLOOR_DB);
	});
});
