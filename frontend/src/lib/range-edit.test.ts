import { describe, expect, test } from 'bun:test';
import { clipById, dissolveAllOrphans, firstSyncBreak, linkClips } from './link-groups';
import * as ops from './link-ops';
import { splitClip } from './links';
import { conformLinks } from './ripple';
import { extractRange, liftRange, rippleDeleteGap, splitAllTracks, type RangeCtx } from './range-edit';
import { timelineToSource } from './speed-ramp';
import { transformAt } from './channels';
import type { Clip, Keyframe, StreamKind, Timeline, Track } from './types';
import { clipDuration, GENERATED_ASSET } from './types';

// `range-edit.ts` is the port of kerf-core's `model/range_edit.rs`, and `link-ops.ts` of `project/range_ops.rs` around
// it. These replay the Rust unit tests (`mod tests` of `range_edit.rs`, `range_ops_tests.rs`) case for case, inputs,
// outputs and refusal sentences included, so a rule changed in Rust and not here names the test it broke.
// `links-corpus.test.ts` replays kerf-core's own answers for the same ops.

let seq = 0;
/** One frame at 30 fps, as the engine's default rate gives it. */
const FRAME = 1 / 30;
/** Linked partners follow, at 30 fps; and with the links off. */
const RULES: RangeCtx = { links: true, frame: FRAME };
const ALONE: RangeCtx = { ...RULES, links: false };
const ASSET = 'asset';
const clip = (asset: string, si: number, so: number, at: number, extra: Partial<Clip> = {}): Clip => ({
	id: `clip-${++seq}`,
	asset_id: asset,
	source_in: si,
	source_out: so,
	timeline_start: at,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});
const V: StreamKind = 'video';
const A: StreamKind = 'audio';
const lane = (kind: StreamKind, name: string, clips: Clip[], extra: Partial<Track> = {}): Track => ({
	id: `track-${name}-${++seq}`,
	kind,
	name,
	clips,
	...extra
});
const timeline = (tracks: Track[]): Timeline => ({ tracks });
const span = (c: Clip): [number, number] => [c.timeline_start, c.timeline_start + clipDuration(c)];
const near = (a: number, b: number) => expect(Math.abs(a - b)).toBeLessThan(1e-9);
const spansOf = (t: Timeline, track: number) => t.tracks[track].clips.map(span);
const assertSpans = (t: Timeline, track: number, want: [number, number][]) => {
	const got = spansOf(t, track);
	expect(got.length).toBe(want.length);
	got.forEach((g, i) => {
		near(g[0], want[i][0]);
		near(g[1], want[i][1]);
	});
};
const same = (a: Timeline, b: Timeline) => JSON.stringify(a) === JSON.stringify(b);
const id = (t: Timeline, track: number, i: number) => t.tracks[track].clips[i].id;
const get = (t: Timeline, clipId: string): Clip => {
	const found = t.tracks.flatMap((x) => x.clips).find((c) => c.id === clipId);
	if (!found) throw new Error(`the clip ${clipId} is not on the timeline`);
	return found;
};
/** The message a refusal carries. */
function refused(run: () => unknown): string {
	try {
		run();
	} catch (e) {
		return (e as Error).message;
	}
	throw new Error('expected a refusal');
}
/** Where `source_in` would sit if the clip played at 1x from the timeline's 0: equal for two forward clips of one
 *  asset exactly when they are in step. */
const offset = (c: Clip) => c.timeline_start - c.source_in;

/** A picture and its sound, one linked group: the same footage at the same place. */
function pairOf(asset: string, [from, to]: [number, number]): [Timeline, string, string] {
	const t = timeline([lane(V, 'V1', [clip(asset, from, to, from)]), lane(A, 'A1', [clip(asset, from, to, from)])]);
	const [c, a] = [id(t, 0, 0), id(t, 1, 0)];
	linkClips(t, [c, a]);
	return [t, c, a];
}

// ---- cut every track ------------------------------------------------------------

describe('splitAllTracks', () => {
	test('every unlocked track with a clip under the time is cut in one call', () => {
		const asset = 'a';
		const t = timeline([
			lane(V, 'V1', [clip(asset, 0, 10, 0)]),
			lane(V, 'V2', [clip(asset, 0, 6, 2)]),
			lane(A, 'A1', [clip(asset, 0, 10, 0)]),
			lane(A, 'A2', [clip(asset, 0, 3, 0)])
		]);
		const done = splitAllTracks(t, 5, null, RULES);
		expect(done.cuts.length).toBe(3);
		expect(done.skipped).toEqual([]);
		assertSpans(t, 0, [[0, 5], [5, 10]]);
		assertSpans(t, 1, [[2, 5], [5, 8]]);
		assertSpans(t, 2, [[0, 5], [5, 10]]);
		assertSpans(t, 3, [[0, 3]]);
		for (const cut of done.cuts) {
			near(span(cut.left)[1], 5);
			near(cut.right.timeline_start, 5);
			expect(cut.left.id).not.toBe(cut.right.id);
		}
		expect(done.cuts.map((c) => c.track_id)).toEqual([t.tracks[0].id, t.tracks[1].id, t.tracks[2].id]);
	});

	test('a time on an existing cut is no cut for that track', () => {
		const asset = 'a';
		const t = timeline([
			lane(V, 'V1', [clip(asset, 0, 5, 0), clip(asset, 5, 10, 5)]),
			lane(V, 'V2', [clip(asset, 0, 10, 0)])
		]);
		const done = splitAllTracks(t, 5, null, RULES);
		expect(done.cuts.length).toBe(1);
		expect(t.tracks[0].clips.length).toBe(2);
		assertSpans(t, 1, [[0, 5], [5, 10]]);
	});

	test('nothing to cut is a refusal that says why and changes nothing', () => {
		const asset = 'a';
		const t = timeline([
			lane(V, 'V1', [clip(asset, 0, 5, 0), clip(asset, 5, 10, 5)]),
			lane(A, 'A1', [clip(asset, 0, 10, 0)], { locked: true })
		]);
		const ask = (at: number) => {
			const copy = structuredClone(t);
			const why = refused(() => splitAllTracks(copy, at, null, RULES));
			expect(same(copy, t)).toBe(true);
			return why;
		};
		const onACut = ask(5);
		expect(onACut).toContain('already a cut');
		expect(onACut).toContain('locked tracks (A1)');
		expect(ask(20)).toContain('no clip on an unlocked track is under that time');
		expect(ask(-3)).toContain('no clip on an unlocked track');
		expect(ask(0)).toContain('already a cut');
		for (const bad of [NaN, Infinity, -Infinity]) expect(ask(bad)).toContain('finite');

		expect(refused(() => splitAllTracks(timeline([]), 1, null, RULES))).toContain('no unlocked track');
		const blank = timeline([lane(V, 'V1', []), lane(A, 'A1', [])]);
		expect(refused(() => splitAllTracks(blank, 1, null, RULES))).toContain('no clip on an unlocked track');
	});

	test('locked tracks are left alone and reported', () => {
		const asset = 'a';
		const t = timeline([
			lane(V, 'V1', [clip(asset, 0, 10, 0)]),
			lane(A, 'A1', [clip(asset, 0, 10, 0)], { locked: true }),
			lane(A, 'A2', [clip(asset, 0, 2, 20)], { locked: true })
		]);
		const done = splitAllTracks(t, 4, null, RULES);
		expect(done.cuts.length).toBe(1);
		expect(done.skipped).toEqual([{ track_id: t.tracks[1].id, name: 'A1' }]);
		assertSpans(t, 1, [[0, 10]]);
	});

	test('named tracks limit the cut and must be unlocked', () => {
		const asset = 'a';
		const t = timeline([
			lane(V, 'V1', [clip(asset, 0, 10, 0)]),
			lane(V, 'V2', [clip(asset, 0, 10, 0)]),
			lane(A, 'A1', [clip(asset, 0, 10, 0)])
		]);
		const v2 = t.tracks[1].id;
		const done = splitAllTracks(t, 4, [v2, v2], RULES);
		expect(done.cuts.length).toBe(1);
		expect([t.tracks[0].clips.length, t.tracks[1].clips.length, t.tracks[2].clips.length]).toEqual([1, 2, 1]);

		const before = structuredClone(t);
		expect(refused(() => splitAllTracks(t, 4, ['missing'], RULES))).toStartWith('track not found: ');
		expect(refused(() => splitAllTracks(t, 4, [], RULES))).toContain('name at least one track');
		t.tracks[0].locked = true;
		const v1 = t.tracks[0].id;
		expect(refused(() => splitAllTracks(t, 2, [v1], RULES))).toContain('track V1 is locked');
		delete t.tracks[0].locked;
		expect(same(t, before)).toBe(true);
	});

	test('linked partners are cut with the clip and the halves linked by side', () => {
		const [t0, x, s] = pairOf('a', [0, 10]);
		const group = get(t0, x).link_id;
		for (const tracks of [null, [t0.tracks[0].id]]) {
			const t = structuredClone(t0);
			const done = splitAllTracks(t, 4, tracks, RULES);
			expect(done.cuts.length).toBe(2);
			expect(get(t, x).link_id).toBe(group);
			expect(get(t, s).link_id).toBe(group);
			const [rx, rs] = [id(t, 0, 1), id(t, 1, 1)];
			const fresh = get(t, rx).link_id;
			expect(fresh && fresh !== group).toBeTruthy();
			expect(get(t, rs).link_id).toBe(fresh);
		}
		const alone = structuredClone(t0);
		splitAllTracks(alone, 4, [t0.tracks[0].id], ALONE);
		expect([alone.tracks[0].clips.length, alone.tracks[1].clips.length]).toEqual([2, 1]);
	});

	test('a partner on a locked track refuses the cut and changes nothing', () => {
		const [t] = pairOf('a', [0, 10]);
		t.tracks[1].locked = true;
		const before = structuredClone(t);
		expect(refused(() => splitAllTracks(t, 4, null, RULES))).toContain('locked track A1');
		expect(same(t, before)).toBe(true);
		splitAllTracks(t, 4, null, ALONE);
		expect([t.tracks[0].clips.length, t.tracks[1].clips.length]).toEqual([2, 1]);
	});
});

