// The end of an edit to one of a title's fields, pure. A field's `change` comes with its blur, and the click that blurs it
// is often the click that selects another title: by then the selection has moved and the field has been drawn again with
// the new title's value. So what is written is built from the title the edit *began on* (the scope the field noted when it
// took focus, and what was typed into it, `FieldEdits`), and what a field goes back to when there is nothing to write is
// the title it is *drawn from now* — the row on screen — which is not the one the edit began on.

import type { FieldEdits, FieldEntry } from './field-edit';

export interface TitleEditor<S, T extends { id: string }> {
	edits: FieldEdits<S, object>;
	/** The title a scope names, as the cut has it now (the one an edit began on). */
	titleOf: (scope: S) => T | undefined;
	/** The title the panel is showing now: the one the field is drawn from. */
	shownTitle: () => T | undefined;
	/** An edit is in flight, or the proposal is on screen: nothing is written. */
	locked: () => boolean;
}

/** What the page has of a field: its text, and whether it is still on the page. */
export interface FieldElement {
	value: string;
	isConnected: boolean;
}

/** End the edit of `el`: `now` is the scope and the text as they are by now, which stand in for what was not noted (a
 *  select, which changes at once). Returns the title to write and what, or null — nothing to write (the change is none,
 *  the cut is locked, the title is gone) — in which case the field goes back to what the title it is drawn from holds. */
export function commitTitleField<S, T extends { id: string }, P>(
	editor: TitleEditor<S, T>,
	el: FieldElement,
	now: FieldEntry<S>,
	patch: (typed: string, began: T) => P | null,
	shown?: (title: T) => string
): { id: string; patch: P } | null {
	const { scope, text } = editor.edits.end(el, now);
	const began = editor.titleOf(scope);
	if (!began) return null;
	const next = editor.locked() ? null : patch(text, began);
	if (next) return { id: began.id, patch: next };
	const showing = editor.shownTitle();
	if (shown && showing && el.isConnected) el.value = shown(showing);
	return null;
}
