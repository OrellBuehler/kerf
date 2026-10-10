import { describe, expect, test } from 'bun:test';
import { planMusicFit } from './music-fit';
import { barLength, describeFit, describeRemainder, fitOffer, fitVerdict, musicSummary, parseTarget } from './music-ui';
import { sampleMusic } from './sample-music';
import type { Clip, Track } from './types';

const clip = (extra: Partial<Clip> = {}): Clip => ({
	id: 'c',
	asset_id: 'music',
	source_in: 0,
	source_out: 30,
	timeline_start: 0,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});
const track = (extra: Partial<Track> = {}): Track => ({ id: 't', kind: 'audio', name: 'A2', clips: [], ...extra });
const analysis = { music: sampleMusic() };

describe('fitOffer', () => {
	test('is offered to a clip on an audio track whose asset has a music analysis', () => {
		expect(fitOffer(track(), clip(), analysis)).toEqual({ show: true, reason: null });
	});

	test('is not offered to speech, to a video track or to a clip nobody analyzed', () => {
		expect(fitOffer(track(), clip(), { music: null }).show).toBe(false);
		expect(fitOffer(track(), clip(), undefined).show).toBe(false);
		expect(fitOffer(track(), clip(), null).show).toBe(false);
		expect(fitOffer(track({ kind: 'video' }), clip(), analysis).show).toBe(false);
		expect(fitOffer(undefined, clip(), analysis).show).toBe(false);
	});

	test('says why it cannot be used, with the backend refusals', () => {
		expect(fitOffer(track({ locked: true }), clip(), analysis)).toEqual({ show: true, reason: 'Track A2 is locked' });
		expect(fitOffer(track(), clip({ link_id: 'pair' }), analysis).reason).toContain('Unlink the music');
		expect(fitOffer(track(), clip({ speed: 2 }), analysis).reason).toContain('normal speed');
		expect(fitOffer(track(), clip({ speed: -1 }), analysis).reason).toContain('normal speed');
		expect(fitOffer(track(), clip({ speed: 1 }), analysis).reason).toBeNull();
	});
});

describe('the words', () => {
	test('the analysis in one line', () => {
		const m = sampleMusic();
		expect(musicSummary(m)).toBe('120 BPM · 4/4 · 29 bars');
		expect(barLength(m)).toBe('2.0 s');
		expect(musicSummary({ ...m, grid: { ...m.grid, period_s: 60 / 93.5 }, bar_chroma: [m.bar_chroma[0]] })).toBe('93.5 BPM · 4/4 · 1 bar');
	});

	test('a remainder reads as exact, short or over', () => {
		expect(fitVerdict({ remainder: 0 })).toBe('exact');
		expect(fitVerdict({ remainder: 0.0001 })).toBe('exact');
		expect(fitVerdict({ remainder: 0.5 })).toBe('short');
		expect(fitVerdict({ remainder: -6 })).toBe('over');
		expect(describeRemainder({ remainder: 0 }, true)).toBe('Lands exactly on the target');
		expect(describeRemainder({ remainder: 0.5 }, true)).toContain('0.5 s short');
		expect(describeRemainder({ remainder: -6 }, true)).toContain('6.0 s over — cut at the target and faded out');
		expect(describeRemainder({ remainder: -6 }, false)).toContain('ending included');
	});

	test('the toast names the length and the splices', () => {
		const fit = planMusicFit(sampleMusic(), 30, 44100);
		expect(describeFit({ duration: 50, faded: true, fit: { ...fit, splices: 1 } })).toBe(
			'Music fitted to 0:50.0 · 1 splice · faded out'
		);
		expect(describeFit({ duration: 34.5, faded: false, fit: { ...fit, splices: 0 } })).toBe('Music fitted to 0:34.5 · no splices');
		expect(describeFit({ duration: 90, faded: false, fit: { ...fit, splices: 3 } })).toContain('3 splices');
	});

	test('a custom length is a positive number of seconds', () => {
		expect(parseTarget('30')).toBe(30);
		expect(parseTarget(' 12,5 ')).toBe(12.5);
		expect(parseTarget('')).toBeNull();
		expect(parseTarget('0')).toBeNull();
		expect(parseTarget('-4')).toBeNull();
		expect(parseTarget('abc')).toBeNull();
		expect(parseTarget('1e400')).toBeNull();
	});
});
