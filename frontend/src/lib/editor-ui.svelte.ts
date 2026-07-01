/* Editor chrome + playback/transport state. The chrome reflects the real
   project: which media is imported, whether analysis is running, the playhead
   and zoom. `runAnalysis` performs real analysis via kerf-core (desktop) or the
   in-browser sample backend; there is no scripted demo workflow. */

import { editor } from './state.svelte';
import { listFonts } from './api';
import { audio } from './audio';

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
	/** Shuttle rate while playing: 1 = normal, ±2/±4/±8 from J/L taps.
	 *  Audio is muted in reverse (the playhead falls back to wall-clock). */
	rate = $state(1);
	/** Timeline zoom, pixels per second. */
	zoom = $state(36);
	/** Bumped when a preview proxy finishes generating, to nudge the preview into
	 *  re-decoding the current frame (now served from the fast all-intra proxy). */
	previewEpoch = $state(0);
	/** System font family names available for the text overlay font picker. */
	availableFonts = $state<string[]>([]);

	#raf: number | null = null;

	/** Fetch the installed system fonts once at startup. */
	async loadFonts() {
		this.availableFonts = await listFonts();
	}

	/** Force the preview to re-fetch the frame under the playhead. Called when a
	 *  background proxy becomes ready so the still updates without a manual scrub. */
	refreshPreview() {
		this.previewEpoch++;
	}

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
	 *  or before zero. Re-anchors audio when it lands mid-playback. */
	seek(t: number) {
		this.time = Math.min(Math.max(0, t), Math.max(0, editor.duration));
		if (this.playing) this.#startAudio();
	}

	togglePlay() {
		this.playing ? this.pause() : this.play();
	}

	play(rate = 1) {
		if (this.playing && rate === this.rate) return;
		if (this.#raf) cancelAnimationFrame(this.#raf);
		if (rate > 0 && this.time >= editor.duration) this.time = 0;
		if (rate < 0 && this.time <= 0) return;
		this.playing = true;
		this.rate = rate;
		this.#startAudio();
		let last = performance.now();
		const step = (now: number) => {
			if (!this.playing) return;
			// Follow the audio clock when it runs, so picture chases sound rather
			// than the other way around; wall-clock otherwise (reverse shuttle,
			// browser demo).
			const ac = audio.clock();
			this.time = ac !== null ? ac : this.time + ((now - last) / 1000) * this.rate;
			last = now;
			if (this.rate > 0 && this.time >= editor.duration) {
				this.time = editor.duration;
				this.pause();
				return;
			}
			if (this.rate < 0 && this.time <= 0) {
				this.time = 0;
				this.pause();
				return;
			}
			this.#raf = requestAnimationFrame(step);
		};
		this.#raf = requestAnimationFrame(step);
	}

	pause() {
		this.playing = false;
		this.rate = 1;
		audio.stop();
		if (this.#raf) cancelAnimationFrame(this.#raf);
		this.#raf = null;
	}

	/** Re-anchor audio playback after a timeline edit so what's heard matches
	 *  the new cut; a no-op when paused. */
	resync() {
		if (this.playing) this.#startAudio();
	}

	#startAudio() {
		if (this.rate > 0) {
			const withAudio = new Set(
				editor.assets.filter((a) => a.streams.some((s) => s.kind === 'audio')).map((a) => a.id)
			);
			audio.start(editor.timeline, withAudio, this.time, this.rate);
		} else {
			audio.stop();
		}
	}
}

export const ui = new EditorUi();
