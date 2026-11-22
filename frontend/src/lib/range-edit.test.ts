import { describe, expect, test } from 'bun:test';
import { dissolveAllOrphans, firstSyncBreak, linkClips } from './link-groups';
import * as ops from './link-ops';
import { splitClip } from './links';
import { conformLinks } from './ripple';
import { extractRange, liftRange, rippleDeleteGap, splitAllTracks } from './range-edit';
import { timelineToSource } from './speed-ramp';
import { transformAt } from './channels';
import type { Clip, Keyframe, StreamKind, Timeline, Track } from './types';
import { clipDuration, GENERATED_ASSET } from './types';

// `range-edit.ts` is the port of kerf-core's `model/range_edit.rs`, and `link-ops.ts` of `project/range_ops.rs` around
// it. These replay the Rust unit tests (`mod tests` of `range_edit.rs`, `range_ops_tests.rs`) case for case, inputs,
// outputs and refusal sentences included, so a rule changed in Rust and not here names the test it broke.
// `links-corpus.test.ts` replays kerf-core's own answers for the same ops.

let seq = 0;
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
		const done = splitAllTracks(t, 5, null, true);
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
		const done = splitAllTracks(t, 5, null, true);
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
			const why = refused(() => splitAllTracks(copy, at, null, true));
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

		expect(refused(() => splitAllTracks(timeline([]), 1, null, true))).toContain('no unlocked track');
		const blank = timeline([lane(V, 'V1', []), lane(A, 'A1', [])]);
		expect(refused(() => splitAllTracks(blank, 1, null, true))).toContain('no clip on an unlocked track');
	});

	test('locked tracks are left alone and reported', () => {
		const asset = 'a';
		const t = timeline([
			lane(V, 'V1', [clip(asset, 0, 10, 0)]),
			lane(A, 'A1', [clip(asset, 0, 10, 0)], { locked: true }),
			lane(A, 'A2', [clip(asset, 0, 2, 20)], { locked: true })
		]);
		const done = splitAllTracks(t, 4, null, true);
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
		const done = splitAllTracks(t, 4, [v2, v2], true);
		expect(done.cuts.length).toBe(1);
		expect([t.tracks[0].clips.length, t.tracks[1].clips.length, t.tracks[2].clips.length]).toEqual([1, 2, 1]);

		const before = structuredClone(t);
		expect(refused(() => splitAllTracks(t, 4, ['missing'], true))).toStartWith('track not found: ');
		expect(refused(() => splitAllTracks(t, 4, [], true))).toContain('name at least one track');
		t.tracks[0].locked = true;
		const v1 = t.tracks[0].id;
		expect(refused(() => splitAllTracks(t, 2, [v1], true))).toContain('track V1 is locked');
		delete t.tracks[0].locked;
		expect(same(t, before)).toBe(true);
	});

	test('linked partners are cut with the clip and the halves linked by side', () => {
		const [t0, x, s] = pairOf('a', [0, 10]);
		const group = get(t0, x).link_id;
		for (const tracks of [null, [t0.tracks[0].id]]) {
			const t = structuredClone(t0);
			const done = splitAllTracks(t, 4, tracks, true);
			expect(done.cuts.length).toBe(2);
			expect(get(t, x).link_id).toBe(group);
			expect(get(t, s).link_id).toBe(group);
			const [rx, rs] = [id(t, 0, 1), id(t, 1, 1)];
			const fresh = get(t, rx).link_id;
			expect(fresh && fresh !== group).toBeTruthy();
			expect(get(t, rs).link_id).toBe(fresh);
		}
		const alone = structuredClone(t0);
		splitAllTracks(alone, 4, [t0.tracks[0].id], false);
		expect([alone.tracks[0].clips.length, alone.tracks[1].clips.length]).toEqual([2, 1]);
	});

	test('a partner on a locked track refuses the cut and changes nothing', () => {
		const [t] = pairOf('a', [0, 10]);
		t.tracks[1].locked = true;
		const before = structuredClone(t);
		expect(refused(() => splitAllTracks(t, 4, null, true))).toContain('locked track A1');
		expect(same(t, before)).toBe(true);
		splitAllTracks(t, 4, null, false);
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
		const done = splitAllTracks(t, at, null, true);
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
		const done = liftRange(t, 3, 9, null, true);
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
		const done = liftRange(t, 3, 6, null, true);
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
		const done = extractRange(t, 3, 6, null, true);
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
		extractRange(t, 0.1, 1.3, null, true);
		expect(t.tracks[0].clips[1].timeline_start).toBe(0.1);
		near(span(t.tracks[0].clips[1])[1], 1.8);
		near(t.tracks[0].clips[2].timeline_start, 1.8);
	});

	test('an empty stretch has nothing to lift but can be closed when something follows', () => {
		const t = timeline([lane(V, 'V1', [clip('a', 0, 2, 0), clip('a', 0, 2, 5)])]);
		const before = structuredClone(t);
		expect(refused(() => liftRange(t, 2.5, 4.5, null, true))).toContain('no clip on a targeted track');
		expect(same(t, before)).toBe(true);
		const done = extractRange(t, 2, 5, null, true);
		expect([done.removed + done.trimmed + done.split, done.moved, done.closed]).toEqual([0, 1, 3]);
		assertSpans(t, 0, [[0, 2], [2, 4]]);
		expect(refused(() => extractRange(t, 10, 12, null, true))).toContain('none follows it');
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
				const got = refused(() => (close ? extractRange(copy, from, to, null, true) : liftRange(copy, from, to, null, true)));
				expect(got, `(${from}, ${to})`).toContain(why);
				expect(same(copy, t)).toBe(true);
			}
		}
		// A start before 0 is held at 0; an end far past everything lifts to the end.
		let copy = structuredClone(t);
		const done = liftRange(copy, -5, 3, null, true);
		expect(done.from).toBe(0);
		assertSpans(copy, 0, [[3, 10]]);
		copy = structuredClone(t);
		extractRange(copy, 4, 1e300, null, true);
		assertSpans(copy, 0, [[0, 4]]);
		// Nothing at all, and no tracks.
		const blank = timeline([lane(V, 'V1', [])]);
		expect(() => liftRange(blank, 0, 5, null, true)).toThrow();
		expect(() => extractRange(blank, 0, 5, null, true)).toThrow();
		expect(refused(() => extractRange(timeline([]), 0, 5, null, true))).toContain('no unlocked track');
	});

	test('locked tracks are skipped when not named and refused when named', () => {
		const t = timeline([
			lane(V, 'V1', [clip('a', 0, 10, 0)]),
			lane(V, 'V2', [clip('a', 0, 10, 0)], { locked: true })
		]);
		const locked = t.tracks[1].id;
		const before = structuredClone(t);
		expect(refused(() => liftRange(t, 2, 4, [locked], true))).toContain('track V2 is locked');
		expect(refused(() => liftRange(t, 2, 4, ['missing'], true))).toStartWith('track not found: ');
		expect(same(t, before)).toBe(true);
		const done = extractRange(t, 2, 4, null, true);
		assertSpans(t, 0, [[0, 2], [2, 8]]);
		assertSpans(t, 1, [[0, 10]]);
		expect(done.skipped.length).toBe(1);
		expect(done.skipped[0].name).toBe('V2');
		// Everything locked: nothing to edit.
		t.tracks[0].locked = true;
		expect(refused(() => liftRange(t, 2, 4, null, true))).toContain('no unlocked track');
	});

	test('a partner on a locked track refuses a range it reaches and not one it misses', () => {
		const [t, x] = pairOf('a', [0, 10]);
		t.tracks[1].locked = true;
		const before = structuredClone(t);
		for (const close of [false, true]) {
			const why = refused(() => (close ? extractRange(t, 4, 6, null, true) : liftRange(t, 4, 6, null, true)));
			expect(why).toContain('locked track A1');
			expect(same(t, before)).toBe(true);
		}
		// With links off the picture alone is cut.
		liftRange(t, 4, 6, null, false);
		assertSpans(t, 0, [[0, 4], [6, 10]]);
		assertSpans(t, 1, [[0, 10]]);
		expect(get(t, x).link_id).toBe(get(before, x).link_id);

		// A sound that ends before the range is not reached by it.
		const [t2, , s] = pairOf('a', [0, 10]);
		get(t2, s).source_out = 3;
		t2.tracks[1].locked = true;
		liftRange(t2, 4, 6, null, true);
		assertSpans(t2, 1, [[0, 3]]);
	});

	test('a linked group falls in two by side and stays in step', () => {
		const [t0, x, s] = pairOf('a', [0, 10]);
		const group = get(t0, x).link_id;
		for (const close of [false, true]) {
			const t = structuredClone(t0);
			if (close) extractRange(t, 4, 6, null, true);
			else liftRange(t, 4, 6, null, true);
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
		extractRange(t, 5, 7, null, true);
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
		const done = extractRange(t, 4, 6, [v1], true);
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
		const why = refused(() => extractRange(t, 4, 6, [v1], true));
		expect(why).toContain('A1');
		expect(why).toContain('not linked to it');
		expect(same(t, before)).toBe(true);
		// Lifting leaves everything where it is, so nothing lands on anything.
		liftRange(t, 4, 6, [v1], true);
	});

	/** For every moment a clip showed outside the range, the piece that shows it afterwards plays the same footage with
	 *  the same animation (at the moment it moved to). */
	function assertFramesKept(name: string, whole: Clip, a: number, b: number) {
		const [start, end] = span(whole);
		for (const close of [false, true]) {
			const t = timeline([lane(V, 'V1', [structuredClone(whole)])]);
			if (close) extractRange(t, a, b, null, true);
			else liftRange(t, a, b, null, true);
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
		liftRange(t, 2, 4, null, true);
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
		liftRange(t, 5, 7, null, true);
		let [l, r] = [t.tracks[0].clips[1], t.tracks[0].clips[2]];
		expect(l.id).toBe(b);
		expect([l.transition_in, l.fade_in, l.fade_out]).toEqual([crossfade, 0.5, 0]);
		expect([r.transition_in ?? null, r.fade_in, r.fade_out]).toEqual([null, 0, 0.5]);
		// A range that ends B's head: its new edge has no transition and no fade.
		t = build();
		liftRange(t, 3, 5, null, true);
		const [a0, b0] = [t.tracks[0].clips[0], t.tracks[0].clips[1]];
		expect([a0.fade_out, b0.transition_in ?? null, b0.fade_in, b0.fade_out]).toEqual([0, null, 0, 0.5]);
		// A range ending exactly at C's start leaves C's own transition alone.
		t = build();
		liftRange(t, 6, 8, null, true);
		expect(t.tracks[0].clips[1].transition_in).toEqual(crossfade);
		expect(t.tracks[0].clips[2].transition_in).toEqual(crossfade);
		// A fade longer than what is left of its clip is held inside it.
		t = timeline([lane(V, 'V1', [clip(asset, 0, 10, 0, { fade_in: 3 })])]);
		liftRange(t, 2, 10, null, true);
		expect(t.tracks[0].clips[0].fade_in).toBe(2);
	});

	test('overlays and markers are not moved by an extract', () => {
		const t = timeline([lane(V, 'V1', [clip('a', 0, 20, 0)])]);
		t.overlays = [{ id: 'o', text: 'Title', start: 12, end: 14 } as never];
		t.markers = [{ id: 'm', time: 15, name: 'm' } as never];
		extractRange(t, 2, 6, null, true);
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

/** What `run` answered, or `null` when it refused (the caller checks the timeline is as it was). */
function attempt<T>(run: () => T): T | null {
	try {
		return run();
	} catch {
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

describe('random edits', () => {
	test('keep every lane legal and lose exactly the range', () => {
		const landed = [0, 0, 0];
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
				const [a, len] = [rng.next() * 14 - 1, 0.05 + rng.next() * 5];
				const named = [t.tracks[rng.pick(3)].id];
				const targeted = rng.next() < 0.3 ? named : null;
				const before = structuredClone(t);
				const reached = overlapWith(before, Math.max(a, 0), a + len);
				const context = `seed ${seed} step ${step}: [${a}, ${a + len}) on ${JSON.stringify(targeted)}`;
				const which = rng.pick(3);
				const done = attempt(() => {
					if (which === 0) return splitAllTracks(t, a, targeted, true);
					return which === 1 ? liftRange(t, a, a + len, targeted, true) : extractRange(t, a, a + len, targeted, true);
				});
				if (done === null) {
					expect(same(t, before), context).toBe(true);
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
					near(total(t), total(before) - reached);
				} else {
					landed[which]++;
					expect(total(t), context).toBeLessThanOrEqual(total(before) + 1e-9);
				}
				for (const track of t.tracks) expect(laneIsLegal(track), `${context}: ${track.name} is not a legal lane`).toBe(true);
			}
		}
		// The edits must really have landed, or the checks above judge nothing.
		expect(landed.every((n) => n > 150), `ops that landed: ${landed}`).toBe(true);
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
				const [a, len] = [rng.next() * 12 - 0.5, 0.05 + rng.next() * 4];
				const named = [t.tracks[rng.pick(2)].id];
				const targeted = rng.next() < 0.5 ? named : null;
				const before = structuredClone(t);
				const context = `seed ${seed} step ${step}: [${a}, ${a + len}) on ${JSON.stringify(targeted)}`;
				const which = rng.pick(3);
				const done = attempt(() => {
					if (which === 0) return splitAllTracks(t, a, targeted, true);
					return which === 1 ? liftRange(t, a, a + len, targeted, true) : extractRange(t, a, a + len, targeted, true);
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
		hasAudio: () => true
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
});
