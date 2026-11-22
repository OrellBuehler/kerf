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
	MAX_CUSTOM_WORKSPACES,
	MAX_WORKSPACE_NAME,
	WORKSPACE_SPECS,
	addBlocked,
	addWorkspace,
	adoptPanels,
	checkName,
	defaultWorkspaces,
	describeAdopted,
	hasStoredArrangement,
	isBuiltinKey,
	isLibraryTab,
	layoutFor,
	libraryTabFor,
	moveWorkspace,
	newWorkspaceKey,
	parseWorkspaces,
	presetKey,
	readWorkspaces,
	removeWorkspace,
	renameWorkspace,
	shouldPersistLayout,
	stepTab,
	stepWorkspace,
	suggestName,
	tidyName,
	withLayout,
	withLibraryTab,
	withoutWorkspaces,
	workspaceEntries,
	workspaceLabel,
	workspaceSpec,
	type WorkspacesState
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
		expect(LIBRARY_TABS).toEqual(['media', 'titles', 'effects', 'transitions', 'motion', 'audio', 'transcript']);
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
			['motion', 'motion'],
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
		expect(parseWorkspaces(null)).toEqual({
			active: 'edit',
			layouts: {},
			offered: {},
			library: { tabs: {}, collapsed: false },
			custom: [],
			order: ['edit', 'color', 'audio', 'motion', 'deliver']
		});
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
			library: { tabs: { edit: 'transcript', audio: 'transitions' }, collapsed: true },
			custom: [],
			order: ['edit', 'color', 'audio', 'motion', 'deliver']
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
		expect(libraryTabFor(state, 'motion')).toBe('motion');
	});

	test('a Titles pick made in Motion before it had a tab of its own still stands', () => {
		const stored = parseWorkspaces({ library: { tabs: { motion: 'titles' }, collapsed: false } });
		expect(libraryTabFor(stored, 'motion')).toBe('titles');
		expect(libraryTabFor(defaultWorkspaces(), 'motion')).toBe('motion');
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
/** The Audio preset as the build before the Sound FX panel shipped it: the mixer alone in its group. */
const audioBeforeSoundFx = () => {
	const l = clone(PRESET_LAYOUTS.audio);
	const visit = (n: any) => {
		if (n.type === 'leaf') n.data.views = n.data.views.filter((v: string) => v !== 'sfx');
		else n.data.forEach(visit);
	};
	visit(l.grid.root);
	delete l.panels.sfx;
	return l;
};
/** That arrangement with a sash dragged: the user's own. */
const arrangedAudioBeforeSoundFx = () => {
	const l = audioBeforeSoundFx();
	l.grid.root.data[0].data[0].size += 50;
	l.grid.root.data[0].data[1].size -= 50;
	return l;
};
/** The panels that build's Audio opened, as it stamped a layout with them. */
const OFFERED_BEFORE_SOUND_FX = ['library', 'preview', 'mixer', 'inspector', 'agent', 'timeline'];

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
		expect(read.adopted).toEqual([{ workspace: 'audio', panels: ['mixer', 'sfx'] }]);
		expect(read.changed).toBe(true);
		expect(viewList(audio)).toEqual(['library', 'preview', 'mixer', 'sfx', 'inspector', 'agent', 'timeline']);
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
		expect(read.adopted).toEqual([{ workspace: 'audio', panels: ['mixer', 'sfx'] }]);
	});

	test('a layout stamped with the mixer but not the Sound FX panel is given that panel as a tab of the mixer’s group, and keeps the rest', () => {
		const mine = arrangedAudioBeforeSoundFx();
		const read = readWorkspaces({ layouts: { audio: mine }, offered: { audio: OFFERED_BEFORE_SOUND_FX } });
		const audio = read.state.layouts.audio!;
		expect(read.adopted).toEqual([{ workspace: 'audio', panels: ['sfx'] }]);
		expect(read.changed).toBe(true);
		expect(viewList(audio)).toEqual(['library', 'preview', 'mixer', 'sfx', 'inspector', 'agent', 'timeline']);
		// Tabbed: no group was cut into the row, so nothing they sized moved.
		expect(sameArrangement(audio, mine)).toBe(false);
		const row = (audio.grid.root as any).data[0].data;
		expect(row.map((n: any) => n.size)).toEqual((mine.grid.root as any).data[0].data.map((n: any) => n.size));
		expect(row[2].data.views).toEqual(['mixer', 'sfx']);
		expect((audio.grid.root as any).data[1].size).toBe((mine.grid.root as any).data[1].size);
		// Recorded against today's preset, so it happens once; and the user is told, in the words of the other panels.
		expect(read.state.offered.audio).toEqual(presetPanelIds('audio'));
		expect(describeAdopted(read.adopted)!.message).toBe('Audio workspace gained the Sound FX panel');
		expect(sanitizeLayout(clone(audio))).toEqual(audio);
		const again = readWorkspaces(clone(read.state));
		expect(again.changed).toBe(false);
		expect(again.adopted).toEqual([]);
	});

	test('a Sound FX panel the user closed stays closed: the layout was offered it', () => {
		const mine = nudged('audio');
		const noSfx = clone(mine);
		const visit = (n: any) => {
			if (n.type === 'leaf') n.data.views = n.data.views.filter((v: string) => v !== 'sfx');
			else n.data.forEach(visit);
		};
		visit(noSfx.grid.root);
		delete noSfx.panels.sfx;
		const read = readWorkspaces({ layouts: { audio: noSfx }, offered: { audio: presetPanelIds('audio') } });
		expect(viewList(read.state.layouts.audio)).not.toContain('sfx');
		expect(read.adopted).toEqual([]);
		expect(read.changed).toBe(false);
	});

	test('with the mixer closed the Sound FX panel is put beside the preview, where the mixer’s group was', () => {
		const mine = arrangedAudioBeforeSoundFx();
		const row = mine.grid.root.data[0].data;
		const at = row.findIndex((n: any) => n.data.views[0] === 'mixer');
		const [gone] = row.splice(at, 1);
		row[at - 1].size += gone.size;
		delete mine.panels.mixer;
		const read = readWorkspaces({ layouts: { audio: mine }, offered: { audio: OFFERED_BEFORE_SOUND_FX } });
		expect(read.adopted).toEqual([{ workspace: 'audio', panels: ['sfx'] }]);
		const audio = read.state.layouts.audio!;
		expect(viewList(audio)).toEqual(['library', 'preview', 'sfx', 'inspector', 'agent', 'timeline']);
		expect(viewList(audio)).not.toContain('mixer');
	});

	test('no other workspace is given a Sound FX panel', () => {
		for (const id of WORKSPACE_IDS.filter((w) => w !== 'audio')) {
			const read = readWorkspaces({ layouts: { [id]: nudged(id) }, offered: { [id]: presetPanelIds(id) } });
			expect(viewList(read.state.layouts[id]!)).not.toContain('sfx');
			expect(read.adopted).toEqual([]);
		}
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
			expect(viewList(read.state.layouts.audio)).toContain('sfx');
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

	test('two panels gained by one workspace are said together', () => {
		const note = describeAdopted([{ workspace: 'audio', panels: ['mixer', 'sfx'] }])!;
		expect(note.message).toBe('Audio workspace gained the Mixer and Sound FX panels');
	});

	test('the Sound FX panel gained on its own is said in the words the mixer was', () => {
		const note = describeAdopted([{ workspace: 'audio', panels: ['sfx'] }])!;
		expect(note.message).toBe('Audio workspace gained the Sound FX panel');
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
		expect(r.layout!.popoutGroups?.[0].data?.views).toEqual(['mixer', 'sfx']);
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


// ---- the workspaces a user saves ------------------------------------------------

const DEFAULT_ORDER = ['edit', 'color', 'audio', 'motion', 'deliver'];

/** A state with two saved workspaces, the way saving them leaves it. */
function withSaved(): WorkspacesState {
	let state = defaultWorkspaces();
	state = addWorkspace(state, 'w-aaaa1111', 'Review', nudged('edit'), 'media');
	state = addWorkspace(state, 'w-bbbb2222', 'Grade wide', nudged('color'), 'effects');
	return state;
}

describe('a value from before saved workspaces', () => {
	// What 0.31.0 wrote: no `custom`, no `order`.
	const stored = () => ({
		active: 'color',
		layouts: { edit: nudged('edit'), audio: nudged('audio') },
		offered: { edit: presetPanelIds('edit'), audio: presetPanelIds('audio') },
		library: { tabs: { edit: 'transcript', color: 'titles' } as const, collapsed: true }
	});

	test('reads as it did, with the buttons in their old order and nothing saved', () => {
		const read = readWorkspaces(stored());
		expect(read.changed).toBe(false);
		expect(read.adopted).toEqual([]);
		expect(read.state).toEqual({ ...stored(), custom: [], order: DEFAULT_ORDER });
	});

	test('every stored layout comes back untouched', () => {
		const read = readWorkspaces(clone(stored()));
		expect(read.state.layouts.edit).toEqual(sanitizeLayout(stored().layouts.edit) ?? undefined);
		expect(read.state.layouts.audio).toEqual(sanitizeLayout(stored().layouts.audio) ?? undefined);
	});

	test('writing what was read and reading it again changes nothing', () => {
		const once = readWorkspaces(stored());
		const again = readWorkspaces(clone(once.state));
		expect(again.state).toEqual(once.state);
		expect(again.changed).toBe(false);
	});
});

describe('the workspaces a user saved, as stored', () => {
	test('round-trip through JSON with their layouts, tabs, order and the one on show', () => {
		const state = { ...withSaved(), active: 'w-bbbb2222' };
		const read = readWorkspaces(clone(state));
		expect(read.changed).toBe(false);
		expect(read.state).toEqual(state);
		expect(read.state.active).toBe('w-bbbb2222');
		expect(libraryTabFor(read.state, 'w-aaaa1111')).toBe('media');
		expect(libraryTabFor(read.state, 'w-bbbb2222')).toBe('effects');
	});

	test('a saved workspace is not offered the preset’s panels, and is not given any', () => {
		const state = withSaved();
		expect(state.offered['w-aaaa1111']).toBeUndefined();
		const read = readWorkspaces(clone(state));
		// The layout is the user's own: no mixer is cut into it, and nobody is told one was.
		expect(read.adopted).toEqual([]);
		expect(read.state.offered['w-aaaa1111']).toBeUndefined();
		const mine = withLayout(state, 'w-aaaa1111', nudged('audio'));
		expect(mine.offered['w-aaaa1111']).toBeUndefined();
		expect(withLayout(state, 'audio', nudged('audio')).offered.audio).toEqual(presetPanelIds('audio'));
	});

	test('one bad entry costs only itself: the good ones, the layouts and the order stand', () => {
		const state = withSaved();
		const raw = clone(state);
		raw.custom = [
			raw.custom[0],
			{ id: 7, name: 'no id' },
			{ id: 'w-cccc3333', name: '   ' },
			'nonsense',
			null,
			{ id: 'edit', name: 'Impostor' },
			{ id: 'w-bad id', name: 'Odd id' },
			raw.custom[1]
		];
		const read = readWorkspaces(raw);
		expect(read.changed).toBe(true);
		expect(read.state.custom.map((c) => c.id)).toEqual(['w-aaaa1111', 'w-bbbb2222']);
		expect(read.state.layouts['w-aaaa1111']).toBeDefined();
		expect(read.state.layouts['w-bbbb2222']).toBeDefined();
		expect(read.state.order).toEqual([...DEFAULT_ORDER, 'w-aaaa1111', 'w-bbbb2222']);
	});

	test('a saved workspace whose layout is unreadable keeps its name and opens as Edit does; the others keep theirs', () => {
		const raw = clone(withSaved());
		raw.layouts['w-aaaa1111'] = { grid: 'broken' };
		const read = readWorkspaces(raw);
		expect(read.changed).toBe(true);
		expect(read.state.custom.map((c) => c.name)).toEqual(['Review', 'Grade wide']);
		expect(read.state.layouts['w-aaaa1111']).toBeUndefined();
		expect(layoutFor(read.state, 'w-aaaa1111')).toEqual(presetLayout('edit'));
		expect(read.state.layouts['w-bbbb2222']).toBeDefined();
	});

	test('a layout that is simply missing is not a change to write back', () => {
		const raw = clone(withSaved());
		delete raw.layouts['w-aaaa1111'];
		expect(readWorkspaces(raw).changed).toBe(false);
	});

	test('a duplicated id is read once; a name is tidied and cut to what a button holds; at most the limit are kept', () => {
		const raw = clone(withSaved());
		raw.custom = [
			{ id: 'w-aaaa1111', name: '  Review \n   cut ' },
			{ id: 'w-aaaa1111', name: 'Again' },
			{ id: 'w-bbbb2222', name: 'x'.repeat(60) },
			...Array.from({ length: 12 }, (_, i) => ({ id: `w-extra${i}`, name: `Extra ${i}` }))
		];
		const read = readWorkspaces(raw);
		expect(read.state.custom).toHaveLength(MAX_CUSTOM_WORKSPACES);
		expect(read.state.custom[0]).toEqual({ id: 'w-aaaa1111', name: 'Review cut' });
		expect(read.state.custom[1].name).toBe('x'.repeat(MAX_WORKSPACE_NAME));
		expect(read.changed).toBe(true);
	});

	test('what is not a list is none, and is written back as none', () => {
		const read = readWorkspaces({ custom: 'a lot', active: 'edit' });
		expect(read.state.custom).toEqual([]);
		expect(read.changed).toBe(true);
	});

	test('the one on show is a saved workspace only if there is one; otherwise Edit', () => {
		expect(readWorkspaces({ active: 'w-aaaa1111' }).state.active).toBe('edit');
		expect(readWorkspaces({ ...clone(withSaved()), active: 'w-bbbb2222' }).state.active).toBe('w-bbbb2222');
		expect(readWorkspaces({ ...clone(withSaved()), active: 'w-nobody' }).state.active).toBe('edit');
	});

	test('the order is read field by field: unknown and repeated entries go, missing ones come last in default order', () => {
		const raw = { ...clone(withSaved()), order: ['w-bbbb2222', 'nope', 'audio', 'audio', 3, 'edit'] };
		expect(readWorkspaces(raw).state.order).toEqual(['w-bbbb2222', 'audio', 'edit', 'color', 'motion', 'deliver', 'w-aaaa1111']);
		expect(readWorkspaces({ ...clone(withSaved()), order: 'x' }).state.order).toEqual([...DEFAULT_ORDER, 'w-aaaa1111', 'w-bbbb2222']);
	});

	test('library tabs are read for the saved ones too, and for no stranger', () => {
		const raw = { ...clone(withSaved()), library: { tabs: { 'w-aaaa1111': 'audio', 'w-nobody': 'titles', color: 'motion', 'w-bbbb2222': 'nonsense' }, collapsed: false } };
		const read = readWorkspaces(raw);
		expect(read.state.library.tabs).toEqual({ 'w-aaaa1111': 'audio', color: 'motion' });
	});

	test('a hostile file does not get past the limits', () => {
		const raw = { custom: Array.from({ length: 5000 }, (_, i) => ({ id: `w-${i}abc`, name: 'n' })), order: Array.from({ length: 5000 }, (_, i) => `w-${i}abc`) };
		const read = readWorkspaces(raw);
		expect(read.state.custom).toHaveLength(MAX_CUSTOM_WORKSPACES);
		expect(read.state.order).toHaveLength(WORKSPACE_SPECS.length + MAX_CUSTOM_WORKSPACES);
	});
});

describe('the buttons', () => {
	test('are the five that come with Kerf, then the saved ones, in the stored order', () => {
		expect(workspaceEntries(defaultWorkspaces()).map((w) => w.key)).toEqual(DEFAULT_ORDER);
		const state = moveWorkspace(withSaved(), 'w-bbbb2222', 1);
		expect(workspaceEntries(state).map((w) => w.label)).toEqual(['Edit', 'Grade wide', 'Color', 'Audio', 'Motion', 'Deliver', 'Review']);
		expect(workspaceEntries(state).filter((w) => !w.builtin).map((w) => w.key)).toEqual(['w-bbbb2222', 'w-aaaa1111']);
	});

	test('a key the state does not know has no button, and labels fall back to Edit', () => {
		const state = { ...defaultWorkspaces(), order: ['edit', 'w-ghost'] };
		expect(workspaceEntries(state).map((w) => w.key)).toEqual(['edit']);
		expect(workspaceLabel(state, 'w-ghost')).toBe('Edit');
	});

	test('built-in keys are told from saved ones, and a saved one starts from Edit’s preset', () => {
		expect(WORKSPACE_IDS.every(isBuiltinKey)).toBe(true);
		expect(isBuiltinKey('w-aaaa1111')).toBe(false);
		expect(isBuiltinKey(undefined)).toBe(false);
		expect(presetKey('audio')).toBe('audio');
		expect(presetKey('w-aaaa1111')).toBe('edit');
	});
});

describe('naming a workspace', () => {
	test('is tidied: control characters and runs of whitespace become one space, the ends go', () => {
		expect(tidyName('  My\t \n cut  ')).toBe('My cut');
		expect(tidyName('a\u0000b')).toBe('a b');
		expect(tidyName('   ')).toBe('');
	});

	test('needs something typed, in what a button holds', () => {
		const state = defaultWorkspaces();
		expect(checkName(state, '   ')).toEqual({ ok: false, reason: 'Give the workspace a name' });
		expect(checkName(state, 'x'.repeat(MAX_WORKSPACE_NAME))).toEqual({ ok: true, name: 'x'.repeat(MAX_WORKSPACE_NAME) });
		const long = checkName(state, 'x'.repeat(MAX_WORKSPACE_NAME + 1));
		expect(long.ok).toBe(false);
		expect(!long.ok && long.reason).toContain(String(MAX_WORKSPACE_NAME));
		// Characters, not UTF-16 units.
		expect(checkName(state, '🎬'.repeat(MAX_WORKSPACE_NAME)).ok).toBe(true);
	});

	test('is not another workspace’s, a built-in one’s included, whatever the case', () => {
		const state = withSaved();
		expect(checkName(state, 'color')).toEqual({ ok: false, reason: 'There is already a workspace called Color' });
		expect(checkName(state, ' REVIEW ')).toEqual({ ok: false, reason: 'There is already a workspace called Review' });
		expect(checkName(state, 'Review 2').ok).toBe(true);
	});

	test('a workspace being renamed may keep its own name, in any case', () => {
		const state = withSaved();
		expect(checkName(state, 'review', 'w-aaaa1111')).toEqual({ ok: true, name: 'review' });
		expect(checkName(state, 'Color', 'w-aaaa1111').ok).toBe(false);
	});

	test('a name is offered that is free', () => {
		let state = defaultWorkspaces();
		expect(suggestName(state)).toBe('My workspace');
		state = addWorkspace(state, 'w-aaaa1111', 'My workspace', presetLayout('edit'), 'media');
		expect(suggestName(state)).toBe('My workspace 2');
		state = addWorkspace(state, 'w-bbbb2222', 'My workspace 2', presetLayout('edit'), 'media');
		expect(suggestName(state)).toBe('My workspace 3');
	});

	test('the limit is told before the dialog opens', () => {
		let state = defaultWorkspaces();
		expect(addBlocked(state)).toBeNull();
		for (let i = 0; i < MAX_CUSTOM_WORKSPACES; i++) state = addWorkspace(state, `w-key${i}abc`, `W${i}`, presetLayout('edit'), 'media');
		expect(addBlocked(state)).toContain(String(MAX_CUSTOM_WORKSPACES));
	});

	test('a new id is one that nobody has, and is one the reader accepts', () => {
		const state = withSaved();
		const key = newWorkspaceKey(state);
		expect(key).toMatch(/^w-[a-z0-9]{8}$/);
		expect(state.custom.some((c) => c.id === key)).toBe(false);
		// The first draw is taken: the next is tried.
		const draws = [0, 0, 0.5];
		const taken = { ...state, custom: [...state.custom, { id: `w-${'0'.repeat(8)}`, name: 'Zeros' }] };
		expect(newWorkspaceKey(taken, () => draws.shift() ?? 0.5)).not.toBe(`w-${'0'.repeat(8)}`);
		const read = readWorkspaces({ ...clone(state), custom: [...clone(state.custom), { id: key, name: 'New' }] });
		expect(read.state.custom.map((c) => c.id)).toContain(key);
	});
});

describe('saving, renaming, deleting and ordering', () => {
	test('saving puts a button last, makes it the one on show and gives it its tab and layout', () => {
		const layout = nudged('edit');
		const state = addWorkspace(defaultWorkspaces(), 'w-aaaa1111', 'Review', layout, 'transcript');
		expect(state.active).toBe('w-aaaa1111');
		expect(state.order).toEqual([...DEFAULT_ORDER, 'w-aaaa1111']);
		expect(state.custom).toEqual([{ id: 'w-aaaa1111', name: 'Review' }]);
		expect(state.layouts['w-aaaa1111']).toBe(layout);
		expect(libraryTabFor(state, 'w-aaaa1111')).toBe('transcript');
		// Nothing of the other workspaces moved.
		expect(state.layouts.edit).toBeUndefined();
		expect(state.library.collapsed).toBe(false);
	});

	test('a saved workspace opens as it was saved, and as Edit when its layout is gone', () => {
		const state = withSaved();
		expect(sameArrangement(layoutFor(state, 'w-aaaa1111'), nudged('edit'))).toBe(true);
		const gone = { ...state, layouts: {} };
		expect(sameArrangement(layoutFor(gone, 'w-aaaa1111'), PRESET_LAYOUTS.edit)).toBe(true);
	});

	test('a library tab of a saved workspace is its own until one is picked there', () => {
		const state = addWorkspace(defaultWorkspaces(), 'w-aaaa1111', 'Review', presetLayout('edit'), 'media');
		const stateNoTab = { ...state, library: { ...state.library, tabs: {} } };
		expect(libraryTabFor(stateNoTab, 'w-aaaa1111')).toBe('media');
		expect(libraryTabFor(withLibraryTab(state, 'w-aaaa1111', 'audio'), 'w-aaaa1111')).toBe('audio');
	});

	test('renaming changes the name and nothing else; a built-in one, a stranger and the same name are no change', () => {
		const state = withSaved();
		const next = renameWorkspace(state, 'w-aaaa1111', 'Rough cut');
		expect(next.custom).toEqual([{ id: 'w-aaaa1111', name: 'Rough cut' }, { id: 'w-bbbb2222', name: 'Grade wide' }]);
		expect(next.layouts).toBe(state.layouts);
		expect(next.order).toBe(state.order);
		expect(state.custom[0].name).toBe('Review');
		expect(renameWorkspace(state, 'edit', 'Cutting')).toBe(state);
		expect(renameWorkspace(state, 'w-nobody', 'X')).toBe(state);
		expect(renameWorkspace(state, 'w-aaaa1111', 'Review')).toBe(state);
	});

	test('deleting forgets the layout, the tab and the button, and leaves the others and the built-in ones alone', () => {
		const state = { ...withSaved(), active: 'edit' };
		const next = removeWorkspace(state, 'w-aaaa1111');
		expect(next.custom.map((c) => c.id)).toEqual(['w-bbbb2222']);
		expect(next.layouts['w-aaaa1111']).toBeUndefined();
		expect(next.layouts['w-bbbb2222']).toBeDefined();
		expect(next.library.tabs['w-aaaa1111']).toBeUndefined();
		expect(next.order).toEqual([...DEFAULT_ORDER, 'w-bbbb2222']);
		expect(next.active).toBe('edit');
		expect(state.custom).toHaveLength(2);
	});

	test('deleting the one on show shows the one beside its button: the one before, else the one after', () => {
		const state = withSaved();
		expect(removeWorkspace({ ...state, active: 'w-bbbb2222' }, 'w-bbbb2222').active).toBe('w-aaaa1111');
		expect(removeWorkspace({ ...state, active: 'w-aaaa1111' }, 'w-aaaa1111').active).toBe('deliver');
		const first = moveWorkspace(state, 'w-aaaa1111', 0);
		expect(removeWorkspace({ ...first, active: 'w-aaaa1111' }, 'w-aaaa1111').active).toBe('edit');
	});

	test('a built-in workspace cannot be deleted: the state is the same object', () => {
		const state = withSaved();
		for (const id of WORKSPACE_IDS) expect(removeWorkspace(state, id)).toBe(state);
		expect(removeWorkspace(state, 'w-nobody')).toBe(state);
	});

	test('moving puts a button at the place it ends up in; the other buttons keep their order', () => {
		const state = withSaved();
		expect(moveWorkspace(state, 'w-bbbb2222', 0).order).toEqual(['w-bbbb2222', ...DEFAULT_ORDER, 'w-aaaa1111']);
		expect(moveWorkspace(state, 'edit', 4).order).toEqual(['color', 'audio', 'motion', 'deliver', 'edit', 'w-aaaa1111', 'w-bbbb2222']);
		expect(moveWorkspace(state, 'edit', 99).order.at(-1)).toBe('edit');
		expect(moveWorkspace(state, 'edit', -5).order[0]).toBe('edit');
		// A built-in button moves like any other.
		expect(moveWorkspace(state, 'deliver', 1).order.slice(0, 3)).toEqual(['edit', 'deliver', 'color']);
	});

	test('moving to where it is, a stranger or a number that is none is no change', () => {
		const state = withSaved();
		expect(moveWorkspace(state, 'edit', 0)).toBe(state);
		expect(moveWorkspace(state, 'w-nobody', 1)).toBe(state);
		expect(moveWorkspace(state, 'edit', NaN)).toBe(state);
		expect(state.order).toEqual([...DEFAULT_ORDER, 'w-aaaa1111', 'w-bbbb2222']);
	});

	test('stepping walks the buttons in their order and wraps', () => {
		const state = moveWorkspace(withSaved(), 'w-aaaa1111', 1);
		expect(stepWorkspace(state, 'edit', 1)).toBe('w-aaaa1111');
		expect(stepWorkspace(state, 'w-aaaa1111', 1)).toBe('color');
		expect(stepWorkspace(state, 'w-bbbb2222', 1)).toBe('edit');
		expect(stepWorkspace(state, 'edit', -1)).toBe('w-bbbb2222');
		expect(stepWorkspace(state, 'w-nobody', 1)).toBe('edit');
	});
});

describe('resetting, with workspaces of one’s own', () => {
	test('resetting every built-in workspace leaves the saved ones, their order and their tabs', () => {
		let state = withSaved();
		state = withLayout(state, 'edit', nudged('edit'));
		state = withLibraryTab(state, 'edit', 'titles');
		const reset = withoutWorkspaces(state, WORKSPACE_IDS);
		expect(reset.layouts.edit).toBeUndefined();
		expect(reset.library.tabs.edit).toBeUndefined();
		expect(reset.layouts['w-aaaa1111']).toBe(state.layouts['w-aaaa1111']);
		expect(reset.custom).toBe(state.custom);
		expect(reset.order).toBe(state.order);
		expect(reset.library.tabs['w-aaaa1111']).toBe('media');
	});

	test('what a reset would throw away is the built-in arrangements, not the saved ones', () => {
		expect(hasStoredArrangement(withSaved())).toBe(false);
		expect(hasStoredArrangement(withLayout(withSaved(), 'audio', nudged('audio')))).toBe(true);
		expect(hasStoredArrangement(withLibraryTab(withSaved(), 'color', 'motion'))).toBe(true);
	});
});

describe('a saved workspace is written only when it was arranged', () => {
	test('shouldPersistLayout holds it to its own reference, and Edit’s preset is no reason to skip a write', () => {
		const settled = nudged('edit');
		const moved = nudged('edit', 120);
		expect(shouldPersistLayout(moved, settled, presetLayout(presetKey('w-aaaa1111')), true)).toBe(true);
		expect(shouldPersistLayout(clone(settled), settled, presetLayout('edit'), true)).toBe(false);
		// Arranged to look like Edit's preset: it has an entry, so that is its arrangement and is kept.
		expect(shouldPersistLayout(presetLayout('edit'), settled, presetLayout('edit'), true)).toBe(true);
	});
});
