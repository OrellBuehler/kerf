import { describe, expect, test } from 'bun:test';
import { CONFIRM, UNSAVED, UPDATE_UNSAVED_NOTE, proceedAfter, unsavedChoice } from './confirm-copy';

describe('the unsaved-project prompts', () => {
	test('say the same fact the same way, and name each ending', () => {
		for (const [name, copy] of Object.entries(UNSAVED)) {
			expect(copy.message, name).toStartWith("This project hasn't been saved.");
			expect(copy.message, name).not.toContain('never been saved');
			expect(copy.message, name).not.toContain('Continue?');
		}
		expect(UNSAVED.close).toMatchObject({ title: 'Close Kerf?', save: 'Save…', discard: "Don't save", cancel: 'Cancel' });
		expect(UNSAVED.quit.message).toBe("This project hasn't been saved. If you quit now it will be lost.");
		expect(UNSAVED.replace).toMatchObject({ title: 'Replace this project?', discard: 'Discard' });
		expect(CONFIRM.restart.message).toBe("This project hasn't been saved. If you restart now it will be lost.");
		expect(UPDATE_UNSAVED_NOTE).toBe("This project hasn't been saved. Save it before restarting or it will be lost.");
	});

	test('no button is Yes or No, and the one that opens the save dialog ends in an ellipsis', () => {
		const buttons = [
			...Object.values(UNSAVED).flatMap((c) => [c.save, c.discard, c.cancel]),
			...Object.values(CONFIRM).flatMap((c) => [c.ok, c.cancel])
		];
		for (const b of buttons) expect(b).not.toMatch(/^(yes|no|ok)$/i);
		for (const c of Object.values(UNSAVED)) expect(c.save.endsWith('…')).toBe(true);
	});

	test('every confirm is a question with a title that names it', () => {
		for (const c of [...Object.values(UNSAVED), ...Object.values(CONFIRM).filter((c): c is typeof CONFIRM.restart => 'message' in c)])
			expect(c.title.endsWith('?')).toBe(true);
		expect(CONFIRM.resetWorkspaces.ok).toBe('Reset all');
		expect(CONFIRM.staleApply.ok).toBe('Apply and replace');
	});
});

describe('which choice a native answer is', () => {
	test('the label of the button pressed', () => {
		expect(unsavedChoice('Save…', UNSAVED.close)).toBe('save');
		expect(unsavedChoice("Don't save", UNSAVED.close)).toBe('discard');
		expect(unsavedChoice('Cancel', UNSAVED.close)).toBe('cancel');
	});
	test('anything else — a closed dialog, an unknown label — is Cancel, so nothing is lost', () => {
		for (const answer of ['', 'No', 'Yes', 'Ok', 'save…', 'undefined']) expect(unsavedChoice(answer, UNSAVED.close)).toBe('cancel');
	});
	test('Replace says Discard where Close says Don’t save', () => {
		expect(unsavedChoice('Discard', UNSAVED.replace)).toBe('discard');
		expect(unsavedChoice('Discard', UNSAVED.close)).toBe('cancel');
	});
});

describe('going ahead', () => {
	test('Cancel stops and does not save; Discard goes on and does not save', async () => {
		let saved = 0;
		const save = async () => (saved++, true);
		expect(await proceedAfter('cancel', save)).toBe(false);
		expect(await proceedAfter('discard', save)).toBe(true);
		expect(saved).toBe(0);
	});
	test('Save goes ahead only once a file was written', async () => {
		expect(await proceedAfter('save', async () => true)).toBe(true);
		// the save dialog was cancelled
		expect(await proceedAfter('save', async () => false)).toBe(false);
		// the write failed
		expect(
			await proceedAfter('save', async () => {
				throw new Error('permission denied');
			})
		).toBe(false);
	});
});
