import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { rowChecked, rowIcon, rowRole } from './menu-roles';

describe('rowRole / rowChecked / rowIcon', () => {
	test('an action is a menuitem with no checked state', () => {
		expect(rowRole({})).toBe('menuitem');
		expect(rowChecked({})).toBeUndefined();
		expect(rowIcon({ icon: 'copy' })).toBe('copy');
	});

	test('a radio is a menuitemradio, checked or not', () => {
		expect(rowRole({ role: 'radio', checked: true })).toBe('menuitemradio');
		expect(rowChecked({ role: 'radio', checked: true })).toBe(true);
		expect(rowChecked({ role: 'radio', checked: false })).toBe(false);
		expect(rowChecked({ role: 'radio' })).toBe(false);
	});

	test('a check is a menuitemcheckbox; so is a row that only says checked', () => {
		expect(rowRole({ role: 'check', checked: false })).toBe('menuitemcheckbox');
		expect(rowRole({ checked: true })).toBe('menuitemcheckbox');
		expect(rowRole({ checked: false })).toBe('menuitemcheckbox');
		expect(rowChecked({ checked: false })).toBe(false);
	});

	test('the tick stands where the icon goes, only while it is on', () => {
		expect(rowIcon({ role: 'radio', checked: true })).toBe('check');
		expect(rowIcon({ role: 'radio', checked: false })).toBeUndefined();
		expect(rowIcon({ checked: true, icon: 'x' })).toBe('check');
		expect(rowIcon({ checked: false, icon: 'x' })).toBe('x');
	});
});

describe('the context menus say what their rows are', () => {
	const read = (f: string) => readFileSync(join(import.meta.dir, f), 'utf8');
	const EDITOR = 'components/editor/';
	const menu = read(`${EDITOR}ContextMenu.svelte`);
	const timeline = read(`${EDITOR}Timeline.svelte`);
	const library = read(`${EDITOR}LibraryPanel.svelte`);

	test('a row’s role and checked state come from the item', () => {
		expect(menu).toContain('role={rowRole(item)}');
		expect(menu).toContain('aria-checked={rowChecked(item)}');
		expect(menu).not.toContain('role="menuitem"');
	});

	test('the title rows and the fact rows are presentation, not unlabelled children of the menu', () => {
		const header = menu.slice(menu.indexOf("item.type === 'header'"), menu.indexOf("item.type === 'info'"));
		const info = menu.slice(menu.indexOf("item.type === 'info'"), menu.indexOf('<button'));
		expect(header).toContain('<div role="presentation"');
		expect(info).toContain('<div role="presentation"');
	});

	test('no menu ticks a row with an icon alone: a choice is `checked`, with a role', () => {
		for (const [name, src] of [['Timeline', timeline], ['LibraryPanel', library]] as const) {
			expect(src, name).not.toMatch(/icon:\s*[^,\n]*\?\s*'check'/);
		}
		expect(timeline.match(/role: 'radio' as const/g)).toHaveLength(4);
		expect(library).toContain("role: 'check', checked: row.active");
	});

	test('the height menu’s size is a fact beside the row, not a key in the shortcut’s place', () => {
		expect(timeline).toContain('meta: `${PRESET_PX[p]} px`');
		expect(timeline).not.toContain('shortcut: `${PRESET_PX');
	});

	test('a shortcut shown in a menu is read from the registry, never typed', () => {
		for (const [name, src] of [['Timeline', timeline], ['LibraryPanel', library]] as const) {
			expect(src, name).not.toMatch(/\bshortcut:\s*[`'"]/);
		}
	});
});
