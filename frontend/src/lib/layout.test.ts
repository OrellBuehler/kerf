import { describe, expect, test } from 'bun:test';
import {
	DEFAULT_LAYOUT,
	LIBRARY_RAIL_WIDTH,
	MAX_POPOUTS,
	PANELS,
	PANEL_IDS,
	PRESET_LAYOUTS,
	EARLIER_PRESETS,
	UNSTAMPED_OFFERED,
	WORKSPACE_IDS,
	insertPanel,
	openPanelIds,
	panelState,
	popoutViews,
	presetLayout,
	presetPanelIds,
	sameArrangement,
	sanitizeLayout
} from './layout';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const clone = (v: unknown): any => JSON.parse(JSON.stringify(v));
const group = (raw: any, id: string): any => {
	const visit = (node: any): any => node.type === 'leaf'
		? (node.data.id === id ? node.data : undefined)
		: node.data.map(visit).find(Boolean);
	return visit(raw.grid.root);
};
/** Every group id in document order. */
const groupIds = (raw: any): string[] => {
	const visit = (node: any): string[] => node.type === 'leaf' ? [node.data.id] : node.data.flatMap(visit);
	return visit(raw.grid.root);
};
const viewsOf = (raw: any): string[] => groupIds(raw).flatMap((id) => group(raw, id).views);

describe('presets', () => {
	test('there is one for every workspace, and Edit is the default', () => {
		expect(Object.keys(PRESET_LAYOUTS).sort()).toEqual([...WORKSPACE_IDS].sort());
		expect(DEFAULT_LAYOUT).toBe(PRESET_LAYOUTS.edit);
	});

	test.each([...WORKSPACE_IDS])('%s survives sanitizing unchanged', (id) => {
		expect(sanitizeLayout(clone(PRESET_LAYOUTS[id]))).toEqual(PRESET_LAYOUTS[id]);
	});

	test.each([...WORKSPACE_IDS])('%s shows each panel once, from the registry, with a timeline', (id) => {
		const layout = PRESET_LAYOUTS[id];
		const views = viewsOf(layout);
		expect(new Set(views).size).toBe(views.length);
		expect(views).toContain('timeline');
		expect(views).toContain('preview');
		for (const v of views) expect(layout.panels[v]).toEqual(panelState(v as never));
		expect(Object.keys(layout.panels).sort()).toEqual([...views].sort());
		// A fresh group id each, so dockview never has to rename one.
		expect(new Set(groupIds(layout)).size).toBe(groupIds(layout).length);
	});

	test.each([...WORKSPACE_IDS])('%s runs the timeline the full width under the other panels', (id) => {
		const root = PRESET_LAYOUTS[id].grid.root as any;
		expect(root.data).toHaveLength(2);
		expect(root.data[1].type).toBe('leaf');
		expect(root.data[1].data.views).toEqual(['timeline']);
		expect(root.data[0].type).toBe('branch');
	});

	test('every panel but the deliver and mixer ones is open in Edit, the agent beside the inspector', () => {
		expect(openPanelIds(PRESET_LAYOUTS.edit).sort()).toEqual(['agent', 'inspector', 'library', 'preview', 'timeline']);
		expect(group(PRESET_LAYOUTS.edit, 'inspector').views).toEqual(['inspector', 'agent']);
	});

	test('Deliver docks the deliver panel beside the preview', () => {
		expect(openPanelIds(PRESET_LAYOUTS.deliver).sort()).toEqual(['agent', 'deliver', 'preview', 'timeline']);
		expect(group(PRESET_LAYOUTS.deliver, 'deliver').views).toEqual(['deliver', 'agent']);
	});

	test('Audio docks the mixer between the preview and the inspector, and no other workspace opens it', () => {
		expect(openPanelIds(PRESET_LAYOUTS.audio).sort()).toEqual(['agent', 'inspector', 'library', 'mixer', 'preview', 'timeline']);
		expect(group(PRESET_LAYOUTS.audio, 'mixer').views).toEqual(['mixer']);
		const row = (PRESET_LAYOUTS.audio.grid.root as any).data[0].data.map((n: any) => n.data.views[0]);
		expect(row).toEqual(['library', 'preview', 'mixer', 'inspector']);
		for (const id of WORKSPACE_IDS.filter((w) => w !== 'audio')) expect(viewsOf(PRESET_LAYOUTS[id])).not.toContain('mixer');
	});

	test('the agent is one tab away in every workspace', () => {
		for (const id of WORKSPACE_IDS) expect(viewsOf(PRESET_LAYOUTS[id])).toContain('agent');
	});

	test('a preset copy is independent of the preset', () => {
		const copy = presetLayout('color');
		(copy.grid.root as any).data = [];
		expect(PRESET_LAYOUTS.color.grid.root.data).toHaveLength(2);
	});
});

describe('the registry', () => {
	test('every panel has a title, and the library may shrink to its rail', () => {
		for (const id of PANEL_IDS) expect(PANELS[id].title.length).toBeGreaterThan(0);
		expect(PANELS.library.minimumWidth).toBe(LIBRARY_RAIL_WIDTH);
		expect(PANELS.deliver.minimumWidth).toBeGreaterThan(0);
	});

	test('the mixer has a title and a minimum a strip and the master fit in', () => {
		expect(PANELS.mixer.title).toBe('Mixer');
		expect(PANELS.mixer.minimumWidth).toBeGreaterThanOrEqual(340);
		expect(PANELS.mixer.minimumHeight).toBeGreaterThanOrEqual(200);
		expect(PANELS.mixer.defaultWidth).toBeGreaterThan(PANELS.mixer.minimumWidth!);
	});

	test('the retired media and transcript panels are gone', () => {
		expect((PANEL_IDS as readonly string[]).includes('media')).toBe(false);
		expect((PANEL_IDS as readonly string[]).includes('transcript')).toBe(false);
	});
});

