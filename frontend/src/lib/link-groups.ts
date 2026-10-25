// Link groups — the faithful TS mirror of the group half of kerf-core's
// `model/links.rs` (crates/kerf-core/src/model/links.rs): which clips are partners,
// linking and unlinking, and the checks every linked edit starts with. Pure, and used
// by the browser harness in `api.ts` (and by the linked edit-mode ports in
// `edit-modes.ts`); the desktop app asks the backend, which applies these very rules.
// A *port*, not a lookalike: the bun tests replay the Rust tests case for case, and
// the messages are the backend's own (`invalid argument: …`).
//
// A link group is the clips sharing a `link_id` — in practice a picture and the audio
// clip that carries its sound — at most one clip of a group per track. Linking is
// identity, not position: an edit carries the *change* to the partners and never
// forces them to line up. A partner on a locked track refuses the whole edit.

import { clipDuration, clipRamp, isRamped, isReversed, sourceToTimeline, speedAt, timelineToSource } from './speed-ramp';
import type { Clip, Timeline, Track } from './types';

/** kerf-core's `STEP_EPS`: offsets closer than this are the same offset. */
export const STEP_EPS = 1e-6;

export const invalid = (why: string) => new Error(`invalid argument: ${why}`);
export const clipNotFound = (id: string) => new Error(`clip not found: ${id}`);

/** `[track index, clip index]` of a clip, or `null`. */
export function locateIndex(timeline: Timeline, clipId: string): [number, number] | null {
	for (let ti = 0; ti < timeline.tracks.length; ti++) {
		const ci = timeline.tracks[ti].clips.findIndex((c) => c.id === clipId);
		if (ci >= 0) return [ti, ci];
	}
	return null;
}

export function clipById(timeline: Timeline, clipId: string): Clip | undefined {
	const at = locateIndex(timeline, clipId);
	return at ? timeline.tracks[at[0]].clips[at[1]] : undefined;
}

export const newId = (): string => (crypto.randomUUID ? crypto.randomUUID() : `id-${Math.random().toString(36).slice(2)}`);

/** The other clips of `clipId`'s link group, in track order — none for an unlinked
 *  clip, a clip that is not on the timeline, or a link whose partners are all gone. */
export function linkPartners(timeline: Timeline, clipId: string): string[] {
	const link = clipById(timeline, clipId)?.link_id;
	if (!link) return [];
	return timeline.tracks.flatMap((t) => t.clips.filter((c) => c.link_id === link && c.id !== clipId).map((c) => c.id));
}

/** `ids` followed by the partners of each, every clip once (the order of `ids` kept). */
export function withLinkPartners(timeline: Timeline, ids: string[]): string[] {
	const seen = new Set(ids);
	const out: string[] = [];
	const first = new Set<string>();
	for (const id of ids) {
		if (!first.has(id)) {
			first.add(id);
			out.push(id);
		}
	}
	for (const id of ids) {
		for (const partner of linkPartners(timeline, id)) {
			if (!seen.has(partner)) {
				seen.add(partner);
				out.push(partner);
			}
		}
	}
	return out;
}

/** The error for a partner the edit cannot touch. */
export function lockedPartner(track: Track): Error {
	return invalid(`a linked clip is on locked track ${track.name} — unlock it first`);
}

/** The partners of `clipId` that are not in `skip`, each on an unlocked track — or the
 *  error that refuses the whole edit. */
export function unlockedPartners(timeline: Timeline, clipId: string, skip: ReadonlySet<string>): string[] {
	const out: string[] = [];
	for (const partner of linkPartners(timeline, clipId)) {
		if (skip.has(partner)) continue;
		const [ti] = locateIndex(timeline, partner)!;
		if (timeline.tracks[ti].locked) throw lockedPartner(timeline.tracks[ti]);
		out.push(partner);
	}
	return out;
}

/** Clear the link of any of `groups` left with a single member: a link of one clip is
 *  nothing, and a stale id would read as a link in the file. */
export function dissolveOrphans(timeline: Timeline, groups: ReadonlySet<string>) {
	const members = new Map<string, number>();
	for (const clip of timeline.tracks.flatMap((t) => t.clips)) {
		if (clip.link_id && groups.has(clip.link_id)) members.set(clip.link_id, (members.get(clip.link_id) ?? 0) + 1);
	}
	for (const clip of timeline.tracks.flatMap((t) => t.clips)) {
		if (clip.link_id && members.get(clip.link_id) === 1) delete clip.link_id;
	}
}

