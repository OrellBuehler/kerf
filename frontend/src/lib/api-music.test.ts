import { beforeEach, describe, expect, test } from 'bun:test';
import { addClip, addTrack, fitMusic, getAssetMetadata, getHistory, listAssets, planMusicFit, revertTo, undo } from './api';
import { clipDuration } from './types';

// Under bun there is no Tauri, so these drive the browser harness's fit: the sample song in the
// library (audio only, with a bar grid) put on a track of its own under the sample picture. The
// planner underneath is `music-fit.ts`, tested against the Rust cases; what is checked here is the
// bridge — the contract of `plan_music_fit` / `fit_music` — and the harness's state around it.

beforeEach(async () => {
	await revertTo(0);
});

/** The song on a new audio track at 0; its clip's id. */
async function withSong(): Promise<{ clip: string; asset: string }> {
	const assets = await listAssets();
	const song = assets.find((a) => a.name === 'music.mp3')!;
	const tl = await addTrack('audio', 'A2');
	const track = tl.tracks[tl.tracks.length - 1];
	const added = await addClip(song.id, 0, song.duration, track.id, 0);
	const clip = added.tracks.find((t) => t.id === track.id)!.clips[0];
	return { clip: clip.id, asset: song.id };
}

describe('the harness song', () => {
	test('is in the library, audio only, analyzed with a bar grid', async () => {
		const assets = await listAssets();
		const song = assets.find((a) => a.name === 'music.mp3')!;
		expect(song.streams.map((s) => s.kind)).toEqual(['audio']);
		const { analysis } = await getAssetMetadata(song.id);
		expect(analysis?.music?.bar_chroma.length).toBe(29);
		expect(analysis?.tempo?.downbeats.length).toBeGreaterThan(20);
		expect(analysis?.audio_class?.class).toBe('music');
		// The speech and the b-roll have no bar grid.
		for (const a of assets.filter((x) => x.id !== song.id)) {
			expect((await getAssetMetadata(a.id)).analysis?.music ?? null).toBeNull();
			expect((await getAssetMetadata(a.id)).analysis?.tempo?.downbeats ?? []).toEqual([]);
		}
	});
});

describe('plan_music_fit (browser harness)', () => {
	test('plans a fit to the picture without changing the cut', async () => {
		const { clip } = await withSong();
		const history = (await getHistory()).length;
		const plan = await planMusicFit(clip);
		// V1 holds the interview (12.5 s) and the b-roll (8 s): the picture is 20.5 s long.
		expect(plan.target).toBeCloseTo(20.5, 9);
		expect(plan.bars).toBe(9);
		expect(Math.abs(plan.remainder)).toBeLessThan(1e-4);
		expect(plan.splices).toBe(1);
		expect((await getHistory()).length).toBe(history);
	});

	test('a custom target replaces the picture length', async () => {
		const { clip } = await withSong();
		expect((await planMusicFit(clip, 19.5)).remainder).toBeLessThan(-0.5);
		expect((await planMusicFit(clip, 22)).remainder).toBeGreaterThan(1);
		expect((await planMusicFit(clip, null)).target).toBeCloseTo(20.5, 9);
	});

	test('says what is wrong, in the backend’s words', async () => {
		const { clip } = await withSong();
		await expect(planMusicFit(clip, 0)).rejects.toThrow('positive');
		await expect(planMusicFit(clip, -3)).rejects.toThrow('positive');
		await expect(planMusicFit('nope')).rejects.toThrow('clip not found');
		// The interview’s sound is on an audio track but is speech: no bar grid.
		await expect(planMusicFit('c3', 10)).rejects.toThrow('no bar grid');
		// A picture clip is not on an audio track.
		await expect(planMusicFit('c1', 10)).rejects.toThrow('audio track');
	});
});

describe('fit_music (browser harness)', () => {
	test('replaces the clip with the arrangement, in one revision, and undo puts it back', async () => {
		const { clip } = await withSong();
		const head = (await getHistory()).find((h) => h.current)!.seq;
		const { timeline, report } = await fitMusic(clip, null, true);
		expect(report.fit.bars).toBe(9);
		expect(report.faded).toBe(false); // it lands exactly: nothing to fade
		expect(report.clips.length).toBe(2);
		const lane = timeline.tracks.find((t) => t.name === 'A2')!;
		expect(lane.clips.map((c) => c.id)).toEqual(report.clips);
		expect(lane.clips.some((c) => c.id === clip)).toBe(false);
		const end = Math.max(...lane.clips.map((c) => c.timeline_start + clipDuration(c)));
		expect(end).toBeCloseTo(20.5, 6);
		expect(lane.clips[1].transition_in?.kind).toBe('crossfade');
		expect(report.duration).toBeCloseTo(20.5, 6);

		const history = await getHistory();
		expect(history.find((h) => h.current)!.seq).toBe(head + 1);
		expect(history[history.length - 1].label).toBe('Fit music to length');

		const back = await undo();
		expect(back.tracks.find((t) => t.name === 'A2')!.clips.map((c) => c.id)).toEqual([clip]);
	});

	test('cuts and fades an arrangement that runs over, unless asked not to', async () => {
		const { clip } = await withSong();
		const faded = await fitMusic(clip, 19.5, true);
		expect(faded.report.faded).toBe(true);
		expect(faded.report.duration).toBeCloseTo(19.5, 6);
		const lane = faded.timeline.tracks.find((t) => t.name === 'A2')!;
		expect(lane.clips[lane.clips.length - 1].fade_out).toBe(2);

		await revertTo(0);
		const { clip: again } = await withSong();
		const whole = await fitMusic(again, 19.5, false);
		expect(whole.report.faded).toBe(false);
		expect(whole.report.duration).toBeGreaterThan(20);
	});

	test('a refusal changes nothing and records nothing', async () => {
		const { clip } = await withSong();
		const before = await getHistory();
		await expect(fitMusic(clip, -1, true)).rejects.toThrow('positive');
		await expect(fitMusic('c3', 10, true)).rejects.toThrow('no bar grid');
		expect((await getHistory()).length).toBe(before.length);
	});
});
