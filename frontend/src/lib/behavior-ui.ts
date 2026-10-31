// What the Behaviours section of the Inspector and the Motion tab's Behaviours group decide before they ask the
// backend — pure, so the sliders' ranges, the words and the grouping are tested: which numbers a kind has and the
// row each is, how a number is written into a behaviour, the seed field, and how adding a behaviour to several
// clips groups them. The arithmetic is `behaviors.ts`; the backend's limits are its.

import { BEHAVIOR_KINDS, BEHAVIOR_LABELS, defaultBehavior, full, type FullBehavior } from './behaviors';
import { DEGREES, PERCENT, type RowSpec } from './slider-row';
import {
	MAX_BEHAVIOR_HZ,
	MAX_BEHAVIOR_PHASE,
	MAX_BEHAVIORS,
	MAX_BREATHE,
	MAX_DRIFT,
	MAX_SHAKE,
	MAX_SPIN_TURNS,
	MAX_SWAY_DEGREES,
	type Behavior,
	type BehaviorKind,
	type Clip
} from './types';

/** The numbers of a behaviour a slider edits (`seed` is a field of its own). */
export type FieldKey = 'amount' | 'speed' | 'angle' | 'phase';

export interface BehaviorField {
	key: FieldKey;
	label: string;
	spec: RowSpec;
	/** What the number does, for the row's tooltip. */
	hint: string;
}

const HZ = { digits: 2, unit: 'Hz' } as const;

const PHASE: BehaviorField = {
	key: 'phase',
	label: 'Phase',
	spec: { min: -10, max: 10, step: 0.05, neutral: 0, digits: 2, unit: 's', limit: { min: -MAX_BEHAVIOR_PHASE, max: MAX_BEHAVIOR_PHASE } },
	hint: 'Seconds added to the clip’s clock: where in its cycle the motion starts. A cut moves it so the motion carries on'
};

/** The rows of a kind, in the order they are drawn. The slider travel is the useful part of the range; a typed
 *  entry may go on to the limit the backend holds (`limit`). */
export function behaviorFields(kind: BehaviorKind): BehaviorField[] {
	switch (kind) {
		case 'sway':
			return [
				{ key: 'amount', label: 'Amount', spec: { min: 0, max: 45, step: 0.5, neutral: 5, ...DEGREES, digits: 1, limit: { max: MAX_SWAY_DEGREES } }, hint: 'How far it rocks each way' },
				{ key: 'speed', label: 'Speed', spec: { min: 0, max: 4, step: 0.05, neutral: 0.5, ...HZ, limit: { max: MAX_BEHAVIOR_HZ } }, hint: 'Rocks a second' },
				PHASE
			];
		case 'breathe':
			return [
				{ key: 'amount', label: 'Depth', spec: { min: 0, max: 0.3, step: 0.01, neutral: 0.04, ...PERCENT, limit: { max: MAX_BREATHE } }, hint: 'How far the zoom swings, as a share of the zoom' },
				{ key: 'speed', label: 'Speed', spec: { min: 0, max: 4, step: 0.05, neutral: 0.3, ...HZ, limit: { max: MAX_BEHAVIOR_HZ } }, hint: 'Breaths a second' },
				PHASE
			];
		case 'spin':
			return [
				{
					key: 'speed',
					label: 'Speed',
					// Turns a second, shown as degrees a second: a tenth of a turn is 36°/s.
					spec: { min: -2, max: 2, step: 0.05, neutral: 0, scale: 360, digits: 0, unit: '°/s', limit: { min: -MAX_SPIN_TURNS, max: MAX_SPIN_TURNS } },
					hint: 'How fast it turns; negative turns the other way'
				},
				PHASE
			];
		case 'drift':
			return [
				{
					key: 'speed',
					label: 'Speed',
					// Frame widths (heights) a second, shown as a share of the frame.
					spec: { min: -0.2, max: 0.2, step: 0.005, neutral: 0, scale: 100, digits: 1, unit: '%/s', limit: { min: -MAX_DRIFT, max: MAX_DRIFT } },
					hint: 'How far it slides a second, as a share of the frame; negative slides it back'
				},
				{ key: 'angle', label: 'Angle', spec: { min: -180, max: 180, step: 5, neutral: 0, bipolar: true, ...DEGREES, limit: { min: -360, max: 360 } }, hint: 'The direction: 0° is right, 90° is down' },
				PHASE
			];
		case 'shake':
			return [
				{ key: 'amount', label: 'Amount', spec: { min: 0, max: 0.05, step: 0.001, neutral: 0.01, scale: 100, digits: 1, unit: '%', limit: { max: MAX_SHAKE } }, hint: 'How far it jitters, as a share of the frame' },
				{ key: 'speed', label: 'Speed', spec: { min: 0, max: 20, step: 0.5, neutral: 8, digits: 1, unit: 'Hz', limit: { max: MAX_BEHAVIOR_HZ } }, hint: 'How fast it jitters' },
				PHASE
			];
	}
}

