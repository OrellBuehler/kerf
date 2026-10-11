<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		checked,
		disabled = false,
		onchange,
		children,
		...rest
	}: {
		checked: boolean;
		disabled?: boolean;
		onchange?: (e: Event & { currentTarget: HTMLInputElement }) => void;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();
</script>

<!-- A checkbox with its words, box first, the whole row the target (`.k-check`). -->
<label class="k-check">
	<input type="checkbox" {checked} {disabled} {onchange} {...rest} />
	<span>{@render children?.()}</span>
</label>
