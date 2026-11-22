// Workspaces: the editor in five arrangements (Edit / Color / Audio / Motion /
// Deliver) and any the user saves, the library rail's tabs, and what the app
// remembers about both.
//
// What is stored is one opaque value in the app settings (`Settings.workspaces`
// on the Rust side — the backend never looks inside it):
//
//   { active, layouts: { <workspace>: <dockview layout> },
//     offered: { <workspace>: [<panel>, …] },
//     library: { tabs: { <workspace>: <tab> }, collapsed },
//     custom: [ { id, name }, … ], order: [ <workspace>, … ] }
//
// A workspace is named by its key: one of the five built-in ids, or the id of a
// saved one (`custom`). `order` is the order of the buttons, all of them. A saved
// workspace's layout is always the user's own (it has no preset to follow, so no
// `offered`); a built-in one's is stored only once it has been arranged.
//
// `offered` is, for each stored layout, the panels its workspace's preset opened
// when the layout was saved: how a later build's new panel (the mixer in Audio)
// can be told from one the user closed (see `adoptPanels`).
//
// The frontend owns that shape and validates it on the way back in, so a file
// from an older or newer build, a hand edit, or a truncated write degrades to
// the presets instead of leaving the editor without a timeline. A value from
// before saved workspaces has neither `custom` nor `order` and reads as it did.

import type { SerializedDockview } from 'dockview';
import {
	EARLIER_PRESETS,
	PANELS,
	UNSTAMPED_OFFERED,
	WORKSPACE_IDS,
	insertPanel,
	isPanelId,
	isWorkspaceId,
	openPanelIds,
	presetLayout,
	presetPanelIds,
	sameArrangement,
	sanitizeLayout,
	type PanelId,
	type WorkspaceId
} from './layout';

export { WORKSPACE_IDS, isWorkspaceId, type WorkspaceId };

// ---- the library rail ------------------------------------------------------

export const LIBRARY_TABS = ['media', 'titles', 'effects', 'transitions', 'motion', 'audio', 'transcript'] as const;
export type LibraryTab = (typeof LIBRARY_TABS)[number];

export interface LibraryTabSpec {
	id: LibraryTab;
	label: string;
	/** Key into `components/editor/icons.ts`. */
	icon: string;
	/** The rail tooltip: what is in there. */
	hint: string;
}

export const LIBRARY_TAB_SPECS: LibraryTabSpec[] = [
	{ id: 'media', label: 'Media', icon: 'film', hint: 'Imported footage, stills and audio' },
	{ id: 'titles', label: 'Titles', icon: 'type', hint: 'Titles, lower thirds and captions' },
	{ id: 'effects', label: 'Effects', icon: 'sparkles', hint: 'Color looks and video effects for the selected clip' },
	{ id: 'transitions', label: 'Transitions', icon: 'blend', hint: 'Fades, slides and pushes into the selected clip' },
	{ id: 'motion', label: 'Motion', icon: 'move', hint: 'Entrances, exits and emphases for the selected clips' },
	{ id: 'audio', label: 'Audio', icon: 'audio-waveform', hint: 'Audio effects and voiceover' },
	{ id: 'transcript', label: 'Transcript', icon: 'file-text', hint: 'The transcript — click a line to seek, × to cut it' }
];

export function isLibraryTab(tab: unknown): tab is LibraryTab {
	return typeof tab === 'string' && (LIBRARY_TABS as readonly string[]).includes(tab);
}

/** The tab after / before `tab` in rail order, wrapping — what the arrow keys
 *  walk. */
export function stepTab(tab: LibraryTab, by: 1 | -1): LibraryTab {
	const i = LIBRARY_TABS.indexOf(tab);
	return LIBRARY_TABS[(i + by + LIBRARY_TABS.length) % LIBRARY_TABS.length];
}

// ---- the workspaces --------------------------------------------------------

export interface WorkspaceSpec {
	id: WorkspaceId;
	label: string;
	hint: string;
	/** The library tab this workspace shows until the user picks another in it:
	 *  the tool it is about. Each workspace remembers its own choice. */
	libraryTab: LibraryTab;
}

