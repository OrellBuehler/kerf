// Central editor state (Svelte 5 runes).

import type { AudioDetached, Placement } from './api';
import {
	addClip,
	addKeyframe,
	addReframeKeyframe,
	addOverlay,
	analyzeAsset,
	generateCaptions,
	clearCaptions,
	importCaptions,
	importCaptionsText,
	clearKeyframes,
	setKeyframeEasing,
	clearReframe,
	concatenate,
	cutClip,
	exportSrt,
	exportTimeline,
	cancelExport,
	onExportProgress,
	exportVariants,
	addAssetAudio,
	detachAudio,
	detachAudioClips,
	extractAudio,
	getAssetMetadata,
	getHistory,
	getRippleMode,
	getTimeline,
	importAsset,
	listAssets,
	addTrack,
	moveClip,
	moveClips,
	removeOverlay,
	setAudioEffects,
	setKeyframes,
	setPropertyKeyframes,
	copyKeyframes,
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
	removeClips,
	removeSilence,
	snapToBeats,
	smartCrop,
	removeTrack,
	setTrackDuck,
	setMask,
	setTrackVolume,
	setTrackPan,
	setMasterVolume,
	setMasterLimiter,
	setMasterDuck,
	fitMusic,
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
	linkClips,
	reattachAudio,
	reattachAudioClips,
	reorderClip,
	rippleDelete,
	cutClipRange,
	rollEdit,
	slipClip,
	slideClip,
	splitRemove,
	splitRemoveClips,
	unlinkClips,
	revertTo as apiRevertTo,
	revisionDiff as apiRevisionDiff,
	applyStagedEdit,
	discardStagedEdit,
	getStagedEdit,
	getStagedTimeline,
	saveProjectAs as apiSaveProjectAs,
	setColor,
	setFade,
	setRippleMode,
	setSpeed,
	setTransform,
	setTransition,
	setVolume,
	splitClip,
	trimClip,
	generateVoiceover,
	separateStems,
	undo as apiUndo
} from './api';
import type {
	ClipCut,
	ClipMove,
	SplitSide,
	ExportProgress,
	Asset,
	AnalysisKind,
	AssetAnalysis,
	AssetMetadata,
	AudioEffect,
	CaptionFormat,
	CaptionImportRequest,
	CaptionImportResult,
	CaptionImportSummary,
	CaptionOptions,
	Clip,
	Color,
	Delivery,
	ExportOptions,
	Easing,
	Keyframe,
	Mask,
	MusicFitReport,
	Projection,
	Property,
	PropertyKey,
	Reframe,
	ReframeKeyframe,
	Revision,
	StagedEdit,
	StemsPlaced,
	StreamKind,
	TextKeyframe,
	Marker,
	TextOverlay,
	Timeline,
	Transform,
	Transition,
	VideoEffect,
	VoiceoverRequest
} from './types';
import { clipDuration } from './types';
import { timelineFps } from './timecode';
import { Generation } from './generation';
import { retimeKeyframes, withKeyframeAt } from './titles';
import { normalize, pruneSelection, type PickMode } from './selection';
import { clickSelectLinked } from './link-ui';
import { locateIndex, withLinkPartners } from './link-groups';

class EditorState {
	assets = $state<Asset[]>([]);
	timeline = $state<Timeline>({ tracks: [] });
	selectedAssetId = $state<string | null>(null);
	/** The clip the Inspector edits — the primary of the selection. */
	selectedClipId = $state<string | null>(null);
	/** The whole selection. Always contains `selectedClipId` when that is set;
	 *  most edits act on the primary, but delete and a drag act on all of these.
	 *  Every change goes through `selection.ts`, so a click, a Shift-click and a
	 *  marquee agree on what they leave behind. */
	selectedClipIds = $state<string[]>([]);
	/** Ripple mode: with it on, an edit that changes how much footage sits ahead
	 *  of a clip (a trim, a delete, a speed change) pulls the later clips on that
	 *  track along. A property of the project, held by the backend; this is the
	 *  copy the toolbar reads. */
	rippleMode = $state(false);
	/** The title being edited. Exclusive with the clip selection: a title is its
	 *  own item on the titles lane, not a property of whichever clip is selected. */
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
	/** Count of edits/exports currently in flight, not a flag — two overlapping
	 *  operations (an edit fired while an export streams, or two edits in quick
	 *  succession) must not have the first to finish turn this off while the
	 *  other is still running. */
	#busyCount = $state(0);
	get busy(): boolean {
		return this.#busyCount > 0;
	}
	/** Whether media is currently being imported (drives the bin spinner). */
	importing = $state(false);
	/**
	 * Fraction done of a slow import (a 360 lens pair being stitched), or `null`
	 * when the import is an ordinary instant probe.
	 */
	importProgress = $state<number | null>(null);
	error = $state<string | null>(null);

