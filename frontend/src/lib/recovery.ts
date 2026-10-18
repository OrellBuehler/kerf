// The launch prompt that offers an untitled project a crashed Kerf left behind: Recover, Discard,
// or Not now (asked again next launch). Copy and answer mapping are data, like `confirm-copy.ts`,
// so a test can hold them and `api.ts` (which shows the native dialog) agrees with the caller.

import type { RecoveryInfo } from './types';

export interface RecoveryCopy {
	title: string;
	message: string;
	recover: string;
	discard: string;
	later: string;
}

export type RecoveryChoice = 'recover' | 'discard' | 'later';

function plural(n: number, one: string, many: string): string {
	return `${n} ${n === 1 ? one : many}`;
}

/** What the session holds, in a few words: `3 clips, 2 media files`. */
export function recoverySummary(info: RecoveryInfo): string {
	const parts: string[] = [];
	if (info.clips > 0) parts.push(plural(info.clips, 'clip', 'clips'));
	if (info.assets > 0) parts.push(plural(info.assets, 'media file', 'media files'));
	if (parts.length === 0 && info.edits > 0) parts.push(plural(info.edits, 'edit', 'edits'));
	return parts.join(', ');
}

export function recoveryCopy(info: RecoveryInfo, when: (iso: string) => string = (iso) => new Date(iso).toLocaleString()): RecoveryCopy {
	const summary = recoverySummary(info);
	const at = info.modified_at ? ` (last changed ${when(info.modified_at)})` : '';
	const holds = summary ? ` It holds ${summary}.` : '';
	return {
		title: 'Recover unsaved project?',
		message: `Kerf closed before an untitled project was saved${at}.${holds}`,
		recover: 'Recover',
		discard: 'Discard',
		later: 'Not now'
	};
}

/** Which choice a native dialog's answer is. Anything that is not Recover or Discard — the window
 *  closed, a platform's own label — is Not now, so a dismissed dialog never deletes work. */
export function recoveryChoice(answer: string, copy: Pick<RecoveryCopy, 'recover' | 'discard'>): RecoveryChoice {
	if (answer === copy.recover) return 'recover';
	if (answer === copy.discard) return 'discard';
	return 'later';
}
