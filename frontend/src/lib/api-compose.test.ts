import { beforeEach, describe, expect, test } from 'bun:test';
import {
	addClip,
	addMarker,
	cancelMusic,
	composeMusic,
	fitMusic,
	getAssetMetadata,
	getBandBalance,
	getHistory,
	getTimeline,
	listAssets,
	musicStatus,
	onMusicProgress,
	prepareMusic,
	revertTo,
	setRippleMode,
	setTrackLocked,
	undo
} from './api';
import { MAX_SEED, scoreDuration } from './music-score';
import type { Clip, ComposeRequest, MusicProgress, MusicScore } from './types';

// Under bun there is no Tauri, so these drive the browser harness's composition: a SoundFont download
// on first use, render ticks, then an audio-only asset as long as the score on the "M" track, its
// analysis written from the score. What is checked is the bridge — the contract of `music_status` /
// `compose_music` / `cancel_music` / `get_band_balance` — and the state around it. The score and
// alignment logic underneath is `music-score.ts`, tested against the Rust cases.

const SLOW = 20_000;

beforeEach(async () => {
	await revertTo(0);
	// The harness's ripple mode is a setting, not part of the history: another file can leave it on.
	await setRippleMode(false);
});

/** A 24-bar tropical-house piece in A minor at 120 BPM: 48 s and a tail. */
function score(): MusicScore {
	return {
		bpm: 120,
		key: 'Am',
		style: 'tropical_house',
		sections: [
			{ kind: 'intro', bars: 4, chords: ['i', 'VI'] },
			{ kind: 'build', bars: 4, chords: ['iv', 'V'] },
			{ kind: 'drop', bars: 8, chords: ['i', 'VI', 'III', 'VII'] },
			{ kind: 'breakdown', bars: 4, chords: ['VI', 'VII'] },
			{ kind: 'outro', bars: 4, chords: ['i'] }
		],
		seed: 0
	};
}

const mClip = (tl: { tracks: { name: string; clips: Clip[] }[] }) => tl.tracks.find((t) => t.name === 'M')?.clips ?? [];

describe('music_status (browser harness)', () => {
	test('lists the styles and the first composition still fetches the SoundFont', async () => {
		const status = await musicStatus();
		expect(status.styles.map((s) => s.name)).toEqual(['tropical_house', 'pop', 'lofi', 'cinematic', 'ambient', 'rock']);
		expect(status.sample_rate).toBe(48000);
		expect(status.rendering).toBe(0);
		if (!status.soundfont_ready) expect(status.approx_download_bytes).toBeGreaterThan(0);
	});
});

