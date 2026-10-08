// Edit modes — roll, slip, slide and split-and-remove: the faithful TS mirror of
// kerf-core's `Timeline::roll_edit` / `slip_clip` / `slide_clip` / `split_remove`
// (crates/kerf-core/src/model.rs). Pure, and used twice: by the browser harness in
// `api.ts`, and by the timeline's drags, which read the `*Range` functions to hold
// the pointer where the edit would go no further and to draw the live preview — the
// desktop app commits through the backend, which applies these very rules. It is a
// *port*, not a lookalike: the bun tests replay the Rust tests case for case, and the
// error messages are the backend's own (`invalid argument: …`), because that is what
// the desktop app's rejected promise carries.
//
// The one idea: every mode moves a *boundary* — the end of one clip and the start of
// the next, or a clip's own window — and clamps rather than refuses.
//
//   roll   the cut between two adjacent clips moves; the pair's span is unchanged
//   slip   the source window moves; the clip stays put and keeps its length
//   slide  the clip moves; the clips touching it give way, its footage is unchanged
//   split  a clip is cut at a time and one half is thrown away
//
// Every function validates before it mutates, so a thrown error leaves the timeline
// exactly as it was (the harness replaces nothing on a throw). None of the first
// three ripples (the caller must not run `rippleFrom` over them); split-and-remove
// follows the project's ripple mode like any trim.

import { curve, EASE_STEPS, keyframeChannel } from './easing';
import { formatTime } from './diff';
import { invalid, locateIndex, unlockedPartners } from './link-groups';
import { toFixedEven } from './format-fixed';
import type {
	Clip,
	ClipCut,
	EditOutcome,
	Keyframe,
	Reframe,
	ReframeKeyframe,
	SplitSide,
	Timeline,
	Transform
} from './types';
import { clipDuration, DEFAULT_REFRAME, DEFAULT_TRANSFORM } from './types';

/** Shortest a clip an edit-mode op may leave behind, seconds — the edge-trim floor too. */
export const MIN_EDIT_CLIP = 0.05;

/** Two clip edges closer than this are one edge: the engine's own test for a
 *  transition partner, so a cut that blends is a cut that can be rolled. */
export const ADJACENT_EPS = 1e-3;

/** How far each asset's footage reaches, per asset id: its duration, or `Infinity`
 *  for a still (it loops, so it never runs out). */
export type SourceLimits = ReadonlyMap<string, number>;

/** How far an edit may go each way, in the units of its `delta`, and what stops it. */
export interface DeltaRange {
	/** Furthest in the negative direction (≤ 0). */
	min: number;
	/** Furthest in the positive direction (≥ 0). */
	max: number;
	whyMin: string;
	whyMax: string;
}

/** kerf-core's `DIFF_EPS`: timing closer than this is float noise from a JSON round-trip.
 *  Defined here (and re-exported by `ripple.ts`, where it always lived) so the sync lock in
 *  `ripple.ts` can use this module's clip helpers without an import cycle. */
export const DIFF_EPS = 1e-6;

const changed = (a: number, b: number) => Math.abs(a - b) > DIFF_EPS;
const speedOf = (c: Clip) => Math.max(Math.abs(c.speed ?? 1), 0.01);
const reversed = (c: Clip) => (c.speed ?? 1) < 0;
const endOf = (c: Clip) => c.timeline_start + clipDuration(c);

// ---- ranges ---------------------------------------------------------------

function openRange(): DeltaRange {
	return { min: -Infinity, max: Infinity, whyMin: '', whyMax: '' };
}

/** Cap the positive direction at `room` seconds (a negative room is none). */
function upTo(r: DeltaRange, room: number, why: string): DeltaRange {
	room = Math.max(room, 0);
	return room < r.max ? { ...r, max: room, whyMax: why } : r;
}

/** Cap the negative direction at `room` seconds (a magnitude). */
function downTo(r: DeltaRange, room: number, why: string): DeltaRange {
	const bound = -Math.max(room, 0);
	return bound > r.min ? { ...r, min: bound, whyMin: why } : r;
}

/** `delta` held inside the range; an error naming what stopped it when that leaves nothing to do. */
function resolve(r: DeltaRange, delta: number, verb: string): number {
	const applied = Math.min(Math.max(delta, r.min), r.max);
	if (Math.abs(applied) > DIFF_EPS) return applied;
	const [way, why] = delta > 0 ? ['later', r.whyMax] : ['earlier', r.whyMin];
	throw invalid(`cannot ${verb} ${way}: ${why}`);
}

/** A `delta` is a finite, non-zero number of seconds. */
function checkDelta(delta: number) {
	if (!Number.isFinite(delta)) throw invalid('delta must be a finite number of seconds');
	if (Math.abs(delta) <= DIFF_EPS) throw invalid('delta is zero — there is nothing to move');
}

export function footageOf(footage: SourceLimits, clip: Clip): number {
	const limit = footage.get(clip.asset_id);
	if (limit === undefined) throw new Error(`asset not found: ${clip.asset_id}`);
	return limit;
}

