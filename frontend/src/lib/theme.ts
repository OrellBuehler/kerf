// The look, as data. Every color the editor chrome draws with is a token in
// `styles/kerf-tokens.css`; a theme is one opaque hex value per token, applied
// as inline custom properties on the root element so it wins over the
// stylesheet. Alpha variants (borders, fills, glows) are derived in the CSS
// with color-mix from these bases, which is what keeps a theme a flat list of
// colors a picker can edit.

import { relativeLuminance } from './contrast';
import { DEFAULT_MONO_FONT, DEFAULT_UI_FONT, fontProps, isMonoFont, isUiFont, type MonoFont, type UiFont } from './theme-fonts';

export type Scheme = 'dark' | 'light';

/** A token a theme sets, in plain words: `label` is its name in the editor, `hint` says where it shows. */
export const COLOR_GROUPS = [
	{
		label: 'Surfaces',
		blurb: 'What everything sits on, from the window down to a text field.',
		tokens: [
			{ name: 'surface-app', label: 'Window', hint: 'The window, the title bar and the strips between panels' },
			{ name: 'surface-panel', label: 'Panel', hint: 'Panels, dialogs and menus' },
			{ name: 'surface-raised', label: 'Raised', hint: 'A card, menu or toast sitting above a panel' },
			{ name: 'surface-inset', label: 'Inset', hint: 'Wells: the media bin, chips and the dark side of a field' },
			{ name: 'surface-void', label: 'Deepest', hint: 'The darkest places: behind the preview, the empty timeline' },
			{ name: 'input', label: 'Field edge', hint: 'The border of search boxes and text fields' },
			{ name: 'surface-hover', label: 'Hover', hint: 'A row or button under the pointer' },
			{ name: 'surface-active', label: 'Pressed', hint: 'A pressed button, the open tab, the unfilled part of a slider' }
		]
	},
	{
		label: 'Text',
		blurb: 'Every color the words are drawn in.',
		tokens: [
			{ name: 'text-primary', label: 'Main text', hint: 'Titles, values and anything you act on' },
			{ name: 'text-secondary', label: 'Secondary text', hint: 'Labels and descriptions' },
			{ name: 'text-muted', label: 'Muted text', hint: 'Captions, counts and idle toggles' },
			{ name: 'text-disabled', label: 'Disabled text', hint: 'What is switched off' },
			{ name: 'text-on-accent', label: 'Text on accent', hint: 'The label of the amber primary button' },
			{ name: 'text-inverted', label: 'Inverted text', hint: 'A label on a fill drawn in the main text color' },
			{ name: 'text-on-video', label: 'Text on video', hint: 'Clip names and overlays drawn over footage' }
		]
	},
	{
		label: 'Accent',
		blurb: "Kerf's amber: you, the cut and the primary action.",
		tokens: [
			{ name: 'kerf-200', label: 'Accent, lightest', hint: 'Amber badges' },
			{ name: 'kerf-300', label: 'Accent, light', hint: 'Lit toggles and hovered rows' },
			{ name: 'kerf-400', label: 'Accent, focus', hint: 'The keyboard focus ring, links and selected text' },
			{ name: 'kerf-500', label: 'Accent', hint: 'The primary button, the playhead, sliders and the selection' },
			{ name: 'kerf-600', label: 'Accent, hovered', hint: 'The primary button under the pointer' },
			{ name: 'kerf-700', label: 'Accent, pressed', hint: 'The primary button while pressed' }
		]
	},
	{
		label: 'Agent',
		blurb: "Cyan, for the agent: its proposals and what it analyses.",
		tokens: [
			{ name: 'agent-200', label: 'Agent, lightest', hint: 'Light tint of the agent color' },
			{ name: 'agent-300', label: 'Agent, light', hint: 'Agent badges and chips' },
			{ name: 'agent-400', label: 'Agent, medium', hint: 'Beat markers and agent highlights' },
			{ name: 'agent-500', label: 'Agent', hint: 'The agent button, tab count and borders' },
			{ name: 'agent-600', label: 'Agent, hovered', hint: 'The agent button under the pointer' },
			{ name: 'agent-700', label: 'Agent, pressed', hint: 'The agent button while pressed' },
			{ name: 'agent-fg', label: 'Text on agent', hint: 'The label of the agent button' }
		]
	},
	{
		label: 'States',
		blurb: 'Success, warning and error, and the scopes\' blue.',
		tokens: [
			{ name: 'green-400', label: 'Success text', hint: 'Saved, done, finished' },
			{ name: 'green-500', label: 'Success', hint: 'Success marks and meters' },
			{ name: 'orange-400', label: 'Warning text', hint: 'Warnings and caution badges' },
			{ name: 'orange-500', label: 'Warning', hint: 'Warning marks and meters' },
			{ name: 'red-400', label: 'Error text', hint: 'Errors and the delete actions' },
			{ name: 'red-500', label: 'Error', hint: 'Error marks, the muted and clipping states' },
			{ name: 'red-600', label: 'Error, dark', hint: 'Error outlines and washes' },
			{ name: 'blue-500', label: 'Scope blue', hint: 'The blue trace in the scopes' }
		]
	},
	{
		label: 'Edges and depth',
		blurb: 'Lines, shadows, the dialog backdrop and scrollbars.',
		tokens: [
			{ name: 'border-base', label: 'Borders', hint: 'Dividers and outlines (how strong they are is under Shape)' },
			{ name: 'shadow-color', label: 'Shadow', hint: 'What menus, dialogs and the preview frame cast' },
			{ name: 'scrim', label: 'Dialog backdrop', hint: 'The dimming behind a dialog, and fades' },
			{ name: 'scrollbar-thumb', label: 'Scrollbar', hint: 'The handle of a scrollbar' },
			{ name: 'scrollbar-thumb-hover', label: 'Scrollbar, hovered', hint: 'The handle under the pointer' }
		]
	},
	{
		label: 'Timeline and clips',
		blurb: 'The lanes, the three kinds of clip, the waveform and the preview matte.',
		tokens: [
			{ name: 'track-bg', label: 'Track lane', hint: 'The empty lane behind clips' },
			{ name: 'track-video', label: 'Video clip', hint: 'The body of a picture clip' },
			{ name: 'track-video-edge', label: 'Video clip edge', hint: 'Its outline' },
			{ name: 'track-audio', label: 'Audio clip', hint: 'The body of a sound clip' },
			{ name: 'track-audio-edge', label: 'Audio clip edge', hint: 'Its outline' },
			{ name: 'track-text', label: 'Title clip', hint: 'The body of a title or caption' },
			{ name: 'track-text-edge', label: 'Title clip edge', hint: 'Its outline' },
			{ name: 'waveform', label: 'Waveform', hint: 'The wave drawn inside a sound clip' },
			{ name: 'clip-accent', label: 'Clip handles and lines', hint: 'Fade handles, the volume line and keyframes on a clip' },
			{ name: 'drag-ghost', label: 'Drag ghost', hint: 'The outline of a clip being moved or trimmed' },
			{ name: 'frame-matte', label: 'Preview matte', hint: 'Behind the picture in the preview' }
		]
	},
	{
		label: 'Review diff',
		blurb: "How an agent's proposed edits are marked on the timeline and in the review card.",
		tokens: [
			{ name: 'diff-add', label: 'Added', hint: 'A clip or title the proposal adds' },
			{ name: 'diff-remove', label: 'Removed', hint: 'What the proposal takes out' },
			{ name: 'diff-shift', label: 'Changed', hint: 'A clip the proposal moves, trims or adjusts' }
		]
	}
] as const;

