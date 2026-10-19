<script lang="ts">
	import { untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import Badge from './Badge.svelte';
	import ClipOverlays from './ClipOverlays.svelte';
	import ClipFilmstrip from './ClipFilmstrip.svelte';
	import ClipWaveform from './ClipWaveform.svelte';
	import LaneBlocks from './LaneBlocks.svelte';
	import HeightGlyph from './HeightGlyph.svelte';
	import ToolbarToggle from './ToolbarToggle.svelte';
	import Minimap from './Minimap.svelte';
	import TimecodeField from './TimecodeField.svelte';
	import MarkerDialog from './MarkerDialog.svelte';
	import { markerTip } from '#lib/marker-edit';
	import { toast } from '#lib/notifications.svelte';
	import { ui, type Tool } from '#lib/editor-ui.svelte';
	import { editor } from '#lib/state.svelte';
	import { settings } from '#lib/settings.svelte';
	import { contextMenu } from '#lib/context-menu.svelte';
	import { documentOf, resizeObserverFor, windowOf } from '#lib/realm';
	import { onWindow } from '#lib/window-events';
	import { windows } from '#lib/windows.svelte';
	import type { MenuItem } from '#lib/context-menu.svelte';
	import {
		addMarker,
		deleteSelection,
		detachSelection,
		duplicateClips,
		holdForPreview,
		linkSelection,
		pasteClips,
		reattachSelection,
		copyKeyframesOf,
		pasteKeyframesOnto,
		report,
		selectionLinkPlans,
		splitSelection,
		trimSelection,
		unlinkSelection
	} from '#lib/ops';
	import { addAnimatedTitle, addCustomSvgTitle, importCaptionFile } from '#lib/title-actions';
	import { keyframeOffer } from '#lib/keyframe-copy';
	import type { ActionId } from '#lib/keymap';
	import { DELIVERY_PRESETS, fitLabel, presetFor } from '#lib/delivery-formats';
	import { setDeliveryPreset } from '#lib/menu-commands';
	import { importMenuEntries, importableAssets } from '#lib/caption-import-ui';
	import type { Clip, Marker, PreviewDraft, StreamKind, SvgOverlay, TextOverlay, Track } from '#lib/types';
	import { moveDraft, trimDraft } from '#lib/preview-draft';
	import { GENERATED_TITLE_FILL, SVG_TITLE_FILL, packRows, snapSpanStart, snapTime, trimSpan } from '#lib/titles';
	import { SVG_PRESETS, svgDisplayName, svgTooltip } from '#lib/svg-titles';
	import { faderToGain, gainLabel, gainToFader, panLabel } from '#lib/mixer';
	import { trackHasSound } from '#lib/mixer-strips';
	import { clampEdge, quantizeSpanStart, quantizeTime, splitPoint, startBefore, trimEdit } from '#lib/frames';
	import { beginDrag } from '#lib/drag';
	import { createAutoscroll, followScroll, liveEnd, type ScrollRegion } from '#lib/autoscroll';
	import { magnetList, markAt, snapLines, type Magnet, type SnapLine } from '#lib/snap';
	import { cutReadout, moveReadout, trimEdgeMonitor, trimEdgeReadout, trimLimitWhy } from '#lib/gesture-readout';
	import { diffView, type DiffView } from '#lib/staged-diff';
	import { applyProposal, discardProposal } from '#lib/proposal-actions';
	import { PREVIEW_READ_ONLY_REASON, proposalLine, touchedOnLive } from '#lib/proposal-ui';
	import { displayTracks, opensAudioGroup } from '#lib/track-order';
	import { NUDGE_QUIET_MS, nudgeFrames, nudgedStart } from '#lib/nudge';
	import {
		CUT_REACH_PX,
		cutsOf,
		limitNotice,
		monitorFor,
		nearestCut,
		previewEdit,
		readoutFor,
		refusalNotice,
		slideMembers,
		slipDelta,
		sourceLimits,
		subjectsPresent,
		TOOL_HINT,
		type EditPreview,
		type GestureEdit,
		type Readout,
		type TrimTool
	} from '#lib/trim-tools';
	import { readPalette, resolveColor } from '#lib/waveform-draw';
	import { filmstrips } from '#lib/filmstrips';
	import {
		HEIGHT_PRESETS,
		MIXER_MIN_PX,
		PRESET_LABEL,
		PRESET_PX,
		TITLE_METRICS,
		titleLaneHeightFor,
		uniformPreset
	} from '#lib/track-heights';
	import { viewCoversCut, type Target } from '#lib/minimap';
	import { visibleLaneRange } from '#lib/waveform-view';
	import {
		blockAt,
		blockMark,
		clipPadding,
		laneDrawing,
		mayPromote,
		pinnedClips,
		pressOnEmpty,
		type LaneDrawing
	} from '#lib/timeline-cull';
	import { clipDuration } from '#lib/types';
	import { barGrid, beatGrid, beatPeriod, gridMagnets, sourceToTimeline } from '#lib/beats';
	import { NAME_CHIP_PCT, showsLabel } from '#lib/clip-label';
	import { titleName, titleOf } from '#lib/title-clips';
	import { fitOffer } from '#lib/music-ui';
	import { stemsOffer } from '#lib/stems';
	import { composeOffer } from '#lib/compose-form';
	import { describeSync, syncOffer, SYNC_CANCELLED } from '#lib/audio-sync';
	import { cancelAudioSync, onAudioSyncProgress } from '#lib/api';
	import { transitionLabel } from '#lib/transitions';
	import { marqueeMode, pickMode, sameIds, type Selection } from '#lib/selection';
	import { linkPartners } from '#lib/link-groups';
	import { ALT_HINT, linkBadges, marqueeSelectLinked, onlyPartners } from '#lib/link-ui';
	import { analyzeSubmenuItem } from '#lib/analyze-menu';
	import { heldForPreview, muteItem, removeRows } from '#lib/menu-items';
	import { doneCount, doneSummary } from '#lib/analysis-steps';
	import { mediaStatus } from '#lib/media-status.svelte';
	import { linkedTrimBounds, linkedTrimPreview } from '#lib/linked-trim';
	import { marqueeHits, normalizeRect, type LaneBox, type SpanClip } from '#lib/marquee';
	import { drawnMoveTracks, planMove, type Ghost, type MovePlan } from '#lib/multi-move';
	import { frameTicksIn, minorTicksIn, rulerGrid, showsFrameTicks, tickLabel, ticksIn } from '#lib/ruler';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { MIN_CLIP, trimBounds, type TrimBounds } from '#lib/ripple-trim';
	import {
		clampZoom,
		fitZoom,
		followZoom,
		laneWidth,
		scrollFor,
		sliderToZoom,
		wheelZoomFactor,
		zoomAround,
		zoomLabel,
		zoomToSlider,
		type FollowInput
	} from '#lib/zoom';
	import { tip, type TipInput } from '#lib/tip';
	import RenderBar from './RenderBar.svelte';

	const pxPerSec = $derived(ui.zoom);
	/** The lane is as long as the cut, and while a gesture is held as far as the gesture has reached,
	 *  so there is room to scroll into past the end of the cut. */
	const contentW = $derived.by(() => laneWidth(liveEnd(editor.duration, gestureEnd), pxPerSec));
	const hasClips = $derived(editor.timeline.tracks.some((t) => t.clips.length > 0));
	/** The tracks in the order they are drawn: the topmost video layer first, V1 above the audio. */
	const drawn = $derived(displayTracks(editor.timeline.tracks));
	/** What a staged proposal on the timeline changes: the clips and titles to outline, what it removes. */
	const diff = $derived.by((): DiffView | null =>
		editor.previewingStaged && editor.staged ? diffView(editor.staged.diff.entries, editor.timeline, editor.liveTimeline) : null
	);

	/** What the proposal touches on the *live* cut, for the faint marks shown before it is previewed. */
	const touched = $derived(
		editor.staged && !editor.previewingStaged ? touchedOnLive(editor.staged.diff.entries, editor.liveTimeline) : null
	);
	let reviewBusy = $state(false);
	async function reviewAct(answer: () => Promise<boolean>) {
		reviewBusy = true;
		try {
			await answer();
		} finally {
			reviewBusy = false;
		}
	}

	/** A press that would start a gesture while the proposal is on screen: the cut is read-only then, so no
	 *  gesture starts — it is remembered, and the first move past the slop (or a razor click) says why. */
	let previewPress = $state<{ x: number; y: number } | null>(null);
	function holdForReview(e: PointerEvent): boolean {
		if (!editor.previewingStaged) return false;
		previewPress = { x: e.clientX, y: e.clientY };
		return true;
	}

	// ---- linked A/V ----------------------------------------------------------------
	//
	// A picture and its detached sound are linked clips: a click selects both, a move, trim,
	// split or delete of one is carried to the other (the backend's default, and what every
	// drag below previews). **Alt** is the escape hatch — Alt-click selects the one clip,
	// Alt-drag / Alt-trim / Alt-razor edit it alone (`link: false`) — and it is read live, from
	// the pointer and the key, so pressing or letting go of it mid-drag redraws the ghosts.

	/** Whether Alt is down. */
	let alt = $state(false);
	/** The link badge of every clip that has one: the chain, and a muted speaker on a picture
	 *  whose sound was detached. */
	const badges = $derived(linkBadges(editor.timeline, (id) => editor.assetName(id)));
	const hasLinkedClips = $derived(editor.timeline.tracks.some((t) => t.clips.some((c) => c.link_id)));
	/** The clip under the pointer, so its partners can light up. */
	let hoverId = $state<string | null>(null);
	/** The clip with keyboard focus: a lane that draws only what is in view keeps it in the page. */
	let focusId = $state<string | null>(null);

	/** The project timecode: what the transport, the status bar and the ruler read in. */
	const tc = (s: number): string => timeDisplay.fmt(s);

	/** A track's lane height: its preset's pixels (`track-heights.ts`). The waveform's
	 *  lane count and whether a clip shows thumbnails both follow from what they
	 *  measure of the clip, so nothing else here knows about presets. */
	const trackHeight = (t: Track): string => `${ui.trackPx(t.id)}px`;

	/** Assets that carry an audio stream, rebuilt only when the bin changes —
	 *  hasSound runs per track on every timeline update, and a linear asset
	 *  scan per clip made that O(clips x assets). */
	const audibleAssets = $derived(
		new Set(editor.assets.filter((a) => a.streams?.some((st) => st.kind === 'audio')).map((a) => a.id))
	);
	/** True when a track can actually be heard — an audio track, or a video track
	 *  whose clips carry sound *and still play it*. A track with no audio at all gets
	 *  no fader: a mixer strip on a silent track is furniture, not a control. The one
	 *  rule the Mixer panel's strips follow too (`mixer-strips.ts`). */
	const hasSound = (t: Track): boolean => trackHasSound(t, audibleAssets);

	/** Shared look for the one-letter S / L track flags. */
	const flagBtn = (on: boolean, accent: string) =>
		`flex:none;font-family:var(--font-mono);font-size:12px;font-weight:600;line-height:1;min-width:26px;min-height:26px;padding:3px;border-radius:3px;cursor:pointer;` +
		`border:var(--line-width) solid ${on ? accent : 'var(--border-strong)'};background:${on ? accent : 'transparent'};color:${on ? 'var(--text-on-accent)' : 'var(--text-muted)'}`;

	/** A locked track refuses drag, trim and razor — the point of locking it. */
	const isLocked = (trackId: string) => !!editor.timeline.tracks.find((t) => t.id === trackId)?.locked;

	/** Mirrors `Timeline::track_renders` in kerf-core: muted tracks never render,
	 *  and while any track of a kind is soloed only the soloed ones of that kind do. */
	const soloed = $derived(
		new Set(editor.timeline.tracks.filter((t) => t.solo).map((t) => t.kind as string))
	);
	const renders = (t: Track) => !t.muted && (!soloed.has(t.kind) || !!t.solo);

	// ---- analysis overlays mapped from source-time to timeline-time ----------

	/** Timeline-seconds position of a source-time point inside a clip, honoring
	 * the clip's speed (and reverse direction). */
	const srcToTimeline = sourceToTimeline;

	const sceneXs = $derived.by(() => {
		const xs: number[] = [];
		for (const t of editor.timeline.tracks) {
			if (t.kind !== 'video') continue;
			for (const c of t.clips) {
				const an = editor.analysisFor(c.asset_id);
				if (!an) continue;
				for (const sc of an.scene_changes) {
					if (sc > c.source_in && sc < c.source_out) {
						xs.push(srcToTimeline(c, sc) * pxPerSec);
					}
				}
			}
		}
		return xs;
	});

	// ---- beat grid (tempo analysis → ruler ticks + snap targets) --------------

	/** Hide (and stop snapping to) the grid when beats land closer than this, px. */
	const BEAT_MIN_PX = 4;

	const beatTimes = $derived.by(() => {
		const ts = beatGrid(editor.timeline, (id) => editor.analysisFor(id)?.tempo);
		return beatPeriod(ts) * pxPerSec < BEAT_MIN_PX ? [] : ts;
	});

	/** Bar starts of fitted music: drawn taller than a beat and a snap target of their own, which
	 *  wins over a beat that is nearer. A bar is four beats or so apart, so they stay when the
	 *  beats have folded away on zoom-out. */
	const barTimes = $derived.by(() => {
		const ts = barGrid(editor.timeline, (id) => editor.analysisFor(id)?.tempo);
		return beatPeriod(ts) * pxPerSec < BEAT_MIN_PX ? [] : ts;
	});

	// The cut's audio is what the grid comes from, and an asset's analysis is only read when
	// something selects or analyzes it — so ask for the ones the audio tracks use. Without it a
	// reopened project's ruler stays bare, and the music menu has nothing to go on, until the
	// music happens to be clicked.
	$effect(() => {
		const ids = new Set<string>();
		for (const t of editor.timeline.tracks) if (t.kind === 'audio') for (const c of t.clips) ids.add(c.asset_id);
		untrack(() => void editor.ensureAnalyses(ids));
	});

	function silenceRegions(c: Clip): { left: number; width: number }[] {
		const an = editor.analysisFor(c.asset_id);
		if (!an) return [];
		return an.silence_segments
			.filter((s) => s.end > c.source_in && s.start < c.source_out)
			.map((s) => {
				const a = Math.max(s.start, c.source_in);
				const b = Math.min(s.end, c.source_out);
				const x1 = srcToTimeline(c, a) * pxPerSec;
				const x2 = srcToTimeline(c, b) * pxPerSec;
				return { left: Math.min(x1, x2), width: Math.abs(x2 - x1) };
			});
	}

	// ---- waveforms -----------------------------------------------------------
	//
	// Each audio clip draws its own canvas over only the part of it that is on
	// screen (`ClipWaveform`), so what the timeline owes it is the viewport — how far
	// it has scrolled, how wide it is, the pixel ratio — the colours of the active
	// theme, and the audio length of every asset.

	let scrollX = $state(0);
	let viewW = $state(1200);
	let dpr = $state(typeof window === 'undefined' ? 1 : window.devicePixelRatio || 1);

	/** The lane box fills the window when the cut is shorter than it: the ruler, the row borders and
	 *  the grid run to the edge, and what is past the end of the cut is drawn as such. */
	const laneW = $derived(Math.max(contentW, viewW));
	/** Where the cut ends in the lane, px. */
	const cutEndX = $derived(editor.duration * pxPerSec);

	function syncView() {
		const v = viewport();
		if (!v) return;
		scrollX = v.el.scrollLeft;
		viewW = v.viewW;
		// The pixel ratio of the window the timeline is in: a detached window may be on
		// another screen.
		dpr = windowOf(v.el).devicePixelRatio || 1;
	}

	$effect(() => {
		const el = scroller;
		if (!el) return;
		// Re-run when the panel moves into a detached window (or back): the observer has
		// to be that window's own, and the pixel ratio is read afresh.
		void windows.version;
		syncView();
		const ro = resizeObserverFor(el, syncView);
		ro.observe(el);
		return () => ro.disconnect();
	});

	$effect(() => {
		ui.timelineView = { start: scrollX / pxPerSec, end: (scrollX + viewW) / pxPerSec };
	});
	$effect(() => () => (ui.timelineView = null));

	// Two primitives rather than one object: a scroll tick that does not cross a
	// quantum changes neither, so no clip's waveform is touched by it.
	const viewLo = $derived(visibleLaneRange(scrollX, viewW).lo);
	const viewHi = $derived(visibleLaneRange(scrollX, viewW).hi);

	// ---- ruler ---------------------------------------------------------------
	//
	// The label step follows the zoom, and only the ticks in (or near) the visible
	// part of the lane exist: at the deep end of the zoom range an hour of cut is
	// far too many to draw, and at the shallow end a fixed step is all gap.

	const grid = $derived(rulerGrid(pxPerSec, editor.fps));
	const visibleSec = $derived([viewLo / pxPerSec, Math.min(viewHi, laneW) / pxPerSec] as const);
	/** The labelled (major) ticks. */
	const ticks = $derived(ticksIn(visibleSec[0], visibleSec[1], grid.step));
	/** A mark per frame, once a frame is wide enough to tell apart; they are then the minor ticks. */
	const frameTicks = $derived(frameTicksIn(visibleSec[0], visibleSec[1], editor.fps, pxPerSec));
	/** The minor ticks between two labels, unless the frames are drawn. */
	const minorTicks = $derived(
		showsFrameTicks(pxPerSec, editor.fps) ? [] : minorTicksIn(visibleSec[0], visibleSec[1], grid)
	);

	/** A canvas cannot read `var(--waveform)`, so the tokens are resolved here, once
	 *  per theme change: every theme edit replaces `settings.theme`, and does so
	 *  right after `applyTheme` has written the tokens this reads back. */
	const palette = $derived.by(() => {
		void settings.theme;
		return readPalette();
	});

	/** How far each asset's footage reaches: what every edit mode clamps to. */
	const footage = $derived(sourceLimits(editor.assets));

	const assetDuration = $derived.by(() => {
		const m = new Map<string, number>();
		for (const a of editor.assets) m.set(a.id, a.duration);
		return m;
	});

	/** A clip's volume while its line is dragged — the waveform follows it live. */
	let liveVolume = $state<Record<string, number | null>>({});

	/** The video clips whose thumbnails are on screen: their label gets a backing so
	 *  it stays legible over a picture, and keeps the plain look until there is one. */
	let filmReady = $state<Record<string, true>>({});

	// An asset removed from the project frees its decoded sheets rather than
	// waiting for the cache to age them out.
	$effect(() => {
		filmstrips.prune(editor.assets.map((a) => a.id));
	});

	/** The one height every track is at (the "all tracks" control lights that one),
	 *  or null when they differ. */
	const allHeight = $derived(
		uniformPreset(
			ui.heights,
			editor.timeline.tracks.map((t) => t.id)
		)
	);

	/** Open a track's height menu under its name button — anchored to the button, not
	 *  the pointer, so it lands in the same place for a keyboard activation (whose
	 *  click has no coordinates). */
	function openHeightMenu(e: MouseEvent, t: Track) {
		const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
		contextMenu.show(new MouseEvent('contextmenu', { clientX: r.left, clientY: r.bottom + 2 }), heightItems(t), e.currentTarget as Node);
	}

	/** The height choices for one track, the current one ticked. */
	const heightItems = (t: Track): MenuItem[] =>
		HEIGHT_PRESETS.map((p) => ({
			label: `${PRESET_LABEL[p]} height`,
			icon: ui.trackPreset(t.id) === p ? 'check' : undefined,
			shortcut: `${PRESET_PX[p]} px`,
			action: () => ui.setTrackHeight(t.id, p)
		}));

	// ---- interaction: select / razor split / drag-to-move --------------------

	type Drag = {
		clipId: string; // the clip the pointer grabbed
		kind: StreamKind;
		origTrackId: string;
		origStart: number;
		grabSec: number; // pointer offset within the clip (seconds)
		dur: number;
		/** Every clip that moves with it: the selection when the press was on one of
		 *  several selected clips, else just the clip itself. Fixed at the press. */
		members: ReadonlySet<string>;
		/** The press was on one of several selected clips — a click without a drag
		 *  then narrows the selection to it, as a click always does. */
		group: boolean;
		/** The press had Alt down: that narrowing is to the one clip, partners and all. */
		alone: boolean;
		start: number; // the grabbed clip's ghost start (seconds)
		trackId: string; // the grabbed clip's ghost destination track
		/** The pointer, in lane space — where the refusal's reason is written. */
		nx: number;
		ny: number;
		downX: number; // where the pointer went down, px: "moved" is judged on this
		moved: boolean;
	};
	let drag = $state<Drag | null>(null);

	/** Where the dragged group would land, and whether it may — recomputed from the pointer's
	 *  position, the cut and Alt (links on: the partners of the dragged clips are carried by the
	 *  same Δt on their own tracks and drawn as ghosts; Alt: they stay where they are). */
	const dragPlan = $derived.by((): MovePlan | null => {
		const d = drag;
		if (d?.moved) return planMove(drawnMoveTracks(editor.timeline), d.members, d.clipId, d.start, d.trackId, { links: !alt });
		const n = nudge;
		if (n && nudgeAt !== null) return planMove(drawnMoveTracks(editor.timeline), n.members, n.clipId, nudgeAt, n.trackId, { links: !n.alone });
		return null;
	});
	/** A run of arrow-key nudges on a focused clip: drawn where the clip will land, as a drag is, and
	 *  written as one edit once the keys go quiet (`nudge.ts`). */
	type Nudge = {
		clipId: string;
		trackId: string;
		members: ReadonlySet<string>;
		origStart: number;
		dur: number;
		/** The run so far, in frames of the cut. */
		frames: number;
		/** Alt was down: the clip alone, its linked partners left where they are. */
		alone: boolean;
		/** The edit is on its way to the backend: the clip stays drawn where it is going. */
		writing: boolean;
	};
	let nudge = $state<Nudge | null>(null);
	let nudgeTimer: ReturnType<typeof setTimeout> | null = null;
	/** Where the grabbed clip of the run would stand. */
	const nudgeAt = $derived.by(() =>
		nudge ? nudgedStart(nudge.origStart, nudge.dur, nudge.frames, clipEdges(nudge.trackId, nudge.members), fps) : null
	);
	/** The ghosts of the drag in progress, by the track each lands on. */
	const ghostsByTrack = $derived.by(() => {
		const m = new Map<string, Ghost[]>();
		for (const g of dragPlan?.ghosts ?? []) m.set(g.trackId, [...(m.get(g.trackId) ?? []), g]);
		return m;
	});
	/** Every clip the drag is moving, partners carried along included. */
	const dragIds = $derived(new Set(dragPlan?.ghosts.map((g) => g.clipId) ?? []));
	const dropRefused = $derived(!!dragPlan && !dragPlan.ok);

	/** Clips to light up as partners of the one under the pointer — not while a gesture is
	 *  under way, when the ghosts say what is moving. */
	const hoverPartners = $derived.by((): Set<string> => {
		if (!hoverId || drag || trimDrag || tg) return new Set();
		return new Set(linkPartners(editor.timeline, hoverId));
	});

	/** The part of a pointer event a gesture reads — so autoscroll can hand a gesture the pointer where
	 *  it is now, after the lanes moved under it, without an event. */
	type PointerLike = Pick<PointerEvent, 'clientX' | 'clientY' | 'altKey' | 'buttons'>;

	/** Pointer travel (px) before a press on a clip or an edge is a drag. Judged on
	 *  the pointer itself, never on the quantized position: at high zoom one pixel
	 *  is under a frame, so a 1 px jitter of a click can round to the next frame. */
	const DRAG_SLOP = 3;

	// ---- edge-dragging trim ---------------------------------------------------

	type TrimDrag = {
		clipId: string;
		trackId: string;
		edge: 'l' | 'r';
		/** The dragged edge's bounds, timeline seconds: stopped by the neighbours (`strict`), or
		 *  with ripple on, which pushes them along instead (`loose`). Each is asked twice — with the
		 *  clip's linked partners, whose own neighbours limit the edge they share, and without
		 *  (`*Alone`: Alt, which leaves the partners out). */
		strict: TrimBounds;
		loose: TrimBounds;
		strictAlone: TrimBounds;
		looseAlone: TrimBounds;
		origStart: number;
		origEnd: number;
		pos: number; // current ghost position of the dragged edge
		downX: number; // where the pointer went down, px
		grab: number; // pointer's offset from the edge it grabbed (seconds): the edge follows the pointer, not jumps to it
		moved: boolean;
		/** Why the edge is held where it is, when the pointer asked for more than the bounds allow. */
		limit: string | null;
		/** The pointer, in lane space — where a refusal's reason is written. */
		nx: number;
		ny: number;
	};
	let trimDrag = $state<TrimDrag | null>(null);

	/** The bounds in force: ripple lifts the neighbour clamps (it pushes them along),
	 *  and the mode is the project's, so it is asked again at every move and at the
	 *  release rather than trusted from the press. Alt leaves the partners out. */
	const trimLimits = (d: TrimDrag): TrimBounds =>
		editor.rippleMode ? (alt ? d.looseAlone : d.loose) : alt ? d.strictAlone : d.strict;

	/** What the trim in progress leaves on the cut — `linked-trim.ts`, over the faithful ports of
	 *  the backend's `carry_extent_edit`, `ripple_from` (its sync lock included) and sync guard:
	 *  the clip, the partners that follow its edge, every clip the ripple pushes and the partners
	 *  of *those*, each where it lands, on whichever track. Not the clip with its edge dragged: a
	 *  left-edge trim keeps the clip's start under ripple. Null while nothing but the clip itself
	 *  changes (no ripple, no partners) — its own ghost is drawn directly — and with Alt held the
	 *  partners are left out, as they are of the edit. */
	const trimView = $derived.by(() => {
		const d = trimDrag;
		if (!d?.moved) return null;
		const links = !alt && linkPartners(editor.timeline, d.clipId).length > 0;
		if (!editor.rippleMode && !links) return null;
		return linkedTrimPreview(editor.timeline, d.clipId, d.edge, d.pos, {
			ripple: editor.rippleMode,
			links: !alt,
			footage
		});
	});

	/** The cut as the gesture under way would leave it, for the GPU preview to draw while it runs
	 *  (`preview-draft.ts`): where the ghosts of a move or a nudge are, or the clip of a trim that moves
	 *  nothing else. A trim that ripples or takes partners along, and the roll / slip / slide tools (whose
	 *  trim monitor stands in for the frame), leave the frame as the cut is until they are let go. */
	const gestureDraft = $derived.by((): PreviewDraft | null => {
		if (dragPlan) return moveDraft(editor.timeline, dragPlan);
		const d = trimDrag;
		if (!d?.moved || trimView) return null;
		const clip = editor.timeline.tracks.find((t) => t.id === d.trackId)?.clips.find((c) => c.id === d.clipId);
		if (!clip) return null;
		const limits = trimLimits(d);
		return trimDraft(editor.timeline, d.clipId, trimEdit(clip, d.edge, clampEdge(d.pos, limits.min, limits.max)));
	});
	/** The draft of a gesture that was let go, kept until its edit has landed: the picture does not step
	 *  back to the cut before it in between. */
	let landing = $state.raw<PreviewDraft | null>(null);
	function landWith(draft: PreviewDraft | null, edit: Promise<unknown>) {
		landing = draft;
		void edit.finally(() => {
			if (landing === draft) landing = null;
		});
	}
	/** The timeline's drafts are its own: another gesture's (an Inspector row's) is never cleared by it. */
	const draftOwner = Symbol('timeline');
	$effect(() => {
		ui.setDraft(draftOwner, gestureDraft ?? landing);
	});
	$effect(() => () => ui.setDraft(draftOwner, null));

	function onEdgePointerDown(e: PointerEvent, c: Clip, t: Track, edge: 'l' | 'r') {
		if (e.button !== 0 || ui.tool !== 'pointer') return; // the other tools own the clip body
		if (t.locked) return;
		e.stopPropagation();
		alt = e.altKey;
		editor.selectClip(c.id, 'replace', e.altKey);
		void editor.select(c.asset_id);
		if (holdForReview(e)) return;
		const laneLeft =
			((e.currentTarget as HTMLElement).closest('[data-lane]') as HTMLElement | null)?.getBoundingClientRect().left ?? 0;
		const asset = editor.assets.find((a) => a.id === c.asset_id);
		// A still image loops, so its source window can grow without limit.
		const still = asset?.streams.some((s) => s.image) ?? false;
		const start = c.timeline_start;
		const end = start + clipDuration(c);
		const clips = [...(editor.timeline.tracks.find((tr) => tr.id === t.id)?.clips ?? [])].sort(
			(a, b) => a.timeline_start - b.timeline_start
		);
		trimDrag = {
			clipId: c.id,
			trackId: t.id,
			edge,
			strict: linkedTrimBounds(editor.timeline, c, edge, asset?.duration, still, false),
			loose: linkedTrimBounds(editor.timeline, c, edge, asset?.duration, still, true),
			strictAlone: trimBounds(c, edge, clips, asset?.duration, still, false),
			looseAlone: trimBounds(c, edge, clips, asset?.duration, still, true),
			origStart: start,
			origEnd: end,
			pos: edge === 'l' ? start : end,
			downX: e.clientX,
			grab: laneTime(e.clientX, laneLeft) - (edge === 'l' ? start : end),
			moved: false,
			limit: null,
			nx: 0,
			ny: 0
		};
		capturePointer(e);
	}

	function onTrimMove(e: PointerLike) {
		if (!trimDrag) return;
		const lane = documentOf(scroller).querySelector(`[data-lane][data-track-id="${trimDrag.trackId}"]`) as HTMLElement | null;
		const laneLeft = lane?.getBoundingClientRect().left ?? 0;
		// Where the pointer says the edge is (it keeps the offset it grabbed it at),
		// rounded once: that is the ghost, and the commit.
		const raw = laneTime(e.clientX, laneLeft) - trimDrag.grab;
		const { min, max } = trimLimits(trimDrag);
		const asked = snapPoint(raw, trimDrag.trackId, trimDrag.clipId);
		const pos = clampEdge(asked, min, max);
		// The pointer went past what the clip may be trimmed to: the readout says what stopped it.
		const free = alt ? trimDrag.looseAlone : trimDrag.loose;
		const limit =
			asked < min - 1e-9
				? trimLimitWhy(trimDrag.edge, 'min', { min, max }, free)
				: asked > max + 1e-9
					? trimLimitWhy(trimDrag.edge, 'max', { min, max }, free)
					: null;
		const moved = trimDrag.moved || Math.abs(e.clientX - trimDrag.downX) >= DRAG_SLOP;
		const box = lanesEl?.getBoundingClientRect();
		trimDrag = { ...trimDrag, pos, moved, limit, nx: box ? e.clientX - box.left : 0, ny: box ? e.clientY - box.top : 0 };
	}

	function onTrimUp() {
		if (!trimDrag) return;
		const d = trimDrag;
		trimDrag = null;
		if (!d.moved) return;
		// The mode can flip mid-drag (the R key, an agent): the position is held to
		// what is legal now, as the ghost was.
		const limits = trimLimits(d);
		const pos = clampEdge(d.pos, limits.min, limits.max);
		// A drag that ended on the frame the edge was already on writes nothing.
		if (Math.abs(pos - (d.edge === 'l' ? d.origStart : d.origEnd)) < 1e-9) return;
		const track = editor.timeline.tracks.find((t) => t.id === d.trackId);
		const clip = track?.clips.find((c) => c.id === d.clipId);
		if (!track || !clip) return;
		// Under ripple the neighbours are pushed along, and linked partners follow the edge —
		// unless the backend would decline to (a ripple it hands back as made would leave this
		// clip over its neighbour; a locked partner, or a pair pulled out of step, it refuses):
		// the ghost was red, and letting go changes nothing. A refusal with a reason says it.
		if (editor.rippleMode || (!alt && linkPartners(editor.timeline, d.clipId).length > 0)) {
			const view = linkedTrimPreview(editor.timeline, d.clipId, d.edge, pos, {
				ripple: editor.rippleMode,
				links: !alt,
				footage
			});
			if (!view?.ok) {
				if (view?.reason) toast.error(view.reason);
				return;
			}
		}
		// `pos` is the gesture's one rounded position (the ghost drew it too); every
		// field the trim writes comes from it. Alt trims the clip alone (`link: false`).
		const e = trimEdit(clip, d.edge, pos);
		const alone = !editor.rippleMode && (alt || linkPartners(editor.timeline, d.clipId).length === 0);
		landWith(
			alone ? trimDraft(editor.timeline, d.clipId, e) : null,
			editor.trim(d.clipId, e.source_in, e.source_out, e.timeline_start, alt ? false : undefined).catch(err)
		);
	}

	/** The frame rate gestures are quantized to — the cut's own (`export_format`'s). */
	const fps = $derived(editor.fps);

	/** Every clip edge on a track but the excepted clips' own — the one being moved,
	 *  or all of a dragged group, which travel with it and so are nothing to butt
	 *  against: what a placement butts against. */
	function clipEdges(trackId: string, except: string | ReadonlySet<string>): number[] {
		const skip = typeof except === 'string' ? (id: string) => id === except : (id: string) => except.has(id);
		const out: number[] = [];
		for (const c of editor.timeline.tracks.find((t) => t.id === trackId)?.clips ?? []) {
			if (!skip(c.id)) out.push(c.timeline_start, c.timeline_start + clipDuration(c));
		}
		return out;
	}

	/** Where a dragged edge (or a cut) lands: a magnet within reach — 0, the
	 *  playhead, a bar or a beat (a bar wins over a nearer beat), a clip edge — when snapping is on, otherwise the nearest
	 *  frame. Frames are not a magnet: they apply with snapping off too. A landing
	 *  within a hair of a neighbour's edge is that edge exactly, magnet or not. */
	function snapPoint(time: number, trackId: string, except: string | ReadonlySet<string>): number {
		const edges = clipEdges(trackId, except);
		return quantizeTime(time, {
			fps,
			magnets: ui.snap
				? [0, ui.time, ...gridMagnets(time, [0], beatTimes, barTimes, 8 / pxPerSec), ...edges]
				: [],
			threshold: 8 / pxPerSec,
			welds: edges
		});
	}

	const laneTime = (clientX: number, laneLeft: number) => (clientX - laneLeft) / pxPerSec;

	const err = report;
	/** The rows of a menu that edit the cut, off while the proposal is previewed. */
	const held = (items: MenuItem[]): MenuItem[] => heldForPreview(items, editor.previewingStaged);

	// The tools, now that the toolbar row is gone: [tool, icon, name, the action whose
	// key selects it] — the tooltip is read from the keymap, so it shows the key the user
	// actually has. The three that move a boundary rather than a clip are set apart, and
	// their tooltips say what the drag does, since the glyphs alone do not.
	const TOOLS: [Tool, string, string, ActionId][] = [
		['pointer', 'MousePointer2', 'Select', 'tool.pointer'],
		['razor', 'Scissors', 'Razor', 'tool.razor']
	];
	const TRIM_TOOLS: [TrimTool, string, string, ActionId][] = [
		['roll', 'separator-vertical', 'Roll', 'tool.roll'],
		['slip', 'gallery-horizontal', 'Slip', 'tool.slip'],
		['slide', 'arrow-left-right', 'Slide', 'tool.slide']
	];

	// The frame the cut is made for. Changing it reshapes the preview, the scrubbed still
	// and the export together, so the vertical crop is something you compose against
	// rather than discover in the rendered file.
	const delivery = $derived(presetFor(editor.timeline.format));

	function pickDelivery(e: MouseEvent) {
		const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
		contextMenu.show(
			new MouseEvent('contextmenu', { clientX: r.right, clientY: r.bottom + 2 }),
			DELIVERY_PRESETS.map((p) => ({
				label: p.label === 'Source' ? 'Source shape' : `${p.label} — ${p.hint}`,
				icon: p.id === delivery.id ? 'check' : undefined,
				action: () => void setDeliveryPreset(p.id)
			})),
			e.currentTarget as Node
		);
	}

	function onClipPointerDown(e: PointerEvent, c: Clip, t: Track) {
		if (e.button !== 0) return;
		const lane = (e.currentTarget as HTMLElement).closest('[data-lane]') as HTMLElement | null;
		const laneLeft = lane?.getBoundingClientRect().left ?? 0;
		const mode = pickMode(e);
		// Alt is the escape hatch from links: the press selects the clip alone, and whatever
		// it starts edits the clip alone.
		const alone = e.altKey;
		alt = alone;
		if (t.locked) {
			// Still selectable — locking guards the edit, not the view.
			e.stopPropagation();
			editor.selectClip(c.id, mode, alone);
			void editor.select(c.asset_id);
			return;
		}
		if (ui.tool === 'razor') {
			e.stopPropagation();
			if (holdForPreview()) return;
			// Snapped like any other placement, then held half a frame inside the clip.
			const at = splitPoint(
				snapPoint(laneTime(e.clientX, laneLeft), t.id, ''),
				c.timeline_start,
				c.timeline_start + clipDuration(c),
				fps
			);
			// A linked clip is cut with its partners at the same moment; Alt cuts this one alone.
			razor = null;
			if (at === null) toast.error('That clip is too short to cut on a frame');
			else void editor.split(c.id, at, alone ? false : undefined).catch(err);
			return;
		}
		if (ui.tool === 'roll' || ui.tool === 'slip' || ui.tool === 'slide') {
			// These act on one clip (or the cut beside it), so the press selects it, as
			// a plain press does; a modifier press is a selection gesture and nothing more.
			editor.selectClip(c.id, mode, alone);
			void editor.select(c.asset_id);
			if (mode !== 'replace') {
				e.stopPropagation();
				return;
			}
			if (holdForReview(e)) return;
			beginTrimTool(e, c, t, laneLeft);
			return;
		}
		// Pressing a clip that is one of several selected keeps them all: it is the
		// start of a group drag. (A click that never becomes one narrows the
		// selection to the clip when the button comes up.)
		// Alt on a selection that is only the clip's own link group narrows it to the clip at once.
		const others = editor.selectedClipIds.filter((id) => id !== c.id);
		const group =
			mode === 'replace' &&
			editor.isSelected(c.id) &&
			editor.selectedClipIds.length > 1 &&
			!(alone && onlyPartners(editor.timeline, c.id, others));
		if (group) editor.setPrimary(c.id);
		else editor.selectClip(c.id, mode, alone);
		void editor.select(c.asset_id);
		// A modifier click is a selection gesture, not the start of a drag.
		if (mode !== 'replace') {
			e.stopPropagation();
			return;
		}
		if (holdForReview(e)) return;
		drag = {
			clipId: c.id,
			kind: t.kind,
			origTrackId: t.id,
			origStart: c.timeline_start,
			grabSec: laneTime(e.clientX, laneLeft) - c.timeline_start,
			dur: clipDuration(c),
			members: new Set(group ? editor.selectedClipIds : [c.id]),
			group,
			alone,
			start: c.timeline_start,
			trackId: t.id,
			nx: 0,
			ny: 0,
			downX: e.clientX,
			moved: false
		};
		capturePointer(e);
	}

	function laneUnder(clientX: number, clientY: number): HTMLElement | null {
		for (const el of documentOf(scroller).elementsFromPoint(clientX, clientY)) {
			if (el instanceof HTMLElement && el.dataset.lane !== undefined) return el;
		}
		return null;
	}

	/** Where a clip of `dur` seconds placed near `start` lands: a magnet — 0, the
	 *  playhead, a bar or a beat, a clip edge, either of its own edges against any of them —
	 *  when snapping is on, otherwise the nearest frame for its start. */
	function snapStart(start: number, trackId: string, except: string | ReadonlySet<string>, dur: number): number {
		const edges = clipEdges(trackId, except);
		const magnets: number[] = [];
		if (ui.snap) {
			magnets.push(0, ui.time);
			// land either edge on a bar, else on a beat
			magnets.push(...gridMagnets(start, [0, dur], beatTimes, barTimes, 8 / pxPerSec));
			// align heads, butt after, butt before
			// `startBefore`, not `edge - dur`: the latter can end an ULP past the edge it butts.
			for (let i = 0; i < edges.length; i += 2) magnets.push(edges[i], edges[i + 1], startBefore(edges[i], dur));
		}
		return quantizeSpanStart(start, dur, { fps, magnets, threshold: 8 / pxPerSec, welds: edges });
	}

	// A lost pointerup (released outside the window, over a native dialog, an
	// OS-level drag-cancel, or a mid-drag right-click) must never let the next
	// unrelated pointerup — a click somewhere else entirely — be mistaken for
	// the end of a still-open drag and commit it at the wrong position.
	// `setPointerCapture` (below, at each pointerdown) keeps move/up events
	// routed here even once the pointer leaves the dragged element or the
	// window; `pointercancel` and a window `blur` are the remaining escapes,
	// and the `buttons` check below is the last-resort net if even those are
	// missed.

	let capturedEl: Element | null = null;
	let capturedPointerId: number | null = null;

	function capturePointer(e: PointerEvent) {
		const el = e.currentTarget as Element;
		try {
			el.setPointerCapture(e.pointerId);
			capturedEl = el;
			capturedPointerId = e.pointerId;
		} catch {
			// Best-effort — a capture that fails leaves the window-level
			// listeners as the fallback, same as before this fix.
		}
	}

	function releaseCapture() {
		if (capturedEl && capturedPointerId !== null) {
			try {
				capturedEl.releasePointerCapture(capturedPointerId);
			} catch {
				// already released
			}
		}
		capturedEl = null;
		capturedPointerId = null;
	}

	/** Clear every drag/scrub state WITHOUT committing anything — the pointer
	 *  is gone and whatever position it last reported is not trustworthy. */
	function resetDragState() {
		// A scrub and a mark drag change the view and the marks as they go; given up, they go back.
		if (scrubbing && scrubFrom !== null) ui.restoreTime(scrubFrom);
		if (markDrag) {
			ui.markIn = markFrom.in;
			ui.markOut = markFrom.out;
		}
		drag = null;
		abandonNudge();
		trimDrag = null;
		titleDrag = null;
		scrubbing = false;
		markDrag = null;
		markerDrag = null;
		previewPress = null;
		// A marquee abandoned gives the selection back as it found it — and the click
		// that still follows when the button comes up is not a click on the lane
		// (it would seek, and clear what was just restored).
		if (marquee) {
			const { base } = marquee;
			marquee = null;
			editor.selectClips(base.ids, base.primary);
			swallowClick = true;
		}
		releaseCapture();
	}

	function onPointerCancel() {
		resetDragState();
	}

	function onWindowBlur() {
		alt = false;
		resetDragState();
	}

	// ---- while a gesture is held: scrolling, magnets, readouts -----------------------
	//
	// Autoscroll (`autoscroll.ts`): a drag, trim, marquee, roll / slip / slide, scrub or marker /
	// mark move that pushes into the band along the edge of the window scrolls it, and the gesture
	// re-reads the pointer — the lanes moved under it. The lane widens with the gesture, so there
	// is room to go on past the end of the cut. Snap lines: the magnet an edge landed on is named
	// (`snap.ts`) and drawn; the readouts are the gesture's number beside the pointer
	// (`gesture-readout.ts`).

	/** The pointer's last client position, and where the press that began the gesture was. */
	let pointer = { x: 0, y: 0 };
	let press = { x: 0, y: 0 };
	/** The buttons the last real pointer event held: a re-read after a scroll says the same, so a release
	 *  that was missed is still the net it always was (`onPointerMove`). */
	let heldButtons = 1;

	const gestureLive = $derived.by(
		() => !!(drag?.moved || trimDrag?.moved || tg?.moved || titleDrag?.moved || marquee?.moved || scrubbing || markDrag || markerDrag)
	);
	/** Whether the gesture can go between tracks, and so scrolls vertically too. */
	const gestureTall = $derived.by(() => !!(drag?.moved || marquee?.moved));

	// A long lane draws the clips in (or near) view, and the narrow ones among them as blocks on one
	// canvas (`timeline-cull.ts`, `LaneBlocks.svelte`). What the pointer or the keyboard holds is a clip
	// wherever it is — the pressed clip, the one under the pointer, the focused and the nudged one;
	// what a gesture only moves is drawn from the model (the ghosts), so it is not kept.
	const pinned = $derived.by(() =>
		pinnedClips({
			pressed: [
				drag?.clipId,
				trimDrag?.clipId,
				tg?.edit?.tool === 'roll' ? tg.edit.a : tg?.edit?.clipId,
				tg?.edit?.tool === 'roll' ? tg.edit.b : null
			],
			hovered: hoverId,
			focused: focusId,
			nudged: nudge?.clipId
		})
	);
	const laneDrawings = $derived.by(() => {
		const m = new Map<string, LaneDrawing<Clip>>();
		for (const t of drawn) m.set(t.id, laneDrawing(t.clips, clipDuration, pxPerSec, viewLo, viewHi, pinned));
		return m;
	});
	/** What a block shows of its clip: the selection, a proposal's marks, a hovered clip's partners. */
	const markOf = (c: Clip) =>
		blockMark(c.id, { selected: editor.selectedSet, diff: diff?.clips, touched: touched?.clips, lit: hoverPartners });
	/** A block moved by the gesture under way, faded as its button would be. */
	const movedBlock = (id: string) =>
		((drag?.moved || !!nudge) && dragIds.has(id)) || !!trimView?.shifted.has(id) || (!!tg?.moved && slidPartners.has(id));
	/** A track's clip colours as a canvas takes them, once per theme change. */
	const blockColors = $derived.by(() => {
		void settings.theme;
		return {
			video: { fill: resolveColor('--track-video'), edge: resolveColor('--track-video-edge') },
			audio: { fill: resolveColor('--track-audio'), edge: resolveColor('--track-audio-edge') },
			marks: {
				selected: resolveColor('--kerf-400'),
				added: resolveColor('--diff-add'),
				shifted: resolveColor('--diff-shift'),
				touched: resolveColor('--agent-400'),
				lit: resolveColor('--kerf-300')
			}
		};
	});

	/** How far right the gesture has reached, seconds — what widens the lane. */
	const gestureEnd = $derived.by((): number | null => {
		if ((drag?.moved || nudge) && dragPlan) return Math.max(...dragPlan.ghosts.map((g) => g.start + g.dur));
		if (trimDrag?.moved) return trimDrag.edge === 'r' ? trimDrag.pos : trimDrag.origEnd;
		if (titleDrag?.moved) return titleDrag.end;
		if (tg?.moved && tgPreview) return Math.max(...tgPreview.ghosts.map((g) => g.start + g.dur));
		if (dropGhost) return dropGhost.start + dropGhost.dur;
		return null;
	});

	/** The part of the scroller the lanes show: right of the sticky headers, under the sticky ruler. */
	function scrollRegion(): ScrollRegion | null {
		const v = viewport();
		if (!v) return null;
		const r = v.el.getBoundingClientRect();
		return {
			left: r.left + v.headerW,
			right: r.left + v.el.clientWidth,
			top: r.top + (rulerEl?.offsetHeight ?? 0),
			bottom: r.top + v.el.clientHeight
		};
	}

	/** The gestures read the pointer again once the view has moved under it. */
	function rereadPointer() {
		const e: PointerLike = { clientX: pointer.x, clientY: pointer.y, altKey: alt, buttons: heldButtons };
		onPointerMove(e);
		if (tg) onTrimToolMove(e);
	}

	$effect(() => {
		if (!gestureLive) return;
		const el = scroller;
		if (!el) return;
		const win = windowOf(el);
		const loop = createAutoscroll(
			{
				region: scrollRegion,
				pointer: () => pointer,
				scrollBy(dx, dy) {
					const x = el.scrollLeft;
					const y = el.scrollTop;
					if (dx) el.scrollLeft = x + dx;
					if (dy) el.scrollTop = y + dy;
					return { x: el.scrollLeft - x, y: el.scrollTop - y };
				},
				scrolled: rereadPointer,
				now: () => win.performance.now(),
				request: (cb) => win.requestAnimationFrame(() => cb()),
				cancel: (id) => win.cancelAnimationFrame(id)
			},
			{ x: true, y: untrack(() => gestureTall) }
		);
		loop.start(press);
		// A pointer that lost its capture can no longer be trusted to report its release: stop scrolling.
		const lost = () => loop.stop();
		win.addEventListener('lostpointercapture', lost);
		return () => {
			win.removeEventListener('lostpointercapture', lost);
			loop.stop();
		};
	});

	/** Every clip edge of the cut, for a title's snap. */
	function allClipEdges(): number[] {
		const out: number[] = [];
		for (const t of editor.timeline.tracks) for (const c of t.clips) out.push(c.timeline_start, c.timeline_start + clipDuration(c));
		return out;
	}

	/** The marks a gesture on `trackId` snaps to — the very set `snapPoint` / `snapStart` offer —
	 *  named, so the one an edge landed on can be told. */
	function magnetsOn(trackId: string, except: string | ReadonlySet<string>): Magnet[] {
		const lane = editor.timeline.tracks.find((t) => t.id === trackId);
		return magnetList({ playhead: ui.time, beats: beatTimes, bars: barTimes, edges: clipEdges(trackId, except), lane: lane?.name });
	}

	/** The lines to draw while a gesture holds an edge on a magnet: where, and what it is. */
	const snapNow = $derived.by((): SnapLine[] => {
		if (!ui.snap) return [];
		const d = drag;
		if (d?.moved) return snapLines([d.start, d.start + d.dur], magnetsOn(d.trackId, d.members));
		const t = trimDrag;
		if (t?.moved) return snapLines([t.pos], magnetsOn(t.trackId, t.clipId));
		const td = titleDrag;
		if (td?.moved) {
			const others = otherTitleEdges(td.id);
			const marks = magnetList({ playhead: ui.time, beats: beatTimes, bars: barTimes, edges: allClipEdges(), titles: others });
			return snapLines(td.mode === 'move' ? [td.start, td.end] : td.mode === 'l' ? [td.start] : [td.end], marks);
		}
		const g = tg;
		if (g?.moved && tgPreview?.ok && g.edit) {
			if (g.edit.tool === 'roll') return snapLines([g.origin + tgPreview.applied], magnetsOn(g.trackId, new Set([g.edit.a, g.edit.b])));
			if (g.edit.tool === 'slide') {
				const track = editor.timeline.tracks.find((x) => x.id === g.trackId);
				const at = g.origin + tgPreview.applied;
				if (track) return snapLines([at, at + g.dur], magnetsOn(g.trackId, slideMembers(track, g.edit.clipId)));
			}
		}
		const drop = dropGhost;
		if (drop) return snapLines([drop.start, drop.start + drop.dur], magnetsOn(drop.trackId, ''));
		return [];
	});

	// The razor: the cut it would make, drawn through the clip under the pointer and the partners that
	// are cut with it, before anything is pressed.
	let razor = $state<{ trackId: string; clipId: string; at: number | null; nx: number; ny: number } | null>(null);
	$effect(() => {
		if (ui.tool !== 'razor') razor = null;
	});
	/** The lanes the cut goes through: the clip's own, and its linked partners' unless Alt. */
	const razorLanes = $derived.by((): Map<string, string> => {
		const r = razor;
		const lanes = new Map<string, string>();
		if (!r || r.at === null) return lanes;
		const own = editor.timeline.tracks.find((t) => t.id === r.trackId);
		lanes.set(r.trackId, own?.name ?? '');
		if (alt) return lanes;
		for (const id of linkPartners(editor.timeline, r.clipId)) {
			const track = editor.timeline.tracks.find((t) => t.clips.some((c) => c.id === id));
			const clip = track?.clips.find((c) => c.id === id);
			if (track && clip && r.at > clip.timeline_start && r.at < clip.timeline_start + clipDuration(clip)) lanes.set(track.id, track.name);
		}
		return lanes;
	});
	/** What the cut is on when it lands on a magnet, for its label. */
	const razorMagnet = $derived(razor && razor.at !== null && ui.snap ? markAt(razor.at, magnetsOn(razor.trackId, '')) : null);

	/** The number beside the pointer for the gestures that had none: an edge trim, a clip move and
	 *  the razor (roll / slip / slide's own is drawn with them). */
	const chip = $derived.by((): { r: Readout; x: number; y: number } | null => {
		const d = drag;
		if (d?.moved && dragPlan) {
			const track = editor.timeline.tracks.find((t) => t.id === d.trackId);
			return {
				r: moveReadout({
					delta: d.start - d.origStart,
					start: d.start,
					track: track?.name ?? '',
					fps,
					count: d.members.size,
					refused: dropRefused ? dragPlan.reason || 'that drop would not fit' : null,
					mode: timeDisplay.mode
				}),
				x: d.nx,
				y: d.ny
			};
		}
		const t = trimDrag;
		if (t?.moved) {
			let r = trimEdgeReadout({ edge: t.edge, start: t.origStart, end: t.origEnd, pos: t.pos, fps, limit: t.limit, mode: timeDisplay.mode });
			// A trim the backend would turn down, or one that cuts a partner's sound back, says so here.
			if (trimView && !trimView.ok && trimView.reason) r = { ...r, detail: trimView.reason, tone: 'refused' };
			else if (trimView?.ok && trimView.trimmed.length > 0) {
				r = { ...r, detail: `Cuts back the sound on ${trimView.trimmed.join(', ')} to make room`, tone: 'limit' };
			}
			return { r, x: t.nx, y: t.ny };
		}
		const z = razor;
		if (z && ui.tool === 'razor' && !drag && !tg) {
			const along = [...razorLanes].filter(([id]) => id !== z.trackId).map(([, name]) => name);
			const r = cutReadout({ at: z.at, fps, along, mode: timeDisplay.mode });
			return { r: razorMagnet ? { ...r, title: `${r.title} · ${razorMagnet.label}` } : r, x: z.nx, y: z.ny };
		}
		return null;
	});

	// The frame at an edge being trimmed, in the Preview, as a roll's two frames are.
	let edgeMonitorOn = false;
	$effect(() => {
		const d = trimDrag;
		const c = chip;
		const track = d ? editor.timeline.tracks.find((t) => t.id === d.trackId) : undefined;
		const clip = d ? track?.clips.find((x) => x.id === d.clipId) : undefined;
		if (d?.moved && c && track && clip) {
			ui.trimMonitor = trimEdgeMonitor(clip, d.edge, d.pos, fps, track.kind, c.r, timeDisplay.mode);
			edgeMonitorOn = true;
		} else if (edgeMonitorOn) {
			ui.trimMonitor = null;
			edgeMonitorOn = false;
		}
	});
	$effect(() => () => {
		if (edgeMonitorOn) ui.trimMonitor = null;
	});

	// ---- arrow-key nudge of a focused clip -------------------------------------------------

	function clearNudgeTimer() {
		if (nudgeTimer !== null) clearTimeout(nudgeTimer);
		nudgeTimer = null;
	}

	function abandonNudge() {
		clearNudgeTimer();
		nudge = null;
	}

	/** Write the run as ONE move — the plan is the drag's own, so ripple-free placement, links and
	 *  the lane checks are the same — and keep it drawn until the timeline has come back. */
	function commitNudge() {
		const n = nudge;
		if (!n || n.writing) return;
		clearNudgeTimer();
		const plan = dragPlan;
		if (!plan || plan.noop) {
			nudge = null;
			return;
		}
		if (!plan.ok) {
			nudge = null;
			if (plan.reason) toast.error(plan.reason[0].toUpperCase() + plan.reason.slice(1));
			return;
		}
		nudge = { ...n, writing: true };
		void editor
			.moveClips(plan.moves, plan.link ? undefined : false)
			.catch(err)
			.finally(() => (nudge = null));
	}

	function onClipKey(e: KeyboardEvent, c: Clip, t: Track) {
		if (e.ctrlKey || e.metaKey) return;
		if (e.key === 'Enter' && nudge) {
			e.preventDefault();
			e.stopPropagation();
			commitNudge();
			return;
		}
		const k = nudgeFrames(e.key, e.shiftKey, fps);
		if (k === null || t.locked || ui.tool !== 'pointer' || drag || trimDrag || tg) return;
		// The proposal on screen is read-only: say so rather than start a nudge that cannot be written.
		if (holdForPreview()) {
			e.preventDefault();
			e.stopPropagation();
			return;
		}
		// The arrows are this clip's while it has the keyboard; the playhead's keys stand down for it.
		e.preventDefault();
		e.stopPropagation();
		if (nudge?.writing) return;
		if (nudge) {
			nudge = { ...nudge, frames: nudge.frames + k };
		} else {
			if (!editor.isSelected(c.id)) {
				editor.selectClip(c.id, 'replace', e.altKey);
				void editor.select(c.asset_id);
			}
			const group = editor.selectedClipIds.length > 1 ? editor.selectedClipIds : [c.id];
			nudge = {
				clipId: c.id,
				trackId: t.id,
				members: new Set(group),
				origStart: c.timeline_start,
				dur: clipDuration(c),
				frames: k,
				alone: e.altKey,
				writing: false
			};
		}
		clearNudgeTimer();
		nudgeTimer = setTimeout(commitNudge, NUDGE_QUIET_MS);
	}
	$effect(() => () => clearNudgeTimer());

	function onPointerMove(e: PointerLike) {
		alt = e.altKey;
		pointer = { x: e.clientX, y: e.clientY };
		heldButtons = e.buttons;
		if (previewPress) {
			// Released, or moved far enough to be a drag: either way the press is over, and a drag says why nothing moved.
			const p = previewPress;
			if ((e.buttons & 1) === 0) previewPress = null;
			else if (Math.hypot(e.clientX - p.x, e.clientY - p.y) >= DRAG_SLOP) {
				previewPress = null;
				holdForPreview();
			}
			return;
		}
		// The primary button is up but we never saw pointerup for it — treat
		// exactly like a cancel rather than trust a move that outran its release.
		if ((drag || trimDrag || titleDrag || marquee || scrubbing || markDrag || markerDrag) && (e.buttons & 1) === 0) {
			resetDragState();
			return;
		}
		if (scrubbing) {
			ui.seek(rulerTime(e.clientX));
			return;
		}
		if (markerDrag) {
			markerDrag = { ...markerDrag, time: rulerTime(e.clientX) };
			return;
		}
		if (markDrag) {
			const t = rulerTime(e.clientX);
			// The pair stays ordered, matching how I/O set them from the keyboard.
			if (markDrag === 'in') ui.markIn = Math.min(t, ui.markOut ?? Infinity);
			else ui.markOut = Math.max(t, ui.markIn ?? 0);
			return;
		}
		if (titleDrag) {
			onTitleDragMove(e);
			return;
		}
		if (trimDrag) {
			onTrimMove(e);
			return;
		}
		if (marquee) {
			onMarqueeMove(e.clientX, e.clientY);
			return;
		}
		if (!drag) return;
		const lane = laneUnder(e.clientX, e.clientY);
		let trackId = drag.trackId;
		let laneLeft: number;
		if (lane && lane.dataset.kind === drag.kind && !isLocked(lane.dataset.trackId!)) {
			trackId = lane.dataset.trackId!;
			laneLeft = lane.getBoundingClientRect().left;
		} else {
			const cur = documentOf(scroller).querySelector(`[data-lane][data-track-id="${trackId}"]`) as HTMLElement | null;
			laneLeft = cur?.getBoundingClientRect().left ?? 0;
		}
		// The grabbed clip is snapped and frame-quantized exactly as a lone clip is
		// (its group's own edges are no magnet: they move with it); the rest of the
		// group keeps its offsets from it, and the plan says whether that all fits.
		const start = snapStart(laneTime(e.clientX, laneLeft) - drag.grabSec, trackId, drag.members, drag.dur);
		const movedEnough =
			drag.moved || trackId !== drag.origTrackId || Math.abs(e.clientX - drag.downX) >= DRAG_SLOP;
		const box = lanesEl?.getBoundingClientRect();
		drag = {
			...drag,
			start: movedEnough ? start : drag.start,
			trackId,
			moved: movedEnough,
			nx: box ? e.clientX - box.left : 0,
			ny: box ? e.clientY - box.top : 0
		};
	}

	function onPointerUp(e: PointerEvent) {
		alt = e.altKey;
		previewPress = null;
		releaseCapture();
		if (scrubbing) {
			scrubbing = false;
			return;
		}
		if (markerDrag) {
			const d = markerDrag;
			markerDrag = null;
			const m = editor.markers.find((x) => x.id === d.id);
			if (m && Math.abs(m.time - d.time) > 1e-6) void editor.updateMarker(d.id, { time: d.time }).catch(err);
			return;
		}
		if (markDrag) {
			markDrag = null;
			return;
		}
		if (titleDrag) {
			onTitleDragUp();
			return;
		}
		if (trimDrag) {
			onTrimUp();
			return;
		}
		if (marquee) {
			onMarqueeUp();
			return;
		}
		if (!drag) return;
		const d = drag;
		drag = null;
		if (!d.moved) {
			// A click on one clip of a selection narrows it to that clip. It selects and nothing more: the
			// playhead stays where it was parked, so a clip can be picked and then cut at the playhead.
			if (d.group) editor.selectClip(d.clipId, 'replace', d.alone);
			return;
		}
		// Asked again, as the ghosts were: the cut, the pointer's last place and Alt are what count now.
		const plan = planMove(drawnMoveTracks(editor.timeline), d.members, d.clipId, d.start, d.trackId, { links: !alt });
		// A refused drop — an overlap, before 0, a locked or missing lane, a partner that does not
		// fit — was red all the way; letting go changes nothing. A drop that moves nothing too.
		if (!plan.ok || plan.noop) return;
		// The group lands as ONE edit: one revision, one undo. Its linked partners are the
		// backend's to carry (the same Δt on their own tracks), unless Alt left them out.
		landWith(moveDraft(editor.timeline, plan), editor.moveClips(plan.moves, plan.link ? undefined : false).catch(err));
	}

	// ---- roll / slip / slide: the tools that move a boundary ------------------------
	//
	// Roll moves the cut between two touching clips, slip moves a clip's footage under
	// its fixed edges, slide moves a clip while the clips touching it give way. The
	// edits and everything a drag needs of them are pure and live in `trim-tools.ts`
	// (the range the pointer is held to, the ghost as the *outcome*, the words, the
	// frames the Preview shows); this is the pointer plumbing. Each gesture is
	// `beginDrag` — pointer capture, Escape / cancel / lost capture / blur abandon —
	// takes one position from the raw pointer and rounds it once (`frames.ts`), shows
	// the result live without writing, and writes ONE edit on release. A drag that
	// hits a limit says so on release, and the backend is asked for what is legal
	// *then*, as the ghost was.

	type TrimGesture = {
		tool: TrimTool;
		trackId: string;
		/** What it edits; null for a roll pressed away from any cut, which is only a click. */
		edit: GestureEdit | null;
		/** The cut's time (roll) or the clip's start (slide): what the pointer is measured from. */
		origin: number;
		/** The pointer's offset from `origin` at the press (seconds): the cut or the clip
		 *  follows the pointer rather than jumping to it. */
		grab: number;
		/** Where the press was on the time axis. */
		pressTime: number;
		downX: number;
		/** How far the view was scrolled at the press, px: a slip is measured in lane space, so
		 *  what autoscroll has moved since counts as pointer travel. */
		downScroll: number;
		/** The pressed clip's length and speed. */
		dur: number;
		speed: number;
		/** What the pointer asks for (rounded once, not yet held to the range). */
		requested: number;
		moved: boolean;
		/** The pointer in lane space — where the readout is written. */
		nx: number;
		ny: number;
	};
	let tg = $state<TrimGesture | null>(null);
	/** What the gesture would leave — the edit run on a scratch copy of the lanes it reaches
	 *  (with the clip's linked partners' too, unless Alt is down), after holding the pointer to
	 *  the tightest range of the group. Derived, so Alt, an agent's edit or a flipped mode redraws it. */
	const tgPreview = $derived.by((): EditPreview | null => {
		const g = tg;
		if (!g?.moved || !g.edit) return null;
		return previewEdit(editor.timeline, g.edit, g.requested, footage, !alt);
	});
	/** The clips a slide carries along: the slid clip's linked partners. */
	const slidPartners = $derived.by((): Set<string> => {
		const g = tg;
		if (!g?.moved || g.edit?.tool !== 'slide') return new Set();
		return new Set(linkPartners(editor.timeline, g.edit.clipId));
	});
	// The Preview pane shows the frames either side of the edit as it would stand.
	$effect(() => {
		const g = tg;
		const p = tgPreview;
		if (!g) return;
		const track = editor.timeline.tracks.find((t) => t.id === g.trackId);
		ui.trimMonitor = p && track ? monitorFor(p, fps, track.kind, timeDisplay.mode) : null;
	});
	let cancelTrimTool: (() => void) | null = null;
	// The panel closing gives a gesture up (this cleanup runs once, at teardown)…
	$effect(() => () => cancelTrimTool?.());
	// …and so does a clip leaving the timeline under it — an agent's edit, an undo, Delete
	// pressed mid-drag. Abandoned, nothing is written and the ghost goes; left to run, the
	// release would ask the backend for an edit on a clip that is gone and answer with an
	// error toast for a drag the user never finished.
	$effect(() => {
		const edit = tg?.edit;
		if (edit && !subjectsPresent(editor.timeline, edit)) cancelTrimTool?.();
	});

	/** The cut under the roll tool's pointer, so it can be lit and the cursor changed
	 *  before anything is pressed. */
	let rollHover = $state<{ trackId: string; time: number } | null>(null);
	$effect(() => {
		if (ui.tool !== 'roll') rollHover = null;
	});

	/** The razor's hover: the cut it would make in the clip under the pointer, snapped as a click's is. */
	function onRazorHover(e: PointerEvent, t: Track) {
		if (t.locked || drag || trimDrag || tg || marquee) {
			if (razor) razor = null;
			return;
		}
		const left = (e.currentTarget as HTMLElement).getBoundingClientRect().left;
		const time = laneTime(e.clientX, left);
		const c = t.clips.find((x) => time >= x.timeline_start && time < x.timeline_start + clipDuration(x));
		if (!c) {
			if (razor) razor = null;
			return;
		}
		const at = splitPoint(snapPoint(time, t.id, ''), c.timeline_start, c.timeline_start + clipDuration(c), fps);
		const box = lanesEl?.getBoundingClientRect();
		razor = { trackId: t.id, clipId: c.id, at, nx: box ? e.clientX - box.left : 0, ny: box ? e.clientY - box.top : 0 };
	}

	function onLaneHover(e: PointerEvent, t: Track) {
		if (ui.tool === 'razor') {
			onRazorHover(e, t);
			return;
		}
		if (ui.tool !== 'roll' || tg || t.locked) {
			if (rollHover) rollHover = null;
			return;
		}
		const left = (e.currentTarget as HTMLElement).getBoundingClientRect().left;
		const cut = nearestCut(cutsOf(t), laneTime(e.clientX, left), CUT_REACH_PX / pxPerSec);
		// Only a change is written: most moves of the pointer land on the same cut.
		if (cut?.time !== rollHover?.time || (cut ? t.id : null) !== (rollHover?.trackId ?? null)) {
			rollHover = cut ? { trackId: t.id, time: cut.time } : null;
		}
	}

	function endTrimTool() {
		tg = null;
		cancelTrimTool = null;
		ui.trimMonitor = null;
	}

	function beginTrimTool(e: PointerEvent, c: Clip, t: Track, laneLeft: number) {
		const tool = ui.tool as TrimTool;
		alt = e.altKey;
		const pressTime = laneTime(e.clientX, laneLeft);
		let edit: GestureEdit | null = null;
		let origin = 0;
		if (tool === 'roll') {
			// The nearest cut to the press, within reach — either clip of it will do.
			const cut = nearestCut(cutsOf(t), pressTime, CUT_REACH_PX / pxPerSec);
			if (cut) {
				edit = { tool, a: cut.a, b: cut.b };
				origin = cut.time;
			}
		} else if (tool === 'slip') {
			edit = { tool, clipId: c.id };
		} else {
			edit = { tool: 'slide', clipId: c.id };
			origin = c.timeline_start;
		}
		tg = {
			tool,
			trackId: t.id,
			edit,
			origin,
			grab: pressTime - origin,
			pressTime,
			downX: e.clientX,
			downScroll: scroller?.scrollLeft ?? 0,
			dur: clipDuration(c),
			speed: c.speed ?? 1,
			requested: 0,
			moved: false,
			nx: 0,
			ny: 0
		};
		cancelTrimTool = beginDrag(e, { move: onTrimToolMove, commit: onTrimToolUp, abandon: endTrimTool });
	}

	function onTrimToolMove(e: PointerLike) {
		const g = tg;
		if (!g) return;
		const moved = g.moved || Math.abs(e.clientX - g.downX) >= DRAG_SLOP;
		const track = editor.timeline.tracks.find((t) => t.id === g.trackId);
		if (!moved || !g.edit || !track) {
			tg = { ...g, moved };
			return;
		}
		const lane = documentOf(scroller).querySelector(`[data-lane][data-track-id="${g.trackId}"]`) as HTMLElement | null;
		const here = laneTime(e.clientX, lane?.getBoundingClientRect().left ?? 0);
		// One rounding, from where the pointer is — never from the last frame's. A magnet
		// within reach (the playhead, a beat, an edge not part of the edit) still wins.
		let requested: number;
		if (g.edit.tool === 'roll') {
			requested = snapPoint(here - g.grab, g.trackId, new Set([g.edit.a, g.edit.b])) - g.origin;
		} else if (g.edit.tool === 'slide') {
			requested = snapStart(here - g.grab, g.trackId, slideMembers(track, g.edit.clipId), g.dur) - g.origin;
		} else {
			requested = slipDelta((e.clientX - g.downX + ((scroller?.scrollLeft ?? 0) - g.downScroll)) / pxPerSec, { speed: g.speed }, fps);
		}
		const box = lanesEl?.getBoundingClientRect();
		tg = { ...g, moved, requested, nx: box ? e.clientX - box.left : 0, ny: box ? e.clientY - box.top : 0 };
	}

	function onTrimToolUp(e: PointerEvent) {
		alt = e.altKey;
		const g = tg;
		endTrimTool();
		if (!g) return;
		// A press that never became a drag is a click: the clip was selected at the press, and the
		// playhead stays where it is, as it does under Select.
		if (!g.moved || !g.edit) return;
		// Asked again: an agent's edit or a flipped mode may have changed what is legal
		// since the last move, and what is written is what the backend will take now.
		const p = previewEdit(editor.timeline, g.edit, g.requested, footage, !alt);
		if (!p.ok) {
			toast.error(refusalNotice(p));
			return;
		}
		if (p.ghosts.length === 0) {
			// Dragged out and back, or pinned against a limit it could not leave.
			if (p.clamped) toast.info(limitNotice(p, fps));
			return;
		}
		const edit = p.edit;
		// The partners go along with a linked clip — the preview showed them — unless Alt left them out.
		const link = alt ? false : undefined;
		const run =
			edit.tool === 'roll'
				? editor.roll(edit.a, edit.b, p.applied, link)
				: edit.tool === 'slip'
					? editor.slip(edit.clipId, p.applied, link)
					: editor.slide(edit.clipId, p.applied, link);
		// The Tauri commands answer with the refreshed timeline, not the backend's
		// `EditOutcome`, so a clamp is told from the range the ghost was held to.
		void run.then(() => p.clamped && toast.info(limitNotice(p, fps))).catch(err);
	}

	/** What the pointer looks like over a clip — the tool's own, and the plain
	 *  not-allowed over a locked track. A roll's cursor is only over a cut. */
	function clipCursor(t: Track): string {
		if (t.locked) return 'not-allowed';
		switch (ui.tool) {
			case 'razor':
				return 'crosshair';
			case 'roll':
				return tg || rollHover?.trackId === t.id ? 'col-resize' : 'default';
			case 'slip':
				return 'ew-resize';
			case 'slide':
				return tg ? 'grabbing' : 'grab';
			default:
				return drag ? (dropRefused ? 'not-allowed' : 'grabbing') : 'grab';
		}
	}

	// ---- marquee: drag on empty space to select what the rectangle touches ----

	type Marquee = {
		from: 'lane' | 'canvas';
		/** Where the drag began and where the pointer is now, in lane space. */
		ax: number;
		ay: number;
		x: number;
		y: number;
		/** The pointer in client px — so a scroll can re-evaluate without a move. */
		cx: number;
		cy: number;
		downX: number;
		downY: number;
		mode: ReturnType<typeof marqueeMode>;
		/** Alt was down at the press: the rectangle selects the clips it touches and no partners. */
		alone: boolean;
		/** The selection as the press found it; every update is computed from this. */
		base: Selection;
		moved: boolean;
	};
	let marquee = $state<Marquee | null>(null);
	let lanesEl = $state<HTMLElement | null>(null);
	/** The click that ends a marquee is not a click on the lane it ends on: it
	 *  would seek and clear the selection the drag just made (or just gave back).
	 *  Held until that click arrives or the next press, which always comes first —
	 *  not on a timer: an Escape mid-drag abandons the marquee long before the
	 *  button comes up. */
	let swallowClick = false;

	/** Where each track's lane sits in lane space — measured, because the heights
	 *  are CSS. */
	function laneBoxes(): LaneBox[] {
		const out: LaneBox[] = [];
		for (const el of lanesEl?.querySelectorAll<HTMLElement>('[data-lane]') ?? []) {
			const trackId = el.dataset.trackId!;
			out.push({ trackId, top: el.offsetTop, height: el.offsetHeight, locked: isLocked(trackId) });
		}
		return out;
	}

	function trackSpans(): Map<string, SpanClip[]> {
		return new Map(
			editor.timeline.tracks.map((t) => [
				t.id,
				t.clips.map((c) => ({ id: c.id, start: c.timeline_start, end: c.timeline_start + clipDuration(c) }))
			])
		);
	}

	function onCanvasPointerDown(e: PointerEvent, from: 'lane' | 'canvas', blocks: readonly Clip[] = []) {
		swallowClick = false;
		// Only a press on the empty surface itself: a clip, a title, the ruler, a
		// marker all have their own gestures and are children of what this is on. A lane's block
		// canvas is empty surface where it draws no block.
		const empty = pressOnEmpty(e.target, e.currentTarget, () => !!blockAt(blocks, clipDuration, pxPerSec, viewLo + e.offsetX));
		if (e.button !== 0 || ui.tool !== 'pointer' || !empty) return;
		const box = lanesEl?.getBoundingClientRect();
		if (!box) return;
		const x = e.clientX - box.left;
		const y = e.clientY - box.top;
		marquee = {
			from,
			ax: x,
			ay: y,
			x,
			y,
			cx: e.clientX,
			cy: e.clientY,
			downX: e.clientX,
			downY: e.clientY,
			mode: marqueeMode(e),
			alone: e.altKey,
			base: { ids: [...editor.selectedClipIds], primary: editor.selectedClipId },
			moved: false
		};
		capturePointer(e);
	}

	function onMarqueeMove(clientX: number, clientY: number) {
		const m = marquee;
		const box = lanesEl?.getBoundingClientRect();
		if (!m || !box) return;
		const x = Math.min(Math.max(clientX - box.left, 0), box.width);
		const y = Math.min(Math.max(clientY - box.top, 0), box.height);
		const moved = m.moved || Math.hypot(clientX - m.downX, clientY - m.downY) >= DRAG_SLOP;
		marquee = { ...m, x, y, cx: clientX, cy: clientY, moved };
		if (!moved) return;
		// Sweeping a picture sweeps its sound with it (a linked clip is picked as a pair), unless Alt.
		const hits = marqueeHits({ x0: m.ax, y0: m.ay, x1: x, y1: y }, laneBoxes(), trackSpans(), pxPerSec);
		const sel = marqueeSelectLinked(editor.timeline, m.base, hits, m.mode, m.alone);
		// Only a change is written: most moves of the pointer touch the same clips.
		if (!sameIds(sel.ids, editor.selectedClipIds) || sel.primary !== editor.selectedClipId) {
			editor.selectClips(sel.ids, sel.primary);
		}
	}

	function onMarqueeUp() {
		const m = marquee;
		marquee = null;
		if (!m) return;
		if (m.moved) {
			swallowClick = true;
			const primary = editor.selectedClip;
			if (primary) void editor.select(primary.asset_id);
		} else if (m.from === 'canvas' && m.mode === 'replace') {
			// A click on the bare space under the tracks: nothing there, so nothing selected.
			editor.clearSelection();
		}
	}

	/** A click on bare lane space. Empty lane space is the only way to deselect (clips stop the click),
	 *  and the click that deselects leaves the playhead where it is parked; with nothing selected it
	 *  puts the playhead there. */
	function onLaneSeek(e: MouseEvent) {
		if (swallowClick) {
			swallowClick = false;
			return;
		}
		const plain = !e.shiftKey && !e.ctrlKey && !e.metaKey;
		if (plain && (editor.selectedClipIds.length > 0 || editor.selectedOverlayId || editor.selectedSvgOverlayId)) {
			editor.clearSelection();
			return;
		}
		const x = e.clientX - (e.currentTarget as HTMLElement).getBoundingClientRect().left;
		ui.seek(x / pxPerSec);
	}

	// ---- drop a bin asset onto a track (HTML5 drag-and-drop) ------------------

	let dropGhost = $state<{ trackId: string; start: number; dur: number; ok: boolean } | null>(null);

	// The bin clears `ui.dndAsset` on dragend; mirror that to drop the ghost.
	$effect(() => {
		if (!ui.dndAsset) dropGhost = null;
	});

	function dropStart(e: DragEvent, t: Track, dur: number): number {
		const laneLeft = (e.currentTarget as HTMLElement).getBoundingClientRect().left;
		return snapStart(laneTime(e.clientX, laneLeft), t.id, '', dur);
	}

	/** Whether [start, start+dur) would overlap an existing clip on the track —
	 *  the same invariant `move_clip` enforces, so adds stay consistent. */
	function wouldOverlap(trackId: string, start: number, dur: number): boolean {
		const track = editor.timeline.tracks.find((t) => t.id === trackId);
		if (!track) return false;
		const end = start + dur;
		return track.clips.some((c) => start < c.timeline_start + clipDuration(c) && c.timeline_start < end);
	}

	function onLaneDragOver(e: DragEvent, t: Track) {
		const a = ui.dndAsset;
		if (!a || a.kind !== t.kind || t.locked || editor.previewingStaged) {
			dropGhost = null; // wrong-kind track: not a drop target
			return;
		}
		// Always allow the drop so the drop event fires reliably across webview
		// engines; onLaneDrop rejects overlaps. The ghost turns red to warn.
		e.preventDefault();
		if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
		const start = dropStart(e, t, a.duration);
		dropGhost = { trackId: t.id, start, dur: a.duration, ok: !wouldOverlap(t.id, start, a.duration) };
	}

	function onLaneDragLeave(e: DragEvent, t: Track) {
		// Only clear when truly leaving the lane (not entering one of its clips).
		const to = e.relatedTarget as Node | null;
		if (!to || !(e.currentTarget as HTMLElement).contains(to)) {
			if (dropGhost?.trackId === t.id) dropGhost = null;
		}
	}

	function onLaneDrop(e: DragEvent, t: Track) {
		const a = ui.dndAsset;
		dropGhost = null;
		ui.dndAsset = null;
		if (!a || a.kind !== t.kind) return;
		if (editor.previewingStaged) {
			e.preventDefault();
			holdForPreview();
			return;
		}
		if (t.locked) {
			toast.error(`Track ${t.name} is locked`);
			return;
		}
		e.preventDefault();
		const start = dropStart(e, t, a.duration);
		if (wouldOverlap(t.id, start, a.duration)) {
			toast.error('Drop into free space — a clip would overlap another here');
			return;
		}
		void editor.add(a.id, 0, a.duration, t.id, start).catch(err);
	}

	// ---- tracks (add / remove) -----------------------------------------------

	const onAddTrack = (kind: StreamKind) => void editor.addTrack(kind).catch(err);
	const onRemoveTrack = (t: Track) =>
		void editor
			.removeTrack(t.id)
			.then(() =>
				toast(`Removed track ${t.name}`, {
					action: { label: 'Undo', onClick: () => void editor.undo() }
				})
			)
			.catch(err);

	// ---- sync by audio ------------------------------------------------------------

	// A sync that has to queue behind another job says so, with a way to stop it.
	const SYNC_TOAST = 'audio-sync-wait';
	async function syncing<T>(run: () => Promise<T>): Promise<T> {
		const unlisten = await onAudioSyncProgress((p) =>
			toast(`Sync by audio: ${p.detail ?? 'waiting'}`, {
				id: SYNC_TOAST,
				duration: Infinity,
				action: { label: 'Cancel', onClick: () => void cancelAudioSync() }
			})
		);
		try {
			return await run();
		} finally {
			unlisten();
			toast.dismiss(SYNC_TOAST);
		}
	}
	const syncErr = (e: unknown) => {
		if (String(e instanceof Error ? e.message : e) !== SYNC_CANCELLED) err(e);
	};

	// "Move anyway" sends the same pair forced: the backend applies the measurement it just made.
	const syncByAudio = (referenceId: string, clipId: string) =>
		void syncing(() => editor.syncByAudio(referenceId, clipId))
			.then((s) =>
				s.applied
					? toast(describeSync(s))
					: toast(describeSync(s), {
							action: {
								label: 'Move anyway',
								onClick: () =>
									void syncing(() => editor.syncByAudio(referenceId, clipId, true))
										.then((f) => toast(describeSync(f)))
										.catch(syncErr)
							}
						})
			)
			.catch(syncErr);

	// ---- fades (toggle a default 0.5s fade on the selected clip) --------------

	const FADE_DEFAULT = 0.5;
	function toggleFadeIn() {
		const c = editor.selectedClip;
		if (c) void editor.setFade(c.id, c.fade_in > 0 ? 0 : FADE_DEFAULT).catch(err);
	}
	function toggleFadeOut() {
		const c = editor.selectedClip;
		if (c) void editor.setFade(c.id, undefined, c.fade_out > 0 ? 0 : FADE_DEFAULT).catch(err);
	}

	// ---- context menus -------------------------------------------------------

	/** Delete the whole selection (as one edit) when the clicked clip is part of it. */
	function removeClip(id: string, ripple: boolean) {
		if (editor.isSelected(id) && editor.selectedClipIds.length > 1) {
			void deleteSelection(ripple);
			return;
		}
		void (ripple ? editor.rippleDelete(id) : editor.remove(id))
			.then(() =>
				toast('Clip removed', {
					action: { label: 'Undo', onClick: () => void editor.undo() }
				})
			)
			.catch(err);
	}

	/** Remove one clip and leave the clips linked to it where they are (`link: false`). */
	function removeAlone(id: string, ripple: boolean) {
		void (ripple ? editor.rippleDelete(id, false) : editor.remove(id, false))
			.then(() =>
				toast('Clip removed — its linked clips stay', {
					action: { label: 'Undo', onClick: () => void editor.undo() }
				})
			)
			.catch(err);
	}

	function trackItems(t: Track): MenuItem[] {
		return [
			...held([
				{ label: 'Add video track', icon: 'video', action: () => onAddTrack('video') },
				{ label: 'Add audio track', icon: 'audio-waveform', action: () => onAddTrack('audio') },
				{ type: 'separator' },
				{
					...muteItem(t),
					action: () => void editor.setTrackMuted(t.id, !t.muted).catch(err)
				},
				{
					label: t.solo ? 'Clear solo' : 'Solo track',
					icon: 'headphones',
					action: () => void editor.setTrackSolo(t.id, !t.solo).catch(err)
				},
				{
					label: t.locked ? 'Unlock track' : 'Lock track',
					icon: 'lock',
					action: () => void editor.setTrackLocked(t.id, !t.locked).catch(err)
				}
			]),
			{ type: 'separator' },
			...heightItems(t),
			{ type: 'separator' },
			...held([{ label: `Remove track ${t.name}`, icon: 'trash', danger: true, action: () => onRemoveTrack(t) }])
		];
	}

	/** What has been analysed for the clip's media, and a way to run more (opens the Analyze menu
	 *  where the pointer was — the menu is long enough already). */
	function clipAnalysisItems(c: Clip, e: MouseEvent): MenuItem[] {
		const asset = editor.assets.find((a) => a.id === c.asset_id);
		if (!asset) return [];
		return [
			{
				type: 'info',
				label: 'Analyzed',
				value: doneCount(mediaStatus.analysis(asset.id)),
				title: doneSummary(mediaStatus.analysis(asset.id))
			},
			analyzeSubmenuItem(asset, { x: e.clientX, y: e.clientY })
		];
	}

	function onClipContextMenu(e: MouseEvent, c: Clip, t: Track) {
		// Right-clicking inside a multi-selection keeps it, so the menu can act on all.
		if (!editor.isSelected(c.id)) editor.selectClip(c.id);
		void editor.select(c.asset_id);
		const within = ui.time > c.timeline_start && ui.time < c.timeline_start + clipDuration(c);
		const enabled = c.enabled !== false;
		const n = editor.isSelected(c.id) ? editor.selectedClipIds.length : 1;
		// Detach / Reattach / Link / Unlink for what is selected, each with the reason it cannot be.
		const lp = selectionLinkPlans();
		const linked = linkPartners(editor.timeline, c.id).length > 0;
		// Music with a fitted bar grid can be made to last as long as the picture.
		const fit = fitOffer(t, c, editor.analysisFor(c.asset_id));
		const kf = keyframeOffer(editor.timeline, editor.keyframeSource, c.id);
		// Footage or music with sound can be split into drums / bass / other / vocals under the clip.
		const stems = stemsOffer(
			t,
			c,
			editor.assets.find((a) => a.id === c.asset_id)
		);
		// Composed music can be edited and rendered again into this very clip.
		const compose = composeOffer(
			t,
			editor.assets.find((a) => a.id === c.asset_id),
			c
		);
		// Two clips selected: the clicked one can be moved into sync with the other by their sound.
		const sync = syncOffer(editor.timeline, editor.assets, editor.selectedClipIds, c.id);
		contextMenu.show(e, [
			...held([
				{
					label: n > 1 ? 'Split selected at playhead' : 'Split at playhead',
					icon: 'Scissors',
					shortcut: settings.shortcut('edit.split'),
					disabled: !within || !!t.locked,
					reason: !within ? 'Move the playhead into this clip first' : t.locked ? `Track ${t.name} is locked` : undefined,
					// The playhead can be anywhere between frames; the cut is on one (`planPlayheadSplit`).
					action: () => void splitSelection()
				},
				{
					label: n > 1 ? 'Trim starts to playhead' : 'Trim start to playhead',
					shortcut: settings.shortcut('edit.trimStart'),
					disabled: !within || !!t.locked,
					action: () => void trimSelection('left')
				},
				{
					label: n > 1 ? 'Trim ends to playhead' : 'Trim end to playhead',
					shortcut: settings.shortcut('edit.trimEnd'),
					disabled: !within || !!t.locked,
					action: () => void trimSelection('right')
				}
			]),
			{ type: 'separator' },
			{
				label: n > 1 ? `Copy ${n} clips` : 'Copy',
				icon: 'copy',
				shortcut: settings.shortcut('edit.copy'),
				action: () => {
					const k = editor.copySelection();
					if (k) toast(k === 1 ? 'Clip copied' : `${k} clips copied`);
				}
			},
			...held([
				{
					label: n > 1 ? `Duplicate ${n} clips` : 'Duplicate',
					shortcut: settings.shortcut('edit.duplicate'),
					action: () => void duplicateClips()
				},
				{ type: 'separator' },
				...(lp.detach.show
					? [
							{
								label: lp.detach.label,
								icon: 'volume-x',
								shortcut: settings.shortcut('edit.detachAudio'),
								disabled: lp.detach.reason !== null,
								reason: lp.detach.reason ?? undefined,
								action: () => void detachSelection()
							} satisfies MenuItem
						]
					: []),
				...(lp.reattach.show
					? [
							{
								label: lp.reattach.label,
								icon: 'audio-waveform',
								shortcut: settings.shortcut('edit.reattachAudio'),
								disabled: lp.reattach.reason !== null,
								reason: lp.reattach.reason ?? undefined,
								action: () => void reattachSelection()
							} satisfies MenuItem
						]
					: []),
				{
					label: lp.link.label,
					icon: 'link',
					shortcut: settings.shortcut('edit.link'),
					disabled: lp.link.reason !== null,
					reason: lp.link.reason ?? undefined,
					action: () => void linkSelection()
				},
				{
					label: lp.unlink.label,
					icon: 'unlink',
					shortcut: settings.shortcut('edit.unlink'),
					disabled: lp.unlink.reason !== null,
					reason: lp.unlink.reason ?? undefined,
					action: () => void unlinkSelection()
				},
				...(sync.show
					? [
							{
								label: 'Sync by audio',
								icon: 'audio-waveform',
								disabled: sync.reason !== null,
								reason: sync.reason ?? undefined,
								action: () => syncByAudio(sync.referenceId!, c.id)
							} satisfies MenuItem
						]
					: [])
			]),
			{ type: 'separator' },
			...clipAnalysisItems(c, e),
			{ type: 'separator' },
			...held([
				...(fit.show
					? [
							{
								label: 'Fit to video…',
								icon: 'music',
								disabled: fit.reason !== null,
								reason: fit.reason ?? undefined,
								action: () => ui.openFitMusic(c.id)
							} satisfies MenuItem
						]
					: []),
				...(stems.show
					? [
							{
								label: 'Separate stems under this clip…',
								icon: 'layers',
								disabled: stems.reason !== null,
								reason: stems.reason ?? undefined,
								action: () => ui.openStems(c.asset_id, c.id)
							} satisfies MenuItem
						]
					: []),
				...(compose.show
					? [
							{
								label: 'Edit & re-compose…',
								icon: 'music',
								disabled: compose.reason !== null,
								reason: compose.reason ?? undefined,
								action: () => {
									const score = editor.assets.find((a) => a.id === c.asset_id)?.music_score;
									if (score) ui.openCompose({ score, replaceClipId: c.id });
								}
							} satisfies MenuItem
						]
					: []),
				...(fit.show || stems.show || compose.show ? [{ type: 'separator' } satisfies MenuItem] : []),
				{
					label: 'Copy keyframes',
					icon: 'diamond',
					disabled: kf.copy.disabled,
					reason: kf.copy.reason,
					action: () => copyKeyframesOf(c.id)
				},
				{
					label: 'Paste keyframes',
					icon: 'clipboard',
					disabled: kf.paste.disabled,
					reason: kf.paste.reason,
					action: () => void pasteKeyframesOnto(c.id)
				},
				{ type: 'separator' },
				{
					label: enabled ? 'Disable clip' : 'Enable clip',
					icon: enabled ? 'eye-off' : 'eye',
					disabled: !!t.locked,
					action: () => void editor.setClipEnabled(c.id, !enabled).catch(err)
				},
				{ type: 'separator' },
				{
					label: c.fade_in > 0 ? 'Remove fade-in' : 'Add fade-in',
					action: () => void editor.setFade(c.id, c.fade_in > 0 ? 0 : FADE_DEFAULT).catch(err)
				},
				{
					label: c.fade_out > 0 ? 'Remove fade-out' : 'Add fade-out',
					action: () => void editor.setFade(c.id, undefined, c.fade_out > 0 ? 0 : FADE_DEFAULT).catch(err)
				},
				{ type: 'separator' },
				// "Only this clip" deletes the picture and keeps its sound, or the other way round: `link: false`.
				...removeRows({
					count: n,
					linked,
					locked: !!t.locked,
					trackName: t.name,
					removeKey: settings.shortcut('edit.delete'),
					rippleKey: settings.shortcut('edit.rippleDelete'),
					remove: (ripple) => removeClip(c.id, ripple),
					removeAlone: (ripple) => removeAlone(c.id, ripple)
				})
			])
		]);
	}

	const onLaneContextMenu = (e: MouseEvent, t: Track) => contextMenu.show(e, trackItems(t));
	const onTrackHeaderContextMenu = (e: MouseEvent, t: Track) => contextMenu.show(e, trackItems(t));

	// ---- viewport: scrolling, zoom anchoring, panel resize --------------------

	let scroller = $state<HTMLElement | null>(null);
	let headersEl = $state<HTMLElement | null>(null);
	let rulerEl = $state<HTMLElement | null>(null);

	/** The visible lane window. The header column is sticky, so it covers the
	 *  first `headerW` px of the scroller — which makes `scrollLeft` index the
	 *  lane-x sitting at the left edge of the *uncovered* area exactly. */
	function viewport() {
		const el = scroller;
		if (!el) return null;
		const headerW = headersEl?.offsetWidth ?? 0;
		return { el, headerW, viewW: Math.max(1, el.clientWidth - headerW) };
	}

	// Keep the playhead on screen while it moves. During playback the view follows it continuously,
	// pinned just inside the edge it is running toward; a jump to somewhere off screen scrolls there
	// smoothly (`followScroll`). Only runs when the playhead moves, so scrolling away by hand while
	// parked isn't fought, and not while a gesture is held: the pointer is where the playhead is, and
	// the autoscroll is what moves the view then.
	$effect(() => {
		const laneX = ui.time * pxPerSec;
		const playing = ui.playing;
		const v = viewport();
		if (!v || untrack(() => gestureLive)) return;
		const f = followScroll({ laneX, scrollLeft: v.el.scrollLeft, viewW: v.viewW, playing });
		if (!f) return;
		const calm = windowOf(v.el).matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;
		if (f.smooth && !calm) v.el.scrollTo({ left: f.left, behavior: 'smooth' });
		else v.el.scrollLeft = f.left;
	});

	// Hold one point in time still across a zoom change, so zooming in on a
	// distant clip doesn't sweep it off screen. A wheel zoom or a fit says where the
	// scroller should end up (`pendingScroll`, worked out by `zoom.ts`); any other
	// change (the slider, the buttons, the keys) holds the playhead. `$effect.pre`
	// samples the scroll position *before* the DOM is patched (i.e. at the old
	// zoom); the paired `$effect` applies the result after the lane has been
	// rewidened.
	let lastZoom = ui.zoom;
	let preScroll = 0;
	let pendingScroll: number | null = null;

	$effect.pre(() => {
		void ui.zoom;
		preScroll = scroller?.scrollLeft ?? 0;
	});

	$effect(() => {
		const z = ui.zoom;
		const v = viewport();
		if (!v) {
			lastZoom = z;
			return;
		}
		if (z === lastZoom) return;
		const prev = lastZoom;
		lastZoom = z;
		if (pendingScroll !== null) {
			v.el.scrollLeft = pendingScroll;
			pendingScroll = null;
			return;
		}
		// A playhead that was off screen would otherwise be preserved off screen;
		// clamp it into the window first.
		const offset = Math.min(Math.max(ui.time * prev - preScroll, 0), v.viewW);
		v.el.scrollLeft = scrollFor(ui.time, offset, z);
	});

	/** ⌘/Ctrl + wheel zooms around the cursor: the time under the pointer stays
	 *  under it. Plain and shift wheel are left to the browser's native vertical /
	 *  horizontal scrolling, which now matters. */
	function onWheel(e: WheelEvent) {
		if (!e.ctrlKey && !e.metaKey) return;
		e.preventDefault();
		const v = viewport();
		if (!v) return;
		// Over the sticky track headers the pointer is not over any time: anchor on
		// the nearest edge of the lanes instead.
		const offset = Math.min(Math.max(e.clientX - v.el.getBoundingClientRect().left - v.headerW, 0), v.viewW);
		const next = clampZoom(ui.zoom * wheelZoomFactor(e.deltaY, e.deltaMode), editor.duration);
		if (next === ui.zoom) return;
		pendingScroll = zoomAround({ zoom: ui.zoom, scrollLeft: v.el.scrollLeft }, offset, next).scrollLeft;
		ui.zoom = next;
	}

	/** Scroll and zoom the timeline to where the minimap asks. A zoom change goes
	 *  through `pendingScroll` — the lane has to be re-widened before a scroll can
	 *  land on it — exactly as a wheel zoom does; an unchanged zoom is just a scroll. */
	function applyView(target: Target) {
		const v = viewport();
		if (!v) return;
		const zoom = clampZoom(target.zoom, editor.duration);
		const scroll = Math.max(0, target.scrollLeft);
		if (zoom === ui.zoom) {
			// A scroll already waiting on a zoom change is superseded, not raced.
			if (pendingScroll !== null) pendingScroll = scroll;
			else v.el.scrollLeft = scroll;
			return;
		}
		pendingScroll = scroll;
		ui.zoom = zoom;
	}

	/** Fit the whole cut in the visible width (⇧Z, or the button), from the left. */
	function fitToWindow() {
		const v = viewport();
		const next = v ? fitZoom(editor.duration, v.viewW) : null;
		if (!v || next === null) return;
		// Asked for by a person: from here the zoom is theirs, so a later resize does not swap
		// this uncapped fit for the capped one the follow takes.
		ui.zoomFollowsCut = false;
		if (next === ui.zoom) {
			v.el.scrollLeft = 0;
			return;
		}
		pendingScroll = 0;
		ui.zoom = next;
	}

	// Until anyone sets the zoom it follows the cut (`followZoom`, `zoom.ts`): fitted when a project
	// opens or its first clip lands, when the window changes width (once it has stopped changing: a
	// panel being dragged would otherwise rescale the lane every frame) and when the cut outgrows the
	// window. The scroller has to exist first, or the width is the placeholder's.
	let followPrev: FollowInput | null = null;
	$effect(() => {
		if (!scroller || !ui.zoomFollowsCut) {
			followPrev = null;
			return;
		}
		const now: FollowInput = { viewW, duration: editor.duration, generation: editor.loadEpoch };
		const resizeOnly =
			!!followPrev &&
			followPrev.generation === now.generation &&
			followPrev.duration === now.duration &&
			followPrev.viewW !== now.viewW;
		const apply = () =>
			untrack(() => {
				const z = followZoom(followPrev, now, ui.zoom);
				followPrev = now;
				if (z === null) return;
				pendingScroll = 0;
				ui.fitZoomTo(z);
			});
		if (!resizeOnly) {
			apply();
			return;
		}
		const timer = setTimeout(apply, 150);
		return () => clearTimeout(timer);
	});

	// The shortcut lives in the page's key handler, which cannot know how wide the
	// timeline is, so it asks by bumping a counter. The first run is not an ask.
	let lastFit = ui.fitEpoch;
	$effect(() => {
		const epoch = ui.fitEpoch;
		if (epoch === lastFit) return;
		lastFit = epoch;
		untrack(fitToWindow);
	});

	// ---- titles lane: titles / lower-thirds / captions are their own items -----
	//
	// They live on the timeline (`Timeline.overlays`), not on any clip, so they
	// get a lane of their own rather than a section in the clip inspector.
	// Overlapping items (a title over a run of captions) stack into rows.

	// The titles lane is not a track, so it follows the global height choice.
	const titleMetrics = $derived(TITLE_METRICS[ui.heights.all]);
	// The lane holds text titles and animated SVG titles in the same packed rows.
	const titleRows = $derived(packRows([...editor.overlays, ...editor.svgOverlays]));
	const titleCount = $derived(editor.overlays.length + editor.svgOverlays.length);
	/** No title, caption or animated title on the cut: the lane is a strip (`track-heights.ts`). */
	const titlesEmpty = $derived(titleCount === 0);
	const titleLaneH = $derived(
		titleLaneHeightFor(ui.heights.all, Math.max(1, ...[...titleRows.values()].map((r) => r + 1)), titleCount)
	);

	type TitleDrag = {
		id: string;
		/** A text title or an animated SVG title: the same gesture, written by a different edit. */
		kind: 'text' | 'svg';
		mode: 'move' | 'l' | 'r';
		grabSec: number;
		origStart: number;
		origEnd: number;
		start: number;
		end: number;
		moved: boolean;
	};
	let titleDrag = $state<TitleDrag | null>(null);
	let titleLaneEl = $state<HTMLElement | null>(null);

	/** The edges of every title on the lane — text and animated — but `id`'s own. */
	function otherTitleEdges(id: string): number[] {
		return [...editor.overlays, ...editor.svgOverlays].filter((o) => o.id !== id).flatMap((o) => [o.start, o.end]);
	}

	/** Where a title's edges snap to: 0, the playhead, beats and bars, every clip edge and the other titles'. */
	function titleSnapPoints(id: string): number[] {
		const pts = [0, ui.time, ...beatTimes, ...barTimes];
		for (const t of editor.timeline.tracks)
			for (const c of t.clips) pts.push(c.timeline_start, c.timeline_start + clipDuration(c));
		pts.push(...otherTitleEdges(id));
		return pts;
	}

	function onTitlePointerDown(e: PointerEvent, o: TextOverlay | SvgOverlay, mode: 'move' | 'l' | 'r', kind: 'text' | 'svg' = 'text') {
		if (e.button !== 0) return;
		e.stopPropagation();
		if (kind === 'svg') editor.selectSvgOverlay(o.id);
		else editor.selectOverlay(o.id);
		if (holdForReview(e)) return;
		const left = titleLaneEl?.getBoundingClientRect().left ?? 0;
		titleDrag = {
			id: o.id,
			kind,
			mode,
			grabSec: laneTime(e.clientX, left) - o.start,
			origStart: o.start,
			origEnd: o.end,
			start: o.start,
			end: o.end,
			moved: false
		};
		capturePointer(e);
	}

	function onTitleDragMove(e: PointerLike) {
		const d = titleDrag;
		if (!d) return;
		const t = laneTime(e.clientX, titleLaneEl?.getBoundingClientRect().left ?? 0);
		const pts = titleSnapPoints(d.id);
		const threshold = 8 / pxPerSec;
		let start: number;
		let end: number;
		if (d.mode === 'move') {
			const dur = d.origEnd - d.origStart;
			start = ui.snap ? snapSpanStart(t - d.grabSec, dur, pts, threshold) : Math.max(0, t - d.grabSec);
			end = start + dur;
		} else {
			({ start, end } = trimSpan(d.origStart, d.origEnd, d.mode, ui.snap ? snapTime(t, pts, threshold) : t));
		}
		const eps = 2 / pxPerSec;
		titleDrag = {
			...d,
			start,
			end,
			moved: d.moved || Math.abs(start - d.origStart) >= eps || Math.abs(end - d.origEnd) >= eps
		};
	}

	function onTitleDragUp() {
		const d = titleDrag;
		titleDrag = null;
		if (!d) return;
		if (!d.moved) return; // a plain click selected the title at the press and leaves the playhead alone
		// An edit that removed it under the drag (an undo, an agent) leaves nothing to write.
		if (d.kind === 'svg' && !editor.svgOverlays.some((o) => o.id === d.id)) return;
		// One edit either way: an animated title's update re-times a preset's artwork in the same revision.
		if (d.kind === 'svg') void editor.updateSvgOverlay(d.id, { start: d.start, end: d.end }).catch(err);
		else void editor.retimeOverlay(d.id, d.start, d.end).catch(err);
	}

	const addTitleHere = () => void editor.addTitle('Title', ui.time, ui.time + settings.editing.title_seconds).catch(err);

	function onTitleContextMenu(e: MouseEvent, o: TextOverlay) {
		e.stopPropagation();
		editor.selectOverlay(o.id);
		contextMenu.show(e, [
			{ label: 'Go to start', icon: 'skip-back', action: () => ui.seek(o.start, { snap: 'up', end: o.end }) },
			{ type: 'separator' },
			...held([
				{
					label: o.generated ? 'Remove caption' : 'Remove title',
					icon: 'trash',
					danger: true,
					shortcut: settings.shortcut('edit.delete'),
					action: () => void editor.removeOverlay(o.id).catch(err)
				}
			])
		]);
	}

	function onSvgContextMenu(e: MouseEvent, o: SvgOverlay) {
		e.stopPropagation();
		editor.selectSvgOverlay(o.id);
		contextMenu.show(e, [
			{ label: 'Go to start', icon: 'skip-back', action: () => ui.seek(o.start, { snap: 'up', end: o.end }) },
			...held(
				o.preset
					? []
					: [
							{
								label: 'Edit SVG source',
								icon: 'code-xml',
								action: () => (ui.svgSourceFocus = o.id)
							}
						]
			),
			{ type: 'separator' },
			...held([
				{
					label: 'Remove animated title',
					icon: 'trash',
					danger: true,
					shortcut: settings.shortcut('edit.delete'),
					action: () => void editor.removeSvgOverlay(o.id).catch(err)
				}
			])
		]);
	}

	function onTitleLaneContextMenu(e: MouseEvent) {
		e.stopPropagation();
		// Importing captions: timed to the cut, or to one of the clips it shows. The
		// controls in Titles carry the same import with every clip to choose from.
		const { entries, more } = importMenuEntries(importableAssets(editor.timeline, editor.assets));
		contextMenu.show(e, [
			...held([
				{ label: 'Add title at playhead', icon: 'captions', action: addTitleHere },
				...SVG_PRESETS.map(
					(p): MenuItem => ({
						label: `Add animated ${p.label.toLowerCase()}`,
						icon: 'orbit',
						disabled: editor.busy,
						action: () => void addAnimatedTitle(p.kind)
					})
				),
				{ label: 'Add custom SVG title…', icon: 'code-xml', disabled: editor.busy, action: () => void addCustomSvgTitle() },
				{ type: 'separator' },
				...entries.map(
					(m): MenuItem => ({
						label: m.label,
						icon: 'file-text',
						disabled: editor.busy,
						action: () => void importCaptionFile(m.choice)
					})
				)
			]),
			...(more > 0
				? [{ type: 'info', label: `${more} more clip${more === 1 ? '' : 's'}`, value: 'in Titles' } satisfies MenuItem]
				: [])
		]);
	}

	// ---- ruler scrub + draggable in/out marks ---------------------------------

	/** ", ⇧I clears" in a mark's tooltip — whatever key does, or nothing when none does. */
	const clearHint = (id: 'range.clearIn' | 'range.clearOut') => {
		const k = settings.shortcut(id);
		return k ? `, ${k} clears` : '';
	};

	let scrubbing = $state(false);
	let markDrag = $state<'in' | 'out' | null>(null);
	/** Where the playhead and the marks were when a scrub or a mark drag began: what giving it up restores. */
	let scrubFrom: number | null = null;
	let markFrom: { in: number | null; out: number | null } = { in: null, out: null };

	const rulerTime = (clientX: number) =>
		Math.max(0, (clientX - (rulerEl?.getBoundingClientRect().left ?? 0)) / pxPerSec);

	function onRulerPointerDown(e: PointerEvent) {
		if (e.button !== 0) return;
		scrubbing = true;
		scrubFrom = ui.time;
		capturePointer(e);
		ui.seek(rulerTime(e.clientX));
	}

	function onMarkPointerDown(e: PointerEvent, which: 'in' | 'out') {
		if (e.button !== 0) return;
		e.stopPropagation();
		markDrag = which;
		markFrom = { in: ui.markIn, out: ui.markOut };
		capturePointer(e);
	}

	// ---- markers --------------------------------------------------------------

	/** Marker being dragged along the ruler, and the id being renamed inline. */
	let markerDrag: { id: string; time: number } | null = $state(null);
	let renaming = $state<string | null>(null);
	/** The marker whose name, kind, length and comment are open in the dialog. */
	let editingMarker = $state<string | null>(null);

	const addMarkerHere = () => void addMarker();

	function onMarkerPointerDown(e: PointerEvent, m: Marker) {
		if (e.button !== 0 || renaming === m.id) return;
		e.stopPropagation();
		if (holdForReview(e)) return;
		markerDrag = { id: m.id, time: m.time };
		capturePointer(e);
	}

	function commitRename(m: Marker, value: string) {
		// Escape clears `renaming` first, and unmounting the focused input then
		// fires its blur — which must not apply the text that was just abandoned.
		if (renaming !== m.id) return;
		renaming = null;
		const name = value.trim();
		if (name && name !== m.name) void editor.updateMarker(m.id, { name }).catch(err);
	}

	function onMarkerContextMenu(e: MouseEvent, m: Marker) {
		e.stopPropagation();
		contextMenu.show(e, [
			{ label: 'Go to marker', icon: 'bookmark', action: () => ui.seek(m.time) },
			...held([
				{ label: 'Rename', action: () => (renaming = m.id) },
				{ label: 'Edit marker…', icon: 'file-text', action: () => (editingMarker = m.id) },
				{
					label: m.kind === 'chapter' ? 'Make it a comment' : 'Make it a chapter',
					action: () => void editor.updateMarker(m.id, { kind: m.kind === 'chapter' ? 'comment' : 'chapter' }).catch(err)
				},
				{ type: 'separator' },
				{
					label: 'Remove marker',
					icon: 'trash',
					danger: true,
					action: () => void editor.removeMarker(m.id).catch(err)
				}
			])
		]);
	}

	// Right-click on empty timeline canvas (ruler / grid / below the tracks). Clip
	// and lane menus stopPropagation, so only the bare background reaches this.
	function onTimelineContextMenu(e: MouseEvent) {
		contextMenu.show(e, [
			...held([
				{ label: 'Add video track', icon: 'video', action: () => onAddTrack('video') },
				{ label: 'Add audio track', icon: 'audio-waveform', action: () => onAddTrack('audio') },
				{ type: 'separator' },
				{
					label: editor.clipboard.length > 1 ? `Paste ${editor.clipboard.length} clips` : 'Paste',
					icon: 'copy',
					shortcut: settings.shortcut('edit.paste'),
					disabled: editor.clipboard.length === 0,
					action: () => void pasteClips()
				},
				{ type: 'separator' },
				{
					label: 'Add marker at playhead',
					icon: 'bookmark',
					shortcut: settings.shortcut('marker.add'),
					action: addMarkerHere
				}
			]),
			{ type: 'separator' },
			{
				label: ui.snap ? 'Disable snapping' : 'Enable snapping',
				icon: 'magnet',
				action: () => (ui.snap = !ui.snap)
			},
			{
				label: editor.rippleMode ? 'Turn ripple mode off' : 'Turn ripple mode on',
				icon: 'between-horizontal-start',
				shortcut: settings.shortcut('tool.rippleMode'),
				action: () => void editor.setRippleMode(!editor.rippleMode).catch(err)
			},
			{
				label: 'Zoom to fit',
				icon: 'fold-horizontal',
				shortcut: settings.shortcut('view.zoomFit'),
				disabled: !hasClips,
				action: () => ui.zoomToFit()
			}
		]);
	}
</script>



<!-- The number beside the pointer while a gesture is held: amber at a limit, red when the edit would be refused. -->
{#snippet readoutChip(r: Readout, x: number, y: number)}
	{@const line = r.tone === 'refused' ? 'var(--red-500)' : r.tone === 'limit' ? 'var(--warning)' : 'var(--border-strong)'}
	<div
		role="status"
		aria-live="off"
		data-trim-readout
		style="position:absolute;left:{Math.max(scrollX + 4, Math.min(x + 14, scrollX + viewW - 440))}px;top:{y + 18}px;z-index:35;pointer-events:none;max-width:430px;padding:3px 8px;border-radius:var(--radius-sm);border:var(--line-width) solid {line};background:var(--surface-raised);color:var(--text-primary);font-size:11px;line-height:1.35"
	>
		<div style="font-family:var(--font-mono);font-weight:600;white-space:nowrap">{r.title}</div>
		{#if r.detail}<div style="color:{r.tone === 'refused' ? 'var(--red-500)' : 'var(--warning)'}">{r.detail}</div>{/if}
	</div>
{/snippet}

{#snippet toolBtn(title: TipInput, label: string, icon: string, on: boolean | null, click: () => void, off = false)}
	<ToolbarToggle {title} {label} {icon} bare pressed={on} disabled={off} onclick={click} />
{/snippet}

<div
	{@attach onWindow({
		// The window the timeline is in, which is a detached window's when it is in one: a
		// pointer move or release that ends a drag happens in *that* window and is never
		// heard by a listener on the editor's.
		pointerdowncapture: (e: PointerEvent) => {
			press = { x: e.clientX, y: e.clientY };
			pointer = press;
			heldButtons = e.buttons;
		},
		pointermove: onPointerMove,
		pointerup: onPointerUp,
		pointercancel: onPointerCancel,
		blur: onWindowBlur,
		resize: syncView,
		keyup: (e: KeyboardEvent) => {
			if (e.key === 'Alt') alt = false;
		},
		keydowncapture: (e: KeyboardEvent) => {
			if (e.key === 'Alt') alt = true;
			// Escape gives a clip, edge, title, marquee, scrub or mark drag up without writing
			// anything (a scrub puts the playhead back, a mark drag the marks) — and that is all it
			// does: the page's Escape (clear the selection) must not also fire.
			if (e.key === 'Escape' && (drag || trimDrag || titleDrag || marquee || nudge || scrubbing || markDrag || markerDrag)) {
				resetDragState();
				e.stopPropagation();
			}
			// A history key while a nudge is unwritten drops the run: undo takes the nudge back (and
			// not the edit before it), redo drops it and goes on.
			if (nudge && !nudge.writing) {
				const act = settings.actionFor(e);
				if (act === 'edit.undo') {
					abandonNudge();
					e.preventDefault();
					e.stopPropagation();
				} else if (act === 'edit.redo') {
					abandonNudge();
				}
			}
		}
	})}
	data-editor-surface
	style="flex:1;min-height:0;background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden;position:relative"
>

	<!-- A proposal waiting, while the cut on screen is still the user's: one line to say so and the three
	     answers, so the review is not only in the Agent panel (which may be a tab away, or closed). -->
	{#if editor.staged && !editor.previewingStaged}
		<div
			data-proposal-strip
			role="status"
			style="flex:none;display:flex;align-items:center;gap:8px;min-height:30px;padding:3px 12px;background:var(--agent-surface);border-bottom:var(--line-width) solid var(--agent-border);color:var(--agent-300);font-size:12px"
		>
			<Icon n="git-pull-request-arrow" s={13} color="var(--agent-300)" />
			<span style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap"
				>{proposalLine(editor.staged.diff.entries.length)}{editor.staged.note ? ` — ${editor.staged.note}` : ''}</span
			>
			<span style="color:var(--text-muted);white-space:nowrap">· outlined on the cut</span>
			<div style="flex:1"></div>
			<Btn size="sm" icon="eye" iconSize={12} disabled={reviewBusy} onclick={() => void editor.previewStaged()}>Preview</Btn>
			<Btn variant="primary" size="sm" disabled={reviewBusy} onclick={() => void reviewAct(applyProposal)}>Apply</Btn>
			<Btn size="sm" disabled={reviewBusy} onclick={() => void reviewAct(discardProposal)}>Discard</Btn>
		</div>
	{/if}

	<!-- Timeline toolbar. What used to be the app's own toolbar row lives here, where
	     it acts: the tools, ripple, snapping, undo / redo and the delivery frame beside
	     the view controls. It wraps onto a second row rather than clipping, so nothing
	     becomes unreachable in a narrow window. -->
	<div
		style="min-height:34px;display:flex;flex-wrap:wrap;align-items:center;column-gap:8px;row-gap:2px;padding:3px 12px;border-bottom:var(--line-width) solid var(--border-subtle);flex:none"
	>
		<div role="toolbar" aria-label="Edit tools" style="display:inline-flex;align-items:center;gap:2px">
			{#each TOOLS as [id, ic, name, action] (id)}
				{@render toolBtn(settings.tip(name, action), name, ic, ui.tool === id, () => (ui.tool = id))}
			{/each}
			<span style="width:1px;height:16px;background:var(--border-strong);margin:0 4px"></span>
			{#each TRIM_TOOLS as [id, ic, name, action] (id)}
				{@render toolBtn(
					settings.tip(`${name} tool`, action, TOOL_HINT[id]),
					`${name} tool`,
					ic,
					ui.tool === id,
					() => (ui.tool = id)
				)}
			{/each}
			<span style="width:1px;height:16px;background:var(--border-strong);margin:0 4px"></span>
			{@render toolBtn(
				editor.previewingStaged ? PREVIEW_READ_ONLY_REASON : settings.tip('Undo', 'edit.undo'),
				'Undo',
				'undo',
				null,
				() => void editor.undo(),
				!editor.canUndo || editor.previewingStaged
			)}
			{@render toolBtn(
				editor.previewingStaged ? PREVIEW_READ_ONLY_REASON : settings.tip('Redo', 'edit.redo'),
				'Redo',
				'redo',
				null,
				() => void editor.redo(),
				!editor.canRedo || editor.previewingStaged
			)}
		</div>
		{#if editor.busy}<Badge tone="agent" dot>working…</Badge>{/if}
		{#if editor.selectedClips.length > 1}
			<span
				use:tip={{
					text: 'Drag any of them to move them all',
					detail: [
						settings.shortcut('edit.delete') ? `${settings.shortcut('edit.delete')} removes them all` : '',
						hasLinkedClips ? ALT_HINT : ''
					]
						.filter(Boolean)
						.join('\n')
				}}
				style="font-size:10px;color:var(--kerf-300)">{editor.selectedClips.length} selected</span
			>
		{/if}
		{#if alt && hasLinkedClips}
			<!-- Alt is down: whatever is dragged, trimmed or cut now leaves linked clips where they are. -->
			<span
				data-alt-chip
				use:tip={ALT_HINT}
				style="display:inline-flex;align-items:center;gap:4px;font-size:10px;padding:1px 6px;border-radius:4px;border:var(--line-width) solid var(--warning);color:var(--orange-400);background:var(--warning-surface)"
				><Icon n="unlink" s={10} />links off</span
			>
		{/if}
		{#if editor.selectedClip}
			{@const sc = editor.selectedClip}
			<span style="display:inline-flex;align-items:center;gap:6px">
				<span
					style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
					>Fade</span
				>
				<ToolbarToggle
					title={editor.previewingStaged ? PREVIEW_READ_ONLY_REASON : `Toggle a ${FADE_DEFAULT}s fade-in on the selected clip`}
					label="Fade in"
					pressed={sc.fade_in > 0}
					disabled={editor.previewingStaged}
					onclick={toggleFadeIn}>in</ToolbarToggle
				>
				<ToolbarToggle
					title={editor.previewingStaged ? PREVIEW_READ_ONLY_REASON : `Toggle a ${FADE_DEFAULT}s fade-out on the selected clip`}
					label="Fade out"
					pressed={sc.fade_out > 0}
					disabled={editor.previewingStaged}
					onclick={toggleFadeOut}>out</ToolbarToggle
				>
			</span>
		{/if}
		<div style="flex:1;min-width:0"></div>
		<div style="display:inline-flex;align-items:center;flex-wrap:wrap;gap:8px;row-gap:2px">
			<span style="display:inline-flex;align-items:center;gap:2px">
				<ToolbarToggle
					title={settings.tip('Zoom out', 'view.zoomOut')}
					label="Zoom out"
					icon="zoom-out"
					bare
					onclick={() => ui.zoomBy(-1)}
				/>
				<input
					type="range"
					min="0"
					max="1"
					step="0.001"
					value={zoomToSlider(ui.zoom)}
					oninput={(e) => (ui.zoom = clampZoom(sliderToZoom(+e.currentTarget.value), editor.duration))}
					use:tip={{ text: `Zoom — ${zoomLabel(ui.zoom)}`, detail: `${settings.modLabel('primary')} + wheel zooms at the cursor` }}
					aria-label="Timeline zoom"
					style="width:90px;height:24px;margin:0 4px"
				/>
				<ToolbarToggle
					title={settings.tip('Zoom in', 'view.zoomIn')}
					label="Zoom in"
					icon="zoom-in"
					bare
					onclick={() => ui.zoomBy(1)}
				/>
				<ToolbarToggle
					title={settings.tip('Zoom to fit the whole cut', 'view.zoomFit')}
					label="Zoom to fit"
					icon="fold-horizontal"
					bare
					disabled={!hasClips}
					onclick={() => ui.zoomToFit()}
				/>
			</span>
			<span style="width:1px;height:16px;background:var(--border-strong);margin:0 4px"></span>
			<span style="display:inline-flex;align-items:center;gap:6px">
				<ToolbarToggle
					title={settings.tip(ui.snap ? 'Snapping on' : 'Snapping off', 'tool.snap', ui.snap ? 'Click to turn it off.' : 'Click to turn it on.')}
					label="Snapping"
					icon="magnet"
					pressed={ui.snap}
					onclick={() => (ui.snap = !ui.snap)}
				/>
				<!-- Ripple mode is the project's, not this panel's: it changes what a trim, a
				     delete and a speed change do everywhere (and an agent can flip it). So it is
				     lit while on, with a second cue in the ruler corner. -->
				<ToolbarToggle
					title={settings.tip(
						editor.rippleMode ? 'Ripple on' : 'Ripple off',
						'tool.rippleMode',
						editor.rippleMode
							? 'A trim, delete or speed change pulls the later clips on that track along, keeping their gaps. Each track ripples on its own; a clip it moves takes its linked partners along.'
							: 'Edits leave a gap. Turn it on to have a trim, delete or speed change pull the later clips on that track along. Each track ripples on its own; linked clips follow their partners.'
					)}
					label="Ripple"
					icon="between-horizontal-start"
					iconSize={12}
					pressed={editor.rippleMode}
					onclick={() => void editor.setRippleMode(!editor.rippleMode).catch(err)}>Ripple</ToolbarToggle
				>
			</span>
			<span style="width:1px;height:16px;background:var(--border-strong);margin:0 4px"></span>
			<span style="display:inline-flex;align-items:center;gap:6px">
				<!-- Track height, every track at once (each track's own name button sets just
				     that one). Lit when every track is at that height. -->
				<div role="group" aria-label="Track height, all tracks" style="display:inline-flex;align-items:center;gap:2px">
					{#each HEIGHT_PRESETS as p (p)}
						<ToolbarToggle
							title="All tracks {PRESET_LABEL[p].toLowerCase()} — {PRESET_PX[p]} px"
							label="All tracks {PRESET_LABEL[p].toLowerCase()}"
							pressed={allHeight === p}
							square
							onclick={() => ui.setAllHeights(p)}><HeightGlyph preset={p} /></ToolbarToggle
						>
					{/each}
				</div>
				<ToolbarToggle
					title={ui.minimap ? 'Hide the overview strip' : 'Show the overview strip of the whole cut'}
					label="Toggle overview"
					icon="chart-no-axes-gantt"
					iconSize={13}
					pressed={ui.minimap}
					onclick={() => ui.toggleMinimap()}
				/>
			</span>
			<span style="width:1px;height:16px;background:var(--border-strong);margin:0 4px"></span>
			<span style="display:inline-flex;align-items:center;gap:6px">
				<ToolbarToggle
					title={editor.previewingStaged ? PREVIEW_READ_ONLY_REASON : 'Add a video track'}
					disabled={editor.previewingStaged}
					onclick={() => onAddTrack('video')}>+ V</ToolbarToggle
				>
				<ToolbarToggle
					title={editor.previewingStaged ? PREVIEW_READ_ONLY_REASON : 'Add an audio track'}
					disabled={editor.previewingStaged}
					onclick={() => onAddTrack('audio')}>+ A</ToolbarToggle
				>
				<!-- The frame the cut is made for: the preview is this shape, and so is the export. -->
				<ToolbarToggle
					title={editor.previewingStaged
						? PREVIEW_READ_ONLY_REASON
						: delivery.format
						? `Delivering ${delivery.format.width}\u00d7${delivery.format.height} — ${fitLabel(delivery.format.fit)}. Click to change.`
						: 'The frame follows the footage. Click to cut for a delivery shape.'}
					icon="crop"
					iconSize={12}
					on={!!delivery.format}
					disabled={editor.previewingStaged}
					aria-haspopup="menu"
					onclick={pickDelivery}>{delivery.label}</ToolbarToggle
				>
			</span>
		</div>
	</div>

	<!-- The whole cut on one strip: where the view is, and a way to move it. A
	     pointer overview, hidden from assistive tech on purpose: everything it does
	     is already reachable from the keyboard (the scroller itself, the zoom
	     buttons and the zoom / fit / shuttle keys, and the playhead's follow) — a
	     second focusable widget with partial parity would be more to learn than
	     to use. Its toggle in the toolbar stays labelled. -->
	{#if ui.minimap && hasClips && !viewCoversCut({ scrollLeft: scrollX, viewW, pxPerSec }, editor.duration)}
		<div
			aria-hidden="true"
			style="display:flex;flex:none;border-bottom:var(--line-width) solid var(--border-subtle);background:var(--surface-app)"
		>
			<div
				style="width:var(--track-header-w);flex:none;box-sizing:border-box;border-right:var(--line-width) solid var(--border-default);display:flex;align-items:center;padding:0 8px;font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
			>
				Overview
			</div>
			<Minimap
				tracks={drawn}
				duration={editor.duration}
				scrollLeft={scrollX}
				{viewW}
				{pxPerSec}
				time={ui.time}
				markIn={ui.markIn}
				markOut={ui.markOut}
				onview={applyView}
				onseek={(t) => ui.seek(t)}
			/>
		</div>
	{/if}

	<!-- One scroller for both columns, so headers and lanes cannot desync. The
	     headers stick to the left, the ruler to the top, and their corner to
	     both. Vertical overflow scrolls, so a 5th track is reachable.
	     `align-items:flex-start` + `min-height:100%` rather than the default
	     stretch: stretch would size each column to the scroller's *client*
	     height, cutting the playhead and grid lines off at the fold while the
	     tracks kept going. The vertical scrollbar's gutter is always kept, and the
	     lanes clip what is drawn past their end: the lane is sized from the
	     scroller's width, so a scrollbar that came and went with it (a tick a
	     pixel past the edge, a horizontal bar pushing the tracks past the fold)
	     would resize the lane, and the lane the scrollbar, every frame. -->
	<div
		bind:this={scroller}
		onwheel={onWheel}
		onscroll={() => {
			syncView();
			// A scroll moves the lanes under a pointer that has not moved.
			if (marquee) onMarqueeMove(marquee.cx, marquee.cy);
		}}
		style="flex:1;min-height:0;overflow:auto;scrollbar-gutter:stable;position:relative;display:flex;align-items:flex-start"
	>
		<!-- track headers -->
		<div
			bind:this={headersEl}
			style="width:var(--track-header-w);flex:none;min-height:100%;position:sticky;left:0;z-index:40;border-right:var(--line-width) solid var(--border-default);background:var(--surface-app)"
		>
			<div
				style="height:var(--ruler-h);box-sizing:border-box;border-bottom:var(--line-width) solid {editor.rippleMode
					? 'var(--kerf-500)'
					: 'var(--border-subtle)'};position:sticky;top:0;z-index:50;background:var(--surface-app);display:flex;align-items:center;gap:6px;padding:0 8px;font-family:var(--font-mono);font-variant-numeric:tabular-nums;white-space:nowrap"
			>
				<!-- Where the playhead is, and how long the cut is: the timeline's own clock, so a scrub or a
				     drag here does not need the Preview on screen to say what time it is. The playhead's half is
				     where a timecode is typed (`TimecodeField`). -->
				<span
					data-timeline-time
					role="timer"
					aria-label="Playhead {tc(ui.time)} of {tc(editor.duration)}"
					style="display:inline-flex;align-items:baseline;gap:6px"
				>
					<TimecodeField entry="timeline" describedby="timeline-cut-length" />
					<span aria-hidden="true" style="font-size:11px;color:var(--text-muted)">/</span>
					<span id="timeline-cut-length" use:tip={'The length of the cut'} style="font-size:11px;color:var(--text-muted)"
						>{tc(editor.duration)}</span
					>
				</span>
				<span style="flex:1"></span>
				{#if editor.rippleMode}
					<span
						role="img"
						aria-label="Ripple mode is on"
						use:tip={'Ripple mode is on — edits pull the later clips on their track along'}
						style="display:inline-flex;color:var(--kerf-300)"><Icon n="between-horizontal-start" s={12} /></span
					>
				{/if}
			</div>
			{#if titlesEmpty}
				<!-- Nothing on the titles lane: a strip, and the whole of its header adds a title. -->
				<button
					data-title-add
					use:tip={'Add a title at the playhead'}
					aria-label="Add a title"
					disabled={editor.busy}
					onclick={addTitleHere}
					style="display:flex;align-items:center;gap:6px;width:100%;height:{titleLaneH}px;box-sizing:border-box;padding:0 8px;border:none;border-bottom:1px solid var(--border-subtle);background:none;cursor:pointer;text-align:left;color:var(--text-muted);font-size:10px"
				>
					<span style="font-family:var(--font-mono);font-weight:600;color:var(--text-secondary);flex:none">T</span>
					<span style="flex:1;min-width:0">Titles</span>
					<Icon n="plus" s={10} />
				</button>
			{:else}
			<div
				style="height:{titleLaneH}px;border-bottom:1px solid var(--border-subtle);display:flex;align-items:center;gap:6px;padding:0 8px;overflow:hidden"
			>
				<span style="font-family:var(--font-mono);font-size:12px;font-weight:600;color:var(--text-secondary);flex:none">T</span>
				<span style="font-size:11px;color:var(--text-muted);flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
					>Titles · {titleCount}</span
				>
				<button
					use:tip={'Add a title at the playhead'}
					aria-label="Add a title"
					disabled={editor.busy}
					onclick={addTitleHere}
					style="background:none;border:none;cursor:pointer;padding:0;min-width:26px;min-height:26px;flex:none;display:grid;place-items:center;color:var(--text-muted)"
					><Icon n="plus" s={12} /></button
				>
			</div>
			{/if}
			{#each drawn as t (t.id)}
				{@const mark = diff?.tracks.get(t.id)}
				<div
					role="presentation"
					data-track-header={t.id}
					data-diff={mark}
					oncontextmenu={(e) => onTrackHeaderContextMenu(e, t)}
					style="height:{trackHeight(t)};border-bottom:var(--line-width) solid var(--border-subtle);display:flex;flex-direction:column;justify-content:center;gap:4px;padding:0 8px;overflow:hidden;box-shadow:{[
						opensAudioGroup(drawn, t) ? 'inset 0 2px 0 var(--border-strong)' : '',
						mark ? `inset 3px 0 0 ${mark === 'added' ? 'var(--diff-add)' : 'var(--diff-shift)'}` : ''
					]
						.filter(Boolean)
						.join(',') || 'none'}"
				>
				<div style="display:flex;align-items:center;gap:5px">
					<!-- The name is the track's height menu (so is the header's right-click). -->
					<button
						use:tip={`${t.name} — ${PRESET_LABEL[ui.trackPreset(t.id)]} height (${ui.trackPx(t.id)} px). Click to change.`}
						aria-label="Track {t.name} height"
						aria-haspopup="menu"
						onclick={(e) => openHeightMenu(e, t)}
						style="flex:none;display:inline-flex;align-items:center;gap:1px;min-height:26px;padding:0;background:none;border:none;cursor:pointer;font-family:var(--font-mono);font-size:12px;font-weight:600;color:var(--text-secondary)"
						>{t.name}<Icon n="chevron-down" s={10} color="var(--text-muted)" /></button
					>
					<!-- No "Video"/"Audio" caption: the name (V1 / A1) and the eye vs
					     speaker icon both already say the kind, and the row now carries
					     mute / solo / lock / remove in a fixed-width column. -->
					<span style="flex:1;min-width:0"></span>
					{#if t.kind === 'audio'}
						<button
							use:tip={t.duck
								? 'Ducking on — this track dips under the rest of the mix on export (the preview plays it at its fader)'
								: 'Duck this track under the rest of the mix on export (the preview plays it at its fader)'}
							aria-label="Toggle ducking"
							onclick={() => void editor.setTrackDuck(t.id, !t.duck).catch(err)}
							style="background:{t.duck ? 'var(--kerf-500)' : 'none'};border:var(--line-width) solid {t.duck
								? 'var(--kerf-500)'
								: 'var(--border-strong)'};border-radius:3px;cursor:pointer;color:{t.duck
								? 'var(--text-on-accent)'
								: 'var(--text-muted)'};font-size:12px;font-weight:600;min-height:26px;padding:2px 4px;flex:none"
							>DUCK</button
						>
					{/if}
					<button
						use:tip={t.muted
							? t.kind === 'video'
								? 'Hidden — click to show'
								: 'Muted — click to unmute'
							: t.kind === 'video'
								? 'Hide this track'
								: 'Mute this track'}
						aria-label="Toggle mute"
						aria-pressed={!!t.muted}
						onclick={() => void editor.setTrackMuted(t.id, !t.muted).catch(err)}
						style="background:none;border:none;cursor:pointer;padding:0;min-width:26px;min-height:26px;flex:none;display:grid;place-items:center;color:{t.muted
							? 'var(--red-500)'
							: 'var(--text-muted)'}"
						><Icon n={t.kind === 'video' ? (t.muted ? 'eye-off' : 'eye') : t.muted ? 'volume-x' : 'volume-2'} s={12} /></button
					>
					<button
						use:tip={t.solo ? 'Soloed — click to clear' : 'Solo this track'}
						aria-label="Toggle solo"
						aria-pressed={!!t.solo}
						onclick={() => void editor.setTrackSolo(t.id, !t.solo).catch(err)}
						style={flagBtn(!!t.solo, 'var(--kerf-400)')}>S</button
					>
					<button
						use:tip={t.locked ? 'Locked — click to unlock' : 'Lock this track against edits'}
						aria-label="Toggle lock"
						aria-pressed={!!t.locked}
						onclick={() => void editor.setTrackLocked(t.id, !t.locked).catch(err)}
						style="flex:none;display:grid;place-items:center;min-width:26px;min-height:26px;padding:3px;border-radius:3px;cursor:pointer;border:var(--line-width) solid {t.locked
							? 'var(--text-secondary)'
							: 'var(--border-strong)'};background:{t.locked ? 'var(--surface-active)' : 'transparent'};color:{t.locked
							? 'var(--text-primary)'
							: 'var(--text-muted)'}"
						><Icon n={t.locked ? 'lock' : 'lock-open'} s={12} /></button
					>
					<button
						use:tip={'Remove track'}
						aria-label="Remove track"
						onclick={() => onRemoveTrack(t)}
						style="background:none;border:none;cursor:pointer;color:var(--text-muted);display:grid;place-items:center;padding:0;min-width:26px;min-height:26px;flex:none"
						><Icon n="x" s={12} /></button
					>
				</div>
					{#if hasSound(t) && ui.trackPx(t.id) >= MIXER_MIN_PX}
						<!-- The mixer strip: a fader over every clip on the track, and its
						     stereo placement. Balancing a music bed against a voice is the
						     one audio move every cut needs, and doing it clip by clip is
						     not the same control. -->
						<div style="display:flex;align-items:center;gap:6px">
							<!-- Position along the fader's dB taper (`mixer.ts`), not linear gain: the
							     same mapping, and so the same place for a level, as the Mixer's fader. -->
							<input
								type="range"
								min="0"
								max="1"
								step="0.005"
								value={gainToFader(t.volume ?? 1)}
								disabled={editor.locked}
								aria-label="{t.name} level"
								use:tip={`Level ${gainLabel(t.volume ?? 1)} — double-click for unity`}
								onchange={(e) => void editor.setTrackVolume(t.id, faderToGain(+e.currentTarget.value)).catch(err)}
								ondblclick={() => void editor.setTrackVolume(t.id, 1).catch(err)}
								style="flex:1;min-width:0;height:20px;--slider-accent:var(--kerf-400);cursor:pointer"
							/>
							<input
								type="range"
								min="-1"
								max="1"
								step="0.05"
								value={t.pan ?? 0}
								disabled={editor.locked}
								aria-label="{t.name} pan"
								use:tip={`Pan ${panLabel(t.pan ?? 0)} — double-click to centre`}
								onchange={(e) => void editor.setTrackPan(t.id, +e.currentTarget.value).catch(err)}
								ondblclick={() => void editor.setTrackPan(t.id, 0).catch(err)}
								style="width:48px;flex:none;height:20px;--slider-accent:var(--text-muted);cursor:pointer"
							/>
						</div>
					{/if}
				</div>
			{/each}
		</div>

		<!-- lanes -->
		<div
			bind:this={lanesEl}
			role="presentation"
			oncontextmenu={onTimelineContextMenu}
			onpointerdown={(e) => onCanvasPointerDown(e, 'canvas')}
			style="width:{contentW}px;min-width:calc(100% - var(--track-header-w));flex:none;min-height:100%;position:relative;overflow-x:clip"
		>
			<!-- Past the end of the cut: the lane runs to the window's edge, and what lies beyond the last
			     clip is dimmed behind a line, so a short cut is not mistaken for a short timeline. -->
			{#if hasClips && cutEndX < laneW}
				<div
					data-cut-end
					aria-hidden="true"
					style="position:absolute;left:{cutEndX}px;right:0;top:var(--ruler-h);bottom:0;z-index:0;pointer-events:none;box-sizing:border-box;border-left:var(--line-width) solid var(--border-strong);background:color-mix(in srgb,var(--surface-void) 55%,transparent)"
				></div>
			{/if}
			<!-- ruler — sticky under the playhead (z 30) but over the drag ghosts (z 25). Three rows: what the
			     analysis found hangs from the top edge (scene cuts, beats, bars), the label sits under it
			     beside its own tick, and the ticks stand on the bottom edge — a major one at every label,
			     minors between, the frames themselves once they are wide enough. -->
			<div
				bind:this={rulerEl}
				role="presentation"
				onpointerdown={onRulerPointerDown}
				style="height:var(--ruler-h);border-bottom:var(--line-width) solid {editor.rippleMode
					? 'var(--kerf-500)'
					: 'var(--border-subtle)'};position:sticky;top:0;z-index:26;background:var(--surface-app);cursor:pointer;touch-action:none"
			>
				{#if hasClips && cutEndX < laneW}
					<span
						aria-hidden="true"
						style="position:absolute;left:{cutEndX}px;right:0;top:0;bottom:0;pointer-events:none;box-sizing:border-box;border-left:var(--line-width) solid var(--border-strong);background:color-mix(in srgb,var(--surface-void) 55%,transparent)"
					></span>
				{/if}
				<RenderBar {pxPerSec} from={viewLo / pxPerSec} to={viewHi / pxPerSec} {tc} />
				{#each minorTicks as m (m.k)}
					<span
						data-tick={m.mid ? 'mid' : 'minor'}
						style="position:absolute;left:{m.t * pxPerSec}px;bottom:0;width:var(--line-width);height:{m.mid ? 7 : 4}px;background:color-mix(in srgb,var(--text-muted) {m.mid ? 70 : 45}%,transparent);pointer-events:none"
					></span>
				{/each}
				{#each frameTicks as f (f.k)}
					<span
						data-tick="frame"
						style="position:absolute;left:{f.t * pxPerSec}px;bottom:0;width:var(--line-width);height:6px;background:color-mix(in srgb,var(--text-muted) 70%,transparent);pointer-events:none"
					></span>
				{/each}
				{#each ticks as t (t.k)}
					<span
						data-tick="major"
						style="position:absolute;left:{t.t * pxPerSec}px;bottom:0;width:var(--line-width);height:12px;background:var(--text-muted);pointer-events:none"
					></span>
					<span
						data-tick-label
						style="position:absolute;left:{t.t * pxPerSec + 4}px;top:5px;font-family:var(--font-mono);font-size:10px;line-height:12px;color:var(--text-muted);pointer-events:none;white-space:nowrap"
						>{tickLabel(t.k, grid, editor.fps, timeDisplay.mode)}</span
					>
				{/each}
				{#each sceneXs as x (x)}
					<span
						use:tip={'Detected scene cut'}
						style="position:absolute;left:{x}px;top:0;width:0;height:0;border-left:4px solid transparent;border-right:4px solid transparent;border-top:5px solid var(--scene-marker);transform:translateX(-50%)"
					></span>
				{/each}
				{#each beatTimes as b (b)}
					<span
						style="position:absolute;left:{b * pxPerSec}px;top:0;width:var(--line-width);height:3px;background:var(--beat-marker);opacity:.75;pointer-events:none"
					></span>
				{/each}
				{#each barTimes as b (b)}
					<span
						style="position:absolute;left:{b * pxPerSec}px;top:0;width:calc(var(--line-width) * 2);height:5px;background:var(--beat-marker);pointer-events:none"
					></span>
				{/each}
				<!-- user markers: click seeks, drag moves, double-click renames -->
				{#each editor.markers as m (m.id)}
					{@const mt = markerDrag?.id === m.id ? markerDrag.time : m.time}
					{@const accent = m.color || 'var(--agent-400)'}
					<span
						role="presentation"
						use:tip={markerTip(m, tc(mt))}
						onpointerdown={(e) => onMarkerPointerDown(e, m)}
						ondblclick={() => (renaming = m.id)}
						oncontextmenu={(e) => onMarkerContextMenu(e, m)}
						style="position:absolute;left:{mt *
							pxPerSec}px;top:0;bottom:0;width:8px;z-index:29;cursor:ew-resize;touch-action:none;display:flex;align-items:center"
					>
						<span style="position:absolute;left:-1px;top:0;bottom:0;width:2px;background:{accent}"></span>
						{#if m.duration}
							<span
								style="position:absolute;left:0;bottom:0;height:3px;width:{m.duration *
									pxPerSec}px;background:{accent};opacity:.6;pointer-events:none"
							></span>
						{/if}
						{#if renaming === m.id}
							<!-- svelte-ignore a11y_autofocus -->
							<input
								autofocus
								value={m.name}
								onblur={(e) => commitRename(m, e.currentTarget.value)}
								onkeydown={(e) => {
									if (e.key === 'Enter') e.currentTarget.blur();
									else if (e.key === 'Escape') renaming = null;
									e.stopPropagation();
								}}
								style="position:relative;flex:none;margin-left:3px;width:96px;font-size:var(--text-2xs);padding:1px 3px;border-radius:3px;border:var(--line-width) solid {accent};background:var(--surface-inset);color:var(--text-primary)"
							/>
						{:else}
							<span
								style="position:relative;flex:none;margin-left:3px;max-width:120px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:var(--text-2xs);font-weight:600;line-height:1;padding:2px 4px;border-radius:3px;background:{accent};color:var(--text-on-accent);pointer-events:none"
								>{m.name}</span
							>
						{/if}
					</span>
				{/each}
				<!-- The playhead's head lives in the ruler, which stays put when the tracks scroll under it. -->
				<span
					data-playhead-head
					aria-hidden="true"
					style="position:absolute;left:{ui.time * pxPerSec - 5}px;top:-1px;width:12px;height:9px;background:var(--playhead);clip-path:polygon(0 0,100% 0,50% 100%);z-index:31;pointer-events:none"
				></span>
				{#if ui.markIn !== null && ui.markOut !== null && ui.markOut > ui.markIn}
					<div
						style="position:absolute;left:{ui.markIn * pxPerSec}px;width:{(ui.markOut - ui.markIn) *
							pxPerSec}px;top:0;bottom:0;background:var(--selection-fill);pointer-events:none"
					></div>
				{/if}
				{#if ui.markIn !== null}
					<span
						role="presentation"
						use:tip={`Mark in ${tc(ui.markIn)} — drag to move${clearHint('range.clearIn')}`}
						onpointerdown={(e) => onMarkPointerDown(e, 'in')}
						style="position:absolute;left:{ui.markIn * pxPerSec -
							3}px;top:0;bottom:0;width:12px;z-index:28;cursor:ew-resize;touch-action:none"
					>
						<span style="position:absolute;left:3px;top:0;bottom:0;width:2px;background:var(--kerf-400)"></span>
						<span
							style="position:absolute;top:0;left:5px;width:7px;height:7px;background:var(--kerf-400);clip-path:polygon(0 0,100% 0,0 100%)"
						></span>
					</span>
				{/if}
				{#if ui.markOut !== null}
					<span
						role="presentation"
						use:tip={`Mark out ${tc(ui.markOut)} — drag to move${clearHint('range.clearOut')}`}
						onpointerdown={(e) => onMarkPointerDown(e, 'out')}
						style="position:absolute;left:{ui.markOut * pxPerSec -
							9}px;top:0;bottom:0;width:12px;z-index:28;cursor:ew-resize;touch-action:none"
					>
						<span style="position:absolute;right:1px;top:0;bottom:0;width:2px;background:var(--kerf-400)"></span>
						<span
							style="position:absolute;top:0;right:3px;width:7px;height:7px;background:var(--kerf-400);clip-path:polygon(0 0,100% 0,100% 100%)"
						></span>
					</span>
				{/if}
			</div>

			<!-- grid lines -->
			{#if hasClips}
				{#each ticks as t (t.k)}
					<span
						style="position:absolute;left:{t.t * pxPerSec}px;top:var(--ruler-h);bottom:0;width:var(--line-width);pointer-events:none;background:{t.k % 2 ? 'var(--timeline-grid)' : 'var(--timeline-grid-major)'}"
					></span>
				{/each}
			{/if}

			<!-- titles lane: every title, lower-third and caption, in its own row of
			     items rather than a section of whichever clip is selected -->
			<div
				bind:this={titleLaneEl}
				role="presentation"
				data-title-lane
				onclick={onLaneSeek}
				onpointerdown={(e) => onCanvasPointerDown(e, 'lane')}
				oncontextmenu={onTitleLaneContextMenu}
				style="height:{titleLaneH}px;border-bottom:1px solid var(--border-subtle);position:relative"
			>
				{#each editor.overlays as o (o.id)}
					{@const live = titleDrag?.moved && titleDrag.id === o.id ? titleDrag : null}
					{@const row = titleRows.get(o.id) ?? 0}
					{@const width = Math.max(6, (o.end - o.start) * pxPerSec)}
					{@const selected = editor.selectedOverlayId === o.id}
					<button
						class="ktitle"
						data-diff={diff?.titles.get(o.id)}
						data-touched={touched?.titles.has(o.id) ? '' : undefined}
						onpointerdown={(e) => onTitlePointerDown(e, o, 'move')}
						oncontextmenu={(e) => onTitleContextMenu(e, o)}
						onclick={(e) => e.stopPropagation()}
						use:tip={`${o.generated ? 'Caption' : 'Title'}: ${o.text}`}
						style="position:absolute;left:{o.start * pxPerSec}px;top:{titleMetrics.pad + row * titleMetrics.row}px;height:{titleMetrics.row - 3}px;width:{width}px;border-radius:2px;overflow:hidden;display:flex;align-items:center;padding:0 7px;touch-action:none;opacity:{live ? 0.4 : 1};cursor:{titleDrag ? 'grabbing' : 'grab'};text-align:left;background:{o.generated ? GENERATED_TITLE_FILL : 'var(--track-text)'};border:{selected ? '1.5px solid var(--kerf-400)' : `1px ${o.generated ? 'dashed' : 'solid'} var(--track-text-edge)`};box-shadow:{selected ? '0 0 0 1px var(--kerf-500)' : 'none'}"
					>
						<span
							style="position:relative;font-size:{titleMetrics.font}px;font-weight:{o.generated ? 500 : 600};color:var(--text-on-video);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
							>{o.text}</span
						>
						{#if width > 24}
							<span
								role="presentation"
								onpointerdown={(e) => onTitlePointerDown(e, o, 'l')}
								style="position:absolute;left:0;top:0;bottom:0;width:6px;cursor:ew-resize;z-index:3;touch-action:none"
							></span>
							<span
								role="presentation"
								onpointerdown={(e) => onTitlePointerDown(e, o, 'r')}
								style="position:absolute;right:0;top:0;bottom:0;width:6px;cursor:ew-resize;z-index:3;touch-action:none"
							></span>
						{/if}
					</button>
					{#if live}
						<div
							style="position:absolute;left:{live.start * pxPerSec}px;top:{titleMetrics.pad + row * titleMetrics.row}px;height:{titleMetrics.row - 3}px;width:{Math.max(
								6,
								(live.end - live.start) * pxPerSec
							)}px;border:1.5px dashed var(--kerf-400);border-radius:2px;background:color-mix(in srgb,var(--drag-ghost) 16%,transparent);pointer-events:none;z-index:25"
						></div>
					{/if}
				{/each}
				{#each editor.svgOverlays as o (o.id)}
					{@const live = titleDrag?.moved && titleDrag.id === o.id ? titleDrag : null}
					{@const row = titleRows.get(o.id) ?? 0}
					{@const width = Math.max(6, (o.end - o.start) * pxPerSec)}
					{@const selected = editor.selectedSvgOverlayId === o.id}
					<!-- An animated SVG title: the same block as a title, in the same rows, behind a motion
					     icon and its own fill and edge so it reads as artwork, not text. -->
					<button
						class="ktitle"
						data-kind="svg"
						data-diff={diff?.titles.get(o.id)}
						onpointerdown={(e) => onTitlePointerDown(e, o, 'move', 'svg')}
						oncontextmenu={(e) => onSvgContextMenu(e, o)}
						onclick={(e) => e.stopPropagation()}
						use:tip={svgTooltip(o)}
						style="position:absolute;left:{o.start * pxPerSec}px;top:{titleMetrics.pad + row * titleMetrics.row}px;height:{titleMetrics.row - 3}px;width:{width}px;border-radius:2px;overflow:hidden;display:flex;align-items:center;gap:4px;padding:0 7px;touch-action:none;opacity:{live ? 0.4 : 1};cursor:{titleDrag ? 'grabbing' : 'grab'};text-align:left;background:{SVG_TITLE_FILL};border:{selected ? '1.5px solid var(--kerf-400)' : '1px solid var(--track-video-edge)'};box-shadow:{selected ? '0 0 0 1px var(--kerf-500)' : 'none'}"
					>
						{#if width > 34}<span style="position:relative;display:inline-flex;flex:none;color:var(--text-on-video)"><Icon n="orbit" s={titleMetrics.font + 1} /></span>{/if}
						<span
							style="position:relative;font-size:{titleMetrics.font}px;font-weight:600;color:var(--text-on-video);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
							>{svgDisplayName(o)}</span
						>
						{#if width > 24}
							<span
								role="presentation"
								onpointerdown={(e) => onTitlePointerDown(e, o, 'l', 'svg')}
								style="position:absolute;left:0;top:0;bottom:0;width:6px;cursor:ew-resize;z-index:3;touch-action:none"
							></span>
							<span
								role="presentation"
								onpointerdown={(e) => onTitlePointerDown(e, o, 'r', 'svg')}
								style="position:absolute;right:0;top:0;bottom:0;width:6px;cursor:ew-resize;z-index:3;touch-action:none"
							></span>
						{/if}
					</button>
					{#if live}
						<div
							style="position:absolute;left:{live.start * pxPerSec}px;top:{titleMetrics.pad + row * titleMetrics.row}px;height:{titleMetrics.row - 3}px;width:{Math.max(
								6,
								(live.end - live.start) * pxPerSec
							)}px;border:1.5px dashed var(--kerf-400);border-radius:2px;background:color-mix(in srgb,var(--drag-ghost) 16%,transparent);pointer-events:none;z-index:25"
						></div>
					{/if}
				{/each}
				{#each diff?.removedTitles ?? [] as r (r.id)}
					<span
						data-diff-removed
						style="position:absolute;left:{r.start * pxPerSec}px;top:{titleMetrics.pad}px;height:{titleMetrics.row - 3}px;width:{Math.max(6, (r.end - r.start) * pxPerSec)}px;box-sizing:border-box;border:1.5px dashed var(--diff-remove);border-radius:2px;background:repeating-linear-gradient(135deg,color-mix(in srgb,var(--diff-remove) 45%,transparent) 0 1px,var(--diff-remove-surface) 1px 6px);pointer-events:none"
					></span>
				{/each}
			</div>

			<!-- tracks, the topmost video layer first: V2 is drawn over V1, as it composites -->
			{#each drawn as t (t.id)}
				<div
					role="presentation"
					data-lane
					data-track-id={t.id}
					data-kind={t.kind}
					data-locked={t.locked ? '' : undefined}
					onclick={onLaneSeek}
					onpointerdown={(e) => onCanvasPointerDown(e, 'lane', laneDrawings.get(t.id)?.blocks)}
					onpointermove={(e) => onLaneHover(e, t)}
					onpointerleave={() => {
						rollHover = null;
						razor = null;
					}}
					oncontextmenu={(e) => onLaneContextMenu(e, t)}
					ondragover={(e) => onLaneDragOver(e, t)}
					ondragleave={(e) => onLaneDragLeave(e, t)}
					ondrop={(e) => onLaneDrop(e, t)}
					style="height:{trackHeight(t)};border-bottom:var(--line-width) solid var(--border-subtle);position:relative;box-shadow:{opensAudioGroup(drawn, t)
						? 'inset 0 2px 0 var(--border-strong)'
						: 'none'}"
				>
					{#if !hasClips && t.id === drawn[0]?.id}
						<!-- In the first lane, where the clip will land — it used to be centred over the whole stack and sat on the line between
						     V1 and A1. Sticky, so it stays on screen when the timeline is scrolled. -->
						<span
							data-drop-hint
							style="position:sticky;left:calc(var(--track-header-w) + 12px);display:flex;align-items:center;width:max-content;max-width:calc(100% - var(--track-header-w) - 24px);height:100%;color:var(--text-muted);font-size:12px;pointer-events:none;white-space:nowrap"
							>Drop media here to start cutting.</span
						>
					{/if}
					{#if drag?.moved && drag.trackId === t.id}
						<!-- Where the dragged clip would land: the lane under the pointer takes a tint. -->
						<span
							aria-hidden="true"
							style="position:absolute;inset:0;background:var(--selection-fill);pointer-events:none"
						></span>
					{:else if drag?.moved && t.kind !== drag.kind}
						<!-- A lane the clip cannot go to (a picture does not go to sound) is stood back. -->
						<span
							aria-hidden="true"
							style="position:absolute;inset:0;z-index:23;background:color-mix(in srgb,var(--surface-panel) 55%,transparent);pointer-events:none"
						></span>
					{/if}
					{#each (diff?.removed ?? []).filter((r) => r.trackId === t.id) as r (r.id)}
						<!-- What the proposal removes, where it stood: hatched under whatever now fills the space, with a
						     strip along the top that stays visible when something does. -->
						<span
							data-diff-removed
							style="position:absolute;left:{r.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(4, r.dur * pxPerSec)}px;box-sizing:border-box;border:1.5px dashed var(--diff-remove);border-radius:2px;background:repeating-linear-gradient(135deg,color-mix(in srgb,var(--diff-remove) 45%,transparent) 0 1px,var(--diff-remove-surface) 1px 6px);pointer-events:none"
						></span>
						<span
							aria-hidden="true"
							style="position:absolute;left:{r.start * pxPerSec}px;top:0;height:4px;width:{Math.max(4, r.dur * pxPerSec)}px;background:var(--diff-remove);opacity:.85;pointer-events:none;z-index:22"
						></span>
					{/each}
					{#if t.locked}
						<!-- A locked track refuses every edit, so it looks it: hatched lane, and a padlock that
						     stays at the left edge of the window as the lane scrolls. -->
						<span
							data-lock-badge
							use:tip={`${t.name} is locked — unlock it to edit`}
							style="position:sticky;left:calc(var(--track-header-w) + 6px);top:0;z-index:20;display:inline-flex;align-items:center;margin:2px 0 0 6px;padding:2px 4px;border-radius:3px;pointer-events:none;color:var(--text-on-video);background:color-mix(in srgb,var(--scrim) {NAME_CHIP_PCT}%,transparent)"
							><Icon n="lock" s={10} /></span
						>
					{/if}
					{#if (laneDrawings.get(t.id)?.blocks.length ?? 0) > 0}
						<LaneBlocks
							blocks={laneDrawings.get(t.id)?.blocks ?? []}
							duration={clipDuration}
							{pxPerSec}
							lo={viewLo}
							hi={Math.min(viewHi, laneW)}
							{dpr}
							fill={blockColors[t.kind === 'audio' ? 'audio' : 'video'].fill}
							edge={blockColors[t.kind === 'audio' ? 'audio' : 'video'].edge}
							marks={blockColors.marks}
							mark={markOf}
							faded={(c) => c.enabled === false || !renders(t) || movedBlock(c.id)}
							off={(c) => c.enabled === false}
							filter={!renders(t) ? 'grayscale(1)' : t.locked ? 'saturate(0.45)' : 'none'}
							cursor={clipCursor(t)}
							onhover={(e, id) => {
								if (mayPromote(e.buttons, !!(drag || trimDrag || tg || marquee))) hoverId = id;
							}}
							onpress={(e, c) => {
								hoverId = c.id;
								onClipPointerDown(e, c, t);
							}}
							oncontext={(e, c) => onClipContextMenu(e, c, t)}
						/>
					{/if}
					{#each laneDrawings.get(t.id)?.buttons ?? t.clips as c (c.id)}
						{@const left = c.timeline_start * pxPerSec}
						{@const width = Math.max(6, clipDuration(c) * pxPerSec)}
						{@const selected = editor.isSelected(c.id)}
						{@const primary = editor.selectedClipId === c.id}
						{@const sliding =
							!!tg?.moved &&
							tg.edit?.tool === 'slide' &&
							(tg.edit.clipId === c.id || slidPartners.has(c.id))}
						{@const dragging = ((drag?.moved || !!nudge) && dragIds.has(c.id)) || !!trimView?.shifted.has(c.id) || sliding}
						<!-- A slip shows the footage moving under the clip's edges as it is dragged:
						     the picture and the waveform are drawn from the window it would have — the
						     clip's linked partners' too, which slip with it. -->
						{@const shown =
							tg?.moved && tg.edit?.tool === 'slip'
								? (tgPreview?.ghosts.find((g) => g.id === c.id)?.clip ?? c)
								: c}
						{@const off = c.enabled === false || !renders(t)}
						{@const badge = badges.get(c.id)}
						{@const lit = hoverPartners.has(c.id)}
						{@const title = titleOf(c)}
						{@const name = title ? titleName(title) : editor.assetName(c.asset_id)}
						<button
							class="kclip"
							data-clip-id={c.id}
							data-lit={lit ? '' : undefined}
							data-diff={diff?.clips.get(c.id)}
							data-touched={touched?.clips.has(c.id) ? '' : undefined}
							use:tip={showsLabel(width) ? undefined : name}
							data-linked={badge && badge.partners.length > 0 ? '' : undefined}
							data-sound-detached={c.source_audio === false ? '' : undefined}
							data-title-clip={title ? '' : undefined}
							onpointerdown={(e) => onClipPointerDown(e, c, t)}
							onkeydown={(e) => onClipKey(e, c, t)}
							onfocus={() => (focusId = c.id)}
							onblur={() => {
								focusId = focusId === c.id ? null : focusId;
								// Focus moving on writes the run; the window losing it (alt-tab) gives it up, as every gesture does.
								if (nudge?.clipId === c.id) documentOf(scroller).hasFocus() ? commitNudge() : abandonNudge();
							}}
							onpointerenter={() => (hoverId = c.id)}
							onpointerleave={() => (hoverId = hoverId === c.id ? null : hoverId)}
							oncontextmenu={(e) => onClipContextMenu(e, c, t)}
							onclick={(e) => {
								e.stopPropagation();
								if (e.detail > 0) {
									// A pointer click: the clip is not left holding the keyboard, or Space (play) would
									// press it — and replace a multi-selection with it — instead of reaching the page.
									(e.currentTarget as HTMLElement).blur();
								} else if (!selected) {
									// Enter or Space on a focused clip: a click has no pointer position. A clip that is
									// already selected leaves the selection (a group, a linked pair) as it is.
									editor.selectClip(c.id);
									void editor.select(c.asset_id);
								}
							}}
							style="--bw:{selected ? 'var(--line-emphasis)' : 'var(--line-width)'};position:absolute;left:{left}px;top:5px;height:calc(100% - 10px);width:{width}px;border-radius:2px;overflow:clip;display:flex;align-items:center;padding:0 {clipPadding(width)}px;touch-action:none;filter:{off
								? 'grayscale(1)'
								: t.locked
									? 'saturate(0.45)'
									: 'none'};opacity:{dragging ? 0.4 : off ? 0.4 : 1};cursor:{clipCursor(t)};text-align:left;background:{t.kind === 'audio' ? 'var(--track-audio)' : 'var(--track-video)'};border:{selected ? 'var(--line-emphasis) solid var(--kerf-400)' : `var(--line-width) solid ${t.kind === 'audio' ? 'var(--track-audio-edge)' : 'var(--track-video-edge)'}`};box-shadow:{primary
								? '0 0 0 1px var(--kerf-500)'
								: selected
									? '0 0 0 1px var(--kerf-600)'
									: 'none'}"
						>
							{#if t.kind === 'audio'}
								<ClipWaveform
									clip={shown}
									{width}
									{pxPerSec}
									{dpr}
									{viewLo}
									{viewHi}
									trackVolume={t.volume ?? 1}
									liveVolume={liveVolume[c.id] ?? null}
									duration={assetDuration.get(c.asset_id) ?? c.source_out}
									{palette}
								/>
								{#each silenceRegions(shown) as r (r.left)}
									<span
										use:tip={'Detected silence'}
										style="position:absolute;left:{r.left - left}px;top:3px;bottom:3px;width:{Math.max(2, r.width)}px;background:var(--silence-region);border:var(--line-width) solid color-mix(in srgb,var(--red-500) 30%,transparent);border-radius:2px"
									></span>
								{/each}
							{:else if title}
								<!-- A title draws no footage: its mark at the head, where a filmstrip's first frame would be. -->
								<span
									data-title-mark
									style="position:absolute;left:4px;top:4px;display:inline-flex;color:color-mix(in srgb,var(--text-on-video) 75%,transparent);pointer-events:none"
									><Icon n="layout-template" s={11} /></span
								>
							{:else}
								<ClipFilmstrip
									clip={shown}
									{width}
									{pxPerSec}
									{dpr}
									{viewLo}
									{viewHi}
									onready={(ready) => {
										if (ready) filmReady[c.id] = true;
										else delete filmReady[c.id];
									}}
								/>
							{/if}
							{#if c.transition_in}
								<span
									use:tip={`${transitionLabel(c.transition_in.kind)} ${c.transition_in.duration.toFixed(2)}s`}
									style="position:absolute;left:0;top:0;bottom:0;width:{Math.min(
										c.transition_in.duration * pxPerSec,
										width
									)}px;background:linear-gradient(to right, color-mix(in srgb,var(--drag-ghost) 55%,transparent), transparent);border-left:2px solid var(--clip-accent);pointer-events:none"
								></span>
							{/if}
							<!-- The name. An audio clip's sits at its foot — the middle belongs to the waveform (and
							     the line between a stereo clip's lanes) — and so does a video clip's once it has
							     thumbnails; both on a backing, since a bright frame or a loud wave under white text
							     is unreadable otherwise. It is sticky: scrolled until the clip's start is off the
							     window, the name stays at the window's left edge for as long as any of the clip is
							     in it, and a clip too narrow for a name has none (its tooltip says it). -->
							{#if showsLabel(width)}
								{@const chip = t.kind === 'audio' || !!filmReady[c.id]}
								<span
									class="kname"
									data-chip={chip ? '' : undefined}
									data-foot={chip ? '' : undefined}
									style="--chip:{NAME_CHIP_PCT}%">{name}</span
								>
							{/if}
							{#if (c.speed ?? 1) !== 1}
								{@const sp = c.speed ?? 1}
								<span
									use:tip={`Speed ${sp}×`}
									style="position:absolute;right:3px;top:3px;font-size:var(--text-2xs);font-weight:700;color:var(--text-on-video);background:color-mix(in srgb,var(--scrim) 55%,transparent);border-radius:3px;padding:1px 4px;pointer-events:none"
									>{sp < 0 ? `${Math.abs(sp)}× ⟲` : `${sp}×`}</span
								>
							{/if}
							{#if badge && width >= 22}
								<!-- The link: a chain on a clip that has partners, and a muted speaker on a
								     picture whose sound was detached. Partners light up while it is hovered
								     (the clip's own hover does that too); the tooltip says what Alt does. -->
								<span
									data-link-badge
									use:tip={badge.title}
									style="position:absolute;right:8px;bottom:3px;z-index:3;display:inline-flex;align-items:center;gap:3px;padding:1px 4px;border-radius:3px;background:color-mix(in srgb,var(--scrim) 60%,transparent);color:{lit
										? 'var(--kerf-200)'
										: 'var(--text-on-video)'};box-shadow:{lit ? '0 0 0 1px var(--kerf-400)' : 'none'}"
								>
									{#if badge.partners.length > 0}<Icon n="link" s={10} />{/if}
									{#if badge.detached}<Icon n="volume-x" s={10} />{/if}
								</span>
							{/if}
							<ClipOverlays
								clip={c}
								{width}
								{pxPerSec}
								{fps}
								sound={audibleAssets.has(c.asset_id) && c.source_audio !== false}
								{selected}
								locked={!!t.locked}
								tooled={ui.tool !== 'pointer'}
								trimEdge={trimDrag?.clipId === c.id ? trimDrag.edge : null}
								onlive={(v) => (liveVolume[c.id] = v)}
								onselect={() => {
									editor.selectClip(c.id);
									void editor.select(c.asset_id);
								}}
								onvolume={(v) => editor.setVolume(c.id, v).catch(err)}
								onfade={(which, v) =>
									(which === 'in' ? editor.setFade(c.id, v) : editor.setFade(c.id, undefined, v)).catch(err)}
								onseek={(time) => ui.seek(time, { snap: 'exact' })}
								onedge={(e, edge) => onEdgePointerDown(e, c, t, edge)}
							/>
						</button>
					{/each}
					<!-- One ghost per clip of the dragged group, where it would land: red
					     all over when the group cannot (the drop then does nothing). A linked
					     partner carried along is a ghost too, on its own track, marked with the chain. -->
					{#each ghostsByTrack.get(t.id) ?? [] as g (g.clipId)}
						<div
							data-ghost={g.carried ? 'carried' : 'moved'}
							style="position:absolute;left:{g.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
								6,
								g.dur * pxPerSec
							)}px;border:1.5px dashed {dropRefused ? 'var(--red-500)' : g.carried ? 'var(--kerf-300)' : 'var(--kerf-400)'};border-radius:2px;background:{dropRefused
								? 'var(--danger-surface)'
								: `color-mix(in srgb,var(--drag-ghost) ${g.carried ? 10 : 16}%,transparent)`};pointer-events:none;z-index:25;overflow:hidden"
						>
							{#if g.carried}
								<span style="position:absolute;left:3px;top:2px;color:{dropRefused ? 'var(--red-500)' : 'var(--kerf-300)'}"
									><Icon n="link" s={10} /></span
								>
							{/if}
						</div>
					{/each}
					{#if trimView}
						<!-- Where the trimmed clip and everything the trim changes would be, on every
						     track: its linked partners following the edge, the clips ripple pushes and
						     — the sync lock — the partners of those. A left edge keeps the clip's start
						     under ripple, so this is not the edge dragged. -->
						{#each trimView.ghosts.filter((g) => g.trackId === t.id) as g (g.id)}
							<div
								data-ghost={g.id === trimDrag?.clipId ? 'trimmed' : trimView.shifted.has(g.id) ? 'shifted' : 'partner'}
								style="position:absolute;left:{g.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
									2,
									g.dur * pxPerSec
								)}px;border:1.5px dashed {trimView.ok ? 'var(--kerf-400)' : 'var(--red-500)'};border-radius:2px;background:{trimView.ok
									? 'color-mix(in srgb,var(--drag-ghost) 16%,transparent)'
									: 'var(--danger-surface)'};pointer-events:none;z-index:25"
							></div>
						{/each}
					{:else if trimDrag?.moved && trimDrag.trackId === t.id}
						{@const gl = trimDrag.edge === 'l' ? trimDrag.pos : trimDrag.origStart}
						{@const gr = trimDrag.edge === 'l' ? trimDrag.origEnd : trimDrag.pos}
						<div
							style="position:absolute;left:{gl * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
								2,
								(gr - gl) * pxPerSec
							)}px;border:1.5px dashed var(--kerf-400);border-radius:2px;background:color-mix(in srgb,var(--drag-ghost) 16%,transparent);pointer-events:none;z-index:25"
						></div>
					{/if}
					{#if trimDrag?.moved && trimDrag.trackId === t.id}
						<!-- The part of the clip the trim takes away is hatched and stood back, so a trim reads as a cut. -->
						{@const cutFrom = trimDrag.edge === 'l' ? trimDrag.origStart : Math.min(trimDrag.pos, trimDrag.origEnd)}
						{@const cutTo = trimDrag.edge === 'l' ? Math.max(trimDrag.pos, trimDrag.origStart) : trimDrag.origEnd}
						{#if cutTo - cutFrom > 1e-6}
							<div
								data-trim-away
								style="position:absolute;left:{cutFrom * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(1, (cutTo - cutFrom) * pxPerSec)}px;border-radius:2px;background:repeating-linear-gradient(135deg,color-mix(in srgb,var(--scrim) 80%,transparent) 0 2px,color-mix(in srgb,var(--scrim) 55%,transparent) 2px 6px);pointer-events:none;z-index:24"
							></div>
						{/if}
					{/if}
					{#if razorLanes.has(t.id) && razor && razor.at !== null && ui.tool === 'razor'}
						<!-- The cut the razor would make, through this clip and the partners cut with it. -->
						<div
							data-razor-line
							style="position:absolute;left:{razor.at * pxPerSec - 1}px;top:2px;bottom:2px;width:2px;border-radius:1px;background:var(--text-primary);box-shadow:0 0 0 1px color-mix(in srgb,var(--scrim) 70%,transparent);pointer-events:none;z-index:27"
						></div>
					{/if}
					{#if rollHover?.trackId === t.id && !tg}
						<!-- The cut the roll tool would take hold of. -->
						<div
							style="position:absolute;left:{rollHover.time * pxPerSec - 1}px;top:3px;bottom:3px;width:2px;border-radius:1px;background:var(--kerf-300);box-shadow:0 0 6px 1px var(--kerf-400);pointer-events:none;z-index:24"
						></div>
					{/if}
					{#if tg?.moved && tgPreview}
						<!-- A roll, slip or slide in progress: every clip it changes, where it would
						     stand — a linked clip's partners on their own tracks. Amber when the drag is
						     held at a limit, red when the backend would turn it down (letting go then
						     writes nothing). -->
						{@const pv = tgPreview}
						{@const edge = !pv.ok ? 'var(--red-500)' : pv.clamped ? 'var(--warning)' : 'var(--kerf-400)'}
						{#each pv.ghosts.filter((g) => g.trackId === t.id) as g (g.id)}
							<div
								data-ghost={g.role === 'partner' ? 'partner' : g.role}
								style="position:absolute;left:{g.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
									2,
									g.dur * pxPerSec
								)}px;border:1.5px dashed {edge};border-radius:2px;background:{!pv.ok
									? 'var(--danger-surface)'
									: pv.clamped
										? 'var(--warning-surface)'
										: 'color-mix(in srgb,var(--drag-ghost) 16%,transparent)'};pointer-events:none;z-index:25"
							></div>
						{/each}
						{#if tg.edit?.tool === 'roll' && tg.trackId === t.id}
							<!-- Where the cut was, and where it is: the distance is the roll. -->
							<div
								style="position:absolute;left:{tg.origin * pxPerSec}px;top:2px;bottom:2px;width:0;border-left:1px dashed var(--text-muted);pointer-events:none;z-index:25"
							></div>
							<div
								style="position:absolute;left:{(tg.origin + pv.applied) * pxPerSec - 1}px;top:2px;bottom:2px;width:2px;border-radius:1px;background:{edge};box-shadow:0 0 6px 1px {edge};pointer-events:none;z-index:26"
							></div>
						{/if}
					{/if}
					{#if dropGhost && dropGhost.trackId === t.id}
						<div
							style="position:absolute;left:{dropGhost.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
								6,
								dropGhost.dur * pxPerSec
							)}px;border:1.5px dashed {dropGhost.ok
								? 'var(--kerf-400)'
								: 'var(--red-500)'};border-radius:2px;background:{dropGhost.ok
								? 'var(--selection-fill)'
								: 'var(--danger-surface)'};pointer-events:none;z-index:25"
						></div>
					{/if}
				</div>
			{/each}

			<!-- the marquee: what a drag on empty space is selecting -->
			{#if marquee?.moved}
				{@const r = normalizeRect({ x0: marquee.ax, y0: marquee.ay, x1: marquee.x, y1: marquee.y })}
				<div
					style="position:absolute;left:{r.x0}px;top:{r.y0}px;width:{r.x1 - r.x0}px;height:{r.y1 - r.y0}px;border:var(--line-width) solid var(--kerf-400);background:var(--selection-fill);pointer-events:none;z-index:24"
				></div>
			{/if}

			<!-- a roll, slip or slide: how far, and what it leaves — or what is stopping it -->
			{#if tg?.moved && tgPreview}
				{@render readoutChip(readoutFor(tgPreview, fps, timeDisplay.mode), tg.nx, tg.ny)}
			{/if}

			<!-- an edge trim, a clip move or the razor: the number the gesture is making, and why it is red or held -->
			{#if chip}
				{@render readoutChip(chip.r, chip.x, chip.y)}
			{/if}

			<!-- an arrow-key nudge: how far the run has taken the clip, by the clip itself -->
			{#if nudge && nudgeAt !== null && dragPlan}
				{@const lane = lanesEl?.querySelector<HTMLElement>(`[data-lane][data-track-id="${nudge.trackId}"]`)}
				{@render readoutChip(
					moveReadout({
						delta: nudgeAt - nudge.origStart,
						start: nudgeAt,
						track: editor.timeline.tracks.find((t) => t.id === nudge!.trackId)?.name ?? '',
						fps,
						count: nudge.members.size,
						refused: dragPlan.ok ? null : dragPlan.reason || 'that move would not fit',
						mode: timeDisplay.mode
					}),
					nudgeAt * pxPerSec - 14,
					(lane?.offsetTop ?? 0) + (lane?.offsetHeight ?? 0) - 18
				)}
			{/if}

			<!-- the magnet a held edge is on: a line through the lanes, and its name -->
			{#each snapNow as line (line.at)}
				<div
					data-snap-line
					aria-hidden="true"
					style="position:absolute;left:{line.at * pxPerSec - 0.5}px;top:var(--ruler-h);bottom:0;width:var(--line-width);z-index:34;pointer-events:none;background:color-mix(in srgb,var(--text-primary) 85%,transparent)"
				>
					<span
						data-snap-label
						style="position:sticky;top:calc(var(--ruler-h) + 3px);display:block;width:max-content;margin-left:{line.at * pxPerSec - scrollX + 96 > viewW ? '-96px' : '4px'};padding:1px 5px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--border-strong);background:var(--surface-raised);color:var(--text-primary);font-family:var(--font-mono);font-size:10px;line-height:13px;white-space:nowrap"
						>{line.label}</span
					>
				</div>
			{/each}

			<!-- playhead -->
			<div
				style="position:absolute;left:{ui.time * pxPerSec}px;top:0;bottom:0;width:var(--playhead-width);background:var(--playhead);box-shadow:0 0 10px 1px var(--playhead-glow);z-index:30;pointer-events:none"
			></div>
		</div>
	</div>
</div>

{#if editingMarker}
	<MarkerDialog markerId={editingMarker} onClose={() => (editingMarker = null)} />
{/if}

<style>
	/* A locked lane is hatched: the track header says so too, but a lane that looks like any other is
	   how an edit gets lost. The lines are the muted text colour at a whisper, so any theme draws them. */
	[data-lane][data-locked] {
		background-image: repeating-linear-gradient(
			135deg,
			color-mix(in srgb, var(--text-muted) 22%, transparent) 0 1px,
			transparent 1px 7px
		);
	}

	/* The clips draw their own outline: a partner of the clip under the pointer lights up, and the one
	   with keyboard focus rings in the one focus colour (an inline `outline: none` used to beat it). */
	.kclip[data-lit] {
		outline: 1.5px solid var(--kerf-300);
		outline-offset: 1px;
	}
	.kclip:focus-visible {
		outline: var(--focus-outline);
		outline-offset: 1px;
		z-index: 4;
	}

	/* A staged proposal on the timeline: what it adds is framed solid in the add colour, what it changes
	   dashed in the shift colour, so the two read apart without the hue. The frame is drawn just inside
	   the clip (a pseudo-element, not the outline), so the partner highlight and the focus ring above
	   still show on a marked clip. */
	.kclip[data-diff]::after,
	.ktitle[data-diff]::after {
		content: '';
		position: absolute;
		inset: 0;
		z-index: 1;
		box-sizing: border-box;
		border: 2px solid var(--diff-add);
		border-radius: inherit;
		pointer-events: none;
	}
	.kclip[data-diff='shifted']::after,
	.ktitle[data-diff='shifted']::after {
		border: 2px dashed var(--diff-shift);
	}
	/* Before the proposal is previewed: the clips and titles it would touch carry a faint dotted frame, so the
	   cut shows where the agent has been without being replaced by it. */
	.kclip[data-touched]::after,
	.ktitle[data-touched]::after {
		content: '';
		position: absolute;
		inset: 0;
		z-index: 1;
		box-sizing: border-box;
		border: 2px dotted color-mix(in srgb, var(--agent-400) 70%, transparent);
		border-radius: inherit;
		pointer-events: none;
	}

	.kname {
		position: sticky;
		left: calc(var(--track-header-w) + 7px);
		min-width: 0;
		font-size: 10px;
		font-weight: 600;
		color: var(--text-on-video);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.kname[data-foot] {
		align-self: flex-end;
		margin-bottom: 3px;
	}
	.kname[data-chip] {
		left: calc(var(--track-header-w) + 3px);
		margin-left: -4px;
		padding: 1px 5px;
		border-radius: 3px;
		background: color-mix(in srgb, var(--scrim) var(--chip), transparent);
		text-shadow: 0 1px 2px color-mix(in srgb, var(--scrim) 85%, transparent);
	}
</style>
