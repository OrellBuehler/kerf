// Speed ramps — the faithful TS mirror of kerf-core's `model/speed_ramp.rs`
// (crates/kerf-core/src/model/speed_ramp.rs) and of the clip's time map built on it.
//
// A clip's `speed` is one rate for its whole length. A ramp (`Clip.speed_ramp`) is a list of keys —
// a clip-local TIMELINE time, a speed magnitude and the easing of the segment that leaves it — and
// while the clip holds a well-formed one it decides the rate everywhere: before the first key the
// first key's speed, after the last the last's, between two keys a straight line in timeline time
// (`linear`) or the left key's speed (`hold`). The direction is still `speed`'s sign.
//
// **One map.** `along(clip, local)` (timeline seconds into the clip -> source seconds travelled) and
// its inverse `localAtAlong`; `clipDuration`, `sourceToTimeline` and `timelineToSource` read them, and
// so must anything that converts between the two clocks: `speedMag` is WRONG for a ramped clip. A clip
// without a ramp keeps the plain `* speed` / `/ speed` it always had, to the last bit.
//
// A ramp that is not well formed (`rampIsValid`) is not read: the clip plays at its constant speed.

import type { Clip, SpeedKey } from './types';

/** Smallest speed magnitude a clip may be set to (`MIN_SPEED`, model.rs). */
export const MIN_SPEED = 0.01;

/** Largest speed magnitude a clip may be set to (`MAX_SPEED`, model.rs): 100x. */
export const MAX_SPEED = 100;

/** The most keys one clip's ramp may hold (`MAX_SPEED_KEYS`): the export nests one `if` per key. */
export const MAX_SPEED_KEYS = 16;

/** Whether `keys` is a ramp the map can read: one to `MAX_SPEED_KEYS` keys, finite strictly increasing
 *  times, finite speeds in `MIN_SPEED..=MAX_SPEED` (`ramp_is_valid`). */
export function rampIsValid(keys: readonly SpeedKey[] | null | undefined): boolean {
	if (!keys || keys.length === 0 || keys.length > MAX_SPEED_KEYS) return false;
	for (let i = 0; i < keys.length; i++) {
		const k = keys[i];
		if (!Number.isFinite(k.time) || !Number.isFinite(k.speed) || k.speed < MIN_SPEED || k.speed > MAX_SPEED) return false;
		if (i > 0 && !(keys[i - 1].time < k.time)) return false;
	}
	return true;
}

const holds = (k: SpeedKey) => k.easing === 'hold';

/** Speed magnitude of a valid ramp at clip-local time `t` (`ramp_speed_at`). */
export function rampSpeedAt(keys: readonly SpeedKey[], t: number): number {
	const first = keys[0];
	const last = keys[keys.length - 1];
	if (t <= first.time) return first.speed;
	if (t >= last.time) return last.speed;
	let i = 0;
	while (i + 1 < keys.length && keys[i + 1].time <= t) i++;
	const [a, b] = [keys[i], keys[i + 1]];
	return holds(a) ? a.speed : a.speed + ((b.speed - a.speed) * (t - a.time)) / (b.time - a.time);
}

/** Source seconds a segment leaving `a` covers in its first `dt` timeline seconds (`dt` within it). */
function segmentDistance(a: SpeedKey, b: SpeedKey, dt: number): number {
	return holds(a) ? a.speed * dt : a.speed * dt + ((b.speed - a.speed) * dt * dt) / (2 * (b.time - a.time));
}

/** Timeline seconds a segment leaving `a` takes to cover `r` source seconds (`r` within it). */
function segmentTime(a: SpeedKey, b: SpeedKey, r: number): number {
	if (holds(a)) return r / a.speed;
	// `k/2 dt^2 + v dt = r`, solved in the form that does not cancel when `k` is near 0.
	const k = (b.speed - a.speed) / (b.time - a.time);
	const disc = Math.max(a.speed * a.speed + 2 * k * r, 0);
	return (2 * r) / (a.speed + Math.sqrt(disc));
}

/** Source seconds a valid ramp has covered at clip-local time `t`, measured from its first key's
 *  time (negative before it). */
