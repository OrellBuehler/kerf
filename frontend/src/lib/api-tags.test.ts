import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import type { Timeline } from './types';

// The browser harness stands in for kerf-app's `edit_asset_tags`, `list_tags` and `assemble_by_tag`. What it
// answers keeps the shape and the refusals of the backend's (`tags.ts` is the port of the spelling rules), or the
// screens built on it would be explored against behaviour the desktop app never shows. `?tags=1` seeds it: the
// interview is `location: Cumbuco`, the b-roll `location: Jeri`.

const g = globalThis as unknown as { window?: unknown; localStorage?: Storage };
const original = { window: g.window, localStorage: g.localStorage };
const store = new Map<string, string>();

let api: typeof import('./api');

beforeAll(async () => {
	g.window = { location: { search: '?tags=1' } };
	g.localStorage = {
		getItem: (k: string) => store.get(k) ?? null,
		setItem: (k: string, v: string) => void store.set(k, String(v)),
		removeItem: (k: string) => void store.delete(k)
	} as Storage;
	const specifier = './api?tags';
	api = (await import(specifier)) as typeof import('./api');
});

afterAll(() => {
	g.window = original.window;
	g.localStorage = original.localStorage;
	if (original.window === undefined) delete g.window;
	if (original.localStorage === undefined) delete g.localStorage;
});

const INTERVIEW = '11111111-1111-1111-1111-111111111111';
const BROLL = '22222222-2222-2222-2222-222222222222';
const SONG = '33333333-3333-3333-3333-333333333333';

const texts = (tags: { key?: string; value: string }[] | undefined) => (tags ?? []).map((t) => (t.key ? `${t.key}: ${t.value}` : t.value));
const tagsOf = async (id: string) => texts((await api.listAssets()).find((a) => a.id === id)?.tags);
const refusal = async (run: () => Promise<unknown>) => {
	try {
		await run();
	} catch (e) {
		return (e as Error).message;
	}
	return null;
};
const clipsOf = (tl: Timeline) => tl.tracks.flatMap((t) => t.clips);

describe('tagging assets', () => {
	test('the harness starts with the seeded tags and the song untagged', async () => {
		expect(await tagsOf(INTERVIEW)).toEqual(['location: Cumbuco', 'rider: Orell', 'interview']);
		expect(await tagsOf(BROLL)).toEqual(['location: Jeri', 'rider: Orell', 'jump']);
		const song = (await api.listAssets()).find((a) => a.id === SONG)!;
		expect('tags' in song).toBe(false);
	});

	test('an edit adds in the engine spelling, to many assets at once, and answers the assets', async () => {
		const done = await api.editAssetTags([SONG, BROLL, SONG], { add: [{ key: ' Mood ', value: '  Chill  out ' }, { value: 'JUMP' }] });
		expect(done.map((a) => a.id)).toEqual([SONG, BROLL]);
		expect(texts(done[0].tags)).toEqual(['mood: Chill out', 'JUMP']);
		expect(texts(done[1].tags)).toEqual(['location: Jeri', 'rider: Orell', 'jump', 'mood: Chill out']);
		expect(await tagsOf(SONG)).toEqual(['mood: Chill out', 'JUMP']);
	});

	test('an edit removes keys, then tags, then adds; and an asset left with none carries no tags member', async () => {
		await api.editAssetTags([SONG], { remove_keys: ['MOOD'], remove: [{ value: 'jump' }] });
		const song = (await api.listAssets()).find((a) => a.id === SONG)!;
		expect('tags' in song).toBe(false);
		await api.editAssetTags([BROLL], { remove_keys: ['mood'], remove: [{ key: 'rider', value: 'orell' }], add: [{ key: 'location', value: 'Atins' }] });
		expect(await tagsOf(BROLL)).toEqual(['location: Jeri', 'jump', 'location: Atins']);
		await api.editAssetTags([BROLL], { remove_keys: ['location'], add: [{ key: 'location', value: 'Jeri' }] });
		expect(await tagsOf(BROLL)).toEqual(['jump', 'location: Jeri']);
	});

	test('the answer is a copy: changing it changes nothing in the library', async () => {
		const [done] = await api.editAssetTags([BROLL], { add: [{ value: 'wave' }] });
		done.tags!.length = 0;
		expect(await tagsOf(BROLL)).toEqual(['jump', 'location: Jeri', 'wave']);
		await api.editAssetTags([BROLL], { remove: [{ value: 'wave' }] });
	});

	test('a refused edit changes nothing, and says so as the engine does', async () => {
		const before = [await tagsOf(INTERVIEW), await tagsOf(BROLL)];
		const unknown = '99999999-9999-9999-9999-999999999999';
		expect(await refusal(() => api.editAssetTags([INTERVIEW, unknown], { add: [{ value: 'wave' }] }))).toBe(`asset not found: ${unknown}`);
		expect(await refusal(() => api.editAssetTags([INTERVIEW], { add: [{ value: '  ' }] }))).toBe('invalid argument: a tag value cannot be empty');
		expect(await refusal(() => api.editAssetTags([INTERVIEW], { remove_keys: [' '] }))).toBe('invalid argument: a tag key cannot be empty');
		expect(await refusal(() => api.editAssetTags([], { add: [{ value: 'x' }] }))).toBe('invalid argument: name at least one asset to tag');
		expect(await refusal(() => api.editAssetTags([INTERVIEW], {}))).toBe('invalid argument: nothing to change: give tags to add or remove');
		const full = Array.from({ length: 70 }, (_, i) => ({ value: `t${i}` }));
		expect(await refusal(() => api.editAssetTags([INTERVIEW], { add: full }))).toBe('invalid argument: an asset holds at most 64 tags');
		expect([await tagsOf(INTERVIEW), await tagsOf(BROLL)]).toEqual(before);
	});

	test('the vocabulary counts assets and sorts', async () => {
		await api.editAssetTags([SONG], { add: [{ key: 'location', value: 'cumbuco' }, { value: 'Interview' }] });
		const summary = await api.listTags();
		expect(summary.keys.map((k) => [k.key, k.assets, k.values.map((v) => [v.value, v.assets])])).toEqual([
			['location', 3, [['Cumbuco', 2], ['Jeri', 1]]],
			['rider', 1, [['Orell', 1]]]
		]);
		expect(summary.plain.map((v) => [v.value, v.assets])).toEqual([['interview', 2], ['jump', 1]]);
		await api.editAssetTags([SONG], { remove_keys: ['location'], remove: [{ value: 'interview' }] });
	});
});

