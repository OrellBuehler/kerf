// Linked clips — the faithful TS mirror of the editing half of kerf-core's
// `model/links.rs` (crates/kerf-core/src/model/links.rs): detach / reattach audio,
// and every edit that carries its change to a clip's linked partners. Pure and used
// by the browser harness in `api.ts`; the desktop app asks the backend, which applies
// these very rules. A *port*, not a lookalike — the bun tests replay the Rust tests
// case for case, so a rule changed there has to change here or a test names it.
//
//   edit                       the partners…
//   move (withLinkedMoves)     move by the same Δt, on their own tracks
//   trim (carryExtentEdit)     follow the edge that changed when they share it, clamped to their footage
//   split (splitClipLinked)    are split at the same time (if it is inside them); the new halves link up
//   remove / ripple delete     are removed too
//   cut a source span          lose the same stretch of *timeline*, and close up
//   speed                      are retimed by the same ratio
//   split and remove           are cut at the same time, if it is inside them
//   roll / slip / slide        (edit-modes.ts) get the same edit, the group clamping to its tightest
//   ripple                     (ripple.ts) follow a partner the ripple moved
//
// A locked partner refuses the whole edit; property edits (volume, fades, effects) are
// not carried. Every function validates before it mutates, so a thrown error leaves
// the timeline exactly as it was.

import {
	ADJACENT_EPS,
	clampFades,
	footageOf,
	handles,
	moveHead,
	moveTail,
	type SourceLimits
} from './edit-modes';
import {
	clipById,
	clipNotFound,
	invalid,
	linkPartners,
	locateIndex,
	lockedPartner,
	newId,
	relinkNewHalves,
	unlockedPartners
} from './link-groups';
import { DIFF_EPS, spansOverlap } from './ripple';
import type { Clip, ClipCut, ClipMove, Timeline } from './types';
import { clipDuration } from './types';

const speedOf = (c: Clip) => Math.max(Math.abs(c.speed ?? 1), 0.01);
const reversed = (c: Clip) => (c.speed ?? 1) < 0;
const endOf = (c: Clip) => c.timeline_start + clipDuration(c);

// ---- detach / reattach -----------------------------------------------------------

/** What `detachAudio` made. */
export interface Detached {
	/** The new audio clip: the picture clip's own span, at its position, linked to it. */
	clip: Clip;
	/** The audio track it landed on. */
	track_id: string;
	/** Whether that track had to be created (no audio track had room). */
	created_track: boolean;
}

/** The audio lane to put a detached clip spanning `span` on: the audio track at the
 *  picture track's own position (V1 → A1, V2 → A2) when it has room, else the first
 *  audio track that does; never a locked one, nor one in `avoid`. */
function audioLaneFor(timeline: Timeline, videoTrack: number, span: [number, number], avoid: ReadonlySet<number>): number | undefined {
	const ordinal = timeline.tracks.slice(0, videoTrack).filter((t) => t.kind === 'video').length;
	const audio = timeline.tracks.map((_, i) => i).filter((i) => timeline.tracks[i].kind === 'audio');
	const preferred = audio[ordinal];
	const order = [...(preferred === undefined ? [] : [preferred]), ...audio.filter((i) => i !== preferred)];
	return order.find((i) => {
		const track = timeline.tracks[i];
		return !track.locked && !avoid.has(i) && !track.clips.some((c) => spansOverlap(span, [c.timeline_start, endOf(c)]));
	});
}

/**
 * **Detach** a picture clip's own sound: a new audio clip with the same source span,
 * speed and timeline position goes on an audio track (a new one when none has room), is
 * linked to the picture clip, and the picture clip's own sound is muted
 * (`source_audio: false`). The audio clip carries what shapes the *sound*: volume,
 * audio effects, fades and the transition; the picture keeps its own, inert while muted.
 * `hasAudio` is whether the clip's asset carries an audio stream. Refuses a clip that is
 * not on a video track, whose sound is already detached, or whose track is locked.
 */
