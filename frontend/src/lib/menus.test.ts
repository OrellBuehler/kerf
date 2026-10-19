import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ACTIONS, actionDef } from './keymap';
import { PANEL_IDS, type PanelId } from './layout';
import { WORKSPACE_IDS } from './workspaces';
import { DELIVERY_PRESETS } from './delivery-formats';
import { buildMenus, collapseMenus, entryLabel, flatten, focusable, stepFocus, typeahead, type MenuEntry, type MenuState } from './menus';

const state = (over: Partial<MenuState> = {}): MenuState => ({
	saved: false,
	canUndo: true,
	canRedo: true,
	hasClips: true,
	exporting: false,
	hasSelection: true,
	rippleMode: false,
	snap: true,
	minimap: true,
	safeAreas: false,
	hasFrame: true,
	tool: 'pointer',
	delivery: 'source',
	allHeight: 'medium',
	workspace: 'edit',
	openPanels: ['library', 'preview', 'timeline', 'inspector', 'agent'],
	detachedPanels: [],
	desktop: true,
	previewing: false,
	recent: [],
	...over
});

const menu = (id: string, over: Partial<MenuState> = {}) => buildMenus(state(over)).find((m) => m.id === id)!;
const labels = (items: readonly MenuEntry[]) =>
	items.flatMap((e) => (e.kind === 'separator' ? [] : [entryLabel(e)]));
const find = (items: readonly MenuEntry[], label: string) =>
	flatten(items).find((e) => e.kind !== 'separator' && entryLabel(e) === label) as Exclude<MenuEntry, { kind: 'separator' }>;

describe('the menu bar', () => {
	test('is File, Edit, View, Playback, Window, Help — in that order, each with its own name', () => {
		const menus = buildMenus(state());
		expect(menus.map((m) => m.label)).toEqual(['File', 'Edit', 'View', 'Playback', 'Window', 'Help']);
		expect(new Set(menus.map((m) => m.id)).size).toBe(6);
	});

	test('every action a menu runs is in the keymap registry, so its key is the user’s', () => {
		for (const m of buildMenus(state())) {
			for (const e of flatten(m.items)) {
				if (e.kind === 'action') expect(actionDef(e.id), `${m.label}: ${e.id}`).toBeDefined();
			}
		}
	});

	test('a separator never leads, trails or doubles in any menu or submenu', () => {
		const check = (items: readonly MenuEntry[], where: string) => {
			expect(items[0]?.kind, `${where} starts`).not.toBe('separator');
			expect(items.at(-1)?.kind, `${where} ends`).not.toBe('separator');
			items.forEach((e, i) => {
				if (e.kind === 'separator') expect(items[i - 1]?.kind, `${where} doubles`).not.toBe('separator');
				if (e.kind === 'submenu') check(e.items, `${where} > ${e.label}`);
			});
		};
		for (const m of buildMenus(state())) check(m.items, m.label);
	});

	test('no menu lists the same entry twice (Help and Edit each carry Keyboard shortcuts, once)', () => {
		for (const m of buildMenus(state())) {
			const own = m.items.filter((e) => e.kind !== 'separator').map((e) => entryLabel(e as never));
			expect(new Set(own).size, m.label).toBe(own.length);
		}
	});
});

