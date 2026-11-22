// A title's colour is the engine's own spelling, and this is its port: `valid_color` / `color_rgb` in kerf-core's
// `engine/colors.rs` (ffmpeg's `av_parse_color`) — a name from ffmpeg's table (any case), `#RRGGBB[AA]` or `0xRRGGBB[AA]`
// (a lower-case `0x` only), each with an optional `@alpha` that is a decimal `0..1` or a lower-case `0x` and one or two
// hex digits. The Inspector's field stays free text, since that is what the agent writes and what a name or an alpha
// needs, and gains a picker and an opacity beside it: this reads the field for them and writes a change back without
// losing what the field had. Nothing here writes a spelling `valid_color` refuses out of one it was given valid, and
// `title-color.test.ts` holds the name table and the grammar to the Rust source, so a change there shows up here.

export interface ParsedColor {
	/** `#rrggbb`, lower case — what an `<input type="color">` takes. */
	hex: string;
	/** The `@alpha` of the spelling as the engine reads it (its byte over 255), or null when it has none. */
	alpha: number | null;
}

/** ffmpeg's colour-name table (`NAMES` in `engine/colors.rs`), lower-cased, with each name's RGB. `grey` is not in it
 *  (ffmpeg has `gray` and `lightgrey`), and neither is `rebeccapurple` or `transparent`: the engine refuses them. */
export const ENGINE_COLOR_NAMES: Readonly<Record<string, readonly [number, number, number]>> = {
	aliceblue: [240, 248, 255],
	antiquewhite: [250, 235, 215],
	aqua: [0, 255, 255],
	aquamarine: [127, 255, 212],
	azure: [240, 255, 255],
	beige: [245, 245, 220],
	bisque: [255, 228, 196],
	black: [0, 0, 0],
	blanchedalmond: [255, 235, 205],
	blue: [0, 0, 255],
	blueviolet: [138, 43, 226],
	brown: [165, 42, 42],
	burlywood: [222, 184, 135],
	cadetblue: [95, 158, 160],
	chartreuse: [127, 255, 0],
	chocolate: [210, 105, 30],
	coral: [255, 127, 80],
	cornflowerblue: [100, 149, 237],
	cornsilk: [255, 248, 220],
	crimson: [220, 20, 60],
	cyan: [0, 255, 255],
	darkblue: [0, 0, 139],
	darkcyan: [0, 139, 139],
	darkgoldenrod: [184, 134, 11],
	darkgray: [169, 169, 169],
	darkgreen: [0, 100, 0],
	darkkhaki: [189, 183, 107],
	darkmagenta: [139, 0, 139],
	darkolivegreen: [85, 107, 47],
	darkorange: [255, 140, 0],
	darkorchid: [153, 50, 204],
	darkred: [139, 0, 0],
	darksalmon: [233, 150, 122],
	darkseagreen: [143, 188, 143],
	darkslateblue: [72, 61, 139],
	darkslategray: [47, 79, 79],
	darkturquoise: [0, 206, 209],
	darkviolet: [148, 0, 211],
	deeppink: [255, 20, 147],
	deepskyblue: [0, 191, 255],
	dimgray: [105, 105, 105],
	dodgerblue: [30, 144, 255],
	firebrick: [178, 34, 34],
	floralwhite: [255, 250, 240],
	forestgreen: [34, 139, 34],
	fuchsia: [255, 0, 255],
	gainsboro: [220, 220, 220],
	ghostwhite: [248, 248, 255],
	gold: [255, 215, 0],
	goldenrod: [218, 165, 32],
	gray: [128, 128, 128],
	green: [0, 128, 0],
	greenyellow: [173, 255, 47],
	honeydew: [240, 255, 240],
	hotpink: [255, 105, 180],
	indianred: [205, 92, 92],
	indigo: [75, 0, 130],
	ivory: [255, 255, 240],
	khaki: [240, 230, 140],
	lavender: [230, 230, 250],
	lavenderblush: [255, 240, 245],
	lawngreen: [124, 252, 0],
	lemonchiffon: [255, 250, 205],
	lightblue: [173, 216, 230],
	lightcoral: [240, 128, 128],
	lightcyan: [224, 255, 255],
	lightgoldenrodyellow: [250, 250, 210],
	lightgreen: [144, 238, 144],
	lightgrey: [211, 211, 211],
	lightpink: [255, 182, 193],
	lightsalmon: [255, 160, 122],
	lightseagreen: [32, 178, 170],
	lightskyblue: [135, 206, 250],
	lightslategray: [119, 136, 153],
	lightsteelblue: [176, 196, 222],
	lightyellow: [255, 255, 224],
	lime: [0, 255, 0],
	limegreen: [50, 205, 50],
	linen: [250, 240, 230],
	magenta: [255, 0, 255],
	maroon: [128, 0, 0],
	mediumaquamarine: [102, 205, 170],
	mediumblue: [0, 0, 205],
	mediumorchid: [186, 85, 211],
	mediumpurple: [147, 112, 216],
	mediumseagreen: [60, 179, 113],
	mediumslateblue: [123, 104, 238],
	mediumspringgreen: [0, 250, 154],
	mediumturquoise: [72, 209, 204],
	mediumvioletred: [199, 21, 133],
	midnightblue: [25, 25, 112],
	mintcream: [245, 255, 250],
	mistyrose: [255, 228, 225],
	moccasin: [255, 228, 181],
	navajowhite: [255, 222, 173],
	navy: [0, 0, 128],
	oldlace: [253, 245, 230],
	olive: [128, 128, 0],
	olivedrab: [107, 142, 35],
	orange: [255, 165, 0],
	orangered: [255, 69, 0],
	orchid: [218, 112, 214],
	palegoldenrod: [238, 232, 170],
	palegreen: [152, 251, 152],
	paleturquoise: [175, 238, 238],
	palevioletred: [216, 112, 147],
	papayawhip: [255, 239, 213],
	peachpuff: [255, 218, 185],
	peru: [205, 133, 63],
	pink: [255, 192, 203],
	plum: [221, 160, 221],
	powderblue: [176, 224, 230],
	purple: [128, 0, 128],
	red: [255, 0, 0],
	rosybrown: [188, 143, 143],
	royalblue: [65, 105, 225],
	saddlebrown: [139, 69, 19],
	salmon: [250, 128, 114],
	sandybrown: [244, 164, 96],
	seagreen: [46, 139, 87],
	seashell: [255, 245, 238],
	sienna: [160, 82, 45],
	silver: [192, 192, 192],
	skyblue: [135, 206, 235],
	slateblue: [106, 90, 205],
	slategray: [112, 128, 144],
	snow: [255, 250, 250],
	springgreen: [0, 255, 127],
	steelblue: [70, 130, 180],
	tan: [210, 180, 140],
	teal: [0, 128, 128],
	thistle: [216, 191, 216],
	tomato: [255, 99, 71],
	turquoise: [64, 224, 208],
	violet: [238, 130, 238],
	wheat: [245, 222, 179],
	white: [255, 255, 255],
	whitesmoke: [245, 245, 245],
	yellow: [255, 255, 0],
	yellowgreen: [154, 205, 50],
};

