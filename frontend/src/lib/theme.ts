// The look, as data. Every color the editor chrome draws with is a token in
// `styles/kerf-tokens.css`; a theme is one opaque hex value per token, applied
// as inline custom properties on the root element so it wins over the
// stylesheet. Alpha variants (borders, fills, glows) are derived in the CSS
// with color-mix from these bases, which is what keeps a theme a flat list of
// colors a picker can edit.

export type Scheme = 'dark' | 'light';

export const COLOR_GROUPS = [
	{
		label: 'Surfaces',
		tokens: [
			{ name: 'surface-void', label: 'Void' },
			{ name: 'surface-app', label: 'App' },
			{ name: 'surface-panel', label: 'Panel' },
			{ name: 'surface-raised', label: 'Raised' },
			{ name: 'surface-inset', label: 'Inset' },
			{ name: 'surface-hover', label: 'Hover' },
			{ name: 'surface-active', label: 'Active' },
			{ name: 'input', label: 'Input' },
			{ name: 'scrim', label: 'Scrim' },
			{ name: 'border-base', label: 'Border' },
			{ name: 'scrollbar-thumb', label: 'Scrollbar' },
			{ name: 'scrollbar-thumb-hover', label: 'Scrollbar hover' }
		]
	},
	{
		label: 'Text',
		tokens: [
			{ name: 'text-primary', label: 'Primary' },
			{ name: 'text-secondary', label: 'Secondary' },
			{ name: 'text-muted', label: 'Muted' },
			{ name: 'text-disabled', label: 'Disabled' },
			{ name: 'text-on-accent', label: 'On accent' },
			{ name: 'text-inverted', label: 'Inverted' },
			{ name: 'text-on-video', label: 'On video' }
		]
	},
	{
		label: 'Accent',
		tokens: [
			{ name: 'kerf-200', label: 'Accent 200' },
			{ name: 'kerf-300', label: 'Accent 300' },
			{ name: 'kerf-400', label: 'Accent 400' },
			{ name: 'kerf-500', label: 'Accent 500' },
			{ name: 'kerf-600', label: 'Accent 600' },
			{ name: 'kerf-700', label: 'Accent 700' }
		]
	},
	{
		label: 'Agent',
		tokens: [
			{ name: 'agent-200', label: 'Agent 200' },
			{ name: 'agent-300', label: 'Agent 300' },
			{ name: 'agent-400', label: 'Agent 400' },
			{ name: 'agent-500', label: 'Agent 500' },
			{ name: 'agent-600', label: 'Agent 600' },
			{ name: 'agent-700', label: 'Agent 700' },
			{ name: 'agent-fg', label: 'Agent text' }
		]
	},
	{
		label: 'Status',
		tokens: [
			{ name: 'green-400', label: 'Green light' },
			{ name: 'green-500', label: 'Green' },
			{ name: 'red-400', label: 'Red light' },
			{ name: 'red-500', label: 'Red' },
			{ name: 'red-600', label: 'Red dark' },
			{ name: 'orange-400', label: 'Orange light' },
			{ name: 'orange-500', label: 'Orange' }
		]
	},
	{
		label: 'Timeline',
		tokens: [
			{ name: 'track-bg', label: 'Track background' },
			{ name: 'track-video', label: 'Video clip' },
			{ name: 'track-video-edge', label: 'Video clip edge' },
			{ name: 'track-audio', label: 'Audio clip' },
			{ name: 'track-audio-edge', label: 'Audio clip edge' },
			{ name: 'track-text', label: 'Text clip' },
			{ name: 'track-text-edge', label: 'Text clip edge' },
			{ name: 'waveform', label: 'Waveform' },
			{ name: 'drag-ghost', label: 'Drag ghost' },
			{ name: 'frame-matte', label: 'Frame matte' }
		]
	}
] as const;

export type ColorToken = (typeof COLOR_GROUPS)[number]['tokens'][number]['name'];
export const COLOR_TOKENS: readonly ColorToken[] = COLOR_GROUPS.flatMap((g) => g.tokens.map((t) => t.name));

