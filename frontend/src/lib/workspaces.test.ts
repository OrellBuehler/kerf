import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import {
	EARLIER_PRESETS,
	PRESET_LAYOUTS,
	WORKSPACE_IDS,
	openPanelIds,
	presetLayout,
	presetPanelIds,
	sameArrangement,
	sanitizeLayout
} from './layout';
import {
	LIBRARY_TABS,
	LIBRARY_TAB_SPECS,
	WORKSPACE_SPECS,
	adoptPanels,
	defaultWorkspaces,
	describeAdopted,
	isLibraryTab,
	layoutFor,
	libraryTabFor,
	parseWorkspaces,
	readWorkspaces,
	shouldPersistLayout,
	stepTab,
	withLayout,
	withLibraryTab,
	withoutWorkspaces,
	workspaceSpec
} from './workspaces';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const clone = (v: unknown): any => JSON.parse(JSON.stringify(v));

/** A preset the user has dragged a sash of: an arrangement of their own, which no
 *  migration may take for a copy of the preset. */
const nudged = (id: (typeof WORKSPACE_IDS)[number], by = 60) => {
	const l = clone(PRESET_LAYOUTS[id]);
	const row = l.grid.root.data[0].data;
	row[0].size += by;
	row[1].size -= by;
	return l;
};

/** The arrangement saved before there were workspaces (media + transcript). */
const legacyLayout = (base = PRESET_LAYOUTS.edit) => {
	const l = clone(base);
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
		expect(parseWorkspaces(null)).toEqual({ active: 'edit', layouts: {}, offered: {}, library: { tabs: {}, collapsed: false } });
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
			layouts: { edit: nudged('edit'), color: nudged('color') },
			offered: { edit: presetPanelIds('edit'), color: presetPanelIds('color') },
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
				color: nudged('color'),
				audio: null,
				motion: nudged('motion'),
				deliver: 5,
				compositing: clone(PRESET_LAYOUTS.edit)
			}
		});
		expect(Object.keys(parsed.layouts).sort()).toEqual(['color', 'motion']);
		expect(parsed.layouts.color).toEqual(nudged('color'));
	});

	test('a stored layout is sanitized on the way in: registry titles, floating groups dropped', () => {
		const edit = nudged('edit');
		edit.panels.inspector.title = 'Old name';
		edit.floatingGroups = [{ data: {}, position: {} }];
		const parsed = parseWorkspaces({ layouts: { edit } });
		expect(parsed.layouts.edit).toEqual(nudged('edit'));
	});

	test('a stored layout from before the library is migrated, not thrown away', () => {
		const parsed = parseWorkspaces({ layouts: { edit: legacyLayout(nudged('edit') as never) } });
		expect(parsed.layouts.edit).toEqual(nudged('edit'));
	});

	test('a layout saved under the wrong workspace is still a layout (it is the user’s arrangement)', () => {
		const parsed = parseWorkspaces({
			layouts: { audio: nudged('deliver') },
			offered: { audio: presetPanelIds('audio') }
		});
		expect(parsed.layouts.audio).toEqual(nudged('deliver'));
	});
});

