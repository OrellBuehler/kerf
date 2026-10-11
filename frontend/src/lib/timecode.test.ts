import { describe, expect, test } from 'bun:test';
import { formatTimecode, renderedDuration, renderedFps, timelineFps } from './timecode';
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

describe('rendered timebase', () => {
	const fast = { id: 'fast', streams: [{ kind: 'video', fps: 60 }] };
	const slow = { id: 'slow', streams: [{ kind: 'video', fps: 24 }] };
	const both = [slow, fast] as unknown as Asset[];
	const track = (asset: string, extra: object = {}, kind = 'video') => ({ kind, clips: [{ asset_id: asset }], ...extra });
	const cut = (...tracks: object[]) => ({ tracks }) as unknown as Timeline;

	// The twin of `the_rate_is_the_cut_as_it_renders_not_a_muted_tracks_footage` (kerf-core): the
	// engine plays at the rate of `for_render()`, so the page's ladder and gate must start from it.
	test('a muted track\u2019s footage does not set the rate, though timecode still counts it', () => {
		const timeline = cut(track('slow', { muted: true }), track('fast'));
		expect(renderedFps(timeline, both)).toBe(60);
		expect(timelineFps(timeline, both)).toBe(24);
	});

	test('nor does a track a solo shadows, but a soloed one does', () => {
		expect(renderedFps(cut(track('slow'), track('fast', { solo: true })), both)).toBe(60);
		expect(renderedFps(cut(track('slow', { solo: true }), track('fast')), both)).toBe(24);
		// A solo on an audio track shadows only audio tracks.
		expect(renderedFps(cut(track('slow'), track('sound', { solo: true }, 'audio'), track('fast')), [...both, assets[2]])).toBe(24);
	});

	test('nor does a disabled clip', () => {
		const timeline = cut({ kind: 'video', clips: [{ asset_id: 'slow', enabled: false }, { asset_id: 'fast', enabled: true }] });
		expect(renderedFps(timeline, both)).toBe(60);
		expect(renderedFps(cut({ kind: 'video', clips: [{ asset_id: 'slow' }, { asset_id: 'fast' }] }), both)).toBe(24);
	});

	test('is the timeline\u2019s own rate when everything renders, and the engine\u2019s 30 when nothing does', () => {
		expect(renderedFps(timeline(['b', 'a']), assets)).toBe(timelineFps(timeline(['b', 'a']), assets));
		expect(renderedFps(cut(track('slow', { muted: true })), both)).toBe(30);
		expect(renderedFps(cut(), both)).toBe(30);
		expect(renderedFps(timeline(['sound']), assets)).toBe(30);
		for (const fps of [undefined, 0, -1, NaN, Infinity]) {
			expect(renderedFps(timeline(['a']), [{ ...assets[0], streams: [{ kind: 'video', index: 0, codec: 'h264', fps }] }])).toBe(30);
		}
	});
});

describe('rendered duration', () => {
	// The twin of `Timeline::for_render().duration()`: the end of the last enabled clip on a track that
	// renders. A stream may not start at or past it, and the timeline's own length also counts what is
	// muted, shadowed by a solo or disabled.
	const clip = (start: number, length: number, extra: object = {}) => ({ asset_id: 'a', source_in: 0, source_out: length, timeline_start: start, ...extra });
	const cut = (...tracks: object[]) => ({ tracks }) as unknown as Timeline;

	test('is the end of the last clip when everything renders, speed included', () => {
		expect(renderedDuration(cut({ kind: 'video', clips: [clip(0, 4), clip(4, 6)] }, { kind: 'audio', clips: [clip(0, 9)] }))).toBe(10);
		expect(renderedDuration(cut({ kind: 'video', clips: [clip(2, 8, { speed: 2 })] }))).toBe(6);
		expect(renderedDuration(cut())).toBe(0);
	});

	test('does not count a muted track, a track a solo shadows, or a disabled clip', () => {
		expect(renderedDuration(cut({ kind: 'video', clips: [clip(0, 4)] }, { kind: 'video', muted: true, clips: [clip(0, 30)] }))).toBe(4);
		expect(renderedDuration(cut({ kind: 'video', clips: [clip(0, 4)] }, { kind: 'video', solo: true, clips: [clip(0, 7)] }))).toBe(7);
		expect(renderedDuration(cut({ kind: 'video', clips: [clip(0, 4), clip(4, 20, { enabled: false })] }))).toBe(4);
		expect(renderedDuration(cut({ kind: 'video', clips: [clip(0, 4)] }, { kind: 'audio', solo: true, clips: [clip(0, 12)] }))).toBe(12);
		expect(renderedDuration(cut({ kind: 'video', muted: true, clips: [clip(0, 4)] }))).toBe(0);
	});
});

describe('formatTimecode', () => {
	test('counts every source frame at 24, 25, 30 and 60 fps', () => {
		for (const fps of [24,25,30,60]) {
			for (let frame=0; frame<fps*3; frame++) {
				expect(formatTimecode(frame/fps, fps)).toBe(`00:00:${String(Math.floor(frame/fps)).padStart(2,'0')}:${String(frame%fps).padStart(2,'0')}`);
			}
		}
	});
	test('is HH:MM:SS:FF with the hours always there, so 20 s of cut is not read as minutes', () => {
		expect(formatTimecode(20.5, 30)).toBe('00:00:20:15');
		expect(formatTimecode(0, 30)).toBe('00:00:00:00');
		expect(formatTimecode(3723 + 12 / 30, 30)).toBe('01:02:03:12');
	});
	test('uses non-drop frame numbering for fractional rates', () => {
		expect(formatTimecode(30/(30000/1001), 30000/1001)).toBe('00:00:01:00');
		expect(formatTimecode(60, 30000/1001)).toBe('00:00:59:28');
	});
	test('handles minute and hour boundaries and invalid input', () => {
		expect(formatTimecode(60, 30)).toBe('00:01:00:00');
		expect(formatTimecode(3600, 30)).toBe('01:00:00:00');
		expect(formatTimecode(36000, 30)).toBe('10:00:00:00');
		for (const t of [-1, NaN, Infinity]) expect(formatTimecode(t,30)).toBe('00:00:00:00');
		expect(formatTimecode(0.5,0)).toBe('00:00:00:15');
	});
	test('an empty cut is zero in every field, not the lane\'s eight-second minimum', () => {
		expect(formatTimecode(0, 24)).toBe('00:00:00:00');
	});
	test('without frames it is the same count cut at the second', () => {
		expect(formatTimecode(5, 30, { frames: false })).toBe('00:00:05');
		expect(formatTimecode(65.9, 30, { frames: false })).toBe('00:01:05');
		// 5 timecode seconds at 29.97 is 150 frames, a hair past 5 s of the clock
		expect(formatTimecode(150 / (30000 / 1001), 30000 / 1001, { frames: false })).toBe('00:00:05');
		expect(formatTimecode(5, 30000 / 1001, { frames: false })).toBe('00:00:04');
	});
});
