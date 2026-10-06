const FOCUSABLE =
	'a[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Svelte action for a modal's root: take focus on open, keep Tab inside, and
 *  hand focus back to whatever had it on close. The app behind a modal is also
 *  `inert` (see `+page.svelte`), so this is what keeps focus from falling out
 *  of the dialog to the browser chrome. */
export function trapFocus(node: HTMLElement) {
	const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
	node.focus();

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

	return {
		destroy() {
			node.removeEventListener('keydown', onKeydown);
			// After the page behind the modal has left `inert`.
			setTimeout(() => previous?.isConnected && previous.focus(), 0);
		}
	};
}
