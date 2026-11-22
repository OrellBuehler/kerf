// Moving about the cut from the keyboard — pure. Where the next cut is, which clip is under the playhead, which
// clip is next to the selected one along its track or on the track above or below. The page's key table and the
// timeline's own key handler both run these (`ops.ts`), so a key that goes to the next edit does the same whichever
// of them heard it.
//
// Nothing here writes anything: a function answers a time or a clip id, and says why when there is none.

import { stepPoint } from './frames';
import type { Clip, Timeline, Track } from './types';
import { clipDuration } from './types';

const endOf = (c: Clip): number => c.timeline_start + clipDuration(c);

// ---- edit points ----------------------------------------------------------------

/**
 * The tracks an edit-point key reads: those that hold the selection, so that with one clip picked "the next edit"
 * is the next cut *of its track*; every track when nothing is selected (`null`). There is no track-targeting
 * switch, so the selection is how a person says which lanes they mean.
 */
export function targetedTracks(timeline: Timeline, selected: readonly string[]): ReadonlySet<string> | null {
	if (selected.length === 0) return null;
	const want = new Set(selected);
	const ids = new Set(timeline.tracks.filter((t) => t.clips.some((c) => want.has(c.id))).map((t) => t.id));
	return ids.size > 0 ? ids : null;
}

/** Every cut of `tracks` (all when `null`): where a clip starts and where one ends, each once, ascending. A start
 *  and an end that meet are one cut; so are the same cut on two tracks. */
export function editPoints(timeline: Timeline, tracks: ReadonlySet<string> | null = null): number[] {
	const out: number[] = [];
	for (const t of timeline.tracks) {
		if (tracks && !tracks.has(t.id)) continue;
		for (const c of t.clips) out.push(c.timeline_start, endOf(c));
	}
	out.sort((a, b) => a - b);
	return out.filter((x, i) => i === 0 || Math.abs(x - out[i - 1]) > 1e-9);
}

/**
 * Where the playhead goes for *Previous* (`-1`) / *Next* (`1`) edit point, or `null` when there is none that way.
 * A cut is compared where the playhead will stand on it (`stepPoint`, rounding up like every "go to the start of"):
 * a cut between two frames is not still ahead of a playhead that has just gone to it, so a press never sticks.
 */
export function stepEdit(timeline: Timeline, time: number, dir: 1 | -1, fps: number, duration: number, tracks: ReadonlySet<string> | null = null): number | null {
	return stepPoint(editPoints(timeline, tracks), time, dir, fps, duration, 'up');
}

/** What to say when there is no edit point that way. */
export const noEditPoint = (dir: 1 | -1): string => (dir > 0 ? 'There is no later edit point' : 'There is no earlier edit point');

// ---- clips under the playhead ----------------------------------------------------

/** The clip of `track` the playhead is inside (start inclusive, end exclusive: at a cut it is the one that
 *  begins there), or undefined. */
export function clipAt(track: Track, time: number): Clip | undefined {
	return track.clips.find((c) => time >= c.timeline_start - 1e-9 && time < endOf(c) - 1e-9);
}

/** Every clip under `time`, topmost drawn track first (`drawn` is `displayTracks`' order). */
export function clipsUnder(drawn: readonly Track[], time: number): { clip: Clip; track: Track }[] {
	const out: { clip: Clip; track: Track }[] = [];
	for (const track of drawn) {
		const clip = clipAt(track, time);
		if (clip) out.push({ clip, track });
	}
	return out;
}

/**
 * The clip *Select the clip at the playhead* picks: the topmost clip under the playhead that is not already selected,
 * so repeated presses walk the stack (V2, V1, A1, ...) — a linked pair, selected together, is one stop — and once
 * everything under the playhead is selected, the topmost again. `null` when the playhead is over nothing.
 */
export function pickAtPlayhead(drawn: readonly Track[], time: number, selected: readonly string[]): string | null {
	const under = clipsUnder(drawn, time);
	if (under.length === 0) return null;
	const have = new Set(selected);
	return (under.find((u) => !have.has(u.clip.id)) ?? under[0]).clip.id;
}

// ---- moving the selection --------------------------------------------------------

/** The clips of a track in time order (a copy: the model's order is not promised). */
const inOrder = (track: Track): Clip[] => [...track.clips].sort((a, b) => a.timeline_start - b.timeline_start);

/** The track that holds `clipId`. */
export function trackOf(tracks: readonly Track[], clipId: string): Track | undefined {
	return tracks.find((t) => t.clips.some((c) => c.id === clipId));
}

