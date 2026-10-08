import { describe, expect, test } from 'bun:test';
import './test-runes';

const { Windows } = await import('./windows.svelte');
const { parseEventKey, windowOf, documentOf, resizeObserverFor, intersectionObserverFor } = await import('./realm');
const { onWindow } = await import('./window-events');

/** A window as far as the registry can tell: it takes listeners, says what it shows and,
 *  like a real one, runs its animation frames only while it shows (`paint()`). */
function fakeWindow(name: string, visible = true) {
	const listeners: Array<[string, unknown, unknown]> = [];
	const rafs: number[] = [];
	const pending = new Map<number, FrameRequestCallback>();
	const changes = new Set<() => void>();
	let last = 0;
	const win = {
		name,
		closed: false,
		document: {
			visibilityState: visible ? 'visible' : 'hidden',
			addEventListener(type: string, handler: () => void) {
				if (type === 'visibilitychange') changes.add(handler);
			},
			removeEventListener(type: string, handler: () => void) {
				if (type === 'visibilitychange') changes.delete(handler);
			}
		},
		listeners,
		rafs,
		pending,
		changes,
		addEventListener(type: string, handler: unknown, options?: unknown) {
			listeners.push([type, handler, options]);
		},
		removeEventListener(type: string, handler: unknown, options?: unknown) {
			const i = listeners.findIndex((l) => l[0] === type && l[1] === handler && l[2] === options);
			if (i >= 0) listeners.splice(i, 1);
		},
		requestAnimationFrame: (cb: FrameRequestCallback) => {
			pending.set(++last, cb);
			rafs.push(last);
			return last;
		},
		cancelAnimationFrame: (id: number) => {
			pending.delete(id);
			rafs.push(-id);
		},
		/** The screen refreshes: a window that shows runs the frames it was asked for. */
		paint(time = 1) {
			if (win.closed || win.document.visibilityState !== 'visible') return;
			const due = [...pending];
			pending.clear();
			for (const [, cb] of due) cb(time);
		},
		/** The window is minimized, covered or brought back. */
		show(shown: boolean) {
			win.document.visibilityState = shown ? 'visible' : 'hidden';
			for (const h of [...changes]) h();
		}
	};
	return win as typeof win & Window;
}

describe('the windows registry', () => {
	test('the editor window is always first, then the detached ones in the order they opened', () => {
		const main = fakeWindow('main');
		const w = new Windows(main);
		const a = fakeWindow('a');
		const b = fakeWindow('b');
		expect(w.all).toEqual([main]);
		w.add(a);
		w.add(b);
		expect(w.all).toEqual([main, a, b]);
		expect(w.popups).toEqual([a, b]);
		w.remove(a);
		expect(w.all).toEqual([main, b]);
	});

	test('a window opening or closing moves the version, and a repeat does not', () => {
		const w = new Windows(fakeWindow('main'));
		const a = fakeWindow('a');
		const v = w.version;
		w.add(a);
		expect(w.version).toBe(v + 1);
		w.add(a);
		expect(w.version).toBe(v + 1);
		w.touch();
		expect(w.version).toBe(v + 2);
		w.remove(a);
		w.remove(a);
		expect(w.version).toBe(v + 3);
	});

	test('a listener hears every window — the ones that are there and the ones that come', () => {
		const main = fakeWindow('main');
		const early = fakeWindow('early');
		const w = new Windows(main);
		w.add(early);
		const handler = () => {};
		const stop = w.listen('pointerdown', handler, true);
		const late = fakeWindow('late');
		w.add(late);
		for (const win of [main, early, late]) expect(win.listeners, win.name).toEqual([['pointerdown', handler, true]]);
		w.remove(early);
		expect(early.listeners).toEqual([]);
		stop();
		expect(main.listeners).toEqual([]);
		expect(late.listeners).toEqual([]);
		// And none arrives after it was stopped.
		w.add(fakeWindow('later'));
		expect(w.popups.at(-1)!.name).toBe('later');
		expect((w.popups.at(-1) as unknown as { listeners: unknown[] }).listeners).toEqual([]);
	});

	test('a window that throws when listened to does not stop the others hearing', () => {
		const main = fakeWindow('main');
		const bad = fakeWindow('bad');
		bad.addEventListener = () => {
			throw new Error('gone');
		};
		const w = new Windows(main);
		w.add(bad);
		const stop = w.listen('blur', () => {});
		expect(main.listeners).toHaveLength(1);
		stop();
	});

	test('rafs are drawn in the editor window while it shows', () => {
		const main = fakeWindow('main');
		const pop = fakeWindow('pop');
		const w = new Windows(main);
		w.add(pop);
		expect(w.frameWindow()).toBe(main);
		const h = w.requestFrame(() => {});
		expect(h.win).toBe(main);
		expect(main.rafs).toEqual([1]);
		w.cancelFrame(h);
		expect(main.rafs).toEqual([1, -1]);
	});

	test('…and in a detached one when the editor window is hidden — minimized, say', () => {
		const main = fakeWindow('main', false);
		const hidden = fakeWindow('hidden', false);
		const shown = fakeWindow('shown');
		const w = new Windows(main);
		w.add(hidden);
		w.add(shown);
		expect(w.frameWindow()).toBe(shown);
		expect(w.requestFrame(() => {}).win).toBe(shown);
	});

	test('…and in the editor window when none shows, or one is closed or unreadable', () => {
		const main = fakeWindow('main', false);
		const w = new Windows(main);
		const closed = fakeWindow('closed');
		closed.closed = true;
		const broken = fakeWindow('broken');
		Object.defineProperty(broken, 'document', {
			get() {
				throw new Error('detached');
			}
		});
		w.add(closed);
		w.add(broken);
		expect(w.frameWindow()).toBe(main);
		w.cancelFrame(null);
		w.cancelFrame({ win: broken, id: 3, live: true, waits: 0, cb: () => {} });
	});
});

