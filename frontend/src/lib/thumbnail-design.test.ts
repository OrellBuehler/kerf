import { beforeEach, describe, expect, test } from 'bun:test';
import {
	defaultThumbnailSize,
	evenFrames,
	HEADLINE_PLACEMENTS,
	keptLayers,
	layerKind,
	parseThumbnailSize,
	placementOf,
	putThumbnail,
	sizeName
} from './thumbnail-design';
import { removeThumbnail, revertTo, setThumbnail, suggestThumbnailFrames, thumbnailPreview } from './api';
import type { Timeline } from './types';

let n = 0;
const uid = () => `id${++n}`;
const empty = (): Timeline => ({ tracks: [] });
const headline = (text: string) => ({ text: { text, color: '#ffffff', accent: '#ffb000' } });

describe('thumbnail sizes', () => {
	test('read like parse_thumbnail_size', () => {
		expect(parseThumbnailSize('youtube')).toEqual([1280, 720]);
		expect(parseThumbnailSize(' Vertical ')).toEqual([1080, 1920]);
		expect(parseThumbnailSize('4:5')).toEqual([1080, 1350]);
		expect(parseThumbnailSize('16:9')).toEqual([1920, 1080]);
		expect(parseThumbnailSize('641x361')).toEqual([640, 360]);
		expect(() => parseThumbnailSize('banner')).toThrow('not a thumbnail size');
		expect(() => parseThumbnailSize('8000x100')).toThrow('16 to 4096');
		expect(sizeName(1080, 1080)).toBe('square');
		expect(sizeName(640, 360)).toBe('640x360');
	});

	test('a new one takes the delivery shape like default_size', () => {
		expect(defaultThumbnailSize(null)).toEqual([1280, 720]);
		expect(defaultThumbnailSize({ width: 1080, height: 1920, fit: 'cover' })).toEqual([1080, 1920]);
		expect(defaultThumbnailSize({ width: 1080, height: 1080, fit: 'cover' })).toEqual([1080, 1080]);
		expect(defaultThumbnailSize({ width: 1080, height: 1350, fit: 'cover' })).toEqual([1080, 1350]);
		expect(defaultThumbnailSize({ width: 3840, height: 2160, fit: 'contain' })).toEqual([1280, 720]);
	});
});

describe('putThumbnail (the harness mirror of prepare_thumbnail)', () => {
	test('adds with defaults, keeps layers by id and changes only what the spec names', () => {
		const tl = empty();
		const t = putThumbnail(tl, null, { time: 2, layers: [headline('Big news\nline two')] }, uid);
		expect([t.width, t.height, t.focus_x, t.graphics, t.name]).toEqual([1280, 720, 0.5, 'titles', 'Thumbnail 1']);
		const [layer] = t.layers!;
		expect(layer.name).toBe('Big news');
		expect(layerKind(layer)).toBe('text');
		expect(placementOf(layer)).toBe('bottom');
		const changed = putThumbnail(
			tl,
			t.id,
			{ size: 'vertical', layers: [{ svg: '<svg/>' }, { ...keptLayers(t)[0], ...HEADLINE_PLACEMENTS.top }] },
			uid
		);
		expect([changed.width, changed.height, changed.time]).toEqual([1080, 1920, 2]);
		expect(changed.layers!.map(layerKind)).toEqual(['svg', 'text']);
		expect(changed.layers![1].id).toBe(layer.id);
		expect(placementOf(changed.layers![1])).toBe('top');
		expect(tl.thumbnails).toHaveLength(1);
	});

	test('refuses what the engine refuses', () => {
		const tl = empty();
		const t = putThumbnail(tl, null, { layers: [headline('x')] }, uid);
		expect(() => putThumbnail(tl, t.id, { layers: [{}] }, uid)).toThrow('needs');
		expect(() => putThumbnail(tl, t.id, { layers: [{ svg: '<svg/>', ...headline('x') }] }, uid)).toThrow('not several');
		expect(() => putThumbnail(tl, t.id, { layers: [{ layer_id: 'nope' }] }, uid)).toThrow('no layer');
		expect(() => putThumbnail(tl, t.id, { layers: [{ layer_id: t.layers![0].id, values: { a: 'b' } }] }, uid)).toThrow(
			'not a template'
		);
		expect(() => putThumbnail(tl, t.id, { focus_x: 2 }, uid)).toThrow('focus_x');
		expect(() => putThumbnail(tl, t.id, { time: -1 }, uid)).toThrow('time');
		expect(() => putThumbnail(tl, 'gone', {}, uid)).toThrow('no thumbnail');
		const kept = { layer_id: t.layers![0].id };
		expect(() => putThumbnail(tl, t.id, { layers: [kept, kept] }, uid)).toThrow('named twice');
	});

	test('suggestions spread through the cut', () => {
		expect(evenFrames(10, 4).map((f) => f.time)).toEqual([1.25, 3.75, 6.25, 8.75]);
		expect(evenFrames(0, 4)).toEqual([]);
	});
});

describe('thumbnails in the browser harness', () => {
	beforeEach(async () => {
		await revertTo(0);
	});

	test('are edits of the cut that undo, with no picture to draw', async () => {
		const added = await setThumbnail(null, { time: 1, layers: [headline('Hi')] });
		expect(added.timeline.thumbnails?.map((t) => t.id)).toEqual([added.thumbnail_id]);
		const changed = await setThumbnail(added.thumbnail_id, { name: 'YouTube' });
		expect(changed.timeline.thumbnails?.[0].name).toBe('YouTube');
		expect(await thumbnailPreview(added.thumbnail_id, 320)).toBeNull();
		expect((await suggestThumbnailFrames(3)).length).toBe(3);
		const removed = await removeThumbnail(added.thumbnail_id);
		expect(removed.thumbnails).toEqual([]);
		await expect(removeThumbnail(added.thumbnail_id)).rejects.toThrow('no thumbnail');
	});
});
