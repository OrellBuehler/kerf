import { describe, expect, test } from 'bun:test';
import { runEdit } from './link-ops';
import { MUSIC_TRACK, musicBed, musicStart, putMusic, rehearseMusic, type IsComposed, type MusicPlacement } from './music-place';
import type { Clip, Timeline, Track } from './types';
import { clipDuration } from './types';

// The cases are the Rust tests of `project/music_tests.rs` (the whole bed, the lane check, locked tracks),
// replayed against the TypeScript mirror the browser harness places music with.

let n = 0;
const newId = () => `new-${++n}`;

const clip = (id: string, asset: string, start: number, length: number, extra: Partial<Clip> = {}): Clip =>
	({ id, asset_id: asset, source_in: 0, source_out: length, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0, ...extra }) as Clip;
const track = (kind: 'audio' | 'video', name: string, clips: Clip[], extra: Partial<Track> = {}): Track =>
	({ id: `t-${name}`, kind, name, clips, ...extra }) as Track;
const timeline = (...tracks: Track[]): Timeline => ({ tracks, markers: [], overlays: [] }) as unknown as Timeline;

/** Assets `a` and `b` are composed music, `plain` is not. */
const composed: IsComposed = (id) => (id === 'a' || id === 'b' || id === 'c' ? true : id === 'plain' ? false : undefined);

/** A bed Fit to video made of one piece: three segments of asset `a` on M from 3 s, with a gap, then a follower. */
function bedTimeline(): Timeline {
	return timeline(
		track('audio', MUSIC_TRACK, [
			clip('s1', 'a', 3, 10, { fade_in: 1.5, volume: 0.9 }),
			clip('s2', 'a', 13, 6, { volume: 0.7, fade_out: 0.5 }),
			clip('s3', 'a', 19, 4, { fade_out: 2 }),
			clip('next', 'other', 23, 4)
		]),
		track('video', 'V1', [clip('v', 'film', 0, 40)])
	);
}

const mTrack = (tl: Timeline) => tl.tracks.find((t) => t.name === MUSIC_TRACK)!;
const asset = (duration: number, id = 'b') => ({ id, duration });

/** `place_music` in miniature: one edit, rippled when asked, the placed clip lane-checked. */
function place(tl: Timeline, a: { id: string; duration: number }, where: MusicPlacement, clipId: string, ripple: boolean) {
	return runEdit(tl, { ripple, links: true, lanes: [clipId] }, (scratch) => putMusic(scratch, a, where, clipId, composed, newId));
}
const rehearse = (tl: Timeline, where: MusicPlacement, duration: number, ripple: boolean) =>
	rehearseMusic(tl, where, duration, ripple, composed, newId);

describe('the bed a clip belongs to', () => {
	test('is every clip of its asset on its track, in time order, whichever segment is named', () => {
		const tl = bedTimeline();
		for (const id of ['s1', 's2', 's3']) expect(musicBed(tl, id, composed).map((c) => c.id)).toEqual(['s1', 's2', 's3']);
		mTrack(tl).clips.reverse();
		expect(musicBed(tl, 's2', composed).map((c) => c.id)).toEqual(['s1', 's2', 's3']);
	});

	test('is refused, in the core’s order and words, when it cannot be replaced', () => {
		const tl = bedTimeline();
		expect(() => musicBed(tl, 'nope', composed)).toThrow('clip not found: nope');
		expect(() => musicBed(tl, 'v', composed)).toThrow('invalid argument: only music on an audio track can be replaced');
		expect(() => musicBed(tl, 'next', composed)).toThrow('asset not found: other');
		mTrack(tl).clips.push(clip('p', 'plain', 40, 2));
		expect(() => musicBed(tl, 'p', composed)).toThrow(
			'invalid argument: that clip is not composed music: only a clip of a composed asset can be replaced'
		);
		mTrack(tl).clips[1].link_id = 'L';
		expect(() => musicBed(tl, 's1', composed)).toThrow('invalid argument: unlink the music from its picture before composing it again');
		mTrack(tl).locked = true;
		expect(() => musicBed(tl, 's1', composed)).toThrow('invalid argument: M is locked');
	});

	test('a locked track is said before a link', () => {
		const tl = bedTimeline();
		mTrack(tl).clips[0].link_id = 'L';
		mTrack(tl).locked = true;
		expect(() => musicBed(tl, 's1', composed)).toThrow('M is locked');
	});
});

