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
//
// **No slivers.** A range that reaches into a clip would often leave a few milliseconds of it behind. `RangeCtx.frame`
// is the length of one frame of the timeline (`link-ops.ts` reads it from the rate the cut renders at, `renderedFps`,
// 30 fps when it has none, held to 1–240 fps like a delivery rate): a lift or an extract **never changes the range it
// was asked for**; when that range would leave a remainder shorter than a frame on a targeted lane or a partner it
// cuts, it refuses, changing nothing, naming the clip and the track and giving the nearest range that works (the
// clip's own edge, for every clip it would then leave a sliver of). A remainder of exactly a frame or more is kept. A
// cut of every track leaves a clip whole when a half of it would be that short (`not_cut`).
//
// Pure and all or nothing: an op works on a copy and replaces the timeline's tracks only when it went through, so a
// throw leaves the timeline as it was. `link-ops.ts` runs each inside one `runEdit` that never ripples (they decide
// their own layout), with the sync lock and guard.

import { MAX_DELIVERY_FPS, MIN_DELIVERY_FPS } from './delivery-formats';
import { cutEndAt, cutStartAt, DIFF_EPS } from './edit-modes';
import { formatTime } from './diff';
import { clipById, invalid, locateIndex, lockedPartner, relinkSides } from './link-groups';
import { splitClip, splitClipLinked } from './links';
import { runsIntoUnlinked, spansOverlap } from './ripple';
import type { Clip, CutAll, ClipSplit, GapClosed, RangeEdit, SkippedTrack, Timeline, Track } from './types';
import { clipDuration } from './types';

/** `RangeCtx`: what a range edit reads besides the timeline. */
export interface RangeCtx {
	/** Whether linked partners are cut and moved with the clips they are linked to. */
	links: boolean;
	/** How long one frame of the timeline lasts, in seconds (`1 / renderedFps`). Held to 1–240 fps; a value that is
	 *  no length at all leaves only the `DIFF_EPS` noise floor, which is no rule. */
	frame: number;
	/** What a refusal calls a clip: its asset's name or a title's own, `undefined` for neither. */
	nameOf?: (clip: Clip) => string | undefined;
}

const endOf = (c: Clip): number => c.timeline_start + clipDuration(c);

/** `at` is inside `clip`, not on one of its edges. */
function strictlyInside(clip: Clip, at: number): boolean {
	return clip.timeline_start + DIFF_EPS < at && at < endOf(clip) - DIFF_EPS;
}

/** `frame_len`: one frame in seconds as the rule uses it, held to what a delivery rate may be, or `null` when `frame`
 *  is no length at all. */
function frameLen(frame: number): number | null {
	return Number.isFinite(frame) && frame > 0 ? Math.min(Math.max(frame, 1 / MAX_DELIVERY_FPS), 1 / MIN_DELIVERY_FPS) : null;
}

/** `sliver`: the longest remainder that still counts as a sliver for a timeline whose frame lasts `frame` seconds.
 *  Anything up to it is shorter than a frame (to within float noise), anything longer is a frame or more and is kept.
 *  A frame that is no length at all leaves only the noise floor (`DIFF_EPS`), which is no rule at all. */
export function sliver(frame: number): number {
	const len = frameLen(frame);
	return len === null ? DIFF_EPS : len - DIFF_EPS;
}

/** `cuttable`: `at` is inside `clip` with more than `keep` seconds of it on each side. */
function cuttable(clip: Clip, at: number, keep: number): boolean {
	return clip.timeline_start + keep < at && at < endOf(clip) - keep;
}

/** How much two spans overlap, in seconds (0 when they do not). */
function overlapLen(a: [number, number], b: [number, number]): number {
	return Math.max(Math.min(a[1], b[1]) - Math.max(a[0], b[0]), 0);
}

/** `secs`: seconds as an agent can paste them back, rounded to the microsecond, shortest digits. */
const secs = (x: number): string => String(Math.round(x * 1e6) / 1e6);

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

/** `Timeline::link_groups`: the link groups of two or more clips, each with the clips in track order, the groups in the
 *  order their first clip appears. */
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
 * tracks that has `at` strictly inside it is split there, as `splitClip` splits one — and with `ctx.links` its linked
 * partners with it (`splitClipLinked`: the halves are re-linked by side, a partner on a locked track refuses the lot).
 * A track whose clips all end or start at `at` already has a cut there and is passed over, and so is a clip that would
 * be left with a half shorter than one frame (it is listed in `not_cut`); a time with nothing to cut at all is an error
 * that says why (nothing under it, it is already a cut or within a frame of one, the clips are locked), never an empty
 * edit.
 *
 * `tracks` names the tracks to cut (each must exist and be unlocked); `null` / `undefined` is every unlocked track, and
 * the locked ones with a clip the cut would have split are reported in `skipped`. Shifts nothing, so it is the same
 * with or without ripple mode. Mutates `tl`; all or nothing.
 */
