<script lang="ts">
	// The curve editor of the grade: a square canvas that fills the Inspector's column (up to a size), a channel at a time,
	// with the others drawn faint behind it. Click on empty space adds a point and a press on one takes hold of it to drag it, a
	// double-click on a point or a right-click takes it out (a double-click on empty space adds the one point and nothing else); with
	// the canvas focused the arrow keys move the selected point (Shift five steps, Alt a tenth), Page Up and Page Down pick another,
	// Delete takes it out and Enter adds one; under it the selected point's In and Out are numbers to type. Each gesture writes one
	// edit when it ends (a run of keys when they go quiet); Escape, pointercancel or a lost focus abandon one in flight. What is
	// written is the channel of the clip the gesture *began* on. The curve is `grade.ts`'s, the TS mirror of the engine's cubic, so
	// what is drawn is what the export renders. The gestures and keys are `curve-control.ts`'s and the hit-testing `curve-edit.ts`'s,
	// which have the tests: a write names a point by its input, never by an index into the list that was drawn.
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
	import { CurveControl, type CurveIO } from '#lib/curve-control';
	import { curveSide, plotRect, pxOf, removeAtX, setAtX, unitAt } from '#lib/curve-edit';
	import { keyEvent } from '#lib/key-route';
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
	/** What a screen reader is told about the point, set on a press, a release and a key, never on a pointer move. */
	let said = $state('');

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

	// ---- gestures and keys ----------------------------------------------------------

	/** Write a change to a channel of the clip `to`; the draft ends once the edit has landed, so the point does not step back in between. */
	async function put(to: { clipId: string; channel: CurveChannel }, change: (points: CurvePoint[]) => CurvePoint[] | null) {
		const mine = draft;
		try {
			await onChange(to.clipId, to.channel, change);
		} finally {
			if (draft === mine) draft = null;
		}
	}

	const io: CurveIO = {
		target: () => ({ clipId: clip.id, channel }),
		points: () => points,
		selected: () => selected,
		select,
		draft: (to, p) => (draft = p ? { clipId: to.clipId, channel: to.channel, points: p } : null),
		put,
		announce: (text) => (said = text),
		locked: () => locked,
		side: () => side - 2 * PAD
	};
	const control = new CurveControl(io);
	onDestroy(() => control.destroy());

	/** The pointer's place in the unit square of the plot. */
	const unit = (el: HTMLElement, e: MouseEvent) => unitAt(plotRect(el.getBoundingClientRect(), PAD), e.clientX, e.clientY);

	function down(e: PointerEvent) {
		if (e.button !== 0) return;
		const el = e.currentTarget as HTMLCanvasElement;
		const p = unit(el, e);
		// A press focuses the editor whether or not it takes anything (no room for another point).
		(el.parentElement as HTMLElement | null)?.focus({ preventScroll: true });
		if (!control.down(p.x, p.y)) return;
		beginDrag(e, {
			move: (ev) => {
				const q = unit(el, ev);
				control.move(q.x, q.y);
			},
			commit: () => control.up(),
			abandon: () => control.cancel()
		});
	}

	/** A double-click on a point takes it out (not the one its own first click added); on empty space it does nothing more. */
	function doubleClick(e: MouseEvent) {
		const p = unit(e.currentTarget as HTMLElement, e);
		if (control.doubleClick(p.x, p.y)) {
			e.preventDefault();
			e.stopPropagation();
		}
	}

	function contextMenu(e: MouseEvent) {
		const p = unit(e.currentTarget as HTMLElement, e);
		if (control.contextMenu(p.x, p.y)) {
			e.preventDefault();
			e.stopPropagation();
		}
	}

	function onkeydown(e: KeyboardEvent) {
		control.key(keyEvent(e, settings.actionFor(e) === 'edit.undo'));
	}

	// ---- the point's numbers --------------------------------------------------------

	/** The point the boxes are on, named by its input, and the clip and channel: what a typed entry began on. */
	const scope = $derived({ clipId: clip.id, channel, x: selected === null ? null : points[selected].x });
	type PointScope = typeof scope;
	function setNumber(v: number, s: PointScope, to: 'x' | 'y') {
		const x = s.x;
		if (x === null) return;
		return put(s, (cur) => setAtX(cur, x, to === 'x' ? { x: v } : { y: v }));
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
				control.run.cancel();
				select(null);
				void put({ clipId: clip.id, channel }, () => []);
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
				onblur={() => control.blur()}
			>
				<canvas bind:this={canvas} aria-hidden="true" onpointerdown={down} ondblclick={doubleClick} oncontextmenu={contextMenu}></canvas>
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
				onclick={() => control.removeSelected()}>Remove point</Btn
			>
		</div>
	</div>
	<span id="{uid}-keys" class="vh">Arrow keys move the selected point. Page Up and Page Down pick another point. Delete takes it out. Enter adds one.</span>
	<span class="vh" aria-live="polite">{said}</span>
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
