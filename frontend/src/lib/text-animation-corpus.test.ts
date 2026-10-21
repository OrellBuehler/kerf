import { describe, expect, test } from 'bun:test';
import corpus from './fixtures/text-animation-corpus.json';
import {
	CURVE_PRESETS,
	TEXT_ANIMATION_PRESETS,
	animationJson,
	curveIdOf,
	curveJson,
	hasMotion,
	motionCurveOf,
	parseAnimation,
	presetOf,
	withMotionCurve
} from './text-animation';
import { titleTexts, withTextAnimation } from './title-text';
import type { AnimationCurve, AnimationCurvePreset, TextAnimation, TextAnimationPreset } from './types';

// The differential test for `withTextAnimation`, the animation `titleTexts` reads, the presets, the JSON an attribute
// holds and its reading, ports of kerf-core's `set_text_animation`, `text_animation_of`, `text_animation_presets`,
// `TextAnimation::to_attribute` and `from_attribute`, the curve presets and `with_motion_curve` / `motion_curve`: kerf-core writes down titles, an animation set on one of their texts and what it wrote (or
// refused) and reads back (`svg/text_animation.rs` → `fixtures/text-animation-corpus.json`), and every case is replayed
// here. Regenerate with `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- text_animation_corpus` after an
// intended change.

interface Case {
	name: string;
	svg: string;
	element: string;
	animation: TextAnimation | null;
	expect: { ok: { svg: string; read: TextAnimation | null } } | { err: string };
}

interface MotionRead {
	mixed: boolean;
	curve: AnimationCurve | null;
}

interface MotionCase {
	name: string;
	animation: TextAnimation;
	curve?: AnimationCurve | null;
	expect?: { ok: { animation: TextAnimation; motion: MotionRead } } | { err: string };
	read?: MotionRead;
}

const { cases, presets, curves, parses, motion } = corpus as unknown as {
	cases: Case[];
	presets: TextAnimationPreset[];
	curves: AnimationCurvePreset[];
	parses: { name: string; raw: string; read: TextAnimation | null }[];
	motion: MotionCase[];
};

describe('the text animation corpus', () => {
	test('has answers and refusals', () => {
		expect(cases.filter((c) => 'ok' in c.expect).length).toBeGreaterThanOrEqual(6);
		expect(cases.filter((c) => 'err' in c.expect).length).toBeGreaterThanOrEqual(4);
	});

	for (const c of cases) {
		test(c.name, () => {
			if ('err' in c.expect) {
				expect(() => withTextAnimation(c.svg, c.element, c.animation)).toThrow(c.expect.err);
				return;
			}
			const out = withTextAnimation(c.svg, c.element, c.animation);
			expect(out).toBe(c.expect.ok.svg);
			expect(titleTexts(out).find((t) => t.id === c.element)?.animation ?? null).toEqual(c.expect.ok.read);
		});
	}
});

describe('the presets', () => {
	test('are the eight kerf-core lists, in its order', () => {
		expect(TEXT_ANIMATION_PRESETS.map((p) => p.id)).toEqual(presets.map((p) => p.id));
		for (const p of presets) expect(TEXT_ANIMATION_PRESETS.find((q) => q.id === p.id)).toEqual(p);
	});

	test('are written as the engine writes them and read back whole', () => {
		for (const p of presets) {
			const json = animationJson(p.animation);
			expect(parseAnimation(json)).toEqual(p.animation);
			expect(JSON.parse(json)).toEqual(p.animation);
		}
	});
});

describe('reading an attribute', () => {
	test('has answers and refusals', () => {
		expect(parses.filter((c) => c.read).length).toBeGreaterThanOrEqual(4);
		expect(parses.filter((c) => !c.read).length).toBeGreaterThanOrEqual(15);
	});

	for (const c of parses) {
		test(c.name, () => {
			expect(parseAnimation(c.raw)).toEqual(c.read);
		});
	}
});

describe('the curves', () => {
	test('are the presets kerf-core lists, in its order, and are written as the engine writes them', () => {
		expect(CURVE_PRESETS.map((p) => p.id)).toEqual(curves.map((p) => p.id));
		for (const p of curves) {
			expect(CURVE_PRESETS.find((q) => q.id === p.id)).toEqual(p);
			if (p.curve) expect(JSON.parse(curveJson(p.curve))).toEqual(p.curve);
		}
	});

	test('a preset with its curves changed is still that preset, and a curve reads back as the one it is', () => {
		const rise = TEXT_ANIMATION_PRESETS.find((p) => p.id === 'rise')!.animation;
		const sprung = withMotionCurve(rise, CURVE_PRESETS.find((p) => p.id === 'spring')!.curve);
		expect(presetOf(sprung)).toBe('rise');
		expect(curveIdOf(sprung)).toBe('spring');
		expect(curveIdOf(rise)).toBe('linear');
		expect(curveIdOf(withMotionCurve(rise, { kind: 'spring', bounce: 0.31 }))).toBeNull();
		const spin = TEXT_ANIMATION_PRESETS.find((p) => p.id === 'spin_in')!.animation;
		const half = withMotionCurve(spin, { kind: 'spring', bounce: 0.5 });
		expect(curveIdOf({ ...half, curves: { scale: half.curves!.scale } })).toBeNull();
		expect(hasMotion(spin) && !hasMotion(TEXT_ANIMATION_PRESETS.find((p) => p.id === 'fade_in')!.animation)).toBe(true);
	});
});

describe('a motion curve', () => {
	test('has answers and refusals', () => {
		expect(motion.filter((c) => c.expect && 'ok' in c.expect).length).toBeGreaterThanOrEqual(30);
		expect(motion.filter((c) => c.expect && 'err' in c.expect).length).toBeGreaterThanOrEqual(10);
		expect(motion.filter((c) => c.read).length).toBeGreaterThanOrEqual(3);
	});

	for (const c of motion) {
		test(c.name, () => {
			if (c.read) {
				expect(motionCurveOf(c.animation)).toEqual(c.read);
				return;
			}
			const expect_ = c.expect!;
			if ('err' in expect_) {
				expect(() => withMotionCurve(c.animation, c.curve ?? null)).toThrow(expect_.err);
				return;
			}
			const out = withMotionCurve(c.animation, c.curve ?? null);
			expect(out).toEqual(expect_.ok.animation);
			expect(animationJson(out)).toBe(animationJson(expect_.ok.animation));
			expect(motionCurveOf(out)).toEqual(expect_.ok.motion);
		});
	}
});
