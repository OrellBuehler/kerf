// What a collapsed Inspector section says about itself, and what a keyframe toggle calls the number it
// animates. Pure, so the words are tested and the rows cannot drift from the summaries.

import type { AnalysisMeter } from './analysis-steps';
import { clock } from './clock';
import { COLOR_PROPERTIES, TRANSFORM_PROPERTIES, baseAt, channelOf, isCropProperty, isKeyed, isTransformProperty, keyTime, propertyKeys, propertyLabel, type Animatable } from './channels';
import { activeLook } from './style-presets';
import { isFullFrame, svgPresetLabel } from './svg-titles';
import { DEFAULT_COLOR, clipDuration, type Clip, type Color, type Easing, type Keyframe, type Property, type PropertyKey, type SvgOverlay } from './types';

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
	/** Clip-local seconds (the earliest of the keys that merged into this moment). */
	time: number;
	/** The moment as whole milliseconds: what keys are merged by, and what a removal takes out (`momentOf`). */
	moment: number;
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

/** The moment a key is at, as whole milliseconds: keys that agree to the millisecond are one moment. */
export const momentOf = (time: number): number => Math.round(time * 1000);

/** Every moment the pose is keyed at, whichever way it was keyed: the keys of the bundle and of the numbers' own tracks
 *  merged by moment, earliest first. It is the union of what the two stores hold, whatever tracks exist (a number may
 *  have a track of its own, even an empty one, and the bundle's keys are still there to be seen and cleared). The
 *  Transform summary, the Animation section and its Clear all read this, so the two ways of keying are one thing to the
 *  panel. */
export function transformKeyRows(clip: Animatable): TransformKeyRow[] {
	const moments = new Map<number, number>();
	const note = (time: number) => {
		const m = momentOf(time);
		moments.set(m, Math.min(moments.get(m) ?? time, time));
	};
	for (const k of clip.keyframes ?? []) note(k.time);
	for (const p of TRANSFORM_PROPERTIES) for (const k of channelOf(clip, p)?.keys ?? []) note(k.time);
	return [...moments.entries()]
		.sort((a, b) => a[0] - b[0])
		.map(([moment, time]) => {
			const at = (key: { time: number }) => momentOf(key.time) === moment;
			const tracks = TRANSFORM_PROPERTIES.filter((p) => channelOf(clip, p)?.keys.some(at));
			const own = tracks.map((p) => channelOf(clip, p)!.keys.find(at)!);
			const bundle = clip.keyframes?.find(at);
			const easing = bundle?.easing ?? own.find((k) => k.easing)?.easing;
			const row: TransformKeyRow = {
				time,
				moment,
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

/** Why a key cannot be put on a clip while the playhead is off it, as the key button says it. */
export const OFF_CLIP = 'Move the playhead onto this clip to put a key on it';
/** The same, for a number whose edit *is* a key, as the row says it. */
export const OFF_CLIP_EDIT = 'This number is keyed, so a change is a key: move the playhead onto this clip to change it';

/** The playhead is on the clip, from its first frame to its end (`playheadOnClip`, with the clip's own end). */
export const onClipAt = (clip: Pick<Clip, 'timeline_start' | 'source_in' | 'source_out' | 'speed' | 'speed_ramp'>, playhead: number): boolean =>
	playheadOnClip(clip, clip.timeline_start + clipDuration(clip as Clip), playhead);

/** Why a key cannot be written at `playhead` on `clip`, or null when it can. What every writer of a key at the playhead
 *  asks first: a key before the clip's start (`keyTime` clamps it to the first frame) or past its end is a moment the clip
 *  does not have. */
export const keyRefusal = (clip: Pick<Clip, 'timeline_start' | 'source_in' | 'source_out' | 'speed' | 'speed_ramp'>, playhead: number): string | null =>
	onClipAt(clip, playhead) ? null : OFF_CLIP;

/** Whether a key of the number sits at the playhead: only on the clip. `keyTime` clamps the playhead to the clip's first frame,
 *  so before the clip a key at the start would otherwise read as under the playhead (and offer to be removed). */
export function keyHere(
	clip: Pick<Clip, 'timeline_start' | 'source_in' | 'source_out' | 'speed' | 'speed_ramp'>,
	playhead: number,
	times: readonly number[],
	eps = 0.02
): boolean {
	return onClipAt(clip, playhead) && times.some((t) => Math.abs(t - keyTime(clip, playhead)) <= eps);
}

/** Whether a change to the number *is* a key: its own keys are in force, the whole-transform bundle has keys (a transform
 *  number's edit then writes a bundle key), or it is the animated crop, which has no static value. What an edit of it does
 *  at a playhead that is not on the clip is the writer's refusal, and the row is off. */
export function editWritesKey(clip: Animatable, prop: Property): boolean {
	return isKeyed(clip, prop) || (isTransformProperty(prop) && (clip.keyframes?.length ?? 0) > 0) || isCropProperty(prop);
}

/** What taking a moment out of the pose's keys writes. */
export interface KeyRemoval {
	/** The numbers that have a key of their own there, each with the keys it keeps. */
	tracks: { prop: Property; keys: PropertyKey[] }[];
	/** The bundle without that moment, or null when the bundle has no key there. */
	bundle: Keyframe[] | null;
	/** Nothing else is keyed: one `clear_keyframes` takes both stores, as one revision. */
	clears: boolean;
}

/** What removing `row` takes out, from whichever store holds it: every key that merged into the row (those within a
 *  millisecond of each other), in the numbers' own tracks and in the bundle. */
export function keyRemoval(clip: Animatable, row: TransformKeyRow, rows: readonly TransformKeyRow[]): KeyRemoval {
	const here = (k: { time: number }) => momentOf(k.time) === row.moment;
	// plain copies: the clip's keys are the page's reactive state, and what is written is data (the harness clones it)
	const plain = <T>(v: T): T => JSON.parse(JSON.stringify(v));
	return {
		tracks: row.tracks.map((prop) => ({ prop, keys: plain(propertyKeys(clip, prop).filter((k) => !here(k))) })),
		bundle: row.bundle ? plain((clip.keyframes ?? []).filter((k) => !here(k))) : null,
		clears: rows.length === 1 && row.bundle && row.tracks.length > 0
	};
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
