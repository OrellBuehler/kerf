import { describe, expect, test } from 'bun:test';
import {
	STEM_NAMES,
	STEMS_CANCELLED,
	capitalized,
	describeStems,
	devStemAssets,
	downloadNote,
	isStemsCancelled,
	placeStems,
	stageLabel,
	stemsOffer
} from './stems';
import type { Asset, Clip, StemsStatus, Timeline, Track } from './types';

// `placeStems` replays `stems_land_under_the_clip_and_silence_its_mix` and the refusals of
// `Project::place_stems` in `project.rs`: a rule changed there has to change here, or one of
// these names it.

const song: Asset = {
	id: 'song',
	path: '/song.wav',
	name: 'song',
	duration: 30,
	streams: [{ index: 0, kind: 'audio', codec: 'pcm_s16le', sample_rate: 44100, channels: 2 }],
	imported_at: '2026-01-01T00:00:00Z'
};
const footage: Asset = {
	...song,
	id: 'footage',
	name: 'footage',
	streams: [{ index: 0, kind: 'video', codec: 'h264', width: 1920, height: 1080, fps: 30 }, ...song.streams]
};
const silent: Asset = { ...footage, id: 'silent', streams: [footage.streams[0]] };

let n = 0;
const newId = () => `id-${++n}`;
const stemsOf = (a: Asset) => devStemAssets(a, newId, a.imported_at);

const clip = (extra: Partial<Clip> = {}): Clip => ({
	id: 'clip',
	asset_id: 'song',
	source_in: 2,
	source_out: 12,
	timeline_start: 5,
	volume: 0.8,
	fade_in: 0.5,
	fade_out: 1,
	...extra
});
const track = (kind: Track['kind'], clips: Clip[], extra: Partial<Track> = {}): Track => ({
	id: `t-${kind}`,
	kind,
	name: kind === 'audio' ? 'A1' : 'V1',
	clips,
	...extra
});
const cut = (...tracks: Track[]): Timeline => ({ tracks });

describe('devStemAssets', () => {
	test('are the four stems in the core order, named after their source', () => {
		const stems = stemsOf(song);
		expect(STEM_NAMES).toEqual(['drums', 'bass', 'other', 'vocals']);
		expect(stems.map((s) => s.name)).toEqual(['song · drums', 'song · bass', 'song · other', 'song · vocals']);
		expect(stems.map((s) => s.path)).toEqual(STEM_NAMES.map((s) => `/stems/song/${s}.flac`));
		expect(new Set(stems.map((s) => s.id)).size).toBe(4);
		for (const s of stems) {
			expect(s.duration).toBe(30);
			expect(s.streams.map((x) => x.kind)).toEqual(['audio']);
		}
	});
});

describe('placeStems', () => {
	test('lays a stem on a new audio track under an audio clip and switches the clip off', () => {
		const original = clip({ speed: 1.5 });
		const tl = cut(track('audio', [original]));
		const stems = stemsOf(song);
		const placed = placeStems(tl, 'song', stems, 'clip', newId);

		expect(placed).toHaveLength(4);
		expect(tl.tracks.map((t) => t.name)).toEqual(['A1', 'Drums', 'Bass', 'Other', 'Vocals']);
		for (const t of tl.tracks.slice(1)) {
			expect(t.kind).toBe('audio');
			expect(t.clips).toHaveLength(1);
		}
		placed.forEach((c, i) => {
			expect(c.asset_id).toBe(stems[i].id);
			expect(tl.tracks[i + 1].clips[0]).toEqual(c);
			expect([c.timeline_start, c.source_in, c.source_out]).toEqual([5, 2, 12]);
			expect([c.speed, c.volume, c.fade_in, c.fade_out]).toEqual([1.5, 0.8, 0.5, 1]);
		});
		expect(new Set(placed.map((c) => c.id)).size).toBe(4);
		// The mix is not heard twice.
		expect(tl.tracks[0].clips[0].enabled).toBe(false);
		expect(tl.tracks[0].clips[0].source_audio).toBeUndefined();
	});

	test("switches off a picture clip's own sound, not the picture", () => {
		const tl = cut(track('video', [clip({ asset_id: 'footage' })]));
		placeStems(tl, 'footage', stemsOf(footage), 'clip', newId);
		const picture = tl.tracks[0].clips[0];
		expect(picture.source_audio).toBe(false);
		expect(picture.enabled).toBeUndefined();
		expect(tl.tracks).toHaveLength(5);
	});

	test('stops a stem at its own length', () => {
		const stems = stemsOf(song).map((s) => ({ ...s, duration: 8 }));
		const tl = cut(track('audio', [clip()]));
		for (const c of placeStems(tl, 'song', stems, 'clip', newId)) expect(c.source_out).toBe(8);
	});

	test('refuses a clip of another asset, a locked track and a picture whose sound is linked, changing nothing', () => {
		const refuse = (tl: Timeline, source: string, message: string) => {
			const before = structuredClone(tl);
			expect(() => placeStems(tl, source, stemsOf(song), 'clip', newId)).toThrow(message);
			expect(tl).toEqual(before);
		};
		refuse(cut(track('audio', [clip()])), 'other', 'not of the separated asset');
		refuse(cut(track('audio', [clip()], { locked: true })), 'song', 'A1 is locked');
		refuse(cut(track('video', [clip({ link_id: 'pair' })])), 'song', 'linked audio clip');
		refuse(cut(track('audio', [])), 'song', 'clip not found: clip');
	});

	test('takes an audio clip that is linked to a picture (it is the sound)', () => {
		const tl = cut(track('audio', [clip({ link_id: 'pair' })]));
		expect(placeStems(tl, 'song', stemsOf(song), 'clip', newId)).toHaveLength(4);
		expect(tl.tracks[0].clips[0].enabled).toBe(false);
	});
});

