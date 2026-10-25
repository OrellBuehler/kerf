import { describe, expect, test } from 'bun:test';
import { rollEditLinked, slideClipLinked, slipClipLinked } from './edit-modes';
import { clipById, firstSyncBreak, linkClips, linkPartners } from './link-groups';
import { carryExtentEdit, cutClipRangeLinked, detachAudio, setSpeedLinked, setSpeedRampLinked, splitClipLinked } from './links';
import { conformLinks } from './ripple';
import { clipDuration, localAtAlong, speedAt, timelineToSource, trimRamp } from './speed-ramp';
import type { Clip, SpeedKey, Timeline } from './types';

// `model/ramp_link_tests.rs`, case for case: a picture and its sound keep the same ramp on the timeline through
// every edit that is carried to the group, and the sync lock and guard judge them by the footage they play, not by
// an offset a ramp cannot have.

const ASSET = 'a';
const key = (time: number, speed: number, easing?: 'linear' | 'hold'): SpeedKey => (easing ? { time, speed, easing } : { time, speed });
const keys = (): SpeedKey[] => [key(0.5, 1), key(1.5, 0.4), key(2.5, 0.4, 'hold'), key(3.5, 1.5)];
const footage = (secs: number) => new Map([[ASSET, secs]]);
const near = (a: number, b: number) => expect(Math.abs(a - b)).toBeLessThan(1e-9);

let serial = 0;
const clip = (sourceIn: number, sourceOut: number, start: number, extra: Partial<Clip> = {}): Clip =>
	({ id: `c${++serial}`, asset_id: ASSET, source_in: sourceIn, source_out: sourceOut, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0, ...extra }) as Clip;
const lane = (kind: 'video' | 'audio', name: string, clips: Clip[]) => ({ id: name.toLowerCase(), kind, name, clips });
const clipOf = (t: Timeline, id: string) => clipById(t, id)!;
const end = (c: Clip) => c.timeline_start + clipDuration(c);

/** V1 holds the picture (src 10..16 at 2.0) and A1 its sound (src 8..16 at 0.0): the sound leads by two seconds. */
function leadingSound(): [Timeline, string, string] {
	const [p, s] = [clip(10, 16, 2), clip(8, 16, 0)];
	const t = { tracks: [lane('video', 'V1', [p]), lane('audio', 'A1', [s])] } as unknown as Timeline;
	linkClips(t, [p.id, s.id]);
	return [t, p.id, s.id];
}

/** V1 and A1 hold the same window at the same place, linked. */
function alignedPair(): [Timeline, string, string] {
	const [t, p, s] = leadingSound();
	for (const id of [p, s]) Object.assign(clipOf(t, id), { source_in: 10, source_out: 16, timeline_start: 0 });
	return [t, p, s];
}

const playing = (t: Timeline, track: number, time: number): number | undefined => {
	const c = t.tracks[track].clips.find((x) => x.timeline_start <= time && time < end(x));
	return c ? timelineToSource(c, time) : undefined;
};

/** The picture's footage and the sound's agree at every sampled moment both play. */
function assertInStep(t: Timeline) {
	const total = Math.max(...t.tracks.flatMap((x) => x.clips.map(end)));
	let both = 0;
	for (let i = 0; i <= 200; i++) {
		const at = (total * i) / 200 + 1e-7;
		const [v, a] = [playing(t, 0, at), playing(t, 1, at)];
		if (v !== undefined && a !== undefined) {
			near(v, a);
			both++;
		}
	}
	expect(both).toBeGreaterThan(20);
}

