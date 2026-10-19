/* Which clips of a lane the timeline puts in the page, and how (pure, bun-tested).
 *
 * A clip is a button with a filmstrip or a waveform, its badges and its handles; an hour of
 * interview cut into a few hundred pieces is a few thousand elements, and every one is laid
 * out and restyled on every zoom, scroll and selection change. So a lane of `CULL_MIN_CLIPS` or
 * more draws only the clips whose box meets the visible window — the quantized, overscanned
 * `visibleLaneRange` the waveforms already draw from, so a scroll tick inside a quantum changes
 * nothing — and when more than `BLOCK_MIN_CLIPS` of them are in view at once (zoomed out over a
 * long cut, each a few pixels wide: no label, no thumbnail, no waveform to read) it draws those
 * as blocks on one canvas (`LaneBlocks.svelte`) instead of as buttons. Whatever a gesture, the
 * keyboard or the pointer holds is a button in any case — a selected clip (which every drag,
 * trim and nudge starts from), the one under the pointer (so a press lands on a real clip: the
 * canvas promotes it on hover), the one with focus, and the clips a gesture is moving — so
 * nothing it holds leaves the page under it. A shorter lane is drawn whole, as it always was. */

/** Below this many clips a lane is drawn whole. */
export const CULL_MIN_CLIPS = 48;

/** Above this many clips in view a lane draws them as blocks on one canvas. */
export const BLOCK_MIN_CLIPS = 80;

/** The minimum width a clip is drawn at, px (the lane's own `Math.max(6, …)`). */
export const MIN_CLIP_PX = 6;

export interface CullClip {
	id: string;
	timeline_start: number;
}

/** How a lane draws its clips: `buttons` as clips, `blocks` on one canvas; both in the lane's order. */
export interface LaneDrawing<T> {
	buttons: readonly T[];
	blocks: readonly T[];
}

/** The box of a clip in the lane, px: from `timeline_start * pxPerSec`, at least 6 px wide. */
export function clipBox(c: CullClip, duration: number, pxPerSec: number): { left: number; right: number } {
	const left = c.timeline_start * pxPerSec;
	return { left, right: left + Math.max(MIN_CLIP_PX, duration * pxPerSec) };
}

/** How a lane draws `clips` with `[lo, hi)` px of it in view: everything as buttons when there are
 *  fewer than `min`; else the clips whose box meets the window, as buttons up to `blocks` of them
 *  and as canvas blocks past that; and every clip `keep` holds as a button wherever it is. */
export function laneDrawing<T extends CullClip>(
	clips: readonly T[],
	duration: (c: T) => number,
	pxPerSec: number,
	lo: number,
	hi: number,
	keep: (id: string) => boolean,
	min = CULL_MIN_CLIPS,
	blocks = BLOCK_MIN_CLIPS
): LaneDrawing<T> {
	if (clips.length < min || !(pxPerSec > 0)) return { buttons: clips, blocks: [] };
	const meets = (c: T) => {
		const box = clipBox(c, duration(c), pxPerSec);
		return box.right > lo && box.left < hi;
	};
	const inView = clips.filter(meets);
	if (inView.length <= blocks) return { buttons: clips.filter((c) => keep(c.id) || meets(c)), blocks: [] };
	return { buttons: clips.filter((c) => keep(c.id)), blocks: inView.filter((c) => !keep(c.id)) };
}

/** The block under lane x `x` (px), the topmost — the last in lane order — when blocks overlap. */
export function blockAt<T extends CullClip>(
	blocks: readonly T[],
	duration: (c: T) => number,
	pxPerSec: number,
	x: number
): T | null {
	for (let i = blocks.length - 1; i >= 0; i--) {
		const box = clipBox(blocks[i], duration(blocks[i]), pxPerSec);
		if (x >= box.left && x < box.right) return blocks[i];
	}
	return null;
}
