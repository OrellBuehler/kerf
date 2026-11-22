import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';

// A key that edits the cut and was not the page's (`focus-area.ts`) says so — once, quietly, under one id.

interface Shown {
	kind: string;
	text: string;
	opts?: { id?: string; duration?: number };
}
const shown: Shown[] = [];
const note = (kind: string) => (text: string, opts?: Shown['opts']) => void shown.push({ kind, text, opts });
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), { success: note('success'), warning: note('warning'), error: note('error'), info: note('info'), dismiss: () => {} })
}));

const { settings } = await import('./settings.svelte');
const ops = await import('./ops');

beforeEach(() => {
	shown.length = 0;
});
afterAll(() => {
	shown.length = 0;
});

describe('the notice for a refused editing key', () => {
	test('names the action from the registry and what to do', () => {
		ops.cutKeyRefused('edit.delete');
		expect(shown).toHaveLength(1);
		expect(shown[0].kind).toBe('info');
		expect(shown[0].text).toStartWith('Delete selection edits the timeline — click it first');
	});

	test('quiet and deduplicated: one id, a couple of seconds, whichever action', () => {
		ops.cutKeyRefused('edit.delete');
		ops.cutKeyRefused('edit.split');
		expect(shown.map((s) => s.opts?.id)).toEqual(['cut-denied', 'cut-denied']);
		for (const s of shown) expect(s.opts?.duration).toBeLessThanOrEqual(2000);
	});

	test('names the key that gets to the timeline, as the user has it', () => {
		const key = settings.shortcut('view.focusTimeline');
		expect(key).toBe('F6');
		ops.cutKeyRefused('edit.paste');
		expect(shown[0].text).toContain(`press ${key}`);
	});
});
