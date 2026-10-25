import { describe, expect, test } from 'bun:test';
import { collapsed, pickAsset, restrictToVisible, selectOnlyLabel, selectionOf } from './bin-selection';
import type { Selection } from './selection';

const none = { shiftKey: false, ctrlKey: false, metaKey: false };
const ctrl = { ...none, ctrlKey: true };
const cmd = { ...none, metaKey: true };
const shift = { ...none, shiftKey: true };

const order = ['a', 'b', 'c', 'd', 'e'];
const sel = (ids: string[], primary: string | null = ids.at(-1) ?? null): Selection => ({ ids, primary });

describe('picking assets in the bin', () => {
	test('a plain click selects that asset alone and makes it the primary', () => {
		expect(pickAsset(sel(['a', 'b']), 'c', none, order)).toEqual({ ids: ['c'], primary: 'c' });
	});

	test('Ctrl or Cmd toggles one asset in and out', () => {
		expect(pickAsset(sel(['a']), 'c', ctrl, order)).toEqual({ ids: ['a', 'c'], primary: 'c' });
		expect(pickAsset(sel(['a']), 'c', cmd, order)).toEqual({ ids: ['a', 'c'], primary: 'c' });
		expect(pickAsset(sel(['a', 'c']), 'c', ctrl, order)).toEqual({ ids: ['a'], primary: 'a' });
	});

	test('toggling off the last selected asset leaves nothing selected', () => {
		expect(pickAsset(sel(['a']), 'a', ctrl, order)).toEqual({ ids: [], primary: null });
	});

	test('Shift runs from the primary to the one clicked, either way', () => {
		expect(pickAsset(sel(['b']), 'd', shift, order)).toEqual({ ids: ['b', 'c', 'd'], primary: 'd' });
		const back = pickAsset(sel(['d']), 'b', shift, order);
		expect(back.ids.slice().sort()).toEqual(['b', 'c', 'd']);
		expect(back.primary).toBe('b');
	});

	test('Shift extends what is selected rather than replacing it', () => {
		expect(pickAsset(sel(['a', 'c']), 'e', shift, order).ids.slice().sort()).toEqual(['a', 'c', 'd', 'e']);
	});

	test('Shift with nothing selected, or an anchor the bin does not show, adds the one clicked', () => {
		expect(pickAsset(sel([]), 'c', shift, order)).toEqual({ ids: ['c'], primary: 'c' });
		expect(pickAsset(sel(['x']), 'c', shift, order)).toEqual({ ids: ['x', 'c'], primary: 'c' });
	});

	test('the range runs over the assets the bin shows, so a filtered-out asset is not swept in', () => {
		const shown = ['a', 'c', 'e'];
		expect(pickAsset(sel(['a']), 'e', shift, shown).ids).toEqual(['a', 'c', 'e']);
	});
});

describe('a filter that hides part of the selection', () => {
	test('takes the hidden ones out', () => {
		const next = restrictToVisible(sel(['a', 'b', 'c'], 'c'), new Set(['b', 'c']));
		expect(next).toEqual({ ids: ['b', 'c'], primary: 'c' });
	});

	test('hands the Inspector another asset when its own is hidden', () => {
		const next = restrictToVisible(sel(['a', 'b', 'c'], 'a'), new Set(['b', 'c']));
		expect(next.ids).toEqual(['b', 'c']);
		expect(next.primary).toBe('c');
	});

	test('leaves the selection alone when all of it shows', () => {
		const s = sel(['a', 'b']);
		expect(restrictToVisible(s, new Set(['a', 'b', 'c']))).toBe(s);
	});

	test('leaves it alone when none of it shows, so the Inspector keeps its asset', () => {
		const s = sel(['a', 'b'], 'b');
		expect(restrictToVisible(s, new Set(['c']))).toBe(s);
	});
});

describe('the selection as the editor holds it', () => {
	test('a primary without a set is a selection of one', () => {
		expect(selectionOf([], 'a')).toEqual({ ids: ['a'], primary: 'a' });
		expect(selectionOf([], null)).toEqual({ ids: [], primary: null });
	});

	test('a primary that is not in the set gives way to the last of it', () => {
		expect(selectionOf(['a', 'b'], 'z')).toEqual({ ids: ['a', 'b'], primary: 'b' });
		expect(selectionOf(['a', 'b'], 'a')).toEqual({ ids: ['a', 'b'], primary: 'a' });
	});

	test('Escape shrinks several to the primary and leaves a single one alone', () => {
		expect(collapsed(sel(['a', 'b', 'c'], 'b'))).toEqual({ ids: ['b'], primary: 'b' });
		expect(collapsed(sel(['a']))).toEqual({ ids: ['a'], primary: 'a' });
		expect(collapsed(sel([]))).toEqual({ ids: [], primary: null });
		expect(selectOnlyLabel('kite-0412.mp4')).toBe('Select only kite-0412.mp4');
	});
});
