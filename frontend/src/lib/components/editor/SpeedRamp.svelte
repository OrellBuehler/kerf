<script lang="ts">
	// A clip's speed ramp: speed over the clip's length, drawn on a log axis (0.1x-10x at the least) with a
	// handle per key. A press on a handle selects it and, dragged, moves its time and speed; the gesture
	// writes ONE `set_speed_ramp` on release, shown as its ghost until then, and Escape, pointercancel or the
	// window losing focus abandon it. A focused handle's arrows nudge it (a frame, a tenth of a stop), Delete
	// removes it, and a double-click on empty graph adds a key there. *Add key at playhead* takes the speed the
	// clip runs at there, so the timing does not jump; the presets replace the ramp; Clear takes it away.
	// A write that changes nothing is not sent, and one on a clip that starts after a linked partner goes through
	// the earliest partner (`planRampWrite`), so the partner's own keys before this clip's start survive.
	// Keys are clip-local timeline seconds, like the engine's (`speed-ramp.ts` is the map they mean).
	import Btn from './Btn.svelte';
	import Chip from './Chip.svelte';
	import SectionHead from './SectionHead.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { report } from '#lib/ops';
	import { beginDrag } from '#lib/drag';
	import { resizeObserverFor } from '#lib/realm';
	import { snapToFrame } from '#lib/frames';
	import { tip } from '#lib/tip';
	import { toast } from '#lib/notifications.svelte';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { clipDuration, MAX_SPEED_KEYS, speedAt, speedMag } from '#lib/speed-ramp';
	import {
		addKeyAt,
		fmtSpeed,
		keysOf,
		moveKey,
		planRampWrite,
		presetKeys,
		RAMP_PRESETS,
		removeKey,
		sameKeys,
		setKeyEasing,
		speedAtY,
		speedLabel,
		speedPath,
		speedRange,
		speedTicks,
		timeAtX,
		timeSpan,
		xOfTime,
		yOfSpeed,
		type RampPreset,
		type SpeedAxis,
		type TimeAxis
	} from '#lib/speed-ramp-edit';
	import type { Clip, SpeedKey } from '#lib/types';

	let { clip, lockedWhy = undefined }: { clip: Clip; lockedWhy?: string } = $props();

	const PAD_Y = 12;
	const PAD_X = 10;
	/** Pointer travel that makes a press on a handle a drag, px. */
	const TRAVEL_PX = 2;
	/** A nudge of a focused handle's speed: a tenth of a stop. */
	const NUDGE = 1.1;

	let plot = $state<HTMLDivElement | null>(null);
	let width = $state(0);
	let height = $state(0);
	$effect(() => {
		if (!plot) return;
		const el = plot;
		const ro = resizeObserverFor(el, (entries) => {
			const box = entries.at(-1)?.contentRect;
			width = box?.width ?? el.clientWidth;
			height = box?.height ?? el.clientHeight;
		});
		ro.observe(el);
		return () => ro.disconnect();
	});

	/** The key the readout and the easing / delete buttons are about. */
	let sel = $state<number | null>(null);
	/** The keys while a drag moves one (its ghost), with the axes it began on — held, so the graph does not
	 *  rescale under the pointer; `null` between gestures. */
	let drag = $state.raw<{ keys: SpeedKey[]; span: number; range: [number, number] } | null>(null);

	const stored = $derived(keysOf(clip));
	const ramped = $derived(stored.length > 0);
	const keys = $derived(drag?.keys ?? stored);
	const constant = $derived(speedMag(clip));
	/** What the graph draws: the ramp, or the constant speed as a level line (no handles). */
	const curve = $derived<SpeedKey[]>(keys.length ? keys : [{ time: 0, speed: constant }]);
	const span = $derived(drag?.span ?? timeSpan(clip, stored));
	const range = $derived(drag?.range ?? speedRange(curve));
	const sAxis = $derived<SpeedAxis>({ lo: range[0], hi: range[1], height, pad: PAD_Y });
	const tAxis = $derived<TimeAxis>({ span, width, pad: PAD_X });
	const ticks = $derived(speedTicks(sAxis));
	const length = $derived(clipDuration(drag ? { ...clip, speed_ramp: drag.keys } : clip));
	const was = $derived(clipDuration(clip));
	const local = $derived(ui.time - clip.timeline_start);
	const atPlayhead = $derived(local >= 0 && local <= span);
	const now = $derived(speedAt({ ...clip, speed_ramp: keys.length ? keys : undefined }, Math.min(Math.max(local, 0), span)));
	const path = $derived(width > 0 && height > 0 ? speedPath(curve, span, (t) => xOfTime(tAxis, t), (v) => yOfSpeed(sAxis, v)) : '');
	const picked = $derived(sel !== null && sel < keys.length ? sel : null);
	const key = $derived(picked !== null ? keys[picked] : null);
	const full = $derived(stored.length >= MAX_SPEED_KEYS);

	const off = $derived(editor.locked || !!lockedWhy);
	const seconds = (s: number) => `${s.toFixed(2)} s`;
	const keyName = (k: SpeedKey, i: number) =>
		`Speed key ${i + 1} of ${keys.length}: ${fmtSpeed(k.speed)} at ${timeDisplay.fmt(clip.timeline_start + k.time)}${k.easing === 'hold' ? ', holds' : ''}`;

	async function write(next: SpeedKey[], then?: () => void) {
		if (off) return;
		const plan = planRampWrite(editor.timeline, clip.id, next);
		if (!plan) return;
		try {
			await editor.setSpeedRamp(plan.clipId, plan.keys);
			then?.();
		} catch (e) {
			report(e);
		}
	}

	const xIn = (e: { clientX: number }) => e.clientX - (plot?.getBoundingClientRect().left ?? 0);
	const yIn = (e: { clientY: number }) => e.clientY - (plot?.getBoundingClientRect().top ?? 0);
	const frame = (t: number) => Math.max(0, snapToFrame(t, editor.fps));

	function down(e: PointerEvent, i: number) {
		if (e.button !== 0) return;
		sel = i;
		if (off) return;
		const begin = stored.map((k) => ({ ...k }));
		const held = { span, range };
		const [x0, y0] = [e.clientX, e.clientY];
		let moved = false;
		beginDrag(e, {
			move(ev) {
				if (!moved && Math.hypot(ev.clientX - x0, ev.clientY - y0) < TRAVEL_PX) return;
				moved = true;
				const at = { span: held.span, width: tAxis.width, pad: PAD_X };
				const next = moveKey(begin, i, frame(timeAtX(at, xIn(ev))), speedAtY({ lo: held.range[0], hi: held.range[1], height, pad: PAD_Y }, yIn(ev)), held.span);
				drag = { keys: next, ...held };
			},
			commit() {
				const d = drag;
				drag = null;
				if (moved && d && !sameKeys(d.keys, begin)) void write(d.keys);
			},
			abandon() {
				drag = null;
			}
		});
	}

	function keyDown(e: KeyboardEvent, i: number) {
		if (e.ctrlKey || e.metaKey || e.altKey) return;
		const k = stored[i];
		if (!k) return;
		if (e.key === 'Delete' || e.key === 'Backspace') {
			e.preventDefault();
			e.stopPropagation();
			if (!e.repeat) remove(i);
			return;
		}
		const step: Record<string, [number, number]> = { ArrowLeft: [-1, 1], ArrowRight: [1, 1], ArrowUp: [0, NUDGE], ArrowDown: [0, 1 / NUDGE] };
		const s = step[e.key];
		if (!s || e.shiftKey) return;
		e.preventDefault();
		e.stopPropagation();
		if (e.repeat) return;
		const rate = Number.isFinite(editor.fps) && editor.fps > 0 ? editor.fps : 30;
		const next = moveKey(stored, i, k.time + s[0] / rate, k.speed * s[1], span);
		if (!sameKeys(next, stored)) void write(next);
	}

	function addAt(t: number) {
		if (off) return;
		const next = addKeyAt(clip, t);
		if (!next) {
			toast.info(full ? `A ramp holds at most ${MAX_SPEED_KEYS} keys` : 'There is a key there already');
			return;
		}
		void write(next, () => (sel = next.findIndex((k) => k.time === t)));
	}

	const addAtPlayhead = () => addAt(Math.min(Math.max(local, 0), was));

	function addAtPointer(e: MouseEvent) {
		if (e.target instanceof Element && e.target.closest('button')) return;
		addAt(frame(timeAtX(tAxis, xIn(e))));
	}

	function remove(i: number) {
		if (off) return;
		sel = null;
		void write(removeKey(stored, i));
	}

	function ease(easing: 'linear' | 'hold') {
		if (picked !== null && !off) void write(setKeyEasing(stored, picked, easing));
	}

	function preset(p: RampPreset) {
		if (off) return;
		sel = null;
		void write(presetKeys(p, clip, local));
	}

	function clear() {
		if (off) return;
		sel = null;
		void write([]);
	}