describe('File', () => {
	test('holds the project commands, import, export and settings, in the order they are used', () => {
		const names = labels(menu('file').items);
		expect(names).toEqual([
			'New project',
			'Open project…',
			'Open Recent',
			'Save project…',
			'Import media…',
			'Import captions…',
			'Compose music…',
			'Export…',
			'Save cover frame…',
			'Export as',
			'Settings…',
			'Quit Kerf'
		]);
	});

	test('Export as holds the side-file exports, the marker lists off until there is a marker', () => {
		const sub = (over: Partial<MenuState>) => {
			const e = find(menu('file', over).items, 'Export as');
			return e.kind === 'submenu' ? e.items : [];
		};
		expect(labels(sub({ hasClips: true }))).toEqual([
			'Export captions…',
			'Export OTIO…',
			'Export EDL…',
			'Export markers as CSV…',
			'Export YouTube chapters…'
		]);
		const csv = find(sub({ hasClips: true, hasMarkers: false }), 'Export markers as CSV…');
		expect('disabled' in csv && csv.disabled).toBe(true);
		const on = find(sub({ hasClips: true, hasMarkers: true }), 'Export markers as CSV…');
		expect('disabled' in on && on.disabled).toBe(false);
	});

	test('Export… follows the title bar’s button: off with a reason for an empty cut, on once there is a clip, and on while a render runs', () => {
		const entry = (over: Partial<MenuState>) => find(menu('file', over).items, 'Export…');
		const empty = entry({ hasClips: false });
		expect('disabled' in empty && empty.disabled).toBe(true);
		expect('reason' in empty && empty.reason).toContain('Nothing to export');
		const live = entry({ hasClips: true });
		expect('disabled' in live && live.disabled).toBe(false);
		const running = entry({ hasClips: false, exporting: true });
		expect('disabled' in running && running.disabled).toBe(false);
	});

	test('Save is "as…" once the project has a file', () => {
		expect(labels(menu('file', { saved: true }).items)).toContain('Save project as…');
		expect(labels(menu('file', { saved: false }).items)).not.toContain('Save project as…');
	});

	test('the cover frame needs a cut; quitting needs the desktop app, and says so', () => {
		const empty = menu('file', { hasClips: false, desktop: false });
		const cover = find(empty.items, 'Save cover frame…');
		expect('disabled' in cover && cover.disabled).toBe(true);
		const quit = find(empty.items, 'Quit Kerf');
		expect('disabled' in quit && quit.disabled).toBe(true);
		expect('reason' in quit && quit.reason).toContain('desktop');
		const live = find(menu('file').items, 'Save cover frame…');
		expect('disabled' in live && live.disabled).toBeFalsy();
	});
});

