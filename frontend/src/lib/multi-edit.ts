// Multi-clip moves and removals — the faithful TS mirror of kerf-core's
// `Timeline::move_clips` / `Timeline::remove_clips` (crates/kerf-core/src/model.rs),
// for the browser harness in `api.ts`. Both are all-or-nothing: every check runs
// before anything is touched, so a thrown error leaves the timeline exactly as it
// was. The messages are the backend's own (`invalid argument: …`), because that
// is what the desktop app's rejected promise carries.
//
// Neither ripples: that is `ripple.ts`, applied by the caller after the edit.

import { formatTime } from './diff';
import { DIFF_EPS, spansOverlap } from './ripple';
import type { Clip, ClipMove, Timeline } from './types';
import { clipDuration } from './types';

const invalid = (why: string) => new Error(`invalid argument: ${why}`);

/** `[track index, clip index]` of a clip, or `null`. */
function locateIndex(timeline: Timeline, clipId: string): [number, number] | null {
	for (let ti = 0; ti < timeline.tracks.length; ti++) {
		const ci = timeline.tracks[ti].clips.findIndex((c) => c.id === clipId);
		if (ci >= 0) return [ti, ci];
	}
	return null;
}

/**
 * Move several clips at once, **all or nothing**. The group is checked as a
 * group: clips moving together never collide with the places they are leaving
 * (nudging abutting clips by a second is legal, where moving them one at a time
 * would trip over each other), but they must not overlap each other or any clip
 * that is *not* moving, on whichever track they land. A clip may change track
 * only to another of the same kind, a locked track — as source or destination —
 * refuses the whole move, and a clip may appear once. A start before 0 is an
 * error rather than a clamp. Mutates `timeline`; returns the moved clips in
 * request order.
 */
export function moveClips(timeline: Timeline, moves: ClipMove[]): Clip[] {
	if (moves.length === 0) throw invalid('no clips to move');
	// Resolved up front: the clip, its destination track, start and duration.
	const plan: { id: string; to: number; start: number; duration: number }[] = [];
	const moving = new Set<string>();
	for (const m of moves) {
		if (moving.has(m.clip_id)) throw invalid(`clip ${m.clip_id} appears more than once`);
		moving.add(m.clip_id);
		if (!Number.isFinite(m.timeline_start)) throw invalid('timeline_start must be a finite number');
		if (m.timeline_start < -DIFF_EPS)
			throw invalid(`clip ${m.clip_id} would start before the beginning of the timeline`);
		const found = locateIndex(timeline, m.clip_id);
		if (!found) throw new Error(`clip not found: ${m.clip_id}`);
		const [from, ci] = found;
		const to = m.track_id == null ? from : timeline.tracks.findIndex((t) => t.id === m.track_id);
		if (to < 0) throw new Error(`track not found: ${m.track_id}`);
		if (timeline.tracks[to].kind !== timeline.tracks[from].kind)
			throw invalid('cannot move a clip to a track of a different kind');
		for (const ti of [from, to]) {
			if (timeline.tracks[ti].locked) throw invalid(`track ${timeline.tracks[ti].name} is locked`);
		}
		plan.push({
			id: m.clip_id,
			to,
			start: Math.max(m.timeline_start, 0),
			duration: clipDuration(timeline.tracks[from].clips[ci])
		});
	}

	// Where everything lands: the moved clips against the ones staying put, and against each other.
	plan.forEach((p, i) => {
		const span: [number, number] = [p.start, p.start + p.duration];
		const track = timeline.tracks[p.to];
		const hitsStaying = track.clips.some(
			(c) => !moving.has(c.id) && spansOverlap(span, [c.timeline_start, c.timeline_start + clipDuration(c)])
		);
		const hitsMoving = plan.some(
			(o, j) => j !== i && o.to === p.to && spansOverlap(span, [o.start, o.start + o.duration])
		);
		if (hitsStaying || hitsMoving)
			throw invalid(`moved clips would overlap on track ${track.name} at ${formatTime(p.start)}`);
	});

	// Everything checks out: lift the clips, set them down, re-order the lanes.
	const lifted = new Map<string, Clip>();
	for (const track of timeline.tracks) {
		for (const c of track.clips) if (moving.has(c.id)) lifted.set(c.id, c);
		track.clips = track.clips.filter((c) => !moving.has(c.id));
	}
	const landed: Clip[] = [];
	for (const p of plan) {
		const clip = lifted.get(p.id)!;
		clip.timeline_start = p.start;
		landed.push(structuredClone(clip));
		timeline.tracks[p.to].clips.push(clip);
	}
	for (const p of plan) timeline.tracks[p.to].clips.sort((a, b) => a.timeline_start - b.timeline_start);
	return landed;
}

/**
 * Remove several clips at once, all or nothing: an unknown id, or a clip on a
 * locked track, refuses the lot. A clip named twice is removed once. Leaves gaps
 * — under ripple mode the caller's `rippleFrom` closes them, per track. Mutates
 * `timeline`; returns how many clips were removed.
 */
export function removeClips(timeline: Timeline, ids: string[]): number {
	if (ids.length === 0) throw invalid('no clips to remove');
	for (const id of ids) {
		const found = locateIndex(timeline, id);
		if (!found) throw new Error(`clip not found: ${id}`);
		if (timeline.tracks[found[0]].locked) throw invalid(`track ${timeline.tracks[found[0]].name} is locked`);
	}
	const doomed = new Set(ids);
	for (const track of timeline.tracks) track.clips = track.clips.filter((c) => !doomed.has(c.id));
	return doomed.size;
}
