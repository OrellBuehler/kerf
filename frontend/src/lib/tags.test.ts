import { describe, expect, test } from 'bun:test';
import {
	MAX_ASSET_TAGS,
	MAX_TAG_KEY,
	MAX_TAG_VALUE,
	applyTagEdit,
	applyTagEditCapped,
	checkTagFilter,
	dedupeTags,
	formatTag,
	groupByKey,
	isTagChange,
	newTag,
	normalizeKey,
	parseTag,
	parseTagFilter,
	planAssembleByTag,
	sameTag,
	sameTags,
	tagEditIsEmpty,
	tagEditThen,
	tagFilterMatches,
	tagSummary,
	tagsMatch
} from './tags';
import type { Asset, Tag, TagEdit, TagFilter } from './types';

// The unit cases of `crates/kerf-core/src/model/tags.rs`, replayed: the browser harness and the dialogs spell, edit,
// filter and group tags with this file, so it has to say what the engine says. The cases keep the Rust names.

const t = parseTag;
const list = (texts: string[]): Tag[] => texts.map(parseTag);
const shown = (tags: readonly Tag[]) => tags.map(formatTag);
const refused = (run: () => unknown) => {
	try {
		run();
	} catch (e) {
		return (e as Error).message;
	}
	return null;
};

describe('a tag', () => {
	test('splits on the first colon and a plain one has none', () => {
		expect(t('location: Cumbuco')).toEqual(newTag('location', 'Cumbuco'));
		expect(t('jump').key).toBeUndefined();
		const url = t('link: https://x.example/a');
		expect([url.key, url.value]).toEqual(['link', 'https://x.example/a']);
		expect(formatTag(t('location: Cumbuco'))).toBe('location: Cumbuco');
		expect(formatTag(t('jump'))).toBe('jump');
	});

	test('only one word before the colon is a key', () => {
		const plain = (text: string) => {
			expect(t(text).key).toBeUndefined();
			return t(text).value;
		};
		expect(plain('intro 10:30')).toBe('intro 10:30');
		expect(plain('Part one: the beach')).toBe('Part one: the beach');
		expect(plain('at the 3:00 mark')).toBe('at the 3:00 mark');
		const long = `${'k'.repeat(MAX_TAG_KEY + 1)}:x`;
		expect(plain(long)).toBe(long);
		expect(t(`${'k'.repeat(MAX_TAG_KEY)}:x`).key).toBe('k'.repeat(MAX_TAG_KEY));
		expect(t(' Location\t: x').key).toBe('location');
		expect(refused(() => parseTag(': x'))).toBe('invalid argument: a tag key cannot be empty');
		expect(plain('a b:')).toBe('a b:');
		expect(parseTagFilter('intro 10:30')).toEqual({ value: 'intro 10:30' });
		expect(parseTagFilter('intro 10:*')).toEqual({ value: 'intro 10:*' });
	});

	test('spelling is normalized', () => {
		const tag = t('  LoCation \t:\n  Jeri   coara\u0007 ');
		expect(tag.key).toBe('location');
		expect(tag.value).toBe('Jeri coara');
		expect(t('a\tb').value).toBe('a b');
	});

	test('whitespace and controls are the ones Rust means, not the ones /\\s/ means', () => {
		expect(t('a\u0085b').value).toBe('a b');
		expect(t('a\u00a0b').value).toBe('a b');
		expect(t('a\u3000b').value).toBe('a b');
		expect(t('a\u2028b').value).toBe('a b');
		expect(t('a\u200bb').value).toBe('a\u200bb');
		expect(t('\ufeffa').value).toBe('\ufeffa');
		expect(t('a\u007fb').value).toBe('ab');
		expect(t('a\u009fb').value).toBe('ab');
		expect(t('a\u001fb').value).toBe('ab');
	});

	test('empty and over-long ones are refused', () => {
		for (const bad of ['', '   ', 'location:', 'location:   ', ': Cumbuco', ' : x', '\u0007']) {
			expect(refused(() => parseTag(bad))).not.toBeNull();
		}
		expect(refused(() => newTag('k'.repeat(MAX_TAG_KEY), 'v'))).toBeNull();
		expect(refused(() => newTag('k'.repeat(MAX_TAG_KEY + 1), 'v'))).toContain('at most 32 characters');
		expect(refused(() => newTag(null, 'é'.repeat(MAX_TAG_VALUE)))).toBeNull();
		expect(refused(() => newTag(null, 'é'.repeat(MAX_TAG_VALUE + 1)))).toContain('at most 64 characters');
		expect(refused(() => parseTag(''))).toBe('invalid argument: a tag value cannot be empty');
		expect(refused(() => parseTag(': x'))).toBe('invalid argument: a tag key cannot be empty');
	});

	test('length counts characters, not UTF-16 units', () => {
		expect(refused(() => newTag(null, '😀'.repeat(MAX_TAG_VALUE)))).toBeNull();
		expect(refused(() => newTag(null, '😀'.repeat(MAX_TAG_VALUE + 1)))).not.toBeNull();
		expect(refused(() => normalizeKey('😀'.repeat(MAX_TAG_KEY)))).toBeNull();
	});

	test('is stored without a missing key', () => {
		expect(JSON.stringify(t('jump'))).toBe('{"value":"jump"}');
		expect(JSON.stringify(t('rider: Orell'))).toBe('{"key":"rider","value":"Orell"}');
		expect(JSON.parse(JSON.stringify(t('rider: Orell')))).toEqual(t('rider: Orell'));
		expect('key' in newTag(undefined, 'x')).toBe(false);
	});

	test('repeats ignore case and the first spelling stays', () => {
		const tags = dedupeTags(list(['location: Cumbuco', 'Location: CUMBUCO', 'jump', 'Jump', 'rider: A', 'rider: B']));
		expect(shown(tags)).toEqual(['location: Cumbuco', 'jump', 'rider: A', 'rider: B']);
		expect(sameTag(t('jump'), t('JUMP'))).toBe(true);
		expect(sameTag(t('jump'), t('x: jump'))).toBe(false);
		expect(sameTags(list(['a', 'b: C']), list(['B: c', 'A']))).toBe(true);
		expect(sameTags(list(['a']), list(['a', 'b']))).toBe(false);
	});
});

