/* The delivery frames a project can be cut for.
 *
 * The frame is a property of the *project*, not of an export: it decides the
 * shape of every rendered picture, so a 9:16 crop is visible while cutting
 * rather than discovered in the rendered file. `null` follows the footage.
 *
 * Sizes match the corresponding export presets so picking "Reels / Shorts"
 * here and exporting with that preset render the identical frame. */

import type { Delivery, Fit } from './types';

export interface DeliveryPreset {
	id: string;
	label: string;
	hint: string;
	format: Delivery | null;
}

export const DELIVERY_PRESETS: DeliveryPreset[] = [
	{ id: 'source', label: 'Source', hint: 'Follow the footage', format: null },
	{ id: 'landscape', label: '16:9', hint: '1920×1080 — YouTube, web', format: { width: 1920, height: 1080, fit: 'contain' } },
	{ id: 'vertical', label: '9:16', hint: '1080×1920 — Reels, Shorts, TikTok', format: { width: 1080, height: 1920, fit: 'cover' } },
	{ id: 'square', label: '1:1', hint: '1080×1080 — feed post', format: { width: 1080, height: 1080, fit: 'cover' } },
	{ id: 'portrait', label: '4:5', hint: '1080×1350 — Instagram portrait', format: { width: 1080, height: 1350, fit: 'cover' } }
];

/** The preset matching a timeline's format, falling back to a custom entry. */
export function presetFor(format: Delivery | null | undefined): DeliveryPreset {
	if (!format) return DELIVERY_PRESETS[0];
	const hit = DELIVERY_PRESETS.find(
		(p) => p.format && p.format.width === format.width && p.format.height === format.height
	);
	if (hit) return hit;
	return {
		id: 'custom',
		label: ratioLabel(format.width, format.height),
		hint: `${format.width}×${format.height}`,
		format
	};
}

/** A reduced "w:h" label, e.g. 1080×1920 -> "9:16". */
export function ratioLabel(width: number, height: number): string {
	const gcd = (a: number, b: number): number => (b ? gcd(b, a % b) : a);
	const d = gcd(width, height) || 1;
	return `${width / d}:${height / d}`;
}

export function fitLabel(fit: Fit): string {
	return fit === 'cover' ? 'fill & crop' : 'fit & letterbox';
}
