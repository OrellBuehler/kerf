import { describe, expect, test } from 'bun:test';
import { describeSync, syncOffer } from './audio-sync';
import type { AudioSync, Clip, Timeline, Track } from './types';

const clip = (id: string, asset: string, extra: Partial<Clip> = {}): Clip =>
	({ id, asset_id: asset, source_in: 0, source_out: 4, timeline_start: 0, ...extra }) as Clip;
const track = (id: string, clips: Clip[], extra: Partial<Track> = {}): Track =>
	({ id, kind: 'audio', name: id, clips, ...extra }) as Track;
const sound = { id: 'snd', streams: [{ index: 0, kind: 'audio' as const }] };
const still = { id: 'img', streams: [{ index: 0, kind: 'video' as const }] };
const assets = [sound, still] as never[];

const tl = (a: Clip, b: Clip, locked = false): Timeline =>
	({ tracks: [track('A1', [a]), track('A2', [b], { locked })] }) as Timeline;

describe('syncOffer', () => {
	test('two selected clips with sound: the clicked one moves to the other', () => {
		const t = tl(clip('a', 'snd'), clip('b', 'snd'));
		expect(syncOffer(t, assets, ['a', 'b'], 'b')).toEqual({ show: true, reason: null, referenceId: 'a' });
		expect(syncOffer(t, assets, ['a', 'b'], 'a').referenceId).toBe('b');
	});

	test('not offered unless exactly two are selected and one was clicked', () => {
		const t = tl(clip('a', 'snd'), clip('b', 'snd'));
		expect(syncOffer(t, assets, ['a'], 'a').show).toBe(false);
		expect(syncOffer(t, assets, ['a', 'b', 'c'], 'a').show).toBe(false);
		expect(syncOffer(t, assets, ['a', 'b'], 'z').show).toBe(false);
	});

	test('says why it cannot run, as the engine would refuse', () => {
		expect(syncOffer(tl(clip('a', 'snd'), clip('b', 'img')), assets, ['a', 'b'], 'b').reason).toBe('Both clips need sound');
		expect(syncOffer(tl(clip('a', 'snd', { speed: 2 }), clip('b', 'snd')), assets, ['a', 'b'], 'b').reason).toBe(
			'Both clips need to play at normal speed'
		);
		expect(
			syncOffer(tl(clip('a', 'snd', { link_id: 'L' }), clip('b', 'snd', { link_id: 'L' })), assets, ['a', 'b'], 'b').reason
		).toBe('The clips are linked — unlink them first');
		expect(syncOffer(tl(clip('a', 'snd'), clip('b', 'snd'), true), assets, ['a', 'b'], 'b').reason).toBe('Track A2 is locked');
	});
});

describe('describeSync', () => {
	const base: AudioSync = {
		reference_id: 'a',
		clip_id: 'b',
		offset: -0.5126,
		timeline_start: 1,
		confidence: 21.3,
		distinct: 3,
		reliable: true,
		inverted: false,
		applied: true
	};
	test('a move, signed, and a match left alone', () => {
		expect(describeSync(base)).toBe('Synced by audio — moved −0.513 s');
		expect(describeSync({ ...base, offset: 2, inverted: true })).toBe('Synced by audio — moved +2.000 s (one recording has inverted polarity)');
		expect(describeSync({ ...base, applied: false, reliable: false, confidence: 4.21 })).toBe(
			'No clear match in the sound (confidence 4.2) — the clip was left where it is'
		);
	});
});
