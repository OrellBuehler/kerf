import { describe, expect, test } from 'bun:test';
import {
	rollEditLinked,
	rollRangeLinked,
	slideClipLinked,
	slideRangeLinked,
	slipClipLinked,
	slipRangeLinked,
	type SourceLimits
} from './edit-modes';
import {
	clipById,
	contentOffset,
	dissolveAllOrphans,
	firstSyncBreak,
	linkClips,
	linkedClipIds,
	linkPartners,
	unlinkClips,
	withLinkPartners
} from './link-groups';
import {
	carryExtentEdit,
	carryLinksSince,
	checkCarriedLanes,
	cutClipRangeLinked,
	detachAudio,
	detachAudioMany,
	reattachAudio,
	reattachAudioMany,
	rippleDeleteLinked,
	setSpeedLinked,
	splitClip,
	splitClipLinked,
	withLinkedCuts,
	withLinkedMoves
} from './links';
import { trimmedSuffix } from './link-ops';
import { moveClips } from './multi-edit';
import { conformLinks, rippleFrom, rippleLanes } from './ripple';
import type { AudioEffect, Clip, ClipMove, StreamKind, Timeline, Track } from './types';
import { clipDuration } from './types';

// The bun mirror of the Rust tests in `crates/kerf-core/src/model/links.rs`, case for
// case: the harness's linked edits (`links.ts`, `link-groups.ts`, the linked modes in
// `edit-modes.ts`, `conformLinks` in `ripple.ts`) are ports of the backend's, and these
// are the cases that keep them honest. Where a Rust test has a name, this one repeats it.

const X = 100;
const uuid = () => crypto.randomUUID();

const clip = (asset: string, sourceIn: number, sourceOut: number, at: number): Clip => ({
	id: uuid(),
	asset_id: asset,
	source_in: sourceIn,
	source_out: sourceOut,
	timeline_start: at,
	volume: 1,
	fade_in: 0,
	fade_out: 0
});
const lane = (kind: StreamKind, name: string, clips: Clip[]): Track => ({ id: uuid(), kind, name, clips });
const timeline = (tracks: Track[]): Timeline => ({ tracks });
const limits = (assets: string[]): SourceLimits => new Map(assets.map((a) => [a, X]));
const idOf = (t: Timeline, track: number, c: number) => t.tracks[track].clips[c].id;
const get = (t: Timeline, id: string): Clip => clipById(t, id)!;
const start = (t: Timeline, id: string) => get(t, id).timeline_start;
const extent = (t: Timeline, id: string): [number, number] => [start(t, id), start(t, id) + clipDuration(get(t, id))];
const lock = (t: Timeline, track: number) => {
	t.tracks[track].locked = true;
};
const same = (a: number, b: number) => expect(Math.abs(a - b)).toBeLessThan(1e-9);

/** V1 holds `c` (source 0..10 at 0), A1 its sound `a` (the same), linked. */
function pair(): { t: Timeline; c: string; a: string; asset: string } {
	const asset = uuid();
	const t = timeline([
		lane('video', 'V1', [clip(asset, 0, 10, 0)]),
		lane('audio', 'A1', [clip(asset, 0, 10, 0)])
	]);
	const [c, a] = [idOf(t, 0, 0), idOf(t, 1, 0)];
	linkClips(t, [c, a]);
	return { t, c, a, asset };
}

const mv = (t: Timeline, id: string, at: number, track?: number): ClipMove => ({
	clip_id: id,
	timeline_start: at,
	track_id: track === undefined ? undefined : t.tracks[track].id
});

describe('link / unlink', () => {
	test('a link joins a picture to its sound and nothing else', () => {
		const asset = uuid();
		const t = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0), clip(asset, 5, 9, 5)]),
			lane('audio', 'A1', [clip(asset, 0, 5, 0)])
		]);
		const [v1, v2, a1] = [idOf(t, 0, 0), idOf(t, 0, 1), idOf(t, 1, 0)];
		expect(linkPartners(t, v1)).toEqual([]);
		expect(() => linkClips(t, [v1])).toThrow('at least two');
		expect(() => linkClips(t, [v1, v1])).toThrow('more than once');
		expect(() => linkClips(t, [v1, uuid()])).toThrow('clip not found');
		expect(() => linkClips(t, [v1, v2])).toThrow('V1');
		expect(linkPartners(t, v1)).toEqual([]);

		const group = linkClips(t, [v1, a1]);
		expect(linkPartners(t, v1)).toEqual([a1]);
		expect(linkPartners(t, a1)).toEqual([v1]);
		expect(get(t, v1).link_id).toBe(group);
		expect(linkPartners(t, v2)).toEqual([]);
		expect(() => linkClips(t, [v1, a1])).toThrow('already linked');
		t.tracks.push(lane('audio', 'A2', [clip(asset, 0, 5, 0)]));
		const a2 = idOf(t, 2, 0);
		linkClips(t, [v1, a1, a2]);
		expect(linkPartners(t, a2)).toHaveLength(2);
		expect(withLinkPartners(t, [v1])).toEqual([v1, a1, a2]);
	});

	test('linking into a new group dissolves the one left behind', () => {
		const asset = uuid();
		const t = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0)]),
			lane('audio', 'A1', [clip(asset, 0, 5, 0)]),
			lane('audio', 'A2', [clip(asset, 0, 5, 0)])
		]);
		const [v, a1, a2] = [idOf(t, 0, 0), idOf(t, 1, 0), idOf(t, 2, 0)];
		linkClips(t, [v, a1]);
		linkClips(t, [a1, a2]);
		expect(linkPartners(t, v)).toEqual([]);
		expect(get(t, v).link_id).toBeUndefined();
		expect(linkPartners(t, a1)).toEqual([a2]);
	});

	test('unlinking either half of a pair unlinks the pair', () => {
		const { t, c, a } = pair();
		expect(unlinkClips(t, [a])).toBe(1);
		expect(get(t, c).link_id).toBeUndefined();
		expect(get(t, a).link_id).toBeUndefined();
		expect(() => unlinkClips(t, [c, a])).toThrow('none of those clips is linked');
		expect(() => unlinkClips(t, [uuid()])).toThrow('clip not found');
	});

	test('unlinking one of three leaves the other two together', () => {
		const asset = uuid();
		const t = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0)]),
			lane('audio', 'A1', [clip(asset, 0, 5, 0)]),
			lane('audio', 'A2', [clip(asset, 0, 5, 0)])
		]);
		const ids = [0, 1, 2].map((i) => idOf(t, i, 0));
		linkClips(t, ids);
		expect(unlinkClips(t, ids.slice(0, 1))).toBe(1);
		expect(linkPartners(t, ids[1])).toEqual([ids[2]]);
		expect(linkPartners(t, ids[0])).toEqual([]);
	});

	test('linking and unlinking refuse a locked track', () => {
		const { t, c, a } = pair();
		lock(t, 1);
		expect(() => unlinkClips(t, [c, a])).toThrow('locked');
		expect(unlinkClips(t, [c])).toBe(1); // a locked partner is only dissolved, not edited
		const asset = uuid();
		const u = timeline([lane('video', 'V1', [clip(asset, 0, 5, 0)]), lane('audio', 'A1', [clip(asset, 0, 5, 0)])]);
		const ids = [idOf(u, 0, 0), idOf(u, 1, 0)];
		lock(u, 1);
		expect(() => linkClips(u, ids)).toThrow('locked');
	});
});

describe('move', () => {
	test('a move carries the partner by the same time on its own track', () => {
		const { t, c, a } = pair();
		const wide = withLinkedMoves(t, [mv(t, c, 4)]);
		expect(wide).toHaveLength(2);
		expect(wide[1]).toEqual({ clip_id: a, timeline_start: 4 });
		moveClips(t, wide);
		expect([start(t, c), start(t, a)]).toEqual([4, 4]);
	});

	test('a partner at another offset keeps its offset', () => {
		const { t, c, a } = pair();
		get(t, a).timeline_start = 2;
		moveClips(t, withLinkedMoves(t, [mv(t, c, 5)]));
		expect([start(t, c), start(t, a)]).toEqual([5, 7]);
	});

	test('a track change belongs to the clip that was named', () => {
		const { t, c, a } = pair();
		t.tracks.splice(1, 0, lane('video', 'V2', []));
		moveClips(t, withLinkedMoves(t, [mv(t, c, 3, 1)]));
		expect(t.tracks.findIndex((tr) => tr.clips.some((x) => x.id === c))).toBe(1);
		expect(t.tracks.findIndex((tr) => tr.clips.some((x) => x.id === a))).toBe(2);
		expect([start(t, c), start(t, a)]).toEqual([3, 3]);
	});

	test('a partner that is named is not moved twice and a still move adds nothing', () => {
		const { t, c, a } = pair();
		const both = withLinkedMoves(t, [mv(t, c, 4), mv(t, a, 6)]);
		expect(both).toHaveLength(2);
		expect(both[1].timeline_start).toBe(6);
		expect(withLinkedMoves(t, [mv(t, c, 0)])).toHaveLength(1);
		const lone = structuredClone(t);
		lone.tracks[1].clips = [];
		expect(withLinkedMoves(lone, [mv(lone, c, 4)])).toHaveLength(1);
	});

	test('a partner that would start before zero or sit on a locked track refuses the move', () => {
		const { t, c, a } = pair();
		get(t, c).timeline_start = 3;
		get(t, a).timeline_start = 1;
		expect(() => withLinkedMoves(t, [mv(t, c, 0.5)])).toThrow(/A1.*beginning/);
		expect(() => withLinkedMoves(t, [mv(t, c, 2)])).not.toThrow();
		lock(t, 1);
		expect(() => withLinkedMoves(t, [mv(t, c, 4)])).toThrow(/locked.*A1|A1.*locked/);
		t.tracks.splice(1, 0, lane('video', 'V2', []));
		expect(() => withLinkedMoves(t, [mv(t, c, 3, 1)])).not.toThrow(); // a pure track change moves no partner
	});

	test('a group move that cannot land changes nothing', () => {
		const { t, c, asset } = pair();
		t.tracks[1].clips.push(clip(asset, 20, 25, 12));
		const before = JSON.stringify(t);
		expect(() => moveClips(t, withLinkedMoves(t, [mv(t, c, 8)]))).toThrow('overlap');
		expect(JSON.stringify(t)).toBe(before);
	});
});