describe('sanitizeLayout', () => {
	test('rejects anything that is not a layout', () => {
		for (const raw of [null, undefined, 'x', 42, [], {}, { grid: {}, panels: {} }]) {
			expect(sanitizeLayout(raw)).toBeNull();
		}
	});

	test('rejects a panel id it does not know', () => {
		const raw = clone(DEFAULT_LAYOUT);
		group(raw, 'inspector').views = ['effects'];
		raw.panels.effects = { id: 'effects', contentComponent: 'effects' };
		expect(sanitizeLayout(raw)).toBeNull();
	});

	test('rejects a view with no panel entry, or one bound to another component', () => {
		const missing = clone(DEFAULT_LAYOUT);
		delete missing.panels.agent;
		expect(sanitizeLayout(missing)).toBeNull();
		const wrong = clone(DEFAULT_LAYOUT);
		wrong.panels.agent.contentComponent = 'preview';
		expect(sanitizeLayout(wrong)).toBeNull();
	});

	test('rejects the same panel shown twice', () => {
		const raw = clone(DEFAULT_LAYOUT);
		group(raw, 'inspector').views = ['agent', 'library'];
		expect(sanitizeLayout(raw)).toBeNull();
	});

	test('rejects two groups with one id, and a group with no views', () => {
		const dup = clone(DEFAULT_LAYOUT);
		group(dup, 'inspector').id = 'preview';
		expect(sanitizeLayout(dup)).toBeNull();
		const empty = clone(DEFAULT_LAYOUT);
		group(empty, 'inspector').views = [];
		expect(sanitizeLayout(empty)).toBeNull();
	});

	test('rejects a node that is neither a group nor a branch', () => {
		const raw = clone(DEFAULT_LAYOUT);
		raw.grid.root.data[1] = { type: 'panel' };
		expect(sanitizeLayout(raw)).toBeNull();
	});

	test('accepts a subset of panels — closing one is a valid layout', () => {
		const raw = clone(DEFAULT_LAYOUT);
		group(raw, 'inspector').views = ['inspector'];
		delete raw.panels.agent;
		const out = sanitizeLayout(raw);
		expect(out).not.toBeNull();
		expect(openPanelIds(out!).sort()).toEqual(['inspector', 'library', 'preview', 'timeline']);
	});

	test('drops panel entries no group shows', () => {
		const raw = clone(DEFAULT_LAYOUT);
		group(raw, 'inspector').views = ['inspector'];
		const out = sanitizeLayout(raw)!;
		expect(out.panels.agent).toBeUndefined();
	});

	test('restores titles and minimum sizes from the registry', () => {
		const raw = clone(DEFAULT_LAYOUT);
		raw.panels.inspector.title = 'Old name';
		raw.panels.inspector.minimumWidth = 10;
		raw.panels.library.minimumWidth = 300;
		const out = sanitizeLayout(raw)!;
		expect(out.panels.inspector).toEqual(DEFAULT_LAYOUT.panels.inspector);
		expect(out.panels.library.minimumWidth).toBe(LIBRARY_RAIL_WIDTH);
	});

	test('drops floating groups, a stale active group and stray group options', () => {
		const raw = clone(DEFAULT_LAYOUT);
		raw.floatingGroups = [{ data: {}, position: {} }];
		raw.activeGroup = 'gone';
		group(raw, 'library').locked = true;
		group(raw, 'library').hideHeader = true;
		const out = sanitizeLayout(raw)!;
		expect(out.floatingGroups).toBeUndefined();
		expect(out.activeGroup).toBeUndefined();
		expect(out.grid.root).toEqual(DEFAULT_LAYOUT.grid.root);
	});

	test('falls back to the first view when the active one is not in the group', () => {
		const raw = clone(DEFAULT_LAYOUT);
		group(raw, 'inspector').activeView = 'preview';
		const out = sanitizeLayout(raw)!;
		expect(group(out, 'inspector').activeView).toBe('inspector');
	});

	test('keeps a hidden group hidden and a negative or missing size out', () => {
		const raw = clone(DEFAULT_LAYOUT);
		raw.grid.root.data[0].data[0].visible = false;
		raw.grid.root.data[0].data[1].size = -5;
		delete raw.grid.root.data[1].size;
		const out = sanitizeLayout(raw) as any;
		expect(out.grid.root.data[0].data[0].visible).toBe(false);
		expect(out.grid.root.data[0].data[1].size).toBeUndefined();
		expect(out.grid.root.data[1].size).toBeUndefined();
	});
});

// The arrangement saved before the library: media and transcript were two
// panels of their own, tabbed together by default.
const LEGACY_PANELS = (ids: string[]) =>
	Object.fromEntries(ids.map((id) => [id, { id, contentComponent: id, title: id }]));
