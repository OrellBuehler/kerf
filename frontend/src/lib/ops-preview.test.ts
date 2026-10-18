import { beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';

// The edit actions as the keymap, the menus and the toasts' buttons run them (`ops.ts`) while the agent's proposal is
// on the timeline. The cut on screen is then the proposal's, not the project's: an action must say so and stop
// *before* it plans against that cut, copies it to the clipboard, remembers one of its clips or analyzes anything —
// and say it as a notice, not as an error. The toasts stand in for what only a window shows.

const shown: { kind: string; text: string }[] = [];
const note = (kind: string) => (text: string) => void shown.push({ kind, text });
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), {
		success: note('success'),
		warning: note('warning'),
		error: note('error'),
		info: note('info'),
		dismiss: () => {}
	})
}));

const { editor, EditRefused, PreviewReadOnly, PREVIEW_READ_ONLY } = await import('./state.svelte');
const ops = await import('./ops');
const { getHistory, revertTo } = await import('./api');

const head = async () => (await getHistory()).find((r) => r.current)!.seq;
const only = (kind: string, text: string) => expect(shown.map((s) => [s.kind, s.text])).toEqual([[kind, text]]);

beforeEach(async () => {
	await revertTo(0);
	await editor.load();
	editor.clearSelection();
	editor.clipboard = [];
	editor.keyframeSource = null;
	editor.previewingStaged = false;
	shown.length = 0;
});

describe('while the proposal is previewed, an edit action says so and does nothing', () => {
	const actions: [string, () => Promise<unknown> | unknown][] = [
		['delete', () => ops.deleteSelection(false)],
		['ripple delete', () => ops.deleteSelection(true)],
		['cut', () => ops.cutSelection()],
		['trim start', () => ops.trimSelection('left')],
		['trim end', () => ops.trimSelection('right')],
		['split', () => ops.splitSelection()],
		['roll', () => ops.keyEdit('roll', 1)],
		['slip', () => ops.keyEdit('slip', -1)],
		['slide', () => ops.keyEdit('slide', 1)],
		['detach audio', () => ops.detachSelection()],
		['reattach audio', () => ops.reattachSelection()],
		['link', () => ops.linkSelection()],
		['unlink', () => ops.unlinkSelection()],
		['append all', () => ops.appendAllMedia()],
		['paste', () => ops.pasteClips()],
		['duplicate', () => ops.duplicateClips()],
		['add marker', () => ops.addMarker()],
		['copy keyframes', () => ops.copyKeyframesOf('c1')],
		['paste keyframes', () => ops.pasteKeyframesOnto('c1')]
	];

	for (const [name, run] of actions) {
		test(`${name}: one notice, no revision, nothing copied or remembered`, async () => {
			editor.selectClips(['c1', 'c3'], 'c1');
			const before = await head();
			editor.previewingStaged = true;
			await run();
			only('info', PREVIEW_READ_ONLY);
			expect(await head()).toBe(before);
			expect(editor.clipboard).toEqual([]);
			expect(editor.keyframeSource).toBeNull();
		});
	}

	test('the same actions run once the preview is left', async () => {
		editor.selectClip('c2');
		editor.previewingStaged = true;
		await ops.cutSelection();
		editor.previewingStaged = false;
		shown.length = 0;
		await ops.cutSelection();
		expect(shown.some((s) => s.kind === 'info' && s.text === PREVIEW_READ_ONLY)).toBe(false);
		expect(editor.clipboard.length).toBeGreaterThan(0);
	});

	test('holdForPreview is the answer to "should this stop here"', () => {
		expect(ops.holdForPreview()).toBe(false);
		expect(shown).toEqual([]);
		editor.previewingStaged = true;
		expect(ops.holdForPreview()).toBe(true);
		only('info', PREVIEW_READ_ONLY);
	});
});

describe('a refusal is said as what it is', () => {
	test('the preview’s is a notice, anyone else’s an error', () => {
		ops.report(new PreviewReadOnly());
		ops.report(new EditRefused('the clip is gone'));
		ops.report(new Error('boom'));
		ops.report('plain');
		expect(shown.map((s) => [s.kind, s.text])).toEqual([
			['info', PREVIEW_READ_ONLY],
			['error', 'the clip is gone'],
			['error', 'boom'],
			['error', 'plain']
		]);
	});

	test('attempt routes through it', async () => {
		editor.previewingStaged = true;
		await ops.attempt(() => editor.split('c1', 3));
		only('info', PREVIEW_READ_ONLY);
	});
});