describe('trim', () => {
	test('a right trim follows when the partner shares the edge', () => {
		const { t, c, a, asset } = pair();
		const was = structuredClone(get(t, c));
		get(t, c).source_out = 7;
		expect(carryExtentEdit(t, c, was, limits([asset]))).toHaveLength(1);
		expect(extent(t, a)).toEqual([0, 7]);
		expect([get(t, a).source_in, get(t, a).source_out]).toEqual([0, 7]);
	});

	test('a partner that never shared the edge is left alone there', () => {
		const { t, c, a, asset } = pair();
		get(t, a).source_out = 8;
		const was = structuredClone(get(t, c));
		get(t, c).source_out = 7;
		carryExtentEdit(t, c, was, limits([asset]));
		expect(extent(t, a)).toEqual([0, 8]);
	});

	test('a left trim moves the partners head and window too', () => {
		const { t, c, a, asset } = pair();
		const was = structuredClone(get(t, c));
		Object.assign(get(t, c), { source_in: 3, timeline_start: 3 });
		carryExtentEdit(t, c, was, limits([asset]));
		expect(extent(t, a)).toEqual([3, 10]);
		expect([get(t, a).source_in, get(t, a).source_out]).toEqual([3, 10]);
	});

	test('extending past the partners footage stops where it runs out', () => {
		const asset = uuid();
		const music = uuid();
		const t = timeline([lane('video', 'V1', [clip(asset, 0, 10, 0)]), lane('audio', 'A1', [clip(music, 90, 100, 0)])]);
		const [c, a] = [idOf(t, 0, 0), idOf(t, 1, 0)];
		linkClips(t, [c, a]);
		const was = structuredClone(get(t, c));
		get(t, c).source_out = 15;
		carryExtentEdit(t, c, was, limits([asset, music]));
		expect(extent(t, c)).toEqual([0, 15]);
		expect(extent(t, a)).toEqual([0, 10]);
	});

	test('a partner at another speed follows in timeline seconds', () => {
		const { t, c, a, asset } = pair();
		Object.assign(get(t, a), { speed: 2, source_in: 0, source_out: 10, timeline_start: 5 });
		expect(extent(t, a)).toEqual([5, 10]);
		const was = structuredClone(get(t, c));
		get(t, c).source_out = 8;
		carryExtentEdit(t, c, was, limits([asset]));
		expect(extent(t, a)[1]).toBe(8);
		expect(get(t, a).source_out).toBe(6);
		expect(get(t, a).timeline_start).toBe(5);
	});

	test('a pure move through trim moves the partner by the same time', () => {
		const { t, c, a, asset } = pair();
		const was = structuredClone(get(t, c));
		get(t, c).timeline_start = 6;
		carryExtentEdit(t, c, was, limits([asset]));
		expect(extent(t, a)).toEqual([6, 16]);
		expect(get(t, a).source_in).toBe(0);
		const u = pair();
		get(u.t, u.c).timeline_start = 3;
		get(u.t, u.a).timeline_start = 3;
		const wasU = structuredClone(get(u.t, u.c));
		get(u.t, u.a).timeline_start = 1;
		get(u.t, u.c).timeline_start = 0;
		// Carried before zero, the partner loses what hangs off the front (and keeps its sync:
		// its head is trimmed, it is not slid) — unless nothing would be left.
		carryExtentEdit(u.t, u.c, wasU, limits([u.asset]));
		const p = get(u.t, u.a);
		expect([p.timeline_start, p.timeline_start + clipDuration(p), p.source_in]).toEqual([0, 8, 2]);
		const w = pair();
		get(w.t, w.a).source_out = 1;
		get(w.t, w.a).timeline_start = 1;
		get(w.t, w.c).timeline_start = 5;
		const wasW = structuredClone(get(w.t, w.c));
		get(w.t, w.c).timeline_start = 0;
		expect(() => carryExtentEdit(w.t, w.c, wasW, limits([w.asset]))).toThrow('beginning of the timeline');
	});

	test('a trim that would remove the partner or touch a locked one is refused', () => {
		const { t, c, a, asset } = pair();
		get(t, a).source_out = 4;
		const was = structuredClone(get(t, c));
		Object.assign(get(t, c), { source_in: 5, timeline_start: 5 });
		expect(() => carryExtentEdit(t, c, was, limits([asset]))).toThrow('trimmed away');
		const u = pair();
		lock(u.t, 1);
		const wasU = structuredClone(get(u.t, u.c));
		get(u.t, u.c).source_out = 7;
		expect(() => carryExtentEdit(u.t, u.c, wasU, limits([u.asset]))).toThrow('locked');
		expect(a).toBeDefined();
	});

	test('a reversed clips head is its out-point', () => {
		const { t, c, a, asset } = pair();
		for (const id of [c, a]) get(t, id).speed = -1;
		const was = structuredClone(get(t, c));
		Object.assign(get(t, c), { source_out: 7, timeline_start: 3 });
		carryExtentEdit(t, c, was, limits([asset]));
		expect(extent(t, a)).toEqual([3, 10]);
		expect([get(t, a).source_in, get(t, a).source_out]).toEqual([0, 7]);
	});

	test('a still partner is trimmed by extent', () => {
		const asset = uuid();
		const still = uuid();
		const t = timeline([lane('video', 'V1', [clip(asset, 0, 10, 0)]), lane('video', 'V2', [clip(still, 0, 10, 0)])]);
		const [c, s] = [idOf(t, 0, 0), idOf(t, 1, 0)];
		linkClips(t, [c, s]);
		const footage: SourceLimits = new Map([
			[asset, X],
			[still, Infinity]
		]);
		const was = structuredClone(get(t, c));
		Object.assign(get(t, c), { source_in: 4, timeline_start: 4 });
		carryExtentEdit(t, c, was, footage);
		expect(extent(t, s)).toEqual([4, 10]);
		expect(get(t, s).source_in).toBeGreaterThanOrEqual(0);
	});

	test('a carried partner may not land on a clip outside its group', () => {
		const { t, c, a, asset } = pair();
		t.tracks[1].clips.push(clip(uuid(), 0, 3, 12));
		const other = idOf(t, 1, 1);
		const before = structuredClone(t);
		const was = structuredClone(get(t, c));
		get(t, c).source_out = 13;
		const carried = carryExtentEdit(t, c, was, limits([asset])).map((p) => p.id);
		expect(extent(t, a)).toEqual([0, 13]); // the carry itself writes no lane check
		expect(() => checkCarriedLanes(t, before, carried)).toThrow(/A1.*0:12\.0.*not linked to it/);
		// Short of it, or with the clip out of the way (the ripple pushed it), nothing is wrong.
		get(t, a).source_out = 11.5;
		expect(() => checkCarriedLanes(t, before, carried)).not.toThrow();
		get(t, a).source_out = 13;
		get(t, other).timeline_start = 14;
		expect(() => checkCarriedLanes(t, before, carried)).not.toThrow();
		// An overlap that was there before the edit is old news.
		get(t, other).timeline_start = 12;
		const wasOverlapping = structuredClone(before);
		get(wasOverlapping, a).source_out = 12.5;
		expect(() => checkCarriedLanes(t, wasOverlapping, carried)).not.toThrow();
		expect(() => checkCarriedLanes(t, before, [])).not.toThrow();
	});
});

