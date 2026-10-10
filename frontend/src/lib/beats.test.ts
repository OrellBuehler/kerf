import { describe, expect, test } from 'bun:test';
import { alignCutsToBeats, barGrid, beatGrid, defaultBeatTolerance, gridMagnets, nearestBeat } from './beats';
import { nearestWithin, quantizeSpanStart, quantizeTime } from './frames';
import type { Clip, Tempo, Timeline } from './types';

const GRID = Array.from({ length: 21 }, (_, i) => i * 0.5);

function clip(assetId: string, sourceIn: number, sourceOut: number, start: number, speed?: number): Clip {
	return {
		id: `${assetId}-${start}`,
		asset_id: assetId,
		source_in: sourceIn,
		source_out: sourceOut,
		timeline_start: start,
		volume: 1,
		fade_in: 0,
		fade_out: 0,
		...(speed === undefined ? {} : { speed })
	};
}

function timeline(kind: 'video' | 'audio', clips: Clip[]): Timeline {
	return { tracks: [{ id: 't', kind, name: 'T1', clips }], overlays: [], markers: [] } as unknown as Timeline;
}

const tempo = (beats: number[], confidence = 0.8, downbeats: number[] = []): Tempo => ({
	bpm: 120,
	beats,
	confidence,
	downbeats
});

describe('beatGrid', () => {
	test('maps source beats onto the timeline and drops the ones outside the window', () => {
		const tl = timeline('audio', [clip('music', 2, 6, 10)]);
		expect(beatGrid(tl, () => tempo([0, 1, 3, 5, 8]))).toEqual([11, 13]);
	});

	test('ignores a low-confidence tempo', () => {
		const tl = timeline('audio', [clip('music', 0, 4, 0)]);
		expect(beatGrid(tl, () => tempo([0.5, 1], 0.2))).toEqual([]);
	});

	test('ignores video tracks — the grid comes from the music', () => {
		const tl = timeline('video', [clip('shot', 0, 4, 0)]);
		expect(beatGrid(tl, () => tempo([0.5, 1]))).toEqual([]);
	});
});

describe('barGrid', () => {
	test('maps the downbeats onto the timeline like beats (Timeline::bar_grid)', () => {
		const tl = timeline('audio', [clip('music', 2, 6, 10)]);
		const t = tempo([0, 1, 2, 3, 4, 5, 6], 0.8, [0, 2, 4, 6, 8]);
		expect(barGrid(tl, () => t)).toEqual([10, 12, 14]);
		expect(beatGrid(tl, () => t)).toEqual([10, 11, 12, 13, 14]);
	});

	test('is empty without a bar grid, for a low-confidence tempo and on video tracks', () => {
		const audio = timeline('audio', [clip('music', 0, 8, 0)]);
		expect(barGrid(audio, () => tempo([0, 1, 2]))).toEqual([]);
		expect(barGrid(audio, () => tempo([0, 1, 2], 0.2, [0, 2]))).toEqual([]);
		expect(barGrid(timeline('video', [clip('shot', 0, 8, 0)]), () => tempo([0, 1], 0.8, [0]))).toEqual([]);
	});

	test('honors speed and reverse, and drops the copies overlapping clips repeat', () => {
		const t = tempo([0, 1, 2, 3, 4], 0.8, [0, 2, 4]);
		const fast = timeline('audio', [clip('music', 0, 4, 10, 2)]);
		expect(barGrid(fast, () => t)).toEqual([10, 11, 12]);
		const back = timeline('audio', [clip('music', 0, 4, 10, -1)]);
		expect(barGrid(back, () => t)).toEqual([10, 12, 14]);
		const twice = { tracks: [{ id: 't', kind: 'audio', name: 'A1', clips: [clip('music', 0, 4, 10), clip('music', 0, 4, 10)] }] } as unknown as Timeline;
		expect(barGrid(twice, () => t)).toEqual([10, 12, 14]);
	});

	test('a tempo from an older project without downbeats has no bars', () => {
		const old = { bpm: 120, beats: [0, 1, 2], confidence: 0.8 } as unknown as Tempo;
		expect(barGrid(timeline('audio', [clip('music', 0, 8, 0)]), () => old)).toEqual([]);
	});
});

