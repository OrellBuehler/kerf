// Central editor state (Svelte 5 runes).

import { duplicateBlocked, pastePlacements } from './track-locks';
import type { AudioDetached, Placement } from './api';
import {
	addClip,
	addKeyframe,
	applyStagedSubset,
	cancelCoverExport,
	cancelImport,
	cancelSmartCrop,
	discardedStagedEdit,
	openRecent as apiOpenRecent,
	relinkAsset,
	removeAsset,
	restoreDiscardedEdits,
	undoStagedEdit,
	addReframeKeyframe,
	addOverlay,
	analyzeAsset,
	refineTranscript,
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
	editAssetTags,
	getAssetMetadata,
	getHistory,
	getRippleMode,
	getTimeline,
	importAsset,
	listAssets,
	listTags,
	addTrack,
	moveClip,
	moveClips,
	removeOverlay,
	addSvgOverlay as apiAddSvgOverlay,
	applyTemplate as apiApplyTemplate,
	applyTemplateClip as apiApplyTemplateClip,
	addTitleClip as apiAddTitleClip,
	updateTitleClip as apiUpdateTitleClip,
	removeSvgOverlay as apiRemoveSvgOverlay,
	setTemplateValues as apiSetTemplateValues,
	setTitleTextBox as apiSetTitleTextBox,
	setTitleTextAnimation as apiSetTitleTextAnimation,
	convertTitleText as apiConvertTitleText,
	updateSvgOverlay as apiUpdateSvgOverlay,
	setAudioEffects,
	setKeyframes,
	setPropertyKeyframes,
	setPropertyTracks,
	copyKeyframes,
	setAssetProjection,
	setReframe,
	setReframeKeyframes,
	setOverlayKeyframes,
	setVideoEffects,
	updateOverlay,
	newProject as apiNewProject,
	openProject as apiOpenProject,
	recoverProject as apiRecoverProject,
	pickMediaPaths,
	projectPath,
	redo as apiRedo,
	removeClip,
	removeClips,
	removeSilence,
	snapToBeats,
	smartCrop,
	syncClipsByAudio,
	removeTrack,
	setTrackDuck,
	setMask,
	setGrade,
	setTrackVolume,
	setTrackPan,
	setMasterVolume,
	setMasterLimiter,
	setMasterTruePeak,
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
	setThumbnail,
	removeThumbnail,
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
	setSpeedRamp,
	setTransform,
	setTransformAt,
	setTransition,
	setVolume,
	splitClip,
	trimClip,
	generateVoiceover,
	composeMusic,
	separateStems,
	undo as apiUndo
} from './api';
import type {
	ThumbnailSpec,
	AudioSync,
	ClipCut,
	ClipMove,
	SpeedKey,
	SplitSide,
	ExportProgress,
	ExportReport,
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
	Grade,
	MusicFitReport,
	Projection,
	Property,
	PropertyKey,
	PropertyTrack,
	Reframe,
	ReframeKeyframe,
	Revision,
	StagedEdit,
	AssetRemoval,
	Relinked,
	SubsetApplied,
	StemsPlaced,
	StreamKind,
	SvgLayout,
	SvgOverlay,
	SvgOverlayPatch,
	SvgSource,
	TagEdit,
	TagSummary,
	TemplateEdit,
	TextAnimation,
	TextBox,
	TitleTextForm,
	TitleClipPatch,
	TextKeyframe,
	Marker,
	MarkerPatch,
	TextOverlay,
	Timeline,
	Transform,
	Transition,
	VideoEffect,
	VoiceoverRequest,
	ComposeRequest,
	ComposeResult
} from './types';
import { clipDuration, GENERATED_ASSET } from './types';
import { findTitle, type TitleRef } from './title-clips';
import { timelineFps } from './timecode';
import { trimRefusal } from './trim-tools';
import { HISTORY_PAGE, redoable, undoable } from './paging';
import { Generation, ReadOrder } from './generation';
import { retimeKeyframes, withKeyframeAt } from './titles';
import { normalize, pruneSelection, type PickMode, type Modifiers, type Selection } from './selection';
import { collapsed, pickAsset as pickAssetSelection, restrictToVisible, selectionOf } from './bin-selection';
import { tagsSignature } from './tag-ui';
import { clickSelectLinked } from './link-ui';
import { locateIndex, withLinkPartners } from './link-groups';

/** What an edit refused for the page's own reasons says while a proposal is being previewed. */
export const PREVIEW_READ_ONLY = 'Exit the preview to edit — you are looking at the agent’s proposed cut, not yours.';

/** What importing files came to: what landed, what failed (with the reason) and how many were stopped by the user. */
export interface ImportOutcome {
	imported: Asset[];
	failed: { name: string; message: string }[];
	cancelled: number;
}

/** An edit that did not happen: the backend refused it, or the page did (see `EditorState.#apply`). The
 *  message is what the user reads; `cause` is what the backend said. */
export class EditRefused extends Error {
	constructor(message: string, cause?: unknown) {
		super(message, cause === undefined ? undefined : { cause });
		this.name = 'EditRefused';
	}
}

/** The page's own refusal while the proposal is previewed: not a failure, so it is said as a notice, not an error. */
export class PreviewReadOnly extends EditRefused {
	constructor() {
		super(PREVIEW_READ_ONLY);
		this.name = 'PreviewReadOnly';
	}
}