describe('split', () => {
	test('a split cuts the partner and links the new halves', () => {
		const { t, c, a } = pair();
		const group = get(t, c).link_id;
		const [left, right] = splitClipLinked(t, c, 4);
		expect([left.id, extent(t, c)]).toEqual([c, [0, 4]]);
		expect(extent(t, a)).toEqual([0, 4]);
		expect(get(t, a).link_id).toBe(group);
		const aRight = t.tracks[1].clips.find((x) => x.id !== a)!.id;
		expect(extent(t, aRight)).toEqual([4, 10]);
		expect(extent(t, right.id)).toEqual([4, 10]);
		expect(right.link_id).toBeTruthy();
		expect(right.link_id).not.toBe(group);
		expect(linkPartners(t, right.id)).toEqual([aRight]);
		expect(linkPartners(t, c)).toEqual([a]);
	});

	test('a split keeps each fade on the half that holds its edge', () => {
		const { t, c, a } = pair();
		for (const id of [c, a]) {
			get(t, id).fade_in = 1;
			get(t, id).fade_out = 2;
		}
		get(t, c).transition_in = { kind: 'crossfade', duration: 0.5 };
		const [left, right] = splitClipLinked(t, c, 4);
		expect([left.fade_in, left.fade_out]).toEqual([1, 0]);
		expect([right.fade_in, right.fade_out]).toEqual([0, 2]);
		expect(left.transition_in).toBeTruthy();
		expect(right.transition_in).toBeNull();
		const aRight = t.tracks[1].clips.find((x) => x.id !== a)!;
		expect([get(t, a).fade_in, get(t, a).fade_out]).toEqual([1, 0]);
		expect([aRight.fade_in, aRight.fade_out]).toEqual([0, 2]);
	});

	test('a fade longer than the half it stays on is clamped to it', () => {
		const { t, c } = pair();
		get(t, c).fade_in = 5;
		get(t, c).fade_out = 6;
		const [left, right] = splitClip(t, c, 2);
		expect([left.fade_in, left.fade_out]).toEqual([2, 0]);
		expect([right.fade_in, right.fade_out]).toEqual([0, 6]);
		const [, tail] = splitClip(t, right.id, 9);
		expect([tail.fade_in, tail.fade_out]).toEqual([0, 1]);
	});

	test('a partner the cut does not reach is left whole and the new half unlinked', () => {
		const { t, c, a } = pair();
		get(t, a).source_out = 3;
		const [, right] = splitClipLinked(t, c, 6);
		expect(extent(t, a)).toEqual([0, 3]);
		expect(t.tracks[1].clips).toHaveLength(1);
		expect(right.link_id).toBeUndefined();
		expect(linkPartners(t, c)).toEqual([a]);
	});

	test('a split at a partners edge does not make a sliver', () => {
		const { t, c } = pair();
		get(t, idOf(t, 1, 0)).timeline_start = 5;
		splitClipLinked(t, c, 5);
		expect(t.tracks[1].clips).toHaveLength(1);
	});

	test('a partner at another start is split at the same timeline time', () => {
		const { t, c, a } = pair();
		get(t, a).timeline_start = 2;
		const [, right] = splitClipLinked(t, c, 6);
		expect(extent(t, a)).toEqual([2, 6]);
		expect(get(t, a).source_out).toBe(4);
		const aRight = linkPartners(t, right.id)[0];
		expect(extent(t, aRight)).toEqual([6, 12]);
		expect(get(t, aRight).source_in).toBe(4);
	});

	test('a locked partner refuses the split and nothing changes', () => {
		const { t, c } = pair();
		lock(t, 1);
		const before = JSON.stringify(t);
		expect(() => splitClipLinked(t, c, 4)).toThrow('locked');
		expect(JSON.stringify(t)).toBe(before);
		const u = pair();
		get(u.t, u.a).source_out = 3;
		lock(u.t, 1);
		expect(() => splitClipLinked(u.t, u.c, 6)).not.toThrow();
	});

	test('a plain split leaves the left half linked and the right free', () => {
		const { t, c, a } = pair();
		const [, right] = splitClip(t, c, 4);
		expect(right.link_id).toBeUndefined();
		expect(linkPartners(t, c)).toEqual([a]);
		expect(extent(t, a)).toEqual([0, 10]);
	});
});

describe('remove', () => {
	test('partners are named once after the clips that were', () => {
		const { t, c, a } = pair();
		expect(withLinkPartners(t, [a, c])).toEqual([a, c]);
		expect(withLinkPartners(t, [c, c])).toEqual([c, a]);
		const lone = uuid();
		expect(withLinkPartners(t, [lone])).toEqual([lone]);
	});

	test('a ripple delete closes the gap on both tracks', () => {
		const { t, c, a, asset } = pair();
		for (const track of [0, 1]) t.tracks[track].clips.push(clip(asset, 10, 16, 10));
		const [c2, a2] = [idOf(t, 0, 1), idOf(t, 1, 1)];
		linkClips(t, [c2, a2]);
		expect(rippleDeleteLinked(t, c)).toBe(2);
		expect(clipById(t, c)).toBeUndefined();
		expect(clipById(t, a)).toBeUndefined();
		expect([start(t, c2), start(t, a2)]).toEqual([0, 0]);
		const u = pair();
		lock(u.t, 1);
		expect(() => rippleDeleteLinked(u.t, u.c)).toThrow('locked');
		expect(clipById(u.t, u.c)).toBeDefined();
	});
});

describe('cut a source range', () => {
	test('cutting a range cuts the partner and closes up both tracks', () => {
		const { t, c, a, asset } = pair();
		for (const track of [0, 1]) t.tracks[track].clips.push(clip(asset, 20, 25, 10));
		const [c2, a2] = [idOf(t, 0, 1), idOf(t, 1, 1)];
		linkClips(t, [c2, a2]);
		const kept = cutClipRangeLinked(t, c, 3, 5);
		expect(kept).toHaveLength(2);
		expect([extent(t, c), extent(t, a)]).toEqual([
			[0, 3],
			[0, 3]
		]);
		const cTail = kept[1].id;
		const aTail = t.tracks[1].clips.find((x) => x.id !== a && x.id !== a2)!.id;
		expect(extent(t, cTail)).toEqual([3, 8]);
		expect(extent(t, aTail)).toEqual([3, 8]);
		expect([start(t, c2), start(t, a2)]).toEqual([8, 8]);
		expect(linkPartners(t, cTail)).toEqual([aTail]);
		expect(linkPartners(t, c)).toEqual([a]);
	});

	test('a partner at another offset loses the same moment, not the same source', () => {
		const { t, c, a } = pair();
		get(t, a).timeline_start = 2;
		cutClipRangeLinked(t, c, 4, 6);
		const sound = t.tracks[1].clips.map((x) => [x.timeline_start, x.timeline_start + clipDuration(x), x.source_in, x.source_out]);
		expect(sound).toEqual([
			[2, 4, 0, 2],
			[4, 10, 4, 10]
		]);
	});

	test('a partner the cut misses is untouched and a locked one refuses', () => {
		const { t, c, a } = pair();
		get(t, a).source_out = 3;
		cutClipRangeLinked(t, c, 5, 8);
		expect(t.tracks[1].clips).toHaveLength(1);
		expect(extent(t, a)).toEqual([0, 3]);
		expect(linkPartners(t, c)).toEqual([a]);
		const u = pair();
		lock(u.t, 1);
		const before = JSON.stringify(u.t);
		expect(() => cutClipRangeLinked(u.t, u.c, 3, 5)).toThrow('locked');
		expect(JSON.stringify(u.t)).toBe(before);
	});

	test('a cut that removes a whole head leaves the pair linked', () => {
		const { t, c, a } = pair();
		cutClipRangeLinked(t, c, 0, 4);
		expect([extent(t, c), extent(t, a)]).toEqual([
			[0, 6],
			[0, 6]
		]);
		expect(linkPartners(t, c)).toEqual([a]);
	});
});

describe('speed', () => {
	test('speed is carried as a ratio', () => {
		const { t, c, a } = pair();
		get(t, a).speed = 0.5;
		setSpeedLinked(t, c, 2);
		expect(get(t, c).speed).toBe(2);
		expect(get(t, a).speed).toBe(1);
		setSpeedLinked(t, c, -2);
		expect(get(t, a).speed).toBe(-1);
		expect(() => setSpeedLinked(t, c, 0)).toThrow('non-zero');
		lock(t, 1);
		expect(() => setSpeedLinked(t, c, 1)).toThrow('locked');
		expect(get(t, c).speed).toBe(-2);
	});
});

describe('split and remove', () => {
	test('a trim to the playhead reaches the partner that spans it', () => {
		const { t, c, a } = pair();
		expect(withLinkedCuts(t, [{ clip_id: c, at: 4 }])).toEqual([
			{ clip_id: c, at: 4 },
			{ clip_id: a, at: 4 }
		]);
		const u = pair();
		expect(
			withLinkedCuts(u.t, [
				{ clip_id: u.c, at: 4 },
				{ clip_id: u.a, at: 5 }
			])
		).toHaveLength(2);
	});

	test('a partner the playhead is outside is left alone and a locked one refuses', () => {
		const { t, c, a } = pair();
		get(t, a).source_out = 3;
		expect(withLinkedCuts(t, [{ clip_id: c, at: 6 }])).toHaveLength(1);
		get(t, a).source_out = 10;
		lock(t, 1);
		expect(() => withLinkedCuts(t, [{ clip_id: c, at: 6 }])).toThrow('locked');
	});

	test('a track the request already cuts is not cut twice', () => {
		const asset = uuid();
		const t = timeline([
			lane('video', 'V1', [clip(asset, 0, 10, 0)]),
			lane('audio', 'A1', [clip(asset, 0, 10, 0), clip(asset, 0, 10, 20)])
		]);
		const [c, a, other] = [idOf(t, 0, 0), idOf(t, 1, 0), idOf(t, 1, 1)];
		linkClips(t, [c, a]);
		expect(
			withLinkedCuts(t, [
				{ clip_id: c, at: 4 },
				{ clip_id: other, at: 24 }
			])
		).toHaveLength(2);
	});
});

/** V1: a [0,5) b [5,10); A1: the same cut, partners pairwise. */
function twoCutPairs(): { t: Timeline; ids: [string, string, string, string]; asset: string } {
	const asset = uuid();
	const t = timeline([
		lane('video', 'V1', [clip(asset, 10, 15, 0), clip(asset, 20, 25, 5)]),
		lane('audio', 'A1', [clip(asset, 10, 15, 0), clip(asset, 20, 25, 5)])
	]);
	const ids: [string, string, string, string] = [idOf(t, 0, 0), idOf(t, 0, 1), idOf(t, 1, 0), idOf(t, 1, 1)];
	linkClips(t, [ids[0], ids[2]]);
	linkClips(t, [ids[1], ids[3]]);
	return { t, ids, asset };
}

