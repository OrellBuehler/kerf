import { describe, expect, test } from 'bun:test';
import { searchTranscripts, terms } from './transcript-search';
import type { Asset, AssetAnalysis, Timeline } from './types';

const asset = (id: string, audio = true): Asset =>
	({
		id,
		path: `/${id}.mp4`,
		name: `${id}.mp4`,
		duration: 60,
		streams: audio ? [{ index: 0, kind: 'audio', codec: 'aac' }] : [],
		imported_at: ''
	}) as unknown as Asset;

const analysis = (id: string): AssetAnalysis =>
	({
		asset_id: id,
		transcript: [
			{ start: 0, end: 4, text: 'we went to the', speaker: 'S1' },
			{ start: 10, end: 12, text: 'Lake today', speaker: 'S2' }
		]
	}) as unknown as AssetAnalysis;

const timeline = {
	tracks: [{ id: 't', clips: [{ id: 'c', asset_id: 'a', source_in: 5, source_out: 20, timeline_start: 100 }] }]
} as unknown as Timeline;

describe('harness transcript search', () => {
	test('folds case, punctuation, apostrophes and accents', () => {
		expect(terms('Don’t STOP, café!')).toEqual(['dont', 'stop', 'cafe']);
	});

	test('a phrase crosses lines and reports where the cut uses it', () => {
		const found = searchTranscripts([asset('a'), asset('b'), asset('x', false)], { a: analysis('a') }, timeline, {
			query: 'the lake'
		});
		expect(found.total).toBe(1);
		expect(found.searched_assets).toBe(1);
		expect(found.assets_without_transcript).toEqual({ count: 1, asset_ids: ['b'] });
		const hit = found.hits[0];
		expect(hit.start).toBeCloseTo(32 / 11);
		expect(hit.end).toBeCloseTo(10 + 8 / 9);
		expect([hit.speaker, hit.words_estimated]).toEqual(['S1', true]);
		expect(hit.context).toBe('we went to «the Lake» today');
		expect(hit.in_cut).toHaveLength(1);
		expect([hit.in_cut[0].clip_id, hit.in_cut[0].start, hit.in_cut[0].partial]).toEqual(['c', 100, true]);
		expect(hit.in_cut[0].end).toBeCloseTo(105 + 8 / 9);
	});
});
