// What a collapsed Inspector section says about itself, and what a keyframe toggle calls the number it
// animates. Pure, so the words are tested and the rows cannot drift from the summaries.

import { clock } from './clock';
import { COLOR_PROPERTIES, isKeyed, propertyKeys, propertyLabel, type Animatable } from './channels';
import { activeLook } from './style-presets';
import { DEFAULT_COLOR, type Color, type Property } from './types';

const close = (a: number, b: number) => Math.abs(a - b) < 1e-6;

/** Whether a colour is the ungraded one: nothing moved off its neutral value. */
export function isNeutralColor(c: Color): boolean {
	return (
		close(c.brightness, DEFAULT_COLOR.brightness) &&
		close(c.contrast, DEFAULT_COLOR.contrast) &&
		close(c.saturation, DEFAULT_COLOR.saturation) &&
		close(c.gamma, DEFAULT_COLOR.gamma) &&
		close(c.temperature ?? 0, DEFAULT_COLOR.temperature ?? 0)
	);
}

/** How many distinct moments the colour numbers are keyed at. */
export function colorKeyCount(clip: Animatable): number {
	const times = new Set<number>();
	for (const p of COLOR_PROPERTIES) for (const k of propertyKeys(clip, p)) times.add(Math.round(k.time * 1000));
	return times.size;
}

/** The Color section's one line: animated when any number is keyed, `None` for the ungraded picture,
 *  the look it matches, else `Custom`. (It read `Custom` for a clip nobody had touched.) */
export function colorSummary(clip: Animatable | undefined, shown: Color): string {
	if (clip && COLOR_PROPERTIES.some((p) => isKeyed(clip, p))) {
		const n = colorKeyCount(clip);
		return `Animated · ${n} ${n === 1 ? 'key' : 'keys'}`;
	}
	if (isNeutralColor(shown)) return 'None';
	return activeLook(shown)?.label ?? 'Custom';
}

/** The Timing section's line, labelled: `Start 0:00.0 · Dur 0:12.5`. */
export function timingSummary(start: number, duration: number): string {
	return `Start ${clock(start)} · Dur ${clock(duration)}`;
}

/** The number a keyframe toggle animates, as the row names it. */
export function keyLabel(p: Property): string {
	return p === 'temperature' ? 'warmth' : propertyLabel(p);
}

export interface KeyNeighbours {
	/** The nearest key before the playhead, clip-local seconds, or null. */
	prev: number | null;
	/** The nearest key after it, or null. */
	next: number | null;
	/** A key sits at the playhead (within `eps`). */
	here: boolean;
}

/** Where the keys of a number are around the playhead — for "previous key" / "next key" on its ◇. A key
 *  within `eps` of the playhead is *here*, not a neighbour: stepping must move somewhere. */
export function keyNeighbours(times: readonly number[], at: number, eps = 0.02): KeyNeighbours {
	let prev: number | null = null;
	let next: number | null = null;
	let here = false;
	for (const t of times) {
		if (Math.abs(t - at) <= eps) here = true;
		else if (t < at && (prev === null || t > prev)) prev = t;
		else if (t > at && (next === null || t < next)) next = t;
	}
	return { prev, next, here };
}