describe('stemsOffer', () => {
	test('is offered to a clip whose asset has sound, and to nothing else', () => {
		expect(stemsOffer(track('audio', []), clip(), song)).toEqual({ show: true, reason: null });
		expect(stemsOffer(track('video', []), clip(), footage)).toEqual({ show: true, reason: null });
		expect(stemsOffer(track('video', []), clip(), silent).show).toBe(false);
		expect(stemsOffer(track('audio', []), clip(), undefined).show).toBe(false);
	});

	test('says why it cannot be used, with the backend refusals', () => {
		expect(stemsOffer(track('audio', [], { locked: true }), clip(), song)).toEqual({ show: true, reason: 'Track A1 is locked' });
		expect(stemsOffer(track('video', []), clip({ link_id: 'pair' }), footage).reason).toContain('linked audio clip');
		// The sound of a linked pair is the audio clip: that one can be separated.
		expect(stemsOffer(track('audio', []), clip({ link_id: 'pair' }), song).reason).toBeNull();
	});
});

describe('words', () => {
	test('the download note names what the first run still fetches', () => {
		const status = (extra: Partial<StemsStatus>): StemsStatus => ({
			runtime_ready: false,
			model_ready: false,
			model_bytes: 174 * 1024 * 1024,
			stems: [...STEM_NAMES],
			...extra
		});
		expect(downloadNote(status({}))).toBe('The first separation downloads the separation model (~174 MB) and the audio runtime.');
		expect(downloadNote(status({ runtime_ready: true }))).toBe('The first separation downloads the separation model (~174 MB).');
		expect(downloadNote(status({ model_ready: true }))).toBe('The first separation downloads the audio runtime.');
		expect(downloadNote(status({ runtime_ready: true, model_ready: true }))).toBeNull();
	});

	test('the toast says where the parts went', () => {
		expect(describeStems('song', { clips: [] })).toBe('Split song into drums, bass, other and vocals — in the library');
		expect(describeStems('song', { clips: [clip(), clip(), clip(), clip()] })).toBe(
			'Split song into drums, bass, other and vocals — on 4 new tracks'
		);
	});

	test('a stop is told from a failure', () => {
		expect(isStemsCancelled(STEMS_CANCELLED)).toBe(true);
		expect(isStemsCancelled(new Error('stems cancelled'))).toBe(true);
		expect(isStemsCancelled(new Error('invalid argument: nope'))).toBe(false);
		expect(isStemsCancelled('voiceover cancelled')).toBe(false);
	});

	test('every stage has a label', () => {
		for (const stage of ['download_runtime', 'download_model', 'separate', 'encode'] as const) {
			expect(stageLabel(stage)).not.toBe('Working…');
		}
		expect(capitalized('drums')).toBe('Drums');
		expect(capitalized('')).toBe('');
	});
});
