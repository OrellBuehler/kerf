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

/** The faces the first frame is drawn in (`@fontsource-variable/*`, bundled): the interface's sans and
 *  mono, and the display face the agent panel's title uses. A face is fetched the first time text
 *  needs it, so a window shown before that paints once in the system fallback and then re-flows. */
export const REVEAL_FONTS: readonly string[] = ['500 12px "Geist Variable"', '500 12px "Geist Mono Variable"', '600 12px "Space Grotesk Variable"'];

/** How long to give those faces. They are local files, so this is a safety net, not an expected wait:
 *  a face that cannot load must not keep the window hidden (the backend's own timer is 3 s). */
export const FONT_TIMEOUT_MS = 400;

/** The part of `document.fonts` the reveal uses. */
export interface FontLoader {
	load(font: string): Promise<unknown>;
}

/** Resolves once every face in `fonts` has loaded or failed, or after `timeoutMs`, whichever is first.
 *  Resolves at once where there is no font set to ask (a page without `document.fonts`), and never
 *  rejects: a face that fails to load is drawn in the fallback, which is what would happen anyway. */
export function fontsReady(
	loader: FontLoader | undefined = typeof document === 'undefined' ? undefined : document.fonts,
	clock: Pick<PaintClock, 'timeout'> = browserClock,
	fonts: readonly string[] = REVEAL_FONTS,
	timeoutMs: number = FONT_TIMEOUT_MS
): Promise<void> {
	if (!loader || typeof loader.load !== 'function') return Promise.resolve();
	return new Promise((resolve) => {
		let done = false;
		const finish = () => {
			if (done) return;
			done = true;
			resolve();
		};
		clock.timeout(finish, timeoutMs);
		try {
			void Promise.allSettled(fonts.map((f) => loader.load(f))).then(finish);
		} catch {
			finish();
		}
	});
}

export interface RevealSteps {
	/** Let pending DOM updates land (Svelte's `tick`). */
	settle: () => Promise<unknown>;
	/** Wait for the interface's fonts (`fontsReady`), so the first frame is not drawn in the fallback and
	 *  re-flowed. Optional: a caller with nothing to wait for leaves it out. */
	fonts?: () => Promise<void>;
	/** Wait for a frame to paint (`afterPaint`). */
	paint: () => Promise<void>;
	/** Ask the backend to show the window. */
	show: () => Promise<void>;
}

/** Settle, fonts, paint, show — in that order. Resolves `true` once the window was asked
 *  for, `false` if that failed; it never rejects, because there is nothing for a
 *  caller to do about it: the `invoke` wrapper has already logged a failed
 *  command, and the backend's timer shows the window regardless. A step *before*
 *  the show that fails does not stop it — a window shown a frame early beats
 *  one that waits out the backend's timer. */
export async function revealWindow({ settle, fonts, paint, show }: RevealSteps): Promise<boolean> {
	try {
		await settle();
		await fonts?.();
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
