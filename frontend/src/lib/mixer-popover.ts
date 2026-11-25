/* The keyboard and focus rules of the Mixer's popovers (`MixerPopover.svelte`), pure so a test holds them:
 *
 *  - what a key does while one is open — Escape closes it; Tab passes (the focus may leave it); a modifier chord the popover does not
 *    use (undo, redo, save: Ctrl / Cmd + a letter) reaches the page; Delete and Backspace never do, with or without a modifier, so a key
 *    meant for a control cannot delete the clip behind it; every other key is the popover's and is kept from the page's shortcuts;
 *  - when moving the focus closes it — to something outside the box and outside the button that opened it (a Tab out leaves it
 *    open over the strips otherwise); a press on the opening button is that button's toggle, and focus that went nowhere (a click on
 *    a part of the box that takes none) is not leaving.
 */

export interface PopoverKey {
	key: string;
	ctrlKey?: boolean;
	metaKey?: boolean;
}

export interface PopoverKeyRule {
	/** Keep the key from the page's shortcuts. */
	stop: boolean;
	/** Close the popover, and hand the focus back to its button. */
	close: boolean;
}

/** Keys the page must never get from a popover, whatever the modifiers: they delete. */
const DELETING = new Set(['Delete', 'Backspace']);

export function popoverKey(e: PopoverKey): PopoverKeyRule {
	if (e.key === 'Escape') return { stop: true, close: true };
	if (e.key === 'Tab') return { stop: false, close: false };
	if (DELETING.has(e.key)) return { stop: true, close: false };
	if (e.ctrlKey || e.metaKey) return { stop: false, close: false };
	return { stop: true, close: false };
}

/** Whether focus moving to `to` (a `relatedTarget`: `null` when it went to nothing) closes a popover whose box is `box` and whose
 *  button is `anchor`. */
export function focusLeaves(to: unknown, box: Node, anchor: Node): boolean {
	if (!(to instanceof Node)) return false;
	return !box.contains(to) && !anchor.contains(to);
}
