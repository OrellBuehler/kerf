/* Partial accept of a staged proposal, for the **browser dev harness**.
 *
 * `kerf_core::Timeline::merge_subset` (crates/kerf-core/src/model/subset.rs) is authoritative: the
 * desktop app asks the backend, which lays the picked changes of the proposal onto the live cut by
 * identity (this clip, this text, this track — never by position). This is the port the harness runs,
 * so the review flow's checkboxes can be driven and tested under `bun run dev` with the backend's
 * own rules and messages:
 *
 *  - a linked partner's change comes along (a picture and its sound are one piece of material), and so
 *    does the track a picked clip lands on, or the moves that take clips off a track the proposal
 *    removes — those are *auto-included* and reported;
 *  - a pick that would put two clips on top of each other is refused with where, because the changes
 *    that make room are a different entry the reviewer did not pick;
 *  - a pick that would leave linked clips out of step with each other (the sync guard,
 *    `Timeline::first_sync_break`) is refused, because the change that keeps them together is one the
 *    reviewer did not pick;
 *  - a pick whose target the user has since changed is *skipped* and listed, not fatal.
 *
 * Pure. The harness' diff entries carry the ids core would give them (`entryId`). */

import { dissolveAllOrphans, firstSyncBreak, hasLinks, locateIndex } from './link-groups';
import { formatTime } from './diff';
import { clipDuration } from './types';
import type { AutoIncluded, Clip, DiffEntry, SkippedChange, Timeline, TimelineDiff } from './types';

/** Core's `DIFF_EPS`: two times closer than this are the same time. */
const EPS = 1e-6;

/** The change's stable name — `<kind>:<the clip, text, marker or track it is about>` — as
 *  `DiffEntry::with_id` makes it, for an entry that does not carry one (a fixture, the harness' seed). */
export function entryId(entry: DiffEntry): string {
	if (entry.id) return entry.id;
	const target = entry.clip_id ?? entry.overlay_id ?? entry.marker_id ?? entry.track_id ?? null;
	return target ? `${entry.kind}:${target}` : entry.kind;
}

export interface SubsetMerge {
	/** The live cut with the picked changes laid on it. */
	timeline: Timeline;
	/** Ids of the changes applied, picked or not. */
	applied: string[];
	autoIncluded: AutoIncluded[];
	skipped: SkippedChange[];
}

const invalid = (why: string) => new Error(`invalid argument: ${why}`);

const spansOverlap = (a: [number, number], b: [number, number]) => a[0] < b[1] - EPS && b[0] < a[1] - EPS;
const clipEnd = (c: Clip) => c.timeline_start + clipDuration(c);

const sortByStart = (clips: Clip[]) => clips.sort((a, b) => a.timeline_start - b.timeline_start);

/** Every link group across the cuts: link id → the clips ever in it. */
function linkGroups(cuts: Timeline[]): Map<string, Set<string>> {
	const groups = new Map<string, Set<string>>();
	for (const cut of cuts)
		for (const track of cut.tracks)
			for (const clip of track.clips)
				if (clip.link_id) groups.set(clip.link_id, (groups.get(clip.link_id) ?? new Set()).add(clip.id));
	return groups;
}

/** The clips linked to `clip` in any of the cuts. */
function partnersOf(groups: Map<string, Set<string>>, cuts: Timeline[], clip: string): string[] {
	const out = new Set<string>();
	for (const cut of cuts) {
		const found = cut.tracks.flatMap((t) => t.clips).find((c) => c.id === clip);
		if (found?.link_id) for (const other of groups.get(found.link_id) ?? []) if (other !== clip) out.add(other);
	}
	return [...out].sort();
}

type Act = 'add' | 'change' | 'remove';

/** Lay one add / change / remove of the item `id` onto `list`, or say why it no longer applies. */
function lay<T extends { id: string }>(list: T[], id: string, act: Act, wanted: T | undefined, what: string): string | null {
	const at = list.findIndex((t) => t.id === id);
	if (act === 'remove') {
		if (at < 0) return `the ${what} is already gone`;
		list.splice(at, 1);
		return null;
	}
	if (act === 'add' && at < 0 && wanted) {
		list.push(structuredClone(wanted));
		return null;
	}
	if (act === 'add') return `the ${what} is already on the timeline`;
	if (at >= 0 && wanted) {
		list[at] = structuredClone(wanted);
		return null;
	}
	return `that ${what} is no longer on the timeline`;
}

