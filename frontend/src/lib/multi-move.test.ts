import { describe, expect, test } from 'bun:test';
import { moveClips } from './multi-edit';
import { moveTracks, planMove, type MoveTrack } from './multi-move';
import type { Clip, StreamKind, Timeline, Track } from './types';
import { clipDuration } from './types';

/** A clip `dur` seconds long at `start` (speed 1, so its length is source_out - source_in). */
function clip(id: string, start: number, dur: number): Clip {
	return { id, asset_id: 'a', source_in: 0, source_out: dur, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0 };
}
function track(id: string, kind: StreamKind, clips: Clip[], locked = false): Track {
	return { id, kind, name: id.toUpperCase(), clips, locked };
}

// V1: c1 0-10, c2 12-20, c3 30-40     V2: c4 5-15
// A1: c5 0-60                          A2: (empty)
function sample(): Timeline {
	return {
		tracks: [
			track('v1', 'video', [clip('c1', 0, 10), clip('c2', 12, 8), clip('c3', 30, 10)]),
			track('v2', 'video', [clip('c4', 5, 10)]),
			track('a1', 'audio', [clip('c5', 0, 60)]),
			track('a2', 'audio', [])
		]
	};
}
const tracksOf = (tl: Timeline = sample()): MoveTrack[] => moveTracks(tl);

describe('moveTracks', () => {
	test('reads the lanes with their clips worked out to seconds', () => {
		const t = moveTracks({
			tracks: [track('v1', 'video', [{ ...clip('c1', 2, 10), speed: 2 }], true)]
		});
		expect(t).toEqual([{ id: 'v1', name: 'V1', kind: 'video', locked: true, clips: [{ id: 'c1', start: 2, dur: 5 }] }]);
	});
});

describe('planMove — one clip', () => {
	test('a slide in time on the same track', () => {
		const p = planMove(tracksOf(), ['c3'], 'c3', 33, 'v1');
		expect(p.ok).toBe(true);
		expect(p.moves).toEqual([{ clip_id: 'c3', timeline_start: 33 }]);
		expect(p.delta).toBe(3);
		expect(p.laneShift).toBe(0);
		expect(p.ghosts).toEqual([{ clipId: 'c3', trackId: 'v1', start: 33, dur: 10 }]);
	});

	test('to another track of its kind names that track', () => {
		const p = planMove(tracksOf(), ['c3'], 'c3', 30, 'v2');
		expect(p.ok).toBe(true);
		expect(p.moves).toEqual([{ clip_id: 'c3', timeline_start: 30, track_id: 'v2' }]);
		expect(p.laneShift).toBe(1);
	});

	test('dropped where it already is changes nothing', () => {
		const p = planMove(tracksOf(), ['c3'], 'c3', 30, 'v1');
		expect(p.ok).toBe(true);
		expect(p.noop).toBe(true);
		expect(p.moves).toEqual([]);
	});

	test('onto a clip that stays is refused, with the backend\'s words', () => {
		const p = planMove(tracksOf(), ['c3'], 'c3', 15, 'v1');
		expect(p.ok).toBe(false);
		expect(p.reason).toBe('The clips would overlap on track V1 at 0:15.0');
		expect(p.moves).toEqual([]);
		expect(p.ghosts).toEqual([{ clipId: 'c3', trackId: 'v1', start: 15, dur: 10 }]); // still drawn, red
	});

	test('butting against a neighbour is fine, however a float lands', () => {
		expect(planMove(tracksOf(), ['c3'], 'c3', 20, 'v1').ok).toBe(true);
		expect(planMove(tracksOf(), ['c3'], 'c3', 20 - 1e-9, 'v1').ok).toBe(true); // inside the backend's tolerance
		expect(planMove(tracksOf(), ['c3'], 'c3', 19.99, 'v1').ok).toBe(false);
	});

	test('before 0 is refused, not clamped', () => {
		const p = planMove(tracksOf(), ['c2'], 'c2', -3, 'v1');
		expect(p.ok).toBe(false);
		expect(p.reason).toContain('before the beginning');
	});

	test('a hair under 0 is float noise and lands on 0', () => {
		const p = planMove(tracksOf(), ['c5'], 'c5', -1e-9, 'a1');
		expect(p.ok).toBe(true);
		expect(p.moves).toEqual([]);
		expect(p.noop).toBe(true);
	});

	test('to a track of the other kind is refused', () => {
		const p = planMove(tracksOf(), ['c3'], 'c3', 30, 'a1');
		expect(p.ok).toBe(false);
		expect(p.reason).toContain('video');
	});

	test('a locked destination or source is refused', () => {
		const tl = sample();
		tl.tracks[1].locked = true;
		expect(planMove(tracksOf(tl), ['c3'], 'c3', 50, 'v2').reason).toBe('Track V2 is locked');
		expect(planMove(tracksOf(tl), ['c4'], 'c4', 50, 'v2').reason).toBe('Track V2 is locked');
	});

	test('a clip or track that is not there is refused', () => {
		expect(planMove(tracksOf(), ['nope'], 'nope', 0, 'v1').ok).toBe(false);
		expect(planMove(tracksOf(), ['c1'], 'c1', 0, 'nope').ok).toBe(false);
	});
});

