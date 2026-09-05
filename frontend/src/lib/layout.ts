// The dockable workspace: which panels exist, how they are arranged by
// default, and what a stored arrangement has to look like to be trusted.
//
// The arrangement is dockview's own serialized form, kept in the app settings.
// It is validated structurally on the way back in — a panel id from an older
// build, a duplicated view or a truncated file must fall back to the default
// rather than leave the editor without a timeline.

import type { GroupviewPanelState, Orientation, SerializedDockview, SerializedGridObject } from 'dockview';

export const PANEL_IDS = ['media', 'transcript', 'preview', 'timeline', 'inspector', 'agent'] as const;
export type PanelId = (typeof PANEL_IDS)[number];

export interface PanelSpec {
	title: string;
	minimumWidth?: number;
	minimumHeight?: number;
}

export const PANELS: Record<PanelId, PanelSpec> = {
	media: { title: 'Media', minimumWidth: 200 },
	transcript: { title: 'Transcript', minimumWidth: 200 },
	preview: { title: 'Preview', minimumWidth: 240, minimumHeight: 160 },
	timeline: { title: 'Timeline', minimumHeight: 140 },
	inspector: { title: 'Inspector', minimumWidth: 250 },
	agent: { title: 'Agent', minimumWidth: 260 }
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

type LeafData = { id: string; views: string[]; activeView?: string };
type Node = SerializedGridObject<LeafData>;

function leaf(id: string, views: PanelId[], size: number): Node {
	return { type: 'leaf', data: { id, views, activeView: views[0] }, size };
}

/** Today's arrangement: bin | preview over timeline | inspector | agent, the
 *  transcript tabbed behind the bin. Sizes are ratios — dockview rescales them
 *  to the real window. */
export const DEFAULT_LAYOUT: SerializedDockview = {
	grid: {
		root: {
			type: 'branch',
			size: 796,
			data: [
				leaf('left', ['media', 'transcript'], 248),
				{
					type: 'branch',
					size: 900,
					data: [leaf('preview', ['preview'], 500), leaf('timeline', ['timeline'], 296)]
				},
				leaf('inspector', ['inspector'], 268),
				leaf('agent', ['agent'], 340)
			]
		},
		width: 1656,
		height: 796,
		orientation: 'HORIZONTAL' as Orientation
	},
	panels: Object.fromEntries(PANEL_IDS.map((id) => [id, panelState(id)])),
	activeGroup: 'preview'
};

function isObj(v: unknown): v is Record<string, unknown> {
	return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function size(v: unknown): number | undefined {
	return typeof v === 'number' && Number.isFinite(v) && v >= 0 ? v : undefined;
}

function walk(node: unknown, views: Set<string>, groups: Set<string>): Node | null {
	if (!isObj(node)) return null;
	const s = size(node.size);
	const hidden = node.visible === false;
	if (node.type === 'leaf') {
		const d = node.data;
		if (!isObj(d) || typeof d.id !== 'string' || !Array.isArray(d.views) || d.views.length === 0) return null;
		if (groups.has(d.id)) return null;
		groups.add(d.id);
		for (const v of d.views) {
			if (typeof v !== 'string' || views.has(v)) return null;
			views.add(v);
		}
		const list = d.views as string[];
		const activeView = typeof d.activeView === 'string' && list.includes(d.activeView) ? d.activeView : list[0];
		const out: Node = { type: 'leaf', data: { id: d.id, views: [...list], activeView } };
		if (s !== undefined) out.size = s;
		if (hidden) out.visible = false;
		return out;
	}
	if (node.type === 'branch') {
		if (!Array.isArray(node.data) || node.data.length === 0) return null;
		const children: Node[] = [];
		for (const c of node.data) {
			const w = walk(c, views, groups);
			if (!w) return null;
			children.push(w);
		}
		const out: Node = { type: 'branch', data: children };
		if (s !== undefined) out.size = s;
		if (hidden) out.visible = false;
		return out;
	}
	return null;
}

/** A stored layout, or `null` when it cannot be trusted. Titles and minimum
 *  sizes are always taken from `PANELS`, so a rename or a retuned minimum
 *  reaches layouts saved before it. Floating and popout groups are dropped:
 *  the workspace does not enable them. */
export function sanitizeLayout(raw: unknown): SerializedDockview | null {
	if (!isObj(raw) || !isObj(raw.grid) || !isObj(raw.panels)) return null;
	const grid = raw.grid;
	const orientation = grid.orientation;
	if (orientation !== 'HORIZONTAL' && orientation !== 'VERTICAL') return null;
	const width = size(grid.width);
	const height = size(grid.height);
	if (!width || !height) return null;
	const views = new Set<string>();
	const groups = new Set<string>();
	const root = walk(grid.root, views, groups);
	if (!root || views.size === 0) return null;
	const panels: Record<string, GroupviewPanelState> = {};
	for (const id of views) {
		if (!isPanelId(id)) return null;
		const p = raw.panels[id];
		if (!isObj(p) || p.contentComponent !== id) return null;
		panels[id] = panelState(id);
	}
	const layout: SerializedDockview = {
		grid: { root, width, height, orientation: orientation as Orientation },
		panels
	};
	if (typeof raw.activeGroup === 'string' && groups.has(raw.activeGroup)) layout.activeGroup = raw.activeGroup;
	return layout;
}

/** The panels a layout shows. */
export function openPanelIds(layout: SerializedDockview): PanelId[] {
	return Object.keys(layout.panels).filter(isPanelId);
}
