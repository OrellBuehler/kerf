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

/** Close whatever is up and whatever waits behind it. */
function clearDialogs() {
	ui.dropQueuedTagDialogs('import');
	ui.dropQueuedTagDialogs('bulk');
	ui.closeTagDialog();
}

beforeEach(() => {
	shown.length = 0;
	clearDialogs();
	settings.askTagsOnImport = true;
	settings.autoAnalysis = { ...settings.autoAnalysis, enabled: false };
});

describe('finishImport', () => {
	test('opens the tag dialog for the files that landed', async () => {
		await finishImport(outcome([asset('a'), asset('b'), asset('c')]));
		expect(ui.tagDialog).toMatchObject({ assetIds: ['a', 'b', 'c'], mode: 'import' });
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
		expect(ui.tagDialog).toMatchObject({ assetIds: ['a'], mode: 'import' });
	});
});

describe('ui.openTagDialog', () => {
	test('opens for some assets and not for none', () => {
		ui.openTagDialog([], 'bulk');
		expect(ui.tagDialog).toBeNull();
		ui.openTagDialog(['a', 'b']);
		expect(ui.tagDialog).toMatchObject({ assetIds: ['a', 'b'], mode: 'bulk' });
		ui.closeTagDialog();
		expect(ui.tagDialog).toBeNull();
	});
});

describe('two prompts at once', () => {
	test('a second one waits for the first, then takes its place afresh', () => {
		ui.openTagDialog(['a'], 'import');
		const first = ui.tagDialog!;
		ui.openTagDialog(['b', 'c'], 'import');
		expect(ui.tagDialog).toBe(first);
		ui.closeTagDialog();
		expect(ui.tagDialog).toMatchObject({ assetIds: ['b', 'c'], mode: 'import' });
		// its own number: the page remounts the dialog rather than reusing the first one's state
		expect(ui.tagDialog!.id).not.toBe(first.id);
		ui.closeTagDialog();
		expect(ui.tagDialog).toBeNull();
	});

	test('overlapping imports each get their prompt, in order', async () => {
		await finishImport(outcome([asset('a')]));
		await finishImport(outcome([asset('b')]));
		await finishImport(outcome([asset('c')]));
		const seen: string[][] = [];
		while (ui.tagDialog) {
			seen.push(ui.tagDialog.assetIds);
			ui.closeTagDialog();
		}
		expect(seen).toEqual([['a'], ['b'], ['c']]);
	});

	test("ticking Don't ask again drops the import prompts that wait, and keeps a bulk dialog that does", () => {
		ui.openTagDialog(['a'], 'import');
		ui.openTagDialog(['b'], 'import');
		ui.openTagDialog(['c'], 'bulk');
		ui.dropQueuedTagDialogs('import');
		ui.closeTagDialog();
		expect(ui.tagDialog).toMatchObject({ assetIds: ['c'], mode: 'bulk' });
		ui.closeTagDialog();
		expect(ui.tagDialog).toBeNull();
	});

	test('closing with nothing waiting leaves none', () => {
		ui.openTagDialog(['a'], 'bulk');
		ui.closeTagDialog();
		expect(ui.tagDialog).toBeNull();
	});
});