describe('planMove — a group', () => {
	test('every clip moves by the grabbed clip\'s Δt', () => {
		// Grab c2 (12 → 14): everything moves 2 s, including a clip on another track.
		const p = planMove(tracksOf(), ['c2', 'c3', 'c5'], 'c2', 14, 'v1');
		expect(p.ok).toBe(true);
		expect(p.delta).toBe(2);
		expect(p.moves).toEqual([
			{ clip_id: 'c5', timeline_start: 2 },
			{ clip_id: 'c2', timeline_start: 14 },
			{ clip_id: 'c3', timeline_start: 32 }
		]);
	});

	test('moves are in time order and name only the clips whose track changes', () => {
		const p = planMove(tracksOf(), ['c1', 'c4'], 'c1', 0, 'v2');
		// c1 → V2 (lane +1) would need V3 for c4: refused. A one-kind group instead:
		expect(p.ok).toBe(false);
		const q = planMove(tracksOf(), ['c1', 'c2'], 'c1', 0, 'v2');
		expect(q.ok).toBe(false); // c1 0-10 against c4 5-15 on V2
		const r = planMove(tracksOf(), ['c1', 'c2'], 'c1', 20, 'v2');
		expect(r.ok).toBe(true);
		expect(r.moves).toEqual([
			{ clip_id: 'c1', timeline_start: 20, track_id: 'v2' },
			{ clip_id: 'c2', timeline_start: 32, track_id: 'v2' }
		]);
	});

	test('a group may pass through the places it is leaving', () => {
		// c1 0-10 and c2 12-20: nudging both right by 2 puts c1 on c2's old spot.
		const p = planMove(tracksOf(), ['c1', 'c2'], 'c1', 2, 'v1');
		expect(p.ok).toBe(true);
	});

	test('but not onto a clip that stays', () => {
		const p = planMove(tracksOf(), ['c1', 'c2'], 'c1', 10, 'v1'); // c2 would land at 22-30... c3 stays at 30
		expect(p.ok).toBe(true);
		const q = planMove(tracksOf(), ['c1', 'c2'], 'c1', 12, 'v1'); // c2 lands on 24-32 over c3 (30-40)
		expect(q.ok).toBe(false);
		expect(q.reason).toContain('overlap');
	});

	test('the grabbed clip may be fine while another of the group is not', () => {
		// Pull the group 5 s left: c2 (12 → 7) is fine, but c1 (0 → -5) is not.
		const p = planMove(tracksOf(), ['c1', 'c2'], 'c2', 7, 'v1');
		expect(p.ok).toBe(false);
		expect(p.reason).toContain('before the beginning');
		// The whole group is drawn so the red ghost shows where it would have gone.
		expect(p.ghosts.map((g) => g.clipId).sort()).toEqual(['c1', 'c2']);
		expect(p.ghosts.every((g) => g.start >= 0)).toBe(true);
	});

	test('one clip of the group on a locked track refuses the lot', () => {
		const tl = sample();
		tl.tracks[2].locked = true; // A1
		const p = planMove(tracksOf(tl), ['c1', 'c5'], 'c1', 1, 'v1');
		expect(p.ok).toBe(false);
		expect(p.reason).toBe('Track A1 is locked');
	});

	test('a selection that names a clip that is gone moves the rest', () => {
		const p = planMove(tracksOf(), ['c3', 'gone'], 'c3', 33, 'v1');
		expect(p.ok).toBe(true);
		expect(p.moves).toEqual([{ clip_id: 'c3', timeline_start: 33 }]);
	});

	test('the grabbed clip is part of the group even if the caller left it out', () => {
		const p = planMove(tracksOf(), ['c2'], 'c3', 33, 'v1');
		expect(p.moves.map((m) => m.clip_id)).toEqual(['c2', 'c3']);
	});
});

