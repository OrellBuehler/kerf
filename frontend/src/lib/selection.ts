/* Clip selection as a set — pure. The editor keeps `selectedClipIds` (the whole
 * selection) and `selectedClipId` (the *primary*: the clip the Inspector edits and
 * range-select anchors on); every way of changing them is one of these functions,
 * so a click, a Shift-click and a marquee agree about what they leave behind.
 *
 * Two invariants hold for every `Selection` returned: no id appears twice, and the
 * primary is a member of the set (or `null` when the set is empty). */

export interface Selection {
	ids: readonly string[];
	primary: string | null;
}

export const NO_SELECTION: Selection = { ids: [], primary: null };

/** How a click on a clip combines with what is selected: a plain click replaces,
 *  Ctrl/Cmd toggles that one clip, Shift extends along its track from the primary. */
export type PickMode = 'replace' | 'toggle' | 'range';

/** How a marquee combines with what was selected when it started: a plain drag
 *  replaces, Shift adds what it touches, Ctrl/Cmd toggles each clip it touches. */
export type MarqueeMode = 'replace' | 'add' | 'toggle';

/** The modifier keys of a pointer event. */
export interface Modifiers {
	shiftKey: boolean;
	ctrlKey: boolean;
	metaKey: boolean;
}

/** The marquee mode a press with these modifiers asks for (Shift wins over Ctrl/Cmd). */
export function marqueeMode(m: Modifiers): MarqueeMode {
	return m.shiftKey ? 'add' : m.ctrlKey || m.metaKey ? 'toggle' : 'replace';
}

/** The click mode a press with these modifiers asks for. */
export function pickMode(m: Modifiers): PickMode {
	return m.shiftKey ? 'range' : m.ctrlKey || m.metaKey ? 'toggle' : 'replace';
}

const unique = (ids: readonly string[]): string[] => [...new Set(ids)];

/** `sel` with duplicates dropped and a primary that is a member: kept when it is
 *  one, else the last of the set. */
export function normalize(ids: readonly string[], primary: string | null): Selection {
	const set = unique(ids);
	if (set.length === 0) return NO_SELECTION;
	return { ids: set, primary: primary !== null && set.includes(primary) ? primary : set[set.length - 1] };
}

/** Whether two selections hold the same clips (order aside). */
export function sameIds(a: readonly string[], b: readonly string[]): boolean {
	if (a.length !== b.length) return false;
	const set = new Set(a);
	return b.every((id) => set.has(id));
}

/**
 * The selection after a click on `clipId`. `trackOrder` is the ids of the clips
 * on the clicked clip's track, in order — what a range runs across; `null` when
 * unknown. A range needs its anchor (the primary) on the same track: without one
 * the click simply adds the clip, which is what "extend" falls back to.
 */
export function clickSelect(
	sel: Selection,
	clipId: string,
	mode: PickMode,
	trackOrder: readonly string[] | null = null
): Selection {
	if (mode === 'toggle') {
		if (sel.ids.includes(clipId)) {
			const rest = sel.ids.filter((id) => id !== clipId);
			// Dropping the primary hands the Inspector whatever is left.
			return normalize(rest, sel.primary === clipId ? null : sel.primary);
		}
		return normalize([...sel.ids, clipId], clipId);
	}
	if (mode === 'range') {
		const anchor = sel.primary !== null && trackOrder ? trackOrder.indexOf(sel.primary) : -1;
		const to = trackOrder ? trackOrder.indexOf(clipId) : -1;
		if (trackOrder && anchor >= 0 && to >= 0) {
			const [lo, hi] = anchor <= to ? [anchor, to] : [to, anchor];
			return normalize([...sel.ids, ...trackOrder.slice(lo, hi + 1)], clipId);
		}
		return normalize([...sel.ids, clipId], clipId);
	}
	return { ids: [clipId], primary: clipId };
}

/**
 * The selection a marquee that touches `hits` leaves, given `base` — the selection
 * as it was when the drag began. It is always computed from that base, so the
 * rectangle can grow and shrink without drift. The primary is the last clip the
 * marquee newly brought in; when it brought none in, the one that was already
 * primary stays while it is still selected.
 */
export function marqueeSelect(base: Selection, hits: readonly string[], mode: MarqueeMode): Selection {
	const hit = new Set(hits);
	let ids: string[];
	if (mode === 'replace') ids = unique(hits);
	else if (mode === 'add') ids = unique([...base.ids, ...hits]);
	else ids = [...base.ids.filter((id) => !hit.has(id)), ...unique(hits).filter((id) => !base.ids.includes(id))];
	const was = new Set(base.ids);
	const added = hits.filter((id) => ids.includes(id) && !was.has(id));
	return normalize(ids, added.length > 0 ? added[added.length - 1] : base.primary);
}

/** `sel` without the ids `exists` says are gone (a clip another edit removed). */
export function pruneSelection(sel: Selection, exists: (id: string) => boolean): Selection {
	if (sel.ids.every(exists)) return sel;
	return normalize(sel.ids.filter(exists), sel.primary);
}
