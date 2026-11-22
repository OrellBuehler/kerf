// Range edits — the faithful TS mirror of kerf-core's `model/range_edit.rs` (crates/kerf-core/src/model/range_edit.rs):
// cut every track at one time, lift or extract a range, close a gap. Pure over a timeline, used by the browser
// harness in `api.ts` through `link-ops.ts` (which is `project/range_ops.rs` and `Project::run_edit` around them),
// and replayed by `range-edit.test.ts` (the Rust unit tests, case for case) and by `links-corpus.test.ts` (kerf-core's
// own answers). It is a *port*: a rule changed in Rust has to change here, or a test names it, and the refusals are
// the backend's own sentences because that is what the desktop app's rejected promise carries.
//
// The ops of a timeline that name a clip (`splitClip`, `splitRemove`, `rippleDeleteClip`, …) work on one clip and
// let linked partners follow. These name a *time* (or a range) and the tracks it is read on — "Add Edit to All
// Tracks", Lift and Extract, "Delete Gap".
//
// **Targeted tracks.** There is no track-targeting switch, so the tracks an op works on are the ones it is asked for:
// with none named, every **unlocked** track (a locked track is left exactly as it is and reported); named tracks must
// exist and be unlocked. A clip the range reaches whose linked partner sits on a track that was not named is cut or
// moved with it, and a partner on a locked track refuses the whole edit.
//
// Every cut is made with the pieces the single-clip ops use (`splitClip`, `cutEndAt`, `cutStartAt`), so keyframes,
// speed ramps, fades, transitions and generated titles behave as for a split, a trim to the playhead and a remove.
// Pure and all or nothing: an op works on a copy and replaces the timeline's tracks only when it went through, so a
// throw leaves the timeline as it was. `link-ops.ts` runs each inside one `runEdit` that never ripples (they decide
// their own layout), with the sync lock and guard.

import { cutEndAt, cutStartAt, DIFF_EPS } from './edit-modes';
import { formatTime } from './diff';
import { clipById, invalid, locateIndex, lockedPartner, relinkSides } from './link-groups';
import { splitClip, splitClipLinked } from './links';
import { runsIntoUnlinked, spansOverlap } from './ripple';
import type { Clip, CutAll, ClipSplit, GapClosed, RangeEdit, SkippedTrack, Timeline, Track } from './types';
import { clipDuration } from './types';

const endOf = (c: Clip): number => c.timeline_start + clipDuration(c);

/** `at` is inside `clip`, not on one of its edges. */
function strictlyInside(clip: Clip, at: number): boolean {
	return clip.timeline_start + DIFF_EPS < at && at < endOf(clip) - DIFF_EPS;
}

/** What the range did to one clip, for putting its link group back together. */
type Fate = { kind: 'removed' } | { kind: 'tail' } | { kind: 'head' } | { kind: 'split'; right: string };

const sortByStart = (track: Track) => track.clips.sort((a, b) => a.timeline_start - b.timeline_start);

/** `Timeline::range_lanes`: the lanes a range edit works on, as indices in track order. With `tracks` the named ones,
 *  each of which must exist and be unlocked; else every unlocked track. */
function rangeLanes(tl: Timeline, tracks: readonly string[] | null | undefined): number[] {
	if (tracks == null) {
		const lanes = tl.tracks.map((_, ti) => ti).filter((ti) => !tl.tracks[ti].locked);
		if (lanes.length === 0) throw invalid('there is no unlocked track to edit — unlock one or add a track');
		return lanes;
	}
	if (tracks.length === 0) throw invalid('name at least one track, or leave the list out to use every unlocked track');
	const lanes: number[] = [];
	for (const id of tracks) {
		const ti = tl.tracks.findIndex((t) => t.id === id);
		if (ti < 0) throw new Error(`track not found: ${id}`);
		if (tl.tracks[ti].locked) throw invalid(`track ${tl.tracks[ti].name} is locked`);
		lanes.push(ti);
	}
	return [...new Set(lanes)].sort((a, b) => a - b);
}

