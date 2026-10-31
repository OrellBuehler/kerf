// What the library's Motion tab decides before it asks the backend: which of the selected clips a preset can be
// written into, why the tiles are off, how the length field reads, the options a click sends and when a click says
// something afterwards. The presets themselves are `motion-presets.ts`.

import { MIN_MOTION_SECONDS, motionSpec, wasClamped } from './motion-presets';
import type { MotionOptions } from './api';
import type { Clip, MotionKind, MotionMode, MotionPreset, MotionPresetInfo, MotionWindow, Timeline } from './types';

export interface MotionGroup {
	kind: MotionKind;
	label: string;
	/** Where in the clip the group's presets land, in the group's tooltip and under its heading. */
	hint: string;
}

/** The three groups, in the order the tab and the backend list them. */
export const MOTION_GROUPS: readonly MotionGroup[] = [
	{ kind: 'entrance', label: 'Entrance', hint: 'How a clip arrives, at its start' },
	{ kind: 'exit', label: 'Exit', hint: 'How a clip leaves, at its end' },
	{ kind: 'emphasis', label: 'Emphasis', hint: 'A beat at the start of the clip, or a slow move over all of it' }
];

/** The two ways a preset meets the keys a clip already has, as the tab words them. */
export const MOTION_MODES: readonly { value: MotionMode; label: string; hint: string }[] = [
	{
		value: 'add',
		label: 'Add',
		hint: 'Keeps the clip’s own animation outside the preset’s window, so an entrance, an exit and a beat in the middle live together'
	},
	{ value: 'replace', label: 'Replace', hint: 'Gives the numbers the preset moves exactly its keys, throwing the clip’s own for them away' }
];

/** The selected clips a preset can be written into: the ones on a picture track (a sound has no picture to move,
 *  and one named in the call would refuse the whole edit). A linked picture and its detached sound are selected
 *  together, so skipping the sound is the usual case, not a problem. */
export function motionTargets(timeline: Timeline, selected: readonly Clip[]): Clip[] {
	const picture = new Set(timeline.tracks.filter((t) => t.kind !== 'audio').flatMap((t) => t.clips.map((c) => c.id)));
	return selected.filter((c) => picture.has(c.id));
}

/** Why the presets are off, or `null` when a click can apply them. */
export function motionReason(timeline: Timeline, selected: readonly Clip[]): string | null {
	if (selected.length === 0) return 'Select a clip on the timeline to give it a motion.';
	if (motionTargets(timeline, selected).length === 0) {
		return selected.length === 1 ? 'This clip is a sound — select a picture clip.' : 'These clips are sounds — select picture clips.';
	}
	return null;
}

/** What the length field means: blank is the preset's own (`null`), a number of at least a hundredth of a second is
 *  that length, anything else (`undefined`) is not an entry and the field goes back to what it held. A length longer
 *  than a clip is the backend's to clamp, not the field's to refuse. */
export function parseMotionLength(text: string): number | null | undefined {
	const t = text.trim();
	if (t === '') return null;
	const v = Number(t.replace(',', '.'));
	return Number.isFinite(v) && v >= MIN_MOTION_SECONDS ? v : undefined;
}

/** A length as the field shows it: `0.5`, `1.25`, nothing for the preset's own. */
export const lengthText = (seconds: number | null): string => (seconds === null ? '' : String(Number(seconds.toFixed(3))));

/** The options a click sends: what is at its default is left out, so the backend's defaults stay the backend's. */
export function motionOptions(mode: MotionMode, length: number | null, intensity: number): MotionOptions {
	return {
		mode,
		...(length === null ? {} : { duration: length }),
		...(intensity === 1 ? {} : { intensity })
	};
}

const secondsText = (s: number) => `${Number(s.toFixed(2))} s`;

/** The tooltip of a tile: the preset's look, how long it runs and where in the clip, or why it is off. `length` is
 *  the tab's length field (`null`: the preset's own). */
export function presetTip(info: MotionPresetInfo, length: number | null, reason: string | null) {
	const seconds = length ?? info.default_duration;
	const span =
		seconds === undefined
			? 'Runs over the whole clip.'
			: `${secondsText(seconds)}, ${info.kind === 'exit' ? 'ending at the clip’s end' : 'from the clip’s start'}.`;
	return { text: info.label, detail: reason ?? `${info.hint} ${span}` };
}

/** What to say after a preset went in, or `null` when nothing needs saying: only a window the clip cut short. */
export function clampedNotice(
	preset: MotionPreset,
	applied: readonly MotionWindow[],
	length: number | null
): string | null {
	const short = applied.filter((w) => wasClamped(preset, w, length));
	if (short.length === 0) return null;
	const label = motionSpec(preset).label;
	if (applied.length === 1) return `${label} shortened to ${secondsText(short[0].end - short[0].start)} to fit the clip`;
	const shortest = Math.min(...short.map((w) => w.end - w.start));
	return `${label} shortened to fit ${short.length} of ${applied.length} clips, down to ${secondsText(shortest)}`;
}