describe('a tag edit', () => {
	test('removes keys, then tags, then adds', () => {
		const tags = list(['location: Cumbuco', 'rider: Orell', 'rider: Sam', 'jump']);
		const edit: TagEdit = { add: list(['location: Jeri', 'wave']), remove: list(['JUMP']), remove_keys: ['Location'] };
		expect(shown(applyTagEdit(tags, edit))).toEqual(['rider: Orell', 'rider: Sam', 'location: Jeri', 'wave']);
		const swap: TagEdit = { add: list(['jump']), remove: list(['jump']) };
		expect(shown(applyTagEdit(tags, swap))).toEqual(['location: Cumbuco', 'rider: Orell', 'rider: Sam', 'jump']);
		expect(shown(applyTagEdit(tags, { remove_keys: ['rider'] }))).toEqual(['location: Cumbuco', 'jump']);
	});

	test('takes off exactly the tag it names', () => {
		const tags = list(['location: Cumbuco', 'Cumbuco']);
		expect(shown(applyTagEdit(tags, { remove: list(['cumbuco']) }))).toEqual(['location: Cumbuco']);
		expect(shown(applyTagEdit(tags, { remove: list(['location: cumbuco']) }))).toEqual(['Cumbuco']);
		expect(applyTagEdit(tags, { remove: list(['nothing']) })).toEqual(tags);
	});

	test('normalizes what it adds and refuses what is invalid', () => {
		const sloppy: TagEdit = { add: [{ key: ' Location ', value: '  Cumbuco  ' }] };
		expect(shown(applyTagEdit([], sloppy))).toEqual(['location: Cumbuco']);
		const bad: TagEdit[] = [
			{ add: [{ value: '  ' }] },
			{ remove: [{ key: '', value: 'x' }] },
			{ remove_keys: [' '] }
		];
		for (const edit of bad) expect(refused(() => applyTagEdit(list(['a']), edit))).not.toBeNull();
	});

	test('an asset holds at most so many tags', () => {
		const full = Array.from({ length: MAX_ASSET_TAGS }, (_, i) => t(`tag${i}`));
		expect(refused(() => applyTagEdit(full, { add: list(['one more']) }))).toContain('at most 64 tags');
		expect(applyTagEdit(full, { add: list(['one more']), remove: list(['tag0']) })).toHaveLength(MAX_ASSET_TAGS);
		expect(applyTagEdit(full, { add: list(['TAG1']) })).toHaveLength(MAX_ASSET_TAGS);
	});

	test('run later over tags that changed never refuses and leaves the overflow off', () => {
		const full = Array.from({ length: MAX_ASSET_TAGS - 1 }, (_, i) => t(`t${i}`));
		const edit: TagEdit = { add: list(['x1', 'x2', 't0']) };
		expect(refused(() => applyTagEdit(full, edit))).not.toBeNull();
		const landed = applyTagEditCapped(full, edit);
		expect(landed).toHaveLength(MAX_ASSET_TAGS);
		expect(shown(landed.slice(MAX_ASSET_TAGS - 1))).toEqual(['x1']);
		const broken: TagEdit = { add: [{ value: '  ' }, t('ok')], remove_keys: [' '] };
		expect(shown(applyTagEditCapped(list(['a']), broken))).toEqual(['a', 'ok']);
	});

	test('with nothing in it is empty', () => {
		expect(tagEditIsEmpty({})).toBe(true);
		expect(tagEditIsEmpty({ add: [], remove: [], remove_keys: [] })).toBe(true);
		expect(tagEditIsEmpty({ remove_keys: ['x'] })).toBe(false);
	});

	test('does not change the tags it was given', () => {
		const tags = list(['a', 'b: c']);
		const before = structuredClone(tags);
		applyTagEdit(tags, { add: list(['d']), remove_keys: ['b'] });
		expect(tags).toEqual(before);
	});
});

