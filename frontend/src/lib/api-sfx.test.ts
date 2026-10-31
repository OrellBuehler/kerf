import { beforeEach, describe, expect, test } from 'bun:test';
import {
	addClip,
	addMarker,
	addOverlay,
	addSoundEffects,
	addTrack,
	getAssetMetadata,
	getHistory,
	getTimeline,
	listAssets,
	previewSoundEffect,
	revertTo,
	setRippleMode,
	setTrackLocked,
	sfxCatalogue,
	suggestSoundEffects,
	undo,
	updateSoundEffect
} from './api';
import { clipDuration } from './types';
import type { Clip, SfxPlacement, Timeline } from './types';

// Under bun there is no Tauri, so these drive the browser harness's sound effects: stand-in assets of the right length put
// on `FX` lanes by the rules of `project/sfx.rs`, and suggestions read from the sample cut. What is checked is the bridge —
// the contract of `sfx_catalogue` / `preview_sound_effect` / `add_sound_effects` / `update_sound_effect` /
// `suggest_sound_effects` — and the harness's state around it. The placement and suggestion logic underneath is `sfx.ts`,
// tested against the Rust cases.

beforeEach(async () => {
	await revertTo(0);
	// The harness's ripple mode is a setting, not part of the history: another file can leave it on.
	await setRippleMode(false);
});

const lane = (tl: Timeline, name: string) => tl.tracks.find((t) => t.name === name)?.clips ?? [];
const fx = (tl: Timeline) => tl.tracks.filter((t) => /^FX( \d+)?$/.test(t.name));
const end = (c: Clip) => c.timeline_start + clipDuration(c);
const head = async () => (await getHistory()).find((r) => r.current)!;

describe('sfx_catalogue (browser harness)', () => {
	test('lists the 21 sounds in five categories, each with its anchor and lengths', async () => {
		const rows = await sfxCatalogue();
		expect(rows.length).toBe(21);
		expect([...new Set(rows.map((r) => r.category))]).toEqual(['transition', 'impact', 'build', 'ui', 'texture']);
		const riser = rows.find((r) => r.kind === 'riser')!;
		expect(riser.anchor).toEqual({ type: 'end' });
		expect(riser.max_duration_s).toBeGreaterThan(riser.default_duration_s);
		expect(rows.find((r) => r.kind === 'whoosh')!.anchor).toEqual({ type: 'peak', fraction: 0.55 });
	});

	test('hands out a copy', async () => {
		const rows = await sfxCatalogue();
		rows[0].label = 'changed';
		rows.pop();
		const again = await sfxCatalogue();
		expect(again.length).toBe(21);
		expect(again[0].label).toBe('Whoosh');
	});
});

describe('preview_sound_effect (browser harness)', () => {
	test('has no sound to give, adds nothing and says what is wrong with a spec', async () => {
		const history = (await getHistory()).length;
		const assets = (await listAssets()).length;
		expect(await previewSoundEffect({ kind: 'whoosh', pitch: 2 })).toBeNull();
		expect((await getHistory()).length).toBe(history);
		expect((await listAssets()).length).toBe(assets);
		await expect(previewSoundEffect({ kind: 'whoosh', pitch: 20 })).rejects.toThrow('invalid argument: pitch must be -12..=12 semitones (got 20)');
		await expect(previewSoundEffect({ kind: 'whoosh', pitc: 2 } as never)).rejects.toThrow('unknown field `pitc`');
	});
});

