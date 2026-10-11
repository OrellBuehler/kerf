import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { contrastRatio, mixSrgb } from './contrast';
import { posix, SRC, uiSources } from './test-sources';
import { PRESET_IDS, PRESETS, SUPERSEDED_KERF_LIGHT, type ColorToken } from './theme';
import { GENERATED_TITLE_FILL } from './titles';

// Two guards that keep "every color is themable" and "every theme is readable"
// true as the UI grows.
//
//  1. No color literal outside the places that are allowed to hold one. A
//     `#hex` or `rgb()` in a component is a color no theme can change — which is
//     how a logo that was fine on Kerf Dark came to be invisible on Kerf Light.
//  2. Every preset meets WCAG contrast for the pairs the UI actually draws.

const REPO = join(SRC, '..', '..');

// ---- 1. color literals ------------------------------------------------------

/** The only files that may write a color down, and why. Anything else uses a
 *  token (`var(--…)`, `color-mix` of tokens). Paths are relative to `src/`. */
const LITERALS_ALLOWED: Record<string, string> = {
	'lib/theme.ts': 'the presets: where a theme\'s colors are written down',
	'lib/styles/kerf-tokens.css': 'the token stylesheet: the defaults the Kerf Dark preset must equal',
	'app.html': 'the paint before the app loads; checked below to equal Kerf Dark',
	'lib/sample-frame.ts': 'the browser harness\'s stand-in for a decoded frame: picture content, not interface',
	'lib/sample-filmstrip.ts': 'the browser harness\'s stand-in for a filmstrip: picture content, not interface'
};

/** Hex colors (3, 4, 6 or 8 digits; not `&#123;` entities or `a#b` anchors) and
 *  the functional notations. `color-mix(` and `var(` are not colors, and a CSS
 *  named color is not looked for: `white` is also a word. */
const COLOR_LITERAL =
	/(?<![\w&])#(?:[0-9a-f]{8}|[0-9a-f]{6}|[0-9a-f]{4}|[0-9a-f]{3})(?![\w-])|\b(?:rgba?|hsla?|hwb|lab|lch|oklab|oklch)\(/gi;

/** Comments say things like `#e29d2e` on purpose; only code counts. */
function withoutComments(text: string): string {
	return text
		.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ' '))
		.replace(/<!--[\s\S]*?-->/g, (m) => m.replace(/[^\n]/g, ' '))
		.replace(/(^|\s)\/\/.*$/gm, '$1');
}

/** Each color literal in `text`, with its 1-based line. */
function colorLiterals(text: string): { line: number; literal: string }[] {
	const code = withoutComments(text);
	return [...code.matchAll(COLOR_LITERAL)].map((m) => ({
		line: code.slice(0, m.index).split('\n').length,
		literal: m[0]
	}));
}