/** `Timeline::link_groups`: the link groups of two or more clips, each with the clips in track order. */
function linkGroups(tl: Timeline): [string, string[]][] {
	const byLink = new Map<string, string[]>();
	for (const clip of tl.tracks.flatMap((t) => t.clips)) {
		if (clip.link_id) byLink.set(clip.link_id, [...(byLink.get(clip.link_id) ?? []), clip.id]);
	}
	return [...byLink].filter(([, members]) => members.length >= 2);
}

/** `Timeline::locked_where`: the locked tracks that hold a clip `reaches` says yes to. */
function lockedWhere(tl: Timeline, reaches: (c: Clip) => boolean): SkippedTrack[] {
	return tl.tracks.filter((t) => t.locked && t.clips.some(reaches)).map((t) => ({ track_id: t.id, name: t.name }));
}

// ---- cut every track at one time ---------------------------------------------

/**
 * **Cut every track at `at`** (`Timeline::split_all_tracks`, "Add Edit to All Tracks"): each clip of the targeted
 * tracks that has `at` strictly inside it is split there, as `splitClip` splits one — and with `links` its linked
 * partners with it (`splitClipLinked`: the halves are re-linked by side, a partner on a locked track refuses the lot).
 * A track whose clips all end or start at `at` already has a cut there and is passed over; a time with nothing to cut
 * at all is an error that says why (nothing under it, it is already a cut, the clips are locked), never an empty edit.
 *
 * `tracks` names the tracks to cut (each must exist and be unlocked); `null` / `undefined` is every unlocked track, and
 * the locked ones with a clip under `at` are reported in `skipped`. Shifts nothing, so it is the same with or without
 * ripple mode. Mutates `tl`; all or nothing.
 */
export function splitAllTracks(tl: Timeline, at: number, tracks: readonly string[] | null | undefined, links: boolean): CutAll {
	if (!Number.isFinite(at)) throw invalid('the cut time must be a finite number');
	const lanes = rangeLanes(tl, tracks);
	const under = lanes.flatMap((ti) => tl.tracks[ti].clips).filter((c) => strictlyInside(c, at)).map((c) => c.id);
	if (under.length === 0) throw nothingToCut(tl, at, lanes);
	const skipped = tracks == null ? lockedWhere(tl, (c) => strictlyInside(c, at)) : [];
	// Every clip the cut will reach, partners included, to pair the halves afterwards.
	const reached: [string, string][] = tl.tracks.flatMap((t) =>
		t.clips.filter((c) => strictlyInside(c, at)).map((c): [string, string] => [t.id, c.id])
	);
	const before = new Set(tl.tracks.flatMap((t) => t.clips.map((c) => c.id)));

	const scratch: Timeline = structuredClone(tl);
	for (const id of under) {
		// A partner cut with an earlier clip is two clips now and `at` is on their edge.
		const clip = clipById(scratch, id);
		if (!clip || !strictlyInside(clip, at)) continue;
		if (links) splitClipLinked(scratch, id, at);
		else splitClip(scratch, id, at);
	}

	const cuts: ClipSplit[] = [];
	for (const [trackId, id] of reached) {
		const track = scratch.tracks.find((t) => t.id === trackId);
		const left = track?.clips.find((c) => c.id === id);
		if (!track || !left || Math.abs(endOf(left) - at) > DIFF_EPS) continue;
		const right = track.clips.find(
			(c) => !before.has(c.id) && Math.abs(c.timeline_start - at) <= DIFF_EPS && c.asset_id === left.asset_id
		);
		if (right) cuts.push({ track_id: trackId, left: structuredClone(left), right: structuredClone(right) });
	}
	tl.tracks = scratch.tracks;
	return { at, cuts, skipped };
}

