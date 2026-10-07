// The linked edits as the project makes them — the faithful TS mirror of `Project`'s
// link-aware ops and of `Project::run_edit` around them (crates/kerf-core/src/project.rs).
// Pure over a timeline, used by the browser harness in `api.ts`, and replayed by the
// differential test (`links-corpus.test.ts`) against a corpus that kerf-core itself writes —
// so a rule changed in Rust has to change here, or that test names the edit it broke.
//
// `runEdit` is `run_edit`: the edit runs on a scratch copy, then — when ripple applies — the
// per-lane ripple, then — with links in force and something linked — the sync lock
// (`conformLinks`, the clips the edit named as its anchors) and the sync guard, and finally a
// link left with one clip is dissolved. A throw leaves the caller's timeline exactly as it was.
//
// Each op returns the timeline it leaves, what it returned (`result`) and the revision `label`
// the project would record — a group edit counts the partners it carried, which only the edit
// knows.

import { type SourceLimits } from './edit-modes';
import {
	clipById,
	clipNotFound,
	dissolveAllOrphans,
	firstSyncBreak,
	hasLinks,
	invalid,
	linkClips as linkClipsOn,
	linkPartners,
	locateIndex,
	newId,
	syncBreakError,
	unlinkClips as unlinkClipsOn,
	unlockedPartners,
	withLinkPartners
} from './link-groups';
import {
	carryExtentEdit,
	carryLinksSince,
	cutClipRange,
	cutClipRangeLinked,
	detachAudio,
	detachAudioMany,
	reattachAudio,
	rippleDeleteClip,
	rippleDeleteLinked,
	setSpeedLinked,
	splitClip,
	splitClipLinked,
	withLinkedCuts,
	withLinkedMoves,
	type Detached,
	type DetachedMany
} from './links';
import { moveClips as moveClipsOn, removeClips as removeClipsOn } from './multi-edit';
import { conformLinks, rippleLanes } from './ripple';
import { splitRemoveClips as splitRemoveClipsOn } from './edit-modes';
import type { Asset, Clip, ClipCut, ClipMove, SplitSide, Timeline, Track } from './types';
import { clipDuration } from './types';

/** What a call is made under: the project's ripple mode (or the call's override), whether
 *  links are in force (`link: false` is off), how far each asset's footage reaches and which
 *  assets carry sound. */
export interface EditEnv {
	ripple: boolean;
	links: boolean;
	footage: SourceLimits;
	hasAudio: (assetId: string) => boolean;
}

export interface Edited<R = void> {
	timeline: Timeline;
	result: R;
	/** The revision label the project records for it. */
	label: string;
}

interface RunOptions {
	ripple: boolean;
	links: boolean;
	/** The clips the edit named: their track speaks for a link group when the sync lock must choose. */
	anchors?: readonly string[];
}

/** `Project::run_edit`: run `edit` on a scratch copy of `timeline` and settle what it left. */
export function runEdit<R>(timeline: Timeline, opts: RunOptions, edit: (scratch: Timeline) => R): { timeline: Timeline; result: R } {
	let next: Timeline = structuredClone(timeline);
	const sync = opts.links && hasLinks(next);
	const before = opts.ripple || sync ? structuredClone(next) : null;
	const result = edit(next);
	if (before) {
		if (opts.ripple) next = rippleLanes(next, before);
		if (sync) {
			conformLinks(next, before, new Set(opts.anchors ?? []));
			const broke = firstSyncBreak(next, before);
			if (broke) throw syncBreakError(broke);
		}
	}
	if (hasLinks(next)) dissolveAllOrphans(next);
	return { timeline: next, result };
}

const edited = <R>(done: { timeline: Timeline; result: R }, label: string): Edited<R> => ({ ...done, label });

/** The track a clip is on, and its index there. */
function locate(tl: Timeline, clipId: string): [Track, number] | null {
	const at = locateIndex(tl, clipId);
	return at ? [tl.tracks[at[0]], at[1]] : null;
}