describe('add_sound_effects (browser harness)', () => {
	test('puts one effect on a new FX track with its hit on the time asked, as one revision', async () => {
		const seq = (await head()).seq;
		const { clips, timeline } = await addSoundEffects([{ kind: 'whoosh', at: 12.5 }]);
		expect(fx(timeline).map((t) => t.name)).toEqual(['FX']);
		const [clip] = clips;
		expect(lane(timeline, 'FX').map((c) => c.id)).toEqual([clip.id]);
		// A whoosh peaks 55 % of the way through its 1 s.
		expect(clip.timeline_start).toBeCloseTo(12.5 - 0.55, 9);
		expect(end(clip)).toBeCloseTo(12.5 - 0.55 + 1, 9);
		expect(clip.volume).toBe(1);
		expect(timeline.tracks.find((t) => t.name === 'FX')!.kind).toBe('audio');

		const assets = await listAssets();
		const asset = assets.find((a) => a.id === clip.asset_id)!;
		expect(asset.name).toBe('SFX: Whoosh');
		expect(asset.duration).toBe(1);
		expect(asset.sound_effect).toEqual({ kind: 'whoosh', duration_s: 1, pitch: 0, intensity: 0.7, tail: 0.3, reverse: false, seed: 0 });
		expect(asset.streams.map((s) => s.kind)).toEqual(['audio']);
		// Its analysis is nothing to read: no transcript, no beats.
		expect((await getAssetMetadata(asset.id)).analysis?.transcript ?? []).toEqual([]);

		expect((await head()).seq).toBe(seq + 1);
		expect((await head()).label).toBe('Add sound effect');
		const back = await undo();
		expect(fx(back)).toEqual([]);
	});

	test('a batch is one revision; overlapping effects take FX 2, the same spec is one asset', async () => {
		const seq = (await head()).seq;
		const { clips, timeline } = await addSoundEffects([
			{ kind: 'riser', at: 12.5, duration_s: 2 },
			{ kind: 'impact', at: 12.5, volume_db: -6 },
			{ kind: 'pop', at: 3 },
			{ kind: 'pop', at: 5 }
		]);
		expect((await head()).seq).toBe(seq + 1);
		expect((await head()).label).toBe('Add sound effects');
		expect(clips.length).toBe(4);
		const [riser, impact, popA, popB] = clips;
		expect(end(riser)).toBeCloseTo(12.5, 9);
		expect(impact.timeline_start).toBeCloseTo(12.496, 9);
		expect(impact.volume).toBeCloseTo(0.501187, 5);
		// The impact starts 4 ms before the riser ends: another lane. The pops fit the first.
		expect(fx(timeline).map((t) => [t.name, t.clips.length])).toEqual([
			['FX', 3],
			['FX 2', 1]
		]);
		expect(lane(timeline, 'FX 2')[0].id).toBe(impact.id);
		expect(popA.asset_id).toBe(popB.asset_id);
		expect(new Set(clips.map((c) => c.asset_id)).size).toBe(3);
		expect((await listAssets()).filter((a) => a.sound_effect?.kind === 'pop').length).toBe(1);
	});

	test('the same effect added again is the same asset', async () => {
		const first = await addSoundEffects([{ kind: 'ding', at: 4, seed: 7 }]);
		const again = await addSoundEffects([{ kind: 'ding', at: 9, seed: 7 }]);
		expect(again.clips[0].asset_id).toBe(first.clips[0].asset_id);
		expect(lane(again.timeline, 'FX').length).toBe(2);
		const other = await addSoundEffects([{ kind: 'ding', at: 14, seed: 8 }]);
		expect(other.clips[0].asset_id).not.toBe(first.clips[0].asset_id);
	});

	test('a start before 0 has its head cut off, or is snapped to 0, or is refused', async () => {
		const { clips } = await addSoundEffects([
			{ kind: 'whoosh', at: 0.2 },
			{ kind: 'impact', at: 0.002 }
		]);
		expect(clips[0].timeline_start).toBe(0);
		expect(clips[0].source_in).toBeCloseTo(0.35, 9);
		expect(clips[0].fade_in).toBeCloseTo(0.004, 9);
		expect(clips[1].timeline_start).toBe(0);
		expect(clips[1].source_in).toBe(0);
		await expect(addSoundEffects([{ kind: 'riser', at: 0.01, duration_s: 3 }])).rejects.toThrow(
			'invalid argument: effects[0] (riser): its hit is 3.00 s into its 3.00 s, so it cannot land at 0.01 s with the start of the timeline in the way; place it at 0.02 s or later'
		);
	});

	test('a track named is used, and must be an unlocked audio track with room', async () => {
		const tl = await addTrack('audio', 'Stings');
		const track = tl.tracks[tl.tracks.length - 1];
		const placed = await addSoundEffects([{ kind: 'pop', at: 3, track_id: track.id }]);
		expect(lane(placed.timeline, 'Stings').length).toBe(1);
		expect(fx(placed.timeline)).toEqual([]);
		await expect(addSoundEffects([{ kind: 'boom', at: 3, track_id: track.id }])).rejects.toThrow(
			/^invalid argument: the clip would overlap another clip on Stings at /
		);
		await expect(addSoundEffects([{ kind: 'pop', at: 3, track_id: 'v1' }])).rejects.toThrow(
			'invalid argument: effects[0] (pop): a sound effect goes on an audio track; V1 is not one'
		);
		await expect(addSoundEffects([{ kind: 'pop', at: 3, track_id: 'nope' }])).rejects.toThrow('track not found: nope');
		await setTrackLocked(track.id, true);
		try {
			await expect(addSoundEffects([{ kind: 'pop', at: 30, track_id: track.id }])).rejects.toThrow(
				'invalid argument: effects[0] (pop): track Stings is locked'
			);
		} finally {
			await setTrackLocked(track.id, false);
		}
	});

	test('a locked FX lane is skipped for a new one', async () => {
		const first = await addSoundEffects([{ kind: 'pop', at: 3 }]);
		const fxTrack = first.timeline.tracks.find((t) => t.name === 'FX')!;
		await setTrackLocked(fxTrack.id, true);
		try {
			const next = await addSoundEffects([{ kind: 'pop', at: 9 }]);
			expect(fx(next.timeline).map((t) => [t.name, t.clips.length])).toEqual([
				['FX', 1],
				['FX 2', 1]
			]);
		} finally {
			await setTrackLocked(fxTrack.id, false);
		}
	});

	test('a named track with a clip in the way is refused without ripple and pushes it with ripple', async () => {
		const song = (await listAssets()).find((a) => a.name === 'music.mp3')!;
		const tl = await addTrack('audio', 'Bed');
		const track = tl.tracks[tl.tracks.length - 1];
		await addClip(song.id, 0, 4, track.id, 5);
		await expect(addSoundEffects([{ kind: 'boom', at: 4.5, track_id: track.id }])).rejects.toThrow(/overlap/);
		const pushed = await addSoundEffects([{ kind: 'boom', at: 4.5, track_id: track.id }], true);
		const [boom, bed] = lane(pushed.timeline, 'Bed');
		expect(bed.timeline_start).toBeGreaterThanOrEqual(end(boom) - 1e-9);
	});

	test('a refusal changes nothing: no history, no asset, no track', async () => {
		const history = (await getHistory()).length;
		const assets = (await listAssets()).length;
		const tracks = (await getTimeline()).tracks.length;
		const refused = async (placements: SfxPlacement[], why: string | RegExp) => {
			await expect(addSoundEffects(placements)).rejects.toThrow(why);
			expect((await getHistory()).length).toBe(history);
			expect((await listAssets()).length).toBe(assets);
			expect((await getTimeline()).tracks.length).toBe(tracks);
		};
		await refused([], 'invalid argument: there are no sound effects to add');
		await refused([{ kind: 'pop', at: 1 }, { kind: 'riser', at: -2 }], 'invalid argument: effects[1] (riser): at must be a time in seconds, 0 or later (got -2)');
		await refused([{ kind: 'pop', at: 1, intensity: 3 }], 'invalid argument: effects[0] (pop): intensity must be 0..=1 (got 3)');
		await refused([{ kind: 'pop', at: 1, volume_db: 70 }], 'invalid argument: effects[0] (pop): volume_db must be -60..=12 (got 70)');
		await refused([{ kind: 'pop', at: 1, seed: 2 ** 53 }], /seed must be 0\.\.=9007199254740991/);
		await refused([{ kind: 'pop', at: 1, pitc: 1 } as never], 'unknown field `pitc`');
		await refused([{ kind: 'zap', at: 1 } as never], 'unknown variant `zap`');
		await refused([{ kind: 'pop' } as never], 'missing field `at`');
		await refused(
			Array.from({ length: 257 }, (_, i) => ({ kind: 'click' as const, at: i })),
			'invalid argument: add at most 256 sound effects at a time'
		);
	});
});

