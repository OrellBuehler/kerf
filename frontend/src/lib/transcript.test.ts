import { describe, expect, test } from 'bun:test';
import { activeLineIndex, estimateWords, fmtClock, speakersOf, srcToTimeline, transcriptLines } from './transcript';
import type { Clip, Track } from './types';

const clip = (over: Partial<Clip>): Clip =>
	({
		id: 'c1',
		asset_id: 'a1',
		source_in: 10,
		source_out: 20,
		timeline_start: 5,
		...over
	}) as Clip;
const tracks = (...clips: Clip[]): Track[] => [{ id: 't1', kind: 'video', clips } as unknown as Track];
const seg = (start: number, end: number, text = 'hi') => ({ start, end, text });

describe('fmtClock', () => {
	test('pads minutes and seconds', () => {
		expect(fmtClock(0)).toBe('00:00');
		expect(fmtClock(65.9)).toBe('01:05');
	});
});

describe('srcToTimeline', () => {
	test('offsets by the clip position and honors speed and reverse', () => {
		expect(srcToTimeline(clip({}), 12)).toBe(7);
		expect(srcToTimeline(clip({ speed: 2 }), 14)).toBe(7);
		expect(srcToTimeline(clip({ speed: -1 }), 18)).toBe(7);
	});
});

describe('transcriptLines', () => {
	test('resolves a segment to the clip showing it, or null once cut', () => {
		const c = clip({});
		const lines = transcriptLines([seg(11, 13), seg(30, 32)], 'a1', tracks(c));
		expect(lines[0].clip).toBe(c);
		expect(lines[1].clip).toBeNull();
		expect(lines[0].t).toBe('00:11');
	});

	test('ignores clips of other assets', () => {
		const lines = transcriptLines([seg(11, 13)], 'other', tracks(clip({})));
		expect(lines[0].clip).toBeNull();
	});
});

describe('activeLineIndex', () => {
	test('finds the line under the playhead in timeline time', () => {
		const lines = transcriptLines([seg(11, 13), seg(14, 16)], 'a1', tracks(clip({})));
		expect(activeLineIndex(lines, 6.5)).toBe(0);
		expect(activeLineIndex(lines, 9.5)).toBe(1);
		expect(activeLineIndex(lines, 0)).toBe(-1);
	});
});

describe('words and speakers', () => {
	test('a line with words cuts to its words, and carries its speaker', () => {
		const line = {
			start: 10,
			end: 14,
			text: 'a b',
			speaker: 'S2',
			words: [
				{ text: 'a', start: 10.5, end: 11 },
				{ text: 'b', start: 11, end: 13 }
			]
		};
		const [l] = transcriptLines([line], 'a1', tracks(clip({})));
		expect(l.cut).toEqual({ start: 10.5, end: 13 });
		expect(l.speaker).toBe('S2');
		const [plain] = transcriptLines([seg(12, 13)], 'a1', tracks(clip({})));
		expect(plain.cut).toEqual({ start: 12, end: 13 });
		expect(plain.speaker).toBeNull();
	});

	test('speakers in order of first appearance', () => {
		const segs = [{ ...seg(0, 1), speaker: 'S2' }, seg(1, 2), { ...seg(2, 3), speaker: 'S1' }, { ...seg(3, 4), speaker: 'S2' }];
		expect(speakersOf(segs)).toEqual(['S2', 'S1']);
	});

	test('estimated words share the line by character length (estimated_words_share_the_line_by_character_length)', () => {
		const words = estimateWords({ start: 1, end: 3, text: 'a bbb' });
		expect(words.map((w) => [w.text, w.start, w.end, w.estimated])).toEqual([
			['a', 1, 1.5, true],
			['bbb', 1.5, 3, true]
		]);
		expect(estimateWords({ start: 2, end: 2, text: 'hi' })).toEqual([]);
	});
});
