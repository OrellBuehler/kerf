// Ripple editing — the faithful TS mirror of kerf-core's `Timeline::ripple_from`
// (crates/kerf-core/src/model.rs). Pure, and used only by the browser harness in
// `api.ts`: the desktop app asks the backend, which applies the very same rules
// after every edit while ripple mode is on. It is a *port*, not a lookalike —
// the bun tests replay the Rust tests case for case, so a rule changed there
// has to change here or a test names it.
//
// The one idea: an edit that changes how much footage sits ahead of a clip
// carries that clip along, with every gap in front of it kept. Per track,
// matched by clip **id**, the track's *followers* — the clips the edit left
// starting where they started — are shifted by the net change in length of what
// the edit did ahead of them:
//
//   the edit…                                         followers at or after…  move by
//   changes a clip's length (right trim, speed, …)    its old end             the change in its length
//   removes a clip from the timeline                  its old end             minus its length
//   adds a clip onto footage that was there           the new clip's start    the new clip's length
//
// Deliberately left alone: a left-edge trim keeps the clip's *start* (the GUI
// commits it as a later `source_in` plus a later `timeline_start`; ripple keeps
// the start and follows the length, so both forms give one result — only when
// the trim is the whole edit on the track); an add that fits in free space moves
// nothing; a split shifts nothing; moves never ripple; a clip the edit itself
// moved is not a follower (so an op that already closed the gap is not shifted
// twice); tracks are independent (no sync lock); a locked track never moves;
// overlays and markers do not move. And it never produces an overlap: a track
// whose ripple would leave a touched clip overlapping another, or before 0, is
// returned exactly as the edit made it.

import type { Clip, Timeline, Track } from './types';
import { clipDuration } from './types';

/** kerf-core's `DIFF_EPS`: timing closer than this is float noise from a JSON round-trip. */
export const DIFF_EPS = 1e-6;

const numChanged = (a: number, b: number): boolean => Math.abs(a - b) > DIFF_EPS;
const clipEnd = (c: Clip): number => c.timeline_start + clipDuration(c);

/** Rust's `f64::total_cmp` for the finite numbers a timeline holds: it orders -0 before +0. */
function totalCmp(a: number, b: number): number {
	if (a < b) return -1;
	if (a > b) return 1;
	const negA = Object.is(a, -0);
	const negB = Object.is(b, -0);
	return negA === negB ? 0 : negA ? -1 : 1;
}

/** Do two spans share any time? Touching end-to-start is not an overlap, and neither is a microsecond of noise. */
export function spansOverlap(a: [number, number], b: [number, number]): boolean {
	return a[0] < b[1] - DIFF_EPS && b[0] < a[1] - DIFF_EPS;
}

/**
 * The cut after a **ripple edit**: `after` is what an edit left behind and
 * `before` is what it started from. Pure — neither argument is modified — and
 * the identity (a copy) when nothing about any track's timing changed.
 */
export function rippleFrom(after: Timeline, before: Timeline): Timeline {
	const inBefore = new Set(before.tracks.flatMap((t) => t.clips.map((c) => c.id)));
	const inAfter = new Set(after.tracks.flatMap((t) => t.clips.map((c) => c.id)));
	const out: Timeline = structuredClone(after);
	out.tracks = out.tracks.map((track) => {
		const prior = before.tracks.find((t) => t.id === track.id);
		if (!prior) return track;
		if (track.locked || prior.locked) return track;
		return rippleTrack(track, prior, inBefore, inAfter) ?? track;
	});
	return out;
}

