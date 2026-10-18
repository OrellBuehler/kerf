import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import type { Timeline } from './types';

// The browser harness stands in for kerf-app's library, staged-proposal, recent-project and paging
// commands. What it answers must keep the shape and the wording the backend's do (`MissingAsset`,
// `Relinked`, `AssetRemoval`, `SubsetApplied`, …), or the screens built on it would be explored against
// behaviour the desktop app never shows. `?missing=1&staged=1&recent=1&exists=1` are the flags that seed it.

const g = globalThis as unknown as { window?: unknown; localStorage?: Storage };
const original = { window: g.window, localStorage: g.localStorage };
const store = new Map<string, string>();

// A fresh copy of the harness, read with the flags set (they are read when it loads).
let api: typeof import('./api');

beforeAll(async () => {
	g.window = { location: { search: '?missing=1&staged=1&recent=1&exists=1' } };
	g.localStorage = {
		getItem: (k: string) => store.get(k) ?? null,
		setItem: (k: string, v: string) => void store.set(k, String(v)),
		removeItem: (k: string) => void store.delete(k)
	} as Storage;
	const specifier = './api?library';
	api = (await import(specifier)) as typeof import('./api');
});

afterAll(() => {
	g.window = original.window;
	g.localStorage = original.localStorage;
	if (original.window === undefined) delete g.window;
	if (original.localStorage === undefined) delete g.localStorage;
});

const BROLL = '22222222-2222-2222-2222-222222222222';
const SONG = '33333333-3333-3333-3333-333333333333';
const INTERVIEW = '11111111-1111-1111-1111-111111111111';

const clipsOf = (tl: Timeline, asset: string) => tl.tracks.flatMap((t) => t.clips).filter((c) => c.asset_id === asset);

describe('a staged proposal', () => {
	test('every diff entry carries the id core would give it', async () => {
		const staged = (await api.getStagedEdit())!;
		expect(staged.diff.entries.length).toBeGreaterThanOrEqual(3);
		for (const e of staged.diff.entries) {
			const target = e.clip_id ?? e.overlay_id ?? e.marker_id ?? e.track_id;
			expect(e.id).toBe(`${e.kind}:${target}`);
		}
		expect(new Set(staged.diff.entries.map((e) => e.id)).size).toBe(staged.diff.entries.length);
		expect(staged.undo_depth).toBe(staged.edits.length);
	});

	test('undo takes back the last step, then a discard can be taken back too', async () => {
		const before = (await api.getStagedEdit())!;
		const undone = await api.undoStagedEdit();
		expect(undone.label).toBe(before.edits[before.edits.length - 1]);
		expect(undone.staged.edits).toEqual(before.edits.slice(0, -1));
		expect(undone.staged.undo_depth).toBe(before.edits.length - 1);
		expect(undone.staged.diff.entries).toHaveLength(before.diff.entries.length - 1);

		expect(await api.discardedStagedEdit()).toBeNull();
		await api.discardStagedEdit();
		expect(await api.getStagedEdit()).toBeNull();
		expect((await api.discardedStagedEdit())?.edits).toEqual(undone.staged.edits);
		const back = await api.restoreDiscardedEdits();
		expect(back.edits).toEqual(undone.staged.edits);
		expect(await api.discardedStagedEdit()).toBeNull();
		await expect(api.restoreDiscardedEdits()).rejects.toThrow('already pending');
	});

	test('a partial accept lays the picked change and the sound linked to it, and the rest stays open', async () => {
		const staged = (await api.getStagedEdit())!;
		const total = staged.diff.entries.length;
		const trim = staged.diff.entries.find((e) => e.kind === 'clip_retrimmed' && e.track_id === 'v1')!;
		const done = await api.applyStagedSubset([trim.id!]);
		expect(done.applied).toBe(2);
		expect(done.auto_included).toHaveLength(1);
		expect(done.auto_included[0].because).toContain('linked');
		expect(done.remaining).toBe(total - 2);
		expect(done.closed).toBe(false);
		expect(done.message).toBe(
			`Applied 2 of ${total} changes — 1 came with the ones you picked (${done.auto_included[0].summary}); ${total - 2} left to review.`
		);
		const rest = (await api.getStagedEdit())!;
		expect(rest.diff.entries.map((e) => e.id)).not.toContain(trim.id);
		expect(rest.stale).toBe(false);
		expect(rest.undo_depth).toBe(0);
	});

	test('picking nothing, or something that is not in the proposal, is refused', async () => {
		await expect(api.applyStagedSubset([])).rejects.toThrow('pick at least one change');
		await expect(api.applyStagedSubset(['clip_moved:nope'])).rejects.toThrow('no change clip_moved:nope');
	});

	test('picking the rest closes the proposal and finishes its task', async () => {
		const rest = (await api.getStagedEdit())!;
		const done = await api.applyStagedSubset(rest.diff.entries.map((e) => e.id!));
		expect(done.closed).toBe(true);
		expect(done.remaining).toBe(0);
		expect(await api.getStagedEdit()).toBeNull();
	});
});