// ---- every kind of clip ---------------------------------------------------------------

const kf = (time: number, scale: number): Keyframe => ({
	time,
	scale,
	pos_x: 0.1 * time,
	pos_y: 0,
	rotation: 0,
	opacity: 1,
	easing: 'linear'
});
const keyed = (at: number, source: [number, number]): Clip =>
	clip('keyed', source[0], source[1], at, { keyframes: [kf(0, 1), kf(3, 2), kf(6, 0.5)] });

/** One clip of every kind the cut has to keep honest. */
function flavours(): [string, Clip][] {
	const asset = 'flavour';
	const ramp = [
		{ time: 0, speed: 1 },
		{ time: 4, speed: 3 },
		{ time: 6, speed: 0.5 }
	];
	return [
		['plain', clip(asset, 2, 12, 1)],
		['2x', clip(asset, 2, 12, 1, { speed: 2 })],
		['0.5x', clip(asset, 2, 12, 1, { speed: 0.5 })],
		['reversed', clip(asset, 2, 12, 1, { speed: -1 })],
		['reversed 2x', clip(asset, 2, 12, 1, { speed: -2 })],
		['ramped', clip(asset, 2, 12, 1, { speed_ramp: structuredClone(ramp) })],
		['ramped reversed', clip(asset, 2, 12, 1, { speed: -1, speed_ramp: structuredClone(ramp) })],
		['keyed', keyed(1, [2, 12])],
		[
			'keyed and ramped',
			{
				...keyed(1, [2, 12]),
				speed_ramp: [
					{ time: 0, speed: 2 },
					{ time: 2, speed: 1 }
				]
			}
		],
		[
			'faded',
			clip(asset, 2, 12, 1, { fade_in: 1, fade_out: 1.5, transition_in: { kind: 'crossfade', duration: 0.5 } })
		],
		[
			'title',
			clip(GENERATED_ASSET, 0, 9, 1, { generator: { kind: 'title', name: 'T', svg: '<svg/>' } })
		]
	];
}

test('a cut makes the halves a split makes', () => {
	for (const [name, c] of flavours()) {
		const at = c.timeline_start + clipDuration(c) * 0.37;
		const t = timeline([lane(V, 'V1', [structuredClone(c)])]);
		const done = splitAllTracks(t, at, null, RULES);
		const plain = timeline([lane(V, 'V1', [structuredClone(c)])]);
		const [left, right] = splitClip(plain, c.id, at);
		const cut = done.cuts[0];
		const wire = (x: Clip, newId = '') => JSON.stringify({ ...x, id: newId || x.id });
		expect(wire(cut.left), `${name}: the left half`).toBe(wire(left));
		expect(wire(cut.right, 'nil'), `${name}: the right half`).toBe(wire(right, 'nil'));
	}
});

// ---- lift and extract ------------------------------------------------------------------

