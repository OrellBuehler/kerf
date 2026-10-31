/**
 * The clip-level half of the live behaviours (B8b): the faithful TS mirror of `Clip::bake_behaviors` /
 * `Clip::set_behaviors` (`model/behavior.rs`) and of `Project::set_behaviors` / `Project::bake_behaviors`
 * (`project/behavior.rs`) — what the browser harness runs when there is no backend. The arithmetic and the limits
 * are `behaviors.ts`'s; the resolver they sit on is `channels.ts`'s (`propertyAt` is the picture, `baseAt` the pose
 * without behaviours). `behaviors.test.ts` replays kerf-core's corpus over both.
 */
import { MAX_CHANNEL_KEYS, TRANSFORM_PROPERTIES, behaves, propertyAt, propertyKeysProblem, setPropertyKeys } from './channels';
import { behaviorsProblem, full } from './behaviors';
import { clipNotFound, invalid } from './link-groups';
import { clipDuration } from './speed-ramp';
import type { Edited } from './link-ops';
import type { Asset, Baked, BakedClip, Behavior, Clip, Property, PropertyKey, Timeline } from './types';

/** The frame rate a bake samples at when none is named and nothing else says (`DEFAULT_BAKE_FPS`). */
export const DEFAULT_BAKE_FPS = 30;

/** A number as Rust's `Display` writes it (what the refusals quote). */
function display(n: number): string {
	if (Number.isNaN(n)) return 'NaN';
	if (!Number.isFinite(n)) return n < 0 ? '-inf' : 'inf';
	return String(n);
}

/** `x` to one decimal as Rust's `{:.1}` writes it: an exact tie (only ever `.25`) goes to the even digit, where
 *  `toFixed` goes up. */
function oneDecimal(x: number): string {
	const tie = x > 0 && Number.isInteger(x * 4) && !Number.isInteger(x * 2);
	return tie && Math.floor(x * 10) % 2 === 0 ? (Math.floor(x * 10) / 10).toFixed(1) : x.toFixed(1);
}

/** Sample the behaviours' combined result into keys of the numbers they move, at the output frames of a `fps` grid,
 *  and clear them (`Clip::bake_behaviors`). **Exact at those frames**: the keys sit on the grid's frame times
 *  (clip-local `k / fps - timeline_start`, plus the clip's first and last moment), linear between, and the base
 *  pose (its keys, eased or held) is folded in the same way. Refuses a clip without behaviours, an `fps` outside
 *  1..=1000, and one that would need more than `maxKeys` keys for a number. `duration` is the clip's length on the
 *  timeline (the clip's own by default). Mutates `clip`; a refusal leaves it as it was. */
export function bakeBehaviors(clip: Clip, fps: number, maxKeys: number = MAX_CHANNEL_KEYS, duration: number = clipDuration(clip)): Baked {
	if (!clip.behaviors?.length) throw invalid('the clip has no behaviours to bake');
	if (!Number.isFinite(fps) || fps < 1 || fps > 1000) throw invalid(`bake at a frame rate within 1..=1000 (got ${display(fps)})`);
	const driven: Property[] = TRANSFORM_PROPERTIES.filter((p) => behaves(clip, p));
	if (driven.length === 0) {
		delete clip.behaviors;
		return { properties: driven, keys: 0, fps };
	}
	const start = clip.timeline_start;
	if (!Number.isFinite(duration) || !Number.isFinite(start) || duration <= 0) {
		throw invalid('the clip has no length to bake behaviours over');
	}
	const frames = Math.ceil(duration * fps);
	if (frames + 2 > maxKeys) {
		throw invalid(
			`baking ${oneDecimal(duration)} s at ${display(fps)} fps needs about ${frames + 2} keys a number and a clip holds at most ${maxKeys}: ` +
				`split the clip into pieces under ${oneDecimal(Math.max(maxKeys - 2, 0) / fps)} s, or keep the behaviour live`
		);
	}
	const times = [0];
	const first = Math.ceil(start * fps - 1e-9);
	for (let k = first; k <= first + frames + 1; k++) {
		const local = k / fps - start;
		if (local >= duration - 1e-9) break;
		if (local > 1e-9) times.push(local);
	}
	times.push(Math.max(duration, 1e-9));
	const tracks = driven.map((p): [Property, PropertyKey[]] => [p, times.map((t) => ({ time: t, value: propertyAt(clip, p, t) }))]);
	for (const [prop, keys] of tracks) setPropertyKeys(clip, prop, keys);
	delete clip.behaviors;
	return { properties: driven, keys: times.length, fps };
}