describe('update_sound_effect (browser harness)', () => {
	test('renders the clip again with its hit where it was, its id and gain kept, as one revision', async () => {
		const { clips } = await addSoundEffects([{ kind: 'riser', at: 12.5, duration_s: 2, volume_db: -6 }]);
		const [riser] = clips;
		const seq = (await head()).seq;
		const { clip, timeline } = await updateSoundEffect(riser.id, { kind: 'riser', duration_s: 4, pitch: -3 });
		expect(clip.id).toBe(riser.id);
		expect(end(clip)).toBeCloseTo(12.5, 9);
		expect(clip.timeline_start).toBeCloseTo(8.5, 9);
		expect(clip.volume).toBeCloseTo(riser.volume, 9);
		expect(clip.asset_id).not.toBe(riser.asset_id);
		expect(lane(timeline, 'FX').map((c) => c.id)).toEqual([riser.id]);
		const asset = (await listAssets()).find((a) => a.id === clip.asset_id)!;
		expect(asset.sound_effect).toMatchObject({ kind: 'riser', duration_s: 4, pitch: -3 });
		expect((await head()).seq).toBe(seq + 1);
		expect((await head()).label).toBe('Edit sound effect');
		const back = await undo();
		expect(lane(back, 'FX')[0].asset_id).toBe(riser.asset_id);
		expect(clipDuration(lane(back, 'FX')[0])).toBeCloseTo(2, 9);
	});

	test('a new kind swaps the sound, a volume replaces the gain, and the whole spec is what is rendered', async () => {
		const { clips } = await addSoundEffects([{ kind: 'whoosh', at: 12.5, pitch: 4, seed: 9 }]);
		const { clip } = await updateSoundEffect(clips[0].id, { kind: 'swoosh' }, -3);
		const asset = (await listAssets()).find((a) => a.id === clip.asset_id)!;
		expect(asset.name).toBe('SFX: Swoosh');
		// What the new spec leaves out is back at its default, not what the clip had.
		expect(asset.sound_effect).toEqual({ kind: 'swoosh', duration_s: 0.7, pitch: 0, intensity: 0.7, tail: 0.3, reverse: false, seed: 0 });
		expect(clip.volume).toBeCloseTo(0.707946, 5);
		// A swoosh peaks 45 % in.
		expect(clip.timeline_start + 0.7 * 0.45).toBeCloseTo(12.5, 9);
	});

	test('is refused for what is not an effect, a locked lane, a bad spec or a render that runs into the next clip', async () => {
		const { clips, timeline } = await addSoundEffects([
			{ kind: 'boom', at: 3, duration_s: 1 },
			{ kind: 'pop', at: 5 }
		]);
		const [boom] = clips;
		const history = (await getHistory()).length;
		const assets = (await listAssets()).length;
		await expect(updateSoundEffect('c3', { kind: 'pop' })).rejects.toThrow('invalid argument: that clip is not a sound effect');
		await expect(updateSoundEffect('c1', { kind: 'pop' })).rejects.toThrow('invalid argument: only a sound effect on an audio track can be reshaped');
		await expect(updateSoundEffect('nope', { kind: 'pop' })).rejects.toThrow('clip not found: nope');
		await expect(updateSoundEffect(boom.id, { kind: 'boom', tail: 2 })).rejects.toThrow('invalid argument: tail must be 0..=1 (got 2)');
		await expect(updateSoundEffect(boom.id, { kind: 'boom' }, 99)).rejects.toThrow('invalid argument: volume_db must be -60..=12 (got 99)');
		await expect(updateSoundEffect(boom.id, { kind: 'boom', tails: 1 } as never)).rejects.toThrow('unknown field `tails`');
		// The 3 s boom would run into the pop on its lane.
		await expect(updateSoundEffect(boom.id, { kind: 'boom', duration_s: 3 })).rejects.toThrow(/^invalid argument: the clip would overlap another clip on FX at /);
		expect((await getHistory()).length).toBe(history);
		expect((await listAssets()).length).toBe(assets);
		const fxTrack = timeline.tracks.find((t) => t.name === 'FX')!;
		await setTrackLocked(fxTrack.id, true);
		try {
			await expect(updateSoundEffect(boom.id, { kind: 'boom', duration_s: 1 })).rejects.toThrow('track FX is locked');
			expect((await listAssets()).length).toBe(assets);
		} finally {
			await setTrackLocked(fxTrack.id, false);
		}
		// With ripple on the longer render pushes the pop instead.
		await setRippleMode(true);
		try {
			const pushed = await updateSoundEffect(boom.id, { kind: 'boom', duration_s: 3 });
			const [made, pop] = lane(pushed.timeline, 'FX');
			expect(made.id).toBe(boom.id);
			expect(pop.timeline_start).toBeGreaterThanOrEqual(end(made) - 1e-9);
		} finally {
			await setRippleMode(false);
		}
	});
});

