/* Dragging a clip's edge when the clip has **linked partners** — pure. `ripple-trim.ts`
 * answers the drag for one track; a linked trim reaches further, in two ways the drag has
 * to show:
 *
 *  - *The bounds.* `trim_clip` carries the edge to every partner that shares it
 *    (`carryExtentEdit`): clamped to the partner's own footage — which only makes the
 *    partner trim less, the way a roll or slide would be held — but never overlap-checked,
 *    so it is the *drag* that has to keep the partner off its own neighbours
 *    (`linkedTrimBounds`: the named clip's bounds intersected with each sharing partner's,
 *    footage aside).
 *  - *The outcome.* With ripple on, the backend ripples every lane, and its sync lock
 *    (`conform_links`) makes the clips a ripple pushed drag their partners along: trim a title
 *    on V1 and the next shot's sound on A1 moves too, a J/L-cut offset kept. `linkedTrimPreview`
 *    applies the trim to a scratch copy of the lanes that matter, then the same steps the
 *    backend takes (`carryExtentEdit`, the per-lane ripple, then `conformLinks` with the
 *    trimmed clip as its anchor), then the sync guard, and reads off every clip that changed —
 *    on any track — so the ghosts of the partners' ripple are drawn live, and a refusal (a
 *    locked partner, an unlinked clip in the way, a pair pulled out of step) is drawn red with
 *    its reason instead of found out on release.
 */

import { ADJACENT_EPS, type SourceLimits } from './edit-modes';
import { trimEdit } from './frames';
import { firstSyncBreak, hasLinks, linkPartners, locateIndex, syncBreakError } from './link-groups';
import { gestureReason, reasonOf } from './link-ui';
import { carryExtentEdit } from './links';
import { conformLinks, DIFF_EPS, rippleLanes } from './ripple';
import { trimBounds, type TrimBounds } from './ripple-trim';
import { plainCopy } from './trim-tools';
import type { Clip, Timeline, Track } from './types';
import { clipDuration } from './types';

const endOf = (c: Clip) => c.timeline_start + clipDuration(c);
const byStart = (clips: readonly Clip[]) => [...clips].sort((a, b) => a.timeline_start - b.timeline_start);

/**
 * Where the `edge` of `clip` may be dragged given its linked partners: `trimBounds` for
 * the clip itself, narrowed by each partner on an unlocked track that shares the edge
 * (within `ADJACENT_EPS`, the backend's own test) to what keeps *its* lane legal. A
 * partner's footage is not a limit — the backend trims it less rather than refusing — and
 * with `ripple` the neighbours on its lane are pushed rather than a limit, as for the clip.
 * The range always holds the edge where it is, so a drag can always stay put.
 */
export function linkedTrimBounds(
	timeline: Timeline,
	clip: Clip,
	edge: 'l' | 'r',
	assetDuration: number | undefined,
	still: boolean,
	ripple: boolean
): TrimBounds {
	const home = timeline.tracks.find((t) => t.clips.some((c) => c.id === clip.id));
	let bounds = trimBounds(clip, edge, byStart(home?.clips ?? [clip]), assetDuration, still, ripple);
	const mine = edge === 'l' ? clip.timeline_start : endOf(clip);
	for (const id of linkPartners(timeline, clip.id)) {
		const [ti, ci] = locateIndex(timeline, id)!;
		const lane: Track = timeline.tracks[ti];
		if (lane.locked) continue; // the preview says why that refuses
		const partner = lane.clips[ci];
		const theirs = edge === 'l' ? partner.timeline_start : endOf(partner);
		if (Math.abs(theirs - mine) > ADJACENT_EPS) continue; // not the partner's edge: untouched
		// `still` drops the footage clamp (the partner is clamped by the backend, not by the drag).
		const pb = trimBounds(partner, edge, byStart(lane.clips), undefined, true, ripple);
		const off = theirs - mine;
		bounds = { min: Math.max(bounds.min, pb.min - off), max: Math.min(bounds.max, pb.max - off) };
	}
	return { min: Math.min(bounds.min, mine), max: Math.max(bounds.max, mine) };
}

/** One clip a linked trim changes, where it ends up, on whichever track. */
export interface LinkedTrimGhost {
	id: string;
	trackId: string;
	start: number;
	dur: number;
}

