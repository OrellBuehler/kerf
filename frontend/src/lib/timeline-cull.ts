/* Which clips of a lane the timeline puts in the page, and how (pure, bun-tested).
 *
 * A clip is a button with a filmstrip or a waveform, its badges and its handles; an hour of
 * interview cut into a few hundred pieces is a few thousand elements, and every one is laid
 * out and restyled on every zoom, scroll and selection change. So a lane of `CULL_MIN_CLIPS` or
 * more draws only the clips whose box meets the visible window — the quantized, overscanned
 * `visibleLaneRange` the waveforms already draw from, so a scroll tick inside a quantum changes
 * nothing — and of those, a clip narrower than `BLOCK_MAX_PX` (no label, no thumbnail, no
 * waveform to read on it) is a block on one canvas (`LaneBlocks.svelte`) instead of a button,
 * which draws its selection, proposal and partner marks itself (`blockMark`). A clip wide enough
 * to read stays a button however many are in view.
 *
 * What the pointer or the keyboard holds is a button wherever it is (`pinnedClips`): the pressed
 * clip (whose button holds the pointer capture), the one under the pointer (so a press lands on a
 * real clip: the canvas promotes it on hover), the one with focus and the one being nudged. What
 * a gesture only *moves* — a selection dragged along, the clips a ripple trim shifts — is drawn
 * from the model (the ghosts), so it is not kept off screen: a select-all over a long cut mounts
 * nothing more. A shorter lane is drawn whole, as it always was. */

/** Below this many clips a lane is drawn whole. */
export const CULL_MIN_CLIPS = 48;

/** Narrower than this (px of footage) a clip in view is a block on the lane's canvas. */
export const BLOCK_MAX_PX = 12;

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

/** The clips the pointer or the keyboard holds, which are buttons wherever they are: the pressed
 *  ones (a drag's or a trim's clip, both sides of a rolled cut), the hovered, the focused and the
 *  nudged one. */
export function pinnedClips(held: {
	pressed?: readonly (string | null | undefined)[];
	hovered?: string | null;
	focused?: string | null;
	nudged?: string | null;
}): ReadonlySet<string> {
	const out = new Set<string>();
	for (const id of [...(held.pressed ?? []), held.hovered, held.focused, held.nudged]) if (id) out.add(id);
	return out;
}

/** How a lane draws `clips` with `[lo, hi)` px of it in view: everything as buttons when there are
 *  fewer than `min`; else the clips whose box meets the window, narrower than `blockPx` as canvas
 *  blocks and the rest as buttons; and every clip in `pinned` as a button wherever it is. */
export function laneDrawing<T extends CullClip>(
	clips: readonly T[],
	duration: (c: T) => number,
	pxPerSec: number,
	lo: number,
	hi: number,
	pinned: ReadonlySet<string>,
	min = CULL_MIN_CLIPS,
	blockPx = BLOCK_MAX_PX
): LaneDrawing<T> {
	if (clips.length < min || !(pxPerSec > 0)) return { buttons: clips, blocks: [] };
	const buttons: T[] = [];
	const blocks: T[] = [];
	for (const c of clips) {
		if (pinned.has(c.id)) {
			buttons.push(c);
			continue;
		}
		const d = duration(c);
		const box = clipBox(c, d, pxPerSec);
		if (box.right <= lo || box.left >= hi) continue;
		(d * pxPerSec < blockPx ? blocks : buttons).push(c);
	}
	return { buttons, blocks };
}

/** What a block shows of its clip's state, as its button would (the strongest first). */
export type BlockMark = 'selected' | 'added' | 'shifted' | 'touched' | 'lit' | null;

/** A block's mark: selected (the ring), a staged proposal's add or shift, a clip the proposal
 *  touches, or a partner of the clip under the pointer. */
export function blockMark(
	id: string,
	state: {
		selected: ReadonlySet<string>;
		diff?: ReadonlyMap<string, string> | null;
		touched?: ReadonlySet<string> | null;
		lit: ReadonlySet<string>;
	}
): BlockMark {
	if (state.selected.has(id)) return 'selected';
	const d = state.diff?.get(id);
	if (d) return d === 'shifted' ? 'shifted' : 'added';
	if (state.touched?.has(id)) return 'touched';
	if (state.lit.has(id)) return 'lit';
	return null;
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

/** Whether a press on a lane landed on its empty surface: the lane itself, or its block canvas
 *  where no block is (`blockUnder`), which is as empty as the lane under it. */
export function pressOnEmpty(target: EventTarget | null, lane: EventTarget | null, blockUnder: () => boolean): boolean {
	if (target === lane) return true;
	const el = target as { hasAttribute?: (name: string) => boolean } | null;
	return typeof el?.hasAttribute === 'function' && el.hasAttribute('data-lane-blocks') && !blockUnder();
}

/** Whether a pointer move over the canvas may promote the block under it: not while a button is
 *  down or a gesture is under way (the clip it would mount is not what the gesture holds). */
export const mayPromote = (buttons: number, gesture: boolean): boolean => buttons === 0 && !gesture;

/** The horizontal padding of a clip's button, px: a box narrower than its padding and border
 *  would grow past `clipBox` (and past the block it promotes), so a narrow clip has none. */
export const clipPadding = (width: number): number => (width >= 2 * 7 + 6 ? 7 : 0);
