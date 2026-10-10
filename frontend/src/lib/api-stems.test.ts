import { beforeEach, describe, expect, test } from 'bun:test';
import { cancelStems, getHistory, listAssets, onStemsProgress, revertTo, separateStems, stemsStatus, undo } from './api';
import { isStemsCancelled } from './stems';
import type { Asset, StemsProgress, Timeline } from './types';

// Under bun there is no Tauri, so these drive the browser harness's separation: the same progress
// sequence the app streams, four stem assets named like the real ones and, under a clip,
// `placeStems` (tested against the Rust cases in stems.test.ts). What is checked here is the
// bridge — the contract of `stems_status` / `separate_stems` / `cancel_stems` — and the state
// around it. The first run pays the (fake) download, so the tests that start one are slow.

const SLOW = 20_000;

beforeEach(async () => {
	await revertTo(0);
});

async function assetNamed(name: string): Promise<Asset> {
	return (await listAssets()).find((a) => a.name === name)!;
}

function record(): { stages: StemsProgress['stage'][]; stop: Promise<() => void> } {
	const stages: StemsProgress['stage'][] = [];
	const stop = onStemsProgress((p) => {
		if (stages[stages.length - 1] !== p.stage) stages.push(p.stage);
	});
	return { stages, stop };
}

/** The revision the cut is at; a redo branch left by an earlier test makes the list's length a poor measure. */
const head = async () => (await getHistory()).find((r) => r.current)!;

const clipOf = (tl: Timeline, id: string) => tl.tracks.flatMap((t) => t.clips).find((c) => c.id === id)!;

describe('separate_stems (browser harness)', () => {
	test('a stopped first run leaves nothing behind and the next one starts over', async () => {
		const music = await assetNamed('music.mp3');
		const before = (await listAssets()).length;
		expect((await stemsStatus()).model_ready).toBe(false);

		const seen: StemsProgress[] = [];
		const stop = await onStemsProgress((p) => {
			seen.push(p);
			void cancelStems();
		});
		const run = separateStems(music.id);
		await expect(run).rejects.toThrow('stems cancelled');
		await run.catch((e) => expect(isStemsCancelled(e)).toBe(true));
		stop();

		expect(seen[0].stage).toBe('download_runtime');
		expect((await listAssets()).length).toBe(before);
		expect((await stemsStatus()).model_ready).toBe(false);
	}, SLOW);

	test('streams download, separate and encode, then adds the four stems to the library', async () => {
		const music = await assetNamed('music.mp3');
		const before = (await listAssets()).length;
		const at = (await head()).seq;
		const { stages, stop } = record();
		const unlisten = await stop;

		const { placed, timeline } = await separateStems(music.id);
		unlisten();

		expect(stages).toEqual(['download_runtime', 'download_model', 'separate', 'encode']);
		expect(placed.assets.map((a) => a.name)).toEqual(['music.mp3 · drums', 'music.mp3 · bass', 'music.mp3 · other', 'music.mp3 · vocals']);
		expect(placed.clips).toEqual([]);
		expect((await listAssets()).length).toBe(before + 4);
		// Into the library alone: the cut is untouched and there is no revision to undo.
		expect(timeline.tracks.length).toBe(2);
		expect((await head()).seq).toBe(at);
		const status = await stemsStatus();
		expect([status.runtime_ready, status.model_ready]).toEqual([true, true]);
		expect(status.stems).toEqual(['drums', 'bass', 'other', 'vocals']);
	}, SLOW);

	test('a second run finds the stems again, without a word of progress', async () => {
		const music = await assetNamed('music.mp3');
		const first = (await listAssets()).filter((a) => a.name.startsWith('music.mp3 · '));
		expect(first).toHaveLength(4);
		const seen: StemsProgress[] = [];
		const unlisten = await onStemsProgress((p) => seen.push(p));
		const { placed } = await separateStems(music.id);
		unlisten();
		expect(seen).toEqual([]);
		expect(placed.assets.map((a) => a.id)).toEqual(first.map((a) => a.id));
		expect((await listAssets()).filter((a) => a.name.startsWith('music.mp3 · '))).toHaveLength(4);
	}, SLOW);

	test("under a clip: four new tracks, the clip's own sound off, one revision that undo takes back", async () => {
		const interview = await assetNamed('interview.mp4');
		const at = (await head()).seq;
		// The sample cut: the interview's sound is the clip on A1, linked to the picture on V1.
		const { placed, timeline } = await separateStems(interview.id, 'c3');

		expect(timeline.tracks.map((t) => t.name)).toEqual(['V1', 'A1', 'Drums', 'Bass', 'Other', 'Vocals']);
		expect(placed.clips).toHaveLength(4);
		const original = clipOf(timeline, 'c3');
		for (const [i, c] of placed.clips.entries()) {
			expect(c.asset_id).toBe(placed.assets[i].id);
			expect([c.timeline_start, c.source_in, c.source_out]).toEqual([original.timeline_start, original.source_in, original.source_out]);
			expect(clipOf(timeline, c.id)).toEqual(c);
		}
		expect(original.enabled).toBe(false);
		expect(await head()).toMatchObject({ seq: at + 1, label: 'Separate stems' });

		const back = await undo();
		expect(back.tracks.map((t) => t.name)).toEqual(['V1', 'A1']);
		expect(clipOf(back, 'c3').enabled).not.toBe(false);
	}, SLOW);

	test('refuses what the app refuses', async () => {
		const interview = await assetNamed('interview.mp4');
		const at = (await head()).seq;
		await expect(separateStems((await assetNamed('broll.mp4')).id)).rejects.toThrow('no sound to separate');
		await expect(separateStems('nope')).rejects.toThrow('asset not found');
		// The picture's sound is the linked audio clip.
		await expect(separateStems(interview.id, 'c1')).rejects.toThrow('linked audio clip');
		// A clip of another asset.
		await expect(separateStems(interview.id, 'c2')).rejects.toThrow('not of the separated asset');
		await expect(separateStems(interview.id, 'missing')).rejects.toThrow('clip not found');
		// Nothing was done to the cut.
		expect((await head()).seq).toBe(at);
	}, SLOW);
});
