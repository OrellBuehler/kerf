import { beforeEach, describe, expect, test } from 'bun:test';
import { revertTo, setBlendMode, undo } from './api';
import { BLEND_MODES, blendModeOf } from './blend-modes';
import type { Clip, Timeline } from './types';

// The browser harness holds the contract `Project::set_blend_mode` does: one revision, `normal` written as no
// field at all, a clip on an audio track refused.

const clipOf = (t: Timeline, id: string): Clip => t.tracks.flatMap((tr) => tr.clips).find((c) => c.id === id)!;

beforeEach(async () => {
	await revertTo(0);
});

describe('blend modes', () => {
	test('the picker offers every mode of the core once, normal first', () => {
		// `BlendMode::ALL` in model.rs, by its serde names.
		expect(BLEND_MODES.map((m) => m.id)).toEqual([
			'normal',
			'multiply',
			'screen',
			'overlay',
			'hard_light',
			'add',
			'darken',
			'lighten',
			'difference',
			'exclusion'
		]);
		expect(new Set(BLEND_MODES.map((m) => m.label)).size).toBe(BLEND_MODES.length);
		expect(blendModeOf({})).toBe('normal');
	});

	test('a mode is set, normal clears the field, and an undo takes it back', async () => {
		const set = await setBlendMode('c1', 'screen');
		expect(clipOf(set, 'c1').blend_mode).toBe('screen');
		const back = await undo();
		expect(clipOf(back, 'c1').blend_mode).toBeUndefined();
		await setBlendMode('c1', 'multiply');
		const plain = await setBlendMode('c1', 'normal');
		expect('blend_mode' in clipOf(plain, 'c1')).toBe(false);
	});

	test('a clip of sound has no picture to blend', async () => {
		await expect(setBlendMode('c3', 'screen')).rejects.toThrow('video track');
	});
});
