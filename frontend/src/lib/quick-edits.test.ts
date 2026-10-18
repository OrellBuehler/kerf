import { describe, expect, test } from 'bun:test';
import { cutSignature, isLocalPreset, quickTarget, silenceOutcome, targetNote } from './quick-edits';
import { PRESETS } from './components/editor/data';
import type { Timeline } from './types';

const assets = [
	{ id: 'a', name: 'interview.mp4' },
	{ id: 'b', name: 'broll.mp4' },
	{ id: 'm', name: 'music.mp3' }
];
const cut = (...ids: string[]): Timeline => ({
	tracks: [{ id: 'v1', kind: 'video', name: 'V1', clips: ids.map((asset_id, i) => ({ id: `c${i}`, asset_id, source_in: 0, source_out: 5, timeline_start: i * 5, volume: 1, fade_in: 0, fade_out: 0 })) }]
});

describe('which presets Kerf runs itself', () => {
	test('assembling a rough cut is an agent task — it used to run the same silence removal as Remove silences', () => {
		expect(isLocalPreset('Remove silences')).toBe(true);
		expect(isLocalPreset('Assemble rough cut')).toBe(false);
		expect(isLocalPreset('Find best 60s')).toBe(false);
	});

	test('every preset is one or the other and the four local ones are all offered', () => {
		expect(PRESETS.filter(isLocalPreset)).toEqual(['Remove silences', 'Cut to the beat', 'Frame for the delivery', 'Caption the cut']);
	});
});

describe('the asset a quick edit acts on', () => {
	test('the selected clip’s footage first, then the library selection, then the cut, then the first asset', () => {
		const timeline = cut('b');
		expect(quickTarget({ clipAssetId: 'm', assetId: 'a', assets, timeline })?.id).toBe('m');
		expect(quickTarget({ clipAssetId: null, assetId: 'a', assets, timeline })?.id).toBe('a');
		expect(quickTarget({ clipAssetId: undefined, assetId: null, assets, timeline })?.id).toBe('b');
		expect(quickTarget({ clipAssetId: null, assetId: null, assets, timeline: cut() })?.id).toBe('a');
	});

	test('an id that is no longer in the library falls through, and an empty library has none', () => {
		expect(quickTarget({ clipAssetId: 'gone', assetId: 'gone', assets, timeline: cut() })?.id).toBe('a');
		expect(quickTarget({ clipAssetId: null, assetId: null, assets: [], timeline: cut() })).toBeNull();
	});
});

describe('what the panel says about it', () => {
	test('the one-asset edit names the asset; the rest say what they act on', () => {
		expect(targetNote('Remove silences', { id: 'a', name: 'interview.mp4' })).toBe('Acts on interview.mp4');
		expect(targetNote('Remove silences', null)).toBe('Import media first');
		expect(targetNote('Caption the cut', null)).toBe('Acts on the whole cut');
		expect(targetNote('Assemble rough cut', null)).toContain('agent');
	});

	test('a silence removal that changed nothing says so instead of claiming it removed something', () => {
		const t = { id: 'a', name: 'interview.mp4' };
		expect(silenceOutcome(t, true)).toEqual({ kind: 'success', text: 'Removed the silences of interview.mp4 from the cut' });
		expect(silenceOutcome(t, false).kind).toBe('info');
	});

	test('the signature tells an edit from a no-op', () => {
		expect(cutSignature(cut('a', 'b'))).toBe(cutSignature(cut('a', 'b')));
		expect(cutSignature(cut('a', 'b'))).not.toBe(cutSignature(cut('a')));
	});
});
