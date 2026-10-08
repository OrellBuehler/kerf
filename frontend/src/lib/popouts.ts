/* The pure side of detached panels: sizes, titles, the rules, and the bookkeeping that
   pairs a window the page announced with the window that opened. The runes singleton
   that drives dockview is `popout.svelte.ts`; the windows' registry is
   `windows.svelte.ts`. */

import { PANELS, type PanelId } from './layout';

/** What a detached window is never smaller than, whatever the panel's own minimums:
 *  a panel is worth a window when it has room. */
export const DETACH_MIN_WIDTH = 420;
export const DETACH_MIN_HEIGHT = 300;
/** …and the biggest one a hand-detached panel opens at, whatever the group it left. */
export const DETACH_MAX = 4000;

/** The size a panel's window opens at when the user detaches it: what the panel had in
 *  the editor window, floored at what it needs and at `DETACH_MIN_*`. */
export function detachSize(panel: PanelId, groupWidth: number, groupHeight: number): { width: number; height: number } {
	const spec = PANELS[panel];
	const size = (have: number, own: number | undefined, floor: number) =>
		Math.min(DETACH_MAX, Math.max(Number.isFinite(have) ? Math.round(have) : 0, own ?? 0, floor));
	return {
		width: size(groupWidth, spec.minimumWidth, DETACH_MIN_WIDTH),
		height: size(groupHeight, spec.minimumHeight, DETACH_MIN_HEIGHT)
	};
}

/** What a detached window is called in the taskbar and the window switcher. */
export function windowTitle(panels: readonly PanelId[]): string {
	if (panels.length === 0) return 'Kerf';
	return `${panels.map((p) => PANELS[p].title).join(' + ')} — Kerf`;
}

export type PanelLocation = 'grid' | 'popout' | 'floating' | 'edge';

/** Why `panel` cannot be moved into a window of its own, or `null` when it can (or is
 *  already in one, in which case moving it is giving it back).
 *
 *  The editor window keeps a panel of its own: a window with nothing in it has no
 *  menus to reach the others from. */
export function detachBlocked(panel: PanelId, where: Readonly<Partial<Record<PanelId, PanelLocation>>>): string | null {
	const at = where[panel];
	if (!at) return `The ${PANELS[panel].title} panel is not open — open it from the Window menu first`;
	if (at === 'popout') return null;
	const left = Object.entries(where).filter(([id, loc]) => id !== panel && loc === 'grid').length;
	if (left === 0) return 'The editor window keeps at least one panel';
	return null;
}

/** What dockview says when a window could not be used, as a sentence. */
export function describeFailure(reason: string): string {
	switch (reason) {
		case 'blocked':
			return 'The system would not open a window for the panel';
		case 'url-refused':
			return 'The panel window page was refused';
		case 'unscriptable':
			return 'The panel window could not be reached from the editor';
		case 'closed':
			return 'The panel window closed before it opened';
		default:
			return 'The panel window could not be opened';
	}
}

/** The labels of windows the page has announced and dockview has not yet opened, in
 *  the order they were announced. `window.open` carries no way to say which window it
 *  is for, so the backend hands them out in the same order and dockview opens one at a
 *  time: the first to open is the first announced. A window that was announced and
 *  never opened (dockview refused before calling `window.open`) is `drop`ped by its
 *  owner so it does not take the next one's label. */
export class LabelBook {
	readonly #waiting: string[] = [];

	expect(label: string): void {
		if (!this.#waiting.includes(label)) this.#waiting.push(label);
	}

	/** The label of the window that has just opened, if one was announced. */
	claim(): string | null {
		return this.#waiting.shift() ?? null;
	}

	drop(label: string): boolean {
		const at = this.#waiting.indexOf(label);
		if (at < 0) return false;
		this.#waiting.splice(at, 1);
		return true;
	}

	/** Every label still waiting, emptied. */
	clear(): string[] {
		return this.#waiting.splice(0);
	}

	get size(): number {
		return this.#waiting.length;
	}
}

interface Root {
	className: string;
	lang: string;
	style: { cssText: string };
}

/** Copy what the theme writes on `<html>` — its class (light / dark), its inline custom
 *  properties and `color-scheme` — onto a detached window's `<html>`. A window's custom
 *  properties are its own: without this it shows the stylesheet's defaults whatever
 *  theme is in use. Returns whether anything changed. */
export function mirrorRoot(from: Root, to: Root): boolean {
	let changed = false;
	if (to.className !== from.className) {
		to.className = from.className;
		changed = true;
	}
	if (to.style.cssText !== from.style.cssText) {
		to.style.cssText = from.style.cssText;
		changed = true;
	}
	if (to.lang !== from.lang) {
		to.lang = from.lang;
		changed = true;
	}
	return changed;
}

/** How far a window may sit from where it was asked to and still be taken as placed. */
export const PLACEMENT_SLACK_PX = 3;
/** …and how far off it can be and still be a platform's offset rather than somewhere else
 *  (the window manager put it elsewhere, or the user took hold of it). */
export const PLACEMENT_DRIFT_MAX_PX = 80;

/** Where to move a window that opened at `got` when it was asked to open at `want`, or
 *  `null` to leave it. The position the platform *reads* (`screenX`) is not always the
 *  one it is *set* to — a title bar, a shadow — and a layout saves the read one: a window
 *  reopened at it would creep by the difference at every launch. Setting it to `want`
 *  minus the error makes the read position `want`. Only a small, steady error is that; a
 *  large one is the window having been put somewhere else on purpose. */
export function positionCorrection(want: readonly [number, number], got: readonly [number, number]): [number, number] | null {
	const dx = got[0] - want[0];
	const dy = got[1] - want[1];
	if (![dx, dy].every(Number.isFinite)) return null;
	if (Math.abs(dx) <= PLACEMENT_SLACK_PX && Math.abs(dy) <= PLACEMENT_SLACK_PX) return null;
	if (Math.abs(dx) > PLACEMENT_DRIFT_MAX_PX || Math.abs(dy) > PLACEMENT_DRIFT_MAX_PX) return null;
	return [want[0] - dx, want[1] - dy];
}
