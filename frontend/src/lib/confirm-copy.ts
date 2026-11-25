// What the confirmation prompts say. Each prompt names its choices as verbs — "Save…", "Don't save",
// "Cancel" — instead of the dialog plugin's Yes / No under "Close Kerf and lose it?", and one fact
// ("this project has never been saved") is worded one way wherever it is asked about. The copy is
// data so a test can hold it and so `api.ts` (which shows the native dialog) and the callers agree.

import { branchSentence } from './history-view';

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
		message: 'Edit, Color, Audio, Motion and Deliver go back to their default layouts. The arrangements you made in them are lost; workspaces you saved are kept.',
		ok: 'Reset all',
		cancel: 'Cancel'
	},
	staleApply: {
		title: 'Apply stale changes?',
		message: 'You edited the timeline after these changes were staged. Applying them replaces your newer cut.',
		ok: 'Apply and replace',
		cancel: 'Cancel'
	},
	replaceCaptions: { title: 'Replace captions?', ok: 'Replace', cancel: 'Cancel' },
	deleteTemplate: { title: 'Delete this template?', ok: 'Delete', cancel: 'Cancel' },
	resetSettings: {
		title: 'Reset all settings?',
		message:
			'Every preference goes back to its default: shortcuts, window layout and the workspaces you saved, analysis, preview and the rest, and the theme goes back to Kerf Dark. Every theme you made or imported is deleted from Kerf (export the ones you want to keep first; the old settings file is kept beside the new one, but Kerf does not read themes back from it). Where the agent server listens and its token stay as they are.',
		ok: 'Reset all settings',
		cancel: 'Cancel'
	},
	clearRecent: {
		title: 'Clear recent projects?',
		message: 'The list in File › Open Recent is emptied. The project files themselves are not touched.',
		ok: 'Clear the list',
		cancel: 'Cancel'
	},
	agentReviewOff: {
		title: 'Let the agent edit without review?',
		message:
			'With review off, an agent’s edits land on your timeline as soon as it makes them, as steps you can undo — also the work it does on the tasks you queue, which then show as applied instead of waiting for you. A proposal that is already waiting stays until you apply or discard it. You can turn review back on at any time.',
		ok: 'Turn review off',
		cancel: 'Keep reviewing'
	},
	serverOff: {
		title: 'Stop letting agents connect?',
		message: 'Kerf’s agent server shuts down. Agents that are connected lose the connection and none can connect until you turn it back on. Nothing in your project changes.',
		ok: 'Turn it off',
		cancel: 'Keep it on'
	},
	tokenOff: {
		title: 'Stop requiring the access token?',
		message:
			'Any program on this computer will be able to read your project and edit it through Kerf without the token. Turn this off only on a machine nobody else uses. Agents that are connected are disconnected and have to connect again.',
		ok: 'Stop requiring it',
		cancel: 'Keep requiring it'
	},
	restoreStale: {
		title: 'Bring back changes from an older cut?',
		message:
			'You edited the timeline after these changes were discarded. They come back as a proposal against the older cut, and applying it then replaces your newer one — you are asked again before that.',
		ok: 'Bring them back',
		cancel: 'Cancel'
	},
	newToken: {
		title: 'Make a new access token?',
		message: 'The current token stops working at once. Every agent that connected with it has to be given the new one.',
		ok: 'Make a new token',
		cancel: 'Cancel'
	},
	discardThemeEdits: {
		title: 'Discard your changes?',
		message: 'The theme goes back to how it was when you opened the editor.',
		ok: 'Discard changes',
		cancel: 'Keep editing'
	},
	deleteTheme: { title: 'Delete this theme?', ok: 'Delete', cancel: 'Cancel' }
} as const;

/** Deleting a workspace the user saved: what goes (the arrangement of its panels) and what does not (the panels, and the project). */
export function deleteWorkspaceCopy(name: string): ConfirmCopy {
	return {
		title: `Delete the ${name} workspace?`,
		message: `The arrangement of panels you saved as ${name} is lost. The panels themselves, your other workspaces and the project are not touched.`,
		ok: 'Delete workspace',
		cancel: 'Cancel'
	};
}

/** Cancelling the whole proxy queue: what stops, what does not come back by itself, and that nothing already built or
 *  exported is touched. `count` is the jobs in the queue, the build under way included. */
