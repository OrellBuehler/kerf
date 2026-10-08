/* The windows the editor lives in (Svelte 5 runes): its own, and one per panel the user
   has detached.

   A detached panel is dockview's popout: the panel's DOM is moved into a second window
   while every line of its script keeps running in the editor window — one JavaScript
   realm, one set of singletons, nothing to synchronise. The price is that "the window"
   is no longer one thing. `window`, `document` and a `<svelte:window>` are the editor
   window's, and an event that happens in a detached window — a pointer move, a key, a
   resize — is dispatched on *that* window and never reaches a listener on this one.
   So code that listens to a window, measures a screen or draws a frame asks which
   window it is in (`realm.ts`), and this registry is how it hears about the others.

   Plain state, no dockview: `popout.svelte.ts` is what opens and closes the windows
   and tells this about them. */

/** A frame that was asked for and has not run. Owned by `Windows`, which moves it to another
 *  window when the one it waits in stops showing — callers only hand it back to
 *  `cancelFrame`. */
export interface FrameHandle {
	/** The window the frame is requested in at the moment; null once it ran or was cancelled. */
	win: Window | null;
	id: number;
	/** Whether the callback is still owed. */
	live: boolean;
	/** Watchdog ticks it has waited through without running. */
	waits: number;
	cb: FrameRequestCallback;
}

/** How often the watchdog looks at frames that have not run. Short enough that a picture
 *  does not stay frozen for a noticeable time after a window stopped showing, long enough
 *  that a loop running at screen rate never meets it. */
const WATCHDOG_MS = 250;

interface Listener {
	type: string;
	handler: EventListener;
	options?: AddEventListenerOptions | boolean;
}

/** Whether a window can be read and is showing — a closed one, or one whose document
 *  throws (a window mid-teardown), is not. */
function showing(win: Window): boolean {
	try {
		return !win.closed && win.document.visibilityState === 'visible';
	} catch {
		return false;
	}
}

export class Windows {
	/** Bumps whenever the set of windows changes or a panel moves between them. An
	 *  effect that reads it runs again and finds the window its element is in now. */
	version = $state(0);
	readonly #popups = new Set<Window>();
	readonly #listeners = new Set<Listener>();
	readonly #main: Window;
	/** Frames asked for and not yet run. */
	readonly #frames = new Set<FrameHandle>();
	/** What stops each window's visibility listener. */
	readonly #visibility = new Map<Window, () => void>();
	#watch: ReturnType<typeof setInterval> | null = null;
	readonly #watchdogMs: number;

	/** `main` is the editor window; a test stands a fake in and a short watchdog. */
	constructor(main: Window = globalThis as unknown as Window, watchdogMs = WATCHDOG_MS) {
		this.#main = main;
		this.#watchdogMs = watchdogMs;
		this.#watchVisibility(main);
	}