describe('liftRange and extractRange', () => {
	test('a lift removes the range and leaves the gap', () => {
		const asset = 'a';
		const t = timeline([
			lane(V, 'V1', [clip(asset, 0, 4, 0), clip(asset, 10, 14, 4), clip(asset, 20, 24, 8)])
		]);
		const [a, b, c] = [id(t, 0, 0), id(t, 0, 1), id(t, 0, 2)];
		const done = liftRange(t, 3, 9, null, RULES);
		assertSpans(t, 0, [[0, 3], [9, 12]]);
		expect([done.removed, done.trimmed, done.split, done.moved]).toEqual([1, 2, 0, 0]);
		expect([done.from, done.to, done.closed]).toEqual([3, 9, 0]);
		expect(done.removed + done.trimmed + done.split).toBe(3);
		expect(done.tracks).toEqual([t.tracks[0].id]);
		expect(t.tracks[0].clips.find((x) => x.id === b)).toBeUndefined();
		const [keptA, keptC] = [get(t, a), get(t, c)];
		expect([keptA.source_in, keptA.source_out]).toEqual([0, 3]);
		expect([keptC.source_in, keptC.source_out]).toEqual([21, 24]);
	});

	test('a range inside one clip cuts it in two around it', () => {
		const t = timeline([lane(V, 'V1', [clip('a', 5, 15, 0)])]);
		const whole = id(t, 0, 0);
		const done = liftRange(t, 3, 6, null, RULES);
		expect([done.removed, done.trimmed, done.split]).toEqual([0, 0, 1]);
		assertSpans(t, 0, [[0, 3], [6, 10]]);
		const [left, right] = t.tracks[0].clips;
		expect(left.id).toBe(whole);
		expect(right.id).not.toBe(whole);
		expect([left.source_in, left.source_out]).toEqual([5, 8]);
		expect([right.source_in, right.source_out]).toEqual([11, 15]);
	});

	test('an extract closes the gap on every track and welds the cut', () => {
		const asset = 'a';
		const t = timeline([
			lane(V, 'V1', [clip(asset, 0, 4, 0), clip(asset, 10, 14, 4), clip(asset, 20, 24, 8)]),
			lane(A, 'A1', [clip(asset, 0, 12, 0)])
		]);
		const done = extractRange(t, 3, 6, null, RULES);
		assertSpans(t, 0, [[0, 3], [3, 5], [5, 9]]);
		assertSpans(t, 1, [[0, 3], [3, 9]]);
		expect(t.tracks[0].clips[1].timeline_start).toBe(3);
		expect(t.tracks[1].clips[1].timeline_start).toBe(3);
		expect([done.removed, done.trimmed, done.split, done.moved]).toEqual([0, 2, 1, 3]);
		expect(done.closed).toBe(3);
		expect(done.tracks).toEqual([t.tracks[0].id, t.tracks[1].id]);
	});

	test('extract welds on numbers that do not subtract cleanly', () => {
		const t = timeline([lane(V, 'V1', [clip('a', 0, 3, 0), clip('a', 0, 1, 3)])]);
		// 1.3 - (1.3 - 0.1) is 0.10000000000000009, the weld is 0.1.
		extractRange(t, 0.1, 1.3, null, RULES);
		expect(t.tracks[0].clips[1].timeline_start).toBe(0.1);
		near(span(t.tracks[0].clips[1])[1], 1.8);
		near(t.tracks[0].clips[2].timeline_start, 1.8);
	});

	test('an empty stretch has nothing to lift but can be closed when something follows', () => {
		const t = timeline([lane(V, 'V1', [clip('a', 0, 2, 0), clip('a', 0, 2, 5)])]);
		const before = structuredClone(t);
		expect(refused(() => liftRange(t, 2.5, 4.5, null, RULES))).toContain('no clip on a targeted track');
		expect(same(t, before)).toBe(true);
		const done = extractRange(t, 2, 5, null, RULES);
		expect([done.removed + done.trimmed + done.split, done.moved, done.closed]).toEqual([0, 1, 3]);
		assertSpans(t, 0, [[0, 2], [2, 4]]);
		expect(refused(() => extractRange(t, 10, 12, null, RULES))).toContain('none follows it');
	});

	test('hostile ranges are refused and change nothing', () => {
		const t = timeline([lane(V, 'V1', [clip('a', 0, 10, 0)])]);
		const bad: [number, number, string][] = [
			[NaN, 1, 'finite'],
			[1, NaN, 'finite'],
			[Infinity, 2, 'finite'],
			[1, Infinity, 'finite'],
			[-Infinity, 1, 'finite'],
			[5, 2, 'before it starts'],
			[2, 2, 'empty'],
			[2, 2 + 1e-9, 'empty'],
			[-5, -1, 'before the start of the timeline'],
			[-5, 0, 'before the start of the timeline']
		];
		for (const [from, to, why] of bad) {
			for (const close of [false, true]) {
				const copy = structuredClone(t);
				const got = refused(() => (close ? extractRange(copy, from, to, null, RULES) : liftRange(copy, from, to, null, RULES)));
				expect(got, `(${from}, ${to})`).toContain(why);
				expect(same(copy, t)).toBe(true);
			}
		}
		// A start before 0 is held at 0; an end far past everything lifts to the end.
		let copy = structuredClone(t);
		const done = liftRange(copy, -5, 3, null, RULES);
		expect(done.from).toBe(0);
		assertSpans(copy, 0, [[3, 10]]);
		copy = structuredClone(t);
		extractRange(copy, 4, 1e300, null, RULES);
		assertSpans(copy, 0, [[0, 4]]);
		// Nothing at all, and no tracks.
		const blank = timeline([lane(V, 'V1', [])]);
		expect(() => liftRange(blank, 0, 5, null, RULES)).toThrow();
		expect(() => extractRange(blank, 0, 5, null, RULES)).toThrow();
		expect(refused(() => extractRange(timeline([]), 0, 5, null, RULES))).toContain('no unlocked track');
	});

	test('locked tracks are skipped when not named and refused when named', () => {
		const t = timeline([
			lane(V, 'V1', [clip('a', 0, 10, 0)]),
			lane(V, 'V2', [clip('a', 0, 10, 0)], { locked: true })
		]);
		const locked = t.tracks[1].id;
		const before = structuredClone(t);
		expect(refused(() => liftRange(t, 2, 4, [locked], RULES))).toContain('track V2 is locked');
		expect(refused(() => liftRange(t, 2, 4, ['missing'], RULES))).toStartWith('track not found: ');
		expect(same(t, before)).toBe(true);
		const done = extractRange(t, 2, 4, null, RULES);
		assertSpans(t, 0, [[0, 2], [2, 8]]);
		assertSpans(t, 1, [[0, 10]]);
		expect(done.skipped.length).toBe(1);
		expect(done.skipped[0].name).toBe('V2');
		// Everything locked: nothing to edit.
		t.tracks[0].locked = true;
		expect(refused(() => liftRange(t, 2, 4, null, RULES))).toContain('no unlocked track');
	});

	test('a partner on a locked track refuses a range it reaches and not one it misses', () => {
		const [t, x] = pairOf('a', [0, 10]);
		t.tracks[1].locked = true;
		const before = structuredClone(t);
		for (const close of [false, true]) {
			const why = refused(() => (close ? extractRange(t, 4, 6, null, RULES) : liftRange(t, 4, 6, null, RULES)));
			expect(why).toContain('locked track A1');
			expect(same(t, before)).toBe(true);
		}
		// With links off the picture alone is cut.
		liftRange(t, 4, 6, null, ALONE);
		assertSpans(t, 0, [[0, 4], [6, 10]]);
		assertSpans(t, 1, [[0, 10]]);
		expect(get(t, x).link_id).toBe(get(before, x).link_id);

		// A sound that ends before the range is not reached by it.
		const [t2, , s] = pairOf('a', [0, 10]);
		get(t2, s).source_out = 3;
		t2.tracks[1].locked = true;
		liftRange(t2, 4, 6, null, RULES);
		assertSpans(t2, 1, [[0, 3]]);
	});

	test('a linked group falls in two by side and stays in step', () => {
		const [t0, x, s] = pairOf('a', [0, 10]);
		const group = get(t0, x).link_id;
		for (const close of [false, true]) {
			const t = structuredClone(t0);
			if (close) extractRange(t, 4, 6, null, RULES);
			else liftRange(t, 4, 6, null, RULES);
			expect(get(t, x).link_id).toBe(group);
			expect(get(t, s).link_id).toBe(group);
			const [rx, rs] = [t.tracks[0].clips[1], t.tracks[1].clips[1]];
			expect(rx.link_id && rx.link_id !== group && rx.link_id === rs.link_id).toBeTruthy();
			near(offset(rx), offset(rs));
			near(offset(rx), close ? -2 : 0);
			expect(rx.timeline_start).toBe(close ? 4 : 6);
			expect(firstSyncBreak(t, t0)).toBeNull();
		}
	});

	test('a J-cut pair is cut and moved in step', () => {
		// The sound leads the picture by two seconds: same footage, same offset, longer.
		const asset = 'a';
		const t = timeline([lane(V, 'V1', [clip(asset, 2, 12, 2)]), lane(A, 'A1', [clip(asset, 0, 12, 0)])]);
		linkClips(t, [id(t, 0, 0), id(t, 1, 0)]);
		const before = structuredClone(t);
		extractRange(t, 5, 7, null, RULES);
		assertSpans(t, 0, [[2, 5], [5, 10]]);
		assertSpans(t, 1, [[0, 5], [5, 10]]);
		near(offset(t.tracks[0].clips[1]), offset(t.tracks[1].clips[1]));
		expect(firstSyncBreak(t, before)).toBeNull();
		const conformed = structuredClone(t);
		conformLinks(conformed, before, new Set(), new Map());
		expect(same(conformed, t), 'the sync lock finds nothing left to put right').toBe(true);
	});

	test('a partner on a track that was not named is cut and moved with its clip', () => {
		const asset = 'a';
		const t = timeline([
			lane(V, 'V1', [clip(asset, 0, 10, 0), clip(asset, 0, 10, 10)]),
			lane(A, 'A1', [clip(asset, 0, 10, 0), clip(asset, 0, 5, 20)])
		]);
		const [x, s, music] = [id(t, 0, 0), id(t, 1, 0), id(t, 1, 1)];
		linkClips(t, [x, s]);
		const v1 = t.tracks[0].id;
		const done = extractRange(t, 4, 6, [v1], RULES);
		assertSpans(t, 0, [[0, 4], [4, 8], [8, 18]]);
		assertSpans(t, 1, [[0, 4], [4, 8], [20, 25]]);
		expect(get(t, music).timeline_start).toBe(20);
		expect(done.tracks.length).toBe(2);
		expect(done.moved).toBe(3);
	});

	test('a moved sound that would land on a clip it is not linked to refuses', () => {
		const asset = 'a';
		const t = timeline([
			lane(V, 'V1', [clip(asset, 0, 10, 0)]),
			lane(A, 'A1', [clip(asset, 0, 3, 2), clip(asset, 6, 10, 6)])
		]);
		linkClips(t, [id(t, 0, 0), id(t, 1, 1)]);
		const before = structuredClone(t);
		const v1 = t.tracks[0].id;
		const why = refused(() => extractRange(t, 4, 6, [v1], RULES));
		expect(why).toContain('A1');
		expect(why).toContain('not linked to it');
		expect(same(t, before)).toBe(true);
		// Lifting leaves everything where it is, so nothing lands on anything.
		liftRange(t, 4, 6, [v1], RULES);
	});

	/** For every moment a clip showed outside the range, the piece that shows it afterwards plays the same footage with
	 *  the same animation (at the moment it moved to). */
	function assertFramesKept(name: string, whole: Clip, a: number, b: number) {
		const [start, end] = span(whole);
		for (const close of [false, true]) {
			const t = timeline([lane(V, 'V1', [structuredClone(whole)])]);
			if (close) extractRange(t, a, b, null, RULES);
			else liftRange(t, a, b, null, RULES);
			for (let i = 0; i < 400; i++) {
				const time = start + ((end - start) * (i + 0.5)) / 400;
				if (time > a - 1e-6 && time < b + 1e-6) continue;
				const now = close && time >= b ? time - (b - a) : time;
				const piece = t.tracks[0].clips.find((c) => c.timeline_start - 1e-9 <= now && now < span(c)[1]);
				if (!piece) throw new Error(`${name} [${a}, ${b}) close=${close}: nothing shows ${time}`);
				const local = now - piece.timeline_start;
				const [was, has] = [transformAt(whole, time - start), transformAt(piece, local)];
				const where = `${name} [${a}, ${b}) close=${close} at ${time}`;
				expect(Math.abs(timelineToSource(piece, now) - timelineToSource(whole, time)), `${where}: footage`).toBeLessThan(1e-6);
				for (const k of ['scale', 'pos_x', 'opacity'] as const) {
					expect(Math.abs(was[k] - has[k]), `${where}: ${k}`).toBeLessThan(1e-6);
				}
			}
		}
	}

	test('every frame outside the range stays on its footage and its keys', () => {
		for (const [name, c] of flavours()) {
			const [s, d] = [c.timeline_start, clipDuration(c)];
			const ranges: [number, number][] = [
				[s + 0.3 * d, s + 0.6 * d], // in the middle: cut in two
				[s - 0.5, s + 0.3 * d], // from before the clip: the head goes
				[s + 0.6 * d, s + d + 0.5], // to past its end: the tail goes
				[s + 0.2 * d, s + 0.2 * d + 0.07] // a sliver
			];
			for (const [a, b] of ranges) assertFramesKept(name, c, a, b);
		}
	});

	test('a title on a track is cut like any clip', () => {
		const title = clip(GENERATED_ASSET, 0, 6, 0, { generator: { kind: 'title', name: 'Card', svg: '<svg/>' } });
		const t = timeline([lane(V, 'V1', [title])]);
		liftRange(t, 2, 4, null, RULES);
		assertSpans(t, 0, [[0, 2], [4, 6]]);
		const right = t.tracks[0].clips[1];
		expect(!!right.generator && !!t.tracks[0].clips[0].generator).toBe(true);
		expect([right.source_in, right.source_out]).toEqual([4, 6]);
	});

	test('fades and transitions go with the half that keeps their edge', () => {
		const asset = 'a';
		const crossfade = { kind: 'crossfade' as const, duration: 1 };
		const build = () => {
			const a = clip(asset, 0, 4, 0, { fade_out: 0.5 });
			const b = clip(asset, 0, 4, 4, { transition_in: crossfade, fade_in: 0.5, fade_out: 0.5 });
			const c = clip(asset, 0, 4, 8, { transition_in: crossfade });
			return timeline([lane(V, 'V1', [a, b, c])]);
		};
		// The range sits inside B: the left half keeps B's transition, the right half has none.
		let t = build();
		const b = id(t, 0, 1);
		liftRange(t, 5, 7, null, RULES);
		let [l, r] = [t.tracks[0].clips[1], t.tracks[0].clips[2]];
		expect(l.id).toBe(b);
		expect([l.transition_in, l.fade_in, l.fade_out]).toEqual([crossfade, 0.5, 0]);
		expect([r.transition_in ?? null, r.fade_in, r.fade_out]).toEqual([null, 0, 0.5]);
		// A range that ends B's head: its new edge has no transition and no fade.
		t = build();
		liftRange(t, 3, 5, null, RULES);
		const [a0, b0] = [t.tracks[0].clips[0], t.tracks[0].clips[1]];
		expect([a0.fade_out, b0.transition_in ?? null, b0.fade_in, b0.fade_out]).toEqual([0, null, 0, 0.5]);
		// A range ending exactly at C's start leaves C's own transition alone.
		t = build();
		liftRange(t, 6, 8, null, RULES);
		expect(t.tracks[0].clips[1].transition_in).toEqual(crossfade);
		expect(t.tracks[0].clips[2].transition_in).toEqual(crossfade);
		// A fade longer than what is left of its clip is held inside it.
		t = timeline([lane(V, 'V1', [clip(asset, 0, 10, 0, { fade_in: 3 })])]);
		liftRange(t, 2, 10, null, RULES);
		expect(t.tracks[0].clips[0].fade_in).toBe(2);
	});

	test('overlays and markers are not moved by an extract', () => {
		const t = timeline([lane(V, 'V1', [clip('a', 0, 20, 0)])]);
		t.overlays = [{ id: 'o', text: 'Title', start: 12, end: 14 } as never];
		t.markers = [{ id: 'm', time: 15, name: 'm' } as never];
		extractRange(t, 2, 6, null, RULES);
		expect([t.overlays![0].start, t.overlays![0].end, t.markers![0].time]).toEqual([12, 14, 15]);
	});
});

// ---- delete a gap ------------------------------------------------------------------------

