/* Where a dragged group of clips lands, and whether that is allowed — pure.
 *
 * Dragging any clip of a selection drags them all by the same amount: one Δt, and,
 * if the pointer is over another lane of the grabbed clip's kind, one *lane
 * offset* — "two lanes down" — applied to every clip within **its own kind's**
 * lanes (video clips shift through the video tracks, audio clips through the audio
 * ones; the lanes of a kind are its tracks in timeline order). A group that would
 * need a lane that is not there, or one that is locked, does not land.
 *
 * `planMove` produces both halves of that: the ghosts the timeline draws (also for
 * a drop that is refused, so it can be drawn red), and the `ClipMove`s for the one
 * `move_clips` call that makes it real. Its checks are the backend's — kerf-core's
 * `Timeline::move_clips`, mirrored for the harness in `multi-edit.ts` — in the same
 * terms: the group is checked *as a group* (the places moving clips are leaving
 * are free, they may not land on each other or on a clip that is staying), a start
 * before 0 refuses the lot rather than clamping one clip and reshaping the group,
 * and a locked track — source or destination — refuses it. Checking here means a
 * refused drop is shown while it is still a drag, with its reason, instead of as
 * an error after it. */

import { formatTime } from './diff';
import { DIFF_EPS, spansOverlap } from './ripple';
import type { ClipMove, StreamKind, Timeline } from './types';
import { clipDuration } from './types';

/** A track as the planner sees it: lane facts and where its clips sit. */
export interface MoveTrack {
	id: string;
	name: string;
	kind: StreamKind;
	locked: boolean;
	clips: { id: string; start: number; dur: number }[];
}

/** The planner's view of a timeline (which is what `Timeline` is, with the
 *  durations worked out — speed and trim are the model's business, not the drag's). */
export function moveTracks(timeline: Timeline): MoveTrack[] {
	return timeline.tracks.map((t) => ({
		id: t.id,
		name: t.name,
		kind: t.kind,
		locked: !!t.locked,
		clips: t.clips.map((c) => ({ id: c.id, start: c.timeline_start, dur: clipDuration(c) }))
	}));
}

/** One clip's landing spot, drawn as the dashed ghost of the drag. */
export interface Ghost {
	clipId: string;
	trackId: string;
	start: number;
	dur: number;
}

export interface MovePlan {
	/** Whether the group may land here. */
	ok: boolean;
	/** Why not, phrased like the backend's refusals; `null` when it may. */
	reason: string | null;
	/** What `move_clips` is called with — empty unless `ok` and something moves. */
	moves: ClipMove[];
	/** Every clip of the group where it would land, `ok` or not. */
	ghosts: Ghost[];
	/** Seconds the group moves by. */
	delta: number;
	/** Lanes the group moves by within each kind (negative: up). */
	laneShift: number;
	/** The drop changes nothing: no time, no lane. */
	noop: boolean;
}

const refuse = (reason: string, ghosts: Ghost[], delta = 0, laneShift = 0): MovePlan => ({
	ok: false,
	reason,
	moves: [],
	ghosts,
	delta,
	laneShift,
	noop: false
});

/**
 * Plan dragging `memberIds` — the grabbed clip among them — so the grabbed clip
 * starts at `grabbedStart` on `destTrackId` (a track of its own kind). The grabbed
 * clip's start is whatever the gesture decided (snapped, frame-quantized); the
 * rest of the group keeps its offsets from it.
 */
export function planMove(
	tracks: readonly MoveTrack[],
	memberIds: Iterable<string>,
	grabbedId: string,
	grabbedStart: number,
	destTrackId: string
): MovePlan {
	const ids = new Set(memberIds);
	ids.add(grabbedId);

	// The group as it stands: only clips that still exist (a selection can name a
	// clip another edit removed).
	const group: { id: string; ti: number; start: number; dur: number }[] = [];
	tracks.forEach((t, ti) => {
		for (const c of t.clips) if (ids.has(c.id)) group.push({ id: c.id, ti, start: c.start, dur: c.dur });
	});
	const grabbed = group.find((g) => g.id === grabbedId);
	if (!grabbed) return refuse('That clip is no longer on the timeline', []);
	const dest = tracks.findIndex((t) => t.id === destTrackId);
	if (dest < 0) return refuse('That track is no longer on the timeline', []);
	const kind = tracks[grabbed.ti].kind;
	if (tracks[dest].kind !== kind) {
		return refuse(`A ${kind} clip can only move to a ${kind} track`, []);
	}

	// A kind's lanes, in timeline order, and each track's place among them.
	const lanes = new Map<StreamKind, number[]>();
	tracks.forEach((t, ti) => lanes.set(t.kind, [...(lanes.get(t.kind) ?? []), ti]));
	const laneOf = (ti: number) => lanes.get(tracks[ti].kind)!.indexOf(ti);
	const laneShift = laneOf(dest) - laneOf(grabbed.ti);
	const delta = grabbedStart - grabbed.start;

	group.sort((a, b) => a.start - b.start || a.ti - b.ti);

	// Where each clip lands, and the first reason it cannot.
	let reason: string | null = null;
	const landed: { id: string; from: number; to: number; start: number; dur: number }[] = [];
	const ghosts: Ghost[] = [];
	for (const g of group) {
		const row = lanes.get(tracks[g.ti].kind)!;
		const to = row[laneOf(g.ti) + laneShift];
		let start = g.start + delta;
		if (start < 0 && start > -DIFF_EPS) start = 0;
		if (to === undefined) {
			reason ??= `There is no ${tracks[g.ti].kind} track ${laneShift > 0 ? 'below' : 'above'} ${tracks[g.ti].name} for its clip`;
			ghosts.push({ clipId: g.id, trackId: tracks[g.ti].id, start: Math.max(0, start), dur: g.dur });
			continue;
		}
		for (const ti of [g.ti, to]) {
			if (tracks[ti].locked) reason ??= `Track ${tracks[ti].name} is locked`;
		}
		if (start < 0) reason ??= 'The clips would start before the beginning of the timeline';
		landed.push({ id: g.id, from: g.ti, to, start, dur: g.dur });
		ghosts.push({ clipId: g.id, trackId: tracks[to].id, start: Math.max(0, start), dur: g.dur });
	}

	// The group against what stays, and against itself.
	if (reason === null) {
		const moving = new Set(landed.map((l) => l.id));
		for (const l of landed) {
			const span: [number, number] = [l.start, l.start + l.dur];
			const staying = tracks[l.to].clips.some(
				(c) => !moving.has(c.id) && spansOverlap(span, [c.start, c.start + c.dur])
			);
			const together = landed.some(
				(o) => o.id !== l.id && o.to === l.to && spansOverlap(span, [o.start, o.start + o.dur])
			);
			if (staying || together) {
				reason = `The clips would overlap on track ${tracks[l.to].name} at ${formatTime(l.start)}`;
				break;
			}
		}
	}
	if (reason !== null) return refuse(reason, ghosts, delta, laneShift);

	const noop = laneShift === 0 && Math.abs(delta) < DIFF_EPS;
	const moves: ClipMove[] = noop
		? []
		: landed.map((l) => {
				const m: ClipMove = { clip_id: l.id, timeline_start: l.start };
				if (l.to !== l.from) m.track_id = tracks[l.to].id;
				return m;
			});
	return { ok: true, reason: null, moves, ghosts, delta, laneShift, noop };
}
