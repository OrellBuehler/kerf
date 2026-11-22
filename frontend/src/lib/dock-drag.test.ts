import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { abandonDragOnEscape, type DragEvents, type DragWindow } from './dock-drag';

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
	const subs = { panel: new Set<(e: unknown) => void>(), group: new Set<(e: unknown) => void>() };
	const api = {
		onWillDragPanel: (l: (e: unknown) => void) => {
			subs.panel.add(l);
			return { dispose: () => subs.panel.delete(l) };
		},
		onWillDragGroup: (l: (e: unknown) => void) => {
			subs.group.add(l);
			return { dispose: () => subs.group.delete(l) };
		}
	} as unknown as DragEvents;
	return { api, subs, startPanel: (e: unknown) => subs.panel.forEach((l) => l(e)), startGroup: (e: unknown) => subs.group.forEach((l) => l(e)) };
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
		expect(d.subs.panel.size).toBe(0);
		expect(d.subs.group.size).toBe(0);
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
