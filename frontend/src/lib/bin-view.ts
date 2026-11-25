/* What the media bin shows and in which order — pure. The bin sorts and filters the way a file manager does: by
 * name, date, size or length, and down to a kind of media, a property of it, part of its name or a range of dates.
 * The tag filter (`tags.ts`) is applied by the caller; everything here is about the file. */

import { audioStream, videoStream } from './media-info';
import type { Asset, AssetFile } from './types';

export type BinSortKey = 'added' | 'name' | 'date' | 'size' | 'duration';

export interface BinSort {
	key: BinSortKey;
	desc: boolean;
}

/** The bin as it was imported: the order the project holds. */
export const DEFAULT_SORT: BinSort = { key: 'added', desc: false };

export const SORT_KEYS: readonly { key: BinSortKey; label: string; hint: string }[] = [
	{ key: 'added', label: 'Import order', hint: 'The order the media came into the project' },
	{ key: 'name', label: 'Name', hint: 'By file name' },
	{ key: 'date', label: 'Date', hint: 'When the file was last written: for camera files, when it was shot' },
	{ key: 'size', label: 'File size', hint: 'By the size of the file on disk' },
	{ key: 'duration', label: 'Length', hint: 'By how long it runs' }
];

/** The direction a key starts in when it is picked: names A to Z, the newest, largest and longest first. */
export function defaultDesc(key: BinSortKey): boolean {
	return key === 'date' || key === 'size' || key === 'duration';
}

/** Picking a sort key: the one in force flips its direction, another starts in its own. */
export function pickSort(sort: BinSort, key: BinSortKey): BinSort {
	if (sort.key === key && key !== 'added') return { key, desc: !sort.desc };
	return { key, desc: defaultDesc(key) };
}

export type BinKind = 'video' | 'audio' | 'image';

/** A property an asset has or has not. Within a group any may hold; every group with a pick must hold. */
export type BinProp =
	| 'res:uhd'
	| 'res:hd'
	| 'res:sd'
	| 'shape:landscape'
	| 'shape:portrait'
	| 'shape:square'
	| 'rate:high'
	| 'rate:normal'
	| 'sound:with'
	| 'sound:without'
	| 'use:used'
	| 'use:unused'
	| 'tags:tagged'
	| 'tags:untagged'
	| 'view:360';

export const PROP_GROUPS: readonly { title: string; props: readonly { id: BinProp; label: string }[] }[] = [
	{
		title: 'Resolution',
		props: [
			{ id: 'res:uhd', label: '4K and above' },
			{ id: 'res:hd', label: 'HD (720p to 1440p)' },
			{ id: 'res:sd', label: 'Below HD' }
		]
	},
	{
		title: 'Shape',
		props: [
			{ id: 'shape:landscape', label: 'Landscape' },
			{ id: 'shape:portrait', label: 'Vertical' },
			{ id: 'shape:square', label: 'Square' }
		]
	},
	{
		title: 'Frame rate',
		props: [
			{ id: 'rate:high', label: '50 fps and above' },
			{ id: 'rate:normal', label: 'Below 50 fps' }
		]
	},
	{
		title: 'Sound',
		props: [
			{ id: 'sound:with', label: 'With sound' },
			{ id: 'sound:without', label: 'Without sound' }
		]
	},
	{
		title: 'On the timeline',
		props: [
			{ id: 'use:used', label: 'Used' },
			{ id: 'use:unused', label: 'Not used yet' }
		]
	},
	{
		title: 'Tags',
		props: [
			{ id: 'tags:tagged', label: 'Tagged' },
			{ id: 'tags:untagged', label: 'Not tagged yet' }
		]
	},
	{ title: '360', props: [{ id: 'view:360', label: '360 footage' }] }
];

export const KIND_LABELS: Record<BinKind, string> = { video: 'Video', audio: 'Audio', image: 'Stills' };

