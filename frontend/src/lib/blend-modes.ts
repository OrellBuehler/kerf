/* The blend modes kerf-core composites a clip with, in the order a picker offers them.
 *
 * Mirrors `BlendMode` in crates/kerf-core/src/model.rs: the ids are the serde wire names, the labels
 * `BlendMode::label`. `normal` is plain compositing and is absent from a clip (the backend omits it). */

import type { BlendMode, Clip } from './types';

export interface BlendModeOption {
	id: BlendMode;
	label: string;
	/** What the mode is for, in a picker's tooltip. */
	hint: string;
}

export const BLEND_MODES: BlendModeOption[] = [
	{ id: 'normal', label: 'Normal', hint: 'Laid over the tracks below' },
	{ id: 'multiply', label: 'Multiply', hint: 'Darkens: white vanishes (textures, shadows, line art)' },
	{ id: 'screen', label: 'Screen', hint: 'Lightens: black vanishes (light leaks, flares, smoke on black)' },
	{ id: 'overlay', label: 'Overlay', hint: 'Contrast from the clip, keeping the shot underneath' },
	{ id: 'hard_light', label: 'Hard light', hint: 'Overlay led by the clip’s own tones' },
	{ id: 'add', label: 'Add', hint: 'Linear dodge: the two summed, a hotter glow' },
	{ id: 'darken', label: 'Darken', hint: 'The darker of the two, per channel' },
	{ id: 'lighten', label: 'Lighten', hint: 'The lighter of the two, per channel' },
	{ id: 'difference', label: 'Difference', hint: 'Inverts the shot where the clip is bright' },
	{ id: 'exclusion', label: 'Exclusion', hint: 'Difference with less contrast' }
];

/** The clip's mode, `normal` where it has none. */
export function blendModeOf(clip: Pick<Clip, 'blend_mode'>): BlendMode {
	return clip.blend_mode ?? 'normal';
}