class EditorState {
	assets = $state<Asset[]>([]);
	timeline = $state<Timeline>({ tracks: [] });
	/** The asset the Inspector describes and a drag out of the bin takes: the primary of `selectedAssetIds`. */
	selectedAssetId = $state<string | null>(null);
	/** The assets selected in the bin (Ctrl/Cmd-click toggles, Shift-click extends). Holds `selectedAssetId` when that is
	 *  set; every way of changing the two goes through `select` / `pickAsset` / `selectAssets`. */
	selectedAssetIds = $state<string[]>([]);
	selectedAssetSet = $derived(new Set(this.selectedAssetIds));
	/** Every tag in use in the library (`list_tags`), for the autocomplete and the bin's filter; read again whenever
	 *  the tags of the assets change (`refreshTagVocabulary`). */
	tagVocabulary = $state.raw<TagSummary>({ keys: [], plain: [] });
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
	/** The animated SVG title being edited. Exclusive with the clip and title selections, for the
	 *  same reason: the Inspector shows one subject. */
	selectedSvgOverlayId = $state<string | null>(null);
	selectedMetadata = $state<AssetMetadata | null>(null);
	analyses = $state<Record<string, AssetAnalysis>>({});
	history = $state<Revision[]>([]);
	/** How many of the newest edits are loaded: a project is read from its latest `HISTORY_PAGE`, and more are
	 *  asked for (`showOlderHistory`) — a long session is hundreds of revisions, and every edit re-reads the list. */
	historyLimit = HISTORY_PAGE;
	/** There may be edits older than the ones loaded. */
	historyMore = $state(false);
	/** The proposal a connected agent has staged, or null. */
	staged = $state<StagedEdit | null>(null);
	/** While true, `timeline` holds the *proposed* cut, not the live one. */
	previewingStaged = $state(false);
	currentPath = $state<string | null>(null);
	loading = $state(false);
	/** Counts completed `load()`s — launch, New, Open: a new one is a different cut, which the
	 *  timeline fits to its window again. */
	loadEpoch = $state(0);
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
	/** A stop has been asked for and the import has not given up yet. */
	stoppingImport = $state(false);
	/** A smart crop is sampling the shots (`smart-crop-progress` fills the fraction); Stop is the status bar's. */
	smartCropping = $state(false);
	smartCropProgress = $state<number | null>(null);
	stoppingSmartCrop = $state(false);
	/** A cover frame is being rendered at the full delivery frame (`saveCoverFrame`). */
	coverRun = $state<{ cancelling: boolean } | null>(null);
	error = $state<string | null>(null);
	/** The proposal thrown away last, while it can still be brought back (only with none open). */
	discarded = $state<StagedEdit | null>(null);