describe('the layout from before workspaces', () => {
	test('becomes the Edit arrangement when there is no workspaces value', () => {
		const parsed = parseWorkspaces(null, legacyLayout(nudged('edit') as never));
		expect(parsed.active).toBe('edit');
		// Migrated: the tabbed pair is the library now.
		expect(parsed.layouts.edit).toEqual(nudged('edit'));
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
		expect(parseWorkspaces('garbage', legacyLayout(nudged('edit') as never)).layouts.edit).toEqual(nudged('edit'));
		expect(parseWorkspaces([1], legacyLayout(nudged('edit') as never)).layouts.edit).toEqual(nudged('edit'));
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

	test('a 12 px nudge of a sash is a change, and is written', () => {
		const nudged = scaled('edit', 1);
		nudged.grid.root.data[0].data[0].size += 12;
		nudged.grid.root.data[0].data[1].size -= 12;
		expect(shouldPersistLayout(nudged, scaled('edit', 1), preset, false)).toBe(true);
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

// ---- layouts stored by an earlier build -------------------------------------

/** What a dock of `width` px reports for a preset it was never rearranged from:
 *  the preset's shares at the window's pixels. */
const scaledTo = (layout: any, width: number) => {
	const l = clone(layout);
	const k = width / l.grid.width;
	const scale = (n: any) => {
		if (typeof n.size === 'number') n.size = Math.round(n.size * k);
		if (n.type === 'branch') n.data.forEach(scale);
	};
	scale(l.grid.root);
	l.grid.width = width;
	return l;
};
const viewList = (layout: any): string[] => {
	const visit = (n: any): string[] => (n.type === 'leaf' ? n.data.views : n.data.flatMap(visit));
	return visit(layout.grid.root);
};
/** An Audio arrangement from before the mixer, which the user had changed. */
const arrangedAudioWithoutMixer = () => {
	const l = clone(EARLIER_PRESETS.audio);
	l.grid.root.data[0].data[0].size = 380;
	l.grid.root.data[0].data[1].size = 640;
	return scaledTo(l, 1280);
};

describe('a layout saved before the mixer', () => {
	test('a stored copy of the earlier Audio preset is dropped, so Audio is the new one (the stale Audio with no Mixer)', () => {
		// The build that stored every workspace the user visited left this behind.
		const stale = scaledTo(EARLIER_PRESETS.audio, 1280);
		expect(openPanelIds(stale)).not.toContain('mixer');
		const read = readWorkspaces({ active: 'audio', layouts: { audio: stale } });
		expect(read.state.layouts.audio).toBeUndefined();
		expect(read.changed).toBe(true);
		expect(read.adopted).toEqual([]);
		expect(openPanelIds(layoutFor(read.state, 'audio'))).toContain('mixer');
		expect(layoutFor(read.state, 'audio')).toEqual(PRESET_LAYOUTS.audio);
	});

	test('an Audio arrangement of the user’s own gets the mixer where the preset puts it, and keeps the rest', () => {
		const mine = arrangedAudioWithoutMixer();
		const read = readWorkspaces({ layouts: { audio: mine } });
		const audio = read.state.layouts.audio!;
		expect(read.adopted).toEqual([{ workspace: 'audio', panels: ['mixer'] }]);
		expect(read.changed).toBe(true);
		expect(viewList(audio)).toEqual(['library', 'preview', 'mixer', 'inspector', 'agent', 'timeline']);
		expect(audio.grid.root).not.toEqual(mine.grid.root);
		// What they had keeps its proportions (their wide library against the preview),
		// and their timeline is as tall as it was.
		const row = (audio.grid.root as any).data[0].data;
		expect(row[0].size / row[1].size).toBeCloseTo(380 / 640, 1);
		expect((audio.grid.root as any).data[1].size).toBe((mine.grid.root as any).data[1].size);
		// It is recorded against today's preset, so this happens once.
		expect(read.state.offered.audio).toEqual(presetPanelIds('audio'));
		expect(sanitizeLayout(clone(audio))).toEqual(audio);
	});

	test('and once is all: reading what was written back changes nothing', () => {
		const first = readWorkspaces({ layouts: { audio: arrangedAudioWithoutMixer() } });
		const second = readWorkspaces(clone(first.state));
		expect(second.changed).toBe(false);
		expect(second.adopted).toEqual([]);
		expect(second.state).toEqual(first.state);
	});

	test('a mixer the user closed stays closed: the layout was offered it', () => {
		const mine = nudged('audio');
		const closedMixer = clone(mine);
		const row = closedMixer.grid.root.data[0].data;
		const at = row.findIndex((n: any) => n.data.views[0] === 'mixer');
		const [gone] = row.splice(at, 1);
		row[at - 1].size += gone.size;
		delete closedMixer.panels.mixer;
		const read = readWorkspaces({
			layouts: { audio: closedMixer },
			offered: { audio: presetPanelIds('audio') }
		});
		expect(viewList(read.state.layouts.audio)).not.toContain('mixer');
		expect(read.adopted).toEqual([]);
		expect(read.changed).toBe(false);
	});

	test('a layout stored with a stamp from before the mixer is given it', () => {
		const read = readWorkspaces({
			layouts: { audio: arrangedAudioWithoutMixer() },
			offered: { audio: ['library', 'preview', 'timeline', 'inspector', 'agent'] }
		});
		expect(viewList(read.state.layouts.audio)).toContain('mixer');
		expect(read.adopted).toEqual([{ workspace: 'audio', panels: ['mixer'] }]);
	});

	test('a mixer the user opened themselves is not opened twice', () => {
		const mine = nudged('audio');
		const read = readWorkspaces({ layouts: { audio: mine }, offered: { audio: ['library', 'preview', 'timeline', 'inspector', 'agent'] } });
		expect(viewList(read.state.layouts.audio).filter((v) => v === 'mixer')).toHaveLength(1);
		expect(read.adopted).toEqual([]);
		expect(read.state.offered.audio).toEqual(presetPanelIds('audio'));
	});

	test('no other workspace is given a mixer', () => {
		for (const id of WORKSPACE_IDS.filter((w) => w !== 'audio')) {
			const read = readWorkspaces({ layouts: { [id]: nudged(id) } });
			expect(viewList(read.state.layouts[id]!)).not.toContain('mixer');
			expect(read.adopted).toEqual([]);
		}
	});

	test('a layout that is only a copy of today’s preset is dropped, so the preset can move later', () => {
		for (const id of WORKSPACE_IDS) {
			const read = readWorkspaces({ layouts: { [id]: scaledTo(PRESET_LAYOUTS[id], 1000) } });
			expect(read.state.layouts[id]).toBeUndefined();
			expect(read.changed).toBe(true);
		}
		// With a stamp it is the user’s: they dragged it back, and it stays.
		const stamped = readWorkspaces({
			layouts: { color: scaledTo(PRESET_LAYOUTS.color, 1000) },
			offered: { color: presetPanelIds('color') }
		});
		expect(stamped.state.layouts.color).toBeDefined();
		expect(stamped.changed).toBe(false);
	});

	test('an unreadable stamp is no stamp', () => {
		const mine = arrangedAudioWithoutMixer();
		for (const offered of ['x', 7, { 0: 'a' }, null]) {
			const read = readWorkspaces({ layouts: { audio: mine }, offered: { audio: offered } });
			expect(viewList(read.state.layouts.audio)).toContain('mixer');
		}
		// Names that are not panels are dropped from a stamp, not trusted.
		const read = readWorkspaces({ layouts: { audio: nudged('audio') }, offered: { audio: ['library', 'bogus', 7] } });
		expect(read.state.offered.audio).toEqual(presetPanelIds('audio'));
	});

	test('a stamp for a layout that does not parse is forgotten with it', () => {
		const read = readWorkspaces({ layouts: { audio: { grid: 'x' } }, offered: { audio: presetPanelIds('audio') } });
		expect(read.state.offered).toEqual({});
	});

	test('the layout from before workspaces is the Edit arrangement, recorded', () => {
		const read = readWorkspaces(null, legacyLayout(nudged('edit') as never));
		expect(read.state.layouts.edit).toEqual(nudged('edit'));
		expect(read.state.offered.edit).toEqual(presetPanelIds('edit'));
		expect(read.changed).toBe(true);
	});

	test('nothing stored is nothing to write back', () => {
		for (const raw of [undefined, null, {}, { active: 'color' }, { library: { collapsed: true } }]) {
			expect(readWorkspaces(raw).changed).toBe(false);
		}
	});
});

describe('adoptPanels', () => {
	test('is a no-op on an up-to-date layout', () => {
		const mine = nudged('color');
		const r = adoptPanels('color', mine, presetPanelIds('color'));
		expect(r.layout).toEqual(mine);
		expect(r.added).toEqual([]);
		expect(r.changed).toBe(false);
	});
});

describe('what the user is told', () => {
	test('one workspace gaining one panel is said plainly', () => {
		const note = describeAdopted([{ workspace: 'audio', panels: ['mixer'] }])!;
		expect(note.message).toBe('Audio workspace gained the Mixer panel');
		expect(note.description).toContain('Resetting the workspace restores its default');
	});

	test('several are listed', () => {
		const note = describeAdopted([
			{ workspace: 'audio', panels: ['mixer'] },
			{ workspace: 'deliver', panels: ['mixer'] }
		])!;
		expect(note.message).toBe('Workspaces gained new panels');
		expect(note.description).toContain('Audio: Mixer; Deliver: Mixer');
	});

	test('nothing gained is nothing said', () => {
		expect(describeAdopted([])).toBeNull();
	});
});

describe('storing and forgetting layouts', () => {
	test('withLayout records the layout against the panels its preset opens now', () => {
		const next = withLayout(defaultWorkspaces(), 'audio', nudged('audio'));
		expect(next.layouts.audio).toEqual(nudged('audio'));
		expect(next.offered.audio).toEqual(presetPanelIds('audio'));
		expect(next.layouts.edit).toBeUndefined();
	});

	test('withoutWorkspaces forgets the arrangement, its record and the tab picked there — not the fold, not the others', () => {
		let state = withLayout(withLayout(defaultWorkspaces(), 'audio', nudged('audio')), 'color', nudged('color'));
		state = withLibraryTab(state, 'audio', 'transcript');
		state = withLibraryTab(state, 'color', 'titles');
		state = { ...state, library: { ...state.library, collapsed: true } };
		const next = withoutWorkspaces(state, ['audio']);
		expect(next.layouts.audio).toBeUndefined();
		expect(next.offered.audio).toBeUndefined();
		expect(next.library.tabs.audio).toBeUndefined();
		expect(libraryTabFor(next, 'audio')).toBe('audio');
		expect(next.layouts.color).toEqual(nudged('color'));
		expect(next.library.tabs.color).toBe('titles');
		expect(next.library.collapsed).toBe(true);
		// The old state is not touched.
		expect(state.layouts.audio).toBeDefined();
	});

	test('withoutWorkspaces on everything leaves the defaults and the fold', () => {
		let state = withLayout(defaultWorkspaces(), 'edit', nudged('edit'));
		state = withLibraryTab({ ...state, active: 'motion' }, 'motion', 'media');
		const next = withoutWorkspaces(state, WORKSPACE_IDS);
		expect(next).toEqual({ ...defaultWorkspaces(), active: 'motion' });
	});

	test('withoutWorkspaces is the same object when there was nothing to forget', () => {
		const state = defaultWorkspaces();
		expect(withoutWorkspaces(state, WORKSPACE_IDS)).toBe(state);
	});

	test('a forgotten workspace is its preset, and a preset is not an arrangement', () => {
		const state = withoutWorkspaces(withLayout(defaultWorkspaces(), 'color', nudged('color')), ['color']);
		expect(layoutFor(state, 'color')).toEqual(PRESET_LAYOUTS.color);
		expect(sameArrangement(layoutFor(state, 'color'), nudged('color'))).toBe(false);
	});
});

describe('a workspace with panels in windows of their own', () => {
	/** A nudged Audio layout whose mixer is in a window: the mixer is *in* the layout, just
	 *  not in its grid. */
	function audioWithDetachedMixer() {
		const l = nudged('audio');
		const visit = (n: any): any => (n.type === 'leaf' ? (n.data.views.includes('mixer') ? n : undefined) : n.data.map(visit).find(Boolean));
		const leaf = visit(l.grid.root);
		const views = leaf.data.views;
		leaf.data.views = [];
		delete leaf.data.activeView;
		leaf.visible = false;
		l.popoutGroups = [{ data: { id: 'win-m', views, activeView: views[0] }, gridReferenceGroup: leaf.data.id, position: { left: 2300, top: 60, width: 460, height: 500 } }];
		return l;
	}

	test('survive being stored and read back, windows and all', () => {
		const layout = audioWithDetachedMixer();
		const stored = withLayout(defaultWorkspaces(), 'audio', layout);
		const read = readWorkspaces(clone(stored));
		expect(read.state.layouts.audio?.popoutGroups).toHaveLength(1);
		expect(read.state.layouts.audio?.popoutGroups?.[0].position).toEqual({ left: 2300, top: 60, width: 460, height: 500 });
		expect(read.changed).toBe(false);
		expect(layoutFor(read.state, 'audio').popoutGroups).toHaveLength(1);
	});

	test('a panel that is in a window is not "new" to a layout and is not put in the grid as well', () => {
		// Stored before the record of what was offered: the mixer is not in `UNSTAMPED_OFFERED`, so
		// the layout would be given it — but it already has it, in a window.
		const layout = audioWithDetachedMixer();
		const r = adoptPanels('audio', sanitizeLayout(layout)!, null);
		expect(r.added).toEqual([]);
		const gridViews = (n: any): string[] => (n.type === 'leaf' ? n.data.views : n.data.flatMap(gridViews));
		expect(gridViews(r.layout!.grid.root)).not.toContain('mixer');
		expect(r.layout!.popoutGroups?.[0].data?.views).toEqual(['mixer']);
	});

	test('is a different arrangement from the preset, so it is not dropped as a copy of one', () => {
		const layout = sanitizeLayout(audioWithDetachedMixer())!;
		expect(sameArrangement(layout, PRESET_LAYOUTS.audio)).toBe(false);
		const r = adoptPanels('audio', layout, null);
		expect(r.layout).not.toBeNull();
	});

	test('a moved window is worth writing, one nudged by the platform is not', () => {
		const reference = sanitizeLayout(audioWithDetachedMixer())!;
		const nudgedBy = (dx: number) => {
			const l = clone(reference);
			l.popoutGroups[0].position.left += dx;
			return l;
		};
		expect(shouldPersistLayout(nudgedBy(8), reference, presetLayout('audio'), true)).toBe(false);
		expect(shouldPersistLayout(nudgedBy(300), reference, presetLayout('audio'), true)).toBe(true);
	});
});
