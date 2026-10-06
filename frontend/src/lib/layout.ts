// The dockable workspace: which panels exist, how each workspace arranges them
// by default, and what a stored arrangement has to look like to be trusted.
//
// The arrangement is dockview's own serialized form, kept in the app settings
// (one per workspace, see `workspaces.ts`). It is validated structurally on the
// way back in — a panel id from an older build, a duplicated view or a
// truncated file must fall back to the preset rather than leave the editor
// without a timeline.

import type { GroupviewPanelState, Orientation, SerializedDockview } from 'dockview';

export const PANEL_IDS = ['library', 'preview', 'timeline', 'inspector', 'agent', 'deliver'] as const;
export type PanelId = (typeof PANEL_IDS)[number];

/** The width of the library's icon rail. The panel's registry minimum, so a
 *  collapsed library may shrink to exactly this and no further. */
export const LIBRARY_RAIL_WIDTH = 40;
/** The narrowest the library may be while it is open: the rail plus what a
 *  media row needs to stay legible. */
export const LIBRARY_MIN_OPEN_WIDTH = 240;
/** What the library grows back to when it is expanded and has no better memory. */
export const LIBRARY_OPEN_WIDTH = 300;

export interface PanelSpec {
	title: string;
	minimumWidth?: number;
	minimumHeight?: number;
	/** The width a closed panel opens at when it is brought back. */
	defaultWidth?: number;
}

export const PANELS: Record<PanelId, PanelSpec> = {
	// Small on purpose: a collapsed library is just its rail. The panel raises
	// its own minimum to `LIBRARY_MIN_OPEN_WIDTH` while it is open.
	library: { title: 'Library', minimumWidth: LIBRARY_RAIL_WIDTH, defaultWidth: LIBRARY_OPEN_WIDTH },
	preview: { title: 'Preview', minimumWidth: 240, minimumHeight: 160 },
	timeline: { title: 'Timeline', minimumHeight: 140 },
	inspector: { title: 'Inspector', minimumWidth: 250 },
	agent: { title: 'Agent', minimumWidth: 260 },
	deliver: { title: 'Deliver', minimumWidth: 280, defaultWidth: 420 }
};

export function isPanelId(id: unknown): id is PanelId {
	return typeof id === 'string' && (PANEL_IDS as readonly string[]).includes(id);
}

export function panelState(id: PanelId): GroupviewPanelState {
	const spec = PANELS[id];
	const state: GroupviewPanelState = { id, contentComponent: id, title: spec.title };
	if (spec.minimumWidth !== undefined) state.minimumWidth = spec.minimumWidth;
	if (spec.minimumHeight !== undefined) state.minimumHeight = spec.minimumHeight;
	return state;
}

// ---- workspace presets -----------------------------------------------------

export const WORKSPACE_IDS = ['edit', 'color', 'audio', 'motion', 'deliver'] as const;
export type WorkspaceId = (typeof WORKSPACE_IDS)[number];

export function isWorkspaceId(id: unknown): id is WorkspaceId {
	return typeof id === 'string' && (WORKSPACE_IDS as readonly string[]).includes(id);
}

// dockview's `SerializedGridObject` does not say which of its `data` shapes goes
// with which `type`; these do, and are assignable to it.
type LeafData = { id: string; views: string[]; activeView?: string };
type Leaf = { type: 'leaf'; data: LeafData; size?: number; visible?: boolean };
type Branch = { type: 'branch'; data: Node[]; size?: number; visible?: boolean };
type Node = Leaf | Branch;

const VERTICAL = 'VERTICAL' as Orientation;
const HORIZONTAL = 'HORIZONTAL' as Orientation;

function leaf(id: string, views: PanelId[], size: number): Leaf {
	return { type: 'leaf', data: { id, views, activeView: views[0] }, size };
}

/** A workspace is one row of panels above a full-width timeline. The numbers
 *  are a 1440×794 reference frame: dockview scales them to the real window, so
 *  only their proportions matter. */
function preset(top: number, row: Leaf[], timeline: number): SerializedDockview {
	const views = new Set<string>(['timeline']);
	const note = (n: Node) => {
		if (n.type === 'leaf') for (const v of n.data.views) views.add(v);
		else n.data.forEach(note);
	};
	row.forEach(note);
	return {
		grid: {
			root: {
				type: 'branch',
				size: 1440,
				data: [{ type: 'branch', size: top, data: row }, leaf('timeline', ['timeline'], timeline)]
			},
			width: 1440,
			height: 794,
			orientation: VERTICAL
		},
		panels: Object.fromEntries([...views].map((id) => [id, panelState(id as PanelId)])),
		activeGroup: 'preview'
	};
}

