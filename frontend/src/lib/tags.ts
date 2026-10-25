/* Asset tags — pure. A faithful port of `kerf-core`'s `model/tags.rs`: the spelling of a tag, an edit of an asset's
 * tags, a filter over them, the vocabulary of a library and the grouping `assemble_by_tag` makes. The browser harness
 * runs it where the desktop app asks the backend; the dialogs and the bin's filter use it to show what the backend will
 * do (`bun test` replays the Rust unit cases in `tags.test.ts`). Change a rule in the engine and here together.
 *
 * A tag is compared case-insensitively and stored in the spelling it was first given. Keys are lowercased. */

import type {
	AssembleByTag,
	Asset,
	GroupTitle,
	StagedAssetChange,
	Tag,
	TagCount,
	TagEdit,
	TagFilter,
	TagKeyCount,
	TagSummary
} from './types';

/** The longest key (characters). */
export const MAX_TAG_KEY = 32;
/** The longest value (characters). */
export const MAX_TAG_VALUE = 64;
/** The most tags one asset holds. */
export const MAX_ASSET_TAGS = 64;

/** The engine's `invalid argument: …` (`Error::InvalidArgument`). */
const invalid = (why: string) => new Error(`invalid argument: ${why}`);

/** Rust's `char::is_whitespace` (Unicode `White_Space`) — not JavaScript's `\s`, which adds U+FEFF and lacks U+0085. */
const WHITESPACE = /[\t-\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]/;
/** Rust's `char::is_control` (category Cc). */
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;

/** Characters, as Rust counts them (code points, not UTF-16 units). */
const length = (s: string) => [...s].length;

/** Rust's `str` order: by code point (JavaScript's `<` compares UTF-16 units, which differs past the BMP). */
function compare(a: string, b: string): number {
	const x = [...a];
	const y = [...b];
	for (let i = 0; i < Math.min(x.length, y.length); i++) {
		const d = x[i].codePointAt(0)! - y[i].codePointAt(0)!;
		if (d !== 0) return d < 0 ? -1 : 1;
	}
	return x.length === y.length ? 0 : x.length < y.length ? -1 : 1;
}

/** `s` trimmed, its whitespace (tabs and line breaks too) collapsed to single spaces and any other control character
 *  dropped. */
function clean(s: string): string {
	let out = '';
	for (const c of s) {
		if (WHITESPACE.test(c)) out += ' ';
		else if (!CONTROL.test(c)) out += c;
	}
	return out.split(' ').filter(Boolean).join(' ');
}

/** A key as stored: cleaned, lowercased, 1..=`MAX_TAG_KEY` characters. */
export function normalizeKey(key: string): string {
	const out = clean(key).toLowerCase();
	if (out === '') throw invalid('a tag key cannot be empty');
	if (length(out) > MAX_TAG_KEY) throw invalid(`a tag key is at most ${MAX_TAG_KEY} characters: "${out}"`);
	return out;
}

function normalizeValue(value: string): string {
	const out = clean(value);
	if (out === '') throw invalid('a tag value cannot be empty');
	if (length(out) > MAX_TAG_VALUE) throw invalid(`a tag value is at most ${MAX_TAG_VALUE} characters: "${out}"`);
	return out;
}

/** A tag from its parts, normalized; an empty or over-long key or value is refused (`Tag::new`). A missing key is a plain
 *  tag, and the result carries no `key` member then, as the engine stores it. */
export function newTag(key: string | null | undefined, value: string): Tag {
	const k = key == null ? null : normalizeKey(key);
	const v = normalizeValue(value);
	return k === null ? { value: v } : { key: k, value: v };
}

/** `location: Cumbuco` splits on the first `:` into a key and a value; with no `:` it is a plain tag (`Tag::parse`). */
export function parseTag(text: string): Tag {
	const at = text.indexOf(':');
	return at < 0 ? newTag(null, text) : newTag(text.slice(0, at), text.slice(at + 1));
}

/** This tag as `newTag` would have made it (`Tag::normalized`). */
export function normalizedTag(tag: Tag): Tag {
	return newTag(tag.key, tag.value);
}

