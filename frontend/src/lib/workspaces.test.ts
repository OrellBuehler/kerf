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
	parseWorkspaces,
	stepTab,
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

	test('the specialised workspaces open a tab that exists; Edit and Deliver leave it be', () => {
		for (const w of WORKSPACE_SPECS) if (w.libraryTab) expect(isLibraryTab(w.libraryTab)).toBe(true);
		expect(workspaceSpec('audio').libraryTab).toBe('audio');
		expect(workspaceSpec('edit').libraryTab).toBeUndefined();
		expect(workspaceSpec('deliver').libraryTab).toBeUndefined();
	});

	test('stepTab walks the rail and wraps at both ends', () => {
		expect(stepTab('media', 1)).toBe('titles');
		expect(stepTab('titles', -1)).toBe('media');
		expect(stepTab('transcript', 1)).toBe('media');
		expect(stepTab('media', -1)).toBe('transcript');
	});
});

describe('parseWorkspaces', () => {
	test('nothing stored is Edit, on the media tab, expanded, with no arrangements', () => {
		expect(parseWorkspaces(undefined)).toEqual(defaultWorkspaces());
		expect(parseWorkspaces(null)).toEqual({ active: 'edit', layouts: {}, library: { tab: 'media', collapsed: false } });
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
			library: { tab: 'transitions', collapsed: true }
		};
		const parsed = parseWorkspaces(clone(stored));
		expect(parsed).toEqual(stored as never);
		expect(parseWorkspaces(clone(parsed))).toEqual(parsed);
	});

	test('a partial value fills in the rest', () => {
		expect(parseWorkspaces({ active: 'motion' })).toEqual({ ...defaultWorkspaces(), active: 'motion' });
		expect(parseWorkspaces({ library: { tab: 'effects' } }).library).toEqual({ tab: 'effects', collapsed: false });
		expect(parseWorkspaces({ library: { collapsed: true } }).library).toEqual({ tab: 'media', collapsed: true });
	});

	test('an unknown workspace or tab falls back, field by field', () => {
		const parsed = parseWorkspaces({
			active: 'compositing',
			library: { tab: 'plugins', collapsed: 'yes' }
		});
		expect(parsed.active).toBe('edit');
		expect(parsed.library).toEqual({ tab: 'media', collapsed: false });
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
