// App preferences (Svelte 5 runes).
//
// The settings that belong to the machine rather than to the cut: how much of
// the computer Kerf's media engine may take (`kerf_core::engine::cpu` enforces
// the budget), whether analysis transcribes, what the preview draws, how the
// workspace is arranged and what colors it is drawn in.
//
// The persisted values live on the Rust side (the platform config directory),
// so this holds only the resolved view and writes through `api.ts` — which
// keeps the browser harness working over localStorage.

import { exportThemeFile, getSettings, importThemeFile, setSettings } from './api';
import { toast } from './notifications.svelte';
import { ui } from './editor-ui.svelte';
import { applyTheme, parseTheme, PRESETS, presetIdFor, themeJson, type ColorToken, type PresetId, type Theme } from './theme';
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
	/** The saved workspace arrangement, read once when the dock is built. */
	layout = $state<unknown>(null);
	theme = $state<Theme>(PRESETS['kerf-dark']);
	/** Color edits apply at once and are written a moment later; while one is
	 *  pending, a view coming back from another write must not overwrite the
	 *  newer colors on screen. */
	private themeDirty = false;
	private themeTimer: ReturnType<typeof setTimeout> | null = null;

	private get current(): AppSettings {
		return {
			cpu_percent: this.cpuPercent,
			transcribe: this.transcribe,
			safe_areas: this.safeAreas,
			layout: this.layout,
			theme: this.theme
		};
	}

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
		this.layout = view.layout;
		if (!this.themeDirty) {
			this.theme = parseTheme(view.theme) ?? PRESETS['kerf-dark'];
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
			this.loaded = true;
		}
	}

	private async write(patch: Partial<AppSettings>, what: string): Promise<boolean> {
		this.saving = true;
		try {
			this.absorb(await setSettings({ ...this.current, ...patch }));
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

	/** Remember the workspace arrangement. */
	async setLayout(layout: unknown) {
		this.layout = layout;
		await this.write({ layout }, 'layout');
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