/** `dissolveOrphans` for every group on the timeline (`Timeline::dissolve_all_orphans`) —
 *  what an edit that removed clips (a cut, a delete, a removed track) leaves behind: a
 *  picture whose sound is gone is not linked to anything. Reports whether it cleared one. */
export function dissolveAllOrphans(timeline: Timeline): boolean {
	const members = new Map<string, number>();
	for (const clip of timeline.tracks.flatMap((t) => t.clips)) {
		if (clip.link_id) members.set(clip.link_id, (members.get(clip.link_id) ?? 0) + 1);
	}
	let cleared = false;
	for (const clip of timeline.tracks.flatMap((t) => t.clips)) {
		if (clip.link_id && members.get(clip.link_id) === 1) {
			delete clip.link_id;
			cleared = true;
		}
	}
	return cleared;
}

/** Every clip that is linked to at least one other (`Timeline::linked_clip_ids`). */
export function linkedClipIds(timeline: Timeline): Set<string> {
	const groups = new Map<string, string[]>();
	for (const clip of timeline.tracks.flatMap((t) => t.clips)) {
		if (clip.link_id) groups.set(clip.link_id, [...(groups.get(clip.link_id) ?? []), clip.id]);
	}
	const out = new Set<string>();
	for (const members of groups.values()) if (members.length >= 2) for (const id of members) out.add(id);
	return out;
}

/** Re-form a group after an edit cut it in two (`Timeline::relink_sides`): the clips of
 *  `left` (before the cut) keep `group`, the clips of `right` (after it) get a fresh id, and
 *  a side with fewer than two clips is no group at all. */
export function relinkSides(timeline: Timeline, group: string | undefined, left: readonly string[], right: readonly string[]) {
	const leftId = left.length >= 2 ? group : undefined;
	const rightId = right.length >= 2 ? newId() : undefined;
	for (const [ids, link] of [
		[left, leftId],
		[right, rightId]
	] as const) {
		for (const id of ids) {
			const clip = clipById(timeline, id);
			if (!clip) continue;
			if (link) clip.link_id = link;
			else delete clip.link_id;
		}
	}
}

/** What `linkClips` has validated: the groups the clips leave behind. */
export interface LinkPlan {
	/** The link ids the named clips are in now (each is left, and dissolves if it ends up with one). */
	old: Set<string>;
}

/**
 * Whether `ids` could be linked — every check `linkClips` makes, none of its writes: the
 * `invalid argument` it would throw, or the plan it would carry out. Pure over `timeline`,
 * so a menu can ask before offering **Link** and say why not.
 */
export function planLink(timeline: Timeline, ids: readonly string[]): LinkPlan {
	const seen = new Set<string>();
	const lanes = new Set<number>();
	for (const id of ids) {
		const at = locateIndex(timeline, id);
		if (!at) throw clipNotFound(id);
		if (seen.has(id)) throw invalid(`clip ${id} appears more than once`);
		seen.add(id);
		const track = timeline.tracks[at[0]];
		if (track.locked) throw invalid(`track ${track.name} is locked`);
		if (lanes.has(at[0]))
			throw invalid(
				`two of the clips are on track ${track.name} — a link joins one clip per track (a picture and its sound)`
			);
		lanes.add(at[0]);
	}
	if (seen.size < 2) throw invalid('linking needs at least two clips');
	const old = new Set(ids.map((id) => clipById(timeline, id)?.link_id).filter((l): l is string => !!l));
	const alreadyOneGroup =
		old.size === 1 &&
		ids.every((id) => !!clipById(timeline, id)?.link_id) &&
		withLinkPartners(timeline, [...ids]).length === seen.size;
	if (alreadyOneGroup) throw invalid('those clips are already linked');
	return { old };
}

/**
 * **Link** `ids` into one group (a new link id; clips already in another group leave
 * it, and a group left with one member dissolves). Needs at least two clips, on
 * different tracks, none on a locked track. Returns the group's id. Linking clips that
 * are already exactly one group is an error, so a no-op records no revision.
 */
export function linkClips(timeline: Timeline, ids: string[]): string {
	const { old } = planLink(timeline, ids);
	const group = newId();
	for (const id of ids) clipById(timeline, id)!.link_id = group;
	dissolveOrphans(timeline, old);
	return group;
}