/** How a tag reads: `location: Cumbuco`, or `jump` (`Display`). */
export function formatTag(tag: Tag): string {
	return tag.key == null ? tag.value : `${tag.key}: ${tag.value}`;
}

/** What two spellings of one tag share: the key and value, lowercased. */
function identity(tag: Tag): string {
	return JSON.stringify([tag.key == null ? null : tag.key.toLowerCase(), tag.value.toLowerCase()]);
}

/** A string that is equal for two spellings of one tag (`sameTag`'s comparison), for sets and map keys. */
export const tagIdentity = identity;

/** Whether `b` is the same tag, ignoring case (`Tag::same_as`). */
export function sameTag(a: Tag, b: Tag): boolean {
	return identity(a) === identity(b);
}

/** `tags` without a repeat (ignoring case), each in the spelling it was first given. */
export function dedupeTags(tags: readonly Tag[]): Tag[] {
	const seen = new Set<string>();
	return tags.filter((t) => {
		const id = identity(t);
		if (seen.has(id)) return false;
		seen.add(id);
		return true;
	});
}

/** Whether two tag lists hold the same tags, whatever their order or spelling. */
export function sameTags(a: readonly Tag[], b: readonly Tag[]): boolean {
	const x = new Set(a.map(identity));
	const y = new Set(b.map(identity));
	return x.size === y.size && [...x].every((id) => y.has(id));
}

/** Whether an edit changes nothing for lack of anything to do (`TagEdit::is_empty`). */
export function tagEditIsEmpty(edit: TagEdit): boolean {
	return !edit.add?.length && !edit.remove?.length && !edit.remove_keys?.length;
}

/** `tags` after the edit, applied in the order `remove_keys`, `remove`, `add`: repeats gone (the first spelling stays), at
 *  most `MAX_ASSET_TAGS`. Everything the edit names is normalized first, and an invalid tag or key refuses the whole edit
 *  (`TagEdit::apply`). */
export function applyTagEdit(tags: readonly Tag[], edit: TagEdit): Tag[] {
	const keys = new Set((edit.remove_keys ?? []).map(normalizeKey));
	const remove = new Set((edit.remove ?? []).map((t) => identity(normalizedTag(t))));
	const add = (edit.add ?? []).map(normalizedTag);
	const kept = tags.filter((t) => !(t.key != null && keys.has(t.key.toLowerCase()))).filter((t) => !remove.has(identity(t)));
	const out = dedupeTags([...kept, ...add]);
	if (out.length > MAX_ASSET_TAGS) throw invalid(`an asset holds at most ${MAX_ASSET_TAGS} tags`);
	return out;
}

/** `location: Cumbuco` is that tag, `location:*` any tag with the key, `jump` the plain tag (`TagFilter::parse`). */
export function parseTagFilter(text: string): TagFilter {
	const at = text.indexOf(':');
	let term: TagFilter;
	if (at < 0) term = { value: text };
	else if (clean(text.slice(at + 1)) === '*') term = { key: text.slice(0, at) };
	else term = { key: text.slice(0, at), value: text.slice(at + 1) };
	return checkTagFilter(term);
}

/** This term with its key and value normalized the way tags are (a value is not length-capped: it only has to be
 *  non-empty); a term naming neither is refused (`TagFilter::checked`). */
export function checkTagFilter(term: TagFilter): TagFilter {
	if (term.key == null && term.value == null) throw invalid('a tag filter names a key, a value or both');
	let value: string | undefined;
	if (term.value != null) {
		value = clean(term.value);
		if (value === '') throw invalid('a tag filter value cannot be empty');
	}
	const out: TagFilter = {};
	if (term.key != null) out.key = normalizeKey(term.key);
	if (value !== undefined) out.value = value;
	return out;
}

/** Whether some tag of `tags` satisfies this term, ignoring case: `{key, value}` is that tag, `{key}` any tag with that
 *  key, `{value}` alone a plain tag with that value (`TagFilter::matches`). */