export const WORKSPACE_SPECS: WorkspaceSpec[] = [
	{ id: 'edit', label: 'Edit', hint: 'Cut and arrange — library, preview, inspector and a full-width timeline', libraryTab: 'media' },
	{ id: 'color', label: 'Color', hint: 'Grade the picture — the preview as large as it gets, controls beside it', libraryTab: 'effects' },
	{ id: 'audio', label: 'Audio', hint: 'Work on sound — the mixer beside the picture, the timeline under it for the waveforms', libraryTab: 'audio' },
	{ id: 'motion', label: 'Motion', hint: 'Motion presets, titles, transitions and keyframes — preview, a wide inspector and the timeline', libraryTab: 'motion' },
	{ id: 'deliver', label: 'Deliver', hint: 'Check where the cut is going and export it', libraryTab: 'media' }
];

export function workspaceSpec(id: WorkspaceId): WorkspaceSpec {
	return WORKSPACE_SPECS.find((w) => w.id === id) ?? WORKSPACE_SPECS[0];
}

/** A workspace's name for storage and for the buttons: the id of a built-in one, or of one the user saved. */
export type WorkspaceKey = string;

/** A workspace the user saved: its own id (never one of the five) and the name on its button. */
export interface CustomWorkspace {
	id: WorkspaceKey;
	name: string;
}

/** How many workspaces a user may save: the title bar holds only so many buttons, and a hand-edited file is
 *  not to be believed about it. */
export const MAX_CUSTOM_WORKSPACES = 5;
/** The longest name a button carries (characters). */
export const MAX_WORKSPACE_NAME = 18;

/** What is remembered about the workspaces. `layouts` holds the saved ones' arrangements and the built-in ones
 *  that have been arranged; a built-in one with no entry is the preset. The library's `tabs` hold only the
 *  choices made; a missing one is the workspace's own tab. `collapsed` is the rail's, shared by every workspace.
 *  `order` is the buttons' order, every workspace once. */
export interface WorkspacesState {
	active: WorkspaceKey;
	layouts: Partial<Record<WorkspaceKey, SerializedDockview>>;
	/** For each stored built-in layout, the panels its preset opened when it was saved. */
	offered: Partial<Record<WorkspaceKey, PanelId[]>>;
	library: { tabs: Partial<Record<WorkspaceKey, LibraryTab>>; collapsed: boolean };
	custom: CustomWorkspace[];
	order: WorkspaceKey[];
}

export function defaultWorkspaces(): WorkspacesState {
	return { active: 'edit', layouts: {}, offered: {}, library: { tabs: {}, collapsed: false }, custom: [], order: [...WORKSPACE_IDS] };
}

/** Whether `key` is a built-in workspace (it has a preset, can be reset, and cannot be deleted). */
export function isBuiltinKey(key: unknown): key is WorkspaceId {
	return isWorkspaceId(key);
}

/** The built-in workspace whose preset `key` starts from: itself, or Edit for one the user saved (the structure a
 *  panel brought back is placed by, and what an unreadable layout of theirs falls back to). */
export function presetKey(key: WorkspaceKey): WorkspaceId {
	return isWorkspaceId(key) ? key : 'edit';
}

/** What a button needs to know about a workspace. */
export interface WorkspaceEntry {
	key: WorkspaceKey;
	label: string;
	hint: string;
	builtin: boolean;
}

/** The workspaces in the order of their buttons. */
export function workspaceEntries(state: WorkspacesState): WorkspaceEntry[] {
	return state.order.flatMap((key): WorkspaceEntry[] => {
		if (isWorkspaceId(key)) {
			const spec = workspaceSpec(key);
			return [{ key, label: spec.label, hint: spec.hint, builtin: true }];
		}
		const saved = state.custom.find((c) => c.id === key);
		return saved ? [{ key, label: saved.name, hint: 'A workspace you saved — your own arrangement of the panels', builtin: false }] : [];
	});
}

/** The name on a workspace's button. */
export function workspaceLabel(state: WorkspacesState, key: WorkspaceKey): string {
	if (isWorkspaceId(key)) return workspaceSpec(key).label;
	return state.custom.find((c) => c.id === key)?.name ?? workspaceSpec('edit').label;
}