export const SHAPE_TOKENS = [
	{ name: 'line-width', label: 'Line thickness', hint: 'Panel dividers, borders, clip edges, ruler ticks', min: 0.5, max: 3, step: 0.5, def: 1 },
	{ name: 'line-emphasis', label: 'Selection thickness', hint: 'The selected clip outline', min: 1, max: 4, step: 0.5, def: 1.5 },
	{ name: 'playhead-width', label: 'Playhead thickness', hint: 'The cut line', min: 1, max: 6, step: 1, def: 2 },
	{ name: 'slider-track', label: 'Slider track', hint: 'Thickness of every slider track', min: 1, max: 10, step: 1, def: 4 },
	{ name: 'slider-track-radius', label: 'Track corners', hint: 'Rounding of the track ends', min: 0, max: 5, step: 1, def: 2 },
	{ name: 'slider-thumb', label: 'Slider thumb', hint: 'Size of the slider handle', min: 8, max: 24, step: 1, def: 14 }
] as const;

export type ShapeToken = (typeof SHAPE_TOKENS)[number]['name'];
export const SHAPE_NAMES: readonly ShapeToken[] = SHAPE_TOKENS.map((t) => t.name);

export type ThumbStyle = 'round' | 'bar';
export const THUMB_STYLES: readonly { id: ThumbStyle; label: string }[] = [
	{ id: 'round', label: 'Round' },
	{ id: 'bar', label: 'Bar' }
];

export type Shape = Record<ShapeToken, number> & { 'slider-thumb-style': ThumbStyle };

export interface Theme {
	name: string;
	version: 1;
	scheme: Scheme;
	colors: Record<ColorToken, string>;
	shape: Shape;
}

const SHAPE_DEFAULT = Object.fromEntries(SHAPE_TOKENS.map((t) => [t.name, t.def])) as Record<ShapeToken, number>;
const DEFAULT_SHAPE: Shape = { ...SHAPE_DEFAULT, 'slider-thumb-style': 'round' };

export type PresetId = 'kerf-dark' | 'kerf-light' | 'high-contrast';

