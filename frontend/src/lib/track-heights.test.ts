import { afterEach, describe, expect, test } from 'bun:test';
import {
	DEFAULT_PRESET,
	HEIGHTS_KEY,
	HEIGHT_PRESETS,
	MAX_REMEMBERED,
	MINIMAP_KEY,
	MIN_TITLE_LANE_PX,
	MIXER_MIN_PX,
	PRESET_PX,
	TITLE_ADD_BTN_PX,
	TITLE_METRICS,
	heightOf,
	heightPx,
	isPreset,
	loadHeights,
	loadMinimap,
	parseHeights,
	saveHeights,
	saveMinimap,
	serializeHeights,
	setAll,
	setTrack,
	stepPreset,
	titleLaneHeight,
	uniformPreset,
	type TrackHeights
} from './track-heights';
import { filmVisible } from './filmstrip-view';
import { STEREO_MIN_HEIGHT, laneCount } from './waveform-view';
import { CLIP_INSET_PX } from './marquee';

const fresh = (): TrackHeights => ({ all: 'medium', tracks: {} });

describe('the presets', () => {
	test('medium is what the timeline always was (64 px) and the three are in order', () => {
		expect(DEFAULT_PRESET).toBe('medium');
		expect(PRESET_PX).toEqual({ compact: 32, medium: 64, large: 112 });
		expect(HEIGHT_PRESETS).toEqual(['compact', 'medium', 'large']);
	});

	test('the clip box a lane leaves is what decides stereo lanes and thumbnails', () => {
		// A clip is drawn `CLIP_INSET_PX` in from the lane at top and bottom.
		const box = (p: keyof typeof PRESET_PX) => PRESET_PX[p] - 2 * CLIP_INSET_PX;
		// compact: one folded lane, no thumbnails
		expect(laneCount(2, box('compact'))).toBe(1);
		expect(filmVisible(box('compact'))).toBe(false);
		// medium: the stereo waveform and the filmstrip both fit
		expect(box('medium')).toBeGreaterThanOrEqual(STEREO_MIN_HEIGHT);
		expect(laneCount(2, box('medium'))).toBe(2);
		expect(filmVisible(box('medium'))).toBe(true);
		// large: both, with room to spare
		expect(laneCount(2, box('large'))).toBe(2);
		expect(filmVisible(box('large'))).toBe(true);
	});

	test('the mixer strip is kept where it fits: medium and large, not compact', () => {
		expect(PRESET_PX.compact).toBeLessThan(MIXER_MIN_PX);
		expect(PRESET_PX.medium).toBeGreaterThanOrEqual(MIXER_MIN_PX);
		expect(PRESET_PX.large).toBeGreaterThanOrEqual(MIXER_MIN_PX);
	});

	test('the titles lane follows the preset, and medium is today\'s 22 px rows with 4 px padding', () => {
		expect(TITLE_METRICS.medium).toMatchObject({ row: 22, pad: 4 });
		expect(titleLaneHeight('medium', 1)).toBe(30);
		expect(titleLaneHeight('medium', 3)).toBe(74);
		expect(titleLaneHeight('compact', 1)).toBeLessThan(titleLaneHeight('medium', 1));
		expect(titleLaneHeight('large', 1)).toBeGreaterThan(titleLaneHeight('medium', 1));
		// no rows is still one row's room (the lane shows its hint)
		expect(titleLaneHeight('medium', 0)).toBe(titleLaneHeight('medium', 1));
	});

	test('the titles lane is never too short for its header\'s add button (plus the lane\'s border)', () => {
		for (const p of HEIGHT_PRESETS) {
			for (const rows of [0, 1, 2, 5]) {
				expect(titleLaneHeight(p, rows)).toBeGreaterThanOrEqual(TITLE_ADD_BTN_PX + 1);
			}
		}
		// the compact lane sits right at that floor, with its one row centred in it
		const c = TITLE_METRICS.compact;
		expect(titleLaneHeight('compact', 1)).toBe(c.row + c.pad * 2);
		expect(titleLaneHeight('compact', 1)).toBe(MIN_TITLE_LANE_PX);
	});

	test('isPreset and stepPreset', () => {
		expect(isPreset('large')).toBe(true);
		expect(isPreset('huge')).toBe(false);
		expect(isPreset(64)).toBe(false);
		expect(stepPreset('medium', 1)).toBe('large');
		expect(stepPreset('medium', -1)).toBe('compact');
		expect(stepPreset('large', 1)).toBe('large');
		expect(stepPreset('compact', -1)).toBe('compact');
	});
});