describe('roll / slip / slide', () => {
	test('a roll moves the cut of every partner pair by the same amount', () => {
		const { t, ids: [va, vb, aa, ab], asset } = twoCutPairs();
		const out = rollEditLinked(t, va, vb, 1, limits([asset]));
		expect(out.applied).toBe(1);
		expect(out.clips).toHaveLength(4);
		expect([extent(t, va), extent(t, vb), extent(t, aa), extent(t, ab)]).toEqual([
			[0, 6],
			[6, 10],
			[0, 6],
			[6, 10]
		]);
	});

	test('a roll clamps to the tightest pair', () => {
		const { t, ids: [va, vb, aa, ab], asset } = twoCutPairs();
		const sound = uuid();
		for (const id of [aa, ab]) get(t, id).asset_id = sound;
		const footage: SourceLimits = new Map([
			[asset, X],
			[sound, 15.5]
		]);
		const range = rollRangeLinked(t, va, vb, footage);
		same(range.max, 0.5);
		const out = rollEditLinked(t, va, vb, 2, footage);
		expect(out.clamped).toBe(true);
		same(out.applied, 0.5);
		same(extent(t, va)[1], 5.5);
		same(extent(t, aa)[1], 5.5);
		same(extent(t, ab)[0], 5.5);
	});

	test('a partner running through the cut is not rolled', () => {
		const { t, ids: [va, vb, aa], asset } = twoCutPairs();
		t.tracks[1].clips.length = 1;
		get(t, aa).source_out = 20;
		const out = rollEditLinked(t, va, vb, 1, limits([asset]));
		expect(out.clips).toHaveLength(2);
		expect(extent(t, aa)).toEqual([0, 10]);
		expect([extent(t, va), extent(t, vb)]).toEqual([
			[0, 6],
			[6, 10]
		]);
	});

	test('a roll with a locked partner refuses', () => {
		const { t, ids: [va, vb], asset } = twoCutPairs();
		lock(t, 1);
		expect(() => rollEditLinked(t, va, vb, 1, limits([asset]))).toThrow('locked');
		expect(extent(t, va)).toEqual([0, 5]);
	});

	test('a slip slips the partner by the same moment of footage', () => {
		const { t, ids: [va, , aa], asset } = twoCutPairs();
		const out = slipClipLinked(t, va, 1.5, limits([asset]));
		expect(out.applied).toBe(1.5);
		expect(out.clips).toHaveLength(2);
		expect([get(t, va).source_in, get(t, aa).source_in]).toEqual([11.5, 11.5]);
		expect([extent(t, va), extent(t, aa)]).toEqual([
			[0, 5],
			[0, 5]
		]);
	});

	test('a slip converts between speeds and skips a still', () => {
		const { t, ids: [va, , aa], asset } = twoCutPairs();
		Object.assign(get(t, aa), { speed: 2, source_in: 10, source_out: 20 });
		slipClipLinked(t, va, 1, limits([asset]));
		expect(get(t, va).source_in).toBe(11);
		expect(get(t, aa).source_in).toBe(12);
		const still = uuid();
		t.tracks.push(lane('video', 'V2', [clip(still, 0, 5, 0)]));
		const s = idOf(t, 2, 0);
		linkClips(t, [va, aa, s]);
		const footage: SourceLimits = new Map([
			[asset, X],
			[still, Infinity]
		]);
		expect(slipClipLinked(t, va, 0.5, footage).clips).toHaveLength(2);
	});

	test('a slip clamps to the tightest member', () => {
		const { t, ids: [va, , aa], asset } = twoCutPairs();
		Object.assign(get(t, aa), { source_in: 1, source_out: 6 });
		Object.assign(get(t, va), { source_in: 3, source_out: 8 });
		const range = slipRangeLinked(t, va, limits([asset]));
		same(range.min, -1);
		const out = slipClipLinked(t, va, -2.5, limits([asset]));
		expect(out.clamped).toBe(true);
		same(out.applied, -1);
		same(get(t, va).source_in, 2);
		same(get(t, aa).source_in, 0);
	});

	test('a slide slides the partner with its own neighbours giving way', () => {
		const { t, ids: [va, vb, aa, ab], asset } = twoCutPairs();
		const asset2 = uuid();
		t.tracks[0].clips.push(clip(asset2, 10, 15, 10));
		t.tracks[1].clips.push(clip(asset2, 10, 15, 10));
		const [vc, ac] = [idOf(t, 0, 2), idOf(t, 1, 2)];
		linkClips(t, [vc, ac]);
		const out = slideClipLinked(t, vb, 1, limits([asset, asset2]));
		expect(out.applied).toBe(1);
		for (const [prev, mid, next] of [
			[va, vb, vc],
			[aa, ab, ac]
		]) {
			expect(extent(t, prev)).toEqual([0, 6]);
			expect(extent(t, mid)).toEqual([6, 11]);
			expect(extent(t, next)).toEqual([11, 15]);
		}
	});

	test('a slide clamps to the tightest member', () => {
		const { t, ids: [, vb, , ab], asset } = twoCutPairs();
		t.tracks[0].clips.push(clip(asset, 30, 35, 10));
		t.tracks[1].clips.push(clip(asset, 30, 30.2, 10));
		const [vc, ac] = [idOf(t, 0, 2), idOf(t, 1, 2)];
		const footage = limits([asset]);
		expect(slideRangeLinked(t, vb, footage).max).toBeCloseTo(0.15, 9);
		const out = slideClipLinked(t, vb, 1, footage);
		expect(out.clamped).toBe(true);
		same(out.applied, 0.15);
		same(start(t, vc), 10.15);
		same(start(t, ac), 10.15);
		same(start(t, ab), 5.15);
	});

	test('a slide moves a partner at another offset by the same time', () => {
		const { t, ids: [, vb, aa, ab], asset } = twoCutPairs();
		get(t, aa).source_out = 16;
		get(t, ab).timeline_start = 6;
		const out = slideClipLinked(t, vb, 1, limits([asset]));
		expect(out.applied).toBe(1);
		expect(extent(t, vb)[0]).toBe(6);
		expect(extent(t, ab)[0]).toBe(7);
		expect(extent(t, aa)[1]).toBe(7);
	});

	test('a slide or slip with a locked partner refuses and changes nothing', () => {
		const { t, ids: [, vb], asset } = twoCutPairs();
		lock(t, 1);
		const before = JSON.stringify(t);
		expect(() => slideClipLinked(t, vb, 1, limits([asset]))).toThrow('locked');
		expect(() => slipClipLinked(t, vb, 1, limits([asset]))).toThrow('locked');
		expect(JSON.stringify(t)).toBe(before);
	});
});

/** V1: title [0,5) then c2 [5,15); A1: a2 [5,15) linked to c2. The title has no sound. */
function titleThenShot(): { before: Timeline; title: string; c2: string; a2: string } {
	const asset = uuid();
	const before = timeline([
		lane('video', 'V1', [clip(asset, 0, 5, 0), clip(asset, 20, 30, 5)]),
		lane('audio', 'A1', [clip(asset, 20, 30, 5)])
	]);
	const [title, c2, a2] = [idOf(before, 0, 0), idOf(before, 0, 1), idOf(before, 1, 0)];
	linkClips(before, [c2, a2]);
	return { before, title, c2, a2 };
}

const trimmedTitle = (before: Timeline, title: string, by: number): Timeline => {
	const after = structuredClone(before);
	get(after, title).source_out += by;
	return after;
};

describe('ripple: the sync lock', () => {
	test('a ripple takes the linked partner along', () => {
		const { before, title, c2, a2 } = titleThenShot();
		const after = trimmedTitle(before, title, -1);
		const out = rippleFrom(after, before);
		expect(start(out, c2)).toBe(4);
		expect(start(out, a2)).toBe(4);
		const off = rippleFrom(after, before, false);
		expect([start(off, c2), start(off, a2)]).toEqual([4, 5]);
	});

	test('only linked clips follow, never the rest of the lane', () => {
		const { before, title, c2, a2 } = titleThenShot();
		before.tracks[1].clips.push(clip(get(before, c2).asset_id, 0, 5, 20));
		const music = idOf(before, 1, 1);
		const out = rippleFrom(trimmedTitle(before, title, -1), before);
		expect(start(out, a2)).toBe(4);
		expect(start(out, music)).toBe(20);
	});

	test('a partner that would overlap stays where it was', () => {
		const { before, title, c2, a2 } = titleThenShot();
		before.tracks[1].clips.unshift(clip(get(before, c2).asset_id, 0, 5, 0));
		const blocker = idOf(before, 1, 0);
		const out = rippleFrom(trimmedTitle(before, title, -1), before);
		expect(start(out, c2)).toBe(4);
		expect([start(out, a2), start(out, blocker)]).toEqual([5, 0]);
	});

	test('a locked partner track does not move', () => {
		const { before, title, c2, a2 } = titleThenShot();
		lock(before, 1);
		const out = rippleFrom(trimmedTitle(before, title, -1), before);
		expect([start(out, c2), start(out, a2)]).toEqual([4, 5]);
	});

	test('tracks that already rippled the same way are not shifted twice', () => {
		const asset = uuid();
		const before = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0), clip(asset, 20, 30, 5)]),
			lane('audio', 'A1', [clip(asset, 0, 5, 0), clip(asset, 20, 30, 5)])
		]);
		const ids = [idOf(before, 0, 0), idOf(before, 0, 1), idOf(before, 1, 0), idOf(before, 1, 1)];
		linkClips(before, [ids[0], ids[2]]);
		linkClips(before, [ids[1], ids[3]]);
		const after = structuredClone(before);
		const was = structuredClone(get(after, ids[0]));
		get(after, ids[0]).source_out = 4;
		carryExtentEdit(after, ids[0], was, limits([asset]));
		const out = rippleFrom(after, before);
		expect([start(out, ids[1]), start(out, ids[3])]).toEqual([4, 4]);
	});

	test('a group whose members were rippled by different amounts follows the first that moved', () => {
		const asset = uuid();
		const before = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0), clip(asset, 20, 30, 5)]),
			lane('audio', 'A1', [clip(asset, 0, 6, 0), clip(asset, 20, 30, 6)]),
			lane('audio', 'A2', [clip(asset, 20, 30, 5)])
		]);
		const [c2, a2, b2] = [idOf(before, 0, 1), idOf(before, 1, 1), idOf(before, 2, 0)];
		linkClips(before, [c2, a2, b2]);
		const after = structuredClone(before);
		after.tracks[0].clips[0].source_out = 4;
		after.tracks[1].clips[0].source_out = 4;
		const out = rippleFrom(after, before);
		// No clip was named, so the first member that moved speaks for the group (V1, by 1 s)
		// and the others keep the relationship they had to it.
		expect([start(out, c2), start(out, a2), start(out, b2)]).toEqual([4, 5, 4]);
	});

	test('a left trim that ripples pulls a partner that never shared the head along', () => {
		const asset = uuid();
		const before = timeline([
			lane('video', 'V1', [clip(asset, 0, 10, 0), clip(asset, 20, 26, 10)]),
			lane('audio', 'A1', [clip(asset, 5, 10, 5)])
		]);
		const [c1, a1] = [idOf(before, 0, 0), idOf(before, 1, 0)];
		linkClips(before, [c1, a1]);
		const after = structuredClone(before);
		Object.assign(get(after, c1), { source_in: 3, timeline_start: 3 });
		carryExtentEdit(after, c1, get(before, c1), limits([asset]));
		expect(extent(after, a1)).toEqual([5, 10]);
		const out = rippleFrom(after, before);
		expect(extent(out, c1)).toEqual([0, 7]);
		expect(start(out, idOf(out, 0, 1))).toBe(7);
		// The picture's content slid 3 s earlier (its start was put back while its in-point
		// moved on), so the sound that goes with it slides too — whole, not cut.
		expect(extent(out, a1)).toEqual([2, 7]);
		expect(firstSyncBreak(out, before)).toBeNull();
	});
});

