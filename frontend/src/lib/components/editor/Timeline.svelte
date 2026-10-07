<script lang="ts">
	import { untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import ClipOverlays from './ClipOverlays.svelte';
	import ClipFilmstrip from './ClipFilmstrip.svelte';
	import ClipWaveform from './ClipWaveform.svelte';
	import HeightGlyph from './HeightGlyph.svelte';
	import Minimap from './Minimap.svelte';
	import { toast } from '$lib/notifications.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { settings } from '$lib/settings.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import type { MenuItem } from '$lib/context-menu.svelte';
	import { deleteSelection, trimSelection } from '$lib/ops';
	import { importCaptionFile } from '$lib/title-actions';
	import { importMenuEntries, importableAssets } from '$lib/caption-import-ui';
	import type { Clip, Marker, StreamKind, TextOverlay, Track } from '$lib/types';
	import { GENERATED_TITLE_FILL, packRows, snapSpanStart, snapTime, trimSpan } from '$lib/titles';
	import { gainLabel, MAX_GAIN, panLabel } from '$lib/mixer';
	import { clampEdge, quantizeSpanStart, quantizeTime, splitPoint, startBefore, trimEdit } from '$lib/frames';
	import { beginDrag } from '$lib/drag';
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
		type EditPreview,
		type GestureEdit,
		type TrimTool
	} from '$lib/trim-tools';
	import { readPalette } from '$lib/waveform-draw';
	import { filmstrips } from '$lib/filmstrips';
	import {
		HEIGHT_PRESETS,
		MIXER_MIN_PX,
		PRESET_LABEL,
		PRESET_PX,
		TITLE_METRICS,
		titleLaneHeight,
		uniformPreset
	} from '$lib/track-heights';
	import type { Target } from '$lib/minimap';
	import { visibleLaneRange } from '$lib/waveform-view';
	import { clipDuration } from '$lib/types';
	import { beatGrid, beatPeriod, sourceToTimeline } from '$lib/beats';
	import { transitionLabel } from '$lib/transitions';
	import { marqueeMode, marqueeSelect, pickMode, sameIds, type Selection } from '$lib/selection';
	import { marqueeHits, normalizeRect, type LaneBox, type SpanClip } from '$lib/marquee';
	import { moveTracks, planMove, type Ghost, type MovePlan } from '$lib/multi-move';
	import { frameTicksIn, rulerStep, tickLabel, ticksIn } from '$lib/ruler';
	import { MIN_CLIP, rippleTrimPreview, trimBounds } from '$lib/ripple-trim';
	import {
		clampZoom,
		fitZoom,
		laneWidth,
		scrollFor,
		sliderToZoom,
		wheelZoomFactor,
		zoomAround,
		zoomLabel,
		zoomToSlider
	} from '$lib/zoom';

	const pxPerSec = $derived(ui.zoom);
	const duration = $derived(Math.max(editor.duration, 8));
	const contentW = $derived(laneWidth(editor.duration, pxPerSec));
	const hasClips = $derived(editor.timeline.tracks.some((t) => t.clips.length > 0));

	function fmt(s: number): string {
		const m = Math.floor(s / 60);
		const sec = Math.floor(s % 60);
		return `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}`;
	}

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
	 *  whose clips carry sound. A track with no audio at all gets no fader: a
	 *  mixer strip on a silent track is furniture, not a control. */
	function hasSound(t: Track): boolean {
		if (t.kind === 'audio') return true;
		return t.clips.some((c) => audibleAssets.has(c.asset_id));
	}

	/** Shared look for the one-letter S / L track flags. */
	const flagBtn = (on: boolean, accent: string) =>
		`flex:none;font-family:var(--font-mono);font-size:12px;font-weight:600;line-height:1;min-width:26px;min-height:26px;padding:3px;border-radius:3px;cursor:pointer;` +
		`border:var(--line-width) solid ${on ? accent : 'var(--border-strong)'};background:${on ? accent : 'transparent'};color:${on ? 'var(--text-on-accent)' : 'var(--text-disabled)'}`;

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

	function syncView() {
		const v = viewport();
		if (!v) return;
		scrollX = v.el.scrollLeft;
		viewW = v.viewW;
		dpr = window.devicePixelRatio || 1;
	}

	$effect(() => {
		const el = scroller;
		if (!el) return;
		syncView();
		const ro = new ResizeObserver(syncView);
		ro.observe(el);
		return () => ro.disconnect();
	});

	// Two primitives rather than one object: a scroll tick that does not cross a
	// quantum changes neither, so no clip's waveform is touched by it.
	const viewLo = $derived(visibleLaneRange(scrollX, viewW).lo);
	const viewHi = $derived(visibleLaneRange(scrollX, viewW).hi);

	// ---- ruler ---------------------------------------------------------------
	//
	// The label step follows the zoom, and only the ticks in (or near) the visible
	// part of the lane exist: at the deep end of the zoom range an hour of cut is
	// far too many to draw, and at the shallow end a fixed step is all gap.

	const tickStep = $derived(rulerStep(pxPerSec));
	const visibleSec = $derived([viewLo / pxPerSec, Math.min(viewHi, contentW) / pxPerSec] as const);
	const ticks = $derived(ticksIn(visibleSec[0], visibleSec[1], tickStep));
	/** A mark per frame, once a frame is wide enough to tell apart. */
	const frameTicks = $derived(frameTicksIn(visibleSec[0], visibleSec[1], editor.fps, pxPerSec));

	/** A canvas cannot read `var(--waveform)`, so the tokens are resolved here, once
	 *  per theme change: every theme edit replaces `settings.theme`, and does so
	 *  right after `applyTheme` has written the tokens this reads back. */
	const palette = $derived.by(() => {
		void settings.theme;
		return readPalette();
	});

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
		contextMenu.show(new MouseEvent('contextmenu', { clientX: r.left, clientY: r.bottom + 2 }), heightItems(t));
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
		start: number; // the grabbed clip's ghost start (seconds)
		trackId: string; // the grabbed clip's ghost destination track
		/** Where the whole group would land, and whether it may. */
		plan: MovePlan | null;
		/** The pointer, in lane space — where the refusal's reason is written. */
		nx: number;
		ny: number;
		downX: number; // where the pointer went down, px: "moved" is judged on this
		moved: boolean;
	};
	let drag = $state<Drag | null>(null);

	/** The ghosts of the drag in progress, by the track each lands on. */
	const ghostsByTrack = $derived.by(() => {
		const m = new Map<string, Ghost[]>();
		for (const g of drag?.moved ? (drag.plan?.ghosts ?? []) : []) m.set(g.trackId, [...(m.get(g.trackId) ?? []), g]);
		return m;
	});
	const dropRefused = $derived(!!drag?.moved && !!drag.plan && !drag.plan.ok);

	/** Pointer travel (px) before a press on a clip or an edge is a drag. Judged on
	 *  the pointer itself, never on the quantized position: at high zoom one pixel
	 *  is under a frame, so a 1 px jitter of a click can round to the next frame. */
	const DRAG_SLOP = 3;

	// ---- edge-dragging trim ---------------------------------------------------

	type TrimDrag = {
		clipId: string;
		trackId: string;
		edge: 'l' | 'r';
		min: number; // dragged-edge bounds, timeline seconds: stopped by the neighbours...
		max: number;
		rMin: number; // ...and with ripple on, which pushes the neighbours along instead
		rMax: number;
		origStart: number;
		origEnd: number;
		pos: number; // current ghost position of the dragged edge
		downX: number; // where the pointer went down, px
		grab: number; // pointer's offset from the edge it grabbed (seconds): the edge follows the pointer, not jumps to it
		moved: boolean;
	};
	let trimDrag = $state<TrimDrag | null>(null);

	/** The bounds in force: ripple lifts the neighbour clamps (it pushes them along),
	 *  and the mode is the project's, so it is asked again at every move and at the
	 *  release rather than trusted from the press. */
	const trimLimits = (d: TrimDrag) => (editor.rippleMode ? { min: d.rMin, max: d.rMax } : { min: d.min, max: d.max });

	/** What the trim in progress leaves on its track under ripple: the clip and every
	 *  clip it moves, where each lands (`ripple-trim.ts`, over the faithful port of
	 *  the backend's `ripple_from`) — not the clip with its edge dragged, because a
	 *  left-edge trim keeps the clip's start. Null without ripple or a moving drag. */
	const trimPreview = $derived.by(() => {
		const d = trimDrag;
		if (!d?.moved || !editor.rippleMode) return null;
		const track = editor.timeline.tracks.find((t) => t.id === d.trackId);
		return track ? rippleTrimPreview(track, d.clipId, d.edge, d.pos) : null;
	});

	function onEdgePointerDown(e: PointerEvent, c: Clip, t: Track, edge: 'l' | 'r') {
		if (e.button !== 0 || ui.tool !== 'pointer') return; // the other tools own the clip body
		if (t.locked) return;
		e.stopPropagation();
		editor.selectClip(c.id);
		void editor.select(c.asset_id);
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
		const strict = trimBounds(c, edge, clips, asset?.duration, still, false);
		const loose = trimBounds(c, edge, clips, asset?.duration, still, true);
		trimDrag = {
			clipId: c.id,
			trackId: t.id,
			edge,
			min: strict.min,
			max: strict.max,
			rMin: loose.min,
			rMax: loose.max,
			origStart: start,
			origEnd: end,
			pos: edge === 'l' ? start : end,
			downX: e.clientX,
			grab: laneTime(e.clientX, laneLeft) - (edge === 'l' ? start : end),
			moved: false
		};
		capturePointer(e);
	}

	function onTrimMove(e: PointerEvent) {
		if (!trimDrag) return;
		const lane = document.querySelector(`[data-lane][data-track-id="${trimDrag.trackId}"]`) as HTMLElement | null;
		const laneLeft = lane?.getBoundingClientRect().left ?? 0;
		// Where the pointer says the edge is (it keeps the offset it grabbed it at),
		// rounded once: that is the ghost, and the commit.
		const raw = laneTime(e.clientX, laneLeft) - trimDrag.grab;
		const { min, max } = trimLimits(trimDrag);
		const pos = clampEdge(snapPoint(raw, trimDrag.trackId, trimDrag.clipId), min, max);
		const moved = trimDrag.moved || Math.abs(e.clientX - trimDrag.downX) >= DRAG_SLOP;
		trimDrag = { ...trimDrag, pos, moved };
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
		// Under ripple the neighbours are pushed along — unless the backend would
		// decline to (it hands the edit back as made), which would leave this clip
		// over its neighbour: the ghost was red, and letting go changes nothing.
		if (editor.rippleMode && !rippleTrimPreview(track, d.clipId, d.edge, pos)?.ok) return;
		// `pos` is the gesture's one rounded position (the ghost drew it too); every
		// field the trim writes comes from it.
		const e = trimEdit(clip, d.edge, pos);
		void editor.trim(d.clipId, e.source_in, e.source_out, e.timeline_start).catch(err);
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
	 *  playhead, a beat, a clip edge — when snapping is on, otherwise the nearest
	 *  frame. Frames are not a magnet: they apply with snapping off too. A landing
	 *  within a hair of a neighbour's edge is that edge exactly, magnet or not. */
	function snapPoint(time: number, trackId: string, except: string | ReadonlySet<string>): number {
		const edges = clipEdges(trackId, except);
		return quantizeTime(time, {
			fps,
			magnets: ui.snap ? [0, ui.time, ...beatTimes, ...edges] : [],
			threshold: 8 / pxPerSec,
			welds: edges
		});
	}

	const laneTime = (clientX: number, laneLeft: number) => (clientX - laneLeft) / pxPerSec;

	function err(e: unknown) {
		toast.error(e instanceof Error ? e.message : String(e));
	}

	function onClipPointerDown(e: PointerEvent, c: Clip, t: Track) {
		if (e.button !== 0) return;
		const lane = (e.currentTarget as HTMLElement).closest('[data-lane]') as HTMLElement | null;
		const laneLeft = lane?.getBoundingClientRect().left ?? 0;
		const mode = pickMode(e);
		if (t.locked) {
			// Still selectable and seekable — locking guards the edit, not the view.
			e.stopPropagation();
			editor.selectClip(c.id, mode);
			void editor.select(c.asset_id);
			ui.seek(laneTime(e.clientX, laneLeft));
			return;
		}
		if (ui.tool === 'razor') {
			e.stopPropagation();
			// Snapped like any other placement, then held half a frame inside the clip.
			const at = splitPoint(
				snapPoint(laneTime(e.clientX, laneLeft), t.id, ''),
				c.timeline_start,
				c.timeline_start + clipDuration(c),
				fps
			);
			if (at === null) toast.error('That clip is too short to cut on a frame');
			else void editor.split(c.id, at).catch(err);
			return;
		}
		if (ui.tool === 'roll' || ui.tool === 'slip' || ui.tool === 'slide') {
			// These act on one clip (or the cut beside it), so the press selects it, as
			// a plain press does; a modifier press is a selection gesture and nothing more.
			editor.selectClip(c.id, mode);
			void editor.select(c.asset_id);
			if (mode !== 'replace') {
				e.stopPropagation();
				return;
			}
			beginTrimTool(e, c, t, laneLeft);
			return;
		}
		// Pressing a clip that is one of several selected keeps them all: it is the
		// start of a group drag. (A click that never becomes one narrows the
		// selection to the clip when the button comes up.)
		const group = mode === 'replace' && editor.isSelected(c.id) && editor.selectedClipIds.length > 1;
		if (group) editor.setPrimary(c.id);
		else editor.selectClip(c.id, mode);
		void editor.select(c.asset_id);
		// A modifier click is a selection gesture, not the start of a drag.
		if (mode !== 'replace') {
			e.stopPropagation();
			return;
		}
		drag = {
			clipId: c.id,
			kind: t.kind,
			origTrackId: t.id,
			origStart: c.timeline_start,
			grabSec: laneTime(e.clientX, laneLeft) - c.timeline_start,
			dur: clipDuration(c),
			members: new Set(group ? editor.selectedClipIds : [c.id]),
			group,
			start: c.timeline_start,
			trackId: t.id,
			plan: null,
			nx: 0,
			ny: 0,
			downX: e.clientX,
			moved: false
		};
		capturePointer(e);
	}

	function laneUnder(clientX: number, clientY: number): HTMLElement | null {
		for (const el of document.elementsFromPoint(clientX, clientY)) {
			if (el instanceof HTMLElement && el.dataset.lane !== undefined) return el;
		}
		return null;
	}

	/** Where a clip of `dur` seconds placed near `start` lands: a magnet — 0, the
	 *  playhead, a beat, a clip edge, either of its own edges against any of them —
	 *  when snapping is on, otherwise the nearest frame for its start. */
	function snapStart(start: number, trackId: string, except: string | ReadonlySet<string>, dur: number): number {
		const edges = clipEdges(trackId, except);
		const magnets: number[] = [];
		if (ui.snap) {
			magnets.push(0, ui.time);
			for (const b of beatTimes) magnets.push(b, b - dur); // land either edge on a beat
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
		drag = null;
		trimDrag = null;
		titleDrag = null;
		scrubbing = false;
		markDrag = null;
		markerDrag = null;
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
		resetDragState();
	}

	function onPointerMove(e: PointerEvent) {
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
			const cur = document.querySelector(`[data-lane][data-track-id="${trackId}"]`) as HTMLElement | null;
			laneLeft = cur?.getBoundingClientRect().left ?? 0;
		}
		// The grabbed clip is snapped and frame-quantized exactly as a lone clip is
		// (its group's own edges are no magnet: they move with it); the rest of the
		// group keeps its offsets from it, and the plan says whether that all fits.
		const start = snapStart(laneTime(e.clientX, laneLeft) - drag.grabSec, trackId, drag.members, drag.dur);
		const movedEnough =
			drag.moved || trackId !== drag.origTrackId || Math.abs(e.clientX - drag.downX) >= DRAG_SLOP;
		const plan = movedEnough ? planMove(moveTracks(editor.timeline), drag.members, drag.clipId, start, trackId) : null;
		const box = lanesEl?.getBoundingClientRect();
		drag = {
			...drag,
			start: movedEnough ? start : drag.start,
			trackId,
			moved: movedEnough,
			plan,
			nx: box ? e.clientX - box.left : 0,
			ny: box ? e.clientY - box.top : 0
		};
	}

	function onPointerUp() {
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
			// A click on one clip of a selection narrows it to that clip.
			if (d.group) editor.selectClip(d.clipId);
			ui.seek(d.origStart + d.grabSec); // a plain click on the clip seeks there
			return;
		}
		// A refused drop — an overlap, before 0, a locked or missing lane — was red
		// all the way; letting go changes nothing. A drop that moves nothing too.
		const plan = d.plan;
		if (!plan || !plan.ok || plan.noop) return;
		// The group lands as ONE edit: one revision, one undo.
		void editor.moveClips(plan.moves).catch(err);
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
		/** Where the press was on the time axis — a click that never drags seeks there. */
		pressTime: number;
		downX: number;
		/** The pressed clip's length and speed. */
		dur: number;
		speed: number;
		/** What the pointer asks for (rounded once, not yet held to the range). */
		requested: number;
		preview: EditPreview | null;
		moved: boolean;
		/** The pointer in lane space — where the readout is written. */
		nx: number;
		ny: number;
	};
	let tg = $state<TrimGesture | null>(null);
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

	/** How far each asset's footage reaches: what every edit mode clamps to. */
	const footage = $derived(sourceLimits(editor.assets));

	/** The cut under the roll tool's pointer, so it can be lit and the cursor changed
	 *  before anything is pressed. */
	let rollHover = $state<{ trackId: string; time: number } | null>(null);
	$effect(() => {
		if (ui.tool !== 'roll') rollHover = null;
	});

	function onLaneHover(e: PointerEvent, t: Track) {
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
			dur: clipDuration(c),
			speed: c.speed ?? 1,
			requested: 0,
			preview: null,
			moved: false,
			nx: 0,
			ny: 0
		};
		cancelTrimTool = beginDrag(e, { move: onTrimToolMove, commit: onTrimToolUp, abandon: endTrimTool });
	}

	function onTrimToolMove(e: PointerEvent) {
		const g = tg;
		if (!g) return;
		const moved = g.moved || Math.abs(e.clientX - g.downX) >= DRAG_SLOP;
		const track = editor.timeline.tracks.find((t) => t.id === g.trackId);
		if (!moved || !g.edit || !track) {
			tg = { ...g, moved };
			return;
		}
		const lane = document.querySelector(`[data-lane][data-track-id="${g.trackId}"]`) as HTMLElement | null;
		const here = laneTime(e.clientX, lane?.getBoundingClientRect().left ?? 0);
		// One rounding, from where the pointer is — never from the last frame's. A magnet
		// within reach (the playhead, a beat, an edge not part of the edit) still wins.
		let requested: number;
		if (g.edit.tool === 'roll') {
			requested = snapPoint(here - g.grab, g.trackId, new Set([g.edit.a, g.edit.b])) - g.origin;
		} else if (g.edit.tool === 'slide') {
			requested = snapStart(here - g.grab, g.trackId, slideMembers(track, g.edit.clipId), g.dur) - g.origin;
		} else {
			requested = slipDelta((e.clientX - g.downX) / pxPerSec, { speed: g.speed }, fps);
		}
		const preview = previewEdit(editor.timeline, g.edit, requested, footage);
		const box = lanesEl?.getBoundingClientRect();
		tg = { ...g, moved, requested, preview, nx: box ? e.clientX - box.left : 0, ny: box ? e.clientY - box.top : 0 };
		ui.trimMonitor = monitorFor(preview, fps, track.kind);
	}

	function onTrimToolUp() {
		const g = tg;
		endTrimTool();
		if (!g) return;
		// A press that never became a drag is a click: the clip was selected at the press,
		// and the playhead goes where it was pressed, as it does under Select.
		if (!g.moved || !g.edit) {
			ui.seek(g.pressTime);
			return;
		}
		// Asked again: an agent's edit or a flipped mode may have changed what is legal
		// since the last move, and what is written is what the backend will take now.
		const p = previewEdit(editor.timeline, g.edit, g.requested, footage);
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
		const run =
			edit.tool === 'roll'
				? editor.roll(edit.a, edit.b, p.applied)
				: edit.tool === 'slip'
					? editor.slip(edit.clipId, p.applied)
					: editor.slide(edit.clipId, p.applied);
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

	function onCanvasPointerDown(e: PointerEvent, from: 'lane' | 'canvas') {
		swallowClick = false;
		// Only a press on the empty surface itself: a clip, a title, the ruler, a
		// marker all have their own gestures and are children of what this is on.
		if (e.button !== 0 || ui.tool !== 'pointer' || e.target !== e.currentTarget) return;
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
		const hits = marqueeHits({ x0: m.ax, y0: m.ay, x1: x, y1: y }, laneBoxes(), trackSpans(), pxPerSec);
		const sel = marqueeSelect(m.base, hits, m.mode);
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

	function onLaneSeek(e: MouseEvent) {
		if (swallowClick) {
			swallowClick = false;
			return;
		}
		const x = e.clientX - (e.currentTarget as HTMLElement).getBoundingClientRect().left;
		ui.seek(x / pxPerSec);
		// Empty lane space is the only way to deselect; clips stopPropagation.
		if (!e.shiftKey && !e.ctrlKey && !e.metaKey) editor.clearSelection();
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
		if (!a || a.kind !== t.kind || t.locked) {
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

	// ---- fades (toggle a default 0.5s fade on the selected clip) --------------

	const FADE_DEFAULT = 0.5;
	function toggleFadeIn() {
		const c = editor.selectedClip;
		if (c) void editor.setFade(c.id, c.fade_in > 0 ? 0 : FADE_DEFAULT);
	}
	function toggleFadeOut() {
		const c = editor.selectedClip;
		if (c) void editor.setFade(c.id, undefined, c.fade_out > 0 ? 0 : FADE_DEFAULT);
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

	function trackItems(t: Track): MenuItem[] {
		return [
			{ label: 'Add video track', icon: 'video', action: () => onAddTrack('video') },
			{ label: 'Add audio track', icon: 'audio-waveform', action: () => onAddTrack('audio') },
			{ type: 'separator' },
			{
				label: t.muted ? (t.kind === 'video' ? 'Show track' : 'Unmute track') : t.kind === 'video' ? 'Hide track' : 'Mute track',
				icon: t.muted ? 'eye' : 'eye-off',
				action: () => void editor.setTrackMuted(t.id, !t.muted).catch(err)
			},
			{
				label: t.solo ? 'Clear solo' : 'Solo track',
				action: () => void editor.setTrackSolo(t.id, !t.solo).catch(err)
			},
			{
				label: t.locked ? 'Unlock track' : 'Lock track',
				icon: 'lock',
				action: () => void editor.setTrackLocked(t.id, !t.locked).catch(err)
			},
			{ type: 'separator' },
			...heightItems(t),
			{ type: 'separator' },
			{ label: `Remove track ${t.name}`, icon: 'trash', danger: true, action: () => onRemoveTrack(t) }
		];
	}

	function onClipContextMenu(e: MouseEvent, c: Clip, t: Track) {
		// Right-clicking inside a multi-selection keeps it, so the menu can act on all.
		if (!editor.isSelected(c.id)) editor.selectClip(c.id);
		void editor.select(c.asset_id);
		const within = ui.time > c.timeline_start && ui.time < c.timeline_start + clipDuration(c);
		const enabled = c.enabled !== false;
		const n = editor.isSelected(c.id) ? editor.selectedClipIds.length : 1;
		contextMenu.show(e, [
			{
				label: 'Split at playhead',
				icon: 'Scissors',
				shortcut: settings.shortcut('tool.razor'),
				disabled: !within || !!t.locked,
				action: () => {
					// The playhead can be anywhere between frames; the cut is on one.
					const at = splitPoint(quantizeTime(ui.time, { fps }), c.timeline_start, c.timeline_start + clipDuration(c), fps);
					if (at === null) toast.error('That clip is too short to cut on a frame');
					else void editor.split(c.id, at).catch(err);
				}
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
			},
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
			{
				label: n > 1 ? `Duplicate ${n} clips` : 'Duplicate',
				shortcut: settings.shortcut('edit.duplicate'),
				action: () =>
					void editor
						.duplicateSelection()
						.then((k) => k && toast(k === 1 ? 'Clip duplicated' : `${k} clips duplicated`))
						.catch(err)
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
			{
				label: n > 1 ? `Remove ${n} clips` : 'Remove',
				icon: 'trash',
				shortcut: settings.shortcut('edit.delete'),
				danger: true,
				action: () => removeClip(c.id, false)
			},
			{
				label: n > 1 ? `Ripple delete ${n} clips` : 'Ripple delete',
				icon: 'trash',
				shortcut: settings.shortcut('edit.rippleDelete'),
				danger: true,
				action: () => removeClip(c.id, true)
			}
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

	// Keep the playhead on screen while it moves — during playback it would
	// otherwise walk straight off the right edge. Only runs when the playhead
	// moves, so scrolling away by hand while parked isn't fought.
	$effect(() => {
		const laneX = ui.time * pxPerSec;
		const v = viewport();
		if (!v) return;
		const margin = Math.min(80, v.viewW * 0.15);
		if (laneX < v.el.scrollLeft + margin || laneX > v.el.scrollLeft + v.viewW - margin) {
			v.el.scrollLeft = Math.max(0, laneX - v.viewW * 0.25);
		}
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
		if (next === ui.zoom) {
			v.el.scrollLeft = 0;
			return;
		}
		pendingScroll = 0;
		ui.zoom = next;
	}

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
	const titleRows = $derived(packRows(editor.overlays));
	const titleLaneH = $derived(
		titleLaneHeight(ui.heights.all, Math.max(1, ...[...titleRows.values()].map((r) => r + 1)))
	);

	type TitleDrag = {
		id: string;
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

	/** Where a title's edges snap to: 0, the playhead, beats, every clip edge and the other titles'. */
	function titleSnapPoints(id: string): number[] {
		const pts = [0, ui.time, ...beatTimes];
		for (const t of editor.timeline.tracks)
			for (const c of t.clips) pts.push(c.timeline_start, c.timeline_start + clipDuration(c));
		for (const o of editor.overlays) if (o.id !== id) pts.push(o.start, o.end);
		return pts;
	}

	function onTitlePointerDown(e: PointerEvent, o: TextOverlay, mode: 'move' | 'l' | 'r') {
		if (e.button !== 0) return;
		e.stopPropagation();
		editor.selectOverlay(o.id);
		const left = titleLaneEl?.getBoundingClientRect().left ?? 0;
		titleDrag = {
			id: o.id,
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

	function onTitleDragMove(e: PointerEvent) {
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
		if (!d.moved) {
			if (d.mode === 'move') ui.seek(d.origStart + d.grabSec); // a plain click seeks there, into the title
			return;
		}
		void editor.retimeOverlay(d.id, d.start, d.end).catch(err);
	}

	const addTitleHere = () => void editor.addTitle('Title', ui.time, ui.time + 3).catch(err);

	function onTitleContextMenu(e: MouseEvent, o: TextOverlay) {
		e.stopPropagation();
		editor.selectOverlay(o.id);
		contextMenu.show(e, [
			{ label: 'Go to start', icon: 'skip-back', action: () => ui.seek(o.start) },
			{ type: 'separator' },
			{
				label: o.generated ? 'Remove caption' : 'Remove title',
				icon: 'trash',
				danger: true,
				shortcut: settings.shortcut('edit.delete'),
				action: () => void editor.removeOverlay(o.id).catch(err)
			}
		]);
	}

	function onTitleLaneContextMenu(e: MouseEvent) {
		e.stopPropagation();
		// Importing captions: timed to the cut, or to one of the clips it shows. The
		// controls in Titles carry the same import with every clip to choose from.
		const { entries, more } = importMenuEntries(importableAssets(editor.timeline, editor.assets));
		contextMenu.show(e, [
			{ label: 'Add title at playhead', icon: 'captions', action: addTitleHere },
			{ type: 'separator' },
			...entries.map(
				(m): MenuItem => ({
					label: m.label,
					icon: 'file-text',
					disabled: editor.busy,
					action: () => void importCaptionFile(m.choice)
				})
			),
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

	let scrubbing = false;
	let markDrag: 'in' | 'out' | null = null;

	const rulerTime = (clientX: number) =>
		Math.max(0, (clientX - (rulerEl?.getBoundingClientRect().left ?? 0)) / pxPerSec);

	function onRulerPointerDown(e: PointerEvent) {
		if (e.button !== 0) return;
		scrubbing = true;
		capturePointer(e);
		ui.seek(rulerTime(e.clientX));
	}

	function onMarkPointerDown(e: PointerEvent, which: 'in' | 'out') {
		if (e.button !== 0) return;
		e.stopPropagation();
		markDrag = which;
		capturePointer(e);
	}

	// ---- markers --------------------------------------------------------------

	/** Marker being dragged along the ruler, and the id being renamed inline. */
	let markerDrag: { id: string; time: number } | null = $state(null);
	let renaming = $state<string | null>(null);

	const addMarkerHere = () => void editor.addMarkerAtPlayhead(ui.time).catch(err);

	function onMarkerPointerDown(e: PointerEvent, m: Marker) {
		if (e.button !== 0 || renaming === m.id) return;
		e.stopPropagation();
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
			{ label: 'Rename', action: () => (renaming = m.id) },
			{ type: 'separator' },
			{
				label: 'Remove marker',
				icon: 'trash',
				danger: true,
				action: () => void editor.removeMarker(m.id).catch(err)
			}
		]);
	}

	// Right-click on empty timeline canvas (ruler / grid / below the tracks). Clip
	// and lane menus stopPropagation, so only the bare background reaches this.
	function onTimelineContextMenu(e: MouseEvent) {
		contextMenu.show(e, [
			{ label: 'Add video track', icon: 'video', action: () => onAddTrack('video') },
			{ label: 'Add audio track', icon: 'audio-waveform', action: () => onAddTrack('audio') },
			{ type: 'separator' },
			{ type: 'separator' },
			{
				label: editor.clipboard.length > 1 ? `Paste ${editor.clipboard.length} clips` : 'Paste',
				icon: 'copy',
				shortcut: settings.shortcut('edit.paste'),
				disabled: editor.clipboard.length === 0,
				action: () =>
					void editor
						.paste(ui.time)
						.then((k) => k && toast(k === 1 ? 'Clip pasted' : `${k} clips pasted`))
						.catch(err)
			},
			{ type: 'separator' },
			{
				label: 'Add marker at playhead',
				icon: 'bookmark',
				shortcut: settings.shortcut('marker.add'),
				action: addMarkerHere
			},
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

<svelte:window
	onpointermove={onPointerMove}
	onpointerup={onPointerUp}
	onpointercancel={onPointerCancel}
	onblur={onWindowBlur}
	onresize={syncView}
	onkeydowncapture={(e) => {
		// Escape gives a clip, edge, title or marquee drag up without writing
		// anything — and that is all it does: the page's Escape (clear the
		// selection) must not also fire.
		if (e.key === 'Escape' && (drag || trimDrag || titleDrag || marquee)) {
			resetDragState();
			e.stopPropagation();
		}
	}}
/>

<div
	style="flex:1;min-height:0;background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden;position:relative"
>

	<!-- timeline toolbar -->
	<div
		style="height:34px;display:flex;align-items:center;gap:8px;padding:0 12px;border-bottom:var(--line-width) solid var(--border-subtle);flex:none"
	>
		<span
			style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
			>Timeline</span
		>
		{#if editor.busy}<Badge tone="agent" dot>working…</Badge>{/if}
		<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">{fmt(duration)}</span>
		{#if editor.selectedClips.length > 1}
			<span
				title="Drag any of them to move them all{settings.shortcut('edit.delete') ? `; ${settings.shortcut('edit.delete')} removes them all` : ''}"
				style="font-size:10px;color:var(--kerf-300)">{editor.selectedClips.length} selected</span
			>
		{/if}
		{#if editor.selectedClip}
			{@const sc = editor.selectedClip}
			<span style="width:1px;height:16px;background:var(--border-strong);margin:0 4px"></span>
			<span
				style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
				>Fade</span
			>
			<button
				title="Toggle a {FADE_DEFAULT}s fade-in on the selected clip"
				onclick={toggleFadeIn}
				style="font-size:10px;padding:2px 7px;border-radius:4px;cursor:pointer;border:var(--line-width) solid var(--border-strong);background:{sc.fade_in >
				0
					? 'var(--surface-hover)'
					: 'transparent'};color:{sc.fade_in > 0 ? 'var(--kerf-300)' : 'var(--text-muted)'}">in</button
			>
			<button
				title="Toggle a {FADE_DEFAULT}s fade-out on the selected clip"
				onclick={toggleFadeOut}
				style="font-size:10px;padding:2px 7px;border-radius:4px;cursor:pointer;border:var(--line-width) solid var(--border-strong);background:{sc.fade_out >
				0
					? 'var(--surface-hover)'
					: 'transparent'};color:{sc.fade_out > 0 ? 'var(--kerf-300)' : 'var(--text-muted)'}">out</button
			>
		{/if}
		<div style="flex:1"></div>
		<button
			title={settings.withShortcut('Zoom out', 'view.zoomOut')}
			aria-label="Zoom out"
			onclick={() => ui.zoomBy(-1)}
			style="background:none;border:none;cursor:pointer;color:var(--text-muted);display:grid;place-items:center"
			><Icon n="zoom-out" s={14} /></button
		>
		<input
			type="range"
			min="0"
			max="1"
			step="0.001"
			value={zoomToSlider(ui.zoom)}
			oninput={(e) => (ui.zoom = clampZoom(sliderToZoom(+e.currentTarget.value), editor.duration))}
			title="Zoom — {zoomLabel(ui.zoom)} (⌘/Ctrl + wheel zooms at the cursor)"
			aria-label="Timeline zoom"
			style="width:90px;height:24px;"
		/>
		<button
			title={settings.withShortcut('Zoom in', 'view.zoomIn')}
			aria-label="Zoom in"
			onclick={() => ui.zoomBy(1)}
			style="background:none;border:none;cursor:pointer;color:var(--text-muted);display:grid;place-items:center"
			><Icon n="zoom-in" s={14} /></button
		>
		<button
			title={settings.withShortcut('Zoom to fit the whole cut', 'view.zoomFit')}
			aria-label="Zoom to fit"
			disabled={!hasClips}
			onclick={() => ui.zoomToFit()}
			style="background:none;border:none;cursor:{hasClips ? 'pointer' : 'default'};color:var(--text-muted);opacity:{hasClips ? 1 : 0.4};display:grid;place-items:center"
			><Icon n="fold-horizontal" s={14} /></button
		>
		<button
			title={ui.snap ? 'Snapping on — click to disable' : 'Snapping off — click to enable'}
			aria-pressed={ui.snap}
			onclick={() => (ui.snap = !ui.snap)}
			style="font-family:var(--font-mono);font-size:10px;padding:2px 7px;border-radius:4px;cursor:pointer;border:var(--line-width) solid var(--border-strong);background:{ui.snap
				? 'var(--surface-hover)'
				: 'transparent'};color:{ui.snap ? 'var(--kerf-300)' : 'var(--text-disabled)'}"
			>{ui.snap ? 'snap on' : 'snap off'}</button
		>
		<!-- Ripple mode is the project's, not this panel's: it changes what a trim, a
		     delete and a speed change do everywhere (and an agent can flip it). So it
		     is lit while on, with a second cue in the ruler corner. -->
		<button
			title={editor.rippleMode
				? `${settings.withShortcut('Ripple on', 'tool.rippleMode')} — a trim, delete or speed change pulls the later clips on that track along, keeping their gaps. Each track ripples on its own: there is no sync lock yet, so linked audio and video do not move together.`
				: `${settings.withShortcut('Ripple off', 'tool.rippleMode')} — edits leave a gap. Turn on to have a trim, delete or speed change pull the later clips on that track along. Each track ripples on its own (no sync lock yet).`}
			aria-pressed={editor.rippleMode}
			onclick={() => void editor.setRippleMode(!editor.rippleMode).catch(err)}
			style="display:inline-flex;align-items:center;gap:5px;font-size:10px;padding:2px 7px;border-radius:4px;cursor:pointer;border:var(--line-width) solid {editor.rippleMode
				? 'var(--kerf-400)'
				: 'var(--border-strong)'};background:{editor.rippleMode
				? 'var(--selection-fill)'
				: 'transparent'};color:{editor.rippleMode ? 'var(--kerf-300)' : 'var(--text-disabled)'}"
			><Icon n="between-horizontal-start" s={12} />Ripple</button
		>
		<span style="width:1px;height:16px;background:var(--border-strong);margin:0 4px"></span>
		<!-- Track height, every track at once (each track's own name button sets just
		     that one). Lit when every track is at that height. -->
		<div role="group" aria-label="Track height, all tracks" style="display:inline-flex;align-items:center;gap:2px">
			{#each HEIGHT_PRESETS as p (p)}
				<button
					title="All tracks {PRESET_LABEL[p].toLowerCase()} — {PRESET_PX[p]} px"
					aria-label="All tracks {PRESET_LABEL[p].toLowerCase()}"
					aria-pressed={allHeight === p}
					onclick={() => ui.setAllHeights(p)}
					style="display:grid;place-items:center;min-width:24px;min-height:24px;padding:0;border-radius:4px;cursor:pointer;border:var(--line-width) solid {allHeight ===
					p
						? 'var(--kerf-400)'
						: 'var(--border-strong)'};background:{allHeight === p ? 'var(--selection-fill)' : 'transparent'};color:{allHeight ===
					p
						? 'var(--kerf-300)'
						: 'var(--text-disabled)'}"><HeightGlyph preset={p} /></button
				>
			{/each}
		</div>
		<button
			title={ui.minimap ? 'Hide the overview strip' : 'Show the overview strip of the whole cut'}
			aria-label="Toggle overview"
			aria-pressed={ui.minimap}
			onclick={() => ui.toggleMinimap()}
			style="display:grid;place-items:center;min-width:24px;min-height:24px;padding:0;border-radius:4px;cursor:pointer;border:var(--line-width) solid {ui.minimap
				? 'var(--kerf-400)'
				: 'var(--border-strong)'};background:{ui.minimap ? 'var(--selection-fill)' : 'transparent'};color:{ui.minimap
				? 'var(--kerf-300)'
				: 'var(--text-disabled)'}"><Icon n="chart-no-axes-gantt" s={13} /></button
		>
		<span style="width:1px;height:16px;background:var(--border-strong);margin:0 4px"></span>
		<button
			title="Add a video track"
			onclick={() => onAddTrack('video')}
			style="font-size:10px;padding:2px 7px;border-radius:4px;cursor:pointer;border:var(--line-width) solid var(--border-strong);background:transparent;color:var(--text-muted)"
			>+ V</button
		>
		<button
			title="Add an audio track"
			onclick={() => onAddTrack('audio')}
			style="font-size:10px;padding:2px 7px;border-radius:4px;cursor:pointer;border:var(--line-width) solid var(--border-strong);background:transparent;color:var(--text-muted)"
			>+ A</button
		>
	</div>

	<!-- The whole cut on one strip: where the view is, and a way to move it. A
	     pointer overview, hidden from assistive tech on purpose: everything it does
	     is already reachable from the keyboard (the scroller itself, the zoom
	     buttons and the zoom / fit / shuttle keys, and the playhead's follow) — a
	     second focusable widget with partial parity would be more to learn than
	     to use. Its toggle in the toolbar stays labelled. -->
	{#if ui.minimap && hasClips}
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
				tracks={editor.timeline.tracks}
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
	     tracks kept going. -->
	<div
		bind:this={scroller}
		onwheel={onWheel}
		onscroll={() => {
			syncView();
			// A scroll moves the lanes under a pointer that has not moved.
			if (marquee) onMarqueeMove(marquee.cx, marquee.cy);
		}}
		style="flex:1;min-height:0;overflow:auto;position:relative;display:flex;align-items:flex-start"
	>
		<!-- track headers -->
		<div
			bind:this={headersEl}
			style="width:var(--track-header-w);flex:none;min-height:100%;position:sticky;left:0;z-index:40;border-right:var(--line-width) solid var(--border-default);background:var(--surface-app)"
		>
			<div
				style="height:var(--ruler-h);border-bottom:var(--line-width) solid {editor.rippleMode
					? 'var(--kerf-500)'
					: 'var(--border-subtle)'};position:sticky;top:0;z-index:50;background:var(--surface-app);display:flex;align-items:center;padding:0 8px"
			>
				{#if editor.rippleMode}
					<span
						title="Ripple mode is on — edits pull the later clips on their track along"
						style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--kerf-300)"
						>Ripple</span
					>
				{/if}
			</div>
			<div
				style="height:{titleLaneH}px;border-bottom:1px solid var(--border-subtle);display:flex;align-items:center;gap:6px;padding:0 8px;overflow:hidden"
			>
				<span style="font-family:var(--font-mono);font-size:12px;font-weight:600;color:var(--text-secondary);flex:none">T</span>
				<span style="font-size:11px;color:var(--text-muted);flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
					>Titles · {editor.overlays.length}</span
				>
				<button
					title="Add a title at the playhead"
					aria-label="Add a title"
					disabled={editor.busy}
					onclick={addTitleHere}
					style="background:none;border:none;cursor:pointer;padding:0;min-width:26px;min-height:26px;flex:none;display:grid;place-items:center;color:var(--text-disabled)"
					><Icon n="plus" s={12} /></button
				>
			</div>
			{#each editor.timeline.tracks as t (t.id)}
				<div
					role="presentation"
					oncontextmenu={(e) => onTrackHeaderContextMenu(e, t)}
					style="height:{trackHeight(t)};border-bottom:var(--line-width) solid var(--border-subtle);display:flex;flex-direction:column;justify-content:center;gap:4px;padding:0 8px;overflow:hidden"
				>
				<div style="display:flex;align-items:center;gap:5px">
					<!-- The name is the track's height menu (so is the header's right-click). -->
					<button
						title="{t.name} — {PRESET_LABEL[ui.trackPreset(t.id)]} height ({ui.trackPx(t.id)} px). Click to change."
						aria-label="Track {t.name} height"
						aria-haspopup="menu"
						onclick={(e) => openHeightMenu(e, t)}
						style="flex:none;display:inline-flex;align-items:center;gap:1px;min-height:26px;padding:0;background:none;border:none;cursor:pointer;font-family:var(--font-mono);font-size:12px;font-weight:600;color:var(--text-secondary)"
						>{t.name}<Icon n="chevron-down" s={10} color="var(--text-disabled)" /></button
					>
					<!-- No "Video"/"Audio" caption: the name (V1 / A1) and the eye vs
					     speaker icon both already say the kind, and the row now carries
					     mute / solo / lock / remove in a fixed-width column. -->
					<span style="flex:1;min-width:0"></span>
					{#if t.kind === 'audio'}
						<button
							title={t.duck
								? 'Ducking on — this track dips under the rest of the mix on export'
								: 'Duck this track under the rest of the mix on export'}
							aria-label="Toggle ducking"
							onclick={() => void editor.setTrackDuck(t.id, !t.duck).catch(err)}
							style="background:{t.duck ? 'var(--kerf-500)' : 'none'};border:var(--line-width) solid {t.duck
								? 'var(--kerf-500)'
								: 'var(--border-strong)'};border-radius:3px;cursor:pointer;color:{t.duck
								? 'var(--text-on-accent)'
								: 'var(--text-disabled)'};font-size:12px;font-weight:600;min-height:26px;padding:2px 4px;flex:none"
							>DUCK</button
						>
					{/if}
					<button
						title={t.muted
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
							: 'var(--text-disabled)'}"
						><Icon n={t.kind === 'video' ? (t.muted ? 'eye-off' : 'eye') : t.muted ? 'volume-x' : 'volume-2'} s={12} /></button
					>
					<button
						title={t.solo ? 'Soloed — click to clear' : 'Solo this track'}
						aria-label="Toggle solo"
						aria-pressed={!!t.solo}
						onclick={() => void editor.setTrackSolo(t.id, !t.solo).catch(err)}
						style={flagBtn(!!t.solo, 'var(--kerf-400)')}>S</button
					>
					<button
						title={t.locked ? 'Locked — click to unlock' : 'Lock this track against edits'}
						aria-label="Toggle lock"
						aria-pressed={!!t.locked}
						onclick={() => void editor.setTrackLocked(t.id, !t.locked).catch(err)}
						style={flagBtn(!!t.locked, 'var(--kerf-300)')}>L</button
					>
					<button
						title="Remove track"
						aria-label="Remove track"
						onclick={() => onRemoveTrack(t)}
						style="background:none;border:none;cursor:pointer;color:var(--text-disabled);display:grid;place-items:center;padding:0;min-width:26px;min-height:26px;flex:none"
						><Icon n="x" s={12} /></button
					>
				</div>
					{#if hasSound(t) && ui.trackPx(t.id) >= MIXER_MIN_PX}
						<!-- The mixer strip: a fader over every clip on the track, and its
						     stereo placement. Balancing a music bed against a voice is the
						     one audio move every cut needs, and doing it clip by clip is
						     not the same control. -->
						<div style="display:flex;align-items:center;gap:6px">
							<input
								type="range"
								min="0"
								max={Math.max(MAX_GAIN, t.volume ?? 1)}
								step="0.01"
								value={t.volume ?? 1}
								disabled={editor.busy}
								aria-label="{t.name} level"
								title="Level {gainLabel(t.volume ?? 1)} — double-click for unity"
								onchange={(e) => void editor.setTrackVolume(t.id, +e.currentTarget.value).catch(err)}
								ondblclick={() => void editor.setTrackVolume(t.id, 1).catch(err)}
								style="flex:1;min-width:0;height:20px;--slider-accent:var(--kerf-400);cursor:pointer"
							/>
							<input
								type="range"
								min="-1"
								max="1"
								step="0.05"
								value={t.pan ?? 0}
								disabled={editor.busy}
								aria-label="{t.name} pan"
								title="Pan {panLabel(t.pan ?? 0)} — double-click to centre"
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
			style="width:{contentW}px;flex:none;min-height:100%;position:relative"
		>
			<!-- ruler — sticky under the playhead (z 30) but over the drag ghosts (z 25) -->
			<div
				bind:this={rulerEl}
				role="presentation"
				onpointerdown={onRulerPointerDown}
				style="height:var(--ruler-h);border-bottom:var(--line-width) solid {editor.rippleMode
					? 'var(--kerf-500)'
					: 'var(--border-subtle)'};position:sticky;top:0;z-index:26;background:var(--surface-app);cursor:ew-resize;touch-action:none"
			>
				{#each ticks as t (t.k)}
					<span
						style="position:absolute;left:{t.t * pxPerSec + 4}px;top:7px;font-family:var(--font-mono);font-size:10px;color:var(--text-disabled);pointer-events:none"
						>{tickLabel(t.k, tickStep)}</span
					>
				{/each}
				{#each frameTicks as f (f.k)}
					<span
						style="position:absolute;left:{f.t * pxPerSec}px;bottom:0;width:var(--line-width);height:4px;background:var(--text-disabled);opacity:.45;pointer-events:none"
					></span>
				{/each}
				{#each sceneXs as x (x)}
					<span
						title="Detected scene cut"
						style="position:absolute;left:{x}px;bottom:0;width:0;height:0;border-left:4px solid transparent;border-right:4px solid transparent;border-top:5px solid var(--scene-marker);transform:translateX(-50%)"
					></span>
				{/each}
				{#each beatTimes as b (b)}
					<span
						title="Beat"
						style="position:absolute;left:{b * pxPerSec}px;bottom:0;width:var(--line-width);height:5px;background:var(--beat-marker);opacity:.75;pointer-events:none"
					></span>
				{/each}
				<!-- user markers: click seeks, drag moves, double-click renames -->
				{#each editor.markers as m (m.id)}
					{@const mt = markerDrag?.id === m.id ? markerDrag.time : m.time}
					{@const accent = m.color || 'var(--agent-400)'}
					<span
						role="presentation"
						title="{m.name} — {fmt(mt)}"
						onpointerdown={(e) => onMarkerPointerDown(e, m)}
						ondblclick={() => (renaming = m.id)}
						oncontextmenu={(e) => onMarkerContextMenu(e, m)}
						style="position:absolute;left:{mt *
							pxPerSec}px;top:0;bottom:0;width:8px;z-index:29;cursor:ew-resize;touch-action:none;display:flex;align-items:center"
					>
						<span style="position:absolute;left:-1px;top:0;bottom:0;width:2px;background:{accent}"></span>
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
								style="position:relative;flex:none;margin-left:3px;width:96px;font-size:9px;padding:1px 3px;border-radius:3px;border:var(--line-width) solid {accent};background:var(--surface-inset);color:var(--text-primary)"
							/>
						{:else}
							<span
								style="position:relative;flex:none;margin-left:3px;max-width:120px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:9px;font-weight:600;line-height:1;padding:2px 4px;border-radius:3px;background:{accent};color:var(--text-on-accent);pointer-events:none"
								>{m.name}</span
							>
						{/if}
					</span>
				{/each}
				{#if ui.markIn !== null && ui.markOut !== null && ui.markOut > ui.markIn}
					<div
						style="position:absolute;left:{ui.markIn * pxPerSec}px;width:{(ui.markOut - ui.markIn) *
							pxPerSec}px;top:0;bottom:0;background:var(--selection-fill);pointer-events:none"
					></div>
				{/if}
				{#if ui.markIn !== null}
					<span
						role="presentation"
						title="Mark in {fmt(ui.markIn)} — drag to move{clearHint('range.clearIn')}"
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
						title="Mark out {fmt(ui.markOut)} — drag to move{clearHint('range.clearOut')}"
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

			{#if !hasClips}
				<div
					style="position:absolute;left:0;right:0;top:var(--ruler-h);bottom:0;display:grid;place-items:center;color:var(--text-disabled);font-size:12px"
				>
					Timeline empty — import media and queue a cut
				</div>
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
				{#if editor.overlays.length === 0}
					<span
						style="position:absolute;left:8px;top:0;bottom:0;display:flex;align-items:center;font-size:11px;color:var(--text-disabled);pointer-events:none;white-space:nowrap"
						>Titles and captions appear here</span
					>
				{/if}
				{#each editor.overlays as o (o.id)}
					{@const live = titleDrag?.moved && titleDrag.id === o.id ? titleDrag : null}
					{@const row = titleRows.get(o.id) ?? 0}
					{@const width = Math.max(6, (o.end - o.start) * pxPerSec)}
					{@const selected = editor.selectedOverlayId === o.id}
					<button
						onpointerdown={(e) => onTitlePointerDown(e, o, 'move')}
						oncontextmenu={(e) => onTitleContextMenu(e, o)}
						onclick={(e) => e.stopPropagation()}
						title="{o.generated ? 'Caption' : 'Title'}: {o.text}"
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
			</div>

			<!-- tracks -->
			{#each editor.timeline.tracks as t (t.id)}
				<div
					role="presentation"
					data-lane
					data-track-id={t.id}
					data-kind={t.kind}
					onclick={onLaneSeek}
					onpointerdown={(e) => onCanvasPointerDown(e, 'lane')}
					onpointermove={(e) => onLaneHover(e, t)}
					onpointerleave={() => (rollHover = null)}
					oncontextmenu={(e) => onLaneContextMenu(e, t)}
					ondragover={(e) => onLaneDragOver(e, t)}
					ondragleave={(e) => onLaneDragLeave(e, t)}
					ondrop={(e) => onLaneDrop(e, t)}
					style="height:{trackHeight(t)};border-bottom:var(--line-width) solid var(--border-subtle);position:relative"
				>
					{#each t.clips as c (c.id)}
						{@const left = c.timeline_start * pxPerSec}
						{@const width = Math.max(6, clipDuration(c) * pxPerSec)}
						{@const selected = editor.isSelected(c.id)}
						{@const primary = editor.selectedClipId === c.id}
						{@const sliding = !!tg?.moved && tg.edit?.tool === 'slide' && tg.edit.clipId === c.id}
						{@const dragging = (drag?.moved && drag.members.has(c.id)) || !!trimPreview?.shifted.has(c.id) || sliding}
						<!-- A slip shows the footage moving under the clip's edges as it is dragged:
						     the picture and the waveform are drawn from the window it would have. -->
						{@const shown =
							tg?.moved && tg.edit?.tool === 'slip' && tg.edit.clipId === c.id
								? (tg.preview?.ghosts.find((g) => g.id === c.id)?.clip ?? c)
								: c}
						{@const off = c.enabled === false || !renders(t)}
						<button
							class="kclip"
							onpointerdown={(e) => onClipPointerDown(e, c, t)}
							oncontextmenu={(e) => onClipContextMenu(e, c, t)}
							onclick={(e) => e.stopPropagation()}
							style="--bw:{selected ? 'var(--line-emphasis)' : 'var(--line-width)'};position:absolute;left:{left}px;top:5px;height:calc(100% - 10px);width:{width}px;border-radius:2px;overflow:hidden;display:flex;align-items:center;padding:0 7px;touch-action:none;filter:{off
								? 'grayscale(1)'
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
										title="Detected silence"
										style="position:absolute;left:{r.left - left}px;top:3px;bottom:3px;width:{Math.max(2, r.width)}px;background:var(--silence-region);border:var(--line-width) solid color-mix(in srgb,var(--red-500) 30%,transparent);border-radius:2px"
									></span>
								{/each}
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
									title="{transitionLabel(c.transition_in.kind)} {c.transition_in.duration.toFixed(2)}s"
									style="position:absolute;left:0;top:0;bottom:0;width:{Math.min(
										c.transition_in.duration * pxPerSec,
										width
									)}px;background:linear-gradient(to right, color-mix(in srgb,var(--drag-ghost) 55%,transparent), transparent);border-left:2px solid var(--kerf-400);pointer-events:none"
								></span>
							{/if}
							<!-- An audio clip's name sits at its foot: the middle belongs to the
							     waveform (and the line between a stereo clip's lanes). A video clip's
							     does too once it has thumbnails, on a backing — a bright frame under
							     white text is unreadable otherwise. -->
							<span
								style="position:relative;font-size:10px;font-weight:600;color:var(--text-on-video);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;{t.kind ===
								'audio'
									? 'align-self:flex-end;margin-bottom:3px;text-shadow:0 1px 2px color-mix(in srgb,var(--scrim) 85%,transparent);'
									: filmReady[c.id]
										? 'align-self:flex-end;margin:0 0 3px -4px;padding:1px 5px;border-radius:3px;background:color-mix(in srgb,var(--scrim) 55%,transparent);text-shadow:0 1px 2px color-mix(in srgb,var(--scrim) 85%,transparent);'
										: ''}"
								>{editor.assetName(c.asset_id)}</span
							>
							{#if (c.speed ?? 1) !== 1}
								{@const sp = c.speed ?? 1}
								<span
									title="Speed {sp}×"
									style="position:absolute;right:3px;top:3px;font-size:9px;font-weight:700;color:var(--text-on-video);background:color-mix(in srgb,var(--scrim) 55%,transparent);border-radius:3px;padding:1px 4px;pointer-events:none"
									>{sp < 0 ? `${Math.abs(sp)}× ⟲` : `${sp}×`}</span
								>
							{/if}
							<ClipOverlays
								clip={c}
								{width}
								{pxPerSec}
								{fps}
								sound={audibleAssets.has(c.asset_id)}
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
								onseek={(time) => ui.seek(time)}
								onedge={(e, edge) => onEdgePointerDown(e, c, t, edge)}
							/>
						</button>
					{/each}
					<!-- One ghost per clip of the dragged group, where it would land: red
					     all over when the group cannot (the drop then does nothing). -->
					{#each ghostsByTrack.get(t.id) ?? [] as g (g.clipId)}
						<div
							style="position:absolute;left:{g.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
								6,
								g.dur * pxPerSec
							)}px;border:1.5px dashed {dropRefused ? 'var(--red-500)' : 'var(--kerf-400)'};border-radius:2px;background:{dropRefused
								? 'var(--danger-surface)'
								: 'color-mix(in srgb,var(--drag-ghost) 16%,transparent)'};pointer-events:none;z-index:25"
						></div>
					{/each}
					{#if trimPreview && trimDrag?.trackId === t.id}
						<!-- Ripple: where the trimmed clip and everything it moves would be — a
						     left edge keeps the clip's start, so this is not the edge dragged. -->
						{#each trimPreview.ghosts as g (g.id)}
							<div
								style="position:absolute;left:{g.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
									2,
									g.dur * pxPerSec
								)}px;border:1.5px dashed {trimPreview.ok ? 'var(--kerf-400)' : 'var(--red-500)'};border-radius:2px;background:{trimPreview.ok
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
					{#if rollHover?.trackId === t.id && !tg}
						<!-- The cut the roll tool would take hold of. -->
						<div
							style="position:absolute;left:{rollHover.time * pxPerSec - 1}px;top:3px;bottom:3px;width:2px;border-radius:1px;background:var(--kerf-300);box-shadow:0 0 6px 1px var(--kerf-400);pointer-events:none;z-index:24"
						></div>
					{/if}
					{#if tg?.moved && tg.preview && tg.trackId === t.id}
						<!-- A roll, slip or slide in progress: every clip it changes, where it would
						     stand. Amber when the drag is held at a limit, red when the backend
						     would turn it down (letting go then writes nothing). -->
						{@const pv = tg.preview}
						{@const edge = !pv.ok ? 'var(--red-500)' : pv.clamped ? 'var(--warning)' : 'var(--kerf-400)'}
						{#each pv.ghosts as g (g.id)}
							<div
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
						{#if tg.edit?.tool === 'roll'}
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
			{#if tg?.moved && tg.preview}
				{@const ro = readoutFor(tg.preview, fps)}
				{@const line = ro.tone === 'refused' ? 'var(--red-500)' : ro.tone === 'limit' ? 'var(--warning)' : 'var(--border-strong)'}
				<div
					role="status"
					aria-live="off"
					data-trim-readout
					style="position:absolute;left:{Math.max(4, Math.min(tg.nx + 14, contentW - 440))}px;top:{tg.ny + 18}px;z-index:35;pointer-events:none;max-width:430px;padding:3px 8px;border-radius:var(--radius-sm);border:var(--line-width) solid {line};background:var(--surface-raised);color:var(--text-primary);font-size:11px;line-height:1.35"
				>
					<div style="font-family:var(--font-mono);font-weight:600;white-space:nowrap">{ro.title}</div>
					{#if ro.detail}<div style="color:{ro.tone === 'refused' ? 'var(--red-500)' : 'var(--warning)'}">{ro.detail}</div>{/if}
				</div>
			{/if}

			<!-- why a group drag is red, while it is still a drag -->
			{#if drag?.moved && dropRefused && drag.plan?.reason}
				<div
					role="status"
					style="position:absolute;left:{drag.nx + 14}px;top:{drag.ny + 18}px;z-index:35;pointer-events:none;max-width:280px;padding:3px 8px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--red-500);background:var(--surface-raised);color:var(--text-primary);font-size:11px;line-height:1.35"
				>
					{drag.plan.reason}
				</div>
			{/if}

			<!-- playhead -->
			<div
				style="position:absolute;left:{ui.time * pxPerSec}px;top:0;bottom:0;width:var(--playhead-width);background:var(--playhead);box-shadow:0 0 10px 1px var(--playhead-glow);z-index:30;pointer-events:none"
			>
				<span
					style="position:absolute;top:-1px;left:-5px;width:12px;height:9px;background:var(--playhead);clip-path:polygon(0 0,100% 0,50% 100%)"
				></span>
			</div>
		</div>
	</div>
</div>
