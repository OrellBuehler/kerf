/**
 * Per-property keyframe channels: the faithful TS mirror of kerf-core's `model/channels.rs`
 * (`Property`, `PropertyTrack`, `Clip::property_keys` / `property_curve` / `property_at`,
 * `color_at`, `volume_at`, `upsert`, `rebase_head`). Any one number of a clip — a transform
 * number, a colour number or the clip's volume — can have keys of its own; a number with a track is
 * driven by it, a transform number without one reads the legacy whole-transform bundle
 * (`clip.keyframes`), anything else is its static value. One resolver (`propertyKeys`), so the
 * editor's sampled pose, the harness's edits and the export read one curve. Its numbers are pinned
 * against the Rust tests (`channels.test.ts`).
 */
import { easedPoints, splitEasing, EASE_STEPS, curve } from './easing';
import type { Clip, Color, Easing, Keyframe, Property, PropertyKey, PropertyTrack, Transform } from './types';
import { DEFAULT_COLOR, DEFAULT_TRANSFORM } from './types';

/** What the samplers read of a clip: a gain clip (`audio-mix.ts`) carries no transform. */
export type Animatable = Pick<Clip, 'volume' | 'channels' | 'keyframes' | 'transform' | 'color'>;

export const PROPERTIES: readonly Property[] = [
	'scale',
	'pos_x',
	'pos_y',
	'rotation',
	'opacity',
	'brightness',
	'contrast',
	'saturation',
	'gamma',
	'temperature',
	'volume'
];
export const TRANSFORM_PROPERTIES: readonly Property[] = ['scale', 'pos_x', 'pos_y', 'rotation', 'opacity'];
export const COLOR_PROPERTIES: readonly Property[] = ['brightness', 'contrast', 'saturation', 'gamma', 'temperature'];

/** The most a clip's volume channel may reach (the master bus's own ceiling, +12 dB). */
export const MAX_CHANNEL_VOLUME = 4;
/** The longest track of keys a property may be given (`MAX_CHANNEL_KEYS` in project.rs). */
export const MAX_CHANNEL_KEYS = 1000;
/** A scale smaller than this is read as this (a scale of 0 is "unset" to FFmpeg's `scale`). */
const MIN_CHANNEL_SCALE = 1e-6;

export const isTransformProperty = (p: Property) => TRANSFORM_PROPERTIES.includes(p);
export const isColorProperty = (p: Property) => COLOR_PROPERTIES.includes(p);

/** A short name for a sentence ("opacity keyframes 2 → 3"). */
export function propertyLabel(p: Property): string {
	if (p === 'pos_x') return 'position x';
	if (p === 'pos_y') return 'position y';
	return p;
}

/** The range the static setter accepts (`undefined` is unbounded on that side). */
export function propertyRange(p: Property): [number | undefined, number | undefined] {
	switch (p) {
		case 'scale':
			return [0, undefined];
		case 'pos_x':
		case 'pos_y':
		case 'rotation':
			return [undefined, undefined];
		case 'opacity':
			return [0, 1];
		case 'brightness':
		case 'temperature':
			return [-1, 1];
		case 'contrast':
			return [0, 4];
		case 'saturation':
			return [0, 3];
		case 'gamma':
			return [0.1, 10];
		case 'volume':
			return [0, MAX_CHANNEL_VOLUME];
	}
}

/** The value that changes nothing. */
export function neutral(p: Property): number {
	return ['scale', 'opacity', 'contrast', 'saturation', 'gamma', 'volume'].includes(p) ? 1 : 0;
}

/** `value` brought into range; a number that is not one becomes the neutral value
 *  (`Property::clamp`). */
export function clampProperty(p: Property, value: number): number {
	if (!Number.isFinite(value)) return neutral(p);
	const [lo, hi] = propertyRange(p);
	let v = p === 'scale' ? Math.max(value, MIN_CHANNEL_SCALE) : value;
	if (lo !== undefined) v = Math.max(v, lo);
	if (hi !== undefined) v = Math.min(v, hi);
	return v;
}