// ---- the sync lock: range-based, for J- and L-cuts ---------------------------------

/** V1 holds x1 (5..15, footage 105..115) and x2 (15..25, footage 215..225); A1 their sound,
 *  which leads the first picture by 5 s (y1 0..13, trimmed to end where y2 begins) and the
 *  second by 2 s (y2 13..25). Each sound is in step with its picture while covering a
 *  different stretch. */
function jlCut(): { t: Timeline; x1: string; x2: string; y1: string; y2: string; asset: string } {
	const asset = uuid();
	const t = timeline([
		lane('video', 'V1', [clip(asset, 105, 115, 5), clip(asset, 215, 225, 15)]),
		lane('audio', 'A1', [clip(asset, 100, 115, 0), clip(asset, 213, 225, 13)])
	]);
	t.tracks[1].clips[0].source_out = 113;
	const [x1, x2, y1, y2] = [idOf(t, 0, 0), idOf(t, 0, 1), idOf(t, 1, 0), idOf(t, 1, 1)];
	linkClips(t, [x1, y1]);
	linkClips(t, [x2, y2]);
	return { t, x1, x2, y1, y2, asset };
}

/** The edit `Project::run_edit` makes: per-lane ripple, then the sync lock with `anchors` named. */
function edited(before: Timeline, after: Timeline, ripple: boolean, anchors: string[]): Timeline {
	return editedNoted(before, after, ripple, anchors)[0];
}

/** `edited`, judging "moved apart" on what the edit itself left (`after`, before the per-lane ripple)
 *  and reporting the sounds the lock trimmed — `Project::run_edit`'s own order. */
function editedNoted(before: Timeline, after: Timeline, ripple: boolean, anchors: string[]): [Timeline, string[]] {
	const out = ripple ? rippleLanes(after, before) : structuredClone(after);
	const notes: string[] = [];
	conformLinks(out, before, new Set(anchors), new Map(), after, notes);
	return [out, notes];
}

describe('the sync lock', () => {
	test('a ripple delete of a J/L-cut closes by the picture removed and keeps every later pair in step', () => {
		const { t, x1, x2, y1, y2 } = jlCut();
		const before = structuredClone(t);
		expect(rippleDeleteLinked(t, x1)).toBe(2);
		expect(clipById(t, x1)).toBeUndefined();
		expect(clipById(t, y1)).toBeUndefined();
		expect(extent(t, x2)).toEqual([5, 15]);
		expect(extent(t, y2)).toEqual([3, 15]);
		expect(firstSyncBreak(t, before)).toBeNull();
	});

	test('a ripple delete leaves an unlinked clip on the partners track where it was', () => {
		const { t, x1, x2, y2, asset } = jlCut();
		t.tracks[1].clips.push(clip(asset, 0, 4, 40));
		const bed = idOf(t, 1, 2);
		rippleDeleteLinked(t, x1);
		expect(extent(t, bed)).toEqual([40, 44]);
		expect([start(t, x2), start(t, y2)]).toEqual([5, 3]);
	});

	test('a follower that would run into an unlinked clip refuses and names the lane', () => {
		const { t, x1, asset } = jlCut();
		t.tracks[1].clips.push(clip(asset, 0, 2, 2));
		const json = JSON.stringify(t);
		let err = '';
		try {
			rippleDeleteLinked(t, x1);
		} catch (e) {
			err = (e as Error).message;
		}
		expect(err).toContain('A1');
		expect(err).toContain('not linked');
		expect(err).not.toContain('links off');
		expect(JSON.stringify(t)).toBe(json);
	});

	test('a follower that lands on linked material trims it back, and one it would cover refuses', () => {
		const { t, x1, x2, y1, y2 } = jlCut();
		const before = structuredClone(t);
		const after = structuredClone(t);
		get(after, x1).source_out = 112;
		carryExtentEdit(after, x1, get(before, x1), limits([uuid()]));
		const out = edited(before, after, true, [x1]);
		expect(extent(out, x2)).toEqual([12, 22]);
		expect(extent(out, y2)).toEqual([10, 22]);
		expect(extent(out, y1)).toEqual([0, 10]);
		expect(firstSyncBreak(out, before)).toBeNull();

		t.tracks[1].clips[0].source_out = 101;
		t.tracks[1].clips[1].timeline_start = 1.5;
		t.tracks[1].clips[1].source_in = 201.5;
		const b2 = structuredClone(t);
		const a2 = structuredClone(t);
		get(a2, x2).timeline_start = 0;
		expect(() => edited(b2, a2, false, [x2])).toThrow(/cover.*A1|A1.*cover/);
	});

	test('a follower pulled before zero loses its head and keeps its sync', () => {
		const { t, x1, x2, y1, y2 } = jlCut();
		t.tracks.forEach((track) => (track.clips = track.clips.filter((c) => c.id !== x1 && c.id !== y1)));
		const before = structuredClone(t);
		const after = structuredClone(t);
		get(after, x2).timeline_start = 0;
		const out = edited(before, after, false, [x2]);
		const y = get(out, y2);
		expect([y.timeline_start, y.timeline_start + clipDuration(y)]).toEqual([0, 10]);
		expect(y.source_in).toBe(215);
		expect(firstSyncBreak(out, before)).toBeNull();
	});

	test('the clip an edit names speaks for the group, and its track for the rest', () => {
		const { t, x1, x2, y2 } = jlCut();
		const before = structuredClone(t);
		const after = structuredClone(t);
		get(after, x2).timeline_start += 6;
		get(after, y2).timeline_start += 4;
		const at = (anchors: string[]) => {
			const out = edited(before, after, false, anchors);
			return [start(out, x2), start(out, y2)];
		};
		expect(at([x2])).toEqual([21, 19]);
		expect(at([y2])).toEqual([19, 17]);
		expect(at([x1])).toEqual([21, 19]);
		expect(at([])).toEqual([21, 19]);
	});

	test('two partners both named and moved apart are left for the guard', () => {
		const { t, x2, y2 } = jlCut();
		const before = structuredClone(t);
		const after = structuredClone(t);
		get(after, x2).timeline_start += 6;
		get(after, y2).timeline_start += 4;
		const out = edited(before, after, false, [x2, y2]);
		expect([start(out, x2), start(out, y2)]).toEqual([21, 17]);
		expect(firstSyncBreak(out, before)).not.toBeNull();
	});

	test('a follower on a locked track refuses', () => {
		const { t, x1 } = jlCut();
		lock(t, 1);
		const before = structuredClone(t);
		const after = structuredClone(t);
		get(after, x1).timeline_start += 1;
		expect(() => edited(before, after, false, [x1])).toThrow(/locked.*A1|A1.*locked/);
	});

	test('a speed change re-places the partners about the named clip', () => {
		const { t, x1, x2, y1, y2 } = jlCut();
		const before = structuredClone(t);
		const after = structuredClone(t);
		setSpeedLinked(after, x1, 2);
		expect(get(after, y1).speed).toBe(2);
		const out = edited(before, after, true, [x1]);
		expect(extent(out, x1)).toEqual([5, 10]);
		expect(extent(out, y1)).toEqual([2.5, 8]);
		expect(extent(out, x2)).toEqual([10, 20]);
		expect(extent(out, y2)).toEqual([8, 20]);
		expect(firstSyncBreak(out, before)).toBeNull();
	});

	test('a cut range whose stretch swallows a partners head resumes it at the cut', () => {
		const asset = uuid();
		const t = timeline([lane('video', 'V1', [clip(asset, 0, 20, 0)]), lane('audio', 'A1', [clip(asset, 8, 20, 8)])]);
		const [x, y] = [idOf(t, 0, 0), idOf(t, 1, 0)];
		linkClips(t, [x, y]);
		const before = structuredClone(t);
		const kept = cutClipRangeLinked(t, x, 5, 12);
		expect(kept).toHaveLength(2);
		expect(extent(t, x)).toEqual([0, 5]);
		expect(extent(t, kept[1].id)).toEqual([5, 13]);
		expect(idOf(t, 1, 0)).toBe(y);
		expect(extent(t, y)).toEqual([5, 13]);
		expect(get(t, y).source_in).toBe(12);
		expect(linkPartners(t, kept[1].id)).toEqual([y]);
		expect(linkPartners(t, x)).toEqual([]);
		expect(firstSyncBreak(t, before)).toBeNull();
	});

	test('a cut range cuts a partner that spans the stretch in two and the tail follows', () => {
		const { t, x1, x2, y2 } = jlCut();
		const kept = cutClipRangeLinked(t, x1, 108, 111);
		expect(kept).toHaveLength(2);
		expect(extent(t, x1)).toEqual([5, 8]);
		expect(extent(t, kept[1].id)).toEqual([8, 12]);
		const sound = t.tracks[1].clips.map((c) => [c.timeline_start, c.timeline_start + clipDuration(c)]);
		expect(sound[0]).toEqual([0, 8]);
		expect(sound[1]).toEqual([8, 10]);
		expect([extent(t, x2), extent(t, y2)]).toEqual([
			[12, 22],
			[10, 22]
		]);
	});

	test('a split hands an unsplit partner to the side it lies on', () => {
		const asset = uuid();
		const t = timeline([lane('video', 'V1', [clip(asset, 0, 10, 0)]), lane('audio', 'A1', [clip(asset, 5, 10, 5)])]);
		const [c, a] = [idOf(t, 0, 0), idOf(t, 1, 0)];
		linkClips(t, [c, a]);
		const [left, right] = splitClipLinked(t, c, 3);
		expect(linkPartners(t, left.id)).toEqual([]);
		expect(get(t, left.id).link_id).toBeUndefined();
		expect(linkPartners(t, right.id)).toEqual([a]);
		expect(withLinkedMoves(t, [mv(t, left.id, 1)])).toHaveLength(1);
		expect(withLinkedMoves(t, [mv(t, right.id, 4)])).toHaveLength(2);

		const u = timeline([lane('video', 'V1', [clip(asset, 0, 10, 0)]), lane('audio', 'A1', [clip(asset, 0, 4, 0)])]);
		const [c2, a2] = [idOf(u, 0, 0), idOf(u, 1, 0)];
		linkClips(u, [c2, a2]);
		const [l2, r2] = splitClipLinked(u, c2, 6);
		expect(linkPartners(u, l2.id)).toEqual([a2]);
		expect(get(u, r2.id).link_id).toBeUndefined();
	});

	test('orphaned links are dissolved in one pass', () => {
		const { t, c, a, asset } = pair();
		const lone = clip(asset, 0, 1, 50);
		lone.link_id = uuid();
		t.tracks[0].clips.push(lone);
		expect(dissolveAllOrphans(t)).toBe(true);
		expect(get(t, lone.id).link_id).toBeUndefined();
		expect(linkPartners(t, c)).toEqual([a]);
		expect(dissolveAllOrphans(t)).toBe(false);
		expect(linkedClipIds(t)).toEqual(new Set([c, a]));
	});

	test('detaching several clips skips what cannot be and fails only when all are', () => {
		const asset = uuid();
		const silent = uuid();
		const t = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0), clip(silent, 0, 5, 5), clip(asset, 5, 9, 10)]),
			lane('audio', 'A1', [])
		]);
		const ids = t.tracks[0].clips.map((c) => c.id);
		const done = detachAudioMany(t, ids, (a) => a === asset);
		expect([done.detached.length, done.skipped.length]).toEqual([2, 1]);
		expect(done.skipped[0].clip_id).toBe(ids[1]);
		expect(done.skipped[0].reason).toContain('no audio');
		const json = JSON.stringify(t);
		expect(() => detachAudioMany(t, ids, (a) => a === asset)).toThrow(/already detached|no audio/);
		expect(JSON.stringify(t)).toBe(json);
	});
});

