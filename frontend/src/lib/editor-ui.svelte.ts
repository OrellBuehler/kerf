/* Editor chrome + playback/transport state. The chrome reflects the real
   project: which media is imported, whether analysis is running, the playhead
   and zoom. `runAnalysis` performs real analysis via kerf-core (desktop) or the
   in-browser sample backend; there is no scripted demo workflow. */

import { editor } from './state.svelte';
import { cancelAnalysis, downloadSpeechModel, listFonts, setSpeechModel, transcriptionStatus } from './api';
import { audio } from './audio';
import { toast } from './notifications.svelte';
import type { AnalysisProgress, TranscriptionStatus } from './types';

export type Tool = 'pointer' | 'razor';

class EditorUi {
	tool = $state<Tool>('pointer');
	snap = $state(true);
	playing = $state(false);
	/** The asset being dragged from the media bin, while a drag is in flight. */
	dndAsset = $state<{ id: string; kind: 'video' | 'audio'; duration: number } | null>(null);
	/** Whether an analysis pass is currently running. */
	analyzing = $state(false);
	/** The asset currently being analyzed (so the bin badges the right one). */
	analyzingId = $state<string | null>(null);
	/** The step analysis is on, streamed from the backend. Analysis is no longer
	 *  a few seconds of ffmpeg: the first transcription downloads a speech model
	 *  and then runs inference for minutes, so the UI names the step and shows a
	 *  real percentage wherever the backend can produce one. */
	analysisStage = $state<AnalysisProgress | null>(null);
	/** Which speech-to-text backend is available, once probed. */
	transcription = $state<TranscriptionStatus | null>(null);
	/** Set while a speech model is being fetched deliberately (not mid-analysis). */
	downloadingModel = $state<string | null>(null);
	/** 0..1 for that download. */
	modelFraction = $state(0);
	/** How many assets are still queued behind the one being analyzed, so the
	 *  UI can say "2 of 5" rather than spin at an unknown length. */
	analysisQueued = $state(0);
	/** Set while a stop has been asked for but the pass hasn't given up yet. */
	stoppingAnalysis = $state(false);
	/** Playhead position, seconds. */
	time = $state(0);
	/** Shuttle rate while playing: 1 = normal, ±2/±4/±8 from J/L taps.
	 *  Audio is muted in reverse (the playhead falls back to wall-clock). */
	rate = $state(1);
	/** In/out marks (seconds) — set with I/O, cleared with Shift+I/O. They
	 *  bracket the working range; export can render just this span. */
	markIn = $state<number | null>(null);
	markOut = $state<number | null>(null);
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

	/** Fetch the speech-to-text backend status once at startup, so the transcript
	 *  tab can explain itself before anything is analyzed. */
	async loadTranscriptionStatus() {
		try {
			this.transcription = await transcriptionStatus();
		} catch {
			this.transcription = null;
		}
	}

	/** Record a step reported by the running analysis pass (the `analysis-progress`
	 *  Tauri event). Ignores events for an asset we're no longer waiting on. */
	noteAnalysisProgress(p: AnalysisProgress) {
		if (this.analyzingId && p.asset_id !== this.analyzingId) return;
		this.analysisStage = p;
	}

	/** A short label for the step analysis is on, or null when idle. */
	get analysisLabel(): string | null {
		if (!this.analyzing) return null;
		const p = this.analysisStage;
		if (!p) return 'analyzing';
		const name =
			{
				silence: 'detecting silence',
				scenes: 'detecting scenes',
				loudness: 'measuring loudness',
				rhythm: 'finding the beat',
				download_model: 'downloading speech model',
				transcribe: 'transcribing',
				done: 'analyzing'
			}[p.stage] ?? p.stage;
		const pct = p.fraction != null ? ` ${Math.round(p.fraction * 100)}%` : '';
		return `${name}${pct}`;
	}

	/** Pick which speech model transcription uses (remembered in the project). */
	async chooseSpeechModel(name: string) {
		this.transcription = await setSpeechModel(name);
	}

	/** Download a speech model up front, so the first transcription doesn't
	 *  stall on a few hundred megabytes. Refreshes the status when it lands. */
	async fetchSpeechModel(name: string) {
		this.downloadingModel = name;
		this.modelFraction = 0;
		try {
			await downloadSpeechModel(name);
			await this.loadTranscriptionStatus();
		} catch (e) {
			// A few hundred megabytes over someone else's CDN fails often enough
			// to be routine — offline, a proxy, a half-written cache file. It used
			// to reject into nothing, so the download simply stopped looking like
			// it was happening.
			toast.error(`Couldn't download the ${name} speech model — ${message(e)}`);
		} finally {
			this.downloadingModel = null;
			this.modelFraction = 0;
		}
	}

