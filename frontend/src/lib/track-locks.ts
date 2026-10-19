// What a locked track means for the GUI's own edits, said before the backend refuses them (pure, bun-tested):
// the backend refuses every layout edit on a locked track (`Timeline::ensure_editable`), so a control that would
// only meet that refusal is off, with the reason, and a paste aimed at a locked track goes somewhere it can land.

import type { Placement } from './api';
import type { Timeline, Track } from './types';

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

/** The clipboard as it can land: each clip on the track it was copied from, unless that track is locked
 *  (or gone) — then on the first unlocked track of its kind. With no such track, the reason it cannot. */
export function pastePlacements(timeline: Timeline, clipboard: readonly Placement[]): { placements: Placement[] } | { reason: string } {
	const placements: Placement[] = [];
	for (const p of clipboard) {
		const source = timeline.tracks.find((t) => t.id === p.track_id);
		if (source && !source.locked) {
			placements.push(p);
			continue;
		}
		const kind = source?.kind ?? (timeline.tracks.find((t) => t.clips.some((c) => c.id === p.clip.id))?.kind ?? 'video');
		const open = timeline.tracks.find((t) => t.kind === kind && !t.locked);
		if (!open) return { reason: `Every ${kind === 'audio' ? 'audio' : 'video'} track is locked` };
		placements.push({ ...p, track_id: open.id });
	}
	return { placements };
}
