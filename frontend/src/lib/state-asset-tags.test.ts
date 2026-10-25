import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import './test-runes';
import { editAssetTags, listAssets } from './api';
import { GENERATED_ASSET } from './types';

// The editor's side of the media bin's multi-selection and of tagging, over the browser harness's three assets
// (the interview, the b-roll and the song). Nothing here is reactive (`test-runes`), so a test reads `selectedAssetIds`,
// not the derived set the bin draws from.

const { editor } = await import('./state.svelte');
const mods = { shiftKey: false, ctrlKey: false, metaKey: false };
const ctrl = { ...mods, ctrlKey: true };
const shift = { ...mods, shiftKey: true };

const tagsOf = (id: string) => (editor.assets.find((a) => a.id === id)?.tags ?? []).map((t) => (t.key ? `${t.key}: ${t.value}` : t.value));
const ids = () => editor.assets.map((a) => a.id);

beforeEach(async () => {
	await editor.load();
	await editor.select(ids()[0]);
});
afterEach(async () => {
	// The harness's assets outlive a test file: leave them untagged.
	for (const a of await listAssets()) if (a.tags?.length) await editAssetTags([a.id], { remove: a.tags });
	await editor.load();
});

describe('selecting assets in the bin', () => {
	test('a plain pick selects one and describes it', async () => {
		const [a, b] = ids();
		await editor.pickAsset(b, mods, ids());
		expect(editor.selectedAssetIds).toEqual([b]);
		expect(editor.selectedAssetId).toBe(b);
		expect(editor.selectedMetadata?.asset.id).toBe(b);
		expect(editor.selectedAssetIds).not.toContain(a);
	});

	test('Ctrl adds an asset and makes it the primary the Inspector describes', async () => {
		const [a, b] = ids();
		await editor.pickAsset(b, ctrl, ids());
		expect(editor.selectedAssetIds).toEqual([a, b]);
		expect(editor.selectedAssetId).toBe(b);
		expect(editor.selectedMetadata?.asset.id).toBe(b);
	});

	test('Ctrl on a selected asset takes it out, and the primary falls back to what is left', async () => {
		const [a, b] = ids();
		await editor.pickAsset(b, ctrl, ids());
		await editor.pickAsset(b, ctrl, ids());
		expect(editor.selectedAssetIds).toEqual([a]);
		expect(editor.selectedAssetId).toBe(a);
		expect(editor.selectedMetadata?.asset.id).toBe(a);
	});

	test('Ctrl on the only selected asset leaves nothing selected and the Inspector empty', async () => {
		const [a] = ids();
		await editor.pickAsset(a, ctrl, ids());
		expect(editor.selectedAssetIds).toEqual([]);
		expect(editor.selectedAssetId).toBeNull();
		expect(editor.selectedMetadata).toBeNull();
	});

	test('Shift selects the run between the primary and the asset clicked', async () => {
		const all = ids();
		const last = all[all.length - 1];
		await editor.pickAsset(last, shift, all);
		expect([...editor.selectedAssetIds].sort()).toEqual([...all].sort());
		expect(editor.selectedAssetId).toBe(last);
	});

	test('picking in the order the bin shows leaves a hidden asset out of the run', async () => {
		const [a, , c] = ids();
		await editor.pickAsset(c, shift, [a, c]);
		expect([...editor.selectedAssetIds].sort()).toEqual([a, c].sort());
	});

	test('a click elsewhere (a clip on the timeline) selects that asset alone', async () => {
		const [a, b] = ids();
		await editor.pickAsset(b, ctrl, ids());
		await editor.select(a);
		expect(editor.selectedAssetIds).toEqual([a]);
	});

	test('Escape shrinks several to the primary, and does nothing to one', async () => {
		const [, b] = ids();
		await editor.pickAsset(b, ctrl, ids());
		expect(editor.collapseAssetSelection()).toBe(true);
		expect(editor.selectedAssetIds).toEqual([b]);
		expect(editor.selectedAssetId).toBe(b);
		expect(editor.collapseAssetSelection()).toBe(false);
	});

	test('a filter that hides part of the selection takes those out', async () => {
		const [a, b] = ids();
		await editor.pickAsset(b, ctrl, ids());
		editor.restrictAssetSelection(new Set([b]));
		expect(editor.selectedAssetIds).toEqual([b]);
		expect(editor.selectedAssetId).toBe(b);
		editor.restrictAssetSelection(new Set([a]));
		expect(editor.selectedAssetIds).toEqual([b]);
	});

	test('a title on a track is no asset: selecting it clears the selection', async () => {
		await editor.select(GENERATED_ASSET);
		expect(editor.selectedAssetIds).toEqual([]);
		expect(editor.selectedAssetId).toBeNull();
	});
});

