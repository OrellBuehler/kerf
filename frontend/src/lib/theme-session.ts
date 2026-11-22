// One sitting in the theme editor, as data. The theme on screen is a draft: every change is applied at once
// (the page shows it) but written nowhere until Save, so Cancel puts back what was there. Changes go on an undo
// stack; the many `input` events of a colour picker being dragged, or of a name being typed, are one step.
// Pure: `theme-book.svelte.ts` holds the session and applies the draft.

import { PRESETS, baseFor, clampShape, isDensity, normalizeHex, type ColorToken, type Density, type Scheme, type Shape, type ShapeToken, type Theme, type ThumbStyle } from './theme';
import { isMonoFont, isUiFont, type MonoFont, type UiFont } from './theme-fonts';

export interface Session {
	/** The user theme being edited. */
	id: string;
	/** As it was when the sitting began: what Cancel returns to, and what "changed" is measured against. */
	saved: Theme;
	/** What is on screen. */
	draft: Theme;
	past: Theme[];
	future: Theme[];
	/** The kind of change still going on (`color:kerf-500`): another of the same kind joins the step instead of starting one. */
	open: string | null;
}

/** How many steps the stack keeps. */
export const HISTORY = 100;

export function begin(id: string, theme: Theme): Session {
	return { id, saved: theme, draft: theme, past: [], future: [], open: null };
}

/** Whether two themes look the same: name, scheme, base, every colour and every shape value. */
export function sameTheme(a: Theme, b: Theme): boolean {
	return (
		a.name === b.name &&
		a.scheme === b.scheme &&
		a.base === b.base &&
		(Object.keys(a.colors) as ColorToken[]).every((t) => a.colors[t] === b.colors[t]) &&
		(Object.keys(a.shape) as (keyof Shape)[]).every((k) => a.shape[k] === b.shape[k])
	);
}

export const dirty = (s: Session): boolean => !sameTheme(s.draft, s.saved);
export const canUndo = (s: Session): boolean => s.past.length > 0;
export const canRedo = (s: Session): boolean => s.future.length > 0;

/** Make `next` the draft. With a `key`, a change of the same kind as the one before (and no `endEdit` between) replaces it
 *  rather than adding a step. A change that changes nothing is not one. */
export function change(s: Session, next: Theme, key: string | null = null): Session {
	if (sameTheme(next, s.draft)) return s;
	if (key !== null && key === s.open) return { ...s, draft: next, future: [] };
	return { ...s, draft: next, past: [...s.past, s.draft].slice(-HISTORY), future: [], open: key };
}

/** The change that was going on is over (the picker closed, the field lost focus): the next one is a step of its own. */
export function endEdit(s: Session): Session {
	return s.open === null ? s : { ...s, open: null };
}

export function undo(s: Session): Session {
	if (s.past.length === 0) return s;
	const prev = s.past[s.past.length - 1];
	return { ...s, draft: prev, past: s.past.slice(0, -1), future: [s.draft, ...s.future], open: null };
}

export function redo(s: Session): Session {
	if (s.future.length === 0) return s;
	return { ...s, draft: s.future[0], past: [...s.past, s.draft].slice(-HISTORY), future: s.future.slice(1), open: null };
}

// ---- the edits ------------------------------------------------------------------------------------

/** A colour, if what was typed or picked is one; anything else changes nothing. */
export function setColor(s: Session, token: ColorToken, value: string): Session {
	const hex = normalizeHex(value);
	if (hex === null || s.draft.colors[token] === hex) return s;
	return change(s, { ...s.draft, colors: { ...s.draft.colors, [token]: hex } }, `color:${token}`);
}

export function setShape(s: Session, token: ShapeToken, value: number): Session {
	if (!Number.isFinite(value)) return s;
	return change(s, { ...s.draft, shape: { ...s.draft.shape, [token]: clampShape(token, value) } }, `shape:${token}`);
}

type Choice = { density: Density; 'font-ui': UiFont; 'font-mono': MonoFont; 'slider-thumb-style': ThumbStyle };

const KNOWN: { [K in keyof Choice]: (v: unknown) => boolean } = {
	density: isDensity,
	'font-ui': isUiFont,
	'font-mono': isMonoFont,
	'slider-thumb-style': (v) => v === 'round' || v === 'bar'
};

/** A choice (density, a typeface, the thumb): each is a step of its own. A value that is not one of the known ones changes nothing. */
export function setChoice<K extends keyof Choice>(s: Session, field: K, value: Choice[K]): Session {
	if (!KNOWN[field](value)) return s;
	return change(s, { ...s.draft, shape: { ...s.draft.shape, [field]: value } });
}

/** The name as typed (cleaned when it is saved, so a space typed at the end is not eaten mid-word). */
export function setName(s: Session, name: string): Session {
	return change(s, { ...s.draft, name }, 'name');
}

export function setScheme(s: Session, scheme: Scheme): Session {
	return change(s, { ...s.draft, scheme });
}

// ---- putting back ---------------------------------------------------------------------------------

/** The built-in a copy's colours and shape are put back to. */
export const baseTheme = (theme: Pick<Theme, 'base' | 'scheme'>): Theme => PRESETS[baseFor(theme)];

/** The colour a token goes back to: the built-in's this theme was copied from. */
export function colorDefault(theme: Theme, token: ColorToken): string {
	return baseTheme(theme).colors[token];
}

export const colorChanged = (theme: Theme, token: ColorToken): boolean => theme.colors[token] !== colorDefault(theme, token);

export function shapeDefault<K extends keyof Shape>(theme: Theme, field: K): Shape[K] {
	return baseTheme(theme).shape[field];
}

export const shapeChanged = (theme: Theme, field: keyof Shape): boolean => theme.shape[field] !== shapeDefault(theme, field);

export function resetColor(s: Session, token: ColorToken): Session {
	return setColorNow(s, token, colorDefault(s.draft, token));
}

function setColorNow(s: Session, token: ColorToken, hex: string): Session {
	if (s.draft.colors[token] === hex) return s;
	return change(endEdit(s), { ...s.draft, colors: { ...s.draft.colors, [token]: hex } });
}

export function resetShape(s: Session, field: keyof Shape): Session {
	const to = shapeDefault(s.draft, field);
	if (s.draft.shape[field] === to) return s;
	return change(endEdit(s), { ...s.draft, shape: { ...s.draft.shape, [field]: to } });
}

/** Every colour and shape value of the draft back to what it was when the sitting began, as one step that can be undone. */
export function revertAll(s: Session): Session {
	return change(endEdit(s), s.saved);
}
