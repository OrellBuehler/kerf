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
import { focusableRows, stepRow, typeaheadRow, type NavRow } from './menu-nav';
import { DELIVERY_PRESETS, fitLabel } from './delivery-formats';
import { exportButtonState } from './export-button';
import { PANEL_IDS, PANELS, type PanelId } from './layout';
import { detachBlocked, type PanelLocation } from './popouts';
import { HEIGHT_PRESETS, PRESET_LABEL, PRESET_PX, type HeightPreset } from './track-heights';
import { WORKSPACE_SPECS, workspaceSpec, type WorkspaceId } from './workspaces';

export type MenuId = 'file' | 'edit' | 'view' | 'playback' | 'window' | 'help' | 'menu';

/** What a menu choice that is not a registry action does. */
export type MenuCommand =
	| { type: 'delivery'; preset: string }
	| { type: 'height'; preset: HeightPreset }
	| { type: 'panel'; panel: PanelId }
	/** Move a panel into a window of its own, or give it back. */
	| { type: 'detach'; panel: PanelId };

/** How an entry is announced and drawn: a plain item, a tickable one, or one of a
 *  group of which exactly one is ticked. */
export type MenuRole = 'item' | 'check' | 'radio';

interface Common {
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
	/** The cut has a marker (the marker exports need one). */
	hasMarkers?: boolean;
	/** A render is under way: File › Export… then reopens its dialog (the title bar's button does too). */
	exporting: boolean;
	hasSelection: boolean;
	rippleMode: boolean;
	snap: boolean;
	minimap: boolean;
	safeAreas: boolean;
	/** The project is cut for a delivery frame (the safe-area guides need one). */
	hasFrame: boolean;
	tool: string;
	/** The id of the delivery preset the project is cut for. */
	delivery: string;
	/** The height every track shares, or null when they differ. */
	allHeight: HeightPreset | null;
	workspace: WorkspaceId;
	openPanels: readonly PanelId[];
	/** The panels that are in a window of their own. */
	detachedPanels: readonly PanelId[];
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
const NEEDS_MARKERS = 'The cut has no markers';
const DESKTOP_ONLY = 'Available in the desktop app';

export function buildMenus(s: MenuState): MenuDef[] {
	const sel = { disabled: !s.hasSelection, reason: s.hasSelection ? undefined : NEEDS_SELECTION };
	const clips = { disabled: !s.hasClips, reason: s.hasClips ? undefined : NEEDS_CLIPS };
	const markers = { disabled: !s.hasMarkers, reason: s.hasMarkers ? undefined : NEEDS_MARKERS };
	const desk = { disabled: !s.desktop, reason: s.desktop ? undefined : DESKTOP_ONLY };
	// The one rule the title bar's Export button and the shortcut use too (`export-button.ts`).
	const exportOff = exportButtonState({ clips: s.hasClips ? 1 : 0, running: s.exporting });

	const file: MenuDef = {
		id: 'file',
		label: 'File',
		items: [
			act('file.new'),
			act('file.open'),
			sep,
			act('file.save', { label: s.saved ? 'Save project as…' : 'Save project…' }),
			sep,
			act('file.import'),
			act('file.importCaptions'),
			sep,
			act('file.export', { disabled: exportOff.disabled, reason: exportOff.reason }),
			act('file.saveCover', { ...clips }),
			{
				kind: 'submenu',
				label: 'Export as',
				items: [
					act('file.exportCaptions', { ...clips }),
					act('file.exportOtio', { ...clips }),
					act('file.exportEdl', { ...clips }),
					sep,
					act('file.exportMarkers', { ...markers }),
					act('file.exportChapters', { ...markers })
				]
			},
			sep,
			act('app.settings'),
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
			act('edit.undo', { disabled: !s.canUndo }),
			act('edit.redo', { disabled: !s.canRedo }),
			sep,
			act('edit.cut', { ...sel }),
			act('edit.copy', { ...sel }),
			act('edit.paste'),
			act('edit.duplicate', { ...sel }),
			sep,
			act('edit.delete', { ...sel }),
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
			act('app.keyboard')
		]
	};

	const view: MenuDef = {
		id: 'view',
		label: 'View',
		items: [
			act('view.zoomIn'),
			act('view.zoomOut'),
			act('view.zoomFit', { ...clips }),
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
			act('playback.toggle', { ...clips }),
			sep,
			act('playback.toStart'),
			act('playback.toEnd'),
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
			act('marker.add'),
			act('marker.prev'),
			act('marker.next')
		]
	};

	const label = workspaceSpec(s.workspace).label;
	// Where each open panel is, for the rule that the editor window keeps one.
	const where: Partial<Record<PanelId, PanelLocation>> = {};
	for (const id of s.openPanels) where[id] = s.detachedPanels.includes(id) ? 'popout' : 'grid';
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
			{
				kind: 'submenu',
				label: 'Panel windows',
				items: [
					...PANEL_IDS.map((id): MenuEntry => {
						const blocked = detachBlocked(id, where);
						return {
							kind: 'command',
							command: { type: 'detach', panel: id },
							label: PANELS[id].title,
							role: 'check',
							checked: s.detachedPanels.includes(id),
							...(blocked ? { disabled: true, reason: blocked } : {})
						};
					}),
					sep,
					act('window.dockAll', {
						...(s.detachedPanels.length === 0 ? { disabled: true, reason: 'No panel is in a window of its own' } : {})
					})
				]
			},
			sep,
			act('window.resetWorkspace', { label: `Reset ${label} workspace` }),
			act('window.resetAllWorkspaces')
		]
	};

	const help: MenuDef = {
		id: 'help',
		label: 'Help',
		items: [
			act('app.keyboard'),
			sep,
			act('app.checkUpdate'),
			act('app.releases'),
			act('app.logs', { ...desk }),
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
// The bar's keyboard model is `menu-nav.ts` (shared with the context menu), over this menu's
// entries: which can take focus, where an arrow goes, where a typed letter jumps.

const rowsOf = (items: readonly MenuEntry[]): NavRow[] =>
	items.map((e) => ({ focusable: e.kind !== 'separator', label: e.kind === 'separator' ? '' : entryLabel(e) }));

/** The entries that can take focus: everything but separators. A disabled entry can
 *  — it says why when asked — which is the menu pattern's rule. */
export function focusable(items: readonly MenuEntry[]): number[] {
	return focusableRows(rowsOf(items));
}

/** The entry `by` steps from `at` in `items`, wrapping at both ends; `at` when there is
 *  nothing to move to. */
export function stepFocus(items: readonly MenuEntry[], at: number, by: 1 | -1): number {
	return stepRow(rowsOf(items), at, by);
}

/** The next entry after `at` whose label starts with `ch` (case-insensitive), wrapping
 *  round to `at` itself last; null when none does. */
export function typeahead(items: readonly MenuEntry[], at: number, ch: string): number | null {
	return typeaheadRow(rowsOf(items), at, ch);
}
