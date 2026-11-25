import { describe, expect, test } from 'bun:test';
import type { MenuItem } from './context-menu.svelte';
import { clipMenu, menuHeight, type ClipMenuActions, type ClipMenuInput } from './clip-menu';
import { contextRows, tidyMenu } from './menu-items';
import { PREVIEW_READ_ONLY_REASON } from './proposal-ui';

const plan = (label: string, over: { show?: boolean; reason?: string | null } = {}) => ({ ids: ['c1'], label, show: over.show ?? true, reason: over.reason ?? null });
const off = { show: false, reason: null };

/** A linked pair of footage with sound, the playhead inside the clip, nothing locked — the long case. */
const input = (over: Partial<ClipMenuInput> = {}): ClipMenuInput => ({
	count: 1,
	kind: 'video',
	trackName: 'V1',
	locked: false,
	playheadIn: true,
	cutAll: { ok: true },
	enabled: true,
	dupWhy: null,
	linked: true,
	detach: plan('Detach audio', { reason: 'Its sound is already detached' }),
	reattach: plan('Reattach audio', { show: false }),
	link: plan('Link clips', { reason: 'Those clips are already linked' }),
	unlink: plan('Unlink clips'),
	sync: off,
	fit: off,
	stems: { show: true, reason: null },
	compose: off,
	keyframes: { copy: { disabled: true, reason: 'This clip has no keyframes to copy' }, paste: { disabled: true, reason: 'Copy the keyframes of a clip first' } },
	fadeIn: false,
	fadeOut: true,
	previewing: false,
	key: (id) => `K:${id}`,
	analysis: [
		{ type: 'info', label: 'Analyzed', value: '5/5' },
		{ label: 'Analyze…', icon: 'scan-line', action: () => {} }
	],
	...over
});

const calls: string[] = [];
const actions = new Proxy({} as ClipMenuActions, {
	get:
		(_t, name: string) =>
		(...args: unknown[]) =>
			void calls.push(args.length > 0 ? `${name}(${args.join(',')})` : name)
});

/** Every runnable row, however deep, with the path of submenus it is under. */
function rows(items: readonly MenuItem[], path: string[] = []): { row: Extract<MenuItem, { action: () => void }>; path: string[] }[] {
	return items.flatMap((i) => {
		if (i.type === 'submenu') return rows(i.items, [...path, i.label]);
		if (i.type === undefined || i.type === 'item') return [{ row: i, path }];
		return [];
	});
}
const labels = (items: readonly MenuItem[]) => items.flatMap((i) => (i.type === 'separator' || i.type === 'header' || i.type === 'info' ? [] : [i.label]));
const find = (items: readonly MenuItem[], label: string) => rows(items).find((r) => r.row.label === label);

