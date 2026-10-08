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

export interface FrameHandle {
	win: Window;
	id: number;
}

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

	/** `main` is the editor window; a test stands a fake in. */
	constructor(main: Window = globalThis as unknown as Window) {
		this.#main = main;
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
		this.version++;
	}

	/** A detached window closed. */
	remove(win: Window): void {
		if (!this.#popups.delete(win)) return;
		for (const l of this.#listeners) this.#detach(win, l);
		this.version++;
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
	 *  its own clock (`performance.now()`) instead. */
	requestFrame(cb: FrameRequestCallback): FrameHandle {
		const win = this.frameWindow();
		return { win, id: win.requestAnimationFrame(cb) };
	}

	cancelFrame(handle: FrameHandle | null): void {
		if (!handle) return;
		try {
			handle.win.cancelAnimationFrame(handle.id);
		} catch {
			// the window went with the frame
		}
	}
}

export const windows = new Windows();
