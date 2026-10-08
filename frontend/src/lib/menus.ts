// The menu bar's content: File, Edit, View, Playback, Window and Help, as data.
//
// Every entry is one of three things. An *action* names an id in the keymap
// registry (`keymap.ts`) — the same id the page's key handler runs — so the menu
// can print the key the user has for it (`settings.shortcut(id)`) and running it
// is the shortcut's own code. A *command* is a menu choice that is one of a
// family (a delivery shape, a track height, a panel) and has no key of its own.
// A *submenu* nests more. Which entries are ticked or greyed out is a function of
// `MenuState`, which the component fills in from the singletons, so this file is
// pure and the menus can be held to the registry in a test.

import { actionDef, type ActionId } from './keymap';
import { DELIVERY_PRESETS, fitLabel } from './delivery-formats';
import { PANEL_IDS, PANELS, type PanelId } from './layout';
import { HEIGHT_PRESETS, PRESET_LABEL, PRESET_PX, type HeightPreset } from './track-heights';
import { WORKSPACE_SPECS, workspaceSpec, type WorkspaceId } from './workspaces';

export type MenuId = 'file' | 'edit' | 'view' | 'playback' | 'window' | 'help' | 'menu';

/** What a menu choice that is not a registry action does. */
export type MenuCommand =
	| { type: 'delivery'; preset: string }
	| { type: 'height'; preset: HeightPreset }
	| { type: 'panel'; panel: PanelId };

/** How an entry is announced and drawn: a plain item, a tickable one, or one of a
 *  group of which exactly one is ticked. */
export type MenuRole = 'item' | 'check' | 'radio';

interface Common {
	icon?: string;
	disabled?: boolean;
	/** Why it is disabled — an entry that cannot be used and does not say why is
	 *  a dead end. */
	reason?: string;
}

export type MenuEntry =
	| (Common & { kind: 'action'; id: ActionId; label?: string; role?: MenuRole; checked?: boolean })
	| (Common & { kind: 'command'; command: MenuCommand; label: string; role: MenuRole; checked?: boolean; hint?: string })
	| (Common & { kind: 'submenu'; label: string; items: MenuEntry[] })
	| { kind: 'separator' };

export interface MenuDef {
	id: MenuId;
	label: string;
	items: MenuEntry[];
}

/** Everything the menus' ticks and greyed-out entries depend on. */
export interface MenuState {
	/** The project has a file (so Save is "as…"); until then it is "Save…". */
	saved: boolean;
	canUndo: boolean;
	canRedo: boolean;
	/** The cut has a clip at all / a clip is selected. */
	hasClips: boolean;
	hasSelection: boolean;
	rippleMode: boolean;
	snap: boolean;
	minimap: boolean;
	safeAreas: boolean;
	/** The project is cut for a delivery frame (the safe-area guides need one). */
	hasFrame: boolean;
	tool: string;
	/** The cut is playing (the transport's play / pause button shows the other). */
	playing: boolean;
	/** The id of the delivery preset the project is cut for. */
	delivery: string;
	/** The height every track shares, or null when they differ. */
	allHeight: HeightPreset | null;
	workspace: WorkspaceId;
	openPanels: readonly PanelId[];
	/** The desktop app: some commands have nothing to act on in a browser. */
	desktop: boolean;
}

const sep: MenuEntry = { kind: 'separator' };
const act = (id: ActionId, extra: Partial<Omit<Extract<MenuEntry, { kind: 'action' }>, 'kind' | 'id'>> = {}): MenuEntry => ({
	kind: 'action',
	id,
	...extra
});

const NEEDS_SELECTION = 'Select a clip first';
const NEEDS_CLIPS = 'The cut has no clips';
const DESKTOP_ONLY = 'Available in the desktop app';

