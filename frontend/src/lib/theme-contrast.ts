// The contrast pairs the UI draws, as data: which token's colour is set on which, and how far apart
// the two must be. `theme-guard.test.ts` holds every preset to them; the theme editor shows the same
// list against a theme being edited, so a user sees that text has become unreadable (a warning, never a
// block). A preset that fails a pair gets its *values* fixed in `theme.ts`, not the pair.
//
// WCAG 2.x ratios between the opaque tokens of a theme, in two tiers:
//
//   READING (4.5:1)    The words people read: text-primary and text-secondary on every surface they
//     sit on, text-muted on the resting surfaces, the label colour on each solid fill.
//   SECONDARY (3:1)    What is not body text: text-muted on the hover / active states, accent and
//     status hues as text or icon colour on a panel, strokes and marks that carry meaning (clip edges,
//     the playhead, the waveform, diff markers), and the label of a disabled control.
//
// Fills the stylesheet builds with `color-mix` are resolved per theme: a tint is its hue at 10-16 %
// over whatever it sits on (`TINTED`), and a few composites are mixes of two tokens (`compositeFindings`).
//
// Not checked, on purpose: text-disabled on a *disabled* control (inactive components are exempt; it
// is still held to 3:1 on the resting surfaces so a greyed label can be found), translucent hairlines
// (they separate rather than identify) and text-on-video over real footage (the picture decides).

import { NAME_CHIP_PCT } from './clip-label';
import { contrastRatio, mixSrgb } from './contrast';
import { tokenLabel, type ColorToken } from './theme';
import { GENERATED_TITLE_FILL, SVG_TITLE_FILL } from './titles';

type Colors = Record<ColorToken, string>;

export const RESTING: ColorToken[] = ['surface-void', 'surface-app', 'surface-panel', 'surface-raised', 'surface-inset', 'input', 'track-bg'];
export const STATES: ColorToken[] = ['surface-hover', 'surface-active'];
export const PANELS: ColorToken[] = ['surface-app', 'surface-panel', 'surface-raised'];
export const CLIP_BODIES: ColorToken[] = ['track-video', 'track-audio', 'track-text'];

export const READING = 4.5;
export const SECONDARY = 3;

export interface Pair {
	fg: ColorToken;
	bg: ColorToken;
	min: number;
	what: string;
}

export function cross(fgs: ColorToken[], bgs: ColorToken[], min: number, what: string): Pair[] {
	return fgs.flatMap((fg) => bgs.map((bg) => ({ fg, bg, min, what })));
}

export const PAIRS: Pair[] = [
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
	// The background-work lines — a running proxy and analysis in amber, a failure in red, a finished step in green — are 12 px
	// text on the bin row (raised), the Inspector's strip (raised) and the first-run card's block (inset), and on the selected row.
	...cross(
		['kerf-300', 'red-400', 'green-400'],
		[...PANELS, 'surface-inset', 'surface-hover'],
		READING,
		'running / failed / done work lines on the bin row, the Inspector strip and the first-run card'
	),
	...cross(['text-disabled'], RESTING, SECONDARY, "a disabled control's label stays findable"),
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
	...cross(['waveform'], ['track-audio'], SECONDARY, 'waveform on its clip'),
	// What is drawn on a clip beside its body (`ClipOverlays`): the volume line, the fade handles and
	// ramps, the keyframe diamonds. Kerf Light's `kerf-300` is a dark amber for text on a white
	// panel, which on its dark clip bodies is 1.0:1; `clip-accent` is the light one.
	...cross(['clip-accent'], CLIP_BODIES, SECONDARY, 'volume line, fade handles and keyframes on the clip body'),
	// How a proposal marks what it adds, removes and changes: outlines on the lane and chips in the review card.
	...cross(['diff-add', 'diff-remove', 'diff-shift'], ['track-bg', ...PANELS], SECONDARY, 'review diff marker on the lane and the panels')
];

/** A translucent token of the stylesheet — `color-mix(in srgb, var(--hue) N%, transparent)` — as its
 *  hue and weight (0-1). `css` names the property it is declared as in `kerf-tokens.css`, which a
 *  test holds to the numbers here. */
export interface Tint {
	css?: string;
	hue: ColorToken;
	weight: number;
}

export interface TintPair {
	fg: ColorToken;
	tint: Tint;
	min: number;
	what: string;
	/** What it sits on; the three panels and the hover surface unless it says otherwise. */
	on?: ColorToken[];
}