describe('frames follow the windows that show', () => {
	const later = (ms: number) => new Promise((r) => setTimeout(r, ms));

	test('a frame runs once, in the window that painted it', () => {
		const main = fakeWindow('main');
		const w = new Windows(main);
		const ran: number[] = [];
		w.requestFrame((t) => ran.push(t));
		main.paint(7);
		main.paint(8);
		expect(ran).toEqual([7]);
	});

	test('a frame waiting in a window that goes hidden moves to one that shows', () => {
		const main = fakeWindow('main');
		const pop = fakeWindow('pop');
		const w = new Windows(main);
		w.add(pop);
		const ran: string[] = [];
		const h = w.requestFrame(() => ran.push('step'));
		expect(h.win).toBe(main);
		// The editor is minimized while the preview plays on the other screen.
		main.show(false);
		main.paint();
		expect(ran).toEqual([]);
		expect(h.win).toBe(pop);
		pop.paint();
		expect(ran).toEqual(['step']);
		// The frame it left behind is gone, not queued to run twice.
		expect(main.pending.size).toBe(0);
	});

	test('…and a frame that was asked for in the detached window comes home when that one hides', () => {
		const main = fakeWindow('main', false);
		const pop = fakeWindow('pop');
		const w = new Windows(main);
		w.add(pop);
		const ran: string[] = [];
		w.requestFrame(() => ran.push('step'));
		pop.show(false);
		main.show(true);
		main.paint();
		expect(ran).toEqual(['step']);
	});

	test('a loop that asks again from inside its frame keeps going across a hide', () => {
		const main = fakeWindow('main');
		const pop = fakeWindow('pop');
		const w = new Windows(main);
		w.add(pop);
		let n = 0;
		const step = () => {
			n++;
			w.requestFrame(step);
		};
		w.requestFrame(step);
		main.paint();
		expect(n).toBe(1);
		main.show(false);
		pop.paint();
		expect(n).toBe(2);
		pop.show(false);
		main.show(true);
		main.paint();
		expect(n).toBe(3);
	});

	test('a window closing hands its frame on', () => {
		const main = fakeWindow('main', false);
		const pop = fakeWindow('pop');
		const other = fakeWindow('other');
		const w = new Windows(main);
		w.add(pop);
		w.add(other);
		const ran: string[] = [];
		const h = w.requestFrame(() => ran.push('step'));
		expect(h.win).toBe(pop);
		pop.closed = true;
		w.remove(pop);
		expect(h.win).toBe(other);
		other.paint();
		expect(ran).toEqual(['step']);
	});

	test('a window opening takes a frame that has nowhere to run', () => {
		const main = fakeWindow('main', false);
		const w = new Windows(main);
		const ran: string[] = [];
		const h = w.requestFrame(() => ran.push('step'));
		expect(h.win).toBe(main);
		const pop = fakeWindow('pop');
		w.add(pop);
		expect(h.win).toBe(pop);
		pop.paint();
		expect(ran).toEqual(['step']);
	});

	test('a window that stops listening is not heard from again', () => {
		const main = fakeWindow('main');
		const pop = fakeWindow('pop');
		const w = new Windows(main);
		w.add(pop);
		expect(pop.changes.size).toBe(1);
		w.remove(pop);
		expect(pop.changes.size).toBe(0);
	});

	test('a cancelled frame never runs, wherever it was moved', () => {
		const main = fakeWindow('main');
		const pop = fakeWindow('pop');
		const w = new Windows(main);
		w.add(pop);
		const ran: string[] = [];
		const h = w.requestFrame(() => ran.push('step'));
		main.show(false);
		w.cancelFrame(h);
		pop.paint();
		main.show(true);
		main.paint();
		expect(ran).toEqual([]);
		expect(main.pending.size + pop.pending.size).toBe(0);
	});

	test('the watchdog asks again where one shows when no event said a window went', async () => {
		const main = fakeWindow('main');
		const pop = fakeWindow('pop');
		const w = new Windows(main, 5);
		w.add(pop);
		const ran: string[] = [];
		const h = w.requestFrame(() => ran.push('step'));
		// The editor stops painting and says nothing — no visibilitychange arrives.
		main.document.visibilityState = 'hidden';
		await later(40);
		expect(h.win).toBe(pop);
		pop.paint();
		expect(ran).toEqual(['step']);
		await later(30);
	});

	test('the watchdog leaves a frame that is waiting in the one window that shows', async () => {
		const main = fakeWindow('main');
		const w = new Windows(main, 5);
		const h = w.requestFrame(() => {});
		const id = h.id;
		await later(40);
		expect(h.win).toBe(main);
		expect(h.id).toBe(id);
		expect(main.pending.size).toBe(1);
		w.cancelFrame(h);
		await later(20);
	});

	test('the watchdog stops by itself once nothing waits', async () => {
		const main = fakeWindow('main');
		const w = new Windows(main, 5);
		const timers: unknown[] = [];
		const realSet = globalThis.setInterval;
		const realClear = globalThis.clearInterval;
		globalThis.setInterval = ((...a: Parameters<typeof setInterval>) => {
			const t = realSet(...a);
			timers.push(t);
			return t;
		}) as typeof setInterval;
		let cleared = 0;
		globalThis.clearInterval = ((t: Parameters<typeof clearInterval>[0]) => {
			cleared++;
			return realClear(t);
		}) as typeof clearInterval;
		try {
			const a = w.requestFrame(() => {});
			const b = w.requestFrame(() => {});
			// One timer serves every frame.
			expect(timers).toHaveLength(1);
			main.paint();
			w.cancelFrame(a);
			w.cancelFrame(b);
			await later(30);
			expect(cleared).toBe(1);
			w.requestFrame(() => {});
			expect(timers).toHaveLength(2);
			w.cancelFrame(null);
		} finally {
			globalThis.setInterval = realSet;
			globalThis.clearInterval = realClear;
		}
	});
});

