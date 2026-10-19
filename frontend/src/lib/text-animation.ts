/* A text's animation — pure. kerf-core stores it as JSON on the `<text>` (`data-kerf-animate`) and compiles it into
 * per-glyph SMIL when the text is glyph outlines (`svg/text_animation.rs`, `svg/glyph_smil.rs`). What is here is what the
 * page needs of it: the bounds an animation is held to (`checkAnimation`), the canonical JSON an attribute holds
 * (`animationJson`) and its reading (`parseAnimation`), and the presets. `text-animation-corpus.test.ts` replays what
 * kerf-core wrote down. Nothing here draws a frame: the engine does. */
import type { AnimateBy, AnimationEasing, AnimationShape, TextAnimation, TextAnimationPreset } from './types';

export const ANIMATE_MARK = 'data-kerf-animate';

export const MAX_DELAY = 30;
export const MAX_DURATION = 30;
export const MIN_DURATION = 0.05;
export const MAX_REACH = 10;
/** The most JSON an attribute may hold (`MAX_JSON`). */
const MAX_JSON = 8 * 1024;

const BY: readonly AnimateBy[] = ['character', 'word', 'line'];
const EASING: readonly AnimationEasing[] = ['linear', 'ease_in', 'ease_out', 'ease_in_out'];
const SHAPE: readonly AnimationShape[] = ['square', 'ramp_up', 'ramp_down', 'triangle', 'round', 'smooth'];

/** The animation with nothing set (`TextAnimation::default`): not one a text may have, it changes nothing. */
export function defaultAnimation(): TextAnimation {
	return {
		by: 'character',
		delay: 0,
		duration: 1,
		easing: 'linear',
		range: { start: 0, end: 1, offset: { from: 0, to: 0 }, shape: 'square' },
		wiggle: null,
		properties: { x: 0, y: 0, scale: 1, rotation: 0, opacity: 1, blur: 0 }
	};
}

const within = (name: string, v: number, lo: number, hi: number): string | null =>
	Number.isFinite(v) && v >= lo && v <= hi ? null : `a text animation's ${name} is between ${lo} and ${hi}`;

/** Why `a` cannot be a text's animation, or `null` (`TextAnimation::check`, the same words). */
export function checkAnimation(a: TextAnimation): string | null {
	const r = a.range;
	const p = a.properties;
	const refusal =
		within('delay', a.delay, 0, MAX_DELAY) ??
		within('duration', a.duration, MIN_DURATION, MAX_DURATION) ??
		within('range start', r.start, -MAX_REACH, MAX_REACH) ??
		within('range end', r.end, -MAX_REACH, MAX_REACH) ??
		(r.end - r.start < 0.001 ? "a text animation's range ends after it starts" : null) ??
		within('offset from', r.offset.from, -MAX_REACH, MAX_REACH) ??
		within('offset to', r.offset.to, -MAX_REACH, MAX_REACH) ??
		(a.wiggle
			? (within('wiggle rate', a.wiggle.rate, 0.1, 30) ??
				within('wiggle min', a.wiggle.min, -1, 1) ??
				within('wiggle max', a.wiggle.max, -1, 1) ??
				(a.wiggle.min > a.wiggle.max ? "a text animation's wiggle min is at most its max" : null) ??
				within('wiggle correlation', a.wiggle.correlation, 0, 1))
			: null) ??
		within('x', p.x, -20, 20) ??
		within('y', p.y, -20, 20) ??
		within('scale', p.scale, 0, 10) ??
		within('rotation', p.rotation, -3600, 3600) ??
		within('opacity', p.opacity, 0, 1) ??
		within('blur', p.blur, 0, 1);
	if (refusal) return refusal;
	if (p.x === 0 && p.y === 0 && p.scale === 1 && p.rotation === 0 && p.opacity === 1 && p.blur === 0) {
		return 'a text animation changes nothing: give it an x, y, scale, rotation, opacity or blur other than the neutral one';
	}
	return null;
}