describe('recomposing replaces the whole bed', () => {
	test('with one clip of the new render, at the earliest start, with the named clip’s id and the bed’s ends', () => {
		const done = place(bedTimeline(), asset(18), { replace_clip_id: 's2' }, 's2', false);
		const clips = mTrack(done.timeline).clips;
		expect(clips.map((c) => c.id)).toEqual(['s2', 'next']);
		const [made] = clips;
		expect(made.asset_id).toBe('b');
		expect(made.timeline_start).toBe(3);
		expect(made.source_in).toBe(0);
		expect(made.source_out).toBe(18);
		expect(made.speed).toBe(1);
		expect(clipDuration(made)).toBe(18);
		// The first clip's level and fade-in, the last-ending clip's fade-out — not the named clip's.
		expect(made.volume).toBe(0.9);
		expect(made.fade_in).toBe(1.5);
		expect(made.fade_out).toBe(2);
		expect(done.result.id).toBe('s2');
	});

	test('takes the first clip’s place in the track’s order, keeps the first clip’s effects and keys', () => {
		const tl = bedTimeline();
		const first = mTrack(tl).clips[0];
		first.effects = [{ type: 'blur', sigma: 2 }];
		first.keyframes = [{ time: 0, scale: 1, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1 }];
		mTrack(tl).clips.unshift(clip('lead', 'other', 0, 3));
		const done = place(tl, asset(8), { replace_clip_id: 's3' }, 's3', false);
		expect(mTrack(done.timeline).clips.map((c) => c.id)).toEqual(['lead', 's3', 'next']);
		const made = mTrack(done.timeline).clips[1];
		expect(made.effects).toEqual([{ type: 'blur', sigma: 2 }]);
		expect(made.keyframes).toEqual([{ time: 0, scale: 1, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1 }]);
		first.effects![0] = { type: 'blur', sigma: 9 };
		expect(made.effects).toEqual([{ type: 'blur', sigma: 2 }]);
	});

	test('fades never outlast the new piece', () => {
		const tl = bedTimeline();
		mTrack(tl).clips[0].fade_in = 5;
		mTrack(tl).clips[2].fade_out = 9;
		const made = mTrack(place(tl, asset(4), { replace_clip_id: 's1' }, 's1', false).timeline).clips[0];
		expect(made.fade_in).toBe(4);
		expect(made.fade_out).toBe(4);
	});

	test('plays at 1x with no ramp, whatever the bed’s first piece did', () => {
		const tl = bedTimeline();
		Object.assign(mTrack(tl).clips[0], { speed: 2, speed_ramp: [{ time: 0, speed: 0.5 }, { time: 2, speed: 3 }] });
		const made = mTrack(place(tl, asset(8), { replace_clip_id: 's1' }, 's1', false).timeline).clips[0];
		expect(made.speed).toBe(1);
		expect(made.speed_ramp).toBeUndefined();
		expect(clipDuration(made)).toBe(8);
	});

	test('ignores `at` and `track_id`: it replaces where the bed is', () => {
		const made = mTrack(place(bedTimeline(), asset(8), { replace_clip_id: 's1', at: 99, track_id: 'nowhere' }, 's1', false).timeline).clips[0];
		expect(made.timeline_start).toBe(3);
	});

	test('a clip of another asset on the track stays', () => {
		const tl = bedTimeline();
		const done = place(tl, asset(8), { replace_clip_id: 's1' }, 's1', false);
		expect(mTrack(done.timeline).clips.map((c) => [c.id, c.asset_id])).toEqual([
			['s1', 'b'],
			['next', 'other']
		]);
		// the timeline that was handed over is untouched
		expect(mTrack(tl).clips.length).toBe(4);
	});

	test('ripple on pulls the next clip in behind a shorter piece and pushes it out for a longer one', () => {
		for (const length of [12, 40]) {
			const done = place(bedTimeline(), asset(length), { replace_clip_id: 's1' }, 's1', true);
			const [made, next] = mTrack(done.timeline).clips;
			expect(next.id).toBe('next');
			expect(next.timeline_start).toBeCloseTo(made.timeline_start + length, 6);
		}
	});

	test('ripple off leaves a gap behind a shorter piece, and refuses one that runs into the next clip', () => {
		const shorter = place(bedTimeline(), asset(12), { replace_clip_id: 's1' }, 's1', false);
		expect(mTrack(shorter.timeline).clips[1].timeline_start).toBe(23);
		expect(() => place(bedTimeline(), asset(30), { replace_clip_id: 's1' }, 's1', false)).toThrow(
			/^invalid argument: the clip would overlap another clip on M at .* — move or trim that one first$/
		);
	});
});

