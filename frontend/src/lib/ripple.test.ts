import { describe, expect, test } from 'bun:test';
import { rippleFrom } from './ripple';
import type { Clip, Marker, StreamKind, TextOverlay, Timeline, Track } from './types';
import { clipDuration } from './types';

// `rippleFrom` is the port of kerf-core's `Timeline::ripple_from`, and these are
// the Rust tests (`// ---- ripple` in model.rs) replayed case for case, with the
// same clips and the same numbers. A rule that changes there has to change here
// — that is what keeps the browser harness honest about what the backend does.

let seq = 0;
const nextId = () => `clip-${++seq}`;

/** A clip of `dur` seconds placed at `start`, cut from `[0, dur)` of nothing in particular. */
function rclip(start: number, dur: number): Clip {
	return clipOf(start, 0, dur);
}

/** A clip placed at `start`, cut from `[sourceIn, sourceOut)`. */
function clipOf(start: number, sourceIn: number, sourceOut: number): Clip {
	return {
		id: nextId(),
		asset_id: 'asset',
		source_in: sourceIn,
		source_out: sourceOut,
		timeline_start: start,
		volume: 1,
		fade_in: 0,
		fade_out: 0
	};
}

function track(kind: StreamKind, name: string, clips: Clip[]): Track {
	return { id: `track-${name}`, kind, name, clips };
}

/** One video track `V1` holding `clips`. */
function oneLane(clips: Clip[]): Timeline {
	return { tracks: [track('video', 'V1', clips)] };
}

/** Each clip of track `ti` as `[start, end]`, in lane order. */
function spansOf(timeline: Timeline, ti: number): [number, number][] {
	return timeline.tracks[ti].clips.map((c) => [c.timeline_start, c.timeline_start + clipDuration(c)]);
}

/** Run `edit` on a copy of `before` and ripple the result: `[what the edit made, what ripple made of it]`. */
function rippled(before: Timeline, edit: (t: Timeline) => void): [Timeline, Timeline] {
	const after = structuredClone(before);
	edit(after);
	return [after, rippleFrom(after, before)];
}

/** `a [0,4)  b [5,8)  c [10,12)` — a gap of 1 before `b` and 2 before `c`. */
function gapped(): [Timeline, [string, string, string]] {
	const [a, b, c] = [rclip(0, 4), rclip(5, 3), rclip(10, 2)];
	return [oneLane([a, b, c]), [a.id, b.id, c.id]];
}

function clipMut(t: Timeline, id: string): Clip {
	for (const tr of t.tracks) {
		const c = tr.clips.find((k) => k.id === id);
		if (c) return c;
	}
	throw new Error(`no clip ${id}`);
}

describe('rippleFrom — length changes', () => {
	test('a right trim that shortens pulls later clips left and keeps every gap', () => {
		const [before, [a]] = gapped();
		const [, out] = rippled(before, (t) => (clipMut(t, a).source_out = 3));
		expect(spansOf(out, 0)).toEqual([
			[0, 3],
			[4, 7],
			[9, 11]
		]);
	});

	test('a right trim that lengthens pushes later clips right', () => {
		const [before, [a]] = gapped();
		const [after, out] = rippled(before, (t) => (clipMut(t, a).source_out = 6));
		// Un-rippled, the lengthened clip would sit on top of `b`.
		expect(spansOf(after, 0)[1]).toEqual([5, 8]);
		expect(spansOf(out, 0)).toEqual([
			[0, 6],
			[7, 10],
			[12, 14]
		]);
	});

	test('a left trim keeps the clips start whether or not the right edge was held', () => {
		const [before, [, b]] = gapped();
		// The GUI's left-edge trim: later in-point, and a later start so the right
		// edge (8.0) stays put. Ripple keeps the *start* and pulls the rest in.
		const [after, held] = rippled(before, (t) => {
			const c = clipMut(t, b);
			c.source_in = 1;
			c.timeline_start = 6;
		});
		expect(spansOf(after, 0)[1]).toEqual([6, 8]);
		expect(spansOf(held, 0)).toEqual([
			[0, 4],
			[5, 7],
			[9, 11]
		]);

		// The same trim without moving the start gives the same cut.
		const [, unmoved] = rippled(before, (t) => (clipMut(t, b).source_in = 1));
		expect(unmoved).toEqual(held);
	});

	test('a left trim that lengthens also keeps the start', () => {
		// `b` is cut from [2, 5) of its source, so it has a second of handle on the left.
		const [a, b, c] = [rclip(0, 4), clipOf(5, 2, 5), rclip(10, 2)];
		const before = oneLane([a, b, c]);
		const [after, out] = rippled(before, (t) => {
			const k = clipMut(t, b.id);
			k.source_in = 1;
			k.timeline_start = 4; // right edge held at 8.0
		});
		expect(spansOf(after, 0)[1]).toEqual([4, 8]); // un-rippled it would sit on `a`
		expect(spansOf(out, 0)).toEqual([
			[0, 4],
			[5, 9],
			[11, 13]
		]);
	});

	test('a speed change is a length change', () => {
		const [before, [, b]] = gapped();
		const [, out] = rippled(before, (t) => (clipMut(t, b).speed = 2)); // 3.0s -> 1.5s
		expect(spansOf(out, 0)).toEqual([
			[0, 4],
			[5, 6.5],
			[8.5, 10.5]
		]);
	});

	test('two trims in one edit accumulate down the track', () => {
		const [before, [a, b]] = gapped();
		const [, out] = rippled(before, (t) => {
			clipMut(t, a).source_out = 3; // -1s
			clipMut(t, b).source_out = 2; // -1s, and `b` itself follows `a`
		});
		expect(spansOf(out, 0)).toEqual([
			[0, 3],
			[4, 6],
			[8, 10]
		]);
	});
});