describe('planMove — lanes', () => {
	test('every clip shifts by the same lane offset within its own kind', () => {
		// V1 → V2 is +1 for video; the audio clip goes A1 → A2.
		const tl = sample();
		tl.tracks[1].clips = []; // clear V2 so the video clip fits
		const p = planMove(tracksOf(tl), ['c3', 'c5'], 'c3', 30, 'v2');
		expect(p.ok).toBe(true);
		expect(p.laneShift).toBe(1);
		expect(p.moves).toEqual([
			{ clip_id: 'c5', timeline_start: 0, track_id: 'a2' },
			{ clip_id: 'c3', timeline_start: 30, track_id: 'v2' }
		]);
	});

	test('a lane that is not there refuses the drop', () => {
		// c5 is on A2 (the last audio lane); +1 has nowhere to go.
		const tl = sample();
		tl.tracks[3].clips = [clip('c6', 0, 5)];
		tl.tracks[1].clips = [];
		const p = planMove(tracksOf(tl), ['c3', 'c6'], 'c3', 30, 'v2');
		expect(p.ok).toBe(false);
		expect(p.reason).toBe('There is no audio track below A2 for its clip');
		expect(p.ghosts).toHaveLength(2);
	});

	test('going up through the lanes shifts negatively', () => {
		const p = planMove(tracksOf(), ['c4'], 'c4', 5, 'v1');
		expect(p.laneShift).toBe(-1);
		expect(p.ok).toBe(false); // c4 5-15 against c1 0-10
		// ...and a lane that is missing above is said to be above.
		const top = planMove(tracksOf(), ['c1', 'c5'], 'c4', 5, 'v1');
		expect(top.ok).toBe(false);
		expect(top.reason).toBe('There is no video track above V1 for its clip');
		const q = planMove(tracksOf(), ['c4'], 'c4', 22, 'v1');
		expect(q.laneShift).toBe(-1);
		expect(q.ok).toBe(false); // against c2 12-20? 22-32 vs c3 30-40 overlaps
		const r = planMove(tracksOf(), ['c4'], 'c4', 20, 'v1');
		expect(r.ok).toBe(true);
		expect(r.moves).toEqual([{ clip_id: 'c4', timeline_start: 20, track_id: 'v1' }]);
	});

	test('lanes of a kind are its tracks in order, not the whole stack', () => {
		// With A1 between V1 and V2 in the track list, V1 → V2 is still one lane.
		const tl: Timeline = {
			tracks: [track('v1', 'video', [clip('x', 0, 4)]), track('a1', 'audio', []), track('v2', 'video', [])]
		};
		const p = planMove(moveTracks(tl), ['x'], 'x', 0, 'v2');
		expect(p.laneShift).toBe(1);
		expect(p.ok).toBe(true);
	});

	test('two clips of a kind keep their lanes apart when the group moves down together', () => {
		const tl: Timeline = {
			tracks: [
				track('v1', 'video', [clip('p', 0, 4)]),
				track('v2', 'video', [clip('q', 0, 4)]),
				track('v3', 'video', [])
			]
		};
		const p = planMove(moveTracks(tl), ['p', 'q'], 'p', 0, 'v2');
		expect(p.ok).toBe(true);
		expect(p.moves).toEqual([
			{ clip_id: 'p', timeline_start: 0, track_id: 'v2' },
			{ clip_id: 'q', timeline_start: 0, track_id: 'v3' }
		]);
	});
});