describe('adding a piece', () => {
	test('makes the M track on first use, at 0', () => {
		const tl = timeline(track('video', 'V1', [clip('v', 'film', 0, 40)]));
		const done = place(tl, asset(20), {}, 'c1', false);
		expect(done.timeline.tracks.map((t) => [t.kind, t.name])).toEqual([
			['video', 'V1'],
			['audio', 'M']
		]);
		expect(mTrack(done.timeline).clips[0]).toMatchObject({ id: 'c1', asset_id: 'b', timeline_start: 0, source_in: 0, source_out: 20, volume: 1 });
		expect(tl.tracks.length).toBe(1);
	});

	test('goes after what the track holds, or at `at`', () => {
		const tl = timeline(track('audio', MUSIC_TRACK, [clip('x', 'a', 2, 10)]));
		expect(mTrack(place(tl, asset(5), {}, 'c1', false).timeline).clips[1].timeline_start).toBe(12);
		expect(mTrack(place(tl, asset(5), { at: 30 }, 'c1', false).timeline).clips[1].timeline_start).toBe(30);
		expect(mTrack(place(tl, asset(5), { at: -4 }, 'c1', true).timeline).clips.some((c) => c.id === 'c1')).toBe(true);
	});

	test('goes on the track named, which must exist and be an audio track', () => {
		const tl = timeline(track('audio', 'A2', []), track('video', 'V1', []));
		expect(place(tl, asset(5), { track_id: 't-A2' }, 'c1', false).timeline.tracks[0].clips.map((c) => c.id)).toEqual(['c1']);
		expect(() => place(tl, asset(5), { track_id: 'nope' }, 'c1', false)).toThrow('track not found: nope');
		expect(() => place(tl, asset(5), { track_id: 't-V1' }, 'c1', false)).toThrow('invalid argument: music goes on an audio track');
	});

	test('a locked track is refused, M or the one named', () => {
		const m = timeline(track('audio', MUSIC_TRACK, [], { locked: true }));
		expect(() => place(m, asset(5), {}, 'c1', false)).toThrow('invalid argument: M is locked');
		const other = timeline(track('audio', 'A2', [], { locked: true }));
		expect(() => place(other, asset(5), { track_id: 't-A2' }, 'c1', false)).toThrow('invalid argument: A2 is locked');
		expect(() => musicStart(m, undefined, undefined)).toThrow('M is locked');
		expect(() => musicStart(other, 't-A2', 3)).toThrow('A2 is locked');
	});

	test('where it would start: at, else the end of M, else 0', () => {
		const tl = timeline(track('audio', MUSIC_TRACK, [clip('x', 'a', 2, 10)]), track('audio', 'A2', [clip('y', 'a', 0, 99)]));
		expect(musicStart(tl, undefined, undefined)).toBe(12);
		expect(musicStart(tl, undefined, 4)).toBe(4);
		expect(musicStart(tl, undefined, -4)).toBe(0);
		expect(musicStart(tl, 't-A2', undefined)).toBe(99);
		expect(musicStart(timeline(), undefined, undefined)).toBe(0);
	});
});