export function tagFilterMatches(term: TagFilter, tags: readonly Tag[]): boolean {
	const key = term.key == null ? null : clean(term.key).toLowerCase();
	const value = term.value == null ? null : clean(term.value).toLowerCase();
	return tags.some((t) => {
		const keyOk = key !== null ? t.key != null && t.key.toLowerCase() === key : t.key == null;
		return keyOk && (value === null || value === t.value.toLowerCase());
	});
}

/** Whether `tags` satisfy every term of `filter` (an empty filter holds for all). */
export function tagsMatch(filter: readonly TagFilter[], tags: readonly Tag[]): boolean {
	return filter.every((term) => tagFilterMatches(term, tags));
}

function sortedCounts(counts: Map<string, TagCount>): TagCount[] {
	return [...counts.values()].sort((a, b) => compare(a.value.toLowerCase(), b.value.toLowerCase()) || compare(a.value, b.value));
}

/** The vocabulary of a library, one tag list per asset. A value is counted once per asset and shown in the spelling met
 *  first (`TagSummary::of`). */
export function tagSummary(assets: Iterable<readonly Tag[]>): TagSummary {
	const plain = new Map<string, TagCount>();
	const keyed = new Map<string, { assets: number; values: Map<string, TagCount> }>();
	for (const tags of assets) {
		const keysSeen = new Set<string>();
		for (const tag of dedupeTags(tags)) {
			let counts = plain;
			if (tag.key != null) {
				const key = tag.key.toLowerCase();
				let entry = keyed.get(key);
				if (!entry) keyed.set(key, (entry = { assets: 0, values: new Map() }));
				if (!keysSeen.has(key)) {
					keysSeen.add(key);
					entry.assets += 1;
				}
				counts = entry.values;
			}
			const folded = tag.value.toLowerCase();
			let count = counts.get(folded);
			if (!count) counts.set(folded, (count = { value: tag.value, assets: 0 }));
			count.assets += 1;
		}
	}
	const keys: TagKeyCount[] = [...keyed.entries()]
		.map(([key, e]) => ({ key, assets: e.assets, values: sortedCounts(e.values) }))
		.sort((a, b) => compare(a.key, b.key));
	return { keys, plain: sortedCounts(plain) };
}

/** One group of `groupByKey`: the value that names it (`null` for the assets with no such key) and the indices of its
 *  members, in the order they were given. */
export interface KeyGroup {
	value: string | null;
	members: number[];
}

/** The groups `groupByKey` made, and the `order` entries that matched no group. */
export interface Grouping {
	groups: KeyGroup[];
	unmatched: string[];
}

/** Group assets (one tag list each) by the value of `key`. Groups follow `order` (case-insensitive); values it does not
 *  list follow A to Z, and with `includeUntagged` the assets lacking the key come last as a group of no value. An asset
 *  with several values of the key joins the first of them in that order. Members keep the order they were given in
 *  (`group_by_key`). */
export function groupByKey(
	assets: readonly (readonly Tag[])[],
	key: string,
	order: readonly string[] = [],
	includeUntagged = false
): Grouping {
	const wanted = normalizeKey(key);
	const ranks = new Map<string, number>();
	const listed: [string, string][] = [];
	for (const entry of order) {
		const shown = clean(entry);
		if (shown === '') throw invalid('an empty value in the order');
		const folded = shown.toLowerCase();
		if (!ranks.has(folded)) {
			ranks.set(folded, ranks.size);
			listed.push([folded, shown]);
		}
	}
	const rank = (folded: string) => ranks.get(folded) ?? ranks.size;
	const groups = new Map<string, { shown: string; members: number[] }>();
	const untagged: number[] = [];
	assets.forEach((tags, i) => {
		let best: Tag | null = null;
		for (const t of tags) {
			if (t.key == null || t.key.toLowerCase() !== wanted) continue;
			if (best === null) best = t;
			else {
				const a = [rank(t.value.toLowerCase()), t.value.toLowerCase()] as const;
				const b = [rank(best.value.toLowerCase()), best.value.toLowerCase()] as const;
				if (a[0] < b[0] || (a[0] === b[0] && compare(a[1], b[1]) < 0)) best = t;
			}
		}
		if (best === null) {
			untagged.push(i);
			return;
		}
		const folded = best.value.toLowerCase();
		let group = groups.get(folded);
		if (!group) groups.set(folded, (group = { shown: best.value, members: [] }));
		group.members.push(i);
	});
	const unmatched = listed.filter(([folded]) => !groups.has(folded)).map(([, shown]) => shown);
	const named = [...groups.entries()].sort(([a], [b]) => rank(a) - rank(b) || compare(a, b));
	const out: KeyGroup[] = named.map(([, g]) => ({ value: g.shown, members: g.members }));
	if (includeUntagged && untagged.length > 0) out.push({ value: null, members: untagged });
	return { groups: out, unmatched };
}

