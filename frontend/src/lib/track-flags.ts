/* The words of a track header's flags — pure. A header repeats the same five buttons on every track, so
 * the name of each says *which* track ("Mute A1"), the way the mixer strip's do: read alone, in a list of
 * controls, "Toggle mute" five times over tells a screen reader nothing. The tooltip is the longer sentence
 * (what the click will do now). */

import type { StreamKind } from './types';

export interface FlagTrack {
	name: string;
	kind: StreamKind;
	muted?: boolean;
	solo?: boolean;
	locked?: boolean;
	duck?: boolean;
}

export interface FlagCopy {
	/** The accessible name: constant while the button is a toggle (its pressed state says which way it is). */
	name: string;
	/** The tooltip: what a click does now. */
	tip: string;
}

export interface TrackFlagCopy {
	mute: FlagCopy;
	solo: FlagCopy;
	lock: FlagCopy;
	duck: FlagCopy;
	remove: FlagCopy;
}

export function trackFlagCopy(t: FlagTrack): TrackFlagCopy {
	const video = t.kind === 'video';
	return {
		mute: {
			name: `${video ? 'Hide' : 'Mute'} ${t.name}`,
			tip: t.muted ? (video ? 'Hidden — click to show' : 'Muted — click to unmute') : video ? 'Hide this track' : 'Mute this track'
		},
		solo: { name: `Solo ${t.name}`, tip: t.solo ? 'Soloed — click to clear' : 'Solo this track' },
		lock: { name: `Lock ${t.name}`, tip: t.locked ? 'Locked — click to unlock' : 'Lock this track against edits' },
		duck: {
			name: `Duck ${t.name}`,
			tip: t.duck
				? 'Ducking on — this track dips under the rest of the mix on export (the preview plays it at its fader)'
				: 'Duck this track under the rest of the mix on export (the preview plays it at its fader)'
		},
		remove: { name: `Remove ${t.name}`, tip: 'Remove track' }
	};
}