interface ClipOps {
	added: boolean;
	removed: boolean;
	moved: boolean;
	retrimmed: boolean;
	changed: boolean;
	entries: number[];
}

/** The closure `merge_subset` takes before it applies anything: what has to come with the changes in `chosen` (indices
 *  into `entries`) — a linked partner's change, the new track a picked clip lands on, the moves that take clips off a
 *  track picked for removal. Grows `chosen` in place and answers why each addition came, `{ index → because }`. `cuts` are
 *  the live cut, the base and the proposal: a link group is the clips ever together in any of them. */
export function closeOver(entries: readonly DiffEntry[], chosen: Set<number>, cuts: Timeline[], base: Timeline): Map<number, string> {
	const because = new Map<number, string>();
	const groups = linkGroups(cuts);
	for (;;) {
		const more: [number, string][] = [];
		for (const i of chosen) {
			const e = entries[i];
			if (e.clip_id) {
				for (const partner of partnersOf(groups, cuts, e.clip_id))
					entries.forEach((f, j) => {
						if (f.clip_id === partner && !chosen.has(j))
							more.push([j, 'it is linked to a clip you picked — a picture and its sound go together']);
					});
			}
			if ((e.kind === 'clip_added' || e.kind === 'clip_moved') && e.track_id) {
				entries.forEach((f, j) => {
					if (f.kind === 'track_added' && f.track_id === e.track_id && !chosen.has(j))
						more.push([j, 'a clip you picked is on this new track']);
				});
			}
			if (e.kind === 'track_removed') {
				entries.forEach((f, j) => {
					const was = f.kind === 'clip_moved' && f.clip_id ? locateIndex(base, f.clip_id) : null;
					if (was && base.tracks[was[0]].id === e.track_id && !chosen.has(j))
						more.push([j, 'the clip would be lost with the track you picked for removal']);
				});
			}
		}
		let grew = false;
		for (const [j, why] of more) {
			if (!chosen.has(j)) {
				chosen.add(j);
				because.set(j, why);
				grew = true;
			}
		}
		if (!grew) break;
	}
	return because;
}

