import { describe, expect, test } from 'bun:test';
import { clock, deliveryCheck, deliveryTarget, fmtNumber, isStale, measureRange, noteTone, readingText, scopeLabel, verdict } from './levels-view';
import { levelNotes } from './levels';
import { formatTime } from './time-format';
import type { LevelReading, MasterBus } from './types';

const reading = (over: Partial<LevelReading> = {}): LevelReading => ({
	integrated_lufs: -14.23,
	loudness_range_lu: 5.2,
	short_term_max_lufs: -11.84,
	peak_dbfs: -2.1,
	true_peak_dbtp: -1.06,
	...over
});

describe('numbers', () => {
	test('one decimal, silence as a dash, never a negative zero', () => {
		expect(fmtNumber(-14.23)).toBe('-14.2');
		expect(fmtNumber(0)).toBe('0.0');
		expect(fmtNumber(-0.04)).toBe('0.0');
		expect(fmtNumber(1.06)).toBe('1.1');
		expect(fmtNumber(null)).toBe('−∞');
		expect(fmtNumber(undefined)).toBe('−∞');
		expect(fmtNumber(-Infinity)).toBe('−∞');
		expect(fmtNumber(5.234, 2)).toBe('5.23');
	});

	test('a reading as the words a strip shows', () => {
		expect(readingText(reading())).toEqual({ lufs: '-14.2', truePeak: '-1.1', range: '5.2', shortTerm: '-11.8' });
		// A span under three seconds has no short-term maximum; a silent mix has nothing.
		expect(readingText(reading({ short_term_max_lufs: null })).shortTerm).toBeNull();
		expect(readingText(reading({ integrated_lufs: null, true_peak_dbtp: null, loudness_range_lu: null }))).toEqual({
			lufs: '−∞',
			truePeak: '−∞',
			range: '—',
			shortTerm: '-11.8'
		});
		expect(readingText(null)).toEqual({ lufs: '−∞', truePeak: '−∞', range: '—', shortTerm: null });
	});
});

describe('verdict', () => {
	const v = (i: number | null) =>
		verdict({ master: reading({ integrated_lufs: i }), target_lufs: -14 });

	test('says how far over or under the target the mix is', () => {
		expect(v(-9.8)).toEqual({ text: '-9.8 LUFS — 4.2 LU louder than the -14 target', tone: 'warn' });
		expect(v(-19)).toEqual({ text: '-19.0 LUFS — 5.0 LU quieter than the -14 target', tone: 'warn' });
		expect(v(-14.5)).toEqual({ text: '-14.5 LUFS — on the -14 target', tone: 'ok' });
	});

	test('uses the notes’ thresholds: over by 1 LU is fine, under by 3 is fine', () => {
		expect(v(-13).tone).toBe('ok');
		expect(v(-12.9).tone).toBe('warn');
		expect(v(-17).tone).toBe('ok');
		expect(v(-17.1).tone).toBe('warn');
	});

	test('a silent mix and a cut with no audio are not a loudness problem', () => {
		expect(v(null)).toEqual({ text: 'The mix is silent', tone: 'info' });
		expect(verdict({ master: null, target_lufs: -14 })).toEqual({ text: 'No audio in the cut', tone: 'info' });
	});
});

describe('noteTone', () => {
	test('reads the engine’s own words (the TS mirror of the Rust notes)', () => {
		const tone = (i: number | null, tp: number | null) => levelNotes(reading({ integrated_lufs: i, true_peak_dbtp: tp }), []).map(noteTone);
		expect(tone(-14, -3)).toEqual(['ok']);
		expect(tone(-9, -3)).toEqual(['warn']);
		expect(tone(-19, -3)).toEqual(['warn']);
		expect(tone(-14, 0.4)).toEqual(['ok', 'warn']);
		expect(tone(null, null)).toEqual(['info']);
		expect(levelNotes(null, []).map(noteTone)).toEqual(['info']);
	});
});

