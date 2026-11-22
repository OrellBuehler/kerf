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
 *  an 8-digit hex — or `fallbackAlpha` when the field is empty (a title's box has no colour until one is chosen, and
 *  a solid box hides the picture). A colour the field already holds stays as solid as it was. */
export function withPicked(current: string | null | undefined, picked: string, fallbackAlpha: number | null = null): string {
	const color = picked.toLowerCase();
	const byte = hexAlphaByte(current);
	if (byte !== null) return `${color}${byte}`;
	const alpha = alphaOf(current) ?? (isBlank(current) ? fallbackAlpha : null);
	return alpha === null ? color : `${color}@${alpha}`;
}

const isBlank = (text: string | null | undefined) => (text ?? '').trim() === '';

/** What a colour field shows: the colour as `#rrggbb` when the swatch can draw it (a name is spelled out), else the text as
 *  it is. The opacity is the field beside it, so neither `@0.55` nor an alpha byte is in the text. */
export function colorText(text: string | null | undefined): string {
	return parseColor(text)?.hex ?? (text ?? '').trim();
}

/** The opacity a spelling has, as a percentage: its `@alpha`, or an 8-digit hex's alpha byte; 100 for a solid colour;
 *  null for no colour at all. */
export function opacityPercent(text: string | null | undefined): number | null {
	if (isBlank(text)) return null;
	const byte = hexAlphaByte(text);
	const alpha = byte !== null ? parseInt(byte, 16) / 255 : alphaOf(text);
	return alpha === null ? 100 : Math.round(Math.min(Math.max(alpha, 0), 1) * 100);
}

/** What a typed opacity means: `55`, `55%` or ` 55 % ` is 55; null for anything that is not a number (the field then goes
 *  back to what the title has). Held to 0..100. */
export function parseOpacity(text: string): number | null {
	const t = text.trim().replace(/\s*%$/, '').trim();
	if (!/^\d+(\.\d+)?$/.test(t)) return null;
	return Math.min(Math.max(Math.round(Number(t)), 0), 100);
}

/** The spelling with its opacity set to `percent`: an 8-digit hex gets its alpha byte, anything else an `@alpha` (a
 *  name stays a name), and a full opacity takes the alpha off. An empty field has no colour to give an opacity. */
export function withOpacity(current: string | null | undefined, percent: number): string {
	const text = (current ?? '').trim();
	if (text === '') return '';
	const pct = Math.min(Math.max(Math.round(percent), 0), 100);
	const at = text.indexOf('@');
	const base = at < 0 ? text : text.slice(0, at);
	const eight = /^(#|0x)([0-9a-f]{6})[0-9a-f]{2}$/i.exec(base);
	if (eight) return pct >= 100 ? `${eight[1]}${eight[2]}` : `${eight[1]}${eight[2]}${Math.round((pct / 100) * 255).toString(16).padStart(2, '0')}`;
	return pct >= 100 ? base : `${base}@${pct / 100}`;
}

/** Whether two spellings draw the same colour at the same opacity (`white` and `#ffffff` do): what tells a pick that
 *  changes nothing from one that does, so it writes no edit. */
export function sameColor(a: string | null | undefined, b: string | null | undefined): boolean {
	const x = parseColor(a);
	const y = parseColor(b);
	if (x && y) return x.hex === y.hex && opacityPercent(a) === opacityPercent(b);
	return (a ?? '').trim() === (b ?? '').trim();
}

/** What typing `typed` into the colour field writes. A hex or a name keeps the opacity the field had (the field beside
 *  it is where that changes), and into an empty field starts at `fallbackAlpha`; a spelling that carries its own alpha, one
 *  the swatch cannot draw, or nothing at all is taken as typed — the engine says whether it can draw it. */
export function typedColor(current: string | null | undefined, typed: string, fallbackAlpha: number | null = null): string {
	const t = typed.trim();
	if (t === '' || t.includes('@') || hexAlphaByte(t) !== null) return t;
	const parsed = parseColor(t);
	return parsed ? withPicked(current, parsed.hex, fallbackAlpha) : t;
}

/** What the picker shows for a field's text: its colour, else black (the picker cannot be empty). */
export function swatchHex(text: string | null | undefined): string {
	return parseColor(text)?.hex ?? '#000000';
}

/** What a thumbnail headline is drawn in when nothing is chosen: the engine's preset defaults
 *  (`SvgPresetParams`), white with an amber subtitle. */
export const HEADLINE_COLOR = '#ffffff';
export const HEADLINE_ACCENT = '#ffb000';
