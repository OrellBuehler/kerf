// Focus the timeline — pure finder. A keyboard-only user in another panel needs one key that brings the keyboard back to
// the timeline: its clips are one tab stop (`rovingId`), so that is the clip to land on; with no clip button in view the
// timeline's own toolbar has the first button. The glue that raises the panel and focuses what this finds is `focusTimeline`
// in `menu-commands.ts`.

/** The part of a document this looks through (so a test needs no DOM). */
export interface Searchable {
	querySelector(selector: string): Element | null;
}

/** The timeline's root: the editor surface that holds the lanes (the preview is one too, and has none). */
export function timelineRoot(doc: Searchable): Element | null {
	const lane = doc.querySelector('[data-lane]');
	return lane?.closest('[data-editor-surface]') ?? null;
}

/** What gets the focus: the clip that is the lanes' tab stop, else any clip button, else the toolbar's first button. */
export function timelineStop(root: Searchable): Element | null {
	return (
		root.querySelector('.kclip:not([tabindex="-1"])') ??
		root.querySelector('.kclip') ??
		root.querySelector('[role="toolbar"] button:not([disabled])')
	);
}