describe('the range measured', () => {
	test('is the marks when both are set and in order, otherwise the whole cut', () => {
		expect(measureRange(null, null)).toBeNull();
		expect(measureRange(2, null)).toBeNull();
		expect(measureRange(null, 9)).toBeNull();
		expect(measureRange(2, 9)).toEqual({ start: 2, end: 9 });
		// The export dialog's rule: a mark that crosses its partner is no range.
		expect(measureRange(9, 2)).toBeNull();
		expect(measureRange(4, 4)).toBeNull();
	});

	test('is named in words', () => {
		expect(scopeLabel(null, 120)).toBe('Whole cut · 2:00');
		expect(scopeLabel({ start: 10, end: 25 }, 120)).toBe('In → out · 0:10–0:25');
		expect(clock(65.4)).toBe('1:05');
		expect(clock(-3)).toBe('0:00');
	});

	test('prints its times the way the interface does — the marks are positions on the playhead’s timeline', () => {
		const timecode = (s: number) => formatTime(s, 30, 'timecode');
		expect(scopeLabel({ start: 10, end: 25 }, 120, timecode)).toBe('In → out · 00:00:10:00–00:00:25:00');
		expect(scopeLabel(null, 120, timecode)).toBe('Whole cut · 00:02:00:00');
		const seconds = (s: number) => formatTime(s, 30, 'seconds');
		expect(scopeLabel({ start: 10, end: 25 }, 120, seconds)).toBe('In → out · 0:10.0–0:25.0');
	});
});

describe('staleness', () => {
	test('any new revision, or another project, outdates a measurement', () => {
		const at = { seq: 4, path: '/a.kerf' };
		expect(isStale(at, { seq: 4, path: '/a.kerf' })).toBe(false);
		expect(isStale(at, { seq: 5, path: '/a.kerf' })).toBe(true);
		expect(isStale(at, { seq: 3, path: '/a.kerf' })).toBe(true); // an undo
		expect(isStale(at, { seq: 4, path: '/b.kerf' })).toBe(true);
		expect(isStale({ seq: null, path: null }, { seq: null, path: null })).toBe(false);
	});
});

describe('the cut against what a delivery is held to', () => {
	const master = (i: number | null, tp: number | null) => reading({ integrated_lufs: i, true_peak_dbtp: tp });
	const check = (i: number | null, tp: number | null, bus: Partial<MasterBus> = {}) =>
		deliveryCheck({ master: master(i, tp), target_lufs: -14 }, { limiter: false, ceiling_db: -1.5, true_peak: false, ...bus });

	test('on target and under the ceiling is two rows with nothing to do', () => {
		const rows = check(-14.2, -2.1)!;
		expect(rows.map((r) => r.id)).toEqual(['loudness', 'true_peak']);
		expect(rows[0]).toMatchObject({ value: '-14.2 LUFS', target: 'streaming target -14 LUFS', delta: 'on target', tone: 'ok', advice: null });
		expect(rows[1]).toMatchObject({ value: '-2.1 dBTP', target: 'ceiling -1 dBTP', delta: 'under the ceiling', tone: 'ok', advice: null });
	});

	test('the notes’ thresholds: more than 1 LU over is loud, more than 3 under is quiet, and a LU inside them is fine', () => {
		expect(check(-13.1, -2)![0]).toMatchObject({ delta: 'on target', tone: 'ok' });
		expect(check(-12.9, -2)![0]).toMatchObject({ delta: '1.1 LU over', tone: 'warn' });
		expect(check(-17.0, -2)![0]).toMatchObject({ delta: 'on target', tone: 'ok' });
		expect(check(-17.4, -2)![0]).toMatchObject({ delta: '3.4 LU under', tone: 'warn' });
	});

	test('says what to do about loudness, in dB of the master', () => {
		expect(check(-11.6, -2)![0].advice).toBe('Lower the master by 2.4 dB, or export with a loudness target.');
		expect(check(-19.0, -2)![0].advice).toBe('Raise the master by 5.0 dB, or export with a loudness target.');
	});

	test('a true peak over -1 dBTP is over, and the advice names the control the engine’s notes do', () => {
		expect(check(-14, -0.4)![1]).toMatchObject({ delta: '0.6 dB over', tone: 'warn', advice: 'Turn on the Limiter with True peak (FX), or lower the master.' });
		// A sample-peak limiter holds the sample peak: true-peak limiting, or a ceiling taken down by the overshoot and half a dB.
		expect(check(-14, -0.4, { limiter: true, ceiling_db: -1.5 })![1].advice).toBe('Turn on True peak (FX), or lower the ceiling to about -2.6 dBFS.');
		// A true-peak limiter already holds it: only the ceiling is left.
		expect(check(-14, -0.4, { limiter: true, true_peak: true, ceiling_db: -1.5 })![1].advice).toBe("Lower the limiter's ceiling to about -2.6 dBFS (FX).");
		// At the lowest ceiling there is nowhere further to go.
		expect(check(-14, 3, { limiter: true, true_peak: true, ceiling_db: -24 })![1].advice).toBe('The limiter is at its lowest ceiling: lower the master or the loudest track.');
		expect(check(-14, 3, { limiter: true, true_peak: false, ceiling_db: -24 })![1].advice).toBe('Turn on True peak (FX), or lower the master or the loudest track.');
	});

	test('agrees with the engine’s own notes about when it is over', () => {
		for (const [i, tp] of [[-14, -0.4], [-11, -3], [-20, -1], [-14, -1.05]] as const) {
			const notes = levelNotes(reading({ integrated_lufs: i, true_peak_dbtp: tp }), []);
			const rows = check(i, tp)!;
			expect(rows[1].tone === 'warn', `${i} ${tp} true peak`).toBe(notes.some((n) => /^True peak/.test(n)));
			expect(rows[0].tone === 'warn', `${i} ${tp} loudness`).toBe(!notes[0].includes('is close to'));
		}
	});

	test('nothing to judge: no audio, a silent mix', () => {
		expect(deliveryCheck({ master: null, target_lufs: -14 }, { limiter: false, ceiling_db: -1.5 })).toBeNull();
		expect(check(null, null)).toBeNull();
		// Loudness but no true peak yet: the one row.
		expect(check(-14, null)!.map((r) => r.id)).toEqual(['loudness']);
	});
});

