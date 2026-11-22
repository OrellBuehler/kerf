<script lang="ts">
	// The box the Mixer's popovers share — the master FX and the delivery check: set in the Mixer's row beside the master
	// strip, over the track strips to its left, so the master's fader and meter stay in view and keep their height; the first
	// control takes the focus; Escape, a press outside, or the window losing focus close it, and the focus goes back to the button that
	// opened it; and its keys do not reach the page (a Delete in it does not delete the clip behind it, nor a Space play).
	//
	// It is drawn in the panel (absolute, against the row), not in the body: a panel can be in a window of its own, where nothing
	// outside the panel has Svelte's events.
	import { untrack, type Snippet } from 'svelte';
	import { onWindow } from '#lib/window-events';

	let {
		label,
		anchor,
		onclose,
		children
	}: {
		/** What the popover is called: its accessible name and, in capitals, its heading. */
		label: string;
		/** The button that opened it: a press there is not "outside", and the focus returns to it. */
		anchor: HTMLElement;
		onclose: () => void;
		children: Snippet;
	} = $props();

	let el = $state<HTMLDivElement | null>(null);

	$effect(() => {
		if (!el) return;
		const box = el;
		untrack(() => box.querySelector<HTMLElement>('button:not(:disabled), select, [role="slider"], input')?.focus({ preventScroll: true }));
	});

	function close() {
		anchor.focus({ preventScroll: true });
		onclose();
	}

	function onkeydown(e: KeyboardEvent) {
		if (e.key !== 'Tab') e.stopPropagation();
		if (e.key === 'Escape') {
			e.preventDefault();
			close();
		}
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
	aria-label={label}
	tabindex="-1"
	{onkeydown}
	{@attach onWindow({ pointerdowncapture: outside, blur: () => onclose() })}
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
		max-height: 100%;
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