describe('the lane check', () => {
	/** 2 + 8 bars at 120 BPM: 20 s of music on M. */
	const first = () => timeline(track('audio', MUSIC_TRACK, [clip('first', 'a', 0, 20)]));

	test('a start inside the first piece is refused, ripple or not: it never splits a clip to make room', () => {
		for (const ripple of [false, true]) {
			expect(() => rehearse(first(), { at: 5 }, 10, ripple), `ripple ${ripple}`).toThrow(
				/^invalid argument: the clip would overlap another clip on M at/
			);
			expect(() => place(first(), asset(10), { at: 5 }, 'c1', ripple), `placing, ripple ${ripple}`).toThrow('would overlap another clip on M');
		}
	});

	test('on the first piece’s start it collides with ripple off, and pushes the piece along with it on', () => {
		expect(() => rehearse(first(), { at: 0 }, 10, false)).toThrow('would overlap another clip on M');
		rehearse(first(), { at: 0 }, 10, true);
		const done = place(first(), asset(10), { at: 0 }, 'c1', true);
		const [added, pushed] = mTrack(done.timeline).clips.sort((a, b) => a.timeline_start - b.timeline_start);
		expect(added.id).toBe('c1');
		expect(pushed.id).toBe('first');
		expect(pushed.timeline_start).toBeCloseTo(10, 6);
	});

	test('free space, the end of the track and "after what it holds" all fit', () => {
		for (const at of [20, 23, undefined]) rehearse(first(), { at }, 10, false);
	});

	test('composing at 0 and then at 2 on M is refused, the second time', () => {
		const one = place(timeline(), asset(30), { at: 0 }, 'c1', false);
		expect(() => rehearse(one.timeline, { at: 2 }, 30, false)).toThrow(
			/^invalid argument: the clip would overlap another clip on M at .*— move or trim that one first$/
		);
		expect(() => place(one.timeline, asset(30), { at: 2 }, 'c2', false)).toThrow('would overlap another clip on M');
	});

	test('the rehearsal changes nothing', () => {
		const tl = first();
		const before = structuredClone(tl);
		rehearse(tl, { at: 30 }, 10, true);
		expect(tl).toEqual(before);
		const bare = timeline();
		rehearse(bare, {}, 10, false);
		expect(bare.tracks.length).toBe(0);
	});

	test('a recomposed piece that is longer runs into the next clip: refused up front without ripple, pushed with it', () => {
		const tl = bedTimeline();
		expect(() => rehearse(tl, { replace_clip_id: 's1' }, 30, false)).toThrow('would overlap another clip on M');
		rehearse(tl, { replace_clip_id: 's1' }, 30, true);
		// A shorter one fits either way.
		rehearse(tl, { replace_clip_id: 's1' }, 6, false);
		rehearse(tl, { replace_clip_id: 's1' }, 6, true);
	});

	test('a locked track or a bad bed is refused in the rehearsal too', () => {
		const tl = bedTimeline();
		mTrack(tl).locked = true;
		expect(() => rehearse(tl, {}, 10, false)).toThrow('M is locked');
		expect(() => rehearse(tl, { replace_clip_id: 's1' }, 10, false)).toThrow('M is locked');
		expect(() => rehearse(bedTimeline(), { replace_clip_id: 'nope' }, 10, false)).toThrow('clip not found');
	});
});