describe('assembling a cut by a tag', () => {
	test('lays the groups out after the cut, titled, as one revision', async () => {
		const before = await api.getTimeline();
		const revisions = (await api.getHistory()).length;
		const done = await api.assembleByTag({ key: 'Location', order: ['jeri', 'nowhere'] });

		expect((await api.getHistory()).length).toBe(revisions + 1);
		const label = (await api.getHistory()).at(-1)!.label;
		expect(label).toBe('Assemble by location (2 groups, 2 clips)');
		expect(done.groups.map((x) => x.value)).toEqual(['Jeri', 'Cumbuco']);
		expect(done.groups.map((x) => x.asset_ids)).toEqual([[BROLL], [INTERVIEW]]);
		expect(done.unmatched).toEqual(['nowhere']);

		// The starter cut's picture ends at 20.5 s; the first group begins there.
		expect(done.groups.map((x) => [x.start, x.end])).toEqual([
			[20.5, 65.5],
			[65.5, 185.5]
		]);
		expect(done.clip_ids).toHaveLength(2);
		const clips = clipsOf(done.timeline);
		expect(clips.length).toBe(clipsOf(before).length + 4);
		const placed = done.clip_ids.map((id) => clips.find((c) => c.id === id)!);
		expect(placed.map((c) => [c.asset_id, c.timeline_start, c.source_in, c.source_out])).toEqual([
			[BROLL, 20.5, 0, 45],
			[INTERVIEW, 65.5, 0, 120]
		]);

		// A three second title over the start of each group, on a track above the picture.
		expect(done.groups.every((x) => typeof x.title_clip_id === 'string')).toBe(true);
		const titles = done.groups.map((x) => clips.find((c) => c.id === x.title_clip_id)!);
		expect(titles.map((c) => [c.generator?.kind, c.timeline_start, c.source_out])).toEqual([
			['title', 20.5, 3],
			['title', 65.5, 3]
		]);
		const video = done.timeline.tracks.filter((t) => t.kind === 'video');
		expect(video).toHaveLength(2);
		expect(video[1].clips.every((c) => c.generator?.kind === 'title')).toBe(true);
		expect(JSON.stringify(titles[0].generator)).toContain('Jeri');
		expect('unmatched' in (await api.assembleByTag({ key: 'location', title: 'none' }))).toBe(false);
		await api.undo();
		await api.undo();
	});

	test('a card makes room before its group and the filter narrows the assets', async () => {
		const done = await api.assembleByTag({ key: 'location', title: 'card', title_seconds: 2, filter: [{ key: 'location' }, { value: 'jump' }] });
		expect(done.groups.map((x) => x.value)).toEqual(['Jeri']);
		expect(done.groups[0].asset_ids).toEqual([BROLL]);
		expect(done.groups[0].start).toBe(20.5);
		const clips = clipsOf(done.timeline);
		const card = clips.find((c) => c.id === done.groups[0].title_clip_id)!;
		expect([card.timeline_start, card.source_out]).toEqual([20.5, 2]);
		expect(clips.find((c) => c.id === done.clip_ids[0])!.timeline_start).toBe(22.5);
		expect(done.timeline.tracks.filter((t) => t.kind === 'video')).toHaveLength(1);
		await api.undo();
	});

	test('untagged assets come last without a title, and a request nothing answers is refused whole', async () => {
		await api.editAssetTags([INTERVIEW], { remove_keys: ['location'] });
		const done = await api.assembleByTag({ key: 'location', include_untagged: true });
		expect(done.groups.map((x) => x.value)).toEqual(['Jeri', null]);
		expect(done.groups[1].title_clip_id).toBeUndefined();
		await api.undo();
		await api.editAssetTags([INTERVIEW], { add: [{ key: 'location', value: 'Cumbuco' }] });

		const revisions = (await api.getHistory()).length;
		const clipCount = clipsOf(await api.getTimeline()).length;
		expect(await refusal(() => api.assembleByTag({ key: 'wind' }))).toBe('invalid argument: no asset has a "wind" tag');
		expect(await refusal(() => api.assembleByTag({ key: 'location', title_seconds: 61 }))).toContain('at most 60');
		expect((await api.getHistory()).length).toBe(revisions);
		expect(clipsOf(await api.getTimeline()).length).toBe(clipCount);
	});
});

