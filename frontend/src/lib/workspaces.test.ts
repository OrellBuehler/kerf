import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { PRESET_LAYOUTS, WORKSPACE_IDS, presetLayout, sanitizeLayout } from './layout';
import {
	LIBRARY_TABS,
	LIBRARY_TAB_SPECS,
	WORKSPACE_SPECS,
	defaultWorkspaces,
	isLibraryTab,
	layoutFor,
	libraryTabFor,
	parseWorkspaces,
	shouldPersistLayout,
	stepTab,
	withLibraryTab,
	workspaceSpec
} from './workspaces';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const clone = (v: unknown): any => JSON.parse(JSON.stringify(v));

/** The arrangement saved before there were workspaces (media + transcript). */
const legacyLayout = () => {
	const l = clone(PRESET_LAYOUTS.edit);
	const group = l.grid.root.data[0].data[0].data;
	group.views = ['media', 'transcript'];
	group.activeView = 'media';
	delete l.panels.library;
	l.panels.media = { id: 'media', contentComponent: 'media', title: 'Media' };
	l.panels.transcript = { id: 'transcript', contentComponent: 'transcript', title: 'Transcript' };
	return l;
};

describe('the workspace and tab lists', () => {
	test('a spec for every workspace, in order, with a label and a hint', () => {
		expect(WORKSPACE_SPECS.map((w) => w.id)).toEqual([...WORKSPACE_IDS]);
		for (const w of WORKSPACE_SPECS) {
			expect(w.label.length).toBeGreaterThan(0);
			expect(w.hint.length).toBeGreaterThan(0);
		}
		expect(workspaceSpec('audio').label).toBe('Audio');
	});

	test('a spec for every library tab, in rail order', () => {
		expect(LIBRARY_TAB_SPECS.map((t) => t.id)).toEqual([...LIBRARY_TABS]);
		expect(LIBRARY_TABS).toEqual(['media', 'titles', 'effects', 'transitions', 'audio', 'transcript']);
	});

	test('the tab and workspace icons exist in the icon registry', () => {
		const registry = readFileSync(new URL('./components/editor/icons.ts', import.meta.url), 'utf8');
		for (const t of LIBRARY_TAB_SPECS) expect(registry).toContain(`'${t.icon}':`);
	});

	test('every workspace has a library tab of its own, the tool it is about', () => {
		for (const w of WORKSPACE_SPECS) expect(isLibraryTab(w.libraryTab)).toBe(true);
		expect(WORKSPACE_SPECS.map((w) => [w.id, w.libraryTab])).toEqual([
			['edit', 'media'],
			['color', 'effects'],
			['audio', 'audio'],
			['motion', 'transitions'],
			['deliver', 'media']
		]);
	});

	test('stepTab walks the rail and wraps at both ends', () => {
		expect(stepTab('media', 1)).toBe('titles');
		expect(stepTab('titles', -1)).toBe('media');
		expect(stepTab('transcript', 1)).toBe('media');
		expect(stepTab('media', -1)).toBe('transcript');
	});
});