describe('color literals', () => {
	test('the detector finds colors and leaves the rest alone', () => {
		const found = (s: string) => colorLiterals(s).map((c) => c.literal);
		expect(found('background:#fff')).toEqual(['#fff']);
		expect(found('a{color:#E29D2E;b:#0d111688;c:#abcd}')).toEqual(['#E29D2E', '#0d111688', '#abcd']);
		expect(found('box-shadow:0 1px rgba(0,0,0,.5);x:hsl(10 40% 30%);y:oklch(0.5 0.1 20)')).toEqual([
			'rgba(',
			'hsl(',
			'oklch('
		]);
		expect(found('fill="#000"')).toEqual(['#000']);

		expect(found('color:var(--kerf-500);background:color-mix(in srgb,var(--scrim) 55%,transparent)')).toEqual([]);
		expect(found('&#123; #${frame} item#anchor #root')).toEqual([]);
		expect(found('hexToRgb(x); label(y); const rgbValue = 1')).toEqual([]);
		expect(found('// #e29d2e is the amber\nconst a = 1; /* rgba(0,0,0,1) */\n<!-- #fff -->')).toEqual([]);
		expect(found('fetch("https://example.com/x")')).toEqual([]);
	});

	test('the detector reports the line a literal is on, comments and all', () => {
		const text = '/* one\n two #fff */\nconst a = 1;\nconst b = "#abc";';
		expect(colorLiterals(text)).toEqual([{ line: 4, literal: '#abc' }]);
	});

	test('no component, route or stylesheet writes a color down', () => {
		const files = uiSources(SRC).map(posix);
		expect(files.length).toBeGreaterThan(100);
		expect(files.some((f) => f.startsWith('lib/components/editor/'))).toBe(true);

		const offenders: string[] = [];
		for (const file of files) {
			if (file in LITERALS_ALLOWED) continue;
			for (const { line, literal } of colorLiterals(readFileSync(join(SRC, file), 'utf8'))) {
				offenders.push(`${file}:${line}  ${literal}  — use a token from kerf-tokens.css`);
			}
		}
		expect(offenders).toEqual([]);
	});

	test('every exception is still needed', () => {
		// A stale entry would quietly let literals back into a file that no longer
		// has any reason to hold them.
		for (const file of Object.keys(LITERALS_ALLOWED)) {
			const path = join(SRC, file);
			expect(existsSync(path), `${file} no longer exists — drop it from LITERALS_ALLOWED`).toBe(true);
			expect(colorLiterals(readFileSync(path, 'utf8')).length, `${file} holds no literal — drop it from LITERALS_ALLOWED`).toBeGreaterThan(0);
		}
	});

	test('what paints before the theme applies is Kerf Dark', () => {
		// `app.html` paints while the bundle loads and the window's own backdrop shows
		// in the instant before that; both have to be the color the app starts in.
		const dark = PRESETS['kerf-dark'].colors['surface-app'];
		const html = readFileSync(join(SRC, 'app.html'), 'utf8');
		expect(html).toMatch(new RegExp(`<html[^>]*style="[^"]*background:\\s*${dark}\\b`, 'i'));
		expect(html).toMatch(/<html[^>]*style="[^"]*color-scheme:\s*dark/i);

		// A detached panel's window paints the same until the live theme is mirrored in.
		const popout = readFileSync(join(SRC, '..', 'static', 'popout.html'), 'utf8');
		expect(popout).toMatch(new RegExp(`<html[^>]*style="[^"]*background:\\s*${dark}\\b`, 'i'));
		expect(popout).toMatch(/<html[^>]*style="[^"]*color-scheme:\s*dark/i);

		const conf = JSON.parse(readFileSync(join(REPO, 'crates/kerf-app/tauri.conf.json'), 'utf8'));
		const main = conf.app.windows.find((w: { label: string }) => w.label === 'main');
		expect(main.visible, 'the main window starts hidden; the page shows it once themed').toBe(false);
		expect(main.backgroundColor.toLowerCase()).toBe(dark);
	});
});

// ---- 1b. the disabled color --------------------------------------------------

/** The only files that draw in `text-disabled`, and what for. It is the color of what is *off*
 *  — a greyed menu row, a disabled button — and it is dimmer than anything people are meant to
 *  read: 3.3:1 on a panel. The status bar, every explanatory paragraph and the idle S / L / DUCK
 *  toggles used it, which is how the Settings gear came to be the dimmest thing in the window.
 *  Text people read is `text-muted` (4.5:1 or better) and a toggle that is live but idle is
 *  `text-muted` too. Paths are relative to `src/`. */
const DISABLED_INK_ALLOWED: Record<string, string> = {
	'lib/styles/kerf-tokens.css': 'the token stylesheet',
	'lib/styles/controls.css': 'the label of a disabled Btn / IconBtn',
	'lib/components/editor/MenuBar.svelte': 'the rows of a menu that are off',
	'lib/components/editor/ContextMenu.svelte': 'the rows of a menu that are off',
	'lib/components/editor/NotificationCenter.svelte': 'a note\'s icon, a read mark, and the buttons that have nothing to act on',
	'lib/components/editor/Timeline.svelte': 'the ruler\'s minor ticks, a mark and not text',
	'lib/components/editor/AgentPanel.svelte': 'the empty queue\'s decorative icon'
};

describe('the disabled color', () => {
	test('only what is off is drawn in it', () => {
		const offenders: string[] = [];
		for (const file of uiSources(SRC).map(posix)) {
			if (file in DISABLED_INK_ALLOWED) continue;
			const code = withoutComments(readFileSync(join(SRC, file), 'utf8'));
			for (const m of code.matchAll(/--text-disabled/g)) {
				offenders.push(`${file}:${code.slice(0, m.index).split('\n').length}  — text people read is text-muted`);
			}
		}
		expect(offenders).toEqual([]);
	});

	test('every exception is still needed', () => {
		for (const file of Object.keys(DISABLED_INK_ALLOWED)) {
			const path = join(SRC, file);
			expect(existsSync(path), `${file} no longer exists — drop it from DISABLED_INK_ALLOWED`).toBe(true);
			expect(
				readFileSync(path, 'utf8').includes('--text-disabled'),
				`${file} no longer uses it — drop it from DISABLED_INK_ALLOWED`
			).toBe(true);
		}
	});
});