	/** The live cut, parked while `previewingStaged` shows the proposal. */
	#liveTimeline = $state.raw<Timeline | null>(null);
	/** The cut an edit lands on: the live one even while a proposal is on screen
	 *  (every edit drops the preview first). `timeline` is what is *shown*. */
	get liveTimeline(): Timeline {
		return this.#liveTimeline ?? this.timeline;
	}
	/** Snapshot guard over `timeline` — every writer bumps it (via `#setTimeline`)
	 *  so a `refreshTimeline()` fetch that started earlier can tell a newer write
	 *  already landed while it was waiting and skip clobbering it. */
	#timelineGen = new Generation();
	/** The single place `timeline` is assigned, so every writer bumps the
	 *  generation above. */
	#setTimeline(tl: Timeline) {
		this.timeline = tl;
		if (this.selectedOverlayId && !(tl.overlays ?? []).some((o) => o.id === this.selectedOverlayId)) {
			this.selectedOverlayId = null;
		}
		// A clip another edit removed (an undo, an agent) is no longer selected —
		// the Inspector would otherwise keep a clip that is not there.
		if (this.selectedClipIds.length > 0) {
			const here = new Set(tl.tracks.flatMap((t) => t.clips.map((c) => c.id)));
			const now = pruneSelection(this.#selection(), (id) => here.has(id));
			if (now.ids.length !== this.selectedClipIds.length) this.#setSelection(now);
		}
		this.#timelineGen.advance();
	}
	/** Sequence guard over `select()` — a slow metadata fetch for an earlier
	 *  click must not overwrite a faster, newer one. */
	#selectGen = new Generation();

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