export interface LinkedTrimPreview {
	/** The trimmed clip, and every clip the trim changes — partners that follow the edge,
	 *  clips the ripple pushes, and the partners that follow *those* — each where it lands. */
	ghosts: LinkedTrimGhost[];
	/** The clips that are moved, not trimmed (the trimmed clip not among them). */
	shifted: Set<string>;
	/** Whether the backend would take it: no lane left overlapping, no partner refused,
	 *  no pair pulled out of step. */
	ok: boolean;
	/** Why not, in a sentence — `null` when `ok`, or when the only fault is a lane overlapping. */
	reason: string | null;
}

export interface LinkedTrimOptions {
	/** The project's ripple mode: the trim ripples, sync lock included. */
	ripple: boolean;
	/** Linked partners follow (false: Alt — the clip is trimmed alone, and ripple does not sync-lock). */
	links: boolean;
	/** How far each asset's footage reaches (what a partner is clamped to). */
	footage: SourceLimits;
}

/**
 * What dragging `edge` of `clipId` to timeline time `pos` leaves on the whole cut:
 * the trim as `trim_clip` writes it, its partners following the edge when `links`, then
 * ripple (and its sync lock, when `links`) when `ripple` — all on a scratch copy, in the
 * order the backend does them. `pos` is the gesture's one rounded position. `null` when
 * the clip is not on the timeline. Never touches `timeline` (a reactive copy is fine).
 */
export function linkedTrimPreview(
	timeline: Timeline,
	clipId: string,
	edge: 'l' | 'r',
	pos: number,
	opts: LinkedTrimOptions
): LinkedTrimPreview | null {
	const at = locateIndex(timeline, clipId);
	if (!at) return null;
	// Only the lanes that can change: the clip's own, and every lane with a linked clip on it
	// (a partner's, and the partners of whatever the ripple pushes). Copied all the way down —
	// the editor's timeline is `$state`, which `structuredClone` refuses.
	const lanes = timeline.tracks.filter((t, i) => i === at[0] || (opts.links && t.clips.some((c) => c.link_id)));
	const before: Timeline = { tracks: plainCopy(lanes) };
	const after: Timeline = structuredClone(before);
	const home = after.tracks.find((t) => t.clips.some((c) => c.id === clipId))!;
	const clip = home.clips.find((c) => c.id === clipId)!;
	const was = structuredClone(clip);
	const e = trimEdit(clip, edge, pos);
	// What `trim_clip` does with those fields.
	if (e.source_in !== undefined) clip.source_in = e.source_in;
	if (e.source_out !== undefined) clip.source_out = e.source_out;
	if (e.timeline_start !== undefined) {
		clip.timeline_start = Math.max(0, e.timeline_start);
		home.clips.sort((a, b) => a.timeline_start - b.timeline_start);
	}
	let reason: string | null = null;
	if (opts.links) {
		try {
			carryExtentEdit(after, clipId, was, opts.footage);
		} catch (err) {
			reason = gestureReason(reasonOf(err));
		}
	}
	// The rest of `Project::run_edit`: the per-lane ripple, then the sync lock and guard.
	let result = after;
	if (reason === null && opts.ripple) result = rippleLanes(after, before);
	if (reason === null && opts.links && hasLinks(before)) {
		try {
			conformLinks(result, before, new Set([clipId]));
		} catch (err) {
			reason = gestureReason(reasonOf(err));
		}
		const broke = reason === null ? firstSyncBreak(result, before) : null;
		if (broke) reason = gestureReason(reasonOf(syncBreakError(broke)));
	}

	const was0 = new Map<string, { start: number; dur: number }>();
	for (const t of before.tracks) for (const c of t.clips) was0.set(c.id, { start: c.timeline_start, dur: clipDuration(c) });
	const ghosts: LinkedTrimGhost[] = [];
	const shifted = new Set<string>();
	let overlapping = false;
	for (const t of result.tracks) {
		let touched = false;
		for (const c of t.clips) {
			const w = was0.get(c.id);
			if (!w) continue;
			const start = c.timeline_start;
			const dur = clipDuration(c);
			const moved = Math.abs(start - w.start) > 1e-6;
			if (c.id === clipId || moved || Math.abs(dur - w.dur) > 1e-6) {
				ghosts.push({ id: c.id, trackId: t.id, start, dur });
				touched = true;
				if (moved && c.id !== clipId) shifted.add(c.id);
			}
		}
		if (!touched) continue;
		// One sweep over the lane in start order: a clip starting before the furthest end so far overlaps.
		let reach = -Infinity;
		for (const c of byStart(t.clips)) {
			if (c.timeline_start < reach - DIFF_EPS) overlapping = true;
			reach = Math.max(reach, endOf(c));
		}
	}
	return { ghosts, shifted, ok: reason === null && !overlapping, reason };
}