/** `ids` without repeats; at least one (`distinct`). */
function distinct(ids: readonly string[]): string[] {
	const out = ids.filter((id, i) => ids.indexOf(id) === i);
	if (out.length === 0) throw invalid('name at least one clip');
	return out;
}

/** The clip `id`, which must have a picture to move (`picture_clip`). */
function pictureClip(tl: Timeline, id: string): Clip {
	for (const track of tl.tracks) {
		const clip = track.clips.find((c) => c.id === id);
		if (!clip) continue;
		if (track.kind === 'audio') throw invalid(`clip ${id} is on audio track ${track.name}: a sound has no picture to move`);
		return clip;
	}
	throw clipNotFound(id);
}

/** The revision label `Project::set_behaviors` records. */
export function setBehaviorsLabel(clips: number, cleared: boolean): string {
	const verb = cleared ? 'Clear behaviours' : 'Set behaviours';
	return clips === 1 ? verb : `${verb} (${clips} clips)`;
}

/** The revision label `Project::bake_behaviors` records. */
export function bakeBehaviorsLabel(clips: number): string {
	return clips === 1 ? 'Bake behaviours' : `Bake behaviours (${clips} clips)`;
}

/** `Project::set_behaviors` on a timeline: each of `clipIds` (a clip named twice once) gets `behaviors` as its whole
 *  list — an empty list clears them — as one edit, all or nothing. The timeline returned is a copy, the one given
 *  is untouched. A clip on an audio track refuses the lot; a locked track does not. */
export function setBehaviors(timeline: Timeline, clipIds: readonly string[], behaviors: readonly Behavior[]): Edited<Clip[]> {
	const ids = distinct(clipIds);
	const problem = behaviorsProblem(behaviors);
	if (problem) throw invalid(problem);
	const tl = structuredClone(timeline);
	const done: Clip[] = [];
	for (const id of ids) {
		const clip = pictureClip(tl, id);
		if (behaviors.length) clip.behaviors = behaviors.map(full);
		else delete clip.behaviors;
		done.push(structuredClone(clip));
	}
	return { timeline: tl, result: done, label: setBehaviorsLabel(ids.length, behaviors.length === 0) };
}

/** The rate a bake samples at when it is not told one (`default_bake_fps`): the delivery's, else the first picture's
 *  footage, else 30. */
export function defaultBakeFps(timeline: Timeline, assets: readonly Asset[]): number {
	if (timeline.format?.fps) return timeline.format.fps;
	for (const track of timeline.tracks) {
		if (track.kind !== 'video') continue;
		for (const clip of track.clips) {
			const fps = assets.find((a) => a.id === clip.asset_id)?.streams.find((s) => s.kind === 'video')?.fps;
			if (fps !== undefined && fps !== null && Number.isFinite(fps) && fps > 0) return fps;
		}
	}
	return DEFAULT_BAKE_FPS;
}

/** `Project::bake_behaviors` on a timeline: each of `clipIds` baked into keys (`bakeBehaviors`) as one edit, all or
 *  nothing; `fps` defaults to `defaultBakeFps`. The timeline returned is a copy. A clip with no behaviours, or one a
 *  bake would put too many keys on, refuses the whole edit and says which. */
export function bakeBehaviorsOn(
	timeline: Timeline,
	assets: readonly Asset[],
	clipIds: readonly string[],
	fps?: number | null
): Edited<BakedClip[]> {
	const ids = distinct(clipIds);
	const rate = fps ?? defaultBakeFps(timeline, assets);
	const tl = structuredClone(timeline);
	const done: BakedClip[] = [];
	for (const id of ids) {
		const clip = pictureClip(tl, id);
		const baked = bakeBehaviors(clip, rate);
		for (const track of clip.channels ?? []) {
			const problem = propertyKeysProblem(track.prop, track.keys);
			if (problem) throw invalid(problem);
		}
		done.push({ clip: structuredClone(clip), baked });
	}
	return { timeline: tl, result: done, label: bakeBehaviorsLabel(ids.length) };
}
