import { describe, expect, test } from 'bun:test';
import corpus from './fixtures/text-animation-corpus.json';
import { TEXT_ANIMATION_PRESETS, animationJson, parseAnimation } from './text-animation';
import { titleTexts, withTextAnimation } from './title-text';
import type { TextAnimation, TextAnimationPreset } from './types';

// The differential test for `withTextAnimation`, the animation `titleTexts` reads, the presets and the JSON an attribute
// holds, ports of kerf-core's `set_text_animation`, `text_animation_of`, `text_animation_presets` and
// `TextAnimation::to_attribute`: kerf-core writes down titles, an animation set on one of their texts and what it wrote (or
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

const { cases, presets } = corpus as unknown as { cases: Case[]; presets: TextAnimationPreset[] };

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
