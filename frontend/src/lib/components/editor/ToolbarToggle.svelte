<script lang="ts">
	// A button in the timeline's toolbar: one height (24 px), one target (24 px square at least),
	// one radius, and one look for "on" — the amber outline and wash — whether it is a tool, a mode,
	// a view or a menu that has something chosen in it. A toggle gives `pressed` (announced as one);
	// a button that only shows a state, like the delivery frame's menu, gives `on`; an action gives
	// neither. Colors and states are `styles/controls.css` (`.k-tt`), so every theme gets them.
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';
	import { tip, tipText, type TipInput } from '$lib/tip';

	let {
		title,
		label = undefined,
		icon = undefined,
		iconSize = 14,
		pressed = undefined,
		on = undefined,
		bare = false,
		square = undefined,
		disabled = false,
		onclick,
		children = undefined,
		...rest
	}: {
		/** The tooltip: what it does, with its shortcut. */
		title: TipInput;
		/** The accessible name when the tooltip is longer than a name should be; defaults to the title. */
		label?: string;
		icon?: string;
		iconSize?: number;
		/** A toggle: announced as pressed or not, and lit while it is. */
		pressed?: boolean | null;
		/** Lit without being a toggle. */
		on?: boolean;
		/** No outline while idle: a tool, a zoom button. */
		bare?: boolean;
		/** 24 px each way, for a button whose content is a picture; by default one with no children. */
		square?: boolean;
		disabled?: boolean;
		onclick?: (e: MouseEvent) => void;
		/** Text beside the icon (or a picture of its own, with `square`). */
		children?: Snippet;
		[key: string]: unknown;
	} = $props();

	const lit = $derived(pressed ?? on ?? false);
</script>

<button
	type="button"
	class="k-tt"
	use:tip={title}
	aria-label={children ? label : (label ?? (tipText(title) || undefined))}
	aria-pressed={pressed ?? undefined}
	data-on={lit || undefined}
	data-bare={bare || undefined}
	data-square={(square ?? !children) ? '' : undefined}
	{disabled}
	{onclick}
	{...rest}
>
	{#if icon}<Icon n={icon} s={iconSize} />{/if}
	{@render children?.()}
</button>