export function detachAudio(timeline: Timeline, clipId: string, hasAudio: boolean): Detached {
	const at = locateIndex(timeline, clipId);
	if (!at) throw clipNotFound(clipId);
	const [vi, ci] = at;
	const clip = timeline.tracks[vi].clips[ci];
	if (timeline.tracks[vi].kind !== 'video')
		throw invalid('only a clip on a video track has sound of its own to detach');
	if (timeline.tracks[vi].locked) throw invalid(`track ${timeline.tracks[vi].name} is locked`);
	if (!hasAudio) throw invalid("the clip's asset has no audio stream");
	if (clip.source_audio === false) throw invalid("this clip's sound is already detached");
	// A track already holding a member of the group cannot take another.
	const avoid = new Set<number>();
	for (const p of linkPartners(timeline, clipId)) avoid.add(locateIndex(timeline, p)![0]);
	const span: [number, number] = [clip.timeline_start, endOf(clip)];
	let laneIx = audioLaneFor(timeline, vi, span, avoid);
	let createdTrack = false;
	if (laneIx === undefined) {
		const count = timeline.tracks.filter((t) => t.kind === 'audio').length;
		timeline.tracks.push({ id: newId(), kind: 'audio', name: `A${count + 1}`, clips: [] });
		laneIx = timeline.tracks.length - 1;
		createdTrack = true;
	}
	const group = clip.link_id ?? newId();
	const audio: Clip = {
		id: newId(),
		asset_id: clip.asset_id,
		source_in: clip.source_in,
		source_out: clip.source_out,
		timeline_start: clip.timeline_start,
		volume: clip.volume,
		fade_in: clip.fade_in,
		fade_out: clip.fade_out,
		speed: clip.speed ?? 1,
		transition_in: clip.transition_in ? { ...clip.transition_in } : null,
		audio: structuredClone(clip.audio ?? []),
		link_id: group
	};
	if (clip.enabled === false) audio.enabled = false;
	const lane = timeline.tracks[laneIx];
	lane.clips.push(audio);
	lane.clips.sort((a, b) => a.timeline_start - b.timeline_start);
	clip.source_audio = false;
	clip.link_id = group;
	return { clip: structuredClone(audio), track_id: lane.id, created_track: createdTrack };
}

/**
 * **Reattach** detached sound: the audio clip(s) linked to the picture clip that carry
 * the same asset are deleted and the picture clip plays its own sound again. Name either
 * the picture clip or its audio clip. A picture whose audio clip is already gone is just
 * unmuted. Returns the picture clip.
 */
export function reattachAudio(timeline: Timeline, clipId: string): Clip {
	const at = locateIndex(timeline, clipId);
	if (!at) throw clipNotFound(clipId);
	const [ti, ci] = at;
	const named = timeline.tracks[ti].clips[ci];
	let pictureId = clipId;
	if (timeline.tracks[ti].kind !== 'video') {
		const found = linkPartners(timeline, clipId).find((p) => {
			const c = clipById(timeline, p);
			const [pt] = locateIndex(timeline, p)!;
			return !!c && c.source_audio === false && c.asset_id === named.asset_id && timeline.tracks[pt].kind === 'video';
		});
		if (!found) throw invalid('no linked picture whose sound was detached');
		pictureId = found;
	}
	const [vi, vc] = locateIndex(timeline, pictureId)!;
	const picture = timeline.tracks[vi].clips[vc];
	if (picture.source_audio !== false) throw invalid("this clip's sound is not detached");
	if (timeline.tracks[vi].locked) throw invalid(`track ${timeline.tracks[vi].name} is locked`);
	const doomed = linkPartners(timeline, pictureId).filter((p) => {
		const [pt, pc] = locateIndex(timeline, p)!;
		return timeline.tracks[pt].kind === 'audio' && timeline.tracks[pt].clips[pc].asset_id === picture.asset_id;
	});
	for (const p of doomed) {
		const [pt] = locateIndex(timeline, p)!;
		if (timeline.tracks[pt].locked) throw lockedPartner(timeline.tracks[pt]);
	}
	const gone = new Set(doomed);
	for (const track of timeline.tracks) track.clips = track.clips.filter((c) => !gone.has(c.id));
	const group = picture.link_id;
	delete picture.source_audio;
	if (group) {
		const members = timeline.tracks.flatMap((t) => t.clips).filter((c) => c.link_id === group).length;
		if (members === 1) delete picture.link_id;
	}
	return structuredClone(picture);
}