const outcome = (requested: number, applied: number, clips: Clip[]): EditOutcome => ({
	requested,
	applied,
	clamped: changed(requested, applied),
	clips
});

// ---- clip primitives ---------------------------------------------------------

/** Unused footage either side of a clip's source window, in timeline seconds, as
 *  `[head, tail]`. A reversed clip plays the window backwards, so its start is the
 *  source's *out* side and the two swap; a still has no footage to run out of. */
export function handles(clip: Clip, limit: number): [number, number] {
	if (!Number.isFinite(limit)) return [Infinity, Infinity];
	const mag = speedOf(clip);
	const before = Math.max(clip.source_in, 0) / mag;
	const after = Math.max(limit - clip.source_out, 0) / mag;
	return reversed(clip) ? [after, before] : [before, after];
}

/** Move the clip's end by `by` timeline seconds (positive lengthens). `looping` (a
 *  still) writes only the out-point whichever way it plays, so a window never goes below 0. */
export function moveTail(clip: Clip, by: number, looping: boolean) {
	const shift = by * speedOf(clip);
	if (reversed(clip) && !looping) clip.source_in -= shift;
	else clip.source_out += shift;
}

/** Move the clip's start by `by` timeline seconds — positive shortens it from the
 *  front, negative pulls it earlier and longer — so its end stays where it was. */
export function moveHead(clip: Clip, by: number, looping: boolean) {
	rebaseAnimation(clip, by);
	const shift = by * speedOf(clip);
	if (reversed(clip) || looping) clip.source_out -= shift;
	else clip.source_in += shift;
	clip.timeline_start += by;
}

/** Hold both fades inside the clip; only ever shortens one. */
export function clampFades(clip: Clip) {
	const d = clipDuration(clip);
	clip.fade_in = Math.min(clip.fade_in, d);
	clip.fade_out = Math.min(clip.fade_out, d);
}

/** The float next above / below `v` (JS has no `Math.nextUp`), for the window points an edit steps. */
function nextAfter(v: number, up: boolean): number {
	if (!Number.isFinite(v)) return v;
	if (v === 0) return up ? Number.MIN_VALUE : -Number.MIN_VALUE;
	const view = new DataView(new ArrayBuffer(8));
	view.setFloat64(0, v);
	// For a positive float the bit pattern grows with the value; for a negative one it shrinks.
	view.setBigUint64(0, view.getBigUint64(0) + (up === v > 0 ? 1n : -1n));
	return view.getFloat64(0);
}

/** Close a cut exactly (`weld` in kerf-core): when `follower` starts where `leader` ends
 *  to within float noise (`DIFF_EPS`), it is set to start *exactly* at the leader's
 *  computed end — the expression a strict overlap test compares against. A genuine gap
 *  or overlap (a cut merely within `ADJACENT_EPS`) is data and is left alone. */
function weld(leader: Clip, follower: Clip) {
	const end = endOf(leader);
	if (Math.abs(follower.timeline_start - end) <= DIFF_EPS) follower.timeline_start = end;
}

/** `weld`'s mirror for the edge the edit left where it was (`fit_end` in kerf-core):
 *  `clip`'s far end meets a clip that did not move, and the window arithmetic can leave
 *  its end a few ulps past that clip's start (`limit`). Float noise of that kind is taken
 *  back by shortening the window point the end is written on — by the overshoot, and by
 *  one ulp when that is too small to move the point — until it is no longer past. */
function fitEnd(clip: Clip, limit: number, looping: boolean) {
	for (let i = 0; i < 16; i++) {
		const over = endOf(clip) - limit;
		if (over <= 0 || over > DIFF_EPS) return;
		const by = over * speedOf(clip);
		if (reversed(clip) && !looping) {
			const moved = clip.source_in + by;
			clip.source_in = moved === clip.source_in ? nextAfter(clip.source_in, true) : moved;
		} else {
			const moved = clip.source_out - by;
			clip.source_out = moved === clip.source_out ? nextAfter(clip.source_out, false) : moved;
		}
	}
}

/** Where the first clip of lane `track` that starts at or after `end` (less the adjacency
 *  tolerance) starts, skipping the lane indices in `skip`: what an edit's far edge runs into. */
function startAfter(clips: readonly Clip[], end: number, skip: readonly number[]): number | undefined {
	let best: number | undefined;
	clips.forEach((c, i) => {
		if (skip.includes(i) || c.timeline_start < end - ADJACENT_EPS) return;
		if (best === undefined || c.timeline_start < best) best = c.timeline_start;
	});
	return best;
}

/** Re-time a clip's animation after its start moved by `by` timeline seconds:
 *  keyframes are clip-local, so they ride with the content. Shortened from the front
 *  (`by > 0`) the pose the clip now opens on is pinned as a key at 0 and later keys
 *  shift back; pulled earlier (`by < 0`) every key shifts later. */
