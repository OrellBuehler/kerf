import { describe, expect, test } from 'bun:test';
import {
	MAX_SUGGESTIONS,
	addNote,
	countLabel,
	dialogTitle,
	filterByTags,
	filterGroups,
	namesLine,
	parseTagInput,
	pendingEdit,
	planAdd,
	rowChips,
	sameTerm,
	suggestTags,
	tagsSignature,
	tallyNote,
	tallyTags,
	taggedLine,
	termLabel,
	toggleTerm
} from './tag-ui';
import { parseTag, tagSummary } from './tags';
import type { Tag, TagFilter } from './types';

const list = (texts: string[]): Tag[] => texts.map(parseTag);
const labels = (s: ReturnType<typeof suggestTags>) => s.map((x) => x.label);

// A library: five clips by spot and rider, two of them jumps.
const library = [
	list(['location: Cumbuco', 'rider: Orell', 'jump']),
	list(['location: Cumbuco', 'rider: Anna']),
	list(['location: Jericoacoara', 'rider: Orell', 'jump']),
	list(['location: Cumbuco', 'lagoon']),
	list([])
];
const vocab = tagSummary(library);

describe('suggestTags', () => {
	test('keys come first, then plain tags', () => {
		expect(labels(suggestTags('', vocab))).toEqual(['location:', 'rider:', 'jump', 'lagoon']);
	});

	test('a key that starts with the typed word is offered, and accepting it types the key and a space', () => {
		const s = suggestTags('loc', vocab);
		expect(s[0]).toEqual({ kind: 'key', label: 'location:', insert: 'location: ', assets: 4 });
		expect(s).toHaveLength(1);
	});

	test('after the colon it is the values of that key, the most used first', () => {
		const s = suggestTags('location: ', vocab);
		expect(labels(s)).toEqual(['location: Cumbuco', 'location: Jericoacoara']);
		expect(s.map((x) => x.assets)).toEqual([3, 1]);
		expect(s[0]).toMatchObject({ kind: 'tag', tag: { key: 'location', value: 'Cumbuco' } });
	});

	test('the value is matched from its start first, then from a word in it, then anywhere', () => {
		const v = tagSummary([list(['spot: Carre']), list(['spot: Big Reef']), list(['spot: Reef']), list(['spot: Sand'])]);
		expect(labels(suggestTags('spot: re', v))).toEqual(['spot: Reef', 'spot: Big Reef', 'spot: Carre']);
		expect(labels(suggestTags('spot: ree', v))).toEqual(['spot: Reef', 'spot: Big Reef']);
	});

	test('the key before the colon is read the way a key is stored: trimmed, lowercased', () => {
		expect(labels(suggestTags('  LOCATION :cu', vocab))).toEqual(['location: Cumbuco']);
	});

	test('a key the library does not have has nothing to offer a value', () => {
		expect(suggestTags('weather: ', vocab)).toEqual([]);
		expect(suggestTags(': x', vocab)).toEqual([]);
	});

	test('plain tags answer a plain word, and keyed values answer it by a word they start with', () => {
		expect(labels(suggestTags('j', vocab))).toEqual(['jump', 'location: Jericoacoara']);
		// "ri" is the start of a key, not of a value; "ra" holds no word beginning with it
		expect(labels(suggestTags('cum', vocab))).toEqual(['location: Cumbuco']);
	});

	test('what is already pending, or already everywhere, is not offered again', () => {
		const taken = list(['location: Cumbuco', 'jump']);
		expect(labels(suggestTags('location: ', vocab, taken))).toEqual(['location: Jericoacoara']);
		expect(labels(suggestTags('', vocab, taken))).toEqual(['location:', 'rider:', 'lagoon']);
	});

	test('never more than the limit, and an empty vocabulary offers nothing', () => {
		const wide = tagSummary([Array.from({ length: 30 }, (_, i) => parseTag(`k${i}: v`))]);
		expect(suggestTags('', wide)).toHaveLength(MAX_SUGGESTIONS);
		expect(suggestTags('', wide, [], 3)).toHaveLength(3);
		expect(suggestTags('anything', { keys: [], plain: [] })).toEqual([]);
	});

	test('ties go to the most used, then A to Z', () => {
		const v = tagSummary([list(['b', 'a']), list(['b']), list(['c'])]);
		expect(labels(suggestTags('', v))).toEqual(['b', 'a', 'c']);
	});
});

describe('parseTagInput', () => {
	test('reads key: value and a plain word, the way the engine spells them', () => {
		expect(parseTagInput('  Location :  Cumbuco  Beach ')).toEqual({ tag: { key: 'location', value: 'Cumbuco Beach' } });
		expect(parseTagInput('jump')).toEqual({ tag: { value: 'jump' } });
	});

	test('says why a text is not a tag, in a sentence', () => {
		expect(parseTagInput('location:')).toEqual({ error: 'A tag value cannot be empty' });
		expect(parseTagInput(': Cumbuco')).toEqual({ error: 'A tag key cannot be empty' });
		expect(parseTagInput('x'.repeat(65))).toMatchObject({ error: expect.stringMatching(/^A tag value is at most 64 characters/) });
		expect('error' in parseTagInput('   ')).toBe(true);
	});
});