// ---- 2. contrast ------------------------------------------------------------
//
// WCAG 2.x contrast ratios between the opaque tokens of a preset, for the pairs
// the UI really draws — chosen from how the components use the tokens, in three
// tiers:
//
//   READING (4.5:1, "AA" for text)  The words people read. text-primary and
//     text-secondary on every surface they sit on, text-muted on the resting
//     surfaces, the label colour on each solid fill (text-on-accent on the amber
//     primary fill and the red badge, agent-fg on the agent fill, text-on-video
//     on the clip bodies it is drawn over).
//   SECONDARY (3:1)  What is not body text: text-muted on the hover / active
//     states, accent and status hues used as text or icon colour on the
//     panels, the amber chips that carry text-on-accent (the S / L flags), the
//     label of a disabled control (text-disabled — see below), and the strokes and
//     marks that carry meaning — clip edges and the playhead / selection amber
//     against the lane, the waveform against its clip, the drag ghost.
//   COMPOSITES  Fills the stylesheet builds with color-mix, resolved per preset
//     and held to READING under the label that sits on them (the generated-caption
//     block in the titles lane), and the *tinted surfaces* — a badge, an analysis
//     chip, an error block is its hue at 10–16 % over whatever it sits on, and the
//     hue's own light shade is the text on it, which no pair of opaque tokens can
//     say (see TINTED below).
//
// Not checked, on purpose:
//   - text-disabled on a *disabled* control: inactive components are exempt from
//     WCAG contrast. It is still held to 3:1 on the resting surfaces so a greyed
//     label can be found, and required to read dimmer than text-muted (below).
//     It is no longer the colour of anything live — the DUCK / S / L toggles, the
//     bell and the status bar were drawn in it — and the scan in "the disabled
//     color" keeps it that way.
//   - Translucent hairlines (border-*, timeline grid, scrims): they are derived in
//     the stylesheet by color-mix, so they are not a pair of opaque tokens; they
//     separate rather than identify.
//   - text-on-video over real footage: the picture decides, not the theme.
//
// A preset that fails a pair gets its *values* fixed (theme.ts), not the pair.

const RESTING: ColorToken[] = ['surface-void', 'surface-app', 'surface-panel', 'surface-raised', 'surface-inset', 'input', 'track-bg'];
const STATES: ColorToken[] = ['surface-hover', 'surface-active'];
const PANELS: ColorToken[] = ['surface-app', 'surface-panel', 'surface-raised'];
const CLIP_BODIES: ColorToken[] = ['track-video', 'track-audio', 'track-text'];

const READING = 4.5;
const SECONDARY = 3;

interface Pair {
	fg: ColorToken;
	bg: ColorToken;
	min: number;
	what: string;
}

function cross(fgs: ColorToken[], bgs: ColorToken[], min: number, what: string): Pair[] {
	return fgs.flatMap((fg) => bgs.map((bg) => ({ fg, bg, min, what })));
}

