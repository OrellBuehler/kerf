import { describe, expect, test } from 'bun:test';
import { blockNotice, told } from './gpu-block';
import type { GpuBlock } from './types';

const crash: GpuBlock = {
	kind: 'crash',
	reason: 'turned itself off: the last run ended while the GPU preview was starting',
	app_version: '0.31.0',
	at: 1,
	told: false
};
const mismatch: GpuBlock = { ...crash, kind: 'mismatch', adapter: 'gpu | Vulkan | driver 1 | ffmpeg' };

describe('the note the page makes of a GPU preview that turned itself off', () => {
	test('there is none without a block, and none for one it already said', () => {
		expect(blockNotice(null)).toBeNull();
		expect(blockNotice(undefined)).toBeNull();
		expect(blockNotice(told(crash))).toBeNull();
	});

	test('a crash and a mismatch each get their own words, and both say how to get it back', () => {
		const a = blockNotice(crash);
		const b = blockNotice(mismatch);
		expect(a?.message).toContain('turned off');
		expect(a?.description).toContain('Settings > Preview');
		expect(a?.description).toContain('usual way');
		expect(b?.message).toContain('off on this computer');
		expect(b?.description).toContain('driver');
		expect(b?.description).toContain('Settings > Preview');
		expect(a?.description).not.toBe(b?.description);
	});

	test('the notice is plain words: the technical reason is Settings\' to show', () => {
		for (const block of [crash, mismatch]) {
			const note = blockNotice(block)!;
			expect(note.description).not.toContain(block.reason);
			expect(note.description).not.toContain('Vulkan');
		}
	});

	test('saying it once writes the same block back, told', () => {
		expect(told(mismatch)).toEqual({ ...mismatch, told: true });
		expect(mismatch.told).toBe(false);
	});
});
