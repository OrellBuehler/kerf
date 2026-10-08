/* Editor chrome + playback/transport state. The chrome reflects the real
   project: which media is imported, whether analysis is running, the playhead
   and zoom. `runAnalysis` performs real analysis via kerf-core (desktop) or the
   in-browser sample backend; there is no scripted demo workflow. */

import { editor } from './state.svelte';
import {
	cancelAnalysis,
	cancelLevels,
	downloadSpeechModel,
	getLevels,
	listFonts,
	setSpeechModel,
	transcriptionStatus
} from './api';
import { audio } from './audio';
import { toast } from './notifications.svelte';
import type {
	AnalysisKind,
	AnalysisProgress,
	AutoAnalysis,
	CaptionStyle,
	CaptionTimeBase,
	Levels,
	TranscriptionStatus
} from './types';
import { autoSteps, kindInfo, kindOfStage, missingSteps } from './analysis-steps';
import { mediaStatus } from './media-status.svelte';
import { isLevelsCancelled, measureRange, type MeasureStamp } from './levels-view';
import type { VoiceoverPrefill } from './voiceover';
import type { TrimMonitor, TrimTool } from './trim-tools';
import { ZOOM_DEFAULT, stepZoom } from './zoom';
import {
	heightOf,
	heightPx,
	loadHeights,
	loadMinimap,
	saveHeights,
	saveMinimap,
	setAll,
	setTrack,
	type HeightPreset,
	type TrackHeights
} from './track-heights';

/** The timeline's tools: select and razor, then the three that move a boundary
 *  rather than a clip (`trim-tools.ts`). */
export type Tool = 'pointer' | 'razor' | TrimTool;

/** A loudness measurement and what it was a measurement *of*. */
export interface MeasureResult {
	levels: Levels;
	/** The span measured, or `null` for the whole cut. */
	range: { start: number; end: number } | null;
	/** The project state it was taken against — to say when it has gone out of date. */
	stamp: MeasureStamp;
}

class EditorUi {
	tool = $state<Tool>('pointer');
	snap = $state(true);
	/** The frames either side of a roll / slip / slide while one is being dragged —
	 *  the timeline writes it, the Preview shows it in place of the playhead's frame,
	 *  and it is `null` the moment the gesture ends however it ends. */
	trimMonitor = $state<TrimMonitor | null>(null);
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
	/** The voiceover dialog: `null` while closed, else what it opens with. */
	voiceoverDialog = $state<{ prefill: VoiceoverPrefill | null } | null>(null);
	/** The export dialog. Opened from the toolbar, ⌘E and the Deliver panel. */
	exportDialog = $state(false);
	/** The delivery shapes (ids from `DELIVERY_PRESETS`) ticked for a multi-format
	 *  export, and whether each shot is framed for each of them first. Held here
	 *  rather than in the export dialog because the Deliver panel edits the same
	 *  choice — it would otherwise be lost on the way to the dialog. */
	deliverShapes = $state<string[]>([]);
	deliverSmartCrop = $state(true);
	/** The look the caption button generates in, and the one an imported subtitle
	 *  file is laid out in. Not derived from the captions
	 *  already on the timeline: a caption's style is not recoverable from the
	 *  text it carries, and guessing from the word count would flip the choice
	 *  every time a sentence happened to be short. Shared by every place that
	 *  offers the button (the Inspector and the library's Titles tab). */
	captionStyle = $state<CaptionStyle>('lines');
	/** What the Titles controls' *Import captions* options say: whether a subtitle
	 *  file's times are the finished cut's (`timeline`) or one clip's own footage
	 *  (`source`, with the asset picked here). Held beside `captionStyle` so the two
	 *  copies of the controls agree. `caption-import-ui.ts` resolves them against
	 *  the cut — an asset it no longer shows, or no clip to time to, falls back. */
	captionImportBase = $state<CaptionTimeBase>('timeline');
	captionImportAsset = $state<string | null>(null);
	/** "Keep the file's lines": `null` until the box is touched, so it follows the
	 *  delivery frame (`keepLinesDefault`); true / false once the user has chosen. */
	captionImportKeepLines = $state<boolean | null>(null);
	/** Seconds the imported cues are shifted by, as typed (`null` = empty). */
	captionImportOffset = $state<number | null>(null);
	/** The Mixer's loudness measurement (`get_levels`): one metered pass over the
	 *  audio the export would render, which takes seconds on a long cut. Held here and
	 *  not in the panel because the panel is rebuilt whenever the workspace changes —
	 *  a measurement under way should survive a switch to Edit and back, and its
	 *  result should be there when the Mixer is. `stopMeasure` abandons the pass
	 *  (`stopping` until the backend has given up); a stopped one leaves the last
	 *  result in place and reports nothing. */
	measure = $state<{ running: boolean; stopping: boolean; result: MeasureResult | null; error: string | null }>({
		running: false,
		stopping: false,
		result: null,
		error: null
	});
	/** Playhead position, seconds. */
	time = $state(0);
	/** Shuttle rate while playing: 1 = normal, ±2/±4/±8 from J/L taps.
	 *  Audio is muted in reverse (the playhead falls back to wall-clock). */
	rate = $state(1);
	/** In/out marks (seconds) — set with I/O, cleared with Shift+I/O. They
	 *  bracket the working range; export can render just this span. */
	markIn = $state<number | null>(null);
	markOut = $state<number | null>(null);
	/** Timeline zoom, pixels per second (`zoom.ts` owns the range and the maths). */
	zoom = $state(ZOOM_DEFAULT);
	/** Bumped to ask the timeline to fit the whole cut in its window. Only the
	 *  timeline knows how wide that is, so the shortcut (which lives in the page's
	 *  key handler) asks rather than computes. A counter, like `seekEpoch`: a
	 *  repeated ask is a new event. */
	fitEpoch = $state(0);
	/** Track heights: a named preset per track (and the global one the titles lane
	 *  follows), remembered per track id in `localStorage` — a viewer's choice, not
	 *  part of the cut (`track-heights.ts` owns the shape and the storage). */
	heights = $state<TrackHeights>(loadHeights());
	/** Whether the minimap strip over the timeline is shown (also remembered). */
	minimap = $state(loadMinimap());
	/** Bumped when a preview proxy finishes generating, to nudge the preview into
	 *  re-decoding the current frame (now served from the fast all-intra proxy). */
	previewEpoch = $state(0);
	/** System font family names available for the text overlay font picker. */
	availableFonts = $state<string[]>([]);