describe('assembling in the harness as the engine does', () => {
	const ART = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 9"><rect width="8" height="4"/></svg>';
	const lane = (tl: Timeline, kind: 'video' | 'audio') => tl.tracks.find((t) => t.kind === kind)!.clips;
	const endOf = (tl: Timeline, kind: 'video' | 'audio') => Math.max(0, ...lane(tl, kind).map((c) => c.timeline_start + (c.source_out - c.source_in) / (c.speed ?? 1)));

	test('refuses a title it does not know, changing nothing', async () => {
		const revisions = (await api.getHistory()).length;
		const clipCount = clipsOf(await api.getTimeline()).length;
		const message = await refusal(() => api.assembleByTag({ key: 'location', title: 'banner' as 'card' }));
		expect(message).toContain('unknown title "banner"');
		expect(await refusal(() => api.assembleByTag({ key: 'location', sort_within: 'newest' as 'name' }))).toContain('unknown sort_within');
		expect((await api.getHistory()).length).toBe(revisions);
		expect(clipsOf(await api.getTimeline()).length).toBe(clipCount);
	});

	test('holds to 50 titles on the tracks, refusing the whole assembly', async () => {
		const room = 49;
		for (let i = 0; i < room; i++) await api.addTitleClip(ART, 500 + 2 * i, 1);
		const revisions = (await api.getHistory()).length;
		const before = JSON.stringify(await api.getTimeline());
		const message = await refusal(() => api.assembleByTag({ key: 'location' }));
		expect(message).toContain('at most 50 titles');
		expect((await api.getHistory()).length).toBe(revisions);
		expect(JSON.stringify(await api.getTimeline())).toBe(before);
		// With no title asked for the same assembly goes through: it adds none.
		await api.assembleByTag({ key: 'location', title: 'none' });
		await api.undo();
		for (let i = 0; i < room; i++) await api.undo();
	});

	test('starts at the end of the picture, not of a long bed on the audio track', async () => {
		for (let i = 0; i < 3; i++) await api.addAssetAudio(SONG);
		const before = await api.getTimeline();
		expect(endOf(before, 'audio')).toBeGreaterThan(endOf(before, 'video'));
		// The song joins the plan as a sound alone: it follows the picture, and the bed does not push anything out.
		await api.editAssetTags([SONG], { add: [{ key: 'location', value: 'Prea' }] });
		const done = await api.assembleByTag({ key: 'location', title: 'none' });
		expect(done.groups.map((x) => x.value)).toEqual(['Cumbuco', 'Jeri', 'Prea']);
		expect(done.groups[0].start).toBe(endOf(before, 'video'));
		await api.editAssetTags([SONG], { remove_keys: ['location'] });
		for (let i = 0; i < 4; i++) await api.undo();
	});
});

describe('the ask-on-import preference', () => {
	test('is on until it is switched off, and a reset puts it back on', async () => {
		expect((await api.getSettings()).ask_tags_on_import).toBe(true);
		expect((await api.setSettings({ ask_tags_on_import: false })).ask_tags_on_import).toBe(false);
		expect((await api.getSettings()).ask_tags_on_import).toBe(false);
		expect((await api.setSettings({ ask_tags_on_import: true })).ask_tags_on_import).toBe(true);
		await api.setSettings({ ask_tags_on_import: false });
		expect((await api.resetSettings()).ask_tags_on_import).toBe(true);
	});
});