/** The tab the library shows in `workspace`: the one picked there, else the one
 *  that workspace is about (a saved workspace has none of its own: Media). */
export function libraryTabFor(state: WorkspacesState, workspace: WorkspaceKey): LibraryTab {
	return state.library.tabs[workspace] ?? (isWorkspaceId(workspace) ? workspaceSpec(workspace).libraryTab : 'media');
}

/** `state` with `tab` picked for `workspace` and no other workspace's tab moved. */
export function withLibraryTab(state: WorkspacesState, workspace: WorkspaceKey, tab: LibraryTab): WorkspacesState {
	return { ...state, library: { ...state.library, tabs: { ...state.library.tabs, [workspace]: tab } } };
}

function isObj(v: unknown): v is Record<string, unknown> {
	return typeof v === 'object' && v !== null && !Array.isArray(v);
}

/** A workspace that gained panels in a stored layout. */
export interface Adopted {
	workspace: WorkspaceId;
	panels: PanelId[];
}

export interface WorkspacesRead {
	state: WorkspacesState;
	/** Whether reading changed what is stored (a layout stamped, dropped or given a
	 *  panel), so the caller writes the result back. */
	changed: boolean;
	/** Panels cut into a layout the user had arranged, for a notice. */
	adopted: Adopted[];
}

/** The panels a stored `offered` list names. Null when it is absent or not a
 *  list (the layout predates the record). */
function readOffered(raw: unknown): PanelId[] | null {
	if (!Array.isArray(raw)) return null;
	return raw.filter(isPanelId);
}

function sameSet(a: readonly string[], b: readonly string[]): boolean {
	return a.length === b.length && a.every((x) => b.includes(x));
}

/** A stored layout brought up to its workspace's preset of today.
 *
 *  `offered` is what the layout's preset opened when it was saved (null: before
 *  that was recorded, when it is taken to be the presets of that time). A panel
 *  the preset opens now that the layout was not offered and does not hold is new
 *  to it and is put in where the preset puts it; one the layout was offered and
 *  lacks was closed by the user and stays closed.
 *
 *  A layout from before the record that is only a copy of a preset — the build
 *  that wrote every workspace the user merely visited left one for each — is
 *  nobody's arrangement: it is dropped (`layout: null`), so the workspace is its
 *  preset, today's. */
export function adoptPanels(
	id: WorkspaceId,
	layout: SerializedDockview,
	offered: PanelId[] | null
): { layout: SerializedDockview | null; offered: PanelId[]; added: PanelId[]; changed: boolean } {
	const now = presetPanelIds(id);
	const had = offered ?? UNSTAMPED_OFFERED[id];
	const have = openPanelIds(layout);
	const fresh = now.filter((p) => !had.includes(p) && !have.includes(p));
	if (offered === null) {
		const earlier = EARLIER_PRESETS[id];
		if (sameArrangement(layout, presetLayout(id)) || (fresh.length > 0 && earlier && sameArrangement(layout, earlier))) {
			return { layout: null, offered: now, added: [], changed: true };
		}
	}
	let out = layout;
	const added: PanelId[] = [];
	for (const p of fresh) {
		const next = insertPanel(out, p, presetLayout(id));
		if (next) {
			out = next;
			added.push(p);
		}
	}
	return { layout: out, offered: now, added, changed: offered === null || added.length > 0 || !sameSet(had, now) };
}

/** The stored value as a state that can be trusted. Anything missing or invalid
 *  falls back to its default, field by field — one bad layout costs that
 *  workspace its arrangement, not the other four. Each surviving layout is then
 *  brought up to date with its preset (`adoptPanels`).
 *
 *  `legacyLayout` is the single arrangement saved before workspaces existed. It
 *  becomes the Edit workspace, but only when there is no `workspaces` value at
 *  all: once there is one, Edit is whatever it says (an Edit that was reset has
 *  no entry, and must not be brought back by a layout from the old build). */
