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

/** Just enough of `navigator` to tell the platform. */
export interface PlatformNavigator {
	userAgentData?: { platform?: string };
	platform?: string;
	userAgent?: string;
}

/** Whether this is a Linux desktop (the webview's own answer; Android also says "Linux" in its
 *  user agent, and is not one). */
export function onLinux(nav: PlatformNavigator | undefined = typeof navigator === 'undefined' ? undefined : navigator): boolean {
	const p = nav?.userAgentData?.platform || nav?.platform || nav?.userAgent || '';
	return /linux/i.test(p) && !/android/i.test(nav?.userAgent ?? '');
}

/** The fallback offered under a failed install. Only the AppImage build updates in place, so
 *  only on Linux is there a build that cannot — elsewhere the error says all there is to say. */
export function installNote(errorKind: 'check' | 'install' | null, linux: boolean): string | null {
	if (errorKind !== 'install' || !linux) return null;
	return 'In-place updates need the AppImage build. For a .deb or .rpm install, download the new version from the release page.';
}