export type ColorToken = (typeof COLOR_GROUPS)[number]['tokens'][number]['name'];
export const COLOR_TOKENS: readonly ColorToken[] = COLOR_GROUPS.flatMap((g) => g.tokens.map((t) => t.name));

const TOKEN_INFO = new Map<string, { label: string; hint: string }>(COLOR_GROUPS.flatMap((g) => g.tokens.map((t) => [t.name, t] as const)));

/** A colour token's name in the editor ("Muted text"). */
export function tokenLabel(token: ColorToken): string {
	return TOKEN_INFO.get(token)?.label ?? token;
}

/** Where a colour token shows. */
export function tokenHint(token: ColorToken): string {
	return TOKEN_INFO.get(token)?.hint ?? '';
}

export type ShapeGroup = 'corners' | 'lines' | 'sliders' | 'depth';

export const SHAPE_GROUPS: readonly { id: ShapeGroup; label: string }[] = [
	{ id: 'corners', label: 'Corners' },
	{ id: 'lines', label: 'Lines' },
	{ id: 'sliders', label: 'Sliders' },
	{ id: 'depth', label: 'Depth' }
];

export const SHAPE_TOKENS = [
	{ name: 'radius-scale', group: 'corners', label: 'Corner roundness', hint: 'Every button, field, chip, menu and dialog corner; 0 is square', min: 0, max: 2, step: 0.25, def: 1, unit: '', percent: true },
	{ name: 'line-width', group: 'lines', label: 'Line thickness', hint: 'Panel dividers, borders, clip edges, ruler ticks', min: 0.5, max: 3, step: 0.5, def: 1, unit: 'px' },
	{ name: 'line-emphasis', group: 'lines', label: 'Selection thickness', hint: 'The selected clip outline', min: 1, max: 4, step: 0.5, def: 1.5, unit: 'px' },
	{ name: 'playhead-width', group: 'lines', label: 'Playhead thickness', hint: 'The cut line', min: 1, max: 6, step: 1, def: 2, unit: 'px' },
	{ name: 'border-alpha-subtle', group: 'lines', label: 'Faint borders', hint: 'How opaque the faintest dividers are', min: 0, max: 100, step: 1, def: 6, unit: '%' },
	{ name: 'border-alpha-default', group: 'lines', label: 'Borders', hint: 'How opaque panel borders and dividers are', min: 0, max: 100, step: 1, def: 10, unit: '%' },
	{ name: 'border-alpha-strong', group: 'lines', label: 'Strong borders', hint: 'How opaque the outlines of buttons, chips, inputs and menus are', min: 0, max: 100, step: 1, def: 16, unit: '%' },
	{ name: 'slider-track', group: 'sliders', label: 'Slider track', hint: 'Thickness of every slider track', min: 1, max: 10, step: 1, def: 4, unit: 'px' },
	{ name: 'slider-track-radius', group: 'sliders', label: 'Track corners', hint: 'Rounding of the track ends', min: 0, max: 5, step: 1, def: 2, unit: 'px' },
	{ name: 'slider-thumb', group: 'sliders', label: 'Slider thumb', hint: 'Size of the slider handle', min: 8, max: 24, step: 1, def: 14, unit: 'px' },
	{ name: 'shadow-strength', group: 'depth', label: 'Shadow strength', hint: 'How dark the shadows of menus, dialogs and the preview frame are', min: 0, max: 150, step: 5, def: 100, unit: '%' }
] as const;

