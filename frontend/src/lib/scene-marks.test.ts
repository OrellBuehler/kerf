import { describe, expect, test } from 'bun:test';
import { sceneTimes } from './scene-marks';
import type { Clip, Timeline } from './types';

function clip(id: string, assetId: string, sourceIn: number, sourceOut: number, start: number, speed?: number): Clip {
	return {
		id,
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

const timeline = (...lanes: [('video' | 'audio'), Clip[]][]): Timeline =>
	({
		tracks: lanes.map(([kind, clips], i) => ({ id: `t${i}`, kind, name: `T${i}`, clips })),
		overlays: [],
		markers: []
	}) as unknown as Timeline;

const ALL = [0, 1e9] as const;
const scenes = (map: Record<string, number[]>) => (id: string) => map[id];

describe('sceneTimes', () => {
	test('two clips of one asset at the same alignment make one mark per scene', () => {
		// Two video tracks showing the same footage in step: the cut at 5 s is one mark, not two.
		const tl = timeline(['video', [clip('a', 'shot', 0, 10, 0)]], ['video', [clip('b', 'shot', 0, 10, 0)]]);
		expect(sceneTimes(tl, scenes({ shot: [3, 5] }), ALL)).toEqual([3, 5]);
	});

	test('maps the source cut onto the timeline through the clip, speed included', () => {
		const tl = timeline(['video', [clip('a', 'shot', 2, 10, 20, 2)]]);
		// source 6 is 4 s into the footage, 2 s of timeline at double speed.
		expect(sceneTimes(tl, scenes({ shot: [1, 6, 11] }), ALL)).toEqual([22]);
	});

	test('keeps different clips\' marks and sorts them', () => {
		const tl = timeline(['video', [clip('b', 'x', 0, 4, 10), clip('a', 'x', 0, 4, 0)]]);
		expect(sceneTimes(tl, scenes({ x: [1, 3] }), ALL)).toEqual([1, 3, 11, 13]);
	});

	test('cuts within a few ms of each other are one mark', () => {
		const tl = timeline(['video', [clip('a', 'x', 0, 10, 0)]], ['video', [clip('b', 'x', 0, 10, 0.002)]]);
		expect(sceneTimes(tl, scenes({ x: [4] }), ALL)).toHaveLength(1);
	});

	test('a window culls marks outside it, edges included', () => {
		const tl = timeline(['video', [clip('a', 'x', 0, 100, 0)]]);
		const sc = scenes({ x: [5, 10, 15, 20, 90] });
		expect(sceneTimes(tl, sc, [10, 20])).toEqual([10, 15, 20]);
		expect(sceneTimes(tl, sc, [50, 60])).toEqual([]);
	});

	test('a mark does not depend on the zoom: nothing in the answer is a pixel', () => {
		const tl = timeline(['video', [clip('a', 'x', 0, 10, 0)]]);
		expect(sceneTimes(tl, scenes({ x: [2.5] }), ALL)).toEqual([2.5]);
	});

	test('audio tracks, unanalysed assets and cuts at or past the clip\'s edges give none', () => {
		const tl = timeline(['audio', [clip('m', 'x', 0, 10, 0)]], ['video', [clip('a', 'y', 2, 8, 0), clip('b', 'z', 0, 5, 9)]]);
		expect(sceneTimes(tl, scenes({ x: [3], y: [2, 8, 9] }), ALL)).toEqual([]);
	});
});