const legacyDefault = () => ({
	grid: {
		root: {
			type: 'branch',
			size: 1440,
			data: [
				{
					type: 'branch',
					size: 490,
					data: [
						{ type: 'leaf', size: 240, data: { id: 'left', views: ['media', 'transcript'], activeView: 'media' } },
						{ type: 'leaf', size: 860, data: { id: 'preview', views: ['preview'], activeView: 'preview' } },
						{ type: 'leaf', size: 340, data: { id: 'inspector', views: ['inspector', 'agent'], activeView: 'inspector' } }
					]
				},
				{ type: 'leaf', size: 304, data: { id: 'timeline', views: ['timeline'], activeView: 'timeline' } }
			]
		},
		width: 1440,
		height: 794,
		orientation: 'VERTICAL'
	},
	panels: LEGACY_PANELS(['media', 'transcript', 'preview', 'timeline', 'inspector', 'agent']),
	activeGroup: 'preview'
});

describe('migrating a layout saved with the media bin and the transcript', () => {
	test('the tabbed pair becomes one library in the same group', () => {
		const out = sanitizeLayout(legacyDefault()) as any;
		expect(out).not.toBeNull();
		expect(group(out, 'left').views).toEqual(['library']);
		expect(group(out, 'left').activeView).toBe('library');
		expect(out.panels.library).toEqual(panelState('library'));
		expect(out.panels.media).toBeUndefined();
		expect(out.panels.transcript).toBeUndefined();
		expect(openPanelIds(out).sort()).toEqual(['agent', 'inspector', 'library', 'preview', 'timeline']);
		// Sizes are untouched.
		expect(out.grid.root.data[0].data[0].size).toBe(240);
		expect(out.activeGroup).toBe('preview');
	});

	test('the old default migrates to what the Edit preset is, down to the group sizes it had', () => {
		const out = sanitizeLayout(legacyDefault()) as any;
		expect(viewsOf(out).sort()).toEqual(viewsOf(PRESET_LAYOUTS.edit).sort());
	});

	test('`bin` is read like `media`', () => {
		const raw = legacyDefault() as any;
		group(raw, 'left').views = ['bin'];
		group(raw, 'left').activeView = 'bin';
		delete raw.panels.media;
		delete raw.panels.transcript;
		raw.panels.bin = { id: 'bin', contentComponent: 'bin' };
		const out = sanitizeLayout(raw) as any;
		expect(group(out, 'left').views).toEqual(['library']);
		expect(out.panels.library).toEqual(panelState('library'));
	});

	test('the transcript alone becomes the library too', () => {
		const raw = legacyDefault() as any;
		group(raw, 'left').views = ['transcript'];
		group(raw, 'left').activeView = 'transcript';
		delete raw.panels.media;
		const out = sanitizeLayout(raw) as any;
		expect(group(out, 'left').views).toEqual(['library']);
	});

	test('the active tab survives the merge, whichever of the two it was', () => {
		const raw = legacyDefault() as any;
		group(raw, 'left').activeView = 'transcript';
		const out = sanitizeLayout(raw) as any;
		expect(group(out, 'left').activeView).toBe('library');
	});

	test('when they sit in groups of their own the first in layout order is the library and the other group goes', () => {
		const raw = legacyDefault() as any;
		const top = raw.grid.root.data[0].data;
		top[0].data.views = ['media'];
		// The transcript on its own, to the right of the preview.
		top.splice(2, 0, { type: 'leaf', size: 200, data: { id: 'tx', views: ['transcript'], activeView: 'transcript' } });
		const out = sanitizeLayout(raw) as any;
		expect(out).not.toBeNull();
		expect(groupIds(out)).toEqual(['left', 'preview', 'inspector', 'timeline']);
		expect(viewsOf(out)).toEqual(['library', 'preview', 'inspector', 'agent', 'timeline']);
		expect(out.grid.root.data[0].data).toHaveLength(3);
	});

	test('transcript first in layout order wins the library', () => {
		const raw = legacyDefault() as any;
		const top = raw.grid.root.data[0].data;
		top[0].data = { id: 'tx', views: ['transcript'], activeView: 'transcript' };
		top.push({ type: 'leaf', size: 100, data: { id: 'bin', views: ['media'], activeView: 'media' } });
		const out = sanitizeLayout(raw) as any;
		expect(groupIds(out)).toEqual(['tx', 'preview', 'inspector', 'timeline']);
		expect(group(out, 'tx').views).toEqual(['library']);
	});

	test('a library already in the layout wins; the old tabs just drop out', () => {
		const raw = legacyDefault() as any;
		group(raw, 'left').views = ['media', 'transcript', 'library'];
		raw.panels.library = { id: 'library', contentComponent: 'library' };
		const out = sanitizeLayout(raw) as any;
		expect(group(out, 'left').views).toEqual(['library']);
	});

	test('a branch left with one group becomes that group, taking the size the branch had', () => {
		const raw = legacyDefault() as any;
		raw.grid.root.data[0].data = [
			{ type: 'leaf', size: 240, data: { id: 'left', views: ['media'], activeView: 'media' } },
			{
				type: 'branch',
				size: 700,
				data: [{ type: 'leaf', size: 400, data: { id: 'tx', views: ['transcript'], activeView: 'transcript' } }]
			}
		];
		delete raw.panels.preview;
		delete raw.panels.inspector;
		delete raw.panels.agent;
		const out = sanitizeLayout(raw) as any;
		// Library + timeline; the emptied branch is gone, not left as a stub.
		expect(groupIds(out)).toEqual(['left', 'timeline']);
		expect(out.grid.root.data.map((n: any) => n.type)).toEqual(['leaf', 'leaf']);
		// The row was 490 tall (along the root's axis); its one group is that tall now.
		expect(out.grid.root.data[0].size).toBe(490);
	});

	test('a branch whose only child is another branch hands over its grandchildren', () => {
		const raw = legacyDefault() as any;
		// root(V) → [ row(H) → [ stack(V) → [preview, tx(transcript)] , library ], timeline ]
		raw.grid.root.data[0].data = [
			{
				type: 'branch',
				size: 600,
				data: [
					{ type: 'leaf', size: 300, data: { id: 'preview', views: ['preview'], activeView: 'preview' } },
					{ type: 'leaf', size: 200, data: { id: 'tx', views: ['transcript'], activeView: 'transcript' } }
				]
			},
			{ type: 'leaf', size: 240, data: { id: 'left', views: ['media'], activeView: 'media' } }
		];
		// transcript comes first in layout order, so it is the library and 'left' goes.
		delete raw.panels.inspector;
		delete raw.panels.agent;
		const out = sanitizeLayout(raw) as any;
		expect(groupIds(out)).toEqual(['preview', 'tx', 'timeline']);
		expect(group(out, 'tx').views).toEqual(['library']);
		// The row had one branch left: its stack's members now stack beside the timeline.
		expect(out.grid.root.data.map((n: any) => n.type)).toEqual(['leaf', 'leaf', 'leaf']);
		expect(out.grid.orientation).toBe('VERTICAL');
	});

	test('a root left with a single branch is that branch, one axis over', () => {
		const raw = legacyDefault() as any;
		raw.grid.root.data = [raw.grid.root.data[0]];
		delete raw.panels.timeline;
		const out = sanitizeLayout(raw) as any;
		expect(out.grid.orientation).toBe('HORIZONTAL');
		expect(out.grid.root.data.map((n: any) => n.data.id)).toEqual(['left', 'preview', 'inspector']);
	});

	test('a layout of one old tab is a library; one with no views at all is not a layout', () => {
		const raw = legacyDefault() as any;
		raw.grid.root = { type: 'branch', data: [{ type: 'leaf', data: { id: 'g', views: ['media'] } }], size: 1 };
		const out = sanitizeLayout(raw) as any;
		expect(out).not.toBeNull();
		expect(viewsOf(out)).toEqual(['library']);
		raw.grid.root.data[0].data.views = [];
		expect(sanitizeLayout(raw)).toBeNull();
	});

	test('the old panels are still held to the rules: no entry, a wrong entry or a repeat is a bad layout', () => {
		const missing = legacyDefault() as any;
		delete missing.panels.media;
		expect(sanitizeLayout(missing)).toBeNull();
		const wrong = legacyDefault() as any;
		wrong.panels.media.contentComponent = 'preview';
		expect(sanitizeLayout(wrong)).toBeNull();
		const twice = legacyDefault() as any;
		twice.grid.root.data[0].data[1].data.views = ['preview', 'media'];
		expect(sanitizeLayout(twice)).toBeNull();
	});

	test('a migrated layout is stable: sanitizing it again changes nothing', () => {
		const once = sanitizeLayout(legacyDefault())!;
		expect(sanitizeLayout(clone(once))).toEqual(once);
	});
});

