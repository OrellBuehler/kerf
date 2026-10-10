import { beforeEach, describe, expect, test } from 'bun:test';
import {
	cancelLevels,
	getHistory,
	getLevels,
	revertTo,
	setMasterDuck,
	setMasterLimiter,
	setMasterVolume,
	setTrackVolume
} from './api';

// Under bun there is no Tauri, so these drive the browser harness's master bus and
// its stand-in for `get_levels`, which have to keep the backend's contract: the
// same clamps, one revision per move, and a master that is absent from the
// timeline until someone touches it.

beforeEach(async () => {
	await revertTo(0);
});

describe('the master bus (browser harness)', () => {
	test('an untouched timeline has no master, and moving the fader writes one', async () => {
		const t = await setMasterVolume(0.5);
		expect(t.master).toEqual({ volume: 0.5, limiter: false, ceiling_db: -1.5 });
		// Back at the defaults it is absent again, as the saved file has it.
		const back = await setMasterVolume(1);
		expect(back.master).toBeUndefined();
	});

	test('clamps where the engine does and refuses what is not a number', async () => {
		expect((await setMasterVolume(99)).master?.volume).toBe(4);
		expect((await setMasterVolume(-3)).master?.volume).toBe(0);
		await expect(setMasterVolume(Number.NaN)).rejects.toThrow('number');
		expect((await setMasterLimiter(true, -90)).master?.ceiling_db).toBe(-24);
		expect((await setMasterLimiter(true, 6)).master?.ceiling_db).toBe(0);
		await expect(setMasterLimiter(true, Number.POSITIVE_INFINITY)).rejects.toThrow('number');
	});

	test('the limiter keeps its ceiling across off and on', async () => {
		await setMasterLimiter(true, -3.5);
		const off = await setMasterLimiter(false);
		expect(off.master).toEqual({ volume: 1, limiter: false, ceiling_db: -3.5 });
		expect((await setMasterLimiter(true)).master?.ceiling_db).toBe(-3.5);
	});

	test('each move is one revision', async () => {
		// `revertTo(0)` leaves a redo branch behind, so count from where the head is.
		const head = (await getHistory()).find((h) => h.current)!.seq;
		await setMasterVolume(0.8);
		await setMasterLimiter(true);
		const history = await getHistory();
		expect(history.find((h) => h.current)!.seq).toBe(head + 2);
		expect(history.map((h) => h.label).slice(-2)).toEqual(['Set master level', 'Set master limiter']);
	});
});

describe('the duck mode (browser harness)', () => {
	test('a depth is the speech gate, clamped to -40..-1, and none is the compressor', async () => {
		const gate = await setMasterDuck(-12);
		expect(gate.master).toEqual({ volume: 1, limiter: false, ceiling_db: -1.5, duck_depth_db: -12 });
		expect((await setMasterDuck(-90)).master?.duck_depth_db).toBe(-40);
		expect((await setMasterDuck(5)).master?.duck_depth_db).toBe(-1);
		// Back to the compressor the key is gone, like the saved file — and the master with it.
		const back = await setMasterDuck(null);
		expect(back.master).toBeUndefined();
		expect((await setMasterDuck(-8)).master?.duck_depth_db).toBe(-8);
		expect((await setMasterDuck()).master).toBeUndefined();
	});

	test('survives the other master moves, and does not make them stick around', async () => {
		await setMasterDuck(-9);
		expect((await setMasterVolume(0.5)).master).toEqual({ volume: 0.5, limiter: false, ceiling_db: -1.5, duck_depth_db: -9 });
		expect((await setMasterLimiter(true)).master?.duck_depth_db).toBe(-9);
		await setMasterLimiter(false);
		await setMasterVolume(1);
		expect((await setMasterDuck(null)).master).toBeUndefined();
	});

	test('refuses what is not a number, and is one revision labelled as the core labels it', async () => {
		await expect(setMasterDuck(Number.NaN)).rejects.toThrow('number');
		await expect(setMasterDuck(Number.NEGATIVE_INFINITY)).rejects.toThrow('number');
		const head = (await getHistory()).find((h) => h.current)!.seq;
		await setMasterDuck(-12);
		await setMasterDuck(null);
		const history = await getHistory();
		expect(history.find((h) => h.current)!.seq).toBe(head + 2);
		expect(history.map((h) => h.label).slice(-2)).toEqual(['Duck by -12 dB under speech', 'Duck with the compressor']);
	});
});

describe('get_levels (browser harness)', () => {
	test('is an estimate, says so, and reads through the faders and the master', async () => {
		const base = await getLevels();
		expect(base.estimated).toBe(true);
		// The sample cut has the interview's sound twice — on V1 (the picture clip carries
		// it) and on A1 — and the sample analysis says -16.2 LUFS / -1.5 dBTP for it, so
		// each strip reads that and the sum is 3 dB more.
		expect(base.tracks.map((t) => t.name)).toEqual(['V1', 'A1']);
		expect(base.tracks[1].level?.integrated_lufs).toBeCloseTo(-16.2, 1);
		expect(base.master?.integrated_lufs).toBeCloseTo(-13.19, 1);
		expect(base.master?.true_peak_dbtp).toBeCloseTo(1.51, 1);
		expect(base.target_lufs).toBe(-14);
		expect(base.notes.some((n) => n.includes('set_master_limiter'))).toBe(true);

		await setTrackVolume('a1', 0.5);
		await setMasterVolume(0.5);
		const quieter = await getLevels();
		expect(quieter.tracks[1].level?.integrated_lufs).toBeCloseTo(-22.2, 1);
		// V1 -16.2 and A1 -22.2 sum to -15.2, then the master fader's -6.02.
		expect(quieter.master?.integrated_lufs).toBeCloseTo(-21.25, 1);
	});

	test('a range past the end of the cut is refused, not widened', async () => {
		await expect(getLevels({ start: 5000, end: 5010 })).rejects.toThrow('only');
	});

	test('a range shortens what is measured, and loudnorm lands on the target', async () => {
		const ranged = await getLevels({ start: 0, end: 2 });
		expect(ranged.duration).toBe(2);
		expect(ranged.master?.short_term_max_lufs).toBeNull();
		const norm = await getLevels(null, true);
		expect(norm.loudnorm).toBe(true);
		expect(norm.master?.integrated_lufs).toBe(-14);
	});

	test('there is nothing to stop in the browser, and asking is not an error', async () => {
		await expect(cancelLevels()).resolves.toBeUndefined();
	});
});