function gappy(): [Timeline, string] {
	const t = timeline([lane(V, 'V1', [clip('a', 0, 2, 0), clip('a', 0, 2, 5), clip('a', 0, 1, 9)])]);
	return [t, t.tracks[0].id];
}

describe('rippleDeleteGap', () => {
	test('a gap is closed by moving everything after it up', () => {
		for (const at of [3, 2, 5, 2 + 1e-9, 4.999]) {
			const [t, v1] = gappy();
			const done = rippleDeleteGap(t, v1, at);
			assertSpans(t, 0, [[0, 2], [2, 4], [6, 7]]);
			expect([done.from, done.to, done.moved]).toEqual([2, 5, 2]);
			expect(t.tracks[0].clips[1].timeline_start).toBe(2);
		}
	});

	test('a gap at the start of a track closes to zero', () => {
		const t = timeline([lane(V, 'V1', [clip('a', 0, 2, 3)])]);
		const done = rippleDeleteGap(t, t.tracks[0].id, 1);
		expect([done.from, done.to]).toEqual([0, 3]);
		assertSpans(t, 0, [[0, 2]]);
	});

	test('a time that is not in a gap is refused', () => {
		const [t, v1] = gappy();
		const before = structuredClone(t);
		const copy = structuredClone(t);
		expect(refused(() => rippleDeleteGap(copy, v1, 1))).toContain('not a gap');
		expect(refused(() => rippleDeleteGap(copy, v1, 9.5))).toContain('not a gap');
		expect(refused(() => rippleDeleteGap(copy, v1, 11))).toContain('nothing after');
		expect(refused(() => rippleDeleteGap(copy, v1, 100))).toContain('nothing after');
		for (const bad of [NaN, Infinity]) expect(refused(() => rippleDeleteGap(copy, v1, bad))).toContain('finite');
		expect(refused(() => rippleDeleteGap(copy, 'missing', 3))).toStartWith('track not found: ');
		copy.tracks[0].locked = true;
		expect(refused(() => rippleDeleteGap(copy, v1, 3))).toContain('locked');
		expect(same(before, t)).toBe(true);

		// Clips that touch have no gap between them.
		const touching = timeline([lane(V, 'V1', [clip('a', 0, 2, 0), clip('a', 0, 2, 2)])]);
		expect(refused(() => rippleDeleteGap(touching, touching.tracks[0].id, 2))).toContain('no gap');
		const blank = timeline([lane(V, 'V1', [])]);
		expect(refused(() => rippleDeleteGap(blank, blank.tracks[0].id, 1))).toContain('nothing after');
	});

	test('a gap beside an old overlap is still the empty stretch', () => {
		const t = timeline([lane(V, 'V1', [clip('a', 0, 10, 0), clip('a', 0, 2, 3), clip('a', 0, 2, 12)])]);
		const done = rippleDeleteGap(t, t.tracks[0].id, 11);
		expect([done.from, done.to, done.moved]).toEqual([10, 12, 1]);
		expect(t.tracks[0].clips[2].timeline_start).toBe(10);
	});
});

// ---- invariants over many random edits -----------------------------------------------------

// ---- no slivers: a range that would leave a remainder shorter than a frame is refused ---------------

/** The asset names a refusal calls a clip by. */
const NAMES: RangeCtx = { ...RULES, nameOf: () => 'interview.mp4' };

describe('no slivers', () => {
	const whole = (): Timeline => timeline([lane(V, 'V1', [clip('a', 0, 10, 0)])]);

	test('a range that would leave a sliver is refused naming the clip and the nearest range', () => {
		// 20 ms of head is less than a frame (33.3 ms): the range is not changed, it is refused.
		const t = whole();
		const before = structuredClone(t);
		const why = refused(() => liftRange(t, 0.02, 5, null, NAMES));
		expect(why).toBe(
			`invalid argument: lifting 0.02–5 s would leave 20 ms of interview.mp4 on V1 (clip ${t.tracks[0].clips[0].id}), less than a frame (33 ms): start at 0 instead`
		);
		expect(same(t, before), 'a refusal changes nothing').toBe(true);
		// The suggestion is the range that works, and it does.
		liftRange(t, 0, 5, null, NAMES);
		assertSpans(t, 0, [[5, 10]]);

		// The same at the other end, and for an extract.
		const why2 = refused(() => extractRange(whole(), 5, 9.98, null, NAMES));
		expect(why2).toMatch(/^invalid argument: extracting 5–9\.98 s would leave 20 ms of interview\.mp4 on V1 \(clip [^)]+\), less than a frame \(33 ms\): end at 10 instead$/);
		// Both ends, in one sentence.
		const why3 = refused(() => liftRange(whole(), 0.02, 9.98, null, NAMES));
		expect(why3.endsWith('less than a frame (33 ms): start at 0 and end at 10 instead'), why3).toBe(true);
		// A clip with no name on hand is named by its id.
		const why4 = refused(() => liftRange(whole(), 0.02, 5, null, RULES));
		expect(why4).toContain('20 ms of clip ');
		expect(why4).toContain(' on V1, less than a frame');
	});

	test('exactly a frame or more is kept', () => {
		// Exactly one frame and a hair over it are clips; a hair under is a sliver.
		for (const [head, kept] of [
			[FRAME, true],
			[FRAME + 1e-4, true],
			[0.0333, false],
			[0.03, false],
			[0.5, true]
		] as const) {
			const t = whole();
			const done = attempt(() => liftRange(t, head, 5, null, RULES));
			expect(done !== null, `head of ${head}`).toBe(kept);
			if (kept) {
				expect(t.tracks[0].clips.length).toBe(2);
				expect(done!.from, 'the range is the one that was asked for').toBe(head);
			}
		}
		// The same after the range.
		expect(attempt(() => extractRange(whole(), 5, 10 - FRAME, null, RULES))).not.toBeNull();
		expect(attempt(() => extractRange(whole(), 5, 9.99, null, RULES))).toBeNull();
	});

	test('the frame is the timeline’s own and is held to a delivery rate', () => {
		const ask = (frame: number) => attempt(() => liftRange(whole(), 0.035, 5, null, { ...RULES, frame })) !== null;
		expect(ask(1 / 30), '35 ms is a frame at 30 fps').toBe(true);
		expect(ask(1 / 24), 'and a sliver at 24 fps (41.7 ms)').toBe(false);
		expect(ask(1 / 60)).toBe(true);
		// A frame that is no length at all leaves only float noise to refuse.
		for (const bad of [0, -1, NaN, Infinity]) expect(ask(bad), `frame ${bad}`).toBe(true);
		// An absurd footage rate cannot make the threshold seconds long: a frame is held to 1 fps ..= 240 fps.
		expect(ask(30), 'a 30 s frame is a 1 s frame').toBe(false);
		expect(attempt(() => liftRange(whole(), 1.5, 5, null, { ...RULES, frame: 30 })), '1.5 s is more than that frame').not.toBeNull();
		expect(ask(1e-9), 'a frame under 1/240 s is held to it: 35 ms is kept').toBe(true);
		expect(attempt(() => liftRange(whole(), 0.001, 5, null, { ...RULES, frame: 1e-9 })), '1 ms is under 1/240 s').toBeNull();
	});

	test('the suggestion covers every lane and partner and works', () => {
		// V1 starts 10 ms before the range, V2 a little over a frame before it: once V1's sliver is out of the way,
		// what is left of V2 is a sliver too, so the range to ask for starts where V2 does.
		const t = timeline([lane(V, 'V1', [clip('a', 0, 10, 4.99)]), lane(V, 'V2', [clip('a', 0, 10, 4.96)])]);
		const why = refused(() => liftRange(structuredClone(t), 5, 8, null, RULES));
		expect(why).toContain('would leave 10 ms of clip ');
		expect(why.endsWith('start at 4.96 instead'), why).toBe(true);
		const again = structuredClone(t);
		liftRange(again, 4.96, 8, null, RULES);
		assertSpans(again, 0, [[8, 14.99]]);
		assertSpans(again, 1, [[8, 14.96]]);
		// Two lanes with a sliver each: the first is named and the other counted.
		const two = timeline([lane(V, 'V1', [clip('a', 0, 10, 4.99)]), lane(V, 'V2', [clip('a', 0, 10, 4.98)])]);
		const why2 = refused(() => liftRange(structuredClone(two), 5, 8, null, RULES));
		expect(why2).toContain('would leave 10 ms of clip ');
		expect(why2).toContain('(and 1 more remainder like it)');
		expect(why2.endsWith('start at 4.98 instead'), why2).toBe(true);
		// Naming the track the sliver is not on is the other way out.
		const named = structuredClone(t);
		liftRange(named, 5, 8, [t.tracks[1].id], RULES);
		assertSpans(named, 0, [[4.99, 14.99]]);
	});

	test('a partner the range cuts is held to the same rule', () => {
		// The sound leads its picture by two seconds; the range starts 10 ms into the picture and 2.01 s into the sound,
		// which is only a sliver for the picture.
		const t = timeline([lane(V, 'V1', [clip('a', 5, 15, 5)]), lane(A, 'A1', [clip('a', 3, 15, 3)])]);
		linkClips(t, [id(t, 0, 0), id(t, 1, 0)]);
		const v1 = t.tracks[0].id;
		const before = structuredClone(t);
		const why = refused(() => extractRange(t, 5.01, 9, [v1], RULES));
		expect(why).toContain('would leave 10 ms of clip ');
		expect(why).toContain(' on V1');
		expect(why.endsWith('start at 5 instead'), why).toBe(true);
		expect(same(t, before)).toBe(true);
		// The picture's own sliver refuses it whether or not the sound follows.
		expect(() => extractRange(t, 5.01, 9, [v1], ALONE)).toThrow();
		// A sliver of a partner the range does not cut is not the range's to refuse.
		const far = structuredClone(before);
		const sound = far.tracks[1].clips[0];
		[sound.source_in, sound.source_out, sound.timeline_start] = [3, 5.005, 3];
		expect(attempt(() => extractRange(far, 5.5, 9, [v1], RULES))).not.toBeNull();
	});

	test('a cut that would leave a sliver is no cut for that clip', () => {
		const t = timeline([lane(V, 'V1', [clip('a', 0, 10, 0)]), lane(V, 'V2', [clip('a', 0, 5.01, 0)])]);
		const short = id(t, 1, 0);
		const done = splitAllTracks(t, 5, null, RULES);
		expect(done.cuts.length, 'V1 is cut, V2 would be left 10 ms').toBe(1);
		expect(done.not_cut).toEqual([short]);
		assertSpans(t, 1, [[0, 5.01]]);

		// One frame is enough, a hair less is not.
		const ok = timeline([lane(V, 'V1', [clip('a', 0, 5 + FRAME, 0)])]);
		expect(splitAllTracks(ok, 5, null, RULES).cuts.length).toBe(1);
		const no = timeline([lane(V, 'V1', [clip('a', 0, 5 + FRAME - 1e-4, 0)])]);
		expect(refused(() => splitAllTracks(no, 5, null, RULES))).toContain('already a cut to within a frame');

		// Nothing cut anywhere is the refusal, with the reason, and nothing changes.
		const head = whole();
		const before = structuredClone(head);
		for (const at of [0.02, 9.98]) {
			expect(refused(() => splitAllTracks(head, at, null, RULES))).toContain('already a cut to within a frame');
			expect(same(head, before)).toBe(true);
		}
		// A clip shorter than two frames has no cut at all.
		const tiny = timeline([lane(V, 'V1', [clip('a', 0, 0.05, 1)])]);
		expect(() => splitAllTracks(tiny, 1.025, null, RULES)).toThrow();
	});

	test('a partner with a sliver half is left whole and stays with its bigger side', () => {
		// The picture is cut at 5; its sound ends 10 ms after that. `not_cut` lists the targeted tracks' clips only: the
		// partner is left whole as well, and is not in it.
		const t = timeline([lane(V, 'V1', [clip('a', 0, 10, 0)]), lane(A, 'A1', [clip('a', 0, 5.01, 0)])]);
		const [x, s] = [id(t, 0, 0), id(t, 1, 0)];
		linkClips(t, [x, s]);
		const group = get(t, x).link_id;
		const done = splitAllTracks(t, 5, [t.tracks[0].id], RULES);
		expect([done.cuts.length, done.not_cut.length], JSON.stringify(done)).toEqual([1, 0]);
		assertSpans(t, 0, [[0, 5], [5, 10]]);
		assertSpans(t, 1, [[0, 5.01]]);
		expect(get(t, x).link_id, 'the left half and the whole sound are one group').toBe(group);
		expect(get(t, s).link_id).toBe(group);
		expect(t.tracks[0].clips[1].link_id, 'the right half is alone').toBeUndefined();
		// With every track targeted the sound is on one and is listed.
		const both = timeline([lane(V, 'V1', [clip('a', 0, 10, 0)]), lane(A, 'A1', [clip('a', 0, 5.01, 0)])]);
		const [bx, bs] = [id(both, 0, 0), id(both, 1, 0)];
		linkClips(both, [bx, bs]);
		const all = splitAllTracks(both, 5, null, RULES);
		expect([all.cuts.length, all.not_cut]).toEqual([1, [bs]]);
		// And one that begins 10 ms before the cut goes with the right half.
		const u = timeline([lane(V, 'V1', [clip('a', 0, 10, 0)]), lane(A, 'A1', [clip('a', 4.99, 10, 4.99)])]);
		linkClips(u, [id(u, 0, 0), id(u, 1, 0)]);
		splitAllTracks(u, 5, null, RULES);
		const [right, sound] = [u.tracks[0].clips[1], u.tracks[1].clips[0]];
		expect(!!right.link_id && right.link_id === sound.link_id, 'the sound joined the right half').toBe(true);
	});
});

