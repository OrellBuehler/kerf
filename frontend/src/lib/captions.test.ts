import { describe, expect, test } from 'bun:test';
import {
	captionsForTimeline,
	chunkWords,
	coversSource,
	sourceToTimeline,
	timeChunks,
	resolveCaptions,
	CAPTION_DEFAULTS,
	CAPTION_STYLES,
	MIN_CAPTION,
	MIN_WORD_CAPTION
} from './captions';
import type { Clip, Timeline, TranscriptSegment } from './types';

const clip = (over: Partial<Clip> = {}): Clip =>
	({
		id: 'c1',
		asset_id: 'a1',
		source_in: 0,
		source_out: 4,
		timeline_start: 0,
		volume: 1,
		speed: 1,
		...over
	}) as Clip;

const timelineOf = (clips: Clip[]): Timeline => ({
	tracks: [{ id: 't1', kind: 'video', name: 'V1', clips }],
	overlays: [],
	markers: []
});

const seg = (start: number, end: number, text: string): TranscriptSegment => ({ start, end, text });

describe('projection', () => {
	test('maps through trim, speed and reverse', () => {
		const c = clip({ source_in: 10, source_out: 20, timeline_start: 4 });
		expect(sourceToTimeline(c, 12)).toBeCloseTo(6, 9);
		expect(sourceToTimeline({ ...c, speed: 2 }, 12)).toBeCloseTo(5, 9);
		// Reversed: the source's tail is heard first.
		expect(sourceToTimeline({ ...c, speed: -1 }, 12)).toBeCloseTo(12, 9);
	});

	test('covers_source is the clip window', () => {
		const c = clip({ source_in: 10, source_out: 20 });
		expect(coversSource(c, 12, 14)).toBe(true);
		expect(coversSource(c, 8, 11)).toBe(true);
		expect(coversSource(c, 0, 10)).toBe(false);
		expect(coversSource(c, 20, 25)).toBe(false);
	});
});

describe('chunking', () => {
	test('splits to the word and character limits', () => {
		const lines = chunkWords('Today we are talking about non-destructive editing in Kerf', CAPTION_DEFAULTS);
		expect(lines.length).toBeGreaterThan(1);
		for (const l of lines) {
			expect(l.split(' ').length).toBeLessThanOrEqual(CAPTION_DEFAULTS.max_words);
		}
		expect(lines.join(' ')).toBe('Today we are talking about non-destructive editing in Kerf');
	});

	test('a word longer than the limit is its own line, not cut in half', () => {
		const lines = chunkWords('supercalifragilisticexpialidocious ok', { ...CAPTION_DEFAULTS, max_chars: 10 });
		expect(lines[0]).toBe('supercalifragilisticexpialidocious');
	});

	test('lines too short to read merge instead of flickering', () => {
		const lines = timeChunks(chunkWords('a b c d e f g h', CAPTION_DEFAULTS), 0, 0.9);
		for (const l of lines) {
			if (lines.length > 1) expect(l.end - l.start).toBeGreaterThanOrEqual(MIN_CAPTION - 1e-6);
		}
		expect(lines.map((l) => l.text).join(' ')).toBe('a b c d e f g h');
	});
});