describe('the beat snap re-sync', () => {
	test('a lane-level retime is carried to the partners afterwards', () => {
		const asset = uuid();
		const t = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0), clip(asset, 20, 30, 5)]),
			lane('audio', 'A1', [clip(asset, 0, 5, 0), clip(asset, 20, 30, 5)])
		]);
		const ids = [idOf(t, 0, 0), idOf(t, 0, 1), idOf(t, 1, 0), idOf(t, 1, 1)];
		linkClips(t, [ids[0], ids[2]]);
		linkClips(t, [ids[1], ids[3]]);
		const before = structuredClone(t);
		get(t, ids[0]).source_out = 5.5;
		Object.assign(get(t, ids[1]), { timeline_start: 5.5, source_out: 29 });
		carryLinksSince(t, before, limits([asset]));
		expect(extent(t, ids[2])).toEqual([0, 5.5]);
		expect(extent(t, ids[3])).toEqual([5.5, 14.5]);
		expect(get(t, ids[3]).source_in).toBe(20);
		expect(get(t, ids[3]).source_out).toBe(29);
	});

	test('a group the edit changed in several members is left as made', () => {
		const { t, c, a, asset } = pair();
		const before = structuredClone(t);
		get(t, c).source_out = 7;
		get(t, a).source_out = 6;
		carryLinksSince(t, before, limits([asset]));
		expect([extent(t, c), extent(t, a)]).toEqual([
			[0, 7],
			[0, 6]
		]);
	});
});

function pictureTimeline(): { t: Timeline; id: string } {
	const asset = uuid();
	const t = timeline([lane('video', 'V1', [clip(asset, 2, 12, 3)]), lane('audio', 'A1', [])]);
	return { t, id: idOf(t, 0, 0) };
}

describe('detach / reattach', () => {
	test('detaching makes a linked audio clip of the same span and mutes the picture', () => {
		const { t, id } = pictureTimeline();
		Object.assign(get(t, id), {
			speed: 1.25,
			volume: 0.7,
			fade_in: 0.3,
			fade_out: 0.6,
			audio: [{ type: 'highpass', hz: 90 }]
		});
		const picture = structuredClone(get(t, id));
		const d = detachAudio(t, id, true);
		expect(d.created_track).toBe(false);
		expect(d.track_id).toBe(t.tracks[1].id);
		const audio = d.clip;
		expect(audio.asset_id).toBe(picture.asset_id);
		expect([audio.source_in, audio.source_out]).toEqual([2, 12]);
		expect([audio.timeline_start, audio.speed]).toEqual([3, 1.25]);
		expect([audio.volume, audio.fade_in, audio.fade_out]).toEqual([0.7, 0.3, 0.6]);
		expect(audio.audio).toEqual(picture.audio);
		expect(get(t, id).source_audio).toBe(false);
		expect(get(t, id).link_id).toBe(audio.link_id);
		expect(get(t, id).link_id).toBeTruthy();
		expect(linkPartners(t, id)).toEqual([audio.id]);
		expect(get(t, id).volume).toBe(0.7);
		expect(() => detachAudio(t, id, true)).toThrow('already detached');
	});

	test('the audio goes to the track at the pictures own position when it has room', () => {
		const asset = uuid();
		const t = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0)]),
			lane('video', 'V2', [clip(asset, 0, 5, 0)]),
			lane('audio', 'A1', []),
			lane('audio', 'A2', [])
		]);
		expect(detachAudio(t, idOf(t, 1, 0), true).track_id).toBe(t.tracks[3].id);
		expect(detachAudio(t, idOf(t, 0, 0), true).track_id).toBe(t.tracks[2].id);
	});

	test('a busy or locked audio track is skipped and a new one made when none fits', () => {
		const asset = uuid();
		const t = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0)]),
			lane('audio', 'A1', [clip(asset, 0, 9, 2)]),
			lane('audio', 'A2', [])
		]);
		lock(t, 2);
		const d = detachAudio(t, idOf(t, 0, 0), true);
		expect(d.created_track).toBe(true);
		expect(t.tracks).toHaveLength(4);
		expect(t.tracks[3].name).toBe('A3');
		expect(d.track_id).toBe(t.tracks[3].id);
		const bare = timeline([lane('video', 'V1', [clip(asset, 0, 5, 0)])]);
		const made = detachAudio(bare, idOf(bare, 0, 0), true);
		expect(made.created_track).toBe(true);
		expect(bare.tracks[1].name).toBe('A1');
	});

	test('detaching refuses what has no sound to detach', () => {
		const { t, id } = pictureTimeline();
		expect(() => detachAudio(t, id, false)).toThrow('no audio stream');
		lock(t, 0);
		expect(() => detachAudio(t, id, true)).toThrow('locked');
		t.tracks[0].locked = false;
		const d = detachAudio(t, id, true);
		expect(() => detachAudio(t, d.clip.id, true)).toThrow('video track');
		expect(() => detachAudio(t, uuid(), true)).toThrow('clip not found');
		const before = JSON.stringify(t);
		expect(() => detachAudio(t, id, true)).toThrow();
		expect(JSON.stringify(t)).toBe(before);
	});

	test('a group never puts two clips on one track', () => {
		const asset = uuid();
		const t = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0)]),
			lane('audio', 'A1', [clip(asset, 50, 55, 20)]),
			lane('audio', 'A2', [])
		]);
		const [v, other] = [idOf(t, 0, 0), idOf(t, 1, 0)];
		linkClips(t, [v, other]);
		const d = detachAudio(t, v, true);
		expect(d.track_id).toBe(t.tracks[2].id);
		expect(linkPartners(t, v)).toHaveLength(2);
	});

	test('reattaching deletes the audio clip and unmutes the picture', () => {
		const { t, id } = pictureTimeline();
		const d = detachAudio(t, id, true);
		const picture = reattachAudio(t, id);
		expect(picture.source_audio).toBeUndefined();
		expect(picture.link_id).toBeUndefined();
		expect(clipById(t, d.clip.id)).toBeUndefined();
		expect(t.tracks[1].clips).toHaveLength(0);
		const again = detachAudio(t, id, true);
		expect(reattachAudio(t, again.clip.id).source_audio).toBeUndefined();
		expect(clipById(t, again.clip.id)).toBeUndefined();
		expect(() => reattachAudio(t, id)).toThrow('not detached');
		expect(() => reattachAudio(t, uuid())).toThrow('clip not found');
	});

	test('reattaching with the audio already gone just unmutes', () => {
		const { t, id } = pictureTimeline();
		detachAudio(t, id, true);
		t.tracks[1].clips = [];
		expect(reattachAudio(t, id).source_audio).toBeUndefined();
	});

	test('reattaching refuses a locked audio track', () => {
		const { t, id } = pictureTimeline();
		detachAudio(t, id, true);
		lock(t, 1);
		expect(() => reattachAudio(t, id)).toThrow('locked');
		expect(get(t, id).source_audio).toBe(false);
	});

	test('reattaching several is all or nothing and a pair named twice counts once', () => {
		const asset = uuid();
		const t = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0), clip(asset, 10, 15, 5)]),
			lane('audio', 'A1', [])
		]);
		const [x1, x2] = [idOf(t, 0, 0), idOf(t, 0, 1)];
		const s1 = detachAudio(t, x1, true).clip.id;
		const s2 = detachAudio(t, x2, true).clip.id;
		// The picture and its sound are one pair; the order named is the order returned.
		const done = structuredClone(t);
		expect(reattachAudioMany(done, [s2, x2, x1]).map((c) => c.id)).toEqual([x2, x1]);
		expect(done.tracks[1].clips).toHaveLength(0);
		expect([get(done, x1).source_audio, get(done, x2).source_audio]).toEqual([undefined, undefined]);
		// A second picture whose reattach is refused leaves the first one's sound where it was.
		unlinkClips(t, [x2]);
		const before = structuredClone(t);
		expect(() => reattachAudioMany(t, [x1, x2])).toThrow(new RegExp(`heard twice.*\\(clip ${x2}\\)`));
		expect(t).toEqual(before);
		expect(get(t, s1)).toBeDefined();
		expect(get(t, s2)).toBeDefined();
		// Alone, the refusal is reattachAudio's own words.
		const alone = (() => {
			try {
				reattachAudio(structuredClone(t), x2);
			} catch (e) {
				return (e as Error).message;
			}
		})();
		expect(() => reattachAudioMany(t, [x2])).toThrow(alone!);
		expect(() => reattachAudioMany(t, [])).toThrow('no clips to reattach');
		expect(() => reattachAudioMany(t, [x1, uuid()])).toThrow('clip not found');
		expect(t).toEqual(before);
	});

	test('a pair split after detaching reattaches piecewise', () => {
		const { t, id } = pictureTimeline();
		detachAudio(t, id, true);
		const [, right] = splitClipLinked(t, id, 8);
		expect(right.source_audio).toBe(false);
		reattachAudio(t, id);
		expect(t.tracks[1].clips).toHaveLength(1);
		expect(get(t, id).source_audio).toBeUndefined();
		expect(get(t, right.id).source_audio).toBe(false);
	});
});