describe('tallyTags', () => {
	test('counts the tags of a selection, the ones on every asset first', () => {
		const t = tallyTags([list(['location: Cumbuco', 'jump']), list(['location: Cumbuco', 'rider: Anna']), list(['LOCATION: cumbuco'])]);
		expect(t.map((x) => [x.tag.key ? `${x.tag.key}: ${x.tag.value}` : x.tag.value, x.count, x.all])).toEqual([
			['location: Cumbuco', 3, true],
			['rider: Anna', 1, false],
			['jump', 1, false]
		]);
		expect(t.every((x) => x.of === 3)).toBe(true);
	});

	test('keeps the first spelling and counts a repeated tag of one asset once', () => {
		const t = tallyTags([[{ value: 'Jump' }, { value: 'jump' }], [{ value: 'JUMP' }]]);
		expect(t).toEqual([{ tag: { value: 'Jump' }, count: 2, of: 2, all: true }]);
	});

	test('a single asset has everything "all"; nothing tagged is an empty tally', () => {
		expect(tallyTags([list(['a', 'b'])]).every((x) => x.all)).toBe(true);
		expect(tallyTags([[], []])).toEqual([]);
		expect(tallyTags([])).toEqual([]);
	});

	test('says all or some (n) once there is more than one asset', () => {
		expect(tallyNote({ count: 3, of: 3, all: true })).toBe('all');
		expect(tallyNote({ count: 2, of: 3, all: false })).toBe('some (2)');
		expect(tallyNote({ count: 1, of: 1, all: true })).toBe('');
	});
});

describe('what a typed tag does to the dialog', () => {
	const tally = tallyTags([list(['location: Cumbuco', 'jump']), list(['location: Cumbuco'])]);
	const t = parseTag;

	test('a new tag is added', () => {
		expect(planAdd(t('rider: Orell'), [], [], tally)).toBe('add');
	});

	test('one that is on only some of the assets is added, so it lands on all of them', () => {
		expect(planAdd(t('jump'), [], [], tally)).toBe('add');
	});

	test('one on every asset adds nothing, and says so', () => {
		const plan = planAdd(t('LOCATION: cumbuco'), [], [], tally);
		expect(plan).toBe('present');
		expect(addNote(plan, t('location: Cumbuco'), 2)).toBe('location: Cumbuco is already on every one of these 2 assets.');
		expect(addNote(plan, t('location: Cumbuco'), 1)).toBe('location: Cumbuco is already on this asset.');
	});

	test('one already pending adds nothing', () => {
		const plan = planAdd(t('rider: orell'), [t('rider: Orell')], [], tally);
		expect(plan).toBe('pending');
		expect(addNote(plan, t('rider: orell'), 2)).toBe('rider: orell is already in the list.');
	});

	test('one marked for removal is kept instead of added', () => {
		expect(planAdd(t('location: Cumbuco'), [], [t('location: Cumbuco')], tally)).toBe('restore');
	});

	test('an add that went through has no note', () => {
		expect(addNote('add', t('x'), 2)).toBe('');
	});
});

describe('pendingEdit', () => {
	const t = parseTag;

	test('is the tags to add and the tags to take off', () => {
		expect(pendingEdit([t('rider: Orell')], [t('jump')])).toEqual({ add: [t('rider: Orell')], remove: [t('jump')] });
	});

	test('leaves out what is empty, and is null when there is nothing to do', () => {
		expect(pendingEdit([t('a')], [])).toEqual({ add: [t('a')] });
		expect(pendingEdit([], [t('a')])).toEqual({ remove: [t('a')] });
		expect(pendingEdit([], [])).toBeNull();
	});

	test('a tag both pending and marked for removal stays', () => {
		expect(pendingEdit([t('Jump')], [t('jump')])).toEqual({ add: [t('Jump')] });
		expect(pendingEdit([t('jump')], [t('jump')])).toEqual({ add: [t('jump')] });
	});
});

describe('the words of the dialog', () => {
	test('titles say what is tagged', () => {
		expect(dialogTitle('import', Array.from({ length: 12 }, (_, i) => `f${i}.mp4`))).toBe('Tag 12 imported files');
		expect(dialogTitle('import', ['a.mp4'])).toBe('Tag a.mp4');
		expect(dialogTitle('bulk', ['a.mp4', 'b.mp4', 'c.mp4'])).toBe('Tag 3 assets');
		expect(dialogTitle('bulk', ['a.mp4'])).toBe('Tag a.mp4');
	});

	test('the toast and the file list', () => {
		expect(taggedLine(['a.mp4'])).toBe('Tagged a.mp4');
		expect(taggedLine(['a.mp4', 'b.mp4'])).toBe('Tagged 2 assets');
		expect(namesLine(['a', 'b', 'c', 'd'])).toBe('a, b, c, d');
		expect(namesLine(['a', 'b', 'c', 'd', 'e', 'f'])).toBe('a, b, c and 3 more');
	});
});