describe('rippleFrom — removals', () => {
	test('removing a clip closes its span and keeps the gaps either side', () => {
		const [before, [, b]] = gapped();
		const [, out] = rippled(before, (t) => (t.tracks[0].clips = t.tracks[0].clips.filter((c) => c.id !== b)));
		// 1s of gap before `b` and 2s after it: 3s between `a` and `c`, as there was.
		expect(spansOf(out, 0)).toEqual([
			[0, 4],
			[7, 9]
		]);
	});

	test('removing several clips closes each span once', () => {
		const clips = [rclip(0, 2), rclip(2, 2), rclip(6, 2), rclip(10, 2)];
		const [a, c] = [clips[0].id, clips[2].id];
		const before = oneLane(clips);
		const [, out] = rippled(before, (t) => (t.tracks[0].clips = t.tracks[0].clips.filter((k) => k.id !== a && k.id !== c)));
		expect(spansOf(out, 0)).toEqual([
			[0, 2],
			[6, 8]
		]);
	});
});

describe('rippleFrom — adds', () => {
	test('a clip added onto footage pushes it and everything after right', () => {
		const before = oneLane([rclip(0, 4), rclip(4, 4), rclip(8, 4)]);
		// Dropped at the cut between `a` and `b`: a 2s insert.
		const [after, out] = rippled(before, (t) => t.tracks[0].clips.push(rclip(4, 2)));
		expect(after.tracks[0].clips[1].timeline_start).toBe(4); // un-rippled it overlaps `b`
		expect(spansOf(out, 0)).toEqual([
			[0, 4],
			[4, 6],
			[6, 10],
			[10, 14]
		]); // kept in lane order, nothing overlapping
	});

	test('a clip added over the head of a clip in a gap pushes by its whole length', () => {
		const before = oneLane([rclip(0, 2), rclip(10, 2), rclip(14, 2)]);
		// 5s at 8.0 runs into the head of the clip at 10.0.
		const [, out] = rippled(before, (t) => t.tracks[0].clips.push(rclip(8, 5)));
		expect(spansOf(out, 0)).toEqual([
			[0, 2],
			[8, 13],
			[15, 17],
			[19, 21]
		]);
	});

	test('a clip that fits moves nothing', () => {
		const before = oneLane([rclip(0, 2), rclip(10, 2)]);
		// Into the gap, and onto the end of the track.
		const [after, out] = rippled(before, (t) => {
			t.tracks[0].clips.push(rclip(4, 2));
			t.tracks[0].clips.push(rclip(12, 3));
		});
		expect(out).toEqual(after);
	});

	test('a clip added inside another clip is left as the edit made it', () => {
		// Resolving it would take a split, which ripple never does.
		const before = oneLane([rclip(0, 10), rclip(10, 4)]);
		const [after, out] = rippled(before, (t) => t.tracks[0].clips.push(rclip(4, 2)));
		expect(out).toEqual(after); // no ripple for the track — not half of one
	});

	test('adjacent adds push once by their combined length', () => {
		const before = oneLane([rclip(0, 4), rclip(4, 4)]);
		const [, out] = rippled(before, (t) => {
			t.tracks[0].clips.push(rclip(4, 2));
			t.tracks[0].clips.push(rclip(6, 3));
		});
		expect(spansOf(out, 0)).toEqual([
			[0, 4],
			[4, 6],
			[6, 9],
			[9, 13]
		]);
	});

	test('replacing a clip in place moves nothing', () => {
		const [before, [, b]] = gapped();
		const [after, out] = rippled(before, (t) => {
			t.tracks[0].clips = t.tracks[0].clips.filter((c) => c.id !== b);
			t.tracks[0].clips.splice(1, 0, rclip(5, 3));
		});
		expect(out).toEqual(after); // -3s for the removal and +3s for the add cancel
	});
});