describe('compose_music (browser harness)', () => {
	test(
		'puts the piece on a new M track with an analysis written from the score',
		async () => {
			const stages: MusicProgress['stage'][] = [];
			const stop = await onMusicProgress((p) => {
				if (stages[stages.length - 1] !== p.stage) stages.push(p.stage);
			});
			const composed = await composeMusic({ score: score() });
			stop();
			expect(stages.at(-1)).toBe('render');
			expect(composed.replaced).toEqual([]);
			expect(composed.align).toBeNull();
			expect(composed.score).toEqual(score());
			expect(composed.asset.duration).toBeCloseTo(scoreDuration(score()), 9);
			expect(composed.asset.music_score).toEqual(score());
			expect(composed.asset.name).toBe('Music: Tropical house · Am · 120 BPM');
			expect(composed.asset.streams.map((s) => s.kind)).toEqual(['audio']);

			const clips = mClip(composed.timeline);
			expect(clips.map((c) => c.id)).toEqual([composed.clip.id]);
			expect(composed.clip.timeline_start).toBe(0);
			expect(composed.clip.source_out).toBeCloseTo(composed.asset.duration, 9);

			const { analysis } = await getAssetMetadata(composed.asset.id);
			expect(analysis?.tempo?.bpm).toBeCloseTo(120, 6);
			expect(analysis?.music?.grid.beats_per_bar).toBe(4);
			expect(analysis?.music?.bar_chroma.length).toBe(24);
			expect(analysis?.audio_class?.class).toBe('music');

			expect((await getHistory()).find((r) => r.current)!.label).toBe('Add music');
			expect((await musicStatus()).soundfont_ready).toBe(true);
		},
		SLOW
	);

	test(
		'the same score is the same asset, and a second piece lands after the first on the M track',
		async () => {
			const first = await composeMusic({ score: score() });
			const again = await composeMusic({ score: score() });
			expect(again.asset.id).toBe(first.asset.id);
			expect(again.clip.timeline_start).toBeCloseTo(first.asset.duration, 9);
			expect(mClip(again.timeline).length).toBe(2);
			expect((await listAssets()).filter((a) => a.id === first.asset.id).length).toBe(1);
		},
		SLOW
	);

	test(
		'replace_clip_id swaps the clip to the new render, keeping its place and id',
		async () => {
			const first = await composeMusic({ score: score(), at: 5 });
			const longer = { ...score(), sections: [...score().sections, { kind: 'verse' as const, bars: 2, chords: ['Am'] }] };
			const redone = await composeMusic({ score: longer, replace_clip_id: first.clip.id });
			expect(redone.replaced).toEqual([first.clip.id]);
			expect(redone.clip.id).toBe(first.clip.id);
			expect(redone.clip.timeline_start).toBe(5);
			expect(redone.clip.asset_id).toBe(redone.asset.id);
			expect(redone.clip.asset_id).not.toBe(first.asset.id);
			expect(redone.clip.source_out).toBeCloseTo(scoreDuration(longer), 9);
			expect(mClip(redone.timeline).length).toBe(1);
			expect((await getHistory()).find((r) => r.current)!.label).toBe('Recompose music');
			const tl = await undo();
			expect(tl.tracks.flatMap((t) => t.clips).find((c) => c.id === first.clip.id)!.asset_id).toBe(first.asset.id);
			expect((await getTimeline()).tracks.flatMap((t) => t.clips).find((c) => c.id === first.clip.id)!.timeline_start).toBe(5);
		},
		SLOW
	);

	test(
		'replacing replaces the whole bed Fit to video made of the piece, with one clip at its earliest start',
		async () => {
			const first = await composeMusic({ score: score(), at: 3 });
			const fitted = await fitMusic(first.clip.id, 22, false);
			const bed = mClip(fitted.timeline);
			expect(bed.length).toBeGreaterThanOrEqual(2);
			const ids = bed.map((c) => c.id);
			const start = bed[0].timeline_start;
			const redone = await composeMusic({ score: { ...score(), bpm: 100 }, replace_clip_id: ids[1] });
			expect(redone.replaced).toEqual(ids);
			expect(redone.clip.id).toBe(ids[1]);
			expect(redone.clip.timeline_start).toBe(start);
			expect(mClip(redone.timeline).map((c) => c.id)).toEqual([ids[1]]);
			expect((await getHistory()).find((r) => r.current)!.label).toBe('Recompose music');
		},
		SLOW
	);

	test(
		'a longer piece that runs into the next clip is refused without ripple and pushes it with ripple',
		async () => {
			const first = await composeMusic({ score: score(), at: 0 });
			const end = first.asset.duration;
			const other = (await listAssets()).find((a) => a.name === 'music.mp3')!;
			const added = await addClip(other.id, 0, 4, first.timeline.tracks.find((t) => t.name === 'M')!.id, end);
			const next = mClip(added)[1];
			const longer = { ...score(), sections: [...score().sections, { kind: 'verse' as const, bars: 8, chords: ['Am'] }] };
			const history = (await getHistory()).length;
			const assets = (await listAssets()).length;
			await expect(composeMusic({ score: longer, replace_clip_id: first.clip.id })).rejects.toThrow(
				/^invalid argument: the clip would overlap another clip on M at .* — move or trim that one first$/
			);
			// refused before the render: no history, no asset
			expect((await getHistory()).length).toBe(history);
			expect((await listAssets()).length).toBe(assets);
			await setRippleMode(true);
			try {
				const redone = await composeMusic({ score: longer, replace_clip_id: first.clip.id });
				const [made, pushed] = mClip(redone.timeline);
				expect(pushed.id).toBe(next.id);
				expect(pushed.timeline_start).toBeCloseTo(made.timeline_start + redone.asset.duration, 6);
			} finally {
				await setRippleMode(false);
			}
		},
		SLOW
	);

	test(
		'a piece that would land on another clip of M is refused before it renders, and one after it is not',
		async () => {
			await composeMusic({ score: score(), at: 0 });
			const assets = (await listAssets()).length;
			const history = (await getHistory()).length;
			const stages: string[] = [];
			const stop = await onMusicProgress((p) => stages.push(p.stage));
			await expect(composeMusic({ score: score(), at: 2 })).rejects.toThrow(/^invalid argument: the clip would overlap another clip on M at /);
			stop();
			expect(stages).toEqual([]);
			expect((await listAssets()).length).toBe(assets);
			expect((await getHistory()).length).toBe(history);
			const after = await composeMusic({ score: score(), at: scoreDuration(score()) });
			expect(mClip(after.timeline).length).toBe(2);
		},
		SLOW
	);

	test(
		'a locked M track refuses a new piece and a recompose before the render',
		async () => {
			const first = await composeMusic({ score: score() });
			const m = first.timeline.tracks.find((t) => t.name === 'M')!;
			await setTrackLocked(m.id, true);
			try {
				await expect(composeMusic({ score: score(), at: 100 })).rejects.toThrow('invalid argument: M is locked');
				await expect(composeMusic({ score: score(), track_id: m.id, at: 100 })).rejects.toThrow('invalid argument: M is locked');
				await expect(composeMusic({ score: score(), replace_clip_id: first.clip.id })).rejects.toThrow('invalid argument: M is locked');
			} finally {
				await setTrackLocked(m.id, false);
			}
		},
		SLOW
	);

	test(
		'a placement refused after the render, against the cut as it is by then, stores neither the asset nor its analysis',
		async () => {
			const first = await composeMusic({ score: score() });
			const m = first.timeline.tracks.find((t) => t.name === 'M')!;
			const other = { ...score(), bpm: 90 };
			const assets = (await listAssets()).length;
			const history = (await getHistory()).length;
			// Locked while it renders: the plan was fine, landing is not.
			const stop = await onMusicProgress((p) => {
				if (p.stage === 'render' && p.fraction === 0.5) void setTrackLocked(m.id, true);
			});
			try {
				await expect(composeMusic({ score: other, at: 200 })).rejects.toThrow('invalid argument: M is locked');
			} finally {
				stop();
				await setTrackLocked(m.id, false);
			}
			expect((await listAssets()).length).toBe(assets);
			expect((await listAssets()).some((a) => a.music_score?.bpm === 90)).toBe(false);
			expect((await getHistory()).length).toBeGreaterThanOrEqual(history);
			const again = await composeMusic({ score: other, at: 200 });
			expect((await getAssetMetadata(again.asset.id)).analysis?.tempo?.bpm).toBeCloseTo(90, 6);
		},
		SLOW
	);

	test(
		'a piece that ends on an outro ends on the tonic, in the analysis too',
		async () => {
			const ending = (written: string): MusicScore => ({
				...score(),
				sections: [
					{ kind: 'verse', bars: 4, chords: ['i', 'VI', 'III', 'VII'] },
					{ kind: 'outro', bars: 4, chords: ['i', 'VI', 'iv', written] }
				]
			});
			let slot = 0;
			const lastBar = async (written: string) => {
				const { asset } = await composeMusic({ score: ending(written), at: 100 * ++slot });
				const chroma = (await getAssetMetadata(asset.id)).analysis!.music!.bar_chroma;
				return chroma[chroma.length - 1];
			};
			// The last bar is the tonic whether it was written as V, as Am or as i; any other bar is its own chord.
			const [asV, asAm, asI] = [await lastBar('V'), await lastBar('Am'), await lastBar('i')];
			expect(asV).toEqual(asAm);
			expect(asV).toEqual(asI);
			const { asset } = await composeMusic({ score: ending('V'), at: 100 * ++slot });
			const chroma = (await getAssetMetadata(asset.id)).analysis!.music!.bar_chroma;
			expect(chroma[chroma.length - 2]).not.toEqual(chroma[chroma.length - 1]);
		},
		SLOW
	);

	test(
		'align fits the tempo to the picture and reports the cuts',
		async () => {
			// V1 holds the interview (12.5 s) and the b-roll (8 s): one cut, at 12.5 s.
			const request: ComposeRequest = { score: { ...score(), bpm: 118 }, align: { to: 'cuts' } };
			const composed = await composeMusic(request);
			expect(composed.align?.cuts).toBe(1);
			expect(composed.align!.bpm_before).toBe(118);
			expect(composed.align!.on_beat_after).toBeGreaterThanOrEqual(composed.align!.on_beat_before);
			expect(composed.score.bpm).toBe(composed.align!.bpm_after);
			expect(Math.abs(composed.align!.tempo_change)).toBeLessThanOrEqual(0.06 + 1e-9);
			expect(composed.asset.music_score).toEqual(composed.score);
		},
		SLOW
	);

	test('align to markers needs markers, and says so before anything renders', async () => {
		const request: ComposeRequest = { score: score(), align: { to: 'markers' } };
		await expect(composeMusic(request)).rejects.toThrow('there are no markers');
		await addMarker(8, 'beat');
		const composed = await composeMusic(request);
		expect(composed.align?.cuts).toBe(1);
		expect(composed.align?.details[0].cut_s).toBe(8);
	}, SLOW);

	test('refuses a key the request and the score structs do not have, as serde does', async () => {
		const stray = { score: score(), replace_clip: 'c' } as unknown as ComposeRequest;
		await expect(composeMusic(stray)).rejects.toThrow(
			'invalid args `request` for command `compose_music`: unknown field `replace_clip`, expected one of `score`, `track_id`, `at`, `align`, `replace_clip_id`'
		);
		const inScore = { score: { ...score(), tempo: 120 } } as unknown as ComposeRequest;
		await expect(composeMusic(inScore)).rejects.toThrow('unknown field `tempo`, expected one of `bpm`, `key`, `style`, `sections`, `hits`, `seed`, `instruments`');
		const inSection = { score: { ...score(), sections: [{ kind: 'intro', bar: 4, chords: ['Am'] }] } } as unknown as ComposeRequest;
		await expect(composeMusic(inSection)).rejects.toThrow('unknown field `bar`');
		await expect(composeMusic({ score: score(), align: { to: 'cuts', tempo: 1 } } as unknown as ComposeRequest)).rejects.toThrow('unknown field `tempo`, expected one of `to`, `track_id`, `max_tempo_change`');
	});

	test('a seed has to survive JSON in JavaScript', async () => {
		await expect(composeMusic({ score: { ...score(), seed: MAX_SEED + 1 } })).rejects.toThrow(
			'seed must be 0..=9007199254740991 (2^53 - 1, so it survives JSON in JavaScript; got 9007199254740992)'
		);
	});

	test('refuses what the engine refuses, naming the section or chord', async () => {
		const bad = score();
		bad.sections[2].chords = ['Am', 'Hm'];
		await expect(composeMusic({ score: bad })).rejects.toThrow(/sections\[2\] \(drop\).*"Hm"/);
		await expect(composeMusic({ score: score(), at: -1 })).rejects.toThrow('at must be');
		await expect(composeMusic({ score: score(), align: { to: 'cuts', max_tempo_change: 0.5 } })).rejects.toThrow('max_tempo_change');
		await expect(composeMusic({ score: score(), track_id: 'v1' })).rejects.toThrow('music goes on an audio track');
		await expect(composeMusic({ score: score(), replace_clip_id: 'nope' })).rejects.toThrow('clip not found');
		const { timeline } = await composeMusic({ score: score() });
		const ordinary = timeline.tracks.flatMap((t) => t.clips).find((c) => !mClip(timeline).some((m) => m.id === c.id))!;
		await expect(composeMusic({ score: score(), replace_clip_id: ordinary.id })).rejects.toThrow(/not composed music|audio track/);
	}, SLOW);

	test(
		'a stopped composition leaves nothing behind and rejects quietly',
		async () => {
			const history = (await getHistory()).length;
			const stop = await onMusicProgress(() => void cancelMusic());
			const run = composeMusic({ score: score() });
			await expect(run).rejects.toThrow('music cancelled');
			stop();
			expect((await getHistory()).length).toBe(history);
			expect((await musicStatus()).rendering).toBe(0);
		},
		SLOW
	);

	test('prepare_music downloads the SoundFont ahead of time', async () => {
		const status = await prepareMusic();
		expect(status.soundfont_ready).toBe(true);
		expect(status.approx_download_bytes).toBe(0);
	}, SLOW);
});

