import { describe, expect, test } from 'bun:test';
import { clipSounds, mixerStrips, stateNote, trackHasSound } from './mixer-strips';
import type { Timeline, Track } from './types';

const clip = (asset_id: string, extra: object = {}) => ({
	id: `c-${asset_id}`,
	asset_id,
	source_in: 0,
	source_out: 5,
	timeline_start: 0,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});
const track = (id: string, kind: 'video' | 'audio', clips: object[], extra: Partial<Track> = {}): Track =>
	({ id, kind, name: id.toUpperCase(), clips, ...extra }) as Track;
const tl = (...tracks: Track[]): Timeline => ({ tracks });

/** `voice` has sound; `broll` is silent footage. */
const SOUND = new Set(['voice']);

describe('clipSounds', () => {
	test('needs an audio stream, and its own sound still on', () => {
		expect(clipSounds(clip('voice'), SOUND)).toBe(true);
		expect(clipSounds(clip('broll'), SOUND)).toBe(false);
		// `source_audio` is written only as false: absent and true are the same.
		expect(clipSounds({ asset_id: 'voice', source_audio: true }, SOUND)).toBe(true);
		expect(clipSounds({ asset_id: 'voice', source_audio: null }, SOUND)).toBe(true);
		// A detached picture is silent — the export mixes it through its audio clip only.
		expect(clipSounds({ asset_id: 'voice', source_audio: false }, SOUND)).toBe(false);
	});
});

describe('trackHasSound', () => {
	test('an audio track always has a strip, empty or not', () => {
		expect(trackHasSound(track('a1', 'audio', []), SOUND)).toBe(true);
	});

	test('a video track has one only while a clip plays sound of its own', () => {
		expect(trackHasSound(track('v1', 'video', [clip('voice')]), SOUND)).toBe(true);
		expect(trackHasSound(track('v1', 'video', [clip('broll')]), SOUND)).toBe(false);
		expect(trackHasSound(track('v1', 'video', []), SOUND)).toBe(false);
	});

	test('a video track whose sound was all detached has none — the audio track carries it', () => {
		const detached = track('v1', 'video', [clip('voice', { source_audio: false })]);
		expect(trackHasSound(detached, SOUND)).toBe(false);
		// One clip still sounding keeps the strip: the fader moves that clip.
		const mixed = track('v1', 'video', [clip('voice', { source_audio: false }), clip('voice')]);
		expect(trackHasSound(mixed, SOUND)).toBe(true);
	});
});

describe('mixerStrips', () => {
	test('one strip per audible track, in track order, with the defaults filled in', () => {
		const strips = mixerStrips(
			tl(track('v1', 'video', [clip('voice')]), track('v2', 'video', [clip('broll')]), track('a1', 'audio', [clip('voice')])),
			SOUND
		);
		expect(strips.map((s) => s.id)).toEqual(['v1', 'a1']);
		expect(strips[1]).toEqual({
			id: 'a1',
			name: 'A1',
			kind: 'audio',
			volume: 1,
			pan: 0,
			muted: false,
			solo: false,
			duck: false,
			locked: false,
			state: 'live',
			clips: 1
		});
	});

	test('carries the fader, the pan and the toggles', () => {
		const [s] = mixerStrips(tl(track('a1', 'audio', [], { volume: 0.5, pan: -0.25, duck: true, solo: true, locked: true })), SOUND);
		expect(s).toMatchObject({ volume: 0.5, pan: -0.25, duck: true, solo: true, locked: true, state: 'live' });
	});

	test('an empty timeline, or one of silent pictures, has no strips', () => {
		expect(mixerStrips(tl(), SOUND)).toEqual([]);
		expect(mixerStrips(tl(track('v1', 'video', [clip('broll')])), SOUND)).toEqual([]);
	});

	test('a detached sound moves its strip from the picture track to the audio track', () => {
		const before = tl(track('v1', 'video', [clip('voice')]), track('a1', 'audio', []));
		expect(mixerStrips(before, SOUND).map((s) => s.id)).toEqual(['v1', 'a1']);
		const after = tl(track('v1', 'video', [clip('voice', { source_audio: false })]), track('a1', 'audio', [clip('voice')]));
		expect(mixerStrips(after, SOUND).map((s) => s.id)).toEqual(['a1']);
	});

	test('says how each strip stands: muted, or shadowed by a solo of its own kind', () => {
		const muted = mixerStrips(tl(track('a1', 'audio', [], { muted: true })), SOUND)[0];
		expect(muted.state).toBe('muted');
		expect(stateNote(muted)).toBe('Muted');

		const soloed = mixerStrips(tl(track('a1', 'audio', [], { solo: true }), track('a2', 'audio', [])), SOUND);
		expect(soloed.map((s) => s.state)).toEqual(['live', 'shadowed']);
		expect(stateNote(soloed[1])).toBe('Not heard — another audio track is soloed');

		// Solo shadows within a kind — the export's `track_renders` — not across kinds.
		const crossKind = mixerStrips(tl(track('v1', 'video', [clip('voice')]), track('a1', 'audio', [], { solo: true })), SOUND);
		expect(crossKind.map((s) => s.state)).toEqual(['live', 'live']);

		expect(stateNote(soloed[0])).toBeNull();
		const hidden = mixerStrips(tl(track('v1', 'video', [clip('voice')], { muted: true })), SOUND)[0];
		expect(stateNote(hidden)).toBe('Hidden — its sound is off too');
	});

	test('a muted track that is also shadowed reads as muted', () => {
		const strips = mixerStrips(tl(track('a1', 'audio', [], { solo: true }), track('a2', 'audio', [], { muted: true })), SOUND);
		expect(strips[1].state).toBe('muted');
	});
});