/** Whether a held-back change of the library is a tag change (its `tags` set) rather than a projection mark. */
export function isTagChange(change: Pick<StagedAssetChange, 'tags'>): boolean {
	return change.tags != null;
}

/** The longest a group's title (or card) may run (`MAX_TAG_TITLE_SECONDS`). */
export const MAX_TAG_TITLE_SECONDS = 60;
/** How long a title runs when `title_seconds` is left out. */
export const DEFAULT_TAG_TITLE_SECONDS = 3;

/** One group `assembleByTag` will lay down: its value (`null`: the assets lacking the key) and its assets, in play order. */
export interface PlannedGroup {
	value: string | null;
	assets: Asset[];
}

/** Which assets go in which group, in order (`AssemblePlan`). */
export interface AssemblePlan {
	/** The key, trimmed and lowercased: what the revision is named after. */
	key: string;
	title: GroupTitle;
	title_seconds: number;
	groups: PlannedGroup[];
	/** Values of `order` that no asset carries. */
	unmatched: string[];
}

const hasStream = (a: Asset, kind: string) => a.streams.some((s) => s.kind === kind);

/** Whether an asset can be laid on the cut: picture or sound, and some length (`Asset::primary_kind` is not `data`). */
const playable = (a: Asset) => (hasStream(a, 'video') || hasStream(a, 'audio')) && a.duration > 0;

const byTime = (a: Asset, b: Asset) => Date.parse(a.imported_at) - Date.parse(b.imported_at);

/** `Project::assemble_by_tag_inputs`: which of `library` go in which group, in order, for `req`. Refuses a title length
 *  out of range, a bad filter term, key or order entry, and a request no asset answers. */
export function planAssembleByTag(library: readonly Asset[], req: AssembleByTag): AssemblePlan {
	const seconds = req.title_seconds ?? DEFAULT_TAG_TITLE_SECONDS;
	if (!(Number.isFinite(seconds) && seconds > 0 && seconds <= MAX_TAG_TITLE_SECONDS)) {
		throw invalid(`title_seconds is a number of seconds above 0 and at most ${MAX_TAG_TITLE_SECONDS}`);
	}
	const filter = (req.filter ?? []).map(checkTagFilter);
	const assets = library.filter((a) => playable(a) && tagsMatch(filter, a.tags ?? []));
	const name = (a: Asset) => a.name.toLowerCase();
	if (req.sort_within === 'name') {
		assets.sort((a, b) => compare(name(a), name(b)) || byTime(a, b) || compare(a.id, b.id));
	} else {
		assets.sort((a, b) => byTime(a, b) || compare(name(a), name(b)) || compare(a.id, b.id));
	}
	const grouping = groupByKey(
		assets.map((a) => a.tags ?? []),
		req.key,
		req.order ?? [],
		req.include_untagged ?? false
	);
	if (grouping.groups.length === 0) {
		throw invalid(`no asset has a "${req.key.trim()}" tag${filter.length === 0 ? '' : ' and matches the filter'}`);
	}
	return {
		key: req.key.trim().toLowerCase(),
		title: req.title ?? 'overlay',
		title_seconds: seconds,
		groups: grouping.groups.map((g) => ({ value: g.value, assets: g.members.map((i) => assets[i]) })),
		unmatched: grouping.unmatched
	};
}