// ---- split / trim / cut ----------------------------------------------------------

/** `Project::split_at`: the clip, and every partner that has `at` inside it, split there. */
export function splitAt(tl: Timeline, env: EditEnv, clipId: string, at: number): Edited<[Clip, Clip]> {
	return edited(
		runEdit(tl, env, (t) => (env.links ? splitClipLinked(t, clipId, at) : splitClip(t, clipId, at))),
		'Split clip'
	);
}

/** `Project::trim`: new source in/out (and start) for a clip; the partners follow the edge. */
export function trim(
	tl: Timeline,
	env: EditEnv,
	clipId: string,
	sourceIn?: number | null,
	sourceOut?: number | null,
	timelineStart?: number | null
): Edited<void> {
	return edited(
		runEdit(tl, { ...env, anchors: [clipId] }, (t) => {
			const found = locate(t, clipId);
			if (!found) throw clipNotFound(clipId);
			const [track, ci] = found;
			const clip = track.clips[ci];
			const was = structuredClone(clip);
			if (sourceIn != null) clip.source_in = sourceIn;
			if (sourceOut != null) clip.source_out = sourceOut;
			if (clip.source_out <= clip.source_in) throw invalid('source_out must be greater than source_in');
			if (timelineStart != null) {
				clip.timeline_start = Math.max(0, timelineStart);
				track.clips.sort((a, b) => a.timeline_start - b.timeline_start);
			}
			if (env.links) carryExtentEdit(t, clipId, was, env.footage);
		}),
		'Trim clip'
	);
}

/** `Project::cut_clip_range`: a source span out of a clip; never rippled, it closes its own gap. */
export function cutRange(tl: Timeline, env: EditEnv, clipId: string, from: number, to: number): Edited<Clip[]> {
	return edited(
		runEdit(tl, { ...env, ripple: false, anchors: [clipId] }, (t) =>
			env.links ? cutClipRangeLinked(t, clipId, from, to) : cutClipRange(t, clipId, from, to)
		),
		'Cut range'
	);
}

/** `Project::ripple_delete`: remove a clip and close its track's gap; never rippled again. */
export function rippleDelete(tl: Timeline, env: EditEnv, clipId: string): Edited<void> {
	return edited(
		runEdit(tl, { ...env, ripple: false, anchors: [clipId] }, (t) => {
			if (env.links) rippleDeleteLinked(t, clipId);
			else rippleDeleteClip(t, clipId);
		}),
		'Ripple delete'
	);
}

// ---- remove / speed / move / reorder ---------------------------------------------

/** `Timeline::remove_clips_linked`: `ids` and every clip linked to one of them, all or nothing —
 *  a partner on a locked track refuses the lot with the error that names the link. */
export function removeClipsLinked(tl: Timeline, ids: readonly string[]): number {
	const named = new Set(ids);
	for (const id of ids) if (locateIndex(tl, id)) unlockedPartners(tl, id, named);
	return removeClipsOn(tl, withLinkPartners(tl, [...ids]));
}

/** `Project::remove_clips`: the clips and their partners, one revision. */
export function removeClips(tl: Timeline, env: EditEnv, ids: readonly string[]): Edited<number> {
	const done = runEdit(tl, { ...env, anchors: ids }, (t) => (env.links ? removeClipsLinked(t, ids) : removeClipsOn(t, [...ids])));
	const n = done.result;
	const label = n === 1 ? (env.ripple ? 'Ripple delete' : 'Remove clip') : `${env.ripple ? 'Ripple delete' : 'Remove'} ${n} clips`;
	return edited(done, label);
}

/** `Project::remove`: a clip with partners is a group removal; one without is a plain remove. */
export function remove(tl: Timeline, env: EditEnv, clipId: string): Edited<number> {
	if (env.links && linkPartners(tl, clipId).length > 0) return removeClips(tl, env, [clipId]);
	const done = runEdit(tl, { ...env, anchors: [clipId] }, (t) => {
		const found = locate(t, clipId);
		if (!found) throw clipNotFound(clipId);
		found[0].clips.splice(found[1], 1);
		return 1;
	});
	return edited(done, 'Remove clip');
}