describe('suggest_sound_effects (browser harness)', () => {
	test('finds the cut between the interview and the b-roll and answers a placement add takes as it is', async () => {
		const out = await suggestSoundEffects();
		expect(out.length).toBe(1);
		const [cut] = out;
		expect(cut.source).toBe('cut');
		expect(cut.placement).toMatchObject({ kind: 'whoosh', at: 12.5, seed: 0 });
		// The interview is still speaking where it ends (its transcript runs to 12.5 s), so the effect sits under the voice.
		expect(cut.placement.volume_db).toBe(-8);
		expect(cut.reason).toBe('Cut from interview.mp4 to broll.mp4 at 0:12.5');
		expect(cut.score).toBeGreaterThan(0.35);
		const added = await addSoundEffects(out.map((s) => s.placement));
		expect(added.clips.length).toBe(1);
		expect(added.clips[0].timeline_start).toBeCloseTo(12.5 - 0.55, 9);
		// Running it again suggests only what is left.
		expect(await suggestSoundEffects()).toEqual([]);
	});

	test('density chooses what counts: subtle leaves the cut, punchy adds the hook', async () => {
		expect(await suggestSoundEffects({ density: 'subtle' })).toEqual([]);
		const punchy = await suggestSoundEffects({ density: 'punchy' });
		expect(punchy.map((s) => [s.source, s.placement.kind, s.placement.at])).toEqual([
			['opening', 'impact', 0],
			['cut', 'whoosh', 12.5]
		]);
		expect(punchy[0].reason).toBe('Hook: a hit as interview.mp4 opens at 0:00.0');
		expect(await suggestSoundEffects({ density: 'punchy', categories: ['transition'] })).toHaveLength(1);
		expect(await suggestSoundEffects({ density: 'punchy', range: { start: 5, end: 20 } })).toHaveLength(1);
	});

	test('reads titles and markers from the cut it is given', async () => {
		await addOverlay('Welcome', 2, 5);
		await addMarker(8, 'sfx:ding');
		await addMarker(9, 'sfx:zap');
		const out = await suggestSoundEffects({ density: 'subtle' });
		expect(out.map((s) => [s.source, s.placement.kind, s.placement.at])).toEqual([
			['title', 'swipe', 2],
			['marker', 'ding', 8]
		]);
		expect(out[1].reason).toBe('Marker "sfx:ding" at 0:08.0 asks for a ding');
	});

	test('a request with a key it does not have, a bad density or a bad range is refused', async () => {
		await expect(suggestSoundEffects({ dense: 'punchy' } as never)).rejects.toThrow(
			'invalid args `request` for command `suggest_sound_effects`: unknown field `dense`, expected one of `range`, `density`, `categories`, `kinds`'
		);
		await expect(suggestSoundEffects({ density: 'loud' } as never)).rejects.toThrow('unknown variant `loud`');
		await expect(suggestSoundEffects({ range: { start: 4, end: 4 } })).rejects.toThrow('invalid argument: range needs 0 <= start < end, in seconds');
	});

	test('is a read: no history, no asset', async () => {
		const history = (await getHistory()).length;
		const assets = (await listAssets()).length;
		await suggestSoundEffects({ density: 'punchy' });
		expect((await getHistory()).length).toBe(history);
		expect((await listAssets()).length).toBe(assets);
	});
});
