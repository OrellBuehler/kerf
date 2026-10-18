// The words and counts of the library's remove / relink flows — pure, so a test holds them (`library-actions.ts`
// is what asks and does).

import type { MissingAsset, Relinked, Timeline } from './types';

/** How many clips of `timeline` play `assetId`. */
export function clipsUsing(timeline: Pick<Timeline, 'tracks'> | null | undefined, assetId: string): number {
	return (timeline?.tracks ?? []).reduce((n, t) => n + t.clips.filter((c) => c.asset_id === assetId).length, 0);
}

/** The sentence after a relink: what was linked and — when it is a different recording — what that cost. */
export function relinkLine(name: string, done: Pick<Relinked, 'analysis_kept'>): string {
	return done.analysis_kept
		? `Relinked ${name}`
		: `Relinked ${name} — it is a different file, so its analysis was dropped; analyze it again if you need it`;
}

/** `2 media files can't be found`, for the toast and the banner. */
export function missingLine(list: readonly Pick<MissingAsset, 'name'>[]): string {
	if (list.length === 0) return '';
	if (list.length === 1) return `${list[0].name} can’t be found`;
	return `${list.length} media files can’t be found`;
}

/** The paragraph over the Relink dialog's list, for one missing file or several. */
export function relinkIntro(list: readonly Pick<MissingAsset, 'name'>[]): string {
	if (list.length === 0) return '';
	const one = list.length === 1;
	return `${missingLine(list)}. ${one ? 'It was' : 'They were'} moved, renamed or ${one ? 'is' : 'are'} on a drive that is not connected. Point ${
		one ? 'it' : 'each'
	} at the file it has become — the clips cut from ${one ? 'it' : 'them'} stay exactly as they are — or take it out of the project. Nothing is deleted from disk either way.`;
}

/** What removing says it did. */
export function removedLine(name: string, clipsRemoved: number): string {
	return clipsRemoved > 0 ? `Removed ${name} and ${clipsRemoved} ${clipsRemoved === 1 ? 'clip' : 'clips'}` : `Removed ${name}`;
}