/** What each workspace looks like before anyone has rearranged it. The cut is
 *  what an editor looks at most, so the timeline always runs the full width and
 *  the agent stays a tab beside the inspector (or the deliver panel), so a
 *  proposal that lands has somewhere to appear in every workspace. */
export const PRESET_LAYOUTS: Record<WorkspaceId, SerializedDockview> = {
	// Editing: library | preview | inspector, the agent tabbed with it.
	edit: preset(
		490,
		[
			leaf('library', ['library'], 300),
			leaf('preview', ['preview'], 800),
			leaf('inspector', ['inspector', 'agent'], 340)
		],
		304
	),
	// Grading: the picture as large as it gets, the controls beside it, and a
	// short timeline. The library is narrow — it is there for looks, not browsing.
	color: preset(
		560,
		[
			leaf('library', ['library'], 240),
			leaf('preview', ['preview'], 820),
			leaf('inspector', ['inspector', 'agent'], 380)
		],
		234
	),
	// Mixing: the timeline is where the waveforms are, so it gets the height.
	audio: preset(
		330,
		[
			leaf('library', ['library'], 280),
			leaf('preview', ['preview'], 740),
			leaf('inspector', ['inspector', 'agent'], 420)
		],
		464
	),
	// Animating: preview, a wide inspector for keyframes, the timeline under it.
	motion: preset(
		480,
		[
			leaf('library', ['library'], 240),
			leaf('preview', ['preview'], 780),
			leaf('inspector', ['inspector', 'agent'], 420)
		],
		314
	),
	// Delivering: the cut, and where it is going.
	deliver: preset(
		520,
		[leaf('preview', ['preview'], 880), leaf('deliver', ['deliver', 'agent'], 560)],
		274
	)
};

/** The Edit arrangement — what the dock opened with before there were
 *  workspaces, and still what a fresh install shows. */
export const DEFAULT_LAYOUT: SerializedDockview = PRESET_LAYOUTS.edit;

/** A fresh copy of a workspace's preset, safe for dockview to take over. */
export function presetLayout(id: WorkspaceId): SerializedDockview {
	return structuredClone(PRESET_LAYOUTS[id]);
}

// ---- validating a stored layout -------------------------------------------

/** The panels `library` replaced. The old media bin and transcript tabs are one
 *  rail now; a layout saved with either shows the library where it was. */
const LEGACY_LIBRARY_IDS: readonly string[] = ['media', 'bin', 'transcript'];