export function buildMenus(s: MenuState): MenuDef[] {
	const sel = { disabled: !s.hasSelection, reason: s.hasSelection ? undefined : NEEDS_SELECTION };
	const clips = { disabled: !s.hasClips, reason: s.hasClips ? undefined : NEEDS_CLIPS };
	const desk = { disabled: !s.desktop, reason: s.desktop ? undefined : DESKTOP_ONLY };

	const file: MenuDef = {
		id: 'file',
		label: 'File',
		items: [
			act('file.new', { icon: 'file-plus' }),
			act('file.open', { icon: 'folder-open' }),
			sep,
			act('file.save', { icon: 'save', label: s.saved ? 'Save project as…' : 'Save project…' }),
			sep,
			act('file.import', { icon: 'film' }),
			act('file.importCaptions', { icon: 'captions' }),
			sep,
			act('file.export', { icon: 'upload' }),
			act('file.saveCover', { icon: 'image', ...clips }),
			sep,
			act('app.settings', { icon: 'settings' }),
			sep,
			act('app.quit', { ...desk })
		]
	};

	const tools: [string, ActionId][] = [
		['pointer', 'tool.pointer'],
		['razor', 'tool.razor'],
		['roll', 'tool.roll'],
		['slip', 'tool.slip'],
		['slide', 'tool.slide']
	];
	const edit: MenuDef = {
		id: 'edit',
		label: 'Edit',
		items: [
			act('edit.undo', { icon: 'undo', disabled: !s.canUndo }),
			act('edit.redo', { icon: 'redo', disabled: !s.canRedo }),
			sep,
			act('edit.cut', { ...sel }),
			act('edit.copy', { icon: 'copy', ...sel }),
			act('edit.paste'),
			act('edit.duplicate', { ...sel }),
			sep,
			act('edit.delete', { icon: 'trash', ...sel }),
			act('edit.rippleDelete', { ...sel }),
			act('edit.selectAll', { ...clips }),
			sep,
			{
				kind: 'submenu',
				label: 'Tool',
				items: tools.map(([tool, id]) => act(id, { role: 'radio', checked: s.tool === tool }))
			},
			{
				kind: 'submenu',
				label: 'Clip',
				items: [
					act('edit.trimStart', { ...sel }),
					act('edit.trimEnd', { ...sel }),
					sep,
					act('edit.detachAudio', { ...sel }),
					act('edit.reattachAudio', { ...sel }),
					sep,
					act('edit.link', { ...sel }),
					act('edit.unlink', { ...sel })
				]
			},
			sep,
			act('tool.rippleMode', { role: 'check', checked: s.rippleMode, label: 'Ripple mode' }),
			act('tool.snap', { role: 'check', checked: s.snap, label: 'Snapping' }),
			sep,
			act('app.keyboard', { icon: 'keyboard', label: 'Keyboard shortcuts…' })
		]
	};

	const view: MenuDef = {
		id: 'view',
		label: 'View',
		items: [
			act('view.zoomIn', { icon: 'zoom-in' }),
			act('view.zoomOut', { icon: 'zoom-out' }),
			act('view.zoomFit', { icon: 'fold-horizontal', ...clips }),
			sep,
			{
				kind: 'submenu',
				label: 'Track height',
				items: HEIGHT_PRESETS.map(
					(p): MenuEntry => ({
						kind: 'command',
						command: { type: 'height', preset: p },
						label: PRESET_LABEL[p],
						hint: `${PRESET_PX[p]} px`,
						role: 'radio',
						checked: s.allHeight === p
					})
				)
			},
			act('view.minimap', { role: 'check', checked: s.minimap, label: 'Overview strip' }),
			act('view.safeAreas', {
				role: 'check',
				checked: s.safeAreas,
				label: 'Safe-area guides',
				disabled: !s.hasFrame,
				reason: s.hasFrame ? undefined : 'Pick a vertical or square delivery frame first'
			}),
			sep,
			{
				kind: 'submenu',
				label: 'Delivery frame',
				items: DELIVERY_PRESETS.map(
					(p): MenuEntry => ({
						kind: 'command',
						command: { type: 'delivery', preset: p.id },
						label: p.label === 'Source' ? 'Source shape' : p.label,
						hint: p.format ? `${p.format.width}×${p.format.height} — ${fitLabel(p.format.fit)}` : p.hint,
						role: 'radio',
						checked: s.delivery === p.id
					})
				)
			},
			{
				kind: 'submenu',
				label: 'Workspace',
				items: WORKSPACE_SPECS.map((w) =>
					act(`workspace.${w.id}` as ActionId, { role: 'radio', checked: s.workspace === w.id, label: w.label })
				)
			}
		]
	};

	// The transport lives in the preview panel, and a panel can be closed: the menu
	// keeps all of it — and the marks and markers its keys set — reachable.
	const playback: MenuDef = {
		id: 'playback',
		label: 'Playback',
		items: [
			act('playback.toggle', { icon: s.playing ? 'pause' : 'play', ...clips }),
			sep,
			act('playback.toStart', { icon: 'skip-back' }),
			act('playback.toEnd', { icon: 'skip-forward' }),
			sep,
			act('playback.stepBack'),
			act('playback.stepForward'),
			act('playback.jumpBack'),
			act('playback.jumpForward'),
			sep,
			act('playback.shuttleBack', { ...clips }),
			act('playback.pause'),
			act('playback.shuttleForward', { ...clips }),
			sep,
			act('range.markIn'),
			act('range.markOut'),
			act('range.clearIn'),
			act('range.clearOut'),
			sep,
			act('marker.add', { icon: 'bookmark' }),
			act('marker.prev'),
			act('marker.next')
		]
	};

	const label = workspaceSpec(s.workspace).label;
	const win: MenuDef = {
		id: 'window',
		label: 'Window',
		items: [
			...PANEL_IDS.map(
				(id): MenuEntry => ({
					kind: 'command',
					command: { type: 'panel', panel: id },
					label: PANELS[id].title,
					role: 'check',
					checked: s.openPanels.includes(id)
				})
			),
			sep,
			act('window.resetWorkspace', { icon: 'rotate-ccw', label: `Reset ${label} workspace` }),
			act('window.resetAllWorkspaces', { label: 'Reset all workspaces' })
		]
	};

	const help: MenuDef = {
		id: 'help',
		label: 'Help',
		items: [
			act('app.keyboard', { icon: 'keyboard', label: 'Keyboard shortcuts' }),
			sep,
			act('app.checkUpdate', { icon: 'refresh-cw' }),
			act('app.releases', { icon: 'external-link' }),
			act('app.logs', { icon: 'folder-open', ...desk }),
			sep,
			act('app.about')
		]
	};

	return [file, edit, view, playback, win, help];
}

