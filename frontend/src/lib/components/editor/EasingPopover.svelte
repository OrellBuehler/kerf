<script lang="ts">
	// The segment easing popover: the presets as curves, drawn from the polyline the renderers play
	// (`easingPath`), the one the selected segments share lit. A pick is one edit and closes it;
	// Escape, a press outside, the window losing focus or a scroll close it without writing.
	import { untrack } from 'svelte';
	import { EASING_CHOICES, easingId } from '#lib/easing';
	import { easingPath } from '#lib/dope-sheet';
	import { onWindow } from '#lib/window-events';
	import { windowOf } from '#lib/realm';
	import type { Easing } from '#lib/types';

	let {
		x,
		y,
		current,
		onpick,
		onclose
	}: {
		/** Where it opens, in the window's viewport (it is kept inside it). */
		x: number;
		y: number;
		/** The easing every selected segment has, or undefined when they differ. */
		current: Easing | undefined;
		onpick: (e: Easing) => void;
		onclose: () => void;
	} = $props();

	const W = 40;
	const H = 26;
	const COLS = 4;
	const lit = $derived(current === undefined ? null : easingId(current));

	let el = $state<HTMLDivElement | null>(null);
	let pos = $state({ x: 0, y: 0 });
	let returnTo: HTMLElement | null = null;

	$effect(() => {
		if (!el) return;
		const win = windowOf(el);
		const margin = 6;
		const nx = Math.min(x, win.innerWidth - el.offsetWidth - margin);
		const ny = y + el.offsetHeight + margin > win.innerHeight ? y - el.offsetHeight - 8 : y;
		pos = { x: Math.max(margin, nx), y: Math.max(margin, ny) };
	});

	$effect(() => {
		if (!el) return;
		const box = el;
		untrack(() => {
			const was = windowOf(box).document.activeElement;
			if (was instanceof HTMLElement && !box.contains(was)) returnTo = was;
			const start = box.querySelector<HTMLElement>('[aria-pressed="true"]') ?? box.querySelector<HTMLElement>('button');
			start?.focus({ preventScroll: true });
		});
	});

	function close() {
		returnTo?.focus({ preventScroll: true });
		onclose();
	}

	function pick(e: Easing) {
		returnTo?.focus({ preventScroll: true });
		onpick(e);
	}

	function onkeydown(e: KeyboardEvent) {
		// The popover has the keys while it is open: a Delete here must not delete the keys it shapes.
		if (e.key !== 'Tab') e.stopPropagation();
		if (e.key === 'Escape') {
			e.preventDefault();
			e.stopPropagation();
			close();
			return;
		}
		const step = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -COLS, ArrowDown: COLS }[e.key];
		if (step === undefined || !el) return;
		const buttons = [...el.querySelectorAll<HTMLElement>('button')];
		const at = buttons.indexOf(windowOf(el).document.activeElement as HTMLElement);
		const next = buttons[Math.min(Math.max((at < 0 ? 0 : at) + step, 0), buttons.length - 1)];
		next?.focus();
		e.preventDefault();
		e.stopPropagation();
	}

	const outside = (e: Event) => {
		if (el && e.target instanceof Node && el.contains(e.target)) return;
		onclose();
	};
</script>

<div
	bind:this={el}
	class="pop"
	role="dialog"
	aria-label="Easing"
	tabindex="-1"
	style="left:{pos.x}px;top:{pos.y}px"
	{onkeydown}
	{@attach onWindow({ pointerdowncapture: outside, blur: () => onclose(), resize: () => onclose(), scrollcapture: outside })}
>
	<div class="head">Easing of the segment after each selected key</div>
	<div class="grid" style="grid-template-columns:repeat({COLS}, 1fr)">
		{#each EASING_CHOICES as c (c.id)}
			<button type="button" class="opt" aria-pressed={lit === c.id} onclick={() => pick(c.easing)}>
				<svg width={W} height={H} viewBox="0 0 {W} {H}" aria-hidden="true">
					<rect x="0.5" y="0.5" width={W - 1} height={H - 1} rx="2" class="frame" />
					<path d={easingPath(c.easing, W, H, 3)} class="curve" />
				</svg>
				<span>{c.label}</span>
			</button>
		{/each}
	</div>
	{#if current !== undefined && lit === 'custom'}
		<div class="note">The selection has a curve of its own; a preset replaces it.</div>
	{/if}
</div>

<style>
	.pop {
		position: fixed;
		z-index: 60;
		padding: 8px;
		border-radius: var(--radius-md);
		background: var(--surface-overlay);
		border: var(--line-width) solid var(--border-default);
		box-shadow: var(--shadow-pop);
		width: max-content;
	}
	.pop:focus {
		outline: none;
	}
	.head,
	.note {
		font-size: 11px;
		color: var(--text-muted);
		padding: 0 2px 6px;
	}
	.note {
		padding: 6px 2px 0;
		max-width: 220px;
	}
	.grid {
		display: grid;
		gap: 4px;
	}
	.opt {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 2px;
		padding: 4px 2px;
		border-radius: var(--radius-sm);
		border: var(--line-width) solid transparent;
		background: none;
		color: var(--text-secondary);
		font-size: 11px;
		cursor: pointer;
	}
	.opt:hover {
		background: var(--hover-fill);
		color: var(--text-primary);
	}
	.opt[aria-pressed='true'] {
		border-color: var(--kerf-400);
		background: var(--selection-fill);
		color: var(--kerf-300);
	}
	.opt:focus-visible {
		outline: var(--focus-outline);
		outline-offset: var(--focus-offset);
	}
	.frame {
		fill: var(--surface-inset);
		stroke: var(--border-subtle);
	}
	.curve {
		fill: none;
		stroke: currentColor;
		stroke-width: 1.5;
		stroke-linejoin: round;
	}
</style>