	#selection() {
		return { ids: this.selectedClipIds, primary: this.selectedClipId };
	}

	#setSelection(sel: { ids: readonly string[]; primary: string | null }) {
		this.selectedClipIds = [...sel.ids];
		this.selectedClipId = sel.primary;
	}

	/**
	 * Select a clip. `replace` (a plain click) drops the rest, `toggle`
	 * (ctrl/cmd-click) adds or removes it, and `range` (shift-click) extends the
	 * selection with everything between the primary and this clip on the same
	 * track (just the clip, when the primary is elsewhere). A linked clip takes its
	 * partners with it — pick the picture and its sound is picked too — unless
	 * `alone` (an Alt-click): see `clickSelectLinked`.
	 */
	selectClip(clipId: string, mode: PickMode = 'replace', alone = false) {
		this.selectedOverlayId = null;
		const track = this.timeline.tracks.find((t) => t.clips.some((c) => c.id === clipId));
		this.#setSelection(
			clickSelectLinked(this.timeline, this.#selection(), clipId, mode, track ? track.clips.map((c) => c.id) : null, alone)
		);
	}

	/** Replace the whole selection (what a marquee ends on). The primary stays a
	 *  member: `primary` when it is one, else the last. */
	selectClips(ids: readonly string[], primary: string | null = null) {
		this.selectedOverlayId = null;
		this.#setSelection(normalize(ids, primary));
	}

	/** Hand the Inspector another clip of the selection, leaving the set alone. A
	 *  clip that is not selected is ignored. */
	setPrimary(clipId: string) {
		if (this.selectedClipIds.includes(clipId)) this.selectedClipId = clipId;
	}

	/** Select every clip on every unlocked track. */
	selectAll() {
		const ids = this.timeline.tracks.filter((t) => !t.locked).flatMap((t) => t.clips.map((c) => c.id));
		this.selectedOverlayId = null;
		this.selectedClipIds = ids;
		this.selectedClipId = ids.at(-1) ?? null;
	}

	clearSelection() {
		this.selectedClipId = null;
		this.selectedClipIds = [];
		this.selectedOverlayId = null;
	}

	/** Select a title, dropping the clip selection so the Inspector shows one thing. */
	selectOverlay(overlayId: string | null) {
		this.selectedOverlayId = overlayId;
		if (overlayId) {
			this.selectedClipId = null;
			this.selectedClipIds = [];
		}
	}

	/**
	 * Delete every selected clip as ONE edit (`remove_clips`: one revision, so one
	 * undo). `ripple` closes the gap behind what was removed on every track it
	 * touched (Shift+Delete); without it the project's ripple mode decides, as for
	 * any other delete. A clip on a locked track is left where it is — and stays
	 * selected — rather than refusing the rest, and is counted in `skipped`.
	 */
	async removeSelected(ripple: boolean): Promise<{ removed: number; skipped: number }> {
		const locked = new Set(this.timeline.tracks.filter((t) => t.locked).flatMap((t) => t.clips.map((c) => c.id)));
		const clips = this.selectedClips;
		const ids = clips.filter((c) => !locked.has(c.id)).map((c) => c.id);
		const skipped = clips.length - ids.length;
		if (ids.length === 0) return { removed: 0, skipped };
		// A linked clip takes its partners with it, and a partner on a locked track refuses the whole
		// removal (a linked edit is a group edit) — said before anything is touched, not as a bare
		// "track A1 is locked" from the backend, with the way round it.
		for (const id of withLinkPartners(this.timeline, ids)) {
			const at = locateIndex(this.timeline, id);
			if (at && this.timeline.tracks[at[0]].locked && !ids.includes(id)) {
				throw new Error(
					`A linked clip is on locked track ${this.timeline.tracks[at[0]].name} — unlock it, or use “Remove only this clip” in the clip menu`
				);
			}
		}
		const kept = clips.filter((c) => locked.has(c.id)).map((c) => c.id);
		this.selectClips(kept, kept.includes(this.selectedClipId ?? '') ? this.selectedClipId : null);
		await this.#apply(removeClips(ids, ripple ? true : undefined));
		return { removed: ids.length, skipped };
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

	/** Source frame rate of the cut. Memoized: playback reads it every animation
	 *  frame and `timelineFps` scans every track and clip. */
	fps = $derived.by(() => timelineFps(this.timeline, this.assets));

	/** Whether the project is backed by a file on disk (vs the in-memory sample). */
	get saved(): boolean {
		return this.currentPath !== null;
	}

	/** Work that exists only in memory: a never-saved project that has media or
	 *  a cut in it. Once saved, a project is a SQLite file and every edit is
	 *  committed as it happens, so only this state can be lost by closing,
	 *  relaunching or replacing the project. */
	get hasUnsavedWork(): boolean {
		if (this.saved) return false;
		return (
			this.assets.length > 0 ||
			(this.timeline.overlays?.length ?? 0) > 0 ||
			this.timeline.tracks.some((t) => t.clips.length > 0)
		);
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

	/** Assets whose cached analysis has been asked for, so a never-analyzed one is asked once. */
	#analysisAsked = new Set<string>();

	/** Load the cached analysis of assets nobody has selected or analyzed yet — the ones the audio tracks
	 *  play, whose tempo and bar grid draw the ruler and whose music analysis offers *Fit to video*.
	 *  Each is asked for once (an unanalyzed asset answers `null`; `analyze` fills it in later); a failed
	 *  read is asked again next time. Never overwrites an analysis already here. */
	async ensureAnalyses(assetIds: Iterable<string>) {
		const wanted = [...assetIds].filter((id) => !this.analyses[id] && !this.#analysisAsked.has(id));
		for (const id of wanted) this.#analysisAsked.add(id);
		await Promise.all(
			wanted.map(async (id) => {
				try {
					const { analysis } = await getAssetMetadata(id);
					if (analysis && !this.analyses[id]) this.analyses[id] = analysis;
				} catch {
					this.#analysisAsked.delete(id);
				}
			})
		);
	}

	async load() {
		this.loading = true;
		this.error = null;
		try {
			this.previewingStaged = false;
			this.#liveTimeline = null;
			this.#analysisAsked.clear();
			// The ripple flag is the project's, saved in its file: a project that was
			// saved with ripple on has to open that way, and a new one has to reset
			// the toggle, so it is read with everything else every load. (It never
			// rejects — a failed read keeps what was showing.)
			const [assets, timeline, history, currentPath] = await Promise.all([
				listAssets(),
				getTimeline(),
				getHistory(),
				projectPath(),
				this.loadRippleMode()
			]);
			this.assets = assets;
			this.#setTimeline(timeline);
			this.history = history;
			this.currentPath = currentPath;
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
		this.selectedOverlayId = null;
		await this.load();
		return true;
	}

	/** Open a `.kerf` file (`path`, else the native picker) and reload; resolves true if opened. */
	async openProject(file?: string): Promise<boolean> {
		const path = await apiOpenProject(file);
		if (path === null) return false; // cancelled, or running in the browser
		this.selectedAssetId = null;
		this.selectedClipId = null;
		this.selectedClipIds = [];
		this.selectedOverlayId = null;
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
		const seq = this.#selectGen.advance();
		try {
			const metadata = await getAssetMetadata(assetId);
			if (!this.#selectGen.isCurrent(seq)) return; // a newer select() has since started
			this.selectedMetadata = metadata;
			if (metadata.analysis) this.analyses[assetId] = metadata.analysis;
		} catch {
			if (!this.#selectGen.isCurrent(seq)) return;
			this.selectedMetadata = null;
		}
	}

	async refreshTimeline() {
		const snapshot = this.#timelineGen.read();
		const live = await getTimeline();
		// While the proposal is on screen the live cut is parked, not shown —
		// an agent edit landing mid-review must not yank the view out from
		// under the person reading it.
		if (this.previewingStaged) {
			this.#liveTimeline = live;
		} else if (this.#timelineGen.isCurrent(snapshot)) {
			// Nothing else assigned `timeline` while this fetch was in flight. If a
			// local edit committed meanwhile, it bumped the generation, so this
			// stale snapshot loses and is dropped instead of clobbering it.
			this.#setTimeline(live);
		}
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
			if (proposed) this.#setTimeline(proposed);
		}
	}

	/** Show the proposed cut in the editor instead of the live one. */
	async previewStaged() {
		const proposed = await getStagedTimeline();
		if (!proposed) return;
		if (!this.previewingStaged) this.#liveTimeline = this.timeline;
		this.#setTimeline(proposed);
		this.previewingStaged = true;
		this.clearSelection();
	}

	async exitStagedPreview() {
		if (!this.previewingStaged) return;
		this.previewingStaged = false;
		this.#setTimeline(this.#liveTimeline ?? (await getTimeline()));
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
		return this.importPaths(await pickMediaPaths());
	}

	/** Import specific files — what the picker resolves to, and what a drop onto
	 *  the window hands over. */
	async importPaths(paths: string[]): Promise<{ imported: Asset[]; failed: { name: string; message: string }[] }> {
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

	/** Re-read the asset list, for assets an agent brought in behind our back
	 *  (a voiceover it generated). Left alone when nothing is new, so the bin
	 *  does not re-render on every agent edit. */
	async refreshAssets() {
		const assets = await listAssets();
		const known = new Set(this.assets.map((a) => a.id));
		if (assets.length !== known.size || assets.some((a) => !known.has(a.id))) this.assets = assets;
	}

	/** Run analysis (the named `steps`, else what Settings leaves switched on) on an asset and
	 *  merge the result into local caches. */
	async analyze(assetId: string, steps?: readonly AnalysisKind[]): Promise<AssetAnalysis> {
		const analysis = await analyzeAsset(assetId, steps);
		this.analyses[assetId] = analysis;
		if (assetId === this.selectedAssetId && this.selectedMetadata) {
			this.selectedMetadata = { ...this.selectedMetadata, analysis };
		}
		return analysis;
	}

	// ---- editing actions (apply backend result to local timeline) -----------

	async #apply(op: Promise<Timeline>) {
		this.#busyCount++;
		this.error = null;
		// Any real edit is an edit to the live cut, so reviewing is over.
		this.previewingStaged = false;
		this.#liveTimeline = null;
		try {
			this.#setTimeline(await op);
			await this.refreshHistory();
		} catch (e) {
			this.error = this.#msg(e);
			throw e;
		} finally {
			this.#busyCount--;
		}
	}

	cut(assetId: string, start: number, end: number) {
		return this.#apply(cutClip(assetId, start, end));
	}
	add(assetId: string, sourceIn: number, sourceOut: number, trackId?: string, timelineStart?: number) {
		return this.#apply(addClip(assetId, sourceIn, sourceOut, trackId, timelineStart));
	}
	/** Split a clip at `at`; its linked partners are cut at the same moment unless `link` is `false`. */
	split(clipId: string, at: number, link?: boolean) {
		return this.#apply(splitClip(clipId, at, link));
	}
	/** Trim a clip; the partners that share the edge follow it unless `link` is `false`. */
	trim(clipId: string, sourceIn?: number, sourceOut?: number, timelineStart?: number, link?: boolean) {
		return this.#apply(trimClip(clipId, sourceIn, sourceOut, timelineStart, link));
	}
	reorder(trackId: string, clipId: string, newIndex: number) {
		return this.#apply(reorderClip(trackId, clipId, newIndex));
	}
	move(clipId: string, timelineStart: number, trackId?: string, link?: boolean) {
		return this.#apply(moveClip(clipId, timelineStart, trackId, link));
	}
	/** Move several clips as ONE edit — a dragged selection. All or nothing: the
	 *  promise rejects, and nothing has moved, if the group does not fit. The partners of
	 *  the clips named move with them (the same Δt, each on its own track) unless `link`
	 *  is `false`. */
	moveClips(moves: ClipMove[], link?: boolean) {
		return this.#apply(moveClips(moves, link));
	}
	/** Roll the cut between two adjacent clips by `delta` seconds (positive later);
	 *  the partner pairs sharing the cut roll with it unless `link` is `false`. */
	roll(clipA: string, clipB: string, delta: number, link?: boolean) {
		return this.#apply(rollEdit(clipA, clipB, delta, link));
	}
	/** Slip a clip's footage by `delta` source seconds (positive = starts later in it). */
	slip(clipId: string, delta: number, link?: boolean) {
		return this.#apply(slipClip(clipId, delta, link));
	}
	/** Slide a clip along its track by `delta` timeline seconds; touching neighbours give way. */
	slide(clipId: string, delta: number, link?: boolean) {
		return this.#apply(slideClip(clipId, delta, link));
	}
	/** Split a clip at `at` and remove the `left` or `right` half; follows ripple mode. */
	splitRemove(clipId: string, at: number, side: SplitSide) {
		return this.#apply(splitRemove(clipId, at, side));
	}
	/** Split and remove on several clips as ONE edit (one revision, one undo). */
	splitRemoveClips(cuts: ClipCut[], side: SplitSide) {
		return this.#apply(splitRemoveClips(cuts, side));
	}

	// ---- ripple mode ----------------------------------------------------------

	/** Read the project's ripple flag — at launch, after New / Open (`load`), and
	 *  when an agent flips it (`ripple-mode-changed`). Resolves to whether it
	 *  differs from what was showing. A failed read leaves what was there. */
	async loadRippleMode(): Promise<boolean> {
		try {
			const on = await getRippleMode();
			const changed = on !== this.rippleMode;
			this.rippleMode = on;
			return changed;
		} catch {
			return false;
		}
	}

	/** Turn ripple mode on or off. The toolbar flips at once; if the backend
	 *  refuses it goes back and the error is thrown. It is a setting, not an
	 *  edit: no revision, and the timeline does not move. */
	async setRippleMode(on: boolean) {
		const was = this.rippleMode;
		this.rippleMode = on;
		try {
			this.rippleMode = await setRippleMode(on);
		} catch (e) {
			this.rippleMode = was;
			throw e;
		}
	}
	/**
	 * The clipboard holds clip *snapshots*, not ids, so cut-then-paste still
	 * works once the sources are gone — and the same clipboard can be pasted
	 * repeatedly, since the backend re-ids on every insert.
	 */
	clipboard = $state<Placement[]>([]);

	/** Copy the selection to the clipboard; resolves to how many clips. With
	 *  `editableOnly`, clips on a locked track are left out — what a cut may take —
	 *  and a selection with nothing left leaves the clipboard as it was. */
	copySelection(editableOnly = false): number {
		const want = new Set(this.selectedClipIds);
		const out: Placement[] = [];
		for (const t of this.timeline.tracks) {
			if (editableOnly && t.locked) continue;
			for (const c of t.clips) if (want.has(c.id)) out.push({ track_id: t.id, clip: $state.snapshot(c) as Clip });
		}
		if (editableOnly && out.length === 0) return 0;
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

	/** Remove a clip — and its linked partners, unless `link` is `false` (delete the
	 *  picture, keep its sound). */
	remove(clipId: string, link?: boolean) {
		this.#forget(clipId);
		return this.#apply(removeClip(clipId, link));
	}
	rippleDelete(clipId: string, link?: boolean) {
		this.#forget(clipId);
		return this.#apply(rippleDelete(clipId, link));
	}

	/** Drop a clip from the selection; the primary falls to what is left. */
	#forget(clipId: string) {
		this.#setSelection(
			normalize(
				this.selectedClipIds.filter((id) => id !== clipId),
				this.selectedClipId === clipId ? null : this.selectedClipId
			)
		);
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
	/** Cut a clip to a shape, or `null` to clear the mask. */
	setMask(clipId: string, mask: Mask | null) {
		return this.#apply(setMask(clipId, mask));
	}
	setTrackVolume(trackId: string, volume: number) {
		return this.#apply(setTrackVolume(trackId, volume));
	}
	setTrackPan(trackId: string, pan: number) {
		return this.#apply(setTrackPan(trackId, pan));
	}
	/** The master fader: a linear gain on the finished mix (1 is unity). */
	setMasterVolume(volume: number) {
		return this.#apply(setMasterVolume(volume));
	}
	/** The master limiter on or off; `ceilingDb` omitted keeps the ceiling it had. */
	setMasterLimiter(enabled: boolean, ceilingDb?: number | null) {
		return this.#apply(setMasterLimiter(enabled, ceilingDb));
	}
	/** How ducked tracks dip: a negative `depthDb` is the speech gate (that many dB under speech,
	 *  `-40..-1`), omitted the sidechain compressor. */
	setMasterDuck(depthDb?: number | null) {
		return this.#apply(setMasterDuck(depthDb));
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
	/** The easing of the segment leaving the keyframe at `time` (clip-local seconds). */
	setKeyframeEasing(clipId: string, time: number, easing: Easing, prop?: Property) {
		return this.#apply(setKeyframeEasing(clipId, time, easing, prop));
	}
	/** Replace the keys of one number — a transform number, a colour number or the volume. */
	setPropertyKeyframes(clipId: string, prop: Property, keys: PropertyKey[]) {
		return this.#apply(setPropertyKeyframes(clipId, prop, keys));
	}
	/** Copy the animation of `props` (all keyed ones when empty) to another clip, `offset` later. */
	copyKeyframes(fromClipId: string, toClipId: string, props: Property[] = [], offset = 0) {
		return this.#apply(copyKeyframes(fromClipId, toClipId, props, offset));
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
	/** Add a title at `start..end` and select it. */
	async addTitle(text: string, start: number, end: number) {
		const known = new Set(this.overlays.map((o) => o.id));
		await this.addOverlay(text, start, end);
		const created = this.overlays.find((o) => !known.has(o.id));
		if (created) this.selectOverlay(created.id);
		return created;
	}
	updateOverlay(overlayId: string, patch: Partial<Omit<TextOverlay, 'id' | 'keyframes'>>) {
		return this.#apply(updateOverlay(overlayId, patch));
	}
	/** Set a title's span. Its keyframes are relative to the start, so a length
	 *  change re-times them (fade-out stays on the end) as a second edit. */
	async retimeOverlay(overlayId: string, start: number, end: number) {
		const o = this.overlays.find((o) => o.id === overlayId);
		if (!o) return this.timeline;
		const keys = o.keyframes ? o.keyframes.map((k) => ({ ...k })) : [];
		const oldDur = o.end - o.start;
		const tl = await this.updateOverlay(overlayId, { start, end });
		if (keys.length && Math.abs(end - start - oldDur) > 1e-9) {
			return this.setOverlayKeyframes(overlayId, retimeKeyframes(keys, oldDur, end - start));
		}
		return tl;
	}
	/** Put a title's centre at `(x, y)`. A still title takes it as its position;
	 *  an animated one gets a keyframe at timeline time `at`, since its static
	 *  position is not what the render reads. */
	moveOverlay(overlayId: string, x: number, y: number, at: number) {
		const o = this.overlays.find((o) => o.id === overlayId);
		if (o?.keyframes?.length) return this.setOverlayKeyframes(overlayId, withKeyframeAt(o, at, x, y));
		return this.updateOverlay(overlayId, { pos_x: x, pos_y: y });
	}
	removeOverlay(overlayId: string) {
		if (this.selectedOverlayId === overlayId) this.selectedOverlayId = null;
		return this.#apply(removeOverlay(overlayId));
	}
	setOverlayKeyframes(overlayId: string, keyframes: TextKeyframe[]) {
		return this.#apply(setOverlayKeyframes(overlayId, keyframes));
	}
	/**
	 * Synthesize a script onto the VO track. The asset carries its script as its
	 * transcript, so unlike an import it is never queued for analysis. Nothing
	 * is reported through `error` — the dialog says what happened, and a
	 * cancelled run is not an error at all.
	 */
	async generateVoiceover(req: VoiceoverRequest): Promise<Asset> {
		this.#busyCount++;
		this.previewingStaged = false;
		this.#liveTimeline = null;
		try {
			const { asset, timeline } = await generateVoiceover(req);
			this.assets = this.assets.some((a) => a.id === asset.id)
				? this.assets.map((a) => (a.id === asset.id ? asset : a))
				: [...this.assets, asset];
			this.#setTimeline(timeline);
			await this.refreshHistory();
			await this.select(asset.id);
			return asset;
		} finally {
			this.#busyCount--;
		}
	}
	/**
	 * Split an asset's sound into drums / bass / other / vocals (Demucs). The four stems join the
	 * library; with `clipId` they are also laid under that clip on four new tracks and its own
	 * sound is switched off — one `Separate stems` revision, and the new clips are selected. Like
	 * `generateVoiceover` it reports nothing through `error`: the dialog says what happened, and
	 * a cancelled run is not an error at all.
	 */
	async separateStems(assetId: string, clipId?: string): Promise<StemsPlaced> {
		this.#busyCount++;
		if (clipId) {
			// An edit to the live cut, so reviewing a proposal is over; into the library alone it is not.
			this.previewingStaged = false;
			this.#liveTimeline = null;
		}
		try {
			const { placed, timeline } = await separateStems(assetId, clipId);
			const known = new Set(this.assets.map((a) => a.id));
			const fresh = placed.assets.filter((a) => !known.has(a.id));
			if (fresh.length > 0) this.assets = [...this.assets, ...fresh];
			if (clipId) {
				this.#setTimeline(timeline);
				await this.refreshHistory();
				if (placed.clips.length > 0) this.selectClips(placed.clips.map((c) => c.id), placed.clips[0].id);
			} else if (placed.assets.length > 0) {
				await this.select(placed.assets[0].id);
			}
			return placed;
		} catch (e) {
			// The stems land in the library before the clip is laid down, so a refusal there
			// leaves them in the project without the bin having heard of them.
			await this.refreshAssets().catch(() => {});
			throw e;
		} finally {
			this.#busyCount--;
		}
	}
	generateCaptions(options?: CaptionOptions) {
		return this.#apply(generateCaptions(options));
	}
	clearCaptions() {
		return this.#apply(clearCaptions());
	}
	/** Caption the cut from a subtitle file on disk (`.srt` / `.ass` / `.ssa`).
	 *  One `Import captions` revision that replaces the generated / imported
	 *  captions; resolves to what the import did (see `describeImport`). */
	importCaptions(path: string, req?: CaptionImportRequest) {
		return this.#applyImport(importCaptions(path, req));
	}
	/** `importCaptions` for text already in hand (a file input, a paste). */
	importCaptionsText(text: string, req?: CaptionImportRequest & { format?: CaptionFormat }) {
		return this.#applyImport(importCaptionsText(text, req));
	}
	async #applyImport(op: Promise<CaptionImportResult>): Promise<CaptionImportSummary> {
		let summary: CaptionImportSummary | undefined;
		await this.#apply(
			op.then((r) => {
				summary = r.summary;
				return r.timeline;
			})
		);
		return summary as CaptionImportSummary;
	}
	/** Write the asset's transcript to `.srt`; returns the path (no timeline change). */
	exportSrt(assetId: string, outputPath: string) {
		return exportSrt(assetId, outputPath);
	}
	removeSilence(assetId: string) {
		return this.#apply(removeSilence(assetId));
	}
	/** **Fit to video**: replace a music clip with a bar-aligned arrangement that lasts `target` seconds
	 *  (by default the picture's length from the clip's start) — one `Fit music to length` revision.
	 *  Resolves to what it did. The clip is gone, so the clips that replace it are selected. */
	async fitMusic(clipId: string, target: number | null | undefined, fadeOut: boolean): Promise<MusicFitReport> {
		let report!: MusicFitReport;
		await this.#apply(
			fitMusic(clipId, target, fadeOut).then((r) => {
				report = r.report;
				return r.timeline;
			})
		);
		if (report.clips.length > 0) this.selectClips(report.clips, report.clips[0]);
		return report;
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
	/** Detach the sound of every clip of an asset still playing its own (one revision); resolves
	 *  to what was detached and what was skipped (a locked track). */
	async extractAudio(assetId: string): Promise<AudioDetached> {
		let report!: AudioDetached;
		await this.#apply(
			extractAudio(assetId).then((r) => {
				report = r;
				return r.timeline;
			})
		);
		return report;
	}
	/** Append an asset's whole audio to the first audio track as a clip of its own. */
	addAssetAudio(assetId: string) {
		return this.#apply(addAssetAudio(assetId));
	}

	// ---- linked A/V -------------------------------------------------------------

	/** Give a picture clip its own sound as a linked clip on an audio track, and mute the
	 *  picture (one `Detach audio` revision). */
	detachAudio(clipId: string) {
		return this.#apply(detachAudio(clipId));
	}
	/** Detach several pictures' own sound as **one** `Detach audio (N clips)` revision; a clip that
	 *  cannot be is skipped and reported. */
	async detachAudioClips(clipIds: string[]): Promise<AudioDetached> {
		let report!: AudioDetached;
		await this.#apply(
			detachAudioClips(clipIds).then((r) => {
				report = r;
				return r.timeline;
			})
		);
		return report;
	}
	/** The way back: delete the linked audio clip and let the picture play its own sound
	 *  again. Name either clip of the pair. */
	reattachAudio(clipId: string) {
		return this.#apply(reattachAudio(clipId));
	}
	/** Reattach several pairs as **one** `Reattach audio (N clips)` revision, all or nothing. */
	reattachAudioClips(clipIds: string[]) {
		return this.#apply(reattachAudioClips(clipIds));
	}
	/** Link clips (two or more, one per track) so an edit to one is carried to the others. */
	linkClips(clipIds: string[]) {
		return this.#apply(linkClips(clipIds));
	}
	/** Take clips out of their link groups. */
	unlinkClips(clipIds: string[]) {
		return this.#apply(unlinkClips(clipIds));
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

	/** The render in flight, or null. It lives here rather than in the export
	 *  dialog so closing the dialog mid-render doesn't orphan it: reopening the
	 *  dialog (or glancing at the status bar) still shows progress and a Stop. */
	exportRun = $state<{ progress: ExportProgress | null; cancelling: boolean } | null>(null);

	async #render<T>(run: () => Promise<T>): Promise<T> {
		this.#busyCount++;
		this.exportRun = { progress: null, cancelling: false };
		let unlisten: (() => void) | undefined;
		try {
			unlisten = await onExportProgress((p) => {
				if (this.exportRun) this.exportRun.progress = p;
			});
			return await run();
		} finally {
			unlisten?.();
			this.exportRun = null;
			this.#busyCount--;
		}
	}

	async stopExport() {
		if (!this.exportRun || this.exportRun.cancelling) return;
		this.exportRun.cancelling = true;
		await cancelExport();
	}

	export(outputPath: string, options: ExportOptions): Promise<string> {
		return this.#render(() => exportTimeline(outputPath, options));
	}

	async exportVariants(
		outputPath: string,
		formats: Delivery[],
		smartCrop: boolean,
		options: ExportOptions
	): Promise<string[]> {
		try {
			return await this.#render(() => exportVariants(outputPath, formats, smartCrop, options));
		} finally {
			// The framing pass wrote onto the clips; the history has a revision
			// the panel has not seen.
			await this.refreshTimeline().catch(() => {});
		}
	}

	#msg(e: unknown): string {
		return e instanceof Error ? e.message : String(e);
	}
}

export const editor = new EditorState();