/** Lay the changes of `proposed` (relative to `base`) named by `picked` onto `live` — see the file header. */
export function mergeSubset(live: Timeline, base: Timeline, proposed: Timeline, diff: TimelineDiff, picked: string[]): SubsetMerge {
	const entries = diff.entries;
	const ids = entries.map(entryId);
	if (picked.length === 0) throw invalid('pick at least one change to apply');
	const chosen = new Set<number>();
	for (const id of picked) {
		const i = ids.indexOf(id);
		if (i < 0) throw invalid(`no change ${id} in the proposal — ids come from the proposal's diff entries`);
		chosen.add(i);
	}
	const pickedSet = new Set(chosen);
	const because = closeOver(entries, chosen, [live, base, proposed], base);

	const out: Timeline = structuredClone(live);
	const applied = new Set<number>();
	const skipped: SkippedChange[] = [];
	const skip = (i: number, why: string) => skipped.push({ id: ids[i], summary: entries[i].summary, why });
	const touched = new Set<string>();
	const ordered = [...chosen].sort((a, b) => a - b);

	// 1. New tracks, so the clips that land on them have somewhere to go.
	for (const i of ordered.filter((i) => entries[i].kind === 'track_added')) {
		const id = entries[i].track_id;
		if (!id) continue;
		const at = proposed.tracks.findIndex((t) => t.id === id);
		if (at < 0) skip(i, 'the proposal no longer has that track');
		else if (out.tracks.some((t) => t.id === id)) skip(i, 'the cut already has that track');
		else {
			const track = structuredClone(proposed.tracks[at]);
			track.clips = [];
			out.tracks.splice(Math.min(at, out.tracks.length), 0, track);
			applied.add(i);
		}
	}

	// 2. Track settings — everything on a track but its clips.
	for (const i of ordered.filter((i) => entries[i].kind === 'track_changed')) {
		const id = entries[i].track_id;
		if (!id) continue;
		const from = proposed.tracks.find((t) => t.id === id);
		const to = out.tracks.findIndex((t) => t.id === id);
		if (!from || to < 0) skip(i, 'that track is no longer on the timeline');
		else {
			const clips = out.tracks[to].clips;
			out.tracks[to] = { ...structuredClone(from), clips };
			applied.add(i);
		}
	}

	// 3. Clips, one clip at a time however many of its changes were picked.
	const ops = new Map<string, ClipOps>();
	for (const i of ordered) {
		const e = entries[i];
		if (!e.clip_id) continue;
		const op = ops.get(e.clip_id) ?? { added: false, removed: false, moved: false, retrimmed: false, changed: false, entries: [] };
		ops.set(e.clip_id, op);
		if (e.kind === 'clip_added') op.added = true;
		else if (e.kind === 'clip_removed') op.removed = true;
		else if (e.kind === 'clip_moved') op.moved = true;
		else if (e.kind === 'clip_retrimmed') op.retrimmed = true;
		else if (e.kind === 'clip_changed') op.changed = true;
		else continue;
		op.entries.push(i);
	}
	const settle = (op: ClipOps) => op.entries.forEach((i) => applied.add(i));
	const refuse = (op: ClipOps, why: string) => op.entries.forEach((i) => skip(i, why));
	for (const [id, op] of ops) {
		const here = locateIndex(out, id);
		const there = locateIndex(proposed, id);
		if (op.removed) {
			if (here) {
				out.tracks[here[0]].clips.splice(here[1], 1);
				settle(op);
			} else refuse(op, 'the clip is already gone from the timeline');
			continue;
		}
		if (op.added) {
			if (here) refuse(op, 'the clip is already on the timeline');
			else if (!there) refuse(op, 'the proposal no longer has that clip');
			else {
				const track = out.tracks.find((t) => t.id === proposed.tracks[there[0]].id);
				if (!track) refuse(op, 'the track it was added to is gone from the timeline');
				else {
					track.clips.push(structuredClone(proposed.tracks[there[0]].clips[there[1]]));
					sortByStart(track.clips);
					touched.add(id);
					settle(op);
				}
			}
			continue;
		}
		if (!here) {
			refuse(op, 'the clip is no longer on the timeline');
			continue;
		}
		if (!there) {
			refuse(op, 'the proposal no longer has that clip');
			continue;
		}
		const liveClip = out.tracks[here[0]].clips[here[1]];
		const wanted = proposed.tracks[there[0]].clips[there[1]];
		const clip: Clip = structuredClone(op.changed ? wanted : liveClip);
		// The position and footage are their own changes: only a pick takes them.
		clip.timeline_start = liveClip.timeline_start;
		clip.source_in = liveClip.source_in;
		clip.source_out = liveClip.source_out;
		if (op.moved) clip.timeline_start = wanted.timeline_start;
		if (op.retrimmed) {
			clip.source_in = wanted.source_in;
			clip.source_out = wanted.source_out;
		}
		const dest = op.moved ? proposed.tracks[there[0]].id : out.tracks[here[0]].id;
		if (dest === out.tracks[here[0]].id) {
			out.tracks[here[0]].clips[here[1]] = clip;
			sortByStart(out.tracks[here[0]].clips);
		} else {
			const destAt = out.tracks.findIndex((t) => t.id === dest);
			if (destAt < 0) {
				refuse(op, 'the track it moves to is gone from the timeline');
				continue;
			}
			out.tracks[here[0]].clips.splice(here[1], 1);
			out.tracks[destAt].clips.push(clip);
			sortByStart(out.tracks[destAt].clips);
		}
		touched.add(id);
		settle(op);
	}

	// 4. Text, markers, the delivery frame and the master bus.
	for (const i of ordered) {
		const e = entries[i];
		const act: Act | null = e.kind.endsWith('_added') ? 'add' : e.kind.endsWith('_removed') ? 'remove' : e.kind.endsWith('_changed') ? 'change' : null;
		if (e.kind.startsWith('overlay_') && e.overlay_id && act) {
			const id = e.overlay_id;
			const animated = [proposed, base, live].some((t) => (t.svg_overlays ?? []).some((o) => o.id === id));
			const why = animated
				? lay((out.svg_overlays ??= []), id, act, (proposed.svg_overlays ?? []).find((o) => o.id === id), 'animated title')
				: lay((out.overlays ??= []), id, act, (proposed.overlays ?? []).find((o) => o.id === id), 'text');
			if (why) skip(i, why);
			else applied.add(i);
		} else if (e.kind.startsWith('marker_') && e.marker_id && act) {
			const why = lay((out.markers ??= []), e.marker_id, act, (proposed.markers ?? []).find((m) => m.id === e.marker_id), 'marker');
			if (why) skip(i, why);
			else {
				out.markers!.sort((a, b) => a.time - b.time);
				applied.add(i);
			}
		} else if (e.kind === 'format_changed') {
			out.format = proposed.format;
			applied.add(i);
		} else if (e.kind === 'master_changed') {
			out.master = proposed.master;
			applied.add(i);
		}
	}

	// 5. Removed tracks last: clips the proposal moved off them have already left.
	for (const i of ordered.filter((i) => entries[i].kind === 'track_removed')) {
		const id = entries[i].track_id;
		const at = out.tracks.findIndex((t) => t.id === id);
		if (at >= 0 && out.tracks.length > 1) {
			out.tracks.splice(at, 1);
			applied.add(i);
		} else if (at >= 0) skip(i, 'it is the last track');
		else skip(i, 'the track is already gone');
	}

	dissolveAllOrphans(out);
	checkLanes(out, live, touched);
	if (hasLinks(out)) {
		const broke = firstSyncBreak(out, live);
		if (broke)
			throw invalid(
				`applying only those changes would pull the linked clips on ${broke[0]} and ${broke[1]} out of step — pick the changes to both, or apply the whole proposal`
			);
	}

	return {
		timeline: out,
		applied: [...applied].sort((a, b) => a - b).map((i) => ids[i]),
		autoIncluded: ordered
			.filter((i) => !pickedSet.has(i) && applied.has(i))
			.map((i) => ({ id: ids[i], summary: entries[i].summary, because: because.get(i) ?? '' })),
		skipped
	};
}

