import { describe, expect, test } from 'bun:test';
import { assetLine, projectFrame, projectLine } from './status-facts';
import type { Asset, Timeline } from './types';

const video = (id: string, width: number, height: number, fps = 30, codec = 'h264') =>
	({ id, streams: [{ index: 0, kind: 'video', codec, width, height, fps }] }) as unknown as Asset;
const clipOf = (asset_id: string) => ({ id: `c-${asset_id}`, asset_id, source_in: 0, source_out: 1, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0 });
const cut = (...ids: string[]): Timeline => ({ tracks: [{ id: 'v1', kind: 'video', name: 'V1', clips: ids.map(clipOf) }] });

describe('the project’s frame', () => {
	const assets = [video('a', 1920, 1080), video('b', 3840, 2160, 24)];

	test('is the delivery frame when the project has one', () => {
		expect(projectFrame({ ...cut('a'), format: { width: 1080, height: 1920, fit: 'cover' } } as Timeline, assets)).toEqual({ width: 1080, height: 1920 });
	});

	test('else the first picture on the cut, not the selected asset', () => {
		expect(projectFrame(cut('b', 'a'), assets)).toEqual({ width: 3840, height: 2160 });
	});

	test('an empty cut has none', () => {
		expect(projectFrame(cut(), assets)).toBeNull();
		expect(projectFrame(cut('gone'), assets)).toBeNull();
	});
});

describe('the lines', () => {
	test('the project: rate and frame, trimmed of float noise', () => {
		expect(projectLine(30, { width: 1920, height: 1080 })).toBe('30 fps · 1920×1080');
		expect(projectLine(29.97002997, null)).toBe('29.97 fps');
		expect(projectLine(24.000001, { width: 3840, height: 2160 })).toBe('24 fps · 3840×2160');
	});

	test('an asset: rate, size and codec, as before — and none when it has none of them', () => {
		expect(assetLine(video('b', 3840, 2160, 24))).toBe('24 fps · 3840×2160 · h264');
		expect(assetLine({ streams: [{ index: 0, kind: 'audio', codec: 'mp3' }] } as unknown as Asset)).toBe('mp3');
		expect(assetLine({ streams: [] })).toBeNull();
	});
});
