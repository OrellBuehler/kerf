import { describe, expect, test } from 'bun:test';
import * as ops from './link-ops';
import { MAX_SPEED, MIN_SPEED } from './edit-modes';
import { checkOwnLanes } from './ripple';
import { trimRefusal } from './trim-tools';
import type { Asset, Clip, Timeline } from './types';

// The refusals `Project::add_clip_to_timeline`, `trim` and `set_speed` gained — footage bounds, the lane
// check after the ripple, speed limits — replayed case for case from `project/review_tests.rs`
// ("clips stay in their footage and their lane"), over a 60 s picture-and-sound asset.

const ASSET: Pick<Asset, 'id' | 'name' | 'duration' | 'streams'> = {
	id: 'a',
	name: 'av.mp4',
	duration: 60,
	streams: [
		{ index: 0, kind: 'video' },
		{ index: 1, kind: 'audio' }
	] as Asset['streams']
};
const STILL: typeof ASSET = { id: 's', name: 'still.png', duration: 5, streams: [{ index: 0, kind: 'video', image: true }] as Asset['streams'] };

const clip = (id: string, sourceIn: number, sourceOut: number, start: number, extra: Partial<Clip> = {}): Clip => ({
	id,
	asset_id: 'a',
	source_in: sourceIn,
	source_out: sourceOut,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});

const lane = (...clips: Clip[]): Timeline => ({
	tracks: [
		{ id: 'v1', kind: 'video', name: 'V1', clips },
		{ id: 'a1', kind: 'audio', name: 'A1', clips: [] }
	]
});

/** c1 0..10 at 0 and c2 20..26 at 10 — the Rust `cut()` fixture. */
const cut = () => lane(clip('c1', 0, 10, 0), clip('c2', 20, 26, 10));

const env = (ripple = false, links = true): ops.EditEnv => ({
	ripple,
	links,
	footage: new Map([
		['a', 60],
		['s', Infinity]
	]),
	hasAudio: () => true,
	asset: (id) => (id === 'a' ? ASSET : id === 's' ? STILL : undefined)
});

const starts = (t: Timeline) => t.tracks[0].clips.map((c) => c.timeline_start);

describe('add clip', () => {
	test('stays inside the footage, off its neighbours, and keeps the track sorted', () => {
		const t = lane();
		const e = env();
		for (const [label, call] of [
			['negative in', () => ops.addClip(t, e, ASSET, null, -1, 5)],
			['past the end', () => ops.addClip(t, e, ASSET, null, 0, 61)],
			['negative start', () => ops.addClip(t, e, ASSET, null, 0, 5, -2)],
			['empty', () => ops.addClip(t, e, ASSET, null, 5, 5)],
			['not a number', () => ops.addClip(t, e, ASSET, null, 0, Number.NaN)]
		] as const) {
			expect(call, label).toThrow();
		}
		expect(() => ops.addClip(t, e, ASSET, null, 0, 61)).toThrow(/past the end of "av\.mp4" \(60\.000 s long\)/);
		expect(() => ops.addClip(t, e, ASSET, null, 0, 61)).toThrow(/source_out 61\.000 s/);

		let cur = t;
		for (const [i, o, at] of [
			[0, 10, 20],
			[0, 5, 12],
			[0, 12, 0],
			[55, 60, 40]
		] as const) {
			cur = ops.addClip(cur, e, ASSET, null, i, o, at).timeline;
		}
		expect(starts(cur)).toEqual([0, 12, 20, 40]);
		expect(cur.tracks[0].clips.map((c) => c.id).length).toBe(4);
	});

	test('a clip on another clip is refused, and the timeline is left as it was', () => {
		const t = lane(clip('x', 0, 5, 12), clip('y', 0, 10, 20));
		const before = structuredClone(t);
		expect(() => ops.addClip(t, env(), ASSET, null, 0, 5, 14)).toThrow(/overlap.*V1/);
		expect(t).toEqual(before);
	});

	test('with ripple on the later clips make the room', () => {
		const t = lane(clip('x', 0, 10, 0), clip('y', 0, 10, 10));
		const done = ops.addClip(t, env(true), ASSET, null, 0, 5, 10);
		expect(starts(done.timeline)).toEqual([0, 10, 15]);
	});

	test('the first track of the asset kind is the default; a still has no end to run past', () => {
		const done = ops.addClip(lane(), env(), STILL, null, 0, 30, 0);
		expect(done.timeline.tracks[0].clips[0].source_out).toBe(30);
		expect(() => ops.addClip(lane(), env(), ASSET, 'nope', 0, 5)).toThrow(/track not found/);
	});
});