describe('parseWorkspaces', () => {
	test('nothing stored is Edit, expanded, with no arrangements and no tab picked', () => {
		expect(parseWorkspaces(undefined)).toEqual(defaultWorkspaces());
		expect(parseWorkspaces(null)).toEqual({ active: 'edit', layouts: {}, library: { tabs: {}, collapsed: false } });
	});

	test('garbage anywhere is the default', () => {
		for (const raw of ['x', 42, true, [], [1, 2], () => 1]) {
			expect(parseWorkspaces(raw)).toEqual(defaultWorkspaces());
		}
		expect(parseWorkspaces({})).toEqual(defaultWorkspaces());
		expect(parseWorkspaces({ active: 7, layouts: 'x', library: 3 })).toEqual(defaultWorkspaces());
	});

	test('a full value round-trips through JSON', () => {
		const stored = {
			active: 'audio',
			layouts: { edit: clone(PRESET_LAYOUTS.edit), color: clone(PRESET_LAYOUTS.color) },
			library: { tabs: { edit: 'transcript', audio: 'transitions' }, collapsed: true }
		};
		const parsed = parseWorkspaces(clone(stored));
		expect(parsed).toEqual(stored as never);
		expect(parseWorkspaces(clone(parsed))).toEqual(parsed);
	});

	test('a partial value fills in the rest', () => {
		expect(parseWorkspaces({ active: 'motion' })).toEqual({ ...defaultWorkspaces(), active: 'motion' });
		expect(parseWorkspaces({ library: { tabs: { color: 'titles' } } }).library).toEqual({
			tabs: { color: 'titles' },
			collapsed: false
		});
		expect(parseWorkspaces({ library: { collapsed: true } }).library).toEqual({ tabs: {}, collapsed: true });
	});

	test('an unknown workspace or tab falls back, field by field', () => {
		const parsed = parseWorkspaces({
			active: 'compositing',
			library: { tabs: { edit: 'plugins', color: 'effects', compositing: 'media', audio: 7 }, collapsed: 'yes' }
		});
		expect(parsed.active).toBe('edit');
		expect(parsed.library).toEqual({ tabs: { color: 'effects' }, collapsed: false });
		expect(parseWorkspaces({ library: { tabs: 'x' } }).library).toEqual({ tabs: {}, collapsed: false });
	});

	test('the one tab every workspace used to share becomes the active workspace’s own', () => {
		const old = { active: 'color', layouts: {}, library: { tab: 'transcript', collapsed: true } };
		const parsed = parseWorkspaces(old);
		expect(parsed.library).toEqual({ tabs: { color: 'transcript' }, collapsed: true });
		// The others are what they would be had nothing been picked.
		expect(libraryTabFor(parsed, 'color')).toBe('transcript');
		expect(libraryTabFor(parsed, 'edit')).toBe('media');
		expect(libraryTabFor(parsed, 'audio')).toBe('audio');
		// An unusable old tab is no tab; a per-workspace value, once there, wins.
		expect(parseWorkspaces({ library: { tab: 'plugins' } }).library.tabs).toEqual({});
		expect(parseWorkspaces({ active: 'audio', library: { tab: 'titles', tabs: { audio: 'media' } } }).library.tabs).toEqual({
			audio: 'media'
		});
	});

	test('one bad layout costs that workspace its arrangement, not the others', () => {
		const parsed = parseWorkspaces({
			active: 'color',
			layouts: {
				edit: { grid: 'nope' },
				color: clone(PRESET_LAYOUTS.color),
				audio: null,
				motion: clone(PRESET_LAYOUTS.motion),
				deliver: 5,
				compositing: clone(PRESET_LAYOUTS.edit)
			}
		});
		expect(Object.keys(parsed.layouts).sort()).toEqual(['color', 'motion']);
		expect(parsed.layouts.color).toEqual(PRESET_LAYOUTS.color);
	});

	test('a stored layout is sanitized on the way in: registry titles, floating groups dropped', () => {
		const edit = clone(PRESET_LAYOUTS.edit);
		edit.panels.inspector.title = 'Old name';
		edit.floatingGroups = [{ data: {}, position: {} }];
		const parsed = parseWorkspaces({ layouts: { edit } });
		expect(parsed.layouts.edit).toEqual(PRESET_LAYOUTS.edit);
	});

	test('a stored layout from before the library is migrated, not thrown away', () => {
		const parsed = parseWorkspaces({ layouts: { edit: legacyLayout() } });
		expect(parsed.layouts.edit).toEqual(PRESET_LAYOUTS.edit);
	});

	test('a layout saved under the wrong workspace is still a layout (it is the user’s arrangement)', () => {
		const parsed = parseWorkspaces({ layouts: { audio: clone(PRESET_LAYOUTS.deliver) } });
		expect(parsed.layouts.audio).toEqual(PRESET_LAYOUTS.deliver);
	});
});