/** What `unlinkClips` has validated: the groups the clips leave, and how many were linked. */
export interface UnlinkPlan {
	old: Set<string>;
	linked: number;
}

/** Whether `ids` could be unlinked — `unlinkClips`' checks without its writes (see `planLink`). */
export function planUnlink(timeline: Timeline, ids: readonly string[]): UnlinkPlan {
	const old = new Set<string>();
	let linked = 0;
	for (const id of ids) {
		const at = locateIndex(timeline, id);
		if (!at) throw clipNotFound(id);
		const track = timeline.tracks[at[0]];
		if (track.locked) throw invalid(`track ${track.name} is locked`);
		const link = track.clips[at[1]].link_id;
		if (link) {
			old.add(link);
			linked += 1;
		}
	}
	if (linked === 0) throw invalid('none of those clips is linked');
	return { old, linked };
}

/**
 * **Unlink** `ids`: each leaves its group, and a group left with a single clip dissolves
 * (so unlinking either half of a pair unlinks the pair). Errors when none of them was
 * linked, or one is on a locked track. Returns how many of the named clips were linked.
 */
export function unlinkClips(timeline: Timeline, ids: string[]): number {
	const { old, linked } = planUnlink(timeline, ids);
	for (const id of ids) delete clipById(timeline, id)!.link_id;
	dissolveOrphans(timeline, old);
	return linked;
}

/** The timeline time at which source time 0 of `clip` would play: the same for two clips
 *  of one asset exactly when they show the same moment of footage at the same moment of
 *  the timeline. A reversed clip plays its window backwards, so it is measured from the
 *  out side (`content_offset` in kerf-core). */
export function contentOffset(clip: Clip): number {
	return sourceToTimeline(clip, 0);
}

/** How far the footage of a clip moved on the timeline from `was` to `now` (the same clip): the difference of
 *  their `contentOffset`s — except when either has a speed ramp, where the offset (a point *outside* the window,
 *  reached by running the first key's speed backwards) moves with every head trim, which moves no footage. Then it
 *  is where the footage at `at` plays: `at` is a footage time both windows hold (`stepAnchor`), `undefined` the
 *  middle of what they share (`content_shift`). */
export function contentShift(was: Clip, now: Clip, at?: number): number {
	if (!isRamped(was) && !isRamped(now)) return contentOffset(now) - contentOffset(was);
	let src = at;
	if (src === undefined) {
		const [lo, hi] = [Math.max(was.source_in, now.source_in), Math.min(was.source_out, now.source_out)];
		src = hi > lo ? (lo + hi) / 2 : was.source_in;
	}
	return sourceToTimeline(now, src) - sourceToTimeline(was, src);
}

const endOf = (c: Clip) => c.timeline_start + clipDuration(c);

/** Whether two linked clips run at the same pace: one speed, or — when either has a ramp — the same speed at
 *  every moment they share (judged at every key of either and between them), in the same direction. A pair that
 *  does is in step when the same footage plays at the same time (`stepGap`) (`same_pace`). */
export function samePace(a: Clip, b: Clip): boolean {
	if (!isRamped(a) && !isRamped(b)) return Math.abs((a.speed ?? 1) - (b.speed ?? 1)) < STEP_EPS;
	if (isReversed(a) !== isReversed(b)) return false;
	let [lo, hi] = [Math.max(a.timeline_start, b.timeline_start), Math.min(endOf(a), endOf(b))];
	if (!(hi > lo)) [lo, hi] = [Math.min(a.timeline_start, b.timeline_start), Math.max(endOf(a), endOf(b))];
	const marks = [lo, hi];
	for (const c of [a, b]) for (const k of clipRamp(c) ?? []) if (c.timeline_start + k.time > lo && c.timeline_start + k.time < hi) marks.push(c.timeline_start + k.time);
	marks.sort((x, y) => x - y);
	const mids = marks.slice(1).map((m, i) => (marks[i] + m) / 2);
	marks.push(...mids);
	return marks.every((t) => Math.abs(speedAt(a, t - a.timeline_start) - speedAt(b, t - b.timeline_start)) < STEP_EPS);
}

/** How far apart two clips of one pace play the same footage, in timeline seconds: the difference of their
 *  `contentOffset`s, or — with a ramp on either — how much later or earlier `b` plays what `a` plays at one moment
 *  they share (`step_gap`). */
