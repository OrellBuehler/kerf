<script lang="ts">
	import type { Snippet } from 'svelte';
	import { tip, type TipInput } from '#lib/tip';

	let {
		pressed,
		title = undefined,
		class: klass = '',
		pending = false,
		onclick = undefined,
		children,
		...rest
	}: {
		/** Whether it is the chosen one of its row; always given, so a chip never reads as an action. */
		pressed: boolean;
		title?: TipInput;
		class?: string;
		/** An edit is in flight: off to the eye and the click, but still focusable (see `Btn`). */
		pending?: boolean;
		onclick?: unknown;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();
</script>

<!-- A toggle among choices. Colors, hover and the on look are `.k-chip` in `styles/controls.css`;
     an action is a `Btn`. -->
<button
	class={['k-chip', klass]}
	aria-pressed={pressed}
	aria-disabled={pending ? 'true' : undefined}
	use:tip={title}
	onclick={(e: MouseEvent) => {
		if (pending) return;
		(onclick as ((e: MouseEvent) => void) | undefined)?.(e);
	}}
	{...rest}
>
	{@render children?.()}
</button>
