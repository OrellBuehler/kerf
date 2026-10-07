// App preferences (Svelte 5 runes).
//
// The settings that belong to the machine rather than to the cut: how much of
// the computer Kerf's media engine may take (`kerf_core::engine::cpu` enforces
// the budget), whether analysis transcribes, what the preview draws, how the
// workspace is arranged, what colors it is drawn in and which keys do what.
//
// The persisted values live on the Rust side (the platform config directory),
// so this holds only the resolved view and writes through `api.ts` — which
// keeps the browser harness working over localStorage.

import { exportThemeFile, getSettings, importThemeFile, setSettings } from './api';
import { toast } from './notifications.svelte';
import { ui } from './editor-ui.svelte';
import { applyTheme, parseTheme, PRESETS, presetIdFor, themeJson, clampShape, upgradeStoredTheme, type ColorToken, type PresetId, type ShapeToken, type ThumbStyle, type Theme } from './theme';
import { singleFlight } from './single-flight';
import {
	defaultWorkspaces,
	libraryTabFor,
	parseWorkspaces,
	withLibraryTab,
	type LibraryTab,
	type WorkspaceId,
	type WorkspacesState
} from './workspaces';
import {
	applyRebind,
	detectPlatform,
	displayChord,
	emptyOverrides,
	hasOverrides,
	matchAction,
	parseKeyOverrides,
	rebindConflicts,
	resetAction,
	resetAll,
	resolveBindings,
	sameChord,
	serializeOverrides,
	setActionChords,
	type ActionDef,
	type ActionId,
	type Chord,
	type KeyEventLike,
	type KeyOverrides,
	type Rebind,
	type Resolution
} from './keymap';
import type { AppSettings, SettingsView } from './types';

/** The named budgets. The slider still offers everything in between; these are
 *  the three answers people actually have to "how much of my computer?". */
export const CPU_PRESETS = [
	{
		id: 'background',
		label: 'Background',
		percent: 25,
		hint: 'Kerf keeps out of the way. Renders take longer; nothing else slows down.'
	},
	{
		id: 'balanced',
		label: 'Balanced',
		percent: 75,
		hint: 'Most of the machine for Kerf, enough left over to keep working beside it.'
	},
	{
		id: 'full',
		label: 'Full speed',
		percent: 100,
		hint: 'Every core, normal priority. Fastest renders — expect the rest of the system to crawl.'
	}
] as const;

class SettingsStore {
	open = $state(false);
	loaded = $state(false);
	saving = $state(false);

	cpuPercent = $state(75);
	/** What the backend last confirmed. `cpuPercent` runs ahead of it while a
	 *  slider is being dragged, so "nothing changed" is judged against this. */
	private savedPercent = 75;
	transcribe = $state(true);
	/** Shade the delivery safe areas over the preview. Only visible while the
	 *  project is cut for a vertical or square frame; a 16:9 web export has no
	 *  chrome to stay clear of. */
	safeAreas = $state(false);
	cpuCores = $state(1);
	cpuThreads = $state(1);
	cpuMinPercent = $state(10);
	/** The workspaces — which is active, how each is arranged, the library
	 *  rail's tabs and collapsed state. Read once when the dock is built; from
	 *  then on this is the live copy (a view coming back from a write never
	 *  replaces it) and every change is written through, newest wins. Replaced,
	 *  never mutated, so the dockview JSON inside is not proxied. */
	workspaces = $state.raw<WorkspacesState>(defaultWorkspaces());
	private workspacesRead = false;
	/** Writes `workspaces` one at a time, reading it as each starts. */
	private writeWorkspaces = singleFlight(async () => {
		await this.write({ workspaces: this.workspaces }, 'workspace layout');
	});
	/** What the user changed about the keyboard — only that; an action they never
	 *  touched follows the defaults of whatever build is running. Like
	 *  `workspaces` it is read once and is the live copy from then on, every
	 *  change written through (newest wins), replaced and never mutated. */
	keyOverrides = $state.raw<KeyOverrides>(emptyOverrides());
	private keysRead = false;
	private writeKeys = singleFlight(async () => {
		await this.write({ keybindings: serializeOverrides(this.keyOverrides) }, 'keyboard shortcuts');
	});
	/** ⌘ and ⇧ are what a Mac writes; Ctrl and Shift are what everywhere else does. */
	readonly platform = detectPlatform();
	/** Every action's chords in force: the defaults with the user's changes on top. */
	bindings = $derived(resolveBindings(this.keyOverrides, this.platform));
	theme = $state<Theme>(PRESETS['kerf-dark']);
	/** Color edits apply at once and are written a moment later; while one is
	 *  pending, a view coming back from another write must not overwrite the
	 *  newer colors on screen. */
	private themeDirty = false;
	private themeTimer: ReturnType<typeof setTimeout> | null = null;

	/** The preset the current percentage *is*, or null when it sits between them. */
	get cpuPreset() {
		return CPU_PRESETS.find((p) => p.percent === this.cpuPercent) ?? null;
	}

