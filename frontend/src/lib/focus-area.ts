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

/** The keys that scroll a page. */
export const isPagingKey = (key: string): boolean => key === 'PageUp' || key === 'PageDown';
