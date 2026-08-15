// One-click polish presets: color looks and text styles. Pure data over the
// existing Color / TextOverlay surfaces — the product goal is a professional
// result without manual grading or typography, so these are deliberately few
// and tasteful rather than deep.

import type { Color, TextOverlay } from './types';

export interface ColorLook {
	id: string;
	label: string;
	color: Color;
}

/** Tasteful one-click grades. Values stay subtle — a look should read as
 *  "graded", not "filtered" — and every look is a plain `Color`, so the
 *  sliders show (and can fine-tune) exactly what a chip applied. */
export const COLOR_LOOKS: ColorLook[] = [
	{ id: 'punchy', label: 'Punchy', color: { brightness: 0.02, contrast: 1.15, saturation: 1.22, gamma: 1, temperature: 0 } },
	{ id: 'warm', label: 'Warm', color: { brightness: 0.02, contrast: 1.05, saturation: 1.1, gamma: 1, temperature: 0.4 } },
	{ id: 'cool', label: 'Cool', color: { brightness: 0, contrast: 1.08, saturation: 1.05, gamma: 1, temperature: -0.35 } },
	{ id: 'faded', label: 'Faded', color: { brightness: 0.06, contrast: 0.88, saturation: 0.78, gamma: 1.08, temperature: 0.15 } },
	{ id: 'mono', label: 'B&W', color: { brightness: 0, contrast: 1.15, saturation: 0, gamma: 1, temperature: 0 } }
];

/** The look a clip's current color matches, if any (so its chip can light up). */
export function activeLook(color: Color): ColorLook | undefined {
	const close = (a: number, b: number) => Math.abs(a - b) < 1e-6;
	return COLOR_LOOKS.find(
		(l) =>
			close(l.color.brightness, color.brightness) &&
			close(l.color.contrast, color.contrast) &&
			close(l.color.saturation, color.saturation) &&
			close(l.color.gamma, color.gamma) &&
			close(l.color.temperature, color.temperature ?? 0)
	);
}

export interface TextStyle {
	id: string;
	label: string;
	/** Placeholder text the overlay is created with. */
	text: string;
	/** Default on-screen duration in seconds. */
	duration: number;
	style: Pick<TextOverlay, 'pos_x' | 'pos_y' | 'size' | 'color' | 'bold'> & { bg: string | null };
	/** Opacity fade in/out in seconds via keyframes; 0 = none. */
	fade: number;
}

/** Ready-made title / lower-third / caption styles. The caption style matches
 *  what `captions_from_transcript` generates, so manual and generated captions
 *  look the same. */
export const TEXT_STYLES: TextStyle[] = [
	{
		id: 'title',
		label: 'Title',
		text: 'Title',
		duration: 3.5,
		style: { pos_x: 0.5, pos_y: 0.4, size: 0.085, color: 'white', bold: true, bg: null },
		fade: 0.5
	},
	{
		id: 'lower_third',
		label: 'Lower third',
		text: 'Name — Role',
		duration: 4,
		style: { pos_x: 0.24, pos_y: 0.86, size: 0.042, color: 'white', bold: true, bg: 'black@0.55' },
		fade: 0.35
	},
	{
		id: 'caption',
		label: 'Caption',
		text: 'Caption',
		duration: 3,
		style: { pos_x: 0.5, pos_y: 0.88, size: 0.05, color: 'white', bold: false, bg: 'black@0.5' },
		fade: 0
	}
];
