// Showing the window at the right moment.
//
// The desktop window is created hidden (`visible: false` in tauri.conf.json), so
// nobody sees the webview's first, unthemed frame. This is the webview's half of
// ending that: once the theme is applied and the dock has been built, wait for
// the frame that draws them, then ask the backend to show the window. The
// backend's half is a timer that shows it anyway if this never happens.

/** How long to wait for animation frames before assuming none are coming. A page
 *  in a window that is not shown is not visible, and a hidden page's
 *  `requestAnimationFrame` may never fire — waiting on frames alone would wait
 *  forever on the very window this exists to show. */
export const PAINT_TIMEOUT_MS = 150;

export interface PaintClock {
	/** Run `cb` before the next frame is painted. */
	frame(cb: () => void): void;
	/** Run `cb` after `ms`. */
	timeout(cb: () => void, ms: number): void;
}

const browserClock: PaintClock = {
	frame: (cb) => void requestAnimationFrame(() => cb()),
	timeout: (cb, ms) => void setTimeout(cb, ms)
};

/** Resolves once what has been put in the DOM has had a frame to be painted in:
 *  two animation frames (the first runs before a paint, the second after it), or
 *  `timeoutMs`, whichever is first. */
export function afterPaint(clock: PaintClock = browserClock, timeoutMs: number = PAINT_TIMEOUT_MS): Promise<void> {
	return new Promise((resolve) => {
		let done = false;
		const finish = () => {
			if (done) return;
			done = true;
			resolve();
		};
		clock.frame(() => clock.frame(finish));
		clock.timeout(finish, timeoutMs);
	});
}

export interface RevealSteps {
	/** Let pending DOM updates land (Svelte's `tick`). */
	settle: () => Promise<unknown>;
	/** Wait for a frame to paint (`afterPaint`). */
	paint: () => Promise<void>;
	/** Ask the backend to show the window. */
	show: () => Promise<void>;
}

/** Settle, paint, show — in that order. Resolves `true` once the window was asked
 *  for, `false` if that failed; it never rejects, because there is nothing for a
 *  caller to do about it: the `invoke` wrapper has already logged a failed
 *  command, and the backend's timer shows the window regardless. A step *before*
 *  the show that fails does not stop it — a window shown a frame early beats
 *  one that waits out the backend's timer. */
export async function revealWindow({ settle, paint, show }: RevealSteps): Promise<boolean> {
	try {
		await settle();
		await paint();
	} catch {
		// Show it anyway.
	}
	try {
		await show();
		return true;
	} catch {
		return false;
	}
}
