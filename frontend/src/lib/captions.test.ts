import { describe, expect, test } from 'bun:test';
import {
	captionsForTimeline,
	chunkWords,
	coversSource,
	sourceToTimeline,
	timeChunks,
	CAPTION_DEFAULTS,
	MIN_CAPTION
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
});