describe('sameArrangement', () => {
	test('a layout is the same arrangement as itself and as a copy', () => {
		for (const id of WORKSPACE_IDS) expect(sameArrangement(PRESET_LAYOUTS[id], clone(PRESET_LAYOUTS[id]))).toBe(true);
	});

	test('different workspaces are different arrangements', () => {
		expect(sameArrangement(PRESET_LAYOUTS.edit, PRESET_LAYOUTS.deliver)).toBe(false);
		// Same panels, different split.
		expect(sameArrangement(PRESET_LAYOUTS.edit, PRESET_LAYOUTS.color)).toBe(false);
	});

	test('window size does not matter: every pixel size scaled is the same arrangement', () => {
		const small = clone(PRESET_LAYOUTS.edit);
		const scale = (n: any) => {
			n.size = Math.round(n.size * 0.5);
			if (n.type === 'branch') n.data.forEach(scale);
		};
		scale(small.grid.root);
		small.grid.width = 720;
		small.grid.height = 397;
		expect(sameArrangement(small, PRESET_LAYOUTS.edit)).toBe(true);
	});

	test('a deliberate nudge — a dozen pixels on a wide window — is a different arrangement', () => {
		const nudged = clone(PRESET_LAYOUTS.edit);
		nudged.grid.root.data[0].data[0].size += 12;
		nudged.grid.root.data[0].data[1].size -= 12;
		expect(sameArrangement(nudged, PRESET_LAYOUTS.edit)).toBe(false);
	});

	test('a sash moved by more than the tolerance is a different arrangement', () => {
		const moved = clone(PRESET_LAYOUTS.edit);
		moved.grid.root.data[0].data[0].size += 60; // 60 of 1440 px
		moved.grid.root.data[0].data[1].size -= 60;
		expect(sameArrangement(moved, PRESET_LAYOUTS.edit)).toBe(false);
		// A pixel or two is rounding, not a drag.
		const nudged = clone(PRESET_LAYOUTS.edit);
		nudged.grid.root.data[0].data[0].size += 2;
		nudged.grid.root.data[0].data[1].size -= 2;
		expect(sameArrangement(nudged, PRESET_LAYOUTS.edit)).toBe(true);
	});

	test('the tolerance is a share of the branch, so the vertical split counts too', () => {
		const moved = clone(PRESET_LAYOUTS.edit);
		moved.grid.root.data[0].size += 100;
		moved.grid.root.data[1].size -= 100;
		expect(sameArrangement(moved, PRESET_LAYOUTS.edit)).toBe(false);
	});

	test('which panels, in which groups and which order, is part of it', () => {
		const closed = clone(PRESET_LAYOUTS.edit);
		group(closed, 'inspector').views = ['inspector'];
		expect(sameArrangement(closed, PRESET_LAYOUTS.edit)).toBe(false);
		const reordered = clone(PRESET_LAYOUTS.edit);
		group(reordered, 'inspector').views = ['agent', 'inspector'];
		expect(sameArrangement(reordered, PRESET_LAYOUTS.edit)).toBe(false);
		const renamed = clone(PRESET_LAYOUTS.edit);
		group(renamed, 'inspector').id = 'right';
		expect(sameArrangement(renamed, PRESET_LAYOUTS.edit)).toBe(false);
	});

	test('the active group and the active tab are not part of it', () => {
		const clicked = clone(PRESET_LAYOUTS.edit);
		clicked.activeGroup = 'timeline';
		group(clicked, 'inspector').activeView = 'agent';
		expect(sameArrangement(clicked, PRESET_LAYOUTS.edit)).toBe(true);
	});

	test('the orientation, a hidden group and a different shape are not the same', () => {
		const flipped = clone(PRESET_LAYOUTS.edit);
		flipped.grid.orientation = 'HORIZONTAL';
		expect(sameArrangement(flipped, PRESET_LAYOUTS.edit)).toBe(false);
		const hidden = clone(PRESET_LAYOUTS.edit);
		hidden.grid.root.data[0].data[0].visible = false;
		expect(sameArrangement(hidden, PRESET_LAYOUTS.edit)).toBe(false);
		const leafForBranch = clone(PRESET_LAYOUTS.edit);
		leafForBranch.grid.root.data[0] = leafForBranch.grid.root.data[1];
		expect(sameArrangement(leafForBranch, PRESET_LAYOUTS.edit)).toBe(false);
	});

	test('sizes that are missing fall back to equal shares rather than throwing', () => {
		const bare = clone(PRESET_LAYOUTS.edit);
		const strip = (n: any) => {
			delete n.size;
			if (n.type === 'branch') n.data.forEach(strip);
		};
		strip(bare.grid.root);
		expect(sameArrangement(bare, bare)).toBe(true);
		expect(sameArrangement(bare, PRESET_LAYOUTS.edit)).toBe(false);
	});
});