export function splitAllTracks(tl: Timeline, at: number, tracks: readonly string[] | null | undefined, ctx: RangeCtx): CutAll {
	if (!Number.isFinite(at)) throw invalid('the cut time must be a finite number');
	const keep = sliver(ctx.frame);
	const lanes = rangeLanes(tl, tracks);
	const inside = lanes.flatMap((ti) => tl.tracks[ti].clips).filter((c) => strictlyInside(c, at));
	const under = inside.filter((c) => cuttable(c, at, keep)).map((c) => c.id);
	const notCut = inside.filter((c) => !cuttable(c, at, keep)).map((c) => c.id);
	if (under.length === 0) throw nothingToCut(tl, at, lanes, keep);
	const skipped = tracks == null ? lockedWhere(tl, (c) => cuttable(c, at, keep)) : [];
	// Every clip the cut will reach, partners included, to pair the halves afterwards.
	const reached: [string, string][] = tl.tracks.flatMap((t) =>
		t.clips.filter((c) => strictlyInside(c, at)).map((c): [string, string] => [t.id, c.id])
	);
	const before = new Set(tl.tracks.flatMap((t) => t.clips.map((c) => c.id)));

	const scratch: Timeline = structuredClone(tl);
	for (const id of under) {
		// A partner cut with an earlier clip is two clips now and `at` is on their edge.
		const clip = clipById(scratch, id);
		if (!clip || !cuttable(clip, at, keep)) continue;
		if (ctx.links) splitClipLinked(scratch, id, at, keep);
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
	return { at, cuts, skipped, not_cut: notCut };
}

/** The error for a cut with nothing to cut, saying what is there instead. */
function nothingToCut(tl: Timeline, at: number, lanes: readonly number[], keep: number): Error {
	const why: string[] = [];
	const clips = lanes.flatMap((ti) => tl.tracks[ti].clips);
	if (clips.some((c) => Math.abs(c.timeline_start - at) <= DIFF_EPS || Math.abs(endOf(c) - at) <= DIFF_EPS))
		why.push('it is already a cut');
	else if (clips.some((c) => strictlyInside(c, at) && !cuttable(c, at, keep)))
		why.push('it is already a cut to within a frame (a half shorter than one frame is not made)');
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
 * an error, and so is one that would leave a remainder shorter than one frame of a clip it cuts (the range is never
 * changed: the refusal gives the nearest one that works). `tracks` as for `splitAllTracks`; with `ctx.links` the linked
 * partners of the clips the range reaches are cut with them, and a partner on a locked track refuses the lot. Mutates
 * `tl`; all or nothing.
 */
export function liftRange(
	tl: Timeline,
	from: number,
	to: number,
	tracks: readonly string[] | null | undefined,
	ctx: RangeCtx
): RangeEdit {
	return rangeEdit(tl, from, to, tracks, false, ctx);
}

/**
 * **Extract** `[from, to)` from the targeted tracks and close the gap (`Timeline::extract_range`): `liftRange`, then every
 * clip that started at or after `to` moves up by the range's length, each track on its own — and a clip that is linked to
 * one of them moves with it wherever it sits, the way the sync lock carries it for any ripple. Unlinked clips on a track
 * that was not named stay where they are, and so do overlays and markers (no ripple edit moves them). A range with nothing
 * in it still closes up when something follows it on a targeted track (deleting an empty stretch); with nothing in it and
 * nothing after it, it is an error. A linked sound that reaches into the range only because it leads or trails a picture
 * that lies after the range is not cut: the sync lock carries it with its picture, as it does for a deleted gap.
 */
export function extractRange(
	tl: Timeline,
	from: number,
	to: number,
	tracks: readonly string[] | null | undefined,
	ctx: RangeCtx
): RangeEdit {
	return rangeEdit(tl, from, to, tracks, true, ctx);
}

/** `Participants`: who takes part in an edit of `[a, b)` — every clip of the targeted lanes, and the linked partners it
 *  cuts or moves with them, wherever they sit (those on `guestLanes`). A partner on a locked track that the range cuts or
 *  moves refuses the lot. */
interface Participants {
	takingPart: Set<string>;
	guestLanes: Set<number>;
}

type Spans = ReadonlyMap<string, [number, number, number]>;

function participants(
	tl: Timeline,
	lanes: readonly number[],
	info: Spans,
	groups: readonly [string, string[]][],
	a: number,
	b: number,
	close: boolean
): Participants {
	const onLane = new Set(lanes);
	const overlaps = (s: number, e: number) => s < b - DIFF_EPS && e > a + DIFF_EPS;
	const reaches = (s: number, e: number) => overlaps(s, e) || (close && s >= b - DIFF_EPS);
	const takingPart = new Set(lanes.flatMap((ti) => tl.tracks[ti].clips.map((c) => c.id)));
	const guestLanes = new Set<number>();
	for (const [, members] of groups) {
		const driven = members.some((m) => {
			const [ti, s, e] = info.get(m)!;
			return onLane.has(ti) && reaches(s, e);
		});
		if (!driven) continue;
		// A partner is cut with the clip it is linked to. One that reaches into the range only by leading or trailing its
		// picture, when the picture itself lies after the range (an extract moves it, no more), is not cut: the sync
		// lock carries it with its picture, as it does for a deleted gap, trimming a linked sound it lands on and
		// refusing where it would land on an unlinked clip.
		const cutWith = members.some((m) => {
			const [ti, s, e] = info.get(m)!;
			return onLane.has(ti) && overlaps(s, e);
		});
		for (const m of members) {
			const [ti, s, e] = info.get(m)!;
			if (onLane.has(ti) || !reaches(s, e)) continue;
			if (overlaps(s, e) && !cutWith) continue;
			if (tl.tracks[ti].locked) throw lockedPartner(tl.tracks[ti]);
			takingPart.add(m);
			guestLanes.add(ti);
		}
	}
	return { takingPart, guestLanes };
}

/** `Sliver`: a clip a range would leave a remainder shorter than a frame of. */
interface Sliver {
	id: string;
	lane: number;
	/** What would stay before the range, when that is a sliver. */
	before?: number;
	/** What would stay after it, when that is a sliver. */
	after?: number;
}

/** `Timeline::slivers`: the clips of `takingPart` that `[a, b)` would leave a remainder of `keep` seconds or less of, in
 *  track order. */
function slivers(tl: Timeline, info: Spans, takingPart: ReadonlySet<string>, a: number, b: number, keep: number): Sliver[] {
	const found: Sliver[] = [];
	tl.tracks.forEach((track, ti) => {
		for (const clip of track.clips.filter((c) => takingPart.has(c.id))) {
			const [, s, e] = info.get(clip.id)!;
			const before = s < a - DIFF_EPS && e > a + DIFF_EPS && a - s <= keep ? a - s : undefined;
			const after = s < b - DIFF_EPS && e > b + DIFF_EPS && e - b <= keep ? e - b : undefined;
			if (before !== undefined || after !== undefined) found.push({ id: clip.id, lane: ti, before, after });
		}
	});
	return found;
}

/** `Timeline::sliver_refusal`: the refusal for a range that would leave a sliver — what it would leave of which clip,
 *  and the nearest range that leaves none: the range grown to the edge of each clip it would leave a sliver of, until no
 *  clip is left one (partners recounted for each try). */
function sliverRefusal(
	tl: Timeline,
	lanes: readonly number[],
	info: Spans,
	groups: readonly [string, string[]][],
	a: number,
	b: number,
	close: boolean,
	keep: number,
	found: readonly Sliver[],
	ctx: RangeCtx
): Error {
	const verb = close ? 'extracting' : 'lifting';
	let [lo, hi] = [a, b];
	let now = found.map((s) => ({ id: s.id, before: s.before !== undefined, after: s.after !== undefined }));
	for (let round = 0; round < 16; round++) {
		for (const { id, before, after } of now) {
			const [, s, e] = info.get(id)!;
			if (before) lo = Math.min(lo, s);
			if (after) hi = Math.max(hi, e);
		}
		let part: Participants;
		try {
			part = participants(tl, lanes, info, groups, lo, hi, close);
		} catch {
			break;
		}
		now = slivers(tl, info, part.takingPart, lo, hi, keep).map((s) => ({
			id: s.id,
			before: s.before !== undefined,
			after: s.after !== undefined
		}));
		if (now.length === 0) break;
	}
	const first = found[0];
	const clip = clipById(tl, first.id)!;
	const left = first.before ?? first.after!;
	const track = tl.tracks[first.lane].name;
	const name = ctx.nameOf?.(clip);
	const named = name !== undefined ? `${name} on ${track} (clip ${clip.id})` : `clip ${clip.id} on ${track}`;
	const n = found.length - 1;
	const more = n === 0 ? '' : ` (and ${n} more remainder${n === 1 ? '' : 's'} like it)`;
	const frameMs = Math.round((frameLen(ctx.frame) ?? 0) * 1000);
	const instead =
		lo !== a && hi !== b ? `start at ${secs(lo)} and end at ${secs(hi)}` : lo !== a ? `start at ${secs(lo)}` : `end at ${secs(hi)}`;
	return invalid(
		`${verb} ${secs(a)}–${secs(b)} s would leave ${Math.max(Math.round(left * 1000), 1)} ms of ${named}${more}, less than a frame (${frameMs} ms): ${instead} instead`
	);
}

function rangeEdit(
	tl: Timeline,
	from: number,
	to: number,
	tracks: readonly string[] | null | undefined,
	close: boolean,
	ctx: RangeCtx
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

	// Where every clip stood before: `[lane, start, end]`.
	const info = new Map<string, [number, number, number]>();
	tl.tracks.forEach((t, ti) => t.clips.forEach((c) => info.set(c.id, [ti, c.timeline_start, endOf(c)])));
	const overlaps = (s: number, e: number) => s < b - DIFF_EPS && e > a + DIFF_EPS;
	const reached = (s: number, e: number) => overlaps(s, e) || (close && s >= b - DIFF_EPS);

	// Who takes part: the clips of the targeted lanes, and the linked partners of those the range reaches, wherever they are.
	const groups = ctx.links ? linkGroups(tl) : [];
	const { takingPart, guestLanes } = participants(tl, lanes, info, groups, a, b, close);
	const skipped = tracks == null ? lockedWhere(tl, (c) => reached(c.timeline_start, endOf(c))) : [];

	// The range is the one that was asked for, or it is refused: a remainder shorter than a frame of a clip it cuts is
	// not left behind, and is not taken with the range either.
	const keep = sliver(ctx.frame);
	const found = slivers(tl, info, takingPart, a, b, keep);
	if (found.length > 0) throw sliverRefusal(tl, lanes, info, groups, a, b, close, keep, found, ctx);

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
	// lock's own rule for a follower), nor further onto one it already overlapped: an overlap that was there may stay
	// as it was, never grow. An overlap of less than a frame with linked material is let through: a sound a few
	// milliseconds longer than its picture is routine, and the clip it lies on is its own shot's.
	const part = (id: string) => takingPart.has(id) || pieces.has(id);
	const was = (id: string): [number, number] | undefined => {
		const found = info.get(pieces.get(id) ?? id);
		return found ? [found[1], found[2]] : undefined;
	};
	for (const ti of [...guestLanes].sort((x, y) => x - y)) {
		const lane = scratch.tracks[ti];
		for (const m of lane.clips.filter((c) => part(c.id))) {
			for (const q of lane.clips.filter((c) => !part(c.id))) {
				const [nowM, nowQ]: [[number, number], [number, number]] = [
					[m.timeline_start, endOf(m)],
					[q.timeline_start, endOf(q)]
				];
				if (!spansOverlap(nowM, nowQ)) continue;
				const [x, y] = [was(m.id), was(q.id)];
				const had = x && y ? overlapLen(x, y) : 0;
				const now = overlapLen(nowM, nowQ);
				if (now <= had + DIFF_EPS || (q.link_id && now <= keep)) continue;
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
	return { from: a, to: b, removed, trimmed, split, moved, closed: close ? d : 0, tracks: changed, skipped, lock_trimmed: [] };
}

// ---- close a gap ---------------------------------------------------------------

/**
 * **Ripple delete a gap** (`Timeline::ripple_delete_gap`): the empty stretch of track `trackId` that `at` is in — from
 * the end of the clip before it (0 at the start of the track) to the start of the clip after it — is closed by moving
 * every clip from there on up by its length. A clip on another track is not touched by this (`runEdit`'s sync lock
 * carries the linked partners of what moved, as for any ripple). `at` on the edge of a gap is in it.
 *
 * Refuses a locked track, a time before the start of the timeline, a time with a clip under it, a time with no gap
 * (clips touching) and a gap with nothing after it (there is nothing to close up). Mutates `tl`; all or nothing.
 */
export function rippleDeleteGap(tl: Timeline, trackId: string, at: number): GapClosed {
	if (!Number.isFinite(at)) throw invalid("the gap's time must be a finite number");
	if (at < 0) throw invalid("the gap's time is before the start of the timeline");
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
	return { track_id: trackId, from: lo, to: hi, moved, lock_trimmed: [] };
}