describe('Edit', () => {
	test('holds undo and redo, the clipboard, deletion, titles, ripple and snapping', () => {
		const ids = flatten(menu('edit').items).flatMap((e) => (e.kind === 'action' ? [e.id] : []));
		for (const id of [
			'edit.undo',
			'edit.redo',
			'edit.cut',
			'edit.copy',
			'edit.paste',
			'edit.duplicate',
			'edit.delete',
			'edit.rippleDelete',
			'edit.selectAll',
			'title.add',
			'title.generateCaptions',
			'tool.rippleMode',
			'tool.snap',
			'app.keyboard'
		]) {
			expect(ids as string[]).toContain(id);
		}
	});

	test('undo and redo grey out with nothing to take back', () => {
		const m = menu('edit', { canUndo: false, canRedo: false });
		const undo = m.items[0];
		const redo = m.items[1];
		expect(undo.kind === 'action' && undo.disabled).toBe(true);
		expect(redo.kind === 'action' && redo.disabled).toBe(true);
		const on = menu('edit');
		expect(on.items[0].kind === 'action' && on.items[0].disabled).toBeFalsy();
	});

	test('what acts on the selection says why it is off when nothing is selected', () => {
		const m = menu('edit', { hasSelection: false });
		for (const label of ['Cut', 'Copy', 'Duplicate', 'Delete selection', 'Ripple delete selection']) {
			const e = find(m.items, label);
			expect('disabled' in e && e.disabled, label).toBe(true);
			expect('reason' in e && e.reason, label).toBe('Select a clip first');
		}
	});

	test('a title can always be added; captions wait for a clip on the cut', () => {
		const empty = menu('edit', { hasClips: false });
		const title = find(empty.items, 'Add title at playhead');
		expect('disabled' in title && title.disabled).toBeFalsy();
		const captions = find(empty.items, 'Generate captions');
		expect('disabled' in captions && captions.disabled).toBe(true);
		const live = find(menu('edit', { hasClips: true }).items, 'Generate captions');
		expect('disabled' in live && live.disabled).toBe(false);
	});

	test('every entry that edits the cut is off, with the one reason, while the proposal is previewed', () => {
		const REASON = 'Exit the proposal preview to edit';
		const m = menu('edit', { previewing: true });
		const edits = [
			'Undo',
			'Redo',
			'Cut',
			'Paste at playhead',
			'Duplicate',
			'Split at playhead',
			'Delete selection',
			'Ripple delete selection',
			'Add title at playhead',
			'Generate captions'
		];
		for (const label of edits) {
			const e = find(m.items, label);
			expect('disabled' in e && e.disabled, label).toBe(true);
			expect('reason' in e && e.reason, label).toBe(REASON);
		}
		for (const sub of ['Clip', 'Roll, slip, slide']) {
			const items = (find(m.items, sub) as Extract<MenuEntry, { kind: 'submenu' }>).items.filter((e) => e.kind !== 'separator');
			expect(items.length, sub).toBeGreaterThan(0);
			for (const e of items) {
				expect('disabled' in e && e.disabled, `${sub}: ${entryLabel(e)}`).toBe(true);
				expect('reason' in e && e.reason, `${sub}: ${entryLabel(e)}`).toBe(REASON);
			}
		}
		const frames = (find(menu('view', { previewing: true }).items, 'Delivery frame') as Extract<MenuEntry, { kind: 'submenu' }>).items;
		for (const e of frames) expect(e.kind !== 'separator' && e.disabled, 'delivery frame').toBe(true);
		for (const label of ['Import captions…', 'Compose music…']) {
			const e = find(menu('file', { previewing: true }).items, label);
			expect('disabled' in e && e.disabled, label).toBe(true);
			expect('reason' in e && e.reason, label).toBe(REASON);
		}
	});

	test('what only looks stays on while the proposal is previewed: copy, select all, the tools, ripple and snapping', () => {
		const m = menu('edit', { previewing: true });
		for (const label of ['Copy', 'Select all clips', 'Ripple mode', 'Snapping']) {
			const e = find(m.items, label);
			expect('disabled' in e && e.disabled, label).toBeFalsy();
		}
		const tool = find(m.items, 'Tool') as Extract<MenuEntry, { kind: 'submenu' }>;
		for (const e of tool.items) expect(e.kind !== 'separator' && e.disabled, 'tool').toBeFalsy();
	});

	test('and nothing is held for the preview when there is none', () => {
		for (const m of buildMenus(state())) {
			for (const e of flatten(m.items)) {
				if (e.kind !== 'separator' && 'reason' in e) expect(e.reason, `${m.label}: ${entryLabel(e)}`).not.toBe('Exit the proposal preview to edit');
			}
		}
	});

	test('ripple mode and snapping are ticks that follow the state', () => {
		for (const on of [true, false]) {
			const m = menu('edit', { rippleMode: on, snap: !on });
			const ripple = find(m.items, 'Ripple mode');
			const snap = find(m.items, 'Snapping');
			expect(ripple.kind === 'action' && ripple.role).toBe('check');
			expect(ripple.kind === 'action' && ripple.checked).toBe(on);
			expect(snap.kind === 'action' && snap.checked).toBe(!on);
		}
	});

	test('the tool submenu is one radio group over the five tools, the current one ticked', () => {
		const sub = find(menu('edit', { tool: 'razor' }).items, 'Tool');
		expect(sub.kind).toBe('submenu');
		const items = (sub as Extract<MenuEntry, { kind: 'submenu' }>).items as Extract<MenuEntry, { kind: 'action' }>[];
		expect(items.map((i) => i.id)).toEqual(['tool.pointer', 'tool.razor', 'tool.roll', 'tool.slip', 'tool.slide']);
		expect(items.every((i) => i.role === 'radio')).toBe(true);
		expect(items.filter((i) => i.checked).map((i) => i.id)).toEqual(['tool.razor']);
	});

	test('the linked-A/V and trim edits are reachable from the Clip submenu', () => {
		const ids = flatten((find(menu('edit').items, 'Clip') as Extract<MenuEntry, { kind: 'submenu' }>).items).flatMap((e) =>
			e.kind === 'action' ? [e.id] : []
		);
		expect(ids).toEqual(['edit.trimStart', 'edit.trimEnd', 'edit.detachAudio', 'edit.reattachAudio', 'edit.link', 'edit.unlink']);
	});
});