const PAIRS: Pair[] = [
	// Reading text.
	...cross(['text-primary', 'text-secondary'], [...RESTING, ...STATES], READING, 'body text on a surface'),
	...cross(['text-muted'], RESTING, READING, 'muted text on a resting surface'),
	...cross(['text-on-accent'], ['kerf-500', 'red-500'], READING, 'label on the amber primary fill / the danger badge'),
	...cross(['agent-fg'], ['agent-500'], READING, 'label on the agent fill'),
	...cross(['text-on-video'], CLIP_BODIES, READING, 'clip name on the clip body'),
	...cross(['text-inverted'], ['text-primary'], READING, 'inverted label on a text-colored fill'),
	// Secondary text, status hues and the marks that carry meaning.
	...cross(['text-muted'], STATES, SECONDARY, 'muted text on a hover / active state'),
	...cross(['kerf-300'], ['surface-hover'], READING, 'amber text on a hovered or selected row (the bin, the inspector)'),
	...cross(['text-disabled'], RESTING, SECONDARY, 'a disabled control\'s label stays findable'),
	...cross(
		['kerf-300', 'kerf-400', 'agent-300', 'agent-400', 'green-400', 'red-400', 'orange-400', 'green-500', 'orange-500', 'red-500'],
		PANELS,
		SECONDARY,
		'accent / status hue as text or icon on a panel'
	),
	...cross(['text-on-accent'], ['kerf-300', 'kerf-400'], SECONDARY, 'label on the amber S / L flag chips'),
	// What a filled button does under the pointer: the primary and agent fills step to their
	// 600 shade on hover and 700 while pressed (`controls.css`), the label stays.
	...cross(['text-on-accent'], ['kerf-600'], READING, 'label on the amber primary fill, hovered'),
	...cross(['text-on-accent'], ['kerf-700'], SECONDARY, 'label on the amber primary fill, pressed'),
	...cross(['agent-fg'], ['agent-600'], READING, 'label on the agent fill, hovered'),
	...cross(['agent-fg'], ['agent-700'], SECONDARY, 'label on the agent fill, pressed'),
	// The keyboard focus outline is drawn in kerf-400 on whatever the control sits on.
	...cross(['kerf-400'], RESTING, SECONDARY, 'keyboard focus outline on a resting surface'),
	...cross(
		['track-video-edge', 'track-audio-edge', 'track-text-edge', 'kerf-500', 'kerf-400', 'drag-ghost'],
		['track-bg'],
		SECONDARY,
		'clip edge, playhead / selection amber and drag ghost on the lane'
	),
	...cross(['kerf-500'], ['surface-panel'], SECONDARY, 'playhead amber on the ruler'),
	...cross(['waveform'], ['track-audio'], SECONDARY, 'waveform on its clip')
];

/** The pairs a set of colors does not meet, each phrased with its numbers. */
function failures(colors: Record<ColorToken, string>, pairs: Pair[]): string[] {
	return pairs.flatMap((p) => {
		const ratio = contrastRatio(colors[p.fg], colors[p.bg]);
		return ratio >= p.min
			? []
			: [`${p.fg} ${colors[p.fg]} on ${p.bg} ${colors[p.bg]}: ${ratio.toFixed(2)}:1, needs ${p.min}:1 (${p.what})`];
	});
}

