/**
 * Motion presets (B8a): the faithful TS mirror of kerf-core's `model/motion.rs` (the `SPECS` table,
 * `motion_window`, `shaped`, `preset_keys`, `merged`, `Clip::apply_motion`) and `project/motion.rs`
 * (`Project::apply_motion_preset`). An entrance, an exit or an emphasis is pure data that expands into
 * eased per-property keys on a clip's transform numbers; nothing else draws it.
 *
 * Each preset is a row: a kind, a default span, a `base` and, per number, keys `(at, x, easing)` — `at` the
 * fraction of the window, `x` a factor of the reference for scale and opacity and an offset for position
 * (frame fractions) and rotation (degrees). The reference is the clip's pose (`baseAt`: its keys, sampled before
 * anything is written; a live behaviour rides on top of them and is not written into them): an entrance ends on
 * the pose at the window's end, an exit starts from the pose at its start, an emphasis rides the line between the
 * two edge poses. `replace` is relative to the number's static value instead. `fixtures/motion-presets-corpus.json` (written by kerf-core) pins every number
 * (`motion-presets.test.ts`).
 */
import { baseAt, clampProperty, propertyKeys, propertyLabel, propertyKeysProblem, propertyProblem, setPropertyKeys, staticValue, upsertKey } from './channels';
import { TINY_SCALE } from './layer-geometry';
import { clipDuration } from './speed-ramp';
import type { Edited } from './link-ops';
import type {
	Clip,
	Easing,
	MotionApplied,
	MotionKind,
	MotionMode,
	MotionPreset,
	MotionPresetInfo,
	Property,
	PropertyKey,
	Timeline
} from './types';

/** The shortest window a preset is written over (seconds). */
export const MIN_MOTION_SECONDS = 0.01;
/** The weakest and the strongest `intensity`. */
export const MIN_MOTION_INTENSITY = 0.25;
export const MAX_MOTION_INTENSITY = 2;

/** Which pose the keys of a preset are relative to. */
export type MotionBase = 'start' | 'end' | 'trend';

export interface MotionKeySpec {
	at: number;
	x: number;
	easing: Easing;
}

export interface MotionTrackSpec {
	prop: Property;
	keys: MotionKeySpec[];
}

export interface MotionSpec {
	id: MotionPreset;
	label: string;
	hint: string;
	kind: MotionKind;
	/** Seconds; `null` fills the rest of the clip from where the window starts. */
	span: number | null;
	base: MotionBase;
	tracks: MotionTrackSpec[];
}

const LIN: Easing = 'linear';
const IN: Easing = 'ease_in';
const OUT: Easing = 'ease_out';
const IO: Easing = 'ease_in_out';

type K = [at: number, x: number, easing: Easing];
const row = (prop: Property, keys: K[]): MotionTrackSpec => ({
	prop,
	keys: keys.map(([at, x, easing]) => ({ at, x, easing }))
});

const spec = (
	id: MotionPreset,
	label: string,
	hint: string,
	kind: MotionKind,
	span: number | null,
	base: MotionBase,
	tracks: MotionTrackSpec[]
): MotionSpec => ({ id, label, hint, kind, span, base, tracks });