// ---- move ------------------------------------------------------------------------

/**
 * `moves` widened with the clips linked to the ones it moves: each partner not already
 * named moves by the same Δt (the named clip's new start minus its old one) and **stays
 * on its own track**. Where the request names two members of one group, the first's Δt
 * is the group's. A partner that would start before 0, or sits on a locked track, is an
 * error. The named moves come first, unchanged.
 */
export function withLinkedMoves(timeline: Timeline, moves: ClipMove[]): ClipMove[] {
	const named = new Set(moves.map((m) => m.clip_id));
	const out = [...moves];
	const added = new Set<string>();
	for (const m of moves) {
		const clip = clipById(timeline, m.clip_id);
		if (!clip) continue; // `moveClips` reports the unknown clip
		const delta = m.timeline_start - clip.timeline_start;
		if (!Number.isFinite(delta) || Math.abs(delta) <= DIFF_EPS) continue;
		for (const partnerId of linkPartners(timeline, m.clip_id)) {
			if (named.has(partnerId) || added.has(partnerId)) continue;
			added.add(partnerId);
			const [pt, pc] = locateIndex(timeline, partnerId)!;
			if (timeline.tracks[pt].locked) throw lockedPartner(timeline.tracks[pt]);
			const start = timeline.tracks[pt].clips[pc].timeline_start + delta;
			if (start < -DIFF_EPS)
				throw invalid(
					`moving the clip that far would take its linked clip on ${timeline.tracks[pt].name} before the beginning of the timeline`
				);
			out.push({ clip_id: partnerId, timeline_start: Math.max(start, 0) });
		}
	}
	return out;
}

// ---- trim ------------------------------------------------------------------------

/** How an edit changed one clip's extent: the content moving (`shift`), the head
 *  trimmed (`head`, + = shorter from the front) and the tail moved (`tail`, + = longer). */
interface ExtentEdit {
	shift: number;
	head: number;
	tail: number;
}

/** What an edit did to `now`, from `was` (the same clip, same speed). `null` when the
 *  extent did not change. A change that keeps the length is a move; otherwise the source
 *  window tells a trimmed edge from a moved one. A still's edges are read off the extent. */
export function extentEdit(was: Clip, now: Clip, looping: boolean): ExtentEdit | null {
	const ds = now.timeline_start - was.timeline_start;
	const de = endOf(now) - endOf(was);
	if (Math.abs(ds) <= DIFF_EPS && Math.abs(de) <= DIFF_EPS) return null;
	if (Math.abs(ds - de) <= DIFF_EPS) return { shift: ds, head: 0, tail: 0 };
	const mag = speedOf(now);
	let head: number;
	let tail: number;
	if (looping) {
		[head, tail] = [ds, de];
	} else if (reversed(now)) {
		head = (was.source_out - now.source_out) / mag;
		tail = (was.source_in - now.source_in) / mag;
	} else {
		head = (now.source_in - was.source_in) / mag;
		tail = (now.source_out - was.source_out) / mag;
	}
	return { shift: ds - head, head, tail };
}

/**
 * `clipId` has just been edited from `was` to what it is now: carry the change to its
 * linked partners. A **move** (same length, new start) moves them by the same Δt. A
 * **trim** moves a partner's edge by the same amount *when the partner shares that edge*
 * with the clip as it was (within `ADJACENT_EPS`), clamped to the footage the partner
 * has. It writes no overlap check — like a trim, it leaves what the ripple pass or the
 * user is about to settle. Throws when a partner is on a locked track, would be trimmed
 * away, or would start before 0. Returns the partners as they stand afterwards.
 */
