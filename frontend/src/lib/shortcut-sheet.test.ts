import { describe, expect, test } from 'bun:test';
import { ACTIONS, FIXED_KEYS, emptyOverrides, resolveBindings, setActionChords, parseChord } from './keymap';
import { shortcutSheet, unboundLine } from './shortcut-sheet';

describe('the shortcut cheat sheet', () => {
	const sheet = shortcutSheet(resolveBindings(emptyOverrides(), 'other'), 'other');

	test('lists the keys in force, grouped as the settings list groups them', () => {
		const find = (id: string) => sheet.groups.flatMap((g) => g.rows).find((r) => r.id === id);
		expect(find('edit.split')?.keys).toEqual(['Ctrl+K']);
		expect(find('file.save')?.keys).toEqual(['Ctrl+S', 'Ctrl+Shift+S']);
		expect(find('app.shortcuts')?.keys).toEqual(['?']);
		expect(sheet.groups.map((g) => g.id)).toEqual(['project', 'edit', 'tools', 'playback', 'markers', 'view', 'help']);
	});

	test('every action with a key appears once, and the rest are counted, not listed', () => {
		const rows = sheet.groups.flatMap((g) => g.rows);
		expect(new Set(rows.map((r) => r.id)).size).toBe(rows.length);
		const bound = ACTIONS.filter((a) => a.defaults.length > 0).length;
		expect(rows.length).toBe(bound);
		expect(sheet.unbound).toBe(ACTIONS.length - bound);
	});

	test('a label is a name, not a promise of a dialog', () => {
		expect(sheet.groups.flatMap((g) => g.rows).find((r) => r.id === 'file.open')?.label).toBe('Open project');
	});

	test('follows what the user changed: a moved key shows where it went, an unbound one drops out', () => {
		const moved = setActionChords(emptyOverrides(), 'edit.split', [parseChord('Mod+Shift+K', 'other')!], 'other');
		const unbound = setActionChords(moved, 'edit.undo', [], 'other');
		const s = shortcutSheet(resolveBindings(unbound, 'other'), 'other');
		const rows = s.groups.flatMap((g) => g.rows);
		expect(rows.find((r) => r.id === 'edit.split')?.keys).toEqual(['Ctrl+Shift+K']);
		expect(rows.find((r) => r.id === 'edit.undo')).toBeUndefined();
	});

	test('prints the platform’s own symbols', () => {
		const mac = shortcutSheet(resolveBindings(emptyOverrides(), 'mac'), 'mac');
		expect(mac.groups.flatMap((g) => g.rows).find((r) => r.id === 'edit.split')?.keys).toEqual(['⌘K']);
	});

	test('carries the fixed keys, which cannot be rebound', () => {
		expect(sheet.fixed).toBe(FIXED_KEYS);
	});

	test('the line about the keyless actions', () => {
		expect(unboundLine(0)).toBeNull();
		expect(unboundLine(1)).toContain('1 more action has no shortcut');
		expect(unboundLine(5)).toContain('5 more actions have no shortcut');
	});
});