/** The error for a cut with nothing to cut, saying what is there instead. */
function nothingToCut(tl: Timeline, at: number, lanes: readonly number[]): Error {
	const why: string[] = [];
	const onEdge = lanes.some((ti) =>
		tl.tracks[ti].clips.some((c) => Math.abs(c.timeline_start - at) <= DIFF_EPS || Math.abs(endOf(c) - at) <= DIFF_EPS)
	);
	if (onEdge) why.push('it is already a cut');
	const locked = lockedWhere(tl, (c) => strictlyInside(c, at));
	if (locked.length > 0) why.push(`the clips under it are on locked tracks (${locked.map((t) => t.name).join(', ')})`);
	if (why.length === 0) why.push('no clip on an unlocked track is under that time');
	return invalid(`nothing to cut at ${formatTime(at)}: ${why.join('; ')}`);
}

// ---- lift and extract ----------------------------------------------------------

/**
 * **Lift** `[from, to)` off the targeted tracks and leave the gap (`Timeline::lift_range`): a clip wholly inside is
 * removed, one the range reaches into from a side is shortened at that end, one the range sits in the middle of is cut
 * in two around it (the right half a new clip, as a split makes it). Nothing else moves; `extractRange` is the version
 * that closes the gap. `from` before 0 is held at 0; a range that is empty, reversed, not finite or has nothing in it is
 * an error. `tracks` as for `splitAllTracks`; with `links` the linked partners of the clips the range reaches are cut
 * with them, and a partner on a locked track refuses the lot. Mutates `tl`; all or nothing.
 */
export function liftRange(
	tl: Timeline,
	from: number,
	to: number,
	tracks: readonly string[] | null | undefined,
	links: boolean
): RangeEdit {
	return rangeEdit(tl, from, to, tracks, false, links);
}

/**
 * **Extract** `[from, to)` from the targeted tracks and close the gap (`Timeline::extract_range`): `liftRange`, then every
 * clip that started at or after `to` moves up by the range's length, each track on its own — and a clip that is linked to
 * one of them moves with it wherever it sits, the way the sync lock carries it for any ripple. Unlinked clips on a track
 * that was not named stay where they are, and so do overlays and markers (no ripple edit moves them). A range with nothing
 * in it still closes up when something follows it on a targeted track (deleting an empty stretch); with nothing in it and
 * nothing after it, it is an error.
 */
export function extractRange(
	tl: Timeline,
	from: number,
	to: number,
	tracks: readonly string[] | null | undefined,
	links: boolean
): RangeEdit {
	return rangeEdit(tl, from, to, tracks, true, links);
}