// ---- a partner may not run further into a clip it is not linked to ---------------------------------

/** V1 holds two shots linked to the two sounds on A1, which overlap each other by 89 ms already. */
function overlappingSounds(): [Timeline, string] {
	const t = timeline([
		lane(V, 'V1', [clip('a', 0.21, 3.645, 0.21), clip('a', 4.096, 7.993, 4.096)]),
		lane(A, 'A1', [clip('a', 0, 4.001, 0), clip('a', 3.912, 8.163, 3.912)])
	]);
	linkClips(t, [id(t, 0, 0), id(t, 1, 0)]);
	linkClips(t, [id(t, 0, 1), id(t, 1, 1)]);
	return [t, t.tracks[0].id];
}

describe('overlaps that were there', () => {
	test('an edit that would grow an overlap that was already there is refused', () => {
		const [t, v1] = overlappingSounds();
		const before = structuredClone(t);
		const copy = structuredClone(t);
		const why = refused(() => extractRange(copy, 3.761, 6.725, [v1], RULES));
		expect(why).toContain('run into another clip on A1');
		expect(why).toContain('not linked to it');
		expect(same(copy, t), 'a refusal changes nothing').toBe(true);

		// Cut and not moved, the sound's overlap only shrinks: that stays allowed.
		const lifted = structuredClone(t);
		liftRange(lifted, 3.761, 6.725, [v1], RULES);
		assertSpans(lifted, 1, [[0, 4.001], [6.725, 8.163]]);
		// And the overlap that was there is not itself a reason to refuse an edit that leaves it alone.
		const elsewhere = structuredClone(before);
		extractRange(elsewhere, 7, 7.5, [v1], RULES);
		assertSpans(elsewhere, 1, [[0, 4.001], [3.912, 7], [7, 7.663]]);
	});

	test('a sub-frame overlap with linked material is let through and an unlinked clip is not', () => {
		// Two shots, each linked to its sound; the first sound runs 10 ms past its picture, into the second shot's sound
		// once that is moved up to where its picture is.
		const build = (): [Timeline, string, string] => {
			const t = timeline([
				lane(V, 'V1', [clip('a', 0, 10, 0), clip('a', 10, 30, 10)]),
				lane(A, 'A1', [clip('a', 0, 10.01, 0), clip('a', 10.01, 30, 10.01)])
			]);
			linkClips(t, [id(t, 0, 0), id(t, 1, 0)]);
			linkClips(t, [id(t, 0, 1), id(t, 1, 1)]);
			return [t, t.tracks[0].id, id(t, 1, 0)];
		};
		// A range that starts 20 ms into the second shot is a sliver of it (and 10 ms of its sound): refused by that
		// rule, which says where to start instead.
		const [t, v1] = build();
		const before = structuredClone(t);
		const why = refused(() => extractRange(t, 10.02, 15, [v1], RULES));
		expect(why).toContain('would leave 20 ms of clip ');
		expect(why).toContain('(and 1 more remainder like it)');
		expect(why.endsWith('start at 10 instead'), why).toBe(true);
		expect(same(t, before)).toBe(true);
		// At the cut itself it is let through: 10 ms of linked sound overlapping is not worth a refusal.
		extractRange(t, 10, 15, [v1], RULES);
		assertSpans(t, 0, [[0, 10], [10, 25]]);
		assertSpans(t, 1, [[0, 10.01], [10, 25]]);
		// The same overlap with an unlinked clip is still refused, naming the lane.
		const [u, uv1, s1] = build();
		delete clipById(u, s1)!.link_id;
		const why2 = refused(() => extractRange(u, 10, 15, [uv1], RULES));
		expect(why2).toContain('run into another clip on A1');
		expect(why2).toContain('not linked to it');
		// And a linked overlap longer than a frame is still refused.
		const [w, wv1, ws1] = build();
		clipById(w, ws1)!.source_out = 10.2;
		expect(refused(() => extractRange(w, 10, 15, [wv1], RULES))).toContain('run into another clip on A1');
	});

	test('an overlap that does not grow is accepted', () => {
		// Music rides under the end of a sound on A1: 0.5 s of overlap to begin with.
		const t = timeline([
			lane(V, 'V1', [clip('a', 0, 10, 0)]),
			lane(A, 'A1', [clip('a', 0, 10, 0), clip('music', 0, 2.5, 9.5)])
		]);
		linkClips(t, [id(t, 0, 0), id(t, 1, 0)]);
		const v1 = t.tracks[0].id;
		// A lift cuts the sound in two and leaves its right half exactly where it was: the same overlap.
		const lifted = structuredClone(t);
		liftRange(lifted, 2, 3, [v1], RULES);
		assertSpans(lifted, 1, [[0, 2], [3, 10], [9.5, 12]]);
		// An extract carries it up, away from the music: the overlap is gone.
		const extracted = structuredClone(t);
		extractRange(extracted, 2, 3, [v1], RULES);
		assertSpans(extracted, 1, [[0, 2], [2, 9], [9.5, 12]]);
	});
});

// ---- a partner that only leads or trails its picture is carried, not cut ----------------------------

/** V1 [0,7) then [10,15) linked to the sound that begins two seconds early: A1 [0,7) and [8,15). */
function aPictureAfterTheRangeAndASoundThatLeadsIt(): [Timeline, string, string, string] {
	const t = timeline([
		lane(V, 'V1', [clip('a', 0, 7, 0), clip('a', 10, 15, 10)]),
		lane(A, 'A1', [clip('a', 0, 7, 0), clip('a', 8, 15, 8)])
	]);
	const [x, s] = [id(t, 0, 1), id(t, 1, 1)];
	linkClips(t, [x, s]);
	return [t, t.tracks[0].id, x, s];
}

