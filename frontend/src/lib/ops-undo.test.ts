import { beforeEach, describe, expect, mock, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { posix, SRC, uiSources } from './test-sources';
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
const { UNDO_ALREADY, UNDO_MOVED_ON, addMarker, undoAction, undoToast } = await import('./ops');
const { revertTo } = await import('./api');

const settle = () => new Promise((r) => setTimeout(r, 20));
/** The harness stamps revisions to the millisecond: let one pass, as a person's next edit would. */
const tick = () => new Promise((r) => setTimeout(r, 5));
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

	test('a revision number reused by a later edit is not the toast\u2019s edit: Undo, then a new edit, then the old toast\u2019s Undo', async () => {
		// Recording an edit drops the redo branch and takes head + 1, so the new edit gets the undone one's `seq`.
		await addMarker();
		const first = shown[0];
		const seqA = editor.history.find((r) => r.current)!.seq;
		await editor.undo();
		await tick();
		await editor.addMarker(9, 'B');
		expect(editor.history.find((r) => r.current)!.seq).toBe(seqA);
		shown.length = 0;
		first.undo!();
		await settle();
		expect(markers()).toEqual(['B']);
		expect(shown).toMatchObject([{ kind: 'warning', text: UNDO_ALREADY }]);
	});

	test('an edit that is already undone says so, and does not send the user to Edit > Undo, which would take back another', async () => {
		await editor.addMarker(1, 'One');
		await addMarker();
		const toast = shown[0];
		await editor.undo(); // the toast's own edit, taken back from the menu
		shown.length = 0;
		toast.undo!();
		await settle();
		expect(markers()).toEqual(['One']);
		expect(shown).toMatchObject([{ kind: 'warning', text: UNDO_ALREADY }]);
		expect(UNDO_ALREADY).not.toContain('Edit > Undo');
		// redone, it is the latest again and the toast works
		await editor.redo();
		shown.length = 0;
		toast.undo!();
		await settle();
		expect(markers()).toEqual(['One']);
		expect(shown).toEqual([]);
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

	test('no file in the app gives a toast a bare editor.undo() — every Undo button is undoAction()', () => {
		const sources = uiSources(SRC).filter((f) => !f.endsWith('.test.ts'));
		expect(sources.length).toBeGreaterThan(100);
		const bare = sources.filter((f) => /onClick:\s*\(\)\s*=>\s*(?:void\s+)?editor\.undo\(\)/.test(readFileSync(f, 'utf8')));
		expect(bare.map(posix)).toEqual([]);
		// and the ones that offer an Undo at all go through it
		const users = sources.filter((f) => /\bundoAction\(|\bundoToast\(/.test(readFileSync(f, 'utf8'))).map(posix);
		for (const f of [
			'lib/ops.ts',
			'lib/sfx-panel.svelte.ts',
			'routes/+page.svelte',
			...[
				'Timeline',
				'MediaBin',
				'StemsDialog',
				'ComposeMusicDialog',
				'FitMusicDialog',
				'AgentPanel',
				'TranscriptPanel',
				'Inspector'
			].map((n) => `lib/components/editor/${n}.svelte`)
		])
			expect(users, f).toContain(f);
	});

	test('the timeline\u2019s, ops.ts\u2019s and the Sound FX panel\u2019s toasts all go through undoAction', () => {
		expect(read('ops.ts').match(/undoAction\(/g)!.length).toBeGreaterThanOrEqual(12);
		expect(read('sfx-panel.svelte.ts')).toContain('toast.success(addedLine(done.clips.length, only), { action: undoAction() })');
		expect(read('components/editor/Timeline.svelte').match(/undoToast\(/g)).toHaveLength(3);
	});

	test('a revision is remembered by its number and its time', () => {
		const ops = read('ops.ts');
		expect(ops).toContain('headStamp(editor.history)');
		expect(ops).toContain('undoVerdict(at, editor.history)');
		expect(ops).not.toContain('headSeq');
	});
});