/** The clip next to `clipId` along its own track: `-1` the one before, `1` the one after; `null` at the end. */
export function alongTrack(track: Track, clipId: string, dir: 1 | -1): string | null {
	const lane = inOrder(track);
	const i = lane.findIndex((c) => c.id === clipId);
	return i < 0 ? null : (lane[i + dir]?.id ?? null);
}

/** How far a clip is from `time`, 0 when it is under it. */
const distance = (c: Clip, time: number): number => (time < c.timeline_start ? c.timeline_start - time : time >= endOf(c) ? time - endOf(c) : 0);

/** The clip of `track` nearest `time`; the earlier on a tie. */
export function nearestClip(track: Track, time: number): Clip | undefined {
	let best: Clip | undefined;
	let bestD = Infinity;
	for (const c of inOrder(track)) {
		const d = distance(c, time);
		if (d < bestD) {
			bestD = d;
			best = c;
		}
	}
	return best;
}

/**
 * The clip a move up (`-1`) / down (`1`) a track lands on: of the next track in that direction in drawn order that
 * has any clip, the one nearest the middle of the clip being left. `null` when there is no track that way with a
 * clip on it. A locked track counts: locking guards an edit, not the selection.
 */
export function acrossTracks(drawn: readonly Track[], clipId: string, dir: 1 | -1): string | null {
	const from = trackOf(drawn, clipId);
	if (!from) return null;
	const clip = from.clips.find((c) => c.id === clipId)!;
	const middle = clip.timeline_start + clipDuration(clip) / 2;
	for (let i = drawn.findIndex((t) => t.id === from.id) + dir; i >= 0 && i < drawn.length; i += dir) {
		const hit = nearestClip(drawn[i], middle);
		if (hit) return hit.id;
	}
	return null;
}

export type Step = 'prev' | 'next' | 'above' | 'below';

export interface Stepped {
	/** The clip the move lands on. */
	id: string;
	/** Whether it is added to the selection instead of replacing it. */
	extend: boolean;
}

/** What a selection move asks, or why there is nothing to do. */
export type StepResult = { ok: true; to: Stepped } | { ok: false; reason: string };

const NOTHING_THERE: Record<Step, string> = {
	prev: 'There is no clip before it on this track',
	next: 'There is no clip after it on this track',
	above: 'There is no clip on a track above',
	below: 'There is no clip on a track below'
};

/**
 * One *select previous / next clip*, *select the clip on the track above / below* — with `extend`, added to the
 * selection (along the track: like Shift-clicking it, the range between).
 *
 * `primary` is the clip the Inspector is on, the one the move starts from. With nothing selected there is nothing
 * to move from: the clip under the playhead is picked, and failing that the nearest one in the direction asked
 * (the next one after the playhead, the previous one before it; above / below start at the topmost / lowest track).
 */
export function stepSelection(drawn: readonly Track[], primary: string | null, step: Step, extend: boolean, time: number): StepResult {
	const from = primary ? trackOf(drawn, primary) : undefined;
	if (primary && from) {
		const id = step === 'prev' || step === 'next' ? alongTrack(from, primary, step === 'prev' ? -1 : 1) : acrossTracks(drawn, primary, step === 'above' ? -1 : 1);
		return id ? { ok: true, to: { id, extend } } : { ok: false, reason: NOTHING_THERE[step] };
	}
	const under = clipsUnder(drawn, time);
	if (under.length > 0) return { ok: true, to: { id: under[0].clip.id, extend: false } };
	const all = drawn.flatMap((t) => t.clips).sort((a, b) => a.timeline_start - b.timeline_start);
	const pick = step === 'prev' ? [...all].reverse().find((c) => endOf(c) <= time + 1e-9) : step === 'next' ? all.find((c) => c.timeline_start >= time - 1e-9) : undefined;
	if (pick) return { ok: true, to: { id: pick.id, extend: false } };
	const lane = step === 'below' ? [...drawn].reverse().find((t) => t.clips.length > 0) : step === 'above' ? drawn.find((t) => t.clips.length > 0) : undefined;
	const clip = lane ? nearestClip(lane, time) : undefined;
	return clip ? { ok: true, to: { id: clip.id, extend: false } } : { ok: false, reason: 'Select a clip first — there is nothing at the playhead' };
}

/** What *Select the clip at the playhead* says when the playhead is over nothing. */
export const NOTHING_AT_PLAYHEAD = 'There is no clip at the playhead';
