<script lang="ts">
	// A mixer slider: the fader's vertical travel and the pan's horizontal one are the
	// same control with a different axis. It is *controlled* — it draws `value`, and
	// reports what a gesture would make it (`onpreview`, as it moves), what it ended
	// as (`oncommit`, once) or that it was abandoned (`oncancel`) — so the strip decides
	// what a value is and when to write it. The geometry and the keys are
	// `slider-gesture.ts`'s, which has the tests.
	//
	// One edit per gesture: a drag commits when the pointer is released, a run of
	// key presses commits when it has been quiet for a moment (or on blur / Enter),
	// and Escape drops whatever is in flight. Double-click resets. Only the thumb
	// takes hold of a drag — a click elsewhere on the travel just focuses the slider —
	// so a double-click to reset is never also a jump.
	import { windowOf } from '#lib/realm';
	import { windows } from '#lib/windows.svelte';
	import { grabOffset, isDrag, moved, positionAt, sliderKey, type SliderKey } from '#lib/slider-gesture';
	import { keyEvent } from '#lib/key-route';
	import { settings } from '#lib/settings.svelte';
	import { StepKeys } from '#lib/step-keys';
	import { tip as tooltip } from '#lib/tip';

	let {
		orientation = 'vertical',
		value,
		toPos,
		fromPos,
		step,
		label,
		valueText,
		tip,
		ticks = [],
		detent = null,
		fillFrom = null,
		accent = 'var(--kerf-400)',
		solid = false,
		disabled = false,
		onpreview,
		oncommit,
		oncancel,
		onreset
	}: {
		orientation?: 'vertical' | 'horizontal';
		/** What is drawn: the committed value, or a gesture's preview of it. */
		value: number;
		/** A value's place on the travel, `0..1`. */
		toPos: (v: number) => number;
		/** The value a place on the travel means. */
		fromPos: (p: number) => number;
		/** The value after a key. */
		step: (v: number, key: SliderKey) => number;
		label: string;
		/** A value in words, for assistive technology. */
		valueText: (v: number) => string;
		/** The tooltip. */
		tip: string;
		/** Marks on the scale beside the rail. */
		ticks?: { pos: number; label?: string }[];
		/** A place drawn as the slider's neutral (the pan's centre). */
		detent?: number | null;
		/** Where the filled part of the rail starts, `0..1`; `null` draws no fill. */
		fillFrom?: number | null;
		accent?: string;
		/** Drawn like the theme's plain range input: a flat track, the fill and the thumb in the accent. */
		solid?: boolean;
		disabled?: boolean;
		onpreview?: (v: number) => void;
		oncommit?: (v: number) => unknown;
		oncancel?: () => void;
		onreset?: () => void;
	} = $props();

	const vertical = $derived(orientation === 'vertical');
	const pos = $derived(Math.min(1, Math.max(0, toPos(value))));

	let root = $state<HTMLDivElement | null>(null);
	let rail = $state<HTMLDivElement | null>(null);

	type Drag = { id: number; grab: number; from: number; last: number; x0: number; y0: number; travelled: boolean };
	let drag: Drag | null = null;
	/** A run of key presses is one edit (`step-keys.ts`, which has the tests): written when the keys go quiet, dropped by Escape or the
	 *  history's undo — the user's own key for it, and not on the Control that begins it — and a key the slider owns never reaches the page. */
	const keys = new StepKeys({
		owns: (k) => sliderKey(k) !== null,
		step: (from, e) => step(from, sliderKey(e.key, { shift: e.shiftKey, alt: e.altKey }) as SliderKey),
		value: () => value,
		start: () => value,
		changed: (a, b) => moved(toPos(a), toPos(b)),
		preview: (v) => onpreview?.(v),
		commit: (v) => void oncommit?.(v),
		abandon: () => oncancel?.(),
		held: () => drag !== null
	});

	/** The travel's extent on screen: the rail's box. */
	function extent() {
		const r = rail!.getBoundingClientRect();
		return vertical ? { start: r.top, size: r.height } : { start: r.left, size: r.width };
	}

	/** How far along the travel the thumb's centre is from the pointer, or `null` when
	 *  the pointer is not on the thumb. */
	const HIT = 13;
	function grabbed(e: PointerEvent): number | null {
		const ext = extent();
		const p = vertical ? e.clientY : e.clientX;
		const along = vertical ? ext.start + ext.size - p : p - ext.start;
		const centre = ext.size * pos;
		if (Math.abs(along - centre) > HIT) return null;
		return grabOffset(p, ext, vertical, pos);
	}

	function onpointerdown(e: PointerEvent) {
		if (disabled || e.button !== 0 || !root || !rail) return;
		root.focus({ preventScroll: true });
		keys.run.finish();
		const grab = grabbed(e);
		if (grab === null) return;
		e.preventDefault();
		root.setPointerCapture(e.pointerId);
		drag = { id: e.pointerId, grab, from: value, last: value, x0: e.clientX, y0: e.clientY, travelled: false };
	}

	function onpointermove(e: PointerEvent) {
		const d = drag;
		if (!d || e.pointerId !== d.id) return;
		const at = vertical ? e.clientY : e.clientX;
		if (!d.travelled) {
			if (!isDrag(e.clientX - d.x0, e.clientY - d.y0)) return;
			d.travelled = true;
			// The slop says it is a drag, not where the drag begins: the thumb takes the pointer up from the value the press found
			// it at, so the first move past the slop does not jump the value by the slop's worth.
			d.grab = grabOffset(at, extent(), vertical, toPos(d.from));
		}
		const v = fromPos(positionAt(at, extent(), vertical, d.grab));
		if (v === d.last) return;
		d.last = v;
		onpreview?.(v);
	}

	function end(e: PointerEvent) {
		const d = drag;
		if (!d || e.pointerId !== d.id) return;
		drag = null;
		if (d.travelled && moved(toPos(d.from), toPos(d.last))) void oncommit?.(d.last);
		else oncancel?.();
	}

	/** The pointer was taken away (a call, an alert): nothing was decided. */
	function lost(e: PointerEvent) {
		const d = drag;
		if (!d || e.pointerId !== d.id) return;
		drag = null;
		oncancel?.();
	}

	/** The pointer was taken away or the run was given up: whatever is shown goes back. */
	function cancelRun() {
		if (keys.run.cancel()) oncancel?.();
	}

	function onkeydown(e: KeyboardEvent) {
		if (disabled) return;
		// Escape in a drag gives it up (the run, if any, with it).
		if (e.key === 'Escape' && drag) {
			drag = null;
			keys.run.cancel();
			oncancel?.();
			e.preventDefault();
			e.stopPropagation();
			return;
		}
		keys.key(keyEvent(e, settings.actionFor(e) === 'edit.undo'));
	}

	function onblur() {
		keys.blur();
	}

	function ondblclick() {
		if (disabled) return;
		drag = null;
		cancelRun();
		onreset?.();
	}

	// An abandoned gesture is not a held one: if the window loses focus mid-drag the
	// pointer events stop coming.
	$effect(() => {
		const away = () => {
			if (drag) {
				drag = null;
				oncancel?.();
			}
			cancelRun();
		};
		// The window the slider is in now: a mixer in a detached window loses focus there.
		void windows.version;
		const win = windowOf(root);
		win.addEventListener('blur', away);
		return () => {
			win.removeEventListener('blur', away);
			keys.run.cancel();
		};
	});

	const p = $derived(`${(pos * 100).toFixed(3)}%`);
	const from = $derived(fillFrom === null ? null : Math.min(1, Math.max(0, fillFrom)));
	const fillLo = $derived(from === null ? 0 : Math.min(from, pos));
	const fillHi = $derived(from === null ? 0 : Math.max(from, pos));
