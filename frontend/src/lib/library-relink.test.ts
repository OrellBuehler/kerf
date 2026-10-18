import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import type { Relinked, RelinkCheck } from './types';

// The relink flow as the bin's menu, the Inspector and the Relink dialog run it: pick a file, ask what a
// different recording would cost, relink, say what happened. The backend's dry run and relink are faked (the
// harness is shared with other files, and relinking changes it); the picker, the question and the toasts stand
// in for what only a window can do.

const real = { ...(await import('./api')) };

const asked: { message: string; title?: string }[] = [];
const shown: { kind: string; text: string }[] = [];
const relinked: string[] = [];
let picked: string | null = null;
let answer = true;
let check: RelinkCheck = { analysis_kept: true, has_analysis: true };
let refusal: string | null = null;

mock.module('./api', () => ({
	...real,
	pickRelinkPath: async () => picked,
	confirmAction: async (message: string, title?: string) => {
		asked.push({ message, title });
		return answer;
	},
	checkRelink: async () => {
		if (refusal) throw new Error(refusal);
		return check;
	},
	relinkAsset: async (assetId: string, path: string): Promise<Relinked> => {
		relinked.push(path);
		const asset = (await real.getAssetMetadata(assetId)).asset;
		return { asset: { ...asset, path }, analysis_kept: check.analysis_kept, previous_path: asset.path };
	}
}));
const note = (kind: string) => (text: string) => void shown.push({ kind, text });
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), { success: note('success'), warning: note('warning'), error: note('error'), info: note('info'), dismiss: () => {} })
}));
afterAll(() => {
	mock.module('./api', () => real);
});

const { relinkMediaFlow } = await import('./library-actions');

const BROLL = '22222222-2222-2222-2222-222222222222';
const flow = () => relinkMediaFlow({ id: BROLL, name: 'broll.mp4' });

beforeEach(() => {
	asked.length = 0;
	shown.length = 0;
	relinked.length = 0;
	answer = true;
	picked = '/restored/broll.mp4';
	check = { analysis_kept: true, has_analysis: true };
	refusal = null;
});

describe('relinking to another file', () => {
	test('the same media under a new place goes without a question', async () => {
		expect(await flow()).toBe(true);
		expect(asked).toEqual([]);
		expect(relinked).toEqual(['/restored/broll.mp4']);
		expect(shown.at(-1)).toEqual({ kind: 'success', text: 'Relinked broll.mp4' });
	});

	test('a different recording says what it drops before anything changes, and Cancel changes nothing', async () => {
		check = { analysis_kept: false, has_analysis: true };
		answer = false;
		expect(await flow()).toBe(false);
		expect(asked).toHaveLength(1);
		expect(asked[0].title).toContain('different recording');
		expect(asked[0].message).toContain('analysis');
		expect(relinked).toEqual([]);
		expect(shown).toEqual([]);
	});

	test('and once agreed it relinks and says the analysis was dropped', async () => {
		check = { analysis_kept: false, has_analysis: true };
		expect(await flow()).toBe(true);
		expect(asked).toHaveLength(1);
		expect(relinked).toEqual(['/restored/broll.mp4']);
		expect(shown.at(-1)?.text).toContain('analysis was dropped');
	});

	test('an asset with no analysis has nothing to lose, so nothing is asked', async () => {
		check = { analysis_kept: false, has_analysis: false };
		expect(await flow()).toBe(true);
		expect(asked).toEqual([]);
	});

	test('a file that cannot stand in is refused by the check, with nothing asked or changed', async () => {
		refusal = 'invalid argument: that file is a still image but "broll.mp4" is picture only — pick the same media';
		expect(await flow()).toBe(false);
		expect(asked).toEqual([]);
		expect(relinked).toEqual([]);
		expect(shown.at(-1)?.kind).toBe('error');
	});

	test('a cancelled picker does nothing', async () => {
		picked = null;
		expect(await flow()).toBe(false);
		expect(asked).toEqual([]);
		expect(shown).toEqual([]);
	});
});

describe('the harness check', () => {
	test('judges by file name like its relink, and refuses what the relink would', async () => {
		expect(await real.checkRelink(BROLL, '/restored/broll.mp4')).toEqual({ analysis_kept: true, has_analysis: true });
		expect(await real.checkRelink(BROLL, '/restored/another-take.mp4')).toEqual({ analysis_kept: false, has_analysis: true });
		await expect(real.checkRelink(BROLL, '/restored/still.png')).rejects.toThrow('still image');
		await expect(real.checkRelink(BROLL, 'relative.mp4')).rejects.toThrow('absolute path');
		await expect(real.checkRelink('nope', '/a.mp4')).rejects.toThrow('asset not found');
	});
});
