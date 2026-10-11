import { describe, expect, test } from 'bun:test';
import type { MenuItem } from './context-menu.svelte';
import { contextRows } from './menu-items';
import { firstRow, focusableRows, lastRow, menuKey, radioKey, rowFor, stepRow, typeaheadRow, type NavRow } from './menu-nav';

const row = (label: string, focusable = true): NavRow => ({ label, focusable });
const rows = [row('Header', false), row('Copy'), row('Cut'), row('', false), row('Paste'), row('Delete')];

describe('stepping', () => {
	test('down and up skip what cannot take focus', () => {
		expect(stepRow(rows, 1, 1)).toBe(2);
		expect(stepRow(rows, 2, 1)).toBe(4);
		expect(stepRow(rows, 4, -1)).toBe(2);
	});
	test('both ends wrap', () => {
		expect(stepRow(rows, 5, 1)).toBe(1);
		expect(stepRow(rows, 1, -1)).toBe(5);
	});
	test('from no row down goes to the first and up to the last', () => {
		expect(stepRow(rows, -1, 1)).toBe(1);
		expect(stepRow(rows, -1, -1)).toBe(5);
		expect(stepRow(rows, 3, 1)).toBe(1);
	});
	test('a menu with nothing to focus stays put', () => {
		expect(stepRow([row('Fact', false)], 0, 1)).toBe(0);
		expect(stepRow([], -1, 1)).toBe(-1);
	});
	test('first and last', () => {
		expect(firstRow(rows)).toBe(1);
		expect(lastRow(rows)).toBe(5);
		expect(firstRow([])).toBe(-1);
		expect(focusableRows(rows)).toEqual([1, 2, 4, 5]);
	});
});

describe('typing a letter', () => {
	test('goes to the next row that starts with it, then round', () => {
		const r = [row('Copy'), row('Cut'), row('Paste'), row('Clear')];
		expect(typeaheadRow(r, 0, 'c')).toBe(1);
		expect(typeaheadRow(r, 1, 'c')).toBe(3);
		expect(typeaheadRow(r, 3, 'c')).toBe(0);
		expect(typeaheadRow(r, 0, 'P')).toBe(2);
		expect(typeaheadRow(r, 0, 'z')).toBeNull();
	});
	test('does not stop on a row that cannot take focus', () => {
		expect(typeaheadRow([row('Copy'), row('Cut', false), row('Clear')], 0, 'c')).toBe(2);
	});
});

describe('which key does what', () => {
	const key = (k: string, extra = {}) => menuKey({ key: k, ...extra });
	test('the menu keys', () => {
		expect(key('ArrowDown')).toEqual({ do: 'next' });
		expect(key('ArrowUp')).toEqual({ do: 'prev' });
		expect(key('Home')).toEqual({ do: 'first' });
		expect(key('End')).toEqual({ do: 'last' });
		expect(key('Enter')).toEqual({ do: 'run' });
		expect(key(' ')).toEqual({ do: 'run' });
		expect(key('Escape')).toEqual({ do: 'close' });
		expect(key('Tab')).toEqual({ do: 'leave' });
		expect(key('Tab', { shiftKey: true })).toEqual({ do: 'leave' });
	});
	test('a letter or a digit is typeahead, a capital too', () => {
		expect(key('c')).toEqual({ do: 'type', ch: 'c' });
		expect(key('C', { shiftKey: true })).toEqual({ do: 'type', ch: 'C' });
		expect(key('7')).toEqual({ do: 'type', ch: '7' });
	});
	test('a chord, a function key and punctuation go on to whoever wanted them', () => {
		expect(key('c', { ctrlKey: true })).toBeNull();
		expect(key('z', { metaKey: true })).toBeNull();
		expect(key('ArrowDown', { altKey: true })).toBeNull();
		expect(key('F5')).toBeNull();
		expect(key(',')).toBeNull();
		expect(key('Shift')).toBeNull();
	});
});

describe('where a key sends focus', () => {
	test('arrows, Home and End', () => {
		expect(rowFor(rows, 1, { do: 'next' })).toBe(2);
		expect(rowFor(rows, 1, { do: 'prev' })).toBe(5);
		expect(rowFor(rows, 4, { do: 'first' })).toBe(1);
		expect(rowFor(rows, 1, { do: 'last' })).toBe(5);
	});
	test('a letter with no match moves nothing; run and close are not moves', () => {
		expect(rowFor(rows, 1, { do: 'type', ch: 'q' })).toBeNull();
		expect(rowFor(rows, 1, { do: 'run' })).toBeNull();
		expect(rowFor(rows, 1, { do: 'close' })).toBeNull();
	});
	test('an empty menu has no first', () => {
		expect(rowFor([], -1, { do: 'first' })).toBeNull();
	});
});

describe('the context menu as rows', () => {
	test('actions take focus, even disabled ones; headers, facts and rules do not', () => {
		const items: MenuItem[] = [
			{ type: 'header', label: 'interview.mp4', sub: '1920×1080' },
			{ type: 'info', label: 'Frame', value: '30 fps' },
			{ type: 'separator' },
			{ label: 'Add at playhead', action: () => {} },
			{ label: 'Separate stems…', disabled: true, reason: 'No sound', action: () => {} }
		];
		expect(contextRows(items)).toEqual([
			{ focusable: false, label: '' },
			{ focusable: false, label: '' },
			{ focusable: false, label: '' },
			{ focusable: true, label: 'Add at playhead' },
			{ focusable: true, label: 'Separate stems…' }
		]);
	});
});

describe('a group of radio buttons', () => {
	test('arrows on either axis step to the neighbour, Home and End jump', () => {
		expect(radioKey({ key: 'ArrowRight' })).toEqual({ do: 'next' });
		expect(radioKey({ key: 'ArrowDown' })).toEqual({ do: 'next' });
		expect(radioKey({ key: 'ArrowLeft' })).toEqual({ do: 'prev' });
		expect(radioKey({ key: 'ArrowUp' })).toEqual({ do: 'prev' });
		expect(radioKey({ key: 'Home' })).toEqual({ do: 'first' });
		expect(radioKey({ key: 'End' })).toEqual({ do: 'last' });
	});
	test('a chord, a letter, Enter and Tab are not the group’s', () => {
		for (const e of [{ key: 'a' }, { key: 'Enter' }, { key: 'Tab' }, { key: 'ArrowRight', ctrlKey: true }, { key: 'ArrowRight', shiftKey: true }])
			expect(radioKey(e)).toBeNull();
	});
	test('a disabled option is stepped over', () => {
		const chips = [row('720'), row('1080', false), row('1280'), row('Off')];
		expect(rowFor(chips, 0, { do: 'next' })).toBe(2);
		expect(rowFor(chips, 3, { do: 'next' })).toBe(0);
		expect(rowFor(chips, 0, { do: 'prev' })).toBe(3);
	});
});
