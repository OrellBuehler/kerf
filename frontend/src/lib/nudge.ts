/* Moving a clip with the arrow keys — pure. A clip that has the keyboard focus takes ← and → as a
 * nudge: one frame, or with Shift one second (the steps the playhead's own keys take). A run of
 * presses is ONE edit, written when the keys go quiet, so holding the key or tapping it ten times
 * is one undo step and does not race a series of writes against a timeline that has not come back
 * yet; until then the clip is drawn where it will land, as a drag draws it. */

import { frameIndex, frameTime, quantizeSpanStart } from './frames';
import { drawnMoveTracks, planMove, type MovePlan } from './multi-move';
import type { Timeline } from './types';
import { clipDuration } from './types';

/** How long the keys stay quiet before the run is written, ms. */
export const NUDGE_QUIET_MS = 450;

/** The nudge a key asks for, in frames of the cut (a second is the rate's nominal frame count, as
 *  the timecode counts it), or null when the key is not one. */
export function nudgeFrames(key: string, shift: boolean, fps: number): number | null {
	const dir = key === 'ArrowLeft' ? -1 : key === 'ArrowRight' ? 1 : 0;
	if (dir === 0) return null;
	return dir * (shift ? Math.max(1, Math.round(Number.isFinite(fps) && fps > 0 ? fps : 30)) : 1);
}

/**
 * Where a clip that started at `origStart` (and is `dur` long) stands after `frames` frames of
 * nudge: the frame it is on, plus the run — so a clip that sits between frames goes to the
 * neighbouring one — landing on a neighbour's edge exactly when it comes within a hair of it
 * (`edges`, the other clips of its lane), and never before 0.
 */
export function nudgedStart(origStart: number, dur: number, frames: number, edges: readonly number[], fps: number): number {
	const to = frameTime(frameIndex(origStart, fps) + frames, fps);
	return quantizeSpanStart(to, dur, { fps, welds: edges });
}

/** A run of nudges as the timeline holds it: the clip that has the keyboard, the clips that go with
 *  it (the selection it is part of), where they stood and how many frames the keys have asked for. */
export interface NudgeRun {
	clipId: string;
	trackId: string;
	members: ReadonlySet<string>;
	origStart: number;
	dur: number;
	frames: number;
	/** Alt was down: the clip alone, its linked partners left where they are. */
	alone: boolean;
}

/** The other clips' edges on a lane — the neighbours a nudge welds to. */
export function laneEdges(timeline: Timeline, trackId: string, except: ReadonlySet<string>): number[] {
	const out: number[] = [];
	for (const c of timeline.tracks.find((t) => t.id === trackId)?.clips ?? []) {
		if (!except.has(c.id)) out.push(c.timeline_start, c.timeline_start + clipDuration(c));
	}
	return out;
}

/**
 * What a run of nudges would do, from the run and the cut alone: where the grabbed clip stands (`at`)
 * and the drag's own plan for it (`planMove`: lanes, locks, links, overlaps), so the ghost and the one
 * `move_clips` the run is written as are the same. It reads no pointer: a press that began a drag
 * while the keys were still quiet must not be able to hand the run the drag's half-way position.
 */
export function nudgePlan(timeline: Timeline, run: NudgeRun, fps: number): { at: number; plan: MovePlan } {
	const at = nudgedStart(run.origStart, run.dur, run.frames, laneEdges(timeline, run.trackId, run.members), fps);
	const plan = planMove(drawnMoveTracks(timeline), run.members, run.clipId, at, run.trackId, { links: !run.alone });
	return { at, plan };
}