</script>

<div
	bind:this={root}
	class="slider {orientation}"
	class:solid
	class:disabled
	role="slider"
	tabindex={disabled ? -1 : 0}
	aria-label={label}
	aria-orientation={orientation}
	aria-valuemin="0"
	aria-valuemax="1"
	aria-valuenow={Number(pos.toFixed(3))}
	aria-valuetext={valueText(value)}
	aria-disabled={disabled}
	use:tooltip={tip}
	{onpointerdown}
	{onpointermove}
	onpointerup={end}
	onpointercancel={lost}
	onlostpointercapture={lost}
	{onkeydown}
	{onblur}
	{ondblclick}
	style="--accent:{accent}"
>
	<div bind:this={rail} class="rail">
		<div class="line"></div>
		{#if from !== null}
			<div
				class="fill"
				style={vertical
					? `bottom:${(fillLo * 100).toFixed(3)}%;height:${((fillHi - fillLo) * 100).toFixed(3)}%`
					: `left:${(fillLo * 100).toFixed(3)}%;width:${((fillHi - fillLo) * 100).toFixed(3)}%`}
			></div>
		{/if}
		{#each ticks as t (t.pos)}
			<div class="tick" style={vertical ? `bottom:${(t.pos * 100).toFixed(3)}%` : `left:${(t.pos * 100).toFixed(3)}%`}></div>
		{/each}
		{#if detent !== null}
			<div class="detent" style={vertical ? `bottom:${(detent * 100).toFixed(3)}%` : `left:${(detent * 100).toFixed(3)}%`}></div>
		{/if}
		<div class="thumb" style={vertical ? `bottom:${p}` : `left:${p}`}></div>
	</div>
</div>

<style>
	.slider {
		position: relative;
		flex: none;
		touch-action: none;
		user-select: none;
		outline: none;
		cursor: default;
		/* Room for the thumb at either end of the travel; the level meter beside a
		   fader is inset by the same, so the two share one scale. */
		--pad: 9px;
	}
	.slider.vertical {
		width: 34px;
		align-self: stretch;
		min-height: 90px;
	}
	.slider.horizontal {
		height: 22px;
		flex: 1 1 0;
		min-width: 0;
	}
	.slider.disabled {
		opacity: 0.5;
	}
	.rail {
		position: absolute;
	}
	.vertical .rail {
		top: var(--pad);
		bottom: var(--pad);
		left: 0;
		right: 0;
	}
	.horizontal .rail {
		left: var(--pad);
		right: var(--pad);
		top: 0;
		bottom: 0;
	}
	/* The track: a groove down (or across) the middle of the travel. */
	.line {
		position: absolute;
		background: var(--surface-inset);
		border: var(--line-width) solid var(--border-default);
		border-radius: var(--slider-track-radius);
		box-sizing: border-box;
	}
	.vertical .line {
		left: 50%;
		top: 0;
		bottom: 0;
		width: calc(var(--slider-track) + 2px);
		transform: translateX(-50%);
	}
	.horizontal .line {
		top: 50%;
		left: 0;
		right: 0;
		height: calc(var(--slider-track) + 2px);
		transform: translateY(-50%);
	}
	.fill {
		position: absolute;
		background: color-mix(in srgb, var(--accent) 60%, transparent);
		border-radius: var(--slider-track-radius);
	}
	.vertical .fill {
		left: 50%;
		width: var(--slider-track);
		transform: translateX(-50%);
	}
	.horizontal .fill {
		top: 50%;
		height: var(--slider-track);
		transform: translateY(-50%);
	}
	.tick {
		position: absolute;
		background: var(--border-strong);
	}
	.vertical .tick {
		left: 0;
		width: 5px;
		height: var(--line-width);
	}
	.horizontal .tick {
		top: 0;
		height: 5px;
		width: var(--line-width);
	}
	.detent {
		position: absolute;
		background: var(--text-muted);
	}
	.vertical .detent {
		left: 0;
		right: 0;
		height: var(--line-width);
	}
	.horizontal .detent {
		top: 3px;
		bottom: 3px;
		width: var(--line-width);
	}
	/* The cap: the theme's thumb, shaped for the axis it travels. */
	.thumb {
		position: absolute;
		background: var(--text-secondary);
		border: var(--line-width) solid var(--border-strong);
		box-sizing: border-box;
		border-radius: var(--slider-thumb-radius);
		cursor: grab;
		transition: background var(--dur-fast) var(--ease-out);
	}
	.vertical .thumb {
		left: 50%;
		width: calc(var(--slider-thumb) * 1.7);
		height: max(var(--slider-thumb-w), 10px);
		transform: translate(-50%, 50%);
	}
	.horizontal .thumb {
		top: 50%;
		width: var(--slider-thumb-w);
		height: var(--slider-thumb);
		transform: translate(-50%, -50%);
	}
	/* The cap's index line, in the slider's accent. */
	.vertical .thumb::after {
		content: '';
		position: absolute;
		left: 20%;
		right: 20%;
		top: 50%;
		height: var(--line-emphasis);
		transform: translateY(-50%);
		background: var(--accent);
	}
	.slider:hover .thumb,
	.slider:focus-visible .thumb {
		background: var(--text-primary);
	}
	.solid .line {
		background: var(--surface-active);
		background-clip: padding-box;
		border-color: transparent;
	}
	.solid .fill {
		background: var(--accent);
	}
	.slider.solid .thumb,
	.slider.solid:hover .thumb,
	.slider.solid:focus-visible .thumb {
		background: var(--accent);
		border-color: transparent;
	}
	.slider.solid:hover .thumb {
		filter: brightness(1.15);
	}
	/* The rest tick is a notch beside the track, no taller than the thumb that sits on it at rest. */
	.horizontal.solid .detent {
		top: 50%;
		bottom: auto;
		height: calc(var(--slider-track) + 8px);
		transform: translateY(-50%);
	}
	.slider:active .thumb {
		cursor: grabbing;
	}
	.slider:focus-visible .thumb {
		box-shadow: var(--focus-ring);
	}
	.disabled .thumb {
		cursor: default;
	}
</style>