describe('setting heights', () => {
	test('a track with no choice of its own follows the global one', () => {
		const h = fresh();
		expect(heightOf(h, 'v1')).toBe('medium');
		expect(heightPx(h, 'v1')).toBe(64);
		expect(heightOf({ all: 'large', tracks: {} }, 'v1')).toBe('large');
	});

	test('one track is set without touching the others', () => {
		const h = setTrack(fresh(), 'v1', 'large');
		expect(heightOf(h, 'v1')).toBe('large');
		expect(heightPx(h, 'v1')).toBe(112);
		expect(heightOf(h, 'a1')).toBe('medium');
		expect(h.all).toBe('medium');
	});

	test('setting a track to the global height drops its override', () => {
		let h = setTrack(fresh(), 'v1', 'compact');
		expect(h.tracks).toEqual({ v1: 'compact' });
		h = setTrack(h, 'v1', 'medium');
		expect(h.tracks).toEqual({});
	});

	test('is pure: the input is never changed', () => {
		const h = fresh();
		const next = setTrack(h, 'v1', 'large');
		expect(h).toEqual(fresh());
		expect(next).not.toBe(h);
		const all = setAll(next, 'compact');
		expect(next.tracks).toEqual({ v1: 'large' });
		expect(all).toEqual({ all: 'compact', tracks: {} });
	});

	test('"all tracks" moves the global height and forgets every exception', () => {
		let h = setTrack(setTrack(fresh(), 'v1', 'large'), 'a1', 'compact');
		h = setAll(h, 'large');
		expect(h).toEqual({ all: 'large', tracks: {} });
		expect(heightOf(h, 'v1')).toBe('large');
		expect(heightOf(h, 'a1')).toBe('large');
		expect(heightOf(h, 'a-track-added-later')).toBe('large');
	});

	test('a track set to the new global height after "all" is not an exception', () => {
		const h = setTrack(setAll(fresh(), 'compact'), 'v1', 'compact');
		expect(h.tracks).toEqual({});
		expect(setTrack(h, 'v1', 'large').tracks).toEqual({ v1: 'large' });
	});

	test('the table is capped: the oldest choice goes first, a re-set one is the newest', () => {
		let h = fresh();
		for (let i = 0; i < MAX_REMEMBERED + 10; i++) h = setTrack(h, `t${i}`, 'large');
		const ids = Object.keys(h.tracks);
		expect(ids).toHaveLength(MAX_REMEMBERED);
		expect(ids[0]).toBe('t10');
		expect(ids[ids.length - 1]).toBe(`t${MAX_REMEMBERED + 9}`);
		h = setTrack(h, 't10', 'compact'); // chosen again: now the newest
		expect(Object.keys(h.tracks).at(-1)).toBe('t10');
		expect(Object.keys(h.tracks)).toHaveLength(MAX_REMEMBERED);
	});

	test('uniformPreset names the one height every track is at, or null', () => {
		const ids = ['v1', 'v2', 'a1'];
		expect(uniformPreset(fresh(), ids)).toBe('medium');
		expect(uniformPreset(setAll(fresh(), 'large'), ids)).toBe('large');
		expect(uniformPreset(setTrack(fresh(), 'a1', 'compact'), ids)).toBeNull();
		expect(uniformPreset(fresh(), [])).toBeNull();
		// every track set one by one to the same non-global height is uniform too
		const each = ids.reduce((h, id) => setTrack(h, id, 'compact'), fresh());
		expect(uniformPreset(each, ids)).toBe('compact');
	});
});