describe('which loudness the cut is held to', () => {
	test('the streaming target the engine measures by, unless Export was set to another', () => {
		expect(deliveryTarget(null, -14)).toEqual({ lufs: -14, name: 'the streaming target', chosen: false });
		expect(deliveryTarget({}, -14)).toEqual({ lufs: -14, name: 'the streaming target', chosen: false });
		expect(deliveryTarget({ loudness: 'off' as never }, -14).chosen).toBe(false);
	});

	test('a target chosen in the Export dialog is read from its remembered options, named, with every target asking -1 dBTP', () => {
		expect(deliveryTarget({ loudness: 'apple' }, -14)).toEqual({ lufs: -16, name: 'the Apple target set in Export', chosen: true });
		expect(deliveryTarget({ loudness: 'broadcast' }, -14)).toEqual({ lufs: -23, name: 'the Broadcast (EBU R128) target set in Export', chosen: true });
		// The old loudnorm flag is the YouTube target, as the engine reads it.
		expect(deliveryTarget({ loudnorm: true }, -14)).toEqual({ lufs: -14, name: 'the YouTube target set in Export', chosen: true });
	});

	test('the check is held to the chosen target, and says so in its line and its advice', () => {
		const levels = { master: reading({ integrated_lufs: -20.5, true_peak_dbtp: -3 }), target_lufs: -14 };
		const bus = { limiter: false, ceiling_db: -1.5, true_peak: false };
		const streaming = deliveryCheck(levels, bus)!;
		expect(streaming[0]).toMatchObject({ delta: '6.5 LU under', target: 'streaming target -14 LUFS' });
		expect(streaming[0].advice).toBe('Raise the master by 6.5 dB, or export with a loudness target.');
		const apple = deliveryCheck(levels, bus, deliveryTarget({ loudness: 'apple' }))!;
		expect(apple[0]).toMatchObject({ delta: '4.5 LU under', target: 'target -16 LUFS', tone: 'warn' });
		expect(apple[0].advice).toBe('Export evens the file out to -16 LUFS; to hear it there, raise the master by 4.5 dB.');
		const onTarget = deliveryCheck({ master: reading({ integrated_lufs: -23.4, true_peak_dbtp: -3 }), target_lufs: -14 }, bus, deliveryTarget({ loudness: 'broadcast' }))!;
		expect(onTarget[0]).toMatchObject({ delta: 'on target', tone: 'ok', advice: null });
		// The true peak line is the same for every target.
		expect(apple[1].target).toBe('ceiling -1 dBTP');
	});
});
