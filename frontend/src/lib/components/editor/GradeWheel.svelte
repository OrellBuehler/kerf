<script lang="ts" module>
	import { wheelHue } from '#lib/grade';

	/** The hue disc: each pixel the direction `colorbalance` pushes at that puck position. It is the same for every wheel and
	 *  every size, so it is painted once and copied onto each canvas. */
	const SIDE = 256;
	let painted: HTMLCanvasElement | null = null;
	function hueDisc(): HTMLCanvasElement {
		if (painted) return painted;
		const el = document.createElement('canvas');
		el.width = SIDE;
		el.height = SIDE;
		const ctx = el.getContext('2d');
		if (ctx) {
			const img = ctx.createImageData(SIDE, SIDE);
			for (let j = 0; j < SIDE; j++) {
				for (let i = 0; i < SIDE; i++) {
					const x = ((i + 0.5) / SIDE) * 2 - 1;
					const y = 1 - ((j + 0.5) / SIDE) * 2;
					const o = (j * SIDE + i) * 4;
					const [r, g, b] = wheelHue(x, y);
					img.data[o] = r;
					img.data[o + 1] = g;
					img.data[o + 2] = b;
					img.data[o + 3] = Math.hypot(x, y) <= 1 ? 255 : 0;
				}
			}
			ctx.putImageData(img, 0, 0);
		}
		painted = el;
		return el;
	}
	function paint(el: HTMLCanvasElement) {
		el.width = SIDE;
		el.height = SIDE;
		el.getContext('2d')?.drawImage(hueDisc(), 0, 0);
	}
</script>

