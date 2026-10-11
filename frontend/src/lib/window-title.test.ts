import { describe, expect, test } from 'bun:test';
import { windowTitle } from './window-title';

describe('the window title', () => {
	test('is the project and the app', () => {
		expect(windowTitle('Untitled project', false)).toBe('Untitled project — Kerf');
		expect(windowTitle('interview.kerf', false)).toBe('interview.kerf — Kerf');
	});

	test('leads with a dot while there is unsaved work, so a cut-off taskbar title still shows it', () => {
		expect(windowTitle('interview.kerf', true)).toBe('• interview.kerf — Kerf');
	});

	test('a blank name is just the app', () => {
		expect(windowTitle('  ', false)).toBe('Kerf');
		expect(windowTitle('', true)).toBe('• Kerf');
	});
});