describe('the layout from before workspaces', () => {
	test('becomes the Edit arrangement when there is no workspaces value', () => {
		const parsed = parseWorkspaces(null, legacyLayout());
		expect(parsed.active).toBe('edit');
		// Migrated: the tabbed pair is the library now.
		expect(parsed.layouts.edit).toEqual(PRESET_LAYOUTS.edit);
		expect(parsed.layouts.color).toBeUndefined();
	});

	test('a current-format old layout (already with a library) is used as is', () => {
		const custom = clone(PRESET_LAYOUTS.edit);
		custom.grid.root.data[0].data[0].size = 410;
		const parsed = parseWorkspaces(undefined, custom);
		expect(parsed.layouts.edit?.grid.root).toEqual(sanitizeLayout(custom)!.grid.root);
		expect((parsed.layouts.edit?.grid.root as any).data[0].data[0].size).toBe(410);
	});

	test('a workspaces value that is not an object counts as none', () => {
		expect(parseWorkspaces('garbage', legacyLayout()).layouts.edit).toEqual(PRESET_LAYOUTS.edit);
		expect(parseWorkspaces([1], legacyLayout()).layouts.edit).toEqual(PRESET_LAYOUTS.edit);
	});

	test('an unusable old layout is just the defaults', () => {
		expect(parseWorkspaces(null, { grid: {} })).toEqual(defaultWorkspaces());
		expect(parseWorkspaces(null, 'x')).toEqual(defaultWorkspaces());
		expect(parseWorkspaces(null, null)).toEqual(defaultWorkspaces());
	});

	test('is ignored once there is a workspaces value — a reset Edit must not come back', () => {
		expect(parseWorkspaces({}, legacyLayout()).layouts.edit).toBeUndefined();
		expect(parseWorkspaces({ active: 'color', layouts: {} }, legacyLayout()).layouts).toEqual({});
	});
});

describe('layoutFor', () => {
	test('is the preset until a workspace has been arranged', () => {
		const state = defaultWorkspaces();
		for (const id of WORKSPACE_IDS) expect(layoutFor(state, id)).toEqual(PRESET_LAYOUTS[id]);
	});

	test('is the arrangement once it has been', () => {
		const state = defaultWorkspaces();
		const custom = clone(PRESET_LAYOUTS.color);
		custom.grid.root.data[1].size = 150;
		state.layouts.color = custom;
		expect(layoutFor(state, 'color')).toEqual(custom);
		expect(layoutFor(state, 'edit')).toEqual(PRESET_LAYOUTS.edit);
	});

	test('falls back to the preset when what is stored no longer sanitizes', () => {
		const state = defaultWorkspaces();
		state.layouts.audio = { grid: {} } as never;
		expect(layoutFor(state, 'audio')).toEqual(presetLayout('audio'));
	});

	test('hands out copies dockview may take over', () => {
		const a = layoutFor(defaultWorkspaces(), 'edit');
		(a.grid.root as any).data = [];
		expect(layoutFor(defaultWorkspaces(), 'edit')).toEqual(PRESET_LAYOUTS.edit);
	});
});

describe('the library tab per workspace', () => {
	test('is the workspace’s own until one is picked there', () => {
		const state = defaultWorkspaces();
		expect(libraryTabFor(state, 'edit')).toBe('media');
		expect(libraryTabFor(state, 'color')).toBe('effects');
		expect(libraryTabFor(state, 'motion')).toBe('transitions');
	});

	test('a pick sticks to its workspace and moves no other', () => {
		let state = defaultWorkspaces();
		state = withLibraryTab(state, 'edit', 'transcript');
		expect(libraryTabFor(state, 'edit')).toBe('transcript');
		expect(libraryTabFor(state, 'color')).toBe('effects');
		expect(libraryTabFor(state, 'audio')).toBe('audio');
		state = withLibraryTab(state, 'color', 'titles');
		expect(libraryTabFor(state, 'edit')).toBe('transcript');
		expect(libraryTabFor(state, 'color')).toBe('titles');
	});

	test('picking does not touch the other fields or mutate the old state', () => {
		const before = defaultWorkspaces();
		const after = withLibraryTab(before, 'audio', 'media');
		expect(before.library.tabs).toEqual({});
		expect(after.active).toBe(before.active);
		expect(after.layouts).toBe(before.layouts);
		expect(after.library.collapsed).toBe(before.library.collapsed);
	});
});