export const SUCCESS_SURFACE: Tint = { css: 'success-surface', hue: 'green-500', weight: 0.12 };
export const WARNING_SURFACE: Tint = { css: 'warning-surface', hue: 'orange-500', weight: 0.12 };
export const DANGER_SURFACE: Tint = { css: 'danger-surface', hue: 'red-500', weight: 0.12 };
export const AGENT_SURFACE: Tint = { css: 'agent-surface', hue: 'agent-500', weight: 0.1 };
export const SELECTION_FILL: Tint = { css: 'selection-fill', hue: 'kerf-500', weight: 0.16 };
export const DIFF_REMOVE_SURFACE: Tint = { css: 'diff-remove-surface', hue: 'diff-remove', weight: 0.14 };

/** What a tint is once it sits on a surface. */
export const SURFACES_UNDER_TINTS: ColorToken[] = [...PANELS, 'surface-inset', 'surface-hover'];

export const TINTED: TintPair[] = [
	{ fg: 'green-400', tint: SUCCESS_SURFACE, min: READING, what: 'success badge, finished analysis chip' },
	{ fg: 'orange-400', tint: WARNING_SURFACE, min: READING, what: 'warning badge, "links off" tag, update warning' },
	{ fg: 'red-400', tint: DANGER_SURFACE, min: READING, what: 'danger badge, failed analysis chip' },
	{ fg: 'agent-300', tint: AGENT_SURFACE, min: READING, what: 'agent badge' },
	{ fg: 'kerf-200', tint: SELECTION_FILL, min: READING, what: "amber badge (a running proxy or analysis, the preview's file name)" },
	// The timeline toolbar's lit toggles (snap, ripple, fades, heights, overview, delivery frame) draw
	// kerf-300 on the selection wash; the toolbar is a panel, so the hover surface is not under it.
	{ fg: 'kerf-300', tint: SELECTION_FILL, min: READING, what: 'lit toggle in the timeline toolbar', on: PANELS },
	// Written inline in the dialogs' error blocks: `color-mix(in srgb,var(--red-600) 14%,transparent)`.
	{ fg: 'red-400', tint: { hue: 'red-600', weight: 0.14 }, min: READING, what: 'error block in a dialog' },
	// Toasts: the kind's 12 % wash over the toast's own surface (`routes/layout.css`), its line, its
	// description, and its glyph and edge in the `-400` shade.
	{ fg: 'text-primary', tint: SUCCESS_SURFACE, min: READING, what: 'success toast', on: ['surface-raised'] },
	{ fg: 'text-secondary', tint: SUCCESS_SURFACE, min: READING, what: 'success toast description', on: ['surface-raised'] },
	{ fg: 'text-secondary', tint: DANGER_SURFACE, min: READING, what: 'error toast description', on: ['surface-raised'] },
	{ fg: 'text-secondary', tint: WARNING_SURFACE, min: READING, what: 'warning toast description', on: ['surface-raised'] },
	{ fg: 'text-primary', tint: DANGER_SURFACE, min: READING, what: 'agent connection banner' },
	{ fg: 'text-primary', tint: WARNING_SURFACE, min: READING, what: 'keyboard conflict prompt' },
	{ fg: 'text-secondary', tint: DIFF_REMOVE_SURFACE, min: READING, what: 'stale proposal note' },
	{ fg: 'text-secondary', tint: AGENT_SURFACE, min: READING, what: 'agent panel header and active chip' }
];

/** The pairs a set of colors does not meet, each phrased with its numbers. */
export function failures(colors: Colors, pairs: Pair[]): string[] {
	return pairs.flatMap((p) => {
		const ratio = contrastRatio(colors[p.fg], colors[p.bg]);
		return ratio >= p.min
			? []
			: [`${p.fg} ${colors[p.fg]} on ${p.bg} ${colors[p.bg]}: ${ratio.toFixed(2)}:1, needs ${p.min}:1 (${p.what})`];
	});
}