describe('two tag edits in a row', () => {
	test('add up keys and removals and drop the adds a later edit takes off', () => {
		const first: TagEdit = { add: list(['jump', 'location: Jeri', 'wave']), remove: list(['old']), remove_keys: ['Rider'] };
		const later: TagEdit = { add: list(['Wave2']), remove: list(['WAVE']), remove_keys: ['location', 'rider'] };
		const both = tagEditThen(first, later);
		expect(shown(both.add)).toEqual(['jump', 'Wave2']);
		expect(shown(both.remove)).toEqual(['old', 'WAVE']);
		expect(both.remove_keys).toEqual(['rider', 'location']);
		expect(tagEditThen({}, first)).toEqual({ add: list(['jump', 'location: Jeri', 'wave']), remove: list(['old']), remove_keys: ['rider'] });
		expect(tagEditIsEmpty(tagEditThen({}, {}))).toBe(true);
		expect(refused(() => tagEditThen(first, { add: [{ value: ' ' }] }))).not.toBeNull();
	});

	// The engine's `two_edits_in_a_row_are_one_edit`: the composed edit does what the two do, one after the other.
	test('do what the composed edit does, over any tags', () => {
		const universe = list(['a', 'A', 'b', 'k: x', 'K: X', 'k: y', 'm: z']);
		let seed = 0x2545f491;
		const next = () => {
			seed ^= seed << 13;
			seed ^= seed >>> 17;
			seed ^= seed << 5;
			return seed >>> 0;
		};
		const pick = () => Array.from({ length: next() % 3 }, () => universe[next() % universe.length]);
		const edits: TagEdit[] = Array.from({ length: 300 }, () => ({
			add: pick(),
			remove: pick(),
			remove_keys: ['k', 'M', 'zz'].filter(() => next() % 4 === 0)
		}));
		const starts = [list([]), list(['a', 'k: x']), list(['B', 'k: Y', 'm: z', 'j'])];
		for (let i = 0; i + 1 < edits.length; i++) {
			const composed = tagEditThen(edits[i], edits[i + 1]);
			for (const start of starts) {
				const stepwise = applyTagEditCapped(applyTagEditCapped(start, edits[i]), edits[i + 1]);
				expect(applyTagEditCapped(start, composed)).toEqual(stepwise);
			}
		}
	});
});

