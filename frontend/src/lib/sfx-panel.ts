// The Sound FX panel's pure half: what a tile draws (a glyph, a hue, a sketch of the sound's envelope), which tiles a
// search and a category let through, the numbers behind the Shape sliders, a spec patched and placed, and the list of
// suggestions with its picks. The panel (`SoundFx.svelte`), its state (`sfx-panel.svelte.ts`) and the Inspector's Sound
// effect section only wire these to the screen.

import { PERCENT, SECONDS, type RowSpec } from './slider-row';
import {
	MAX_SEED,
	MAX_SFX_GAIN_DB,
	MAX_SFX_PITCH,
	MIN_SFX_GAIN_DB,
	SFX_CATEGORIES,
	resolvedDuration,
	sfxAnchor,
	sfxPreset,
	sfxSpan
} from './sfx';
import type {
	SfxAnchor,
	SfxCategory,
	SfxDensity,
	SfxKind,
	SfxPlacement,
	SfxPreset,
	SfxSource,
	SfxSpec,
	SfxSuggestRequest,
	SfxSuggestion
} from './types';

// ---- how a tile looks ----------------------------------------------------------------------

/** The icon of each kind (a key of `components/editor/icons.ts`). */
export const SFX_GLYPHS: Record<SfxKind, string> = {
	whoosh: 'wind',
	swoosh: 'feather',
	swipe: 'chevrons-right',
	impact: 'zap',
	boom: 'bomb',
	thud: 'drum',
	punch: 'hammer',
	riser: 'trending-up',
	downlifter: 'trending-down',
	reverse_swell: 'rewind',
	pop: 'sparkle',
	click: 'mouse-pointer-click',
	ding: 'bell',
	notify: 'bell-ring',
	bubble: 'bubbles',
	typewriter: 'keyboard',
	glitch: 'tv',
	tape_stop: 'cassette-tape',
	record_scratch: 'disc-3',
	shutter: 'camera',
	heartbeat: 'heart-pulse'
};

/** The theme token each category is drawn in: a glyph and its sketch, so the grid reads by colour as well as by shape. */
export const CATEGORY_HUES: Record<SfxCategory, string> = {
	transition: 'var(--agent-400)',
	impact: 'var(--red-400)',
	build: 'var(--kerf-400)',
	ui: 'var(--green-400)',
	texture: 'var(--blue-500)'
};

export const glyphOf = (kind: SfxKind): string => SFX_GLYPHS[kind];
export const hueOf = (category: SfxCategory): string => CATEGORY_HUES[category];

/** The chips over the grid: everything, then each category as the engine names it. */
export type CategoryFilter = SfxCategory | 'all';
export const CATEGORY_OPTIONS: readonly { value: CategoryFilter; label: string }[] = [
	{ value: 'all', label: 'All' },
	...SFX_CATEGORIES.map((c) => ({ value: c.name as CategoryFilter, label: c.label }))
];

export const isCategoryFilter = (v: unknown): v is CategoryFilter => CATEGORY_OPTIONS.some((o) => o.value === v);

// ---- the envelope sketch ---------------------------------------------------------------------

type Env = (t: number) => number;

/** Instant attack at `at`, then an exponential fall. */
const hit =
	(at: number, decay: number, gain = 1): Env =>
	(t) =>
		t < at ? 0 : gain * Math.exp(-(t - at) / decay);
/** A swell to `centre`, `left` wide before it and `right` after. */
const swell =
	(centre: number, left: number, right = left, gain = 1): Env =>
	(t) =>
		gain * Math.exp(-(((t - centre) / (t < centre ? left : right)) ** 2));
const sum =
	(...parts: Env[]): Env =>
	(t) =>
		parts.reduce((s, f) => s + f(t), 0);
const times =
	(a: Env, b: Env): Env =>
	(t) =>
		a(t) * b(t);
const taps = (positions: readonly number[], decay: number, gains: readonly number[] = []): Env =>
	sum(...positions.map((p, i) => hit(p, decay, gains[i] ?? 1)));
/** A level that holds for a stretch and then changes: `steps` are `[from, level]` in order. */
const stepped =
	(steps: readonly (readonly [number, number])[]): Env =>
	(t) => {
		let level = 0;
		for (const [from, l] of steps) if (t >= from) level = l;
		return level;
	};

/** What each sound does over its length, 0 to 1 against 0 to 1 of the time — the *character* of the sound (a whoosh
 *  swells and falls, an impact lands and rings out, a riser climbs to a sharp end), not a render of it. Pure and
 *  deterministic: nothing is random, so a tile never changes between draws. */
