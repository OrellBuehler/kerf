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
	import { grabOffset, KEY_COMMIT_MS, moved, positionAt, sliderKey, type SliderKey } from '$lib/slider-gesture';

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
	let run: { from: number; last: number; timer: ReturnType<typeof setTimeout> } | null = null;

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
		finishRun();
		const grab = grabbed(e);
		if (grab === null) return;
		e.preventDefault();
		root.setPointerCapture(e.pointerId);
		drag = { id: e.pointerId, grab, from: value, last: value, x0: e.clientX, y0: e.clientY, travelled: false };
	}

	function onpointermove(e: PointerEvent) {
		const d = drag;
		if (!d || e.pointerId !== d.id) return;
		if (!d.travelled && Math.hypot(e.clientX - d.x0, e.clientY - d.y0) < 2) return;
		d.travelled = true;
		const v = fromPos(positionAt(vertical ? e.clientY : e.clientX, extent(), vertical, d.grab));
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

	/** Write out the run of key presses so far as one edit. */
	function finishRun() {
		const r = run;
		if (!r) return;
		clearTimeout(r.timer);
		run = null;
		if (moved(toPos(r.from), toPos(r.last))) void oncommit?.(r.last);
		else oncancel?.();
	}

	function cancelRun() {
		if (!run) return;
		clearTimeout(run.timer);
		run = null;
		oncancel?.();
	}

	function onkeydown(e: KeyboardEvent) {
		if (disabled) return;
		if (e.key === 'Escape') {
			// Only a gesture in flight is Escape's: with none, it falls through to the page
			// (which clears the selection).
			if (drag || run) {
				drag = null;
				if (run) clearTimeout(run.timer);
				run = null;
				oncancel?.();
				e.preventDefault();
				e.stopPropagation();
			}
			return;
		}
		if (e.key === 'Enter' && run) {
			e.preventDefault();
			finishRun();
			return;
		}
		if (e.ctrlKey || e.metaKey) {
			// A run not yet written out is the newest change: undo takes it back rather
			// than the edit before it (which the run would then land on top of); any
			// other shortcut sees it written first.
			if (run && e.key.toLowerCase() === 'z' && !e.shiftKey) {
				cancelRun();
				e.preventDefault();
				e.stopPropagation();
			} else finishRun();
			return;
		}
		const key = sliderKey(e.key, { shift: e.shiftKey, alt: e.altKey });
		if (!key || drag) return;
		e.preventDefault();
		const from = run ? run.from : value;
		const last = step(run ? run.last : value, key);
		if (run) clearTimeout(run.timer);
		run = { from, last, timer: setTimeout(finishRun, KEY_COMMIT_MS) };
		onpreview?.(last);
	}

	function onblur() {
		finishRun();
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
		window.addEventListener('blur', away);
		return () => {
			window.removeEventListener('blur', away);
			if (run) clearTimeout(run.timer);
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
	title={tip}
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
