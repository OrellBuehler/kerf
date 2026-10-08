import { describe, expect, test } from 'bun:test';
import { clock, fmtNumber, isStale, measureRange, noteTone, readingText, scopeLabel, verdict } from './levels-view';
import { levelNotes } from './levels';
import type { LevelReading } from './types';

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