function rangeEdit(
	tl: Timeline,
	from: number,
	to: number,
	tracks: readonly string[] | null | undefined,
	close: boolean,
	links: boolean
): RangeEdit {
	const verb = close ? 'extract' : 'lift';
	if (!Number.isFinite(from) || !Number.isFinite(to)) throw invalid(`the range to ${verb} must have finite ends`);
	if (to < from - DIFF_EPS)
		throw invalid(`the range to ${verb} ends (${formatTime(to)}) before it starts (${formatTime(from)})`);
	const [a, b] = [Math.max(from, 0), to];
	if (b - a <= DIFF_EPS)
		throw invalid(to <= 0 ? `the range to ${verb} is before the start of the timeline` : `the range to ${verb} is empty`);
	const d = b - a;
	const lanes = rangeLanes(tl, tracks);
	const onLane = new Set(lanes);

	// Where every clip stood before: `[lane, start, end]`.
	const info = new Map<string, [number, number, number]>();
	tl.tracks.forEach((t, ti) => t.clips.forEach((c) => info.set(c.id, [ti, c.timeline_start, endOf(c)])));
	const overlaps = (s: number, e: number) => s < b - DIFF_EPS && e > a + DIFF_EPS;
	const reached = (s: number, e: number) => overlaps(s, e) || (close && s >= b - DIFF_EPS);

	// Who takes part: the clips of the targeted lanes, and the linked partners of those the range reaches, wherever they are.
	const takingPart = new Set(lanes.flatMap((ti) => tl.tracks[ti].clips.map((c) => c.id)));
	const groups = links ? linkGroups(tl) : [];
	const guestLanes = new Set<number>();
	for (const [, members] of groups) {
		const driven = members.some((m) => {
			const [ti, s, e] = info.get(m)!;
			return onLane.has(ti) && reached(s, e);
		});
		if (!driven) continue;
		for (const m of members) {
			const [ti, s, e] = info.get(m)!;
			if (onLane.has(ti) || !reached(s, e)) continue;
			if (tl.tracks[ti].locked) throw lockedPartner(tl.tracks[ti]);
			takingPart.add(m);
			guestLanes.add(ti);
		}
	}
	const skipped = tracks == null ? lockedWhere(tl, (c) => reached(c.timeline_start, endOf(c))) : [];

	const scratch: Timeline = structuredClone(tl);
	const fate = new Map<string, Fate>();
	// The clips this edit made (the halves after the range) and the clip each was cut from.
	const pieces = new Map<string, string>();
	const touched = new Set<number>();
	let [removed, trimmed, split, moved] = [0, 0, 0, 0];

	const cutting = scratch.tracks
		.flatMap((t) => t.clips)
		.filter((c) => takingPart.has(c.id) && overlaps(c.timeline_start, endOf(c)))
		.map((c) => c.id);
	for (const id of cutting) {
		const [ti, ci] = locateIndex(scratch, id)!;
		const clip = scratch.tracks[ti].clips[ci];
		const [start, end] = [clip.timeline_start, endOf(clip)];
		touched.add(ti);
		const fromBefore = start < a - DIFF_EPS;
		const pastEnd = end > b + DIFF_EPS;
		if (!fromBefore && !pastEnd) {
			scratch.tracks[ti].clips.splice(ci, 1);
			fate.set(id, { kind: 'removed' });
			removed++;
		} else if (fromBefore && !pastEnd) {
			cutEndAt(clip, a);
			fate.set(id, { kind: 'tail' });
			trimmed++;
		} else if (!fromBefore && pastEnd) {
			cutStartAt(clip, b);
			fate.set(id, { kind: 'head' });
			trimmed++;
		} else {
			// Two ordinary splits, the part between them dropped: the pieces are exactly what a split makes of a clip,
			// keyframes, ramp and fades included.
			const [, rest] = splitClip(scratch, id, a);
			const [middle, right] = splitClip(scratch, rest.id, b);
			const [mti, mci] = locateIndex(scratch, middle.id)!;
			scratch.tracks[mti].clips.splice(mci, 1);
			fate.set(id, { kind: 'split', right: right.id });
			pieces.set(right.id, id);
			split++;
		}
	}

	if (close) {
		scratch.tracks.forEach((track, ti) => {
			for (const clip of track.clips) {
				const mine = takingPart.has(clip.id) || pieces.has(clip.id);
				if (!mine || clip.timeline_start < b - DIFF_EPS) continue;
				// What began at the range's end begins at its start, exactly.
				clip.timeline_start = Math.abs(clip.timeline_start - b) <= DIFF_EPS ? a : Math.max(clip.timeline_start - d, 0);
				moved++;
				touched.add(ti);
			}
		});
	}
	for (const ti of touched) sortByStart(scratch.tracks[ti]);
	if (removed + trimmed + split === 0 && moved === 0) {
		const locked = skipped.map((t) => t.name);
		const and = locked.length === 0 ? '' : ` (${locked.join(', ')} locked)`;
		throw invalid(
			`nothing to ${verb} between ${formatTime(a)} and ${formatTime(b)}: ${
				close
					? 'no clip is in that range and none follows it on a targeted track'
					: 'no clip on a targeted track is in that range'
			}${and}`
		);
	}

	// A linked clip that was not on a named track may not have been moved onto a clip that took no part (the sync
	// lock's own rule for a follower).
	const part = (id: string) => takingPart.has(id) || pieces.has(id);
	for (const ti of [...guestLanes].sort((x, y) => x - y)) {
		const lane = scratch.tracks[ti];
		for (const m of lane.clips.filter((c) => part(c.id))) {
			for (const q of lane.clips.filter((c) => !part(c.id))) {
				if (!spansOverlap([m.timeline_start, endOf(m)], [q.timeline_start, endOf(q)])) continue;
				const was = (id: string): [number, number] | undefined => {
					const found = info.get(pieces.get(id) ?? id);
					return found ? [found[1], found[2]] : undefined;
				};
				const [x, y] = [was(m.id), was(q.id)];
				if (x && y && spansOverlap(x, y)) continue;
				throw runsIntoUnlinked(lane.name, Math.max(m.timeline_start, q.timeline_start));
			}
		}
	}

	// A group the range cut in two falls in two by side, as for a split: what is before the range keeps the link, what
	// is after it gets a new one.
	for (const [link, members] of groups) {
		if (!members.some((m) => fate.has(m))) continue;
		const [lefts, rights]: [string[], string[]] = [[], []];
		for (const m of members) {
			const f = fate.get(m);
			if (!f) {
				const start = info.get(m)![1];
				(start >= b - DIFF_EPS ? rights : lefts).push(m);
			} else if (f.kind === 'tail') lefts.push(m);
			else if (f.kind === 'head') rights.push(m);
			else if (f.kind === 'split') {
				lefts.push(m);
				rights.push(f.right);
			}
		}
		relinkSides(scratch, link, lefts, rights);
	}

	const changed = scratch.tracks.filter((_, ti) => touched.has(ti)).map((t) => t.id);
	tl.tracks = scratch.tracks;
	return { from: a, to: b, removed, trimmed, split, moved, closed: close ? d : 0, tracks: changed, skipped };
}

