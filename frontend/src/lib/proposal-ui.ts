/* What the page says and asks about the agent's staged proposal — pure. The proposal's own geometry is
 * `staged-diff.ts` and its wording `diff.ts`; this is the rest of the review: when throwing it away
 * needs a second thought, how it is named on a badge or a strip, which clips of the live cut it
 * touches (for the marks on the timeline before it is previewed), and what clicking one of its rows
 * does — the row of a clip that only exists in the proposal has nothing on the live cut to select,
 * so it opens the preview and selects it there. */

import type { DiffEntry, Timeline } from './types';

/** A proposal with more changes than this asks before it is thrown away; a small one is one click
 *  and an Undo-less toast, a large one is minutes of an agent's work. */
export const DISCARD_ASKS_ABOVE = 3;

export const discardNeedsConfirm = (changes: number): boolean => changes > DISCARD_ASKS_ABOVE;

/** Why an editing control is off while the proposal is previewed: the cut on screen is the agent's, not the project's. */
export const PREVIEW_READ_ONLY_REASON = 'Exit the proposal preview to edit';

const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? '' : 's'}`;

/** `1 change`, `4 changes`. */
export const changeWord = (n: number): string => plural(n, 'change');

/** The strip over the timeline: what is waiting. */
export const proposalLine = (n: number): string => `Agent proposes ${changeWord(n)}`;

/** The title-bar chip's tooltip (the chip's own words are `Review 4 changes`, `agent-status.ts`). */
export const badgeTip = (n: number): string => `The agent proposes ${changeWord(n)} — open the Agent panel to review them`;

/** The banner's counter tooltip: where the proposal changes the cut, and the keys that step between those places —
 *  the user's own (`keysFor`), so a rebound key is not described by the one it replaced. */
export function reviewStepTip(places: number, changes: number, prev: readonly string[], next: readonly string[]): string {
	const where = `The proposal changes the cut at ${plural(places, 'place')}; ${changeWord(changes)} in all.`;
	const keys =
		prev.length > 0 && next.length > 0
			? `${prev[0]} and ${next[0]} step between them.`
			: prev.length > 0
				? `${prev[0]} steps back between them.`
				: next.length > 0
					? `${next[0]} steps forward between them.`
					: '';
	return keys ? `${where} ${keys}` : where;
}

/** The clips and titles of the *live* cut that the proposal touches: moved, trimmed, changed or
 *  removed ones — everything but what it adds, which is not on the live cut to be marked. */
export function touchedOnLive(
	entries: readonly DiffEntry[],
	live: Timeline
): { clips: Set<string>; titles: Set<string> } {
	const clips = new Set<string>();
	const titles = new Set<string>();
	const here = new Set(live.tracks.flatMap((t) => t.clips.map((c) => c.id)));
	const titlesHere = new Set((live.overlays ?? []).map((o) => o.id));
	for (const e of entries) {
		if (e.clip_id && here.has(e.clip_id)) clips.add(e.clip_id);
		if (e.overlay_id && titlesHere.has(e.overlay_id)) titles.add(e.overlay_id);
	}
	return { clips, titles };
}

export interface ReviewStep {
	/** Where to put the playhead, or null when the entry has no place. */
	seek: number | null;
	/** Show the proposal first: the thing the row is about only exists in it. */
	preview: boolean;
	/** What to select once it is there (if it is: a removed clip is not on the proposed cut). */
	target: { kind: 'clip' | 'title'; id: string } | null;
}

const hasClip = (tl: Timeline, id: string) => tl.tracks.some((t) => t.clips.some((c) => c.id === id));
const hasTitle = (tl: Timeline, id: string) => (tl.overlays ?? []).some((o) => o.id === id);

/**
 * What clicking a row of the proposal does. `live` is the project's own cut and `shown` what the editor
 * is showing (the proposal while it is previewed). A clip or title the live cut has is selected where
 * it is; one it does not (an added clip) is selected in the preview, which is opened for it. While the
 * proposal is already showing, nothing is opened, and what is not on it (a removal) is not selected.
 */
export function reviewTarget(
	entry: Pick<DiffEntry, 'at' | 'clip_id' | 'overlay_id'>,
	ctx: { previewing: boolean; live: Timeline; shown: Timeline }
): ReviewStep {
	const seek = typeof entry.at === 'number' && Number.isFinite(entry.at) ? entry.at : null;
	const target: ReviewStep['target'] = entry.clip_id
		? { kind: 'clip', id: entry.clip_id }
		: entry.overlay_id
			? { kind: 'title', id: entry.overlay_id }
			: null;
	if (!target) return { seek, preview: false, target: null };
	const exists = (tl: Timeline) => (target.kind === 'clip' ? hasClip(tl, target.id) : hasTitle(tl, target.id));
	if (ctx.previewing) return { seek, preview: false, target: exists(ctx.shown) ? target : null };
	if (exists(ctx.live)) return { seek, preview: false, target };
	return { seek, preview: true, target };
}