export type ShapeToken = (typeof SHAPE_TOKENS)[number]['name'];
export const SHAPE_NAMES: readonly ShapeToken[] = SHAPE_TOKENS.map((t) => t.name);

/** How a number shape token is written for a person: `1.25` of the corner scale is "125%". */
export function shapeText(token: (typeof SHAPE_TOKENS)[number], value: number): string {
	return 'percent' in token && token.percent ? `${Math.round(value * 100)}%` : `${value}${token.unit}`;
}

export type ThumbStyle = 'round' | 'bar';
export const THUMB_STYLES: readonly { id: ThumbStyle; label: string }[] = [
	{ id: 'round', label: 'Round' },
	{ id: 'bar', label: 'Bar' }
];

export type Density = 'compact' | 'default' | 'comfortable';

/** How tall the controls and the bars are. Spacing is the pixels of these, not a multiplier, because most of the
 *  chrome is laid out in whole pixels and a fraction would blur a border. It scales what is a token — buttons, chips,
 *  fields, tabs, menu rows, the title and status bars — and leaves type alone: text sizes are the scale in
 *  `kerf-tokens.css`, which `type-guard.test.ts` holds, and a control is always tall enough for the text it holds
 *  (`theme.test.ts`), and never shorter than the 24 px a pointer target must be; the status bar, whose buttons are 24 px, is never
 *  under 26 (`StatusBar` also keeps `min-height` for a thick line). Padding written as a literal inside a
 *  component does not follow it. */
export const DENSITIES: readonly { id: Density; label: string; hint: string; props: Readonly<Record<string, string>> }[] = [
	{
		id: 'compact',
		label: 'Compact',
		hint: 'Shorter buttons, fields, menu rows, tab strips and title bar. The smallest targets stay 24 px.',
		props: {
			'--control-xs': '20px',
			'--control-sm': '24px',
			'--control-md': '30px',
			'--control-lg': '36px',
			'--control-tool': '24px',
			'--row-h': '24px',
			'--tabstrip-h': '28px',
			'--titlebar-h': '32px',
			'--statusbar-h': '26px',
			'--toolbar-h': '40px'
		}
	},
	{
		id: 'default',
		label: 'Default',
		hint: 'As Kerf is drawn.',
		props: {
			'--control-xs': '22px',
			'--control-sm': '28px',
			'--control-md': '34px',
			'--control-lg': '40px',
			'--control-tool': '24px',
			'--row-h': '26px',
			'--tabstrip-h': '30px',
			'--titlebar-h': '36px',
			'--statusbar-h': '26px',
			'--toolbar-h': '44px'
		}
	},
	{
		id: 'comfortable',
		label: 'Comfortable',
		hint: 'Taller controls and bars: easier targets.',
		props: {
			'--control-xs': '24px',
			'--control-sm': '32px',
			'--control-md': '38px',
			'--control-lg': '44px',
			'--control-tool': '28px',
			'--row-h': '30px',
			'--tabstrip-h': '34px',
			'--titlebar-h': '40px',
			'--statusbar-h': '30px',
			'--toolbar-h': '48px'
		}
	}
];

