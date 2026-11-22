<script lang="ts">
	// The curve editor of the grade: a square canvas that fills the Inspector's column (up to a size), a channel at a time,
	// with the others drawn faint behind it. Click adds a point and drag moves it, double-click or right-click takes one out; with
	// the canvas focused the arrow keys move the selected point (Shift five steps, Alt a tenth), Page Up and Page Down pick another,
	// Delete takes it out and Enter adds one; under it the selected point's In and Out are numbers to type. Each gesture writes one
	// edit when it ends (a run of keys when they go quiet); Escape, pointercancel or a lost focus abandon one in flight. What is
	// written is the channel of the clip the gesture *began* on. The curve is `grade.ts`'s, the TS mirror of the engine's cubic, so
	// what is drawn is what the export renders; the pointer, keys and hit-testing are `curve-edit.ts`'s, which has the tests.
	import Btn from './Btn.svelte';
	import Chip from './Chip.svelte';
	import GradeField from './GradeField.svelte';
	import { beginDrag } from '#lib/drag';
	import {
		CURVE_CHANNELS,
		IDENTITY_POINTS,
		addPoint,
		curveShape,
		evalShape,
		movePoint,
		normalizeCurve,
		normalizeGrade,
		type CurveChannel
	} from '#lib/grade';
	import {
		addAfter,
		afterRemoval,
		curveKey,
		curveSide,
		nudgePoint,
		pickPoint,
		plotRect,
		pointWords,
		pxOf,
		removePoint,
		selectPoint,
		setPoint,
		unitAt
	} from '#lib/curve-edit';
	import { KeyRun } from '#lib/key-run';
	import { settings } from '#lib/settings.svelte';
	import { tip } from '#lib/tip';
	import { resolveColor } from '#lib/waveform-draw';
	import type { Clip, CurvePoint } from '#lib/types';
	import { onDestroy } from 'svelte';

	let {
		clip,
		locked = false,
		onChange
	}: {
		clip: Clip;
		/** An edit is in flight, or the proposal is on screen: the controls are held (they keep focus). */
		locked?: boolean;
		/** Change one channel of clip `clipId`'s curves as the cut has them now: `change` is given its points (the identity's two ends
		 *  when it has none) and returns the ones to write, or `null` for no change. */
		onChange: (clipId: string, channel: CurveChannel, change: (points: CurvePoint[]) => CurvePoint[] | null) => Promise<unknown>;
	} = $props();

	/** The canvas's largest side, px, and the margin the square is drawn inside so a point on its edge is a whole circle. */
	const MAX_SIDE = 288;
	const PAD = 7;
	/** The channels' hues are the scopes' (`--scope-*`: red, green and blue as a theme has them), not the status colours:
	 *  the info hue is cyan in the default theme, and a blue curve is blue. */
	const CHANNEL_TOKEN: Record<CurveChannel, string> = {
		master: '--text-primary',
		red: '--scope-red',
		green: '--scope-green',
		blue: '--scope-blue'
	};
	const CHANNEL_LABEL: Record<CurveChannel, string> = { master: 'Master', red: 'Red', green: 'Green', blue: 'Blue' };
	const POINT_SPEC = { min: 0, max: 1, step: 0.01, digits: 2 } as const;
	const uid = $props.id();

	let channel = $state<CurveChannel>('master');
	/** The points of one channel while a gesture moves them, and the clip it began on; `null` between gestures. */
	let draft = $state.raw<{ clipId: string; channel: CurveChannel; points: CurvePoint[] } | null>(null);
	/** The selected point: an index into the channel's points, kept for the clip and channel it was picked on. */
	let pick = $state<{ key: string; index: number } | null>(null);
	let canvas = $state<HTMLCanvasElement | null>(null);
	let avail = $state(0);
	let focused = $state(false);
	let dragging = false;

	const side = $derived(curveSide(avail, MAX_SIDE));
	const grade = $derived(normalizeGrade(clip.grade));
	const stored = (ch: CurveChannel): CurvePoint[] => grade.curves?.[ch] ?? [];
	const edited = (ch: CurveChannel) => stored(ch).length > 0;
	const pointsOf = (ch: CurveChannel): CurvePoint[] => {
		const p = draft && draft.clipId === clip.id && draft.channel === ch ? draft.points : stored(ch);
		return p.length ? p : IDENTITY_POINTS;
	};
	const points = $derived(pointsOf(channel));
	const here = $derived(`${clip.id}:${channel}`);
	const selected = $derived(pick && pick.key === here && pick.index < points.length ? pick.index : null);
	const select = (index: number | null) => (pick = index === null ? null : { key: here, index });

	// ---- drawing -----------------------------------------------------------------

	const palette = $derived.by(() => {
		void settings.theme;
		const el = canvas;
		if (!el) return null;
		return {
			bg: resolveColor('--surface-inset', el),
			grid: resolveColor('--border-default', el),
			axis: resolveColor('--border-strong', el),
			diagonal: resolveColor('--text-muted', el),
			point: resolveColor('--surface-panel', el),
			channel: Object.fromEntries(CURVE_CHANNELS.map((c) => [c, resolveColor(CHANNEL_TOKEN[c], el)])) as Record<CurveChannel, string>
		};
	});

	$effect(() => {
		const el = canvas;
		const p = palette;
		if (!el || !p) return;
		const size = side;
		const span = size - 2 * PAD;
		const X = (x: number) => PAD + x * span;
		const Y = (y: number) => PAD + (1 - y) * span;
		const dpr = el.ownerDocument.defaultView?.devicePixelRatio || 1;
		el.width = Math.round(size * dpr);
		el.height = Math.round(size * dpr);
		const ctx = el.getContext('2d');
		if (!ctx) return;
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		ctx.fillStyle = p.bg;
		ctx.fillRect(0, 0, size, size);
		// the grid: the square and its quarters, with the middle a step stronger
		ctx.lineWidth = 1;
		ctx.strokeStyle = p.grid;
		ctx.strokeRect(Math.round(PAD) + 0.5, Math.round(PAD) + 0.5, Math.round(span), Math.round(span));
		for (let i = 1; i < 4; i++) {
			ctx.strokeStyle = i === 2 ? p.axis : p.grid;
			const v = Math.round(PAD + (i * span) / 4) + 0.5;
			ctx.beginPath();
			ctx.moveTo(v, PAD);
			ctx.lineTo(v, size - PAD);
			ctx.moveTo(PAD, v);
			ctx.lineTo(size - PAD, v);
			ctx.stroke();
		}
		// the identity: what a curve with no points does, dashed so a curve laid over it still reads
		ctx.save();
		ctx.setLineDash([4, 4]);
		ctx.strokeStyle = p.diagonal;
		ctx.lineWidth = 1;
		ctx.beginPath();
		ctx.moveTo(X(0), Y(0));
		ctx.lineTo(X(1), Y(1));
		ctx.stroke();
		ctx.restore();
		for (const ch of [...CURVE_CHANNELS.filter((c) => c !== channel), channel]) {
			const shape = curveShape(pointsOf(ch));
			if (!shape && ch !== channel) continue;
			ctx.globalAlpha = ch === channel ? 1 : 0.4;
			ctx.strokeStyle = p.channel[ch];
			ctx.lineWidth = ch === channel ? 2 : 1.5;
			ctx.beginPath();
			const steps = Math.max(1, Math.round(span));
			for (let i = 0; i <= steps; i++) {
				const x = i / steps;
				const y = shape ? evalShape(shape, x) : x;
				if (i === 0) ctx.moveTo(X(x), Y(y));
				else ctx.lineTo(X(x), Y(y));
			}
			ctx.stroke();
		}
		ctx.globalAlpha = 1;
		const own = p.channel[channel];
		if (selected !== null) {
			// guides from the selected point to the two axes
			const s = pxOf(points[selected], size, PAD);
			ctx.save();
			ctx.setLineDash([2, 3]);
			ctx.strokeStyle = own;
			ctx.globalAlpha = 0.6;
			ctx.lineWidth = 1;
			ctx.beginPath();
			ctx.moveTo(s.x, s.y);
			ctx.lineTo(s.x, size - PAD);
			ctx.moveTo(s.x, s.y);
			ctx.lineTo(PAD, s.y);
			ctx.stroke();
			ctx.restore();
		}
		points.forEach((pt, i) => {
			const c = pxOf(pt, size, PAD);
			const on = i === selected;
			ctx.beginPath();
			ctx.arc(c.x, c.y, on ? 5 : 4, 0, Math.PI * 2);
			ctx.fillStyle = on ? own : p.point;
			ctx.fill();
			ctx.strokeStyle = on ? p.point : own;
			ctx.lineWidth = 2;
			ctx.stroke();
			if (on) {
				ctx.beginPath();
				ctx.arc(c.x, c.y, 7.5, 0, Math.PI * 2);
				ctx.strokeStyle = own;
				ctx.lineWidth = 1.5;
				ctx.stroke();
			}
		});
	});

	// ---- writing ------------------------------------------------------------------

	/** Write a change to a channel of the clip `to`; the draft ends once the edit has landed, so the point does not step back in between. */
	async function put(to: string, ch: CurveChannel, change: (points: CurvePoint[]) => CurvePoint[] | null) {
		const mine = draft;
		try {
			await onChange(to, ch, change);
		} finally {
			if (draft === mine) draft = null;
		}
	}
	const absolute = (to: CurvePoint[]) => () => to;

	// ---- the keys -----------------------------------------------------------------

	let runAt = { clipId: '', channel: 'master' as CurveChannel };
	const run = new KeyRun<CurvePoint[]>((from, last) => {
		const same = JSON.stringify(normalizeCurve(from)) === JSON.stringify(normalizeCurve(last));
		if (same) {
			draft = null;
			return;
		}
		void put(runAt.clipId, runAt.channel, absolute(last));
	});
	onDestroy(() => {
		run.finish();
		settle(false);
	});

	function onkeydown(e: KeyboardEvent) {
		if (locked || dragging) return;
		if (e.key === 'Escape') {
			// Only a run in flight is Escape's: with none, it falls through to the page (which clears the selection).
			if (run.cancel()) {
				draft = null;
				e.preventDefault();
				e.stopPropagation();
			}
			return;
		}
		if (e.ctrlKey || e.metaKey) {
			// A run not yet written out is the newest change: undo takes it back rather than the edit before it.
			if (run.active && e.key.toLowerCase() === 'z' && !e.shiftKey) {
				run.cancel();
				draft = null;
				e.preventDefault();
				e.stopPropagation();
			} else run.finish();
			return;
		}
		const key = curveKey(e.key, { shift: e.shiftKey, alt: e.altKey });
		if (!key) return;
		e.preventDefault();
		const base = run.last ?? points;
		switch (key.kind) {
			case 'select':
				run.finish();
				select(selectPoint(selected, points.length, key.to));
				break;
			case 'move': {
				const i = selected ?? selectPoint(null, base.length, 'first');
				if (i === null) break;
				if (selected === null) select(i);
				if (!run.active) runAt = { clipId: clip.id, channel };
				const next = nudgePoint(base, i, key.dx, key.dy, key.size);
				run.push(points, next);
				draft = { clipId: runAt.clipId, channel: runAt.channel, points: next };
				break;
			}
			case 'remove':
				if (run.active) run.finish();
				if (selected !== null) removeSelected(selected);
				break;
			case 'add': {
				if (run.active) {
					run.finish();
					break;
				}
				const added = addAfter(points, selected ?? 0);
				if (!added) break;
				select(added.index);
				draft = { clipId: clip.id, channel, points: added.points };
				void put(clip.id, channel, absolute(added.points));
				break;
			}
		}
	}

	function removeSelected(index: number) {
		const [to, ch] = [clip.id, channel];
		select(afterRemoval(index, points.length));
		settle(true);
		draft = null;
		void put(to, ch, (cur) => removePoint(cur, index));
	}

	// ---- the pointer --------------------------------------------------------------

	/** A click that only added a point waits this long for the second click of a double-click, which takes it out again, so
	 *  a double-click writes no edit and its opposite. */
	const DOUBLE_CLICK_MS = 300;
	let waiting: { timer: ReturnType<typeof setTimeout>; flush: () => void } | null = null;
	function defer(flush: () => void) {
		settle(true);
		const timer = setTimeout(() => {
			waiting = null;
			flush();
		}, DOUBLE_CLICK_MS);
		waiting = { timer, flush };
	}
	/** Drops the click that is waiting (it was the first of a double-click), or writes it now. Whether there was one. */
	function settle(drop: boolean): boolean {
		const w = waiting;
		if (!w) return false;
		clearTimeout(w.timer);
		waiting = null;
		if (!drop) w.flush();
		return true;
	}

	function down(e: PointerEvent) {
		if (e.button !== 0 || locked) return;
		const el = e.currentTarget as HTMLCanvasElement;
		run.finish();
		const [to, ch, before] = [clip.id, channel, selected];
		// The second click of a double-click lands on the point the first one is still waiting to add.
		const carried = settle(true);
		const start = pointsOf(ch);
		const p = unitAt(plotRect(el.getBoundingClientRect(), PAD), e.clientX, e.clientY);
		let index = pickPoint(start, p.x, p.y, side - 2 * PAD);
		let current = start;
		let added = carried;
		if (index < 0) {
			const next = addPoint(start, p.x, p.y);
			if (!next) {
				el.focus({ preventScroll: true });
				return;
			}
			current = next.points;
			index = next.index;
			added = true;
		}
		select(index);
		draft = { clipId: to, channel: ch, points: current };
		dragging = true;
		let moved = false;
		beginDrag(e, {
			move(ev) {
				moved = true;
				const q = unitAt(plotRect(el.getBoundingClientRect(), PAD), ev.clientX, ev.clientY);
				draft = { clipId: to, channel: ch, points: movePoint(current, index, q.x, q.y) };
			},
			commit() {
				dragging = false;
				const done = draft?.points ?? current;
				if (!moved && added) {
					defer(() => void put(to, ch, absolute(done)));
					return;
				}
				if (!moved) {
					draft = null;
					return;
				}
				void put(to, ch, absolute(done));
			},
			abandon() {
				dragging = false;
				draft = null;
				select(before);
			}
		});
		el.focus({ preventScroll: true });
	}

	/** A double-click or a right-click on a point takes it out. */
	function removeAt(e: MouseEvent) {
		if (locked) return;
		const el = e.currentTarget as HTMLCanvasElement;
		const p = unitAt(plotRect(el.getBoundingClientRect(), PAD), e.clientX, e.clientY);
		const i = pickPoint(points, p.x, p.y, side - 2 * PAD);
		if (i < 0) return;
		e.preventDefault();
		e.stopPropagation();
		run.cancel();
		removeSelected(i);
	}

	// ---- the point's numbers --------------------------------------------------------

	const scope = $derived({ clipId: clip.id, channel, index: selected });
	type PointScope = typeof scope;
	function setNumber(v: number, s: PointScope, to: 'x' | 'y') {
		const i = s.index;
		if (i === null) return;
		return put(s.clipId, s.channel, (cur) => setPoint(cur, i, to === 'x' ? { x: v } : { y: v }));
	}