export function readWorkspaces(raw: unknown, legacyLayout: unknown = null): WorkspacesRead {
	const out = defaultWorkspaces();
	const adopted: Adopted[] = [];
	let changed = false;
	const keep = (id: WorkspaceId, stored: unknown, offered: unknown) => {
		const layout = sanitizeLayout(stored);
		if (!layout) return;
		const r = adoptPanels(id, layout, readOffered(offered));
		changed ||= r.changed;
		if (!r.layout) return;
		out.layouts[id] = r.layout;
		out.offered[id] = r.offered;
		if (r.added.length) adopted.push({ workspace: id, panels: r.added });
	};
	if (!isObj(raw)) {
		keep('edit', legacyLayout, null);
		return { state: out, changed, adopted };
	}
	// The saved workspaces come first: `active`, the layouts, the tabs and the order all name them.
	const saved = readCustom(raw.custom);
	out.custom = saved.custom;
	changed ||= saved.dropped;
	const keys = new Set<string>([...WORKSPACE_IDS, ...out.custom.map((c) => c.id)]);
	out.order = readOrder(raw.order, out.custom);
	if (typeof raw.active === 'string' && keys.has(raw.active)) out.active = raw.active;
	if (isObj(raw.layouts)) {
		const offered = isObj(raw.offered) ? raw.offered : {};
		for (const id of WORKSPACE_IDS) keep(id, raw.layouts[id], offered[id]);
		// A saved workspace's layout is its own and follows no preset. One that cannot be trusted costs that workspace its
		// arrangement, not its name or the others: it opens as Edit does.
		for (const c of out.custom) {
			const layout = sanitizeLayout(raw.layouts[c.id]);
			if (layout) out.layouts[c.id] = layout;
			else if (raw.layouts[c.id] !== undefined) changed = true;
		}
	}
	if (isObj(raw.library)) {
		if (isObj(raw.library.tabs)) {
			for (const id of keys) {
				const tab = raw.library.tabs[id];
				if (isLibraryTab(tab)) out.library.tabs[id] = tab;
			}
		} else if (isLibraryTab(raw.library.tab)) {
			// Saved when one tab was shared by every workspace: it is the tab the
			// user was last looking at, in the workspace the app was left on.
			out.library.tabs[out.active] = raw.library.tab;
		}
		if (typeof raw.library.collapsed === 'boolean') out.library.collapsed = raw.library.collapsed;
	}
	return { state: out, changed, adopted };
}

export function parseWorkspaces(raw: unknown, legacyLayout: unknown = null): WorkspacesState {
	return readWorkspaces(raw, legacyLayout).state;
}

/** What to tell the user when a layout of theirs gained a panel, or null. */
export function describeAdopted(adopted: Adopted[]): { message: string; description: string } | null {
	if (adopted.length === 0) return null;
	const title = (id: WorkspaceId) => workspaceSpec(id).label;
	const panels = (a: Adopted) => a.panels.map((p) => PANELS[p].title).join(' and ');
	const one = adopted.length === 1;
	return {
		message: one
			? `${title(adopted[0].workspace)} workspace gained the ${panels(adopted[0])} panel${adopted[0].panels.length > 1 ? 's' : ''}`
			: 'Workspaces gained new panels',
		description: `${one ? '' : `${adopted.map((a) => `${title(a.workspace)}: ${panels(a)}`).join('; ')}. `}New in this version, put where the default arrangement has ${one ? 'it' : 'them'}; the rest of your arrangement is kept. Resetting the workspace restores its default.`
	};
}

// ---- changing what is stored ------------------------------------------------

/** `state` with `layout` stored for `key`. A built-in workspace's is recorded against the panels its preset opens in
 *  this build; a saved workspace's has no preset to record. */
export function withLayout(state: WorkspacesState, key: WorkspaceKey, layout: SerializedDockview): WorkspacesState {
	const layouts = { ...state.layouts, [key]: layout };
	if (!isWorkspaceId(key)) return { ...state, layouts };
	return { ...state, layouts, offered: { ...state.offered, [key]: presetPanelIds(key) } };
}

