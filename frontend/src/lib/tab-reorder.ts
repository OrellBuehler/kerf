// Dragging a workspace's button along the title bar to put it elsewhere (`WorkspaceTabs.svelte`). A pointer drag, never
// the browser's own: the app's drag-drop handler, kept on for dropping files, leaves the webview no in-page HTML5 drag on
// Windows (see `dock-drag.ts`). The geometry is here so it is tested without a DOM.

/** How far the pointer has to go from where it was pressed before a press is a drag and not a click, in px. */
export const DRAG_THRESHOLD = 5;

export function pastThreshold(dx: number, dy: number): boolean {
	return Math.hypot(dx, dy) >= DRAG_THRESHOLD;
}

export interface Span {
	left: number;
	right: number;
}

/** Where a button dragged to pointer position `x` lands: the place it takes among the buttons once it is there (0 is
 *  first). `others` are the spans of the buttons that are *not* being dragged, in order; it lands before the first one whose
 *  middle is to the right of the pointer, and last when none is. */
export function dropIndex(others: readonly Span[], x: number): number {
	let at = 0;
	while (at < others.length && x > (others[at].left + others[at].right) / 2) at++;
	return at;
}

/** Where the marker that says where a button will land is drawn, as an x on the same scale as `others`: the left edge of the
 *  button it lands before, the right edge of the last when it lands last. Null when there is nothing to put it beside. */
export function markerX(others: readonly Span[], at: number): number | null {
	if (others.length === 0) return null;
	return at >= others.length ? others[others.length - 1].right : others[at].left;
}