describe('a tag filter', () => {
	const term = (key?: string, value?: string): TagFilter => ({ ...(key !== undefined ? { key } : {}), ...(value !== undefined ? { value } : {}) });

	test('term matches a key, a pair or a plain tag', () => {
		const tags = list(['location: Cumbuco', 'rider: Orell', 'jump']);
		expect(tagFilterMatches(term('location'), tags)).toBe(true);
		expect(tagFilterMatches(term('LOCATION', 'cumbuco'), tags)).toBe(true);
		expect(tagFilterMatches(term('location', 'Jeri'), tags)).toBe(false);
		expect(tagFilterMatches(term('wind'), tags)).toBe(false);
		expect(tagFilterMatches(term(undefined, 'JUMP'), tags)).toBe(true);
		expect(tagFilterMatches(term(undefined, 'Cumbuco'), tags)).toBe(false);
		expect(tagFilterMatches(term(undefined, 'jump'), list(['style: jump']))).toBe(false);
	});

	test('terms all have to hold', () => {
		const tags = list(['location: Cumbuco', 'rider: Orell']);
		const all = [parseTagFilter('location:*'), parseTagFilter('rider: orell')];
		expect(tagsMatch(all, tags)).toBe(true);
		expect(tagsMatch([], tags)).toBe(true);
		const oneOff = [parseTagFilter('location: Cumbuco'), parseTagFilter('jump')];
		expect(tagsMatch(oneOff, tags)).toBe(false);
		expect(tagsMatch(all, list(['location: Cumbuco']))).toBe(false);
	});

	test('is parsed and checked', () => {
		expect(parseTagFilter(' Location : * ')).toEqual({ key: 'location' });
		expect(parseTagFilter('location: Cumbuco')).toEqual({ key: 'location', value: 'Cumbuco' });
		expect(parseTagFilter('jump')).toEqual({ value: 'jump' });
		for (const bad of ['', '  ', 'location:', ': x']) expect(refused(() => parseTagFilter(bad))).not.toBeNull();
		expect(refused(() => checkTagFilter({}))).toBe('invalid argument: a tag filter names a key, a value or both');
		expect(JSON.stringify(parseTagFilter('location:*'))).toBe('{"key":"location"}');
	});

	test('a value is not length-capped, only non-empty', () => {
		expect(refused(() => checkTagFilter({ value: 'x'.repeat(MAX_TAG_VALUE + 10) }))).toBeNull();
		expect(refused(() => checkTagFilter({ key: 'k'.repeat(MAX_TAG_KEY + 1) }))).not.toBeNull();
		expect(refused(() => checkTagFilter({ value: ' ' }))).toBe('invalid argument: a tag filter value cannot be empty');
	});
});

describe('the vocabulary', () => {
	test('counts assets and sorts', () => {
		const a = list(['location: Jeri', 'rider: Orell', 'jump']);
		const b = list(['location: Cumbuco', 'Rider: orell', 'wave']);
		const c = list(['location: cumbuco', 'jump', 'Jump']);
		const summary = tagSummary([a, b, c, []]);
		expect(summary.keys.map((k) => [k.key, k.assets, k.values.map((v) => [v.value, v.assets])])).toEqual([
			['location', 3, [['Cumbuco', 2], ['Jeri', 1]]],
			['rider', 2, [['Orell', 2]]]
		]);
		expect(summary.plain.map((v) => [v.value, v.assets])).toEqual([['jump', 2], ['wave', 1]]);
		expect(tagSummary([])).toEqual({ keys: [], plain: [] });
	});

	test('sorts by code point, as Rust sorts a string', () => {
		const summary = tagSummary([list(['b']), list(['a']), list(['\u{1f600}']), list(['\uff21'])]);
		expect(summary.plain.map((v) => v.value)).toEqual(['a', 'b', '\uff21', '\u{1f600}']);
	});
});

