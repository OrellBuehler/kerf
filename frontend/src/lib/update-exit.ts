// Running an update's installer, as far as the GPU preview's markers are concerned.
//
// On Windows the updater plugin runs the installer and ends the process with `std::process::exit(0)`: no
// `RunEvent::Exit`, so the backend never learns that this run closed on purpose, and two updates in a row with the
// GPU preview up would be read as two crashes. The plugin has an `on_before_exit` hook for it, but only for an updater
// built in Rust; the page's `downloadAndInstall` has none. So the page says so first, and says so again if the
// install failed and the app goes on.

/**
 * `install` with the backend told before it (`leaving`) and, if it fails, after (`staying`). A backend that cannot be
 * told (no GPU preview, a command that failed) is not a reason not to install: both are best effort.
 */
export async function installLeaving<T>(
	install: () => Promise<T>,
	leaving: () => Promise<unknown>,
	staying: () => Promise<unknown>
): Promise<T> {
	await leaving().catch(() => {});
	try {
		return await install();
	} catch (error) {
		await staying().catch(() => {});
		throw error;
	}
}
