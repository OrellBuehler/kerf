import { describe, expect, test } from 'bun:test';
import {
	NO_SELECTION,
	clickSelect,
	marqueeMode,
	marqueeSelect,
	normalize,
	pickMode,
	pruneSelection,
	sameIds,
	type Selection
} from './selection';

const sel = (ids: string[], primary: string | null = ids.at(-1) ?? null): Selection => ({ ids, primary });
const set = (s: Selection) => [...s.ids].sort();

const none = { shiftKey: false, ctrlKey: false, metaKey: false };

describe('modifiers', () => {
	test('a press with no modifier replaces, in both vocabularies', () => {
		expect(pickMode(none)).toBe('replace');
		expect(marqueeMode(none)).toBe('replace');
	});

	test('Shift extends a click along a track and adds to a marquee', () => {
		expect(pickMode({ ...none, shiftKey: true })).toBe('range');
		expect(marqueeMode({ ...none, shiftKey: true })).toBe('add');
	});

	test('Ctrl or Cmd toggles, for a click and a marquee alike', () => {
		for (const m of [{ ...none, ctrlKey: true }, { ...none, metaKey: true }]) {
			expect(pickMode(m)).toBe('toggle');
			expect(marqueeMode(m)).toBe('toggle');
		}
	});

	test('Shift wins when both are down', () => {
		expect(pickMode({ shiftKey: true, ctrlKey: true, metaKey: false })).toBe('range');
		expect(marqueeMode({ shiftKey: true, ctrlKey: true, metaKey: false })).toBe('add');
	});
});

describe('normalize', () => {
	test('drops duplicates and keeps a primary that is a member', () => {
		expect(normalize(['a', 'b', 'a'], 'a')).toEqual({ ids: ['a', 'b'], primary: 'a' });
	});

	test('a primary that is not in the set falls to the last member', () => {
		expect(normalize(['a', 'b'], 'zzz')).toEqual({ ids: ['a', 'b'], primary: 'b' });
		expect(normalize(['a', 'b'], null)).toEqual({ ids: ['a', 'b'], primary: 'b' });
	});

	test('an empty set has no primary', () => {
		expect(normalize([], 'a')).toEqual(NO_SELECTION);
	});
});

describe('clickSelect', () => {
	const track = ['c1', 'c2', 'c3', 'c4', 'c5'];

	test('a plain click selects only that clip', () => {
		expect(clickSelect(sel(['c1', 'c2']), 'c4', 'replace')).toEqual({ ids: ['c4'], primary: 'c4' });
	});

	test('clicking the only selected clip keeps it selected', () => {
		expect(clickSelect(sel(['c1']), 'c1', 'replace')).toEqual({ ids: ['c1'], primary: 'c1' });
	});

	test('toggle adds a clip that was not selected and makes it primary', () => {
		const out = clickSelect(sel(['c1'], 'c1'), 'c3', 'toggle');
		expect(set(out)).toEqual(['c1', 'c3']);
		expect(out.primary).toBe('c3');
	});

	test('toggle removes a selected clip and leaves the primary alone when it was another', () => {
		const out = clickSelect(sel(['c1', 'c2', 'c3'], 'c3'), 'c1', 'toggle');
		expect(set(out)).toEqual(['c2', 'c3']);
		expect(out.primary).toBe('c3');
	});

	test('toggling the primary off hands the Inspector whatever is left', () => {
		const out = clickSelect(sel(['c1', 'c2', 'c3'], 'c2'), 'c2', 'toggle');
		expect(set(out)).toEqual(['c1', 'c3']);
		expect(out.primary).toBe('c3');
	});

	test('toggling the last clip off empties the selection', () => {
		expect(clickSelect(sel(['c1']), 'c1', 'toggle')).toEqual(NO_SELECTION);
	});

	test('range takes everything between the primary and the click, both ways', () => {
		expect(set(clickSelect(sel(['c2'], 'c2'), 'c4', 'range', track))).toEqual(['c2', 'c3', 'c4']);
		expect(set(clickSelect(sel(['c4'], 'c4'), 'c2', 'range', track))).toEqual(['c2', 'c3', 'c4']);
	});

	test('range makes the clicked clip primary, so the next range anchors on it', () => {
		const first = clickSelect(sel(['c1'], 'c1'), 'c3', 'range', track);
		expect(first.primary).toBe('c3');
		expect(set(clickSelect(first, 'c5', 'range', track))).toEqual(['c1', 'c2', 'c3', 'c4', 'c5']);
	});

	test('range extends the selection rather than replacing it', () => {
		const out = clickSelect(sel(['x1', 'c2'], 'c2'), 'c3', 'range', track);
		expect(set(out)).toEqual(['c2', 'c3', 'x1']);
	});

	test('range with no anchor on that track just adds the clip', () => {
		const out = clickSelect(sel(['other'], 'other'), 'c3', 'range', track);
		expect(set(out)).toEqual(['c3', 'other']);
		expect(out.primary).toBe('c3');
		expect(set(clickSelect(NO_SELECTION, 'c3', 'range', track))).toEqual(['c3']);
		expect(set(clickSelect(sel(['c1']), 'c3', 'range', null))).toEqual(['c1', 'c3']);
	});

	test('does not change the selection it was given', () => {
		const before = sel(['c1', 'c2']);
		const copy = JSON.parse(JSON.stringify(before));
		clickSelect(before, 'c3', 'toggle');
		clickSelect(before, 'c3', 'range', track);
		clickSelect(before, 'c3', 'replace');
		expect(before).toEqual(copy);
	});
});