</script>

<SectionHead label="Speed ramp" />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="plot" class:off bind:this={plot} ondblclick={addAtPointer} data-speed-ramp>
	{#if width > 0 && height > 0}
		<svg class="lines" aria-hidden="true">
			{#each ticks as v (v)}
				<line class="grid" class:unity={v === 1} x1="0" x2="100%" y1={yOfSpeed(sAxis, v)} y2={yOfSpeed(sAxis, v)} />
			{/each}
			{#if span > was + 1e-9}
				<rect class="past" x={xOfTime(tAxis, was)} y="0" width={Math.max(0, xOfTime(tAxis, span) - xOfTime(tAxis, was))} height="100%" />
			{/if}
			<path class="curve" class:implied={!ramped && !drag} d={path} />
			{#if atPlayhead}
				<line class="head" x1={xOfTime(tAxis, local)} x2={xOfTime(tAxis, local)} y1="0" y2="100%" />
			{/if}
		</svg>
		{#each ticks as v (v)}
			<span class="vlabel" style="top:{yOfSpeed(sAxis, v)}px" aria-hidden="true">{speedLabel(v)}</span>
		{/each}
		{#if ramped || drag}
			{#each keys as k, i (i)}
				<button
					type="button"
					class="key"
					class:on={picked === i}
					class:hold={k.easing === 'hold'}
					style="left:{xOfTime(tAxis, k.time) - 6}px;top:{yOfSpeed(sAxis, k.speed) - 6}px"
					aria-label={keyName(k, i)}
					aria-pressed={picked === i}
					aria-disabled={off ? 'true' : undefined}
					use:tip={keyName(k, i)}
					data-key={i}
					onfocus={() => (sel = i)}
					onpointerdown={(e) => down(e, i)}
					onkeydown={(e) => keyDown(e, i)}
				></button>
			{/each}
		{/if}
	{/if}
</div>

<div class="readout" aria-live="polite">
	<span data-ramp-now>Now {fmtSpeed(now)}</span>
	<span class="sep" aria-hidden="true">·</span>
	<span data-ramp-length>
		{#if Math.abs(length - was) > 1e-6}Length {seconds(was)} → {seconds(length)}{:else}Length {seconds(length)}{/if}
	</span>
</div>

{#if !ramped}
	<div class="note" data-ramp-hint>
		Constant {fmtSpeed(constant)}. Add a key at the playhead, double-click the graph, or start from a preset.
	</div>
{:else if key && picked !== null}
	<div class="key-row" data-ramp-key>
		<span class="what">Key {picked + 1} · {seconds(key.time)} · {fmtSpeed(key.speed)}</span>
		<div role="group" aria-label="How the speed leaves this key" class="easing">
			<Chip pressed={key.easing !== 'hold'} pending={off} disabled={picked === keys.length - 1} title="The speed moves in a straight line to the next key" onclick={() => ease('linear')}>
				Linear
			</Chip>
			<Chip pressed={key.easing === 'hold'} pending={off} disabled={picked === keys.length - 1} title="The speed stays at this key's until the next one, then jumps" onclick={() => ease('hold')}>
				Hold
			</Chip>
		</div>
		<Btn size="sm" variant="ghost" icon="trash" iconSize={12} pending={off} aria-label="Delete key {picked + 1}" title="Delete this key" onclick={() => remove(picked)}>
			Delete
		</Btn>
	</div>
{/if}

<div class="actions">
	<Btn size="sm" icon="diamond" iconSize={12} pending={off || full} title={full ? `A ramp holds at most ${MAX_SPEED_KEYS} keys` : 'A key at the playhead, at the speed the clip runs there now'} onclick={addAtPlayhead} data-add-key>
		Add key at playhead
	</Btn>
	<Btn size="sm" variant="ghost" pending={off || !ramped} title="Back to the clip's constant speed" onclick={clear} data-clear-ramp>
		Clear ramp
	</Btn>
</div>
<div class="presets" role="group" aria-label="Speed ramp presets">
	{#each RAMP_PRESETS as p (p.id)}
		<Btn size="sm" pending={off} title={p.hint} onclick={() => preset(p)} data-preset={p.id}>{p.label}</Btn>
	{/each}
</div>

<style>
	.plot {
		position: relative;
		height: 128px;
		overflow: hidden;
		touch-action: none;
		background: var(--surface-inset);
		border: var(--line-width) solid var(--border-strong);
		border-radius: var(--radius-sm);
	}
	.plot.off {
		opacity: 0.7;
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
		stroke: var(--timeline-grid-major);
		stroke-width: 1;
	}
	.grid.unity {
		stroke: var(--border-default);
	}
	.past {
		fill: var(--surface-app);
		opacity: 0.5;
	}
	.curve {
		fill: none;
		stroke: var(--clip-accent);
		stroke-width: 1.5;
		stroke-linejoin: round;
	}
	.curve.implied {
		stroke: var(--text-muted);
		stroke-dasharray: 4 3;
	}
	.head {
		stroke: var(--playhead);
		stroke-width: 1;
	}
	.vlabel {
		position: absolute;
		right: 4px;
		margin-top: -12px;
		font-family: var(--font-mono);
		font-size: var(--text-2xs);
		color: var(--text-muted);
		pointer-events: none;
	}
	.key {
		position: absolute;
		z-index: 1;
		width: 12px;
		height: 12px;
		padding: 0;
		border: none;
		background: none;
		cursor: move;
		touch-action: none;
	}
	.key::before {
		content: '';
		position: absolute;
		left: 2px;
		top: 2px;
		width: 8px;
		height: 8px;
		box-sizing: border-box;
		transform: rotate(45deg);
		background: var(--text-secondary);
		border: var(--line-width) solid var(--surface-panel);
	}
	.key.hold::before {
		transform: none;
	}
	.key:hover::before {
		background: var(--text-primary);
	}
	.key.on::before {
		background: var(--clip-accent);
		border-color: var(--kerf-400);
	}
	.key:focus-visible {
		outline: var(--focus-outline);
		outline-offset: 0;
	}
	.readout {
		display: flex;
		gap: 6px;
		margin-top: 6px;
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		color: var(--text-secondary);
	}
	.sep {
		color: var(--text-muted);
	}
	.note {
		margin-top: 6px;
		font-size: var(--text-sm);
		line-height: 1.4;
		color: var(--text-muted);
	}
	.key-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 6px 8px;
		margin-top: 8px;
	}
	.what {
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		color: var(--text-secondary);
		margin-right: auto;
	}
	.easing {
		display: flex;
		gap: 4px;
	}
	.actions,
	.presets {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		margin-top: 8px;
	}
</style>