	/** The detached windows, oldest first. */
	get popups(): Window[] {
		return [...this.#popups];
	}

	/** Every window: the editor's own first. */
	get all(): Window[] {
		return [this.#main, ...this.#popups];
	}

	/** A detached window opened. Everything registered with `listen` starts hearing it. */
	add(win: Window): void {
		if (this.#popups.has(win)) return;
		this.#popups.add(win);
		for (const l of this.#listeners) this.#attach(win, l);
		this.#watchVisibility(win);
		this.version++;
		// A window that shows now is a better place for a frame than one that does not.
		this.#kickStale();
	}

	/** A detached window closed. A frame that was waiting in it will never run there. */
	remove(win: Window): void {
		if (!this.#popups.delete(win)) return;
		for (const l of this.#listeners) this.#detach(win, l);
		this.#visibility.get(win)?.();
		this.#visibility.delete(win);
		this.version++;
		this.#kickStale();
	}

	/** A panel moved between windows (or into one): whatever resolved its window
	 *  resolves it again. */
	touch(): void {
		this.version++;
	}

	/** Listen to `type` on **every** window — the editor's and each detached one, now
	 *  and later. For what is the app's rather than a panel's: the shortcut handler,
	 *  the click that dismisses a menu. Returns what stops it. */
	listen<K extends keyof WindowEventMap>(
		type: K,
		handler: (e: WindowEventMap[K]) => void,
		options?: AddEventListenerOptions | boolean
	): () => void {
		const l: Listener = { type, handler: handler as EventListener, options };
		this.#listeners.add(l);
		for (const win of this.all) this.#attach(win, l);
		return () => {
			this.#listeners.delete(l);
			for (const win of this.all) this.#detach(win, l);
		};
	}

	#attach(win: Window, l: Listener) {
		try {
			win.addEventListener(l.type, l.handler, l.options);
		} catch {
			// A window that is already gone has nothing left to hear.
		}
	}

	#detach(win: Window, l: Listener) {
		try {
			win.removeEventListener(l.type, l.handler, l.options);
		} catch {
			// as above
		}
	}

	/** The window to draw frames in: the editor's if it is showing, else the first
	 *  detached one that is. A window that is hidden — minimized, on another desktop,
	 *  covered — pauses its animation frames, and the transport clock lives in the
	 *  editor window while the picture may be on the other screen; so it runs on
	 *  whichever one is being looked at, and on the editor's when none is. */
	frameWindow(): Window {
		return this.all.find(showing) ?? this.#main;
	}

	/** `requestAnimationFrame` in `frameWindow()`. The timestamp the callback is given
	 *  counts from *that* window's start, so a caller that moves between windows reads
	 *  its own clock (`performance.now()`) instead.
	 *
	 *  The choice is not final. A frame already waiting in a window that then stops
	 *  showing (the editor minimized while the preview plays on the other screen) or
	 *  closes would never run there, and a loop that asks for its next frame from inside
	 *  the last one would stop for good — so the frame follows the windows: it is asked
	 *  for again where one is showing when a window's visibility changes, when a window
	 *  is added or removed, and, as a backstop for a platform that reports none of it,
	 *  when a short watchdog finds it has not run. The callback runs once. */
	requestFrame(cb: FrameRequestCallback): FrameHandle {
		const handle: FrameHandle = { win: null, id: 0, live: true, waits: 0, cb };
		this.#frames.add(handle);
		this.#arm(handle);
		this.#ensureWatchdog();
		return handle;
	}

	cancelFrame(handle: FrameHandle | null): void {
		if (!handle) return;
		handle.live = false;
		this.#frames.delete(handle);
		this.#disarm(handle);
	}

	/** Ask `handle`'s window for its frame. */
	#arm(handle: FrameHandle): void {
		const win = this.frameWindow();
		handle.win = win;
		handle.waits = 0;
		try {
			handle.id = win.requestAnimationFrame((time) => this.#run(handle, win, time));
		} catch {
			// A window that cannot be asked is found out by the watchdog.
			handle.id = -1;
		}
	}

	#disarm(handle: FrameHandle): void {
		const win = handle.win;
		handle.win = null;
		try {
			win?.cancelAnimationFrame(handle.id);
		} catch {
			// the window went with the frame
		}
	}

	#run(handle: FrameHandle, win: Window, time: number): void {
		// A frame the handle was moved away from may still fire where it was.
		if (!handle.live || handle.win !== win) return;
		handle.live = false;
		handle.win = null;
		this.#frames.delete(handle);
		handle.cb(time);
	}

	/** Move every frame waiting in a window that is not where frames are drawn now to
	 *  the window that is. */
	#kickStale(): void {
		for (const handle of [...this.#frames]) {
			if (!handle.live || handle.win === this.frameWindow()) continue;
			this.#disarm(handle);
			this.#arm(handle);
		}
	}

	#watchVisibility(win: Window): void {
		const onChange = () => this.#kickStale();
		try {
			win.document.addEventListener('visibilitychange', onChange);
		} catch {
			return;
		}
		this.#visibility.set(win, () => {
			try {
				win.document.removeEventListener('visibilitychange', onChange);
			} catch {
				// the document went first
			}
		});
	}

	#ensureWatchdog(): void {
		if (this.#watch === null) this.#watch = setInterval(() => this.#tick(), this.#watchdogMs);
	}

	/** A frame that has waited through two ticks (a quarter second at least) without
	 *  running is asked for again where one should run now. If that is the window it is
	 *  already in, it keeps waiting: when nothing shows there is nowhere better. */
	#tick(): void {
		if (this.#frames.size === 0) {
			if (this.#watch !== null) clearInterval(this.#watch);
			this.#watch = null;
			return;
		}
		for (const handle of [...this.#frames]) {
			if (!handle.live) continue;
			if (++handle.waits < 2) continue;
			if (handle.win !== this.frameWindow()) {
				this.#disarm(handle);
				this.#arm(handle);
			} else {
				handle.waits = 0;
			}
		}
	}
}

export const windows = new Windows();