const ENVELOPES: Record<SfxKind, Env> = {
	whoosh: swell(0.55, 0.26, 0.2),
	swoosh: swell(0.45, 0.17, 0.12),
	swipe: swell(0.4, 0.11, 0.09),
	impact: sum(hit(0, 0.17), hit(0, 0.7, 0.22)),
	boom: times(sum(swell(0.04, 0.03, 0.4), hit(0.05, 0.55, 0.35)), (t) => 0.88 + 0.12 * Math.sin(t * 46)),
	thud: sum(hit(0, 0.1), hit(0, 0.22, 0.2)),
	punch: sum(hit(0, 0.055), hit(0, 0.2, 0.28)),
	riser: (t) => (t < 0.965 ? Math.pow(t, 2.1) * (0.9 + 0.1 * Math.sin(t * 90)) : 0),
	downlifter: sum(hit(0, 0.3), hit(0, 0.9, 0.18)),
	reverse_swell: (t) => (t < 0.97 ? Math.pow(t, 3.2) : 0),
	pop: sum(hit(0, 0.07), hit(0, 0.2, 0.15)),
	click: hit(0, 0.045),
	ding: times(hit(0, 0.42), (t) => 0.9 + 0.1 * Math.cos(t * 70)),
	notify: sum(hit(0.04, 0.12, 0.85), hit(0.36, 0.22)),
	bubble: sum(swell(0.2, 0.06, 0.05, 0.75), swell(0.47, 0.07, 0.06, 0.9), swell(0.76, 0.08, 0.07)),
	typewriter: taps([0.04, 0.16, 0.27, 0.42, 0.51, 0.64, 0.74, 0.85, 0.94], 0.022, [1, 0.8, 0.95, 0.7, 1, 0.85, 0.75, 0.95, 0.9]),
	glitch: stepped([
		[0, 0.9],
		[0.09, 0.15],
		[0.16, 0.7],
		[0.27, 0],
		[0.31, 1],
		[0.42, 0.35],
		[0.5, 0.8],
		[0.58, 0.1],
		[0.66, 0.95],
		[0.8, 0.25],
		[0.88, 0.6],
		[0.96, 0]
	]),
	tape_stop: (t) => (t < 0.14 ? 0.95 : Math.pow(Math.max(0, 1 - (t - 0.14) / 0.86), 1.7)),
	record_scratch: times(swell(0.45, 0.32, 0.3), (t) => 0.3 + 0.7 * Math.abs(Math.sin(t * 21))),
	shutter: sum(hit(0.04, 0.035), hit(0.24, 0.05, 0.9), swell(0.14, 0.05, 0.05, 0.14)),
	heartbeat: taps([0.04, 0.2, 0.56, 0.72], 0.05, [1, 0.72, 0.9, 0.66])
};

/** The points of a sound's envelope: `n` samples of 0 to 1 over its length. */
export function envelopeOf(kind: SfxKind, n = 64, reverse = false): number[] {
	const f = ENVELOPES[kind];
	const raw = Array.from({ length: n }, (_, i) => {
		const j = reverse ? n - 1 - i : i;
		return Math.max(0, f(n > 1 ? j / (n - 1) : 0));
	});
	const peak = Math.max(...raw, 1e-9);
	return raw.map((v) => v / peak);
}

/** Where the hit point is in the sound, 0 to 1 of its length: the catalogue's anchor against the default length, mirrored by
 *  `reverse` (`sfxAnchor`'s own rule). */
export function hitFraction(preset: Pick<SfxPreset, 'anchor' | 'default_duration_s'>, reverse = false): number {
	const a: SfxAnchor = preset.anchor;
	const at = a.type === 'hit' ? Math.min(a.seconds / Math.max(preset.default_duration_s, 1e-9), 1) : a.type === 'peak' ? a.fraction : 1;
	return reverse ? 1 - at : at;
}

export interface Sketch {
	/** A closed SVG path: the envelope drawn both ways from the middle, like a waveform. */
	d: string;
	/** Where the hit lands, 0 to 1 across. */
	hit: number;
}

const r1 = (v: number) => Math.round(v * 10) / 10;

/** The sketch of `kind` in a `w`×`h` box (the tile draws it with `preserveAspectRatio="none"`, so any box that has the
 *  right proportions will do): a hairline of floor so the quietest stretch is still there. */