describe('marqueeSelect', () => {
	const base = sel(['a', 'b'], 'b');

	test('replace is what the marquee touches', () => {
		const out = marqueeSelect(base, ['c', 'd'], 'replace');
		expect(set(out)).toEqual(['c', 'd']);
		expect(out.primary).toBe('d');
	});

	test('replace with nothing touched clears', () => {
		expect(marqueeSelect(base, [], 'replace')).toEqual(NO_SELECTION);
	});

	test('add keeps what was selected and adds what is touched', () => {
		const out = marqueeSelect(base, ['b', 'c'], 'add');
		expect(set(out)).toEqual(['a', 'b', 'c']);
		expect(out.primary).toBe('c');
	});

	test('add with nothing new keeps the primary it had', () => {
		const out = marqueeSelect(base, ['a'], 'add');
		expect(set(out)).toEqual(['a', 'b']);
		expect(out.primary).toBe('b');
		expect(marqueeSelect(base, [], 'add')).toEqual({ ids: ['a', 'b'], primary: 'b' });
	});

	test('toggle flips every clip the marquee touches', () => {
		const out = marqueeSelect(base, ['b', 'c'], 'toggle');
		expect(set(out)).toEqual(['a', 'c']);
		expect(out.primary).toBe('c');
	});

	test('toggle that removes the primary falls to the last remaining', () => {
		const out = marqueeSelect(base, ['b'], 'toggle');
		expect(set(out)).toEqual(['a']);
		expect(out.primary).toBe('a');
	});

	test('is computed from the base, so a growing and shrinking rectangle does not drift', () => {
		const grown = marqueeSelect(base, ['c', 'd', 'e'], 'toggle');
		expect(set(grown)).toEqual(['a', 'b', 'c', 'd', 'e']);
		const shrunk = marqueeSelect(base, ['c'], 'toggle');
		expect(set(shrunk)).toEqual(['a', 'b', 'c']);
		const gone = marqueeSelect(base, [], 'toggle');
		expect(set(gone)).toEqual(['a', 'b']);
		expect(gone.primary).toBe('b');
	});

	test('a clip named twice is selected once', () => {
		expect(marqueeSelect(NO_SELECTION, ['a', 'a', 'b'], 'replace').ids).toEqual(['a', 'b']);
	});

	test('keeps the invariant: the primary is always a member', () => {
		for (const mode of ['replace', 'add', 'toggle'] as const) {
			for (const hits of [[], ['a'], ['b'], ['a', 'b'], ['c'], ['a', 'c']]) {
				const out = marqueeSelect(base, hits, mode);
				if (out.ids.length === 0) expect(out.primary).toBeNull();
				else expect(out.ids).toContain(out.primary as string);
				expect(new Set(out.ids).size).toBe(out.ids.length);
			}
		}
	});

	test('does not change the base', () => {
		const copy = JSON.parse(JSON.stringify(base));
		marqueeSelect(base, ['c'], 'toggle');
		expect(base).toEqual(copy);
	});
});

describe('pruneSelection', () => {
	test('drops clips that are gone and re-picks the primary if it was one', () => {
		const out = pruneSelection(sel(['a', 'b', 'c'], 'b'), (id) => id !== 'b');
		expect(out.ids).toEqual(['a', 'c']);
		expect(out.primary).toBe('c');
	});

	test('returns the very same selection when nothing was lost (no churn for reactive state)', () => {
		const s = sel(['a', 'b']);
		expect(pruneSelection(s, () => true)).toBe(s);
	});

	test('empties when everything is gone', () => {
		expect(pruneSelection(sel(['a']), () => false)).toEqual(NO_SELECTION);
	});
});

describe('sameIds', () => {
	test('compares as sets', () => {
		expect(sameIds(['a', 'b'], ['b', 'a'])).toBe(true);
		expect(sameIds(['a'], ['a', 'b'])).toBe(false);
		expect(sameIds([], [])).toBe(true);
		expect(sameIds(['a', 'b'], ['a', 'c'])).toBe(false);
	});
});
