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

// ---- a strip that scrolls ---------------------------------------------------------

/** A position on the screen as a position in the strip's content: the content starts at the strip's left edge less what has
 *  scrolled away. The spans of the buttons and the pointer are both read this way, so a drag — and the marker, which is drawn
 *  inside the scrolling content — is right however far the strip has been scrolled. */
export function contentX(clientX: number, stripLeft: number, scrollLeft: number): number {
	return clientX - stripLeft + scrollLeft;
}

/** How a drag near an edge of the strip scrolls it: by `step` toward an edge the pointer is within `edge` px of, else not. */
export function edgeNudge(clientX: number, strip: Span, edge = 28, step = 14): number {
	if (clientX < strip.left + edge) return -step;
	if (clientX > strip.right - edge) return step;
	return 0;
}

/** Which ends of a scrolling strip have more of it behind them (a pixel of slack: scroll positions are fractional). */
export interface Fade {
	left: boolean;
	right: boolean;
}

export function fadeOf(scrollLeft: number, clientWidth: number, scrollWidth: number): Fade {
	return { left: scrollLeft > 1, right: scrollLeft + clientWidth < scrollWidth - 1 };
}

/** The mask that fades the ends that go on, `edge` px of it, in a colour whose alpha is all that is read; '' when neither does. */
export function fadeMask(fade: Fade, edge = 28): string {
	if (!fade.left && !fade.right) return '';
	const from = fade.left ? `transparent 0, var(--surface-app) ${edge}px` : 'var(--surface-app) 0';
	const to = fade.right ? `var(--surface-app) calc(100% - ${edge}px), transparent 100%` : 'var(--surface-app) 100%';
	const gradient = `linear-gradient(to right, ${from}, ${to})`;
	return `-webkit-mask-image:${gradient};mask-image:${gradient};`;
}
