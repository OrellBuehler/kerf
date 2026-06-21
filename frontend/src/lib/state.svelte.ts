// Central editor state (Svelte 5 runes).

import {
	addClip,
	analyzeAsset,
	concatenate,
	cutClip,
	exportTimeline,
	extractAudio,
	getAssetMetadata,
	getHistory,
	getTimeline,
	getWaveform,
	listAssets,
	openProject as apiOpenProject,
	pickAndImport,
	projectPath,
	redo as apiRedo,
	removeClip,
	removeSilence,
	reorderClip,
	revertTo as apiRevertTo,
	saveProjectAs as apiSaveProjectAs,
	setVolume,
	splitClip,
	trimClip,
	undo as apiUndo
} from './api';
import type { Asset, AssetAnalysis, AssetMetadata, Clip, Revision, Timeline } from './types';

class EditorState {
	assets = $state<Asset[]>([]);
	timeline = $state<Timeline>({ tracks: [] });
	selectedAssetId = $state<string | null>(null);
	selectedClipId = $state<string | null>(null);
	selectedMetadata = $state<AssetMetadata | null>(null);
	analyses = $state<Record<string, AssetAnalysis>>({});
	history = $state<Revision[]>([]);
	currentPath = $state<string | null>(null);
	loading = $state(false);
	busy = $state(false);
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

	get duration(): number {
		let max = 0;
		for (const t of this.timeline.tracks) {
			for (const c of t.clips) max = Math.max(max, c.timeline_start + Math.max(0, c.source_out - c.source_in));
		}
		return max;
	}

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

	// ---- project file (open / save) -----------------------------------------

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

	async importMedia(): Promise<Asset | null> {
		const asset = await pickAndImport();
		if (asset) {
			this.assets = [...this.assets, asset];
			await this.select(asset.id);
		}
		return asset;
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
	trim(clipId: string, sourceIn?: number, sourceOut?: number) {
		return this.#apply(trimClip(clipId, sourceIn, sourceOut));
	}
	reorder(trackId: string, clipId: string, newIndex: number) {
		return this.#apply(reorderClip(trackId, clipId, newIndex));
	}
	remove(clipId: string) {
		if (this.selectedClipId === clipId) this.selectedClipId = null;
		return this.#apply(removeClip(clipId));
	}
	setVolume(clipId: string, volume: number) {
		return this.#apply(setVolume(clipId, volume));
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

	async export(outputPath: string, format: string): Promise<string> {
		this.busy = true;
		try {
			return await exportTimeline(outputPath, format);
		} finally {
			this.busy = false;
		}
	}

	#msg(e: unknown): string {
		return e instanceof Error ? e.message : String(e);
	}
}

export const editor = new EditorState();
