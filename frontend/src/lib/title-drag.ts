/* A title or a marker dragged along the timeline — pure. A press is not yet a drag: nothing about the
 * thing changes until the pointer has travelled `slopPx`, and judged on the pointer's travel, never on where
 * the result lands — a magnet within reach pulls a result several pixels off a press that did not move, which
 * is how a plain click used to re-time a title. What the pointer holds is the offset it grabbed at, so a press
 * a few pixels inside an edge does not make the edge jump to the pointer. */

import { quantizeTime } from './frames';
import { snapSpanStart, snapTime, trimSpan } from './titles';

/** A title gesture as pressed: the span it began as, which part was grabbed and how far from that part's edge. */
export interface TitleGrab {
	mode: 'move' | 'l' | 'r';
	origStart: number;
	origEnd: number;
	/** The pointer's time at the press minus the grabbed time: the title's start for a move or a left edge, its end for a right edge. */
	grabSec: number;
	/** Where the pointer went down, px. */
	downX: number;
	/** The pointer has travelled past the slop at some point (it stays so). */
	moved: boolean;
}

export interface TitleStepInput {
	/** The pointer, px. */
	clientX: number;
	/** The pointer's time on the lane, seconds. */
	time: number;
	slopPx: number;
	/** The magnets are on. */
	snap: boolean;
	/** What an edge snaps to. */
	points: number[];
	/** Snap reach, seconds. */
	threshold: number;
}

/** The span the gesture would give the title now, and whether it has become a drag. Under the slop it is the
 *  span the title began as, whatever the snapping would say. */
export function titleDragStep(d: TitleGrab, i: TitleStepInput): { start: number; end: number; moved: boolean } {
	const moved = d.moved || Math.abs(i.clientX - d.downX) >= i.slopPx;
	if (!moved) return { start: d.origStart, end: d.origEnd, moved: false };
	const at = i.time - d.grabSec;
	if (d.mode === 'move') {
		const dur = d.origEnd - d.origStart;
		const start = i.snap ? snapSpanStart(at, dur, i.points, i.threshold) : Math.max(0, at);
		return { start, end: start + dur, moved: true };
	}
	const to = i.snap ? snapTime(at, i.points, i.threshold) : at;
	return { ...trimSpan(d.origStart, d.origEnd, d.mode, to), moved: true };
}

/** A marker gesture as pressed. */
export interface MarkerGrab {
	origTime: number;
	/** The pointer's time at the press minus the marker's. */
	grabSec: number;
	downX: number;
	moved: boolean;
}

export interface MarkerStepInput {
	clientX: number;
	/** The pointer's time on the ruler, seconds. */
	time: number;
	slopPx: number;
	/** The cut's frame rate. */
	fps: number;
}

/** Where the marker stands now: where it was until the pointer has travelled past the slop, then on the frame
 *  nearest the pointer (less the offset it was grabbed at), never before 0. */
export function markerDragStep(d: MarkerGrab, i: MarkerStepInput): { time: number; moved: boolean } {
	const moved = d.moved || Math.abs(i.clientX - d.downX) >= i.slopPx;
	if (!moved) return { time: d.origTime, moved: false };
	return { time: Math.max(0, quantizeTime(i.time - d.grabSec, { fps: i.fps })), moved: true };
}