describe('View', () => {
	test('zoom, track heights, the overview strip and the safe-area guides', () => {
		const names = labels(menu('view').items);
		expect(names).toEqual(expect.arrayContaining(['Zoom in', 'Zoom out', 'Zoom to fit', 'Track height', 'Overview strip', 'Safe-area guides']));
	});

	test('track height is a radio group, lit when every track agrees', () => {
		const sub = find(menu('view', { allHeight: 'large' }).items, 'Track height') as Extract<MenuEntry, { kind: 'submenu' }>;
		const items = sub.items as Extract<MenuEntry, { kind: 'command' }>[];
		expect(items.map((i) => i.label)).toEqual(['Compact', 'Medium', 'Large']);
		expect(items.filter((i) => i.checked).map((i) => i.label)).toEqual(['Large']);
		const mixed = find(menu('view', { allHeight: null }).items, 'Track height') as Extract<MenuEntry, { kind: 'submenu' }>;
		expect((mixed.items as Extract<MenuEntry, { kind: 'command' }>[]).some((i) => i.checked)).toBe(false);
	});

	test('the delivery frame submenu is Source, 16:9, 9:16, 1:1, 4:5 with the project’s ticked', () => {
		const sub = find(menu('view', { delivery: 'vertical' }).items, 'Delivery frame') as Extract<MenuEntry, { kind: 'submenu' }>;
		const items = sub.items as Extract<MenuEntry, { kind: 'command' }>[];
		expect(items.map((i) => i.label)).toEqual(['Source shape', '16:9', '9:16', '1:1', '4:5']);
		expect(items.map((i) => (i.command.type === 'delivery' ? i.command.preset : null))).toEqual(DELIVERY_PRESETS.map((p) => p.id));
		expect(items.filter((i) => i.checked).map((i) => i.label)).toEqual(['9:16']);
	});

	test('the safe-area guides are a tick that needs a delivery frame', () => {
		const on = find(menu('view', { safeAreas: true }).items, 'Safe-area guides');
		expect(on.kind === 'action' && on.checked).toBe(true);
		const noFrame = find(menu('view', { hasFrame: false }).items, 'Safe-area guides');
		expect('disabled' in noFrame && noFrame.disabled).toBe(true);
		expect('reason' in noFrame && noFrame.reason).toContain('delivery frame');
	});

	test('workspaces are a radio group over every workspace, bound to the workspace actions', () => {
		const sub = find(menu('view', { workspace: 'audio' }).items, 'Workspace') as Extract<MenuEntry, { kind: 'submenu' }>;
		const items = sub.items as Extract<MenuEntry, { kind: 'action' }>[];
		expect(items.map((i) => i.id as string)).toEqual(WORKSPACE_IDS.map((w) => `workspace.${w}`));
		expect(items.filter((i) => i.checked).map((i) => i.id)).toEqual(['workspace.audio']);
	});
});

describe('Playback', () => {
	const ids = (over: Partial<MenuState> = {}) =>
		flatten(menu('playback', over).items).flatMap((e) => (e.kind === 'action' ? [e.id] : []));

	test('keeps the whole transport reachable when the preview panel is closed', () => {
		// Play / pause, to start / end, frame and second steps, J / K / L, marks, markers.
		expect(ids()).toEqual([
			'playback.toggle',
			'playback.toStart',
			'playback.toEnd',
			'playback.goToTimecode',
			'playback.stepBack',
			'playback.stepForward',
			'playback.jumpBack',
			'playback.jumpForward',
			'playback.shuttleBack',
			'playback.pause',
			'playback.shuttleForward',
			'range.markIn',
			'range.markOut',
			'range.clearIn',
			'range.clearOut',
			'marker.add',
			'marker.prev',
			'marker.next'
		]);
	});

	test('every playback, range and marker action in the registry has an entry', () => {
		const have = new Set(ids());
		for (const a of ACTIONS.filter((a) => /^(playback|range|marker)\./.test(a.id))) expect(have.has(a.id as never), a.id).toBe(true);
	});

	test('a menu bar is text-first: no entry carries an icon, so the labels line up in one column', () => {
		const all = buildMenus(state()).flatMap((m) => flatten(m.items));
		expect(all.length).toBeGreaterThan(50);
		expect(all.filter((e) => 'icon' in e)).toEqual([]);
	});

	test('play / pause needs a cut, and says so', () => {
		expect((find(menu('playback').items, 'Play / pause') as { disabled?: boolean }).disabled).toBe(false);
		const empty = find(menu('playback', { hasClips: false }).items, 'Play / pause');
		expect('disabled' in empty && empty.disabled).toBe(true);
		expect('reason' in empty && empty.reason).toBeTruthy();
	});
});

