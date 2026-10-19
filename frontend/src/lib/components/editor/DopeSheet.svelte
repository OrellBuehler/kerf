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
	// The pure half is `dope-sheet.ts` (and `graph-editor.ts`).
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import Btn from './Btn.svelte';
	import EmptyState from './EmptyState.svelte';
	import EasingPopover from './EasingPopover.svelte';
	import DopeGraph from './DopeGraph.svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import { activeRow } from '#lib/graph-editor';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { settings } from '#lib/settings.svelte';
	import { contextMenu } from '#lib/context-menu.svelte';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { beginDrag } from '#lib/drag';
	import { resizeObserverFor } from '#lib/realm';
	import { holdForPreview, report } from '#lib/ops';
	import { tip } from '#lib/tip';
	import { allowsRepeat } from '#lib/keymap';
	import { rulerGrid, ticksIn, tickLabel } from '#lib/ruler';
	import { PROPERTIES } from '#lib/channels';
	import { clipDuration } from '#lib/types';
	import type { Clip, Easing, Property } from '#lib/types';
	import {
		allKeys,
		changes,
		copyKeys,
		deleteKeys,
		dopeKey,
		dopeRows,
		dragMagnets,
		easeKeys,
		easingTargets,
		isSelected,
		liveSelection,
		marqueeKeys,
		mergeSelection,
		pasteKeys,
		retimeKeys,
		rowLabel,
		sharedEasing,
		snapShift,
		timeAt,
		toggleKey,
		xOf,
		type Axis,
		type DopeEdit,
		type DopeRow,
		type KeyRef
	} from '#lib/dope-sheet';

	const ROW_H = 22;
	const RULER_H = 22;
	const PAD = 8;
	/** How near a magnet pulls a dragged key, px. */
	const REACH_PX = 6;
	/** Pointer travel that makes a press a drag, px. */
	const TRAVEL_PX = 3;

	const clip = $derived(editor.selectedClip);
	const rows = $derived(clip ? dopeRows(clip) : []);
	const duration = $derived(clip ? clipDuration(clip) : 0);
	const lastKey = $derived(Math.max(0, ...rows.flatMap((r) => r.keys.map((k) => k.time))));
	let width = $state(0);
	const axis = $derived<Axis>({ duration: Math.max(duration, lastKey, 1e-3), width, pad: PAD });
	const pxPerSec = $derived(Math.max(1, width - 2 * PAD) / axis.duration);
	const playhead = $derived(clip ? ui.time - clip.timeline_start : null);

	let selection = $state<KeyRef[]>([]);
	let selectedFor: string | null = null;
	$effect(() => {
		const id = clip?.id ?? null;
		if (id !== selectedFor) {
			selectedFor = id;
			selection = [];
			picked = null;
		}
	});
	const live = $derived(liveSelection(rows, selection));
	/** The row the graph draws with handles, when one was picked by its label. */
	let picked = $state<Property | null>(null);
	const active = $derived(activeRow(rows, live, picked));

	type Move = { kind: 'move'; clipId: string; base: KeyRef[]; grabbed: number; x0: number; moved: boolean; dt: number; alt: boolean };
	type Marquee = { kind: 'marquee'; base: KeyRef[]; x0: number; y0: number; x1: number; y1: number; moved: boolean };
	let gesture = $state<Move | Marquee | null>(null);
	/** A gesture's edit while it is being written: shown until it settles. */
	let writing = $state.raw<{ clipId: string; edit: DopeEdit } | null>(null);

	const ghost = $derived.by((): DopeEdit | null => {
		if (clip && gesture?.kind === 'move' && gesture.moved) return retimeKeys(clip, gesture.base, gesture.dt, gesture.alt);
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

	// ---- writing ---------------------------------------------------------------

	const clipById = (id: string): Clip | undefined => editor.timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === id);

	async function write(c: Clip, edit: DopeEdit) {
		if (edit.tracks.length === 0 || !changes(c, edit)) return;
		if (holdForPreview()) return;
		const mine = { clipId: c.id, edit };
		writing = mine;
		try {
			await editor.setPropertyTracks(c.id, edit.tracks);
			if (writing === mine) selection = edit.selected;
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
		void write(c, pasteKeys(c, board, Math.min(Math.max(playhead ?? 0, 0), duration)));
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

	// ---- the easing popover ------------------------------------------------------

	let popover = $state<{ x: number; y: number } | null>(null);
	let easingButton = $state<HTMLElement | null>(null);
	function openEasing(at: { x: number; y: number }) {
		if (targets.length === 0) return;
		popover = at;
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
		root?.focus({ preventScroll: true });
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
				disabled: !board,
				reason: board ? undefined : 'copy keys first',
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
		if (e.key === 'Escape') {
			if (selection.length === 0) return;
			selection = [];
		} else {
			const what = dopeKey(settings.actionFor(e), e.repeat, allowsRepeat);
			if (what === 'copy') copy();
			else if (what === 'cut') cut();
			else if (what === 'paste') paste();
			else if (what === 'delete') remove();
			else if (what === 'selectAll') selection = allKeys(rows);
			else if (what !== 'swallow') return;
		}
		e.preventDefault();
		e.stopPropagation();
	}

	const keyName = (label: string, t: number) => `${label} key at ${timeDisplay.fmt((clip?.timeline_start ?? 0) + t)}`;
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div class="dope" bind:this={root} tabindex="0" role="region" aria-label="Keyframes" {onkeydown} onkeyup={altChanged}>
	{#if !clip}
		<EmptyState icon="diamond" title="No clip selected" hint="Select a clip to see its keyframes, one row per animated number." />
	{:else}
		<div class="bar">
			<span class="name">{rows.length === 0 ? 'No keyframes' : `${rows.length} animated ${rows.length === 1 ? 'number' : 'numbers'}`}</span>
			<span class="sel">{live.length > 0 ? `${live.length} selected` : ''}</span>
			<span class="view">
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
			</span>
			<Btn
				size="sm"
				variant="ghost"
				icon="chevron-down"
				pending={busy}
				disabled={targets.length === 0}
				title={targets.length === 0 ? 'Select a key that starts a segment' : 'The shape of the segments the selected keys start'}
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
			<IconBtn size={24} title={settings.tip('Paste keys at the playhead', 'edit.paste')} pending={busy} disabled={!board} onclick={paste}
				><Icon n="clipboard" s={13} /></IconBtn
			>
			<IconBtn size={24} title={settings.tip('Delete keys', 'edit.delete')} pending={busy} disabled={live.length === 0} onclick={remove}
				><Icon n="trash" s={13} /></IconBtn
			>
		</div>
		{#if rows.length === 0 && !ghost}
			<EmptyState
				icon="diamond"
				title="This clip has no keyframes"
				hint="Key a number with the ◇ beside it in the Inspector, or paste keys copied from another clip."
				pad={20}
			/>
		{:else}
			<div class="sheet">
				<div class="labels" style="padding-top:{RULER_H}px">
					{#if view === 'graph'}
						{#each shown as r (r.prop)}
							<button
								type="button"
								class="label pick"
								class:active={r.prop === active}
								style="height:{ROW_H}px"
								aria-pressed={r.prop === active}
								onclick={() => (picked = r.prop)}>{r.label}</button
							>
						{/each}
					{:else}
						{#each shown as r (r.prop)}
							<div class="label" style="height:{ROW_H}px">{r.label}</div>
						{/each}
					{/if}
				</div>
				<div class="lanes" class:graph={view === 'graph'} bind:this={lanes}>
					<!-- svelte-ignore a11y_no_static_element_interactions -->
					<div class="ruler" style="height:{RULER_H}px" onpointerdown={rulerDown} aria-hidden="true">
						{#each ticks as t (t.k)}
							{@const x = xOf(axis, t.t - clip.timeline_start)}
							<span class="tick" style="left:{x}px"></span>
							<span class="tlabel" style="left:{x + 3}px">{tickLabel(t.k, grid, editor.fps, timeDisplay.mode)}</span>
						{/each}
					</div>
					{#if view === 'graph'}
						<DopeGraph
							{clip}
							rows={shown}
							{axis}
							{active}
							selection={shownSel}
							{duration}
							onpress={() => root?.focus({ preventScroll: true })}
							onpick={graphPick}
							onclear={graphClear}
							onwrite={(c, edit) => void write(c, edit)}
						/>
					{:else}
						<!-- svelte-ignore a11y_no_static_element_interactions -->
						<div class="body" bind:this={body} style="height:{shown.length * ROW_H}px" onpointerdown={laneDown}>
							{#if axis.duration > duration}
								<span class="past" style="left:{xOf(axis, duration)}px" aria-hidden="true"></span>
							{/if}
							{#each shown as r, i (r.prop)}
								<div class="row" style="top:{i * ROW_H}px;height:{ROW_H}px" role="group" aria-label={r.label}>
									{#each r.keys as k, j (j)}
										{@const on = isSelected(shownSel, r.prop, k.time)}
										<button
											type="button"
											class="key"
											class:on
											class:held={k.easing === 'hold'}
											class:eased={!!k.easing && k.easing !== 'hold'}
											style="left:{xOf(axis, k.time) - 7}px"
											aria-label={keyName(r.label, k.time)}
											aria-pressed={on}
											use:tip={keyName(r.label, k.time)}
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
		{/if}
	{/if}
	{#if popover && clip}
		<EasingPopover x={popover.x} y={popover.y} current={sharedEasing(clip, targets)} opener={easingButton} onpick={ease} onclose={() => (popover = null)} />
	{/if}
</div>

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
		font-size: 11px;
		color: var(--text-secondary);
	}
	.name {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.view {
		flex: none;
	}
	.sel {
		flex: 1;
		color: var(--text-muted);
		white-space: nowrap;
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
		width: 96px;
		border-right: var(--line-width) solid var(--border-subtle);
	}
	.label {
		display: flex;
		align-items: center;
		padding: 0 8px;
		font-size: 11px;
		color: var(--text-secondary);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		border-bottom: var(--line-width) solid var(--border-subtle);
		box-sizing: border-box;
	}
	.lanes {
		position: relative;
		flex: 1;
		min-width: 0;
	}
	.lanes.graph {
		display: flex;
		flex-direction: column;
	}
	.lanes.graph .ruler {
		flex: none;
	}
	.label.pick {
		width: 100%;
		border: none;
		border-bottom: var(--line-width) solid var(--border-subtle);
		background: none;
		text-align: left;
		cursor: pointer;
	}
	.label.pick:hover {
		color: var(--text-primary);
		background: var(--hover-fill);
	}
	.label.pick.active {
		color: var(--text-primary);
		box-shadow: inset 2px 0 0 var(--clip-accent);
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
	.tlabel {
		position: absolute;
		top: 3px;
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--text-muted);
		white-space: nowrap;
		pointer-events: none;
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
	.key {
		position: absolute;
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
		width: var(--playhead-width);
		margin-left: calc(var(--playhead-width) / -2);
		background: var(--playhead);
		pointer-events: none;
	}
</style>
