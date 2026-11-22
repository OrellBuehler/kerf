// A panel dragged in the dock is a pointer drag (`dndStrategy: 'pointer'` in `Workspace.svelte`), not the
// browser's own drag-and-drop: Tauri's drag-drop handler, kept on for dropping files, answers an in-page HTML5
// drag with `DROPEFFECT_NONE` on Windows, so WebView2 never delivers its `dragover` / `drop` and no tab could be
// moved there at all. The browser's drag was abandoned by Escape and by losing the window; a pointer drag is not,
// so that is done here, the way every other gesture of the editor is: Escape, `pointercancel` and a blur put the
// panel back where it was.
//
// dockview ends a pointer drag on a `pointercancel` of the pointer it was started by (and drops nothing), so
// abandoning one is saying exactly that.
//
// Two more things the browser's drag did not need said: a drag is only the primary button's (dockview's pointer source
// starts one from any button, so a right-click that opens the tab's menu moved the panel), and a drag does not outlive
// the dock it began in — a workspace switch, a reset or a window opening rebuilds the panels under the pointer, and the
// drop would then land on a panel of the new arrangement and be written as the user's.

import type { DockviewApi } from 'dockview';

/** The part of a window the watch listens to. */
export interface DragWindow {
	addEventListener(type: string, listener: (e: Event) => void, capture?: boolean): void;
	removeEventListener(type: string, listener: (e: Event) => void, capture?: boolean): void;
	dispatchEvent(e: Event): boolean;
}

/** The part of the dock that says a drag has begun, and what ends one from outside: a window opening or closing. */
export type DragEvents = Pick<
	DockviewApi,
	'onWillDragPanel' | 'onWillDragGroup' | 'onDidAddPopoutGroup' | 'onDidRemovePopoutGroup' | 'onWillClosePopoutWindow'
>;

/** A watch over the dock's drags. */
export interface DragWatch {
	/** Give the drag under way up, as Escape does (nothing when there is none). Called before the dock is rebuilt. */
	abandon(): void;
	dispose(): void;
}

interface Started {
	nativeEvent: unknown;
}

/** The pointer and window of a drag's start, or null for one that is not a pointer's (a native drag the browser
 *  itself abandons). */
function originOf(e: Started, fallback: DragWindow): { pointerId: number; win: DragWindow } | null {
	const ev = e.nativeEvent as { pointerId?: unknown; target?: { ownerDocument?: { defaultView?: DragWindow | null } | null } | null } | null;
	if (!ev || typeof ev.pointerId !== 'number') return null;
	return { pointerId: ev.pointerId, win: ev.target?.ownerDocument?.defaultView ?? fallback };
}

/** What dockview is told to abandon a drag: a `pointercancel` of the pointer that holds it. */
function cancelOf(pointerId: number): Event {
	return new PointerEvent('pointercancel', { pointerId, bubbles: true, cancelable: false });
}

/** Abandon a panel's drag on Escape, when the window loses focus and when the dock is about to be rebuilt (`abandon()`;
 *  a window opening or closing is that too). `win` is the editor's window; a drag that began in a detached window is
 *  watched in that one. `cancel` makes the event that says so (a test passes its own). */
export function abandonDragOnEscape(
	api: DragEvents,
	win: DragWindow = window,
	cancel: (pointerId: number) => Event = cancelOf
): DragWatch {
	let stop: (() => void) | null = null;
	let giveUp: (() => void) | null = null;
	const begin = (e: Started) => {
		const origin = originOf(e, win);
		if (!origin) return;
		stop?.();
		const { pointerId, win: where } = origin;
		const end = () => {
			where.removeEventListener('keydown', onKey, true);
			where.removeEventListener('blur', onBlur);
			where.removeEventListener('pointerup', onUp, true);
			where.removeEventListener('pointercancel', onUp, true);
			if (stop === end) {
				stop = null;
				giveUp = null;
			}
		};
		const abandon = () => {
			end();
			where.dispatchEvent(cancel(pointerId));
		};
		giveUp = abandon;
		const onKey = (k: Event) => {
			if ((k as KeyboardEvent).key !== 'Escape') return;
			// The key belongs to the drag: nothing else (a menu, a field) is told it was pressed.
			k.preventDefault();
			k.stopPropagation();
			abandon();
		};
		const onBlur = () => abandon();
		const onUp = (u: Event) => {
			if ((u as PointerEvent).pointerId === pointerId) end();
		};
		where.addEventListener('keydown', onKey, true);
		where.addEventListener('blur', onBlur);
		where.addEventListener('pointerup', onUp, true);
		where.addEventListener('pointercancel', onUp, true);
		stop = end;
	};
	const abandon = () => giveUp?.();
	const subs = [
		api.onWillDragPanel(begin),
		api.onWillDragGroup(begin),
		// A window opening or closing moves panels about under the pointer.
		api.onDidAddPopoutGroup(abandon),
		api.onDidRemovePopoutGroup(abandon),
		api.onWillClosePopoutWindow(abandon)
	];
	return {
		abandon,
		dispose() {
			for (const s of subs) s.dispose();
			stop?.();
		}
	};
}

/** The part of the page the drags start from: a tab, the empty strip beside the tabs (it moves the whole group) and the
 *  strip itself. */
const DRAG_HANDLES = '.dv-tab, .dv-void-container, .dv-tabs-and-actions-container';

/** Whether a press may start a drag: the primary button's (and a finger's or a pen's tip, which are button 0). */
export function startsDrag(press: { button: number }): boolean {
	return press.button === 0;
}

interface Press extends Event {
	button: number;
	target: EventTarget | null;
}

/** Only the primary button drags a panel. dockview's pointer source starts a drag from any button, so a right-click on a
 *  tab — which is only to open its menu — moved the panel under it, and a middle press did the same. Another button's press
 *  on a tab or a group's strip is kept from the dock; the `contextmenu` that follows a right-click is another event and
 *  still reaches it. Returns what stops listening. */
export function onlyPrimaryDrags(win: DragWindow): () => void {
	const listener = (e: Event) => {
		const press = e as Press;
		if (startsDrag(press)) return;
		const el = press.target as { closest?: (selector: string) => unknown } | null;
		if (el?.closest?.(DRAG_HANDLES)) e.stopPropagation();
	};
	win.addEventListener('pointerdown', listener, true);
	return () => win.removeEventListener('pointerdown', listener, true);
}
