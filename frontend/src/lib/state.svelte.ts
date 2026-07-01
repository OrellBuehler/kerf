// Central editor state (Svelte 5 runes).

import {
	addClip,
	addKeyframe,
	addOverlay,
	analyzeAsset,
	captionsFromTranscript,
	clearKeyframes,
	concatenate,
	cutClip,
	exportSrt,
	exportTimeline,
	extractAudio,
	getAssetMetadata,
	getHistory,
	getTimeline,
	getWaveform,
	importAsset,
	listAssets,
	addTrack,
	moveClip,
	removeOverlay,
	setAudioEffects,
	setKeyframes,
	setOverlayKeyframes,
	setVideoEffects,
	updateOverlay,
	newProject as apiNewProject,
	openProject as apiOpenProject,
	pickMediaPaths,
	projectPath,
	redo as apiRedo,
	removeClip,
	removeSilence,
	removeTrack,
	setTrackDuck,
	reorderClip,
	rippleDelete,
	revertTo as apiRevertTo,
	saveProjectAs as apiSaveProjectAs,
	setColor,
	setFade,
	setSpeed,
	setTransform,
	setTransition,
	setVolume,
	splitClip,
	trimClip,
	undo as apiUndo
} from './api';
import type {
	Asset,
	AssetAnalysis,
	AssetMetadata,
	AudioEffect,
	Clip,
	Color,
	ExportOptions,
	Keyframe,
	Revision,
	StreamKind,
	TextKeyframe,
	TextOverlay,
	Timeline,
	Transform,
	Transition,
	VideoEffect
} from './types';
import { clipDuration } from './types';

class EditorState {
	assets = $state<Asset[]>([]);
	timeline = $state<Timeline>({ tracks: [] });
	selectedAssetId = $state<string | null>(null);
	selectedClipId = $state<string | null>(null);
	selectedOverlayId = $state<string | null>(null);
	selectedMetadata = $state<AssetMetadata | null>(null);
	analyses = $state<Record<string, AssetAnalysis>>({});
	history = $state<Revision[]>([]);
	currentPath = $state<string | null>(null);
	loading = $state(false);
	busy = $state(false);
	/** Whether media is currently being imported (drives the bin spinner). */
	importing = $state(false);
	error = $state<string | null>(null);

	#waveforms = new Map<string, number[]>();

	get selectedAsset(): Asset | undefined {
		return this.assets.find((a) => a.id === this.selectedAssetId);
	}

	get selectedClip(): Clip | undefined {
		for (const t of this.timeline.tracks) {
			const c = t.clips.find((c) => c.id === this.selectedClipId);
			if (c) return c;
		}
		return undefined;
	}

	get overlays(): TextOverlay[] {
		return this.timeline.overlays ?? [];
	}

	get selectedOverlay(): TextOverlay | undefined {
		return this.overlays.find((o) => o.id === this.selectedOverlayId);
	}

	/** Timeline length in seconds. Memoized: `timeline` is reassigned wholesale
	 *  on every edit, so this recomputes only then — not on every playhead tick
	 *  (the rAF playback loop reads it ~60×/sec). */
	duration = $derived.by(() => {
		let max = 0;
		for (const t of this.timeline.tracks) {
			for (const c of t.clips) max = Math.max(max, c.timeline_start + clipDuration(c));
		}
		return max;
	});

	/** Whether the project is backed by a file on disk (vs the in-memory sample). */
	get saved(): boolean {
		return this.currentPath !== null;
	}

	/** File name of the open project, or a placeholder when unsaved. */
	get projectName(): string {
		if (!this.currentPath) return 'Untitled project';
		const parts = this.currentPath.split(/[\\/]/);
		return parts[parts.length - 1] || this.currentPath;
	}

	get canUndo(): boolean {
		const i = this.history.findIndex((r) => r.current);
		return i > 0;
	}

	get canRedo(): boolean {
		const i = this.history.findIndex((r) => r.current);
		return i >= 0 && i < this.history.length - 1;
	}

	assetName(assetId: string): string {
		return this.assets.find((a) => a.id === assetId)?.name ?? 'unknown';
	}

	analysisFor(assetId: string): AssetAnalysis | undefined {
		return this.analyses[assetId];
	}

