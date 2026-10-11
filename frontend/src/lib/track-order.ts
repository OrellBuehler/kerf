/* The order the timeline draws its tracks in — pure, and only the drawing. The project keeps its
 * tracks as V1, V2, …, A1, A2, …, and every edit, the export and the agent's tools go on
 * addressing them that way: a later video track composites on top of an earlier one. The
 * timeline stacks them the way the picture does — the track on top of the pile on top of the
 * screen, as every editing program draws it — so V2 sits above V1 and the videos sit above the
 * audio, with a rule between the two groups. Nothing here reorders `Timeline.tracks`. */

import type { StreamKind } from './types';

/** `tracks` as the timeline draws them: video from the topmost layer down to V1, then everything
 *  else (audio) in the order the project holds it. */
export function displayTracks<T extends { kind: StreamKind }>(tracks: readonly T[]): T[] {
	return [...tracks.filter((t) => t.kind === 'video').reverse(), ...tracks.filter((t) => t.kind !== 'video')];
}

/** Whether a rule is drawn above `track`: it is the first of the non-video tracks and a video
 *  track sits above it. `drawn` is `displayTracks`' answer. */
export function opensAudioGroup<T extends { kind: StreamKind }>(drawn: readonly T[], track: T): boolean {
	const i = drawn.indexOf(track);
	return i > 0 && track.kind !== 'video' && drawn[i - 1].kind === 'video';
}