</script>

<div class="curve">
	<div class="channels" role="group" aria-label="Curve channel">
		{#each CURVE_CHANNELS as ch (ch)}
			<Chip pressed={channel === ch} onclick={() => (channel = ch)}>
				{CHANNEL_LABEL[ch]}
				{#if edited(ch)}
					<span class="dot" style="background:var({CHANNEL_TOKEN[ch]})" aria-hidden="true"></span>
					<span class="vh">, has points</span>
				{/if}
			</Chip>
		{/each}
		<Btn
			size="sm"
			pending={locked}
			disabled={!edited(channel)}
			title={`Straighten the ${CHANNEL_LABEL[channel].toLowerCase()} curve`}
			style="margin-left:auto"
			onclick={() => {
				settle(true);
				select(null);
				void put(clip.id, channel, () => []);
			}}>Reset</Btn
		>
	</div>
	<div class="body">
		<div class="frame" bind:clientWidth={avail}>
			<!-- The wrapper is what takes focus and the keys (an application: it keeps the arrows for itself); the canvas under it is a
			     picture that takes the pointer. The same two ignores as the dope sheet's keyboard surface. -->
			<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
			<div
				class="stage"
				data-curve-editor
				tabindex="0"
				role="application"
				aria-roledescription="curve editor"
				aria-label="{CHANNEL_LABEL[channel]} curve, {edited(channel) ? `${points.length} points` : 'a straight line'}"
				aria-describedby="{uid}-keys"
				style="width:{side}px;height:{side}px"
				use:tip={{
					text: `${CHANNEL_LABEL[channel]} curve`,
					detail:
						'Click to add a point, drag to move it, double-click to take it out.\nArrows move the selected point (Shift bigger steps, Alt finer), Page Up and Page Down pick another, Delete takes it out, Enter adds one.'
				}}
				{onkeydown}
				onfocus={() => (focused = true)}
				onblur={() => {
					focused = false;
					run.finish();
				}}
			>
				<canvas bind:this={canvas} aria-hidden="true" onpointerdown={down} ondblclick={removeAt} oncontextmenu={removeAt}></canvas>
			</div>
		</div>
		<div class="point" role="group" aria-label="Selected point">
			<GradeField
				label="In"
				name="{CHANNEL_LABEL[channel]} curve point, input"
				hint="Where the selected point is along the input, 0 to 1"
				value={selected === null ? 0 : points[selected].x}
				spec={POINT_SPEC}
				{scope}
				blank={selected === null}
				disabled={locked}
				onCommit={(v, s) => setNumber(v, s, 'x')}
			/>
			<GradeField
				label="Out"
				name="{CHANNEL_LABEL[channel]} curve point, output"
				hint="What the selected point makes of its input, 0 to 1"
				value={selected === null ? 0 : points[selected].y}
				spec={POINT_SPEC}
				{scope}
				blank={selected === null}
				disabled={locked}
				onCommit={(v, s) => setNumber(v, s, 'y')}
			/>
			<Btn
				size="sm"
				pending={locked}
				disabled={selected === null || !edited(channel)}
				title="Take the selected point out of the curve"
				onclick={() => selected !== null && removeSelected(selected)}>Remove point</Btn
			>
		</div>
	</div>
	<span id="{uid}-keys" class="vh">Arrow keys move the selected point. Page Up and Page Down pick another point. Delete takes it out. Enter adds one.</span>
	<span class="vh" aria-live="polite">{focused ? pointWords(points, selected ?? -1) : ''}</span>
	<div class="hint">Click to add a point, drag to move it, double-click to remove it. Each channel runs first, then the master.</div>
</div>

<style>
	/* The box the layout is judged from: its width, not the window's. */
	.curve {
		container: curve / inline-size;
	}
	.channels {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 5px;
		margin-bottom: 6px;
	}
	.dot {
		display: inline-block;
		width: 6px;
		height: 6px;
		margin-left: 5px;
		border-radius: var(--radius-full);
		vertical-align: middle;
	}
	.body {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		row-gap: 8px;
	}
	.frame {
		min-width: 0;
	}
	.stage {
		max-width: 100%;
		margin-inline: auto;
		/* the frame is outside the box, so the bitmap is the canvas's whole size and the column is not made wider by it */
		box-shadow: 0 0 0 var(--line-width) var(--border-strong);
		border-radius: var(--radius-sm);
	}
	.stage:focus-visible {
		outline: var(--focus-outline);
		outline-offset: var(--focus-offset);
	}
	canvas {
		display: block;
		width: 100%;
		height: 100%;
		border-radius: var(--radius-sm);
		cursor: crosshair;
		touch-action: none;
	}
	.point {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 6px;
	}
	.point :global(.cell) {
		flex: 0 0 64px;
	}
	.point > :global(.k-btn) {
		margin-left: auto;
	}
	.hint {
		margin: 6px 0;
		font-size: var(--text-xs);
		color: var(--text-muted);
	}
	/* Room for it: the canvas on the left and the selected point's numbers in a column beside it. */
	@container curve (min-width: 380px) {
		.body {
			grid-template-columns: minmax(0, 288px) minmax(104px, 1fr);
			column-gap: 14px;
			align-items: start;
		}
		.stage {
			margin-inline: 0;
		}
		.point {
			flex-direction: column;
			align-items: stretch;
		}
		.point :global(.cell) {
			flex: none;
		}
		.point > :global(.k-btn) {
			margin-left: 0;
		}
	}
	/* Said to a screen reader, not shown. */
	.vh {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
	}
</style>
