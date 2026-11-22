/* What the export would duck, read off a timeline: which tracks dip, which key them, and in which mode (`duck.ts` has the
 * machine and the reasons). Pure; `audio.ts` wires its graph from it and the lane overlay draws from it. */

import { duckMode, type DuckMode } from './duck';
import { trackRenders } from './levels';
import { clipSounds } from './mixer-strips';
import type { Timeline } from './types';

/** What a timeline's ducking is, as the export sees it. */
export interface DuckPlan {
	/** Whether the mix is ducked at all: a track flagged Duck that sounds *and* something else that does to key it from
	 *  (the export leaves a graph with only one of the two as a flat sum). */
	active: boolean;
	mode: DuckMode;
	/** Ids of the tracks that are ducked / the key, among those that render. */
	ducked: string[];
	keyed: string[];
}

/** The ducking the export would do to `timeline`: which tracks dip, which key them, and how. A track is in it as `trackRenders` and the clips that make sound say (`clipSounds`, an enabled clip). */
export function duckPlan(timeline: Pick<Timeline, 'tracks' | 'master'>, audible: ReadonlySet<string>): DuckPlan {
	const whole = timeline as Timeline;
	const ducked: string[] = [];
	const keyed: string[] = [];
	for (const track of timeline.tracks) {
		if (!trackRenders(whole, track)) continue;
		const heard = track.clips.filter((c) => c.enabled !== false && clipSounds(c, audible));
		if (heard.length === 0) continue;
		if (track.duck) ducked.push(track.id);
		else keyed.push(track.id);
	}
	const mode = duckMode(timeline.master ?? {});
	const active = ducked.length > 0 && keyed.length > 0;
	return { active, mode, ducked, keyed };
}