describe('grouping by a key', () => {
	const groups = (assets: string[][], key: string, order: string[] = [], untagged = false) =>
		groupByKey(assets.map(list), key, order, untagged);
	const outline = (g: ReturnType<typeof groupByKey>) => g.groups.map((x) => [x.value, x.members]);

	test('run A to Z by default and members keep their order', () => {
		const g = groups([['location: Jeri'], ['location: Cumbuco'], ['location: jeri'], ['location: Atins']], 'Location');
		expect(outline(g)).toEqual([['Atins', [3]], ['Cumbuco', [1]], ['Jeri', [0, 2]]]);
		expect(g.unmatched).toEqual([]);
	});

	test('an order goes first and the rest follow A to Z', () => {
		const assets = [['location: Jeri'], ['location: Cumbuco'], ['location: Atins'], ['location: Prea']];
		const g = groups(assets, 'location', ['prea', 'JERI', 'Nowhere', 'jeri']);
		expect(outline(g)).toEqual([['Prea', [3]], ['Jeri', [0]], ['Atins', [2]], ['Cumbuco', [1]]]);
		expect(g.unmatched).toEqual(['Nowhere']);
	});

	test('assets without the key come last only when asked', () => {
		const assets = [['jump'], ['location: Jeri'], [], ['rider: Orell']];
		expect(outline(groups(assets, 'location'))).toEqual([['Jeri', [1]]]);
		expect(outline(groups(assets, 'location', [], true))).toEqual([['Jeri', [1]], [null, [0, 2, 3]]]);
		expect(outline(groups(assets, 'wind', [], true))).toEqual([[null, [0, 1, 2, 3]]]);
		expect(groups(assets, 'wind').groups).toEqual([]);
	});

	test('an asset with several values joins the first in order', () => {
		const assets = [['rider: Sam', 'rider: Orell'], ['rider: Sam']];
		expect(outline(groups(assets, 'rider'))).toEqual([['Orell', [0]], ['Sam', [1]]]);
		expect(outline(groups(assets, 'rider', ['sam']))).toEqual([['Sam', [0, 1]]]);
	});

	test('refuse an empty key or order entry', () => {
		expect(refused(() => groupByKey([], ' '))).not.toBeNull();
		expect(refused(() => groupByKey([], 'location', ['  ']))).toBe('invalid argument: an empty value in the order');
	});

	test('a group is named in the spelling its first member gave it', () => {
		expect(outline(groups([['location: JERI'], ['location: Jeri']], 'location'))).toEqual([['JERI', [0, 1]]]);
	});
});

describe('a staged change', () => {
	test('is a tag change when it carries a tag edit, and a projection mark otherwise', () => {
		expect(isTagChange({ tag_edit: { add: [t('jump')] } })).toBe(true);
		expect(isTagChange({ tag_edit: {} })).toBe(true);
		expect(isTagChange({})).toBe(false);
	});
});

// ---- planAssembleByTag: Project::assemble_by_tag_inputs ----------------------------------------------------------

let nextAt = 0;
function asset(name: string, tags: string[], over: Partial<Asset> = {}): Asset {
	nextAt += 1;
	return {
		id: `00000000-0000-0000-0000-${String(nextAt).padStart(12, '0')}`,
		path: `/footage/${name}.mp4`,
		name: `${name}.mp4`,
		duration: 10,
		streams: [{ index: 0, kind: 'video', codec: 'h264', width: 1920, height: 1080, fps: 30 }],
		imported_at: new Date(Date.UTC(2026, 0, 1, 0, 0, nextAt)).toISOString(),
		...(tags.length ? { tags: list(tags) } : {}),
		...over
	};
}

/** Six 10 s assets: two in Jeri, two in Cumbuco, one in Atins and one with no location (the engine's `trip`). */
const trip = () => [
	asset('jeri-a', ['location: Jeri', 'jump']),
	asset('cumbuco-a', ['location: Cumbuco', 'jump']),
	asset('atins', ['location: Atins']),
	asset('jeri-b', ['location: Jeri']),
	asset('cumbuco-b', ['location: Cumbuco', 'wave']),
	asset('loose', ['jump'])
];