describe('a linked sound that leads its picture', () => {
	test('extracting an empty stretch does not eat the lead-in of a linked sound', () => {
		const [t, v1, x, s] = aPictureAfterTheRangeAndASoundThatLeadsIt();
		const done = extractRange(t, 7, 10, [v1], RULES);
		assertSpans(t, 0, [[0, 7], [7, 12]]);
		expect([done.removed, done.trimmed, done.split, done.moved]).toEqual([0, 0, 0, 1]);
		expect(done.tracks, 'the sound’s lane is the sync lock’s').toEqual([t.tracks[0].id]);
		// The sound is exactly as it was — not cut at the range's end, not moved: the lock carries it.
		assertSpans(t, 1, [[0, 7], [8, 15]]);
		const sound = get(t, s);
		expect([sound.source_in, sound.source_out], 'its lead-in is still there').toEqual([8, 15]);
		expect(sound.link_id).toBe(get(t, x).link_id);
	});

	test('a partner that overlaps the range with its picture is still cut with it', () => {
		// The same sound, but the picture reaches into the range too (it starts at 9): both are cut.
		const t = timeline([
			lane(V, 'V1', [clip('a', 0, 7, 0), clip('a', 9, 15, 9)]),
			lane(A, 'A1', [clip('a', 0, 7, 0), clip('a', 8, 15, 8)])
		]);
		linkClips(t, [id(t, 0, 1), id(t, 1, 1)]);
		const done = extractRange(t, 7, 10, [t.tracks[0].id], RULES);
		assertSpans(t, 0, [[0, 7], [7, 12]]);
		assertSpans(t, 1, [[0, 7], [7, 12]]);
		expect([done.trimmed, done.moved]).toEqual([2, 2]);
	});
});

// ---- refusals name the same lane every time ---------------------------------------------------------

/** V1 holds two shots, each linked to a sound on a track of its own; the tracks are in `order`, and the sounds'
 *  tracks are locked when `lock` says so. */
function twoSoundsOnTwoLanes(order: [string, string], lock: boolean): [Timeline, string] {
	const lanes = [
		['A1', 0],
		['A2', 10]
	].map(([name, start]) => lane(A, name as string, [clip('a', start as number, (start as number) + 10, start as number)]));
	lanes.sort((x, y) => order.indexOf(x.name) - order.indexOf(y.name));
	const t = timeline([lane(V, 'V1', [clip('a', 0, 10, 0), clip('a', 10, 20, 10)]), ...lanes]);
	const picture = [id(t, 0, 0), id(t, 0, 1)];
	['A1', 'A2'].forEach((name, i) => {
		const ti = t.tracks.findIndex((x) => x.name === name);
		linkClips(t, [picture[i], id(t, ti, 0)]);
	});
	for (const track of t.tracks.slice(1)) if (lock) track.locked = true;
	return [t, t.tracks[0].id];
}

describe('refusals name the same lane every time', () => {
	test('the lane of the first group, however the groups are stored', () => {
		for (const order of [['A1', 'A2'], ['A2', 'A1']] as [string, string][]) {
			const [t, v1] = twoSoundsOnTwoLanes(order, true);
			const why = refused(() => extractRange(structuredClone(t), 5, 15, [v1], RULES));
			expect(why, `the first group's sound is on A1 (${order}): ${why}`).toContain('locked track A1');
		}
	});

	test('a collision names the first lane in track order', () => {
		// One shot after a gap on V1, with a sound of the same group on each of two lanes; each lane holds music where
		// the sound would land once the gap is closed. Both refuse: the lane that comes first is named.
		for (const order of [['A1', 'A2'], ['A2', 'A1']]) {
			const lanes = [lane(V, 'V1', [clip('a', 0, 5, 0), clip('a', 10, 20, 10)])];
			for (const name of order) lanes.push(lane(A, name, [clip('music', 0, 1, 6), clip('a', 10, 20, 10)]));
			const t = timeline(lanes);
			linkClips(t, [id(t, 0, 1), id(t, 1, 1), id(t, 2, 1)]);
			const before = structuredClone(t);
			const why = refused(() => extractRange(t, 5, 10, [t.tracks[0].id], RULES));
			expect(why, `${order}`).toContain(`clip on ${order[0]} at`);
			expect(same(t, before)).toBe(true);
		}
	});
});

describe('a gap before the start of the timeline', () => {
	test('is refused, and 0 itself is in the head gap', () => {
		const t = timeline([lane(V, 'V1', [clip('a', 0, 2, 3)])]);
		const v1 = t.tracks[0].id;
		const before = structuredClone(t);
		for (const at of [-1, -1e-3, -1e-9]) {
			expect(refused(() => rippleDeleteGap(t, v1, at)), `${at}`).toContain('before the start of the timeline');
			expect(same(t, before)).toBe(true);
		}
		const done = rippleDeleteGap(t, v1, 0);
		expect([done.from, done.to]).toEqual([0, 3]);
	});
});

/** What `run` answered, or `null` when it refused (the caller checks the timeline is as it was). */
function attempt<T>(run: () => T, why: (message: string) => void = () => {}): T | null {
	try {
		return run();
	} catch (e) {
		why((e as Error).message);
		return null;
	}
}

/** kerf-core's test generator, bit for bit (a 64-bit LCG), so a failure names the seed in both languages. */
class Lcg {
	state: bigint;
	constructor(seed: number) {
		this.state = BigInt(seed);
	}
	next(): number {
		this.state = (this.state * 6364136223846793005n + 1442695040888963407n) & 0xffffffffffffffffn;
		return Number(this.state >> 33n) / 2 ** 31;
	}
	pick(n: number): number {
		return Math.min(Math.floor(this.next() * n), n - 1);
	}
}

function laneIsLegal(track: Track): boolean {
	const clips = [...track.clips].sort((a, b) => a.timeline_start - b.timeline_start);
	return (
		clips.slice(1).every((c, i) => span(clips[i])[1] <= c.timeline_start + 1e-6) &&
		clips.every((c) => c.timeline_start >= -1e-9 && clipDuration(c) > 0 && Number.isFinite(clipDuration(c)))
	);
}
const total = (t: Timeline) => t.tracks.flatMap((x) => x.clips).reduce((sum, c) => sum + clipDuration(c), 0);
const overlapWith = (t: Timeline, a: number, b: number) =>
	t.tracks
		.flatMap((x) => x.clips)
		.reduce((sum, c) => sum + Math.max(Math.min(span(c)[1], b) - Math.max(c.timeline_start, a), 0), 0);

/** A start and a length for a random edit. Half the time an end of it is aimed at the edge of a clip, a few
 *  milliseconds off: where a range leaves a sliver (`random_range` in `range_edit.rs`, call for call). */
function randomRange(rng: Lcg, t: Timeline): [number, number] {
	const [a, len] = [rng.next() * 14 - 1, 0.05 + rng.next() * 5];
	const edges = t.tracks.flatMap((x) => x.clips).flatMap((c) => [c.timeline_start, span(c)[1]]);
	if (edges.length === 0 || rng.next() >= 0.5) return [a, len];
	const edge = edges[rng.pick(edges.length)];
	const jitter = (rng.next() - 0.5) * 0.08;
	return rng.next() < 0.5 ? [edge + jitter, len] : [edge + jitter - len, len];
}