<script lang="ts">
	// One colour wheel of the grade: a name and its reset, the hue disc with a puck, the lightness slider, and four boxes for
	// the numbers `colorbalance` gets (R, G, B) and the lightness. The disc is a slider with two axes: drag the puck, or focus it and
	// use the arrow keys (Shift five steps, Alt a tenth), Home or a double-click to put it back in the middle, Escape to drop
	// what is in flight. A drag, a run of keys, a typed number or a double-click is one edit; pointercancel and a lost focus
	// abandon a drag. What is written is built from the wheel of the clip the edit began on, as the cut has it when it ends
	// (`onChange` gets a function of that wheel), never from the wheel on screen by then. The arithmetic is `grade-wheel.ts`'s.
	// The layout is a container query on the section's box: three across when there is room, a row each when there is not.
	import { onDestroy } from 'svelte';
	import GradeField from './GradeField.svelte';
	import GradeSlider from './GradeSlider.svelte';
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import { beginDrag } from '#lib/drag';
	import { NEUTRAL_WHEEL, normalizeWheel, wheelNeutral } from '#lib/grade';
	import {
		CHANNEL_LABEL,
		CHANNEL_NAME,
		DISC_NOTE,
		RESET_NOTE,
		WHEEL_CHANNELS,
		WHEEL_SPEC,
		channelValue,
		lightnessWords,
		nudgeWheel,
		puckAt,
		puckPercent,
		sameWheel,
		wheelKey,
		wheelPolar,
		wheelWords,
		withChannelValue
	} from '#lib/grade-wheel';
	import { KeyRun } from '#lib/key-run';
	import { tip } from '#lib/tip';
	import type { Wheel } from '#lib/types';

	let {
		label,
		wheel,
		clipId,
		locked = false,
		onChange
	}: {
		/** `Shadows`. */
		label: string;
		/** The wheel of the clip on screen, as the cut has it. */
		wheel: Wheel;
		clipId: string;
		/** An edit is in flight, or the proposal is on screen: the controls are held (they keep focus). */
		locked?: boolean;
		/** Change the wheel of clip `clipId` as the cut has it now: `change` is given it and returns the one to write. */
		onChange: (clipId: string, change: (wheel: Wheel) => Wheel) => Promise<unknown>;
	} = $props();

	const uid = $props.id();

	/** The wheel a gesture shows while it is in flight, and the clip it began on; `null` between gestures. */
	let draft = $state.raw<{ clipId: string; wheel: Wheel } | null>(null);
	const shown = $derived(draft && draft.clipId === clipId ? draft.wheel : wheel);
	const at = $derived(puckPercent(shown));
	const neutral = $derived(wheelNeutral(shown));
	let dragging = false;

	/** Write a change to the wheel of `to`; the draft ends once the edit has landed, so the puck does not step back in between. */
	async function put(to: string, change: (wheel: Wheel) => Wheel) {
		const mine = draft;
		try {
			await onChange(to, change);
		} finally {
			if (draft === mine) draft = null;
		}
	}
	/** Only the puck: what the disc's gestures and keys change, whatever the lightness is by the time they are written. */
	const puckTo = (p: Wheel) => (w: Wheel) => ({ ...w, x: p.x, y: p.y });

	// ---- the keys ----------------------------------------------------------------

	let runClip = '';
	const run = new KeyRun<Wheel>((from, last) => {
		if (sameWheel(from, last)) {
			draft = null;
			return;
		}
		void put(runClip, puckTo(last));
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
		if (e.key === 'Enter' && run.active) {
			e.preventDefault();
			run.finish();
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
		const key = wheelKey(e.key, { shift: e.shiftKey, alt: e.altKey });
		if (!key) return;
		e.preventDefault();
		if (!run.active) runClip = clipId;
		const next = nudgeWheel(run.last ?? wheel, key);
		run.push(wheel, next);
		draft = { clipId: runClip, wheel: next };
	}

	// ---- the pointer -------------------------------------------------------------

	/** A click that only put the puck down waits this long for the second click of a double-click, which centres it, so a
	 *  double-click writes the centring and not the click before it. */
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
		const el = e.currentTarget as HTMLElement;
		run.finish();
		const to = clipId;
		const base = wheel;
		const place = (ev: PointerEvent) => {
			const p = puckAt(el.getBoundingClientRect(), ev.clientX, ev.clientY);
			draft = { clipId: to, wheel: normalizeWheel({ ...base, x: p.x, y: p.y }) };
		};
		settle(true);
		place(e);
		dragging = true;
		let moved = false;
		beginDrag(e, {
			move(ev) {
				moved = true;
				place(ev);
			},
			commit() {
				dragging = false;
				const done = draft?.wheel ?? base;
				if (!moved) defer(() => void put(to, puckTo(done)));
				else void put(to, puckTo(done));
			},
			abandon() {
				dragging = false;
				draft = null;
			}
		});
	}

	function centre() {
		if (locked) return;
		settle(true);
		run.cancel();
		draft = { clipId, wheel: { ...wheel, x: 0, y: 0 } };
		void put(clipId, puckTo({ x: 0, y: 0, lightness: 0 }));
	}
</script>

<div class="wheel" role="group" aria-label={label}>
	<div class="head">
		<span class="name">{label}</span>
		<IconBtn
			size={24}
			title={{ text: `Reset ${label.toLowerCase()} to neutral`, detail: RESET_NOTE }}
			aria-describedby="{uid}-reset"
			pending={locked}
			disabled={neutral}
			onclick={() => void put(clipId, () => NEUTRAL_WHEEL)}
		>
			<Icon n="rotate-ccw" s={14} />
		</IconBtn>
	</div>
	<div
		class="disc"
		role="slider"
		tabindex="0"
		aria-label="{label} color"
		aria-roledescription="color wheel"
		aria-valuemin={0}
		aria-valuemax={100}
		aria-valuenow={Math.round(wheelPolar(shown).amount * 100)}
		aria-valuetext={wheelWords(shown)}
		aria-disabled={locked || undefined}
		aria-describedby="{uid}-keys"
		use:tip={{ text: `${label} color wheel`, detail: `Drag the puck, or focus it and use the keys.\n${DISC_NOTE}` }}
		onpointerdown={down}
		ondblclick={centre}
		{onkeydown}
		onblur={() => run.finish()}
	>
		<canvas use:paint></canvas>
		<span class="puck" style="left:{at.left}%;top:{at.top}%"></span>
	</div>
	<span id="{uid}-keys" class="vh">{DISC_NOTE}</span>
	<span id="{uid}-reset" class="vh">{RESET_NOTE}</span>
	<div class="bar">
		<GradeSlider
			name="{label} lightness"
			value={shown.lightness}
			spec={WHEEL_SPEC}
			scope={clipId}
			disabled={locked}
			words={lightnessWords}
			onCommit={(v, to) => put(to, (w) => ({ ...w, lightness: v }))}
		/>
	</div>
	<div class="nums">
		{#each WHEEL_CHANNELS as c (c)}
			<GradeField
				label={CHANNEL_LABEL[c]}
				name="{label} {CHANNEL_NAME[c]}"
				hint="What the grade adds to the {CHANNEL_NAME[c]} of the {label.toLowerCase()}"
				value={channelValue(shown, c)}
				spec={WHEEL_SPEC}
				scope={clipId}
				disabled={locked}
				onCommit={(v, to) => put(to, (w) => withChannelValue(w, c, v))}
			/>
		{/each}
		<GradeField
			label="L"
			name="{label} lightness value"
			hint={lightnessWords(shown.lightness)}
			value={shown.lightness}
			spec={WHEEL_SPEC}
			scope={clipId}
			disabled={locked}
			onCommit={(v, to) => put(to, (w) => ({ ...w, lightness: v }))}
		/>
	</div>
</div>

<style>
	.wheel {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		row-gap: 4px;
		min-width: 0;
		align-content: start;
	}
	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 4px;
		min-width: 0;
		min-height: 24px;
	}
	.name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--text-sm);
		font-weight: var(--fw-medium);
		color: var(--text-secondary);
	}
	.disc {
		position: relative;
		width: 100%;
		max-width: 132px;
		aspect-ratio: 1;
		margin-inline: auto;
		border-radius: var(--radius-full);
		touch-action: none;
		cursor: crosshair;
	}
	.disc:focus-visible {
		outline: var(--focus-outline);
		outline-offset: var(--focus-offset);
	}
	.disc[aria-disabled='true'] {
		cursor: default;
	}
	canvas {
		display: block;
		box-sizing: border-box;
		width: 100%;
		height: 100%;
		border-radius: var(--radius-full);
		border: var(--line-width) solid var(--border-strong);
	}
	/* The cross at the middle: where the puck is when the wheel pushes nothing. */
	.disc::before,
	.disc::after {
		content: '';
		position: absolute;
		background: color-mix(in srgb, var(--surface-app) 45%, transparent);
		pointer-events: none;
	}
	.disc::before {
		left: 50%;
		top: 14%;
		bottom: 14%;
		width: var(--line-width);
	}
	.disc::after {
		top: 50%;
		left: 14%;
		right: 14%;
		height: var(--line-width);
	}
	.puck {
		position: absolute;
		width: 12px;
		height: 12px;
		margin: -6px 0 0 -6px;
		box-sizing: border-box;
		border-radius: var(--radius-full);
		background: var(--surface-panel);
		border: 2px solid var(--text-primary);
		pointer-events: none;
		z-index: 1;
	}
	.bar {
		min-width: 0;
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
	.nums {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 3px;
		min-width: 0;
	}
	/* Not room for three across: a row each, the disc on the left and everything else beside it. */
	@container wheels (max-width: 287px) {
		.wheel {
			grid-template-columns: 84px minmax(0, 1fr);
			grid-template-areas:
				'disc head'
				'disc nums'
				'disc bar';
			column-gap: 10px;
			row-gap: 6px;
		}
		.head {
			grid-area: head;
		}
		.disc {
			grid-area: disc;
			width: 84px;
			margin-inline: 0;
		}
		.nums {
			grid-area: nums;
		}
		.bar {
			grid-area: bar;
		}
	}
</style>
