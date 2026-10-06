// Workspaces: the editor in five arrangements (Edit / Color / Audio / Motion /
// Deliver), the library rail's tabs, and what the app remembers about both.
//
// What is stored is one opaque value in the app settings (`Settings.workspaces`
// on the Rust side — the backend never looks inside it):
//
//   { active, layouts: { <workspace>: <dockview layout> }, library: { tab, collapsed } }
//
// The frontend owns that shape and validates it on the way back in, so a file
// from an older or newer build, a hand edit, or a truncated write degrades to
// the presets instead of leaving the editor without a timeline.

import type { SerializedDockview } from 'dockview';
import { WORKSPACE_IDS, isWorkspaceId, presetLayout, sanitizeLayout, type WorkspaceId } from './layout';

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
	/** The library tab this workspace opens on when it is switched to, for the
	 *  ones that are about one kind of tool. Edit has none: it keeps whatever
	 *  the library was showing. */
	libraryTab?: LibraryTab;
}

export const WORKSPACE_SPECS: WorkspaceSpec[] = [
	{ id: 'edit', label: 'Edit', hint: 'Cut and arrange — library, preview, inspector and a full-width timeline' },
	{ id: 'color', label: 'Color', hint: 'Grade the picture — the preview as large as it gets, controls beside it', libraryTab: 'effects' },
	{ id: 'audio', label: 'Audio', hint: 'Work on sound — a tall timeline for the waveforms', libraryTab: 'audio' },
	{ id: 'motion', label: 'Motion', hint: 'Titles, transitions and keyframes — preview, a wide inspector and the timeline', libraryTab: 'transitions' },
	{ id: 'deliver', label: 'Deliver', hint: 'Check where the cut is going and export it' }
];

export function workspaceSpec(id: WorkspaceId): WorkspaceSpec {
	return WORKSPACE_SPECS.find((w) => w.id === id) ?? WORKSPACE_SPECS[0];
}

/** What is remembered about the workspaces. `layouts` holds only the ones that
 *  have been arranged; a missing entry is the preset. */
export interface WorkspacesState {
	active: WorkspaceId;
	layouts: Partial<Record<WorkspaceId, SerializedDockview>>;
	library: { tab: LibraryTab; collapsed: boolean };
}

export function defaultWorkspaces(): WorkspacesState {
	return { active: 'edit', layouts: {}, library: { tab: 'media', collapsed: false } };
}

function isObj(v: unknown): v is Record<string, unknown> {
	return typeof v === 'object' && v !== null && !Array.isArray(v);
}

/** The stored value as a state that can be trusted. Anything missing or invalid
 *  falls back to its default, field by field — one bad layout costs that
 *  workspace its arrangement, not the other four.
 *
 *  `legacyLayout` is the single arrangement saved before workspaces existed. It
 *  becomes the Edit workspace, but only when there is no `workspaces` value at
 *  all: once there is one, Edit is whatever it says (an Edit that was reset has
 *  no entry, and must not be brought back by a layout from the old build). */
export function parseWorkspaces(raw: unknown, legacyLayout: unknown = null): WorkspacesState {
	const out = defaultWorkspaces();
	if (!isObj(raw)) {
		const legacy = sanitizeLayout(legacyLayout);
		if (legacy) out.layouts.edit = legacy;
		return out;
	}
	if (isWorkspaceId(raw.active)) out.active = raw.active;
	if (isObj(raw.layouts)) {
		for (const id of WORKSPACE_IDS) {
			const layout = sanitizeLayout(raw.layouts[id]);
			if (layout) out.layouts[id] = layout;
		}
	}
	if (isObj(raw.library)) {
		if (isLibraryTab(raw.library.tab)) out.library.tab = raw.library.tab;
		if (typeof raw.library.collapsed === 'boolean') out.library.collapsed = raw.library.collapsed;
	}
	return out;
}

/** The layout to put in front of the user for `id`: the one they arranged, or
 *  the preset. */
export function layoutFor(state: WorkspacesState, id: WorkspaceId): SerializedDockview {
	return sanitizeLayout(state.layouts[id]) ?? presetLayout(id);
}