	#raf: number | null = null;

	/** Measure the cut's loudness — the whole of it, or the in / out range when both
	 *  marks are set (the export dialog's rule). A no-op while one is already running. */
	async measureLevels(): Promise<void> {
		if (this.measure.running) return;
		const range = measureRange(this.markIn, this.markOut);
		const stamp: MeasureStamp = {
			seq: editor.history.find((r) => r.current)?.seq ?? null,
			path: editor.currentPath
		};
		this.measure.running = true;
		this.measure.error = null;
		try {
			this.measure.result = { levels: await getLevels(range), range, stamp };
		} catch (e) {
			// A stop is the user's own doing, not a failure to report.
			if (!isLevelsCancelled(e)) {
				this.measure.error = message(e);
				toast.error(`Couldn't measure the loudness — ${this.measure.error}`);
			}
		} finally {
			this.measure.running = false;
			this.measure.stopping = false;
		}
	}

	/** Ask the running measurement to give up. It reads the whole mix, so on a long
	 *  cut that is minutes the user should not have to wait out. */
	stopMeasure() {
		if (!this.measure.running || this.measure.stopping) return;
		this.measure.stopping = true;
		void cancelLevels();
	}

	openVoiceover(prefill: VoiceoverPrefill | null = null) {
		this.voiceoverDialog = { prefill };
	}

	closeVoiceover() {
		this.voiceoverDialog = null;
	}

	openExport() {
		this.exportDialog = true;
	}

	closeExport() {
		this.exportDialog = false;
	}

	toggleDeliverShape(id: string) {
		this.deliverShapes = this.deliverShapes.includes(id)
			? this.deliverShapes.filter((v) => v !== id)
			: [...this.deliverShapes, id];
	}

	/** The height preset of one track. */
	trackPreset(trackId: string): HeightPreset {
		return heightOf(this.heights, trackId);
	}

	/** The lane height of one track, px. */
	trackPx(trackId: string): number {
		return heightPx(this.heights, trackId);
	}

	/** Set one track's height. */
	setTrackHeight(trackId: string, preset: HeightPreset) {
		this.heights = setTrack(this.heights, trackId, preset);
		saveHeights(this.heights);
	}

	/** Set every track's height, and the titles lane's. */
	setAllHeights(preset: HeightPreset) {
		this.heights = setAll(this.heights, preset);
		saveHeights(this.heights);
	}

	toggleMinimap() {
		this.minimap = !this.minimap;
		saveMinimap(this.minimap);
	}

