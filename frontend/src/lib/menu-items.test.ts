import { describe, expect, test } from 'bun:test';
import type { MenuItem } from './context-menu.svelte';
import { heldForPreview, muteItem, removeRows, tidyMenu, type RemoveRows } from './menu-items';

const item = (label: string): MenuItem => ({ label, action: () => {} });
const sep: MenuItem = { type: 'separator' };
const shape = (items: readonly MenuItem[]) => items.map((i) => (i.type === 'separator' ? '-' : 'label' in i ? i.label : '?'));

describe('tidyMenu', () => {
	test('collapses two separators in a row into one', () => {
		expect(shape(tidyMenu([item('Add video'), item('Add audio'), sep, sep, item('Paste')]))).toEqual([
			'Add video',
			'Add audio',
			'-',
			'Paste'
		]);
	});

	test('drops a rule at either end', () => {
		expect(shape(tidyMenu([sep, item('A'), sep, item('B'), sep]))).toEqual(['A', '-', 'B']);
	});

	test('a group that is not there takes its rule with it', () => {
		// The clip menu: Analyze rows, then optional Fit / Stems offers, each between rules.
		const none: MenuItem[] = [];
		expect(shape(tidyMenu([item('Unlink'), sep, ...none, sep, ...none, sep, item('Disable')]))).toEqual([
			'Unlink',
			'-',
			'Disable'
		]);
	});

	test('keeps headers and info rows in place and leaves a clean menu as it was', () => {
		const clean: MenuItem[] = [{ type: 'header', label: 'Analyze' }, item('Silence'), sep, { type: 'info', label: 'Analyzed', value: '5' }];
		expect(tidyMenu(clean)).toEqual(clean);
	});

	test('an all-separator list is empty', () => {
		expect(tidyMenu([sep, sep])).toEqual([]);
	});
});

describe('muteItem', () => {
	test('a picture track is hidden and shown, with the eye', () => {
		expect(muteItem({ kind: 'video', muted: false })).toEqual({ label: 'Hide track', icon: 'eye-off' });
		expect(muteItem({ kind: 'video', muted: true })).toEqual({ label: 'Show track', icon: 'eye' });
	});

	test('a sound track is muted and unmuted, with a speaker — not the eye', () => {
		expect(muteItem({ kind: 'audio', muted: false })).toEqual({ label: 'Mute track', icon: 'volume-x' });
		expect(muteItem({ kind: 'audio', muted: true })).toEqual({ label: 'Unmute track', icon: 'volume-2' });
	});
});

describe('removeRows', () => {
	const base: RemoveRows = {
		count: 1,
		linked: false,
		locked: false,
		trackName: 'V1',
		removeKey: 'Del',
		rippleKey: 'Shift+Del',
		remove: () => {},
		removeAlone: () => {}
	};
	type Row = Extract<MenuItem, { action: () => void }>;
	const rows = (o: Partial<RemoveRows> = {}): Row[] => removeRows({ ...base, ...o }).filter((i): i is Row => 'action' in i);

	test('an unlinked clip has Remove and Ripple delete, with icons of their own', () => {
		const r = rows();
		expect(r.map((i) => i.label)).toEqual(['Remove', 'Ripple delete']);
		expect(r.map((i) => i.icon)).toEqual(['trash', 'between-horizontal-start']);
		expect(r.map((i) => i.shortcut)).toEqual(['Del', 'Shift+Del']);
	});

	test('no two rows share an icon, and a row without one is the linked-only variant', () => {
		const r = rows({ linked: true });
		const icons = r.map((i) => i.icon).filter(Boolean);
		expect(new Set(icons).size).toBe(icons.length);
		expect(r.filter((i) => !i.icon).map((i) => i.label)).toEqual(['Remove only this clip', 'Ripple delete only this clip']);
	});

	test('the menu is no taller for the linked variants than it needs to be: no rule of their own', () => {
		expect(removeRows({ ...base, linked: true }).some((i) => i.type === 'separator')).toBe(false);
		expect(removeRows(base).some((i) => i.type === 'separator')).toBe(false);
	});

	test('a selection says how many clips', () => {
		expect(rows({ count: 3 }).map((i) => i.label)).toEqual(['Remove 3 clips', 'Ripple delete 3 clips']);
	});

	test('a locked track disables the linked variants and says why', () => {
		const alone = rows({ linked: true, locked: true, trackName: 'A1' }).filter((i) => !i.icon);
		expect(alone.every((i) => i.disabled)).toBe(true);
		expect(alone.map((i) => i.reason)).toEqual(['Track A1 is locked', 'Track A1 is locked']);
	});

	test('the rows run what they say, with the ripple flag', () => {
		const calls: string[] = [];
		const r = rows({
			linked: true,
			remove: (ripple) => calls.push(`remove:${ripple}`),
			removeAlone: (ripple) => calls.push(`alone:${ripple}`)
		});
		for (const i of r) i.action();
		expect(calls).toEqual(['remove:false', 'remove:true', 'alone:false', 'alone:true']);
	});
});

describe('heldForPreview', () => {
	const rows: MenuItem[] = [
		{ label: 'Duplicate', action: () => {} },
		{ type: 'separator' },
		{ type: 'header', label: 'Clip' },
		{ label: 'Remove', danger: true, disabled: false, reason: 'Select a clip first', action: () => {} },
		{ type: 'info', label: 'Analyzed', value: '2' }
	];

	test('is the same rows when there is no preview', () => {
		expect(heldForPreview(rows, false)).toEqual(rows);
	});

	test('turns every action off with the one reason, and leaves separators, headers and facts as they are', () => {
		const held = heldForPreview(rows, true);
		expect(held).toHaveLength(rows.length);
		expect(held[0]).toMatchObject({ label: 'Duplicate', disabled: true, reason: 'Exit the proposal preview to edit' });
		expect(held[1]).toEqual(rows[1]);
		expect(held[2]).toEqual(rows[2]);
		expect(held[3]).toMatchObject({ label: 'Remove', danger: true, disabled: true, reason: 'Exit the proposal preview to edit' });
		expect(held[4]).toEqual(rows[4]);
	});

	test('does not touch the rows it was given', () => {
		heldForPreview(rows, true);
		expect((rows[0] as { disabled?: boolean }).disabled).toBeUndefined();
	});
});
