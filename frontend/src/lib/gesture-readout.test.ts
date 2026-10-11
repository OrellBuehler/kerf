import { describe, expect, test } from 'bun:test';
import { cutReadout, moveReadout, trimEdgeMonitor, trimEdgeReadout, trimLimitWhy } from './gesture-readout';
import { MIN_CLIP, trimBounds } from './ripple-trim';
import { trimEdit } from './frames';
import type { Clip } from './types';

const clip = (id: string, start: number, len: number, over: Partial<Clip> = {}): Clip => ({
	id,
	asset_id: 'a',
	source_in: 2,
	source_out: 2 + len,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...over
});

describe('trimEdgeReadout', () => {
	test('a right edge pulled in says how much was lost and how long the clip now is', () => {
		const r = trimEdgeReadout({ edge: 'r', start: 10, end: 20, pos: 19.6, fps: 30, limit: null });
		expect(r.title).toBe('Trim out −12 f · −0.40 s · length 00:00:09:18');
		expect(r.tone).toBe('ok');
		expect(r.detail).toBeNull();
	});

	test('a left edge pulled in shortens it too, and one pushed out lengthens it', () => {
		expect(trimEdgeReadout({ edge: 'l', start: 10, end: 20, pos: 10.5, fps: 30, limit: null }).title).toBe(
			'Trim in −15 f · −0.50 s · length 00:00:09:15'
		);
		expect(trimEdgeReadout({ edge: 'l', start: 10, end: 20, pos: 9, fps: 30, limit: null }).title).toBe(
			'Trim in +30 f · +1.00 s · length 00:00:11:00'
		);
	});

	test('an edge not moved reads zero', () => {
		expect(trimEdgeReadout({ edge: 'r', start: 0, end: 5, pos: 5, fps: 24, limit: null }).title).toBe('Trim out 0 f · length 00:00:05:00');
	});

	test('held at a limit it is amber and says what holds it', () => {
		const r = trimEdgeReadout({ edge: 'r', start: 0, end: 5, pos: 7, fps: 30, limit: 'no more footage on that side' });
		expect(r.tone).toBe('limit');
		expect(r.detail).toBe('No more footage on that side');
	});
});

describe('trimLimitWhy', () => {
	const c = clip('c', 5, 4); // [5, 9), source 2..6 of a 10 s asset: 2 s of head, 4 s of tail
	const neighbours = [clip('p', 0, 5), c, clip('n', 9, 3)];
	const strict = (edge: 'l' | 'r') => trimBounds(c, edge, neighbours, 10, false, false);
	const loose = (edge: 'l' | 'r') => trimBounds(c, edge, neighbours, 10, false, true);

	test('a neighbour in the way', () => {
		expect(trimLimitWhy('r', 'max', strict('r'), loose('r'))).toBe('the next clip is in the way');
		expect(trimLimitWhy('l', 'min', strict('l'), loose('l'))).toBe('the previous clip is in the way');
	});

	test('with ripple the neighbours give way, and what is left is the footage', () => {
		expect(trimLimitWhy('r', 'max', loose('r'), loose('r'))).toBe('no more footage on that side');
		expect(trimLimitWhy('l', 'min', loose('l'), loose('l'))).toBe('no more footage on that side');
	});

	test('the shortest a clip can be', () => {
		expect(trimLimitWhy('r', 'min', strict('r'), loose('r'))).toBe(`a clip is at least ${MIN_CLIP} s`);
		expect(trimLimitWhy('l', 'max', strict('l'), loose('l'))).toBe(`a clip is at least ${MIN_CLIP} s`);
	});

	test('the start of the cut', () => {
		const atZero = clip('z', 0, 4, { source_in: 3, source_out: 7 });
		const bz = trimBounds(atZero, 'l', [atZero], 10, false, false);
		expect(trimLimitWhy('l', 'min', bz, bz)).toBe('the cut starts here');
	});
});

describe('moveReadout', () => {
	test('how far, where the start is now, which lane', () => {
		const r = moveReadout({ delta: 1.2, start: 15, track: 'V2', fps: 30, count: 1, refused: null });
		expect(r.title).toBe('Move +36 f · +1.20 s · 00:00:15:00 · V2');
		expect(r.tone).toBe('ok');
	});

	test('a group says how many', () => {
		expect(moveReadout({ delta: -0.5, start: 3, track: 'A1', fps: 30, count: 3, refused: null }).title).toBe(
			'Move −15 f · −0.50 s · 00:00:03:00 · A1 · 3 clips'
		);
	});

	test('a drop the engine would refuse is red and says why', () => {
		const r = moveReadout({ delta: 2, start: 4, track: 'V1', fps: 30, count: 1, refused: 'it would overlap another clip' });
		expect(r.tone).toBe('refused');
		expect(r.detail).toBe('It would overlap another clip');
	});
});

describe('cutReadout', () => {
	test('the time of the cut, and the lanes it goes through with it', () => {
		expect(cutReadout({ at: 5.4, fps: 30, along: [] })).toEqual({ title: 'Cut 00:00:05:12', detail: null, tone: 'ok' });
		expect(cutReadout({ at: 5.4, fps: 30, along: ['A1'] }).title).toBe('Cut 00:00:05:12 · with A1');
	});

	test('a clip too short to cut says so', () => {
		const r = cutReadout({ at: null, fps: 30, along: [] });
		expect(r.tone).toBe('refused');
		expect(r.detail).toContain('too short');
	});
});

describe('trimEdgeMonitor', () => {
	const c = clip('c', 5, 4); // source 2..6
	const ro = { title: 't', detail: null, tone: 'ok' as const };

	test('a right edge shows the last frame it would leave, from the source', () => {
		const m = trimEdgeMonitor(c, 'r', 8, 30, 'video', ro)!;
		expect(m.cells).toHaveLength(1);
		const cell = m.cells[0];
		expect(cell.label).toBe('Out');
		expect(cell.clipId).toBe('c');
		expect(cell.assetId).toBe('a');
		// the clip is trimmed to [5, 8): source 2..5, last shown frame one frame short of 5
		expect(cell.time).toBeCloseTo(5 - 1 / 30, 9);
		expect(m.title).toBe('t');
	});

	test('a left edge shows the first frame it would leave', () => {
		const cell = trimEdgeMonitor(c, 'l', 6, 30, 'video', ro)!.cells[0];
		expect(cell.label).toBe('In');
		expect(cell.time).toBeCloseTo(3, 9);
		// and it is the trim the commit writes
		expect(trimEdit(c, 'l', 6).source_in).toBeCloseTo(3, 9);
	});

	test('a reversed clip swaps what is first and last', () => {
		const rev = clip('r', 5, 4, { speed: -1 });
		const right = trimEdgeMonitor(rev, 'r', 8, 30, 'video', ro)!.cells[0];
		// reversed, the right edge is the end of playback: the footage's earliest frame still shown
		expect(right.time).toBeCloseTo(trimEdit(rev, 'r', 8).source_in!, 9);
	});

	test('sound has no picture to show', () => {
		expect(trimEdgeMonitor(c, 'r', 8, 30, 'audio', ro)).toBeNull();
	});
});