export const isDensity = (v: unknown): v is Density => DENSITIES.some((d) => d.id === v);

/** The custom properties a density sets. */
export function densityProps(density: Density): Record<string, string> {
	return { ...(DENSITIES.find((d) => d.id === density) ?? DENSITIES[1]).props };
}

export type Shape = Record<ShapeToken, number> & {
	'slider-thumb-style': ThumbStyle;
	density: Density;
	'font-ui': UiFont;
	'font-mono': MonoFont;
};

export interface Theme {
	name: string;
	version: 1;
	scheme: Scheme;
	/** The built-in theme a copy was made from, which a colour can be put back to. Absent on a built-in. */
	base?: PresetId;
	colors: Record<ColorToken, string>;
	shape: Shape;
}

const SHAPE_DEFAULT = Object.fromEntries(SHAPE_TOKENS.map((t) => [t.name, t.def])) as Record<ShapeToken, number>;
const DEFAULT_SHAPE: Shape = {
	...SHAPE_DEFAULT,
	'slider-thumb-style': 'round',
	density: 'default',
	'font-ui': DEFAULT_UI_FONT,
	'font-mono': DEFAULT_MONO_FONT
};

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
			'shadow-color': '#000000',
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
			'blue-500': '#4d8dff',
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
			'clip-accent': '#f0c277',
			'drag-ghost': '#788cff',
			'frame-matte': '#000000',
			'diff-add': '#28c585',
			'diff-remove': '#f1543f',
			'diff-shift': '#f59020'
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
			'shadow-color': '#141e32',
			'border-base': '#000000',
			'scrollbar-thumb': '#c2c9d1',
			'scrollbar-thumb-hover': '#a9b2bd',
			'text-primary': '#14181e',
			'text-secondary': '#3f4956',
			'text-muted': '#5a6776',
			'text-disabled': '#76828f',
			'text-on-accent': '#1a1205',
			'text-inverted': '#ffffff',
			'text-on-video': '#ffffff',
			'kerf-200': '#7a4f0e',
			'kerf-300': '#8a5a10',
			'kerf-400': '#a96d11',
			'kerf-500': '#b8770f',
			'kerf-600': '#e29d2e',
			'kerf-700': '#eab14e',
			'agent-200': '#0b4a54',
			'agent-300': '#116b7b',
			'agent-400': '#1894a6',
			'agent-500': '#1a9fb0',
			'agent-600': '#22b4c4',
			'agent-700': '#3fcdd4',
			'agent-fg': '#04181c',
			'green-400': '#186d49',
			'green-500': '#1c9a68',
			'red-400': '#a62d23',
			'red-500': '#e4452f',
			'red-600': '#b32d1e',
			'blue-500': '#2f6ad8',
			'orange-400': '#8f4e09',
			'orange-500': '#c76f0a',
			'track-bg': '#eceff2',
			'track-video': '#3b6d8c',
			'track-video-edge': '#3f7ea3',
			'track-audio': '#2f6f56',
			'track-audio-edge': '#2f7d5e',
			'track-text': '#5d4180',
			'track-text-edge': '#7b56a0',
			waveform: '#a8ecca',
			'clip-accent': '#ffdf9e',
			'drag-ghost': '#4a5fd6',
			'frame-matte': '#000000',
			'diff-add': '#1c9a68',
			'diff-remove': '#e4452f',
			'diff-shift': '#c76f0a'
		},
		shape: {
			...DEFAULT_SHAPE,
			'border-alpha-subtle': 8,
			'border-alpha-default': 14,
			'border-alpha-strong': 22,
			'shadow-strength': 30
		}
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
			'shadow-color': '#000000',
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
			'blue-500': '#6aa8ff',
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
			'clip-accent': '#ffd280',
			'drag-ghost': '#9aa8ff',
			'frame-matte': '#000000',
			'diff-add': '#22d37a',
			'diff-remove': '#ff5c47',
			'diff-shift': '#ff9a1f'
		},
		shape: {
			...DEFAULT_SHAPE,
			'line-width': 1.5,
			'line-emphasis': 3,
			'playhead-width': 3,
			'slider-track': 6,
			'slider-thumb': 16,
			'border-alpha-subtle': 30,
			'border-alpha-default': 45,
			'border-alpha-strong': 70
		}
	}
};

