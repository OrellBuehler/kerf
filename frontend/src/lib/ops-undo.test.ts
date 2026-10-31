import { beforeEach, describe, expect, mock, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import './test-runes';

// A toast's Undo outlives the edit it is about (seconds, and a click on an old notice): it must take back
// *that* edit, and only while it is still the latest. Over the browser harness's cut, with the toasts standing
// in for what only a window shows.

const shown: { kind: string; text: string; undo?: () => void }[] = [];
const note =
	(kind: string) =>
	(text: string, opts?: { action?: { onClick: () => void } }) =>
		void shown.push({ kind, text, undo: opts?.action?.onClick });
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), {
		success: note('success'),
		warning: note('warning'),
		error: note('error'),
		info: note('info'),
		dismiss: () => {}
	})
}));

const { editor } = await import('./state.svelte');
const { UNDO_MOVED_ON, addMarker, undoAction, undoToast } = await import('./ops');
const { revertTo } = await import('./api');

const settle = () => new Promise((r) => setTimeout(r, 20));
const markers = () => editor.markers.map((m) => m.name);

beforeEach(async () => {
	await revertTo(0);
	await editor.load();
	editor.clearSelection();
	shown.length = 0;
});

describe('a toast’s Undo', () => {
	test('takes back its own edit while it is still the latest', async () => {
		await addMarker();
		expect(editor.markers).toHaveLength(1);
		expect(shown).toHaveLength(1);
		shown[0].undo!();
		await settle();
		expect(editor.markers).toHaveLength(0);
		expect(shown).toHaveLength(1);
	});

	test('after another edit it takes nothing back, leaves that edit applied and says where to go', async () => {
		await addMarker();
		const first = shown[0];
		await editor.addMarker(9, 'Later');
		first.undo!();
		await settle();
		expect(markers()).toContain('Later');
		expect(editor.markers).toHaveLength(2);
		expect(shown.at(-1)).toMatchObject({ kind: 'warning', text: UNDO_MOVED_ON });
		expect(UNDO_MOVED_ON).toContain('Edit > Undo');
		// the newest edit's own toast still works
		await addMarker();
		shown.at(-1)!.undo!();
		await settle();
		expect(editor.markers).toHaveLength(2);
	});

	test('is good again once the edits after it have been undone', async () => {
		await addMarker();
		const first = shown[0];
		await editor.addMarker(9, 'Later');
		await editor.undo();
		first.undo!();
		await settle();
		expect(editor.markers).toHaveLength(0);
	});

	test('a toast about several revisions takes them all back, and only while it is the latest', async () => {
		await editor.addMarker(1, 'One');
		await editor.addMarker(2, 'Two');
		const both = undoAction(2);
		both.onClick();
		await settle();
		expect(editor.markers).toHaveLength(0);
		await editor.addMarker(1, 'One');
		await editor.addMarker(2, 'Two');
		const stale = undoAction(2);
		await editor.addMarker(3, 'Three');
		shown.length = 0;
		stale.onClick();
		await settle();
		expect(editor.markers).toHaveLength(3);
		expect(shown).toMatchObject([{ kind: 'warning' }]);
	});

	test('undoToast shows the message at its level with that Undo', async () => {
		await editor.addMarker(1, 'One');
		undoToast('Done', 'success');
		undoToast('Plain');
		expect(shown.map((s) => [s.kind, s.text])).toEqual([
			['success', 'Done'],
			['note', 'Plain']
		]);
		expect(typeof shown[0].undo).toBe('function');
	});
});

describe('no toast undoes whatever is newest', () => {
	const read = (f: string) => readFileSync(join(import.meta.dir, f), 'utf8');
	const stale = /label: 'Undo',\s*onClick: \(\) => void editor\.undo\(\)/;

	test('the timeline’s, ops.ts’s and the Sound FX panel’s toasts all go through undoAction', () => {
		for (const f of ['ops.ts', 'sfx-panel.svelte.ts', 'components/editor/Timeline.svelte']) {
			expect(read(f), f).not.toMatch(stale);
		}
		expect(read('ops.ts').match(/undoAction\(/g)!.length).toBeGreaterThanOrEqual(12);
		expect(read('sfx-panel.svelte.ts')).toContain('toast.success(addedLine(done.clips.length, only), { action: undoAction() })');
		expect(read('components/editor/Timeline.svelte').match(/undoToast\(/g)).toHaveLength(3);
	});
});
