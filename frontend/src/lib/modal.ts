const FOCUSABLE =
	'a[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Where focus goes when a dialog opened from the menu bar by keyboard closes: the entry that
 *  held it is gone and the page is `body`. The bar marks its tab stop only while keys are
 *  driving it — a pointer user is left on `body`, or Space would open a menu instead of playing. */
const RETURN_TARGET = '[data-focus-return]';
/** What takes focus when the control that had it is replaced (Generate giving way to Cancel):
 *  the actions of a dialog's footer carry the mark. */
const RESCUE_TARGET = '[data-rescue]:not([disabled])';

interface Focusable {
	isConnected: boolean;
	focus(): void;
}

/** Where to hand focus back on close: the control that had it when the dialog opened, while it
 *  is still there and was a control at all, else the fallback. */
export function returnTarget<T extends Focusable>(previous: T | null, body: unknown, fallback: () => T | null): T | null {
	return previous && previous !== body && previous.isConnected ? previous : fallback();
}

/** Whether focus has been left with nothing and should go back on a control: it is on the page,
 *  or on the dialog's own root because an earlier rescue found nothing enabled to give it to
 *  (`parked`). A root the user clicked on is where they put it; so is any control. */
export function focusLost(active: unknown, body: unknown, root: unknown, parked = false): boolean {
	return !active || active === body || (parked && active === root);
}

export type CloseVia = 'escape' | 'scrim' | 'button';

export interface CloseRules {
	/** A run that cannot be walked away from: nothing closes the dialog but its own Cancel. */
	busy?: boolean;
	/** A click on the scrim closes it. Off for a dialog that holds work: typed input, chosen options. */
	dismissOnScrim?: boolean;
}

/** Whether a way of closing a dialog works right now. Escape and the close button are
 *  deliberate; the scrim is where a stray click lands, so a dialog can set it aside. */
export function mayClose(via: CloseVia, { busy = false, dismissOnScrim = true }: CloseRules = {}): boolean {
	if (busy) return false;
	return via === 'scrim' ? dismissOnScrim : true;
}

/** A click on the scrim is one that began there too: a press inside the dialog that is
 *  released outside it (selecting text in a field, then letting go over the dark) lands its
 *  click on the scrim as well, and must not close what the user was typing in. */
export function scrimClick(pressedOnScrim: boolean, target: unknown, scrim: unknown): boolean {
	return pressedOnScrim && target === scrim;
}

export interface TrapOptions {
	/** What takes focus on open instead of the dialog itself: a selector for a field. */
	initial?: string;
	/** Keep focus in the dialog when what had it goes away — the Generate button becoming Cancel,
	 *  a button being disabled: it moves to the `[data-rescue]` action, or the root when none is
	 *  enabled. For dialogs that swap their footer for a run. */
	rescue?: boolean;
}

/** Svelte action for a modal's root: take focus on open, keep Tab inside, and
 *  hand focus back to whatever had it on close. The app behind a modal is also
 *  `inert` (see `+page.svelte`), so this is what keeps focus from falling out
 *  of the dialog to the browser chrome. */
export function trapFocus(node: HTMLElement, options: TrapOptions = {}) {
	const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
	const start = options.initial ? node.querySelector<HTMLElement>(options.initial) : null;
	(start ?? node).focus();

	function onKeydown(e: KeyboardEvent) {
		if (e.key !== 'Tab') return;
		const items = [...node.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((el) => el.offsetParent !== null);
		if (items.length === 0) {
			e.preventDefault();
			node.focus();
			return;
		}
		const first = items[0];
		const last = items[items.length - 1];
		const active = document.activeElement;
		if (e.shiftKey && (active === first || active === node)) {
			e.preventDefault();
			last.focus();
		} else if (!e.shiftKey && active === last) {
			e.preventDefault();
			first.focus();
		}
	}
	node.addEventListener('keydown', onKeydown);

	let parked = false;
	const watcher = options.rescue
		? new MutationObserver(() => {
				if (!focusLost(document.activeElement, document.body, node, parked)) {
					parked = false;
					return;
				}
				const target = node.querySelector<HTMLElement>(RESCUE_TARGET);
				parked = !target;
				(target ?? node).focus();
			})
		: null;
	watcher?.observe(node, { childList: true, subtree: true, attributes: true, attributeFilter: ['disabled'] });

	return {
		destroy() {
			node.removeEventListener('keydown', onKeydown);
			watcher?.disconnect();
			// After the page behind the modal has left `inert`.
			setTimeout(
				() => returnTarget(previous, document.body, () => document.querySelector<HTMLElement>(RETURN_TARGET))?.focus(),
				0
			);
		}
	};
}