export function carryExtentEdit(timeline: Timeline, clipId: string, was: Clip, footage: SourceLimits): Clip[] {
	const now = clipById(timeline, clipId);
	if (!now) throw clipNotFound(clipId);
	const looping = footage.get(now.asset_id) === Infinity;
	const edit = extentEdit(was, now, looping);
	if (!edit) return [];
	const out: Clip[] = [];
	// Validate every partner before writing any, so a refusal changes nothing.
	const updates: [number, number, Clip][] = [];
	for (const partnerId of unlockedPartners(timeline, clipId, new Set([clipId]))) {
		const [pt, pc] = locateIndex(timeline, partnerId)!;
		const p = structuredClone(timeline.tracks[pt].clips[pc]);
		const limit = footage.get(p.asset_id) ?? Infinity;
		const pLooping = !Number.isFinite(limit);
		const headShared = Math.abs(edit.head) > DIFF_EPS && Math.abs(p.timeline_start - was.timeline_start) <= ADJACENT_EPS;
		const tailShared = Math.abs(edit.tail) > DIFF_EPS && Math.abs(endOf(p) - endOf(was)) <= ADJACENT_EPS;
		if (Math.abs(edit.shift) > DIFF_EPS) p.timeline_start += edit.shift;
		const [headRoom, tailRoom] = handles(p, limit);
		if (headShared) {
			const by = edit.head < 0 && !pLooping ? Math.max(edit.head, -headRoom) : edit.head;
			moveHead(p, by, pLooping);
		}
		if (tailShared) {
			const by = edit.tail > 0 && !pLooping ? Math.min(edit.tail, tailRoom) : edit.tail;
			moveTail(p, by, pLooping);
		}
		if (clipDuration(p) <= DIFF_EPS)
			throw invalid(`the linked clip on ${timeline.tracks[pt].name} would be trimmed away — edit with links off`);
		if (p.timeline_start < -DIFF_EPS)
			throw invalid(`the linked clip on ${timeline.tracks[pt].name} would start before the beginning of the timeline`);
		p.timeline_start = Math.max(p.timeline_start, 0);
		clampFades(p);
		updates.push([pt, pc, p]);
	}
	for (const [pt, pc, p] of updates) {
		timeline.tracks[pt].clips[pc] = p;
		out.push(structuredClone(p));
	}
	return out;
}

/**
 * `carryExtentEdit` for every clip an edit changed, read off a snapshot: a lane-level op
 * (the beat snap) retimes clips without knowing about links, and this carries each one's
 * change to its partners afterwards. A group where more than one member changed is left
 * alone — the edit named them explicitly.
 */
export function carryLinksSince(timeline: Timeline, before: Timeline, footage: SourceLimits) {
	if (!timeline.tracks.some((t) => t.clips.some((c) => c.link_id))) return;
	const changed = (was: Clip, now: Clip) =>
		Math.abs(was.timeline_start - now.timeline_start) > DIFF_EPS ||
		Math.abs(was.source_in - now.source_in) > DIFF_EPS ||
		Math.abs(was.source_out - now.source_out) > DIFF_EPS;
	const drivers = new Map<string, string[]>();
	for (const clip of timeline.tracks.flatMap((t) => t.clips)) {
		const was = clip.link_id ? clipById(before, clip.id) : undefined;
		if (!clip.link_id || !was) continue;
		if (changed(was, clip)) drivers.set(clip.link_id, [...(drivers.get(clip.link_id) ?? []), clip.id]);
	}
	for (const ids of drivers.values()) {
		if (ids.length !== 1) continue;
		const was = structuredClone(clipById(before, ids[0])!);
		const scratch: Timeline = structuredClone(timeline);
		carryExtentEdit(scratch, ids[0], was, footage);
		timeline.tracks = scratch.tracks;
	}
}