function distanceFromFirst(keys: readonly SpeedKey[], t: number): number {
	const first = keys[0];
	if (t <= first.time) return (t - first.time) * first.speed;
	let acc = 0;
	for (let i = 0; i + 1 < keys.length; i++) {
		const [a, b] = [keys[i], keys[i + 1]];
		if (t <= b.time) return acc + segmentDistance(a, b, t - a.time);
		acc += segmentDistance(a, b, b.time - a.time);
	}
	const last = keys[keys.length - 1];
	return acc + (t - last.time) * last.speed;
}

/** The inverse of `distanceFromFirst`: the clip-local time at which a valid ramp has covered `d`
 *  source seconds since its first key. */
function timeFromFirst(keys: readonly SpeedKey[], d: number): number {
	const first = keys[0];
	if (d <= 0) return first.time + d / first.speed;
	let acc = 0;
	for (let i = 0; i + 1 < keys.length; i++) {
		const [a, b] = [keys[i], keys[i + 1]];
		const span = segmentDistance(a, b, b.time - a.time);
		if (d <= acc + span) return a.time + Math.min(segmentTime(a, b, d - acc), b.time - a.time);
		acc += span;
	}
	const last = keys[keys.length - 1];
	return last.time + (d - acc) / last.speed;
}

/** Source seconds a valid ramp covers from the clip's start to clip-local time `t` (negative for
 *  `t < 0`, extrapolated at the first key's speed) (`ramp_along`). */
export function rampAlong(keys: readonly SpeedKey[], t: number): number {
	return distanceFromFirst(keys, t) - distanceFromFirst(keys, 0);
}

/** The clip-local time at which a valid ramp has covered `along` source seconds from the clip's
 *  start — the inverse of `rampAlong` (`ramp_local_at`). */
export function rampLocalAt(keys: readonly SpeedKey[], along: number): number {
	return timeFromFirst(keys, along + distanceFromFirst(keys, 0));
}

// ---- the clip's map ---------------------------------------------------------------

/** What the map reads of a clip. */
export type Speeding = Pick<Clip, 'speed' | 'speed_ramp'>;
export type Windowed = Speeding & Pick<Clip, 'source_in' | 'source_out'>;

/** Speed magnitude, clamped away from zero (direction dropped). A ramped clip's rate varies: convert
 *  between the clocks with `along` / `localAtAlong` instead (`Clip::speed_mag`). */
export const speedMag = (c: Pick<Clip, 'speed'>): number => Math.max(Math.abs(c.speed ?? 1), MIN_SPEED);

/** Whether the clip plays its source in reverse (`Clip::is_reversed`). */
export const isReversed = (c: Pick<Clip, 'speed'>): boolean => (c.speed ?? 1) < 0;

/** The clip's ramp, when it holds one the map reads (`Clip::ramp`). */
export function clipRamp(c: Pick<Clip, 'speed_ramp'>): readonly SpeedKey[] | null {
	return rampIsValid(c.speed_ramp) ? c.speed_ramp! : null;
}

/** Whether the clip's rate changes over its length (`Clip::is_ramped`). */
export const isRamped = (c: Pick<Clip, 'speed_ramp'>): boolean => clipRamp(c) !== null;

/** Speed magnitude at clip-local time `local`: the ramp's there, else the constant one (`Clip::speed_at`). */
export function speedAt(c: Speeding, local: number): number {
	const keys = clipRamp(c);
	return keys ? rampSpeedAt(keys, local) : speedMag(c);
}

/** Source seconds the clip travels (in its playing direction) from its start to clip-local time
 *  `local`. Not clamped to the clip (`Clip::along`). */
export function along(c: Speeding, local: number): number {
	const keys = clipRamp(c);
	return keys ? rampAlong(keys, local) : local * speedMag(c);
}

/** The clip-local time at which the clip has travelled `a` source seconds from its start — the
 *  inverse of `along`. Not clamped to the clip (`Clip::local_at_along`). */
export function localAtAlong(c: Speeding, a: number): number {
	const keys = clipRamp(c);
	return keys ? rampLocalAt(keys, a) : a / speedMag(c);
}

/** The slowest the clip ever plays: its ramp's lowest key, else its constant speed (a level of detail to draw it at). */
export function slowestSpeed(c: Speeding): number {
	const keys = clipRamp(c);
	return keys ? Math.min(...keys.map((k) => k.speed)) : speedMag(c);
}

