import { describe, expect, test } from 'bun:test';
import {
	DEFAULT_SORT,
	NO_FILTER,
	binView,
	dayStart,
	filterActive,
	filterChips,
	fmtSize,
	kindOf,
	matchesFilter,
	pickSort,
	sortAssets,
	sortLabel,
	toggleIn,
	type BinFacts
} from './bin-view';
import type { Asset, StreamInfo } from './types';

const video = (width: number, height: number, fps = 25): StreamInfo => ({ index: 0, kind: 'video', codec: 'hevc', width, height, fps });
const sound: StreamInfo = { index: 1, kind: 'audio', codec: 'aac', sample_rate: 48000, channels: 2 };

function asset(id: string, name: string, streams: StreamInfo[], duration = 10, more: Partial<Asset> = {}): Asset {
	return { id, name, path: `/media/${name}`, duration, streams, imported_at: '2026-01-01T00:00:00Z', ...more };
}

const A = asset('a', 'clip10.mp4', [video(3840, 2160, 100), sound], 30);
const B = asset('b', 'Clip2.mp4', [video(1080, 1920)], 5, { tags: [{ value: 'jump' }] });
const C = asset('c', 'song.wav', [sound], 180);
const D = asset('d', 'still.png', [{ ...video(640, 480), image: true }], 0);
const ALL = [A, B, C, D];

const FACTS: Record<string, BinFacts> = {
	a: { file: { size_bytes: 900, modified: '2026-03-10T12:00:00Z' }, uses: 2 },
	b: { file: { size_bytes: 100, modified: '2026-03-12T12:00:00Z' }, uses: 0 },
	c: { file: {}, uses: 0 },
	d: { file: { size_bytes: 500, modified: '2026-02-01T12:00:00Z' }, uses: 0 }
};
const factsOf = (id: string) => FACTS[id];
const ids = (list: readonly Asset[]) => list.map((a) => a.id).join('');

describe('sorting the bin', () => {
	test('the default is the order the project holds', () => {
		expect(ids(sortAssets(ALL, factsOf, DEFAULT_SORT))).toBe('abcd');
	});

	test('names sort the way a file manager sorts them: no case, numbers by value', () => {
		expect(ids(sortAssets(ALL, factsOf, { key: 'name', desc: false }))).toBe('bacd');
		expect(ids(sortAssets(ALL, factsOf, { key: 'name', desc: true }))).toBe('dcab');
	});

	test('a file with no size goes last in either direction', () => {
		expect(ids(sortAssets(ALL, factsOf, { key: 'size', desc: true }))).toBe('adbc');
		expect(ids(sortAssets(ALL, factsOf, { key: 'size', desc: false }))).toBe('bdac');
	});

	test('the date is the file’s, and the import’s where the file has none', () => {
		expect(ids(sortAssets(ALL, factsOf, { key: 'date', desc: true }))).toBe('badc');
	});

	test('length', () => {
		expect(ids(sortAssets(ALL, factsOf, { key: 'duration', desc: true }))).toBe('cabd');
	});

	test('picking the key in force turns it round; another starts in its own direction', () => {
		expect(pickSort(DEFAULT_SORT, 'name')).toEqual({ key: 'name', desc: false });
		expect(pickSort({ key: 'name', desc: false }, 'name')).toEqual({ key: 'name', desc: true });
		expect(pickSort({ key: 'name', desc: true }, 'size')).toEqual({ key: 'size', desc: true });
		expect(pickSort(DEFAULT_SORT, 'added')).toEqual(DEFAULT_SORT);
		expect(sortLabel({ key: 'date', desc: true })).toBe('Date, newest first');
	});
});

describe('filtering the bin', () => {
	const shown = (f: Partial<typeof NO_FILTER>) => ids(binView(ALL, factsOf, { ...NO_FILTER, ...f }, DEFAULT_SORT));

	test('no filter shows everything', () => {
		expect(filterActive(NO_FILTER)).toBe(false);
		expect(shown({})).toBe('abcd');
	});

	test('every word of the text must be in the name, ignoring case', () => {
		expect(shown({ text: 'CLIP' })).toBe('ab');
		expect(shown({ text: 'clip 10' })).toBe('a');
		expect(shown({ text: '  ' })).toBe('abcd');
	});

	test('kinds', () => {
		expect(ALL.map(kindOf)).toEqual(['video', 'video', 'audio', 'image']);
		expect(shown({ kinds: ['audio', 'image'] })).toBe('cd');
	});

	test('within a group either may hold, every group must', () => {
		expect(shown({ props: ['res:uhd'] })).toBe('a');
		expect(shown({ props: ['res:uhd', 'res:hd'] })).toBe('ab');
		expect(shown({ props: ['res:uhd', 'res:hd', 'shape:portrait'] })).toBe('b');
		expect(shown({ props: ['res:sd'] })).toBe('d');
		expect(shown({ props: ['rate:high'] })).toBe('a');
		expect(shown({ props: ['sound:without'] })).toBe('bd');
		expect(shown({ props: ['use:used'] })).toBe('a');
		expect(shown({ props: ['tags:untagged'] })).toBe('acd');
	});

	test('a date range holds both of its days, in local time', () => {
		const day = (iso: string) => {
			const d = new Date(iso);
			return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
		};
		const [a, b] = [day('2026-03-10T12:00:00Z'), day('2026-03-12T12:00:00Z')];
		expect(shown({ from: a, to: b })).toBe('ab');
		expect(shown({ from: b })).toBe('b');
		expect(shown({ to: a })).toBe('acd');
		expect(matchesFilter(A, FACTS.a, { ...NO_FILTER, from: a, to: a })).toBe(true);
		expect(dayStart('nonsense')).toBeNull();
	});

	test('each part of the filter is a chip that takes itself off', () => {
		const f = { ...NO_FILTER, text: 'x', kinds: ['video' as const], props: ['use:unused' as const], from: '2026-03-01', to: '' };
		const chips = filterChips(f);
		expect(chips.map((c) => c.label)).toEqual(['Video', 'Not used yet', 'from 2026-03-01']);
		expect(chips[0].without.kinds).toEqual([]);
		expect(chips[2].without.from).toBe('');
		expect(chips[1].without.text).toBe('x');
	});

	test('toggleIn and sizes', () => {
		expect(toggleIn(['a'], 'a')).toEqual([]);
		expect(toggleIn(['a'], 'b')).toEqual(['a', 'b']);
		expect(fmtSize(512)).toBe('512 B');
		expect(fmtSize(1536)).toBe('1.5 KB');
		expect(fmtSize(250 * 1024 * 1024)).toBe('250 MB');
		expect(fmtSize(3.25 * 1024 ** 3)).toBe('3.3 GB');
	});
});