/** A number as kerf-core's JSON writes an `f64`: a whole one with a `.0`. */
const num = (v: number): string => (Number.isInteger(v) ? v.toFixed(1) : String(v));

/** The JSON an attribute holds, byte for byte what `TextAnimation::to_attribute` writes (fields in the engine's order). */
export function animationJson(a: TextAnimation): string {
	const { range: r, wiggle: w, properties: p } = a;
	const wiggle = w
		? `{"rate":${num(w.rate)},"min":${num(w.min)},"max":${num(w.max)},"correlation":${num(w.correlation)},"seed":${w.seed}}`
		: 'null';
	return (
		`{"by":"${a.by}","delay":${num(a.delay)},"duration":${num(a.duration)},"easing":"${a.easing}",` +
		`"range":{"start":${num(r.start)},"end":${num(r.end)},"offset":{"from":${num(r.offset.from)},"to":${num(r.offset.to)}},"shape":"${r.shape}"},` +
		`"wiggle":${wiggle},` +
		`"properties":{"x":${num(p.x)},"y":${num(p.y)},"scale":${num(p.scale)},"rotation":${num(p.rotation)},"opacity":${num(p.opacity)},"blur":${num(p.blur)}}}`
	);
}

type Json = Record<string, unknown>;
const isObject = (v: unknown): v is Json => typeof v === 'object' && v !== null && !Array.isArray(v);

/** The members of `v` laid over `defaults`, none of a name `defaults` lacks (`deny_unknown_fields`). */
function merged<T extends object>(what: string, v: unknown, defaults: T): T {
	if (v === undefined) return defaults;
	if (!isObject(v)) throw new Error(`${what} is not an object`);
	for (const k of Object.keys(v)) if (!(k in defaults)) throw new Error(`unknown field \`${k}\` in ${what}`);
	return { ...defaults, ...v };
}

function number(what: string, v: unknown): number {
	if (typeof v !== 'number') throw new Error(`${what} is not a number`);
	return v;
}

function oneOf<T extends string>(what: string, v: unknown, allowed: readonly T[]): T {
	if (typeof v !== 'string' || !(allowed as readonly string[]).includes(v)) throw new Error(`${what} is not one of ${allowed.join(', ')}`);
	return v as T;
}

/** An animation read from an attribute's JSON and checked, or `null` when it is not one (`TextAnimation::from_attribute`):
 *  missing fields take their defaults, an unknown one, a wrong type or a number out of bounds is refused. */
export function parseAnimation(raw: string | undefined): TextAnimation | null {
	if (raw === undefined || raw.length > MAX_JSON) return null;
	try {
		const d = defaultAnimation();
		const top = merged('the animation', JSON.parse(raw), { ...d, wiggle: d.wiggle as unknown });
		const range = merged('range', top.range, d.range);
		const offset = merged('offset', range.offset, d.range.offset);
		const properties = merged('properties', top.properties, d.properties);
		let wiggle: TextAnimation['wiggle'] = null;
		if (top.wiggle !== null && top.wiggle !== undefined) {
			const w = merged('wiggle', top.wiggle, { rate: 2, min: -1, max: 1, correlation: 0, seed: 0 });
			if (!Number.isInteger(w.seed) || w.seed < 0 || w.seed > 0xffffffff) throw new Error('wiggle seed is not a u32');
			wiggle = {
				rate: number('rate', w.rate),
				min: number('min', w.min),
				max: number('max', w.max),
				correlation: number('correlation', w.correlation),
				seed: w.seed
			};
		}
		const a: TextAnimation = {
			by: oneOf('by', top.by, BY),
			delay: number('delay', top.delay),
			duration: number('duration', top.duration),
			easing: oneOf('easing', top.easing, EASING),
			range: {
				start: number('start', range.start),
				end: number('end', range.end),
				offset: { from: number('from', offset.from), to: number('to', offset.to) },
				shape: oneOf('shape', range.shape, SHAPE)
			},
			wiggle,
			properties: {
				x: number('x', properties.x),
				y: number('y', properties.y),
				scale: number('scale', properties.scale),
				rotation: number('rotation', properties.rotation),
				opacity: number('opacity', properties.opacity),
				blur: number('blur', properties.blur)
			}
		};
		return checkAnimation(a) ? null : a;
	} catch {
		return null;
	}
}