export function sketchOf(preset: Pick<SfxPreset, 'kind' | 'anchor' | 'default_duration_s'>, w: number, h: number, reverse = false, n = 64): Sketch {
	const env = envelopeOf(preset.kind, n, reverse);
	const mid = h / 2;
	const room = Math.max(mid - 1, 0);
	const top = env.map((v, i) => `${r1((i / (n - 1)) * w)},${r1(mid - Math.max(v, 0.035) * room)}`);
	const bottom = env.map((v, i) => `${r1((i / (n - 1)) * w)},${r1(mid + Math.max(v, 0.035) * room)}`).reverse();
	return { d: `M${top.join('L')}L${bottom.join('L')}Z`, hit: hitFraction(preset, reverse) };
}

// ---- which tiles ---------------------------------------------------------------------------------

const words = (s: string) =>
	s
		.toLowerCase()
		.replace(/[_-]+/g, ' ')
		.split(/\s+/)
		.filter(Boolean);

/** The presets a search and a category let through, in catalogue order. Every word typed has to be in the preset's name, kind,
 *  category or description (so `riser build` finds the riser and `tension` finds the ones the description says it for). */
export function filterPresets(rows: readonly SfxPreset[], query: string, category: CategoryFilter): SfxPreset[] {
	const wanted = words(query);
	return rows.filter((p) => {
		if (category !== 'all' && p.category !== category) return false;
		if (wanted.length === 0) return true;
		const label = SFX_CATEGORIES.find((c) => c.name === p.category)?.label ?? p.category;
		const hay = words(`${p.label} ${p.kind} ${label} ${p.description}`).join(' ');
		return wanted.every((w) => hay.includes(w));
	});
}

/** What the grid says when nothing matches. */
export function noMatchLine(query: string, category: CategoryFilter): string {
	const q = query.trim();
	const cat = CATEGORY_OPTIONS.find((o) => o.value === category)?.label;
	if (q && category !== 'all') return `No ${cat} sound matches “${q}”. Try All.`;
	if (q) return `No sound matches “${q}”.`;
	return 'Nothing in this category.';
}

/** The tile after `index` in a grid of `columns`, for the arrow keys: left and right step, up and down a row, Home and End
 *  the ends. Stays put at an edge. */
export function gridStep(index: number, count: number, columns: number, key: 'ArrowLeft' | 'ArrowRight' | 'ArrowUp' | 'ArrowDown' | 'Home' | 'End'): number {
	if (count <= 0) return -1;
	const cols = Math.max(1, columns);
	const next =
		key === 'ArrowLeft' ? index - 1 : key === 'ArrowRight' ? index + 1 : key === 'ArrowUp' ? index - cols : key === 'ArrowDown' ? index + cols : key === 'Home' ? 0 : count - 1;
	return Math.min(Math.max(next, 0), count - 1);
}

// ---- the Shape sliders -------------------------------------------------------------------------------

/** The Length slider: the kind's own range, resting at its default length. */
export const lengthSpec = (p: Pick<SfxPreset, 'min_duration_s' | 'max_duration_s' | 'default_duration_s'>): RowSpec => ({
	min: p.min_duration_s,
	max: p.max_duration_s,
	step: 0.01,
	neutral: p.default_duration_s,
	...SECONDS
});
export const PITCH_SPEC: RowSpec = { min: -MAX_SFX_PITCH, max: MAX_SFX_PITCH, step: 1, neutral: 0, digits: 0, unit: ' st' };
export const INTENSITY_SPEC: RowSpec = { min: 0, max: 1, step: 0.01, neutral: 0.7, ...PERCENT };
export const TAIL_SPEC: RowSpec = { min: 0, max: 1, step: 0.01, neutral: 0.3, ...PERCENT };
/** A sound effect sits well inside the clip gain's -60..12: the slider covers what is used, a typed entry reaches the rest. */
export const GAIN_SPEC: RowSpec = {
	min: -24,
	max: 6,
	step: 0.5,
	neutral: 0,
	digits: 1,
	unit: 'dB',
	limit: { min: MIN_SFX_GAIN_DB, max: MAX_SFX_GAIN_DB }
};

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** `spec` with `patch` written over it, every number held to what the engine accepts (`SfxSpec::validate`), so no slider or
 *  typed entry can make a spec the backend refuses. The length is held to the kind's own range. */
export function patchSpec(spec: SfxSpec, patch: Partial<SfxSpec>): SfxSpec {
	const next: SfxSpec = { ...spec, ...patch };
	if (patch.kind && patch.kind !== spec.kind && patch.duration_s === undefined) delete next.duration_s;
	if (next.duration_s !== undefined) next.duration_s = resolvedDuration(next);
	next.pitch = clamp(Math.round(next.pitch), -MAX_SFX_PITCH, MAX_SFX_PITCH);
	next.intensity = clamp(next.intensity, 0, 1);
	next.tail = clamp(next.tail, 0, 1);
	next.seed = clamp(Math.trunc(next.seed), 0, MAX_SEED);
	return next;
}