/** The table, in kerf-core's order: entrances, exits, emphases. */
export const MOTION_SPECS: readonly MotionSpec[] = [
	spec('fade_in', 'Fade in', "Opacity rises from 0 to the clip's own.", 'entrance', 0.5, 'end', [
		row('opacity', [[0, 0, OUT], [1, 1, LIN]])
	]),
	spec('slide_in_from_left', 'Slide in from left', 'Slides in from beyond the left edge of the frame.', 'entrance', 0.6, 'end', [
		row('pos_x', [[0, -1, OUT], [1, 0, LIN]])
	]),
	spec('slide_in_from_right', 'Slide in from right', 'Slides in from beyond the right edge of the frame.', 'entrance', 0.6, 'end', [
		row('pos_x', [[0, 1, OUT], [1, 0, LIN]])
	]),
	spec('slide_in_from_top', 'Slide in from top', 'Slides in from above the frame.', 'entrance', 0.6, 'end', [
		row('pos_y', [[0, -1, OUT], [1, 0, LIN]])
	]),
	spec('slide_in_from_bottom', 'Slide in from bottom', 'Slides in from below the frame.', 'entrance', 0.6, 'end', [
		row('pos_y', [[0, 1, OUT], [1, 0, LIN]])
	]),
	spec('zoom_in', 'Zoom in', 'Grows from 70% of its size while fading in.', 'entrance', 0.6, 'end', [
		row('scale', [[0, 0.7, OUT], [1, 1, LIN]]),
		row('opacity', [[0, 0, OUT], [1, 1, LIN]])
	]),
	spec('pop_in', 'Pop in', 'Springs up from nothing, overshoots a little and settles.', 'entrance', 0.5, 'end', [
		row('scale', [[0, 0, OUT], [0.65, 1.12, IO], [1, 1, LIN]]),
		row('opacity', [[0, 0, LIN], [0.3, 1, LIN], [1, 1, LIN]])
	]),
	spec('spin_in', 'Spin in', 'Turns half a revolution while growing into place.', 'entrance', 0.7, 'end', [
		row('rotation', [[0, -180, OUT], [1, 0, LIN]]),
		row('scale', [[0, 0.2, OUT], [1, 1, LIN]])
	]),
	spec('drop_in', 'Drop in', 'Falls in from above the frame and settles with a small bounce.', 'entrance', 0.7, 'end', [
		row('pos_y', [[0, -1, IN], [0.62, 0.05, OUT], [0.82, -0.015, IO], [1, 0, LIN]])
	]),
	spec('fade_out', 'Fade out', "Opacity falls from the clip's own to 0.", 'exit', 0.5, 'start', [
		row('opacity', [[0, 1, IN], [1, 0, LIN]])
	]),
	spec('slide_out_to_left', 'Slide out to left', 'Slides out beyond the left edge of the frame.', 'exit', 0.6, 'start', [
		row('pos_x', [[0, 0, IN], [1, -1, LIN]])
	]),
	spec('slide_out_to_right', 'Slide out to right', 'Slides out beyond the right edge of the frame.', 'exit', 0.6, 'start', [
		row('pos_x', [[0, 0, IN], [1, 1, LIN]])
	]),
	spec('slide_out_to_top', 'Slide out to top', 'Slides out above the frame.', 'exit', 0.6, 'start', [
		row('pos_y', [[0, 0, IN], [1, -1, LIN]])
	]),
	spec('slide_out_to_bottom', 'Slide out to bottom', 'Slides out below the frame.', 'exit', 0.6, 'start', [
		row('pos_y', [[0, 0, IN], [1, 1, LIN]])
	]),
	spec('zoom_out', 'Zoom out', 'Shrinks to 70% of its size while fading out.', 'exit', 0.6, 'start', [
		row('scale', [[0, 1, IN], [1, 0.7, LIN]]),
		row('opacity', [[0, 1, IN], [1, 0, LIN]])
	]),
	spec('pop_out', 'Pop out', 'Swells a little, then shrinks to nothing.', 'exit', 0.5, 'start', [
		row('scale', [[0, 1, IO], [0.35, 1.12, IN], [1, 0, LIN]]),
		row('opacity', [[0, 1, LIN], [0.7, 1, LIN], [1, 0, LIN]])
	]),
	spec('spin_out', 'Spin out', 'Turns half a revolution while shrinking away.', 'exit', 0.7, 'start', [
		row('rotation', [[0, 0, IN], [1, 180, LIN]]),
		row('scale', [[0, 1, IN], [1, 0.2, LIN]])
	]),
	spec('drop_out', 'Drop out', 'Rises a touch, then falls out of the bottom of the frame.', 'exit', 0.7, 'start', [
		row('pos_y', [[0, 0, OUT], [0.2, -0.04, IN], [1, 1, LIN]])
	]),
	spec('pulse', 'Pulse', 'A quick beat: grows 8% and returns.', 'emphasis', 0.6, 'trend', [
		row('scale', [[0, 1, IO], [0.5, 1.08, IO], [1, 1, LIN]])
	]),
	spec('wiggle', 'Wiggle', 'Rocks left and right, settling to still.', 'emphasis', 0.8, 'trend', [
		row('rotation', [[0, 0, IO], [0.14, 5, IO], [0.36, -5, IO], [0.57, 3.5, IO], [0.78, -2, IO], [1, 0, LIN]])
	]),
	spec('bounce', 'Bounce', 'Hops in place, each hop lower than the last.', 'emphasis', 0.8, 'trend', [
		row('pos_y', [[0, 0, OUT], [0.22, -0.1, IN], [0.45, 0, OUT], [0.62, -0.04, IN], [0.8, 0, OUT], [0.9, -0.012, IN], [1, 0, LIN]])
	]),
	spec('push_in', 'Push in', 'A slow zoom in, 15%, over the whole clip (Ken Burns).', 'emphasis', null, 'start', [
		row('scale', [[0, 1, IO], [1, 1.15, LIN]])
	]),
	spec('pull_out', 'Pull out', 'A slow zoom out, 15%, over the whole clip.', 'emphasis', null, 'end', [
		row('scale', [[0, 1.15, IO], [1, 1, LIN]])
	])
];

