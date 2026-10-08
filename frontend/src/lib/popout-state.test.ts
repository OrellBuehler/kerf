import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, jest, mock, spyOn, test } from 'bun:test';
import './test-runes';

// The runes singleton that drives dockview's popouts, against a stand-in dock and stand-in
// windows: what is checked is the bookkeeping — which announced label a window gets, what
// the editor does to a window that opened, how a window that went is forgotten — not
// dockview or the desktop shell.

const toasts: Array<[string, string]> = [];
mock.module('./notifications.svelte', () => ({
	toast: Object.assign((m: string) => toasts.push(['note', m]), {
		success: (m: string) => toasts.push(['success', m]),
		info: (m: string) => toasts.push(['info', m]),
		error: (m: string) => toasts.push(['error', m]),
		warning: (m: string) => toasts.push(['warning', m]),
		dismiss: () => {}
	})
}));
const sliderFills: Array<{ doc: unknown; stopped: boolean }> = [];
mock.module('./slider-fill', () => ({
	installSliderFill: (doc: unknown) => {
		const rec = { doc, stopped: false };
		sliderFills.push(rec);
		return () => (rec.stopped = true);
	}
}));

const calls: Array<[string, ...unknown[]]> = [];
let announce: (n: number) => { label: string; position: [number, number] | null } | Error | null = (n) => ({ label: `popout-${n}`, position: null });
let announced = 0;
let closedHandler: ((label: string) => void) | null = null;
const realApi = { ...(await import('./api')) };
beforeAll(() => {
	mock.module('./api', () => ({
		...realApi,
		popoutExpect: async (request: unknown) => {
			calls.push(['expect', request]);
			const r = announce(announced++);
			if (r instanceof Error) throw r;
			return r;
		},
		popoutCancel: async (label: string) => void calls.push(['cancel', label]),
		closePopout: async (label: string) => void calls.push(['close', label]),
		popoutFocus: async (label: string) => void calls.push(['focus', label]),
		popoutMove: async (label: string, x: number, y: number) => void calls.push(['move', label, x, y]),
		onPopoutClosed: async (cb: (label: string) => void) => {
			closedHandler = cb;
			return () => {};
		}
	}));
});
afterAll(() => {
	mock.module('./api', () => realApi);
	jest.useRealTimers();
	for (const k of ['document', 'getComputedStyle', 'MutationObserver']) delete (globalThis as Record<string, unknown>)[k];
});

// ---- stand-ins ----------------------------------------------------------------

const mainRoot = { className: 'dark', lang: 'en', style: { cssText: '--surface-app: #0f1318;' } };
class FakeObserver {
	static live = 0;
	observe() {
		FakeObserver.live++;
	}
	disconnect() {
		FakeObserver.live--;
	}
}

function fakeWindow(x = 100, y = 100) {
	const doc = {
		title: '',
		visibilityState: 'visible',
		documentElement: { className: '', lang: '', style: { cssText: '' } },
		body: {
			inert: false,
			toggleAttribute(name: string, on: boolean) {
				if (name === 'inert') this.inert = on;
			}
		}
	};
	const win = {
		document: doc,
		screenX: x,
		screenY: y,
		closed: false,
		focused: 0,
		closeCalls: 0,
		listeners: [] as Array<[string, unknown]>,
		addEventListener(type: string, h: unknown) {
			this.listeners.push([type, h]);
		},
		removeEventListener(type: string, h: unknown) {
			const i = this.listeners.findIndex((l) => l[0] === type && l[1] === h);
			if (i >= 0) this.listeners.splice(i, 1);
		},
		dispatchEvent() {
			return true;
		},
		focus() {
			this.focused++;
		},
		close() {
			this.closeCalls++;
			this.closed = true;
		}
	};
	return win as typeof win & Window;
}