describe('the clip menu is short', () => {
	test('the longest it gets stays well under a 768 px window, with no row doubled by a reason', () => {
		const menu = tidyMenu(clipMenu(input({ fit: { show: true, reason: null }, compose: { show: true, reason: null }, sync: { show: true, reason: 'Select exactly two clips' } }), actions));
		expect(menuHeight(menu)).toBeLessThan(640);
		expect(menuHeight(menu)).toBeLessThan(768 - 12);
	});

	test('the top level is the daily verbs and a few groups', () => {
		const menu = tidyMenu(clipMenu(input(), actions));
		expect(labels(menu)).toEqual([
			'Split at playhead',
			'Add edit to all tracks',
			'Trim to playhead',
			'Copy',
			'Duplicate',
			'Disable clip',
			'Audio',
			'Link',
			'Keyframes',
			'Fades',
			'Analyze…',
			'Remove',
			'Ripple delete',
			'Only this clip'
		]);
	});

	test('a plain unlinked clip with nothing to detach is shorter still, and has no empty group', () => {
		const menu = tidyMenu(
			clipMenu(input({ linked: false, stems: off, detach: plan('Detach audio', { show: false }), analysis: [] }), actions)
		);
		const names = labels(menu);
		expect(names).not.toContain('Audio');
		expect(names).not.toContain('Only this clip');
		expect(names).toContain('Remove');
	});

	test('a group of one row is that row, not a menu around it', () => {
		const menu = tidyMenu(clipMenu(input({ detach: plan('Detach audio', { show: false }), stems: { show: true, reason: null } }), actions));
		expect(labels(menu)).toContain('Separate stems under this clip…');
		expect(labels(menu)).not.toContain('Audio');
	});

	test('a menu of several clips says how many where a row means all of them', () => {
		const menu = tidyMenu(clipMenu(input({ count: 3 }), actions));
		expect(labels(menu)).toContain('Split selected at playhead');
		expect(labels(menu)).toContain('Copy 3 clips');
		expect(labels(menu)).toContain('Duplicate 3 clips');
		expect(labels(menu)).toContain('Remove 3 clips');
		expect(labels(menu)).toContain('Ripple delete 3 clips');
		expect(labels(menu)).not.toContain('Remove');
	});

	test('music has Fit to video where it can be seen, and so has Edit & re-compose', () => {
		const menu = tidyMenu(clipMenu(input({ kind: 'audio', fit: { show: true, reason: null }, compose: { show: true, reason: 'Unlink the music from its picture first' } }), actions));
		const top = labels(menu);
		expect(top).toContain('Fit to video…');
		expect(top).toContain('Edit & re-compose…');
	});
});

describe('every row is one line and says why it is off', () => {
	test('a row that cannot run has a reason, in every group', () => {
		for (const over of [{}, { locked: true, playheadIn: false }, { count: 2 }, { previewing: true }]) {
			const menu = tidyMenu(clipMenu(input(over), actions));
			for (const { row, path } of rows(menu)) {
				if (row.disabled) expect([path.join('/'), row.label, !!row.reason]).toEqual([path.join('/'), row.label, true]);
			}
		}
	});

	test('the reasons are the engine’s and the offers’ own words', () => {
		const menu = tidyMenu(clipMenu(input(), actions));
		expect(find(menu, 'Detach audio')?.row.reason).toBe('Its sound is already detached');
		expect(find(menu, 'Link clips')?.row.reason).toBe('Those clips are already linked');
		expect(find(menu, 'Copy keyframes')?.row.reason).toBe('This clip has no keyframes to copy');
	});

	test('a locked track and a playhead outside the clip say so on the cuts', () => {
		const locked = tidyMenu(clipMenu(input({ locked: true }), actions));
		expect(find(locked, 'Split at playhead')?.row.reason).toBe('Track V1 is locked');
		expect(find(locked, 'Disable clip')?.row.reason).toBe('Track V1 is locked');
		const away = tidyMenu(clipMenu(input({ playheadIn: false }), actions));
		expect(find(away, 'Split at playhead')?.row.reason).toBe('Move the playhead into this clip first');
		expect(find(away, 'Trim start to playhead')?.row.reason).toBe('Move the playhead into this clip first');
	});

	test('a cut through every track with nothing to cut gives the reason it was handed', () => {
		const menu = tidyMenu(clipMenu(input({ cutAll: { ok: false, reason: 'Move the playhead onto a clip to cut every track there' } }), actions));
		const row = find(menu, 'Add edit to all tracks')!.row;
		expect(row.disabled).toBe(true);
		expect(row.reason).toBe('Move the playhead onto a clip to cut every track there');
	});

	test('no row carries the second line the menu used to draw under its label', () => {
		const menu = tidyMenu(clipMenu(input(), actions));
		for (const { row } of rows(menu)) expect(row.label).not.toContain('\n');
	});
});

