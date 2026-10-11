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

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** A release date as people write it — `1 Oct 2026` — from the ISO stamp the release carries; null
 *  when it is not one. English month names always: the dialog is English, and the same release
 *  should not read `10/1/2026` to one person and `01.10.2026` to another. */
export function releaseDate(iso: string | null | undefined): string | null {
	const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(iso ?? '');
	if (!m) return null;
	const month = MONTHS[Number(m[2]) - 1];
	return month ? `${Number(m[3])} ${month} ${m[1]}` : null;
}

/** The line over the dialog's body: what is running, and what is on offer. `Current 0.24.0 → 0.25.0 ·
 *  1 Oct 2026` — not "Installed … → …", which reads as though the new version were already in. */
export function versionLine(current: string, update: { version: string; date?: string | null } | null): string {
	const now = `Current ${current || '…'}`;
	if (!update) return now;
	const date = releaseDate(update.date);
	return `${now} → ${update.version}${date ? ` · ${date}` : ''}`;
}

/** What the dialog says when there is nothing to install, and while a download runs. The download
 *  is not something to walk away from — on Windows the installer takes the app down when it is
 *  done — so the dialog holds still and says so, instead of claiming to work "in the background". */
export const CURRENT_LINE = "You're on the latest version.";
export function downloadingNote(windows: boolean): string {
	return windows ? 'Downloading the update. Kerf may close when it is done.' : 'Downloading the update…';
}

/** The fallback offered under a failed install. Only the AppImage build updates in place, so
 *  only on Linux is there a build that cannot — elsewhere the error says all there is to say. */
export function installNote(errorKind: 'check' | 'install' | null, linux: boolean): string | null {
	if (errorKind !== 'install' || !linux) return null;
	return 'In-place updates need the AppImage build. For a .deb or .rpm install, download the new version from the release page.';
}