// The point of checking here is to agree with the backend, so check that: every plan
// agrees with `Timeline::move_clips` (via its faithful mirror), on a spread of
// drags over a few layouts — accepted exactly when the backend accepts, and, when
// it does, landing every clip exactly where the ghosts were drawn.
describe('planMove agrees with move_clips', () => {
	// A small deterministic generator, so a failure names a case that can be replayed.
	let state = 12345;
	const rand = () => {
		state = (state * 1103515245 + 12345) & 0x7fffffff;
		return state / 0x7fffffff;
	};

	function layout(): Timeline {
		return {
			tracks: [
				track('v1', 'video', [clip('a', 0, 4), clip('b', 5, 3), clip('c', 12, 6)]),
				track('v2', 'video', [clip('d', 2, 5), clip('e', 20, 2)]),
				track('v3', 'video', []),
				track('a1', 'audio', [clip('f', 1, 9), clip('g', 14, 4)]),
				track('a2', 'audio', [clip('h', 0, 3)])
			]
		};
	}
	const all = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'];

	test('on a few hundred random drags', () => {
		let accepted = 0;
		let refused = 0;
		for (let n = 0; n < 600; n++) {
			const tl = layout();
			if (rand() < 0.15) tl.tracks[Math.floor(rand() * tl.tracks.length)].locked = true;
			const members = all.filter(() => rand() < 0.4);
			const grabbed = all[Math.floor(rand() * all.length)];
			const kinds = new Map(tl.tracks.flatMap((t) => t.clips.map((c) => [c.id, t.kind] as const)));
			const sameKind = tl.tracks.filter((t) => t.kind === kinds.get(grabbed));
			const dest = sameKind[Math.floor(rand() * sameKind.length)].id;
			const start = Math.round((rand() * 30 - 5) * 4) / 4; // quarter seconds, some negative

			const plan = planMove(moveTracks(tl), members, grabbed, start, dest);
			const trial = structuredClone(tl);
			let threw = false;
			try {
				if (plan.ok && plan.moves.length > 0) moveClips(trial, plan.moves);
				else if (!plan.ok) {
					// What the backend would say to the same group, were it asked.
					const group = new Set([...members, grabbed]);
					const delta = start - tl.tracks.flatMap((t) => t.clips).find((c) => c.id === grabbed)!.timeline_start;
					const lanes = (k: StreamKind) => tl.tracks.filter((t) => t.kind === k);
					const shift =
						lanes(kinds.get(grabbed)!).findIndex((t) => t.id === dest) -
						lanes(kinds.get(grabbed)!).findIndex((t) => t.clips.some((c) => c.id === grabbed));
					const asked = tl.tracks.flatMap((t, ti) =>
						t.clips
							.filter((c) => group.has(c.id))
							.map((c) => {
								const row = lanes(t.kind);
								const to = row[row.findIndex((x) => x.id === t.id) + shift];
								return { clip_id: c.id, timeline_start: c.timeline_start + delta, track_id: to?.id ?? '__missing__', ti };
							})
					);
					moveClips(trial, asked.map(({ ti: _ti, ...m }) => m));
				}
			} catch {
				threw = true;
			}
			expect(threw).toBe(!plan.ok);
			if (plan.ok) {
				accepted++;
				// Every ghost is where the clip really is afterwards.
				const after = new Map(trial.tracks.flatMap((t) => t.clips.map((c) => [c.id, { t: t.id, s: c.timeline_start, d: clipDuration(c) }] as const)));
				for (const g of plan.ghosts) {
					const at = after.get(g.clipId)!;
					expect(at.t).toBe(g.trackId);
					expect(at.s).toBeCloseTo(g.start, 9);
					expect(at.d).toBeCloseTo(g.dur, 9);
				}
			} else {
				refused++;
			}
		}
		// The generator has to have exercised both outcomes, or this proves nothing.
		expect(accepted).toBeGreaterThan(50);
		expect(refused).toBeGreaterThan(50);
	});
});