describe('rippleFrom — what it leaves alone', () => {
	test('a split shifts nothing', () => {
		const [before, [a]] = gapped();
		const [after, out] = rippled(before, (t) => {
			// What `Project::split_at` does at 1.5.
			const left = clipMut(t, a);
			const right: Clip = { ...left, id: nextId(), timeline_start: 1.5, source_in: 1.5 };
			left.source_out = 1.5;
			t.tracks[0].clips.splice(1, 0, right);
		});
		expect(after.tracks[0].clips).toHaveLength(4);
		expect(out).toEqual(after);
	});

	test('a move does not ripple within a track or across tracks', () => {
		const [before, [a, b]] = gapped();
		before.tracks.splice(1, 0, track('video', 'V2', [rclip(20, 2)]));

		// Within the track: `a` slides into the gap.
		let [after, out] = rippled(before, (t) => (clipMut(t, a).timeline_start = 0.5));
		expect(out).toEqual(after);

		// Across tracks: `b` goes up to V2. V1 does not close behind it.
		[after, out] = rippled(before, (t) => {
			const ci = t.tracks[0].clips.findIndex((c) => c.id === b);
			const [moved] = t.tracks[0].clips.splice(ci, 1);
			t.tracks[1].clips.unshift(moved);
		});
		expect(out).toEqual(after);
	});

	test('tracks ripple independently', () => {
		const [a, b, c] = [rclip(0, 4), rclip(5, 3), rclip(10, 2)];
		const audio = [rclip(0, 4), rclip(5, 3), rclip(10, 2)];
		const before: Timeline = { tracks: [track('video', 'V1', [a, b, c]), track('audio', 'A1', audio)] };
		const [, out] = rippled(before, (t) => (clipMut(t, a.id).source_out = 3));
		expect(spansOf(out, 0)).toEqual([
			[0, 3],
			[4, 7],
			[9, 11]
		]);
		expect(spansOf(out, 1)).toEqual(spansOf(before, 1)); // no sync lock: the audio stays put
	});

	test('a locked track never moves', () => {
		const [before, [a]] = gapped();
		before.tracks[0].locked = true;
		const [after, out] = rippled(before, (t) => (clipMut(t, a).source_out = 3));
		expect(out).toEqual(after);
	});

	test('an edit that already rippled is not rippled twice', () => {
		// `Project::ripple_delete` of `b`: removed, and `c` closed up by 3.
		const [deleted, [, b]] = gapped();
		const [afterDelete, outDelete] = rippled(deleted, (t) => {
			t.tracks[0].clips = t.tracks[0].clips.filter((c) => c.id !== b);
			t.tracks[0].clips[1].timeline_start -= 3;
		});
		expect(spansOf(afterDelete, 0)).toEqual([
			[0, 4],
			[7, 9]
		]);
		expect(outDelete).toEqual(afterDelete);

		// `Project::cut_clip_range` of the middle second of `a`: the head keeps the
		// id, the tail is a new clip, and everything later is moved left 1s.
		const [cut, [a]] = gapped();
		const [afterCut, outCut] = rippled(cut, (t) => {
			const head = clipMut(t, a);
			const tail: Clip = { ...head, id: nextId(), source_in: 2, timeline_start: 1 };
			head.source_out = 1;
			t.tracks[0].clips.splice(1, 0, tail);
			for (const c of t.tracks[0].clips.slice(2)) c.timeline_start -= 1;
		});
		expect(spansOf(afterCut, 0)).toEqual([
			[0, 1],
			[1, 3],
			[4, 7],
			[9, 11]
		]);
		expect(outCut).toEqual(afterCut);
	});

	test('overlays and markers stay where they were', () => {
		const [before, [a]] = gapped();
		const overlay: TextOverlay = {
			id: 'o1',
			text: 'title',
			start: 6,
			end: 9,
			pos_x: 0.5,
			pos_y: 0.5,
			size: 0.05,
			color: '#ffffff',
			bold: false
		};
		const marker: Marker = { id: 'm1', time: 7, name: 'beat', color: null };
		before.overlays = [overlay];
		before.markers = [marker];
		const [, out] = rippled(before, (t) => (clipMut(t, a).source_out = 3));
		expect(out.overlays?.[0].start).toBe(6);
		expect(out.markers?.[0].time).toBe(7);
		expect(spansOf(out, 0)[1]).toEqual([4, 7]); // while the clips did move
	});
});