describe('missing media', () => {
	test('lists the assets whose file is gone, with how many clips use each', async () => {
		const missing = await api.missingAssets();
		expect(missing.map((m) => m.asset_id).sort()).toEqual([BROLL, SONG].sort());
		const broll = missing.find((m) => m.asset_id === BROLL)!;
		expect(broll).toMatchObject({ name: 'broll.mp4', path: '/samples/broll.mp4', clips: 1 });
		expect(missing.find((m) => m.asset_id === SONG)!.clips).toBe(0);
	});

	test('a file of another kind is refused in the backend’s words', async () => {
		await expect(api.relinkAsset(BROLL, '/restored/broll.mp3')).rejects.toThrow(
			'that file is sound only but "broll.mp4" is picture only — pick the same media'
		);
		await expect(api.relinkAsset(BROLL, 'broll.mp4')).rejects.toThrow('absolute path');
		await expect(api.relinkAsset(BROLL, '/samples/interview.mp4')).rejects.toThrow('is already in the library as "interview.mp4"');
		expect((await api.missingAssets()).map((m) => m.asset_id)).toContain(BROLL);
	});

	test('relinking the same media moved keeps its analysis and clears the missing mark', async () => {
		const relinked = await api.relinkAsset(BROLL, '/backup/broll.mp4');
		expect(relinked).toMatchObject({ analysis_kept: true, previous_path: '/samples/broll.mp4' });
		expect(relinked.asset.path).toBe('/backup/broll.mp4');
		expect((await api.missingAssets()).map((m) => m.asset_id)).toEqual([SONG]);
		expect((await api.getAssetMetadata(BROLL)).analysis).not.toBeNull();
		// The clip that used it is still there.
		expect(clipsOf(await api.getTimeline(), BROLL)).toHaveLength(1);
	});

	test('a different file that fits drops the analysis, which was about another recording', async () => {
		const relinked = await api.relinkAsset(SONG, '/restored/other-song.mp3');
		expect(relinked.analysis_kept).toBe(false);
		expect((await api.getAssetMetadata(SONG)).analysis).toBeNull();
		expect(await api.missingAssets()).toEqual([]);
	});

	test('the picker answers a plausible path in the harness', async () => {
		expect(await api.pickRelinkPath('broll.mp4')).toBe('/restored/broll.mp4');
	});
});

describe('removing media', () => {
	test('a used asset is refused unless forced, in the backend’s words', async () => {
		await expect(api.removeAsset(INTERVIEW)).rejects.toThrow(/is used by 2 clip\(s\) on the timeline or in the proposal/);
		expect((await api.listAssets()).map((a) => a.id)).toContain(INTERVIEW);
	});

	test('forcing removes its clips in one revision, then the asset', async () => {
		const before = (await api.getHistory()).length;
		const done = await api.removeAsset(INTERVIEW, true);
		expect(done.clips_removed).toBe(2);
		expect(done.asset.id).toBe(INTERVIEW);
		expect(clipsOf(done.timeline, INTERVIEW)).toEqual([]);
		expect((await api.listAssets()).map((a) => a.id)).not.toContain(INTERVIEW);
		const history = await api.getHistory();
		expect(history).toHaveLength(before + 1);
		expect(history[history.length - 1].label).toBe('Remove media interview.mp4');
	});

	test('an unused asset goes without a revision', async () => {
		const before = (await api.getHistory()).length;
		const done = await api.removeAsset(SONG);
		expect(done.clips_removed).toBe(0);
		expect(await api.getHistory()).toHaveLength(before);
		await expect(api.removeAsset(SONG)).rejects.toThrow('asset not found');
	});
});

describe('history and task paging', () => {
	test('limit and offset window the list from its newest end, oldest first', async () => {
		const all = await api.getHistory();
		expect(all.length).toBeGreaterThanOrEqual(2);
		const last = await api.getHistory(1);
		expect(last).toEqual([all[all.length - 1]]);
		const before = await api.getHistory(1, 1);
		expect(before).toEqual([all[all.length - 2]]);
		expect(await api.getHistory(100, all.length)).toEqual([]);
		const tasks = await api.listTasks();
		expect(await api.listTasks(1)).toEqual([tasks[tasks.length - 1]]);
		expect(await api.listTasks(undefined, 1)).toEqual(tasks.slice(0, -1));
	});
});

describe('recent projects and paths', () => {
	test('the harness seeds a list and clears it', async () => {
		const recent = await api.recentProjects();
		expect(recent.length).toBeGreaterThan(1);
		expect((await api.getSettings()).recent_projects).toEqual(recent);
		await api.clearRecentProjects();
		expect(await api.recentProjects()).toEqual([]);
		expect((await api.getSettings()).recent_projects).toEqual([]);
	});

	test('paths_exist answers one boolean per path', async () => {
		expect(await api.pathsExist(['/a.mp4', '/b.mp4'])).toEqual([true, true]);
		expect(await api.pathsExist([])).toEqual([]);
	});

	test('the new preferences round-trip and a reset puts them back', async () => {
		expect(await api.getSettings()).toMatchObject({ time_display: 'timecode', reopen_last_project: false });
		const set = await api.setSettings({ time_display: 'seconds', reopen_last_project: true });
		expect(set).toMatchObject({ time_display: 'seconds', reopen_last_project: true });
		expect(await api.resetSettings()).toMatchObject({ time_display: 'timecode', reopen_last_project: false });
	});
});