// ---- split -----------------------------------------------------------------------

/** Split one clip at timeline time `at` into two adjacent halves; the right half is a new
 *  clip (new id, no transition, no link). `at` must lie strictly inside the clip. */
export function splitClip(timeline: Timeline, clipId: string, at: number): [Clip, Clip] {
	const found = locateIndex(timeline, clipId);
	if (!found) throw clipNotFound(clipId);
	const [ti, ci] = found;
	const clip = timeline.tracks[ti].clips[ci];
	if (at <= clip.timeline_start || at >= endOf(clip)) throw invalid('split point must lie strictly inside the clip');
	const mag = speedOf(clip);
	const offset = (at - clip.timeline_start) * mag;
	const right: Clip = structuredClone(clip);
	right.id = newId();
	right.timeline_start = at;
	right.transition_in = null; // the transition stays with the left (start) half
	delete right.link_id;
	if (reversed(clip)) {
		const splitSrc = Math.min(Math.max(clip.source_out - offset, clip.source_in), clip.source_out);
		right.source_out = splitSrc;
		clip.source_in = splitSrc;
	} else {
		const splitSrc = Math.min(Math.max(clip.source_in + offset, clip.source_in), clip.source_out);
		right.source_in = splitSrc;
		clip.source_out = splitSrc;
	}
	timeline.tracks[ti].clips.splice(ci + 1, 0, right);
	return [structuredClone(clip), structuredClone(right)];
}

/**
 * **Split** `clipId` at `at` *and* every linked partner that has `at` inside it (a partner
 * that does not reach that moment is left whole); the new right halves are linked to each
 * other, the left halves keep the group. A partner on a locked track that would be split
 * refuses the whole edit. Returns the named clip's `[left, right]`.
 */
export function splitClipLinked(timeline: Timeline, clipId: string, at: number): [Clip, Clip] {
	const partners = linkPartners(timeline, clipId).filter((id) => {
		const c = clipById(timeline, id)!;
		return c.timeline_start + DIFF_EPS < at && at < endOf(c) - DIFF_EPS;
	});
	for (const id of partners) {
		const [ti] = locateIndex(timeline, id)!;
		if (timeline.tracks[ti].locked) throw lockedPartner(timeline.tracks[ti]);
	}
	const [left, right] = splitClip(timeline, clipId, at);
	const halves = [right.id];
	for (const id of partners) halves.push(splitClip(timeline, id, at)[1].id);
	relinkNewHalves(timeline, halves);
	return [left, structuredClone(clipById(timeline, right.id)!)];
}

// ---- remove ----------------------------------------------------------------------

/** Remove a clip and close the gap it leaves: every later clip on the same track shifts
 *  left by its duration. */
export function rippleDeleteClip(timeline: Timeline, clipId: string) {
	const found = locateIndex(timeline, clipId);
	if (!found) throw clipNotFound(clipId);
	const [ti, ci] = found;
	const track = timeline.tracks[ti];
	const removed = track.clips[ci];
	const dur = clipDuration(removed);
	const from = removed.timeline_start;
	track.clips.splice(ci, 1);
	for (const c of track.clips) if (c.timeline_start >= from) c.timeline_start = Math.max(c.timeline_start - dur, 0);
}

/** `rippleDeleteClip` on the clip and each of its linked partners, every track closing up
 *  behind its own. A partner on a locked track refuses the lot. Returns how many were deleted. */
export function rippleDeleteLinked(timeline: Timeline, clipId: string): number {
	if (!locateIndex(timeline, clipId)) throw clipNotFound(clipId);
	const partners = unlockedPartners(timeline, clipId, new Set([clipId]));
	rippleDeleteClip(timeline, clipId);
	for (const p of partners) rippleDeleteClip(timeline, p);
	return 1 + partners.length;
}

// ---- cut a source range ----------------------------------------------------------