describe('tagging assets', () => {
	test('one call tags every asset and the bin follows', async () => {
		const [a, b, c] = ids();
		const done = await editor.tagAssets([a, b], { add: [{ key: 'location', value: 'Cumbuco' }, { value: 'jump' }] });
		expect(done.map((x) => x.id)).toEqual([a, b]);
		expect(tagsOf(a)).toEqual(['location: Cumbuco', 'jump']);
		expect(tagsOf(b)).toEqual(['location: Cumbuco', 'jump']);
		expect(tagsOf(c)).toEqual([]);
	});

	test('the Inspector’s asset is refreshed with the same tags', async () => {
		const [a] = ids();
		await editor.tagAssets([a], { add: [{ value: 'jump' }] });
		expect(editor.selectedMetadata?.asset.tags).toEqual([{ value: 'jump' }]);
	});

	test('removing and adding in one edit applies in the engine’s order', async () => {
		const [a] = ids();
		await editor.tagAssets([a], { add: [{ key: 'location', value: 'Cumbuco' }] });
		await editor.tagAssets([a], { remove_keys: ['location'], add: [{ key: 'location', value: 'Jeri' }] });
		expect(tagsOf(a)).toEqual(['location: Jeri']);
		await editor.tagAssets([a], { remove: [{ key: 'location', value: 'jeri' }] });
		expect(tagsOf(a)).toEqual([]);
	});

	test('a refusal changes nothing, for any of the assets', async () => {
		const [a, b] = ids();
		await editor.tagAssets([a], { add: [{ value: 'jump' }] });
		await expect(editor.tagAssets([a, b], { add: [{ key: 'x'.repeat(33), value: 'y' }] })).rejects.toThrow(/key is at most 32/);
		await expect(editor.tagAssets([a, 'no-such-asset'], { add: [{ value: 'new' }] })).rejects.toThrow();
		expect(tagsOf(a)).toEqual(['jump']);
		expect(tagsOf(b)).toEqual([]);
	});

	test('the vocabulary is read again on request, counting an asset once per value', async () => {
		const [a, b] = ids();
		await editor.tagAssets([a, b], { add: [{ key: 'location', value: 'Cumbuco' }, { value: 'jump' }] });
		await editor.refreshTagVocabulary();
		expect(editor.tagVocabulary).toEqual({
			keys: [{ key: 'location', assets: 2, values: [{ value: 'Cumbuco', assets: 2 }] }],
			plain: [{ value: 'jump', assets: 2 }]
		});
		await editor.tagAssets([a, b], { remove: [{ value: 'jump' }] });
		await editor.refreshTagVocabulary();
		expect(editor.tagVocabulary.plain).toEqual([]);
	});

	test('tags an agent put on behind our back show up when the assets are read again', async () => {
		const [a] = ids();
		await editAssetTags([a], { add: [{ key: 'rider', value: 'Orell' }] });
		expect(tagsOf(a)).toEqual([]);
		await editor.refreshAssets();
		expect(tagsOf(a)).toEqual(['rider: Orell']);
	});
});