type Handler<T> = (e: T) => void;
function fakeDock() {
	const subs = { add: [] as Handler<unknown>[], remove: [] as Handler<unknown>[], will: [] as Handler<unknown>[], fail: [] as Handler<unknown>[], move: [] as Handler<unknown>[], panelAdd: [] as Handler<unknown>[], panelRemove: [] as Handler<unknown>[] };
	const on = (list: Handler<unknown>[]) => (h: Handler<unknown>) => {
		list.push(h);
		return { dispose: () => list.splice(list.indexOf(h), 1) };
	};
	const popouts: Array<{ id: string; group: unknown; window: Window }> = [];
	const panels: Array<{ id: string; group: { api: { location: { type: string; getWindow?: () => Window } }; element: { getBoundingClientRect(): { width: number; height: number } } } }> = [];
	const grid = (id: string) => ({ id, group: { api: { location: { type: 'grid' } }, element: { getBoundingClientRect: () => ({ width: 640, height: 480 }) } } });
	for (const id of ['library', 'preview', 'timeline']) panels.push(grid(id));
	const dock = {
		panels,
		popouts,
		subs,
		openWith: undefined as undefined | ((panel: (typeof panels)[number]) => Window | null),
		opened: 0,
		onDidAddPopoutGroup: on(subs.add),
		onDidRemovePopoutGroup: on(subs.remove),
		onWillClosePopoutWindow: on(subs.will),
		onDidOpenPopoutWindowFail: on(subs.fail),
		onDidMovePanel: on(subs.move),
		onDidAddPanel: on(subs.panelAdd),
		onDidRemovePanel: on(subs.panelRemove),
		getPopouts: () => popouts,
		getPanel: (id: string) => panels.find((p) => p.id === id),
		/** dockview opening a window: `window.open`, then the group moves in and the event fires. */
		async addPopoutGroup(panel: (typeof panels)[number]) {
			dock.opened++;
			const win = dock.openWith ? dock.openWith(panel) : fakeWindow();
			if (!win) return false;
			panel.group.api.location = { type: 'popout', getWindow: () => win };
			const p = { id: `g-${panel.id}`, group: panel.group, window: win };
			popouts.push(p);
			subs.add.forEach((h) => h(p));
			return true;
		},
		/** The window went: dockview lists no window for it any more and says so. */
		close(win: Window) {
			const i = popouts.findIndex((p) => p.window === win);
			if (i < 0) return;
			const [p] = popouts.splice(i, 1);
			for (const panel of panels) if (panel.group === p.group) panel.group.api.location = { type: 'grid' };
			subs.remove.forEach((h) => h({ id: p.id, group: p.group, window: null }));
		}
	};
	return dock;
}

async function boot() {
	const { popout } = await import('./popout.svelte');
	const { windows } = await import('./windows.svelte');
	const dock = fakeDock();
	popout.detach();
	// What the previous test left, and its close-up, is not this test's.
	calls.length = 0;
	FakeObserver.live = 0;
	closedHandler = null;
	popout.attach(dock as never);
	await Promise.resolve();
	return { popout, windows, dock };
}

beforeEach(() => {
	jest.useFakeTimers();
	calls.length = 0;
	toasts.length = 0;
	sliderFills.length = 0;
	announced = 0;
	announce = (n) => ({ label: `popout-${n}`, position: null });
	FakeObserver.live = 0;
	const g = globalThis as Record<string, unknown>;
	g.document = { documentElement: mainRoot };
	g.getComputedStyle = () => ({ getPropertyValue: () => ' #0f1318 ' });
	g.MutationObserver = FakeObserver;
});
afterEach(() => {
	jest.useRealTimers();
});

// ---- tests ---------------------------------------------------------------------