	/** The preset the current theme *is*, or `custom`. */
	get themePreset(): PresetId | 'custom' {
		return presetIdFor(this.theme);
	}

	private absorb(view: SettingsView) {
		this.cpuPercent = view.cpu_percent;
		this.savedPercent = view.cpu_percent;
		this.transcribe = view.transcribe;
		this.safeAreas = view.safe_areas;
		this.cpuCores = view.cpu_cores;
		this.cpuThreads = view.cpu_threads;
		this.cpuMinPercent = view.cpu_min_percent;
		if (!this.workspacesRead) {
			this.workspaces = parseWorkspaces(view.workspaces, view.layout);
			this.workspacesRead = true;
		}
		if (!this.keysRead) {
			this.keyOverrides = parseKeyOverrides(view.keybindings, this.platform);
			this.keysRead = true;
		}
		if (!this.themeDirty) {
			const stored = parseTheme(view.theme);
			this.theme = stored ? upgradeStoredTheme(stored) : PRESETS['kerf-dark'];
			applyTheme(this.theme);
		}
		this.loaded = true;
	}

	async load() {
		try {
			this.absorb(await getSettings());
		} catch (e) {
			// Not worth a toast at launch: the dialog just shows the defaults.
			console.error('could not read settings', e);
		} finally {
			this.workspacesRead = true;
			this.keysRead = true;
			this.loaded = true;
		}
	}

	private async write(patch: Partial<AppSettings>, what: string): Promise<boolean> {
		this.saving = true;
		try {
			this.absorb(await setSettings(patch));
			return true;
		} catch (e) {
			toast.error(`Could not save the ${what}`, { description: String(e) });
			await this.load();
			return false;
		} finally {
			this.saving = false;
		}
	}

	/** Write the CPU budget through. The engine clamps, so the view that comes
	 *  back — not the value asked for — is what gets shown. */
	async setCpuPercent(percent: number) {
		const want = Math.round(percent);
		if (want === this.savedPercent) return;
		this.cpuPercent = want; // optimistic: the slider must not lag the drag
		await this.write({ cpu_percent: want }, 'CPU limit');
	}

	/** Turn speech-to-text in the analysis pass on or off. */
	async setTranscribe(on: boolean) {
		if (on === this.transcribe) return;
		this.transcribe = on;
		if (await this.write({ transcribe: on }, 'transcription setting')) await ui.loadTranscriptionStatus();
	}

	/** Show or hide the safe-area guides over the preview. */
	async setSafeAreas(on: boolean) {
		if (on === this.safeAreas) return;
		this.safeAreas = on;
		await this.write({ safe_areas: on }, 'safe-area setting');
	}

	/** The tab the library shows in the active workspace. Each workspace keeps
	 *  its own, so picking Transcript while editing does not follow you to Color. */
	get libraryTab(): LibraryTab {
		return libraryTabFor(this.workspaces, this.workspaces.active);
	}

	get libraryCollapsed(): boolean {
		return this.workspaces.library.collapsed;
	}

	/** Change the live workspaces and write them through (see `singleFlight`: a
	 *  dock save, a rail click and a switch can overlap). */
	private changeWorkspaces(next: WorkspacesState) {
		this.workspaces = next;
		this.writeWorkspaces.request();
	}

	/** Make `id` the workspace the app opens on. */
	setActiveWorkspace(id: WorkspaceId) {
		if (id === this.workspaces.active) return;
		this.changeWorkspaces({ ...this.workspaces, active: id });
	}

	/** Remember how `id` is arranged now. */
	saveWorkspaceLayout(id: WorkspaceId, layout: unknown) {
		this.changeWorkspaces({
			...this.workspaces,
			layouts: { ...this.workspaces.layouts, [id]: layout as WorkspacesState['layouts'][WorkspaceId] }
		});
	}

	/** Forget how `id` was arranged, so it is its preset again. */
	clearWorkspaceLayout(id: WorkspaceId) {
		if (!(id in this.workspaces.layouts)) return;
		const layouts = { ...this.workspaces.layouts };
		delete layouts[id];
		this.changeWorkspaces({ ...this.workspaces, layouts });
	}

	/** Pick the library's tab for the active workspace. */
	setLibraryTab(tab: LibraryTab) {
		if (tab === this.libraryTab) return;
		this.changeWorkspaces(withLibraryTab(this.workspaces, this.workspaces.active, tab));
	}

	setLibraryCollapsed(collapsed: boolean) {
		if (collapsed === this.workspaces.library.collapsed) return;
		this.changeWorkspaces({ ...this.workspaces, library: { ...this.workspaces.library, collapsed } });
	}

	// ---- keyboard ---------------------------------------------------------------

	/** The action a keypress runs, or null. */
	actionFor(e: KeyEventLike): ActionId | null {
		return matchAction(e, this.bindings) as ActionId | null;
	}

	/** What an action is bound to, as menus print it (`⌘Z`, `Ctrl+Z`), in order. */
	keysFor(id: ActionId | string): string[] {
		return (this.bindings[id] ?? []).map((c) => displayChord(c, this.platform));
	}

