/* The ruler's detected-scene-cut marks — pure. A mark is a point in time, so it is keyed and
 * culled by seconds, never by the pixel it happens to sit at: two clips of one asset laid at the
 * same alignment put the same cut at the same place (one mark, not two keys that collide) and a
 * zoom moves a mark without making it a different one. */

import { sourceToTimeline } from './beats';
import type { Clip, Timeline } from './types';

/** Cuts closer than this, seconds, are one mark (the beat grid's rule, `beats.ts`). */
export const SCENE_MARK_EPS = 0.005;

/**
 * The scene cuts of the video tracks' clips in timeline seconds, ascending, de-duplicated, and
 * only those inside `window` (`[from, to]`, seconds). A cut at a clip's very edge belongs to
 * neither side, as the clip's own cut is not inside it.
 */
export function sceneTimes(
	timeline: Timeline,
	scenesFor: (assetId: string) => readonly number[] | null | undefined,
	window: readonly [number, number]
): number[] {
	const [from, to] = window;
	const times: number[] = [];
	for (const track of timeline.tracks) {
		if (track.kind !== 'video') continue;
		for (const c of track.clips as Clip[]) {
			const scenes = scenesFor(c.asset_id);
			if (!scenes) continue;
			for (const sc of scenes) {
				if (!(sc > c.source_in && sc < c.source_out)) continue;
				const t = sourceToTimeline(c, sc);
				if (t >= from && t <= to) times.push(t);
			}
		}
	}
	times.sort((a, b) => a - b);
	return times.filter((t, i) => i === 0 || t - times[i - 1] > SCENE_MARK_EPS);
}