describe('the sync guard', () => {
	test('a pair that stays in step is not a break however far it moved', () => {
		const { t: before, c, a } = pair();
		const t = structuredClone(before);
		for (const id of [c, a]) get(t, id).timeline_start = 7;
		expect(firstSyncBreak(t, before)).toBeNull();
		const u = structuredClone(before);
		Object.assign(get(u, c), { source_in: 3, timeline_start: 3 });
		expect(firstSyncBreak(u, before)).toBeNull();
	});

	test('moving one clip of a pair is a break and names both tracks', () => {
		const { t: before, c } = pair();
		const t = structuredClone(before);
		get(t, c).timeline_start = 1;
		expect(firstSyncBreak(t, before)).toEqual(['V1', 'A1']);
		const u = structuredClone(before);
		get(u, c).speed = 2;
		expect(firstSyncBreak(u, before)).not.toBeNull();
		const w = structuredClone(before);
		Object.assign(get(w, c), { source_in: 1, source_out: 11 });
		expect(firstSyncBreak(w, before)).not.toBeNull();
	});

	test('a pair that was already apart or cannot be compared is left alone', () => {
		const { t: before, c, a } = pair();
		get(before, a).timeline_start = 2;
		const t = structuredClone(before);
		get(t, c).timeline_start = 5;
		expect(firstSyncBreak(t, before)).toBeNull();
		const u = pair();
		get(u.t, u.a).asset_id = uuid();
		const moved = structuredClone(u.t);
		get(moved, u.c).timeline_start = 5;
		expect(firstSyncBreak(moved, u.t)).toBeNull();
		const v = pair();
		const gone = structuredClone(v.t);
		gone.tracks[1].clips = [];
		get(gone, v.c).timeline_start = 5;
		expect(firstSyncBreak(gone, v.t)).toBeNull();
	});

	test('a reversed pair is compared from its out point', () => {
		const { t: before, c, a } = pair();
		for (const id of [c, a]) get(before, id).speed = -1;
		const t = structuredClone(before);
		for (const id of [c, a]) Object.assign(get(t, id), { source_out: 7, timeline_start: 3 });
		expect(firstSyncBreak(t, before)).toBeNull();
		get(t, c).timeline_start = 3.5;
		expect(firstSyncBreak(t, before)).not.toBeNull();
	});
});

// ---- the second review: authority as the edit left it, leftovers, victims, the fader fold ----------

describe('named partners and the ripple', () => {
	test('named partners that the edit cut together are one authority whatever the ripple did', () => {
		// x1 5..15 / y1 0..13, then x2 15..25 / y2 13..25: trim to the playhead at 8 from the left
		// names *both* x1 and y1 and cuts them at the same time — they agree.
		const { t, x1, x2, y1, y2 } = jlCut();
		const before = structuredClone(t);
		const after = structuredClone(t);
		for (const id of [x1, y1]) {
			const c = get(after, id);
			const head = 8 - c.timeline_start;
			c.source_in += head;
			c.timeline_start = 8;
		}
		const [out, notes] = editedNoted(before, after, true, [x1, y1]);
		expect(extent(out, x1)).toEqual([5, 12]);
		expect(extent(out, y1)).toEqual([5, 10]);
		expect(extent(out, y2)).toEqual([10, 22]);
		same(contentOffset(get(out, x1)), contentOffset(get(out, y1)));
		same(start(out, x2), 12);
		same(contentOffset(get(out, x2)), contentOffset(get(out, y2)));
		expect(firstSyncBreak(out, before)).toBeNull();
		expect(notes.length === 0 || (notes.length === 1 && notes[0] === 'A1')).toBe(true);
		// Moved apart by the edit itself they are still left for the guard.
		const apart = structuredClone(before);
		get(apart, x1).timeline_start += 1;
		get(apart, y1).timeline_start += 2;
		const [parted] = editedNoted(before, apart, true, [x1, y1]);
		expect(firstSyncBreak(parted, before)).not.toBeNull();
	});

	test('the guard names the lowest pair of tracks whatever order it scans in', () => {
		const asset = uuid();
		const before = timeline([
			lane('video', 'V1', [clip(asset, 0, 5, 0)]),
			lane('video', 'V2', [clip(asset, 0, 5, 10)]),
			lane('audio', 'A1', [clip(asset, 0, 5, 0)]),
			lane('audio', 'A2', [clip(asset, 0, 5, 10)])
		]);
		const ids = [0, 1, 2, 3].map((i) => idOf(before, i, 0));
		linkClips(before, [ids[0], ids[2]]);
		linkClips(before, [ids[1], ids[3]]);
		const after = structuredClone(before);
		get(after, ids[2]).timeline_start = 1;
		get(after, ids[3]).timeline_start = 11;
		expect(firstSyncBreak(after, before)).toEqual(['V1', 'A1']);
		// And with the groups met in the other order.
		const swapped = structuredClone(after);
		swapped.tracks = [swapped.tracks[1], swapped.tracks[0], swapped.tracks[3], swapped.tracks[2]];
		const priorSwapped = structuredClone(before);
		priorSwapped.tracks = [priorSwapped.tracks[1], priorSwapped.tracks[0], priorSwapped.tracks[3], priorSwapped.tracks[2]];
		expect(firstSyncBreak(swapped, priorSwapped)).toEqual(['V2', 'A2']);
	});
});

