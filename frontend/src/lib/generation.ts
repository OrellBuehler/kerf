/* A monotonic version counter for guarding a piece of state against an
 * out-of-order async write — the two shapes that race in `EditorState`
 * (`state.svelte.ts`):
 *
 *  - a *sequence* guard, where the async call is the only writer and only the
 *    most recently started one should ever land (`select()`: click asset A —
 *    slow — then B — fast; B must win even though A resolves last). Call
 *    `advance()` when the call starts and again check `isCurrent()` against
 *    that value when it resolves.
 *  - a *snapshot* guard, where other writers exist beside the async fetch
 *    (`refreshTimeline()`: a GUI edit can commit while a refresh triggered by
 *    the backend's `project-changed` event is still in flight). Call `read()`
 *    when the fetch starts, and have every writer — including the fetch's own
 *    eventual commit — call `advance()`; a refresh only commits if `read()`'s
 *    snapshot is still current, so a local edit that landed in between always
 *    wins over the stale snapshot the refresh was carrying. */
export class Generation {
	#v = 0;

	/** Snapshot the current version, without advancing it. */
	read(): number {
		return this.#v;
	}

	/** Bump the version — call whenever the guarded state is actually written,
	 *  from any source. Returns the new value. */
	advance(): number {
		return ++this.#v;
	}

	/** Whether no `advance()` has happened since `snapshot` was taken. */
	isCurrent(snapshot: number): boolean {
		return snapshot === this.#v;
	}
}
