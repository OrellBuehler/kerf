<script lang="ts">
	import type { Snippet } from 'svelte';
	import { tip, tipText, type TipInput } from '$lib/tip';

	let {
		active = undefined,
		title = undefined,
		size = 32,
		style = '',
		class: klass = '',
		pending = false,
		onclick = undefined,
		children,
		...rest
	}: {
		active?: boolean;
		/** The tooltip, and the button's name: a string, or `{ text, keys, detail }` (see `tip.ts`). */
		title?: TipInput;
		size?: number;
		style?: string;
		class?: string;
		/** An edit is in flight: off to the eye and the click, but still focusable (see `Btn`). */
		pending?: boolean;
		onclick?: unknown;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();
</script>

<!-- `aria-pressed` only when it is a toggle (`active` given): a plain icon button announced as
     "not pressed" reads as a switch. Colors and states are `styles/controls.css`. -->
<button
	class={['k-ibtn', klass]}
	use:tip={title}
	aria-label={tipText(title) || undefined}
	aria-pressed={active}
	onclick={(e: MouseEvent) => {
		if (pending) return;
		(onclick as ((e: MouseEvent) => void) | undefined)?.(e);
	}}
	style="width:{size}px;height:{size}px;{style}"
	{...rest}
	aria-disabled={pending ? 'true' : (rest['aria-disabled'] as boolean | 'true' | 'false' | undefined)}
>
	{@render children?.()}
</button>
