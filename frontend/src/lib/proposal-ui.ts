/* What the page says and asks about the agent's staged proposal — pure. The proposal's own geometry is
 * `staged-diff.ts` and its wording `diff.ts`; this is the rest of the review: when throwing it away
 * needs a second thought, how it is named on a badge or a strip, which clips of the live cut it
 * touches (for the marks on the timeline before it is previewed), and what clicking one of its rows
 * does — the row of a clip that only exists in the proposal has nothing on the live cut to select,
 * so it opens the preview and selects it there. */

import { diffHeadline } from './diff';
import { formatTime, type TimeMode } from './time-format';
import type { DiffEntry, Timeline, TimelineDiff } from './types';

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

// ---- how a proposal reads ----------------------------------------------------------------------------------------

/** A time as kerf-core prints one in a change's sentence: `m:ss.d` (`fmt_time`), minutes unbounded, seconds to the tenth. */
const CORE_TIME = /(^|[^\d:.])(\d+):([0-5]\d\.\d)(?![\d:]|\.\d)/g;

/**
 * A change's sentence with its times in the spelling the rest of the editor uses. The backend phrases a change
 * (`Trimmed clip on V1 at 0:12.5 — 12.5s → 9.0s`) in `m:ss.d` while the transport, the ruler and the Inspector read
 * `00:00:12:15`; the sentence is the backend's but the times in it are plain tokens, so they are respelled
 * (`timeDisplay`'s mode: `seconds` leaves the sentence as it came). Text between curly quotes is the user's own — a title
 * that says "meet at 3:45.0" — and is never touched. `exact` are the times the sentence is about, in full precision (the
 * entry's `at`, the cut's two durations): a token that is one of them to the tenth is shown at the frame, not at the tenth
 * the backend rounded it to.
 */
export function respellTimes(text: string, fps: number, mode: TimeMode, exact: readonly number[] = []): string {
	if (mode === 'seconds') return text;
	return text
		.split(/(“[^”]*”)/)
		.map((part, i) => {
			if (i % 2 === 1) return part;
			return part.replace(CORE_TIME, (_all, lead: string, minutes: string, rest: string) => {
				const printed = Number(minutes) * 60 + Number(rest);
				const precise = exact.find((e) => Number.isFinite(e) && Math.abs(e - printed) <= 0.05 + 1e-9);
				return lead + formatTime(precise ?? printed, fps, mode);
			});
		})
		.join('');
}

/** A change's own line: its sentence and its detail, with the times respelled. */
export function changeLine(entry: Pick<DiffEntry, 'summary' | 'detail' | 'at'>, fps: number, mode: TimeMode): { summary: string; detail: string | null } {
	const exact = typeof entry.at === 'number' ? [entry.at] : [];
	return {
		summary: respellTimes(entry.summary, fps, mode, exact),
		detail: entry.detail ? respellTimes(entry.detail, fps, mode, exact) : null
	};
}

/** The headline's parts — `4 changes`, `00:00:20:15 → 00:00:17:00 (-3.5s)`, `4 → 3 clips` — in the editor's times
 *  (`diffHeadline` is the Rust headline; these are the same words, respelled). */
export function headlineParts(diff: TimelineDiff, fps: number, mode: TimeMode): string[] {
	return respellTimes(diffHeadline(diff), fps, mode, [diff.duration_before, diff.duration_after]).split(' · ');
}

/** Where the preview's stepper stands: `Place 1 of 3` (`index` 0 is before the first), and — when several changes share a
 *  place, which is why "1 of 3" and "4 changes" are both true — the sentence that says so. */
export function placeStep(places: number, index: number): string {
	if (places === 0) return 'No places';
	return index > 0 ? `Place ${index} of ${places}` : plural(places, 'place');
}

export function placesNote(places: number, changes: number): string | null {
	if (places === 0 || places === changes) return null;
	return `${changeWord(changes)} in ${plural(places, 'place')}`;
}

/** What a stale proposal tells the user: how far the cut has moved on since the agent branched from it. `head` is the
 *  seq of the revision the cut is at, `base` the one the proposal started from; the revisions are numbered one after
 *  another, so the difference is how many edits ago it was. A negative one is the user having gone *back* past it. */
export interface StaleWords {
	title: string;
	body: string;
	/** What the way back does, for its button's tooltip; `null` when the cut is not at a different revision (nothing to return to). */
	back: { label: string; tip: string } | null;
}

export function staleWords(base: number, head: number | null): StaleWords {
	const title = 'Your cut has changed since this was proposed';
	const apply = 'Applying it puts the agent’s version in place of the cut you have now.';
	const gap = head === null ? 0 : head - base;
	if (gap > 0) {
		return {
			title,
			body: `You made ${plural(gap, 'edit')} after the agent staged it. ${apply} Discard it, or go back to the cut it started from.`,
			back: {
				label: 'Go back to that cut',
				tip: `Go back ${plural(gap, 'edit')} so the proposal fits again. Your ${gap === 1 ? 'edit stays' : 'edits stay'} in History until you make a new edit.`
			}
		};
	}
	if (gap < 0) {
		return {
			title,
			body: `You went back ${plural(-gap, 'edit')} past the cut the agent started from. ${apply}`,
			back: {
				label: 'Return to that cut',
				tip: `Go forward ${plural(-gap, 'edit')} to the cut the proposal started from, so it fits again.`
			}
		};
	}
	return { title, body: `The cut is not the one the agent started from. ${apply}`, back: null };
}

/** The calm toast for a proposal that has just arrived. A long note is cut, since a toast is one line. */
export function arrivalNotice(staged: { diff: { entries: readonly unknown[] }; note?: string | null }): { text: string; action: string } {
	const n = staged.diff.entries.length;
	const note = staged.note?.trim();
	const short = note && note.length > 48 ? `${note.slice(0, 47)}…` : note;
	const what = n === 0 ? 'The agent has a proposal for you' : `The agent proposed ${changeWord(n)}`;
	return { text: short ? `${what} — ${short}` : what, action: 'Review' };
}