// What dockview itself serializes for a pristine workspace at 1440×754 (captured
// from `api.toJSON()`), as opposed to what `PRESET_LAYOUTS` says: the same
// shape at the window's pixel sizes, the key order dockview writes, and group
// data in a different order.
const REAL = {"edit":{"grid":{"root":{"type":"branch","data":[{"type":"branch","data":[{"type":"leaf","data":{"views":["library"],"activeView":"library","id":"library"},"size":300},{"type":"leaf","data":{"views":["preview"],"activeView":"preview","id":"preview"},"size":800},{"type":"leaf","data":{"views":["inspector","agent"],"activeView":"inspector","id":"inspector"},"size":340}],"size":465},{"type":"leaf","data":{"views":["timeline"],"activeView":"timeline","id":"timeline"},"size":289}],"size":1440},"width":1440,"height":754,"orientation":"VERTICAL"}},"deliver":{"grid":{"root":{"type":"branch","data":[{"type":"branch","data":[{"type":"leaf","data":{"views":["preview"],"activeView":"preview","id":"preview"},"size":880},{"type":"leaf","data":{"views":["deliver","agent"],"activeView":"deliver","id":"deliver"},"size":560}],"size":494},{"type":"leaf","data":{"views":["timeline"],"activeView":"timeline","id":"timeline"},"size":260}],"size":1440},"width":1440,"height":754,"orientation":"VERTICAL"}}};

describe('sameArrangement against what the dock really reports', () => {
	test('a pristine workspace is its preset, whatever the pixels', () => {
		expect(sameArrangement(REAL.edit as never, PRESET_LAYOUTS.edit)).toBe(true);
		expect(sameArrangement(REAL.deliver as never, PRESET_LAYOUTS.deliver)).toBe(true);
	});

	test('and is not another workspace’s', () => {
		expect(sameArrangement(REAL.edit as never, PRESET_LAYOUTS.deliver)).toBe(false);
		expect(sameArrangement(REAL.deliver as never, PRESET_LAYOUTS.edit)).toBe(false);
	});

	test('a sash dragged 150 px in the real layout is no longer the preset', () => {
		const dragged = clone(REAL.edit);
		dragged.grid.root.data[0].data[0].size += 150;
		dragged.grid.root.data[0].data[1].size -= 150;
		expect(sameArrangement(dragged, PRESET_LAYOUTS.edit)).toBe(false);
	});
});

// ---- panels a later build adds ----------------------------------------------

/** `layout` with `id` closed, as the dock does it: its view gone, an emptied group
 *  with it, and the room it had shared out among the groups beside it. */