function rebaseAnimation(clip: Clip, by: number) {
	if (by > 0) {
		if (clip.keyframes?.length) {
			const pose = transformAt(clip, by);
			const pinned: Keyframe = {
				time: 0,
				scale: pose.scale,
				pos_x: pose.pos_x,
				pos_y: pose.pos_y,
				rotation: pose.rotation,
				opacity: pose.opacity
			};
			const kfs: Keyframe[] = [pinned];
			// Cut inside an eased segment: a hold keeps holding, a curve's remaining pieces
			// become plain keys (the curve is those pieces).
			const sorted = [...clip.keyframes].sort(byTime);
			for (let i = 0; i + 1 < sorted.length; i++) {
				const [a, b] = [sorted[i], sorted[i + 1]];
				if (!(a.time < by && by < b.time && b.time - a.time >= 1e-9)) continue;
				const easing = a.easing ?? 'linear';
				if (easing === 'hold') pinned.easing = 'hold';
				else if (easing !== 'linear') {
					for (let j = 1; j < EASE_STEPS; j++) {
						const u = j / EASE_STEPS;
						const at = a.time + (b.time - a.time) * u;
						if (at <= by) continue;
						const p = curve(easing, u);
						const mix = (x: number, y: number) => x + (y - x) * p;
						kfs.push({
							time: at - by,
							scale: mix(a.scale, b.scale),
							pos_x: mix(a.pos_x, b.pos_x),
							pos_y: mix(a.pos_y, b.pos_y),
							rotation: mix(a.rotation, b.rotation),
							opacity: mix(a.opacity, b.opacity)
						});
					}
				}
				break;
			}
			kfs.push(...sorted.filter((k) => k.time > by).map((k) => ({ ...k, time: k.time - by })));
			clip.keyframes = kfs;
		}
		const rf = clip.reframe;
		if (rf?.keyframes?.length) {
			const pose = reframeAt(rf, by);
			const pinned: ReframeKeyframe = { time: 0, ...pose };
			rf.keyframes = [pinned, ...rf.keyframes.filter((k) => k.time > by).map((k) => ({ ...k, time: k.time - by }))];
		}
	} else if (by < 0) {
		for (const k of clip.keyframes ?? []) k.time -= by;
		for (const k of clip.reframe?.keyframes ?? []) k.time -= by;
	}
}

// ---- keyframe sampling (kerf-core's `interpolate` / `Clip::transform_at` / `Reframe::sample`) ----

const byTime = (a: { time: number }, b: { time: number }) => a.time - b.time;

