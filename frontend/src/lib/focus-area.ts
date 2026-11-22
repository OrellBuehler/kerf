/* Which part of the window a key press belongs to — pure. A key that scrolls (Page Up / Page Down) is
 * the browser's for whatever panel has the keyboard: the Agent queue, the Library, a list. The page
 * only takes it for an action — stepping through a proposal's changes — when the focus is on the
 * editor surface itself: nothing in particular (the page body), the timeline or the preview.
 * Those panels mark their root `data-editor-surface`. */

/** The panels that are the editor surface. */
export const EDITOR_SURFACE = '[data-editor-surface]';

/** The slice of an element this reads (so a test needs no DOM). */
export interface FocusTarget {
	closest(selector: string): unknown;
	querySelector?(selector: string): unknown;
	ownerDocument?: { body?: unknown; documentElement?: unknown } | null;
}

/** Whether `target` is the editor surface: no element, the page itself (its body or root, in whichever
 *  window it is), something inside a panel that marks itself one, or the dock's own container around
 *  one — a click on empty space in the timeline puts the focus on the container the panel sits in, which
 *  holds the surface and nothing else. */
export function onEditorSurface(target: FocusTarget | null): boolean {
	if (!target) return true;
	const doc = target.ownerDocument;
	if (doc && (target === doc.body || target === doc.documentElement)) return true;
	return !!target.closest(EDITOR_SURFACE) || !!target.querySelector?.(EDITOR_SURFACE);
}

// ---- which area the keyboard is in, and whether it may edit the cut ----------------------------------------------
//
// A key meant for a panel must never reach a clip. A panel that removes the element that has the focus (a list row
// that re-renders, a button that is gone) leaves the focus on the page body, and the next Delete or ⌘X is then the
// page's — and deletes the selected clip. So an action that edits the cut (`ActionDef.edits === 'cut'`) is the page's
// only while the keyboard is in the timeline or the preview, or on nothing in particular *and the timeline was the last
// place the user worked in*. The last place is remembered from where the pointer was pressed and where focus went
// (`noteArea`, listened to in every window); a dialog, a menu or a toast is no place — focus returning from one to the
// page body leaves the last area as it was.

/** What a dock panel's content, or its tabs, sit in. */
export const DOCK_PANEL = '.dv-content-container, .dv-groupview';
/** The menu bar's own menus: a chord there closes them and goes on, so they are as good as nothing in particular. */
export const BAR_MENU = '[role="menubar"], [role="menu"]';

/** `editor`: the timeline or the preview (with its transport). `panel`: any other dock panel or a panel's tabs.
 *  `neutral`: nothing in particular — the page body, or the menu bar's menus. `chrome`: anything else (the title
 *  bar's buttons, the status bar, a banner, a toast, a dialog). */
export type FocusArea = 'editor' | 'panel' | 'neutral' | 'chrome';

/** An event's target as an element — in any window: a detached panel's `Element` is not this window's, so `instanceof`
 *  would call every key there "nowhere in particular". */
export function elementOf(target: EventTarget | null): (Element & FocusTarget) | null {
	return target && typeof (target as Element).closest === 'function' ? (target as Element & FocusTarget) : null;
}

export function areaOf(target: FocusTarget | null): FocusArea {
	if (!target) return 'neutral';
	const doc = target.ownerDocument;
	if (doc && (target === doc.body || target === doc.documentElement)) return 'neutral';
	if (target.closest(EDITOR_SURFACE) || target.querySelector?.(EDITOR_SURFACE)) return 'editor';
	if (target.closest(BAR_MENU)) return 'neutral';
	if (target.closest(DOCK_PANEL)) return 'panel';
	return 'chrome';
}

let lastArea: 'editor' | 'panel' | null = null;

/** The pointer was pressed, or focus moved, to `target`: if that is a dock panel, it is where the user is working. */
export function noteArea(target: FocusTarget | null): void {
	const area = areaOf(target);
	if (area === 'editor' || area === 'panel') lastArea = area;
}

/** The panel area the user last worked in (null before they have). */
export const lastWorkedArea = (): 'editor' | 'panel' | null => lastArea;

/** Forget where the user was working — a test's reset. */
export function forgetArea(): void {
	lastArea = null;
}

/** Whether a key pressed with the focus on `target` may run an action that edits the cut: in the timeline or the preview,
 *  or on nothing in particular with the timeline the last place worked in. `last` is `lastWorkedArea()` unless a test
 *  says otherwise. */
export function mayEditCut(target: FocusTarget | null, last: 'editor' | 'panel' | null = lastArea): boolean {
	const area = areaOf(target);
	if (area === 'editor') return true;
	return area === 'neutral' && last === 'editor';
}

/** The keys that scroll a page. */
export const isPagingKey = (key: string): boolean => key === 'PageUp' || key === 'PageDown';

/** The keys that scroll a page by lines or by pages. */
export const isScrollKey = (key: string): boolean => isPagingKey(key) || key === 'ArrowUp' || key === 'ArrowDown';

/** Whether an action pressed on `key` is the page's only from the editor surface: the browser's for whichever
 *  panel has the keyboard otherwise. A proposal's change steps on Page Up / Down, and moving to the next edit
 *  point or about the clips on ↑ ↓, must not stop the Library, the Agent queue or the Inspector scrolling. */
export function needsEditorSurface(id: string, key: string): boolean {
	if (id.startsWith('review.')) return isPagingKey(key);
	if (id === 'playback.prevEdit' || id === 'playback.nextEdit' || id.startsWith('select.')) return isScrollKey(key);
	return false;
}