describe('which window an element is in', () => {
	const win = { name: 'popup' } as unknown as Window;
	const doc = { nodeType: 9, ownerDocument: null, defaultView: win } as unknown as Document;
	const el = { nodeType: 1, ownerDocument: doc };

	test('is the window of its document', () => {
		expect(windowOf(el)).toBe(win);
		expect(documentOf(el)).toBe(doc);
		// A document is its own window's.
		expect(windowOf(doc)).toBe(win);
	});

	test('is the editor window for no element, or one in no document', () => {
		const fallback = (globalThis as unknown as { window?: Window }).window;
		const main = { name: 'main' } as unknown as Window;
		(globalThis as unknown as { window: Window; document: Document }).window = main;
		(globalThis as unknown as { document: Document }).document = { defaultView: main } as unknown as Document;
		try {
			expect(windowOf(null)).toBe(main);
			expect(windowOf(undefined)).toBe(main);
			expect(windowOf({ nodeType: 1, ownerDocument: null })).toBe(main);
			// A document with no window (a detached one).
			expect(windowOf({ nodeType: 1, ownerDocument: { defaultView: null } as unknown as Document })).toBe(main);
		} finally {
			(globalThis as unknown as { window?: Window }).window = fallback;
		}
	});

	test('observers are made with the constructors of that window', () => {
		const made: string[] = [];
		const popup = {
			ResizeObserver: class {
				constructor(cb: unknown) {
					made.push(`resize:${typeof cb}`);
				}
			},
			IntersectionObserver: class {
				constructor(cb: unknown, init: unknown) {
					made.push(`intersect:${typeof cb}:${JSON.stringify(init)}`);
				}
			}
		} as unknown as Window;
		const inPopup = { nodeType: 1, ownerDocument: { nodeType: 9, defaultView: popup } };
		resizeObserverFor(inPopup as never, () => {});
		intersectionObserverFor(inPopup as never, () => {}, { threshold: 0.5 });
		expect(made).toEqual(['resize:function', 'intersect:function:{"threshold":0.5}']);
	});
});

describe('window handlers', () => {
	test('a capture suffix is the capture phase, as <svelte:window onpointerdowncapture> has it', () => {
		expect(parseEventKey('pointerdown')).toEqual({ type: 'pointerdown', capture: false });
		expect(parseEventKey('pointerdowncapture')).toEqual({ type: 'pointerdown', capture: true });
		expect(parseEventKey('pointermove')).toEqual({ type: 'pointermove', capture: false });
		// An event that merely ends in the word is not a phase.
		expect(parseEventKey('capture')).toEqual({ type: 'capture', capture: false });
	});

	test('the attachment listens on the window its element is in, and stops when it goes', () => {
		const win = fakeWindow('popup');
		const el = { nodeType: 1, ownerDocument: { nodeType: 9, defaultView: win } };
		const move = () => {};
		const key = () => {};
		const stop = onWindow({ pointermove: move, pointerdowncapture: key })(el as never) as () => void;
		expect(win.listeners).toEqual([
			['pointermove', move, false],
			['pointerdown', key, true]
		]);
		stop();
		expect(win.listeners).toEqual([]);
	});
});