describe('Window', () => {
	test('every panel is a tick that follows whether it is open', () => {
		const m = menu('window', { openPanels: ['preview', 'timeline', 'mixer'] });
		const panels = m.items.filter((e) => e.kind === 'command') as Extract<MenuEntry, { kind: 'command' }>[];
		expect(panels.map((p) => (p.command.type === 'panel' ? p.command.panel : null))).toEqual([...PANEL_IDS]);
		expect(panels.every((p) => p.role === 'check')).toBe(true);
		expect(panels.filter((p) => p.checked).map((p) => p.label)).toEqual(['Preview', 'Timeline', 'Mixer']);
	});

	test('panels can go to windows of their own: one entry each, ticked while it is in one', () => {
		const m = menu('window', { openPanels: ['preview', 'timeline', 'inspector'], detachedPanels: ['inspector'] });
		const sub = m.items.find((e) => e.kind === 'submenu' && e.label === 'Panel windows') as Extract<MenuEntry, { kind: 'submenu' }>;
		expect(sub).toBeTruthy();
		const entries = sub.items.filter((e) => e.kind === 'command') as Extract<MenuEntry, { kind: 'command' }>[];
		expect(entries.map((e) => (e.command.type === 'detach' ? e.command.panel : null))).toEqual([...PANEL_IDS]);
		expect(entries.every((e) => e.role === 'check')).toBe(true);
		expect(entries.filter((e) => e.checked).map((e) => e.label)).toEqual(['Inspector']);
		// A panel that is not open has nothing to move, and says so.
		const closed = entries.find((e) => e.label === 'Mixer')!;
		expect(closed.disabled).toBe(true);
		expect(closed.reason).toBeTruthy();
		expect(entries.find((e) => e.label === 'Timeline')!.disabled).toBeFalsy();
	});

	test('the last panel in the editor window cannot go, and the entry says why', () => {
		const entries = (openPanels: PanelId[], detachedPanels: PanelId[]) => {
			const sub = menu('window', { openPanels, detachedPanels }).items.find((e) => e.kind === 'submenu' && e.label === 'Panel windows') as Extract<MenuEntry, { kind: 'submenu' }>;
			return sub.items.filter((e) => e.kind === 'command') as Extract<MenuEntry, { kind: 'command' }>[];
		};
		const named = (list: ReturnType<typeof entries>, title: string) => list.find((e) => e.label === title)!;
		// Two in the editor window: either can go.
		let list = entries(['preview', 'timeline'], []);
		expect(named(list, 'Preview').disabled).toBeFalsy();
		expect(named(list, 'Timeline').disabled).toBeFalsy();
		// One has gone: the other is the last, and is greyed out with the reason.
		list = entries(['preview', 'timeline'], ['preview']);
		expect(named(list, 'Timeline').disabled).toBe(true);
		expect(named(list, 'Timeline').reason).toBe('The editor window keeps at least one panel');
		// The one in a window can always come back.
		expect(named(list, 'Preview').disabled).toBeFalsy();
		expect(named(list, 'Preview').checked).toBe(true);
		// A closed panel says to open it.
		expect(named(list, 'Mixer').reason).toContain('open it from the Window menu');
	});

	test('giving them all back is offered only while some are out', () => {
		const dockAll = (detachedPanels: PanelId[]) => {
			const sub = menu('window', { detachedPanels }).items.find((e) => e.kind === 'submenu') as Extract<MenuEntry, { kind: 'submenu' }>;
			return sub.items.find((e) => e.kind === 'action' && e.id === 'window.dockAll') as Extract<MenuEntry, { kind: 'action' }>;
		};
		expect(dockAll([]).disabled).toBe(true);
		expect(dockAll([]).reason).toBeTruthy();
		expect(dockAll(['preview']).disabled).toBeFalsy();
	});

	test('resetting names the workspace on screen, and every workspace can be reset at once', () => {
		expect(labels(menu('window', { workspace: 'color' }).items)).toEqual(
			expect.arrayContaining(['Reset Color workspace', 'Reset all workspaces…'])
		);
		expect(labels(menu('window', { workspace: 'deliver' }).items)).toContain('Reset Deliver workspace');
	});
});