// ---- close a gap ---------------------------------------------------------------

/**
 * **Ripple delete a gap** (`Timeline::ripple_delete_gap`): the empty stretch of track `trackId` that `at` is in — from
 * the end of the clip before it (0 at the start of the track) to the start of the clip after it — is closed by moving
 * every clip from there on up by its length. A clip on another track is not touched by this (`runEdit`'s sync lock
 * carries the linked partners of what moved, as for any ripple). `at` on the edge of a gap is in it.
 *
 * Refuses a locked track, a time with a clip under it, a time with no gap (clips touching) and a gap with nothing after
 * it (there is nothing to close up). Mutates `tl`; all or nothing.
 */
export function rippleDeleteGap(tl: Timeline, trackId: string, at: number): GapClosed {
	if (!Number.isFinite(at)) throw invalid("the gap's time must be a finite number");
	const track = tl.tracks.find((t) => t.id === trackId);
	if (!track) throw new Error(`track not found: ${trackId}`);
	if (track.locked) throw invalid(`track ${track.name} is locked`);
	if (track.clips.some((c) => strictlyInside(c, at)))
		throw invalid(`there is a clip on ${track.name} at ${formatTime(at)}, not a gap`);
	const lo = track.clips.filter((c) => endOf(c) <= at + DIFF_EPS).reduce((m, c) => Math.max(m, endOf(c)), 0);
	const later = track.clips.filter((c) => c.timeline_start >= at - DIFF_EPS).map((c) => c.timeline_start);
	if (later.length === 0) throw invalid(`there is nothing after ${formatTime(at)} on ${track.name} to close up`);
	const hi = later.reduce((m, s) => Math.min(m, s));
	if (hi - lo <= DIFF_EPS) throw invalid(`there is no gap on ${track.name} at ${formatTime(at)}`);
	let moved = 0;
	for (const clip of track.clips) {
		if (clip.timeline_start < hi - DIFF_EPS) continue;
		clip.timeline_start = Math.abs(clip.timeline_start - hi) <= DIFF_EPS ? lo : Math.max(clip.timeline_start - (hi - lo), 0);
		moved++;
	}
	sortByStart(track);
	return { track_id: trackId, from: lo, to: hi, moved };
}