describe('rowChips', () => {
	test('shows what fits and counts the rest', () => {
		const r = rowChips(list(['a: 1', 'b: 2', 'c: 3', 'd: 4', 'e: 5', 'f: 6']));
		expect(r.shown.map((x) => x.key)).toEqual(['a', 'b', 'c']);
		expect(r.hidden).toBe(3);
		expect(r.all).toBe('a: 1\nb: 2\nc: 3\nd: 4\ne: 5\nf: 6');
	});

	test('a row with exactly as many as fit shows them all', () => {
		const r = rowChips(list(['a', 'b', 'c']));
		expect(r.shown).toHaveLength(3);
		expect(r.hidden).toBe(0);
		expect(rowChips(list(['a', 'b', 'c', 'd']))).toMatchObject({ hidden: 1 });
		expect(rowChips(list(['a', 'b', 'c', 'd']), 2).shown).toHaveLength(2);
	});

	test('no tags, no chips', () => {
		expect(rowChips(undefined)).toEqual({ shown: [], hidden: 0, all: '' });
		expect(rowChips([])).toEqual({ shown: [], hidden: 0, all: '' });
	});
});

describe('the filter', () => {
	test('a term toggles in and out, ignoring case', () => {
		const a = toggleTerm([], { key: 'location', value: 'Cumbuco' });
		expect(a).toEqual([{ key: 'location', value: 'Cumbuco' }]);
		const b = toggleTerm(a, { key: 'rider' });
		expect(b).toHaveLength(2);
		expect(toggleTerm(b, { key: 'LOCATION', value: 'cumbuco' })).toEqual([{ key: 'rider' }]);
		expect(sameTerm({ key: 'a' }, { key: 'a', value: 'b' })).toBe(false);
	});

	test('terms read as the bin shows them', () => {
		expect(termLabel({ key: 'location', value: 'Cumbuco' })).toBe('location: Cumbuco');
		expect(termLabel({ key: 'location' })).toBe('location: any');
		expect(termLabel({ value: 'jump' })).toBe('jump');
	});

	const assets = library.map((tags, i) => ({ id: `a${i}`, tags }));
	const ids = (filter: TagFilter[]) => filterByTags(assets, filter).map((a) => a.id);

	test('every term must hold (AND), through tagsMatch', () => {
		expect(ids([])).toEqual(['a0', 'a1', 'a2', 'a3', 'a4']);
		expect(ids([{ key: 'location', value: 'cumbuco' }])).toEqual(['a0', 'a1', 'a3']);
		expect(ids([{ key: 'location', value: 'Cumbuco' }, { value: 'jump' }])).toEqual(['a0']);
		expect(ids([{ key: 'rider' }, { value: 'jump' }])).toEqual(['a0', 'a2']);
		expect(ids([{ value: 'nothing' }])).toEqual([]);
	});

	test('a plain term does not match a keyed tag of that value', () => {
		expect(ids([{ value: 'Cumbuco' }])).toEqual([]);
	});

	test('the header count is the total, or "3 of 12" once filtered', () => {
		expect(countLabel(12, 12, false)).toBe('12');
		expect(countLabel(3, 12, true)).toBe('3 of 12');
		expect(countLabel(12, 12, true)).toBe('12 of 12');
	});

	test('the menu groups the vocabulary by key and marks what is in the filter', () => {
		const g = filterGroups(vocab, [{ key: 'location', value: 'Cumbuco' }, { value: 'jump' }]);
		expect(g.map((x) => x.title)).toEqual(['location', 'rider', 'Plain tags']);
		expect(g[0].assets).toBe(4);
		expect(g[0].rows.map((r) => [r.label, r.assets, r.active])).toEqual([
			['Any location', 4, false],
			['Cumbuco', 3, true],
			['Jericoacoara', 1, false]
		]);
		expect(g[0].rows[1].term).toEqual({ key: 'location', value: 'Cumbuco' });
		expect(g[2].assets).toBeNull();
		expect(g[2].rows.map((r) => [r.label, r.active])).toEqual([
			['jump', true],
			['lagoon', false]
		]);
		expect(g[2].rows[0].term).toEqual({ value: 'jump' });
	});

	test('an empty vocabulary has no groups', () => {
		expect(filterGroups({ keys: [], plain: [] }, [])).toEqual([]);
	});
});

describe('tagsSignature', () => {
	test('moves when a tag is added, removed or renamed, and not for anything else', () => {
		const a = { id: '1', tags: list(['x: y']) };
		const before = tagsSignature([a, { id: '2' }]);
		expect(tagsSignature([{ ...a }, { id: '2', tags: [] }])).toBe(before);
		expect(tagsSignature([{ ...a, tags: list(['x: y', 'z']) }, { id: '2' }])).not.toBe(before);
		expect(tagsSignature([{ ...a, tags: list(['x: Y']) }, { id: '2' }])).not.toBe(before);
		expect(tagsSignature([a, { id: '2', tags: list(['q']) }])).not.toBe(before);
	});
});
