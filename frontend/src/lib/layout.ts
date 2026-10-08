// The dockable workspace: which panels exist, how each workspace arranges them
// by default, and what a stored arrangement has to look like to be trusted.
//
// The arrangement is dockview's own serialized form, kept in the app settings
// (one per workspace, see `workspaces.ts`). It is validated structurally on the
// way back in — a panel id from an older build, a duplicated view or a
// truncated file must fall back to the preset rather than leave the editor
// without a timeline.

import type { GroupviewPanelState, Orientation, SerializedDockview } from 'dockview';

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
	return a.grid.orientation === b.grid.orientation && sameNode(a.grid.root, b.grid.root, tolerance);
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