/** Every preset id, in table order. */
export const MOTION_PRESET_IDS: readonly MotionPreset[] = MOTION_SPECS.map((s) => s.id);

export const MOTION_KINDS: readonly MotionKind[] = ['entrance', 'exit', 'emphasis'];

export const isMotionPreset = (id: unknown): id is MotionPreset => MOTION_PRESET_IDS.includes(id as MotionPreset);

export function motionSpec(preset: MotionPreset): MotionSpec {
	const found = MOTION_SPECS.find((s) => s.id === preset);
	if (!found) throw new Error(`invalid argument: no motion preset ${preset}`);
	return found;
}

/** Every preset as a picker lists it (`motion_presets()`). */
export function motionPresets(): MotionPresetInfo[] {
	return MOTION_SPECS.map((s) => ({
		id: s.id,
		label: s.label,
		kind: s.kind,
		...(s.span === null ? {} : { default_duration: s.span }),
		hint: s.hint,
		properties: s.tracks.map((t) => t.prop)
	}));
}

/** A number as Rust's `Display` writes it (what the refusals quote). */
function display(n: number): string {
	if (Number.isNaN(n)) return 'NaN';
	if (!Number.isFinite(n)) return n < 0 ? '-inf' : 'inf';
	return String(n);
}

const refuse = (why: string) => new Error(`invalid argument: ${why}`);

/** `(start, end)` of a preset's window on a clip `len` seconds long (`motion_window`): `at` is where it starts
 *  (default 0, or for an exit `duration` before the clip's end), `duration` defaults to the preset's — a preset
 *  with no fixed span fills the rest of the clip — and is clamped to what is left after `at`. */
export function motionWindow(
	spec: Pick<MotionSpec, 'kind' | 'span'>,
	len: number,
	at?: number | null,
	duration?: number | null
): [number, number] {
	const a = at ?? undefined;
	const d0 = duration ?? undefined;
	if (!Number.isFinite(len) || len < MIN_MOTION_SECONDS) {
		throw refuse(`the clip is ${len.toFixed(3)} s long: a motion needs at least ${display(MIN_MOTION_SECONDS)} s`);
	}
	if (d0 !== undefined && (!Number.isFinite(d0) || d0 < MIN_MOTION_SECONDS)) {
		throw refuse(`duration must be a number of at least ${display(MIN_MOTION_SECONDS)} seconds`);
	}
	if (a !== undefined && (!Number.isFinite(a) || a < 0)) throw refuse('at must be a number of seconds >= 0');
	const from = a ?? 0;
	const room = len - from;
	if (room < MIN_MOTION_SECONDS) {
		throw refuse(
			`at ${from.toFixed(3)} s leaves no room in a clip ${len.toFixed(3)} s long (a motion needs at least ${display(MIN_MOTION_SECONDS)} s)`
		);
	}
	const wanted = d0 ?? spec.span ?? room;
	const d = Math.min(wanted, room);
	if (a === undefined && spec.kind === 'exit') return [len - d, len];
	return [from, d >= room ? len : from + d];
}

/** `reference` moved by `x` (a factor for scale and opacity, an offset for the rest) at `intensity`, held to the
 *  range of the number; a scale never goes below `TINY_SCALE` (or below its own reference, when that is smaller). */
function shaped(prop: Property, reference: number, x: number, intensity: number): number {
	const multiplies = prop === 'scale' || prop === 'opacity';
	let raw = multiplies ? reference * (1 + (x - 1) * intensity) : reference + x * intensity;
	if (!Number.isFinite(raw)) throw refuse(`the motion would put the ${propertyLabel(prop)} at ${display(raw)}`);
	if (prop === 'scale') raw = Math.max(raw, Math.min(TINY_SCALE, reference));
	const value = clampProperty(prop, raw);
	const problem = propertyProblem(prop, value);
	if (problem) throw refuse(problem);
	return value;
}

/** The preset's keys for one number over `window` (`preset_keys`). Added, they are relative to what the clip does
 *  now; replacing, to the number's static value. */
function presetKeys(
	clip: Clip,
	s: MotionSpec,
	t: MotionTrackSpec,
	window: [number, number],
	intensity: number,
	mode: MotionMode
): PropertyKey[] {
	const [start, end] = window;
	const pose = (at: number) => (mode === 'add' ? baseAt(clip, t.prop, at) : staticValue(clip, t.prop));
	const [poseStart, poseEnd] = [pose(start), pose(end)];
	const last = t.keys.length - 1;
	return t.keys.map((key, i) => {
		const time = key.at >= 1 ? end : start + key.at * (end - start);
		const reference = s.base === 'start' ? poseStart : s.base === 'end' ? poseEnd : poseStart * (1 - key.at) + poseEnd * key.at;
		return { time, value: shaped(t.prop, reference, key.x, intensity), easing: i === last ? 'linear' : key.easing };
	});
}