export type DiffToken = 'diff-add' | 'diff-remove' | 'diff-shift';
const DIFF_FROM: Record<DiffToken, ColorToken> = { 'diff-add': 'green-500', 'diff-remove': 'red-500', 'diff-shift': 'orange-500' };

/** The review colours a theme that predates them drew with: its own success, error and warning hues. */
function legacyDiff(colors: Partial<Record<ColorToken, string>>): Record<DiffToken, string> {
	return {
		'diff-add': colors[DIFF_FROM['diff-add']] ?? '',
		'diff-remove': colors[DIFF_FROM['diff-remove']] ?? '',
		'diff-shift': colors[DIFF_FROM['diff-shift']] ?? ''
	};
}

/** Kerf Light as earlier builds shipped it, oldest first. A theme is stored as
 *  the colors it had when it was picked, so a user who chose Kerf Light keeps those
 *  colors after the preset itself is revised — here, after the contrast pass that
 *  darkened its clip bodies, amber and status hues. `upgradeStoredTheme` moves a
 *  stored theme whose colors are *exactly* one of these to the current preset and
 *  leaves everything else alone. Whenever Kerf Light's colors change again, the
 *  outgoing set goes on the end of this list. A token added after a set was
 *  superseded (`clip-accent`) carries the current value in it: a stored theme
 *  that lacks it is given Kerf Light's on parse (`colorsBase`, the preset its other
 *  colors are). */
const SUPERSEDED_LIGHT_SETS: readonly Omit<Record<ColorToken, string>, DiffToken>[] = [
	// As shipped up to v0.22.0.
	{
		'surface-void': '#e2e6ea',
		'surface-app': '#f2f4f6',
		'surface-panel': '#ffffff',
		'surface-raised': '#f6f8fa',
		'surface-inset': '#e9edf1',
		'surface-hover': '#e4e8ed',
		'surface-active': '#d3d9e0',
		input: '#ffffff',
		scrim: '#000000',
		'shadow-color': '#141e32',
		'border-base': '#000000',
		'scrollbar-thumb': '#c2c9d1',
		'scrollbar-thumb-hover': '#a9b2bd',
		'text-primary': '#14181e',
		'text-secondary': '#3f4956',
		'text-muted': '#5e6b7a',
		'text-disabled': '#97a3b2',
		'text-on-accent': '#1a1205',
		'text-inverted': '#ffffff',
		'text-on-video': '#ffffff',
		'kerf-200': '#7a4f0e',
		'kerf-300': '#9a6413',
		'kerf-400': '#b8770f',
		'kerf-500': '#d18f1e',
		'kerf-600': '#e29d2e',
		'kerf-700': '#eab14e',
		'agent-200': '#0b4a54',
		'agent-300': '#126f80',
		'agent-400': '#1894a6',
		'agent-500': '#1a9fb0',
		'agent-600': '#22b4c4',
		'agent-700': '#3fcdd4',
		'agent-fg': '#ffffff',
		'green-400': '#1f8f60',
		'green-500': '#22a874',
		'red-400': '#c8362a',
		'red-500': '#e0402c',
		'red-600': '#b32d1e',
		'blue-500': '#2f6ad8',
		'orange-400': '#c46b0c',
		'orange-500': '#e07f14',
		'track-bg': '#eceff2',
		'track-video': '#b7d2e4',
		'track-video-edge': '#3f7ea3',
		'track-audio': '#b8dccb',
		'track-audio-edge': '#2f7d5e',
		'track-text': '#d8c8e6',
		'track-text-edge': '#7b56a0',
		waveform: '#1f8f60',
		'clip-accent': '#ffdf9e',
		'drag-ghost': '#4a5fd6',
		'frame-matte': '#000000'
	},
	// The first contrast pass, before `text-disabled` was raised for idle toggles.
	{
		'surface-void': '#e2e6ea',
		'surface-app': '#f2f4f6',
		'surface-panel': '#ffffff',
		'surface-raised': '#f6f8fa',
		'surface-inset': '#e9edf1',
		'surface-hover': '#e4e8ed',
		'surface-active': '#d3d9e0',
		input: '#ffffff',
		scrim: '#000000',
		'shadow-color': '#141e32',
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
		'blue-500': '#2f6ad8',
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
		'clip-accent': '#ffdf9e',
		'drag-ghost': '#4a5fd6',
		'frame-matte': '#000000'
	},
	// As shipped in v0.24.0, before the tinted-surface contrast pass darkened the light
	// status hues the badges and chips draw as text.
	{
		'surface-void': '#e2e6ea',
		'surface-app': '#f2f4f6',
		'surface-panel': '#ffffff',
		'surface-raised': '#f6f8fa',
		'surface-inset': '#e9edf1',
		'surface-hover': '#e4e8ed',
		'surface-active': '#d3d9e0',
		input: '#ffffff',
		scrim: '#000000',
		'shadow-color': '#141e32',
		'border-base': '#000000',
		'scrollbar-thumb': '#c2c9d1',
		'scrollbar-thumb-hover': '#a9b2bd',
		'text-primary': '#14181e',
		'text-secondary': '#3f4956',
		'text-muted': '#5a6776',
		'text-disabled': '#76828f',
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
		'blue-500': '#2f6ad8',
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
		'clip-accent': '#ffdf9e',
		'drag-ghost': '#4a5fd6',
		'frame-matte': '#000000'
	},
	// Before the amber chips' text (`kerf-300`) was darkened to read 4.5:1 on their own wash.
	{
		'surface-void': '#e2e6ea',
		'surface-app': '#f2f4f6',
		'surface-panel': '#ffffff',
		'surface-raised': '#f6f8fa',
		'surface-inset': '#e9edf1',
		'surface-hover': '#e4e8ed',
		'surface-active': '#d3d9e0',
		input: '#ffffff',
		scrim: '#000000',
		'shadow-color': '#141e32',
		'border-base': '#000000',
		'scrollbar-thumb': '#c2c9d1',
		'scrollbar-thumb-hover': '#a9b2bd',
		'text-primary': '#14181e',
		'text-secondary': '#3f4956',
		'text-muted': '#5a6776',
		'text-disabled': '#76828f',
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
		'agent-300': '#116b7b',
		'agent-400': '#1894a6',
		'agent-500': '#1a9fb0',
		'agent-600': '#22b4c4',
		'agent-700': '#3fcdd4',
		'agent-fg': '#04181c',
		'green-400': '#186d49',
		'green-500': '#1c9a68',
		'red-400': '#a62d23',
		'red-500': '#e4452f',
		'red-600': '#b32d1e',
		'blue-500': '#2f6ad8',
		'orange-400': '#8f4e09',
		'orange-500': '#c76f0a',
		'track-bg': '#eceff2',
		'track-video': '#3b6d8c',
		'track-video-edge': '#3f7ea3',
		'track-audio': '#2f6f56',
		'track-audio-edge': '#2f7d5e',
		'track-text': '#5d4180',
		'track-text-edge': '#7b56a0',
		waveform: '#a8ecca',
		'clip-accent': '#ffdf9e',
		'drag-ghost': '#4a5fd6',
		'frame-matte': '#000000'
	}
];

