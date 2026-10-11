// A title's colour is the engine's own spelling (`valid_color` in kerf-core's cli engine): a name
// (`white`), `#rrggbb` / `#rrggbbaa` / `0x…`, each optionally `@alpha` (`black@0.55`). The Inspector's
// field stays free text, since that is what the agent writes and what a name or an alpha needs, and
// gains a picker beside it: this reads the field for the picker's swatch and writes a pick back
// without losing the alpha the field had.

export interface ParsedColor {
	/** `#rrggbb`, lower case — what an `<input type="color">` takes. */
	hex: string;
	/** The `@alpha` of the spelling, or null when it has none. */
	alpha: number | null;
}

/** The names a title is likely to carry (FFmpeg's are the CSS ones: `green` is `#008000`). Anything else
 *  has no swatch, and the picker starts from black. */
const NAMES: Record<string, string> = {
	white: '#ffffff',
	black: '#000000',
	red: '#ff0000',
	green: '#008000',
	lime: '#00ff00',
	blue: '#0000ff',
	yellow: '#ffff00',
	cyan: '#00ffff',
	aqua: '#00ffff',
	magenta: '#ff00ff',
	fuchsia: '#ff00ff',
	orange: '#ffa500',
	gray: '#808080',
	grey: '#808080',
	silver: '#c0c0c0',
	purple: '#800080',
	pink: '#ffc0cb',
	gold: '#ffd700',
	navy: '#000080',
	teal: '#008080',
	maroon: '#800000',
	olive: '#808000'
};

/** The `@alpha` a spelling carries, or null: none, or one that is not a number. */
export function alphaOf(text: string | null | undefined): number | null {
	const t = (text ?? '').trim();
	const at = t.indexOf('@');
	if (at < 0) return null;
	const alphaText = t.slice(at + 1);
	const alpha = Number(alphaText);
	return alphaText === '' || !Number.isFinite(alpha) ? null : alpha;
}

/** What a field's text spells, or null for a spelling the swatch cannot show (an unlisted name, a typo). */
export function parseColor(text: string | null | undefined): ParsedColor | null {
	const t = (text ?? '').trim();
	if (!t) return null;
	const at = t.indexOf('@');
	const base = at < 0 ? t : t.slice(0, at);
	if (at >= 0 && alphaOf(t) === null) return null;
	const alpha = alphaOf(t);
	const hex = /^(?:#|0x)([0-9a-f]{6})(?:[0-9a-f]{2})?$/i.exec(base)?.[1];
	if (hex) return { hex: `#${hex.toLowerCase()}`, alpha };
	const named = NAMES[base.toLowerCase()];
	return named ? { hex: named, alpha } : null;
}

/** The alpha byte of an 8-digit hex spelling (`#rrggbbaa`, `0xRRGGBBAA`) as two hex digits, or null: that
 *  spelling carries its alpha in the colour itself, not after an `@`. */
export function hexAlphaByte(text: string | null | undefined): string | null {
	const m = /^(?:#|0x)[0-9a-f]{6}([0-9a-f]{2})$/i.exec((text ?? '').trim());
	return m ? m[1].toLowerCase() : null;
}

/** The text after a pick: the picked `#rrggbb`, with the field's alpha kept — an `@alpha`, or the last byte of
 *  an 8-digit hex — or `fallbackAlpha` when the field had none (a title's box has no alpha until one is chosen, and
 *  a solid box hides the picture). */
export function withPicked(current: string | null | undefined, picked: string, fallbackAlpha: number | null = null): string {
	const color = picked.toLowerCase();
	const byte = hexAlphaByte(current);
	if (byte !== null) return `${color}${byte}`;
	const alpha = alphaOf(current) ?? (current?.includes('@') ? null : fallbackAlpha);
	return alpha === null ? color : `${color}@${alpha}`;
}

/** What the picker shows for a field's text: its colour, else black (the picker cannot be empty). */
export function swatchHex(text: string | null | undefined): string {
	return parseColor(text)?.hex ?? '#000000';
}