/** `Project::set_speed`: the clip and its partners retimed by the same ratio. */
export function setSpeed(tl: Timeline, env: EditEnv, clipId: string, speed: number): Edited<Clip> {
	if (!Number.isFinite(speed) || speed === 0) throw invalid('speed must be a non-zero, finite number');
	return edited(
		runEdit(tl, { ...env, anchors: [clipId] }, (t) => {
			if (env.links) return setSpeedLinked(t, clipId, speed);
			const found = locate(t, clipId);
			if (!found) throw clipNotFound(clipId);
			found[0].clips[found[1]].speed = speed;
			return structuredClone(found[0].clips[found[1]]);
		}),
		'Set speed'
	);
}

/** `Project::move_clips`: a group move, never rippled; the partners travel by the same Δt. */
export function moveClips(tl: Timeline, env: EditEnv, moves: readonly ClipMove[]): Edited<Clip[]> {
	const done = runEdit(tl, { ...env, ripple: false, anchors: moves.map((m) => m.clip_id) }, (t) => {
		const all = env.links ? withLinkedMoves(t, [...moves]) : [...moves];
		return moveClipsOn(t, all);
	});
	return edited(done, done.result.length === 1 ? 'Move clip' : `Move ${done.result.length} clips`);
}

/** `Project::reorder`: re-lay one track with a clip at a new index; its linked partners follow. */
export function reorder(tl: Timeline, env: EditEnv, trackId: string, clipId: string, newIndex: number): Edited<void> {
	return edited(
		runEdit(tl, { ...env, ripple: false, anchors: [clipId] }, (t) => {
			const track = t.tracks.find((x) => x.id === trackId);
			if (!track) throw new Error(`track not found: ${trackId}`);
			const cur = track.clips.findIndex((c) => c.id === clipId);
			if (cur < 0) throw clipNotFound(clipId);
			const [clip] = track.clips.splice(cur, 1);
			track.clips.splice(Math.min(newIndex, track.clips.length), 0, clip);
			let cursor = 0;
			for (const c of track.clips) {
				c.timeline_start = cursor;
				cursor += clipDuration(c);
			}
		}),
		'Reorder clip'
	);
}

/** `Project::split_remove_clips`: cut clips at a time and drop one side, with their partners. */
export function splitRemoveClips(tl: Timeline, env: EditEnv, cuts: readonly ClipCut[], side: SplitSide): Edited<Clip[]> {
	const what = side === 'left' ? 'Split and remove left' : 'Split and remove right';
	const done = runEdit(tl, { ...env, anchors: cuts.map((c) => c.clip_id) }, (t) => {
		const all = env.links ? withLinkedCuts(t, cuts) : [...cuts];
		return splitRemoveClipsOn(t, all, side);
	});
	return edited(done, done.result.length > 1 ? `${what} (${done.result.length} clips)` : what);
}

/** `Project::snap_to_beats`'s re-sync half: `carryLinksSince` over a lane-level retime. */
export function carrySince(tl: Timeline, env: EditEnv, retime: (t: Timeline) => void): Edited<void> {
	return edited(
		runEdit(tl, { ...env, ripple: false }, (t) => {
			const snapshot = structuredClone(t);
			retime(t);
			if (env.links) carryLinksSince(t, snapshot, env.footage);
		}),
		'Cut to the beat'
	);
}

// ---- detach / reattach / extract / add audio ---------------------------------------

/** `Project::detach_audio`. */
export function detach(tl: Timeline, env: EditEnv, clipId: string): Edited<Detached> {
	return edited(
		runEdit(tl, { ...env, ripple: false }, (t) => {
			const clip = clipById(t, clipId);
			if (!clip) throw clipNotFound(clipId);
			return detachAudio(t, clipId, env.hasAudio(clip.asset_id));
		}),
		'Detach audio'
	);
}