describe('get_band_balance (browser harness)', () => {
	test(
		'reports a composed asset by its score sections',
		async () => {
			const { asset } = await composeMusic({ score: score() });
			const balance = await getBandBalance({ asset_id: asset.id });
			expect(balance.target).toBe('asset');
			expect(balance.windows_from).toBe('score');
			expect(balance.windows.map((w) => w.label)).toEqual(['intro', 'build', 'drop', 'breakdown', 'outro']);
			expect(balance.windows[0].start).toBe(0);
			expect(balance.overall.bands.length).toBe(6);
			expect(balance.windows.every((w) => w.bands.length === 6 && w.loudness_lufs !== null)).toBe(true);
			const loudness = (label: string) => balance.windows.find((w) => w.label === label)!.loudness_lufs!;
			expect(loudness('drop')).toBeGreaterThan(loudness('intro'));
		},
		SLOW
	);

	test('explicit sections win, a mix range is timeline seconds, and music without a score is in phrases', async () => {
		const assets = await listAssets();
		const song = assets.find((a) => a.name === 'music.mp3')!;
		const phrases = await getBandBalance({ asset_id: song.id });
		expect(phrases.windows_from).toBe('phrases');
		expect(phrases.windows[0].label).toBe('bars 1–4');
		const speech = assets.find((a) => a.name === 'interview.mp4')!;
		expect((await getBandBalance({ asset_id: speech.id })).windows_from).toBe('equal');
		const given = await getBandBalance({ asset_id: speech.id, sections: [{ start: 0, end: 5, label: 'hello' }, { start: 5, end: 12 }] });
		expect(given.windows_from).toBe('sections');
		expect(given.windows.map((w) => w.label)).toEqual(['hello', null]);
		const mix = await getBandBalance({ range: { start: 2, end: 10 } });
		expect(mix.target).toBe('mix');
		expect([mix.start, mix.end]).toEqual([2, 10]);
	});

	test('refuses a key the request does not have', async () => {
		await expect(getBandBalance({ clip: 'x' } as never)).rejects.toThrow(
			'invalid args `request` for command `get_band_balance`: unknown field `clip`, expected one of `asset_id`, `clip_id`, `range`, `sections`'
		);
		await expect(getBandBalance({ asset_id: 'x', sections: [{ start: 0, end: 1, name: 'a' }] } as never)).rejects.toThrow(
			'unknown field `name`, expected one of `start`, `end`, `label`'
		);
	});

	test('names exactly one target and refuses what cannot be measured', async () => {
		await expect(getBandBalance({})).rejects.toThrow('exactly one');
		await expect(getBandBalance({ asset_id: 'x', range: { start: 0, end: 1 } })).rejects.toThrow('exactly one');
		await expect(getBandBalance({ range: { start: 50, end: 60 } })).rejects.toThrow('the cut is');
		const broll = (await listAssets()).find((a) => a.name === 'broll.mp4')!;
		await expect(getBandBalance({ asset_id: broll.id })).rejects.toThrow('no audio stream');
		await expect(getBandBalance({ asset_id: broll.id.replace('2', '9') })).rejects.toThrow('asset not found');
	});
});
