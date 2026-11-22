<script lang="ts">
	// The box the Mixer's popovers share — the master FX and the delivery check: set in the Mixer's row beside the master
	// strip, over the track strips to its left, so the master's fader and meter stay in view and keep their height; the first
	// control takes the focus (the box itself when it has none: a delivery check is only text); Escape closes it from wherever the focus is,
	// and so do a press outside, the window losing focus, and the focus moving out of it (a Tab out of the last control) — Escape hands the
	// focus back to the button that opened it. Its keys do not reach the page (a Delete in it does not delete the clip behind it, nor a
	// Space play), except a Ctrl / Cmd chord it does not use: undo, redo and save work with it open (`mixer-popover.ts` has the rules).
	//
	// It is drawn in the panel (absolute, against the row), not in the body: a panel can be in a window of its own, where nothing
	// outside the panel has Svelte's events.
	import { untrack, type Snippet } from 'svelte';
	import { onWindow } from '#lib/window-events';
	import { focusLeaves, popoverKey } from '#lib/mixer-popover';

	let {
		label,
		name = undefined,
		anchor,
		onclose,
		children
	}: {
		/** What the popover is called: its heading, in capitals, and its accessible name unless `name` says more. */
		label: string;
		/** The accessible name when it should say more than the heading (that what it shows is out of date). */
		name?: string;
		/** The button that opened it: a press there is not "outside", and the focus returns to it. */
		anchor: HTMLElement;
		onclose: () => void;
		children: Snippet;
	} = $props();

	let el = $state<HTMLDivElement | null>(null);

	$effect(() => {
		if (!el) return;
		const box = el;
		// The first control; a popover with none (the delivery check is text) takes the focus itself, so Escape has somewhere to be heard.
		untrack(() => (box.querySelector<HTMLElement>('button:not(:disabled), select, [role="slider"], input') ?? box).focus({ preventScroll: true }));
	});

	function close() {
		anchor.focus({ preventScroll: true });
		onclose();
	}

	function onkeydown(e: KeyboardEvent) {
		const rule = popoverKey(e);
		if (rule.stop) e.stopPropagation();
		if (rule.close) {
			e.preventDefault();
			close();
		}
	}

	/** Escape with the focus outside the box (still on the button, or wherever it was): the window hears it first, so the page does not
	 *  also take it for its own (clearing the selection). */
	function escapeOutside(e: KeyboardEvent) {
		if (e.key !== 'Escape' || e.defaultPrevented || (e.target instanceof Node && el?.contains(e.target))) return;
		e.preventDefault();
		close();
	}

	/** The focus moved out of the box — to something that is not its button: a Tab past the last control. */
	function onfocusout(e: FocusEvent) {
		if (el && focusLeaves(e.relatedTarget, el, anchor)) onclose();
	}

	function outside(e: Event) {
		const t = e.target as Node | null;
		if (!el || (t && (el.contains(t) || anchor.contains(t)))) return;
		onclose();
	}
</script>

<!-- A popover's keys (Escape, and the swallowing of the page's) go on the box that holds the controls, which is no control itself. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
	bind:this={el}
	class="pop"
	role="group"
	aria-label={name ?? label}
	tabindex="-1"
	{onkeydown}
	{onfocusout}
	{@attach onWindow({ pointerdowncapture: outside, keydowncapture: escapeOutside, blur: () => onclose() })}
>
	<div class="head"><span class="title">{label}</span></div>
	{@render children()}
</div>

<style>
	/* Beside the master strip, over the strips to its left: the row is as wide as the panel, and the strip and its gap are --master-w + 8 px of it. */
	.pop {
		position: absolute;
		z-index: 20;
		top: 0;
		right: calc(var(--master-w, 132px) + 8px);
		width: min(232px, calc(100% - var(--master-w, 132px) - 16px));
		/* No taller than the panel shows (the Mixer is a size container: 100cqh is its visible height less nothing, 16 px being its padding), so a
		   panel docked at its minimum height scrolls the popover inside itself rather than leaving its lower part below the fold. */
		max-height: calc(100cqh - 16px);
		overflow-y: auto;
		box-sizing: border-box;
		display: flex;
		flex-direction: column;
		gap: 10px;
		padding: 10px 12px 12px;
		background: var(--surface-raised);
		border: var(--line-width) solid var(--border-strong);
		border-radius: var(--radius-md);
		box-shadow: var(--shadow-pop);
	}
	.head {
		display: flex;
		align-items: center;
	}
	.title {
		font: var(--type-overline);
		letter-spacing: var(--tracking-caps);
		text-transform: uppercase;
		color: var(--text-secondary);
	}
</style>