/** The same sets as full themes: a set from before the review colours existed drew them in its own status
 *  hues, so that is what they are here. */
export const SUPERSEDED_KERF_LIGHT: readonly Record<ColorToken, string>[] = SUPERSEDED_LIGHT_SETS.map((set) => ({
	...set,
	...legacyDiff(set)
}));

export const PRESET_IDS = Object.keys(PRESETS) as PresetId[];

const HEX = /^#[0-9a-f]{6}$/i;

/** `#rrggbb` in lower case, or null. Only the six-digit form is a colour: `#fff`, `rgb()` and names are not. */
export function normalizeHex(v: unknown): string | null {
	return typeof v === 'string' && HEX.test(v) ? v.toLowerCase() : null;
}

/** A colour as a person types it into a field: `#e29d2e`, `E29D2E`, `#fa0` (each digit doubled). Lower case `#rrggbb`, or
 *  null while it is not (yet) one. A file is stricter: `normalizeHex` takes only the six-digit form. */
export function parseTypedHex(text: string): string | null {
	const m = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(text.trim());
	if (!m) return null;
	const digits = m[1].length === 3 ? [...m[1]].map((d) => d + d).join('') : m[1];
	return `#${digits.toLowerCase()}`;
}

function shapeEqual(a: Shape, b: Shape): boolean {
	return (
		SHAPE_NAMES.every((t) => a[t] === b[t]) &&
		a['slider-thumb-style'] === b['slider-thumb-style'] &&
		a.density === b.density &&
		a['font-ui'] === b['font-ui'] &&
		a['font-mono'] === b['font-mono']
	);
}

