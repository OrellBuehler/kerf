/* Marquee hit-testing — pure. The rectangle a drag on empty timeline space draws
 * is in *lane space*: x is `seconds * pxPerSec`, y is the pixel offset down the
 * lanes' container. A clip is touched when that rectangle meets the clip's own box
 * — the same box the timeline draws (a body inset from its lane, never narrower
 * than `MIN_CLIP_PX`) — so what is highlighted under the rectangle is what gets
 * selected. The component measures the lanes (their heights are CSS), this decides
 * what is under the rectangle. */

/** A rectangle by two opposite corners — `x0`/`y0` is where the drag began, so
 *  they are not ordered; `normalizeRect` orders them. */
export interface Rect {
	x0: number;
	y0: number;
	x1: number;
	y1: number;
}

/** One track's lane: where it sits in the lane container (px from its top), how
 *  tall it is, and whether a clip on it may be selected by a marquee. */
export interface LaneBox {
	trackId: string;
	top: number;
	height: number;
	/** A locked track's clips are not swept up: locking guards the edit, and a
	 *  selection is the thing every edit acts on. They stay clickable. */
	locked?: boolean;
}

/** A clip's span on its track, seconds. */
export interface SpanClip {
	id: string;
	start: number;
	end: number;
}

/** The gap between a lane's edge and the clip drawn in it, px (top and bottom). */
export const CLIP_INSET_PX = 5;
/** A clip is drawn at least this wide, px, so a sliver is still grabbable. */
export const MIN_CLIP_PX = 6;

/** `r` with its corners ordered: `x0 <= x1` and `y0 <= y1`. */
export function normalizeRect(r: Rect): Rect {
	return {
		x0: Math.min(r.x0, r.x1),
		x1: Math.max(r.x0, r.x1),
		y0: Math.min(r.y0, r.y1),
		y1: Math.max(r.y0, r.y1)
	};
}

/** Whether two ordered rectangles share any area — touching edges count, so a
 *  marquee that just grazes a clip has touched it. */
export function rectsTouch(a: Rect, b: Rect): boolean {
	return a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1;
}

/**
 * The ids of the clips `rect` touches, in time order (ties by lane order).
 * `clips` maps a track id to its clips; a lane with no entry has none. Clips on a
 * locked lane are skipped.
 */
export function marqueeHits(
	rect: Rect,
	lanes: readonly LaneBox[],
	clips: ReadonlyMap<string, readonly SpanClip[]>,
	pxPerSec: number
): string[] {
	const r = normalizeRect(rect);
	const hits: { id: string; start: number; lane: number }[] = [];
	lanes.forEach((lane, li) => {
		if (lane.locked) return;
		const y0 = lane.top + CLIP_INSET_PX;
		const y1 = lane.top + lane.height - CLIP_INSET_PX;
		if (y1 < y0) return;
		for (const c of clips.get(lane.trackId) ?? []) {
			const x0 = c.start * pxPerSec;
			const x1 = x0 + Math.max(MIN_CLIP_PX, (c.end - c.start) * pxPerSec);
			if (rectsTouch(r, { x0, x1, y0, y1 })) hits.push({ id: c.id, start: c.start, lane: li });
		}
	});
	hits.sort((a, b) => a.start - b.start || a.lane - b.lane);
	return hits.map((h) => h.id);
}
