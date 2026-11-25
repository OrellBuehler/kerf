import { afterEach, describe, expect, test } from 'bun:test';
import './test-runes';

const { contextMenu } = await import('./context-menu.svelte');

afterEach(() => {
	contextMenu.visible = false;
	contextMenu.win = null;
	contextMenu.items = [];
});

describe('a context menu holds the keyboard only while it is really open', () => {
	test('closed, it holds nothing', () => {
		expect(contextMenu.holdsKeys()).toBe(false);
	});

	test('open in a window that is still there, it holds every key', () => {
		contextMenu.visible = true;
		contextMenu.win = { closed: false } as Window;
		expect(contextMenu.holdsKeys()).toBe(true);
		expect(contextMenu.visible).toBe(true);
	});

	test('open in the editor window (no window named), it holds them too', () => {
		contextMenu.visible = true;
		contextMenu.win = null;
		expect(contextMenu.holdsKeys()).toBe(true);
	});

	test('a menu whose window closed without a blur is closed, not kept for good: the keyboard is the page’s again', () => {
		contextMenu.visible = true;
		contextMenu.win = { closed: true } as Window;
		expect(contextMenu.holdsKeys()).toBe(false);
		expect(contextMenu.visible).toBe(false);
		expect(contextMenu.win).toBeNull();
		// …and it stays closed.
		expect(contextMenu.holdsKeys()).toBe(false);
	});
});
