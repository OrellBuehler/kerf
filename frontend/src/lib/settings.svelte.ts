// App preferences (Svelte 5 runes).
//
// The settings that belong to the machine rather than to the cut. Today that is
// one thing — how much of the computer Kerf's media engine may take — and it is
// the reason this surface exists: ffmpeg is written to finish as fast as
// possible, so a batch of analyses used to spawn one all-cores decode per file
// and leave the desktop unusable until they finished. `kerf_core::engine::cpu`
// enforces the budget; this is the state the dialog renders.
//
// The persisted value lives on the Rust side (the platform config directory),
// so this holds only the resolved view and writes through `api.ts` — which
// keeps the browser harness working over localStorage.

import { getSettings, setSettings } from './api';
import { toast } from './notifications.svelte';
import { ui } from './editor-ui.svelte';
import type { SettingsView } from './types';

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
	transcribe = $state(true);
	/** Shade the delivery safe areas over the preview. Only visible while the
	 *  project is cut for a vertical or square frame; a 16:9 web export has no
	 *  chrome to stay clear of. */
	safeAreas = $state(false);
	cpuCores = $state(1);
	cpuThreads = $state(1);
	cpuMinPercent = $state(10);

	private get current() {
		return { cpu_percent: this.cpuPercent, transcribe: this.transcribe, safe_areas: this.safeAreas };
	}

	/** The preset the current percentage *is*, or null when it sits between them. */
	get cpuPreset() {
		return CPU_PRESETS.find((p) => p.percent === this.cpuPercent) ?? null;
	}

	private absorb(view: SettingsView) {
		this.cpuPercent = view.cpu_percent;
		this.transcribe = view.transcribe;
		this.safeAreas = view.safe_areas;
		this.cpuCores = view.cpu_cores;
		this.cpuThreads = view.cpu_threads;
		this.cpuMinPercent = view.cpu_min_percent;
		this.loaded = true;
	}

	async load() {
		try {
			this.absorb(await getSettings());
		} catch (e) {
			// Not worth a toast at launch: the dialog just shows the defaults.
			console.error('could not read settings', e);
		}
	}

	/** Write the CPU budget through. The engine clamps, so the view that comes
	 *  back — not the value asked for — is what gets shown. */
	async setCpuPercent(percent: number) {
		const want = Math.round(percent);
		if (want === this.cpuPercent) return;
		this.cpuPercent = want; // optimistic: the slider must not lag the drag
		this.saving = true;
		try {
			this.absorb(await setSettings({ ...this.current, cpu_percent: want }));
		} catch (e) {
			toast.error('Could not save the CPU limit', { description: String(e) });
			await this.load();
		} finally {
			this.saving = false;
		}
	}

	/** Turn speech-to-text in the analysis pass on or off. */
	async setTranscribe(on: boolean) {
		if (on === this.transcribe) return;
		this.transcribe = on;
		this.saving = true;
		try {
			this.absorb(await setSettings({ ...this.current, transcribe: on }));
			await ui.loadTranscriptionStatus();
		} catch (e) {
			toast.error('Could not save the transcription setting', { description: String(e) });
			await this.load();
		} finally {
			this.saving = false;
		}
	}

	/** Show or hide the safe-area guides over the preview. */
	async setSafeAreas(on: boolean) {
		if (on === this.safeAreas) return;
		this.safeAreas = on;
		this.saving = true;
		try {
			this.absorb(await setSettings({ ...this.current, safe_areas: on }));
		} catch (e) {
			toast.error('Could not save the safe-area setting', { description: String(e) });
			await this.load();
		} finally {
			this.saving = false;
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
