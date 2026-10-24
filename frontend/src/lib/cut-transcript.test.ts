import { describe, expect, test } from 'bun:test';
import { cutTranscript } from './cut-transcript';
import type { Clip, StreamKind, Timeline, TranscriptSegment } from './types';

const clip = (id: string, asset: string, source_in: number, source_out: number, timeline_start: number, over: Partial<Clip> = {}): Clip =>
	({ id, asset_id: asset, source_in, source_out, timeline_start, volume: 1, speed: 1, ...over }) as Clip;

const timeline = (tracks: [StreamKind, Clip[]][]): Timeline => ({
	tracks: tracks.map(([kind, clips], i) => ({ id: `t${i}`, kind, name: `T${i}`, clips })),
	overlays: [],
	markers: []
});

const timed = (text: string, words: [string, number, number][]): TranscriptSegment => ({
	start: words[0][1],
	end: words[words.length - 1][2],
	text,
	words: words.map(([t, start, end]) => ({ text: t, start, end })),
	speaker: 'S1'
});

const sentence = () =>
	timed('One two three four.', [
		['One', 10, 10.5],
		['two', 10.5, 11],
		['three', 11, 11.5],
		['four.', 11.5, 12]
	]);

describe('cutTranscript (mirrors model::cut_transcript tests)', () => {
	test('a trimmed, moved clip reads in timeline time', () => {
		const t = timeline([['video', [clip('c', 'a', 10, 12, 3)]]]);
		const [l] = cutTranscript(t, { a: [sentence()] });
		expect(l.text).toBe('One two three four.');
		expect([l.start, l.end, l.source_start, l.source_end]).toEqual([3, 5, 10, 12]);
		expect([l.clip_id, l.track_id, l.speaker]).toEqual(['c', 't0', 'S1']);
		expect(l.words![2].start).toBeCloseTo(4, 9);
	});

	test('a word belongs to the clip that plays its midpoint', () => {
		const t = timeline([['audio', [clip('l', 'a', 10, 11.2, 0), clip('r', 'a', 11.2, 12, 1.2)]]]);
		const lines = cutTranscript(t, { a: [sentence()] });
		expect(lines.map((l) => l.text)).toEqual(['One two', 'three four.']);
		expect(lines[1].words![0].source_start).toBeCloseTo(11.2, 9);
		expect(lines[1].words![0].start).toBeCloseTo(1.2, 9);
		expect(lines[0].end).toBeCloseTo(1, 9);
	});

	test('a sliver of a word is not read', () => {
		const t = timeline([['audio', [clip('c', 'a', 10, 10.6, 0)]]]);
		expect(cutTranscript(t, { a: [sentence()] }).map((l) => l.text)).toEqual(['One']);
	});

	test('speed and reverse retime the words', () => {
		const t = timeline([['audio', [clip('f', 'a', 10, 12, 0, { speed: 2 }), clip('b', 'a', 10, 12, 5, { speed: -1 })]]]);
		const lines = cutTranscript(t, { a: [sentence()] });
		expect([lines[0].start, lines[0].end, lines[0].words![1].start]).toEqual([0, 1, 0.25]);
		expect([lines[1].start, lines[1].end]).toEqual([5, 7]);
		expect([lines[1].words![0].start, lines[1].words![0].end]).toEqual([6.5, 7]);
	});

	test('muted tracks, disabled clips and detached pictures say nothing', () => {
		const t = timeline([
			['video', [clip('p', 'a', 10, 12, 0, { source_audio: false })]],
			['audio', [clip('s', 'a', 10, 12, 0), clip('off', 'a', 10, 12, 20, { enabled: false })]],
			['audio', [clip('m', 'a', 10, 12, 30)]]
		]);
		t.tracks[2].muted = true;
		expect(cutTranscript(t, { a: [sentence()] }).map((l) => l.clip_id)).toEqual(['s']);
	});

	test('the same words heard from two clips come once', () => {
		const t = timeline([
			['video', [clip('p', 'a', 10, 12, 0)]],
			['audio', [clip('s', 'a', 10, 12, 0)]]
		]);
		expect(cutTranscript(t, { a: [sentence()] }).map((l) => l.clip_id)).toEqual(['p']);
	});

	test('overlapping tracks interleave by start', () => {
		const t = timeline([
			['audio', [clip('x', 'a', 10, 12, 1)]],
			['audio', [clip('y', 'b', 10, 12, 0)]]
		]);
		const lines = cutTranscript(t, {
			a: [sentence()],
			b: [
				timed('Hi there.', [
					['Hi', 10, 10.4],
					['there.', 10.4, 11]
				])
			]
		});
		expect(lines.map((l) => [l.text, l.track_id])).toEqual([
			['Hi there.', 't1'],
			['One two three four.', 't0']
		]);
	});

	test('a line without words is cut by estimated ones', () => {
		const t = timeline([['audio', [clip('c', 'a', 0, 2, 0)]]]);
		const [l] = cutTranscript(t, { a: [{ start: 0, end: 4, text: 'aaaa bbbb cccc dddd' }] });
		expect(l.text).toBe('aaaa bbbb');
		expect(l.words!.every((w) => w.estimated)).toBe(true);
		expect(l.end).toBeCloseTo(2, 9);
	});

	test('footage outside every clip and untimed lines are not read', () => {
		const t = timeline([['audio', [clip('c', 'a', 0, 5, 0)]]]);
		const lines = cutTranscript(t, {
			a: [sentence(), { start: 1, end: 2, text: '  ' }, { start: Number.NaN, end: 2, text: 'nope' }]
		});
		expect(lines).toEqual([]);
		expect(cutTranscript(t, { other: [sentence()] })).toEqual([]);
	});
});