/** The refusal a key value gets (`Property::check`), or `undefined` when it is fine. */
export function propertyProblem(p: Property, value: number): string | undefined {
	const [lo, hi] = propertyRange(p);
	const bad =
		!Number.isFinite(value) || (p === 'scale' && value <= 0) || (lo !== undefined && value < lo) || (hi !== undefined && value > hi);
	if (!bad) return undefined;
	const range =
		lo !== undefined && hi !== undefined ? `within ${lo}..=${hi}` : lo !== undefined ? 'a finite value > 0' : 'finite';
	return `${p} keyframe values must be ${range} (got ${value})`;
}

/** What `Project::set_property_keyframes` refuses a list of keys for, or `undefined`. */
export function propertyKeysProblem(p: Property, keys: PropertyKey[]): string | undefined {
	if (keys.length > MAX_CHANNEL_KEYS) return `at most ${MAX_CHANNEL_KEYS} keyframes per property (got ${keys.length})`;
	for (const k of keys) {
		if (!Number.isFinite(k.time) || k.time < 0) return 'keyframe time must be >= 0';
		const value = propertyProblem(p, k.value);
		if (value) return value;
		const e = k.easing;
		if (e && typeof e !== 'string' && Object.values(e.bezier).some((v) => !Number.isFinite(v) || v < 0 || v > 1)) {
			return 'bezier control points must be within 0.0..=1.0 (no overshoot)';
		}
	}
	return undefined;
}

const byTime = (a: { time: number }, b: { time: number }) => a.time - b.time;

/** The track of `prop`, if the clip has one. */
export function channelOf(clip: Animatable, prop: Property): PropertyTrack | undefined {
	return clip.channels?.find((c) => c.prop === prop);
}

function bundleValue(k: Keyframe, prop: Property): number {
	switch (prop) {
		case 'scale':
			return k.scale;
		case 'pos_x':
			return k.pos_x;
		case 'pos_y':
			return k.pos_y;
		case 'rotation':
			return k.rotation;
		case 'opacity':
			return k.opacity;
		default:
			return neutral(prop);
	}
}

/** The keys that drive `prop`, sorted by time: the property's own track (values held to range),
 *  else — for a transform property — the legacy bundle's keys as stored, else none. */
export function propertyKeys(clip: Animatable, prop: Property): PropertyKey[] {
	const track = channelOf(clip, prop);
	if (track) {
		return track.keys
			.map((k) => ({ ...k, value: clampProperty(prop, k.value) }))
			.sort(byTime);
	}
	if (isTransformProperty(prop)) {
		return [...(clip.keyframes ?? [])]
			.sort(byTime)
			.map((k) => ({ time: k.time, value: bundleValue(k, prop), ...(k.easing ? { easing: k.easing } : {}) }));
	}
	return [];
}

/** Whether `prop` has any key. */
export function isKeyed(clip: Animatable, prop: Property): boolean {
	const track = channelOf(clip, prop);
	return track ? track.keys.length > 0 : isTransformProperty(prop) && (clip.keyframes?.length ?? 0) > 0;
}

/** Linearly interpolate `(time, value)` points at `at`, holding the ends (`interpolate`). */
export function interpolate(points: [number, number][], at: number): number | undefined {
	if (points.length === 0) return undefined;
	if (points.length === 1) return points[0][1];
	if (at <= points[0][0]) return points[0][1];
	for (let i = 0; i + 1 < points.length; i++) {
		const [t0, v0] = points[i];
		const [t1, v1] = points[i + 1];
		if (at < t1) {
			if (t1 <= t0) return v0;
			return v0 + ((v1 - v0) * (at - t0)) / (t1 - t0);
		}
	}
	return points[points.length - 1][1];
}