describe('parseHeights', () => {
	test('nothing stored is the defaults', () => {
		expect(parseHeights(null)).toEqual(fresh());
		expect(parseHeights(undefined)).toEqual(fresh());
		expect(parseHeights('')).toEqual(fresh());
	});

	test('garbage is the defaults, not an error', () => {
		for (const raw of ['{', 'null', '[]', '"large"', '42', 'true', '{"all":7,"tracks":3}']) {
			expect(parseHeights(raw)).toEqual(fresh());
		}
	});

	test('a good value round-trips', () => {
		const h: TrackHeights = { all: 'large', tracks: { v1: 'compact', a1: 'medium' } };
		expect(parseHeights(serializeHeights(h))).toEqual(h);
	});

	test('a bad entry costs only itself', () => {
		const raw = JSON.stringify({
			all: 'compact',
			tracks: { good: 'large', unknown: 'huge', number: 64, nul: null, '': 'large', same: 'compact' }
		});
		// `same` equals the global height, so it is not an exception; the rest are dropped
		expect(parseHeights(raw)).toEqual({ all: 'compact', tracks: { good: 'large' } });
	});

	test('an unknown global height is the default, keeping the valid overrides', () => {
		expect(parseHeights('{"all":"gigantic","tracks":{"v1":"large"}}')).toEqual({ all: 'medium', tracks: { v1: 'large' } });
	});

	test('an oversized table is cut to the newest entries', () => {
		const tracks = Object.fromEntries(Array.from({ length: MAX_REMEMBERED + 5 }, (_, i) => [`t${i}`, 'large']));
		const h = parseHeights(JSON.stringify({ all: 'medium', tracks }));
		expect(Object.keys(h.tracks)).toHaveLength(MAX_REMEMBERED);
		expect(h.tracks['t0']).toBeUndefined();
		expect(h.tracks[`t${MAX_REMEMBERED + 4}`]).toBe('large');
	});
});

/** A `localStorage` the tests can hand the module, or break. */
function fakeStorage(opts: { throws?: boolean } = {}) {
	const data = new Map<string, string>();
	const store = {
		getItem(k: string) {
			if (opts.throws) throw new Error('blocked');
			return data.get(k) ?? null;
		},
		setItem(k: string, v: string) {
			if (opts.throws) throw new Error('blocked');
			data.set(k, v);
		}
	};
	(globalThis as unknown as { localStorage: unknown }).localStorage = store;
	return data;
}

afterEach(() => {
	delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
});

describe('persistence', () => {
	test('saved heights come back on the next load', () => {
		const data = fakeStorage();
		const h = setTrack(setAll(fresh(), 'large'), 'a1', 'compact');
		saveHeights(h);
		expect(data.has(HEIGHTS_KEY)).toBe(true);
		expect(loadHeights()).toEqual(h);
	});

	test('nothing saved is the defaults', () => {
		fakeStorage();
		expect(loadHeights()).toEqual(fresh());
	});

	test('a blocked or missing store means defaults and never throws', () => {
		fakeStorage({ throws: true });
		expect(loadHeights()).toEqual(fresh());
		expect(() => saveHeights(fresh())).not.toThrow();
		delete (globalThis as unknown as { localStorage?: unknown }).localStorage; // no store at all
		expect(loadHeights()).toEqual(fresh());
		expect(() => saveHeights(fresh())).not.toThrow();
		expect(loadMinimap()).toBe(true);
		expect(() => saveMinimap(false)).not.toThrow();
	});

	test('the minimap is on until it is turned off, and that is remembered', () => {
		const data = fakeStorage();
		expect(loadMinimap()).toBe(true);
		saveMinimap(false);
		expect(data.get(MINIMAP_KEY)).toBe('0');
		expect(loadMinimap()).toBe(false);
		saveMinimap(true);
		expect(loadMinimap()).toBe(true);
	});
});