	/** One step in (`1`) or out (`-1`) on the zoom; the timeline holds the playhead still. */
	zoomBy(dir: 1 | -1) {
		this.zoom = stepZoom(this.zoom, dir, editor.duration);
	}

	/** Zoom so the whole cut fits the timeline's width (⇧Z). */
	zoomToFit() {
		this.fitEpoch++;
	}

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

	/** The kind of analysis the running pass is on, once it has said (the chips show it spinning). */
	get analysisKind(): AnalysisKind | null {
		return this.analyzing ? kindOfStage(this.analysisStage?.stage) : null;
	}

	/** A short label for the step analysis is on, or null when idle. */
	get analysisLabel(): string | null {
		if (!this.analyzing) return null;
		const p = this.analysisStage;
		if (!p) return 'analyzing';
		const kind = kindOfStage(p.stage);
		const name =
			(kind ? kindInfo(kind).doing : undefined) ??
			{
				waiting: p.detail ?? 'waiting its turn',
				download_model: 'downloading speech model',
				done: 'analyzing'
			}[p.stage] ??
			p.stage;
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
	 *  the streamed step reports. `steps` names the kinds to run (only those, merged into what
	 *  is cached); without it the backend runs what Settings leaves switched on. Resolves
	 *  false when the pass was stopped; a step that failed is reported here, once, and the
	 *  others still landed. */
	async runAnalysis(assetId: string, steps?: readonly AnalysisKind[]): Promise<boolean> {
		this.analyzing = true;
		this.analyzingId = assetId;
		this.analysisStage = null;
		try {
			await editor.analyze(assetId, steps);
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
			await mediaStatus.refresh();
			this.reportFailedSteps(assetId, steps);
		}
	}

	/** Say which steps of a run failed (the backend carries on without them, so the run itself
	 *  succeeded). Only the kinds this run asked for — an earlier failure is not news. */
	private reportFailedSteps(assetId: string, steps?: readonly AnalysisKind[]) {
		const failed = (mediaStatus.analysis(assetId)?.kinds ?? []).filter(
			(k) => k.state === 'failed' && (!steps || steps.includes(k.kind))
		);
		if (failed.length === 0) return;
		const name = editor.assets.find((a) => a.id === assetId)?.name ?? 'that clip';
		for (const k of failed) {
			toast.error(`Couldn't ${kindInfo(k.kind).action.toLowerCase()} for ${name} — ${k.reason ?? 'it failed'}`);
		}
	}

	/** Make sure the `needed` kinds are done for an asset, running only the ones that are not —
	 *  what a quick edit does before it reads the result. */
	async ensureAnalysis(assetId: string, needed: readonly AnalysisKind[]): Promise<void> {
		const missing = missingSteps(mediaStatus.analysis(assetId), needed);
		if (missing.length > 0) await this.runAnalysis(assetId, missing);
	}

	/** Analyze a batch one at a time — each pass is ffmpeg-bound, so running
	 *  them together would only make every one of them slower. Stopping drops
	 *  the whole rest of the queue: importing ten clips must not be a
	 *  commitment to ten transcriptions. */
	async analyzeQueue(jobs: { id: string; steps?: readonly AnalysisKind[] }[]) {
		for (let i = 0; i < jobs.length; i++) {
			this.analysisQueued = jobs.length - i - 1;
			try {
				if (!(await this.runAnalysis(jobs[i].id, jobs[i].steps))) break;
			} catch (e) {
				// One unanalyzable file shouldn't abandon the rest of the import;
				// its media is already in the bin either way. Say so, though —
				// swallowed, a failed transcription reads as one that found no
				// speech.
				const name = editor.assets.find((a) => a.id === jobs[i].id)?.name ?? 'that clip';
				toast.error(`Couldn't analyze ${name} — ${message(e)}`);
			}
		}
		this.analysisQueued = 0;
	}

	/** Analyze what just arrived, by the user's own rules (Settings › Analysis): nothing when
	 *  the master switch is off, otherwise only the kinds switched on that the asset has not
	 *  already had — importing a file the project holds again redoes nothing. */
	async analyzeImported(assetIds: string[], auto: AutoAnalysis) {
		if (!auto.enabled) return;
		await mediaStatus.refresh();
		const available = this.transcription?.available ?? false;
		const jobs = assetIds
			.map((id) => ({ id, steps: autoSteps(auto, mediaStatus.analysis(id), available) }))
			.filter((j) => j.steps.length > 0);
		await this.analyzeQueue(jobs);
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