describe('random edits', () => {
	test('keep every lane legal and lose exactly the range', () => {
		const landed = [0, 0, 0];
		let suggested = 0;
		for (let seed = 0; seed < 300; seed++) {
			const rng = new Lcg(seed + 1);
			const asset = 'random';
			const tracks: Track[] = [];
			for (const [kind, name] of [
				[V, 'V1'],
				[V, 'V2'],
				[A, 'A1']
			] as const) {
				let at = rng.next() * 2;
				const clips: Clip[] = [];
				const n = rng.pick(5) + 1;
				for (let i = 0; i < n; i++) {
					const len = 0.2 + rng.next() * 4;
					clips.push(clip(asset, 1, 1 + len, at, { speed: [1, 2, 0.5, -1][rng.pick(4)] }));
					at = span(clips[clips.length - 1])[1] + (rng.next() < 0.4 ? rng.next() * 3 : 0);
				}
				tracks.push(lane(kind, name, clips));
			}
			const t = timeline(tracks);
			for (let step = 0; step < 6; step++) {
				const [a, len] = randomRange(rng, t);
				const named = [t.tracks[rng.pick(3)].id];
				const targeted = rng.next() < 0.3 ? named : null;
				const before = structuredClone(t);
				const context = `seed ${seed} step ${step}: [${a}, ${a + len}) on ${JSON.stringify(targeted)}`;
				const which = rng.pick(3);
				let failure = '';
				const done = attempt(() => {
					if (which === 0) return splitAllTracks(t, a, targeted, RULES);
					return which === 1 ? liftRange(t, a, a + len, targeted, RULES) : extractRange(t, a, a + len, targeted, RULES);
				}, (why) => (failure = why));
				if (done === null) {
					expect(same(t, before), context).toBe(true);
					// A sliver refusal's range works: it is not itself refused for a sliver.
					if (failure.includes('less than a frame')) {
						const tail = failure.split(': ').pop()!;
						const number = (key: string) => {
							const rest = tail.split(key)[1];
							return rest === undefined ? undefined : Number(rest.split(/\s+/)[0]);
						};
						const [lo, hi] = [number('start at ') ?? Math.max(a, 0), number('end at ') ?? a + len];
						const again = structuredClone(before);
						let second = '';
						attempt(
							() => (which === 1 ? liftRange(again, lo, hi, targeted, RULES) : extractRange(again, lo, hi, targeted, RULES)),
							(message) => (second = message)
						);
						expect(second.includes('less than a frame'), `${context}: ${lo}..${hi} is a sliver too: ${second}`).toBe(false);
						suggested++;
					}
				} else if (which === 0) {
					landed[0]++;
					const cuts = (done as { cuts: unknown[] }).cuts.length;
					expect(cuts, context).toBeGreaterThan(0);
					near(total(t), total(before));
					expect(t.tracks.reduce((n, x) => n + x.clips.length, 0), context).toBe(
						before.tracks.reduce((n, x) => n + x.clips.length, 0) + cuts
					);
				} else if (targeted === null) {
					landed[which]++;
					// Everything the range as applied reached is gone, and only that.
					const applied = done as { from: number; to: number };
					near(total(t), total(before) - overlapWith(before, applied.from, applied.to));
					expect([applied.from, applied.to], 'the range is the one asked for').toEqual([Math.max(a, 0), a + len]);
				} else {
					landed[which]++;
					expect(total(t), context).toBeLessThanOrEqual(total(before) + 1e-9);
				}
				for (const track of t.tracks) {
					expect(laneIsLegal(track), `${context}: ${track.name} is not a legal lane`).toBe(true);
					expect(
						track.clips.every((c) => clipDuration(c) >= FRAME - 2e-6),
						`${context}: ${track.name} holds a clip shorter than a frame: ${JSON.stringify(track.clips.map(span))}`
					).toBe(true);
				}
			}
		}
		// The edits must really have landed, or the checks above judge nothing.
		expect(landed.every((n) => n > 150), `ops that landed: ${landed}`).toBe(true);
		expect(suggested, 'slivers that were refused with a range').toBeGreaterThan(50);
	});

	test('leave linked pairs identical and in step', () => {
		let landed = 0;
		for (let seed = 0; seed < 300; seed++) {
			const rng = new Lcg(seed + 7);
			const asset = 'linked';
			let at = rng.next();
			const [picture, sound]: [Clip[], Clip[]] = [[], []];
			const n = rng.pick(5) + 2;
			for (let i = 0; i < n; i++) {
				const len = 0.3 + rng.next() * 4;
				const [src, speed] = [rng.next() * 3, [1, 2, -1][rng.pick(3)]];
				for (const out of [picture, sound]) out.push(clip(asset, src, src + len, at, { speed }));
				at = span(picture[picture.length - 1])[1] + (rng.next() < 0.4 ? rng.next() * 2 : 0);
			}
			const t = timeline([lane(V, 'V1', picture), lane(A, 'A1', sound)]);
			for (let i = 0; i < t.tracks[0].clips.length; i++) linkClips(t, [id(t, 0, i), id(t, 1, i)]);
			for (let step = 0; step < 5; step++) {
				const [a, len] = randomRange(rng, t);
				const named = [t.tracks[rng.pick(2)].id];
				const targeted = rng.next() < 0.5 ? named : null;
				const before = structuredClone(t);
				const context = `seed ${seed} step ${step}: [${a}, ${a + len}) on ${JSON.stringify(targeted)}`;
				const which = rng.pick(3);
				const done = attempt(() => {
					if (which === 0) return splitAllTracks(t, a, targeted, RULES);
					return which === 1 ? liftRange(t, a, a + len, targeted, RULES) : extractRange(t, a, a + len, targeted, RULES);
				});
				if (done === null) {
					expect(same(t, before), context).toBe(true);
					continue;
				}
				landed++;
				const [sp, ss] = [spansOf(t, 0), spansOf(t, 1)];
				expect(sp.length, `${context}: ${JSON.stringify(sp)} ${JSON.stringify(ss)}`).toBe(ss.length);
				sp.forEach((x, i) => {
					near(x[0], ss[i][0]);
					near(x[1], ss[i][1]);
				});
				expect(
					t.tracks.flatMap((x) => x.clips).every((c) => clipDuration(c) >= FRAME - 2e-6),
					`${context}: a clip is shorter than a frame: ${JSON.stringify(sp)}`
				).toBe(true);
				expect(firstSyncBreak(t, before), context).toBeNull();
				const conformed = structuredClone(t);
				conformLinks(conformed, before, new Set(), new Map());
				expect(same(conformed, t), `${context}: the sync lock still had something to move`).toBe(true);
				t.tracks[0].clips.forEach((p, i) => {
					const s = t.tracks[1].clips[i];
					expect(!!p.link_id, context).toBe(!!s.link_id);
					if (p.link_id) expect(p.link_id === s.link_id, `${context}: partners stay one group`).toBe(true);
				});
				dissolveAllOrphans(t);
			}
		}
		expect(landed, `ops that landed: ${landed}`).toBeGreaterThan(600);
	});
});

// ---- the Project's revision around them (`project/range_ops_tests.rs`) ---------------------------

