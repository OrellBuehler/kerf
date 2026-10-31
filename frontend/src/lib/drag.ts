/* One pointer gesture, with the ways out every drag in the editor has to honour.
 *
 * `beginDrag` is called from a `pointerdown`: it captures the pointer on the
 * element so move / up keep arriving wherever the pointer goes (off the clip, off
 * the window), then ends in exactly one of two ways —
 *  - `commit` on `pointerup`, once;
 *  - `abandon` on Escape, `pointercancel`, a lost capture, the window losing focus,
 *    or a move that reports the primary button already up (a release that was
 *    missed — over a native dialog, outside the webview). Nothing is written.
 * Whichever happens, the listeners are gone and the capture released, and a later
 * event of the same gesture is ignored. It returns a function that abandons it
 * from outside (the clip it belongs to was removed mid-drag).
 *
 * Every listener is on the window (capture phase, filtered by `pointerId`), none on the
 * element: a gesture must outlive the element it began on. A removed element is sent
 * no more events and loses its capture (the browser fires `lostpointercapture` at the
 * document), so listeners on it would leave the gesture's Escape / blur hooks installed,
 * swallowing the next Escape, with nothing ever to end it. The window sees the release,
 * the cancel and the lost capture wherever the pointer is, and runs before any handler
 * of the page, as the element's own listener did. A caller whose element can go while the
 * gesture is held also keeps the returned function and calls it on teardown. */

import { windowOf } from './realm';

export interface DragHandlers {
	/** The pointer moved (only while the primary button is down). */
	move(e: PointerEvent): void;
	/** The button was released: the gesture is complete. */
	commit(e: PointerEvent): void;
	/** The gesture was given up: put everything back. */
	abandon(): void;
}

/** What a drag listens on besides the captured element — injectable for tests. By
 *  default the window the element is in: a drag in a detached window ends there. */
export interface DragEnv {
	window: Pick<Window, 'addEventListener' | 'removeEventListener'>;
}

export function beginDrag(e: PointerEvent, h: DragHandlers, env: DragEnv = { window: windowOf(e.currentTarget as Element) }): () => void {
	const el = e.currentTarget as Element;
	const id = e.pointerId;
	let done = false;
	try {
		el.setPointerCapture(id);
	} catch {
		// Best effort: without a capture the gesture still ends on pointerup,
		// cancel, blur or Escape — it just may not see a release outside the window.
	}

	const finish = (then: () => void) => {
		if (done) return;
		done = true;
		const w = env.window;
		w.removeEventListener('pointermove', onMove as EventListener, true);
		w.removeEventListener('pointerup', onUp as EventListener, true);
		w.removeEventListener('pointercancel', onCancel as EventListener, true);
		w.removeEventListener('lostpointercapture', onLost as EventListener, true);
		w.removeEventListener('keydown', onKey as EventListener, true);
		w.removeEventListener('blur', onBlur);
		try {
			el.releasePointerCapture(id);
		} catch {
			// already released, or the element is gone
		}
		then();
	};
	const abandon = () => finish(() => h.abandon());

	function onMove(ev: PointerEvent) {
		if (ev.pointerId !== id) return;
		if ((ev.buttons & 1) === 0) return abandon(); // a release we never saw
		h.move(ev);
	}
	function onUp(ev: PointerEvent) {
		if (ev.pointerId !== id) return;
		finish(() => h.commit(ev));
	}
	function onCancel(ev: PointerEvent) {
		if (ev.pointerId === id) abandon();
	}
	function onLost(ev: PointerEvent) {
		// Fires after pointerup too, by which time the gesture is already done.
		if (ev.pointerId === id) abandon();
	}
	function onKey(ev: KeyboardEvent) {
		if (ev.key !== 'Escape') return;
		ev.stopPropagation(); // abandoning a drag is not also "close whatever is open"
		abandon();
	}
	function onBlur() {
		abandon();
	}

	const w = env.window;
	w.addEventListener('pointermove', onMove as EventListener, true);
	w.addEventListener('pointerup', onUp as EventListener, true);
	w.addEventListener('pointercancel', onCancel as EventListener, true);
	w.addEventListener('lostpointercapture', onLost as EventListener, true);
	w.addEventListener('keydown', onKey as EventListener, true);
	w.addEventListener('blur', onBlur);
	return abandon;
}