describe('a ramp on a linked group', () => {
	test('lands at the same moment of the timeline in a partner that starts elsewhere', () => {
		const [t, p, s] = leadingSound();
		const before = structuredClone(t);
		const set = setSpeedRampLinked(t, p, keys());
		expect(set.speed_ramp).toEqual(keys());
		expect(clipOf(t, s).speed_ramp!.map((k) => k.time)).toEqual([2.5, 3.5, 4.5, 5.5]);
		expect(clipOf(t, s).speed_ramp![2].easing).toBe('hold');
		for (let i = 0; i <= 60; i++) {
			const at = 2 + 0.1 * i;
			near(speedAt(clipOf(t, p), at - 2), speedAt(clipOf(t, s), at));
		}
		conformLinks(t, before, new Set([p]), new Map());
		expect(firstSyncBreak(t, before)).toBeNull();
		near(clipOf(t, s).timeline_start, 0);
		assertInStep(t);
	});

	test('set on the sound, it reaches the picture the other way round', () => {
		const [t, p, s] = leadingSound();
		setSpeedRampLinked(t, s, keys());
		expect(clipOf(t, p).speed_ramp!.map((k) => k.time)).toEqual([-1.5, -0.5, 0.5, 1.5]);
		for (let i = 0; i <= 60; i++) {
			const at = 2 + 0.1 * i;
			near(speedAt(clipOf(t, p), at - 2), speedAt(clipOf(t, s), at));
		}
	});

	test('a partner at another pace keeps its ratio, and one that would leave the range refuses', () => {
		const [t, p, s] = alignedPair();
		clipOf(t, s).speed = 2;
		setSpeedRampLinked(t, p, [key(0, 1), key(1, 0.5)]);
		expect(clipOf(t, s).speed_ramp!.map((k) => [k.time, k.speed])).toEqual([
			[0, 2],
			[1, 1]
		]);

		const [u, q, r] = alignedPair();
		clipOf(u, r).speed = 50;
		const before = JSON.stringify(u);
		expect(() => setSpeedRampLinked(u, q, [key(0, 4)])).toThrow('200.000×');
		expect(JSON.stringify(u)).toBe(before);
	});

	test('a clear is carried, and a constant speed replaces the ramp on every clip', () => {
		const [t, p, s] = alignedPair();
		setSpeedRampLinked(t, p, keys());
		setSpeedRampLinked(t, p, []);
		expect(clipOf(t, p).speed_ramp).toBeUndefined();
		expect(clipOf(t, s).speed_ramp).toBeUndefined();
		expect(() => setSpeedRampLinked(t, p, [])).toThrow('no speed ramp to clear');
		setSpeedRampLinked(t, p, keys());
		setSpeedLinked(t, s, 2);
		expect(clipOf(t, p).speed_ramp).toBeUndefined();
		expect(clipOf(t, s).speed_ramp).toBeUndefined();
		near(clipOf(t, p).speed!, 2);
		near(clipOf(t, s).speed!, 2);
	});

	test('a locked track refuses it, whether it holds the named clip or a partner', () => {
		for (const lock of [0, 1]) {
			const [t, p] = alignedPair();
			t.tracks[lock].locked = true;
			expect(() => setSpeedRampLinked(t, p, keys())).toThrow('locked');
			expect(t.tracks.every((x) => x.clips.every((c) => !c.speed_ramp))).toBe(true);
		}
	});
});