	async load() {
		this.loading = true;
		this.error = null;
		try {
			[this.assets, this.timeline, this.history, this.currentPath] = await Promise.all([
				listAssets(),
				getTimeline(),
				getHistory(),
				projectPath()
			]);
			if (!this.selectedAssetId && this.assets.length > 0) {
				await this.select(this.assets[0].id);
			}
		} catch (e) {
			this.error = this.#msg(e);
		} finally {
			this.loading = false;
		}
	}

	// ---- project file (new / open / save) -----------------------------------

	/** Discard the open project for a fresh, empty one; resolves true if Tauri. */
	async newProject(): Promise<boolean> {
		if (!(await apiNewProject())) return false; // running in the browser
		this.selectedAssetId = null;
		this.selectedClipId = null;
		await this.load();
		return true;
	}

	/** Open a `.kerf` file (native picker) and reload; resolves true if opened. */
	async openProject(): Promise<boolean> {
		const path = await apiOpenProject();
		if (path === null) return false; // cancelled, or running in the browser
		this.selectedAssetId = null;
		this.selectedClipId = null;
		await this.load();
		return true;
	}

	/** Persist the project to a chosen `.kerf` file; resolves true if saved. */
	async saveProjectAs(): Promise<boolean> {
		const path = await apiSaveProjectAs(this.currentPath ?? undefined);
		if (path === null) return false;
		this.currentPath = path;
		return true;
	}

	async select(assetId: string) {
		this.selectedAssetId = assetId;
		try {
			this.selectedMetadata = await getAssetMetadata(assetId);
			if (this.selectedMetadata.analysis) this.analyses[assetId] = this.selectedMetadata.analysis;
		} catch {
			this.selectedMetadata = null;
		}
	}

	async refreshTimeline() {
		this.timeline = await getTimeline();
	}

	async refreshHistory() {
		try {
			this.history = await getHistory();
		} catch {
			/* history is best-effort; ignore */
		}
	}