/** The polyline every renderer draws for these keys (`key_polyline`). */
export function keyPolyline(keys: PropertyKey[]): [number, number][] {
	return easedPoints(keys.map((k): [number, number, Easing] => [k.time, k.value, k.easing ?? 'linear']));
}

/** The polyline every renderer draws for `prop`; empty when the property is not keyed. */
export function propertyCurve(clip: Animatable, prop: Property): [number, number][] {
	return keyPolyline(propertyKeys(clip, prop));
}

/** The value `prop` is written as when it is not keyed. */
export function staticValue(clip: Animatable, prop: Property): number {
	const t = clip.transform ?? DEFAULT_TRANSFORM;
	const c = clip.color ?? DEFAULT_COLOR;
	switch (prop) {
		case 'scale':
			return t.scale ?? 1;
		case 'pos_x':
			return t.pos_x ?? 0;
		case 'pos_y':
			return t.pos_y ?? 0;
		case 'rotation':
			return t.rotation ?? 0;
		case 'opacity':
			return t.opacity ?? 1;
		case 'brightness':
			return c.brightness ?? 0;
		case 'contrast':
			return c.contrast ?? 1;
		case 'saturation':
			return c.saturation ?? 1;
		case 'gamma':
			return c.gamma ?? 1;
		case 'temperature':
			return c.temperature ?? 0;
		case 'volume':
			return clip.volume ?? 1;
	}
}

/** `prop` at `local` seconds from the clip's start: its curve, else its static value. */
export function propertyAt(clip: Animatable, prop: Property, local: number): number {
	return interpolate(propertyCurve(clip, prop), local) ?? staticValue(clip, prop);
}

/** Whether any transform number is keyed (`Clip::is_animated`). */
export const transformAnimated = (clip: Animatable) => TRANSFORM_PROPERTIES.some((p) => isKeyed(clip, p));
/** Whether any colour number is keyed (`Clip::color_animated`). */
export const colorAnimated = (clip: Animatable) => COLOR_PROPERTIES.some((p) => isKeyed(clip, p));
/** Whether the clip's volume is keyed (`Clip::volume_animated`). */
export const volumeAnimated = (clip: Animatable) => isKeyed(clip, 'volume');

/** The clip's static transform with every keyed number sampled at `local` (`Clip::transform_at`). */
export function transformAt(clip: Animatable, local: number): Transform {
	const t: Transform = { ...DEFAULT_TRANSFORM, ...(clip.transform ?? {}) };
	if (!transformAnimated(clip)) return t;
	t.scale = propertyAt(clip, 'scale', local);
	t.pos_x = propertyAt(clip, 'pos_x', local);
	t.pos_y = propertyAt(clip, 'pos_y', local);
	t.rotation = propertyAt(clip, 'rotation', local);
	t.opacity = propertyAt(clip, 'opacity', local);
	return t;
}

/** The clip's colour with every keyed number sampled at `local` (`Clip::color_at`). */
export function colorAt(clip: Animatable, local: number): Color {
	const c: Color = { ...DEFAULT_COLOR, ...(clip.color ?? {}) };
	if (!colorAnimated(clip)) return c;
	return {
		brightness: propertyAt(clip, 'brightness', local),
		contrast: propertyAt(clip, 'contrast', local),
		saturation: propertyAt(clip, 'saturation', local),
		gamma: propertyAt(clip, 'gamma', local),
		temperature: propertyAt(clip, 'temperature', local)
	};
}

/** The clip's gain (linear) at `local`, fades and the track fader not included. */
export const volumeAt = (clip: Animatable, local: number) => propertyAt(clip, 'volume', local);

// ---- edits (mutate `clip`: the harness works on a scratch copy) ------------------------

const plain = (k: PropertyKey): PropertyKey => {
	const out: PropertyKey = { time: k.time, value: k.value };
	if (k.easing && k.easing !== 'linear') out.easing = k.easing;
	return out;
};

