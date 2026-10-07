import { describe, expect, test } from 'bun:test';
import {
	CAPTION_EXTENSIONS,
	CAPTION_CONFIRM_TITLE,
	CAPTION_HINT,
	IMPORT_BASES,
	KEEP_LINES,
	MENU_ASSET_LIMIT,
	NO_SOURCE_HINT,
	OFFSET_HINT,
	TIMELINE_CHOICE,
	baseHint,
	baseName,
	clearHint,
	generatedCount,
	importMenuEntries,
	importRequest,
	importTone,
	importableAssets,
	keepLinesDefault,
	keepLinesHint,
	keepLinesOn,
	normalizeOffset,
	pickImportAsset,
	recaptionHint,
	replaceConfirm,
	resolveChoice,
	shorten,
	type ImportAsset
} from './caption-import-ui';
import type { Asset, CaptionImportSummary, Clip, TextOverlay, Timeline, Track } from './types';

const asset = (id: string, name: string, path = `/media/${name}`): Asset => ({
	id,
	path,
	name,
	duration: 60,
	streams: [],
	imported_at: ''
});

let n = 0;
const clip = (asset_id: string, timeline_start: number, extra: Partial<Clip> = {}): Clip => ({
	id: `c${n++}`,
	asset_id,
	source_in: 0,
	source_out: 5,
	timeline_start,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});

const track = (id: string, kind: Track['kind'], clips: Clip[], extra: Partial<Track> = {}): Track => ({
	id,
	kind,
	name: id,
	clips,
	...extra
});

const timeline = (...tracks: Track[]): Timeline => ({ tracks, overlays: [] }) as unknown as Timeline;

const A = asset('a', 'interview.mp4');
const B = asset('b', 'broll.mp4');
const C = asset('c', 'music.wav');

describe('which clips a file can be timed to', () => {
	test('assets the cut shows, in the order it first shows them', () => {
		const tl = timeline(
			track('V1', 'video', [clip('b', 0), clip('a', 5), clip('b', 10)]),
			track('A1', 'audio', [clip('c', 2)])
		);
		expect(importableAssets(tl, [A, B, C]).map((a) => [a.id, a.clips])).toEqual([
			['b', 2],
			['c', 1],
			['a', 1]
		]);
	});

	test('an asset first shown later on a lower track still orders by time, not by track', () => {
		const tl = timeline(track('V1', 'video', [clip('a', 8)]), track('V2', 'video', [clip('b', 1)]));
		expect(importableAssets(tl, [A, B]).map((a) => a.id)).toEqual(['b', 'a']);
	});

	test('the same start time keeps the track order', () => {
		const tl = timeline(track('V1', 'video', [clip('a', 0)]), track('A1', 'audio', [clip('c', 0)]));
		expect(importableAssets(tl, [A, B, C]).map((a) => a.id)).toEqual(['a', 'c']);
	});

	test('a muted track, a solo-shadowed one and a disabled clip show nothing', () => {
		const tl = timeline(
			track('V1', 'video', [clip('a', 0), clip('b', 5, { enabled: false })]),
			track('V2', 'video', [clip('c', 0)], { muted: true })
		);
		expect(importableAssets(tl, [A, B, C]).map((a) => a.id)).toEqual(['a']);

		const soloed = timeline(
			track('V1', 'video', [clip('a', 0)], { solo: true }),
			track('V2', 'video', [clip('b', 0)]),
			track('A1', 'audio', [clip('c', 0)])
		);
		// Solo shadows the other tracks of its own kind only.
		expect(importableAssets(soloed, [A, B, C]).map((a) => a.id)).toEqual(['a', 'c']);
	});

	test('an asset the project no longer lists is left out, and an empty cut offers nothing', () => {
		const tl = timeline(track('V1', 'video', [clip('ghost', 0), clip('a', 5)]));
		expect(importableAssets(tl, [A]).map((a) => a.id)).toEqual(['a']);
		expect(importableAssets(timeline(), [A, B])).toEqual([]);
		expect(importableAssets(timeline(track('V1', 'video', [])), [A, B])).toEqual([]);
	});

	test('two assets of one name are told apart by their folder', () => {
		const one = asset('x', 'GOPR0001.MP4', '/cards/day1/GOPR0001.MP4');
		const two = asset('y', 'GOPR0001.MP4', 'C:\\cards\\day2\\GOPR0001.MP4');
		const tl = timeline(track('V1', 'video', [clip('x', 0), clip('y', 5), clip('a', 10)]));
		const offered = importableAssets(tl, [one, two, A]);
		expect(offered.map((a) => a.label)).toEqual(['GOPR0001.MP4 · day1', 'GOPR0001.MP4 · day2', 'interview.mp4']);
		expect(offered.map((a) => a.name)).toEqual(['GOPR0001.MP4', 'GOPR0001.MP4', 'interview.mp4']);
	});
});

