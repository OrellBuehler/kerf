/* Detached panels (Svelte 5 runes): the panels the user moved into windows of their
   own, for a second screen.

   This is dockview's popout. `addPopoutGroup` opens a window with `window.open` and
   moves the panel's DOM into it; the panel keeps running in the editor window's
   JavaScript realm, so the `editor` / `ui` singletons, the transport clock, the Web
   Audio engine and every Tauri channel are the same objects in every window and
   nothing is synchronised. In the desktop app the window is made by `popout.rs`, which
   only answers a `window.open` the page announced first — so every open here goes
   through `#open`, which announces (`popoutExpect`) and hands the label it gets back
   to the window that opens.

   What this adds to dockview: opening and closing on the user's terms (`popOut`,
   `dockBack`, the Window menu and the tab menu), what a window needs that dockview
   does not give it (the live theme on its `<html>`, a title, the shortcut handler's
   keys — `windows.listen` — and `inert` behind a modal), and the bookkeeping that
   pairs an announced window with the window that opened. Which window a panel is in,
   and how its code finds out, is `windows.svelte.ts` and `realm.ts`. */

import type { DockviewApi, PopoutGroup, PopoutWindowFailure } from 'dockview';
import { POPOUT_URL, isPanelId, type PanelId } from './layout';
import { windows } from './windows.svelte';
import {
	LabelBook,
	describeFailure,
	detachBlocked,
	detachSize,
	mirrorRoot,
	positionCorrection,
	windowTitle,
	type PanelLocation
} from './popouts';
import {
	closePopout,
	onPopoutClosed,
	popoutCancel,
	popoutExpect,
	popoutFocus,
	popoutMove,
	type PopoutAnnounced,
	type PopoutRequest
} from './api';
import { installSliderFill } from './slider-fill';
import { toast } from './notifications.svelte';

/** How long a window destroyed from outside may stay listed before it is told to close. */
const CLOSE_BACKSTOP_MS = 600;
/** How long after a window opens the platform has had to put it where it is going. */
const PLACE_SETTLE_MS = 300;

/** What the page adds to each detached window beyond what this does itself. */
export interface PopoutHooks {
	/** A window opened: put whatever the page needs in it (the context menu's own
	 *  instance, which cannot be drawn from another window's document). Returns what
	 *  takes it out again. */
	window?: (win: Window) => () => void;
}

interface Entry {
	win: Window;
	/** The label the backend gave it: how `close_popout` and `popout_focus` name it. */
	label: string | null;
	stops: Array<() => void>;
}

/** The theme's app surface as `#rrggbb`, the colour a window is painted until its
 *  panel is in it; null when the theme gives something else. */
function surfaceColor(): string | null {
	const value = getComputedStyle(document.documentElement).getPropertyValue('--surface-app').trim();
	return /^#[0-9a-f]{6}$/i.test(value) ? value : null;
}

class PopoutState {
	/** The panels in a window of their own, in dockview's order. */
	detached = $state<PanelId[]>([]);
	/** How many windows. */
	count = $state(0);
	#api: DockviewApi | null = null;
	readonly #entries = new Map<Window, Entry>();
	readonly #labels = new LabelBook();
	/** Where each announced window will open, by label, for the correction after it did. */
	readonly #wanted = new Map<string, [number, number]>();
	#subs: Array<{ dispose(): void }> = [];
	#stops: Array<() => void> = [];
	/** Opens go one at a time: `window.open` cannot say which announcement it is for, so
	 *  the backend hands them out in order and nothing else may interleave. */
	#chain: Promise<unknown> = Promise.resolve();
	#inert = false;
	#signature = '';
	#hooks: PopoutHooks = {};

