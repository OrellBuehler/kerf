/* The transitions kerf-core can render, grouped the way a picker should offer
 * them.
 *
 * Mirrors `TransitionKind` in crates/kerf-core/src/model.rs; the ids are the
 * serde wire names, and the grouping is the one distinction a user has to make
 * before picking a direction — whether the shot they are leaving stays put or
 * is carried out of frame with the one arriving.
 *
 * A direction names the direction of *travel*, the way an editor says it:
 * `slide_left` brings the new shot in from the right edge and moves it left. */

import type { TransitionKind } from './types';

export interface TransitionOption {
	id: TransitionKind;
	label: string;
}

export interface TransitionGroup {
	label: string;
	hint: string;
	options: TransitionOption[];
}

export const TRANSITION_GROUPS: TransitionGroup[] = [
	{
		label: 'Fade',
		hint: 'Between scenes',
		options: [
			{ id: 'crossfade', label: 'Crossfade' },
			{ id: 'dip_to_black', label: 'Dip to black' },
			{ id: 'dip_to_white', label: 'Dip to white' }
		]
	},
	{
		label: 'Slide',
		hint: 'The new shot travels in over the old one',
		options: [
			{ id: 'slide_left', label: 'Slide left' },
			{ id: 'slide_right', label: 'Slide right' },
			{ id: 'slide_up', label: 'Slide up' },
			{ id: 'slide_down', label: 'Slide down' }
		]
	},
	{
		label: 'Push',
		hint: 'The new shot carries the old one out of frame',
		options: [
			{ id: 'push_left', label: 'Push left' },
			{ id: 'push_right', label: 'Push right' },
			{ id: 'push_up', label: 'Push up' },
			{ id: 'push_down', label: 'Push down' }
		]
	}
];

/** Every kind, flattened, in picker order. */
export const TRANSITION_OPTIONS: TransitionOption[] = TRANSITION_GROUPS.flatMap((g) => g.options);

/** Seconds a transition gets when one is first applied. Long enough to read as
 *  deliberate, short enough not to eat a short shot whole. */
export const DEFAULT_TRANSITION_SECONDS = 0.5;

/** The human label for a kind, falling back to the wire name so a kind added to
 *  the engine ahead of this list still shows as something rather than blank. */
export function transitionLabel(kind: TransitionKind): string {
	return TRANSITION_OPTIONS.find((o) => o.id === kind)?.label ?? kind;
}

/** True when the transition needs the outgoing clip's unused source to play
 *  underneath — which is every one but a dip, and the reason a clip trimmed to
 *  the very end of its footage falls back to a hard cut. */
export function needsSourceHandle(kind: TransitionKind): boolean {
	return kind !== 'dip_to_black' && kind !== 'dip_to_white';
}
