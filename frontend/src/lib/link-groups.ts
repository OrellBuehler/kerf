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

import type { Clip, Timeline, Track } from './types';

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
	return invalid(`a linked clip is on locked track ${track.name} — unlock it, or edit with links off`);
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

/** Give `ids` — clips an edit just created from one clip and its partners — one fresh
 *  link group when there are two or more of them, else none. */
export function relinkNewHalves(timeline: Timeline, ids: string[]) {
	const group = ids.length >= 2 ? newId() : undefined;
	for (const id of ids) {
		const clip = clipById(timeline, id);
		if (!clip) continue;
		if (group) clip.link_id = group;
		else delete clip.link_id;
	}
}

/**
 * **Link** `ids` into one group (a new link id; clips already in another group leave
 * it, and a group left with one member dissolves). Needs at least two clips, on
 * different tracks, none on a locked track. Returns the group's id. Linking clips that
 * are already exactly one group is an error, so a no-op records no revision.
 */
export function linkClips(timeline: Timeline, ids: string[]): string {
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
		withLinkPartners(timeline, ids).length === seen.size;
	if (alreadyOneGroup) throw invalid('those clips are already linked');
	const group = newId();
	for (const id of ids) clipById(timeline, id)!.link_id = group;
	dissolveOrphans(timeline, old);
	return group;
}

/**
 * **Unlink** `ids`: each leaves its group, and a group left with a single clip dissolves
 * (so unlinking either half of a pair unlinks the pair). Errors when none of them was
 * linked, or one is on a locked track. Returns how many of the named clips were linked.
 */
export function unlinkClips(timeline: Timeline, ids: string[]): number {
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
	for (const id of ids) delete clipById(timeline, id)!.link_id;
	dissolveOrphans(timeline, old);
	return linked;
}

/** The timeline time at which source time 0 of `clip` would play: the same for two clips
 *  of one asset exactly when they show the same moment of footage at the same moment of
 *  the timeline. A reversed clip plays its window backwards, so it is measured from the
 *  out side (`content_offset` in kerf-core). */
function contentOffset(clip: Clip): number {
	const mag = Math.max(Math.abs(clip.speed ?? 1), 0.01);
	return (clip.speed ?? 1) < 0 ? clip.timeline_start + clip.source_out / mag : clip.timeline_start - clip.source_in / mag;
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
	const groups = new Map<string, { track: Track; clip: Clip }[]>();
	for (const track of after.tracks) {
		for (const clip of track.clips) {
			if (!clip.link_id) continue;
			const members = groups.get(clip.link_id) ?? [];
			members.push({ track, clip });
			groups.set(clip.link_id, members);
		}
	}
	const EPS = 1e-6;
	const speed = (c: Clip) => c.speed ?? 1;
	for (const members of groups.values()) {
		for (let i = 0; i < members.length; i++) {
			for (let j = i + 1; j < members.length; j++) {
				const [a, b] = [members[i], members[j]];
				const pa = prior.get(a.clip.id);
				const pb = prior.get(b.clip.id);
				if (!pa || !pb) continue;
				const inStepBefore =
					pa.asset_id === pb.asset_id &&
					Math.abs(speed(pa) - speed(pb)) < EPS &&
					Math.abs(contentOffset(pa) - contentOffset(pb)) < EPS;
				if (!inStepBefore) continue;
				if (
					Math.abs(speed(a.clip) - speed(b.clip)) >= EPS ||
					Math.abs(contentOffset(a.clip) - contentOffset(b.clip)) >= EPS
				)
					return [a.track.name, b.track.name];
			}
		}
	}
	return null;
}

/** The refusal for an edit `firstSyncBreak` found, in the backend's words. */
export function syncBreakError([a, b]: [string, string]): Error {
	return invalid(
		`that edit would put the linked clips on ${a} and ${b} out of step — edit with links off to move one of them on its own`
	);
}
