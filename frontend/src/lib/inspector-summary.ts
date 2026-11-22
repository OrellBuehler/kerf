// What a collapsed Inspector section says about itself, and what a keyframe toggle calls the number it
// animates. Pure, so the words are tested and the rows cannot drift from the summaries.

import type { AnalysisMeter } from './analysis-steps';
import { clock } from './clock';
import { COLOR_PROPERTIES, TRANSFORM_PROPERTIES, baseAt, channelOf, isKeyed, propertyKeys, propertyLabel, type Animatable } from './channels';
import { activeLook } from './style-presets';
import { isFullFrame, svgPresetLabel } from './svg-titles';
import { DEFAULT_COLOR, type Color, type Easing, type Property, type SvgOverlay } from './types';

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

/** One moment a clip's pose is keyed at: the five numbers there, and where the key is kept. */
export interface TransformKeyRow {
	/** Clip-local seconds. */
	time: number;
	scale: number;
	pos_x: number;
	pos_y: number;
	rotation: number;
	opacity: number;
	/** How the pose leaves this moment, when a key says (absent is linear). */
	easing?: Easing;
	/** A key of the whole-transform bundle (`clip.keyframes`, what *Keyframe at playhead* writes) sits here. */
	bundle: boolean;
	/** The numbers that have a key of their own here (`clip.channels`, what a ◇ writes). */
	tracks: Property[];
}

/** Every moment the pose is keyed at, whichever way it was keyed: the bundle's keys and the numbers' own, merged by time,
 *  earliest first. The Transform summary, the Animation section and its Clear all read this, so the two ways of keying
 *  are one thing to the panel. */
export function transformKeyRows(clip: Animatable): TransformKeyRow[] {
	const times = new Map<number, number>();
	for (const p of TRANSFORM_PROPERTIES) for (const k of propertyKeys(clip, p)) times.set(Math.round(k.time * 1000), k.time);
	return [...times.entries()]
		.sort((a, b) => a[0] - b[0])
		.map(([, time]) => {
			const at = (key: { time: number }) => close(key.time, time);
			const tracks = TRANSFORM_PROPERTIES.filter((p) => channelOf(clip, p)?.keys.some(at));
			const own = tracks.map((p) => channelOf(clip, p)!.keys.find(at)!);
			const bundle = clip.keyframes?.find(at);
			const easing = bundle?.easing ?? own.find((k) => k.easing)?.easing;
			const row: TransformKeyRow = {
				time,
				scale: baseAt(clip, 'scale', time),
				pos_x: baseAt(clip, 'pos_x', time),
				pos_y: baseAt(clip, 'pos_y', time),
				rotation: baseAt(clip, 'rotation', time),
				opacity: baseAt(clip, 'opacity', time),
				bundle: !!bundle,
				tracks
			};
			if (easing) row.easing = easing;
			return row;
		});
}

/** The Transform section's one line: `Animated · 3 keys` when any of the five numbers is keyed (a ◇ or the bundle), else
 *  its pose (`100% · 0°`). */
export function transformSummary(clip: Animatable, pose: { scale: number; rotation: number }): string {
	const n = transformKeyRows(clip).length;
	return n > 0 ? `Animated · ${n} ${n === 1 ? 'key' : 'keys'}` : `${Math.round(pose.scale * 100)}% · ${Math.round(pose.rotation)}°`;
}

/** Whether the playhead is on the clip, where a key can be put: a key before the clip's start or past its end would be a
 *  moment the clip does not have. `end` is the clip's end on the timeline. */
export function playheadOnClip(clip: { timeline_start: number }, end: number, playhead: number, eps = 1e-6): boolean {
	return playhead >= clip.timeline_start - eps && playhead <= end + eps;
}

/** The Timing section's line, labelled: `Start 0:00.0 · Dur 0:12.5`. `fmt` is how a time reads (the
 *  interface's own mode); without it, seconds. */
export function timingSummary(start: number, duration: number, fmt: (seconds: number) => string = clock): string {
	return `Start ${fmt(start)} · Dur ${fmt(duration)}`;
}

/** An animated title's Placement line: `Full frame`, else its size and corner (`40% × 16% at 30%, 42%`), and a
 *  title that is not opaque says how much is (`Full frame · 60% opaque`). */
export function svgPlacementSummary(o: Pick<SvgOverlay, 'x' | 'y' | 'width' | 'height' | 'opacity'>): string {
	const pct = (v: number) => `${Math.round(v * 100)}%`;
	const where = isFullFrame(o) ? 'Full frame' : `${pct(o.width)} × ${pct(o.height)} at ${pct(o.x)}, ${pct(o.y)}`;
	return o.opacity < 1 ? `${where} · ${pct(o.opacity)} opaque` : where;
}

/** An animated title's Artwork line: the preset it was made from, `Template` when it came from one, else `Custom SVG`. */
export function svgSourceSummary(o: Pick<SvgOverlay, 'preset'> & Partial<Pick<SvgOverlay, 'template'>>): string {
	return o.preset ? svgPresetLabel(o.preset.kind) : o.template ? 'Template' : 'Custom SVG';
}

/** The number a keyframe toggle animates, as the row names it. */
export function keyLabel(p: Property): string {
	return p === 'temperature' ? 'warmth' : propertyLabel(p);
}

/** The number with its article, for a sentence about one key of it: `a scale keyframe`, `an opacity keyframe`. */
export function keyframeOf(p: Property): string {
	const label = keyLabel(p);
	return `${/^[aeiou]/i.test(label) ? 'an' : 'a'} ${label} keyframe`;
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

export interface AnalysisPill {
	/** What the pill says: `5/5` when everything has run, `2/5`, `Analyze` when nothing has, `Analyzing`, `1 failed`, `Off`. */
	text: string;
	/** Which `Badge` tone draws it. */
	tone: 'success' | 'agent' | 'danger' | 'neutral';
}

/** A clip's analysis as the one small pill its header carries, instead of a tag per kind: the five chips are facts
 *  about the media, and the bin and the asset's own Inspector draw them in full. */
export function analysisPill(meter: AnalysisMeter | null): AnalysisPill | null {
	if (!meter) return null;
	if (meter.tone === 'running') return { text: 'Analyzing', tone: 'agent' };
	if (meter.tone === 'failed') return { text: meter.text, tone: 'danger' };
	if (meter.total === 0) return { text: 'Off', tone: 'neutral' };
	// Nothing has run: say what the pill does, not a `0/5` that reads as a score.
	if (meter.done === 0) return { text: 'Analyze', tone: 'neutral' };
	return { text: `${meter.done}/${meter.total}`, tone: meter.tone === 'done' ? 'success' : 'neutral' };
}