describe('range edits as the project makes them', () => {
	const FOOTAGE = 'av';
	const env = (ripple: boolean, links = true): ops.EditEnv => ({
		ripple,
		links,
		footage: new Map([[FOOTAGE, 60]]),
		hasAudio: () => true,
		frameRate: () => 30,
		asset: (id) => (id === FOOTAGE ? { name: '/av.mp4', duration: 60, streams: [{ kind: 'video' }, { kind: 'audio' }] } : undefined)
	});
	/** V1 holds two shots, `c1` source 0..10 at 0 and `c2` source 20..26 at `gapTo`, each with its sound detached onto
	 *  A1 and linked: two pairs with `gapTo - 10` seconds of nothing between them. */
	function pairs(gapTo: number): { t: Timeline; c: [string, string]; a: [string, string] } {
		const mute = { source_audio: false };
		const [c1, c2] = [clip(FOOTAGE, 0, 10, 0, mute), clip(FOOTAGE, 20, 26, gapTo, mute)];
		const [a1, a2] = [clip(FOOTAGE, 0, 10, 0), clip(FOOTAGE, 20, 26, gapTo)];
		const t = timeline([lane(V, 'V1', [c1, c2]), lane(A, 'A1', [a1, a2])]);
		linkClips(t, [c1.id, a1.id]);
		linkClips(t, [c2.id, a2.id]);
		return { t, c: [c1.id, c2.id], a: [a1.id, a2.id] };
	}
	const lanes = (t: Timeline, track: number) => spansOf(t, track).map(([s, e]) => [s, e]);

	test('cutting every track is one revision', () => {
		const { t } = pairs(10);
		const done = ops.splitAllTracks(t, env(false), 4, null);
		expect(done.result.cuts.length).toBe(2);
		expect(done.label).toBe('Split all tracks (2 clips)');
		expect(lanes(done.timeline, 0)).toEqual([[0, 4], [4, 10], [10, 16]]);
		expect(lanes(done.timeline, 1)).toEqual(lanes(done.timeline, 0));
	});

	test('a lift leaves the gap and an extract closes it whatever ripple mode says', () => {
		for (const ripple of [false, true]) {
			const { t } = pairs(10);
			const before = structuredClone(t);
			const lifted = ops.liftRange(t, env(ripple), 3, 7, null);
			expect(lifted.label).toBe('Lift range (2 clips)');
			expect([lifted.result.trimmed, lifted.result.split]).toEqual([0, 2]);
			expect(lanes(lifted.timeline, 0), `the gap stays (ripple ${ripple})`).toEqual([[0, 3], [7, 10], [10, 16]]);
			expect(lanes(lifted.timeline, 1)).toEqual([[0, 3], [7, 10], [10, 16]]);
			expect(same(t, before), 'the timeline it was handed is untouched').toBe(true);

			const extracted = ops.extractRange(t, env(ripple), 3, 7, null);
			expect(extracted.label).toBe('Extract range (2 clips)');
			expect(extracted.result.closed).toBe(4);
			const closed = [[0, 3], [3, 6], [6, 12]];
			expect(lanes(extracted.timeline, 0), `closed by the range once, not twice (ripple ${ripple})`).toEqual(closed);
			expect(lanes(extracted.timeline, 1)).toEqual(closed);
		}
	});

	test('cutting every track shifts nothing under ripple', () => {
		const { t } = pairs(12);
		const done = ops.splitAllTracks(t, env(true), 4, null);
		expect(done.result.cuts.length).toBe(2);
		expect(done.label).toBe('Split all tracks (2 clips)');
		expect(lanes(done.timeline, 0)).toEqual([[0, 4], [4, 10], [12, 18]]);
		expect(lanes(done.timeline, 1)).toEqual(lanes(done.timeline, 0));
	});

	test('a refusal changes nothing', () => {
		const { t } = pairs(12);
		const before = structuredClone(t);
		const v1 = t.tracks[0].id;
		const e = env(false);
		expect(() => ops.splitAllTracks(t, e, 11, null)).toThrow();
		expect(() => ops.splitAllTracks(t, e, 10, null)).toThrow();
		expect(() => ops.liftRange(t, e, 10.5, 11.5, null)).toThrow();
		expect(() => ops.extractRange(t, e, 30, 40, null)).toThrow();
		expect(() => ops.rippleDeleteGap(t, e, v1, 3)).toThrow();
		for (const bad of [NaN, Infinity, -Infinity]) {
			expect(refused(() => ops.splitAllTracks(t, e, bad, null))).toBe('invalid argument: the cut time must be a finite number');
			expect(refused(() => ops.liftRange(t, e, bad, 1, null))).toBe(
				'invalid argument: the start of the range must be a finite number'
			);
			expect(refused(() => ops.extractRange(t, e, 0, bad, null))).toBe(
				'invalid argument: the end of the range must be a finite number'
			);
			expect(refused(() => ops.rippleDeleteGap(t, e, v1, bad))).toBe("invalid argument: the gap's time must be a finite number");
		}
		expect(() => ops.liftRange(t, e, 5, 2, null)).toThrow();
		expect(() => ops.liftRange(t, e, 2, 4, ['missing'])).toThrow();
		expect(same(t, before)).toBe(true);
	});

	test('a locked track is left alone and its partner refuses the edit', () => {
		const { t } = pairs(12);
		t.tracks[1].locked = true;
		const before = structuredClone(t);
		const e = env(false);
		for (const run of [
			() => ops.splitAllTracks(t, e, 4, null),
			() => ops.liftRange(t, e, 3, 7, null),
			() => ops.extractRange(t, e, 3, 7, null)
		]) {
			expect(refused(run)).toContain('locked track A1');
		}
		expect(same(t, before)).toBe(true);
		// Told to leave the links alone, the picture is cut and the locked sound is not.
		const done = ops.liftRange(t, env(false, false), 3, 7, null);
		expect(lanes(done.timeline, 0)).toEqual([[0, 3], [7, 10], [12, 18]]);
		expect(lanes(done.timeline, 1)).toEqual([[0, 10], [12, 18]]);
	});

	test('naming one track still takes the linked sound along', () => {
		const { t } = pairs(10);
		const done = ops.extractRange(t, env(false), 3, 7, [t.tracks[0].id]);
		expect(done.result.tracks.length, 'the sound’s track changed with it').toBe(2);
		expect(lanes(done.timeline, 0)).toEqual([[0, 3], [3, 6], [6, 12]]);
		expect(lanes(done.timeline, 1)).toEqual(lanes(done.timeline, 0));
	});

	test('closing a gap is one revision and carries the linked sound', () => {
		const { t, c, a } = pairs(14);
		const done = ops.rippleDeleteGap(t, env(false), t.tracks[0].id, 12);
		expect(done.label).toBe('Ripple delete gap');
		expect([done.result.from, done.result.to, done.result.moved]).toEqual([10, 14, 1]);
		expect(span(get(done.timeline, c[1]))).toEqual([10, 16]);
		expect(span(get(done.timeline, a[1])), 'its sound followed through the sync lock').toEqual([10, 16]);
		expect(span(get(done.timeline, a[0]))).toEqual([0, 10]);
		// With the links off only the track's own clips move.
		const alone = ops.rippleDeleteGap(t, env(false, false), t.tracks[0].id, 12);
		expect(span(get(alone.timeline, a[1]))).toEqual([14, 20]);
	});

	test('a gap close that would push a sound onto an unlinked clip refuses', () => {
		const { t } = pairs(14);
		// Music on A1 right where the second sound would come to rest.
		t.tracks[1].clips.push(clip('music', 0, 3, 10.5));
		t.tracks[1].clips.sort((x, y) => x.timeline_start - y.timeline_start);
		const before = structuredClone(t);
		const why = refused(() => ops.rippleDeleteGap(t, env(false), t.tracks[0].id, 12));
		expect(why).toContain('A1');
		expect(same(t, before)).toBe(true);
	});

	test('a remainder shorter than a frame of the cut refuses the range at the rate it renders at', () => {
		// The footage is 30 fps (a frame is 33.3 ms), so 35 ms of head is a clip.
		const { t } = pairs(10);
		const done = ops.liftRange(t, env(false), 0.035, 3, null);
		expect([done.result.from, done.result.to], 'the range that was asked for').toEqual([0.035, 3]);
		expect(lanes(done.timeline, 0)).toEqual([[0, 0.035], [3, 10], [10, 16]]);
		// Delivered at 24 fps the frame is 41.7 ms and the same 35 ms is a sliver: refused, changing nothing, with the
		// range that works, which is then taken.
		const at24: ops.EditEnv = { ...env(false), frameRate: () => 24 };
		const before = structuredClone(t);
		const why = refused(() => ops.liftRange(t, at24, 0.035, 3, null));
		expect(why).toMatch(
			/^invalid argument: lifting 0\.035–3 s would leave 35 ms of \/av\.mp4 on V1 \(clip [^)]+\) \(and 1 more remainder like it\), less than a frame \(42 ms\): start at 0 instead$/
		);
		expect(same(t, before)).toBe(true);
		const taken = ops.liftRange(t, at24, 0, 3, null);
		expect(lanes(taken.timeline, 0)).toEqual([[3, 10], [10, 16]]);
		expect(lanes(taken.timeline, 1), 'the sound is cut with it').toEqual(lanes(taken.timeline, 0));
		expect(refused(() => ops.extractRange(t, at24, 0.03, 12, null))).toMatch(/^invalid argument: extracting 0\.03–12 s would leave 30 ms of \/av\.mp4 on V1 \(clip /);
		expect(refused(() => ops.splitAllTracks(t, at24, 0.02, null))).toContain('already a cut to within a frame');
	});

	test('a footage rate that is absurd cannot make the threshold seconds long', () => {
		// A half-frame-per-second clip has a two-second frame; it is held to a one-second one (1 fps), so a range that
		// leaves 1.5 s is fine and one that leaves 0.5 s is not.
		const t = timeline([lane(V, 'V1', [clip(FOOTAGE, 0, 20, 0)])]);
		const slow: ops.EditEnv = { ...env(false), frameRate: () => 0.5 };
		expect(ops.liftRange(t, slow, 1.5, 5, null).result.from).toBe(1.5);
		const why = refused(() => ops.liftRange(t, slow, 0.5, 5, null));
		expect(why).toContain('would leave 500 ms of /av.mp4 on V1');
		expect(why).toContain('less than a frame (1000 ms)');
	});

	/** V1 `[0,7)` and `[10,15)`, the second linked to a sound that begins two seconds early (A1 `[8,15)`) and the first to
	 *  its own sound on A1, `[0,7)`: linked to it, unlinked from it or gone, as `first` says. */
	function aLeadingSoundAfterAShot(first: 'linked' | 'unlinked' | 'gone'): { t: Timeline; c2: string; a1: string; a2: string } {
		const [c1, c2] = [clip(FOOTAGE, 0, 7, 0, { source_audio: false }), clip(FOOTAGE, 10, 15, 10, { source_audio: false })];
		const [a1, a2] = [clip(FOOTAGE, 0, 7, 0), clip(FOOTAGE, 8, 15, 8)];
		const t = timeline([lane(V, 'V1', [c1, c2]), lane(A, 'A1', first === 'gone' ? [a2] : [a1, a2])]);
		linkClips(t, [c2.id, a2.id]);
		if (first === 'linked') linkClips(t, [c1.id, a1.id]);
		return { t, c2: c2.id, a1: a1.id, a2: a2.id };
	}

	test('extracting an empty stretch carries a leading sound like closing the gap does', () => {
		// A clip of A1 that is linked to nothing sits where the sound would land: both refuse, in the same words.
		const { t } = aLeadingSoundAfterAShot('unlinked');
		const v1 = t.tracks[0].id;
		const before = structuredClone(t);
		const byExtract = refused(() => ops.extractRange(t, env(false), 7, 10, [v1]));
		const byGap = refused(() => ops.rippleDeleteGap(t, env(false), v1, 8.5));
		expect(byExtract).toContain('would run into another clip on A1');
		expect(byExtract, 'extract and the gap delete refuse the same case with the same sentence').toBe(byGap);
		expect(same(t, before)).toBe(true);
		// With room before the sound, both carry it up with its picture; its lead-in is still there.
		const free = aLeadingSoundAfterAShot('gone');
		const done = ops.extractRange(free.t, env(false), 7, 10, [free.t.tracks[0].id]);
		expect([done.result.removed, done.result.trimmed, done.result.split]).toEqual([0, 0, 0]);
		expect(done.result.lock_trimmed, 'nothing was in the way').toEqual([]);
		expect(span(get(done.timeline, free.c2))).toEqual([7, 12]);
		expect(span(get(done.timeline, free.a2)), 'the sound moved with its picture, whole').toEqual([5, 12]);
		expect(get(done.timeline, free.a2).source_in).toBe(8);
	});

	test('a carried sound trims the linked sound of the shot before and the reply says so', () => {
		// Both shots have their sound, each pair linked: the sound that leads the second picture lands on the first
		// shot's sound, which the sync lock trims back (the rule it has for any ripple) — not a refusal.
		for (const gap of [false, true]) {
			const { t, c2, a1, a2 } = aLeadingSoundAfterAShot('linked');
			const v1 = t.tracks[0].id;
			const done = gap ? ops.rippleDeleteGap(t, env(false), v1, 8.5) : ops.extractRange(t, env(false), 7, 10, [v1]);
			if (gap) expect([(done.result as { from: number }).from, (done.result as { to: number }).to]).toEqual([7, 10]);
			expect(done.label).toBe(gap ? 'Ripple delete gap (trimmed sound on A1)' : 'Extract range (trimmed sound on A1)');
			const [trim, ...rest] = done.result.lock_trimmed;
			expect(rest).toEqual([]);
			expect([trim.track_id, trim.track, trim.clip_id]).toEqual([t.tracks[1].id, 'A1', a1]);
			near(trim.seconds, 2);
			expect(span(get(done.timeline, c2))).toEqual([7, 12]);
			expect(span(get(done.timeline, a2)), 'the leading sound came up with its picture, whole').toEqual([5, 12]);
			expect(span(get(done.timeline, a1)), 'and the sound it landed on lost its tail').toEqual([0, 5]);
			expect(get(done.timeline, a1).source_in).toBe(0);
		}
	});

	test('a sound a few milliseconds past its picture does not block an edit at the cut', () => {
		// V1 `[0,10)` and `[10,30)`, each linked to its sound on A1, the first sound 10 ms longer than its picture.
		const [p1, p2] = [clip(FOOTAGE, 0, 10, 0, { source_audio: false }), clip(FOOTAGE, 10, 30, 10, { source_audio: false })];
		const [s1, s2] = [clip(FOOTAGE, 0, 10.01, 0), clip(FOOTAGE, 10.01, 30, 10.01)];
		const t = timeline([lane(V, 'V1', [p1, p2]), lane(A, 'A1', [s1, s2])]);
		linkClips(t, [p1.id, s1.id]);
		linkClips(t, [p2.id, s2.id]);
		const v1 = t.tracks[0].id;
		const before = structuredClone(t);
		// Twenty milliseconds into the second shot leaves a sliver of it: refused, and where to start is said.
		const why = refused(() => ops.extractRange(t, env(false), 10.02, 15, [v1]));
		expect(why).toContain('would leave 20 ms of /av.mp4 on V1');
		expect(why.endsWith('start at 10 instead'), why).toBe(true);
		expect(same(t, before)).toBe(true);
		// Taking that advice goes through: the sound is 10 ms into its neighbour, which is linked material.
		const done = ops.extractRange(t, env(false), 10, 15, [v1]);
		expect(lanes(done.timeline, 0)).toEqual([[0, 10], [10, 25]]);
		expect(lanes(done.timeline, 1)).toEqual([[0, 10.01], [10, 25]]);
		// With the first sound linked to nothing the overlap is an unlinked clip's, and refuses.
		delete get(t, s1.id).link_id;
		expect(refused(() => ops.extractRange(t, env(false), 10, 15, [v1]))).toContain('would run into another clip on A1');
	});
});