export const PRESETS: Record<PresetId, Theme> = {
	'kerf-dark': {
		name: 'Kerf Dark',
		version: 1,
		scheme: 'dark',
		colors: {
			'surface-void': '#0a0c0f',
			'surface-app': '#0f1318',
			'surface-panel': '#14181e',
			'surface-raised': '#181d24',
			'surface-inset': '#0a0c0f',
			'surface-hover': '#222932',
			'surface-active': '#2c3540',
			input: '#10151b',
			scrim: '#000000',
			'border-base': '#ffffff',
			'scrollbar-thumb': '#2c3540',
			'scrollbar-thumb-hover': '#36404c',
			'text-primary': '#eef1f5',
			'text-secondary': '#97a3b2',
			'text-muted': '#7a8696',
			'text-disabled': '#5e6b7a',
			'text-on-accent': '#1a1205',
			'text-inverted': '#0a0c0f',
			'text-on-video': '#ffffff',
			'kerf-200': '#f6d9a3',
			'kerf-300': '#f0c277',
			'kerf-400': '#eab14e',
			'kerf-500': '#e29d2e',
			'kerf-600': '#c8851c',
			'kerf-700': '#9a6413',
			'agent-200': '#aef0ec',
			'agent-300': '#6fe0e0',
			'agent-400': '#3fcdd4',
			'agent-500': '#22b4c4',
			'agent-600': '#1894a6',
			'agent-700': '#126f80',
			'agent-fg': '#04181c',
			'green-400': '#5fe0a8',
			'green-500': '#28c585',
			'red-400': '#ff8a80',
			'red-500': '#f1543f',
			'red-600': '#d63a27',
			'orange-400': '#ffb24a',
			'orange-500': '#f59020',
			'track-bg': '#11151a',
			'track-video': '#294253',
			'track-video-edge': '#5b93b0',
			'track-audio': '#234438',
			'track-audio-edge': '#4f8f78',
			'track-text': '#3a2c49',
			'track-text-edge': '#8a6aa8',
			waveform: '#6fcfa8',
			'drag-ghost': '#788cff',
			'frame-matte': '#000000'
		},
		shape: { ...DEFAULT_SHAPE }
	},
	'kerf-light': {
		name: 'Kerf Light',
		version: 1,
		scheme: 'light',
		colors: {
			'surface-void': '#e2e6ea',
			'surface-app': '#f2f4f6',
			'surface-panel': '#ffffff',
			'surface-raised': '#f6f8fa',
			'surface-inset': '#e9edf1',
			'surface-hover': '#e4e8ed',
			'surface-active': '#d3d9e0',
			input: '#ffffff',
			scrim: '#000000',
			'border-base': '#000000',
			'scrollbar-thumb': '#c2c9d1',
			'scrollbar-thumb-hover': '#a9b2bd',
			'text-primary': '#14181e',
			'text-secondary': '#3f4956',
			'text-muted': '#5a6776',
			'text-disabled': '#97a3b2',
			'text-on-accent': '#1a1205',
			'text-inverted': '#ffffff',
			'text-on-video': '#ffffff',
			'kerf-200': '#7a4f0e',
			'kerf-300': '#9a6413',
			'kerf-400': '#a96d11',
			'kerf-500': '#b8770f',
			'kerf-600': '#e29d2e',
			'kerf-700': '#eab14e',
			'agent-200': '#0b4a54',
			'agent-300': '#126f80',
			'agent-400': '#1894a6',
			'agent-500': '#1a9fb0',
			'agent-600': '#22b4c4',
			'agent-700': '#3fcdd4',
			'agent-fg': '#04181c',
			'green-400': '#1f8f60',
			'green-500': '#1c9a68',
			'red-400': '#c8362a',
			'red-500': '#e4452f',
			'red-600': '#b32d1e',
			'orange-400': '#c46b0c',
			'orange-500': '#c76f0a',
			'track-bg': '#eceff2',
			'track-video': '#3b6d8c',
			'track-video-edge': '#3f7ea3',
			'track-audio': '#2f6f56',
			'track-audio-edge': '#2f7d5e',
			'track-text': '#5d4180',
			'track-text-edge': '#7b56a0',
			waveform: '#a8ecca',
			'drag-ghost': '#4a5fd6',
			'frame-matte': '#000000'
		},
		shape: { ...DEFAULT_SHAPE }
	},
	'high-contrast': {
		name: 'High contrast',
		version: 1,
		scheme: 'dark',
		colors: {
			'surface-void': '#000000',
			'surface-app': '#000000',
			'surface-panel': '#0a0a0a',
			'surface-raised': '#141414',
			'surface-inset': '#000000',
			'surface-hover': '#262626',
			'surface-active': '#3a3a3a',
			input: '#050505',
			scrim: '#000000',
			'border-base': '#ffffff',
			'scrollbar-thumb': '#4d4d4d',
			'scrollbar-thumb-hover': '#666666',
			'text-primary': '#ffffff',
			'text-secondary': '#e6e6e6',
			'text-muted': '#c4c4c4',
			'text-disabled': '#8f8f8f',
			'text-on-accent': '#000000',
			'text-inverted': '#000000',
			'text-on-video': '#ffffff',
			'kerf-200': '#ffe2a8',
			'kerf-300': '#ffd280',
			'kerf-400': '#ffc247',
			'kerf-500': '#ffb000',
			'kerf-600': '#e69d00',
			'kerf-700': '#b37a00',
			'agent-200': '#c8fbff',
			'agent-300': '#9ef3ff',
			'agent-400': '#5fe9ff',
			'agent-500': '#22d3ee',
			'agent-600': '#06b6d4',
			'agent-700': '#0891b2',
			'agent-fg': '#000000',
			'green-400': '#6ee7a0',
			'green-500': '#22d37a',
			'red-400': '#ff9d94',
			'red-500': '#ff5c47',
			'red-600': '#e63b26',
			'orange-400': '#ffc266',
			'orange-500': '#ff9a1f',
			'track-bg': '#050505',
			'track-video': '#1e3a4f',
			'track-video-edge': '#7cc4f0',
			'track-audio': '#173d2c',
			'track-audio-edge': '#6be0a8',
			'track-text': '#35244a',
			'track-text-edge': '#c39bf0',
			waveform: '#8ef5c2',
			'drag-ghost': '#9aa8ff',
			'frame-matte': '#000000'
		},
		shape: {
			...DEFAULT_SHAPE,
			'line-width': 1.5,
			'line-emphasis': 3,
			'playhead-width': 3,
			'slider-track': 6,
			'slider-thumb': 16
		}
	}
};

export const PRESET_IDS = Object.keys(PRESETS) as PresetId[];

const HEX = /^#[0-9a-f]{6}$/i;

