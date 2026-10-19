// What a locked track means for the GUI's own edits, said before the backend refuses them (pure, bun-tested):
// the backend refuses every layout edit on a locked track (`Timeline::ensure_editable`), so a control that would
// only meet that refusal is off, with the reason, and a paste aimed at a locked track goes somewhere it can land.

import type { Placement } from './api';
import type { Asset, Clip, Timeline, Track } from './types';

/** The track a clip is on when that track is locked, else `null`. */
export function lockedTrackOf(timeline: Timeline, clipId: string): Track | null {
	const track = timeline.tracks.find((t) => t.clips.some((c) => c.id === clipId));
	return track?.locked ? track : null;
}

/** The sentence a control on a locked track's clip says instead of acting, or `undefined`. */
export function lockedReason(timeline: Timeline, clipId: string): string | undefined {
	const track = lockedTrackOf(timeline, clipId);
	return track ? `Track ${track.name} is locked` : undefined;
}

/** Why the clips cannot be duplicated (a duplicate stays on its own track), or `null`. */
export function duplicateBlocked(timeline: Timeline, ids: readonly string[]): string | null {
	for (const id of ids) {
		const why = lockedReason(timeline, id);
		if (why) return why;
	}
	return null;
}

/** The kind of track a clip whose own track is gone goes on, from what its footage has: a title or a
 *  picture alone is video, sound alone is audio. Footage with both cannot say (a detached sound is a clip
 *  of its picture's own asset), nor can footage the bin no longer holds: `null`. */
export function clipKind(clip: Clip, assets: readonly Pick<Asset, 'id' | 'streams'>[]): Track['kind'] | null {
	if (clip.generator) return 'video';
	const streams = assets.find((a) => a.id === clip.asset_id)?.streams;
	if (!streams) return null;
	const picture = streams.some((s) => s.kind === 'video');
	const sound = streams.some((s) => s.kind === 'audio');
	if (picture === sound) return null;
	return picture ? 'video' : 'audio';
}

/** The clipboard as it can land: each clip on the track it was copied from, unless that track is locked —
 *  then on the first unlocked track of that track's kind — or gone, then on the first unlocked track of the
 *  clip's own kind (`clipKind`). With no such track, or no telling the kind, the reason it cannot. */
export function pastePlacements(
	timeline: Timeline,
	clipboard: readonly Placement[],
	assets: readonly Pick<Asset, 'id' | 'streams'>[]
): { placements: Placement[] } | { reason: string } {
	const placements: Placement[] = [];
	for (const p of clipboard) {
		const source = timeline.tracks.find((t) => t.id === p.track_id);
		if (source && !source.locked) {
			placements.push(p);
			continue;
		}
		const kind = source?.kind ?? clipKind(p.clip, assets);
		if (!kind) return { reason: 'A copied clip’s track is gone, and its footage does not say which kind of track it goes on' };
		const open = timeline.tracks.find((t) => t.kind === kind && !t.locked);
		if (!open) return { reason: `Every ${kind === 'audio' ? 'audio' : 'video'} track is locked` };
		placements.push({ ...p, track_id: open.id });
	}
	return { placements };
}

/** `effective_lock` as a forced accept lands it (`apply_staged`): a track of the proposal `landing` is locked when
 *  it is locked on the `live` cut now, or the proposal locked it where its `base` had not — a lock it only
 *  inherited, which the user has since lifted, is not put back. In place. */
export function landingLocks(landing: Timeline, base: Timeline, live: Timeline): void {
	const liveLocks = new Set(live.tracks.filter((t) => t.locked).map((t) => t.id));
	const baseLocks = new Set(base.tracks.filter((t) => t.locked).map((t) => t.id));
	for (const t of landing.tracks) t.locked = liveLocks.has(t.id) || (!!t.locked && !baseLocks.has(t.id));
}
