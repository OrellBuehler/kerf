/* Editor chrome + the agent workflow state machine.
   The cut workflow (empty → analyzing → review → editing) drives the editor
   chrome. In the desktop app `runAnalysis` performs real local analysis over
   MCP/kerf-core; in the browser the status-bar "Demo state" selector and the
   import action animate through the same states the design showcases. */

import { editor } from './state.svelte';
import { inTauri } from './api';

export type EditorPhase = 'empty' | 'analyzing' | 'review' | 'editing';
export type Tool = 'pointer' | 'razor' | 'bookmark';

class EditorUi {
	phase = $state<EditorPhase>('empty');
	tool = $state<Tool>('pointer');
	snap = $state(true);
	agentOpen = $state(true);
	playing = $state(false);
	progress = $state(0);
	/** Playhead position, seconds. */
	time = $state(0);
	/** Timeline zoom, pixels per second. */
	zoom = $state(36);

	#timer: ReturnType<typeof setInterval> | null = null;
	#advance: ReturnType<typeof setTimeout> | null = null;
	#raf: number | null = null;

	#clear() {
		if (this.#timer) clearInterval(this.#timer);
		if (this.#advance) clearTimeout(this.#advance);
		this.#timer = null;
		this.#advance = null;
	}

	setPhase(phase: EditorPhase) {
		this.#clear();
		this.phase = phase;
		if (phase === 'analyzing') this.#runMockAnalysis();
	}

	#runMockAnalysis() {
		this.progress = 8;
		this.#timer = setInterval(() => {
			if (this.progress >= 100) {
				this.#clear();
				this.#advance = setTimeout(() => this.setPhase('review'), 500);
				return;
			}
			this.progress = Math.min(100, this.progress + 8);
		}, 220);
	}

	/** Browser-only showcase: fake the analyze → review transition. */
	startAnalyze() {
		this.progress = 0;
		this.setPhase('analyzing');
	}

	/** Real analysis: animate progress while kerf-core works, then land in edit. */
	async runAnalysis(assetId: string) {
		if (!inTauri()) {
			this.startAnalyze();
			return;
		}
		this.#clear();
		this.phase = 'analyzing';
		this.progress = 6;
		this.#timer = setInterval(() => {
			this.progress = Math.min(94, this.progress + 6);
		}, 200);
		try {
			await editor.analyze(assetId);
		} finally {
			this.#clear();
			this.progress = 100;
			this.phase = 'editing';
		}
	}

	apply() {
		this.setPhase('editing');
	}

	reject() {
		this.setPhase('editing');
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
