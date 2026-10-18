<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		checked,
		disabled = false,
		pending = false,
		onchange,
		children,
		...rest
	}: {
		checked: boolean;
		disabled?: boolean;
		/** An edit is in flight: the box ignores a click but keeps focus (see `Btn`). */
		pending?: boolean;
		onchange?: (e: Event & { currentTarget: HTMLInputElement }) => void;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();
</script>

<!-- A checkbox with its words, box first, the whole row the target (`.k-check`). -->
<label class="k-check">
	<input
		type="checkbox"
		{checked}
		{disabled}
		aria-disabled={pending ? 'true' : undefined}
		onclick={(e) => {
			// A box flips as it is pressed; an edit in flight holds it where it was.
			if (pending) e.preventDefault();
		}}
		onchange={(e) => {
			if (!pending) onchange?.(e);
		}}
		{...rest}
	/>
	<span>{@render children?.()}</span>
</label>
