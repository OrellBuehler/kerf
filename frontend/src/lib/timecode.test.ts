import { describe, expect, test } from 'bun:test';
import { formatTimecode, timelineFps } from './timecode';
import type { Asset, Timeline } from './types';

const assets = [
	{ id: 'a', streams: [{ kind: 'video', fps: 30 }] },
	{ id: 'b', streams: [{ kind: 'video', fps: 24 }] },
	{ id: 'sound', streams: [{ kind: 'audio' }] }
] as Asset[];
const timeline = (ids: string[]) => ({ tracks: [{ kind: 'video', clips: ids.map(asset_id => ({ asset_id })) }] }) as Timeline;

describe('timeline timebase', () => {
	test('follows the first source with video in timeline order, independent of bin order', () => {
		expect(timelineFps(timeline(['b', 'a']), assets)).toBe(24);
		expect(timelineFps(timeline(['sound', 'missing', 'a', 'b']), assets)).toBe(30);
	});
	test('uses the engine fallback when no video or no valid source rate exists', () => {
		expect(timelineFps(timeline(['sound']), assets)).toBe(30);
		for (const fps of [undefined, 0, -1, NaN, Infinity]) {
			expect(timelineFps(timeline(['a', 'b']), [{...assets[0], streams:[{kind:'video', index:0, codec:'h264', fps}]}, assets[1]])).toBe(30);
		}
	});
});

describe('formatTimecode', () => {
	test('counts every source frame at 24, 25, 30 and 60 fps', () => {
		for (const fps of [24,25,30,60]) {
			for (let frame=0; frame<fps*3; frame++) {
				expect(formatTimecode(frame/fps, fps)).toBe(`00:${String(Math.floor(frame/fps)).padStart(2,'0')}:${String(frame%fps).padStart(2,'0')}`);
			}
		}
	});
	test('uses non-drop frame numbering for fractional rates', () => {
		expect(formatTimecode(30/(30000/1001), 30000/1001)).toBe('00:01:00');
		expect(formatTimecode(60, 30000/1001)).toBe('00:59:28');
	});
	test('handles minute boundaries and invalid input', () => {
		expect(formatTimecode(60, 30)).toBe('01:00:00');
		expect(formatTimecode(3600, 30)).toBe('60:00:00');
		for (const t of [-1, NaN, Infinity]) expect(formatTimecode(t,30)).toBe('00:00:00');
		expect(formatTimecode(0.5,0)).toBe('00:00:15');
	});
});