export interface BinFilter {
	/** Part of the file name: every word must be in it, ignoring case. */
	text: string;
	/** The kinds shown; none means all. */
	kinds: BinKind[];
	props: BinProp[];
	/** The first and the last day shown, `YYYY-MM-DD`, in local time; `''` leaves that end open. */
	from: string;
	to: string;
}

export const NO_FILTER: BinFilter = { text: '', kinds: [], props: [], from: '', to: '' };

export function filterActive(f: BinFilter): boolean {
	return f.text.trim() !== '' || f.kinds.length > 0 || f.props.length > 0 || f.from !== '' || f.to !== '';
}

export function toggleIn<T>(list: readonly T[], item: T): T[] {
	return list.includes(item) ? list.filter((i) => i !== item) : [...list, item];
}

export function propLabel(id: BinProp): string {
	for (const g of PROP_GROUPS) for (const p of g.props) if (p.id === id) return p.label;
	return id;
}

/** What the bin needs to know of an asset beyond the asset: its file on disk and how often the cut uses it. */
export interface BinFacts {
	file?: Pick<AssetFile, 'size_bytes' | 'modified' | 'created'>;
	uses: number;
}

export function kindOf(a: Pick<Asset, 'streams'>): BinKind {
	if (a.streams.some((s) => s.image)) return 'image';
	return videoStream(a) ? 'video' : 'audio';
}

/** The date an asset is sorted and filtered by, in ms: when its file was last written, else when it was imported. */
export function dateOf(a: Pick<Asset, 'imported_at'>, facts: BinFacts | undefined): number | null {
	const t = Date.parse(facts?.file?.modified ?? a.imported_at);
	return Number.isNaN(t) ? null : t;
}

function hasProp(a: Asset, facts: BinFacts | undefined, prop: BinProp): boolean {
	const v = videoStream(a);
	const [w, h] = [v?.width ?? 0, v?.height ?? 0];
	const short = Math.min(w, h);
	const fps = v?.fps ?? 0;
	const still = a.streams.some((s) => s.image);
	switch (prop) {
		case 'res:uhd':
			return short >= 2160;
		case 'res:hd':
			return short >= 720 && short < 2160;
		case 'res:sd':
			return short > 0 && short < 720;
		case 'shape:landscape':
			return w > h && h > 0;
		case 'shape:portrait':
			return h > w && w > 0;
		case 'shape:square':
			return w > 0 && w === h;
		case 'rate:high':
			return !still && fps >= 49.5;
		case 'rate:normal':
			return !still && fps > 0 && fps < 49.5;
		case 'sound:with':
			return !!audioStream(a);
		case 'sound:without':
			return !audioStream(a);
		case 'use:used':
			return (facts?.uses ?? 0) > 0;
		case 'use:unused':
			return (facts?.uses ?? 0) === 0;
		case 'tags:tagged':
			return (a.tags?.length ?? 0) > 0;
		case 'tags:untagged':
			return (a.tags?.length ?? 0) === 0;
		case 'view:360':
			return !!v?.projection && v.projection !== 'flat';
	}
}

/** Local midnight at the start of a `YYYY-MM-DD` day, in ms; `null` for anything else. */
export function dayStart(day: string): number | null {
	const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day);
	if (!m) return null;
	const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
	return Number.isNaN(d.getTime()) ? null : d.getTime();
}

const DAY_MS = 86_400_000;

export function matchesFilter(a: Asset, facts: BinFacts | undefined, f: BinFilter): boolean {
	const words = f.text.toLowerCase().split(/\s+/).filter(Boolean);
	if (words.length > 0) {
		const name = a.name.toLowerCase();
		if (!words.every((w) => name.includes(w))) return false;
	}
	if (f.kinds.length > 0 && !f.kinds.includes(kindOf(a))) return false;
	const groups = new Map<string, BinProp[]>();
	for (const p of f.props) {
		const g = p.slice(0, p.indexOf(':'));
		groups.set(g, [...(groups.get(g) ?? []), p]);
	}
	for (const props of groups.values()) if (!props.some((p) => hasProp(a, facts, p))) return false;
	const from = dayStart(f.from);
	const to = dayStart(f.to);
	if (from !== null || to !== null) {
		const at = dateOf(a, facts);
		if (at === null) return false;
		if (from !== null && at < from) return false;
		if (to !== null && at >= to + DAY_MS) return false;
	}
	return true;
}

