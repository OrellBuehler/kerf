import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { abandonDragOnEscape, onlyPrimaryDrags, startsDrag, type DragEvents, type DragWindow } from './dock-drag';

type Listener = (e: Event) => void;

class FakeWindow implements DragWindow {
	#on = new Map<string, Set<Listener>>();
	dispatched: Event[] = [];
	addEventListener(type: string, listener: Listener) {
		if (!this.#on.has(type)) this.#on.set(type, new Set());
		this.#on.get(type)!.add(listener);
	}
	removeEventListener(type: string, listener: Listener) {
		this.#on.get(type)?.delete(listener);
	}
	dispatchEvent(e: Event) {
		this.dispatched.push(e);
		for (const l of [...(this.#on.get(e.type) ?? [])]) l(e);
		return true;
	}
	listeners(type: string) {
		return this.#on.get(type)?.size ?? 0;
	}
}

function dock() {
	const names = ['panel', 'group', 'added', 'removed', 'closing'] as const;
	const subs = Object.fromEntries(names.map((n) => [n, new Set<(e: unknown) => void>()])) as Record<(typeof names)[number], Set<(e: unknown) => void>>;
	const on = (n: (typeof names)[number]) => (l: (e: unknown) => void) => {
		subs[n].add(l);
		return { dispose: () => subs[n].delete(l) };
	};
	const api = {
		onWillDragPanel: on('panel'),
		onWillDragGroup: on('group'),
		onDidAddPopoutGroup: on('added'),
		onDidRemovePopoutGroup: on('removed'),
		onWillClosePopoutWindow: on('closing')
	} as unknown as DragEvents;
	const fire = (n: (typeof names)[number]) => (e: unknown = {}) => subs[n].forEach((l) => l(e));
	return { api, subs, startPanel: fire('panel'), startGroup: fire('group'), popoutAdded: fire('added'), popoutRemoved: fire('removed'), popoutClosing: fire('closing') };
}

const key = (k: string) => Object.assign(new Event('keydown', { cancelable: true }), { key: k });
const cancelOf = (pointerId: number) => Object.assign(new Event('pointercancel'), { pointerId });
const started = (pointerId: number, view?: FakeWindow) => ({ nativeEvent: { pointerId, target: { ownerDocument: { defaultView: view ?? null } } } });

describe('abandoning a dock drag', () => {
	test('Escape cancels the pointer that holds the drag, once, and says nothing more', () => {
		const win = new FakeWindow();
		const d = dock();
		abandonDragOnEscape(d.api, win, cancelOf);
		d.startPanel(started(7));
		const esc = key('Escape');
		win.dispatchEvent(esc);
		expect(esc.defaultPrevented).toBe(true);
		const cancels = win.dispatched.filter((e) => e.type === 'pointercancel');
		expect(cancels).toHaveLength(1);
		expect((cancels[0] as unknown as { pointerId: number }).pointerId).toBe(7);
		// The watch is over with the drag: a second Escape is nobody's.
		const again = key('Escape');
		win.dispatchEvent(again);
		expect(again.defaultPrevented).toBe(false);
		expect(win.dispatched.filter((e) => e.type === 'pointercancel')).toHaveLength(1);
		expect(win.listeners('keydown')).toBe(0);
	});

	test('another key is left alone', () => {
		const win = new FakeWindow();
		const d = dock();
		abandonDragOnEscape(d.api, win, cancelOf);
		d.startPanel(started(1));
		const e = key('a');
		win.dispatchEvent(e);
		expect(e.defaultPrevented).toBe(false);
		expect(win.dispatched.filter((x) => x.type === 'pointercancel')).toHaveLength(0);
	});

	test('losing the window abandons it', () => {
		const win = new FakeWindow();
		const d = dock();
		abandonDragOnEscape(d.api, win, cancelOf);
		d.startGroup(started(3));
		win.dispatchEvent(new Event('blur'));
		expect(win.dispatched.filter((e) => e.type === 'pointercancel')).toHaveLength(1);
	});

	test('a drop ends the watch without cancelling anything', () => {
		const win = new FakeWindow();
		const d = dock();
		abandonDragOnEscape(d.api, win, cancelOf);
		d.startPanel(started(2));
		win.dispatchEvent(Object.assign(new Event('pointerup'), { pointerId: 2 }));
		expect(win.listeners('keydown')).toBe(0);
		expect(win.listeners('blur')).toBe(0);
		const esc = key('Escape');
		win.dispatchEvent(esc);
		expect(esc.defaultPrevented).toBe(false);
		expect(win.dispatched.filter((e) => e.type === 'pointercancel')).toHaveLength(0);
	});

	test('the release of another pointer does not end it', () => {
		const win = new FakeWindow();
		const d = dock();
		abandonDragOnEscape(d.api, win, cancelOf);
		d.startPanel(started(2));
		win.dispatchEvent(Object.assign(new Event('pointerup'), { pointerId: 9 }));
		win.dispatchEvent(key('Escape'));
		expect(win.dispatched.filter((e) => e.type === 'pointercancel')).toHaveLength(1);
	});

	test('a drag that began in a detached window is watched there, not in the editor’s', () => {
		const editor = new FakeWindow();
		const detached = new FakeWindow();
		const d = dock();
		abandonDragOnEscape(d.api, editor, cancelOf);
		d.startPanel(started(4, detached));
		expect(editor.listeners('keydown')).toBe(0);
		expect(detached.listeners('keydown')).toBe(1);
		detached.dispatchEvent(key('Escape'));
		expect(detached.dispatched.filter((e) => e.type === 'pointercancel')).toHaveLength(1);
		expect(editor.dispatched).toHaveLength(0);
	});

	test('a drag the browser owns (no pointer) is not watched', () => {
		const win = new FakeWindow();
		const d = dock();
		abandonDragOnEscape(d.api, win, cancelOf);
		d.startPanel({ nativeEvent: { type: 'dragstart' } });
		expect(win.listeners('keydown')).toBe(0);
	});

	test('a second drag replaces the first, and disposing ends the watch', () => {
		const win = new FakeWindow();
		const d = dock();
		const watch = abandonDragOnEscape(d.api, win, cancelOf);
		d.startPanel(started(1));
		d.startPanel(started(2));
		expect(win.listeners('keydown')).toBe(1);
		watch.dispose();
		expect(win.listeners('keydown')).toBe(0);
		for (const set of Object.values(d.subs)) expect(set.size).toBe(0);
	});
});

describe('giving a drag up before the dock is rebuilt', () => {
	test('abandon() cancels the pointer that holds the drag, once, and ends the watch', () => {
		const win = new FakeWindow();
		const d = dock();
		const watch = abandonDragOnEscape(d.api, win, cancelOf);
		d.startPanel(started(5));
		watch.abandon();
		const cancels = win.dispatched.filter((e) => e.type === 'pointercancel');
		expect(cancels).toHaveLength(1);
		expect((cancels[0] as unknown as { pointerId: number }).pointerId).toBe(5);
		expect(win.listeners('keydown')).toBe(0);
		watch.abandon();
		expect(win.dispatched.filter((e) => e.type === 'pointercancel')).toHaveLength(1);
	});

	test('with no drag under way it does nothing', () => {
		const win = new FakeWindow();
		const watch = abandonDragOnEscape(dock().api, win, cancelOf);
		watch.abandon();
		expect(win.dispatched).toEqual([]);
	});

	test('a drag that was dropped is not cancelled afterwards', () => {
		const win = new FakeWindow();
		const d = dock();
		const watch = abandonDragOnEscape(d.api, win, cancelOf);
		d.startPanel(started(2));
		win.dispatchEvent(Object.assign(new Event('pointerup'), { pointerId: 2 }));
		watch.abandon();
		expect(win.dispatched.filter((e) => e.type === 'pointercancel')).toHaveLength(0);
	});

	test('a drag begun in a detached window is cancelled there', () => {
		const editor = new FakeWindow();
		const detached = new FakeWindow();
		const d = dock();
		const watch = abandonDragOnEscape(d.api, editor, cancelOf);
		d.startGroup(started(8, detached));
		watch.abandon();
		expect(detached.dispatched.filter((e) => e.type === 'pointercancel')).toHaveLength(1);
		expect(editor.dispatched).toHaveLength(0);
	});

	test('a window opening, closing or about to close moves panels under the pointer: each ends the drag', () => {
		for (const event of ['popoutAdded', 'popoutRemoved', 'popoutClosing'] as const) {
			const win = new FakeWindow();
			const d = dock();
			abandonDragOnEscape(d.api, win, cancelOf);
			d.startPanel(started(4));
			d[event]();
			expect(win.dispatched.filter((e) => e.type === 'pointercancel'), event).toHaveLength(1);
			expect(win.listeners('keydown'), event).toBe(0);
		}
	});

	test('a window opening with no drag under way is nothing', () => {
		const win = new FakeWindow();
		const d = dock();
		abandonDragOnEscape(d.api, win, cancelOf);
		d.popoutAdded();
		expect(win.dispatched).toEqual([]);
	});
});

describe('only the primary button drags a panel', () => {
	/** A press as the window hears it: a plain object, since an Event's target cannot be set by hand. */
	const on = (selectors: string[]) => ({ closest: (s: string) => (selectors.some((x) => s.split(',').map((p) => p.trim()).includes(x)) ? {} : null) });
	const watched = () => {
		const win = new FakeWindow();
		const stop = onlyPrimaryDrags(win);
		const send = (button: number, target: unknown) => {
			let stopped = false;
			const e = { type: 'pointerdown', button, target, stopPropagation: () => (stopped = true) } as unknown as Event;
			win.dispatchEvent(e);
			return stopped;
		};
		return { win, stop, send };
	};

	test('a button other than the first is kept from a tab, the strip beside the tabs and the group’s header', () => {
		const { send } = watched();
		for (const button of [1, 2, 3, 4]) {
			for (const handle of ['.dv-tab', '.dv-void-container', '.dv-tabs-and-actions-container']) {
				expect(send(button, on([handle])), `button ${button} on ${handle}`).toBe(true);
			}
		}
	});

	test('the primary button, a finger and a pen tip are not', () => {
		const { send } = watched();
		expect(send(0, on(['.dv-tab']))).toBe(false);
		expect(startsDrag({ button: 0 })).toBe(true);
		expect(startsDrag({ button: 2 })).toBe(false);
		expect(startsDrag({ button: -1 })).toBe(false);
	});

	test('a press anywhere else is the page’s own: a timeline gesture on the right button still hears it', () => {
		const { send } = watched();
		expect(send(2, on(['.timeline-lane']))).toBe(false);
		expect(send(2, null)).toBe(false);
		expect(send(2, {})).toBe(false);
	});

	test('the context menu that follows a right-click is another event and is not kept', () => {
		const { win } = watched();
		let heard = 0;
		win.addEventListener('contextmenu', () => heard++);
		win.dispatchEvent(new Event('contextmenu'));
		expect(heard).toBe(1);
	});

	test('stopping the watch lets every press through again', () => {
		const { send, stop, win } = watched();
		stop();
		expect(win.listeners('pointerdown')).toBe(0);
		expect(send(2, on(['.dv-tab']))).toBe(false);
	});
});

// What the dock is created with. Nothing in it may pin a panel (the preview was reported as locked): no `locked`, no
// `disableDnd`, no switched-off edges, no per-group lock, and every drag is a pointer drag, since the browser's own is dead in
// the Windows webview. Floating groups stay off for now (`frontend.md`: they cannot be seen through the GPU surface's hole,
// and the stored layout does not carry them).
describe('how the dock is created', () => {
	const source = readFileSync(join(import.meta.dir, 'components/editor/Workspace.svelte'), 'utf8');
	const allSources = [source, readFileSync(join(import.meta.dir, 'workspace.svelte.ts'), 'utf8'), readFileSync(join(import.meta.dir, 'layout.ts'), 'utf8')].join('\n');

	test('panels are dragged by pointer events', () => {
		expect(source).toContain("dndStrategy: 'pointer'");
		expect(source).not.toContain("dndStrategy: 'html5'");
	});

	test('no panel, group or edge is locked or refuses a drop', () => {
		for (const pinned of ['locked:', '.locked =', 'disableDnd', 'dndEdges: false', "'no-drop-target'", 'onWillDrop', 'onUnhandledDragOverEvent', 'headerPosition', 'hideHeader']) {
			expect(allSources, pinned).not.toContain(pinned);
		}
	});

	test('floating groups are off, and a stored layout does not carry them', () => {
		expect(source).toContain('disableFloatingGroups: true');
	});
});
