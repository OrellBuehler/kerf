// Central editor state (Svelte 5 runes).

import type { Placement } from './api';
import {
	addClip,
	addKeyframe,
	addReframeKeyframe,
	addOverlay,
	analyzeAsset,
	generateCaptions,
	clearCaptions,
	clearKeyframes,
	clearReframe,
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
	setAssetProjection,
	setReframe,
	setReframeKeyframes,
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
	snapToBeats,
	smartCrop,
	removeTrack,
	setTrackDuck,
	setTrackVolume,
	setTrackPan,
	setDeliveryFormat,
	setTrackMuted,
	setTrackSolo,
	setTrackLocked,
	setClipEnabled,
	duplicateClips,
	insertClips,
	addMarker,
	updateMarker,
	removeMarker,
	reorderClip,
	rippleDelete,
	cutClipRange,
	revertTo as apiRevertTo,
	revisionDiff as apiRevisionDiff,
	applyStagedEdit,
	discardStagedEdit,
	getStagedEdit,
	getStagedTimeline,
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
	CaptionOptions,
	Clip,
	Color,
	Delivery,
	ExportOptions,
	Keyframe,
	Projection,
	Reframe,
	ReframeKeyframe,
	Revision,
	StagedEdit,
	StreamKind,
	TextKeyframe,
	Marker,
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
	/** The clip the Inspector edits — the primary of the selection. */
	selectedClipId = $state<string | null>(null);
	/** The whole selection. Always contains `selectedClipId` when that is set;
	 *  most edits act on the primary, but delete acts on all of these. */
	selectedClipIds = $state<string[]>([]);
	selectedOverlayId = $state<string | null>(null);
	selectedMetadata = $state<AssetMetadata | null>(null);
	analyses = $state<Record<string, AssetAnalysis>>({});
	history = $state<Revision[]>([]);
	/** The proposal a connected agent has staged, or null. */
	staged = $state<StagedEdit | null>(null);
	/** While true, `timeline` holds the *proposed* cut, not the live one. */
	previewingStaged = $state(false);
	currentPath = $state<string | null>(null);
	loading = $state(false);
	busy = $state(false);
	/** Whether media is currently being imported (drives the bin spinner). */
	importing = $state(false);
	/**
	 * Fraction done of a slow import (a 360 lens pair being stitched), or `null`
	 * when the import is an ordinary instant probe.
	 */
	importProgress = $state<number | null>(null);
	error = $state<string | null>(null);

	#waveforms = new Map<string, number[] | Promise<number[]>>();
	/** The live cut, parked while `previewingStaged` shows the proposal. */
	#liveTimeline: Timeline | null = null;

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

	/** Every selected clip that still exists, in timeline order. */
	get selectedClips(): Clip[] {
		const want = new Set(this.selectedClipIds);
		const out: Clip[] = [];
		for (const t of this.timeline.tracks) for (const c of t.clips) if (want.has(c.id)) out.push(c);
		return out.sort((a, b) => a.timeline_start - b.timeline_start);
	}

	isSelected(clipId: string): boolean {
		return this.selectedClipIds.includes(clipId);
	}

	/**
	 * Select a clip. `replace` (a plain click) drops the rest, `toggle`
	 * (ctrl/cmd-click) adds or removes it, and `range` (shift-click) takes
	 * everything between the primary and this clip on the same track.
	 */
	selectClip(clipId: string, mode: 'replace' | 'toggle' | 'range' = 'replace') {
		if (mode === 'toggle') {
			const has = this.selectedClipIds.includes(clipId);
			this.selectedClipIds = has
				? this.selectedClipIds.filter((id) => id !== clipId)
				: [...this.selectedClipIds, clipId];
			// Dropping the primary hands the Inspector whatever is left.
			if (has && this.selectedClipId === clipId) this.selectedClipId = this.selectedClipIds.at(-1) ?? null;
			else if (!has) this.selectedClipId = clipId;
			return;
		}
		if (mode === 'range' && this.selectedClipId) {
			const track = this.timeline.tracks.find((t) => t.clips.some((c) => c.id === clipId));
			const anchor = track?.clips.findIndex((c) => c.id === this.selectedClipId) ?? -1;
			const to = track?.clips.findIndex((c) => c.id === clipId) ?? -1;
			if (track && anchor >= 0 && to >= 0) {
				const [lo, hi] = anchor <= to ? [anchor, to] : [to, anchor];
				this.selectedClipIds = track.clips.slice(lo, hi + 1).map((c) => c.id);
				this.selectedClipId = clipId;
				return;
			}
		}
		this.selectedClipId = clipId;
		this.selectedClipIds = [clipId];
	}

	/** Select every clip on every unlocked track. */
	selectAll() {
		const ids = this.timeline.tracks.filter((t) => !t.locked).flatMap((t) => t.clips.map((c) => c.id));
		this.selectedClipIds = ids;
		this.selectedClipId = ids.at(-1) ?? null;
	}

	clearSelection() {
		this.selectedClipId = null;
		this.selectedClipIds = [];
	}

	/** Delete every selected clip as one user gesture. Ripple deletes run
	 *  right-to-left so each removal cannot shift the ones still to come. */
	async removeSelected(ripple: boolean): Promise<number> {
		const ids = ripple ? this.selectedClips.map((c) => c.id).reverse() : this.selectedClips.map((c) => c.id);
		if (ids.length === 0) return 0;
		this.clearSelection();
		for (const id of ids) await (ripple ? this.rippleDelete(id) : this.remove(id));
		return ids.length;
	}

	get overlays(): TextOverlay[] {
		return this.timeline.overlays ?? [];
	}

	/** Markers, kept sorted by time by the backend. */
	get markers(): Marker[] {
		return this.timeline.markers ?? [];
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
			this.previewingStaged = false;
			this.#liveTimeline = null;
			[this.assets, this.timeline, this.history, this.currentPath] = await Promise.all([
				listAssets(),
				getTimeline(),
				getHistory(),
				projectPath()
			]);
			await this.refreshStaged();
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
		this.selectedClipIds = [];
		await this.load();
		return true;
	}

	/** Open a `.kerf` file (native picker) and reload; resolves true if opened. */
	async openProject(): Promise<boolean> {
		const path = await apiOpenProject();
		if (path === null) return false; // cancelled, or running in the browser
		this.selectedAssetId = null;
		this.selectedClipId = null;
		this.selectedClipIds = [];
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
		const live = await getTimeline();
		// While the proposal is on screen the live cut is parked, not shown —
		// an agent edit landing mid-review must not yank the view out from
		// under the person reading it.
		if (this.previewingStaged) this.#liveTimeline = live;
		else this.timeline = live;
		await this.refreshStaged();
	}

	// ---- staged edits (the agent's pending proposal) ------------------------

	async refreshStaged() {
		try {
			this.staged = await getStagedEdit();
		} catch {
			this.staged = null;
		}
		if (!this.staged && this.previewingStaged) await this.exitStagedPreview();
		else if (this.staged && this.previewingStaged) {
			// The agent staged more while we were looking at it.
			const proposed = await getStagedTimeline();
			if (proposed) this.timeline = proposed;
		}
	}

	/** Show the proposed cut in the editor instead of the live one. */
	async previewStaged() {
		const proposed = await getStagedTimeline();
		if (!proposed) return;
		if (!this.previewingStaged) this.#liveTimeline = this.timeline;
		this.timeline = proposed;
		this.previewingStaged = true;
		this.clearSelection();
	}

	async exitStagedPreview() {
		if (!this.previewingStaged) return;
		this.previewingStaged = false;
		this.timeline = this.#liveTimeline ?? (await getTimeline());
		this.#liveTimeline = null;
		this.clearSelection();
	}

	/** Accept the proposal — it lands on the live timeline as one revision. */
	async applyStaged(force = false) {
		await this.#apply(applyStagedEdit(force));
		await this.refreshStaged();
	}

	/** Throw the proposal away; the live timeline is untouched. */
	async discardStaged() {
		await this.#apply(discardStagedEdit());
		await this.refreshStaged();
	}

	async refreshHistory() {
		try {
			this.history = await getHistory();
		} catch {
			/* history is best-effort; ignore */
		}
	}

	/** Pick one or more media files and import them. All files probe
	 *  concurrently (each lands in the bin as it resolves); imports continue
	 *  past a failed file and resolve to the successes plus per-file errors. */
	async importMedia(): Promise<{ imported: Asset[]; failed: { name: string; message: string }[] }> {
		const paths = await pickMediaPaths();
		if (paths.length === 0) return { imported: [], failed: [] };
		this.importing = true;
		this.error = null;
		const failed: { name: string; message: string }[] = [];
		try {
			const results = await Promise.all(
				paths.map(async (path) => {
					try {
						const asset = await importAsset(path);
						// Both lens files of a 360 capture resolve to one stitched
						// asset, so importing the pair must not list it twice.
						if (!this.assets.some((a) => a.id === asset.id)) {
							this.assets = [...this.assets, asset];
						}
						return asset;
					} catch (e) {
						failed.push({ name: path.split(/[\\/]/).pop() || path, message: this.#msg(e) });
						return null;
					}
				})
			);
			const imported = results.filter((a): a is Asset => a !== null);
			if (imported.length > 0) await this.select(imported[imported.length - 1].id);
			return { imported, failed };
		} finally {
			this.importing = false;
			this.importProgress = null;
		}
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

	/** Cached waveform peaks for an asset's audio. Single-flight: concurrent
	 *  callers for the same asset share one in-flight request instead of each
	 *  kicking off their own whole-file decode. */
	waveform(assetId: string, buckets: number): Promise<number[]> {
		const key = `${assetId}:${buckets}`;
		const cached = this.#waveforms.get(key);
		if (cached) return Promise.resolve(cached);
		const pending = getWaveform(assetId, buckets).then(
			(peaks) => {
				this.#waveforms.set(key, peaks);
				return peaks;
			},
			() => {
				// Transient failure: drop the entry so a later caller retries.
				this.#waveforms.delete(key);
				return [];
			}
		);
		this.#waveforms.set(key, pending);
		return pending;
	}

	// ---- editing actions (apply backend result to local timeline) -----------

	async #apply(op: Promise<Timeline>) {
		this.busy = true;
		this.error = null;
		// Any real edit is an edit to the live cut, so reviewing is over.
		this.previewingStaged = false;
		this.#liveTimeline = null;
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
	/**
	 * The clipboard holds clip *snapshots*, not ids, so cut-then-paste still
	 * works once the sources are gone — and the same clipboard can be pasted
	 * repeatedly, since the backend re-ids on every insert.
	 */
	clipboard = $state<Placement[]>([]);

	copySelection(): number {
		const want = new Set(this.selectedClipIds);
		const out: Placement[] = [];
		for (const t of this.timeline.tracks)
			for (const c of t.clips) if (want.has(c.id)) out.push({ track_id: t.id, clip: $state.snapshot(c) as Clip });
		this.clipboard = out.sort((a, b) => a.clip.timeline_start - b.clip.timeline_start);
		return this.clipboard.length;
	}

	/** Paste the clipboard so its earliest clip lands at `at`. */
	async paste(at: number): Promise<number> {
		if (this.clipboard.length === 0) return 0;
		await this.#apply(insertClips([...this.clipboard], at));
		return this.clipboard.length;
	}

	/** Duplicate the selection immediately after itself. */
	async duplicateSelection(): Promise<number> {
		const sel = this.selectedClips;
		if (sel.length === 0) return 0;
		const end = Math.max(...sel.map((c) => c.timeline_start + clipDuration(c)));
		await this.#apply(duplicateClips(sel.map((c) => c.id), end));
		return sel.length;
	}

	remove(clipId: string) {
		this.#forget(clipId);
		return this.#apply(removeClip(clipId));
	}
	rippleDelete(clipId: string) {
		this.#forget(clipId);
		return this.#apply(rippleDelete(clipId));
	}

	#forget(clipId: string) {
		if (this.selectedClipId === clipId) this.selectedClipId = null;
		this.selectedClipIds = this.selectedClipIds.filter((id) => id !== clipId);
	}
	cutRange(clipId: string, from: number, to: number) {
		return this.#apply(cutClipRange(clipId, from, to));
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
	setTrackVolume(trackId: string, volume: number) {
		return this.#apply(setTrackVolume(trackId, volume));
	}
	setTrackPan(trackId: string, pan: number) {
		return this.#apply(setTrackPan(trackId, pan));
	}
	/** The frame this project is cut for; `null` follows the footage's shape. */
	setDeliveryFormat(format: Delivery | null) {
		return this.#apply(setDeliveryFormat(format));
	}
	/** Add an auto-named marker at `time` — the M shortcut and the timeline menu. */
	addMarkerAtPlayhead(time: number) {
		return this.addMarker(time, `Marker ${this.markers.length + 1}`);
	}
	addMarker(time: number, name: string, color?: string) {
		return this.#apply(addMarker(time, name, color));
	}
	updateMarker(markerId: string, patch: { time?: number; name?: string; color?: string }) {
		return this.#apply(updateMarker(markerId, patch));
	}
	removeMarker(markerId: string) {
		return this.#apply(removeMarker(markerId));
	}
	setTrackMuted(trackId: string, muted: boolean) {
		return this.#apply(setTrackMuted(trackId, muted));
	}
	setTrackSolo(trackId: string, solo: boolean) {
		return this.#apply(setTrackSolo(trackId, solo));
	}
	setTrackLocked(trackId: string, locked: boolean) {
		return this.#apply(setTrackLocked(trackId, locked));
	}
	setClipEnabled(clipId: string, enabled: boolean) {
		return this.#apply(setClipEnabled(clipId, enabled));
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
	setReframe(clipId: string, patch: Partial<Reframe>) {
		return this.#apply(setReframe(clipId, patch));
	}
	/** Mark (or unmark) an asset as 360 footage; later cuts from it reframe. */
	async setAssetProjection(assetId: string, projection: Projection | null) {
		const asset = await setAssetProjection(assetId, projection);
		this.assets = this.assets.map((a) => (a.id === asset.id ? asset : a));
		if (this.selectedMetadata?.asset.id === asset.id) {
			this.selectedMetadata = { ...this.selectedMetadata, asset };
		}
		return asset;
	}
	clearReframe(clipId: string) {
		return this.#apply(clearReframe(clipId));
	}
	setReframeKeyframes(clipId: string, keyframes: ReframeKeyframe[]) {
		return this.#apply(setReframeKeyframes(clipId, keyframes));
	}
	addReframeKeyframe(
		clipId: string,
		time: number,
		patch: Partial<Omit<ReframeKeyframe, 'time'>> = {}
	) {
		return this.#apply(addReframeKeyframe(clipId, time, patch));
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
	generateCaptions(options?: CaptionOptions) {
		return this.#apply(generateCaptions(options));
	}
	clearCaptions() {
		return this.#apply(clearCaptions());
	}
	/** Write the asset's transcript to `.srt`; returns the path (no timeline change). */
	exportSrt(assetId: string, outputPath: string) {
		return exportSrt(assetId, outputPath);
	}
	removeSilence(assetId: string) {
		return this.#apply(removeSilence(assetId));
	}
	/** Ripple a track's cuts onto the music's beat grid; all video tracks by default. */
	snapToBeats(trackId?: string, tolerance?: number) {
		return this.#apply(snapToBeats(trackId, tolerance));
	}
	/**
	 * Frame each shot for the delivery frame instead of centring it blindly.
	 * One clip when `clipId` is given, otherwise every clip on an unlocked video
	 * track. Lands as one undoable `Smart crop` revision.
	 */
	smartCrop(clipId?: string) {
		return this.#apply(smartCrop(clipId));
	}
	extractAudio(assetId: string) {
		return this.#apply(extractAudio(assetId));
	}
	concatenate(assetIds: string[]) {
		return this.#apply(concatenate(assetIds));
	}

	// ---- history (undo / redo / revert) -------------------------------------

	undo() {
		this.clearSelection();
		return this.#apply(apiUndo());
	}
	redo() {
		this.clearSelection();
		return this.#apply(apiRedo());
	}
	revertTo(seq: number) {
		this.clearSelection();
		return this.#apply(apiRevertTo(seq));
	}
	/** What one revision changed; null where the backend can't say (browser). */
	revisionDiff(seq: number) {
		return apiRevisionDiff(seq);
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