/** The rippled version of one track, or `null` when there is nothing to do or the ripple would leave the lane illegal. */
function rippleTrack(after: Track, before: Track, inBefore: Set<string>, inAfter: Set<string>): Track | null {
	const prior = new Map(before.clips.map((c) => [c.id, c]));
	const here = new Set(after.clips.map((c) => c.id));

	// Clips the edit left starting where they started follow whatever the edit did
	// ahead of them, whether or not their own length changed too.
	// `[index in after, where it stood, whether it is untouched]`.
	const anchored: [number, number, boolean][] = [];
	const resized: [number, Clip][] = []; // same id, new length (and the old clip)
	const added: number[] = [];
	let otherEdits = 0; // moves, arrivals, departures

	after.clips.forEach((clip, i) => {
		const was = prior.get(clip.id);
		if (was) {
			const sameStart = !numChanged(was.timeline_start, clip.timeline_start);
			const sameLength = !numChanged(clipDuration(was), clipDuration(clip));
			if (sameStart) anchored.push([i, was.timeline_start, sameLength]);
			if (!sameLength) resized.push([i, was]);
			else if (!sameStart) otherEdits += 1;
		} else if (!inBefore.has(clip.id)) {
			added.push(i);
		} else {
			otherEdits += 1;
		}
	});
	const removed = before.clips.filter((c) => !inAfter.has(c.id));
	otherEdits += before.clips.filter((c) => inAfter.has(c.id) && !here.has(c.id)).length;

	// What the edit did to the amount of footage ahead of the clips after it:
	// `[where, by how much]`, in before-timeline terms.
	const events: [number, number][] = [];
	for (const [i, was] of resized) events.push([clipEnd(was), clipDuration(after.clips[i]) - clipDuration(was)]);
	for (const clip of removed) events.push([clipEnd(clip), -clipDuration(clip)]);
	// Adjacent adds are one insertion; it only counts when it landed on footage
	// that was there (a clip that fits in free space moves nothing).
	const spans = added
		.map((i): [number, number] => [after.clips[i].timeline_start, clipEnd(after.clips[i])])
		.sort((a, b) => totalCmp(a[0], b[0]));
	const chains: [number, number][] = [];
	for (const [start, end] of spans) {
		const last = chains[chains.length - 1];
		if (last && start <= last[1] + DIFF_EPS) last[1] = Math.max(last[1], end);
		else chains.push([start, end]);
	}
	for (const [lo, hi] of chains) {
		if (before.clips.some((c) => spansOverlap([lo, hi], [c.timeline_start, clipEnd(c)]))) events.push([lo, hi - lo]);
	}

	const clips: Clip[] = after.clips.map((c) => ({ ...c }));
	// Untouched clips are pristine until shifted; everything else the edit touched is not.
	const pristine: boolean[] = clips.map(() => false);
	let changed = false;

	// A left-edge trim that held the right edge still: the clip keeps its start.
	// Only when it is the whole edit.
	if (resized.length === 1) {
		const [i, was] = resized[0];
		const sole = removed.length === 0 && added.length === 0 && otherEdits === 0;
		if (sole && !numChanged(clipEnd(was), clipEnd(clips[i]))) {
			clips[i].timeline_start = was.timeline_start;
			changed = true;
		}
	}

	for (const [i, stood, untouched] of anchored) {
		pristine[i] = untouched;
		let shift = 0;
		for (const [at, by] of events) if (at <= stood + DIFF_EPS) shift += by;
		if (Math.abs(shift) > DIFF_EPS) {
			clips[i].timeline_start += shift;
			pristine[i] = false;
			changed = true;
		}
	}
	if (!changed || !laneIsLegal(clips, pristine)) return null;
	clips.sort((a, b) => totalCmp(a.timeline_start, b.timeline_start));
	return { ...after, clips };
}

/**
 * Whether a lane is fit to keep after a ripple: nothing the edit or the ripple
 * touched starts before 0 or overlaps another clip. Two `pristine` clips
 * overlapping is old news and does not count.
 */
function laneIsLegal(clips: Clip[], pristine: boolean[]): boolean {
	if (clips.some((c, i) => !pristine[i] && c.timeline_start < -DIFF_EPS)) return false;
	const order = clips.map((_, i) => i).sort((a, b) => totalCmp(clips[a].timeline_start, clips[b].timeline_start));
	for (let pos = 0; pos < order.length; pos++) {
		const i = order[pos];
		const end = clipEnd(clips[i]);
		for (const j of order.slice(pos + 1)) {
			if (clips[j].timeline_start >= end - DIFF_EPS) break;
			if (!pristine[i] || !pristine[j]) return false;
		}
	}
	return true;
}