/** `Project::detach_audio_clips`: several clips, one revision; a clip that cannot be is skipped. */
export function detachClips(tl: Timeline, env: EditEnv, ids: readonly string[]): Edited<DetachedMany> {
	const done = runEdit(tl, { ...env, ripple: false }, (t) => detachAudioMany(t, ids, env.hasAudio));
	return edited(done, done.result.detached.length === 1 ? 'Detach audio' : `Detach audio (${done.result.detached.length} clips)`);
}

/** `Project::reattach_audio`. */
export function reattach(tl: Timeline, env: EditEnv, clipId: string): Edited<Clip> {
	return edited(
		runEdit(tl, { ...env, ripple: false }, (t) => reattachAudio(t, clipId)),
		'Reattach audio'
	);
}

/** `Project::extract_audio`: every clip of the asset on a video track still playing its own
 *  sound is detached; nothing to detach is an error, not a quiet fall-through. */
export function extractAudio(tl: Timeline, env: EditEnv, assetId: string): Edited<DetachedMany> {
	return edited(
		runEdit(tl, { ...env, ripple: false }, (t) => {
			const onVideo = t.tracks.filter((x) => x.kind === 'video').flatMap((x) => x.clips).filter((c) => c.asset_id === assetId);
			const sounding = onVideo.filter((c) => c.source_audio !== false).map((c) => c.id);
			if (sounding.length === 0)
				throw invalid(
					onVideo.length === 0
						? 'no clip of this asset is on a video track, so there is no sound of its own to extract — add_asset_audio puts its audio on an audio track'
						: "this asset's sound is already on an audio track"
				);
			return detachAudioMany(t, sounding, () => true);
		}),
		'Extract audio'
	);
}

/** `Project::add_asset_audio`: the asset's whole audio as a clip at the end of the first audio track. */
export function addAssetAudio(tl: Timeline, env: EditEnv, asset: Pick<Asset, 'id' | 'duration'>): Edited<Clip> {
	return edited(
		runEdit(tl, { ...env, ripple: false }, (t) => {
			let track = t.tracks.find((x) => x.kind === 'audio');
			if (!track) {
				track = { id: newId(), kind: 'audio', name: 'A1', clips: [] };
				t.tracks.push(track);
			}
			const start = track.clips.reduce((m, c) => Math.max(m, c.timeline_start + clipDuration(c)), 0);
			const clip: Clip = {
				id: newId(),
				asset_id: asset.id,
				source_in: 0,
				source_out: asset.duration,
				timeline_start: start,
				volume: 1,
				fade_in: 0,
				fade_out: 0
			};
			track.clips.push(clip);
			return structuredClone(clip);
		}),
		'Add audio'
	);
}

/** `Project::link_clips`. */
export function link(tl: Timeline, env: EditEnv, ids: readonly string[]): Edited<string> {
	return edited(
		runEdit(tl, { ...env, ripple: false }, (t) => linkClipsOn(t, [...ids])),
		`Link ${ids.length} clips`
	);
}

/** `Project::unlink_clips`. */
export function unlink(tl: Timeline, env: EditEnv, ids: readonly string[]): Edited<number> {
	return edited(
		runEdit(tl, { ...env, ripple: false }, (t) => unlinkClipsOn(t, [...ids])),
		'Unlink clips'
	);
}

// ---- paste -----------------------------------------------------------------------

/** One clipboard entry: the clip's data plus the track it should land on. */
export interface PastePlacement {
	track_id: string;
	clip: Clip;
}

/** `Project::insert_clips`: copies of `placements`, the earliest landing at `at`, each with a new
 *  id; all or nothing — a copy that would overlap what is there or another copy refuses the lot.
 *  A copy of a linked *group* is linked to its fellow copies under a fresh id, and a picture
 *  copy left without its sound gets its own back (see `pasteLinks`). Never rippled. */
