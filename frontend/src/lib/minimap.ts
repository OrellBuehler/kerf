/* The minimap's geometry — pure. The strip shows the whole cut, left to right, as
 * `span` seconds across `width` px, and a rectangle on it is the part of the cut
 * the timeline is showing. The rectangle and the timeline's view are one thing
 * seen two ways, and this module is the translation both ways:
 *
 *  - **view -> rectangle** (`windowRect`): the timeline's `scrollLeft`, visible
 *    width and zoom (px/s) become the seconds on screen, then a rectangle on the
 *    strip. It is held inside the strip and never thinner than `MIN_WINDOW_PX`
 *    (a zoomed-out hour of cut would otherwise be an invisible sliver).
 *  - **rectangle -> view** (`targetForRect`, and the three gestures built on it):
 *    a rectangle's edges are two times, the width they enclose is what has to be
 *    visible, so the zoom is `viewW / (t1 - t0)` (held inside the zoom range) and
 *    the scroll puts `t0` at the left edge. *Moving* the rectangle keeps the zoom
 *    exactly (`moveTo`) — deriving it back from a rectangle that was widened to
 *    its minimum, or clamped to the strip, would change it. Dragging an edge
 *    anchors the *other* edge, and re-derives the dragged one after the zoom was
 *    clamped, so the anchor really stays where it was.
 *
 * Every gesture is absolute — a function of the pointer's position on the strip
 * and the view when the gesture began — so nothing accumulates over a drag.
 *
 * Also here: the clips as blocks per track (`trackBlocks`, merging the runs too
 * fine to tell apart so the strip holds at most a block per two pixels however
 * many clips the cut has), the rows the tracks stack into (`rowLayout`), and the
 * hit-testing that decides whether a press is on an edge, on the body, or on the
 * bare strip. */

import { MIN_SPAN_SEC, clampZoom, zoomCeiling } from './zoom';

/** The rectangle is never thinner than this, px. */
export const MIN_WINDOW_PX = 8;
/** The grab zone of each edge of the rectangle, px (less on a thin rectangle). */
export const EDGE_PX = 6;
/** How far outside the rectangle an edge can still be grabbed, px. */
export const EDGE_REACH_PX = 3;

/** What the strip is a map of. */
export interface MapGeo {
	/** Seconds across the whole strip. */
	span: number;
	/** The strip's width, px. */
	width: number;
}

/** The seconds the strip spans for a cut `duration` long: the cut, but never less
 *  than the timeline lays out for an empty one. */
export const mapSpan = (duration: number): number =>
	Math.max(Number.isFinite(duration) ? duration : 0, MIN_SPAN_SEC);

export const timeToX = (t: number, g: MapGeo): number => (t / g.span) * g.width;
export const xToTime = (x: number, g: MapGeo): number => (x / Math.max(g.width, 1e-6)) * g.span;

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** The timeline's view: what the scroller shows. */
export interface View {
	scrollLeft: number;
	/** The visible lane width, px (the scroller less its sticky header). */
	viewW: number;
	/** Zoom, px per second. */
	pxPerSec: number;
}

/** Where the view is a target to move it to. */
export interface Target {
	scrollLeft: number;
	zoom: number;
}

/** The rectangle on the strip, px. */
export interface WindowRect {
	x: number;
	w: number;
}

/** The seconds the view shows: `[from, to]`. */
export function viewSpan(v: View): { from: number; to: number } {
	const pps = Math.max(v.pxPerSec, 1e-6);
	return { from: Math.max(0, v.scrollLeft) / pps, to: (Math.max(0, v.scrollLeft) + Math.max(v.viewW, 0)) / pps };
}

/** The rectangle that stands for the view. */
export function windowRect(v: View, g: MapGeo): WindowRect {
	const { from, to } = viewSpan(v);
	const x0 = clamp(timeToX(from, g), 0, g.width);
	const x1 = clamp(timeToX(to, g), 0, g.width);
	const w = Math.min(g.width, Math.max(MIN_WINDOW_PX, x1 - x0));
	return { x: Math.min(x0, Math.max(0, g.width - w)), w };
}

/** The view a rectangle stands for: the zoom that makes the enclosed seconds fill
 *  `viewW`, held inside the zoom range for a cut `duration` long, and the scroll
 *  that — for `anchor: 'left'` — keeps the rectangle's left edge on its time, or,
 *  for `'right'`, the right edge (after the zoom was clamped, the other edge is
 *  what gives). */
export function targetForRect(
	rect: WindowRect,
	viewW: number,
	g: MapGeo,
	duration: number,
	anchor: 'left' | 'right' = 'left'
): Target {
	const t0 = xToTime(rect.x, g);
	const t1 = xToTime(rect.x + rect.w, g);
	const zoom = clampZoom(Math.max(viewW, 1) / Math.max(t1 - t0, 1e-6), duration);
	if (anchor === 'right') return { zoom, scrollLeft: Math.max(0, t1 * zoom - viewW) };
	return { zoom, scrollLeft: Math.max(0, t0 * zoom) };
}

/** The shortest the rectangle may be, seconds: the most the zoom range lets the
 *  view show at once, so no drag can ask for a zoom past its ceiling. */
export const minWindowSeconds = (viewW: number, duration: number): number =>
	Math.max(viewW, 1) / zoomCeiling(duration);

/** The latest the view's left edge can be, seconds, with the zoom as it is: the
 *  view's right edge at the end of the strip (or at 0 when it is wider than that). */