/** Cut a **source-time** range out of a clip: split around the intersection of
 *  `[from, to]` with its source window, the middle piece removed, and later clips on the
 *  track ripple left to close the gap. Returns the kept pieces in play order. A tail
 *  piece is a new clip with no link. */
export function cutClipRange(timeline: Timeline, clipId: string, from: number, to: number): Clip[] {
	const found = locateIndex(timeline, clipId);
	if (!found) throw clipNotFound(clipId);
	const [ti, ci] = found;
	const track = timeline.tracks[ti];
	const clip = track.clips[ci];
	const a = Math.max(from, clip.source_in);
	const b = Math.min(to, clip.source_out);
	if (b - a <= 1e-9) throw invalid("range does not overlap the clip's source window");
	const mag = speedOf(clip);
	const removed = (b - a) / mag;
	const [head, tail] = reversed(clip)
		? [
				[b, clip.source_out],
				[clip.source_in, a]
			]
		: [
				[clip.source_in, a],
				[b, clip.source_out]
			];
	const headOk = head[1] - head[0] > 1e-9;
	const tailOk = tail[1] - tail[0] > 1e-9;
	const pieces: Clip[] = [];
	let cursor = clip.timeline_start;
	if (headOk) {
		const p = structuredClone(clip);
		[p.source_in, p.source_out] = head;
		p.timeline_start = cursor;
		if (tailOk) p.fade_out = 0;
		cursor = endOf(p);
		pieces.push(p);
	}
	if (tailOk) {
		const p = structuredClone(clip);
		[p.source_in, p.source_out] = tail;
		p.timeline_start = cursor;
		if (headOk) {
			p.id = newId();
			p.fade_in = 0;
			p.transition_in = null;
			delete p.link_id;
		}
		pieces.push(p);
	}
	track.clips.splice(ci, 1);
	for (const c of track.clips)
		if (c.timeline_start > clip.timeline_start + 1e-9) c.timeline_start = Math.max(c.timeline_start - removed, 0);
	track.clips.push(...pieces.map((p) => structuredClone(p)));
	track.clips.sort((x, y) => x.timeline_start - y.timeline_start);
	return pieces;
}

/**
 * `cutClipRange` on the clip *and* its linked partners: the stretch of **timeline** the
 * cut removes is taken out of every partner it overlaps too (a partner of another asset,
 * or at another offset, loses the same moment, not the same source span), each closing
 * the gap on its own track. A partner the cut misses is untouched; one on a locked track
 * refuses the lot. The tail pieces the cut makes are linked to each other. Returns the
 * named clip's kept pieces.
 */
export function cutClipRangeLinked(timeline: Timeline, clipId: string, from: number, to: number): Clip[] {
	const clip = clipById(timeline, clipId);
	if (!clip) throw clipNotFound(clipId);
	const a = Math.max(from, clip.source_in);
	const b = Math.min(to, clip.source_out);
	const partners = unlockedPartners(timeline, clipId, new Set([clipId]));
	if (partners.length === 0 || b - a <= 1e-9) return cutClipRange(timeline, clipId, from, to);
	const spanA = sourceToTimeline(clip, a);
	const spanB = sourceToTimeline(clip, b);
	const span: [number, number] = [Math.min(spanA, spanB), Math.max(spanA, spanB)];
	const cuts: [string, number, number][] = [];
	for (const partner of partners) {
		const p = clipById(timeline, partner)!;
		const lo = Math.max(span[0], p.timeline_start);
		const hi = Math.min(span[1], endOf(p));
		if (hi - lo <= DIFF_EPS) continue;
		const [s0, s1] = [timelineToSource(p, lo), timelineToSource(p, hi)];
		cuts.push([partner, Math.min(s0, s1), Math.max(s0, s1)]);
	}
	const before = new Set(timeline.tracks.flatMap((t) => t.clips.map((c) => c.id)));
	const scratch: Timeline = structuredClone(timeline);
	const kept = cutClipRange(scratch, clipId, from, to);
	for (const [partner, lo, hi] of cuts) cutClipRange(scratch, partner, lo, hi);
	const fresh = scratch.tracks.flatMap((t) => t.clips).filter((c) => !before.has(c.id)).map((c) => c.id);
	relinkNewHalves(scratch, fresh);
	timeline.tracks = scratch.tracks;
	return kept.map((c) => structuredClone(clipById(timeline, c.id) ?? c));
}