describe('detaching a panel', () => {
	test('announces a window of the panel\'s own size, then opens it, and the window takes the label it was announced with', async () => {
		const { popout, windows, dock } = await boot();
		expect(await popout.popOut('library')).toBe(true);
		const expects = calls.filter((c) => c[0] === 'expect');
		expect(expects).toHaveLength(1);
		expect((expects[0][1] as { size: unknown }).size).toEqual({ width: 640, height: 480 });
		expect((expects[0][1] as { background: unknown }).background).toBe('#0f1318');
		// It was registered, with the label, among the windows the editor listens to.
		const win = dock.popouts[0].window;
		expect(windows.popups).toEqual([win]);
		expect(popout.detached).toEqual(['library']);
		expect(popout.count).toBe(1);
		popout.dockBack('library');
		expect(calls).toContainEqual(['close', 'popout-0']);
	});

	test('a window is given the live theme, a title, a slider fill of its own, and inert follows a modal', async () => {
		const { popout, dock } = await boot();
		await popout.popOut('timeline');
		const win = dock.popouts[0].window as unknown as ReturnType<typeof fakeWindow>;
		expect(win.document.documentElement.className).toBe('dark');
		expect(win.document.documentElement.style.cssText).toBe('--surface-app: #0f1318;');
		expect(win.document.title).toBe('Timeline — Kerf');
		expect(sliderFills).toHaveLength(1);
		expect(sliderFills[0].doc).toBe(win.document);
		expect(win.document.body.inert).toBe(false);
		popout.setInert(true);
		expect(win.document.body.inert).toBe(true);
		popout.setInert(false);
		expect(win.document.body.inert).toBe(false);
		// A window that opens during a modal is out of reach from the start.
		popout.setInert(true);
		await popout.popOut('preview');
		expect((dock.popouts[1].window as unknown as ReturnType<typeof fakeWindow>).document.body.inert).toBe(true);
	});

	test('the editor window keeps a panel of its own', async () => {
		const { popout, dock } = await boot();
		dock.panels.splice(1);
		expect(await popout.popOut('library')).toBe(false);
		expect(dock.opened).toBe(0);
		expect(toasts[0][0]).toBe('info');
		expect(calls).toEqual([]);
	});

	test('a window dockview could not open is cancelled, so it does not take the next one\'s label', async () => {
		const { popout, dock } = await boot();
		dock.openWith = () => null;
		expect(await popout.popOut('library')).toBe(false);
		expect(calls).toContainEqual(['cancel', 'popout-0']);
		dock.openWith = undefined;
		await popout.popOut('preview');
		await popout.popOut('timeline');
		// The two that opened got their own labels, in order.
		popout.dockBack('preview');
		popout.dockBack('timeline');
		expect(calls.filter((c) => c[0] === 'close')).toEqual([
			['close', 'popout-1'],
			['close', 'popout-2']
		]);
	});

	test('a backend that will not announce a window says so, and nothing opens', async () => {
		const { popout, dock } = await boot();
		announce = () => new Error('too many popout windows are waiting to open');
		expect(await popout.popOut('library')).toBe(false);
		expect(dock.opened).toBe(0);
		expect(toasts.some(([kind]) => kind === 'error')).toBe(true);
	});

	test('opens go one at a time, in the order they were asked', async () => {
		const { popout, dock } = await boot();
		const results = await Promise.all([popout.popOut('library'), popout.popOut('preview'), popout.popOut('timeline')]);
		expect(results).toEqual([true, true, true]);
		expect(dock.popouts.map((p) => p.group)).toHaveLength(3);
		expect(calls.filter((c) => c[0] === 'expect')).toHaveLength(3);
		popout.dockAll();
		expect(calls.filter((c) => c[0] === 'close').map((c) => c[1])).toEqual(['popout-0', 'popout-1', 'popout-2']);
	});
});

describe('a window that went', () => {
	test('is forgotten, though dockview no longer says which window it was', async () => {
		const { popout, windows, dock } = await boot();
		await popout.popOut('library');
		await popout.popOut('preview');
		const [a, b] = dock.popouts.map((p) => p.window);
		expect(windows.popups).toEqual([a, b]);
		const version = windows.version;
		dock.close(a);
		expect(windows.popups).toEqual([b]);
		expect(windows.version).toBeGreaterThan(version);
		expect(popout.count).toBe(1);
		expect(popout.detached).toEqual(['preview']);
		// Its slider-fill observer went with it.
		expect(sliderFills[0].stopped).toBe(true);
		expect(sliderFills[1].stopped).toBe(false);
	});

	test('dockview closing one tells the shell, which destroys it by name (WKWebView cannot close itself)', async () => {
		const { popout, dock } = await boot();
		await popout.popOut('library');
		const win = dock.popouts[0].window;
		dock.subs.will.forEach((h) => h({ id: 'g', window: win }));
		expect(calls).toContainEqual(['close', 'popout-0']);
	});

	test('a window the shell destroyed that dockview still lists is told it is unloading, after a moment', async () => {
		const warn = spyOn(console, 'warn').mockImplementation(() => {});
		const { popout, dock } = await boot();
		await popout.popOut('library');
		const win = dock.popouts[0].window as unknown as ReturnType<typeof fakeWindow>;
		let unloading = 0;
		win.dispatchEvent = () => {
			unloading++;
			return true;
		};
		closedHandler!('popout-0');
		expect(unloading).toBe(0);
		jest.advanceTimersByTime(700);
		expect(unloading).toBe(1);
		// A window dockview did let go of in the meantime is left alone.
		await popout.popOut('preview');
		const other = dock.popouts[1].window as unknown as ReturnType<typeof fakeWindow>;
		let again = 0;
		other.dispatchEvent = () => {
			again++;
			return true;
		};
		closedHandler!('popout-1');
		dock.close(other);
		jest.advanceTimersByTime(700);
		expect(again).toBe(0);
		// …and a label nobody has is ignored.
		closedHandler!('popout-99');
		jest.advanceTimersByTime(700);
		expect(warn).toHaveBeenCalledTimes(1);
		warn.mockRestore();
	});

	test('detaching the dock closes what is left, by name', async () => {
		const { popout } = await boot();
		await popout.popOut('library');
		popout.detach();
		expect(calls).toContainEqual(['close', 'popout-0']);
		expect(popout.detached).toEqual([]);
		expect(popout.count).toBe(0);
	});
});