/** Whether any of the five workspaces that come with Kerf has an arrangement (or a library tab) of its own — something a
 *  reset would throw away. The ones the user saved are not reset, so they are not counted. */
export function hasStoredArrangement(w: { layouts: Record<string, unknown>; library: { tabs: Record<string, unknown> } }): boolean {
	return Object.keys(w.layouts).some(isWorkspaceId) || Object.keys(w.library.tabs).some(isWorkspaceId);
}

/** `state` with the workspaces in `ids` back to their presets: the stored
 *  arrangement and the library tab picked there forgotten. The library's fold is
 *  the rail's, not a workspace's, and stays. The same object when there was
 *  nothing to forget. */
export function withoutWorkspaces(state: WorkspacesState, ids: readonly WorkspaceId[]): WorkspacesState {
	const layouts = { ...state.layouts };
	const offered = { ...state.offered };
	const tabs = { ...state.library.tabs };
	let touched = false;
	for (const id of ids) {
		touched ||= id in layouts || id in offered || id in tabs;
		delete layouts[id];
		delete offered[id];
		delete tabs[id];
	}
	return touched ? { ...state, layouts, offered, library: { ...state.library, tabs } } : state;
}

/** The layout to put in front of the user for `key`: the one they arranged, or the preset (Edit's, for a saved
 *  workspace whose layout is gone). */
export function layoutFor(state: WorkspacesState, key: WorkspaceKey): SerializedDockview {
	return sanitizeLayout(state.layouts[key]) ?? presetLayout(presetKey(key));
}

/** Whether the dock's layout is worth writing down. Every change event is not:
 *  restoring a workspace, the library folding to its rail, a click that only
 *  moves the active group and a window resize all fire one, and writing each
 *  would mark every workspace the user merely visited as arranged (and bring a
 *  just-reset one straight back).
 *
 *  `reference` is what restoring would give back — the layout as it settled
 *  after the workspace was built, or the one last written — and `null` while
 *  that is still settling, when nothing is written. A layout is only worth
 *  keeping if it is arranged differently from that, and, when there is no entry
 *  yet, from the preset (which needs none). */
export function shouldPersistLayout(
	current: SerializedDockview,
	reference: SerializedDockview | null,
	preset: SerializedDockview,
	hasEntry: boolean
): boolean {
	if (!reference) return false;
	if (sameArrangement(current, reference)) return false;
	if (!hasEntry && sameArrangement(current, preset)) return false;
	return true;
}

// ---- the workspaces a user saves ---------------------------------------------

/** What a hand-edited or older file may call a saved workspace's id: ours (`w-` and a few characters), so no entry
 *  can stand in for a built-in one or carry anything odd into a selector or a key. */
const CUSTOM_KEY = /^w-[a-z0-9]{3,28}$/;

/** Characters that draw nothing or draw the text around them differently, which a button must not carry: the zero-width
 *  ones (space, joiners, word joiner, the byte-order mark, the soft hyphen, the fillers) and the bidirectional marks,
 *  embeddings, overrides and isolates (a name written `gnp.exe` that reads the other way). */
// eslint-disable-next-line no-misleading-character-class
const INVISIBLE = /[\u00ad\u034f\u061c\u115f\u1160\u17b4\u17b5\u180e\u200b-\u200f\u202a-\u202e\u2060-\u2064\u2066-\u206f\u3164\ufeff\uffa0]/g;

/** A name as typed, tidied: control characters become spaces, the invisible and direction-changing ones (`INVISIBLE`) go,
 *  runs of whitespace are one space, the ends are trimmed. What is left is what is checked and what is kept, so a name of
 *  nothing but zero-width characters is empty, and one that differs from another only by them is that other. */
export function tidyName(raw: string): string {
	// eslint-disable-next-line no-control-regex
	return raw.replace(/[\u0000-\u001f\u007f-\u009f]/g, ' ').replace(INVISIBLE, '').replace(/\s+/g, ' ').trim();
}

/** `name` if no one has it, else `name 2`, `name 3`, … the first free, the name cut as far as the number needs to fit
 *  `MAX_WORKSPACE_NAME`. `taken` holds lower-cased names. */
