/* Which window a piece of the UI is in, and how to listen to it, measure it and
   observe it from there. See `windows.svelte.ts` for why that is a question at all:
   a detached panel is the editor's own code running against another window's
   document.

   Rules of thumb for a panel:
   - the window of an element is `windowOf(el)`, never `window` — for its size, its
     `devicePixelRatio`, where an overlay is placed, where an event listens;
   - the document of an element is `documentOf(el)` — for `querySelector`,
     `elementsFromPoint`, `getElementById`, anything appended to `body`;
   - an observer made with the editor window's constructor sees a detached
     window's element wrongly (an `IntersectionObserver` reports it not
     intersecting for good), so make it with `resizeObserverFor` /
     `intersectionObserverFor`;
   - what a window-level listener hears is `onWindow` (`window-events.ts`), an
     attachment that follows the element to whichever window it is in.

   Free of runes on purpose: `drag.ts` and the pure modules use it, and are tested
   without them. */

type Anchor = Pick<Node, 'ownerDocument' | 'nodeType'> | null | undefined;

/** The document `node` is in: the editor's when there is no node. */
export function documentOf(node?: Anchor): Document {
	if (node) {
		// A document is its own owner's null; its window is its `defaultView`.
		const doc = node.nodeType === 9 ? (node as unknown as Document) : node.ownerDocument;
		if (doc) return doc;
	}
	return document;
}

/** The window `node` is in now. The editor's when there is no node, or it is in no
 *  document. Read it when it is needed: a panel can be moved to another window
 *  between two reads. */
export function windowOf(node?: Anchor): Window {
	return documentOf(node).defaultView ?? window;
}

/** `ResizeObserver` from `node`'s own window. */
export function resizeObserverFor(node: Anchor, cb: ResizeObserverCallback): ResizeObserver {
	return new (windowOf(node) as unknown as typeof globalThis).ResizeObserver(cb);
}

/** `IntersectionObserver` from `node`'s own window. */
export function intersectionObserverFor(
	node: Anchor,
	cb: IntersectionObserverCallback,
	init?: IntersectionObserverInit
): IntersectionObserver {
	return new (windowOf(node) as unknown as typeof globalThis).IntersectionObserver(cb, init);
}

/** Handlers by event name; a `capture` suffix listens in the capture phase, as
 *  `<svelte:window onpointerdowncapture>` does. */
export type WindowHandlers = Record<string, (e: never) => void>;

/** Split `pointerdowncapture` into the event and the phase. */
export function parseEventKey(key: string): { type: string; capture: boolean } {
	return key.endsWith('capture') && key.length > 'capture'.length
		? { type: key.slice(0, -'capture'.length), capture: true }
		: { type: key, capture: false };
}