function interpolate(points: [number, number][], at: number): number | undefined {
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

function wrap180(deg: number): number {
	if (!Number.isFinite(deg)) return 0;
	let d = (deg + 180) % 360;
	if (d < 0) d += 360;
	return d - 180;
}

function interpolateAngle(points: [number, number][], at: number): number | undefined {
	if (points.length === 0) return undefined;
	let prev = wrap180(points[0][1]);
	const unwrapped: [number, number][] = [[points[0][0], prev]];
	for (const [t, v] of points.slice(1)) {
		prev += wrap180(v - prev);
		unwrapped.push([t, prev]);
	}
	const v = interpolate(unwrapped, at);
	return v === undefined ? undefined : wrap180(v);
}

/** The clip's static transform with its animatable channels sampled at `local` seconds. */
function transformAt(clip: Clip, local: number): Transform {
	const t: Transform = { ...DEFAULT_TRANSFORM, ...(clip.transform ?? {}) };
	const ks = clip.keyframes ?? [];
	if (ks.length === 0) return t;
	const chan = (get: (k: Keyframe) => number) => interpolate(keyframeChannel(ks, get), local);
	t.scale = chan((k) => k.scale) ?? t.scale;
	t.pos_x = chan((k) => k.pos_x) ?? t.pos_x;
	t.pos_y = chan((k) => k.pos_y) ?? t.pos_y;
	t.rotation = chan((k) => k.rotation) ?? t.rotation;
	t.opacity = chan((k) => k.opacity) ?? t.opacity;
	return t;
}

const MIN_FOV = 1;
const MAX_FOV = 359;
const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

/** The virtual camera at `local` seconds: yaw / pitch / roll / fov, as a keyframe takes them. */
function reframeAt(rf: Reframe, local: number): Omit<ReframeKeyframe, 'time'> {
	const base = { ...DEFAULT_REFRAME, ...rf };
	const pose = {
		yaw: wrap180(base.yaw),
		pitch: clamp(base.pitch, -90, 90),
		roll: wrap180(base.roll),
		fov: clamp(base.fov, MIN_FOV, MAX_FOV)
	};
	const ks = [...(rf.keyframes ?? [])].sort(byTime);
	if (ks.length === 0) return pose;
	const pts = (get: (k: ReframeKeyframe) => number): [number, number][] => ks.map((k) => [k.time, get(k)]);
	pose.yaw = interpolateAngle(pts((k) => k.yaw), local) ?? pose.yaw;
	const pitch = interpolate(pts((k) => k.pitch), local);
	if (pitch !== undefined) pose.pitch = clamp(pitch, -90, 90);
	pose.roll = interpolateAngle(pts((k) => k.roll), local) ?? pose.roll;
	const fov = interpolate(pts((k) => k.fov), local);
	if (fov !== undefined) pose.fov = clamp(fov, MIN_FOV, MAX_FOV);
	return pose;
}

// ---- locating -----------------------------------------------------------------

/** Find a clip for an edit: it must exist and its track must not be locked. */
function editableClip(timeline: Timeline, clipId: string): [number, number] {
	for (let ti = 0; ti < timeline.tracks.length; ti++) {
		const ci = timeline.tracks[ti].clips.findIndex((c) => c.id === clipId);
		if (ci < 0) continue;
		if (timeline.tracks[ti].locked) throw invalid(`track ${timeline.tracks[ti].name} is locked`);
		return [ti, ci];
	}
	throw new Error(`clip not found: ${clipId}`);
}

// ---- roll ---------------------------------------------------------------------

interface RollPlan {
	ti: number;
	ia: number;
	ib: number;
	range: DeltaRange;
}

function rollPlan(timeline: Timeline, clipA: string, clipB: string, footage: SourceLimits): RollPlan {
	if (clipA === clipB) throw invalid('a cut lies between two different clips');
	const [ta, ia] = editableClip(timeline, clipA);
	const [tb, ib] = editableClip(timeline, clipB);
	if (ta !== tb)
		throw invalid(
			`clips are on different tracks (${timeline.tracks[ta].name} and ${timeline.tracks[tb].name}) — a roll moves the cut between two clips of one track`
		);
	const a = timeline.tracks[ta].clips[ia];
	const b = timeline.tracks[ta].clips[ib];
	const gap = b.timeline_start - endOf(a);
	if (Math.abs(gap) >= ADJACENT_EPS) {
		if (Math.abs(a.timeline_start - endOf(b)) < ADJACENT_EPS)
			throw invalid('clip_a must be the earlier clip: a roll moves the cut where clip_a ends and clip_b begins');
		throw invalid(
			gap > 0
				? `the clips are not adjacent — there is a ${toFixedEven(gap, 2)}s gap between them, and a roll needs a shared cut`
				: `the clips are not adjacent — they overlap by ${toFixedEven(-gap, 2)}s, and a roll needs a shared cut`
		);
	}
	const [, tailA] = handles(a, footageOf(footage, a));
	const [headB] = handles(b, footageOf(footage, b));
	let range = openRange();
	range = upTo(range, tailA, 'the outgoing clip has no footage left to extend into');
	range = upTo(range, clipDuration(b) - MIN_EDIT_CLIP, 'the incoming clip would be shorter than 0.05s');
	range = downTo(range, clipDuration(a) - MIN_EDIT_CLIP, 'the outgoing clip would be shorter than 0.05s');
	range = downTo(range, headB, 'the incoming clip has no footage left to extend into');
	return { ti: ta, ia, ib, range };
}

/** How far the cut between `clipA` and `clipB` may move each way — what `rollEdit` clamps to. */
export function rollRange(timeline: Timeline, clipA: string, clipB: string, footage: SourceLimits): DeltaRange {
	return rollPlan(timeline, clipA, clipB, footage).range;
}

/**
 * **Roll** the cut between two adjacent clips: `clipA`'s end and `clipB`'s start both
 * move by `delta` seconds (positive is later), so the pair covers the same stretch of
 * timeline and nothing after it moves. `clipA` must be the earlier clip and the two
 * must touch (within `ADJACENT_EPS`) on one unlocked track. Clamps to the footage each
 * clip has left (speed and direction honored, a still unbounded) and to `MIN_EDIT_CLIP`
 * for the clip that shrinks; errors only when the clamp leaves nothing to move.
 * `clipB`'s head moves, so its keyframes shift with the content; fades are held inside
 * a clip that shrank. Mutates `timeline`; all or nothing.
 */
export function rollEdit(
	timeline: Timeline,
	clipA: string,
	clipB: string,
	delta: number,
	footage: SourceLimits
): EditOutcome {
	checkDelta(delta);
	const { ti, ia, ib, range } = rollPlan(timeline, clipA, clipB, footage);
	const applied = resolve(range, delta, 'roll the cut');
	const track = timeline.tracks[ti];
	const a = structuredClone(track.clips[ia]);
	const b = structuredClone(track.clips[ib]);
	const [loopingA, loopingB] = [!Number.isFinite(footageOf(footage, a)), !Number.isFinite(footageOf(footage, b))];
	const far = endOf(b);
	moveTail(a, applied, loopingA);
	moveHead(b, applied, loopingB);
	weld(a, b);
	const limit = startAfter(track.clips, far, [ia, ib]);
	if (limit !== undefined) fitEnd(b, limit, loopingB);
	clampFades(a);
	clampFades(b);
	track.clips[ia] = a;
	track.clips[ib] = b;
	return outcome(delta, applied, [structuredClone(a), structuredClone(b)]);
}

// ---- slip ---------------------------------------------------------------------

function slipPlan(timeline: Timeline, clipId: string, footage: SourceLimits): [number, number, DeltaRange] {
	const [ti, ci] = editableClip(timeline, clipId);
	const clip = timeline.tracks[ti].clips[ci];
	const limit = footageOf(footage, clip);
	if (!Number.isFinite(limit)) throw invalid('a still image has no footage to slip — it looks the same at every moment');
	// Source seconds the window could move earlier / later.
	const earlier = Math.max(clip.source_in, 0);
	const later = Math.max(limit - clip.source_out, 0);
	const noLater = "there is no footage left after the clip's out-point";
	const noEarlier = "there is no footage left before the clip's in-point";
	// Positive `delta` is "starts later in its own footage" — the *lower* source time for
	// a reversed clip, so the two directions swap.
	let range = openRange();
	if (reversed(clip)) {
		range = upTo(range, earlier, noEarlier);
		range = downTo(range, later, noLater);
	} else {
		range = upTo(range, later, noLater);
		range = downTo(range, earlier, noEarlier);
	}
	return [ti, ci, range];
}

/** How far a clip's footage may slip each way, in source seconds — what `slipClip` clamps to. */
export function slipRange(timeline: Timeline, clipId: string, footage: SourceLimits): DeltaRange {
	return slipPlan(timeline, clipId, footage)[2];
}

/**
 * **Slip** a clip: show a different part of its footage without moving it or changing
 * its length. The source window shifts by `delta` **source** seconds (so at 2× speed a
 * 1 s slip moves the picture half a second). Positive means the clip starts **later in
 * its own footage**; a reversed clip is mirrored (its window moves down), so the sign
 * always means the same on screen. Clamps to the asset's footage; a still is an error.
 * Keyframes and fades are clip-local and the timing is unchanged, so they stay.
 * Mutates `timeline`; all or nothing.
 */
export function slipClip(timeline: Timeline, clipId: string, delta: number, footage: SourceLimits): EditOutcome {
	checkDelta(delta);
	const [ti, ci, range] = slipPlan(timeline, clipId, footage);
	const applied = resolve(range, delta, 'slip the footage');
	const clip = timeline.tracks[ti].clips[ci];
	const shift = reversed(clip) ? -applied : applied;
	clip.source_in += shift;
	clip.source_out += shift;
	return outcome(delta, applied, [structuredClone(clip)]);
}

// ---- slide --------------------------------------------------------------------

/** A slide's neighbour on one side: its index in the lane and whether it touches the clip. */
type Neighbour = [number, boolean] | null;

interface SlidePlan {
	ti: number;
	ci: number;
	prev: Neighbour;
	next: Neighbour;
	range: DeltaRange;
}

function slidePlan(timeline: Timeline, clipId: string, footage: SourceLimits): SlidePlan {
	const [ti, ci] = editableClip(timeline, clipId);
	const clips = timeline.tracks[ti].clips;
	const clip = clips[ci];
	// Neighbours by start time; a stable sort, as the backend's.
	const order = clips.map((_, i) => i).sort((a, b) => clips[a].timeline_start - clips[b].timeline_start);
	const pos = order.indexOf(ci);
	const prevIndex = pos > 0 ? order[pos - 1] : undefined;
	const nextIndex = pos + 1 < order.length ? order[pos + 1] : undefined;
	const prev: Neighbour =
		prevIndex === undefined ? null : [prevIndex, Math.abs(clip.timeline_start - endOf(clips[prevIndex])) < ADJACENT_EPS];
	const next: Neighbour =
		nextIndex === undefined ? null : [nextIndex, Math.abs(clips[nextIndex].timeline_start - endOf(clip)) < ADJACENT_EPS];

	let range = openRange();
	// Later: a touching previous clip grows to fill what the clip leaves, a touching next
	// clip is pushed back; a next clip across a gap is left alone.
	if (prev?.[1]) {
		const [, tail] = handles(clips[prev[0]], footageOf(footage, clips[prev[0]]));
		range = upTo(range, tail, 'the previous clip has no footage left to extend into');
	}
	if (next) {
		range = next[1]
			? upTo(range, clipDuration(clips[next[0]]) - MIN_EDIT_CLIP, 'the next clip would be shorter than 0.05s')
			: upTo(
					range,
					clips[next[0]].timeline_start - endOf(clip),
					'the next clip is not touching this one, so it is left alone and the clip can only use the free space before it'
				);
	}
	// Earlier: the mirror image.
	if (prev) {
		range = prev[1]
			? downTo(range, clipDuration(clips[prev[0]]) - MIN_EDIT_CLIP, 'the previous clip would be shorter than 0.05s')
			: downTo(
					range,
					clip.timeline_start - endOf(clips[prev[0]]),
					'the previous clip is not touching this one, so it is left alone and the clip can only use the free space after it'
				);
	} else {
		range = downTo(range, clip.timeline_start, 'the clip is already at the start of the timeline');
	}
	if (next?.[1]) {
		const [head] = handles(clips[next[0]], footageOf(footage, clips[next[0]]));
		range = downTo(range, head, 'the next clip has no footage left to extend into');
	}
	return { ti, ci, prev, next, range };
}

/** How far a clip may slide each way, in timeline seconds — what `slideClip` clamps to. */
export function slideRange(timeline: Timeline, clipId: string, footage: SourceLimits): DeltaRange {
	return slidePlan(timeline, clipId, footage).range;
}

/**
 * **Slide** a clip along its track, keeping its content: it moves by `delta` timeline
 * seconds (positive is later) and the neighbours that *touch* it give way — the previous
 * clip's end and the next clip's start both move by `delta`, so the clip's window and
 * length and the span of the three are unchanged. A neighbour across a gap is never
 * touched: the clip moves through the free space and stops where it would meet it. With
 * no previous clip it cannot go before 0; with no next clip it slides later without limit
 * and the track's end moves with it. Clamps to the neighbours' footage (a still is
 * unbounded) and `MIN_EDIT_CLIP`; errors only when nothing can move. The next clip's
 * keyframes shift with its content; the slid clip's stay. Mutates `timeline`; all or nothing.
 */
export function slideClip(timeline: Timeline, clipId: string, delta: number, footage: SourceLimits): EditOutcome {
	checkDelta(delta);
	const { ti, ci, prev, next, range } = slidePlan(timeline, clipId, footage);
	const applied = resolve(range, delta, 'slide the clip');
	const clips = timeline.tracks[ti].clips;
	// Everything that can still throw is read before anything is written.
	const moved = structuredClone(clips[ci]);
	let prevClip: Clip | null = null;
	let nextClip: Clip | null = null;
	let nextLooping = false;
	let prevLooping = false;
	if (prev?.[1]) {
		prevClip = structuredClone(clips[prev[0]]);
		prevLooping = !Number.isFinite(footageOf(footage, prevClip));
	}
	if (next?.[1]) {
		nextClip = structuredClone(clips[next[0]]);
		nextLooping = !Number.isFinite(footageOf(footage, nextClip));
	}

	// Where the edit's far edge was — the end of the last clip it changes — and which
	// clips are the edit's own, so what that edge runs into can be found.
	let far = endOf(moved);
	const skip = [ci];
	moved.timeline_start += applied;
	if (prevClip && prev) {
		moveTail(prevClip, applied, prevLooping);
		weld(prevClip, moved);
		clampFades(prevClip);
		skip.push(prev[0]);
	}
	if (nextClip && next) {
		far = endOf(nextClip);
		moveHead(nextClip, applied, nextLooping);
		weld(moved, nextClip);
		clampFades(nextClip);
		skip.push(next[0]);
	}
	const limit = startAfter(clips, far, skip);
	if (limit !== undefined) {
		if (nextClip) fitEnd(nextClip, limit, nextLooping);
		else fitEnd(moved, limit, (footage.get(moved.asset_id) ?? 0) === Infinity);
	}

	const out: Clip[] = [];
	if (prevClip && prev) {
		clips[prev[0]] = prevClip;
		out.push(structuredClone(prevClip));
	}
	clips[ci] = moved;
	out.push(structuredClone(moved));
	if (nextClip && next) {
		clips[next[0]] = nextClip;
		out.push(structuredClone(nextClip));
	}
	return outcome(delta, applied, out);
}

// ---- split and remove ----------------------------------------------------------

/**
 * **Split and remove**: cut a clip at timeline time `at` and throw one half away.
 * Returns the half that stays, which keeps the clip's id. `'right'` shortens the clip at
 * its end; `'left'` moves its start up to `at` so its end stays put (leaving the gap
 * where the removed half was — under ripple mode `rippleFrom` closes it, holding the
 * clip's start). What belonged to the removed half goes with it: removing the left drops
 * `fade_in` and `transition_in`, removing the right drops `fade_out`; the other fade is
 * held inside what is left, and keyframes ride with the content when the head moved.
 * `at` must lie inside the clip and leave at least `MIN_EDIT_CLIP` of it. Mutates
 * `timeline`; all or nothing.
 */
export function splitRemove(timeline: Timeline, clipId: string, at: number, side: SplitSide): Clip {
	if (!Number.isFinite(at)) throw invalid('the split point must be a finite time');
	const [ti, ci] = editableClip(timeline, clipId);
	const clip = structuredClone(timeline.tracks[ti].clips[ci]);
	const start = clip.timeline_start;
	const end = endOf(clip);
	if (at <= start + DIFF_EPS || at >= end - DIFF_EPS)
		throw invalid(`the split point ${formatTime(at)} is not inside the clip (${formatTime(start)}–${formatTime(end)})`);
	const kept = side === 'left' ? end - at : at - start;
	if (kept < MIN_EDIT_CLIP - DIFF_EPS)
		throw invalid(`that would leave only ${toFixedEven(kept, 2)}s of the clip — remove the clip instead`);
	if (side === 'left') {
		moveHead(clip, at - start, false);
		clip.timeline_start = at;
		clip.fade_in = 0;
		clip.transition_in = null;
	} else {
		moveTail(clip, at - end, false);
		clip.fade_out = 0;
	}
	clampFades(clip);
	timeline.tracks[ti].clips[ci] = clip;
	return structuredClone(clip);
}

/**
 * **Split and remove** on several clips as one edit (`Timeline::split_remove_clips`): the
 * playhead trim of a selection — a picture and its sound together. Every cut is
 * `splitRemove` with the same `side`; the survivors come back in request order. All or
 * nothing: an unknown clip, a locked track, a cut outside its clip or one that would leave
 * under `MIN_EDIT_CLIP` refuses the group and the timeline is untouched. At most one clip
 * per track, and a clip once: under ripple a lane trimmed at two places has no single edit
 * point to hold still, so each track ripples on its own, from its own one cut. Mutates
 * `timeline`.
 */
export function splitRemoveClips(timeline: Timeline, cuts: readonly ClipCut[], side: SplitSide): Clip[] {
	if (cuts.length === 0) throw invalid('no clips to cut');
	const seenClips = new Set<string>();
	const seenTracks = new Set<number>();
	for (const cut of cuts) {
		const ti = timeline.tracks.findIndex((t) => t.clips.some((c) => c.id === cut.clip_id));
		if (ti < 0) throw new Error(`clip not found: ${cut.clip_id}`);
		if (seenClips.has(cut.clip_id)) throw invalid(`clip ${cut.clip_id} appears more than once`);
		seenClips.add(cut.clip_id);
		if (seenTracks.has(ti))
			throw invalid(`two of the clips are on track ${timeline.tracks[ti].name} — a group trim cuts one clip per track`);
		seenTracks.add(ti);
	}
	// Cut a copy, so a refusal part-way through leaves the timeline untouched.
	const scratch: Timeline = structuredClone(timeline);
	const kept = cuts.map((cut) => splitRemove(scratch, cut.clip_id, cut.at, side));
	timeline.tracks = scratch.tracks;
	return kept;
}

// ---- linked edits ----------------------------------------------------------------
// The group versions of roll, slip and slide — the port of `roll_edit_linked` /
// `slip_clip_linked` / `slide_clip_linked` in kerf-core's `model/links.rs`. Each applies
// the same edit to the linked partners and clamps the whole group to its tightest
// member; a partner on a locked track refuses the lot. With no partners to carry they
// are exactly the plain edit.

/** The range both allow: the tighter bound each way, with the reason it came from. */
function intersect(a: DeltaRange, b: DeltaRange): DeltaRange {
	const out = { ...a };
	if (b.min > out.min) {
		out.min = b.min;
		out.whyMin = b.whyMin;
	}
	if (b.max < out.max) {
		out.max = b.max;
		out.whyMax = b.whyMax;
	}
	return out;
}

/** The same range in units `k` times as large (`k > 0`). */
function scaled(r: DeltaRange, k: number): DeltaRange {
	return { ...r, min: r.min * k, max: r.max * k };
}

/** The reasons, prefixed with the track of the linked clip they belong to. */
function onLinked(r: DeltaRange, track: string): DeltaRange {
	const why = (w: string) => (w ? `linked clip on ${track}: ${w}` : w);
	return { ...r, whyMin: why(r.whyMin), whyMax: why(r.whyMax) };
}

const trackNameOf = (timeline: Timeline, clipId: string): string => {
	const at = locateIndex(timeline, clipId);
	return at ? timeline.tracks[at[0]].name : '';
};

/** The pairs of partners a roll of the cut between `clipA` and `clipB` also rolls: a
 *  partner of `clipA` and one of `clipB` that touch on one track with the first earlier. */
function rollPartnerPairs(timeline: Timeline, clipA: string, clipB: string, footage: SourceLimits): [string, string][] {
	const named = new Set([clipA, clipB]);
	const pa = unlockedPartners(timeline, clipA, named);
	const pb = unlockedPartners(timeline, clipB, named);
	const pairs: [string, string][] = [];
	for (const a of pa) {
		for (const b of pb) {
			if (a === b) continue;
			try {
				rollPlan(timeline, a, b, footage);
				pairs.push([a, b]);
			} catch {
				// not a pair: no shared cut
			}
		}
	}
	return pairs;
}

/** `rollRange` for the group: the roll's range intersected with each partner pair's. */
export function rollRangeLinked(timeline: Timeline, clipA: string, clipB: string, footage: SourceLimits): DeltaRange {
	let range = rollRange(timeline, clipA, clipB, footage);
	for (const [a, b] of rollPartnerPairs(timeline, clipA, clipB, footage))
		range = intersect(range, onLinked(rollRange(timeline, a, b, footage), trackNameOf(timeline, a)));
	return range;
}

/** **Roll** the cut and the cut of each linked partner pair sharing it, all by `delta`,
 *  clamped to the tightest pair. Mutates `timeline`; all or nothing. */
export function rollEditLinked(
	timeline: Timeline,
	clipA: string,
	clipB: string,
	delta: number,
	footage: SourceLimits
): EditOutcome {
	checkDelta(delta);
	const pairs = rollPartnerPairs(timeline, clipA, clipB, footage);
	if (pairs.length === 0) return rollEdit(timeline, clipA, clipB, delta, footage);
	const applied = resolve(rollRangeLinked(timeline, clipA, clipB, footage), delta, 'roll the cut');
	const scratch: Timeline = structuredClone(timeline);
	const clips = rollEdit(scratch, clipA, clipB, applied, footage).clips;
	for (const [a, b] of pairs) clips.push(...rollEdit(scratch, a, b, applied, footage).clips);
	timeline.tracks = scratch.tracks;
	return outcome(delta, applied, clips);
}

/** Partners a slip also slips: every linked partner but a still (no footage to slip). */
function slipPartners(timeline: Timeline, clipId: string, footage: SourceLimits): string[] {
	const out: string[] = [];
	for (const partner of unlockedPartners(timeline, clipId, new Set([clipId]))) {
		const [ti, ci] = locateIndex(timeline, partner)!;
		if (!Number.isFinite(footageOf(footage, timeline.tracks[ti].clips[ci]))) continue;
		out.push(partner);
	}
	return out;
}

/** `slipRange` for the group, in the named clip's source seconds: a partner's range is
 *  converted by the ratio of the two speeds, then intersected. */
export function slipRangeLinked(timeline: Timeline, clipId: string, footage: SourceLimits): DeltaRange {
	let range = slipRange(timeline, clipId, footage);
	const mag = speedOf(clipAt(timeline, clipId));
	for (const partner of slipPartners(timeline, clipId, footage)) {
		const theirs = slipRange(timeline, partner, footage);
		range = intersect(range, onLinked(scaled(theirs, mag / speedOf(clipAt(timeline, partner))), trackNameOf(timeline, partner)));
	}
	return range;
}

function clipAt(timeline: Timeline, clipId: string): Clip {
	const [ti, ci] = locateIndex(timeline, clipId)!;
	return timeline.tracks[ti].clips[ci];
}

/** **Slip** the clip and its linked partners by the same timeline shift of the footage,
 *  clamped to the tightest member. `delta` is in the named clip's source seconds. */
export function slipClipLinked(timeline: Timeline, clipId: string, delta: number, footage: SourceLimits): EditOutcome {
	checkDelta(delta);
	const partners = slipPartners(timeline, clipId, footage);
	if (partners.length === 0) return slipClip(timeline, clipId, delta, footage);
	const applied = resolve(slipRangeLinked(timeline, clipId, footage), delta, 'slip the footage');
	const mag = speedOf(clipAt(timeline, clipId));
	const scratch: Timeline = structuredClone(timeline);
	const clips = slipClip(scratch, clipId, applied, footage).clips;
	for (const partner of partners) {
		const theirs = (applied * speedOf(clipAt(scratch, partner))) / mag;
		if (Math.abs(theirs) > DIFF_EPS) clips.push(...slipClip(scratch, partner, theirs, footage).clips);
	}
	timeline.tracks = scratch.tracks;
	return outcome(delta, applied, clips);
}

/** `slideRange` for the group: the slide's range intersected with each partner's. */
export function slideRangeLinked(timeline: Timeline, clipId: string, footage: SourceLimits): DeltaRange {
	let range = slideRange(timeline, clipId, footage);
	for (const partner of unlockedPartners(timeline, clipId, new Set([clipId])))
		range = intersect(range, onLinked(slideRange(timeline, partner, footage), trackNameOf(timeline, partner)));
	return range;
}

/** **Slide** the clip and each linked partner by the same `delta`, every one's touching
 *  neighbours giving way on its own track, clamped to the tightest. */
export function slideClipLinked(timeline: Timeline, clipId: string, delta: number, footage: SourceLimits): EditOutcome {
	checkDelta(delta);
	const partners = unlockedPartners(timeline, clipId, new Set([clipId]));
	if (partners.length === 0) return slideClip(timeline, clipId, delta, footage);
	const applied = resolve(slideRangeLinked(timeline, clipId, footage), delta, 'slide the clip');
	const scratch: Timeline = structuredClone(timeline);
	const clips = slideClip(scratch, clipId, applied, footage).clips;
	for (const partner of partners) clips.push(...slideClip(scratch, partner, applied, footage).clips);
	timeline.tracks = scratch.tracks;
	return outcome(delta, applied, clips);
}
