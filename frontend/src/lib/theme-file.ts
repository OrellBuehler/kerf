// A theme as a file: what an import accepts and says when it does not, and what an export is called. The file
// is the `Theme` JSON itself (`version: 1`), so one a 0.31 build wrote imports here and one written here
// imports there (an older build ignores the fields it has not heard of). Reading is `parseTheme`, which takes
// each field on its own terms; this adds the size limit, the JSON parse and a sentence for a refusal.

import { COLOR_TOKENS, cleanName, normalizeHex, parseTheme, type Theme } from './theme';

/** The longest text read as a theme, in characters: a theme is a few kilobytes, and a file the picker
 *  could be pointed at is not to be parsed because it was chosen. The desktop side stops at 1 MiB first. */
export const THEME_FILE_MAX = 256 * 1024;

export type ThemeFile = { ok: true; theme: Theme } | { ok: false; reason: string };

/** Why `parseTheme` would refuse `raw`, in a sentence a person can act on; `null` when it would not. */
export function themeProblem(raw: unknown): string | null {
	if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) return 'It should be a JSON object with a name, a scheme and colors.';
	const r = raw as Record<string, unknown>;
	if (r.version === undefined) return 'It has no "version": a Kerf theme file says "version": 1.';
	if (r.version !== 1) return `It is version ${show(r.version)}; this Kerf reads version 1.`;
	if (r.scheme !== 'dark' && r.scheme !== 'light') return '"scheme" has to be "dark" or "light".';
	if (cleanName(r.name) === '') return 'It has no name.';
	if (typeof r.colors !== 'object' || r.colors === null || Array.isArray(r.colors)) return '"colors" has to be an object of #rrggbb values.';
	const colors = r.colors as Record<string, unknown>;
	for (const t of COLOR_TOKENS) if (colors[t] !== undefined && normalizeHex(colors[t]) === null) return `"${t}" is ${show(colors[t])}; a color is written #rrggbb.`;
	return null;
}

/** A value as it is quoted in a sentence: short, and with no markup to run. */
function show(v: unknown): string {
	const text = typeof v === 'string' ? `"${v}"` : JSON.stringify(v) ?? String(v);
	return Array.from(text.replace(/\s+/g, ' ')).slice(0, 24).join('') + (text.length > 24 ? '…' : '');
}

/** Read the text of a theme file. */
export function readThemeFile(text: string): ThemeFile {
	if (text.length > THEME_FILE_MAX) return { ok: false, reason: 'The file is too large to be a theme.' };
	let raw: unknown;
	try {
		raw = JSON.parse(text);
	} catch {
		return { ok: false, reason: "The file isn't JSON." };
	}
	const theme = parseTheme(raw);
	return theme ? { ok: true, theme } : { ok: false, reason: themeProblem(raw) ?? "The file isn't a Kerf theme." };
}

/** The file name an export offers: the theme's name, lower case, dashes for whatever is not a letter or digit. */
export function themeFileName(name: string): string {
	const slug = cleanName(name)
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-|-$/g, '')
		.slice(0, 40)
		.replace(/-$/, '');
	return `${slug || 'theme'}.kerf-theme.json`;
}
