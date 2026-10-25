/* The media bin's selection — pure. The bin picks assets the way the timeline picks clips (`selection.ts`: a click
 * replaces, Ctrl/Cmd toggles, Shift extends from the primary to the one clicked), over the assets the bin shows, in the
 * order it shows them. The primary is the asset the Inspector describes and the one a drag takes onto the timeline. */

import { clickSelect, normalize, pickMode, type Modifiers, type Selection } from './selection';

/** The selection after a click on `assetId`. `order` is the ids of the assets the bin shows, top to bottom: what a
 *  Shift-click runs across. An anchor (the primary) that the bin does not show — a filter hides it — leaves a Shift-click
 *  adding just the one. */
export function pickAsset(sel: Selection, assetId: string, mods: Modifiers, order: readonly string[]): Selection {
	return clickSelect(sel, assetId, pickMode(mods), order);
}

/** The selection with the assets the bin does not show (`visible`) taken out — a filter that hides some of a
 *  selection must not leave them in, or "Tag 5 assets" would name five when three are in view. When none of the
 *  selection is visible it is left alone: the Inspector keeps the asset it is on rather than going blank. */
export function restrictToVisible(sel: Selection, visible: ReadonlySet<string>): Selection {
	const kept = sel.ids.filter((id) => visible.has(id));
	if (kept.length === 0 || kept.length === sel.ids.length) return sel;
	return normalize(kept, sel.primary);
}

/** The selection as the editor holds it: the ids, and the primary (which `ids` must contain). */
export function selectionOf(ids: readonly string[], primary: string | null): Selection {
	if (ids.length === 0 && primary !== null) return { ids: [primary], primary };
	return normalize(ids, primary);
}

/** A selection of more than one asset: the bin's right-click acts on all of them. */
export function isMulti(sel: Selection): boolean {
	return sel.ids.length > 1;
}

/** The selection a bare Escape leaves: only the primary, so the Inspector keeps its asset. */
export function collapsed(sel: Selection): Selection {
	return sel.primary === null ? sel : { ids: [sel.primary], primary: sel.primary };
}