function maxStart(v: View, g: MapGeo): number {
	const { from, to } = viewSpan(v);
	return Math.max(0, g.span - (to - from));
}

/** Drag the body: the rectangle's left edge goes to `left` px; the zoom is kept. */
export function moveTo(v: View, g: MapGeo, left: number): Target {
	const t0 = clamp(xToTime(left, g), 0, maxStart(v, g));
	return { zoom: v.pxPerSec, scrollLeft: t0 * v.pxPerSec };
}

/** A press on the bare strip: centre the view on `x`; the zoom is kept. */
export function centerOn(v: View, g: MapGeo, x: number): Target {
	const { from, to } = viewSpan(v);
	const t0 = clamp(xToTime(x, g) - (to - from) / 2, 0, maxStart(v, g));
	return { zoom: v.pxPerSec, scrollLeft: t0 * v.pxPerSec };
}

/** Drag the left edge to `x` px: the right edge keeps its time, the zoom follows. */
export function resizeLeft(v: View, g: MapGeo, x: number, duration: number): Target {
	const { to } = viewSpan(v);
	const t1 = Math.min(to, g.span); // a view hanging past the end of the cut is anchored at the end
	const t0 = clamp(xToTime(x, g), 0, Math.max(0, t1 - minWindowSeconds(v.viewW, duration)));
	return targetForRect({ x: timeToX(t0, g), w: timeToX(t1 - t0, g) }, v.viewW, g, duration, 'right');
}

/** Drag the right edge to `x` px: the left edge keeps its time, the zoom follows. */
export function resizeRight(v: View, g: MapGeo, x: number, duration: number): Target {
	const { from } = viewSpan(v);
	const t1 = clamp(xToTime(x, g), from + minWindowSeconds(v.viewW, duration), Math.max(g.span, from + 1e-3));
	return targetForRect({ x: timeToX(from, g), w: timeToX(t1 - from, g) }, v.viewW, g, duration, 'left');
}

// ---- hit testing ---------------------------------------------------------------

export type Hit = 'left' | 'right' | 'body' | 'outside';

/** What a press at strip position `x` lands on. The edges win over the body where
 *  they meet, and a thin rectangle keeps a body to drag (the edge zone is at most
 *  a third of it). */
export function hitTest(x: number, r: WindowRect): Hit {
	const edge = Math.min(EDGE_PX, r.w / 3);
	if (x >= r.x - EDGE_REACH_PX && x <= r.x + edge) return 'left';
	if (x >= r.x + r.w - edge && x <= r.x + r.w + EDGE_REACH_PX) return 'right';
	if (x > r.x && x < r.x + r.w) return 'body';
	return 'outside';
}

// ---- the cut itself ------------------------------------------------------------

/** A clip's span on the timeline, seconds. */
export interface MapClip {
	start: number;
	end: number;
}

/** A run of clips as the strip draws it: px from its left, and how many clips it
 *  stands for (more than one when they were too fine to tell apart). */
export interface Block {
	x: number;
	w: number;
	clips: number;
}

/** A clip is drawn at least this wide, px: a one-second clip in an hour of cut is
 *  still a mark. */
export const MIN_BLOCK_PX = 1;
/** Neighbours closer than this, px, are one block when either is thinner than
 *  `MERGE_BELOW_PX`. */
export const MERGE_GAP_PX = 1;
export const MERGE_BELOW_PX = 3;

/**
 * One track's clips as blocks, left to right. Clips wide enough to tell apart stay
 * their own blocks (a cut between two shots is a gap the renderer can draw); a
 * clip thinner than `MERGE_BELOW_PX` joins its neighbour when the gap between
 * them is under `MERGE_GAP_PX`, so a cut of thousands of clips is a few hundred
 * blocks, bounded by the strip's width and not by the project's.
 */
export function trackBlocks(clips: readonly MapClip[], g: MapGeo): Block[] {
	const sorted = [...clips].sort((a, b) => a.start - b.start);
	const out: Block[] = [];
	for (const c of sorted) {
		const x0 = clamp(timeToX(c.start, g), 0, g.width);
		const x1 = clamp(timeToX(c.end, g), 0, g.width);
		if (x1 < x0) continue;
		const w = Math.max(MIN_BLOCK_PX, x1 - x0);
		const last = out[out.length - 1];
		if (last) {
			const gap = x0 - (last.x + last.w);
			if (gap < MERGE_GAP_PX && (w < MERGE_BELOW_PX || last.w < MERGE_BELOW_PX)) {
				const right = Math.max(last.x + last.w, x0 + w);
				last.w = right - last.x;
				last.clips++;
				continue;
			}
		}
		out.push({ x: x0, w, clips: 1 });
	}
	return out;
}

/** Where each track's row sits on a strip `heightPx` tall: top to bottom in track
 *  order, each as tall as the strip leaves it (at most `maxRow`). */
export function rowLayout(
	trackCount: number,
	heightPx: number,
	o: { pad?: number; gap?: number; maxRow?: number } = {}
): { y: number; h: number }[] {
	const { pad = 4, gap = 2, maxRow = 12 } = o;
	if (trackCount <= 0) return [];
	const room = Math.max(0, heightPx - pad * 2 - gap * (trackCount - 1));
	const h = clamp(room / trackCount, 1, maxRow);
	return Array.from({ length: trackCount }, (_, i) => ({ y: pad + i * (h + gap), h }));
}
