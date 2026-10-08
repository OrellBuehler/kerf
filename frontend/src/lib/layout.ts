// The dockable workspace: which panels exist, how each workspace arranges them
// by default, and what a stored arrangement has to look like to be trusted.
//
// The arrangement is dockview's own serialized form, kept in the app settings
// (one per workspace, see `workspaces.ts`). It is validated structurally on the
// way back in — a panel id from an older build, a duplicated view or a
// truncated file must fall back to the preset rather than leave the editor
// without a timeline.

import type { GroupviewPanelState, Orientation, SerializedDockview, SerializedPopoutGroup } from 'dockview';

/** The page a detached panel's window opens at (`frontend/static/popout.html`). */
export const POPOUT_URL = '/popout.html';
/** How many windows a stored layout may hold: one per panel there is, none of them
 *  a reason to trust a hand-edited file to be reasonable. */
export const MAX_POPOUTS = 7;

export const PANEL_IDS = ['library', 'preview', 'timeline', 'inspector', 'agent', 'deliver', 'mixer'] as const;
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
	deliver: { title: 'Deliver', minimumWidth: 280, defaultWidth: 420 },
	// A strip is ~84 px wide and the master another ~130: two tracks and the master
	// fit in 340 px, and a third scrolls. Tall enough for a fader to have travel.
	mixer: { title: 'Mixer', minimumWidth: 340, minimumHeight: 230, defaultWidth: 460 }
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
	// Mixing: the mixer beside the picture, the timeline under both with the waveforms
	// it is read against. The library is narrow — audio effects and voiceover, not
	// browsing — and the inspector keeps what a clip's volume and effects need.
	audio: preset(
		450,
		[
			leaf('library', ['library'], 240),
			leaf('preview', ['preview'], 470),
			leaf('mixer', ['mixer'], 430),
			leaf('inspector', ['inspector', 'agent'], 300)
		],
		344
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
		if (!isObj(d) || typeof d.id !== 'string' || !Array.isArray(d.views)) return null;
		if (d.views.length === 0 && !hidden) return null;
		if (w.groups.has(d.id)) return null;
		w.groups.add(d.id);
		// What a group that was popped out into a window leaves behind in the grid: empty
		// and hidden, holding the place its panels return to. Kept for now; it is dropped
		// below unless a stored window still points at it.
		if (d.views.length === 0) {
			const empty: Leaf = { type: 'leaf', data: { id: d.id, views: [] }, visible: false };
			if (s !== undefined) empty.size = s;
			return [empty];
		}
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

/** The grid of a layout (the editor window's, or a detached window's own), or `null`
 *  when it cannot be trusted: the nodes walked, a root left with one branch child
 *  turned a level, the sizes kept. */
function sanitizeGrid(raw: unknown, w: Walk): SerializedDockview['grid'] | null {
	if (!isObj(raw)) return null;
	const orientation = raw.orientation;
	if (orientation !== 'HORIZONTAL' && orientation !== 'VERTICAL') return null;
	const width = size(raw.width);
	const height = size(raw.height);
	if (!width || !height) return null;
	if (!isObj(raw.root)) return null;
	// The root is always a branch; a lone group is wrapped in one.
	let kids = raw.root.type === 'branch' ? walkKids(raw.root, w) : walk(raw.root, w);
	if (!kids || kids.length === 0) return null;
	// A root left with a single branch child is that branch, one axis over.
	let axis = orientation as Orientation;
	while (kids.length === 1 && kids[0].type === 'branch') {
		kids = kids[0].data;
		axis = axis === VERTICAL ? HORIZONTAL : VERTICAL;
	}
	const root: Branch = { type: 'branch', data: kids };
	const rootSize = size(raw.root.size);
	if (rootSize !== undefined) root.size = rootSize;
	return { root, width, height, orientation: axis };
}

/** `nodes` without the empty (hidden) groups `keep` does not name. A branch left
 *  with nothing goes, one left with a single child is that child — `walk`'s rules. */
function pruneEmpty(nodes: Node[], keep: ReadonlySet<string>): Node[] {
	const out: Node[] = [];
	for (const n of nodes) {
		if (n.type === 'leaf') {
			if (n.data.views.length > 0 || keep.has(n.data.id)) out.push(n);
			continue;
		}
		const kids = pruneEmpty(n.data, keep);
		if (kids.length === 0) continue;
		if (kids.length === 1) {
			const only = kids[0];
			if (only.type === 'branch') out.push(...only.data);
			else out.push(n.size !== undefined ? { ...only, size: n.size } : only);
			continue;
		}
		out.push({ ...n, data: kids });
	}
	return out;
}

/** The popped-out groups of a stored layout that can be trusted. dockview writes a
 *  window as `data` (one group) or `grid` (a nested layout, once the user has split the
 *  window); both are read, through the same walk as the editor's own grid, so a panel
 *  is still shown once and a group id is still unique. The page is always the popout
 *  page — a stored URL is never opened — and a reference group the editor window no
 *  longer has is dropped (dockview then re-docks at the root). */
function sanitizePopouts(raw: unknown, w: Walk): SerializedPopoutGroup[] {
	if (!Array.isArray(raw)) return [];
	const out: SerializedPopoutGroup[] = [];
	for (const p of raw.slice(0, MAX_POPOUTS)) {
		if (!isObj(p)) continue;
		const popout: SerializedPopoutGroup = { url: POPOUT_URL, position: sanitizeBox(p.position) };
		// A window that does not read cleanly costs that window, not the layout: what was
		// walked of it is undone so its panels are free for the grid's to claim.
		const seen = snapshot(w);
		if (isObj(p.grid)) {
			const grid = sanitizeGrid(p.grid, w);
			if (!grid) {
				restore(w, seen);
				continue;
			}
			popout.grid = grid as NonNullable<SerializedPopoutGroup['grid']>;
		} else if (isObj(p.data)) {
			const nodes = walk({ type: 'leaf', data: p.data }, w);
			const only = nodes?.[0];
			if (!nodes || nodes.length !== 1 || only?.type !== 'leaf' || only.data.views.length === 0) {
				restore(w, seen);
				continue;
			}
			popout.data = only.data;
		} else {
			continue;
		}
		if (typeof p.gridReferenceGroup === 'string') popout.gridReferenceGroup = p.gridReferenceGroup;
		out.push(popout);
	}
	return out;
}

/** A window's place: finite numbers, a size that is a size. `null` (the platform's
 *  choice) for anything else. */
function sanitizeBox(raw: unknown): SerializedPopoutGroup['position'] {
	if (!isObj(raw)) return null;
	const { left, top, width, height } = raw;
	if (![left, top, width, height].every((v) => typeof v === 'number' && Number.isFinite(v))) return null;
	if ((width as number) < 1 || (height as number) < 1) return null;
	return { left: left as number, top: top as number, width: width as number, height: height as number };
}

function snapshot(w: Walk): Walk {
	return { ...w, stored: new Set(w.stored), views: new Map(w.views), groups: new Set(w.groups) };
}

function restore(w: Walk, from: Walk) {
	w.stored = from.stored;
	w.views = from.views;
	w.groups = from.groups;
	w.libraryTaken = from.libraryTaken;
}

/** A stored layout, or `null` when it cannot be trusted. Titles and minimum
 *  sizes are always taken from `PANELS`, so a rename or a retuned minimum
 *  reaches layouts saved before it. Floating groups are dropped (the workspace
 *  does not enable them); **popped-out groups are kept** — panels the user moved
 *  into windows of their own — if each reads cleanly, at most `MAX_POPOUTS` of
 *  them, and the editor window keeps a panel of its own. A layout saved when the
 *  media bin and the transcript were panels of their own is migrated: the first
 *  of those two becomes the library, the other is dropped, and a group (or
 *  branch) that leaves empty goes with it. */
export function sanitizeLayout(raw: unknown): SerializedDockview | null {
	if (!isObj(raw) || !isObj(raw.grid) || !isObj(raw.panels)) return null;
	const stored: string[] = [];
	viewIds(raw.grid.root, stored);
	for (const p of Array.isArray(raw.popoutGroups) ? raw.popoutGroups : []) {
		if (!isObj(p)) continue;
		if (isObj(p.data)) viewIds({ type: 'leaf', data: p.data }, stored);
		if (isObj(p.grid)) viewIds(p.grid.root, stored);
	}
	const w: Walk = {
		stored: new Set(),
		views: new Map(),
		groups: new Set(),
		hasLibrary: stored.includes('library'),
		libraryTaken: false
	};
	const grid = sanitizeGrid(raw.grid, w);
	if (!grid) return null;
	const popouts = sanitizePopouts(raw.popoutGroups, w);
	// The empty groups kept for a window to come back to are kept only for one that
	// survived; the rest are noise.
	const keep = new Set(popouts.flatMap((p) => (p.gridReferenceGroup ? [p.gridReferenceGroup] : [])));
	const rootKids = pruneEmpty((grid.root as Branch).data, keep);
	if (rootKids.length === 0 || !hasShownPanel(rootKids)) return null;
	let kids = rootKids;
	let axis = grid.orientation as Orientation;
	while (kids.length === 1 && kids[0].type === 'branch') {
		kids = kids[0].data;
		axis = axis === VERTICAL ? HORIZONTAL : VERTICAL;
	}
	const root: Branch = { type: 'branch', data: kids };
	if ((grid.root as Branch).size !== undefined) root.size = (grid.root as Branch).size;
	if (w.views.size === 0) return null;

	const panels: Record<string, GroupviewPanelState> = {};
	for (const [id, from] of w.views) {
		if (!isPanelId(id)) return null;
		const p = raw.panels[from];
		if (!isObj(p) || p.contentComponent !== from) return null;
		panels[id] = panelState(id);
	}
	const layout: SerializedDockview = { grid: { root, width: grid.width, height: grid.height, orientation: axis }, panels };
	const groups = new Set<string>();
	groupIds(kids, groups);
	// A reference group that is not in the grid any more is not pointed at.
	if (popouts.length > 0) {
		layout.popoutGroups = popouts.map((p) => (p.gridReferenceGroup && !groups.has(p.gridReferenceGroup) ? omitReference(p) : p));
	}
	if (typeof raw.activeGroup === 'string' && groups.has(raw.activeGroup)) layout.activeGroup = raw.activeGroup;
	return layout;
}

function omitReference(p: SerializedPopoutGroup): SerializedPopoutGroup {
	const { gridReferenceGroup: _ignored, ...rest } = p;
	return rest;
}

/** Whether the editor window holds a panel at all. */
function hasShownPanel(nodes: Node[]): boolean {
	return nodes.some((n) => (n.type === 'leaf' ? n.data.views.length > 0 : hasShownPanel(n.data)));
}

// ---- comparing arrangements ------------------------------------------------

/** How far a group's share of its branch may drift (as a fraction of the branch)
 *  before two layouts count as arranged differently: pixel rounding, a few
 *  hundredths of a percent. Small enough that a deliberate nudge of a sash — a
 *  dozen pixels on a wide window — is a rearrangement; a window resize is not
 *  held against this (the workspace takes a fresh reference when one settles). */
export const ARRANGEMENT_TOLERANCE = 0.004;

/** The share each child takes of its branch; equal shares when sizes are absent. */
function shares(nodes: Node[]): number[] {
	const total = nodes.reduce((sum, n) => sum + (typeof n.size === 'number' && n.size > 0 ? n.size : 0), 0);
	const sized = nodes.every((n) => typeof n.size === 'number' && n.size > 0);
	return nodes.map((n) => (sized && total > 0 ? (n.size as number) / total : 1 / nodes.length));
}

function sameNode(a: unknown, b: unknown, tolerance: number): boolean {
	if (!isObj(a) || !isObj(b) || a.type !== b.type) return false;
	if ((a.visible === false) !== (b.visible === false)) return false;
	if (a.type === 'leaf') {
		const x = a.data;
		const y = b.data;
		if (!isObj(x) || !isObj(y) || x.id !== y.id) return false;
		const xv = x.views;
		const yv = y.views;
		if (!Array.isArray(xv) || !Array.isArray(yv) || xv.length !== yv.length) return false;
		return xv.every((v, i) => v === yv[i]);
	}
	const ad = a.data;
	const bd = b.data;
	if (a.type !== 'branch' || !Array.isArray(ad) || !Array.isArray(bd) || ad.length !== bd.length) return false;
	const sa = shares(ad as Node[]);
	const sb = shares(bd as Node[]);
	return ad.every((child, i) => Math.abs(sa[i] - sb[i]) <= tolerance && sameNode(child, bd[i], tolerance));
}

/** Whether two layouts arrange the panels the same way: the same groups holding
 *  the same panels in the same order, split the same way, at the same shares of
 *  their branches (within `tolerance`). Pixel sizes, which move with the window,
 *  the active group and the active tab (which a click changes) do not count. */
export function sameArrangement(
	a: SerializedDockview,
	b: SerializedDockview,
	tolerance = ARRANGEMENT_TOLERANCE
): boolean {
	return (
		a.grid.orientation === b.grid.orientation &&
		sameNode(a.grid.root, b.grid.root, tolerance) &&
		samePopouts(a.popoutGroups ?? [], b.popoutGroups ?? [])
	);
}

/** How far a window may have moved or been resized, in pixels, and still be where it
 *  was: the platform nudges a window by a title bar or a shadow when it places one. A
 *  window the user took hold of moves further than this. */
export const WINDOW_TOLERANCE_PX = 12;

/** The panels a detached window shows, in order, whichever way it was written. */
export function popoutViews(p: SerializedPopoutGroup): string[] {
	if (p.data) return [...p.data.views];
	const out: string[] = [];
	if (p.grid) viewIds(p.grid.root, out);
	return out;
}

/** Whether two sets of detached windows are the same: the same panels in each, in the
 *  same order, each window where it was (within `WINDOW_TOLERANCE_PX`) and returning
 *  to the same group. */
function samePopouts(a: readonly SerializedPopoutGroup[], b: readonly SerializedPopoutGroup[]): boolean {
	if (a.length !== b.length) return false;
	return a.every((x, i) => {
		const y = b[i];
		const xv = popoutViews(x);
		const yv = popoutViews(y);
		if (xv.length !== yv.length || !xv.every((v, k) => v === yv[k])) return false;
		if (x.gridReferenceGroup !== y.gridReferenceGroup) return false;
		if (!x.position || !y.position) return !x.position && !y.position;
		return (['left', 'top', 'width', 'height'] as const).every((k) => Math.abs(x.position![k] - y.position![k]) <= WINDOW_TOLERANCE_PX);
	});
}

/** The panels a layout shows. */
export function openPanelIds(layout: SerializedDockview): PanelId[] {
	return Object.keys(layout.panels).filter(isPanelId);
}

// ---- keeping a stored layout up to date -------------------------------------
//
// A stored layout is a snapshot, and a snapshot does not learn about a panel a
// later build adds to its workspace's preset (the mixer in Audio). So each one
// is stored with the panels its preset offered at the time (`workspaces.ts`),
// and a panel the preset offers now that the layout was never offered is put
// into it where the preset puts it. A panel the layout *was* offered and does
// not hold is one the user closed, and stays closed.

/** The panels a workspace's preset opens. */
export function presetPanelIds(id: WorkspaceId): PanelId[] {
	return openPanelIds(PRESET_LAYOUTS[id]);
}

/** What a layout stored before layouts recorded it was offered: the panels of
 *  each preset as they stood when that was the only kind there was — every
 *  preset as it is now, bar the mixer Audio gained. */
export const UNSTAMPED_OFFERED: Record<WorkspaceId, PanelId[]> = Object.fromEntries(
	WORKSPACE_IDS.map((id) => [id, presetPanelIds(id).filter((p) => p !== 'mixer')])
) as Record<WorkspaceId, PanelId[]>;

/** Presets as an earlier build shipped them, for the ones that changed shape
 *  and not just size. The build that wrote a layout for every workspace the
 *  user merely visited left a copy of the preset of its day; a copy of one of
 *  these is nobody's arrangement and becomes today's preset rather than a
 *  layout with a panel cut into it. */
export const EARLIER_PRESETS: Partial<Record<WorkspaceId, SerializedDockview>> = {
	// Before the mixer: a tall timeline and the inspector wide.
	audio: preset(
		330,
		[
			leaf('library', ['library'], 280),
			leaf('preview', ['preview'], 740),
			leaf('inspector', ['inspector', 'agent'], 420)
		],
		464
	)
};

interface Spot {
	leaf: Leaf;
	/** The array the leaf sits in, and where. */
	siblings: Node[];
	index: number;
	/** How deep that array is: the root's children are level 0. */
	level: number;
}

function findLeaf(nodes: Node[], level: number, has: (views: string[]) => boolean): Spot | null {
	for (let i = 0; i < nodes.length; i++) {
		const n = nodes[i];
		if (n.type === 'leaf') {
			if (has(n.data.views)) return { leaf: n, siblings: nodes, index: i, level };
		} else {
			const found = findLeaf(n.data, level + 1, has);
			if (found) return found;
		}
	}
	return null;
}

function leavesOf(node: Node): Leaf[] {
	return node.type === 'leaf' ? [node] : node.data.flatMap(leavesOf);
}

/** The way the children of the branch at `level` are laid out: the root's along
 *  the grid's own orientation, each level down across the one above. */
function axisAt(root: Orientation, level: number): Orientation {
	if (level % 2 === 0) return root;
	return root === VERTICAL ? HORIZONTAL : VERTICAL;
}

/** Cut a new group for `id` into `anchor`'s row, on the side `side`, taking the
 *  share the preset gives it out of what is there. Null when the sizes of the
 *  row are not numbers to work with. */
function placeBeside(
	anchor: Spot,
	side: 'before' | 'after',
	made: Leaf,
	share: number,
	minimum: number | undefined
): boolean {
	const row = anchor.siblings;
	if (!row.every((n) => typeof n.size === 'number' && n.size > 0)) return false;
	const total = row.reduce((sum, n) => sum + (n.size as number), 0);
	const want = Math.min(Math.max(Math.round(share * total), minimum ?? 0, 1), Math.floor(total * 0.75));
	const keep = (total - want) / total;
	let used = 0;
	for (const n of row) {
		n.size = Math.max(1, Math.round((n.size as number) * keep));
		used += n.size;
	}
	made.size = Math.max(1, total - used);
	row.splice(anchor.index + (side === 'after' ? 1 : 0), 0, made);
	return true;
}

/** `layout` with `id` put where `preset` puts it, or null when `preset` has no
 *  such panel. Reliable by construction, in order of preference:
 *  1. tabbed with a panel it shares a group with in the preset, if that is there;
 *  2. in a group of its own beside the nearest panel it sits next to in the
 *     preset, taking the share the preset gives it (when that panel's row runs
 *     the same way as the preset's);
 *  3. as a tab in the group holding the preview (or, failing that, any group).
 *  So a layout is never reset to make room, and the panel is always reachable. */
export function insertPanel(layout: SerializedDockview, id: PanelId, preset: SerializedDockview): SerializedDockview | null {
	const out = structuredClone(layout);
	const mine = (out.grid.root as Branch).data;
	if (findLeaf(mine, 0, (v) => v.includes(id))) return out;
	const where = findLeaf((preset.grid.root as Branch).data, 0, (v) => v.includes(id));
	if (!where) return null;

	const done = () => {
		out.panels[id] = panelState(id);
		return out;
	};

	// 1. Beside its tab mates.
	for (const mate of where.leaf.data.views) {
		if (mate === id) continue;
		const found = findLeaf(mine, 0, (v) => v.includes(mate));
		if (!found) continue;
		const at = where.leaf.data.views.indexOf(id);
		found.leaf.data.views.splice(Math.min(at, found.leaf.data.views.length), 0, id);
		return done();
	}

	// 2. A group of its own next to a neighbour.
	const axis = axisAt(preset.grid.orientation, where.level);
	const groups = new Set<string>();
	groupIds(mine, groups);
	const taken = (gid: string) => groups.has(gid);
	let gid = where.leaf.data.id;
	for (let n = 2; taken(gid); n++) gid = `${id}-${n}`;
	const share = shares(where.siblings)[where.index];
	const spec = PANELS[id];
	const minimum = axis === HORIZONTAL ? spec.minimumWidth : spec.minimumHeight;
	for (let k = 1; k < where.siblings.length; k++) {
		for (const [at, side] of [
			[where.index - k, 'after'],
			[where.index + k, 'before']
		] as const) {
			const sibling = where.siblings[at];
			if (!sibling) continue;
			for (const near of leavesOf(sibling)) {
				for (const view of near.data.views) {
					const anchor = findLeaf(mine, 0, (v) => v.includes(view));
					if (!anchor || axisAt(out.grid.orientation, anchor.level) !== axis) continue;
					const made: Leaf = { type: 'leaf', data: { id: gid, views: [id], activeView: id } };
					if (placeBeside(anchor, side, made, share, minimum)) return done();
				}
			}
		}
	}

	// 3. A tab where the user already looks.
	const home = findLeaf(mine, 0, (v) => v.includes('preview')) ?? findLeaf(mine, 0, () => true);
	if (!home) return null;
	home.leaf.data.views.push(id);
	return done();
}
