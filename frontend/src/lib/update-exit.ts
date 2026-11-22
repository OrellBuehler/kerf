// Running an update's installer, as far as the GPU preview's markers are concerned.
//
// On Windows the updater plugin runs the installer and ends the process with `std::process::exit(0)`: no
// `RunEvent::Exit`, so the backend never learns that this run closed on purpose, and two updates in a row with the
// GPU preview up would be read as two crashes. The plugin has an `on_before_exit` hook for it, but only for an updater
// built in Rust. The page's `download()` and `install()` are two calls, so the page says so between them: as late as
// the plugin lets it, only on Windows (the one platform where the process ends there), and only when a backend is up
// (with the GPU preview off nothing is sent). It says so again if the install failed and the app goes on.

/** How long the page waits for the backend to take either word, milliseconds. The backend answers at once; a worker
 *  that does not (a stuck frame holding something) must not hold the install, or the error of one that failed, back. */
export const ANNOUNCE_LIMIT_MS = 2000;

/** `promise` for at most `ms`, and never a rejection: the word is best effort. */
async function bounded(promise: Promise<unknown>, ms: number): Promise<void> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	await Promise.race([
		promise.then(
			() => {},
			() => {}
		),
		new Promise<void>((resolve) => (timer = setTimeout(resolve, ms)))
	]);
	clearTimeout(timer);
}

/**
 * Whether a backend is up *now*: the status read after it was asked to refresh, for at most `limit`. The page's copy of
 * the status is not refreshed when the renderer goes from FFmpeg's to the GPU's (it is read when a frame the GPU did not
 * draw comes back, and when the setting changes), so a backend that came up since can still read as down; a refresh
 * that does not answer, or fails, leaves what the copy says.
 */
export async function readyAfterRefresh(refresh: () => Promise<unknown>, ready: () => boolean, limit = ANNOUNCE_LIMIT_MS): Promise<boolean> {
	await bounded(refresh(), limit);
	return ready();
}

/**
 * Download, then install, with the backend told in between when `announce()` (asked after the download, so it says
 * whether a backend is up *now*) is true: `leaving` before the install, `staying` if it fails. The install's own error
 * is always the caller's, and neither word can hold either of them back for more than `limit`. A failed install
 * also lets go of what was downloaded (`discard`: the installer package, which would otherwise stay in memory until a
 * retry downloads another or the page goes), whether or not the backend was told.
 */
export async function installAfterDownload<T>(steps: {
	download: () => Promise<unknown>;
	install: () => Promise<T>;
	announce: () => boolean | Promise<boolean>;
	leaving: () => Promise<unknown>;
	staying: () => Promise<unknown>;
	discard?: () => Promise<unknown>;
	limit?: number;
}): Promise<T> {
	const limit = steps.limit ?? ANNOUNCE_LIMIT_MS;
	await steps.download();
	const told = await steps.announce();
	if (told) await bounded(steps.leaving(), limit);
	try {
		return await steps.install();
	} catch (error) {
		if (told) await bounded(steps.staying(), limit);
		if (steps.discard) await bounded(steps.discard(), limit);
		throw error;
	}
}