describe('Help', () => {
	test('shortcuts, updates, the release page, the logs and the version', () => {
		const ids = menu('help').items.flatMap((e) => (e.kind === 'action' ? [e.id] : []));
		expect(ids).toEqual(['app.shortcuts', 'app.keyboard', 'app.checkUpdate', 'app.releases', 'app.logs', 'app.about']);
	});

	test('the log folder is a desktop thing, and says so elsewhere', () => {
		const logs = find(menu('help', { desktop: false }).items, 'Show log folder');
		expect('disabled' in logs && logs.disabled).toBe(true);
	});
});

describe('a bar too narrow for its titles', () => {
	test('collapses to one "Menu" whose entries are the menus', () => {
		const full = buildMenus(state());
		const compact = collapseMenus(full);
		expect(compact).toHaveLength(1);
		expect(compact[0].label).toBe('Menu');
		expect(labels(compact[0].items)).toEqual(['File', 'Edit', 'View', 'Playback', 'Window', 'Help']);
		const subs = compact[0].items as Extract<MenuEntry, { kind: 'submenu' }>[];
		expect(subs.map((s) => s.items)).toEqual(full.map((m) => m.items));
	});
});

// Nothing may become unreachable when a toolbar row is taken apart: each control
// that lived in it has a home in the panel it acts on, and a path through the menus.
describe('where the old toolbar went', () => {
	const read = (rel: string) => readFileSync(join(import.meta.dir, rel), 'utf8');
	const preview = read('./components/editor/Preview.svelte');
	const timeline = read('./components/editor/Timeline.svelte');

	test('the transport is in the preview', () => {
		for (const id of ['playback.toStart', 'playback.toggle', 'playback.toEnd']) expect(preview).toContain(`'${id}'`);
		expect(preview).toContain('editor.fps');
	});

	test('the tools, ripple, snapping, undo / redo and the delivery frame are in the timeline', () => {
		for (const id of ['tool.pointer', 'tool.razor', 'tool.roll', 'tool.slip', 'tool.slide', 'tool.rippleMode', 'tool.snap', 'edit.undo', 'edit.redo']) {
			expect(timeline).toContain(`'${id}'`);
		}
		expect(timeline).toContain('pickDelivery');
	});

	test('and each of them is also a menu entry', () => {
		const ids = new Set(
			buildMenus(state()).flatMap((m) => flatten(m.items).flatMap((e) => (e.kind === 'action' ? [e.id] : [])))
		);
		for (const id of [
			'file.new',
			'file.open',
			'file.save',
			'file.export',
			'edit.undo',
			'edit.redo',
			'tool.pointer',
			'tool.razor',
			'tool.roll',
			'tool.slip',
			'tool.slide',
			'tool.rippleMode',
			'tool.snap',
			'window.resetWorkspace',
			'playback.toggle',
			'playback.toStart',
			'playback.toEnd'
		]) {
			expect(ids.has(id as never), id).toBe(true);
		}
	});

	test('the toolbar row is gone', () => {
		expect(() => readFileSync(join(import.meta.dir, './components/editor/Toolbar.svelte'))).toThrow();
		expect(read('../routes/+page.svelte')).not.toContain('Toolbar');
	});
});

