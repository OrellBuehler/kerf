import { describe, expect, test } from 'bun:test';
import { alignCutsToBeats, beatGrid, defaultBeatTolerance, nearestBeat } from './beats';
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

const tempo = (beats: number[], confidence = 0.8): Tempo => ({ bpm: 120, beats, confidence });

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