/** Where a source timestamp of `clip` lands on the timeline (kerf-core's `Clip::source_to_timeline`). */
function sourceToTimeline(clip: Clip, source: number): number {
	const offset = reversed(clip) ? clip.source_out - source : source - clip.source_in;
	return clip.timeline_start + offset / speedOf(clip);
}

/** The source timestamp playing at timeline time `time` (kerf-core's `Clip::timeline_to_source`). */
function timelineToSource(clip: Clip, time: number): number {
	const along = (time - clip.timeline_start) * speedOf(clip);
	return reversed(clip) ? clip.source_out - along : clip.source_in + along;
}

// ---- speed -----------------------------------------------------------------------

/**
 * Retime `clipId` to `speed` and every linked partner by the same *ratio* (a partner at
 * 1× next to a clip going 1× → 2× goes to 2×; one already at 0.5× goes to 1×; a sign flip
 * reverses it too). Each keeps its window and start, so each track's length changes by
 * its own. Throws when a partner's new speed would be zero or not finite, or a partner is
 * on a locked track. Returns the named clip.
 */
export function setSpeedLinked(timeline: Timeline, clipId: string, speed: number): Clip {
	if (!Number.isFinite(speed) || speed === 0) throw invalid('speed must be a non-zero, finite number');
	const found = locateIndex(timeline, clipId);
	if (!found) throw clipNotFound(clipId);
	const [ti, ci] = found;
	const old = timeline.tracks[ti].clips[ci].speed ?? 1;
	const ratio = old === 0 || !Number.isFinite(old) ? 1 : speed / old;
	const updates: [Clip, number][] = [];
	for (const partner of unlockedPartners(timeline, clipId, new Set([clipId]))) {
		const p = clipById(timeline, partner)!;
		const next = (p.speed ?? 1) * ratio;
		if (!Number.isFinite(next) || next === 0)
			throw invalid('a linked clip would end up with no speed — set the speed with links off');
		updates.push([p, next]);
	}
	timeline.tracks[ti].clips[ci].speed = speed;
	for (const [p, next] of updates) p.speed = next;
	return structuredClone(timeline.tracks[ti].clips[ci]);
}

// ---- split and remove ------------------------------------------------------------

/**
 * `cuts` widened with the partners of the clips it cuts: a partner not already named, on a
 * track the request does not already cut, with `at` inside it, is cut at the same time (a
 * partner `at` does not fall inside is untouched). A partner on a locked track that would
 * be cut refuses it. The named cuts come first, unchanged.
 */
export function withLinkedCuts(timeline: Timeline, cuts: readonly ClipCut[]): ClipCut[] {
	const named = new Set(cuts.map((c) => c.clip_id));
	const lanes = new Set<number>();
	for (const cut of cuts) {
		const at = locateIndex(timeline, cut.clip_id);
		if (at) lanes.add(at[0]);
	}
	const out = [...cuts];
	for (const cut of cuts) {
		for (const partner of linkPartners(timeline, cut.clip_id)) {
			if (named.has(partner)) continue;
			const [ti, ci] = locateIndex(timeline, partner)!;
			const p = timeline.tracks[ti].clips[ci];
			if (!(p.timeline_start + DIFF_EPS < cut.at && cut.at < endOf(p) - DIFF_EPS) || lanes.has(ti)) continue;
			lanes.add(ti);
			if (timeline.tracks[ti].locked) throw lockedPartner(timeline.tracks[ti]);
			out.push({ clip_id: partner, at: cut.at });
		}
	}
	return out;
}