function freeName(name: string, taken: ReadonlySet<string>): string {
	if (!taken.has(name.toLowerCase())) return name;
	for (let n = 2; ; n++) {
		const suffix = ` ${n}`;
		const candidate = `${Array.from(name).slice(0, MAX_WORKSPACE_NAME - suffix.length).join('').trim()}${suffix}`;
		if (!taken.has(candidate.toLowerCase())) return candidate;
	}
}

/** The saved workspaces a stored value holds, each only if it reads: an id of ours, once, and a name. A name that is
 *  too long is cut rather than the workspace lost. `dropped` says something was not kept (or was changed), so the
 *  caller writes the tidy result back. */
function readCustom(raw: unknown): { custom: CustomWorkspace[]; dropped: boolean } {
	if (raw === undefined) return { custom: [], dropped: false };
	if (!Array.isArray(raw)) return { custom: [], dropped: true };
	const custom: CustomWorkspace[] = [];
	let dropped = false;
	// A name is one workspace's: the built-in labels and the names already read are taken, whatever the case.
	const taken = new Set<string>(WORKSPACE_SPECS.map((w) => w.label.toLowerCase()));
	// A few entries past the limit are looked at, so some bad ones ahead of good ones cost only themselves; a file of
	// thousands is not read through.
	const looked = raw.slice(0, MAX_CUSTOM_WORKSPACES * 4);
	for (const entry of looked) {
		const id = isObj(entry) ? entry.id : undefined;
		const name = isObj(entry) && typeof entry.name === 'string' ? tidyName(entry.name) : '';
		if (custom.length >= MAX_CUSTOM_WORKSPACES || typeof id !== 'string' || !CUSTOM_KEY.test(id) || custom.some((c) => c.id === id) || name === '') {
			dropped = true;
			continue;
		}
		const cut = Array.from(name).slice(0, MAX_WORKSPACE_NAME).join('').trim();
		// A name that clashes with another is renamed, deterministically, rather than the workspace dropped: it is the user's.
		const unique = freeName(cut, taken);
		if (unique !== (entry as Record<string, unknown>).name) dropped = true;
		taken.add(unique.toLowerCase());
		custom.push({ id, name: unique });
	}
	return { custom, dropped: dropped || raw.length > looked.length };
}

/** The order of the buttons a stored value holds: what it names that exists, once each, then whatever it left out in the
 *  default order (the built-in ones, then the saved ones as they were saved). */
function readOrder(raw: unknown, custom: readonly CustomWorkspace[]): WorkspaceKey[] {
	const known = [...WORKSPACE_IDS, ...custom.map((c) => c.id)] as WorkspaceKey[];
	const out: WorkspaceKey[] = [];
	if (Array.isArray(raw)) {
		for (const key of raw) if (typeof key === 'string' && known.includes(key) && !out.includes(key)) out.push(key);
	}
	for (const key of known) if (!out.includes(key)) out.push(key);
	return out;
}

export type NameCheck = { ok: true; name: string } | { ok: false; reason: string };

/** Whether `raw` can be a workspace's name: something is typed, it fits a button, and no other workspace (a built-in one
 *  included, whatever its case) already has it. `except` is the workspace being renamed. */
export function checkName(state: WorkspacesState, raw: string, except?: WorkspaceKey): NameCheck {
	const name = tidyName(raw);
	if (name === '') return { ok: false, reason: 'Give the workspace a name' };
	if (Array.from(name).length > MAX_WORKSPACE_NAME) return { ok: false, reason: `A name is at most ${MAX_WORKSPACE_NAME} characters` };
	const clash = workspaceEntries(state).find((w) => w.key !== except && w.label.toLowerCase() === name.toLowerCase());
	if (clash) return { ok: false, reason: `There is already a workspace called ${clash.label}` };
	return { ok: true, name };
}

/** A name to offer for a new workspace: "My workspace", then "My workspace 2", … the first that is free. */
export function suggestName(state: WorkspacesState): string {
	for (let n = 1; ; n++) {
		const name = n === 1 ? 'My workspace' : `My workspace ${n}`;
		if (checkName(state, name).ok) return name;
	}
}

