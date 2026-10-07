/* Painting a clip's thumbnails onto a canvas, from the slots `filmstrip-view.ts`
 * lays out: each slot is one blit of a thumbnail's rectangle on its decoded sheet
 * into the slot's rectangle on the canvas. No colour literals — the picture is
 * the footage's own, and a slot with nothing to draw is left clear so the clip's
 * own colour shows through. */

import type { SlotDraw, StripShape } from './filmstrip-view';

/** The subset of `CanvasRenderingContext2D` the painting uses — so a test can hand
 *  in a recorder. */
export interface FilmCtx {
	imageSmoothingEnabled: boolean;
	imageSmoothingQuality: 'low' | 'medium' | 'high';
	clearRect(x: number, y: number, w: number, h: number): void;
	drawImage(
		image: CanvasImageSource,
		sx: number,
		sy: number,
		sw: number,
		sh: number,
		dx: number,
		dy: number,
		dw: number,
		dh: number
	): void;
}

export interface FilmDrawOptions {
	/** Canvas size, device pixels. */
	width: number;
	height: number;
	strip: Pick<StripShape, 'frame_width' | 'frame_height'>;
	/** The decoded sheet at an index, or `undefined` when it is not there (it is
	 *  then skipped, not an error). */
	sheet: (index: number) => CanvasImageSource | undefined;
}

/** Clear the canvas and blit every slot. Returns how many slots were drawn. */
export function drawFilmstrip(ctx: FilmCtx, slots: readonly SlotDraw[], o: FilmDrawOptions): number {
	ctx.clearRect(0, 0, o.width, o.height);
	// Thumbnails are drawn smaller than they are stored; the default (low) quality
	// aliases a downscale into a shimmer when the zoom changes.
	ctx.imageSmoothingEnabled = true;
	ctx.imageSmoothingQuality = 'high';
	let drawn = 0;
	for (const s of slots) {
		if (s.dw <= 0 || s.dx >= o.width || s.dx + s.dw <= 0) continue; // wholly off the canvas
		const image = o.sheet(s.sheet);
		if (!image) continue;
		ctx.drawImage(image, s.sx, 0, o.strip.frame_width, o.strip.frame_height, s.dx, 0, s.dw, o.height);
		drawn++;
	}
	return drawn;
}