const HEX = /^(#|0x)([0-9a-fA-F]{6})([0-9a-fA-F]{2})?$/;

const nameRgb = (name: string): readonly [number, number, number] | undefined =>
	/^[A-Za-z]+$/.test(name) ? ENGINE_COLOR_NAMES[name.toLowerCase()] : undefined;

/** The text and the `@alpha` of a spelling, split at the first `@` as the engine does. */
function split(text: string): [base: string, alpha: string | null] {
	const at = text.indexOf('@');
	return at < 0 ? [text, null] : [text.slice(0, at), text.slice(at + 1)];
}

/** The alpha byte `av_parse_color` makes of the text after an `@`, or null for one `valid_color` refuses: a decimal in
 *  `0..=1` (digits and at most one point, no sign or exponent; the byte is 255 times it, cut), or a lower-case `0x` and
 *  one or two hex digits (the byte itself). */
function alphaByte(alpha: string): number | null {
	if (alpha.startsWith('0x')) {
		const digits = alpha.slice(2);
		return /^[0-9a-fA-F]{1,2}$/.test(digits) ? parseInt(digits, 16) : null;
	}
	if (!/^[0-9.]+$/.test(alpha) || !/[0-9]/.test(alpha) || alpha.split('.').length > 2) return null;
	const x = parseFloat(alpha);
	return x >= 0 && x <= 1 ? Math.trunc(255 * x) : null;
}

/** Whether `text` is a colour the engine accepts (`valid_color`): not trimmed, not lower-cased, no second chance. */
export function validColor(text: string): boolean {
	const [base, alpha] = split(text);
	if (alpha !== null && alphaByte(alpha) === null) return false;
	return HEX.test(base) || nameRgb(base) !== undefined;
}

const toHex = ([r, g, b]: readonly number[]): string => `#${[r, g, b].map((v) => v.toString(16).padStart(2, '0')).join('')}`;

/** The `@alpha` a spelling carries, as the engine reads it (a fraction), or null: none, or one the engine refuses. */
export function alphaOf(text: string | null | undefined): number | null {
	const alpha = split((text ?? '').trim())[1];
	const byte = alpha === null ? null : alphaByte(alpha);
	return byte === null ? null : byte / 255;
}

/** What a field's text spells, or null for a spelling the engine refuses (`grey`, `#fff`, `white@2`). */
export function parseColor(text: string | null | undefined): ParsedColor | null {
	const t = (text ?? '').trim();
	if (!validColor(t)) return null;
	const [base] = split(t);
	const hex = HEX.exec(base);
	return { hex: hex ? `#${hex[2].toLowerCase()}` : toHex(nameRgb(base)!), alpha: alphaOf(t) };
}

/** The alpha byte of an 8-digit hex spelling (`#rrggbbaa`, `0xRRGGBBaa`) as two hex digits, or null: that
 *  spelling carries its alpha in the colour itself, not after an `@`. */
export function hexAlphaByte(text: string | null | undefined): string | null {
	const m = HEX.exec((text ?? '').trim());
	return m?.[3] ? m[3].toLowerCase() : null;
}

/** The text after the `@`, as written, when the engine reads it: what a colour change carries over unchanged. */
function alphaText(text: string | null | undefined): string | null {
	const alpha = split((text ?? '').trim())[1];
	return alpha !== null && alphaByte(alpha) !== null ? alpha : null;
}

const isBlank = (text: string | null | undefined) => (text ?? '').trim() === '';

/** The text after a pick: the picked `#rrggbb`, with the field's alpha kept — an `@alpha` (which the engine lets win
 *  over the last byte of an 8-digit hex) or that byte — or `fallbackAlpha` when the field is empty (a title's box has
 *  no colour until one is chosen, and a solid box hides the picture). A colour the field already holds stays as solid as
 *  it was. */
export function withPicked(current: string | null | undefined, picked: string, fallbackAlpha: number | null = null): string {
	const color = picked.toLowerCase();
	const kept = alphaText(current);
	if (kept !== null) return `${color}@${kept}`;
	const byte = hexAlphaByte(current);
	if (byte !== null) return `${color}${byte}`;
	return isBlank(current) && fallbackAlpha !== null ? `${color}@${fallbackAlpha}` : color;
}

/** What a colour field shows: the colour as `#rrggbb` when the swatch can draw it (a name is spelled out), else the text as
 *  it is. The opacity is the field beside it, so neither `@0.55` nor an alpha byte is in the text. */
export function colorText(text: string | null | undefined): string {
	return parseColor(text)?.hex ?? (text ?? '').trim();
}

/** The alpha byte the engine draws a colour with: its `@alpha`, else the last byte of an 8-digit hex, else 255; null for
 *  no colour or one the engine refuses. */
function opacityByte(text: string | null | undefined): number | null {
	const t = (text ?? '').trim();
	if (!validColor(t)) return null;
	const alpha = split(t)[1];
	if (alpha !== null) return alphaByte(alpha);
	const byte = hexAlphaByte(t);
	return byte === null ? 255 : parseInt(byte, 16);
}

/** The opacity a spelling has, as a percentage; null for no colour, or one the engine refuses (there is nothing to give an
 *  opacity to until it is fixed). */
export function opacityPercent(text: string | null | undefined): number | null {
	const byte = opacityByte(text);
	return byte === null ? null : Math.round((byte / 255) * 100);
}

/** What a typed opacity means: `55`, `55%` or ` 55 % ` is 55; null for anything that is not a number (the field then goes
 *  back to what the title has). Held to 0..100. */
export function parseOpacity(text: string): number | null {
	const t = text.trim().replace(/\s*%$/, '').trim();
	if (!/^\d+(\.\d+)?$/.test(t)) return null;
	return Math.min(Math.max(Math.round(Number(t)), 0), 100);
}

/** The spelling with its opacity set to `percent`: an 8-digit hex gets its alpha byte, anything else a decimal `@alpha` (a
 *  name stays a name), and a full opacity takes the alpha off. Always a spelling the engine accepts when it was given one;
 *  one it refuses (or none) comes back as it was, since an alpha on it would not make it a colour. */
export function withOpacity(current: string | null | undefined, percent: number): string {
	const text = (current ?? '').trim();
	if (!validColor(text)) return text;
	const pct = Math.min(Math.max(Math.round(percent), 0), 100);
	const [base, alpha] = split(text);
	const hex = HEX.exec(base);
	if (hex && hex[3] !== undefined && alpha === null) {
		return pct >= 100 ? `${hex[1]}${hex[2]}` : `${hex[1]}${hex[2]}${Math.round((pct / 100) * 255).toString(16).padStart(2, '0')}`;
	}
	// Two alphas would fight (the `@` one wins): the opacity lives in one place.
	const solid = hex ? `${hex[1]}${hex[2]}` : base;
	return pct >= 100 ? solid : `${solid}@${pct / 100}`;
}

/** Whether two spellings draw the same colour at the same opacity (`white` and `#ffffff` do): what tells a pick that
 *  changes nothing from one that does, so it writes no edit. */
export function sameColor(a: string | null | undefined, b: string | null | undefined): boolean {
	const x = parseColor(a);
	const y = parseColor(b);
	if (x && y) return x.hex === y.hex && opacityByte(a) === opacityByte(b);
	return (a ?? '').trim() === (b ?? '').trim();
}

/** What typing `typed` into the colour field writes. A hex or a name keeps the opacity the field had (the field beside
 *  it is where that changes), and into an empty field starts at `fallbackAlpha`; a spelling that carries its own alpha,
 *  one the engine refuses, or nothing at all is taken as typed — the engine says why it cannot draw a bad one. */
export function typedColor(current: string | null | undefined, typed: string, fallbackAlpha: number | null = null): string {
	const t = typed.trim();
	if (t === '' || t.includes('@') || hexAlphaByte(t) !== null) return t;
	const parsed = parseColor(t);
	return parsed ? withPicked(current, parsed.hex, fallbackAlpha) : t;
}

/** What a title's colour fields hold: its text colour, and its box (absent for none). */
export interface TitleColors {
	color: string;
	bg?: string | null;
}

/** The opacity a box has when its colour is first chosen: a solid box hides the picture. */
export const BOX_ALPHA = 0.55;

/** One change to one of a title's colour fields: text typed into the hex field, a typed opacity, a pick from the
 *  swatch, or the box taken away. */
export type ColorChange = { kind: 'text' | 'opacity' | 'pick'; text: string } | { kind: 'clear' };

/** What a change writes to a title, or null for nothing (not a number, or what the title already has). It is the
 *  *title the edit began on* that is handed in, never the one that is selected when the field commits: the click that
 *  blurs a field is often the click that selects another title, and a colour built from that one is written onto this. */
export function colorEdit(title: TitleColors, field: 'color' | 'bg', change: ColorChange): { color: string } | { bg: string } | null {
	const current = (field === 'color' ? title.color : title.bg) ?? '';
	const fallback = field === 'bg' ? BOX_ALPHA : null;
	let next: string | null;
	switch (change.kind) {
		case 'pick':
			next = withPicked(current, change.text, fallback);
			break;
		case 'text':
			next = typedColor(current, change.text, fallback);
			break;
		case 'opacity': {
			const pct = parseOpacity(change.text);
			next = pct === null ? null : withOpacity(current, pct);
			break;
		}
		case 'clear':
			next = '';
			break;
	}
	if (next === null || sameColor(next, current)) return null;
	return field === 'color' ? { color: next } : { bg: next };
}

/** What the picker shows for a field's text: its colour, else black (the picker cannot be empty). */
export function swatchHex(text: string | null | undefined): string {
	return parseColor(text)?.hex ?? '#000000';
}

/** What a thumbnail headline is drawn in when nothing is chosen: the engine's preset defaults
 *  (`SvgPresetParams`), white with an amber subtitle. */
export const HEADLINE_COLOR = '#ffffff';
export const HEADLINE_ACCENT = '#ffb000';