/** A shape value pulled into its token's range and onto its step. */
export function clampShape(token: ShapeToken, v: number): number {
	const t = SHAPE_TOKENS.find((x) => x.name === token)!;
	const stepped = Math.round(v / t.step) * t.step;
	// Steps like 0.25 are exact in binary, but 0.1 would not be: keep the float noise out of a stored value.
	return Math.min(t.max, Math.max(t.min, Math.round(stepped * 1e6) / 1e6));
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

/** The preset a copy of a theme is put back to: the one it was made from, else the scheme's own. */
export function baseFor(theme: Pick<Theme, 'base' | 'scheme'>): PresetId {
	return theme.base ?? (theme.scheme === 'light' ? 'kerf-light' : 'kerf-dark');
}

/** The shape a theme with these colors takes for values it does not carry: that of the preset
 *  whose colors they are exactly (a High contrast saved before the border and shadow values
 *  existed is still High contrast), else the scheme's own. */
function shapeBase(scheme: Scheme, colors: Record<ColorToken, string>): Shape {
	const same = PRESET_IDS.find(
		(id) => PRESETS[id].scheme === scheme && COLOR_TOKENS.every((t) => PRESETS[id].colors[t] === colors[t].toLowerCase())
	);
	return PRESETS[same ?? (scheme === 'light' ? 'kerf-light' : 'kerf-dark')].shape;
}

/** The preset a theme with only some of its colors was taken from: the first of its scheme whose
 *  colors are exactly the ones given (a High contrast saved before `clip-accent` existed has every
 *  other token of High contrast), else the scheme's own. What it lacks is read from there. */
function colorsBase(scheme: Scheme, given: Partial<Record<ColorToken, string>>): Record<ColorToken, string> {
	const same = PRESET_IDS.find(
		(id) => PRESETS[id].scheme === scheme && COLOR_TOKENS.every((t) => given[t] === undefined || PRESETS[id].colors[t] === given[t])
	);
	return PRESETS[same ?? (scheme === 'light' ? 'kerf-light' : 'kerf-dark')].colors;
}

/** The longest a theme's name is kept. */
export const NAME_MAX = 48;

/** A name as a person typed it, made safe to show and to write into a file name: control, zero-width and
 *  bidirectional-override characters gone (a name must not be able to reorder what is drawn around it), runs of
 *  white space one space, at most `NAME_MAX` characters. Empty when nothing is left. */
export function cleanName(v: unknown): string {
	if (typeof v !== 'string') return '';
	const flat = v.replace(/[\u0000-\u001f\u007f-\u009f\u200b-\u200f\u2028-\u202e\u2066-\u2069\ufeff]/g, ' ').replace(/\s+/g, ' ').trim();
	return Array.from(flat).slice(0, NAME_MAX).join('').trim();
}

/** Every shape field of a stored or imported theme, one at a time: a number is clamped and put on its step, a choice
 *  must be one of the known ones, and anything else (a missing field, a string where a number belongs, an unknown key)
 *  keeps what `base` has. */
function parseShape(raw: unknown, base: Shape): Shape {
	const gs = typeof raw === 'object' && raw !== null && !Array.isArray(raw) ? (raw as Record<string, unknown>) : {};
	const shape = { ...base };
	for (const t of SHAPE_NAMES) {
		const v = gs[t];
		if (typeof v === 'number' && Number.isFinite(v)) shape[t] = clampShape(t, v);
	}
	const ts = gs['slider-thumb-style'];
	if (ts === 'round' || ts === 'bar') shape['slider-thumb-style'] = ts;
	if (isDensity(gs.density)) shape.density = gs.density;
	if (isUiFont(gs['font-ui'])) shape['font-ui'] = gs['font-ui'];
	if (isMonoFont(gs['font-mono'])) shape['font-mono'] = gs['font-mono'];
	return shape;
}

/** A theme from stored or imported JSON, or `null` when it is not one.
 *  Read field by field: unknown keys are dropped, a colour must be exactly `#rrggbb` (anything else makes the
 *  whole theme unreadable rather than guessed at), numbers are clamped, and missing values are filled from the
 *  preset the colors it does carry are exactly (else the scheme's own), so a file from a build with fewer
 *  tokens still applies and stays the preset it was. The review colours a file written before they existed lacks
 *  are the theme's own success, error and warning hues. */
export function parseTheme(raw: unknown): Theme | null {
	if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) return null;
	const r = raw as Record<string, unknown>;
	if (r.version !== 1) return null;
	if (r.scheme !== 'dark' && r.scheme !== 'light') return null;
	const name = cleanName(r.name);
	if (name === '') return null;
	if (typeof r.colors !== 'object' || r.colors === null || Array.isArray(r.colors)) return null;
	const stored = r.colors as Record<string, unknown>;
	const given: Partial<Record<ColorToken, string>> = {};
	for (const t of COLOR_TOKENS) {
		const v = stored[t];
		if (v === undefined) continue;
		const hex = normalizeHex(v);
		if (hex === null) return null;
		given[t] = hex;
	}
	const base = colorsBase(r.scheme, given);
	const colors = {} as Record<ColorToken, string>;
	for (const t of COLOR_TOKENS) colors[t] = given[t] ?? base[t];
	for (const [diff, hue] of Object.entries(DIFF_FROM) as [DiffToken, ColorToken][]) if (given[diff] === undefined) colors[diff] = colors[hue];
	const out: Theme = { name, version: 1, scheme: r.scheme, colors, shape: parseShape(r.shape, shapeBase(r.scheme, colors)) };
	if (typeof r.base === 'string' && (PRESET_IDS as string[]).includes(r.base)) out.base = r.base as PresetId;
	return out;
}