export function insertClips(tl: Timeline, env: EditEnv, placements: readonly PastePlacement[], at: number): Edited<Clip[]> {
	if (placements.length === 0) throw invalid('no clips to insert');
	const where = Math.max(at, 0);
	const base = Math.min(...placements.map((p) => p.clip.timeline_start));
	return edited(
		runEdit(tl, { ...env, ripple: false }, (t) => {
			const kindOf = (id: string) => t.tracks.find((x) => x.id === id)?.kind;
			const fresh = pasteLinks(
				placements.map((p) => {
					const kind = kindOf(p.track_id);
					return { clip: p.clip, kind: kind ?? 'video' };
				})
			);
			const staged: { ti: number; clip: Clip }[] = placements.map((p) => {
				const ti = t.tracks.findIndex((x) => x.id === p.track_id);
				if (ti < 0) throw new Error(`track not found: ${p.track_id}`);
				const copy: Clip = JSON.parse(JSON.stringify(p.clip));
				copy.id = newId();
				copy.timeline_start = where + (p.clip.timeline_start - base);
				fresh(p.clip, copy, t.tracks[ti].kind);
				return { ti, clip: copy };
			});
			staged.forEach(({ ti, clip }, i) => {
				const [start, end] = [clip.timeline_start, clip.timeline_start + clipDuration(clip)];
				const hitsExisting = t.tracks[ti].clips.some((c) => start < c.timeline_start + clipDuration(c) && c.timeline_start < end);
				const hitsSibling = staged.some(
					(o, j) => j !== i && o.ti === ti && start < o.clip.timeline_start + clipDuration(o.clip) && o.clip.timeline_start < end
				);
				if (hitsExisting || hitsSibling) throw invalid('pasted clips would overlap existing clips — move the playhead to free space');
			});
			for (const { ti, clip } of staged) {
				t.tracks[ti].clips.push(clip);
				t.tracks[ti].clips.sort((a, b) => a.timeline_start - b.timeline_start);
			}
			return staged.map((x) => structuredClone(x.clip));
		}),
		'Insert clips'
	);
}

/** `Project::duplicate_clips`: `insertClips` of the named clips, each staying on its track. */
export function duplicateClips(tl: Timeline, env: EditEnv, ids: readonly string[], at: number): Edited<Clip[]> {
	const placements = ids.map((id) => {
		const found = locate(tl, id);
		if (!found) throw clipNotFound(id);
		return { track_id: found[0].id, clip: structuredClone(found[0].clips[found[1]]) };
	});
	return insertClips(tl, env, placements, at);
}

/** What pasting does to link ids and to a detached picture's sound (`Project::insert_clips`).
 *  A copy is a new clip, so it cannot share a group with its original — but copies of a linked
 *  *group* are linked to each other under a fresh id, and a copy whose partner was not pasted
 *  with it is unlinked. A muted picture is silent *because* its sound plays from its partner:
 *  pasted without a partner that carries the same footage's audio it gets its own sound back.
 *  Returns what to call on each `[original, copy, copy's track kind]` after the copies exist. */
export function pasteLinks(
	originals: readonly { clip: Clip; kind: Track['kind'] }[]
): (original: Clip, copy: Clip, kind: Track['kind']) => void {
	const members = new Map<string, number>();
	for (const { clip } of originals) if (clip.link_id) members.set(clip.link_id, (members.get(clip.link_id) ?? 0) + 1);
	const fresh = new Map<string, string>();
	for (const [link, n] of members) if (n >= 2) fresh.set(link, newId());
	// Which `(fresh link, asset)` pairs have an audio-track copy carrying them.
	const carriers = new Set<string>();
	for (const { clip, kind } of originals) {
		const next = clip.link_id ? fresh.get(clip.link_id) : undefined;
		if (kind === 'audio' && next) carriers.add(`${next}|${clip.asset_id}`);
	}
	return (original, copy, kind) => {
		const next = original.link_id ? fresh.get(original.link_id) : undefined;
		if (next) copy.link_id = next;
		else delete copy.link_id;
		if (kind === 'video' && copy.source_audio === false && !(next && carriers.has(`${next}|${copy.asset_id}`))) {
			delete copy.source_audio;
		}
	};
}