/** `fresh` (the preset's keys, sorted, spanning the window) merged into the clip's keys of `prop` (`add`): its own
 *  inside the window go, the ones outside stay, and the segment each window edge falls in is split first so that
 *  what stays is the curve it was. */
function merged(clip: Clip, prop: Property, fresh: PropertyKey[]): PropertyKey[] {
	const SAME = 1e-6;
	const existing = propertyKeys(clip, prop);
	if (existing.length === 0) return fresh;
	const [start, end] = [fresh[0].time, fresh[fresh.length - 1].time];
	let keys = existing;
	for (const edge of [start, end]) keys = upsertKey(keys, { time: edge, value: baseAt(clip, prop, edge) });
	fresh[fresh.length - 1].easing = keys.find((k) => Math.abs(k.time - end) <= SAME)?.easing ?? 'linear';
	return [...keys.filter((k) => k.time < start - SAME), ...fresh, ...keys.filter((k) => k.time > end + SAME)];
}

/** Write `preset` into `clip`'s animation and answer the window it covers (`Clip::apply_motion`); `len` is the
 *  clip's length on the timeline. All or nothing: a refusal leaves the clip as it was. */
export function applyMotion(
	clip: Clip,
	len: number,
	preset: MotionPreset,
	mode: MotionMode = 'add',
	at?: number | null,
	duration?: number | null,
	intensity?: number | null
): [number, number] {
	const strength = intensity ?? 1;
	if (intensity != null && !(Number.isFinite(intensity) && intensity >= MIN_MOTION_INTENSITY && intensity <= MAX_MOTION_INTENSITY)) {
		throw refuse(`intensity must be within ${display(MIN_MOTION_INTENSITY)}..=${display(MAX_MOTION_INTENSITY)} (got ${display(intensity)})`);
	}
	const s = motionSpec(preset);
	const window = motionWindow(s, len, at, duration);
	const written = s.tracks.map((t) => {
		const fresh = presetKeys(clip, s, t, window, strength, mode);
		return [t.prop, mode === 'replace' ? fresh : merged(clip, t.prop, fresh)] as const;
	});
	for (const [prop, keys] of written) setPropertyKeys(clip, prop, keys);
	return window;
}

/** The revision label `Project::apply_motion_preset` records. */
export function motionLabel(preset: MotionPreset, clips: number): string {
	const label = motionSpec(preset).label;
	return clips === 1 ? `Apply motion preset ${label}` : `Apply motion preset ${label} (${clips} clips)`;
}

/** `Project::apply_motion_preset` on a timeline: the preset written into each of `clipIds` (a clip named twice once)
 *  as one edit, all or nothing — the timeline returned is a copy, the one given is untouched. A clip on an audio
 *  track refuses the whole edit; a locked track does not (a lock guards layout, keyframes are look). */
export function applyMotionPreset(
	timeline: Timeline,
	clipIds: string[],
	preset: MotionPreset,
	mode: MotionMode = 'add',
	at?: number | null,
	duration?: number | null,
	intensity?: number | null
): Edited<MotionApplied[]> {
	const ids = clipIds.filter((id, i) => clipIds.indexOf(id) === i);
	if (ids.length === 0) throw refuse('name at least one clip');
	const tl = structuredClone(timeline);
	const touched = motionSpec(preset).tracks.map((t) => t.prop);
	const applied: MotionApplied[] = [];
	for (const id of ids) {
		const track = tl.tracks.find((t) => t.clips.some((c) => c.id === id));
		const clip = track?.clips.find((c) => c.id === id);
		if (!track || !clip) throw new Error(`clip not found: ${id}`);
		if (track.kind === 'audio') throw refuse(`clip ${id} is on audio track ${track.name}: a sound has no picture to move`);
		const [start, end] = applyMotion(clip, clipDuration(clip), preset, mode, at, duration, intensity);
		for (const t of clip.channels ?? []) {
			if (!touched.includes(t.prop)) continue;
			const problem = propertyKeysProblem(t.prop, t.keys);
			if (problem) throw refuse(problem);
		}
		applied.push({ clip: structuredClone(clip), start, end });
	}
	return { timeline: tl, result: applied, label: motionLabel(preset, ids.length) };
}

/** Whether a window came out shorter than the one asked for: the clip was in the way. The Motion tab says so
 *  only then. `asked` is the duration requested (or the preset's own); a preset that fills the rest of the clip
 *  is never clamped. */
export function wasClamped(preset: MotionPreset, window: { start: number; end: number }, duration?: number | null): boolean {
	const asked = duration ?? motionSpec(preset).span;
	return asked !== null && window.end - window.start < asked - 1e-9;
}