/** A different seed from `current`: small enough to read (`#4821`), never the same, never past the bound. */
export function nextSeed(current: number, rand: () => number = Math.random): number {
	const pick = 1 + Math.floor(rand() * 9999);
	return pick === current ? (pick % 9999) + 1 : pick;
}

/** What the spec has been moved off, bar its length: `+3 st`, `Tail 60%`, `Reversed`, `#12`. */
export function shapeChanges(spec: SfxSpec): string[] {
	const out: string[] = [];
	if (spec.pitch !== 0) out.push(`${spec.pitch > 0 ? '+' : ''}${spec.pitch} st`);
	if (Math.abs(spec.intensity - 0.7) > 0.004) out.push(`Intensity ${Math.round(spec.intensity * 100)}%`);
	if (Math.abs(spec.tail - 0.3) > 0.004) out.push(`Tail ${Math.round(spec.tail * 100)}%`);
	if (spec.reverse) out.push('Reversed');
	if (spec.seed !== 0) out.push(`#${spec.seed}`);
	return out;
}

/** What the spec has been moved off — `1.50 s · +3 st · Tail 60%` — for a summary line; `Defaults` when nothing. */
export function shapeSummary(spec: SfxSpec, preset: Pick<SfxPreset, 'default_duration_s'> = sfxPreset(spec.kind)): string {
	const out: string[] = [];
	const len = resolvedDuration(spec);
	if (Math.abs(len - preset.default_duration_s) > 0.0005) out.push(`${len.toFixed(2)} s`);
	out.push(...shapeChanges(spec));
	return out.length ? out.join(' · ') : 'Defaults';
}

/** The sound as it will be placed, in a line: its length first, then what was moved off the defaults. */
export function shapeLine(spec: SfxSpec): string {
	return [`${resolvedDuration(spec).toFixed(2)} s`, ...shapeChanges(spec)].join(' · ');
}

/** The note under Add at playhead: which part of the sound lands on the playhead, and so how long before it the clip starts. */
export function anchorNote(spec: SfxSpec): string {
	const len = resolvedDuration(spec);
	const hitAt = sfxAnchor(spec, len);
	const ratio = len > 0 ? hitAt / len : 0;
	if (hitAt < 0.05) return 'Its hit lands on the playhead.';
	const part = ratio > 0.98 ? 'end' : 'peak';
	return `Its ${part} lands on the playhead; the clip starts ${hitAt.toFixed(2)} s before it.`;
}

/** The placement of `spec` with its hit at `at`: the spec's fields flat, the length only when it was set, the gain only when
 *  it is not the default 0. */
export function placementOf(spec: SfxSpec, at: number, gainDb = 0, trackId?: string): SfxPlacement {
	const { duration_s, ...rest } = spec;
	return {
		...rest,
		...(duration_s !== undefined ? { duration_s } : {}),
		at,
		...(gainDb !== 0 ? { volume_db: gainDb } : {}),
		...(trackId ? { track_id: trackId } : {})
	};
}

/** The dB a clip's linear gain is (`volume` is 0 when the clip is silent). */
export function dbOf(volume: number): number {
	return volume > 0 ? Math.round(20 * Math.log10(volume) * 10) / 10 : MIN_SFX_GAIN_DB;
}

/** Whether a rejection is the backend saying the project was replaced while the sound rendered (`SFX_CANCELLED`). */
export function isSfxCancelled(e: unknown): boolean {
	return (e instanceof Error ? e.message : String(e)) === 'sound effect cancelled';
}

// ---- what a drop onto the timeline means ----------------------------------------------------------------

/** How the timeline draws a tile dragged over a lane: where the clip would start and how long it shows, with `ok` false where
 *  it cannot land (the hit too close to the start, or another clip in the way). */
export interface DropGhost {
	start: number;
	dur: number;
	ok: boolean;
}

/** The payload a tile drag carries (`ui.dndSfx`): the spec to place and its length, so the lanes can draw the ghost before the
 *  drop. */
export interface SfxDrag {
	spec: SfxSpec;
	gainDb: number;
	/** The render's length, seconds. */
	duration: number;
	label: string;
}

export const dragOf = (spec: SfxSpec, gainDb: number, label: string): SfxDrag => ({ spec, gainDb, duration: resolvedDuration(spec), label });

/** Where a dragged sound would sit on a lane if its hit were dropped at `hit` (`sfxSpan`, the placement's own rule: the clip
 *  starts earlier by how far in its hit is, and a start before 0 cuts the head off), and whether it fits among `others`, the
 *  `[start, end)` of the clips already there. A hit that cannot land at all is drawn where it would be, `ok: false`. */