/** The number a row shows. */
export function fieldValue(b: Behavior, key: FieldKey): number {
	return (full(b) as unknown as Partial<Record<FieldKey, number>>)[key] ?? 0;
}

/** `b` with one of its numbers set. */
export function withField(b: Behavior, key: FieldKey, value: number): Behavior {
	return { ...full(b), [key]: value } as Behavior;
}

/** `b` with its shake seed set. */
export function withSeed(b: Behavior, seed: number): Behavior {
	if (b.kind !== 'shake') return b;
	return { ...(full(b) as Extract<FullBehavior, { kind: 'shake' }>), seed };
}

/** The largest seed (a `u32`). */
export const MAX_SEED = 4_294_967_295;

/** What the seed field means: a whole number from 0 to `MAX_SEED`; anything else (`undefined`) is no entry and the
 *  field goes back to what it held. */
export function parseSeed(text: string): number | undefined {
	const t = text.trim();
	if (!/^\d{1,10}$/.test(t)) return undefined;
	const v = Number(t);
	return v <= MAX_SEED ? v : undefined;
}

/** A row's name, with a number when its kind is on the clip more than once: "Sway", "Sway 2". */
export function rowName(list: readonly Behavior[], index: number): string {
	const kind = list[index].kind;
	const same = list.filter((b) => b.kind === kind);
	const label = BEHAVIOR_LABELS[kind];
	return same.length > 1 ? `${label} ${list.slice(0, index + 1).filter((b) => b.kind === kind).length}` : label;
}

/** A behaviour's starting numbers in the units the rows show: "5° · 0.5Hz". */
function startingNumbers(d: FullBehavior): string {
	switch (d.kind) {
		case 'sway':
			return `${d.amount}° · ${d.speed}Hz`;
		case 'breathe':
			return `${Math.round(d.amount * 100)}% · ${d.speed}Hz`;
		case 'spin':
			return `${Math.round(d.speed * 360)}°/s`;
		case 'drift':
			return `${Math.round(d.speed * 1000) / 10}%/s`;
		case 'shake':
			return `${Math.round(d.amount * 1000) / 10}% · ${d.speed}Hz`;
	}
}

/** The menu of kinds a new behaviour is chosen from: the name and its starting numbers. */
export const ADD_CHOICES: readonly { kind: BehaviorKind; label: string; meta: string }[] = BEHAVIOR_KINDS.map((kind) => ({
	kind,
	label: BEHAVIOR_LABELS[kind],
	meta: startingNumbers(defaultBehavior(kind))
}));

/** The tooltip of a section's Bake button, or why it is off. */
export function bakeTip(count: number): string {
	return count === 0
		? 'Add a behaviour first'
		: 'Turn the behaviours into ordinary keyframes at the delivery frame rate, so the motion can be edited. Exact on every frame; a long clip is refused: split it, or keep the behaviours live';
}

// ---- adding to several clips (the Motion tab) ----------------------------------------------------

/** What adding `kind` does to `targets`: the clips already at the limit (the whole edit is refused, as the backend
 *  would), else one entry per distinct list the clips carry now — each is the clips that share it and that list
 *  with the new behaviour last. Clips with the same list are one edit; the usual case, none having any, is one. */
export function additions(
	targets: readonly Clip[],
	kind: BehaviorKind,
	seed: number
): { full: Clip[]; groups: { ids: string[]; list: Behavior[] }[] } {
	const fresh = defaultBehavior(kind, seed);
	const atLimit = targets.filter((c) => (c.behaviors?.length ?? 0) >= MAX_BEHAVIORS);
	const groups = new Map<string, { ids: string[]; list: Behavior[] }>();
	for (const c of targets) {
		const have = (c.behaviors ?? []).map(full);
		const key = JSON.stringify(have);
		const group = groups.get(key) ?? { ids: [], list: [...have, fresh] };
		group.ids.push(c.id);
		groups.set(key, group);
	}
	return { full: atLimit, groups: [...groups.values()] };
}

/** What to say when a click is refused for clips at the limit. */
export function limitNotice(atLimit: readonly Clip[], targets: number): string {
	return targets === 1
		? `This clip already has ${MAX_BEHAVIORS} behaviours — remove one first`
		: `${atLimit.length} of the ${targets} selected clips already have ${MAX_BEHAVIORS} behaviours — remove one first`;
}

/** The tile's tooltip: what the kind does and its starting numbers, or why it is off. */
export function tileTip(kind: BehaviorKind, hint: string, reason: string | null) {
	const choice = ADD_CHOICES.find((c) => c.kind === kind)!;
	return { text: choice.label, detail: reason ?? `${hint}. Starts at ${choice.meta}; tune it in the Inspector.` };
}