describe('picking the asset', () => {
	const offered: ImportAsset[] = [
		{ id: 'a', name: 'a', label: 'a', clips: 1 },
		{ id: 'b', name: 'b', label: 'b', clips: 1 }
	];

	test('a choice the cut still shows wins over everything', () => {
		expect(pickImportAsset(offered, 'b', ['a'])).toBe('b');
	});

	test('else the first hint that is offered, else the first offered', () => {
		expect(pickImportAsset(offered, null, [undefined, 'zzz', 'b'])).toBe('b');
		expect(pickImportAsset(offered, 'gone', ['nope'])).toBe('a');
		expect(pickImportAsset(offered, null)).toBe('a');
	});

	test('nothing offered, nothing picked', () => {
		expect(pickImportAsset([], 'a', ['a'])).toBeNull();
	});
});

describe('the choice and the request', () => {
	const offered: ImportAsset[] = [{ id: 'a', name: 'a', label: 'a', clips: 1 }];

	test('timeline stays timeline, whatever asset is remembered', () => {
		expect(resolveChoice('timeline', 'a', offered)).toEqual(TIMELINE_CHOICE);
	});

	test('source resolves its asset', () => {
		expect(resolveChoice('source', null, offered)).toEqual({ base: 'source', assetId: 'a' });
		expect(resolveChoice('source', 'a', offered)).toEqual({ base: 'source', assetId: 'a' });
		expect(resolveChoice('source', 'gone', offered)).toEqual({ base: 'source', assetId: 'a' });
	});

	test('source with nothing to time to falls back to the cut', () => {
		expect(resolveChoice('source', 'a', [])).toEqual(TIMELINE_CHOICE);
	});

	test('the request carries the timing and the look, and names an asset only for source', () => {
		expect(importRequest(TIMELINE_CHOICE, 'lines')).toEqual({ base: 'timeline', options: { style: 'lines' } });
		expect(importRequest({ base: 'source', assetId: 'a' }, 'word_punch')).toEqual({
			base: 'source',
			assetId: 'a',
			options: { style: 'word_punch' }
		});
		// A source choice that lost its asset is not a request that can work.
		expect(importRequest({ base: 'source', assetId: null }, 'lines')).toEqual({
			base: 'timeline',
			options: { style: 'lines' }
		});
	});

	test('the offset and "keep the file\'s lines" ride along, and only when they do something', () => {
		const keep = { max_words: KEEP_LINES.max_words, max_chars: KEEP_LINES.max_chars };
		expect(importRequest(TIMELINE_CHOICE, 'lines', { keepLines: true })).toEqual({
			base: 'timeline',
			options: { style: 'lines', ...keep }
		});
		// Word punch has no lines to keep: sending the override would defeat the look.
		expect(importRequest(TIMELINE_CHOICE, 'word_punch', { keepLines: true })).toEqual({
			base: 'timeline',
			options: { style: 'word_punch' }
		});
		expect(importRequest(TIMELINE_CHOICE, 'lines', { offset: -3600 })).toEqual({
			base: 'timeline',
			options: { style: 'lines' },
			offset: -3600
		});
		expect(importRequest({ base: 'source', assetId: 'a' }, 'lines', { keepLines: true, offset: 2.5 })).toEqual({
			base: 'source',
			assetId: 'a',
			options: { style: 'lines', ...keep },
			offset: 2.5
		});
		// No shift is no field, however it was spelled.
		for (const none of [0, -0, null, undefined, NaN, Infinity]) {
			expect(importRequest(TIMELINE_CHOICE, 'lines', { offset: none })).toEqual({
				base: 'timeline',
				options: { style: 'lines' }
			});
		}
		// A whole cue is one chunk: more words and characters than a cue can hold.
		expect(KEEP_LINES.max_chars).toBeGreaterThanOrEqual(2000);
		expect(KEEP_LINES.max_words).toBeGreaterThanOrEqual(1000);
	});

	test('the offset is held to what the engine takes', () => {
		expect(normalizeOffset(12.5)).toBe(12.5);
		expect(normalizeOffset(-3600)).toBe(-3600);
		expect(normalizeOffset(1e9)).toBe(360_000);
		expect(normalizeOffset(-1e9)).toBe(-360_000);
		expect(normalizeOffset(null)).toBe(0);
		expect(normalizeOffset(NaN)).toBe(0);
		expect(OFFSET_HINT).toContain('-3600');
	});

	test('keeping the file\'s lines follows the frame until it is touched, and never in word punch', () => {
		const wide = { width: 1920, height: 1080 };
		const tall = { width: 1080, height: 1920 };
		const square = { width: 1080, height: 1080 };
		// Landscape and unframed: the file's lines were made for this shape.
		expect(keepLinesDefault(wide)).toBe(true);
		expect(keepLinesDefault(null)).toBe(true);
		expect(keepLinesDefault(undefined)).toBe(true);
		// Square and tall: one long line would be drawn too small to read.
		expect(keepLinesDefault(tall)).toBe(false);
		expect(keepLinesDefault(square)).toBe(false);
		expect(keepLinesOn(null, 'lines', wide)).toBe(true);
		expect(keepLinesOn(null, 'lines', tall)).toBe(false);
		// Touching the box settles it, whatever the frame.
		expect(keepLinesOn(false, 'lines', wide)).toBe(false);
		expect(keepLinesOn(true, 'lines', tall)).toBe(true);
		// Word punch is never "lines".
		expect(keepLinesOn(true, 'word_punch', wide)).toBe(false);
		expect(keepLinesOn(null, 'word_punch', null)).toBe(false);
		expect(keepLinesHint('word_punch', false, wide)).toContain('one word at a time');
		expect(keepLinesHint('lines', true, wide)).toContain('stays one caption');
		expect(keepLinesHint('lines', false, tall)).toContain('tall frame');
		expect(keepLinesHint('lines', false, wide)).toContain('split into short lines');
		expect(CAPTION_CONFIRM_TITLE).toBe('Caption the cut');
	});

	test('the two timings are named for what they mean', () => {
		expect(IMPORT_BASES.map((b) => b.id)).toEqual(['timeline', 'source']);
		expect(IMPORT_BASES.map((b) => b.label)).toEqual(['Timed to the cut', 'Timed to a source clip']);
		expect(baseHint(TIMELINE_CHOICE)).toContain('finished cut');
		expect(baseHint({ base: 'source', assetId: 'a' }, 'interview.mp4')).toContain("interview.mp4's own footage");
		expect(NO_SOURCE_HINT).toContain('clip on the timeline');
	});
});