describe('edits carried to a ramped group', () => {
	test('a trim is carried to a partner that shares the edge, with the same ramp rebase', () => {
		const [t, p, s] = alignedPair();
		setSpeedRampLinked(t, p, keys());
		const was = structuredClone(clipOf(t, p));
		// What `Project::trim` does to the picture: the in-point moves, the ramp follows, the start moves.
		const c = clipOf(t, p);
		c.source_in = 11.1;
		trimRamp(was, c);
		c.timeline_start = was.timeline_start + localAtAlong(was, 1.1);
		expect(carryExtentEdit(t, p, was, footage(100))).toHaveLength(1);
		const a = clipOf(t, s);
		near(a.timeline_start, c.timeline_start);
		near(a.source_in, c.source_in);
		expect(a.speed_ramp!.length).toBe(c.speed_ramp!.length);
		a.speed_ramp!.forEach((k, i) => {
			near(k.time, c.speed_ramp![i].time);
			near(k.speed, c.speed_ramp![i].speed);
		});
		assertInStep(t);
		// And the tail the other way: both lengthen by the same timeline seconds.
		const was2 = structuredClone(clipOf(t, p));
		clipOf(t, p).source_out = 17;
		carryExtentEdit(t, p, was2, footage(100));
		near(clipOf(t, s).source_out, 17);
		near(end(clipOf(t, s)), end(clipOf(t, p)));
	});

	test('a head trim the partner does not share moves no footage and so displaces nothing', () => {
		const [t, p, s] = leadingSound();
		setSpeedRampLinked(t, p, keys());
		const before = structuredClone(t);
		const was = structuredClone(clipOf(t, p));
		const c = clipOf(t, p);
		c.source_in = 11;
		trimRamp(was, c);
		c.timeline_start = was.timeline_start + localAtAlong(was, 1);
		carryExtentEdit(t, p, was, footage(100));
		const sound = clipOf(t, s);
		expect([sound.timeline_start, sound.source_in]).toEqual([0, 8]);
		expect(sound.speed_ramp).toEqual(clipOf(before, s).speed_ramp);
		conformLinks(t, before, new Set([p]), new Map());
		near(clipOf(t, s).timeline_start, 0);
		expect(firstSyncBreak(t, before)).toBeNull();
		assertInStep(t);
	});

	test('a split cuts both and the halves stay in step', () => {
		const [t, p, s] = alignedPair();
		setSpeedRampLinked(t, p, keys());
		const [left, right] = splitClipLinked(t, p, 2.2);
		expect(t.tracks[0].clips).toHaveLength(2);
		expect(t.tracks[1].clips).toHaveLength(2);
		assertInStep(t);
		near(end(left), 2.2);
		near(right.timeline_start, 2.2);
		expect(linkPartners(t, right.id)).toHaveLength(1);
		expect(clipById(t, s)).toBeDefined();
	});

	test('a detached sound carries the ramp', () => {
		const picture = clip(0, 6, 1, { speed_ramp: keys() });
		const t = { tracks: [lane('video', 'V1', [picture]), lane('audio', 'A1', [])] } as unknown as Timeline;
		const detached = detachAudio(t, picture.id, true);
		expect(detached.clip.speed_ramp).toEqual(keys());
		near(clipDuration(detached.clip), clipDuration(clipOf(t, picture.id)));
	});

	/** Three pairs side by side: the first and last on the ramp, the middle one plain. */
	function threePairs(): [Timeline, [string, string][]] {
		const t = { tracks: [lane('video', 'V1', []), lane('audio', 'A1', [])] } as unknown as Timeline;
		const ids: [string, string][] = [];
		for (const [ti] of [[0], [1]]) {
			let at = 0;
			[
				[10, 16, true],
				[50, 53, false],
				[40, 46, true]
			].forEach(([si, so, ramp], i) => {
				const c = clip(si as number, so as number, at, ramp ? { speed_ramp: keys() } : {});
				at = end(c);
				ids[i] = ti === 0 ? [c.id, ''] : [ids[i][0], c.id];
				t.tracks[ti].clips.push(c);
			});
		}
		for (const [p, s] of ids) linkClips(t, [p, s]);
		return [t, ids];
	}

	test('a roll is the same edit on both pairs of a cut', () => {
		const [t, ids] = threePairs();
		const before = structuredClone(t);
		const out = rollEditLinked(t, ids[0][0], ids[1][0], 0.6, footage(100));
		expect(out.clips).toHaveLength(4);
		assertInStep(t);
		near(end(clipOf(t, ids[0][0])), end(clipOf(before, ids[0][0])) + 0.6);
		near(end(clipOf(t, ids[0][0])), end(clipOf(t, ids[0][1])));
		near(clipOf(t, ids[1][0]).timeline_start, clipOf(t, ids[1][1]).timeline_start);
	});

	test('a slide moves both and the ramped neighbours trade footage with it', () => {
		const [t, ids] = threePairs();
		const before = structuredClone(t);
		const out = slideClipLinked(t, ids[1][0], 0.4, footage(100));
		expect(out.clips).toHaveLength(6);
		assertInStep(t);
		for (const [p, s] of ids) {
			near(clipOf(t, p).timeline_start, clipOf(t, s).timeline_start);
			near(end(clipOf(t, p)), end(clipOf(t, s)));
		}
		near(clipOf(t, ids[1][0]).timeline_start, clipOf(before, ids[1][0]).timeline_start + 0.4);
		near(end(clipOf(t, ids[2][0])), end(clipOf(before, ids[2][0])));
	});

	test('a slip moves the footage of both by the same source seconds', () => {
		const [t, ids] = threePairs();
		const before = structuredClone(t);
		slipClipLinked(t, ids[0][0], 1.5, footage(100));
		assertInStep(t);
		near(clipDuration(clipOf(t, ids[0][0])), clipDuration(clipOf(before, ids[0][0])));
		near(clipOf(t, ids[0][0]).source_in, 11.5);
		near(clipOf(t, ids[0][1]).source_in, 11.5);
	});

	test('a slip against a partner on another pace has no one ratio and refuses; on the same ramp it is the same slip', () => {
		const [t, p, s] = alignedPair();
		clipOf(t, p).speed_ramp = keys();
		expect(() => slipClipLinked(t, p, 1, footage(100))).toThrow('another pace');
		expect(clipOf(t, s).speed_ramp).toBeUndefined();
		clipOf(t, s).speed_ramp = keys();
		slipClipLinked(t, p, 1, footage(100));
		near(clipOf(t, p).source_in, clipOf(t, s).source_in);
	});

	test('cutting a source range takes the same stretch from both and keeps them in step', () => {
		const [t, p] = alignedPair();
		setSpeedRampLinked(t, p, keys());
		const whole = clipDuration(clipOf(t, p));
		// Footage 11.2..12.4 plays 1.2 source seconds of the ramp, inside the first segments.
		const c = clipOf(t, p);
		const removed = localAtAlong(c, 2.4) - localAtAlong(c, 1.2);
		cutClipRangeLinked(t, p, 11.2, 12.4);
		expect(t.tracks[0].clips).toHaveLength(2);
		expect(t.tracks[1].clips).toHaveLength(2);
		near(Math.max(...t.tracks.flatMap((x) => x.clips.map(end))), whole - removed);
		for (const track of t.tracks) near(track.clips[1].timeline_start, end(track.clips[0]));
		assertInStep(t);
	});
});

describe('the sync guard on a ramped pair', () => {
	test('reads a pair on the same ramp as in step and one a beat apart as broken', () => {
		const [t, p, s] = alignedPair();
		setSpeedRampLinked(t, p, keys());
		const before = structuredClone(t);
		expect(firstSyncBreak(t, before)).toBeNull();
		// Move the sound alone: the pair was in step and now is not.
		clipOf(t, s).timeline_start += 0.5;
		expect(firstSyncBreak(t, before)).not.toBeNull();
		// A pair on different ramps is never "in step" to begin with, so it is not the edit's doing.
		const apart = structuredClone(before);
		clipOf(apart, s).speed_ramp = clipOf(apart, s).speed_ramp!.slice(0, 2);
		const moved = structuredClone(apart);
		clipOf(moved, s).timeline_start += 0.5;
		expect(firstSyncBreak(moved, apart)).toBeNull();
	});
});