	/** Analyze an asset, flagging `analyzing` while kerf-core works and following
	 *  the streamed step reports. Resolves false when the pass was stopped. */
	async runAnalysis(assetId: string): Promise<boolean> {
		this.analyzing = true;
		this.analyzingId = assetId;
		this.analysisStage = null;
		try {
			await editor.analyze(assetId);
			// Transcription may have downloaded a model on the way.
			if (this.transcription && !this.transcription.model_ready) await this.loadTranscriptionStatus();
			return true;
		} catch (e) {
			// A stop is the user's own doing, not a failure to report.
			if (isCancelled(e)) return false;
			throw e;
		} finally {
			this.analyzing = false;
			this.analyzingId = null;
			this.analysisStage = null;
			this.stoppingAnalysis = false;
		}
	}

	/** Analyze a batch one at a time — each pass is ffmpeg-bound, so running
	 *  them together would only make every one of them slower. Stopping drops
	 *  the whole rest of the queue: importing ten clips must not be a
	 *  commitment to ten transcriptions. */
	async analyzeQueue(assetIds: string[]) {
		for (let i = 0; i < assetIds.length; i++) {
			this.analysisQueued = assetIds.length - i - 1;
			try {
				if (!(await this.runAnalysis(assetIds[i]))) break;
			} catch (e) {
				// One unanalyzable file shouldn't abandon the rest of the import;
				// its media is already in the bin either way. Say so, though —
				// swallowed, a failed transcription reads as one that found no
				// speech.
				const name = editor.assets.find((a) => a.id === assetIds[i])?.name ?? 'that clip';
				toast.error(`Couldn't analyze ${name} — ${message(e)}`);
			}
		}
		this.analysisQueued = 0;
	}

	/** Ask the running analysis pass to give up. It stops between steps, and
	 *  within about a second during transcription — the step long enough for
	 *  the wait to matter. */
	stopAnalysis() {
		if (!this.analyzing) return;
		this.stoppingAnalysis = true;
		this.analysisQueued = 0;
		void cancelAnalysis();
	}

	// ---- playback ----------------------------------------------------------

	/**
	 * Bumped on every *deliberate* move of the playhead — a seek or a fresh
	 * play — but never by playback advancing it, which writes `time` directly.
	 * Playback's streamed frame source keys off this: it has to restart when the
	 * user jumps somewhere, and must not restart 60 times a second just because
	 * time is passing.
	 */
	seekEpoch = $state(0);

	/** Move the playhead, clamped to the timeline so it can't park past the end
	 *  or before zero. Re-anchors audio when it lands mid-playback. */
	seek(t: number) {
		this.time = Math.min(Math.max(0, t), Math.max(0, editor.duration));
		this.seekEpoch++;
		if (this.playing) this.#startAudio();
	}

	/** Jump to the nearest marker before (-1) or after (+1) the playhead. */
	gotoMarker(dir: 1 | -1) {
		const eps = 1e-4;
		const times = editor.markers
			.map((m) => m.time)
			.filter((t) => (dir > 0 ? t > this.time + eps : t < this.time - eps));
		if (times.length === 0) return;
		this.seek(dir > 0 ? Math.min(...times) : Math.max(...times));
	}

	togglePlay() {
		this.playing ? this.pause() : this.play();
	}

	/** J/K/L shuttle: a tap in the current play direction doubles the rate
	 *  (capped at 8×), a tap the other way starts fresh at 1×. */
	shuttle(dir: 1 | -1) {
		const sameDir = this.playing && Math.sign(this.rate) === dir;
		const target = sameDir ? Math.min(8, Math.abs(this.rate) * 2) : 1;
		this.play(dir * target);
	}

	play(rate = 1) {
		if (this.playing && rate === this.rate) return;
		if (rate < 0 && this.time <= 0) return; // nothing to shuttle back into
		if (this.#raf) cancelAnimationFrame(this.#raf);
		if (rate > 0 && this.time >= editor.duration) this.time = 0;
		this.playing = true;
		this.rate = rate;
		this.seekEpoch++;
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

/** The readable part of whatever the backend rejected with. */
function message(e: unknown): string {
	return e instanceof Error ? e.message : String(e);
}

/** Whether a rejected analysis was cancelled rather than broken. The backend
 *  reports a stop as an ordinary error string; this is the one it uses. */
function isCancelled(e: unknown): boolean {
	return String(e instanceof Error ? e.message : e).includes('analysis cancelled');
}

export const ui = new EditorUi();