describe('the titles-lane menu', () => {
	const mk = (i: number): ImportAsset => ({ id: `id${i}`, name: `clip${i}`, label: `clip${i}`, clips: 1 });

	test('leads with the cut-timed import, then one entry per clip', () => {
		const { entries, more } = importMenuEntries([mk(1), mk(2)]);
		expect(entries.map((e) => e.label)).toEqual([
			'Import captions…',
			'Import captions timed to clip1…',
			'Import captions timed to clip2…'
		]);
		expect(entries[0].choice).toEqual(TIMELINE_CHOICE);
		expect(entries[2].choice).toEqual({ base: 'source', assetId: 'id2' });
		expect(more).toBe(0);
	});

	test('an empty cut offers only the cut-timed import', () => {
		const { entries, more } = importMenuEntries([]);
		expect(entries).toHaveLength(1);
		expect(more).toBe(0);
	});

	test('past the limit the rest are counted, not listed', () => {
		const offered = Array.from({ length: MENU_ASSET_LIMIT + 2 }, (_, i) => mk(i));
		const { entries, more } = importMenuEntries(offered);
		expect(entries).toHaveLength(1 + MENU_ASSET_LIMIT);
		expect(more).toBe(2);
	});

	test('a long name is shortened in the label but not in the choice', () => {
		const long: ImportAsset = { id: 'x', name: 'n', label: 'a-very-long-interview-recording-name-take-3.mp4', clips: 1 };
		const { entries } = importMenuEntries([long]);
		expect(entries[1].label.length).toBeLessThan(`Import captions timed to ${long.label}…`.length);
		expect(entries[1].label).toContain('…');
		expect(entries[1].choice.assetId).toBe('x');
	});
});

