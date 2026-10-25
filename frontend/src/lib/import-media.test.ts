import { beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import type { Asset } from './types';

// After an import: the tag prompt opens for the files that landed (once each), unless Settings turned it off; a failed
// or stopped import has nothing to tag. The analysis that follows is stood out of the way (it is off in these runs).

const shown: string[] = [];
const note = (kind: string) => (text: string) => void shown.push(`${kind}: ${text}`);
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), { success: note('success'), warning: note('warning'), error: note('error'), info: note('info'), dismiss: () => {} })
}));

const { ui } = await import('./editor-ui.svelte');
const { settings } = await import('./settings.svelte');
const { finishImport } = await import('./import-media');

const asset = (id: string, name = `${id}.mp4`) => ({ id, name }) as Asset;
const outcome = (imported: Asset[], extra: { failed?: { name: string; message: string }[]; cancelled?: number } = {}) =>
	Promise.resolve({ imported, failed: extra.failed ?? [], cancelled: extra.cancelled ?? 0 });

beforeEach(() => {
	shown.length = 0;
	ui.closeTagDialog();
	settings.askTagsOnImport = true;
	settings.autoAnalysis = { ...settings.autoAnalysis, enabled: false };
});

describe('finishImport', () => {
	test('opens the tag dialog for the files that landed', async () => {
		await finishImport(outcome([asset('a'), asset('b'), asset('c')]));
		expect(ui.tagDialog).toEqual({ assetIds: ['a', 'b', 'c'], mode: 'import' });
		expect(shown).toContain('success: Imported 3 files');
	});

	test('names an asset once when a 360 pair resolves to it twice', async () => {
		await finishImport(outcome([asset('a'), asset('a')]));
		expect(ui.tagDialog?.assetIds).toEqual(['a']);
	});

	test('does not ask when Settings turned the prompt off', async () => {
		settings.askTagsOnImport = false;
		await finishImport(outcome([asset('a')]));
		expect(ui.tagDialog).toBeNull();
	});

	test('has nothing to tag when nothing landed, and says what failed', async () => {
		await finishImport(outcome([], { failed: [{ name: 'x.mov', message: 'unreadable' }], cancelled: 1 }));
		expect(ui.tagDialog).toBeNull();
		expect(shown).toContain("error: Couldn't import x.mov: unreadable");
	});

	test('tags what landed even when some files failed', async () => {
		await finishImport(outcome([asset('a')], { failed: [{ name: 'x.mov', message: 'unreadable' }] }));
		expect(ui.tagDialog).toEqual({ assetIds: ['a'], mode: 'import' });
	});
});

describe('ui.openTagDialog', () => {
	test('opens for some assets and not for none', () => {
		ui.openTagDialog([], 'bulk');
		expect(ui.tagDialog).toBeNull();
		ui.openTagDialog(['a', 'b']);
		expect(ui.tagDialog).toEqual({ assetIds: ['a', 'b'], mode: 'bulk' });
		ui.closeTagDialog();
		expect(ui.tagDialog).toBeNull();
	});
});