const byName = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

/** The assets in the order `sort` asks for. An asset with no answer (a file that cannot be read has no size) goes
 *  last in either direction; ties keep the import order. */
export function sortAssets<T extends Asset>(assets: readonly T[], factsOf: (id: string) => BinFacts | undefined, sort: BinSort): T[] {
	if (sort.key === 'added') return sort.desc ? [...assets].reverse() : [...assets];
	if (sort.key === 'name') {
		const sorted = [...assets].sort((a, b) => byName.compare(a.name, b.name));
		return sort.desc ? sorted.reverse() : sorted;
	}
	const value = (a: T): number | null => {
		if (sort.key === 'duration') return a.duration;
		const facts = factsOf(a.id);
		if (sort.key === 'size') return facts?.file?.size_bytes ?? null;
		return dateOf(a, facts);
	};
	const keyed = assets.map((asset, at) => ({ asset, at, v: value(asset) }));
	keyed.sort((a, b) => {
		if (a.v === null || b.v === null) return a.v === b.v ? a.at - b.at : a.v === null ? 1 : -1;
		if (a.v !== b.v) return sort.desc ? b.v - a.v : a.v - b.v;
		return a.at - b.at;
	});
	return keyed.map((k) => k.asset);
}

/** What the bin shows: the assets the filter lets through, in the sort's order. */
export function binView<T extends Asset>(
	assets: readonly T[],
	factsOf: (id: string) => BinFacts | undefined,
	filter: BinFilter,
	sort: BinSort
): T[] {
	const kept = filterActive(filter) ? assets.filter((a) => matchesFilter(a, factsOf(a.id), filter)) : assets;
	return sortAssets(kept, factsOf, sort);
}

/** `1.2 GB`, `340 MB`, `12 KB`: a file's size the way a file manager says it (powers of 1024). */
export function fmtSize(bytes: number): string {
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	let v = bytes;
	let u = 0;
	while (v >= 1024 && u < units.length - 1) {
		v /= 1024;
		u++;
	}
	return `${u === 0 || v >= 100 ? Math.round(v) : v.toFixed(1)} ${units[u]}`;
}

/** The filter's parts as the chips the bin draws, each with the filter it leaves when taken off. */
export function filterChips(f: BinFilter): { key: string; label: string; without: BinFilter }[] {
	const chips: { key: string; label: string; without: BinFilter }[] = [];
	for (const k of f.kinds) chips.push({ key: `kind:${k}`, label: KIND_LABELS[k], without: { ...f, kinds: f.kinds.filter((x) => x !== k) } });
	for (const p of f.props) chips.push({ key: p, label: propLabel(p), without: { ...f, props: f.props.filter((x) => x !== p) } });
	if (f.from || f.to) {
		const label = f.from && f.to ? (f.from === f.to ? f.from : `${f.from} to ${f.to}`) : f.from ? `from ${f.from}` : `until ${f.to}`;
		chips.push({ key: 'date', label, without: { ...f, from: '', to: '' } });
	}
	return chips;
}

/** What the sort button says: `Name, A to Z`. */
export function sortLabel(sort: BinSort): string {
	const dir: Record<BinSortKey, [string, string]> = {
		added: ['first in first', 'last in first'],
		name: ['A to Z', 'Z to A'],
		date: ['oldest first', 'newest first'],
		size: ['smallest first', 'largest first'],
		duration: ['shortest first', 'longest first']
	};
	const label = SORT_KEYS.find((k) => k.key === sort.key)?.label ?? sort.key;
	return `${label}, ${dir[sort.key][sort.desc ? 1 : 0]}`;
}