describe('restoring a layout', () => {
	test('announces a window for each, in order, with where it should open, and forgets the ones nobody took', async () => {
		const { popout } = await boot();
		announce = (n) => ({ label: `popout-${n}`, position: n === 0 ? [300, 200] : null });
		await popout.announce([
			{ left: 300, top: 200, width: 500, height: 400 },
			null
		]);
		const asked = calls.filter((c) => c[0] === 'expect').map((c) => (c[1] as { rect: unknown }).rect);
		expect(asked).toEqual([{ x: 300, y: 200, width: 500, height: 400 }, null]);
		popout.settle();
		expect(calls.filter((c) => c[0] === 'cancel')).toEqual([
			['cancel', 'popout-0'],
			['cancel', 'popout-1']
		]);
	});

	test('a window that failed to open gives up its label, so the next one is not given it', async () => {
		const { popout, dock } = await boot();
		announce = (n) => ({ label: `popout-${n}`, position: [100 * (n + 1), 100] });
		await popout.announce([
			{ left: 100, top: 100, width: 400, height: 300 },
			{ left: 200, top: 100, width: 400, height: 300 },
			{ left: 300, top: 100, width: 400, height: 300 }
		]);
		// The first was refused after the backend took its label (the script was blocked).
		dock.subs.fail.forEach((h) => h({ reason: 'blocked', error: new Error('blocked') }));
		dock.openWith = () => fakeWindow(200, 100);
		await dock.addPopoutGroup(dock.panels[1]);
		dock.openWith = () => fakeWindow(300, 100);
		await dock.addPopoutGroup(dock.panels[2]);
		// What is left to cancel is nothing: every announcement was taken or spent.
		popout.settle();
		expect(calls.filter((c) => c[0] === 'cancel').map((c) => c[1])).toEqual(['popout-0']);
		// The two that opened close under their own labels.
		popout.dockBack('preview');
		popout.dockBack('timeline');
		expect(calls.filter((c) => c[0] === 'close')).toEqual([
			['close', 'popout-1'],
			['close', 'popout-2']
		]);
		// And the refused one's position is not applied to a window that has nothing to do with it.
		jest.advanceTimersByTime(400);
		expect(calls.filter((c) => c[0] === 'move')).toEqual([]);
	});

	test('a URL dockview refused never reached the backend, so its announcement still waits', async () => {
		const { popout, dock } = await boot();
		announce = (n) => ({ label: `popout-${n}`, position: null });
		await popout.announce([null, null]);
		dock.subs.fail.forEach((h) => h({ reason: 'url-refused' }));
		dock.openWith = () => fakeWindow();
		await dock.addPopoutGroup(dock.panels[0]);
		popout.dockBack('library');
		expect(calls.filter((c) => c[0] === 'close')).toEqual([['close', 'popout-0']]);
	});

	test('a window that opened where it was asked is left, and one the platform put a title bar off is moved by the error', async () => {
		const { popout, dock } = await boot();
		announce = (n) => ({ label: `popout-${n}`, position: [166, 811] });
		await popout.announce([{ left: 166, top: 811, width: 473, height: 447 }]);
		dock.openWith = () => fakeWindow(134, 779);
		await dock.addPopoutGroup(dock.panels[0]);
		popout.settle();
		jest.advanceTimersByTime(400);
		expect(calls.filter((c) => c[0] === 'move')).toEqual([['move', 'popout-0', 198, 843]]);

		calls.length = 0;
		announced = 0;
		announce = () => ({ label: 'popout-5', position: [500, 500] });
		await popout.announce([{ left: 500, top: 500, width: 400, height: 300 }]);
		dock.openWith = () => fakeWindow(501, 499);
		await dock.addPopoutGroup(dock.panels[1]);
		jest.advanceTimersByTime(400);
		expect(calls.filter((c) => c[0] === 'move')).toEqual([]);
	});

	test('a window that was closed before the platform settled is not moved', async () => {
		const { popout, dock } = await boot();
		announce = () => ({ label: 'popout-0', position: [166, 811] });
		await popout.announce([{ left: 166, top: 811, width: 473, height: 447 }]);
		dock.openWith = () => fakeWindow(134, 779);
		await dock.addPopoutGroup(dock.panels[0]);
		dock.close(dock.popouts[0].window);
		jest.advanceTimersByTime(400);
		expect(calls.filter((c) => c[0] === 'move')).toEqual([]);
	});
});

describe('the theme', () => {
	test('is watched while the dock is attached, and let go when it is not', async () => {
		const { popout } = await boot();
		expect(FakeObserver.live).toBe(1);
		popout.detach();
		expect(FakeObserver.live).toBe(0);
	});
});
