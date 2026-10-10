// What the update dialog says for each state of the updater — pure, so the state that
// went wrong (a failed check titled "up to date") has a test.

import type { UpdatePhase } from './updater.svelte';
import type { UpdateInfo } from './types';

/** The dialog's title. A failed check has no update to name and is not "up to date" either. */
export function updateTitle(phase: UpdatePhase, update: Pick<UpdateInfo, 'version'> | null): string {
	if (phase === 'downloading') return 'Downloading update';
	if (phase === 'ready') return 'Update installed';
	if (phase === 'checking') return 'Checking for updates';
	if (update) return `Kerf ${update.version} is available`;
	return phase === 'error' ? "Couldn't check for updates" : 'Kerf is up to date';
}

const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

export interface ErrorView {
	/** What the box says. */
	headline: string;
	/** The raw message under a disclosure, when the headline is a summary of it. */
	details: string | null;
}

/** The error box. A failed check says what happened in words; the transport's own message — a
 *  request error naming a URL, which reads as noise — goes under *Details*. Any other message is
 *  the headline itself, its first letter capitalised. */
export function errorView(kind: 'check' | 'install' | null, raw: string): ErrorView {
	const message = capital(raw.trim());
	if (kind !== 'check') return { headline: message, details: null };
	return { headline: "Couldn't reach the update server. Check your connection and try again.", details: message };
}

/** The fallback offered under a failed install. Only the AppImage build updates in place, so
 *  only on Linux is there a build that cannot — elsewhere the error says all there is to say. */
export function installNote(errorKind: 'check' | 'install' | null, linux: boolean): string | null {
	if (errorKind !== 'install' || !linux) return null;
	return 'In-place updates need the AppImage build. For a .deb or .rpm install, download the new version from the release page.';
}