describe('shortcuts come from the registry', () => {
	test('each row that has a key asks for its action’s, and a row without one asks for none', () => {
		const menu = tidyMenu(clipMenu(input({ stems: off }), actions));
		const key = (label: string) => find(menu, label)?.row.shortcut;
		expect(key('Split at playhead')).toBe('K:edit.split');
		expect(key('Add edit to all tracks')).toBe('K:edit.splitAll');
		expect(key('Trim start to playhead')).toBe('K:edit.trimStart');
		expect(key('Trim end to playhead')).toBe('K:edit.trimEnd');
		expect(key('Ripple trim start to playhead')).toBe('K:edit.rippleTrimStart');
		expect(key('Ripple trim end to playhead')).toBe('K:edit.rippleTrimEnd');
		expect(key('Copy')).toBe('K:edit.copy');
		expect(key('Duplicate')).toBe('K:edit.duplicate');
		expect(key('Detach audio')).toBe('K:edit.detachAudio');
		expect(key('Link clips')).toBe('K:edit.link');
		expect(key('Unlink clips')).toBe('K:edit.unlink');
		expect(key('Remove')).toBe('K:edit.delete');
		expect(key('Ripple delete')).toBe('K:edit.rippleDelete');
		expect(key('Disable clip')).toBeUndefined();
		expect(key('Copy keyframes')).toBeUndefined();
	});

	test('no label spells a key', () => {
		const menu = tidyMenu(clipMenu(input({ fit: { show: true, reason: null } }), actions));
		for (const { row } of rows(menu)) expect(row.label).not.toMatch(/\((?:⌘|Ctrl|Shift|Alt)|Ctrl\+|⌘[A-Z]/);
	});
});

describe('the proposal on screen holds every row that edits', () => {
	test('each runnable row is off with the reason, in a submenu too — except looking at motion presets’ library', () => {
		const menu = tidyMenu(clipMenu(input({ previewing: true }), actions));
		for (const { row, path } of rows(menu)) {
			if (row.label === 'Copy' || row.label === 'Analyze…') continue;
			expect([path.join('/'), row.label, row.disabled, row.reason]).toEqual([path.join('/'), row.label, true, PREVIEW_READ_ONLY_REASON]);
		}
	});

	test('Copy is a look, not an edit, and stays on', () => {
		const menu = tidyMenu(clipMenu(input({ previewing: true }), actions));
		expect(find(menu, 'Copy')?.row.disabled).toBeFalsy();
	});
});

describe('the rows run what they say', () => {
	test('each callback is the row’s own', () => {
		calls.length = 0;
		const menu = tidyMenu(clipMenu(input({ detach: plan('Detach audio'), link: plan('Link clips') }), actions));
		for (const label of ['Split at playhead', 'Add edit to all tracks', 'Trim start to playhead', 'Ripple trim end to playhead', 'Copy', 'Duplicate', 'Disable clip', 'Detach audio', 'Link clips', 'Add fade-in', 'Remove fade-out']) {
			find(menu, label)!.row.action();
		}
		expect(calls).toEqual(['split', 'cutAll', 'trimStart', 'rippleTrimEnd', 'copy', 'duplicate', 'toggleEnabled', 'detach', 'link', 'toggleFadeIn', 'toggleFadeOut']);
	});

	test('Remove and Ripple delete pass whether the gap closes; the "only this clip" rows say so', () => {
		calls.length = 0;
		const menu = tidyMenu(clipMenu(input(), actions));
		find(menu, 'Remove')!.row.action();
		find(menu, 'Ripple delete')!.row.action();
		find(menu, 'Remove only this clip')!.row.action();
		find(menu, 'Ripple delete only this clip')!.row.action();
		expect(calls).toEqual(['remove(false)', 'remove(true)', 'removeAlone(false)', 'removeAlone(true)']);
	});
});

describe('the keyboard reaches every row', () => {
	test('the top level and each submenu are rows a menu can focus, none of them a dead end', () => {
		const menu = tidyMenu(clipMenu(input(), actions));
		const top = contextRows(menu).filter((r) => r.focusable);
		expect(top.length).toBeGreaterThanOrEqual(12);
		for (const item of menu) {
			if (item.type !== 'submenu') continue;
			expect(item.items.length).toBeGreaterThanOrEqual(2);
			expect(contextRows(item.items).some((r) => r.focusable && r.enabled !== false) || item.items.every((i) => i.type !== 'item' && i.type !== undefined || !!i.disabled)).toBe(true);
		}
	});
});