/** `key` put into `keys` (`upsert`): a key within a microsecond of one replaces it and keeps how
 *  that one leaves; one inside a segment splits it, so a hold stays held and a curve stays the
 *  same curve. Returns a new, sorted list. */
export function upsertKey(keys: PropertyKey[], key: PropertyKey): PropertyKey[] {
	const SAME = 1e-6;
	const sorted = keys.map(plain).sort(byTime);
	const placed = plain(key);
	const old = sorted.find((k) => Math.abs(k.time - key.time) <= SAME);
	if (old) {
		if (old.easing) placed.easing = old.easing;
		else delete placed.easing;
	} else {
		const i = sorted.findIndex((k, j) => k.time < key.time && j + 1 < sorted.length && key.time < sorted[j + 1].time);
		if (i >= 0) {
			const [a, b] = [sorted[i], sorted[i + 1]];
			const [before, after] = splitEasing(a.easing ?? 'linear', (key.time - a.time) / (b.time - a.time));
			for (const [k, e] of [[sorted[i], before], [placed, after]] as [PropertyKey, Easing][]) {
				if (e === 'linear') delete k.easing;
				else k.easing = e;
			}
		}
	}
	return [...sorted.filter((k) => Math.abs(k.time - key.time) > SAME), placed].sort(byTime);
}

/** `keys` (sorted, in range) re-timed after the clip's start moved `by` seconds **later**: the
 *  pose the head now opens on is pinned as a key at 0, the keys after it shift back, and a cut
 *  inside an eased segment keeps the rest of it exactly (`rebase_head`). */
export function rebaseHead(keys: PropertyKey[], by: number): PropertyKey[] {
	const pose = interpolate(keyPolyline(keys), by);
	if (pose === undefined) return [];
	const out: PropertyKey[] = [{ time: 0, value: pose }];
	for (let i = 0; i + 1 < keys.length; i++) {
		const [a, b] = [keys[i], keys[i + 1]];
		if (!(a.time <= by && by < b.time && b.time - a.time >= 1e-9)) continue;
		const easing = a.easing ?? 'linear';
		if (easing === 'hold') out[0].easing = 'hold';
		else if (easing !== 'linear') {
			for (let j = 1; j < EASE_STEPS; j++) {
				const u = j / EASE_STEPS;
				const at = a.time + (b.time - a.time) * u;
				if (at > by) out.push({ time: at - by, value: a.value + (b.value - a.value) * curve(easing, u) });
			}
		}
		break;
	}
	out.push(...keys.filter((k) => k.time > by).map((k) => ({ ...plain(k), time: k.time - by })));
	return out;
}

/** The channels' share of `rebaseAnimation`: the start moved `by` seconds later (the head is
 *  cut) or earlier (every key shifts later). */
export function rebaseChannels(clip: Clip, by: number) {
	if (by === 0) return;
	for (const track of clip.channels ?? []) {
		const keys = propertyKeys(clip, track.prop);
		track.keys =
			by > 0 ? rebaseHead(keys, by) : keys.map((k) => ({ ...plain(k), time: k.time - by }));
	}
}

/** Drop the tracks that say nothing: an empty one, unless it is holding a transform property off
 *  the bundle. */
export function pruneChannels(clip: Clip) {
	if (!clip.channels) return;
	const bundled = (clip.keyframes?.length ?? 0) > 0;
	clip.channels = clip.channels.filter((c) => c.keys.length > 0 || (bundled && isTransformProperty(c.prop)));
	clip.channels.sort((a, b) => PROPERTIES.indexOf(a.prop) - PROPERTIES.indexOf(b.prop));
	if (clip.channels.length === 0) delete clip.channels;
}

/** Give `prop` a track to edit: its own, else a new one holding what drives it now. */
function channelFor(clip: Clip, prop: Property): PropertyTrack {
	let track = channelOf(clip, prop);
	if (!track) {
		track = { prop, keys: propertyKeys(clip, prop).map(plain) };
		clip.channels = [...(clip.channels ?? []), track];
	}
	return track;
}

