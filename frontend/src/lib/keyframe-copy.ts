// Copying one clip's animation onto another — pure. The engine could always do it (`copy_keyframes`: the keyed
// numbers of a clip, onto another clip, shifted by an offset) and nothing in the interface reached it. The
// interface is Copy on the clip that is animated and Paste on the clip that should be: the source is remembered
// as a clip id, so a paste always reads the animation as it is *now*, not as it was when it was copied.

import { PROPERTIES, isKeyed, propertyLabel } from './channels';
import type { Clip, Property, Timeline } from './types';

const clipById = (timeline: Timeline, id: string | null | undefined): Clip | undefined =>
	id ? timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === id) : undefined;

/** The numbers of a clip that are keyed — a transform number, a colour number or the volume. */
export function animatedProps(clip: Clip): Property[] {
	return PROPERTIES.filter((p) => isKeyed(clip, p));
}

export interface KeyframeOffer {
	copy: { disabled: boolean; reason?: string };
	paste: { disabled: boolean; reason?: string };
}

/**
 * What Copy keyframes and Paste keyframes offer for `clipId`, given the clip a copy was taken from (`source`).
 * Each is off with the reason it is — an entry that cannot be used and does not say why is a dead end.
 */
export function keyframeOffer(timeline: Timeline, source: string | null, clipId: string | null): KeyframeOffer {
	const here = clipById(timeline, clipId);
	const from = clipById(timeline, source);
	const copy = !here
		? { disabled: true, reason: 'Select a clip first' }
		: animatedProps(here).length === 0
			? { disabled: true, reason: 'This clip has no keyframes to copy' }
			: { disabled: false };
	const paste = !here
		? { disabled: true, reason: 'Select a clip first' }
		: !source
			? { disabled: true, reason: 'Copy keyframes from an animated clip first' }
			: !from
				? { disabled: true, reason: 'The clip they were copied from is gone — copy again' }
				: from.id === here.id
					? { disabled: true, reason: 'Select another clip to paste them onto' }
					: animatedProps(from).length === 0
						? { disabled: true, reason: 'The clip they were copied from has no keyframes any more' }
						: { disabled: false };
	return { copy, paste };
}

/** `scale, position x and opacity`, for the toast. */
export function propsPhrase(props: readonly Property[]): string {
	const names = props.map(propertyLabel);
	if (names.length < 2) return names.join('');
	return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
}