	/** Pick one or more media files and import them. Imports continue past a
	 *  failed file; resolves to the assets that succeeded plus per-file errors. */
	async importMedia(): Promise<{ imported: Asset[]; failed: { name: string; message: string }[] }> {
		const paths = await pickMediaPaths();
		if (paths.length === 0) return { imported: [], failed: [] };
		this.importing = true;
		this.error = null;
		const imported: Asset[] = [];
		const failed: { name: string; message: string }[] = [];
		try {
			for (const path of paths) {
				try {
					const asset = await importAsset(path);
					this.assets = [...this.assets, asset];
					imported.push(asset);
				} catch (e) {
					failed.push({ name: path.split(/[\\/]/).pop() || path, message: this.#msg(e) });
				}
			}
			if (imported.length > 0) await this.select(imported[imported.length - 1].id);
		} finally {
			this.importing = false;
		}
		return { imported, failed };
	}

	/** Run analysis on an asset and merge the result into local caches. */
	async analyze(assetId: string): Promise<AssetAnalysis> {
		const analysis = await analyzeAsset(assetId);
		this.analyses[assetId] = analysis;
		if (assetId === this.selectedAssetId && this.selectedMetadata) {
			this.selectedMetadata = { ...this.selectedMetadata, analysis };
		}
		return analysis;
	}

	/** Cached waveform peaks for an asset's audio. */
	async waveform(assetId: string, buckets: number): Promise<number[]> {
		const key = `${assetId}:${buckets}`;
		const cached = this.#waveforms.get(key);
		if (cached) return cached;
		try {
			const peaks = await getWaveform(assetId, buckets);
			this.#waveforms.set(key, peaks);
			return peaks;
		} catch {
			return [];
		}
	}

	// ---- editing actions (apply backend result to local timeline) -----------

	async #apply(op: Promise<Timeline>) {
		this.busy = true;
		this.error = null;
		try {
			this.timeline = await op;
			await this.refreshHistory();
		} catch (e) {
			this.error = this.#msg(e);
			throw e;
		} finally {
			this.busy = false;
		}
	}

	cut(assetId: string, start: number, end: number) {
		return this.#apply(cutClip(assetId, start, end));
	}
	add(assetId: string, sourceIn: number, sourceOut: number, trackId?: string, timelineStart?: number) {
		return this.#apply(addClip(assetId, sourceIn, sourceOut, trackId, timelineStart));
	}
	split(clipId: string, at: number) {
		return this.#apply(splitClip(clipId, at));
	}
	trim(clipId: string, sourceIn?: number, sourceOut?: number, timelineStart?: number) {
		return this.#apply(trimClip(clipId, sourceIn, sourceOut, timelineStart));
	}
	reorder(trackId: string, clipId: string, newIndex: number) {
		return this.#apply(reorderClip(trackId, clipId, newIndex));
	}
	move(clipId: string, timelineStart: number, trackId?: string) {
		return this.#apply(moveClip(clipId, timelineStart, trackId));
	}
	remove(clipId: string) {
		if (this.selectedClipId === clipId) this.selectedClipId = null;
		return this.#apply(removeClip(clipId));
	}
	rippleDelete(clipId: string) {
		if (this.selectedClipId === clipId) this.selectedClipId = null;
		return this.#apply(rippleDelete(clipId));
	}
	addTrack(kind: StreamKind, name?: string) {
		return this.#apply(addTrack(kind, name));
	}
	removeTrack(trackId: string) {
		return this.#apply(removeTrack(trackId));
	}
	setTrackDuck(trackId: string, duck: boolean) {
		return this.#apply(setTrackDuck(trackId, duck));
	}
	setVolume(clipId: string, volume: number) {
		return this.#apply(setVolume(clipId, volume));
	}
	setFade(clipId: string, fadeIn?: number, fadeOut?: number) {
		return this.#apply(setFade(clipId, fadeIn, fadeOut));
	}
	setSpeed(clipId: string, speed: number) {
		return this.#apply(setSpeed(clipId, speed));
	}
	setTransform(clipId: string, patch: Partial<Transform>) {
		return this.#apply(setTransform(clipId, patch));
	}
	setColor(clipId: string, patch: Partial<Color>) {
		return this.#apply(setColor(clipId, patch));
	}
	setTransition(clipId: string, transition: Transition | null) {
		return this.#apply(setTransition(clipId, transition));
	}
	setVideoEffects(clipId: string, effects: VideoEffect[]) {
		return this.#apply(setVideoEffects(clipId, effects));
	}
	setAudioEffects(clipId: string, effects: AudioEffect[]) {
		return this.#apply(setAudioEffects(clipId, effects));
	}
	setKeyframes(clipId: string, keyframes: Keyframe[]) {
		return this.#apply(setKeyframes(clipId, keyframes));
	}
	addKeyframe(clipId: string, time: number, patch: Partial<Omit<Keyframe, 'time'>> = {}) {
		return this.#apply(addKeyframe(clipId, time, patch));
	}
	clearKeyframes(clipId: string) {
		return this.#apply(clearKeyframes(clipId));
	}
	addOverlay(text: string, start: number, end: number) {
		return this.#apply(addOverlay(text, start, end));
	}
	updateOverlay(overlayId: string, patch: Partial<Omit<TextOverlay, 'id' | 'keyframes'>>) {
		return this.#apply(updateOverlay(overlayId, patch));
	}
	removeOverlay(overlayId: string) {
		if (this.selectedOverlayId === overlayId) this.selectedOverlayId = null;
		return this.#apply(removeOverlay(overlayId));
	}
	setOverlayKeyframes(overlayId: string, keyframes: TextKeyframe[]) {
		return this.#apply(setOverlayKeyframes(overlayId, keyframes));
	}
	captionsFromTranscript(assetId: string) {
		return this.#apply(captionsFromTranscript(assetId));
	}
	/** Write the asset's transcript to `.srt`; returns the path (no timeline change). */
	exportSrt(assetId: string, outputPath: string) {
		return exportSrt(assetId, outputPath);
	}
	removeSilence(assetId: string) {
		return this.#apply(removeSilence(assetId));
	}
	extractAudio(assetId: string) {
		return this.#apply(extractAudio(assetId));
	}
	concatenate(assetIds: string[]) {
		return this.#apply(concatenate(assetIds));
	}

	// ---- history (undo / redo / revert) -------------------------------------

	undo() {
		this.selectedClipId = null;
		return this.#apply(apiUndo());
	}
	redo() {
		this.selectedClipId = null;
		return this.#apply(apiRedo());
	}
	revertTo(seq: number) {
		this.selectedClipId = null;
		return this.#apply(apiRevertTo(seq));
	}

	async export(outputPath: string, options: ExportOptions): Promise<string> {
		this.busy = true;
		try {
			return await exportTimeline(outputPath, options);
		} finally {
			this.busy = false;
		}
	}

	#msg(e: unknown): string {
		return e instanceof Error ? e.message : String(e);
	}
}

export const editor = new EditorState();
