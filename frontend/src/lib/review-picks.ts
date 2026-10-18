/* Picking some of an agent's changes instead of all of them — pure. The card's checkboxes choose `entryId`s
 * (`staged-subset.ts`: the change's stable name, which is what `apply_staged_subset` is sent) and the Apply button
 * says how many. A picture and its sound are one piece of material, so picking one of a linked pair brings the
 * other along; the backend does that for real and reports it (`SubsetApplied.auto_included`), and `comingAlong`
 * is the same rule read off the two cuts the page has (the live one and the proposal) so the card can say so
 * *before* the press. The track a picked clip lands on comes along too. */

import { entryId } from './staged-subset';
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

/** Link ids of the clips of a cut, by clip id. */
function linkOf(cut: Timeline | null | undefined): Map<string, string> {
	const out = new Map<string, string>();
	for (const t of cut?.tracks ?? []) for (const c of t.clips) if (c.link_id) out.set(c.id, c.link_id);
	return out;
}

/** The changes that would be applied without being picked, and why — `{ id → because }`. Read off the live
 *  cut and the proposal; the backend's answer after the press is the authority. */
export function comingAlong(
	entries: readonly DiffEntry[],
	picks: ReadonlySet<string>,
	cuts: readonly (Timeline | null | undefined)[]
): Map<string, string> {
	const out = new Map<string, string>();
	if (picks.size === 0) return out;
	const links = cuts.map(linkOf);
	const groupOf = (clip: string) => links.map((l) => l.get(clip)).find((g) => g !== undefined);
	const ids = entries.map(entryId);
	const chosen = new Set(ids.filter((id) => picks.has(id)));
	for (let again = true; again; ) {
		again = false;
		entries.forEach((e, i) => {
			if (!chosen.has(ids[i])) return;
			if (e.clip_id) {
				const group = groupOf(e.clip_id);
				entries.forEach((f, j) => {
					if (chosen.has(ids[j]) || !f.clip_id || f.clip_id === e.clip_id) return;
					if (group !== undefined && groupOf(f.clip_id) === group) {
						chosen.add(ids[j]);
						out.set(ids[j], 'linked to a clip you picked — a picture and its sound go together');
						again = true;
					}
				});
			}
			if ((e.kind === 'clip_added' || e.kind === 'clip_moved') && e.track_id) {
				entries.forEach((f, j) => {
					if (!chosen.has(ids[j]) && f.kind === 'track_added' && f.track_id === e.track_id) {
						chosen.add(ids[j]);
						out.set(ids[j], 'a clip you picked is on this new track');
						again = true;
					}
				});
			}
		});
	}
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