	/** Take over a dock: hear its windows open and close. */
	attach(api: DockviewApi, hooks: PopoutHooks = {}): void {
		this.detach();
		this.#api = api;
		this.#hooks = hooks;
		this.#subs.push(
			api.onDidAddPopoutGroup((p) => this.#opened(p)),
			api.onDidRemovePopoutGroup(() => this.#reconcile()),
			api.onWillClosePopoutWindow((e) => this.#willClose(e.window)),
			api.onDidOpenPopoutWindowFail((e) => this.#failed(e)),
			api.onDidMovePanel(() => this.#sync()),
			api.onDidAddPanel(() => this.#sync()),
			api.onDidRemovePanel(() => this.#sync())
		);
		this.#stops.push(this.#watchTheme());
		void onPopoutClosed((label) => this.#destroyed(label)).then((stop) => {
			if (this.#api === api) this.#stops.push(stop);
			else stop();
		});
		for (const p of api.getPopouts()) this.#opened(p);
		this.#sync();
	}

	detach(): void {
		for (const s of this.#subs.splice(0)) s.dispose();
		for (const s of this.#stops.splice(0)) s();
		for (const entry of this.#entries.values()) {
			// The dock is going: a window dockview can no longer close by itself is closed here.
			if (entry.label) void closePopout(entry.label).catch(() => {});
			this.#release(entry);
		}
		this.#entries.clear();
		this.#labels.clear();
		this.#wanted.clear();
		this.#api = null;
		this.detached = [];
		this.count = 0;
		this.#signature = '';
	}

	isDetached(id: PanelId): boolean {
		return this.detached.includes(id);
	}

	/** Where each open panel is: the editor window's grid, or a window of its own. */
	where(): Partial<Record<PanelId, PanelLocation>> {
		const out: Partial<Record<PanelId, PanelLocation>> = {};
		for (const p of this.#api?.panels ?? []) {
			if (isPanelId(p.id)) out[p.id] = p.group.api.location.type;
		}
		return out;
	}

	/** Why `id` cannot be detached, or null. */
	blocked(id: PanelId): string | null {
		return detachBlocked(id, this.where());
	}

	// ---- opening and closing ------------------------------------------------------

	/** Move a panel into a window of its own — or, if it is in one, bring that to the
	 *  front. Whether it opened. */
	async popOut(id: PanelId): Promise<boolean> {
		const api = this.#api;
		const panel = api?.getPanel(id);
		if (!api || !panel) return false;
		if (panel.group.api.location.type === 'popout') {
			this.reveal(id);
			return true;
		}
		const reason = this.blocked(id);
		if (reason) {
			toast.info(reason);
			return false;
		}
		const at = panel.group.element.getBoundingClientRect();
		const size = detachSize(id, at.width, at.height);
		return this.#open({ size }, () => api.addPopoutGroup(panel, { popoutUrl: POPOUT_URL }));
	}

	/** Give a panel back to the editor window: close its window, and dockview puts the
	 *  panels in it where they came from. */
	dockBack(id: PanelId): void {
		const entry = this.#entryOf(id);
		if (entry) this.#close(entry);
	}

	/** Every detached panel back. */
	dockAll(): void {
		for (const entry of [...this.#entries.values()]) this.#close(entry);
	}

	/** Detach a panel, or give it back if it is detached. */
	toggle(id: PanelId): void {
		if (this.isDetached(id)) this.dockBack(id);
		else void this.popOut(id);
	}

	/** Raise the window a panel is in. */
	reveal(id: PanelId): void {
		const entry = this.#entryOf(id);
		if (!entry) return;
		try {
			entry.win.focus();
		} catch {
			// the system raises it below
		}
		if (entry.label) void popoutFocus(entry.label).catch(() => {});
	}

	/** A modal is on screen over the editor window (or gone): what is in a detached window
	 *  is as out of reach as what is under it. */
	setInert(on: boolean): void {
		this.#inert = on;
		for (const entry of this.#entries.values()) this.#decorate(entry);
	}

	// ---- restoring a layout ---------------------------------------------------------

	/** Announce the windows a layout is about to open, in the order dockview restores them
	 *  (`fromJSON` opens each from a timer, with nothing to say which window it is for).
	 *  Call before `fromJSON` and `settle` once `popoutRestorationPromise` is over. */
	async announce(boxes: ReadonlyArray<{ left: number; top: number; width: number; height: number } | null>): Promise<void> {
		for (const box of boxes) {
			const rect = box ? { x: box.left, y: box.top, width: box.width, height: box.height } : null;
			try {
				this.#note(await popoutExpect({ rect, background: surfaceColor() }));
			} catch (e) {
				console.error('could not announce a detached window', e);
			}
		}
	}

	/** The restore is over: an announcement no window took is forgotten. */
	settle(): void {
		for (const label of this.#labels.clear()) {
			this.#wanted.delete(label);
			void popoutCancel(label).catch(() => {});
		}
	}

	// ---- internals ---------------------------------------------------------------------

	#open(request: PopoutRequest, run: () => Promise<boolean>): Promise<boolean> {
		const next = this.#chain.then(async () => {
			let label: string | null = null;
			try {
				const announced = await popoutExpect({ ...request, background: surfaceColor() });
				label = announced?.label ?? null;
				this.#note(announced);
			} catch (e) {
				toast.error('Could not open a window for the panel', { description: e instanceof Error ? e.message : String(e) });
				return false;
			}
			let ok = false;
			try {
				ok = await run();
			} catch (e) {
				console.error('could not open a detached window', e);
			}
			if (!ok && label) {
				this.#labels.drop(label);
				this.#wanted.delete(label);
				void popoutCancel(label).catch(() => {});
			}
			return ok;
		});
		this.#chain = next.catch(() => {});
		return next;
	}

	/** A window was announced: its label waits for the window that opens, and so does where it
	 *  is going. */
	#note(announced: PopoutAnnounced | null): void {
		if (!announced) return;
		this.#labels.expect(announced.label);
		if (announced.position) this.#wanted.set(announced.label, announced.position);
	}

	/** The window opened; once the platform has put it somewhere, move it by the error if it
	 *  is not where it was asked (`positionCorrection`). */
	#placed(entry: Entry): void {
		const label = entry.label;
		const want = label ? this.#wanted.get(label) : undefined;
		if (!label || !want) return;
		this.#wanted.delete(label);
		setTimeout(() => {
			if (!this.#entries.has(entry.win)) return;
			const fix = positionCorrection(want, [entry.win.screenX, entry.win.screenY]);
			if (fix) void popoutMove(label, fix[0], fix[1]).catch(() => {});
		}, PLACE_SETTLE_MS);
	}

	#entryOf(id: PanelId): Entry | undefined {
		const panel = this.#api?.getPanel(id);
		const location = panel?.group.api.location;
		if (location?.type !== 'popout') return undefined;
		return this.#entries.get(location.getWindow());
	}

	#close(entry: Entry): void {
		try {
			entry.win.close();
		} catch {
			// The platform may not honour it (WKWebView); the backend does.
		}
		if (entry.label) void closePopout(entry.label).catch(() => {});
	}

	#opened(p: PopoutGroup): void {
		const win = p.window;
		if (this.#entries.has(win)) return;
		const entry: Entry = { win, label: this.#labels.claim(), stops: [] };
		this.#entries.set(win, entry);
		windows.add(win);
		try {
			entry.stops.push(installSliderFill(win.document));
		} catch (e) {
			console.error('slider fill in a detached window', e);
		}
		if (this.#hooks.window) {
			try {
				entry.stops.push(this.#hooks.window(win));
			} catch (e) {
				console.error('could not set up a detached window', e);
			}
		}
		this.#decorate(entry);
		this.#placed(entry);
		this.#sync();
	}

	/** A window went. dockview says which group, and by then has let go of the window
	 *  (`PopoutGroup.window` is null once its window closed), so the ones to forget are
	 *  those no longer among the windows it lists. */
	#reconcile(): void {
		const api = this.#api;
		if (!api) return;
		const live = new Set<Window>(api.getPopouts().map((p) => p.window));
		for (const [win, entry] of [...this.#entries]) {
			if (live.has(win)) continue;
			this.#entries.delete(win);
			this.#release(entry);
		}
		this.#sync();
	}

	#release(entry: Entry): void {
		for (const stop of entry.stops.splice(0)) {
			try {
				stop();
			} catch {
				// the document went first
			}
		}
		windows.remove(entry.win);
	}

	/** dockview is closing a window, for whatever reason. A webview that cannot close
	 *  itself (WKWebView) is destroyed by name. */
	#willClose(win: Window): void {
		const label = this.#entries.get(win)?.label;
		if (label) void closePopout(label).catch(() => {});
	}

	/** The backend says a window is gone. dockview learns that from the window's `closed`
	 *  flag within a quarter second; if a platform never sets it, the panel would stay
	 *  in a window that does not exist, so after a moment the window is told it is
	 *  unloading — which is what dockview listens for. */
	#destroyed(label: string): void {
		const entry = [...this.#entries.values()].find((e) => e.label === label);
		if (!entry) return;
		setTimeout(() => {
			if (!this.#entries.has(entry.win)) return;
			console.warn('a detached window is gone but still listed; closing it', label);
			try {
				entry.win.dispatchEvent(new Event('beforeunload'));
			} catch {
				// nothing left to tell
			}
		}, CLOSE_BACKSTOP_MS);
	}

	#failed(e: PopoutWindowFailure): void {
		console.warn('a detached window did not open', e);
		toast.error(describeFailure(e.reason), { description: e.error?.message });
	}

	/** What a window needs that dockview does not give it. */
	#decorate(entry: Entry): void {
		const doc = entry.win.document;
		try {
			mirrorRoot(document.documentElement, doc.documentElement);
			doc.body.toggleAttribute('inert', this.#inert);
		} catch {
			// a window that is going away
		}
	}

	#watchTheme(): () => void {
		const observer = new MutationObserver(() => {
			for (const entry of this.#entries.values()) this.#decorate(entry);
		});
		observer.observe(document.documentElement, { attributes: true, attributeFilter: ['style', 'class', 'lang'] });
		return () => observer.disconnect();
	}

	/** Recount what is detached, retitle the windows, and tell whoever resolved a window
	 *  for an element that it may have moved. */
	#sync(): void {
		const api = this.#api;
		if (!api) return;
		const inWindow = new Map<Window, PanelId[]>();
		const ids: PanelId[] = [];
		for (const p of api.panels) {
			const at = p.group.api.location;
			if (at.type !== 'popout' || !isPanelId(p.id)) continue;
			ids.push(p.id);
			const win = at.getWindow();
			inWindow.set(win, [...(inWindow.get(win) ?? []), p.id]);
		}
		for (const [win, panels] of inWindow) {
			try {
				win.document.title = windowTitle(panels);
			} catch {
				// as above
			}
		}
		this.detached = ids;
		this.count = this.#entries.size;
		const signature = `${[...inWindow.entries()].map(([, v]) => v.join('+')).join('|')}#${this.#entries.size}`;
		if (signature !== this.#signature) {
			this.#signature = signature;
			windows.touch();
		}
	}
}

export const popout = new PopoutState();
