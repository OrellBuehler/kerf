/* Moving a clip with the arrow keys — pure. A clip that has the keyboard focus takes ← and → as a
 * nudge: one frame, or with Shift one second (the steps the playhead's own keys take). A run of
 * presses is ONE edit, written when the keys go quiet, so holding the key or tapping it ten times
 * is one undo step and does not race a series of writes against a timeline that has not come back
 * yet; until then the clip is drawn where it will land, as a drag draws it. */

import { frameIndex, frameTime, quantizeSpanStart } from './frames';

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