function shapeEqual(a: Shape, b: Shape): boolean {
	return SHAPE_NAMES.every((t) => a[t] === b[t]) && a['slider-thumb-style'] === b['slider-thumb-style'];
}

/** A shape value pulled into its token's range and onto its step. */
export function clampShape(token: ShapeToken, v: number): number {
	const t = SHAPE_TOKENS.find((x) => x.name === token)!;
	const stepped = Math.round(v / t.step) * t.step;
	return Math.min(t.max, Math.max(t.min, stepped));
}

/** The preset a theme's colors and shape are, or `custom`. The name is not consulted:
 *  a renamed copy of Kerf Dark is still Kerf Dark. */
export function presetIdFor(theme: Theme): PresetId | 'custom' {
	for (const id of PRESET_IDS) {
		const p = PRESETS[id].colors;
		if (
			COLOR_TOKENS.every((t) => p[t].toLowerCase() === theme.colors[t].toLowerCase()) &&
			shapeEqual(PRESETS[id].shape, theme.shape)
		)
			return id;
	}
	return 'custom';
}

/** A theme from stored or imported JSON, or `null` when it is not one.
 *  Unknown tokens are dropped and missing ones filled from the scheme's
 *  preset, so a file from a build with fewer tokens still applies. */
export function parseTheme(raw: unknown): Theme | null {
	if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) return null;
	const r = raw as Record<string, unknown>;
	if (r.version !== 1) return null;
	if (r.scheme !== 'dark' && r.scheme !== 'light') return null;
	if (typeof r.name !== 'string' || r.name.trim() === '') return null;
	if (typeof r.colors !== 'object' || r.colors === null) return null;
	const given = r.colors as Record<string, unknown>;
	const base = PRESETS[r.scheme === 'light' ? 'kerf-light' : 'kerf-dark'].colors;
	const colors = {} as Record<ColorToken, string>;
	for (const t of COLOR_TOKENS) {
		const v = given[t];
		if (v === undefined) {
			colors[t] = base[t];
			continue;
		}
		if (typeof v !== 'string' || !HEX.test(v)) return null;
		colors[t] = v.toLowerCase();
	}
	const baseShape = PRESETS[r.scheme === 'light' ? 'kerf-light' : 'kerf-dark'].shape;
	const gs = typeof r.shape === 'object' && r.shape !== null && !Array.isArray(r.shape) ? (r.shape as Record<string, unknown>) : {};
	const shape = { ...baseShape };
	for (const t of SHAPE_NAMES) {
		const v = gs[t];
		if (typeof v === 'number' && Number.isFinite(v)) shape[t] = clampShape(t, v);
	}
	const ts = gs['slider-thumb-style'];
	if (ts === 'round' || ts === 'bar') shape['slider-thumb-style'] = ts;
	return { name: r.name.trim(), version: 1, scheme: r.scheme, colors, shape };
}

/** The custom properties a shape sets. The thumb is a circle (or square at
 *  size) when round, and a narrow upright bar when not; the stylesheet
 *  defaults are the round case. */
export function shapeProps(shape: Shape): Record<string, string> {
	const size = shape['slider-thumb'];
	const bar = shape['slider-thumb-style'] === 'bar';
	const props: Record<string, string> = {};
	for (const t of SHAPE_NAMES) props[`--${t}`] = `${shape[t]}px`;
	props['--slider-thumb-w'] = `${bar ? Math.max(4, Math.round(size * 0.4)) : size}px`;
	props['--slider-thumb-radius'] = bar ? '2px' : '999px';
	return props;
}

export function themeJson(theme: Theme): string {
	return JSON.stringify(theme, null, 2) + '\n';
}

/** Put a theme into force: the tokens as inline properties on the root (so
 *  they beat the stylesheet), the `dark` class and `color-scheme` for anything
 *  that keys off them, and the pre-hydration background `app.html` painted. */
export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement) {
	for (const t of COLOR_TOKENS) root.style.setProperty(`--${t}`, theme.colors[t]);
	for (const [k, v] of Object.entries(shapeProps(theme.shape))) root.style.setProperty(k, v);
	root.classList.toggle('dark', theme.scheme === 'dark');
	root.style.colorScheme = theme.scheme;
	root.style.background = theme.colors['surface-app'];
}
