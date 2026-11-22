// A panel dragged in the dock is a pointer drag (`dndStrategy: 'pointer'` in `Workspace.svelte`), not the
// browser's own drag-and-drop: Tauri's drag-drop handler, kept on for dropping files, answers an in-page HTML5
// drag with `DROPEFFECT_NONE` on Windows, so WebView2 never delivers its `dragover` / `drop` and no tab could be
// moved there at all. The browser's drag was abandoned by Escape and by losing the window; a pointer drag is not,
// so that is done here, the way every other gesture of the editor is: Escape, `pointercancel` and a blur put the
// panel back where it was.
//
// dockview ends a pointer drag on a `pointercancel` of the pointer it was started by (and drops nothing), so
// abandoning one is saying exactly that.

import type { DockviewApi } from 'dockview';

/** The part of a window the watch listens to. */
export interface DragWindow {
	addEventListener(type: string, listener: (e: Event) => void, capture?: boolean): void;
	removeEventListener(type: string, listener: (e: Event) => void, capture?: boolean): void;
	dispatchEvent(e: Event): boolean;
}

/** The part of the dock that says a drag has begun. */
export type DragEvents = Pick<DockviewApi, 'onWillDragPanel' | 'onWillDragGroup'>;

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

/** Abandon a panel's drag on Escape and when the window loses focus. `win` is the editor's window; a drag that began
 *  in a detached window is watched in that one. `cancel` makes the event that says so (a test passes its own). */
export function abandonDragOnEscape(
	api: DragEvents,
	win: DragWindow = window,
	cancel: (pointerId: number) => Event = cancelOf
): { dispose(): void } {
	let stop: (() => void) | null = null;
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
			if (stop === end) stop = null;
		};
		const abandon = () => {
			end();
			where.dispatchEvent(cancel(pointerId));
		};
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
	const subs = [api.onWillDragPanel(begin), api.onWillDragGroup(begin)];
	return {
		dispose() {
			for (const s of subs) s.dispose();
			stop?.();
		}
	};
}
