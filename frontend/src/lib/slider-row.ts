/* What a slider row does with a number — pure, so the Inspector's rows, the dialogs' and the
 * tests agree on it, and the component only wires events. A row is a label, a slider, a
 * number field and a slot for the keyframe button; the value it edits is in the property's
 * own units, and what is *shown* is that times `scale` with `digits` decimals and a `unit`
 * (a fraction shown as a percentage, an angle in degrees). */

import type { SliderKey } from './slider-gesture';

export interface RowSpec {
	/** The slider's travel. */
	min: number;
	max: number;
	/** What a drag and a key press snap to. A typed entry is not held to it (1.1× on a 0.25 step). */
	step: number;
	/** Where the property rests: drawn as a tick on the travel, and where a double-click puts it back.
	 *  Omitted, a travel that spans zero rests at zero and any other has no rest to name. */
	neutral?: number | null;
	/** The fill grows from the rest value instead of from the left: for what goes both ways.
	 *  Omitted, a travel that spans zero is bipolar. */
	bipolar?: boolean;
	/** Shown = value × scale. */
	scale?: number;
	/** Decimals shown, and the precision of a typed entry. */
	digits?: number;
	unit?: string;
	/** Typed entries may go past the travel, up to here (a fade as long as its clip, on a 5 s slider). */
	limit?: { min?: number; max?: number };
}

/** Width of a row's number column, px: `10.00s` and `-1.00×` in the row's mono type, with room for `100.00s`. */
export const NUMBER_COL = 60;

/** The units rows are shown in, to spread into a spec: a fraction as a percentage, an angle, a rate, a time. */
export const PERCENT = { scale: 100, digits: 0, unit: '%' } as const;
export const DEGREES = { digits: 0, unit: '°' } as const;
export const TIMES = { digits: 2, unit: '×' } as const;
export const SECONDS = { digits: 2, unit: 's' } as const;

/** What an edit was on when it began: the scope, the value it started from and the row's spec,
 *  which depends on the scope (a fade's limit is its clip's length). */
export interface Began<S> {
	scope: S;
	value: number;
	spec: RowSpec;
}

/** What an edit was on when it began, held until it ends, so a write lands where the edit was
 *  started even when the selection has moved on in between: a typed entry that is written by the
 *  very click that selects another clip, a run of arrow keys written when focus leaves. The entry
 *  is parsed with the spec it began with, not the one the new selection has. */
export class Pending<S> {
	#began: Began<S> | null = null;

	/** Note what the edit is on, unless one is already under way. */
	begin(scope: S, value: number, spec: RowSpec): void {
		this.#began ??= { scope, value, spec };
	}

	/** What the edit began on, or `now` when it did not begin anywhere (a double-click); ends it. */
	end(now: Began<S>): Began<S> {
		const began = this.#began ?? now;
		this.#began = null;
		return began;
	}

	/** The edit was given up. */
	drop(): void {
		this.#began = null;
	}
}

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** Float noise off a computed value: 0.30000000000000004 is 0.3. */
const clean = (v: number) => Number(v.toPrecision(12));

const span = (s: RowSpec) => s.max - s.min;

/** A value's place on the travel, `0..1`. */
export function toPos(v: number, s: RowSpec): number {
	return span(s) > 0 ? clamp((v - s.min) / span(s), 0, 1) : 0;
}

/** The value a place on the travel means, snapped to the step and held to the travel. */
export function fromPos(p: number, s: RowSpec): number {
	return snap(s.min + clamp(p, 0, 1) * span(s), s);
}

/** `v` on a grid of `grid` that starts at the minimum, inside the travel. */
function onGrid(v: number, s: RowSpec, grid: number): number {
	const stepped = grid > 0 ? s.min + Math.round((v - s.min) / grid) * grid : v;
	return clean(clamp(stepped, s.min, s.max));
}

/** `v` on the step grid, inside the travel. */
export const snap = (v: number, s: RowSpec): number => onGrid(v, s, s.step);

export const isBipolar = (s: RowSpec): boolean => s.bipolar ?? (s.min < 0 && s.max > 0);

