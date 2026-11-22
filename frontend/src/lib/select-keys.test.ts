import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { chooseFromList, stepsSelect } from './select-keys';
import { SRC } from './test-sources';

const key = (k: string, mods: Partial<Record<'altKey' | 'ctrlKey' | 'metaKey', boolean>> = {}) => ({ key: k, altKey: false, ctrlKey: false, metaKey: false, ...mods });

describe('which keys step a closed select', () => {
	test('the arrows, Home, End and the Page keys do', () => {
		for (const k of ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End', 'PageUp', 'PageDown']) expect(stepsSelect(key(k)), k).toBe(true);
	});

	test('Space, Enter, Tab, Escape and letters do not (Space opens the list, Enter chooses in it)', () => {
		for (const k of [' ', 'Enter', 'Tab', 'Escape', 'a']) expect(stepsSelect(key(k)), k).toBe(false);
	});

	test('Alt+Down opens the list, and a chord is the page’s', () => {
		expect(stepsSelect(key('ArrowDown', { altKey: true }))).toBe(false);
		expect(stepsSelect(key('ArrowDown', { ctrlKey: true }))).toBe(false);
		expect(stepsSelect(key('ArrowDown', { metaKey: true }))).toBe(false);
	});
});

describe('the action', () => {
	test('holds a stepping key and leaves any other alone, and lets go on destroy', () => {
		const handlers: ((e: KeyboardEvent) => void)[] = [];
		const node = {
			addEventListener: (_: string, h: (e: KeyboardEvent) => void) => handlers.push(h),
			removeEventListener: (_: string, h: (e: KeyboardEvent) => void) => handlers.splice(handlers.indexOf(h), 1)
		} as unknown as HTMLSelectElement;
		const action = chooseFromList(node);
		expect(handlers.length).toBe(1);
		let prevented = 0;
		const press = (k: string) => handlers[0]({ ...key(k), preventDefault: () => prevented++ } as unknown as KeyboardEvent);
		press('ArrowDown');
		press('Enter');
		press(' ');
		expect(prevented).toBe(1);
		action.destroy();
		expect(handlers.length).toBe(0);
	});
});

describe('the project frame does not write per key', () => {
	const read = (f: string) => readFileSync(join(SRC, 'lib/components/editor', f), 'utf8');

	test('the shape chips only move focus on an arrow (Space / Enter choose)', () => {
		expect(read('ProjectFrame.svelte')).toContain('follow={false}');
	});

	test('every select that writes the frame holds the stepping keys', () => {
		const source = read('ProjectFrame.svelte');
		const selects = source.match(/<select\b/g) ?? [];
		expect(selects.length).toBe(2);
		expect(source.match(/use:chooseFromList/g)?.length).toBe(2);
	});
});
