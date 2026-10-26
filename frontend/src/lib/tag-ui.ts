/* What the tag controls show and do, as plain functions — pure. The bin's chips, the header's filter, the tag
 * dialog and the Inspector's editor all draw from here, over `tags.ts` (the spelling, the filter and the edit are
 * the engine's; this is how a person gets at them): the autocomplete's ranking, which tags a bulk selection shares,
 * how many chips a row has room for, the filter menu's rows and the edit a dialog's pending changes make. */

import type { Tag, TagEdit, TagFilter, TagSummary } from './types';
import { formatTag, parseTag, sameTag, tagIdentity, tagsMatch } from './tags';
import { tidyReason } from './failure-copy';

// ---- the input: `key: value` or plain ---------------------------------------------------------------------------------

/** The most suggestions the list shows. */
export const MAX_SUGGESTIONS = 8;

/** What the autocomplete offers: a key (accepting it types `key: ` and goes on) or a whole tag (accepting it adds it). */
export type Suggestion =
	| { kind: 'key'; label: string; insert: string; assets: number }
	| { kind: 'tag'; label: string; tag: Tag; assets: number };

/** Lowercased, trimmed, inner whitespace single: what a typed fragment is compared as. */
const fold = (s: string) => s.trim().replace(/\s+/g, ' ').toLowerCase();

/** How well `candidate` answers the typed `query`: 0 it starts with it, 1 a word of it does, 2 it holds it, `null` not at all.
 *  An empty query answers everything alike. */
function closeness(candidate: string, query: string): number | null {
	if (query === '') return 0;
	const c = candidate.toLowerCase();
	if (c.startsWith(query)) return 0;
	if (c.split(/[\s\-_/.]+/).some((w) => w.startsWith(query))) return 1;
	return c.includes(query) ? 2 : null;
}

interface Candidate<T> {
	item: T;
	name: string;
	assets: number;
}

/** The candidates that answer `query`, nearest first, then the most used, then A to Z. */
function ranked<T>(candidates: readonly Candidate<T>[], query: string): T[] {
	const scored: { c: Candidate<T>; at: number }[] = [];
	for (const c of candidates) {
		const at = closeness(c.name, query);
		if (at !== null) scored.push({ c, at });
	}
	scored.sort((a, b) => a.at - b.at || b.c.assets - a.c.assets || a.c.name.toLowerCase().localeCompare(b.c.name.toLowerCase()));
	return scored.map((s) => s.c.item);
}

/**
 * The suggestions for what has been typed so far, from the library's vocabulary. Without a colon: the keys come first
 * (accepting one types `key: `), then the plain tags, then keyed tags by their value for a person who forgot the key.
 * With a colon: the values of the key before it, when the library has that key. Nearest match first, then the most
 * used. A tag in `exclude` (already pending, already on everything) is not offered again.
 */
export function suggestTags(input: string, vocab: TagSummary, exclude: readonly Tag[] = [], limit = MAX_SUGGESTIONS): Suggestion[] {
	const skip = new Set(exclude.map(tagIdentity));
	const wholeTags = (tags: { tag: Tag; assets: number }[]): Suggestion[] =>
		tags.filter((t) => !skip.has(tagIdentity(t.tag))).map((t) => ({ kind: 'tag', tag: t.tag, label: formatTag(t.tag), assets: t.assets }));
	const at = input.indexOf(':');
	if (at >= 0) {
		const key = fold(input.slice(0, at));
		const entry = vocab.keys.find((k) => k.key === key);
		if (!entry) return [];
		const values = ranked(
			entry.values.map((v) => ({ item: v, name: v.value, assets: v.assets })),
			fold(input.slice(at + 1))
		);
		return wholeTags(values.map((v) => ({ tag: { key: entry.key, value: v.value }, assets: v.assets }))).slice(0, limit);
	}
	const query = fold(input);
	const keys = ranked(
		vocab.keys.map((k) => ({ item: k, name: k.key, assets: k.assets })),
		query
	).map((k): Suggestion => ({ kind: 'key', label: `${k.key}:`, insert: `${k.key}: `, assets: k.assets }));
	const plain = wholeTags(
		ranked(
			vocab.plain.map((v) => ({ item: v, name: v.value, assets: v.assets })),
			query
		).map((v) => ({ tag: { value: v.value }, assets: v.assets }))
	);
	// Keyed tags by their value, for a person who forgot the key: only values a word of which starts with the query
	// (a short query would otherwise list every value that holds the letter).
	const byValue = wholeTags(
		query === ''
			? []
			: ranked(
					vocab.keys.flatMap((k) =>
						k.values
							.filter((v) => (closeness(v.value, query) ?? 2) < 2)
							.map((v) => ({ item: { tag: { key: k.key, value: v.value }, assets: v.assets }, name: v.value, assets: v.assets }))
					),
					query
				)
	);
	return [...keys, ...plain, ...byValue].slice(0, limit);
}