describe('rippleFrom — never a broken lane', () => {
	test('a ripple that would overlap is not applied', () => {
		// V1: a [0,4)  r [4,6)  f [6,8).  The edit deletes `r` and drops a clip
		// from V2 into its place. Closing `f` up behind `r` would land it on top.
		const [a, r, f] = [rclip(0, 4), rclip(4, 2), rclip(6, 2)];
		const m = rclip(0, 2);
		const before: Timeline = { tracks: [track('video', 'V1', [a, r, f]), track('video', 'V2', [m])] };
		const [after, out] = rippled(before, (t) => {
			t.tracks[0].clips = t.tracks[0].clips.filter((c) => c.id !== r.id);
			const [moved] = t.tracks[1].clips.splice(0, 1);
			expect(moved.id).toBe(m.id);
			moved.timeline_start = 4;
			t.tracks[0].clips.splice(1, 0, moved);
		});
		expect(out).toEqual(after); // left exactly as the edit made it
		expect(spansOf(out, 0)).toEqual([
			[0, 4],
			[4, 6],
			[6, 8]
		]);
	});

	test('a ripple that would start a clip before zero is not applied', () => {
		// An old project with overlapping clips: deleting both `r1` and `r2` would
		// close `f` up by 8s, which is more than there is room for.
		const [r1, r2, f] = [rclip(0, 4), rclip(1, 4), rclip(5, 2)];
		const before = oneLane([r1, r2, f]);
		const [after, out] = rippled(before, (t) => (t.tracks[0].clips = t.tracks[0].clips.filter((c) => c.id !== r1.id && c.id !== r2.id)));
		expect(out).toEqual(after);
		expect(spansOf(out, 0)).toEqual([[5, 7]]);
	});

	test('an old overlap elsewhere in the track blocks nothing', () => {
		// `a` and `a2` already overlap and nothing touches them.
		const [a, a2, b, c] = [rclip(0, 4), rclip(2, 4), rclip(10, 2), rclip(14, 2)];
		const before = oneLane([a, a2, b, c]);
		const [, out] = rippled(before, (t) => (t.tracks[0].clips = t.tracks[0].clips.filter((k) => k.id !== b.id)));
		expect(spansOf(out, 0)).toEqual([
			[0, 4],
			[2, 6],
			[12, 14]
		]);
	});
});

describe('rippleFrom — identities', () => {
	test('an edit that changes no timing comes back untouched', () => {
		const [before, [a]] = gapped();
		const [after, out] = rippled(before, (t) => {
			clipMut(t, a).volume = 0.4;
			t.tracks[0].muted = true;
		});
		expect(out).toEqual(after);
		expect(rippleFrom(before, before)).toEqual(before);
	});

	test('rippling twice is rippling once', () => {
		const [before, [, b]] = gapped();
		const [after, once] = rippled(before, (t) => (clipMut(t, b).source_out = 1));
		const twice = rippleFrom(once, before);
		expect(twice).toEqual(once);
		expect(once).not.toEqual(after);
	});

	test('it never modifies what it was given', () => {
		const [before, [a]] = gapped();
		const [after] = rippled(before, (t) => (clipMut(t, a).source_out = 3));
		const frozenAfter = structuredClone(after);
		const frozenBefore = structuredClone(before);
		const out = rippleFrom(after, before);
		expect(after).toEqual(frozenAfter);
		expect(before).toEqual(frozenBefore);
		expect(out).not.toBe(after);
	});
});
