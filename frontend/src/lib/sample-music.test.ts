import { describe, expect, test } from 'bun:test';
import { barStart, firstDownbeat, gridBarSeconds, planMusicFit, wholeBars } from './music-fit';
import { sampleMusic, sampleMusicAnalysis, SAMPLE_MUSIC_DURATION, SAMPLE_MUSIC_GRID } from './sample-music';

describe('the harness song', () => {
	const m = sampleMusic();

	test('is a bar grid with one chroma per whole bar', () => {
		expect(m.bar_chroma.length).toBe(wholeBars(SAMPLE_MUSIC_GRID, SAMPLE_MUSIC_DURATION));
		expect(m.bar_chroma.length).toBe(29);
		for (const c of m.bar_chroma) {
			expect(c.length).toBe(12);
			expect(Math.sqrt(c.reduce((s, x) => s + x * x, 0))).toBeCloseTo(1, 5);
		}
	});

	test('repeats its loop and not its bridge', () => {
		expect(m.phrases).toContainEqual({ a: 0, b: 8, bars: 8 });
		expect(m.phrases).toContainEqual({ a: 4, b: 12, bars: 4 });
		expect(m.phrases).toContainEqual({ a: 0, b: 20, bars: 4 });
		// Nothing that covers bars 16-19 is repeated anywhere.
		expect(m.phrases.some((p) => [p.a, p.b].some((s) => s < 20 && s + p.bars > 16))).toBe(false);
	});

	test('has a tempo whose beats and bars are the grid', () => {
		const a = sampleMusicAnalysis('music');
		expect(a.tempo?.bpm).toBe(120);
		expect(a.tempo?.downbeats[0]).toBeCloseTo(firstDownbeat(SAMPLE_MUSIC_GRID), 9);
		// 29 whole bars and the start of the ending's.
		expect(a.tempo?.downbeats.length).toBe(30);
		expect(a.audio_class?.class).toBe('music');
		expect(a.music).toEqual(m);
	});

	test('fits the harness picture exactly, and shows short and over for other lengths', () => {
		// The picture is 20.5 s: intro 0.52 + nine bars + ending 1.98.
		const fit = planMusicFit(m, 20.5, 44100);
		expect(fit.bars).toBe(9);
		expect(fit.splices).toBeGreaterThan(0);
		expect(Math.abs(fit.remainder)).toBeLessThan(1e-4);
		expect(fit.segments[0].source_start).toBe(0);
		expect(fit.segments[fit.segments.length - 1].source_end).toBe(SAMPLE_MUSIC_DURATION);
		expect(barStart(m.grid, 0)).toBeCloseTo(0.52, 9);
		expect(gridBarSeconds(m.grid)).toBe(2);
		expect(planMusicFit(m, 22, 44100).remainder).toBeGreaterThan(1);
		expect(planMusicFit(m, 19.5, 44100).remainder).toBeLessThan(-0.5);
	});
});
