import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';

// Undo and redo as the keys, the menu and the toast's own button run them (`ops.ts`): one step at a time, one line that
// counts up while nothing else is edited, and a quiet word — not the last line's Redo, not a red error — at the end of
// the history. The toasts stand in for what only a window shows.

interface Shown {
	kind: string;
	text: string;
	opts?: { id?: string; action?: { label: string }; duration?: number };
}
const shown: Shown[] = [];
const dismissed: (string | undefined)[] = [];
const note = (kind: string) => (text: string, opts?: Shown['opts']) => void shown.push({ kind, text, opts });
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), {
		success: note('success'),
		warning: note('warning'),
		error: note('error'),
		info: note('info'),
		dismiss: (id?: string) => void dismissed.push(id)
	})
}));

const { editor } = await import('./state.svelte');
const ops = await import('./ops');
const { revertTo } = await import('./api');

const lines = () => shown.filter((s) => s.opts?.id === 'undo-redo').map((s) => s.text);
const mark = async () => {
	await editor.addMarkerAtPlayhead(1);
};

beforeEach(async () => {
	await revertTo(0);
	await editor.load();
	editor.clearSelection();
	shown.length = 0;
	dismissed.length = 0;
});
afterAll(async () => {
	// The harness is a shared singleton: leave it at its seed for the next file.
	await revertTo(0);
	await editor.load();
});

describe('a held key', () => {
	test('never has two steps in flight, and runs the one that arrived meanwhile once the first is done', async () => {
		await mark();
		await mark();
		await mark();
		let running = 0;
		let most = 0;
		const real = editor.undo.bind(editor);
		editor.undo = (async () => {
			running++;
			most = Math.max(most, running);
			await new Promise((r) => setTimeout(r, 15));
			const done = await real();
			running--;
			return done;
		}) as typeof editor.undo;
		try {
			// A key held: five presses while the first step is still in flight.
			const all = [ops.undoEdit(), ops.undoEdit(), ops.undoEdit(), ops.undoEdit(), ops.undoEdit()];
			await Promise.all(all);
		} finally {
			// @ts-expect-error: put the class's own method back
			delete editor.undo;
		}
		expect(most).toBe(1);
		// The first, and the latest of the rest (not a queue of four): two steps in all.
		expect(lines()).toEqual(['Undid: Add marker', 'Undid 2 edits, back to before Add marker']);
	});
});

describe('the line a burst says', () => {
	test('presses with nothing else between count up and say where they got to', async () => {
		await mark();
		await mark();
		await ops.undoEdit();
		await ops.undoEdit();
		expect(lines()).toEqual(['Undid: Add marker', 'Undid 2 edits, back to before Add marker']);
	});

	test('undo, edit, undo is two lines of one — another edit between breaks the burst', async () => {
		await mark();
		await mark();
		await ops.undoEdit();
		await mark();
		await ops.undoEdit();
		expect(lines()).toEqual(['Undid: Add marker', 'Undid: Add marker']);
	});

	test('the opposite action is offered beside it', async () => {
		await mark();
		await ops.undoEdit();
		expect(shown.at(-1)?.opts?.action?.label).toBe('Redo');
		await ops.redoEdit();
		expect(shown.at(-1)?.text).toBe('Redid: Add marker');
		expect(shown.at(-1)?.opts?.action?.label).toBe('Undo');
	});
});

describe('the end of the history', () => {
	test('is said once, quietly, as its own toast with no Redo of the line before, which goes', async () => {
		await mark();
		await ops.undoEdit();
		shown.length = 0;
		dismissed.length = 0;
		await ops.undoEdit();
		const end = shown.at(-1)!;
		expect([end.kind, end.text]).toEqual(['info', 'Nothing to undo']);
		expect(end.opts?.id).toBe('undo-end');
		expect(end.opts?.action).toBeUndefined();
		expect(dismissed).toContain('undo-redo');
		expect(shown.some((s) => s.kind === 'error')).toBe(false);
	});

	test('a press that outran the history — the backend refusing "nothing to undo" — is the same quiet notice, not a red toast', async () => {
		// The head not yet read back says there is something to undo; the backend knows better.
		Object.defineProperty(editor, 'canUndo', { get: () => true, configurable: true });
		editor.undo = (async () => {
			throw new Error('invalid argument: nothing to undo');
		}) as typeof editor.undo;
		try {
			await ops.undoEdit();
		} finally {
			delete (editor as unknown as Record<string, unknown>).canUndo;
			delete (editor as unknown as Record<string, unknown>).undo;
		}
		expect(shown.some((s) => s.kind === 'error')).toBe(false);
		expect(shown.at(-1)).toMatchObject({ kind: 'info', text: 'Nothing to undo' });
		expect(shown.at(-1)?.opts?.action).toBeUndefined();
	});

	test('any other refusal is still an error', async () => {
		await mark();
		editor.undo = (async () => {
			throw new Error('the project is busy');
		}) as typeof editor.undo;
		try {
			await ops.undoEdit();
		} finally {
			delete (editor as unknown as Record<string, unknown>).undo;
		}
		expect(shown.at(-1)).toMatchObject({ kind: 'error', text: 'the project is busy' });
	});
});
