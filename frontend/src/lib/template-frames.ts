// A motion template's animation as a short run of pictures, for the Inspector's preview of a template in the library:
// the engine draws the title at each time (`templateFrames`, one call, the text set as outlines once) and the page
// flips through them. Kept for the few templates looked at last, so going back to one starts at once.

import { onTemplatesChanged, templateFrames } from './api';

/** Pictures per second of the preview, and the most one template gets (the engine's cap per call). */
export const PREVIEW_FPS = 12;
export const MAX_FRAMES = 64;
const KEPT = 4;

/** The times the preview draws: evenly over the template, each at the middle of its step. */
export function frameTimes(duration: number, fps = PREVIEW_FPS, max = MAX_FRAMES): number[] {
	if (!(duration > 0)) return [0];
	const n = Math.max(1, Math.min(max, Math.round(duration * fps)));
	return Array.from({ length: n }, (_, i) => ((i + 0.5) * duration) / n);
}

/** The picture shown at `time`: the one whose step holds it. */
export function frameAt(time: number, duration: number, frames: number): number {
	if (frames <= 0 || !(duration > 0)) return 0;
	return Math.min(frames - 1, Math.max(0, Math.floor((time / duration) * frames)));
}

const cache = new Map<string, Promise<string[]>>();
let watching = false;

/** A save, an import or a delete may have replaced what an id holds: the pictures are drawn again. */
function watch() {
	if (watching) return;
	watching = true;
	onTemplatesChanged(() => cache.clear()).catch(() => (watching = false));
}

export function framesOf(id: string, duration: number, width = 480): Promise<string[]> {
	watch();
	const kept = cache.get(id);
	if (kept) {
		cache.delete(id);
		cache.set(id, kept);
		return kept;
	}
	const frames = templateFrames(id, frameTimes(duration), width);
	frames.catch(() => cache.delete(id));
	cache.set(id, frames);
	while (cache.size > KEPT) cache.delete(cache.keys().next().value as string);
	return frames;
}

