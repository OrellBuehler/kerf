// A group of radio buttons the keyboard moves through as one: one tab stop (the markup gives the
// chosen one `tabindex=0` and the rest `-1`), the arrows move to the neighbour and choose it — in the
// ARIA radio pattern selection follows focus — and Home / End jump. `<div role="radiogroup" use:roving>`.

import { radioKey, rowFor } from './menu-nav';

export function roving(node: HTMLElement) {
	function onKey(e: KeyboardEvent) {
		const k = radioKey(e);
		if (!k) return;
		const radios = [...node.querySelectorAll<HTMLElement>('[role="radio"]')];
		const rows = radios.map((r) => ({
			focusable: !r.hasAttribute('disabled') && r.getAttribute('aria-disabled') !== 'true',
			label: r.textContent ?? ''
		}));
		const to = rowFor(rows, radios.indexOf(e.target as HTMLElement), k);
		if (to === null) return;
		e.preventDefault();
		radios[to].focus();
		radios[to].click();
	}
	node.addEventListener('keydown', onKey);
	return {
		destroy() {
			node.removeEventListener('keydown', onKey);
		}
	};
}
