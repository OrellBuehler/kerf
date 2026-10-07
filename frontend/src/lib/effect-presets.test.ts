import { describe, expect, test } from 'bun:test';
import {
	AUDIO_EFFECT_PRESETS,
	AUDIO_FX,
	VIDEO_EFFECT_PRESETS,
	VIDEO_FX,
	effectLabel,
	freshEffect
} from './effect-presets';

describe('effect presets', () => {
	test('each preset is keyed by the type of the effect it applies', () => {
		for (const p of [...VIDEO_EFFECT_PRESETS, ...AUDIO_EFFECT_PRESETS]) expect(p.effect.type).toBe(p.key);
	});

	test('the by-key maps list every preset, in order, once', () => {
		expect(Object.keys(VIDEO_FX)).toEqual(VIDEO_EFFECT_PRESETS.map((p) => p.key));
		expect(Object.keys(AUDIO_FX)).toEqual(AUDIO_EFFECT_PRESETS.map((p) => p.key));
		expect(new Set(Object.keys(VIDEO_FX)).size).toBe(VIDEO_EFFECT_PRESETS.length);
	});

	test('the defaults are the ones the Inspector has always started from', () => {
		expect(VIDEO_FX.blur).toEqual({ type: 'blur', sigma: 6 });
		expect(VIDEO_FX.chroma_key).toEqual({ type: 'chroma_key', color: 'green', similarity: 0.15, blend: 0.1 });
		expect(AUDIO_FX.compressor).toEqual({
			type: 'compressor',
			threshold_db: -18,
			ratio: 3,
			attack_ms: 20,
			release_ms: 250,
			makeup_db: 6
		});
		expect(AUDIO_FX.gate).toEqual({ type: 'gate', threshold_db: -45 });
	});

	test('every preset says what it is and what it does', () => {
		for (const p of [...VIDEO_EFFECT_PRESETS, ...AUDIO_EFFECT_PRESETS]) {
			expect(p.label.length).toBeGreaterThan(0);
			expect(p.hint.length).toBeGreaterThan(0);
		}
	});

	test('a fresh effect is a copy, so tuning a clip never edits the preset', () => {
		const a = freshEffect(VIDEO_EFFECT_PRESETS[0]);
		(a as { sigma: number }).sigma = 99;
		expect(VIDEO_FX.blur).toEqual({ type: 'blur', sigma: 6 });
	});

	test('a chain entry reads as words', () => {
		expect(effectLabel({ type: 'chroma_key' })).toBe('Chroma key');
		expect(effectLabel({ type: 'blur' })).toBe('Blur');
	});
});