describe('shouldPersistLayout', () => {
	// What the dock reports: sizes in the window's pixels (here 1200 wide, not the
	// presets' 1440), group ids and views as built.
	const scaled = (id: (typeof WORKSPACE_IDS)[number], k = 1200 / 1440) => {
		const l = clone(PRESET_LAYOUTS[id]);
		const scale = (n: any) => {
			if (typeof n.size === 'number') n.size = Math.round(n.size * k);
			if (n.type === 'branch') n.data.forEach(scale);
		};
		scale(l.grid.root);
		l.grid.width = 1200;
		return l;
	};
	const preset = PRESET_LAYOUTS.edit;
	const dragged = () => {
		const l = scaled('edit');
		const row = l.grid.root.data[0].data;
		row[0].size += 150;
		row[1].size -= 150;
		return l;
	};

	test('nothing is written while the layout is still settling', () => {
		expect(shouldPersistLayout(dragged(), null, preset, false)).toBe(false);
	});

	test('the layout as it settled is not a change, whatever the window size', () => {
		const settled = scaled('edit');
		expect(shouldPersistLayout(scaled('edit'), settled, preset, false)).toBe(false);
		// A resize rescales every pixel size, and rounding moves each by a pixel or two.
		const resized = scaled('edit', 1000 / 1440);
		resized.grid.root.data[0].data[1].size += 1;
		expect(shouldPersistLayout(resized, settled, preset, false)).toBe(false);
	});

	test('a click that only changes the active group or tab is not a change', () => {
		const settled = scaled('edit');
		const clicked = scaled('edit');
		clicked.activeGroup = 'timeline';
		clicked.grid.root.data[0].data[2].data.activeView = 'agent';
		expect(shouldPersistLayout(clicked, settled, preset, false)).toBe(false);
	});

	test('a sash dragged is a change, and is written', () => {
		expect(shouldPersistLayout(dragged(), scaled('edit'), preset, false)).toBe(true);
		expect(shouldPersistLayout(dragged(), scaled('edit'), preset, true)).toBe(true);
	});

	test('a panel closed, or moved to another group, is a change', () => {
		const settled = scaled('edit');
		const closed = scaled('edit');
		closed.grid.root.data[0].data[2].data.views = ['inspector'];
		delete closed.panels.agent;
		expect(shouldPersistLayout(closed, settled, preset, false)).toBe(true);
		const moved = scaled('edit');
		const [first, ...rest] = moved.grid.root.data[0].data;
		moved.grid.root.data[0].data = [...rest, first];
		expect(shouldPersistLayout(moved, settled, preset, false)).toBe(true);
	});

	test('after a write, what was written is the reference: unchanged again is not written twice', () => {
		const written = dragged();
		expect(shouldPersistLayout(dragged(), written, preset, true)).toBe(false);
	});

	test('with no entry yet, a layout that merely equals the preset needs none', () => {
		// The settled layout drifted from the preset (the library folded) and then
		// came back to it: still nothing worth keeping.
		const settled = scaled('edit');
		settled.grid.root.data[0].data[0].size = 40;
		expect(shouldPersistLayout(scaled('edit'), settled, preset, false)).toBe(false);
		// With an entry, the same layout is a real change from it and is written.
		expect(shouldPersistLayout(scaled('edit'), settled, preset, true)).toBe(true);
	});

	test('a different panel set is never the preset', () => {
		const settled = scaled('deliver');
		expect(shouldPersistLayout(scaled('edit'), settled, PRESET_LAYOUTS.deliver, false)).toBe(true);
	});
});
