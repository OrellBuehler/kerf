import { describe, expect, test } from 'bun:test';
import { LAND_EPS, magnetList, markAt, snapLines } from './snap';
import { quantizeSpanStart, quantizeTime } from './frames';
import { gridMagnets } from './beats';
import { startBefore } from './frames';

describe('magnetList', () => {
	test('names the playhead, the start, bars, beats and the edges of the lane', () => {
		const m = magnetList({ playhead: 4, beats: [0.5, 1], bars: [2, 6], edges: [3, 9], lane: 'V1' });
		expect(m.map((x) => [x.kind, x.label, x.at])).toEqual([
			['playhead', 'Playhead', 4],
			['start', 'Start', 0],
			['bar', 'Bar 1', 2],
			['bar', 'Bar 2', 6],
			['beat', 'Beat', 0.5],
			['beat', 'Beat', 1],
			['edge', 'Edge V1', 3],
			['edge', 'Edge V1', 9]
		]);
	});

	test('an edge with no lane name is just an edge, and titles are their own kind', () => {
		const m = magnetList({ playhead: 0, edges: [1], titles: [2] });
		expect(m.slice(-2).map((x) => x.label)).toEqual(['Edge', 'Title edge']);
	});
});

describe('markAt', () => {
	const marks = magnetList({ playhead: 4, beats: [1.5], bars: [2], edges: [4, 7.25], lane: 'A1' });

	test('names the mark an edge sits exactly on', () => {
		expect(markAt(2, marks)?.label).toBe('Bar 1');
		expect(markAt(1.5, marks)?.label).toBe('Beat');
		expect(markAt(7.25, marks)?.label).toBe('Edge A1');
		expect(markAt(0, marks)?.label).toBe('Start');
	});

	test('on a tie the playhead is named before the edge that shares its time', () => {
		expect(markAt(4, marks)?.label).toBe('Playhead');
	});

	test('an edge that only rounded to a frame is on nothing', () => {
		expect(markAt(2.0333333, marks)).toBeNull();
		expect(markAt(2 + 1e-6, marks)).toBeNull();
	});

	test('float noise is not a miss, but a real distance is', () => {
		expect(markAt(2 + LAND_EPS / 2, marks)?.label).toBe('Bar 1');
		expect(markAt(2 + LAND_EPS * 10, marks)).toBeNull();
		expect(markAt(Number.NaN, marks)).toBeNull();
	});
});

describe('snapLines', () => {
	const marks = magnetList({ playhead: 5, bars: [8], edges: [12], lane: 'V1' });

	test('one line per edge that is on a magnet', () => {
		expect(snapLines([5, 9], marks)).toEqual([{ at: 5, label: 'Playhead' }]);
		expect(snapLines([5, 12], marks)).toEqual([
			{ at: 5, label: 'Playhead' },
			{ at: 12, label: 'Edge V1' }
		]);
	});

	test('no line when no edge is on one', () => {
		expect(snapLines([6, 10], marks)).toEqual([]);
		expect(snapLines([], marks)).toEqual([]);
	});

	test('two edges on the same time are one line', () => {
		expect(snapLines([8, 8], marks)).toEqual([{ at: 8, label: 'Bar 1' }]);
	});
});

// The line is read off the gesture's result: what the snap decided, the marks it was offered.
// So the claim that matters is that a magnet the snap took shows, and a frame it merely rounded
// to does not — held against the real snap functions.
describe('against the snap itself', () => {
	const fps = 30;
	const threshold = 8 / 40;
	const beats = [1.25, 1.75, 2.25];
	const bars = [1.25];
	const edges = [6, 9.5];
	const playhead = 3.1;
	const marks = magnetList({ playhead, beats, bars, edges, lane: 'V1' });
	const magnets = [0, playhead, ...gridMagnets(0, [0], beats, bars, Infinity), ...edges];

	test('a point pulled onto a magnet is named; the same point snapping off is not', () => {
		const near = quantizeTime(6.1, { fps, magnets, threshold, welds: edges });
		expect(near).toBe(6);
		expect(snapLines([near], marks)).toEqual([{ at: 6, label: 'Edge V1' }]);
		const off = quantizeTime(6.1, { fps, magnets: [], threshold, welds: edges });
		expect(snapLines([off], [])).toEqual([]);
	});

	test('a point that no magnet reached rounds to a frame and shows nothing', () => {
		const free = quantizeTime(4.5, { fps, magnets, threshold, welds: edges });
		expect(free).toBeCloseTo(4.5, 9);
		expect(snapLines([free], marks)).toEqual([]);
	});

	test('a clip span landed head-on or tail-on a magnet shows the line where the edge is', () => {
		const dur = 1.2;
		// head within reach of the playhead
		const head = quantizeSpanStart(3.15, dur, { fps, magnets: [playhead], threshold });
		expect(head).toBe(playhead);
		expect(snapLines([head, head + dur], marks)).toEqual([{ at: playhead, label: 'Playhead' }]);
		// tail within reach of an edge: the start is `edge - dur` by the backend's own arithmetic
		const start = startBefore(9.5, dur);
		const placed = quantizeSpanStart(start + 0.1, dur, { fps, magnets: [start], threshold, welds: edges });
		expect(snapLines([placed, placed + dur], marks).map((l) => l.at)).toEqual([9.5]);
	});
});
