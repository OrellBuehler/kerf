<script lang="ts">
	import type { Snippet } from 'svelte';
	import { tip, type TipInput } from '#lib/tip';

	let {
		pressed,
		title = undefined,
		class: klass = '',
		children,
		...rest
	}: {
		/** Whether it is the chosen one of its row; always given, so a chip never reads as an action. */
		pressed: boolean;
		title?: TipInput;
		class?: string;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();
</script>

<!-- A toggle among choices. Colors, hover and the on look are `.k-chip` in `styles/controls.css`;
     an action is a `Btn`. -->
<button class={['k-chip', klass]} aria-pressed={pressed} use:tip={title} {...rest}>
	{@render children?.()}
</button>