describe('words around the import', () => {
	const overlay = (generated?: boolean): TextOverlay => ({ id: 'o', text: 't', start: 0, end: 1, generated }) as TextOverlay;

	test('generated overlays are the set an import replaces', () => {
		expect(generatedCount([overlay(true), overlay(), overlay(false), overlay(true)])).toBe(2);
		expect(generatedCount([])).toBe(0);
		expect(generatedCount(undefined)).toBe(0);
	});

	test('the replace question counts the captions and names the file', () => {
		expect(replaceConfirm(12)).toBe('Replace the 12 captions already on the cut? Titles you added by hand are kept.');
		expect(replaceConfirm(1, 'movie.srt')).toBe(
			'Replace the caption already on the cut with those in movie.srt? Titles you added by hand are kept.'
		);
		expect(replaceConfirm(3, 'movie.srt')).toContain('Replace the 3 captions already on the cut with those in movie.srt?');
	});

	const summary: CaptionImportSummary = {
		format: 'srt',
		cues: 10,
		placed: 10,
		captions: 12,
		skipped_lines: 0,
		dropped_outside: 0,
		dropped_short: 0,
		dropped_overlap: 0,
		replaced: 0
	};

	test('anything that did not land makes the toast a warning', () => {
		expect(importTone(summary)).toBe('success');
		// Replacing captions is not a problem.
		expect(importTone({ ...summary, replaced: 9 })).toBe('success');
		expect(importTone({ ...summary, placed: 9, dropped_outside: 1 })).toBe('warning');
		expect(importTone({ ...summary, placed: 9, dropped_overlap: 1 })).toBe('warning');
		expect(importTone({ ...summary, placed: 9, dropped_short: 1 })).toBe('warning');
		expect(importTone({ ...summary, skipped_lines: 2 })).toBe('warning');
	});

	test('recaption admits it replaces an imported set; clear admits it removes one', () => {
		expect(recaptionHint(5)).toContain('Replaces the 5 captions');
		expect(recaptionHint(5)).toContain('imported from a subtitle file');
		expect(recaptionHint(1)).toContain('Replaces the 1 caption on the cut');
		expect(clearHint(5)).toBe('Remove the 5 generated or imported captions. Titles you added by hand stay.');
		expect(clearHint(1)).toBe('Remove the generated or imported caption. Titles you added by hand stay.');
		expect(CAPTION_HINT).toContain('transcripts');
	});
});

describe('files and paths', () => {
	test('the picker offers the extensions the backend reads', () => {
		expect([...CAPTION_EXTENSIONS]).toEqual(['srt', 'ass', 'ssa']);
	});

	test('a base name takes either separator', () => {
		expect(baseName('/home/me/subs/movie.srt')).toBe('movie.srt');
		expect(baseName('C:\\Users\\me\\subs\\movie.srt')).toBe('movie.srt');
		expect(baseName('movie.srt')).toBe('movie.srt');
		expect(baseName('/home/me/subs/')).toBe('subs');
		expect(baseName('')).toBe('');
	});

	test('shorten keeps what fits and counts characters, not code units', () => {
		expect(shorten('short', 10)).toBe('short');
		expect(shorten('abcdefghij', 5)).toBe('abcd…');
		expect([...shorten('🎬🎬🎬🎬🎬🎬', 4)]).toHaveLength(4);
	});
});
