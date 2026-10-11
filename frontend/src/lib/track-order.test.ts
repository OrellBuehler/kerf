import { describe, expect, test } from 'bun:test';
import { displayTracks, opensAudioGroup } from './track-order';
import type { StreamKind } from './types';

const t = (name: string, kind: StreamKind) => ({ name, kind });

describe('displayTracks', () => {
	test('draws the highest video layer on top and the audio below, in the project order', () => {
		const tracks = [t('V1', 'video'), t('V2', 'video'), t('V3', 'video'), t('A1', 'audio'), t('A2', 'audio')];
		expect(displayTracks(tracks).map((x) => x.name)).toEqual(['V3', 'V2', 'V1', 'A1', 'A2']);
	});

	test('does not reorder what it is given', () => {
		const tracks = [t('V1', 'video'), t('V2', 'video'), t('A1', 'audio')];
		displayTracks(tracks);
		expect(tracks.map((x) => x.name)).toEqual(['V1', 'V2', 'A1']);
	});

	test('video that the project stores after audio is still drawn above it', () => {
		const tracks = [t('A1', 'audio'), t('V1', 'video'), t('V2', 'video')];
		expect(displayTracks(tracks).map((x) => x.name)).toEqual(['V2', 'V1', 'A1']);
	});

	test('a single track, and none', () => {
		expect(displayTracks([t('V1', 'video')]).map((x) => x.name)).toEqual(['V1']);
		expect(displayTracks([])).toEqual([]);
	});

	test('the same tracks, only reordered: ids are what edits and hit-testing go by', () => {
		const tracks = [t('V1', 'video'), t('V2', 'video'), t('A1', 'audio')];
		expect(new Set(displayTracks(tracks))).toEqual(new Set(tracks));
	});
});

describe('opensAudioGroup', () => {
	const tracks = [t('V1', 'video'), t('V2', 'video'), t('A1', 'audio'), t('A2', 'audio')];
	const drawn = displayTracks(tracks);

	test('only the first audio track under a video track has the rule', () => {
		expect(drawn.map((x) => opensAudioGroup(drawn, x))).toEqual([false, false, true, false]);
	});

	test('no rule without video above, or without audio below', () => {
		const audioOnly = displayTracks([t('A1', 'audio'), t('A2', 'audio')]);
		expect(audioOnly.map((x) => opensAudioGroup(audioOnly, x))).toEqual([false, false]);
		const videoOnly = displayTracks([t('V1', 'video')]);
		expect(videoOnly.map((x) => opensAudioGroup(videoOnly, x))).toEqual([false]);
	});
});