export function tintFailures(colors: Colors, pairs: TintPair[]): string[] {
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

/** `color-mix(in srgb,var(--a) P%,var(--b))` resolved against a theme's colors. */
export function resolveMix(expr: string, colors: Colors): string {
	const m = /^color-mix\(in srgb,\s*var\(--([a-z0-9-]+)\)\s+([\d.]+)%,\s*var\(--([a-z0-9-]+)\)\)$/.exec(expr);
	if (!m) throw new Error(`not a two-token srgb color-mix: ${expr}`);
	return mixSrgb(colors[m[1] as ColorToken], colors[m[3] as ColorToken], Number(m[2]) / 100);
}

/** One pair a theme does not meet, with the tokens that could be changed to meet it. */
export interface Finding {
	fg: ColorToken;
	/** The token it sits on, or the first of the mix it sits on (see `tokens`). */
	bg: ColorToken;
	ratio: number;
	min: number;
	what: string;
	/** Every token the pair depends on: the text, the surface and any tint's hue. */
	tokens: ColorToken[];
	/** Plain words: "Muted text on Panel". */
	text: string;
}

function finding(fg: ColorToken, bgLabel: string, bg: ColorToken, tokens: ColorToken[], ratio: number, min: number, what: string): Finding {
	return { fg, bg, ratio, min, what, tokens: [...new Set(tokens)], text: `${tokenLabel(fg)} on ${bgLabel}` };
}

/** The mixes that carry a label: a generated caption's block, an animated title's block, and a clip's
 *  name chip over its brightest backdrop. Each is held to READING for `text-on-video`. */
function compositeFindings(colors: Colors): Finding[] {
	const out: Finding[] = [];
	const check = (fill: string, tokens: ColorToken[], what: string, bgLabel: string) => {
		const ratio = contrastRatio(colors['text-on-video'], fill);
		if (ratio < READING) out.push(finding('text-on-video', bgLabel, tokens[0], ['text-on-video', ...tokens], ratio, READING, what));
	};
	check(resolveMix(GENERATED_TITLE_FILL, colors), ['track-text', 'scrim'], "a generated caption's block", 'a generated caption block');
	check(resolveMix(SVG_TITLE_FILL, colors), ['track-text', 'track-video'], "an animated title's block", 'an animated title block');
	for (const under of ['waveform', 'track-audio', 'track-video'] as ColorToken[]) {
		const chip = mixSrgb(colors['scrim'], colors[under], NAME_CHIP_PCT / 100);
		check(chip, ['scrim', under], "a clip's name chip", `a clip name chip over ${tokenLabel(under)}`);
	}
	return out;
}

/** Every pair the theme does not meet, in the order the list is written. */
export function findings(colors: Colors): Finding[] {
	const out: Finding[] = [];
	for (const p of PAIRS) {
		const ratio = contrastRatio(colors[p.fg], colors[p.bg]);
		if (ratio < p.min) out.push(finding(p.fg, tokenLabel(p.bg), p.bg, [p.fg, p.bg], ratio, p.min, p.what));
	}
	for (const p of TINTED) {
		for (const surface of p.on ?? SURFACES_UNDER_TINTS) {
			const fill = mixSrgb(colors[p.tint.hue], colors[surface], p.tint.weight);
			const ratio = contrastRatio(colors[p.fg], fill);
			if (ratio < p.min)
				out.push(finding(p.fg, `${tokenLabel(p.tint.hue)} tint on ${tokenLabel(surface)}`, p.tint.hue, [p.fg, p.tint.hue, surface], ratio, p.min, p.what));
		}
	}
	out.push(...compositeFindings(colors));
	return out;
}

/** The findings that involve each token, worst first. */
export function findingsByToken(list: readonly Finding[]): Map<ColorToken, Finding[]> {
	const by = new Map<ColorToken, Finding[]>();
	for (const f of list) for (const t of f.tokens) by.set(t, [...(by.get(t) ?? []), f]);
	for (const [t, fs] of by) by.set(t, fs.sort((a, b) => a.ratio / a.min - b.ratio / b.min));
	return by;
}

/** One line of the summary: a text colour and what it is for, with how many of its surfaces fail. */
export interface Problem {
	fg: ColorToken;
	what: string;
	worst: Finding;
	count: number;
}

/** The findings folded by the colour that is read (`fg`) and what for: a text colour that fails on
 *  seven surfaces is one line, with the worst of them named. */
export function problems(list: readonly Finding[]): Problem[] {
	const by = new Map<string, Problem>();
	for (const f of list) {
		const key = `${f.fg}|${f.what}`;
		const p = by.get(key);
		if (!p) by.set(key, { fg: f.fg, what: f.what, worst: f, count: 1 });
		else {
			p.count++;
			if (f.ratio / f.min < p.worst.ratio / p.worst.min) p.worst = f;
		}
	}
	return [...by.values()].sort((a, b) => a.worst.ratio / a.worst.min - b.worst.ratio / b.worst.min);
}

/** "Muted text on Panel: 3.2:1, needs 4.5:1". */
export function describeFinding(f: Pick<Finding, 'text' | 'ratio' | 'min'>): string {
	return `${f.text}: ${f.ratio.toFixed(1)}:1, needs ${f.min}:1`;
}
