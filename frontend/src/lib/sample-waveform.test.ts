import { describe, expect, test } from 'bun:test';
import { clippedSpan, SAMPLE_MAX_BUCKETS, synthWaveformRange, type SampleAudio } from './sample-waveform';

const stereo: SampleAudio = { id: 'interview', duration: 120, channels: 2 };
const mono: SampleAudio = { id: 'voiceover', duration: 30, channels: 1 };

/** Every value of a range, lanes and polarities flattened. */
const everything = (r: { min: number[][]; max: number[][] }) => [...r.min.flat(), ...r.max.flat()];

describe('shape', () => {
	test('a stereo file is two lanes of `buckets` min / max pairs in -1..1', () => {
		const r = synthWaveformRange(stereo, 0, 12, 300);
		expect(r.channels).toBe(2);
		expect(r.buckets).toBe(300);
		expect(r.duration).toBe(120);
		expect(r.min).toHaveLength(2);
		expect(r.max).toHaveLength(2);
		for (const lane of [...r.min, ...r.max]) expect(lane).toHaveLength(300);
		for (let c = 0; c < 2; c++) {
			for (let j = 0; j < 300; j++) {
				expect(r.min[c][j]).toBeLessThanOrEqual(0);
				expect(r.max[c][j]).toBeGreaterThanOrEqual(0);
				expect(r.min[c][j]).toBeGreaterThanOrEqual(-1);
				expect(r.max[c][j]).toBeLessThanOrEqual(1);
			}
		}
	});

	test('mono is one lane', () => {
		const r = synthWaveformRange(mono, 0, 10, 100);
		expect(r.channels).toBe(1);
		expect(r.min).toHaveLength(1);
		expect(r.max).toHaveLength(1);
		expect(r.max[0].some((v) => v > 0.05)).toBe(true);
	});

	test('anything but mono is drawn as the engine draws it: two lanes', () => {
		expect(synthWaveformRange({ ...stereo, channels: 6 }, 0, 1, 10).channels).toBe(2);
		expect(synthWaveformRange({ ...stereo, channels: 2 }, 0, 1, 10).channels).toBe(2);
	});

	test('stereo lanes are alike but not identical', () => {
		const r = synthWaveformRange(stereo, 0, 12, 300);
		expect(r.max[0]).not.toEqual(r.max[1]);
	});

	test('values are rounded to four places and never -0', () => {
		const r = synthWaveformRange(stereo, 0, 12, 300);
		for (const v of everything(r)) {
			expect(Math.abs(v * 10_000 - Math.round(v * 10_000))).toBeLessThan(1e-6);
			expect(Object.is(v, -0)).toBe(false);
		}
	});
});

describe('determinism', () => {
	test('the same asset and window always read the same', () => {
		expect(synthWaveformRange(stereo, 3, 17, 640)).toEqual(synthWaveformRange(stereo, 3, 17, 640));
	});

	test('different assets do not look alike', () => {
		const other = { ...stereo, id: 'broll' };
		expect(synthWaveformRange(other, 0, 12, 300).max).not.toEqual(synthWaveformRange(stereo, 0, 12, 300).max);
	});

	test('windows over the same footage agree about the audio they share', () => {
		// 500 columns over 10 s: 50 per second, read from the 100/s level, so a
		// window shifted by one column (20 ms) is the same picture one column over.
		const a = synthWaveformRange(stereo, 10, 20, 500);
		const b = synthWaveformRange(stereo, 10.02, 20.02, 500);
		expect(a.peaks_per_second).toBe(100);
		for (let c = 0; c < 2; c++) {
			for (let j = 0; j < 499; j++) {
				expect(b.min[c][j]).toBe(a.min[c][j + 1]);
				expect(b.max[c][j]).toBe(a.max[c][j + 1]);
			}
		}
	});
});