describe('captions follow the cut', () => {
	test('a trimmed clip captions in timeline time', () => {
		const out = captionsForTimeline(timelineOf([clip({ source_in: 30, source_out: 34 })]), {
			a1: [seg(30, 34, 'one two three four')]
		});
		expect(out).toHaveLength(1);
		expect(out[0].start).toBeCloseTo(0, 9);
		expect(out[0].end).toBeCloseTo(4, 9);
		expect(out[0].generated).toBe(true);
	});

	test('words cut out get no caption', () => {
		const out = captionsForTimeline(timelineOf([clip({ source_in: 0, source_out: 2 })]), {
			a1: [seg(0, 4, 'alpha bravo charlie delta echo foxtrot')]
		});
		expect(out.length).toBeGreaterThan(0);
		for (const o of out) expect(o.end).toBeLessThanOrEqual(2 + 1e-9);
		expect(out.some((o) => o.text.includes('foxtrot'))).toBe(false);
	});

	test('a reordered cut captions in the order it plays', () => {
		const out = captionsForTimeline(
			timelineOf([
				clip({ id: 'c1', source_in: 10, source_out: 12, timeline_start: 0 }),
				clip({ id: 'c2', source_in: 0, source_out: 2, timeline_start: 2 })
			]),
			{ a1: [seg(0, 2, 'first'), seg(10, 12, 'second')] }
		);
		expect(out.map((o) => o.text)).toEqual(['second', 'first']);
	});

	test('two captions never share the screen', () => {
		// The same footage twice in the cut — a callback shot, or a full source
		// parked under the edit — would caption the same words on top of itself.
		const out = captionsForTimeline(
			timelineOf([
				clip({ id: 'c1', source_in: 3, source_out: 12 }),
				clip({ id: 'c2', source_in: 0, source_out: 12 })
			]),
			{ a1: [seg(0, 6, 'alpha bravo charlie'), seg(6, 12, 'delta echo foxtrot')] }
		);
		expect(out.length).toBeGreaterThan(1);
		for (let i = 1; i < out.length; i++) {
			expect(out[i].start).toBeGreaterThanOrEqual(out[i - 1].end - 1e-6);
		}
	});

	test('a muted track is not captioned', () => {
		const tl = timelineOf([clip()]);
		expect(captionsForTimeline(tl, { a1: [seg(0, 4, 'heard')] })).toHaveLength(1);
		tl.tracks[0].muted = true;
		expect(captionsForTimeline(tl, { a1: [seg(0, 4, 'heard')] })).toHaveLength(0);
	});

	test('word punch puts one word on screen at a time', () => {
		const tl = timelineOf([clip({ source_out: 5 })]);
		const words = { a1: [seg(0, 5, 'alpha bravo charlie delta echo')] };
		const punched = captionsForTimeline(tl, words, resolveCaptions({ style: 'word_punch' }));
		expect(punched.map((o) => o.text)).toEqual(['alpha', 'bravo', 'charlie', 'delta', 'echo']);
		// The whole look, not just the word count.
		for (const o of punched) {
			expect(o.bold).toBe(true);
			expect(o.size).toBe(CAPTION_STYLES.word_punch.size);
			expect(o.pos_y).toBe(CAPTION_STYLES.word_punch.pos_y);
		}
		// Each word hands the screen straight to the next.
		for (let i = 1; i < punched.length; i++) expect(punched[i].start).toBeCloseTo(punched[i - 1].end, 9);
		// The default style is untouched by any of this.
		const lines = captionsForTimeline(tl, words);
		expect(lines).toHaveLength(2);
		expect(lines.every((o) => !o.bold)).toBe(true);
	});

	test('a word too short to read joins its neighbour', () => {
		// "a" is one character of thirty, so its character share is two frames.
		const punched = captionsForTimeline(
			timelineOf([clip({ source_out: 2 })]),
			{ a1: [seg(0, 2, 'a fairly quickly spoken sentence')] },
			resolveCaptions({ style: 'word_punch' })
		);
		for (const o of punched) expect(o.end - o.start).toBeGreaterThanOrEqual(MIN_WORD_CAPTION - 1e-6);
		expect(punched[0].text).toBe('a fairly');
	});

	test('an override moves one number and leaves the style alone', () => {
		const resolved = resolveCaptions({ style: 'word_punch', size: 0.2 });
		expect(resolved.max_words).toBe(1);
		expect(resolved.size).toBe(0.2);
		expect(resolved.pos_y).toBe(CAPTION_STYLES.word_punch.pos_y);
		// An unusable override falls back to the style rather than through it.
		expect(resolveCaptions({ style: 'word_punch', size: NaN }).size).toBe(CAPTION_STYLES.word_punch.size);
		expect(resolveCaptions({ style: 'word_punch', pos_y: 9 }).pos_y).toBe(1);
		// No options at all is the line style.
		expect(resolveCaptions()).toEqual(CAPTION_DEFAULTS);
	});

	test('a long word is shrunk to fit a vertical frame', () => {
		const tl = timelineOf([clip({ source_out: 4 })]);
		const words = { a1: [seg(0, 4, 'non-destructive editing')] };
		const punch = resolveCaptions({ style: 'word_punch' });
		// Unframed, so 16:9 — wide enough that nothing is shrunk.
		expect(captionsForTimeline(tl, words, punch).every((o) => o.size === punch.size)).toBe(true);
		// 9:16: drawtext neither wraps nor scales, so the long word would be
		// drawn off both edges.
		tl.format = { width: 1080, height: 1920, fit: 'cover' };
		const tall = captionsForTimeline(tl, words, punch);
		const long = tall.find((o) => o.text === 'non-destructive')!;
		const short = tall.find((o) => o.text === 'editing')!;
		expect(long.size).toBeLessThan(punch.size);
		expect(short.size).toBe(punch.size);
		expect('non-destructive'.length * 0.6 * long.size).toBeLessThanOrEqual(0.9 * (1080 / 1920) + 1e-9);
	});
});