/** The rest value, or `null` when the property has none to name. */
export function neutralOf(s: RowSpec): number | null {
	if (s.neutral !== undefined) return s.neutral;
	return s.min < 0 && s.max > 0 ? 0 : null;
}

/** Where the filled part of the travel starts, `0..1`: at the rest value for a bipolar row,
 *  else at the left. */
export function fillOrigin(s: RowSpec): number {
	if (!isBipolar(s)) return 0;
	const n = neutralOf(s);
	return n === null ? 0.5 : toPos(n, s);
}

/** Where to draw the rest tick, or `null`: a rest at either end of the travel is the end. */
export function tickPos(s: RowSpec): number | null {
	const n = neutralOf(s);
	if (n === null) return null;
	const p = toPos(n, s);
	return p > 1e-6 && p < 1 - 1e-6 ? p : null;
}

/** The value a double-click puts the row back to, or `null` when there is none or it is already exactly there. */
export function resetTo(v: number, s: RowSpec): number | null {
	const n = neutralOf(s);
	if (n === null) return null;
	return same(v, n) ? null : n;
}

/** Two values are the same when they are, give or take float noise: not when they read the same,
 *  or a 0.4° rotation (which reads 0°) could neither be reset nor typed back to 0. */
const same = (a: number, b: number): boolean => Math.abs(a - b) < 1e-9;

const display = (v: number, s: RowSpec): number => {
	const d = s.digits ?? 2;
	const n = Math.round(v * (s.scale ?? 1) * 10 ** d) / 10 ** d;
	return Object.is(n, -0) ? 0 : n;
};

/** The number as shown in the field: `1.00×`, `45°`, `55%`, `0.50`. */
export function readout(v: number, s: RowSpec): string {
	return `${display(v, s).toFixed(s.digits ?? 2)}${s.unit ?? ''}`;
}

/** The number without its unit, which is what the field offers for editing. */
export function editable(v: number, s: RowSpec): string {
	return display(v, s).toFixed(s.digits ?? 2);
}

const ENTRY = /^([+-]?(?:\d+\.?\d*|\.\d+))\s*(%|°|×|x|s)?$/i;

/** A typed entry as a value, or `null` for something that is not a number. The unit may be
 *  left off or written; it is shown units that are typed (50 in a percentage field is 50 %), and
 *  the result is held to the row's limits and its displayed precision. */
export function parseEntry(text: string, s: RowSpec): number | null {
	const m = ENTRY.exec(text.normalize('NFKC').trim().replace(',', '.'));
	if (!m) return null;
	const shown = Number(m[1]);
	if (!Number.isFinite(shown)) return null;
	const d = s.digits ?? 2;
	const rounded = Math.round(shown * 10 ** d) / 10 ** d;
	const lo = s.limit?.min ?? s.min;
	const hi = s.limit?.max ?? s.max;
	return clean(clamp(rounded / (s.scale ?? 1), lo, hi));
}

/** What committing a typed entry does: write `value`, or put the field back (not a number,
 *  or no change). */
export type Entry = { kind: 'commit'; value: number } | { kind: 'revert' };

export function entryFor(text: string, current: number, s: RowSpec): Entry {
	const v = parseEntry(text, s);
	if (v === null || same(v, current)) return { kind: 'revert' };
	return { kind: 'commit', value: v };
}

/** The value after a key. An arrow is a step, Shift five, Alt the last shown digit (never more
 *  than a step), a page a tenth of the travel; Home and End are the ends. */
export function stepBy(v: number, key: SliderKey, s: RowSpec): number {
	if (key.kind === 'edge') return key.to === 0 ? s.min : s.max;
	const digit = 1 / ((s.scale ?? 1) * 10 ** (s.digits ?? 2));
	const fine = Math.min(s.step, digit);
	const size = {
		fine,
		normal: s.step,
		coarse: s.step * 5,
		page: Math.max(s.step, span(s) / 10)
	}[key.size];
	return onGrid(v + key.dir * size, s, key.size === 'fine' ? fine : s.step);
}
