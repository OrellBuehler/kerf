import { describe, expect, test } from 'bun:test';
import { LUFS_TO_RMS_DB, synthPcm } from './sample-audio';

const rmsDb = (pcm: Int16Array) => {
	let sum = 0;
	for (const s of pcm) sum += (s / 32768) ** 2;
	return 10 * Math.log10(sum / pcm.length);
};
const peakDb = (pcm: Int16Array) => 20 * Math.log10(Math.max(...Array.from(pcm, Math.abs)) / 32768);

describe('the harness’s synthetic audio', () => {
	test('is as long as asked, at the rate asked', () => {
		expect(synthPcm('a', 0, 1, 32000).length).toBe(32000);
		expect(synthPcm('a', 5, 0.5, 8000).length).toBe(4000);
		expect(synthPcm('a', 0, 0, 32000).length).toBe(0);
		expect(synthPcm('a', 0, -1, 32000).length).toBe(0);
	});

	test('is a function of source time: windows join, and a seek hears the same sound', () => {
		const whole = synthPcm('a', 10, 2, 8000);
		const first = synthPcm('a', 10, 1, 8000);
		const second = synthPcm('a', 11, 1, 8000);
		expect(Array.from(first)).toEqual(Array.from(whole.slice(0, 8000)));
		expect(Array.from(second)).toEqual(Array.from(whole.slice(8000)));
		// Deterministic: the same call, the same samples.
		expect(Array.from(synthPcm('a', 10, 2, 8000))).toEqual(Array.from(whole));
	});

	test('differs per asset', () => {
		const a = synthPcm('interview', 0, 1, 8000);
		const b = synthPcm('music', 0, 1, 8000);
		expect(Array.from(a)).not.toEqual(Array.from(b));
	});

	test('measures the loudness the analysis gave it, as dual mono', () => {
		for (const lufs of [-16.2, -23, -11.8]) {
			// A whole number of phrases (6.5 s each) so the window is the calibration's.
			const pcm = synthPcm('interview', 0, 26, 8000, lufs);
			expect(rmsDb(pcm)).toBeCloseTo(lufs + LUFS_TO_RMS_DB, 0);
		}
	});

	test('has a voice’s crest factor and stays inside full scale', () => {
		const pcm = synthPcm('interview', 0, 26, 8000, -16.2);
		const crest = peakDb(pcm) - rmsDb(pcm);
		expect(crest).toBeGreaterThan(6);
		expect(crest).toBeLessThan(20);
		expect(peakDb(pcm)).toBeLessThanOrEqual(0);
		for (const s of pcm) {
			expect(s).toBeLessThanOrEqual(32767);
			expect(s).toBeGreaterThanOrEqual(-32767);
		}
	});

	test('rests between syllables — it is not a steady tone', () => {
		const pcm = synthPcm('interview', 0, 4, 8000, -16.2);
		const block = 400; // 50 ms
		const levels: number[] = [];
		for (let i = 0; i + block <= pcm.length; i += block) levels.push(rmsDb(pcm.slice(i, i + block)));
		expect(Math.max(...levels) - Math.min(...levels)).toBeGreaterThan(10);
	});
});