export function stepGap(a: Clip, b: Clip): number {
	if (!isRamped(a) && !isRamped(b)) return Math.abs(contentOffset(a) - contentOffset(b));
	const [lo, hi] = [Math.max(a.timeline_start, b.timeline_start), Math.min(endOf(a), endOf(b))];
	const t = hi > lo ? (lo + hi) / 2 : a.timeline_start;
	return Math.abs(sourceToTimeline(b, timelineToSource(a, t)) - t);
}

/** For a link group with a ramp in it: the footage time of each asset at which the members' movement is
 *  measured — the middle of the footage every member of that asset holds before and after the edit (`pairs` is
 *  each member's clip before, and after). Members of one asset are then judged at the same footage, which is what
 *  keeps a picture and its sound in step when an edit stretches the time before it. An asset whose members share
 *  no footage has none, and each is judged at its own (`step_anchor`). */
export function stepAnchor(pairs: readonly (readonly [Clip, Clip])[]): Map<string, number> {
	const held = new Map<string, [number, number]>();
	for (const [was, now] of pairs) {
		const [lo, hi] = [Math.max(was.source_in, now.source_in), Math.min(was.source_out, now.source_out)];
		const slot = held.get(now.asset_id) ?? [-Infinity, Infinity];
		held.set(now.asset_id, [Math.max(slot[0], lo), Math.min(slot[1], hi)]);
	}
	const out = new Map<string, number>();
	for (const [asset, [lo, hi]] of held) if (hi > lo) out.set(asset, (lo + hi) / 2);
	return out;
}

/** Whether any clip is linked — the cheap test that lets an unlinked project skip the guard. */
export function hasLinks(timeline: Timeline): boolean {
	return timeline.tracks.some((t) => t.clips.some((c) => c.link_id));
}

/**
 * The **sync guard** (`Timeline::first_sync_break` in kerf-core): the first link group the
 * edit that turned `before` into `after` pulled out of step, as the names of the two
 * tracks — or `null`. Two linked clips of the same asset at the same speed are *in step*
 * when equal content offsets say they show the same moment of footage at the same time.
 * An edit that would leave an in-step pair apart is refused rather than silently
 * desynchronizing sound from picture; a pair that was already apart, or whose clips cannot
 * be compared (different assets), is not the edit's doing and is not looked at.
 */
export function firstSyncBreak(after: Timeline, before: Timeline): [string, string] | null {
	const prior = new Map<string, Clip>();
	for (const c of before.tracks.flatMap((t) => t.clips)) prior.set(c.id, c);
	const groups = new Map<string, { ti: number; clip: Clip }[]>();
	after.tracks.forEach((track, ti) => {
		for (const clip of track.clips) {
			if (!clip.link_id) continue;
			const members = groups.get(clip.link_id) ?? [];
			members.push({ ti, clip });
			groups.set(clip.link_id, members);
		}
	});
	const EPS = STEP_EPS;
	// Of every pair that broke, the one on the lowest tracks (`Timeline::first_sync_break`).
	let first: [number, number] | null = null;
	for (const members of groups.values()) {
		for (let i = 0; i < members.length; i++) {
			for (let j = i + 1; j < members.length; j++) {
				const [a, b] = [members[i], members[j]];
				const pa = prior.get(a.clip.id);
				const pb = prior.get(b.clip.id);
				if (!pa || !pb) continue;
				const inStepBefore = pa.asset_id === pb.asset_id && samePace(pa, pb) && stepGap(pa, pb) < EPS;
				if (!inStepBefore) continue;
				if (!samePace(a.clip, b.clip) || stepGap(a.clip, b.clip) >= EPS) {
					const pair: [number, number] = [Math.min(a.ti, b.ti), Math.max(a.ti, b.ti)];
					if (!first || pair[0] < first[0] || (pair[0] === first[0] && pair[1] < first[1])) first = pair;
				}
			}
		}
	}
	return first ? [after.tracks[first[0]].name, after.tracks[first[1]].name] : null;
}

/** The refusal for an edit `firstSyncBreak` found, in the backend's words. */
export function syncBreakError([a, b]: [string, string]): Error {
	return invalid(
		`that edit would leave the linked clips on ${a} and ${b} out of step with each other — unlink them first if they are meant to part`
	);
}
