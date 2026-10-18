/* Picking some of an agent's changes instead of all of them — pure. The card's checkboxes choose `entryId`s
 * (`staged-subset.ts`: the change's stable name, which is what `apply_staged_subset` is sent) and the Apply button
 * says how many. A picture and its sound are one piece of material, so picking one of a linked pair brings the
 * other along; the backend does that for real and reports it (`SubsetApplied.auto_included`), and `comingAlong`
 * is the same closure (`closeOver`, shared with the harness' `mergeSubset`) read off the cuts the page has, so the
 * card can say so *before* the press. The track a picked clip lands on comes along too, and so do the moves that take
 * clips off a track picked for removal. */

import { closeOver, entryId } from './staged-subset';
import type { DiffEntry, StagedAssetChange, Timeline } from './types';

/** One row of the review: a change of the cut, or a held-back change to the library. */
export interface PickRow {
	id: string;
	summary: string;
	entry: DiffEntry | null;
}

/** Every pickable change of a proposal, in the order the card lists them. */
export function pickRows(entries: readonly DiffEntry[], assets: readonly StagedAssetChange[] = []): PickRow[] {
	return [
		...entries.map((entry): PickRow => ({ id: entryId(entry), summary: entry.summary, entry })),
		...assets.map((a): PickRow => ({ id: a.id, summary: a.summary, entry: null }))
	];
}

/** `picks` with `id` flipped. */
export function toggled(picks: ReadonlySet<string>, id: string): Set<string> {
	const next = new Set(picks);
	if (next.has(id)) next.delete(id);
	else next.add(id);
	return next;
}

/** Whether a group is picked in full, in part or not at all (a header's checkbox). */
export function groupState(ids: readonly string[], picks: ReadonlySet<string>): 'none' | 'some' | 'all' {
	const n = ids.filter((id) => picks.has(id)).length;
	return n === 0 ? 'none' : n === ids.length ? 'all' : 'some';
}

/** A header's checkbox: a group picked in full is cleared, anything else is picked in full. */
export function toggledGroup(picks: ReadonlySet<string>, ids: readonly string[]): Set<string> {
	const next = new Set(picks);
	if (groupState(ids, picks) === 'all') for (const id of ids) next.delete(id);
	else for (const id of ids) next.add(id);
	return next;
}

/** What is still in the proposal: a pick for a change that has since gone (the agent staged more, an apply
 *  took some) is dropped. Returns `picks` itself when nothing changed. */
export function keepPresent(picks: ReadonlySet<string>, rows: readonly Pick<PickRow, 'id'>[]): ReadonlySet<string> {
	const present = new Set(rows.map((r) => r.id));
	const kept = [...picks].filter((id) => present.has(id));
	return kept.length === picks.size ? picks : new Set(kept);
}

/** The cuts a proposal is read against. `base` is the cut the proposal branched from; the page does not have it, and
 *  the live cut is it unless the user edited since (a stale proposal), so it defaults to `live`. */
export interface PickCuts {
	live: Timeline | null | undefined;
	base?: Timeline | null;
	proposed?: Timeline | null;
}

const NO_CUT: Timeline = { tracks: [] };

/** The changes that would be applied without being picked, and why — `{ id → because }`. The same closure the
 *  backend takes (`staged-subset.ts`'s `closeOver`, the port of `Timeline::merge_subset`), read off the cuts the
 *  page has; the backend's answer after the press is the authority. */
export function comingAlong(entries: readonly DiffEntry[], picks: ReadonlySet<string>, cuts: PickCuts): Map<string, string> {
	const out = new Map<string, string>();
	if (picks.size === 0) return out;
	const ids = entries.map(entryId);
	const chosen = new Set<number>();
	ids.forEach((id, i) => picks.has(id) && chosen.add(i));
	if (chosen.size === 0) return out;
	const live = cuts.live ?? NO_CUT;
	const base = cuts.base ?? live;
	const proposed = cuts.proposed ?? live;
	for (const [i, why] of closeOver(entries, chosen, [live, base, proposed], base)) out.set(ids[i], why);
	return out;
}

/** What the Apply button says. Nothing picked is the whole proposal. */
export function applyLabel(picked: number, total: number, busy: boolean): string {
	if (busy) return 'Applying…';
	if (picked === 0 || picked >= total) return 'Apply';
	return `Apply ${picked} of ${total}`;
}

/** The line over the list while a pick is in progress. */
export function pickLine(picked: number, along: number, total: number): string {
	if (picked === 0) return 'Pick changes to apply only those, or apply everything.';
	const base = `${picked} of ${total} picked`;
	return along > 0 ? `${base} · ${along} more ${along === 1 ? 'comes' : 'come'} along` : base;
}

/** The toast after a partial accept: core's own sentence (`SubsetApplied::message`). Said as a warning when
 *  some picks no longer applied. */
export function subsetTone(done: { skipped: readonly unknown[] }): 'success' | 'warning' {
	return done.skipped.length > 0 ? 'warning' : 'success';
}
