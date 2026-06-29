/* Editor chrome + playback/transport state. The chrome reflects the real
   project: which media is imported, whether analysis is running, the playhead
   and zoom. `runAnalysis` performs real analysis via kerf-core (desktop) or the
   in-browser sample backend; there is no scripted demo workflow. */

import { editor } from './state.svelte';

export type Tool = 'pointer' | 'razor';

class EditorUi {
	tool = $state<Tool>('pointer');
	snap = $state(true);
	agentOpen = $state(true);
	playing = $state(false);
	/** The asset being dragged from the media bin, while a drag is in flight. */
	dndAsset = $state<{ id: string; kind: 'video' | 'audio'; duration: number } | null>(null);
	/** Whether an analysis pass is currently running. */
	analyzing = $state(false);
	/** The asset currently being analyzed (so the bin badges the right one). */
	analyzingId = $state<string | null>(null);
	/** Playhead position, seconds. */
	time = $state(0);
	/** Timeline zoom, pixels per second. */
	zoom = $state(36);

	#raf: number | null = null;

	/** Analyze an asset, flagging `analyzing` while kerf-core works. The work has
	 *  no real progress signal, so the UI shows an indeterminate state rather than
	 *  a fabricated percentage. */
	async runAnalysis(assetId: string) {
		this.analyzing = true;
		this.analyzingId = assetId;
		try {
			await editor.analyze(assetId);
		} finally {
			this.analyzing = false;
			this.analyzingId = null;
		}
	}

	// ---- playback ----------------------------------------------------------

	/** Move the playhead, clamped to the timeline so it can't park past the end
	 *  or before zero. */
	seek(t: number) {
		this.time = Math.min(Math.max(0, t), Math.max(0, editor.duration));
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