test('the registry has an action for every action entry the menus name, with a label', () => {
	const named = new Set(
		buildMenus(state()).flatMap((m) => flatten(m.items).flatMap((e) => (e.kind === 'action' ? [e.id] : [])))
	);
	for (const id of named) expect(ACTIONS.some((a) => a.id === id && a.label.length > 0), id).toBe(true);
});

describe('moving about a menu', () => {
	const items = menu('edit').items;
	const at = (label: string) => items.findIndex((e) => e.kind !== 'separator' && entryLabel(e) === label);

	test('separators take no focus, disabled entries do', () => {
		const idx = focusable(menu('edit', { canUndo: false }).items);
		expect(idx.every((i) => menu('edit').items[i].kind !== 'separator')).toBe(true);
		expect(idx).toContain(0);
	});

	test('arrows step along the entries and skip separators', () => {
		expect(stepFocus(items, at('Redo'), 1)).toBe(at('Cut'));
		expect(stepFocus(items, at('Cut'), -1)).toBe(at('Redo'));
	});

	test('arrows wrap at both ends', () => {
		const last = focusable(items).at(-1)!;
		expect(stepFocus(items, last, 1)).toBe(0);
		expect(stepFocus(items, 0, -1)).toBe(last);
	});

	test('from no entry, down goes to the first and up to the last', () => {
		const sepAt = items.findIndex((e) => e.kind === 'separator');
		expect(stepFocus(items, sepAt, 1)).toBe(0);
		expect(stepFocus(items, sepAt, -1)).toBe(focusable(items).at(-1)!);
		expect(stepFocus([], 3, 1)).toBe(3);
	});

	test('a letter jumps to the next entry that starts with it, then round again', () => {
		const first = typeahead(items, 0, 'c')!;
		expect(entryLabel(items[first] as never)).toBe('Cut');
		const second = typeahead(items, first, 'c')!;
		expect(entryLabel(items[second] as never)).toBe('Copy');
		const third = typeahead(items, second, 'c')!;
		expect(entryLabel(items[third] as never)).toBe('Clip');
		const fourth = typeahead(items, third, 'c')!;
		expect(entryLabel(items[fourth] as never)).toBe('Customize keyboard shortcuts…');
		expect(typeahead(items, fourth, 'c')).toBe(first);
		expect(typeahead(items, 0, 'C')).toBe(first);
		expect(typeahead(items, 0, 'q')).toBeNull();
	});
});

describe('File › Open Recent', () => {
	const recent = (over: Partial<MenuState> = {}) => find(menu('file', over).items, 'Open Recent') as Extract<MenuEntry, { kind: 'submenu' }>;

	test('lists the projects newest first, each by its file name, then clears', () => {
		const sub = recent({ recent: ['/a/summer.kerf', '/b/interview.kerf'] });
		expect(sub.kind).toBe('submenu');
		expect(labels(sub.items)).toEqual(['summer.kerf', 'interview.kerf', 'Clear recent projects…']);
		const first = sub.items[0];
		expect(first.kind === 'command' && first.command).toEqual({ type: 'recent', path: '/a/summer.kerf' });
		const last = sub.items[sub.items.length - 1];
		expect(last.kind === 'command' && last.command).toEqual({ type: 'recent-clear' });
	});

	test('two projects with one name are told apart by their folder', () => {
		const sub = recent({ recent: ['/work/a/cut.kerf', '/work/b/cut.kerf', '/c/other.kerf'] });
		expect(labels(sub.items).slice(0, 3)).toEqual(['cut.kerf — a', 'cut.kerf — b', 'other.kerf']);
	});

	test('with none it is one greyed entry that says where they come from', () => {
		const sub = recent({ recent: [] });
		expect(sub.items).toHaveLength(1);
		const only = sub.items[0];
		expect(only.kind === 'command' && only.disabled).toBe(true);
		expect(only.kind === 'command' && only.reason).toContain('listed here');
	});

	test('stays listed in a browser (an open there says it is a desktop thing)', () => {
		const sub = recent({ desktop: false, recent: ['/a.kerf'] });
		expect(sub.disabled).toBeUndefined();
		expect(labels(sub.items)[0]).toBe('a.kerf');
	});
});
