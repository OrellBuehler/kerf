// The effects a clip can be given with one click, and the defaults each one
// starts from. Shared by the Inspector's effect chains and the library's
// Effects and Audio tabs, so a preset added here shows up in both.
//
// A preset is a plain `VideoEffect` / `AudioEffect`, so the Inspector's fields
// show (and tune) exactly what a click applied; the `key` is the effect's
// `type`, which is also what the Inspector's picker lists by.

import type { AudioEffect, VideoEffect } from './types';

export interface EffectPreset<E> {
	key: E extends { type: infer T } ? T & string : string;
	label: string;
	/** One line on what it does, for a tooltip. */
	hint: string;
	effect: E;
}

export const VIDEO_EFFECT_PRESETS: EffectPreset<VideoEffect>[] = [
	{ key: 'blur', label: 'Blur', hint: 'Soften the picture — a gaussian blur', effect: { type: 'blur', sigma: 6 } },
	{ key: 'sharpen', label: 'Sharpen', hint: 'Bring out edge detail', effect: { type: 'sharpen', amount: 1 } },
	{ key: 'grayscale', label: 'Grayscale', hint: 'Drain the color', effect: { type: 'grayscale' } },
	{ key: 'invert', label: 'Invert', hint: 'Swap every color for its opposite', effect: { type: 'invert' } },
	{ key: 'vignette', label: 'Vignette', hint: 'Darken the corners to hold the eye on the middle', effect: { type: 'vignette' } },
	{
		key: 'chroma_key',
		label: 'Chroma key',
		hint: 'Make a color transparent so the track below shows through',
		effect: { type: 'chroma_key', color: 'green', similarity: 0.15, blend: 0.1 }
	}
];

export const AUDIO_EFFECT_PRESETS: EffectPreset<AudioEffect>[] = [
	{ key: 'highpass', label: 'High-pass', hint: 'Cut rumble below 80 Hz', effect: { type: 'highpass', hz: 80 } },
	{ key: 'lowpass', label: 'Low-pass', hint: 'Cut hiss above 12 kHz', effect: { type: 'lowpass', hz: 12000 } },
	{
		key: 'equalizer',
		label: 'Equalizer',
		hint: 'Lift or dip one band — a 3 dB presence boost at 3 kHz to start',
		effect: { type: 'equalizer', hz: 3000, width: 1000, gain_db: 3 }
	},
	{
		key: 'compressor',
		label: 'Compressor',
		hint: 'Even out loud and quiet — 3:1 above −18 dB, with makeup gain',
		effect: { type: 'compressor', threshold_db: -18, ratio: 3, attack_ms: 20, release_ms: 250, makeup_db: 6 }
	},
	{ key: 'gate', label: 'Gate', hint: 'Silence what falls under −45 dB', effect: { type: 'gate', threshold_db: -45 } }
];

/** The presets by key — what the Inspector's "Add effect" picker reads. */
export const VIDEO_FX: Record<string, VideoEffect> = Object.fromEntries(
	VIDEO_EFFECT_PRESETS.map((p) => [p.key, p.effect])
);
export const AUDIO_FX: Record<string, AudioEffect> = Object.fromEntries(
	AUDIO_EFFECT_PRESETS.map((p) => [p.key, p.effect])
);

/** A fresh copy of a preset's effect, safe to put on a clip. */
export function freshEffect<E extends VideoEffect | AudioEffect>(preset: EffectPreset<E>): E {
	return structuredClone(preset.effect);
}

/** How a chain entry reads in a list: `chroma_key` as "Chroma key". */
export function effectLabel(effect: { type: string }): string {
	const words = effect.type.replaceAll('_', ' ');
	return words.charAt(0).toUpperCase() + words.slice(1);
}