	/** The first chord, for a menu item or a tooltip — empty when unbound, so
	 *  the hint disappears with the key. */
	shortcut(id: ActionId): string {
		return this.keysFor(id)[0] ?? '';
	}

	/** `Razor (C)`, or `Razor` when nothing is bound to it. */
	withShortcut(label: string, id: ActionId): string {
		const k = this.shortcut(id);
		return k ? `${label} (${k})` : label;
	}

	/** The other actions that already use this chord where `rebind` would put it. */
	conflictsFor(rebind: Rebind): ActionDef[] {
		return rebindConflicts(this.bindings, rebind);
	}

	get hasKeyOverrides(): boolean {
		return hasOverrides(this.keyOverrides);
	}

	/** Whether an action has been changed from its defaults. */
	isCustomKey(id: string): boolean {
		return id in this.keyOverrides.bindings;
	}

	/** Give an action a chord — in place of `rebind.replacing`, or beside its
	 *  others — settling any collision the way `resolution` says. With a collision
	 *  and no resolution nothing changes (the dialog asks first). */
	rebind(rebind: Rebind, resolution: Resolution | null = null) {
		this.changeKeys(applyRebind(this.keyOverrides, this.bindings, rebind, resolution, this.platform));
	}

	/** Take one chord off an action (it may end up unbound). */
	removeChord(id: string, chord: Chord) {
		const mine = (this.bindings[id] ?? []).filter((c) => !sameChord(c, chord));
		this.changeKeys(setActionChords(this.keyOverrides, id, mine, this.platform));
	}

	resetKey(id: string) {
		this.changeKeys(resetAction(this.keyOverrides, id));
	}

	resetAllKeys() {
		this.changeKeys(resetAll(this.keyOverrides));
	}

	private changeKeys(next: KeyOverrides) {
		if (next === this.keyOverrides) return;
		this.keyOverrides = next;
		this.writeKeys.request();
	}

	/** Put a theme into force now and save it shortly — a color picker fires
	 *  on every pixel of a drag. */
	setTheme(theme: Theme) {
		this.theme = theme;
		this.themeDirty = true;
		applyTheme(theme);
		if (this.themeTimer) clearTimeout(this.themeTimer);
		this.themeTimer = setTimeout(() => {
			this.themeTimer = null;
			const sent = this.theme;
			void this.write({ theme: sent }, 'theme').then(() => {
				if (this.theme === sent) this.themeDirty = false;
			});
		}, 300);
	}

	applyPreset(id: PresetId) {
		this.setTheme(PRESETS[id]);
	}

	/** Change one color; the result is a custom theme derived from whatever
	 *  was on screen. */
	setColor(token: ColorToken, hex: string) {
		if (this.theme.colors[token] === hex) return;
		const custom = this.themePreset !== 'custom';
		this.setTheme({
			...this.theme,
			name: custom ? 'Custom' : this.theme.name,
			colors: { ...this.theme.colors, [token]: hex }
		});
	}

	/** Change one shape value (clamped to its range); custom like a color. */
	setShape(token: ShapeToken, value: number) {
		const v = clampShape(token, value);
		if (this.theme.shape[token] === v) return;
		this.editShape({ ...this.theme.shape, [token]: v });
	}

	setThumbStyle(style: ThumbStyle) {
		if (this.theme.shape['slider-thumb-style'] === style) return;
		this.editShape({ ...this.theme.shape, 'slider-thumb-style': style });
	}

	private editShape(shape: Theme['shape']) {
		const custom = this.themePreset !== 'custom';
		this.setTheme({ ...this.theme, name: custom ? 'Custom' : this.theme.name, shape });
	}

	setThemeName(name: string) {
		const trimmed = name.trim();
		if (!trimmed || trimmed === this.theme.name) return;
		this.setTheme({ ...this.theme, name: trimmed });
	}

	setScheme(scheme: Theme['scheme']) {
		if (scheme === this.theme.scheme) return;
		this.setTheme({ ...this.theme, scheme });
	}

	async importTheme() {
		const text = await importThemeFile();
		if (text == null) return;
		let raw: unknown = null;
		try {
			raw = JSON.parse(text);
		} catch {
			raw = null;
		}
		const theme = parseTheme(raw);
		if (!theme) {
			toast.error('Not a Kerf theme file', { description: 'Expected the JSON a theme export writes.' });
			return;
		}
		this.setTheme(theme);
		toast.success(`Theme "${theme.name}" applied`);
	}

	async exportTheme() {
		const slug = this.theme.name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'theme';
		try {
			const where = await exportThemeFile(themeJson(this.theme), `${slug}.kerf-theme.json`);
			if (where) toast.success('Theme saved', { description: where });
		} catch (e) {
			toast.error('Could not save the theme', { description: String(e) });
		}
	}

	toggle() {
		this.open = !this.open;
		if (this.open && !this.loaded) void this.load();
	}

	close() {
		this.open = false;
	}
}

export const settings = new SettingsStore();