/** Length of the referenced source span (seconds), ignoring speed (`Clip::source_duration`). */
export const sourceDuration = (c: Pick<Clip, 'source_in' | 'source_out'>): number => Math.max(0, c.source_out - c.source_in);

/** Duration on the timeline (seconds): the source span retimed by speed, or its ramp (`Clip::duration`). */
export const clipDuration = (c: Windowed): number => localAtAlong(c, sourceDuration(c));

/** The clip's source span over its timeline length: its average speed magnitude (`Clip::mean_speed`). */
export function meanSpeed(c: Windowed): number {
	const keys = clipRamp(c);
	if (!keys) return speedMag(c);
	const d = clipDuration(c);
	return d > 0 ? sourceDuration(c) / d : rampSpeedAt(keys, 0);
}

/** Where a source timestamp of the clip lands on the timeline (`Clip::source_to_timeline`). Honors
 *  speed, its ramp and reverse, so an analysis marker maps to the moment it is actually heard. */
export function sourceToTimeline(c: Windowed & Pick<Clip, 'timeline_start'>, source: number): number {
	const offset = isReversed(c) ? c.source_out - source : source - c.source_in;
	return c.timeline_start + localAtAlong(c, offset);
}

/** The source timestamp playing at timeline time `time` — the inverse of `sourceToTimeline`
 *  (`Clip::timeline_to_source`). Not clamped to the clip's window. */
export function timelineToSource(c: Windowed & Pick<Clip, 'timeline_start'>, time: number): number {
	const a = along(c, time - c.timeline_start);
	return isReversed(c) ? c.source_out - a : c.source_in + a;
}

/** Timeline seconds the `src` source seconds just before the clip's first frame take to play at the
 *  clip's head (`src` of footage a head trim could still pull out). The plain `/ speed` unramped. */
export function headTime(c: Speeding, src: number): number {
	const keys = clipRamp(c);
	return keys ? -rampLocalAt(keys, -src) : src / speedMag(c);
}

/** Timeline seconds the `src` source seconds just after the clip's last frame take to play at its
 *  tail (what a tail trim could still pull out). The plain `/ speed` unramped. */
export function tailTime(c: Windowed, src: number): number {
	const keys = clipRamp(c);
	if (!keys) return src / speedMag(c);
	const d = sourceDuration(c);
	return rampLocalAt(keys, d + src) - rampLocalAt(keys, d);
}

/** Source seconds the clip's last frame covers (`speed / fps` unramped): the span the ramp runs through in the
 *  frame before the clip's end. */
export function lastFrameSpan(c: Windowed, fps: number): number {
	const keys = clipRamp(c);
	return keys ? sourceDuration(c) - rampAlong(keys, clipDuration(c) - 1 / fps) : speedMag(c) / fps;
}

/** Whether two clips run the same ramp (or neither runs one), keys within `eps` of each other. */
export function sameRamp(a: Pick<Clip, 'speed_ramp'>, b: Pick<Clip, 'speed_ramp'>, eps: number): boolean {
	const [ka, kb] = [clipRamp(a), clipRamp(b)];
	if (!ka || !kb) return !ka && !kb;
	return (
		ka.length === kb.length &&
		ka.every(
			(k, i) =>
				Math.abs(k.time - kb[i].time) < eps && Math.abs(k.speed - kb[i].speed) < eps && holds(k) === holds(kb[i])
		)
	);
}

/** Source seconds the clip's window moves when its start moves `by` timeline seconds (positive
 *  shortens it): what `Clip::move_head` writes, `by * speed` unramped. Read BEFORE `rebaseRamp`. */
export const headSource = (c: Speeding, by: number): number => along(c, by);

/** Source seconds the clip's window moves when its end moves `by` timeline seconds (positive
 *  lengthens it): what `Clip::move_tail` writes, `by * speed` unramped. */
export function tailSource(c: Windowed, by: number): number {
	const keys = clipRamp(c);
	if (!keys) return by * speedMag(c);
	const d = clipDuration(c);
	return rampAlong(keys, d + by) - rampAlong(keys, d);
}

