/* What a mixer slider does with a pointer and a key — pure, so the Fader and the
 * Pan share one tested behaviour and the component only wires events.
 *
 * Positions are `0..1` along the slider's travel: bottom to top for a vertical one,
 * left to right for a horizontal one. What a position *means* (dB, a pan) is the
 * caller's: this knows geometry and keys, not units. */

/** The slider's extent along its axis, in px. */
export interface Extent {
	start: number;
	size: number;
}

/** A position `0..1` from where the pointer is. `grab` is how far the pointer was from
 *  the thumb's centre when it took hold (px, along the travel's direction of increase),
 *  so a drag moves the thumb by the distance the pointer moves instead of snapping its
 *  centre under it. */
export function positionAt(pointer: number, extent: Extent, vertical: boolean, grab = 0): number {
	if (!(extent.size > 0)) return 0;
	const along = vertical ? extent.start + extent.size - pointer : pointer - extent.start;
	return Math.min(1, Math.max(0, (along + grab) / extent.size));
}

/** How far along the travel (px, in the direction of increase) the thumb's centre at
 *  `pos` is from the pointer — what `positionAt` is given back as `grab`. */
export function grabOffset(pointer: number, extent: Extent, vertical: boolean, pos: number): number {
	const along = vertical ? extent.start + extent.size - pointer : pointer - extent.start;
	return extent.size * pos - along;
}

/** How big a step a key asks for. */
export type KeySize = 'fine' | 'normal' | 'coarse' | 'page';

/** What a key does to a slider. */
export type SliderKey = { kind: 'step'; dir: 1 | -1; size: KeySize } | { kind: 'edge'; to: 0 | 1 };

/** The slider action for a key, or `null` for a key that is not the slider's. Arrow
 *  keys step (up / right is more), Shift is a coarse step and Alt a fine one, Page
 *  keys take the largest step, Home and End go to the ends — the WAI-ARIA slider
 *  pattern. */
export function sliderKey(key: string, mods: { shift?: boolean; alt?: boolean } = {}): SliderKey | null {
	const size: KeySize = mods.alt ? 'fine' : mods.shift ? 'coarse' : 'normal';
	switch (key) {
		case 'ArrowUp':
		case 'ArrowRight':
			return { kind: 'step', dir: 1, size };
		case 'ArrowDown':
		case 'ArrowLeft':
			return { kind: 'step', dir: -1, size };
		case 'PageUp':
			return { kind: 'step', dir: 1, size: 'page' };
		case 'PageDown':
			return { kind: 'step', dir: -1, size: 'page' };
		case 'Home':
			return { kind: 'edge', to: 0 };
		case 'End':
			return { kind: 'edge', to: 1 };
		default:
			return null;
	}
}

/** Whether a gesture changed anything worth an edit: positions closer than this are the
 *  same (a click on the thumb, a wobble, a drag that came back). */
export const MOVED_EPSILON = 1e-4;

export const moved = (from: number, to: number): boolean => Math.abs(to - from) > MOVED_EPSILON;

/** How long after the last key press a run of presses becomes one edit, ms. */
export const KEY_COMMIT_MS = 450;
