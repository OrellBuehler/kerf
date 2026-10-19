<script lang="ts">
	// The dope sheet's graph view: each keyed number's curve on the sheet's time axis, the active row's
	// in full on its own value axis with a handle at each control point of the segments it eases, the
	// others faint, each on its own range. Dragging a handle reshapes that segment's bezier (held inside
	// the segment's box: no overshoot) and writes one edit on release, shown as its ghost until then;
	// Escape, pointercancel or the window losing focus abandon it. Keys select (and seek) as in the
	// sheet. The pure half is `graph-editor.ts`.
	import { beginDrag } from '#lib/drag';
	import { resizeObserverFor } from '#lib/realm';
	import { tip } from '#lib/tip';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { timeAt, xOf, isSelected, rowLabel, type Axis, type DopeEdit, type DopeRow, type KeyRef } from '#lib/dope-sheet';
	import {
		curvePath,
		dragHandle,
		handleEdit,
		handlePoint,
		segments,
		tickText,
		valueAt,
		valueRange,
		valueTicks,
		yOf,
		type Handle,
		type Segment,
		type ValueAxis
	} from '#lib/graph-editor';
	import type { Clip, Easing, Property } from '#lib/types';

	let {
		clip,
		rows,
		axis,
		active,
		selection,
		duration,
		onpress,
		onpick,
		onclear,
		onwrite
	}: {
		clip: Clip;
		/** The rows as the sheet shows them (its pending edit folded in). */
		rows: DopeRow[];
		axis: Axis;
		/** The row drawn with its handles. */
		active: Property | null;
		selection: KeyRef[];
		/** The clip's own length: past it the axis is shaded. */
		duration: number;
		/** Any press: the panel takes the keys. */
		onpress: () => void;
		/** A key clicked: select it (`additive` toggles it) and, alone, seek to it. */
		onpick: (prop: Property, time: number, additive: boolean) => void;
		/** A press on empty space. */
		onclear: (additive: boolean) => void;
		/** A gesture's edit, on release. */
		onwrite: (c: Clip, edit: DopeEdit) => void;
	} = $props();

	const PAD = 12;
	const HANDLES: Handle[] = [1, 2];
	/** Pointer travel that makes a press on a handle a drag, px. */
	const TRAVEL_PX = 2;

	let height = $state(0);
	let plot = $state<HTMLDivElement | null>(null);
	$effect(() => {
		if (!plot) return;
		const el = plot;
		const ro = resizeObserverFor(el, (entries) => (height = entries.at(-1)?.contentRect.height ?? el.clientHeight));
		ro.observe(el);
		return () => ro.disconnect();
	});

	type Drag = { seg: Segment; which: Handle; easing: Easing; moved: boolean };
	let drag = $state<Drag | null>(null);

	/** The rows with the handle drag's ghost on the active one. */
	const drawn = $derived.by((): DopeRow[] => {
		const d = drag;
		if (!d?.moved) return rows;
		const edit = handleEdit(clip, d.seg, d.easing);
		return rows.map((r) => {
			const t = edit.tracks.find((x) => x.prop === r.prop);
			return t ? { ...r, keys: t.keys } : r;
		});
	});
	const activeRow = $derived(drawn.find((r) => r.prop === active) ?? null);
	const vaOf = (r: DopeRow): ValueAxis => {
		const [lo, hi] = valueRange(r.keys);
		return { lo, hi, height, pad: PAD };
	};
	// The value axis follows the row as it is, not the ghost (a handle cannot move a key's value).
	const va = $derived.by((): ValueAxis | null => {
		const r = rows.find((x) => x.prop === active);
		return r ? vaOf(r) : null;
	});
	const ticks = $derived(va && height > 0 ? valueTicks(va.lo, va.hi, Math.max(2, Math.floor(height / 40))) : []);
	const segs = $derived(activeRow ? segments(activeRow.prop, activeRow.keys) : []);
	const end = $derived(axis.duration);
	const x = (t: number) => xOf(axis, t);

	const xIn = (e: { clientX: number }) => e.clientX - (plot?.getBoundingClientRect().left ?? 0);
	const yIn = (e: { clientY: number }) => e.clientY - (plot?.getBoundingClientRect().top ?? 0);

	function handleDown(e: PointerEvent, seg: Segment, which: Handle) {
		if (!va) return;
		const v = va;
		const c = clip;
		const x0 = e.clientX;
		const y0 = e.clientY;
		let d: Drag = { seg, which, easing: seg.easing, moved: false };
		drag = d;
		beginDrag(e, {
			move(ev) {
				if (!d.moved && Math.hypot(ev.clientX - x0, ev.clientY - y0) < TRAVEL_PX) return;
				d = { ...d, moved: true, easing: dragHandle(seg, which, timeAt(axis, xIn(ev)), valueAt(v, yIn(ev))) };
				drag = d;
			},
			commit() {
				drag = null;
				if (d.moved) onwrite(c, handleEdit(c, seg, d.easing));
			},
			abandon() {
				drag = null;
			}
		});
	}

	/** A focused handle's arrows: a twentieth of the segment's box a press, one edit each (a held key
	 *  does not repeat). Up is up on the screen, whichever way the segment goes. */
	function handleKey(e: KeyboardEvent, seg: Segment, which: Handle) {
		const step = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, 1], ArrowDown: [0, -1] }[e.key];
		if (!step) return;
		e.preventDefault();
		e.stopPropagation();
		const p = handlePoint(seg, which);
		if (!p || e.repeat) return;
		const t = p[0] + (step[0] * (seg.t1 - seg.t0)) / 20;
		const v = p[1] + (step[1] * Math.abs(seg.v1 - seg.v0)) / 20;
		onwrite(clip, handleEdit(clip, seg, dragHandle(seg, which, t, v)));
	}

	function plotDown(e: PointerEvent) {
		if (e.button !== 0) return;
		e.preventDefault();
		onpress();
		const additive = e.shiftKey || e.ctrlKey || e.metaKey;
		const el = e.target instanceof Element ? e.target : null;
		const h = el?.closest<HTMLElement>('[data-handle]');
		if (h) {
			const seg = segs[Number(h.dataset.seg)];
			if (seg) handleDown(e, seg, Number(h.dataset.handle) as Handle);
			return;
		}
		const key = el?.closest<HTMLElement>('[data-prop]');
		if (key) {
			onpick(key.dataset.prop as Property, Number(key.dataset.time), additive);
			return;
		}
		onclear(additive);
	}

	const keyName = (label: string, t: number) => `${label} key at ${timeDisplay.fmt(clip.timeline_start + t)}`;
	const handleName = (seg: Segment, which: Handle) =>
		`${rowLabel(seg.prop)}: ${which === 1 ? 'leaving' : 'arriving'} handle of the segment from ${timeDisplay.fmt(clip.timeline_start + seg.t0)}`;
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="plot" bind:this={plot} onpointerdown={plotDown}>
	{#if va && height > 0}
		{#if end > duration}
			<span class="past" style="left:{x(duration)}px" aria-hidden="true"></span>
		{/if}
		<svg class="lines" aria-hidden="true">
			{#each ticks as v (v)}
				<line class="grid" x1="0" x2="100%" y1={yOf(va, v)} y2={yOf(va, v)} />
			{/each}
			{#each drawn as r (r.prop)}
				{#if r.prop !== active}
					{@const rva = vaOf(r)}
					<path class="curve other" d={curvePath(r.keys, x, (v) => yOf(rva, v), 0, end)} />
				{/if}
			{/each}
			{#if activeRow}
				{#each segs as seg, i (i)}
					{#each HANDLES as which (which)}
						{@const p = handlePoint(seg, which, seg.easing)}
						{#if p}
							{@const from = which === 1 ? [seg.t0, seg.v0] : [seg.t1, seg.v1]}
							<line class="arm" x1={x(from[0])} y1={yOf(va, from[1])} x2={x(p[0])} y2={yOf(va, p[1])} />
						{/if}
					{/each}
				{/each}
				<path class="curve" d={curvePath(activeRow.keys, x, (v) => yOf(va, v), 0, end)} />
			{/if}
		</svg>
		{#each ticks as v (v)}
			<span class="vlabel" style="top:{yOf(va, v)}px" aria-hidden="true">{tickText(v, ticks)}</span>
		{/each}
		{#if activeRow}
			{#each segs as seg, i (i)}
				{#each HANDLES as which (which)}
					{@const p = handlePoint(seg, which, seg.easing)}
					{#if p}
						<button
							type="button"
							class="handle"
							class:on={drag?.seg.t0 === seg.t0 && drag.which === which}
							style="left:{x(p[0]) - 6}px;top:{yOf(va, p[1]) - 6}px"
							aria-label={handleName(seg, which)}
							use:tip={handleName(seg, which)}
							data-seg={i}
							data-handle={which}
							onkeydown={(e) => handleKey(e, seg, which)}
						></button>
					{/if}
				{/each}
			{/each}
			{#each activeRow.keys as k, j (j)}
				{@const on = isSelected(selection, activeRow.prop, k.time)}
				<button
					type="button"
					class="key"
					class:on
					style="left:{x(k.time) - 6}px;top:{yOf(va, k.value) - 6}px"
					aria-label={keyName(activeRow.label, k.time)}
					aria-pressed={on}
					use:tip={keyName(activeRow.label, k.time)}
					data-prop={activeRow.prop}
					data-time={String(k.time)}
					onclick={(e) => {
						if (e.detail === 0) onpick(activeRow.prop, k.time, true);
					}}
				></button>
			{/each}
		{/if}
	{/if}
</div>

<style>
	.plot {
		position: relative;
		flex: 1;
		min-height: 120px;
		overflow: hidden;
		touch-action: none;
	}
	.past {
		position: absolute;
		top: 0;
		bottom: 0;
		right: 0;
		background: var(--surface-inset);
		pointer-events: none;
	}
	.lines {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		pointer-events: none;
		overflow: visible;
	}
	.grid {
		stroke: var(--timeline-grid);
		stroke-width: 1;
	}
	.curve {
		fill: none;
		stroke: var(--clip-accent);
		stroke-width: 1.5;
		stroke-linejoin: round;
	}
	.curve.other {
		stroke: var(--text-muted);
		stroke-width: 1;
		opacity: 0.6;
	}
	.arm {
		stroke: var(--text-secondary);
		stroke-width: 1;
	}
	.vlabel {
		position: absolute;
		right: 4px;
		margin-top: -12px;
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--text-muted);
		pointer-events: none;
	}
	.key,
	.handle {
		position: absolute;
		width: 12px;
		height: 12px;
		padding: 0;
		border: none;
		background: none;
		touch-action: none;
	}
	.key::before,
	.handle::before {
		content: '';
		position: absolute;
		left: 2px;
		top: 2px;
		width: 8px;
		height: 8px;
		box-sizing: border-box;
	}
	.key {
		cursor: pointer;
	}
	.key::before {
		transform: rotate(45deg);
		background: var(--text-secondary);
		border: var(--line-width) solid var(--surface-panel);
	}
	.key:hover::before {
		background: var(--text-primary);
	}
	.key.on::before {
		background: var(--clip-accent);
		border-color: var(--kerf-400);
	}
	.handle {
		cursor: move;
	}
	.handle::before {
		border-radius: 50%;
		background: var(--surface-panel);
		border: 1.5px solid var(--kerf-400);
	}
	.handle:hover::before,
	.handle.on::before {
		background: var(--kerf-400);
	}
	.key:focus-visible,
	.handle:focus-visible {
		outline: var(--focus-outline);
		outline-offset: 0;
	}
</style>