const closed = (layout: any, id: string): any => {
	const l = clone(layout);
	const prune = (nodes: any[]): any[] => {
		const total = nodes.reduce((sum, n) => sum + n.size, 0);
		const kept = nodes.flatMap((n) => {
			if (n.type === 'leaf') {
				n.data.views = n.data.views.filter((v: string) => v !== id);
				if (n.data.views.length === 0) return [];
				if (!n.data.views.includes(n.data.activeView)) n.data.activeView = n.data.views[0];
				return [n];
			}
			n.data = prune(n.data);
			return n.data.length ? [n] : [];
		});
		const left = kept.reduce((sum, n) => sum + n.size, 0);
		if (left > 0 && left < total) for (const n of kept) n.size = (n.size * total) / left;
		return kept;
	};
	l.grid.root.data = prune(l.grid.root.data);
	delete l.panels[id];
	return l;
};
/** The sizes of the children of the branch holding group `id`. */
const rowSizes = (raw: any, id: string): number[] => {
	const visit = (nodes: any[]): number[] | undefined => {
		if (nodes.some((n) => n.type === 'leaf' && n.data.id === id)) return nodes.map((n) => n.size);
		for (const n of nodes) if (n.type === 'branch') { const r = visit(n.data); if (r) return r; }
		return undefined;
	};
	return visit(raw.grid.root.data)!;
};

describe('what a preset offers', () => {
	test('is the panels it opens, and the mixer is Audio’s alone', () => {
		expect(presetPanelIds('audio').sort()).toEqual(['agent', 'inspector', 'library', 'mixer', 'preview', 'timeline']);
		for (const id of WORKSPACE_IDS.filter((w) => w !== 'audio')) expect(presetPanelIds(id)).not.toContain('mixer');
	});

	test('a layout stored before the record is taken to have been offered every panel but the mixer', () => {
		for (const id of WORKSPACE_IDS) {
			expect(UNSTAMPED_OFFERED[id]).toEqual(presetPanelIds(id).filter((p) => p !== 'mixer'));
		}
	});

	test('the earlier Audio preset is a layout of its own: no mixer, a tall timeline', () => {
		const earlier = EARLIER_PRESETS.audio!;
		expect(openPanelIds(earlier)).not.toContain('mixer');
		expect(sanitizeLayout(clone(earlier))).toEqual(earlier);
		expect(sameArrangement(earlier, PRESET_LAYOUTS.audio)).toBe(false);
	});
});

describe('insertPanel', () => {
	test('puts a closed panel back exactly where the preset has it', () => {
		for (const id of WORKSPACE_IDS) {
			for (const panel of presetPanelIds(id).filter((p) => p !== 'timeline')) {
				const without = closed(PRESET_LAYOUTS[id], panel);
				const back = insertPanel(without, panel, PRESET_LAYOUTS[id])!;
				expect(back).not.toBeNull();
				expect(sameArrangement(back, PRESET_LAYOUTS[id], 0.003)).toBe(true);
				expect(sanitizeLayout(clone(back))).toEqual(back);
				expect(viewsOf(back).filter((v) => v === panel)).toHaveLength(1);
			}
		}
	});

	test('the mixer goes between the preview and the inspector and takes its share from the row', () => {
		const without = closed(PRESET_LAYOUTS.audio, 'mixer');
		const before = rowSizes(without, 'preview');
		const back = insertPanel(without, 'mixer', PRESET_LAYOUTS.audio)!;
		const row = (back.grid.root as any).data[0].data.map((n: any) => n.data.views[0]);
		expect(row).toEqual(['library', 'preview', 'mixer', 'inspector']);
		const after = rowSizes(back, 'mixer');
		expect(after.reduce((a, b) => a + b, 0)).toBe(before.reduce((a, b) => a + b, 0));
		expect(after[2]).toBeGreaterThanOrEqual(PANELS.mixer.minimumWidth!);
	});

	test('a layout the user arranged keeps its arrangement; the panel takes a proportional share', () => {
		const mine = closed(PRESET_LAYOUTS.audio, 'mixer');
		const row = mine.grid.root.data[0].data;
		row[0].size = 380; // a wide library
		row[1].size = 330;
		const kept = clone(mine);
		const back = insertPanel(mine, 'mixer', PRESET_LAYOUTS.audio)!;
		// The inputs are not touched.
		expect(mine).toEqual(kept);
		expect(viewsOf(back).sort()).toEqual([...viewsOf(mine), 'mixer'].sort());
		const sizes = rowSizes(back, 'mixer');
		// library : preview stays 380 : 330 within rounding.
		expect(sizes[0] / sizes[1]).toBeCloseTo(380 / 330, 1);
	});

	test('a tab mate brings it back to the group it shared', () => {
		const without = closed(PRESET_LAYOUTS.edit, 'agent');
		const back = insertPanel(without, 'agent', PRESET_LAYOUTS.edit)!;
		expect(group(back, 'inspector').views).toEqual(['inspector', 'agent']);
	});

	test('with no tab mate and no neighbour in the layout it becomes a tab beside the preview', () => {
		// Only the preview and the timeline are left.
		let mine = PRESET_LAYOUTS.audio as any;
		for (const p of ['library', 'inspector', 'agent']) mine = closed(mine, p);
		mine = closed(mine, 'mixer');
		const back = insertPanel(mine, 'agent', PRESET_LAYOUTS.audio)!;
		// The nearest neighbour in the preset is the mixer's row; with the mixer gone too,
		// the agent finds the preview next in line.
		expect(viewsOf(back).sort()).toEqual(['agent', 'preview', 'timeline']);
		expect(sanitizeLayout(clone(back))).toEqual(back);
	});

	test('a row that runs the other way is not cut into: the panel is tabbed with the preview instead', () => {
		// The preview stacked over the timeline, nothing beside it.
		let mine = PRESET_LAYOUTS.audio as any;
		for (const p of ['library', 'inspector', 'agent', 'mixer']) mine = closed(mine, p);
		mine = clone(mine);
		mine.grid.root.data = [mine.grid.root.data[0].data[0], mine.grid.root.data[1]];
		const clean = sanitizeLayout(clone(mine))!;
		expect(clean).not.toBeNull();
		expect(viewsOf(clean).sort()).toEqual(['preview', 'timeline']);
		const back = insertPanel(clean, 'mixer', PRESET_LAYOUTS.audio)!;
		expect(group(back, 'preview').views).toEqual(['preview', 'mixer']);
		expect(sanitizeLayout(clone(back))).toEqual(back);
	});

	test('a panel already there is left where it is', () => {
		const back = insertPanel(PRESET_LAYOUTS.audio, 'mixer', PRESET_LAYOUTS.audio)!;
		expect(back).toEqual(PRESET_LAYOUTS.audio);
		expect(back).not.toBe(PRESET_LAYOUTS.audio);
	});

	test('a panel the preset does not open is nobody’s to insert', () => {
		expect(insertPanel(PRESET_LAYOUTS.edit, 'mixer', PRESET_LAYOUTS.edit)).toBeNull();
	});

	test('a new group never reuses a group id that is taken', () => {
		const without = closed(PRESET_LAYOUTS.audio, 'mixer');
		// Someone else's group is called "mixer".
		(without.grid.root.data[1] as any).data.id = 'mixer';
		const back = insertPanel(without, 'mixer', PRESET_LAYOUTS.audio)!;
		const ids = groupIds(back);
		expect(new Set(ids).size).toBe(ids.length);
	});
});

