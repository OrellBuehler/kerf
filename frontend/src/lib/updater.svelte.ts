// Auto-update state (Svelte 5 runes).
//
// Kerf ships from GitHub releases, so the desktop app asks that release feed
// whether a newer *signed* bundle exists (see the `updater` block in
// crates/kerf-app/tauri.conf.json) and offers to install it in place. The
// checking and installing live in `api.ts`; this singleton is the state
// machine the title bar badge and the update dialog render:
//
//   idle → checking → { current | available → downloading → ready } | error
//
// A silent check runs at startup and every few hours after; the dialog only
// pops itself open the first time a given version is seen (remembered in
// localStorage), so declining an update doesn't nag on every launch — the
// title-bar badge keeps offering it instead.

import {
	appVersion,
	checkUpdate,
	installUpdate,
	openReleases,
	relaunchApp,
	RELEASES_URL,
	type UpdateProgress
} from './api';
import type { UpdateInfo } from './types';

const SEEN_KEY = 'kerf.update.seen';
const RECHECK_MS = 6 * 60 * 60 * 1000;

export type UpdatePhase = 'idle' | 'checking' | 'current' | 'available' | 'downloading' | 'ready' | 'error';

class Updater {
	/** The running build's version (`dev` in the browser harness). */
	version = $state('');
	phase = $state<UpdatePhase>('idle');
	update = $state<UpdateInfo | null>(null);
	progress = $state<UpdateProgress | null>(null);
	error = $state<string | null>(null);
	/** Which step failed — an install failure has a manual-download fallback. */
	errorKind = $state<'check' | 'install' | null>(null);
	dialogOpen = $state(false);

	/** True while a check the user asked for is running (drives the spinner only). */
	manual = $state(false);

	#timer: ReturnType<typeof setInterval> | null = null;

	/** Fraction of the download done, or null while the size is unknown. */
	get fraction(): number | null {
		const p = this.progress;
		if (!p?.total) return null;
		return Math.min(1, p.downloaded / p.total);
	}

	/** Start the silent startup check and the periodic re-check. Returns a stop fn. */
	init(): () => void {
		void appVersion().then((v) => (this.version = v));
		void this.check(true);
		this.#timer = setInterval(() => void this.check(true), RECHECK_MS);
		return () => {
			if (this.#timer) clearInterval(this.#timer);
			this.#timer = null;
		};
	}

	/**
	 * Ask GitHub for a newer release. A silent check swallows network errors
	 * (offline is not something to interrupt an edit over) and only opens the
	 * dialog for a version this install has not been offered before; a manual
	 * check always reports what it found.
	 */
	async check(silent = false): Promise<void> {
		if (this.phase === 'checking' || this.phase === 'downloading') return;
		this.phase = 'checking';
		this.manual = !silent;
		this.error = null;
		this.errorKind = null;
		try {
			const found = await checkUpdate();
			this.update = found;
			this.phase = found ? 'available' : 'current';
			if (found && (!silent || !this.#seen(found.version))) {
				this.#remember(found.version);
				this.dialogOpen = true;
			}
		} catch (e) {
			this.error = msg(e);
			this.errorKind = 'check';
			this.phase = 'error';
			if (silent) this.update = null;
		} finally {
			this.manual = false;
		}
	}

	/** Download and install the pending update, then wait for the user to restart. */
	async install(): Promise<void> {
		if (!this.update) return;
		this.phase = 'downloading';
		this.progress = { downloaded: 0, total: null };
		this.error = null;
		this.errorKind = null;
		try {
			await installUpdate((p) => (this.progress = p));
			this.phase = 'ready';
		} catch (e) {
			this.error = msg(e);
			this.errorKind = 'install';
			this.phase = 'error';
		}
	}

	/** Restart into the installed version. */
	async restart(): Promise<void> {
		await relaunchApp();
	}

	/**
	 * Manual fallback when the in-place install can't run (a .deb / .rpm install).
	 * A rejection here (no browser, a URL the opener scope refuses) would be
	 * invisible otherwise — the button would just look dead — so report it.
	 */
	async openReleasePage(): Promise<void> {
		try {
			await openReleases();
		} catch (e) {
			this.error = `Couldn't open ${RELEASES_URL} — ${msg(e)}`;
		}
	}

	open() {
		this.dialogOpen = true;
		if (this.phase === 'idle' || this.phase === 'current' || this.phase === 'error') void this.check(false);
	}

	close() {
		this.dialogOpen = false;
		if (this.phase === 'error') this.phase = this.update ? 'available' : 'idle';
	}

	#seen(version: string): boolean {
		try {
			return localStorage.getItem(SEEN_KEY) === version;
		} catch {
			return false;
		}
	}

	#remember(version: string) {
		try {
			localStorage.setItem(SEEN_KEY, version);
		} catch {
			// Private-mode storage failure just means the dialog opens again.
		}
	}
}

function msg(e: unknown): string {
	return e instanceof Error ? e.message : String(e);
}

export const updater = new Updater();
