import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { leavesKeyToControl } from './page-keys';

const button = { operable: true, slider: false };
const slider = { operable: true, slider: true };
const nothing = { operable: false, slider: false };

describe('which keys the page leaves to the focused control', () => {
	test('Space, Enter and the arrows press a button; the page leaves them', () => {
		for (const k of [' ', 'Enter', 'ArrowLeft', 'ArrowDown']) expect(leavesKeyToControl(k, button), k).toBe(true);
		for (const k of ['j', 'k', 'l', 'Delete', 'Backspace', 'Home', 'End', 'PageUp', 'Escape', 'c']) expect(leavesKeyToControl(k, button), k).toBe(false);
	});

	test('Space plays with a slider focused (a fader, a colour wheel, a grade’s lightness): it is not the slider’s key', () => {
		expect(leavesKeyToControl(' ', slider)).toBe(false);
		expect(leavesKeyToControl('Enter', slider)).toBe(true);
		for (const k of ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown']) expect(leavesKeyToControl(k, slider), k).toBe(true);
	});

	test('Delete and Backspace with a slider focused never run the page’s delete', () => {
		for (const k of ['Delete', 'Backspace']) expect(leavesKeyToControl(k, slider), k).toBe(true);
		// …and are still the page's everywhere else
		for (const k of ['Delete', 'Backspace']) {
			expect(leavesKeyToControl(k, button), k).toBe(false);
			expect(leavesKeyToControl(k, nothing), k).toBe(false);
		}
	});

	test('the shuttle keys and the other shortcuts are the page’s with a slider focused, as they were', () => {
		for (const k of ['j', 'k', 'l', 's', 'c', 'Home', 'End', 'PageUp', 'PageDown']) expect(leavesKeyToControl(k, slider), k).toBe(false);
	});

	test('with nothing operable focused nothing is left to a control', () => {
		for (const k of [' ', 'Enter', 'ArrowLeft', 'Delete']) expect(leavesKeyToControl(k, nothing), k).toBe(false);
	});

	test('the page asks this, with the two things it can see of its target', () => {
		const page = readFileSync(join(import.meta.dir, '..', 'routes', '+page.svelte'), 'utf8');
		expect(page.includes("const onSlider = !!target?.closest('[role=\"slider\"]');")).toBe(true);
		expect(page.includes("leavesKeyToControl(e.key, { operable, slider: onSlider })")).toBe(true);
		expect(page.includes("const operates = k === ' '")).toBe(false);
	});
});