// ---- detached windows --------------------------------------------------------

/** The Edit preset with its inspector group popped out into a window, as dockview
 *  writes it: the group stays in the grid, empty and hidden, holding the place its
 *  panels return to, and the window points back at it. */
function withInspectorDetached(position: unknown = { left: 2200, top: 120, width: 600, height: 700 }): any {
	const raw = clone(DEFAULT_LAYOUT);
	const g = group(raw, 'inspector');
	const views = g.views;
	g.views = [];
	delete g.activeView;
	const visit = (node: any): any => {
		if (node.type === 'leaf') return node.data.id === 'inspector' ? node : undefined;
		return node.data.map(visit).find(Boolean);
	};
	visit(raw.grid.root).visible = false;
	raw.popoutGroups = [
		{ data: { id: 'win-1', views, activeView: views[0] }, gridReferenceGroup: 'inspector', position, url: '/popout.html' }
	];
	return raw;
}

describe('detached windows', () => {
	test('a window is kept, with the group it returns to', () => {
		const out = sanitizeLayout(withInspectorDetached())!;
		expect(out.popoutGroups).toHaveLength(1);
		expect(out.popoutGroups![0].data!.views).toEqual(['inspector', 'agent']);
		expect(out.popoutGroups![0].gridReferenceGroup).toBe('inspector');
		expect(out.popoutGroups![0].position).toEqual({ left: 2200, top: 120, width: 600, height: 700 });
		const hidden = group(out, 'inspector');
		expect(hidden.views).toEqual([]);
		expect([...openPanelIds(out)].sort()).toEqual(Object.keys(PRESET_LAYOUTS.edit.panels).sort() as string[] as never);
	});

	test('what dockview wrote is what comes back, and again', () => {
		const once = sanitizeLayout(withInspectorDetached())!;
		expect(sanitizeLayout(clone(once))).toEqual(once);
	});

	test('the page is always the popout page, whatever was stored', () => {
		const raw = withInspectorDetached();
		raw.popoutGroups[0].url = 'https://example.com/';
		expect(sanitizeLayout(raw)!.popoutGroups![0].url).toBe('/popout.html');
		delete raw.popoutGroups[0].url;
		expect(sanitizeLayout(raw)!.popoutGroups![0].url).toBe('/popout.html');
	});

	test('a window with no readable place opens where the platform puts it', () => {
		for (const position of [null, undefined, 'x', {}, { left: 1, top: 1, width: 0, height: 5 }, { left: NaN, top: 1, width: 5, height: 5 }]) {
			const raw = withInspectorDetached();
			raw.popoutGroups[0].position = position;
			expect(sanitizeLayout(raw)!.popoutGroups![0].position, String(position)).toBeNull();
		}
	});

	test('an empty hidden group nothing points at is dropped, and its row closes up', () => {
		const raw = withInspectorDetached();
		delete raw.popoutGroups;
		const out = sanitizeLayout(raw)!;
		expect(groupIds(out)).not.toContain('inspector');
		expect(out.popoutGroups).toBeUndefined();
		// The same goes for a window that did not read, which cannot hold a place.
		const bad = withInspectorDetached();
		bad.popoutGroups[0].data.views = [];
		expect(groupIds(sanitizeLayout(bad)!)).not.toContain('inspector');
	});

	test('a reference group the grid no longer has is not pointed at', () => {
		const raw = withInspectorDetached();
		raw.popoutGroups[0].gridReferenceGroup = 'gone';
		const out = sanitizeLayout(raw)!;
		expect('gridReferenceGroup' in out.popoutGroups![0]).toBe(false);
	});

	test('a panel the grid shows cannot also be in a window: that window is dropped, the layout is not', () => {
		const raw = withInspectorDetached();
		raw.popoutGroups[0].data.views = ['inspector', 'library'];
		const out = sanitizeLayout(raw);
		expect(out).not.toBeNull();
		expect(out!.popoutGroups).toBeUndefined();
		expect(viewsOf(out!)).toContain('library');
	});

	test('two windows cannot show one panel, nor share a group id', () => {
		const raw = withInspectorDetached();
		raw.popoutGroups.push({ data: { id: 'win-2', views: ['agent'] }, position: null });
		raw.popoutGroups[0].data.views = ['inspector'];
		raw.panels.agent = { id: 'agent', contentComponent: 'agent' };
		const ok = sanitizeLayout(raw)!;
		expect(ok.popoutGroups).toHaveLength(2);
		raw.popoutGroups[1].data.views = ['inspector'];
		expect(sanitizeLayout(raw)!.popoutGroups).toHaveLength(1);
		raw.popoutGroups[1].data = { id: 'win-1', views: ['agent'] };
		expect(sanitizeLayout(raw)!.popoutGroups).toHaveLength(1);
		raw.popoutGroups[1].data = { id: 'preview', views: ['agent'] };
		expect(sanitizeLayout(raw)!.popoutGroups).toHaveLength(1);
	});

	test('a window split into groups is read through the same walk', () => {
		const raw = withInspectorDetached();
		const nested = {
			root: {
				type: 'branch',
				data: [
					{ type: 'leaf', data: { id: 'win-a', views: ['inspector'], activeView: 'inspector' }, size: 300 },
					{ type: 'leaf', data: { id: 'win-b', views: ['agent'], activeView: 'agent' }, size: 300 }
				],
				size: 600
			},
			width: 600,
			height: 700,
			orientation: 'VERTICAL'
		};
		delete raw.popoutGroups[0].data;
		raw.popoutGroups[0].grid = nested;
		const out = sanitizeLayout(raw)!;
		expect(out.popoutGroups![0].data).toBeUndefined();
		expect(popoutViews(out.popoutGroups![0])).toEqual(['inspector', 'agent']);
		expect(sanitizeLayout(clone(out))).toEqual(out);
	});

	test('the editor window keeps a panel of its own', () => {
		const raw = clone(DEFAULT_LAYOUT);
		const all = viewsOf(raw);
		const leafOf = (id: string) => group(raw, id);
		const win: any[] = [];
		for (const gid of groupIds(raw)) {
			const g = leafOf(gid);
			win.push({ data: { id: `w-${gid}`, views: g.views }, position: null });
			g.views = [];
		}
		raw.popoutGroups = win;
		expect(all.length).toBeGreaterThan(0);
		expect(sanitizeLayout(raw)).toBeNull();
	});

	test('there is a limit to how many windows a stored layout may open', () => {
		const raw = withInspectorDetached();
		for (let i = 0; i < 30; i++) raw.popoutGroups.push({ data: { id: `x${i}`, views: ['agent'] }, position: null });
		expect(sanitizeLayout(raw)!.popoutGroups!.length).toBeLessThanOrEqual(MAX_POPOUTS);
	});

	test('a layout with no window has no window list', () => {
		expect(sanitizeLayout(clone(DEFAULT_LAYOUT))!.popoutGroups).toBeUndefined();
		const raw = clone(DEFAULT_LAYOUT);
		raw.popoutGroups = 'nope';
		expect(sanitizeLayout(raw)!.popoutGroups).toBeUndefined();
	});
});

