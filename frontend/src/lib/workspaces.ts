// Workspaces: the editor in five arrangements (Edit / Color / Audio / Motion /
// Deliver), the library rail's tabs, and what the app remembers about both.
//
// What is stored is one opaque value in the app settings (`Settings.workspaces`
// on the Rust side — the backend never looks inside it):
//
//   { active, layouts: { <workspace>: <dockview layout> },
//     offered: { <workspace>: [<panel>, …] },
//     library: { tabs: { <workspace>: <tab> }, collapsed } }
//
// `offered` is, for each stored layout, the panels its workspace's preset opened
// when the layout was saved: how a later build's new panel (the mixer in Audio)
// can be told from one the user closed (see `adoptPanels`).
//
// The frontend owns that shape and validates it on the way back in, so a file
// from an older or newer build, a hand edit, or a truncated write degrades to
// the presets instead of leaving the editor without a timeline.

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

export const LIBRARY_TABS = ['media', 'titles', 'effects', 'transitions', 'audio', 'transcript'] as const;
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
	{ id: 'motion', label: 'Motion', hint: 'Titles, transitions and keyframes — preview, a wide inspector and the timeline', libraryTab: 'transitions' },
	{ id: 'deliver', label: 'Deliver', hint: 'Check where the cut is going and export it', libraryTab: 'media' }
];

export function workspaceSpec(id: WorkspaceId): WorkspaceSpec {
	return WORKSPACE_SPECS.find((w) => w.id === id) ?? WORKSPACE_SPECS[0];
}

/** What is remembered about the workspaces. `layouts` holds only the ones that
 *  have been arranged; a missing entry is the preset. The library's `tabs` hold
 *  only the choices made; a missing one is the workspace's own tab. `collapsed`
 *  is the rail's, shared by every workspace. */
export interface WorkspacesState {
	active: WorkspaceId;
	layouts: Partial<Record<WorkspaceId, SerializedDockview>>;
	/** For each stored layout, the panels its preset opened when it was saved. */
	offered: Partial<Record<WorkspaceId, PanelId[]>>;
	library: { tabs: Partial<Record<WorkspaceId, LibraryTab>>; collapsed: boolean };
}

export function defaultWorkspaces(): WorkspacesState {
	return { active: 'edit', layouts: {}, offered: {}, library: { tabs: {}, collapsed: false } };
}

/** The tab the library shows in `workspace`: the one picked there, else the one
 *  that workspace is about. */
export function libraryTabFor(state: WorkspacesState, workspace: WorkspaceId): LibraryTab {
	return state.library.tabs[workspace] ?? workspaceSpec(workspace).libraryTab;
}

/** `state` with `tab` picked for `workspace` and no other workspace's tab moved. */
export function withLibraryTab(state: WorkspacesState, workspace: WorkspaceId, tab: LibraryTab): WorkspacesState {
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
	if (isWorkspaceId(raw.active)) out.active = raw.active;
	if (isObj(raw.layouts)) {
		const offered = isObj(raw.offered) ? raw.offered : {};
		for (const id of WORKSPACE_IDS) keep(id, raw.layouts[id], offered[id]);
	}
	if (isObj(raw.library)) {
		if (isObj(raw.library.tabs)) {
			for (const id of WORKSPACE_IDS) {
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
		message: one ? `${title(adopted[0].workspace)} workspace gained the ${panels(adopted[0])} panel` : 'Workspaces gained new panels',
		description: `${one ? '' : `${adopted.map((a) => `${title(a.workspace)}: ${panels(a)}`).join('; ')}. `}New in this version, put where the default arrangement has ${one ? 'it' : 'them'}; the rest of your arrangement is kept. Resetting the workspace restores its default.`
	};
}

// ---- changing what is stored ------------------------------------------------

/** `state` with `layout` stored for `id`, recorded against the panels its preset
 *  opens in this build. */
export function withLayout(state: WorkspacesState, id: WorkspaceId, layout: SerializedDockview): WorkspacesState {
	return {
		...state,
		layouts: { ...state.layouts, [id]: layout },
		offered: { ...state.offered, [id]: presetPanelIds(id) }
	};
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

/** The layout to put in front of the user for `id`: the one they arranged, or
 *  the preset. */
export function layoutFor(state: WorkspacesState, id: WorkspaceId): SerializedDockview {
	return sanitizeLayout(state.layouts[id]) ?? presetLayout(id);
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