/** Whether two animations are the same one. */
export const sameAnimation = (a: TextAnimation, b: TextAnimation): boolean => animationJson(a) === animationJson(b);

const props = (p: Partial<TextAnimation['properties']>): TextAnimation['properties'] => ({
	...defaultAnimation().properties,
	...p
});

/** A window of `width` text lengths slid from just before the text to just past it, the amount rising across it. */
const sweep = (width: number, shape: AnimationShape): TextAnimation['range'] => ({
	start: 0,
	end: width,
	offset: { from: -width, to: 1 },
	shape
});

const preset = (id: string, name: string, description: string, change: Partial<TextAnimation>): TextAnimationPreset => ({
	id,
	name,
	description,
	animation: { ...defaultAnimation(), ...change }
});

/** The eight presets, as `text_animation_presets` lists them (replayed against it by the corpus test): the harness's
 *  list, and what the Inspector's picker matches a text's animation against. */
export const TEXT_ANIMATION_PRESETS: readonly TextAnimationPreset[] = [
	preset('rise', 'Rise', 'Characters fade in one after another, rising into place.', {
		duration: 1.4,
		easing: 'ease_out',
		range: sweep(0.4, 'ramp_up'),
		properties: props({ y: 0.5, opacity: 0 })
	}),
	preset('fade_in', 'Fade in', 'Words fade in one after another.', {
		by: 'word',
		duration: 1.2,
		range: sweep(0.5, 'ramp_up'),
		properties: props({ opacity: 0 })
	}),
	preset('typewriter', 'Typewriter', 'Characters appear one at a time, left to right.', {
		duration: 1.5,
		range: { start: 0, end: 1, offset: { from: 0, to: 1 }, shape: 'square' },
		properties: props({ opacity: 0 })
	}),
	preset('pop', 'Pop', 'Characters grow from nothing to their size, one after another.', {
		duration: 1,
		easing: 'ease_out',
		range: sweep(0.3, 'ramp_up'),
		properties: props({ scale: 0, opacity: 0 })
	}),
	preset('blur_in', 'Blur in', 'Characters come into focus one after another.', {
		duration: 1.4,
		easing: 'ease_out',
		range: sweep(0.5, 'ramp_up'),
		properties: props({ opacity: 0, blur: 0.25 })
	}),
	preset('spin_in', 'Spin in', 'Characters turn and grow into place, one after another.', {
		duration: 1.4,
		easing: 'ease_out',
		range: sweep(0.35, 'ramp_up'),
		properties: props({ scale: 0.3, rotation: -90, opacity: 0 })
	}),
	preset('drop_in', 'Drop in', 'Words drop in from above, one after another.', {
		by: 'word',
		duration: 1.2,
		easing: 'ease_out',
		range: sweep(0.5, 'ramp_up'),
		properties: props({ y: -0.7, opacity: 0 })
	}),
	preset('wiggle', 'Wiggle', 'Every character jitters on its own, then settles.', {
		duration: 3,
		wiggle: { rate: 6, min: -1, max: 1, correlation: 0, seed: 7 },
		properties: props({ x: 0.02, y: 0.06, rotation: 3 })
	})
];

/** The id of the preset `a` is, or `null`. */
export const presetOf = (a: TextAnimation | null): string | null =>
	a ? (TEXT_ANIMATION_PRESETS.find((p) => sameAnimation(p.animation, a))?.id ?? null) : null;