describe('sameArrangement and detached windows', () => {
	const base = () => sanitizeLayout(withInspectorDetached())!;

	test('a window the platform nudged by a title bar is where it was', () => {
		const a = base();
		const b = clone(a);
		b.popoutGroups[0].position.left += 8;
		b.popoutGroups[0].position.top -= 9;
		expect(sameArrangement(a, b)).toBe(true);
	});

	test('a window the user moved or resized is arranged differently', () => {
		const a = base();
		const moved = clone(a);
		moved.popoutGroups[0].position.left += 200;
		expect(sameArrangement(a, moved)).toBe(false);
		const resized = clone(a);
		resized.popoutGroups[0].position.width -= 100;
		expect(sameArrangement(a, resized)).toBe(false);
	});

	test('a window opening or closing, or changing what it holds, is a different arrangement', () => {
		const a = base();
		expect(sameArrangement(a, sanitizeLayout(clone(DEFAULT_LAYOUT))!)).toBe(false);
		const fewer = clone(a);
		fewer.popoutGroups[0].data.views = ['inspector'];
		expect(sameArrangement(a, fewer)).toBe(false);
		const reordered = clone(a);
		reordered.popoutGroups[0].data.views = ['agent', 'inspector'];
		expect(sameArrangement(a, reordered)).toBe(false);
	});

	test('a layout with no window is the same as one with an empty list', () => {
		const a = clone(DEFAULT_LAYOUT);
		const b = clone(DEFAULT_LAYOUT);
		b.popoutGroups = [];
		expect(sameArrangement(a, b)).toBe(true);
	});
});
