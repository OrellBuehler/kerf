// The typefaces a theme may choose, all of them already on the machine: the variable fonts bundled
// with the app (`@fontsource-variable/*`, imported in `routes/layout.css`, so nothing is fetched and the
// CSP needs no font host) and the platform's own faces. A theme names one by id; the stack is what
// `--font-sans` / `--font-mono` become. The licence notice of every bundled face is in
// `crates/kerf-app/licenses/fonts-NOTICE.md`, which ships in all three resource lists.

export interface FontChoice {
	id: string;
	label: string;
	/** The CSS `font-family` list. */
	stack: string;
	/** A `document.fonts.load` string for the face, or null for one the platform provides. */
	face: string | null;
	/** One line on what it is. */
	hint: string;
}

const SANS_FALLBACK = "ui-sans-serif, system-ui, -apple-system, 'Segoe UI', sans-serif";
const MONO_FALLBACK = "ui-monospace, 'SF Mono', 'Cascadia Code', monospace";

export const UI_FONTS: readonly FontChoice[] = [
	{ id: 'geist', label: 'Geist', stack: `'Geist Variable', ${SANS_FALLBACK}`, face: '500 12px "Geist Variable"', hint: "Kerf's own: even, a little technical." },
	{ id: 'inter', label: 'Inter', stack: `'Inter Variable', ${SANS_FALLBACK}`, face: '500 12px "Inter Variable"', hint: 'Roomy and plain; the most familiar.' },
	{
		id: 'space-grotesk',
		label: 'Space Grotesk',
		stack: `'Space Grotesk Variable', ${SANS_FALLBACK}`,
		face: '500 12px "Space Grotesk Variable"',
		hint: 'Geometric, with character.'
	},
	{ id: 'system', label: 'System UI', stack: SANS_FALLBACK, face: null, hint: "Whatever this computer's own interface uses." }
];

export const MONO_FONTS: readonly FontChoice[] = [
	{ id: 'geist-mono', label: 'Geist Mono', stack: `'Geist Mono Variable', ${MONO_FALLBACK}`, face: '500 12px "Geist Mono Variable"', hint: "Kerf's own." },
	{
		id: 'jetbrains-mono',
		label: 'JetBrains Mono',
		stack: `'JetBrains Mono Variable', ${MONO_FALLBACK}`,
		face: '500 12px "JetBrains Mono Variable"',
		hint: 'Tall, with open digits; good for timecode.'
	},
	{ id: 'system-mono', label: 'System mono', stack: MONO_FALLBACK, face: null, hint: "This computer's own fixed-width face." }
];

export type UiFont = 'geist' | 'inter' | 'space-grotesk' | 'system';
export type MonoFont = 'geist-mono' | 'jetbrains-mono' | 'system-mono';

export const DEFAULT_UI_FONT: UiFont = 'geist';
export const DEFAULT_MONO_FONT: MonoFont = 'geist-mono';

export const isUiFont = (v: unknown): v is UiFont => UI_FONTS.some((f) => f.id === v);
export const isMonoFont = (v: unknown): v is MonoFont => MONO_FONTS.some((f) => f.id === v);

/** The display face of Kerf's own pairing: the agent panel's title is set in Space Grotesk. With any other
 *  interface face the title takes that face, so a theme that chose one is not left with a stray third. */
const DISPLAY_DEFAULT = "'Space Grotesk Variable', 'Geist Variable', ui-sans-serif, system-ui, sans-serif";

/** The custom properties a font choice sets. */
export function fontProps(ui: UiFont, mono: MonoFont): Record<string, string> {
	const sans = UI_FONTS.find((f) => f.id === ui) ?? UI_FONTS[0];
	const fixed = MONO_FONTS.find((f) => f.id === mono) ?? MONO_FONTS[0];
	return {
		'--font-sans': sans.stack,
		'--font-mono': fixed.stack,
		'--font-display': sans.id === DEFAULT_UI_FONT ? DISPLAY_DEFAULT : sans.stack
	};
}

/** The faces the first frame is drawn in, for `fontsReady`: the ones a theme chose, so a window is not shown
 *  while they are still loading (it would paint in the fallback and re-flow). */
export function faceFor(ui: UiFont, mono: MonoFont): string[] {
	return [UI_FONTS.find((f) => f.id === ui)?.face, MONO_FONTS.find((f) => f.id === mono)?.face].filter((f): f is string => !!f);
}