function isObj(v: unknown): v is Record<string, unknown> {
	return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function size(v: unknown): number | undefined {
	return typeof v === 'number' && Number.isFinite(v) && v >= 0 ? v : undefined;
}

function viewIds(node: unknown, out: string[]) {
	if (!isObj(node)) return;
	if (node.type === 'leaf' && isObj(node.data) && Array.isArray(node.data.views)) {
		for (const v of node.data.views) if (typeof v === 'string') out.push(v);
	} else if (node.type === 'branch' && Array.isArray(node.data)) {
		for (const c of node.data) viewIds(c, out);
	}
}

interface Walk {
	/** Every view as it was stored, to refuse the same panel twice. */
	stored: Set<string>;
	/** The views as they will be, and the stored panel each one came from. */
	views: Map<string, string>;
	groups: Set<string>;
	/** Whether the layout already has a library of its own. It then wins, and
	 *  the old tabs simply drop out. */
	hasLibrary: boolean;
	libraryTaken: boolean;
}

/** What a stored view becomes: itself, the library (the first of the old media
 *  / transcript tabs found), or nothing (any other of them). */
function mapView(view: string, w: Walk): string | null {
	if (!LEGACY_LIBRARY_IDS.includes(view)) return view;
	if (w.hasLibrary || w.libraryTaken) return null;
	w.libraryTaken = true;
	return 'library';
}

/** A node as zero or more nodes: a group whose panels all dropped out is gone,
 *  and a branch left with one child is that child (a branch whose only child is
 *  another branch hands over its grandchildren, which lie along the same axis as
 *  the branch's own siblings). `null` is a node that cannot be trusted. */
function walk(node: unknown, w: Walk): Node[] | null {
	if (!isObj(node)) return null;
	const s = size(node.size);
	const hidden = node.visible === false;
	if (node.type === 'leaf') {
		const d = node.data;
		if (!isObj(d) || typeof d.id !== 'string' || !Array.isArray(d.views) || d.views.length === 0) return null;
		if (w.groups.has(d.id)) return null;
		w.groups.add(d.id);
		const views: string[] = [];
		let activeView: string | undefined;
		for (const v of d.views) {
			if (typeof v !== 'string' || w.stored.has(v)) return null;
			w.stored.add(v);
			const id = mapView(v, w);
			if (id === null) continue;
			if (w.views.has(id)) return null;
			w.views.set(id, v);
			views.push(id);
			if (v === d.activeView) activeView = id;
		}
		if (views.length === 0) return [];
		const out: Leaf = { type: 'leaf', data: { id: d.id, views, activeView: activeView ?? views[0] } };
		if (s !== undefined) out.size = s;
		if (hidden) out.visible = false;
		return [out];
	}
	if (node.type === 'branch') {
		const kids = walkKids(node, w);
		if (!kids) return null;
		if (kids.length === 0) return [];
		if (kids.length === 1) {
			const only = kids[0];
			if (only.type === 'branch') return only.data;
			const out = { ...only };
			if (s !== undefined) out.size = s;
			return [out];
		}
		const out: Branch = { type: 'branch', data: kids };
		if (s !== undefined) out.size = s;
		if (hidden) out.visible = false;
		return [out];
	}
	return null;
}

function walkKids(branch: Record<string, unknown>, w: Walk): Node[] | null {
	if (!Array.isArray(branch.data) || branch.data.length === 0) return null;
	const kids: Node[] = [];
	for (const c of branch.data) {
		const nodes = walk(c, w);
		if (!nodes) return null;
		kids.push(...nodes);
	}
	return kids;
}

function groupIds(nodes: Node[], out: Set<string>) {
	for (const n of nodes) {
		if (n.type === 'leaf') out.add(n.data.id);
		else groupIds(n.data, out);
	}
}

/** A stored layout, or `null` when it cannot be trusted. Titles and minimum
 *  sizes are always taken from `PANELS`, so a rename or a retuned minimum
 *  reaches layouts saved before it. Floating and popout groups are dropped:
 *  the workspace does not enable them. A layout saved when the media bin and
 *  the transcript were panels of their own is migrated: the first of those two
 *  becomes the library, the other is dropped, and a group (or branch) that
 *  leaves empty goes with it. */
export function sanitizeLayout(raw: unknown): SerializedDockview | null {
	if (!isObj(raw) || !isObj(raw.grid) || !isObj(raw.panels)) return null;
	const grid = raw.grid;
	const orientation = grid.orientation;
	if (orientation !== 'HORIZONTAL' && orientation !== 'VERTICAL') return null;
	const width = size(grid.width);
	const height = size(grid.height);
	if (!width || !height) return null;
	if (!isObj(grid.root)) return null;

	const stored: string[] = [];
	viewIds(grid.root, stored);
	const w: Walk = {
		stored: new Set(),
		views: new Map(),
		groups: new Set(),
		hasLibrary: stored.includes('library'),
		libraryTaken: false
	};
	// The root is always a branch; a lone group is wrapped in one.
	let kids = grid.root.type === 'branch' ? walkKids(grid.root, w) : walk(grid.root, w);
	if (!kids || kids.length === 0 || w.views.size === 0) return null;

	// A root left with a single branch child is that branch, one axis over.
	let axis = orientation as Orientation;
	while (kids.length === 1 && kids[0].type === 'branch') {
		kids = kids[0].data;
		axis = axis === VERTICAL ? HORIZONTAL : VERTICAL;
	}
	const root: Branch = { type: 'branch', data: kids };
	const rootSize = size(grid.root.size);
	if (rootSize !== undefined) root.size = rootSize;

	const panels: Record<string, GroupviewPanelState> = {};
	for (const [id, from] of w.views) {
		if (!isPanelId(id)) return null;
		const p = raw.panels[from];
		if (!isObj(p) || p.contentComponent !== from) return null;
		panels[id] = panelState(id);
	}
	const layout: SerializedDockview = { grid: { root, width, height, orientation: axis }, panels };
	const groups = new Set<string>();
	groupIds(kids, groups);
	if (typeof raw.activeGroup === 'string' && groups.has(raw.activeGroup)) layout.activeGroup = raw.activeGroup;
	return layout;
}

/** The panels a layout shows. */
export function openPanelIds(layout: SerializedDockview): PanelId[] {
	return Object.keys(layout.panels).filter(isPanelId);
}