describe('gridMagnets', () => {
	// 120 BPM: a beat every 0.5 s, a bar every 2 s.
	const beats = Array.from({ length: 21 }, (_, i) => i * 0.5);
	const bars = [0, 2, 4, 6, 8];

	test('offers the beats within reach', () => {
		expect(gridMagnets(1.1, [0], beats, [], 0.3)).toEqual([1]);
		expect(gridMagnets(1.25, [0], beats, [], 0.3)).toEqual([1, 1.5]);
		expect(gridMagnets(1.75, [0], beats, [], 0.1)).toEqual([]);
	});

	test('a bar in reach is offered instead of a nearer beat', () => {
		// 1.4 is 0.1 from the beat at 1.5 and 0.6 from the bar at 2: a wide reach prefers the bar.
		expect(gridMagnets(1.4, [0], beats, bars, 0.7)).toEqual([2]);
		// ...and a narrow one, with no bar in it, takes the beat.
		expect(gridMagnets(1.4, [0], beats, bars, 0.3)).toEqual([1.5]);
	});

	test('with the nearest-magnet rule the bar wins, and a closer clip edge still beats it', () => {
		const snap = (t: number, tol: number, extra: number[] = []) =>
			quantizeTime(t, { fps: 30, magnets: [...gridMagnets(t, [0], beats, bars, tol), ...extra], threshold: tol });
		expect(snap(1.4, 0.7)).toBe(2);
		expect(snap(1.4, 0.3)).toBe(1.5);
		// An edge at 1.45 is nearer than the bar, so it is the magnet that wins.
		expect(snap(1.4, 0.7, [1.45])).toBe(1.45);
		expect(nearestWithin(1.4, [2, 1.45], 0.7)).toBe(1.45);
	});

	test('a span puts either end on the grid, bars first', () => {
		// A 1.2 s clip starting at 0.9: its head is 0.1 from the beat at 1, its tail (2.1) is
		// 0.1 from the bar at 2 — so the bar is the one offered, as a start of 0.8.
		const m = gridMagnets(0.9, [0, 1.2], beats, bars, 0.35);
		expect(m.map((x) => Math.round(x * 1e6) / 1e6)).toEqual([0.8]);
		expect(quantizeSpanStart(0.9, 1.2, { fps: 30, magnets: m, threshold: 0.35 })).toBeCloseTo(0.8, 9);
		// No bar in reach of either end: the beats at both are offered.
		const only = gridMagnets(0.4, [0, 0.65], beats, bars, 0.15);
		expect(only.map((x) => Math.round(x * 1e6) / 1e6)).toEqual([0.5, 0.35]);
	});

	test('nothing is offered without a tolerance, and a mark exactly a tolerance away is out of reach', () => {
		expect(gridMagnets(1, [0], beats, bars, 0)).toEqual([]);
		expect(gridMagnets(1, [0], beats, bars, -1)).toEqual([]);
		expect(gridMagnets(1.25, [0], [1.5], [], 0.25)).toEqual([]);
	});
});

describe('nearestBeat', () => {
	test('takes the closest beat within tolerance', () => {
		expect(nearestBeat(GRID, 0.6, 0.25)).toBe(0.5);
		expect(nearestBeat(GRID, 0.9, 0.25)).toBe(1);
		expect(nearestBeat(GRID, 0.75, 0.1)).toBeNull();
		expect(nearestBeat(GRID, 99, 0.25)).toBeNull();
	});

	test('defaults tolerance to half a beat', () => {
		expect(defaultBeatTolerance(GRID)).toBe(0.25);
		expect(defaultBeatTolerance([])).toBe(0);
	});
});

describe('alignCutsToBeats', () => {
	test('ripples every cut onto the grid and is a no-op on a second run', () => {
		const clips = [clip('a', 0, 1.1, 0), clip('a', 4, 4.9, 1.1)];
		expect(alignCutsToBeats(clips, GRID, 0.25, () => 10)).toBe(2);
		expect(clips[0].source_out).toBeCloseTo(1, 9);
		expect(clips[1].timeline_start).toBeCloseTo(1, 9);
		expect(clips[1].source_in).toBe(4);
		expect(clips[1].source_out).toBeCloseTo(5, 9);
		expect(alignCutsToBeats(clips, GRID, 0.25, () => 10)).toBe(0);
	});

	test('keeps gaps and stretches a clip only as far as it has footage', () => {
		const clips = [clip('a', 0, 0.4, 0), clip('a', 0, 1.4, 0.9)];
		alignCutsToBeats(clips, GRID, 0.25, () => 1.2);
		expect(clips[0].source_out).toBeCloseTo(0.5, 9);
		expect(clips[1].timeline_start).toBeCloseTo(1, 9);
		expect(clips[1].source_out).toBeCloseTo(1.2, 9);
	});

	test('trims a reversed clip at its outgoing edge, which is the source start', () => {
		const clips = [clip('a', 1, 2.1, 0, -1)];
		alignCutsToBeats(clips, GRID, 0.25, () => 10);
		expect(clips[0].source_out).toBe(2.1);
		expect(clips[0].source_in).toBeCloseTo(1.1, 9);
	});
});