/** What Tab puts in the field for a highlighted suggestion: a key completes to `key: `, a tag to its whole spelling. Only
 *  Enter adds it. */
export function fillFor(s: Suggestion): string {
	return s.kind === 'key' ? s.insert : s.label;
}

/** What typing a tag and pressing Enter makes of the text: the tag, or why it is not one (in a sentence). */
export function parseTagInput(text: string): { tag: Tag } | { error: string } {
	if (text.trim() === '') return { error: 'Type a tag first, like location: Cumbuco or jump.' };
	try {
		return { tag: parseTag(text) };
	} catch (e) {
		return { error: tidyReason(e instanceof Error ? e.message : String(e)) };
	}
}

// ---- the dialog's pending changes -------------------------------------------------------------------------------------

/** A tag of a selection, and on how many of its assets it is. */
export interface TagTally {
	tag: Tag;
	/** How many of the assets carry it. */
	count: number;
	/** How many assets there are. */
	of: number;
	/** Every asset has it. */
	all: boolean;
}

/** The tags on the assets of a selection, one entry for each tag (ignoring case, the first spelling kept) with how many
 *  of the assets have it. The ones on every asset come first; keyed before plain, A to Z. */
export function tallyTags(lists: readonly (readonly Tag[])[]): TagTally[] {
	const seen = new Map<string, TagTally>();
	for (const list of lists) {
		const once = new Set<string>();
		for (const tag of list) {
			const id = tagIdentity(tag);
			if (once.has(id)) continue;
			once.add(id);
			const t = seen.get(id);
			if (t) t.count += 1;
			else seen.set(id, { tag, count: 1, of: lists.length, all: false });
		}
	}
	const out = [...seen.values()].map((t) => ({ ...t, all: t.count === t.of }));
	const rank = (t: TagTally) => [t.all ? 0 : 1, t.tag.key == null ? 1 : 0, t.tag.key ?? '', t.tag.value.toLowerCase()] as const;
	return out.sort((a, b) => {
		const x = rank(a);
		const y = rank(b);
		for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
		return 0;
	});
}

/** How a tally reads next to its chip: nothing for a single asset, `all` or `some (2)` for a selection. */
export function tallyNote(t: Pick<TagTally, 'count' | 'of' | 'all'>): string {
	if (t.of <= 1) return '';
	return t.all ? 'all' : `some (${t.count})`;
}

/** What typing `tag` does to a dialog that has `added` pending, `removed` marked for removal and `tally` already there. */
export type AddPlan = 'add' | 'restore' | 'pending' | 'present';

/** A tag that is marked for removal is un-marked rather than added; one that is already pending, or on every asset, adds nothing. */
export function planAdd(tag: Tag, added: readonly Tag[], removed: readonly Tag[], tally: readonly TagTally[]): AddPlan {
	if (added.some((t) => sameTag(t, tag))) return 'pending';
	if (removed.some((t) => sameTag(t, tag))) return 'restore';
	if (tally.some((t) => t.all && sameTag(t.tag, tag))) return 'present';
	return 'add';
}

/** The sentence for a typed tag that adds nothing. */
export function addNote(plan: AddPlan, tag: Tag, assets: number): string {
	if (plan === 'pending') return `${formatTag(tag)} is already in the list.`;
	if (plan === 'present') return assets === 1 ? `${formatTag(tag)} is already on this asset.` : `${formatTag(tag)} is already on every one of these ${assets} assets.`;
	return '';
}

/** The edit for what a dialog collected. A tag that is both pending and marked for removal is just kept. `null` when there is nothing to do. */
export function pendingEdit(added: readonly Tag[], removed: readonly Tag[]): TagEdit | null {
	const remove = removed.filter((r) => !added.some((a) => sameTag(a, r)));
	if (added.length === 0 && remove.length === 0) return null;
	const edit: TagEdit = {};
	if (remove.length > 0) edit.remove = [...remove];
	if (added.length > 0) edit.add = [...added];
	return edit;
}

/** `Tagged clip.mp4`, `Tagged 12 assets` — the toast after a tag change. */
export function taggedLine(names: readonly string[]): string {
	return names.length === 1 ? `Tagged ${names[0]}` : `Tagged ${names.length} assets`;
}

/** The files a dialog is about, said short: `a.mp4, b.mp4, c.mp4 and 9 more`. */
export function namesLine(names: readonly string[], show = 3): string {
	if (names.length <= show + 1) return names.join(', ');
	return `${names.slice(0, show).join(', ')} and ${names.length - show} more`;
}

/** The dialog's title: `Tag 12 imported files`, `Tag 3 assets`, `Tag clip.mp4`. */
export function dialogTitle(mode: 'import' | 'bulk', names: readonly string[]): string {
	const n = names.length;
	if (mode === 'import') return n === 1 ? `Tag ${names[0]}` : `Tag ${n} imported files`;
	return n === 1 ? `Tag ${names[0]}` : `Tag ${n} assets`;
}