export function cancelProxyQueueCopy(count: number): ConfirmCopy {
	const what = count === 1 ? 'the preview proxy that is queued' : `all ${count} preview proxies in the queue`;
	return {
		title: count === 1 ? 'Cancel the proxy in the queue?' : `Cancel ${count} proxies?`,
		message: `Kerf stops building ${what}. They are not queued again by themselves this session: Build next or Rebuild asks for one, and until it is built the preview reads the original file (or waits for it, with Proxy only on). Proxies that are already built, and exports, are not touched.`,
		ok: count === 1 ? 'Cancel it' : 'Cancel all',
		cancel: 'Keep building'
	};
}

/** Taking media out of the project. Unused media is a plain question; media a clip uses (or a proposal holds) says
 *  what goes with it. `uses` is the clips of the cut, `proposed` whether an agent's proposal also holds clips of it. */
export function removeMediaCopy(name: string, uses: number, proposed = false): ConfirmCopy {
	const clips = uses === 1 ? '1 clip' : `${uses} clips`;
	if (uses === 0 && !proposed)
		return {
			title: `Remove ${name}?`,
			message: `${name} leaves the library, and its analysis and preview proxies are deleted. The file on disk stays where it is.`,
			ok: 'Remove media',
			cancel: 'Cancel'
		};
	const where = uses > 0 ? `${clips} on the timeline${proposed ? ' and in the agent’s proposal' : ''}` : 'the agent’s proposal';
	return {
		title: `Remove ${name} and what uses it?`,
		message: `${name} is used by ${where}. Removing it removes ${uses > 0 ? 'those clips' : 'those clips from the proposal'} as one step — and that step cannot be undone, because the media is gone. The file on disk stays.`,
		ok: uses > 0 ? `Remove media and ${uses === 1 ? 'clip' : 'clips'}` : 'Remove media',
		cancel: 'Cancel'
	};
}

/** Pointing media at a different recording drops what was analysed from the old one (its transcript, silences,
 *  beats …). The clips stay; the analysis would have to be run again. */
export function relinkDropsAnalysisCopy(name: string): ConfirmCopy {
	return {
		title: `Relink ${name} to a different recording?`,
		message: `That file is not ${name} moved — its length, streams or bytes differ. The clips stay as they are, but the analysis of ${name} (transcript, silences, beats …) would be wrong for it, so it is dropped and has to be run again.`,
		ok: 'Relink and drop the analysis',
		cancel: 'Cancel'
	};
}

/** The export would replace files that are already there. */
export function overwriteCopy(files: readonly string[]): ConfirmCopy {
	const names = files.map((f) => f.split(/[\\/]/).filter(Boolean).pop() ?? f);
	const shown = names.slice(0, 4).join(', ') + (names.length > 4 ? ` and ${names.length - 4} more` : '');
	return {
		title: files.length === 1 ? 'Replace the existing file?' : `Replace ${files.length} existing files?`,
		message: `${shown} ${files.length === 1 ? 'is' : 'are'} already there. Exporting replaces ${files.length === 1 ? 'it' : 'them'}.`,
		ok: files.length === 1 ? 'Replace' : 'Replace all',
		cancel: 'Cancel'
	};
}

/** Throwing away a proposal of several changes: named for what is lost (the agent's work), with the
 *  button that keeps it saying what it does. The agent's work is not gone for good — the project keeps the last proposal
 *  thrown away and the Agent panel offers Restore — but only until the agent stages another, and the question says so rather
 *  than promising more than that. */
export function discardProposalCopy(changes: number): ConfirmCopy {
	return {
		title: 'Discard the agent’s changes?',
		message: `${changes} proposed change${changes === 1 ? '' : 's'} will be thrown away. Your timeline is not touched. You can bring them back with Restore in the Agent panel until the agent proposes something new.`,
		ok: 'Discard',
		cancel: 'Keep reviewing'
	};
}

/** Applying a proposal the cut has moved on from. `edits` is how many edits the user made since the agent staged it, when that
 *  is known (`null`: it cannot be stood behind, and no number is given). A whole apply puts the agent's version in place of the
 *  cut; a partial one puts each ticked change onto the cut as it now is and skips one whose target is gone — so the two are
 *  not told the same sentence. The answer names what Undo does, because it is the way back. */
