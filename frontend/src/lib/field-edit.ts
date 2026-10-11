/** What a plain field (a text or number input) was editing, so its commit goes where the edit began.
 *
 *  A field's `change` comes with its blur. The click that blurs it is often the click that selects
 *  another clip or title, so by then the selection has moved on *and* the field has been re-rendered
 *  with the new selection's value. Neither the selection nor the field's value is what was edited:
 *  the scope is noted when the field takes focus, and what was typed on every `input`. */
export interface FieldEntry<S> {
	scope: S;
	text: string;
}

export class FieldEdits<S, F extends object = HTMLElement> {
	#by = new WeakMap<F, { scope: S; text: string | null }>();

	/** The field took focus: what it is on now is what its edit is on. */
	focus(field: F, scope: S): void {
		this.#by.set(field, { scope, text: null });
	}

	/** Something was typed, or stepped, into the field. */
	input(field: F, text: string): void {
		const edit = this.#by.get(field);
		if (edit) edit.text = text;
	}

	/** The field committed. `now` stands in for whatever was not seen: a field that was never focused
	 *  first, or a checkbox or a select, which have no typed text and change at once. */
	end(field: F, now: FieldEntry<S>): FieldEntry<S> {
		const edit = this.#by.get(field);
		const text = edit?.text ?? now.text;
		if (edit) edit.text = null;
		return { scope: edit?.scope ?? now.scope, text };
	}
}