/** Re-time a clip's ramp after its start moved by `by` timeline seconds, so it rides with the content
 *  like keyframes do (`Clip::rebase_ramp`). Shortened from the front (`by > 0`) the speed the clip now
 *  opens on is pinned as a key at 0 — holding, when `by` fell inside a hold segment — the keys at or
 *  before `by` go and the rest shift back; pulled earlier (`by < 0`) every key shifts later, and the new
 *  head runs at the first key's speed, which is what the map does before the first key. A ramp the map
 *  does not read is left alone. */
export function rebaseRamp(c: Pick<Clip, 'speed_ramp'>, by: number) {
	const keys = clipRamp(c);
	if (!keys || by === 0) return;
	if (by < 0) {
		c.speed_ramp = keys.map((k) => ({ ...k, time: k.time - by }));
		return;
	}
	const rest = keys.filter((k) => k.time > by).map((k) => ({ ...k, time: k.time - by }));
	let at = -1;
	while (at + 1 < keys.length && keys[at + 1].time <= by) at++;
	if (at < 0) {
		c.speed_ramp = rest;
		return;
	}
	const pinned: SpeedKey = { time: 0, speed: rampSpeedAt(keys, by) };
	if (at + 1 < keys.length && holds(keys[at])) pinned.easing = 'hold';
	c.speed_ramp = [pinned, ...rest];
}

/** What a trim does to a ramp (`Project::trim`): the clip's head moved when its window's start side did, and
 *  the ramp rides with the content — rebased by the timeline seconds the footage given up (or taken on) took on
 *  the curve `was` had — so the clip's tail stays where the trim put it. `now` is `was` with its window written. */
export function trimRamp(was: Windowed, now: Windowed) {
	if (!isRamped(was)) return;
	const moved = isReversed(was) ? was.source_out - now.source_out : now.source_in - was.source_in;
	if (moved !== 0) rebaseRamp(now, localAtAlong(was, moved));
}

/** One event of a playback-rate automation: `speed` is held from `time` (`step`), or reached by a straight
 *  line from the previous event. */
export interface RatePoint {
	time: number;
	speed: number;
	step: boolean;
}

/**
 * The speed of a ramp across clip-local times `[a, b]` as an `AudioParam`'s events, in the order a player
 * meets them: ascending, or descending when it plays `back`. The first point is where it starts (a `step`);
 * a linear segment is a ramp to the next key, a hold a step where the next key begins. A player running the
 * source at this rate covers exactly `rampAlong`'s distance, which is what keeps the preview's sound on the
 * picture.
 */
export function rateAutomation(keys: readonly SpeedKey[], a: number, b: number, back = false): RatePoint[] {
	type Seg = { t0: number; t1: number; v0: number; v1: number; hold: boolean };
	const first = keys[0];
	const last = keys[keys.length - 1];
	const segs: Seg[] = [{ t0: -Infinity, t1: first.time, v0: first.speed, v1: first.speed, hold: false }];
	for (let i = 0; i + 1 < keys.length; i++) {
		const [k, n] = [keys[i], keys[i + 1]];
		segs.push({ t0: k.time, t1: n.time, v0: k.speed, v1: n.speed, hold: holds(k) });
	}
	segs.push({ t0: last.time, t1: Infinity, v0: last.speed, v1: last.speed, hold: false });
	const valueAt = (s: Seg, t: number) => (s.v0 === s.v1 ? s.v0 : s.v0 + ((s.v1 - s.v0) * (t - s.t0)) / (s.t1 - s.t0));
	const out: RatePoint[] = [{ time: back ? b : a, speed: rampSpeedAt(keys, back ? b : a), step: true }];
	for (const s of back ? [...segs].reverse() : segs) {
		if (s.t1 <= a || s.t0 >= b) continue;
		if (!back) {
			if (!s.hold) out.push({ time: Math.min(s.t1, b), speed: valueAt(s, Math.min(s.t1, b)), step: false });
			else if (s.t1 < b) out.push({ time: s.t1, speed: s.v1, step: true });
		} else if (!s.hold) {
			out.push({ time: Math.max(s.t0, a), speed: valueAt(s, Math.max(s.t0, a)), step: false });
		} else {
			if (s.t1 < b) out.push({ time: s.t1, speed: s.v0, step: true });
			out.push({ time: Math.max(s.t0, a), speed: s.v0, step: false });
		}
	}
	return out;
}
