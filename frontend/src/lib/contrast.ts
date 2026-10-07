// WCAG 2.x contrast, for checking that a theme can be read. Pure arithmetic on
// the opaque `#rrggbb` values a theme is made of (`theme.ts`); the theme guard
// tests hold the presets to it.

/** One 8-bit sRGB channel as linear light. */
function linear(channel: number): number {
	const c = channel / 255;
	return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

/** Relative luminance of a `#rrggbb` color: 0 for black, 1 for white. */
export function relativeLuminance(hex: string): number {
	const m = /^#([0-9a-f]{6})$/i.exec(hex);
	if (!m) throw new Error(`not a #rrggbb color: ${hex}`);
	const n = parseInt(m[1], 16);
	return 0.2126 * linear((n >> 16) & 255) + 0.7152 * linear((n >> 8) & 255) + 0.0722 * linear(n & 255);
}

/** The contrast ratio between two `#rrggbb` colors, 1 (identical) to 21 (black
 *  on white), whichever way round they are given. */
export function contrastRatio(a: string, b: string): number {
	const [hi, lo] = [relativeLuminance(a), relativeLuminance(b)].sort((x, y) => y - x);
	return (hi + 0.05) / (lo + 0.05);
}

/** What CSS `color-mix(in srgb, a <weight>%, b)` gives for two opaque `#rrggbb`
 *  colors, as `#rrggbb`: each channel linearly between the two, `weightA` (0–1)
 *  of the way to `a`. Browsers keep the fractions; this rounds to 8 bits. */
export function mixSrgb(a: string, b: string, weightA: number): string {
	const parse = (hex: string) => {
		const m = /^#([0-9a-f]{6})$/i.exec(hex);
		if (!m) throw new Error(`not a #rrggbb color: ${hex}`);
		const n = parseInt(m[1], 16);
		return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
	};
	const [pa, pb] = [parse(a), parse(b)];
	const channel = (i: number) => Math.round(pa[i] * weightA + pb[i] * (1 - weightA));
	return '#' + [0, 1, 2].map((i) => channel(i).toString(16).padStart(2, '0')).join('');
}