/** Replace `prop`'s keys (`Clip::set_property_keys`). */
export function setPropertyKeys(clip: Clip, prop: Property, keys: PropertyKey[]) {
	const sorted = keys.map(plain).sort(byTime);
	const track = channelOf(clip, prop);
	if (track) track.keys = sorted;
	else clip.channels = [...(clip.channels ?? []), { prop, keys: sorted }];
	pruneChannels(clip);
}

/** Put a key into `prop`'s animation (`Clip::insert_property_key`). */
export function insertPropertyKey(clip: Clip, prop: Property, key: PropertyKey) {
	const track = channelFor(clip, prop);
	track.keys = upsertKey(track.keys, key);
	pruneChannels(clip);
}

/** Set the easing of the segment leaving `prop`'s key at `time` (within a millisecond); `false`
 *  when there is no such key (`Clip::set_property_easing`). */
export function setPropertyEasing(clip: Clip, prop: Property, time: number, easing: Easing): boolean {
	if (!propertyKeys(clip, prop).some((k) => Math.abs(k.time - time) <= 1e-3)) return false;
	const track = channelFor(clip, prop);
	const near = track.keys
		.filter((k) => Math.abs(k.time - time) <= 1e-3)
		.sort((a, b) => Math.abs(a.time - time) - Math.abs(b.time - time))[0];
	if (!near) return false;
	if (easing === 'linear') delete near.easing;
	else near.easing = easing;
	return true;
}

/** Back to the static transform: the bundle and every transform number's track go. */
export function clearTransformAnimation(clip: Clip) {
	clip.keyframes = [];
	if (clip.channels) {
		clip.channels = clip.channels.filter((c) => !isTransformProperty(c.prop));
		if (clip.channels.length === 0) delete clip.channels;
	}
}

/** The keys of `props` (every keyed number when empty) as they would be on a clip whose start is
 *  `offset` seconds later than this one's (`Clip::property_keys_shifted`). */
export function propertyKeysShifted(clip: Animatable, props: Property[], offset: number): PropertyTrack[] {
	const which = props.length ? props : PROPERTIES.filter((p) => isKeyed(clip, p));
	return which.map((prop) => {
		const keys = propertyKeys(clip, prop);
		const shifted =
			keys.length === 0
				? keys
				: offset < 0
					? rebaseHead(keys, -offset)
					: keys.map((k) => ({ ...plain(k), time: k.time + offset }));
		return { prop, keys: shifted.map(plain) };
	});
}

/** What a clip's channels changed, one phrase per property (`channel_changes`). What counts is
 *  what drives the number, not whether it has a track: a bundle-animated number held static by an
 *  empty track changed (its render did), and one taken over with the keys the bundle gave it did
 *  not. */
export function channelChanges(before: Clip, after: Clip): string[] {
	const parts: string[] = [];
	for (const prop of PROPERTIES) {
		if (JSON.stringify(channelOf(before, prop)) === JSON.stringify(channelOf(after, prop))) continue;
		const a = propertyKeys(before, prop);
		const b = propertyKeys(after, prop);
		if (JSON.stringify(a) === JSON.stringify(b)) continue;
		const label = propertyLabel(prop);
		if (a.length !== b.length) {
			parts.push(`${label} keyframes ${a.length} → ${b.length}`);
			continue;
		}
		const same = (x: PropertyKey, y: PropertyKey) => x.time === y.time && x.value === y.value;
		if (a.some((x, i) => !same(x, b[i]))) parts.push(`${label} keyframes changed`);
		const eased = a.filter((x, i) => JSON.stringify(x.easing ?? 'linear') !== JSON.stringify(b[i].easing ?? 'linear')).length;
		if (eased > 0) parts.push(`easing changed on ${eased} ${label} keyframe${eased === 1 ? '' : 's'}`);
	}
	return parts;
}
