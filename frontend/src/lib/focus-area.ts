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
	ownerDocument?: { body?: unknown; documentElement?: unknown; querySelector?(selector: string): unknown } | null;
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
// place the user worked in* (it is, until they work in another panel). The last place is remembered from where the
// pointer was pressed and where focus went (`noteArea`, listened to in every window); a dialog, a menu, a toast or a
// title-bar button is no place — focus returning from one to the page leaves the last area as it was.

/** The content of a dock panel. */
export const DOCK_CONTENT = '.dv-content-container';
/** A dock group: its tabs and header, and its content. */
export const DOCK_GROUP = '.dv-groupview';
/** A dock tab names its panel in this attribute. */
export const PANEL_TAB = '[data-tab-panel-id]';
/** The menu bar's own menus: a chord there closes them and goes on, so they are as good as nothing in particular. */
export const BAR_MENU = '[role="menubar"], [role="menu"]';
/** The panels that are the editor: the timeline and the preview with its transport. Their tabs count as them. */
export const EDITOR_PANELS: ReadonlySet<string> = new Set(['timeline', 'preview']);

/** `editor`: the timeline or the preview (with its transport), and their dock tabs. `panel`: any other dock panel or its
 *  tabs, and the body of a detached window that holds none of the editor. `neutral`: anything else — the main window's
 *  body, the title bar's buttons, the status bar, a banner, a toast, a dialog, the menu bar's menus. */
export type FocusArea = 'editor' | 'panel' | 'neutral';

/** An event's target as an element — in any window: a detached panel's `Element` is not this window's, so `instanceof`
 *  would call every key there "nowhere in particular". */
export function elementOf(target: EventTarget | null): (Element & FocusTarget) | null {
	return target && typeof (target as Element).closest === 'function' ? (target as Element & FocusTarget) : null;
}

/** An element as far as the dock's tabs are read. */
interface Tabbed {
	getAttribute?(name: string): string | null;
	querySelector?(selector: string): unknown;
}

const panelOfTab = (tab: unknown): FocusArea => (EDITOR_PANELS.has((tab as Tabbed | null)?.getAttribute?.('data-tab-panel-id') ?? '') ? 'editor' : 'panel');

/** The editor window's own document, which a detached window's is not. A test says which it is. */
let mainDocument: unknown = typeof document === 'undefined' ? undefined : document;
export function setMainDocument(doc: unknown): void {
	mainDocument = doc;
}

export function areaOf(target: FocusTarget | null, main: unknown = mainDocument): FocusArea {
	if (!target) return 'neutral';
	const doc = target.ownerDocument;
	if (doc && (target === doc.body || target === doc.documentElement)) {
		// The page body of the editor window is nothing in particular. A detached window's is not: the panels in it have
		// the keyboard, and the body is theirs — unless the editor itself is what it holds.
		if (main === undefined || doc === main) return 'neutral';
		return doc.querySelector?.(EDITOR_SURFACE) ? 'editor' : 'panel';
	}
	if (target.closest(EDITOR_SURFACE) || target.querySelector?.(EDITOR_SURFACE)) return 'editor';
	// A dock tab names the panel it opens — the timeline's own tab is the timeline, before it has been activated.
	const tab = target.closest(PANEL_TAB);
	if (tab) return panelOfTab(tab);
	if (target.closest(BAR_MENU)) return 'neutral';
	if (target.closest(DOCK_CONTENT)) return 'panel';
	// The rest of a group — its header, the empty space beside the tabs — is whichever panel its active tab opens.
	const group = target.closest(DOCK_GROUP);
	if (group) return panelOfTab((group as Tabbed).querySelector?.('.dv-tab.dv-active-tab'));
	return 'neutral';
}

let lastArea: 'editor' | 'panel' = 'editor';

/** The pointer was pressed, or focus moved, to `target`: if that is a dock panel, it is where the user is working. */
export function noteArea(target: FocusTarget | null, main?: unknown): void {
	const area = areaOf(target, main);
	if (area === 'editor' || area === 'panel') lastArea = area;
}

/** The area the user last worked in. It starts as the timeline — a fresh start, an opened project — until they work in another. */
export const lastWorkedArea = (): 'editor' | 'panel' => lastArea;

/** Say where the user is working: the timeline takes a drop from the library, the focus moving to it by a key. */
export function setLastArea(area: 'editor' | 'panel'): void {
	lastArea = area;
}

/** Back to the start: the timeline is the last place worked in (a new project; a test's reset). */
export function forgetArea(): void {
	lastArea = 'editor';
}

/** Whether a key pressed with the focus on `target` may run an action that edits the cut: in the timeline or the preview,
 *  or on nothing in particular (the page body, the title bar, a dialog's return) with the timeline the last place worked
 *  in. A dock panel — the Inspector, the transcript, the dope sheet — never does. `last` is `lastWorkedArea()` unless a
 *  test says otherwise. */
export function mayEditCut(target: FocusTarget | null, last: 'editor' | 'panel' = lastArea, main?: unknown): boolean {
	const area = areaOf(target, main);
	if (area === 'editor') return true;
	return area === 'neutral' && last === 'editor';
}

/** What a key that edits the cut says when it is not the page's here — a dead key says why. `focusKey` is the key the user has
 *  for *Focus timeline*, if any. */
export function cutDeniedNotice(label: string, focusKey: string | null): string {
	return `${label} edits the timeline — click it first${focusKey ? `, or press ${focusKey}` : ''}`;
}

/** What to do with a `'cut'` key the area rule refused. `textSelected`: the page has a text selection (the user is working
 *  with text, and the browser's own copy, cut and select-all are theirs). Select all is still taken, so it does not
 *  highlight the whole page; copy and cut are left to the browser. No notice while text is selected. */
export function cutDenied(id: string, o: { textSelected: boolean }): { notice: boolean; preventDefault: boolean } {
	return { notice: !o.textSelected, preventDefault: id === 'edit.selectAll' };
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