describe('victims of the sync lock', () => {
	test('a follower never trims a picture and reports the sound it does trim', () => {
		// The sound track is named: V1's second picture follows y2 onto the first one.
		const { t, x2, y1, y2 } = jlCut();
		const before = structuredClone(t);
		const after = structuredClone(t);
		get(after, y2).timeline_start -= 4;
		let err = '';
		try {
			editedNoted(before, after, false, [y2]);
		} catch (e) {
			err = (e as Error).message;
		}
		expect(err).toContain('picture');
		expect(err).toContain('V1');
		expect(err).toContain('never trimmed');
		// A sound in the way is trimmed back — and reported.
		const again = structuredClone(t);
		get(again, x2).timeline_start -= 3;
		const [out, notes] = editedNoted(before, again, false, [x2]);
		expect(extent(out, y2)).toEqual([10, 22]);
		expect(extent(out, y1)).toEqual([0, 10]);
		expect(notes).toEqual(['A1']);
	});

	test('a clip that would be left under the floor is refused, not stubbed', () => {
		const { t, x2, y1 } = jlCut();
		// y1 is 13 s long (0..13); y2 would land 12.97 s in: 0.03 s of y1 would remain.
		get(t, y1).source_out = 100 + 13;
		const before = structuredClone(t);
		const ok = structuredClone(t);
		get(ok, x2).timeline_start -= 2.03;
		expect(() => editedNoted(before, ok, false, [x2])).not.toThrow();
		const after = structuredClone(before);
		get(after, x2).timeline_start -= 12.97;
		let err = '';
		try {
			editedNoted(before, after, false, [x2]);
		} catch (e) {
			err = (e as Error).message;
		}
		expect(err).toContain('under 0.05s');
		expect(err).toContain('A1');
	});

	test('a sound pulled before zero is trimmed and reported and a picture refuses', () => {
		const { t, x1, x2, y1, y2 } = jlCut();
		t.tracks.forEach((track) => (track.clips = track.clips.filter((c) => c.id !== x1 && c.id !== y1)));
		const before = structuredClone(t);
		const after = structuredClone(t);
		get(after, x2).timeline_start = 0;
		const [out, notes] = editedNoted(before, after, false, [x2]);
		expect(start(out, y2)).toBe(0);
		expect(notes).toEqual(['A1']);
		// A picture that would have to start before 0 refuses: x 0..10, its sound y 3..10 named and
		// pulled up to 0 takes the picture with it.
		const asset = uuid();
		const u = timeline([lane('video', 'V1', [clip(asset, 0, 10, 0)]), lane('audio', 'A1', [clip(asset, 3, 10, 3)])]);
		const [x, y] = [idOf(u, 0, 0), idOf(u, 1, 0)];
		linkClips(u, [x, y]);
		const priorU = structuredClone(u);
		const afterU = structuredClone(u);
		get(afterU, y).timeline_start = 0;
		let err = '';
		try {
			editedNoted(priorU, afterU, false, [y]);
		} catch (e) {
			err = (e as Error).message;
		}
		expect(err).toContain('V1');
		expect(err).toContain('never trimmed');
	});

	test('a move that carries a sound before zero reports it, and a picture refuses', () => {
		// The picture at 5, its sound leading it by 3 (at 2): moving the picture to 0 would put the sound at -3.
		const asset = uuid();
		const t = timeline([lane('video', 'V1', [clip(asset, 5, 15, 5)]), lane('audio', 'A1', [clip(asset, 2, 15, 2)])]);
		const [x, y] = [idOf(t, 0, 0), idOf(t, 1, 0)];
		linkClips(t, [x, y]);
		const was = structuredClone(get(t, x));
		get(t, x).timeline_start = 0;
		const notes: string[] = [];
		carryExtentEdit(t, x, was, limits([asset]), notes);
		expect(notes).toEqual(['A1']);
		expect(extent(t, y)).toEqual([0, 10]);
		expect(get(t, y).source_in).toBe(5);
		// The other way round, the picture is the one that would start before 0: refused, nothing changes.
		const u = timeline([lane('video', 'V1', [clip(asset, 2, 15, 2)]), lane('audio', 'A1', [clip(asset, 5, 15, 5)])]);
		const [p, s] = [idOf(u, 0, 0), idOf(u, 1, 0)];
		linkClips(u, [p, s]);
		const wasS = structuredClone(get(u, s));
		get(u, s).timeline_start = 0;
		const json = JSON.stringify(u.tracks[0]);
		expect(() => carryExtentEdit(u, s, wasS, limits([asset]))).toThrow(/V1.*never trimmed/);
		expect(JSON.stringify(u.tracks[0])).toBe(json);
		// And a sound that would be left under the floor is refused outright.
		const w = timeline([lane('video', 'V1', [clip(asset, 5, 15, 5)]), lane('audio', 'A1', [clip(asset, 2, 5.04, 2)])]);
		const [px, sx] = [idOf(w, 0, 0), idOf(w, 1, 0)];
		linkClips(w, [px, sx]);
		const wasW = structuredClone(get(w, px));
		get(w, px).timeline_start = 0;
		expect(() => carryExtentEdit(w, px, wasW, limits([asset]))).toThrow('under 0.05s');
		expect(sx).toBeDefined();
	});
});

describe('a cut and what it leaves of a partner', () => {
	test('a cut that leaves a partners head, no group, does not make it an obstacle', () => {
		// V1 X 5..15 / A1 S 0..13, X2 15..25 / S2 13..25 (footage offset -10): cutting all of X.
		const asset = uuid();
		const t = timeline([
			lane('video', 'V1', [clip(asset, 5, 15, 5), clip(asset, 25, 35, 15)]),
			lane('audio', 'A1', [clip(asset, 0, 13, 0), clip(asset, 23, 35, 13)])
		]);
		const ids = [idOf(t, 0, 0), idOf(t, 0, 1), idOf(t, 1, 0), idOf(t, 1, 1)];
		linkClips(t, [ids[0], ids[2]]);
		linkClips(t, [ids[1], ids[3]]);
		const before = structuredClone(t);
		const notes: string[] = [];
		cutClipRangeLinked(t, ids[0], 5, 15, notes);
		// X is gone; S keeps its head (0..5), now trimmed by S2 coming up to 3 — a leftover of a
		// linked clip, so it gives way, and the edit says so.
		expect(extent(t, ids[1])).toEqual([5, 15]);
		expect(extent(t, ids[3])).toEqual([3, 15]);
		expect(extent(t, ids[2])).toEqual([0, 3]);
		expect(notes).toEqual(['A1']);
		expect(get(t, ids[2]).link_id).toBeUndefined();
		expect(firstSyncBreak(t, before)).toBeNull();
	});

	test('a lone leftover of a partner still resumes at the cut', () => {
		// X 0..10 / S 9..15 (the sound starts inside what is cut): cut X's 8..10.
		const asset = uuid();
		const t = timeline([lane('video', 'V1', [clip(asset, 0, 10, 0)]), lane('audio', 'A1', [clip(asset, 9, 15, 9)])]);
		const [x, s] = [idOf(t, 0, 0), idOf(t, 1, 0)];
		linkClips(t, [x, s]);
		cutClipRangeLinked(t, x, 8, 10);
		expect(extent(t, x)).toEqual([0, 8]);
		expect(extent(t, s)).toEqual([8, 13]);
		expect(get(t, s).source_in).toBe(10);
		// A partner wholly after the stretch comes up by it even when the named clip keeps nothing after.
		const u = timeline([lane('video', 'V1', [clip(asset, 0, 10, 0)]), lane('audio', 'A1', [clip(asset, 12, 20, 12)])]);
		const [x2, s2] = [idOf(u, 0, 0), idOf(u, 1, 0)];
		linkClips(u, [x2, s2]);
		cutClipRangeLinked(u, x2, 8, 10);
		expect(extent(u, s2)).toEqual([10, 18]);
	});
});

describe('detach under a compressor or gate', () => {
	const compressor = (): AudioEffect => ({
		type: 'compressor',
		threshold_db: -18,
		ratio: 4,
		attack_ms: 10,
		release_ms: 100,
		makeup_db: 0
	});

	test('meets the same fader instead of folding it', () => {
		for (const dynamic of [compressor(), { type: 'gate', threshold_db: -40 } as AudioEffect]) {
			const asset = uuid();
			const c = clip(asset, 0, 10, 0);
			c.volume = 0.8;
			c.audio = [dynamic];
			const t = timeline([lane('video', 'V1', [c]), lane('audio', 'A1', []), lane('audio', 'A2', [])]);
			t.tracks[0].volume = 0.5;
			t.tracks[1].volume = 2;
			t.tracks[2].volume = 0.5;
			// A1's fader differs; A2's equals the picture track's: the sound goes to A2, volume untouched.
			const d = detachAudio(t, c.id, true);
			expect([d.track_id, d.created_track, d.clip.volume]).toEqual([t.tracks[2].id, false, 0.8]);
			// With no lane at that fader a new one is made at it.
			const c2 = clip(asset, 0, 10, 0);
			c2.volume = 0.8;
			c2.audio = [structuredClone(dynamic)];
			const u = timeline([lane('video', 'V1', [c2]), lane('audio', 'A1', [])]);
			u.tracks[0].volume = 0.5;
			u.tracks[1].volume = 2;
			const made = detachAudio(u, c2.id, true);
			expect(made.created_track).toBe(true);
			const track = u.tracks.find((x) => x.id === made.track_id)!;
			expect([track.name, track.volume, made.clip.volume]).toEqual(['A2', 0.5, 0.8]);
			expect(u.tracks[1].clips).toHaveLength(0);
		}
		// Linear chains still fold, and equal faders need neither.
		const c = clip(uuid(), 0, 10, 0);
		c.audio = [{ type: 'highpass', hz: 80 }];
		const t = timeline([lane('video', 'V1', [c]), lane('audio', 'A1', [])]);
		t.tracks[0].volume = 0.5;
		t.tracks[1].volume = 2;
		const d = detachAudio(t, c.id, true);
		expect(d.created_track).toBe(false);
		same(d.clip.volume * 2, 0.5);
	});
});

describe('the label says what the lock trimmed', () => {
	test('each track once, in the order it happened, and nothing when nothing was trimmed', () => {
		expect(trimmedSuffix([])).toBe('');
		expect(trimmedSuffix(['A1'])).toBe(' (trimmed sound on A1)');
		expect(trimmedSuffix(['A2', 'A1', 'A2', 'A3'])).toBe(' (trimmed sound on A2, A1, A3)');
	});
});
