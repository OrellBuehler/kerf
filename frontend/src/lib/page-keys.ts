/* Which keys the page's shortcut handler leaves to the control that has the keyboard — pure, so the list is tested. The page runs
 * a shortcut for almost any key (J, K, L shuttle even after a fader was clicked), but a key that *operates* the focused control is
 * the control's: Space and Enter press a button, a summary, a tab; the arrows move a slider.
 *
 * A slider is not pressed by Space (nothing happens to it), so Space still plays with a fader, a colour wheel or a grade's lightness
 * focused — it used to do nothing there. And Delete and Backspace are not a slider's keys either, but must never delete the clip a
 * fader was set on (a control with a delete of its own, the colour wheel's reset, stops them before they get here). */

export interface KeyTarget {
	/** The target is, or is inside, a button, an input, a summary, a slider, a tab or a menu item. */
	operable: boolean;
	/** The target is, or is inside, a `role="slider"`. */
	slider: boolean;
}

/** Whether the page leaves `key` to the focused control instead of running whatever it is bound to. */
export function leavesKeyToControl(key: string, on: KeyTarget): boolean {
	const k = key.toLowerCase();
	if (on.slider && (k === 'delete' || k === 'backspace')) return true;
	const operates = (k === ' ' && !on.slider) || k === 'enter' || k.startsWith('arrow');
	return operates && on.operable;
}