export function staleApplyCopy(edits: number | null, partial = false): ConfirmCopy {
	const since = edits !== null && edits > 0 ? `You made ${edits} edit${edits === 1 ? '' : 's'} after these changes were staged.` : 'You edited the timeline after these changes were staged.';
	const does = partial
		? 'Applying the ticked ones puts each onto your cut as it is now; one whose clip you have since removed is skipped and listed.'
		: 'Applying them puts the agent’s version in place of your cut as it is now.';
	return {
		title: partial ? 'Apply ticked changes to a changed cut?' : CONFIRM.staleApply.title,
		message: `${since} ${does} Undo brings your cut back.`,
		ok: partial ? 'Apply ticked' : CONFIRM.staleApply.ok,
		cancel: CONFIRM.staleApply.cancel
	};
}

/** What has to be asked before an apply lands, or `null` when nothing does. Two things can: the cut changed since the proposal
 *  was staged (`stale`), and the cut is stepped back in History (`undone`) — an apply is an edit, and an edit deletes every
 *  revision after the head for good, so those edits are named, and how to keep them is said. */
export function applyConfirmCopy(o: {
	stale: boolean;
	edits: number | null;
	partial: boolean;
	undone: { count: number; labels: readonly string[]; exact: boolean };
}): ConfirmCopy | null {
	const { stale, edits, partial, undone } = o;
	if (undone.count === 0) return stale ? staleApplyCopy(edits, partial) : null;
	const n = undone.exact ? `the ${undone.count} edit${undone.count === 1 ? '' : 's'}` : `at least ${undone.count} edit${undone.count === 1 ? '' : 's'}`;
	const named = branchSentence(undone.labels, 3);
	const parts = [
		`Applying drops ${n} you stepped back from${named ? `: ${named}` : ''}. They cannot be brought back after that.`,
		stale
			? 'The cut has also changed since these changes were staged, so the agent’s version replaces it.'
			: '',
		'To keep them, cancel and go forward first (Go forward in the Agent panel, or Redo): applying over your latest cut can be undone, this cannot.'
	].filter(Boolean);
	return {
		title: `Apply and drop ${undone.count === 1 && undone.exact ? 'an edit' : 'edits'} you stepped back from?`,
		message: parts.join(' '),
		ok: 'Apply and drop',
		cancel: 'Cancel'
	};
}

/** What would be lost if the app closed this moment: the project nobody saved, a render, an analysis. */
export interface InstallRisk {
	unsaved: boolean;
	exporting: boolean;
	analyzing: boolean;
	/** A file import, or a 360 lens stitch, is running. */
	importing: boolean;
}

/** The prompt before an update is installed. On Windows the installer takes the app down when the
 *  download is done, past every close guard, so the question is asked *before* it starts: with
 *  unsaved work it is the three-button Save… / Install anyway / Cancel, with only a job running a
 *  two-button one, and with nothing at stake `null`. Elsewhere the app is not taken down — the
 *  update waits for "Restart now", which asks about unsaved work itself — so there is no question
 *  and the old flow stands: `null`. */
export function installPrompt(risk: InstallRisk, windows: boolean):
	| { kind: 'unsaved'; copy: UnsavedCopy }
	| { kind: 'jobs'; copy: ConfirmCopy }
	| null {
	if (!windows) return null;
	const lines = ['Kerf will close to install the update.'];
	if (risk.unsaved) lines.push(`${UNSAVED_FACT} If you install now it will be lost.`);
	if (risk.exporting) lines.push('The export in progress will be stopped.');
	if (risk.analyzing) lines.push('The analysis in progress will be stopped.');
	if (risk.importing) lines.push('The import in progress will be stopped.');
	const message = lines.join(' ');
	if (risk.unsaved)
		return { kind: 'unsaved', copy: { title: 'Install the update?', message, save: 'Save…', discard: 'Install anyway', cancel: 'Cancel' } };
	if (risk.exporting || risk.analyzing || risk.importing)
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
/** [`proceedAfter`] for closing or quitting: an explicit "Don't save" also lets the untitled
 *  project's recovery session go (`letGo`), so the next launch does not offer it back. Only that
 *  answer does — a failed prompt never gets here — and a `letGo` that fails still closes. */
export async function leaveAfter(
	choice: UnsavedChoice,
	save: () => Promise<boolean>,
	letGo: () => Promise<void>
): Promise<boolean> {
	const go = await proceedAfter(choice, save);
	if (go && choice === 'discard') await letGo().catch(() => {});
	return go;
}

export async function proceedAfter(choice: UnsavedChoice, save: () => Promise<boolean>): Promise<boolean> {
	if (choice === 'cancel') return false;
	if (choice === 'discard') return true;
	try {
		return await save();
	} catch {
		return false;
	}
}
