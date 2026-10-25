import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, spyOn, test } from 'bun:test';
import './test-runes';
import type { Asset, TagSummary } from './types';

// The editor's reads of the library and of the tag vocabulary come back in any order. A read that a local write
// overtook (a tag edit landed while `list_assets` was on its way) must not put the old tags back, an older read must
// not overwrite a newer one, and when the asset the Inspector is on goes away the Inspector moves to one that is left.
// `listAssets` and `listTags` are held here so a test decides which answer comes back when.

const api = await import('./api');
const { editor } = await import('./state.svelte');
const held = { assets: [] as Promise<Asset[]>[], tags: [] as Promise<TagSummary>[] };
const real = { listAssets: api.listAssets, listTags: api.listTags };
// Held for this file's tests only, by spying on the two reads rather than mocking the module: a module mock made while
// the files load would stand in for `./api` in the files that run before this one.
let spies: { mockRestore(): void }[] = [];
beforeAll(() => {
	spies = [
		spyOn(api, 'listAssets').mockImplementation(() => held.assets.shift() ?? real.listAssets()),
		spyOn(api, 'listTags').mockImplementation(() => held.tags.shift() ?? real.listTags())
	];
});
afterAll(() => spies.forEach((s) => s.mockRestore()));
const mods = { shiftKey: false, ctrlKey: false, metaKey: false };
const ctrl = { ...mods, ctrlKey: true };

function deferred<T>() {
	let resolve!: (v: T) => void;
	let reject!: (e: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}
const settle = () => new Promise((r) => setTimeout(r, 0));
const tagsOf = (id: string) => (editor.assets.find((a) => a.id === id)?.tags ?? []).map((t) => (t.key ? `${t.key}: ${t.value}` : t.value));
const copy = (assets: readonly Asset[]): Asset[] => JSON.parse(JSON.stringify(assets));

beforeEach(async () => {
	held.assets.length = 0;
	held.tags.length = 0;
	await editor.load();
	await editor.select(editor.assets[0].id);
});
afterEach(async () => {
	held.assets.length = 0;
	held.tags.length = 0;
	for (const a of await real.listAssets()) if (a.tags?.length) await api.editAssetTags([a.id], { remove: a.tags });
	await editor.load();
});

describe('refreshAssets', () => {
	test('a read that a tag edit overtook is dropped, so it cannot put the old tags back', async () => {
		const [a] = editor.assets.map((x) => x.id);
		const stale = deferred<Asset[]>();
		held.assets.push(stale.promise);
		const refresh = editor.refreshAssets();
		await editor.tagAssets([a], { add: [{ value: 'jump' }] });
		expect(tagsOf(a)).toEqual(['jump']);
		stale.resolve(copy(editor.assets).map((x) => ({ ...x, tags: undefined })));
		await refresh;
		expect(tagsOf(a)).toEqual(['jump']);
	});

	test('a read nothing overtook lands, tags an agent put on included', async () => {
		const [a] = editor.assets.map((x) => x.id);
		const answer = deferred<Asset[]>();
		held.assets.push(answer.promise);
		const refresh = editor.refreshAssets();
		answer.resolve(copy(editor.assets).map((x) => (x.id === a ? { ...x, tags: [{ key: 'rider', value: 'Orell' }] } : x)));
		await refresh;
		expect(tagsOf(a)).toEqual(['rider: Orell']);
	});

	test('when the Inspector’s asset goes, it passes to another asset of the selection', async () => {
		const [a, b] = editor.assets.map((x) => x.id);
		await editor.pickAsset(b, ctrl, editor.assets.map((x) => x.id));
		expect(editor.selectedAssetId).toBe(b);
		held.assets.push(Promise.resolve(copy(editor.assets).filter((x) => x.id !== b)));
		await editor.refreshAssets();
		expect(editor.selectedAssetIds).toEqual([a]);
		expect(editor.selectedAssetId).toBe(a);
		expect(editor.selectedMetadata?.asset.id).toBe(a);
	});

	test('when every selected asset goes, the Inspector is cleared', async () => {
		const [a] = editor.assets.map((x) => x.id);
		held.assets.push(Promise.resolve(copy(editor.assets).filter((x) => x.id !== a)));
		await editor.refreshAssets();
		expect(editor.selectedAssetIds).toEqual([]);
		expect(editor.selectedAssetId).toBeNull();
		expect(editor.selectedMetadata).toBeNull();
	});

	test('an asset that goes but was not selected leaves the selection alone', async () => {
		const [a, , c] = editor.assets.map((x) => x.id);
		held.assets.push(Promise.resolve(copy(editor.assets).filter((x) => x.id !== c)));
		await editor.refreshAssets();
		expect(editor.selectedAssetIds).toEqual([a]);
		expect(editor.selectedAssetId).toBe(a);
	});
});

describe('refreshTagVocabulary', () => {
	const vocab = (value: string): TagSummary => ({ keys: [], plain: [{ value, assets: 1 }] });

	test('an older answer arriving late does not overwrite a newer one', async () => {
		const older = deferred<TagSummary>();
		const newer = deferred<TagSummary>();
		held.tags.push(older.promise, newer.promise);
		const first = editor.refreshTagVocabulary();
		const second = editor.refreshTagVocabulary();
		newer.resolve(vocab('new'));
		await second;
		older.resolve(vocab('old'));
		await first;
		await settle();
		expect(editor.tagVocabulary.plain[0].value).toBe('new');
	});

	test('a newer read that fails does not stop an older one from landing', async () => {
		const older = deferred<TagSummary>();
		const newer = deferred<TagSummary>();
		held.tags.push(older.promise, newer.promise);
		const first = editor.refreshTagVocabulary();
		const second = editor.refreshTagVocabulary();
		newer.reject(new Error('backend busy'));
		await second;
		older.resolve(vocab('old'));
		await first;
		expect(editor.tagVocabulary.plain[0].value).toBe('old');
	});
});
