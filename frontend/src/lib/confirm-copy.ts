// What the confirmation prompts say. Each prompt names its choices as verbs — "Save…", "Don't save",
// "Cancel" — instead of the dialog plugin's Yes / No under "Close Kerf and lose it?", and one fact
// ("this project has never been saved") is worded one way wherever it is asked about. The copy is
// data so a test can hold it and so `api.ts` (which shows the native dialog) and the callers agree.

/** An ask with two ends: do it, or do not. */
export interface ConfirmCopy {
	title: string;
	message: string;
	/** The button that goes ahead, named for what it does. */
	ok: string;
	cancel: string;
}

/** The unsaved-project prompt: save first, let it go, or stop. */
export interface UnsavedCopy {
	title: string;
	message: string;
	/** Ends in an ellipsis: it opens the save dialog. */
	save: string;
	discard: string;
	cancel: string;
}

export type UnsavedChoice = 'save' | 'discard' | 'cancel';

const UNSAVED_FACT = "This project hasn't been saved.";

export const UNSAVED = {
	close: {
		title: 'Close Kerf?',
		message: `${UNSAVED_FACT} If you close now it will be lost.`,
		save: 'Save…',
		discard: "Don't save",
		cancel: 'Cancel'
	},
	quit: {
		title: 'Quit Kerf?',
		message: `${UNSAVED_FACT} If you quit now it will be lost.`,
		save: 'Save…',
		discard: "Don't save",
		cancel: 'Cancel'
	},
	replace: {
		title: 'Replace this project?',
		message: `${UNSAVED_FACT} Starting another one will lose it.`,
		save: 'Save…',
		discard: 'Discard',
		cancel: 'Cancel'
	}
} as const satisfies Record<string, UnsavedCopy>;

export const CONFIRM = {
	restart: {
		title: 'Restart Kerf?',
		message: `${UNSAVED_FACT} If you restart now it will be lost.`,
		ok: 'Restart anyway',
		cancel: 'Cancel'
	},
	resetWorkspaces: {
		title: 'Reset all workspaces?',
		message: 'Every workspace goes back to its default layout. Arrangements you made are lost.',
		ok: 'Reset all',
		cancel: 'Cancel'
	},
	staleApply: {
		title: 'Apply stale changes?',
		message: 'You edited the timeline after these changes were staged. Applying them replaces your newer cut.',
		ok: 'Apply and replace',
		cancel: 'Cancel'
	},
	replaceCaptions: { title: 'Replace captions?', ok: 'Replace', cancel: 'Cancel' }
} as const;

/** What would be lost if the app closed this moment: the project nobody saved, a render, an analysis. */
export interface InstallRisk {
	unsaved: boolean;
	exporting: boolean;
	analyzing: boolean;
}

/** The prompt before an update is installed. On Windows the installer takes the app down when the
 *  download is done, past every close guard, so the question is asked *before* it starts: with
 *  unsaved work it is the three-button Save… / Install anyway / Cancel, with only a job running a
 *  two-button one, and with nothing at stake `null`. */
export function installPrompt(risk: InstallRisk):
	| { kind: 'unsaved'; copy: UnsavedCopy }
	| { kind: 'jobs'; copy: ConfirmCopy }
	| null {
	const lines = ['Kerf will close to install the update.'];
	if (risk.unsaved) lines.push(`${UNSAVED_FACT} If you install now it will be lost.`);
	if (risk.exporting) lines.push('The export in progress will be stopped.');
	if (risk.analyzing) lines.push('The analysis in progress will be stopped.');
	const message = lines.join(' ');
	if (risk.unsaved)
		return { kind: 'unsaved', copy: { title: 'Install the update?', message, save: 'Save…', discard: 'Install anyway', cancel: 'Cancel' } };
	if (risk.exporting || risk.analyzing)
		return { kind: 'jobs', copy: { title: 'Install the update?', message, ok: 'Install anyway', cancel: 'Cancel' } };
	return null;
}

/** The update dialog's own line, over the Restart button: the same fact, the way to fix it. */
export const UPDATE_UNSAVED_NOTE = `${UNSAVED_FACT} Save it before restarting or it will be lost.`;

/** Which choice a native dialog's answer is. A custom-button dialog answers with the label of the
 *  button pressed; closing the window answers with the cancel label (or something else entirely) —
 *  anything that is not Save or Discard is Cancel, so a dismissed dialog never loses work. */
export function unsavedChoice(answer: string, copy: Pick<UnsavedCopy, 'save' | 'discard'>): UnsavedChoice {
	if (answer === copy.save) return 'save';
	if (answer === copy.discard) return 'discard';
	return 'cancel';
}

/** Whether to go ahead once the choice is made. "Save…" saves first and goes ahead only if a file
 *  was written (the save dialog can be cancelled, or fail); "Discard" goes ahead; "Cancel" does not. */
export async function proceedAfter(choice: UnsavedChoice, save: () => Promise<boolean>): Promise<boolean> {
	if (choice === 'cancel') return false;
	if (choice === 'discard') return true;
	try {
		return await save();
	} catch {
		return false;
	}
}
