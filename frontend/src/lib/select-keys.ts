// A select that writes the project (the Deliver panel's Fit and Frame rate) must not write once per arrow press: a native
// `<select>` that is closed changes its value, and fires `change`, on every arrow, Home, End and Page key — three arrows are three
// revisions and three notices. The keys that only *step* are held; the list opens with Space / Alt+Down, its arrows move through
// the options without choosing, and Enter (or a click) chooses once. The radio chips hold the same line with `follow={false}`.

const STEPS = new Set(['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End', 'PageUp', 'PageDown']);

/** Whether a key press on a closed select would step to another option (and so choose it). Alt+Down opens the list and
 *  stays the browser's. */
export function stepsSelect(e: Pick<KeyboardEvent, 'key' | 'altKey' | 'ctrlKey' | 'metaKey'>): boolean {
	return STEPS.has(e.key) && !e.altKey && !e.ctrlKey && !e.metaKey;
}

/** `<select use:chooseFromList>`: arrows do not choose; the list does. */
export function chooseFromList(node: HTMLSelectElement) {
	const onKey = (e: KeyboardEvent) => {
		if (stepsSelect(e)) e.preventDefault();
	};
	node.addEventListener('keydown', onKey);
	return { destroy: () => node.removeEventListener('keydown', onKey) };
}