describe('contrast', () => {
	test('the pair list is not vacuous', () => {
		expect(PAIRS.length).toBeGreaterThan(60);
		expect(new Set(PAIRS.map((p) => p.what)).size).toBeGreaterThan(10);
	});

	for (const id of PRESET_IDS) {
		test(`${PRESETS[id].name} meets WCAG contrast for every pair the UI draws`, () => {
			expect(failures(PRESETS[id].colors, PAIRS)).toEqual([]);
		});

		test(`${PRESETS[id].name}: disabled text reads as dimmer than muted text`, () => {
			const c = PRESETS[id].colors;
			for (const bg of PANELS) {
				expect(contrastRatio(c['text-disabled'], c[bg]), bg).toBeLessThan(contrastRatio(c['text-muted'], c[bg]));
			}
		});
	}

	test('High contrast reaches the enhanced level (7:1) for its reading text', () => {
		// What makes the preset high contrast is its text; the 4.5 floor above is the
		// bar every preset clears, this is the one it is for.
		const enhanced: Pair[] = [
			...cross(['text-primary', 'text-secondary', 'text-muted'], RESTING, 7, 'reading text on a resting surface'),
			...cross(['text-on-accent'], ['kerf-500'], 7, 'label on the amber primary fill'),
			...cross(['agent-fg'], ['agent-500'], 7, 'label on the agent fill'),
			...cross(['text-on-video'], CLIP_BODIES, 7, 'clip name on the clip body')
		];
		expect(failures(PRESETS['high-contrast'].colors, enhanced)).toEqual([]);
	});

	// ---- tinted surfaces ------------------------------------------------------
	//
	// `--success-surface` and its siblings are `color-mix(in srgb, <hue> N%, transparent)`:
	// over a panel they composite to a faint wash of the hue, and the Badge, the analysis
	// chips and the error blocks put the hue's light shade (`green-400` …) on it as 10 px
	// text. The guard above only held those hues against the opaque panel, which is how
	// Kerf Light's "Saved" badge came to read at 3.2:1.

	const TOKENS_CSS = readFileSync(join(SRC, 'lib/styles/kerf-tokens.css'), 'utf8');

	/** A translucent token of the stylesheet — `color-mix(in srgb, var(--hue) N%, transparent)` — as its hue and weight. */
	function tintOf(name: string): Tint {
		const m = new RegExp(`--${name}:\\s*color-mix\\(in srgb,\\s*var\\(--([a-z0-9-]+)\\)\\s+([\\d.]+)%,\\s*transparent\\)`).exec(TOKENS_CSS);
		if (!m) throw new Error(`--${name} is not a color-mix of one token over transparent in kerf-tokens.css`);
		return { hue: m[1] as ColorToken, weight: Number(m[2]) / 100 };
	}

	interface Tint {
		hue: ColorToken;
		weight: number;
	}

	interface TintPair {
		fg: ColorToken;
		tint: Tint;
		min: number;
		what: string;
		/** What it sits on; the three panels and the hover surface unless it says otherwise. */
		on?: ColorToken[];
	}

	/** What a tint is once it sits on a surface. */
	const SURFACES_UNDER_TINTS: ColorToken[] = [...PANELS, 'surface-hover'];

	const TINTED: TintPair[] = [
		{ fg: 'green-400', tint: tintOf('success-surface'), min: READING, what: 'success badge, finished analysis chip' },
		{ fg: 'orange-400', tint: tintOf('warning-surface'), min: READING, what: 'warning badge, "links off" tag, update warning' },
		{ fg: 'red-400', tint: tintOf('danger-surface'), min: READING, what: 'danger badge, failed analysis chip' },
		{ fg: 'agent-300', tint: tintOf('agent-surface'), min: READING, what: 'agent badge, running analysis chip' },
		{ fg: 'kerf-200', tint: tintOf('selection-fill'), min: READING, what: 'amber badge (360, voiceover, the preview\'s file name)' },
		// The timeline toolbar's lit toggles (snap, ripple, fades, heights, overview, delivery frame) draw
		// kerf-300 on the selection wash; the toolbar is a panel, so the hover surface is not under it.
		{ fg: 'kerf-300', tint: tintOf('selection-fill'), min: READING, what: 'lit toggle in the timeline toolbar', on: PANELS },
		// Written inline in the dialogs' error blocks: `color-mix(in srgb,var(--red-600) 14%,transparent)`.
		{ fg: 'red-400', tint: { hue: 'red-600', weight: 0.14 }, min: READING, what: 'error block in a dialog' },
		// Toasts: the kind's 12 % wash over the toast's own surface (`routes/layout.css`), its line, its
		// description, and its glyph and edge in the `-400` shade.
		{ fg: 'text-primary', tint: tintOf('success-surface'), min: READING, what: 'success toast', on: ['surface-raised'] },
		{ fg: 'text-secondary', tint: tintOf('success-surface'), min: READING, what: 'success toast description', on: ['surface-raised'] },
		{ fg: 'text-secondary', tint: tintOf('danger-surface'), min: READING, what: 'error toast description', on: ['surface-raised'] },
		{ fg: 'text-secondary', tint: tintOf('warning-surface'), min: READING, what: 'warning toast description', on: ['surface-raised'] },
		{ fg: 'text-primary', tint: tintOf('danger-surface'), min: READING, what: 'agent connection banner' },
		{ fg: 'text-primary', tint: tintOf('warning-surface'), min: READING, what: 'keyboard conflict prompt' },
		{ fg: 'text-secondary', tint: tintOf('diff-remove-surface'), min: READING, what: 'stale proposal note' },
		{ fg: 'text-secondary', tint: tintOf('agent-surface'), min: READING, what: 'agent panel header and active chip' }
	];

	function tintFailures(colors: Record<ColorToken, string>, pairs: TintPair[]): string[] {
		return pairs.flatMap((p) =>
			(p.on ?? SURFACES_UNDER_TINTS).flatMap((surface) => {
				const fill = mixSrgb(colors[p.tint.hue], colors[surface], p.tint.weight);
				const ratio = contrastRatio(colors[p.fg], fill);
				return ratio >= p.min
					? []
					: [
							`${p.fg} ${colors[p.fg]} on ${p.tint.hue} ${Math.round(p.tint.weight * 100)}% over ${surface} (${fill}): ${ratio.toFixed(2)}:1, needs ${p.min}:1 (${p.what})`
						];
			})
		);
	}

	for (const id of PRESET_IDS) {
		test(`${PRESETS[id].name}: text on a tinted surface is readable`, () => {
			expect(tintFailures(PRESETS[id].colors, TINTED)).toEqual([]);
		});
	}

	test('the tinted-surface check would have caught Kerf Light as it shipped', () => {
		// As of v0.24.0 Light's `green-400` and `orange-400` read 3.2:1 and 3.1:1 on their own washes,
		// and its `kerf-300` 3.8:1 on the selection wash the toolbar's lit toggles sit on.
		const shipped = SUPERSEDED_KERF_LIGHT.find((o) => o['green-400'] === '#1f8f60' && o['text-disabled'] === '#76828f')!;
		const out = tintFailures(shipped, TINTED);
		for (const hue of ['green-400', 'orange-400', 'kerf-300']) expect(out.some((l) => l.startsWith(hue)), hue).toBe(true);
		// …and kerf-300 alone is what the later set still failed.
		const later = tintFailures(SUPERSEDED_KERF_LIGHT[SUPERSEDED_KERF_LIGHT.length - 1], TINTED);
		expect(later.every((l) => l.startsWith('kerf-300'))).toBe(true);
		expect(later.length).toBeGreaterThan(0);
	});

	// ---- component edges --------------------------------------------------------

	/** An edge of `weight` (a theme's border alpha) over a surface, as the opaque color it draws. */
	const edge = (colors: Record<ColorToken, string>, alpha: number, surface: ColorToken) =>
		mixSrgb(colors['border-base'], colors[surface], alpha / 100);

	test('High contrast outlines its components at 3:1 or better (WCAG 1.4.11)', () => {
		const hc = PRESETS['high-contrast'];
		const bad: string[] = [];
		for (const surface of [...PANELS, 'surface-hover' as ColorToken]) {
			for (const [token, min] of [
				['border-alpha-default', 3],
				['border-alpha-strong', 4.5]
			] as const) {
				const ratio = contrastRatio(edge(hc.colors, hc.shape[token], surface), hc.colors[surface]);
				if (ratio < min) bad.push(`${token} ${hc.shape[token]}% on ${surface}: ${ratio.toFixed(2)}:1, needs ${min}:1`);
			}
		}
		expect(bad).toEqual([]);
	});

	for (const id of PRESET_IDS) {
		test(`${PRESETS[id].name}: the three border weights step up`, () => {
			const { shape } = PRESETS[id];
			expect(shape['border-alpha-subtle']).toBeLessThan(shape['border-alpha-default']);
			expect(shape['border-alpha-default']).toBeLessThan(shape['border-alpha-strong']);
		});
	}

	/** `color-mix(in srgb,var(--a) P%,var(--b))` resolved against a preset's colors. */
	function resolveMix(expr: string, colors: Record<ColorToken, string>): string {
		const m = /^color-mix\(in srgb,\s*var\(--([a-z0-9-]+)\)\s+([\d.]+)%,\s*var\(--([a-z0-9-]+)\)\)$/.exec(expr);
		if (!m) throw new Error(`not a two-token srgb color-mix: ${expr}`);
		return mixSrgb(colors[m[1] as ColorToken], colors[m[3] as ColorToken], Number(m[2]) / 100);
	}

	for (const id of PRESET_IDS) {
		test(`${PRESETS[id].name}: a generated caption's block keeps its label readable`, () => {
			const c = PRESETS[id].colors;
			const fill = resolveMix(GENERATED_TITLE_FILL, c);
			const ratio = contrastRatio(c['text-on-video'], fill);
			expect(ratio, `text-on-video ${c['text-on-video']} on the generated fill ${fill}`).toBeGreaterThanOrEqual(READING);
		});
	}

	test('the composite check would have caught the mix it replaced', () => {
		// Dimming the title color toward the panel — a pale one on Kerf Light — left
		// the white label on it at 2.7:1.
		const c = PRESETS['kerf-light'].colors;
		const washed = resolveMix('color-mix(in srgb,var(--track-text) 55%,var(--surface-panel))', c);
		expect(contrastRatio(c['text-on-video'], washed)).toBeLessThan(READING);
	});

	test('a failing pair is reported with its numbers', () => {
		// The guard has to fail loudly rather than pass vacuously: muted text that is
		// nearly the surface's own color must be caught, and named.
		const broken = { ...PRESETS['kerf-dark'].colors, 'text-muted': '#1a1f26' };
		const out = failures(broken, PAIRS);
		expect(out.length).toBeGreaterThan(0);
		expect(out[0]).toContain('text-muted #1a1f26 on surface-void');
		expect(out[0]).toMatch(/:1, needs 4\.5:1 \(muted text on a resting surface\)/);
	});
});
