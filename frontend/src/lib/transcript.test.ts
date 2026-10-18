import { describe, expect, test } from 'bun:test';
import { activeLineIndex, fmtClock, srcToTimeline, srtStamp, toSrt, transcriptLines } from './transcript';
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

// The twin of `transcript_to_srt` (kerf-core `model.rs`): the harness writes the same file the app does.
describe('a transcript as .srt', () => {
	test('a timestamp is rounded to the millisecond and carried, never negative', () => {
		expect(srtStamp(0)).toBe('00:00:00,000');
		expect(srtStamp(5.5)).toBe('00:00:05,500');
		expect(srtStamp(3725.0495)).toBe('01:02:05,050');
		expect(srtStamp(59.9996)).toBe('00:01:00,000');
		for (const t of [-4, NaN, Infinity]) expect(srtStamp(t)).toBe('00:00:00,000');
	});

	test('every segment is a numbered cue ending in a blank line', () => {
		expect(
			toSrt([
				{ start: 0, end: 5.5, text: 'Welcome back to the channel.' },
				{ start: 5.5, end: 12.5, text: '  Today we are talking about editing.  ' }
			])
		).toBe(
			'1\n00:00:00,000 --> 00:00:05,500\nWelcome back to the channel.\n\n2\n00:00:05,500 --> 00:00:12,500\nToday we are talking about editing.\n\n'
		);
	});

	test('no segments is an empty document', () => {
		expect(toSrt([])).toBe('');
	});
});
