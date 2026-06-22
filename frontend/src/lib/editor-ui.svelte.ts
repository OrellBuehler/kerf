/* Editor chrome + playback/transport state. The chrome reflects the real
   project: which media is imported, whether analysis is running, the playhead
   and zoom. `runAnalysis` performs real analysis via kerf-core (desktop) or the
   in-browser sample backend; there is no scripted demo workflow. */

import { editor } from './state.svelte';

export type Tool = 'pointer' | 'razor' | 'bookmark';

class EditorUi {
	tool = $state<Tool>('pointer');
	snap = $state(true);
	agentOpen = $state(true);
	playing = $state(false);
	/** Whether an analysis pass is currently running. */
	analyzing = $state(false);
	/** Analysis progress, 0–100. */
	progress = $state(0);
	/** Playhead position, seconds. */
	time = $state(0);
	/** Timeline zoom, pixels per second. */
	zoom = $state(36);

	#timer: ReturnType<typeof setInterval> | null = null;
	#raf: number | null = null;

	#clear() {
		if (this.#timer) clearInterval(this.#timer);
		this.#timer = null;
	}

	/** Analyze an asset, animating progress while kerf-core works. */
	async runAnalysis(assetId: string) {
		this.#clear();
		this.analyzing = true;
		this.progress = 6;
		this.#timer = setInterval(() => {
			this.progress = Math.min(94, this.progress + 6);
		}, 200);
		try {
			await editor.analyze(assetId);
		} finally {
			this.#clear();
			this.progress = 100;
			this.analyzing = false;
		}
	}

	// ---- playback ----------------------------------------------------------

	seek(t: number) {
		this.time = Math.max(0, t);
	}

	togglePlay() {
		this.playing ? this.pause() : this.play();
	}

	play() {
		if (this.playing) return;
		if (this.time >= editor.duration) this.time = 0;
		this.playing = true;
		let last = performance.now();
		const step = (now: number) => {
			if (!this.playing) return;
			this.time += (now - last) / 1000;
			last = now;
			if (this.time >= editor.duration) {
				this.time = editor.duration;
				this.playing = false;
				return;
			}
			this.#raf = requestAnimationFrame(step);
		};
		this.#raf = requestAnimationFrame(step);
	}

	pause() {
		this.playing = false;
		if (this.#raf) cancelAnimationFrame(this.#raf);
		this.#raf = null;
	}
}

export const ui = new EditorUi();
