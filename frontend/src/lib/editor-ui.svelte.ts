/* Editor chrome + playback/transport state. The chrome reflects the real
   project: which media is imported, whether analysis is running, the playhead
   and zoom. `runAnalysis` performs real analysis via kerf-core (desktop) or the
   in-browser sample backend; there is no scripted demo workflow. */

import { editor } from './state.svelte';
import {
	cancelAnalysis,
	cancelLevels,
	cancelModelDownload,
	downloadSpeechModel,
	getLevels,
	listFonts,
	setSpeechModel,
	transcriptionStatus
} from './api';
import { audio } from './audio';
import { windows, type FrameHandle } from './windows.svelte';
import { toast } from './notifications.svelte';
import type {
	AnalysisKind,
	AnalysisProgress,
	AutoAnalysis,
	CaptionStyle,
	CaptionTimeBase,
	Levels,
	ModelProgress,
	TranscriptionStatus
} from './types';
import { autoSteps, counterKind, kindInfo, kindOfStage, missingSteps, stepCounter, stepWords, type StepCounter } from './analysis-steps';
import { mediaStatus } from './media-status.svelte';
import { isLevelsCancelled, measureRange, type MeasureStamp } from './levels-view';
import type { ExportDraft } from './export-draft';
import type { VoiceoverPrefill } from './voiceover';
import type { ComposePrefill } from './compose-form';
import type { TrimMonitor, TrimTool } from './trim-tools';
import { ZOOM_DEFAULT, stepZoom } from './zoom';
import { seekTime, stepPoint, type SeekOptions } from './frames';
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
	/** The kinds the running pass was asked for, for "step 2 of 5" — `null` when the run was left to Settings'
	 *  switches and the plan is not known here. */
	analysisPlan = $state<readonly AnalysisKind[] | null>(null);
	/** Which speech-to-text backend is available, once probed. */
	transcription = $state<TranscriptionStatus | null>(null);
	/** Set while a speech model is being fetched deliberately (not mid-analysis). */
	downloadingModel = $state<string | null>(null);
	/** 0..1 for that download. */
	modelFraction = $state(0);
	/** The latest `model-progress` report: the bytes fetched of the model, for a line under the bar. */
	modelProgress = $state<ModelProgress | null>(null);
	/** A stop has been asked for and the download has not given up yet. */
	cancellingModel = $state(false);
	/** How many assets are still queued behind the one being analyzed, so the
	 *  UI can say "2 of 5" rather than spin at an unknown length. */
	analysisQueued = $state(0);
	/** Set while a stop has been asked for but the pass hasn't given up yet. */
	stoppingAnalysis = $state(false);
	/** The voiceover dialog: `null` while closed, else what it opens with. */
	voiceoverDialog = $state<{ prefill: VoiceoverPrefill | null } | null>(null);
	/** The Compose-music dialog: `null` while closed, else the piece it edits again (none: a fresh one). */
	composeDialog = $state<{ prefill: ComposePrefill | null } | null>(null);
	/** The Separate-stems dialog: `null` while closed, else the asset whose sound it splits and,
	 *  when opened from a clip, the clip the stems are laid under. */
	stemsDialog = $state<{ assetId: string; clipId: string | null } | null>(null);
	/** The Fit-to-video dialog: `null` while closed, else the music clip it plans a fit for. */
	fitMusicDialog = $state<{ clipId: string } | null>(null);
	/** The export dialog. Opened from the toolbar, ⌘E and the Deliver panel. */
	exportDialog = $state(false);
	/** The About dialog (Help › About Kerf). */
	aboutDialog = $state(false);
	/** The Relink-media dialog: opens on its own when a project comes up with media whose file is gone. */
	relinkDialog = $state(false);
	/** The read-only shortcut cheat sheet (`?`, Help › Keyboard shortcuts). */
	shortcutSheet = $state(false);
	/** The options the export dialog was last closed on, for the session (`export-draft.ts`). */
	exportDraft = $state.raw<ExportDraft | null>(null);
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
	/** The animated SVG title whose source the Inspector should open and focus: set when *Custom SVG…* makes
	 *  one (or a lane menu asks to edit it), cleared by the Inspector once it has taken the focus. */
	svgSourceFocus = $state<string | null>(null);
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
	/** The timeline seconds on screen, written by the timeline as it scrolls and zooms (`null`
	 *  while it is not mounted) — part of the editor context an agent can read. */
	timelineView = $state<{ start: number; end: number } | null>(null);
	/** The dock panel last activated (dockview's active panel), for the same context. */
	focusedPanel = $state<string | null>(null);
	#zoom = $state(ZOOM_DEFAULT);
	/** Timeline zoom, pixels per second (`zoom.ts` owns the range and the maths). Setting it is a
	 *  person taking the zoom, so the timeline stops fitting the cut to its window
	 *  (`zoomFollowsCut`); the timeline's own fit goes through `fitZoomTo`. */
	get zoom(): number {
		return this.#zoom;
	}
	set zoom(z: number) {
		this.#zoom = z;
		this.zoomFollowsCut = false;
	}
	/** Whether the zoom is still the timeline's own: fitted to the cut when it opens and as it
	 *  outgrows the window (`followZoom`), until anyone sets `zoom`. A new project starts it again. */
	zoomFollowsCut = $state(true);
	/** The timeline following the cut (`followZoom`): not a person's choice, so it keeps following. An
	 *  explicit Fit (⇧Z, the button) is one, and goes through the setter. */
	fitZoomTo(z: number) {
		this.#zoom = z;
	}
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
	/** The width the Preview asks its frames at (`null` before it has a box): the render bar
	 *  judges the cut at it. */
	previewWidth = $state<number | null>(null);
	/** System font family names available for the text overlay font picker. */
	availableFonts = $state<string[]>([]);

	#raf: FrameHandle | null = null;

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

	openCompose(prefill: ComposePrefill | null = null) {
		this.composeDialog = { prefill };
	}

	closeCompose() {
		this.composeDialog = null;
	}

	openAbout() {
		this.aboutDialog = true;
	}

	closeAbout() {
		this.aboutDialog = false;
	}

	openRelink() {
		this.relinkDialog = true;
	}

	closeRelink() {
		this.relinkDialog = false;
	}

	openShortcuts() {
		this.shortcutSheet = true;
	}

	closeShortcuts() {
		this.shortcutSheet = false;
	}

	openStems(assetId: string, clipId: string | null = null) {
		this.stemsDialog = { assetId, clipId };
	}

	closeStems() {
		this.stemsDialog = null;
	}

	openFitMusic(clipId: string) {
		this.fitMusicDialog = { clipId };
	}

	closeFitMusic() {
		this.fitMusicDialog = null;
	}

	openExport() {
		this.exportDialog = true;
	}

	closeExport() {
		this.exportDialog = false;
	}

	/** A different project was opened or created: what the session remembered about the last one
	 *  (the export options — a title, a codec — are not the new project's) is forgotten. */
	projectReplaced() {
		this.exportDraft = null;
		this.svgSourceFocus = null;
		this.zoomFollowsCut = true;
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

	/** Where the running pass is in what it was asked for, once it has said which step it is on. */
	get analysisStep(): StepCounter | null {
		return this.analyzing ? stepCounter(this.analysisPlan, counterKind(this.analysisStage?.stage)) : null;
	}

	/** A short label for the step analysis is on, or null when idle: `step 2 of 5 · detecting scenes 40%`. */
	get analysisLabel(): string | null {
		if (!this.analyzing) return null;
		const words = stepWords(this.analysisStep);
		const named = this.#stageLabel();
		return words ? `${words} · ${named}` : named;
	}

	#stageLabel(): string {
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
		this.modelProgress = null;
		this.cancellingModel = false;
		try {
			await downloadSpeechModel(name);
			await this.loadTranscriptionStatus();
		} catch (e) {
			// A stop the user asked for is not a failure.
			if (isModelCancelled(e)) toast.info(`Stopped downloading the ${name} speech model`);
			// A few hundred megabytes over someone else's CDN fails often enough
			// to be routine — offline, a proxy, a half-written cache file. It used
			// to reject into nothing, so the download simply stopped looking like
			// it was happening.
			else toast.error(`Couldn't download the ${name} speech model — ${message(e)}`);
		} finally {
			this.downloadingModel = null;
			this.modelFraction = 0;
			this.modelProgress = null;
			this.cancellingModel = false;
		}
	}

	/** A `model-progress` report from the backend (the user's download or an agent's). */
	noteModelProgress(p: ModelProgress) {
		this.modelProgress = p;
		this.modelFraction = p.fraction ?? 0;
	}

	/** Stop the speech model download in flight; it then rejects and `fetchSpeechModel` says so. */
	async stopModelDownload() {
		this.cancellingModel = true;
		try {
			await cancelModelDownload();
		} catch (e) {
			this.cancellingModel = false;
			toast.error(`Couldn't stop the download — ${message(e)}`);
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
		this.analysisPlan = steps ?? null;
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
			this.analysisPlan = null;
			this.stoppingAnalysis = false;
			await mediaStatus.refresh();
			this.reportFailedSteps(assetId, steps);
		}
	}

	/** Re-time an asset's transcript words and label its speakers, shown and stoppable like an
	 *  analysis pass. Resolves the speaker count, or null when it was stopped. */
	async refineTranscript(assetId: string): Promise<number | null> {
		this.analyzing = true;
		this.analyzingId = assetId;
		this.analysisStage = null;
		try {
			const analysis = await editor.refineTranscript(assetId);
			return new Set(analysis.transcript.map((s) => s.speaker).filter(Boolean)).size;
		} catch (e) {
			if (isCancelled(e)) return null;
			throw e;
		} finally {
			this.analyzing = false;
			this.analyzingId = null;
			this.analysisStage = null;
			this.stoppingAnalysis = false;
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
			.map((id) => {
				const asset = editor.assets.find((a) => a.id === id);
				const media = {
					audio: asset?.streams.some((s) => s.kind === 'audio') ?? true,
					image: asset?.streams.some((s) => s.image) ?? false
				};
				return { id, steps: autoSteps(auto, mediaStatus.analysis(id), available, media) };
			})
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

	/** Move the playhead to the frame `t` falls on, clamped to the timeline so it can't park past
	 *  the end or before zero. A scrub or a click lands on a frame of the cut, so a step from it is
	 *  a whole frame and the timecode, the picture and the playhead agree; the end of the cut itself
	 *  is kept as it is. Pass `{ snap: 'up' }` to go to the *start* of something (a title, a changed clip,
	 *  a transcript line): the first frame at or after it, so the playhead is inside it, not before it.
	 *  Re-anchors audio when it lands mid-playback. */
	seek(t: number, o?: SeekOptions) {
		this.time = seekTime(t, editor.fps, editor.duration, o);
		this.seekEpoch++;
		if (this.playing) this.#startAudio();
	}

	/** Put the playhead back exactly where it was (a scrub given up): the time as it stood, not
	 *  re-rounded to a frame — it may be mid-frame after playback — held to the cut as it is now. */
	restoreTime(t: number) {
		this.time = Math.min(Math.max(0, Number.isFinite(t) ? t : 0), Math.max(0, editor.duration));
		this.seekEpoch++;
		if (this.playing) this.#startAudio();
	}

	/** Jump to the nearest marker before (-1) or after (+1) the playhead. */
	gotoMarker(dir: 1 | -1) {
		// Compared where the playhead will stand on them (it lands on a frame): a marker between frames
		// is not still ahead of a playhead that has just gone to it.
		const at = stepPoint(editor.markers.map((m) => m.time), this.time, dir, editor.fps, editor.duration);
		if (at !== null) this.seek(at);
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
		windows.cancelFrame(this.#raf);
		if (rate > 0 && this.time >= editor.duration) this.time = 0;
		this.playing = true;
		this.rate = rate;
		this.seekEpoch++;
		this.#startAudio();
		let last = performance.now();
		// Frames come from whichever window is showing (a detached preview may be the
		// only one that is), whose timestamps count from its own start — so the clock
		// read is this window's, always.
		const step = () => {
			if (!this.playing) return;
			const now = performance.now();
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
			this.#raf = windows.requestFrame(step);
		};
		this.#raf = windows.requestFrame(step);
	}

	pause() {
		this.playing = false;
		this.rate = 1;
		audio.stop();
		windows.cancelFrame(this.#raf);
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
/** The rejection of a download the user stopped (`cancel_model_download`). */
export function isModelCancelled(e: unknown): boolean {
	return /cancel+ed/i.test(e instanceof Error ? e.message : String(e));
}

function message(e: unknown): string {
	return e instanceof Error ? e.message : String(e);
}

/** Whether a rejected analysis was cancelled rather than broken. The backend
 *  reports a stop as an ordinary error string; this is the one it uses. */
function isCancelled(e: unknown): boolean {
	return String(e instanceof Error ? e.message : e).includes('analysis cancelled');
}

export const ui = new EditorUi();