describe('the window', () => {
	test('outside the media reads exactly zero', () => {
		const before = synthWaveformRange(stereo, -5, 0, 50);
		const after = synthWaveformRange(stereo, 120, 130, 50);
		const past = synthWaveformRange(stereo, 500, 510, 50);
		for (const r of [before, after, past]) {
			expect(r.buckets).toBe(50);
			expect(everything(r).every((v) => v === 0)).toBe(true);
		}
	});

	test('a window straddling the start is silent only where it hangs over', () => {
		// Forty columns of 0.1 s: the first twenty are before the file begins.
		const r = synthWaveformRange(stereo, -2, 2, 40);
		expect(r.max[0].slice(0, 20).every((v) => v === 0)).toBe(true);
		expect(r.min[0].slice(0, 20).every((v) => v === 0)).toBe(true);
		expect(r.max[0].slice(20).filter((v) => v > 0).length).toBe(20);
	});

	test('a window straddling the end is silent only past it', () => {
		// Forty columns of 0.1 s: the audio ends at the twentieth.
		const r = synthWaveformRange({ ...stereo, duration: 10 }, 8, 12, 40);
		expect(r.max[0].slice(0, 20).filter((v) => v > 0).length).toBe(20);
		expect(r.max[0].slice(20).every((v) => v === 0)).toBe(true);
		expect(r.min[0].slice(20).every((v) => v === 0)).toBe(true);
	});

	test('buckets are capped, rounded, and zero gives empty lanes', () => {
		expect(synthWaveformRange(stereo, 0, 60, 100_000).buckets).toBe(SAMPLE_MAX_BUCKETS);
		expect(synthWaveformRange(stereo, 0, 60, 100_000).min[0]).toHaveLength(SAMPLE_MAX_BUCKETS);
		expect(synthWaveformRange(stereo, 0, 60, 12.6).buckets).toBe(13);
		const none = synthWaveformRange(stereo, 0, 60, 0);
		expect(none.buckets).toBe(0);
		expect(none.min).toEqual([[], []]);
		expect(synthWaveformRange(stereo, 0, 60, NaN).buckets).toBe(0);
		expect(synthWaveformRange(stereo, 0, 60, -4).buckets).toBe(0);
	});

	test('an empty, inverted or non-finite window is `buckets` silent ones', () => {
		for (const [start, end] of [
			[5, 5],
			[9, 2],
			[NaN, 4],
			[0, Infinity]
		]) {
			const r = synthWaveformRange(stereo, start, end, 30);
			expect(r.buckets).toBe(30);
			expect(everything(r).every((v) => v === 0)).toBe(true);
		}
	});

	test('the level is the coarsest that still has a source bucket per column', () => {
		const rate = (start: number, end: number, buckets: number) =>
			synthWaveformRange(stereo, start, end, buckets).peaks_per_second;
		expect(rate(0, 120, 200)).toBe(10);
		expect(rate(0, 120, 3000)).toBe(25);
		expect(rate(0, 10, 500)).toBe(100);
		expect(rate(0, 10, 3000)).toBe(500);
		// Finer than anything stored: the finest level, neighbours repeat.
		expect(rate(0, 1, 4096)).toBe(500);
	});

	test('every bucket of a very wide window is still read', () => {
		const r = synthWaveformRange({ ...stereo, duration: 7200 }, 0, 7200, SAMPLE_MAX_BUCKETS);
		expect(r.buckets).toBe(SAMPLE_MAX_BUCKETS);
		expect(r.max[0].filter((v) => v > 0).length).toBe(SAMPLE_MAX_BUCKETS);
	});
});

describe('what the harness shows', () => {
	test('a stretch clips: exactly full scale, and nowhere else', () => {
		const clip = clippedSpan(stereo.duration);
		const buckets = 1000;
		const r = synthWaveformRange(stereo, 0, 120, buckets);
		const width = 120 / buckets;
		const clipped = (j: number) => r.max[0][j] === 1 || r.min[0][j] === -1;
		const hit = [...Array(buckets).keys()].filter(clipped);
		expect(hit.length).toBeGreaterThan(5);
		for (const j of hit) {
			expect(j * width).toBeGreaterThanOrEqual(clip.start - width);
			expect(j * width).toBeLessThan(clip.end);
		}
		// Nothing well away from it comes near the rails.
		for (let j = 0; j < buckets; j++) {
			const t = j * width;
			if (t < clip.start - 2 * width || t > clip.end + 2 * width) {
				expect(r.max[0][j]).toBeLessThan(0.9);
				expect(r.min[0][j]).toBeGreaterThan(-0.9);
			}
		}
		// Both lanes clip, and both polarities.
		expect(r.max[1].some((v) => v === 1)).toBe(true);
		expect(r.min[1].some((v) => v === -1)).toBe(true);
	});

	test('zoomed in on the clip, the plateau is flat at full scale', () => {
		const clip = clippedSpan(stereo.duration);
		const r = synthWaveformRange(stereo, clip.start + 0.1, clip.end - 0.1, 200);
		const rails = r.max[0].filter((v) => v === 1).length;
		expect(rails).toBeGreaterThan(150);
	});

	test('analysis silence reads as a quiet floor, not as outside-the-media zeros', () => {
		const quiet: SampleAudio = { ...stereo, silence: [{ start: 5, end: 8 }] };
		const inside = synthWaveformRange(quiet, 5.5, 7.5, 40);
		const outside = synthWaveformRange(quiet, 8.5, 10.5, 40);
		expect(inside.max[0].every((v) => v > 0 && v < 0.02)).toBe(true);
		expect(Math.max(...outside.max[0])).toBeGreaterThan(0.1);
	});

	test('music pulses on the beat', () => {
		const music: SampleAudio = { id: 'bed', duration: 60, channels: 2, kind: 'music', bpm: 120 };
		// Two beats a second: a column right after a beat is louder than one just before the next.
		const r = synthWaveformRange(music, 20, 24, 400);
		const onBeat = r.max[0].filter((_, j) => j % 50 < 3);
		const offBeat = r.max[0].filter((_, j) => j % 50 >= 45);
		const mean = (xs: number[]) => xs.reduce((a, b) => a + b, 0) / xs.length;
		expect(mean(onBeat)).toBeGreaterThan(mean(offBeat));
	});
});