	/** The clip whose animation was last copied (Copy keyframes); a paste reads it as it is then (`keyframe-copy.ts`). */
	keyframeSource = $state<string | null>(null);

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
		if (this.selectedSvgOverlayId && !(tl.svg_overlays ?? []).some((o) => o.id === this.selectedSvgOverlayId)) {
			this.selectedSvgOverlayId = null;
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
	#vocabGen = new Generation();

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

	/** The selection as a set: the timeline asks of every clip it draws. */
	selectedSet = $derived(new Set(this.selectedClipIds));

	isSelected(clipId: string): boolean {
		return this.selectedSet.has(clipId);
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
		this.selectedSvgOverlayId = null;
		const track = this.timeline.tracks.find((t) => t.clips.some((c) => c.id === clipId));
		this.#setSelection(
			clickSelectLinked(this.timeline, this.#selection(), clipId, mode, track ? track.clips.map((c) => c.id) : null, alone)
		);
	}

	/** Replace the whole selection (what a marquee ends on). The primary stays a
	 *  member: `primary` when it is one, else the last. */
	selectClips(ids: readonly string[], primary: string | null = null) {
		this.selectedOverlayId = null;
		this.selectedSvgOverlayId = null;
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
		this.selectedSvgOverlayId = null;
		this.selectedClipIds = ids;
		this.selectedClipId = ids.at(-1) ?? null;
	}

	clearSelection() {
		this.selectedClipId = null;
		this.selectedClipIds = [];
		this.selectedOverlayId = null;
		this.selectedSvgOverlayId = null;
	}

	/** Select a title, dropping the clip selection so the Inspector shows one thing. */
	selectOverlay(overlayId: string | null) {
		this.selectedOverlayId = overlayId;
		if (overlayId) {
			this.selectedClipId = null;
			this.selectedClipIds = [];
			this.selectedSvgOverlayId = null;
		}
	}

	/** Select an animated SVG title, dropping the clip and title selections. */
	selectSvgOverlay(overlayId: string | null) {
		this.selectedSvgOverlayId = overlayId;
		if (overlayId) {
			this.selectedClipId = null;
			this.selectedClipIds = [];
			this.selectedOverlayId = null;
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
		this.#assertEditable();
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
		await this.#apply(() => removeClips(ids, ripple ? true : undefined));
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

	/** The animated SVG titles in draw order, from the timeline being shown (the backend omits an empty list). */
	get svgOverlays(): SvgOverlay[] {
		return this.timeline.svg_overlays ?? [];
	}

	/** A title by id, floating or on a track (`findTitle`): what the template controls and the save dialog read. */
	titleById(id: string): TitleRef | undefined {
		return findTitle(this.timeline, id);
	}

	get selectedSvgOverlay(): SvgOverlay | undefined {
		return this.svgOverlays.find((o) => o.id === this.selectedSvgOverlayId);
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
			(this.timeline.svg_overlays?.length ?? 0) > 0 ||
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
		return undoable(this.history, this.historyMore);
	}

	get canRedo(): boolean {
		return redoable(this.history);
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
			// A read of the old project's proposal still in flight must not put it back.
			this.#stagedOrder.invalidate();
			this.staged = null;
			this.discarded = null;
			this.keyframeSource = null;
			this.#analysisAsked.clear();
			// The ripple flag is the project's, saved in its file: a project that was
			// saved with ripple on has to open that way, and a new one has to reset
			// the toggle, so it is read with everything else every load. (It never
			// rejects — a failed read keeps what was showing.)
			// A project is read from its newest edits; older ones are asked for (`showOlderHistory`).
			this.historyLimit = HISTORY_PAGE;
			const [assets, timeline, history, currentPath] = await Promise.all([
				listAssets(),
				getTimeline(),
				getHistory(this.historyLimit),
				projectPath(),
				this.loadRippleMode()
			]);
			this.assets = assets;
			this.#setTimeline(timeline);
			this.history = history;
			this.historyMore = history.length >= this.historyLimit;
			this.currentPath = currentPath;
			this.loadEpoch++;
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
		this.selectedAssetIds = [];
		this.selectedClipId = null;
		this.selectedClipIds = [];
		this.selectedOverlayId = null;
		this.selectedSvgOverlayId = null;
		await this.load();
		return true;
	}

	/** Open a `.kerf` file (`path`, else the native picker) and reload; resolves true if opened. */
	async openProject(file?: string): Promise<boolean> {
		const path = await apiOpenProject(file);
		if (path === null) return false; // cancelled, or running in the browser
		this.selectedAssetId = null;
		this.selectedAssetIds = [];
		this.selectedClipId = null;
		this.selectedClipIds = [];
		this.selectedOverlayId = null;
		this.selectedSvgOverlayId = null;
		await this.load();
		return true;
	}

	/** Open one of the recent projects and reload; a file that has since gone rejects (and drops off the list). */
	async openRecent(file: string): Promise<boolean> {
		const path = await apiOpenRecent(file);
		if (path === null) return false; // running in the browser
		this.selectedAssetId = null;
		this.selectedAssetIds = [];
		this.selectedClipId = null;
		this.selectedClipIds = [];
		this.selectedOverlayId = null;
		this.selectedSvgOverlayId = null;
		await this.load();
		return true;
	}

	/** Open an untitled project a crashed Kerf left behind; resolves true if recovered. */
	async recoverProject(id: string): Promise<boolean> {
		if (!(await apiRecoverProject(id))) return false;
		this.selectedAssetId = null;
		this.selectedAssetIds = [];
		this.selectedClipId = null;
		this.selectedClipIds = [];
		this.selectedOverlayId = null;
		this.selectedSvgOverlayId = null;
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
		// A title on a track is no asset: nothing to look up.
		if (assetId === GENERATED_ASSET) {
			this.#selectGen.advance();
			this.selectedAssetId = null;
			this.selectedAssetIds = [];
			this.selectedMetadata = null;
			return;
		}
		this.selectedAssetIds = [assetId];
		await this.#describe(assetId);
	}

	/** Make `assetId` the one the Inspector describes and read its metadata; the set of selected assets is the caller's. */
	async #describe(assetId: string) {
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

	/** The bin's selection as a `Selection`: the assets and the primary. */
	get assetSelection(): Selection {
		return selectionOf(this.selectedAssetIds, this.selectedAssetId);
	}

	/** Select these assets, `selection.primary` being the one the Inspector describes (a primary that is already the
	 *  Inspector's is not read again). */
	async selectAssets(selection: Selection) {
		this.selectedAssetIds = [...selection.ids];
		if (selection.primary === this.selectedAssetId) return;
		if (selection.primary === null) {
			this.#selectGen.advance();
			this.selectedAssetId = null;
			this.selectedMetadata = null;
			return;
		}
		await this.#describe(selection.primary);
	}

	/** A click on an asset of the bin: a plain click selects it alone, Ctrl/Cmd toggles it, Shift extends from the
	 *  primary across `order` (the assets the bin shows, in order). */
	pickAsset(assetId: string, mods: Modifiers, order: readonly string[]) {
		const plain = !mods.shiftKey && !mods.ctrlKey && !mods.metaKey;
		if (plain) return this.select(assetId);
		return this.selectAssets(pickAssetSelection(this.assetSelection, assetId, mods, order));
	}

	/** Drop the assets from the selection that `visible` does not hold (the bin's filter hides them). */
	restrictAssetSelection(visible: ReadonlySet<string>) {
		const now = this.assetSelection;
		const next = restrictToVisible(now, visible);
		if (next !== now) void this.selectAssets(next);
	}

	/** Escape in the bin: a selection of several shrinks to its primary. Whether it had anything to do. */
	collapseAssetSelection(): boolean {
		if (this.selectedAssetIds.length < 2) return false;
		this.selectedAssetIds = [...collapsed(this.assetSelection).ids];
		return true;
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

	/** Orders the reads of the proposal: of the ones that come back, the newest writes (`refreshStaged`); a project
	 *  loaded or a proposal settled makes every read in flight stale. */
	#stagedOrder = new ReadOrder();

	async refreshStaged() {
		const ticket = this.#stagedOrder.begin();
		let next: StagedEdit | null;
		try {
			next = await getStagedEdit();
		} catch {
			// A read that failed says nothing about the proposal: it is neither gone nor changed — and it
			// takes nobody's place, so an older read that does come back still lands.
			return;
		}
		// A newer read has written while this one was out (the agent staged twice in a burst), or the project
		// was loaded / the proposal settled: its answer is the later one, and this one must not overwrite it.
		if (!this.#stagedOrder.land(ticket)) return;
		this.staged = next;
		if (!next) {
			if (this.previewingStaged) await this.exitStagedPreview();
			// A proposal that was thrown away can be brought back; ask what, if anything, is waiting. A read that
			// fails offers nothing (a card that may be a stale one is worse), and one that was overtaken while it
			// was out — a project loaded, a proposal opened or settled — is not what is waiting now.
			let waiting: StagedEdit | null = null;
			try {
				waiting = await discardedStagedEdit();
			} catch {
				/* nothing to restore */
			}
			if (this.#stagedOrder.isLatest(ticket)) this.discarded = waiting;
			return;
		}
		this.discarded = null;
		if (!this.previewingStaged) return;
		// The agent staged more while we were looking at it.
		let proposed: Timeline | null = null;
		try {
			proposed = await getStagedTimeline();
		} catch {
			return;
		}
		// The preview may have been left, or the proposal changed again, while that was out.
		if (proposed && this.previewingStaged && this.#stagedOrder.isLatest(ticket)) this.#setTimeline(proposed);
	}

	/** Show the proposed cut in the editor instead of the live one. */
	async previewStaged() {
		const proposed = await getStagedTimeline();
		// Applied or discarded while it was being fetched: there is nothing to show.
		if (!proposed || !this.staged) return;
		if (!this.previewingStaged) this.#liveTimeline = this.timeline;
		this.#setTimeline(proposed);
		this.previewingStaged = true;
		this.clearSelection();
		// It may have been replaced in that time; read it again now that the preview follows it.
		void this.refreshStaged();
	}

	async exitStagedPreview() {
		if (!this.previewingStaged) return;
		this.previewingStaged = false;
		this.#setTimeline(this.#liveTimeline ?? (await getTimeline()));
		this.#liveTimeline = null;
		this.clearSelection();
	}

	/** Accept the proposal — it lands on the live timeline as one revision. A refusal (a stale cut, a
	 *  clip that no longer fits) leaves the preview on screen. */
	async applyStaged(force = false) {
		await this.#apply(() => applyStagedEdit(force), { review: true });
		this.#proposalSettled();
		await this.refreshStaged();
	}

	/** Throw the proposal away; the live timeline is untouched. */
	async discardStaged() {
		await this.#apply(() => discardStagedEdit(), { review: true });
		this.#proposalSettled();
		await this.refreshStaged();
	}

	/** Accept only the picked changes of the proposal (`entryIds`: the ids of its diff entries) as one revision.
	 *  What was not picked stays open for review, branched from the cut as it now is, until nothing is left. */
	async applyStagedSubset(entryIds: string[], force = false): Promise<SubsetApplied> {
		let done!: SubsetApplied;
		await this.#apply(
			async () => {
				done = await applyStagedSubset(entryIds, force);
				return done.timeline;
			},
			{ review: true }
		);
		if (done.closed) this.#proposalSettled();
		await this.refreshStaged();
		return done;
	}

	/** Take back the latest step the agent staged, inside the proposal (the live cut is untouched);
	 *  resolves to the step's label. */
	async undoStagedStep(): Promise<string> {
		const undone = await undoStagedEdit();
		await this.refreshStaged();
		return undone.label;
	}

	/** Bring the proposal that was thrown away back (only while none is open). */
	async restoreDiscarded(): Promise<void> {
		await restoreDiscardedEdits();
		this.discarded = null;
		await this.refreshStaged();
	}

	/** The proposal is gone (applied or discarded): whatever a slower read still brings is older than that. */
	#proposalSettled() {
		this.#stagedOrder.invalidate();
		this.staged = null;
	}

	async refreshHistory() {
		try {
			const history = await getHistory(this.historyLimit);
			this.history = history;
			this.historyMore = history.length >= this.historyLimit;
		} catch {
			/* history is best-effort; ignore */
		}
	}

	/** Load another page of older edits (the Agent panel's History lists them newest first). */
	async showOlderHistory() {
		this.historyLimit += HISTORY_PAGE;
		await this.refreshHistory();
	}

	/** Pick one or more media files and import them. All files probe
	 *  concurrently (each lands in the bin as it resolves); imports continue
	 *  past a failed file and resolve to the successes plus per-file errors. */
	async importMedia(): Promise<ImportOutcome> {
		return this.importPaths(await pickMediaPaths());
	}

	/** Import specific files — what the picker resolves to, and what a drop onto
	 *  the window hands over. */
	async importPaths(paths: string[]): Promise<ImportOutcome> {
		if (paths.length === 0) return { imported: [], failed: [], cancelled: 0 };
		this.importing = true;
		this.stoppingImport = false;
		this.error = null;
		const failed: { name: string; message: string }[] = [];
		let cancelled = 0;
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
						// A stop the user asked for is not a failure of the file.
						if (/import cancel+ed/i.test(this.#msg(e))) cancelled++;
						else failed.push({ name: path.split(/[\\/]/).pop() || path, message: this.#msg(e) });
						return null;
					}
				})
			);
			const imported = results.filter((a): a is Asset => a !== null);
			if (imported.length > 0) await this.select(imported[imported.length - 1].id);
			return { imported, failed, cancelled };
		} finally {
			this.importing = false;
			this.stoppingImport = false;
			this.importProgress = null;
		}
	}

	/** Re-read the asset list, for assets an agent brought in behind our back
	 *  (a voiceover it generated) or tagged. Left alone when nothing is new, so the bin
	 *  does not re-render on every agent edit. */
	async refreshAssets() {
		const assets = await listAssets();
		const known = new Set(this.assets.map((a) => a.id));
		if (assets.length !== known.size || assets.some((a) => !known.has(a.id)) || tagsSignature(assets) !== tagsSignature(this.assets)) {
			this.assets = assets;
			const here = new Set(assets.map((a) => a.id));
			if (this.selectedAssetIds.some((id) => !here.has(id))) this.selectedAssetIds = this.selectedAssetIds.filter((id) => here.has(id));
		}
	}

	/** Put tags on assets, take them off or replace a key's, all in one transaction (`edit_asset_tags`): the bin, the
	 *  Inspector and the vocabulary follow at once. Not an edit of the cut, so no revision and no undo. */
	async tagAssets(assetIds: readonly string[], edit: TagEdit): Promise<Asset[]> {
		const done = await editAssetTags([...assetIds], edit);
		const by = new Map(done.map((a) => [a.id, a]));
		this.assets = this.assets.map((a) => by.get(a.id) ?? a);
		const meta = this.selectedMetadata;
		const fresh = meta ? by.get(meta.asset.id) : undefined;
		if (meta && fresh) this.selectedMetadata = { ...meta, asset: fresh };
		return done;
	}

	/** Read the library's tag vocabulary again. The newest read wins; a failed one keeps what was there. */
	async refreshTagVocabulary() {
		const seq = this.#vocabGen.advance();
		try {
			const vocab = await listTags();
			if (this.#vocabGen.isCurrent(seq)) this.tagVocabulary = vocab;
		} catch {
			/* the autocomplete just has less to offer */
		}
	}

	/** Take media out of the project: refused while a clip (or the proposal) uses it, unless `force`, which first
	 *  removes those clips as one `Remove media` step. Its analysis and proxies go with it; the file on disk stays. */
	async removeMedia(assetId: string, force = false): Promise<AssetRemoval> {
		let done!: AssetRemoval;
		await this.#apply(async () => {
			done = await removeAsset(assetId, force);
			return done.timeline;
		});
		this.assets = this.assets.filter((a) => a.id !== assetId);
		delete this.analyses[assetId];
		this.selectedAssetIds = this.selectedAssetIds.filter((id) => id !== assetId);
		if (this.selectedAssetId === assetId) {
			this.selectedAssetId = null;
			this.selectedMetadata = null;
			// The others that were selected hand the Inspector one of them.
			if (this.selectedAssetIds.length > 0) await this.selectAssets(normalize(this.selectedAssetIds, null));
		}
		return done;
	}

	/** Point an asset at another file (the media was moved, renamed or restored). It keeps its id and every clip
	 *  cut from it; its analysis stays only when the file is the old media moved. */
	async relinkMedia(assetId: string, path: string): Promise<Relinked> {
		const done = await relinkAsset(assetId, path);
		this.assets = this.assets.map((a) => (a.id === assetId ? done.asset : a));
		if (!done.analysis_kept) delete this.analyses[assetId];
		if (this.selectedAssetId === assetId) await this.select(assetId);
		return done;
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

	/** Re-time the selected asset's transcript words and label its speakers, keeping the
	 *  local caches in step. */
	async refineTranscript(assetId: string): Promise<AssetAnalysis> {
		const analysis = await refineTranscript(assetId);
		this.analyses[assetId] = analysis;
		if (assetId === this.selectedAssetId && this.selectedMetadata) {
			this.selectedMetadata = { ...this.selectedMetadata, analysis };
		}
		return analysis;
	}

	// ---- editing actions (apply backend result to local timeline) -----------

	/** Whether the cut on screen is the project's own: false while a proposal is being previewed, when
	 *  every id on the timeline is the proposal's and an edit would land on a cut nobody is looking at. */
	get editable(): boolean {
		return !this.previewingStaged;
	}
	/** Whether a control that edits the cut should be off: an edit is in flight, or the proposal is on screen. */
	get locked(): boolean {
		return this.busy || this.previewingStaged;
	}
	#assertEditable() {
		if (this.previewingStaged) throw new PreviewReadOnly();
	}

	/**
	 * Run one edit and show what it leaves. The proposal preview is a read-only view of a cut that is not
	 * the project's, so an edit is refused *before* the backend is asked (`run` is a thunk for that reason)
	 * — except the two that settle the proposal itself (`review`: apply, discard), which end it. The
	 * preview is dropped only once the edit has gone through; a refusal leaves it exactly as it was.
	 * A failure rejects with an `EditRefused` and is reported by whoever called (a toast); a caller that
	 * did not catch it is caught by the page (`window-events`), so it is said once either way.
	 */
	async #apply(run: () => Promise<Timeline>, o: { review?: boolean } = {}) {
		if (!o.review) this.#assertEditable();
		this.#busyCount++;
		try {
			const tl = await run();
			this.previewingStaged = false;
			this.#liveTimeline = null;
			this.#setTimeline(tl);
			await this.refreshHistory();
			// The proposal's `stale` flag is the backend's, and an edit just flipped it: re-read it, so the review
			// says "you have edited since" now and not only when the agent next stages something. The same for a
			// discarded one waiting to be restored, which an edit makes a restore of an older cut.
			if ((this.staged || this.discarded) && !o.review) await this.refreshStaged();
		} catch (e) {
			throw e instanceof EditRefused ? e : new EditRefused(this.#msg(e), e);
		} finally {
			this.#busyCount--;
		}
	}

	cut(assetId: string, start: number, end: number) {
		return this.#apply(() => cutClip(assetId, start, end));
	}
	add(assetId: string, sourceIn: number, sourceOut: number, trackId?: string, timelineStart?: number) {
		return this.#apply(() => addClip(assetId, sourceIn, sourceOut, trackId, timelineStart));
	}
	/** Split a clip at `at`; its linked partners are cut at the same moment unless `link` is `false`. */
	split(clipId: string, at: number, link?: boolean) {
		return this.#apply(() => splitClip(clipId, at, link));
	}
	/** Trim a clip; the partners that share the edge follow it unless `link` is `false`. */
	trim(clipId: string, sourceIn?: number, sourceOut?: number, timelineStart?: number, link?: boolean) {
		return this.#apply(() => trimClip(clipId, sourceIn, sourceOut, timelineStart, link));
	}
	/** The sentence the backend would refuse a trim with (footage it does not have, a lane it would land on,
	 *  after the ripple when that is on), or `null` when it takes it — answered here, from the cut on screen. */
	trimRefusal(clipId: string, sourceIn?: number, sourceOut?: number, link?: boolean): string | null {
		return trimRefusal(this.timeline, { ripple: this.rippleMode, assets: this.assets }, clipId, sourceIn, sourceOut, undefined, link);
	}
	reorder(trackId: string, clipId: string, newIndex: number) {
		return this.#apply(() => reorderClip(trackId, clipId, newIndex));
	}
	move(clipId: string, timelineStart: number, trackId?: string, link?: boolean) {
		return this.#apply(() => moveClip(clipId, timelineStart, trackId, link));
	}
	/** Move several clips as ONE edit — a dragged selection. All or nothing: the
	 *  promise rejects, and nothing has moved, if the group does not fit. The partners of
	 *  the clips named move with them (the same Δt, each on its own track) unless `link`
	 *  is `false`. */
	moveClips(moves: ClipMove[], link?: boolean) {
		return this.#apply(() => moveClips(moves, link));
	}
	/** Roll the cut between two adjacent clips by `delta` seconds (positive later);
	 *  the partner pairs sharing the cut roll with it unless `link` is `false`. */
	roll(clipA: string, clipB: string, delta: number, link?: boolean) {
		return this.#apply(() => rollEdit(clipA, clipB, delta, link));
	}
	/** Slip a clip's footage by `delta` source seconds (positive = starts later in it). */
	slip(clipId: string, delta: number, link?: boolean) {
		return this.#apply(() => slipClip(clipId, delta, link));
	}
	/** Slide a clip along its track by `delta` timeline seconds; touching neighbours give way. */
	slide(clipId: string, delta: number, link?: boolean) {
		return this.#apply(() => slideClip(clipId, delta, link));
	}
	/** Split a clip at `at` and remove the `left` or `right` half; follows ripple mode. */
	splitRemove(clipId: string, at: number, side: SplitSide) {
		return this.#apply(() => splitRemove(clipId, at, side));
	}
	/** Split and remove on several clips as ONE edit (one revision, one undo). */
	splitRemoveClips(cuts: ClipCut[], side: SplitSide) {
		return this.#apply(() => splitRemoveClips(cuts, side));
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

	/** Paste the clipboard so its earliest clip lands at `at` — each clip on the track it came from, or the first
	 *  unlocked one of its kind when that track is locked (`pastePlacements`). */
	async paste(at: number): Promise<number> {
		if (this.clipboard.length === 0) return 0;
		const aimed = pastePlacements(this.timeline, this.clipboard, this.assets);
		if ('reason' in aimed) throw new Error(aimed.reason);
		await this.#apply(() => insertClips(aimed.placements, at));
		return this.clipboard.length;
	}

	/** Duplicate the selection immediately after itself. */
	async duplicateSelection(): Promise<number> {
		const sel = this.selectedClips;
		if (sel.length === 0) return 0;
		const blocked = duplicateBlocked(this.timeline, sel.map((c) => c.id));
		if (blocked) throw new Error(blocked);
		const end = Math.max(...sel.map((c) => c.timeline_start + clipDuration(c)));
		await this.#apply(() => duplicateClips(sel.map((c) => c.id), end));
		return sel.length;
	}

	/** Remove a clip — and its linked partners, unless `link` is `false` (delete the
	 *  picture, keep its sound). */
	remove(clipId: string, link?: boolean) {
		this.#forget(clipId);
		return this.#apply(() => removeClip(clipId, link));
	}
	rippleDelete(clipId: string, link?: boolean) {
		this.#forget(clipId);
		return this.#apply(() => rippleDelete(clipId, link));
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
		return this.#apply(() => cutClipRange(clipId, from, to));
	}
	addTrack(kind: StreamKind, name?: string) {
		return this.#apply(() => addTrack(kind, name));
	}
	removeTrack(trackId: string) {
		return this.#apply(() => removeTrack(trackId));
	}
	setTrackDuck(trackId: string, duck: boolean) {
		return this.#apply(() => setTrackDuck(trackId, duck));
	}
	/** Cut a clip to a shape, or `null` to clear the mask. */
	setMask(clipId: string, mask: Mask | null) {
		return this.#apply(() => setMask(clipId, mask));
	}
	setGrade(clipId: string, grade: Grade) {
		return this.#apply(() => setGrade(clipId, grade));
	}
	setTrackVolume(trackId: string, volume: number) {
		return this.#apply(() => setTrackVolume(trackId, volume));
	}
	setTrackPan(trackId: string, pan: number) {
		return this.#apply(() => setTrackPan(trackId, pan));
	}
	/** The master fader: a linear gain on the finished mix (1 is unity). */
	setMasterVolume(volume: number) {
		return this.#apply(() => setMasterVolume(volume));
	}
	/** The master limiter on or off; `ceilingDb` omitted keeps the ceiling it had. */
	setMasterLimiter(enabled: boolean, ceilingDb?: number | null) {
		return this.#apply(() => setMasterLimiter(enabled, ceilingDb));
	}
	/** The master limiter holds the true peak (4x oversampled on export) or only the sample peak. */
	setMasterTruePeak(enabled: boolean) {
		return this.#apply(() => setMasterTruePeak(enabled));
	}
	/** How ducked tracks dip: a negative `depthDb` is the speech gate (that many dB under speech,
	 *  `-40..-1`), omitted the sidechain compressor. */
	setMasterDuck(depthDb?: number | null) {
		return this.#apply(() => setMasterDuck(depthDb));
	}
	/** The frame this project is cut for; `null` follows the footage's shape. */
	setDeliveryFormat(format: Delivery | null) {
		return this.#apply(() => setDeliveryFormat(format));
	}
	/** Add an auto-named marker at `time` — the M shortcut and the timeline menu. */
	addMarkerAtPlayhead(time: number) {
		return this.addMarker(time, `Marker ${this.markers.length + 1}`);
	}
	addMarker(time: number, name: string, color?: string) {
		return this.#apply(() => addMarker(time, name, color));
	}
	updateMarker(markerId: string, patch: MarkerPatch) {
		return this.#apply(() => updateMarker(markerId, patch));
	}
	/** Add a thumbnail (`thumbnailId` null) or change one; resolves the thumbnail's id and the warnings. One edit. */
	async setThumbnail(thumbnailId: string | null, spec: ThumbnailSpec): Promise<{ id: string; warnings: string[] }> {
		let out = { id: '', warnings: [] as string[] };
		await this.#apply(async () => {
			const edit = await setThumbnail(thumbnailId, spec);
			out = { id: edit.thumbnail_id, warnings: edit.warnings };
			return edit.timeline;
		});
		return out;
	}
	removeThumbnail(thumbnailId: string) {
		return this.#apply(() => removeThumbnail(thumbnailId));
	}
	removeMarker(markerId: string) {
		return this.#apply(() => removeMarker(markerId));
	}
	setTrackMuted(trackId: string, muted: boolean) {
		return this.#apply(() => setTrackMuted(trackId, muted));
	}
	setTrackSolo(trackId: string, solo: boolean) {
		return this.#apply(() => setTrackSolo(trackId, solo));
	}
	setTrackLocked(trackId: string, locked: boolean) {
		return this.#apply(() => setTrackLocked(trackId, locked));
	}
	setClipEnabled(clipId: string, enabled: boolean) {
		return this.#apply(() => setClipEnabled(clipId, enabled));
	}
	setVolume(clipId: string, volume: number) {
		return this.#apply(() => setVolume(clipId, volume));
	}
	setFade(clipId: string, fadeIn?: number, fadeOut?: number) {
		return this.#apply(() => setFade(clipId, fadeIn, fadeOut));
	}
	setSpeed(clipId: string, speed: number) {
		return this.#apply(() => setSpeed(clipId, speed));
	}
	setSpeedRamp(clipId: string, keys: SpeedKey[]) {
		return this.#apply(() => setSpeedRamp(clipId, keys));
	}
	setTransformAt(clipId: string, time: number, patch: Partial<Transform>) {
		return this.#apply(() => setTransformAt(clipId, time, patch));
	}
	setTransform(clipId: string, patch: Partial<Transform>) {
		return this.#apply(() => setTransform(clipId, patch));
	}
	setColor(clipId: string, patch: Partial<Color>) {
		return this.#apply(() => setColor(clipId, patch));
	}
	setTransition(clipId: string, transition: Transition | null) {
		return this.#apply(() => setTransition(clipId, transition));
	}
	setVideoEffects(clipId: string, effects: VideoEffect[]) {
		return this.#apply(() => setVideoEffects(clipId, effects));
	}
	setAudioEffects(clipId: string, effects: AudioEffect[]) {
		return this.#apply(() => setAudioEffects(clipId, effects));
	}
	setKeyframes(clipId: string, keyframes: Keyframe[]) {
		return this.#apply(() => setKeyframes(clipId, keyframes));
	}
	addKeyframe(clipId: string, time: number, patch: Partial<Omit<Keyframe, 'time'>> = {}) {
		return this.#apply(() => addKeyframe(clipId, time, patch));
	}
	/** The easing of the segment leaving the keyframe at `time` (clip-local seconds). */
	setKeyframeEasing(clipId: string, time: number, easing: Easing, prop?: Property) {
		return this.#apply(() => setKeyframeEasing(clipId, time, easing, prop));
	}
	/** Replace the keys of one number — a transform number, a colour number or the volume. */
	setPropertyKeyframes(clipId: string, prop: Property, keys: PropertyKey[]) {
		return this.#apply(() => setPropertyKeyframes(clipId, prop, keys));
	}
	/** Replace the keys of several numbers in one edit (one dope-sheet gesture). */
	setPropertyTracks(clipId: string, tracks: PropertyTrack[]) {
		return this.#apply(() => setPropertyTracks(clipId, tracks));
	}
	/** Copy the animation of `props` (all keyed ones when empty) to another clip, `offset` later. */
	copyKeyframes(fromClipId: string, toClipId: string, props: Property[] = [], offset = 0) {
		return this.#apply(() => copyKeyframes(fromClipId, toClipId, props, offset));
	}
	clearKeyframes(clipId: string) {
		return this.#apply(() => clearKeyframes(clipId));
	}
	setReframe(clipId: string, patch: Partial<Reframe>) {
		return this.#apply(() => setReframe(clipId, patch));
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
		return this.#apply(() => clearReframe(clipId));
	}
	setReframeKeyframes(clipId: string, keyframes: ReframeKeyframe[]) {
		return this.#apply(() => setReframeKeyframes(clipId, keyframes));
	}
	addReframeKeyframe(
		clipId: string,
		time: number,
		patch: Partial<Omit<ReframeKeyframe, 'time'>> = {}
	) {
		return this.#apply(() => addReframeKeyframe(clipId, time, patch));
	}
	addOverlay(text: string, start: number, end: number) {
		return this.#apply(() => addOverlay(text, start, end));
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
		return this.#apply(() => updateOverlay(overlayId, patch));
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
		return this.#apply(() => removeOverlay(overlayId));
	}
	/** Add an animated SVG title (custom artwork or a preset) from `start`, to `end` or as long as the artwork
	 *  runs. Custom SVG has been through `validateSvg` first when the caller wants its warnings. */
	addSvgOverlay(source: SvgSource, start: number, end?: number | null, layout?: SvgLayout) {
		return this.#apply(() => apiAddSvgOverlay(source, start, end, layout));
	}
	/** Add an animated SVG title and select it. */
	async addSvgTitle(source: SvgSource, start: number, end?: number | null, layout?: SvgLayout) {
		const known = new Set(this.svgOverlays.map((o) => o.id));
		await this.addSvgOverlay(source, start, end, layout);
		const created = this.svgOverlays.find((o) => !known.has(o.id));
		if (created) this.selectSvgOverlay(created.id);
		return created;
	}
	updateSvgOverlay(overlayId: string, patch: SvgOverlayPatch) {
		return this.#apply(() => apiUpdateSvgOverlay(overlayId, patch));
	}
	removeSvgOverlay(overlayId: string) {
		if (this.selectedSvgOverlayId === overlayId) this.selectedSvgOverlayId = null;
		return this.#apply(() => apiRemoveSvgOverlay(overlayId));
	}
	/** Add a library template as an animated title from `start` (to `end`, else as long as the template runs), with
	 *  `values` (control id to value) already in its artwork, and select it. One edit; the warnings are the caller's. */
	async applyTemplate(templateId: string, values: Record<string, string>, start: number, end?: number | null, layout?: SvgLayout) {
		let edit!: TemplateEdit;
		await this.#apply(async () => {
			edit = await apiApplyTemplate(templateId, values, start, end, layout);
			return edit.timeline;
		});
		if (edit.overlay_id) this.selectSvgOverlay(edit.overlay_id);
		return edit;
	}
	/** Add a library template as a title clip on a video track at `start` (as long as the template runs unless
	 *  `duration`), with `values` in its artwork, and select it. One edit; the warnings are the caller's. */
	async applyTemplateClip(templateId: string, values: Record<string, string>, start: number, duration?: number | null, trackId?: string | null) {
		return this.#titleClipEdit(() => apiApplyTemplateClip(templateId, values, start, duration, trackId));
	}
	/** Add custom SVG artwork as a title clip on a video track at `start`, and select it. */
	async addTitleClip(svg: string, start: number, duration?: number | null, trackId?: string | null, name?: string | null) {
		return this.#titleClipEdit(() => apiAddTitleClip(svg, start, duration, trackId, name));
	}
	/** Rename a title clip or replace its artwork (which ends a template). */
	async updateTitleClip(clipId: string, patch: TitleClipPatch) {
		return this.#titleClipEdit(() => apiUpdateTitleClip(clipId, patch));
	}
	async #titleClipEdit(call: () => Promise<TemplateEdit>) {
		let edit!: TemplateEdit;
		await this.#apply(async () => {
			edit = await call();
			return edit.timeline;
		});
		if (edit.clip_id) this.selectClip(edit.clip_id);
		return edit;
	}
	/** Change some control values of a template title: one edit, the title keeps its id, timing and placement. */
	async setTemplateValues(overlayId: string, values: Record<string, string>) {
		let edit!: TemplateEdit;
		await this.#apply(async () => {
			edit = await apiSetTemplateValues(overlayId, values);
			return edit.timeline;
		});
		return edit;
	}
	/** Give a title's `<text>` a box (or none); its text is set as outlines again. */
	async setTitleTextBox(titleId: string, element: string, textBox: TextBox) {
		let edit!: TemplateEdit;
		await this.#apply(async () => {
			edit = await apiSetTitleTextBox(titleId, element, textBox);
			return edit.timeline;
		});
		return edit;
	}
	/** Give a title's `<text>` an animation (or none); its text is set as outlines again. */
	async setTitleTextAnimation(titleId: string, element: string, animation: TextAnimation | null) {
		let edit!: TemplateEdit;
		await this.#apply(async () => {
			edit = await apiSetTitleTextAnimation(titleId, element, animation);
			return edit.timeline;
		});
		return edit;
	}
	/** Draw a title's text as glyph outlines or with its fonts. */
	async convertTitleText(titleId: string, form: TitleTextForm) {
		let edit!: TemplateEdit;
		await this.#apply(async () => {
			edit = await apiConvertTitleText(titleId, form);
			return edit.timeline;
		});
		return edit;
	}
	setOverlayKeyframes(overlayId: string, keyframes: TextKeyframe[]) {
		return this.#apply(() => setOverlayKeyframes(overlayId, keyframes));
	}
	/**
	 * Synthesize a script onto the VO track. The asset carries its script as its
	 * transcript, so unlike an import it is never queued for analysis. Nothing
	 * is reported through `error` — the dialog says what happened, and a
	 * cancelled run is not an error at all.
	 */
	async generateVoiceover(req: VoiceoverRequest): Promise<Asset> {
		this.#assertEditable();
		this.#busyCount++;
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
	 * Render a score onto the M track (or, with `replace_clip_id`, into that composed clip) and select the
	 * clip it lands as. The core writes the music's tempo and bar analysis from the score, so nothing is
	 * queued for analysis. Like `generateVoiceover` it reports nothing through `error`: the dialog says
	 * what happened, and a cancelled run is not an error at all.
	 */
	async composeMusic(req: ComposeRequest): Promise<ComposeResult> {
		this.#assertEditable();
		this.#busyCount++;
		try {
			const done = await composeMusic(req);
			this.assets = this.assets.some((a) => a.id === done.asset.id)
				? this.assets.map((a) => (a.id === done.asset.id ? done.asset : a))
				: [...this.assets, done.asset];
			this.#setTimeline(done.timeline);
			await this.refreshHistory();
			this.selectClips([done.clip.id], done.clip.id);
			await this.ensureAnalyses([done.asset.id]);
			return done;
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
		// Laid under a clip it is an edit to the live cut; into the library alone it is not.
		if (clipId) this.#assertEditable();
		this.#busyCount++;
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
		return this.#apply(() => generateCaptions(options));
	}
	clearCaptions() {
		return this.#apply(() => clearCaptions());
	}
	/** Caption the cut from a subtitle file on disk (`.srt` / `.vtt` / `.ass` / `.ssa`).
	 *  One `Import captions` revision that replaces the generated / imported
	 *  captions; resolves to what the import did (see `describeImport`). */
	importCaptions(path: string, req?: CaptionImportRequest) {
		return this.#applyImport(() => importCaptions(path, req));
	}
	/** `importCaptions` for text already in hand (a file input, a paste). */
	importCaptionsText(text: string, req?: CaptionImportRequest & { format?: CaptionFormat }) {
		return this.#applyImport(() => importCaptionsText(text, req));
	}
	async #applyImport(op: () => Promise<CaptionImportResult>): Promise<CaptionImportSummary> {
		let summary: CaptionImportSummary | undefined;
		await this.#apply(() =>
			op().then((r) => {
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
		return this.#apply(() => removeSilence(assetId));
	}
	/** **Fit to video**: replace a music clip with a bar-aligned arrangement that lasts `target` seconds
	 *  (by default the picture's length from the clip's start) — one `Fit music to length` revision.
	 *  Resolves to what it did. The clip is gone, so the clips that replace it are selected. */
	async fitMusic(clipId: string, target: number | null | undefined, fadeOut: boolean): Promise<MusicFitReport> {
		let report!: MusicFitReport;
		await this.#apply(() =>
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
		return this.#apply(() => snapToBeats(trackId, tolerance));
	}
	/**
	 * Frame each shot for the delivery frame instead of centring it blindly.
	 * One clip when `clipId` is given, otherwise every clip on an unlocked video
	 * track. Lands as one undoable `Smart crop` revision.
	 */
	async smartCrop(clipId?: string) {
		this.smartCropping = true;
		this.smartCropProgress = null;
		this.stoppingSmartCrop = false;
		try {
			await this.#apply(() => smartCrop(clipId));
		} finally {
			this.smartCropping = false;
			this.smartCropProgress = null;
			this.stoppingSmartCrop = false;
		}
	}
	/** Stop the smart crop in flight; it then rejects with `smart crop cancelled`, which `report` says as a notice. */
	async stopSmartCrop() {
		this.stoppingSmartCrop = true;
		try {
			await cancelSmartCrop();
		} catch {
			this.stoppingSmartCrop = false;
		}
	}

	/** Stop the cover frame being rendered; `saveCoverFrame` then says it was stopped. */
	async stopCover() {
		if (!this.coverRun) return;
		this.coverRun = { cancelling: true };
		try {
			await cancelCoverExport();
		} catch {
			this.coverRun = { cancelling: false };
		}
	}

	/** Stop every import in flight (a 360 lens pair being stitched is the one that takes long enough to stop). */
	async stopImport() {
		this.stoppingImport = true;
		try {
			await cancelImport();
		} catch {
			this.stoppingImport = false;
		}
	}
	/** Move `clipId` so its sound lines up with `referenceId`'s; resolves to what was found.
	 *  An unreliable match moves nothing unless `force`. One `Move clip` revision when it moves. */
	async syncByAudio(referenceId: string, clipId: string, force = false): Promise<AudioSync> {
		let sync!: AudioSync;
		await this.#apply(() =>
			syncClipsByAudio(referenceId, clipId, force).then((r) => {
				sync = r.sync;
				return r.timeline;
			})
		);
		return sync;
	}
	/** Detach the sound of every clip of an asset still playing its own (one revision); resolves
	 *  to what was detached and what was skipped (a locked track). */
	async extractAudio(assetId: string): Promise<AudioDetached> {
		let report!: AudioDetached;
		await this.#apply(() =>
			extractAudio(assetId).then((r) => {
				report = r;
				return r.timeline;
			})
		);
		return report;
	}
	/** Append an asset's whole audio to the first audio track as a clip of its own. */
	addAssetAudio(assetId: string) {
		return this.#apply(() => addAssetAudio(assetId));
	}

	// ---- linked A/V -------------------------------------------------------------

	/** Give a picture clip its own sound as a linked clip on an audio track, and mute the
	 *  picture (one `Detach audio` revision). */
	detachAudio(clipId: string) {
		return this.#apply(() => detachAudio(clipId));
	}
	/** Detach several pictures' own sound as **one** `Detach audio (N clips)` revision; a clip that
	 *  cannot be is skipped and reported. */
	async detachAudioClips(clipIds: string[]): Promise<AudioDetached> {
		let report!: AudioDetached;
		await this.#apply(() =>
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
		return this.#apply(() => reattachAudio(clipId));
	}
	/** Reattach several pairs as **one** `Reattach audio (N clips)` revision, all or nothing. */
	reattachAudioClips(clipIds: string[]) {
		return this.#apply(() => reattachAudioClips(clipIds));
	}
	/** Link clips (two or more, one per track) so an edit to one is carried to the others. */
	linkClips(clipIds: string[]) {
		return this.#apply(() => linkClips(clipIds));
	}
	/** Take clips out of their link groups. */
	unlinkClips(clipIds: string[]) {
		return this.#apply(() => unlinkClips(clipIds));
	}
	concatenate(assetIds: string[]) {
		return this.#apply(() => concatenate(assetIds));
	}

	// ---- history (undo / redo / revert) -------------------------------------

	undo() {
		// Refused while the proposal is previewed (`#apply`): the selection it would clear is the proposal's.
		if (!this.previewingStaged) this.clearSelection();
		return this.#apply(() => apiUndo());
	}
	redo() {
		if (!this.previewingStaged) this.clearSelection();
		return this.#apply(() => apiRedo());
	}
	revertTo(seq: number) {
		if (!this.previewingStaged) this.clearSelection();
		return this.#apply(() => apiRevertTo(seq));
	}
	/** What one revision changed; null where the backend can't say (browser). */
	revisionDiff(seq: number) {
		return apiRevisionDiff(seq);
	}

	/** The render in flight, or null. It lives here rather than in the export
	 *  dialog so closing the dialog mid-render doesn't orphan it: reopening the
	 *  dialog (or glancing at the status bar) still shows progress and a Stop. */
	exportRun = $state<{ progress: ExportProgress | null; cancelling: boolean } | null>(null);

	/** Run a render. It raises `exportRun` (the status bar's progress and Stop, the dialog's state) but
	 *  not `busy`: the export reads the cut it was started on, so the editor stays open while it runs —
	 *  an export that takes minutes must not freeze every control. `blocks` is for the one that does
	 *  write to the cut while it runs (the framing pass of a multi-format export). */
	async #render<T>(run: () => Promise<T>, blocks = false): Promise<T> {
		if (blocks) this.#busyCount++;
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
			if (blocks) this.#busyCount--;
		}
	}

	async stopExport() {
		if (!this.exportRun || this.exportRun.cancelling) return;
		this.exportRun.cancelling = true;
		await cancelExport();
	}

	export(outputPath: string, options: ExportOptions): Promise<ExportReport> {
		return this.#render(() => exportTimeline(outputPath, options));
	}

	async exportVariants(
		outputPath: string,
		formats: Delivery[],
		smartCrop: boolean,
		options: ExportOptions,
		singlePass = true
	): Promise<ExportReport[]> {
		try {
			return await this.#render(() => exportVariants(outputPath, formats, smartCrop, options, singlePass), smartCrop);
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
