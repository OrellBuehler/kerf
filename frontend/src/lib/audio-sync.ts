/* "Sync by audio" in the clip menu: when it is offered, why it is greyed out, and what the
 * toast says afterwards. The engine (`sync_clips_by_audio`) refuses the same cases; saying so
 * up front keeps the menu from offering what would only fail. */

import { linkPartners } from './link-groups';
import type { Asset, AudioSync, Timeline } from './types';

export interface SyncOffer {
	show: boolean;
	/** Why it cannot run, or `null`. */
	reason: string | null;
	/** The clip the clicked one is moved to. */
	referenceId: string | null;
}

/** Offered when exactly two clips are selected and one of them was right-clicked: the clicked
 *  clip moves into sync with the other. */
export function syncOffer(
	timeline: Pick<Timeline, 'tracks'>,
	assets: Pick<Asset, 'id' | 'streams'>[],
	selected: string[],
	clickedId: string
): SyncOffer {
	if (selected.length !== 2 || !selected.includes(clickedId)) return { show: false, reason: null, referenceId: null };
	const referenceId = selected.find((id) => id !== clickedId) ?? null;
	const find = (id: string | null) => {
		for (const track of timeline.tracks) {
			const clip = track.clips.find((c) => c.id === id);
			if (clip) return { track, clip };
		}
		return null;
	};
	const [ref, own] = [find(referenceId), find(clickedId)];
	const offer = (reason: string | null): SyncOffer => ({ show: true, reason, referenceId });
	if (!ref || !own) return offer('The clips are not on the timeline');
	for (const { clip } of [ref, own]) {
		if (!assets.find((a) => a.id === clip.asset_id)?.streams.some((s) => s.kind === 'audio')) return offer('Both clips need sound');
		if ((clip.speed ?? 1) !== 1) return offer('Both clips need to play at normal speed');
	}
	if (linkPartners(timeline as Timeline, clickedId).includes(ref.clip.id)) return offer('The clips are linked — unlink them first');
	if (own.track.locked) return offer(`Track ${own.track.name} is locked`);
	return offer(null);
}

const signed = (s: number) => `${s >= 0 ? '+' : '−'}${Math.abs(s).toFixed(3)} s`;

/** The toast after a sync. */
export function describeSync(s: AudioSync): string {
	const polarity = s.inverted ? ' (one recording has inverted polarity)' : '';
	if (s.applied) return `Synced by audio — moved ${signed(s.offset)}${polarity}`;
	return `No clear match in the sound (confidence ${s.confidence.toFixed(1)}) — the clip was left where it is${polarity}`;
}
