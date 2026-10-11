// A group of radio buttons the keyboard moves through as one: one tab stop (the markup gives it to
// `tabStop()`'s pick and `-1` to the rest), the arrows move to the neighbour and Home / End jump.
// `<div role="radiogroup" use:roving>`: in the ARIA radio pattern selection follows focus, so the
// arrow also chooses. `use:roving={{ follow: false }}` is for a group whose choice is *expensive* —
// Proxy size rebuilds every proxy, so an arrow run across four sizes must not queue four rebuilds:
// the arrows only move focus, and Space / Enter (the button's own click) choose.

import { radioKey, rowFor } from './menu-nav';

export interface RovingOptions {
	/** Choose the option the arrow lands on (default), or only focus it. */
	follow?: boolean;
}

export function roving(node: HTMLElement, options: RovingOptions = {}) {
	let follow = options.follow ?? true;
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
		if (follow) radios[to].click();
	}
	node.addEventListener('keydown', onKey);
	return {
		update(next: RovingOptions = {}) {
			follow = next.follow ?? true;
		},
		destroy() {
			node.removeEventListener('keydown', onKey);
		}
	};
}