// ---- chips in the bin -------------------------------------------------------------------------------------------------

/** The most chips a bin row draws before it says `+N`. */
export const ROW_CHIPS = 3;

export interface RowChips {
	shown: Tag[];
	hidden: number;
	/** Every tag, one per line: the tooltip of the `+N`. */
	all: string;
}

/** The first `max` tags of a row, and how many it left out. */
export function rowChips(tags: readonly Tag[] | undefined, max = ROW_CHIPS): RowChips {
	const list = tags ?? [];
	return { shown: list.slice(0, max), hidden: Math.max(list.length - max, 0), all: list.map(formatTag).join('\n') };
}

// ---- the filter -------------------------------------------------------------------------------------------------------

/** Two terms that ask for the same thing, ignoring case. */
export function sameTerm(a: TagFilter, b: TagFilter): boolean {
	const x = (t: TagFilter) => JSON.stringify([t.key?.trim().toLowerCase() ?? null, t.value?.trim().toLowerCase() ?? null]);
	return x(a) === x(b);
}

/** The filter with `term` added, or taken off when it is already in it. */
export function toggleTerm(filter: readonly TagFilter[], term: TagFilter): TagFilter[] {
	return filter.some((t) => sameTerm(t, term)) ? filter.filter((t) => !sameTerm(t, term)) : [...filter, term];
}

/** How a term reads: `location: Cumbuco`, `location: any`, `jump`. */
export function termLabel(term: TagFilter): string {
	if (term.key != null && term.value == null) return `${term.key}: any`;
	return term.key != null ? `${term.key}: ${term.value}` : (term.value ?? '');
}

/** A key for a term in a list: terms that read alike (`{key:'a'}` is `a: any`, `{key:'a', value:'any'}` too) stay apart. */
export function termKey(term: TagFilter): string {
	return JSON.stringify([term.key?.trim().toLowerCase() ?? null, term.value?.trim().toLowerCase() ?? null]);
}

/** The items that satisfy every term (all of them when there is no filter). */
export function filterByTags<T extends { tags?: readonly Tag[] }>(items: readonly T[], filter: readonly TagFilter[]): T[] {
	return filter.length === 0 ? [...items] : items.filter((i) => tagsMatch(filter, i.tags ?? []));
}

/** `12`, or `3 of 12` once a filter narrows it. */
export function countLabel(shown: number, total: number, filtered: boolean): string {
	return filtered ? `${shown} of ${total}` : String(total);
}

/** One row of the filter menu: a term, how it reads, how many assets have it and whether the filter has it. */
export interface FilterRow {
	term: TagFilter;
	label: string;
	assets: number;
	active: boolean;
}

/** A group of rows: a key with its values (`Any location` first), or the plain tags. */
export interface FilterGroup {
	title: string;
	/** How many assets have the key; `null` for the plain tags, which are not one thing to count. */
	assets: number | null;
	rows: FilterRow[];
}

/** The vocabulary laid out for picking a term: one group per key, then the plain tags, in the vocabulary's own order. */
export function filterGroups(vocab: TagSummary, filter: readonly TagFilter[]): FilterGroup[] {
	const active = (term: TagFilter) => filter.some((t) => sameTerm(t, term));
	const groups: FilterGroup[] = vocab.keys.map((k) => ({
		title: k.key,
		assets: k.assets,
		rows: [
			{ term: { key: k.key }, label: `Any ${k.key}`, assets: k.assets, active: active({ key: k.key }) },
			...k.values.map((v) => ({
				term: { key: k.key, value: v.value },
				label: v.value,
				assets: v.assets,
				active: active({ key: k.key, value: v.value })
			}))
		]
	}));
	if (vocab.plain.length > 0) {
		groups.push({
			title: 'Plain tags',
			assets: null,
			rows: vocab.plain.map((v) => ({ term: { value: v.value }, label: v.value, assets: v.assets, active: active({ value: v.value }) }))
		});
	}
	return groups;
}

/** The signature of the tags an asset list carries: changes when a tag is added, removed or renamed, and not for anything else. */
export function tagsSignature(assets: readonly { id: string; tags?: readonly Tag[] }[]): string {
	return assets.map((a) => `${a.id}\u0001${(a.tags ?? []).map(formatTag).join('\u0002')}`).join('\u0003');
}

/** The bin menu's "append them all" row: every file of the library, or — under a tag filter — the ones shown. */
export function appendAllLabel(shown: number, total: number): string {
	if (shown < total) return `Append ${shown} shown ${shown === 1 ? 'file' : 'files'} to the timeline`;
	return `Append all ${total} files to the timeline`;
}

/** The toast after it: what was appended, and that a filter narrowed it. */
export function appendedLine(appended: number, total: number): string {
	if (appended === 1) return 'Appended 1 file to the timeline';
	return appended < total
		? `Appended ${appended} shown files to the timeline, in library order`
		: `Appended ${appended} files to the timeline, in library order`;
}