export function dropGhostFor(drag: SfxDrag, hit: number, others: readonly (readonly [number, number])[]): DropGhost {
	const span = sfxSpan(drag.spec, drag.duration, hit);
	if (!span) return { start: 0, dur: Math.max(drag.duration - sfxAnchor(drag.spec, drag.duration) + hit, 0.02), ok: false };
	const [start, cut] = span;
	const dur = drag.duration - cut;
	return { start, dur, ok: !others.some(([s, e]) => start < e - 1e-9 && s < start + dur - 1e-9) };
}

// ---- auto sound design ---------------------------------------------------------------------------------

export const DENSITY_OPTIONS: readonly { value: SfxDensity; label: string; hint: string }[] = [
	{ value: 'subtle', label: 'Subtle', hint: 'Transitions, titles, drops in the music and markers, at least 4 s apart' },
	{ value: 'balanced', label: 'Balanced', hint: 'Adds the cuts between shots of 1.5 s or more, at least 2 s apart' },
	{ value: 'punchy', label: 'Punchy', hint: 'Every moment that can take a sound, at least 1 s apart' }
];

/** What Find moments asks for. */
export function suggestRequest(density: SfxDensity, category: CategoryFilter): SfxSuggestRequest {
	return { density, ...(category === 'all' ? {} : { categories: [category] }) };
}

const SOURCES: Record<SfxSource, string> = {
	cut: 'Cut',
	transition: 'Transition',
	title: 'Title',
	punch_in: 'Punch-in',
	music_drop: 'Music drop',
	marker: 'Marker',
	opening: 'Opening'
};
export const sourceLabel = (source: SfxSource): string => SOURCES[source] ?? source;

/** One suggestion as a row of the list. */
export interface SuggestionRow {
	index: number;
	kind: SfxKind;
	label: string;
	category: SfxCategory;
	source: string;
	/** Where the hit lands, seconds. */
	at: number;
	reason: string;
	gainDb: number | null;
	/** How much the moment wants a sound, 0 to 1. */
	score: number;
}

export function suggestionRows(suggestions: readonly SfxSuggestion[]): SuggestionRow[] {
	return suggestions.map((s, index) => ({
		index,
		kind: s.placement.kind,
		label: sfxPreset(s.placement.kind).label,
		category: sfxPreset(s.placement.kind).category,
		source: sourceLabel(s.source),
		at: s.placement.at,
		reason: s.reason,
		gainDb: s.placement.volume_db ?? null,
		score: s.score
	}));
}

/** Which suggestions are ticked, by position. */
export type Picks = ReadonlySet<number>;
export const pickAll = (count: number): Picks => new Set(Array.from({ length: count }, (_, i) => i));
export const pickNone = (): Picks => new Set();
export function togglePick(picks: Picks, index: number): Picks {
	const next = new Set(picks);
	if (!next.delete(index)) next.add(index);
	return next;
}
/** The placements of the ticked suggestions, in time order, exactly as the engine suggested them. */
export function pickedPlacements(suggestions: readonly SfxSuggestion[], picks: Picks): SfxPlacement[] {
	return suggestions.flatMap((s, i) => (picks.has(i) ? [s.placement] : []));
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** The label of the button that adds the ticked suggestions. */
export const addLabel = (n: number): string => (n === 0 ? 'Add effects' : `Add ${plural(n, 'effect')}`);
/** `5 of 12 selected`. */
export const pickLine = (picked: number, total: number): string => `${picked} of ${total} selected`;
/** The notice after an add. */
export const addedLine = (n: number, label?: string): string => (n === 1 && label ? `Added ${label}` : `Added ${plural(n, 'sound effect')}`);

/** What stays under the notice of an add: how to take it back. */
export const undoLine = (n: number): string => `Undo takes ${n === 1 ? 'it' : 'them'} out again.`;

/** What the list says when Find moments came back with nothing. */
export function nothingFoundLine(density: SfxDensity, category: CategoryFilter): string {
	const cat = CATEGORY_OPTIONS.find((o) => o.value === category)?.label;
	const narrowed = category !== 'all' ? ` among ${cat}` : '';
	const more = density === 'punchy' ? '' : ' Try a punchier density.';
	return `Nothing in the cut calls for a sound${narrowed}.${more}`;
}

/** The fingerprint of the cut a list of suggestions was found on: it goes stale the moment the cut changes. */
export const stampOf = (seq: number | null, path: string | null): string => `${path ?? ''}#${seq ?? ''}`;
