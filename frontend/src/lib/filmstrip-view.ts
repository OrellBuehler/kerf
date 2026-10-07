/* The geometry of a clip's thumbnails: which thumbnail goes in which slot of the
 * clip, and where on its sheet that thumbnail is. Pure — `filmstrip-cache.ts`
 * owns the decoded sheets and `filmstrip-draw.ts` the painting, so this is what
 * the tests pin.
 *
 * A clip's picture is a row of *slots*, each as wide as one thumbnail is at the
 * clip's height (the strip's aspect times the lane height, whole pixels so the
 * slots abut exactly). The grid starts at the clip's own left edge, so scrolling
 * moves nothing on it. Each slot shows the frame at the source time under its
 * centre — read through the same mapping the waveform uses (`sourceAt`: trim,
 * speed, and a reversed clip counting down from `source_out`), so a reversed
 * clip shows its footage backwards rather than flipped — picked with the
 * backend's own lookups (`frameAt` / `locate`) from the strip's sheets.
 *
 * Like the waveform, only the part of a clip that is on screen is ever laid out:
 * a one-hour clip at 96 px/s is 345 600 px, and at a 96 px slot that is 3 600
 * slots no canvas holds. `slotRange` is the slots a canvas range touches. */

import { frameAt, locate, type SheetGeometry, type StripGeometry } from './filmstrip-geometry';
import { sourceAt, type ClipWindow } from './waveform-view';

/** A clip box shorter than this (css px) gets no thumbnails: below it the picture
 *  is a smear, and the clip's own label needs the room. */
export const MIN_FILM_HEIGHT = 28;

/** A slot is never narrower than this (css px), whatever the aspect: a degenerate
 *  strip must not turn into thousands of slivers. */
export const MIN_SLOT_PX = 8;

/** Whether a clip box `heightPx` tall shows thumbnails. */
export const filmVisible = (heightPx: number): boolean => Number.isFinite(heightPx) && heightPx >= MIN_FILM_HEIGHT;

/** The part of a strip's geometry the layout reads. */
export type StripShape = Pick<StripGeometry, 'interval' | 'frames' | 'frame_width' | 'frame_height'> & {
	sheets: SheetGeometry[];
};

/** Width (css px) of one slot of a clip `heightPx` tall: the thumbnail's aspect at
 *  that height, a whole number of pixels. */
export function slotWidth(strip: Pick<StripShape, 'frame_width' | 'frame_height'>, heightPx: number): number {
	const aspect = strip.frame_height > 0 ? strip.frame_width / strip.frame_height : 16 / 9;
	return Math.max(MIN_SLOT_PX, Math.round(aspect * Math.max(heightPx, 1)));
}

/** The slots `[first, last]` (inclusive) that clip-local css pixels `[x0, x1)`
 *  touch, or `null` when the range is empty. */
export function slotRange(x0: number, x1: number, slot: number): { first: number; last: number } | null {
	if (!(x1 > x0) || !(slot > 0)) return null;
	return { first: Math.max(0, Math.floor(x0 / slot)), last: Math.max(0, Math.ceil(x1 / slot) - 1) };
}

/**
 * The source second slot `i` shows: the time under the middle of its *visible*
 * part — a clip's last slot is usually cut by the clip's right edge, and the
 * middle of the part that is left is what a viewer sees — held inside the clip's
 * source window (a clip a few pixels wide must not read past its footage).
 */
export function slotTime(clip: ClipWindow, i: number, slot: number, clipWidth: number, pxPerSec: number): number {
	const left = i * slot;
	const right = Math.min(left + slot, Math.max(clipWidth, 0));
	const centre = Math.max(left, (left + right) / 2);
	const t = sourceAt(clip, Math.min(centre, Math.max(clipWidth, 0)), pxPerSec);
	const lo = Math.min(clip.source_in, clip.source_out);
	const hi = Math.max(clip.source_in, clip.source_out);
	return Math.min(hi, Math.max(lo, t));
}

/** Where one slot's thumbnail comes from and goes to. */
export interface SlotDraw {
	slot: number;
	/** The thumbnail on the strip. */
	frame: number;
	/** Index into the strip's `sheets`, and the thumbnail's x on that sheet, px. */
	sheet: number;
	sx: number;
	/** Destination on the canvas, device px. The rectangle may run past either
	 *  edge of the canvas (a slot the clip's edge or the canvas range cuts). */
	dx: number;
	dw: number;
}

export interface PlanArgs {
	strip: StripShape;
	clip: ClipWindow;
	pxPerSec: number;
	/** The clip's width, css px. */
	clipWidth: number;
	/** The clip box's height, css px. */
	heightPx: number;
	/** Clip-local css px of the canvas's left edge. */
	x0: number;
	/** Canvas width, device px. */
	columns: number;
	/** The ratio the canvas is drawn at (already capped). */
	scale: number;
}

/**
 * What to blit for a canvas covering clip-local `[x0, x0 + columns / scale)`: one
 * entry per slot it touches, left to right. Slot edges are rounded to device
 * pixels *as edges* (a slot ends where the next begins), so neighbours never
 * leave a seam or overlap by a pixel. A slot whose thumbnail the strip does not
 * hold (a real strip can come up short) is left out and shows the clip colour.
 */
export function planSlots(a: PlanArgs): SlotDraw[] {
	const slot = slotWidth(a.strip, a.heightPx);
	const range = slotRange(a.x0, a.x0 + a.columns / Math.max(a.scale, 1e-6), slot);
	if (!range) return [];
	const edge = (k: number) => Math.round((k * slot - a.x0) * a.scale);
	const out: SlotDraw[] = [];
	for (let i = range.first; i <= range.last; i++) {
		const frame = frameAt(a.strip, slotTime(a.clip, i, slot, a.clipWidth, a.pxPerSec));
		const found = locate(a.strip, frame);
		if (!found) continue;
		const dx = edge(i);
		out.push({ slot: i, frame, sheet: a.strip.sheets.indexOf(found.sheet), sx: found.x, dx, dw: edge(i + 1) - dx });
	}
	return out;
}