/** Refuse a merge that left a clip it wrote on top of another clip of its track, unless the two
 *  already overlapped in `live` (an overlap that was not this merge's doing). */
function checkLanes(cut: Timeline, live: Timeline, touched: Set<string>) {
	if (touched.size === 0) return;
	const was = new Map<string, [number, number]>();
	for (const track of live.tracks) for (const c of track.clips) was.set(c.id, [c.timeline_start, clipEnd(c)]);
	for (const track of cut.tracks) {
		for (const p of track.clips.filter((c) => touched.has(c.id))) {
			const span: [number, number] = [p.timeline_start, clipEnd(p)];
			for (const q of track.clips) {
				const sameGroup = !!p.link_id && p.link_id === q.link_id;
				if (q.id === p.id || sameGroup || !spansOverlap(span, [q.timeline_start, clipEnd(q)])) continue;
				const a = was.get(p.id);
				const b = was.get(q.id);
				if (a && b && spansOverlap(a, b)) continue;
				throw invalid(
					`applying only those changes would put two clips on top of each other on ${track.name} at ${formatTime(Math.max(p.timeline_start, q.timeline_start))} — also pick the moves that make room, or apply the whole proposal`
				);
			}
		}
	}
}

/** The sentence core writes for the toast after a partial accept (`SubsetApplied::message`). */
export function subsetMessage(
	applied: number,
	total: number,
	autoIncluded: AutoIncluded[],
	skipped: SkippedChange[],
	remaining: number
): string {
	let message = `Applied ${applied} of ${total} change${total === 1 ? '' : 's'}`;
	if (autoIncluded.length)
		message += ` — ${autoIncluded.length} came with the ones you picked (${autoIncluded.map((a) => a.summary).join('; ')})`;
	if (skipped.length) message += `; ${skipped.length} no longer apply`;
	return message + (remaining === 0 ? '.' : `; ${remaining} left to review.`);
}
