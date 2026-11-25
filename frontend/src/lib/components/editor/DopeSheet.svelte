<script module lang="ts">
	import type { KeyClipboard } from '#lib/dope-sheet';

	/** The copied keys: one for the editor, so a paste works on another clip and after the panel moved. */
	let board = $state<KeyClipboard | undefined>(undefined);
	/** Keys on lanes, or the curves: kept while the panel moves between docks. */
	let view = $state<'keys' | 'graph'>('keys');
</script>

<script lang="ts">
	// The dope sheet (the Keyframes panel): one row per keyed number of the selected clip, its keys as
	// diamonds on one time axis — the clip's span, labelled in the cut's time. Click selects (and seeks),
	// Shift / Ctrl add, a drag on empty space is a marquee; dragging a key retimes the selection (the
	// grabbed key lands on a frame or on a magnet: the playhead, the clip's ends, a key that is not
	// moving), and Alt at the release leaves the originals and drops copies. Copy / paste / delete / select
	// all take the editor's own keys while the panel has focus; the easing popover shapes the segments
	// the selected keys start. Each gesture writes one `set_property_tracks` edit on release, holding what
	// it shows until that edit settles; Escape, pointercancel or the window losing focus abandon it.
	// The Graph view (`DopeGraph`) draws the same rows as curves on the same axis, with bezier handles.
	// Keys are reached from the keyboard too: Tab walks them, Up / Down jump to the next row, Space selects, and the
	// step and jump keys (`playback.step*` / `jump*`) move the selected keys a frame / a second: a run of presses is ONE
	// edit, written once the keys have been quiet for a moment, and Escape drops it. A clip's speed ramp has its own row
	// (read-only: it is another store with its own write, and it changes the clip's length, so it cannot be one gesture
	// with the numbers); its label opens the Inspector's Speed section.
	// The pure half is `dope-sheet.ts` (and `graph-editor.ts`).
	import { tick, untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import Btn from './Btn.svelte';
	import EmptyState from './EmptyState.svelte';
	import EasingPopover from './EasingPopover.svelte';
	import DopeGraph from './DopeGraph.svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import { activeProp } from '#lib/graph-editor';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { settings } from '#lib/settings.svelte';
	import { contextMenu } from '#lib/context-menu.svelte';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { beginDrag } from '#lib/drag';
	import { resizeObserverFor } from '#lib/realm';
	import { holdForPreview, report } from '#lib/ops';
	import { tip } from '#lib/tip';
	import { toast } from '#lib/notifications.svelte';
	import { allowsRepeat } from '#lib/keymap';
	import { rulerGrid, ticksIn, tickLabel } from '#lib/ruler';
	import { PROPERTIES, baseAt, keyTime, propertyKeys, upsertKey } from '#lib/channels';
	import { clipDuration } from '#lib/types';
	import { KEY_COMMIT_MS } from '#lib/slider-gesture';
	import { keyRefusal, onClipAt } from '#lib/inspector-summary';
	import { PROPERTY_NAMES, clipProperties, inspectorLabel } from '#lib/property-labels';
	import { titleOf } from '#lib/title-clips';
	import { fmtSpeed } from '#lib/speed-ramp-edit';
	import { workspace } from '#lib/workspace.svelte';
	import { revealSection } from './InspectorSection.svelte';
	import type { Clip, Easing, Property } from '#lib/types';
	import {
		allKeys,
		changes,
		copyKeys,
		curveHue,
		deleteKeys,
		dopeKey,
		dopeRows,
		dragMagnets,
		easeKeys,
		easingName,
		easingReason,
		easingTargets,
		fitBoard,
		fitRun,
		fitTickLabels,
		keyIds,
		isSelected,
		liveSelection,
		marqueeKeys,
		mergeSelection,
		nearestKey,
		neighbourKey,
		nudgeOf,
		partitionRamp,
		pasteKeys,
		rampLevels,
		rampLine,
		rampMarks,
		retimeKeys,
		rowLabel,
		runShift,
		sharedEasing,
		sheetStatus,
		snapShift,
		stopText,
		timeAt,
		toggleKey,
		xOf,
		type Axis,
		type DopeEdit,
		type DopeRow,
		type KeyRef,
		type Nudge,
		type NudgeRun
	} from '#lib/dope-sheet';

	const ROW_H = 22;
	const RULER_H = 22;
	/** The speed ramp's row is taller than a number's: its keys sit at their speed, so a slowdown reads as a fall. */
	const RAMP_H = 36;
	/** Room at each end of the axis, px: the key's whole hit box fits in the lane (14 px keys, 24 px graph keys). */
	const PAD_KEYS = 10;
	const PAD_GRAPH = 12;
	/** The graph's value labels have a column of their own at the lane's right, so no key (or its focus ring) is under one. */
	const GUTTER = 36;
	/** How near a magnet pulls a dragged key, px. */
	const REACH_PX = 6;
	/** Pointer travel that makes a press a drag, px. */
	const TRAVEL_PX = 3;
	const MODIFIERS = new Set(['Control', 'Shift', 'Alt', 'Meta']);

	const clip = $derived(editor.selectedClip);
	const rows = $derived(clip ? dopeRows(clip) : []);
	const duration = $derived(clip ? clipDuration(clip) : 0);
	/** The clip's speed ramp, as marks: shown, never written from here. Those out past `RAMP_REACH` clip lengths are only
	 *  counted (`beyond`), so a key eleven days out does not squeeze the lane to nothing or leave a tab stop off it. */
	const ramp = $derived(clip ? rampMarks(clip, (t) => timeDisplay.fmt(clip.timeline_start + t)) : []);
	const rampIn = $derived(partitionRamp(ramp, duration));
	const lastKey = $derived(Math.max(0, ...rows.flatMap((r) => r.keys.map((k) => k.time)), ...rampIn.inLane.map((m) => m.time)));
	const playhead = $derived(clip ? ui.time - clip.timeline_start : null);
	/** The playhead is on the clip, where a key can be put (`keyRefusal`: the Inspector's rule). */
	const onClip = $derived(clip ? onClipAt(clip, ui.time) : false);
	/** What the Inspector decides a clip has rows for (`clipBlocks`): a picture on a picture track, sound of its own. */
	const allowed = $derived.by(() => {
		const c = clip;
		if (!c) return [];
		const track = editor.timeline.tracks.find((t) => t.clips.some((x) => x.id === c.id));
		const asset = editor.assets.find((a) => a.id === c.asset_id);
		const picture = track?.kind !== 'audio' && (!!titleOf(c) || (asset?.streams.some((x) => x.kind === 'video') ?? false));
		return clipProperties({ picture, hasAudio: asset?.streams.some((x) => x.kind === 'audio') ?? false, soundDetached: c.source_audio === false });
	});
	/** A clip with no picture of its own has no pose: its way in is the volume, the one number it can animate. */
	const sound = $derived(!allowed.includes('scale'));
	/** The copied keys this clip can take, and why not when none can. */
	const fitted = $derived(board ? fitBoard(board, allowed) : null);
	const pasteWhy = $derived(
		!board
			? 'Copy keys first'
			: !fitted?.board
				? `None of the copied numbers (${fitted?.dropped.map((p) => rowLabel(p)).join(', ')}) are ones this clip has`
				: !onClip
					? 'Move the playhead onto this clip to paste keys'
					: null
	);

	let selection = $state<KeyRef[]>([]);
	let selectedFor: string | null = null;
	$effect(() => {
		const id = clip?.id ?? null;
		if (id !== selectedFor) {
			// A run of key presses on the clip being left is written to it (`write` reads its clip by id).
			untrack(() => void finishRun());
			selectedFor = id;
			selection = [];
			picked = null;
		}
	});
	$effect(() => () => void finishRun());
	const live = $derived(liveSelection(rows, selection));
	/** The row the graph draws with handles, when one was picked by its label. */
	let picked = $state<Property | null>(null);
	const active = $derived(activeProp(rows, live, picked));

	type Move = { kind: 'move'; clipId: string; base: KeyRef[]; grabbed: number; x0: number; moved: boolean; dt: number; alt: boolean };
	type Marquee = { kind: 'marquee'; base: KeyRef[]; x0: number; y0: number; x1: number; y1: number; moved: boolean };
	let gesture = $state<Move | Marquee | null>(null);
	/** A run of step / jump presses on the selected keys, not written yet: the keys it moves are drawn at their new
	 *  places until it is (the grabbed key is where focus goes back to). */
	type Run = { clipId: string; base: KeyRef[]; run: NudgeRun; focus: KeyRef };
	let nudge = $state.raw<Run | null>(null);
	/** The run's source of truth, a plain variable: `finishRun` takes it and clears it before anything else, so a second call
	 *  (focus leaving, then the panel being torn down) finds none and writes nothing. `nudge` mirrors it for drawing. */
	let pending: Run | null = null;
	let nudgeTimer: ReturnType<typeof setTimeout> | null = null;
	const setRun = (r: Run | null) => {
		pending = r;
		nudge = r;
	};
	/** What stopped the last press, said in the corner for a moment. */
	let notice = $state('');
	let noticeTimer: ReturnType<typeof setTimeout> | null = null;
	function say(text: string) {
		notice = text;
		if (noticeTimer) clearTimeout(noticeTimer);
		noticeTimer = setTimeout(() => (notice = ''), 1800);
	}
	/** Where focus was last in the panel: the key (or handle) it was on, so that when a write takes that element away it
	 *  goes to the nearest one that is left, or to the panel, and never to the page behind (where the next Delete is the
	 *  timeline's). Null once the user has taken it elsewhere. */
	let held: { ref: KeyRef | null } | null = null;
	/** A gesture's edit while it is being written: shown until it settles. */
	let writing = $state.raw<{ clipId: string; edit: DopeEdit } | null>(null);

	const ghost = $derived.by((): DopeEdit | null => {
		if (clip && gesture?.kind === 'move' && gesture.moved) return retimeKeys(clip, gesture.base, gesture.dt, gesture.alt);
		if (clip && nudge?.clipId === clip.id) return retimeKeys(clip, nudge.base, runShift(nudge.run, editor.fps));
		if (clip && writing?.clipId === clip.id) return writing.edit;
		return null;
	});
	const shown = $derived.by((): DopeRow[] => {
		if (!ghost) return rows;
		const has = (p: Property) => rows.some((r) => r.prop === p) || ghost.tracks.some((t) => t.prop === p && t.keys.length > 0);
		return PROPERTIES.filter(has).map((prop) => ({
			prop,
			label: rowLabel(prop),
			keys: ghost.tracks.find((t) => t.prop === prop)?.keys ?? rows.find((r) => r.prop === prop)?.keys ?? []
		}));
	});
	const marqueeRect = $derived(
		gesture?.kind === 'marquee' && gesture.moved
			? {
					x: Math.min(gesture.x0, gesture.x1),
					y: Math.min(gesture.y0, gesture.y1),
					w: Math.abs(gesture.x1 - gesture.x0),
					h: Math.abs(gesture.y1 - gesture.y0)
				}
			: null
	);
	const shownSel = $derived.by((): KeyRef[] => {
		if (ghost) return ghost.selected;
		if (gesture?.kind === 'marquee' && marqueeRect) return mergeSelection(gesture.base, inRect(marqueeRect));
		return live;
	});

	const targets = $derived(clip ? easingTargets(clip, live) : []);
	const busy = $derived(editor.locked);
	/** Why the easing button has nothing to shape, or null. */
	const noEasing = $derived(easingReason(live.length, targets.length));
	const status = $derived(notice || sheetStatus(rows.length, shownSel.length));
	/** The height of the lanes: a row for each number, then the ramp's taller one when the clip has one. */
	const laneH = $derived(shown.length * ROW_H + (ramp.length > 0 ? RAMP_H : 0));
	const rampPlot = $derived(
		rampLevels(
			rampIn.inLane.map((m) => m.speed),
			RAMP_H
		)
	);

	const nothing = $derived(rows.length === 0 && !ghost && ramp.length === 0);
	/** The curves, unless there is no number to draw one for (a clip with only a ramp). */
	const graphOn = $derived(view === 'graph' && shown.length > 0);

	let width = $state(0);
	/** The lane's drawn width: all of it, but for the graph's gutter. */
	const lane = $derived(graphOn ? Math.max(1, width - GUTTER) : width);
	const axis = $derived<Axis>({ duration: Math.max(duration, lastKey, 1e-3), width: lane, pad: graphOn ? PAD_GRAPH : PAD_KEYS });
	const pxPerSec = $derived(Math.max(1, axis.width - 2 * axis.pad) / axis.duration);

	// ---- geometry ------------------------------------------------------------

	let root = $state<HTMLDivElement | null>(null);
	let lanes = $state<HTMLDivElement | null>(null);
	let body = $state<HTMLDivElement | null>(null);
	$effect(() => {
		if (!lanes) return;
		const el = lanes;
		const ro = resizeObserverFor(el, (entries) => (width = entries.at(-1)?.contentRect.width ?? el.clientWidth));
		ro.observe(el);
		return () => ro.disconnect();
	});

	const xIn = (e: { clientX: number }) => e.clientX - (lanes?.getBoundingClientRect().left ?? 0);
	const yIn = (e: { clientY: number }) => e.clientY - (body?.getBoundingClientRect().top ?? 0);
	const clampTime = (t: number) => Math.min(Math.max(t, 0), axis.duration);

	function inRect(r: { x: number; y: number; w: number; h: number }): KeyRef[] {
		const r0 = Math.floor(r.y / ROW_H);
		const r1 = Math.floor((r.y + r.h) / ROW_H);
		return marqueeKeys(rows, r0, r1, timeAt(axis, r.x), timeAt(axis, r.x + r.w));
	}

	const grid = $derived(rulerGrid(pxPerSec, editor.fps));
	const ticks = $derived(clip && width > 0 ? ticksIn(clip.timeline_start, clip.timeline_start + axis.duration, grid.step, 400) : []);
	/** The ruler's labels as they fit the lane (`fitTickLabels`): none runs past its edge. */
	const placed = $derived(
		fitTickLabels(
			ticks.map((t) => ({ x: xOf(axis, t.t - (clip?.timeline_start ?? 0)), text: tickLabel(t.k, grid, editor.fps, timeDisplay.mode) })),
			axis.width
		)
	);
	/** The speed at each mark, beside it, where it fits. */
	const rampLabels = $derived(
		fitTickLabels(
			rampIn.inLane.map((m, i) => ({ x: xOf(axis, m.time), text: fmtSpeed(m.speed, 2), y: rampPlot.y[i] })),
			axis.width,
			6.2,
			10
		)
	);

	// ---- writing ---------------------------------------------------------------

	const clipById = (id: string): Clip | undefined => editor.timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === id);

	async function write(c: Clip, edit: DopeEdit) {
		if (edit.tracks.length === 0 || !changes(c, edit)) return;
		if (holdForPreview()) return;
		const mine = { clipId: c.id, edit };
		writing = mine;
		try {
			await editor.setPropertyTracks(c.id, edit.tracks);
			// What is selected is the keys it put, on the clip it was written to: by now another clip may be shown.
			if (writing === mine && editor.selectedClip?.id === c.id) selection = edit.selected;
		} catch (e) {
			report(e);
		} finally {
			if (writing === mine) writing = null;
		}
	}

	function copy() {
		if (!clip) return;
		const got = copyKeys(clip, live);
		if (got) board = got;
	}
	function paste() {
		const c = clip;
		if (!c || !board) return;
		// Keys are put at the playhead, so only where it is on the clip (the Inspector's key buttons say the same), and only
		// the numbers the clip has (a pose copied from a picture is nothing to a sound clip).
		const why = keyRefusal(c, ui.time) ?? (fitted?.board ? null : pasteWhy);
		if (why) {
			toast.info(why);
			return;
		}
		if (fitted && fitted.dropped.length > 0) toast.info(`Left out ${fitted.dropped.map((p) => rowLabel(p)).join(', ')}: this clip has no such number`);
		void write(c, pasteKeys(c, board, Math.min(Math.max(playhead ?? 0, 0), duration), allowed));
	}
	function remove() {
		const c = clip;
		if (!c || live.length === 0) return;
		void write(c, deleteKeys(c, live));
	}
	function cut() {
		copy();
		remove();
	}
	function ease(e: Easing) {
		const c = clip;
		popover = null;
		if (!c || targets.length === 0) return;
		void write(c, easeKeys(c, live, e));
	}

	// ---- the way in ---------------------------------------------------------------

	/** Key the pose at the playhead: the Animation section's own *Keyframe at playhead* (scale, position, rotation and
	 *  opacity together), refused off the clip as the Inspector's key buttons are. */
	async function keyPose() {
		const c = clip;
		if (!c || busy || keyRefusal(c, ui.time) || holdForPreview()) return;
		try {
			await editor.addKeyframe(c.id, keyTime(c, ui.time));
		} catch (e) {
			report(e);
		}
	}
	/** The same for a clip of sound: its volume keyed at the playhead at the level it has there (`keyAtPlayhead` in the Inspector). */
	async function keyVolume() {
		const c = clip;
		if (!c || busy || keyRefusal(c, ui.time) || holdForPreview()) return;
		const at = keyTime(c, ui.time);
		try {
			await editor.setPropertyKeyframes(c.id, 'volume', upsertKey(propertyKeys(c, 'volume'), { time: at, value: baseAt(c, 'volume', at) }));
		} catch (e) {
			report(e);
		}
	}
	/** Off the clip there is no key to put: take the playhead to the clip's first frame, from where *Key the pose* works. */
	function goToClip() {
		if (clip) ui.seek(clip.timeline_start, { snap: 'exact' });
	}
	/** The ramp's label: open the Inspector's Speed section, bringing the Inspector forward first. */
	async function showSpeed() {
		workspace.show('inspector');
		const doc = root?.ownerDocument;
		if (!doc) return;
		for (let tries = 0; tries < 12; tries++) {
			await tick();
			const section = doc.querySelector<HTMLDetailsElement>('details[data-section="speed"]');
			if (section) {
				revealSection(section);
				section.scrollIntoView({ block: 'nearest' });
				section.querySelector<HTMLElement>('summary')?.focus({ preventScroll: true });
				return;
			}
			await new Promise((r) => setTimeout(r, 40));
		}
	}

	// ---- the easing popover ------------------------------------------------------

	let popover = $state<{ x: number; y: number } | null>(null);
	let easingButton = $state<HTMLElement | null>(null);
	/** Open it where it was asked; with no segment to shape it says why instead of the presets. */
	function openEasing(at: { x: number; y: number }) {
		popover = at;
	}

	// ---- focus ----------------------------------------------------------------------

	const keyButtons = () => [...(root?.querySelectorAll<HTMLElement>('button[data-prop]') ?? [])];
	/** The key a button is (a key of the sheet or the graph). */
	const refOf = (el: Element | null): KeyRef | null => {
		const k = el instanceof Element ? el.closest<HTMLElement>('button[data-prop]') : null;
		return k ? { prop: k.dataset.prop as Property, time: Number(k.dataset.time) } : null;
	};
	/** What an element in the panel stands for when focus has to come back to its place: a key, or a graph handle (by the
	 *  key that starts its segment). */
	const placeOf = (el: Element | null): KeyRef | null => {
		const k = el instanceof Element ? el.closest<HTMLElement>('[data-prop], [data-focus-prop]') : null;
		if (!k) return null;
		return { prop: (k.dataset.prop ?? k.dataset.focusProp) as Property, time: Number(k.dataset.time ?? k.dataset.focusTime) };
	};
	/** Put focus on the key at `at` (the nearest of that number, as a write may round a time). */
	function focusKey(at: KeyRef): boolean {
		let best: HTMLElement | null = null;
		let gap = Infinity;
		for (const el of keyButtons()) {
			if (el.dataset.prop !== at.prop) continue;
			const d = Math.abs(Number(el.dataset.time) - at.time);
			if (d < gap) [best, gap] = [el, d];
		}
		if (!best || gap >= 1e-3) return false;
		best.focus({ preventScroll: true });
		return true;
	}
	/** Focus is in the panel but on nothing now (the element it was on went with a delete, a paste, an undo, a view change, a
	 *  run that re-timed its key): put it on the nearest key that is left, else on the panel. A press on a key left it
	 *  there; a click elsewhere cleared `held`, and nothing is taken back from where the user went. */
	async function restoreFocus() {
		await tick();
		const doc = root?.ownerDocument;
		if (!root || !doc || !held) return;
		const at = doc.activeElement;
		if (at && at !== doc.body && root.contains(at) && at.isConnected) return;
		if (held.ref) {
			// the key itself, if it is still (or again) drawn; else the nearest of those that are: the rows as shown (a run's
			// keys are at their new places) and only the numbers whose keys have buttons (the graph draws one row's)
			if (focusKey(held.ref)) return;
			const drawn = new Set(keyButtons().map((b) => b.dataset.prop));
			const to = nearestKey(
				shown.filter((r) => drawn.has(r.prop)),
				held.ref
			);
			if (to && focusKey(to)) {
				held = { ref: to };
				return;
			}
		}
		root.focus({ preventScroll: true });
		held = { ref: null };
	}
	function onfocusin(e: FocusEvent) {
		held = { ref: placeOf(e.target as Element | null) };
	}
	/** Focus left an element of the panel. Chrome says so when the element is merely removed (a delete took its key), with no
	 *  new owner: a moment later, if the element is gone, that is a write's doing and focus goes back (`restoreFocus`); if it
	 *  is still there the user has left, and a run of presses is written. */
	function onfocusout(e: FocusEvent) {
		if (e.relatedTarget instanceof Node && root?.contains(e.relatedTarget)) return;
		const gone = e.target as Element;
		setTimeout(() => {
			const at = root?.ownerDocument.activeElement;
			if (!root || (at && root.contains(at))) return;
			if (!gone.isConnected && held) {
				void restoreFocus();
				return;
			}
			held = null;
			void finishRun();
		}, 0);
	}
	// Whatever a write does to the rows, focus stays in the panel (a browser that does not say an element was removed too).
	$effect(() => {
		void shown;
		void view;
		untrack(() => {
			if (held) void restoreFocus();
		});
	});

	// ---- a run of key presses ------------------------------------------------------

	/** Write the run so far as one edit, to the clip it began on. Idempotent: the run is taken (and cleared) first, so
	 *  focus leaving and the panel going away at once write it once. */
	async function finishRun() {
		const r = pending;
		if (nudgeTimer) clearTimeout(nudgeTimer);
		nudgeTimer = null;
		if (!r) return;
		setRun(null);
		const now = clipById(r.clipId);
		if (!now) return;
		const shift = runShift(r.run, editor.fps);
		await write(now, retimeKeys(now, r.base, shift));
		if (held) void restoreFocus();
	}
	/** Drop the run: nothing is written (Escape, or an undo that takes the newest change back). */
	function dropRun() {
		if (nudgeTimer) clearTimeout(nudgeTimer);
		nudgeTimer = null;
		setRun(null);
	}
	/** A step / jump press on the selected keys (or on the focused key, which it selects first). False when there is
	 *  nothing to move, and the press is the editor's. The keys stay on the clip and land on no key: a press that would
	 *  is refused and says so (`fitRun`). */
	function press(e: KeyboardEvent, n: Nudge): boolean {
		const c = clip;
		if (!c) return false;
		if (pending && pending.clipId !== c.id) void finishRun();
		let base: KeyRef[];
		let focus: KeyRef;
		if (pending) {
			// A run under way: its keys are drawn where they would land, so what is focused is not what it began on.
			base = pending.base;
			focus = pending.focus;
		} else {
			const on = refOf(e.target as Element | null);
			base = on && !isSelected(live, on.prop, on.time) ? [on] : live;
			if (base.length === 0) return false;
			if (busy || holdForPreview()) return true;
			selection = base;
			focus = on ?? base[0];
		}
		const fit = fitRun(rows, base, pending?.run ?? null, n, editor.fps, duration);
		if (fit.stop) say(stopText(fit.stop));
		if (!pending && fit.run.frames === 0 && fit.run.seconds === 0) return true;
		setRun({ clipId: c.id, base, run: fit.run, focus });
		// the key that moved is where focus goes back to as its button is made again at the new time
		if (held) held = { ref: { prop: focus.prop, time: focus.time + fit.shift } };
		if (nudgeTimer) clearTimeout(nudgeTimer);
		nudgeTimer = setTimeout(() => void finishRun(), KEY_COMMIT_MS);
		return true;
	}

	// ---- gestures ----------------------------------------------------------------

	/** A press on a key. The drag is captured by the lanes, not the diamond: a diamond may go
	 *  mid-drag (the key it drew landed on another), which would end the capture. */
	function keyDown(e: PointerEvent, prop: Property, time: number) {
		if (!clip) return;
		const additive = e.shiftKey || e.ctrlKey || e.metaKey;
		const hit = isSelected(live, prop, time);
		const base = hit ? live : additive ? [...live, { prop, time }] : [{ prop, time }];
		selection = base;
		const c = clip;
		let g: Move = { kind: 'move', clipId: c.id, base, grabbed: time, x0: xIn(e), moved: false, dt: 0, alt: e.altKey };
		gesture = g;
		beginDrag(e, {
			move(ev) {
				const x = xIn(ev);
				if (!g.moved && Math.abs(x - g.x0) < TRAVEL_PX) return;
				const raw = timeAt(axis, x) - timeAt(axis, g.x0);
				const dt = snapShift(base, time, raw, {
					magnets: dragMagnets(rows, base, duration, playhead),
					reach: REACH_PX / pxPerSec,
					fps: editor.fps,
					clipStart: c.timeline_start
				});
				g = { ...g, moved: true, dt, alt: ev.altKey };
				gesture = g;
			},
			commit(ev) {
				gesture = null;
				if (!g.moved) {
					if (hit && additive) selection = toggleKey(live, { prop, time });
					else if (!additive) {
						selection = [{ prop, time }];
						ui.seek(c.timeline_start + time, { snap: 'exact' });
					}
					return;
				}
				const now = clipById(g.clipId);
				if (now) void write(now, retimeKeys(now, base, g.dt, ev.altKey));
			},
			abandon() {
				gesture = null;
			}
		});
	}

	function laneDown(e: PointerEvent) {
		if (e.button !== 0 || !clip) return;
		void finishRun();
		root?.focus({ preventScroll: true });
		// A mark of the speed ramp: where it is, nothing to drag (the ramp is the Inspector's).
		const mark = e.target instanceof Element ? e.target.closest<HTMLElement>('[data-ramp]') : null;
		if (mark) {
			e.preventDefault();
			ui.seek(clip.timeline_start + Number(mark.dataset.ramp), { snap: 'exact' });
			return;
		}
		const key = e.target instanceof Element ? e.target.closest<HTMLElement>('[data-prop]') : null;
		if (key) return keyDown(e, key.dataset.prop as Property, Number(key.dataset.time));
		const additive = e.shiftKey || e.ctrlKey || e.metaKey;
		const x0 = xIn(e);
		const y0 = yIn(e);
		let g: Marquee = { kind: 'marquee', base: additive ? live : [], x0, y0, x1: x0, y1: y0, moved: false };
		gesture = g;
		beginDrag(e, {
			move(ev) {
				const x1 = xIn(ev);
				const y1 = yIn(ev);
				const moved = g.moved || Math.hypot(x1 - x0, y1 - y0) >= TRAVEL_PX;
				g = { ...g, x1, y1, moved };
				gesture = g;
			},
			commit() {
				gesture = null;
				if (!g.moved) {
					if (!additive) selection = [];
					return;
				}
				const r = { x: Math.min(g.x0, g.x1), y: Math.min(g.y0, g.y1), w: Math.abs(g.x1 - g.x0), h: Math.abs(g.y1 - g.y0) };
				selection = mergeSelection(g.base, inRect(r));
			},
			abandon() {
				gesture = null;
			}
		});
	}

	/** A key clicked in the graph: as a click on a diamond. */
	function graphPick(prop: Property, time: number, additive: boolean) {
		picked = prop;
		if (additive) {
			selection = toggleKey(live, { prop, time });
			return;
		}
		selection = [{ prop, time }];
		if (clip) ui.seek(clip.timeline_start + time, { snap: 'exact' });
	}

	function graphClear(additive: boolean) {
		if (!additive) selection = [];
	}

	function rulerDown(e: PointerEvent) {
		if (e.button !== 0 || !clip) return;
		const start = clip.timeline_start;
		const was = ui.time;
		const go = (ev: PointerEvent) => ui.seek(start + clampTime(timeAt(axis, xIn(ev))));
		go(e);
		beginDrag(e, {
			move: go,
			commit() {},
			abandon() {
				ui.seek(was, { snap: 'exact' });
			}
		});
	}

	function keyMenu(e: MouseEvent, prop: Property, time: number) {
		e.preventDefault();
		e.stopPropagation();
		if (!isSelected(live, prop, time)) selection = [{ prop, time }];
		const at = { x: e.clientX, y: e.clientY };
		contextMenu.show(e, [
			{
				label: 'Easing…',
				icon: 'chart-no-axes-gantt',
				disabled: targets.length === 0,
				reason: targets.length === 0 ? 'the last key of a row starts no segment' : undefined,
				action: () => openEasing(at)
			},
			{ type: 'separator' },
			{ label: 'Copy keys', icon: 'copy', shortcut: settings.shortcut('edit.copy'), action: copy },
			{
				label: 'Paste keys at the playhead',
				icon: 'clipboard',
				shortcut: settings.shortcut('edit.paste'),
				disabled: !board || !onClip,
				reason: !board ? 'copy keys first' : !onClip ? 'the playhead is not on this clip' : undefined,
				action: paste
			},
			{ label: 'Delete keys', icon: 'trash', danger: true, shortcut: settings.shortcut('edit.delete'), action: remove }
		]);
	}

	/** Alt pressed or let go mid-drag: the ghost says at once whether the release duplicates. */
	function altChanged(e: KeyboardEvent) {
		if (e.key === 'Alt' && gesture?.kind === 'move' && gesture.moved) gesture = { ...gesture, alt: e.type === 'keydown' };
	}

	function onkeydown(e: KeyboardEvent) {
		altChanged(e);
		if (!clip || e.defaultPrevented) return;
		const action = settings.actionFor(e);
		if (e.key === 'Escape') {
			// Only what is in flight is Escape's first: a run, then the selection; with neither it is the page's.
			if (pending) dropRun();
			else if (selection.length > 0) selection = [];
			else return;
		} else if (pending && action === 'edit.undo') {
			// The run is the newest change and is not written: undo takes it back, not the edit before it.
			dropRun();
		} else {
			const what = dopeKey(action, e.repeat, allowsRepeat);
			const n = nudgeOf(what);
			if (n) {
				if (!press(e, n)) return;
			} else if (what === 'copy') {
				void finishRun();
				copy();
			} else if (what === 'cut') {
				void finishRun();
				cut();
			} else if (what === 'paste') {
				void finishRun();
				paste();
			} else if (what === 'delete') {
				void finishRun();
				remove();
			} else if (what === 'selectAll') selection = allKeys(rows);
			else if (what !== 'swallow') {
				const on = refOf(e.target as Element | null);
				const dir = e.key === 'ArrowUp' ? -1 : e.key === 'ArrowDown' ? 1 : 0;
				if (on && dir !== 0 && view === 'keys' && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey) {
					const to = neighbourKey(shown, on, dir);
					if (!to) return;
					focusKey(to);
				} else if (pending && e.key === 'Enter') {
					// Enter writes the run; it is the key's own press too (it toggles the key), and must not be both.
					void finishRun();
				} else {
					// Any other chord is written first: a shortcut sees the run as it was. (A modifier going down on its way
					// to a chord is not one: Ctrl, then Z, must still find the run to take back.)
					if (pending && !MODIFIERS.has(e.key) && (e.ctrlKey || e.metaKey)) void finishRun();
					return;
				}
			}
		}
		e.preventDefault();
		e.stopPropagation();
	}

	const keyName = (label: string, t: number, easing?: Easing) => {
		const how = easingName(easing);
		return `${label} key at ${timeDisplay.fmt((clip?.timeline_start ?? 0) + t)}${how ? `, ${how}` : ''}`;
	};
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div class="dope" bind:this={root} tabindex="0" role="region" aria-label="Keyframes" {onkeydown} onkeyup={altChanged} {onfocusin} {onfocusout}>
	{#if !clip}
		<EmptyState icon="diamond" title="No clip selected" hint="Select a clip to see its keyframes, one row per animated number." />
	{:else}
		<div class="bar">
			<ChoiceChips
				label="Keyframes view"
				options={[
					{ value: 'keys', label: 'Keys', hint: 'Keys on one lane per number' },
					{ value: 'graph', label: 'Graph', hint: 'Curves, with bezier handles to shape each segment' }
				]}
				value={view}
				onpick={(v) => (view = v)}
				testid="dope-view"
			/>
			<span class="tools">
				<Btn
					size="sm"
					variant="ghost"
					icon="chevron-down"
					pending={busy}
					title={noEasing ?? 'The shape of the segments the selected keys start'}
					aria-haspopup="dialog"
					aria-expanded={popover !== null}
					onclick={(e: MouseEvent) => {
						easingButton = e.currentTarget as HTMLElement;
						if (popover) {
							popover = null;
							return;
						}
						const r = easingButton.getBoundingClientRect();
						openEasing({ x: r.left, y: r.bottom + 4 });
					}}>Easing</Btn
				>
				<IconBtn size={24} title={settings.tip('Copy keys', 'edit.copy')} disabled={live.length === 0} onclick={copy}
					><Icon n="copy" s={13} /></IconBtn
				>
				<IconBtn
					size={24}
					title={pasteWhy ?? settings.tip('Paste keys at the playhead', 'edit.paste')}
					pending={busy || (!!board && !!pasteWhy)}
					disabled={!board}
					onclick={paste}
					><Icon n="clipboard" s={13} /></IconBtn
				>
				<IconBtn size={24} title={settings.tip('Delete keys', 'edit.delete')} pending={busy} disabled={live.length === 0} onclick={remove}
					><Icon n="trash" s={13} /></IconBtn
				>
			</span>
		</div>
		{#if nothing}
			{@render offer(20)}
		{:else}
			<!-- Focusing a key must never scroll the sheet sideways (the lane clips its own contents; this is the belt). -->
			<div
				class="sheet"
				onscroll={(e) => {
					if (e.currentTarget.scrollLeft !== 0) e.currentTarget.scrollLeft = 0;
				}}
			>
				<div class="labels">
					<div
						class="corner"
						style="height:{RULER_H}px"
						role="status"
						use:tip={shownSel.length > 0
							? {
									text: `${shownSel.length} selected`,
									detail: `${settings.shortcut('playback.stepBack')} / ${settings.shortcut('playback.stepForward')} move them a frame, ${settings.shortcut('playback.jumpBack')} / ${settings.shortcut('playback.jumpForward')} a second. Escape drops a move.`
								}
							: `${rows.length} animated ${rows.length === 1 ? 'number' : 'numbers'}`}
					>
						{status}
					</div>
					{#if graphOn}
						{#each shown as r, i (r.prop)}
							<button
								type="button"
								class="label pick"
								class:active={r.prop === active}
								style="height:{ROW_H}px"
								aria-pressed={r.prop === active}
								onclick={() => (picked = r.prop)}
							>
								<span class="dot" style="background:{curveHue(i)}" aria-hidden="true"></span>
								<span class="text">{r.label}</span>
							</button>
						{/each}
					{:else}
						{#each shown as r (r.prop)}
							<div class="label" style="height:{ROW_H}px"><span class="text">{r.label}</span></div>
						{/each}
					{/if}
					{#if ramp.length > 0}
						<button
							type="button"
							class="label pick ramp"
							style="height:{RAMP_H}px"
							use:tip={{
								text: 'Speed ramp',
								detail: `Its keys are marked here, higher where the clip runs faster.${rampIn.beyond.length > 0 ? ` ${rampIn.beyond.length} more lie past the clip’s end.` : ''} Edit them in the Inspector’s Speed section.`
							}}
							onclick={() => void showSpeed()}
						>
							<Icon n="trending-up" s={12} />
							<span class="text">Speed ramp</span>
							<Icon n="chevron-right" s={11} />
						</button>
					{/if}
				</div>
				<div class="lanes" class:graph={graphOn} bind:this={lanes}>
					<!-- svelte-ignore a11y_no_static_element_interactions -->
					<div class="ruler" style="height:{RULER_H}px" onpointerdown={rulerDown} aria-hidden="true">
						{#each ticks as t (t.k)}
							<span class="tick" style="left:{xOf(axis, t.t - clip.timeline_start)}px"></span>
						{/each}
						{#each placed as l (l.text)}
							<span class="tlabel" style="left:{l.left}px">{l.text}</span>
						{/each}
					</div>
					{#if graphOn}
						<DopeGraph
							{clip}
							rows={shown}
							{axis}
							{active}
							selection={shownSel}
							{duration}
							hue={(prop) => curveHue(shown.findIndex((r) => r.prop === prop))}
							onpress={() => root?.focus({ preventScroll: true })}
							onpick={graphPick}
							onclear={graphClear}
							onwrite={(id, make) => {
								const now = clipById(id);
								const edit = now ? make(now) : null;
								if (now && edit) void write(now, edit);
							}}
						/>
					{:else}
						<!-- svelte-ignore a11y_no_static_element_interactions -->
						<div class="body" bind:this={body} style="height:{laneH}px" onpointerdown={laneDown}>
							{#if axis.duration > duration}
								<span class="past" style="left:{xOf(axis, duration)}px" aria-hidden="true"></span>
							{/if}
							{#each shown as r, i (r.prop)}
								{@const ids = keyIds(r.keys)}
								<div class="row" style="top:{i * ROW_H}px;height:{ROW_H}px" role="group" aria-label={r.label}>
									{#each r.keys as k, j (ids[j])}
										{@const on = isSelected(shownSel, r.prop, k.time)}
										<button
											type="button"
											class="key"
											class:on
											class:held={k.easing === 'hold'}
											class:eased={!!k.easing && k.easing !== 'hold'}
											style="left:{xOf(axis, k.time) - 7}px"
											aria-label={keyName(r.label, k.time, k.easing)}
											aria-pressed={on}
											use:tip={keyName(r.label, k.time, k.easing)}
											data-prop={r.prop}
											data-time={String(k.time)}
											onclick={(e) => {
												if (e.detail === 0) selection = toggleKey(live, { prop: r.prop, time: k.time });
											}}
											oncontextmenu={(e) => keyMenu(e, r.prop, k.time)}
										><span class="diamond"></span></button>
									{/each}
								</div>
							{/each}
							{#if ramp.length > 0}
								<div
									class="row ramp"
									style="top:{shown.length * ROW_H}px;height:{RAMP_H}px"
									role="group"
									aria-label={rampIn.beyond.length > 0 ? `Speed ramp, ${rampIn.beyond.length} more past the clip's end` : 'Speed ramp'}
								>
									<svg class="rampline" aria-hidden="true">
										{#if rampPlot.baseline !== null}<line class="one" x1="0" x2="100%" y1={rampPlot.baseline} y2={rampPlot.baseline} />{/if}
										<path d={rampLine(rampIn.inLane, axis.duration, (t) => xOf(axis, t), RAMP_H)} />
									</svg>
									{#each rampLabels as l (l.left)}
										<span class="rlabel" style="left:{l.left}px;top:{l.y - 7}px" aria-hidden="true">{l.text}</span>
									{/each}
									{#each rampIn.inLane as m, i (m.time)}
										<button
											type="button"
											class="key mark"
											class:held={m.hold}
											style="left:{xOf(axis, m.time) - 7}px;top:{rampPlot.y[i]}px;margin-top:-7px"
											aria-label={`Speed key: ${m.text}`}
											use:tip={{ text: `Speed ${m.text}`, detail: 'A key of the speed ramp. Edit it in the Inspector’s Speed section.' }}
											data-ramp={String(m.time)}
											onclick={(e) => {
												if (e.detail === 0 && clip) ui.seek(clip.timeline_start + m.time, { snap: 'exact' });
											}}
										><span class="diamond"></span></button>
									{/each}
									{#if rampIn.beyond.length > 0}
										<span class="beyond" aria-hidden="true">+{rampIn.beyond.length}</span>
									{/if}
								</div>
							{/if}
							{#if marqueeRect}
								<span
									class="marquee"
									style="left:{marqueeRect.x}px;top:{marqueeRect.y}px;width:{marqueeRect.w}px;height:{marqueeRect.h}px"
									aria-hidden="true"
								></span>
							{/if}
						</div>
					{/if}
					{#if playhead !== null && playhead >= 0 && playhead <= axis.duration}
						<span class="playhead" style="left:{xOf(axis, playhead)}px" aria-hidden="true"></span>
					{/if}
				</div>
			</div>
			{#if rows.length === 0 && !ghost}
				{@render offer(8)}
			{/if}
		{/if}
	{/if}
	{#if popover && clip}
		<EasingPopover
			x={popover.x}
			y={popover.y}
			current={sharedEasing(clip, targets)}
			reason={noEasing}
			opener={easingButton}
			onpick={ease}
			onclose={() => (popover = null)}
		/>
	{/if}
</div>

<!-- The way in for a clip nothing is animated on: an action that works from where the playhead is, and where the
     numbers' own diamonds are, not a paragraph about a control elsewhere. -->
{#snippet offer(pad: number)}
	{@const only = rows.length === 0 && ramp.length > 0}
	<EmptyState
		icon="diamond"
		title={only ? 'No numbers are animated' : 'This clip has no keyframes'}
		hint={onClip
			? `Key the ${sound ? 'volume' : 'pose'} at the playhead, or use the diamond beside ${inspectorLabel(sound ? 'volume' : 'scale')} in the Inspector’s ${PROPERTY_NAMES[sound ? 'volume' : 'scale'].group} section.`
			: `The playhead is not on this clip. Go to it, then key the ${sound ? 'volume' : 'pose'}, or use the diamond beside ${inspectorLabel(sound ? 'volume' : 'scale')} in the Inspector.`}
		{pad}
	>
		{#if onClip && sound}
			<Btn size="sm" icon="diamond" iconSize={12} pending={busy} title="The volume it has here, as a key you can then move" onclick={() => void keyVolume()}>
				Key the volume here
			</Btn>
		{:else if onClip}
			<Btn size="sm" icon="diamond" iconSize={12} pending={busy} title="Scale, position, rotation and opacity, together, as they are now" onclick={() => void keyPose()}>
				Key the pose here
			</Btn>
		{:else}
			<Btn size="sm" icon="skip-back" iconSize={12} title="Move the playhead to this clip’s first frame" onclick={goToClip}>Go to the clip</Btn>
		{/if}
		{#if board}
			<Btn size="sm" icon="clipboard" iconSize={12} pending={busy || !!pasteWhy} title={pasteWhy ?? 'Paste the copied keys at the playhead'} onclick={paste}>
				Paste keys
			</Btn>
		{/if}
	</EmptyState>
{/snippet}

<style>
	.dope {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		background: var(--surface-panel);
		overflow: hidden;
	}
	.dope:focus {
		outline: none;
	}
	.bar {
		flex: none;
		display: flex;
		align-items: center;
		gap: 4px;
		min-height: 32px;
		padding: 2px 8px;
		border-bottom: var(--line-width) solid var(--border-subtle);
	}
	.tools {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 2px;
	}
	.sheet {
		flex: 1;
		min-height: 0;
		display: flex;
		overflow-y: auto;
		overflow-x: hidden;
	}
	.labels {
		flex: none;
		width: 120px;
		border-right: var(--line-width) solid var(--border-subtle);
	}
	/* The corner above the labels, beside the ruler: what is selected, else how many numbers are listed. It is
	   where the status lives because the bar has no room left for words at the panel's narrowest. */
	.corner {
		display: flex;
		align-items: center;
		padding: 0 8px;
		box-sizing: border-box;
		border-bottom: var(--line-width) solid var(--border-subtle);
		font-family: var(--font-mono);
		font-size: var(--text-2xs);
		color: var(--text-muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.label {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 0 8px;
		font-size: var(--text-xs);
		color: var(--text-secondary);
		border-bottom: var(--line-width) solid var(--border-subtle);
		box-sizing: border-box;
	}
	.label .text {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	/* `clip`, not `hidden`: a clipped box is not a scroll container, so focusing a key (or anything) out of the lane cannot
	   scroll the sheet sideways out of reach. */
	.lanes {
		position: relative;
		flex: 1;
		min-width: 0;
		overflow: clip;
	}
	.lanes.graph {
		display: flex;
		flex-direction: column;
	}
	.lanes.graph .ruler {
		flex: none;
	}
	/* A label that does something: the graph's rows pick the curve with handles, the ramp's opens the Inspector. */
	.label.pick {
		width: 100%;
		border: none;
		border-bottom: var(--line-width) solid var(--border-subtle);
		background: none;
		font-family: inherit;
		text-align: left;
		cursor: pointer;
	}
	.label.pick:hover {
		color: var(--text-primary);
		box-shadow: var(--hover-fill);
	}
	.label.pick.active {
		color: var(--text-primary);
		font-weight: 600;
		box-shadow: inset 2px 0 0 var(--clip-accent);
	}
	.label.pick.active:hover {
		box-shadow:
			var(--hover-fill),
			inset 2px 0 0 var(--clip-accent);
	}
	.label.pick:focus-visible {
		outline: var(--focus-outline);
		outline-offset: -2px;
	}
	.label.ramp {
		color: var(--text-secondary);
	}
	.label.ramp .text {
		flex: 1;
	}
	.dot {
		flex: none;
		width: 8px;
		height: 8px;
		border-radius: var(--radius-full);
	}
	.ruler {
		position: relative;
		border-bottom: var(--line-width) solid var(--border-subtle);
		cursor: col-resize;
		touch-action: none;
		overflow: hidden;
	}
	.tick {
		position: absolute;
		bottom: 0;
		width: var(--line-width);
		height: 8px;
		background: var(--timeline-grid-major);
	}
	/* Above the playhead, with a halo of the panel's own colour, so the line never strikes through a time. */
	.tlabel {
		position: absolute;
		top: 3px;
		z-index: 3;
		font-family: var(--font-mono);
		font-size: var(--text-2xs);
		color: var(--text-muted);
		white-space: nowrap;
		pointer-events: none;
		text-shadow:
			0 0 2px var(--surface-panel),
			0 0 2px var(--surface-panel),
			0 0 3px var(--surface-panel);
	}
	.body {
		position: relative;
		touch-action: none;
		min-height: 100%;
	}
	.past {
		position: absolute;
		top: 0;
		bottom: 0;
		right: 0;
		background: var(--surface-inset);
		pointer-events: none;
	}
	.row {
		position: absolute;
		left: 0;
		right: 0;
		border-bottom: var(--line-width) solid var(--border-subtle);
		box-sizing: border-box;
	}
	/* 14 px drawn, 24 px to hit: the pseudo-element is part of the button for the pointer. */
	.key {
		position: absolute;
		z-index: 3;
		top: 50%;
		width: 14px;
		height: 14px;
		margin-top: -7px;
		padding: 0;
		border: none;
		background: none;
		cursor: ew-resize;
		touch-action: none;
	}
	.key::after {
		content: '';
		position: absolute;
		inset: -5px;
	}
	.key:focus-visible {
		outline: var(--focus-outline);
		outline-offset: 0;
	}
	.diamond {
		position: absolute;
		left: 3px;
		top: 3px;
		width: 8px;
		height: 8px;
		transform: rotate(45deg);
		background: var(--text-secondary);
		border: var(--line-width) solid var(--surface-panel);
		box-sizing: border-box;
	}
	.key.eased .diamond {
		border-radius: 50%;
		transform: none;
	}
	.key.held .diamond {
		border-radius: 0;
		transform: none;
	}
	.key:hover .diamond {
		background: var(--text-primary);
	}
	.key.on .diamond {
		background: var(--clip-accent);
		border-color: var(--kerf-400);
	}
	/* A speed key is outlined, not filled: it can be seen and clicked to go there, and it is not one of the numbers' keys. */
	.key.mark {
		cursor: pointer;
	}
	.key.mark .diamond {
		background: var(--surface-panel);
		border: var(--line-emphasis) solid var(--text-secondary);
	}
	.key.mark:hover .diamond {
		background: var(--surface-panel);
		border-color: var(--text-primary);
	}
	/* The ramp's row: its line (faint) and the speed beside each key, so a slowdown falls and a speed-up rises. */
	.rampline {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		overflow: visible;
		pointer-events: none;
	}
	.rampline path {
		fill: none;
		stroke: var(--text-muted);
		stroke-width: var(--line-width);
		stroke-linejoin: round;
		opacity: 0.7;
	}
	.rampline .one {
		stroke: var(--border-default);
		stroke-width: var(--line-width);
		stroke-dasharray: 2 3;
	}
	.rlabel {
		position: absolute;
		z-index: 1;
		font-family: var(--font-mono);
		font-size: var(--text-2xs);
		color: var(--text-muted);
		white-space: nowrap;
		pointer-events: none;
		text-shadow:
			0 0 2px var(--surface-panel),
			0 0 2px var(--surface-panel),
			0 0 3px var(--surface-panel);
	}
	.beyond {
		position: absolute;
		right: 4px;
		top: 4px;
		z-index: 1;
		font-family: var(--font-mono);
		font-size: var(--text-2xs);
		color: var(--text-muted);
		pointer-events: none;
	}
	.marquee {
		position: absolute;
		border: var(--line-width) solid var(--selection-border);
		background: var(--selection-fill);
		pointer-events: none;
	}
	.playhead {
		position: absolute;
		top: 0;
		bottom: 0;
		z-index: 2;
		width: var(--playhead-width);
		margin-left: calc(var(--playhead-width) / -2);
		background: var(--playhead);
		box-shadow: 0 0 0 var(--line-width) color-mix(in srgb, var(--surface-panel) 70%, transparent);
		pointer-events: none;
	}
</style>