/** Why another workspace cannot be saved, or null when it can. */
export function addBlocked(state: WorkspacesState): string | null {
	return state.custom.length >= MAX_CUSTOM_WORKSPACES
		? `You can keep ${MAX_CUSTOM_WORKSPACES} workspaces of your own — delete one to save another`
		: null;
}

/** An id for a workspace to be saved, one that no workspace of `state` has. `random` is `Math.random`. */
export function newWorkspaceKey(state: WorkspacesState, random: () => number = Math.random): WorkspaceKey {
	for (;;) {
		const key = `w-${Math.floor(random() * 36 ** 8).toString(36).padStart(8, '0')}`;
		if (!state.custom.some((c) => c.id === key)) return key;
	}
}

/** `state` with a workspace saved under `key`, called `name` and arranged as `layout`, its library showing `tab`; it
 *  goes last among the buttons and is the one on show. The caller has checked the name (`checkName`) and the room
 *  (`addBlocked`). */
export function addWorkspace(state: WorkspacesState, key: WorkspaceKey, name: string, layout: SerializedDockview, tab: LibraryTab): WorkspacesState {
	return {
		...state,
		active: key,
		custom: [...state.custom, { id: key, name }],
		layouts: { ...state.layouts, [key]: layout },
		library: { ...state.library, tabs: { ...state.library.tabs, [key]: tab } },
		order: [...state.order, key]
	};
}

/** `state` with the saved workspace `key` called `name`: the same object for a built-in one (it keeps its name), one that
 *  is not there, or a name it already has. */
export function renameWorkspace(state: WorkspacesState, key: WorkspaceKey, name: string): WorkspacesState {
	const at = state.custom.findIndex((c) => c.id === key);
	if (at < 0 || state.custom[at].name === name) return state;
	const custom = state.custom.slice();
	custom[at] = { id: key, name };
	return { ...state, custom };
}

/** The workspace whose button is beside `key`'s — the one before it, else the one after. */
function neighbour(order: readonly WorkspaceKey[], key: WorkspaceKey): WorkspaceKey {
	const at = order.indexOf(key);
	return order[at - 1] ?? order[at + 1] ?? 'edit';
}

/** `state` without the saved workspace `key`: its arrangement and its library tab forgotten, its button gone. When it was
 *  the one on show, the one beside its button is. The same object for a built-in workspace (it can be reset, not deleted)
 *  or one that is not there. */
export function removeWorkspace(state: WorkspacesState, key: WorkspaceKey): WorkspacesState {
	if (!state.custom.some((c) => c.id === key)) return state;
	const layouts = { ...state.layouts };
	const offered = { ...state.offered };
	const tabs = { ...state.library.tabs };
	delete layouts[key];
	delete offered[key];
	delete tabs[key];
	return {
		...state,
		active: state.active === key ? neighbour(state.order, key) : state.active,
		custom: state.custom.filter((c) => c.id !== key),
		layouts,
		offered,
		library: { ...state.library, tabs },
		order: state.order.filter((k) => k !== key)
	};
}

/** `state` with `key`'s button moved to place `to` (0 is first) among the buttons — the place it has once it is there.
 *  The same object when it is there already or is not a workspace. */
export function moveWorkspace(state: WorkspacesState, key: WorkspaceKey, to: number): WorkspacesState {
	const from = state.order.indexOf(key);
	if (from < 0 || !Number.isFinite(to)) return state;
	const at = Math.min(Math.max(Math.trunc(to), 0), state.order.length - 1);
	if (at === from) return state;
	const order = state.order.slice();
	order.splice(from, 1);
	order.splice(at, 0, key);
	return { ...state, order };
}

/** The workspace `by` places along the buttons from `key`, wrapping at the ends. */
export function stepWorkspace(state: WorkspacesState, key: WorkspaceKey, by: 1 | -1): WorkspaceKey {
	const n = state.order.length;
	const at = state.order.indexOf(key);
	if (n === 0) return key;
	return state.order[(at < 0 ? 0 : at + by + n) % n];
}