/** A stored theme, brought up to date. One whose colors are exactly an earlier
 *  Kerf Light (`SUPERSEDED_KERF_LIGHT`) was that preset picked and never edited, and
 *  takes the current preset's colors — keeping the name and shape it was saved
 *  with. Anything else, a theme the user edited or imported included, is returned
 *  as it is. Run on what is read back from the settings, never on an import. */
export function upgradeStoredTheme(theme: Theme): Theme {
	if (theme.scheme !== 'light') return theme;
	const colors = theme.colors;
	const wasKerfLight = SUPERSEDED_KERF_LIGHT.some((old) => COLOR_TOKENS.every((t) => old[t] === colors[t].toLowerCase()));
	return wasKerfLight ? { ...theme, colors: { ...PRESETS['kerf-light'].colors } } : theme;
}

/** The custom properties a shape sets. The thumb is a circle (or square at
 *  size) when round, and a narrow upright bar when not; the stylesheet
 *  defaults are the round case. */
export function shapeProps(shape: Shape): Record<string, string> {
	const size = shape['slider-thumb'];
	const bar = shape['slider-thumb-style'] === 'bar';
	const props: Record<string, string> = {};
	for (const t of SHAPE_TOKENS) props[`--${t.name}`] = `${shape[t.name]}${t.unit}`;
	props['--slider-thumb-w'] = `${bar ? Math.max(4, Math.round(size * 0.4)) : size}px`;
	props['--slider-thumb-radius'] = bar ? '2px' : '999px';
	return props;
}

/** Every custom property a theme writes on the root: colors, shape, density and typefaces. */
export function themeProps(theme: Theme): Record<string, string> {
	const props: Record<string, string> = {};
	for (const t of COLOR_TOKENS) props[`--${t}`] = theme.colors[t];
	return {
		...props,
		...shapeProps(theme.shape),
		...densityProps(theme.shape.density),
		...fontProps(theme.shape['font-ui'], theme.shape['font-mono'])
	};
}

/** A theme as inline CSS for one element: every custom property it sets, and its `color-scheme`. What is inside that
 *  element is drawn in the theme whatever the rest of the page is (the editor stays in the theme it was opened on
 *  while the page shows the draft). */
export function themeStyle(theme: Theme): string {
	return `${Object.entries(themeProps(theme))
		.map(([k, v]) => `${k}:${v}`)
		.join(';')};color-scheme:${theme.scheme}`;
}

/** The scheme a set of colours looks like: Light when the panel is light, else Dark. The scheme sets what the browser draws
 *  itself (scrollbars, form controls, the toast library), so one that disagrees with the colours shows. */
export function suggestScheme(colors: Pick<Record<ColorToken, string>, 'surface-panel'>): Scheme {
	return relativeLuminance(colors['surface-panel']) > 0.4 ? 'light' : 'dark';
}

/** A theme as the file carries it: only the fields a theme has, so an id or a stray key never leaves with it. */
export function themeFields(theme: Theme): Theme {
	return {
		name: theme.name,
		version: 1,
		scheme: theme.scheme,
		...(theme.base ? { base: theme.base } : {}),
		colors: { ...theme.colors },
		shape: { ...theme.shape }
	};
}

export function themeJson(theme: Theme): string {
	return JSON.stringify(themeFields(theme), null, 2) + '\n';
}

/** Put a theme into force: the tokens as inline properties on the root (so
 *  they beat the stylesheet), the `dark` class and `color-scheme` for anything
 *  that keys off them, and the pre-hydration background `app.html` painted. */
export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement) {
	for (const [k, v] of Object.entries(themeProps(theme))) root.style.setProperty(k, v);
	root.classList.toggle('dark', theme.scheme === 'dark');
	root.style.colorScheme = theme.scheme;
	root.style.background = theme.colors['surface-app'];
}