/** The text of an action entry: its own label or the registry's. */
export function entryLabel(e: Extract<MenuEntry, { kind: 'action' | 'command' | 'submenu' }>): string {
	if (e.kind === 'action') return e.label ?? actionDef(e.id)?.label ?? e.id;
	return e.label;
}

/** Every entry of a menu, depth first — submenus opened. */
export function flatten(items: readonly MenuEntry[]): MenuEntry[] {
	return items.flatMap((e) => (e.kind === 'submenu' ? [e, ...flatten(e.items)] : [e]));
}

/** What a menu bar too narrow for its menus shows instead: one "Menu" whose
 *  entries are the menus themselves, each a submenu. */
export function collapseMenus(menus: readonly MenuDef[]): MenuDef[] {
	return [{ id: 'menu', label: 'Menu', items: menus.map((m): MenuEntry => ({ kind: 'submenu', label: m.label, items: m.items })) }];
}

// ---- moving about a menu -------------------------------------------------------
//
// The bar's keyboard model, as pure functions over a menu's entries so it can be
// tested without a DOM: which entries can take focus, where an arrow goes, where a
// typed letter jumps.

/** The entries that can take focus: everything but separators. A disabled entry can
 *  — it says why when asked — which is the menu pattern's rule. */
export function focusable(items: readonly MenuEntry[]): number[] {
	return items.flatMap((e, i) => (e.kind === 'separator' ? [] : [i]));
}

/** The entry `by` steps from `at` in `items`, wrapping at both ends; `at` when there is
 *  nothing to move to. */
export function stepFocus(items: readonly MenuEntry[], at: number, by: 1 | -1): number {
	const idx = focusable(items);
	if (idx.length === 0) return at;
	const pos = idx.indexOf(at);
	// From nowhere (a separator, or no entry), down goes to the first and up to the last.
	if (pos < 0) return by === 1 ? idx[0] : idx[idx.length - 1];
	return idx[(pos + by + idx.length) % idx.length];
}

/** The next entry after `at` whose label starts with `ch` (case-insensitive), wrapping
 *  round to `at` itself last; null when none does. */
export function typeahead(items: readonly MenuEntry[], at: number, ch: string): number | null {
	const idx = focusable(items);
	const order = [...idx.filter((i) => i > at), ...idx.filter((i) => i <= at)];
	const want = ch.toLowerCase();
	const hit = order.find((i) => {
		const e = items[i];
		return e.kind !== 'separator' && entryLabel(e).trim().toLowerCase().startsWith(want);
	});
	return hit ?? null;
}