describe('planning an assembly', () => {
	test('groups come out in order, imported order within a group', () => {
		const a = trip();
		const plan = planAssembleByTag(a, { key: 'Location', order: ['jeri'] });
		expect(plan.groups.map((g) => g.value)).toEqual(['Jeri', 'Atins', 'Cumbuco']);
		expect(plan.groups.map((g) => g.assets.map((x) => x.id))).toEqual([[a[0].id, a[3].id], [a[2].id], [a[1].id, a[4].id]]);
		expect(plan.key).toBe('location');
		expect(plan.title).toBe('overlay');
		expect(plan.title_seconds).toBe(3);
		expect(plan.unmatched).toEqual([]);
	});

	test('a filter keeps the assets that match every term', () => {
		const a = trip();
		const plan = planAssembleByTag(a, { key: 'location', filter: [{ value: 'jump' }], title: 'card', title_seconds: 2 });
		expect(plan.groups.map((g) => g.value)).toEqual(['Cumbuco', 'Jeri']);
		expect(plan.title).toBe('card');
		expect(plan.title_seconds).toBe(2);
	});

	test('untagged assets come last only when asked', () => {
		const a = trip();
		const plan = planAssembleByTag(a, { key: 'location', include_untagged: true });
		expect(plan.groups).toHaveLength(4);
		expect(plan.groups[3].value).toBeNull();
		expect(plan.groups[3].assets.map((x) => x.id)).toEqual([a[5].id]);
	});

	test('assets are ordered within a group by import or by name', () => {
		const by = (sort_within?: 'imported' | 'name') => {
			const lib = [asset('b-shot', ['spot: X']), asset('c-shot', ['spot: X']), asset('a-shot', ['spot: X'])];
			return planAssembleByTag(lib, { key: 'spot', sort_within }).groups[0].assets.map((x) => x.name);
		};
		expect(by('imported')).toEqual(['b-shot.mp4', 'c-shot.mp4', 'a-shot.mp4']);
		expect(by()).toEqual(['b-shot.mp4', 'c-shot.mp4', 'a-shot.mp4']);
		expect(by('name')).toEqual(['a-shot.mp4', 'b-shot.mp4', 'c-shot.mp4']);
	});

	test('order values no asset carries are reported', () => {
		const plan = planAssembleByTag(trip(), { key: 'location', order: ['Fortaleza', 'cumbuco'] });
		expect(plan.unmatched).toEqual(['Fortaleza']);
		expect(plan.groups[0].value).toBe('Cumbuco');
	});

	test('only picture and sound with some length can be laid down', () => {
		const lib = [
			asset('still-data', ['spot: X'], { streams: [{ index: 0, kind: 'data', codec: 'none' }] }),
			asset('empty', ['spot: X'], { duration: 0 }),
			asset('song', ['spot: X'], { streams: [{ index: 0, kind: 'audio', codec: 'aac', sample_rate: 48000, channels: 2 }] })
		];
		const plan = planAssembleByTag(lib, { key: 'spot' });
		expect(plan.groups[0].assets.map((x) => x.name)).toEqual(['song.mp4']);
	});

	test('a request nothing answers is refused', () => {
		const a = trip();
		const requests = [
			{ key: 'wind' },
			{ key: 'location', filter: [{ key: 'rider' }] },
			{ key: '  ' },
			{ key: 'location', title_seconds: 0 },
			{ key: 'location', title_seconds: Number.NaN },
			{ key: 'location', title_seconds: 61 },
			{ key: 'location', filter: [{}] },
			{ key: 'location', order: [' '] }
		];
		for (const req of requests) expect(refused(() => planAssembleByTag(a, req))).not.toBeNull();
		expect(refused(() => planAssembleByTag(a, { key: 'wind' }))).toBe('invalid argument: no asset has a "wind" tag');
		expect(refused(() => planAssembleByTag(a, { key: 'location', filter: [{ key: 'rider' }] }))).toContain('and matches the filter');
		expect(refused(() => planAssembleByTag(a, { key: 'location', title_seconds: 60 }))).toBeNull();
		expect(refused(() => planAssembleByTag(a, { key: 'location', title_seconds: 61 }))).toContain('at most 60');
		expect(refused(() => planAssembleByTag([], { key: 'location' }))).not.toBeNull();
	});

	test('a title or sort the engine does not know is refused as its request reader refuses it', () => {
		const a = trip();
		for (const title of ['banner', 'Overlay', '']) {
			expect(refused(() => planAssembleByTag(a, { key: 'location', title: title as 'card' }))).toContain('unknown title');
		}
		expect(refused(() => planAssembleByTag(a, { key: 'location', sort_within: 'newest' as 'name' }))).toContain('unknown sort_within');
		for (const title of ['overlay', 'card', 'none'] as const) {
			expect(refused(() => planAssembleByTag(a, { key: 'location', title }))).toBeNull();
		}
	});
});