describe('trim', () => {
	test('stays inside the footage', () => {
		const t = cut();
		expect(() => ops.trim(t, env(), 'c1', -0.5, null, null)).toThrow(/source_in must be >= 0/);
		expect(() => ops.trim(t, env(), 'c2', null, 61, null)).toThrow(/past the end of "av\.mp4"/);
		expect(() => ops.trim(t, env(), 'c1', Number.NaN, null, null)).toThrow(/finite/);
	});

	test('may not run onto the next clip unless ripple makes the room', () => {
		const t = cut();
		const before = structuredClone(t);
		expect(() => ops.trim(t, env(), 'c1', null, 12, null)).toThrow(/overlap.*V1/);
		expect(t).toEqual(before);

		expect(ops.trim(t, env(), 'c1', null, 8, null).timeline.tracks[0].clips[0].source_out).toBe(8);
		expect(ops.trim(t, env(), 'c1', null, 10, null).timeline.tracks[0].clips[0].source_out).toBe(10);

		const rippled = ops.trim(t, env(true), 'c1', null, 12, null).timeline;
		expect(starts(rippled)).toEqual([0, 12]);
	});

	test('a clip that already overlapped its neighbour can still be shortened', () => {
		const t = lane(clip('c1', 0, 10, 0), clip('c2', 20, 26, 8));
		expect(() => ops.trim(t, env(), 'c1', null, 9, null)).not.toThrow();
	});

	test('a still may be trimmed to any length', () => {
		const t: Timeline = { tracks: [{ id: 'v1', kind: 'video', name: 'V1', clips: [clip('p', 0, 5, 0, { asset_id: 's' })] }] };
		expect(() => ops.trim(t, env(), 'p', null, 30, null)).not.toThrow();
	});
});

describe('speed', () => {
	test('stays within the limits', () => {
		const t = cut();
		for (const bad of [0, 0.001, 0.009, 100.5, 1000, -1000, Number.NaN, Number.POSITIVE_INFINITY]) {
			expect(() => ops.setSpeed(t, env(), 'c1', bad), String(bad)).toThrow();
		}
		expect(() => ops.setSpeed(t, env(), 'c1', 1000)).toThrow(/0\.01.*100/);
		for (const ok of [0.01, 0.5, 2, 100, -100, -0.01]) {
			expect(() => ops.setSpeed(t, env(), 'c1', ok), String(ok)).not.toThrow();
		}
		expect([MIN_SPEED, MAX_SPEED]).toEqual([0.01, 100]);
	});

	test('a linked partner is held to the limits too', () => {
		const t: Timeline = {
			tracks: [
				{ id: 'v1', kind: 'video', name: 'V1', clips: [clip('pic', 0, 40, 0, { link_id: 'g', source_audio: false })] },
				{ id: 'a1', kind: 'audio', name: 'A1', clips: [clip('snd', 0, 40, 0, { link_id: 'g', speed: 50 })] }
			]
		};
		// Taking the picture to 4× takes its partner to 200×.
		expect(() => ops.setSpeed(t, env(), 'pic', 4)).toThrow(/linked clip.*200/);
	});
});

describe('checkOwnLanes', () => {
	test('judges the named clips only, and ignores an overlap that was already there', () => {
		const before = lane(clip('a1', 0, 10, 0), clip('b1', 0, 10, 8));
		const after = lane(clip('a1', 0, 10, 0), clip('b1', 0, 10, 8));
		expect(() => checkOwnLanes(after, before, ['a1'])).not.toThrow();
		const clean = lane(clip('a1', 0, 10, 0), clip('b1', 0, 10, 10));
		const moved = lane(clip('a1', 0, 12, 0), clip('b1', 0, 10, 10));
		expect(() => checkOwnLanes(moved, clean, ['a1'])).toThrow(/overlap another clip on V1 at 0:10\.0/);
		expect(() => checkOwnLanes(moved, clean, ['gone'])).not.toThrow();
		expect(() => checkOwnLanes(moved, clean, [])).not.toThrow();
	});
});

describe('trimRefusal', () => {
	test('answers with the backend sentence, or null when the trim goes through', () => {
		const t = cut();
		const world = { ripple: false, assets: [ASSET] };
		expect(trimRefusal(t, world, 'c2', undefined, 61)).toMatch(/^Source_out 61\.000 s is past the end of "av\.mp4"/);
		expect(trimRefusal(t, world, 'c1', undefined, 12)).toMatch(/overlap another clip on V1/);
		expect(trimRefusal(t, { ...world, ripple: true }, 'c1', undefined, 12)).toBeNull();
		expect(trimRefusal(t, world, 'c1', undefined, 8)).toBeNull();
		expect(t).toEqual(cut());
	});
});
